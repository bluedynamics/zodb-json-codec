use std::cell::RefCell;

use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use serde_json::{json, Map, Value};

use crate::btrees;
use crate::error::CodecError;
use crate::json_writer::JsonWriter;
use crate::known_types;
use crate::types::{InstanceData, PickleValue};

/// Convert a PickleValue AST to a serde_json Value.
pub fn pickle_value_to_json(val: &PickleValue) -> Result<Value, CodecError> {
    pickle_value_to_json_impl(val, false, false, 0)
}

/// Convert a PickleValue AST to a serde_json Value for PostgreSQL JSONB.
///
/// Like `pickle_value_to_json` but with PG-specific transformations:
/// - Null-byte sanitization: strings containing `\0` → `{"@ns": base64}`
/// - Persistent ref compaction: `(oid_bytes, None)` → `{"@ref": "hex_oid"}`
#[cfg(test)]
pub fn pickle_value_to_json_pg(val: &PickleValue) -> Result<Value, CodecError> {
    pickle_value_to_json_impl(val, true, true, 0)
}

const MAX_DEPTH: usize = 1000;

fn pickle_value_to_json_impl(
    val: &PickleValue,
    sanitize_nulls: bool,
    compact_refs: bool,
    depth: usize,
) -> Result<Value, CodecError> {
    if depth > MAX_DEPTH {
        return Err(CodecError::InvalidData(
            "maximum nesting depth exceeded in JSON conversion".to_string(),
        ));
    }
    // Recursive closure that captures the flags
    let to_json = |v: &PickleValue| -> Result<Value, CodecError> {
        pickle_value_to_json_impl(v, sanitize_nulls, compact_refs, depth + 1)
    };
    match val {
        PickleValue::None => Ok(Value::Null),
        PickleValue::Bool(b) => Ok(Value::Bool(*b)),
        PickleValue::Int(i) => Ok(json!(*i)),
        PickleValue::BigInt(bi) => {
            // Store as string to avoid precision loss
            Ok(json!({"@bi": bi.to_string()}))
        }
        PickleValue::Float(f) => {
            Ok(serde_json::Number::from_f64(*f)
                .map(Value::Number)
                .unwrap_or(Value::Null))
        }
        PickleValue::String(s) => {
            if sanitize_nulls && s.contains('\0') {
                // PG JSONB cannot store \u0000 — base64-encode with @ns marker
                Ok(json!({"@ns": BASE64.encode(s.as_bytes())}))
            } else {
                Ok(Value::String(s.clone()))
            }
        }
        PickleValue::Bytes(b) => {
            Ok(json!({"@b": BASE64.encode(b)}))
        }
        PickleValue::List(items) => {
            let arr: Result<Vec<Value>, _> = items.iter().map(&to_json).collect();
            Ok(Value::Array(arr?))
        }
        PickleValue::Tuple(items) => {
            let arr: Result<Vec<Value>, _> = items.iter().map(&to_json).collect();
            Ok(json!({"@t": arr?}))
        }
        PickleValue::Dict(pairs) => {
            let all_string_keys = pairs.iter().all(|(k, _)| matches!(k, PickleValue::String(_)));
            if all_string_keys {
                let mut map = Map::new();
                for (k, v) in pairs {
                    if let PickleValue::String(key) = k {
                        let json_key = if sanitize_nulls && key.contains('\0') {
                            // Null-byte in dict key — use @ns: prefix for JSON key
                            format!("@ns:{}", BASE64.encode(key.as_bytes()))
                        } else {
                            key.clone()
                        };
                        map.insert(json_key, to_json(v)?);
                    }
                }
                Ok(Value::Object(map))
            } else {
                let arr: Result<Vec<Value>, CodecError> = pairs
                    .iter()
                    .map(|(k, v)| Ok(json!([to_json(k)?, to_json(v)?])))
                    .collect();
                Ok(json!({"@d": arr?}))
            }
        }
        PickleValue::Set(items) => {
            let arr: Result<Vec<Value>, _> = items.iter().map(&to_json).collect();
            Ok(json!({"@set": arr?}))
        }
        PickleValue::FrozenSet(items) => {
            let arr: Result<Vec<Value>, _> = items.iter().map(&to_json).collect();
            Ok(json!({"@fset": arr?}))
        }
        PickleValue::Global { module, name } => {
            Ok(json!({"@cls": [module, name]}))
        }
        PickleValue::Instance(inst) => {
            let InstanceData { module, name, state, dict_items, list_items } = inst.as_ref();
            if let Some(typed) =
                known_types::try_instance_to_typed_json(module, name, state, &to_json)?
            {
                return Ok(typed);
            }
            let state_json = if let Some(info) = btrees::classify_btree(module, name) {
                btrees::btree_state_to_json(&info, state, &to_json)?
            } else {
                to_json(state)?
            };
            if module.is_empty() && name.is_empty() {
                Ok(json!({"@inst": state_json}))
            } else {
                let mut obj = json!({
                    "@cls": [module, name],
                    "@s": state_json,
                });
                if let Some(pairs) = dict_items {
                    let items_json: Result<Vec<Value>, CodecError> = pairs
                        .iter()
                        .map(|(k, v)| Ok(json!([to_json(k)?, to_json(v)?])))
                        .collect();
                    obj.as_object_mut().unwrap().insert("@items".to_string(), json!(items_json?));
                }
                if let Some(items) = list_items {
                    let appends_json: Result<Vec<Value>, _> = items.iter().map(&to_json).collect();
                    obj.as_object_mut().unwrap().insert("@appends".to_string(), json!(appends_json?));
                }
                Ok(obj)
            }
        }
        PickleValue::PersistentRef(inner) => {
            if compact_refs {
                return compact_ref_to_json(inner, &to_json);
            }
            let inner_json = to_json(inner)?;
            Ok(json!({"@ref": inner_json}))
        }
        PickleValue::Reduce { callable, args, dict_items, list_items } => {
            if let Some(typed) =
                known_types::try_reduce_to_typed_json(callable, args, &to_json)?
            {
                return Ok(typed);
            }
            let callable_json = to_json(callable)?;
            let args_json = to_json(args)?;
            let mut reduce_obj = json!({
                "callable": callable_json,
                "args": args_json,
            });
            if let Some(pairs) = dict_items {
                let items_json: Result<Vec<Value>, CodecError> = pairs
                    .iter()
                    .map(|(k, v)| Ok(json!([to_json(k)?, to_json(v)?])))
                    .collect();
                reduce_obj.as_object_mut().unwrap().insert("items".to_string(), json!(items_json?));
            }
            if let Some(items) = list_items {
                let appends_json: Result<Vec<Value>, _> = items.iter().map(&to_json).collect();
                reduce_obj.as_object_mut().unwrap().insert("appends".to_string(), json!(appends_json?));
            }
            Ok(json!({"@reduce": reduce_obj}))
        }
        PickleValue::RawPickle(data) => {
            Ok(json!({"@pkl": BASE64.encode(data)}))
        }
    }
}

/// Compact a ZODB persistent ref to JSON.
/// inner is typically Tuple([Bytes(oid), None_or_Global])
fn compact_ref_to_json(
    inner: &PickleValue,
    to_json: &dyn Fn(&PickleValue) -> Result<Value, CodecError>,
) -> Result<Value, CodecError> {
    if let PickleValue::Tuple(items) = inner {
        if items.len() == 2 {
            if let PickleValue::Bytes(oid) = &items[0] {
                let hex = hex::encode(oid);
                match &items[1] {
                    PickleValue::None => {
                        return Ok(json!({"@ref": hex}));
                    }
                    PickleValue::Global { module, name } => {
                        let class_path = if module.is_empty() {
                            name.clone()
                        } else {
                            format!("{module}.{name}")
                        };
                        return Ok(json!({"@ref": [hex, class_path]}));
                    }
                    _ => {}
                }
            }
        }
    }
    // Fallback: generic ref
    let inner_json = to_json(inner)?;
    Ok(json!({"@ref": inner_json}))
}

// ===========================================================================
// Direct JSON string writer path (no serde_json::Value intermediate)
// ===========================================================================

thread_local! {
    static JSON_BUF: RefCell<JsonWriter> = RefCell::new(JsonWriter::with_capacity(4096));
}

/// Convert a PickleValue AST directly to a JSON string for PostgreSQL JSONB.
///
/// This is the fast path that eliminates all serde_json::Value allocations.
/// It handles BTree dispatch internally.
pub fn pickle_value_to_json_string_pg(
    val: &PickleValue,
    module: &str,
    name: &str,
) -> Result<String, CodecError> {
    JSON_BUF.with(|cell| {
        let mut w = cell.borrow_mut();
        w.clear();

        if let Some(info) = btrees::classify_btree(module, name) {
            btrees::btree_state_to_json_writer(&info, val, &write_value_pg_flat, &mut w)?;
        } else {
            write_value_pg_depth(&mut w, val, 0)?;
        }

        Ok(w.take())
    })
}

/// Recursive walker: write a PickleValue as PG-compatible JSON to a JsonWriter.
fn write_value_pg_depth(w: &mut JsonWriter, val: &PickleValue, depth: usize) -> Result<(), CodecError> {
    if depth > MAX_DEPTH {
        return Err(CodecError::InvalidData(
            "maximum nesting depth exceeded in JSON conversion".to_string(),
        ));
    }
    let recurse =
        |w: &mut JsonWriter, v: &PickleValue| -> Result<(), CodecError> { write_value_pg_depth(w, v, depth + 1) };

    match val {
        PickleValue::None => {
            w.write_null();
        }
        PickleValue::Bool(b) => {
            w.write_bool(*b);
        }
        PickleValue::Int(i) => {
            w.write_i64(*i);
        }
        PickleValue::BigInt(bi) => {
            // {"@bi": "..."}
            w.begin_object();
            w.write_key_literal("@bi");
            w.write_string(&bi.to_string());
            w.end_object();
        }
        PickleValue::Float(f) => {
            w.write_f64(*f);
        }
        PickleValue::String(s) => {
            if s.contains('\0') {
                // PG JSONB cannot store \u0000 — base64-encode with @ns marker
                w.begin_object();
                w.write_key_literal("@ns");
                w.write_string_literal(&BASE64.encode(s.as_bytes()));
                w.end_object();
            } else {
                w.write_string(s);
            }
        }
        PickleValue::Bytes(b) => {
            // {"@b": base64}
            w.begin_object();
            w.write_key_literal("@b");
            w.write_string_literal(&BASE64.encode(b));
            w.end_object();
        }
        PickleValue::List(items) => {
            w.begin_array();
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    w.write_comma();
                }
                recurse(w, item)?;
            }
            w.end_array();
        }
        PickleValue::Tuple(items) => {
            // {"@t": [...]}
            w.begin_object();
            w.write_key_literal("@t");
            w.begin_array();
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    w.write_comma();
                }
                recurse(w, item)?;
            }
            w.end_array();
            w.end_object();
        }
        PickleValue::Dict(pairs) => {
            let all_string_keys = pairs
                .iter()
                .all(|(k, _)| matches!(k, PickleValue::String(_)));
            if all_string_keys {
                w.begin_object();
                for (i, (k, v)) in pairs.iter().enumerate() {
                    if i > 0 {
                        w.write_comma();
                    }
                    if let PickleValue::String(key) = k {
                        if key.contains('\0') {
                            let encoded = format!("@ns:{}", BASE64.encode(key.as_bytes()));
                            w.write_key(&encoded);
                        } else {
                            w.write_key(key);
                        }
                        recurse(w, v)?;
                    }
                }
                w.end_object();
            } else {
                // {"@d": [[k, v], ...]}
                w.begin_object();
                w.write_key_literal("@d");
                w.begin_array();
                for (i, (k, v)) in pairs.iter().enumerate() {
                    if i > 0 {
                        w.write_comma();
                    }
                    w.begin_array();
                    recurse(w, k)?;
                    w.write_comma();
                    recurse(w, v)?;
                    w.end_array();
                }
                w.end_array();
                w.end_object();
            }
        }
        PickleValue::Set(items) => {
            // {"@set": [...]}
            w.begin_object();
            w.write_key_literal("@set");
            w.begin_array();
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    w.write_comma();
                }
                recurse(w, item)?;
            }
            w.end_array();
            w.end_object();
        }
        PickleValue::FrozenSet(items) => {
            // {"@fset": [...]}
            w.begin_object();
            w.write_key_literal("@fset");
            w.begin_array();
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    w.write_comma();
                }
                recurse(w, item)?;
            }
            w.end_array();
            w.end_object();
        }
        PickleValue::Global { module, name } => {
            // {"@cls": ["module", "name"]}
            w.begin_object();
            w.write_key_literal("@cls");
            w.begin_array();
            w.write_string(module);
            w.write_comma();
            w.write_string(name);
            w.end_array();
            w.end_object();
        }
        PickleValue::Instance(inst) => {
            let InstanceData {
                module,
                name,
                state,
                dict_items,
                list_items,
            } = inst.as_ref();

            // Try known type handlers first
            if known_types::try_write_instance_typed(w, module, name, state)? {
                return Ok(());
            }

            // BTree handling
            let has_btree = btrees::classify_btree(module, name);

            if module.is_empty() && name.is_empty() {
                // {"@inst": state}
                w.begin_object();
                w.write_key_literal("@inst");
                if let Some(info) = &has_btree {
                    btrees::btree_state_to_json_writer(info, state, &recurse, w)?;
                } else {
                    recurse(w, state)?;
                }
                w.end_object();
            } else {
                // {"@cls": [mod, name], "@s": state, ...}
                w.begin_object();
                w.write_key_literal("@cls");
                w.begin_array();
                w.write_string(module);
                w.write_comma();
                w.write_string(name);
                w.end_array();
                w.write_comma();
                w.write_key_literal("@s");
                if let Some(info) = &has_btree {
                    btrees::btree_state_to_json_writer(info, state, &recurse, w)?;
                } else {
                    recurse(w, state)?;
                }
                if let Some(pairs) = dict_items {
                    w.write_comma();
                    w.write_key_literal("@items");
                    w.begin_array();
                    for (i, (k, v)) in pairs.iter().enumerate() {
                        if i > 0 {
                            w.write_comma();
                        }
                        w.begin_array();
                        recurse(w, k)?;
                        w.write_comma();
                        recurse(w, v)?;
                        w.end_array();
                    }
                    w.end_array();
                }
                if let Some(items) = list_items {
                    w.write_comma();
                    w.write_key_literal("@appends");
                    w.begin_array();
                    for (i, item) in items.iter().enumerate() {
                        if i > 0 {
                            w.write_comma();
                        }
                        recurse(w, item)?;
                    }
                    w.end_array();
                }
                w.end_object();
            }
        }
        PickleValue::PersistentRef(inner) => {
            // Compact ref: always use compact mode for PG path
            write_compact_ref_pg(w, inner, &recurse)?;
        }
        PickleValue::Reduce {
            callable,
            args,
            dict_items,
            list_items,
        } => {
            // Try known types first
            if known_types::try_write_reduce_typed(w, callable, args, &recurse)? {
                return Ok(());
            }
            // Fallback: {"@reduce": {"callable": ..., "args": ..., ...}}
            w.begin_object();
            w.write_key_literal("@reduce");
            w.begin_object();
            w.write_key_literal("callable");
            recurse(w, callable)?;
            w.write_comma();
            w.write_key_literal("args");
            recurse(w, args)?;
            if let Some(pairs) = dict_items {
                w.write_comma();
                w.write_key_literal("items");
                w.begin_array();
                for (i, (k, v)) in pairs.iter().enumerate() {
                    if i > 0 {
                        w.write_comma();
                    }
                    w.begin_array();
                    recurse(w, k)?;
                    w.write_comma();
                    recurse(w, v)?;
                    w.end_array();
                }
                w.end_array();
            }
            if let Some(items) = list_items {
                w.write_comma();
                w.write_key_literal("appends");
                w.begin_array();
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        w.write_comma();
                    }
                    recurse(w, item)?;
                }
                w.end_array();
            }
            w.end_object();
            w.end_object();
        }
        PickleValue::RawPickle(data) => {
            // {"@pkl": base64}
            w.begin_object();
            w.write_key_literal("@pkl");
            w.write_string_literal(&BASE64.encode(data));
            w.end_object();
        }
    }
    Ok(())
}

/// Wrapper for BTree callbacks — they take (w, val) not (w, val, depth).
fn write_value_pg_flat(w: &mut JsonWriter, val: &PickleValue) -> Result<(), CodecError> {
    write_value_pg_depth(w, val, 0)
}

/// Write a compact persistent ref for PG path.
fn write_compact_ref_pg(
    w: &mut JsonWriter,
    inner: &PickleValue,
    recurse: &dyn Fn(&mut JsonWriter, &PickleValue) -> Result<(), CodecError>,
) -> Result<(), CodecError> {
    if let PickleValue::Tuple(items) = inner {
        if items.len() == 2 {
            if let PickleValue::Bytes(oid) = &items[0] {
                let hex = hex::encode(oid);
                match &items[1] {
                    PickleValue::None => {
                        // {"@ref": "hex_oid"}
                        w.begin_object();
                        w.write_key_literal("@ref");
                        w.write_string_literal(&hex);
                        w.end_object();
                        return Ok(());
                    }
                    PickleValue::Global { module, name } => {
                        let class_path = if module.is_empty() {
                            name.clone()
                        } else {
                            format!("{module}.{name}")
                        };
                        // {"@ref": ["hex_oid", "class_path"]}
                        w.begin_object();
                        w.write_key_literal("@ref");
                        w.begin_array();
                        w.write_string_literal(&hex);
                        w.write_comma();
                        w.write_string(&class_path);
                        w.end_array();
                        w.end_object();
                        return Ok(());
                    }
                    _ => {}
                }
            }
        }
    }
    // Fallback: generic ref
    w.begin_object();
    w.write_key_literal("@ref");
    recurse(w, inner)?;
    w.end_object();
    Ok(())
}

/// Convert a serde_json Value back to a PickleValue AST.
pub fn json_to_pickle_value(val: &Value) -> Result<PickleValue, CodecError> {
    match val {
        Value::Null => Ok(PickleValue::None),
        Value::Bool(b) => Ok(PickleValue::Bool(*b)),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Ok(PickleValue::Int(i))
            } else if let Some(f) = n.as_f64() {
                Ok(PickleValue::Float(f))
            } else {
                Err(CodecError::Json(format!("unsupported number: {n}")))
            }
        }
        Value::String(s) => Ok(PickleValue::String(s.clone())),
        Value::Array(arr) => {
            let items: Result<Vec<PickleValue>, _> =
                arr.iter().map(json_to_pickle_value).collect();
            Ok(PickleValue::List(items?))
        }
        Value::Object(map) => {
            // Check for our special type markers
            if let Some(v) = map.get("@t") {
                // Tuple
                if let Value::Array(arr) = v {
                    let items: Result<Vec<PickleValue>, _> =
                        arr.iter().map(json_to_pickle_value).collect();
                    return Ok(PickleValue::Tuple(items?));
                }
            }
            if let Some(v) = map.get("@b") {
                // Bytes
                if let Value::String(s) = v {
                    let bytes = BASE64
                        .decode(s)
                        .map_err(|e| CodecError::Json(format!("base64 decode: {e}")))?;
                    return Ok(PickleValue::Bytes(bytes));
                }
            }
            if let Some(v) = map.get("@bi") {
                // BigInt
                if let Value::String(s) = v {
                    let bi: num_bigint::BigInt = s
                        .parse()
                        .map_err(|e| CodecError::Json(format!("bigint parse: {e}")))?;
                    return Ok(PickleValue::BigInt(bi));
                }
            }
            if let Some(v) = map.get("@d") {
                // Dict with non-string keys
                if let Value::Array(arr) = v {
                    let mut pairs = Vec::new();
                    for pair in arr {
                        if let Value::Array(kv) = pair {
                            if kv.len() == 2 {
                                let k = json_to_pickle_value(&kv[0])?;
                                let v = json_to_pickle_value(&kv[1])?;
                                pairs.push((k, v));
                            }
                        }
                    }
                    return Ok(PickleValue::Dict(pairs));
                }
            }
            if let Some(v) = map.get("@set") {
                if let Value::Array(arr) = v {
                    let items: Result<Vec<PickleValue>, _> =
                        arr.iter().map(json_to_pickle_value).collect();
                    return Ok(PickleValue::Set(items?));
                }
            }
            if let Some(v) = map.get("@fset") {
                if let Value::Array(arr) = v {
                    let items: Result<Vec<PickleValue>, _> =
                        arr.iter().map(json_to_pickle_value).collect();
                    return Ok(PickleValue::FrozenSet(items?));
                }
            }
            if let Some(v) = map.get("@ref") {
                let inner = json_to_pickle_value(v)?;
                return Ok(PickleValue::PersistentRef(Box::new(inner)));
            }
            if let Some(v) = map.get("@pkl") {
                if let Value::String(s) = v {
                    let bytes = BASE64
                        .decode(s)
                        .map_err(|e| CodecError::Json(format!("base64 decode: {e}")))?;
                    return Ok(PickleValue::RawPickle(bytes));
                }
            }
            // Check for known typed markers (@dt, @date, @time, @td, @dec, @uuid)
            if let Some(pv) =
                known_types::try_typed_json_to_pickle_value(map, &json_to_pickle_value)?
            {
                return Ok(pv);
            }
            // Check for instance: has both @cls and @s
            if map.contains_key("@cls") && map.contains_key("@s") {
                if let Some(Value::Array(cls)) = map.get("@cls") {
                    if cls.len() == 2 {
                        let module = cls[0].as_str().unwrap_or("").to_string();
                        let name = cls[1].as_str().unwrap_or("").to_string();
                        let state_json = map.get("@s").unwrap();
                        // Use BTree-specific state decoding if applicable
                        let state =
                            if let Some(info) = btrees::classify_btree(&module, &name) {
                                btrees::json_to_btree_state(
                                    &info,
                                    state_json,
                                    &json_to_pickle_value,
                                )?
                            } else {
                                json_to_pickle_value(state_json)?
                            };
                        let dict_items = if let Some(Value::Array(items_arr)) = map.get("@items") {
                            let mut pairs = Vec::new();
                            for pair in items_arr {
                                if let Value::Array(kv) = pair {
                                    if kv.len() == 2 {
                                        let k = json_to_pickle_value(&kv[0])?;
                                        let v = json_to_pickle_value(&kv[1])?;
                                        pairs.push((k, v));
                                    }
                                }
                            }
                            Some(Box::new(pairs))
                        } else {
                            None
                        };
                        let list_items = if let Some(Value::Array(appends_arr)) = map.get("@appends") {
                            let items: Result<Vec<PickleValue>, _> =
                                appends_arr.iter().map(json_to_pickle_value).collect();
                            Some(Box::new(items?))
                        } else {
                            None
                        };
                        return Ok(PickleValue::Instance(Box::new(InstanceData {
                            module,
                            name,
                            state: Box::new(state),
                            dict_items,
                            list_items,
                        })));
                    }
                }
            }
            // Check for standalone @cls (Global reference)
            if let Some(Value::Array(cls)) = map.get("@cls") {
                if cls.len() == 2 && !map.contains_key("@s") {
                    let module = cls[0].as_str().unwrap_or("").to_string();
                    let name = cls[1].as_str().unwrap_or("").to_string();
                    return Ok(PickleValue::Global { module, name });
                }
            }
            if let Some(v) = map.get("@reduce") {
                if let Value::Object(reduce_map) = v {
                    let callable =
                        json_to_pickle_value(reduce_map.get("callable").unwrap_or(&Value::Null))?;
                    let args =
                        json_to_pickle_value(reduce_map.get("args").unwrap_or(&Value::Null))?;
                    let dict_items = if let Some(Value::Array(items_arr)) = reduce_map.get("items") {
                        let mut pairs = Vec::new();
                        for pair in items_arr {
                            if let Value::Array(kv) = pair {
                                if kv.len() == 2 {
                                    let k = json_to_pickle_value(&kv[0])?;
                                    let v = json_to_pickle_value(&kv[1])?;
                                    pairs.push((k, v));
                                }
                            }
                        }
                        Some(Box::new(pairs))
                    } else {
                        None
                    };
                    let list_items = if let Some(Value::Array(appends_arr)) = reduce_map.get("appends") {
                        let items: Result<Vec<PickleValue>, _> =
                            appends_arr.iter().map(json_to_pickle_value).collect();
                        Some(Box::new(items?))
                    } else {
                        None
                    };
                    return Ok(PickleValue::Reduce {
                        callable: Box::new(callable),
                        args: Box::new(args),
                        dict_items,
                        list_items,
                    });
                }
            }
            // Regular dict with string keys
            let mut pairs = Vec::new();
            for (k, v) in map {
                pairs.push((
                    PickleValue::String(k.clone()),
                    json_to_pickle_value(v)?,
                ));
            }
            Ok(PickleValue::Dict(pairs))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::InstanceData;

    #[test]
    fn test_roundtrip_none() {
        let val = PickleValue::None;
        let json = pickle_value_to_json(&val).unwrap();
        let back = json_to_pickle_value(&json).unwrap();
        assert_eq!(val, back);
    }

    #[test]
    fn test_roundtrip_string() {
        let val = PickleValue::String("hello".to_string());
        let json = pickle_value_to_json(&val).unwrap();
        assert_eq!(json, Value::String("hello".to_string()));
        let back = json_to_pickle_value(&json).unwrap();
        assert_eq!(val, back);
    }

    #[test]
    fn test_roundtrip_bytes() {
        let val = PickleValue::Bytes(vec![1, 2, 3]);
        let json = pickle_value_to_json(&val).unwrap();
        let back = json_to_pickle_value(&json).unwrap();
        assert_eq!(val, back);
    }

    #[test]
    fn test_roundtrip_tuple() {
        let val = PickleValue::Tuple(vec![PickleValue::Int(1), PickleValue::Int(2)]);
        let json = pickle_value_to_json(&val).unwrap();
        let back = json_to_pickle_value(&json).unwrap();
        assert_eq!(val, back);
    }

    #[test]
    fn test_roundtrip_dict_string_keys() {
        let val = PickleValue::Dict(vec![
            (PickleValue::String("a".to_string()), PickleValue::Int(1)),
            (PickleValue::String("b".to_string()), PickleValue::Int(2)),
        ]);
        let json = pickle_value_to_json(&val).unwrap();
        // Should be a plain JSON object
        assert!(json.is_object());
        let back = json_to_pickle_value(&json).unwrap();
        // Note: JSON object key order may differ, compare as sets
        if let (PickleValue::Dict(orig), PickleValue::Dict(restored)) = (&val, &back) {
            assert_eq!(orig.len(), restored.len());
        }
    }

    #[test]
    fn test_roundtrip_dict_nonstring_keys() {
        let val = PickleValue::Dict(vec![
            (PickleValue::Int(1), PickleValue::String("a".to_string())),
            (PickleValue::Int(2), PickleValue::String("b".to_string())),
        ]);
        let json = pickle_value_to_json(&val).unwrap();
        // Should use @d encoding
        assert!(json.get("@d").is_some());
        let back = json_to_pickle_value(&json).unwrap();
        assert_eq!(val, back);
    }

    #[test]
    fn test_instance() {
        let val = PickleValue::Instance(Box::new(InstanceData {
            module: "myapp".to_string(),
            name: "MyClass".to_string(),
            state: Box::new(PickleValue::Dict(vec![(
                PickleValue::String("x".to_string()),
                PickleValue::Int(42),
            )])),
            dict_items: None,
            list_items: None,
        }));
        let json = pickle_value_to_json(&val).unwrap();
        assert_eq!(json["@cls"][0], "myapp");
        assert_eq!(json["@cls"][1], "MyClass");
        assert_eq!(json["@s"]["x"], 42);
        let back = json_to_pickle_value(&json).unwrap();
        assert_eq!(val, back);
    }

    #[test]
    fn test_instance_with_dict_items() {
        let val = PickleValue::Instance(Box::new(InstanceData {
            module: "collections".to_string(),
            name: "OrderedDict".to_string(),
            state: Box::new(PickleValue::None),
            dict_items: Some(Box::new(vec![
                (PickleValue::String("a".to_string()), PickleValue::Int(1)),
                (PickleValue::String("b".to_string()), PickleValue::Int(2)),
            ])),
            list_items: None,
        }));
        let json = pickle_value_to_json(&val).unwrap();
        assert_eq!(json["@cls"][0], "collections");
        assert_eq!(json["@cls"][1], "OrderedDict");
        assert!(json.get("@items").is_some());
        let items = json["@items"].as_array().unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0][0], "a");
        assert_eq!(items[0][1], 1);
        let back = json_to_pickle_value(&json).unwrap();
        assert_eq!(val, back);
    }

    #[test]
    fn test_instance_with_list_items() {
        let val = PickleValue::Instance(Box::new(InstanceData {
            module: "mymod".to_string(),
            name: "MyList".to_string(),
            state: Box::new(PickleValue::None),
            dict_items: None,
            list_items: Some(Box::new(vec![PickleValue::Int(10), PickleValue::Int(20)])),
        }));
        let json = pickle_value_to_json(&val).unwrap();
        assert!(json.get("@appends").is_some());
        let appends = json["@appends"].as_array().unwrap();
        assert_eq!(appends.len(), 2);
        assert_eq!(appends[0], 10);
        assert_eq!(appends[1], 20);
        let back = json_to_pickle_value(&json).unwrap();
        assert_eq!(val, back);
    }

    #[test]
    fn test_reduce_with_dict_items() {
        let val = PickleValue::Reduce {
            callable: Box::new(PickleValue::Global {
                module: "collections".to_string(),
                name: "OrderedDict".to_string(),
            }),
            args: Box::new(PickleValue::Tuple(vec![])),
            dict_items: Some(Box::new(vec![
                (PickleValue::String("x".to_string()), PickleValue::Int(1)),
            ])),
            list_items: None,
        };
        let json = pickle_value_to_json(&val).unwrap();
        let reduce = json.get("@reduce").unwrap();
        assert!(reduce.get("items").is_some());
        let items = reduce["items"].as_array().unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0][0], "x");
        assert_eq!(items[0][1], 1);
        let back = json_to_pickle_value(&json).unwrap();
        assert_eq!(val, back);
    }

    #[test]
    fn test_reduce_with_list_items() {
        let val = PickleValue::Reduce {
            callable: Box::new(PickleValue::Global {
                module: "mymod".to_string(),
                name: "MyList".to_string(),
            }),
            args: Box::new(PickleValue::Tuple(vec![])),
            dict_items: None,
            list_items: Some(Box::new(vec![PickleValue::Int(5), PickleValue::Int(6)])),
        };
        let json = pickle_value_to_json(&val).unwrap();
        let reduce = json.get("@reduce").unwrap();
        assert!(reduce.get("appends").is_some());
        let appends = reduce["appends"].as_array().unwrap();
        assert_eq!(appends.len(), 2);
        let back = json_to_pickle_value(&json).unwrap();
        assert_eq!(val, back);
    }

    // ── PG-specific tests ──────────────────────────────────────────

    #[test]
    fn test_pg_null_byte_sanitization() {
        let val = PickleValue::String("hello\0world".to_string());
        // Standard path: no sanitization
        let json = pickle_value_to_json(&val).unwrap();
        assert_eq!(json, Value::String("hello\0world".to_string()));
        // PG path: @ns marker with base64
        let pg_json = pickle_value_to_json_pg(&val).unwrap();
        assert!(pg_json.get("@ns").is_some());
        let encoded = pg_json["@ns"].as_str().unwrap();
        let decoded = BASE64.decode(encoded).unwrap();
        assert_eq!(decoded, b"hello\0world");
    }

    #[test]
    fn test_pg_null_byte_in_dict_key() {
        let val = PickleValue::Dict(vec![(
            PickleValue::String("key\0null".to_string()),
            PickleValue::Int(42),
        )]);
        let pg_json = pickle_value_to_json_pg(&val).unwrap();
        let map = pg_json.as_object().unwrap();
        // Key should be @ns: prefixed base64
        assert!(map.keys().any(|k| k.starts_with("@ns:")));
        assert_eq!(*map.values().next().unwrap(), json!(42));
    }

    #[test]
    fn test_pg_string_without_null_unchanged() {
        let val = PickleValue::String("normal".to_string());
        let pg_json = pickle_value_to_json_pg(&val).unwrap();
        assert_eq!(pg_json, Value::String("normal".to_string()));
    }

    #[test]
    fn test_pg_compact_ref_oid_only() {
        // Tuple([Bytes(oid), None]) → {"@ref": "hex_oid"}
        let oid = vec![0, 0, 0, 0, 0, 0, 0, 3u8];
        let val = PickleValue::PersistentRef(Box::new(PickleValue::Tuple(vec![
            PickleValue::Bytes(oid),
            PickleValue::None,
        ])));
        let pg_json = pickle_value_to_json_pg(&val).unwrap();
        assert_eq!(pg_json, json!({"@ref": "0000000000000003"}));
    }

    #[test]
    fn test_pg_compact_ref_with_class() {
        // Tuple([Bytes(oid), Global{mod, name}]) → {"@ref": ["hex", "mod.name"]}
        let oid = vec![0, 0, 0, 0, 0, 0, 0, 5u8];
        let val = PickleValue::PersistentRef(Box::new(PickleValue::Tuple(vec![
            PickleValue::Bytes(oid),
            PickleValue::Global {
                module: "myapp.models".to_string(),
                name: "Document".to_string(),
            },
        ])));
        let pg_json = pickle_value_to_json_pg(&val).unwrap();
        assert_eq!(
            pg_json,
            json!({"@ref": ["0000000000000005", "myapp.models.Document"]})
        );
    }

    #[test]
    fn test_pg_generic_ref_no_compact() {
        // Standard path: no compaction
        let oid = vec![0, 0, 0, 0, 0, 0, 0, 3u8];
        let val = PickleValue::PersistentRef(Box::new(PickleValue::Tuple(vec![
            PickleValue::Bytes(oid),
            PickleValue::None,
        ])));
        let json = pickle_value_to_json(&val).unwrap();
        // Should NOT be compact — should be nested structure
        let ref_val = &json["@ref"];
        assert!(ref_val.is_object(), "standard path should produce nested ref");
    }

    #[test]
    fn test_pg_null_sanitization_in_list() {
        let val = PickleValue::List(vec![
            PickleValue::String("ok".to_string()),
            PickleValue::String("has\0null".to_string()),
        ]);
        let pg_json = pickle_value_to_json_pg(&val).unwrap();
        let arr = pg_json.as_array().unwrap();
        assert_eq!(arr[0], Value::String("ok".to_string()));
        assert!(arr[1].get("@ns").is_some());
    }

    #[test]
    fn test_pg_compact_ref_empty_module() {
        // Global with empty module: just use name
        let oid = vec![0, 0, 0, 0, 0, 0, 0, 1u8];
        let val = PickleValue::PersistentRef(Box::new(PickleValue::Tuple(vec![
            PickleValue::Bytes(oid),
            PickleValue::Global {
                module: "".to_string(),
                name: "SomeClass".to_string(),
            },
        ])));
        let pg_json = pickle_value_to_json_pg(&val).unwrap();
        assert_eq!(
            pg_json,
            json!({"@ref": ["0000000000000001", "SomeClass"]})
        );
    }

    // ── Direct JSON writer path tests ────────────────────────────────

    /// Helper: compare old path (serde_json::Value → to_string) vs new path (direct writer).
    /// Compares via parsed serde_json::Value since key order may differ (serde_json
    /// sorts alphabetically, direct writer preserves insertion order — both are valid JSON).
    fn assert_pg_paths_match(val: &PickleValue, module: &str, name: &str) {
        // Old path
        let state_json = if let Some(info) = crate::btrees::classify_btree(module, name) {
            crate::btrees::btree_state_to_json(&info, val, &pickle_value_to_json_pg).unwrap()
        } else {
            pickle_value_to_json_pg(val).unwrap()
        };

        // New path
        let new_str = pickle_value_to_json_string_pg(val, module, name).unwrap();

        // Parse new_str back to Value for order-insensitive comparison
        let new_val: Value = serde_json::from_str(&new_str).unwrap_or_else(|e| {
            panic!("new path produced invalid JSON: {e}\nJSON: {new_str}")
        });

        assert_eq!(state_json, new_val, "PG paths differ for module={module}, name={name}\nold: {}\nnew: {new_str}", serde_json::to_string(&state_json).unwrap());
    }

    // -- Primitives --

    #[test]
    fn test_direct_none() {
        assert_pg_paths_match(&PickleValue::None, "", "");
    }

    #[test]
    fn test_direct_bool() {
        assert_pg_paths_match(&PickleValue::Bool(true), "", "");
        assert_pg_paths_match(&PickleValue::Bool(false), "", "");
    }

    #[test]
    fn test_direct_int() {
        assert_pg_paths_match(&PickleValue::Int(42), "", "");
        assert_pg_paths_match(&PickleValue::Int(-1), "", "");
        assert_pg_paths_match(&PickleValue::Int(0), "", "");
        assert_pg_paths_match(&PickleValue::Int(i64::MAX), "", "");
        assert_pg_paths_match(&PickleValue::Int(i64::MIN), "", "");
    }

    #[test]
    fn test_direct_bigint() {
        let bi = num_bigint::BigInt::from(1234567890123456789_i128);
        assert_pg_paths_match(&PickleValue::BigInt(bi), "", "");
    }

    #[test]
    fn test_direct_float() {
        assert_pg_paths_match(&PickleValue::Float(3.14), "", "");
        assert_pg_paths_match(&PickleValue::Float(0.0), "", "");
        assert_pg_paths_match(&PickleValue::Float(-1.5), "", "");
        assert_pg_paths_match(&PickleValue::Float(f64::NAN), "", "");
        assert_pg_paths_match(&PickleValue::Float(f64::INFINITY), "", "");
        assert_pg_paths_match(&PickleValue::Float(f64::NEG_INFINITY), "", "");
    }

    #[test]
    fn test_direct_string() {
        assert_pg_paths_match(&PickleValue::String("hello".into()), "", "");
        assert_pg_paths_match(&PickleValue::String("".into()), "", "");
        assert_pg_paths_match(&PickleValue::String("日本語".into()), "", "");
    }

    #[test]
    fn test_direct_string_with_escapes() {
        assert_pg_paths_match(&PickleValue::String("a\"b\\c\nd\re\tf".into()), "", "");
    }

    #[test]
    fn test_direct_string_null_byte() {
        assert_pg_paths_match(&PickleValue::String("hello\0world".into()), "", "");
    }

    #[test]
    fn test_direct_string_control_chars() {
        assert_pg_paths_match(&PickleValue::String("\x01\x1f".into()), "", "");
    }

    #[test]
    fn test_direct_bytes() {
        assert_pg_paths_match(&PickleValue::Bytes(vec![1, 2, 3, 255]), "", "");
        assert_pg_paths_match(&PickleValue::Bytes(vec![]), "", "");
    }

    // -- Containers --

    #[test]
    fn test_direct_list() {
        assert_pg_paths_match(
            &PickleValue::List(vec![PickleValue::Int(1), PickleValue::String("x".into())]),
            "",
            "",
        );
        assert_pg_paths_match(&PickleValue::List(vec![]), "", "");
    }

    #[test]
    fn test_direct_tuple() {
        assert_pg_paths_match(
            &PickleValue::Tuple(vec![PickleValue::Int(1), PickleValue::Bool(true)]),
            "",
            "",
        );
        assert_pg_paths_match(&PickleValue::Tuple(vec![]), "", "");
    }

    #[test]
    fn test_direct_dict_string_keys() {
        assert_pg_paths_match(
            &PickleValue::Dict(vec![
                (PickleValue::String("a".into()), PickleValue::Int(1)),
                (PickleValue::String("b".into()), PickleValue::Int(2)),
            ]),
            "",
            "",
        );
    }

    #[test]
    fn test_direct_dict_null_key() {
        assert_pg_paths_match(
            &PickleValue::Dict(vec![(
                PickleValue::String("key\0null".into()),
                PickleValue::Int(42),
            )]),
            "",
            "",
        );
    }

    #[test]
    fn test_direct_dict_nonstring_keys() {
        assert_pg_paths_match(
            &PickleValue::Dict(vec![
                (PickleValue::Int(1), PickleValue::String("a".into())),
                (PickleValue::Int(2), PickleValue::String("b".into())),
            ]),
            "",
            "",
        );
    }

    #[test]
    fn test_direct_dict_empty() {
        assert_pg_paths_match(&PickleValue::Dict(vec![]), "", "");
    }

    #[test]
    fn test_direct_set() {
        assert_pg_paths_match(
            &PickleValue::Set(vec![PickleValue::Int(1), PickleValue::Int(2)]),
            "",
            "",
        );
    }

    #[test]
    fn test_direct_frozenset() {
        assert_pg_paths_match(
            &PickleValue::FrozenSet(vec![PickleValue::Int(1), PickleValue::Int(2)]),
            "",
            "",
        );
    }

    // -- Globals, Instances, Refs --

    #[test]
    fn test_direct_global() {
        assert_pg_paths_match(
            &PickleValue::Global {
                module: "mymod".into(),
                name: "MyClass".into(),
            },
            "",
            "",
        );
    }

    #[test]
    fn test_direct_instance() {
        let inst = PickleValue::Instance(Box::new(InstanceData {
            module: "myapp".into(),
            name: "MyClass".into(),
            state: Box::new(PickleValue::Dict(vec![(
                PickleValue::String("x".into()),
                PickleValue::Int(42),
            )])),
            dict_items: None,
            list_items: None,
        }));
        assert_pg_paths_match(&inst, "", "");
    }

    #[test]
    fn test_direct_instance_with_dict_items() {
        let inst = PickleValue::Instance(Box::new(InstanceData {
            module: "collections".into(),
            name: "OrderedDict".into(),
            state: Box::new(PickleValue::None),
            dict_items: Some(Box::new(vec![
                (PickleValue::String("a".into()), PickleValue::Int(1)),
            ])),
            list_items: None,
        }));
        assert_pg_paths_match(&inst, "", "");
    }

    #[test]
    fn test_direct_instance_with_list_items() {
        let inst = PickleValue::Instance(Box::new(InstanceData {
            module: "mymod".into(),
            name: "MyList".into(),
            state: Box::new(PickleValue::None),
            dict_items: None,
            list_items: Some(Box::new(vec![PickleValue::Int(10)])),
        }));
        assert_pg_paths_match(&inst, "", "");
    }

    #[test]
    fn test_direct_persistent_ref_oid_only() {
        let val = PickleValue::PersistentRef(Box::new(PickleValue::Tuple(vec![
            PickleValue::Bytes(vec![0, 0, 0, 0, 0, 0, 0, 3]),
            PickleValue::None,
        ])));
        assert_pg_paths_match(&val, "", "");
    }

    #[test]
    fn test_direct_persistent_ref_with_class() {
        let val = PickleValue::PersistentRef(Box::new(PickleValue::Tuple(vec![
            PickleValue::Bytes(vec![0, 0, 0, 0, 0, 0, 0, 5]),
            PickleValue::Global {
                module: "myapp.models".into(),
                name: "Document".into(),
            },
        ])));
        assert_pg_paths_match(&val, "", "");
    }

    #[test]
    fn test_direct_persistent_ref_fallback() {
        // Non-standard ref: just an int
        let val = PickleValue::PersistentRef(Box::new(PickleValue::Int(42)));
        assert_pg_paths_match(&val, "", "");
    }

    // -- Known types --

    fn make_reduce(module: &str, name: &str, args: PickleValue) -> PickleValue {
        PickleValue::Reduce {
            callable: Box::new(PickleValue::Global {
                module: module.into(),
                name: name.into(),
            }),
            args: Box::new(args),
            dict_items: None,
            list_items: None,
        }
    }

    #[test]
    fn test_direct_datetime_naive() {
        let bytes = vec![0x07, 0xE9, 6, 15, 12, 0, 0, 0, 0, 0];
        let val = make_reduce(
            "datetime",
            "datetime",
            PickleValue::Tuple(vec![PickleValue::Bytes(bytes)]),
        );
        assert_pg_paths_match(&val, "", "");
    }

    #[test]
    fn test_direct_datetime_with_microseconds() {
        let us: u32 = 123456;
        let bytes = vec![
            0x07, 0xE9, 6, 15, 12, 30, 45,
            ((us >> 16) & 0xff) as u8,
            ((us >> 8) & 0xff) as u8,
            (us & 0xff) as u8,
        ];
        let val = make_reduce(
            "datetime",
            "datetime",
            PickleValue::Tuple(vec![PickleValue::Bytes(bytes)]),
        );
        assert_pg_paths_match(&val, "", "");
    }

    #[test]
    fn test_direct_datetime_utc() {
        let bytes = vec![0x07, 0xE9, 1, 1, 0, 0, 0, 0, 0, 0];
        let tz = make_reduce(
            "datetime",
            "timezone",
            PickleValue::Tuple(vec![make_reduce(
                "datetime",
                "timedelta",
                PickleValue::Tuple(vec![
                    PickleValue::Int(0),
                    PickleValue::Int(0),
                    PickleValue::Int(0),
                ]),
            )]),
        );
        let val = make_reduce(
            "datetime",
            "datetime",
            PickleValue::Tuple(vec![PickleValue::Bytes(bytes), tz]),
        );
        assert_pg_paths_match(&val, "", "");
    }

    #[test]
    fn test_direct_datetime_offset() {
        let bytes = vec![0x07, 0xE9, 1, 1, 0, 0, 0, 0, 0, 0];
        let tz = make_reduce(
            "datetime",
            "timezone",
            PickleValue::Tuple(vec![make_reduce(
                "datetime",
                "timedelta",
                PickleValue::Tuple(vec![
                    PickleValue::Int(0),
                    PickleValue::Int(19800), // +05:30
                    PickleValue::Int(0),
                ]),
            )]),
        );
        let val = make_reduce(
            "datetime",
            "datetime",
            PickleValue::Tuple(vec![PickleValue::Bytes(bytes), tz]),
        );
        assert_pg_paths_match(&val, "", "");
    }

    #[test]
    fn test_direct_datetime_negative_offset() {
        let bytes = vec![0x07, 0xE9, 1, 1, 0, 0, 0, 0, 0, 0];
        let tz = make_reduce(
            "datetime",
            "timezone",
            PickleValue::Tuple(vec![make_reduce(
                "datetime",
                "timedelta",
                PickleValue::Tuple(vec![
                    PickleValue::Int(0),
                    PickleValue::Int(-18000), // -05:00
                    PickleValue::Int(0),
                ]),
            )]),
        );
        let val = make_reduce(
            "datetime",
            "datetime",
            PickleValue::Tuple(vec![PickleValue::Bytes(bytes), tz]),
        );
        assert_pg_paths_match(&val, "", "");
    }

    #[test]
    fn test_direct_datetime_pytz_utc() {
        let bytes = vec![0x07, 0xE9, 1, 1, 0, 0, 0, 0, 0, 0];
        let tz = make_reduce("pytz", "_UTC", PickleValue::Tuple(vec![]));
        let val = make_reduce(
            "datetime",
            "datetime",
            PickleValue::Tuple(vec![PickleValue::Bytes(bytes), tz]),
        );
        assert_pg_paths_match(&val, "", "");
    }

    #[test]
    fn test_direct_datetime_pytz_named() {
        let bytes = vec![0x07, 0xE9, 1, 1, 0, 0, 0, 0, 0, 0];
        let tz = make_reduce(
            "pytz",
            "_p",
            PickleValue::Tuple(vec![
                PickleValue::String("US/Eastern".into()),
                PickleValue::Int(-18000),
                PickleValue::Int(0),
                PickleValue::String("EST".into()),
            ]),
        );
        let val = make_reduce(
            "datetime",
            "datetime",
            PickleValue::Tuple(vec![PickleValue::Bytes(bytes), tz]),
        );
        assert_pg_paths_match(&val, "", "");
    }

    #[test]
    fn test_direct_date() {
        let bytes = vec![0x07, 0xE9, 6, 15];
        let val = make_reduce(
            "datetime",
            "date",
            PickleValue::Tuple(vec![PickleValue::Bytes(bytes)]),
        );
        assert_pg_paths_match(&val, "", "");
    }

    #[test]
    fn test_direct_time_naive() {
        let bytes = vec![12, 30, 45, 0, 0, 0];
        let val = make_reduce(
            "datetime",
            "time",
            PickleValue::Tuple(vec![PickleValue::Bytes(bytes)]),
        );
        assert_pg_paths_match(&val, "", "");
    }

    #[test]
    fn test_direct_time_with_microseconds() {
        let us: u32 = 500000;
        let bytes = vec![
            12, 30, 45,
            ((us >> 16) & 0xff) as u8,
            ((us >> 8) & 0xff) as u8,
            (us & 0xff) as u8,
        ];
        let val = make_reduce(
            "datetime",
            "time",
            PickleValue::Tuple(vec![PickleValue::Bytes(bytes)]),
        );
        assert_pg_paths_match(&val, "", "");
    }

    #[test]
    fn test_direct_time_with_offset() {
        let bytes = vec![12, 30, 45, 0, 0, 0];
        let tz = make_reduce(
            "datetime",
            "timezone",
            PickleValue::Tuple(vec![make_reduce(
                "datetime",
                "timedelta",
                PickleValue::Tuple(vec![
                    PickleValue::Int(0),
                    PickleValue::Int(3600),
                    PickleValue::Int(0),
                ]),
            )]),
        );
        let val = make_reduce(
            "datetime",
            "time",
            PickleValue::Tuple(vec![PickleValue::Bytes(bytes), tz]),
        );
        assert_pg_paths_match(&val, "", "");
    }

    #[test]
    fn test_direct_timedelta() {
        let val = make_reduce(
            "datetime",
            "timedelta",
            PickleValue::Tuple(vec![
                PickleValue::Int(7),
                PickleValue::Int(3600),
                PickleValue::Int(500000),
            ]),
        );
        assert_pg_paths_match(&val, "", "");
    }

    #[test]
    fn test_direct_decimal() {
        let val = make_reduce(
            "decimal",
            "Decimal",
            PickleValue::Tuple(vec![PickleValue::String("3.14159".into())]),
        );
        assert_pg_paths_match(&val, "", "");
    }

    #[test]
    fn test_direct_set_reduce() {
        let val = make_reduce(
            "builtins",
            "set",
            PickleValue::Tuple(vec![PickleValue::List(vec![
                PickleValue::Int(1),
                PickleValue::Int(2),
            ])]),
        );
        assert_pg_paths_match(&val, "", "");
    }

    #[test]
    fn test_direct_frozenset_reduce() {
        let val = make_reduce(
            "builtins",
            "frozenset",
            PickleValue::Tuple(vec![PickleValue::List(vec![
                PickleValue::Int(1),
            ])]),
        );
        assert_pg_paths_match(&val, "", "");
    }

    #[test]
    fn test_direct_uuid() {
        let int_val: u128 = 0x12345678_1234_5678_1234_5678_1234_5678;
        let bi = num_bigint::BigInt::from(int_val);
        let val = PickleValue::Instance(Box::new(InstanceData {
            module: "uuid".into(),
            name: "UUID".into(),
            state: Box::new(PickleValue::Dict(vec![(
                PickleValue::String("int".into()),
                PickleValue::BigInt(bi),
            )])),
            dict_items: None,
            list_items: None,
        }));
        assert_pg_paths_match(&val, "", "");
    }

    #[test]
    fn test_direct_uuid_small_int() {
        // UUID with int fitting in i64
        let val = PickleValue::Instance(Box::new(InstanceData {
            module: "uuid".into(),
            name: "UUID".into(),
            state: Box::new(PickleValue::Dict(vec![(
                PickleValue::String("int".into()),
                PickleValue::Int(12345),
            )])),
            dict_items: None,
            list_items: None,
        }));
        assert_pg_paths_match(&val, "", "");
    }

    // -- Unknown REDUCE (fallback) --

    #[test]
    fn test_direct_unknown_reduce() {
        let val = make_reduce(
            "mymod",
            "myfunc",
            PickleValue::Tuple(vec![PickleValue::Int(1)]),
        );
        assert_pg_paths_match(&val, "", "");
    }

    #[test]
    fn test_direct_reduce_with_dict_items() {
        let val = PickleValue::Reduce {
            callable: Box::new(PickleValue::Global {
                module: "collections".into(),
                name: "OrderedDict".into(),
            }),
            args: Box::new(PickleValue::Tuple(vec![])),
            dict_items: Some(Box::new(vec![
                (PickleValue::String("x".into()), PickleValue::Int(1)),
            ])),
            list_items: None,
        };
        assert_pg_paths_match(&val, "", "");
    }

    #[test]
    fn test_direct_reduce_with_list_items() {
        let val = PickleValue::Reduce {
            callable: Box::new(PickleValue::Global {
                module: "mymod".into(),
                name: "MyList".into(),
            }),
            args: Box::new(PickleValue::Tuple(vec![])),
            dict_items: None,
            list_items: Some(Box::new(vec![PickleValue::Int(5)])),
        };
        assert_pg_paths_match(&val, "", "");
    }

    // -- RawPickle --

    #[test]
    fn test_direct_raw_pickle() {
        let val = PickleValue::RawPickle(vec![0x80, 0x03, 0x4e, 0x2e]);
        assert_pg_paths_match(&val, "", "");
    }

    // -- BTree types --

    #[test]
    fn test_direct_btree_empty() {
        assert_pg_paths_match(&PickleValue::None, "BTrees.OOBTree", "OOBTree");
    }

    #[test]
    fn test_direct_btree_small() {
        let state = PickleValue::Tuple(vec![PickleValue::Tuple(vec![PickleValue::Tuple(
            vec![PickleValue::Tuple(vec![
                PickleValue::String("a".into()),
                PickleValue::Int(1),
                PickleValue::String("b".into()),
                PickleValue::Int(2),
            ])],
        )])]);
        assert_pg_paths_match(&state, "BTrees.OOBTree", "OOBTree");
    }

    #[test]
    fn test_direct_btree_bucket() {
        let state = PickleValue::Tuple(vec![PickleValue::Tuple(vec![
            PickleValue::String("x".into()),
            PickleValue::Int(10),
            PickleValue::String("y".into()),
            PickleValue::Int(20),
        ])]);
        assert_pg_paths_match(&state, "BTrees.OOBTree", "OOBucket");
    }

    #[test]
    fn test_direct_btree_set() {
        let state = PickleValue::Tuple(vec![PickleValue::Tuple(vec![
            PickleValue::String("a".into()),
            PickleValue::String("b".into()),
        ])]);
        assert_pg_paths_match(&state, "BTrees.OOBTree", "OOSet");
    }

    #[test]
    fn test_direct_btree_treeset() {
        let state = PickleValue::Tuple(vec![PickleValue::Tuple(vec![PickleValue::Tuple(
            vec![PickleValue::Tuple(vec![
                PickleValue::Int(1),
                PickleValue::Int(2),
                PickleValue::Int(3),
            ])],
        )])]);
        assert_pg_paths_match(&state, "BTrees.IIBTree", "IITreeSet");
    }

    #[test]
    fn test_direct_btree_linked_bucket() {
        let state = PickleValue::Tuple(vec![
            PickleValue::Tuple(vec![
                PickleValue::String("a".into()),
                PickleValue::Int(1),
            ]),
            PickleValue::PersistentRef(Box::new(PickleValue::Tuple(vec![
                PickleValue::Bytes(vec![0, 0, 0, 0, 0, 0, 0, 3]),
                PickleValue::None,
            ]))),
        ]);
        assert_pg_paths_match(&state, "BTrees.OOBTree", "OOBucket");
    }

    #[test]
    fn test_direct_btree_large_with_refs() {
        let ref0 = PickleValue::PersistentRef(Box::new(PickleValue::Tuple(vec![
            PickleValue::Bytes(vec![0, 0, 0, 0, 0, 0, 0, 2]),
            PickleValue::None,
        ])));
        let ref1 = PickleValue::PersistentRef(Box::new(PickleValue::Tuple(vec![
            PickleValue::Bytes(vec![0, 0, 0, 0, 0, 0, 0, 3]),
            PickleValue::None,
        ])));
        let first = PickleValue::PersistentRef(Box::new(PickleValue::Tuple(vec![
            PickleValue::Bytes(vec![0, 0, 0, 0, 0, 0, 0, 2]),
            PickleValue::None,
        ])));
        let state = PickleValue::Tuple(vec![
            PickleValue::Tuple(vec![ref0, PickleValue::String("sep".into()), ref1]),
            first,
        ]);
        assert_pg_paths_match(&state, "BTrees.OOBTree", "OOBTree");
    }

    // -- Nested/complex structures --

    #[test]
    fn test_direct_nested_dict() {
        let inner = PickleValue::Dict(vec![
            (PickleValue::String("x".into()), PickleValue::Int(1)),
        ]);
        let outer = PickleValue::Dict(vec![
            (PickleValue::String("nested".into()), inner),
            (PickleValue::String("flat".into()), PickleValue::Bool(true)),
        ]);
        assert_pg_paths_match(&outer, "", "");
    }

    #[test]
    fn test_direct_mixed_types_in_list() {
        let val = PickleValue::List(vec![
            PickleValue::None,
            PickleValue::Bool(true),
            PickleValue::Int(42),
            PickleValue::Float(3.14),
            PickleValue::String("text".into()),
            PickleValue::Bytes(vec![1, 2, 3]),
            PickleValue::Tuple(vec![PickleValue::Int(1)]),
        ]);
        assert_pg_paths_match(&val, "", "");
    }

    #[test]
    fn test_direct_deeply_nested() {
        // 10 levels of nesting
        let mut val = PickleValue::Int(42);
        for i in 0..10 {
            val = PickleValue::Dict(vec![(
                PickleValue::String(format!("level_{i}")),
                val,
            )]);
        }
        assert_pg_paths_match(&val, "", "");
    }

    #[test]
    fn test_direct_persistent_mapping_like() {
        // Simulates a typical ZODB PersistentMapping state
        let state = PickleValue::Dict(vec![
            (PickleValue::String("title".into()), PickleValue::String("My Document".into())),
            (PickleValue::String("count".into()), PickleValue::Int(42)),
            (PickleValue::String("active".into()), PickleValue::Bool(true)),
            (PickleValue::String("tags".into()), PickleValue::List(vec![
                PickleValue::String("tag1".into()),
                PickleValue::String("tag2".into()),
            ])),
            (PickleValue::String("ref".into()), PickleValue::PersistentRef(Box::new(
                PickleValue::Tuple(vec![
                    PickleValue::Bytes(vec![0, 0, 0, 0, 0, 0, 0, 7]),
                    PickleValue::None,
                ]),
            ))),
        ]);
        assert_pg_paths_match(&state, "persistent.mapping", "PersistentMapping");
    }

    #[test]
    fn test_direct_state_with_datetime_and_ref() {
        // Realistic ZODB state: dict with datetime field + persistent ref
        let dt_bytes = vec![0x07, 0xE9, 6, 15, 12, 0, 0, 0, 0, 0];
        let dt = make_reduce(
            "datetime",
            "datetime",
            PickleValue::Tuple(vec![PickleValue::Bytes(dt_bytes)]),
        );
        let state = PickleValue::Dict(vec![
            (PickleValue::String("created".into()), dt),
            (PickleValue::String("name".into()), PickleValue::String("test".into())),
        ]);
        assert_pg_paths_match(&state, "", "");
    }

    // -- Empty bucket BTree --

    #[test]
    fn test_direct_btree_empty_bucket() {
        let state = PickleValue::Tuple(vec![PickleValue::Tuple(vec![])]);
        assert_pg_paths_match(&state, "BTrees.OOBTree", "OOBucket");
    }

    #[test]
    fn test_direct_btree_empty_inline() {
        let state = PickleValue::Tuple(vec![PickleValue::Tuple(vec![PickleValue::Tuple(
            vec![PickleValue::Tuple(vec![])],
        )])]);
        assert_pg_paths_match(&state, "BTrees.OOBTree", "OOBTree");
    }

    // -- Instance inside BTree context --

    #[test]
    fn test_direct_instance_empty_module_name() {
        let inst = PickleValue::Instance(Box::new(InstanceData {
            module: "".into(),
            name: "".into(),
            state: Box::new(PickleValue::Int(42)),
            dict_items: None,
            list_items: None,
        }));
        assert_pg_paths_match(&inst, "", "");
    }
}

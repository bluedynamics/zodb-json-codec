use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use serde_json::{json, Map, Value};

use crate::btrees;
use crate::error::CodecError;
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
}

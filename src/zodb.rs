use crate::error::CodecError;
use crate::types::PickleValue;

#[cfg(test)]
use base64::Engine as _;
#[cfg(test)]
use crate::btrees;
#[cfg(test)]
use crate::decode::decode_pickle;
#[cfg(test)]
use crate::encode::encode_pickle;
#[cfg(test)]
use crate::json::{json_to_pickle_value, pickle_value_to_json};
#[cfg(test)]
use serde_json::{json, Value};

/// A ZODB record consists of two concatenated pickles:
/// 1. Class pickle: (module, classname)
/// 2. State pickle: the object's __getstate__() result
///
/// We need to find the boundary between the two pickles.
/// The first pickle ends at its STOP opcode (0x2e = '.').
pub fn split_zodb_record(data: &[u8]) -> Result<(&[u8], &[u8]), CodecError> {
    // We need to properly walk the first pickle to find its STOP opcode.
    // Simple approach: scan for STOP, but STOP byte (0x2e) can appear inside
    // string/bytes data. We need a minimal pickle walker.
    let boundary = find_pickle_end(data)?;
    Ok((&data[..boundary], &data[boundary..]))
}

/// Find the end (exclusive) of the first pickle in the data.
/// This walks the pickle opcodes to correctly skip over string/bytes
/// data that might contain the STOP byte.
fn find_pickle_end(data: &[u8]) -> Result<usize, CodecError> {
    use crate::opcodes::*;
    let mut pos = 0;

    loop {
        if pos >= data.len() {
            return Err(CodecError::UnexpectedEof);
        }
        let op = data[pos];
        pos += 1;

        match op {
            STOP => return Ok(pos),
            PROTO => pos += 1,
            FRAME => pos += 8,

            // Zero-argument opcodes
            NONE | NEWTRUE | NEWFALSE | EMPTY_DICT | EMPTY_LIST | EMPTY_TUPLE | EMPTY_SET
            | MARK | POP | DUP | APPEND | APPENDS | BUILD | SETITEM | SETITEMS | ADDITEMS
            | REDUCE | NEWOBJ | BINPERSID | TUPLE | TUPLE1 | TUPLE2 | TUPLE3 | LIST | DICT
            | FROZENSET | STACK_GLOBAL | MEMOIZE | NEWOBJ_EX => {}

            // 1-byte argument
            BININT1 | BINPUT | BINGET => pos += 1,

            // 2-byte argument
            BININT2 => pos += 2,

            // 4-byte argument
            BININT | LONG_BINPUT | LONG_BINGET => pos += 4,

            // 8-byte argument
            BINFLOAT => pos += 8,

            // Counted binary data (4-byte length)
            BINUNICODE | BINSTRING | BINBYTES => {
                if pos + 4 > data.len() {
                    return Err(CodecError::UnexpectedEof);
                }
                let n = u32::from_le_bytes(data[pos..pos + 4].try_into().unwrap()) as usize;
                pos += 4 + n;
            }

            // Short counted binary data (1-byte length)
            SHORT_BINUNICODE | SHORT_BINSTRING | SHORT_BINBYTES => {
                if pos >= data.len() {
                    return Err(CodecError::UnexpectedEof);
                }
                let n = data[pos] as usize;
                pos += 1 + n;
            }

            // 8-byte length variants
            BINUNICODE8 | BINBYTES8 | BYTEARRAY8 => {
                if pos + 8 > data.len() {
                    return Err(CodecError::UnexpectedEof);
                }
                let n = u64::from_le_bytes(data[pos..pos + 8].try_into().unwrap()) as usize;
                pos += 8 + n;
            }

            // LONG1: 1-byte length + data
            LONG1 => {
                if pos >= data.len() {
                    return Err(CodecError::UnexpectedEof);
                }
                let n = data[pos] as usize;
                pos += 1 + n;
            }

            // LONG4: 4-byte length + data
            LONG4 => {
                if pos + 4 > data.len() {
                    return Err(CodecError::UnexpectedEof);
                }
                let n = i32::from_le_bytes(data[pos..pos + 4].try_into().unwrap()) as usize;
                pos += 4 + n;
            }

            // Text-mode opcodes (newline-terminated)
            INT | LONG | FLOAT | STRING | UNICODE | GLOBAL | PUT | GET | PERSID => {
                // Read until newline
                while pos < data.len() && data[pos] != b'\n' {
                    pos += 1;
                }
                pos += 1; // skip newline
                // GLOBAL has TWO newline-terminated lines
                if op == GLOBAL {
                    while pos < data.len() && data[pos] != b'\n' {
                        pos += 1;
                    }
                    pos += 1;
                }
            }

            NEXT_BUFFER | READONLY_BUFFER => {}

            _ => {
                return Err(CodecError::UnknownOpcode(op));
            }
        }
    }
}

/// Decode a ZODB record (two concatenated pickles) into a JSON value.
/// (serde_json path — used by Rust tests; Python API uses pyconv instead)
///
/// Returns: `{"@cls": ["module", "name"], "@s": { ... state ... }}`
#[cfg(test)]
fn decode_zodb_record(data: &[u8]) -> Result<Value, CodecError> {
    let (class_pickle, state_pickle) = split_zodb_record(data)?;

    let class_val = decode_pickle(class_pickle)?;
    let state_val = decode_pickle(state_pickle)?;

    // Extract class info
    let (module, name) = extract_class_info(&class_val);

    // Use BTree-specific state conversion if applicable
    let state_json = if let Some(info) = btrees::classify_btree(&module, &name) {
        btrees::btree_state_to_json(&info, &state_val, &pickle_value_to_json)?
    } else {
        pickle_value_to_json(&state_val)?
    };

    // Post-process: convert ZODB persistent references to compact form
    let state_json = transform_persistent_refs(state_json);

    Ok(json!({
        "@cls": [module, name],
        "@s": state_json,
    }))
}

/// Encode a ZODB JSON record back into two concatenated pickles.
/// (serde_json path — used by Rust tests; Python API uses pyconv instead)
/// Takes ownership to avoid cloning the state tree for persistent ref restoration.
#[cfg(test)]
fn encode_zodb_record(mut json_val: Value) -> Result<Vec<u8>, CodecError> {
    let cls = json_val
        .get("@cls")
        .ok_or_else(|| CodecError::InvalidData("missing @cls in ZODB record".to_string()))?;

    let (module, name) = if let Value::Array(arr) = cls {
        if arr.len() == 2 {
            (
                arr[0].as_str().unwrap_or("").to_string(),
                arr[1].as_str().unwrap_or("").to_string(),
            )
        } else {
            return Err(CodecError::InvalidData("@cls must be [module, name]".to_string()));
        }
    } else {
        return Err(CodecError::InvalidData("@cls must be an array".to_string()));
    };

    // Check for BTree class before moving module/name into Global
    let btree_info = btrees::classify_btree(&module, &name);

    // Encode class pickle: ZODB uses GLOBAL opcode, not a tuple
    let class_val = PickleValue::Global { module, name };
    let class_bytes = encode_pickle(&class_val)?;

    // Take ownership of @s to avoid cloning, then restore persistent refs
    let state = json_val
        .as_object_mut()
        .and_then(|m| m.remove("@s"))
        .unwrap_or(Value::Null);
    let state = restore_persistent_refs(state);

    // Use BTree-specific state decoding if applicable
    let state_val = if let Some(info) = btree_info {
        btrees::json_to_btree_state(&info, &state, &json_to_pickle_value)?
    } else {
        json_to_pickle_value(&state)?
    };
    let state_bytes = encode_pickle(&state_val)?;

    // Concatenate
    let mut result = class_bytes;
    result.extend_from_slice(&state_bytes);
    Ok(result)
}

#[cfg(test)]
/// Transform ZODB persistent references from generic form to compact form.
///
/// ZODB persistent references in pickle are tuples: (oid_bytes, class_info)
/// where class_info is None or (module, name).
///
/// Generic JSON form (from pickle_value_to_json):
///   {"@ref": {"@t": [{"@b": "AAAAAAAAAAM="}, {"@cls": ["mod", "Cls"]}]}}
///   {"@ref": {"@t": [{"@b": "AAAAAAAAAAM="}, null]}}
///
/// Compact ZODB form:
///   {"@ref": "0000000000000003"}                          (oid only)
///   {"@ref": ["0000000000000003", "mod.Cls"]}             (oid + class)
fn transform_persistent_refs(val: Value) -> Value {
    match val {
        Value::Object(mut map) => {
            if let Some(ref_val) = map.get("@ref") {
                // Check if this is a ZODB-style persistent ref
                if let Some(compact) = try_compact_ref(ref_val) {
                    map.insert("@ref".to_string(), compact);
                    return Value::Object(map);
                }
            }
            // Recurse into all values
            let transformed: serde_json::Map<String, Value> = map
                .into_iter()
                .map(|(k, v)| (k, transform_persistent_refs(v)))
                .collect();
            Value::Object(transformed)
        }
        Value::Array(arr) => {
            Value::Array(arr.into_iter().map(transform_persistent_refs).collect())
        }
        other => other,
    }
}

#[cfg(test)]
/// Try to convert a generic persistent ref value to compact ZODB form.
fn try_compact_ref(ref_val: &Value) -> Option<Value> {
    // Expected: {"@t": [{"@b": "base64_oid"}, class_or_null]}
    let tuple_items = ref_val.as_object()?.get("@t")?.as_array()?;
    if tuple_items.len() != 2 {
        return None;
    }

    // First element: oid bytes as {"@b": "base64..."}
    let oid_b64 = tuple_items[0].as_object()?.get("@b")?.as_str()?;
    let oid_bytes = base64::engine::general_purpose::STANDARD
        .decode(oid_b64)
        .ok()?;
    let oid_hex = hex::encode(&oid_bytes);

    // Second element: None or {"@cls": ["module", "name"]}
    if tuple_items[1].is_null() {
        Some(Value::String(oid_hex))
    } else if let Some(cls_arr) = tuple_items[1].as_object()?.get("@cls")?.as_array() {
        if cls_arr.len() == 2 {
            let module = cls_arr[0].as_str().unwrap_or("");
            let name = cls_arr[1].as_str().unwrap_or("");
            let class_path = if module.is_empty() {
                name.to_string()
            } else {
                format!("{module}.{name}")
            };
            Some(json!([oid_hex, class_path]))
        } else {
            None
        }
    } else {
        None
    }
}

#[cfg(test)]
/// Restore compact ZODB persistent refs back to the generic form for encoding.
///
/// Compact: {"@ref": "0000000000000003"} or {"@ref": ["oid_hex", "mod.Cls"]}
/// Generic: {"@ref": {"@t": [{"@b": "base64"}, null_or_cls]}}
fn restore_persistent_refs(val: Value) -> Value {
    match val {
        Value::Object(mut map) => {
            if let Some(ref_val) = map.get("@ref").cloned() {
                if let Some(expanded) = try_expand_ref(&ref_val) {
                    map.insert("@ref".to_string(), expanded);
                    return Value::Object(map);
                }
            }
            let transformed: serde_json::Map<String, Value> = map
                .into_iter()
                .map(|(k, v)| (k, restore_persistent_refs(v)))
                .collect();
            Value::Object(transformed)
        }
        Value::Array(arr) => {
            Value::Array(arr.into_iter().map(restore_persistent_refs).collect())
        }
        other => other,
    }
}

#[cfg(test)]
/// Expand a compact ref back to generic tuple form.
fn try_expand_ref(ref_val: &Value) -> Option<Value> {
    match ref_val {
        // Simple string oid: {"@ref": "0000000000000003"}
        Value::String(oid_hex) => {
            let oid_bytes = hex::decode(oid_hex).ok()?;
            let oid_b64 = base64::engine::general_purpose::STANDARD.encode(&oid_bytes);
            Some(json!({"@t": [{"@b": oid_b64}, null]}))
        }
        // Array [oid, class]: {"@ref": ["0000000000000003", "mod.Cls"]}
        Value::Array(arr) if arr.len() == 2 => {
            let oid_hex = arr[0].as_str()?;
            let class_path = arr[1].as_str()?;
            let oid_bytes = hex::decode(oid_hex).ok()?;
            let oid_b64 = base64::engine::general_purpose::STANDARD.encode(&oid_bytes);

            // Split "module.ClassName" back into ["module", "ClassName"]
            let (module, name) = if let Some(dot_pos) = class_path.rfind('.') {
                (&class_path[..dot_pos], &class_path[dot_pos + 1..])
            } else {
                ("", class_path)
            };

            Some(json!({"@t": [{"@b": oid_b64}, {"@cls": [module, name]}]}))
        }
        _ => None,
    }
}

/// Extract (module, name) from a class pickle value.
pub fn extract_class_info(val: &PickleValue) -> (String, String) {
    match val {
        PickleValue::Tuple(items) if items.len() == 2 => {
            let module = match &items[0] {
                PickleValue::String(s) => s.clone(),
                _ => String::new(),
            };
            let name = match &items[1] {
                PickleValue::String(s) => s.clone(),
                _ => String::new(),
            };
            (module, name)
        }
        PickleValue::Global { module, name } => (module.clone(), name.clone()),
        _ => (String::new(), String::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_split_zodb_record() {
        // Two simple pickles concatenated: None + True
        let mut data = Vec::new();
        data.extend_from_slice(b"\x80\x02N."); // pickle 1: None
        data.extend_from_slice(b"\x80\x02\x88."); // pickle 2: True

        let (p1, p2) = split_zodb_record(&data).unwrap();
        assert_eq!(p1, b"\x80\x02N.");
        assert_eq!(p2, b"\x80\x02\x88.");
    }

    #[test]
    fn test_decode_encode_roundtrip() {
        // Build a minimal ZODB-like record:
        // Class pickle: ("mymodule", "MyClass")
        // State pickle: {"title": "hello"}
        let class_val = PickleValue::Tuple(vec![
            PickleValue::String("mymodule".to_string()),
            PickleValue::String("MyClass".to_string()),
        ]);
        let state_val = PickleValue::Dict(vec![(
            PickleValue::String("title".to_string()),
            PickleValue::String("hello".to_string()),
        )]);

        let class_bytes = encode_pickle(&class_val).unwrap();
        let state_bytes = encode_pickle(&state_val).unwrap();
        let mut record = class_bytes;
        record.extend_from_slice(&state_bytes);

        // Decode to JSON
        let json = decode_zodb_record(&record).unwrap();
        assert_eq!(json["@cls"][0], "mymodule");
        assert_eq!(json["@cls"][1], "MyClass");
        assert_eq!(json["@s"]["title"], "hello");

        // Encode back (clone since encode takes ownership)
        let re_encoded = encode_zodb_record(json.clone()).unwrap();

        // Decode again to verify
        let json2 = decode_zodb_record(&re_encoded).unwrap();
        assert_eq!(json["@cls"], json2["@cls"]);
        assert_eq!(json["@s"], json2["@s"]);
    }
}

use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use serde_json::{json, Map, Value};

use crate::btrees;
use crate::error::CodecError;
use crate::known_types;
use crate::types::PickleValue;

/// Convert a PickleValue AST to a serde_json Value.
pub fn pickle_value_to_json(val: &PickleValue) -> Result<Value, CodecError> {
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
        PickleValue::String(s) => Ok(Value::String(s.clone())),
        PickleValue::Bytes(b) => {
            Ok(json!({"@b": BASE64.encode(b)}))
        }
        PickleValue::List(items) => {
            let arr: Result<Vec<Value>, _> =
                items.iter().map(pickle_value_to_json).collect();
            Ok(Value::Array(arr?))
        }
        PickleValue::Tuple(items) => {
            let arr: Result<Vec<Value>, _> =
                items.iter().map(pickle_value_to_json).collect();
            Ok(json!({"@t": arr?}))
        }
        PickleValue::Dict(pairs) => {
            // Check if all keys are strings
            let all_string_keys = pairs.iter().all(|(k, _)| matches!(k, PickleValue::String(_)));
            if all_string_keys {
                let mut map = Map::new();
                for (k, v) in pairs {
                    if let PickleValue::String(key) = k {
                        map.insert(key.clone(), pickle_value_to_json(v)?);
                    }
                }
                Ok(Value::Object(map))
            } else {
                // Non-string keys: use array-of-pairs representation
                let arr: Result<Vec<Value>, CodecError> = pairs
                    .iter()
                    .map(|(k, v)| {
                        Ok(json!([pickle_value_to_json(k)?, pickle_value_to_json(v)?]))
                    })
                    .collect();
                Ok(json!({"@d": arr?}))
            }
        }
        PickleValue::Set(items) => {
            let arr: Result<Vec<Value>, _> =
                items.iter().map(pickle_value_to_json).collect();
            Ok(json!({"@set": arr?}))
        }
        PickleValue::FrozenSet(items) => {
            let arr: Result<Vec<Value>, _> =
                items.iter().map(pickle_value_to_json).collect();
            Ok(json!({"@fset": arr?}))
        }
        PickleValue::Global { module, name } => {
            Ok(json!({"@cls": [module, name]}))
        }
        PickleValue::Instance { module, name, state } => {
            // Try known type handlers first (e.g., uuid.UUID)
            if let Some(typed) =
                known_types::try_instance_to_typed_json(module, name, state, &pickle_value_to_json)?
            {
                return Ok(typed);
            }
            // Try BTree state flattening
            let state_json = if let Some(info) = btrees::classify_btree(module, name) {
                btrees::btree_state_to_json(&info, state, &pickle_value_to_json)?
            } else {
                pickle_value_to_json(state)?
            };
            if module.is_empty() && name.is_empty() {
                // Anonymous instance (couldn't extract class info)
                Ok(json!({"@inst": state_json}))
            } else {
                Ok(json!({
                    "@cls": [module, name],
                    "@s": state_json,
                }))
            }
        }
        PickleValue::PersistentRef(inner) => {
            let inner_json = pickle_value_to_json(inner)?;
            Ok(json!({"@ref": inner_json}))
        }
        PickleValue::Reduce { callable, args } => {
            // Try known type handlers first (datetime, Decimal, set, etc.)
            if let Some(typed) =
                known_types::try_reduce_to_typed_json(callable, args, &pickle_value_to_json)?
            {
                return Ok(typed);
            }
            // Fall back to generic @reduce
            let callable_json = pickle_value_to_json(callable)?;
            let args_json = pickle_value_to_json(args)?;
            Ok(json!({
                "@reduce": {
                    "callable": callable_json,
                    "args": args_json,
                }
            }))
        }
        PickleValue::RawPickle(data) => {
            Ok(json!({"@pkl": BASE64.encode(data)}))
        }
    }
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
                        return Ok(PickleValue::Instance {
                            module,
                            name,
                            state: Box::new(state),
                        });
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
                    return Ok(PickleValue::Reduce {
                        callable: Box::new(callable),
                        args: Box::new(args),
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
        let val = PickleValue::Instance {
            module: "myapp".to_string(),
            name: "MyClass".to_string(),
            state: Box::new(PickleValue::Dict(vec![(
                PickleValue::String("x".to_string()),
                PickleValue::Int(42),
            )])),
        };
        let json = pickle_value_to_json(&val).unwrap();
        assert_eq!(json["@cls"][0], "myapp");
        assert_eq!(json["@cls"][1], "MyClass");
        assert_eq!(json["@s"]["x"], 42);
        let back = json_to_pickle_value(&json).unwrap();
        assert_eq!(val, back);
    }
}

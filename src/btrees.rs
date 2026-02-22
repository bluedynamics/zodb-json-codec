//! BTree state flattening: convert deeply nested tuple state to queryable JSON.
//!
//! BTrees use nested tuples for `__getstate__()`:
//! - Small BTree/TreeSet: 4 levels of tuple wrapping around flat data
//! - Bucket/Set: 2 levels of tuple wrapping
//! - Large BTree: 2-tuple with persistent refs to child buckets
//!
//! This module recognizes these patterns and produces flat, queryable JSON
//! with `@kv` (key-value pairs) and `@ks` (keys) markers.

use serde_json::{json, Map, Value};

use crate::error::CodecError;
use crate::types::PickleValue;

// ---------------------------------------------------------------------------
// BTree class classification
// ---------------------------------------------------------------------------

/// The kind of BTree node.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BTreeNodeKind {
    /// `*BTree` — tree node for maps, 4-level nesting when inline
    BTree,
    /// `*Bucket` — leaf node for maps, 2-level nesting
    Bucket,
    /// `*TreeSet` — tree node for sets, 4-level nesting when inline
    TreeSet,
    /// `*Set` — leaf node for sets, 2-level nesting
    Set,
}

/// Classification result for a BTree class.
#[derive(Debug, Clone)]
pub struct BTreeClassInfo {
    pub kind: BTreeNodeKind,
    /// Whether this is a map type (has values) vs set type (keys only).
    pub is_map: bool,
}

/// Check if a class is a BTree type and classify it.
/// Returns None for non-BTree classes (including BTrees.Length.Length).
pub fn classify_btree(module: &str, name: &str) -> Option<BTreeClassInfo> {
    // Must be in a BTrees.* module
    if !module.starts_with("BTrees.") {
        return None;
    }

    // Skip BTrees.Length — it stores a scalar int, already works fine
    if module == "BTrees.Length" {
        return None;
    }

    // Determine kind from class name suffix
    if name.ends_with("BTree") {
        Some(BTreeClassInfo {
            kind: BTreeNodeKind::BTree,
            is_map: true,
        })
    } else if name.ends_with("Bucket") {
        Some(BTreeClassInfo {
            kind: BTreeNodeKind::Bucket,
            is_map: true,
        })
    } else if name.ends_with("TreeSet") {
        Some(BTreeClassInfo {
            kind: BTreeNodeKind::TreeSet,
            is_map: false,
        })
    } else if name.ends_with("Set") {
        Some(BTreeClassInfo {
            kind: BTreeNodeKind::Set,
            is_map: false,
        })
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Forward direction: PickleValue state → JSON
// ---------------------------------------------------------------------------

/// Convert a BTree state PickleValue to flattened JSON.
///
/// Falls back to the generic `to_json` converter if the state pattern is not recognized.
pub fn btree_state_to_json(
    info: &BTreeClassInfo,
    state: &PickleValue,
    to_json: &dyn Fn(&PickleValue) -> Result<Value, CodecError>,
) -> Result<Value, CodecError> {
    // Empty BTree: state is None
    if *state == PickleValue::None {
        return Ok(Value::Null);
    }

    match info.kind {
        BTreeNodeKind::BTree | BTreeNodeKind::TreeSet => {
            btree_node_state_to_json(info, state, to_json)
        }
        BTreeNodeKind::Bucket | BTreeNodeKind::Set => {
            bucket_state_to_json(info, state, to_json)
        }
    }
}

/// Handle BTree/TreeSet state (4-level nesting for inline, 2-tuple with refs for large).
fn btree_node_state_to_json(
    info: &BTreeClassInfo,
    state: &PickleValue,
    to_json: &dyn Fn(&PickleValue) -> Result<Value, CodecError>,
) -> Result<Value, CodecError> {
    // State must be a Tuple
    let outer = match state {
        PickleValue::Tuple(items) => items,
        _ => return to_json(state), // Fallback
    };

    // Case 1: Small inline BTree — 1-tuple: ((bucket_state,),)
    // Full nesting: ((((<flat_data>,),),),)
    // outer is the outermost tuple, should be 1-element
    if outer.len() == 1 {
        // Try to unwrap the 4-level nesting
        if let Some(flat_data) = unwrap_inline_btree(&outer[0]) {
            return format_flat_data(info, flat_data, to_json);
        }
        // Didn't match expected pattern — fallback
        return to_json(state);
    }

    // Case 2: Large BTree with persistent refs — 2-tuple: (children_tuple, firstbucket)
    if outer.len() == 2 {
        if let PickleValue::Tuple(children) = &outer[0] {
            // Check if children contain persistent refs (indicates large BTree)
            if children_has_refs(children) {
                return format_large_btree(children, &outer[1], to_json);
            }
        }
        // Didn't match expected pattern — fallback
        return to_json(state);
    }

    // Unknown pattern — fallback
    to_json(state)
}

/// Unwrap the inner 3 levels of a small inline BTree.
/// Input: the single element of the outermost tuple.
/// Expected: (((flat_data,),),) → returns flat_data items.
pub fn unwrap_inline_btree(val: &PickleValue) -> Option<&[PickleValue]> {
    // Level 2: should be 1-tuple
    let level2 = match val {
        PickleValue::Tuple(items) if items.len() == 1 => &items[0],
        _ => return None,
    };
    // Level 3: should be 1-tuple (bucket state outer)
    let level3 = match level2 {
        PickleValue::Tuple(items) if items.len() == 1 => &items[0],
        _ => return None,
    };
    // Level 4: the flat data tuple
    match level3 {
        PickleValue::Tuple(items) => Some(items),
        _ => None,
    }
}

/// Check if a children tuple contains any PersistentRef values.
pub fn children_has_refs(children: &[PickleValue]) -> bool {
    children
        .iter()
        .any(|item| matches!(item, PickleValue::PersistentRef(_)))
}

/// Format flat data (from innermost tuple) as @kv or @ks JSON.
fn format_flat_data(
    info: &BTreeClassInfo,
    items: &[PickleValue],
    to_json: &dyn Fn(&PickleValue) -> Result<Value, CodecError>,
) -> Result<Value, CodecError> {
    let mut map = Map::new();

    if info.is_map {
        // Map type: alternating key-value pairs → @kv: [[k, v], ...]
        if items.len() % 2 != 0 {
            return Err(CodecError::InvalidData(
                "BTree bucket has odd number of items for key-value pairs".to_string(),
            ));
        }
        let mut pairs = Vec::new();
        let mut i = 0;
        while i + 1 < items.len() {
            let k = to_json(&items[i])?;
            let v = to_json(&items[i + 1])?;
            pairs.push(json!([k, v]));
            i += 2;
        }
        map.insert("@kv".to_string(), Value::Array(pairs));
    } else {
        // Set type: keys only → @ks: [k1, k2, ...]
        let keys: Result<Vec<Value>, _> = items.iter().map(|item| to_json(item)).collect();
        map.insert("@ks".to_string(), Value::Array(keys?));
    }

    Ok(Value::Object(map))
}

/// Format a large BTree with persistent ref children.
fn format_large_btree(
    children: &[PickleValue],
    firstbucket: &PickleValue,
    to_json: &dyn Fn(&PickleValue) -> Result<Value, CodecError>,
) -> Result<Value, CodecError> {
    let children_json: Result<Vec<Value>, _> =
        children.iter().map(|item| to_json(item)).collect();
    let first_json = to_json(firstbucket)?;

    let mut map = Map::new();
    map.insert("@children".to_string(), Value::Array(children_json?));
    map.insert("@first".to_string(), first_json);
    Ok(Value::Object(map))
}

/// Handle Bucket/Set state (2-level nesting, optional next ref).
fn bucket_state_to_json(
    info: &BTreeClassInfo,
    state: &PickleValue,
    to_json: &dyn Fn(&PickleValue) -> Result<Value, CodecError>,
) -> Result<Value, CodecError> {
    let outer = match state {
        PickleValue::Tuple(items) => items,
        _ => return to_json(state), // Fallback
    };

    // Case 1: Standalone bucket — 1-tuple: ((flat_data,),)
    if outer.len() == 1 {
        if let PickleValue::Tuple(flat_data) = &outer[0] {
            return format_flat_data(info, flat_data, to_json);
        }
        return to_json(state);
    }

    // Case 2: Linked bucket — 2-tuple: ((flat_data,), next_ref)
    if outer.len() == 2 {
        if let PickleValue::Tuple(flat_data) = &outer[0] {
            let mut result_map = Map::new();

            if info.is_map {
                if flat_data.len() % 2 != 0 {
                    return Err(CodecError::InvalidData(
                        "BTree bucket has odd number of items for key-value pairs".to_string(),
                    ));
                }
                let mut pairs = Vec::new();
                let mut i = 0;
                while i + 1 < flat_data.len() {
                    let k = to_json(&flat_data[i])?;
                    let v = to_json(&flat_data[i + 1])?;
                    pairs.push(json!([k, v]));
                    i += 2;
                }
                result_map.insert("@kv".to_string(), Value::Array(pairs));
            } else {
                let keys: Result<Vec<Value>, _> =
                    flat_data.iter().map(|item| to_json(item)).collect();
                result_map.insert("@ks".to_string(), Value::Array(keys?));
            }

            let next_json = to_json(&outer[1])?;
            result_map.insert("@next".to_string(), next_json);
            return Ok(Value::Object(result_map));
        }
        return to_json(state);
    }

    // Unknown pattern — fallback
    to_json(state)
}

// ---------------------------------------------------------------------------
// Reverse direction: JSON → PickleValue state
// ---------------------------------------------------------------------------

/// Convert flattened BTree JSON back to the nested tuple PickleValue state.
pub fn json_to_btree_state(
    info: &BTreeClassInfo,
    state_json: &Value,
    from_json: &dyn Fn(&Value) -> Result<PickleValue, CodecError>,
) -> Result<PickleValue, CodecError> {
    // Empty BTree: null → None
    if state_json.is_null() {
        return Ok(PickleValue::None);
    }

    let map = match state_json {
        Value::Object(m) => m,
        // Not a JSON object — use generic decoder (e.g., scalar state for Length)
        _ => return from_json(state_json),
    };

    // Check for @kv (map data)
    if let Some(kv_val) = map.get("@kv") {
        let flat_data = decode_kv_pairs(kv_val, from_json)?;
        let next_ref = if let Some(next_val) = map.get("@next") {
            Some(from_json(next_val)?)
        } else {
            None
        };
        return wrap_flat_data(info, flat_data, next_ref);
    }

    // Check for @ks (set data)
    if let Some(ks_val) = map.get("@ks") {
        let flat_data = decode_keys(ks_val, from_json)?;
        let next_ref = if let Some(next_val) = map.get("@next") {
            Some(from_json(next_val)?)
        } else {
            None
        };
        return wrap_flat_data(info, flat_data, next_ref);
    }

    // Check for @children (large BTree with refs)
    if let Some(children_val) = map.get("@children") {
        let first_val = map
            .get("@first")
            .ok_or_else(|| CodecError::InvalidData("@children without @first".into()))?;
        return decode_large_btree(children_val, first_val, from_json);
    }

    // Not a BTree marker — fallback to generic decoder
    from_json(state_json)
}

/// Decode @kv pairs: [[k1, v1], [k2, v2], ...] → flat [k1, v1, k2, v2, ...]
fn decode_kv_pairs(
    val: &Value,
    from_json: &dyn Fn(&Value) -> Result<PickleValue, CodecError>,
) -> Result<Vec<PickleValue>, CodecError> {
    let arr = val
        .as_array()
        .ok_or_else(|| CodecError::InvalidData("@kv must be an array".into()))?;

    let mut flat = Vec::with_capacity(arr.len() * 2);
    for pair in arr {
        let pair_arr = pair
            .as_array()
            .ok_or_else(|| CodecError::InvalidData("@kv pair must be [k, v]".into()))?;
        if pair_arr.len() != 2 {
            return Err(CodecError::InvalidData("@kv pair must have 2 elements".into()));
        }
        flat.push(from_json(&pair_arr[0])?);
        flat.push(from_json(&pair_arr[1])?);
    }
    Ok(flat)
}

/// Decode @ks keys: [k1, k2, ...] → flat [k1, k2, ...]
fn decode_keys(
    val: &Value,
    from_json: &dyn Fn(&Value) -> Result<PickleValue, CodecError>,
) -> Result<Vec<PickleValue>, CodecError> {
    let arr = val
        .as_array()
        .ok_or_else(|| CodecError::InvalidData("@ks must be an array".into()))?;

    arr.iter().map(|item| from_json(item)).collect()
}

/// Wrap flat data items in the appropriate tuple nesting for the BTree kind.
pub fn wrap_flat_data(
    info: &BTreeClassInfo,
    flat_data: Vec<PickleValue>,
    next_ref: Option<PickleValue>,
) -> Result<PickleValue, CodecError> {
    let data_tuple = PickleValue::Tuple(flat_data);

    match info.kind {
        BTreeNodeKind::BTree | BTreeNodeKind::TreeSet => {
            // 4-level nesting: ((((<data>,),),),)
            // data_tuple is the innermost, then 3 more wrapping tuples
            let level3 = PickleValue::Tuple(vec![data_tuple]);
            let level2 = PickleValue::Tuple(vec![level3]);
            let level1 = PickleValue::Tuple(vec![level2]);
            Ok(level1)
        }
        BTreeNodeKind::Bucket | BTreeNodeKind::Set => {
            // 2-level nesting: ((<data>,),) or ((<data>,), next_ref)
            if let Some(next) = next_ref {
                Ok(PickleValue::Tuple(vec![data_tuple, next]))
            } else {
                Ok(PickleValue::Tuple(vec![data_tuple]))
            }
        }
    }
}

/// Decode a large BTree state with @children and @first.
fn decode_large_btree(
    children_val: &Value,
    first_val: &Value,
    from_json: &dyn Fn(&Value) -> Result<PickleValue, CodecError>,
) -> Result<PickleValue, CodecError> {
    let children_arr = children_val
        .as_array()
        .ok_or_else(|| CodecError::InvalidData("@children must be an array".into()))?;

    let children: Result<Vec<PickleValue>, _> =
        children_arr.iter().map(|item| from_json(item)).collect();
    let children_tuple = PickleValue::Tuple(children?);
    let firstbucket = from_json(first_val)?;

    Ok(PickleValue::Tuple(vec![children_tuple, firstbucket]))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::json::{json_to_pickle_value, pickle_value_to_json};

    // -- classify_btree --

    #[test]
    fn test_classify_oobtree() {
        let info = classify_btree("BTrees.OOBTree", "OOBTree").unwrap();
        assert_eq!(info.kind, BTreeNodeKind::BTree);
        assert!(info.is_map);
    }

    #[test]
    fn test_classify_iibucket() {
        let info = classify_btree("BTrees.IIBTree", "IIBucket").unwrap();
        assert_eq!(info.kind, BTreeNodeKind::Bucket);
        assert!(info.is_map);
    }

    #[test]
    fn test_classify_ootreeset() {
        let info = classify_btree("BTrees.OOBTree", "OOTreeSet").unwrap();
        assert_eq!(info.kind, BTreeNodeKind::TreeSet);
        assert!(!info.is_map);
    }

    #[test]
    fn test_classify_iiset() {
        let info = classify_btree("BTrees.IIBTree", "IISet").unwrap();
        assert_eq!(info.kind, BTreeNodeKind::Set);
        assert!(!info.is_map);
    }

    #[test]
    fn test_classify_length_returns_none() {
        assert!(classify_btree("BTrees.Length", "Length").is_none());
    }

    #[test]
    fn test_classify_non_btree_returns_none() {
        assert!(classify_btree("myapp.models", "Document").is_none());
    }

    #[test]
    fn test_classify_fsbtree() {
        let info = classify_btree("BTrees.fsBTree", "fsBucket").unwrap();
        assert_eq!(info.kind, BTreeNodeKind::Bucket);
        assert!(info.is_map);
    }

    #[test]
    fn test_classify_all_prefixes() {
        for prefix in &["OO", "IO", "OI", "II", "LO", "OL", "LL", "LF", "IF", "QQ"] {
            let module = format!("BTrees.{}BTree", prefix);
            let name = format!("{}BTree", prefix);
            assert!(
                classify_btree(&module, &name).is_some(),
                "failed for {prefix}BTree"
            );
        }
    }

    // -- btree_state_to_json: small BTree --

    #[test]
    fn test_small_oobtree_to_json() {
        let info = classify_btree("BTrees.OOBTree", "OOBTree").unwrap();
        // State: (((("a", 1, "b", 2),),),)
        let state = PickleValue::Tuple(vec![PickleValue::Tuple(vec![PickleValue::Tuple(
            vec![PickleValue::Tuple(vec![
                PickleValue::String("a".into()),
                PickleValue::Int(1),
                PickleValue::String("b".into()),
                PickleValue::Int(2),
            ])],
        )])]);
        let json = btree_state_to_json(&info, &state, &pickle_value_to_json).unwrap();
        assert_eq!(json, json!({"@kv": [["a", 1], ["b", 2]]}));
    }

    #[test]
    fn test_small_iibtree_to_json() {
        let info = classify_btree("BTrees.IIBTree", "IIBTree").unwrap();
        // State: ((((1, 100, 2, 200),),),)
        let state = PickleValue::Tuple(vec![PickleValue::Tuple(vec![PickleValue::Tuple(
            vec![PickleValue::Tuple(vec![
                PickleValue::Int(1),
                PickleValue::Int(100),
                PickleValue::Int(2),
                PickleValue::Int(200),
            ])],
        )])]);
        let json = btree_state_to_json(&info, &state, &pickle_value_to_json).unwrap();
        assert_eq!(json, json!({"@kv": [[1, 100], [2, 200]]}));
    }

    #[test]
    fn test_small_treeset_to_json() {
        let info = classify_btree("BTrees.IIBTree", "IITreeSet").unwrap();
        // State: ((((1, 2, 3),),),)
        let state = PickleValue::Tuple(vec![PickleValue::Tuple(vec![PickleValue::Tuple(
            vec![PickleValue::Tuple(vec![
                PickleValue::Int(1),
                PickleValue::Int(2),
                PickleValue::Int(3),
            ])],
        )])]);
        let json = btree_state_to_json(&info, &state, &pickle_value_to_json).unwrap();
        assert_eq!(json, json!({"@ks": [1, 2, 3]}));
    }

    // -- btree_state_to_json: Bucket/Set --

    #[test]
    fn test_bucket_to_json() {
        let info = classify_btree("BTrees.OOBTree", "OOBucket").unwrap();
        // State: (("x", 10, "y", 20),)
        let state = PickleValue::Tuple(vec![PickleValue::Tuple(vec![
            PickleValue::String("x".into()),
            PickleValue::Int(10),
            PickleValue::String("y".into()),
            PickleValue::Int(20),
        ])]);
        let json = btree_state_to_json(&info, &state, &pickle_value_to_json).unwrap();
        assert_eq!(json, json!({"@kv": [["x", 10], ["y", 20]]}));
    }

    #[test]
    fn test_set_to_json() {
        let info = classify_btree("BTrees.OOBTree", "OOSet").unwrap();
        // State: (("a", "b"),)
        let state = PickleValue::Tuple(vec![PickleValue::Tuple(vec![
            PickleValue::String("a".into()),
            PickleValue::String("b".into()),
        ])]);
        let json = btree_state_to_json(&info, &state, &pickle_value_to_json).unwrap();
        assert_eq!(json, json!({"@ks": ["a", "b"]}));
    }

    #[test]
    fn test_linked_bucket_to_json() {
        let info = classify_btree("BTrees.OOBTree", "OOBucket").unwrap();
        // State: (("a", 1), next_ref)
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
        let json = btree_state_to_json(&info, &state, &pickle_value_to_json).unwrap();
        let map = json.as_object().unwrap();
        assert!(map.contains_key("@kv"));
        assert!(map.contains_key("@next"));
        assert_eq!(map["@kv"], json!([["a", 1]]));
    }

    // -- btree_state_to_json: empty/None --

    #[test]
    fn test_empty_btree_to_json() {
        let info = classify_btree("BTrees.OOBTree", "OOBTree").unwrap();
        let json = btree_state_to_json(&info, &PickleValue::None, &pickle_value_to_json).unwrap();
        assert_eq!(json, Value::Null);
    }

    // -- btree_state_to_json: large BTree with refs --

    #[test]
    fn test_large_btree_to_json() {
        let info = classify_btree("BTrees.OOBTree", "OOBTree").unwrap();
        // State: ((ref0, "sep", ref1), firstbucket_ref)
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
            PickleValue::Tuple(vec![
                ref0,
                PickleValue::String("sep".into()),
                ref1,
            ]),
            first,
        ]);
        let json = btree_state_to_json(&info, &state, &pickle_value_to_json).unwrap();
        let map = json.as_object().unwrap();
        assert!(map.contains_key("@children"));
        assert!(map.contains_key("@first"));
        assert_eq!(map["@children"].as_array().unwrap().len(), 3);
    }

    // -- json_to_btree_state: roundtrips --

    #[test]
    fn test_roundtrip_small_btree() {
        let info = classify_btree("BTrees.OOBTree", "OOBTree").unwrap();
        let state = PickleValue::Tuple(vec![PickleValue::Tuple(vec![PickleValue::Tuple(
            vec![PickleValue::Tuple(vec![
                PickleValue::String("a".into()),
                PickleValue::Int(1),
                PickleValue::String("b".into()),
                PickleValue::Int(2),
            ])],
        )])]);
        let json = btree_state_to_json(&info, &state, &pickle_value_to_json).unwrap();
        let restored =
            json_to_btree_state(&info, &json, &json_to_pickle_value).unwrap();
        assert_eq!(state, restored);
    }

    #[test]
    fn test_roundtrip_small_treeset() {
        let info = classify_btree("BTrees.IIBTree", "IITreeSet").unwrap();
        let state = PickleValue::Tuple(vec![PickleValue::Tuple(vec![PickleValue::Tuple(
            vec![PickleValue::Tuple(vec![
                PickleValue::Int(1),
                PickleValue::Int(2),
                PickleValue::Int(3),
            ])],
        )])]);
        let json = btree_state_to_json(&info, &state, &pickle_value_to_json).unwrap();
        let restored =
            json_to_btree_state(&info, &json, &json_to_pickle_value).unwrap();
        assert_eq!(state, restored);
    }

    #[test]
    fn test_roundtrip_bucket() {
        let info = classify_btree("BTrees.OOBTree", "OOBucket").unwrap();
        let state = PickleValue::Tuple(vec![PickleValue::Tuple(vec![
            PickleValue::String("x".into()),
            PickleValue::Int(10),
            PickleValue::String("y".into()),
            PickleValue::Int(20),
        ])]);
        let json = btree_state_to_json(&info, &state, &pickle_value_to_json).unwrap();
        let restored =
            json_to_btree_state(&info, &json, &json_to_pickle_value).unwrap();
        assert_eq!(state, restored);
    }

    #[test]
    fn test_roundtrip_set() {
        let info = classify_btree("BTrees.OOBTree", "OOSet").unwrap();
        let state = PickleValue::Tuple(vec![PickleValue::Tuple(vec![
            PickleValue::String("a".into()),
            PickleValue::String("b".into()),
        ])]);
        let json = btree_state_to_json(&info, &state, &pickle_value_to_json).unwrap();
        let restored =
            json_to_btree_state(&info, &json, &json_to_pickle_value).unwrap();
        assert_eq!(state, restored);
    }

    #[test]
    fn test_roundtrip_empty_btree() {
        let info = classify_btree("BTrees.OOBTree", "OOBTree").unwrap();
        let json = json!(null);
        let restored =
            json_to_btree_state(&info, &json, &json_to_pickle_value).unwrap();
        assert_eq!(PickleValue::None, restored);
    }

    #[test]
    fn test_roundtrip_large_btree() {
        let info = classify_btree("BTrees.OOBTree", "OOBTree").unwrap();
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
            PickleValue::Tuple(vec![
                ref0,
                PickleValue::String("sep".into()),
                ref1,
            ]),
            first,
        ]);
        let json = btree_state_to_json(&info, &state, &pickle_value_to_json).unwrap();
        let restored =
            json_to_btree_state(&info, &json, &json_to_pickle_value).unwrap();
        assert_eq!(state, restored);
    }

    #[test]
    fn test_roundtrip_linked_bucket() {
        let info = classify_btree("BTrees.OOBTree", "OOBucket").unwrap();
        let next = PickleValue::PersistentRef(Box::new(PickleValue::Tuple(vec![
            PickleValue::Bytes(vec![0, 0, 0, 0, 0, 0, 0, 3]),
            PickleValue::None,
        ])));
        let state = PickleValue::Tuple(vec![
            PickleValue::Tuple(vec![
                PickleValue::String("a".into()),
                PickleValue::Int(1),
            ]),
            next,
        ]);
        let json = btree_state_to_json(&info, &state, &pickle_value_to_json).unwrap();
        let restored =
            json_to_btree_state(&info, &json, &json_to_pickle_value).unwrap();
        assert_eq!(state, restored);
    }

    #[test]
    fn test_empty_bucket_to_json() {
        let info = classify_btree("BTrees.OOBTree", "OOBucket").unwrap();
        // Empty bucket: ((),)
        let state = PickleValue::Tuple(vec![PickleValue::Tuple(vec![])]);
        let json = btree_state_to_json(&info, &state, &pickle_value_to_json).unwrap();
        assert_eq!(json, json!({"@kv": []}));
    }

    #[test]
    fn test_empty_btree_inline_to_json() {
        let info = classify_btree("BTrees.OOBTree", "OOBTree").unwrap();
        // Empty inline BTree: ((((),),),)
        let state = PickleValue::Tuple(vec![PickleValue::Tuple(vec![PickleValue::Tuple(
            vec![PickleValue::Tuple(vec![])],
        )])]);
        let json = btree_state_to_json(&info, &state, &pickle_value_to_json).unwrap();
        assert_eq!(json, json!({"@kv": []}));
    }

    #[test]
    fn test_format_flat_data_odd_items_error() {
        let info = BTreeClassInfo {
            kind: BTreeNodeKind::Bucket,
            is_map: true,
        };
        // 3 items — odd number for key-value pairs
        let items = vec![
            PickleValue::Int(1),
            PickleValue::String("one".to_string()),
            PickleValue::Int(2),
        ];
        let to_json = |v: &PickleValue| -> Result<serde_json::Value, CodecError> {
            match v {
                PickleValue::Int(i) => Ok(serde_json::json!(*i)),
                PickleValue::String(s) => Ok(serde_json::json!(s)),
                _ => Err(CodecError::InvalidData("unexpected".to_string())),
            }
        };
        let result = format_flat_data(&info, &items, &to_json);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("odd number"));
    }
}

//! Direct PickleValue ↔ PyObject conversion, bypassing serde_json::Value.
//!
//! This module provides the fast path for the Python dict/ZODB APIs
//! (`pickle_to_dict`, `dict_to_pickle`, `decode_zodb_record`, `encode_zodb_record`).
//! It handles all JSON markers, known type detection, BTree flattening, and
//! persistent ref compact/expand in a single tree walk.
//!
//! The JSON string API (`pickle_to_json`, `json_to_pickle`) still uses
//! json.rs + serde_json::Value.

use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use pyo3::prelude::*;
use pyo3::intern;
use pyo3::types::{PyBool, PyDict, PyFloat, PyInt, PyList, PyString};

use crate::btrees;
use crate::error::CodecError;
use crate::known_types;
use crate::types::PickleValue;

// ---------------------------------------------------------------------------
// Forward direction: PickleValue → PyObject
// ---------------------------------------------------------------------------

/// Convert a PickleValue AST directly to a Python object with marker dicts.
///
/// When `compact_refs` is true, ZODB persistent references are compacted inline
/// (hex OID strings instead of nested tuple/bytes).
pub fn pickle_value_to_pyobject(
    py: Python<'_>,
    val: &PickleValue,
    compact_refs: bool,
) -> PyResult<PyObject> {
    match val {
        PickleValue::None => Ok(py.None()),
        PickleValue::Bool(b) => Ok(b.into_pyobject(py)?.to_owned().into_any().unbind()),
        PickleValue::Int(i) => Ok(i.into_pyobject(py)?.into_any().unbind()),
        PickleValue::BigInt(bi) => {
            let dict = PyDict::new(py);
            dict.set_item(intern!(py, "@bi"), bi.to_string())?;
            Ok(dict.into_any().unbind())
        }
        PickleValue::Float(f) => Ok(f.into_pyobject(py)?.into_any().unbind()),
        PickleValue::String(s) => Ok(s.into_pyobject(py)?.into_any().unbind()),
        PickleValue::Bytes(b) => {
            let dict = PyDict::new(py);
            dict.set_item(intern!(py, "@b"), BASE64.encode(b))?;
            Ok(dict.into_any().unbind())
        }
        PickleValue::List(items) => {
            let py_items: PyResult<Vec<PyObject>> = items
                .iter()
                .map(|item| pickle_value_to_pyobject(py, item, compact_refs))
                .collect();
            let list = PyList::new(py, py_items?)?;
            Ok(list.into_any().unbind())
        }
        PickleValue::Tuple(items) => {
            let py_items: PyResult<Vec<PyObject>> = items
                .iter()
                .map(|item| pickle_value_to_pyobject(py, item, compact_refs))
                .collect();
            let list = PyList::new(py, py_items?)?;
            let dict = PyDict::new(py);
            dict.set_item(intern!(py, "@t"), list)?;
            Ok(dict.into_any().unbind())
        }
        PickleValue::Dict(pairs) => {
            // Single-pass: optimistically build string-key PyDict.
            // >99% of ZODB dicts have all string keys — skip the pre-scan.
            let dict = PyDict::new(py);
            for (k, v) in pairs {
                if let PickleValue::String(key) = k {
                    dict.set_item(key, pickle_value_to_pyobject(py, v, compact_refs)?)?;
                } else {
                    // Rare: non-string key found — fall back to @d format.
                    // Re-process all pairs (this path is almost never hit).
                    let py_pairs: PyResult<Vec<PyObject>> = pairs
                        .iter()
                        .map(|(k, v)| {
                            let pk = pickle_value_to_pyobject(py, k, compact_refs)?;
                            let pv = pickle_value_to_pyobject(py, v, compact_refs)?;
                            let pair = PyList::new(py, [pk, pv])?;
                            Ok(pair.into_any().unbind())
                        })
                        .collect();
                    let arr = PyList::new(py, py_pairs?)?;
                    let d = PyDict::new(py);
                    d.set_item(intern!(py, "@d"), arr)?;
                    return Ok(d.into_any().unbind());
                }
            }
            Ok(dict.into_any().unbind())
        }
        PickleValue::Set(items) => {
            let py_items: PyResult<Vec<PyObject>> = items
                .iter()
                .map(|item| pickle_value_to_pyobject(py, item, compact_refs))
                .collect();
            let list = PyList::new(py, py_items?)?;
            let dict = PyDict::new(py);
            dict.set_item(intern!(py, "@set"), list)?;
            Ok(dict.into_any().unbind())
        }
        PickleValue::FrozenSet(items) => {
            let py_items: PyResult<Vec<PyObject>> = items
                .iter()
                .map(|item| pickle_value_to_pyobject(py, item, compact_refs))
                .collect();
            let list = PyList::new(py, py_items?)?;
            let dict = PyDict::new(py);
            dict.set_item(intern!(py, "@fset"), list)?;
            Ok(dict.into_any().unbind())
        }
        PickleValue::Global { module, name } => {
            let cls_list = PyList::new(py, [module.as_str(), name.as_str()])?;
            let dict = PyDict::new(py);
            dict.set_item(intern!(py, "@cls"), cls_list)?;
            Ok(dict.into_any().unbind())
        }
        PickleValue::Instance { module, name, state } => {
            // Try known type handlers first (e.g., uuid.UUID)
            if let Some(obj) =
                try_instance_to_pyobject(py, module, name, state, compact_refs)?
            {
                return Ok(obj);
            }
            // Try BTree state flattening
            let state_obj = if let Some(info) = btrees::classify_btree(module, name) {
                btree_state_to_pyobject(py, &info, state, compact_refs)?
            } else {
                pickle_value_to_pyobject(py, state, compact_refs)?
            };
            if module.is_empty() && name.is_empty() {
                // Anonymous instance
                let dict = PyDict::new(py);
                dict.set_item(intern!(py, "@inst"), state_obj)?;
                Ok(dict.into_any().unbind())
            } else {
                let cls_list = PyList::new(py, [module.as_str(), name.as_str()])?;
                let dict = PyDict::new(py);
                dict.set_item(intern!(py, "@cls"), cls_list)?;
                dict.set_item(intern!(py, "@s"), state_obj)?;
                Ok(dict.into_any().unbind())
            }
        }
        PickleValue::PersistentRef(inner) => {
            if compact_refs {
                compact_ref_to_pyobject(py, inner, compact_refs)
            } else {
                let inner_obj = pickle_value_to_pyobject(py, inner, compact_refs)?;
                let dict = PyDict::new(py);
                dict.set_item(intern!(py, "@ref"), inner_obj)?;
                Ok(dict.into_any().unbind())
            }
        }
        PickleValue::Reduce { callable, args } => {
            // Try known type handlers first (datetime, Decimal, set, etc.)
            if let Some(obj) =
                try_reduce_to_pyobject(py, callable, args, compact_refs)?
            {
                return Ok(obj);
            }
            // Fall back to generic @reduce
            let callable_obj = pickle_value_to_pyobject(py, callable, compact_refs)?;
            let args_obj = pickle_value_to_pyobject(py, args, compact_refs)?;
            let inner_dict = PyDict::new(py);
            inner_dict.set_item(intern!(py, "callable"), callable_obj)?;
            inner_dict.set_item(intern!(py, "args"), args_obj)?;
            let dict = PyDict::new(py);
            dict.set_item(intern!(py, "@reduce"), inner_dict)?;
            Ok(dict.into_any().unbind())
        }
        PickleValue::RawPickle(data) => {
            let dict = PyDict::new(py);
            dict.set_item(intern!(py, "@pkl"), BASE64.encode(data))?;
            Ok(dict.into_any().unbind())
        }
    }
}

// ---------------------------------------------------------------------------
// Forward: persistent ref compaction
// ---------------------------------------------------------------------------

/// Compact a ZODB persistent ref directly to PyObject.
/// inner is typically Tuple([Bytes(oid), None_or_Global])
fn compact_ref_to_pyobject(
    py: Python<'_>,
    inner: &PickleValue,
    compact_refs: bool,
) -> PyResult<PyObject> {
    if let PickleValue::Tuple(items) = inner {
        if items.len() == 2 {
            if let PickleValue::Bytes(oid) = &items[0] {
                let hex = hex::encode(oid);
                let dict = PyDict::new(py);
                match &items[1] {
                    PickleValue::None => {
                        dict.set_item(intern!(py, "@ref"), &hex)?;
                        return Ok(dict.into_any().unbind());
                    }
                    PickleValue::Global { module, name } => {
                        let class_path = if module.is_empty() {
                            name.clone()
                        } else {
                            format!("{module}.{name}")
                        };
                        let ref_list = PyList::new(py, [hex.as_str(), class_path.as_str()])?;
                        dict.set_item(intern!(py, "@ref"), ref_list)?;
                        return Ok(dict.into_any().unbind());
                    }
                    _ => {}
                }
            }
        }
    }
    // Fallback: generic ref
    let inner_obj = pickle_value_to_pyobject(py, inner, compact_refs)?;
    let dict = PyDict::new(py);
    dict.set_item(intern!(py, "@ref"), inner_obj)?;
    Ok(dict.into_any().unbind())
}

// ---------------------------------------------------------------------------
// Forward: known type handlers (PickleValue → PyObject)
// ---------------------------------------------------------------------------

/// Try to convert a known REDUCE to a compact typed PyObject.
fn try_reduce_to_pyobject(
    py: Python<'_>,
    callable: &PickleValue,
    args: &PickleValue,
    compact_refs: bool,
) -> PyResult<Option<PyObject>> {
    let (module, name) = match callable {
        PickleValue::Global { module, name } => (module.as_str(), name.as_str()),
        _ => return Ok(None),
    };

    match (module, name) {
        ("datetime", "datetime") => encode_datetime_pyobject(py, args, compact_refs),
        ("datetime", "date") => encode_date_pyobject(py, args),
        ("datetime", "time") => encode_time_pyobject(py, args, compact_refs),
        ("datetime", "timedelta") => encode_timedelta_pyobject(py, args),
        ("decimal", "Decimal") => encode_decimal_pyobject(py, args),
        ("builtins", "set") => encode_set_pyobject(py, args, compact_refs),
        ("builtins", "frozenset") => encode_frozenset_pyobject(py, args, compact_refs),
        _ => Ok(None),
    }
}

fn encode_datetime_pyobject(
    py: Python<'_>,
    args: &PickleValue,
    _compact_refs: bool,
) -> PyResult<Option<PyObject>> {
    let tuple_items = match args {
        PickleValue::Tuple(items) => items,
        _ => return Ok(None),
    };
    let dt_bytes = match tuple_items.first() {
        Some(PickleValue::Bytes(b)) if b.len() == 10 => b,
        _ => return Ok(None),
    };
    let (year, month, day, hour, min, sec, us) = match known_types::decode_datetime_bytes(dt_bytes)
    {
        Some(v) => v,
        None => return Ok(None),
    };
    let iso = known_types::format_datetime_iso(year, month, day, hour, min, sec, us);

    let dict = PyDict::new(py);
    if tuple_items.len() == 1 {
        // Naive datetime
        dict.set_item(intern!(py, "@dt"), &iso)?;
        Ok(Some(dict.into_any().unbind()))
    } else if tuple_items.len() == 2 {
        // We need a to_json callback for extract_tz_info — use a dummy that
        // only handles the pytz args case (simple types: string, int).
        let to_json_dummy =
            |pv: &PickleValue| -> Result<serde_json::Value, CodecError> {
                match pv {
                    PickleValue::String(s) => Ok(serde_json::Value::String(s.clone())),
                    PickleValue::Int(i) => Ok(serde_json::json!(*i)),
                    PickleValue::Float(f) => Ok(serde_json::json!(*f)),
                    PickleValue::None => Ok(serde_json::Value::Null),
                    _ => Ok(serde_json::Value::Null),
                }
            };
        match known_types::extract_tz_info(&tuple_items[1], &to_json_dummy)? {
            Some(known_types::TzInfo::FixedOffset(secs)) => {
                let offset = known_types::format_offset(secs);
                dict.set_item(intern!(py, "@dt"), format!("{iso}{offset}"))?;
                Ok(Some(dict.into_any().unbind()))
            }
            Some(known_types::TzInfo::PytzUtc) => {
                dict.set_item(intern!(py, "@dt"), format!("{iso}+00:00"))?;
                Ok(Some(dict.into_any().unbind()))
            }
            Some(known_types::TzInfo::Pytz { name, args: tz_args }) => {
                dict.set_item(intern!(py, "@dt"), &iso)?;
                let tz_dict = PyDict::new(py);
                let py_args: PyResult<Vec<PyObject>> = tz_args
                    .iter()
                    .map(|a| json_value_to_simple_pyobject(py, a))
                    .collect();
                let py_list = PyList::new(py, py_args?)?;
                tz_dict.set_item(intern!(py, "pytz"), py_list)?;
                tz_dict.set_item(intern!(py, "name"), &name)?;
                dict.set_item(intern!(py, "@tz"), tz_dict)?;
                Ok(Some(dict.into_any().unbind()))
            }
            Some(known_types::TzInfo::ZoneInfo(key)) => {
                dict.set_item(intern!(py, "@dt"), &iso)?;
                let tz_dict = PyDict::new(py);
                tz_dict.set_item(intern!(py, "zoneinfo"), &key)?;
                dict.set_item(intern!(py, "@tz"), tz_dict)?;
                Ok(Some(dict.into_any().unbind()))
            }
            None => {
                // Unknown tz — fall through to generic @reduce
                Ok(None)
            }
        }
    } else {
        Ok(None)
    }
}

/// Convert a serde_json::Value (only simple types) to a PyObject.
/// Used for pytz timezone args which come back from extract_tz_info as serde_json::Value.
fn json_value_to_simple_pyobject(
    py: Python<'_>,
    val: &serde_json::Value,
) -> PyResult<PyObject> {
    match val {
        serde_json::Value::String(s) => Ok(s.into_pyobject(py)?.into_any().unbind()),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Ok(i.into_pyobject(py)?.into_any().unbind())
            } else if let Some(f) = n.as_f64() {
                Ok(f.into_pyobject(py)?.into_any().unbind())
            } else {
                Ok(py.None())
            }
        }
        serde_json::Value::Null => Ok(py.None()),
        _ => Ok(py.None()),
    }
}

fn encode_date_pyobject(
    py: Python<'_>,
    args: &PickleValue,
) -> PyResult<Option<PyObject>> {
    let tuple_items = match args {
        PickleValue::Tuple(items) if items.len() == 1 => items,
        _ => return Ok(None),
    };
    let bytes = match &tuple_items[0] {
        PickleValue::Bytes(b) if b.len() == 4 => b,
        _ => return Ok(None),
    };
    let year = (bytes[0] as u16) * 256 + bytes[1] as u16;
    let month = bytes[2];
    let day = bytes[3];
    let dict = PyDict::new(py);
    dict.set_item(intern!(py, "@date"), format!("{year:04}-{month:02}-{day:02}"))?;
    Ok(Some(dict.into_any().unbind()))
}

fn encode_time_pyobject(
    py: Python<'_>,
    args: &PickleValue,
    _compact_refs: bool,
) -> PyResult<Option<PyObject>> {
    let tuple_items = match args {
        PickleValue::Tuple(items) if !items.is_empty() => items,
        _ => return Ok(None),
    };
    let bytes = match &tuple_items[0] {
        PickleValue::Bytes(b) if b.len() == 6 => b,
        _ => return Ok(None),
    };
    let (hour, min, sec, us) = match known_types::decode_time_bytes(bytes) {
        Some(v) => v,
        None => return Ok(None),
    };
    let time_str = if us > 0 {
        format!("{hour:02}:{min:02}:{sec:02}.{us:06}")
    } else {
        format!("{hour:02}:{min:02}:{sec:02}")
    };

    let dict = PyDict::new(py);
    if tuple_items.len() == 1 {
        dict.set_item(intern!(py, "@time"), &time_str)?;
        Ok(Some(dict.into_any().unbind()))
    } else if tuple_items.len() == 2 {
        let to_json_dummy =
            |pv: &PickleValue| -> Result<serde_json::Value, CodecError> {
                match pv {
                    PickleValue::String(s) => Ok(serde_json::Value::String(s.clone())),
                    PickleValue::Int(i) => Ok(serde_json::json!(*i)),
                    _ => Ok(serde_json::Value::Null),
                }
            };
        match known_types::extract_tz_info(&tuple_items[1], &to_json_dummy)? {
            Some(known_types::TzInfo::FixedOffset(secs)) => {
                let offset = known_types::format_offset(secs);
                dict.set_item(intern!(py, "@time"), format!("{time_str}{offset}"))?;
                Ok(Some(dict.into_any().unbind()))
            }
            Some(known_types::TzInfo::PytzUtc) => {
                dict.set_item(intern!(py, "@time"), format!("{time_str}+00:00"))?;
                Ok(Some(dict.into_any().unbind()))
            }
            Some(known_types::TzInfo::Pytz { name, args: tz_args }) => {
                dict.set_item(intern!(py, "@time"), &time_str)?;
                let tz_dict = PyDict::new(py);
                let py_args: PyResult<Vec<PyObject>> = tz_args
                    .iter()
                    .map(|a| json_value_to_simple_pyobject(py, a))
                    .collect();
                let py_list = PyList::new(py, py_args?)?;
                tz_dict.set_item(intern!(py, "pytz"), py_list)?;
                tz_dict.set_item(intern!(py, "name"), &name)?;
                dict.set_item(intern!(py, "@tz"), tz_dict)?;
                Ok(Some(dict.into_any().unbind()))
            }
            Some(known_types::TzInfo::ZoneInfo(key)) => {
                dict.set_item(intern!(py, "@time"), &time_str)?;
                let tz_dict = PyDict::new(py);
                tz_dict.set_item(intern!(py, "zoneinfo"), &key)?;
                dict.set_item(intern!(py, "@tz"), tz_dict)?;
                Ok(Some(dict.into_any().unbind()))
            }
            None => {
                // Unknown tz — fall through to generic @reduce
                Ok(None)
            }
        }
    } else {
        Ok(None)
    }
}

fn encode_timedelta_pyobject(
    py: Python<'_>,
    args: &PickleValue,
) -> PyResult<Option<PyObject>> {
    let tuple_items = match args {
        PickleValue::Tuple(items) if items.len() == 3 => items,
        _ => return Ok(None),
    };
    let days = match &tuple_items[0] {
        PickleValue::Int(i) => *i,
        _ => return Ok(None),
    };
    let secs = match &tuple_items[1] {
        PickleValue::Int(i) => *i,
        _ => return Ok(None),
    };
    let us = match &tuple_items[2] {
        PickleValue::Int(i) => *i,
        _ => return Ok(None),
    };
    let list = PyList::new(py, [days, secs, us])?;
    let dict = PyDict::new(py);
    dict.set_item(intern!(py, "@td"), list)?;
    Ok(Some(dict.into_any().unbind()))
}

fn encode_decimal_pyobject(
    py: Python<'_>,
    args: &PickleValue,
) -> PyResult<Option<PyObject>> {
    let tuple_items = match args {
        PickleValue::Tuple(items) if items.len() == 1 => items,
        _ => return Ok(None),
    };
    let s = match &tuple_items[0] {
        PickleValue::String(s) => s,
        _ => return Ok(None),
    };
    let dict = PyDict::new(py);
    dict.set_item(intern!(py, "@dec"), s.as_str())?;
    Ok(Some(dict.into_any().unbind()))
}

fn encode_set_pyobject(
    py: Python<'_>,
    args: &PickleValue,
    compact_refs: bool,
) -> PyResult<Option<PyObject>> {
    let tuple_items = match args {
        PickleValue::Tuple(items) if items.len() == 1 => items,
        _ => return Ok(None),
    };
    let list_items = match &tuple_items[0] {
        PickleValue::List(items) => items,
        _ => return Ok(None),
    };
    let py_items: PyResult<Vec<PyObject>> = list_items
        .iter()
        .map(|i| pickle_value_to_pyobject(py, i, compact_refs))
        .collect();
    let list = PyList::new(py, py_items?)?;
    let dict = PyDict::new(py);
    dict.set_item(intern!(py, "@set"), list)?;
    Ok(Some(dict.into_any().unbind()))
}

fn encode_frozenset_pyobject(
    py: Python<'_>,
    args: &PickleValue,
    compact_refs: bool,
) -> PyResult<Option<PyObject>> {
    let tuple_items = match args {
        PickleValue::Tuple(items) if items.len() == 1 => items,
        _ => return Ok(None),
    };
    let list_items = match &tuple_items[0] {
        PickleValue::List(items) => items,
        _ => return Ok(None),
    };
    let py_items: PyResult<Vec<PyObject>> = list_items
        .iter()
        .map(|i| pickle_value_to_pyobject(py, i, compact_refs))
        .collect();
    let list = PyList::new(py, py_items?)?;
    let dict = PyDict::new(py);
    dict.set_item(intern!(py, "@fset"), list)?;
    Ok(Some(dict.into_any().unbind()))
}

/// Try to convert a known Instance to a compact typed PyObject.
fn try_instance_to_pyobject(
    py: Python<'_>,
    module: &str,
    name: &str,
    state: &PickleValue,
    _compact_refs: bool,
) -> PyResult<Option<PyObject>> {
    match (module, name) {
        ("uuid", "UUID") => encode_uuid_pyobject(py, state),
        _ => Ok(None),
    }
}

fn encode_uuid_pyobject(
    py: Python<'_>,
    state: &PickleValue,
) -> PyResult<Option<PyObject>> {
    let pairs = match state {
        PickleValue::Dict(pairs) => pairs,
        _ => return Ok(None),
    };
    for (k, v) in pairs {
        if let PickleValue::String(key) = k {
            if key == "int" {
                let int_val = match v {
                    PickleValue::Int(i) => *i as u128,
                    PickleValue::BigInt(bi) => {
                        let (_, bytes) = bi.to_bytes_be();
                        let mut val: u128 = 0;
                        for b in bytes {
                            val = (val << 8) | b as u128;
                        }
                        val
                    }
                    _ => return Ok(None),
                };
                let hex = format!("{int_val:032x}");
                let uuid_str = format!(
                    "{}-{}-{}-{}-{}",
                    &hex[0..8],
                    &hex[8..12],
                    &hex[12..16],
                    &hex[16..20],
                    &hex[20..32]
                );
                let dict = PyDict::new(py);
                dict.set_item(intern!(py, "@uuid"), &uuid_str)?;
                return Ok(Some(dict.into_any().unbind()));
            }
        }
    }
    Ok(None)
}

// ---------------------------------------------------------------------------
// Forward: BTree state → PyObject
// ---------------------------------------------------------------------------

/// Convert a BTree state PickleValue to flattened PyObject.
pub fn btree_state_to_pyobject(
    py: Python<'_>,
    info: &btrees::BTreeClassInfo,
    state: &PickleValue,
    compact_refs: bool,
) -> PyResult<PyObject> {
    // Empty BTree: state is None
    if *state == PickleValue::None {
        return Ok(py.None());
    }

    match info.kind {
        btrees::BTreeNodeKind::BTree | btrees::BTreeNodeKind::TreeSet => {
            btree_node_to_pyobject(py, info, state, compact_refs)
        }
        btrees::BTreeNodeKind::Bucket | btrees::BTreeNodeKind::Set => {
            bucket_to_pyobject(py, info, state, compact_refs)
        }
    }
}

fn btree_node_to_pyobject(
    py: Python<'_>,
    info: &btrees::BTreeClassInfo,
    state: &PickleValue,
    compact_refs: bool,
) -> PyResult<PyObject> {
    let outer = match state {
        PickleValue::Tuple(items) => items,
        _ => return pickle_value_to_pyobject(py, state, compact_refs),
    };

    // Small inline BTree — 1-tuple
    if outer.len() == 1 {
        if let Some(flat_data) = btrees::unwrap_inline_btree(&outer[0]) {
            return format_flat_data_pyobject(py, info, flat_data, compact_refs);
        }
        return pickle_value_to_pyobject(py, state, compact_refs);
    }

    // Large BTree with persistent refs — 2-tuple
    if outer.len() == 2 {
        if let PickleValue::Tuple(children) = &outer[0] {
            if btrees::children_has_refs(children) {
                let py_children: PyResult<Vec<PyObject>> = children
                    .iter()
                    .map(|item| pickle_value_to_pyobject(py, item, compact_refs))
                    .collect();
                let children_list = PyList::new(py, py_children?)?;
                let first_obj = pickle_value_to_pyobject(py, &outer[1], compact_refs)?;
                let dict = PyDict::new(py);
                dict.set_item(intern!(py, "@children"), children_list)?;
                dict.set_item(intern!(py, "@first"), first_obj)?;
                return Ok(dict.into_any().unbind());
            }
        }
        return pickle_value_to_pyobject(py, state, compact_refs);
    }

    pickle_value_to_pyobject(py, state, compact_refs)
}

fn bucket_to_pyobject(
    py: Python<'_>,
    info: &btrees::BTreeClassInfo,
    state: &PickleValue,
    compact_refs: bool,
) -> PyResult<PyObject> {
    let outer = match state {
        PickleValue::Tuple(items) => items,
        _ => return pickle_value_to_pyobject(py, state, compact_refs),
    };

    // Standalone bucket — 1-tuple
    if outer.len() == 1 {
        if let PickleValue::Tuple(flat_data) = &outer[0] {
            return format_flat_data_pyobject(py, info, flat_data, compact_refs);
        }
        return pickle_value_to_pyobject(py, state, compact_refs);
    }

    // Linked bucket — 2-tuple: (flat_data, next_ref)
    if outer.len() == 2 {
        if let PickleValue::Tuple(flat_data) = &outer[0] {
            let dict = PyDict::new(py);
            if info.is_map {
                let mut pairs = Vec::new();
                let mut i = 0;
                while i + 1 < flat_data.len() {
                    let k = pickle_value_to_pyobject(py, &flat_data[i], compact_refs)?;
                    let v = pickle_value_to_pyobject(py, &flat_data[i + 1], compact_refs)?;
                    let pair = PyList::new(py, [k, v])?;
                    pairs.push(pair.into_any().unbind());
                    i += 2;
                }
                let kv_list = PyList::new(py, pairs)?;
                dict.set_item(intern!(py, "@kv"), kv_list)?;
            } else {
                let py_keys: PyResult<Vec<PyObject>> = flat_data
                    .iter()
                    .map(|item| pickle_value_to_pyobject(py, item, compact_refs))
                    .collect();
                let ks_list = PyList::new(py, py_keys?)?;
                dict.set_item(intern!(py, "@ks"), ks_list)?;
            }
            let next_obj = pickle_value_to_pyobject(py, &outer[1], compact_refs)?;
            dict.set_item(intern!(py, "@next"), next_obj)?;
            return Ok(dict.into_any().unbind());
        }
        return pickle_value_to_pyobject(py, state, compact_refs);
    }

    pickle_value_to_pyobject(py, state, compact_refs)
}

fn format_flat_data_pyobject(
    py: Python<'_>,
    info: &btrees::BTreeClassInfo,
    items: &[PickleValue],
    compact_refs: bool,
) -> PyResult<PyObject> {
    let dict = PyDict::new(py);
    if info.is_map {
        let mut pairs = Vec::with_capacity(items.len() / 2);
        let mut i = 0;
        while i + 1 < items.len() {
            let k = pickle_value_to_pyobject(py, &items[i], compact_refs)?;
            let v = pickle_value_to_pyobject(py, &items[i + 1], compact_refs)?;
            let pair = PyList::new(py, [k, v])?;
            pairs.push(pair.into_any().unbind());
            i += 2;
        }
        let kv_list = PyList::new(py, pairs)?;
        dict.set_item(intern!(py, "@kv"), kv_list)?;
    } else {
        let py_keys: PyResult<Vec<PyObject>> = items
            .iter()
            .map(|item| pickle_value_to_pyobject(py, item, compact_refs))
            .collect();
        let ks_list = PyList::new(py, py_keys?)?;
        dict.set_item(intern!(py, "@ks"), ks_list)?;
    }
    Ok(dict.into_any().unbind())
}

// ---------------------------------------------------------------------------
// Reverse direction: PyObject → PickleValue
// ---------------------------------------------------------------------------

/// Convert a Python object to a PickleValue AST with marker detection.
///
/// When `expand_refs` is true, compact ZODB persistent refs are expanded inline.
pub fn pyobject_to_pickle_value(
    obj: &Bound<'_, pyo3::PyAny>,
    expand_refs: bool,
) -> PyResult<PickleValue> {
    // Ordered by frequency in ZODB data: string > dict > int > none > float > list > bool
    if obj.is_instance_of::<PyString>() {
        let s: String = obj.extract()?;
        return Ok(PickleValue::String(s));
    }
    if obj.is_instance_of::<PyDict>() {
        let dict = obj.downcast::<PyDict>()?;
        return pydict_to_pickle_value(dict, expand_refs);
    }
    if obj.is_none() {
        return Ok(PickleValue::None);
    }
    // Check bool BEFORE int (bool is a subclass of int in Python)
    if obj.is_instance_of::<PyBool>() {
        let b: bool = obj.extract()?;
        return Ok(PickleValue::Bool(b));
    }
    if obj.is_instance_of::<PyInt>() {
        let i: i64 = obj.extract()?;
        return Ok(PickleValue::Int(i));
    }
    if obj.is_instance_of::<PyFloat>() {
        let f: f64 = obj.extract()?;
        return Ok(PickleValue::Float(f));
    }
    if obj.is_instance_of::<PyList>() {
        let list = obj.downcast::<PyList>()?;
        let items: PyResult<Vec<PickleValue>> = list
            .iter()
            .map(|item| pyobject_to_pickle_value(&item, expand_refs))
            .collect();
        return Ok(PickleValue::List(items?));
    }
    // Fallback: try str() representation
    let s = obj.str()?.to_string();
    Ok(PickleValue::String(s))
}

/// Convert a PyDict to PickleValue, checking for marker keys.
fn pydict_to_pickle_value(
    dict: &Bound<'_, PyDict>,
    expand_refs: bool,
) -> PyResult<PickleValue> {
    let py = dict.py();
    let len = dict.len();

    // Fast path: no JSON marker dict has more than 4 keys.
    // Skip all marker checks for large plain dicts.
    if len > 4 {
        let mut pairs = Vec::with_capacity(len);
        for (k, v) in dict {
            let key: String = k.extract()?;
            pairs.push((
                PickleValue::String(key),
                pyobject_to_pickle_value(&v, expand_refs)?,
            ));
        }
        return Ok(PickleValue::Dict(pairs));
    }

    // Fast path: if no key starts with '@', skip all marker checks.
    // This saves up to 15 hash-based get_item lookups per small data dict.
    // For deep_nesting (10 levels × 15 lookups = 150 wasted C API calls), this is huge.
    {
        let mut has_marker_key = false;
        for (k, _) in dict {
            if let Ok(s) = k.downcast::<PyString>() {
                if let Ok(key_str) = s.to_str() {
                    if key_str.starts_with('@') {
                        has_marker_key = true;
                        break;
                    }
                }
            }
        }
        if !has_marker_key {
            let mut pairs = Vec::with_capacity(len);
            for (k, v) in dict {
                let key: String = k.extract()?;
                pairs.push((
                    PickleValue::String(key),
                    pyobject_to_pickle_value(&v, expand_refs)?,
                ));
            }
            return Ok(PickleValue::Dict(pairs));
        }
    }

    // Check for markers in priority order (using interned strings)

    // @t — Tuple
    if let Some(v) = dict.get_item(intern!(py, "@t"))? {
        if let Ok(list) = v.downcast::<PyList>() {
            let items: PyResult<Vec<PickleValue>> = list
                .iter()
                .map(|item| pyobject_to_pickle_value(&item, expand_refs))
                .collect();
            return Ok(PickleValue::Tuple(items?));
        }
    }

    // @b — Bytes
    if let Some(v) = dict.get_item(intern!(py, "@b"))? {
        if let Ok(s) = v.extract::<String>() {
            let bytes = BASE64
                .decode(&s)
                .map_err(|e| CodecError::Json(format!("base64 decode: {e}")))?;
            return Ok(PickleValue::Bytes(bytes));
        }
    }

    // @bi — BigInt
    if let Some(v) = dict.get_item(intern!(py, "@bi"))? {
        if let Ok(s) = v.extract::<String>() {
            let bi: num_bigint::BigInt = s
                .parse()
                .map_err(|e| CodecError::Json(format!("bigint parse: {e}")))?;
            return Ok(PickleValue::BigInt(bi));
        }
    }

    // @d — Dict with non-string keys
    if let Some(v) = dict.get_item(intern!(py, "@d"))? {
        if let Ok(list) = v.downcast::<PyList>() {
            let mut pairs = Vec::with_capacity(list.len());
            for pair_obj in list.iter() {
                if let Ok(pair_list) = pair_obj.downcast::<PyList>() {
                    if pair_list.len() == 2 {
                        let k = pyobject_to_pickle_value(&pair_list.get_item(0)?, expand_refs)?;
                        let v = pyobject_to_pickle_value(&pair_list.get_item(1)?, expand_refs)?;
                        pairs.push((k, v));
                    }
                }
            }
            return Ok(PickleValue::Dict(pairs));
        }
    }

    // @set — Set
    if let Some(v) = dict.get_item(intern!(py, "@set"))? {
        if let Ok(list) = v.downcast::<PyList>() {
            let items: PyResult<Vec<PickleValue>> = list
                .iter()
                .map(|item| pyobject_to_pickle_value(&item, expand_refs))
                .collect();
            return Ok(PickleValue::Set(items?));
        }
    }

    // @fset — FrozenSet
    if let Some(v) = dict.get_item(intern!(py, "@fset"))? {
        if let Ok(list) = v.downcast::<PyList>() {
            let items: PyResult<Vec<PickleValue>> = list
                .iter()
                .map(|item| pyobject_to_pickle_value(&item, expand_refs))
                .collect();
            return Ok(PickleValue::FrozenSet(items?));
        }
    }

    // @ref — Persistent reference
    if let Some(v) = dict.get_item(intern!(py, "@ref"))? {
        if expand_refs {
            return expand_compact_ref(&v);
        } else {
            let inner = pyobject_to_pickle_value(&v, expand_refs)?;
            return Ok(PickleValue::PersistentRef(Box::new(inner)));
        }
    }

    // @pkl — Raw pickle
    if let Some(v) = dict.get_item(intern!(py, "@pkl"))? {
        if let Ok(s) = v.extract::<String>() {
            let bytes = BASE64
                .decode(&s)
                .map_err(|e| CodecError::Json(format!("base64 decode: {e}")))?;
            return Ok(PickleValue::RawPickle(bytes));
        }
    }

    // Known type markers: @dt, @date, @time, @td, @dec, @uuid
    if let Some(pv) = try_typed_pydict_to_pickle_value(dict, expand_refs)? {
        return Ok(pv);
    }

    // @cls — Instance or Global (combined check to avoid double lookup)
    if let Some(cls_val) = dict.get_item(intern!(py, "@cls"))? {
        if let Ok(cls_list) = cls_val.downcast::<PyList>() {
            if cls_list.len() == 2 {
                let module: String = cls_list.get_item(0)?.extract()?;
                let name: String = cls_list.get_item(1)?.extract()?;
                // @cls + @s — Instance
                if let Some(state_val) = dict.get_item(intern!(py, "@s"))? {
                    let state = if let Some(info) = btrees::classify_btree(&module, &name) {
                        btree_state_from_pyobject(&info, &state_val, expand_refs)?
                    } else {
                        pyobject_to_pickle_value(&state_val, expand_refs)?
                    };
                    return Ok(PickleValue::Instance {
                        module,
                        name,
                        state: Box::new(state),
                    });
                }
                // @cls alone — Global reference
                return Ok(PickleValue::Global { module, name });
            }
        }
    }

    // @reduce — Generic reduce
    if let Some(v) = dict.get_item(intern!(py, "@reduce"))? {
        if let Ok(reduce_dict) = v.downcast::<PyDict>() {
            let callable_obj = reduce_dict
                .get_item(intern!(py, "callable"))?
                .unwrap_or_else(|| py.None().into_bound(py));
            let args_obj = reduce_dict
                .get_item(intern!(py, "args"))?
                .unwrap_or_else(|| py.None().into_bound(py));
            let callable = pyobject_to_pickle_value(&callable_obj, expand_refs)?;
            let args = pyobject_to_pickle_value(&args_obj, expand_refs)?;
            return Ok(PickleValue::Reduce {
                callable: Box::new(callable),
                args: Box::new(args),
            });
        }
    }

    // Regular dict with string keys
    let mut pairs = Vec::with_capacity(len);
    for (k, v) in dict {
        let key: String = k.extract()?;
        pairs.push((
            PickleValue::String(key),
            pyobject_to_pickle_value(&v, expand_refs)?,
        ));
    }
    Ok(PickleValue::Dict(pairs))
}

// ---------------------------------------------------------------------------
// Reverse: persistent ref expansion
// ---------------------------------------------------------------------------

/// Expand a compact ZODB persistent ref from PyObject.
fn expand_compact_ref(ref_val: &Bound<'_, pyo3::PyAny>) -> PyResult<PickleValue> {
    // Simple string oid: "0000000000000003"
    if let Ok(hex_str) = ref_val.extract::<String>() {
        let oid_bytes = hex::decode(&hex_str)
            .map_err(|e| CodecError::Json(format!("hex decode: {e}")))?;
        return Ok(PickleValue::PersistentRef(Box::new(PickleValue::Tuple(
            vec![PickleValue::Bytes(oid_bytes), PickleValue::None],
        ))));
    }

    // Array [oid_hex, class_path]
    if let Ok(list) = ref_val.downcast::<PyList>() {
        if list.len() == 2 {
            let oid_hex: String = list.get_item(0)?.extract()?;
            let class_path: String = list.get_item(1)?.extract()?;
            let oid_bytes = hex::decode(&oid_hex)
                .map_err(|e| CodecError::Json(format!("hex decode: {e}")))?;

            // Split "module.ClassName" back into module + name
            let (module, name) = if let Some(dot_pos) = class_path.rfind('.') {
                (
                    class_path[..dot_pos].to_string(),
                    class_path[dot_pos + 1..].to_string(),
                )
            } else {
                (String::new(), class_path)
            };

            return Ok(PickleValue::PersistentRef(Box::new(PickleValue::Tuple(
                vec![
                    PickleValue::Bytes(oid_bytes),
                    PickleValue::Global { module, name },
                ],
            ))));
        }
    }

    // Fallback: generic conversion
    let inner = pyobject_to_pickle_value(ref_val, false)?;
    Ok(PickleValue::PersistentRef(Box::new(inner)))
}

// ---------------------------------------------------------------------------
// Reverse: known type markers → PickleValue
// ---------------------------------------------------------------------------

fn try_typed_pydict_to_pickle_value(
    dict: &Bound<'_, PyDict>,
    expand_refs: bool,
) -> PyResult<Option<PickleValue>> {
    let py = dict.py();

    // @dt — datetime
    if let Some(v) = dict.get_item(intern!(py, "@dt"))? {
        if let Ok(iso) = v.extract::<String>() {
            let tz_obj = dict.get_item(intern!(py, "@tz"))?;
            return decode_datetime_from_pyobject(&iso, tz_obj.as_ref(), expand_refs).map(Some);
        }
    }

    // @date — date
    if let Some(v) = dict.get_item(intern!(py, "@date"))? {
        if let Ok(s) = v.extract::<String>() {
            return decode_date_from_str(&s).map(Some);
        }
    }

    // @time — time
    if let Some(v) = dict.get_item(intern!(py, "@time"))? {
        if let Ok(s) = v.extract::<String>() {
            let tz_obj = dict.get_item(intern!(py, "@tz"))?;
            return decode_time_from_pyobject(&s, tz_obj.as_ref(), expand_refs).map(Some);
        }
    }

    // @td — timedelta
    if let Some(v) = dict.get_item(intern!(py, "@td"))? {
        if let Ok(list) = v.downcast::<PyList>() {
            if list.len() == 3 {
                let days: i64 = list.get_item(0)?.extract()?;
                let secs: i64 = list.get_item(1)?.extract()?;
                let us: i64 = list.get_item(2)?.extract()?;
                return Ok(Some(PickleValue::Reduce {
                    callable: Box::new(PickleValue::Global {
                        module: "datetime".into(),
                        name: "timedelta".into(),
                    }),
                    args: Box::new(PickleValue::Tuple(vec![
                        PickleValue::Int(days),
                        PickleValue::Int(secs),
                        PickleValue::Int(us),
                    ])),
                }));
            }
        }
    }

    // @dec — Decimal
    if let Some(v) = dict.get_item(intern!(py, "@dec"))? {
        if let Ok(s) = v.extract::<String>() {
            return Ok(Some(PickleValue::Reduce {
                callable: Box::new(PickleValue::Global {
                    module: "decimal".into(),
                    name: "Decimal".into(),
                }),
                args: Box::new(PickleValue::Tuple(vec![PickleValue::String(s)])),
            }));
        }
    }

    // @uuid — UUID
    if let Some(v) = dict.get_item(intern!(py, "@uuid"))? {
        if let Ok(s) = v.extract::<String>() {
            return decode_uuid_from_str(&s).map(Some);
        }
    }

    Ok(None)
}

fn decode_datetime_from_pyobject(
    iso: &str,
    tz_obj: Option<&Bound<'_, pyo3::PyAny>>,
    _expand_refs: bool,
) -> PyResult<PickleValue> {
    let (datetime_part, offset_part) = known_types::parse_iso_datetime(iso)?;
    let (year, month, day, hour, min, sec, us) = datetime_part;
    let dt_bytes = PickleValue::Bytes(known_types::encode_datetime_bytes(
        year, month, day, hour, min, sec, us,
    ));

    let tz_pickle = if let Some(tz_val) = tz_obj {
        Some(decode_tz_from_pyobject(tz_val)?)
    } else if let Some(offset_secs) = offset_part {
        Some(known_types::make_stdlib_timezone(offset_secs))
    } else {
        None
    };

    let args = if let Some(tz) = tz_pickle {
        PickleValue::Tuple(vec![dt_bytes, tz])
    } else {
        PickleValue::Tuple(vec![dt_bytes])
    };

    Ok(PickleValue::Reduce {
        callable: Box::new(PickleValue::Global {
            module: "datetime".into(),
            name: "datetime".into(),
        }),
        args: Box::new(args),
    })
}

fn decode_date_from_str(s: &str) -> PyResult<PickleValue> {
    if s.len() < 10 {
        return Err(CodecError::InvalidData(format!("invalid date: {s}")).into());
    }
    let year: u16 = s[0..4]
        .parse()
        .map_err(|_| CodecError::InvalidData(format!("invalid year: {s}")))?;
    let month: u8 = s[5..7]
        .parse()
        .map_err(|_| CodecError::InvalidData(format!("invalid month: {s}")))?;
    let day: u8 = s[8..10]
        .parse()
        .map_err(|_| CodecError::InvalidData(format!("invalid day: {s}")))?;
    let bytes = vec![(year >> 8) as u8, (year & 0xff) as u8, month, day];
    Ok(PickleValue::Reduce {
        callable: Box::new(PickleValue::Global {
            module: "datetime".into(),
            name: "date".into(),
        }),
        args: Box::new(PickleValue::Tuple(vec![PickleValue::Bytes(bytes)])),
    })
}

fn decode_time_from_pyobject(
    s: &str,
    tz_obj: Option<&Bound<'_, pyo3::PyAny>>,
    _expand_refs: bool,
) -> PyResult<PickleValue> {
    let (time_part, offset_part) = known_types::parse_iso_time(s)?;
    let (hour, min, sec, us) = time_part;
    let bytes = vec![
        hour,
        min,
        sec,
        ((us >> 16) & 0xff) as u8,
        ((us >> 8) & 0xff) as u8,
        (us & 0xff) as u8,
    ];

    let tz_pickle = if let Some(tz_val) = tz_obj {
        Some(decode_tz_from_pyobject(tz_val)?)
    } else if let Some(offset_secs) = offset_part {
        Some(known_types::make_stdlib_timezone(offset_secs))
    } else {
        None
    };

    let args = if let Some(tz) = tz_pickle {
        PickleValue::Tuple(vec![PickleValue::Bytes(bytes), tz])
    } else {
        PickleValue::Tuple(vec![PickleValue::Bytes(bytes)])
    };

    Ok(PickleValue::Reduce {
        callable: Box::new(PickleValue::Global {
            module: "datetime".into(),
            name: "time".into(),
        }),
        args: Box::new(args),
    })
}

fn decode_uuid_from_str(s: &str) -> PyResult<PickleValue> {
    let hex: String = s.chars().filter(|c| *c != '-').collect();
    if hex.len() != 32 {
        return Err(CodecError::InvalidData(format!("invalid UUID: {s}")).into());
    }
    let int_val = u128::from_str_radix(&hex, 16)
        .map_err(|_| CodecError::InvalidData(format!("invalid UUID hex: {s}")))?;
    let int_pickle = if int_val <= i64::MAX as u128 {
        PickleValue::Int(int_val as i64)
    } else {
        PickleValue::BigInt(num_bigint::BigInt::from(int_val))
    };
    Ok(PickleValue::Instance {
        module: "uuid".into(),
        name: "UUID".into(),
        state: Box::new(PickleValue::Dict(vec![(
            PickleValue::String("int".into()),
            int_pickle,
        )])),
    })
}

/// Decode @tz PyObject back to a timezone PickleValue.
fn decode_tz_from_pyobject(tz_val: &Bound<'_, pyo3::PyAny>) -> PyResult<PickleValue> {
    if let Ok(tz_dict) = tz_val.downcast::<PyDict>() {
        let py = tz_dict.py();
        // pytz: {"pytz": [...args], "name": "US/Eastern"}
        if let Some(pytz_args) = tz_dict.get_item(intern!(py, "pytz"))? {
            if let Ok(args_list) = pytz_args.downcast::<PyList>() {
                let pickle_args: PyResult<Vec<PickleValue>> = args_list
                    .iter()
                    .map(|a| {
                        if let Ok(s) = a.extract::<String>() {
                            Ok(PickleValue::String(s))
                        } else if let Ok(i) = a.extract::<i64>() {
                            Ok(PickleValue::Int(i))
                        } else {
                            Err(CodecError::InvalidData("unsupported pytz arg".into()).into())
                        }
                    })
                    .collect();
                return Ok(PickleValue::Reduce {
                    callable: Box::new(PickleValue::Global {
                        module: "pytz".into(),
                        name: "_p".into(),
                    }),
                    args: Box::new(PickleValue::Tuple(pickle_args?)),
                });
            }
        }

        // zoneinfo: {"zoneinfo": "US/Eastern"}
        if let Some(key_val) = tz_dict.get_item(intern!(py, "zoneinfo"))? {
            if let Ok(key) = key_val.extract::<String>() {
                let inner_reduce = PickleValue::Reduce {
                    callable: Box::new(PickleValue::Global {
                        module: "builtins".into(),
                        name: "getattr".into(),
                    }),
                    args: Box::new(PickleValue::Tuple(vec![
                        PickleValue::Global {
                            module: "zoneinfo".into(),
                            name: "ZoneInfo".into(),
                        },
                        PickleValue::String("_unpickle".into()),
                    ])),
                };
                return Ok(PickleValue::Reduce {
                    callable: Box::new(inner_reduce),
                    args: Box::new(PickleValue::Tuple(vec![
                        PickleValue::String(key),
                        PickleValue::Int(1),
                    ])),
                });
            }
        }
    }

    Err(CodecError::InvalidData("unrecognized @tz format".to_string()).into())
}

// ---------------------------------------------------------------------------
// Reverse: BTree state from PyObject
// ---------------------------------------------------------------------------

/// Convert a BTree state PyObject back to nested tuple PickleValue.
pub fn btree_state_from_pyobject(
    info: &btrees::BTreeClassInfo,
    state_obj: &Bound<'_, pyo3::PyAny>,
    expand_refs: bool,
) -> PyResult<PickleValue> {
    // null → None
    if state_obj.is_none() {
        return Ok(PickleValue::None);
    }

    let dict = match state_obj.downcast::<PyDict>() {
        Ok(d) => d,
        // Not a dict — use generic decoder
        Err(_) => return pyobject_to_pickle_value(state_obj, expand_refs),
    };

    let py = dict.py();

    // @kv — map data
    if let Some(kv_val) = dict.get_item(intern!(py, "@kv"))? {
        let flat_data = decode_kv_from_pyobject(&kv_val, expand_refs)?;
        let next_ref = if let Some(next_val) = dict.get_item(intern!(py, "@next"))? {
            Some(pyobject_to_pickle_value(&next_val, expand_refs)?)
        } else {
            None
        };
        return Ok(btrees::wrap_flat_data(info, flat_data, next_ref)?);
    }

    // @ks — set data
    if let Some(ks_val) = dict.get_item(intern!(py, "@ks"))? {
        let flat_data = decode_keys_from_pyobject(&ks_val, expand_refs)?;
        let next_ref = if let Some(next_val) = dict.get_item(intern!(py, "@next"))? {
            Some(pyobject_to_pickle_value(&next_val, expand_refs)?)
        } else {
            None
        };
        return Ok(btrees::wrap_flat_data(info, flat_data, next_ref)?);
    }

    // @children + @first — large BTree
    if let Some(children_val) = dict.get_item(intern!(py, "@children"))? {
        let first_val = dict
            .get_item(intern!(py, "@first"))?
            .ok_or_else(|| CodecError::InvalidData("@children without @first".into()))?;
        if let Ok(children_list) = children_val.downcast::<PyList>() {
            let children: PyResult<Vec<PickleValue>> = children_list
                .iter()
                .map(|item| pyobject_to_pickle_value(&item, expand_refs))
                .collect();
            let children_tuple = PickleValue::Tuple(children?);
            let firstbucket = pyobject_to_pickle_value(&first_val, expand_refs)?;
            return Ok(PickleValue::Tuple(vec![children_tuple, firstbucket]));
        }
    }

    // Fallback
    pyobject_to_pickle_value(state_obj, expand_refs)
}

fn decode_kv_from_pyobject(
    val: &Bound<'_, pyo3::PyAny>,
    expand_refs: bool,
) -> PyResult<Vec<PickleValue>> {
    let list = val.downcast::<PyList>()?;
    let mut flat = Vec::with_capacity(list.len() * 2);
    for pair_obj in list.iter() {
        let pair = pair_obj.downcast::<PyList>()?;
        if pair.len() != 2 {
            return Err(CodecError::InvalidData("@kv pair must have 2 elements".into()).into());
        }
        flat.push(pyobject_to_pickle_value(&pair.get_item(0)?, expand_refs)?);
        flat.push(pyobject_to_pickle_value(&pair.get_item(1)?, expand_refs)?);
    }
    Ok(flat)
}

fn decode_keys_from_pyobject(
    val: &Bound<'_, pyo3::PyAny>,
    expand_refs: bool,
) -> PyResult<Vec<PickleValue>> {
    let list = val.downcast::<PyList>()?;
    list.iter()
        .map(|item| pyobject_to_pickle_value(&item, expand_refs))
        .collect()
}

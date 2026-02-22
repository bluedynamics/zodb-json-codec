//! Direct PickleValue ↔ Py<PyAny> conversion, bypassing serde_json::Value.
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
use crate::encode::{encode_value_into, write_bytes_val, write_global, write_int, write_string};
use crate::error::CodecError;
use crate::known_types;
use crate::opcodes::*;
use crate::types::PickleValue;

const MAX_DEPTH: usize = 1000;

// ---------------------------------------------------------------------------
// Forward direction: PickleValue → Py<PyAny>
// ---------------------------------------------------------------------------

/// Convert a PickleValue AST directly to a Python object with marker dicts.
///
/// When `compact_refs` is true, ZODB persistent references are compacted inline
/// (hex OID strings instead of nested tuple/bytes).
pub fn pickle_value_to_pyobject(
    py: Python<'_>,
    val: &PickleValue,
    compact_refs: bool,
) -> PyResult<Py<PyAny>> {
    pickle_value_to_pyobject_impl(py, val, compact_refs, false, 0)
}

/// Like `pickle_value_to_pyobject` but sanitizes strings containing null bytes
/// for PostgreSQL JSONB compatibility (replaces with `{"@ns": base64}` markers).
pub fn pickle_value_to_pyobject_pg(
    py: Python<'_>,
    val: &PickleValue,
    compact_refs: bool,
) -> PyResult<Py<PyAny>> {
    pickle_value_to_pyobject_impl(py, val, compact_refs, true, 0)
}

/// Collect all persistent reference OIDs from a PickleValue tree.
///
/// OIDs are returned as i64 (big-endian interpretation of 8-byte ZODB OID).
/// Cross-database refs (non-8-byte OIDs) are skipped.
pub fn collect_refs_from_pickle_value(val: &PickleValue, refs: &mut Vec<i64>) {
    match val {
        PickleValue::PersistentRef(inner) => {
            // Extract OID from Tuple([Bytes(oid), ...])
            if let PickleValue::Tuple(items) = inner.as_ref() {
                if let Some(PickleValue::Bytes(oid)) = items.first() {
                    if oid.len() == 8 {
                        if let Ok(arr) = <[u8; 8]>::try_from(oid.as_slice()) {
                            refs.push(i64::from_be_bytes(arr));
                        }
                    }
                }
            }
        }
        PickleValue::List(items)
        | PickleValue::Tuple(items)
        | PickleValue::Set(items)
        | PickleValue::FrozenSet(items) => {
            for item in items {
                collect_refs_from_pickle_value(item, refs);
            }
        }
        PickleValue::Dict(pairs) => {
            for (k, v) in pairs {
                collect_refs_from_pickle_value(k, refs);
                collect_refs_from_pickle_value(v, refs);
            }
        }
        PickleValue::Instance { state, .. } => {
            collect_refs_from_pickle_value(state, refs);
        }
        PickleValue::Reduce { args, .. } => {
            collect_refs_from_pickle_value(args, refs);
        }
        _ => {}
    }
}

/// Core implementation with optional null-byte sanitization for PG JSONB.
fn pickle_value_to_pyobject_impl(
    py: Python<'_>,
    val: &PickleValue,
    compact_refs: bool,
    sanitize_nulls: bool,
    depth: usize,
) -> PyResult<Py<PyAny>> {
    if depth > MAX_DEPTH {
        return Err(pyo3::exceptions::PyValueError::new_err("maximum nesting depth exceeded"));
    }
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
        PickleValue::String(s) => {
            if sanitize_nulls && s.contains('\0') {
                // PG JSONB cannot store \u0000 — base64-encode with @ns marker
                let dict = PyDict::new(py);
                dict.set_item(intern!(py, "@ns"), BASE64.encode(s.as_bytes()))?;
                Ok(dict.into_any().unbind())
            } else {
                Ok(s.into_pyobject(py)?.into_any().unbind())
            }
        }
        PickleValue::Bytes(b) => {
            let dict = PyDict::new(py);
            dict.set_item(intern!(py, "@b"), BASE64.encode(b))?;
            Ok(dict.into_any().unbind())
        }
        PickleValue::List(items) => {
            let py_items: PyResult<Vec<Py<PyAny>>> = items
                .iter()
                .map(|item| pickle_value_to_pyobject_impl(py, item, compact_refs, sanitize_nulls, depth + 1))
                .collect();
            let list = PyList::new(py, py_items?)?;
            Ok(list.into_any().unbind())
        }
        PickleValue::Tuple(items) => {
            let py_items: PyResult<Vec<Py<PyAny>>> = items
                .iter()
                .map(|item| pickle_value_to_pyobject_impl(py, item, compact_refs, sanitize_nulls, depth + 1))
                .collect();
            let list = PyList::new(py, py_items?)?;
            let dict = PyDict::new(py);
            dict.set_item(intern!(py, "@t"), list)?;
            Ok(dict.into_any().unbind())
        }
        PickleValue::Dict(pairs) => {
            // Pre-scan: check if all keys are strings to avoid double processing
            let all_string_keys = pairs.iter().all(|(k, _)| matches!(k, PickleValue::String(_)));
            if all_string_keys {
                let dict = PyDict::new(py);
                for (k, v) in pairs {
                    if let PickleValue::String(key) = k {
                        let py_key = if sanitize_nulls && key.contains('\0') {
                            let marker = PyDict::new(py);
                            marker.set_item(intern!(py, "@ns"), BASE64.encode(key.as_bytes()))?;
                            marker.into_any().unbind()
                        } else {
                            key.into_pyobject(py)?.into_any().unbind()
                        };
                        dict.set_item(py_key, pickle_value_to_pyobject_impl(py, v, compact_refs, sanitize_nulls, depth + 1)?)?;
                    }
                }
                Ok(dict.into_any().unbind())
            } else {
                // Non-string keys: use @d format
                let py_pairs: PyResult<Vec<Py<PyAny>>> = pairs
                    .iter()
                    .map(|(k, v)| {
                        let pk = pickle_value_to_pyobject_impl(py, k, compact_refs, sanitize_nulls, depth + 1)?;
                        let pv = pickle_value_to_pyobject_impl(py, v, compact_refs, sanitize_nulls, depth + 1)?;
                        let pair = PyList::new(py, [pk, pv])?;
                        Ok(pair.into_any().unbind())
                    })
                    .collect();
                let arr = PyList::new(py, py_pairs?)?;
                let d = PyDict::new(py);
                d.set_item(intern!(py, "@d"), arr)?;
                Ok(d.into_any().unbind())
            }
        }
        PickleValue::Set(items) => {
            let py_items: PyResult<Vec<Py<PyAny>>> = items
                .iter()
                .map(|item| pickle_value_to_pyobject_impl(py, item, compact_refs, sanitize_nulls, depth + 1))
                .collect();
            let list = PyList::new(py, py_items?)?;
            let dict = PyDict::new(py);
            dict.set_item(intern!(py, "@set"), list)?;
            Ok(dict.into_any().unbind())
        }
        PickleValue::FrozenSet(items) => {
            let py_items: PyResult<Vec<Py<PyAny>>> = items
                .iter()
                .map(|item| pickle_value_to_pyobject_impl(py, item, compact_refs, sanitize_nulls, depth + 1))
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
                btree_state_to_pyobject_impl(py, &info, state, compact_refs, sanitize_nulls, depth + 1)?
            } else {
                pickle_value_to_pyobject_impl(py, state, compact_refs, sanitize_nulls, depth + 1)?
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
                compact_ref_to_pyobject_impl(py, inner, compact_refs, sanitize_nulls, depth + 1)
            } else {
                let inner_obj = pickle_value_to_pyobject_impl(py, inner, compact_refs, sanitize_nulls, depth + 1)?;
                let dict = PyDict::new(py);
                dict.set_item(intern!(py, "@ref"), inner_obj)?;
                Ok(dict.into_any().unbind())
            }
        }
        PickleValue::Reduce { callable, args } => {
            // Try known type handlers first (datetime, Decimal, set, etc.)
            if let Some(obj) =
                try_reduce_to_pyobject_impl(py, callable, args, compact_refs, sanitize_nulls, depth)?
            {
                return Ok(obj);
            }
            // Fall back to generic @reduce
            let callable_obj = pickle_value_to_pyobject_impl(py, callable, compact_refs, sanitize_nulls, depth + 1)?;
            let args_obj = pickle_value_to_pyobject_impl(py, args, compact_refs, sanitize_nulls, depth + 1)?;
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

/// Compact a ZODB persistent ref directly to Py<PyAny>.
/// inner is typically Tuple([Bytes(oid), None_or_Global])
fn compact_ref_to_pyobject_impl(
    py: Python<'_>,
    inner: &PickleValue,
    compact_refs: bool,
    sanitize_nulls: bool,
    depth: usize,
) -> PyResult<Py<PyAny>> {
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
    let inner_obj = pickle_value_to_pyobject_impl(py, inner, compact_refs, sanitize_nulls, depth)?;
    let dict = PyDict::new(py);
    dict.set_item(intern!(py, "@ref"), inner_obj)?;
    Ok(dict.into_any().unbind())
}

// ---------------------------------------------------------------------------
// Forward: known type handlers (PickleValue → Py<PyAny>)
// ---------------------------------------------------------------------------

/// Try to convert a known REDUCE to a compact typed Py<PyAny>.
fn try_reduce_to_pyobject_impl(
    py: Python<'_>,
    callable: &PickleValue,
    args: &PickleValue,
    compact_refs: bool,
    sanitize_nulls: bool,
    depth: usize,
) -> PyResult<Option<Py<PyAny>>> {
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
        ("builtins", "set") => encode_set_pyobject_impl(py, args, compact_refs, sanitize_nulls, depth + 1),
        ("builtins", "frozenset") => encode_frozenset_pyobject_impl(py, args, compact_refs, sanitize_nulls, depth + 1),
        _ => Ok(None),
    }
}

fn encode_datetime_pyobject(
    py: Python<'_>,
    args: &PickleValue,
    _compact_refs: bool,
) -> PyResult<Option<Py<PyAny>>> {
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
                let py_args: PyResult<Vec<Py<PyAny>>> = tz_args
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

/// Convert a serde_json::Value (only simple types) to a Py<PyAny>.
/// Used for pytz timezone args which come back from extract_tz_info as serde_json::Value.
fn json_value_to_simple_pyobject(
    py: Python<'_>,
    val: &serde_json::Value,
) -> PyResult<Py<PyAny>> {
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
) -> PyResult<Option<Py<PyAny>>> {
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
) -> PyResult<Option<Py<PyAny>>> {
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
                let py_args: PyResult<Vec<Py<PyAny>>> = tz_args
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
) -> PyResult<Option<Py<PyAny>>> {
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
) -> PyResult<Option<Py<PyAny>>> {
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

fn encode_set_pyobject_impl(
    py: Python<'_>,
    args: &PickleValue,
    compact_refs: bool,
    sanitize_nulls: bool,
    depth: usize,
) -> PyResult<Option<Py<PyAny>>> {
    let tuple_items = match args {
        PickleValue::Tuple(items) if items.len() == 1 => items,
        _ => return Ok(None),
    };
    let list_items = match &tuple_items[0] {
        PickleValue::List(items) => items,
        _ => return Ok(None),
    };
    let py_items: PyResult<Vec<Py<PyAny>>> = list_items
        .iter()
        .map(|i| pickle_value_to_pyobject_impl(py, i, compact_refs, sanitize_nulls, depth))
        .collect();
    let list = PyList::new(py, py_items?)?;
    let dict = PyDict::new(py);
    dict.set_item(intern!(py, "@set"), list)?;
    Ok(Some(dict.into_any().unbind()))
}

fn encode_frozenset_pyobject_impl(
    py: Python<'_>,
    args: &PickleValue,
    compact_refs: bool,
    sanitize_nulls: bool,
    depth: usize,
) -> PyResult<Option<Py<PyAny>>> {
    let tuple_items = match args {
        PickleValue::Tuple(items) if items.len() == 1 => items,
        _ => return Ok(None),
    };
    let list_items = match &tuple_items[0] {
        PickleValue::List(items) => items,
        _ => return Ok(None),
    };
    let py_items: PyResult<Vec<Py<PyAny>>> = list_items
        .iter()
        .map(|i| pickle_value_to_pyobject_impl(py, i, compact_refs, sanitize_nulls, depth))
        .collect();
    let list = PyList::new(py, py_items?)?;
    let dict = PyDict::new(py);
    dict.set_item(intern!(py, "@fset"), list)?;
    Ok(Some(dict.into_any().unbind()))
}

/// Try to convert a known Instance to a compact typed Py<PyAny>.
fn try_instance_to_pyobject(
    py: Python<'_>,
    module: &str,
    name: &str,
    state: &PickleValue,
    _compact_refs: bool,
) -> PyResult<Option<Py<PyAny>>> {
    match (module, name) {
        ("uuid", "UUID") => encode_uuid_pyobject(py, state),
        _ => Ok(None),
    }
}

fn encode_uuid_pyobject(
    py: Python<'_>,
    state: &PickleValue,
) -> PyResult<Option<Py<PyAny>>> {
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
// Forward: BTree state → Py<PyAny>
// ---------------------------------------------------------------------------

/// Convert a BTree state PickleValue to flattened Py<PyAny>.
pub fn btree_state_to_pyobject(
    py: Python<'_>,
    info: &btrees::BTreeClassInfo,
    state: &PickleValue,
    compact_refs: bool,
) -> PyResult<Py<PyAny>> {
    btree_state_to_pyobject_impl(py, info, state, compact_refs, false, 0)
}

/// Like `btree_state_to_pyobject` but with null-byte sanitization for PG JSONB.
pub fn btree_state_to_pyobject_pg(
    py: Python<'_>,
    info: &btrees::BTreeClassInfo,
    state: &PickleValue,
    compact_refs: bool,
) -> PyResult<Py<PyAny>> {
    btree_state_to_pyobject_impl(py, info, state, compact_refs, true, 0)
}

/// Core BTree state conversion with optional null-byte sanitization.
fn btree_state_to_pyobject_impl(
    py: Python<'_>,
    info: &btrees::BTreeClassInfo,
    state: &PickleValue,
    compact_refs: bool,
    sanitize_nulls: bool,
    depth: usize,
) -> PyResult<Py<PyAny>> {
    // Empty BTree: state is None
    if *state == PickleValue::None {
        return Ok(py.None());
    }

    match info.kind {
        btrees::BTreeNodeKind::BTree | btrees::BTreeNodeKind::TreeSet => {
            btree_node_to_pyobject_impl(py, info, state, compact_refs, sanitize_nulls, depth)
        }
        btrees::BTreeNodeKind::Bucket | btrees::BTreeNodeKind::Set => {
            bucket_to_pyobject_impl(py, info, state, compact_refs, sanitize_nulls, depth)
        }
    }
}

fn btree_node_to_pyobject_impl(
    py: Python<'_>,
    info: &btrees::BTreeClassInfo,
    state: &PickleValue,
    compact_refs: bool,
    sanitize_nulls: bool,
    depth: usize,
) -> PyResult<Py<PyAny>> {
    let outer = match state {
        PickleValue::Tuple(items) => items,
        _ => return pickle_value_to_pyobject_impl(py, state, compact_refs, sanitize_nulls, depth),
    };

    // Small inline BTree — 1-tuple
    if outer.len() == 1 {
        if let Some(flat_data) = btrees::unwrap_inline_btree(&outer[0]) {
            return format_flat_data_pyobject_impl(py, info, flat_data, compact_refs, sanitize_nulls, depth);
        }
        return pickle_value_to_pyobject_impl(py, state, compact_refs, sanitize_nulls, depth);
    }

    // Large BTree with persistent refs — 2-tuple
    if outer.len() == 2 {
        if let PickleValue::Tuple(children) = &outer[0] {
            if btrees::children_has_refs(children) {
                let py_children: PyResult<Vec<Py<PyAny>>> = children
                    .iter()
                    .map(|item| pickle_value_to_pyobject_impl(py, item, compact_refs, sanitize_nulls, depth + 1))
                    .collect();
                let children_list = PyList::new(py, py_children?)?;
                let first_obj = pickle_value_to_pyobject_impl(py, &outer[1], compact_refs, sanitize_nulls, depth + 1)?;
                let dict = PyDict::new(py);
                dict.set_item(intern!(py, "@children"), children_list)?;
                dict.set_item(intern!(py, "@first"), first_obj)?;
                return Ok(dict.into_any().unbind());
            }
        }
        return pickle_value_to_pyobject_impl(py, state, compact_refs, sanitize_nulls, depth);
    }

    pickle_value_to_pyobject_impl(py, state, compact_refs, sanitize_nulls, depth)
}

fn bucket_to_pyobject_impl(
    py: Python<'_>,
    info: &btrees::BTreeClassInfo,
    state: &PickleValue,
    compact_refs: bool,
    sanitize_nulls: bool,
    depth: usize,
) -> PyResult<Py<PyAny>> {
    let outer = match state {
        PickleValue::Tuple(items) => items,
        _ => return pickle_value_to_pyobject_impl(py, state, compact_refs, sanitize_nulls, depth),
    };

    // Standalone bucket — 1-tuple
    if outer.len() == 1 {
        if let PickleValue::Tuple(flat_data) = &outer[0] {
            return format_flat_data_pyobject_impl(py, info, flat_data, compact_refs, sanitize_nulls, depth);
        }
        return pickle_value_to_pyobject_impl(py, state, compact_refs, sanitize_nulls, depth);
    }

    // Linked bucket — 2-tuple: (flat_data, next_ref)
    if outer.len() == 2 {
        if let PickleValue::Tuple(flat_data) = &outer[0] {
            let dict = PyDict::new(py);
            if info.is_map {
                let mut pairs = Vec::new();
                let mut i = 0;
                while i + 1 < flat_data.len() {
                    let k = pickle_value_to_pyobject_impl(py, &flat_data[i], compact_refs, sanitize_nulls, depth + 1)?;
                    let v = pickle_value_to_pyobject_impl(py, &flat_data[i + 1], compact_refs, sanitize_nulls, depth + 1)?;
                    let pair = PyList::new(py, [k, v])?;
                    pairs.push(pair.into_any().unbind());
                    i += 2;
                }
                let kv_list = PyList::new(py, pairs)?;
                dict.set_item(intern!(py, "@kv"), kv_list)?;
            } else {
                let py_keys: PyResult<Vec<Py<PyAny>>> = flat_data
                    .iter()
                    .map(|item| pickle_value_to_pyobject_impl(py, item, compact_refs, sanitize_nulls, depth + 1))
                    .collect();
                let ks_list = PyList::new(py, py_keys?)?;
                dict.set_item(intern!(py, "@ks"), ks_list)?;
            }
            let next_obj = pickle_value_to_pyobject_impl(py, &outer[1], compact_refs, sanitize_nulls, depth + 1)?;
            dict.set_item(intern!(py, "@next"), next_obj)?;
            return Ok(dict.into_any().unbind());
        }
        return pickle_value_to_pyobject_impl(py, state, compact_refs, sanitize_nulls, depth);
    }

    pickle_value_to_pyobject_impl(py, state, compact_refs, sanitize_nulls, depth)
}

fn format_flat_data_pyobject_impl(
    py: Python<'_>,
    info: &btrees::BTreeClassInfo,
    items: &[PickleValue],
    compact_refs: bool,
    sanitize_nulls: bool,
    depth: usize,
) -> PyResult<Py<PyAny>> {
    let dict = PyDict::new(py);
    if info.is_map {
        let mut pairs = Vec::with_capacity(items.len() / 2);
        let mut i = 0;
        while i + 1 < items.len() {
            let k = pickle_value_to_pyobject_impl(py, &items[i], compact_refs, sanitize_nulls, depth + 1)?;
            let v = pickle_value_to_pyobject_impl(py, &items[i + 1], compact_refs, sanitize_nulls, depth + 1)?;
            let pair = PyList::new(py, [k, v])?;
            pairs.push(pair.into_any().unbind());
            i += 2;
        }
        let kv_list = PyList::new(py, pairs)?;
        dict.set_item(intern!(py, "@kv"), kv_list)?;
    } else {
        let py_keys: PyResult<Vec<Py<PyAny>>> = items
            .iter()
            .map(|item| pickle_value_to_pyobject_impl(py, item, compact_refs, sanitize_nulls, depth + 1))
            .collect();
        let ks_list = PyList::new(py, py_keys?)?;
        dict.set_item(intern!(py, "@ks"), ks_list)?;
    }
    Ok(dict.into_any().unbind())
}

// ---------------------------------------------------------------------------
// Reverse direction: Py<PyAny> → PickleValue
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
        let dict = obj.cast::<PyDict>()?;
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
        let list = obj.cast::<PyList>()?;
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
///
/// Optimized dispatch:
/// - len > 4: skip marker checks (no marker dict has >4 keys)
/// - len == 1: direct key match (avoids all hash-based get_item lookups)
/// - len 2-4: single-pass '@' scan, then targeted marker checks
fn pydict_to_pickle_value(
    dict: &Bound<'_, PyDict>,
    expand_refs: bool,
) -> PyResult<PickleValue> {
    let py = dict.py();
    let len = dict.len();

    // Fast path: no JSON marker dict has more than 4 keys.
    if len > 4 {
        return plain_dict_to_pickle_value(dict, expand_refs);
    }

    if len == 0 {
        return Ok(PickleValue::Dict(vec![]));
    }

    // Fast path for single-key dicts: extract the key and match directly.
    // Avoids up to 15 hash-based get_item lookups for marker detection.
    // Common markers like @ref, @dt, @b are all single-key dicts.
    if len == 1 {
        let (k, v) = dict.iter().next().unwrap();
        if let Ok(s) = k.cast::<PyString>() {
            if let Ok(key) = s.to_str() {
                if key.starts_with('@') {
                    if let Some(pv) = try_decode_single_key_marker(py, key, &v, expand_refs)? {
                        return Ok(pv);
                    }
                }
                // Non-marker key, or marker with unrecognized value type
                return Ok(PickleValue::Dict(vec![(
                    PickleValue::String(key.to_owned()),
                    pyobject_to_pickle_value(&v, expand_refs)?,
                )]));
            }
        }
        let k_str: String = k.extract()?;
        return Ok(PickleValue::Dict(vec![(
            PickleValue::String(k_str),
            pyobject_to_pickle_value(&v, expand_refs)?,
        )]));
    }

    // For 2-4 key dicts: single-pass scan for '@' prefix.
    // Builds pairs inline — if no '@' found, the dict is already constructed.
    // If '@' found, breaks early and falls through to targeted marker checks.
    let mut pairs = Vec::with_capacity(len);
    let mut found_marker = false;
    for (k, v) in dict {
        if let Ok(s) = k.cast::<PyString>() {
            if let Ok(key_str) = s.to_str() {
                if key_str.starts_with('@') {
                    found_marker = true;
                    break;
                }
                pairs.push((
                    PickleValue::String(key_str.to_owned()),
                    pyobject_to_pickle_value(&v, expand_refs)?,
                ));
                continue;
            }
        }
        let key: String = k.extract()?;
        pairs.push((
            PickleValue::String(key),
            pyobject_to_pickle_value(&v, expand_refs)?,
        ));
    }
    if !found_marker {
        return Ok(PickleValue::Dict(pairs));
    }
    drop(pairs);

    // Multi-key marker dict — targeted checks, ordered by frequency.

    // @cls (+@s) is the most common multi-key marker pattern
    if let Some(cls_val) = dict.get_item(intern!(py, "@cls"))? {
        if let Ok(cls_list) = cls_val.cast::<PyList>() {
            if cls_list.len() == 2 {
                let module: String = cls_list.get_item(0)?.extract()?;
                let name: String = cls_list.get_item(1)?.extract()?;
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
                return Ok(PickleValue::Global { module, name });
            }
        }
    }

    // Known type markers: @dt (+@tz), @date, @time (+@tz), @td, @dec, @uuid
    if let Some(pv) = try_typed_pydict_to_pickle_value(dict, expand_refs)? {
        return Ok(pv);
    }

    // Remaining markers (rare in multi-key context)

    // @t — Tuple
    if let Some(v) = dict.get_item(intern!(py, "@t"))? {
        if let Ok(list) = v.cast::<PyList>() {
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
        if let Ok(list) = v.cast::<PyList>() {
            let mut pairs = Vec::with_capacity(list.len());
            for pair_obj in list.iter() {
                if let Ok(pair_list) = pair_obj.cast::<PyList>() {
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
        if let Ok(list) = v.cast::<PyList>() {
            let items: PyResult<Vec<PickleValue>> = list
                .iter()
                .map(|item| pyobject_to_pickle_value(&item, expand_refs))
                .collect();
            return Ok(PickleValue::Set(items?));
        }
    }

    // @fset — FrozenSet
    if let Some(v) = dict.get_item(intern!(py, "@fset"))? {
        if let Ok(list) = v.cast::<PyList>() {
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

    // @reduce — Generic reduce
    if let Some(v) = dict.get_item(intern!(py, "@reduce"))? {
        if let Ok(reduce_dict) = v.cast::<PyDict>() {
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

    // Fallback: regular dict with string keys
    plain_dict_to_pickle_value(dict, expand_refs)
}

/// Build a plain dict PickleValue from a PyDict (no marker checking).
#[inline]
fn plain_dict_to_pickle_value(
    dict: &Bound<'_, PyDict>,
    expand_refs: bool,
) -> PyResult<PickleValue> {
    let mut pairs = Vec::with_capacity(dict.len());
    for (k, v) in dict {
        let key: String = k.extract()?;
        pairs.push((
            PickleValue::String(key),
            pyobject_to_pickle_value(&v, expand_refs)?,
        ));
    }
    Ok(PickleValue::Dict(pairs))
}

/// Fast path for single-key marker dicts.
/// Returns Some(PickleValue) if the marker was recognized and value type matched.
fn try_decode_single_key_marker(
    py: Python<'_>,
    key: &str,
    v: &Bound<'_, pyo3::PyAny>,
    expand_refs: bool,
) -> PyResult<Option<PickleValue>> {
    match key {
        "@t" => {
            if let Ok(list) = v.cast::<PyList>() {
                let items: PyResult<Vec<PickleValue>> = list
                    .iter()
                    .map(|item| pyobject_to_pickle_value(&item, expand_refs))
                    .collect();
                return Ok(Some(PickleValue::Tuple(items?)));
            }
        }
        "@b" => {
            if let Ok(s) = v.extract::<String>() {
                let bytes = BASE64
                    .decode(&s)
                    .map_err(|e| CodecError::Json(format!("base64 decode: {e}")))?;
                return Ok(Some(PickleValue::Bytes(bytes)));
            }
        }
        "@bi" => {
            if let Ok(s) = v.extract::<String>() {
                let bi: num_bigint::BigInt = s
                    .parse()
                    .map_err(|e| CodecError::Json(format!("bigint parse: {e}")))?;
                return Ok(Some(PickleValue::BigInt(bi)));
            }
        }
        "@d" => {
            if let Ok(list) = v.cast::<PyList>() {
                let mut pairs = Vec::with_capacity(list.len());
                for pair_obj in list.iter() {
                    if let Ok(pair_list) = pair_obj.cast::<PyList>() {
                        if pair_list.len() == 2 {
                            let k =
                                pyobject_to_pickle_value(&pair_list.get_item(0)?, expand_refs)?;
                            let v =
                                pyobject_to_pickle_value(&pair_list.get_item(1)?, expand_refs)?;
                            pairs.push((k, v));
                        }
                    }
                }
                return Ok(Some(PickleValue::Dict(pairs)));
            }
        }
        "@set" => {
            if let Ok(list) = v.cast::<PyList>() {
                let items: PyResult<Vec<PickleValue>> = list
                    .iter()
                    .map(|item| pyobject_to_pickle_value(&item, expand_refs))
                    .collect();
                return Ok(Some(PickleValue::Set(items?)));
            }
        }
        "@fset" => {
            if let Ok(list) = v.cast::<PyList>() {
                let items: PyResult<Vec<PickleValue>> = list
                    .iter()
                    .map(|item| pyobject_to_pickle_value(&item, expand_refs))
                    .collect();
                return Ok(Some(PickleValue::FrozenSet(items?)));
            }
        }
        "@ref" => {
            if expand_refs {
                return Ok(Some(expand_compact_ref(v)?));
            } else {
                let inner = pyobject_to_pickle_value(v, expand_refs)?;
                return Ok(Some(PickleValue::PersistentRef(Box::new(inner))));
            }
        }
        "@pkl" => {
            if let Ok(s) = v.extract::<String>() {
                let bytes = BASE64
                    .decode(&s)
                    .map_err(|e| CodecError::Json(format!("base64 decode: {e}")))?;
                return Ok(Some(PickleValue::RawPickle(bytes)));
            }
        }
        "@dt" => {
            if let Ok(iso) = v.extract::<String>() {
                return Ok(Some(decode_datetime_from_pyobject(&iso, None, expand_refs)?));
            }
        }
        "@date" => {
            if let Ok(s) = v.extract::<String>() {
                return Ok(Some(decode_date_from_str(&s)?));
            }
        }
        "@time" => {
            if let Ok(s) = v.extract::<String>() {
                return Ok(Some(decode_time_from_pyobject(&s, None, expand_refs)?));
            }
        }
        "@td" => {
            if let Ok(list) = v.cast::<PyList>() {
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
        "@dec" => {
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
        "@uuid" => {
            if let Ok(s) = v.extract::<String>() {
                return Ok(Some(decode_uuid_from_str(&s)?));
            }
        }
        "@cls" => {
            if let Ok(cls_list) = v.cast::<PyList>() {
                if cls_list.len() == 2 {
                    let module: String = cls_list.get_item(0)?.extract()?;
                    let name: String = cls_list.get_item(1)?.extract()?;
                    return Ok(Some(PickleValue::Global { module, name }));
                }
            }
        }
        "@reduce" => {
            if let Ok(reduce_dict) = v.cast::<PyDict>() {
                let callable_obj = reduce_dict
                    .get_item(intern!(py, "callable"))?
                    .unwrap_or_else(|| py.None().into_bound(py));
                let args_obj = reduce_dict
                    .get_item(intern!(py, "args"))?
                    .unwrap_or_else(|| py.None().into_bound(py));
                let callable = pyobject_to_pickle_value(&callable_obj, expand_refs)?;
                let args = pyobject_to_pickle_value(&args_obj, expand_refs)?;
                return Ok(Some(PickleValue::Reduce {
                    callable: Box::new(callable),
                    args: Box::new(args),
                }));
            }
        }
        _ => {}
    }
    Ok(None)
}

// ---------------------------------------------------------------------------
// Reverse: persistent ref expansion
// ---------------------------------------------------------------------------

/// Expand a compact ZODB persistent ref from Py<PyAny>.
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
    if let Ok(list) = ref_val.cast::<PyList>() {
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
        if let Ok(list) = v.cast::<PyList>() {
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

/// Decode @tz Py<PyAny> back to a timezone PickleValue.
fn decode_tz_from_pyobject(tz_val: &Bound<'_, pyo3::PyAny>) -> PyResult<PickleValue> {
    if let Ok(tz_dict) = tz_val.cast::<PyDict>() {
        let py = tz_dict.py();
        // pytz: {"pytz": [...args], "name": "US/Eastern"}
        if let Some(pytz_args) = tz_dict.get_item(intern!(py, "pytz"))? {
            if let Ok(args_list) = pytz_args.cast::<PyList>() {
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
// Reverse: BTree state from Py<PyAny>
// ---------------------------------------------------------------------------

/// Convert a BTree state Py<PyAny> back to nested tuple PickleValue.
pub fn btree_state_from_pyobject(
    info: &btrees::BTreeClassInfo,
    state_obj: &Bound<'_, pyo3::PyAny>,
    expand_refs: bool,
) -> PyResult<PickleValue> {
    // null → None
    if state_obj.is_none() {
        return Ok(PickleValue::None);
    }

    let dict = match state_obj.cast::<PyDict>() {
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
        if let Ok(children_list) = children_val.cast::<PyList>() {
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
    let list = val.cast::<PyList>()?;
    let mut flat = Vec::with_capacity(list.len() * 2);
    for pair_obj in list.iter() {
        let pair = pair_obj.cast::<PyList>()?;
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
    let list = val.cast::<PyList>()?;
    list.iter()
        .map(|item| pyobject_to_pickle_value(&item, expand_refs))
        .collect()
}

// ---------------------------------------------------------------------------
// Direct encoder: Py<PyAny> → pickle bytes (bypasses PickleValue allocation)
// ---------------------------------------------------------------------------

/// Encode a Py<PyAny> directly to pickle bytes with PROTO 2 framing.
/// Used by `dict_to_pickle`.
pub fn encode_pyobject_as_pickle(
    obj: &Bound<'_, pyo3::PyAny>,
    expand_refs: bool,
) -> PyResult<Vec<u8>> {
    let mut buf = Vec::with_capacity(256);
    buf.push(PROTO);
    buf.push(2);
    encode_pyobject_to_pickle(obj, &mut buf, expand_refs)?;
    buf.push(STOP);
    Ok(buf)
}

/// Encode a ZODB record (class + state) directly to concatenated pickle bytes.
/// Writes both the class pickle and state pickle directly, skipping
/// all PickleValue intermediate allocations.
pub fn encode_zodb_record_direct(
    module: &str,
    name: &str,
    state_obj: &Bound<'_, pyo3::PyAny>,
) -> PyResult<Vec<u8>> {
    let btree_info = btrees::classify_btree(module, name);

    let mut result = Vec::with_capacity(256);

    // Class pickle: PROTO 2 + ((module, name), None) as tuple + STOP
    // This is the format produced by ZODB's PersistentPickler and expected
    // by ZODB's standard unpickling (ObjectReader and zodb_unpickle).
    result.extend_from_slice(&[PROTO, 2]);
    write_string(&mut result, module);
    write_string(&mut result, name);
    result.push(TUPLE2);  // inner tuple: (module, name)
    result.push(NONE);
    result.push(TUPLE2);  // outer tuple: ((module, name), None)
    result.push(STOP);

    // State pickle: PROTO 2 + state opcodes + STOP
    result.extend_from_slice(&[PROTO, 2]);
    if let Some(info) = btree_info {
        encode_btree_state_to_pickle(&info, state_obj, &mut result, true)?;
    } else {
        encode_pyobject_to_pickle(state_obj, &mut result, true)?;
    }
    result.push(STOP);

    Ok(result)
}

/// Write a Py<PyAny> as pickle opcodes into the buffer (no PROTO/STOP framing).
/// Handles common types directly; falls back to PickleValue for complex markers.
pub fn encode_pyobject_to_pickle(
    obj: &Bound<'_, pyo3::PyAny>,
    buf: &mut Vec<u8>,
    expand_refs: bool,
) -> PyResult<()> {
    // String: borrow &str from Python, write directly (zero-copy)
    if obj.is_instance_of::<PyString>() {
        let s = obj.cast::<PyString>()?.to_str()?;
        write_string(buf, s);
        return Ok(());
    }

    // Dict: handle markers or write plain dict
    if obj.is_instance_of::<PyDict>() {
        let dict = obj.cast::<PyDict>()?;
        return encode_pydict_to_pickle(dict, buf, expand_refs);
    }

    // None
    if obj.is_none() {
        buf.push(NONE);
        return Ok(());
    }

    // Bool before Int (bool is subclass of int in Python)
    if obj.is_instance_of::<PyBool>() {
        buf.push(if obj.extract::<bool>()? {
            NEWTRUE
        } else {
            NEWFALSE
        });
        return Ok(());
    }

    // Int
    if obj.is_instance_of::<PyInt>() {
        let i: i64 = obj.extract()?;
        write_int(buf, i);
        return Ok(());
    }

    // Float
    if obj.is_instance_of::<PyFloat>() {
        let f: f64 = obj.extract()?;
        buf.push(BINFLOAT);
        buf.extend_from_slice(&f.to_be_bytes());
        return Ok(());
    }

    // List
    if obj.is_instance_of::<PyList>() {
        let list = obj.cast::<PyList>()?;
        buf.push(EMPTY_LIST);
        if !list.is_empty() {
            buf.push(MARK);
            for item in list.iter() {
                encode_pyobject_to_pickle(&item, buf, expand_refs)?;
            }
            buf.push(APPENDS);
        }
        return Ok(());
    }

    // Fallback: convert to PickleValue first, then encode
    let pv = pyobject_to_pickle_value(obj, expand_refs)?;
    encode_value_into(&pv, buf)?;
    Ok(())
}

/// Write a PyDict as pickle opcodes with marker detection.
fn encode_pydict_to_pickle(
    dict: &Bound<'_, PyDict>,
    buf: &mut Vec<u8>,
    expand_refs: bool,
) -> PyResult<()> {
    let len = dict.len();

    // Fast path: no marker dict has more than 4 keys
    if len > 4 {
        return encode_plain_dict_to_pickle(dict, buf, expand_refs);
    }

    if len == 0 {
        buf.push(EMPTY_DICT);
        return Ok(());
    }

    // Single-key fast path for markers
    if len == 1 {
        let (k, v) = dict.iter().next().unwrap();
        if let Ok(s) = k.cast::<PyString>() {
            if let Ok(key) = s.to_str() {
                if key.starts_with('@') {
                    if try_encode_marker_to_pickle(key, &v, buf, expand_refs)? {
                        return Ok(());
                    }
                }
                // Non-marker single key or unrecognized marker value
                buf.push(EMPTY_DICT);
                buf.push(MARK);
                write_string(buf, key);
                encode_pyobject_to_pickle(&v, buf, expand_refs)?;
                buf.push(SETITEMS);
                return Ok(());
            }
        }
        // Non-string key: fallback
        let pv = pydict_to_pickle_value(dict, expand_refs)?;
        encode_value_into(&pv, buf)?;
        return Ok(());
    }

    // 2-4 key dicts: quick scan for '@' prefix
    let mut has_marker = false;
    for (k, _v) in dict {
        if let Ok(s) = k.cast::<PyString>() {
            if let Ok(key_str) = s.to_str() {
                if key_str.starts_with('@') {
                    has_marker = true;
                    break;
                }
            }
        }
    }

    if !has_marker {
        return encode_plain_dict_to_pickle(dict, buf, expand_refs);
    }

    // Multi-key marker dict — targeted checks
    let py = dict.py();

    // @cls (+@s) is the most common multi-key marker
    if let Some(cls_val) = dict.get_item(intern!(py, "@cls"))? {
        if let Ok(cls_list) = cls_val.cast::<PyList>() {
            if cls_list.len() == 2 {
                let item0 = cls_list.get_item(0)?;
                let item1 = cls_list.get_item(1)?;
                if let (Ok(mod_py), Ok(name_py)) = (
                    item0.cast::<PyString>(),
                    item1.cast::<PyString>(),
                ) {
                    let module = mod_py.to_str()?;
                    let name = name_py.to_str()?;

                    if let Some(state_val) = dict.get_item(intern!(py, "@s"))? {
                        // Instance: GLOBAL module\nname\n EMPTY_TUPLE NEWOBJ state BUILD
                        write_global(buf, module, name);
                        buf.push(EMPTY_TUPLE);
                        buf.push(NEWOBJ);

                        if let Some(info) = btrees::classify_btree(module, name) {
                            encode_btree_state_to_pickle(&info, &state_val, buf, expand_refs)?;
                        } else {
                            encode_pyobject_to_pickle(&state_val, buf, expand_refs)?;
                        }
                        buf.push(BUILD);
                        return Ok(());
                    }
                    // @cls alone → GLOBAL
                    write_global(buf, module, name);
                    return Ok(());
                }
            }
        }
    }

    // Other multi-key markers: fall back to PickleValue path
    let pv = pydict_to_pickle_value(dict, expand_refs)?;
    encode_value_into(&pv, buf)?;
    Ok(())
}

/// Write a plain dict (no markers) directly to pickle buffer.
#[inline]
fn encode_plain_dict_to_pickle(
    dict: &Bound<'_, PyDict>,
    buf: &mut Vec<u8>,
    expand_refs: bool,
) -> PyResult<()> {
    buf.push(EMPTY_DICT);
    if !dict.is_empty() {
        buf.push(MARK);
        for (k, v) in dict {
            // Optimistically assume string keys (>99% in ZODB)
            if let Ok(s) = k.cast::<PyString>() {
                if let Ok(key_str) = s.to_str() {
                    write_string(buf, key_str);
                    encode_pyobject_to_pickle(&v, buf, expand_refs)?;
                    continue;
                }
            }
            // Non-string key: fallback to PickleValue for this key
            let k_pv = pyobject_to_pickle_value(&k, expand_refs)?;
            encode_value_into(&k_pv, buf)?;
            encode_pyobject_to_pickle(&v, buf, expand_refs)?;
        }
        buf.push(SETITEMS);
    }
    Ok(())
}

/// Try to encode a single-key marker dict directly to pickle.
/// Returns true if the marker was handled.
fn try_encode_marker_to_pickle(
    key: &str,
    v: &Bound<'_, pyo3::PyAny>,
    buf: &mut Vec<u8>,
    expand_refs: bool,
) -> PyResult<bool> {
    match key {
        "@ref" => {
            if expand_refs {
                // Expand compact hex ref → PersistentRef(Tuple([Bytes(oid), None/Global]))
                if let Ok(s) = v.cast::<PyString>() {
                    if let Ok(hex_str) = s.to_str() {
                        let oid = hex::decode(hex_str)
                            .map_err(|e| CodecError::Json(format!("hex decode: {e}")))?;
                        write_bytes_val(buf, &oid);
                        buf.push(NONE);
                        buf.push(TUPLE2);
                        buf.push(BINPERSID);
                        return Ok(true);
                    }
                }
                if let Ok(list) = v.cast::<PyList>() {
                    if list.len() == 2 {
                        let item0 = list.get_item(0)?;
                        let item1 = list.get_item(1)?;
                        if let (Ok(oid_py), Ok(cls_py)) = (
                            item0.cast::<PyString>(),
                            item1.cast::<PyString>(),
                        ) {
                            let oid_str = oid_py.to_str()?;
                            let cls_str = cls_py.to_str()?;
                            let oid = hex::decode(oid_str)
                                .map_err(|e| CodecError::Json(format!("hex decode: {e}")))?;
                            write_bytes_val(buf, &oid);
                            let (module, name) = if let Some(dot) = cls_str.rfind('.') {
                                (&cls_str[..dot], &cls_str[dot + 1..])
                            } else {
                                ("", cls_str)
                            };
                            write_global(buf, module, name);
                            buf.push(TUPLE2);
                            buf.push(BINPERSID);
                            return Ok(true);
                        }
                    }
                }
                // Fallback for complex @ref values
                let pv = expand_compact_ref(v)?;
                encode_value_into(&pv, buf)?;
                Ok(true)
            } else {
                // Non-expanding: encode inner value + BINPERSID
                encode_pyobject_to_pickle(v, buf, expand_refs)?;
                buf.push(BINPERSID);
                Ok(true)
            }
        }
        "@t" => {
            if let Ok(list) = v.cast::<PyList>() {
                let n = list.len();
                match n {
                    0 => buf.push(EMPTY_TUPLE),
                    1 => {
                        encode_pyobject_to_pickle(&list.get_item(0)?, buf, expand_refs)?;
                        buf.push(TUPLE1);
                    }
                    2 => {
                        encode_pyobject_to_pickle(&list.get_item(0)?, buf, expand_refs)?;
                        encode_pyobject_to_pickle(&list.get_item(1)?, buf, expand_refs)?;
                        buf.push(TUPLE2);
                    }
                    3 => {
                        encode_pyobject_to_pickle(&list.get_item(0)?, buf, expand_refs)?;
                        encode_pyobject_to_pickle(&list.get_item(1)?, buf, expand_refs)?;
                        encode_pyobject_to_pickle(&list.get_item(2)?, buf, expand_refs)?;
                        buf.push(TUPLE3);
                    }
                    _ => {
                        buf.push(MARK);
                        for item in list.iter() {
                            encode_pyobject_to_pickle(&item, buf, expand_refs)?;
                        }
                        buf.push(TUPLE);
                    }
                }
                return Ok(true);
            }
            Ok(false)
        }
        "@b" => {
            if let Ok(s) = v.cast::<PyString>() {
                if let Ok(b64_str) = s.to_str() {
                    let bytes = BASE64
                        .decode(b64_str)
                        .map_err(|e| CodecError::Json(format!("base64 decode: {e}")))?;
                    write_bytes_val(buf, &bytes);
                    return Ok(true);
                }
            }
            Ok(false)
        }
        "@cls" => {
            if let Ok(cls_list) = v.cast::<PyList>() {
                if cls_list.len() == 2 {
                    let item0 = cls_list.get_item(0)?;
                    let item1 = cls_list.get_item(1)?;
                    if let (Ok(mod_py), Ok(name_py)) = (
                        item0.cast::<PyString>(),
                        item1.cast::<PyString>(),
                    ) {
                        let module = mod_py.to_str()?;
                        let name = name_py.to_str()?;
                        write_global(buf, module, name);
                        return Ok(true);
                    }
                }
            }
            Ok(false)
        }
        _ => {
            // All other single-key markers (@dt, @date, @time, @td, @dec, @uuid,
            // @pkl, @reduce, @bi, @d, @set, @fset, @inst):
            // fall back to PickleValue conversion + encode
            let py = v.py();
            let pv =
                if let Some(pv) = try_decode_single_key_marker(py, key, v, expand_refs)? {
                    pv
                } else {
                    // Unrecognized marker: encode as plain dict
                    let val_pv = pyobject_to_pickle_value(v, expand_refs)?;
                    PickleValue::Dict(vec![(PickleValue::String(key.to_owned()), val_pv)])
                };
            encode_value_into(&pv, buf)?;
            Ok(true)
        }
    }
}

/// Encode BTree state Py<PyAny> directly to pickle opcodes.
pub fn encode_btree_state_to_pickle(
    info: &btrees::BTreeClassInfo,
    state_obj: &Bound<'_, pyo3::PyAny>,
    buf: &mut Vec<u8>,
    expand_refs: bool,
) -> PyResult<()> {
    // None → NONE
    if state_obj.is_none() {
        buf.push(NONE);
        return Ok(());
    }

    let dict = match state_obj.cast::<PyDict>() {
        Ok(d) => d,
        Err(_) => {
            // Not a dict — generic encode (e.g., scalar for BTrees.Length)
            encode_pyobject_to_pickle(state_obj, buf, expand_refs)?;
            return Ok(());
        }
    };

    let py = dict.py();

    // @kv — map data
    if let Some(kv_val) = dict.get_item(intern!(py, "@kv"))? {
        if let Ok(kv_list) = kv_val.cast::<PyList>() {
            let next_val = dict.get_item(intern!(py, "@next"))?;

            // Write flat data tuple: MARK k1 v1 k2 v2 ... TUPLE (or TUPLE2 etc.)
            encode_flat_kv_tuple(kv_list, buf, expand_refs)?;

            // Wrap with appropriate nesting
            match info.kind {
                btrees::BTreeNodeKind::BTree | btrees::BTreeNodeKind::TreeSet => {
                    // 4-level: (((data,),),)
                    buf.push(TUPLE1);
                    buf.push(TUPLE1);
                    buf.push(TUPLE1);
                }
                btrees::BTreeNodeKind::Bucket | btrees::BTreeNodeKind::Set => {
                    // 2-level: (data,) or (data, next_ref)
                    if let Some(next) = next_val {
                        encode_pyobject_to_pickle(&next, buf, expand_refs)?;
                        buf.push(TUPLE2);
                    } else {
                        buf.push(TUPLE1);
                    }
                }
            }
            return Ok(());
        }
    }

    // @ks — set data
    if let Some(ks_val) = dict.get_item(intern!(py, "@ks"))? {
        if let Ok(ks_list) = ks_val.cast::<PyList>() {
            let next_val = dict.get_item(intern!(py, "@next"))?;

            encode_flat_keys_tuple(ks_list, buf, expand_refs)?;

            match info.kind {
                btrees::BTreeNodeKind::BTree | btrees::BTreeNodeKind::TreeSet => {
                    buf.push(TUPLE1);
                    buf.push(TUPLE1);
                    buf.push(TUPLE1);
                }
                btrees::BTreeNodeKind::Bucket | btrees::BTreeNodeKind::Set => {
                    if let Some(next) = next_val {
                        encode_pyobject_to_pickle(&next, buf, expand_refs)?;
                        buf.push(TUPLE2);
                    } else {
                        buf.push(TUPLE1);
                    }
                }
            }
            return Ok(());
        }
    }

    // @children + @first — large BTree
    if let Some(children_val) = dict.get_item(intern!(py, "@children"))? {
        let first_val = dict
            .get_item(intern!(py, "@first"))?
            .ok_or_else(|| CodecError::InvalidData("@children without @first".into()))?;
        if let Ok(children_list) = children_val.cast::<PyList>() {
            let n = children_list.len();
            match n {
                0 => buf.push(EMPTY_TUPLE),
                1 => {
                    encode_pyobject_to_pickle(&children_list.get_item(0)?, buf, expand_refs)?;
                    buf.push(TUPLE1);
                }
                2 => {
                    encode_pyobject_to_pickle(&children_list.get_item(0)?, buf, expand_refs)?;
                    encode_pyobject_to_pickle(&children_list.get_item(1)?, buf, expand_refs)?;
                    buf.push(TUPLE2);
                }
                3 => {
                    encode_pyobject_to_pickle(&children_list.get_item(0)?, buf, expand_refs)?;
                    encode_pyobject_to_pickle(&children_list.get_item(1)?, buf, expand_refs)?;
                    encode_pyobject_to_pickle(&children_list.get_item(2)?, buf, expand_refs)?;
                    buf.push(TUPLE3);
                }
                _ => {
                    buf.push(MARK);
                    for item in children_list.iter() {
                        encode_pyobject_to_pickle(&item, buf, expand_refs)?;
                    }
                    buf.push(TUPLE);
                }
            }
            // First bucket
            encode_pyobject_to_pickle(&first_val, buf, expand_refs)?;
            buf.push(TUPLE2);
            return Ok(());
        }
    }

    // Fallback: generic encode
    encode_pyobject_to_pickle(state_obj, buf, expand_refs)?;
    Ok(())
}

/// Write @kv pairs as a flat tuple: MARK k1 v1 k2 v2 ... TUPLE
fn encode_flat_kv_tuple(
    kv_list: &Bound<'_, PyList>,
    buf: &mut Vec<u8>,
    expand_refs: bool,
) -> PyResult<()> {
    let n_pairs = kv_list.len();
    let n_items = n_pairs * 2;

    if n_items == 0 {
        buf.push(EMPTY_TUPLE);
        return Ok(());
    }

    if n_items > 3 {
        // Common path: OOBucket with many pairs
        buf.push(MARK);
        for pair_obj in kv_list.iter() {
            let pair = pair_obj.cast::<PyList>()?;
            encode_pyobject_to_pickle(&pair.get_item(0)?, buf, expand_refs)?;
            encode_pyobject_to_pickle(&pair.get_item(1)?, buf, expand_refs)?;
        }
        buf.push(TUPLE);
    } else {
        // 1 pair (2 items)
        for pair_obj in kv_list.iter() {
            let pair = pair_obj.cast::<PyList>()?;
            encode_pyobject_to_pickle(&pair.get_item(0)?, buf, expand_refs)?;
            encode_pyobject_to_pickle(&pair.get_item(1)?, buf, expand_refs)?;
        }
        match n_items {
            2 => buf.push(TUPLE2),
            3 => buf.push(TUPLE3),
            _ => {
                // Shouldn't happen (1 pair = 2 items minimum)
                buf.push(TUPLE1);
            }
        }
    }
    Ok(())
}

/// Write @ks keys as a flat tuple: MARK k1 k2 ... TUPLE
fn encode_flat_keys_tuple(
    ks_list: &Bound<'_, PyList>,
    buf: &mut Vec<u8>,
    expand_refs: bool,
) -> PyResult<()> {
    let n = ks_list.len();

    if n == 0 {
        buf.push(EMPTY_TUPLE);
        return Ok(());
    }

    if n > 3 {
        buf.push(MARK);
        for item in ks_list.iter() {
            encode_pyobject_to_pickle(&item, buf, expand_refs)?;
        }
        buf.push(TUPLE);
    } else {
        for item in ks_list.iter() {
            encode_pyobject_to_pickle(&item, buf, expand_refs)?;
        }
        match n {
            1 => buf.push(TUPLE1),
            2 => buf.push(TUPLE2),
            3 => buf.push(TUPLE3),
            _ => unreachable!(),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::PickleValue;

    #[test]
    fn test_collect_refs_empty() {
        let val = PickleValue::Dict(vec![]);
        let mut refs = Vec::new();
        collect_refs_from_pickle_value(&val, &mut refs);
        assert!(refs.is_empty());
    }

    #[test]
    fn test_collect_refs_persistent_ref() {
        // OID = 8 bytes big-endian for value 42
        let oid = vec![0, 0, 0, 0, 0, 0, 0, 42];
        let val = PickleValue::PersistentRef(Box::new(PickleValue::Tuple(vec![
            PickleValue::Bytes(oid),
            PickleValue::None,
        ])));
        let mut refs = Vec::new();
        collect_refs_from_pickle_value(&val, &mut refs);
        assert_eq!(refs, vec![42]);
    }

    #[test]
    fn test_collect_refs_nested_in_dict() {
        let oid1 = vec![0, 0, 0, 0, 0, 0, 0, 1];
        let oid2 = vec![0, 0, 0, 0, 0, 0, 0, 2];
        let ref1 = PickleValue::PersistentRef(Box::new(PickleValue::Tuple(vec![
            PickleValue::Bytes(oid1),
            PickleValue::None,
        ])));
        let ref2 = PickleValue::PersistentRef(Box::new(PickleValue::Tuple(vec![
            PickleValue::Bytes(oid2),
            PickleValue::None,
        ])));
        let val = PickleValue::Dict(vec![
            (PickleValue::String("a".to_string()), ref1),
            (PickleValue::String("b".to_string()), ref2),
        ]);
        let mut refs = Vec::new();
        collect_refs_from_pickle_value(&val, &mut refs);
        assert_eq!(refs, vec![1, 2]);
    }

    #[test]
    fn test_collect_refs_in_list() {
        let oid = vec![0, 0, 0, 0, 0, 0, 0, 99];
        let pref = PickleValue::PersistentRef(Box::new(PickleValue::Tuple(vec![
            PickleValue::Bytes(oid),
            PickleValue::None,
        ])));
        let val = PickleValue::List(vec![
            PickleValue::String("hello".to_string()),
            pref,
        ]);
        let mut refs = Vec::new();
        collect_refs_from_pickle_value(&val, &mut refs);
        assert_eq!(refs, vec![99]);
    }

    #[test]
    fn test_collect_refs_skips_short_oid() {
        // Cross-database refs may have different OID lengths
        let oid = vec![1, 2, 3];  // Not 8 bytes → skip
        let val = PickleValue::PersistentRef(Box::new(PickleValue::Tuple(vec![
            PickleValue::Bytes(oid),
            PickleValue::None,
        ])));
        let mut refs = Vec::new();
        collect_refs_from_pickle_value(&val, &mut refs);
        assert!(refs.is_empty());
    }

    #[test]
    fn test_collect_refs_in_instance() {
        let oid = vec![0, 0, 0, 0, 0, 0, 0, 7];
        let pref = PickleValue::PersistentRef(Box::new(PickleValue::Tuple(vec![
            PickleValue::Bytes(oid),
            PickleValue::None,
        ])));
        let val = PickleValue::Instance {
            module: "myapp".to_string(),
            name: "Obj".to_string(),
            state: Box::new(PickleValue::Dict(vec![
                (PickleValue::String("ref".to_string()), pref),
            ])),
        };
        let mut refs = Vec::new();
        collect_refs_from_pickle_value(&val, &mut refs);
        assert_eq!(refs, vec![7]);
    }

    #[test]
    fn test_collect_refs_in_reduce() {
        let oid = vec![0, 0, 0, 0, 0, 0, 0, 5];
        let pref = PickleValue::PersistentRef(Box::new(PickleValue::Tuple(vec![
            PickleValue::Bytes(oid),
            PickleValue::None,
        ])));
        let val = PickleValue::Reduce {
            callable: Box::new(PickleValue::Global {
                module: "builtins".to_string(),
                name: "set".to_string(),
            }),
            args: Box::new(PickleValue::Tuple(vec![
                PickleValue::List(vec![pref]),
            ])),
        };
        let mut refs = Vec::new();
        collect_refs_from_pickle_value(&val, &mut refs);
        assert_eq!(refs, vec![5]);
    }

    #[test]
    fn test_collect_refs_no_refs() {
        let val = PickleValue::Dict(vec![
            (PickleValue::String("title".to_string()), PickleValue::String("Hello".to_string())),
            (PickleValue::String("count".to_string()), PickleValue::Int(42)),
        ]);
        let mut refs = Vec::new();
        collect_refs_from_pickle_value(&val, &mut refs);
        assert!(refs.is_empty());
    }
}

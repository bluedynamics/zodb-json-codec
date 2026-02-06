mod btrees;
mod decode;
mod encode;
mod error;
mod json;
mod known_types;
mod opcodes;
mod types;
mod zodb;

use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict};

use crate::decode::decode_pickle;
use crate::encode::encode_pickle;
use crate::error::CodecError;
use crate::json::{json_to_pickle_value, pickle_value_to_json};

/// Convert pickle bytes to a JSON string.
#[pyfunction]
fn pickle_to_json(py: Python<'_>, data: &[u8]) -> PyResult<String> {
    let val = decode_pickle(data).map_err(CodecError::from)?;
    let json_val = pickle_value_to_json(&val)?;
    let json_str = serde_json::to_string_pretty(&json_val)
        .map_err(|e| CodecError::Json(e.to_string()))?;
    let _ = py; // suppress unused warning
    Ok(json_str)
}

/// Convert a JSON string to pickle bytes.
#[pyfunction]
fn json_to_pickle(py: Python<'_>, json_str: &str) -> PyResult<Py<PyBytes>> {
    let json_val: serde_json::Value =
        serde_json::from_str(json_str).map_err(|e| CodecError::Json(e.to_string()))?;
    let pickle_val = json_to_pickle_value(&json_val)?;
    let bytes = encode_pickle(&pickle_val)?;
    Ok(PyBytes::new(py, &bytes).into())
}

/// Convert pickle bytes to a Python dict (via JSON internally).
#[pyfunction]
fn pickle_to_dict(py: Python<'_>, data: &[u8]) -> PyResult<PyObject> {
    let val = decode_pickle(data).map_err(CodecError::from)?;
    let json_val = pickle_value_to_json(&val)?;
    json_value_to_pyobject(py, &json_val)
}

/// Convert a Python dict to pickle bytes.
#[pyfunction]
fn dict_to_pickle(py: Python<'_>, obj: &Bound<'_, PyDict>) -> PyResult<Py<PyBytes>> {
    let json_val = pyobject_to_json_value(obj.as_any())?;
    let pickle_val = json_to_pickle_value(&json_val)?;
    let bytes = encode_pickle(&pickle_val)?;
    Ok(PyBytes::new(py, &bytes).into())
}

/// Decode a ZODB record (two concatenated pickles) into a JSON string.
/// Returns: `{"@cls": ["module", "name"], "@s": { ... state ... }}`
#[pyfunction]
fn decode_zodb_record(py: Python<'_>, data: &[u8]) -> PyResult<PyObject> {
    let json_val = zodb::decode_zodb_record(data)?;
    json_value_to_pyobject(py, &json_val)
}

/// Encode a ZODB JSON record back into two concatenated pickles.
#[pyfunction]
fn encode_zodb_record(py: Python<'_>, obj: &Bound<'_, PyDict>) -> PyResult<Py<PyBytes>> {
    let json_val = pyobject_to_json_value(obj.as_any())?;
    let bytes = zodb::encode_zodb_record(&json_val)?;
    Ok(PyBytes::new(py, &bytes).into())
}

/// Convert a serde_json Value to a Python object.
fn json_value_to_pyobject(py: Python<'_>, val: &serde_json::Value) -> PyResult<PyObject> {
    match val {
        serde_json::Value::Null => Ok(py.None()),
        serde_json::Value::Bool(b) => Ok(b.into_pyobject(py)?.to_owned().into_any().unbind()),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Ok(i.into_pyobject(py)?.into_any().unbind())
            } else if let Some(f) = n.as_f64() {
                Ok(f.into_pyobject(py)?.into_any().unbind())
            } else {
                Ok(py.None())
            }
        }
        serde_json::Value::String(s) => Ok(s.into_pyobject(py)?.into_any().unbind()),
        serde_json::Value::Array(arr) => {
            let list = pyo3::types::PyList::empty(py);
            for item in arr {
                list.append(json_value_to_pyobject(py, item)?)?;
            }
            Ok(list.into_any().unbind())
        }
        serde_json::Value::Object(map) => {
            let dict = PyDict::new(py);
            for (k, v) in map {
                dict.set_item(k, json_value_to_pyobject(py, v)?)?;
            }
            Ok(dict.into_any().unbind())
        }
    }
}

/// Convert a Python object to a serde_json Value.
fn pyobject_to_json_value(obj: &Bound<'_, pyo3::PyAny>) -> PyResult<serde_json::Value> {
    if obj.is_none() {
        return Ok(serde_json::Value::Null);
    }
    if let Ok(b) = obj.extract::<bool>() {
        return Ok(serde_json::Value::Bool(b));
    }
    if let Ok(i) = obj.extract::<i64>() {
        return Ok(serde_json::json!(i));
    }
    if let Ok(f) = obj.extract::<f64>() {
        return Ok(serde_json::Number::from_f64(f)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null));
    }
    if let Ok(s) = obj.extract::<String>() {
        return Ok(serde_json::Value::String(s));
    }
    if let Ok(list) = obj.downcast::<pyo3::types::PyList>() {
        let arr: PyResult<Vec<serde_json::Value>> =
            list.iter().map(|item| pyobject_to_json_value(&item)).collect();
        return Ok(serde_json::Value::Array(arr?));
    }
    if let Ok(dict) = obj.downcast::<PyDict>() {
        let mut map = serde_json::Map::new();
        for (k, v) in dict {
            let key: String = k.extract()?;
            map.insert(key, pyobject_to_json_value(&v)?);
        }
        return Ok(serde_json::Value::Object(map));
    }
    // Fallback: try str() representation
    let s = obj.str()?.to_string();
    Ok(serde_json::Value::String(s))
}

/// Python module definition
#[pymodule]
fn _rust(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(pickle_to_json, m)?)?;
    m.add_function(wrap_pyfunction!(json_to_pickle, m)?)?;
    m.add_function(wrap_pyfunction!(pickle_to_dict, m)?)?;
    m.add_function(wrap_pyfunction!(dict_to_pickle, m)?)?;
    m.add_function(wrap_pyfunction!(decode_zodb_record, m)?)?;
    m.add_function(wrap_pyfunction!(encode_zodb_record, m)?)?;
    Ok(())
}

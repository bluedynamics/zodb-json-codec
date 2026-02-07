mod btrees;
mod decode;
mod encode;
mod error;
mod json;
mod known_types;
mod opcodes;
mod pyconv;
mod types;
mod zodb;

use pyo3::prelude::*;
use pyo3::intern;
use pyo3::types::{PyBytes, PyDict, PyList};

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

/// Convert pickle bytes to a Python dict (direct PickleValue → PyObject).
#[pyfunction]
fn pickle_to_dict(py: Python<'_>, data: &[u8]) -> PyResult<PyObject> {
    let val = decode_pickle(data).map_err(CodecError::from)?;
    pyconv::pickle_value_to_pyobject(py, &val, false)
}

/// Convert a Python dict to pickle bytes (direct PyObject → PickleValue).
#[pyfunction]
fn dict_to_pickle(py: Python<'_>, obj: &Bound<'_, PyDict>) -> PyResult<Py<PyBytes>> {
    let pickle_val = pyconv::pyobject_to_pickle_value(obj.as_any(), false)?;
    let bytes = encode_pickle(&pickle_val)?;
    Ok(PyBytes::new(py, &bytes).into())
}

/// Decode a ZODB record (two concatenated pickles) into a Python dict.
/// Returns: `{"@cls": ["module", "name"], "@s": { ... state ... }}`
#[pyfunction]
fn decode_zodb_record(py: Python<'_>, data: &[u8]) -> PyResult<PyObject> {
    let (class_data, state_data) = zodb::split_zodb_record(data)?;
    let class_val = decode_pickle(class_data).map_err(CodecError::from)?;
    let state_val = decode_pickle(state_data).map_err(CodecError::from)?;
    let (module, name) = zodb::extract_class_info(&class_val);

    // BTree-aware state conversion with inline persistent ref compaction
    let state_obj = if let Some(info) = btrees::classify_btree(&module, &name) {
        pyconv::btree_state_to_pyobject(py, &info, &state_val, true)?
    } else {
        pyconv::pickle_value_to_pyobject(py, &state_val, true)?
    };

    // Build result dict directly
    let dict = PyDict::new(py);
    let cls_list = PyList::new(py, [module.as_str(), name.as_str()])?;
    dict.set_item(intern!(py, "@cls"), cls_list)?;
    dict.set_item(intern!(py, "@s"), state_obj)?;
    Ok(dict.into_any().unbind())
}

/// Encode a ZODB JSON record back into two concatenated pickles.
#[pyfunction]
fn encode_zodb_record(py: Python<'_>, obj: &Bound<'_, PyDict>) -> PyResult<Py<PyBytes>> {
    let cls_val = obj
        .get_item(intern!(py, "@cls"))?
        .ok_or_else(|| CodecError::InvalidData("missing @cls in ZODB record".to_string()))?;
    let cls_list = cls_val.downcast::<PyList>().map_err(|_| {
        CodecError::InvalidData("@cls must be a list".to_string())
    })?;
    if cls_list.len() != 2 {
        return Err(CodecError::InvalidData("@cls must be [module, name]".to_string()).into());
    }
    let module: String = cls_list.get_item(0)?.extract()?;
    let name: String = cls_list.get_item(1)?.extract()?;

    // Check for BTree class
    let btree_info = btrees::classify_btree(&module, &name);

    // Encode class pickle
    let class_val = types::PickleValue::Global {
        module: module.clone(),
        name: name.clone(),
    };
    let class_bytes = encode_pickle(&class_val)?;

    // Get state with persistent ref expansion
    let state_obj = obj
        .get_item(intern!(py, "@s"))?
        .unwrap_or_else(|| py.None().into_bound(py));

    let state_val = if let Some(info) = btree_info {
        pyconv::btree_state_from_pyobject(&info, &state_obj, true)?
    } else {
        pyconv::pyobject_to_pickle_value(&state_obj, true)?
    };
    let state_bytes = encode_pickle(&state_val)?;

    // Concatenate
    let mut result = class_bytes;
    result.extend_from_slice(&state_bytes);
    Ok(PyBytes::new(py, &result).into())
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

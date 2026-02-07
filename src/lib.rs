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
use pyo3::types::{PyBytes, PyDict, PyList, PyString};

use crate::decode::{decode_pickle, decode_zodb_pickles};
use crate::encode::encode_pickle;
use crate::error::CodecError;
use crate::json::{json_to_pickle_value, pickle_value_to_json};

/// Convert pickle bytes to a JSON string.
#[pyfunction]
fn pickle_to_json(data: &[u8]) -> PyResult<String> {
    let val = decode_pickle(data).map_err(CodecError::from)?;
    let json_val = pickle_value_to_json(&val)?;
    let json_str = serde_json::to_string_pretty(&json_val)
        .map_err(|e| CodecError::Json(e.to_string()))?;
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

/// Convert pickle bytes to a Python dict (direct PickleValue → Py<PyAny>).
#[pyfunction]
fn pickle_to_dict(py: Python<'_>, data: &[u8]) -> PyResult<Py<PyAny>> {
    let val = decode_pickle(data).map_err(CodecError::from)?;
    pyconv::pickle_value_to_pyobject(py, &val, false)
}

/// Convert a Python dict to pickle bytes (direct Py<PyAny> → pickle bytes).
#[pyfunction]
fn dict_to_pickle(py: Python<'_>, obj: &Bound<'_, PyDict>) -> PyResult<Py<PyBytes>> {
    let bytes = pyconv::encode_pyobject_as_pickle(obj.as_any(), false)?;
    Ok(PyBytes::new(py, &bytes).into())
}

/// Decode a ZODB record (two concatenated pickles) into a Python dict.
/// Returns: `{"@cls": ["module", "name"], "@s": { ... state ... }}`
#[pyfunction]
fn decode_zodb_record(py: Python<'_>, data: &[u8]) -> PyResult<Py<PyAny>> {
    let (class_val, state_val) = decode_zodb_pickles(data).map_err(CodecError::from)?;
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
/// Uses the direct Py<PyAny> → pickle encoder, bypassing PickleValue allocations.
#[pyfunction]
fn encode_zodb_record(py: Python<'_>, obj: &Bound<'_, PyDict>) -> PyResult<Py<PyBytes>> {
    let cls_val = obj
        .get_item(intern!(py, "@cls"))?
        .ok_or_else(|| CodecError::InvalidData("missing @cls in ZODB record".to_string()))?;
    let cls_list = cls_val.cast::<PyList>().map_err(|_| {
        CodecError::InvalidData("@cls must be a list".to_string())
    })?;
    if cls_list.len() != 2 {
        return Err(CodecError::InvalidData("@cls must be [module, name]".to_string()).into());
    }

    // Borrow module/name as &str from Python (zero-copy)
    let item0 = cls_list.get_item(0)?;
    let item1 = cls_list.get_item(1)?;
    let module = item0.cast::<PyString>()
        .map_err(|_| CodecError::InvalidData("@cls[0] must be a string".to_string()))?
        .to_str()?;
    let name = item1.cast::<PyString>()
        .map_err(|_| CodecError::InvalidData("@cls[1] must be a string".to_string()))?
        .to_str()?;

    // Get state
    let state_obj = obj
        .get_item(intern!(py, "@s"))?
        .unwrap_or_else(|| py.None().into_bound(py));

    // Direct encode: class pickle + state pickle, no PickleValue intermediates
    let result = pyconv::encode_zodb_record_direct(module, name, &state_obj)?;
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

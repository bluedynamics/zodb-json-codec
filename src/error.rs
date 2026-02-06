use pyo3::exceptions::PyValueError;
use pyo3::PyErr;
use std::fmt;

#[derive(Debug)]
pub enum CodecError {
    /// Unexpected end of pickle stream
    UnexpectedEof,
    /// Unknown or unsupported pickle opcode
    UnknownOpcode(u8),
    /// Stack underflow during pickle evaluation
    StackUnderflow,
    /// Invalid pickle data
    InvalidData(String),
    /// JSON serialization/deserialization error
    Json(String),
    /// Invalid UTF-8 in pickle string
    InvalidUtf8,
}

impl fmt::Display for CodecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CodecError::UnexpectedEof => write!(f, "unexpected end of pickle stream"),
            CodecError::UnknownOpcode(op) => write!(f, "unknown pickle opcode: 0x{op:02x}"),
            CodecError::StackUnderflow => write!(f, "pickle stack underflow"),
            CodecError::InvalidData(msg) => write!(f, "invalid pickle data: {msg}"),
            CodecError::Json(msg) => write!(f, "JSON error: {msg}"),
            CodecError::InvalidUtf8 => write!(f, "invalid UTF-8 in pickle string"),
        }
    }
}

impl std::error::Error for CodecError {}

impl From<CodecError> for PyErr {
    fn from(err: CodecError) -> PyErr {
        PyValueError::new_err(err.to_string())
    }
}

impl From<serde_json::Error> for CodecError {
    fn from(err: serde_json::Error) -> Self {
        CodecError::Json(err.to_string())
    }
}

//! Direct JSON string writer — writes JSON tokens to a String buffer
//! without allocating intermediate serde_json::Value nodes.

use std::fmt::Write;

/// A low-level JSON token writer that appends directly to a String buffer.
pub struct JsonWriter {
    buf: String,
}

impl JsonWriter {
    pub fn new() -> Self {
        Self {
            buf: String::new(),
        }
    }

    pub fn with_capacity(cap: usize) -> Self {
        Self {
            buf: String::with_capacity(cap),
        }
    }

    /// Consume the writer and return the JSON string.
    pub fn into_string(self) -> String {
        self.buf
    }

    /// Borrow the inner buffer (for length checks, etc.).
    #[inline]
    pub fn as_str(&self) -> &str {
        &self.buf
    }

    /// Take the string out, leaving an empty buffer that retains its allocation.
    pub fn take(&mut self) -> String {
        std::mem::take(&mut self.buf)
    }

    /// Clear the buffer while retaining capacity.
    pub fn clear(&mut self) {
        self.buf.clear();
    }

    // -- Primitives --

    #[inline]
    pub fn write_null(&mut self) {
        self.buf.push_str("null");
    }

    #[inline]
    pub fn write_bool(&mut self, b: bool) {
        self.buf.push_str(if b { "true" } else { "false" });
    }

    #[inline]
    pub fn write_i64(&mut self, n: i64) {
        let _ = write!(self.buf, "{n}");
    }

    #[inline]
    pub fn write_f64(&mut self, f: f64) {
        if f.is_nan() || f.is_infinite() {
            // Match serde_json behavior: NaN/Infinity → null
            self.buf.push_str("null");
        } else {
            // Use ryu for fast, exact float formatting
            let mut ryu_buf = ryu::Buffer::new();
            self.buf.push_str(ryu_buf.format_finite(f));
        }
    }

    /// Write a JSON-escaped string (with surrounding quotes).
    #[inline]
    pub fn write_string(&mut self, s: &str) {
        self.buf.push('"');
        write_escaped(&mut self.buf, s);
        self.buf.push('"');
    }

    /// Write a pre-known string literal that needs no escaping (with quotes).
    /// SAFETY: caller must guarantee `s` contains no characters that need JSON escaping.
    #[inline]
    pub fn write_string_literal(&mut self, s: &str) {
        self.buf.push('"');
        self.buf.push_str(s);
        self.buf.push('"');
    }

    // -- Containers --

    #[inline]
    pub fn begin_object(&mut self) {
        self.buf.push('{');
    }

    #[inline]
    pub fn end_object(&mut self) {
        self.buf.push('}');
    }

    #[inline]
    pub fn begin_array(&mut self) {
        self.buf.push('[');
    }

    #[inline]
    pub fn end_array(&mut self) {
        self.buf.push(']');
    }

    /// Write `"key":` — a JSON object key followed by colon.
    #[inline]
    pub fn write_key(&mut self, key: &str) {
        self.write_string(key);
        self.buf.push(':');
    }

    /// Write a key that is known to need no escaping.
    #[inline]
    pub fn write_key_literal(&mut self, key: &str) {
        self.buf.push('"');
        self.buf.push_str(key);
        self.buf.push_str("\":");
    }

    #[inline]
    pub fn write_comma(&mut self) {
        self.buf.push(',');
    }

    /// Write a raw string directly to the buffer (for pre-formatted content).
    #[inline]
    pub fn write_raw(&mut self, s: &str) {
        self.buf.push_str(s);
    }
}

/// Write JSON-escaped string content (without surrounding quotes) to a String.
#[inline]
fn write_escaped(buf: &mut String, s: &str) {
    // Fast path: if no special chars, push entire string at once
    let needs_escape = s.bytes().any(|b| {
        b == b'"' || b == b'\\' || b < 0x20
    });
    if !needs_escape {
        buf.push_str(s);
        return;
    }

    // Slow path: escape character by character
    for ch in s.chars() {
        match ch {
            '"' => buf.push_str("\\\""),
            '\\' => buf.push_str("\\\\"),
            '\n' => buf.push_str("\\n"),
            '\r' => buf.push_str("\\r"),
            '\t' => buf.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                // Control characters → \u00XX
                let _ = write!(buf, "\\u{:04x}", c as u32);
            }
            c => buf.push(c),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_null() {
        let mut w = JsonWriter::new();
        w.write_null();
        assert_eq!(w.into_string(), "null");
    }

    #[test]
    fn test_bool_true() {
        let mut w = JsonWriter::new();
        w.write_bool(true);
        assert_eq!(w.into_string(), "true");
    }

    #[test]
    fn test_bool_false() {
        let mut w = JsonWriter::new();
        w.write_bool(false);
        assert_eq!(w.into_string(), "false");
    }

    #[test]
    fn test_i64() {
        let mut w = JsonWriter::new();
        w.write_i64(42);
        assert_eq!(w.into_string(), "42");
    }

    #[test]
    fn test_i64_negative() {
        let mut w = JsonWriter::new();
        w.write_i64(-100);
        assert_eq!(w.into_string(), "-100");
    }

    #[test]
    fn test_i64_zero() {
        let mut w = JsonWriter::new();
        w.write_i64(0);
        assert_eq!(w.into_string(), "0");
    }

    #[test]
    fn test_i64_max() {
        let mut w = JsonWriter::new();
        w.write_i64(i64::MAX);
        assert_eq!(w.into_string(), i64::MAX.to_string());
    }

    #[test]
    fn test_i64_min() {
        let mut w = JsonWriter::new();
        w.write_i64(i64::MIN);
        assert_eq!(w.into_string(), i64::MIN.to_string());
    }

    #[test]
    fn test_f64() {
        let mut w = JsonWriter::new();
        w.write_f64(3.14);
        let s = w.into_string();
        // ryu may format slightly differently, just check it parses back
        let parsed: f64 = s.parse().unwrap();
        assert!((parsed - 3.14).abs() < f64::EPSILON);
    }

    #[test]
    fn test_f64_nan() {
        let mut w = JsonWriter::new();
        w.write_f64(f64::NAN);
        assert_eq!(w.into_string(), "null");
    }

    #[test]
    fn test_f64_infinity() {
        let mut w = JsonWriter::new();
        w.write_f64(f64::INFINITY);
        assert_eq!(w.into_string(), "null");
    }

    #[test]
    fn test_f64_neg_infinity() {
        let mut w = JsonWriter::new();
        w.write_f64(f64::NEG_INFINITY);
        assert_eq!(w.into_string(), "null");
    }

    #[test]
    fn test_f64_zero() {
        let mut w = JsonWriter::new();
        w.write_f64(0.0);
        assert_eq!(w.into_string(), "0.0");
    }

    #[test]
    fn test_f64_integer_value() {
        let mut w = JsonWriter::new();
        w.write_f64(1.0);
        assert_eq!(w.into_string(), "1.0");
    }

    #[test]
    fn test_string_simple() {
        let mut w = JsonWriter::new();
        w.write_string("hello");
        assert_eq!(w.into_string(), "\"hello\"");
    }

    #[test]
    fn test_string_empty() {
        let mut w = JsonWriter::new();
        w.write_string("");
        assert_eq!(w.into_string(), "\"\"");
    }

    #[test]
    fn test_string_escapes() {
        let mut w = JsonWriter::new();
        w.write_string("a\"b\\c\nd\re\tf");
        assert_eq!(w.into_string(), "\"a\\\"b\\\\c\\nd\\re\\tf\"");
    }

    #[test]
    fn test_string_control_chars() {
        let mut w = JsonWriter::new();
        w.write_string("\x00\x01\x1f");
        assert_eq!(w.into_string(), "\"\\u0000\\u0001\\u001f\"");
    }

    #[test]
    fn test_string_unicode() {
        let mut w = JsonWriter::new();
        w.write_string("日本語");
        assert_eq!(w.into_string(), "\"日本語\"");
    }

    #[test]
    fn test_object() {
        let mut w = JsonWriter::new();
        w.begin_object();
        w.write_key("name");
        w.write_string("Alice");
        w.write_comma();
        w.write_key("age");
        w.write_i64(30);
        w.end_object();
        assert_eq!(w.into_string(), r#"{"name":"Alice","age":30}"#);
    }

    #[test]
    fn test_array() {
        let mut w = JsonWriter::new();
        w.begin_array();
        w.write_i64(1);
        w.write_comma();
        w.write_i64(2);
        w.write_comma();
        w.write_i64(3);
        w.end_array();
        assert_eq!(w.into_string(), "[1,2,3]");
    }

    #[test]
    fn test_nested() {
        let mut w = JsonWriter::new();
        w.begin_object();
        w.write_key_literal("items");
        w.begin_array();
        w.begin_object();
        w.write_key_literal("id");
        w.write_i64(1);
        w.end_object();
        w.end_array();
        w.end_object();
        assert_eq!(w.into_string(), r#"{"items":[{"id":1}]}"#);
    }

    #[test]
    fn test_with_capacity() {
        let w = JsonWriter::with_capacity(1024);
        assert_eq!(w.as_str(), "");
    }

    #[test]
    fn test_take_and_reuse() {
        let mut w = JsonWriter::new();
        w.write_null();
        let s = w.take();
        assert_eq!(s, "null");
        assert_eq!(w.as_str(), "");
        // Can reuse
        w.write_bool(true);
        assert_eq!(w.into_string(), "true");
    }

    #[test]
    fn test_clear() {
        let mut w = JsonWriter::with_capacity(100);
        w.write_i64(42);
        w.clear();
        assert_eq!(w.as_str(), "");
        w.write_string("fresh");
        assert_eq!(w.into_string(), "\"fresh\"");
    }

    #[test]
    fn test_key_literal() {
        let mut w = JsonWriter::new();
        w.begin_object();
        w.write_key_literal("@dt");
        w.write_string("2025-01-01");
        w.end_object();
        assert_eq!(w.into_string(), r#"{"@dt":"2025-01-01"}"#);
    }
}

//! Direct JSON string writer — writes JSON tokens to a String buffer
//! without allocating intermediate serde_json::Value nodes.

use base64::{engine::general_purpose::STANDARD as BASE64, Engine};

const HEX: &[u8; 16] = b"0123456789abcdef";
/// A low-level JSON token writer that appends directly to a String buffer.
/// Thread-local output buffers larger than this after a call are released
/// instead of retained, so one huge record does not pin memory for the
/// thread's lifetime. ZODB records are almost always far smaller.
pub const MAX_RETAINED_CAPACITY: usize = 4 << 20;

pub struct JsonWriter {
    buf: String,
}

impl JsonWriter {
    #[cfg(test)]
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
    #[cfg(test)]
    pub fn into_string(self) -> String {
        self.buf
    }

    /// Borrow the inner buffer (for length checks, etc.).
    #[inline]
    pub fn as_str(&self) -> &str {
        &self.buf
    }

    #[inline]
    pub fn capacity(&self) -> usize {
        self.buf.capacity()
    }

    /// Take the string out, leaving an empty buffer that retains its allocation.
    #[cfg(test)]
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
        let mut b = itoa::Buffer::new();
        self.buf.push_str(b.format(n));
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

    /// `"<hex of bytes>"` written straight into the buffer (hex needs no escaping).
    #[inline]
    pub fn write_hex_string(&mut self, bytes: &[u8]) {
        self.buf.reserve(bytes.len() * 2 + 2);
        self.buf.push('"');
        for &b in bytes {
            self.buf.push(HEX[(b >> 4) as usize] as char);
            self.buf.push(HEX[(b & 0x0f) as usize] as char);
        }
        self.buf.push('"');
    }

    /// `"<base64 of bytes>"` encoded straight into the buffer.
    #[inline]
    pub fn write_base64_string(&mut self, bytes: &[u8]) {
        self.buf.push('"');
        BASE64.encode_string(bytes, &mut self.buf);
        self.buf.push('"');
    }

    /// `"@ns:<base64 of key>":` for a dict key that holds NUL bytes.
    #[inline]
    pub fn write_ns_key(&mut self, key: &[u8]) {
        self.buf.push_str("\"@ns:");
        BASE64.encode_string(key, &mut self.buf);
        self.buf.push_str("\":");
    }

    /// `"<module>.<name>"` (just `"<name>"` for an empty module), escaped, as one string.
    #[inline]
    pub fn write_class_path(&mut self, module: &str, name: &str) {
        self.buf.push('"');
        if !module.is_empty() {
            write_escaped(&mut self.buf, module);
            self.buf.push('.');
        }
        write_escaped(&mut self.buf, name);
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
const ONES: u64 = 0x0101_0101_0101_0101;
const HIGHS: u64 = 0x8080_8080_8080_8080;
const LOWS: u64 = 0x7f7f_7f7f_7f7f_7f7f;

/// High bit set in every byte of `v` that is zero, exact per byte: the carry
/// of `(b & 0x7f) + 0x7f` never leaves its byte.
#[inline(always)]
fn zero_bytes(v: u64) -> u64 {
    !(((v & LOWS) + LOWS) | v) & HIGHS
}

/// High bit set in every byte of the chunk that needs escaping in a JSON
/// string: below 0x20 (the top three bits clear), `"` or `\`.
#[inline(always)]
fn escape_mask(chunk: u64) -> u64 {
    zero_bytes(chunk & (ONES * 0xe0))
        | zero_bytes(chunk ^ (ONES * u64::from(b'"')))
        | zero_bytes(chunk ^ (ONES * u64::from(b'\\')))
}

/// The JSON escape of one byte that needs it (below 0x20, `"` or `\`).
#[inline(always)]
fn push_escape(buf: &mut String, b: u8) {
    match b {
        b'"' => buf.push_str("\\\""),
        b'\\' => buf.push_str("\\\\"),
        b'\n' => buf.push_str("\\n"),
        b'\r' => buf.push_str("\\r"),
        b'\t' => buf.push_str("\\t"),
        _ => {
            buf.push_str("\\u00");
            buf.push(HEX[(b >> 4) as usize] as char);
            buf.push(HEX[(b & 0xf) as usize] as char);
        }
    }
}

#[inline]
fn write_escaped(buf: &mut String, s: &str) {
    // Eight bytes at a time: one SWAR mask marks the bytes that need an
    // escape, clean chunks are skipped and clean runs are copied whole.
    // Every escaped byte is ASCII, so slicing at its index is on a char
    // boundary; multi-byte chars are never split, whatever chunk they
    // straddle. A plain byte loop here was compiled up to 1.8x slower for
    // long strings in PGO builds (#52); the explicit scan does not depend on
    // the optimizer's unrolling decisions.
    let bytes = s.as_bytes();
    let mut start = 0;
    let mut i = 0;
    while i + 8 <= bytes.len() {
        let chunk = u64::from_le_bytes(bytes[i..i + 8].try_into().expect("8 bytes"));
        let mut mask = escape_mask(chunk);
        while mask != 0 {
            let at = i + (mask.trailing_zeros() / 8) as usize;
            buf.push_str(&s[start..at]);
            push_escape(buf, bytes[at]);
            start = at + 1;
            mask &= mask - 1;
        }
        i += 8;
    }
    for (j, &b) in bytes[i..].iter().enumerate() {
        if b >= 0x20 && b != b'"' && b != b'\\' {
            continue;
        }
        let at = i + j;
        buf.push_str(&s[start..at]);
        push_escape(buf, b);
        start = at + 1;
    }
    buf.push_str(&s[start..]);
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
    fn test_escape_runs_long_mixed() {
        // long safe runs around a few escapes: for the characters used here the
        // output must equal serde_json's (the writer emits \u0008 and \u000c where
        // serde_json writes \b and \f; neither occurs in this text)
        let mut text = String::new();
        for i in 0..200 {
            text.push_str("The quick brown fox jumps over the lazy dog, again and again ");
            if i % 7 == 0 {
                text.push('\n');
            }
            if i % 11 == 0 {
                text.push('"');
            }
            if i % 13 == 0 {
                text.push('\u{1f}');
            }
            if i % 17 == 0 {
                text.push_str("Zürich 日本語 \\ ");
            }
        }
        let mut w = JsonWriter::new();
        w.write_string(&text);
        let expected = serde_json::to_string(&text).unwrap();
        assert_eq!(w.into_string(), expected);
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

    #[test]
    fn test_allocation_free_writers() {
        let mut w = JsonWriter::new();
        w.write_hex_string(&[0x00, 0x0f, 0xf0, 0xff]);
        w.write_comma();
        w.write_base64_string(b"a\x00b");
        w.write_comma();
        w.write_class_path("mod.sub", "Cls\"q");
        w.write_comma();
        w.write_class_path("", "Bare");
        w.write_comma();
        w.write_ns_key(b"k\x00");
        assert_eq!(
            w.as_str(),
            "\"000ff0ff\",\"YQBi\",\"mod.sub.Cls\\\"q\",\"Bare\",\"@ns:awA=\":"
        );
    }

    fn reference_escape(s: &str) -> String {
        let mut out = String::new();
        for ch in s.chars() {
            match ch {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
                c => out.push(c),
            }
        }
        out
    }

    #[test]
    fn test_write_escaped_every_offset_matches_reference() {
        // an escape byte at every offset of strings up to 40 bytes: inside,
        // at and across the 8-byte chunk boundaries and in the short tail (#52)
        let escapes = [b'"', b'\\', b'\n', b'\r', b'\t', 0x00, 0x01, 0x1f];
        for len in 0..40 {
            for &esc in &escapes {
                for pos in 0..=len {
                    let mut bytes: Vec<u8> = (0..len).map(|i| b'a' + (i % 26) as u8).collect();
                    bytes.insert(pos, esc);
                    let s = String::from_utf8(bytes).unwrap();
                    let mut buf = String::new();
                    write_escaped(&mut buf, &s);
                    assert_eq!(buf, reference_escape(&s), "len {len} esc {esc:#x} pos {pos}");
                }
            }
        }
        // multi-byte chars around the 8-byte boundary, with and without an escape after them
        let euro9 = "\u{20ac}".repeat(9);
        let samples = [
            "abcdef\u{e9}\"".to_string(),
            "abcdef\u{20ac}\n".to_string(),
            "abcde\u{1f600}x\\y".to_string(),
            "\u{1f600}\u{1f600}\u{1f600}\"".to_string(),
            "\u{e9}".repeat(20),
            format!("{euro9}\t{euro9}"),
            String::new(),
            "1234567".to_string(),
            "12345678".to_string(),
            "\"\\\n\t\r\u{1}\u{1f}\"".to_string(),
            "a\"b\"c\"d\"e\"f\"g\"h\"i".to_string(),
            "\u{7f}\u{7f}\u{7f}\u{7f}\u{7f}\u{7f}\u{7f}\u{7f}\u{7f}".to_string(),
            "\u{a2}\u{22}\u{a2}\u{22}\u{a2}\u{22}\u{a2}\u{22}".to_string(),
        ];
        for s in samples {
            let mut buf = String::new();
            write_escaped(&mut buf, &s);
            assert_eq!(buf, reference_escape(&s), "{s:?}");
        }
    }
}

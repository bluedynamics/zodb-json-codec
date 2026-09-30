//! Protocol 0 text opcodes carry Python repr / raw-unicode-escape text.

use crate::error::CodecError;

/// Decode the body of a protocol 0 `STRING` argument (quotes already stripped):
/// the escape set of `codecs.escape_decode` / `PyBytes_DecodeEscape`.
/// Unknown escapes keep the backslash, like CPython.
pub fn unescape_string_repr(body: &[u8]) -> Result<Vec<u8>, CodecError> {
    let mut out = Vec::with_capacity(body.len());
    let mut i = 0;
    while i < body.len() {
        let b = body[i];
        i += 1;
        if b != b'\\' {
            out.push(b);
            continue;
        }
        let Some(&e) = body.get(i) else {
            return Err(CodecError::InvalidData("STRING: trailing backslash".to_string()));
        };
        i += 1;
        match e {
            b'\\' => out.push(b'\\'),
            b'\'' => out.push(b'\''),
            b'"' => out.push(b'"'),
            b'a' => out.push(0x07),
            b'b' => out.push(0x08),
            b'f' => out.push(0x0c),
            b'n' => out.push(b'\n'),
            b'r' => out.push(b'\r'),
            b't' => out.push(b'\t'),
            b'v' => out.push(0x0b),
            b'x' => {
                let hex = body
                    .get(i..i + 2)
                    .filter(|h| h.iter().all(u8::is_ascii_hexdigit))
                    .ok_or_else(|| {
                        CodecError::InvalidData(format!("STRING: invalid \\x escape at byte {}", i - 2))
                    })?;
                out.push(u8::from_str_radix(std::str::from_utf8(hex).unwrap(), 16).unwrap());
                i += 2;
            }
            b'0'..=b'7' => {
                let mut value = u32::from(e - b'0');
                let mut digits = 1;
                while digits < 3 && i < body.len() && (b'0'..=b'7').contains(&body[i]) {
                    value = value * 8 + u32::from(body[i] - b'0');
                    i += 1;
                    digits += 1;
                }
                out.push((value & 0xff) as u8);
            }
            other => {
                out.push(b'\\');
                out.push(other);
            }
        }
    }
    Ok(out)
}

/// Decode a protocol 0 `UNICODE` argument: raw-unicode-escape, i.e. Latin-1 bytes
/// with `\uXXXX` and `\UXXXXXXXX` escapes. Like CPython's decoder, backslashes
/// are copied in pairs: only an odd run of them followed by `u` or `U` is an
/// escape, every other backslash is literal. (CPython's pickler writes a
/// backslash as `\u005c`, so real streams never contain a bare one.)
pub fn decode_raw_unicode_escape(body: &[u8]) -> Result<String, CodecError> {
    let mut out = String::with_capacity(body.len());
    let mut i = 0;
    while i < body.len() {
        if body[i] != b'\\' {
            out.push(char::from(body[i])); // Latin-1: every byte is one char
            i += 1;
            continue;
        }
        let run = body[i..].iter().take_while(|&&b| b == b'\\').count();
        let width = match (run % 2 == 1, body.get(i + run)) {
            (true, Some(b'u')) => 4,
            (true, Some(b'U')) => 8,
            _ => {
                out.extend(std::iter::repeat_n('\\', run));
                i += run;
                continue;
            }
        };
        out.extend(std::iter::repeat_n('\\', run - 1));
        let escape_at = i + run - 1;
        let start = i + run + 1;
        let hex = body
            .get(start..start + width)
            .filter(|h| h.iter().all(u8::is_ascii_hexdigit))
            .ok_or_else(|| {
                CodecError::InvalidData(format!(
                    "UNICODE: truncated \\{} escape at byte {escape_at}",
                    body[i + run] as char
                ))
            })?;
        let code = u32::from_str_radix(std::str::from_utf8(hex).unwrap(), 16).unwrap();
        let ch = char::from_u32(code).ok_or_else(|| {
            CodecError::InvalidData(format!("UNICODE: U+{code:04X} is not a valid scalar value"))
        })?;
        out.push(ch);
        i = start + width;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn string_repr_escapes() {
        assert_eq!(unescape_string_repr(br"it\'s").unwrap(), b"it's");
        assert_eq!(
            unescape_string_repr(br#"a\x00b\xff\n\t\\\"\r\a\b\f\v"#).unwrap(),
            b"a\x00b\xff\n\t\\\"\r\x07\x08\x0c\x0b"
        );
        // octal: one to three digits, then a literal digit
        assert_eq!(unescape_string_repr(br"\101\7\0778").unwrap(), b"A\x07?8");
        // unknown escape keeps the backslash, like CPython
        assert_eq!(unescape_string_repr(br"\q").unwrap(), br"\q");
        assert!(unescape_string_repr(br"abc\").is_err());
        assert!(unescape_string_repr(br"\xZZ").is_err());
        assert!(unescape_string_repr(br"\x4").is_err());
    }

    #[test]
    fn raw_unicode_escape() {
        assert_eq!(
            decode_raw_unicode_escape(b"\xe9 \\u2603 \\U0001F600 \\u005c").unwrap(),
            "\u{e9} \u{2603} \u{1F600} \\"
        );
        // only \u and \U are escapes
        assert_eq!(decode_raw_unicode_escape(b"a\\nb\\").unwrap(), "a\\nb\\");
        // CPython: an even run of backslashes before `u` is literal, an odd run escapes
        assert_eq!(decode_raw_unicode_escape(b"\\\\u0041").unwrap(), "\\\\u0041");
        assert_eq!(decode_raw_unicode_escape(b"\\\\\\u0041").unwrap(), "\\\\A");
        assert_eq!(decode_raw_unicode_escape(b"\\\\u0d").unwrap(), "\\\\u0d");
        assert!(decode_raw_unicode_escape(b"\\u12").is_err());
        assert!(decode_raw_unicode_escape(b"\\ud800").is_err());
    }
}

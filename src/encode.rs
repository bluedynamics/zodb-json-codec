use crate::error::CodecError;
use crate::opcodes::*;
use crate::types::PickleValue;

/// Encode a PickleValue AST into pickle bytes (protocol 2).
pub fn encode_pickle(val: &PickleValue) -> Result<Vec<u8>, CodecError> {
    let mut encoder = Encoder::new();
    encoder.write_u8(PROTO);
    encoder.write_u8(2); // protocol 2
    encoder.encode_value(val)?;
    encoder.write_u8(STOP);
    Ok(encoder.buf)
}

/// Encode a PickleValue into an existing buffer (without PROTO/STOP framing).
/// Used as a fallback when the direct PyObject→pickle encoder encounters
/// complex types that need the PickleValue intermediate representation.
pub fn encode_value_into(val: &PickleValue, buf: &mut Vec<u8>) -> Result<(), CodecError> {
    let mut encoder = Encoder {
        buf: std::mem::take(buf),
    };
    encoder.encode_value(val)?;
    *buf = encoder.buf;
    Ok(())
}

// --- Public inline helpers for direct pickle writing ---
// Used by pyconv.rs to write pickle opcodes without PickleValue intermediates.

#[inline]
pub fn write_string(buf: &mut Vec<u8>, s: &str) {
    let bytes = s.as_bytes();
    let n = bytes.len();
    if n < 256 {
        buf.push(SHORT_BINUNICODE);
        buf.push(n as u8);
    } else {
        buf.push(BINUNICODE);
        buf.extend_from_slice(&(n as u32).to_le_bytes());
    }
    buf.extend_from_slice(bytes);
}

#[inline]
pub fn write_int(buf: &mut Vec<u8>, val: i64) {
    if val >= 0 && val < 256 {
        buf.push(BININT1);
        buf.push(val as u8);
    } else if val >= 0 && val < 65536 {
        buf.push(BININT2);
        buf.extend_from_slice(&(val as u16).to_le_bytes());
    } else if val >= i32::MIN as i64 && val <= i32::MAX as i64 {
        buf.push(BININT);
        buf.extend_from_slice(&(val as i32).to_le_bytes());
    } else {
        let bi = num_bigint::BigInt::from(val);
        let bytes = bi.to_signed_bytes_le();
        buf.push(LONG1);
        buf.push(bytes.len() as u8);
        buf.extend_from_slice(&bytes);
    }
}

#[inline]
pub fn write_bytes_val(buf: &mut Vec<u8>, data: &[u8]) {
    let n = data.len();
    if n < 256 {
        buf.push(SHORT_BINBYTES);
        buf.push(n as u8);
    } else {
        buf.push(BINBYTES);
        buf.extend_from_slice(&(n as u32).to_le_bytes());
    }
    buf.extend_from_slice(data);
}

#[inline]
pub fn write_global(buf: &mut Vec<u8>, module: &str, name: &str) {
    buf.push(GLOBAL);
    buf.extend_from_slice(module.as_bytes());
    buf.push(b'\n');
    buf.extend_from_slice(name.as_bytes());
    buf.push(b'\n');
}

struct Encoder {
    buf: Vec<u8>,
}

impl Encoder {
    fn new() -> Self {
        Self {
            buf: Vec::with_capacity(256),
        }
    }

    #[inline]
    fn write_u8(&mut self, b: u8) {
        self.buf.push(b);
    }

    #[inline]
    fn write_bytes(&mut self, data: &[u8]) {
        self.buf.extend_from_slice(data);
    }

    fn encode_value(&mut self, val: &PickleValue) -> Result<(), CodecError> {
        match val {
            PickleValue::None => {
                self.write_u8(NONE);
            }
            PickleValue::Bool(true) => {
                self.write_u8(NEWTRUE);
            }
            PickleValue::Bool(false) => {
                self.write_u8(NEWFALSE);
            }
            PickleValue::Int(i) => {
                self.encode_int(*i);
            }
            PickleValue::BigInt(bi) => {
                let bytes = bi.to_signed_bytes_le();
                let n = bytes.len();
                if n < 256 {
                    self.write_u8(LONG1);
                    self.write_u8(n as u8);
                } else {
                    self.write_u8(LONG4);
                    self.write_bytes(&(n as i32).to_le_bytes());
                }
                self.write_bytes(&bytes);
            }
            PickleValue::Float(f) => {
                self.write_u8(BINFLOAT);
                self.write_bytes(&f.to_be_bytes());
            }
            PickleValue::String(s) => {
                let bytes = s.as_bytes();
                let n = bytes.len();
                if n < 256 {
                    self.write_u8(SHORT_BINUNICODE);
                    self.write_u8(n as u8);
                } else {
                    self.write_u8(BINUNICODE);
                    self.write_bytes(&(n as u32).to_le_bytes());
                }
                self.write_bytes(bytes);
            }
            PickleValue::Bytes(b) => {
                let n = b.len();
                if n < 256 {
                    self.write_u8(SHORT_BINBYTES);
                    self.write_u8(n as u8);
                } else {
                    self.write_u8(BINBYTES);
                    self.write_bytes(&(n as u32).to_le_bytes());
                }
                self.write_bytes(b);
            }
            PickleValue::List(items) => {
                self.write_u8(EMPTY_LIST);
                if !items.is_empty() {
                    self.write_u8(MARK);
                    for item in items {
                        self.encode_value(item)?;
                    }
                    self.write_u8(APPENDS);
                }
            }
            PickleValue::Tuple(items) => {
                match items.len() {
                    0 => self.write_u8(EMPTY_TUPLE),
                    1 => {
                        self.encode_value(&items[0])?;
                        self.write_u8(TUPLE1);
                    }
                    2 => {
                        self.encode_value(&items[0])?;
                        self.encode_value(&items[1])?;
                        self.write_u8(TUPLE2);
                    }
                    3 => {
                        self.encode_value(&items[0])?;
                        self.encode_value(&items[1])?;
                        self.encode_value(&items[2])?;
                        self.write_u8(TUPLE3);
                    }
                    _ => {
                        self.write_u8(MARK);
                        for item in items {
                            self.encode_value(item)?;
                        }
                        self.write_u8(TUPLE);
                    }
                }
            }
            PickleValue::Dict(pairs) => {
                self.write_u8(EMPTY_DICT);
                if !pairs.is_empty() {
                    self.write_u8(MARK);
                    for (k, v) in pairs {
                        self.encode_value(k)?;
                        self.encode_value(v)?;
                    }
                    self.write_u8(SETITEMS);
                }
            }
            PickleValue::Set(items) => {
                // Protocol 4 set. For protocol 2 compat, use REDUCE with builtins.set
                self.write_u8(EMPTY_SET);
                if !items.is_empty() {
                    self.write_u8(MARK);
                    for item in items {
                        self.encode_value(item)?;
                    }
                    self.write_u8(ADDITEMS);
                }
            }
            PickleValue::FrozenSet(items) => {
                self.write_u8(MARK);
                for item in items {
                    self.encode_value(item)?;
                }
                self.write_u8(FROZENSET);
            }
            PickleValue::Global { module, name } => {
                self.write_u8(GLOBAL);
                self.write_bytes(module.as_bytes());
                self.write_u8(b'\n');
                self.write_bytes(name.as_bytes());
                self.write_u8(b'\n');
            }
            PickleValue::Instance { module, name, state } => {
                // Emit as: GLOBAL module\nname\n EMPTY_TUPLE NEWOBJ BUILD
                // This is the standard ZODB pattern.
                self.write_u8(GLOBAL);
                self.write_bytes(module.as_bytes());
                self.write_u8(b'\n');
                self.write_bytes(name.as_bytes());
                self.write_u8(b'\n');
                self.write_u8(EMPTY_TUPLE);
                self.write_u8(NEWOBJ);
                self.encode_value(state)?;
                self.write_u8(BUILD);
            }
            PickleValue::PersistentRef(inner) => {
                self.encode_value(inner)?;
                self.write_u8(BINPERSID);
            }
            PickleValue::Reduce { callable, args } => {
                self.encode_value(callable)?;
                self.encode_value(args)?;
                self.write_u8(REDUCE);
            }
            PickleValue::RawPickle(data) => {
                // Raw pickle bytes are already valid pickle — but we can't
                // just splice them in since they include PROTO/STOP.
                // For now, encode as bytes with a marker.
                // In practice this should be rare.
                self.write_u8(SHORT_BINBYTES);
                let n = data.len();
                if n < 256 {
                    self.write_u8(n as u8);
                    self.write_bytes(data);
                } else {
                    // Switch to BINBYTES
                    *self.buf.last_mut().unwrap() = BINBYTES;
                    self.write_bytes(&(n as u32).to_le_bytes());
                    self.write_bytes(data);
                }
            }
        }
        Ok(())
    }

    #[inline]
    fn encode_int(&mut self, val: i64) {
        if val >= 0 && val < 256 {
            self.write_u8(BININT1);
            self.write_u8(val as u8);
        } else if val >= 0 && val < 65536 {
            self.write_u8(BININT2);
            self.write_bytes(&(val as u16).to_le_bytes());
        } else if val >= i32::MIN as i64 && val <= i32::MAX as i64 {
            self.write_u8(BININT);
            self.write_bytes(&(val as i32).to_le_bytes());
        } else {
            // Use LONG1 for larger values
            let bi = num_bigint::BigInt::from(val);
            let bytes = bi.to_signed_bytes_le();
            self.write_u8(LONG1);
            self.write_u8(bytes.len() as u8);
            self.write_bytes(&bytes);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decode::decode_pickle;

    #[test]
    fn test_roundtrip_none() {
        let val = PickleValue::None;
        let bytes = encode_pickle(&val).unwrap();
        let decoded = decode_pickle(&bytes).unwrap();
        assert_eq!(val, decoded);
    }

    #[test]
    fn test_roundtrip_bool() {
        for b in [true, false] {
            let val = PickleValue::Bool(b);
            let bytes = encode_pickle(&val).unwrap();
            let decoded = decode_pickle(&bytes).unwrap();
            assert_eq!(val, decoded);
        }
    }

    #[test]
    fn test_roundtrip_ints() {
        for i in [0i64, 1, 42, 255, 256, 65535, 65536, -1, -128, i32::MAX as i64, i32::MIN as i64]
        {
            let val = PickleValue::Int(i);
            let bytes = encode_pickle(&val).unwrap();
            let decoded = decode_pickle(&bytes).unwrap();
            assert_eq!(val, decoded, "failed for i={i}");
        }
    }

    #[test]
    fn test_roundtrip_float() {
        let val = PickleValue::Float(3.14);
        let bytes = encode_pickle(&val).unwrap();
        let decoded = decode_pickle(&bytes).unwrap();
        assert_eq!(val, decoded);
    }

    #[test]
    fn test_roundtrip_string() {
        let val = PickleValue::String("hello world".to_string());
        let bytes = encode_pickle(&val).unwrap();
        let decoded = decode_pickle(&bytes).unwrap();
        assert_eq!(val, decoded);
    }

    #[test]
    fn test_roundtrip_bytes() {
        let val = PickleValue::Bytes(vec![0, 1, 2, 255]);
        let bytes = encode_pickle(&val).unwrap();
        let decoded = decode_pickle(&bytes).unwrap();
        assert_eq!(val, decoded);
    }

    #[test]
    fn test_roundtrip_list() {
        let val = PickleValue::List(vec![
            PickleValue::Int(1),
            PickleValue::String("two".to_string()),
            PickleValue::None,
        ]);
        let bytes = encode_pickle(&val).unwrap();
        let decoded = decode_pickle(&bytes).unwrap();
        assert_eq!(val, decoded);
    }

    #[test]
    fn test_roundtrip_dict() {
        let val = PickleValue::Dict(vec![
            (PickleValue::String("a".to_string()), PickleValue::Int(1)),
            (PickleValue::String("b".to_string()), PickleValue::Float(2.5)),
        ]);
        let bytes = encode_pickle(&val).unwrap();
        let decoded = decode_pickle(&bytes).unwrap();
        assert_eq!(val, decoded);
    }

    #[test]
    fn test_roundtrip_nested() {
        let val = PickleValue::Dict(vec![(
            PickleValue::String("items".to_string()),
            PickleValue::List(vec![
                PickleValue::Tuple(vec![PickleValue::Int(1), PickleValue::Int(2)]),
                PickleValue::Dict(vec![]),
            ]),
        )]);
        let bytes = encode_pickle(&val).unwrap();
        let decoded = decode_pickle(&bytes).unwrap();
        assert_eq!(val, decoded);
    }

    #[test]
    fn test_roundtrip_tuple_sizes() {
        for n in 0..=5 {
            let items: Vec<PickleValue> = (0..n).map(|i| PickleValue::Int(i)).collect();
            let val = PickleValue::Tuple(items);
            let bytes = encode_pickle(&val).unwrap();
            let decoded = decode_pickle(&bytes).unwrap();
            assert_eq!(val, decoded, "failed for tuple size {n}");
        }
    }
}

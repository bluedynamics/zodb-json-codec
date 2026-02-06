use crate::error::CodecError;
use crate::opcodes::*;
use crate::types::PickleValue;
use num_bigint::BigInt;

/// A marker value used on the stack to delimit sequences (MARK opcode).
#[derive(Debug, Clone)]
enum StackItem {
    Mark,
    Value(PickleValue),
}

/// Decode pickle bytes into a PickleValue AST.
///
/// This implements a subset of the pickle virtual machine sufficient
/// for ZODB records (protocol 2-3, with some protocol 4 support).
/// No Python objects are constructed — only our intermediate AST.
pub fn decode_pickle(data: &[u8]) -> Result<PickleValue, CodecError> {
    let mut decoder = Decoder::new(data);
    decoder.run()
}

struct Decoder<'a> {
    data: &'a [u8],
    pos: usize,
    stack: Vec<StackItem>,
    memo: Vec<PickleValue>,
    /// Metastack for MARK-based operations
    metastack: Vec<Vec<StackItem>>,
}

impl<'a> Decoder<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            pos: 0,
            stack: Vec::new(),
            memo: Vec::new(),
            metastack: Vec::new(),
        }
    }

    fn run(&mut self) -> Result<PickleValue, CodecError> {
        loop {
            let op = self.read_u8()?;
            match op {
                STOP => {
                    return self.pop_value();
                }
                PROTO => {
                    // Skip protocol byte
                    self.read_u8()?;
                }
                FRAME => {
                    // Protocol 4 framing: skip 8-byte frame length
                    self.read_bytes(8)?;
                }

                // -- None, Bool --
                NONE => self.push(PickleValue::None),
                NEWTRUE => self.push(PickleValue::Bool(true)),
                NEWFALSE => self.push(PickleValue::Bool(false)),

                // -- Integers --
                BININT => {
                    let val = self.read_i32()?;
                    self.push(PickleValue::Int(val as i64));
                }
                BININT1 => {
                    let val = self.read_u8()?;
                    self.push(PickleValue::Int(val as i64));
                }
                BININT2 => {
                    let val = self.read_u16()?;
                    self.push(PickleValue::Int(val as i64));
                }
                INT => {
                    let line = self.read_line()?;
                    let s = std::str::from_utf8(line).map_err(|_| CodecError::InvalidUtf8)?;
                    let s = s.trim();
                    // INT can encode booleans too: "00" = False, "01" = True
                    if s == "00" {
                        self.push(PickleValue::Bool(false));
                    } else if s == "01" {
                        self.push(PickleValue::Bool(true));
                    } else {
                        let val: i64 = s
                            .parse()
                            .map_err(|e| CodecError::InvalidData(format!("INT parse: {e}")))?;
                        self.push(PickleValue::Int(val));
                    }
                }
                LONG => {
                    let line = self.read_line()?;
                    let s = std::str::from_utf8(line).map_err(|_| CodecError::InvalidUtf8)?;
                    let s = s.trim().trim_end_matches('L');
                    let val: BigInt = s
                        .parse()
                        .map_err(|e| CodecError::InvalidData(format!("LONG parse: {e}")))?;
                    // Try to fit in i64 first
                    if let Ok(v) = i64::try_from(&val) {
                        self.push(PickleValue::Int(v));
                    } else {
                        self.push(PickleValue::BigInt(val));
                    }
                }
                LONG1 => {
                    let n = self.read_u8()? as usize;
                    let bytes = self.read_bytes(n)?;
                    let val = BigInt::from_signed_bytes_le(bytes);
                    if let Ok(v) = i64::try_from(&val) {
                        self.push(PickleValue::Int(v));
                    } else {
                        self.push(PickleValue::BigInt(val));
                    }
                }
                LONG4 => {
                    let n = self.read_i32()? as usize;
                    let bytes = self.read_bytes(n)?;
                    let val = BigInt::from_signed_bytes_le(bytes);
                    if let Ok(v) = i64::try_from(&val) {
                        self.push(PickleValue::Int(v));
                    } else {
                        self.push(PickleValue::BigInt(val));
                    }
                }

                // -- Float --
                BINFLOAT => {
                    let bytes = self.read_bytes(8)?;
                    let val = f64::from_be_bytes(bytes.try_into().unwrap());
                    self.push(PickleValue::Float(val));
                }
                FLOAT => {
                    let line = self.read_line()?;
                    let s = std::str::from_utf8(line).map_err(|_| CodecError::InvalidUtf8)?;
                    let val: f64 = s
                        .trim()
                        .parse()
                        .map_err(|e| CodecError::InvalidData(format!("FLOAT parse: {e}")))?;
                    self.push(PickleValue::Float(val));
                }

                // -- Strings (Python 2 str / bytes) --
                BINSTRING => {
                    let n = self.read_i32()? as usize;
                    let bytes = self.read_bytes(n)?.to_vec();
                    self.push(PickleValue::Bytes(bytes));
                }
                SHORT_BINSTRING => {
                    let n = self.read_u8()? as usize;
                    let bytes = self.read_bytes(n)?.to_vec();
                    self.push(PickleValue::Bytes(bytes));
                }
                STRING => {
                    let line = self.read_line()?;
                    let s = std::str::from_utf8(line).map_err(|_| CodecError::InvalidUtf8)?;
                    let s = s.trim();
                    // STRING values are repr'd: strip quotes
                    let inner = if (s.starts_with('\'') && s.ends_with('\''))
                        || (s.starts_with('"') && s.ends_with('"'))
                    {
                        &s[1..s.len() - 1]
                    } else {
                        s
                    };
                    self.push(PickleValue::Bytes(inner.as_bytes().to_vec()));
                }

                // -- Unicode strings --
                BINUNICODE => {
                    let n = self.read_u32()? as usize;
                    let bytes = self.read_bytes(n)?;
                    let s =
                        std::str::from_utf8(bytes).map_err(|_| CodecError::InvalidUtf8)?;
                    self.push(PickleValue::String(s.to_string()));
                }
                SHORT_BINUNICODE => {
                    let n = self.read_u8()? as usize;
                    let bytes = self.read_bytes(n)?;
                    let s =
                        std::str::from_utf8(bytes).map_err(|_| CodecError::InvalidUtf8)?;
                    self.push(PickleValue::String(s.to_string()));
                }
                UNICODE => {
                    let line = self.read_line()?;
                    let s = std::str::from_utf8(line).map_err(|_| CodecError::InvalidUtf8)?;
                    self.push(PickleValue::String(s.to_string()));
                }
                BINUNICODE8 => {
                    let n = self.read_u64()? as usize;
                    let bytes = self.read_bytes(n)?;
                    let s =
                        std::str::from_utf8(bytes).map_err(|_| CodecError::InvalidUtf8)?;
                    self.push(PickleValue::String(s.to_string()));
                }

                // -- Bytes --
                BINBYTES => {
                    let n = self.read_u32()? as usize;
                    let bytes = self.read_bytes(n)?.to_vec();
                    self.push(PickleValue::Bytes(bytes));
                }
                SHORT_BINBYTES => {
                    let n = self.read_u8()? as usize;
                    let bytes = self.read_bytes(n)?.to_vec();
                    self.push(PickleValue::Bytes(bytes));
                }
                BINBYTES8 => {
                    let n = self.read_u64()? as usize;
                    let bytes = self.read_bytes(n)?.to_vec();
                    self.push(PickleValue::Bytes(bytes));
                }

                // -- Mark --
                MARK => {
                    // Save current stack, start a new one
                    let old_stack = std::mem::take(&mut self.stack);
                    self.metastack.push(old_stack);
                    // Don't push Mark itself; everything above the mark
                    // is captured by the current stack being empty
                }

                // -- Tuple --
                EMPTY_TUPLE => self.push(PickleValue::Tuple(Vec::new())),
                TUPLE => {
                    let items = self.pop_mark()?;
                    self.push(PickleValue::Tuple(items));
                }
                TUPLE1 => {
                    let a = self.pop_value()?;
                    self.push(PickleValue::Tuple(vec![a]));
                }
                TUPLE2 => {
                    let b = self.pop_value()?;
                    let a = self.pop_value()?;
                    self.push(PickleValue::Tuple(vec![a, b]));
                }
                TUPLE3 => {
                    let c = self.pop_value()?;
                    let b = self.pop_value()?;
                    let a = self.pop_value()?;
                    self.push(PickleValue::Tuple(vec![a, b, c]));
                }

                // -- List --
                EMPTY_LIST => self.push(PickleValue::List(Vec::new())),
                LIST => {
                    let items = self.pop_mark()?;
                    self.push(PickleValue::List(items));
                }
                APPEND => {
                    let val = self.pop_value()?;
                    let list = self.top_value_mut()?;
                    if let PickleValue::List(ref mut items) = list {
                        items.push(val);
                    } else {
                        return Err(CodecError::InvalidData(
                            "APPEND on non-list".to_string(),
                        ));
                    }
                }
                APPENDS => {
                    let items = self.pop_mark()?;
                    let list = self.top_value_mut()?;
                    if let PickleValue::List(ref mut list_items) = list {
                        list_items.extend(items);
                    } else {
                        return Err(CodecError::InvalidData(
                            "APPENDS on non-list".to_string(),
                        ));
                    }
                }

                // -- Dict --
                EMPTY_DICT => self.push(PickleValue::Dict(Vec::new())),
                DICT => {
                    let items = self.pop_mark()?;
                    let pairs = items_to_pairs(items)?;
                    self.push(PickleValue::Dict(pairs));
                }
                SETITEM => {
                    let val = self.pop_value()?;
                    let key = self.pop_value()?;
                    let dict = self.top_value_mut()?;
                    if let PickleValue::Dict(ref mut pairs) = dict {
                        pairs.push((key, val));
                    } else {
                        return Err(CodecError::InvalidData(
                            "SETITEM on non-dict".to_string(),
                        ));
                    }
                }
                SETITEMS => {
                    let items = self.pop_mark()?;
                    let new_pairs = items_to_pairs(items)?;
                    let dict = self.top_value_mut()?;
                    if let PickleValue::Dict(ref mut pairs) = dict {
                        pairs.extend(new_pairs);
                    } else {
                        return Err(CodecError::InvalidData(
                            "SETITEMS on non-dict".to_string(),
                        ));
                    }
                }

                // -- Set/FrozenSet (protocol 4) --
                EMPTY_SET => self.push(PickleValue::Set(Vec::new())),
                ADDITEMS => {
                    let items = self.pop_mark()?;
                    let set = self.top_value_mut()?;
                    if let PickleValue::Set(ref mut set_items) = set {
                        set_items.extend(items);
                    } else {
                        return Err(CodecError::InvalidData(
                            "ADDITEMS on non-set".to_string(),
                        ));
                    }
                }
                FROZENSET => {
                    let items = self.pop_mark()?;
                    self.push(PickleValue::FrozenSet(items));
                }

                // -- Global (class reference) --
                GLOBAL => {
                    let module_line = self.read_line()?;
                    let name_line = self.read_line()?;
                    let module = std::str::from_utf8(module_line)
                        .map_err(|_| CodecError::InvalidUtf8)?
                        .to_string();
                    let name = std::str::from_utf8(name_line)
                        .map_err(|_| CodecError::InvalidUtf8)?
                        .to_string();
                    self.push(PickleValue::Global { module, name });
                }
                STACK_GLOBAL => {
                    let name_val = self.pop_value()?;
                    let module_val = self.pop_value()?;
                    let module = match module_val {
                        PickleValue::String(s) => s,
                        _ => {
                            return Err(CodecError::InvalidData(
                                "STACK_GLOBAL: module is not a string".to_string(),
                            ))
                        }
                    };
                    let name = match name_val {
                        PickleValue::String(s) => s,
                        _ => {
                            return Err(CodecError::InvalidData(
                                "STACK_GLOBAL: name is not a string".to_string(),
                            ))
                        }
                    };
                    self.push(PickleValue::Global { module, name });
                }

                // -- Object construction --
                REDUCE => {
                    let args = self.pop_value()?;
                    let callable = self.pop_value()?;
                    // Recognize set/frozenset REDUCE pattern (protocol 3)
                    match (&callable, &args) {
                        (
                            PickleValue::Global { module, name },
                            PickleValue::Tuple(tuple_items),
                        ) if module == "builtins"
                            && name == "set"
                            && tuple_items.len() == 1 =>
                        {
                            if let PickleValue::List(items) = &tuple_items[0] {
                                self.push(PickleValue::Set(items.clone()));
                            } else {
                                self.push(PickleValue::Reduce {
                                    callable: Box::new(callable),
                                    args: Box::new(args),
                                });
                            }
                        }
                        (
                            PickleValue::Global { module, name },
                            PickleValue::Tuple(tuple_items),
                        ) if module == "builtins"
                            && name == "frozenset"
                            && tuple_items.len() == 1 =>
                        {
                            if let PickleValue::List(items) = &tuple_items[0] {
                                self.push(PickleValue::FrozenSet(items.clone()));
                            } else {
                                self.push(PickleValue::Reduce {
                                    callable: Box::new(callable),
                                    args: Box::new(args),
                                });
                            }
                        }
                        _ => {
                            self.push(PickleValue::Reduce {
                                callable: Box::new(callable),
                                args: Box::new(args),
                            });
                        }
                    }
                }
                BUILD => {
                    let state = self.pop_value()?;
                    let obj = self.pop_value()?;
                    match obj {
                        PickleValue::Global { module, name } => {
                            self.push(PickleValue::Instance {
                                module,
                                name,
                                state: Box::new(state),
                            });
                        }
                        PickleValue::Instance {
                            module,
                            name,
                            state: _old_state,
                        } => {
                            // BUILD on an existing instance updates its state
                            self.push(PickleValue::Instance {
                                module,
                                name,
                                state: Box::new(state),
                            });
                        }
                        PickleValue::Reduce { callable, args } => {
                            // REDUCE followed by BUILD: the common pattern.
                            // Extract class info if callable is a Global.
                            match *callable {
                                PickleValue::Global { module, name } => {
                                    // Merge: state includes both constructor args and BUILD state
                                    let combined = if *args == PickleValue::Tuple(vec![]) {
                                        state
                                    } else {
                                        PickleValue::Dict(vec![
                                            (
                                                PickleValue::String("@args".to_string()),
                                                *args,
                                            ),
                                            (
                                                PickleValue::String("@state".to_string()),
                                                state,
                                            ),
                                        ])
                                    };
                                    self.push(PickleValue::Instance {
                                        module,
                                        name,
                                        state: Box::new(combined),
                                    });
                                }
                                _ => {
                                    // Can't decompose further — wrap as-is
                                    self.push(PickleValue::Instance {
                                        module: String::new(),
                                        name: String::new(),
                                        state: Box::new(PickleValue::Dict(vec![
                                            (
                                                PickleValue::String("@callable".to_string()),
                                                *callable,
                                            ),
                                            (
                                                PickleValue::String("@args".to_string()),
                                                *args,
                                            ),
                                            (
                                                PickleValue::String("@state".to_string()),
                                                state,
                                            ),
                                        ])),
                                    });
                                }
                            }
                        }
                        _ => {
                            // BUILD on something unexpected — keep both
                            self.push(PickleValue::Instance {
                                module: String::new(),
                                name: String::new(),
                                state: Box::new(PickleValue::Dict(vec![
                                    (PickleValue::String("@obj".to_string()), obj),
                                    (PickleValue::String("@state".to_string()), state),
                                ])),
                            });
                        }
                    }
                }
                NEWOBJ => {
                    let args = self.pop_value()?;
                    let cls = self.pop_value()?;
                    match cls {
                        PickleValue::Global { module, name } => {
                            self.push(PickleValue::Reduce {
                                callable: Box::new(PickleValue::Global { module, name }),
                                args: Box::new(args),
                            });
                        }
                        _ => {
                            self.push(PickleValue::Reduce {
                                callable: Box::new(cls),
                                args: Box::new(args),
                            });
                        }
                    }
                }
                NEWOBJ_EX => {
                    let kwargs = self.pop_value()?;
                    let args = self.pop_value()?;
                    let cls = self.pop_value()?;
                    // For now, combine args and kwargs
                    let combined_args = PickleValue::Dict(vec![
                        (PickleValue::String("@args".to_string()), args),
                        (PickleValue::String("@kwargs".to_string()), kwargs),
                    ]);
                    self.push(PickleValue::Reduce {
                        callable: Box::new(cls),
                        args: Box::new(combined_args),
                    });
                }

                // -- Persistent references (ZODB) --
                BINPERSID => {
                    let pid = self.pop_value()?;
                    self.push(PickleValue::PersistentRef(Box::new(pid)));
                }
                PERSID => {
                    let line = self.read_line()?;
                    let s = std::str::from_utf8(line)
                        .map_err(|_| CodecError::InvalidUtf8)?
                        .to_string();
                    self.push(PickleValue::PersistentRef(Box::new(
                        PickleValue::String(s),
                    )));
                }

                // -- Memo --
                BINPUT => {
                    let idx = self.read_u8()? as usize;
                    let val = self.peek_value()?.clone();
                    self.memo_put(idx, val);
                }
                LONG_BINPUT => {
                    let idx = self.read_u32()? as usize;
                    let val = self.peek_value()?.clone();
                    self.memo_put(idx, val);
                }
                MEMOIZE => {
                    let val = self.peek_value()?.clone();
                    let idx = self.memo.len();
                    self.memo_put(idx, val);
                }
                BINGET => {
                    let idx = self.read_u8()? as usize;
                    let val = self.memo_get(idx)?;
                    self.push(val);
                }
                LONG_BINGET => {
                    let idx = self.read_u32()? as usize;
                    let val = self.memo_get(idx)?;
                    self.push(val);
                }
                PUT => {
                    let line = self.read_line()?;
                    let s = std::str::from_utf8(line).map_err(|_| CodecError::InvalidUtf8)?;
                    let idx: usize = s
                        .trim()
                        .parse()
                        .map_err(|e| CodecError::InvalidData(format!("PUT index: {e}")))?;
                    let val = self.peek_value()?.clone();
                    self.memo_put(idx, val);
                }
                GET => {
                    let line = self.read_line()?;
                    let s = std::str::from_utf8(line).map_err(|_| CodecError::InvalidUtf8)?;
                    let idx: usize = s
                        .trim()
                        .parse()
                        .map_err(|e| CodecError::InvalidData(format!("GET index: {e}")))?;
                    let val = self.memo_get(idx)?;
                    self.push(val);
                }

                // -- Stack manipulation --
                POP => {
                    self.pop_value()?;
                }
                DUP => {
                    let val = self.peek_value()?.clone();
                    self.push(val);
                }

                _ => {
                    return Err(CodecError::UnknownOpcode(op));
                }
            }
        }
    }

    // -- Reading primitives --

    fn read_u8(&mut self) -> Result<u8, CodecError> {
        if self.pos >= self.data.len() {
            return Err(CodecError::UnexpectedEof);
        }
        let val = self.data[self.pos];
        self.pos += 1;
        Ok(val)
    }

    fn read_bytes(&mut self, n: usize) -> Result<&'a [u8], CodecError> {
        if self.pos + n > self.data.len() {
            return Err(CodecError::UnexpectedEof);
        }
        let slice = &self.data[self.pos..self.pos + n];
        self.pos += n;
        Ok(slice)
    }

    fn read_u16(&mut self) -> Result<u16, CodecError> {
        let bytes = self.read_bytes(2)?;
        Ok(u16::from_le_bytes(bytes.try_into().unwrap()))
    }

    fn read_i32(&mut self) -> Result<i32, CodecError> {
        let bytes = self.read_bytes(4)?;
        Ok(i32::from_le_bytes(bytes.try_into().unwrap()))
    }

    fn read_u32(&mut self) -> Result<u32, CodecError> {
        let bytes = self.read_bytes(4)?;
        Ok(u32::from_le_bytes(bytes.try_into().unwrap()))
    }

    fn read_u64(&mut self) -> Result<u64, CodecError> {
        let bytes = self.read_bytes(8)?;
        Ok(u64::from_le_bytes(bytes.try_into().unwrap()))
    }

    fn read_line(&mut self) -> Result<&'a [u8], CodecError> {
        let start = self.pos;
        while self.pos < self.data.len() {
            if self.data[self.pos] == b'\n' {
                let line = &self.data[start..self.pos];
                self.pos += 1; // skip newline
                return Ok(line);
            }
            self.pos += 1;
        }
        Err(CodecError::UnexpectedEof)
    }

    // -- Stack operations --

    fn push(&mut self, val: PickleValue) {
        self.stack.push(StackItem::Value(val));
    }

    fn pop_value(&mut self) -> Result<PickleValue, CodecError> {
        match self.stack.pop() {
            Some(StackItem::Value(v)) => Ok(v),
            Some(StackItem::Mark) => Err(CodecError::StackUnderflow),
            None => {
                // Check metastack
                if let Some(mut old_stack) = self.metastack.pop() {
                    std::mem::swap(&mut self.stack, &mut old_stack);
                    // old_stack now has what was in current stack (empty or mark items)
                    // This shouldn't happen normally
                    Err(CodecError::StackUnderflow)
                } else {
                    Err(CodecError::StackUnderflow)
                }
            }
        }
    }

    fn peek_value(&self) -> Result<&PickleValue, CodecError> {
        for item in self.stack.iter().rev() {
            if let StackItem::Value(v) = item {
                return Ok(v);
            }
        }
        Err(CodecError::StackUnderflow)
    }

    fn top_value_mut(&mut self) -> Result<&mut PickleValue, CodecError> {
        for item in self.stack.iter_mut().rev() {
            if let StackItem::Value(ref mut v) = item {
                return Ok(v);
            }
        }
        Err(CodecError::StackUnderflow)
    }

    /// Pop all items above the last MARK from the stack.
    fn pop_mark(&mut self) -> Result<Vec<PickleValue>, CodecError> {
        // Everything in self.stack since the last MARK push is our items
        let items: Vec<PickleValue> = self
            .stack
            .drain(..)
            .map(|item| match item {
                StackItem::Value(v) => v,
                StackItem::Mark => unreachable!("marks should not be on stack"),
            })
            .collect();

        // Restore the previous stack from metastack
        if let Some(old_stack) = self.metastack.pop() {
            self.stack = old_stack;
        }
        // else: stack is empty, which is fine for the first mark

        Ok(items)
    }

    // -- Memo operations --

    fn memo_put(&mut self, idx: usize, val: PickleValue) {
        if idx >= self.memo.len() {
            self.memo.resize(idx + 1, PickleValue::None);
        }
        self.memo[idx] = val;
    }

    fn memo_get(&self, idx: usize) -> Result<PickleValue, CodecError> {
        self.memo
            .get(idx)
            .cloned()
            .ok_or_else(|| CodecError::InvalidData(format!("memo index {idx} not found")))
    }
}

/// Convert a flat list [k1, v1, k2, v2, ...] into pairs [(k1, v1), (k2, v2), ...].
fn items_to_pairs(
    items: Vec<PickleValue>,
) -> Result<Vec<(PickleValue, PickleValue)>, CodecError> {
    if items.len() % 2 != 0 {
        return Err(CodecError::InvalidData(
            "odd number of items for dict".to_string(),
        ));
    }
    let mut pairs = Vec::with_capacity(items.len() / 2);
    let mut iter = items.into_iter();
    while let (Some(k), Some(v)) = (iter.next(), iter.next()) {
        pairs.push((k, v));
    }
    Ok(pairs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_decode_none() {
        // protocol 2: \x80\x02 N .
        let data = b"\x80\x02N.";
        let result = decode_pickle(data).unwrap();
        assert_eq!(result, PickleValue::None);
    }

    #[test]
    fn test_decode_bool() {
        let data = b"\x80\x02\x88."; // True
        assert_eq!(decode_pickle(data).unwrap(), PickleValue::Bool(true));

        let data = b"\x80\x02\x89."; // False
        assert_eq!(decode_pickle(data).unwrap(), PickleValue::Bool(false));
    }

    #[test]
    fn test_decode_int() {
        // BININT1: K\x2a = 42
        let data = b"\x80\x02K\x2a.";
        assert_eq!(decode_pickle(data).unwrap(), PickleValue::Int(42));
    }

    #[test]
    fn test_decode_string() {
        // SHORT_BINUNICODE: \x8c\x05hello
        let data = b"\x80\x02\x8c\x05hello.";
        assert_eq!(
            decode_pickle(data).unwrap(),
            PickleValue::String("hello".to_string())
        );
    }

    #[test]
    fn test_decode_empty_list() {
        let data = b"\x80\x02].";
        assert_eq!(decode_pickle(data).unwrap(), PickleValue::List(vec![]));
    }

    #[test]
    fn test_decode_empty_dict() {
        let data = b"\x80\x02}.";
        assert_eq!(decode_pickle(data).unwrap(), PickleValue::Dict(vec![]));
    }

    #[test]
    fn test_decode_empty_tuple() {
        let data = b"\x80\x02).";
        assert_eq!(decode_pickle(data).unwrap(), PickleValue::Tuple(vec![]));
    }

    #[test]
    fn test_decode_tuple1() {
        // TUPLE1: \x85
        let data = b"\x80\x02K\x01\x85.";
        assert_eq!(
            decode_pickle(data).unwrap(),
            PickleValue::Tuple(vec![PickleValue::Int(1)])
        );
    }

    #[test]
    fn test_decode_dict_with_items() {
        // {"\x8c\x01a": 1}  →  } \x8c\x01a K\x01 s .
        let data = b"\x80\x02}\x8c\x01aK\x01s.";
        let result = decode_pickle(data).unwrap();
        assert_eq!(
            result,
            PickleValue::Dict(vec![(
                PickleValue::String("a".to_string()),
                PickleValue::Int(1)
            )])
        );
    }
}

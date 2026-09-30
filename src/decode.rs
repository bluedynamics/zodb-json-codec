use crate::escape;
use crate::error::CodecError;
use crate::opcodes::*;
use crate::types::{InstanceData, PickleValue};
use num_bigint::BigInt;

const MAX_MEMO_SIZE: usize = 100_000;
/// Deepest value the decoder will build; deeper input is a crafted pickle.
const MAX_DEPTH: u32 = 1000;
const MAX_BINARY_SIZE: u64 = 256 * 1024 * 1024; // 256 MB

/// Decode pickle bytes into a PickleValue AST.
///
/// This implements a subset of the pickle virtual machine sufficient
/// for ZODB records (protocol 2-3, with some protocol 4 support).
/// No Python objects are constructed — only our intermediate AST.
pub fn decode_pickle(data: &[u8]) -> Result<PickleValue, CodecError> {
    let mut decoder = Decoder::new(data);
    decoder.run()
}

/// Decode a ZODB record (two concatenated pickles) with shared memo.
/// ZODB shares the pickler memo between the class and state pickles,
/// so state pickles can reference memo entries from the class pickle.
/// Returns (class_value, state_value).
pub fn decode_zodb_pickles(data: &[u8]) -> Result<(PickleValue, PickleValue), CodecError> {
    let mut decoder = Decoder::new(data);
    let class_val = decoder.run()?;
    // Continue with same memo — ZODB shares memo between both pickles
    let state_val = decoder.run()?;
    Ok((class_val, state_val))
}

/// The decoder's bookkeeping vectors, kept per thread and reused by every
/// record decoded on it (#26): after the first few records they never
/// allocate again. Vectors that grew past `MAX_SCRATCH_ELEMS` are dropped
/// instead of kept, so one huge record does not pin memory for the thread's
/// lifetime; the ceiling is about 8 MiB per thread (48-byte values in `stack`
/// and `memo`, 24-byte binding vectors, the rest small).
#[derive(Default)]
struct Scratch {
    stack: Vec<PickleValue>,
    /// `None` for an index that was never put (a put above it grows the
    /// vector); a `GET` of such an index is an error, as in CPython (#47).
    memo: Vec<Option<PickleValue>>,
    stack_memo: Vec<Vec<usize>>,
    marks: Vec<usize>,
    dirty_memo: Vec<bool>,
    depth: Vec<u32>,
    memo_depth: Vec<u32>,
}

const MAX_SCRATCH_ELEMS: usize = 1 << 16;

thread_local! {
    static SCRATCH: std::cell::RefCell<Scratch> = std::cell::RefCell::new(Scratch::default());
}

impl Scratch {
    fn take() -> Self {
        // a re-entrant decode on the same thread (impossible today: the decoder
        // runs no Python code) would simply get fresh vectors
        let mut scratch = SCRATCH
            .try_with(|cell| match cell.try_borrow_mut() {
                Ok(mut held) => std::mem::take(&mut *held),
                Err(_) => Scratch::default(),
            })
            .unwrap_or_default();
        scratch.stack.clear();
        scratch.memo.clear();
        scratch.stack_memo.clear();
        scratch.marks.clear();
        scratch.dirty_memo.clear();
        scratch.depth.clear();
        scratch.memo_depth.clear();
        scratch
    }

    fn give_back(mut self) {
        self.stack.clear();
        self.memo.clear();
        self.stack_memo.clear();
        self.marks.clear();
        self.dirty_memo.clear();
        self.depth.clear();
        self.memo_depth.clear();
        let oversized = self.stack.capacity() > MAX_SCRATCH_ELEMS
            || self.memo.capacity() > MAX_SCRATCH_ELEMS
            || self.stack_memo.capacity() > MAX_SCRATCH_ELEMS
            || self.dirty_memo.capacity() > MAX_SCRATCH_ELEMS
            || self.depth.capacity() > MAX_SCRATCH_ELEMS
            || self.memo_depth.capacity() > MAX_SCRATCH_ELEMS
            || self.marks.capacity() > MAX_SCRATCH_ELEMS;
        if oversized {
            return;
        }
        // try_with: never panic from Drop, even during thread-local destruction
        let _ = SCRATCH.try_with(|cell| {
            if let Ok(mut held) = cell.try_borrow_mut() {
                *held = self;
            }
        });
    }
}

struct Decoder<'a> {
    data: &'a [u8],
    pos: usize,
    stack: Vec<PickleValue>,
    memo: Vec<Option<PickleValue>>,
    /// Tracks which memo indices are bound to each stack slot (parallel to stack).
    /// When BINPUT stores from stack top, the memo index is recorded here.
    stack_memo: Vec<Vec<usize>>,
    /// Stack length at each open MARK: the values above the last one are
    /// the current "sub-stack" (#26; CPython swaps in a fresh stack per MARK).
    marks: Vec<usize>,
    /// Dirty flags parallel to memo: true means the memo entry is stale
    /// (the owning stack slot was mutated after BINPUT stored the value).
    /// Resolved lazily at BINGET or eagerly when the slot is popped.
    dirty_memo: Vec<bool>,
    /// Nesting depth of each stack slot (parallel to `stack`): 0 for scalars,
    /// 1 + deepest child for containers, including the dicts BUILD wraps around
    /// `@args`/`@state` (those may count one level more than the value has, never
    /// less). Lets container opcodes refuse a value deeper than MAX_DEPTH before
    /// it exists (its Drop/Clone would recurse).
    depth: Vec<u32>,
    /// Depth of each memo entry (parallel to memo).
    memo_depth: Vec<u32>,
    /// Which memo indices are ever read by a GET/BINGET/LONG_BINGET in this
    /// stream (pre-scanned). Puts to any other index are skipped entirely:
    /// no clone at BINPUT, no binding bookkeeping, no dirty sync at pop.
    memo_needed: MemoNeeds,
    /// Number of memo puts seen so far: MEMOIZE's auto-index (protocol 4).
    /// Equals CPython's `len(memo)` for every stream a pickler emits (no slot
    /// is put twice). Counted even when the put is skipped, so later indices
    /// stay aligned.
    memoize_count: usize,
}

/// Result of the memo-read pre-scan.
enum MemoNeeds {
    /// The scan could not follow the stream; keep every memo entry.
    All,
    /// Exactly these indices are read; anything else is never looked at again.
    Only(Vec<bool>),
}

impl Drop for Decoder<'_> {
    fn drop(&mut self) {
        Scratch {
            stack: std::mem::take(&mut self.stack),
            memo: std::mem::take(&mut self.memo),
            stack_memo: std::mem::take(&mut self.stack_memo),
            marks: std::mem::take(&mut self.marks),
            dirty_memo: std::mem::take(&mut self.dirty_memo),
            depth: std::mem::take(&mut self.depth),
            memo_depth: std::mem::take(&mut self.memo_depth),
        }
        .give_back();
    }
}

impl<'a> Decoder<'a> {
    fn new(data: &'a [u8]) -> Self {
        let scratch = Scratch::take();
        Self {
            data,
            pos: 0,
            stack: scratch.stack,
            memo: scratch.memo,
            stack_memo: scratch.stack_memo,
            marks: scratch.marks,
            dirty_memo: scratch.dirty_memo,
            depth: scratch.depth,
            memo_depth: scratch.memo_depth,
            memo_needed: scan_memo_reads(data),
            memoize_count: 0,
        }
    }

    /// Depth of a container whose deepest child has depth `child_max`.
    #[inline]
    fn nest(child_max: u32) -> Result<u32, CodecError> {
        if child_max >= MAX_DEPTH {
            return Err(CodecError::InvalidData(
                "maximum nesting depth exceeded".to_string(),
            ));
        }
        Ok(child_max + 1)
    }

    fn run(&mut self) -> Result<PickleValue, CodecError> {
        loop {
            let op = self.read_u8()?;
            match op {
                STOP => {
                    let value = self.pop_value()?;
                    self.marks.clear();
                    return Ok(value);
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
                    if s.len() > 10_000 {
                        return Err(CodecError::InvalidData("LONG value too large".to_string()));
                    }
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
                    if n == 0 {
                        self.push(PickleValue::Int(0));
                    } else if n <= 8 {
                        // sign-extend the little-endian two's complement into i64
                        // without going through BigInt (#26)
                        let mut buf = [0u8; 8];
                        buf[..n].copy_from_slice(bytes);
                        let shift = 64 - 8 * n as u32;
                        let v = (i64::from_le_bytes(buf) << shift) >> shift;
                        self.push(PickleValue::Int(v));
                    } else {
                        let val = BigInt::from_signed_bytes_le(bytes);
                        match i64::try_from(&val) {
                            Ok(v) => self.push(PickleValue::Int(v)),
                            Err(_) => self.push(PickleValue::BigInt(val)),
                        }
                    }
                }
                LONG4 => {
                    let n = self.read_i32()?;
                    if n < 0 {
                        return Err(CodecError::InvalidData("negative length in LONG4".to_string()));
                    }
                    let n = n as usize;
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
                    let n = self.read_i32()?;
                    if n < 0 {
                        return Err(CodecError::InvalidData("negative length in BINSTRING".to_string()));
                    }
                    let n = n as usize;
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
                    // CPython requires matching quotes around a bytes repr
                    let quoted = line.len() >= 2
                        && line[0] == line[line.len() - 1]
                        && (line[0] == b'\'' || line[0] == b'"');
                    if !quoted {
                        return Err(CodecError::InvalidData(
                            "STRING opcode argument must be quoted".to_string(),
                        ));
                    }
                    let body = escape::unescape_string_repr(&line[1..line.len() - 1])?;
                    self.push(PickleValue::Bytes(body));
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
                    self.push(PickleValue::String(escape::decode_raw_unicode_escape(line)?));
                }
                BINUNICODE8 => {
                    let n = self.read_u64()?;
                    if n > MAX_BINARY_SIZE {
                        return Err(CodecError::InvalidData("BINUNICODE8 data too large".to_string()));
                    }
                    let n = n as usize;
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
                    let n = self.read_u64()?;
                    if n > MAX_BINARY_SIZE {
                        return Err(CodecError::InvalidData("BINBYTES8 data too large".to_string()));
                    }
                    let n = n as usize;
                    let bytes = self.read_bytes(n)?.to_vec();
                    self.push(PickleValue::Bytes(bytes));
                }

                // -- Mark --
                MARK => {
                    // Every open mark ends in a container, so more open marks than
                    // MAX_DEPTH cannot become a valid value: refuse here, before the
                    // bookkeeping grows (a corrupt stream of MARKs would otherwise
                    // grow `marks` without bound).
                    if self.marks.len() >= MAX_DEPTH as usize {
                        return Err(CodecError::InvalidData(
                            "maximum nesting depth exceeded".to_string(),
                        ));
                    }
                    // Everything pushed from here on is above the mark
                    self.marks.push(self.stack.len());
                }

                // -- Tuple --
                EMPTY_TUPLE => self.push(PickleValue::Tuple(Vec::new())),
                TUPLE => {
                    let (items, d) = self.pop_mark_d()?;
                    self.push_at(PickleValue::Tuple(items), Self::nest(d)?);
                }
                TUPLE1 => {
                    let (a, da) = self.pop_value_d()?;
                    self.push_at(PickleValue::Tuple(vec![a]), Self::nest(da)?);
                }
                TUPLE2 => {
                    let (b, db) = self.pop_value_d()?;
                    let (a, da) = self.pop_value_d()?;
                    self.push_at(PickleValue::Tuple(vec![a, b]), Self::nest(da.max(db))?);
                }
                TUPLE3 => {
                    let (c, dc) = self.pop_value_d()?;
                    let (b, db) = self.pop_value_d()?;
                    let (a, da) = self.pop_value_d()?;
                    self.push_at(PickleValue::Tuple(vec![a, b, c]), Self::nest(da.max(db).max(dc))?);
                }

                // -- List --
                EMPTY_LIST => self.push(PickleValue::List(Vec::new())),
                LIST => {
                    let (items, d) = self.pop_mark_d()?;
                    self.push_at(PickleValue::List(items), Self::nest(d)?);
                }
                APPEND => {
                    let (val, dv) = self.pop_value_d()?;
                    self.raise_top_depth(dv)?;
                    let top = self.top_value_mut()?;
                    match top {
                        PickleValue::List(ref mut items) => {
                            items.push(val);
                        }
                        PickleValue::Reduce {
                            ref mut list_items, ..
                        } => match list_items {
                            Some(ref mut existing) => existing.push(val),
                            None => *list_items = Some(Box::new(vec![val])),
                        },
                        PickleValue::Instance(ref mut inst) => match inst.list_items {
                            Some(ref mut existing) => existing.push(val),
                            None => inst.list_items = Some(Box::new(vec![val])),
                        },
                        _ => {
                            return Err(CodecError::InvalidData(
                                "APPEND on non-list/non-object".to_string(),
                            ));
                        }
                    }
                    self.mark_top_dirty();
                }
                APPENDS => {
                    let (items, d) = self.pop_mark_d()?;
                    self.raise_top_depth(d)?;
                    let top = self.top_value_mut()?;
                    match top {
                        PickleValue::List(ref mut list_items) => {
                            list_items.extend(items);
                        }
                        PickleValue::Reduce {
                            ref mut list_items, ..
                        } => match list_items {
                            Some(ref mut existing) => existing.extend(items),
                            None => *list_items = Some(Box::new(items)),
                        },
                        PickleValue::Instance(ref mut inst) => match inst.list_items {
                            Some(ref mut existing) => existing.extend(items),
                            None => inst.list_items = Some(Box::new(items)),
                        },
                        _ => {
                            return Err(CodecError::InvalidData(
                                "APPENDS on non-list/non-object".to_string(),
                            ));
                        }
                    }
                    self.mark_top_dirty();
                }

                // -- Dict --
                EMPTY_DICT => self.push(PickleValue::Dict(Vec::new())),
                DICT => {
                    let (pairs, d) = self.pop_mark_pairs_d()?;
                    self.push_at(PickleValue::Dict(pairs), Self::nest(d)?);
                }
                SETITEM => {
                    let (val, dv) = self.pop_value_d()?;
                    let (key, dk) = self.pop_value_d()?;
                    self.raise_top_depth(dv.max(dk))?;
                    let top = self.top_value_mut()?;
                    match top {
                        PickleValue::Dict(ref mut pairs) => {
                            pairs.push((key, val));
                        }
                        PickleValue::Reduce {
                            ref mut dict_items, ..
                        } => match dict_items {
                            Some(ref mut existing) => existing.push((key, val)),
                            None => *dict_items = Some(Box::new(vec![(key, val)])),
                        },
                        PickleValue::Instance(ref mut inst) => match inst.dict_items {
                            Some(ref mut existing) => existing.push((key, val)),
                            None => inst.dict_items = Some(Box::new(vec![(key, val)])),
                        },
                        _ => {
                            return Err(CodecError::InvalidData(
                                "SETITEM on non-dict/non-object".to_string(),
                            ));
                        }
                    }
                    self.mark_top_dirty();
                }
                SETITEMS => {
                    let (new_pairs, d) = self.pop_mark_pairs_d()?;
                    self.raise_top_depth(d)?;
                    let top = self.top_value_mut()?;
                    match top {
                        PickleValue::Dict(ref mut pairs) => {
                            pairs.extend(new_pairs);
                        }
                        PickleValue::Reduce {
                            ref mut dict_items, ..
                        } => match dict_items {
                            Some(ref mut existing) => existing.extend(new_pairs),
                            None => *dict_items = Some(Box::new(new_pairs)),
                        },
                        PickleValue::Instance(ref mut inst) => match inst.dict_items {
                            Some(ref mut existing) => existing.extend(new_pairs),
                            None => inst.dict_items = Some(Box::new(new_pairs)),
                        },
                        _ => {
                            return Err(CodecError::InvalidData(
                                "SETITEMS on non-dict/non-object".to_string(),
                            ));
                        }
                    }
                    self.mark_top_dirty();
                }

                // -- Set/FrozenSet (protocol 4) --
                EMPTY_SET => self.push(PickleValue::Set(Vec::new())),
                ADDITEMS => {
                    let (items, d) = self.pop_mark_d()?;
                    self.raise_top_depth(d)?;
                    let set = self.top_value_mut()?;
                    if let PickleValue::Set(ref mut set_items) = set {
                        set_items.extend(items);
                    } else {
                        return Err(CodecError::InvalidData(
                            "ADDITEMS on non-set".to_string(),
                        ));
                    }
                    self.mark_top_dirty();
                }
                FROZENSET => {
                    let (items, d) = self.pop_mark_d()?;
                    self.push_at(PickleValue::FrozenSet(items), Self::nest(d)?);
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
                    let (args, da) = self.pop_value_d()?;
                    let (callable, dc) = self.pop_value_d()?;
                    let reduce_depth = Self::nest(da.max(dc))?;
                    // Recognize set/frozenset REDUCE pattern (protocol 3).
                    // Uses two-step check: borrow callable first, then consume
                    // args by value to move list items instead of cloning.
                    let set_variant = match &callable {
                        PickleValue::Global { module, name } if module == "builtins" => {
                            match name.as_str() {
                                "set" => Some(true),
                                "frozenset" => Some(false),
                                _ => None,
                            }
                        }
                        _ => None,
                    };
                    if let Some(is_set) = set_variant {
                        match args {
                            PickleValue::Tuple(mut tuple_items) if tuple_items.len() == 1 => {
                                match tuple_items.swap_remove(0) {
                                    PickleValue::List(items) => {
                                        // the set replaces the (list,) args tuple: one level below it
                                        self.push_at(if is_set {
                                            PickleValue::Set(items)
                                        } else {
                                            PickleValue::FrozenSet(items)
                                        }, da.saturating_sub(1).max(1));
                                    }
                                    other => {
                                        self.push_at(PickleValue::Reduce {
                                            callable: Box::new(callable),
                                            args: Box::new(PickleValue::Tuple(vec![other])),
                                            dict_items: None,
                                            list_items: None,
                                            newobj: false,
                                            state: None,
                                        }, reduce_depth);
                                    }
                                }
                            }
                            args => {
                                self.push_at(PickleValue::Reduce {
                                    callable: Box::new(callable),
                                    args: Box::new(args),
                                    dict_items: None,
                                    list_items: None,
                                    newobj: false,
                                    state: None,
                                }, reduce_depth);
                            }
                        }
                    } else {
                        self.push_at(PickleValue::Reduce {
                            callable: Box::new(callable),
                            args: Box::new(args),
                            dict_items: None,
                            list_items: None,
                            newobj: false,
                            state: None,
                        }, reduce_depth);
                    }
                }
                BUILD => {
                    let (state, ds) = self.pop_value_d()?;
                    // Pop object and its memo bindings (so we can transfer them)
                    let obj_bindings = self.stack_memo.pop().unwrap_or_default();
                    let dobj = self.depth.pop().unwrap_or(0);
                    let obj = self.stack.pop().ok_or(CodecError::StackUnderflow)?;
                    let build_depth = Self::nest(ds.max(dobj))?;
                    match obj {
                        PickleValue::Global { module, name } => {
                            self.push_at(PickleValue::Instance(Box::new(InstanceData {
                                module,
                                name,
                                state: Box::new(state),
                                dict_items: None,
                                list_items: None,
                                newobj: true,
                            })), build_depth);
                        }
                        PickleValue::Instance(inst) => {
                            // BUILD on an existing instance updates its state
                            self.push_at(PickleValue::Instance(Box::new(InstanceData {
                                module: inst.module,
                                name: inst.name,
                                state: Box::new(state),
                                dict_items: inst.dict_items,
                                list_items: inst.list_items,
                                newobj: inst.newobj,
                            })), build_depth);
                        }
                        PickleValue::Reduce {
                            callable,
                            args,
                            dict_items,
                            list_items,
                            newobj,
                            state: _,
                        } => {
                            // REDUCE/NEWOBJ followed by BUILD: the common pattern.
                            // Extract class info if callable is a Global.
                            match *callable {
                                PickleValue::Global { module, name } => {
                                    if *args == PickleValue::Tuple(vec![]) {
                                        // No constructor args: plain instance (both kinds, see #32)
                                        self.push_at(PickleValue::Instance(Box::new(InstanceData {
                                            module,
                                            name,
                                            state: Box::new(state),
                                            dict_items,
                                            list_items,
                                            newobj,
                                        })), build_depth);
                                    } else if newobj {
                                        // NEWOBJ with constructor args: the stored shape keeps
                                        // args and state side by side (#12); the wrapping dict is
                                        // one more level
                                        self.push_at(PickleValue::Instance(Box::new(InstanceData {
                                            module,
                                            name,
                                            state: Box::new(PickleValue::Dict(vec![
                                                (PickleValue::String("@args".to_string()), *args),
                                                (PickleValue::String("@state".to_string()), state),
                                            ])),
                                            dict_items,
                                            list_items,
                                            newobj: true,
                                        })), Self::nest(build_depth)?);
                                    } else {
                                        // REDUCE with args then BUILD: re-emitting as NEWOBJ would
                                        // call cls.__new__ with the args, so it stays a Reduce.
                                        self.push_at(PickleValue::Reduce {
                                            callable: Box::new(PickleValue::Global { module, name }),
                                            args,
                                            dict_items,
                                            list_items,
                                            newobj: false,
                                            state: Some(Box::new(state)),
                                        }, build_depth);
                                    }
                                }
                                _ => {
                                    // BUILD after REDUCE/NEWOBJ on a callable that is not a
                                    // Global (e.g. the result of another REDUCE): the Reduce
                                    // keeps the state and re-encodes as
                                    // `callable args REDUCE|NEWOBJ state BUILD` (#25).
                                    self.push_at(PickleValue::Reduce {
                                        callable,
                                        args,
                                        dict_items,
                                        list_items,
                                        newobj,
                                        state: Some(Box::new(state)),
                                    }, build_depth);
                                }
                            }
                        }
                        _ => {
                            // BUILD on something unexpected — keep both
                            self.push_at(PickleValue::Instance(Box::new(InstanceData {
                                module: String::new(),
                                name: String::new(),
                                state: Box::new(PickleValue::Dict(vec![
                                    (PickleValue::String("@obj".to_string()), obj),
                                    (PickleValue::String("@state".to_string()), state),
                                ])),
                                dict_items: None,
                                list_items: None,
                                newobj: true,
                            })), Self::nest(build_depth)?);
                        }
                    }
                    // Transfer memo bindings from the old object to the new
                    // stack top and update memo entries.
                    if let Some(top_bindings) = self.stack_memo.last_mut() {
                        top_bindings.extend(obj_bindings);
                    }
                    self.mark_top_dirty();
                }
                NEWOBJ => {
                    let (args, da) = self.pop_value_d()?;
                    let (cls, dc) = self.pop_value_d()?;
                    let d = Self::nest(da.max(dc))?;
                    self.push_at(PickleValue::Reduce {
                        callable: Box::new(cls),
                        args: Box::new(args),
                        dict_items: None,
                        list_items: None,
                        newobj: true,
                        state: None,
                    }, d);
                }
                NEWOBJ_EX => {
                    let (kwargs, dk) = self.pop_value_d()?;
                    let (args, da) = self.pop_value_d()?;
                    let (cls, dc) = self.pop_value_d()?;
                    // `cls.__new__(cls, *args, **kwargs)` as the reduce form CPython's own
                    // `object.__reduce_ex__` returns for `__getnewargs_ex__` objects:
                    // `copyreg.__newobj_ex__(cls, args, kwargs)`. Protocol 3 can express
                    // it as a plain REDUCE, a following BUILD becomes its state (#35).
                    let tuple_depth = Self::nest(dk.max(da).max(dc))?;
                    let d = Self::nest(tuple_depth)?;
                    self.push_at(PickleValue::Reduce {
                        callable: Box::new(PickleValue::Global {
                            module: "copyreg".to_string(),
                            name: "__newobj_ex__".to_string(),
                        }),
                        args: Box::new(PickleValue::Tuple(vec![cls, args, kwargs])),
                        dict_items: None,
                        list_items: None,
                        newobj: false,
                        state: None,
                    }, d);
                }

                // -- Persistent references (ZODB) --
                BINPERSID => {
                    let (pid, d) = self.pop_value_d()?;
                    self.push_at(PickleValue::PersistentRef(Box::new(pid)), Self::nest(d)?);
                }
                PERSID => {
                    let line = self.read_line()?;
                    let s = std::str::from_utf8(line)
                        .map_err(|_| CodecError::InvalidUtf8)?
                        .to_string();
                    self.push_at(PickleValue::PersistentRef(Box::new(PickleValue::String(s))), 1);
                }

                // -- Memo --
                BINPUT => {
                    let idx = self.read_u8()? as usize;
                    self.memo_store(idx)?;
                }
                LONG_BINPUT => {
                    let idx = self.read_u32()? as usize;
                    self.memo_store(idx)?;
                }
                MEMOIZE => {
                    // CPython: memo[len(memo)] = top. Picklers never put a slot
                    // twice, so the number of puts so far (stored or skipped) is
                    // that length.
                    let idx = self.memoize_count;
                    self.memo_store(idx)?;
                }
                BINGET => {
                    let idx = self.read_u8()? as usize;
                    let (val, d) = self.memo_get(idx)?;
                    self.push_at(val, d);
                }
                LONG_BINGET => {
                    let idx = self.read_u32()? as usize;
                    let (val, d) = self.memo_get(idx)?;
                    self.push_at(val, d);
                }
                PUT => {
                    let line = self.read_line()?;
                    let s = std::str::from_utf8(line).map_err(|_| CodecError::InvalidUtf8)?;
                    let idx: usize = s
                        .trim()
                        .parse()
                        .map_err(|e| CodecError::InvalidData(format!("PUT index: {e}")))?;
                    self.memo_store(idx)?;
                }
                GET => {
                    let line = self.read_line()?;
                    let s = std::str::from_utf8(line).map_err(|_| CodecError::InvalidUtf8)?;
                    let idx: usize = s
                        .trim()
                        .parse()
                        .map_err(|e| CodecError::InvalidData(format!("GET index: {e}")))?;
                    let (val, d) = self.memo_get(idx)?;
                    self.push_at(val, d);
                }

                // -- Stack manipulation --
                POP => {
                    // Nothing above the last MARK: CPython's load_pop discards
                    // the mark itself (protocol 0 writes `POP`s to unwind a
                    // recursive tuple, #49); otherwise the top value.
                    if self.stack.len() <= self.floor() && !self.marks.is_empty() {
                        self.marks.pop();
                    } else {
                        self.pop_value()?;
                    }
                }
                POP_MARK => {
                    // Discard everything above the last MARK and the mark; the
                    // memo bindings of the discarded slots are flushed live
                    // first (#49).
                    let mark = self.begin_pop_mark()?;
                    self.stack.truncate(mark);
                    self.stack_memo.truncate(mark);
                    self.depth.truncate(mark);
                }
                DUP => {
                    let val = self.peek_value()?.clone();
                    let d = self.top_depth();
                    self.push_at(val, d);
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

    /// Push a scalar (depth 0).
    #[inline]
    fn push(&mut self, val: PickleValue) {
        self.push_at(val, 0);
    }

    /// Push a value of known nesting depth.
    #[inline]
    fn push_at(&mut self, val: PickleValue, depth: u32) {
        self.stack.push(val);
        self.stack_memo.push(Vec::new());
        self.depth.push(depth);
    }

    #[inline]
    fn pop_value(&mut self) -> Result<PickleValue, CodecError> {
        Ok(self.pop_value_d()?.0)
    }

    /// Pop a value together with its nesting depth.
    #[inline]
    fn pop_value_d(&mut self) -> Result<(PickleValue, u32), CodecError> {
        if self.stack.len() <= self.floor() {
            return Err(CodecError::StackUnderflow);
        }
        let bindings = self.stack_memo.pop().unwrap_or_default();
        let depth = self.depth.pop().unwrap_or(0);
        let val = self.stack.pop().ok_or(CodecError::StackUnderflow)?;
        // Sync any dirty memo entries before the value leaves the stack
        if !bindings.is_empty() {
            let mut need_clone = false;
            for &idx in &bindings {
                if idx < self.dirty_memo.len() && self.dirty_memo[idx] {
                    need_clone = true;
                    break;
                }
            }
            if need_clone {
                for &idx in &bindings {
                    if idx < self.dirty_memo.len() && self.dirty_memo[idx] {
                        if idx < self.memo.len() {
                            self.memo[idx] = Some(val.clone());
                            self.memo_depth[idx] = depth;
                        }
                        self.dirty_memo[idx] = false;
                    }
                }
            }
        }
        Ok((val, depth))
    }

    /// Index below which the current sub-stack ends (the last open MARK).
    #[inline]
    fn floor(&self) -> usize {
        self.marks.last().copied().unwrap_or(0)
    }

    #[inline]
    fn peek_value(&self) -> Result<&PickleValue, CodecError> {
        if self.stack.len() <= self.floor() {
            return Err(CodecError::StackUnderflow);
        }
        self.stack.last().ok_or(CodecError::StackUnderflow)
    }

    #[inline]
    fn top_value_mut(&mut self) -> Result<&mut PickleValue, CodecError> {
        if self.stack.len() <= self.floor() {
            return Err(CodecError::StackUnderflow);
        }
        self.stack.last_mut().ok_or(CodecError::StackUnderflow)
    }

    /// Pop all items above the last MARK together with their deepest nesting depth.
    fn pop_mark_d(&mut self) -> Result<(Vec<PickleValue>, u32), CodecError> {
        let mark = self.begin_pop_mark()?;
        let max_depth = self.depth[mark..].iter().copied().max().unwrap_or(0);
        let items: Vec<PickleValue> = self.stack.drain(mark..).collect();
        self.stack_memo.truncate(mark);
        self.depth.truncate(mark);
        Ok((items, max_depth))
    }

    /// Like `pop_mark_d`, but pairs the drained slots up as dict items (DICT,
    /// SETITEMS) without an intermediate vector.
    fn pop_mark_pairs_d(&mut self) -> Result<(Vec<(PickleValue, PickleValue)>, u32), CodecError> {
        let mark = self.begin_pop_mark()?;
        if (self.stack.len() - mark) % 2 != 0 {
            return Err(CodecError::InvalidData(
                "odd number of items for dict".to_string(),
            ));
        }
        let max_depth = self.depth[mark..].iter().copied().max().unwrap_or(0);
        let mut pairs = Vec::with_capacity((self.stack.len() - mark) / 2);
        let mut drained = self.stack.drain(mark..);
        while let (Some(k), Some(v)) = (drained.next(), drained.next()) {
            pairs.push((k, v));
        }
        drop(drained);
        self.stack_memo.truncate(mark);
        self.depth.truncate(mark);
        Ok((pairs, max_depth))
    }

    /// Pop the last MARK and sync the dirty memo entries of every slot above it
    /// before those values move out of the stack. Returns the mark position.
    fn begin_pop_mark(&mut self) -> Result<usize, CodecError> {
        let mark = self.marks.pop().ok_or(CodecError::StackUnderflow)?;
        for si in mark..self.stack.len() {
            if self.stack_memo[si].is_empty() {
                continue;
            }
            for bi in 0..self.stack_memo[si].len() {
                let idx = self.stack_memo[si][bi];
                if idx < self.dirty_memo.len() && self.dirty_memo[idx] {
                    if idx < self.memo.len() {
                        self.memo[idx] = Some(self.stack[si].clone());
                        self.memo_depth[idx] = self.depth[si];
                    }
                    self.dirty_memo[idx] = false;
                }
            }
        }
        Ok(mark)
    }

    /// Depth of the current stack top (0 when empty).
    #[inline]
    fn top_depth(&self) -> u32 {
        self.depth.last().copied().unwrap_or(0)
    }

    /// Raise the stack top's depth after an in-place mutation that added
    /// children of depth `child_max`.
    #[inline]
    fn raise_top_depth(&mut self, child_max: u32) -> Result<(), CodecError> {
        let nested = Self::nest(child_max)?;
        if let Some(d) = self.depth.last_mut() {
            *d = (*d).max(nested);
        }
        Ok(())
    }

    // -- Memo operations --

    /// True if some later opcode reads memo entry `idx`.
    #[inline]
    fn memo_needed(&self, idx: usize) -> bool {
        match &self.memo_needed {
            MemoNeeds::All => true,
            MemoNeeds::Only(needed) => needed.get(idx).copied().unwrap_or(false),
        }
    }

    /// Store the stack top in memo slot `idx`, but only if something later
    /// reads that slot; skipping the put avoids the clone entirely (#22).
    #[inline]
    fn memo_store(&mut self, idx: usize) -> Result<(), CodecError> {
        self.peek_value()?; // the stack must not be empty even when the put is skipped
        if idx >= MAX_MEMO_SIZE {
            return Err(CodecError::InvalidData(format!(
                "memo index {idx} exceeds maximum {MAX_MEMO_SIZE}"
            )));
        }
        self.memoize_count += 1;
        if !self.memo_needed(idx) {
            return Ok(());
        }
        let val = self.peek_value()?.clone();
        let d = self.top_depth();
        self.memo_put(idx, val, d)?;
        self.record_memo_binding(idx);
        Ok(())
    }

    fn memo_put(&mut self, idx: usize, val: PickleValue, depth: u32) -> Result<(), CodecError> {
        if idx >= MAX_MEMO_SIZE {
            return Err(CodecError::InvalidData(format!("memo index {idx} exceeds maximum {MAX_MEMO_SIZE}")));
        }
        if idx >= self.memo.len() {
            self.memo.resize(idx + 1, None);
            self.dirty_memo.resize(idx + 1, false);
            self.memo_depth.resize(idx + 1, 0);
        }
        self.memo[idx] = Some(val);
        self.memo_depth[idx] = depth;
        self.dirty_memo[idx] = false;
        Ok(())
    }

    /// Get a memo entry and its depth, lazily resolving dirty (stale) entries first.
    fn memo_get(&mut self, idx: usize) -> Result<(PickleValue, u32), CodecError> {
        if idx < self.dirty_memo.len() && self.dirty_memo[idx] {
            self.resolve_dirty_memo(idx);
        }
        let val = self
            .memo
            .get(idx)
            .and_then(|slot| slot.clone())
            .ok_or_else(|| CodecError::InvalidData(format!("memo index {idx} not found")))?;
        Ok((val, self.memo_depth[idx]))
    }

    /// Resolve a dirty memo entry by finding its live value on the stack.
    fn resolve_dirty_memo(&mut self, memo_idx: usize) {
        // Search the stack (all frames, marks included) for the slot that
        // owns this memo binding
        // newest binding first: a re-PUT of the same index binds the newer slot
        for (si, bindings) in self.stack_memo.iter().enumerate().rev() {
            if bindings.contains(&memo_idx) {
                self.memo[memo_idx] = Some(self.stack[si].clone());
                self.memo_depth[memo_idx] = self.depth[si];
                self.dirty_memo[memo_idx] = false;
                return;
            }
        }
        // Value was already consumed from stack — memo has the last stored value
        self.dirty_memo[memo_idx] = false;
    }

    /// Record that the current stack top was stored in memo at `idx`.
    #[inline]
    fn record_memo_binding(&mut self, idx: usize) {
        if let Some(bindings) = self.stack_memo.last_mut() {
            bindings.push(idx);
        }
    }

    /// Mark memo entries bound to the current stack top as dirty (stale).
    /// Called after in-place mutations (SETITEMS, APPENDS, etc.) instead of
    /// eagerly cloning.  Resolution is deferred to memo_get() or pop_value().
    #[inline]
    fn mark_top_dirty(&mut self) {
        if let Some(bindings) = self.stack_memo.last() {
            for &idx in bindings {
                if idx < self.dirty_memo.len() {
                    self.dirty_memo[idx] = true;
                }
            }
        }
    }
}

/// Pre-scan the opcode stream and collect the memo indices that are read by
/// GET / BINGET / LONG_BINGET. Everything else the pickler memoized (in
/// protocol 2/3 that is every string and every container) is never looked
/// at again, so the decoder can skip those puts.
///
/// The walk uses the same argument-length rules as the decoder, so it never
/// gets ahead of what the decoder itself parses: wherever the walk stops on
/// truncated input, the decoder fails at the same opcode. On an opcode it
/// cannot size it gives up and reports `MemoNeeds::All`. A ZODB record is
/// two pickles sharing one memo: the caller passes the whole record.
fn scan_memo_reads(data: &[u8]) -> MemoNeeds {
    fn mark(needed: &mut Vec<bool>, idx: usize) {
        if idx < MAX_MEMO_SIZE {
            if idx >= needed.len() {
                needed.resize(idx + 1, false);
            }
            needed[idx] = true;
        }
    }
    fn skip_line(data: &[u8], mut pos: usize) -> usize {
        while pos < data.len() && data[pos] != b'\n' {
            pos += 1;
        }
        pos + 1
    }

    let mut needed: Vec<bool> = Vec::new();
    let n = data.len();
    let mut pos = 0usize;
    while pos < n {
        let op = data[pos];
        pos += 1;
        match op {
            BINGET => {
                if pos >= n {
                    break;
                }
                mark(&mut needed, data[pos] as usize);
                pos += 1;
            }
            LONG_BINGET => {
                if pos + 4 > n {
                    break;
                }
                let idx = u32::from_le_bytes(data[pos..pos + 4].try_into().unwrap()) as usize;
                mark(&mut needed, idx);
                pos += 4;
            }
            GET => {
                let end = skip_line(data, pos);
                if let Ok(s) = std::str::from_utf8(&data[pos..(end - 1).min(n)]) {
                    if let Ok(idx) = s.trim().parse::<usize>() {
                        mark(&mut needed, idx);
                    }
                }
                pos = end;
            }
            STOP | NONE | NEWTRUE | NEWFALSE | EMPTY_DICT | EMPTY_LIST | EMPTY_TUPLE
            | EMPTY_SET | MARK | POP | POP_MARK | DUP | APPEND | APPENDS | BUILD | SETITEM
            | SETITEMS
            | ADDITEMS | REDUCE | NEWOBJ | BINPERSID | TUPLE | TUPLE1 | TUPLE2 | TUPLE3
            | LIST | DICT | FROZENSET | STACK_GLOBAL | MEMOIZE | NEWOBJ_EX => {}
            PROTO | BININT1 | BINPUT => pos += 1,
            BININT2 => pos += 2,
            BININT | LONG_BINPUT => pos += 4,
            BINFLOAT | FRAME => pos += 8,
            BINUNICODE | BINSTRING | BINBYTES => {
                if pos + 4 > n {
                    break;
                }
                let len = u32::from_le_bytes(data[pos..pos + 4].try_into().unwrap()) as usize;
                pos += 4;
                pos = match pos.checked_add(len) {
                    Some(p) => p,
                    None => break,
                };
            }
            SHORT_BINUNICODE | SHORT_BINSTRING | SHORT_BINBYTES | LONG1 => {
                if pos >= n {
                    break;
                }
                let len = data[pos] as usize;
                pos += 1 + len;
            }
            BINUNICODE8 | BINBYTES8 => {
                if pos + 8 > n {
                    break;
                }
                let len = u64::from_le_bytes(data[pos..pos + 8].try_into().unwrap());
                pos += 8;
                pos = match usize::try_from(len).ok().and_then(|l| pos.checked_add(l)) {
                    Some(p) => p,
                    None => break,
                };
            }
            LONG4 => {
                if pos + 4 > n {
                    break;
                }
                let len = i32::from_le_bytes(data[pos..pos + 4].try_into().unwrap());
                pos += 4;
                if len < 0 {
                    break;
                }
                pos += len as usize;
            }
            INT | LONG | FLOAT | STRING | UNICODE | PUT | PERSID => {
                pos = skip_line(data, pos);
            }
            GLOBAL => {
                pos = skip_line(data, pos);
                pos = skip_line(data, pos);
            }
            _ => return MemoNeeds::All,
        }
    }
    MemoNeeds::Only(needed)
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

    #[test]
    fn test_memo_updated_after_build() {
        // Reproduces the shared-reference bug: NEWOBJ + BINPUT creates a
        // Reduce in memo, BUILD transforms the stack top to Instance, but
        // memo was stale. After the fix, BINGET should return Instance.
        //
        // Pickle VM sequence:
        //   PROTO 3
        //   GLOBAL "mymod\nMyClass\n"
        //   EMPTY_TUPLE
        //   NEWOBJ           -> Reduce{Global{mymod,MyClass}, ()}
        //   BINPUT 0         -> memo[0] = clone of Reduce
        //   EMPTY_DICT
        //   SHORT_BINUNICODE "name"
        //   SHORT_BINUNICODE "test"
        //   SETITEM          -> {"name": "test"}
        //   BUILD            -> Instance{mymod, MyClass, {"name": "test"}}
        //   BINGET 0         -> should be Instance (was stale Reduce before fix)
        //   TUPLE2           -> (Instance_from_build, memo_copy)
        //   STOP
        let data: &[u8] = &[
            0x80, 0x03,                                     // PROTO 3
            b'c', b'm', b'y', b'm', b'o', b'd', b'\n',     // GLOBAL module="mymod"
            b'M', b'y', b'C', b'l', b's', b'\n',            // GLOBAL name="MyCls"
            b')',                                            // EMPTY_TUPLE
            0x81,                                            // NEWOBJ
            b'q', 0x00,                                      // BINPUT 0
            b'}',                                            // EMPTY_DICT
            0x8c, 0x04, b'n', b'a', b'm', b'e',             // SHORT_BINUNICODE "name"
            0x8c, 0x04, b't', b'e', b's', b't',             // SHORT_BINUNICODE "test"
            b's',                                            // SETITEM
            b'b',                                            // BUILD
            b'h', 0x00,                                      // BINGET 0
            0x86,                                            // TUPLE2
            b'.',                                            // STOP
        ];
        let result = decode_pickle(data).unwrap();

        // Result should be a tuple of two elements
        if let PickleValue::Tuple(items) = &result {
            assert_eq!(items.len(), 2, "expected 2-tuple");

            let expected = PickleValue::Instance(Box::new(InstanceData {
                module: "mymod".to_string(),
                name: "MyCls".to_string(),
                state: Box::new(PickleValue::Dict(vec![(
                    PickleValue::String("name".to_string()),
                    PickleValue::String("test".to_string()),
                )])),
                dict_items: None,
                list_items: None,
                newobj: true,
            }));

            // First element: the Instance from BUILD (stack top)
            assert_eq!(items[0], expected, "BUILD result should be Instance");

            // Second element: the memo copy from BINGET — must also be
            // Instance after the fix (was stale Reduce before)
            assert_eq!(
                items[1], expected,
                "BINGET after BUILD should return updated Instance, not stale Reduce"
            );
        } else {
            panic!("expected Tuple, got {:?}", result);
        }
    }

    #[test]
    fn test_memo_updated_after_setitems() {
        // Reproduces issue #18: EMPTY_DICT + BINPUT + SETITEMS leaves stale
        // empty dict in memo. When the same dict is referenced twice (once in
        // a list, once as a dict value), the second reference via BINGET returns
        // the empty snapshot.
        //
        // Pickle VM sequence:
        //   PROTO 3
        //   EMPTY_DICT             -> outer dict
        //   MARK
        //     BINUNICODE "a"
        //     EMPTY_DICT           -> inner dict (shared)
        //     BINPUT 0             -> memo[0] = empty clone
        //     MARK
        //       BINUNICODE "x"
        //       BININT1 1
        //     SETITEMS              -> inner dict = {"x": 1}
        //     BINUNICODE "b"
        //     BINGET 0             -> should be {"x": 1} (was {} before fix)
        //   SETITEMS               -> outer = {"a": {"x": 1}, "b": {"x": 1}}
        //   STOP
        let data: &[u8] = &[
            0x80, 0x03,                                                 // PROTO 3
            b'}',                                                       // EMPTY_DICT (outer)
            b'(',                                                       // MARK
            b'X', 0x01, 0x00, 0x00, 0x00, b'a',                       // BINUNICODE "a"
            b'}',                                                       // EMPTY_DICT (inner)
            b'q', 0x00,                                                 // BINPUT 0
            b'(',                                                       // MARK
            b'X', 0x01, 0x00, 0x00, 0x00, b'x',                       // BINUNICODE "x"
            b'K', 0x01,                                                 // BININT1 1
            b'u',                                                       // SETITEMS (fills inner)
            b'X', 0x01, 0x00, 0x00, 0x00, b'b',                       // BINUNICODE "b"
            b'h', 0x00,                                                 // BINGET 0
            b'u',                                                       // SETITEMS (fills outer)
            b'.',                                                       // STOP
        ];
        let result = decode_pickle(data).unwrap();

        let inner = PickleValue::Dict(vec![(
            PickleValue::String("x".to_string()),
            PickleValue::Int(1),
        )]);
        let expected = PickleValue::Dict(vec![
            (PickleValue::String("a".to_string()), inner.clone()),
            (PickleValue::String("b".to_string()), inner),
        ]);
        assert_eq!(
            result, expected,
            "BINGET after SETITEMS should return populated dict, not empty"
        );
    }

    #[test]
    fn test_memo_updated_after_appends() {
        // Same pattern but for lists: EMPTY_LIST + BINPUT + APPENDS.
        //
        // Pickle VM sequence:
        //   PROTO 3
        //   EMPTY_LIST             -> inner list (shared)
        //   BINPUT 0               -> memo[0] = empty clone
        //   MARK
        //     BININT1 1
        //     BININT1 2
        //   APPENDS                -> inner list = [1, 2]
        //   BINGET 0               -> should be [1, 2]
        //   TUPLE2                 -> ([1,2], [1,2])
        //   STOP
        let data: &[u8] = &[
            0x80, 0x03,                     // PROTO 3
            b']',                           // EMPTY_LIST
            b'q', 0x00,                     // BINPUT 0
            b'(',                           // MARK
            b'K', 0x01,                     // BININT1 1
            b'K', 0x02,                     // BININT1 2
            b'e',                           // APPENDS
            b'h', 0x00,                     // BINGET 0
            0x86,                           // TUPLE2
            b'.',                           // STOP
        ];
        let result = decode_pickle(data).unwrap();

        let inner = PickleValue::List(vec![PickleValue::Int(1), PickleValue::Int(2)]);
        let expected = PickleValue::Tuple(vec![inner.clone(), inner]);
        assert_eq!(
            result, expected,
            "BINGET after APPENDS should return populated list, not empty"
        );
    }

    #[test]
    fn test_long4_negative_length() {
        // PROTO 2, LONG4 with length=-1 (0xFFFFFFFF as i32)
        let data = b"\x80\x02\x8b\xff\xff\xff\xff";
        let err = decode_pickle(data).unwrap_err();
        assert!(err.to_string().contains("negative length"));
    }

    #[test]
    fn test_binstring_negative_length() {
        // PROTO 2, BINSTRING with length=-1
        let data = b"\x80\x02T\xff\xff\xff\xff";
        let err = decode_pickle(data).unwrap_err();
        assert!(err.to_string().contains("negative length"));
    }

    #[test]
    fn test_memo_index_too_large() {
        // PROTO 2, NONE, LONG_BINPUT with index=4_000_000_000
        let idx_bytes = 4_000_000_000u32.to_le_bytes();
        let mut data = vec![0x80, 0x02, b'N', b'r'];
        data.extend_from_slice(&idx_bytes);
        let err = decode_pickle(&data).unwrap_err();
        assert!(err.to_string().contains("memo index"));
    }

    #[test]
    fn test_long_value_too_large() {
        // PROTO 2, LONG with huge text representation
        let mut data = vec![0x80, 0x02, b'L'];
        data.extend_from_slice(&vec![b'9'; 20_000]);
        data.push(b'\n');
        data.push(b'.');
        let err = decode_pickle(&data).unwrap_err();
        assert!(err.to_string().contains("too large"));
    }

    #[test]
    fn test_binunicode8_too_large() {
        // PROTO 4, BINUNICODE8 with huge length
        let mut data = vec![0x80, 0x04];
        data.push(0x8d); // BINUNICODE8
        data.extend_from_slice(&(1u64 << 40).to_le_bytes()); // 1 TB
        let err = decode_pickle(&data).unwrap_err();
        assert!(err.to_string().contains("too large"));
    }

    #[test]
    fn test_binbytes8_too_large() {
        // PROTO 4, BINBYTES8 with huge length
        let mut data = vec![0x80, 0x04];
        data.push(0x8e); // BINBYTES8
        data.extend_from_slice(&(1u64 << 40).to_le_bytes()); // 1 TB
        let err = decode_pickle(&data).unwrap_err();
        assert!(err.to_string().contains("too large"));
    }

    #[test]
    fn test_setitems_on_reduce() {
        // Simulates dict subclass: GLOBAL + EMPTY_TUPLE + REDUCE + MARK + items + SETITEMS
        // Pattern: collections.OrderedDict() then od["key1"] = "val1", od["key2"] = "val2"
        let data: &[u8] = &[
            0x80, 0x03, // PROTO 3
            b'c', b'c', b'o', b'l', b'l', b'e', b'c', b't', b'i', b'o', b'n', b's',
            b'\n', // GLOBAL module="collections"
            b'O', b'r', b'd', b'e', b'r', b'e', b'd', b'D', b'i', b'c', b't',
            b'\n', // GLOBAL name="OrderedDict"
            b')', // EMPTY_TUPLE
            b'R', // REDUCE
            b'(', // MARK
            0x8c, 0x04, b'k', b'e', b'y', b'1', // SHORT_BINUNICODE "key1"
            0x8c, 0x04, b'v', b'a', b'l', b'1', // SHORT_BINUNICODE "val1"
            0x8c, 0x04, b'k', b'e', b'y', b'2', // SHORT_BINUNICODE "key2"
            0x8c, 0x04, b'v', b'a', b'l', b'2', // SHORT_BINUNICODE "val2"
            b'u', // SETITEMS
            b'.', // STOP
        ];
        let result = decode_pickle(data).unwrap();
        match &result {
            PickleValue::Reduce {
                callable,
                args,
                dict_items,
                list_items,
                ..
            } => {
                if let PickleValue::Global { module, name } = callable.as_ref() {
                    assert_eq!(module, "collections");
                    assert_eq!(name, "OrderedDict");
                } else {
                    panic!("expected Global callable");
                }
                assert_eq!(**args, PickleValue::Tuple(vec![]));
                let items = dict_items.as_ref().expect("dict_items should be Some");
                assert_eq!(items.len(), 2);
                assert_eq!(items[0].0, PickleValue::String("key1".to_string()));
                assert_eq!(items[0].1, PickleValue::String("val1".to_string()));
                assert_eq!(items[1].0, PickleValue::String("key2".to_string()));
                assert_eq!(items[1].1, PickleValue::String("val2".to_string()));
                assert!(list_items.is_none());
            }
            _ => panic!("expected Reduce, got {:?}", result),
        }
    }

    #[test]
    fn test_setitem_on_reduce() {
        // Single SETITEM on a Reduce (dict subclass with one item)
        let data: &[u8] = &[
            0x80, 0x03, // PROTO 3
            b'c', b'c', b'o', b'l', b'l', b'e', b'c', b't', b'i', b'o', b'n', b's',
            b'\n', b'O', b'r', b'd', b'e', b'r', b'e', b'd', b'D', b'i', b'c', b't',
            b'\n', b')', b'R', // GLOBAL + EMPTY_TUPLE + REDUCE
            0x8c, 0x03, b'k', b'e', b'y', // SHORT_BINUNICODE "key"
            0x8c, 0x03, b'v', b'a', b'l', // SHORT_BINUNICODE "val"
            b's', // SETITEM
            b'.', // STOP
        ];
        let result = decode_pickle(data).unwrap();
        if let PickleValue::Reduce { dict_items, .. } = &result {
            let items = dict_items.as_ref().unwrap();
            assert_eq!(items.len(), 1);
            assert_eq!(items[0].0, PickleValue::String("key".to_string()));
            assert_eq!(items[0].1, PickleValue::String("val".to_string()));
        } else {
            panic!("expected Reduce");
        }
    }

    #[test]
    fn test_appends_on_reduce() {
        // List subclass: GLOBAL + EMPTY_TUPLE + REDUCE + MARK + items + APPENDS
        let data: &[u8] = &[
            0x80, 0x03, // PROTO 3
            b'c', b'c', b'o', b'l', b'l', b'e', b'c', b't', b'i', b'o', b'n', b's',
            b'\n', b'd', b'e', b'q', b'u', b'e', b'\n', // GLOBAL collections.deque
            b')', // EMPTY_TUPLE
            b'R', // REDUCE
            b'(', // MARK
            b'K', 0x01, // BININT1 1
            b'K', 0x02, // BININT1 2
            b'K', 0x03, // BININT1 3
            b'e', // APPENDS
            b'.', // STOP
        ];
        let result = decode_pickle(data).unwrap();
        if let PickleValue::Reduce { list_items, .. } = &result {
            let items = list_items.as_ref().unwrap();
            assert_eq!(items.len(), 3);
            assert_eq!(items[0], PickleValue::Int(1));
            assert_eq!(items[1], PickleValue::Int(2));
            assert_eq!(items[2], PickleValue::Int(3));
        } else {
            panic!("expected Reduce");
        }
    }

    fn tuple1_chain(n: usize) -> Vec<u8> {
        let mut data = vec![0x80, 0x03, b'N'];
        data.extend(std::iter::repeat(TUPLE1).take(n));
        data.push(b'.');
        data
    }

    #[test]
    fn test_decoder_depth_boundary() {
        assert!(decode_pickle(&tuple1_chain(1000)).is_ok());
        let err = decode_pickle(&tuple1_chain(1001)).unwrap_err();
        assert!(err.to_string().contains("nesting depth"), "{err}");
    }

    #[test]
    fn test_memo_depth_carries() {
        // value of depth 999, memoized, fetched back and wrapped twice -> 1001
        let mut data = vec![0x80, 0x03, b'N'];
        data.extend(std::iter::repeat(TUPLE1).take(999));
        data.extend_from_slice(&[BINPUT, 0, POP, BINGET, 0, TUPLE1, TUPLE1, b'.']);
        let err = decode_pickle(&data).unwrap_err();
        assert!(err.to_string().contains("nesting depth"), "{err}");
        // wrapped once -> exactly 1000, fine
        let mut ok = vec![0x80, 0x03, b'N'];
        ok.extend(std::iter::repeat(TUPLE1).take(999));
        ok.extend_from_slice(&[BINPUT, 0, POP, BINGET, 0, TUPLE1, b'.']);
        assert!(decode_pickle(&ok).is_ok());
    }

    #[test]
    fn test_mutation_raises_depth() {
        // EMPTY_LIST, MARK, <value of depth 1000>, APPENDS  -> list of depth 1001
        let mut data = vec![0x80, 0x03, EMPTY_LIST, MARK, b'N'];
        data.extend(std::iter::repeat(TUPLE1).take(1000));
        data.extend_from_slice(&[APPENDS, b'.']);
        let err = decode_pickle(&data).unwrap_err();
        assert!(err.to_string().contains("nesting depth"), "{err}");
    }

    #[test]
    fn test_reduce_with_args_then_build_keeps_reduce_with_state() {
        // GLOBAL m.C, "a", TUPLE1, REDUCE, EMPTY_DICT, BUILD
        let data: &[u8] = &[
            0x80, 0x03, b'c', b'm', b'\n', b'C', b'\n', 0x8c, 0x01, b'a', 0x85, b'R', b'}', b'b', b'.',
        ];
        match decode_pickle(data).unwrap() {
            PickleValue::Reduce { newobj, state, args, .. } => {
                assert!(!newobj);
                assert_eq!(*args, PickleValue::Tuple(vec![PickleValue::String("a".into())]));
                assert_eq!(state, Some(Box::new(PickleValue::Dict(vec![]))));
            }
            other => panic!("expected Reduce with state, got {other:?}"),
        }
    }

    #[test]
    fn test_newobj_with_args_then_build_folds_into_instance() {
        // GLOBAL m.C, "a", TUPLE1, NEWOBJ, EMPTY_DICT, BUILD  ->  Instance with @args/@state
        let data: &[u8] = &[
            0x80, 0x03, b'c', b'm', b'\n', b'C', b'\n', 0x8c, 0x01, b'a', 0x85, 0x81, b'}', b'b', b'.',
        ];
        match decode_pickle(data).unwrap() {
            PickleValue::Instance(inst) => {
                assert_eq!(
                    *inst.state,
                    PickleValue::Dict(vec![
                        (PickleValue::String("@args".into()), PickleValue::Tuple(vec![PickleValue::String("a".into())])),
                        (PickleValue::String("@state".into()), PickleValue::Dict(vec![])),
                    ])
                );
            }
            other => panic!("expected Instance, got {other:?}"),
        }
    }

    #[test]
    fn test_reduce_empty_args_build_keeps_reduce_kind() {
        // GLOBAL m.C, EMPTY_TUPLE, REDUCE, {"x": 1}, BUILD: a __reduce__ returning (cls, (), state)
        let data = b"\x80\x03cm\nC\n)R}X\x01\x00\x00\x00xK\x01sb.";
        match decode_pickle(data).unwrap() {
            PickleValue::Instance(inst) => {
                assert_eq!(inst.module, "m");
                assert!(!inst.newobj, "REDUCE-created instance must not be marked newobj");
            }
            other => panic!("expected Instance, got {other:?}"),
        }
        // NEWOBJ stays newobj
        let data = b"\x80\x03cm\nC\n)\x81}X\x01\x00\x00\x00xK\x01sb.";
        match decode_pickle(data).unwrap() {
            PickleValue::Instance(inst) => assert!(inst.newobj),
            other => panic!("expected Instance, got {other:?}"),
        }
        // a second BUILD keeps the kind
        let data = b"\x80\x03cm\nC\n)R}X\x01\x00\x00\x00xK\x01sb}X\x01\x00\x00\x00yK\x02sb.";
        match decode_pickle(data).unwrap() {
            PickleValue::Instance(inst) => {
                assert!(!inst.newobj);
                assert_eq!(
                    *inst.state,
                    PickleValue::Dict(vec![(PickleValue::String("y".into()), PickleValue::Int(2))])
                );
            }
            other => panic!("expected Instance, got {other:?}"),
        }
    }

    #[test]
    fn test_newobj_ex_decodes_to_copyreg_newobj_ex_reduce() {
        // PROTO 4, GLOBAL m.C, (1,), {"k": 2}, NEWOBJ_EX, STOP
        let data = b"\x80\x04cm\nC\nK\x01\x85}X\x01\x00\x00\x00kK\x02s\x92.";
        match decode_pickle(data).unwrap() {
            PickleValue::Reduce { callable, args, newobj, state, .. } => {
                assert_eq!(
                    *callable,
                    PickleValue::Global { module: "copyreg".into(), name: "__newobj_ex__".into() }
                );
                assert_eq!(
                    *args,
                    PickleValue::Tuple(vec![
                        PickleValue::Global { module: "m".into(), name: "C".into() },
                        PickleValue::Tuple(vec![PickleValue::Int(1)]),
                        PickleValue::Dict(vec![(PickleValue::String("k".into()), PickleValue::Int(2))]),
                    ])
                );
                assert!(!newobj);
                assert!(state.is_none());
            }
            other => panic!("expected Reduce, got {other:?}"),
        }
        // with BUILD: the state rides on the Reduce
        let data = b"\x80\x04cm\nC\nK\x01\x85}X\x01\x00\x00\x00kK\x02s\x92}X\x01\x00\x00\x00xK\x03sb.";
        match decode_pickle(data).unwrap() {
            PickleValue::Reduce { state: Some(st), newobj: false, .. } => {
                assert_eq!(
                    *st,
                    PickleValue::Dict(vec![(PickleValue::String("x".into()), PickleValue::Int(3))])
                );
            }
            other => panic!("expected Reduce with state, got {other:?}"),
        }
    }

    #[test]
    fn test_pop_mark_discards_to_the_mark() {
        // PROTO 2, MARK, K1, K2, POP_MARK, K3, STOP -> 3
        assert_eq!(decode_pickle(b"\x80\x02(K\x01K\x021K\x03.").unwrap(), PickleValue::Int(3));
        // memo bindings above the mark are flushed live before the discard:
        // MARK, EMPTY_LIST, BINPUT 1, K1, APPEND, POP_MARK, BINGET 1, STOP -> [1]
        assert_eq!(
            decode_pickle(b"\x80\x02(]q\x01K\x01a1h\x01.").unwrap(),
            PickleValue::List(vec![PickleValue::Int(1)])
        );
        // no mark: error
        assert!(decode_pickle(b"\x80\x02K\x011.").is_err());
    }

    #[test]
    fn test_pop_at_a_mark_pops_the_mark() {
        // MARK, POP, K3, STOP -> 3 (CPython: the stack is at the fence, so POP removes the mark)
        assert_eq!(decode_pickle(b"\x80\x02(0K\x03.").unwrap(), PickleValue::Int(3));
        // POP of a value is unchanged: K1, K2, POP, STOP -> 1
        assert_eq!(decode_pickle(b"\x80\x02K\x01K\x020.").unwrap(), PickleValue::Int(1));
        // empty stack, no mark: still an error
        assert!(decode_pickle(b"\x80\x020K\x01.").is_err());
    }

    #[test]
    fn test_get_of_never_put_index_below_a_put_one_is_an_error() {
        // PROTO 3, BININT1 1, BINPUT 4, BINGET 4, BINGET 0, TUPLE2, STOP
        let err = decode_pickle(b"\x80\x03K\x01q\x04h\x04h\x00\x86.").unwrap_err();
        assert!(err.to_string().contains("memo index 0 not found"), "{err}");
        // protocol 0 text opcodes, same shape
        let err = decode_pickle(b"I1\np4\ng4\ng0\n\x86.").unwrap_err();
        assert!(err.to_string().contains("memo index 0 not found"), "{err}");
        // a put that is never read followed by a read of a higher index is fine
        assert_eq!(
            decode_pickle(b"\x80\x03K\x01q\x00K\x02q\x04h\x04.").unwrap(),
            PickleValue::Int(2)
        );
        // a dirty entry (container mutated after the put) after a gap put resolves live
        // EMPTY_LIST BINPUT 3, K1 APPEND, BINGET 3, STOP -> [1]
        assert_eq!(
            decode_pickle(b"\x80\x03]q\x03K\x01ah\x03.").unwrap(),
            PickleValue::List(vec![PickleValue::Int(1)])
        );
    }

    #[test]
    fn test_newobj_without_build_is_flagged() {
        let data: &[u8] = &[0x80, 0x03, b'c', b'm', b'\n', b'C', b'\n', 0x8c, 0x01, b'a', 0x85, 0x81, b'.'];
        match decode_pickle(data).unwrap() {
            PickleValue::Reduce { newobj, state, .. } => {
                assert!(newobj);
                assert!(state.is_none());
            }
            other => panic!("expected Reduce, got {other:?}"),
        }
    }

    #[test]
    fn test_setitems_on_reduce_with_build() {
        // Dict subclass: REDUCE + SETITEMS + BUILD (all three combined)
        // This tests that dict_items carry through from Reduce to Instance
        let data: &[u8] = &[
            0x80, 0x03, // PROTO 3
            b'c', b'm', b'y', b'm', b'o', b'd', b'\n', b'M', b'y', b'D', b'i', b'c', b't',
            b'\n', // GLOBAL mymod.MyDict
            b')', // EMPTY_TUPLE
            b'R', // REDUCE
            b'(', // MARK
            0x8c, 0x01, b'a', // "a"
            b'K', 0x01, // 1
            b'u', // SETITEMS
            b'}', // EMPTY_DICT (state for BUILD)
            0x8c, 0x05, b'e', b'x', b't', b'r', b'a', // "extra"
            0x8c, 0x03, b'y', b'e', b's', // "yes"
            b's', // SETITEM (on the dict, building state)
            b'b', // BUILD
            b'.', // STOP
        ];
        let result = decode_pickle(data).unwrap();
        if let PickleValue::Instance(ref inst) = &result {
            assert_eq!(inst.module, "mymod");
            assert_eq!(inst.name, "MyDict");
            let items = inst.dict_items.as_ref().expect("dict_items should carry through BUILD");
            assert_eq!(items.len(), 1);
            assert_eq!(items[0].0, PickleValue::String("a".to_string()));
            assert_eq!(items[0].1, PickleValue::Int(1));
        } else {
            panic!("expected Instance, got {:?}", result);
        }
    }

    fn run_decoder(data: &[u8]) -> (Result<PickleValue, CodecError>, usize) {
        let mut d = Decoder::new(data);
        let r = d.run();
        (r, d.memo.len())
    }

    #[test]
    fn test_prescan_skips_unread_puts() {
        // {"k": "v"} as CPython writes it: every value memoized, nothing read
        let data: &[u8] = &[
            0x80, 0x03, b'}', b'q', 0x00, b'X', 1, 0, 0, 0, b'k', b'q', 0x01, b'X', 1, 0, 0, 0,
            b'v', b'q', 0x02, b's', b'.',
        ];
        let (result, memo_len) = run_decoder(data);
        assert_eq!(
            result.unwrap(),
            PickleValue::Dict(vec![(PickleValue::String("k".into()), PickleValue::String("v".into()))])
        );
        assert_eq!(memo_len, 0, "no memo entry may be stored when nothing reads it");
    }

    #[test]
    fn test_prescan_keeps_read_entry_and_dirty_sync() {
        // EMPTY_DICT BINPUT 0 MARK "a" 1 SETITEMS BINGET 0 TUPLE2 STOP: the read
        // index is kept and the 1.6.0 dirty sync still delivers the filled dict
        let data: &[u8] = &[
            0x80, 0x03, b'}', b'q', 0x00, b'(', b'X', 1, 0, 0, 0, b'a', b'K', 0x01, b'u', b'h',
            0x00, 0x86, b'.',
        ];
        let (result, memo_len) = run_decoder(data);
        let filled = PickleValue::Dict(vec![(PickleValue::String("a".into()), PickleValue::Int(1))]);
        assert_eq!(result.unwrap(), PickleValue::Tuple(vec![filled.clone(), filled]));
        assert_eq!(memo_len, 1);
    }

    #[test]
    fn test_prescan_read_before_fill() {
        // EMPTY_DICT BINPUT 0 BINGET 0 TUPLE2 STOP: read with no fill in between
        let data: &[u8] = &[0x80, 0x03, b'}', b'q', 0x00, b'h', 0x00, 0x86, b'.'];
        let (result, _) = run_decoder(data);
        assert_eq!(
            result.unwrap(),
            PickleValue::Tuple(vec![PickleValue::Dict(vec![]), PickleValue::Dict(vec![])])
        );
    }

    #[test]
    fn test_prescan_overwritten_index() {
        // BININT1 1 BINPUT 0 POP BININT1 2 BINPUT 0 BINGET 0 TUPLE1 STOP -> (2,)
        let data: &[u8] = &[
            0x80, 0x03, b'K', 1, b'q', 0x00, b'0', b'K', 2, b'q', 0x00, b'h', 0x00, 0x85, b'.',
        ];
        let (result, _) = run_decoder(data);
        assert_eq!(result.unwrap(), PickleValue::Tuple(vec![PickleValue::Int(2)]));
    }

    #[test]
    fn test_prescan_protocol0_put_get() {
        // ] p0\n g0\n TUPLE2 . -> ([], [])
        let data: &[u8] = b"]p0\ng0\n\x86.";
        let (result, memo_len) = run_decoder(data);
        assert_eq!(
            result.unwrap(),
            PickleValue::Tuple(vec![PickleValue::List(vec![]), PickleValue::List(vec![])])
        );
        assert_eq!(memo_len, 1);
    }

    #[test]
    fn test_prescan_memoize_counts_skipped_puts() {
        // protocol 4: FRAME, EMPTY_LIST MEMOIZE (index 0, never read), BININT1 1
        // MEMOIZE (index 1, read), POP, BINGET 1, TUPLE2 -> ([], 1); the second
        // MEMOIZE must get index 1 although the first put was skipped, or the
        // BINGET fails
        let body: &[u8] = &[b']', 0x94, b'K', 1, 0x94, b'0', b'h', 0x01, 0x86, b'.'];
        let mut data = vec![0x80, 0x04, 0x95];
        data.extend_from_slice(&(body.len() as u64).to_le_bytes());
        data.extend_from_slice(body);
        let (result, memo_len) = run_decoder(&data);
        assert_eq!(
            result.unwrap(),
            PickleValue::Tuple(vec![PickleValue::List(vec![]), PickleValue::Int(1)])
        );
        assert_eq!(memo_len, 2, "index 1 is stored, index 0 stays an unused slot");
    }

    #[test]
    fn test_prescan_long_binget() {
        // LONG_BINPUT 300 (u32), POP, BININT1 1, LONG_BINGET 300, TUPLE2 -> (1, 2)... the
        // wide read keeps the wide put: BININT1 2, LONG_BINPUT 300, BININT1 1, LONG_BINGET 300
        let data: &[u8] = &[
            0x80, 0x03, b'K', 2, b'r', 44, 1, 0, 0, b'K', 1, b'j', 44, 1, 0, 0, 0x86, b'.',
        ];
        let (result, memo_len) = run_decoder(data);
        assert_eq!(result.unwrap(), PickleValue::Tuple(vec![PickleValue::Int(1), PickleValue::Int(2)]));
        assert_eq!(memo_len, 301);
    }

    #[test]
    fn test_prescan_get_of_unput_index_errors() {
        let data: &[u8] = &[0x80, 0x03, b'h', 0x05, b'.'];
        let err = decode_pickle(data).unwrap_err();
        assert!(matches!(err, CodecError::InvalidData(ref m) if m.contains("memo index 5 not found")), "{err:?}");
    }

    #[test]
    fn test_prescan_truncated_argument() {
        // BINUNICODE announces 100 bytes, 3 follow: the scan stops, the decoder fails cleanly
        let data: &[u8] = &[0x80, 0x03, b'}', b'q', 0x00, b'X', 100, 0, 0, 0, b'a', b'b', b'c'];
        assert!(matches!(decode_pickle(data), Err(CodecError::UnexpectedEof)));
        let data: &[u8] = &[0x80, 0x03, b'}', b'q', 0x00, b'X', 1, 0];
        assert!(matches!(decode_pickle(data), Err(CodecError::UnexpectedEof)));
    }

    #[test]
    fn test_prescan_unknown_opcode_keeps_all() {
        // 0x97 (NEXT_BUFFER) is not sized by the scan: it reports All ...
        assert!(matches!(scan_memo_reads(&[0x80, 0x03, b'}', b'q', 0x00, 0x97, b'.']), MemoNeeds::All));
        // ... and the decoder itself still rejects the opcode as before
        assert!(decode_pickle(&[0x80, 0x03, b'}', b'q', 0x00, 0x97, b'.']).is_err());
        // a normal stream reports exactly the indices read
        match scan_memo_reads(&[0x80, 0x03, b'}', b'q', 0x00, b'h', 0x00, b'q', 0x02, b'.']) {
            MemoNeeds::Only(needed) => assert_eq!(needed, vec![true]),
            MemoNeeds::All => panic!("expected Only"),
        }
    }

    #[test]
    fn test_long1_direct_matches_bigint() {
        // every length 0..=9, four fill bytes: the direct i64 path and BigInt agree
        for n in 0..=9usize {
            for fill in [0x00u8, 0x7f, 0x80, 0xff] {
                let mut data = vec![0x80, 0x03, 0x8a, n as u8];
                data.extend(std::iter::repeat_n(fill, n));
                data.push(b'.');
                let got = decode_pickle(&data).unwrap();
                let expected = num_bigint::BigInt::from_signed_bytes_le(&data[4..4 + n]);
                match got {
                    PickleValue::Int(i) => {
                        assert_eq!(num_bigint::BigInt::from(i), expected, "n={n} fill={fill:#x}")
                    }
                    PickleValue::BigInt(b) => {
                        assert!(i64::try_from(&expected).is_err(), "n={n} fill={fill:#x} should be Int");
                        assert_eq!(b, expected);
                    }
                    other => panic!("{other:?}"),
                }
            }
        }
        // i64::MIN is exactly 8 bytes; 2**63 needs 9 and stays a BigInt
        let data = [&[0x80u8, 0x03, 0x8a, 8][..], &i64::MIN.to_le_bytes(), b"."].concat();
        assert_eq!(decode_pickle(&data).unwrap(), PickleValue::Int(i64::MIN));
        let data = [&[0x80u8, 0x03, 0x8a, 9][..], &[0, 0, 0, 0, 0, 0, 0, 0x80, 0], b"."].concat();
        assert!(matches!(decode_pickle(&data).unwrap(), PickleValue::BigInt(_)));
    }

    #[test]
    fn test_nested_marks_with_pop_and_dup() {
        // MARK, 1, MARK, 2, 3, TUPLE, DUP, POP, TUPLE, STOP -> (1, (2, 3))
        let data: &[u8] = &[0x80, 0x03, b'(', b'K', 1, b'(', b'K', 2, b'K', 3, b't', b'2', b'0', b't', b'.'];
        assert_eq!(
            decode_pickle(data).unwrap(),
            PickleValue::Tuple(vec![
                PickleValue::Int(1),
                PickleValue::Tuple(vec![PickleValue::Int(2), PickleValue::Int(3)])
            ])
        );
        // POP below the mark and a second POP on an empty stack must both error
        assert!(decode_pickle(&[0x80, 0x03, b'K', 1, b'(', b'0', b't', b'.']).is_err());
        assert!(decode_pickle(&[0x80, 0x03, b'(', b't', b'0', b'0', b'.']).is_err());
        // BINPUT right after a MARK sees an empty sub-stack (CPython: IndexError)
        assert!(decode_pickle(&[0x80, 0x03, b'K', 1, b'(', b'q', 0, b't', b'.']).is_err());
        // TUPLE without any MARK errors instead of taking the whole stack
        assert!(decode_pickle(&[0x80, 0x03, b'K', 1, b't', b'.']).is_err());
    }

    #[test]
    fn test_marks_cleared_between_record_pickles() {
        // a MARK left open by the first pickle must not poison the second one
        let class_pickle: &[u8] = &[0x80, 0x03, b'(', b'K', 1, b'.'];
        let state_pickle: &[u8] = &[0x80, 0x03, b'}', b'.'];
        let data = [class_pickle, state_pickle].concat();
        let (c, st) = decode_zodb_pickles(&data).unwrap();
        assert_eq!(c, PickleValue::Int(1));
        assert_eq!(st, PickleValue::Dict(vec![]));
    }

    #[test]
    fn test_scratch_reused_and_capped() {
        // a record leaves its capacity behind for the next one on this thread
        let small: &[u8] = &[0x80, 0x03, b'}', b'q', 0x00, b'X', 1, 0, 0, 0, b'k', b'K', 1, b's', b'.'];
        decode_pickle(small).unwrap();
        let cap_after_small = SCRATCH.with(|c| c.borrow().stack.capacity());
        assert!(cap_after_small >= 2);
        // an error mid-way must not leak state into the next decode
        assert!(decode_pickle(&[0x80, 0x03, b'(', b'K', 1, b'K', 2]).is_err());
        assert_eq!(
            decode_pickle(small).unwrap(),
            PickleValue::Dict(vec![(PickleValue::String("k".into()), PickleValue::Int(1))])
        );
        // a huge stack is not kept: MARK then MAX_SCRATCH_ELEMS + 1 scalars
        let mut huge = vec![0x80, 0x03, b'('];
        huge.extend(std::iter::repeat_n(b'N', MAX_SCRATCH_ELEMS + 1));
        huge.extend_from_slice(b"t.");
        assert!(matches!(decode_pickle(&huge).unwrap(), PickleValue::Tuple(ref t) if t.len() == MAX_SCRATCH_ELEMS + 1));
        assert!(SCRATCH.with(|c| c.borrow().stack.capacity()) <= MAX_SCRATCH_ELEMS);
    }

    #[test]
    fn test_open_marks_bounded_by_depth_limit() {
        // MAX_DEPTH open marks are fine to build up (each must close as a container)...
        let mut ok = vec![0x80, 0x03];
        ok.extend(std::iter::repeat_n(b'(', MAX_DEPTH as usize - 1));
        ok.extend(std::iter::repeat_n(b't', MAX_DEPTH as usize - 1));
        ok.push(b'.');
        assert!(decode_pickle(&ok).is_ok());
        // ... one more is refused at the MARK, before any vector grows further
        let mut bad = vec![0x80, 0x03];
        bad.extend(std::iter::repeat_n(b'(', MAX_DEPTH as usize + 1));
        let err = decode_pickle(&bad).unwrap_err();
        assert!(matches!(err, CodecError::InvalidData(ref m) if m.contains("nesting depth")), "{err:?}");
        SCRATCH.with(|c| assert!(c.borrow().marks.capacity() <= MAX_SCRATCH_ELEMS));
    }

    #[test]
    fn test_dirty_memo_resolves_newest_binding() {
        // list bound to memo 0, MARK, another list bound to 0, APPEND (dirty), BINGET 0:
        // CPython gives ([1], [1]); the newest binding of index 0 must win
        let data: &[u8] = b"\x80\x03]q\x00(]q\x00K\x01ah\x00t.";
        let one = PickleValue::List(vec![PickleValue::Int(1)]);
        assert_eq!(
            decode_pickle(data).unwrap(),
            PickleValue::Tuple(vec![one.clone(), one.clone()])
        );
        // same within one frame
        let data: &[u8] = b"\x80\x03]q\x00]q\x00K\x01ah\x00\x86.";
        assert_eq!(decode_pickle(data).unwrap(), PickleValue::Tuple(vec![one.clone(), one]));
    }
}

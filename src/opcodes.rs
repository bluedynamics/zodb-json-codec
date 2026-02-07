/// Pickle protocol opcodes (protocol 0-5).
/// We focus on protocol 2-3 which ZODB typically uses,
/// but include protocol 4-5 opcodes for future support.
///
/// Reference: Python pickletools.py and PEP 3154 (protocol 4), PEP 574 (protocol 5)

// -- Protocol 0/1 (text-based, legacy) --
pub const MARK: u8 = b'('; // push special markobject on stack
pub const STOP: u8 = b'.'; // every pickle ends with STOP
pub const POP: u8 = b'0'; // discard topmost stack item
pub const DUP: u8 = b'2'; // duplicate top stack item
pub const FLOAT: u8 = b'F'; // push float; decimal string argument
pub const INT: u8 = b'I'; // push integer or bool
pub const LONG: u8 = b'L'; // push long; decimal string argument
pub const NONE: u8 = b'N'; // push None
pub const REDUCE: u8 = b'R'; // apply callable to argtuple, both on stack
pub const STRING: u8 = b'S'; // push string; NL-terminated string argument
pub const UNICODE: u8 = b'V'; // push Unicode string; NL-terminated UTF-8
pub const APPEND: u8 = b'a'; // append stack top to list below it
pub const BUILD: u8 = b'b'; // call __setstate__ or update __dict__
pub const GLOBAL: u8 = b'c'; // push class/callable by module\nname\n
pub const DICT: u8 = b'd'; // build a dict from stack items
pub const EMPTY_DICT: u8 = b'}'; // push empty dict
pub const APPENDS: u8 = b'e'; // extend list on stack by topmost slice
pub const GET: u8 = b'g'; // push item from memo by string index
pub const LIST: u8 = b'l'; // build list from topmost stack slice
pub const EMPTY_LIST: u8 = b']'; // push empty list
pub const PUT: u8 = b'p'; // store stack top in memo by string index
pub const SETITEM: u8 = b's'; // add key+value pair to dict
pub const TUPLE: u8 = b't'; // build tuple from topmost stack slice
pub const EMPTY_TUPLE: u8 = b')'; // push empty tuple
pub const SETITEMS: u8 = b'u'; // modify dict by adding topmost key+value pairs
pub const PERSID: u8 = b'P'; // push persistent id (string arg)
pub const BINPERSID: u8 = b'Q'; // push persistent id from stack

// -- Protocol 1 (binary) --
pub const BININT: u8 = b'J'; // push 4-byte signed int
pub const BININT1: u8 = b'K'; // push 1-byte unsigned int
pub const BININT2: u8 = b'M'; // push 2-byte unsigned int
pub const BINSTRING: u8 = b'T'; // push string; counted binary string
pub const SHORT_BINSTRING: u8 = b'U'; // push string; counted binary string <= 255 bytes
pub const BINUNICODE: u8 = b'X'; // push Unicode string; counted UTF-8 string
// Note: SHORT_BINUNICODE (0x8c) is protocol 4 only. Use BINUNICODE (0x58) for protocol 3.
pub const BINGET: u8 = b'h'; // push item from memo by 1-byte index
pub const LONG_BINGET: u8 = b'j'; // push item from memo by 4-byte index
pub const BINPUT: u8 = b'q'; // store stack top in memo by 1-byte index
pub const LONG_BINPUT: u8 = b'r'; // store stack top in memo by 4-byte index
pub const BINFLOAT: u8 = b'G'; // push float; binary 8-byte IEEE
pub const BINBYTES: u8 = b'B'; // push bytes; counted binary
pub const SHORT_BINBYTES: u8 = b'C'; // push bytes; counted <= 255

// -- Protocol 2 --
pub const PROTO: u8 = 0x80; // identify pickle protocol
pub const NEWOBJ: u8 = 0x81; // build object by applying cls.__new__ to argtuple
pub const TUPLE1: u8 = 0x85; // build 1-tuple from top of stack
pub const TUPLE2: u8 = 0x86; // build 2-tuple from top two stack items
pub const TUPLE3: u8 = 0x87; // build 3-tuple from top three stack items
pub const NEWTRUE: u8 = 0x88; // push True
pub const NEWFALSE: u8 = 0x89; // push False
pub const LONG1: u8 = 0x8a; // push long from < 256 bytes
pub const LONG4: u8 = 0x8b; // push really big long

// -- Protocol 3 --
// (no new opcodes, just allows BINBYTES)

// -- Protocol 4 --
pub const SHORT_BINUNICODE: u8 = 0x8c; // 1-byte length unicode (protocol 4+ only!)
pub const BINUNICODE8: u8 = 0x8d; // 8-byte length unicode
pub const BINBYTES8: u8 = 0x8e; // 8-byte length bytes
pub const EMPTY_SET: u8 = 0x8f; // push empty set
pub const ADDITEMS: u8 = 0x90; // add items to set
pub const FROZENSET: u8 = 0x91; // build frozenset from items on stack
pub const NEWOBJ_EX: u8 = 0x92; // like NEWOBJ but with kwargs
pub const STACK_GLOBAL: u8 = 0x93; // like GLOBAL but takes args from stack
pub const MEMOIZE: u8 = 0x94; // store top in memo (auto-incrementing key)
pub const FRAME: u8 = 0x95; // framing for protocol 4+

// -- Protocol 5 --
pub const BYTEARRAY8: u8 = 0x96; // push bytearray
pub const NEXT_BUFFER: u8 = 0x97; // push next out-of-band buffer
pub const READONLY_BUFFER: u8 = 0x98; // make top-of-stack read-only

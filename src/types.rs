use num_bigint::BigInt;

/// Intermediate representation of a pickle value.
/// This AST sits between pickle bytes and JSON — it can be losslessly
/// converted in both directions.
#[derive(Debug, Clone, PartialEq)]
pub enum PickleValue {
    None,
    Bool(bool),
    Int(i64),
    BigInt(BigInt),
    Float(f64),
    String(String),
    Bytes(Vec<u8>),
    List(Vec<PickleValue>),
    Tuple(Vec<PickleValue>),
    Dict(Vec<(PickleValue, PickleValue)>),
    Set(Vec<PickleValue>),
    FrozenSet(Vec<PickleValue>),
    /// A class/callable reference: (module, qualname)
    Global {
        module: String,
        name: String,
    },
    /// An object instance: class + state (result of BUILD or REDUCE+BUILD)
    Instance {
        module: String,
        name: String,
        state: Box<PickleValue>,
    },
    /// ZODB persistent reference (the argument to BINPERSID)
    PersistentRef(Box<PickleValue>),
    /// Result of REDUCE that we don't have a specific handler for.
    /// Stores the callable Global and args tuple.
    Reduce {
        callable: Box<PickleValue>,
        args: Box<PickleValue>,
    },
    /// Escape hatch: raw pickle bytes we couldn't meaningfully decode
    RawPickle(Vec<u8>),
}

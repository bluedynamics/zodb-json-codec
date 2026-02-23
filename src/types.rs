use num_bigint::BigInt;

/// Data for an object instance (result of BUILD or REDUCE+BUILD).
/// Boxed inside the enum to keep PickleValue small.
#[derive(Debug, Clone, PartialEq)]
pub struct InstanceData {
    pub module: String,
    pub name: String,
    pub state: Box<PickleValue>,
    /// Dict items set via SETITEMS/SETITEM after BUILD (dict subclasses)
    pub dict_items: Option<Box<Vec<(PickleValue, PickleValue)>>>,
    /// List items appended via APPENDS/APPEND after BUILD (list subclasses)
    pub list_items: Option<Box<Vec<PickleValue>>>,
}

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
    /// An object instance: class + state (boxed to reduce enum size)
    Instance(Box<InstanceData>),
    /// ZODB persistent reference (the argument to BINPERSID)
    PersistentRef(Box<PickleValue>),
    /// Result of REDUCE that we don't have a specific handler for.
    /// Stores the callable Global and args tuple.
    Reduce {
        callable: Box<PickleValue>,
        args: Box<PickleValue>,
        /// Dict items set via SETITEMS/SETITEM after REDUCE (dict subclasses)
        dict_items: Option<Box<Vec<(PickleValue, PickleValue)>>>,
        /// List items appended via APPENDS/APPEND after REDUCE (list subclasses)
        list_items: Option<Box<Vec<PickleValue>>>,
    },
    /// Escape hatch: raw pickle bytes we couldn't meaningfully decode
    RawPickle(Vec<u8>),
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::size_of;

    #[test]
    fn test_pickle_value_size() {
        let pv_size = size_of::<PickleValue>();
        eprintln!("sizeof(PickleValue) = {} bytes", pv_size);
        eprintln!("sizeof(InstanceData) = {} bytes", size_of::<InstanceData>());
        eprintln!("sizeof(String) = {} bytes", size_of::<String>());
        eprintln!("sizeof(Vec<u8>) = {} bytes", size_of::<Vec<u8>>());
        eprintln!("sizeof(Box<PickleValue>) = {} bytes", size_of::<Box<PickleValue>>());
        eprintln!("sizeof(BigInt) = {} bytes", size_of::<num_bigint::BigInt>());
        // Enum should be <= 56 bytes (Global at 48 is now the largest variant)
        assert!(pv_size <= 56, "PickleValue enum too large: {} bytes", pv_size);
    }
}

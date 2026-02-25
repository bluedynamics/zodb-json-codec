# Working with ZODB Records

<!-- diataxis: tutorial -->

This tutorial shows you how to decode and encode ZODB's record format, work
with persistent references, and handle BTree state. You will also learn about
the single-pass PostgreSQL API.

## Prerequisites

- Completed the {doc}`Getting Started <getting-started>` tutorial
- ZODB installed (`pip install ZODB`)

## ZODB record format

Every persistent object in ZODB is stored as **two concatenated pickles**:

1. **Class pickle** -- identifies the object's class as a `(module, name)` tuple
2. **State pickle** -- the result of `__getstate__()`, typically a dict

The codec understands this format and produces a single dict with two keys:

- `@cls` -- a `[module, name]` list identifying the class
- `@s` -- the object's state

## Decoding a simple record

Let's build a minimal ZODB-like record by hand and decode it:

```python
import pickle
import zodb_json_codec

def make_zodb_record(module, classname, state):
    """Build a ZODB record from class info and state dict."""
    class_pickle = pickle.dumps((module, classname), protocol=3)
    state_pickle = pickle.dumps(state, protocol=3)
    return class_pickle + state_pickle

# Simulate a Document object with title and count
record = make_zodb_record(
    "myapp.models", "Document",
    {"title": "Hello World", "count": 42, "tags": ["draft", "review"]},
)

result = zodb_json_codec.decode_zodb_record(record)
print(result)
```

Output:

```python
{
    '@cls': ['myapp.models', 'Document'],
    '@s': {
        'title': 'Hello World',
        'count': 42,
        'tags': ['draft', 'review'],
    },
}
```

The `@cls` key tells you this is a `myapp.models.Document` instance. The `@s`
key holds its state dict -- the same data that `__getstate__()` would return.

## Decoding a real ZODB object

With a running ZODB database, you can load the raw record bytes and decode
them:

```python
from ZODB import DB
from persistent.mapping import PersistentMapping
import transaction
import zodb_json_codec

# Create an in-memory ZODB database
db = DB(None)
conn = db.open()
root = conn.root()

# Store a PersistentMapping
root["users"] = PersistentMapping({
    "alice": "admin",
    "bob": "editor",
})
transaction.commit()

# Load raw record bytes from storage
data, _tid = db.storage.load(root["users"]._p_oid)

# Decode with zodb-json-codec
result = zodb_json_codec.decode_zodb_record(data)
print(result["@cls"])
# ['persistent.mapping', 'PersistentMapping']

print(result["@s"])
# {'data': {'alice': 'admin', 'bob': 'editor'}}
```

PersistentMapping stores its contents in a `data` key inside its state dict.
The codec preserves this internal structure exactly.

## Persistent references

When one ZODB object references another, the reference is stored as a
persistent ref. The codec represents these with the `@ref` marker, using
compact 16-character hex OID strings:

```python
from persistent.mapping import PersistentMapping
import transaction

# Create parent and child objects
child = PersistentMapping({"role": "editor"})
root["child"] = child
transaction.commit()

# Decode the root object -- it references 'child'
data, _ = db.storage.load(root._p_oid)
result = zodb_json_codec.decode_zodb_record(data)

# Find the reference to the child object
ref = result["@s"]["data"]["child"]
print(ref)
# {'@ref': '0000000000000002'}
```

The `@ref` value is the 8-byte ZODB OID encoded as a 16-character hex string.
Some references include the class path for direct resolution:

```python
# OID-only reference
{"@ref": "0000000000000002"}

# Reference with class hint
{"@ref": ["0000000000000002", "persistent.mapping.PersistentMapping"]}
```

## Encoding records back

The `encode_zodb_record()` function takes a dict with `@cls` and `@s` keys and
produces the two concatenated pickles that ZODB expects:

```python
# Start with a record dict
record_dict = {
    "@cls": ["myapp.models", "Document"],
    "@s": {"title": "Re-encoded", "count": 99},
}

# Encode to ZODB record bytes
encoded = zodb_json_codec.encode_zodb_record(record_dict)

# Verify by decoding again
decoded = zodb_json_codec.decode_zodb_record(encoded)
assert decoded["@cls"] == ["myapp.models", "Document"]
assert decoded["@s"]["title"] == "Re-encoded"
assert decoded["@s"]["count"] == 99
```

### Full roundtrip

The codec guarantees that decoding a record and encoding it back produces
equivalent data:

```python
# Load a real record
original_data, _ = db.storage.load(root["users"]._p_oid)

# Decode
decoded = zodb_json_codec.decode_zodb_record(original_data)

# Encode back
re_encoded = zodb_json_codec.encode_zodb_record(decoded)

# Decode both and compare
result1 = zodb_json_codec.decode_zodb_record(original_data)
result2 = zodb_json_codec.decode_zodb_record(re_encoded)
assert result1 == result2
```

## BTree state

The BTrees package (OOBTree, IIBTree, IOBTree, etc.) stores state as deeply
nested tuples. The codec flattens these into human-readable JSON using the
`@kv` and `@ks` markers.

### Map types use `@kv`

BTree and Bucket objects store key-value pairs:

```python
# Simulate a small OOBTree record
# (ZODB stores small BTrees as 4-level nested tuples)
record = make_zodb_record(
    "BTrees.OOBTree", "OOBTree",
    (((("alpha", 1, "beta", 2, "gamma", 3),),),),
)

result = zodb_json_codec.decode_zodb_record(record)
print(result["@cls"])
# ['BTrees.OOBTree', 'OOBTree']

print(result["@s"])
# {'@kv': [['alpha', 1], ['beta', 2], ['gamma', 3]]}
```

The `@kv` marker holds an array of `[key, value]` pairs. This flattened format
is queryable as PostgreSQL JSONB and far easier to read than the original
4-level tuple nesting.

### Set types use `@ks`

TreeSet and Set objects store only keys:

```python
record = make_zodb_record(
    "BTrees.IIBTree", "IITreeSet",
    ((((10, 20, 30),),),),
)

result = zodb_json_codec.decode_zodb_record(record)
print(result["@s"])
# {'@ks': [10, 20, 30]}
```

### Empty BTrees

An empty BTree has `None` as its state:

```python
record = make_zodb_record("BTrees.OOBTree", "OOBTree", None)
result = zodb_json_codec.decode_zodb_record(record)
print(result["@s"])
# None
```

### Large BTrees

When a BTree grows large enough to split into internal nodes, the state
contains persistent references to child buckets. The codec represents this
with `@children` and `@first` markers:

```python
{
    "@cls": ["BTrees.OOBTree", "OOBTree"],
    "@s": {
        "@children": [
            {"@ref": "0000000000000005"},
            "separator_key",
            {"@ref": "0000000000000006"},
        ],
        "@first": {"@ref": "0000000000000004"},
    },
}
```

The `@first` reference points to the first leaf bucket in the linked list.
The `@children` array alternates between child references and separator keys.

## Various state shapes

Not all ZODB objects use dict state. The codec handles whatever
`__getstate__()` returns:

```python
# Dict state (most common) -- PersistentMapping, custom Persistent classes
{"@cls": ["myapp", "Doc"], "@s": {"title": "Hello"}}

# Tuple state -- DateTime objects
{"@cls": ["DateTime.DateTime", "DateTime"],
 "@s": {"@t": [1736937000000000, False, "UTC"]}}

# Scalar state -- BTrees.Length
{"@cls": ["BTrees.Length", "Length"], "@s": 42}

# None state -- empty BTree
{"@cls": ["BTrees.OOBTree", "OOBTree"], "@s": None}
```

## Single-pass PostgreSQL API

For storage backends that need class info, state, and persistent references in
a single call, the codec provides `decode_zodb_record_for_pg()`:

```python
mod, name, state, refs = zodb_json_codec.decode_zodb_record_for_pg(data)
```

This returns a 4-tuple:

- `mod` (str) -- the module name (e.g., `"persistent.mapping"`)
- `name` (str) -- the class name (e.g., `"PersistentMapping"`)
- `state` (dict) -- the decoded state (same as `@s` from `decode_zodb_record`)
- `refs` (list[int]) -- all persistent reference OIDs as integers

```python
# Encode a record with persistent references
record_dict = {
    "@cls": ["myapp", "Container"],
    "@s": {
        "child_a": {"@ref": "0000000000000001"},
        "child_b": {"@ref": "0000000000000002"},
    },
}
data = zodb_json_codec.encode_zodb_record(record_dict)

# Single-pass decode
mod, name, state, refs = zodb_json_codec.decode_zodb_record_for_pg(data)
print(mod, name)
# myapp Container

print(state)
# {'child_a': {'@ref': '0000000000000001'},
#  'child_b': {'@ref': '0000000000000002'}}

print(sorted(refs))
# [1, 2]
```

The `refs` list contains integer OIDs extracted from all `@ref` markers in
the state tree. This is used by PostgreSQL storage backends for the `refs`
column that enables pure-SQL garbage collection (pack).

The PostgreSQL variant also sanitizes null bytes in strings (which PostgreSQL
JSONB cannot store) by replacing them with `{"@ns": "<base64>"}` markers.

## Cleanup

```python
transaction.abort()
conn.close()
db.close()
```

## What's next

You now know how to decode and encode ZODB records, work with persistent
references, and handle BTree state. For the complete list of markers and type
mappings, see the {doc}`reference documentation </reference/index>`.

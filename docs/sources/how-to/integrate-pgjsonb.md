# Integrate with zodb-pgjsonb

<!-- diataxis: how-to -->

The [zodb-pgjsonb](https://github.com/bluedynamics/zodb-pgjsonb) storage backend uses zodb-json-codec to transcode ZODB pickle records into PostgreSQL JSONB.
This guide explains the codec functions designed for that integration.

## The data pipeline

When a ZODB object is stored, its pickle bytes flow through the codec into PostgreSQL:

```
pickle bytes --> Rust decode --> JSON --> PostgreSQL JSONB column
```

On read, the reverse path reconstructs the original pickle bytes:

```
PostgreSQL JSONB --> JSON --> Rust encode --> pickle bytes
```

## Two fast paths for decoding

The codec provides two PG-specific decode functions.
Both handle null-byte sanitization and persistent reference extraction in a single pass.

### Python dict path

`decode_zodb_record_for_pg()` returns a Python dict.
The GIL is released during the Rust pickle-parsing phase, then reacquired to build the Python dict.

```python
from zodb_json_codec import decode_zodb_record_for_pg

class_mod, class_name, state_dict, refs = decode_zodb_record_for_pg(pickle_data)
# class_mod:   "persistent.mapping" (str)
# class_name:  "PersistentMapping" (str)
# state_dict:  {"data": {"key": "value"}} (dict)
# refs:        [123456789, ...] (list of int OIDs)
```

Use this path when you need to inspect or transform the state in Python before writing to the database (for example, extracting extra columns via a state processor).

### Direct JSON string path

`decode_zodb_record_for_pg_json()` returns a JSON string directly.
The **entire** pipeline -- pickle decode, JSON serialization, null-byte sanitization, and ref extraction -- runs in Rust with the GIL released.

```python
from zodb_json_codec import decode_zodb_record_for_pg_json

class_mod, class_name, json_str, refs = decode_zodb_record_for_pg_json(pickle_data)
# json_str is a ready-to-insert JSON string
```

This is the fastest path: no intermediate Python dicts are allocated, and other Python threads can run during the entire operation.

## Null-byte sanitization

PostgreSQL JSONB cannot store `\u0000` (null bytes) in strings.
Both PG decode functions automatically replace strings containing null bytes with `{"@ns": "<base64>"}` markers.
On encode, these markers are transparently converted back to the original byte sequences.

## Persistent reference extraction

The `refs` list returned by both functions contains all persistent reference OIDs found in the object state, as Python integers (big-endian interpretation of the 8-byte ZODB OID).
Cross-database references with non-standard OID sizes are silently skipped.

zodb-pgjsonb stores these in a `refs` column for pure-SQL garbage collection (pack) without needing to deserialize the JSON.

## Encoding back to pickle

To reconstruct ZODB pickle bytes from a JSON record:

```python
from zodb_json_codec import encode_zodb_record

record = {
    "@cls": ["persistent.mapping", "PersistentMapping"],
    "@s": {"data": {"key": "value"}},
}
pickle_bytes = encode_zodb_record(record)
```

The encoder produces two concatenated pickles (class pickle + state pickle) in protocol 3 format, matching ZODB's expected record layout.

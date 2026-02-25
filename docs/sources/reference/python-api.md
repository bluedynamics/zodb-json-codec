# Python API

<!-- diataxis: reference -->

All public functions are exported from the top-level `zodb_json_codec`
package. The implementation is a compiled Rust extension (via PyO3);
there are no pure-Python fallbacks.

```python
import zodb_json_codec
```

## ZODB Record Functions

These functions work with ZODB's two-pickle record format: a class pickle
followed by a state pickle, concatenated as a single `bytes` object.

### `decode_zodb_record`

```python
decode_zodb_record(data: bytes) -> dict
```

Decode a ZODB two-pickle record into a Python dict with marker keys.

The GIL is released during the pure-Rust pickle parsing phase, allowing
other Python threads to run concurrently.

Parameters
: `data`
  : Raw bytes of a ZODB record (two concatenated pickles).

Returns
: A dict with two keys:

  `"@cls"`
  : A list of two strings: `[module, class_name]`.

  `"@s"`
  : The object state. Typically a dict, but can be any JSON-representable
    value (including `None` for empty BTrees). BTree state is
    automatically flattened using `@kv`/`@ks` markers.

Raises
: `ValueError`
  : If the pickle data is malformed, uses unsupported opcodes, or
    exceeds safety limits.

Example:

```python
record = decode_zodb_record(raw_bytes)
# {'@cls': ['persistent.mapping', 'PersistentMapping'],
#  '@s': {'data': {'title': 'Hello', 'count': 42}}}
```

---

### `encode_zodb_record`

```python
encode_zodb_record(record: dict) -> bytes
```

Encode a Python dict back into a ZODB two-pickle record.

Uses the direct PyObject-to-pickle encoder, bypassing the intermediate
PickleValue AST for maximum speed. The output uses pickle protocol 3,
as required by zodbpickle.

Parameters
: `record`
  : A dict with `"@cls"` (list of `[module, name]`) and `"@s"` (state
    value) keys. The state may contain any JSON marker dicts (`@t`,
    `@b`, `@dt`, `@ref`, `@kv`, etc.).

Returns
: Raw bytes of a ZODB record (two concatenated pickles in protocol 3).

Raises
: `ValueError`
  : If `@cls` is missing, not a two-element list of strings, or if the
    state contains values that cannot be encoded.

Example:

```python
raw_bytes = encode_zodb_record({
    '@cls': ['persistent.mapping', 'PersistentMapping'],
    '@s': {'data': {'title': 'Hello', 'count': 42}},
})
```

---

### `decode_zodb_record_for_pg`

```python
decode_zodb_record_for_pg(data: bytes) -> tuple
```

Single-pass decode optimized for PostgreSQL JSONB storage. Combines
pickle decoding, persistent reference extraction, and null-byte
sanitization in one operation.

The GIL is released during the pure-Rust pickle parsing and reference
extraction phases.

Parameters
: `data`
  : Raw bytes of a ZODB record (two concatenated pickles).

Returns
: A 4-tuple:

  `class_mod` (`str`)
  : The module name from the class pickle (e.g., `"persistent.mapping"`).

  `class_name` (`str`)
  : The class name from the class pickle (e.g., `"PersistentMapping"`).

  `state` (`dict`)
  : The decoded object state as a Python dict with marker keys. Strings
    containing null bytes (`\x00`) are replaced with `{"@ns": base64}`
    markers, because PostgreSQL JSONB cannot store `\u0000`.

  `refs` (`list[int]`)
  : All persistent reference OIDs found in the state, as integers. Used
    for the `refs` column in SQL-based garbage collection (pack).

Raises
: `ValueError`
  : If the pickle data is malformed.

Example:

```python
mod, name, state, refs = decode_zodb_record_for_pg(raw_bytes)
# mod = 'persistent.mapping'
# name = 'PersistentMapping'
# state = {'data': {'title': 'Hello'}}
# refs = [3, 7, 42]
```

---

### `decode_zodb_record_for_pg_json`

```python
decode_zodb_record_for_pg_json(data: bytes) -> tuple
```

Direct JSON string path for PostgreSQL. The entire pipeline -- pickle
parsing, JSON conversion, null-byte sanitization, and reference
extraction -- runs in Rust with the GIL released. No intermediate Python
dicts are created.

This is the fastest path for storing ZODB records in PostgreSQL JSONB
columns: pass the returned JSON string directly to a SQL `INSERT`
parameter.

Parameters
: `data`
  : Raw bytes of a ZODB record (two concatenated pickles).

Returns
: A 4-tuple:

  `class_mod` (`str`)
  : The module name from the class pickle.

  `class_name` (`str`)
  : The class name from the class pickle.

  `state_json` (`str`)
  : The object state serialized as a JSON string, ready for PostgreSQL
    JSONB insertion. Null bytes are sanitized.

  `refs` (`list[int]`)
  : All persistent reference OIDs found in the state, as integers.

Raises
: `ValueError`
  : If the pickle data is malformed.

Example:

```python
mod, name, json_str, refs = decode_zodb_record_for_pg_json(raw_bytes)
# json_str is a ready-to-use JSON string:
# '{"data": {"title": "Hello"}}'
cursor.execute(
    "INSERT INTO object_state (class_mod, class_name, state, refs) "
    "VALUES (%s, %s, %s::jsonb, %s)",
    (mod, name, json_str, refs),
)
```

## Standalone Pickle Functions

These functions work with individual pickle byte streams (not ZODB
two-pickle records). They are useful for general pickle-to-JSON
conversion outside of ZODB.

---

### `pickle_to_dict`

```python
pickle_to_dict(data: bytes) -> dict
```

Decode a single pickle byte stream into a Python dict (or other Python
object) using the direct PickleValue-to-PyObject conversion path.

The GIL is released during the pure-Rust pickle parsing phase.

Parameters
: `data`
  : Raw pickle bytes (protocol 2-3, partial protocol 4).

Returns
: The decoded Python object. Simple pickles return native Python types;
  objects with class information return marker dicts (`@cls`, `@s`,
  `@reduce`, etc.).

Raises
: `ValueError`
  : If the pickle data is malformed.

---

### `dict_to_pickle`

```python
dict_to_pickle(data: dict) -> bytes
```

Encode a Python dict into pickle bytes using the direct
PyObject-to-pickle encoder. This is the inverse of `pickle_to_dict`.

Parameters
: `data`
  : A Python dict, potentially containing JSON marker keys (`@t`, `@b`,
    `@dt`, `@ref`, `@cls` + `@s`, etc.).

Returns
: Pickle bytes in protocol 3 format.

Raises
: `ValueError`
  : If the dict contains values that cannot be encoded, or if recursion
    depth exceeds 1,000 levels.

---

### `pickle_to_json`

```python
pickle_to_json(data: bytes) -> str
```

Convert a single pickle byte stream to a pretty-printed JSON string.
The entire operation runs in Rust with the GIL released.

This goes through the serde_json intermediate representation, producing
human-readable output with indentation.

Parameters
: `data`
  : Raw pickle bytes (protocol 2-3, partial protocol 4).

Returns
: A pretty-printed JSON string.

Raises
: `ValueError`
  : If the pickle data is malformed or cannot be represented in JSON.

---

### `json_to_pickle`

```python
json_to_pickle(data: str) -> bytes
```

Convert a JSON string back to pickle bytes. This is the inverse of
`pickle_to_json`.

All JSON markers (`@t`, `@b`, `@dt`, `@ref`, `@cls` + `@s`, etc.) are
recognized and converted back to the appropriate pickle opcodes.

Parameters
: `data`
  : A JSON string, potentially containing marker objects.

Returns
: Pickle bytes in protocol 3 format.

Raises
: `ValueError`
  : If the JSON is malformed or contains invalid marker structures.

## Error Handling

All functions raise `ValueError` on failure. Common error conditions:

- **Unexpected end of pickle stream** -- truncated input data.
- **Unknown pickle opcode** -- opcode not supported by the decoder.
- **Pickle stack underflow** -- malformed pickle with missing stack
  values.
- **Invalid pickle data** -- structural errors (wrong types, missing
  fields).
- **JSON error** -- serialization or deserialization failures in the JSON
  path.
- **Invalid UTF-8** -- non-UTF-8 bytes in a pickle string.

## Safety Limits

The codec enforces several limits to prevent resource exhaustion from
malicious or malformed pickle data:

- **Memo size:** Maximum 100,000 entries.
- **Recursion depth:** Maximum 1,000 levels (encoder and PyObject
  converter).
- **Binary data size:** BINUNICODE8/BINBYTES8 capped at 256 MB before
  allocation.
- **Integer size:** LONG opcode text limited to 10,000 characters.
- **BTree validation:** Odd-length item lists in BTree buckets are
  rejected.
- **Length validation:** Non-negative lengths enforced for LONG4 and
  BINSTRING opcodes.

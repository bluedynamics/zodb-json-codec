# Getting Started

<!-- diataxis: tutorial -->

This tutorial walks you through installing zodb-json-codec, converting Python
pickles to JSON and back, and understanding the marker format that preserves
type fidelity.

## Prerequisites

- Python 3.10 or later

## Install zodb-json-codec

Install the package from PyPI:

```bash
pip install zodb-json-codec
```

Verify the installation:

```python
>>> import zodb_json_codec
```

## Your first encode and decode

The codec provides two pairs of functions for standalone pickle conversion:

- `pickle_to_json()` / `json_to_pickle()` -- work with JSON strings
- `pickle_to_dict()` / `dict_to_pickle()` -- work with Python dicts directly

Let's start with a simple dictionary.

### Pickle to JSON string

```python
import pickle
import json
import zodb_json_codec

# Create a pickle using protocol 3 (the protocol ZODB uses)
data = {"name": "Alice", "age": 30, "active": True}
pickled = pickle.dumps(data, protocol=3)

# Decode to a JSON string
json_str = zodb_json_codec.pickle_to_json(pickled)
print(json_str)
```

Output:

```json
{
  "active": true,
  "age": 30,
  "name": "Alice"
}
```

Simple Python types -- strings, integers, floats, booleans, `None`, lists, and
string-keyed dicts -- map directly to their JSON equivalents with no markers.

### Pickle to Python dict

The `pickle_to_dict()` function returns a Python dict instead of a JSON string.
This is faster when you need to work with the data in Python:

```python
result = zodb_json_codec.pickle_to_dict(pickled)
print(result)
# {'active': True, 'age': 30, 'name': 'Alice'}
```

## Understanding type markers

Python has types that JSON does not: tuples, bytes, sets, datetimes, and more.
The codec uses single-key marker dicts to preserve these types through the
roundtrip.

### Tuples use `@t`

```python
val = (1, "two", 3.0)
pickled = pickle.dumps(val, protocol=3)
json_str = zodb_json_codec.pickle_to_json(pickled)
print(json_str)
```

Output:

```json
{
  "@t": [1, "two", 3.0]
}
```

The `@t` marker wraps the tuple elements in a JSON array. Without it, there
would be no way to distinguish a tuple from a list after roundtripping.

### Bytes use `@b`

Binary data is base64-encoded:

```python
val = b"\x00\x01\x02\xff"
pickled = pickle.dumps(val, protocol=3)
json_str = zodb_json_codec.pickle_to_json(pickled)
print(json_str)
```

Output:

```json
{
  "@b": "AAEC/w=="
}
```

### Sets use `@set` and `@fset`

```python
val = {1, 2, 3}
pickled = pickle.dumps(val, protocol=3)
result = zodb_json_codec.pickle_to_dict(pickled)
print(result)
# {'@set': [1, 2, 3]}
```

Frozensets use the `@fset` marker:

```python
val = frozenset(["a", "b"])
pickled = pickle.dumps(val, protocol=3)
result = zodb_json_codec.pickle_to_dict(pickled)
print(result)
# {'@fset': ['a', 'b']}
```

### Datetimes use `@dt`, `@date`, `@time`

The codec recognizes common Python types serialized via pickle's REDUCE opcode
and produces human-readable, JSONB-queryable markers:

```python
from datetime import datetime, date, time

# datetime
val = datetime(2025, 6, 15, 12, 30, 45)
pickled = pickle.dumps(val, protocol=3)
result = zodb_json_codec.pickle_to_dict(pickled)
print(result)
# {'@dt': '2025-06-15T12:30:45'}

# date
val = date(2025, 6, 15)
pickled = pickle.dumps(val, protocol=3)
result = zodb_json_codec.pickle_to_dict(pickled)
print(result)
# {'@date': '2025-06-15'}

# time
val = time(12, 30, 45)
pickled = pickle.dumps(val, protocol=3)
result = zodb_json_codec.pickle_to_dict(pickled)
print(result)
# {'@time': '12:30:45'}
```

Other known types include `@td` (timedelta), `@dec` (Decimal), and `@uuid`
(UUID). See the {doc}`type mapping reference </reference/index>` for the
complete list.

### Dicts with non-string keys use `@d`

JSON objects only support string keys. When a Python dict has non-string keys,
the codec uses an array-of-pairs representation:

```python
val = {1: "a", 2: "b"}
pickled = pickle.dumps(val, protocol=3)
result = zodb_json_codec.pickle_to_dict(pickled)
print(result)
# {'@d': [[1, 'a'], [2, 'b']]}
```

## Roundtrip verification

A key property of the codec is **roundtrip fidelity**: encoding to JSON and
back produces identical pickle bytes.

```python
import pickle
import zodb_json_codec

# Start with a value that exercises multiple markers
original = {
    "name": "Alice",
    "scores": (95, 87, 92),
    "avatar": b"\x89PNG\r\n",
    "active": True,
    "tags": ["staff", "admin"],
}
pickled = pickle.dumps(original, protocol=3)

# Roundtrip via JSON string
json_str = zodb_json_codec.pickle_to_json(pickled)
restored_pickle = zodb_json_codec.json_to_pickle(json_str)
assert pickled == restored_pickle  # identical bytes

# Roundtrip via Python dict
as_dict = zodb_json_codec.pickle_to_dict(pickled)
restored_pickle2 = zodb_json_codec.dict_to_pickle(as_dict)
assert pickled == restored_pickle2  # identical bytes
```

Both paths -- JSON string and Python dict -- produce the exact same pickle
bytes as the original. This guarantee means you can transcode ZODB data to JSON
for storage and querying, then reconstruct the original pickle when ZODB needs
it back.

## Nested structures

Markers compose naturally inside larger structures:

```python
val = {
    "items": [(1, "a"), (2, "b")],
    "metadata": {"created": b"\x01\x02"},
}
pickled = pickle.dumps(val, protocol=3)
json_str = zodb_json_codec.pickle_to_json(pickled)
print(json_str)
```

Output:

```json
{
  "items": [
    {"@t": [1, "a"]},
    {"@t": [2, "b"]}
  ],
  "metadata": {
    "created": {"@b": "AQI="}
  }
}
```

Tuples inside a list each get their own `@t` marker, and bytes inside a nested
dict get a `@b` marker. The structure is human-readable and fully queryable
when stored as PostgreSQL JSONB.

## What's next

Now that you understand the basics, move on to
{doc}`Working with ZODB Records <zodb-records>` to learn how to decode and
encode the two-pickle format that ZODB uses for persistent object storage.

# Type Mapping Reference

This document describes how Python types are represented in JSON by the
`zodb-json-codec`. All representations are designed to be **roundtrip-safe**:
encoding to JSON and decoding back produces identical pickle bytes.

## Native JSON Types

These Python types map directly to JSON without markers:

| Python Type | JSON | Example |
|---|---|---|
| `str` | string | `"hello"` |
| `int` | number | `42` |
| `float` | number | `3.14` |
| `bool` | boolean | `true` / `false` |
| `None` | null | `null` |
| `list` | array | `[1, 2, 3]` |
| `dict` (string keys) | object | `{"key": "value"}` |

## Structural Markers

These markers preserve Python types that have no direct JSON equivalent:

### `@t` — Tuple

```json
{"@t": [1, 2, 3]}
```

Python: `(1, 2, 3)`

### `@b` — Bytes

Base64-encoded binary data.

```json
{"@b": "AQID/w=="}
```

Python: `b'\x01\x02\x03\xff'`

### `@bi` — BigInt

Integers that exceed JSON's safe integer range, stored as strings.

```json
{"@bi": "123456789012345678901234567890"}
```

### `@d` — Dict with non-string keys

Array-of-pairs representation for dicts with non-string keys.

```json
{"@d": [[1, "a"], [2, "b"]]}
```

Python: `{1: "a", 2: "b"}`

### `@set` — Set

```json
{"@set": [1, 2, 3]}
```

Python: `{1, 2, 3}`

Note: In ZODB's pickle protocol 3, sets are serialized via REDUCE. The codec
recognizes this pattern and produces the `@set` marker.

### `@fset` — Frozenset

```json
{"@fset": [1, 2, 3]}
```

Python: `frozenset([1, 2, 3])`

## Known Type Markers

These markers provide human-readable, JSONB-queryable representations for
common Python types that are stored via pickle's REDUCE opcode:

### `@dt` — datetime.datetime

ISO 8601 format. Naive datetimes have no offset; tz-aware datetimes include
the UTC offset.

```json
{"@dt": "2025-06-15T12:30:45"}
{"@dt": "2025-06-15T12:30:45.123456"}
{"@dt": "2025-06-15T12:00:00+00:00"}
{"@dt": "2025-06-15T12:00:00+05:30"}
{"@dt": "2025-06-15T12:00:00-05:00"}
```

Python: `datetime(2025, 6, 15, 12, 30, 45)`

**Timezone handling:**

| Timezone Source | JSON Form |
|---|---|
| Naive (no tz) | `{"@dt": "2025-01-01T00:00:00"}` |
| `datetime.timezone.utc` | `{"@dt": "2025-01-01T00:00:00+00:00"}` |
| `datetime.timezone(offset)` | `{"@dt": "2025-01-01T00:00:00+05:30"}` |
| `pytz.utc` | `{"@dt": "2025-01-01T00:00:00+00:00"}` |
| `pytz.timezone("US/Eastern")` | `{"@dt": "2025-01-01T00:00:00", "@tz": {"name": "US/Eastern", "pytz": [...]}}` |
| `zoneinfo.ZoneInfo("US/Eastern")` | `{"@dt": "2025-01-01T00:00:00", "@tz": {"zoneinfo": "US/Eastern"}}` |

For pytz named timezones, the `@tz.pytz` array preserves the full constructor
arguments (name, UTC offset in seconds, DST offset, abbreviation) for exact
roundtrip fidelity. For zoneinfo, only the zone key is needed.

### `@date` — datetime.date

ISO 8601 date format.

```json
{"@date": "2025-06-15"}
```

Python: `date(2025, 6, 15)`

### `@time` — datetime.time

ISO 8601 time format. Microseconds are included only when non-zero.

```json
{"@time": "12:30:45"}
{"@time": "12:30:45.123456"}
```

Python: `time(12, 30, 45)`

### `@td` — datetime.timedelta

Array of `[days, seconds, microseconds]`.

```json
{"@td": [7, 3600, 500000]}
```

Python: `timedelta(days=7, seconds=3600, microseconds=500000)`

### `@dec` — decimal.Decimal

String representation preserving exact decimal value.

```json
{"@dec": "3.14159"}
{"@dec": "Infinity"}
{"@dec": "NaN"}
```

Python: `Decimal("3.14159")`

### `@uuid` — uuid.UUID

Standard UUID string format (8-4-4-4-12 hex digits).

```json
{"@uuid": "12345678-1234-5678-1234-567812345678"}
```

Python: `uuid.UUID("12345678-1234-5678-1234-567812345678")`

## ZODB-Specific Markers

### `@cls` — Class reference

Module and class name pair, used for ZODB class pickle and GLOBAL opcode.

```json
{"@cls": ["myapp.models", "Document"]}
```

### `@s` — Object state

The state dict (or other value) from `__getstate__()`. Always paired with
`@cls` in a ZODB record.

```json
{
  "@cls": ["myapp.models", "Document"],
  "@s": {"title": "Hello", "count": 42}
}
```

### `@ref` — Persistent reference

ZODB persistent object reference, using hex OID format.

```json
{"@ref": "0000000000000003"}
{"@ref": ["0000000000000003", "myapp.models.Document"]}
```

The first form is an OID-only reference (class resolved at load time).
The second form includes the class path for direct resolution.

## BTree Markers

BTrees from the `BTrees` package (OOBTree, IIBTree, IOBTree, etc.) store their
state as deeply nested tuples. The codec flattens these into human-readable,
JSONB-queryable JSON while preserving full roundtrip fidelity.

### Supported BTree classes

All classes matching the pattern `BTrees.{PREFIX}BTree.{PREFIX}{Type}` are
recognized, where type is one of `BTree`, `Bucket`, `TreeSet`, or `Set`.
Prefixes include: `OO`, `IO`, `OI`, `II`, `LO`, `OL`, `LL`, `LF`, `IF`,
`QQ`, and `fs`.

`BTrees.Length.Length` stores a plain integer and needs no special handling.

### `@kv` — Key-value pairs (map types)

Used for BTree, Bucket, OOBTree, IIBTree, etc. — any map-type BTree node.
Array of `[key, value]` pairs.

```json
{"@cls": ["BTrees.OOBTree", "OOBTree"],
 "@s": {"@kv": [["a", 1], ["b", 2], ["c", 3]]}}
```

```json
{"@cls": ["BTrees.IIBTree", "IIBTree"],
 "@s": {"@kv": [[1, 100], [2, 200]]}}
```

```json
{"@cls": ["BTrees.OOBTree", "OOBucket"],
 "@s": {"@kv": [["x", 10], ["y", 20]]}}
```

### `@ks` — Keys only (set types)

Used for TreeSet and Set nodes. Plain array of keys.

```json
{"@cls": ["BTrees.IIBTree", "IITreeSet"],
 "@s": {"@ks": [1, 2, 3]}}
```

```json
{"@cls": ["BTrees.OOBTree", "OOSet"],
 "@s": {"@ks": ["a", "b", "c"]}}
```

### `@next` — Next bucket/set in linked list

When a Bucket or Set has a `next` pointer (linked list of leaf nodes in a
large BTree), the next persistent reference is included.

```json
{"@cls": ["BTrees.OOBTree", "OOBucket"],
 "@s": {"@kv": [["a", 1], ["b", 2]], "@next": {"@ref": "0000000000000003"}}}
```

### `@children` + `@first` — Large BTree internal nodes

When a BTree is too large for a single bucket, it splits into internal nodes
with persistent references to child buckets. The `@children` array contains
alternating child references and separator keys. `@first` points to the first
bucket in the leaf chain.

```json
{"@cls": ["BTrees.OOBTree", "OOBTree"],
 "@s": {"@children": [{"@ref": "..."}, "separator_key", {"@ref": "..."}],
        "@first": {"@ref": "..."}}}
```

### Empty BTree

An empty BTree has `null` state:

```json
{"@cls": ["BTrees.OOBTree", "OOBTree"], "@s": null}
```

### BTrees.Length

Length objects store a plain integer, no special markers needed:

```json
{"@cls": ["BTrees.Length", "Length"], "@s": 42}
```

## Fallback Markers

### `@reduce` — Generic REDUCE

For REDUCE operations not handled by a known type handler. Preserves the
callable and arguments for roundtripping.

```json
{
  "@reduce": {
    "callable": {"@cls": ["some.module", "SomeClass"]},
    "args": {"@t": ["arg1", "arg2"]}
  }
}
```

### `@inst` — Anonymous instance

For BUILD operations where the class couldn't be identified.

```json
{"@inst": {"@state": ...}}
```

### `@pkl` — Raw pickle escape hatch

Base64-encoded pickle fragment for types that can't be represented in JSON.
This is the "never fails" fallback — any pickle data can roundtrip through
this marker.

```json
{"@pkl": "gAJjc29tZS5tb2R1bGUKU29tZUNsYXNzCnEAKVxxAX0="}
```

## Marker Priority

When decoding JSON back to pickle, markers are checked in this order:

1. `@t` (tuple)
2. `@b` (bytes)
3. `@bi` (bigint)
4. `@d` (non-string-key dict)
5. `@set` (set)
6. `@fset` (frozenset)
7. `@ref` (persistent reference)
8. `@pkl` (raw pickle)
9. `@dt`, `@date`, `@time`, `@td`, `@dec`, `@uuid` (known types)
10. `@cls` + `@s` (instance)
11. `@cls` alone (global reference)
12. `@reduce` (generic reduce)
13. Plain JSON object → Python dict

## Backward Compatibility

If JSON data was stored using the generic `@reduce` format for types that now
have dedicated markers (e.g., datetime stored as `@reduce` before Phase 3),
the decoder still handles `@reduce` correctly. The new markers only affect the
forward direction (pickle → JSON).

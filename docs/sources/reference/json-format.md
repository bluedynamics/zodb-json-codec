# JSON format

<!-- diataxis: reference -->

This page describes how Python types are represented in JSON by
zodb-json-codec.
All representations are **round-trip safe**: encoding to JSON and decoding
back yields a pickle that unpickles to an equal object. The bytes are not
identical to the input: memo puts (`BINPUT`, `LONG_BINPUT`, `MEMOIZE`) for
values that are never read back are dropped, the protocol header is
rewritten, a ZODB class pickle is written in the `((module, name), None)`
form, and shared sub-objects come back as equal copies. The string API
`pickle_to_json` sorts dict keys and `json_to_pickle` writes protocol 3; the
record functions (`decode_zodb_record`, `decode_zodb_record_for_pg_json`,
`encode_zodb_record`) and `dict_to_pickle` keep key order and write
protocol 2.

## Native JSON types

These Python types map directly to JSON without any markers:

| Python Type | JSON Type | Example |
|---|---|---|
| `str` | string | `"hello"` |
| `int` | number | `42` |
| `float` | number | `3.14` |
| `bool` | boolean | `true` / `false` |
| `None` | null | `null` |
| `list` | array | `[1, 2, 3]` |
| `dict` (string keys) | object | `{"key:" "value"}` |

## Structural markers

These markers preserve Python types that have no direct JSON equivalent.
Each uses a single-key dict with a `@`-prefixed key.

### `@t` -- tuple

```json
{"@t": [1, 2, 3]}
```

Python: `(1, 2, 3)`

### `@b` -- Bytes

Base64-encoded binary data.

```json
{"@b": "AQID/w=="}
```

Python: `b'\x01\x02\x03\xff'`

### `@bi` -- BigInt

Integers that exceed JSON's safe integer range, stored as strings.

```json
{"@bi": "123456789012345678901234567890"}
```

Python: `123456789012345678901234567890`

### `@d` -- Dict with Non-String Keys

Array-of-pairs representation for dicts whose keys are not all strings.

```json
{"@d": [[1, "a"], [2, "b"]]}
```

Python: `{1: "a," 2: "b"}`

### `@set` -- Set

```json
{"@set": [1, 2, 3]}
```

Python: `{1, 2, 3}`

In ZODB's pickle protocol 3, sets are serialized via the REDUCE opcode.
The codec recognizes this pattern and produces the `@set` marker.

### `@fset` -- Frozenset

```json
{"@fset": [1, 2, 3]}
```

Python: `frozenset([1, 2, 3])`

## Known Type Markers

These markers provide human-readable, JSONB-queryable representations
for common Python types that are stored via pickle's REDUCE opcode.
Instead of the generic `@reduce` format, each gets a compact,
purpose-built marker.

### `@dt` -- datetime.datetime

ISO 8601 format.
Naive datetimes have no offset; timezone-aware
datetimes include the UTC offset.

```json
{"@dt": "2025-06-15T12:30:45"}
{"@dt": "2025-06-15T12:30:45.123456"}
{"@dt": "2025-06-15T12:00:00+00:00"}
{"@dt": "2025-06-15T12:00:00+05:30"}
{"@dt": "2025-06-15T12:00:00-05:00"}
```

Python: `datetime(2025, 6, 15, 12, 30, 45)`

**Timezone handling:**

Fixed-offset timezones are embedded directly in the ISO 8601 string.
Named timezones (pytz, zoneinfo) use a separate `@tz` key to preserve
the zone name for exact roundtrip fidelity.
These two forms are
mutually exclusive:

| Timezone Source | JSON Form |
|---|---|
| Naive (no tz) | `{"@dt:" "2025-01-01T00:00:00"}` |
| `datetime.timezone.utc` | `{"@dt:" "2025-01-01T00:00:00+00:00"}` |
| `datetime.timezone(offset)` | `{"@dt:" "2025-01-01T00:00:00+05:30"}` |
| `pytz.utc` | `{"@dt:" "2025-01-01T00:00:00+00:00"}` |
| `pytz.timezone("US/Eastern")` | `{"@dt:" "...," "@tz:" {"name:" "US/Eastern," "pytz:" [...]}}` |
| `zoneinfo.ZoneInfo("US/Eastern")` | `{"@dt:" "...," "@tz:" {"zoneinfo:" "US/Eastern"}}` |

For pytz named timezones, the `@tz.pytz` array preserves the full
constructor arguments (name, UTC offset in seconds, DST offset,
abbreviation) for exact roundtrip fidelity.
For zoneinfo, only the zone
key is needed.

### `@date` -- datetime.date

ISO 8601 date format.

```json
{"@date": "2025-06-15"}
```

Python: `date(2025, 6, 15)`

### `@time` -- datetime.time

ISO 8601 time format.
Microseconds are included only when non-zero.

```json
{"@time": "12:30:45"}
{"@time": "12:30:45.123456"}
```

Python: `time(12, 30, 45)`

### `@td` -- datetime.timedelta

Array of `[days, seconds, microseconds]`.

```json
{"@td": [7, 3600, 500000]}
```

Python: `timedelta(days=7, seconds=3600, microseconds=500000)`

### `@dec` -- `decimal.Decimal`

String representation preserving exact decimal value.

```json
{"@dec": "3.14159"}
{"@dec": "Infinity"}
{"@dec": "NaN"}
```

Python: `Decimal("3.14159")`

### `@uuid` -- `uuid.UUID`

Standard UUID string format (8-4-4-4-12 hex digits).

```json
{"@uuid": "12345678-1234-5678-1234-567812345678"}
```

Python: `uuid.UUID("12345678-1234-5678-1234-567812345678")`

## ZODB-Specific Markers

### `@cls` -- Class Reference

Module and class name pair, used for ZODB class pickle and the GLOBAL
opcode.

```json
{"@cls": ["myapp.models", "Document"]}
```

### `@s` -- Object State

The state value from `__getstate__()`.
Always paired with `@cls` in a
ZODB record.

```json
{
  "@cls": ["myapp.models", "Document"],
  "@s": {"title": "Hello", "count": 42}
}
```

### `@ref` -- Persistent Reference

ZODB persistent object reference, using hex OID format (16 hex digits,
zero-padded).

```json
{"@ref": "0000000000000003"}
{"@ref": ["0000000000000003", "myapp.models.Document"]}
```

The first form is an OID-only reference (class resolved at load time).
The second form includes the class path for direct resolution.

## Fallback Markers

### `@reduce` -- Generic REDUCE

For REDUCE operations not handled by a known type handler.
Preserves the
callable and arguments for roundtripping.

```json
{
  "@reduce": {
    "callable": {"@cls": ["some.module", "SomeClass"]},
    "args": {"@t": ["arg1", "arg2"]}
  }
}
```

Optional keys inside `@reduce`:

`items`, `appends`
: Contents of dict/list subclasses set through `SETITEMS`/`APPENDS` after the
  call (`OrderedDict`, `defaultdict`, `deque`, user subclasses).

`newobj`
: `true` when the object was created with `NEWOBJ` (`cls.__new__(cls, *args)`)
  instead of calling the callable (`REDUCE`). Absent means `REDUCE`.

`state`
: `BUILD` state that followed the call: after a `REDUCE` with non-empty
  arguments, or after a `REDUCE`/`NEWOBJ` whose callable is not a global (the
  result of another `REDUCE`, for example a `functools.partial`), with any
  arguments.

```json
{
  "@reduce": {
    "callable": {"@cls": ["some.module", "SomeClass"]},
    "args": {"@t": ["arg1"]},
    "state": {"extra": 1}
  }
}
```

Instances created with `NEWOBJ` and non-empty constructor arguments
(`__getnewargs__`) followed by `BUILD` keep the `@cls`/`@s` form; their state
holds both parts:

```json
{
  "@cls": ["some.module", "SomeClass"],
  "@s": {"@args": {"@t": ["arg1"]}, "@state": {"token": "arg1"}}
}
```

`@items` and `@appends` next to `@cls`/`@s` carry dict/list subclass contents of
such instances.

Keys starting with `@` inside an instance state are reserved for the codec:
a state that is exactly `{"@args": ..., "@state": ...}` is always read as
constructor arguments plus state. A `@reduce` with both `newobj` and `state`
is written only when the class itself is not a global (`NEWOBJ` on a class
pickled by value, then `BUILD`); for a global class the `@cls`/`@s` form above
is used. Readers accept both.

### `@ns` -- String with NUL bytes (PostgreSQL storage)

PostgreSQL JSONB cannot store U+0000. The PG decode functions
(`decode_zodb_record_for_pg_json`, `decode_zodb_record_for_pg`) replace a
string value containing NUL bytes with a marker holding the base64 of its
UTF-8 bytes, and a dict key containing NUL bytes with a string key carrying
the same payload behind an `@ns:` prefix:

```json
{"@ns": "YQBi"}
{"@ns:awBleQ==": 1}
```

Python: `"a\x00b"` and `{"k\x00ey": 1}`

The encoders (`encode_zodb_record`, `json_to_pickle`) restore both forms.
Keys that genuinely start with `@ns:` are escaped the same way by every
decode path, so they survive a round trip too. Single-key dicts `{"@ns": str}`
are reserved for the marker, like every other `@`-prefixed single-key dict.

### `@inst` -- BUILD on a non-instance

`BUILD` applied to a value that is not a class instance (a corrupt or hand-made
pickle) is kept as an anonymous instance and re-encoded as `value state BUILD`:

```json
{"@inst": {"@obj": {}, "@state": {}}}
```

Releases before 1.7.0 also wrote `{"@inst": {"@callable": ..., "@args": ...,
"@state": ...}}` for `BUILD` after a `REDUCE` whose callable is not a global;
that stored form is still read and encodes as `callable args REDUCE state
BUILD`. New output uses `@reduce` with the `state` key instead. Any other
`@inst` content is rejected by the encoders.

### `@pkl` -- Raw Pickle Escape Hatch

Base64-encoded pickle fragment for types that cannot be represented in
JSON.
This is the "never fails" fallback -- any pickle data can
roundtrip through this marker.

```json
{"@pkl": "gAJjc29tZS5tb2R1bGUKU29tZUNsYXNzCnEAKVxxAX0="}
```

## Marker Priority

When decoding JSON back to pickle, markers are checked in a specific
order:

**Single-key markers** (checked first):

`@t`, `@b`, `@bi`, `@d`, `@set`, `@fset`, `@ref`, `@pkl`, `@ns`,
`@dt`, `@date`, `@time`, `@td`, `@dec`, `@uuid`, `@reduce`

**Multi-key markers:**

`@cls` + `@s` (instance with BTree detection), `@dt` + `@tz`
(timezone-aware datetime)

**Fallback:** Plain JSON object becomes a Python dict.

## Backward Compatibility

If JSON data was stored using the generic `@reduce` format for types
that now have dedicated markers (for example, datetime stored as `@reduce`
before the known type handlers were added), the decoder still handles
`@reduce` correctly.
The new markers only affect the forward direction
(pickle to JSON).

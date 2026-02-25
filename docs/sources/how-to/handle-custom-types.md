# Handle custom and unknown types

<!-- diataxis: how-to -->

The codec recognizes a set of common Python types and converts them to compact, human-readable JSON markers.
Types it does not recognize are still preserved with full roundtrip fidelity through fallback markers.

## Known types with dedicated markers

The codec has built-in handlers for these types:

| Python type              | JSON marker | Example JSON value                               |
|--------------------------|-------------|--------------------------------------------------|
| `datetime.datetime`      | `@dt`       | `{"@dt": "2024-01-15T10:30:00+00:00"}`          |
| `datetime.date`          | `@date`     | `{"@date": "2024-01-15"}`                        |
| `datetime.time`          | `@time`     | `{"@time": "10:30:00"}`                          |
| `datetime.timedelta`     | `@td`       | `{"@td": [86400, 0, 0]}`                         |
| `decimal.Decimal`        | `@dec`      | `{"@dec": "3.14"}`                               |
| `uuid.UUID`              | `@uuid`     | `{"@uuid": "550e8400-e29b-41d4-a716-446655440000"}` |
| `builtins.set`           | `@set`      | `{"@set": [1, 2, 3]}`                            |
| `builtins.frozenset`     | `@fset`     | `{"@fset": [1, 2, 3]}`                           |
| All `BTrees.*` types     | `@kv`, `@children`, etc. | Flattened key-value and tree structure |

These markers are queryable in PostgreSQL JSONB and decode back to the exact original pickle bytes.

## Unknown REDUCE operations: `@reduce`

When the codec encounters a pickle `REDUCE` operation (calling a constructor with arguments) for a type it does not have a dedicated handler for, it preserves the full call structure:

```json
{
  "@reduce": {
    "callable": {"@cls": ["some.module", "SomeClass"]},
    "args": [1, "hello", true]
  }
}
```

The `@reduce` marker captures:

- **callable**: the function or class being called (as a `@cls` global reference)
- **args**: the positional arguments passed to the callable
- **items**: dict-like items set via `__setitem__` after construction (if present)
- **appends**: list-like items appended after construction (if present)

This is sufficient to reconstruct the original pickle operation on encode.

## The escape hatch: `@pkl`

If the codec encounters pickle data that cannot be represented structurally (for example, deeply nested protocol-specific opcodes), it falls back to base64-encoding the raw pickle bytes:

```json
{
  "@pkl": "gASVFAAAAAAAAACMCGJ1aWx0aW5zlIwDc2V0lJOUXZQoSwFLAksDZYWUUi4="
}
```

The `@pkl` marker **never fails** -- any valid pickle fragment can be encoded this way.
On decode, the base64 is converted back to the original bytes.

### What does `@pkl` in your data mean?

If you see `@pkl` values in your JSON data, it means the codec did not have a dedicated handler for that particular type or pickle pattern.
The data is fully preserved and will roundtrip correctly, but it is not human-readable or JSONB-queryable.

In practice, `@pkl` appears rarely.
The codec handles all standard Python types, ZODB persistent references, and the full BTrees family.
If you encounter `@pkl` frequently for a specific type, consider opening an issue to request a dedicated handler.

## Backward compatibility

When new type markers are added to the codec (for example, `@dt` for datetime was added after `@reduce` already existed), existing data encoded with the older `@reduce` format still decodes correctly.
The JSON-to-pickle path recognizes both the new compact marker and the older `@reduce` representation for the same type.

This means you do not need to re-encode existing data when upgrading the codec.

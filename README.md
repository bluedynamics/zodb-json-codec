# zodb-json-codec

Fast pickle-to-JSON transcoder for ZODB, implemented in Rust via PyO3.

Converts ZODB pickle records into human-readable, JSONB-queryable JSON
while maintaining full roundtrip fidelity. Designed as the codec layer for
a RelStorage JSONB storage backend.

## Why?

ZODB stores object state as Python pickle bytes. This is compact and fast,
but completely opaque to SQL queries. By transcoding pickle to JSON, we
enable PostgreSQL JSONB queries on ZODB object attributes — without changing
the application code.

The codec does **more work** than pickle (2 conversions + type-aware
transformation vs pickle's single C-extension pass), yet the Rust
implementation matches or beats CPython pickle on most operations.

## Installation

Requires Rust toolchain and [maturin](https://www.maturin.rs/):

```bash
pip install maturin
maturin develop          # debug build
maturin develop --release  # optimized build
```

## Python API

```python
import zodb_json_codec

# ZODB records (two concatenated pickles: class + state)
record: dict = zodb_json_codec.decode_zodb_record(data)
# -> {"@cls": ["myapp.models", "Document"], "@s": {"title": "Hello", ...}}
data: bytes = zodb_json_codec.encode_zodb_record(record)

# Standalone pickle <-> Python dict
result: dict = zodb_json_codec.pickle_to_dict(pickle_bytes)
pickle_bytes: bytes = zodb_json_codec.dict_to_pickle(result)

# Standalone pickle <-> JSON string
json_str: str = zodb_json_codec.pickle_to_json(pickle_bytes)
pickle_bytes: bytes = zodb_json_codec.json_to_pickle(json_str)
```

## JSON Format

The codec uses compact marker keys (`@t`, `@b`, `@dt`, etc.) to represent
Python types that have no direct JSON equivalent. All markers are designed
for roundtrip safety: encode to JSON and decode back produces identical
pickle bytes.

### Quick Reference

| Python Type | Marker | JSON Example |
|---|---|---|
| `tuple` | `@t` | `{"@t": [1, 2, 3]}` |
| `bytes` | `@b` | `{"@b": "AQID"}` (base64) |
| `set` | `@set` | `{"@set": [1, 2, 3]}` |
| `frozenset` | `@fset` | `{"@fset": [1, 2, 3]}` |
| `datetime` | `@dt` | `{"@dt": "2025-06-15T12:00:00"}` |
| `date` | `@date` | `{"@date": "2025-06-15"}` |
| `time` | `@time` | `{"@time": "12:30:45"}` |
| `timedelta` | `@td` | `{"@td": [7, 3600, 0]}` |
| `Decimal` | `@dec` | `{"@dec": "3.14"}` |
| `UUID` | `@uuid` | `{"@uuid": "12345678-..."}` |
| Persistent ref | `@ref` | `{"@ref": "0000000000000003"}` |
| BTree map data | `@kv` | `{"@kv": [["a", 1], ["b", 2]]}` |
| BTree set data | `@ks` | `{"@ks": [1, 2, 3]}` |
| Unknown type | `@pkl` | `{"@pkl": "base64..."}` (escape hatch) |

For the complete type mapping reference, see [TYPE_MAPPING.md](TYPE_MAPPING.md).

## Performance

Benchmarked against CPython's `pickle` module (C extension) on synthetic
ZODB records. The codec does fundamentally more work (2 conversions + type
transformation) yet beats pickle on most categories:

| Operation | Best | Worst | Typical ZODB |
|---|---|---|---|
| Decode | **1.9x faster** | 1.2x slower | 1.4x faster |
| Encode | **7.4x faster** | 1.4x faster | 3.8x faster |
| Roundtrip | **3.1x faster** | 1.0x slower | 2.0x faster |

On a real Plone 6 database (8,400+ records, 182 distinct types, 0 errors):
decode is **1.4x faster** (median) with **14.6x faster** mean due to
eliminating Python pickle's extreme outliers.

For detailed numbers and optimization history, see [BENCHMARKS.md](BENCHMARKS.md).

## Development

### Prerequisites

- Rust 1.70+ (`rustup` recommended)
- Python 3.10+
- maturin (`pip install maturin` or `uv tool install maturin`)

### Build & Test

```bash
# Rust unit tests (75 tests)
cargo test

# Build Python extension (debug)
maturin develop

# Python integration tests (149 tests)
pytest tests/ -v

# Build optimized for benchmarking
maturin develop --release

# Run benchmarks
python benchmarks/bench.py synthetic --iterations 1000
python benchmarks/bench.py filestorage /path/to/Data.fs
```

### Project Structure

```
src/
  lib.rs          # PyO3 module: Python-facing functions
  decode.rs       # Pickle bytes -> PickleValue AST
  encode.rs       # PickleValue AST -> pickle bytes
  pyconv.rs       # Direct PickleValue <-> PyObject (fast path)
  json.rs         # PickleValue <-> serde_json (JSON string path)
  known_types.rs  # Known REDUCE handlers (datetime, Decimal, etc.)
  btrees.rs       # BTree state flattening/reconstruction
  zodb.rs         # ZODB two-pickle record handling
  types.rs        # PickleValue enum definition
  opcodes.rs      # Pickle opcode constants
  error.rs        # Error types
python/
  zodb_json_codec/
    __init__.py   # Re-exports from Rust extension
tests/
  test_*.py       # Python integration tests
benchmarks/
  bench.py        # Performance benchmarks vs CPython pickle
```

## License

MIT

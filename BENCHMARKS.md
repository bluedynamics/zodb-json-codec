# Performance Benchmarks

Comparison of `zodb-json-codec` (Rust + PyO3) vs CPython's `pickle` module
for ZODB record encoding/decoding.

Measured on: 2026-02-06
Python: 3.13.9, 500 iterations, 50 warmup

## Context

The codec does fundamentally more work than `pickle.loads`/`pickle.dumps`:

- **pickle**: C extension, direct bytes <-> Python objects (1 conversion)
- **codec**: pickle bytes <-> Rust AST <-> serde_json <-> PyO3 Python objects
  (3 conversions), plus type-aware JSON transformation (datetime, BTree, etc.)

The codec's value is not raw speed but **JSONB queryability** — enabling SQL
queries on ZODB object attributes in PostgreSQL.

## Synthetic Benchmarks

### Decode (pickle bytes -> Python dict)

| Category | Python | Codec | Ratio |
|---|---|---|---|
| simple_flat_dict (120 B) | 2.5 us | 15.0 us | 6x |
| nested_dict (187 B) | 3.4 us | 27.5 us | 8x |
| large_flat_dict (2.5 KB) | 27.6 us | 266.9 us | 10x |
| bytes_in_state (1 KB) | 2.3 us | 20.7 us | 9x |
| special_types (314 B) | 8.9 us | 52.7 us | 6x |
| btree_small (112 B) | 2.3 us | 20.5 us | 9x |
| btree_length (44 B) | 1.7 us | 4.8 us | 3x |
| scalar_string (72 B) | 2.3 us | 5.2 us | 2x |
| wide_dict (27 KB) | 316.9 us | 3.6 ms | 11x |
| deep_nesting (379 B) | 8.7 us | 104.8 us | 12x |

### Encode (Python dict -> pickle bytes)

| Category | Python | Codec | Ratio |
|---|---|---|---|
| simple_flat_dict | 1.9 us | 11.6 us | 6x |
| nested_dict | 1.6 us | 21.3 us | 13x |
| large_flat_dict | 6.1 us | 161.4 us | 26x |
| bytes_in_state | 1.3 us | 24.6 us | 19x |
| special_types | 7.9 us | 31.6 us | 4x |
| btree_small | 1.5 us | 14.4 us | 10x |
| btree_length | 1.2 us | 5.7 us | 5x |
| scalar_string | 1.7 us | 5.4 us | 3x |
| wide_dict | 85.4 us | 1.9 ms | 22x |
| deep_nesting | 3.2 us | 83.8 us | 26x |

### Full Roundtrip (decode + encode)

| Category | Python | Codec | Ratio |
|---|---|---|---|
| simple_flat_dict | 4.7 us | 29.3 us | 6x |
| nested_dict | 5.9 us | 51.6 us | 9x |
| large_flat_dict | 37.1 us | 509.8 us | 14x |
| special_types | 16.4 us | 81.5 us | 5x |
| btree_small | 4.3 us | 39.9 us | 9x |
| wide_dict | 414.3 us | 5.5 ms | 13x |

### Size Comparison (pickle bytes vs JSON)

| Category | Pickle | JSON | Ratio |
|---|---|---|---|
| simple_flat_dict | 120 B | 110 B | 0.92x |
| nested_dict | 187 B | 156 B | 0.83x |
| large_flat_dict | 2,508 B | 2,197 B | 0.88x |
| bytes_in_state | 1,087 B | 1,414 B | 1.30x |
| special_types | 314 B | 228 B | 0.73x |
| btree_small | 112 B | 111 B | 0.99x |
| wide_dict | 27,057 B | 15,818 B | **0.58x** |
| deep_nesting | 379 B | 586 B | 1.55x |

JSON is typically smaller than pickle for string-heavy data (wide_dict: 42%
smaller). It is larger for binary data (base64 overhead) and deeply nested
structures (marker overhead).

## Bottleneck Analysis

The main cost is the **PyO3 boundary** — converting between Rust and Python
objects. The pure-Rust portion (pickle parsing, AST transformation, JSON
construction) is fast; the overhead comes from:

1. **serde_json::Value -> PyObject** (decode path): each dict key/value
   requires a Python API call to create the Python object
2. **PyObject -> serde_json::Value** (encode path): type checking and
   extraction for every value in the input dict
3. **Intermediate representation**: the codec goes through 3 layers
   (pickle <-> AST <-> JSON <-> Python) where pickle only does 1

## Optimizations Applied

- **Type-based dispatch** (`is_instance_of` vs try-extract) in the encode
  path — avoids creating/discarding Python error objects on type mismatches.
  Result: ~1.8x faster encode across all categories.
- **Pre-collected PyList** (`PyList::new` vs empty+append loop) — builds
  Python lists in one allocation instead of repeated appends.
- **Zero-copy encode** — `encode_zodb_record` takes ownership of the JSON
  value to avoid cloning the state tree.

## Future Optimization Opportunities

- **Bypass serde_json::Value**: go directly PickleValue -> PyObject and
  PyObject -> PickleValue, eliminating the JSON intermediate layer entirely.
  This is the single largest potential win but requires significant refactoring.
- **Batch string interning**: reuse Python string objects for repeated keys
  like common dict field names.
- **Release-mode build**: benchmarks currently run against debug builds;
  `maturin develop --release` would show production performance.

## Running Benchmarks

```bash
cd sources/zodb-json-codec

# Synthetic micro-benchmarks
python benchmarks/bench.py synthetic --iterations 1000

# Scan a real FileStorage
python benchmarks/bench.py filestorage /path/to/Data.fs

# Both, with JSON export for tracking
python benchmarks/bench.py all --filestorage /path/to/Data.fs --output results.json
```

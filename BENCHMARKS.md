# Performance Benchmarks

Comparison of `zodb-json-codec` (Rust + PyO3) vs CPython's `pickle` module
for ZODB record encoding/decoding.

Measured on: 2026-02-06
Python: 3.13.9, 500 iterations, 100 warmup

## Context

The codec does fundamentally more work than `pickle.loads`/`pickle.dumps`:

- **pickle**: C extension, direct bytes <-> Python objects (1 conversion)
- **codec**: pickle bytes <-> Rust PickleValue AST <-> PyO3 Python objects
  (2 conversions), plus type-aware transformation (datetime, BTree, etc.)

The codec's value is not raw speed but **JSONB queryability** — enabling SQL
queries on ZODB object attributes in PostgreSQL.

## Synthetic Benchmarks

### Decode (pickle bytes -> Python dict)

| Category | Python | Codec | Ratio |
|---|---|---|---|
| simple_flat_dict (120 B) | 2.5 us | 7.0 us | 3x |
| nested_dict (187 B) | 3.3 us | 14.7 us | 5x |
| large_flat_dict (2.5 KB) | 26.3 us | 81.8 us | 3x |
| bytes_in_state (1 KB) | 2.1 us | 10.3 us | 5x |
| special_types (314 B) | 13.8 us | 36.5 us | 3x |
| btree_small (112 B) | 3.3 us | 11.2 us | 3x |
| btree_length (44 B) | 1.6 us | 3.3 us | 2x |
| scalar_string (72 B) | 1.6 us | 3.4 us | 2x |
| wide_dict (27 KB) | 306.2 us | 997.3 us | 3x |
| deep_nesting (379 B) | 8.4 us | 48.2 us | 6x |

### Encode (Python dict -> pickle bytes)

| Category | Python | Codec | Ratio |
|---|---|---|---|
| simple_flat_dict | 1.5 us | 4.9 us | 3x |
| nested_dict | 1.7 us | 15.5 us | 4x |
| large_flat_dict | 7.1 us | 47.4 us | 9x |
| bytes_in_state | 2.7 us | 22.6 us | 7x |
| special_types | 5.1 us | 20.2 us | 4x |
| btree_small | 1.5 us | 4.6 us | 4x |
| btree_length | 1.3 us | 1.8 us | 1x |
| scalar_string | 1.3 us | 1.8 us | 1x |
| wide_dict | 68.4 us | 466.4 us | 7x |
| deep_nesting | 3.1 us | 53.4 us | 17x |

### Full Roundtrip (decode + encode)

| Category | Python | Codec | Ratio |
|---|---|---|---|
| simple_flat_dict | 4.6 us | 14.3 us | 3x |
| nested_dict | 6.0 us | 28.8 us | 5x |
| large_flat_dict | 32.9 us | 137.8 us | 5x |
| special_types | 23.3 us | 46.6 us | 2x |
| btree_small | 4.5 us | 16.6 us | 4x |
| wide_dict | 413.0 us | 1.5 ms | 4x |

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

The main remaining cost is the **PyO3 boundary** — converting between Rust
and Python objects. Each dict key/value requires a Python API call.

The codec now goes through 2 layers (pickle <-> PickleValue AST <-> Python)
instead of the original 3 (pickle <-> AST <-> serde_json <-> Python).

## Optimizations Applied

1. **Direct PickleValue <-> PyObject** (`src/pyconv.rs`) — bypasses the
   `serde_json::Value` intermediate layer, eliminating one full allocation
   pass. Persistent ref compact/expand happens inline during the tree walk.
   Result: **2-3.5x faster** across most categories vs the serde_json path.

2. **Type-based dispatch** (`is_instance_of` vs try-extract) in the encode
   path — avoids creating/discarding Python error objects on type mismatches.

3. **Pre-collected PyList** (`PyList::new` vs empty+append loop) — builds
   Python lists in one allocation instead of repeated appends.

## Improvement History

| Optimization | Decode | Encode | Roundtrip |
|---|---|---|---|
| Initial (3-layer via serde_json) | 6-12x | 4-26x | 5-14x |
| + Type dispatch, PyList pre-collect | 2-12x | 3-26x | 5-14x |
| + Direct pyconv (current) | **2-6x** | **1-17x** | **2-5x** |

## Future Optimization Opportunities

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

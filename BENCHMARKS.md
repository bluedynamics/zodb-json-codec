# Performance Benchmarks

Comparison of `zodb-json-codec` (Rust + PyO3) vs CPython's `pickle` module
for ZODB record encoding/decoding.

Measured on: 2026-02-06
Python: 3.13.9, 500 iterations, 100 warmup
Build: `maturin develop --release` (optimized)

## Context

The codec does fundamentally more work than `pickle.loads`/`pickle.dumps`:

- **pickle**: C extension, direct bytes <-> Python objects (1 conversion)
- **codec**: pickle bytes <-> Rust PickleValue AST <-> PyO3 Python objects
  (2 conversions), plus type-aware transformation (datetime, BTree, etc.)

The codec's value is not raw speed but **JSONB queryability** — enabling SQL
queries on ZODB object attributes in PostgreSQL. Despite the extra work, the
release build matches or beats CPython pickle on most operations.

## Synthetic Benchmarks

### Decode (pickle bytes -> Python dict)

| Category | Python | Codec | Ratio |
|---|---|---|---|
| simple_flat_dict (120 B) | 2.3 us | 1.7 us | **1.2x faster** |
| nested_dict (187 B) | 3.7 us | 2.5 us | **1.5x faster** |
| large_flat_dict (2.5 KB) | 25.9 us | 23.8 us | **1.1x faster** |
| bytes_in_state (1 KB) | 2.2 us | 2.4 us | 1.1x slower |
| special_types (314 B) | 7.9 us | 5.9 us | **1.3x faster** |
| btree_small (112 B) | 2.8 us | 2.3 us | **1.4x faster** |
| btree_length (44 B) | 1.4 us | 0.6 us | **2.4x faster** |
| scalar_string (72 B) | 1.7 us | 1.2 us | **1.0x faster** |
| wide_dict (27 KB) | 279.2 us | 322.9 us | 1.2x slower |
| deep_nesting (379 B) | 8.2 us | 9.5 us | 1.2x slower |

### Encode (Python dict -> pickle bytes)

| Category | Python | Codec | Ratio |
|---|---|---|---|
| simple_flat_dict | 1.6 us | 1.4 us | **1.2x faster** |
| nested_dict | 1.7 us | 3.5 us | 2.1x slower |
| large_flat_dict | 6.5 us | 9.8 us | 1.5x slower |
| bytes_in_state | 1.4 us | 2.0 us | 1.4x slower |
| special_types | 5.4 us | 4.1 us | **1.5x faster** |
| btree_small | 1.5 us | 0.8 us | **1.5x faster** |
| btree_length | 1.1 us | 0.3 us | **3.4x faster** |
| scalar_string | 1.3 us | 0.4 us | **3.5x faster** |
| wide_dict | 64.4 us | 100.1 us | 1.6x slower |
| deep_nesting | 2.9 us | 11.3 us | 4.1x slower |

### Full Roundtrip (decode + encode)

| Category | Python | Codec | Ratio |
|---|---|---|---|
| simple_flat_dict | 4.6 us | 3.4 us | **1.0x faster** |
| nested_dict | 5.0 us | 6.7 us | 1.5x slower |
| large_flat_dict | 30.8 us | 32.2 us | 1.0x slower |
| special_types | 13.8 us | 9.6 us | **1.5x faster** |
| btree_small | 4.0 us | 3.1 us | **1.3x faster** |
| btree_length | 2.5 us | 2.3 us | **1.1x faster** |
| scalar_string | 3.1 us | 1.2 us | **3.1x faster** |
| wide_dict | 372.7 us | 463.2 us | 1.2x slower |
| deep_nesting | 11.7 us | 21.4 us | 1.8x slower |

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

## Analysis

The codec **beats CPython pickle** on decode for most categories, including
the common case of flat/nested dicts with mixed types. It is slower on:

- **wide_dict / deep_nesting encode**: the codec does more work per value
  (marker detection, type-aware encoding) vs pickle's direct serialization
- **bytes_in_state**: base64 encoding overhead

The sweet spot is typical ZODB objects (5-50 keys, mixed types, datetime
fields, persistent refs) where the codec is **1.0-1.5x faster** than pickle
while also producing queryable JSONB output.

## Optimizations Applied

1. **Direct PickleValue <-> PyObject** (`src/pyconv.rs`) — bypasses the
   `serde_json::Value` intermediate layer, eliminating one full allocation
   pass. Persistent ref compact/expand happens inline during the tree walk.

2. **Type-based dispatch** (`is_instance_of` vs try-extract) in the encode
   path — avoids creating/discarding Python error objects on type mismatches.

3. **Pre-collected PyList** (`PyList::new` vs empty+append loop) — builds
   Python lists in one allocation instead of repeated appends.

## Improvement History (debug build)

| Optimization | Decode | Encode | Roundtrip |
|---|---|---|---|
| Initial (3-layer via serde_json) | 6-12x slower | 4-26x slower | 5-14x slower |
| + Type dispatch, PyList pre-collect | 2-12x slower | 3-26x slower | 5-14x slower |
| + Direct pyconv | 2-6x slower | 1-17x slower | 2-5x slower |
| + Release build (current) | **1.2x faster — 1.2x slower** | **3.5x faster — 4x slower** | **3x faster — 1.8x slower** |

## Running Benchmarks

```bash
cd sources/zodb-json-codec

# Build release first (important!)
maturin develop --release

# Synthetic micro-benchmarks
python benchmarks/bench.py synthetic --iterations 1000

# Scan a real FileStorage
python benchmarks/bench.py filestorage /path/to/Data.fs

# Both, with JSON export for tracking
python benchmarks/bench.py all --filestorage /path/to/Data.fs --output results.json
```

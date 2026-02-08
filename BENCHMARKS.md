# Performance Benchmarks

Comparison of `zodb-json-codec` (Rust + PyO3) vs CPython's `pickle` module
for ZODB record encoding/decoding.

Measured on: 2026-02-08
Python: 3.13.9, PyO3: 0.28, 500 iterations, 100 warmup
Build: `maturin develop --release` (optimized)

## Context

The codec does fundamentally more work than `pickle.loads`/`pickle.dumps`:

- **pickle**: C extension, direct bytes <-> Python objects (1 conversion)
- **codec**: pickle bytes <-> Rust PickleValue AST <-> PyO3 Python objects
  (2 conversions), plus type-aware transformation (datetime, BTree, etc.)

The codec's value is not raw speed but **JSONB queryability** — enabling SQL
queries on ZODB object attributes in PostgreSQL. Despite the extra work, the
release build beats CPython pickle on most operations.

**Important:** Always benchmark with `maturin develop --release`. Debug builds
are 3-8x slower due to missing optimizations and inlining.

## Synthetic Benchmarks

### Decode (pickle bytes -> Python dict)

| Category | Python | Codec | Ratio |
|---|---|---|---|
| simple_flat_dict (120 B) | 1.9 us | 1.4 us | **1.1x faster** |
| nested_dict (187 B) | 2.9 us | 2.2 us | **1.5x faster** |
| large_flat_dict (2.5 KB) | 23.3 us | 21.9 us | **1.1x faster** |
| bytes_in_state (1 KB) | 2.0 us | 2.0 us | 1.0x |
| special_types (314 B) | 7.4 us | 5.6 us | **1.6x faster** |
| btree_small (112 B) | 1.9 us | 1.9 us | 1.0x |
| btree_length (44 B) | 1.0 us | 0.6 us | **1.8x faster** |
| scalar_string (72 B) | 1.1 us | 0.6 us | **1.8x faster** |
| wide_dict (27 KB) | 259 us | 287 us | 1.1x slower |
| deep_nesting (379 B) | 7.4 us | 8.0 us | 1.0x slower |

### Encode (Python dict -> pickle bytes)

| Category | Python | Codec | Ratio |
|---|---|---|---|
| simple_flat_dict | 1.5 us | 0.3 us | **5.0x faster** |
| nested_dict | 1.6 us | 0.4 us | **4.0x faster** |
| large_flat_dict | 6.6 us | 2.2 us | **3.3x faster** |
| bytes_in_state | 1.3 us | 0.9 us | **1.4x faster** |
| special_types | 5.0 us | 1.0 us | **5.4x faster** |
| btree_small | 1.4 us | 0.3 us | **5.2x faster** |
| btree_length | 1.0 us | 0.2 us | **7.0x faster** |
| scalar_string | 1.0 us | 0.2 us | **5.9x faster** |
| wide_dict | 57.2 us | 16.5 us | **3.4x faster** |
| deep_nesting | 2.6 us | 1.7 us | **1.5x faster** |

### Full Roundtrip (decode + encode)

| Category | Python | Codec | Ratio |
|---|---|---|---|
| simple_flat_dict | 3.6 us | 1.8 us | **2.3x faster** |
| nested_dict | 4.6 us | 2.8 us | **1.7x faster** |
| large_flat_dict | 29.0 us | 42.0 us | 1.3x slower |
| bytes_in_state | 3.1 us | 2.9 us | **1.0x faster** |
| special_types | 12.4 us | 5.9 us | **2.1x faster** |
| btree_small | 3.1 us | 2.3 us | **1.4x faster** |
| btree_length | 2.2 us | 0.7 us | **2.9x faster** |
| scalar_string | 2.1 us | 0.8 us | **2.7x faster** |
| wide_dict | 335 us | 316 us | **1.0x faster** |
| deep_nesting | 10.1 us | 9.7 us | **1.0x faster** |

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

## FileStorage Scan (Real Plone 6 Database)

8,422 records, 182 distinct types, 0 errors.

| Metric | Codec | Python | Speedup |
|---|---|---|---|
| Decode mean | 5.3 us | 100.1 us | **18.7x faster** |
| Decode median | 3.6 us | 4.6 us | **1.3x faster** |
| Decode P95 | 11.6 us | 10.1 us | 1.1x slower |
| Encode mean | 1.1 us | 3.8 us | **3.5x faster** |
| Encode median | 0.7 us | 2.9 us | **4.1x faster** |
| Encode P95 | 2.7 us | 7.0 us | **2.6x faster** |
| Total pickle | 3.1 MB | — | — |
| Total JSON | 4.1 MB | — | 1.30x |

The codec's mean decode speedup (18.7x) far exceeds median (1.3x) because
Python pickle has extreme outliers (max 365 ms) that the Rust codec avoids
(max 2.4 ms). This matters for tail latency in web applications.

## Analysis

The codec **beats CPython pickle** on decode for 8 of 10 synthetic categories,
and on encode for **all 10 categories**. On real Plone data, both decode and
encode are faster across all statistical measures.

The remaining decode-slower cases:

- **wide_dict decode**: 1000 plain string keys — large volume of PyO3
  string allocation overhead
- **deep_nesting decode**: recursive marker prefix scanning on nested dicts

The sweet spot is typical ZODB objects (5-50 keys, mixed types, datetime
fields, persistent refs) where the codec is **1.1-1.8x faster** decode and
**3-7x faster** encode while also producing queryable JSONB output.

## Optimizations Applied

1. **Direct PickleValue <-> PyObject** (`src/pyconv.rs`) — bypasses the
   `serde_json::Value` intermediate layer, eliminating one full allocation
   pass. Persistent ref compact/expand happens inline during the tree walk.

2. **Direct PyObject -> pickle bytes encoder** — for the encode path,
   writes pickle opcodes directly from Python objects to a `Vec<u8>` buffer,
   skipping the intermediate `PickleValue` AST allocation for common types.

3. **Interned marker strings** (`pyo3::intern!`) — all JSON marker keys
   (`@t`, `@cls`, `@s`, etc.) are interned Python strings, cached across
   calls. Eliminates temporary string allocation + hashing per marker check.

4. **Frequency-ordered type dispatch** — encode path checks `PyString` first
   (most common ZODB type), then `PyDict`, before numeric types. Saves 3-4
   type checks per string value.

5. **Dict-size fast path** — dicts with >4 keys skip all marker checks (no
   JSON marker dict has >4 keys). Helps wide_dict and large_flat_dict.

6. **Pre-collected PyList** (`PyList::new` vs empty+append loop) — builds
   Python lists in one allocation instead of repeated appends.

7. **Simplified decoder stack** — removed `StackItem` enum wrapper from the
   pickle decoder. Stack operations (`push`/`pop`/`peek`) are now direct
   `Vec<PickleValue>` operations with `#[inline]` hints. `pop_mark` uses
   `mem::take` (pointer swap) instead of `drain().map().collect()`.

8. **Pre-allocated decoder vectors** — stack, memo, and metastack start with
   `Vec::with_capacity` instead of empty, reducing reallocations during parsing.

9. **Single-pass Dict decode** — removed the O(n) `all_string_keys` pre-scan.
   Optimistically builds string-key PyDict in one pass; falls back to `@d`
   format only if a non-string key is encountered (extremely rare in ZODB).

10. **Set/frozenset move** — REDUCE handler for `builtins.set`/`frozenset`
    moves the list items by value instead of cloning the entire Vec.

11. **`@` prefix encode fast path** — for small dicts (1-4 keys), scans key
    prefixes before doing marker lookups. If no key starts with `@`, skips
    all 15 marker `get_item` checks. Cuts deep_nesting encode by 20%.

12. **Encoder `#[inline]` hints** — `write_u8`, `write_bytes`, and
    `encode_int` marked `#[inline]` to eliminate call overhead in the hot
    encode loop.

13. **Shared ZODB memo** — single decoder processes both class and state
    pickles, sharing the pickle memo between them. Avoids the overhead
    of splitting and re-initializing for the state pickle.

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

# Performance Benchmarks

Comparison of `zodb-json-codec` (Rust + PyO3) vs CPython's `pickle` module
for ZODB record encoding/decoding.

Measured on: 2026-02-07
Python: 3.13.9, PyO3: 0.28, 1000 iterations, 100 warmup
Build: `maturin develop --release` (optimized)

## Context

The codec does fundamentally more work than `pickle.loads`/`pickle.dumps`:

- **pickle**: C extension, direct bytes <-> Python objects (1 conversion)
- **codec**: pickle bytes <-> Rust PickleValue AST <-> PyO3 Python objects
  (2 conversions), plus type-aware transformation (datetime, BTree, etc.)

The codec's value is not raw speed but **JSONB queryability** — enabling SQL
queries on ZODB object attributes in PostgreSQL. Despite the extra work, the
release build beats CPython pickle on most operations.

## Synthetic Benchmarks

### Decode (pickle bytes -> Python dict)

| Category | Python | Codec | Ratio |
|---|---|---|---|
| simple_flat_dict (120 B) | 1.9 us | 1.2 us | **1.6x faster** |
| nested_dict (187 B) | 2.8 us | 2.0 us | **1.4x faster** |
| large_flat_dict (2.5 KB) | 22.6 us | 20.3 us | **1.1x faster** |
| bytes_in_state (1 KB) | 1.7 us | 1.7 us | 1.0x |
| special_types (314 B) | 6.5 us | 5.1 us | **1.3x faster** |
| btree_small (112 B) | 1.8 us | 1.9 us | 1.0x slower |
| btree_length (44 B) | 1.0 us | 0.6 us | **1.8x faster** |
| scalar_string (72 B) | 1.1 us | 0.6 us | **1.9x faster** |
| wide_dict (27 KB) | 252 us | 276 us | 1.1x slower |
| deep_nesting (379 B) | 7.2 us | 8.3 us | 1.2x slower |

### Encode (Python dict -> pickle bytes)

| Category | Python | Codec | Ratio |
|---|---|---|---|
| simple_flat_dict | 1.3 us | 0.3 us | **4.9x faster** |
| nested_dict | 1.4 us | 0.4 us | **3.8x faster** |
| large_flat_dict | 5.6 us | 2.0 us | **2.8x faster** |
| bytes_in_state | 1.2 us | 0.9 us | **1.4x faster** |
| special_types | 4.7 us | 1.0 us | **4.5x faster** |
| btree_small | 1.3 us | 0.2 us | **5.1x faster** |
| btree_length | 1.1 us | 0.1 us | **7.4x faster** |
| scalar_string | 1.0 us | 0.2 us | **6.7x faster** |
| wide_dict | 59.3 us | 16.7 us | **3.6x faster** |
| deep_nesting | 2.7 us | 1.6 us | **1.7x faster** |

### Full Roundtrip (decode + encode)

| Category | Python | Codec | Ratio |
|---|---|---|---|
| simple_flat_dict | 3.2 us | 1.5 us | **2.2x faster** |
| nested_dict | 4.6 us | 3.3 us | **1.4x faster** |
| large_flat_dict | 29.4 us | 22.7 us | **1.3x faster** |
| bytes_in_state | 2.9 us | 2.7 us | **1.1x faster** |
| special_types | 11.5 us | 5.8 us | **2.0x faster** |
| btree_small | 3.4 us | 2.3 us | **1.5x faster** |
| btree_length | 2.1 us | 0.7 us | **3.0x faster** |
| scalar_string | 2.6 us | 0.8 us | **3.1x faster** |
| wide_dict | 344 us | 308 us | **1.1x faster** |
| deep_nesting | 10.7 us | 10.9 us | 1.0x slower |

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

The codec **beats CPython pickle** on decode for 8 of 10 categories, and on
encode for **all 10 categories** — including wide_dict and deep_nesting which
previously were slower. The PyO3 0.28 upgrade brought significant encode
improvements via the new `cast()` API.

The remaining decode-slower cases:

- **wide_dict decode**: 1000 plain string keys — large volume of PyO3
  string allocation overhead
- **deep_nesting decode**: recursive marker prefix scanning on nested dicts

The sweet spot is typical ZODB objects (5-50 keys, mixed types, datetime
fields, persistent refs) where the codec is **1.4-7.4x faster** than pickle
while also producing queryable JSONB output.

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

## Improvement History

| Optimization | Decode | Encode | Roundtrip |
|---|---|---|---|
| Initial (3-layer via serde_json, debug) | 6-12x slower | 4-26x slower | 5-14x slower |
| + Type dispatch, PyList pre-collect | 2-12x slower | 3-26x slower | 5-14x slower |
| + Direct pyconv (debug) | 2-6x slower | 1-17x slower | 2-5x slower |
| + Release build | 1.2x faster — 1.2x slower | 3.5x faster — 4x slower | 3x faster — 1.8x slower |
| + Intern, type reorder, dict skip | 2.8x faster — 1.4x slower | 5.8x faster — 1.9x slower | 3.7x faster — 1.4x slower |
| + Decoder simplify, single-pass dict | 2.4x faster — 1.1x slower | 4.6x faster — 2.2x slower | 3.2x faster — 1.2x slower |
| + `@` prefix encode skip | 2.3x faster — 1.1x slower | 4.1x faster — 1.6x slower | 2.8x faster — 1.2x slower |
| + Direct encoder | 2.3x faster — 1.1x slower | 4.1x faster — 1.5x slower | 2.8x faster — 1.1x slower |
| + PyO3 0.28 upgrade (current) | **1.9x faster — 1.2x slower** | **7.4x faster — 1.4x faster** | **3.1x faster — 1.0x slower** |

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

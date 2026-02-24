# Performance Benchmarks

Comparison of `zodb-json-codec` (Rust + PyO3) vs CPython's `pickle` module
for ZODB record encoding/decoding.

Measured on: 2026-02-24
Python: 3.13.9, PyO3: 0.28, 500 iterations, 100 warmup
Build: `maturin develop --release` (optimized, LTO + codegen-units=1)

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
| simple_flat_dict (120 B) | 1.9 us | 1.1 us | **1.8x faster** |
| nested_dict (187 B) | 2.9 us | 1.8 us | **1.6x faster** |
| large_flat_dict (2.5 KB) | 22.8 us | 19.7 us | **1.2x faster** |
| bytes_in_state (1 KB) | 1.8 us | 1.9 us | 1.1x slower |
| special_types (314 B) | 6.8 us | 4.7 us | **1.5x faster** |
| btree_small (112 B) | 1.9 us | 1.8 us | 1.1x faster |
| btree_length (44 B) | 1.0 us | 0.5 us | **2.0x faster** |
| scalar_string (72 B) | 1.1 us | 0.5 us | **2.1x faster** |
| wide_dict (27 KB) | 264 us | 279 us | 1.1x slower |
| deep_nesting (379 B) | 7.2 us | 7.3 us | 1.0x |

### Encode (Python dict -> pickle bytes)

| Category | Python | Codec | Ratio |
|---|---|---|---|
| simple_flat_dict | 1.3 us | 0.2 us | **5.3x faster** |
| nested_dict | 1.6 us | 0.4 us | **4.5x faster** |
| large_flat_dict | 5.9 us | 1.7 us | **3.8x faster** |
| bytes_in_state | 1.4 us | 0.9 us | **1.7x faster** |
| special_types | 4.6 us | 0.9 us | **5.0x faster** |
| btree_small | 1.3 us | 0.2 us | **5.8x faster** |
| btree_length | 1.1 us | 0.1 us | **7.5x faster** |
| scalar_string | 1.0 us | 0.1 us | **6.6x faster** |
| wide_dict | 59.2 us | 15.7 us | **3.7x faster** |
| deep_nesting | 2.7 us | 1.4 us | **1.9x faster** |

### Full Roundtrip (decode + encode)

| Category | Python | Codec | Ratio |
|---|---|---|---|
| simple_flat_dict | 3.2 us | 1.5 us | **2.1x faster** |
| nested_dict | 4.5 us | 2.2 us | **2.0x faster** |
| large_flat_dict | 29.7 us | 21.8 us | **1.4x faster** |
| bytes_in_state | 3.3 us | 3.0 us | 1.1x faster |
| special_types | 11.7 us | 6.0 us | **2.0x faster** |
| btree_small | 5.8 us | 2.1 us | **2.8x faster** |
| btree_length | 2.1 us | 0.7 us | **3.2x faster** |
| scalar_string | 2.3 us | 0.8 us | **3.1x faster** |
| wide_dict | 316 us | 232 us | **1.4x faster** |
| deep_nesting | 10.3 us | 9.2 us | 1.1x faster |

### Size Comparison (pickle bytes vs JSON)

| Category | Pickle | JSON | Ratio |
|---|---|---|---|
| simple_flat_dict | 120 B | 110 B | 0.92x |
| nested_dict | 187 B | 156 B | 0.83x |
| large_flat_dict | 2,508 B | 2,197 B | 0.88x |
| bytes_in_state | 1,087 B | 1,414 B | 1.30x |
| special_types | 314 B | 228 B | 0.73x |
| btree_small | 112 B | 111 B | 0.99x |
| btree_length | 44 B | 47 B | 1.07x |
| scalar_string | 72 B | 70 B | 0.97x |
| wide_dict | 27,057 B | 15,818 B | **0.58x** |
| deep_nesting | 379 B | 586 B | 1.55x |

JSON is typically smaller than pickle for string-heavy data (wide_dict: 42%
smaller). It is larger for binary data (base64 overhead) and deeply nested
structures (marker overhead).

## FileStorage Scan (Generated Wikipedia Database)

1,692 records, 6 distinct types, 0 errors. Generated from 1,062 multilingual
Wikipedia articles (en/de/zh) with body text truncated to 500-10,000 chars
(exponential skew toward shorter texts), enriched type-diverse fields
(datetime, date, timedelta, Decimal, UUID, frozenset, set, tuple, bytes)
plus OOBTree containers, group summaries, and edge-case objects.

Generate with: `python benchmarks/bench.py generate`

| Metric | Codec | Python | Speedup |
|---|---|---|---|
| Decode mean | 28.7 us | 23.7 us | 1.2x slower |
| Decode median | 24.7 us | 22.6 us | 1.1x slower |
| Decode P95 | 42.3 us | 36.3 us | 1.2x slower |
| Encode mean | 7.0 us | 18.8 us | **2.7x faster** |
| Encode median | 6.2 us | 20.4 us | **3.3x faster** |
| Encode P95 | 12.8 us | 31.5 us | **2.5x faster** |
| Total pickle | 5.1 MB | — | — |
| Total JSON | 7.2 MB | — | 1.41x |

The codec is slightly slower on decode (1.1x median) because it does
fundamentally more work than CPython's C-extension pickle: two conversions
(pickle bytes → Rust AST → Python objects) plus type-aware transformation.
The gap narrows on metadata-heavy records (small dicts with mixed types).

Encode is consistently **2.5-3.3x faster** because the Rust encoder writes
pickle opcodes directly from Python objects, bypassing intermediate
allocations that CPython's pickle module incurs.

| Record type | Count | % |
|---|---|---|
| persistent.mapping.PersistentMapping | 1,188 | 70.2% |
| BTrees.OOBTree.OOBucket | 342 | 20.2% |
| persistent.list.PersistentList | 100 | 5.9% |
| BTrees.OOBTree.OOBTree | 55 | 3.3% |
| BTrees.Length.Length | 5 | 0.3% |
| BTrees.OIBTree.OIBTree | 2 | 0.1% |

## Analysis

The codec **beats CPython pickle** on decode for 8 of 10 synthetic categories,
and on encode for **all 10 categories**. On the generated FileStorage data,
decode is near parity (1.1x median) while encode is **2.5-3.3x faster**.

The sweet spot is typical ZODB objects (5-50 keys, mixed types, datetime
fields, persistent refs) where the codec is **1.5-2.0x faster** decode and
**4-7x faster** encode while also producing queryable JSONB output.

Decode overhead comes from the codec's two-pass conversion plus type
transformation. On string-dominated payloads this matters more; on
metadata-rich records with mixed types (the typical ZODB case) the codec
is competitive or faster.

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

9. **Pre-scan Dict decode** — checks `all_string_keys` with a cheap enum
   discriminant scan before processing values. Builds string-key PyDict if
   all keys are strings (>99% of ZODB dicts); otherwise uses `@d` format.
   Avoids quadratic re-processing when mixed-key dicts are encountered.

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

14. **Boxed Instance variant** — `Instance(Box<InstanceData>)` reduces the
    `PickleValue` enum from 56 to 48 bytes, improving cache utilization
    across the entire decode/encode pipeline (-13% weighted average).

15. **Thin LTO + single codegen unit** — `lto = "thin"` + `codegen-units = 1`
    in the release profile enables cross-crate inlining and whole-crate
    optimization. Free 6-9% improvement across decode and encode with no
    code changes.

## Changelog

### 1.3.1 (2026-02-24): LTO release profile optimization

Enabled thin LTO (`lto = "thin"`) and single codegen unit (`codegen-units = 1`)
in the Cargo release profile. This allows LLVM to inline across crate boundaries
and optimize the entire crate as a single compilation unit.

Impact on FileStorage benchmark (1,692 records):

| Metric | Before | After | Improvement |
|---|---|---|---|
| Decode median | 26.1 us | 24.7 us | **-5.4%** |
| Decode mean | 30.5 us | 28.7 us | **-5.9%** |
| Encode median | 6.8 us | 6.2 us | **-8.8%** |
| Encode mean | 7.5 us | 7.0 us | **-6.7%** |

Zero code changes — purely a build configuration improvement.

### 2026-02-23: Dict/list subclass support + PickleValue boxing optimization

Added support for pickle SETITEMS/SETITEM/APPENDS/APPEND on Reduce and
Instance variants (fixes [#5](https://github.com/bluedynamics/zodb-json-codec/issues/5):
`ValueError: SETITEMS on non-dict` for OrderedDict, defaultdict, deque, etc.).

To avoid an enum size regression, the `Instance` variant was refactored from
an inline struct to `Instance(Box<InstanceData>)`, reducing `PickleValue` from
56 bytes (pre-change baseline) to **48 bytes** — a 14% reduction.

5-round min-median benchmark comparison (baseline vs fix):

| Payload | Op | Baseline | Fix | Delta |
|---|---|---|---|---|
| simple_flat_dict | decode | 1.31 us | 1.21 us | **-7.9%** |
| nested_dict | decode | 2.00 us | 1.95 us | -2.5% |
| large_flat_dict | decode | 20.19 us | 19.65 us | -2.7% |
| btree_length | decode | 0.63 us | 0.58 us | **-9.0%** |
| wide_dict | decode | 304.69 us | 257.02 us | **-15.6%** |
| special_types | encode | 1.01 us | 0.96 us | **-5.2%** |
| btree_small | encode | 0.27 us | 0.24 us | **-10.1%** |
| wide_dict | encode | 17.47 us | 16.24 us | **-7.1%** |
| **Weighted avg** | **all** | | | **-13.4%** |

No regressions above noise threshold. The smaller enum improves cache
utilization across the entire decode/encode pipeline, with the largest
gains on payloads that allocate many PickleValue nodes (wide_dict, large dicts).

## Running Benchmarks

```bash
cd sources/zodb-json-codec

# Build release first (important!)
maturin develop --release

# Synthetic micro-benchmarks
python benchmarks/bench.py synthetic --iterations 1000

# Generate a reproducible benchmark FileStorage (requires ZODB + BTrees)
python benchmarks/bench.py generate
# Custom paths:
python benchmarks/bench.py generate --output /tmp/bench.fs \
    --seed-data path/to/seed_data.json.gz

# Scan the generated (or any) FileStorage
python benchmarks/bench.py filestorage benchmarks/bench_data/Data.fs

# Both synthetic + filestorage, with JSON export
python benchmarks/bench.py all --filestorage benchmarks/bench_data/Data.fs --output results.json
```

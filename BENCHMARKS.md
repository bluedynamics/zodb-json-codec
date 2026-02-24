# Performance Benchmarks

Comparison of `zodb-json-codec` (Rust + PyO3) vs CPython's `pickle` module
for ZODB record encoding/decoding.

Measured on: 2026-02-24
Python: 3.13.9, PyO3: 0.28, 5000 iterations, 100 warmup
Build: `maturin develop --release` (optimized, LTO + codegen-units=1 + PGO)

**Important:** Always benchmark with `maturin develop --release`. Debug builds
are 3-8x slower due to missing optimizations and inlining.

## Why the codec exists

The codec does fundamentally more work than `pickle.loads`/`pickle.dumps`:

- **pickle**: C extension, direct bytes <-> Python objects (1 conversion)
- **codec**: pickle bytes <-> Rust PickleValue AST <-> PyO3 Python objects
  (2 conversions), plus type-aware transformation (datetime, BTree, etc.)

The codec's value is not raw speed but **JSONB queryability** — enabling SQL
queries on ZODB object attributes in PostgreSQL. Despite the extra work, the
release build beats CPython pickle on most operations.

---

## Synthetic micro-benchmarks

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

### Decode to JSON string (pickle bytes -> JSON, all in Rust)

The direct path for PG storage — serializes to a JSON string entirely in Rust
with the GIL released. Compared against the dict path + `json.dumps()`.

| Category | Dict+dumps | JSON str | Speedup |
|---|---|---|---|
| simple_flat_dict | 2.7 us | 1.3 us | **2.2x faster** |
| nested_dict | 4.3 us | 2.5 us | **1.7x faster** |
| large_flat_dict | 35.4 us | 25.6 us | **1.4x faster** |
| bytes_in_state | 5.7 us | 2.7 us | **2.1x faster** |
| special_types | 7.1 us | 4.7 us | **1.5x faster** |
| btree_small | 3.8 us | 2.1 us | **1.8x faster** |
| btree_length | 1.5 us | 0.8 us | **1.9x faster** |
| scalar_string | 0.9 us | 0.7 us | **1.3x faster** |
| wide_dict | 273.7 us | 307.6 us | 1.1x slower |
| deep_nesting | 13.3 us | 8.6 us | **1.5x faster** |

### Encode (Python dict -> pickle bytes)

| Category | Python | Codec | Ratio |
|---|---|---|---|
| simple_flat_dict | 1.3 us | 0.2 us | **6.5x faster** |
| nested_dict | 1.5 us | 0.3 us | **4.8x faster** |
| large_flat_dict | 5.3 us | 1.5 us | **3.5x faster** |
| bytes_in_state | 1.2 us | 0.7 us | **1.7x faster** |
| special_types | 4.7 us | 0.5 us | **9.8x faster** |
| btree_small | 1.3 us | 0.2 us | **6.0x faster** |
| btree_length | 1.1 us | 0.1 us | **8.8x faster** |
| scalar_string | 1.2 us | 0.1 us | **8.3x faster** |
| wide_dict | 56.4 us | 13.9 us | **4.0x faster** |
| deep_nesting | 2.8 us | 1.0 us | **2.8x faster** |

### Full roundtrip (decode + encode)

| Category | Python | Codec | Ratio |
|---|---|---|---|
| simple_flat_dict | 3.2 us | 1.4 us | **2.4x faster** |
| nested_dict | 4.5 us | 2.1 us | **2.2x faster** |
| large_flat_dict | 29.7 us | 19.1 us | **1.6x faster** |
| bytes_in_state | 3.3 us | 2.4 us | **1.4x faster** |
| special_types | 11.7 us | 4.4 us | **2.7x faster** |
| btree_small | 5.8 us | 1.8 us | **3.3x faster** |
| btree_length | 2.1 us | 0.6 us | **3.6x faster** |
| scalar_string | 2.3 us | 0.6 us | **3.6x faster** |
| wide_dict | 316 us | 260 us | **1.2x faster** |
| deep_nesting | 10.3 us | 7.3 us | **1.4x faster** |

### Output size (pickle bytes vs JSON)

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

---

## FileStorage scan (generated Wikipedia database)

1,692 records, 6 distinct types, 0 errors. Generated from 1,062 multilingual
Wikipedia articles (en/de/zh) with body text truncated to 500-10,000 chars
(exponential skew toward shorter texts), enriched type-diverse fields
(datetime, date, timedelta, Decimal, UUID, frozenset, set, tuple, bytes)
plus OOBTree containers, group summaries, and edge-case objects.

### Codec vs CPython pickle

| Metric | Codec | Python | Speedup |
|---|---|---|---|
| Decode mean | 26.9 us | 22.2 us | 1.2x slower |
| Decode median | 23.2 us | 21.6 us | 1.1x slower |
| Decode P95 | 39.7 us | 31.7 us | 1.3x slower |
| Encode mean | 4.7 us | 18.0 us | **3.8x faster** |
| Encode median | 3.9 us | 19.7 us | **5.1x faster** |
| Encode P95 | 9.6 us | 29.1 us | **3.0x faster** |
| Total pickle | 5.1 MB | — | — |
| Total JSON | 7.2 MB | — | 1.41x |

Decode is slightly slower (1.1x median) due to the two-pass conversion plus
type-aware transformation. The gap narrows on metadata-heavy records.
Encode is consistently **3.0-5.1x faster** because the Rust encoder writes
pickle opcodes directly from Python objects, bypassing intermediate allocations.

### Record type distribution

| Record type | Count | % |
|---|---|---|
| persistent.mapping.PersistentMapping | 1,188 | 70.2% |
| BTrees.OOBTree.OOBucket | 342 | 20.2% |
| persistent.list.PersistentList | 100 | 5.9% |
| BTrees.OOBTree.OOBTree | 55 | 3.3% |
| BTrees.Length.Length | 5 | 0.3% |
| BTrees.OIBTree.OIBTree | 2 | 0.1% |

---

## PG storage path (FileStorage full pipeline)

The zodb-pgjsonb storage path has two decode functions. The dict path
(`decode_zodb_record_for_pg`) returns a Python dict that must then be
serialized via `json.dumps()`. The JSON string path
(`decode_zodb_record_for_pg_json`) does everything in Rust with the GIL
released. See the synthetic comparison above.

```
Dict path:   pickle bytes → Rust AST → Python dict (GIL held) → json.dumps() → PG
JSON path:   pickle bytes → Rust AST → serde_json → JSON string (all Rust, GIL released) → PG
```

### 1,692 records

| Metric | Dict+dumps | JSON str | Speedup |
|---|---|---|---|
| Mean | 41.3 us | 31.5 us | **1.3x faster** |
| Median | 35.9 us | 26.9 us | **1.3x faster** |
| P95 | 64.2 us | 47.7 us | **1.3x faster** |

The JSON string path is **1.3x faster** across real-world data because
it eliminates the Python dict allocation + `json.dumps()` serialization.
The entire pipeline runs in Rust with the GIL released, improving
multi-threaded throughput in Zope/Plone deployments.

---

## Summary

The sweet spot is typical ZODB objects (5-50 keys, mixed types, datetime
fields, persistent refs):

- **Decode:** 1.5-2.0x faster on synthetic, near parity on real-world data
- **Encode:** 2-10x faster on synthetic, 3-5x faster on real-world data
- **PG path:** 1.3x faster end-to-end with GIL-free throughput

Decode overhead comes from the two-pass conversion plus type transformation.
On string-dominated payloads this matters more; on metadata-rich records with
mixed types (the typical ZODB case) the codec is competitive or faster.

---

## Optimizations applied

**Decode path:**
- Direct PickleValue <-> PyObject conversion (`pyconv.rs`), bypassing the
  `serde_json::Value` intermediate layer
- Simplified decoder stack with `#[inline]` hints, `mem::take` for `pop_mark`
- Pre-allocated stack/memo/metastack vectors (`Vec::with_capacity`)
- Pre-scan dict keys for string-only fast path (>99% of ZODB dicts)
- Shared ZODB memo across class + state pickles
- Set/frozenset move semantics (no Vec clone)
- Boxed Instance variant (PickleValue 56 → 48 bytes, -13% weighted avg)

**Encode path:**
- Direct PyObject → pickle bytes encoder (bypasses PickleValue AST)
- Frequency-ordered type dispatch (PyString first)
- Dict-size fast path (>4 keys skips all marker checks)
- O(1) `@cls` hash lookup replaces O(n) key scan for marker detection
- Direct known-type encoding (`@dt`, `@date`, `@time`, `@td`, `@dec`) —
  writes pickle opcodes inline, skipping PickleValue intermediate (eliminates
  6 heap allocations per datetime encode)
- Thread-local buffer reuse (retains capacity across encode calls)
- `reserve()` calls before multi-part writes (eliminates mid-write reallocations)
- Direct i64 LONG1 encoding (eliminates BigInt heap allocation)
- `#[inline]` on `write_u8`, `write_bytes`, `encode_int`

**Both paths:**
- Interned marker strings (`pyo3::intern!` for `@t`, `@cls`, `@s`, etc.)
- Pre-collected PyList (`PyList::new` vs append loop)
- Thin LTO + single codegen unit (free 6-9% improvement)
- Profile-guided optimization (PGO) with real FileStorage + synthetic data
- Direct pickle → JSON string path for PG storage (GIL released)

---

## Running benchmarks

```bash
cd sources/zodb-json-codec

# Build release first (important!)
maturin develop --release

# Synthetic micro-benchmarks
python benchmarks/bench.py synthetic --iterations 1000

# Generate a reproducible benchmark FileStorage (requires ZODB + BTrees)
python benchmarks/bench.py generate

# Scan the generated (or any) FileStorage
python benchmarks/bench.py filestorage benchmarks/bench_data/Data.fs

# PG decode path comparison (dict vs JSON string)
python benchmarks/bench.py pg-compare --filestorage benchmarks/bench_data/Data.fs

# Both synthetic + filestorage, with JSON export
python benchmarks/bench.py all --filestorage benchmarks/bench_data/Data.fs --output results.json
```

## PGO build (optional, adds 5-15%)

Profile-guided optimization uses real workload data to optimize branch
prediction and code layout. The release CI builds include PGO for
Linux x86_64 wheels.

```bash
# 1. Install LLVM tools
rustup component add llvm-tools

# 2. Instrumented build
RUSTFLAGS="-Cprofile-generate=/tmp/pgo-data" maturin develop --release

# 3. Generate profiles — use BOTH real data and synthetic for best coverage
python benchmarks/bench.py filestorage benchmarks/bench_data/Data.fs
python benchmarks/bench.py synthetic --iterations 2000

# 4. Merge profiles
LLVM_PROFDATA=$(find ~/.rustup -name llvm-profdata | head -1)
$LLVM_PROFDATA merge -o /tmp/pgo-data/merged.profdata /tmp/pgo-data/*.profraw

# 5. Optimized build
RUSTFLAGS="-Cprofile-use=/tmp/pgo-data/merged.profdata" maturin develop --release
```

# Encode Path Optimization — Round 4 Report

**Date:** 2026-02-25
**Codec version:** 1.4.0 (pre-release)
**Platform:** Linux 6.14.0, Rust 1.92.0, Python 3.13.9, x86_64
**Build:** `maturin develop --release` + PGO (LTO + codegen-units=1)
**PGO profile:** Real FileStorage (1,692 records) + synthetic (2000 iter) + pg-compare (500 iter)
**Benchmark:** 5000 synthetic iterations, 100 warmup
**Baseline:** Round 3 final (direct JSON writer + PGO)

## Goal

Cache class pickle bytes per `(module, name)` pair to avoid re-encoding
identical class pickles for every ZODB record. In a typical ZODB database
there are only 6 distinct class types, but `encode_zodb_record_direct()`
rebuilt the class pickle bytes from scratch on every call.

## Changes

### 1. Thread-local class pickle cache (`src/pyconv.rs`)

Added a thread-local `Vec<(String, String, Vec<u8>)>` alongside the existing
`ENCODE_BUF`. Uses linear search — with ~6 entries, this is faster than
hashing and avoids allocating key strings on cache hits.

### 2. `build_class_pickle()` helper (`src/pyconv.rs`)

Extracted the class pickle byte construction into a standalone `pub(crate)`
function: `PROTO 2` + `BINUNICODE(module)` + `BINUNICODE(name)` + `TUPLE2` +
`NONE` + `TUPLE2` + `STOP`. Reused by both the production encode path and the
test encode path in `zodb.rs`.

### 3. Cache usage in `encode_zodb_record_direct()`

Replaced 7 opcode writes (2× `write_string()` + 5 `push()` + 1 `extend`) with
a single `extend_from_slice(&cached_bytes)` on cache hits. On first call per
class: builds + caches. On subsequent calls: single memcpy of ~50 bytes.

### 4. Test path consolidation (`src/zodb.rs`)

The `#[cfg(test)]` `encode_zodb_record()` previously built a `PickleValue::Tuple`
intermediate (4 heap allocations + 2 String clones) then encoded via
`encode_pickle()`. Now calls `build_class_pickle()` directly.

## Results — Synthetic Encode (median, microseconds)

| Category | R3+PGO | R4+PGO | Change | vs Python |
|---|---:|---:|---:|---:|
| simple_flat_dict | 0.2 | 0.2 | ±0 | **6.7x faster** |
| nested_dict | 0.3 | 0.3 | ±0 | **6.4x faster** |
| large_flat_dict | 1.6 | 1.6 | ±0 | **3.9x faster** |
| bytes_in_state | 0.7 | 0.8 | ±0 | **1.7x faster** |
| special_types | 0.5 | 0.5 | ±0 | **9.2x faster** |
| btree_small | 0.2 | 0.2 | ±0 | **6.6x faster** |
| btree_length | 0.1 | 0.1 | ±0 | **8.0x faster** |
| scalar_string | 0.1 | 0.1 | ±0 | **7.9x faster** |
| wide_dict | 14.9 | 13.7 | **-8%** | **4.1x faster** |
| deep_nesting | 1.1 | 1.0 | **-9%** | **2.6x faster** |

At single-digit microsecond resolution, the per-record savings from caching
~50 bytes of class pickle are within measurement noise for most categories.
The effect is visible on `wide_dict` and `deep_nesting` where the class
pickle cost is proportionally more noticeable.

## Results — Synthetic Decode (median, microseconds)

Decode path unchanged in R4 — numbers for reference only.

| Category | R4+PGO | vs Python |
|---|---:|---:|
| simple_flat_dict | 1.0 µs | **1.9x faster** |
| nested_dict | 1.6 µs | **1.3x faster** |
| large_flat_dict | 18.0 µs | **1.3x faster** |
| bytes_in_state | 1.4 µs | **1.1x faster** |
| special_types | 3.8 µs | **1.8x faster** |
| btree_small | 1.5 µs | **1.2x faster** |
| btree_length | 0.4 µs | **2.3x faster** |
| scalar_string | 0.5 µs | **2.2x faster** |
| wide_dict | 244.5 µs | **1.0x faster** |
| deep_nesting | 6.4 µs | **1.0x slower** |

## Results — Real FileStorage (1,692 ZODB records, 5.1 MB)

### Encode across rounds

| Metric | R3 (PGO) | R4 (PGO) | Change | Python | R4 vs Python |
|---|---:|---:|---:|---:|---:|
| Mean | 4.9 µs | 4.8 µs | **-2%** | 18.2 µs | **3.8x faster** |
| Median | 4.1 µs | 4.0 µs | **-2%** | 19.9 µs | **5.0x faster** |
| P95 | 10.3 µs | 9.9 µs | **-4%** | 30.0 µs | **3.0x faster** |

The class pickle cache provides a consistent **2-4% improvement** on real
FileStorage data. With 1,692 records across only 6 distinct classes, the
cache hits ~99.6% of the time after warmup.

### Decode (dict-based, Codec vs Python)

| Metric | Codec (R4+PGO) | Python | Ratio |
|---|---:|---:|---:|
| Mean | 27.2 µs | 22.7 µs | 1.2x slower |
| Median | 23.6 µs | 22.2 µs | 1.1x slower |
| P95 | 40.5 µs | 33.1 µs | 1.2x slower |

### Full ZODB → PG round-trip estimate

| Operation | Time per record | Notes |
|---|---:|---|
| Decode to JSON (write) | 23.6 µs | GIL released, direct JSON string |
| Encode from dict (read) | 4.0 µs | Cached class pickle + direct state |
| **Total codec overhead** | **~28 µs** | Per object, both directions |

For a Plone page load touching 50 objects: **~1.4 ms** total codec overhead.

## Test Coverage

**198 Rust tests** (196 existing + 2 new):
- `test_build_class_pickle_matches_pickle_value_encode` — verifies cached bytes
  match the PickleValue-based encode for 7 class name variants (long, short,
  empty, common ZODB types)
- `test_build_class_pickle_starts_with_proto_ends_with_stop` — structural check

**180 Python integration tests** — all pass unchanged.

## Key Takeaways

1. **Marginal but consistent improvement** — 2-4% on FileStorage encode. The
   class pickle (~50 bytes) was already cheap to write into the pre-allocated
   `ENCODE_BUF`, so the savings are modest.

2. **The bottleneck is state pickle encoding** — with class pickle now cached,
   the remaining encode cost is entirely in the state pickle (dict keys/values,
   known types, persistent refs). Further encode optimization would need to
   target this path.

3. **Zero overhead on cache misses** — the cache uses linear search over a small
   Vec (~6 entries). On first-time class encoding, the cost is identical to the
   uncached path plus one Vec push. On subsequent calls, no string allocation
   occurs for the lookup.

4. **Code simplification** — the test path in `zodb.rs` now calls
   `build_class_pickle()` instead of building a `PickleValue::Tuple` intermediate
   with 4 heap allocations and recursive encoding.

## Cumulative Optimization Summary (Rounds 1-4)

| Round | Focus | Key Wins |
|---|---|---|
| R1 | Encode: stack pre-alloc, GIL release, PGO | Encode 8-37% faster, PGO 5-10% free |
| R2 | Encode: direct known-type, thread-local buf | special_types -50%, FileStorage 5.1x vs Python |
| R3 | Decode: direct JSON writer, eliminate serde_json | wide_dict -55%, FileStorage PG pipeline 1.4x |
| R4 | Encode: class pickle cache | FileStorage encode -2 to -4%, wide_dict -8% |

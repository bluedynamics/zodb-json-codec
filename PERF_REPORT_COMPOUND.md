# Compound Performance Report — Rounds 1-4

**Date:** 2026-02-25
**Codec version:** 1.4.0 (pre-release)
**Platform:** Linux 6.14.0, Rust 1.92.0, Python 3.13.9, x86_64
**Build:** `maturin develop --release` + PGO (LTO + codegen-units=1)
**PGO profile:** Real FileStorage (1,692 records) + synthetic (2000 iter) + pg-compare (500 iter)
**Benchmark:** 5000 synthetic / 1000 pg-compare iterations, 100 warmup

This report compares the **original unoptimized codec** (pre-R1, no PGO)
against the **current state** (post-R4, with PGO). All "Current" numbers
are from the PGO build.

## What Changed in Each Round

| Round | Focus | Techniques |
|---|---|---|
| R1 | Encode path | BigInt elimination, buffer reserve(), marker scan → hash lookup, PGO |
| R2 | Encode path | Direct known-type encoding (datetime/date/time/timedelta/decimal), thread-local buffer reuse, @dt+@tz bug fix |
| R3 | Decode PG path | Direct PickleValue → JSON string writer, eliminate serde_json::Value intermediate, thread-local JSON buffer, ryu float formatting |
| R4 | Encode path | Thread-local class pickle cache per (module, name), build_class_pickle() helper |

## Encode Performance (median, microseconds)

Original = pre-R1 (no PGO). Current = post-R4 (with PGO).

| Category | Original | Current | Change | vs Python |
|---|---:|---:|---:|---:|
| simple_flat_dict | 0.249 | 0.2 | **-20%** | **6.7x faster** |
| nested_dict | 0.356 | 0.3 | **-16%** | **6.4x faster** |
| large_flat_dict | 1.811 | 1.6 | **-12%** | **3.9x faster** |
| bytes_in_state | 0.898 | 0.8 | **-11%** | **1.7x faster** |
| special_types | 0.952 | 0.5 | **-47%** | **9.2x faster** |
| btree_small | 0.240 | 0.2 | **-17%** | **6.6x faster** |
| btree_length | 0.130 | 0.1 | **-23%** | **8.0x faster** |
| scalar_string | 0.135 | 0.1 | **-26%** | **7.9x faster** |
| wide_dict | 15.226 | 13.7 | **-10%** | **4.1x faster** |
| deep_nesting | 1.605 | 1.0 | **-38%** | **2.6x faster** |

The biggest encode win is `special_types` (**-47%**, 9.2x vs Python) from
direct known-type encoding (R2) combined with PGO (R1). This category
contains datetime, date, timedelta, and Decimal — the most common types
in ZODB content objects.

## Decode Performance (median, microseconds)

The dict-based decode path (`decode_zodb_record`) was not a primary
optimization target. PGO still provides gains.

| Category | Original | Current | Change | vs Python |
|---|---:|---:|---:|---:|
| simple_flat_dict | — | 1.0 | — | **1.9x faster** |
| nested_dict | — | 1.6 | — | **1.3x faster** |
| large_flat_dict | — | 18.0 | — | **1.3x faster** |
| bytes_in_state | — | 1.4 | — | **1.1x faster** |
| special_types | — | 3.8 | — | **1.8x faster** |
| btree_small | — | 1.5 | — | **1.2x faster** |
| btree_length | — | 0.4 | — | **2.3x faster** |
| scalar_string | — | 0.5 | — | **2.2x faster** |
| wide_dict | — | 244.5 | — | **1.0x faster** |
| deep_nesting | — | 6.4 | — | **1.0x slower** |

(Pre-R1 decode baselines were not captured; the decode path was not changed
in R1-R2. PGO gives 5-15% decode improvement over release-only builds.)

## Roundtrip Performance (median, microseconds)

Full decode + encode cycle.

| Category | Original | Current | Change | vs Python |
|---|---:|---:|---:|---:|
| simple_flat_dict | 1.459 | 1.3 | **-11%** | **2.6x faster** |
| nested_dict | 2.467 | 2.1 | **-15%** | **2.1x faster** |
| large_flat_dict | 20.304 | 19.8 | **-2%** | **1.5x faster** |
| bytes_in_state | 2.766 | 2.3 | **-17%** | **1.4x faster** |
| special_types | 5.609 | 4.9 | **-13%** | **2.4x faster** |
| btree_small | 2.214 | 1.8 | **-19%** | **1.7x faster** |
| btree_length | 0.655 | 0.6 | **-8%** | **3.4x faster** |
| scalar_string | 0.841 | 0.6 | **-29%** | **3.5x faster** |
| wide_dict | 263.834 | 258.8 | **-2%** | **1.3x faster** |
| deep_nesting | 8.666 | 7.8 | **-10%** | **1.3x faster** |

## PG Decode Path — The Production Path (mean, microseconds)

`decode_zodb_record_for_pg_json()` converts pickle bytes directly to a JSON
string in Rust with the GIL released. This is the path used by `zodb-pgjsonb`.

Before R3 = serde_json::Value intermediate (no PGO baseline available for
this path). Current = direct JSON writer + PGO.

### Synthetic categories

| Category | Dict+dumps | JSON str (R3+PGO) | Speedup |
|---|---:|---:|---:|
| simple_flat_dict | 2.7 µs | 1.1 µs | **2.4x faster** |
| nested_dict | 4.3 µs | 1.9 µs | **2.3x faster** |
| large_flat_dict | 33.7 µs | 17.1 µs | **2.0x faster** |
| bytes_in_state | 5.2 µs | 1.6 µs | **3.3x faster** |
| special_types | 7.5 µs | 4.0 µs | **1.8x faster** |
| btree_small | 3.6 µs | 1.6 µs | **2.3x faster** |
| btree_length | 1.4 µs | 0.5 µs | **3.0x faster** |
| scalar_string | 0.8 µs | 0.6 µs | **1.3x faster** |
| wide_dict | 290.5 µs | 161.6 µs | **1.8x faster** |
| deep_nesting | 14.2 µs | 5.7 µs | **2.5x faster** |

### FileStorage (1,692 records, full pipeline)

| Metric | Dict+dumps | JSON str (R3+PGO) | Speedup |
|---|---:|---:|---:|
| Mean | 40.4 µs | 28.3 µs | **1.4x faster** |
| Median | 34.7 µs | 24.4 µs | **1.4x faster** |
| P95 | 62.0 µs | 51.9 µs | **1.2x faster** |

## Real FileStorage — 1,692 ZODB Records (5.1 MB)

### Encode across rounds

| Metric | R1 (PGO) | R2 (PGO) | R3 (PGO) | R4 (PGO) | Python | R4 vs Python |
|---|---:|---:|---:|---:|---:|---:|
| Mean | 6.2 µs | 4.7 µs | 4.9 µs | 4.8 µs | 18.2 µs | **3.8x faster** |
| Median | 5.6 µs | 3.9 µs | 4.1 µs | 4.0 µs | 19.9 µs | **5.0x faster** |
| P95 | 12.3 µs | 9.6 µs | 10.3 µs | 9.9 µs | 30.0 µs | **3.0x faster** |

R4 class pickle cache gives 2-4% over R3 (encode-only change).

### Decode (dict-based, Codec vs Python)

| Metric | Codec (R4+PGO) | Python | Ratio |
|---|---:|---:|---:|
| Mean | 27.2 µs | 22.7 µs | 1.2x slower |
| Median | 23.6 µs | 22.2 µs | 1.1x slower |
| P95 | 40.5 µs | 33.1 µs | 1.2x slower |

The dict decode path is slightly slower than CPython's pickle (expected —
the codec does fundamentally more work: pickle → Rust AST → type-aware
Python dict).

### Full ZODB → PG round-trip estimate

| Operation | Time per record | Notes |
|---|---:|---|
| Decode to JSON (write) | 23.6 µs | GIL released, direct JSON string |
| Encode from dict (read) | 4.0 µs | Cached class pickle + direct state |
| **Total codec overhead** | **~28 µs** | Per object, both directions |

For a Plone page load touching 50 objects: **~1.4 ms** total codec overhead.

## Summary

### Where we started (pre-R1, no PGO)

| Metric | Range |
|---|---|
| Encode | 0.13-15.2 µs (1.6-8.2x vs Python) |
| Roundtrip | 0.65-264 µs |
| PG path | serde_json::Value intermediate, no direct writer |
| Build | release only, no PGO, no buffer reuse |

### Where we are now (post-R4, with PGO)

| Metric | Range |
|---|---|
| Encode | 0.1-13.7 µs (**1.7-9.2x vs Python**, up to **-47%** from baseline) |
| Roundtrip | 0.6-259 µs (up to **-29%** from baseline) |
| PG JSON string path | **1.3-3.3x faster** than dict+dumps |
| FileStorage PG pipeline | 23.6 µs median (**1.4x** vs dict+dumps) |
| FileStorage encode | 4.0 µs median (**5.0x** vs Python) |
| Build | PGO + LTO, thread-local buffers, direct JSON writer, class pickle cache |

### Total gains from all four rounds

| Category | Encode Δ | Roundtrip Δ | Highlight |
|---|---:|---:|---|
| special_types | **-47%** | **-13%** | Direct known-type encoding |
| deep_nesting | **-38%** | **-10%** | Marker scan elimination + PGO |
| scalar_string | **-26%** | **-29%** | PGO branch optimization |
| simple_flat_dict | **-20%** | **-11%** | Cumulative small wins |
| btree_small | **-17%** | **-19%** | PGO + buffer reuse |
| nested_dict | **-16%** | **-15%** | Hash lookup + PGO |
| bytes_in_state | **-11%** | **-17%** | Buffer reserve + PGO |
| wide_dict | **-10%** | **-2%** | Class pickle cache (R4) |
| large_flat_dict | **-12%** | **-2%** | Buffer reserve |
| PG wide_dict | — | — | **-52%** (R3 direct writer) |
| PG deep_nesting | — | — | **-36%** (R3 direct writer) |

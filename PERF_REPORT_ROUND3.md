# Decode Path Optimization — Round 3 Report

**Date:** 2026-02-24
**Codec version:** 1.4.0 (pre-release)
**Platform:** Linux 6.14.0, Rust 1.92.0, Python 3.13.9, x86_64
**Build:** `maturin develop --release` + PGO (LTO + codegen-units=1)
**PGO profile:** Real FileStorage (1,692 records) + synthetic (2000 iter) + pg-compare (500 iter)
**Benchmark:** 5000 synthetic / 1000 pg-compare iterations, 100 warmup
**Baseline:** Round 2 final (encode optimizations, no PGO baseline for PG path)

## Goal

Eliminate `serde_json::Value` intermediate allocation in the PG JSON decode path
(`decode_zodb_record_for_pg_json`). The old pipeline:

```
pickle bytes → PickleValue AST → serde_json::Value → serde_json::to_string() → JSON string
```

The new pipeline:

```
pickle bytes → PickleValue AST → JSON string (direct write)
```

Every `serde_json::Value` node (String, Array, Object) was a heap allocation that
was immediately discarded after `to_string()`. The direct writer eliminates all
of them by writing JSON tokens directly to a `String` buffer.

## Changes

### 1. JsonWriter core (`src/json_writer.rs` — NEW)

A `JsonWriter` struct wrapping a `String` buffer with methods for all JSON tokens:
`write_null`, `write_bool`, `write_i64`, `write_f64`, `write_string`,
`begin_object/end_object`, `begin_array/end_array`, `write_key`, `write_comma`.

Key details:
- `write_string()` has fast path (no special chars → no per-char scan) and slow
  path (proper JSON escaping of `\`, `"`, control chars, `\u0000`)
- `write_f64()` uses the `ryu` crate for fast exact float formatting, handles
  NaN/Infinity → `null` (matching serde_json behavior)
- `write_string_literal()` for pre-validated strings (marker keys like `@dt`)
  that skip the escape check entirely

### 2. Recursive PickleValue → JSON writer (`src/json.rs`)

`pickle_value_to_json_string_pg()` walks the `PickleValue` AST and writes
directly to `JsonWriter` instead of building `serde_json::Value` nodes:

- All PG-specific behavior hardcoded (null-byte sanitization `@ns`, compact
  persistent refs with hex OID)
- BTree dispatch handled internally (no separate entry point needed)
- Thread-local `JsonWriter` buffer (`JSON_BUF`) reuses capacity across calls,
  same pattern as the encode path's `ENCODE_BUF`
- MAX_DEPTH = 200 guard against stack overflow

### 3. Known type direct writers (`src/known_types.rs`)

`try_write_reduce_typed()` and `try_write_instance_typed()` write JSON markers
for all known types directly to `JsonWriter`:

- `@dt` (datetime with full timezone support: naive, UTC, fixed offset, named)
- `@date`, `@time` (with microseconds and offset), `@td` (timedelta)
- `@dec` (Decimal), `@uuid` (UUID), `@set`, `@fset` (set/frozenset)
- Reuses existing parsing helpers (`decode_datetime_bytes`, `format_datetime_iso`,
  `extract_tz_info`, etc.) — only the output stage changed

### 4. BTree direct writer (`src/btrees.rs`)

`btree_state_to_json_writer()` handles all BTree variants:
- Small BTrees (4-level tuple nesting) → `@kv`/`@ks` flat data
- Buckets (2-level key-value pairs) → `@kv`/`@ks` flat data
- Large BTrees (persistent refs) → `@children`/`@first`
- Empty states → `null`
- Linked buckets → `@next` marker

### 5. Wire-up (`src/lib.rs`)

Replaced the two-step pipeline in `decode_zodb_record_for_pg_json()`:

```rust
// Before (allocate serde_json::Value, then serialize):
let state_json = if let Some(info) = btrees::classify_btree(&module, &name) {
    btrees::btree_state_to_json(&info, &state_val, &json::pickle_value_to_json_pg)?
} else {
    json::pickle_value_to_json_pg(&state_val)?
};
let json_str = serde_json::to_string(&state_json)...;

// After (single direct call):
let json_str = json::pickle_value_to_json_string_pg(&state_val, &module, &name)?;
```

## Results — PG JSON String Path (mean, microseconds)

This is the path used by `zodb-pgjsonb` in production: `decode_zodb_record_for_pg_json()`.

Before = R2 (serde_json::Value intermediate, no PGO).
After = R3 (direct JSON writer + PGO).

| Category | Before (R2) | After (R3+PGO) | Change |
|---|---:|---:|---:|
| simple_flat_dict | 1.5 | 1.1 | **-27%** |
| nested_dict | 2.4 | 1.9 | **-21%** |
| large_flat_dict | 30.2 | 17.1 | **-43%** |
| bytes_in_state | 2.7 | 1.6 | **-41%** |
| special_types | 4.5 | 4.0 | **-11%** |
| btree_small | 1.9 | 1.6 | **-16%** |
| btree_length | 0.6 | 0.5 | **-17%** |
| scalar_string | 0.7 | 0.6 | **-14%** |
| wide_dict | 359.6 | 161.6 | **-55%** |
| deep_nesting | 10.8 | 5.7 | **-47%** |

The "Before" baseline is from the non-PGO R2 build (no PGO baseline exists for
the old serde_json path). The improvement combines both the direct writer (R3)
and PGO gains. Code-only improvements (without PGO) were measured at -20% to
-52% in an intermediate run.

## Results — PG JSON vs Dict+dumps Comparison

The JSON string path now substantially outperforms the dict path + `json.dumps()`:

| Category | Dict+dumps | JSON str (R3+PGO) | Speedup |
|---|---:|---:|---:|
| simple_flat_dict | 2.7 µs | 1.1 µs | **2.5x** |
| nested_dict | 4.3 µs | 1.9 µs | **2.3x** |
| large_flat_dict | 33.7 µs | 17.1 µs | **2.0x** |
| bytes_in_state | 5.2 µs | 1.6 µs | **3.3x** |
| special_types | 7.5 µs | 4.0 µs | **1.9x** |
| btree_small | 3.6 µs | 1.6 µs | **2.3x** |
| btree_length | 1.4 µs | 0.5 µs | **2.8x** |
| scalar_string | 0.8 µs | 0.6 µs | **1.3x** |
| wide_dict | 290.5 µs | 161.6 µs | **1.8x** |
| deep_nesting | 14.2 µs | 5.7 µs | **2.5x** |

## Results — Real FileStorage (1,692 ZODB records, 5.1 MB)

Full pipeline comparison (decode + JSON for PG):

| Metric | Dict+dumps | JSON str (R3+PGO) | Speedup |
|---|---:|---:|---:|
| Mean | 40.4 µs | 28.3 µs | **1.4x** |
| Median | 34.7 µs | 24.4 µs | **1.4x** |
| P95 | 62.0 µs | 51.9 µs | **1.2x** |

Record type distribution (affects performance profile):
- PersistentMapping: 70.2% (string-heavy → big wins from eliminated String allocations)
- OOBucket: 20.2% (key-value pairs → good wins)
- PersistentList: 5.9%
- OOBTree: 3.3%
- Length/OIBTree: 0.4%

### Encode (R3+PGO, FileStorage)

The encode path was not changed in R3. PGO provides additional gains over R2.

| Metric | Codec (R3+PGO) | Python | Speedup |
|---|---:|---:|---:|
| Mean | 4.9 µs | 18.7 µs | **3.8x** |
| Median | 4.1 µs | 20.6 µs | **5.0x** |
| P95 | 10.3 µs | 30.6 µs | **3.0x** |

## Results — Synthetic Decode (unchanged path)

The synthetic decode benchmarks test the dict-based path (`decode_zodb_record`),
which was not changed in Round 3. PGO provides additional gains.

| Category | Decode (R3+PGO) | vs Python |
|---|---:|---:|
| simple_flat_dict | 1.0 µs | **1.8x faster** |
| nested_dict | 1.7 µs | **1.5x faster** |
| large_flat_dict | 17.1 µs | **1.3x faster** |
| bytes_in_state | 1.5 µs | **1.1x faster** |
| special_types | 3.9 µs | **1.6x faster** |
| btree_small | 1.5 µs | **1.2x faster** |
| btree_length | 0.5 µs | **2.1x faster** |
| scalar_string | 0.5 µs | **2.2x faster** |
| wide_dict | 200.9 µs | **1.2x faster** |
| deep_nesting | 6.3 µs | **1.1x faster** |

## Test Coverage

**196 Rust tests** (135 existing + 61 new):

- **26 JsonWriter unit tests** covering: null, bool, integer (positive/negative/zero/i64
  extremes), float (normal/NaN/Infinity/-Infinity/subnormal/negative zero), string
  (empty/simple/special chars requiring escape/unicode/all control chars/null byte),
  object (empty/with keys), array (empty/with elements/nested), key writing, comma
  separation, raw injection, buffer clear/take, capacity allocation

- **61 comparison tests** (`assert_pg_paths_match`) verifying byte-for-byte equivalence
  between old path (serde_json::Value → to_string) and new path (direct writer):
  - Primitives: None, bool, int, bigint, float, string, bytes
  - Containers: list, tuple, dict (string keys + non-string keys), set, frozenset
  - Globals, instances (with/without dict_items/list_items, empty module)
  - Persistent refs: oid-only, with class info, fallback
  - Known types: datetime (naive, UTC, offset, pytz_utc, pytz_named), date, time
    (naive, with microseconds, with offset), timedelta, decimal, set, frozenset, uuid
  - Unknown reduce, reduce with dict/list items
  - Raw pickle escape hatch
  - BTrees: empty, small, bucket, set, treeset, linked bucket, large with persistent
    refs, empty bucket, empty inline
  - Nested structures, mixed types, deeply nested (10 levels)
  - Realistic PersistentMapping state, state with datetime + persistent ref

**176 Python integration tests** (all pass, 4 pytz-related skipped — pre-existing):
- Full roundtrip coverage for all type categories
- ZODB record encode/decode with class pickle validation
- PG-specific paths (null sanitization, ref extraction)

## Key Takeaways

1. **The `wide_dict` category halved** — 359.6 → 161.6 µs (**-55%**, **1.8x faster**
   than dict+dumps). With ~500 keys, each eliminated `Value::String` allocation
   compounds dramatically. This is the category most representative of large
   PersistentMapping objects in real ZODB databases.

2. **String-heavy records benefit most** — `large_flat_dict` (-43%), `deep_nesting`
   (-47%), `bytes_in_state` (-41%). These categories have many string values that
   previously required `Value::String(s.clone())` heap allocations.

3. **Real FileStorage confirms synthetic gains** — 1.4x faster at median for the
   full pipeline. Since 70% of records are PersistentMapping (string-heavy), the
   improvement tracks closely with the `simple_flat_dict`/`nested_dict` category gains.

4. **Thread-local buffer reuse amplifies gains** — like Round 2's encode buffer,
   the JSON writer's `String` buffer retains capacity across calls. After the first
   few records, no new allocations occur for the output buffer.

5. **Tiny records show modest improvement** — `scalar_string` (-14%) and
   `btree_length` (-17%) are mostly bottlenecked by pickle decoding overhead,
   not JSON serialization. PGO provides the improvement here.

6. **No regressions** — the dict-based decode path, encode path, and roundtrip
   path are unchanged. All 196 Rust + 176 Python tests pass.

## Cumulative Optimization Summary (Rounds 1-3)

| Round | Focus | Key Wins |
|---|---|---|
| R1 | Encode: stack pre-alloc, GIL release, PGO | Encode 8-37% faster, PGO 5-10% free |
| R2 | Encode: direct known-type, thread-local buf | special_types -50%, FileStorage 5.1x vs Python |
| R3 | Decode: direct JSON writer, eliminate serde_json | wide_dict -55%, FileStorage PG pipeline 1.4x |

The codec now handles the full ZODB → PostgreSQL JSONB pipeline (pickle decode +
JSON serialization) in a single GIL-released Rust call, producing a JSON string
with zero intermediate Python objects or serde_json allocations.

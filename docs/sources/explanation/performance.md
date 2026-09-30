# Performance

<!-- diataxis: explanation -->

This page summarizes the codec's benchmark results and provides context for
interpreting them.
For the history of every optimization, see {doc}`optimization-journal`.
For running the benchmarks yourself, see {doc}`/how-to/run-benchmarks`.

## Why the codec exists

The codec does fundamentally more work than `pickle.loads` / `pickle.dumps`:

- **Pickle** (CPython C extension): one conversion, bytes to Python objects or
  back. A single C function call per direction.
- **Codec**: pickle bytes to Rust `PickleValue` AST to Python dict or JSON
  string (two conversions), plus type-aware transformation for datetimes,
  Decimals, BTrees, persistent references, and other types without direct JSON
  equivalents.

The codec's value is not raw speed but **JSONB queryability**: SQL queries on
ZODB object attributes in PostgreSQL. Since 1.7.0 it is also faster than
CPython pickle on encode everywhere and on decode everywhere but the
categories the tables mark as parity.

## How the numbers were measured

All tables on this page come from one session on 2026-09-30: one desktop core
(`taskset`), glibc, Python 3.13.12, minimum of the medians over three
interleaved rounds of `benchmarks/bench.py` (5,000 iterations per synthetic
category, 100 warmup; the 1,692-record FileStorage sample; the PG comparison
mode). Columns:

- **CPython pickle**: `pickle.loads` / `pickle.dumps` measured in the same run
  as the 1.7.0 codec.
- **v1.5.0**, **1.6.1**: the release tags, built from source without PGO
  (1.6.1 is the tree at `fe19ede`, the 1.6.1 release plus the first 1.7.0
  fixes that do not touch performance).
- **1.7.0**: `main` at `7b26c79`, built without PGO: thin LTO for the tags,
  fat LTO and mimalloc for 1.7.0, `codegen-units = 1` everywhere.
- **1.7.0 PGO**: the same tree built the way the release wheels are built
  (`RUSTFLAGS=-Cprofile-generate`, the profiling workload of `release.yml`,
  then `-Cprofile-use`).

Debug builds are 3 to 8 times slower than release builds; always benchmark
`maturin develop --release`.

## Synthetic micro-benchmarks

### Decode (pickle bytes to Python dict)

| Category | CPython pickle | v1.5.0 | 1.6.1 | 1.7.0 | 1.7.0 PGO | 1.7.0 vs pickle |
|---|---|---|---|---|---|---|
| simple_flat_dict (120 B) | 1.90 us | 1.28 us | 1.59 us | 0.99 us | 0.90 us | 1.9x |
| nested_dict (187 B) | 3.05 us | 2.04 us | 3.11 us | 1.73 us | 1.48 us | 1.8x |
| large_flat_dict (2,508 B) | 22.6 us | 19.2 us | 30.2 us | 11.8 us | 11.0 us | 1.9x |
| bytes_in_state (1,087 B) | 1.67 us | 1.68 us | 2.02 us | 1.55 us | 1.37 us | 1.1x |
| special_types (314 B) | 6.76 us | 4.53 us | 6.81 us | 2.77 us | 2.84 us | 2.4x |
| btree_small (112 B) | 1.90 us | 1.69 us | 2.17 us | 1.07 us | 1.01 us | 1.8x |
| btree_length (44 B) | 1.05 us | 0.49 us | 0.61 us | 0.50 us | 0.47 us | 2.1x |
| scalar_string (72 B) | 1.14 us | 0.57 us | 0.68 us | 0.50 us | 0.51 us | 2.3x |
| wide_dict (27,057 B) | 263.4 us | 278.3 us | 421.2 us | 130.2 us | 118.9 us | 2.0x |
| deep_nesting (379 B) | 7.35 us | 7.14 us | 25.7 us | 7.46 us | 6.64 us | 1.0x (parity) |

Parity means within 5% of CPython pickle: `deep_nesting`.

1.6.1 was slower than 1.5.0 on every category (the 1.6.0 memo fix deep-copied
every container into the memo, journal entry 19). 1.7.0 recovers that and
more through the memo pre-scan, the single value stack with per-thread
scratch vectors, and mimalloc (journal entries 19 to 21).

### Encode (Python dict to pickle bytes)

| Category | CPython pickle | v1.5.0 | 1.6.1 | 1.7.0 | 1.7.0 PGO | 1.7.0 vs pickle |
|---|---|---|---|---|---|---|
| simple_flat_dict | 1.36 us | 0.25 us | 0.24 us | 0.24 us | 0.21 us | 5.6x |
| nested_dict | 1.66 us | 0.33 us | 0.37 us | 0.40 us | 0.31 us | 4.2x |
| large_flat_dict | 5.75 us | 1.62 us | 1.62 us | 1.81 us | 1.63 us | 3.2x |
| bytes_in_state | 1.32 us | 0.84 us | 0.84 us | 0.80 us | 0.76 us | 1.7x |
| special_types | 5.22 us | 0.58 us | 0.67 us | 0.63 us | 0.53 us | 8.3x |
| btree_small | 1.42 us | 0.25 us | 0.25 us | 0.25 us | 0.21 us | 5.8x |
| btree_length | 1.10 us | 0.15 us | 0.15 us | 0.14 us | 0.12 us | 8.0x |
| scalar_string | 1.13 us | 0.16 us | 0.16 us | 0.14 us | 0.13 us | 8.2x |
| wide_dict | 60.2 us | 15.4 us | 15.4 us | 15.6 us | 14.8 us | 3.9x |
| deep_nesting | 2.71 us | 1.24 us | 1.24 us | 1.80 us | 1.34 us | 1.5x |

Encode is unchanged in 1.7.0 apart from the nesting-depth guard (#19), which
costs a few nanoseconds per container and shows on `deep_nesting` and
`nested_dict`. The Rust encoder writes pickle opcodes directly from Python
objects; known types (`@dt`, `@date`, ...) are encoded inline.

### Decode to JSON string (PG storage path)

The direct path for PostgreSQL storage writes JSON tokens straight from the
`PickleValue` AST into a thread-local buffer, entirely in Rust with the GIL
released. Compared with the dict path plus `json.dumps()`, 1.7.0 without
PGO:

| Category | Dict path decode | JSON path decode | Dict path + `json.dumps` | JSON path | Pipeline speedup |
|---|---|---|---|---|---|
| simple_flat_dict | 1.1 us | 0.9 us | 2.7 us | 1.0 us | 2.7x |
| nested_dict | 1.8 us | 1.5 us | 3.8 us | 1.6 us | 2.4x |
| large_flat_dict | 13.5 us | 11.1 us | 28.8 us | 11.2 us | 2.6x |
| bytes_in_state | 1.6 us | 1.4 us | 5.5 us | 1.4 us | 3.9x |
| special_types | 2.9 us | 2.6 us | 5.6 us | 2.5 us | 2.2x |
| btree_small | 1.1 us | 1.0 us | 3.1 us | 1.1 us | 2.8x |
| btree_length | 0.5 us | 0.5 us | 1.5 us | 0.5 us | 3.0x |
| scalar_string | 0.5 us | 0.6 us | 0.9 us | 0.6 us | 1.5x |
| wide_dict | 137.7 us | 99.2 us | 208.2 us | 98.5 us | 2.1x |
| deep_nesting | 7.7 us | 6.4 us | 15.5 us | 6.6 us | 2.3x |

## FileStorage scan (real-world data)

1,692 records from a generated Wikipedia-style database, 6 distinct classes, 0 errors; per record:

| Operation | CPython pickle | v1.5.0 | 1.6.1 | 1.7.0 | 1.7.0 PGO | 1.7.0 vs pickle |
|---|---|---|---|---|---|---|
| decode | 22.3 us | 25.7 us | 38.6 us | 13.6 us | 12.9 us | 1.6x |
| encode | 19.7 us | 4.70 us | 5.00 us | 4.37 us | 3.86 us | 4.5x |
| roundtrip | 42.0 us | 32.9 us | 47.5 us | 17.5 us | 16.1 us | 2.4x |

Real records are dominated by `PersistentMapping` states with long text
strings and persistent references. Up to 1.6.1 decode was slower than
CPython here; 1.7.0 is faster.

### Record type distribution

| Record type | Count | % |
|---|---|---|
| `persistent.mapping.PersistentMapping` | 1,188 | 70.2% |
| `BTrees.OOBTree.OOBucket` | 342 | 20.2% |
| `persistent.list.PersistentList` | 100 | 5.9% |
| `BTrees.OOBTree.OOBTree` | 55 | 3.3% |
| `BTrees.Length.Length` | 5 | 0.3% |
| `BTrees.OIBTree.OIBTree` | 2 | 0.1% |

## PG storage path (full pipeline)

The PostgreSQL storage backend, zodb-pgjsonb, calls
`decode_zodb_record_for_pg_json`:

```
Dict path:   pickle bytes -> Rust AST -> Python dict (GIL held) -> json.dumps() -> PG
JSON path:   pickle bytes -> Rust AST -> JSON string (direct write, GIL released) -> PG
```

### 1,692 records, 1.7.0

| Metric | Dict path + `json.dumps` | JSON path | Speedup |
|---|---|---|---|
| Mean | 29.0 us | 12.3 us | 2.4x |
| Median | 22.8 us | 9.7 us | 2.4x |
| P95 | 49.2 us | 25.8 us | 1.9x |

### 1,692 records, 1.7.0 PGO

| Metric | Dict path + `json.dumps` | JSON path | Speedup |
|---|---|---|---|
| Mean | 30.5 us | 15.8 us | 1.9x |
| Median | 22.9 us | 10.9 us | 2.1x |
| P95 | 53.9 us | 37.4 us | 1.4x |

### PGO and the two paths

The release wheels are PGO builds (`release.yml` profiles the FileStorage and
synthetic dict-path benchmarks). In this session PGO made the Python-dict
paths 2 to 15% faster and encode up to 25% faster, but the PG JSON pipeline on
the FileStorage sample slower: median 9.8 to 10.9 us, P95 25 to 37 us. The
synthetic JSON-path categories do not show this; the real records with long
strings, persistent references and BTree buckets do. Two other profile mixes
(adding the PG comparison run, and weighting the real-data runs) gave the same
picture (P95 34 to 38 us), so it is not a matter of profile coverage. The
cause is not identified; it is tracked in
[#52](https://github.com/bluedynamics/zodb-json-codec/issues/52). Until it is,
a build without PGO is the faster choice for the storage path, and the numbers
zodb-pgjsonb sees from the PyPI wheels are the PGO column.

## Output size comparison

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
| wide_dict | 27,057 B | 15,818 B | 0.58x |
| deep_nesting | 379 B | 586 B | 1.55x |

JSON is smaller than pickle for string-heavy data because pickle carries a
memo opcode per string; it is larger for binary data (base64 adds a third)
and for deeply nested structures (marker keys). The FileStorage sample is
1.41x (7.2 MB JSON versus 5.1 MB pickle) for the whole database.

## Summary

1.7.0 against CPython pickle, from the tables above (non-PGO build):

| Operation | Best | Worst | FileStorage sample |
|---|---|---|---|
| Decode | 2.4x faster | 1.0x | 1.6x faster |
| Encode | 8.3x faster | 1.5x faster | 4.5x faster |
| PG JSON path vs dict path + `json.dumps` | 3.9x faster | 1.5x faster | 2.4x faster (median) |

The sweet spot is the typical ZODB object: 5 to 50 keys, mixed types,
datetime fields, persistent references. The remaining cost on decode is the
conversion into Python objects; the PG JSON path avoids it.

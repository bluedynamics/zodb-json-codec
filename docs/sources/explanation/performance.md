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
(`taskset`), glibc, Python 3.13.12, `benchmarks/bench.py` driven by an
interleaved A/B runner. Synthetic tables: minimum over three rounds of the
median of 5,000 iterations (200 warmup). FileStorage tables: minimum over
three rounds of the per-record median of the 1,692-record sample. The PG
comparison tables: `bench.py pg-compare` (5,000 iterations, 100 warmup)
prints means per category and mean, median and P95 for the sample; each
cell is the minimum of that statistic over three rounds. Ratios use
unrounded values. Columns:

- **CPython pickle**: `pickle.loads` / `pickle.dumps` measured in the same run
  as the 1.7.0 codec.
- **v1.5.0**, **1.6.1**: the release tags, built from source without PGO
  (1.6.1 is the tree at `fe19ede`, the 1.6.1 release plus the first 1.7.0
  fixes that do not touch performance).
- **1.7.0**: the 1.7.0 tree (`main` with #52 applied), built without PGO:
  thin LTO for the tags, fat LTO and mimalloc for 1.7.0, `codegen-units = 1`
  everywhere.
- **1.7.0 PGO**: the same tree built the way the release wheels are built
  (`RUSTFLAGS=-Cprofile-generate`, the profiling workload of `release.yml`,
  then `-Cprofile-use`).

The tags build with PyO3 0.28, 1.7.0 with PyO3 0.29. Each column other than
1.7.0 was measured in its own interleaved run against 1.7.0, so the
resolution of the page is the spread of the 1.7.0 build across those runs:
2 to 8% on decode, 14 to 20% on the sub-microsecond encode categories.
Differences inside that spread are noise. Debug builds are 3 to 8 times
slower than release builds; always benchmark `maturin develop --release`.

## Synthetic micro-benchmarks

### Decode (pickle bytes to Python dict)

| Category | CPython pickle | v1.5.0 | 1.6.1 | 1.7.0 | 1.7.0 PGO | 1.7.0 vs pickle |
|---|---|---|---|---|---|---|
| simple_flat_dict (120 B) | 2.05 us | 1.39 us | 1.66 us | 1.10 us | 0.98 us | 1.9x |
| nested_dict (187 B) | 3.14 us | 2.21 us | 3.23 us | 1.98 us | 1.58 us | 1.6x |
| large_flat_dict (2,508 B) | 24.7 us | 20.6 us | 33.1 us | 13.0 us | 11.8 us | 1.9x |
| bytes_in_state (1,087 B) | 1.78 us | 1.84 us | 2.05 us | 1.57 us | 1.47 us | 1.1x |
| special_types (314 B) | 7.46 us | 4.99 us | 7.18 us | 3.09 us | 2.76 us | 2.4x |
| btree_small (112 B) | 1.90 us | 1.82 us | 2.21 us | 1.29 us | 1.03 us | 1.5x |
| btree_length (44 B) | 1.14 us | 0.51 us | 0.72 us | 0.56 us | 0.46 us | 2.0x |
| scalar_string (72 B) | 1.26 us | 0.57 us | 0.77 us | 0.53 us | 0.51 us | 2.4x |
| wide_dict (27,057 B) | 283.9 us | 304.5 us | 454.9 us | 149.4 us | 131.2 us | 1.9x |
| deep_nesting (379 B) | 8.19 us | 8.04 us | 27.7 us | 8.44 us | 7.17 us | 1.0x (parity) |

Parity means within 5% of CPython pickle: `deep_nesting`.

1.6.1 was slower than 1.5.0 on every decode category (the 1.6.0 memo fix deep-copied
every container into the memo, journal entry 19). 1.7.0 recovers that and
more through the memo pre-scan, the single value stack with per-thread
scratch vectors, and mimalloc (journal entries 19 to 21).

### Encode (Python dict to pickle bytes)

| Category | CPython pickle | v1.5.0 | 1.6.1 | 1.7.0 | 1.7.0 PGO | 1.7.0 vs pickle |
|---|---|---|---|---|---|---|
| simple_flat_dict | 1.40 us | 0.25 us | 0.27 us | 0.24 us | 0.23 us | 5.8x |
| nested_dict | 1.68 us | 0.37 us | 0.36 us | 0.38 us | 0.36 us | 4.4x |
| large_flat_dict | 6.35 us | 1.74 us | 1.71 us | 1.96 us | 1.81 us | 3.2x |
| bytes_in_state | 1.38 us | 0.93 us | 0.92 us | 0.80 us | 0.79 us | 1.7x |
| special_types | 5.47 us | 0.59 us | 0.59 us | 0.59 us | 0.55 us | 9.3x |
| btree_small | 1.49 us | 0.29 us | 0.26 us | 0.29 us | 0.22 us | 5.2x |
| btree_length | 1.29 us | 0.17 us | 0.16 us | 0.16 us | 0.15 us | 8.3x |
| scalar_string | 1.15 us | 0.17 us | 0.17 us | 0.14 us | 0.15 us | 8.4x |
| wide_dict | 66.2 us | 16.6 us | 16.5 us | 17.1 us | 16.3 us | 3.9x |
| deep_nesting | 2.96 us | 1.43 us | 1.27 us | 1.83 us | 1.54 us | 1.6x |

The encoder changes of 1.7.0 were correctness work (see the changelog:
subclass items, NUL markers, anonymous instances, big ints, unknown types,
re-entrancy); their cost shows on `nested_dict`, `large_flat_dict` and
`deep_nesting`, the last from the nesting-depth guard
([#19](https://github.com/bluedynamics/zodb-json-codec/issues/19)), a few
nanoseconds per container. The Rust encoder writes pickle opcodes directly
from Python objects; known types (`@dt`, `@date`, ...) are encoded inline.

### Decode to JSON string (PG storage path)

The direct path for PostgreSQL storage, `decode_zodb_record_for_pg_json`,
writes JSON tokens straight from the `PickleValue` AST into a thread-local
buffer, entirely in Rust with the GIL released. Compared with the dict
variant `decode_zodb_record_for_pg` plus `json.dumps()`, 1.7.0 without PGO
(means, as `bench.py pg-compare` prints them; the dict decode here is the
PG variant, not the `decode_zodb_record` median of the decode table):

| Category | `decode_zodb_record_for_pg` | plus `json.dumps` | `decode_zodb_record_for_pg_json` | Pipeline speedup |
|---|---|---|---|---|
| simple_flat_dict | 1.2 us | 3.1 us | 1.1 us | 2.8x |
| nested_dict | 2.0 us | 4.6 us | 1.6 us | 2.9x |
| large_flat_dict | 14.9 us | 32.6 us | 13.4 us | 2.4x |
| bytes_in_state | 1.8 us | 6.2 us | 1.8 us | 3.4x |
| special_types | 3.6 us | 6.5 us | 3.0 us | 2.2x |
| btree_small | 1.4 us | 3.5 us | 1.1 us | 3.2x |
| btree_length | 0.5 us | 1.7 us | 0.7 us | 2.4x |
| scalar_string | 0.5 us | 0.9 us | 0.7 us | 1.3x |
| wide_dict | 161.9 us | 239.5 us | 111.2 us | 2.2x |
| deep_nesting | 9.4 us | 16.8 us | 7.1 us | 2.4x |

## FileStorage scan (real-world data)

1,692 records from a generated Wikipedia-style database, 6 distinct classes, 0 errors; per record:

| Operation | CPython pickle | v1.5.0 | 1.6.1 | 1.7.0 | 1.7.0 PGO | 1.7.0 vs pickle |
|---|---|---|---|---|---|---|
| decode | 23.9 us | 28.1 us | 42.0 us | 15.3 us | 14.1 us | 1.6x |
| encode | 21.1 us | 5.31 us | 5.50 us | 4.85 us | 4.20 us | 4.4x |
| roundtrip* | 45.0 us | 36.2 us | 50.9 us | 19.4 us | 17.5 us | 2.3x |

\* `bench.py` times no pickle round trip; the CPython value is the sum of its
decode and encode medians, the codec values are one timing of decode plus
encode per record.

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
| Mean | 33.8 us | 14.2 us | 2.4x |
| Median | 25.9 us | 10.7 us | 2.4x |
| P95 | 60.5 us | 29.0 us | 2.1x |

### 1,692 records, 1.7.0 PGO

| Metric | Dict path + `json.dumps` | JSON path | Speedup |
|---|---|---|---|
| Mean | 32.9 us | 12.8 us | 2.6x |
| Median | 24.9 us | 9.6 us | 2.6x |
| P95 | 60.8 us | 28.1 us | 2.2x |

Mean, median and P95 are each the minimum of that statistic over the three
rounds.

### PGO and the two paths

The release wheels are PGO builds (`release.yml` profiles the FileStorage and
synthetic dict-path benchmarks and the PG comparison; the recipe is in
{doc}`/how-to/run-benchmarks`). Until this release PGO made the Python-dict
paths 0 to 15% faster and encode up to 25% faster, but the PG JSON pipeline on
the FileStorage sample slower: median 9.7 to 10.9 us, P95 25.8 to 37.4 us,
with every synthetic JSON category equal or faster. The cause was the byte
loop of the JSON string writer, which the profile-guided build compiled up to
1.8x slower for long strings without escapes
([#52](https://github.com/bluedynamics/zodb-json-codec/issues/52), journal
entry 22). The writer now scans eight bytes at a time with a SWAR mask, and
the tables above are measured with that scan: the PGO build is the faster one
on the storage path as well (JSON path median 10.7 to 9.6 us; the P95, 29.0
to 28.1 us, is inside the noise floor) and the Python-dict paths keep their
PGO gain. The PGO column is what
the PyPI wheels deliver and what zodb-pgjsonb sees.

### Allocator notes for operators

Since 1.7.0 the Rust side uses [mimalloc](https://github.com/microsoft/mimalloc)
as its global allocator ([#24](https://github.com/bluedynamics/zodb-json-codec/issues/24),
journal entry 21 has the before-and-after numbers); Python objects keep using
pymalloc. The wheel grows by about 170 KB, building from source needs a C
compiler, and the measured allocator is mimalloc 3.3.2 (crate `mimalloc`
0.1.52 with `libmimalloc-sys` 0.1.49), pinned exactly in `Cargo.toml`. It is
built with local-dynamic thread-local storage: the module is loaded with
`dlopen`, and initial-exec TLS would take part of glibc's fixed static TLS
surplus, which can make later imports fail with "cannot allocate memory in
static TLS block"; that costs 5 to 12% of decode time against an initial-exec
build and is kept for the import safety.

Memory behaviour differs from glibc malloc in two ways an operator will see on
RSS graphs. Peak RSS while decoding one very large record is up to twice as
high (a 41 MB pickle: about 600 MB extra with glibc, about 1 GB with mimalloc),
and after such a record mimalloc returns the freed pages lazily: a worker that
decodes one huge record and then only idles keeps that memory until the next
medium-sized activity, while under normal traffic it drops back within about a
second (glibc never returns most of it). `MIMALLOC_PURGE_DELAY=10`
(milliseconds) makes the release immediate at a small cost;
`MIMALLOC_SHOW_STATS=1` prints allocator statistics at exit.

mimalloc has no fork handlers: a process that forks while another thread is
decoding with the GIL released (the `multiprocessing` fork start method, for
example) can deadlock in the child on its next Rust allocation, a case glibc
malloc handles. Zope and Plone workers are threads, not forks.

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
| Encode | 9.3x faster | 1.6x faster | 4.4x faster |
| PG JSON path vs dict path + `json.dumps` | 3.4x faster | 1.3x faster | 2.4x faster (median) |

The sweet spot is the typical ZODB object: 5 to 50 keys, mixed types,
datetime fields, persistent references. The remaining cost on decode is the
conversion into Python objects; the PG JSON path avoids it.

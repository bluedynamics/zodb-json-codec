# Performance

<!-- diataxis: explanation -->

This page summarizes the codec's current benchmark results and provides context
for interpreting them. For the full optimization history, see
{doc}`optimization-journal`. For instructions on running benchmarks yourself,
see {doc}`/how-to/run-benchmarks`.

## Why the codec exists

The codec does fundamentally more work than `pickle.loads` / `pickle.dumps`:

- **Pickle** (CPython C extension): one conversion, bytes to Python objects or
  back. A single C function call per direction.
- **Codec**: pickle bytes to Rust `PickleValue` AST to Python dict or JSON
  string (two conversions), plus type-aware transformation for datetimes,
  Decimals, BTrees, persistent references, and other types without direct JSON
  equivalents.

The codec's value is not raw speed but **JSONB queryability** -- enabling SQL
queries on ZODB object attributes in PostgreSQL. Despite the extra work, the
Rust implementation beats CPython pickle on encode and roundtrip across all
categories, and on decode for all but the largest string-dominated payloads.

:::{important}
Always benchmark with release builds. Debug builds are 3-8x slower due to
missing optimizations and inlining:

```bash
maturin develop --release
```

For production-accurate numbers, use PGO builds. See {doc}`/how-to/run-benchmarks`.
:::

## Synthetic micro-benchmarks

Measured with 5,000 iterations and 100 warmup on Python 3.13, PyO3 0.28, PGO
build with LTO and `codegen-units=1`.

### Decode (pickle bytes to Python dict)

| Category | Python | Codec | Ratio |
|---|---|---|---|
| simple_flat_dict (120 B) | 1.9 us | 1.0 us | **1.9x faster** |
| nested_dict (187 B) | 2.7 us | 1.6 us | **1.3x faster** |
| large_flat_dict (2.5 KB) | 22.6 us | 18.0 us | **1.3x faster** |
| bytes_in_state (1 KB) | 1.6 us | 1.4 us | **1.1x faster** |
| special_types (314 B) | 6.8 us | 3.8 us | **1.8x faster** |
| btree_small (112 B) | 1.7 us | 1.5 us | **1.2x faster** |
| btree_length (44 B) | 1.0 us | 0.4 us | **2.3x faster** |
| scalar_string (72 B) | 1.1 us | 0.5 us | **2.2x faster** |
| wide_dict (27 KB) | 250 us | 244.5 us | **1.0x** |
| deep_nesting (379 B) | 6.9 us | 6.4 us | 1.0x |

The codec is fastest on small to medium dicts with mixed types (the typical
ZODB case). The advantage narrows on large string-dominated payloads where
string allocation dominates -- the PyO3 boundary crossing cost for each
Python `str` is the primary bottleneck.

### Encode (Python dict to pickle bytes)

| Category | Python | Codec | Ratio |
|---|---|---|---|
| simple_flat_dict | 1.3 us | 0.2 us | **6.7x faster** |
| nested_dict | 1.6 us | 0.3 us | **6.4x faster** |
| large_flat_dict | 5.7 us | 1.6 us | **3.9x faster** |
| bytes_in_state | 1.3 us | 0.8 us | **1.7x faster** |
| special_types | 4.6 us | 0.5 us | **9.2x faster** |
| btree_small | 1.3 us | 0.2 us | **6.6x faster** |
| btree_length | 1.0 us | 0.1 us | **8.0x faster** |
| scalar_string | 1.0 us | 0.1 us | **7.9x faster** |
| wide_dict | 56.9 us | 13.7 us | **4.1x faster** |
| deep_nesting | 2.6 us | 1.0 us | **2.6x faster** |

Encode is consistently faster because the Rust encoder writes pickle opcodes
directly from Python objects, bypassing all intermediate data structures.
Known types (`@dt`, `@date`, etc.) are encoded inline without allocating
`PickleValue` nodes. Thread-local buffer reuse and class pickle caching
further reduce allocation overhead.

### Decode to JSON string (PG storage path)

The direct path for PostgreSQL storage -- writes JSON tokens directly from the
`PickleValue` AST to a `String` buffer, entirely in Rust with the GIL
released. Compared against the dict path plus `json.dumps()`.

| Category | Dict+dumps | JSON str | Speedup |
|---|---|---|---|
| simple_flat_dict | 2.7 us | 1.1 us | **2.5x faster** |
| nested_dict | 4.3 us | 1.9 us | **2.3x faster** |
| large_flat_dict | 33.7 us | 17.1 us | **2.0x faster** |
| bytes_in_state | 5.2 us | 1.6 us | **3.3x faster** |
| special_types | 7.5 us | 4.0 us | **1.9x faster** |
| wide_dict | 290.5 us | 161.6 us | **1.8x faster** |
| deep_nesting | 14.2 us | 5.7 us | **2.5x faster** |

This path eliminates two sources of overhead: the Python dict allocation
(PyO3 boundary crossing) and the `json.dumps()` serialization. The entire
pipeline runs in Rust.

## FileStorage scan (real-world data)

1,692 records from a generated Wikipedia database, 6 distinct types, 0 errors.

| Metric | Codec | Python | Speedup |
|---|---|---|---|
| Decode mean | 27.2 us | 22.7 us | 1.2x slower |
| Decode median | 23.6 us | 22.2 us | 1.1x slower |
| Decode P95 | 40.5 us | 33.1 us | 1.2x slower |
| Encode mean | 4.8 us | 18.2 us | **3.8x faster** |
| Encode median | 4.0 us | 19.9 us | **5.0x faster** |
| Encode P95 | 9.9 us | 30.0 us | **3.0x faster** |

Decode is slightly slower on real-world data (1.1x median) because these
records are dominated by `PersistentMapping` with long text strings, where
string allocation is the bottleneck. Encode is consistently **3.0-5.0x
faster** because the Rust encoder writes pickle opcodes directly.

### Record type distribution

| Record type | Count | % |
|---|---|---|
| persistent.mapping.PersistentMapping | 1,188 | 70.2% |
| BTrees.OOBTree.OOBucket | 342 | 20.2% |
| persistent.list.PersistentList | 100 | 5.9% |
| BTrees.OOBTree.OOBTree | 55 | 3.3% |
| BTrees.Length.Length | 5 | 0.3% |
| BTrees.OIBTree.OIBTree | 2 | 0.1% |

## PG storage path (full pipeline)

The PostgreSQL storage backend has two decode functions:

```
Dict path:   pickle bytes -> Rust AST -> Python dict (GIL held) -> json.dumps() -> PG
JSON path:   pickle bytes -> Rust AST -> JSON string (direct write, GIL released) -> PG
```

### 1,692 records

| Metric | Dict+dumps | JSON str | Speedup |
|---|---|---|---|
| Mean | 40.4 us | 28.3 us | **1.4x faster** |
| Median | 34.7 us | 24.4 us | **1.4x faster** |
| P95 | 62.0 us | 51.9 us | **1.2x faster** |

The JSON string path is faster because it eliminates both the Python dict
allocation and the `json.dumps()` serialization. It also releases the GIL for
the entire conversion, improving multi-threaded throughput in Zope/Plone
deployments.

## Output size comparison

| Category | Pickle | JSON | Ratio |
|---|---|---|---|
| simple_flat_dict | 120 B | 110 B | 0.92x |
| nested_dict | 187 B | 156 B | 0.83x |
| large_flat_dict | 2,508 B | 2,197 B | 0.88x |
| bytes_in_state | 1,087 B | 1,414 B | 1.30x |
| special_types | 314 B | 228 B | 0.73x |
| wide_dict | 27,057 B | 15,818 B | **0.58x** |
| deep_nesting | 379 B | 586 B | 1.55x |

JSON is typically smaller than pickle for string-heavy data (wide_dict is 42%
smaller) because pickle includes per-string opcode overhead. JSON is larger for
binary data (base64 encoding adds ~33%) and deeply nested structures (marker
key overhead).

The FileStorage scan shows an overall ratio of 1.41x (7.2 MB JSON vs 5.1 MB
pickle) for the full database, reflecting the mix of string-heavy and
binary-containing records.

## Summary

The sweet spot for the codec is typical ZODB objects: 5-50 keys, mixed types,
datetime fields, persistent references.

| Operation | Best | Worst | Typical ZODB |
|---|---|---|---|
| Decode | **2.3x faster** | Near parity | 1.1-1.9x faster |
| Encode | **9.2x faster** | 1.7x faster | 3-5x faster |
| PG path | **3.3x faster** | 1.2x faster | 1.4x faster |

Decode overhead comes from the two-pass conversion plus type transformation.
On string-dominated payloads this matters more; on metadata-rich records with
mixed types (the typical ZODB case) the codec is competitive or faster.
Encode is consistently faster because the Rust encoder avoids intermediate
allocations entirely.

# Optimization Journal

<!-- diataxis: explanation -->

This is a chronological record of every significant performance optimization
applied to the codec, from the initial v1.0.0 development through v1.5.0.
Each entry describes the technique, explains *why* it helps (the underlying
insight), states the measured impact, and notes any lessons learned.

This document exists as a reference for our future selves. When you wonder
"why does the encode path skip PickleValue for datetimes?" or "why not use
Rc for the decoder stack?", the answer is here.

## Early optimizations (v1.0.0 development)

### 1. PyO3 boundary crossing

**Technique:** Replaced `try_extract()` type dispatch with `is_instance_of()`
checks. Pre-collected `PyList` with `PyList::new()` instead of
empty-plus-append. Eliminated `state.clone()` by taking ownership.

**Why it helps:** `try_extract()` on type mismatch creates and immediately
discards a Python exception object. In a type-dispatch loop that tries string,
then int, then float, etc., most branches fail -- and each failure allocates
an exception on the Python heap. `is_instance_of()` is a cheap pointer
comparison against the type object, with no allocation on failure.

Pre-collecting lists avoids the overhead of repeated `PyList::append()` calls,
each of which crosses the PyO3 boundary. Taking ownership of `state` instead
of cloning avoids a deep copy of the entire PickleValue tree.

**Impact:** ~1.8x faster encode path.

### 2. Bypass serde_json intermediate

**Technique:** Added `src/pyconv.rs` for direct `PickleValue` to/from
`PyObject` conversion, eliminating `serde_json::Value` as an intermediate
layer. Persistent reference compact/expand happens inline during a single tree
walk.

**Why it helps:** The original path was: pickle bytes -> PickleValue ->
serde_json::Value -> Python dict. The serde_json step allocates a second tree
of heap objects (`Value::Object`, `Value::Array`, `Value::String`) that mirror
the PickleValue tree, only to be immediately consumed by the PyO3 conversion.
By going directly from PickleValue to PyObject, we eliminate an entire tree
allocation and traversal.

**Impact:** 2-3.5x faster across most categories.

### 3. Decode path tuning

**Technique:** Single-pass Dict decode, pre-allocated `Vec::with_capacity()`,
set/frozenset move semantics (no `Vec` clone), `StackItem` removal, `@`
prefix fast path to skip marker checks on plain dicts.

**Why it helps:** The decoder's inner loop runs once per pickle opcode. Each
opcode that pushes to the stack or memo must be as cheap as possible. Pre-
allocating vectors avoids repeated reallocation during growth. Move semantics
for sets avoids cloning the element vector when constructing `FrozenSet` from
`Set`. The `@` prefix check lets >99% of ZODB dicts (which have plain string
keys like `"title"`, `"description"`) skip all 15+ marker key comparisons.

**Impact:** Codec competitive with CPython pickle decode.

### 4. Encode marker fast path

**Technique:** Single-key dicts use direct key extraction plus match instead
of 15 sequential `dict.get_item()` calls. 2-4 key dicts use a single-pass
`@` scan that builds pairs inline. Multi-key dicts check `@cls` first.

**Why it helps:** In ZODB records, dicts containing JSON markers are the
norm: every record has `{"@cls": ..., "@s": ...}`, and state dicts contain
`{"@dt": ...}`, `{"@ref": ...}`, etc. The original code called
`dict.get_item()` for each possible marker key. Each call crosses the PyO3
boundary and does a Python dict lookup. For a simple `{"@dt": "..."}` dict,
that was 15+ failed lookups plus 1 success.

The fast path reduces this: a single-key dict does 1 key extraction + 1 Rust
string match. A `{"@cls": ..., "@s": ...}` dict does a single scan for `@`
prefixed keys and matches on the first character after `@`.

**Impact:** Reduced `get_item()` calls for `@cls` + `@s` from ~16 to 2.

### 5. Direct PyObject encoder

**Technique:** `pyconv.rs` encode path writes pickle opcodes straight from
Python objects, bypassing the PickleValue tree allocation. Handles common
types directly, falls back for complex markers.

**Why it helps:** The original encode path was: Python dict -> PickleValue
tree -> pickle bytes. The PickleValue tree is a complete copy of the data
structure in Rust heap memory, allocated node by node, only to be immediately
serialized and dropped. By writing opcodes directly from the Python objects,
we eliminate the entire intermediate tree.

**Impact:** Encode competitive with CPython C pickler.

### 6. Shared ZODB memo

**Technique:** Single Decoder instance processes both class and state pickles,
preserving memo entries across the boundary.

**Why it helps:** In ZODB's two-pickle format, the class pickle may memo
values (especially the class reference itself) that the state pickle references
via `GET`/`BINGET`. Previously decoding each pickle with a fresh decoder lost
these memo entries, causing lookup failures or redundant decoding.

**Impact:** Correctness fix that also avoids redundant work.

## v1.2.0

### 7. GIL release

**Technique:** Call `py.detach()` during pure-Rust phases: pickle parsing,
class extraction, reference collection. No Python API calls happen in these
phases.

**Why it helps:** Zope/Plone deployments typically run multiple threads. While
the codec is parsing pickle bytes (pure Rust computation), holding the GIL
blocks all other Python threads. Releasing it lets the web server handle other
requests concurrently.

**Impact:** Other Python threads can run during CPU-bound parse phases. No
single-thread speedup, but significant throughput improvement in
multi-threaded deployments.

### 8. Single-pass PG decode

**Technique:** `decode_zodb_record_for_pg()` combines decode + persistent
reference extraction + null-byte sanitization in one function, eliminating two
separate Python-level tree walks.

**Why it helps:** The PostgreSQL storage backend previously called three
functions: decode the record (tree walk 1), extract persistent references
(tree walk 2), and sanitize null bytes for JSONB compatibility (tree walk 3).
Each tree walk crosses the PyO3 boundary for every node. Combining them into
a single Rust function that does all three operations in one pass eliminates
two full traversals.

**Impact:** 2.5x faster PG path (0.35ms to 0.14ms per 100 objects).

## v1.3.0

### 9. Box Instance variant

**Technique:** Changed `Instance(InstanceData)` to `Instance(Box<InstanceData>)`
in the `PickleValue` enum.

**Why it helps:** Rust enums are sized to their largest variant. `InstanceData`
contains a `Global` (two `String`s) plus a `PickleValue` for state, making it
the largest variant at 56 bytes. Since every `PickleValue` node pays this size
cost regardless of which variant it actually is, the 56-byte enum wastes space
for the common scalar variants (`Int`, `Float`, `Bool`, `None`, `String`).

Boxing the Instance data behind a pointer reduces the enum to 48 bytes. The
Instance variant itself becomes a single pointer (8 bytes). This is a net win
because Instance values are rare compared to scalars, so the per-node savings
(8 bytes * thousands of nodes) far exceed the occasional extra indirection.

Smaller enum -> fewer cache misses, less stack/memo memory, better CPU cache
utilization.

**Impact:** -13% weighted benchmark improvement.

## v1.3.1

### 10. Thin LTO + codegen-units=1

**Technique:** Set `lto = "thin"` and `codegen-units = 1` in the Cargo release
profile.

**Why it helps:** By default, `cargo` splits a crate into multiple codegen
units for parallel compilation. This prevents the optimizer from inlining
across unit boundaries. Setting `codegen-units = 1` gives the optimizer
visibility into the entire crate. Thin LTO extends this across crate
boundaries (e.g., into `serde`, `ryu`, `base64`), enabling cross-crate
inlining without the full-program compile time of fat LTO.

This is zero code changes for measurable improvement.

**Impact:** Free 6-9% on both decode and encode.

**Lesson:** Always enable thin LTO for release builds. The compile time
increase is modest and the performance gain is consistent.

## v1.4.0

### 11. Direct pickle-to-JSON string path

**Technique:** `decode_zodb_record_for_pg_json()` converts ZODB pickle records
entirely in Rust, producing a JSON string directly. The GIL is released for
the entire conversion.

**Why it helps:** The previous PG path was: pickle bytes -> Rust AST ->
Python dict (GIL held) -> `json.dumps()` (GIL held) -> JSON string. This
crossed the PyO3 boundary for every value (building the Python dict) and then
did a second full traversal in Python's `json.dumps()`.

The new path is: pickle bytes -> Rust AST -> JSON string (all in Rust, GIL
released). No Python objects are allocated. No PyO3 boundary crossings. No
second traversal. The JSON string is returned to Python as a single string
allocation.

**Impact:** 1.3x faster full pipeline on real-world data, plus improved
multi-threaded throughput from GIL release.

## v1.5.0 -- Encode Performance Rounds 1-4

### Round 1

#### 12. BigInt elimination

**Technique:** Direct `i64` LONG1 encoding from `i64::to_le_bytes()` with
sign-extension trimming. Buffer `reserve()` before multi-part writes.
O(1) `@cls` hash lookup replaces O(n) key scan.

**Why it helps:** The pickle LONG1 opcode stores arbitrary-precision integers
as little-endian bytes with a length prefix. The original code used the
`num-bigint` crate, which heap-allocates a `BigInt` for every integer, even
though 99%+ of ZODB integers fit in `i64`. Direct `i64` encoding avoids the
heap allocation entirely: convert to bytes, trim trailing sign-extension bytes,
write.

Buffer `reserve()` before writing multi-byte sequences (opcode + length +
data) eliminates mid-write `Vec` reallocations. The reallocation cost is not
just the `memcpy` but also the allocator overhead of finding a larger block.

The `@cls` hash lookup replaces a linear scan over all possible marker keys
when encoding dicts with many keys. For typical 20-50 key ZODB state dicts,
this replaces ~20 string comparisons with a single hash probe.

**Impact:** Measurable improvement across all encode benchmarks, especially
for integer-heavy and large-dict payloads.

#### 13. Profile-Guided Optimization (PGO)

**Technique:** Two-pass instrumented build. First pass collects execution
profiles using real FileStorage data (5 MB Wikipedia database, 1,692 records)
plus synthetic benchmarks. Second pass uses the profiles to optimize branch
prediction, function layout, and inlining decisions.

**Why it helps:** The compiler normally makes generic optimization decisions
based on heuristics. PGO tells it which branches are actually taken, which
functions are actually hot, and which call patterns actually occur. For a codec
that has many dispatch branches (opcode dispatch, type dispatch, marker
dispatch), this information is especially valuable.

Using real FileStorage data alongside synthetic data ensures the profiles
reflect actual ZODB usage patterns (dominated by PersistentMapping and
OOBucket records with mixed string/int/datetime fields).

**Impact:** Additional 5-15% on top of code optimizations. Integrated into CI
release workflow.

**Lesson:** PGO with real data profiles gives better results than
synthetic-only. The synthetic benchmarks exercise all code paths equally, but
real data has strong skew (70% PersistentMapping, 20% OOBucket) that PGO
can exploit.

### Round 2

#### 14. Direct known-type encoding

**Technique:** For `@dt`, `@date`, `@time`, `@td`, `@dec` markers: write
pickle opcodes directly to the output buffer instead of allocating
`PickleValue` intermediate nodes. Inline the timezone REDUCE chain for
`@dt`.

**Why it helps:** A datetime encode previously built a `PickleValue::Instance`
containing a `PickleValue::Global` and a `PickleValue::Tuple` of arguments --
at least 6 heap allocations -- only to immediately serialize them to opcodes
and drop them. Direct encoding writes the same opcodes without any
intermediate allocation.

The timezone REDUCE chain (which encodes `pytz.timezone("UTC")` or similar)
is particularly expensive to build as PickleValue nodes because it nests
multiple GLOBAL + REDUCE operations. Writing the opcodes inline is a single
sequence of buffer writes.

**Impact:** special_types **9.2x faster** than Python pickle.

#### 15. Thread-local buffer reuse

**Technique:** `ENCODE_BUF` thread-local `Vec<u8>` retains capacity across
calls. On each encode, the buffer is cleared (length reset to 0) but the
allocated memory is kept. The same pattern was later applied to the JSON
writer path.

**Why it helps:** Each ZODB transaction encodes dozens to thousands of objects.
Without buffer reuse, each encode allocates a fresh `Vec`, grows it through
several reallocations as data is written, and frees it. With reuse, the first
encode in a transaction grows the buffer to its final size, and all subsequent
encodes reuse that capacity with zero allocation.

For a typical Plone page save that writes 20 objects, this eliminates ~20
allocations + ~40 reallocations (assuming 2 reallocations per encode on
average).

**Impact:** Significant improvement on repeated encode calls. The benefit
compounds with transaction size.

**Lesson:** Thread-local buffers are a big win for hot paths called thousands
of times per transaction. The pattern is simple, safe (no cross-thread
sharing), and the memory cost is bounded (one buffer per thread).

### Round 3

#### 16. Direct JSON string writer

**Technique:** `json_writer.rs` writes JSON tokens directly from the
`PickleValue` AST to a `String` buffer. Fast-path string escaping (scan for
characters that need escaping, fast-copy runs of safe characters). `ryu` crate
for float-to-string conversion.

**Why it helps:** The serde path was: PickleValue -> serde_json::Value (heap
tree) -> serde_json::to_string (traversal + formatting). The direct writer
does: PickleValue -> JSON tokens written to String (single traversal, no
intermediate tree).

The `serde_json::Value` tree mirrors the PickleValue tree in structure but
uses different types (`Value::Object(Map<String, Value>)` vs
`PickleValue::Dict(Vec<(PickleValue, PickleValue)>)`). Eliminating it removes
an entire tree of heap allocations.

Fast-path string escaping checks whether a string contains any characters
that need JSON escaping (`"`, `\`, control characters). For the common case
(ASCII strings with no special characters), it writes the string with a single
`memcpy` instead of character-by-character escaping. The `ryu` crate converts
floats to strings without going through `format!()`, avoiding a temporary
`String` allocation.

**Impact:** wide_dict -55%, PG path 1.4x faster.

### Round 4

#### 17. Class pickle cache

**Technique:** Thread-local cache per `(module, name)` pair. Linear search
over ~6 entries (faster than `HashMap` for small sets). First call builds and
caches ~50 bytes of pickle opcodes, subsequent calls do a single `memcpy`.

**Why it helps:** Every ZODB record starts with a class pickle that encodes
the object's Python class. In a typical Plone database, 70%+ of records are
`persistent.mapping.PersistentMapping`, meaning the same ~50-byte class pickle
is regenerated thousands of times. The class pickle is deterministic (same
module + name always produces the same bytes), so caching it is safe.

Linear search over 6 entries is faster than `HashMap` because there is no
hashing overhead and the small array fits in a single cache line. The typical
ZODB database has only 5-10 distinct classes, so the cache is both small and
highly effective.

**Impact:** ~2-4% encode improvement, 99.6% cache hit rate on real data.

## Cumulative result

| Operation | vs CPython pickle |
|---|---|
| Encode (synthetic) | 1.7-9.2x faster |
| Encode (real FileStorage) | 3-5x faster |
| Decode (synthetic) | 1.0-2.3x faster |
| Decode (real FileStorage) | Near parity |
| PG JSON path | 1.3-3.3x faster, GIL-free |

## Lessons learned

The following insights emerged from the optimization work and should inform
future changes:

**Rc\<PickleValue\> for stack+memo was a regression.**
: Early experiments tried `Rc<PickleValue>` to share values between the stack
  and memo without cloning. The result was *slower* because `Rc::new()` per
  stack push adds a heap allocation that exceeds the savings from avoiding
  memo clones. Most values in a pickle stack machine are created once and
  consumed once -- they are never actually shared. Value semantics beat
  reference counting when sharing is rare.

**The decode bottleneck is string allocation, not memo cloning.**
: Profiling showed that string allocation (both Rust `String` and Python
  `str` via PyO3) dominates decode time. Optimizing memo handling or stack
  operations yields diminishing returns once the string path is fast.

**Thin LTO is essentially free performance.**
: `lto = "thin"` adds modest compile time but consistently yields 6-9%
  runtime improvement. There is no reason not to enable it for release builds.

**PGO with real data is better than synthetic-only.**
: Synthetic benchmarks exercise all code paths equally. Real ZODB data has
  strong type skew (70% PersistentMapping, 20% OOBucket) that PGO exploits
  for better branch prediction. Always include real data in PGO profiles.

**Thread-local buffers compound across transactions.**
: The first encode in a transaction pays the allocation cost; all subsequent
  encodes are nearly free. For large transactions (bulk imports, catalog
  rebuilds), this is a significant win.

**Small-set linear search beats HashMap.**
: For the class pickle cache (5-10 entries), linear search over a `Vec` is
  faster than `HashMap` because there is no hash computation and the entire
  array fits in L1 cache. Only switch to `HashMap` if the working set exceeds
  ~20 entries.

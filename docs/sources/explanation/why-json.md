# Why Convert Pickle to JSON?

<!-- diataxis: explanation -->

## The problem: pickle is opaque

ZODB stores every persistent object's state as Python pickle bytes. Pickle is
an excellent serialization format for Python: the CPython implementation is a C
extension that serializes and deserializes Python objects in a single pass, and
it handles arbitrary object graphs including cycles and shared references.

But pickle is **completely opaque to anything that is not Python**. A PostgreSQL
database can store pickle bytes in a `BYTEA` column, but it cannot look inside
them. You cannot write a SQL `WHERE` clause that filters on an object's
`title` attribute, build a GIN index on a list field, or join two object tables
on a shared key. Every query must deserialize the full pickle on the Python
side, which means full table scans and no database-level indexing.

This is the fundamental limitation that the codec addresses.

## The solution: transcode to JSON

By converting pickle bytes to JSON *at write time*, we store ZODB object state
in PostgreSQL's `JSONB` column type. JSONB is a structured, indexed binary
format that PostgreSQL understands natively. This unlocks:

SQL queries on object attributes
: `SELECT * FROM object_state WHERE state @> '{"title": "Welcome"}'`

GIN indexes for fast lookups
: `CREATE INDEX idx_state ON object_state USING GIN (state)`

JSON path expressions
: `SELECT state -> '@s' ->> 'title' FROM object_state`

Cross-object joins
: Join on any shared attribute without deserializing in Python.

Full-text search integration
: Extract text fields into `tsvector` columns for native PostgreSQL FTS.

All of this works **without changing application code**. The codec sits
between ZODB and PostgreSQL as a transparent transcoding layer. Application
code continues to use standard ZODB APIs (`transaction.commit()`,
`connection.root()`, persistent objects), while the storage backend
transparently converts pickle to JSON on write and JSON back to pickle on
read.

## The tradeoff: more work per record

The codec does fundamentally more work than plain pickle:

- **Pickle** (CPython C extension): one pass, bytes to Python objects or back.
  A single C function call per direction.
- **Codec**: two conversions per direction (pickle bytes to Rust AST to
  JSON/Python dict), plus type-aware transformation for datetimes, Decimals,
  BTrees, persistent references, and other Python types that have no direct
  JSON equivalent.

This is not a free lunch. The extra conversions add CPU cost per record. For
string-dominated payloads where pickle's C extension does very little work per
byte, the overhead is measurable.

## Compensating with Rust

The codec compensates for the extra work through its Rust implementation:

- The pickle decoder and JSON writer run entirely in Rust with the GIL
  released, enabling other Python threads to work in parallel.
- The encoder writes pickle opcodes directly from Python objects, bypassing
  intermediate data structures.
- Known types (datetime, Decimal, UUID, etc.) are handled inline during the
  single tree walk, avoiding separate conversion passes.
- Thread-local buffer reuse eliminates repeated allocation for hot paths.
- Profile-guided optimization (PGO) with real ZODB data tunes branch
  prediction and inlining for actual usage patterns.

The result: on typical ZODB objects (5-50 keys, mixed types, datetime fields,
persistent references), the codec **matches or beats CPython pickle** on most
operations. Encode is consistently 1.7-9.2x faster; decode ranges from near
parity to 2.3x faster depending on payload shape. On the full PostgreSQL
storage path, the direct JSON writer is 1.3-3.3x faster than the dict path
plus `json.dumps()`.

## When the tradeoff pays off

The codec is designed for ZODB storage backends where queryability matters more
than raw per-record throughput. The typical deployment is a Zope/Plone
application where:

- Object reads and writes already go through Python, so the codec overhead is
  a small fraction of total request time.
- The ability to query object attributes via SQL eliminates the need for
  maintaining separate catalog indexes for many use cases.
- GIN indexes on JSONB provide sub-millisecond lookups that would otherwise
  require full Python-side scans.
- Multi-threaded Zope deployments benefit from GIL-free Rust phases, which
  improve throughput even when individual record latency is similar.

For use cases that need maximum per-record throughput with no queryability
requirement, plain pickle with a traditional ZODB storage (FileStorage,
RelStorage with `BYTEA`) remains the simpler choice.

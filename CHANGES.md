# Changelog

## 1.5.0 (unreleased)

- Direct PickleValue → JSON string writer (`json_writer.rs`), bypassing
  all `serde_json::Value` intermediate allocations (PG path 1.3-3.3x
  faster than dict + `json.dumps()`)
- Direct known-type encoding for datetime, date, time, timedelta, and
  Decimal — writes pickle opcodes inline, skipping PickleValue intermediate
- Thread-local buffer reuse for both encode and JSON writer paths
- Thread-local class pickle cache per (module, name) pair — single memcpy
  replaces 7 opcode writes for ~99.6% of records
- O(1) `@cls` hash lookup replaces O(n) key scan for marker detection
- Direct i64 LONG1 encoding (eliminates BigInt heap allocation)
- Profile-guided optimization (PGO) support with real FileStorage +
  synthetic data profiling (adds 5-15%)

### Performance (PGO build, vs CPython pickle)

- Encode: 1.7-9.2x faster (synthetic), 3-5x faster (real FileStorage)
- Decode: 1.0-2.3x faster (synthetic), near parity on real-world data
- PG JSON path: 1.4x faster at median on 1,692 real ZODB records
- Full codec overhead: ~28 µs per object (both directions)

## 1.4.0 (2026-02-24)

- Add `decode_zodb_record_for_pg_json()` — converts ZODB pickle records
  directly to a JSON string entirely in Rust with the GIL released,
  eliminating the intermediate Python dict + `json.dumps()` step
  (1.3x faster full pipeline on real-world data)
- Enable thin LTO (`lto = "thin"`) and single codegen unit
  (`codegen-units = 1`) in Cargo release profile for 6-9% faster
  decode/encode

## 1.3.0 (2026-02-24)

- Fix SETITEMS/SETITEM/APPENDS/APPEND on dict/list subclasses (OrderedDict,
  defaultdict, deque, etc.) — previously crashed with
  `ValueError: SETITEMS on non-dict` [#5]
- Box Instance variant as `Instance(Box<InstanceData>)`, reducing PickleValue
  enum from 56 to 48 bytes (-13% weighted benchmark improvement)

## 1.2.2 (2026-02-22)

Security review fixes (addresses #3):

- **CODEC-C1:** Validate non-negative length in LONG4 and BINSTRING opcodes.
- **CODEC-C2:** Cap memo size at 100,000 entries to prevent OOM via LONG_BINPUT.
- **CODEC-H1:** Add recursion depth limit (1,000) to encoder and PyObject converter.
- **CODEC-H2:** Pre-scan dict keys to avoid quadratic re-processing of mixed-key dicts.
- **CODEC-M1:** Limit LONG opcode text representation to 10,000 characters.
- **CODEC-M2:** Reject odd-length item lists in BTree bucket `format_flat_data()`.
- **CODEC-M3:** Cap BINUNICODE8/BINBYTES8 length at 256 MB before allocation.

## 1.2.1 (2026-02-17)

- Fix shared reference data loss: update memo after BUILD [#2]

## 1.2.0 (2026-02-10)

- Release GIL during pure-Rust pickle decoding phases, allowing other
  Python threads to run during the CPU-bound parse
- Add `decode_zodb_record_for_pg` for single-pass PG optimization
  (combines decode + ref extraction + null-byte sanitization)

## 1.1.0

- Add builds for Python 3.14

## 1.0.0

### Features

- Pickle protocol 2-3 support (ZODB standard), partial protocol 4 support
- ZODB two-pickle record format with shared memo between class and state pickles
- Compact JSON markers for Python types without direct JSON equivalents:
  `@t` (tuple), `@b` (bytes), `@set`, `@fset`, `@dt` (datetime), `@date`,
  `@time`, `@td` (timedelta), `@dec` (Decimal), `@uuid`, `@ref` (persistent ref)
- Known type handlers for datetime (with full timezone support), date, time,
  timedelta, Decimal, UUID, set, frozenset
- BTree support: flattened JSON with `@kv`, `@ks`, `@children`, `@first`, `@next` markers
- Escape hatch: unknown types safely encoded as `@pkl` (base64 pickle fragment)
- Full roundtrip fidelity: encode to JSON and decode back produces identical pickle bytes
- Direct PickleValue to PyObject conversion (bypasses serde_json intermediate layer)
- Direct PyObject to pickle bytes encoder (bypasses PickleValue AST for encode)
- Python 3.10-3.14 support, wheels for Linux/macOS/Windows

### Performance (release build)

- Decode: up to 1.8x faster than CPython pickle, 1.3x typical ZODB
- Encode: up to 7.0x faster than CPython pickle, 4.0x typical ZODB
- On real Plone 6 database (8,400+ records): 1.3x faster decode (median),
  18.7x faster mean; 3.5x faster encode, 0 errors across 182 distinct types

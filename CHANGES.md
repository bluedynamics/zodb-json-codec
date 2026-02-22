# Changelog

## 1.2.2

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

# Changelog

## 0.1.0b1 (unreleased)

Initial beta release.

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

### Performance (release build)

- Decode: up to 1.8x faster than CPython pickle, 1.3x typical ZODB
- Encode: up to 7.0x faster than CPython pickle, 4.0x typical ZODB
- On real Plone 6 database (8,400+ records): 1.3x faster decode (median),
  18.7x faster mean; 3.5x faster encode, 0 errors across 182 distinct types

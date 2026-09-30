# Changelog

## unreleased

- Docs: the round-trip claim now says what holds (an equal object, not
  identical bytes) and lists what differs; the tutorial example is run by a
  test; the architecture page names `decode_zodb_record_for_pg_json` as the
  storage path [#27]

- Docs: the performance page is re-measured on 1.7.0 against CPython pickle,
  v1.5.0 and 1.6.1, with and without PGO, one session, build and protocol
  stated once for every table; the journal's cumulative table follows. PGO
  turns out to slow the PG JSON pipeline on real records (see #52) [#27]

- Release wheels: the PGO profile now includes the PG JSON path
  (`bench.py pg-compare`), which the profile used to leave out. Note that PGO
  still makes the PG JSON pipeline on real records slower than a build without
  PGO (median 9.8 to 10.4 µs, P95 26 to 35 µs) while it speeds up the
  Python-dict paths; the cause is open in #52 [#52]

- Require Python 3.12 or newer: wheels and CI for 3.10 and 3.11 are dropped.
  3.10 reaches end of life on 2026-10-04, and on 3.10/3.11 the cyclic garbage
  collector can run finalizers inside any GC-tracked allocation, a class of
  re-entrancy the extension no longer has to consider; Python 3.15 wheels and
  CI are added (PyO3 0.29) [#41]

- Protocol 0 text opcodes decode like CPython: `STRING` unescapes the bytes repr
  (`\'`, `\xNN`, octal, ...; it used to keep the backslashes) and requires the
  quotes, `UNICODE` is read as raw-unicode-escape (Latin-1 bytes plus `\uXXXX`;
  non-ASCII used to raise `InvalidUtf8` and `\u` escapes stayed literal); lone
  surrogate escapes are rejected, like invalid UTF-8 on the `BINUNICODE` path
  [#25]

- Anonymous instances (`@inst`) round-trip: `BUILD` after a `REDUCE`/`NEWOBJ`
  whose callable is not a global now decodes to `@reduce` with `state` and
  re-encodes faithfully; `@inst` written by earlier releases (`@callable`,
  `@args`, `@state`) and `BUILD` on a non-instance (`@obj`, `@state`) are read
  by all encoder paths, which used to write them back as a plain dict or as a
  `GLOBAL` with empty module and name [#25]

- `encode_zodb_record` no longer panics with `already borrowed` if it is
  re-entered on the same thread while an encode is running: the thread-local
  buffer and class cache fall back to a fresh local one for the inner call
  [#25]

- Decode performance: `LONG1` integers of up to 8 bytes are sign-extended into
  an `i64` directly instead of going through `BigInt` (a record of 2,500 such
  ints decodes 17% faster; the sample database has none) [#26]

- PG JSON performance: persistent references (hex oid and class path), bytes,
  `@ns` strings and keys and `@pkl` are written straight into the output
  buffer instead of through temporary strings (PG JSON pipeline median 17.1 to
  16.4 µs on the sample database, which has 9 references and 12 bytes values
  per record) [#26]

- Decode performance: the decoder keeps one value stack with mark positions
  instead of swapping in a fresh stack (three vectors) at every `MARK`, builds
  dict pairs straight from the closed frame, and reuses its bookkeeping
  vectors per thread across records (vectors that grew past 65,536 entries are
  released instead). Small and nested records decode 15 to 20% faster; a
  1,000-key dict decodes in the Rust core in 97 µs instead of 110 µs. Two
  malformed shapes that used to be accepted now raise like CPython: a `TUPLE`,
  `LIST`, `DICT` or `SETITEMS` without a `MARK` (the whole stack used to be
  taken), and more than 1000 open marks; a second pickle in a record no longer
  inherits an open mark from the first [#26]

- Build: `lto = "fat"` for release builds (FileStorage decode 3% and
  large_flat_dict decode 10% faster than thin LTO, extension 7% smaller,
  release compile takes longer) [#26]

- Decode performance: memo puts that no later `GET`/`BINGET` reads are skipped
  after a pre-scan of the opcode stream. The 1.6.0 memo fix deep-copied every
  container into the memo once per nesting level (typical records decoded 30
  to 55% slower than 1.5.0, deeply nested ones 3.5x slower, and nobody
  re-measured); shared references keep the 1.6.0 behaviour [#22]

- Fix `encode_zodb_record` raising `TypeError` on records that hold ZODB weakref
  (`['w', ...]`) or multi-database (`['m', ...]`, `['n', ...]`) persistent ids;
  the compact `["oid", "module.Class"]` form is now only recognized when both
  elements are strings, all other forms are encoded generically. `refs` now
  includes bare-oid references (classes with `__getnewargs__`) like
  `ZODB.serialize.referencesf` does. Compact refs are validated (8-byte oid,
  class path string). Note for zodb-pgjsonb: rows stored by earlier codec
  versions that contain bare-oid references have an incomplete `refs` column
  until they are rewritten; recompute `refs` before the first pack if such
  classes exist in the database [#17]

- Fix constructor arguments being lost on round trip: objects pickled with
  `NEWOBJ` and non-empty `__getnewargs__` (with or without state) are
  re-emitted with their args and as `NEWOBJ`, objects pickled with
  `REDUCE(cls, args)` followed by `BUILD` keep both; `@reduce` gained the
  additive keys `newobj` and `state`. Class pickles that carry `newargs`
  now raise (naming the class) instead of silently dropping them, and ZODB's
  `(klass, None)` class tuple form is read correctly. Records written before
  this release for stateless `NEWOBJ` objects carry no `newobj` key and keep
  being re-emitted as `REDUCE`; the stored shape cannot tell the two apart [#12]

- Performance: the PG JSON writer's thread-local buffer now really keeps its
  capacity between calls (it was emptied on every return), integers are
  formatted with `itoa`, JSON escaping copies runs of safe bytes instead of
  going character by character once an escape is present, `encode_zodb_record`
  builds its result without an intermediate copy, and the class pickle cache
  is bounded (32 entries, move-to-front) instead of growing without limit
  and being scanned linearly on every encode. PG JSON pipeline median on the
  sample database 39.8 to 35.5 µs, a 10 KB rich-text record 30 to 19 µs; both
  thread-local output buffers are released after a record larger than 4 MiB [#23]

- Performance: mimalloc is the Rust-side global allocator (Python objects keep
  using pymalloc). Decode is allocation-bound, so medium and large records
  decode 20 to 50% faster (FileStorage decode 38.8 to 22.2 µs per record, PG
  JSON pipeline median 39.3 to 20.7 µs), small ones 0 to 10%, encode 0 to 14%.
  The wheel grows by about 170 KB and building from source needs a C compiler.
  Peak RSS on very large records is up to twice glibc's and freed pages are
  released lazily (`MIMALLOC_PURGE_DELAY`), and mimalloc has no fork handlers;
  see the performance page [#24]

- Fix data loss for dict/list subclasses (`OrderedDict`, `defaultdict`, `deque`,
  and user subclasses): the Python-dict decode path now emits their
  `items`/`appends` (`@items`/`@appends` for instances) and both encoder paths
  read them back; the `PickleValue` encoder emits items before `BUILD` like
  CPython [#16]

- Encoder input robustness: Python ints beyond the i64 range now encode as
  `LONG1`/`LONG4` (they used to raise `OverflowError`); objects the encoder does
  not understand (bytes, tuples, sets, datetimes, arbitrary instances) raise
  `TypeError` naming the type instead of being silently pickled as their
  `str()`; dicts with non-string keys (int, float, bool, `None`) no longer
  raise `TypeError` on the `PickleValue` path and for single-key dicts [#20]

- NUL bytes survive the PostgreSQL round trip through the codec alone: the
  `{"@ns": base64}` value marker and the `"@ns:base64"` key form written by the
  PG decode paths are now restored by `encode_zodb_record` and `json_to_pickle`
  (malformed markers raise `ValueError`), and `decode_zodb_record_for_pg` no
  longer raises `TypeError` on dict keys with NUL bytes. Keys that genuinely
  start with `@ns:` are escaped by every decode path so they round-trip too.
  zodb-pgjsonb can drop its Python-side `_unsanitize_from_pg` walk once it
  requires this release [#18]

- Commit `Cargo.lock` so all builds, including the PGO release builds, use the
  same dependency versions; document the benchmark-against-the-previous-release
  step and the lockfile policy in `RELEASE.md` (wheel table now lists Python
  3.14, which the release workflow already builds) [#21]

- Bump PyO3 to 0.29 (fixes Dependabot advisories GHSA-36hh-v3qg-5jq4 and
  GHSA-chgr-c6px-7xpp; neither API was used by the codec). PyO3 0.29 drops
  free-threaded Python 3.13t, which the wheels never targeted; 3.10 to 3.14
  stay supported [#31]

- Nesting depth is bounded everywhere: the decoder refuses pickles that would
  build values nested deeper than 1000 levels (they used to crash the
  interpreter when the value was dropped), and `encode_zodb_record` /
  `dict_to_pickle` refuse Python input nested deeper than 1000 levels (they
  used to overflow the stack); both raise `ValueError`. The bound assumes the
  platform's default thread stack (8 MiB on glibc); threads created with a
  much smaller `threading.stack_size()` can still overflow on deep input, see
  #40 [#19]

- Lower ruff's C901 max-complexity threshold from 15 to 13 as part of the
  ecosystem-wide complexity ratchet. The code base passes as-is.

- Enable ruff's cyclomatic-complexity check (`C901`, mccabe) with
  `max-complexity = 15`. `python/` and `tests/` pass as-is; `benchmarks/`
  is exempted via per-file-ignores (CLI harness code, legitimately
  branchy). The repo had no explicit ruff config before, so the lint rule
  selection is now pinned in `pyproject.toml` (classic ruff defaults plus
  `C901`) and Markdown files are excluded from `ruff format`, keeping
  results stable across ruff releases.

- Add `cdk8s-plone` to the ecosystem navigation dropdown in the docs.

- Add `cloud-vinyl` and `plone.observability` to the ecosystem navigation
  dropdown in the docs.

- Fix Python 3.10 incompatibility in the test suite: replace the
  Python 3.11+ `datetime.UTC` import with `timezone.utc` in
  `tests/test_pg_json.py` and `tests/test_known_types.py` [#10]

## 1.6.1 (2026-02-27)

- CRITICAL Linux!
  The 1.6.0 build failed for Linux and wheels were not uploaded!

## 1.6.0 (2026-02-27)

- CRITICAL!
  Fix shared reference data loss in pickle memo for mutable containers:
  BINPUT stored empty snapshot before SETITEMS/APPENDS populated the
  container, causing BINGET to return stale empty values [#18]
- Optimize memo sync with lazy dirty flags: mark memo entries as stale
  after mutations, resolve only on BINGET read or stack pop (eliminates
  unnecessary clones for memo entries never re-read)
  Correction (1.7.0): the lazy variant still cloned every dirty container
  into the memo when it left the stack, once per nesting level; see the #22
  entry above.
- Clean up dead code and compiler warnings: gate test-only functions with
  `#[cfg(test)]`, prefix unused variables, convert doc comments on macros
  to regular comments

## 1.5.0 (2026-02-25)

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

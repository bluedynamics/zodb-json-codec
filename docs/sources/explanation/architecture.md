# Architecture

<!-- diataxis: explanation -->

This page describes the internal structure of the codec: how data flows through
the system, what each module is responsible for, and why the design looks the
way it does.

## The PickleValue AST

At the center of the codec is the `PickleValue` enum, defined in `types.rs`.
It is a Rust-native abstract syntax tree (AST) that mirrors the semantic
structure of Python pickle data:

- Scalar values: `Int`, `Float`, `Bool`, `None`, `String`, `Bytes`, `BigInt`
- Containers: `List`, `Tuple`, `Dict`, `Set`, `FrozenSet`
- Object types: `Instance` (boxed), `Global`, `PersistentRef`

Every conversion passes through this AST. Pickle bytes decode *into*
`PickleValue`, and encode operations produce pickle bytes *from* `PickleValue`
(or, in the fast encode path, directly from Python objects).

The `Instance` variant is boxed (`Instance(Box<InstanceData>)`) to keep the
enum at 48 bytes instead of 56. Since most `PickleValue` nodes are scalars or
containers, this reduces cache pressure and stack usage across the entire
pipeline.

## Decode pipeline

The decode side has three output paths, all sharing the same initial pickle
parsing step.

```{mermaid}
flowchart LR
    PB["pickle bytes"] --> DEC["Decoder<br/>(decode.rs)"]
    DEC --> PV["PickleValue AST<br/>(types.rs)"]
    PV --> PYCONV["pyconv.rs<br/>PickleValue → PyObject"]
    PV --> JSON["json.rs<br/>PickleValue → serde_json"]
    PV --> JW["json_writer.rs<br/>PickleValue → JSON string"]
    PYCONV --> PD["Python dict"]
    JSON --> JS1["JSON string<br/>(via serde)"]
    JW --> JS2["JSON string<br/>(direct write)"]
```

### Step 1: Pickle parsing (decode.rs)

The decoder implements a pickle virtual machine that reads opcodes and builds
up the `PickleValue` tree. It maintains a stack, a memo (for shared
references), and a metastack (for `MARK`-delimited regions).

Key design choices:

- **Value semantics on the stack.** Each `PickleValue` is owned, not
  reference-counted. Early experiments with `Rc<PickleValue>` showed that the
  heap allocation cost of `Rc::new` per stack push exceeded any savings from
  shared memo references. Most values are created once and consumed once.
- **GIL release.** The parser calls `py.detach()` before entering the
  pure-Rust parsing loop. No Python API calls happen during parsing, so the
  GIL can be released to let other threads run.
- **Shared ZODB memo.** For ZODB records (which contain two concatenated
  pickles), a single `Decoder` instance processes both pickles so that memo
  entries from the class pickle carry over to the state pickle.

### Step 2a: PickleValue to Python dict (pyconv.rs)

The `pyconv.rs` module walks the `PickleValue` tree and constructs Python
objects via PyO3. This is the original and most general output path. It
produces a Python `dict` that the caller can inspect, modify, or serialize.

This path crosses the PyO3 boundary (Rust to Python) for every value,
which requires the GIL. The primary cost is string allocation: every Python
`str` requires a PyO3 call that allocates on the Python heap.

### Step 2b: PickleValue to JSON via serde (json.rs)

The `json.rs` module converts `PickleValue` to `serde_json::Value` and then
serializes to a JSON string. This path was the original JSON output method.
It is still used for the `pickle_to_json()` and `json_to_pickle()` standalone
APIs.

### Step 2c: PickleValue to JSON string (json_writer.rs)

The `json_writer.rs` module writes JSON tokens directly from the `PickleValue`
AST to a `String` buffer. It eliminates all `serde_json::Value` intermediate
allocations and runs entirely in Rust with the GIL released.

This is the fastest decode path and the one used by the PostgreSQL storage
backend (`decode_zodb_record_for_pg_json`). It uses a thread-local buffer that
retains its capacity across calls, avoiding repeated allocation.

## Encode pipeline

The encode side has two paths: a fast direct path and a JSON-to-pickle path.

```{mermaid}
flowchart LR
    PD["Python dict"] --> PYENC["pyconv.rs<br/>PyObject → pickle bytes"]
    JS["JSON string"] --> JDEC["json.rs<br/>JSON → PickleValue"]
    JDEC --> ENC["encode.rs<br/>PickleValue → pickle bytes"]
    PYENC --> OUT["pickle bytes"]
    ENC --> OUT
```

### Direct encode (pyconv.rs)

The primary encode path writes pickle opcodes directly from Python objects,
without constructing a `PickleValue` tree. It dispatches on Python type
(string, int, float, bool, None, list, dict, tuple, bytes) and writes the
appropriate opcodes to a buffer.

For dicts containing JSON marker keys (`@cls`, `@dt`, `@ref`, etc.), the
encoder detects markers via fast-path checks:

- **Single-key dicts:** extract the key directly and match.
- **2-4 key dicts:** single-pass scan for `@` prefixed keys.
- **Larger dicts:** check for `@cls` first (the most common marker in ZODB
  records), then fall through to plain dict encoding.

Known types (`@dt`, `@date`, `@time`, `@td`, `@dec`) are encoded by writing
pickle opcodes inline, without allocating intermediate `PickleValue` nodes.
This eliminates 6 heap allocations per datetime encode.

### JSON-to-pickle (json.rs + encode.rs)

The `json_to_pickle()` path first parses a JSON string into a `PickleValue`
tree via serde, then encodes that tree to pickle bytes via `encode.rs`. This
is used when the input is already a JSON string (e.g., from PostgreSQL).

## Known types (known_types.rs)

Certain Python types have no direct JSON equivalent: `datetime`, `date`,
`time`, `timedelta`, `Decimal`, `UUID`, `set`, `frozenset`. These types appear
in pickle as `REDUCE` operations (a global callable plus an argument tuple).

The `known_types.rs` module intercepts these at the `PickleValue` to JSON
boundary. On decode, it recognizes the `(module, class, args)` pattern and
emits a compact JSON marker (e.g., `{"@dt": "2025-06-15T12:00:00"}`). On
encode, it recognizes the marker and reconstructs the `REDUCE` operation.

This interception happens during the single tree walk, not as a separate pass.

## BTree handling (btrees.rs)

BTrees from the `BTrees` package use complex nested tuple structures for their
internal state. The `btrees.rs` module classifies BTree records and transforms
them between pickle state and a flat JSON representation using markers like
`@kv` (key-value pairs), `@ks` (key sets), `@children` (internal node
references), `@first`, and `@next` (bucket chain pointers).

BTree handling is wired into both the ZODB record path (`zodb.rs`) and the
standalone Instance path (`json.rs`), so BTrees are flattened regardless of
which API is used.

## ZODB records (zodb.rs)

A ZODB record consists of two concatenated pickles:

1. **Class pickle:** identifies the Python class of the persistent object.
   Must use tuple format `((module, name), None)`.
2. **State pickle:** the object's `__getstate__()` result, typically a `dict`.

The `zodb.rs` module manages the two-pickle protocol:

- On decode, it runs the decoder twice (with shared memo), extracts class
  info, and combines the results into `{"@cls": [module, name], "@s": state}`.
- On encode, it generates the class pickle from the `@cls` marker and the
  state pickle from the `@s` value.
- The `decode_zodb_record_for_pg` function combines decode, persistent
  reference extraction, and null-byte sanitization (required for PostgreSQL
  `JSONB`, which cannot store `\u0000`) in a single pass.
- The `decode_zodb_record_for_pg_json` function does the same but outputs a
  JSON string directly, with the GIL released for the entire conversion.

## Module summary

| Module | Responsibility |
|---|---|
| `types.rs` | `PickleValue` enum definition |
| `opcodes.rs` | Pickle opcode constants |
| `decode.rs` | Pickle bytes to `PickleValue` AST |
| `encode.rs` | `PickleValue` AST to pickle bytes |
| `pyconv.rs` | Direct `PickleValue` to/from Python objects; direct encode path |
| `json.rs` | `PickleValue` to/from `serde_json::Value` |
| `json_writer.rs` | Direct `PickleValue` to JSON string writer |
| `known_types.rs` | Known REDUCE handlers (datetime, Decimal, UUID, etc.) |
| `btrees.rs` | BTree state flattening and reconstruction |
| `zodb.rs` | ZODB two-pickle record handling |
| `lib.rs` | PyO3 module definition and Python-facing functions |
| `error.rs` | Error types |

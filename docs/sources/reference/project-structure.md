# Project Structure

<!-- diataxis: reference -->

Source layout and module responsibilities for the zodb-json-codec Rust
crate and Python package.

## Directory Layout

```
src/
  lib.rs            # PyO3 module: Python-facing function definitions
  decode.rs         # Pickle bytes -> PickleValue AST
  encode.rs         # PickleValue AST -> pickle bytes
  pyconv.rs         # Direct PickleValue <-> PyObject (fast path)
  json.rs           # PickleValue <-> serde_json::Value (JSON string path)
  json_writer.rs    # Direct PickleValue -> JSON string writer (PG path)
  known_types.rs    # Known REDUCE handlers (datetime, Decimal, UUID, etc.)
  btrees.rs         # BTree state flattening/reconstruction
  zodb.rs           # ZODB two-pickle record handling
  types.rs          # PickleValue enum definition
  opcodes.rs        # Pickle opcode constants
  error.rs          # Error types
python/
  zodb_json_codec/
    __init__.py     # Re-exports from Rust extension (_rust)
tests/
  test_basic_types.py     # Native types, structural markers
  test_known_types.py     # Datetime, Decimal, UUID, set, frozenset
  test_btrees.py          # BTree flattening and reconstruction
  test_zodb_records.py    # ZODB two-pickle record roundtrips
  test_pg_json.py         # PostgreSQL JSON path functions
benchmarks/
  bench.py          # Performance benchmarks vs CPython pickle
```

## Rust Modules

### `lib.rs` -- PyO3 Module

Defines the Python-facing functions (`#[pyfunction]`) that are exported
as the `zodb_json_codec._rust` extension module. Each function
coordinates the decode/encode pipeline by calling into the appropriate
internal modules. Handles GIL release (`py.detach()`) around pure-Rust
phases.

### `types.rs` -- PickleValue AST

Defines the `PickleValue` enum, the intermediate representation that
sits between pickle bytes and JSON. Every pickle value is represented as
one of: `None`, `Bool`, `Int`, `BigInt`, `Float`, `String`, `Bytes`,
`List`, `Tuple`, `Dict`, `Set`, `FrozenSet`, `Global`, `Instance`,
`PersistentRef`, `Reduce`, or `RawPickle`.

Also defines `InstanceData` (module, name, state, plus optional
`dict_items` and `list_items` for subclass support). The `Instance`
variant is boxed to keep the enum size at 48 bytes.

### `decode.rs` -- Pickle Decoder

Implements a subset of the pickle virtual machine sufficient for ZODB
records (protocol 2-3, partial protocol 4). Reads pickle bytes and
produces a `PickleValue` AST. No Python objects are constructed.

Key functions:

- `decode_pickle(data)` -- decode a single pickle stream.
- `decode_zodb_pickles(data)` -- decode two concatenated pickles with
  shared memo (ZODB record format).

Safety limits: memo capped at 100,000 entries, binary allocations capped
at 256 MB, LONG text at 10,000 characters.

### `encode.rs` -- Pickle Encoder

Converts a `PickleValue` AST back to pickle bytes in protocol 3 (the
maximum supported by zodbpickle). Handles all value types including
`Instance`, `Reduce`, `Global`, and `PersistentRef`.

Recursion depth is limited to 1,000 levels. Integers are encoded with
minimal byte length for signed little-endian representation.

### `pyconv.rs` -- Direct PyObject Bridge

The fast path for the Python dict API. Converts between `PickleValue`
AST and Python objects directly, bypassing the `serde_json::Value`
intermediate layer. Handles all JSON markers, known type detection,
BTree flattening, and persistent reference compact/expand in a single
tree walk.

Provides both standard and PG-specific variants:

- `pickle_value_to_pyobject` / `pickle_value_to_pyobject_pg` -- decode
  direction.
- `encode_pyobject_as_pickle` / `encode_zodb_record_direct` -- encode
  direction.
- `btree_state_to_pyobject` / `btree_state_to_pyobject_pg` --
  BTree-aware decode.
- `collect_refs_from_pickle_value` -- extract persistent reference OIDs.

### `json.rs` -- JSON String Path

Converts between `PickleValue` AST and `serde_json::Value` for the JSON
string API (`pickle_to_json`, `json_to_pickle`). Also provides the
PG-specific `pickle_value_to_json_string_pg` which uses the
`JsonWriter` for zero-allocation output.

Key functions:

- `pickle_value_to_json` -- standard PickleValue to JSON Value.
- `pickle_value_to_json_pg` -- PG-safe variant with null-byte
  sanitization.
- `json_to_pickle_value` -- JSON Value back to PickleValue.
- `pickle_value_to_json_string_pg` -- direct string output for PG
  (uses `json_writer.rs`).

### `json_writer.rs` -- Direct JSON String Writer

A low-level JSON token writer that appends directly to a `String`
buffer. Used by the PG JSON path to avoid allocating intermediate
`serde_json::Value` nodes entirely. Writes JSON tokens (object open/
close, array open/close, strings, numbers, booleans, null) as raw
characters.

### `known_types.rs` -- Known Type Handlers

Intercepts common Python REDUCE patterns at the PickleValue/JSON
boundary and produces compact typed markers instead of generic `@reduce`
output. Handles both directions:

- **Forward** (PickleValue to JSON): `try_reduce_to_typed_json` --
  recognizes `datetime.datetime`, `datetime.date`, `datetime.time`,
  `datetime.timedelta`, `decimal.Decimal`, `uuid.UUID`,
  `builtins.set`, and `builtins.frozenset`.
- **Reverse** (JSON to PickleValue): `try_typed_json_to_reduce` --
  converts `@dt`, `@date`, `@time`, `@td`, `@dec`, `@uuid`, `@set`,
  `@fset` markers back to REDUCE patterns.

Full timezone support: naive, fixed-offset (`datetime.timezone`),
pytz (including named zones with full constructor args), and zoneinfo.

### `btrees.rs` -- BTree State Handling

Classifies BTree classes by module/name and flattens their deeply nested
tuple state into queryable JSON. Handles both directions:

- **Forward**: `btree_state_to_json` -- flatten nested tuples to `@kv`,
  `@ks`, `@children`, `@first`, `@next` markers.
- **Reverse**: `json_to_btree_state` -- reconstruct nested tuples from
  flat markers.
- **Classification**: `classify_btree` -- identify BTree class and node
  kind from module/name strings.

### `zodb.rs` -- ZODB Record Handling

Handles the ZODB two-pickle record format. Provides:

- `split_zodb_record` -- find the boundary between class and state
  pickles by walking the first pickle to its STOP opcode.
- `extract_class_info` -- extract (module, name) from the class pickle
  value, handling GLOBAL, flat tuple, and nested tuple
  `((module, name), None)` formats.

Also contains `#[cfg(test)]` encode functions for ZODB record
roundtrip testing.

### `opcodes.rs` -- Pickle Opcode Constants

Defines constants for all pickle opcodes from protocol 0 through 5. The
codec focuses on protocol 2-3 (ZODB standard) but includes protocol 4-5
opcodes for partial forward compatibility.

### `error.rs` -- Error Types

Defines `CodecError` with variants for all failure modes: unexpected
EOF, unknown opcode, stack underflow, invalid data, JSON errors, and
invalid UTF-8. Implements conversion to Python `ValueError` via PyO3.

## Data Flow

The following diagram shows the three conversion paths through the
codebase:

```{mermaid}
flowchart LR
    PB["Pickle bytes"]
    PV["PickleValue AST"]
    PY["Python objects"]
    JV["serde_json::Value"]
    JS["JSON string"]

    PB -->|"decode.rs"| PV
    PV -->|"encode.rs"| PB

    PV -->|"pyconv.rs"| PY
    PY -->|"pyconv.rs"| PB

    PV -->|"json.rs"| JV
    JV -->|"json.rs"| PV
    JV -->|"serde_json"| JS

    PV -->|"json_writer.rs"| JS
```

**Path 1 -- Python dict API** (`decode_zodb_record`, `pickle_to_dict`):
Pickle bytes go through `decode.rs` to `PickleValue`, then `pyconv.rs`
converts directly to Python objects with marker dicts. The encode
direction goes from Python objects through `pyconv.rs` directly to
pickle bytes, bypassing the AST.

**Path 2 -- JSON string API** (`pickle_to_json`, `json_to_pickle`):
Pickle bytes go through `decode.rs` to `PickleValue`, then `json.rs`
converts to `serde_json::Value`, which is serialized to a JSON string.
The reverse path deserializes JSON, converts through `json.rs` back to
`PickleValue`, and encodes via `encode.rs`.

**Path 3 -- PG JSON path** (`decode_zodb_record_for_pg_json`):
Pickle bytes go through `decode.rs` to `PickleValue`, then
`json_writer.rs` writes JSON tokens directly to a string buffer,
skipping the `serde_json::Value` intermediate entirely. This is the
fastest path.

In all paths, `known_types.rs` and `btrees.rs` are consulted during
conversion to handle special type patterns and BTree state flattening.

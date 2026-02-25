# Build from source

<!-- diataxis: how-to -->

## Prerequisites

- **Rust 1.70+** -- install via [rustup](https://rustup.rs/)
- **Python 3.10+** -- with a virtual environment active
- **maturin** -- the Rust/Python build tool

Install maturin:

```bash
uv tool install maturin
```

Or with pip:

```bash
pip install maturin
```

## Clone the repository

```bash
git clone https://github.com/bluedynamics/zodb-json-codec.git
cd zodb-json-codec
```

## Debug build

For development iteration (faster compile, slower runtime):

```bash
maturin develop
```

This compiles the Rust code and installs the resulting Python extension into the active virtual environment.

## Release build

For accurate performance testing:

```bash
maturin develop --release
```

The release profile uses thin LTO and single codegen unit for best runtime performance (configured in `Cargo.toml`).

## Run the test suites

### Rust tests

```bash
cargo test
```

This runs the 75 Rust unit tests covering pickle decode/encode, JSON conversion, known type handlers, and BTree flattening.

### Python tests

Install the test dependencies first:

```bash
pip install ".[test]"
```

Then run the test suite:

```bash
pytest tests/ -v
```

This runs the 149 Python integration tests covering the full roundtrip through the PyO3 boundary, ZODB record handling, and edge cases.

## Project structure

The codebase is organized as:

```
src/
  lib.rs          # PyO3 module definition and Python-facing functions
  decode.rs       # Pickle byte stream -> PickleValue AST
  encode.rs       # PickleValue AST -> pickle bytes
  json.rs         # PickleValue <-> serde_json::Value (JSON string path)
  pyconv.rs       # PickleValue <-> Py<PyAny> (Python dict path)
  known_types.rs  # Compact markers for datetime, Decimal, UUID, set, etc.
  btrees.rs       # BTree family detection and flattening
  zodb.rs         # ZODB two-pickle record handling
  types.rs        # PickleValue AST definition
  opcodes.rs      # Pickle protocol opcode constants
  json_writer.rs  # Streaming JSON writer (no serde_json allocation)
  error.rs        # Error types
python/
  zodb_json_codec/__init__.py   # Python package re-exports
```

For detailed architecture information, see the reference documentation.

## Rebuild after changes

After modifying Rust code, re-run `maturin develop` (or `maturin develop --release`) to recompile and reinstall.
Python-only changes in the `python/` directory do not require a rebuild.

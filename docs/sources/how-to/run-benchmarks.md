# Run benchmarks

<!-- diataxis: how-to -->

The benchmark suite compares zodb-json-codec against Python's pickle module on both synthetic data and real ZODB FileStorage databases.

## Prerequisites

Always benchmark against a **release** build.
Debug builds are 5-10x slower and do not reflect production performance.

```bash
maturin develop --release
```

Install the benchmark dependencies:

```bash
pip install "zodb-json-codec[bench]"
```

## Benchmark commands

### Synthetic micro-benchmarks

Tests individual type encode/decode paths with generated data:

```bash
python benchmarks/bench.py synthetic --iterations 5000
```

### Real FileStorage scan

Decodes every record from an actual ZODB FileStorage file:

```bash
# Decompress the bundled sample data first
gunzip benchmarks/bench_data/Data.fs.gz

python benchmarks/bench.py filestorage benchmarks/bench_data/Data.fs
```

Limit the number of records with `--max-records N`.

### PG path comparison

Compares the two PostgreSQL decode paths -- `decode_zodb_record_for_pg()` (returns Python dict) vs. `decode_zodb_record_for_pg_json()` (returns JSON string directly):

```bash
python benchmarks/bench.py pg-compare --filestorage benchmarks/bench_data/Data.fs
```

### Combined run

Runs both synthetic and filestorage benchmarks and exports results to JSON:

```bash
python benchmarks/bench.py all --filestorage benchmarks/bench_data/Data.fs --output results.json
```

### Generate benchmark data

Creates a reproducible FileStorage from Wikipedia seed data:

```bash
python benchmarks/bench.py generate
```

Output defaults to `benchmarks/bench_data/Data.fs`.

## Output options

All benchmark commands accept:

- `--output FILE` -- export results as JSON
- `--format {table,json,both}` -- output format (default: `table`)

## PGO builds for production-accurate numbers

Profile-Guided Optimization (PGO) produces the most accurate performance numbers by optimizing based on actual benchmark workloads.

### 1. Install LLVM tools

```bash
rustup component add llvm-tools
```

### 2. Instrumented build

Build with profiling instrumentation enabled:

```bash
RUSTFLAGS="-Cprofile-generate=/tmp/pgo-data" maturin develop --release
```

### 3. Generate profiles

Run both benchmark types to capture representative workload data:

```bash
python benchmarks/bench.py synthetic --iterations 5000
python benchmarks/bench.py filestorage benchmarks/bench_data/Data.fs
```

### 4. Merge profile data

```bash
llvm-profdata merge -o /tmp/pgo-data/merged.profdata /tmp/pgo-data/*.profraw
```

### 5. Final PGO build

```bash
RUSTFLAGS="-Cprofile-use=/tmp/pgo-data/merged.profdata" maturin develop --release
```

Now re-run the benchmarks against this optimized build for production-representative numbers.

## CI regression detection

The `check` subcommand is designed for CI pipelines.
It runs synthetic benchmarks and fails with a non-zero exit code if performance has regressed:

```bash
python benchmarks/bench.py check --iterations 500
```

"""Benchmark: zodb-json-codec (Rust) vs Python pickle for ZODB records.

Usage:
    python benchmarks/bench.py synthetic [--iterations N] [--warmup N]
    python benchmarks/bench.py filestorage PATH [--max-records N]
    python benchmarks/bench.py all --filestorage PATH [--iterations N]

All commands accept --output FILE (JSON export) and --format {table,json,both}.
"""

from __future__ import annotations

import argparse
import gzip
import hashlib
import io
import json
import pickle
import statistics
import sys
import time
import uuid
from dataclasses import dataclass, field
from datetime import date, datetime, timedelta, timezone
from decimal import Decimal
from pathlib import Path


# ---------------------------------------------------------------------------
# Data classes for results
# ---------------------------------------------------------------------------


@dataclass
class TimingStats:
    """Aggregated timing statistics (microseconds)."""

    samples: list[float] = field(default_factory=list)

    @property
    def count(self) -> int:
        return len(self.samples)

    @property
    def mean(self) -> float:
        return statistics.mean(self.samples) if self.samples else 0.0

    @property
    def median(self) -> float:
        return statistics.median(self.samples) if self.samples else 0.0

    @property
    def p95(self) -> float:
        return _percentile(self.samples, 0.95)

    @property
    def p99(self) -> float:
        return _percentile(self.samples, 0.99)

    @property
    def min_val(self) -> float:
        return min(self.samples) if self.samples else 0.0

    @property
    def max_val(self) -> float:
        return max(self.samples) if self.samples else 0.0

    @property
    def stddev(self) -> float:
        return statistics.stdev(self.samples) if len(self.samples) > 1 else 0.0

    @property
    def total(self) -> float:
        return sum(self.samples)

    def to_dict(self) -> dict:
        return {
            "count": self.count,
            "mean_us": round(self.mean, 3),
            "median_us": round(self.median, 3),
            "p95_us": round(self.p95, 3),
            "p99_us": round(self.p99, 3),
            "min_us": round(self.min_val, 3),
            "max_us": round(self.max_val, 3),
            "stddev_us": round(self.stddev, 3),
            "total_ms": round(self.total / 1000, 3),
        }


def _percentile(data: list[float], pct: float) -> float:
    if not data:
        return 0.0
    s = sorted(data)
    k = (len(s) - 1) * pct
    f = int(k)
    c = f + 1
    if c >= len(s):
        return s[f]
    return s[f] + (k - f) * (s[c] - s[f])


@dataclass
class BenchmarkResult:
    """Result of a single synthetic benchmark category."""

    name: str
    pickle_size: int
    json_size: int
    decode_python: TimingStats
    decode_codec: TimingStats
    encode_python: TimingStats
    encode_codec: TimingStats
    roundtrip_python: TimingStats
    roundtrip_codec: TimingStats


@dataclass
class FileStorageResult:
    """Result of scanning a FileStorage file."""

    path: str
    total_records: int = 0
    total_pickle_bytes: int = 0
    total_json_bytes: int = 0
    errors: int = 0
    error_details: list[str] = field(default_factory=list)
    decode_codec: TimingStats = field(default_factory=TimingStats)
    decode_python: TimingStats = field(default_factory=TimingStats)
    encode_python: TimingStats = field(default_factory=TimingStats)
    encode_codec: TimingStats = field(default_factory=TimingStats)
    roundtrip_codec: TimingStats = field(default_factory=TimingStats)
    by_class: dict[str, int] = field(default_factory=dict)
    python_failed_classes: dict[str, int] = field(default_factory=dict)


# ---------------------------------------------------------------------------
# Synthetic data generators
# ---------------------------------------------------------------------------


def make_zodb_record(
    module: str, classname: str, state: object, protocol: int = 3
) -> bytes:
    """Build a minimal ZODB-like record (two concatenated pickles)."""
    class_pickle = pickle.dumps((module, classname), protocol=protocol)
    state_pickle = pickle.dumps(state, protocol=protocol)
    return class_pickle + state_pickle


def _build_deep_dict(depth: int) -> dict:
    if depth == 0:
        return {"leaf": True, "value": 42}
    return {
        "level": depth,
        "child": _build_deep_dict(depth - 1),
        "siblings": list(range(5)),
    }


def generate_synthetic_data() -> dict[str, bytes]:
    """Generate named test datasets as ZODB record bytes."""
    return {
        "simple_flat_dict": make_zodb_record(
            "myapp",
            "Document",
            {"title": "Hello World", "count": 42, "active": True, "ratio": 3.14159},
        ),
        "nested_dict": make_zodb_record(
            "myapp",
            "Container",
            {
                "metadata": {"created": "2025-01-01", "author": "admin"},
                "items": [1, 2, 3],
                "config": {"nested": {"deep": True}},
            },
        ),
        "large_flat_dict": make_zodb_record(
            "myapp",
            "BigObj",
            {
                f"field_{i:03d}": (
                    f"value_{i}" if i % 3 == 0 else i if i % 3 == 1 else i * 0.1
                )
                for i in range(100)
            },
        ),
        "bytes_in_state": make_zodb_record(
            "myapp",
            "BlobHolder",
            {"data": b"\x00\x01\x02" * 333, "name": "test-blob"},
        ),
        "special_types": make_zodb_record(
            "myapp",
            "TypedObj",
            {
                "created": datetime(2025, 6, 15, 12, 0, 0),
                "birthday": date(1990, 5, 20),
                "duration": timedelta(days=7, seconds=3600),
                "price": Decimal("99.99"),
                "tags": {"python", "rust", "zodb"},
            },
        ),
        "btree_small": make_zodb_record(
            "BTrees.OOBTree",
            "OOBTree",
            (((("alpha", 1, "beta", 2, "gamma", 3, "delta", 4),),),),
        ),
        "btree_length": make_zodb_record("BTrees.Length", "Length", 42),
        "scalar_string": make_zodb_record(
            "myapp", "StrHolder", "just a plain string as state"
        ),
        "wide_dict": make_zodb_record(
            "myapp", "WideObj", {f"k{i}": f"v{i}" for i in range(1000)}
        ),
        "deep_nesting": make_zodb_record(
            "myapp", "DeepObj", _build_deep_dict(depth=10)
        ),
    }


# ---------------------------------------------------------------------------
# Python baseline functions
# ---------------------------------------------------------------------------


def python_decode_zodb_record(data: bytes) -> tuple:
    """Decode a ZODB record using pure Python pickle.

    Uses a single Unpickler with two load() calls, matching how ZODB
    actually unpickles records (the two pickles share the memo table).
    """
    f = io.BytesIO(data)
    up = pickle.Unpickler(f)
    up.persistent_load = lambda ref: ref
    class_info = up.load()
    state = up.load()
    return class_info, state


def python_encode_zodb_record(
    class_info: tuple, state: object, protocol: int = 3
) -> bytes:
    """Encode back using pure Python pickle."""
    return pickle.dumps(class_info, protocol=protocol) + pickle.dumps(
        state, protocol=protocol
    )


# ---------------------------------------------------------------------------
# Core timing
# ---------------------------------------------------------------------------


def bench_one(
    fn, *args, iterations: int = 1000, warmup: int = 100
) -> TimingStats:
    """Time a function, return stats in microseconds."""
    for _ in range(warmup):
        fn(*args)

    stats = TimingStats()
    for _ in range(iterations):
        t0 = time.perf_counter_ns()
        fn(*args)
        t1 = time.perf_counter_ns()
        stats.samples.append((t1 - t0) / 1000.0)
    return stats


# ---------------------------------------------------------------------------
# Synthetic benchmarks
# ---------------------------------------------------------------------------


def run_synthetic_benchmarks(
    iterations: int = 1000, warmup: int = 100
) -> list[BenchmarkResult]:
    """Run all synthetic benchmarks."""
    import zodb_json_codec

    datasets = generate_synthetic_data()
    results = []

    for name, record_data in datasets.items():
        # Measure sizes
        pickle_size = len(record_data)
        try:
            decoded = zodb_json_codec.decode_zodb_record(record_data)
            json_size = len(json.dumps(decoded))
        except Exception as exc:
            print(f"  SKIP {name}: decode failed: {exc}", file=sys.stderr)
            continue

        # --- Decode ---
        decode_python = bench_one(
            python_decode_zodb_record,
            record_data,
            iterations=iterations,
            warmup=warmup,
        )
        decode_codec = bench_one(
            zodb_json_codec.decode_zodb_record,
            record_data,
            iterations=iterations,
            warmup=warmup,
        )

        # --- Encode ---
        class_info, state = python_decode_zodb_record(record_data)
        encode_python = bench_one(
            python_encode_zodb_record,
            class_info,
            state,
            iterations=iterations,
            warmup=warmup,
        )
        encode_codec = bench_one(
            zodb_json_codec.encode_zodb_record,
            decoded,
            iterations=iterations,
            warmup=warmup,
        )

        # --- Roundtrip ---
        def _python_roundtrip(data=record_data):
            ci, st = python_decode_zodb_record(data)
            return python_encode_zodb_record(ci, st)

        def _codec_roundtrip(data=record_data):
            d = zodb_json_codec.decode_zodb_record(data)
            return zodb_json_codec.encode_zodb_record(d)

        roundtrip_python = bench_one(
            _python_roundtrip, iterations=iterations, warmup=warmup
        )
        roundtrip_codec = bench_one(
            _codec_roundtrip, iterations=iterations, warmup=warmup
        )

        results.append(
            BenchmarkResult(
                name=name,
                pickle_size=pickle_size,
                json_size=json_size,
                decode_python=decode_python,
                decode_codec=decode_codec,
                encode_python=encode_python,
                encode_codec=encode_codec,
                roundtrip_python=roundtrip_python,
                roundtrip_codec=roundtrip_codec,
            )
        )

    return results


# ---------------------------------------------------------------------------
# FileStorage benchmark
# ---------------------------------------------------------------------------


def run_filestorage_benchmark(
    path: str,
    max_records: int | None = None,
) -> FileStorageResult:
    """Scan a FileStorage and benchmark every record."""
    import zodb_json_codec
    from ZODB.FileStorage import FileStorage

    storage = FileStorage(path, read_only=True)
    result = FileStorageResult(path=path)

    count = 0
    for txn in storage.iterator():
        for record in txn:
            if max_records and count >= max_records:
                break
            data = record.data
            if not data:
                continue

            result.total_records += 1
            result.total_pickle_bytes += len(data)

            # --- Codec decode ---
            try:
                t0 = time.perf_counter_ns()
                decoded = zodb_json_codec.decode_zodb_record(data)
                t1 = time.perf_counter_ns()
                codec_decode_us = (t1 - t0) / 1000.0
            except Exception as exc:
                result.errors += 1
                oid_hex = record.oid.hex()
                result.error_details.append(f"oid={oid_hex}: {exc}")
                continue

            result.decode_codec.samples.append(codec_decode_us)

            # JSON size
            result.total_json_bytes += len(json.dumps(decoded))

            # Classify by type
            cls = decoded.get("@cls", ["unknown", "unknown"])
            if isinstance(cls, list) and len(cls) == 2:
                class_path = f"{cls[0]}.{cls[1]}"
            else:
                class_path = str(cls)
            result.by_class[class_path] = result.by_class.get(class_path, 0) + 1

            # --- Python baseline decode + encode ---
            # Only count when Python succeeds, so comparisons are fair
            try:
                t0 = time.perf_counter_ns()
                class_info, state = python_decode_zodb_record(data)
                t1 = time.perf_counter_ns()
                python_decode_us = (t1 - t0) / 1000.0

                t0 = time.perf_counter_ns()
                python_encode_zodb_record(class_info, state)
                t1 = time.perf_counter_ns()
                python_encode_us = (t1 - t0) / 1000.0

                # Both succeeded — record paired samples
                result.decode_python.samples.append(python_decode_us)
                result.encode_python.samples.append(python_encode_us)
            except Exception:
                result.python_failed_classes[class_path] = (
                    result.python_failed_classes.get(class_path, 0) + 1
                )

            # --- Codec encode ---
            try:
                t0 = time.perf_counter_ns()
                zodb_json_codec.encode_zodb_record(decoded)
                t1 = time.perf_counter_ns()
                result.encode_codec.samples.append((t1 - t0) / 1000.0)
            except Exception:
                pass

            # --- Codec roundtrip ---
            try:
                t0 = time.perf_counter_ns()
                d = zodb_json_codec.decode_zodb_record(data)
                zodb_json_codec.encode_zodb_record(d)
                t1 = time.perf_counter_ns()
                result.roundtrip_codec.samples.append((t1 - t0) / 1000.0)
            except Exception:
                pass

            count += 1
        if max_records and count >= max_records:
            break

    storage.close()
    return result


# ---------------------------------------------------------------------------
# Output: terminal tables
# ---------------------------------------------------------------------------

HEADER = "\033[1m"
RESET = "\033[0m"
DIM = "\033[2m"


def _speedup(baseline: float, candidate: float) -> str:
    if candidate <= 0 or baseline <= 0:
        return "N/A"
    ratio = baseline / candidate
    if ratio >= 1:
        return f"{ratio:.1f}x faster"
    return f"{1/ratio:.1f}x slower"


def _fmt_us(val: float) -> str:
    if val >= 1000:
        return f"{val/1000:.1f} ms"
    return f"{val:.1f} us"


def print_synthetic_results(results: list[BenchmarkResult]) -> None:
    print(f"\n{HEADER}{'='*72}")
    print(f" Synthetic Benchmarks")
    print(f"{'='*72}{RESET}\n")

    for r in results:
        ratio = r.json_size / r.pickle_size if r.pickle_size else 0
        print(f"{HEADER}{r.name}{RESET}")
        print(
            f"  {DIM}pickle: {r.pickle_size} bytes | "
            f"json: {r.json_size} bytes ({ratio:.2f}x){RESET}"
        )
        print()
        print(
            f"  {'Operation':<26} {'Mean':>10} {'Median':>10} "
            f"{'P95':>10} {'vs Python':>14}"
        )
        print(f"  {'-'*70}")

        rows = [
            ("Decode (Python)", r.decode_python, None),
            ("Decode (Codec)", r.decode_codec, r.decode_python),
            ("Encode (Python)", r.encode_python, None),
            ("Encode (Codec)", r.encode_codec, r.encode_python),
            ("Roundtrip (Python)", r.roundtrip_python, None),
            ("Roundtrip (Codec)", r.roundtrip_codec, r.roundtrip_python),
        ]
        for label, stats, baseline in rows:
            sp = _speedup(baseline.mean, stats.mean) if baseline else ""
            print(
                f"  {label:<26} {_fmt_us(stats.mean):>10} "
                f"{_fmt_us(stats.median):>10} {_fmt_us(stats.p95):>10} "
                f"{sp:>14}"
            )
        print()

    # Size comparison summary
    print(f"{HEADER}Size Comparison{RESET}")
    print(f"  {'Category':<24} {'Pickle':>10} {'JSON':>10} {'Ratio':>8}")
    print(f"  {'-'*52}")
    for r in results:
        ratio = r.json_size / r.pickle_size if r.pickle_size else 0
        print(
            f"  {r.name:<24} {r.pickle_size:>10} {r.json_size:>10} "
            f"{ratio:>7.2f}x"
        )
    print()


def print_filestorage_results(
    result: FileStorageResult,
    show_python_failures: bool = False,
) -> None:
    print(f"\n{HEADER}{'='*72}")
    print(f" FileStorage Scan: {result.path}")
    print(f"{'='*72}{RESET}\n")

    ratio = (
        result.total_json_bytes / result.total_pickle_bytes
        if result.total_pickle_bytes
        else 0
    )
    print(f"  Total records:  {result.total_records:,}")
    print(f"  Total pickle:   {_fmt_bytes(result.total_pickle_bytes)}")
    print(f"  Total JSON:     {_fmt_bytes(result.total_json_bytes)} ({ratio:.2f}x)")
    print(f"  Errors:         {result.errors}")
    if result.error_details:
        for detail in result.error_details[:5]:
            print(f"    {detail}")
        if len(result.error_details) > 5:
            print(f"    ... and {len(result.error_details) - 5} more")
    print()

    # Timing summary
    for label, stats in [
        ("Decode (Codec)", result.decode_codec),
        ("Decode (Python)", result.decode_python),
        ("Encode (Codec)", result.encode_codec),
        ("Encode (Python)", result.encode_python),
        ("Roundtrip (Codec)", result.roundtrip_codec),
    ]:
        if not stats.samples:
            continue
        print(f"  {HEADER}{label}{RESET} ({stats.count:,} samples)")
        print(
            f"    Mean: {_fmt_us(stats.mean):>12}   "
            f"Median: {_fmt_us(stats.median):>12}"
        )
        print(
            f"    P95:  {_fmt_us(stats.p95):>12}   "
            f"P99:    {_fmt_us(stats.p99):>12}"
        )
        print(
            f"    Min:  {_fmt_us(stats.min_val):>12}   "
            f"Max:    {_fmt_us(stats.max_val):>12}"
        )
        print()

    # Speedup summary
    if result.decode_python.samples and result.decode_codec.samples:
        py_count = len(result.decode_python.samples)
        total = len(result.decode_codec.samples)
        if py_count < total:
            pct = py_count / total * 100 if total else 0
            print(
                f"  {DIM}Note: Python pickle only decoded {py_count:,} of "
                f"{total:,} records ({pct:.0f}%) — classes not installed "
                f"for the rest.{RESET}"
            )
            print(
                f"  {DIM}Speedup is NOT comparable (different record sets). "
                f"Use synthetic benchmarks for fair comparison.{RESET}"
            )
        sp = _speedup(result.decode_python.mean, result.decode_codec.mean)
        print(f"  Decode speedup: {sp}")
    if result.encode_python.samples and result.encode_codec.samples:
        sp = _speedup(result.encode_python.mean, result.encode_codec.mean)
        print(f"  Encode speedup: {sp}")
    print()

    # Python decode failures by class
    if show_python_failures and result.python_failed_classes:
        print(f"  {HEADER}Python pickle failed to decode ({len(result.python_failed_classes)} classes){RESET}")
        sorted_fails = sorted(
            result.python_failed_classes.items(), key=lambda x: x[1], reverse=True
        )
        for cls, cnt in sorted_fails:
            print(f"    {cls:<55} {cnt:>6}")
        print()

    # Top record types
    if result.by_class:
        print(f"  {HEADER}Top record types{RESET}")
        sorted_classes = sorted(
            result.by_class.items(), key=lambda x: x[1], reverse=True
        )
        total = result.total_records
        for cls, cnt in sorted_classes[:15]:
            pct = cnt / total * 100 if total else 0
            print(f"    {cls:<55} {cnt:>6}  ({pct:5.1f}%)")
        if len(sorted_classes) > 15:
            print(f"    ... and {len(sorted_classes) - 15} more types")
        print()


def _fmt_bytes(n: int) -> str:
    if n >= 1_000_000_000:
        return f"{n / 1_000_000_000:.1f} GB"
    if n >= 1_000_000:
        return f"{n / 1_000_000:.1f} MB"
    if n >= 1_000:
        return f"{n / 1_000:.1f} KB"
    return f"{n} bytes"


# ---------------------------------------------------------------------------
# Output: JSON export
# ---------------------------------------------------------------------------


def results_to_json(
    synthetic: list[BenchmarkResult] | None,
    filestorage: FileStorageResult | None,
) -> dict:
    out: dict = {
        "timestamp": datetime.now(timezone.utc).isoformat(),
        "python_version": sys.version,
    }
    try:
        import zodb_json_codec

        out["codec_version"] = getattr(zodb_json_codec, "__version__", "unknown")
    except ImportError:
        pass

    if synthetic:
        out["synthetic"] = {}
        for r in synthetic:
            out["synthetic"][r.name] = {
                "pickle_size": r.pickle_size,
                "json_size": r.json_size,
                "decode_python": r.decode_python.to_dict(),
                "decode_codec": r.decode_codec.to_dict(),
                "encode_python": r.encode_python.to_dict(),
                "encode_codec": r.encode_codec.to_dict(),
                "roundtrip_python": r.roundtrip_python.to_dict(),
                "roundtrip_codec": r.roundtrip_codec.to_dict(),
            }

    if filestorage:
        out["filestorage"] = {
            "path": filestorage.path,
            "total_records": filestorage.total_records,
            "total_pickle_bytes": filestorage.total_pickle_bytes,
            "total_json_bytes": filestorage.total_json_bytes,
            "errors": filestorage.errors,
            "decode_codec": filestorage.decode_codec.to_dict(),
            "decode_python": filestorage.decode_python.to_dict(),
            "encode_codec": filestorage.encode_codec.to_dict(),
            "encode_python": filestorage.encode_python.to_dict(),
            "roundtrip_codec": filestorage.roundtrip_codec.to_dict(),
            "by_class": filestorage.by_class,
        }

    return out


# ---------------------------------------------------------------------------
# FileStorage generation
# ---------------------------------------------------------------------------


def _find_seed_data(seed_path: str | None) -> str:
    """Locate seed_data.json.gz, auto-detecting from monorepo layout."""
    if seed_path and Path(seed_path).exists():
        return seed_path
    # Auto-detect relative to this script
    script_dir = Path(__file__).resolve().parent
    candidates = [
        script_dir / ".." / ".." / "plone-pgcatalog" / "example" / "seed_data.json.gz",
    ]
    for p in candidates:
        p = p.resolve()
        if p.exists():
            return str(p)
    print(
        "ERROR: seed_data.json.gz not found. Use --seed-data PATH.",
        file=sys.stderr,
    )
    sys.exit(1)


def _body_max_len(index: int) -> int:
    """Deterministic body length limit with exponential skew toward short texts.

    Uses a hash to get a uniform value in [0,1), then applies x^3 to skew
    heavily toward the low end.  Maps to the range [500, 10000].
    """
    h = hashlib.md5(index.to_bytes(4, "big")).digest()
    uniform = int.from_bytes(h[:4], "big") / 0xFFFFFFFF  # [0, 1)
    skewed = uniform**3  # most values near 0
    return int(500 + skewed * 9500)


def run_generate(output_path: str, seed_path: str | None) -> None:
    """Generate a FileStorage populated with diverse ZODB objects."""
    import transaction
    from BTrees.Length import Length
    from BTrees.OIBTree import OIBTree
    from BTrees.OOBTree import OOBTree
    from persistent.list import PersistentList
    from persistent.mapping import PersistentMapping
    from ZODB import DB
    from ZODB.FileStorage import FileStorage

    # Load seed data
    resolved = _find_seed_data(seed_path)
    with gzip.open(resolved, "rt", encoding="utf-8") as f:
        data = json.load(f)
    articles = data["articles"]
    print(f"Loaded {len(articles)} seed articles from {resolved}")

    # Prepare output
    out = Path(output_path)
    out.parent.mkdir(parents=True, exist_ok=True)
    if out.exists():
        print(f"Removing existing {out}")
        for suffix in ("", ".index", ".lock", ".tmp"):
            p = Path(str(out) + suffix)
            if p.exists():
                p.unlink()

    storage = FileStorage(str(out))
    db = DB(storage)
    conn = db.open()
    root = conn.root()

    # --- Structural containers ---
    root["categories"] = OOBTree()
    root["by_language"] = OOBTree()
    root["groups"] = OOBTree()
    root["stats"] = OIBTree()
    root["total_articles"] = Length(0)

    config = PersistentMapping()
    config["site"] = PersistentMapping()
    config["site"]["name"] = "Benchmark Wiki"
    config["site"]["version"] = 1
    config["site"]["languages"] = PersistentMapping()
    root["config"] = config

    transaction.commit()

    # --- Articles ---
    BATCH = 100
    cat_trees: dict[str, OOBTree] = {}
    lang_trees: dict[str, OOBTree] = {}
    category_counts: dict[str, int] = {}
    language_counts: dict[str, int] = {}
    groups_map: dict[int, list] = {}

    tag_pool = [
        "science", "history", "geography", "culture", "nature",
        "politics", "economy", "society", "travel", "education",
    ]

    for i, seed in enumerate(articles):
        art = PersistentMapping()
        art["title"] = seed["title"]
        art["description"] = seed["description"]
        art["body"] = seed["body"][:_body_max_len(i)]
        art["url"] = seed["url"]
        art["language"] = seed["language"]
        art["category"] = seed["category"]
        art["group_id"] = seed["group_id"]

        # Enriched type-diverse fields (deterministic from index)
        art["created"] = datetime(
            2024, 1, 1, tzinfo=timezone.utc
        ) + timedelta(days=i % 365, hours=i % 24, minutes=i % 60)
        art["modified"] = art["created"] + timedelta(
            days=i % 30 + 1, seconds=i * 37 % 86400
        )
        art["pub_date"] = date(2024, 1, 1) + timedelta(days=i % 365)
        art["reading_time"] = timedelta(seconds=max(60, len(seed["body"]) // 20))
        art["word_count"] = Decimal(str(len(seed["body"].split())))
        art["score"] = Decimal(f"{(i * 17 % 1000) / 100:.2f}")
        art["article_uuid"] = uuid.UUID(int=i + 1)
        art["tags"] = frozenset(
            tag_pool[(i + j) % len(tag_pool)] for j in range(2 + i % 4)
        )
        art["related_ids"] = {i % 200, (i + 7) % 200, (i + 13) % 200}
        art["coordinates"] = (
            float(f"{(i * 7 % 180) - 90:.4f}"),
            float(f"{(i * 13 % 360) - 180:.4f}"),
        )
        art["raw_header"] = seed["title"].encode("utf-8")[:64]

        root[f"article_{i:04d}"] = art

        # Category OOBTree
        cat = seed["category"]
        if cat not in cat_trees:
            cat_trees[cat] = OOBTree()
            root["categories"][cat] = cat_trees[cat]
        cat_trees[cat][seed["title"]] = art
        category_counts[cat] = category_counts.get(cat, 0) + 1

        # Language OOBTree
        lang = seed["language"]
        if lang not in lang_trees:
            lang_trees[lang] = OOBTree()
            root["by_language"][lang] = lang_trees[lang]
        lang_trees[lang][seed["title"]] = art
        language_counts[lang] = language_counts.get(lang, 0) + 1

        # Group tracking
        gid = seed["group_id"]
        if gid not in groups_map:
            groups_map[gid] = []
        groups_map[gid].append(art)

        if (i + 1) % BATCH == 0:
            transaction.commit()
            print(f"  Committed {i + 1}/{len(articles)} articles...")

    transaction.commit()
    print(f"  Created {len(articles)} article objects")

    # --- Group summaries ---
    group_count = 0
    for gid, group_articles in sorted(groups_map.items())[:100]:
        grp = PersistentMapping()
        grp["group_id"] = gid
        grp["article_refs"] = PersistentList(group_articles)
        grp["languages"] = frozenset(a["language"] for a in group_articles)
        grp["total_words"] = Decimal(
            str(sum(len(a["body"].split()) for a in group_articles))
        )
        grp["created"] = datetime(
            2024, 6, 1, tzinfo=timezone.utc
        ) + timedelta(days=gid)
        root["groups"][str(gid)] = grp
        group_count += 1
    transaction.commit()
    print(f"  Created {group_count} group summary objects")

    # --- Stats & counters ---
    for cat, count in category_counts.items():
        root["stats"][cat] = count
    root["total_articles"].change(len(articles))
    for lang, count in language_counts.items():
        root[f"total_{lang}"] = Length(count)

    # --- Language config ---
    display_names = {"en": "English", "de": "Deutsch", "zh": "Chinese"}
    for lang, count in language_counts.items():
        lc = PersistentMapping()
        lc["enabled"] = True
        lc["article_count"] = count
        lc["display_name"] = display_names.get(lang, lang)
        config["site"]["languages"][lang] = lc

    # --- Edge-case objects ---
    # Large binary
    root["binary_sample"] = PersistentMapping()
    root["binary_sample"]["data"] = bytes(range(256)) * 100
    root["binary_sample"]["name"] = "binary-test"

    # All-None fields
    root["null_obj"] = PersistentMapping()
    root["null_obj"]["a"] = None
    root["null_obj"]["b"] = None
    root["null_obj"]["c"] = None

    # Deep nesting (plain dicts in state)
    root["deep_nested"] = PersistentMapping()
    deep: dict = {}
    current = deep
    for depth in range(15):
        current["level"] = depth
        current["siblings"] = list(range(depth + 1))
        current["child"] = {}
        current = current["child"]
    current["leaf"] = True
    root["deep_nested"]["tree"] = deep

    # Wide dict (500 keys)
    root["wide_obj"] = PersistentMapping()
    for j in range(500):
        root["wide_obj"][f"field_{j:04d}"] = f"value_{j}"

    # Tuple-heavy
    root["tuple_obj"] = PersistentMapping()
    root["tuple_obj"]["data"] = tuple(
        (i, f"item_{i}", i * 0.1) for i in range(50)
    )

    transaction.commit()

    conn.close()
    db.close()
    storage.close()

    # Summary
    fs_size = out.stat().st_size
    print(f"\nGenerated FileStorage: {out}")
    print(f"  File size: {_fmt_bytes(fs_size)}")
    print(f"  Articles: {len(articles)}")
    print(f"  Category BTrees: {len(cat_trees)}")
    print(f"  Language BTrees: {len(lang_trees)}")
    print(f"  Group summaries: {group_count}")
    print(f"  Edge-case objects: 5")
    print("\nRun benchmark with:")
    print(f"  python benchmarks/bench.py filestorage {out}")


# ---------------------------------------------------------------------------
# PG decode path comparison: dict vs JSON string
# ---------------------------------------------------------------------------


def run_pg_compare(
    iterations: int = 1000, warmup: int = 100
) -> list[dict]:
    """Compare decode_zodb_record_for_pg (dict) vs decode_zodb_record_for_pg_json (str).

    Measures both raw decode time and full pipeline time (decode + JSON serialization),
    since the dict path still needs json.dumps() before sending to PG.
    """
    import zodb_json_codec

    datasets = generate_synthetic_data()
    results = []

    for name, record_data in datasets.items():
        # Raw decode: dict path
        pg_dict = bench_one(
            zodb_json_codec.decode_zodb_record_for_pg,
            record_data,
            iterations=iterations,
            warmup=warmup,
        )
        # Raw decode: JSON string path
        pg_json = bench_one(
            zodb_json_codec.decode_zodb_record_for_pg_json,
            record_data,
            iterations=iterations,
            warmup=warmup,
        )

        # Full pipeline: dict path needs json.dumps() for PG
        def _dict_pipeline(data=record_data):
            _, _, state, _ = zodb_json_codec.decode_zodb_record_for_pg(data)
            json.dumps(state)

        # Full pipeline: JSON string path already has the JSON string
        def _json_pipeline(data=record_data):
            zodb_json_codec.decode_zodb_record_for_pg_json(data)

        pipeline_dict = bench_one(
            _dict_pipeline, iterations=iterations, warmup=warmup
        )
        pipeline_json = bench_one(
            _json_pipeline, iterations=iterations, warmup=warmup
        )

        results.append({
            "name": name,
            "pickle_size": len(record_data),
            "pg_dict": pg_dict,
            "pg_json": pg_json,
            "pipeline_dict": pipeline_dict,
            "pipeline_json": pipeline_json,
        })

    return results


def run_pg_compare_filestorage(path: str) -> dict:
    """Compare PG decode paths on real FileStorage data."""
    import zodb_json_codec
    from ZODB.FileStorage import FileStorage

    storage = FileStorage(path, read_only=True)
    pg_dict_stats = TimingStats()
    pg_json_stats = TimingStats()
    pipeline_dict_stats = TimingStats()
    pipeline_json_stats = TimingStats()
    count = 0

    for txn in storage.iterator():
        for record in txn:
            data = record.data
            if not data:
                continue
            try:
                # Raw dict decode
                t0 = time.perf_counter_ns()
                zodb_json_codec.decode_zodb_record_for_pg(data)
                t1 = time.perf_counter_ns()
                pg_dict_stats.samples.append((t1 - t0) / 1000.0)

                # Raw JSON string decode
                t0 = time.perf_counter_ns()
                zodb_json_codec.decode_zodb_record_for_pg_json(data)
                t1 = time.perf_counter_ns()
                pg_json_stats.samples.append((t1 - t0) / 1000.0)

                # Full pipeline: dict + json.dumps
                t0 = time.perf_counter_ns()
                _, _, state, _ = zodb_json_codec.decode_zodb_record_for_pg(data)
                json.dumps(state)
                t1 = time.perf_counter_ns()
                pipeline_dict_stats.samples.append((t1 - t0) / 1000.0)

                # Full pipeline: JSON string (already serialized)
                t0 = time.perf_counter_ns()
                zodb_json_codec.decode_zodb_record_for_pg_json(data)
                t1 = time.perf_counter_ns()
                pipeline_json_stats.samples.append((t1 - t0) / 1000.0)

                count += 1
            except Exception:
                continue

    storage.close()
    return {
        "records": count,
        "pg_dict": pg_dict_stats,
        "pg_json": pg_json_stats,
        "pipeline_dict": pipeline_dict_stats,
        "pipeline_json": pipeline_json_stats,
    }


def print_pg_compare_results(
    synthetic: list[dict],
    filestorage: dict | None = None,
) -> None:
    print(f"\n{HEADER}{'='*72}")
    print(f" PG Decode: dict path vs JSON string path")
    print(f"{'='*72}{RESET}\n")

    # Raw decode comparison
    print(f"  {HEADER}Raw decode (no JSON serialization){RESET}")
    print(
        f"  {'Category':<26} {'Dict (mean)':>12} {'JSON (mean)':>12} "
        f"{'Speedup':>12}"
    )
    print(f"  {'-'*62}")
    for r in synthetic:
        d_mean = r["pg_dict"].mean
        j_mean = r["pg_json"].mean
        sp = _speedup(d_mean, j_mean)
        print(
            f"  {r['name']:<26} {_fmt_us(d_mean):>12} {_fmt_us(j_mean):>12} "
            f"{sp:>12}"
        )
    print()

    # Full pipeline comparison (dict+json.dumps vs JSON string)
    print(f"  {HEADER}Full pipeline (decode + JSON serialization for PG){RESET}")
    print(
        f"  {'Category':<26} {'Dict+dumps':>12} {'JSON str':>12} "
        f"{'Speedup':>12}"
    )
    print(f"  {'-'*62}")
    for r in synthetic:
        d_mean = r["pipeline_dict"].mean
        j_mean = r["pipeline_json"].mean
        sp = _speedup(d_mean, j_mean)
        print(
            f"  {r['name']:<26} {_fmt_us(d_mean):>12} {_fmt_us(j_mean):>12} "
            f"{sp:>12}"
        )
    print()

    if filestorage:
        print(f"  {HEADER}FileStorage ({filestorage['records']:,} records){RESET}")
        print(f"\n  Raw decode:")
        d = filestorage["pg_dict"]
        j = filestorage["pg_json"]
        print(f"    {'Metric':<16} {'Dict':>12} {'JSON str':>12} {'Speedup':>12}")
        print(f"    {'-'*52}")
        for label, attr in [("Mean", "mean"), ("Median", "median"), ("P95", "p95")]:
            dv = getattr(d, attr)
            jv = getattr(j, attr)
            sp = _speedup(dv, jv)
            print(f"    {label:<16} {_fmt_us(dv):>12} {_fmt_us(jv):>12} {sp:>12}")

        print(f"\n  Full pipeline (decode + JSON serialization):")
        d = filestorage["pipeline_dict"]
        j = filestorage["pipeline_json"]
        print(f"    {'Metric':<16} {'Dict+dumps':>12} {'JSON str':>12} {'Speedup':>12}")
        print(f"    {'-'*52}")
        for label, attr in [("Mean", "mean"), ("Median", "median"), ("P95", "p95")]:
            dv = getattr(d, attr)
            jv = getattr(j, attr)
            sp = _speedup(dv, jv)
            print(f"    {label:<16} {_fmt_us(dv):>12} {_fmt_us(jv):>12} {sp:>12}")
        print()


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------


# ---------------------------------------------------------------------------
# Regression check: ratio-based thresholds (machine-independent)
# ---------------------------------------------------------------------------

# Expected minimum codec/python speed ratios (codec_time / python_time).
# Values < 1.0 mean codec is faster. Values > 1.0 mean codec is slower.
# Thresholds are generous to account for CI runner variability.
# A regression is when the ratio exceeds the threshold (codec gets slower).
REGRESSION_THRESHOLDS: dict[str, dict[str, float]] = {
    # category: {operation: max_allowed_ratio}
    # ratio = codec_mean / python_mean (lower is better, <1.0 = codec wins)
    # Thresholds are ~2-3x current measured ratios to absorb CI runner noise.
    # Current perf (PyO3 0.28): decode 0.5-1.2, encode 0.1-0.8
    "simple_flat_dict": {"decode": 2.0, "encode": 1.5},
    "nested_dict": {"decode": 2.0, "encode": 1.5},
    "large_flat_dict": {"decode": 2.5, "encode": 2.0},
    "bytes_in_state": {"decode": 3.0, "encode": 2.5},
    "special_types": {"decode": 2.5, "encode": 1.5},
    "btree_small": {"decode": 3.0, "encode": 1.5},
    "btree_length": {"decode": 2.0, "encode": 1.0},
    "scalar_string": {"decode": 2.0, "encode": 1.0},
    "wide_dict": {"decode": 3.0, "encode": 1.5},
    "deep_nesting": {"decode": 3.0, "encode": 2.5},
}


def check_regression(results: list[BenchmarkResult]) -> list[str]:
    """Check benchmark results against thresholds. Returns list of failures."""
    failures = []
    for r in results:
        thresholds = REGRESSION_THRESHOLDS.get(r.name, {})
        for op, max_ratio in thresholds.items():
            if op == "decode":
                codec_mean = r.decode_codec.mean
                python_mean = r.decode_python.mean
            elif op == "encode":
                codec_mean = r.encode_codec.mean
                python_mean = r.encode_python.mean
            else:
                continue
            if python_mean <= 0 or codec_mean <= 0:
                continue
            ratio = codec_mean / python_mean
            if ratio > max_ratio:
                failures.append(
                    f"REGRESSION: {r.name} {op}: "
                    f"codec/python ratio {ratio:.2f} exceeds "
                    f"threshold {max_ratio:.1f} "
                    f"(codec={codec_mean:.1f}us, python={python_mean:.1f}us)"
                )
    return failures


def main() -> None:
    parser = argparse.ArgumentParser(
        description="Benchmark zodb-json-codec vs Python pickle"
    )
    sub = parser.add_subparsers(dest="command")

    # synthetic
    syn = sub.add_parser("synthetic", help="Run synthetic micro-benchmarks")
    syn.add_argument("--iterations", type=int, default=1000)
    syn.add_argument("--warmup", type=int, default=100)

    # filestorage
    fs = sub.add_parser("filestorage", help="Scan a FileStorage file")
    fs.add_argument("path", help="Path to Data.fs")
    fs.add_argument("--max-records", type=int, default=None)
    fs.add_argument(
        "--show-python-failures",
        action="store_true",
        help="Show classes that Python pickle failed to decode",
    )

    # all
    both = sub.add_parser("all", help="Run synthetic + filestorage benchmarks")
    both.add_argument("--filestorage", dest="path", help="Path to Data.fs")
    both.add_argument("--iterations", type=int, default=1000)
    both.add_argument("--warmup", type=int, default=100)
    both.add_argument("--max-records", type=int, default=None)
    both.add_argument(
        "--show-python-failures",
        action="store_true",
        help="Show classes that Python pickle failed to decode",
    )

    # check (CI regression detection)
    chk = sub.add_parser(
        "check",
        help="Run synthetic benchmarks and fail on performance regression",
    )
    chk.add_argument("--iterations", type=int, default=500)
    chk.add_argument("--warmup", type=int, default=100)

    # pg-compare
    pgc = sub.add_parser(
        "pg-compare",
        help="Compare decode_for_pg (dict) vs decode_for_pg_json (string)",
    )
    pgc.add_argument("--iterations", type=int, default=1000)
    pgc.add_argument("--warmup", type=int, default=100)
    pgc.add_argument("--filestorage", dest="pg_fs_path", default=None)

    # generate
    gen = sub.add_parser(
        "generate",
        help="Generate a FileStorage for benchmarking from Wikipedia seed data",
    )
    gen.add_argument(
        "--output",
        default="benchmarks/bench_data/Data.fs",
        help="Output path for generated Data.fs",
    )
    gen.add_argument("--seed-data", default=None, help="Path to seed_data.json.gz")

    for p in [syn, fs, both, chk, pgc]:
        p.add_argument("--output", help="Write JSON results to file")
        p.add_argument(
            "--format",
            choices=["table", "json", "both"],
            default="table",
            dest="fmt",
        )

    args = parser.parse_args()
    if not args.command:
        parser.print_help()
        sys.exit(1)

    # Generate doesn't need the codec — handle early
    if args.command == "generate":
        run_generate(args.output, args.seed_data)
        return

    # Check codec is importable
    try:
        import zodb_json_codec  # noqa: F401
    except ImportError:
        print(
            "ERROR: zodb_json_codec not found. Run 'maturin develop' first.",
            file=sys.stderr,
        )
        sys.exit(1)

    # pg-compare is self-contained
    if args.command == "pg-compare":
        iters = args.iterations
        warm = args.warmup
        print(f"PG decode comparison ({iters} iterations, {warm} warmup)...")
        pg_syn = run_pg_compare(iters, warm)
        pg_fs = None
        pg_fs_path = getattr(args, "pg_fs_path", None)
        if pg_fs_path and Path(pg_fs_path).exists():
            print(f"Scanning FileStorage: {pg_fs_path}")
            pg_fs = run_pg_compare_filestorage(pg_fs_path)
        print_pg_compare_results(pg_syn, pg_fs)
        return

    synthetic_results = None
    fs_result = None

    if args.command in ("synthetic", "all", "check"):
        iters = getattr(args, "iterations", 1000)
        warm = getattr(args, "warmup", 100)
        print(f"Running synthetic benchmarks ({iters} iterations, {warm} warmup)...")
        synthetic_results = run_synthetic_benchmarks(iters, warm)

    if args.command in ("filestorage", "all"):
        path = getattr(args, "path", None)
        if not path:
            print("ERROR: FileStorage path is required", file=sys.stderr)
            sys.exit(1)
        if not Path(path).exists():
            print(f"ERROR: {path} not found", file=sys.stderr)
            sys.exit(1)
        max_rec = getattr(args, "max_records", None)
        print(f"Scanning FileStorage: {path}")
        fs_result = run_filestorage_benchmark(path, max_records=max_rec)

    fmt = getattr(args, "fmt", "table")
    if fmt in ("table", "both"):
        if synthetic_results:
            print_synthetic_results(synthetic_results)
        if fs_result:
            show_pf = getattr(args, "show_python_failures", False)
            print_filestorage_results(fs_result, show_python_failures=show_pf)

    json_data = results_to_json(synthetic_results, fs_result)
    if fmt in ("json", "both"):
        print(json.dumps(json_data, indent=2))

    output = getattr(args, "output", None)
    if output:
        Path(output).write_text(json.dumps(json_data, indent=2))
        print(f"Results written to {output}")

    # Regression check
    if args.command == "check" and synthetic_results:
        failures = check_regression(synthetic_results)
        if failures:
            print(f"\n{'='*60}")
            print(f"PERFORMANCE REGRESSION DETECTED ({len(failures)} failures)")
            print(f"{'='*60}")
            for f in failures:
                print(f"  {f}")
            print()
            sys.exit(1)
        else:
            print(f"\nPerformance check passed (all ratios within thresholds)")


if __name__ == "__main__":
    main()

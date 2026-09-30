"""Crafted nesting depth raises ValueError instead of crashing the interpreter (#19)."""

import pickle
import subprocess
import sys
import textwrap
import zodb_json_codec


def run_isolated(code):
    """Run code in a fresh interpreter: a stack overflow there cannot take this process down."""
    return subprocess.run(
        [sys.executable, "-c", textwrap.dedent(code)], capture_output=True, text=True
    )


def class_pickle():
    return pickle.dumps(("m", "C"), protocol=3)


class TestDecode:
    def test_decode_deep_nesting_raises(self):
        r = run_isolated(
            """
            import pickle, zodb_json_codec
            rec = pickle.dumps(("m", "C"), protocol=3) + b"\\x80\\x03N" + b"\\x85" * 300000 + b"."
            for fn in (
                zodb_json_codec.decode_zodb_record,
                zodb_json_codec.decode_zodb_record_for_pg_json,
            ):
                try:
                    fn(rec)
                    raise SystemExit("no error raised")
                except ValueError as e:
                    assert "nesting depth" in str(e), e
            print("ok")
            """
        )
        assert r.returncode == 0 and r.stdout.strip() == "ok", (
            r.returncode,
            r.stdout,
            r.stderr[-300:],
        )

    def test_depth_900_ok(self):
        deep = []
        for _ in range(900):
            deep = [deep]
        rec = class_pickle() + pickle.dumps({"d": deep}, protocol=3)
        assert zodb_json_codec.decode_zodb_record(rec)["@s"] == {"d": deep}


class TestEncode:
    def test_encode_deep_nesting_raises(self):
        r = run_isolated(
            """
            import zodb_json_codec
            deep = []
            for _ in range(200000):
                deep = [deep]
            cases = (
                (zodb_json_codec.encode_zodb_record, {"@cls": ["m", "C"], "@s": {"deep": deep}}),
                (zodb_json_codec.dict_to_pickle, {"deep": deep}),
            )
            for fn, arg in cases:
                try:
                    fn(arg)
                    raise SystemExit("no error raised")
                except ValueError as e:
                    assert "nesting depth" in str(e), e
            print("ok")
            """
        )
        assert r.returncode == 0 and r.stdout.strip() == "ok", (
            r.returncode,
            r.stdout,
            r.stderr[-300:],
        )

    def test_encode_depth_400_ok(self):
        # well inside the limit even where the direct encoder hands over to the PickleValue path
        deep = []
        for _ in range(400):
            deep = [deep]
        data = zodb_json_codec.encode_zodb_record(
            {"@cls": ["m", "C"], "@s": {"d": deep, "t": {"@t": [deep]}}}
        )
        assert len(data) > 800

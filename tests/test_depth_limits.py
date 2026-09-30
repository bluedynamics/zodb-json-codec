"""Crafted nesting depth raises ValueError instead of crashing the interpreter (#19)."""

import io
import pickle
import pytest
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


def load_state(record):
    u = pickle.Unpickler(io.BytesIO(record))
    u.load()
    return u.load()


def wrap(n):
    deep = []
    for _ in range(n):
        deep = [deep]
    return deep


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
        assert load_state(data) == {"d": deep, "t": (deep,)}

    def test_encode_boundary_is_exact(self):
        # only containers count: the state dict, the wrappers and the innermost
        # list make 1000 levels with 998 wrappers; the 1001st level is refused
        ok = {"deep": wrap(998)}
        bad = {"deep": wrap(999)}
        assert (
            load_state(
                zodb_json_codec.encode_zodb_record({"@cls": ["m", "C"], "@s": ok})
            )
            == ok
        )
        assert pickle.loads(zodb_json_codec.dict_to_pickle(ok)) == ok
        with pytest.raises(ValueError, match="nesting depth"):
            zodb_json_codec.encode_zodb_record({"@cls": ["m", "C"], "@s": bad})
        with pytest.raises(ValueError, match="nesting depth"):
            zodb_json_codec.dict_to_pickle(bad)

    def test_scalars_do_not_count(self):
        # a wide, flat dict at any depth is fine: the guard only counts containers
        state = {"deep": wrap(990)}
        state["deep"][0] if False else None
        inner = state["deep"]
        for _ in range(990):
            inner = inner[0]
        inner.extend(range(5000))
        assert (
            load_state(
                zodb_json_codec.encode_zodb_record({"@cls": ["m", "C"], "@s": state})
            )
            == state
        )

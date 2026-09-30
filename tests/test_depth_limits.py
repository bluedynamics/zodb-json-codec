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


def nesting(value):
    """Count single-element list wrappers without recursion: comparing or
    pickling a 900-deep value in Python hits its recursion limit on some
    versions, so the deep tests never do either."""
    n = 0
    while isinstance(value, list) and len(value) == 1:
        value = value[0]
        n += 1
    return n, value


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
        # {"d": 900 nested lists} written by hand: EMPTY_DICT, key, 901 EMPTY_LIST,
        # 900 APPEND (each nests the top list into the one below), SETITEM
        state = (
            b"\x80\x03}q\x00X\x01\x00\x00\x00dq\x01" + b"]" * 901 + b"a" * 900 + b"s."
        )
        decoded = zodb_json_codec.decode_zodb_record(class_pickle() + state)["@s"]
        assert list(decoded) == ["d"]
        assert nesting(decoded["d"]) == (900, [])


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
        back = load_state(data)
        assert set(back) == {"d", "t"} and isinstance(back["t"], tuple)
        assert nesting(back["d"]) == (400, [])
        assert nesting(back["t"][0]) == (400, [])

    def test_encode_boundary_is_exact(self):
        # only containers count: the state dict, the wrappers and the innermost
        # list make 1000 levels with 998 wrappers; the 1001st level is refused
        ok = {"deep": wrap(998)}
        bad = {"deep": wrap(999)}
        back = load_state(
            zodb_json_codec.encode_zodb_record({"@cls": ["m", "C"], "@s": ok})
        )
        assert nesting(back["deep"]) == (998, [])
        assert nesting(pickle.loads(zodb_json_codec.dict_to_pickle(ok))["deep"]) == (
            998,
            [],
        )
        with pytest.raises(ValueError, match="nesting depth"):
            zodb_json_codec.encode_zodb_record({"@cls": ["m", "C"], "@s": bad})
        with pytest.raises(ValueError, match="nesting depth"):
            zodb_json_codec.dict_to_pickle(bad)

    def test_scalars_do_not_count(self):
        # a wide, flat dict at any depth is fine: the guard only counts containers
        state = {"deep": wrap(990)}
        inner = state["deep"]
        for _ in range(990):
            inner = inner[0]
        inner.extend(range(5000))
        back = load_state(
            zodb_json_codec.encode_zodb_record({"@cls": ["m", "C"], "@s": state})
        )
        assert nesting(back["deep"]) == (990, list(range(5000)))

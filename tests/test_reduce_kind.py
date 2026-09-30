"""Instances created by REDUCE(cls, ()) + BUILD re-encode with REDUCE, so __init__ runs again (#32)."""

import json
import pickle
import pickletools

import pytest
from collections import OrderedDict

import zodb_json_codec

INIT_CALLS = []


class Counted:
    """__reduce__ returns (cls, (), state): unpickling calls cls(), which runs __init__."""

    def __init__(self):
        INIT_CALLS.append(1)
        self.x = 0

    def __reduce__(self):
        return (Counted, (), {"x": self.x})

    def __setstate__(self, state):
        self.x = state["x"]


class Plain:
    """Default reduce: NEWOBJ + BUILD."""


class ODSub(OrderedDict):
    """Inherits OrderedDict.__reduce__: REDUCE + SETITEMS + BUILD when it has a __dict__."""


def record(value):
    cls = pickle.dumps(("persistent.mapping", "PersistentMapping"), protocol=3)
    return cls + pickle.dumps({"data": {"v": value}}, protocol=3)


def opcodes(data):
    return [op.name for op, _, _ in pickletools.genops(data)]


def state_pickle(rec):
    """The second pickle of a ZODB record (genops stops at the class pickle's STOP)."""
    for op, _, pos in pickletools.genops(rec):
        if op.name == "STOP":
            return rec[pos + 1 :]
    raise AssertionError("no STOP in the class pickle")


def test_pg_json_marks_reduce_kind():
    c = Counted()
    c.x = 7
    _, _, js, _ = zodb_json_codec.decode_zodb_record_for_pg_json(record(c))
    inner = json.loads(js)["data"]["v"]
    assert inner["@cls"] == [__name__, "Counted"]
    assert inner["@s"] == {"x": 7}
    assert inner["@newobj"] is False


def test_newobj_instance_has_no_flag():
    p = Plain()
    p.x = 1
    _, _, js, _ = zodb_json_codec.decode_zodb_record_for_pg_json(record(p))
    inner = json.loads(js)["data"]["v"]
    assert inner["@cls"] == [__name__, "Plain"]
    assert "@newobj" not in inner


def test_json_reencode_runs_init():
    c = Counted()
    c.x = 3
    data = pickle.dumps(c, protocol=3)
    assert "REDUCE" in opcodes(data) and "NEWOBJ" not in opcodes(data)
    back = zodb_json_codec.json_to_pickle(zodb_json_codec.pickle_to_json(data))
    ops = opcodes(back)
    assert "REDUCE" in ops and "NEWOBJ" not in ops
    INIT_CALLS.clear()
    restored = pickle.loads(back)
    assert restored.x == 3
    assert INIT_CALLS == [1], (
        "cls() must run __init__ on unpickling, as the original pickle does"
    )


def test_dict_path_reencode_runs_init():
    c = Counted()
    c.x = 5
    data = pickle.dumps(c, protocol=3)
    as_dict = zodb_json_codec.pickle_to_dict(data)
    assert as_dict["@newobj"] is False
    back = zodb_json_codec.dict_to_pickle(as_dict)
    ops = opcodes(back)
    assert "REDUCE" in ops and "NEWOBJ" not in ops
    INIT_CALLS.clear()
    assert pickle.loads(back).x == 5
    assert INIT_CALLS == [1]
    # the ZODB record path (direct encoder) too
    rec = zodb_json_codec.decode_zodb_record(record(c))
    assert rec["@s"]["data"]["v"]["@newobj"] is False
    back_rec = zodb_json_codec.encode_zodb_record(rec)
    assert "NEWOBJ" not in opcodes(state_pickle(back_rec))


def test_reduce_kind_with_items():
    od = ODSub([("z", 1), ("a", 2)])
    od.attr = "kept"
    data = pickle.dumps(od, protocol=3)
    back = zodb_json_codec.json_to_pickle(zodb_json_codec.pickle_to_json(data))
    ops = opcodes(back)
    assert "REDUCE" in ops and "NEWOBJ" not in ops
    assert ops.index("SETITEMS") < ops.index("BUILD")
    restored = pickle.loads(back)
    assert list(restored.items()) == [("z", 1), ("a", 2)]
    assert restored.attr == "kept"


def test_stored_json_without_flag_stays_newobj():
    stored = '{"@cls": ["%s", "Counted"], "@s": {"x": 1}}' % __name__
    ops = opcodes(zodb_json_codec.json_to_pickle(stored))
    assert "NEWOBJ" in ops and "REDUCE" not in ops


def test_flag_other_than_false_means_newobj():
    for flag in ("true", '"no"', "null", "1"):
        stored = '{"@cls": ["%s", "Counted"], "@s": {"x": 1}, "@newobj": %s}' % (
            __name__,
            flag,
        )
        assert "NEWOBJ" in opcodes(zodb_json_codec.json_to_pickle(stored))


class Both(list):
    """__reduce__ with state, list items and dict items: five marker keys in JSON (#32 review)."""

    def __init__(self):
        INIT_CALLS.append("both")
        self.extra = {}

    def __setitem__(self, key, value):
        self.extra[key] = value

    def __reduce__(self):
        return (Both, (), {"tag": self.tag}, iter(list(self)), iter(self.extra.items()))

    def __setstate__(self, state):
        self.tag = state["tag"]


def test_five_key_instance_dict_takes_the_marker_path():
    b = Both()
    b.append(1)
    b["k"] = "v"
    b.tag = "t"
    data = pickle.dumps(b, protocol=3)
    as_dict = zodb_json_codec.pickle_to_dict(data)
    assert set(as_dict) == {"@cls", "@s", "@newobj", "@items", "@appends"}
    for back in (
        zodb_json_codec.dict_to_pickle(as_dict),
        zodb_json_codec.json_to_pickle(zodb_json_codec.pickle_to_json(data)),
    ):
        ops = opcodes(back)
        assert "NEWOBJ" not in ops
        assert (
            ops.index("REDUCE")
            < ops.index("APPENDS")
            < ops.index("SETITEMS")
            < ops.index("BUILD")
        )
        INIT_CALLS.clear()
        loaded = pickle.loads(back)
        assert isinstance(loaded, Both)
        assert (list(loaded), loaded.extra, loaded.tag, INIT_CALLS) == (
            [1],
            {"k": "v"},
            "t",
            ["both"],
        )
    rec = zodb_json_codec.decode_zodb_record(record(b))
    assert set(rec["@s"]["data"]["v"]) == {
        "@cls",
        "@s",
        "@newobj",
        "@items",
        "@appends",
    }
    back_rec = zodb_json_codec.encode_zodb_record(rec)
    assert "NEWOBJ" not in opcodes(state_pickle(back_rec)) and "SETITEMS" in opcodes(
        state_pickle(back_rec)
    )


def test_dict_path_flag_other_than_false_means_newobj():
    for flag in (True, None, 0, "false"):
        d = {"@cls": [__name__, "Counted"], "@s": {"x": 1}, "@newobj": flag}
        assert "NEWOBJ" in opcodes(zodb_json_codec.dict_to_pickle(d))
        rec = {"@cls": ["persistent.mapping", "PersistentMapping"], "@s": {"v": d}}
        assert "NEWOBJ" in opcodes(
            state_pickle(zodb_json_codec.encode_zodb_record(rec))
        )


def test_instance_keys_without_state_are_rejected():
    for key, value in (("@newobj", False), ("@items", []), ("@appends", [])):
        bad = {"@cls": [__name__, "Counted"], key: value}
        with pytest.raises(ValueError):
            zodb_json_codec.json_to_pickle(json.dumps(bad))
        with pytest.raises(ValueError):
            zodb_json_codec.dict_to_pickle(bad)

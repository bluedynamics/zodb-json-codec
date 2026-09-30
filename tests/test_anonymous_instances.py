"""BUILD on callables that are not globals, and the @inst marker (#25)."""

import functools
import io
import json
import pickle
import pickletools
import pytest
import zodb_json_codec


class Made:
    def __init__(self, n):
        self.n = n


def make(n):
    return Made(n)


class ViaPartial:
    """__reduce__ returns a callable that is not a global: a functools.partial."""

    def __reduce__(self):
        return (functools.partial(make, 1), (), {"extra": 2})


def two_pickles(state):
    return pickle.dumps(("m", "C"), protocol=3) + pickle.dumps(state, protocol=3)


def load_state(record):
    u = pickle.Unpickler(io.BytesIO(record))
    u.load()
    return u.load()


def ops(data):
    return [op.name for op, _arg, _pos in pickletools.genops(data)]


def test_reduce_keeps_state_for_non_global_callable_json_api():
    js = zodb_json_codec.pickle_to_json(pickle.dumps(ViaPartial(), protocol=3))
    reduce = json.loads(js)["@reduce"]
    assert reduce["state"] == {"extra": 2}
    assert "@inst" not in js
    back = pickle.loads(zodb_json_codec.json_to_pickle(js))
    assert isinstance(back, Made) and (back.n, back.extra) == (1, 2)


def test_reduce_keeps_state_for_non_global_callable_record_paths():
    rec = two_pickles({"v": ViaPartial()})
    d = zodb_json_codec.decode_zodb_record(rec)
    assert d["@s"]["v"]["@reduce"]["state"] == {"extra": 2}
    back = load_state(zodb_json_codec.encode_zodb_record(d))["v"]
    assert (back.n, back.extra) == (1, 2)
    mod, name, js, _ = zodb_json_codec.decode_zodb_record_for_pg_json(rec)
    back = load_state(
        zodb_json_codec.encode_zodb_record({"@cls": [mod, name], "@s": json.loads(js)})
    )["v"]
    assert (back.n, back.extra) == (1, 2)


def test_newobj_flag_survives_for_non_global_callable():
    # NEWOBJ on a class that is itself a REDUCE result, then BUILD
    js = zodb_json_codec.pickle_to_json(pickle.dumps(ViaPartial(), protocol=3))
    reduce = json.loads(js)["@reduce"]
    reduce["newobj"] = True
    data = zodb_json_codec.json_to_pickle(json.dumps({"@reduce": reduce}))
    names = ops(data)
    # the inner partial has its own REDUCE/BUILD pair; the outer object is
    # `cls () NEWOBJ state BUILD`, so NEWOBJ precedes the last BUILD
    last_build = max(i for i, n in enumerate(names) if n == "BUILD")
    assert names.index("NEWOBJ") < last_build
    assert names[names.index("NEWOBJ") - 1] == "EMPTY_TUPLE"


def legacy_inst_json():
    # what releases before 1.7.0 wrote for BUILD after a non-global REDUCE
    reduce = json.loads(
        zodb_json_codec.pickle_to_json(pickle.dumps(ViaPartial(), protocol=3))
    )["@reduce"]
    return {
        "@inst": {
            "@callable": reduce["callable"],
            "@args": reduce["args"],
            "@state": reduce["state"],
        }
    }


def test_stored_legacy_inst_callable_shape_still_encodes():
    legacy = legacy_inst_json()
    back = pickle.loads(zodb_json_codec.json_to_pickle(json.dumps(legacy)))
    assert (back.n, back.extra) == (1, 2)
    back = load_state(
        zodb_json_codec.encode_zodb_record({"@cls": ["m", "C"], "@s": {"v": legacy}})
    )["v"]
    assert (back.n, back.extra) == (1, 2)
    # inside a tuple marker: the PickleValue path
    back = load_state(
        zodb_json_codec.encode_zodb_record(
            {"@cls": ["m", "C"], "@s": {"t": {"@t": [legacy]}}}
        )
    )["t"][0]
    assert (back.n, back.extra) == (1, 2)


def test_build_on_non_instance_roundtrips_opcodes():
    # BUILD applied to a plain dict: not loadable by Python, but the codec keeps the shape
    data = b"\x80\x03}q\x00}q\x01b."
    js = zodb_json_codec.pickle_to_json(data)
    assert json.loads(js) == {"@inst": {"@obj": {}, "@state": {}}}
    back = zodb_json_codec.json_to_pickle(js)
    assert [o for o in ops(back) if o != "BINPUT"] == [
        "PROTO",
        "EMPTY_DICT",
        "EMPTY_DICT",
        "BUILD",
        "STOP",
    ]
    rec = two_pickles({}).replace(b"}q\x00.", b"}q\x00}q\x01b.")
    d = zodb_json_codec.decode_zodb_record(rec)
    assert d["@s"] == {"@inst": {"@obj": {}, "@state": {}}}
    assert "BUILD" in ops(
        zodb_json_codec.encode_zodb_record(d).split(b"\x80\x02", 2)[2]
    )


@pytest.mark.parametrize(
    "bad",
    [
        {"@inst": {"x": 1}},
        {"@inst": {"@callable": {"@cls": ["m", "f"]}}},
        {"@inst": 5},
        {"@inst": {"@obj": {}, "@state": {}, "junk": 1}},
        {
            "@inst": {
                "@callable": {"@cls": ["m", "f"]},
                "@args": {"@t": []},
                "@state": {},
                "@extra": 1,
            }
        },
    ],
)
def test_malformed_inst_raises(bad):
    with pytest.raises(ValueError, match="anonymous instance"):
        zodb_json_codec.json_to_pickle(json.dumps(bad))
    with pytest.raises(ValueError, match="anonymous instance"):
        zodb_json_codec.encode_zodb_record({"@cls": ["m", "C"], "@s": {"v": bad}})

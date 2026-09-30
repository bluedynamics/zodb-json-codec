"""NEWOBJ_EX (protocol 4) objects re-encode as copyreg.__newobj_ex__ REDUCE and load equal (#35)."""

import copyreg
import io
import json
import pickle
import pickletools

import pytest

import zodb_json_codec


class KwNew:
    """__new__ takes keyword arguments; __init__ must not run on unpickling."""

    def __new__(cls, a, *, k=None):
        self = object.__new__(cls)
        self.a, self.k = a, k
        return self

    def __getnewargs_ex__(self):
        return ((self.a,), {"k": self.k})

    def __init__(self, *args, **kwargs):
        raise RuntimeError("__init__ must not be called during unpickle")


class KwNewWithState(KwNew):
    def __getstate__(self):
        return {"extra": self.extra}

    def __setstate__(self, state):
        self.extra = state["extra"]


class EmptyBoth:
    """NEWOBJ_EX with empty args and empty kwargs: only a custom __reduce_ex__ produces it."""

    def __reduce_ex__(self, protocol):
        return (copyreg.__newobj_ex__, (EmptyBoth, (), {}), {"tag": self.tag})


class KwOnly:
    """NEWOBJ_EX with empty args and one keyword argument."""

    def __new__(cls, *, k):
        self = object.__new__(cls)
        self.k = k
        return self

    def __reduce_ex__(self, protocol):
        return (copyreg.__newobj_ex__, (KwOnly, (), {"k": self.k}))


def zodb_record(state, protocol):
    """A ZODB record: class pickle and state pickle from one pickler, sharing the memo."""
    buf = io.BytesIO()
    pickler = pickle.Pickler(buf, protocol=protocol)
    pickler.dump(("persistent.mapping", "PersistentMapping"))
    pickler.dump(state)
    return buf.getvalue()


def opcodes(data):
    return [op.name for op, _, _ in pickletools.genops(data)]


def make(cls, a, k, extra=None):
    obj = cls.__new__(cls, a, k=k)
    if extra is not None:
        obj.extra = extra
    return obj


def state_pickle(record):
    """The second pickle of a ZODB record (after the class pickle's STOP)."""
    end = 0
    for op, _, pos in pickletools.genops(record):
        if op.name == "STOP":
            end = pos + 1
            break
    return record[end:]


def check_roundtrip(data, expect):
    assert "NEWOBJ_EX" in opcodes(data)
    back = zodb_json_codec.json_to_pickle(zodb_json_codec.pickle_to_json(data))
    ops = opcodes(back)
    assert "NEWOBJ_EX" not in ops and "NEWOBJ" not in ops and "REDUCE" in ops
    assert pickle.loads(back).__dict__ == expect
    # dict path
    back2 = zodb_json_codec.dict_to_pickle(zodb_json_codec.pickle_to_dict(data))
    assert pickle.loads(back2).__dict__ == expect
    assert "NEWOBJ_EX" not in opcodes(back2)
    return back


@pytest.mark.parametrize(("a", "k"), [(1, "v"), ((), None), (0, {})])
def test_newobj_ex_roundtrip(a, k):
    data = pickle.dumps(make(KwNew, a, k), protocol=4)
    check_roundtrip(data, {"a": a, "k": k})


def test_newobj_ex_with_state():
    data = pickle.dumps(make(KwNewWithState, 7, "kw", extra="state"), protocol=4)
    assert "BUILD" in opcodes(data)
    back = check_roundtrip(data, {"a": 7, "k": "kw", "extra": "state"})
    assert opcodes(back).index("REDUCE") < opcodes(back).index("BUILD")


def test_json_shape_is_the_copyreg_form():
    data = pickle.dumps(make(KwNew, 1, "v"), protocol=4)
    js = json.loads(zodb_json_codec.pickle_to_json(data))
    assert js["@reduce"]["callable"] == {"@cls": ["copyreg", "__newobj_ex__"]}
    assert js["@reduce"]["args"]["@t"][0] == {"@cls": [__name__, "KwNew"]}
    assert "newobj" not in js["@reduce"]


def test_empty_args_and_kwargs():
    e = EmptyBoth()
    e.tag = "t"
    data = pickle.dumps(e, protocol=4)
    assert "NEWOBJ_EX" in opcodes(data)
    back = zodb_json_codec.json_to_pickle(zodb_json_codec.pickle_to_json(data))
    assert pickle.loads(back).tag == "t"
    data = pickle.dumps(KwOnly.__new__(KwOnly, k=9), protocol=4)
    assert "NEWOBJ_EX" in opcodes(data)
    back = zodb_json_codec.dict_to_pickle(zodb_json_codec.pickle_to_dict(data))
    assert pickle.loads(back).k == 9


def test_record_path_protocol_4():
    obj = make(KwNewWithState, 2, "z", extra="e")
    # the repeated string is a memo reference across the shared memo of the record
    rec = zodb_record({"data": {"v": obj, "again": "persistent.mapping"}}, protocol=4)
    _, _, js, _ = zodb_json_codec.decode_zodb_record_for_pg_json(rec)
    inner = json.loads(js)["data"]["v"]
    assert inner["@reduce"]["callable"] == {"@cls": ["copyreg", "__newobj_ex__"]}
    back = zodb_json_codec.encode_zodb_record(zodb_json_codec.decode_zodb_record(rec))
    loaded = pickle.loads(state_pickle(back))
    assert loaded["data"]["v"].__dict__ == {"a": 2, "k": "z", "extra": "e"}
    assert loaded["data"]["again"] == "persistent.mapping"


def test_legacy_json_shapes_encode():
    mod = __name__
    legacy_reduce = json.dumps(
        {
            "@reduce": {
                "callable": {"@cls": [mod, "KwNew"]},
                "args": {"@args": {"@t": [5]}, "@kwargs": {"k": "legacy"}},
                "newobj": True,
            }
        }
    )
    obj = pickle.loads(zodb_json_codec.json_to_pickle(legacy_reduce))
    assert obj.__dict__ == {"a": 5, "k": "legacy"}
    # 1.6.1 had no newobj key at all
    legacy_161 = json.dumps(
        {
            "@reduce": {
                "callable": {"@cls": [mod, "KwNew"]},
                "args": {"@args": {"@t": [8]}, "@kwargs": {"k": "161"}},
            }
        }
    )
    obj = pickle.loads(zodb_json_codec.json_to_pickle(legacy_161))
    assert obj.__dict__ == {"a": 8, "k": "161"}
    legacy_instance = json.dumps(
        {
            "@cls": [mod, "KwNewWithState"],
            "@s": {
                "@args": {"@args": {"@t": [6]}, "@kwargs": {"k": "old"}},
                "@state": {"extra": "kept"},
            },
        }
    )
    obj = pickle.loads(zodb_json_codec.json_to_pickle(legacy_instance))
    assert obj.__dict__ == {"a": 6, "k": "old", "extra": "kept"}
    # the dict path reads the same shapes
    obj = pickle.loads(zodb_json_codec.dict_to_pickle(json.loads(legacy_reduce)))
    assert obj.__dict__ == {"a": 5, "k": "legacy"}

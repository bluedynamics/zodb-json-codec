"""Constructor arguments (NEWOBJ args, REDUCE args + BUILD, class pickle newargs) round-trip (#12)."""

from persistent.mapping import PersistentMapping

import io
import json
import pickle
import pickletools
import pytest
import zodb_json_codec


class Weird:
    """__new__ needs an argument and __init__ must not run during unpickling (#12)."""

    def __new__(cls, token):
        self = object.__new__(cls)
        self.token = token
        return self

    def __getnewargs__(self):
        return (self.token,)

    def __init__(self, *args):
        raise RuntimeError("__init__ must not be called during unpickle")


class Stateless:
    """NEWOBJ with args and no state; __init__ must not run."""

    __slots__ = ("token",)

    def __new__(cls, token):
        self = object.__new__(cls)
        self.token = token
        return self

    def __getnewargs__(self):
        return (self.token,)

    def __getstate__(self):
        return None

    def __init__(self, *args):
        raise RuntimeError("__init__ must not be called during unpickle")


class Reduced:
    """Custom __reduce__ with args and state: must come back through REDUCE, not NEWOBJ."""

    def __init__(self, a):
        self.a = a
        self.extra = None

    def __reduce__(self):
        return (Reduced, (self.a,), {"extra": self.extra})


class StrSub(str):
    pass


def two_pickles(cls_tuple, state):
    return pickle.dumps(cls_tuple, protocol=3) + pickle.dumps(state, protocol=3)


def load_state(record):
    u = pickle.Unpickler(io.BytesIO(record))
    u.load()
    return u.load()


def through_pg_json(state):
    mod, name, js, _ = zodb_json_codec.decode_zodb_record_for_pg_json(
        two_pickles(("m", "C"), state)
    )
    return load_state(
        zodb_json_codec.encode_zodb_record({"@cls": [mod, name], "@s": json.loads(js)})
    )


def through_dict_path(state):
    return load_state(
        zodb_json_codec.encode_zodb_record(
            zodb_json_codec.decode_zodb_record(two_pickles(("m", "C"), state))
        )
    )


def through_json_string(obj):
    return pickle.loads(
        zodb_json_codec.json_to_pickle(
            zodb_json_codec.pickle_to_json(pickle.dumps(obj, protocol=2))
        )
    )


class TestNewobjWithArgs:
    def test_issue_12_reproduction_json_string_api(self):
        assert through_json_string(Weird.__new__(Weird, "abc")).token == "abc"

    @pytest.mark.parametrize("path", [through_pg_json, through_dict_path])
    def test_newobj_with_args_and_state_roundtrip(self, path):
        back = path({"w": Weird.__new__(Weird, "abc")})["w"]
        assert isinstance(back, Weird)
        assert back.token == "abc"

    @pytest.mark.parametrize("path", [through_pg_json, through_dict_path])
    def test_newobj_with_args_no_state_roundtrip(self, path):
        back = path({"s": Stateless.__new__(Stateless, "tok")})["s"]
        assert isinstance(back, Stateless)
        assert back.token == "tok"

    def test_str_subclass_roundtrip(self):
        back = through_pg_json({"s": StrSub("x")})["s"]
        assert isinstance(back, StrSub) and back == "x"

    def test_json_marks_newobj(self):
        js = zodb_json_codec.pickle_to_json(
            pickle.dumps(Stateless.__new__(Stateless, "t"), protocol=2)
        )
        assert json.loads(js)["@reduce"]["newobj"] is True
        js = zodb_json_codec.pickle_to_json(pickle.dumps(Reduced("a"), protocol=2))
        assert "newobj" not in json.loads(js)["@reduce"]


class TestReduceWithArgsAndState:
    @pytest.mark.parametrize("path", [through_pg_json, through_dict_path])
    def test_reduce_with_args_and_state_roundtrip(self, path):
        r = Reduced("a")
        r.extra = "e"
        back = path({"r": r})["r"]
        assert isinstance(back, Reduced)
        assert (back.a, back.extra) == ("a", "e")

    def test_json_shape_has_state_inside_reduce(self):
        _, _, js, _ = zodb_json_codec.decode_zodb_record_for_pg_json(
            two_pickles(("m", "C"), {"r": Reduced("a")})
        )
        reduce = json.loads(js)["r"]["@reduce"]
        assert reduce["args"] == {"@t": ["a"]}
        assert reduce["state"] == {"extra": None}


class TestStoredJson:
    def test_stored_args_state_json_loads(self):
        stored = (
            '{"w":{"@s":{"@state":{"token":"abc"},"@args":{"@t":["abc"]}},'
            '"@cls":["test_newargs","Weird"]}}'
        )
        back = load_state(
            zodb_json_codec.encode_zodb_record(
                {"@cls": ["m", "C"], "@s": json.loads(stored)}
            )
        )
        assert back["w"].token == "abc"

    def test_stored_reduce_without_flag_stays_reduce(self):
        stored = (
            '{"s":{"@reduce":{"callable":{"@cls":["test_newargs","StrSub"]},'
            '"args":{"@t":["x"]}}}}'
        )
        data = zodb_json_codec.encode_zodb_record(
            {"@cls": ["m", "C"], "@s": json.loads(stored)}
        )
        state_pickle = data.split(b"\x80\x02", 2)[2]
        ops = [op.name for op, _arg, _pos in pickletools.genops(state_pickle)]
        assert "REDUCE" in ops and "NEWOBJ" not in ops
        assert load_state(data)["s"] == "x"

    def test_newobj_flag_must_be_a_true_bool(self):
        # Writers only emit `true`; readers agree that anything else means REDUCE
        stored = (
            '{"s":{"@reduce":{"callable":{"@cls":["test_newargs","StrSub"]},'
            '"args":{"@t":["x"]},"newobj":1}}}'
        )
        data = zodb_json_codec.encode_zodb_record(
            {"@cls": ["m", "C"], "@s": json.loads(stored)}
        )
        ops = [
            op.name
            for op, _arg, _pos in pickletools.genops(data.split(b"\x80\x02", 2)[2])
        ]
        assert "REDUCE" in ops and "NEWOBJ" not in ops


class TestClassPickleNewargs:
    @pytest.mark.parametrize(
        "fn",
        [
            zodb_json_codec.decode_zodb_record,
            zodb_json_codec.decode_zodb_record_for_pg,
            zodb_json_codec.decode_zodb_record_for_pg_json,
        ],
    )
    def test_class_pickle_with_newargs_is_rejected(self, fn):
        record = two_pickles((("mymod", "MyCls"), (1, 2)), {"a": 1})
        with pytest.raises(ValueError, match=r"mymod\.MyCls.*__getnewargs__"):
            fn(record)

    def test_class_pickle_global_tuple_form(self):
        # ZODB format 2: (klass, None) with the class as a GLOBAL
        record = pickle.dumps((PersistentMapping, None), protocol=3) + pickle.dumps(
            {"data": {}}, protocol=3
        )
        assert zodb_json_codec.decode_zodb_record(record)["@cls"] == [
            "persistent.mapping",
            "PersistentMapping",
        ]

    def test_class_pickle_global_with_newargs_is_rejected(self):
        record = pickle.dumps((PersistentMapping, (1,)), protocol=3) + pickle.dumps(
            {}, protocol=3
        )
        with pytest.raises(ValueError, match="__getnewargs__"):
            zodb_json_codec.decode_zodb_record(record)

"""Encoder input robustness: big ints encode, unknown types raise (#20)."""

from datetime import datetime

import io
import pickle
import pytest
import zodb_json_codec


def load_state(record):
    u = pickle.Unpickler(io.BytesIO(record))
    u.load()
    return u.load()


def encode_state(state):
    return load_state(
        zodb_json_codec.encode_zodb_record({"@cls": ["m", "C"], "@s": state})
    )


class IntSub(int):
    pass


class RaisingIndex(int):
    def __index__(self):
        raise KeyError("boom")


class TestBigInts:
    @pytest.mark.parametrize("value", [2**63, 2**70, -(2**63) - 1, -(2**70), 2**200])
    def test_big_int_roundtrip(self, value):
        assert encode_state({"n": value}) == {"n": value}
        assert pickle.loads(zodb_json_codec.dict_to_pickle({"n": value})) == {
            "n": value
        }

    def test_boundary_ints(self):
        assert encode_state({"lo": -(2**63), "hi": 2**63 - 1, "over": 2**63}) == {
            "lo": -(2**63),
            "hi": 2**63 - 1,
            "over": 2**63,
        }

    def test_big_int_positions(self):
        state = {"list": [2**70, 1], "d": {2**70: "key"}}
        assert encode_state(state) == state

    def test_bool_and_int_subclass(self):
        back = encode_state({"b": True, "i": IntSub(5)})
        assert back["b"] is True
        assert back["i"] == 5 and type(back["i"]) is int

    def test_int_subclass_uses_stored_value_not_index(self):
        # CPython reads the stored value of int subtypes directly; __index__ is not consulted
        assert encode_state({"i": RaisingIndex(1)}) == {"i": 1}


class TestUnknownTypes:
    @pytest.mark.parametrize(
        "value, name",
        [
            (b"\x01", "bytes"),
            ((1, 2), "tuple"),
            ({1}, "set"),
            (datetime(2025, 1, 1), "datetime"),
            (object(), "object"),
        ],
    )
    def test_unknown_types_raise_type_error(self, value, name):
        with pytest.raises(TypeError, match=f"of type {name};"):
            zodb_json_codec.encode_zodb_record({"@cls": ["m", "C"], "@s": {"v": value}})
        with pytest.raises(TypeError, match=f"of type {name};"):
            zodb_json_codec.dict_to_pickle({"v": value})

    def test_unknown_type_inside_marker_value(self):
        with pytest.raises(TypeError, match="bytes"):
            zodb_json_codec.encode_zodb_record(
                {"@cls": ["m", "C"], "@s": {"t": {"@t": [b"raw"]}}}
            )


class TestNonStringKeys:
    @pytest.mark.parametrize(
        "d",
        [
            {1: "a"},  # single key: direct encoder defers to the PickleValue path
            {1: "a", "s": "b"},  # 2-4 keys, mixed
            {None: "n", 3.5: "f", True: "b"},
            {i: i for i in range(6)},  # more than 4 keys
        ],
    )
    def test_non_string_keys_encode(self, d):
        assert encode_state({"d": d}) == {"d": d}
        assert pickle.loads(zodb_json_codec.dict_to_pickle({"d": d})) == {"d": d}

    def test_non_string_keys_inside_marker(self):
        # forces the PickleValue path for a dict nested in a tuple marker
        assert encode_state({"t": {"@t": [{1: "a", 2: "b"}]}}) == {
            "t": ({1: "a", 2: "b"},)
        }

    def test_tuple_keys_are_rejected_like_tuple_values(self):
        # tuples are only accepted as {"@t": [...]} markers, which cannot be dict keys
        with pytest.raises(TypeError, match="tuple"):
            zodb_json_codec.encode_zodb_record(
                {"@cls": ["m", "C"], "@s": {"d": {(1, 2): "t"}}}
            )

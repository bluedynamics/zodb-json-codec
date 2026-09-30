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

    def test_int_subclass_index_error_propagates(self):
        with pytest.raises(KeyError):
            zodb_json_codec.encode_zodb_record(
                {"@cls": ["m", "C"], "@s": {"i": RaisingIndex(1)}}
            )


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
        with pytest.raises(TypeError, match=name):
            zodb_json_codec.encode_zodb_record({"@cls": ["m", "C"], "@s": {"v": value}})
        with pytest.raises(TypeError, match=name):
            zodb_json_codec.dict_to_pickle({"v": value})

    def test_unknown_type_inside_marker_value(self):
        with pytest.raises(TypeError, match="bytes"):
            zodb_json_codec.encode_zodb_record(
                {"@cls": ["m", "C"], "@s": {"t": {"@t": [b"raw"]}}}
            )

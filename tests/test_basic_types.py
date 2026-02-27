"""Test round-trip: Python object → pickle → JSON → pickle → Python object.

For each type, we:
1. Pickle the Python value with protocol 3
2. Transcode pickle → JSON via our Rust codec
3. Verify the JSON looks right
4. Transcode JSON → pickle via our Rust codec
5. Unpickle and verify we get back the original value
"""

import json
import pickle

import pytest

import zodb_json_codec


class TestNone:
    def test_roundtrip(self):
        data = pickle.dumps(None, protocol=3)
        json_str = zodb_json_codec.pickle_to_json(data)
        assert json.loads(json_str) is None

        restored_pickle = zodb_json_codec.json_to_pickle(json_str)
        assert pickle.loads(restored_pickle) is None

    def test_to_dict(self):
        data = pickle.dumps(None, protocol=3)
        result = zodb_json_codec.pickle_to_dict(data)
        assert result is None


class TestBool:
    @pytest.mark.parametrize("val", [True, False])
    def test_roundtrip(self, val):
        data = pickle.dumps(val, protocol=3)
        json_str = zodb_json_codec.pickle_to_json(data)
        assert json.loads(json_str) is val

        restored_pickle = zodb_json_codec.json_to_pickle(json_str)
        assert pickle.loads(restored_pickle) is val


class TestInt:
    @pytest.mark.parametrize(
        "val",
        [0, 1, -1, 42, 255, 256, 65535, 65536, -128, 2**31 - 1, -(2**31)],
    )
    def test_roundtrip(self, val):
        data = pickle.dumps(val, protocol=3)
        json_str = zodb_json_codec.pickle_to_json(data)
        assert json.loads(json_str) == val

        restored_pickle = zodb_json_codec.json_to_pickle(json_str)
        assert pickle.loads(restored_pickle) == val


class TestFloat:
    @pytest.mark.parametrize("val", [0.0, 1.5, -3.14, 1e100, 1e-100])
    def test_roundtrip(self, val):
        data = pickle.dumps(val, protocol=3)
        json_str = zodb_json_codec.pickle_to_json(data)
        assert json.loads(json_str) == pytest.approx(val)

        restored_pickle = zodb_json_codec.json_to_pickle(json_str)
        assert pickle.loads(restored_pickle) == pytest.approx(val)


class TestString:
    @pytest.mark.parametrize(
        "val",
        [
            "",
            "hello",
            "hello world",
            "unicode: \u00e4\u00f6\u00fc\u00df",
            "a" * 300,  # longer than SHORT_BINUNICODE
        ],
    )
    def test_roundtrip(self, val):
        data = pickle.dumps(val, protocol=3)
        json_str = zodb_json_codec.pickle_to_json(data)
        assert json.loads(json_str) == val

        restored_pickle = zodb_json_codec.json_to_pickle(json_str)
        assert pickle.loads(restored_pickle) == val


class TestBytes:
    @pytest.mark.parametrize(
        "val",
        [
            b"",
            b"hello",
            b"\x00\x01\x02\xff",
            b"x" * 300,
        ],
    )
    def test_roundtrip(self, val):
        data = pickle.dumps(val, protocol=3)
        json_str = zodb_json_codec.pickle_to_json(data)
        parsed = json.loads(json_str)
        assert "@b" in parsed  # bytes are encoded with @b marker

        restored_pickle = zodb_json_codec.json_to_pickle(json_str)
        assert pickle.loads(restored_pickle) == val


class TestList:
    def test_empty(self):
        data = pickle.dumps([], protocol=3)
        json_str = zodb_json_codec.pickle_to_json(data)
        assert json.loads(json_str) == []

        restored_pickle = zodb_json_codec.json_to_pickle(json_str)
        assert pickle.loads(restored_pickle) == []

    def test_simple(self):
        val = [1, "two", 3.0, None, True]
        data = pickle.dumps(val, protocol=3)
        json_str = zodb_json_codec.pickle_to_json(data)
        parsed = json.loads(json_str)
        assert parsed == [1, "two", 3.0, None, True]

        restored_pickle = zodb_json_codec.json_to_pickle(json_str)
        # Note: json roundtrip converts to list (not tuple), which is correct
        result = pickle.loads(restored_pickle)
        assert result == [1, "two", 3.0, None, True]

    def test_nested(self):
        val = [[1, 2], [3, [4, 5]]]
        data = pickle.dumps(val, protocol=3)
        json_str = zodb_json_codec.pickle_to_json(data)

        restored_pickle = zodb_json_codec.json_to_pickle(json_str)
        assert pickle.loads(restored_pickle) == val


class TestDict:
    def test_empty(self):
        data = pickle.dumps({}, protocol=3)
        json_str = zodb_json_codec.pickle_to_json(data)
        assert json.loads(json_str) == {}

        restored_pickle = zodb_json_codec.json_to_pickle(json_str)
        assert pickle.loads(restored_pickle) == {}

    def test_string_keys(self):
        val = {"a": 1, "b": "two", "c": None}
        data = pickle.dumps(val, protocol=3)
        json_str = zodb_json_codec.pickle_to_json(data)
        parsed = json.loads(json_str)
        assert parsed == val

        restored_pickle = zodb_json_codec.json_to_pickle(json_str)
        assert pickle.loads(restored_pickle) == val

    def test_nested(self):
        val = {"outer": {"inner": [1, 2, 3]}}
        data = pickle.dumps(val, protocol=3)
        json_str = zodb_json_codec.pickle_to_json(data)

        restored_pickle = zodb_json_codec.json_to_pickle(json_str)
        assert pickle.loads(restored_pickle) == val


class TestSharedReferences:
    """Test that pickle memo sharing (same object in multiple places) roundtrips.

    Regression tests for https://github.com/bluedynamics/zodb-json-codec/issues/18
    """

    def test_dict_values_shared_with_list(self):
        """Dict values that are also list elements must preserve content."""
        sizes = [
            {"height": 75, "label": "Square", "width": 75},
            {"height": 683, "label": "Large", "width": 1024},
        ]
        # sizes_dict values are the SAME objects as in sizes list
        sizes_dict = {item["label"]: item for item in sizes}
        val = {"sizes": sizes, "sizes_dict": sizes_dict}

        data = pickle.dumps(val, protocol=3)
        json_str = zodb_json_codec.pickle_to_json(data)
        parsed = json.loads(json_str)
        # Verify sizes_dict values are NOT empty
        assert parsed["sizes_dict"]["Square"] == sizes[0]
        assert parsed["sizes_dict"]["Large"] == sizes[1]

        # Full roundtrip
        restored_pickle = zodb_json_codec.json_to_pickle(json_str)
        result = pickle.loads(restored_pickle)
        assert result == val

    def test_list_shared_reference(self):
        """Same list appearing twice via memo must preserve content."""
        inner = [1, 2, 3]
        val = {"a": inner, "b": inner}  # same object

        data = pickle.dumps(val, protocol=3)
        json_str = zodb_json_codec.pickle_to_json(data)
        parsed = json.loads(json_str)
        assert parsed["a"] == [1, 2, 3]
        assert parsed["b"] == [1, 2, 3]


class TestTuple:
    def test_empty(self):
        data = pickle.dumps((), protocol=3)
        json_str = zodb_json_codec.pickle_to_json(data)
        parsed = json.loads(json_str)
        assert "@t" in parsed
        assert parsed["@t"] == []

    @pytest.mark.parametrize(
        "val",
        [
            (1,),
            (1, 2),
            (1, 2, 3),
            (1, 2, 3, 4),
            (1, "two", 3.0),
        ],
    )
    def test_roundtrip_via_dict(self, val):
        """Tuples round-trip through the dict API correctly."""
        data = pickle.dumps(val, protocol=3)
        result = zodb_json_codec.pickle_to_dict(data)
        assert "@t" in result
        assert len(result["@t"]) == len(val)


class TestPickleToDict:
    """Test the pickle_to_dict function that returns Python objects directly."""

    def test_simple_dict(self):
        val = {"name": "Alice", "age": 30}
        data = pickle.dumps(val, protocol=3)
        result = zodb_json_codec.pickle_to_dict(data)
        assert result == val

    def test_nested_structures(self):
        val = {"items": [1, 2, 3], "nested": {"x": True}}
        data = pickle.dumps(val, protocol=3)
        result = zodb_json_codec.pickle_to_dict(data)
        assert result == val

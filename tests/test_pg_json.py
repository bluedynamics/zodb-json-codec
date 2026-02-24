"""Test decode_zodb_record_for_pg_json — direct pickle → JSON string path.

Verifies that the JSON string path produces identical output to the dict path.
"""

import json
import pickle
from datetime import date, datetime, timedelta, timezone
from decimal import Decimal
from uuid import UUID

import pytest

import zodb_json_codec


def make_zodb_record(module, classname, state, protocol=3):
    """Build a minimal ZODB-like record from class info and state."""
    class_pickle = pickle.dumps((module, classname), protocol=protocol)
    state_pickle = pickle.dumps(state, protocol=protocol)
    return class_pickle + state_pickle


def _normalize(obj):
    """Normalize a Python dict for comparison (handles ordering)."""
    return json.loads(json.dumps(obj, sort_keys=True, default=str))


class TestPgJsonMatchesDict:
    """Verify JSON string output matches the dict path for all types."""

    def _assert_match(self, record):
        """Assert the JSON string path produces equivalent output to the dict path."""
        mod1, name1, state_dict, refs1 = zodb_json_codec.decode_zodb_record_for_pg(record)
        mod2, name2, state_json, refs2 = zodb_json_codec.decode_zodb_record_for_pg_json(record)

        assert mod1 == mod2
        assert name1 == name2
        assert refs1 == refs2

        # state_json is a string, state_dict is a dict — compare as JSON
        actual = json.loads(state_json)
        expected = _normalize(state_dict)
        assert actual == expected, f"Mismatch:\n  expected: {expected}\n  actual: {actual}"

    def test_simple_dict(self):
        record = make_zodb_record("myapp", "Obj", {"title": "Hello", "count": 42})
        self._assert_match(record)

    def test_nested_dict(self):
        record = make_zodb_record(
            "myapp", "Obj",
            {"items": [1, 2, 3], "meta": {"key": "value"}},
        )
        self._assert_match(record)

    def test_none_state(self):
        record = make_zodb_record("myapp", "Obj", None)
        self._assert_match(record)

    def test_empty_dict(self):
        record = make_zodb_record("myapp", "Obj", {})
        self._assert_match(record)

    def test_bytes_value(self):
        record = make_zodb_record("myapp", "Obj", {"data": b"\x00\x01\x02\xff"})
        self._assert_match(record)

    def test_tuple_value(self):
        record = make_zodb_record("myapp", "Obj", {"coords": (1.5, 2.5)})
        self._assert_match(record)

    def test_set_value(self):
        record = make_zodb_record("myapp", "Obj", {"tags": frozenset(["a", "b"])})
        self._assert_match(record)

    def test_bool_none_mixed(self):
        record = make_zodb_record(
            "myapp", "Obj",
            {"flag": True, "empty": None, "num": 3.14},
        )
        self._assert_match(record)

    def test_large_dict(self):
        state = {f"key_{i:03d}": f"value_{i}" for i in range(200)}
        record = make_zodb_record("myapp", "Obj", state)
        self._assert_match(record)


class TestPgJsonKnownTypes:
    """Verify known type markers are identical between paths."""

    def _assert_match(self, record):
        mod1, name1, state_dict, refs1 = zodb_json_codec.decode_zodb_record_for_pg(record)
        mod2, name2, state_json, refs2 = zodb_json_codec.decode_zodb_record_for_pg_json(record)
        assert mod1 == mod2
        assert name1 == name2
        assert refs1 == refs2
        actual = json.loads(state_json)
        expected = _normalize(state_dict)
        assert actual == expected

    def test_datetime(self):
        dt = datetime(2024, 6, 15, 12, 30, 45, tzinfo=timezone.utc)
        record = make_zodb_record("myapp", "Obj", {"created": dt})
        self._assert_match(record)

    def test_date(self):
        record = make_zodb_record("myapp", "Obj", {"pub_date": date(2024, 3, 15)})
        self._assert_match(record)

    def test_timedelta(self):
        record = make_zodb_record("myapp", "Obj", {"duration": timedelta(days=7, hours=2)})
        self._assert_match(record)

    def test_decimal(self):
        record = make_zodb_record("myapp", "Obj", {"price": Decimal("19.99")})
        self._assert_match(record)

    def test_uuid(self):
        record = make_zodb_record(
            "myapp", "Obj",
            {"id": UUID("12345678-1234-5678-1234-567812345678")},
        )
        self._assert_match(record)

    def test_mixed_types(self):
        state = {
            "title": "Test",
            "created": datetime(2024, 1, 1, tzinfo=timezone.utc),
            "score": Decimal("3.14"),
            "tags": frozenset(["a", "b"]),
            "coords": (1.0, 2.0),
            "raw": b"\x00\x01",
        }
        record = make_zodb_record("myapp", "Obj", state)
        self._assert_match(record)


class TestPgJsonReturnType:
    """Verify the return type and format of the new function."""

    def test_returns_tuple(self):
        record = make_zodb_record("myapp", "Obj", {"x": 1})
        result = zodb_json_codec.decode_zodb_record_for_pg_json(record)
        assert isinstance(result, tuple)
        assert len(result) == 4

    def test_state_is_string(self):
        record = make_zodb_record("myapp", "Obj", {"x": 1})
        _, _, state_json, _ = zodb_json_codec.decode_zodb_record_for_pg_json(record)
        assert isinstance(state_json, str)
        # Must be valid JSON
        parsed = json.loads(state_json)
        assert isinstance(parsed, dict)

    def test_class_info(self):
        record = make_zodb_record("myapp.models", "Document", {})
        mod, name, _, _ = zodb_json_codec.decode_zodb_record_for_pg_json(record)
        assert mod == "myapp.models"
        assert name == "Document"

    def test_refs_are_ints(self):
        record = make_zodb_record("myapp", "Obj", {"x": 1})
        _, _, _, refs = zodb_json_codec.decode_zodb_record_for_pg_json(record)
        assert isinstance(refs, list)

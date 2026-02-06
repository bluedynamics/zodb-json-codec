"""Test ZODB record decoding/encoding.

ZODB stores each object as two concatenated pickles:
1. Class pickle: (module_name, class_name)
2. State pickle: the object's __getstate__() result
"""

import json
import pickle
from datetime import datetime

import pytest
from persistent import Persistent

import zodb_json_codec


# Module-level classes for ZODB pickling (must be importable)
class SampleObj(Persistent):
    pass


class DateObj(Persistent):
    pass


def make_zodb_record(module, classname, state, protocol=3):
    """Build a minimal ZODB-like record from class info and state."""
    class_pickle = pickle.dumps((module, classname), protocol=protocol)
    state_pickle = pickle.dumps(state, protocol=protocol)
    return class_pickle + state_pickle


class TestDecodeZodbRecord:
    def test_simple_object(self):
        record = make_zodb_record(
            "myapp.models",
            "Document",
            {"title": "Hello", "count": 42},
        )
        result = zodb_json_codec.decode_zodb_record(record)

        assert result["@cls"] == ["myapp.models", "Document"]
        assert result["@s"]["title"] == "Hello"
        assert result["@s"]["count"] == 42

    def test_nested_state(self):
        record = make_zodb_record(
            "myapp.models",
            "Container",
            {"items": [1, 2, 3], "metadata": {"created": "2025-01-01"}},
        )
        result = zodb_json_codec.decode_zodb_record(record)

        assert result["@cls"] == ["myapp.models", "Container"]
        assert result["@s"]["items"] == [1, 2, 3]
        assert result["@s"]["metadata"]["created"] == "2025-01-01"

    def test_empty_state(self):
        record = make_zodb_record("myapp", "Empty", {})
        result = zodb_json_codec.decode_zodb_record(record)
        assert result["@cls"] == ["myapp", "Empty"]
        assert result["@s"] == {}

    def test_bytes_in_state(self):
        record = make_zodb_record(
            "myapp",
            "BlobHolder",
            {"data": b"\x00\x01\x02\xff", "name": "test"},
        )
        result = zodb_json_codec.decode_zodb_record(record)
        assert result["@s"]["name"] == "test"
        # bytes should be base64-encoded with @b marker
        assert "@b" in result["@s"]["data"]

    def test_tuple_state(self):
        """Some ZODB objects have tuple state (not dict)."""
        record = make_zodb_record(
            "DateTime.DateTime",
            "DateTime",
            (1736937000000000, False, "UTC"),
        )
        result = zodb_json_codec.decode_zodb_record(record)
        assert result["@cls"] == ["DateTime.DateTime", "DateTime"]
        # State should be a tuple marker
        assert "@t" in result["@s"]
        assert result["@s"]["@t"][0] == 1736937000000000
        assert result["@s"]["@t"][1] is False
        assert result["@s"]["@t"][2] == "UTC"

    def test_scalar_state(self):
        """BTrees.Length stores just an integer as state."""
        record = make_zodb_record("BTrees.Length", "Length", 42)
        result = zodb_json_codec.decode_zodb_record(record)
        assert result["@cls"] == ["BTrees.Length", "Length"]
        assert result["@s"] == 42

    def test_none_values_in_state(self):
        record = make_zodb_record(
            "myapp", "Obj", {"a": None, "b": [None, 1], "c": {"d": None}}
        )
        result = zodb_json_codec.decode_zodb_record(record)
        assert result["@s"]["a"] is None
        assert result["@s"]["b"] == [None, 1]
        assert result["@s"]["c"]["d"] is None


class TestEncodeZodbRecord:
    def test_roundtrip(self):
        """Decode a record, then encode it back, then decode again."""
        original_state = {"title": "Test", "value": 123, "tags": ["a", "b"]}
        record = make_zodb_record("myapp", "Doc", original_state)

        # Decode
        decoded = zodb_json_codec.decode_zodb_record(record)
        assert decoded["@cls"] == ["myapp", "Doc"]

        # Re-encode
        re_encoded = zodb_json_codec.encode_zodb_record(decoded)

        # Decode again
        decoded2 = zodb_json_codec.decode_zodb_record(re_encoded)
        assert decoded2["@cls"] == decoded["@cls"]
        assert decoded2["@s"]["title"] == "Test"
        assert decoded2["@s"]["value"] == 123
        assert decoded2["@s"]["tags"] == ["a", "b"]

    def test_state_preserved_through_roundtrip(self):
        """Verify the re-encoded pickle produces equivalent Python objects."""
        original_state = {"x": 1, "y": "hello", "z": [True, None, 3.14]}
        record = make_zodb_record("pkg", "Cls", original_state)

        decoded = zodb_json_codec.decode_zodb_record(record)
        re_encoded = zodb_json_codec.encode_zodb_record(decoded)
        decoded2 = zodb_json_codec.decode_zodb_record(re_encoded)
        assert decoded2["@s"]["x"] == 1
        assert decoded2["@s"]["y"] == "hello"
        assert decoded2["@s"]["z"] == [True, None, 3.14]

    def test_class_pickle_uses_global(self):
        """Verify encoder produces GLOBAL opcode (like real ZODB), not tuple."""
        record = make_zodb_record("myapp", "Doc", {"x": 1})
        decoded = zodb_json_codec.decode_zodb_record(record)
        re_encoded = zodb_json_codec.encode_zodb_record(decoded)

        # The re-encoded record should start with PROTO + GLOBAL
        assert re_encoded[0:1] == b"\x80"  # PROTO
        assert re_encoded[2:3] == b"c"  # GLOBAL opcode


class TestRealZODB:
    """Integration tests with actual ZODB storage."""

    @pytest.fixture
    def zodb(self):
        from ZODB import DB
        import transaction

        db = DB(None)
        conn = db.open()
        root = conn.root()
        yield db, conn, root
        transaction.abort()
        conn.close()
        db.close()

    def test_persistent_mapping(self, zodb):
        from persistent.mapping import PersistentMapping
        import transaction

        db, conn, root = zodb
        root["test"] = PersistentMapping({"key": "value", "num": 42})
        transaction.commit()

        data, _ = db.storage.load(root["test"]._p_oid)
        result = zodb_json_codec.decode_zodb_record(data)

        assert result["@cls"] == ["persistent.mapping", "PersistentMapping"]
        assert result["@s"]["data"]["key"] == "value"
        assert result["@s"]["data"]["num"] == 42

    def test_persistent_list(self, zodb):
        from persistent.list import PersistentList
        import transaction

        db, conn, root = zodb
        root["test"] = PersistentList([10, 20, 30])
        transaction.commit()

        data, _ = db.storage.load(root["test"]._p_oid)
        result = zodb_json_codec.decode_zodb_record(data)

        assert result["@cls"] == ["persistent.list", "PersistentList"]
        assert result["@s"]["data"] == [10, 20, 30]

    def test_persistent_reference_format(self, zodb):
        """Persistent refs should use compact hex oid format."""
        from persistent.mapping import PersistentMapping
        import transaction

        db, conn, root = zodb
        child = PersistentMapping({"x": 1})
        root["child"] = child
        transaction.commit()

        # Root contains a ref to child
        data, _ = db.storage.load(root._p_oid)
        result = zodb_json_codec.decode_zodb_record(data)

        ref = result["@s"]["data"]["child"]["@ref"]
        # ref should be a string (hex oid) or [hex_oid, class_path]
        if isinstance(ref, str):
            # oid-only ref: hex string, 16 chars for 8-byte oid
            assert len(ref) == 16
            int(ref, 16)  # should parse as hex
        elif isinstance(ref, list):
            assert len(ref) == 2
            assert len(ref[0]) == 16
            int(ref[0], 16)
            assert isinstance(ref[1], str)  # class path

    def test_roundtrip_with_refs(self, zodb):
        """Encode a record with persistent refs, decode again."""
        from persistent.mapping import PersistentMapping
        import transaction

        db, conn, root = zodb
        root["a"] = PersistentMapping({"val": 1})
        root["b"] = PersistentMapping({"val": 2})
        transaction.commit()

        # Root has refs to both children
        data, _ = db.storage.load(root._p_oid)
        decoded = zodb_json_codec.decode_zodb_record(data)
        re_encoded = zodb_json_codec.encode_zodb_record(decoded)
        decoded2 = zodb_json_codec.decode_zodb_record(re_encoded)

        assert decoded["@cls"] == decoded2["@cls"]
        assert decoded["@s"] == decoded2["@s"]

    def test_complex_object(self, zodb):
        """Test with an object that has various Python types as attributes."""
        import transaction

        obj = SampleObj()
        obj.title = "Hello World"
        obj.count = 12345
        obj.ratio = 3.14
        obj.active = True
        obj.nothing = None
        obj.data = b"\xde\xad\xbe\xef"
        obj.tags = ("a", "b", "c")
        obj.config = {"nested": {"deep": True}}

        root = zodb[2]
        root["complex"] = obj
        transaction.commit()

        data, _ = zodb[0].storage.load(obj._p_oid)
        result = zodb_json_codec.decode_zodb_record(data)

        assert result["@s"]["title"] == "Hello World"
        assert result["@s"]["count"] == 12345
        assert result["@s"]["ratio"] == pytest.approx(3.14)
        assert result["@s"]["active"] is True
        assert result["@s"]["nothing"] is None
        assert "@b" in result["@s"]["data"]
        assert "@t" in result["@s"]["tags"]
        assert result["@s"]["config"]["nested"]["deep"] is True

        # Roundtrip
        re_encoded = zodb_json_codec.encode_zodb_record(result)
        result2 = zodb_json_codec.decode_zodb_record(re_encoded)
        assert result == result2

    def test_datetime_in_state(self, zodb):
        """datetime objects now use compact @dt marker."""
        import transaction

        obj = DateObj()
        obj.created = datetime(2025, 6, 15, 12, 0, 0)
        root = zodb[2]
        root["dateobj"] = obj
        transaction.commit()

        data, _ = zodb[0].storage.load(obj._p_oid)
        result = zodb_json_codec.decode_zodb_record(data)

        # datetime uses the compact @dt marker
        created = result["@s"]["created"]
        assert created == {"@dt": "2025-06-15T12:00:00"}

        # Roundtrip
        re_encoded = zodb_json_codec.encode_zodb_record(result)
        result2 = zodb_json_codec.decode_zodb_record(re_encoded)
        assert result == result2

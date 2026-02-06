"""Test BTree state flattening: nested tuples → @kv/@ks markers.

BTrees use deeply nested tuple state from __getstate__():
- Small BTree/TreeSet: 4-level nesting
- Bucket/Set: 2-level nesting
- Large BTree: persistent refs to child buckets

The codec flattens these into queryable JSON.
"""

import json
import pickle

import pytest

import zodb_json_codec


def make_zodb_record(module, classname, state, protocol=3):
    """Build a minimal ZODB-like record from class info and state."""
    class_pickle = pickle.dumps((module, classname), protocol=protocol)
    state_pickle = pickle.dumps(state, protocol=protocol)
    return class_pickle + state_pickle


class TestSmallOOBTree:
    """Small OOBTree with string keys (single inline bucket)."""

    def test_format(self):
        record = make_zodb_record(
            "BTrees.OOBTree", "OOBTree",
            (((("a", 1, "b", 2),),),),
        )
        result = zodb_json_codec.decode_zodb_record(record)
        assert result["@cls"] == ["BTrees.OOBTree", "OOBTree"]
        assert "@kv" in result["@s"]
        assert result["@s"]["@kv"] == [["a", 1], ["b", 2]]

    def test_roundtrip(self):
        record = make_zodb_record(
            "BTrees.OOBTree", "OOBTree",
            (((("a", 1, "b", 2),),),),
        )
        decoded = zodb_json_codec.decode_zodb_record(record)
        re_encoded = zodb_json_codec.encode_zodb_record(decoded)
        decoded2 = zodb_json_codec.decode_zodb_record(re_encoded)
        assert decoded == decoded2


class TestSmallIIBTree:
    """Small IIBTree with integer keys and values."""

    def test_format(self):
        record = make_zodb_record(
            "BTrees.IIBTree", "IIBTree",
            ((((1, 100, 2, 200),),),),
        )
        result = zodb_json_codec.decode_zodb_record(record)
        assert result["@cls"] == ["BTrees.IIBTree", "IIBTree"]
        assert result["@s"]["@kv"] == [[1, 100], [2, 200]]

    def test_roundtrip(self):
        record = make_zodb_record(
            "BTrees.IIBTree", "IIBTree",
            ((((1, 100, 2, 200),),),),
        )
        decoded = zodb_json_codec.decode_zodb_record(record)
        re_encoded = zodb_json_codec.encode_zodb_record(decoded)
        decoded2 = zodb_json_codec.decode_zodb_record(re_encoded)
        assert decoded == decoded2


class TestSmallIOBTree:
    """Small IOBTree with integer keys, object values."""

    def test_format(self):
        record = make_zodb_record(
            "BTrees.IOBTree", "IOBTree",
            ((((1, "hello", 2, "world"),),),),
        )
        result = zodb_json_codec.decode_zodb_record(record)
        assert result["@cls"] == ["BTrees.IOBTree", "IOBTree"]
        assert result["@s"]["@kv"] == [[1, "hello"], [2, "world"]]

    def test_roundtrip(self):
        record = make_zodb_record(
            "BTrees.IOBTree", "IOBTree",
            ((((1, "hello", 2, "world"),),),),
        )
        decoded = zodb_json_codec.decode_zodb_record(record)
        re_encoded = zodb_json_codec.encode_zodb_record(decoded)
        decoded2 = zodb_json_codec.decode_zodb_record(re_encoded)
        assert decoded == decoded2


class TestSmallTreeSet:
    """IITreeSet with integer keys (set, no values)."""

    def test_format(self):
        record = make_zodb_record(
            "BTrees.IIBTree", "IITreeSet",
            ((((1, 2, 3),),),),
        )
        result = zodb_json_codec.decode_zodb_record(record)
        assert result["@cls"] == ["BTrees.IIBTree", "IITreeSet"]
        assert "@ks" in result["@s"]
        assert result["@s"]["@ks"] == [1, 2, 3]

    def test_roundtrip(self):
        record = make_zodb_record(
            "BTrees.IIBTree", "IITreeSet",
            ((((1, 2, 3),),),),
        )
        decoded = zodb_json_codec.decode_zodb_record(record)
        re_encoded = zodb_json_codec.encode_zodb_record(decoded)
        decoded2 = zodb_json_codec.decode_zodb_record(re_encoded)
        assert decoded == decoded2


class TestBucket:
    """Standalone OOBucket (2-level nesting)."""

    def test_format(self):
        record = make_zodb_record(
            "BTrees.OOBTree", "OOBucket",
            (("x", 10, "y", 20),),
        )
        result = zodb_json_codec.decode_zodb_record(record)
        assert result["@cls"] == ["BTrees.OOBTree", "OOBucket"]
        assert result["@s"]["@kv"] == [["x", 10], ["y", 20]]

    def test_roundtrip(self):
        record = make_zodb_record(
            "BTrees.OOBTree", "OOBucket",
            (("x", 10, "y", 20),),
        )
        decoded = zodb_json_codec.decode_zodb_record(record)
        re_encoded = zodb_json_codec.encode_zodb_record(decoded)
        decoded2 = zodb_json_codec.decode_zodb_record(re_encoded)
        assert decoded == decoded2


class TestSet:
    """Standalone OOSet (2-level nesting, keys only)."""

    def test_format(self):
        record = make_zodb_record(
            "BTrees.OOBTree", "OOSet",
            (("a", "b", "c"),),
        )
        result = zodb_json_codec.decode_zodb_record(record)
        assert result["@cls"] == ["BTrees.OOBTree", "OOSet"]
        assert result["@s"]["@ks"] == ["a", "b", "c"]

    def test_roundtrip(self):
        record = make_zodb_record(
            "BTrees.OOBTree", "OOSet",
            (("a", "b", "c"),),
        )
        decoded = zodb_json_codec.decode_zodb_record(record)
        re_encoded = zodb_json_codec.encode_zodb_record(decoded)
        decoded2 = zodb_json_codec.decode_zodb_record(re_encoded)
        assert decoded == decoded2


class TestEmptyBTree:
    """Empty BTree (state is None)."""

    def test_format(self):
        record = make_zodb_record(
            "BTrees.OOBTree", "OOBTree",
            None,
        )
        result = zodb_json_codec.decode_zodb_record(record)
        assert result["@cls"] == ["BTrees.OOBTree", "OOBTree"]
        assert result["@s"] is None

    def test_roundtrip(self):
        record = make_zodb_record(
            "BTrees.OOBTree", "OOBTree",
            None,
        )
        decoded = zodb_json_codec.decode_zodb_record(record)
        re_encoded = zodb_json_codec.encode_zodb_record(decoded)
        decoded2 = zodb_json_codec.decode_zodb_record(re_encoded)
        assert decoded == decoded2


class TestLength:
    """BTrees.Length stores just an integer. No change needed."""

    def test_format(self):
        record = make_zodb_record(
            "BTrees.Length", "Length",
            42,
        )
        result = zodb_json_codec.decode_zodb_record(record)
        assert result["@cls"] == ["BTrees.Length", "Length"]
        assert result["@s"] == 42

    def test_roundtrip(self):
        record = make_zodb_record(
            "BTrees.Length", "Length",
            42,
        )
        decoded = zodb_json_codec.decode_zodb_record(record)
        re_encoded = zodb_json_codec.encode_zodb_record(decoded)
        decoded2 = zodb_json_codec.decode_zodb_record(re_encoded)
        assert decoded == decoded2


class TestEmptyInlineBTree:
    """Empty inline BTree (state is ((((),),),))."""

    def test_format(self):
        record = make_zodb_record(
            "BTrees.OOBTree", "OOBTree",
            ((((),),),),
        )
        result = zodb_json_codec.decode_zodb_record(record)
        assert result["@s"]["@kv"] == []

    def test_roundtrip(self):
        record = make_zodb_record(
            "BTrees.OOBTree", "OOBTree",
            ((((),),),),
        )
        decoded = zodb_json_codec.decode_zodb_record(record)
        re_encoded = zodb_json_codec.encode_zodb_record(decoded)
        decoded2 = zodb_json_codec.decode_zodb_record(re_encoded)
        assert decoded == decoded2


class TestEmptyBucket:
    """Empty bucket (state is ((),))."""

    def test_format(self):
        record = make_zodb_record(
            "BTrees.OOBTree", "OOBucket",
            ((),),
        )
        result = zodb_json_codec.decode_zodb_record(record)
        assert result["@s"]["@kv"] == []

    def test_roundtrip(self):
        record = make_zodb_record(
            "BTrees.OOBTree", "OOBucket",
            ((),),
        )
        decoded = zodb_json_codec.decode_zodb_record(record)
        re_encoded = zodb_json_codec.encode_zodb_record(decoded)
        decoded2 = zodb_json_codec.decode_zodb_record(re_encoded)
        assert decoded == decoded2


class TestRealZODB:
    """Integration tests with actual ZODB storage and BTrees objects."""

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

    def test_oobtree(self, zodb):
        from BTrees.OOBTree import OOBTree
        import transaction

        db, conn, root = zodb
        tree = OOBTree()
        tree["alpha"] = "first"
        tree["beta"] = "second"
        tree["gamma"] = "third"
        root["tree"] = tree
        transaction.commit()

        data, _ = db.storage.load(tree._p_oid)
        result = zodb_json_codec.decode_zodb_record(data)

        assert result["@cls"] == ["BTrees.OOBTree", "OOBTree"]
        assert "@kv" in result["@s"]
        kv = result["@s"]["@kv"]
        # BTree keeps items sorted
        keys = [pair[0] for pair in kv]
        assert keys == sorted(keys)
        # Check key-value pairs
        kv_dict = {pair[0]: pair[1] for pair in kv}
        assert kv_dict["alpha"] == "first"
        assert kv_dict["beta"] == "second"
        assert kv_dict["gamma"] == "third"

        # Roundtrip
        re_encoded = zodb_json_codec.encode_zodb_record(result)
        result2 = zodb_json_codec.decode_zodb_record(re_encoded)
        assert result == result2

    def test_iibtree(self, zodb):
        from BTrees.IIBTree import IIBTree
        import transaction

        db, conn, root = zodb
        tree = IIBTree()
        tree[1] = 100
        tree[2] = 200
        tree[3] = 300
        root["iitree"] = tree
        transaction.commit()

        data, _ = db.storage.load(tree._p_oid)
        result = zodb_json_codec.decode_zodb_record(data)

        assert result["@cls"] == ["BTrees.IIBTree", "IIBTree"]
        kv = result["@s"]["@kv"]
        kv_dict = {pair[0]: pair[1] for pair in kv}
        assert kv_dict[1] == 100
        assert kv_dict[2] == 200
        assert kv_dict[3] == 300

        # Roundtrip
        re_encoded = zodb_json_codec.encode_zodb_record(result)
        result2 = zodb_json_codec.decode_zodb_record(re_encoded)
        assert result == result2

    def test_iitreeset(self, zodb):
        from BTrees.IIBTree import IITreeSet
        import transaction

        db, conn, root = zodb
        ts = IITreeSet()
        ts.insert(10)
        ts.insert(20)
        ts.insert(30)
        root["treeset"] = ts
        transaction.commit()

        data, _ = db.storage.load(ts._p_oid)
        result = zodb_json_codec.decode_zodb_record(data)

        assert result["@cls"] == ["BTrees.IIBTree", "IITreeSet"]
        assert "@ks" in result["@s"]
        assert sorted(result["@s"]["@ks"]) == [10, 20, 30]

        # Roundtrip
        re_encoded = zodb_json_codec.encode_zodb_record(result)
        result2 = zodb_json_codec.decode_zodb_record(re_encoded)
        assert result == result2

    def test_length(self, zodb):
        from BTrees.Length import Length
        import transaction

        db, conn, root = zodb
        length = Length()
        length.set(42)
        root["length"] = length
        transaction.commit()

        data, _ = db.storage.load(length._p_oid)
        result = zodb_json_codec.decode_zodb_record(data)

        assert result["@cls"] == ["BTrees.Length", "Length"]
        assert result["@s"] == 42

        # Roundtrip
        re_encoded = zodb_json_codec.encode_zodb_record(result)
        result2 = zodb_json_codec.decode_zodb_record(re_encoded)
        assert result == result2

    def test_empty_oobtree(self, zodb):
        from BTrees.OOBTree import OOBTree
        import transaction

        db, conn, root = zodb
        tree = OOBTree()
        root["empty"] = tree
        transaction.commit()

        data, _ = db.storage.load(tree._p_oid)
        result = zodb_json_codec.decode_zodb_record(data)

        assert result["@cls"] == ["BTrees.OOBTree", "OOBTree"]
        assert result["@s"] is None

        # Roundtrip
        re_encoded = zodb_json_codec.encode_zodb_record(result)
        result2 = zodb_json_codec.decode_zodb_record(re_encoded)
        assert result == result2

    def test_large_oobtree(self, zodb):
        """Large OOBTree with enough items to force bucket splits."""
        from BTrees.OOBTree import OOBTree
        import transaction

        db, conn, root = zodb
        tree = OOBTree()
        for i in range(100):
            tree[f"key_{i:04d}"] = f"value_{i}"
        root["large"] = tree
        transaction.commit()

        data, _ = db.storage.load(tree._p_oid)
        result = zodb_json_codec.decode_zodb_record(data)

        assert result["@cls"] == ["BTrees.OOBTree", "OOBTree"]
        state = result["@s"]
        # Large BTree should have @children and @first
        assert "@children" in state
        assert "@first" in state
        # Children should contain refs and separator keys
        children = state["@children"]
        assert len(children) > 1
        # Should have some @ref entries
        refs = [c for c in children if isinstance(c, dict) and "@ref" in c]
        assert len(refs) >= 2

        # Roundtrip
        re_encoded = zodb_json_codec.encode_zodb_record(result)
        result2 = zodb_json_codec.decode_zodb_record(re_encoded)
        assert result == result2

    def test_ooset(self, zodb):
        from BTrees.OOBTree import OOSet
        import transaction

        db, conn, root = zodb
        s = OOSet()
        s.insert("x")
        s.insert("y")
        s.insert("z")
        root["ooset"] = s
        transaction.commit()

        data, _ = db.storage.load(s._p_oid)
        result = zodb_json_codec.decode_zodb_record(data)

        assert result["@cls"] == ["BTrees.OOBTree", "OOSet"]
        assert "@ks" in result["@s"]
        assert sorted(result["@s"]["@ks"]) == ["x", "y", "z"]

        # Roundtrip
        re_encoded = zodb_json_codec.encode_zodb_record(result)
        result2 = zodb_json_codec.decode_zodb_record(re_encoded)
        assert result == result2


class TestPickleRoundtrip:
    """Test standalone pickle (non-ZODB) roundtrip via pickle_to_json/json_to_pickle."""

    def test_oobtree_standalone(self):
        from BTrees.OOBTree import OOBTree

        tree = OOBTree()
        tree["a"] = 1
        tree["b"] = 2
        data = pickle.dumps(tree, protocol=3)
        json_str = zodb_json_codec.pickle_to_json(data)
        result = json.loads(json_str)

        assert result["@cls"] == ["BTrees.OOBTree", "OOBTree"]
        assert "@kv" in result["@s"]

        # Roundtrip
        restored_bytes = zodb_json_codec.json_to_pickle(json_str)
        json_str2 = zodb_json_codec.pickle_to_json(restored_bytes)
        result2 = json.loads(json_str2)
        assert result == result2

    def test_iibucket_standalone(self):
        from BTrees.IIBTree import IIBucket

        bucket = IIBucket()
        bucket[1] = 10
        bucket[2] = 20
        data = pickle.dumps(bucket, protocol=3)
        json_str = zodb_json_codec.pickle_to_json(data)
        result = json.loads(json_str)

        assert result["@cls"] == ["BTrees.IIBTree", "IIBucket"]
        assert "@kv" in result["@s"]

        # Roundtrip
        restored_bytes = zodb_json_codec.json_to_pickle(json_str)
        json_str2 = zodb_json_codec.pickle_to_json(restored_bytes)
        result2 = json.loads(json_str2)
        assert result == result2

"""All persistent id shapes ZODB writes round-trip and are counted like referencesf (#17)."""

from BTrees.OOBTree import OOBTree
from persistent import Persistent
from persistent.mapping import PersistentMapping
from persistent.wref import WeakRef
from ZODB.MappingStorage import MappingStorage
from ZODB.serialize import referencesf

import io
import json
import pickle
import pickletools
import pytest
import transaction
import ZODB
import zodb_json_codec


OID5 = b"\x00\x00\x00\x00\x00\x00\x00\x05"
OID7 = b"\x00\x00\x00\x00\x00\x00\x00\x07"
OID9 = b"\x00\x00\x00\x00\x00\x00\x00\x09"


class RefPickler(pickle.Pickler):
    """("PREF", pid) tuples become persistent ids, like ZODB's ObjectWriter."""

    def persistent_id(self, obj):
        if isinstance(obj, tuple) and len(obj) == 2 and obj[0] == "PREF":
            return obj[1]
        return None


def make_record(state):
    buf = io.BytesIO()
    pickle.Pickler(buf, protocol=3).dump(("myapp.models", "Doc"))
    RefPickler(buf, protocol=3).dump(state)
    return buf.getvalue()


def load_state(record):
    u = pickle.Unpickler(io.BytesIO(record))
    u.persistent_load = lambda pid: ("REF", pid)
    u.load()
    return u.load()


def roundtrip_pg_json(state):
    mod, name, js, refs = zodb_json_codec.decode_zodb_record_for_pg_json(
        make_record(state)
    )
    back = zodb_json_codec.encode_zodb_record(
        {"@cls": [mod, name], "@s": json.loads(js)}
    )
    return load_state(back), refs


# Every shape ZODB.serialize.ObjectWriter.persistent_id can return
ALL_SHAPES = {
    "tuple": ("PREF", (OID5, PersistentMapping)),
    "tuple_none": ("PREF", (OID5, None)),
    "bare": ("PREF", OID7),
    "weak": ("PREF", ["w", (OID9,)]),
    "weak_db": ("PREF", ["w", (OID9, "other")]),
    "multi": ("PREF", ["m", ("other", OID9, PersistentMapping)]),
    "multi_newargs": ("PREF", ["n", ("other", OID9)]),
}


class TestRoundTrip:
    @pytest.mark.parametrize("shape", sorted(ALL_SHAPES))
    def test_shape_roundtrips_pg_json(self, shape):
        pid = ALL_SHAPES[shape][1]
        back, _ = roundtrip_pg_json({"x": ALL_SHAPES[shape]})
        assert back["x"] == ("REF", pid)

    @pytest.mark.parametrize("shape", sorted(ALL_SHAPES))
    def test_shape_roundtrips_dict_path(self, shape):
        pid = ALL_SHAPES[shape][1]
        decoded = zodb_json_codec.decode_zodb_record(
            make_record({"x": ALL_SHAPES[shape]})
        )
        back = load_state(zodb_json_codec.encode_zodb_record(decoded))
        assert back["x"] == ("REF", pid)

    def test_multidb_ref_roundtrip(self):
        back, _ = roundtrip_pg_json({"x": ALL_SHAPES["multi"]})
        assert back["x"][1][1][2] is PersistentMapping

    def test_compact_ref_without_module(self):
        # ["hex", "Cls"] is the compact form for a class without module: the encoder
        # must emit GLOBAL "" "Cls" and the oid under TUPLE2 BINPERSID.
        data = zodb_json_codec.encode_zodb_record(
            {"@cls": ["m", "C"], "@s": {"x": {"@ref": ["0000000000000005", "Cls"]}}}
        )
        assert OID5 in data
        ops = [
            (op.name, arg)
            for op, arg, _pos in pickletools.genops(data.split(b"\x80\x02", 2)[2])
        ]
        i = ops.index(("GLOBAL", " Cls"))
        assert [name for name, _ in ops[i + 1 : i + 3]] == ["TUPLE2", "BINPERSID"]

    @pytest.mark.parametrize(
        "ref, message",
        [
            (["05", "m.C"], "8 bytes"),
            ("05", "8 bytes"),
            (["0000000000000005", None], "compact"),
        ],
    )
    def test_malformed_compact_refs_are_rejected(self, ref, message):
        with pytest.raises(ValueError, match=message):
            zodb_json_codec.encode_zodb_record(
                {"@cls": ["m", "C"], "@s": {"x": {"@ref": ref}}}
            )

    def test_stored_weakref_json_from_1_6_1_encodes(self):
        stored = '{"x":{"@ref":["w",{"@t":[{"@b":"AAAAAAAAAAk="}]}]}}'
        back = load_state(
            zodb_json_codec.encode_zodb_record(
                {"@cls": ["m", "C"], "@s": json.loads(stored)}
            )
        )
        assert back["x"] == ("REF", ["w", (OID9,)])

    def test_malformed_compact_ref_reports_hex(self):
        with pytest.raises(ValueError, match="hex"):
            zodb_json_codec.encode_zodb_record(
                {"@cls": ["m", "C"], "@s": {"x": {"@ref": ["w", "x"]}}}
            )


class TestRefs:
    def test_refs_match_referencesf(self):
        record = make_record(dict(ALL_SHAPES))
        expected = sorted(int.from_bytes(o, "big") for o in referencesf(record))
        _, _, _, refs_json = zodb_json_codec.decode_zodb_record_for_pg_json(record)
        _, _, _, refs_dict = zodb_json_codec.decode_zodb_record_for_pg(record)
        assert sorted(refs_json) == expected
        assert sorted(refs_dict) == expected
        assert 7 in refs_json  # bare oid counted
        assert 9 not in refs_json  # weak / multi-database excluded


class Plain(Persistent):
    pass


class NewArgs(Persistent):
    """__getnewargs__ makes ZODB write a bare-oid persistent id for references to it."""

    def __getnewargs__(self):
        return ()


class TestRealZodbRecords:
    def test_real_zodb_records_match_referencesf_and_reload(self):
        # Records written by ZODB itself: plain ref, weakref, bare-oid ref, cross-database
        # 'm' and 'n' refs, in a PersistentMapping and an OOBTree. The re-encoded pickle is
        # read back by ZODB's own ObjectReader, which resolves every id form.
        dbs = {}
        db1 = ZODB.DB(MappingStorage(), databases=dbs, database_name="one")
        ZODB.DB(MappingStorage(), databases=dbs, database_name="two")
        conn1 = db1.open()
        conn2 = conn1.get_connection("two")
        try:
            r1, r2 = conn1.root(), conn2.root()
            a, b, na = Plain(), Plain(), NewArgs()
            pm = PersistentMapping()
            r1["pm"], r1["b"] = pm, b
            pm["a"], pm["w"], pm["na"] = a, WeakRef(b), na
            bt = OOBTree()
            r1["bt"] = bt
            bt["a"], bt["w"], bt["na"] = a, WeakRef(b), na
            o2, na2 = Plain(), NewArgs()
            conn2.add(o2)
            conn2.add(na2)
            r2["o2"], r2["na2"] = o2, na2
            # cross-database: ['m', ('two', oid, Plain)] and ['n', ('two', oid)]
            r1["x_m"], r1["x_n"] = o2, na2
            transaction.commit()

            for obj in (r1, pm, bt):
                data, _ = db1.storage.load(obj._p_oid)
                expected = sorted(int.from_bytes(o, "big") for o in referencesf(data))
                mod, name, js, refs_json = (
                    zodb_json_codec.decode_zodb_record_for_pg_json(data)
                )
                _, _, _, refs_dict = zodb_json_codec.decode_zodb_record_for_pg(data)
                back = zodb_json_codec.encode_zodb_record(
                    {"@cls": [mod, name], "@s": json.loads(js)}
                )
                assert sorted(refs_json) == expected
                assert sorted(refs_dict) == expected
                assert (
                    sorted(int.from_bytes(o, "big") for o in referencesf(back))
                    == expected
                )
                assert conn1._reader.getState(back) == conn1._reader.getState(data)
        finally:
            transaction.abort()
            conn1.close()
            db1.close()

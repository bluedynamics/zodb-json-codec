"""All persistent id shapes ZODB writes round-trip and are counted like referencesf (#17)."""

from persistent.mapping import PersistentMapping
from ZODB.serialize import referencesf

import io
import json
import pickle
import pytest
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
        # ["hex", "Cls"] is the compact form for a class without module; the encoder
        # must recognize it (no ValueError/TypeError) and emit the oid.
        data = zodb_json_codec.encode_zodb_record(
            {"@cls": ["m", "C"], "@s": {"x": {"@ref": ["0000000000000005", "Cls"]}}}
        )
        assert OID5 in data

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

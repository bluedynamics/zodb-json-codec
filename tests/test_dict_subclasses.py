"""Dict/list subclass contents (SETITEMS/APPENDS after REDUCE/NEWOBJ) survive every codec path (#16)."""

from collections import defaultdict
from collections import deque
from collections import OrderedDict

import io
import json
import pickle
import pytest
import zodb_json_codec


class ODSub(OrderedDict):
    """Importable OrderedDict subclass: pickles as REDUCE + SETITEMS + BUILD (inherits __reduce__)."""


class DictSub(dict):
    """Importable plain dict subclass: pickles as NEWOBJ + SETITEMS + BUILD."""


class ListSub(list):
    """Importable list subclass: pickles as NEWOBJ + APPENDS + BUILD."""


def make_record(state, protocol=3):
    return pickle.dumps(("myapp.models", "Doc"), protocol=protocol) + pickle.dumps(
        state, protocol=protocol
    )


def load_state(record):
    """Unpickle a two-pickle record like ZODB does (one unpickler, shared memo)."""
    u = pickle.Unpickler(io.BytesIO(record))
    u.persistent_load = lambda pid: ("REF", pid)
    u.load()
    return u.load()


class RefPickler(pickle.Pickler):
    """Turns ("PREF", pid) tuples into persistent ids, like ZODB's ObjectWriter."""

    def persistent_id(self, obj):
        if isinstance(obj, tuple) and len(obj) == 2 and obj[0] == "PREF":
            return obj[1]
        return None


def roundtrip_pg_json(state):
    mod, name, js, _refs = zodb_json_codec.decode_zodb_record_for_pg_json(
        make_record(state)
    )
    return load_state(
        zodb_json_codec.encode_zodb_record({"@cls": [mod, name], "@s": json.loads(js)})
    )


def roundtrip_dict_path(state):
    return load_state(
        zodb_json_codec.encode_zodb_record(
            zodb_json_codec.decode_zodb_record(make_record(state))
        )
    )


class TestOrderedDict:
    def test_ordereddict_roundtrip_pg_json(self):
        od = OrderedDict([("z", 1), ("a", 2), ("m", 3)])
        back = roundtrip_pg_json({"od": od})["od"]
        assert isinstance(back, OrderedDict)
        assert list(back.items()) == [("z", 1), ("a", 2), ("m", 3)]

    def test_ordereddict_roundtrip_dict_path(self):
        od = OrderedDict([("z", 1), ("a", 2)])
        back = roundtrip_dict_path({"od": od})["od"]
        assert list(back.items()) == [("z", 1), ("a", 2)]

    def test_dict_path_exposes_items(self):
        decoded = zodb_json_codec.decode_zodb_record(
            make_record({"od": OrderedDict([("k", "v")])})
        )
        assert decoded["@s"]["od"]["@reduce"]["items"] == [["k", "v"]]

    def test_empty_ordereddict_roundtrip(self):
        back = roundtrip_pg_json({"od": OrderedDict()})["od"]
        assert isinstance(back, OrderedDict)
        assert back == OrderedDict()

    def test_ordereddict_subclass_with_attribute(self):
        od = ODSub([("k1", "v1")])
        od.extra = "attr"
        back = roundtrip_pg_json({"od": od})["od"]
        assert isinstance(back, ODSub)
        assert list(back.items()) == [("k1", "v1")]
        assert back.extra == "attr"


class TestDefaultDictAndDeque:
    def test_defaultdict_roundtrip(self):
        dd = defaultdict(list)
        dd["x"].append(1)
        back = roundtrip_pg_json({"dd": dd})["dd"]
        assert isinstance(back, defaultdict)
        assert back.default_factory is list
        assert dict(back) == {"x": [1]}

    def test_deque_roundtrip_pg_json(self):
        back = roundtrip_pg_json({"dq": deque([1, 2, 3])})["dq"]
        assert isinstance(back, deque)
        assert list(back) == [1, 2, 3]

    def test_list_subclass_with_attribute(self):
        ls = ListSub([1, 2])
        ls.tag = "t"
        back = roundtrip_pg_json({"ls": ls})["ls"]
        assert isinstance(back, ListSub)
        assert list(back) == [1, 2]
        assert back.tag == "t"


class TestStoredJson:
    def test_stored_json_from_1_6_1_encodes(self):
        # Literal output of decode_zodb_record_for_pg_json in 1.6.1, keys reordered like JSONB
        stored = (
            '{"od":{"@reduce":{"items":[["k1","v1"],["k2",2]],"args":{"@t":[]},'
            '"callable":{"@cls":["collections","OrderedDict"]}}},'
            '"dq":{"@reduce":{"appends":[1,2,3],"args":{"@t":[]},'
            '"callable":{"@cls":["collections","deque"]}}}}'
        )
        back = load_state(
            zodb_json_codec.encode_zodb_record(
                {"@cls": ["myapp.models", "Doc"], "@s": json.loads(stored)}
            )
        )
        assert list(back["od"].items()) == [("k1", "v1"), ("k2", 2)]
        assert list(back["dq"]) == [1, 2, 3]

    def test_stored_instance_items_encode(self):
        # Shape written by the JSON writer for NEWOBJ + SETITEMS + BUILD instances
        stored = (
            '{"od":{"@s":{"extra":"attr"},"@cls":["test_dict_subclasses","ODSub"],'
            '"@items":[["k1","v1"]]}}'
        )
        back = load_state(
            zodb_json_codec.encode_zodb_record(
                {"@cls": ["myapp.models", "Doc"], "@s": json.loads(stored)}
            )
        )
        assert isinstance(back["od"], ODSub)
        assert list(back["od"].items()) == [("k1", "v1")]
        assert back["od"].extra == "attr"


class TestMoreShapes:
    def test_dict_subclass_newobj_roundtrip(self):
        ds = DictSub(a=1)
        ds.extra = "x"
        back = roundtrip_pg_json({"ds": ds})["ds"]
        assert isinstance(back, DictSub)
        assert dict(back) == {"a": 1}
        assert back.extra == "x"

    def test_items_with_non_string_keys(self):
        od = OrderedDict([(1, "a"), ((2, 3), "b"), (None, "c")])
        back = roundtrip_pg_json({"od": od})["od"]
        assert list(back.items()) == [(1, "a"), ((2, 3), "b"), (None, "c")]

    def test_nested_subclasses(self):
        inner = OrderedDict([("i", deque([1, 2]))])
        back = roundtrip_pg_json({"od": OrderedDict([("outer", inner)])})["od"]
        assert isinstance(back["outer"], OrderedDict)
        assert isinstance(back["outer"]["i"], deque)
        assert list(back["outer"]["i"]) == [1, 2]

    def test_persistent_ref_inside_items_is_collected_and_restored(self):
        oid = b"\x00\x00\x00\x00\x00\x00\x00\x05"
        buf = io.BytesIO()
        pickle.Pickler(buf, protocol=3).dump(("myapp.models", "Doc"))
        RefPickler(buf, protocol=3).dump(
            {
                "od": OrderedDict([("child", ("PREF", (oid, None)))]),
                "dq": deque([("PREF", (oid, None))]),
            }
        )
        mod, name, js, refs = zodb_json_codec.decode_zodb_record_for_pg_json(
            buf.getvalue()
        )
        assert refs == [5, 5]
        back = load_state(
            zodb_json_codec.encode_zodb_record(
                {"@cls": [mod, name], "@s": json.loads(js)}
            )
        )
        assert back["od"]["child"] == ("REF", (oid, None))
        assert list(back["dq"]) == [("REF", (oid, None))]

    def test_stored_appends_instance_literal(self):
        stored = (
            '{"ls":{"@appends":[1,2],"@s":{"tag":"t"},'
            '"@cls":["test_dict_subclasses","ListSub"]}}'
        )
        back = load_state(
            zodb_json_codec.encode_zodb_record(
                {"@cls": ["myapp.models", "Doc"], "@s": json.loads(stored)}
            )
        )
        assert isinstance(back["ls"], ListSub)
        assert list(back["ls"]) == [1, 2]
        assert back["ls"].tag == "t"


class TestMalformedInput:
    def test_cls_with_items_but_no_state_is_rejected(self):
        with pytest.raises(ValueError, match="@items"):
            zodb_json_codec.encode_zodb_record(
                {
                    "@cls": ["m", "C"],
                    "@s": {"x": {"@cls": ["m", "D"], "@items": [["k", 1]]}},
                }
            )

    def test_malformed_items_reports_the_key(self):
        with pytest.raises(ValueError, match="items"):
            zodb_json_codec.encode_zodb_record(
                {
                    "@cls": ["m", "C"],
                    "@s": {
                        "x": {
                            "@reduce": {
                                "callable": {"@cls": ["m", "D"]},
                                "args": {"@t": []},
                                "items": "nope",
                            }
                        }
                    },
                }
            )

    def test_malformed_appends_reports_the_key(self):
        with pytest.raises(ValueError, match="appends"):
            zodb_json_codec.encode_zodb_record(
                {
                    "@cls": ["m", "C"],
                    "@s": {
                        "x": {
                            "@cls": ["m", "D"],
                            "@s": None,
                            "@appends": {"not": "a list"},
                        }
                    },
                }
            )

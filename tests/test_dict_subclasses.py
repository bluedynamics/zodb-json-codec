"""Dict/list subclass contents (SETITEMS/APPENDS after REDUCE/NEWOBJ) survive every codec path (#16)."""

from collections import defaultdict
from collections import deque
from collections import OrderedDict

import io
import json
import pickle
import zodb_json_codec


class ODSub(OrderedDict):
    """Importable OrderedDict subclass: pickles as NEWOBJ + SETITEMS + BUILD."""


class ListSub(list):
    """Importable list subclass: pickles as NEWOBJ + APPENDS + BUILD."""


def make_record(state, protocol=3):
    return pickle.dumps(("myapp.models", "Doc"), protocol=protocol) + pickle.dumps(
        state, protocol=protocol
    )


def load_state(record):
    """Unpickle a two-pickle record like ZODB does (one unpickler, shared memo)."""
    u = pickle.Unpickler(io.BytesIO(record))
    u.load()
    return u.load()


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

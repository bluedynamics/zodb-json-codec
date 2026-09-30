"""The tutorial's round-trip example, run as printed (#27)."""

import pickle
import zodb_json_codec


def test_tutorial_roundtrip_example():
    original = {
        "name": "Alice",
        "scores": (95, 87, 92),
        "avatar": b"\x89PNG\r\n",
        "active": True,
        "tags": ["staff", "admin"],
    }
    pickled = pickle.dumps(original, protocol=3)

    json_str = zodb_json_codec.pickle_to_json(pickled)
    restored_pickle = zodb_json_codec.json_to_pickle(json_str)
    assert pickle.loads(restored_pickle) == original

    as_dict = zodb_json_codec.pickle_to_dict(pickled)
    restored_pickle2 = zodb_json_codec.dict_to_pickle(as_dict)
    assert pickle.loads(restored_pickle2) == original

    # neither path reproduces the input bytes (memo opcodes are dropped, the
    # header is rewritten), and the two paths differ from each other too: the
    # JSON string path sorts dict keys and writes protocol 3, the dict path
    # keeps key order and writes protocol 2
    assert restored_pickle != pickled
    assert restored_pickle2 != pickled
    assert restored_pickle[:2] == b"\x80\x03" and restored_pickle2[:2] == b"\x80\x02"

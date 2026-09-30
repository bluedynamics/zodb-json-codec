"""Memo pre-scan: puts nobody reads are skipped, shared references still resolve (#22)."""

import io
import json
import pickle
import pickletools
import pytest
import zodb_json_codec


def two_pickles(state):
    return pickle.dumps(("m", "C"), protocol=3) + pickle.dumps(state, protocol=3)


def load_state(record):
    u = pickle.Unpickler(io.BytesIO(record))
    u.load()
    return u.load()


def shared_state():
    inner = {"x": 1, "y": [1, 2]}
    return {"a": inner, "b": inner, "l": [inner, inner], "t": (inner, "s", "s")}


EXPECTED = {
    "a": {"x": 1, "y": [1, 2]},
    "b": {"x": 1, "y": [1, 2]},
    "l": [{"x": 1, "y": [1, 2]}, {"x": 1, "y": [1, 2]}],
    "t": ({"x": 1, "y": [1, 2]}, "s", "s"),
}


def through_json(data):
    return pickle.loads(
        zodb_json_codec.json_to_pickle(zodb_json_codec.pickle_to_json(data))
    )


@pytest.mark.parametrize("protocol", [0, 1, 2, 3, 4, 5])
def test_shared_references_every_protocol(protocol):
    assert through_json(pickle.dumps(shared_state(), protocol=protocol)) == EXPECTED


def test_optimized_pickle_without_puts():
    data = pickletools.optimize(pickle.dumps(shared_state(), protocol=3))
    assert through_json(data) == EXPECTED


def test_record_paths_shared_references():
    rec = two_pickles(shared_state())
    d = zodb_json_codec.decode_zodb_record(rec)
    assert d["@s"]["b"] == {"x": 1, "y": [1, 2]}
    assert load_state(zodb_json_codec.encode_zodb_record(d)) == EXPECTED
    _, _, js, _ = zodb_json_codec.decode_zodb_record_for_pg_json(rec)
    assert json.loads(js)["l"] == EXPECTED["l"]


def test_prescan_memo_shared_across_record_pickles():
    # ZODB shares the memo between the class and the state pickle: "C" is memo
    # index 1 of the class pickle and is read from the state pickle by BINGET 1
    class_pickle = pickle.dumps(("m", "C"), protocol=3)
    assert class_pickle[16:18] == b"q\x01"  # BINPUT 1 right after "C"
    state_pickle = b"\x80\x03}q\x03X\x01\x00\x00\x00kq\x04h\x01s."  # {"k": memo[1]}
    d = zodb_json_codec.decode_zodb_record(class_pickle + state_pickle)
    assert d["@s"] == {"k": "C"}


def test_frame_and_memoize_stream():
    data = pickle.dumps({"k" * 300: [shared_state()] * 2}, protocol=5)
    assert b"\x95" in data  # FRAME present
    assert through_json(data) == {"k" * 300: [EXPECTED, EXPECTED]}

"""A GET of a memo index that was never put is an error, as in CPython (#47)."""

import pickle
import pickletools

import pytest

import zodb_json_codec

# PROTO 3, BININT1 1, BINPUT 4, BINGET 4, BINGET 0, TUPLE2, STOP: index 0 was never put
GAP = b"\x80\x03K\x01q\x04h\x04h\x00\x86."
# the two pickles of a ZODB record share one memo, so the class pickle must not
# put anything for index 0 to stay unset in the state pickle
CLASS_PICKLE = pickletools.optimize(
    pickle.dumps(("persistent.mapping", "PersistentMapping"), protocol=3)
)


def test_cpython_rejects_the_stream():
    with pytest.raises(pickle.UnpicklingError, match="Memo value not found at index 0"):
        pickle.loads(GAP)


@pytest.mark.parametrize(
    "call",
    [
        zodb_json_codec.pickle_to_dict,
        zodb_json_codec.pickle_to_json,
        lambda data: zodb_json_codec.decode_zodb_record(CLASS_PICKLE + data),
        lambda data: zodb_json_codec.decode_zodb_record_for_pg_json(
            CLASS_PICKLE + data
        ),
    ],
)
def test_codec_rejects_the_stream(call):
    with pytest.raises(ValueError, match="memo index 0 not found"):
        call(GAP)


def test_gap_put_without_a_read_of_the_gap_is_fine():
    # BININT1 1, BINPUT 0, BININT1 2, BINPUT 4, BINGET 4: index 0 put, never read
    assert zodb_json_codec.pickle_to_dict(b"\x80\x03K\x01q\x00K\x02q\x04h\x04.") == 2


def test_record_memo_is_shared_between_the_two_pickles():
    # the unoptimized class pickle puts its strings at 0..2: GET 0 in the state is the module name
    rec = pickle.dumps(("persistent.mapping", "PersistentMapping"), protocol=3) + GAP
    assert zodb_json_codec.decode_zodb_record(rec)["@s"] == {"@t": [1, "persistent.mapping"]}

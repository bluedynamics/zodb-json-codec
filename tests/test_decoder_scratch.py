"""The decoder reuses its vectors across records on a thread; an error must not leak state (#26)."""

import pickle
import pytest
import zodb_json_codec


def test_scratch_reset_after_error():
    good = pickle.dumps(("m", "C"), protocol=3) + pickle.dumps(
        {"a": [1, 2], "b": "x"}, protocol=3
    )
    truncated = good[:-6]
    with pytest.raises(ValueError):
        zodb_json_codec.decode_zodb_record(truncated)
    assert zodb_json_codec.decode_zodb_record(good)["@s"] == {"a": [1, 2], "b": "x"}
    # a MARK left open by the failure must not survive either
    open_mark = pickle.dumps(("m", "C"), protocol=3) + b"\x80\x03(K\x01K\x02"
    with pytest.raises(ValueError):
        zodb_json_codec.decode_zodb_record(open_mark)
    assert zodb_json_codec.decode_zodb_record(good)["@s"]["a"] == [1, 2]


def test_many_records_same_thread_stay_correct():
    records = [
        pickle.dumps(("m", "C"), protocol=3)
        + pickle.dumps({f"k{j}": [j, {"n": j}] for j in range(i % 50)}, protocol=3)
        for i in range(300)
    ]
    for i, rec in enumerate(records):
        d = zodb_json_codec.decode_zodb_record(rec)
        assert len(d["@s"]) == i % 50

"""POP_MARK and POP at a mark, as picklers write them for recursive tuples (#49).

POP_MARK: any recursive tuple at protocol 1, four or more elements at every binary
protocol; POP: protocol 0, and short tuples at protocol 2 and up.
"""

import pickle
import pickletools

import pytest

import zodb_json_codec


def opcodes(data):
    return [op.name for op, _, _ in pickletools.genops(data)]


def recursive_tuple():
    t = ([],)
    t[0].append(t)
    return t


@pytest.mark.parametrize(("protocol", "opcode"), [(0, "POP"), (1, "POP_MARK")])
def test_recursive_tuple_streams_decode(protocol, opcode):
    data = pickle.dumps(recursive_tuple(), protocol=protocol)
    assert opcode in opcodes(data)
    # The codec cannot represent the cycle: the GET that closes it sees the
    # memoized tuple as it was, a tuple holding the list before the append.
    # What matters here is that the mark opcodes decode like CPython's.
    assert zodb_json_codec.pickle_to_dict(data) == {"@t": [[]]}
    back = pickle.loads(
        zodb_json_codec.json_to_pickle(zodb_json_codec.pickle_to_json(data))
    )
    assert back == ([],)


def test_protocol_2_recursive_tuple_is_unchanged():
    data = pickle.dumps(recursive_tuple(), protocol=2)
    assert zodb_json_codec.pickle_to_dict(data) == {"@t": [[]]}


def test_pop_mark_without_mark_and_pop_on_empty_stack_raise():
    with pytest.raises(ValueError):
        zodb_json_codec.pickle_to_dict(b"\x80\x02K\x011.")
    with pytest.raises(ValueError):
        zodb_json_codec.pickle_to_dict(b"\x80\x020K\x01.")


def recursive_four_tuple():
    t = ([], 11, 12, 13)
    t[0].append(t)
    return t


@pytest.mark.parametrize("protocol", [1, 2, 3])
def test_four_element_recursive_tuple_uses_pop_mark_at_every_binary_protocol(protocol):
    data = pickle.dumps(recursive_four_tuple(), protocol=protocol)
    assert "POP_MARK" in opcodes(data)
    assert zodb_json_codec.pickle_to_dict(data) == {"@t": [[], 11, 12, 13]}
    back = pickle.loads(
        zodb_json_codec.json_to_pickle(zodb_json_codec.pickle_to_json(data))
    )
    assert back == ([], 11, 12, 13)


def test_pop_mark_flushes_memo_bindings_of_the_discarded_slots():
    # the list bound to a memo index is discarded by POP_MARK and read afterwards:
    # the memo must hold its live value (with the tuple appended), not the empty copy
    t = recursive_four_tuple()
    data = pickle.dumps([t, t[0]], protocol=3)
    assert "POP_MARK" in opcodes(data)
    snap = {"@t": [[], 11, 12, 13]}
    assert zodb_json_codec.pickle_to_dict(data) == [snap, [snap]]


def test_protocol_3_record_with_a_recursive_tuple():
    t = recursive_four_tuple()
    cls = pickle.dumps(("persistent.mapping", "PersistentMapping"), protocol=3)
    rec = cls + pickletools.optimize(pickle.dumps({"data": {"t": t}}, protocol=3))
    assert "POP_MARK" in opcodes(rec[len(cls) :])
    assert zodb_json_codec.decode_zodb_record(rec)["@s"] == {
        "data": {"t": {"@t": [[], 11, 12, 13]}}
    }

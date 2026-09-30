"""POP_MARK and POP at a mark, as protocol 0 and 1 picklers write them for recursive tuples (#49)."""

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

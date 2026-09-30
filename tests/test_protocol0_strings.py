"""Protocol 0 text opcodes: STRING is a bytes repr, UNICODE is raw-unicode-escape (#25)."""

import base64
import json
import pickle
import pytest
import zodb_json_codec


def roundtrip(obj, protocol):
    return pickle.loads(
        zodb_json_codec.json_to_pickle(
            zodb_json_codec.pickle_to_json(pickle.dumps(obj, protocol=protocol))
        )
    )


@pytest.mark.parametrize(
    "text",
    [
        "it's",
        'say "hi"',
        "back\\slash",
        "tab\tnew\nline",
        "é ☃ 😀",
        "\\u0041 stays literal",
        "nul\x00byte",
    ],
)
def test_unicode_protocol0(text):
    # the pickler writes V<raw-unicode-escape>\n: Latin-1 bytes plus \uXXXX
    assert roundtrip(text, 0) == text
    assert roundtrip({"k": text, "l": [text]}, 0) == {"k": text, "l": [text]}


def test_string_opcode_unescapes_repr():
    # S'...' as Python 2 wrote it; protocol 0 STRING stays bytes in the codec
    data = b"S'it\\'s a \\x00\\xff \\101\\n'\np0\n."
    expected = b"it's a \x00\xff A\n"
    js = zodb_json_codec.pickle_to_json(data)
    assert json.loads(js) == {"@b": base64.b64encode(expected).decode()}
    assert pickle.loads(zodb_json_codec.json_to_pickle(js)) == expected


@pytest.mark.parametrize(
    "data, message",
    [
        (b"Sunquoted\n.", "quoted"),
        (b"S'trailing\\'\n.", "backslash"),
        (b"S'\\xZZ'\n.", "escape"),
        (b"V\\u12\n.", "escape"),
    ],
)
def test_malformed_protocol0_text_raises(data, message):
    with pytest.raises(ValueError, match=message):
        zodb_json_codec.pickle_to_json(data)

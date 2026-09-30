"""Escapes at every offset of long strings survive the PG JSON writer (#52)."""

import json
import pickle

import pytest

import zodb_json_codec

ESCAPES = ['"', "\\", "\n", "\r", "\t", "\x01", "\x02", "\x1f", "\x7f"]


def record(value):
    cls = pickle.dumps(("persistent.mapping", "PersistentMapping"), protocol=3)
    return cls + pickle.dumps({"data": {"v": value}}, protocol=3)


def pg_value(value):
    _, _, js, _ = zodb_json_codec.decode_zodb_record_for_pg_json(record(value))
    return json.loads(js)["data"]["v"]


@pytest.mark.parametrize("esc", ESCAPES)
@pytest.mark.parametrize("filler", ["a", "é", "€", "\U0001f600"])
def test_escape_at_every_offset(esc, filler):
    for length in range(40):
        for pos in range(length + 1):
            value = filler * pos + esc + filler * (length - pos)
            assert pg_value(value) == value


def test_long_clean_string_is_copied_verbatim():
    value = "abcdefgh" * 100 + "€" * 100
    _, _, js, _ = zodb_json_codec.decode_zodb_record_for_pg_json(record(value))
    assert json.loads(js)["data"]["v"] == value
    assert "\\" not in js

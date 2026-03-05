"""Fast pickle <-> JSON transcoder for ZODB, implemented in Rust."""

from zodb_json_codec._rust import decode_zodb_record
from zodb_json_codec._rust import decode_zodb_record_for_pg
from zodb_json_codec._rust import decode_zodb_record_for_pg_json
from zodb_json_codec._rust import dict_to_pickle
from zodb_json_codec._rust import encode_zodb_record
from zodb_json_codec._rust import json_to_pickle
from zodb_json_codec._rust import pickle_to_dict
from zodb_json_codec._rust import pickle_to_json


__all__ = [
    "decode_zodb_record",
    "decode_zodb_record_for_pg",
    "decode_zodb_record_for_pg_json",
    "dict_to_pickle",
    "encode_zodb_record",
    "json_to_pickle",
    "pickle_to_dict",
    "pickle_to_json",
]

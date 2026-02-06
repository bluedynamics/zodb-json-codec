"""Fast pickle <-> JSON transcoder for ZODB, implemented in Rust."""

from zodb_json_codec._rust import (
    decode_zodb_record,
    encode_zodb_record,
    pickle_to_dict,
    pickle_to_json,
    json_to_pickle,
    dict_to_pickle,
)

__all__ = [
    "pickle_to_json",
    "json_to_pickle",
    "pickle_to_dict",
    "dict_to_pickle",
    "decode_zodb_record",
    "encode_zodb_record",
]

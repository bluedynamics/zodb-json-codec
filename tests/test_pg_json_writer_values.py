"""The direct PG JSON writer agrees with the dict path on refs, bytes and NUL strings (#26)."""

import json
import pickle
import zodb_json_codec


class Ref:
    """Persistent-reference stand-in: pickled through a persistent_id hook."""

    def __init__(self, oid, cls=None):
        self.oid, self.cls = oid, cls


def dumps_with_refs(obj):
    import io

    buf = io.BytesIO()
    p = pickle.Pickler(buf, protocol=3)

    def persistent_id(o):
        if isinstance(o, Ref):
            return (o.oid, o.cls) if o.cls is not None else (o.oid, None)
        return None

    p.persistent_id = persistent_id
    p.dump(obj)
    return buf.getvalue()


class SomeClass:
    pass


def test_writer_matches_dict_path_on_refs_bytes_and_nul():
    state = {
        "plain": Ref(b"\x00\x00\x00\x00\x00\x00\x00\x03"),
        "typed": Ref(b"\x00\x00\x00\x00\x00\x00\x00\x2a", SomeClass),
        "blob": b"\x00\xff\xfe binary",
        "nul": "a\x00b",
    }
    record = pickle.dumps(("m", "C"), protocol=3) + dumps_with_refs(state)
    mod, name, js, refs = zodb_json_codec.decode_zodb_record_for_pg_json(record)
    _, _, dict_state, dict_refs = zodb_json_codec.decode_zodb_record_for_pg(record)
    assert json.loads(js) == dict_state
    assert refs == dict_refs
    parsed = json.loads(js)
    assert parsed["plain"] == {"@ref": "0000000000000003"}
    assert parsed["typed"] == {"@ref": ["000000000000002a", f"{__name__}.SomeClass"]}
    assert parsed["nul"] == {"@ns": "YQBi"}
    assert parsed["blob"] == {"@b": "AP/+IGJpbmFyeQ=="}


def test_writer_nul_key():
    # the dict PG path cannot represent this key before #18 lands; the JSON writer can
    record = pickle.dumps(("m", "C"), protocol=3) + pickle.dumps(
        {"k\x00ey": 1}, protocol=3
    )
    _, _, js, _ = zodb_json_codec.decode_zodb_record_for_pg_json(record)
    assert json.loads(js) == {"@ns:awBleQ==": 1}

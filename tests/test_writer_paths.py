"""Pins for the writer and encoder paths touched by #23 (buffer reuse, escaping, itoa, class cache)."""

import io
import json
import pickle
import zodb_json_codec


def two_pickles(cls_tuple, state):
    return pickle.dumps(cls_tuple, protocol=3) + pickle.dumps(state, protocol=3)


def load_record(record):
    u = pickle.Unpickler(io.BytesIO(record))
    return u.load(), u.load()


RICH_TEXT = "".join(
    f'<p class="p{i}">Zürich &amp; 日本語, line {i}\twith "quotes" and a backslash \\ and \x1f</p>\n'
    for i in range(400)
)


class TestJsonWriter:
    def test_escaped_text_roundtrip_pg_json(self):
        state = {"text": RICH_TEXT, "short": "a\nb"}
        mod, name, js, _ = zodb_json_codec.decode_zodb_record_for_pg_json(
            two_pickles(("m", "C"), state)
        )
        assert json.loads(js) == state
        _, _, st, _ = zodb_json_codec.decode_zodb_record_for_pg(
            two_pickles(("m", "C"), state)
        )
        assert st == json.loads(js)
        back = zodb_json_codec.encode_zodb_record(
            {"@cls": [mod, name], "@s": json.loads(js)}
        )
        assert load_record(back)[1] == state

    def test_i64_boundaries_in_pg_json(self):
        state = {"lo": -(2**63), "hi": 2**63 - 1, "zero": 0, "neg": -1, "big": 2**70}
        _, _, js, _ = zodb_json_codec.decode_zodb_record_for_pg_json(
            two_pickles(("m", "C"), state)
        )
        loaded = json.loads(js)
        assert {k: loaded[k] for k in ("lo", "hi", "zero", "neg")} == {
            k: state[k] for k in ("lo", "hi", "zero", "neg")
        }
        assert loaded["big"] == {"@bi": str(2**70)}

    def test_reuse_does_not_leak(self):
        big = {"text": RICH_TEXT, "n": list(range(1000))}
        small = {"x": 1}
        big_rec, small_rec = (
            two_pickles(("m", "C"), big),
            two_pickles(("m", "C"), small),
        )
        # decode: large then small on the same thread, compared with a fresh small decode
        zodb_json_codec.decode_zodb_record_for_pg_json(big_rec)
        after = zodb_json_codec.decode_zodb_record_for_pg_json(small_rec)
        assert after == zodb_json_codec.decode_zodb_record_for_pg_json(small_rec)
        assert json.loads(after[2]) == small
        # encode: large then small
        zodb_json_codec.encode_zodb_record({"@cls": ["m", "C"], "@s": big})
        out = zodb_json_codec.encode_zodb_record({"@cls": ["m", "C"], "@s": small})
        assert out == zodb_json_codec.encode_zodb_record(
            {"@cls": ["m", "C"], "@s": small}
        )
        assert load_record(out)[1] == small


class TestClassPickleCache:
    def test_class_pickle_cache_eviction(self):
        # more distinct classes than the cache holds, round-robin twice: every record
        # must carry its own class pickle
        classes = [(f"pkg.mod{i}", f"Class{i}") for i in range(80)]
        for _ in range(2):
            for mod, name in classes:
                rec = zodb_json_codec.encode_zodb_record(
                    {"@cls": [mod, name], "@s": {"i": name}}
                )
                cls, state = load_record(rec)
                assert cls == ((mod, name), None)
                assert state == {"i": name}
        # a hot class interleaved with misses stays correct
        for i in range(200):
            mod, name = classes[i % 80] if i % 2 else classes[0]
            assert load_record(
                zodb_json_codec.encode_zodb_record({"@cls": [mod, name], "@s": None})
            )[0] == ((mod, name), None)

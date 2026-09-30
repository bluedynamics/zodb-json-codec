"""NUL bytes survive the PostgreSQL round trip through @ns markers (#18)."""

import base64
import io
import json
import pickle
import pytest
import zodb_json_codec


NUL_VAL = "a\x00b"
NUL_KEY = "k\x00ey"


def two_pickles(cls_tuple, state):
    return pickle.dumps(cls_tuple, protocol=3) + pickle.dumps(state, protocol=3)


def load_state(record):
    u = pickle.Unpickler(io.BytesIO(record))
    u.load()
    return u.load()


def through_pg_json(cls, state):
    mod, name, js, _ = zodb_json_codec.decode_zodb_record_for_pg_json(
        two_pickles(cls, state)
    )
    back = zodb_json_codec.encode_zodb_record(
        {"@cls": [mod, name], "@s": json.loads(js)}
    )
    return json.loads(js), load_state(back)


def through_pg_dict(cls, state):
    mod, name, st, _ = zodb_json_codec.decode_zodb_record_for_pg(
        two_pickles(cls, state)
    )
    back = zodb_json_codec.encode_zodb_record({"@cls": [mod, name], "@s": st})
    return st, load_state(back)


PATHS = [through_pg_json, through_pg_dict]
CLS = ("m", "C")


class TestValuesAndKeys:
    @pytest.mark.parametrize("path", PATHS)
    def test_nul_value_roundtrip(self, path):
        js, back = path(CLS, {"v": NUL_VAL})
        assert js["v"] == {"@ns": base64.b64encode(NUL_VAL.encode()).decode()}
        assert back == {"v": NUL_VAL}

    @pytest.mark.parametrize("path", PATHS)
    def test_nul_key_roundtrip(self, path):
        js, back = path(CLS, {NUL_KEY: 1})
        assert list(js) == ["@ns:" + base64.b64encode(NUL_KEY.encode()).decode()]
        assert back == {NUL_KEY: 1}

    @pytest.mark.parametrize("path", PATHS)
    def test_nul_key_nested(self, path):
        _, back = path(CLS, {"outer": {NUL_KEY: {NUL_KEY: NUL_VAL}}})
        assert back == {"outer": {NUL_KEY: {NUL_KEY: NUL_VAL}}}

    @pytest.mark.parametrize("path", PATHS)
    def test_nul_in_mixed_key_dict(self, path):
        _, back = path(CLS, {"d": {1: "x", NUL_KEY: NUL_VAL}})
        assert back == {"d": {1: "x", NUL_KEY: NUL_VAL}}

    @pytest.mark.parametrize("path", PATHS)
    def test_nul_inside_tuple_and_set(self, path):
        _, back = path(CLS, {"t": (NUL_VAL, 1), "s": {NUL_VAL}})
        assert back == {"t": (NUL_VAL, 1), "s": {NUL_VAL}}

    def test_nul_in_btree_bucket_key(self):
        state = ((NUL_KEY, 1, "k2", NUL_VAL),)
        _, back = through_pg_json(("BTrees.OOBTree", "OOBucket"), state)
        assert back == state

    def test_single_key_dict_with_nul_key(self):
        # exercises the single-key fast paths of both encoders
        _, back = through_pg_json(CLS, {"d": {NUL_KEY: 1}})
        assert back == {"d": {NUL_KEY: 1}}


class TestStandaloneJsonApi:
    def test_json_to_pickle_restores_markers(self):
        js = json.dumps(
            {
                "v": {"@ns": base64.b64encode(NUL_VAL.encode()).decode()},
                "@ns:" + base64.b64encode(NUL_KEY.encode()).decode(): 1,
            }
        )
        assert pickle.loads(zodb_json_codec.json_to_pickle(js)) == {
            "v": NUL_VAL,
            NUL_KEY: 1,
        }


class TestNonPgPaths:
    def test_non_pg_paths_keep_raw_nul(self):
        rec = two_pickles(CLS, {NUL_KEY: NUL_VAL})
        assert zodb_json_codec.decode_zodb_record(rec)["@s"] == {NUL_KEY: NUL_VAL}
        raw = pickle.dumps({NUL_KEY: NUL_VAL}, protocol=3)
        assert zodb_json_codec.pickle_to_dict(raw) == {NUL_KEY: NUL_VAL}


class TestMalformed:
    @pytest.mark.parametrize(
        "state",
        [
            {"v": {"@ns": "not base64!"}},
            {"v": {"@ns": base64.b64encode(b"\xff\xfe").decode()}},
            {"@ns:not base64!": 1},
            {"@ns:" + base64.b64encode(b"\xff").decode(): 1},
        ],
    )
    def test_malformed_ns_markers(self, state):
        with pytest.raises(ValueError, match="@ns"):
            zodb_json_codec.encode_zodb_record({"@cls": ["m", "C"], "@s": state})

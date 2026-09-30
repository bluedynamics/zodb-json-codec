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

    @pytest.mark.parametrize("path", PATHS)
    def test_nul_in_btree_bucket_key(self, path):
        state = ((NUL_KEY, 1, "k2", NUL_VAL),)
        _, back = path(("BTrees.OOBTree", "OOBucket"), state)
        assert back == state

    @pytest.mark.parametrize("path", PATHS)
    def test_single_key_dict_with_nul_key(self, path):
        # single-key dict: the direct encoder defers to the PickleValue path here
        _, back = path(CLS, {"d": {NUL_KEY: 1}})
        assert back == {"d": {NUL_KEY: 1}}

    @pytest.mark.parametrize("path", PATHS)
    @pytest.mark.parametrize(
        "d",
        [
            {NUL_KEY: 1, "b": 2, "c": 3},  # 3 keys, NUL first: 2-4 key scan
            {"a": 1, "b": 2, NUL_KEY: 3},  # 3 keys, NUL last
            {NUL_KEY: 0, "a": 1, "b": 2, "c": 3, "d": 4, "e": 5},  # 6 keys: plain path
            {"a": 1, "b": 2, "c": 3, "d": 4, "e": 5, NUL_KEY: 6},
        ],
    )
    def test_nul_key_in_multi_key_dicts(self, path, d):
        _, back = path(CLS, d)
        assert back == d
        # nested inside a @d value reaches pydict_to_pickle_value's scan
        _, back = path(CLS, {"outer": {1: d}})
        assert back == {"outer": {1: d}}


class TestGenuineNsPrefixedKeys:
    """Keys that happen to start with "@ns:" are user data and must survive every path."""

    GENUINE = {"@ns:YWJj": 1, "@ns:": 2, "@ns:not base64!": 3}

    @pytest.mark.parametrize("path", PATHS)
    def test_pg_paths_escape_genuine_prefix(self, path):
        _, back = path(CLS, dict(self.GENUINE))
        assert back == self.GENUINE

    def test_non_pg_record_path(self):
        rec = two_pickles(CLS, dict(self.GENUINE))
        back = load_state(
            zodb_json_codec.encode_zodb_record(zodb_json_codec.decode_zodb_record(rec))
        )
        assert back == self.GENUINE

    def test_json_string_api(self):
        raw = pickle.dumps(dict(self.GENUINE), protocol=3)
        assert (
            pickle.loads(
                zodb_json_codec.json_to_pickle(zodb_json_codec.pickle_to_json(raw))
            )
            == self.GENUINE
        )


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

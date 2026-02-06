"""Test known type handlers: Python types that get compact JSON markers.

These types are commonly stored as inline values in ZODB object state dicts.
Instead of generic @reduce JSON, they get human-readable, queryable forms.
"""

import json
import pickle
from datetime import date, datetime, time, timedelta, timezone
from decimal import Decimal
import uuid

import pytest

import zodb_json_codec


class TestDatetime:
    def test_naive(self):
        dt = datetime(2025, 6, 15, 12, 30, 45)
        data = pickle.dumps(dt, protocol=3)
        result = json.loads(zodb_json_codec.pickle_to_json(data))
        assert result == {"@dt": "2025-06-15T12:30:45"}

    def test_with_microseconds(self):
        dt = datetime(2025, 6, 15, 12, 30, 45, 123456)
        data = pickle.dumps(dt, protocol=3)
        result = json.loads(zodb_json_codec.pickle_to_json(data))
        assert result == {"@dt": "2025-06-15T12:30:45.123456"}

    def test_roundtrip_naive(self):
        dt = datetime(2025, 6, 15, 12, 30, 45)
        data = pickle.dumps(dt, protocol=3)
        json_str = zodb_json_codec.pickle_to_json(data)
        restored = pickle.loads(zodb_json_codec.json_to_pickle(json_str))
        assert restored == dt

    def test_roundtrip_with_microseconds(self):
        dt = datetime(2025, 6, 15, 12, 30, 45, 123456)
        data = pickle.dumps(dt, protocol=3)
        json_str = zodb_json_codec.pickle_to_json(data)
        restored = pickle.loads(zodb_json_codec.json_to_pickle(json_str))
        assert restored == dt

    @pytest.mark.parametrize(
        "year",
        [1, 100, 255, 256, 1000, 2025, 9999],
    )
    def test_year_boundaries(self, year):
        dt = datetime(year, 1, 1)
        data = pickle.dumps(dt, protocol=3)
        json_str = zodb_json_codec.pickle_to_json(data)
        restored = pickle.loads(zodb_json_codec.json_to_pickle(json_str))
        assert restored == dt

    def test_tz_stdlib_utc(self):
        dt = datetime(2025, 1, 1, tzinfo=timezone.utc)
        data = pickle.dumps(dt, protocol=3)
        result = json.loads(zodb_json_codec.pickle_to_json(data))
        assert result == {"@dt": "2025-01-01T00:00:00+00:00"}

    def test_roundtrip_tz_stdlib_utc(self):
        dt = datetime(2025, 1, 1, tzinfo=timezone.utc)
        data = pickle.dumps(dt, protocol=3)
        json_str = zodb_json_codec.pickle_to_json(data)
        restored = pickle.loads(zodb_json_codec.json_to_pickle(json_str))
        assert restored == dt

    def test_tz_stdlib_offset(self):
        tz = timezone(timedelta(hours=5, minutes=30))
        dt = datetime(2025, 1, 1, tzinfo=tz)
        data = pickle.dumps(dt, protocol=3)
        result = json.loads(zodb_json_codec.pickle_to_json(data))
        assert result == {"@dt": "2025-01-01T00:00:00+05:30"}

    def test_roundtrip_tz_stdlib_offset(self):
        tz = timezone(timedelta(hours=5, minutes=30))
        dt = datetime(2025, 1, 1, tzinfo=tz)
        data = pickle.dumps(dt, protocol=3)
        json_str = zodb_json_codec.pickle_to_json(data)
        restored = pickle.loads(zodb_json_codec.json_to_pickle(json_str))
        assert restored == dt

    def test_tz_negative_offset(self):
        tz = timezone(timedelta(hours=-5))
        dt = datetime(2025, 1, 1, tzinfo=tz)
        data = pickle.dumps(dt, protocol=3)
        result = json.loads(zodb_json_codec.pickle_to_json(data))
        assert result == {"@dt": "2025-01-01T00:00:00-05:00"}

    def test_roundtrip_tz_negative_offset(self):
        tz = timezone(timedelta(hours=-5))
        dt = datetime(2025, 1, 1, tzinfo=tz)
        data = pickle.dumps(dt, protocol=3)
        json_str = zodb_json_codec.pickle_to_json(data)
        restored = pickle.loads(zodb_json_codec.json_to_pickle(json_str))
        assert restored == dt

    def test_tz_pytz_utc(self):
        import pytz

        dt = datetime(2025, 1, 1, tzinfo=pytz.utc)
        data = pickle.dumps(dt, protocol=3)
        result = json.loads(zodb_json_codec.pickle_to_json(data))
        assert result == {"@dt": "2025-01-01T00:00:00+00:00"}

    def test_roundtrip_tz_pytz_utc(self):
        import pytz

        dt = datetime(2025, 1, 1, tzinfo=pytz.utc)
        data = pickle.dumps(dt, protocol=3)
        json_str = zodb_json_codec.pickle_to_json(data)
        restored = pickle.loads(zodb_json_codec.json_to_pickle(json_str))
        # pytz.utc roundtrips to stdlib timezone.utc (both represent UTC)
        assert restored == dt

    def test_tz_pytz_named(self):
        import pytz

        tz = pytz.timezone("US/Eastern")
        dt = tz.localize(datetime(2025, 1, 1))
        data = pickle.dumps(dt, protocol=3)
        result = json.loads(zodb_json_codec.pickle_to_json(data))
        assert result["@dt"] == "2025-01-01T00:00:00"
        assert "@tz" in result
        assert result["@tz"]["name"] == "US/Eastern"

    def test_roundtrip_tz_pytz_named(self):
        import pytz

        tz = pytz.timezone("US/Eastern")
        dt = tz.localize(datetime(2025, 1, 1))
        data = pickle.dumps(dt, protocol=3)
        json_str = zodb_json_codec.pickle_to_json(data)
        restored = pickle.loads(zodb_json_codec.json_to_pickle(json_str))
        assert restored == dt

    def test_tz_zoneinfo(self):
        import zoneinfo

        dt = datetime(2025, 1, 1, tzinfo=zoneinfo.ZoneInfo("US/Eastern"))
        data = pickle.dumps(dt, protocol=3)
        result = json.loads(zodb_json_codec.pickle_to_json(data))
        assert result["@dt"] == "2025-01-01T00:00:00"
        assert "@tz" in result
        assert result["@tz"]["zoneinfo"] == "US/Eastern"

    def test_roundtrip_tz_zoneinfo(self):
        import zoneinfo

        dt = datetime(2025, 1, 1, tzinfo=zoneinfo.ZoneInfo("US/Eastern"))
        data = pickle.dumps(dt, protocol=3)
        json_str = zodb_json_codec.pickle_to_json(data)
        restored = pickle.loads(zodb_json_codec.json_to_pickle(json_str))
        assert restored == dt


class TestDate:
    def test_basic(self):
        d = date(2025, 6, 15)
        data = pickle.dumps(d, protocol=3)
        result = json.loads(zodb_json_codec.pickle_to_json(data))
        assert result == {"@date": "2025-06-15"}

    def test_roundtrip(self):
        d = date(2025, 6, 15)
        data = pickle.dumps(d, protocol=3)
        json_str = zodb_json_codec.pickle_to_json(data)
        restored = pickle.loads(zodb_json_codec.json_to_pickle(json_str))
        assert restored == d

    @pytest.mark.parametrize("year", [1, 100, 2025, 9999])
    def test_year_boundaries(self, year):
        d = date(year, 1, 1)
        data = pickle.dumps(d, protocol=3)
        json_str = zodb_json_codec.pickle_to_json(data)
        restored = pickle.loads(zodb_json_codec.json_to_pickle(json_str))
        assert restored == d


class TestTime:
    def test_basic(self):
        t = time(12, 30, 45)
        data = pickle.dumps(t, protocol=3)
        result = json.loads(zodb_json_codec.pickle_to_json(data))
        assert result == {"@time": "12:30:45"}

    def test_with_microseconds(self):
        t = time(12, 30, 45, 123456)
        data = pickle.dumps(t, protocol=3)
        result = json.loads(zodb_json_codec.pickle_to_json(data))
        assert result == {"@time": "12:30:45.123456"}

    def test_roundtrip(self):
        t = time(12, 30, 45)
        data = pickle.dumps(t, protocol=3)
        json_str = zodb_json_codec.pickle_to_json(data)
        restored = pickle.loads(zodb_json_codec.json_to_pickle(json_str))
        assert restored == t

    def test_roundtrip_with_microseconds(self):
        t = time(12, 30, 45, 123456)
        data = pickle.dumps(t, protocol=3)
        json_str = zodb_json_codec.pickle_to_json(data)
        restored = pickle.loads(zodb_json_codec.json_to_pickle(json_str))
        assert restored == t

    def test_midnight(self):
        t = time(0, 0, 0)
        data = pickle.dumps(t, protocol=3)
        json_str = zodb_json_codec.pickle_to_json(data)
        restored = pickle.loads(zodb_json_codec.json_to_pickle(json_str))
        assert restored == t


class TestTimedelta:
    def test_basic(self):
        td = timedelta(days=7, seconds=3600, microseconds=500000)
        data = pickle.dumps(td, protocol=3)
        result = json.loads(zodb_json_codec.pickle_to_json(data))
        assert result == {"@td": [7, 3600, 500000]}

    def test_roundtrip(self):
        td = timedelta(days=7, seconds=3600, microseconds=500000)
        data = pickle.dumps(td, protocol=3)
        json_str = zodb_json_codec.pickle_to_json(data)
        restored = pickle.loads(zodb_json_codec.json_to_pickle(json_str))
        assert restored == td

    def test_negative(self):
        td = timedelta(days=-1)
        data = pickle.dumps(td, protocol=3)
        json_str = zodb_json_codec.pickle_to_json(data)
        restored = pickle.loads(zodb_json_codec.json_to_pickle(json_str))
        assert restored == td

    def test_zero(self):
        td = timedelta()
        data = pickle.dumps(td, protocol=3)
        json_str = zodb_json_codec.pickle_to_json(data)
        restored = pickle.loads(zodb_json_codec.json_to_pickle(json_str))
        assert restored == td


class TestDecimal:
    @pytest.mark.parametrize(
        "val",
        ["3.14159", "0", "-1.5", "1E+10", "Infinity", "-Infinity"],
    )
    def test_format(self, val):
        d = Decimal(val)
        data = pickle.dumps(d, protocol=3)
        result = json.loads(zodb_json_codec.pickle_to_json(data))
        assert result == {"@dec": val}

    @pytest.mark.parametrize(
        "val",
        ["3.14159", "0", "-1.5", "1E+10", "Infinity", "-Infinity"],
    )
    def test_roundtrip(self, val):
        d = Decimal(val)
        data = pickle.dumps(d, protocol=3)
        json_str = zodb_json_codec.pickle_to_json(data)
        restored = pickle.loads(zodb_json_codec.json_to_pickle(json_str))
        assert restored == d

    def test_nan_roundtrip(self):
        d = Decimal("NaN")
        data = pickle.dumps(d, protocol=3)
        json_str = zodb_json_codec.pickle_to_json(data)
        restored = pickle.loads(zodb_json_codec.json_to_pickle(json_str))
        assert restored.is_nan()


class TestUUID:
    def test_format(self):
        u = uuid.UUID("12345678-1234-5678-1234-567812345678")
        data = pickle.dumps(u, protocol=3)
        result = json.loads(zodb_json_codec.pickle_to_json(data))
        assert result == {"@uuid": "12345678-1234-5678-1234-567812345678"}

    def test_roundtrip(self):
        u = uuid.UUID("12345678-1234-5678-1234-567812345678")
        data = pickle.dumps(u, protocol=3)
        json_str = zodb_json_codec.pickle_to_json(data)
        restored = pickle.loads(zodb_json_codec.json_to_pickle(json_str))
        assert restored == u

    def test_random_uuid_roundtrip(self):
        u = uuid.uuid4()
        data = pickle.dumps(u, protocol=3)
        json_str = zodb_json_codec.pickle_to_json(data)
        restored = pickle.loads(zodb_json_codec.json_to_pickle(json_str))
        assert restored == u

    def test_max_uuid(self):
        """UUID with all bits set (tests BigInt handling)."""
        u = uuid.UUID("ffffffff-ffff-ffff-ffff-ffffffffffff")
        data = pickle.dumps(u, protocol=3)
        json_str = zodb_json_codec.pickle_to_json(data)
        result = json.loads(json_str)
        assert result == {"@uuid": "ffffffff-ffff-ffff-ffff-ffffffffffff"}
        restored = pickle.loads(zodb_json_codec.json_to_pickle(json_str))
        assert restored == u

    def test_nil_uuid(self):
        u = uuid.UUID("00000000-0000-0000-0000-000000000000")
        data = pickle.dumps(u, protocol=3)
        json_str = zodb_json_codec.pickle_to_json(data)
        restored = pickle.loads(zodb_json_codec.json_to_pickle(json_str))
        assert restored == u


class TestSet:
    def test_format(self):
        """Protocol 3 sets use REDUCE, should decode to @set."""
        s = {1, 2, 3}
        data = pickle.dumps(s, protocol=3)
        result = json.loads(zodb_json_codec.pickle_to_json(data))
        assert "@set" in result
        assert sorted(result["@set"]) == [1, 2, 3]

    def test_roundtrip(self):
        s = {1, 2, 3}
        data = pickle.dumps(s, protocol=3)
        json_str = zodb_json_codec.pickle_to_json(data)
        restored = pickle.loads(zodb_json_codec.json_to_pickle(json_str))
        assert restored == s

    def test_empty_set(self):
        s = set()
        data = pickle.dumps(s, protocol=3)
        json_str = zodb_json_codec.pickle_to_json(data)
        result = json.loads(json_str)
        assert "@set" in result
        assert result["@set"] == []


class TestFrozenset:
    def test_format(self):
        s = frozenset([1, 2, 3])
        data = pickle.dumps(s, protocol=3)
        result = json.loads(zodb_json_codec.pickle_to_json(data))
        assert "@fset" in result
        assert sorted(result["@fset"]) == [1, 2, 3]

    def test_roundtrip(self):
        s = frozenset([1, 2, 3])
        data = pickle.dumps(s, protocol=3)
        json_str = zodb_json_codec.pickle_to_json(data)
        restored = pickle.loads(zodb_json_codec.json_to_pickle(json_str))
        assert restored == s

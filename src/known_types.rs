//! Known type handlers: convert common Python types between PickleValue and typed JSON.
//!
//! Instead of representing well-known types like datetime.datetime as generic
//! `@reduce` JSON, we use compact typed markers (`@dt`, `@date`, `@dec`, etc.)
//! that are human-readable and queryable in PostgreSQL JSONB.

use serde_json::{json, Map, Value};

use crate::error::CodecError;
use crate::types::PickleValue;

// ---------------------------------------------------------------------------
// Forward direction: PickleValue → typed JSON
// ---------------------------------------------------------------------------

/// Try to convert a known REDUCE pattern to compact typed JSON.
/// Returns Ok(None) if the callable is not recognized.
pub fn try_reduce_to_typed_json(
    callable: &PickleValue,
    args: &PickleValue,
    to_json: &dyn Fn(&PickleValue) -> Result<Value, CodecError>,
) -> Result<Option<Value>, CodecError> {
    let (module, name) = match callable {
        PickleValue::Global { module, name } => (module.as_str(), name.as_str()),
        _ => return Ok(None),
    };

    match (module, name) {
        ("datetime", "datetime") => try_encode_datetime(args, to_json),
        ("datetime", "date") => try_encode_date(args),
        ("datetime", "time") => try_encode_time(args, to_json),
        ("datetime", "timedelta") => try_encode_timedelta(args),
        ("decimal", "Decimal") => try_encode_decimal(args),
        ("builtins", "set") => try_encode_set(args, to_json),
        ("builtins", "frozenset") => try_encode_frozenset(args, to_json),
        _ => Ok(None),
    }
}

/// Try to convert a known Instance (NEWOBJ+BUILD) pattern to compact typed JSON.
/// Returns Ok(None) if the class is not recognized.
pub fn try_instance_to_typed_json(
    module: &str,
    name: &str,
    state: &PickleValue,
    _to_json: &dyn Fn(&PickleValue) -> Result<Value, CodecError>,
) -> Result<Option<Value>, CodecError> {
    match (module, name) {
        ("uuid", "UUID") => try_encode_uuid(state),
        _ => Ok(None),
    }
}

// ---------------------------------------------------------------------------
// Reverse direction: typed JSON → PickleValue
// ---------------------------------------------------------------------------

/// Try to convert typed JSON markers back to PickleValue.
/// Returns Ok(None) if no known marker is found.
pub fn try_typed_json_to_pickle_value(
    map: &Map<String, Value>,
    from_json: &dyn Fn(&Value) -> Result<PickleValue, CodecError>,
) -> Result<Option<PickleValue>, CodecError> {
    if let Some(v) = map.get("@dt") {
        return try_decode_datetime(v, map.get("@tz"), from_json).map(Some);
    }
    if let Some(v) = map.get("@date") {
        return try_decode_date(v).map(Some);
    }
    if let Some(v) = map.get("@time") {
        return try_decode_time(v, map.get("@tz")).map(Some);
    }
    if let Some(v) = map.get("@td") {
        return try_decode_timedelta(v).map(Some);
    }
    if let Some(v) = map.get("@dec") {
        return try_decode_decimal(v).map(Some);
    }
    if let Some(v) = map.get("@uuid") {
        return try_decode_uuid(v).map(Some);
    }
    Ok(None)
}

// ===========================================================================
// datetime.datetime
// ===========================================================================

/// Decode 10-byte datetime binary: (year_hi, year_lo, month, day, hour, min, sec, us_hi, us_mid, us_lo)
pub fn decode_datetime_bytes(b: &[u8]) -> Option<(u16, u8, u8, u8, u8, u8, u32)> {
    if b.len() != 10 {
        return None;
    }
    let year = (b[0] as u16) * 256 + b[1] as u16;
    let month = b[2];
    let day = b[3];
    let hour = b[4];
    let minute = b[5];
    let second = b[6];
    let microsecond = ((b[7] as u32) << 16) | ((b[8] as u32) << 8) | (b[9] as u32);
    Some((year, month, day, hour, minute, second, microsecond))
}

pub fn encode_datetime_bytes(year: u16, month: u8, day: u8, hour: u8, min: u8, sec: u8, us: u32) -> Vec<u8> {
    vec![
        (year >> 8) as u8,
        (year & 0xff) as u8,
        month,
        day,
        hour,
        min,
        sec,
        ((us >> 16) & 0xff) as u8,
        ((us >> 8) & 0xff) as u8,
        (us & 0xff) as u8,
    ]
}

pub fn format_datetime_iso(year: u16, month: u8, day: u8, hour: u8, min: u8, sec: u8, us: u32) -> String {
    if us > 0 {
        format!("{year:04}-{month:02}-{day:02}T{hour:02}:{min:02}:{sec:02}.{us:06}")
    } else {
        format!("{year:04}-{month:02}-{day:02}T{hour:02}:{min:02}:{sec:02}")
    }
}

/// Extract timezone info from a PickleValue (the second arg of datetime REDUCE).
pub fn extract_tz_info(
    tz_val: &PickleValue,
    to_json: &dyn Fn(&PickleValue) -> Result<Value, CodecError>,
) -> Result<Option<TzInfo>, CodecError> {
    match tz_val {
        PickleValue::Reduce { callable, args } => {
            if let PickleValue::Global { module, name } = callable.as_ref() {
                match (module.as_str(), name.as_str()) {
                    // datetime.timezone(timedelta(...))
                    ("datetime", "timezone") => {
                        if let PickleValue::Tuple(items) = args.as_ref() {
                            if items.len() == 1 {
                                if let Some(secs) = extract_timedelta_seconds(&items[0]) {
                                    return Ok(Some(TzInfo::FixedOffset(secs)));
                                }
                            }
                        }
                        // Couldn't parse — fall through to generic
                        Ok(None)
                    }
                    // pytz._UTC()
                    ("pytz", "_UTC") => Ok(Some(TzInfo::PytzUtc)),
                    // pytz._p('US/Eastern', -18000, 0, 'EST')
                    ("pytz", "_p") => {
                        if let PickleValue::Tuple(items) = args.as_ref() {
                            if items.len() >= 1 {
                                if let PickleValue::String(tz_name) = &items[0] {
                                    // Collect all args as JSON for roundtrip
                                    let args_json: Result<Vec<Value>, _> =
                                        items.iter().map(|i| to_json(i)).collect();
                                    return Ok(Some(TzInfo::Pytz {
                                        name: tz_name.clone(),
                                        args: args_json?,
                                    }));
                                }
                            }
                        }
                        Ok(None)
                    }
                    _ => {
                        // Check for zoneinfo double-REDUCE:
                        // callable = Reduce{Global(builtins, getattr), ...}
                        // This won't match here since callable is Global.
                        Ok(None)
                    }
                }
            } else if let PickleValue::Reduce { callable: inner_callable, .. } = callable.as_ref() {
                // Double-REDUCE: zoneinfo.ZoneInfo._unpickle
                // Outer callable is itself a Reduce (getattr(ZoneInfo, '_unpickle'))
                // Args: Tuple([String("US/Eastern"), Int(1)])
                if let PickleValue::Global { module, name } = inner_callable.as_ref() {
                    if module == "builtins" && name == "getattr" {
                        if let PickleValue::Tuple(outer_args) = args.as_ref() {
                            if outer_args.len() >= 1 {
                                if let PickleValue::String(tz_key) = &outer_args[0] {
                                    return Ok(Some(TzInfo::ZoneInfo(tz_key.clone())));
                                }
                            }
                        }
                    }
                }
                Ok(None)
            } else {
                Ok(None)
            }
        }
        _ => Ok(None),
    }
}

/// Extract total seconds from a timedelta PickleValue (REDUCE(datetime.timedelta, (d, s, us))).
pub fn extract_timedelta_seconds(val: &PickleValue) -> Option<i64> {
    if let PickleValue::Reduce { callable, args } = val {
        if let PickleValue::Global { module, name } = callable.as_ref() {
            if module == "datetime" && name == "timedelta" {
                if let PickleValue::Tuple(items) = args.as_ref() {
                    if items.len() == 3 {
                        let days = match &items[0] {
                            PickleValue::Int(i) => *i,
                            _ => return None,
                        };
                        let secs = match &items[1] {
                            PickleValue::Int(i) => *i,
                            _ => return None,
                        };
                        // Microseconds don't affect the offset string
                        return Some(days * 86400 + secs);
                    }
                }
            }
        }
    }
    None
}

#[derive(Debug)]
pub enum TzInfo {
    FixedOffset(i64), // total seconds from UTC
    PytzUtc,
    Pytz { name: String, args: Vec<Value> },
    ZoneInfo(String),
}

pub fn format_offset(total_seconds: i64) -> String {
    let sign = if total_seconds >= 0 { '+' } else { '-' };
    let abs_secs = total_seconds.unsigned_abs();
    let hours = abs_secs / 3600;
    let minutes = (abs_secs % 3600) / 60;
    format!("{sign}{hours:02}:{minutes:02}")
}

fn try_encode_datetime(
    args: &PickleValue,
    to_json: &dyn Fn(&PickleValue) -> Result<Value, CodecError>,
) -> Result<Option<Value>, CodecError> {
    let tuple_items = match args {
        PickleValue::Tuple(items) => items,
        _ => return Ok(None),
    };

    // First element must be 10-byte binary
    let dt_bytes = match tuple_items.first() {
        Some(PickleValue::Bytes(b)) if b.len() == 10 => b,
        _ => return Ok(None),
    };

    let (year, month, day, hour, min, sec, us) = match decode_datetime_bytes(dt_bytes) {
        Some(v) => v,
        None => return Ok(None),
    };

    let iso = format_datetime_iso(year, month, day, hour, min, sec, us);

    // Check for timezone (second element in the tuple)
    if tuple_items.len() == 1 {
        // Naive datetime
        Ok(Some(json!({"@dt": iso})))
    } else if tuple_items.len() == 2 {
        // Timezone-aware
        match extract_tz_info(&tuple_items[1], to_json)? {
            Some(TzInfo::FixedOffset(secs)) => {
                let offset = format_offset(secs);
                Ok(Some(json!({"@dt": format!("{iso}{offset}")})))
            }
            Some(TzInfo::PytzUtc) => {
                Ok(Some(json!({"@dt": format!("{iso}+00:00")})))
            }
            Some(TzInfo::Pytz { name, args }) => {
                Ok(Some(json!({"@dt": iso, "@tz": {"pytz": args, "name": name}})))
            }
            Some(TzInfo::ZoneInfo(key)) => {
                Ok(Some(json!({"@dt": iso, "@tz": {"zoneinfo": key}})))
            }
            None => {
                // Unknown tz pattern — fall through to generic @reduce
                Ok(None)
            }
        }
    } else {
        Ok(None)
    }
}

// ===========================================================================
// datetime.date
// ===========================================================================

fn try_encode_date(args: &PickleValue) -> Result<Option<Value>, CodecError> {
    let tuple_items = match args {
        PickleValue::Tuple(items) if items.len() == 1 => items,
        _ => return Ok(None),
    };

    let bytes = match &tuple_items[0] {
        PickleValue::Bytes(b) if b.len() == 4 => b,
        _ => return Ok(None),
    };

    let year = (bytes[0] as u16) * 256 + bytes[1] as u16;
    let month = bytes[2];
    let day = bytes[3];

    Ok(Some(json!({"@date": format!("{year:04}-{month:02}-{day:02}")})))
}

// ===========================================================================
// datetime.time
// ===========================================================================

pub fn decode_time_bytes(b: &[u8]) -> Option<(u8, u8, u8, u32)> {
    if b.len() != 6 {
        return None;
    }
    let hour = b[0];
    let minute = b[1];
    let second = b[2];
    let microsecond = ((b[3] as u32) << 16) | ((b[4] as u32) << 8) | (b[5] as u32);
    Some((hour, minute, second, microsecond))
}

fn try_encode_time(
    args: &PickleValue,
    to_json: &dyn Fn(&PickleValue) -> Result<Value, CodecError>,
) -> Result<Option<Value>, CodecError> {
    let tuple_items = match args {
        PickleValue::Tuple(items) if !items.is_empty() => items,
        _ => return Ok(None),
    };

    let bytes = match &tuple_items[0] {
        PickleValue::Bytes(b) if b.len() == 6 => b,
        _ => return Ok(None),
    };

    let (hour, min, sec, us) = match decode_time_bytes(bytes) {
        Some(v) => v,
        None => return Ok(None),
    };

    let time_str = if us > 0 {
        format!("{hour:02}:{min:02}:{sec:02}.{us:06}")
    } else {
        format!("{hour:02}:{min:02}:{sec:02}")
    };

    // Check for timezone (optional second element)
    if tuple_items.len() == 1 {
        Ok(Some(json!({"@time": time_str})))
    } else if tuple_items.len() == 2 {
        match extract_tz_info(&tuple_items[1], to_json)? {
            Some(TzInfo::FixedOffset(secs)) => {
                let offset = format_offset(secs);
                Ok(Some(json!({"@time": format!("{time_str}{offset}")})))
            }
            Some(TzInfo::PytzUtc) => {
                Ok(Some(json!({"@time": format!("{time_str}+00:00")})))
            }
            Some(TzInfo::Pytz { name, args }) => {
                Ok(Some(json!({"@time": time_str, "@tz": {"pytz": args, "name": name}})))
            }
            Some(TzInfo::ZoneInfo(key)) => {
                Ok(Some(json!({"@time": time_str, "@tz": {"zoneinfo": key}})))
            }
            None => Ok(None),
        }
    } else {
        Ok(None)
    }
}

// ===========================================================================
// datetime.timedelta
// ===========================================================================

fn try_encode_timedelta(args: &PickleValue) -> Result<Option<Value>, CodecError> {
    let tuple_items = match args {
        PickleValue::Tuple(items) if items.len() == 3 => items,
        _ => return Ok(None),
    };

    let days = match &tuple_items[0] {
        PickleValue::Int(i) => *i,
        _ => return Ok(None),
    };
    let secs = match &tuple_items[1] {
        PickleValue::Int(i) => *i,
        _ => return Ok(None),
    };
    let us = match &tuple_items[2] {
        PickleValue::Int(i) => *i,
        _ => return Ok(None),
    };

    Ok(Some(json!({"@td": [days, secs, us]})))
}

// ===========================================================================
// decimal.Decimal
// ===========================================================================

fn try_encode_decimal(args: &PickleValue) -> Result<Option<Value>, CodecError> {
    let tuple_items = match args {
        PickleValue::Tuple(items) if items.len() == 1 => items,
        _ => return Ok(None),
    };

    let s = match &tuple_items[0] {
        PickleValue::String(s) => s,
        _ => return Ok(None),
    };

    Ok(Some(json!({"@dec": s})))
}

// ===========================================================================
// set / frozenset (REDUCE in protocol 3)
// ===========================================================================

fn try_encode_set(
    args: &PickleValue,
    to_json: &dyn Fn(&PickleValue) -> Result<Value, CodecError>,
) -> Result<Option<Value>, CodecError> {
    // builtins.set([items]) → Tuple([List([items])])
    let tuple_items = match args {
        PickleValue::Tuple(items) if items.len() == 1 => items,
        _ => return Ok(None),
    };

    let list_items = match &tuple_items[0] {
        PickleValue::List(items) => items,
        _ => return Ok(None),
    };

    let arr: Result<Vec<Value>, _> = list_items.iter().map(|i| to_json(i)).collect();
    Ok(Some(json!({"@set": arr?})))
}

fn try_encode_frozenset(
    args: &PickleValue,
    to_json: &dyn Fn(&PickleValue) -> Result<Value, CodecError>,
) -> Result<Option<Value>, CodecError> {
    let tuple_items = match args {
        PickleValue::Tuple(items) if items.len() == 1 => items,
        _ => return Ok(None),
    };

    let list_items = match &tuple_items[0] {
        PickleValue::List(items) => items,
        _ => return Ok(None),
    };

    let arr: Result<Vec<Value>, _> = list_items.iter().map(|i| to_json(i)).collect();
    Ok(Some(json!({"@fset": arr?})))
}

// ===========================================================================
// uuid.UUID (Instance from NEWOBJ+BUILD, state = {'int': N})
// ===========================================================================

fn try_encode_uuid(state: &PickleValue) -> Result<Option<Value>, CodecError> {
    let pairs = match state {
        PickleValue::Dict(pairs) => pairs,
        _ => return Ok(None),
    };

    // Look for the 'int' key
    for (k, v) in pairs {
        if let PickleValue::String(key) = k {
            if key == "int" {
                let int_val = match v {
                    PickleValue::Int(i) => *i as u128,
                    PickleValue::BigInt(bi) => {
                        // Convert BigInt to u128
                        let (_, bytes) = bi.to_bytes_be();
                        let mut val: u128 = 0;
                        for b in bytes {
                            val = (val << 8) | b as u128;
                        }
                        val
                    }
                    _ => return Ok(None),
                };

                let hex = format!("{int_val:032x}");
                let uuid_str = format!(
                    "{}-{}-{}-{}-{}",
                    &hex[0..8],
                    &hex[8..12],
                    &hex[12..16],
                    &hex[16..20],
                    &hex[20..32]
                );
                return Ok(Some(json!({"@uuid": uuid_str})));
            }
        }
    }
    Ok(None)
}

// ===========================================================================
// Reverse: typed JSON → PickleValue
// ===========================================================================

fn try_decode_datetime(
    dt_val: &Value,
    tz_val: Option<&Value>,
    _from_json: &dyn Fn(&Value) -> Result<PickleValue, CodecError>,
) -> Result<PickleValue, CodecError> {
    let iso = dt_val
        .as_str()
        .ok_or_else(|| CodecError::InvalidData("@dt must be a string".into()))?;

    let (datetime_part, offset_part) = parse_iso_datetime(iso)?;
    let (year, month, day, hour, min, sec, us) = datetime_part;
    let dt_bytes = PickleValue::Bytes(encode_datetime_bytes(year, month, day, hour, min, sec, us));

    // Build the timezone PickleValue if present
    let tz_pickle = if let Some(tz_json) = tz_val {
        // Explicit @tz field: pytz or zoneinfo
        Some(decode_tz_json(tz_json)?)
    } else if let Some(offset_secs) = offset_part {
        // Offset in the ISO string
        Some(make_stdlib_timezone(offset_secs))
    } else {
        None
    };

    let args = if let Some(tz) = tz_pickle {
        PickleValue::Tuple(vec![dt_bytes, tz])
    } else {
        PickleValue::Tuple(vec![dt_bytes])
    };

    Ok(PickleValue::Reduce {
        callable: Box::new(PickleValue::Global {
            module: "datetime".into(),
            name: "datetime".into(),
        }),
        args: Box::new(args),
    })
}

fn try_decode_date(val: &Value) -> Result<PickleValue, CodecError> {
    let s = val
        .as_str()
        .ok_or_else(|| CodecError::InvalidData("@date must be a string".into()))?;

    // Parse YYYY-MM-DD
    if s.len() < 10 {
        return Err(CodecError::InvalidData(format!("invalid date: {s}")));
    }
    let year: u16 = s[0..4]
        .parse()
        .map_err(|_| CodecError::InvalidData(format!("invalid year in date: {s}")))?;
    let month: u8 = s[5..7]
        .parse()
        .map_err(|_| CodecError::InvalidData(format!("invalid month in date: {s}")))?;
    let day: u8 = s[8..10]
        .parse()
        .map_err(|_| CodecError::InvalidData(format!("invalid day in date: {s}")))?;

    let bytes = vec![(year >> 8) as u8, (year & 0xff) as u8, month, day];

    Ok(PickleValue::Reduce {
        callable: Box::new(PickleValue::Global {
            module: "datetime".into(),
            name: "date".into(),
        }),
        args: Box::new(PickleValue::Tuple(vec![PickleValue::Bytes(bytes)])),
    })
}

fn try_decode_time(val: &Value, tz_val: Option<&Value>) -> Result<PickleValue, CodecError> {
    let s = val
        .as_str()
        .ok_or_else(|| CodecError::InvalidData("@time must be a string".into()))?;

    let (time_part, offset_part) = parse_iso_time(s)?;
    let (hour, min, sec, us) = time_part;

    let bytes = vec![
        hour,
        min,
        sec,
        ((us >> 16) & 0xff) as u8,
        ((us >> 8) & 0xff) as u8,
        (us & 0xff) as u8,
    ];

    let tz_pickle = if let Some(tz_json) = tz_val {
        Some(decode_tz_json(tz_json)?)
    } else if let Some(offset_secs) = offset_part {
        Some(make_stdlib_timezone(offset_secs))
    } else {
        None
    };

    let args = if let Some(tz) = tz_pickle {
        PickleValue::Tuple(vec![PickleValue::Bytes(bytes), tz])
    } else {
        PickleValue::Tuple(vec![PickleValue::Bytes(bytes)])
    };

    Ok(PickleValue::Reduce {
        callable: Box::new(PickleValue::Global {
            module: "datetime".into(),
            name: "time".into(),
        }),
        args: Box::new(args),
    })
}

fn try_decode_timedelta(val: &Value) -> Result<PickleValue, CodecError> {
    let arr = val
        .as_array()
        .ok_or_else(|| CodecError::InvalidData("@td must be an array".into()))?;
    if arr.len() != 3 {
        return Err(CodecError::InvalidData("@td must have 3 elements".into()));
    }

    let days = arr[0]
        .as_i64()
        .ok_or_else(|| CodecError::InvalidData("@td[0] must be int".into()))?;
    let secs = arr[1]
        .as_i64()
        .ok_or_else(|| CodecError::InvalidData("@td[1] must be int".into()))?;
    let us = arr[2]
        .as_i64()
        .ok_or_else(|| CodecError::InvalidData("@td[2] must be int".into()))?;

    Ok(PickleValue::Reduce {
        callable: Box::new(PickleValue::Global {
            module: "datetime".into(),
            name: "timedelta".into(),
        }),
        args: Box::new(PickleValue::Tuple(vec![
            PickleValue::Int(days),
            PickleValue::Int(secs),
            PickleValue::Int(us),
        ])),
    })
}

fn try_decode_decimal(val: &Value) -> Result<PickleValue, CodecError> {
    let s = val
        .as_str()
        .ok_or_else(|| CodecError::InvalidData("@dec must be a string".into()))?;

    Ok(PickleValue::Reduce {
        callable: Box::new(PickleValue::Global {
            module: "decimal".into(),
            name: "Decimal".into(),
        }),
        args: Box::new(PickleValue::Tuple(vec![PickleValue::String(s.to_string())])),
    })
}

fn try_decode_uuid(val: &Value) -> Result<PickleValue, CodecError> {
    let s = val
        .as_str()
        .ok_or_else(|| CodecError::InvalidData("@uuid must be a string".into()))?;

    // Parse UUID string: remove hyphens, parse as hex to 128-bit int
    let hex: String = s.chars().filter(|c| *c != '-').collect();
    if hex.len() != 32 {
        return Err(CodecError::InvalidData(format!("invalid UUID: {s}")));
    }

    let int_val = u128::from_str_radix(&hex, 16)
        .map_err(|_| CodecError::InvalidData(format!("invalid UUID hex: {s}")))?;

    // UUID values can exceed i64, so use BigInt for large values
    let int_pickle = if int_val <= i64::MAX as u128 {
        PickleValue::Int(int_val as i64)
    } else {
        let bi = num_bigint::BigInt::from(int_val);
        PickleValue::BigInt(bi)
    };

    Ok(PickleValue::Instance {
        module: "uuid".into(),
        name: "UUID".into(),
        state: Box::new(PickleValue::Dict(vec![(
            PickleValue::String("int".into()),
            int_pickle,
        )])),
    })
}

// ===========================================================================
// ISO 8601 parsing helpers
// ===========================================================================

/// Parse ISO datetime string, returning components and optional offset in seconds.
pub fn parse_iso_datetime(
    s: &str,
) -> Result<((u16, u8, u8, u8, u8, u8, u32), Option<i64>), CodecError> {
    // Format: YYYY-MM-DDTHH:MM:SS[.ffffff][+HH:MM]
    if s.len() < 19 {
        return Err(CodecError::InvalidData(format!(
            "datetime too short: {s}"
        )));
    }

    let year: u16 = s[0..4]
        .parse()
        .map_err(|_| CodecError::InvalidData(format!("bad year: {s}")))?;
    let month: u8 = s[5..7]
        .parse()
        .map_err(|_| CodecError::InvalidData(format!("bad month: {s}")))?;
    let day: u8 = s[8..10]
        .parse()
        .map_err(|_| CodecError::InvalidData(format!("bad day: {s}")))?;
    let hour: u8 = s[11..13]
        .parse()
        .map_err(|_| CodecError::InvalidData(format!("bad hour: {s}")))?;
    let min: u8 = s[14..16]
        .parse()
        .map_err(|_| CodecError::InvalidData(format!("bad minute: {s}")))?;
    let sec: u8 = s[17..19]
        .parse()
        .map_err(|_| CodecError::InvalidData(format!("bad second: {s}")))?;

    let rest = &s[19..];

    // Parse optional microseconds
    let (us, rest) = if rest.starts_with('.') {
        let frac_end = rest[1..]
            .find(|c: char| !c.is_ascii_digit())
            .map(|i| i + 1)
            .unwrap_or(rest.len());
        let frac_str = &rest[1..frac_end];
        // Pad or truncate to 6 digits
        let padded = format!("{frac_str:0<6}");
        let us: u32 = padded[..6]
            .parse()
            .map_err(|_| CodecError::InvalidData(format!("bad microseconds: {s}")))?;
        (us, &rest[frac_end..])
    } else {
        (0u32, rest)
    };

    // Parse optional timezone offset
    let offset = parse_offset_suffix(rest)?;

    Ok(((year, month, day, hour, min, sec, us), offset))
}

/// Parse ISO time string.
pub fn parse_iso_time(s: &str) -> Result<((u8, u8, u8, u32), Option<i64>), CodecError> {
    if s.len() < 8 {
        return Err(CodecError::InvalidData(format!("time too short: {s}")));
    }

    let hour: u8 = s[0..2]
        .parse()
        .map_err(|_| CodecError::InvalidData(format!("bad hour: {s}")))?;
    let min: u8 = s[3..5]
        .parse()
        .map_err(|_| CodecError::InvalidData(format!("bad minute: {s}")))?;
    let sec: u8 = s[6..8]
        .parse()
        .map_err(|_| CodecError::InvalidData(format!("bad second: {s}")))?;

    let rest = &s[8..];

    let (us, rest) = if rest.starts_with('.') {
        let frac_end = rest[1..]
            .find(|c: char| !c.is_ascii_digit())
            .map(|i| i + 1)
            .unwrap_or(rest.len());
        let frac_str = &rest[1..frac_end];
        let padded = format!("{frac_str:0<6}");
        let us: u32 = padded[..6]
            .parse()
            .map_err(|_| CodecError::InvalidData(format!("bad microseconds: {s}")))?;
        (us, &rest[frac_end..])
    } else {
        (0u32, rest)
    };

    let offset = parse_offset_suffix(rest)?;

    Ok(((hour, min, sec, us), offset))
}

/// Parse a timezone offset suffix like "+05:30", "-05:00", "+00:00", "Z".
fn parse_offset_suffix(s: &str) -> Result<Option<i64>, CodecError> {
    if s.is_empty() {
        return Ok(None);
    }
    if s == "Z" {
        return Ok(Some(0));
    }

    let (sign, rest) = match s.as_bytes().first() {
        Some(b'+') => (1i64, &s[1..]),
        Some(b'-') => (-1i64, &s[1..]),
        _ => return Err(CodecError::InvalidData(format!("bad offset: {s}"))),
    };

    if rest.len() < 5 || rest.as_bytes()[2] != b':' {
        return Err(CodecError::InvalidData(format!("bad offset format: {s}")));
    }

    let hours: i64 = rest[0..2]
        .parse()
        .map_err(|_| CodecError::InvalidData(format!("bad offset hours: {s}")))?;
    let minutes: i64 = rest[3..5]
        .parse()
        .map_err(|_| CodecError::InvalidData(format!("bad offset minutes: {s}")))?;

    Ok(Some(sign * (hours * 3600 + minutes * 60)))
}

// ===========================================================================
// Timezone reconstruction helpers
// ===========================================================================

/// Construct PickleValue for datetime.timezone(timedelta(seconds=N)).
pub fn make_stdlib_timezone(offset_seconds: i64) -> PickleValue {
    let td = PickleValue::Reduce {
        callable: Box::new(PickleValue::Global {
            module: "datetime".into(),
            name: "timedelta".into(),
        }),
        args: Box::new(PickleValue::Tuple(vec![
            PickleValue::Int(0),
            PickleValue::Int(offset_seconds),
            PickleValue::Int(0),
        ])),
    };
    PickleValue::Reduce {
        callable: Box::new(PickleValue::Global {
            module: "datetime".into(),
            name: "timezone".into(),
        }),
        args: Box::new(PickleValue::Tuple(vec![td])),
    }
}

/// Decode @tz JSON back to a timezone PickleValue.
fn decode_tz_json(tz_json: &Value) -> Result<PickleValue, CodecError> {
    if let Value::Object(map) = tz_json {
        if let Some(pytz_args) = map.get("pytz") {
            // Reconstruct pytz._p(args...)
            let args = pytz_args
                .as_array()
                .ok_or_else(|| CodecError::InvalidData("pytz tz args must be array".into()))?;

            let pickle_args: Vec<PickleValue> = args
                .iter()
                .map(|a| match a {
                    Value::String(s) => Ok(PickleValue::String(s.clone())),
                    Value::Number(n) => {
                        if let Some(i) = n.as_i64() {
                            Ok(PickleValue::Int(i))
                        } else {
                            Err(CodecError::InvalidData("bad pytz arg".into()))
                        }
                    }
                    _ => Err(CodecError::InvalidData("unsupported pytz arg type".into())),
                })
                .collect::<Result<_, _>>()?;

            return Ok(PickleValue::Reduce {
                callable: Box::new(PickleValue::Global {
                    module: "pytz".into(),
                    name: "_p".into(),
                }),
                args: Box::new(PickleValue::Tuple(pickle_args)),
            });
        }

        if let Some(Value::String(key)) = map.get("zoneinfo") {
            // Reconstruct zoneinfo.ZoneInfo._unpickle(key, 1)
            // This is a double-REDUCE: getattr(ZoneInfo, '_unpickle')(key, 1)
            let inner_reduce = PickleValue::Reduce {
                callable: Box::new(PickleValue::Global {
                    module: "builtins".into(),
                    name: "getattr".into(),
                }),
                args: Box::new(PickleValue::Tuple(vec![
                    PickleValue::Global {
                        module: "zoneinfo".into(),
                        name: "ZoneInfo".into(),
                    },
                    PickleValue::String("_unpickle".into()),
                ])),
            };
            return Ok(PickleValue::Reduce {
                callable: Box::new(inner_reduce),
                args: Box::new(PickleValue::Tuple(vec![
                    PickleValue::String(key.clone()),
                    PickleValue::Int(1),
                ])),
            });
        }
    }

    Err(CodecError::InvalidData(
        "unrecognized @tz format".to_string(),
    ))
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::json::pickle_value_to_json;

    fn make_reduce(module: &str, name: &str, args: PickleValue) -> PickleValue {
        PickleValue::Reduce {
            callable: Box::new(PickleValue::Global {
                module: module.into(),
                name: name.into(),
            }),
            args: Box::new(args),
        }
    }

    // -- datetime --

    #[test]
    fn test_datetime_naive() {
        // 2025-06-15T12:00:00 → year=0x07E9
        let bytes = vec![0x07, 0xE9, 6, 15, 12, 0, 0, 0, 0, 0];
        let reduce = make_reduce(
            "datetime",
            "datetime",
            PickleValue::Tuple(vec![PickleValue::Bytes(bytes)]),
        );
        let json = pickle_value_to_json(&reduce).unwrap();
        assert_eq!(json, json!({"@dt": "2025-06-15T12:00:00"}));
    }

    #[test]
    fn test_datetime_with_microseconds() {
        // 2025-06-15T12:30:45.123456
        let us: u32 = 123456;
        let bytes = vec![
            0x07, 0xE9, 6, 15, 12, 30, 45,
            ((us >> 16) & 0xff) as u8,
            ((us >> 8) & 0xff) as u8,
            (us & 0xff) as u8,
        ];
        let reduce = make_reduce(
            "datetime",
            "datetime",
            PickleValue::Tuple(vec![PickleValue::Bytes(bytes)]),
        );
        let json = pickle_value_to_json(&reduce).unwrap();
        assert_eq!(json, json!({"@dt": "2025-06-15T12:30:45.123456"}));
    }

    #[test]
    fn test_datetime_with_stdlib_utc() {
        let bytes = vec![0x07, 0xE9, 1, 1, 0, 0, 0, 0, 0, 0];
        let tz = make_reduce(
            "datetime",
            "timezone",
            PickleValue::Tuple(vec![make_reduce(
                "datetime",
                "timedelta",
                PickleValue::Tuple(vec![
                    PickleValue::Int(0),
                    PickleValue::Int(0),
                    PickleValue::Int(0),
                ]),
            )]),
        );
        let reduce = make_reduce(
            "datetime",
            "datetime",
            PickleValue::Tuple(vec![PickleValue::Bytes(bytes), tz]),
        );
        let json = pickle_value_to_json(&reduce).unwrap();
        assert_eq!(json, json!({"@dt": "2025-01-01T00:00:00+00:00"}));
    }

    #[test]
    fn test_datetime_with_offset() {
        let bytes = vec![0x07, 0xE9, 1, 1, 0, 0, 0, 0, 0, 0];
        // +05:30 = 19800 seconds
        let tz = make_reduce(
            "datetime",
            "timezone",
            PickleValue::Tuple(vec![make_reduce(
                "datetime",
                "timedelta",
                PickleValue::Tuple(vec![
                    PickleValue::Int(0),
                    PickleValue::Int(19800),
                    PickleValue::Int(0),
                ]),
            )]),
        );
        let reduce = make_reduce(
            "datetime",
            "datetime",
            PickleValue::Tuple(vec![PickleValue::Bytes(bytes), tz]),
        );
        let json = pickle_value_to_json(&reduce).unwrap();
        assert_eq!(json, json!({"@dt": "2025-01-01T00:00:00+05:30"}));
    }

    #[test]
    fn test_datetime_with_pytz_utc() {
        let bytes = vec![0x07, 0xE9, 1, 1, 0, 0, 0, 0, 0, 0];
        let tz = make_reduce("pytz", "_UTC", PickleValue::Tuple(vec![]));
        let reduce = make_reduce(
            "datetime",
            "datetime",
            PickleValue::Tuple(vec![PickleValue::Bytes(bytes), tz]),
        );
        let json = pickle_value_to_json(&reduce).unwrap();
        assert_eq!(json, json!({"@dt": "2025-01-01T00:00:00+00:00"}));
    }

    #[test]
    fn test_datetime_with_pytz_named() {
        let bytes = vec![0x07, 0xE9, 1, 1, 0, 0, 0, 0, 0, 0];
        let tz = make_reduce(
            "pytz",
            "_p",
            PickleValue::Tuple(vec![
                PickleValue::String("US/Eastern".into()),
                PickleValue::Int(-18000),
                PickleValue::Int(0),
                PickleValue::String("EST".into()),
            ]),
        );
        let reduce = make_reduce(
            "datetime",
            "datetime",
            PickleValue::Tuple(vec![PickleValue::Bytes(bytes), tz]),
        );
        let json = pickle_value_to_json(&reduce).unwrap();
        assert_eq!(json["@dt"], "2025-01-01T00:00:00");
        let tz_info = &json["@tz"];
        assert_eq!(tz_info["name"], "US/Eastern");
    }

    // -- date --

    #[test]
    fn test_date() {
        let bytes = vec![0x07, 0xE9, 6, 15];
        let reduce = make_reduce(
            "datetime",
            "date",
            PickleValue::Tuple(vec![PickleValue::Bytes(bytes)]),
        );
        let json = pickle_value_to_json(&reduce).unwrap();
        assert_eq!(json, json!({"@date": "2025-06-15"}));
    }

    // -- time --

    #[test]
    fn test_time_no_us() {
        let bytes = vec![12, 30, 45, 0, 0, 0];
        let reduce = make_reduce(
            "datetime",
            "time",
            PickleValue::Tuple(vec![PickleValue::Bytes(bytes)]),
        );
        let json = pickle_value_to_json(&reduce).unwrap();
        assert_eq!(json, json!({"@time": "12:30:45"}));
    }

    #[test]
    fn test_time_with_us() {
        let us: u32 = 500000;
        let bytes = vec![
            12, 30, 45,
            ((us >> 16) & 0xff) as u8,
            ((us >> 8) & 0xff) as u8,
            (us & 0xff) as u8,
        ];
        let reduce = make_reduce(
            "datetime",
            "time",
            PickleValue::Tuple(vec![PickleValue::Bytes(bytes)]),
        );
        let json = pickle_value_to_json(&reduce).unwrap();
        assert_eq!(json, json!({"@time": "12:30:45.500000"}));
    }

    // -- timedelta --

    #[test]
    fn test_timedelta() {
        let reduce = make_reduce(
            "datetime",
            "timedelta",
            PickleValue::Tuple(vec![
                PickleValue::Int(7),
                PickleValue::Int(3600),
                PickleValue::Int(500000),
            ]),
        );
        let json = pickle_value_to_json(&reduce).unwrap();
        assert_eq!(json, json!({"@td": [7, 3600, 500000]}));
    }

    // -- Decimal --

    #[test]
    fn test_decimal() {
        let reduce = make_reduce(
            "decimal",
            "Decimal",
            PickleValue::Tuple(vec![PickleValue::String("3.14159".into())]),
        );
        let json = pickle_value_to_json(&reduce).unwrap();
        assert_eq!(json, json!({"@dec": "3.14159"}));
    }

    // -- set --

    #[test]
    fn test_set_reduce() {
        let reduce = make_reduce(
            "builtins",
            "set",
            PickleValue::Tuple(vec![PickleValue::List(vec![
                PickleValue::Int(1),
                PickleValue::Int(2),
                PickleValue::Int(3),
            ])]),
        );
        let json = pickle_value_to_json(&reduce).unwrap();
        assert_eq!(json, json!({"@set": [1, 2, 3]}));
    }

    // -- frozenset --

    #[test]
    fn test_frozenset_reduce() {
        let reduce = make_reduce(
            "builtins",
            "frozenset",
            PickleValue::Tuple(vec![PickleValue::List(vec![
                PickleValue::Int(1),
                PickleValue::Int(2),
            ])]),
        );
        let json = pickle_value_to_json(&reduce).unwrap();
        assert_eq!(json, json!({"@fset": [1, 2]}));
    }

    // -- UUID --

    #[test]
    fn test_uuid() {
        // UUID: 12345678-1234-5678-1234-567812345678
        // Integer value: 0x12345678123456781234567812345678
        let int_val: u128 = 0x12345678_1234_5678_1234_5678_1234_5678;
        let bi = num_bigint::BigInt::from(int_val);
        let instance = PickleValue::Instance {
            module: "uuid".into(),
            name: "UUID".into(),
            state: Box::new(PickleValue::Dict(vec![(
                PickleValue::String("int".into()),
                PickleValue::BigInt(bi),
            )])),
        };
        let json = pickle_value_to_json(&instance).unwrap();
        assert_eq!(json, json!({"@uuid": "12345678-1234-5678-1234-567812345678"}));
    }

    // -- Roundtrip tests --

    #[test]
    fn test_roundtrip_datetime_naive() {
        let json = json!({"@dt": "2025-06-15T12:30:45.123456"});
        let pv = crate::json::json_to_pickle_value(&json).unwrap();
        let json2 = pickle_value_to_json(&pv).unwrap();
        assert_eq!(json, json2);
    }

    #[test]
    fn test_roundtrip_datetime_utc() {
        let json = json!({"@dt": "2025-01-01T00:00:00+00:00"});
        let pv = crate::json::json_to_pickle_value(&json).unwrap();
        let json2 = pickle_value_to_json(&pv).unwrap();
        assert_eq!(json, json2);
    }

    #[test]
    fn test_roundtrip_datetime_offset() {
        let json = json!({"@dt": "2025-01-01T00:00:00+05:30"});
        let pv = crate::json::json_to_pickle_value(&json).unwrap();
        let json2 = pickle_value_to_json(&pv).unwrap();
        assert_eq!(json, json2);
    }

    #[test]
    fn test_roundtrip_date() {
        let json = json!({"@date": "2025-06-15"});
        let pv = crate::json::json_to_pickle_value(&json).unwrap();
        let json2 = pickle_value_to_json(&pv).unwrap();
        assert_eq!(json, json2);
    }

    #[test]
    fn test_roundtrip_time() {
        let json = json!({"@time": "12:30:45"});
        let pv = crate::json::json_to_pickle_value(&json).unwrap();
        let json2 = pickle_value_to_json(&pv).unwrap();
        assert_eq!(json, json2);
    }

    #[test]
    fn test_roundtrip_timedelta() {
        let json = json!({"@td": [7, 3600, 0]});
        let pv = crate::json::json_to_pickle_value(&json).unwrap();
        let json2 = pickle_value_to_json(&pv).unwrap();
        assert_eq!(json, json2);
    }

    #[test]
    fn test_roundtrip_decimal() {
        let json = json!({"@dec": "3.14159"});
        let pv = crate::json::json_to_pickle_value(&json).unwrap();
        let json2 = pickle_value_to_json(&pv).unwrap();
        assert_eq!(json, json2);
    }

    #[test]
    fn test_roundtrip_uuid() {
        let json = json!({"@uuid": "12345678-1234-5678-1234-567812345678"});
        let pv = crate::json::json_to_pickle_value(&json).unwrap();
        let json2 = pickle_value_to_json(&pv).unwrap();
        assert_eq!(json, json2);
    }
}

//! Shared JSON value coercion and UTF-8 BOM-stripping helpers used by
//! `onnxmodelconfig` and `onnxtokenizer`.

use std::fs;
use std::path::Path;

/// UTF-8 byte-order mark skipped before JSON parsing.
const UTF8_BOM: &[u8] = &[0xEF, 0xBB, 0xBF];

/// Zero-copy UTF-8 BOM strip (`&bytes[3..]` when prefixed with `EF BB BF`).
#[inline]
pub(crate) fn strip_utf8_bom(bytes: &[u8]) -> &[u8] {
    bytes.strip_prefix(UTF8_BOM).unwrap_or(bytes)
}

/// Converts a JSON number to `i64` only when it is an exact integer representable
/// in `i64`; otherwise returns `default`.
pub(crate) fn to_integer(value: Option<&serde_json::Value>, default: i64) -> i64 {
    let Some(serde_json::Value::Number(number)) = value else {
        return default;
    };
    if let Some(integer) = number.as_i64() {
        return integer;
    }
    let Some(double) = number.as_f64() else {
        return default;
    };
    if double.is_finite()
        && double.fract() == 0.0
        && double >= i64::MIN as f64
        && double < 9_223_372_036_854_775_808.0
    {
        double as i64
    } else {
        default
    }
}

/// Converts a JSON number to `i32`, rejecting non-integral and out-of-range values.
pub(crate) fn to_int(value: Option<&serde_json::Value>, default: i32) -> i32 {
    i32::try_from(to_integer(value, default as i64)).unwrap_or(default)
}

/// Converts a finite JSON number to `f64`, otherwise returns `default`.
pub(crate) fn to_double(value: Option<&serde_json::Value>, default: f64) -> f64 {
    match value {
        Some(serde_json::Value::Number(number)) => {
            let double = number.as_f64().unwrap_or(f64::INFINITY);
            if double.is_finite() {
                double
            } else {
                default
            }
        }
        _ => default,
    }
}

/// Extracts a JSON string value, returning an empty string for missing or non-string values.
pub(crate) fn as_str_or_empty(value: Option<&serde_json::Value>) -> String {
    value
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
        .to_owned()
}

/// Returns `true` if `object` has an entry for `key`.
pub(crate) fn contains(object: &serde_json::Value, key: &str) -> bool {
    object.get(key).is_some()
}

/// Reads and parses a JSON object from `path`, returning an empty JSON object on error.
pub(crate) fn read_json_object(path: &Path) -> serde_json::Value {
    fn empty_object() -> serde_json::Value {
        serde_json::Value::Object(serde_json::Map::new())
    }
    let Ok(bytes) = fs::read(path) else {
        return empty_object();
    };
    let bytes = strip_utf8_bom(&bytes);
    match serde_json::from_slice::<serde_json::Value>(bytes) {
        Ok(root) if root.is_object() => root,
        _ => empty_object(),
    }
}

////////////////////////////////////////////////////////////////////////////////
//
// Copyright (c) 2026 Ripose
//
// This file is part of Memento.
//
// Memento is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, version 2 of the License.
//
// Memento is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with Memento.  If not, see <https://www.gnu.org/licenses/>.
//
////////////////////////////////////////////////////////////////////////////////

//! Shared Qt-compatible JSON value coercion and BOM-stripping helpers used by
//! `onnxmodelconfig` and `onnxtokenizer`.

use std::fs;
use std::path::Path;

/// UTF-8 byte-order mark skipped by `QJsonDocument::fromJson`.
const UTF8_BOM: &[u8] = &[0xEF, 0xBB, 0xBF];

/// Zero-copy UTF-8 BOM strip (`&bytes[3..]` when prefixed with `EF BB BF`).
#[inline]
pub(crate) fn strip_utf8_bom(bytes: &[u8]) -> &[u8] {
    bytes.strip_prefix(UTF8_BOM).unwrap_or(bytes)
}

/// C++: `QJsonValue::toInteger(defaultValue)` — only whole numbers
/// representable as `i64` convert. Fractional JSON numbers do not silently
/// truncate, and 64-bit JSON integers above `2^53` retain full precision.
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

/// C++: `QJsonValue::toInt(defaultValue)` — rejects non-integral and
/// out-of-`i32`-range values rather than narrowing modulo `2^32`.
pub(crate) fn to_int(value: Option<&serde_json::Value>, default: i32) -> i32 {
    i32::try_from(to_integer(value, default as i64)).unwrap_or(default)
}

/// C++: `QJsonValue::toDouble(defaultValue)`.
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

/// C++: `QJsonValue::toString()` — empty string when the value is not a
/// string.
pub(crate) fn to_string_qt(value: Option<&serde_json::Value>) -> String {
    value
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
        .to_owned()
}

/// C++: `QJsonObject::contains(key)`.
pub(crate) fn contains(object: &serde_json::Value, key: &str) -> bool {
    object.get(key).is_some()
}

/// C++: `readJsonObject(path)` — returns an empty JSON object when the file
/// cannot be read or does not parse as a JSON object.
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

//! In-memory UTF-16 JSON values used by the pure note and glossary routines.

use std::collections::BTreeMap;

/// Object keys are unique and sorted lexicographically by UTF-16 code units.
pub type JsonObject = BTreeMap<Vec<u16>, JsonValue>;

#[derive(Debug, Clone, PartialEq)]
pub enum JsonValue {
    Undefined,
    Null,
    Bool(bool),
    Number(f64),
    String(Vec<u16>),
    Array(Vec<JsonValue>),
    Object(JsonObject),
}

impl JsonValue {
    /// Returns the inner UTF-16 slice if this value is a string.
    pub fn as_qstring(&self) -> Option<&[u16]> {
        match self {
            Self::String(text) => Some(text),
            _ => None,
        }
    }

    pub fn as_object(&self) -> Option<&JsonObject> {
        match self {
            Self::Object(obj) => Some(obj),
            _ => None,
        }
    }
}

/// Inserts `value` into `object`, removing `key` if `value` is `Undefined`.
pub fn insert_qjson(object: &mut JsonObject, key: Vec<u16>, value: JsonValue) {
    if matches!(value, JsonValue::Undefined) {
        object.remove(&key);
    } else {
        object.insert(key, value);
    }
}

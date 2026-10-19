//! In-memory QJsonValue/QJsonObject-shaped values used by the pure note and
//! glossary routines. Strings and object keys remain UTF-16; a JSON text
//! serializer (and its UTF-8/surrogate policy) is deliberately not implied.

use std::collections::BTreeMap;

/// QJsonObject keys are unique and sorted as QStrings; inserting an existing
/// key overwrites its value. Lexicographic UTF-16 ordering preserves that
/// order without changing lone surrogate code units.
pub type JsonObject = BTreeMap<Vec<u16>, JsonValue>;

#[derive(Debug, Clone, PartialEq)]
pub enum JsonValue
{
    Undefined,
    Null,
    Bool(bool),
    Number(f64),
    String(Vec<u16>),
    Array(Vec<JsonValue>),
    Object(JsonObject),
}

impl JsonValue
{
    /// `QJsonValue::toString()` without a default: only strings coerce to
    /// strings; numbers, booleans, objects, arrays and null do not.
    pub fn as_qstring(&self) -> Option<&[u16]>
    {
        match self
        {
            Self::String(text) => Some(text),
            _ => None,
        }
    }

    pub fn as_object(&self) -> Option<&JsonObject>
    {
        match self
        {
            Self::Object(obj) => Some(obj),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<&[JsonValue]>
    {
        match self
        {
            Self::Array(arr) => Some(arr),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool>
    {
        match self
        {
            Self::Bool(b) => Some(*b),
            _ => None,
        }
    }

    pub fn as_number(&self) -> Option<f64>
    {
        match self
        {
            Self::Number(n) => Some(*n),
            _ => None,
        }
    }

    pub fn is_null(&self) -> bool
    {
        matches!(self, Self::Null)
    }

    pub fn is_undefined(&self) -> bool
    {
        matches!(self, Self::Undefined)
    }

    pub fn is_object(&self) -> bool
    {
        matches!(self, Self::Object(_))
    }

    pub fn is_array(&self) -> bool
    {
        matches!(self, Self::Array(_))
    }

    pub fn is_string(&self) -> bool
    {
        matches!(self, Self::String(_))
    }

    pub fn is_double(&self) -> bool
    {
        matches!(self, Self::Number(_))
    }

    pub fn is_bool(&self) -> bool
    {
        matches!(self, Self::Bool(_))
    }

    /// Qt stores NaN and infinities passed to `QJsonValue(double)` as null.
    pub fn from_qjson_number(value: f64) -> Self
    {
        if value.is_finite() { Self::Number(value) } else { Self::Null }
    }
}

/// `QJsonObject::insert(key, QJsonValue::Undefined)` removes the key. The
/// distinction matters for `Context::setFields(QJsonValue())`.
pub fn insert_qjson(object: &mut JsonObject, key: Vec<u16>, value: JsonValue)
{
    if matches!(value, JsonValue::Undefined)
    {
        object.remove(&key);
    }
    else
    {
        object.insert(key, value);
    }
}

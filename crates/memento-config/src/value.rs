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

//! The value model: the set of Rust types Memento actually round-trips
//! through `QSettings`, as established by auditing every `QSettings::value`
//! / `QSettings::setValue` call in `src/setting/settings.cpp` and
//! `src/setting/migration.cpp`:
//!
//! | C++ source pattern                    | [`Value`] variant       |
//! |---------------------------------------|-------------------------|
//! | `s.value(key, bool).toBool()`         | [`Value::Bool`]         |
//! | `s.value(key, int).toInt()`           | [`Value::Int`]          |
//! | `s.value(key, double).toDouble()`     | [`Value::Real`]         |
//! | `s.value(key, "…")` / `.toString()`   | [`Value::Text`]         |
//! | `s.value(key).toStringList()`         | [`Value::Strings`]      |
//! | `s.value(key).toList()` of int64      | [`Value::Ints`]         |
//! | keybind profiles: group of strings    | [`Value::Map`]          |
//!
//! Ints are carried as `i64` (QVariant ints written to disk are integral and
//! Memento never overflows `i64` ranges), reals as `f64`. Conversion between
//! [`Value`] and plain Rust types goes through [`IntoValue`] / [`FromValue`].

use std::collections::BTreeMap;
use std::fmt;

/// A stored configuration value.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    /// bool flag (`true` / `false`).
    Bool(bool),
    /// Integral setting (`i64`).
    Int(i64),
    /// Floating point setting (`f64`).
    Real(f64),
    /// Unicode string.
    Text(String),
    /// Ordered list of strings (QSettings `QStringList`).
    Strings(Vec<String>),
    /// Ordered list of integers (QSettings `QVariantList` of int64; used for
    /// the dictionary display order).
    Ints(Vec<i64>),
    /// String-to-string map (used for the flat string fields of a keybind
    /// profile, which C++ stores as a nested settings group).
    Map(BTreeMap<String, String>),
}

impl Value {
    /// The declared slot type of this value.
    pub fn value_type(&self) -> ValueType {
        match self {
            Value::Bool(_) => ValueType::Bool,
            Value::Int(_) => ValueType::Int,
            Value::Real(_) => ValueType::Real,
            Value::Text(_) => ValueType::Text,
            Value::Strings(_) => ValueType::Strings,
            Value::Ints(_) => ValueType::Ints,
            Value::Map(_) => ValueType::Map,
        }
    }
}

/// The type a key declares; the store uses it both for typed accessors
/// (`get::<T>(Key)`) and to disambiguate empty containers on load.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ValueType {
    Bool,
    Int,
    Real,
    Text,
    Strings,
    Ints,
    Map,
}

impl ValueType {
    /// Human readable name used in diagnostics.
    pub fn name(self) -> &'static str {
        match self {
            ValueType::Bool => "bool",
            ValueType::Int => "int",
            ValueType::Real => "real",
            ValueType::Text => "text",
            ValueType::Strings => "string-list",
            ValueType::Ints => "int-list",
            ValueType::Map => "map",
        }
    }
}

impl fmt::Display for ValueType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Conversion from a [`Value`] into a typed setting.
///
/// Conversions are strict: the slot type must match (empty containers are
/// handled by the store based on the *declared* type of the key, not by
/// guessing here).
pub trait FromValue: Sized {
    /// Strict conversion; `Err` carries the expected and found types.
    fn from_value(value: &Value) -> Result<Self, (ValueType, ValueType)>;

    /// Neutral value of this type, used when a key has no declared default
    /// and is simply absent (matches C++ `QSettings` reading a missing key
    /// into a default-constructed `QVariant`).
    fn neutral() -> Self;
}

/// Conversion from a typed setting into a [`Value`].
pub trait IntoValue: Sized {
    fn into_value(self) -> Value;
}

impl IntoValue for bool {
    fn into_value(self) -> Value {
        Value::Bool(self)
    }
}

impl FromValue for bool {
    fn from_value(value: &Value) -> Result<Self, (ValueType, ValueType)> {
        match value {
            Value::Bool(v) => Ok(*v),
            other => Err((other.value_type(), ValueType::Bool)),
        }
    }

    fn neutral() -> Self {
        false
    }
}

macro_rules! int_impls {
    ($(@$t:ty),*) => {$(
        impl FromValue for $t {
            fn from_value(value: &Value) -> Result<Self, (ValueType, ValueType)> {
                match value {
                    Value::Int(v) => <$t>::try_from(*v)
                        .map_err(|_| (ValueType::Int, ValueType::Int)),
                    other => Err((other.value_type(), ValueType::Int)),
                }
            }

            fn neutral() -> Self {
                0
            }
        }

        impl IntoValue for $t {
            fn into_value(self) -> Value {
                Value::Int(i64::from(self))
            }
        }
    )*};
}

int_impls!(@i32, @i64, @u32);

impl FromValue for f64 {
    fn from_value(value: &Value) -> Result<Self, (ValueType, ValueType)> {
        match value {
            Value::Real(v) => Ok(*v),
            other => Err((other.value_type(), ValueType::Real)),
        }
    }

    fn neutral() -> Self {
        0.0
    }
}

impl IntoValue for f64 {
    fn into_value(self) -> Value {
        Value::Real(self)
    }
}

impl FromValue for String {
    fn from_value(value: &Value) -> Result<Self, (ValueType, ValueType)> {
        match value {
            Value::Text(v) => Ok(v.clone()),
            other => Err((other.value_type(), ValueType::Text)),
        }
    }

    fn neutral() -> Self {
        String::new()
    }
}

impl IntoValue for String {
    fn into_value(self) -> Value {
        Value::Text(self)
    }
}

impl IntoValue for &str {
    fn into_value(self) -> Value {
        Value::Text(self.to_owned())
    }
}

impl FromValue for Vec<String> {
    fn from_value(value: &Value) -> Result<Self, (ValueType, ValueType)> {
        match value {
            Value::Strings(v) => Ok(v.clone()),
            other => Err((other.value_type(), ValueType::Strings)),
        }
    }

    fn neutral() -> Self {
        Vec::new()
    }
}

impl IntoValue for Vec<String> {
    fn into_value(self) -> Value {
        Value::Strings(self)
    }
}

impl FromValue for Vec<i64> {
    fn from_value(value: &Value) -> Result<Self, (ValueType, ValueType)> {
        match value {
            Value::Ints(v) => Ok(v.clone()),
            other => Err((other.value_type(), ValueType::Ints)),
        }
    }

    fn neutral() -> Self {
        Vec::new()
    }
}

impl IntoValue for Vec<i64> {
    fn into_value(self) -> Value {
        Value::Ints(self)
    }
}

impl FromValue for BTreeMap<String, String> {
    fn from_value(value: &Value) -> Result<Self, (ValueType, ValueType)> {
        match value {
            Value::Map(v) => Ok(v.clone()),
            other => Err((other.value_type(), ValueType::Map)),
        }
    }

    fn neutral() -> Self {
        BTreeMap::new()
    }
}

impl IntoValue for BTreeMap<String, String> {
    fn into_value(self) -> Value {
        Value::Map(self)
    }
}

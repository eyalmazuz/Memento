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

//! Minimal JSON value model standing in for QJsonDocument/QJsonObject at the
//! integration seam. Only the QJsonValue operations the CSS engine uses are
//! provided (`isString`, `isDouble`, `isObject`, `isArray`, `toString`,
//! `toDouble`, key lookup, `keys()`).
//!
//! PORT-NOTE: QJsonObject stores keys sorted, so `data.keys()` in
//! `structuredElement` (structuredrichtext_css.cpp L1163) walks sorted keys;
//! this module's [`BTreeMap`] yields the same sorted order for the ASCII keys
//! structured content uses. Attribute insertion order is never observed
//! downstream (QHash), so the sorted order is unobservable there too.

use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq)]
pub enum Json {
    /// QJsonValue(Undefined): a missing object key.
    Missing,
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    Array(Vec<Json>),
    Object(BTreeMap<String, Json>),
}

impl Default for Json {
    fn default() -> Self {
        Json::Missing
    }
}

impl Json {
    pub fn object(pairs: impl IntoIterator<Item = (impl Into<String>, Json)>) -> Json {
        let mut map = BTreeMap::new();
        for (key, value) in pairs {
            map.insert(key.into(), value);
        }
        Json::Object(map)
    }

    pub fn string(text: impl Into<String>) -> Json {
        Json::String(text.into())
    }

    /// `obj[key]`; a missing key is QJsonValue(Undefined).
    pub fn get(&self, key: &str) -> &Json {
        match self {
            Json::Object(map) => map.get(key).unwrap_or(&Json::Missing),
            _ => &Json::Missing,
        }
    }

    /// QJsonValue::isString().
    pub fn is_string(&self) -> bool {
        matches!(self, Json::String(_))
    }

    /// QJsonValue::isDouble() (bools are not doubles).
    pub fn is_double(&self) -> bool {
        matches!(self, Json::Number(_))
    }

    pub fn is_object(&self) -> bool {
        matches!(self, Json::Object(_))
    }

    pub fn is_array(&self) -> bool {
        matches!(self, Json::Array(_))
    }

    /// QJsonValue::toString(): the string value, or "" for all other types.
    pub fn to_string_value(&self) -> String {
        match self {
            Json::String(text) => text.clone(),
            _ => String::new(),
        }
    }

    /// QJsonValue::toString(): borrowed form.
    pub fn as_str(&self) -> &str {
        match self {
            Json::String(text) => text,
            _ => "",
        }
    }

    /// QJsonValue::toDouble(): the number value, or 0.0 for other types.
    pub fn to_double(&self) -> f64 {
        match self {
            Json::Number(value) => *value,
            _ => 0.0,
        }
    }

    /// QJsonObject::keys(): sorted by QString's UTF-16 code-unit order.
    pub fn keys(&self) -> Vec<String> {
        match self {
            Json::Object(map) => {
                let mut keys: Vec<_> = map.keys().cloned().collect();
                if keys.iter().any(|key| !key.is_ascii()) {
                    keys.sort_unstable_by(|left, right| left.encode_utf16().cmp(right.encode_utf16()));
                }
                keys
            }
            _ => Vec::new(),
        }
    }

    /// QJsonValue::toObject(): the object value, or an empty object.
    pub fn to_object(&self) -> Json {
        match self {
            Json::Object(_) => self.clone(),
            _ => Json::Object(BTreeMap::new()),
        }
    }

    /// QJsonValue::toArray(): the array value, or an empty array.
    ///
    /// The borrowed form for iteration; ports `toArray()`'s accessor use.
    pub fn as_array(&self) -> &[Json] {
        match self {
            Json::Array(values) => values,
            _ => &[],
        }
    }

    /// Parse a JSON document (RFC 8259 subset). Duplicate object keys keep the
    /// last value (QJsonObject::insert overwrite behavior).
    pub fn parse(text: &str) -> Result<Json, String> {
        let mut parser = Parser {
            chars: text.chars().collect(),
            position: 0,
        };
        let value = parser.parse_value(0)?;
        parser.skip_whitespace();
        if parser.position != parser.chars.len() {
            return Err(format!("trailing data at offset {}", parser.position));
        }
        Ok(value)
    }
}

struct Parser {
    chars: Vec<char>,
    position: usize,
}

/// QJsonValue can nest through structured-content documents; structured
/// content in practice stays under dozens of levels. Bound recursion anyway.
const MAXIMUM_NESTING: usize = 128;

impl Parser {
    fn parse_value(&mut self, depth: usize) -> Result<Json, String> {
        if depth > MAXIMUM_NESTING {
            return Err("maximum nesting depth exceeded".to_string());
        }
        self.skip_whitespace();
        let Some(&ch) = self.peek() else {
            return Err("unexpected end of input".to_string());
        };
        match ch {
            '{' => self.parse_object(depth),
            '[' => self.parse_array(depth),
            '"' => self.parse_string().map(Json::String),
            't' => self.parse_keyword("true", Json::Bool(true)),
            'f' => self.parse_keyword("false", Json::Bool(false)),
            'n' => self.parse_keyword("null", Json::Null),
            '-' | '0'..='9' => self.parse_number(),
            _ => Err(format!("unexpected character {ch:?} at offset {}", self.position)),
        }
    }

    fn parse_keyword(&mut self, word: &str, value: Json) -> Result<Json, String> {
        if self.chars[self.position..].starts_with(&word.chars().collect::<Vec<_>>()) {
            self.position += word.chars().count();
            Ok(value)
        } else {
            Err(format!("invalid literal at offset {}", self.position))
        }
    }

    fn parse_object(&mut self, depth: usize) -> Result<Json, String> {
        self.position += 1; // '{'
        let mut map = BTreeMap::new();
        self.skip_whitespace();
        if self.peek() == Some(&'}') {
            self.position += 1;
            return Ok(Json::Object(map));
        }
        loop {
            self.skip_whitespace();
            if self.peek() != Some(&'"') {
                return Err(format!("expected object key at offset {}", self.position));
            }
            let key = self.parse_string()?;
            self.skip_whitespace();
            if self.peek() != Some(&':') {
                return Err(format!("expected ':' at offset {}", self.position));
            }
            self.position += 1;
            let value = self.parse_value(depth + 1)?;
            map.insert(key, value);
            self.skip_whitespace();
            match self.peek() {
                Some(&',') => {
                    self.position += 1;
                }
                Some(&'}') => {
                    self.position += 1;
                    return Ok(Json::Object(map));
                }
                _ => return Err(format!("expected ',' or '}}' at offset {}", self.position)),
            }
        }
    }

    fn parse_array(&mut self, depth: usize) -> Result<Json, String> {
        self.position += 1; // '['
        let mut values = Vec::new();
        self.skip_whitespace();
        if self.peek() == Some(&']') {
            self.position += 1;
            return Ok(Json::Array(values));
        }
        loop {
            values.push(self.parse_value(depth + 1)?);
            self.skip_whitespace();
            match self.peek() {
                Some(&',') => {
                    self.position += 1;
                }
                Some(&']') => {
                    self.position += 1;
                    return Ok(Json::Array(values));
                }
                _ => return Err(format!("expected ',' or ']' at offset {}", self.position)),
            }
        }
    }

    fn parse_string(&mut self) -> Result<String, String> {
        self.position += 1; // '"'
        let mut text = String::new();
        loop {
            let Some(ch) = self.next() else {
                return Err("unterminated string".to_string());
            };
            match ch {
                '"' => return Ok(text),
                '\\' => {
                    let Some(escape) = self.next() else {
                        return Err("unterminated escape".to_string());
                    };
                    match escape {
                        '"' => text.push('"'),
                        '\\' => text.push('\\'),
                        '/' => text.push('/'),
                        'b' => text.push('\u{0008}'),
                        'f' => text.push('\u{000C}'),
                        'n' => text.push('\n'),
                        'r' => text.push('\r'),
                        't' => text.push('\t'),
                        'u' => {
                            let high = self.parse_hex4()?;
                            // Surrogate pairs combine; a lone surrogate decodes
                            // to U+FFFD without aborting the parse.
                            let code = if (0xd800..=0xdbff).contains(&high) {
                                if self.peek() == Some(&'\\')
                                    && self.chars.get(self.position + 1) == Some(&'u')
                                {
                                    let low_start = self.position + 2;
                                    if self
                                        .chars
                                        .get(low_start..low_start + 4)
                                        .is_some_and(|slice| {
                                            slice.iter().all(|c| c.is_ascii_hexdigit())
                                        })
                                    {
                                        self.position += 2;
                                        let low = self.parse_hex4()?;
                                        if (0xdc00..=0xdfff).contains(&low) {
                                            0x10000 + (u32::from(high - 0xd800) << 10)
                                                + u32::from(low - 0xdc00)
                                        } else {
                                            text.push(
                                                char::try_from(high as u32).unwrap_or('\u{fffd}'),
                                            );
                                            u32::from(low)
                                        }
                                    } else {
                                        u32::from(high)
                                    }
                                } else {
                                    u32::from(high)
                                }
                            } else {
                                u32::from(high)
                            };
                            text.push(char::try_from(code).unwrap_or('\u{fffd}'));
                        }
                        _ => return Err(format!("invalid escape \\{escape}")),
                    }
                }
                ch if ch <= '\u{001f}' => {
                    return Err(format!("unescaped control character at offset {}", self.position - 1));
                }
                _ => text.push(ch),
            }
        }
    }

    fn parse_hex4(&mut self) -> Result<u16, String> {
        let mut value: u16 = 0;
        for _ in 0..4 {
            let Some(ch) = self.next() else {
                return Err("unterminated \\u escape".to_string());
            };
            let digit = ch.to_digit(16).ok_or("invalid \\u escape")?;
            value = value * 16 + digit as u16;
        }
        Ok(value)
    }

    fn parse_number(&mut self) -> Result<Json, String> {
        let start = self.position;
        if self.peek() == Some(&'-') {
            self.position += 1;
        }
        match self.peek() {
            Some(&'0') => {
                self.position += 1;
            }
            Some(c) if c.is_ascii_digit() => {
                while matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
                    self.position += 1;
                }
            }
            _ => return Err(format!("invalid number at offset {start}")),
        }
        if self.peek() == Some(&'.') {
            self.position += 1;
            let mut fraction_digits = 0;
            while matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
                self.position += 1;
                fraction_digits += 1;
            }
            if fraction_digits == 0 {
                return Err(format!("invalid number at offset {start}"));
            }
        }
        if matches!(self.peek(), Some('e') | Some('E')) {
            self.position += 1;
            if matches!(self.peek(), Some('+') | Some('-')) {
                self.position += 1;
            }
            let mut exponent_digits = 0;
            while matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
                self.position += 1;
                exponent_digits += 1;
            }
            if exponent_digits == 0 {
                return Err(format!("invalid number at offset {start}"));
            }
        }
        let literal: String = self.chars[start..self.position].iter().collect();
        literal
            .parse::<f64>()
            .map(Json::Number)
            .map_err(|_| format!("invalid number at offset {start}"))
    }

    fn peek(&self) -> Option<&char> {
        self.chars.get(self.position)
    }

    fn next(&mut self) -> Option<char> {
        let ch = self.chars.get(self.position).copied();
        if ch.is_some() {
            self.position += 1;
        }
        ch
    }

    fn skip_whitespace(&mut self) {
        // QJsonDocument accepts JSON whitespace only. QString::trimmed uses
        // QChar::isSpace, but vertical tab, form feed and Unicode separators
        // are not legal separators in JSON documents.
        while self.peek().is_some_and(|ch| matches!(ch, ' ' | '\t' | '\r' | '\n')) {
            self.position += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_scalars_containers_and_numbers() {
        let value =
            Json::parse(r#"{"a":1, "b":"x", "c":[true,false,null,{"d":1.5e2}], "e":-0.5}"#).unwrap();
        assert!(value.get("a").is_double());
        assert_eq!(value.get("a").to_double(), 1.0);
        assert!(value.get("b").is_string());
        assert_eq!(value.get("b").as_str(), "x");
        assert!(value.get("c").is_array());
        assert_eq!(value.get("c").as_array()[2], Json::Null);
        assert_eq!(
            value.get("c").as_array()[3].get("d").to_double(),
            150.0,
        );
        assert!(value.get("missing").is_string() == false);
        assert_eq!(value.get("e").to_double(), -0.5);
        assert_eq!(value.keys(), vec!["a", "b", "c", "e"]);
    }

    #[test]
    fn booleans_are_not_doubles_and_missing_keys_are_undefined() {
        let value = Json::parse(r#"{"flag":true}"#).unwrap();
        assert!(!value.get("flag").is_double());
        assert!(!value.get("flag").is_string());
        assert!(!value.get("nope").is_string());
        assert!(!value.get("nope").is_object());
        assert_eq!(value.get("nope").to_string_value(), "");
        assert_eq!(value.get("nope").to_double(), 0.0);
    }

    #[test]
    fn escapes_and_surrogate_pairs_decode() {
        let value = Json::parse(r#""a\nb\\\"😀 Aé""#).unwrap();
        assert!(value.is_string());
        assert_eq!(value.as_str(), "a\nb\\\"😀 Aé");
        // Lone surrogate decodes to U+FFFD.
        let value = Json::parse(r#""\ud83d""#).unwrap();
        assert_eq!(value.as_str(), "\u{fffd}");
    }

    #[test]
    fn duplicate_keys_keep_last_and_errors_report() {
        let value = Json::parse(r#"{"a":1,"a":2}"#).unwrap();
        assert_eq!(value.get("a").to_double(), 2.0);
        assert!(Json::parse(r#"{"a":}"#).is_err());
        assert!(Json::parse(r#"[1,]"#).is_err());
        assert!(Json::parse(r#"1 2"#).is_err());
        assert!(Json::parse(r#""unterminated"#).is_err());
    }

    #[test]
    fn rejects_qchar_whitespace_outside_json_and_unescaped_controls() {
        for input in ["[ \u{000b} ]", "[\u{000c}]", "[\u{2028}]", "[\u{00a0}]",
                      "[\"x\u{0000}y\"]", "[\"x\ny\"]"] {
            assert!(Json::parse(input).is_err(), "QJsonDocument rejects {input:?}");
        }
        assert_eq!(Json::parse("[\"x\\ny\"]").unwrap(), Json::Array(vec![Json::string("x\ny")]));
    }
    #[test]
    fn qjsonobject_keys_compare_utf16_units_not_unicode_scalars() {
        let data = Json::parse(r#"{"\ue000":"bmp","\ud83d\ude00":"supplementary","a":"ascii"}"#).unwrap();
        assert_eq!(data.keys(), ["a", "😀", "\u{e000}"]);
    }

}

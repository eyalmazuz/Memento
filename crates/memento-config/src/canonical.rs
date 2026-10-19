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

//! The canonical store file format ("memento-config-store v1") and the
//! scalar codec shared by the canonical document decoder and the legacy
//! QSettings INI loader.
//!
//! A TOML-like, hand-implemented grammar chosen to make Memento's settings
//! diffable and self-describing while staying trivially hand-parsable with
//! `std` only:
//!
//! ```text
//! # memento-config-store v1
//! # revision: 3
//! # change-log:
//! #   rev 2: +1 ~1  [+ general/version, ~ search/autofit]
//!
//! [application]
//! language = 1
//! auto-update-check = false
//!
//! [search]
//! replace-with = ""
//! remove-regex = "^[,;#]+"
//! ```
//!
//! Grammar:
//!
//! * lines are separated by `\n`; the writer emits `\n`, the reader
//!   tolerates a `\r` before it (CRLF files) and a UTF-8 BOM at the start;
//! * `#` at the start of a line starts a comment; the first comment block is
//!   the header: the magic line, the `revision:` line and the change-log
//!   rows (kept verbatim so generated files are diffable byte-for-byte);
//! * `[section-header]` lines open a section; the header text is the full
//!   group path (`keybind/profiles/Default`); characters that are illegal
//!   inside a header (`[`, `]`, `=`, newline, carriage return, `%`) are
//!   percent-encoded as `%XX` over the UTF-8 bytes (lowercase hex), and a
//!   literal `%` is always emitted as `%25` so encoding is unambiguous.
//!   The same rule applies to key names;
//! * `key = value` lines store one entry per line; values are encoded by
//!   [`encode_value`];
//! * rows are emitted sorted: group paths lexicographically, then keys
//!   lexicographically — independent of insertion or build order.
//!
//! Scalar value grammar ([`encode_value`]):
//!
//! * bools `true` / `false`;
//! * ints in decimal (`-5`), also read back from the C++-side hex forms
//!   (`0x1a`);
//! * reals in shortest round-trip decimal, always carrying a fractional
//!   part or exponent (`0.05`, `15.0`) so a float can never parse back as
//!   an int;
//! * strings, quoted only when needed ([`needs_quotes`]): empty strings, or
//!   values with leading/trailing whitespace, or containing any of
//!   `" \ \n \r \t # ; = [ ] { } ,` or control characters, or starting with
//!   `[` / `{`; inside quotes the sequences `\\`, `\"`, `\n`, `\r`,
//!   `\t` are the only escapes (canonical mode rejects any other);
//!   unquoted strings never contain a backslash, so no escape ambiguity
//!   exists;
//! * lists as `[a, b, c]` (string lists quote each element, int lists
//!   don't); an empty list is `[]` and only decodes per the *declared* key
//!   type;
//! * maps as `{ "key" = "value", "k2" = "v2" }` — keys are always quoted;
//!   an empty map is `{}`.

use std::collections::BTreeMap;

use crate::error::ConfigError;
use crate::value::{Value, ValueType};

/// First header line of every canonical store file.
pub const FORMAT_MAGIC: &str = "# memento-config-store v1";

/// Prefix introducing the revision line.
pub const REVISION_PREFIX: &str = "# revision: ";

/// Prefix introducing a change-log line.
pub const CHANGELOG_PREFIX: &str = "# change-log:";

/// Prefix introducing one change-log entry row.
pub const CHANGELOG_ROW_PREFIX: &str = "#   ";

// ---------------------------------------------------------------- encoding

/// Strings are quoted (see the module header) when they are empty, carry
/// leading/trailing ASCII whitespace, could be confused with a container /
/// numeric token, or contain any special character.
fn needs_quotes(s: &str) -> bool {
    if s.is_empty() {
        return true;
    }
    let bytes = s.as_bytes();
    let first = bytes[0];
    let last = bytes[bytes.len() - 1];
    if matches!(first, b' ' | b'\t') || matches!(last, b' ' | b'\t') {
        return true;
    }
    if matches!(first, b'[' | b'{' | b'+' | b'-' | b'#') {
        return true;
    }
    bytes.iter().any(|&b| {
        matches!(
            b,
            b'"'
                | b'\\'
                | b'\n'
                | b'\r'
                | b'\t'
                | b'#'
                | b';'
                | b'='
                | b'['
                | b']'
                | b'{'
                | b'}'
                | b','
        ) || b < 0x20
            || b == 0x7f
            || !b.is_ascii()
    })
}

/// Encodes a string value: enters quotes when [`needs_quotes`] says so.
pub fn escape_string(s: &str) -> String {
    if !needs_quotes(s) {
        return s.to_owned();
    }
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Formats an `f64` in the canonical real notation (shortest round-trip on
/// `Display`, always distinguishing itself from an int).
pub fn real_string(value: f64) -> Result<String, ConfigError> {
    if !value.is_finite() {
        return Err(ConfigError::Format {
            message: format!(
                "cannot store the non-finite real {value:?} (Memento settings are finite)"
            ),
        });
    }
    let mut text = format!("{value}");
    if !text.contains(['.', 'e', 'E']) {
        text.push_str(".0");
    }
    Ok(text)
}

/// Encodes a [`Value`] into a canonical value string.
pub fn encode_value(value: &Value) -> Result<String, ConfigError> {
    Ok(match value {
        Value::Bool(v) => {
            if *v {
                "true".to_owned()
            } else {
                "false".to_owned()
            }
        }
        Value::Int(v) => v.to_string(),
        Value::Real(v) => real_string(*v)?,
        Value::Text(text) => escape_string(text),
        Value::Strings(items) => {
            let parts: Vec<String> = items.iter().map(String::as_str).map(escape_string).collect();
            format!("[{}]", parts.join(", "))
        }
        Value::Ints(items) => {
            let parts: Vec<String> = items.iter().map(ToString::to_string).collect();
            format!("[{}]", parts.join(", "))
        }
        Value::Map(map) => {
            let parts: Vec<String> = map
                .iter()
                .map(|(k, v)| {
                    let key = escape_string(k);
                    let value = escape_string(v);
                    format!("{key} = {value}")
                })
                .collect();
            format!("{{{}}}", parts.join(", "))
        }
    })
}

// ---------------------------------------------------------------- decoding

/// Boolean decoding. Accepts the QSettings spellings `true`/`false`
/// (case-insensitive) and, for legacy values entered by hand, integral
/// values (`0` == false, any non-zero integer == true — matches
/// `QVariant::toBool`).
pub fn decode_bool(raw: &str) -> Result<bool, String> {
    let trimmed = raw.trim();
    if trimmed.eq_ignore_ascii_case("true") {
        return Ok(true);
    }
    if trimmed.eq_ignore_ascii_case("false") {
        return Ok(false);
    }
    if let Ok(n) = decode_int(trimmed) {
        return Ok(n != 0);
    }
    Err(format!("not a boolean: {raw:?}"))
}

/// Integer decoding. Accepts decimal with an optional sign plus the
/// C++-side hex forms (`0x1a`, signed variants included), mirroring the
/// integer forms `QSettings` INI accepts.
pub fn decode_int(raw: &str) -> Result<i64, String> {
    let trimmed = raw.trim();
    let (sign, digits) = if let Some(rest) = trimmed.strip_prefix('-') {
        (-1i64, rest)
    } else if let Some(rest) = trimmed.strip_prefix('+') {
        (1i64, rest)
    } else {
        (1i64, trimmed)
    };
    if let Some(hex) = digits.strip_prefix("0x").or_else(|| digits.strip_prefix("0X")) {
        if hex.is_empty() || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(format!("not an integer: {raw:?}"));
        }
        let magnitude =
            u64::from_str_radix(hex, 16).map_err(|e| format!("not an integer: {raw:?}: {e}"))?;
        return check_sign_magnitude(sign, magnitude, raw);
    }
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err(format!("not an integer: {raw:?}"));
    }
    let magnitude = digits
        .parse::<u64>()
        .map_err(|e| format!("not an integer: {raw:?}: {e}"))?;
    check_sign_magnitude(sign, magnitude, raw)
}

fn check_sign_magnitude(sign: i64, magnitude: u64, raw: &str) -> Result<i64, String> {
    if sign >= 0 {
        i64::try_from(magnitude).map_err(|_| format!("integer out of range: {raw:?}"))
    } else {
        i64::try_from(magnitude)
            .ok()
            .and_then(|m| m.checked_neg())
            .ok_or_else(|| format!("integer out of range: {raw:?}"))
    }
}

/// Real decoding (`f64`). `nan`/`inf` literals are rejected: Memento stores
/// finite settings only.
pub fn decode_real(raw: &str) -> Result<f64, String> {
    let value: f64 = raw
        .trim()
        .parse()
        .map_err(|e| format!("not a real number: {raw:?}: {e}"))?;
    if !value.is_finite() {
        return Err(format!("not a finite real number: {raw:?}"));
    }
    Ok(value)
}

/// Splits the body of a bracketed container on top-level commas (commas
/// inside quoted strings are protected).
pub fn split_elements(body: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    let mut escaped = false;
    for c in body.chars() {
        if escaped {
            current.push(c);
            escaped = false;
            continue;
        }
        match c {
            '\\' => {
                current.push(c);
                escaped = true;
            }
            '"' => {
                in_quotes = !in_quotes;
                current.push(c);
            }
            ',' if !in_quotes => {
                out.push(current.trim().to_owned());
                current.clear();
            }
            _ => current.push(c),
        }
    }
    out.push(current.trim().to_owned());
    out
}

/// Unescapes the body of a quoted string. In `strict` mode only the five
/// documented escapes exist; otherwise an unknown escape keeps the escaped
/// character verbatim (legacy INI assumption, see `legacy_ini.rs`).
fn unescape_body(body: &str, strict: bool) -> String {
    let mut out = String::with_capacity(body.len());
    let mut chars = body.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.peek().copied() {
                Some(next) => {
                    let decoded = match (strict, next) {
                        (_, '\\') => '\\',
                        (_, '"') => '"',
                        (_, 'n') => '\n',
                        (_, 'r') => '\r',
                        (_, 't') => '\t',
                        (false, other) => other,
                        // Cannot happen: callers only run in strict mode on
                        // engine-produced data, but a defensively unknown
                        // escape becomes a replacement char in canon mode.
                        (true, _) => '\u{FFFD}',
                    };
                    chars.next();
                    out.push(decoded);
                }
                // A dangling backslash is kept literal in both modes.
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Parses `"…"`; returns the decoded string, or `None` iff the input does
/// not start with `"`. In strict mode unknown escapes fail; the caller
/// surfaces that as a type/format error. The closing quote must terminate
/// the input (only trailing spaces/tabs may follow).
fn parse_quoted(raw: &str, strict: bool) -> Result<Option<String>, String> {
    if !raw.starts_with('"') {
        return Ok(None);
    }
    let mut out = String::new();
    let mut chars = raw.chars().skip(1);
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                // closing quote; only trailing spaces/tabs may remain
                let trailer: String = chars.collect();
                if !trailer.bytes().all(|b| b == b' ' || b == b'\t') {
                    return Err(format!(
                        "unexpected characters after the closing quote: {trailer:?}"
                    ));
                }
                return Ok(Some(out));
            }
            '\\' => {
                match chars.next() {
                    Some('n') => out.push('\n'),
                    Some('r') => out.push('\r'),
                    Some('t') => out.push('\t'),
                    Some('"') => out.push('"'),
                    Some('\\') => out.push('\\'),
                    Some(other) => {
                        if strict {
                            return Err(format!("unknown escape sequence '\\{other}'"));
                        }
                        out.push(other);
                    }
                    None => out.push('\\'), // dangling backslash kept literal
                }
            }
            _ => out.push(c),
        }
    }
    Err("unterminated quoted string".to_owned())
}

/// Decodes a text value: either a quoted string or a bare token.
pub fn decode_text(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim();
    match parse_quoted(trimmed, true)? {
        Some(text) => Ok(text),
        None => {
            if trimmed.contains('\\') {
                Err(format!(
                    "unescaped backslash in unquoted text {trimmed:?} (must be quoted)"
                ))
            } else {
                Ok(trimmed.to_owned())
            }
        }
    }
}

/// Decodes the string element of a list (plain or quoted).
fn decode_element_string(elem: &str) -> String {
    match parse_quoted(elem.trim(), false) {
        Ok(Some(text)) => text,
        Ok(None) => unescape_body(elem.trim(), false),
        Err(_) => elem.trim().to_owned(),
    }
}

fn decode_int_element(elem: &str) -> Result<i64, String> {
    decode_int(elem)
}

/// Decodes a typed canonical value for a declared key type.
///
/// Type-directed decoding is what makes empty containers unambiguous: `[]`
/// decodes to the declared list variant and `{}` to [`Value::Map`].
pub fn decode_value(raw: &str, ty: ValueType) -> Result<Value, String> {
    let trimmed = raw.trim();
    if trimmed == "@Invalid()" {
        return match ty {
            ValueType::Bool => Ok(Value::Bool(false)),
            ValueType::Int => Ok(Value::Int(0)),
            ValueType::Real => Ok(Value::Real(0.0)),
            ValueType::Text => Ok(Value::Text(String::new())),
            ValueType::Strings => Ok(Value::Strings(Vec::new())),
            ValueType::Ints => Ok(Value::Ints(Vec::new())),
            ValueType::Map => Ok(Value::Map(BTreeMap::new())),
        };
    }
    match ty {
        ValueType::Bool => decode_bool(trimmed).map(Value::Bool),
        ValueType::Int => decode_int(trimmed).map(Value::Int),
        ValueType::Real => decode_real(trimmed).map(Value::Real),
        ValueType::Text => decode_text(trimmed).map(Value::Text),
        ValueType::Strings => decode_strings(trimmed).map(Value::Strings),
        ValueType::Ints => decode_ints(trimmed).map(Value::Ints),
        ValueType::Map => decode_map(trimmed).map(Value::Map),
    }
}

fn decode_qvariant_payload(raw: &str) -> Option<Vec<u8>> {
    let trimmed = raw.trim();
    if !trimmed.starts_with("@Variant(") || !trimmed.ends_with(')') {
        return None;
    }
    let inner = &trimmed["@Variant(".len()..trimmed.len() - 1];
    let bytes = inner.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 1 < bytes.len() {
            i += 1;
            match bytes[i] {
                b'0' => out.push(0),
                b'a' => out.push(7),
                b'b' => out.push(8),
                b't' => out.push(9),
                b'n' => out.push(10),
                b'v' => out.push(11),
                b'f' => out.push(12),
                b'r' => out.push(13),
                b'\\' => out.push(b'\\'),
                b'x' => {
                    i += 1;
                    let mut val = 0u8;
                    let mut digits = 0;
                    while i < bytes.len() && digits < 2 && (bytes[i] as char).is_ascii_hexdigit() {
                        let d = (bytes[i] as char).to_digit(16).unwrap() as u8;
                        val = (val << 4) | d;
                        digits += 1;
                        i += 1;
                    }
                    if digits > 0 {
                        out.push(val);
                    }
                    continue;
                }
                other => out.push(other),
            }
        } else {
            out.push(bytes[i]);
        }
        i += 1;
    }
    Some(out)
}

fn decode_qvariant_strings(raw: &str) -> Option<Vec<String>> {
    let payload = decode_qvariant_payload(raw)?;
    if payload.len() < 8 {
        return None;
    }
    let type_id = u32::from_be_bytes(payload[0..4].try_into().ok()?);
    let count = u32::from_be_bytes(payload[4..8].try_into().ok()?) as usize;
    let mut pos = 8;
    let minimum_item_bytes = match type_id {
        9 => 8,  // QVariantList: inner type id plus a QString length.
        11 => 4, // QStringList: QString length.
        _ => return None,
    };
    // The INI file is user-controlled: reject impossible counts before allocating.
    if count > (payload.len() - pos) / minimum_item_bytes {
        return None;
    }
    let mut result = Vec::with_capacity(count);
    for _ in 0..count {
        if pos >= payload.len() {
            return None;
        }
        if type_id == 9 {
            // QVariantList: each item has inner type_id
            if pos + 4 > payload.len() {
                return None;
            }
            let inner_type = u32::from_be_bytes(payload[pos..pos + 4].try_into().ok()?);
            pos += 4;
            if inner_type == 10 {
                // QString
                if pos + 4 > payload.len() {
                    return None;
                }
                let byte_len = u32::from_be_bytes(payload[pos..pos + 4].try_into().ok()?);
                pos += 4;
                if byte_len == 0xffff_ffff {
                    result.push(String::new());
                } else {
                    let bl = byte_len as usize;
                    if byte_len % 2 != 0 || byte_len as usize > payload.len().saturating_sub(pos) {
                        return None;
                    }
                    let u16_chars: Vec<u16> = payload[pos..pos + bl]
                        .chunks_exact(2)
                        .map(|chunk| u16::from_be_bytes([chunk[0], chunk[1]]))
                        .collect();
                    result.push(String::from_utf16_lossy(&u16_chars));
                    pos += bl;
                }
            } else {
                return None;
            }
        } else if type_id == 11 {
            // QStringList
            if pos + 4 > payload.len() {
                return None;
            }
            let byte_len = u32::from_be_bytes(payload[pos..pos + 4].try_into().ok()?);
            pos += 4;
            if byte_len == 0xffff_ffff {
                result.push(String::new());
            } else {
                let bl = byte_len as usize;
                if byte_len % 2 != 0 || byte_len as usize > payload.len().saturating_sub(pos) {
                    return None;
                }
                let u16_chars: Vec<u16> = payload[pos..pos + bl]
                    .chunks_exact(2)
                    .map(|chunk| u16::from_be_bytes([chunk[0], chunk[1]]))
                    .collect();
                result.push(String::from_utf16_lossy(&u16_chars));
                pos += bl;
            }
        } else {
            return None;
        }
    }
    (pos == payload.len()).then_some(result)
}

fn decode_qvariant_ints(raw: &str) -> Option<Vec<i64>> {
    let payload = decode_qvariant_payload(raw)?;
    if payload.len() < 8 {
        return None;
    }
    let type_id = u32::from_be_bytes(payload[0..4].try_into().ok()?);
    if type_id != 9 {
        return None;
    }
    let count = u32::from_be_bytes(payload[4..8].try_into().ok()?) as usize;
    let mut pos = 8;
    if count > (payload.len() - pos) / 8 {
        return None;
    }
    let mut result = Vec::with_capacity(count);
    for _ in 0..count {
        if pos + 4 > payload.len() {
            return None;
        }
        let inner_type = u32::from_be_bytes(payload[pos..pos + 4].try_into().ok()?);
        pos += 4;
        if inner_type == 2 {
            // int (i32)
            if pos + 4 > payload.len() {
                return None;
            }
            let val = i32::from_be_bytes(payload[pos..pos + 4].try_into().ok()?);
            pos += 4;
            result.push(val as i64);
        } else if inner_type == 3 {
            // uint (u32)
            if pos + 4 > payload.len() {
                return None;
            }
            let val = u32::from_be_bytes(payload[pos..pos + 4].try_into().ok()?);
            pos += 4;
            result.push(val as i64);
        } else if inner_type == 4 {
            // longlong (i64)
            if pos + 8 > payload.len() {
                return None;
            }
            let val = i64::from_be_bytes(payload[pos..pos + 8].try_into().ok()?);
            pos += 8;
            result.push(val);
        } else {
            return None;
        }
    }
    (pos == payload.len()).then_some(result)
}

/// Decodes a string list. Brackets are the canonical form; legacy
/// comma-separated QSettings values are accepted on read.
pub fn decode_strings(raw: &str) -> Result<Vec<String>, String> {
    let trimmed = raw.trim();
    if trimmed == "[]" || trimmed == "@Invalid()" || trimmed.is_empty() {
        return Ok(Vec::new());
    }
    if let Some(items) = decode_qvariant_strings(trimmed) {
        return Ok(items);
    }
    if trimmed.starts_with('[') && trimmed.ends_with(']') {
        let body = &trimmed[1..trimmed.len() - 1];
        return Ok(split_elements(body)
            .into_iter()
            .map(|e| decode_element_string(&e))
            .collect());
    }
    if let Some(inner) = parse_quoted(trimmed, false)? {
        // A comma inside quotes is part of the one QString, not a separator.
        return Ok(vec![inner]);
    }
    Ok(split_elements(trimmed)
        .into_iter()
        .map(|e| decode_element_string(&e))
        .collect())
}

pub fn decode_ints(raw: &str) -> Result<Vec<i64>, String> {
    let trimmed = raw.trim();
    if trimmed == "[]" || trimmed == "@Invalid()" || trimmed.is_empty() || trimmed == "\"\"" {
        return Ok(Vec::new());
    }
    if let Some(items) = decode_qvariant_ints(trimmed) {
        return Ok(items);
    }
    let body = if trimmed.starts_with('[') && trimmed.ends_with(']') {
        &trimmed[1..trimmed.len() - 1]
    } else {
        trimmed
    };
    split_elements(body)
        .into_iter()
        .filter(|e| !e.is_empty())
        .map(|e| decode_int_element(&e))
        .collect()
}

fn decode_map(raw: &str) -> Result<BTreeMap<String, String>, String> {
    let trimmed = raw.trim();
    if trimmed == "{}" {
        return Ok(BTreeMap::new());
    }
    if !trimmed.starts_with('{') || !trimmed.ends_with('}') {
        return Err(format!("not a map: {raw:?}"));
    }
    let body = &trimmed[1..trimmed.len() - 1];
    let mut map = BTreeMap::new();
    for pair in split_elements(body) {
        if pair.is_empty() {
            continue;
        }
        let eq = pair
            .find('=')
            .ok_or_else(|| format!("map entry without '=': {pair:?}"))?;
        let key_part = pair[..eq].trim();
        let value_part = pair[eq + 1..].trim();
        let key = match parse_quoted(key_part, false)? {
            Some(text) => unescape_body(&text, false),
            None => key_part.to_owned(),
        };
        map.insert(key, decode_element_string(value_part));
    }
    Ok(map)
}

/// Best-effort decode for keys that are not part of the key model. Never
/// fails: anything it cannot classify stays a [`Value::Text`]. Used by the
/// legacy INI loader; the canonical loader keeps the raw bytes for unknown
/// keys verbatim so they round-trip byte-identically regardless of typing.
pub fn decode_value_inferred(raw: &str) -> Value {
    let trimmed = raw.trim();
    if trimmed == "@Invalid()" {
        return Value::Text(String::new());
    }
    if let Some(items) = decode_qvariant_strings(trimmed) {
        return Value::Strings(items);
    }
    if let Some(items) = decode_qvariant_ints(trimmed) {
        return Value::Ints(items);
    }
    if trimmed.starts_with('[') && trimmed.ends_with(']') && trimmed.len() > 2 {
        let body = &trimmed[1..trimmed.len() - 1];
        let elements = split_elements(body);
        let all_ints = !elements.is_empty() && elements.iter().all(|e| decode_int(e).is_ok());
        if all_ints {
            return Value::Ints(
                elements
                    .iter()
                    .filter_map(|e| decode_int(e).ok())
                    .collect(),
            );
        }
        let items: Vec<String> = elements
            .iter()
            .filter(|e| !e.is_empty())
            .map(|e| decode_element_string(e))
            .collect();
        return Value::Strings(items);
    }
    if trimmed.starts_with('{') && trimmed.ends_with('}') {
        if let Ok(map) = decode_map(trimmed) {
            return Value::Map(map);
        }
    }
    if trimmed.starts_with('"') {
        if let Ok(Some(text)) = parse_quoted(trimmed, false) {
            return Value::Text(text);
        }
    }
    if decode_int(trimmed).is_ok() {
        return Value::Int(decode_int(trimmed).expect("checked above"));
    }
    if trimmed.eq_ignore_ascii_case("true") || trimmed.eq_ignore_ascii_case("false") {
        return Value::Bool(decode_bool(trimmed).expect("checked above"));
    }
    if trimmed.contains(['.', 'e', 'E']) {
        if let Ok(v) = decode_real(trimmed) {
            return Value::Real(v);
        }
    }
    Value::Text(trimmed.to_owned())
}

// ------------------------------------------------------- percent encoding

/// Percent-encodes a section header or key name: every byte that is not a
/// plain ASCII `0-9 a-z A-Z _ - . /` byte, plus `%` itself, is written as
/// `%XX` with uppercase hex over the UTF-8 bytes.
pub fn percent_encode_component(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for &b in name.as_bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.' | b'/') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Tolerant inverse of [`percent_encode_component`]: unknown `%XX` sequences
/// are kept literally.
pub fn percent_decode_component(name: &str) -> String {
    let bytes = name.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && bytes[i + 1].is_ascii_hexdigit()
            && bytes[i + 2].is_ascii_hexdigit()
        {
            let hi = (bytes[i + 1] as char).to_digit(16).expect("hex digit");
            let lo = (bytes[i + 2] as char).to_digit(16).expect("hex digit");
            out.push((hi * 16 + lo) as u8);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

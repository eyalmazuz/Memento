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

//! Interop loader for the legacy *QSettings / IniFormat* configuration
//! Memento currently writes (`NativeFormat` on Unix is `IniFormat`).
//!
//! This module is the migration path off Qt: it reads an existing
//! `memento.conf` (or the app's default `QSettings` file) and produces the
//! same raw map the canonical store uses, so values are preserved and the
//! file can be rewritten into the canonical format.
//!
//! Grammar implemented — plus conservatively chosen readings where Qt's
//! behaviour is undocumented (each deviation is listed in the module
//! `GRAMMAR_ASSUMPTIONS` text):
//!
//! * `[section]` headings open groups; the whole *nested* path is written
//!   in one heading with `/` separators (`[keybind/profiles/Default]`);
//! * headings and keys use QSettings' `%XX` (Latin-1 code unit) or `%UXXXX`
//!   (UTF-16 code unit) escapes; `/` is escaped as `\` because it separates
//!   group paths; e.g. `Font%2DDirection` decodes to `Font-Direction`;
//! * QSettings **arrays** (`beginWriteArray` / `setArrayIndex`) surface in
//!   this module's model as the heading group plus numeric `\`-prefixed
//!   keys (`[audio-srcs]` + `1\name=…`), which are re-folded into indexed
//!   group paths (`audio-srcs/1`);
//! * values: leading/trailing whitespace around `=` is discarded; a value
//!   starting with `"` is a quoted string supporting the escapes
//!   `\\ \" \n \r \t`; if the quoted string is not terminated on its own
//!   row the parser continues onto following rows (QSettings multi-row
//!   values);
//! * `#` / `;` full-line comments and `[`-headings are the only structural
//!   rows; blank rows are skipped; a UTF-8 BOM is tolerated;
//! * unquoted values are taken literally to end of row, boolean/
//!   integer/real forms are recognised when the *declared* key type
//!   asks for it (`true`/`false` case-insensitive, decimal/hex integers);
//! * root keys (rows before any heading) map to the store's synthetic
//!   `general` section.

use std::collections::BTreeMap;

use crate::canonical::{decode_value, decode_value_inferred, encode_value};
use crate::error::{ConfigError, ConfigResult};
use crate::keys::{lookup, GENERAL_GROUP};
use crate::value::{Value, ValueType};

/// Grammar assumptions where Qt's behaviour is explicitly unknowable are
/// listed with the [`GRAMMAR_ASSUMPTIONS`] constant so tests and reviewers
/// can anchor on them.
///
/// # Notes on assumed deviations
///
/// * Semicolon-family comments only count at the *start of a row* (after
///   whitespace). Nothing in `qsettings_ini.cpp`'s documented behaviour
///   promises anything else.
/// * Trailing whitespace of values IS discarded (QSettings trims the row);
///   leading whitespace of the value is only significant when the value is
///   a quoted string's pre-quote context. Assumption: QSettings trims
///   around `=`.
/// * Escape handling inside *unquoted* values: also honoured
///   (`\n` → newline etc.) — before quoting was mandatory, QSettings'
///   writer did escape backslashes in unquoted values too.
/// * Unknown numeric escape sequences like `\q` keep the `q` verbatim.
/// * A quoted value whose closing quote never arrives consumes following
///   rows (joined with `\n` in this module) until a closing quote is found.
/// * The numeric `\`-array form is only recognised when the digits sit at
///   the start of a key (`1\name`).
#[derive(Debug, Clone, Copy)]
pub struct GrammarAssumptions;

/// Marker constant so the assumption notes land in the public API docs.
pub const GRAMMAR_ASSUMPTIONS: GrammarAssumptions = GrammarAssumptions;

/// A parsed legacy INI document: raw sections keyed by resolved group path
/// plus warnings collected along the way (unknown keys, type mismatches
/// against the declared key model, structure that could not be decoded).
#[derive(Debug, Default, Clone)]
pub struct LegacyIni {
    pub sections: BTreeMap<String, BTreeMap<String, String>>,
    pub warnings: Vec<String>,
}

/// Parses the legacy INI text into raw canonical-encoded sections.
pub fn parse_legacy_ini(text: &str) -> ConfigResult<LegacyIni> {
    let mut state = Parser::default();
    let logical_rows = merge_multiline_rows(text)?;
    drop_rows(logical_rows, &mut state)?;
    Ok(state.into_ini())
}

#[derive(Default)]
struct Parser {
    sections: BTreeMap<String, BTreeMap<String, String>>,
    warnings: Vec<String>,
    current_section: Option<String>,
}

impl Parser {
    fn warn(&mut self, message: String) {
        self.warnings.push(message);
    }

    fn set_raw(&mut self, group: &str, key: &str, raw: String) {
        let entry = self.sections.entry(group.to_owned()).or_default();
        entry.insert(key.to_owned(), raw);
    }

    fn into_ini(self) -> LegacyIni {
        LegacyIni {
            sections: self.sections,
            warnings: self.warnings,
        }
    }
}

/// Splits into logical rows; quoted values whose quote never closes on
/// their row consume following rows (joined with `\n`).
fn merge_multiline_rows(text: &str) -> ConfigResult<Vec<String>> {
    let mut rows = Vec::new();
    let raw_rows: Vec<&str> = text
        .lines()
        .map(|row| row.strip_suffix('\r').unwrap_or(row))
        .collect();
    let mut idx = 0;
    while idx < raw_rows.len() {
        let mut row = raw_rows[idx].to_owned();
        if idx == 0 {
            row = row.strip_prefix('\u{FEFF}').map(str::to_owned).unwrap_or(row);
        }
        if needs_continuation(&row) {
            let start_line = idx + 1;
            let mut j = idx + 1;
            while j < raw_rows.len() {
                let next = raw_rows[j];
                row.push('\n');
                row.push_str(next);
                idx = j;
                if !needs_continuation(&row) {
                    break;
                }
                j += 1;
            }
            if needs_continuation(&row) {
                return Err(ConfigError::Parse {
                    file: "legacy-ini",
                    line: start_line,
                    message: "unterminated quoted value (no closing quote)".to_owned(),
                });
            }
        }
        rows.push(row);
        idx += 1;
    }
    Ok(rows)
}

/// True when the value part of a `key=value` row opens with `"` and does
/// not contain the closing quote.
fn needs_continuation(row: &str) -> bool {
    let find_eq = match row.find('=') {
        Some(pos) => pos,
        None => return false,
    };
    let value = row[find_eq + 1..].trim_start();
    if !value.starts_with('"') {
        return false;
    }
    // unescaped quotes count
    let mut escaped = false;
    let mut count = 0usize;
    for c in value.chars() {
        if c == '\\' && !escaped {
            escaped = true;
            continue;
        }
        let was_escaped = escaped;
        escaped = false;
        if c == '"' && !was_escaped {
            count += 1;
        }
    }
    count % 2 == 1
}

/// Walks the logical rows and populates the parser.
fn drop_rows(rows: Vec<String>, state: &mut Parser) -> ConfigResult<()> {
    for (index, row) in rows.iter().enumerate() {
        let line = index + 1;
        let trimmed = row.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed.starts_with('#') || trimmed.starts_with(';') {
            continue; // comment row
        }
        if trimmed.starts_with('[') && !trimmed.contains('=') {
            let Some(end) = trimmed.find(']') else {
                return Err(ConfigError::Parse {
                    file: "legacy-ini",
                    line,
                    message: "unterminated section heading".to_owned(),
                });
            };
            let heading = trimmed[1..end].trim();
            let section = decode_qt_key(heading);
            state.current_section = Some(if heading.eq_ignore_ascii_case("general") {
                GENERAL_GROUP.to_owned()
            } else {
                section.trim_matches('/').to_owned()
            });
            continue;
        }
        let Some(eq) = trimmed.find('=') else {
            state.warn(format!(
                "line {line}: ignored stray row (no '='): {trimmed:?}"
            ));
            continue;
        };
        let raw_key = decode_qt_key(trimmed[..eq].trim());
        let raw_value = trimmed[eq + 1..].trim_end().to_owned();
        if raw_key.is_empty() {
            state.warn(format!("line {line}: ignored empty key"));
            continue;
        }
        let (group, key) = resolve_key_coordinate(state.current_section.as_deref(), &raw_key);
        decode_and_store(state, &group, &key, &raw_value, line);
    }
    Ok(())
}

/// QSettings uses backslashes as path delimiters in INI keys. Both arrays
/// (`1\name`) and ordinary nested groups (`profiles\Default\card-add`)
/// use the same spelling, so the rightmost component is always the key.
fn resolve_key_coordinate(current_section: Option<&str>, raw_key: &str) -> (String, String) {
    let group = current_section.unwrap_or(GENERAL_GROUP);
    if let Some((nested, key)) = raw_key.rsplit_once('/') {
        let nested = nested.trim_matches('/');
        if !nested.is_empty() {
            return (format!("{group}/{nested}"), key.to_owned());
        }
    }
    (group.to_owned(), raw_key.to_owned())
}

/// Qt's percent escapes are Unicode scalar values (`%XX` as Latin-1 and
/// `%UXXXX` as UTF-16 code units), not UTF-8 byte escapes.
fn decode_qt_key(raw: &str) -> String {
    let mut decoded = Vec::with_capacity(raw.encode_utf16().count());
    let mut chars = raw.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => decoded.push('/' as u16),
            '%' => {
                let wide = chars.peek() == Some(&'U');
                let digits = if wide { 4 } else { 2 };
                let mut trial = chars.clone();
                if wide {
                    trial.next();
                }
                let hex: String = trial.by_ref().take(digits).collect();
                if hex.len() == digits && hex.bytes().all(|b| b.is_ascii_hexdigit()) {
                    let value = u16::from_str_radix(&hex, 16).expect("validated hex digits");
                    if wide {
                        chars.next();
                    }
                    for _ in 0..digits {
                        chars.next();
                    }
                    decoded.push(value);
                } else {
                    decoded.push('%' as u16);
                }
            }
            _ => {
                let mut encoded = [0; 2];
                decoded.extend_from_slice(c.encode_utf16(&mut encoded));
            }
        }
    }
    String::from_utf16_lossy(&decoded)
}

fn decode_and_store(
    state: &mut Parser,
    group: &str,
    key: &str,
    raw_value: &str,
    line: usize,
) {
    let declared = lookup(group, key).map(|key| key.value_type(&Default::default()));
    decode_typed(state, group, key, raw_value, declared, line);
}

/// Applies QSettings' INI string escapes before the value is re-encoded in
/// the canonical format. This is separate from the stricter canonical codec.
fn decode_qt_ini_text(raw: &str) -> String {
    let raw = raw.trim();
    let bytes = raw.as_bytes();
    let mut out = String::with_capacity(raw.len());
    let mut pos = 0;
    let mut quoted = false;

    while pos < bytes.len() {
        match bytes[pos] {
            b'"' => {
                quoted = !quoted;
                pos += 1;
                if !quoted {
                    while bytes
                        .get(pos)
                        .is_some_and(|&byte| byte == b' ' || byte == b'\t')
                    {
                        pos += 1;
                    }
                }
            }
            b'\\' => {
                pos += 1;
                let Some(&escaped) = bytes.get(pos) else {
                    break;
                };
                pos += 1;
                let decoded = match escaped {
                    b'a' => Some('\u{0007}'),
                    b'b' => Some('\u{0008}'),
                    b'f' => Some('\u{000c}'),
                    b'n' => Some('\n'),
                    b'r' => Some('\r'),
                    b't' => Some('\t'),
                    b'v' => Some('\u{000b}'),
                    b'"' => Some('"'),
                    b'?' => Some('?'),
                    b'\'' => Some('\''),
                    b'\\' => Some('\\'),
                    b'x' => {
                        let start = pos;
                        let mut value = 0u16;
                        while let Some(&digit) = bytes.get(pos) {
                            let Some(digit) = (digit as char).to_digit(16) else {
                                break;
                            };
                            value = value.wrapping_shl(4) | digit as u16;
                            pos += 1;
                        }
                        (pos != start).then(|| char::from_u32(value as u32).unwrap_or('\u{fffd}'))
                    }
                    b'0'..=b'7' => {
                        let mut value = (escaped - b'0') as u16;
                        while let Some(&digit) = bytes.get(pos) {
                            if !(b'0'..=b'7').contains(&digit) {
                                break;
                            }
                            value = value.wrapping_shl(3) | (digit - b'0') as u16;
                            pos += 1;
                        }
                        Some(char::from_u32(value as u32).unwrap_or('\u{fffd}'))
                    }
                    b'\n' | b'\r' => {
                        if bytes.get(pos).is_some_and(|&next| {
                            matches!(next, b'\n' | b'\r') && next != escaped
                        }) {
                            pos += 1;
                        }
                        None
                    }
                    _ => None,
                };
                if let Some(c) = decoded {
                    out.push(c);
                }
            }
            _ => {
                let c = raw[pos..].chars().next().expect("position is within the string");
                out.push(c);
                pos += c.len_utf8();
            }
        }
    }
    out
}

fn decode_declared_value(raw: &str, ty: ValueType) -> Result<Value, String> {
    let trimmed = raw.trim();
    if ty == ValueType::Text
        && trimmed != "@Invalid()"
        && !trimmed.starts_with("@Variant(")
    {
        return Ok(Value::Text(decode_qt_ini_text(trimmed)));
    }
    decode_value(raw, ty)
}

/// Decodes one raw value using the declared key type when available.
/// Everything decodes into the canonical raw form regardless: unknown keys
/// are preserved, and a broken value for a known key warns and keeps the
/// raw text.
fn decode_typed(
    state: &mut Parser,
    group: &str,
    key: &str,
    raw_value: &str,
    declared: Option<ValueType>,
    line: usize,
) {
    let value: Value = match declared {
        Some(ty) => match decode_declared_value(raw_value, ty) {
            Ok(value) => value,
            Err(reason) => {
                state.warn(format!(
                    "line {line}: type mismatch for {group}/{key} \
                     (expected {ty}, kept verbatim): {reason}"
                ));
                decode_value_inferred(raw_value)
            }
        },
        None => {
            let trimmed = raw_value.trim();
            if trimmed == "@Invalid()" {
                state.set_raw(group, key, "@Invalid()".to_owned());
                return;
            }
            if trimmed.starts_with('"')
                || (trimmed.contains('\\') && !trimmed.starts_with("@Variant("))
            {
                Value::Text(decode_qt_ini_text(trimmed))
            } else {
                decode_value_inferred(raw_value)
            }
        }
    };
    let encoded = match encode_value(&value) {
        Ok(encoded) => encoded,
        Err(err) => {
            state.warn(format!("line {line}: undecodable value: {err}"));
            // fall back to the raw text so nothing is lost
            raw_value.to_owned()
        }
    };
    state.set_raw(group, key, encoded);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canonical::decode_value;
    use crate::keys::{Key, raw};
    use crate::value::ValueType;

    // [window] group with plain bools
    #[test]
    fn plain_groups_and_values() {
        let ini = parse_legacy_ini("[main-window]\nsubtitle-list=true\nsearch = false\n")
            .expect("parses");
        let key =
            lookup(raw::window::GROUP, raw::window::SUBTITLE_LIST).expect("known");
        assert!(matches!(key, Key::Window(_)));
        assert_eq!(
            decode_value(
                ini.sections["main-window"]["subtitle-list"].as_str(),
                ValueType::Bool
            ),
            Ok(Value::Bool(true))
        );
        assert_eq!(
            decode_value(
                ini.sections["main-window"]["search"].as_str(),
                ValueType::Bool
            ),
            Ok(Value::Bool(false))
        );
    }

    // QSettings escapes in quoted and unquoted strings, plus multiline input.
    #[test]
    fn escape_families_and_multiline() {
        let text = concat!(
            "[search]\n",
            "replace-with=\"line1\\nline2\\tdone\"\n",
            "remove-regex=a\\\\[bc]\n",
            "unknown=\"bell\\a-hex\\x42-oct\\101\"\n",
        );
        let ini = parse_legacy_ini(text).expect("parses");
        let v = decode_value(
            ini.sections["search"]["replace-with"].as_str(),
            ValueType::Text,
        );
        assert_eq!(v, Ok(Value::Text("line1\nline2\tdone".to_owned())));
        let regex = decode_value(
            ini.sections["search"]["remove-regex"].as_str(),
            ValueType::Text,
        );
        assert_eq!(regex, Ok(Value::Text(r"a\[bc]".to_owned())));
        assert_eq!(
            decode_value(
                ini.sections["search"]["unknown"].as_str(),
                ValueType::Text
            ),
            Ok(Value::Text("bell\u{0007}-hexB-octA".to_owned()))
        );

        let multi = "[search]\nremove-regex=\"first\nsecond\n third\"\n";
        let ini = parse_legacy_ini(multi).expect("parses");
        assert_eq!(
            decode_value(
                ini.sections["search"]["remove-regex"].as_str(),
                ValueType::Text
            ),
            Ok(Value::Text("first\nsecond\n third".to_owned()))
        );
    }

    // ; and # comments, blank rows, BOM and CRLF
    #[test]
    fn comments_blank_bom_crlf() {
        let text = "﻿[application]\r\n; comment\r\n# comment\r\n\r\nlanguage=1\r\n";
        let ini = parse_legacy_ini(text).expect("parses");
        assert_eq!(
            ini.sections["application"]["language"],
            "1"
        );
    }

    // Qt's key escapes are Latin-1 / UTF-16 code units, not UTF-8 bytes.
    #[test]
    fn qt_key_escapes_and_nested_coordinates() {
        let text = concat!(
            "[caf%E9]\n",
            "name%U65E5%UD83D%UDE00=value\n",
            "1\\name=item\n",
            "profiles\\Default\\card-add=F\n",
        );
        let ini = parse_legacy_ini(text).expect("parses");
        assert_eq!(ini.sections["café"]["name日😀"], "value");
        assert_eq!(ini.sections["café/1"]["name"], "item");
        assert_eq!(ini.sections["café/profiles/Default"]["card-add"], "F");
        assert_eq!(decode_qt_key("key%2Dwith%2Ddash"), "key-with-dash");
        assert_eq!(decode_qt_key("%25"), "%");
        assert_eq!(decode_qt_key("%C3%A9"), "Ã©");
        assert_eq!(decode_qt_key("%Fo"), "%Fo");
    }

    // array entries (beginWriteArray -> 1\name keys)
    #[test]
    fn array_entries_are_folded_into_indexed_groups() {
        let text = concat!(
            "[audio-srcs]\n",
            "1\\name=JapanesePod101\n",
            "1\\url=https://example.com/\n",
            "1\\type=0\n",
            "2\\name=Forvo\n",
        );
        let ini = parse_legacy_ini(text).expect("parses");
        let first = &ini.sections["audio-srcs/1"];
        assert_eq!(first["name"], "JapanesePod101");
        assert_eq!(first["url"], "https://example.com/");
        assert_eq!(first["type"], "0");
        assert!(ini.sections.contains_key("audio-srcs/2"));
        let resolved =
            lookup("audio-srcs/1", raw::audio_source::NAME).expect("known field");
        assert!(matches!(resolved, Key::AudioSource { index: 1, .. }));
    }

    // nested group headings (keybind/profiles/<name>)
    #[test]
    fn nested_profile_groups() {
        let text = "[keybind]\ncurrent=Default\n[keybind/profiles/Default]\ncard-add=F\n";
        let ini = parse_legacy_ini(text).expect("parses");
        assert!(ini.sections.contains_key("keybind"));
        let profiles = &ini.sections["keybind/profiles/Default"];
        let k = lookup("keybind/profiles/Default", "card-add").expect("known field");
        assert!(matches!(k, Key::KeybindProfile { field: _, .. }));
        assert!(profiles.contains_key("card-add"));
    }

    // unicode values survive
    #[test]
    fn unicode_values() {
        let text = "[interface]\nsub-font=「Noto Sans CJK JP」\n";
        let ini = parse_legacy_ini(text).expect("parses");
        assert_eq!(
            decode_value(
                ini.sections["interface"]["sub-font"].as_str(),
                ValueType::Text
            ),
            Ok(Value::Text("「Noto Sans CJK JP」".to_owned()))
        );
    }

    // root keys (before any heading) land in the general section
    #[test]
    fn root_keys_map_to_general() {
        let ini = parse_legacy_ini("version=3\n[leftover]=x\n").expect("parses");
        assert!(ini.sections.contains_key("general"));
        assert!(ini.sections["general"].contains_key("version"));
        assert!(ini.sections["general"].contains_key("[leftover]"));
    }

    // type mismatch: declared int fed a string keeps the raw and warns
    #[test]
    fn type_mismatch_warns_and_keeps_raw() {
        let text = "[search]\ndelay=abc\n";
        let ini = parse_legacy_ini(text).expect("parses");
        assert_eq!(ini.sections["search"]["delay"], "abc");
        assert!(ini
            .warnings
            .iter()
            .any(|w| w.contains("type mismatch") && w.contains("search/delay")));
    }

    // unknown keys are preserved (canonical value re-encoded)
    #[test]
    fn unknown_keys_preserved() {
        let text = "[search]\nsome-unknown-key=keep me\n";
        let ini = parse_legacy_ini(text).expect("parses");
        assert_eq!(ini.sections["search"]["some-unknown-key"], "keep me");
        assert!(ini.warnings.is_empty());
    }

    // Numeric keys are not array indices unless followed by a path delimiter.
    #[test]
    fn plain_digit_keys_unchanged() {
        let ini = parse_legacy_ini("[recent]\n1=value\nfiles=\"a, b\"\n").expect("parses");
        assert_eq!(ini.sections["recent"]["1"], "value");
        assert_eq!(
            decode_value(
                ini.sections["recent"]["files"].as_str(),
                ValueType::Strings
            ),
            Ok(Value::Strings(vec!["a, b".to_owned()]))
        );
        assert_eq!(
            crate::canonical::decode_strings("\"\""),
            Ok(vec![String::new()])
        );
    }

    // value forms: bare int, bool, float, string with '=' handling
    #[test]
    fn value_kinds() {
        let text = concat!(
            "[General]\n",
            "i=-42\n",
            "h=0x1A\n",
            "f=0.045\n",
            "b=TRUE\n",
            "s=a=b=c\n",
            "empty=\n",
        );
        let ini = parse_legacy_ini(text).expect("parses");
        let s = &ini.sections["general"];
        assert_eq!(s["i"], "-42");
        assert_eq!(s["h"], "26");
        assert_eq!(s["f"], "0.045");
        assert_eq!(s["b"], "true");
        // '=' in values forces quoting in the canonical re-encoding
        assert_eq!(s["s"], "\"a=b=c\"");
        // empty strings are quoted as ""
        assert_eq!(s["empty"], "\"\"");
        assert!(ini.warnings.is_empty());
    }

    #[test]
    fn qsettings_recent_files_and_dictionary_order() {
        let text = concat!(
            "[recent]\n",
            "files=movie1.mkv, movie2.mkv\n",
            "[dictionary]\n",
            "order=1, 2, 3\n",
        );
        let ini = parse_legacy_ini(text).expect("parses");
        assert!(ini.warnings.is_empty(), "unexpected warnings: {:?}", ini.warnings);
        assert_eq!(
            decode_value(
                ini.sections["recent"]["files"].as_str(),
                ValueType::Strings
            ),
            Ok(Value::Strings(vec!["movie1.mkv".to_owned(), "movie2.mkv".to_owned()]))
        );
        assert_eq!(
            decode_value(
                ini.sections["dictionary"]["order"].as_str(),
                ValueType::Ints
            ),
            Ok(Value::Ints(vec![1, 2, 3]))
        );
    }

    #[test]
    fn qsettings_at_invalid_and_at_variant() {
        let text = concat!(
            "[recent]\n",
            "empty=@Invalid()\n",
            "files=@Variant(\\0\\0\\0\\t\\0\\0\\0\\x1\\0\\0\\0\\n\\0\\0\\0\\xe\\0o\\0n\\0\\x65\\0.\\0m\\0k\\0v)\n",
            "[dictionary]\n",
            "order=@Variant(\\0\\0\\0\\t\\0\\0\\0\\x1\\0\\0\\0\\x2\\0\\0\\0*)\n",
        );
        let ini = parse_legacy_ini(text).expect("parses");
        assert_eq!(
            decode_value(
                ini.sections["recent"]["empty"].as_str(),
                ValueType::Strings
            ),
            Ok(Value::Strings(Vec::new()))
        );
        assert_eq!(
            decode_value(
                ini.sections["recent"]["files"].as_str(),
                ValueType::Strings
            ),
            Ok(Value::Strings(vec!["one.mkv".to_owned()]))
        );
        assert_eq!(
            decode_value(
                ini.sections["dictionary"]["order"].as_str(),
                ValueType::Ints
            ),
            Ok(Value::Ints(vec![42]))
        );
    }
}

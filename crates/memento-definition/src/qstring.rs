
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

//! QString/QChar/QColor value-semantics helpers shared by the CSS engine port.

use unicode_general_category::{get_general_category, GeneralCategory};

/// Whitespace per `QChar::isSpace()`: separators (Zs, Zl, Zp) plus \t \n \v \f \r.
///
/// PORT-NOTE: `char::is_whitespace` is the Unicode White_Space property, which
/// additionally contains U+0085 (NEL, category Cc). Qt rejects U+0085, so it
/// is excluded here; for every other code point classified today the two
/// predicates agree.
#[inline]
pub fn is_qchar_space(ch: char) -> bool {
    ch.is_whitespace() && ch != '\u{0085}'
}

/// `QString::trimmed()`.
pub fn trimmed(text: &str) -> &str {
    text.trim_matches(is_qchar_space)
}

/// `QString::toLower()`: Unicode lowercase, including multi-character
/// expansions (matching the mapping used by the memento-dict parser port).
pub fn to_lower(text: &str) -> String {
    if text.is_ascii() {
        return text.to_ascii_lowercase();
    }
    let mut result = String::with_capacity(text.len());
    for ch in text.chars() {
        if ch.is_ascii() {
            result.push(ch.to_ascii_lowercase());
        } else {
            result.extend(ch.to_lowercase());
        }
    }
    result
}

/// `QChar::toLower()` for one BMP code point: the single-character (simple) lowercase.
pub fn to_lower_char(ch: char) -> char {
    if ch.is_ascii() {
        ch.to_ascii_lowercase()
    } else if ch as u32 > 0xFFFF {
        // QChar::toLower() receives one UTF-16 code unit; QString iteration
        // therefore leaves both halves of supplementary characters intact.
        ch
    } else {
        ch.to_lowercase().next().unwrap_or(ch)
    }
}

/// `QChar::isDigit()`: Unicode category Nd on a single UTF-16 code unit.
///
/// Supplementary-plane decimal digits are represented by surrogate code units
/// in QString iteration, and neither surrogate is itself a digit.
#[inline]
pub fn is_qchar_digit(ch: char) -> bool {
    ch as u32 <= 0xffff && get_general_category(ch) == GeneralCategory::DecimalNumber
}

/// `QChar::isUpper()`: Unicode category Lu for one UTF-16 code unit.
///
/// `char::is_uppercase` uses the broader derived `Uppercase` property. Its
/// BMP-only `Other_Uppercase` characters (Roman numerals and circled letters)
/// are not Lu; supplementary letters are represented by two non-cased QChars.
#[inline]
pub fn is_qchar_upper(ch: char) -> bool {
    let code = ch as u32;
    code <= 0xFFFF
        && !matches!(code, 0x2160..=0x216F | 0x24B6..=0x24CF)
        && ch.is_uppercase()
}

/// UTF-16 code-unit length of a string (QString::size()).
pub fn utf16_size(text: &str) -> usize {
    text.chars().map(char::len_utf16).sum()
}

// --- PCRE2-flavored helpers (QRegularExpression ports) ----------------------

/// PCRE2 `\s` without the Unicode-properties option: ASCII whitespace only.
#[inline]
pub fn is_pcre_space(ch: char) -> bool {
    matches!(ch, ' ' | '\t' | '\n' | '\r' | '\u{000B}' | '\u{000C}')
}

/// PCRE2 `\d` without the Unicode-properties option: ASCII digits.
#[inline]
pub fn is_pcre_digit(ch: char) -> bool {
    ch.is_ascii_digit()
}

/// PCRE2 `[A-Za-z]` (the function-name capture) or, case-insensitively, the
/// `[a-z%]` unit class of the calc pattern.
#[inline]
pub fn is_css_function_char(ch: char) -> bool {
    ch.is_ascii_lowercase() || ch.is_ascii_uppercase() || ch == '-'
}

#[inline]
pub fn is_css_unit_char(ch: char) -> bool {
    ch.is_ascii_lowercase() || ch.is_ascii_uppercase() || ch == '%'
}

/// PCRE2 `\w` for `\b` without the Unicode-properties option.
#[inline]
pub fn is_pcre_word_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || ch == '_' || is_css_function_char(ch)
}

/// Case-insensitive search for an ASCII needle, matching QString's Unicode
/// case-fold comparison. Rust returns a UTF-8 byte offset so callers can slice
/// at the returned boundary.
///
/// Qt applies simple folding to the same-length search window; only BMP
/// characters whose single-code-point fold is ASCII can match this needle.
pub fn find_ascii_ci(text: &str, needle: &str, from: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    if needle.is_empty() {
        return (from <= bytes.len()).then_some(from);
    }
    if !needle.is_ascii() || from > bytes.len() {
        return None;
    }
    if text.is_ascii() {
        if bytes.len() - from < needle.len() {
            return None;
        }
        for start in from..=bytes.len() - needle.len() {
            if bytes[start..start + needle.len()]
                .iter()
                .zip(needle.bytes())
                .all(|(a, b)| fold_byte(*a) == fold_byte(b))
            {
                return Some(start);
            }
        }
        return None;
    }

    text.char_indices()
        .filter(|(start, _)| *start >= from)
        .find_map(|(start, _)| {
            let mut chars = text[start..].chars();
            needle.bytes()
                .all(|expected| chars.next().and_then(fold_qchar_ascii) == Some(fold_byte(expected)))
                .then_some(start)
        })
}

fn fold_byte(byte: u8) -> u8 {
    byte.to_ascii_lowercase()
}

fn fold_qchar_ascii(ch: char) -> Option<u8> {
    if ch.is_ascii() {
        return Some(ch.to_ascii_lowercase() as u8);
    }
    if ch.len_utf16() != 1 {
        return None;
    }
    match ch {
        '\u{017F}' => Some(b's'), // LONG S
        '\u{212A}' => Some(b'k'), // KELVIN SIGN
        _ => None,
    }
}

/// QString::startsWith(needle, Qt::CaseInsensitive) for ASCII needles.
pub fn starts_with_ascii_ci(text: &str, needle: &str) -> bool {
    if !needle.is_ascii() {
        return false;
    }
    if text.is_ascii() {
        return text.len() >= needle.len()
            && text
                .as_bytes()
                .iter()
                .zip(needle.bytes())
                .all(|(a, b)| fold_byte(*a) == fold_byte(b));
    }
    let mut chars = text.chars();
    needle.bytes()
        .all(|expected| chars.next().and_then(fold_qchar_ascii) == Some(fold_byte(expected)))
}

/// QString::contains(needle, Qt::CaseInsensitive) for ASCII needles.
pub fn contains_ascii_ci(text: &str, needle: &str) -> bool {
    find_ascii_ci(text, needle, 0).is_some()
}

/// QString::compare(other, Qt::CaseInsensitive) == 0 for ASCII needles.
pub fn equals_ascii_ci(text: &str, needle: &str) -> bool {
    if !needle.is_ascii() {
        return false;
    }
    if text.is_ascii() {
        return text.len() == needle.len() && starts_with_ascii_ci(text, needle);
    }
    let mut chars = text.chars();
    needle.bytes()
        .all(|expected| chars.next().and_then(fold_qchar_ascii) == Some(fold_byte(expected)))
        && chars.next().is_none()
}

/// QString::startsWith / contains (case sensitive); byte-based, exact.
pub fn starts_with_cs(text: &str, needle: &str) -> bool {
    text.starts_with(needle)
}

// --- double parsing / formatting -------------------------------------------

/// QString::toDouble (C-locale): optional sign, digits with optional fraction
/// and optional exponent. Returns None when malformed (ignoring that Qt also
/// accepts inf/nan spellings, which CSS never feeds here).
pub fn c_double(text: &str) -> Option<f64> {
    let mut chars = text.chars().peekable();
    let mut cleaned = String::with_capacity(text.len() + 4);
    if matches!(chars.peek(), Some('+') | Some('-')) {
        cleaned.push(chars.next().unwrap());
    }
    let mut had_digits = false;
    while matches!(chars.peek(), Some(c) if c.is_ascii_digit()) {
        cleaned.push(chars.next().unwrap());
        had_digits = true;
    }
    if chars.peek() == Some(&'.') {
        cleaned.push(chars.next().unwrap());
        while matches!(chars.peek(), Some(c) if c.is_ascii_digit()) {
            cleaned.push(chars.next().unwrap());
            had_digits = true;
        }
    }
    if !had_digits {
        return None;
    }
    // Optional exponent: [eE] [+-]? digit+ (QString::toDouble accepts exponents;
    // the CSS regex captures never contain one, but JSON literals do).
    if matches!(chars.peek(), Some('e') | Some('E')) {
        cleaned.push('e');
        chars.next();
        if matches!(chars.peek(), Some('+') | Some('-')) {
            cleaned.push(chars.next().unwrap());
        }
        let mut exponent_digits = 0;
        while matches!(chars.peek(), Some(c) if c.is_ascii_digit()) {
            cleaned.push(chars.next().unwrap());
            exponent_digits += 1;
        }
        if exponent_digits == 0 {
            return None;
        }
    }
    if chars.next().is_some() {
        return None;
    }
    // Rust rejects a trailing bare "." where C accepts it as zero fraction.
    if cleaned.ends_with('.') {
        cleaned.pop();
    }
    if cleaned.starts_with('.') || cleaned.starts_with("+.") || cleaned.starts_with("-.") {
        let insert_at = usize::from(!cleaned.starts_with('.'));
        cleaned.insert(insert_at, '0');
    }
    cleaned.parse::<f64>().ok()
}

/// QString::number(double): printf %g with precision 6 (C locale).
pub fn qstring_number(value: f64) -> String {
    format_g(value, 6)
}

/// printf `%g` (C locale) with the given precision.
///
/// PORT-NOTE: ties round to even, matching glibc/Qt on the values reachable
/// from JSON numbers and pixel arithmetic; NaN -> "nan", infinities -> "inf"/
/// "-inf" as QString::number produces for Qt's default double formatting.
pub fn format_g(value: f64, precision: u32) -> String {
    if value.is_nan() {
        return "nan".to_string();
    }
    if value.is_infinite() {
        return if value < 0.0 { "-inf" } else { "inf" }.to_string();
    }
    let precision = if precision == 0 { 1 } else { precision };
    if value == 0.0 {
        return if value.is_sign_negative() { "-0" } else { "0" }.to_string();
    }
    let precise = format!("{:.*e}", (precision - 1) as usize, value);
    let pivot = precise.find('e').expect("Rust e-format always has 'e'");
    let mantissa = &precise[..pivot];
    let exponent: i32 = precise[pivot + 1..].parse().expect("valid exponent");
    if exponent < -4 || exponent >= precision as i32 {
        let (whole, fraction) = mantissa.split_once('.').unwrap_or((mantissa, ""));
        let fraction = fraction.trim_end_matches('0');
        let mantissa = if fraction.is_empty() {
            whole.to_string()
        } else {
            format!("{whole}.{fraction}")
        };
        let sign = if exponent < 0 { '-' } else { '+' };
        format!("{mantissa}e{sign}{:02}", exponent.abs())
    } else {
        let decimals = (precision as i32 - 1 - exponent).max(0) as usize;
        let mut formatted = format!("{:.*}", decimals, value);
        if formatted.contains('.') {
            while formatted.ends_with('0') {
                formatted.pop();
            }
            if formatted.ends_with('.') {
                formatted.pop();
            }
        }
        formatted
    }
}

/// QString::number(double, 'f', precision) (C locale).
pub fn qstring_number_f(value: f64, precision: usize) -> String {
    if value.is_nan() {
        return "nan".to_string();
    }
    if value.is_infinite() {
        return if value < 0.0 { "-inf" } else { "inf" }.to_string();
    }
    format!("{:.*}", precision, value)
}

// --- shared quoted-string scanning ------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whitespace_matches_qchar_class() {
        assert!(is_qchar_space(' '));
        assert!(is_qchar_space('\t'));
        assert!(is_qchar_space('\n'));
        assert!(is_qchar_space('\u{2028}'));
        // NEL is White_Space in Rust but category Cc in Qt.
        assert!(!is_qchar_space('\u{0085}'));
    }


    #[test]
    fn qchar_digit_matches_decimal_category_and_utf16_units() {
        assert!(is_qchar_digit('7'));
        assert!(is_qchar_digit('\u{0667}')); // Arabic-Indic seven (Nd)
        assert!(!is_qchar_digit('\u{2167}')); // Roman numeral eight (Nl)
        assert!(!is_qchar_digit('\u{00b2}')); // Superscript two (No)
        // Mathematical double-struck digits are supplementary and therefore
        // seen as two non-digit QChars by the C++ QString algorithm.
        assert!(!is_qchar_digit('\u{1d7df}'));
    }
    #[test]
    fn qstring_number_matches_printf_g() {
        // Expected values derived from the printf %g contract Qt documents.
        assert_eq!(qstring_number(0.0), "0");
        assert_eq!(qstring_number(-0.0), "-0");
        assert_eq!(qstring_number(2.0), "2");
        assert_eq!(qstring_number(2.5), "2.5");
        assert_eq!(qstring_number(150.0), "150");
        assert_eq!(qstring_number(100000.0), "100000");
        assert_eq!(qstring_number(0.0001), "0.0001");
        assert_eq!(qstring_number(0.00001), "1e-05");
        assert_eq!(qstring_number(1e-7), "1e-07");
        assert_eq!(qstring_number(1234567.0), "1.23457e+06");
        assert_eq!(qstring_number(123456789.0), "1.23457e+08");
        assert_eq!(qstring_number(2.25e-8), "2.25e-08");
    }

    #[test]
    fn qstring_number_f_matches_printf_f() {
        assert_eq!(qstring_number_f(5.0, 3), "5.000");
        assert_eq!(qstring_number_f(-0.0, 3), "-0.000");
        assert_eq!(qstring_number_f(0.6625, 3), "0.662");
    }

    #[test]
    fn c_double_covers_locale_c_forms() {
        assert_eq!(c_double("5"), Some(5.0));
        assert_eq!(c_double("5."), Some(5.0));
        assert_eq!(c_double(".5"), Some(0.5));
        assert_eq!(c_double("+007"), Some(7.0));
        assert_eq!(c_double("-12"), Some(-12.0));
        assert_eq!(c_double("1.5e2"), Some(150.0));
        assert_eq!(c_double("12px"), None);
        assert_eq!(c_double(""), None);
        assert_eq!(c_double("."), None);
        assert_eq!(c_double("1e"), None);
    }

    #[test]
    fn qt_case_insensitive_search_handles_simple_unicode_case_folding() {
        assert_eq!(find_ascii_ci("hello Var( x)", "var(", 0), Some(6));
        assert_eq!(find_ascii_ci("hello Var( x)", "var(", 7), None);
        assert!(starts_with_ascii_ci("VAR(", "var("));
        assert!(contains_ascii_ci("a GRADIENT(b)", "gradient("));
        assert!(equals_ascii_ci("Keep-All", "keep-all"));
        assert!(!equals_ascii_ci("keep-al", "keep-all"));
        // QString compares with Unicode case folding, not toLower().
        assert!(!starts_with_ascii_ci("İN(", "in(")); // full fold expands
        assert!(starts_with_ascii_ci("ſT(", "st("));
        assert_eq!(find_ascii_ci("😀ſT(", "st(", 0), Some(4));
        assert!(equals_ascii_ci("KEEP", "keep"));
    }

    #[test]
    fn to_lower_matches_unicode_expansions() {
        assert_eq!(to_lower("ABCüÄİ"), "abcüäi̇");
        assert_eq!(to_lower_char('İ'), 'i');
    }
}

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

//! Manual PCRE2-equivalent scanners for the three QRegularExpression uses in
//! `structuredrichtext_css.cpp`, plus the fixed support-data sets from
//! `structuredrichtext_p.h` (L1354-1392) and the
//! `DictionaryStyles::candidateRuleIndexes(tag, childIndex)` lookup chain.

use std::collections::HashMap;

use crate::element::ParsedStylesheet;
use crate::qstring::{c_double, is_pcre_space, starts_with_ascii_ci};

/// Set of supported structured content tags (structuredrichtext_p.h
/// L1357-1361). QSet membership only; sorted for `binary_search` - iteration
/// order of the Qt set is never observable.
pub(crate) const SUPPORTED_TAGS: &[&str] = &[
    "a", "br", "details", "div", "img", "li", "ol", "rp", "rt", "ruby", "span", "summary",
    "table", "tbody", "td", "tfoot", "th", "thead", "tr", "ul",
];

/// Set of supported CSS properties (structuredrichtext_p.h L1364-1382),
/// sorted for `binary_search`.
pub(crate) const SUPPORTED_CSS_PROPERTIES: &[&str] = &[
    "background-color",
    "border",
    "border-bottom",
    "border-bottom-color",
    "border-bottom-style",
    "border-bottom-width",
    "border-collapse",
    "border-color",
    "border-left",
    "border-left-color",
    "border-left-style",
    "border-left-width",
    "border-radius",
    "border-right",
    "border-right-color",
    "border-right-style",
    "border-right-width",
    "border-style",
    "border-top",
    "border-top-color",
    "border-top-style",
    "border-top-width",
    "border-width",
    "color",
    "cursor",
    "display",
    "float",
    "font",
    "font-family",
    "font-kerning",
    "font-size",
    "font-style",
    "font-variant",
    "font-weight",
    "image-rendering",
    "line-height",
    "list-style-type",
    "margin-bottom",
    "margin-left",
    "margin-right",
    "margin-top",
    "padding",
    "padding-bottom",
    "padding-left",
    "padding-right",
    "padding-top",
    "text-align",
    "text-decoration",
    "text-indent",
    "text-transform",
    "vertical-align",
    "white-space",
    "width",
    "word-break",
    "word-spacing",
];

/// CSS functions that are supported by Qt rich text
/// (structuredrichtext_p.h L1389-1392), sorted for `binary_search`.
pub(crate) const SUPPORTED_CSS_FUNCTIONS: &[&str] = &["hsl", "hsla", "rgb", "rgba"];

pub(crate) fn is_supported_css_function(function_name: &str) -> bool {
    SUPPORTED_CSS_FUNCTIONS.binary_search(&function_name).is_ok()
}

pub(crate) fn is_supported_css_property(property: &str) -> bool {
    SUPPORTED_CSS_PROPERTIES.binary_search(&property).is_ok()
}

pub(crate) fn is_supported_structured_tag(tag: &str) -> bool {
    SUPPORTED_TAGS.binary_search(&tag).is_ok()
}

/// PCRE2 `\w` (no Unicode-properties option): `[A-Za-z0-9_]`.
///
/// PORT-NOTE: `qstring::is_pcre_word_char` additionally classifies '-' as a
/// word character, diverging from PCRE2. For `\b([A-Za-z-]+)\s*\(` that
/// changes which start positions pass the `\b` assertion (e.g. a capture may
/// begin at a '-' preceded by a word character). The pattern is mirrored
/// here with the genuine PCRE2 `\w` definition.
#[inline]
fn is_pcre2_word_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

/// The captures of `\b([A-Za-z-]+)\s*\(` (`m_cssFunctionRegex`,
/// structuredrichtext_p.h L1384-1387) applied globally.
///
/// PORT-NOTE: hand-rolled PCRE2-equivalent scan. One pattern iteration is a
/// `\b` boundary at the capture start, a greedy `[A-Za-z-]+` run that may
/// backtrack to shorter lengths, then `\s*` and a literal '('; successive
/// global matches never overlap (scanning resumes after the whole match).
/// Greedy backtracking order is reproduced by trying capture ends from
/// longest to shortest.
pub(crate) fn css_function_captures(text: &str) -> Vec<String> {
    let bytes = text.as_bytes();
    let mut captures = Vec::new();
    let mut i = 0usize;
    while i < bytes.len() {
        /* `\b` at position i: exactly one of the sides is a PCRE2 `\w` char
         * (the subject edge counts as non-word). */
        let word_prev = i > 0 && is_pcre2_word_byte(bytes[i - 1]);
        let word_here = is_pcre2_word_byte(bytes[i]);
        if word_prev == word_here {
            i += 1;
            continue;
        }

        /* [A-Za-z-]+ maximal run */
        let mut run_end = i;
        while run_end < bytes.len()
            && (bytes[run_end].is_ascii_lowercase()
                || bytes[run_end].is_ascii_uppercase()
                || bytes[run_end] == b'-')
        {
            run_end += 1;
        }

        let mut matched = false;
        for capture_end in (i + 1..=run_end).rev() {
            let mut probe = capture_end;
            while probe < bytes.len() && is_pcre_space(bytes[probe] as char) {
                probe += 1;
            }
            if probe < bytes.len() && bytes[probe] == b'(' {
                captures.push(text[i..capture_end].to_string());
                i = probe + 1;
                matched = true;
                break;
            }
        }
        if !matched {
            i += 1;
        }
    }
    captures
}

/// Manual equivalent of the trailing percentage regexes
/// `\s+([0-9]+(?:\.[0-9]+)?)%\s*$` (structuredrichtext_css.cpp L709-711,
/// `PERCENTAGE`, capture requested) and
/// `\s+[0-9]+(?:\.[0-9]+)?%\s*$` (L818-820, `COLOR_STOP`, no capture).
///
/// Returns the whole-match start - the PCRE2 `capturedStart()`, i.e. the
/// beginning of the required whitespace run (leftmost start wins - and the
/// captured number slice. `capture` selects which of the two patterns is
/// mirrored; the match positions are identical.
///
/// PORT-NOTE: with PCRE2 defaults (no `PCRE2_DOLLAR_ENDONLY`), `$` also
/// matches just before a final newline. Both callers pass
/// `QString::trimmed()` results (`colorValue = arguments[i].trimmed()`,
/// L715/L816), which cannot end in '\n', so treating `$` as end-of-subject
/// only is behavior-classified equivalent here. `\s` and `\d` are the ASCII
/// PCRE2 classes. The `\.[0-9]+` branch requires fraction digits when a dot
/// participates, and the leading `[0-9]+` requires a digit before it.
pub(crate) fn match_trailing_percentage(text: &str, capture: bool) -> Option<(usize, &str)> {
    let bytes = text.as_bytes();
    let _ = capture;

    let mut pos = bytes.len();
    /* \s* before '$' */
    while pos > 0 && is_pcre_space(bytes[pos - 1] as char) {
        pos -= 1;
    }
    if pos == 0 || bytes[pos - 1] != b'%' {
        return None;
    }
    /* number: [0-9]+(?:\.[0-9]+)? scanned backwards; `end` is the '%' byte
     * offset, so the number occupies [start, end) when complete. */
    let end = pos - 1;
    let mut j = end;
    while j > 0 && (bytes[j - 1] as char).is_ascii_digit() {
        j -= 1;
    }
    if end > j && j > 0 && bytes[j - 1] == b'.' {
        /* optional \.[0-9]+ group: the dot only participates when fraction
         * digits were read above; the leading [0-9]+ still needs a digit. */
        j -= 1;
        let integer_digits_end = j;
        while j > 0 && (bytes[j - 1] as char).is_ascii_digit() {
            j -= 1;
        }
        if j == integer_digits_end {
            return None;
        }
    } else if j == end {
        /* No digit run at all - `[0-9]+` cannot be satisfied. */
        return None;
    }
    let number_start = j;

    /* \s+ before the number: at least one whitespace, with the leftmost
     * possible match start (the beginning of the maximal whitespace run). */
    if number_start == 0 || !is_pcre_space(bytes[number_start - 1] as char) {
        return None;
    }
    let mut match_start = number_start;
    while match_start > 0 && is_pcre_space(bytes[match_start - 1] as char) {
        match_start -= 1;
    }
    Some((match_start, &text[number_start..end]))
}

/// Manual parser for the `DIVISION` regex in `resolveCssCalc`
/// (structuredrichtext_css.cpp L778-782):
/// `^calc\(\s*([+-]?(?:\d+(?:\.\d*)?|\.\d+))([a-z%]+)\s*/\s*([+-]?(?:\d+(?:
/// \.\d*)?|\.\d+))\s*\)$` with `QRegularExpression::CaseInsensitiveOption`
/// (so `[a-z%]` also accepts `A-Z`). Returns (number, unit, denominator).
///
/// PORT-NOTE: byte-level scanner over the UTF-8 representation; `\s`/`\d`
/// are the ASCII PCRE2 classes; `^`/`$` are end-of-subject anchors (values
/// passed here are sliced call substrings that end in ')'). The greedy
/// `[a-z%]+` unit stops at the first non-member byte; the class is disjoint
/// from the number grammar, so no backtracking interaction exists.
pub(crate) fn match_calc_division(value: &str) -> Option<(f64, String, f64)> {
    if !starts_with_ascii_ci(value, "calc(") {
        return None;
    }
    let bytes = value.as_bytes();
    let mut pos = 5usize; /* "calc(" */
    while pos < bytes.len() && is_pcre_space(bytes[pos] as char) {
        pos += 1;
    }

    let (number, number_end) = scan_pcre_number(bytes, pos)?;
    pos = number_end;

    /* ([a-z%]+) case-insensitive */
    let unit_start = pos;
    while pos < bytes.len()
        && (bytes[pos].is_ascii_lowercase()
            || bytes[pos].is_ascii_uppercase()
            || bytes[pos] == b'%')
    {
        pos += 1;
    }
    if pos == unit_start {
        return None;
    }
    let unit = value[unit_start..pos].to_string();

    while pos < bytes.len() && is_pcre_space(bytes[pos] as char) {
        pos += 1;
    }
    if pos >= bytes.len() || bytes[pos] != b'/' {
        return None;
    }
    pos += 1;
    while pos < bytes.len() && is_pcre_space(bytes[pos] as char) {
        pos += 1;
    }

    let (denominator, denominator_end) = scan_pcre_number(bytes, pos)?;
    pos = denominator_end;
    while pos < bytes.len() && is_pcre_space(bytes[pos] as char) {
        pos += 1;
    }
    if pos != bytes.len() - 1 || bytes[pos] != b')' {
        return None;
    }

    Some((number, unit, denominator))
}

/// `[+-]?(?:\d+(?:\.\d*)?|\.\d+)` at `pos`; returns
/// (numeric value, end byte index).
fn scan_pcre_number(bytes: &[u8], mut pos: usize) -> Option<(f64, usize)> {
    let start = pos;
    if pos < bytes.len() && (bytes[pos] == b'+' || bytes[pos] == b'-') {
        pos += 1;
    }
    let digits_before = {
        let run_start = pos;
        while pos < bytes.len() && bytes[pos].is_ascii_digit() {
            pos += 1;
        }
        pos - run_start
    };
    if digits_before > 0 {
        if pos < bytes.len() && bytes[pos] == b'.' {
            pos += 1;
            while pos < bytes.len() && bytes[pos].is_ascii_digit() {
                pos += 1;
            }
            /* \.\d* : zero digits after the dot are allowed */
        }
    } else {
        /* \.\d+ alternative */
        if pos >= bytes.len() || bytes[pos] != b'.' {
            return None;
        }
        pos += 1;
        let mut digits = 0usize;
        while pos < bytes.len() && bytes[pos].is_ascii_digit() {
            pos += 1;
            digits += 1;
        }
        if digits == 0 {
            return None;
        }
    }
    let end = pos;
    /* The scanned bytes are ASCII, so the byte slice is valid UTF-8 and the
     * value conversion matches QString::toDouble on the same capture. */
    let number = c_double(&String::from_utf8_lossy(&bytes[start..end]))?;
    Some((number, end))
}

/// Candidates for `DictionaryStyles::candidateRuleIndexes(tag, childIndex)`
/// (dictionarystyles.cpp L81-104).
///
/// PORT-NOTE: `element.rs`'s `candidate_rule_indexes_for_child` returns
/// `universal_rule_indexes` when the tag is not indexed, but the C++ looks
/// up the parser-seeded universal key `""` in `nonNthRuleIndexesByTargetTag`
/// and `ruleIndexesByTargetTagAndChildIndex` (dictionarystyles.cpp L37-42;
/// dictionarystyles_parser.cpp L481, L500-540), which excludes exact-target
/// `:nth-child()` rules from the universal fallback and adds them per exact
/// child index. This mirrors the exact C++ lookup chain.
///
/// SEAM: stylesheet producers must seed the `""` key entries exactly the way
/// dictionarystyles_parser.cpp does.
pub fn candidate_rule_indexes_for<'a>(
    stylesheet: &'a ParsedStylesheet,
    tag: &'a str,
    child_index: i64,
) -> &'a [usize] {
    /* candidateTagKey (dictionarystyles.cpp L37-42): the tag itself when
     * indexed, otherwise the shared universal "" key. */
    let key = if stylesheet.rule_indexes_by_target_tag.contains_key(tag) {
        tag
    } else {
        ""
    };
    if let Some(by_tag) = stylesheet
        .rule_indexes_by_target_tag_and_child_index
        .get(&key.to_string())
    {
        if let Some(indexes) = by_tag.get(&child_index) {
            return indexes;
        }
    }
    stylesheet
        .non_nth_rule_indexes_by_target_tag
        .get(&key.to_string())
        .unwrap_or(&stylesheet.universal_rule_indexes)
}

/// Referenced to keep `HashMap` listed for the ParsedStylesheet maps layout:
/// the stylesheet index maps are hash maps that are only ever keyed by tag or
/// child index, never iterated by this engine (see element.rs doc comments).
#[allow(dead_code)]
type StylesheetIndexMapShape = HashMap<String, Vec<usize>>;

//! Hand-written CSS function/calc/percentage scanners, supported tag and
//! property whitelists, and stylesheet rule candidate lookup.

use crate::element::ParsedStylesheet;
use super::text::{parse_css_f64, strip_prefix_ascii_ci};

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

#[inline]
pub(crate) fn is_supported_css_function_ascii_ci(function_name: &str) -> bool {
    function_name.eq_ignore_ascii_case("rgb")
        || function_name.eq_ignore_ascii_case("rgba")
        || function_name.eq_ignore_ascii_case("hsl")
        || function_name.eq_ignore_ascii_case("hsla")
}

#[inline]
pub(crate) fn is_supported_css_property(property: &str) -> bool {
    SUPPORTED_CSS_PROPERTIES.binary_search(&property).is_ok()
}

#[inline]
pub(crate) fn is_supported_structured_tag(tag: &str) -> bool {
    SUPPORTED_TAGS.binary_search(&tag).is_ok()
}

/// ASCII word byte (`[A-Za-z0-9_]`) for CSS function-name word boundaries.
#[inline]
fn is_pcre2_word_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

/// Check whether `text` contains any CSS function capture matching
/// `\b([A-Za-z-]+)\s*\(` (`m_cssFunctionRegex`, structuredrichtext_p.h
/// L1384-1387) whose name is not a supported CSS function.
///
/// Fast-paths `!text.contains('(')` and allocates nothing on the hot path.
pub(crate) fn has_unsupported_css_function(text: &str) -> bool {
    if !text.contains('(') {
        return false;
    }
    let bytes = text.as_bytes();
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
            while probe < bytes.len() && (bytes[probe] as char).is_ascii_whitespace() {
                probe += 1;
            }
            if probe < bytes.len() && bytes[probe] == b'(' {
                if !is_supported_css_function_ascii_ci(&text[i..capture_end]) {
                    return true;
                }
                i = probe + 1;
                matched = true;
                break;
            }
        }
        if !matched {
            i += 1;
        }
    }
    false
}

/// Manual equivalent of the trailing percentage regexes
/// `\s+([0-9]+(?:\.[0-9]+)?)%\s*$` (structuredrichtext_css.cpp L709-711,
/// `PERCENTAGE`, capture requested) and
/// `\s+[0-9]+(?:\.[0-9]+)?%\s*$` (L818-820, `COLOR_STOP`, no capture).
///
/// Returns the whole-match start - the PCRE2 `capturedStart()`, i.e. the
/// beginning of the required whitespace run (leftmost start wins) - and the
/// captured number slice.
pub(crate) fn match_trailing_percentage(text: &str, capture: bool) -> Option<(usize, &str)> {
    let bytes = text.as_bytes();
    let _ = capture;

    let mut pos = bytes.len();
    /* \s* before '$' */
    while pos > 0 && (bytes[pos - 1] as char).is_ascii_whitespace() {
        pos -= 1;
    }
    if pos == 0 || bytes[pos - 1] != b'%' {
        return None;
    }
    /* number: [0-9]+(?:\.[0-9]+)? scanned backwards; `end` is the '%' byte
     * offset, so the number occupies [start, end) when complete. */
    let end = pos - 1;
    let mut j = end;
    while j > 0 && bytes[j - 1].is_ascii_digit() {
        j -= 1;
    }
    if end > j && j > 0 && bytes[j - 1] == b'.' {
        /* optional \.[0-9]+ group: the dot only participates when fraction
         * digits were read above; the leading [0-9]+ still needs a digit. */
        j -= 1;
        let integer_digits_end = j;
        while j > 0 && bytes[j - 1].is_ascii_digit() {
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
    if number_start == 0 || !(bytes[number_start - 1] as char).is_ascii_whitespace() {
        return None;
    }
    let mut match_start = number_start;
    while match_start > 0 && (bytes[match_start - 1] as char).is_ascii_whitespace() {
        match_start -= 1;
    }
    Some((match_start, &text[number_start..end]))
}

/// Manual parser for the `DIVISION` regex in `resolveCssCalc`
/// (structuredrichtext_css.cpp L778-782):
/// `^calc\(\s*([+-]?(?:\d+(?:\.\d*)?|\.\d+))([a-z%]+)\s*/\s*([+-]?(?:\d+(?:
/// \.\d*)?|\.\d+))\s*\)$` with `QRegularExpression::CaseInsensitiveOption`
/// (so `[a-z%]` also accepts `A-Z`). Returns (number, unit, denominator).
pub(crate) fn match_calc_division(value: &str) -> Option<(f64, String, f64)> {
    let rest = strip_prefix_ascii_ci(value, "calc(")?;
    let bytes = rest.as_bytes();
    let mut pos = 0usize;
    while pos < bytes.len() && (bytes[pos] as char).is_ascii_whitespace() {
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
    let unit = rest[unit_start..pos].to_string();

    while pos < bytes.len() && (bytes[pos] as char).is_ascii_whitespace() {
        pos += 1;
    }
    if pos >= bytes.len() || bytes[pos] != b'/' {
        return None;
    }
    pos += 1;
    while pos < bytes.len() && (bytes[pos] as char).is_ascii_whitespace() {
        pos += 1;
    }

    let (denominator, denominator_end) = scan_pcre_number(bytes, pos)?;
    pos = denominator_end;
    while pos < bytes.len() && (bytes[pos] as char).is_ascii_whitespace() {
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
    /* The scanned bytes are pure ASCII, so `from_utf8` always succeeds without allocation. */
    let slice = std::str::from_utf8(&bytes[start..end]).ok()?;
    let number = parse_css_f64(slice)?;
    Some((number, end))
}

/// Candidates for `DictionaryStyles::candidateRuleIndexes(tag, childIndex)`
/// (dictionarystyles.cpp L81-104).
///
/// The returned slice borrows only `stylesheet`, allowing callers to iterate
/// without cloning into a `Vec`.
pub fn candidate_rule_indexes_for<'a>(
    stylesheet: &'a ParsedStylesheet,
    tag: &str,
    child_index: i64,
) -> &'a [usize] {
    stylesheet.candidate_rule_indexes_for_child(tag, child_index)
}

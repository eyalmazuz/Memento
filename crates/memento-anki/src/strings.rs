//! Qt string semantics over raw UTF-16 code units.
//!
//! Everything in this stage operates on `&[u16]`: Qt strings are UTF-16
//! code unit buffers, token markers may carry lone surrogates or astral
//! text, and no string here is ever converted through a `String` (which is
//! only defined for scalar-formed UTF-16). The helpers mirror Qt
//! (`QString::split`, `QString::trimmed`, the `QChar::isSpace` set and the
//! `QRegularExpression("{.*?}")` global scan) unit by unit.

/// Start of a marker token: `{`.
pub const OPEN_BRACE: u16 = 0x7B;
/// End of a marker token: `}`.
pub const CLOSE_BRACE: u16 = 0x7D;
/// Line feed. PCRE2 is built with the LF newline convention, so it is the
/// only unit `.` of the `{.*?}` pattern refuses: CR, VT, FF, U+0085, U+2028
/// and U+2029 all match inside `{.*?}` (verified against Qt 6.11.2).
pub const LF: u16 = 0x0A;
/// `tokenText.split('|')` separator.
pub const VERTICAL_BAR: u16 = 0x7C;
/// `marker.split(':')` separator.
pub const COLON: u16 = 0x3A;
/// `markerArgs[1].split(',')` separator.
pub const COMMA: u16 = 0x2C;
/// `arg.split('=')` separator.
pub const EQUALS: u16 = 0x3D;

/// True when the unit belongs to the `QChar::isSpace` set used by
/// `QString::trimmed`. Every accepted unit is a single BMP code point:
/// U+0009..U+000D, U+0020, U+0085, U+00A0, U+1680, U+2000..U+200A,
/// U+2028, U+2029, U+202F, U+205F and U+3000. Because astral code points are
/// never whitespace, trimming over code units is exact.
#[inline]
pub fn qt_is_space(unit: u16) -> bool {
    matches!(
        unit,
        0x09..=0x0D
            | 0x20
            | 0x85
            | 0xA0
            | 0x1680
            | 0x2000..=0x200A
            | 0x2028
            | 0x2029
            | 0x202F
            | 0x205F
            | 0x3000
    )
}

/// `QString::trimmed` over code units: units accepted by `qt_is_space` are
/// removed from both ends. Returns an empty slice for all-whitespace input.
pub fn trim_u16(units: &[u16]) -> &[u16] {
    let mut start = 0;
    while start < units.len() && qt_is_space(units[start]) {
        start += 1;
    }
    let mut end = units.len();
    while end > start && qt_is_space(units[end - 1]) {
        end -= 1;
    }
    &units[start..end]
}

/// `QString::split` (Qt 6 default `KeepEmptyParts`) over code units: splits
/// on every occurrence of `delim` and keeps empty fields, including a
/// trailing one. Always yields at least one field.
pub fn split_keep_empty(delim: u16, units: &[u16]) -> Vec<&[u16]> {
    let mut fields = Vec::new();
    let mut begin = 0;
    for (index, &unit) in units.iter().enumerate() {
        if unit == delim {
            fields.push(&units[begin..index]);
            begin = index + 1;
        }
    }
    fields.push(&units[begin..]);
    fields
}

/// True when the subject is valid UTF-16 in the sense PCRE2 checks for a
/// UTF-mode match: every high surrogate must be followed by a low surrogate
/// and every low surrogate must follow one. Qt validates the whole subject
/// this way before any match attempt, and a lone surrogate anywhere in the
/// text makes every `globalMatch` yield zero matches (verified against
/// Qt 6.11.2 with subjects carrying lone units before, between and after
/// otherwise valid `{a}` tokens).
pub fn is_valid_utf16(text: &[u16]) -> bool {
    let mut index = 0;
    while index < text.len() {
        let unit = text[index];
        if !(0xD800..=0xDFFF).contains(&unit) {
            index += 1;
            continue;
        }
        let paired = matches!(unit, 0xD800..=0xDBFF)
            && index + 1 < text.len()
            && (0xDC00..=0xDFFF).contains(&text[index + 1]);
        if !paired {
            return false;
        }
        index += 2;
    }
    true
}

/// A half-open `[start, end)` range of UTF-16 code units matched by the
/// `{.*?}` global scan. `end` points behind the closing brace. Reused by the
/// later glossary/note stages, which iterate the same regex-space tokens.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TokenSpan {
    pub start: usize,
    pub end: usize,
}

/// Regex-equivalence scanner for `QRegularExpression("{.*?}").globalMatch`.
///
/// Equivalent by construction once the subject is valid UTF-16:
///
/// - PCRE2 scans for the required first unit `{`; after a successful match
///   iteration resumes behind the match, and after a failed start (no `}`
///   before the first LF) it resumes at the unit right behind that `{`. The
///   lazy scan only ever advances up to the first LF or `}` and `{` never
///   stops a scan, so no later candidate can be skipped by resuming that way.
/// - `.` matches any code point except LF. With PCRE2_UTF a surrogate pair is
///   one `.` step, but the only units with special meaning (`{`, `}`, LF) are
///   single BMP units that can never appear inside a pair, so the per-unit
///   walk below decides exactly the same matches (all subjects reaching here
///   are valid UTF-16, where lone units cannot occur).
/// - the lazy quantifier ends the match at the FIRST `}` after the `{`, even
///   when braces are nested: `{a{b}c}` matches `{a{b}`.
pub fn token_spans(text: &[u16]) -> Vec<TokenSpan> {
    if !is_valid_utf16(text) {
        return Vec::new();
    }
    let mut spans = Vec::new();
    let mut probe = 0;
    while probe < text.len() {
        let Some(offset) = text[probe..].iter().position(|&unit| unit == OPEN_BRACE) else {
            break;
        };
        let start = probe + offset;
        let mut end = None;
        let mut scan = start + 1;
        while scan < text.len() {
            if text[scan] == CLOSE_BRACE {
                end = Some((start, scan + 1));
                break;
            }
            if text[scan] == LF {
                break;
            }
            scan += 1;
        }
        match end {
            Some((start, end)) => {
                spans.push(TokenSpan { start, end });
                probe = end;
            }
            None => probe = start + 1,
        }
    }
    spans
}

#[cfg(test)]
mod tests
{
    use super::*;

    #[test]
    fn split_always_yields_at_least_one_field() {
        assert_eq!(split_keep_empty(b'|' as u16, &[]).len(), 1);
        assert_eq!(split_keep_empty(b':' as u16, &[]).len(), 1);
    }

    #[test]
    fn split_keeps_empty_fields_including_trailing() {
        let fields = split_keep_empty(b'|' as u16, &[7, 7, 7, 7, 7]);
        assert_eq!(fields, vec![&[7, 7, 7, 7, 7][..]]);
        assert_eq!(
            split_keep_empty(b'|' as u16, &[1, 124, 2]),
            vec![&[1][..], &[2][..]]
        );
    }

    #[test]
    fn trim_removals() {
        let text = [0x20, 0xA0, 7, 0x2005, 9];
        assert_eq!(trim_u16(&text), [7]);
        assert_eq!(trim_u16(&[0x2028]), &[] as &[u16]);
        // interior whitespace survives
        assert_eq!(trim_u16(&[1, 0x3000, 1]), [1, 0x3000, 1]);
    }

    #[test]
    fn space_set_membership() {
        for unit in [0x09, 0x0D, 0x20, 0x85, 0xA0, 0x1680, 0x2005, 0x202F, 0x3000] {
            assert!(qt_is_space(unit));
        }
        for unit in [0x00, 0x0E, 0x21, 0x7F, 0x200B] {
            assert!(!qt_is_space(unit));
        }
        // astral code points are never whitespace: their surrogate units are not
        assert!(!qt_is_space(0xD834));
        assert!(!qt_is_space(0xDD1E));
    }

    #[test]
    fn validity_checks() {
        assert!(is_valid_utf16(&[]));
        assert!(is_valid_utf16(&[0xD834, 0xDD1E]));
        assert!(is_valid_utf16(&[0xD800, 0xDD1E])); // any high+low value is a pair
        assert!(!is_valid_utf16(&[0xDC3D]));
        assert!(!is_valid_utf16(&[0xD800, 0xD800])); // high followed by high
        assert!(!is_valid_utf16(&[1, 0xD83D, 2]));
    }

    #[test]
    fn spans_basic_scan() {
        let text = String::from("x{a}y{b{c}d").encode_utf16().collect::<Vec<_>>();
        let spans = token_spans(&text);
        assert_eq!(spans.len(), 2);
        assert_eq!(String::from_utf16_lossy(&text[spans[0].start..spans[0].end]), "{a}");
        assert_eq!(String::from_utf16_lossy(&text[spans[1].start..spans[1].end]), "{b{c}");
    }

    #[test]
    fn spans_reject_lone_surrogates() {
        let text = vec![0x7B, 0x61, 0x7D, 0xDC3D];
        assert!(token_spans(&text).is_empty());
    }
}

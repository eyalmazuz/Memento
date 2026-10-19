//! Marker tokenizer: exact port of `Anki::Tokenizer::tokenize` from
//! `src/anki/markertokenizer.cpp`, line by line down to the dead branch.
//!
//! The C++ pipeline runs over `QString`/`QHash`; here raw UTF-16 code units
//! and insertion-ordered `Vec<(key, value)>` pairs with last-wins insertions
//! stand in for them, so lone surrogate text never round-trips through a
//! `String` (which is only defined for scalar-formed UTF-16).

use crate::strings::{
    split_keep_empty, token_spans, trim_u16, CLOSE_BRACE, COLON, COMMA, EQUALS,
    OPEN_BRACE, VERTICAL_BAR,
};

/// `constexpr qsizetype MAX_ARG_DELIM_COUNT{1}` — a marker field yielding
/// anything but `MAX_ARG_DELIM_COUNT + 1` split fields is abandoned together
/// with every marker field after it.
pub const MAX_ARG_DELIM_COUNT: usize = 1;
/// `constexpr qsizetype MAX_KEY_VALUE_DELIM_COUNT{1}` — an argument yielding
/// anything but `MAX_KEY_VALUE_DELIM_COUNT + 1` fields is skipped.
pub const MAX_KEY_VALUE_DELIM_COUNT: usize = 1;

/// `Anki::Tokenizer::Marker`: marker name plus an insertion-ordered argument
/// list overwriting duplicate keys (QHash last-wins insertion).
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Marker
{
    pub name: Vec<u16>,
    pub args: Vec<(Vec<u16>, Vec<u16>)>,
}

impl Marker
{
    /// `m.args.emplace(key, value)`: duplicate keys overwrite, last wins.
    pub fn insert_arg(&mut self, key: Vec<u16>, value: Vec<u16>)
    {
        for entry in self.args.iter_mut()
        {
            if entry.0 == key
            {
                entry.1 = value;
                return;
            }
        }
        self.args.push((key, value));
    }
}

/// `Anki::Tokenizer::Token`: the raw `{...}` text (including braces) plus the
/// markers collected before a later field was abandoned.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Token
{
    pub raw: Vec<u16>,
    pub markers: Vec<Marker>,
}

/// Get a list of tokens found in the text — a token exists only when it
/// collected at least one marker. Mirrors the C++ loop statement for
/// statement, including the dead `markerArgs.isEmpty()` branch (a split
/// always yields at least one field) and the abandon-on-overlong-field
/// `break` that keeps previously collected markers.
pub fn tokenize(text: &[u16]) -> Vec<Token>
{
    let mut tokens = Vec::new();
    for span in token_spans(text)
    {
        let raw = &text[span.start..span.end];
        debug_assert!(
            raw.len() >= 2 && raw[0] == OPEN_BRACE && raw[raw.len() - 1] == CLOSE_BRACE,
        );

        // QString::mid(1, size - 2): braces dropped, the content split on '|'
        // with empty fields kept.
        let token_text = &raw[1..raw.len() - 1];
        let mut markers = Vec::<Marker>::new();
        for field in split_keep_empty(VERTICAL_BAR, token_text)
        {
            let mut marker = Marker::default();
            let marker_args = split_keep_empty(COLON, field);
            if marker_args.is_empty()
            {
                // Dead code in C++ too: split never returns an empty list.
                break;
            }
            marker.name = trim_u16(marker_args[0]).to_vec();
            if marker_args.len() == 1
            {
                markers.push(marker);
                continue;
            }
            if marker_args.len() != MAX_ARG_DELIM_COUNT + 1
            {
                break;
            }
            for arg in split_keep_empty(COMMA, marker_args[1])
            {
                let kv = split_keep_empty(EQUALS, arg);
                if kv.len() != MAX_KEY_VALUE_DELIM_COUNT + 1
                {
                    continue;
                }
                marker.insert_arg(trim_u16(kv[0]).to_vec(), trim_u16(kv[1]).to_vec());
            }
            markers.push(marker);
        }

        if !markers.is_empty()
        {
            tokens.push(Token { raw: raw.to_vec(), markers });
        }
    }

    tokens
}

#[cfg(test)]
mod tests
{
    use super::*;
    use crate::strings::LF;

    fn str_units(s: &str) -> Vec<u16> { s.encode_utf16().collect() }

    /// Flatten tokenize output into (raws, marker names, args) as strings for
    /// readability.
    fn run(source: &[u16]) -> (
        Vec<Vec<u16>>,
        Vec<Vec<String>>,
        Vec<Vec<Vec<(String, String)>>>,
    )
    {
        let tokens = tokenize(source);
        (
            tokens.iter().map(|t| t.raw.clone()).collect(),
            tokens.iter().map(|t| t.markers.iter().map(|m| String::from_utf16_lossy(&m.name)).collect()).collect(),
            tokens.iter().map(|t| t.markers.iter().map(|m| {
                m.args.iter().map(|(k, v)| (String::from_utf16_lossy(k), String::from_utf16_lossy(v))).collect()
            }).collect()).collect(),
        )
    }

    fn run_str(source: &str) -> (
        Vec<Vec<u16>>,
        Vec<Vec<String>>,
        Vec<Vec<Vec<(String, String)>>>,
    )
    {
        run(&str_units(source))
    }

    #[test]
    fn empty_text_has_no_tokens()
    {
        assert!(tokenize(&[]).is_empty());
    }

    #[test]
    fn plain_text_has_no_tokens()
    {
        assert!(tokenize(&str_units("no tokens in plain text")).is_empty());
    }

    #[test]
    fn empty_token_carries_empty_marker()
    {
        let (raws, names, args) = run_str("{}");
        assert_eq!(raws, vec![str_units("{}")]);
        assert_eq!(names, vec![vec![String::new()]]);
        assert_eq!(args, vec![vec![vec![]]]);
    }

    #[test]
    fn simple_markers()
    {
        let (raws, names, args) = run_str("{glossary}");
        assert_eq!(raws, vec![str_units("{glossary}")]);
        assert_eq!(names, vec![vec!("glossary".to_string())]);
        assert_eq!(args, vec![vec![vec![]]]);

        let (raws, names, args) = run_str("{expression}");
        assert_eq!(raws, vec![str_units("{expression}")]);
        assert_eq!(names, vec![vec!("expression".to_string())]);
        assert_eq!(args, vec![vec![vec![]]]);
    }

    #[test]
    fn consecutive_tokens()
    {
        let (raws, names, _) = run_str("x{a}y{b}z");
        assert_eq!(raws, vec![str_units("{a}"), str_units("{b}")]);
        assert_eq!(names, vec![vec!("a".to_string()), vec!("b".to_string())]);
    }

    #[test]
    fn multi_marker_token()
    {
        let (_, names, _) = run_str("{glossary|reading}");
        assert_eq!(names, vec![vec!("glossary".to_string(), "reading".to_string())]);
    }

    #[test]
    fn audio_with_args()
    {
        let (raws, names, args) = run_str("{audio:source=custom,db=-10}");
        assert_eq!(raws, vec![str_units("{audio:source=custom,db=-10}")]);
        assert_eq!(names, vec![vec!("audio".to_string())]);
        assert_eq!(args, vec![vec![vec![
            ("source".to_string(), "custom".to_string()),
            ("db".to_string(), "-10".to_string()),
        ]]]);
    }

    #[test]
    fn marker_without_args_via_empty_value_part()
    {
        // "{furigana-plain:}": the empty value part splits into one empty
        // argument without '=', which is skipped — no args at all.
        let (_, names, args) = run_str("{furigana-plain:}");
        assert_eq!(names, vec![vec!("furigana-plain".to_string())]);
        assert_eq!(args, vec![vec![vec![]]]);
    }

    #[test]
    fn overlong_marker_field_abandons_rest()
    {
        // two ':' separators: the field is abandoned, later fields too, and
        // nothing is pushed for the token.
        assert!(tokenize(&str_units("{a:b:c}")).is_empty());
        //but markers collected before the abandon stay: one marker ("m" is
        // an argument without a '=' separator, so it is skipped).
        let (raws, names, args) = run_str("{a:m|x:b:c|c}");
        assert_eq!(raws, vec![str_units("{a:m|x:b:c|c}")]);
        assert_eq!(names, vec![vec!("a".to_string())]);
        assert_eq!(args, vec![vec![vec![]]]);
    }

    #[test]
    fn malformed_arguments_are_skipped()
    {
        // "x=1" and "=3" kept (an "=" split always yields two fields, so
        // "=3" becomes an empty key); "w", "+2" skipped (no '=' separator).
        let (_, names, args) = run_str("{audio:x=1,=3,w,+2}");
        assert_eq!(names, vec![vec!("audio".to_string())]);
        assert_eq!(args, vec![vec![vec![
            ("x".to_string(), "1".to_string()),
            (String::new(), "3".to_string()),
        ]]]);
    }

    #[test]
    fn duplicate_keys_last_wins()
    {
        let (_, _, args) = run_str("{a:k=1,k=2,k=3}");
        assert_eq!(args, vec![vec![vec![("k".to_string(), "3".to_string())]]]);
    }

    #[test]
    fn empty_and_edge_values()
    {
        // empty value kept
        let (_, _, args) = run_str("{a:k=}");
        assert_eq!(args, vec![vec![vec![("k".to_string(), String::new())]]]);
        // empty key kept
        let (_, _, args) = run_str("{a:=1}");
        assert_eq!(args, vec![vec![vec![(String::new(), "1".to_string())]]]);
        // whitespace-only value trimmed away
        let (_, _, args) = run_str("{a:k= }");
        assert_eq!(args, vec![vec![vec![("k".to_string(), String::new())]]]);
    }

    #[test]
    fn splitting_keeps_empty_marker_fields()
    {
        let (_, names, _) = run_str("{|}");
        assert_eq!(names, vec![vec![String::new(), String::new()]]);
        let (_, names, _) = run_str("{a|}");
        assert_eq!(names, vec![vec!("a".to_string(), String::new())]);
        let (_, names, _) = run_str("{a||}");
        assert_eq!(names, vec![vec!("a".to_string(), String::new(), String::new())]);
    }

    #[test]
    fn trimming_names_and_args()
    {
        // plain whitespace
        let (_, names, args) = run_str("{ a : k = 1 }");
        assert_eq!(names, vec![vec!("a".to_string())]);
        assert_eq!(args, vec![vec![vec![("k".to_string(), "1".to_string())]]]);
        // interior whitespace kept
        let (_, names, _) = run_str("{a b}");
        assert_eq!(names, vec![vec!("a b".to_string())]);
        // U+00A0 (NBSP) trimmed at the edges
        let (raws, names, _) = run(&[0x7B, 0xA0, 0x61, 0xA0, 0x7D]);
        assert_eq!(raws, vec![vec![0x7B, 0xA0, 0x61, 0xA0, 0x7D]]);
        assert_eq!(names, vec![vec!("a".to_string())]);
        // U+2005 (four-per-em space) trimmed at the edges
        let (_, names, _) = run(&[0x7B, 0x2005, 0x61, 0x2005, 0x7D]);
        assert_eq!(names, vec![vec!("a".to_string())]);
        // all-whitespace name collapses to empty, marker still pushed
        let (_, names, _) = run_str("{ }");
        assert_eq!(names, vec![vec![String::new()]]);
        // every trimming unit trims around a lone interior letter (LF used
        // inside braces would break the match, but tab, CR, VT, FF, NEL match)
        let (_, names, _) = run(&[0x7B, 9, 13, 0x61, 11, 12, 0x85, 0x7D]);
        assert_eq!(names, vec![vec!("a".to_string())]);
    }

    #[test]
    fn lf_breaks_the_token()
    {
        // the `.` of the token regex refuses only LF; an LF inside braces ends
        // the match attempt at the enclosing '{' (the rest is residue).
        assert!(tokenize(&str_units("{a\nb}")).is_empty());
        assert!(tokenize(&str_units("{\n}")).is_empty());
        // a token can resume after the failed start
        let (raws, names, _) = run_str("{a\nb}{c}");
        assert_eq!(raws, vec![str_units("{c}")]);
        assert_eq!(names, vec![vec!("c".to_string())]);
        // resume from behind the failed '{' reveals an inner token
        let (raws, names, _) = run_str("{a\n{b}");
        assert_eq!(raws, vec![str_units("{b}")]);
        assert_eq!(names, vec![vec!("b".to_string())]);
    }

    #[test]
    fn cr_still_matches_but_lf_does_not()
    {
        // CR alone: matched (dot excludes only LF, verified on Qt 6.11.2).
        let (raws, names, _) = run_str("{a\rb}");
        assert_eq!(raws, vec![vec![0x7B, 0x61, 0x0D, 0x62, 0x7D]]);
        assert_eq!(names, vec![vec!("a\rb".to_string())]);
        // CR before LF: the scan stops at the LF, no match.
        assert!(tokenize(&str_units("{a\r\nb}")).is_empty());
    }

    #[test]
    fn other_line_like_units_still_match()
    {
        // VT, FF, NEL, line and paragraph separators are matched by `.`.
        let (raws, names, _) = run(&[
            0x7B, 0x61, 0x0B, 0x0C, 0x85, 0x2028, 0x2029, 0x62, 0x7D,
        ]);
        assert_eq!(raws, vec![vec![0x7B, 0x61, 0x0B, 0x0C, 0x85, 0x2028, 0x2029, 0x62, 0x7D]]);
        assert_eq!(names, vec![vec!("a\u{b}\u{c}\u{85}\u{2028}\u{2029}b".to_string())]);
    }

    #[test]
    fn broken_and_one_sided_input()
    {
        assert!(tokenize(&str_units("{a")).is_empty());
        assert!(tokenize(&str_units("}")).is_empty());
        assert!(tokenize(&str_units("{")).is_empty());
        assert!(tokenize(&str_units("text}more{")).is_empty());
    }

    #[test]
    fn nested_braces_match_through_the_first_closer()
    {
        // the lazy scan sees 'a', '{', 'b', then the first '}' — braces nest
        // freely from `.`'s point of view, so the raw text has uneven braces.
        let (raws, names, _) = run_str("{a{b}c}");
        assert_eq!(raws, vec![str_units("{a{b}")]);
        assert_eq!(names, vec![vec!("a{b".to_string())]);
        // iteration resumes after the match, exposing a later token
        let (raws, names, _) = run_str("{a{b}c}{d}");
        assert_eq!(raws, vec![str_units("{a{b}"), str_units("{d}")]);
        assert_eq!(names, vec![vec!("a{b".to_string()), vec!("d".to_string())]);
        // an inner closer also ends a value
        let (_, names, args) = run_str("{a:source=x{y}}");
        assert_eq!(names, vec![vec!("a".to_string())]);
        assert_eq!(args, vec![vec![vec![("source".to_string(), "x{y".to_string())]]]);
    }

    #[test]
    fn cjk_text()
    {
        let (raws, names, _) = run_str("「{読み方}」");
        assert_eq!(raws, vec![str_units("{読み方}")]);
        assert_eq!(names, vec![vec!("読み方".to_string())]);
    }

    #[test]
    fn astral_codepoints()
    {
        // U+1D11E (MUSICAL SYMBOL G CLEF) is one code point, two UTF-16 units.
        let (raws, names, _) = run(&[0x7B, 0xD834, 0xDD1E, 0x7D]);
        assert_eq!(raws, vec![vec![0x7B, 0xD834, 0xDD1E, 0x7D]]);
        assert_eq!(names, vec![vec!("\u{1D11E}".to_string())]);
    }

    #[test]
    fn lone_surrogates_reject_the_whole_subject()
    {
        // Qt validates the whole subject before matching (PCRE2 UTF mode):
        // with a lone surrogate anywhere, nothing matches at all — verified
        // against Qt 6.11.2 with lone units before, between and after valid
        // tokens. The per-unit rule is documented in strings.rs.
        assert!(tokenize(&[0x7B, 0xDC3D, 0x7D]).is_empty());
        assert!(tokenize(&[0x7B, 0x61, 0x7D, 0xDC3D, 0x7B, 0x62, 0x7D]).is_empty());
        assert!(tokenize(&[0x7B, 0x61, 0xDD3D, 0x7D]).is_empty());
        // valid pairs still pass
        let (_, names, _) = run(&[0x7B, 0xD83D, 0xDE00, 0x7D]);
        assert_eq!(names, vec![vec!("\u{1F600}".to_string())]);
    }

    #[test]
    fn delimiter_constants_cover_the_documented_units()
    {
        // parity with the C++ constexpr counters
        assert_eq!(MAX_ARG_DELIM_COUNT, 1);
        assert_eq!(MAX_KEY_VALUE_DELIM_COUNT, 1);
        assert_eq!(OPEN_BRACE as u32, u32::from('{'));
        assert_eq!(CLOSE_BRACE as u32, u32::from('}'));
        assert_eq!(VERTICAL_BAR as u32, u32::from('|'));
        assert_eq!(COLON as u32, u32::from(':'));
        assert_eq!(COMMA as u32, u32::from(','));
        assert_eq!(EQUALS as u32, u32::from('='));
        assert_eq!(LF as u32, u32::from('\n'));
    }

    #[test]
    fn insertion_overwrites_last_wins_first_slot()
    {
        let mut marker = Marker::default();
        marker.insert_arg(str_units("k"), str_units("1"));
        marker.insert_arg(str_units("other"), str_units("x"));
        marker.insert_arg(str_units("k"), str_units("2"));
        assert_eq!(
            marker.args.iter().map(|(k, v)| (k.clone(), v.clone())).collect::<Vec<_>>(),
            vec![(str_units("k"), str_units("2")), (str_units("other"), str_units("x"))]
        );
    }
}

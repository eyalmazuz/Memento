//! Anki template marker tokenizer (`{marker:key=val|fallback}`).

/// Maximum `:` separators permitted in a single marker field before abandoning the rest.
pub const MAX_ARG_DELIM_COUNT: usize = 1;
/// Maximum `=` separators permitted in a single marker argument.
pub const MAX_KEY_VALUE_DELIM_COUNT: usize = 1;

/// A half-open UTF-8 byte range `[start, end)` matching a single `{...}` token
/// on a single line (no `\n` inside).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TokenSpan {
    pub start: usize,
    pub end: usize,
}

/// Scans `text` for non-overlapping `{...}` tokens that do not span `\n`.
/// Matches lazily to the first `}` after `{`.
pub fn token_spans(text: &str) -> Vec<TokenSpan> {
    let bytes = text.as_bytes();
    let mut spans = Vec::new();
    let mut probe = 0;
    while probe < bytes.len() {
        let Some(offset) = bytes[probe..].iter().position(|&b| b == b'{') else {
            break;
        };
        let start = probe + offset;
        let mut end = None;
        let mut scan = start + 1;
        while scan < bytes.len() {
            match bytes[scan] {
                b'}' => {
                    end = Some((start, scan + 1));
                    break;
                }
                b'\n' => break,
                _ => scan += 1,
            }
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

/// Parsed template marker with name and insertion-ordered arguments
/// (duplicate keys overwrite in place, last wins).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Marker {
    pub name: String,
    pub args: Vec<(String, String)>,
}

impl Marker {
    /// Inserts or overwrites an argument value for `key` (last wins).
    pub fn insert_arg(&mut self, key: String, value: String) {
        if let Some(entry) = self.args.iter_mut().find(|entry| entry.0 == key) {
            entry.1 = value;
        } else {
            self.args.push((key, value));
        }
    }

    /// Looks up an argument by exact key name.
    pub fn arg(&self, key: &str) -> Option<&str> {
        self.args
            .iter()
            .find_map(|(k, v)| (k == key).then_some(v.as_str()))
    }
}

/// A `{...}` template token with its raw brace-enclosed slice and parsed alternative markers.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Token {
    pub raw: String,
    pub markers: Vec<Marker>,
}

/// Extracts all valid `{...}` marker tokens from `text`.
pub fn tokenize(text: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    for span in token_spans(text) {
        let raw = &text[span.start..span.end];
        let token_text = &raw[1..raw.len() - 1];
        let mut markers = Vec::new();

        for field in token_text.split('|') {
            let mut parts = field.split(':');
            let Some(raw_name) = parts.next() else {
                break;
            };
            let mut marker = Marker {
                name: raw_name.trim().to_owned(),
                args: Vec::new(),
            };
            let Some(raw_arg_list) = parts.next() else {
                markers.push(marker);
                continue;
            };
            if parts.next().is_some() {
                break;
            }
            for arg in raw_arg_list.split(',') {
                let mut kv = arg.split('=');
                if let (Some(k), Some(v), None) = (kv.next(), kv.next(), kv.next()) {
                    marker.insert_arg(k.trim().to_owned(), v.trim().to_owned());
                }
            }
            markers.push(marker);
        }

        if !markers.is_empty() {
            tokens.push(Token {
                raw: raw.to_owned(),
                markers,
            });
        }
    }
    tokens
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(source: &str) -> (Vec<String>, Vec<Vec<String>>, Vec<Vec<Vec<(String, String)>>>) {
        let tokens = tokenize(source);
        (
            tokens.iter().map(|t| t.raw.clone()).collect(),
            tokens
                .iter()
                .map(|t| t.markers.iter().map(|m| m.name.clone()).collect())
                .collect(),
            tokens
                .iter()
                .map(|t| t.markers.iter().map(|m| m.args.clone()).collect())
                .collect(),
        )
    }

    #[test]
    fn empty_and_plain_text_have_no_tokens() {
        assert!(tokenize("").is_empty());
        assert!(tokenize("no tokens in plain text").is_empty());
    }

    #[test]
    fn empty_token_carries_empty_marker() {
        let (raws, names, args) = run("{}");
        assert_eq!(raws, vec!["{}"]);
        assert_eq!(names, vec![vec![""]]);
        assert_eq!(args, vec![vec![vec![]]]);
    }

    #[test]
    fn simple_and_consecutive_tokens() {
        let (raws, names, args) = run("{glossary}");
        assert_eq!(raws, vec!["{glossary}"]);
        assert_eq!(names, vec![vec!["glossary"]]);
        assert_eq!(args, vec![vec![vec![]]]);

        let (raws, names, _) = run("x{a}y{b}z");
        assert_eq!(raws, vec!["{a}", "{b}"]);
        assert_eq!(names, vec![vec!["a"], vec!["b"]]);
    }

    #[test]
    fn multi_marker_and_args() {
        let (_, names, _) = run("{glossary|reading}");
        assert_eq!(names, vec![vec!["glossary", "reading"]]);

        let (raws, names, args) = run("{audio:source=custom,db=-10}");
        assert_eq!(raws, vec!["{audio:source=custom,db=-10}"]);
        assert_eq!(names, vec![vec!["audio"]]);
        assert_eq!(
            args,
            vec![vec![vec![
                ("source".to_owned(), "custom".to_owned()),
                ("db".to_owned(), "-10".to_owned()),
            ]]]
        );

        let (_, names, args) = run("{furigana-plain:}");
        assert_eq!(names, vec![vec!["furigana-plain"]]);
        assert_eq!(args, vec![vec![vec![]]]);
    }

    #[test]
    fn overlong_marker_field_abandons_rest() {
        assert!(tokenize("{a:b:c}").is_empty());
        let (raws, names, args) = run("{a:m|x:b:c|c}");
        assert_eq!(raws, vec!["{a:m|x:b:c|c}"]);
        assert_eq!(names, vec![vec!["a"]]);
        assert_eq!(args, vec![vec![vec![]]]);
    }

    #[test]
    fn malformed_duplicate_and_empty_arguments() {
        let (_, names, args) = run("{audio:x=1,=3,w,+2}");
        assert_eq!(names, vec![vec!["audio"]]);
        assert_eq!(
            args,
            vec![vec![vec![
                ("x".to_owned(), "1".to_owned()),
                (String::new(), "3".to_owned()),
            ]]]
        );

        let (_, _, args) = run("{a:k=1,k=2,k=3}");
        assert_eq!(args, vec![vec![vec![("k".to_owned(), "3".to_owned())]]]);

        let (_, _, args) = run("{a:k=}");
        assert_eq!(args, vec![vec![vec![("k".to_owned(), String::new())]]]);
        let (_, _, args) = run("{a:=1}");
        assert_eq!(args, vec![vec![vec![(String::new(), "1".to_owned())]]]);
        let (_, _, args) = run("{a:k= }");
        assert_eq!(args, vec![vec![vec![("k".to_owned(), String::new())]]]);
    }

    #[test]
    fn splitting_keeps_empty_marker_fields() {
        let (_, names, _) = run("{|}");
        assert_eq!(names, vec![vec!["", ""]]);
        let (_, names, _) = run("{a|}");
        assert_eq!(names, vec![vec!["a", ""]]);
        let (_, names, _) = run("{a||}");
        assert_eq!(names, vec![vec!["a", "", ""]]);
    }

    #[test]
    fn trimming_unicode_whitespace() {
        let (_, names, args) = run("{ a : k = 1 }");
        assert_eq!(names, vec![vec!["a"]]);
        assert_eq!(args, vec![vec![vec![("k".to_owned(), "1".to_owned())]]]);

        let (_, names, _) = run("{a b}");
        assert_eq!(names, vec![vec!["a b"]]);

        let (_, names, _) = run("{\u{00A0}a\u{00A0}}");
        assert_eq!(names, vec![vec!["a"]]);

        let (_, names, _) = run("{\u{2005}a\u{2005}}");
        assert_eq!(names, vec![vec!["a"]]);

        let (_, names, _) = run("{\t\ra\x0b\x0c\u{0085}}");
        assert_eq!(names, vec![vec!["a"]]);

        let (_, names, _) = run("{ }");
        assert_eq!(names, vec![vec![""]]);
    }

    #[test]
    fn lf_breaks_tokens_while_cr_and_other_chars_match() {
        assert!(tokenize("{a\nb}").is_empty());
        assert!(tokenize("{\n}").is_empty());
        assert!(tokenize("{a\r\nb}").is_empty());

        let (raws, names, _) = run("{a\nb}{c}");
        assert_eq!(raws, vec!["{c}"]);
        assert_eq!(names, vec![vec!["c"]]);

        let (raws, names, _) = run("{a\n{b}");
        assert_eq!(raws, vec!["{b}"]);
        assert_eq!(names, vec![vec!["b"]]);

        let (raws, names, _) = run("{a\rb}");
        assert_eq!(raws, vec!["{a\rb}"]);
        assert_eq!(names, vec![vec!["a\rb"]]);
    }

    #[test]
    fn broken_nested_and_unicode_tokens() {
        assert!(tokenize("{a").is_empty());
        assert!(tokenize("}").is_empty());
        assert!(tokenize("{").is_empty());
        assert!(tokenize("text}more{").is_empty());

        let (raws, names, _) = run("{a{b}c}{d}");
        assert_eq!(raws, vec!["{a{b}", "{d}"]);
        assert_eq!(names, vec![vec!["a{b"], vec!["d"]]);

        let (_, names, args) = run("{a:source=x{y}}");
        assert_eq!(names, vec![vec!["a"]]);
        assert_eq!(args, vec![vec![vec![("source".to_owned(), "x{y".to_owned())]]]);

        let (raws, names, _) = run("「{読み方}」{\u{1D11E}}");
        assert_eq!(raws, vec!["{読み方}", "{\u{1D11E}}"]);
        assert_eq!(names, vec![vec!["読み方"], vec!["\u{1D11E}"]]);
    }

    #[test]
    fn insertion_overwrites_in_place() {
        let mut marker = Marker::default();
        marker.insert_arg("k".to_owned(), "1".to_owned());
        marker.insert_arg("other".to_owned(), "x".to_owned());
        marker.insert_arg("k".to_owned(), "2".to_owned());
        assert_eq!(
            marker.args,
            vec![
                ("k".to_owned(), "2".to_owned()),
                ("other".to_owned(), "x".to_owned()),
            ]
        );
        assert_eq!(marker.arg("k"), Some("2"));
        assert_eq!(marker.arg("missing"), None);
    }
}

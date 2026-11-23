//! Longest-to-shortest exact prefix query generator.

/// One exact dictionary query; no conjugation filter or explanation applies.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExactQuery {
    pub deconj: String,
    // Unmodified prefix queries share their surface with `deconj`.
    surface_override: Option<String>,
}

impl ExactQuery {
    pub fn surface(&self) -> &str {
        self.surface_override.as_deref().unwrap_or(&self.deconj)
    }
}

#[inline]
fn is_ascii_ws(ch: char) -> bool {
    matches!(ch, '\t'..='\r' | ' ')
}

/// Generates longest-to-shortest Unicode character prefixes and whitespace-free
/// variants for `text`.
pub fn generate_queries(text: &str) -> Vec<ExactQuery> {
    if text.is_empty() {
        return Vec::new();
    }
    let first_ws = text.find(is_ascii_ws);
    let ends: Vec<usize> = text
        .char_indices()
        .map(|(idx, ch)| idx + ch.len_utf8())
        .collect();
    let mut result = Vec::with_capacity(ends.len());
    for end in ends.into_iter().rev() {
        let prefix = &text[..end];
        result.push(ExactQuery {
            deconj: prefix.to_owned(),
            surface_override: None,
        });
        if first_ws.is_some_and(|ws_idx| ws_idx < end) {
            let without_whitespace: String =
                prefix.chars().filter(|&ch| !is_ascii_ws(ch)).collect();
            result.push(ExactQuery {
                deconj: without_whitespace,
                surface_override: Some(prefix.trim().to_owned()),
            });
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefixes_preserve_whitespace_and_variants() {
        let result = generate_queries(" あ b");
        let pairs: Vec<(&str, &str)> = result
            .iter()
            .map(|q| (q.deconj.as_str(), q.surface()))
            .collect();
        assert_eq!(
            pairs,
            vec![
                (" あ b", " あ b"),
                ("あb", "あ b"),
                (" あ ", " あ "),
                ("あ", "あ"),
                (" あ", " あ"),
                ("あ", "あ"),
                (" ", " "),
                ("", ""),
            ]
        );
        assert!(generate_queries("").is_empty());
    }

    #[test]
    fn handles_supplementary_unicode_characters_cleanly() {
        let result = generate_queries("😀");
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].deconj, "😀");

        let res_ext = generate_queries("𠮟る");
        assert_eq!(res_ext.len(), 2);
        assert_eq!(res_ext[0].deconj, "𠮟る");
        assert_eq!(res_ext[1].deconj, "𠮟");

        let res_hj = generate_queries("\u{31350}\u{323B0}");
        assert_eq!(res_hj.len(), 2);
        assert_eq!(res_hj[0].deconj, "\u{31350}\u{323B0}");
        assert_eq!(res_hj[1].deconj, "\u{31350}");
    }
}

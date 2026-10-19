//! Pure prefix search queries from `src/dict/exactquerygenerator.cpp`.
//! Qt's `QString::chop(1)` counts UTF-16 code units, not Unicode scalars.

/// One exact dictionary query; no conjugation filter or explanation applies.
#[derive(Debug, Eq, PartialEq)]
pub struct ExactQuery {
    pub deconj: Vec<u16>,
    // Unmodified prefix queries share their surface with `deconj`.
    surface_override: Option<Vec<u16>>,
}

impl ExactQuery {
    pub fn surface(&self) -> &[u16] {
        self.surface_override.as_deref().unwrap_or(&self.deconj)
    }

    pub(crate) fn into_parts(self) -> (Vec<u16>, Option<Vec<u16>>) {
        (self.deconj, self.surface_override)
    }
}

fn regex_whitespace(unit: u16) -> bool {
    matches!(unit, 0x0009..=0x000d | 0x0020)
}

fn trim_whitespace(unit: u16) -> bool {
    unit != 0x0085 && char::from_u32(u32::from(unit)).is_some_and(char::is_whitespace)
}


/// Generate longest-to-shortest UTF-16 prefixes and whitespace-free variants.
pub fn generate_queries(text: &[u16]) -> Vec<ExactQuery> {
    let mut result = Vec::new();
    for end in (1..=text.len()).rev() {
        let prefix = &text[..end];
        result.push(ExactQuery { deconj: prefix.to_vec(), surface_override: None });
        if prefix.iter().any(|unit| regex_whitespace(*unit)) &&
            char::decode_utf16(prefix.iter().copied()).all(|unit| unit.is_ok()) {
            let without_whitespace: Vec<u16> = prefix.iter().copied()
                .filter(|unit| !regex_whitespace(*unit)).collect();
            let left = prefix.iter().position(|unit| !trim_whitespace(*unit));
            let right = prefix.iter().rposition(|unit| !trim_whitespace(*unit));
            let trimmed = left.zip(right)
                .map(|(start, stop)| prefix[start..=stop].to_vec())
                .unwrap_or_default();
            result.push(ExactQuery {
                deconj: without_whitespace, surface_override: Some(trimmed),
            });
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefixes_preserve_whitespace_and_utf16_units() {
        let input: Vec<u16> = " あ b".encode_utf16().collect();
        let result = generate_queries(&input);
        let pairs: Vec<(String, String)> = result.iter().map(|q| (
            String::from_utf16(&q.deconj).unwrap(),
            String::from_utf16(q.surface()).unwrap(),
        )).collect();
        assert_eq!(pairs, vec![
            (" あ b".into(), " あ b".into()),
            ("あb".into(), "あ b".into()),
            (" あ ".into(), " あ ".into()),
            ("あ".into(), "あ".into()),
            (" あ".into(), " あ".into()),
            ("あ".into(), "あ".into()),
            (" ".into(), " ".into()),
            ("".into(), "".into()),
        ]);
        assert!(generate_queries(&[]).is_empty());
    }

    #[test]
    fn splitting_surrogate_pair_matches_qstring_chop() {
        let input: Vec<u16> = "😀".encode_utf16().collect();
        let result = generate_queries(&input);
        assert_eq!(result[0].deconj, input);
        assert_eq!(result[1].deconj, vec![input[0]]);
    }

    #[test]
    fn nel_u0085_is_not_trimmed_matching_qchar_is_space() {
        // " \u{0085} ": Qt trimmed() removes ASCII spaces but keeps U+0085
        let input: Vec<u16> = " \u{0085} ".encode_utf16().collect();
        let result = generate_queries(&input);
        // Prefix 0: " \u{0085} " -> stripped deconj: "\u{0085}", surface: trimmed " \u{0085} " -> "\u{0085}"
        assert_eq!(result[0].deconj, input);
        assert_eq!(result[1].deconj, vec![0x0085]);
        assert_eq!(result[1].surface(), &[0x0085]);
    }
}


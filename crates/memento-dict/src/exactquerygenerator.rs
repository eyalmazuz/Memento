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
}

fn regex_whitespace(unit: u16) -> bool {
    matches!(unit, 0x0009..=0x000d | 0x0020)
}

fn trim_whitespace(unit: u16) -> bool {
    unit != 0x0085 && char::from_u32(u32::from(unit)).is_some_and(char::is_whitespace)
}


/// Returns `true` if `end` falls between the high and low surrogate of a valid UTF-16 pair.
#[inline]
fn splits_surrogate_pair(text: &[u16], end: usize) -> bool {
    end > 0
        && end < text.len()
        && (0xD800..=0xDBFF).contains(&text[end - 1])
        && (0xDC00..=0xDFFF).contains(&text[end])
}

/// Returns the index of the first unpaired UTF-16 surrogate, or `text.len()` if well-formed.
fn first_unpaired_utf16(text: &[u16]) -> usize {
    let mut i = 0;
    while i < text.len() {
        let unit = text[i];
        if !(0xD800..=0xDFFF).contains(&unit) {
            i += 1;
            continue;
        }
        if (0xD800..=0xDBFF).contains(&unit)
            && i + 1 < text.len()
            && (0xDC00..=0xDFFF).contains(&text[i + 1])
        {
            i += 2;
            continue;
        }
        return i;
    }
    text.len()
}

/// Generate longest-to-shortest UTF-16 prefixes and whitespace-free variants
/// without splitting valid UTF-16 surrogate pairs.
pub fn generate_queries(text: &[u16]) -> Vec<ExactQuery> {
    let first_unpaired = first_unpaired_utf16(text);
    let first_ws = text.iter().position(|&unit| regex_whitespace(unit));
    let mut result = Vec::with_capacity(text.len());
    for end in (1..=text.len()).rev() {
        if splits_surrogate_pair(text, end) {
            continue;
        }
        let prefix = &text[..end];
        result.push(ExactQuery {
            deconj: prefix.to_vec(),
            surface_override: None,
        });
        if end <= first_unpaired && first_ws.is_some_and(|ws_idx| ws_idx < end) {
            let without_whitespace: Vec<u16> = prefix
                .iter()
                .copied()
                .filter(|unit| !regex_whitespace(*unit))
                .collect();
            let left = prefix.iter().position(|unit| !trim_whitespace(*unit));
            let right = prefix.iter().rposition(|unit| !trim_whitespace(*unit));
            let trimmed = left
                .zip(right)
                .map(|(start, stop)| prefix[start..=stop].to_vec())
                .unwrap_or_default();
            result.push(ExactQuery {
                deconj: without_whitespace,
                surface_override: Some(trimmed),
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
    fn does_not_split_surrogate_pair() {
        let input: Vec<u16> = "😀".encode_utf16().collect();
        let result = generate_queries(&input);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].deconj, input);

        let cjk_ext: Vec<u16> = "𠮟る".encode_utf16().collect();
        let res_ext = generate_queries(&cjk_ext);
        assert_eq!(res_ext.len(), 2);
        assert_eq!(String::from_utf16(&res_ext[0].deconj).unwrap(), "𠮟る");
        assert_eq!(String::from_utf16(&res_ext[1].deconj).unwrap(), "𠮟");

        // CJK Extension H (U+31350) & Extension J (U+323B0) surrogate pairs (PR #2522):
        let ext_hj: Vec<u16> = "\u{31350}\u{323B0}".encode_utf16().collect();
        let res_hj = generate_queries(&ext_hj);
        assert_eq!(res_hj.len(), 2);
        assert_eq!(
            String::from_utf16(&res_hj[0].deconj).unwrap(),
            "\u{31350}\u{323B0}"
        );
        assert_eq!(String::from_utf16(&res_hj[1].deconj).unwrap(), "\u{31350}");
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


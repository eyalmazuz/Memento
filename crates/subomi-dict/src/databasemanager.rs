//! Dictionary pitch/mora segmentation, stylesheet resolution, and JSON array helpers.

use std::path::{Component, Path, PathBuf};

pub const KATAKANA_LOW: u32 = 0x30A1;
pub const KATAKANA_HIGH: u32 = 0x30F6;
pub const HIRAGANA_LOW: u32 = 0x3041;
pub const HIRAGANA_HIGH: u32 = 0x3096;

/// Small kana and combining dakuten/handakuten characters that attach to the
/// preceding base kana rather than forming an independent mora:
/// ぁ ぃ ぅ ぇ ぉ ゃ ゅ ょ ゎ ゕ ゖ ァ ィ ゥ ェ ォ ャ ュ ョ ヮ ヵ ヶ U+3099 U+309A
pub const MORA_SKIP_CHARS: [char; 24] = [
    '\u{3041}', // ぁ
    '\u{3043}', // ぃ
    '\u{3045}', // ぅ
    '\u{3047}', // ぇ
    '\u{3049}', // ぉ
    '\u{3083}', // ゃ
    '\u{3085}', // ゅ
    '\u{3087}', // ょ
    '\u{308E}', // ゎ
    '\u{3095}', // ゕ
    '\u{3096}', // ゖ
    '\u{3099}', // combining voiced sound mark (dakuten)
    '\u{309A}', // combining semi-voiced sound mark (handakuten / nasal velar)
    '\u{30A1}', // ァ
    '\u{30A3}', // ィ
    '\u{30A5}', // ゥ
    '\u{30A7}', // ェ
    '\u{30A9}', // ォ
    '\u{30E3}', // ャ
    '\u{30E5}', // ュ
    '\u{30E7}', // ョ
    '\u{30EE}', // ヮ
    '\u{30F5}', // ヵ
    '\u{30F6}', // ヶ
];

#[inline]
#[must_use]
pub fn is_mora_skip_char(ch: char) -> bool {
    MORA_SKIP_CHARS.contains(&ch)
}

/// Splits a UTF-8 reading into mora strings according to Japanese pitch accent
/// segmentation rules.
#[must_use]
pub fn split_mora_str(reading: &str) -> Vec<String> {
    let mut mora: Vec<String> = Vec::new();
    let mut current_mora = String::new();

    for ch in reading.chars() {
        let is_skip = is_mora_skip_char(ch);
        if !current_mora.is_empty() && !is_skip {
            mora.push(std::mem::take(&mut current_mora));
        }
        current_mora.push(ch);
    }
    if !current_mora.is_empty() {
        mora.push(current_mora);
    }
    mora
}

/// Converts a raw JSON array of strings to a `Vec<String>`.
#[must_use]
pub fn json_array_to_utf8_strings(json_bytes: &[u8]) -> Vec<String> {
    let Ok(serde_json::Value::Array(arr)) = serde_json::from_slice(json_bytes) else {
        return Vec::new();
    };
    arr.into_iter()
        .filter_map(|val| match val {
            serde_json::Value::String(s) => Some(s),
            _ => None,
        })
        .collect()
}

/// Yomitan `term_meta_bank_v3` pitch position: either a numeric downstep index
/// (`0` = heiban, `1..=N` = downstep mora) or an exact high/low pattern string
/// (`"LHH"`, `"HLL"`, `"LHHL"`, etc.).
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum PitchPosition {
    Downstep(u32),
    Pattern(String),
}

impl PitchPosition {
    /// Returns `true` if `s` is a valid non-empty Yomitan pitch pattern string (`^[HLhl]+$`).
    #[must_use]
    pub fn is_valid_pattern(s: &str) -> bool {
        !s.is_empty() && s.bytes().all(|b| matches!(b, b'H' | b'L' | b'h' | b'l'))
    }

    /// Parses a Yomitan `term_meta_bank_v3` `"position"` JSON value (integer downstep or
    /// exact `"H"`/`"L"` pattern string).
    #[must_use]
    pub fn from_json_value(val: &serde_json::Value) -> Option<Self> {
        if let Some(s) = val.as_str() {
            return Self::is_valid_pattern(s).then(|| Self::Pattern(s.to_ascii_uppercase()));
        }
        let n = val.as_f64()?;
        if n.is_finite() && n.fract() == 0.0 && n >= 0.0 && n <= f64::from(u32::MAX) {
            Some(Self::Downstep(n as u32))
        } else {
            None
        }
    }

    #[must_use]
    pub const fn as_downstep(&self) -> Option<u32> {
        match self {
            Self::Downstep(pos) => Some(*pos),
            Self::Pattern(_) => None,
        }
    }

    #[must_use]
    pub fn as_pattern(&self) -> Option<&str> {
        match self {
            Self::Downstep(_) => None,
            Self::Pattern(pat) => Some(pat.as_str()),
        }
    }

    /// Expands this pitch position into boolean high/low pitch levels (`true` = High, `false` = Low).
    #[must_use]
    pub fn to_levels(&self, mora_count: usize) -> Vec<bool> {
        match self {
            Self::Pattern(pat) => pat.bytes().map(|b| b == b'H' || b == b'h').collect(),
            Self::Downstep(downstep) => {
                if mora_count == 0 {
                    return Vec::new();
                }
                let total = mora_count + 1;
                let mut levels = Vec::with_capacity(total);
                match *downstep {
                    0 => {
                        levels.push(false);
                        for _ in 1..total {
                            levels.push(true);
                        }
                    }
                    1 => {
                        levels.push(true);
                        for _ in 1..total {
                            levels.push(false);
                        }
                    }
                    n => {
                        let drop_after = n as usize;
                        levels.push(false);
                        for i in 1..total {
                            levels.push(i < drop_after);
                        }
                    }
                }
                levels
            }
        }
    }
}

/// One pitch pattern inside a Yomitan `pitches` JSON array.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PitchEntryDetail {
    pub position: i32,
    /// Exact high/low pitch pattern string (`"LHH"`, `"HLL"`, `"LHHL"`, etc.) from Yomitan
    /// `term_meta_bank_v3`, or `None` for integer downstep entries.
    pub pattern: Option<String>,
    pub nasal: Vec<usize>,
    pub devoice: Vec<usize>,
    pub tags: Vec<String>,
}

impl PitchEntryDetail {
    /// Returns the typed [`PitchPosition`] (`Pattern` when an exact string pattern was present,
    /// otherwise `Downstep` for non-negative integer positions).
    #[must_use]
    pub fn pitch_position(&self) -> Option<PitchPosition> {
        if let Some(ref pat) = self.pattern {
            Some(PitchPosition::Pattern(pat.clone()))
        } else {
            u32::try_from(self.position)
                .ok()
                .map(PitchPosition::Downstep)
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PitchBlobUtf8Data {
    pub matched: bool,
    pub exact_reading_match: bool,
    pub has_reading_key: bool,
    pub mora: Vec<String>,
    pub entries: Vec<PitchEntryDetail>,
}

fn parse_mora_index_list(val: Option<&serde_json::Value>) -> Vec<usize> {
    fn to_usize(v: &serde_json::Value) -> Option<usize> {
        let n = v.as_f64()?;
        if n.fract() == 0.0 && n >= 0.0 && n <= usize::MAX as f64 {
            Some(n as usize)
        } else {
            None
        }
    }
    match val {
        Some(serde_json::Value::Array(arr)) => arr.iter().filter_map(to_usize).collect(),
        Some(single) => to_usize(single).into_iter().collect(),
        None => Vec::new(),
    }
}

fn parse_pitch_tags(val: Option<&serde_json::Value>) -> Vec<String> {
    match val {
        Some(serde_json::Value::Array(arr)) => arr
            .iter()
            .filter_map(|item| item.as_str().map(str::to_owned))
            .filter(|s| !s.is_empty())
            .collect(),
        Some(serde_json::Value::String(s)) => s
            .split_whitespace()
            .filter(|t| !t.is_empty())
            .map(str::to_owned)
            .collect(),
        _ => Vec::new(),
    }
}

fn parse_pitch_entries(obj: &serde_json::Map<String, serde_json::Value>) -> Vec<PitchEntryDetail> {
    let Some(arr) = obj.get("pitches").and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    arr.iter()
        .map(|val| {
            let Some(o) = val.as_object() else {
                return PitchEntryDetail::default();
            };
            let pos_val = o.get("position");
            let pattern = pos_val
                .and_then(|p| p.as_str())
                .filter(|s| PitchPosition::is_valid_pattern(s))
                .map(str::to_ascii_uppercase);
            let position = pos_val
                .and_then(|p| p.as_f64())
                .filter(|&position| {
                    position.fract() == 0.0
                        && position >= f64::from(i32::MIN)
                        && position <= f64::from(i32::MAX)
                })
                .map(|position| position as i32)
                .unwrap_or(0);
            let nasal = parse_mora_index_list(o.get("nasal"));
            let devoice = parse_mora_index_list(o.get("devoice"));
            let tags = parse_pitch_tags(o.get("tags"));
            PitchEntryDetail {
                position,
                pattern,
                nasal,
                devoice,
                tags,
            }
        })
        .collect()
}

/// Checks whether a `term_meta_bank` JSON `"reading"` value matches `(expression, caller_reading)`
/// without leaking homographs when `caller_reading` is empty or non-empty.
///
/// Returns `(matched, exact_reading_match)`.
#[must_use]
pub fn metadata_reading_matches(
    expression: &str,
    caller_reading: &str,
    meta_reading: Option<&str>,
) -> (bool, bool) {
    match meta_reading {
        Some(r) => {
            if !caller_reading.is_empty() {
                let exact = r == caller_reading;
                (exact, exact)
            } else {
                let matched = r.is_empty()
                    || r == expression
                    || crate::normalize::normalized_readings_eq(r, expression);
                (matched, matched && !r.is_empty())
            }
        }
        None => (true, false),
    }
}

/// Parses pitch accent JSON blob in UTF-8 with strict homograph reading checks.
#[must_use]
pub fn parse_pitch_blob_utf8(
    json_bytes: &[u8],
    expression: &str,
    caller_reading: &str,
) -> PitchBlobUtf8Data {
    let Ok(serde_json::Value::Object(obj)) = serde_json::from_slice::<serde_json::Value>(json_bytes)
    else {
        return PitchBlobUtf8Data::default();
    };

    let meta_reading = obj.get("reading").and_then(|v| v.as_str());
    let has_reading_key = meta_reading.is_some();
    let (matched, exact_reading_match) =
        metadata_reading_matches(expression, caller_reading, meta_reading);
    if !matched {
        return PitchBlobUtf8Data {
            has_reading_key,
            ..PitchBlobUtf8Data::default()
        };
    }

    let reading_for_mora = match meta_reading {
        Some(r) if !r.is_empty() => r,
        _ if !caller_reading.is_empty() => caller_reading,
        _ => expression,
    };
    let mora = split_mora_str(reading_for_mora);
    let entries = parse_pitch_entries(&obj);

    PitchBlobUtf8Data {
        matched: true,
        exact_reading_match,
        has_reading_key,
        mora,
        entries,
    }
}

fn is_safe_single_dir_component(name: &str) -> bool {
    if name.is_empty() || name.contains('/') || name.contains('\\') || name.contains('\0') {
        return false;
    }
    let mut comps = Path::new(name).components();
    matches!(comps.next(), Some(Component::Normal(_))) && comps.next().is_none()
}

/// Resolves a dictionary's `styles.css` on disk, preferring `res_dir/dict-{dic_id}/styles.css`
/// (matching [`crate::yomidbbuilder::dictionary_resource_dir`]) and safely falling back to
/// `res_dir/{dict_name}/styles.css`.
#[must_use]
pub fn resolve_dictionary_style_path(
    res_dir: &Path,
    dic_id: i64,
    dict_name: &str,
) -> Option<PathBuf> {
    if res_dir.as_os_str().is_empty() {
        return None;
    }
    if dic_id > 0 {
        let by_id = res_dir.join(format!("dict-{dic_id}")).join("styles.css");
        if by_id.is_file() {
            return Some(by_id);
        }
    }
    if is_safe_single_dir_component(dict_name) {
        let by_name = res_dir.join(dict_name).join("styles.css");
        if by_name.is_file() {
            return Some(by_name);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_split_mora_including_small_kana_and_combining_marks() {
        assert_eq!(split_mora_str(""), Vec::<String>::new());
        assert_eq!(split_mora_str("きょう"), vec!["きょ", "う"]);
        assert_eq!(split_mora_str("しゃしん"), vec!["しゃ", "し", "ん"]);
        assert_eq!(
            split_mora_str("きゃぷしょん"),
            vec!["きゃ", "ぷ", "しょ", "ん"]
        );
        assert_eq!(split_mora_str("あ"), vec!["あ"]);
        assert_eq!(split_mora_str("ティ"), vec!["ティ"]);

        assert_eq!(split_mora_str("くゎし"), vec!["くゎ", "し"]);
        assert_eq!(
            split_mora_str("シークヮーサー"),
            vec!["シ", "ー", "クヮ", "ー", "サ", "ー"]
        );
        assert_eq!(split_mora_str("いっヵげつ"), vec!["い", "っヵ", "げ", "つ"]);
        assert_eq!(
            split_mora_str("か\u{309A}き\u{3099}"),
            vec!["か\u{309A}", "き\u{3099}"]
        );
    }

    #[test]
    fn test_json_array_to_utf8_strings() {
        assert_eq!(json_array_to_utf8_strings(b""), Vec::<String>::new());
        assert_eq!(
            json_array_to_utf8_strings(b"not json"),
            Vec::<String>::new()
        );
        assert_eq!(
            json_array_to_utf8_strings(b"{\"key\": \"val\"}"),
            Vec::<String>::new()
        );

        let json = r#"["hello", 123, "world", true, null, "日本語"]"#.as_bytes();
        assert_eq!(
            json_array_to_utf8_strings(json),
            vec!["hello", "world", "日本語"]
        );
    }

    #[test]
    fn test_parse_pitch_blob_with_nasal_devoice_tags_and_homograph_guard() {
        let json = r#"{"reading": "たべる", "pitches": [{"position": 2, "nasal": 2, "devoice": [1, 3], "tags": ["common", "v1"]}, {"position": 0, "nasal": [1]}]}"#.as_bytes();
        let data = parse_pitch_blob_utf8(json, "食べる", "たべる");
        assert!(data.matched);
        assert_eq!(data.mora, vec!["た", "べ", "る"]);
        assert_eq!(data.entries.len(), 2);
        assert_eq!(data.entries[0].position, 2);
        assert_eq!(data.entries[0].nasal, vec![2]);
        assert_eq!(data.entries[0].devoice, vec![1, 3]);
        assert_eq!(
            data.entries[0].tags,
            vec!["common".to_owned(), "v1".to_owned()]
        );
        assert_eq!(data.entries[1].position, 0);
        assert_eq!(data.entries[1].nasal, vec![1]);

        let homograph_json =
            r#"{"reading": "にんき", "pitches": [{"position": 0}]}"#.as_bytes();
        assert!(!parse_pitch_blob_utf8(homograph_json, "人気", "").matched);
        assert!(!parse_pitch_blob_utf8(homograph_json, "人気", "ひとけ").matched);
        assert!(parse_pitch_blob_utf8(homograph_json, "人気", "にんき").matched);

        let kana_json = r#"{"reading": "そこ", "pitches": [{"position": 0}]}"#.as_bytes();
        assert!(parse_pitch_blob_utf8(kana_json, "そこ", "").matched);

        assert!(!parse_pitch_blob_utf8(b"invalid", "食べる", "たべる").matched);
    }

    #[test]
    fn pitch_positions_reject_fractional_and_out_of_range_json_numbers() {
        let blob = r#"{"reading":"あ","pitches":[{"position":1.5},{"position":-2.5},{"position":2147483648},{"position":-2147483649},{"position":2.0},{"position":-2147483648},{"position":"3"},{"position":true}]}"#;
        let data = parse_pitch_blob_utf8(blob.as_bytes(), "亜", "あ");
        assert!(data.matched);
        let positions: Vec<i32> = data.entries.iter().map(|e| e.position).collect();
        assert_eq!(positions, [0, 0, 0, 0, 2, i32::MIN, 0, 0]);
        assert!(data.entries.iter().all(|e| e.pattern.is_none()));
    }

    #[test]
    fn pitch_positions_parse_string_exact_patterns_pr_2055() {
        let blob = r#"{"reading":"たべる","pitches":[{"position":"LHH"},{"position":"HLL"},{"position":"lhhl","tags":["kansai"]},{"position":2},{"position":"invalid"}]}"#;
        let parsed = parse_pitch_blob_utf8(blob.as_bytes(), "食べる", "たべる");
        assert!(parsed.matched);
        assert_eq!(parsed.mora, vec!["た", "べ", "る"]);
        assert_eq!(parsed.entries.len(), 5);

        assert_eq!(parsed.entries[0].pattern.as_deref(), Some("LHH"));
        assert_eq!(
            parsed.entries[0].pitch_position(),
            Some(PitchPosition::Pattern("LHH".to_owned()))
        );
        assert_eq!(
            parsed.entries[0].pitch_position().unwrap().to_levels(3),
            vec![false, true, true]
        );

        assert_eq!(parsed.entries[1].pattern.as_deref(), Some("HLL"));
        assert_eq!(
            parsed.entries[1].pitch_position(),
            Some(PitchPosition::Pattern("HLL".to_owned()))
        );

        assert_eq!(parsed.entries[2].pattern.as_deref(), Some("LHHL"));
        assert_eq!(parsed.entries[2].tags, vec!["kansai".to_owned()]);

        assert_eq!(parsed.entries[3].pattern, None);
        assert_eq!(parsed.entries[3].position, 2);
        assert_eq!(
            parsed.entries[3].pitch_position(),
            Some(PitchPosition::Downstep(2))
        );
        assert_eq!(
            parsed.entries[3].pitch_position().unwrap().to_levels(3),
            vec![false, true, false, false]
        );

        assert_eq!(parsed.entries[4].pattern, None);
        assert_eq!(parsed.entries[4].position, 0);
    }

    #[test]
    fn test_resolve_style_paths() {
        let temp = tempfile::tempdir().unwrap();
        let legacy_dir = temp.path().join("JMdict");
        std::fs::create_dir_all(&legacy_dir).unwrap();
        let legacy_css = legacy_dir.join("styles.css");
        std::fs::write(&legacy_css, "/* legacy */").unwrap();

        assert_eq!(
            resolve_dictionary_style_path(temp.path(), 3, "JMdict"),
            Some(legacy_css)
        );

        let id_dir = temp.path().join("dict-3");
        std::fs::create_dir_all(&id_dir).unwrap();
        let id_css = id_dir.join("styles.css");
        std::fs::write(&id_css, "/* by id */").unwrap();
        assert_eq!(
            resolve_dictionary_style_path(temp.path(), 3, "JMdict"),
            Some(id_css)
        );

        assert_eq!(
            resolve_dictionary_style_path(temp.path(), 0, "../JMdict"),
            None
        );
    }
}

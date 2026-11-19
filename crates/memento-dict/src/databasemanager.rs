//! DatabaseManager pure query, pitch/mora, frequency, stylesheet, and tag helpers.

use std::path::{Component, Path, PathBuf};

pub const KATAKANA_LOW: u16 = 0x30A1;
pub const KATAKANA_HIGH: u16 = 0x30F6;
pub const HIRAGANA_LOW: u16 = 0x3041;
pub const HIRAGANA_HIGH: u16 = 0x3096;

/// Small kana and combining dakuten/handakuten code units that attach to the
/// preceding base kana rather than forming an independent mora:
/// ぁ ぃ ぅ ぇ ぉ ゃ ゅ ょ ゎ ゕ ゖ ァ ィ ゥ ェ ォ ャ ュ ョ ヮ ヵ ヶ U+3099 U+309A
pub const MORA_SKIP_CHARS: [u16; 24] = [
    0x3041, // ぁ
    0x3043, // ぃ
    0x3045, // ぅ
    0x3047, // ぇ
    0x3049, // ぉ
    0x3083, // ゃ
    0x3085, // ゅ
    0x3087, // ょ
    0x308E, // ゎ
    0x3095, // ゕ
    0x3096, // ゖ
    0x3099, // combining voiced sound mark (dakuten)
    0x309A, // combining semi-voiced sound mark (handakuten / nasal velar)
    0x30A1, // ァ
    0x30A3, // ィ
    0x30A5, // ゥ
    0x30A7, // ェ
    0x30A9, // ォ
    0x30E3, // ャ
    0x30E5, // ュ
    0x30E7, // ョ
    0x30EE, // ヮ
    0x30F5, // ヵ
    0x30F6, // ヶ
];

#[inline]
#[must_use]
pub fn is_mora_skip_unit(unit: u16) -> bool {
    MORA_SKIP_CHARS.contains(&unit)
}

#[inline]
#[must_use]
pub fn is_mora_skip_char(ch: char) -> bool {
    u16::try_from(ch as u32).is_ok_and(is_mora_skip_unit)
}

/// Converts all katakana in `query` in the range `0x30A1..=0x30F6` to hiragana
/// by shifting code units by `-0x60` (`0x3041 + (code - 0x30A1)`), matching
/// `DatabaseManager::kataToHira`.
#[must_use]
pub fn kata_to_hira(query: &[u16]) -> Vec<u16> {
    query
        .iter()
        .map(|&ch| {
            if (KATAKANA_LOW..=KATAKANA_HIGH).contains(&ch) {
                HIRAGANA_LOW + (ch - KATAKANA_LOW)
            } else {
                ch
            }
        })
        .collect()
}

/// Splits a UTF-16 reading into mora segments according to Memento/Yomitan pitch
/// accent segmentation rules.
#[must_use]
pub fn split_mora(reading: &[u16]) -> Vec<Vec<u16>> {
    let mut mora: Vec<Vec<u16>> = Vec::new();
    let mut current_mora: Vec<u16> = Vec::new();

    for &ch in reading {
        let is_skip = is_mora_skip_unit(ch);
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

/// Splits a UTF-8 reading into mora strings without UTF-16 intermediate allocations.
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

/// One pitch pattern inside a Yomitan `pitches` JSON array.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PitchEntryDetail {
    pub position: i32,
    pub nasal: Vec<usize>,
    pub devoice: Vec<usize>,
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PitchBlobData {
    pub matched: bool,
    pub mora: Vec<Vec<u16>>,
    pub positions: Vec<i32>,
    pub nasal: Vec<usize>,
    pub devoice: Vec<usize>,
    pub tags: Vec<String>,
    pub entries: Vec<PitchEntryDetail>,
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
            let position = o
                .get("position")
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
                    || crate::normalize::normalize_reading_str(r)
                        == crate::normalize::normalize_reading_str(expression);
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

/// Parses pitch accent JSON blob and segments into mora, pitch positions, nasal, devoice, and tags.
#[must_use]
pub fn parse_pitch_blob(
    json_bytes: &[u8],
    term_exp: &[u16],
    term_reading: &[u16],
) -> PitchBlobData {
    let Ok(serde_json::Value::Object(obj)) = serde_json::from_slice::<serde_json::Value>(json_bytes)
    else {
        let empty_matched = term_reading.is_empty() && term_exp.is_empty();
        return PitchBlobData {
            matched: empty_matched,
            ..PitchBlobData::default()
        };
    };

    let meta_reading = obj.get("reading").and_then(|v| v.as_str());
    let reading_str = meta_reading.unwrap_or("");
    let reading_u16: Vec<u16> = reading_str.encode_utf16().collect();

    let matched = if !term_reading.is_empty() {
        reading_u16 == term_reading || reading_u16 == term_exp
    } else {
        reading_u16.is_empty()
            || reading_u16 == term_exp
            || kata_to_hira(&reading_u16) == kata_to_hira(term_exp)
    };
    if !matched {
        return PitchBlobData::default();
    }

    let mora_source: &[u16] = if !reading_u16.is_empty() {
        &reading_u16
    } else if !term_reading.is_empty() {
        term_reading
    } else {
        term_exp
    };
    let mora = split_mora(mora_source);
    let entries = parse_pitch_entries(&obj);
    let positions: Vec<i32> = entries.iter().map(|e| e.position).collect();
    let mut nasal = Vec::new();
    let mut devoice = Vec::new();
    let mut tags = Vec::new();
    for entry in &entries {
        for &n in &entry.nasal {
            if !nasal.contains(&n) {
                nasal.push(n);
            }
        }
        for &d in &entry.devoice {
            if !devoice.contains(&d) {
                devoice.push(d);
            }
        }
        for tag in &entry.tags {
            if !tags.contains(tag) {
                tags.push(tag.clone());
            }
        }
    }

    PitchBlobData {
        matched: true,
        mora,
        positions,
        nasal,
        devoice,
        tags,
        entries,
    }
}

/// Builds the legacy dictionary asset stylesheet path `{resource_path}/{dict_name}/styles.css`.
/// Kept backward-compatible for callers expecting the two-argument UTF-16 builder;
/// prefer [`resolve_dictionary_style_path`] or [`build_dict_id_style_path`] when `dic_id` is known.
#[must_use]
pub fn build_style_path(resource_path: &[u16], dict_name: &[u16]) -> Vec<u16> {
    const SLASH: u16 = b'/' as u16;
    const STYLE_FILENAME: [u16; 10] = [
        b's' as u16,
        b't' as u16,
        b'y' as u16,
        b'l' as u16,
        b'e' as u16,
        b's' as u16,
        b'.' as u16,
        b'c' as u16,
        b's' as u16,
        b's' as u16,
    ];

    let mut result =
        Vec::with_capacity(resource_path.len() + 1 + dict_name.len() + 1 + STYLE_FILENAME.len());
    result.extend_from_slice(resource_path);
    result.push(SLASH);
    result.extend_from_slice(dict_name);
    result.push(SLASH);
    result.extend_from_slice(&STYLE_FILENAME);
    result
}

/// Builds `{resource_path}/dict-{dic_id}/styles.css` in UTF-16, matching
/// [`crate::yomidbbuilder::dictionary_resource_dir`].
#[must_use]
pub fn build_dict_id_style_path(resource_path: &[u16], dic_id: i64) -> Vec<u16> {
    let dir_name: Vec<u16> = format!("dict-{dic_id}").encode_utf16().collect();
    build_style_path(resource_path, &dir_name)
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

    fn to_u16(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    fn from_u16(u: &[u16]) -> String {
        String::from_utf16_lossy(u)
    }

    #[test]
    fn test_kata_to_hira() {
        assert_eq!(kata_to_hira(&[]), Vec::<u16>::new());
        assert_eq!(from_u16(&kata_to_hira(&to_u16("カタカナ"))), "かたかな");
        assert_eq!(from_u16(&kata_to_hira(&to_u16("ひらがな"))), "ひらがな");
        assert_eq!(
            from_u16(&kata_to_hira(&to_u16("漢字 mixed テスト 123"))),
            "漢字 mixed てすと 123"
        );
        assert_eq!(kata_to_hira(&[0x30A1]), vec![0x3041]);
        assert_eq!(kata_to_hira(&[0x30F6]), vec![0x3096]);
        assert_eq!(kata_to_hira(&[0x30A0]), vec![0x30A0]);
        assert_eq!(kata_to_hira(&[0x30F7]), vec![0x30F7]);
        assert_eq!(kata_to_hira(&[0x30FC]), vec![0x30FC]);
    }

    #[test]
    fn test_split_mora_including_small_kana_and_combining_marks() {
        assert_eq!(split_mora(&[]), Vec::<Vec<u16>>::new());
        assert_eq!(split_mora_str("きょう"), vec!["きょ", "う"]);
        assert_eq!(split_mora_str("しゃしん"), vec!["しゃ", "し", "ん"]);
        assert_eq!(split_mora_str("きゃぷしょん"), vec!["きゃ", "ぷ", "しょ", "ん"]);
        assert_eq!(split_mora_str("あ"), vec!["あ"]);
        assert_eq!(split_mora_str("ティ"), vec!["ティ"]);

        // Extended small kana (ゎ, ヮ, ゕ, ゖ, ヵ, ヶ) and combining dakuten/handakuten:
        assert_eq!(split_mora_str("くゎし"), vec!["くゎ", "し"]);
        assert_eq!(split_mora_str("シークヮーサー"), vec!["シ", "ー", "クヮ", "ー", "サ", "ー"]);
        assert_eq!(split_mora_str("いっヵげつ"), vec!["い", "っヵ", "げ", "つ"]);
        assert_eq!(split_mora_str("か\u{309A}き\u{3099}"), vec!["か\u{309A}", "き\u{3099}"]);
    }

    #[test]
    fn test_json_array_to_utf8_strings() {
        assert_eq!(json_array_to_utf8_strings(b""), Vec::<String>::new());
        assert_eq!(json_array_to_utf8_strings(b"not json"), Vec::<String>::new());
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
        let exp = to_u16("食べる");
        let reading = to_u16("たべる");

        let data = parse_pitch_blob(json, &exp, &reading);
        assert!(data.matched);
        let mora_strs: Vec<String> = data.mora.iter().map(|m| from_u16(m)).collect();
        assert_eq!(mora_strs, vec!["た", "べ", "る"]);
        assert_eq!(data.positions, vec![2, 0]);
        assert_eq!(data.nasal, vec![2, 1]);
        assert_eq!(data.devoice, vec![1, 3]);
        assert_eq!(data.tags, vec!["common".to_owned(), "v1".to_owned()]);
        assert_eq!(data.entries.len(), 2);
        assert_eq!(data.entries[0].nasal, vec![2]);
        assert_eq!(data.entries[0].devoice, vec![1, 3]);

        // Matched expression (when reading matches expression)
        let json_exp_reading = r#"{"reading": "食べる", "pitches": [{"position": 1}]}"#.as_bytes();
        let data_exp = parse_pitch_blob(json_exp_reading, &exp, &reading);
        assert!(data_exp.matched);
        assert_eq!(data_exp.positions, vec![1]);

        // Empty-reading kanji homograph must NOT match a specific kana reading:
        let homograph_json =
            r#"{"reading": "にんき", "pitches": [{"position": 0}]}"#.as_bytes();
        assert!(!parse_pitch_blob(homograph_json, &to_u16("人気"), &[]).matched);
        assert!(!parse_pitch_blob_utf8(homograph_json, "人気", "").matched);
        assert!(!parse_pitch_blob_utf8(homograph_json, "人気", "ひとけ").matched);
        assert!(parse_pitch_blob_utf8(homograph_json, "人気", "にんき").matched);

        // Kana word with empty caller reading matches its own reading:
        let kana_json = r#"{"reading": "そこ", "pitches": [{"position": 0}]}"#.as_bytes();
        assert!(parse_pitch_blob(kana_json, &to_u16("そこ"), &[]).matched);
        assert!(parse_pitch_blob_utf8(kana_json, "そこ", "").matched);

        // Invalid JSON
        let data_invalid = parse_pitch_blob(b"invalid", &exp, &reading);
        assert!(!data_invalid.matched);
    }

    #[test]
    fn pitch_positions_reject_fractional_and_out_of_range_json_numbers() {
        let blob = r#"{"reading":"あ","pitches":[{"position":1.5},{"position":-2.5},{"position":2147483648},{"position":-2147483649},{"position":2.0},{"position":-2147483648},{"position":"3"},{"position":true}]}"#;
        let data = parse_pitch_blob(blob.as_bytes(), &to_u16("亜"), &to_u16("あ"));
        assert!(data.matched);
        assert_eq!(data.positions, [0, 0, 0, 0, 2, i32::MIN, 0, 0]);
    }

    #[test]
    fn test_build_and_resolve_style_paths() {
        let res_path = to_u16("/home/user/.local/share/memento");
        let dict_name = to_u16("JMdict");
        let style_path = build_style_path(&res_path, &dict_name);
        assert_eq!(
            from_u16(&style_path),
            "/home/user/.local/share/memento/JMdict/styles.css"
        );
        assert_eq!(
            from_u16(&build_dict_id_style_path(&res_path, 7)),
            "/home/user/.local/share/memento/dict-7/styles.css"
        );

        let temp = tempfile::tempdir().unwrap();
        let legacy_dir = temp.path().join("JMdict");
        std::fs::create_dir_all(&legacy_dir).unwrap();
        let legacy_css = legacy_dir.join("styles.css");
        std::fs::write(&legacy_css, "/* legacy */").unwrap();

        // Fallback to `{dict_name}/styles.css` when `dict-{id}` is absent:
        assert_eq!(
            resolve_dictionary_style_path(temp.path(), 3, "JMdict"),
            Some(legacy_css)
        );

        // Prefers `dict-{id}/styles.css` when present:
        let id_dir = temp.path().join("dict-3");
        std::fs::create_dir_all(&id_dir).unwrap();
        let id_css = id_dir.join("styles.css");
        std::fs::write(&id_css, "/* by id */").unwrap();
        assert_eq!(
            resolve_dictionary_style_path(temp.path(), 3, "JMdict"),
            Some(id_css)
        );

        // Rejects path traversal in fallback name:
        assert_eq!(
            resolve_dictionary_style_path(temp.path(), 0, "../JMdict"),
            None
        );
    }
}

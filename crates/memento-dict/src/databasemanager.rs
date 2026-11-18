//! DatabaseManager pure query, pitch/mora, frequency, stylesheet, and tag helpers.

use std::path::{Component, Path, PathBuf};

pub const KATAKANA_LOW: u16 = 0x30A1;
pub const KATAKANA_HIGH: u16 = 0x30F6;
pub const HIRAGANA_LOW: u16 = 0x3041;
pub const HIRAGANA_HIGH: u16 = 0x3096;

pub const SQLITE_ROW: i32 = 100;
pub const SQLITE_DONE: i32 = 101;

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

/// Returns true if SQLite step result is an error (`step != 100 && step != 101`).
#[must_use]
pub fn is_step_error(step: i32) -> bool {
    step != SQLITE_ROW && step != SQLITE_DONE
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FreqKind {
    None = 0,
    Double = 1,
    String = 2,
    Object = 3,
}

impl From<i32> for FreqKind {
    fn from(val: i32) -> Self {
        match val {
            1 => FreqKind::Double,
            2 => FreqKind::String,
            3 => FreqKind::Object,
            _ => FreqKind::None,
        }
    }
}

/// Extracts frequency text from a Yomichan YOMI_BLOB_TYPE_OBJECT frequency entry.
#[must_use]
#[allow(clippy::too_many_arguments)]
pub fn extract_frequency_object(
    has_reading_key: bool,
    obj_reading: &[u16],
    target_reading: &[u16],
    freq_kind: i32,
    freq_str: &[u16],
    has_nested_display: bool,
    nested_display: &[u16],
    has_nested_value_num: bool,
    nested_value_num_str: &[u16],
    has_top_display: bool,
    top_display: &[u16],
    has_top_value_num: bool,
    top_value_num_str: &[u16],
) -> Option<Vec<u16>> {
    if has_reading_key && obj_reading != target_reading {
        return None;
    }

    let kind = FreqKind::from(freq_kind);
    let mut result = Vec::new();

    match kind {
        FreqKind::Double | FreqKind::String => {
            result.extend_from_slice(freq_str);
        }
        FreqKind::Object => {
            if has_nested_display {
                result.extend_from_slice(nested_display);
            } else if has_nested_value_num {
                result.extend_from_slice(nested_value_num_str);
            } else {
                return None;
            }
        }
        FreqKind::None => {
            if has_top_display || has_top_value_num {
                if has_top_display {
                    result.extend_from_slice(top_display);
                } else if has_top_value_num {
                    result.extend_from_slice(top_value_num_str);
                } else {
                    return None;
                }
            }
        }
    }

    Some(result)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum TermQueryKind {
    Plain = 0,
    WithKatakana = 1,
    WithHalfwidth = 2,
}

impl From<i32> for TermQueryKind {
    fn from(val: i32) -> Self {
        match val {
            1 => TermQueryKind::WithKatakana,
            2 => TermQueryKind::WithHalfwidth,
            _ => TermQueryKind::Plain,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TermQueryPlan {
    pub hiragana: Vec<u16>,
    pub contains_half: bool,
    pub contains_kata: bool,
    pub query_kind: TermQueryKind,
}

/// Plans term query bindings and SQL variant.
/// Matches `DatabaseManager::queryTerms` lines 371-393.
#[must_use]
pub fn plan_term_query(raw_query: &[u16], nfkc_query: &[u16]) -> TermQueryPlan {
    let hiragana = kata_to_hira(nfkc_query);
    let contains_half = nfkc_query != raw_query;
    let contains_kata = hiragana != nfkc_query;
    let query_kind = if contains_half {
        TermQueryKind::WithHalfwidth
    } else if contains_kata {
        TermQueryKind::WithKatakana
    } else {
        TermQueryKind::Plain
    };
    TermQueryPlan {
        hiragana,
        contains_half,
        contains_kata,
        query_kind,
    }
}

/// Converts a raw JSON array of strings to a list of UTF-16 string vectors.
/// Matches `DatabaseManager::jsonArrayToStringList`.
#[must_use]
pub fn json_array_to_strings(json_bytes: &[u8]) -> Vec<Vec<u16>> {
    let Ok(serde_json::Value::Array(arr)) = serde_json::from_slice(json_bytes) else {
        return Vec::new();
    };
    arr.into_iter()
        .filter_map(|val| match val {
            serde_json::Value::String(s) => Some(s.encode_utf16().collect()),
            _ => None,
        })
        .collect()
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum KanjiStatCategory {
    Skip = 0,
    Index = 1,
    Misc = 2,
    Class = 3,
    Code = 4,
}

impl From<i32> for KanjiStatCategory {
    fn from(val: i32) -> Self {
        match val {
            1 => KanjiStatCategory::Index,
            2 => KanjiStatCategory::Misc,
            3 => KanjiStatCategory::Class,
            4 => KanjiStatCategory::Code,
            _ => KanjiStatCategory::Skip,
        }
    }
}

/// Classifies a kanji stat tag category string (in UTF-16).
#[must_use]
pub fn classify_kanji_stat_category(category: &[u16]) -> KanjiStatCategory {
    const INDEX: [u16; 5] = [b'i' as u16, b'n' as u16, b'd' as u16, b'e' as u16, b'x' as u16];
    const MISC: [u16; 4] = [b'm' as u16, b'i' as u16, b's' as u16, b'c' as u16];
    const CLASS: [u16; 5] = [b'c' as u16, b'l' as u16, b'a' as u16, b's' as u16, b's' as u16];
    const CODE: [u16; 4] = [b'c' as u16, b'o' as u16, b'd' as u16, b'e' as u16];

    if category == INDEX {
        KanjiStatCategory::Index
    } else if category == MISC {
        KanjiStatCategory::Misc
    } else if category == CLASS {
        KanjiStatCategory::Class
    } else if category == CODE {
        KanjiStatCategory::Code
    } else {
        KanjiStatCategory::Skip
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
    fn test_is_step_error() {
        assert!(!is_step_error(100));
        assert!(!is_step_error(101));
        assert!(is_step_error(0));
        assert!(is_step_error(1));
        assert!(is_step_error(-1));
    }

    #[test]
    fn test_extract_frequency_object_reading_filter() {
        let reading_foo = to_u16("foo");
        let reading_bar = to_u16("bar");
        let val = to_u16("123");

        let res = extract_frequency_object(
            true,
            &reading_foo,
            &reading_foo,
            1,
            &val,
            false,
            &[],
            false,
            &[],
            false,
            &[],
            false,
            &[],
        );
        assert_eq!(res, Some(val.clone()));

        let res_skip = extract_frequency_object(
            true,
            &reading_foo,
            &reading_bar,
            1,
            &val,
            false,
            &[],
            false,
            &[],
            false,
            &[],
            false,
            &[],
        );
        assert_eq!(res_skip, None);

        let res_no_key = extract_frequency_object(
            false,
            &reading_foo,
            &reading_bar,
            1,
            &val,
            false,
            &[],
            false,
            &[],
            false,
            &[],
            false,
            &[],
        );
        assert_eq!(res_no_key, Some(val));
    }

    #[test]
    fn test_extract_frequency_object_scenarios() {
        let dummy = to_u16("reading");
        let freq_str = to_u16("42");
        let nested_disp = to_u16("top 100");
        let nested_num = to_u16("500");
        let top_disp = to_u16("common");
        let top_num = to_u16("1000");

        let s1 = extract_frequency_object(
            false, &dummy, &dummy, 1, &freq_str, false, &[], false, &[], false, &[], false, &[],
        );
        assert_eq!(s1, Some(freq_str.clone()));

        let s2 = extract_frequency_object(
            false, &dummy, &dummy, 2, &freq_str, false, &[], false, &[], false, &[], false, &[],
        );
        assert_eq!(s2, Some(freq_str));

        let s3_disp = extract_frequency_object(
            false,
            &dummy,
            &dummy,
            3,
            &[],
            true,
            &nested_disp,
            true,
            &nested_num,
            false,
            &[],
            false,
            &[],
        );
        assert_eq!(s3_disp, Some(nested_disp));

        let s3_val = extract_frequency_object(
            false,
            &dummy,
            &dummy,
            3,
            &[],
            false,
            &[],
            true,
            &nested_num,
            false,
            &[],
            false,
            &[],
        );
        assert_eq!(s3_val, Some(nested_num));

        let s3_skip = extract_frequency_object(
            false, &dummy, &dummy, 3, &[], false, &[], false, &[], false, &[], false, &[],
        );
        assert_eq!(s3_skip, None);

        let s4_disp = extract_frequency_object(
            false, &dummy, &dummy, 0, &[], false, &[], false, &[], true, &top_disp, true, &top_num,
        );
        assert_eq!(s4_disp, Some(top_disp));

        let s4_num = extract_frequency_object(
            false, &dummy, &dummy, 0, &[], false, &[], false, &[], false, &[], true, &top_num,
        );
        assert_eq!(s4_num, Some(top_num));

        let fallback = extract_frequency_object(
            false, &dummy, &dummy, 0, &[], false, &[], false, &[], false, &[], false, &[],
        );
        assert_eq!(fallback, Some(Vec::new()));
    }

    #[test]
    fn test_plan_term_query() {
        let raw = to_u16("たべる");
        let nfkc = to_u16("たべる");
        let plan = plan_term_query(&raw, &nfkc);
        assert_eq!(plan.query_kind, TermQueryKind::Plain);
        assert!(!plan.contains_half);
        assert!(!plan.contains_kata);
        assert_eq!(from_u16(&plan.hiragana), "たべる");

        let raw_kata = to_u16("タベル");
        let nfkc_kata = to_u16("タベル");
        let plan_kata = plan_term_query(&raw_kata, &nfkc_kata);
        assert_eq!(plan_kata.query_kind, TermQueryKind::WithKatakana);
        assert!(!plan_kata.contains_half);
        assert!(plan_kata.contains_kata);
        assert_eq!(from_u16(&plan_kata.hiragana), "たべる");

        let raw_half = to_u16("ﾀﾍﾞﾙ");
        let nfkc_half = to_u16("タベル");
        let plan_half = plan_term_query(&raw_half, &nfkc_half);
        assert_eq!(plan_half.query_kind, TermQueryKind::WithHalfwidth);
        assert!(plan_half.contains_half);
        assert!(plan_half.contains_kata);
        assert_eq!(from_u16(&plan_half.hiragana), "たべる");
    }

    #[test]
    fn test_json_array_to_strings() {
        assert_eq!(json_array_to_strings(b""), Vec::<Vec<u16>>::new());
        assert_eq!(json_array_to_strings(b"not json"), Vec::<Vec<u16>>::new());
        assert_eq!(
            json_array_to_strings(b"{\"key\": \"val\"}"),
            Vec::<Vec<u16>>::new()
        );

        let json = r#"["hello", 123, "world", true, null, "日本語"]"#.as_bytes();
        let res = json_array_to_strings(json);
        let strings: Vec<String> = res.iter().map(|s| from_u16(s)).collect();
        assert_eq!(strings, vec!["hello", "world", "日本語"]);
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
    fn test_classify_kanji_stat_category() {
        assert_eq!(
            classify_kanji_stat_category(&to_u16("index")),
            KanjiStatCategory::Index
        );
        assert_eq!(
            classify_kanji_stat_category(&to_u16("misc")),
            KanjiStatCategory::Misc
        );
        assert_eq!(
            classify_kanji_stat_category(&to_u16("class")),
            KanjiStatCategory::Class
        );
        assert_eq!(
            classify_kanji_stat_category(&to_u16("code")),
            KanjiStatCategory::Code
        );
        assert_eq!(
            classify_kanji_stat_category(&to_u16("strokes")),
            KanjiStatCategory::Skip
        );
        assert_eq!(
            classify_kanji_stat_category(&to_u16("")),
            KanjiStatCategory::Skip
        );
        assert_eq!(
            classify_kanji_stat_category(&to_u16("INDEX")),
            KanjiStatCategory::Skip
        );
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

//! DatabaseManager pure query, pitch/mora, frequency and tag helpers.

pub const KATAKANA_LOW: u16 = 0x30A1;
pub const KATAKANA_HIGH: u16 = 0x30F6;
pub const HIRAGANA_LOW: u16 = 0x3041;
pub const HIRAGANA_HIGH: u16 = 0x3096;

pub const SQLITE_ROW: i32 = 100;
pub const SQLITE_DONE: i32 = 101;

/// The 16 small kana characters that cannot form independent mora on their own:
/// ぁ ぃ ぅ ぇ ぉ ゃ ゅ ょ ァ ィ ゥ ェ ォ ャ ュ ョ
pub const MORA_SKIP_CHARS: [u16; 16] = [
    0x3041, // ぁ
    0x3043, // ぃ
    0x3045, // ぅ
    0x3047, // ぇ
    0x3049, // ぉ
    0x3083, // ゃ
    0x3085, // ゅ
    0x3087, // ょ
    0x30A1, // ァ
    0x30A3, // ィ
    0x30A5, // ゥ
    0x30A7, // ェ
    0x30A9, // ォ
    0x30E3, // ャ
    0x30E5, // ュ
    0x30E7, // ョ
];

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

/// Splits a reading into mora segments according to Memento's pitch accent
/// segmentation logic in `DatabaseManager::addPitches`.
#[must_use]
pub fn split_mora(reading: &[u16]) -> Vec<Vec<u16>> {
    let mut mora: Vec<Vec<u16>> = Vec::new();
    let mut current_mora: Vec<u16> = Vec::new();

    for &ch in reading {
        let is_skip = MORA_SKIP_CHARS.contains(&ch);
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
///
/// Returns:
/// - `None` if the entry should be skipped (`continue` in C++ loop):
///   - when `has_reading_key` is true and `obj_reading != target_reading`
///   - when scenario 3 (nested object) has neither string displayValue nor double value
/// - `Some(vec)` containing the resulting frequency string:
///   - scenario 1: double value (passed in `freq_str`)
///   - scenario 2: string value (passed in `freq_str`)
///   - scenario 3: nested displayValue if string, else nested value if double
///   - scenario 4: top-level displayValue if string, else top-level value if double
///   - fallback: if none of scenarios 1-4 apply, C++ leaves `freqStr` empty and breaks
///     (does NOT continue!). This returns `Some(vec![])`.
#[must_use]
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
    // Check if this frequency is dependent on reading
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
                return None; // continue!
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
            // else: neither was valid, C++ hits break with empty freqStr.
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PitchBlobData {
    pub matched: bool,
    pub mora: Vec<Vec<u16>>,
    pub positions: Vec<i32>,
}

/// Parses pitch accent JSON blob and segments into mora and pitch positions.
/// Matches `DatabaseManager::addPitches`.
#[must_use]
pub fn parse_pitch_blob(
    json_bytes: &[u8],
    term_exp: &[u16],
    term_reading: &[u16],
) -> PitchBlobData {
    let Ok(serde_json::Value::Object(obj)) = serde_json::from_slice::<serde_json::Value>(json_bytes) else {
        // Fallback matching QJsonDocument::fromJson(...).object():
        // If term_reading or term_exp is empty, empty reading matches.
        let empty_matched = term_reading.is_empty() || term_exp.is_empty();
        return PitchBlobData {
            matched: empty_matched,
            mora: Vec::new(),
            positions: Vec::new(),
        };
    };

    let reading_str = obj
        .get("reading")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let reading_u16: Vec<u16> = reading_str.encode_utf16().collect();

    if reading_u16 != term_reading && reading_u16 != term_exp {
        return PitchBlobData {
            matched: false,
            mora: Vec::new(),
            positions: Vec::new(),
        };
    }

    let mora = split_mora(&reading_u16);
    let positions = obj
        .get("pitches")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .map(|val| {
                    val.as_object()
                        .and_then(|o| o.get("position"))
                        // QJsonValue::toInt() returns 0 for fractional and
                        // out-of-range values rather than truncating/saturating.
                        .and_then(|p| p.as_f64())
                        .filter(|&position| position.fract() == 0.0
                            && position >= f64::from(i32::MIN)
                            && position <= f64::from(i32::MAX))
                        .map(|position| position as i32)
                        .unwrap_or(0)
                })
                .collect()
        })
        .unwrap_or_default();

    PitchBlobData {
        matched: true,
        mora,
        positions,
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
/// Matches `DatabaseManager::queryKanji`:
/// "index" -> Index (1)
/// "misc"  -> Misc (2)
/// "class" -> Class (3)
/// "code"  -> Code (4)
/// otherwise -> Skip (0)
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

/// Builds the dictionary asset stylesheet path `{resource_path}/{dict_name}/styles.css`.
/// Matches `DatabaseManager::loadDictionaryAssets`.
#[must_use]
pub fn build_style_path(resource_path: &[u16], dict_name: &[u16]) -> Vec<u16> {
    const SLASH: u16 = b'/' as u16;
    const STYLE_FILENAME: [u16; 10] = [
        b's' as u16, b't' as u16, b'y' as u16, b'l' as u16, b'e' as u16,
        b's' as u16, b'.' as u16, b'c' as u16, b's' as u16, b's' as u16,
    ];

    let mut result = Vec::with_capacity(resource_path.len() + 1 + dict_name.len() + 1 + STYLE_FILENAME.len());
    result.extend_from_slice(resource_path);
    result.push(SLASH);
    result.extend_from_slice(dict_name);
    result.push(SLASH);
    result.extend_from_slice(&STYLE_FILENAME);
    result
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
        assert_eq!(from_u16(&kata_to_hira(&to_u16("漢字 mixed テスト 123"))), "漢字 mixed てすと 123");
        // Boundary characters:
        // 0x30A1 ('ァ') -> 0x3041 ('ぁ')
        assert_eq!(kata_to_hira(&[0x30A1]), vec![0x3041]);
        // 0x30F6 ('ヶ') -> 0x3096 ('ゖ')
        assert_eq!(kata_to_hira(&[0x30F6]), vec![0x3096]);
        // Below 0x30A1: 0x30A0 ('゠') unchanged
        assert_eq!(kata_to_hira(&[0x30A0]), vec![0x30A0]);
        // Above 0x30F6: 0x30F7 ('ヷ') unchanged
        assert_eq!(kata_to_hira(&[0x30F7]), vec![0x30F7]);
        // Prolonged sound mark: 0x30FC ('ー') unchanged
        assert_eq!(kata_to_hira(&[0x30FC]), vec![0x30FC]);
    }

    #[test]
    fn test_split_mora() {
        assert_eq!(split_mora(&[]), Vec::<Vec<u16>>::new());

        let res = split_mora(&to_u16("きょう"));
        let strings: Vec<String> = res.iter().map(|m| from_u16(m)).collect();
        assert_eq!(strings, vec!["きょ", "う"]);

        let res2 = split_mora(&to_u16("しゃしん"));
        let strings2: Vec<String> = res2.iter().map(|m| from_u16(m)).collect();
        assert_eq!(strings2, vec!["しゃ", "し", "ん"]);

        let res3 = split_mora(&to_u16("きゃぷしょん"));
        let strings3: Vec<String> = res3.iter().map(|m| from_u16(m)).collect();
        assert_eq!(strings3, vec!["きゃ", "ぷ", "しょ", "ん"]);

        let res4 = split_mora(&to_u16("あ"));
        let strings4: Vec<String> = res4.iter().map(|m| from_u16(m)).collect();
        assert_eq!(strings4, vec!["あ"]);

        let res5 = split_mora(&to_u16("ティ"));
        let strings5: Vec<String> = res5.iter().map(|m| from_u16(m)).collect();
        assert_eq!(strings5, vec!["ティ"]);
    }

    #[test]
    fn test_is_step_error() {
        assert!(!is_step_error(100)); // SQLITE_ROW
        assert!(!is_step_error(101)); // SQLITE_DONE
        assert!(is_step_error(0));
        assert!(is_step_error(1));
        assert!(is_step_error(-1));
    }

    #[test]
    fn test_extract_frequency_object_reading_filter() {
        let reading_foo = to_u16("foo");
        let reading_bar = to_u16("bar");
        let val = to_u16("123");

        // Has reading key and matches
        let res = extract_frequency_object(
            true, &reading_foo, &reading_foo,
            1, &val,
            false, &[], false, &[], false, &[], false, &[],
        );
        assert_eq!(res, Some(val.clone()));

        // Has reading key and does NOT match -> None (skip)
        let res_skip = extract_frequency_object(
            true, &reading_foo, &reading_bar,
            1, &val,
            false, &[], false, &[], false, &[], false, &[],
        );
        assert_eq!(res_skip, None);

        // Does NOT have reading key -> matches regardless
        let res_no_key = extract_frequency_object(
            false, &reading_foo, &reading_bar,
            1, &val,
            false, &[], false, &[], false, &[], false, &[],
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

        // Scenario 1: Double
        let s1 = extract_frequency_object(
            false, &dummy, &dummy,
            1, &freq_str,
            false, &[], false, &[], false, &[], false, &[],
        );
        assert_eq!(s1, Some(freq_str.clone()));

        // Scenario 2: String
        let s2 = extract_frequency_object(
            false, &dummy, &dummy,
            2, &freq_str,
            false, &[], false, &[], false, &[], false, &[],
        );
        assert_eq!(s2, Some(freq_str));

        // Scenario 3: Object with displayValue
        let s3_disp = extract_frequency_object(
            false, &dummy, &dummy,
            3, &[],
            true, &nested_disp, true, &nested_num, false, &[], false, &[],
        );
        assert_eq!(s3_disp, Some(nested_disp));

        // Scenario 3: Object with value only
        let s3_val = extract_frequency_object(
            false, &dummy, &dummy,
            3, &[],
            false, &[], true, &nested_num, false, &[], false, &[],
        );
        assert_eq!(s3_val, Some(nested_num));

        // Scenario 3: Object with neither -> skip (None)
        let s3_skip = extract_frequency_object(
            false, &dummy, &dummy,
            3, &[],
            false, &[], false, &[], false, &[], false, &[],
        );
        assert_eq!(s3_skip, None);

        // Scenario 4: Top display
        let s4_disp = extract_frequency_object(
            false, &dummy, &dummy,
            0, &[],
            false, &[], false, &[], true, &top_disp, true, &top_num,
        );
        assert_eq!(s4_disp, Some(top_disp));

        // Scenario 4: Top value num
        let s4_num = extract_frequency_object(
            false, &dummy, &dummy,
            0, &[],
            false, &[], false, &[], false, &[], true, &top_num,
        );
        assert_eq!(s4_num, Some(top_num));

        // Fallback: None of the above -> Some(empty) (NOT skip!)
        let fallback = extract_frequency_object(
            false, &dummy, &dummy,
            0, &[],
            false, &[], false, &[], false, &[], false, &[],
        );
        assert_eq!(fallback, Some(Vec::new()));
    }

    #[test]
    fn test_plan_term_query() {
        // 1. Plain hiragana: raw == nfkc == hiragana -> Plain (0)
        let raw = to_u16("たべる");
        let nfkc = to_u16("たべる");
        let plan = plan_term_query(&raw, &nfkc);
        assert_eq!(plan.query_kind, TermQueryKind::Plain);
        assert!(!plan.contains_half);
        assert!(!plan.contains_kata);
        assert_eq!(from_u16(&plan.hiragana), "たべる");

        // 2. Katakana: raw == nfkc, hiragana != nfkc -> WithKatakana (1)
        let raw_kata = to_u16("タベル");
        let nfkc_kata = to_u16("タベル");
        let plan_kata = plan_term_query(&raw_kata, &nfkc_kata);
        assert_eq!(plan_kata.query_kind, TermQueryKind::WithKatakana);
        assert!(!plan_kata.contains_half);
        assert!(plan_kata.contains_kata);
        assert_eq!(from_u16(&plan_kata.hiragana), "たべる");

        // 3. Halfwidth katakana: raw != nfkc -> WithHalfwidth (2)
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
        // Empty / invalid
        assert_eq!(json_array_to_strings(b""), Vec::<Vec<u16>>::new());
        assert_eq!(json_array_to_strings(b"not json"), Vec::<Vec<u16>>::new());
        assert_eq!(json_array_to_strings(b"{\"key\": \"val\"}"), Vec::<Vec<u16>>::new());

        // Valid array with strings and non-strings
        let json = r#"["hello", 123, "world", true, null, "日本語"]"#.as_bytes();
        let res = json_array_to_strings(json);
        let strings: Vec<String> = res.iter().map(|s| from_u16(s)).collect();
        assert_eq!(strings, vec!["hello", "world", "日本語"]);
    }

    #[test]
    fn test_parse_pitch_blob() {
        let json = r#"{"reading": "たべる", "pitches": [{"position": 2}, {"position": 0}]}"#.as_bytes();
        let exp = to_u16("食べる");
        let reading = to_u16("たべる");

        // Matched reading
        let data = parse_pitch_blob(json, &exp, &reading);
        assert!(data.matched);
        let mora_strs: Vec<String> = data.mora.iter().map(|m| from_u16(m)).collect();
        assert_eq!(mora_strs, vec!["た", "べ", "る"]);
        assert_eq!(data.positions, vec![2, 0]);

        // Matched expression (when reading matches expression)
        let json_exp_reading = r#"{"reading": "食べる", "pitches": [{"position": 1}]}"#.as_bytes();
        let data_exp = parse_pitch_blob(json_exp_reading, &exp, &reading);
        assert!(data_exp.matched);
        assert_eq!(data_exp.positions, vec![1]);

        // Mismatched reading
        let other_reading = to_u16("のむ");
        let other_exp = to_u16("飲む");
        let data_mismatch = parse_pitch_blob(json, &other_exp, &other_reading);
        assert!(!data_mismatch.matched);
        assert!(data_mismatch.mora.is_empty());
        assert!(data_mismatch.positions.is_empty());

        // Invalid JSON or non-object
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
        assert_eq!(classify_kanji_stat_category(&to_u16("index")), KanjiStatCategory::Index);
        assert_eq!(classify_kanji_stat_category(&to_u16("misc")), KanjiStatCategory::Misc);
        assert_eq!(classify_kanji_stat_category(&to_u16("class")), KanjiStatCategory::Class);
        assert_eq!(classify_kanji_stat_category(&to_u16("code")), KanjiStatCategory::Code);
        assert_eq!(classify_kanji_stat_category(&to_u16("strokes")), KanjiStatCategory::Skip);
        assert_eq!(classify_kanji_stat_category(&to_u16("")), KanjiStatCategory::Skip);
        assert_eq!(classify_kanji_stat_category(&to_u16("INDEX")), KanjiStatCategory::Skip);
    }

    #[test]
    fn test_build_style_path() {
        let res_path = to_u16("/home/user/.local/share/memento");
        let dict_name = to_u16("JMdict");
        let style_path = build_style_path(&res_path, &dict_name);
        assert_eq!(from_u16(&style_path), "/home/user/.local/share/memento/JMdict/styles.css");
    }
}


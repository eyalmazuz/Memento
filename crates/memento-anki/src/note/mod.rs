//! Pure operations from `src/anki/notebuilder.cpp` for Anki note metadata,
//! marker substitution, HTML formatting, frequency/pitch stats, and media
//! marker parameter parsing.

mod format;
mod frequency;
mod media_params;
mod metadata;
mod pitch;

pub use self::format::{
    find_stroke_count, format_audio_url_and_filename, format_furigana, format_kanji_glossary,
    format_kanji_readings, format_selection, format_tags, format_term_glossary,
    format_term_glossary_selected, format_term_glossary_variant, FuriganaPair, KanjiDefEntry,
    StrokeCountStat, TagEntry, TermGlossaryDef, TermGlossaryOutput, TermGlossaryVariant,
    TermGlossaryVariants,
};
pub use self::frequency::{
    build_frequencies_html, frequency_average, frequency_harmonic, frequency_numbers,
    frequency_stats, parse_frequency_value, FrequencyEntry,
};
pub use self::media_params::{
    audio_context_params, audio_media_params, compute_clip_range, screenshot_params, video_params,
    AudioMediaParams, ScreenshotParams, TimingSource, VideoParams,
};
pub use self::metadata::{
    build_note_metadata, replace_newlines, substitute_markers, DuplicatePolicy, MarkerResult,
};
pub use self::pitch::{
    classify_pitch, classify_pitch_categories, format_pitch, format_pitch_categories,
    format_pitch_graphs, format_pitch_positions, is_kifuku_applicable, PitchCategory,
    PitchCategoryInput, PitchEntry, PitchGraphEntry,
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::json::{JsonObject, JsonValue};
    use crate::tokenizer::{self, Marker};
    use std::cell::RefCell;

    fn u(text: &str) -> Vec<u16> {
        text.encode_utf16().collect()
    }

    fn u_str(units: &[u16]) -> String {
        String::from_utf16_lossy(units)
    }

    fn parse_marker(template: &str) -> Marker {
        tokenizer::tokenize(&u(template))
            .into_iter()
            .next()
            .unwrap()
            .markers
            .into_iter()
            .next()
            .unwrap()
    }

    fn text(value: &str) -> MarkerResult {
        MarkerResult {
            handled: true,
            text: u(value),
            ..Default::default()
        }
    }

    #[test]
    fn newlines_are_literal_and_replacement_is_not_recursive() {
        assert_eq!(
            replace_newlines(&u("A\r\nB\nC\u{2028}"), &u("<br>\n")),
            u("A\r<br>\nB<br>\nC\u{2028}")
        );
        assert_eq!(replace_newlines(&u("a\nb"), &[]), u("ab"));
    }

    #[test]
    fn alternatives_preserve_empty_fallback_and_unknown_token() {
        let calls = RefCell::new(Vec::new());
        let result = substitute_markers(
            &u("[{unknown}] {blank|missing} {blank|filled} {missing|filled}"),
            |marker| {
                calls
                    .borrow_mut()
                    .push(format!("common:{}", u_str(&marker.name)));
                if marker.name == u("blank") {
                    MarkerResult {
                        handled: true,
                        ..Default::default()
                    }
                } else {
                    MarkerResult::default()
                }
            },
            |marker| {
                calls
                    .borrow_mut()
                    .push(format!("specific:{}", u_str(&marker.name)));
                if marker.name == u("filled") {
                    text("OK")
                } else {
                    MarkerResult::default()
                }
            },
        );
        assert_eq!(result, u("[{unknown}]  OK OK"));
        assert_eq!(
            calls.into_inner(),
            [
                "common:unknown",
                "specific:unknown",
                "common:blank",
                "common:missing",
                "specific:missing",
                "common:blank",
                "common:filled",
                "specific:filled",
                "common:missing",
                "specific:missing",
                "common:filled",
                "specific:filled",
            ]
        );
    }

    #[test]
    fn media_short_circuits_and_removes_raw_marker() {
        let result = substitute_markers(
            &u("before {audio|expression} after"),
            |marker| {
                if marker.name == u("audio") {
                    MarkerResult {
                        handled: true,
                        media: true,
                        ..Default::default()
                    }
                } else {
                    panic!("media did not short circuit")
                }
            },
            |_| panic!("specific handler should not run"),
        );
        assert_eq!(result, u("before  after"));
    }

    #[test]
    fn replacements_are_global_and_cascade_between_original_tokens() {
        let result = substitute_markers(
            &u("{a} / {a} / {b}"),
            |_| MarkerResult::default(),
            |marker| {
                if marker.name == u("a") {
                    text("{b}")
                } else {
                    text("x")
                }
            },
        );
        assert_eq!(result, u("x / x / x"));
        let result =
            substitute_markers(&u("{a} {a}"), |_| MarkerResult::default(), |_| text("{a}x"));
        assert_eq!(result, u("{a}xx {a}xx"));
    }

    #[test]
    fn utf16_code_units_survive_even_without_tokens() {
        let input = vec![0xD800, b'{' as u16, b'a' as u16, b'}' as u16];
        assert_eq!(
            substitute_markers(&input, |_| panic!("no match"), |_| panic!("no match")),
            input
        );
    }

    #[test]
    fn metadata_has_exact_qjson_field_and_policy_shapes() {
        for (policy, expected_key, expected_value) in [
            (
                DuplicatePolicy::None,
                "allowDuplicate",
                JsonValue::Bool(false),
            ),
            (
                DuplicatePolicy::DifferentDeck,
                "duplicateScope",
                JsonValue::String(u("deck")),
            ),
            (
                DuplicatePolicy::SameDeck,
                "allowDuplicate",
                JsonValue::Bool(true),
            ),
        ] {
            let mut fields = JsonObject::new();
            fields.insert(u("Front"), JsonValue::String(u("old")));
            fields.insert(u("Front"), JsonValue::String(u("<new>")));
            let note = build_note_metadata(
                u("deck\u{1F600}"),
                u("model"),
                vec![u("a"), u("a")],
                JsonValue::Object(fields),
                policy,
            );
            assert_eq!(note.len(), 5);
            assert_eq!(
                note.get(&u("deckName")),
                Some(&JsonValue::String(u("deck\u{1F600}")))
            );
            assert_eq!(
                note.get(&u("modelName")),
                Some(&JsonValue::String(u("model")))
            );
            assert_eq!(
                note.get(&u("tags")),
                Some(&JsonValue::Array(vec![
                    JsonValue::String(u("a")),
                    JsonValue::String(u("a")),
                ]))
            );
            let Some(JsonValue::Object(fields)) = note.get(&u("fields")) else {
                panic!("fields object")
            };
            assert_eq!(fields.len(), 1);
            assert_eq!(
                fields.get(&u("Front")),
                Some(&JsonValue::String(u("<new>")))
            );
            let Some(JsonValue::Object(options)) = note.get(&u("options")) else {
                panic!("options object")
            };
            assert_eq!(options.len(), 1);
            assert_eq!(options.get(&u(expected_key)), Some(&expected_value));
        }
    }

    #[test]
    fn parse_frequency_value_matches_qt_regex_and_stars() {
        assert_eq!(parse_frequency_value(&u("123.45Hz")), Some(123.45));
        assert_eq!(parse_frequency_value(&u("+0.5")), Some(0.5));
        assert_eq!(parse_frequency_value(&u(".25")), Some(0.25));
        assert_eq!(parse_frequency_value(&u("-.5")), Some(-0.5));
        assert_eq!(parse_frequency_value(&u("12.")), Some(12.0));
        assert_eq!(parse_frequency_value(&u("★")), Some(20.0));
        assert_eq!(parse_frequency_value(&u("★★")), Some(40.0));
        assert_eq!(parse_frequency_value(&u("★★★")), Some(60.0));
        assert_eq!(parse_frequency_value(&u("★★★★")), Some(80.0));
        assert_eq!(parse_frequency_value(&u("★★★★★")), Some(100.0));

        assert_eq!(parse_frequency_value(&u("★★★★★★")), None);
        assert_eq!(parse_frequency_value(&u("１２３")), None);
        assert_eq!(parse_frequency_value(&u("")), None);
        assert_eq!(parse_frequency_value(&u("+")), None);
        assert_eq!(parse_frequency_value(&u(".")), None);
        assert_eq!(parse_frequency_value(&u("+.")), None);
        assert_eq!(parse_frequency_value(&u("abc")), None);
        assert_eq!(parse_frequency_value(&u("★ ")), None);
        assert_eq!(parse_frequency_value(&u(" ★")), None);
    }

    #[test]
    fn frequency_stats_aggregates_and_skips_duplicates_and_empty_dicts() {
        let d_empty = u("");
        let d_a = u("DictA");
        let d_b = u("DictB");
        let f_zero = u("0.0");
        let f_neg = u("-5.0");
        let f_3 = u("3");
        let f_5 = u("5");
        let f_10 = u("10.5");
        let f_20 = u("20.0");

        let entries = [
            FrequencyEntry {
                dictionary: &d_empty,
                freq: &f_10,
            },
            FrequencyEntry {
                dictionary: &d_a,
                freq: &f_20,
            },
        ];
        let (numbers, harmonic, average) = frequency_stats(&entries);
        assert_eq!(numbers, [20.0]);
        assert_eq!(harmonic, Some(20));
        assert_eq!(average, Some(20));

        let entries2 = [
            FrequencyEntry {
                dictionary: &d_a,
                freq: &f_zero,
            },
            FrequencyEntry {
                dictionary: &d_a,
                freq: &f_neg,
            },
            FrequencyEntry {
                dictionary: &d_a,
                freq: &f_10,
            },
            FrequencyEntry {
                dictionary: &d_a,
                freq: &f_20,
            },
        ];
        let (numbers2, _, _) = frequency_stats(&entries2);
        assert_eq!(numbers2, [10.5]);

        let entries3 = [
            FrequencyEntry {
                dictionary: &d_a,
                freq: &f_3,
            },
            FrequencyEntry {
                dictionary: &d_b,
                freq: &f_5,
            },
        ];
        let (numbers3, harmonic3, average3) = frequency_stats(&entries3);
        assert_eq!(numbers3, [3.0, 5.0]);
        assert_eq!(harmonic3, Some(3));
        assert_eq!(average3, Some(4));

        let (numbers4, harmonic4, average4) = frequency_stats(&[]);
        assert!(numbers4.is_empty());
        assert_eq!(harmonic4, None);
        assert_eq!(average4, None);
    }

    #[test]
    fn test_is_kifuku_applicable() {
        let v1 = u("v1");
        let v5 = u("v5");
        let vk = u("vk");
        let vz = u("vz");
        let adj_i = u("adj-i");
        let vs = u("vs");
        let n = u("n");

        assert!(!is_kifuku_applicable(&[]));
        assert!(is_kifuku_applicable(&[&v1]));
        assert!(is_kifuku_applicable(&[&v5]));
        assert!(is_kifuku_applicable(&[&vk]));
        assert!(is_kifuku_applicable(&[&vz]));
        assert!(is_kifuku_applicable(&[&adj_i]));
        assert!(!is_kifuku_applicable(&[&n]));
        assert!(!is_kifuku_applicable(&[&vs]));
        assert!(!is_kifuku_applicable(&[&v1, &vs]));
        assert!(!is_kifuku_applicable(&[&vs, &v1]));
        assert!(is_kifuku_applicable(&[&n, &v5]));
    }

    #[test]
    fn test_format_furigana() {
        let surface1 = u("日");
        let reading1 = u("に");
        let surface2 = u("本");
        let reading2 = u("ほん");
        let reading_empty = u("");

        let pairs = [
            FuriganaPair {
                surface: &surface1,
                reading: &reading1,
            },
            FuriganaPair {
                surface: &surface2,
                reading: &reading2,
            },
        ];
        assert_eq!(
            format_furigana(&pairs, false),
            u("<ruby>日<rt>に</rt></ruby><ruby>本<rt>ほん</rt></ruby>")
        );
        assert_eq!(format_furigana(&pairs, true), u("日[に]本[ほん]"));

        let pairs_with_plain = [
            FuriganaPair {
                surface: &surface1,
                reading: &reading_empty,
            },
            FuriganaPair {
                surface: &surface2,
                reading: &reading2,
            },
        ];
        assert_eq!(
            format_furigana(&pairs_with_plain, false),
            u("日<ruby>本<rt>ほん</rt></ruby>")
        );
        assert_eq!(format_furigana(&pairs_with_plain, true), u("日本[ほん]"));

        assert_eq!(format_furigana(&[], false), u(""));
        assert_eq!(format_furigana(&[], true), u(""));
    }

    #[test]
    fn test_build_frequencies_html_and_escaping() {
        let d1 = u("Dict1");
        let d2 = u("Dict2:");
        let f1 = u("100");
        let f2 = u("50");
        let f_invalid = u("invalid");

        let entries = [
            FrequencyEntry {
                dictionary: &d1,
                freq: &f1,
            },
            FrequencyEntry {
                dictionary: &d2,
                freq: &f2,
            },
        ];

        assert_eq!(
            build_frequencies_html(&entries, false, false),
            u("<ul><li>Dict1: 100</li><li>Dict2: 50</li></ul>")
        );
        assert_eq!(
            build_frequencies_html(&entries, true, false),
            u("100<br>50<br>")
        );
        assert_eq!(
            build_frequencies_html(&entries, false, true),
            u("<ul><li>Dict2: 50</li></ul>")
        );
        assert_eq!(build_frequencies_html(&entries, true, true), u("50"));
        assert_eq!(build_frequencies_html(&[], false, false), u(""));

        let entries_invalid = [FrequencyEntry {
            dictionary: &d1,
            freq: &f_invalid,
        }];
        assert_eq!(
            build_frequencies_html(&entries_invalid, false, true),
            u("<ul><li></li></ul>")
        );
        assert_eq!(build_frequencies_html(&entries_invalid, true, true), u(""));

        let evil_dict = u("<script>D&1</script>");
        let evil_freq = u("10<b>x</b>");
        let escaped_entries = [FrequencyEntry {
            dictionary: &evil_dict,
            freq: &evil_freq,
        }];
        assert_eq!(
            u_str(&build_frequencies_html(&escaped_entries, false, false)),
            "<ul><li>&lt;script&gt;D&amp;1&lt;/script&gt;: 10&lt;b&gt;x&lt;/b&gt;</li></ul>"
        );
        assert_eq!(
            u_str(&build_frequencies_html(&escaped_entries, true, true)),
            "10&lt;b&gt;x&lt;/b&gt;"
        );
    }

    #[test]
    fn test_format_pitch_and_positions_with_escaping() {
        let d1 = u("DictA");
        let d2 = u("Dict<B>");
        let m1 = u("に");
        let m2 = u("ほ");
        let m3 = u("ん");

        let mora = [&m1[..], &m2[..], &m3[..]];
        let pos0 = [0u8];
        let p_heiban = [PitchEntry {
            dictionary_name: &d1,
            positions: &pos0,
            mora: &mora,
        }];
        assert_eq!(
            format_pitch(&p_heiban),
            u("<span class=\"memento-pitch\">に<span style=\"border-top: solid;\">ほん</span></span>")
        );
        assert_eq!(
            format_pitch_positions(&p_heiban),
            u("<span class=\"memento-pitch-position\">[0]</span>")
        );

        let pos1 = [1u8];
        let p_atama = [PitchEntry {
            dictionary_name: &d1,
            positions: &pos1,
            mora: &mora,
        }];
        assert_eq!(
            format_pitch(&p_atama),
            u("<span class=\"memento-pitch\"><span style=\"border-top: solid; border-right: solid;\">に</span>ほん</span>")
        );

        let pos2 = [2u8];
        let p_naka = [PitchEntry {
            dictionary_name: &d1,
            positions: &pos2,
            mora: &mora,
        }];
        assert_eq!(
            format_pitch(&p_naka),
            u("<span class=\"memento-pitch\">に<span style=\"border-top: solid; border-right: solid;\">ほ</span>ん</span>")
        );

        let p_multi_dicts = [
            PitchEntry {
                dictionary_name: &d1,
                positions: &pos0,
                mora: &mora,
            },
            PitchEntry {
                dictionary_name: &d2,
                positions: &pos1,
                mora: &mora,
            },
        ];
        assert!(u_str(&format_pitch(&p_multi_dicts)).contains("<i>Dict&lt;B&gt;</i>"));
        assert!(u_str(&format_pitch_positions(&p_multi_dicts)).contains("<i>Dict&lt;B&gt;</i>"));
        assert_eq!(format_pitch(&[]), u(""));
        assert_eq!(format_pitch_positions(&[]), u(""));
    }

    #[test]
    fn test_format_tags_and_escaping() {
        let t1_name = u("JLPT <N5>");
        let t1_notes = u("common & \"basic\"");
        let t2_name = u("archaic");
        let t2_notes = u("");

        let tags = [
            TagEntry {
                name: &t1_name,
                notes: &t1_notes,
            },
            TagEntry {
                name: &t2_name,
                notes: &t2_notes,
            },
        ];

        assert_eq!(
            u_str(&format_tags(&tags, false)),
            "<ul><li>JLPT &lt;N5&gt;: common &amp; &quot;basic&quot;</li><li>archaic</li></ul>"
        );
        assert_eq!(
            u_str(&format_tags(&tags, true)),
            "<ul><li>JLPT &lt;N5&gt;</li><li>archaic</li></ul>"
        );
        assert_eq!(format_tags(&[], false), u(""));
    }

    #[test]
    fn test_format_audio_url_percent_encoding_and_filename_sanitization() {
        let url = u("https://example.com/?term={term}&reading={reading}");
        let src = u("Forvo Audio");
        let expr = u("食べる");
        let read = u("たべる");

        let (out_url, out_fn) = format_audio_url_and_filename(&url, &src, &expr, &read, false);
        assert_eq!(
            u_str(&out_url),
            "https://example.com/?term=%E9%A3%9F%E3%81%B9%E3%82%8B&reading=%E3%81%9F%E3%81%B9%E3%82%8B"
        );
        assert_eq!(u_str(&out_fn), "memento_Forvo_Audio_たべる_食べる.mp3");

        let evil_src = u("../evil/src");
        let evil_expr = u("a&b=c?d#e[f]/g\\h\0.");
        let evil_read = u("r:1*2\"3<4>5|6");
        let (safe_url, safe_fn) =
            format_audio_url_and_filename(&url, &evil_src, &evil_expr, &evil_read, false);
        let safe_url_str = u_str(&safe_url);
        let safe_fn_str = u_str(&safe_fn);
        assert!(safe_url_str.contains("term=a%26b%3Dc%3Fd%23e%5Bf%5D%2Fg%5Ch%00."));
        assert!(!safe_fn_str.contains(".."));
        assert!(!safe_fn_str.contains('/'));
        assert!(!safe_fn_str.contains('\\'));
        assert!(!safe_fn_str.contains('['));
        assert!(!safe_fn_str.contains(']'));
        assert!(!safe_fn_str.contains('\0'));
        assert!(safe_fn_str.ends_with(".mp3"));
    }

    #[test]
    fn test_format_term_glossary_escaping_and_variant_skipping() {
        assert_eq!(format_term_glossary(&[]), TermGlossaryOutput::default());

        let d1 = u("JM\"dict<&>");
        let t1 = u("<v1>");
        let item1 = u("to eat");
        let item2 = u("to consume");
        let style1 = u(".x { color: red; }</STYLE><script>alert(1)</script>");

        let tags = [&t1[..]];
        let items = [&item1[..], &item2[..]];
        let defs = [TermGlossaryDef {
            dict_id: 1,
            dict_name: &d1,
            tags: &tags,
            items: &items,
            has_styles: true,
            stylesheet: &style1,
        }];

        let out = format_term_glossary(&defs);
        let full = u_str(&out.glossary);
        assert!(full.contains("<li data-dictionary=\"JM&quot;dict&lt;&amp;&gt;\">"));
        assert!(full.contains("<i>(&lt;v1&gt;, JM&quot;dict&lt;&amp;&gt;)</i>"));
        assert!(!full.to_ascii_lowercase().contains("</style><script>"));
        assert!(full.contains("<\\/STYLE>"));

        let brief_only = format_term_glossary_selected(&defs, TermGlossaryVariants::BRIEF);
        assert!(brief_only.glossary.is_empty());
        assert!(!brief_only.glossary_brief.is_empty());
        assert!(brief_only.glossary_compact.is_empty());
        assert_eq!(
            brief_only.glossary_brief,
            format_term_glossary_variant(&defs, TermGlossaryVariant::Brief)
        );
    }

    #[test]
    fn media_marker_arguments_reject_non_finite_and_trim_nel() {
        let nan_db = parse_marker("{audio-media:db=NaN}");
        assert_eq!(audio_media_params(&nan_db, false, -20.0), None);
        let inf_db = parse_marker("{video:db=inf}");
        assert_eq!(video_params(&inf_db, false, -20.0), None);
        let neg_inf_db = parse_marker("{audio-media:db=-infinity}");
        assert_eq!(audio_media_params(&neg_inf_db, false, -20.0), None);

        // U+0085 (NEL) is trimmed consistently with strings::qt_is_space
        let mut nel_db = Marker::default();
        nel_db.insert_arg(u("db"), u("\u{0085}-12.5\u{0085}"));
        assert_eq!(audio_media_params(&nel_db, false, -20.0).unwrap().db, -12.5);
    }

    #[test]
    fn test_compute_clip_range() {
        assert_eq!(
            compute_clip_range(TimingSource::Subtitle, 5.0, 10.0, 2.0, 15.0, 0.5, 0.5),
            Some((4.5, 10.5))
        );
        assert_eq!(
            compute_clip_range(TimingSource::Context, 5.0, 10.0, 2.0, 15.0, 0.5, 0.5),
            Some((1.5, 15.5))
        );
        assert_eq!(
            compute_clip_range(TimingSource::Subtitle, 0.2, 1.0, 0.0, 0.0, 0.5, 0.0),
            Some((0.0, 1.0))
        );
        assert_eq!(
            compute_clip_range(TimingSource::Subtitle, f64::NAN, 1.0, 0.0, 0.0, 0.0, 0.0),
            None
        );
    }
}

//! Pure operations from `src/anki/notebuilder.cpp` for Anki note metadata,
//! marker substitution, HTML formatting, frequency/pitch stats, and media
//! marker parameter parsing.

mod format;
mod frequency;
mod media_params;
mod metadata;
mod pitch;

pub use self::format::{
    find_stroke_count, format_cloze_escaped, format_cloze_parts_escaped, format_furigana,
    format_kanji_glossary, format_kanji_readings, format_selection, format_selection_u16,
    format_tags, format_term_glossary_selected, FuriganaPair, KanjiDefEntry, StrokeCountStat,
    TagEntry, TermGlossaryDef, TermGlossaryOutput, TermGlossaryVariant, TermGlossaryVariants,
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
    build_note_metadata, substitute_markers, DuplicatePolicy, MarkerResult,
};
pub use self::pitch::{
    classify_pitch, classify_pitch_categories, classify_pitch_categories_ex, classify_pitch_spec,
    format_pitch, format_pitch_categories, format_pitch_categories_ex, format_pitch_ex,
    format_pitch_graphs, format_pitch_graphs_ex, format_pitch_graphs_ex_styled,
    format_pitch_positions, format_pitch_positions_ex, generate_pitch_graph_svg,
    is_kifuku_applicable, PitchCategory, PitchCategoryInput, PitchEntry, PitchEntryEx,
    PitchGraphEntry, PitchSpec, DEFAULT_PITCH_GRAPH_FILL, DEFAULT_PITCH_GRAPH_STROKE,
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
    fn test_format_term_glossary_escaping_and_variant_skipping() {
        assert_eq!(
            format_term_glossary_selected(&[], TermGlossaryVariants::ALL),
            TermGlossaryOutput::default()
        );

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

        let out = format_term_glossary_selected(&defs, TermGlossaryVariants::ALL);
        let full = u_str(&out.glossary);
        assert!(full.contains("<li data-dictionary=\"JM&quot;dict&lt;&amp;&gt;\">"));
        assert!(full.contains("<i>(&lt;v1&gt;, JM&quot;dict&lt;&amp;&gt;)</i>"));
        assert!(!full.to_ascii_lowercase().contains("</style><script>"));
        assert!(full.contains("<\\/STYLE>"));

        let brief_only = format_term_glossary_selected(&defs, TermGlossaryVariants::BRIEF);
        assert!(brief_only.glossary.is_empty());
        assert!(!brief_only.glossary_brief.is_empty());
        assert!(brief_only.glossary_compact.is_empty());
        assert_eq!(brief_only.glossary_brief, out.glossary_brief);
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

    #[test]
    fn exact_pitch_patterns_render_html_svg_positions_and_categories() {
        let dict = u("NHK");
        let m1 = u("に");
        let m2 = u("ほ");
        let m3 = u("ん");
        let m4 = u("ご");
        let mora3 = [&m1[..], &m2[..], &m3[..]];
        let mora4 = [&m1[..], &m2[..], &m3[..], &m4[..]];
        let mora2 = [&m1[..], &m2[..]];

        // 3-mora Heiban ("LHH" and "LHHH") vs Odaka ("LHHL")
        let s_lhh = [PitchSpec::Pattern("LHH")];
        let s_lhhh = [PitchSpec::Pattern("LHHH")];
        let s_lhhl = [PitchSpec::Pattern("LHHL")];
        let s_hll = [PitchSpec::Pattern("HLL")];
        let s_lhl = [PitchSpec::Pattern("LHL")];

        let e_lhh = [PitchEntryEx {
            dictionary_name: &dict,
            specs: &s_lhh,
            mora: &mora3,
        }];
        let e_lhhh = [PitchEntryEx {
            dictionary_name: &dict,
            specs: &s_lhhh,
            mora: &mora3,
        }];
        let e_lhhl_odaka = [PitchEntryEx {
            dictionary_name: &dict,
            specs: &s_lhhl,
            mora: &mora3,
        }];
        let e_lhhl_naka = [PitchEntryEx {
            dictionary_name: &dict,
            specs: &s_lhhl,
            mora: &mora4,
        }];
        let e_hll = [PitchEntryEx {
            dictionary_name: &dict,
            specs: &s_hll,
            mora: &mora3,
        }];
        let e_lhl = [PitchEntryEx {
            dictionary_name: &dict,
            specs: &s_lhl,
            mora: &mora3,
        }];
        let e_lhl_odaka2 = [PitchEntryEx {
            dictionary_name: &dict,
            specs: &s_lhl,
            mora: &mora2,
        }];

        // Overline + downstep HTML
        assert_eq!(
            u_str(&format_pitch_ex(&e_lhh)),
            "<span class=\"memento-pitch\">に<span style=\"border-top: solid;\">ほん</span></span>"
        );
        assert_eq!(
            u_str(&format_pitch_ex(&e_lhhh)),
            "<span class=\"memento-pitch\">に<span style=\"border-top: solid;\">ほん</span></span>"
        );
        // Odaka ("LHHL" on 3 morae): downstep after final mora -> border-right: solid!
        assert_eq!(
            u_str(&format_pitch_ex(&e_lhhl_odaka)),
            "<span class=\"memento-pitch\">に<span style=\"border-top: solid; border-right: solid;\">ほん</span></span>"
        );
        // Nakadaka ("LHHL" on 4 morae): downstep after 3rd mora -> ん has border-right, ご is low.
        assert_eq!(
            u_str(&format_pitch_ex(&e_lhhl_naka)),
            "<span class=\"memento-pitch\">に<span style=\"border-top: solid; border-right: solid;\">ほん</span>ご</span>"
        );
        // Atamadaka ("HLL")
        assert_eq!(
            u_str(&format_pitch_ex(&e_hll)),
            "<span class=\"memento-pitch\"><span style=\"border-top: solid; border-right: solid;\">に</span>ほん</span>"
        );
        // Nakadaka ("LHL")
        assert_eq!(
            u_str(&format_pitch_ex(&e_lhl)),
            "<span class=\"memento-pitch\">に<span style=\"border-top: solid; border-right: solid;\">ほ</span>ん</span>"
        );

        // Position text
        assert_eq!(
            u_str(&format_pitch_positions_ex(&e_lhh)),
            "<span class=\"memento-pitch-position\">[LHH]</span>"
        );
        assert_eq!(
            u_str(&format_pitch_positions_ex(&e_lhhl_odaka)),
            "<span class=\"memento-pitch-position\">[LHHL]</span>"
        );
        assert_eq!(
            u_str(&format_pitch_positions_ex(&e_hll)),
            "<span class=\"memento-pitch-position\">[HLL]</span>"
        );
        assert_eq!(
            u_str(&format_pitch_positions_ex(&e_lhl)),
            "<span class=\"memento-pitch-position\">[LHL]</span>"
        );

        // Categories: heiban vs odaka vs atamadaka vs nakadaka vs kifuku
        assert_eq!(u_str(&format_pitch_categories_ex(&e_lhh, false)), "heiban");
        assert_eq!(u_str(&format_pitch_categories_ex(&e_lhhh, false)), "heiban");
        assert_eq!(u_str(&format_pitch_categories_ex(&e_lhhl_odaka, false)), "odaka");
        assert_eq!(u_str(&format_pitch_categories_ex(&e_lhhl_odaka, true)), "odaka");
        assert_eq!(u_str(&format_pitch_categories_ex(&e_lhl_odaka2, false)), "odaka");
        assert_eq!(u_str(&format_pitch_categories_ex(&e_lhhl_naka, false)), "nakadaka");
        assert_eq!(u_str(&format_pitch_categories_ex(&e_lhhl_naka, true)), "kifuku");
        assert_eq!(u_str(&format_pitch_categories_ex(&e_hll, false)), "atamadaka");
        assert_eq!(u_str(&format_pitch_categories_ex(&e_hll, true)), "kifuku");
        assert_eq!(u_str(&format_pitch_categories_ex(&e_lhl, false)), "nakadaka");
        assert_eq!(u_str(&format_pitch_categories_ex(&e_lhl, true)), "kifuku");

        // SVG pitch graphs: verify exact parity with Downstep and heiban vs odaka particle dots
        let svg_lhh = generate_pitch_graph_svg(3, PitchSpec::Pattern("LHH"), "#fff", "#000").unwrap();
        let svg_d0 = generate_pitch_graph_svg(3, PitchSpec::Downstep(0), "#fff", "#000").unwrap();
        assert_eq!(svg_lhh, svg_d0);
        assert!(svg_lhh.contains("viewBox=\"0 0 200 100\""));
        assert!(svg_lhh.contains("<use href=\"#black-dot\" x=\"125\" y=\"25\"/>"));
        assert!(svg_lhh.contains("<use href=\"#white-dot\" x=\"175\" y=\"25\"/>"));

        let svg_lhhl = generate_pitch_graph_svg(3, PitchSpec::Pattern("LHHL"), "#fff", "#000").unwrap();
        let svg_d3 = generate_pitch_graph_svg(3, PitchSpec::Downstep(3), "#fff", "#000").unwrap();
        assert_eq!(svg_lhhl, svg_d3);
        assert!(svg_lhhl.contains("<use href=\"#change-dot\" x=\"125\" y=\"25\"/>"));
        assert!(svg_lhhl.contains("<use href=\"#white-dot\" x=\"175\" y=\"75\"/>"));

        let svg_hll = generate_pitch_graph_svg(3, PitchSpec::Pattern("HLL"), "#fff", "#000").unwrap();
        let svg_d1 = generate_pitch_graph_svg(3, PitchSpec::Downstep(1), "#fff", "#000").unwrap();
        assert_eq!(svg_hll, svg_d1);

        let svg_lhl = generate_pitch_graph_svg(3, PitchSpec::Pattern("LHL"), "#fff", "#000").unwrap();
        let svg_d2 = generate_pitch_graph_svg(3, PitchSpec::Downstep(2), "#fff", "#000").unwrap();
        assert_eq!(svg_lhl, svg_d2);

        // Non-standard / Kansai-ben exact patterns: "HHH", "LLL", "HLHL", lowercase "lhh"
        let s_multi = [
            PitchSpec::Pattern("HHH"),
            PitchSpec::Pattern("LLL"),
            PitchSpec::Pattern("HLHL"),
            PitchSpec::Pattern("lhh"),
            PitchSpec::Pattern("INVALID"),
        ];
        let e_multi = [PitchEntryEx {
            dictionary_name: &dict,
            specs: &s_multi,
            mora: &mora4,
        }];
        let multi_html = u_str(&format_pitch_ex(&e_multi));
        assert!(multi_html.contains("<span style=\"border-top: solid;\">にほんご</span>"));
        assert!(multi_html.contains("<li style=\"padding: 2px 0px 2px 0px;\">にほんご</li>"));
        assert!(multi_html.contains(
            "<span style=\"border-top: solid; border-right: solid;\">に</span>ほ<span style=\"border-top: solid; border-right: solid;\">ん</span>ご"
        ));
        let multi_pos = u_str(&format_pitch_positions_ex(&e_multi));
        assert!(multi_pos.contains("<li>[HHH]</li>"));
        assert!(multi_pos.contains("<li>[LLL]</li>"));
        assert!(multi_pos.contains("<li>[HLHL]</li>"));
        assert!(multi_pos.contains("<li>[LHH]</li>"));
        assert!(!multi_pos.contains("INVALID"));

        let graphs_html = u_str(&format_pitch_graphs_ex(&e_multi));
        assert!(graphs_html.starts_with("<span class=\"memento-pitch-graph\"><ol>"));
        assert_eq!(graphs_html.matches("<svg ").count(), 4);
        assert_eq!(classify_pitch_spec(3, PitchSpec::Pattern(""), false), None);
        assert_eq!(classify_pitch_spec(3, PitchSpec::Pattern("XY"), false), None);
    }

    #[test]
    fn format_selection_handles_multi_definitions_and_prevents_double_br() {
        // Empty / blank selections
        assert_eq!(format_selection(&[]), "");
        assert_eq!(format_selection(&["", "   ", "\r\n\t\n"]), "");

        // Single clean entry (blank neighbors ignored, no <ul> wrapper)
        assert_eq!(format_selection(&["  only one  "]), "only one");
        assert_eq!(format_selection(&["", "  only one  ", "\r\n"]), "only one");

        // Line-ending normalization & no double <br> (Yomitan #2248)
        let single_multiline = format_selection(&["\r\n\nline 1\r\n\r\n  \nline 2\rline 3\n\n"]);
        assert_eq!(single_multiline, "line 1<br>line 2<br>line 3");
        assert!(!single_multiline.contains("<br><br>"));
        assert!(!single_multiline.contains('\r'));
        assert!(!single_multiline.contains('\n'));

        // Multi-definition selections (#262) wrapped in <ul><li>...</li></ul>
        let multi = format_selection(&[
            "first def\r\n\r\nsecond line\r\n",
            "   ",
            "second <def> & \"quoted\"",
        ]);
        assert_eq!(
            multi,
            "<ul><li>first def<br>second line</li><li>second &lt;def&gt; &amp; &quot;quoted&quot;</li></ul>"
        );
        assert!(!multi.contains("<br><br>"));

        // UTF-16 wrapper matches UTF-8 formatter
        let u1 = u("a\r\n\r\nb");
        let u2 = u("c < d");
        assert_eq!(
            u_str(&format_selection_u16(&[&u1, &u2])),
            "<ul><li>a<br>b</li><li>c &lt; d</li></ul>"
        );
    }

    #[test]
    fn regression_yomitan_2492_2394_html_entity_escaping_across_glossary_cloze_pitch_and_dicts() {
        use crate::glossary::{build_glossary, escape_html_str, escape_html_u16};

        let payload = "<script>\"a&b\"</script>";
        let expected_escaped = "&lt;script&gt;&quot;a&amp;b&quot;&lt;/script&gt;";

        // 1. Direct escape helpers
        assert_eq!(escape_html_str(payload), expected_escaped);
        assert_eq!(u_str(&escape_html_u16(&u(payload))), expected_escaped);

        // 2. Glossary (plain string, text object, structured-content text & attrs, kanji glossary)
        let mut text_obj = JsonObject::new();
        text_obj.insert(u("type"), JsonValue::String(u("text")));
        text_obj.insert(u("text"), JsonValue::String(u(payload)));

        let mut sc_span = JsonObject::new();
        sc_span.insert(u("tag"), JsonValue::String(u("span")));
        sc_span.insert(u("title"), JsonValue::String(u(payload)));
        sc_span.insert(u("content"), JsonValue::String(u(payload)));
        let mut sc_obj = JsonObject::new();
        sc_obj.insert(u("type"), JsonValue::String(u("structured-content")));
        sc_obj.insert(u("content"), JsonValue::Object(sc_span));

        let (built, _) = build_glossary(
            &[
                JsonValue::String(u(payload)),
                JsonValue::Object(text_obj),
                JsonValue::Object(sc_obj),
            ],
            &u("/base"),
            |_| None,
        );
        assert_eq!(u_str(&built[0]), expected_escaped);
        assert_eq!(u_str(&built[1]), expected_escaped);
        assert_eq!(
            u_str(&built[2]),
            format!("<span title=\"{expected_escaped}\">{expected_escaped}</span>")
        );

        let dict_name = u(payload);
        let tag_name = u("tag<1>&\"2\"");
        let kanji_line = u(payload);
        let kanji_html = u_str(&format_kanji_glossary(&[KanjiDefEntry {
            dictionary_name: &dict_name,
            tags: &[&tag_name],
            glossary: &[&kanji_line],
        }]));
        assert!(!kanji_html.contains("<script>"));
        assert!(kanji_html.contains(expected_escaped));
        assert!(kanji_html.contains("tag&lt;1&gt;&amp;&quot;2&quot;"));

        // 3. Cloze prefix / body / suffix escaping
        assert_eq!(
            format_cloze_parts_escaped("a<b", "c&\"d\"", "e>f"),
            (
                "a&lt;b".to_owned(),
                "c&amp;&quot;d&quot;".to_owned(),
                "e&gt;f".to_owned()
            )
        );
        assert_eq!(
            format_cloze_escaped("<b>", "x & \"y\"", "</b>", 1),
            "&lt;b&gt;{{c1::x &amp; &quot;y&quot;}}&lt;/b&gt;"
        );

        // 4. Pitch dictionary names & mora (both legacy and _ex APIs)
        let d_a = u("Dict<A>&\"1\"");
        let d_b = u("Dict<B>");
        let evil_m1 = u("<に>");
        let evil_m2 = u("ほ&\"ん\"");
        let evil_mora = [&evil_m1[..], &evil_m2[..]];
        let pos0 = [0u8];
        let legacy_pitches = [
            PitchEntry {
                dictionary_name: &d_a,
                positions: &pos0,
                mora: &evil_mora,
            },
            PitchEntry {
                dictionary_name: &d_b,
                positions: &pos0,
                mora: &evil_mora,
            },
        ];
        let legacy_html = u_str(&format_pitch(&legacy_pitches));
        assert!(legacy_html.contains("<i>Dict&lt;A&gt;&amp;&quot;1&quot;</i>"));
        assert!(legacy_html.contains("&lt;に&gt;"));
        assert!(legacy_html.contains("ほ&amp;&quot;ん&quot;"));
        assert!(!legacy_html.contains("<に>"));

        let specs = [PitchSpec::Pattern("LH")];
        let ex_pitches = [
            PitchEntryEx {
                dictionary_name: &d_a,
                specs: &specs,
                mora: &evil_mora,
            },
            PitchEntryEx {
                dictionary_name: &d_b,
                specs: &specs,
                mora: &evil_mora,
            },
        ];
        let ex_html = u_str(&format_pitch_ex(&ex_pitches));
        assert!(ex_html.contains("<i>Dict&lt;A&gt;&amp;&quot;1&quot;</i>"));
        assert!(ex_html.contains("&lt;に&gt;"));
        assert!(ex_html.contains("ほ&amp;&quot;ん&quot;"));
        let ex_pos_html = u_str(&format_pitch_positions_ex(&ex_pitches));
        assert!(ex_pos_html.contains("<i>Dict&lt;A&gt;&amp;&quot;1&quot;</i>"));
        let ex_graph_html = u_str(&format_pitch_graphs_ex(&ex_pitches));
        assert!(ex_graph_html.contains("<i>Dict&lt;A&gt;&amp;&quot;1&quot;</i>"));
    }

    #[test]
    fn regression_yomitan_2144_deduplicated_compact_style_blocks_per_dict_id() {
        let d1 = u("JMdict");
        let d2 = u("大辞林");
        let i1 = u("sense 1");
        let i2 = u("sense 2");
        let i3 = u("sense 3");
        let i4 = u("語釈 1");
        let i5 = u("語釈 2");
        let blank_css = u("   \r\n\t  ");
        let css1 = u(".d1-tag{color:#59b2ff;}");
        let css2 = u(".d2-note{font-weight:bold;}</style><img src=x>");

        let items1 = [&i1[..]];
        let items2 = [&i2[..]];
        let items3 = [&i3[..]];
        let items4 = [&i4[..]];
        let items5 = [&i5[..]];

        let defs = [
            // Whitespace-only stylesheet for dict_id 1 must NOT emit empty <style> or block later non-empty CSS.
            TermGlossaryDef {
                dict_id: 1,
                dict_name: &d1,
                tags: &[],
                items: &items1,
                has_styles: true,
                stylesheet: &blank_css,
            },
            TermGlossaryDef {
                dict_id: 1,
                dict_name: &d1,
                tags: &[],
                items: &items2,
                has_styles: true,
                stylesheet: &css1,
            },
            // Duplicate dict_id 1 must NOT emit a second copy of css1.
            TermGlossaryDef {
                dict_id: 1,
                dict_name: &d1,
                tags: &[],
                items: &items3,
                has_styles: true,
                stylesheet: &css1,
            },
            TermGlossaryDef {
                dict_id: 2,
                dict_name: &d2,
                tags: &[],
                items: &items4,
                has_styles: true,
                stylesheet: &css2,
            },
            // Duplicate dict_id 2 must NOT emit a second copy of css2.
            TermGlossaryDef {
                dict_id: 2,
                dict_name: &d2,
                tags: &[],
                items: &items5,
                has_styles: true,
                stylesheet: &css2,
            },
        ];

        let out = format_term_glossary_selected(&defs, TermGlossaryVariants::ALL);
        for (label, html) in [
            ("glossary", u_str(&out.glossary)),
            ("glossary_brief", u_str(&out.glossary_brief)),
            ("glossary_compact", u_str(&out.glossary_compact)),
        ] {
            // Exactly 3 <style> blocks: 1 for dict_id 1 + 1 for dict_id 2 + 1 built-in base <style>.
            assert_eq!(
                html.matches("<style>").count(),
                3,
                "{label} should have deduplicated <style> blocks: {html}"
            );
            assert_eq!(
                html.matches(".d1-tag{color:#59b2ff;}").count(),
                1,
                "{label} duplicated dict 1 CSS"
            );
            assert_eq!(
                html.matches(".d2-note{font-weight:bold;}").count(),
                1,
                "{label} duplicated dict 2 CSS"
            );
            assert!(!html.contains("<style></style>"), "{label} emitted empty style");
            assert!(!html.contains("</style><img"), "{label} failed to escape </style>");
            assert!(html.contains("<\\/style><img"), "{label} missing escaped </style>");
        }
    }
}

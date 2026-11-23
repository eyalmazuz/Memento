//! Anki note metadata, marker substitution, HTML formatting, frequency/pitch
//! stats, and media marker parameter parsing.

mod format;
mod frequency;
mod media_params;
mod metadata;
mod pitch;

pub use self::format::{
    find_stroke_count, format_cloze_escaped, format_cloze_parts_escaped, format_furigana,
    format_kanji_glossary, format_kanji_readings, format_selection, format_tags,
    format_term_glossary_selected, FuriganaPair, KanjiDefEntry, StrokeCountStat, TagEntry,
    TermGlossaryDef, TermGlossaryOutput, TermGlossaryVariant, TermGlossaryVariants,
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
    use crate::tokenizer::{self, Marker};
    use serde_json::{json, Value};
    use std::cell::RefCell;

    fn parse_marker(template: &str) -> Marker {
        tokenizer::tokenize(template)
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
            text: value.to_owned(),
            ..Default::default()
        }
    }

    #[test]
    fn alternatives_preserve_empty_fallback_and_unknown_token() {
        let calls = RefCell::new(Vec::new());
        let result = substitute_markers(
            "[{unknown}] {blank|missing} {blank|filled} {missing|filled}",
            |marker| {
                calls.borrow_mut().push(format!("common:{}", marker.name));
                if marker.name == "blank" {
                    MarkerResult {
                        handled: true,
                        ..Default::default()
                    }
                } else {
                    MarkerResult::default()
                }
            },
            |marker| {
                calls.borrow_mut().push(format!("specific:{}", marker.name));
                if marker.name == "filled" {
                    text("OK")
                } else {
                    MarkerResult::default()
                }
            },
        );
        assert_eq!(result, "[{unknown}]  OK OK");
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
            "before {audio|expression} after",
            |marker| {
                if marker.name == "audio" {
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
        assert_eq!(result, "before  after");
    }

    #[test]
    fn replacements_are_global_and_cascade_between_original_tokens() {
        let result = substitute_markers(
            "{a} / {a} / {b}",
            |_| MarkerResult::default(),
            |marker| {
                if marker.name == "a" {
                    text("{b}")
                } else {
                    text("x")
                }
            },
        );
        assert_eq!(result, "x / x / x");
        let result =
            substitute_markers("{a} {a}", |_| MarkerResult::default(), |_| text("{a}x"));
        assert_eq!(result, "{a}xx {a}xx");
    }

    #[test]
    fn metadata_has_exact_json_field_and_policy_shapes() {
        for (policy, expected_key, expected_value) in [
            (DuplicatePolicy::None, "allowDuplicate", json!(false)),
            (DuplicatePolicy::DifferentDeck, "duplicateScope", json!("deck")),
            (DuplicatePolicy::SameDeck, "allowDuplicate", json!(true)),
        ] {
            let note = build_note_metadata(
                "deck\u{1F600}".to_owned(),
                "model".to_owned(),
                vec!["a".to_owned(), "a".to_owned()],
                json!({ "Front": "<new>" }),
                policy,
            );
            assert_eq!(note.len(), 5);
            assert_eq!(note.get("deckName"), Some(&json!("deck\u{1F600}")));
            assert_eq!(note.get("modelName"), Some(&json!("model")));
            assert_eq!(note.get("tags"), Some(&json!(["a", "a"])));
            let Some(Value::Object(fields)) = note.get("fields") else {
                panic!("fields object")
            };
            assert_eq!(fields.len(), 1);
            assert_eq!(fields.get("Front"), Some(&json!("<new>")));
            let Some(Value::Object(options)) = note.get("options") else {
                panic!("options object")
            };
            assert_eq!(options.len(), 1);
            assert_eq!(options.get(expected_key), Some(&expected_value));
        }
    }

    #[test]
    fn parse_frequency_value_matches_numbers_and_stars() {
        assert_eq!(parse_frequency_value("123.45Hz"), Some(123.45));
        assert_eq!(parse_frequency_value("+0.5"), Some(0.5));
        assert_eq!(parse_frequency_value(".25"), Some(0.25));
        assert_eq!(parse_frequency_value("-.5"), Some(-0.5));
        assert_eq!(parse_frequency_value("12."), Some(12.0));
        assert_eq!(parse_frequency_value("★"), Some(20.0));
        assert_eq!(parse_frequency_value("★★"), Some(40.0));
        assert_eq!(parse_frequency_value("★★★"), Some(60.0));
        assert_eq!(parse_frequency_value("★★★★"), Some(80.0));
        assert_eq!(parse_frequency_value("★★★★★"), Some(100.0));

        assert_eq!(parse_frequency_value("★★★★★★"), None);
        assert_eq!(parse_frequency_value("１２３"), None);
        assert_eq!(parse_frequency_value(""), None);
        assert_eq!(parse_frequency_value("+"), None);
        assert_eq!(parse_frequency_value("."), None);
        assert_eq!(parse_frequency_value("+."), None);
        assert_eq!(parse_frequency_value("abc"), None);
        assert_eq!(parse_frequency_value("★ "), None);
        assert_eq!(parse_frequency_value(" ★"), None);
    }

    #[test]
    fn frequency_stats_aggregates_and_skips_duplicates_and_empty_dicts() {
        let entries = [
            FrequencyEntry {
                dictionary: "",
                freq: "10.5",
            },
            FrequencyEntry {
                dictionary: "DictA",
                freq: "20.0",
            },
        ];
        let (numbers, harmonic, average) = frequency_stats(&entries);
        assert_eq!(numbers, [20.0]);
        assert_eq!(harmonic, Some(20));
        assert_eq!(average, Some(20));

        let entries2 = [
            FrequencyEntry {
                dictionary: "DictA",
                freq: "0.0",
            },
            FrequencyEntry {
                dictionary: "DictA",
                freq: "-5.0",
            },
            FrequencyEntry {
                dictionary: "DictA",
                freq: "10.5",
            },
            FrequencyEntry {
                dictionary: "DictA",
                freq: "20.0",
            },
        ];
        let (numbers2, _, _) = frequency_stats(&entries2);
        assert_eq!(numbers2, [10.5]);

        let entries3 = [
            FrequencyEntry {
                dictionary: "DictA",
                freq: "3",
            },
            FrequencyEntry {
                dictionary: "DictB",
                freq: "5",
            },
        ];
        let (numbers3, harmonic3, average3) = frequency_stats(&entries3);
        assert_eq!(numbers3, [3.0, 5.0]);
        assert_eq!(harmonic3, Some(3));
        assert_eq!(average3, Some(4));

        assert_eq!(frequency_stats(&[]), (vec![], None, None));
    }

    #[test]
    fn test_is_kifuku_applicable() {
        assert!(!is_kifuku_applicable(&[]));
        assert!(is_kifuku_applicable(&["v1"]));
        assert!(is_kifuku_applicable(&["v5"]));
        assert!(is_kifuku_applicable(&["vk"]));
        assert!(is_kifuku_applicable(&["vz"]));
        assert!(is_kifuku_applicable(&["adj-i"]));
        assert!(!is_kifuku_applicable(&["n"]));
        assert!(!is_kifuku_applicable(&["vs"]));
        assert!(!is_kifuku_applicable(&["v1", "vs"]));
        assert!(!is_kifuku_applicable(&["vs", "v1"]));
        assert!(is_kifuku_applicable(&["n", "v5"]));
    }

    #[test]
    fn test_format_furigana() {
        let pairs = [
            FuriganaPair {
                surface: "日",
                reading: "に",
            },
            FuriganaPair {
                surface: "本",
                reading: "ほん",
            },
        ];
        assert_eq!(
            format_furigana(&pairs, false),
            "<ruby>日<rt>に</rt></ruby><ruby>本<rt>ほん</rt></ruby>"
        );
        assert_eq!(format_furigana(&pairs, true), "日[に]本[ほん]");

        let pairs_with_plain = [
            FuriganaPair {
                surface: "日",
                reading: "",
            },
            FuriganaPair {
                surface: "本",
                reading: "ほん",
            },
        ];
        assert_eq!(
            format_furigana(&pairs_with_plain, false),
            "日<ruby>本<rt>ほん</rt></ruby>"
        );
        assert_eq!(format_furigana(&pairs_with_plain, true), "日本[ほん]");

        assert_eq!(format_furigana(&[], false), "");
        assert_eq!(format_furigana(&[], true), "");
    }

    #[test]
    fn test_build_frequencies_html_and_escaping() {
        let entries = [
            FrequencyEntry {
                dictionary: "Dict1",
                freq: "100",
            },
            FrequencyEntry {
                dictionary: "Dict2:",
                freq: "50",
            },
        ];

        assert_eq!(
            build_frequencies_html(&entries, false, false),
            "<ul><li>Dict1: 100</li><li>Dict2: 50</li></ul>"
        );
        assert_eq!(build_frequencies_html(&entries, true, false), "100<br>50<br>");
        assert_eq!(
            build_frequencies_html(&entries, false, true),
            "<ul><li>Dict2: 50</li></ul>"
        );
        assert_eq!(build_frequencies_html(&entries, true, true), "50");
        assert_eq!(build_frequencies_html(&[], false, false), "");

        let entries_invalid = [FrequencyEntry {
            dictionary: "Dict1",
            freq: "invalid",
        }];
        assert_eq!(
            build_frequencies_html(&entries_invalid, false, true),
            "<ul><li></li></ul>"
        );
        assert_eq!(build_frequencies_html(&entries_invalid, true, true), "");

        let escaped_entries = [FrequencyEntry {
            dictionary: "<script>D&1</script>",
            freq: "10<b>x</b>",
        }];
        assert_eq!(
            build_frequencies_html(&escaped_entries, false, false),
            "<ul><li>&lt;script&gt;D&amp;1&lt;/script&gt;: 10&lt;b&gt;x&lt;/b&gt;</li></ul>"
        );
        assert_eq!(
            build_frequencies_html(&escaped_entries, true, true),
            "10&lt;b&gt;x&lt;/b&gt;"
        );
    }

    #[test]
    fn test_format_pitch_and_positions_with_escaping() {
        let mora = ["に", "ほ", "ん"];
        let p_heiban = [PitchEntry {
            dictionary_name: "DictA",
            positions: &[0],
            mora: &mora,
        }];
        assert_eq!(
            format_pitch(&p_heiban),
            "<span class=\"subomi-pitch\">に<span style=\"border-top: solid;\">ほん</span></span>"
        );
        assert_eq!(
            format_pitch_positions(&p_heiban),
            "<span class=\"subomi-pitch-position\">[0]</span>"
        );

        let p_atama = [PitchEntry {
            dictionary_name: "DictA",
            positions: &[1],
            mora: &mora,
        }];
        assert_eq!(
            format_pitch(&p_atama),
            "<span class=\"subomi-pitch\"><span style=\"border-top: solid; border-right: solid;\">に</span>ほん</span>"
        );

        let p_naka = [PitchEntry {
            dictionary_name: "DictA",
            positions: &[2],
            mora: &mora,
        }];
        assert_eq!(
            format_pitch(&p_naka),
            "<span class=\"subomi-pitch\">に<span style=\"border-top: solid; border-right: solid;\">ほ</span>ん</span>"
        );

        let p_multi_dicts = [
            PitchEntry {
                dictionary_name: "DictA",
                positions: &[0],
                mora: &mora,
            },
            PitchEntry {
                dictionary_name: "Dict<B>",
                positions: &[1],
                mora: &mora,
            },
        ];
        assert!(format_pitch(&p_multi_dicts).contains("<i>Dict&lt;B&gt;</i>"));
        assert!(format_pitch_positions(&p_multi_dicts).contains("<i>Dict&lt;B&gt;</i>"));
        assert_eq!(format_pitch(&[]), "");
        assert_eq!(format_pitch_positions(&[]), "");
    }

    #[test]
    fn test_format_tags_and_escaping() {
        let tags = [
            TagEntry {
                name: "JLPT <N5>",
                notes: "common & \"basic\"",
            },
            TagEntry {
                name: "archaic",
                notes: "",
            },
        ];

        assert_eq!(
            format_tags(&tags, false),
            "<ul><li>JLPT &lt;N5&gt;: common &amp; &quot;basic&quot;</li><li>archaic</li></ul>"
        );
        assert_eq!(
            format_tags(&tags, true),
            "<ul><li>JLPT &lt;N5&gt;</li><li>archaic</li></ul>"
        );
        assert_eq!(format_tags(&[], false), "");
    }

    #[test]
    fn test_format_term_glossary_escaping_and_variant_skipping() {
        assert_eq!(
            format_term_glossary_selected(&[], TermGlossaryVariants::ALL),
            TermGlossaryOutput::default()
        );

        let defs = [TermGlossaryDef {
            dict_id: 1,
            dict_name: "JM\"dict<&>",
            tags: &["<v1>"],
            items: &["to eat", "to consume"],
            has_styles: true,
            stylesheet: ".x { color: red; }</STYLE><script>alert(1)</script>",
        }];

        let out = format_term_glossary_selected(&defs, TermGlossaryVariants::ALL);
        assert!(out
            .glossary
            .contains("<li data-dictionary=\"JM&quot;dict&lt;&amp;&gt;\">"));
        assert!(out
            .glossary
            .contains("<i>(&lt;v1&gt;, JM&quot;dict&lt;&amp;&gt;)</i>"));
        assert!(!out.glossary.to_ascii_lowercase().contains("</style><script>"));
        assert!(out.glossary.contains("<\\/STYLE>"));

        let brief_only = format_term_glossary_selected(&defs, TermGlossaryVariants::BRIEF);
        assert!(brief_only.glossary.is_empty());
        assert!(!brief_only.glossary_brief.is_empty());
        assert!(brief_only.glossary_compact.is_empty());
        assert_eq!(brief_only.glossary_brief, out.glossary_brief);
    }

    #[test]
    fn media_marker_arguments_reject_non_finite_and_trim_unicode_whitespace() {
        let nan_db = parse_marker("{audio-media:db=NaN}");
        assert_eq!(audio_media_params(&nan_db, false, -20.0), None);
        let inf_db = parse_marker("{video:db=inf}");
        assert_eq!(video_params(&inf_db, false, -20.0), None);
        let neg_inf_db = parse_marker("{audio-media:db=-infinity}");
        assert_eq!(audio_media_params(&neg_inf_db, false, -20.0), None);

        let mut nel_db = Marker::default();
        nel_db.insert_arg("db".to_owned(), "\u{0085}-12.5\u{0085}".to_owned());
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
        let mora3 = ["に", "ほ", "ん"];
        let mora4 = ["に", "ほ", "ん", "ご"];
        let mora2 = ["に", "ほ"];

        let e_lhh = [PitchEntryEx {
            dictionary_name: "NHK",
            specs: &[PitchSpec::Pattern("LHH")],
            mora: &mora3,
        }];
        let e_lhhh = [PitchEntryEx {
            dictionary_name: "NHK",
            specs: &[PitchSpec::Pattern("LHHH")],
            mora: &mora3,
        }];
        let e_lhhl_odaka = [PitchEntryEx {
            dictionary_name: "NHK",
            specs: &[PitchSpec::Pattern("LHHL")],
            mora: &mora3,
        }];
        let e_lhhl_naka = [PitchEntryEx {
            dictionary_name: "NHK",
            specs: &[PitchSpec::Pattern("LHHL")],
            mora: &mora4,
        }];
        let e_hll = [PitchEntryEx {
            dictionary_name: "NHK",
            specs: &[PitchSpec::Pattern("HLL")],
            mora: &mora3,
        }];
        let e_lhl = [PitchEntryEx {
            dictionary_name: "NHK",
            specs: &[PitchSpec::Pattern("LHL")],
            mora: &mora3,
        }];
        let e_lhl_odaka2 = [PitchEntryEx {
            dictionary_name: "NHK",
            specs: &[PitchSpec::Pattern("LHL")],
            mora: &mora2,
        }];

        assert_eq!(
            format_pitch_ex(&e_lhh),
            "<span class=\"subomi-pitch\">に<span style=\"border-top: solid;\">ほん</span></span>"
        );
        assert_eq!(
            format_pitch_ex(&e_lhhh),
            "<span class=\"subomi-pitch\">に<span style=\"border-top: solid;\">ほん</span></span>"
        );
        assert_eq!(
            format_pitch_ex(&e_lhhl_odaka),
            "<span class=\"subomi-pitch\">に<span style=\"border-top: solid; border-right: solid;\">ほん</span></span>"
        );
        assert_eq!(
            format_pitch_ex(&e_lhhl_naka),
            "<span class=\"subomi-pitch\">に<span style=\"border-top: solid; border-right: solid;\">ほん</span>ご</span>"
        );
        assert_eq!(
            format_pitch_ex(&e_hll),
            "<span class=\"subomi-pitch\"><span style=\"border-top: solid; border-right: solid;\">に</span>ほん</span>"
        );
        assert_eq!(
            format_pitch_ex(&e_lhl),
            "<span class=\"subomi-pitch\">に<span style=\"border-top: solid; border-right: solid;\">ほ</span>ん</span>"
        );

        assert_eq!(
            format_pitch_positions_ex(&e_lhh),
            "<span class=\"subomi-pitch-position\">[LHH]</span>"
        );
        assert_eq!(
            format_pitch_positions_ex(&e_lhhl_odaka),
            "<span class=\"subomi-pitch-position\">[LHHL]</span>"
        );
        assert_eq!(
            format_pitch_positions_ex(&e_hll),
            "<span class=\"subomi-pitch-position\">[HLL]</span>"
        );
        assert_eq!(
            format_pitch_positions_ex(&e_lhl),
            "<span class=\"subomi-pitch-position\">[LHL]</span>"
        );

        assert_eq!(format_pitch_categories_ex(&e_lhh, false), "heiban");
        assert_eq!(format_pitch_categories_ex(&e_lhhh, false), "heiban");
        assert_eq!(format_pitch_categories_ex(&e_lhhl_odaka, false), "odaka");
        assert_eq!(format_pitch_categories_ex(&e_lhhl_odaka, true), "odaka");
        assert_eq!(format_pitch_categories_ex(&e_lhl_odaka2, false), "odaka");
        assert_eq!(format_pitch_categories_ex(&e_lhhl_naka, false), "nakadaka");
        assert_eq!(format_pitch_categories_ex(&e_lhhl_naka, true), "kifuku");
        assert_eq!(format_pitch_categories_ex(&e_hll, false), "atamadaka");
        assert_eq!(format_pitch_categories_ex(&e_hll, true), "kifuku");
        assert_eq!(format_pitch_categories_ex(&e_lhl, false), "nakadaka");
        assert_eq!(format_pitch_categories_ex(&e_lhl, true), "kifuku");

        let svg_lhh =
            generate_pitch_graph_svg(3, PitchSpec::Pattern("LHH"), "#fff", "#000").unwrap();
        let svg_d0 =
            generate_pitch_graph_svg(3, PitchSpec::Downstep(0), "#fff", "#000").unwrap();
        assert_eq!(svg_lhh, svg_d0);
        assert!(svg_lhh.contains("viewBox=\"0 0 200 100\""));
        assert!(svg_lhh.contains("<use href=\"#black-dot\" x=\"125\" y=\"25\"/>"));
        assert!(svg_lhh.contains("<use href=\"#white-dot\" x=\"175\" y=\"25\"/>"));

        let svg_lhhl =
            generate_pitch_graph_svg(3, PitchSpec::Pattern("LHHL"), "#fff", "#000").unwrap();
        let svg_d3 =
            generate_pitch_graph_svg(3, PitchSpec::Downstep(3), "#fff", "#000").unwrap();
        assert_eq!(svg_lhhl, svg_d3);
        assert!(svg_lhhl.contains("<use href=\"#change-dot\" x=\"125\" y=\"25\"/>"));
        assert!(svg_lhhl.contains("<use href=\"#white-dot\" x=\"175\" y=\"75\"/>"));

        assert_eq!(
            generate_pitch_graph_svg(3, PitchSpec::Pattern("HLL"), "#fff", "#000").unwrap(),
            generate_pitch_graph_svg(3, PitchSpec::Downstep(1), "#fff", "#000").unwrap()
        );
        assert_eq!(
            generate_pitch_graph_svg(3, PitchSpec::Pattern("LHL"), "#fff", "#000").unwrap(),
            generate_pitch_graph_svg(3, PitchSpec::Downstep(2), "#fff", "#000").unwrap()
        );

        let e_multi = [PitchEntryEx {
            dictionary_name: "NHK",
            specs: &[
                PitchSpec::Pattern("HHH"),
                PitchSpec::Pattern("LLL"),
                PitchSpec::Pattern("HLHL"),
                PitchSpec::Pattern("lhh"),
                PitchSpec::Pattern("INVALID"),
            ],
            mora: &mora4,
        }];
        let multi_html = format_pitch_ex(&e_multi);
        assert!(multi_html.contains("<span style=\"border-top: solid;\">にほんご</span>"));
        assert!(multi_html.contains("<li style=\"padding: 2px 0px 2px 0px;\">にほんご</li>"));
        assert!(multi_html.contains(
            "<span style=\"border-top: solid; border-right: solid;\">に</span>ほ<span style=\"border-top: solid; border-right: solid;\">ん</span>ご"
        ));
        let multi_pos = format_pitch_positions_ex(&e_multi);
        assert!(multi_pos.contains("<li>[HHH]</li>"));
        assert!(multi_pos.contains("<li>[LLL]</li>"));
        assert!(multi_pos.contains("<li>[HLHL]</li>"));
        assert!(multi_pos.contains("<li>[LHH]</li>"));
        assert!(!multi_pos.contains("INVALID"));

        let graphs_html = format_pitch_graphs_ex(&e_multi);
        assert!(graphs_html.starts_with("<span class=\"subomi-pitch-graph\"><ol>"));
        assert_eq!(graphs_html.matches("<svg ").count(), 4);
        assert_eq!(classify_pitch_spec(3, PitchSpec::Pattern(""), false), None);
        assert_eq!(
            classify_pitch_spec(3, PitchSpec::Pattern("XY"), false),
            None
        );
    }

    #[test]
    fn format_selection_handles_multi_definitions_and_prevents_double_br() {
        assert_eq!(format_selection(&[]), "");
        assert_eq!(format_selection(&["", "   ", "\r\n\t\n"]), "");
        assert_eq!(format_selection(&["  only one  "]), "only one");
        assert_eq!(format_selection(&["", "  only one  ", "\r\n"]), "only one");

        let single_multiline =
            format_selection(&["\r\n\nline 1\r\n\r\n  \nline 2\rline 3\n\n"]);
        assert_eq!(single_multiline, "line 1<br>line 2<br>line 3");
        assert!(!single_multiline.contains("<br><br>"));
        assert!(!single_multiline.contains('\r'));
        assert!(!single_multiline.contains('\n'));

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
    }

    #[test]
    fn regression_html_entity_escaping_across_glossary_cloze_pitch_and_dicts() {
        use crate::glossary::{build_glossary, escape_html_str};

        let payload = "<script>\"a&b\"</script>";
        let expected_escaped = "&lt;script&gt;&quot;a&amp;b&quot;&lt;/script&gt;";

        assert_eq!(escape_html_str(payload), expected_escaped);

        let defs = [
            json!(payload),
            json!({ "type": "text", "text": payload }),
            json!({
                "type": "structured-content",
                "content": { "tag": "span", "title": payload, "content": payload }
            }),
        ];
        let (built, _) = build_glossary(&defs, "/base", |_| None);
        assert_eq!(built[0], expected_escaped);
        assert_eq!(built[1], expected_escaped);
        assert_eq!(
            built[2],
            format!("<span title=\"{expected_escaped}\">{expected_escaped}</span>")
        );

        let kanji_html = format_kanji_glossary(&[KanjiDefEntry {
            dictionary_name: payload,
            tags: &["tag<1>&\"2\""],
            glossary: &[payload],
        }]);
        assert!(!kanji_html.contains("<script>"));
        assert!(kanji_html.contains(expected_escaped));
        assert!(kanji_html.contains("tag&lt;1&gt;&amp;&quot;2&quot;"));

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

        let evil_mora = ["<に>", "ほ&\"ん\""];
        let legacy_pitches = [
            PitchEntry {
                dictionary_name: "Dict<A>&\"1\"",
                positions: &[0],
                mora: &evil_mora,
            },
            PitchEntry {
                dictionary_name: "Dict<B>",
                positions: &[0],
                mora: &evil_mora,
            },
        ];
        let legacy_html = format_pitch(&legacy_pitches);
        assert!(legacy_html.contains("<i>Dict&lt;A&gt;&amp;&quot;1&quot;</i>"));
        assert!(legacy_html.contains("&lt;に&gt;"));
        assert!(legacy_html.contains("ほ&amp;&quot;ん&quot;"));
        assert!(!legacy_html.contains("<に>"));

        let ex_pitches = [
            PitchEntryEx {
                dictionary_name: "Dict<A>&\"1\"",
                specs: &[PitchSpec::Pattern("LH")],
                mora: &evil_mora,
            },
            PitchEntryEx {
                dictionary_name: "Dict<B>",
                specs: &[PitchSpec::Pattern("LH")],
                mora: &evil_mora,
            },
        ];
        let ex_html = format_pitch_ex(&ex_pitches);
        assert!(ex_html.contains("<i>Dict&lt;A&gt;&amp;&quot;1&quot;</i>"));
        assert!(ex_html.contains("&lt;に&gt;"));
        assert!(ex_html.contains("ほ&amp;&quot;ん&quot;"));
        assert!(format_pitch_positions_ex(&ex_pitches)
            .contains("<i>Dict&lt;A&gt;&amp;&quot;1&quot;</i>"));
        assert!(format_pitch_graphs_ex(&ex_pitches)
            .contains("<i>Dict&lt;A&gt;&amp;&quot;1&quot;</i>"));
    }

    #[test]
    fn regression_deduplicated_compact_style_blocks_per_dict_id() {
        let defs = [
            TermGlossaryDef {
                dict_id: 1,
                dict_name: "JMdict",
                tags: &[],
                items: &["sense 1"],
                has_styles: true,
                stylesheet: "   \r\n\t  ",
            },
            TermGlossaryDef {
                dict_id: 1,
                dict_name: "JMdict",
                tags: &[],
                items: &["sense 2"],
                has_styles: true,
                stylesheet: ".d1-tag{color:#59b2ff;}",
            },
            TermGlossaryDef {
                dict_id: 1,
                dict_name: "JMdict",
                tags: &[],
                items: &["sense 3"],
                has_styles: true,
                stylesheet: ".d1-tag{color:#59b2ff;}",
            },
            TermGlossaryDef {
                dict_id: 2,
                dict_name: "大辞林",
                tags: &[],
                items: &["語釈 1"],
                has_styles: true,
                stylesheet: ".d2-note{font-weight:bold;}</style><img src=x>",
            },
            TermGlossaryDef {
                dict_id: 2,
                dict_name: "大辞林",
                tags: &[],
                items: &["語釈 2"],
                has_styles: true,
                stylesheet: ".d2-note{font-weight:bold;}</style><img src=x>",
            },
        ];

        let out = format_term_glossary_selected(&defs, TermGlossaryVariants::ALL);
        for (label, html) in [
            ("glossary", &out.glossary),
            ("glossary_brief", &out.glossary_brief),
            ("glossary_compact", &out.glossary_compact),
        ] {
            assert_eq!(
                html.matches("<style>").count(),
                3,
                "{label} should have deduplicated <style> blocks: {html}"
            );
            assert_eq!(html.matches(".d1-tag{color:#59b2ff;}").count(), 1);
            assert_eq!(html.matches(".d2-note{font-weight:bold;}").count(), 1);
            assert!(!html.contains("<style></style>"));
            assert!(!html.contains("</style><img"));
            assert!(html.contains("<\\/style><img"));
        }
    }
}

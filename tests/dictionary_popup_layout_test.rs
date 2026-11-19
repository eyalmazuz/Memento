use i_slint_backend_testing::{init_no_event_loop, ElementQuery};
use memento_app::{DictResult, MementoApp, PopupGlossaryBlock};
use slint::{ComponentHandle, Model, VecModel};
use std::cell::Cell;
use std::rc::Rc;

#[path = "support/compact_popup.rs"]
mod compact_popup;

#[test]
fn compact_header_keeps_all_frequencies_and_wraps_at_small_widths() {
    init_no_event_loop();
    let app = MementoApp::new().unwrap();
    memento_app::install_popup_navigation(&app);
    let rows = compact_popup::definitions();
    let frequencies = rows[0].frequencies.clone();
    let normal = app
        .global::<memento_app::PopupLayout>()
        .invoke_frequency_lines(frequencies.clone(), 442.);
    assert_eq!(
        normal.row_count(),
        2,
        "seven ranks should occupy two compact rows"
    );
    let narrow = app
        .global::<memento_app::PopupLayout>()
        .invoke_frequency_lines(frequencies.clone(), 240.);
    assert!(narrow.row_count() > normal.row_count());
    let flattened: Vec<_> = narrow
        .iter()
        .flat_map(|line| line.entries.iter().collect::<Vec<_>>())
        .collect();
    assert_eq!(
        flattened,
        frequencies.iter().collect::<Vec<_>>(),
        "wrapping must retain every source and rank in order"
    );
    memento_app::set_dictionary_rows(&app, rows);
    app.set_anki_enabled(true);
    app.set_dict_open(true);
    app.window().set_size(slint::LogicalSize::new(1280., 720.));
    app.show().unwrap();
    if std::env::var("SLINT_EMIT_DEBUG_INFO").as_deref() == Ok("1") {
        let first = ElementQuery::from_root(&app)
            .match_id("YomitanPopup::term-segment")
            .find_first()
            .unwrap();
        assert!(
            first.size().height < 285.,
            "header crowds out the definitions: {:?}",
            first.size()
        );
    }
}

fn definition(
    expression: &str,
    reading: &str,
    glossary: &str,
    start: bool,
    end: bool,
) -> DictResult {
    DictResult {
        expression: expression.into(),
        reading: reading.into(),
        glossary: glossary.into(),
        glossary_blocks: Rc::new(VecModel::from(vec![PopupGlossaryBlock {
            text: glossary.into(),
            kind: 0,
            href: "".into(),
        }]))
        .into(),
        term_start: start,
        term_end: end,
        selected: true,
        ..Default::default()
    }
}

#[test]
fn long_dictionary_entries_keep_their_full_height_and_collapse_without_blank_space() {
    // ElementQuery needs Slint's generated IDs. Run with
    // `SLINT_EMIT_DEBUG_INFO=1 cargo test --test dictionary_popup_layout_test`.
    if std::env::var("SLINT_EMIT_DEBUG_INFO").as_deref() != Ok("1") {
        return;
    }
    init_no_event_loop();
    let app = MementoApp::new().unwrap();
    app.window().set_size(slint::LogicalSize::new(1280., 720.));
    let long_glossary = "A long definition with enough text to wrap in the popup. ".repeat(28);
    let mut definitions = vec![
        definition("迎えに行く", "むかえにいく", &long_glossary, true, false),
        definition(
            "迎えに行く",
            "むかえにいく",
            "another dictionary",
            false,
            true,
        ),
        definition("迎", "むかえ", "a distinct term", true, true),
    ];
    for (index, row) in definitions.iter_mut().enumerate() {
        row.index = (index + 1) as i32;
    }
    let rows = Rc::new(VecModel::from(definitions));
    app.set_dict_results(rows.clone().into());
    app.set_popup_results(rows.clone().into());
    app.set_popup_loaded_count(3);
    app.set_dict_open(true);
    let clicked = Rc::new(Cell::new(-1));
    let clicked_from_callback = clicked.clone();
    let rows_from_callback = rows.clone();
    app.on_toggle_term_collapse(move |index| {
        clicked_from_callback.set(index);
        for row_index in 0..2 {
            let mut row = rows_from_callback.row_data(row_index).unwrap();
            row.collapsed = true;
            rows_from_callback.set_row_data(row_index, row);
        }
    });
    app.show().unwrap();

    let query = ElementQuery::from_root(&app);
    let segments = query.match_id("YomitanPopup::term-segment").find_all();
    assert!(!segments.is_empty());
    assert!(
        segments[0].size().height > 440.,
        "long definition was clipped"
    );
    let content_before = ElementQuery::from_root(&app)
        .match_id("YomitanPopup::definition-content")
        .find_first()
        .unwrap()
        .size()
        .height;

    let collapse_button = ElementQuery::from_root(&app)
        .match_id("YomitanPopup::collapse-touch")
        .find_first()
        .unwrap();
    collapse_button.mock_single_click(slint::platform::PointerEventButton::Left);
    assert_eq!(
        clicked.get(),
        0,
        "collapse button did not trigger the first term"
    );
    let query = ElementQuery::from_root(&app);
    let segments = query.match_id("YomitanPopup::term-segment").find_all();
    let content_after = ElementQuery::from_root(&app)
        .match_id("YomitanPopup::definition-content")
        .find_first()
        .unwrap()
        .size()
        .height;
    assert!(!segments.is_empty());
    assert!(
        content_after < content_before - 300.,
        "collapse left a large blank gap"
    );
    assert!(
        segments[0].size().height < 100.,
        "collapsed heading did not shrink"
    );
}

#[test]
fn scrolling_the_first_term_requests_the_next_group() {
    if std::env::var("SLINT_EMIT_DEBUG_INFO").as_deref() != Ok("1") {
        return;
    }
    init_no_event_loop();
    let app = MementoApp::new().unwrap();
    app.window().set_size(slint::LogicalSize::new(1280., 720.));
    let long_glossary = "A definition long enough to scroll in the popup. ".repeat(28);
    let mut first = definition("世界中", "せかいじゅう", &long_glossary, true, true);
    first.index = 1;
    let mut second = definition("世界", "せかい", "the world", true, true);
    second.index = 2;
    app.set_dict_results(Rc::new(VecModel::from(vec![first.clone(), second])).into());
    app.set_popup_results(Rc::new(VecModel::from(vec![first])).into());
    app.set_popup_loaded_count(1);
    app.set_dict_open(true);
    let requests = Rc::new(Cell::new(0));
    let requests_callback = requests.clone();
    app.on_load_more_definition_results(move |_| {
        requests_callback.set(requests_callback.get() + 1)
    });
    app.show().unwrap();

    let list = ElementQuery::from_root(&app)
        .match_id("YomitanPopup::definition-list")
        .find_first()
        .unwrap();
    list.scroll(0., -600.);
    assert!(
        requests.get() > 0,
        "scrolling near the end did not request more results"
    );
}

#[test]
fn short_initial_page_requests_more_definitions_without_scrolling() {
    if std::env::var("SLINT_EMIT_DEBUG_INFO").as_deref() != Ok("1") {
        return;
    }
    init_no_event_loop();
    let app = MementoApp::new().unwrap();
    app.window().set_size(slint::LogicalSize::new(1280., 720.));
    let mut definitions = (0..8)
        .map(|index| {
            let mut row = definition(
                "世界中",
                "せかいじゅう",
                "the whole world",
                index == 0,
                index == 7,
            );
            row.index = index + 1;
            row
        })
        .collect::<Vec<_>>();
    let full: slint::ModelRc<DictResult> = Rc::new(VecModel::from(definitions.clone())).into();
    let count = memento_app::next_popup_batch_end(&full, 0);
    assert!(
        count < definitions.len(),
        "initial page laid out every dictionary"
    );
    definitions.truncate(count);
    app.set_dict_results(full.clone());
    app.set_popup_results(Rc::new(VecModel::from(definitions)).into());
    app.set_popup_loaded_count(count as i32);
    let requests = Rc::new(Cell::new(0));
    let requests_callback = requests.clone();
    app.on_load_more_definition_results(move |_| {
        requests_callback.set(requests_callback.get() + 1)
    });
    app.set_dict_open(true);
    app.show().unwrap();
    // Force the same geometry evaluation that painting the scroll view needs.
    let list = ElementQuery::from_root(&app)
        .match_id("YomitanPopup::definition-list")
        .find_first()
        .unwrap();
    assert!(list.size().height > 0.);
    slint::platform::update_timers_and_animations();
    assert!(
        requests.get() > 0,
        "short initial page left empty space without loading more"
    );
    assert_eq!(
        app.get_dict_results().row_count(),
        8,
        "full Anki result model was truncated"
    );
}

#[test]
fn paging_and_collapse_preserve_every_dictionary_and_its_selection() {
    init_no_event_loop();
    let app = MementoApp::new().unwrap();
    memento_app::install_popup_navigation(&app);
    let rows: Vec<_> = (0..12)
        .map(|index| {
            let same_term = index < 10;
            let mut row = definition(
                if same_term { "世界中" } else { "世界" },
                if same_term {
                    "せかいじゅう"
                } else {
                    "せかい"
                },
                "definition",
                index == 0 || index == 10,
                index == 9 || index == 11,
            );
            row.index = index + 1;
            row
        })
        .collect();
    memento_app::set_dictionary_rows(&app, rows);
    assert_eq!(app.get_popup_results().row_count(), 12);
    app.invoke_toggle_definition_selection(1);
    app.invoke_toggle_term_collapse(0);
    assert_eq!(app.get_popup_results().row_count(), 3);
    app.invoke_toggle_term_collapse(0);
    let full = app.get_dict_results();
    let visible = app.get_popup_results();
    assert_eq!(full.row_count(), 12);
    assert_eq!(visible.row_count(), 12);
    assert!(!visible.row_data(1).unwrap().selected);
    for index in 0..12 {
        assert_eq!(visible.row_data(index).unwrap().index, index as i32 + 1);
    }
}

mod rich_popup_phase4_tests {
    use super::*;
    use memento_app::controller::config::ConfigController;
    use memento_app::controller::dict::{
        resolve_dictionary_style_path, DictController, DictTag, TermPitchDetail,
    };
    use memento_app::{
        install_popup_navigation, install_rich_ui_bridges, load_dictionary_image_pixmap,
        parse_dictionary_css_str, render_structured_glossary_entry, rich_pitch_graphs_from_details,
        set_dictionary_rows, suppress_redundant_tags, tag_category_hex_color, GlossaryZoneAction,
        PopupAudioSource, PopupBridge,
    };
    use memento_definition::LegacyGlossaryStyle;
    use memento_dict::yomidbbuilder;
    use std::collections::HashMap;
    use std::fs;
    use std::io::Write;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    static FIXTURE_SEQ: AtomicUsize = AtomicUsize::new(1);

    struct TempRichFixture {
        root: PathBuf,
        config_dir: PathBuf,
        db_path: PathBuf,
        dic_id: i64,
    }

    impl Drop for TempRichFixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn crc32(data: &[u8]) -> u32 {
        let mut crc = 0xffff_ffffu32;
        for &byte in data {
            crc ^= u32::from(byte);
            for _ in 0..8 {
                let mask = (crc & 1).wrapping_neg();
                crc = (crc >> 1) ^ (0xedb8_8320 & mask);
            }
        }
        !crc
    }

    fn write_yomi_zip(path: &Path, entries: &[(&str, &[u8])]) {
        let mut buf = Vec::new();
        let mut central = Vec::new();
        for &(name, data) in entries {
            let offset = buf.len() as u32;
            let crc = crc32(data);
            let size = data.len() as u32;
            let name_bytes = name.as_bytes();
            let name_len = name_bytes.len() as u16;

            buf.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
            buf.extend_from_slice(&20u16.to_le_bytes());
            buf.extend_from_slice(&0u16.to_le_bytes());
            buf.extend_from_slice(&0u16.to_le_bytes());
            buf.extend_from_slice(&0u16.to_le_bytes());
            buf.extend_from_slice(&0u16.to_le_bytes());
            buf.extend_from_slice(&crc.to_le_bytes());
            buf.extend_from_slice(&size.to_le_bytes());
            buf.extend_from_slice(&size.to_le_bytes());
            buf.extend_from_slice(&name_len.to_le_bytes());
            buf.extend_from_slice(&0u16.to_le_bytes());
            buf.extend_from_slice(name_bytes);
            buf.extend_from_slice(data);

            central.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
            central.extend_from_slice(&20u16.to_le_bytes());
            central.extend_from_slice(&20u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&crc.to_le_bytes());
            central.extend_from_slice(&size.to_le_bytes());
            central.extend_from_slice(&size.to_le_bytes());
            central.extend_from_slice(&name_len.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u32.to_le_bytes());
            central.extend_from_slice(&offset.to_le_bytes());
            central.extend_from_slice(name_bytes);
        }
        let cd_offset = buf.len() as u32;
        let cd_size = central.len() as u32;
        let count = entries.len() as u16;
        buf.extend_from_slice(&central);
        buf.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
        buf.extend_from_slice(&0u16.to_le_bytes());
        buf.extend_from_slice(&0u16.to_le_bytes());
        buf.extend_from_slice(&count.to_le_bytes());
        buf.extend_from_slice(&count.to_le_bytes());
        buf.extend_from_slice(&cd_size.to_le_bytes());
        buf.extend_from_slice(&cd_offset.to_le_bytes());
        buf.extend_from_slice(&0u16.to_le_bytes());

        let mut file = fs::File::create(path).unwrap();
        file.write_all(&buf).unwrap();
    }

    fn build_rich_fixture() -> TempRichFixture {
        let seq = FIXTURE_SEQ.fetch_add(1, Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "memento-p4-rich-popup-{}-{seq}-{nanos}",
            std::process::id()
        ));
        let config_dir = root.join("config");
        let dict_resources = config_dir.join("dict_resources");
        fs::create_dir_all(&dict_resources).unwrap();
        let db_path = config_dir.join("dictionaries.sqlite");

        let styles_css = br#"
            span[data-sc-badge="jitendex"] {
                background-color: #2563EB;
                color: #FFFFFF;
                font-weight: bold;
            }
            span[data-sc-note]::before { content: "\3010"; }
            span[data-sc-note]::after { content: "\3011"; }
            td[data-sc-col="key"] { color: #FDE047; font-weight: bold; }
        "#;

        let mono_svg = br##"<svg xmlns="http://www.w3.org/2000/svg" width="20" height="20" viewBox="0 0 20 20"><circle cx="10" cy="10" r="8" fill="#000000"/></svg>"##;

        let structured_sekaijuu = serde_json::json!({
            "type": "structured-content",
            "content": [
                {
                    "tag": "div",
                    "content": [
                        {
                            "tag": "span",
                            "data": { "class": "tag", "content": "part-of-speech-info" },
                            "title": "noun (common)",
                            "content": "n"
                        },
                        " ",
                        {
                            "tag": "span",
                            "data": { "badge": "jitendex" },
                            "content": "CORE"
                        },
                        " throughout the world; ",
                        {
                            "tag": "span",
                            "data": { "note": "yes" },
                            "title": "Usage note tooltip",
                            "content": "詳解"
                        },
                        " ",
                        {
                            "tag": "img",
                            "path": "assets/mono.svg",
                            "width": 18,
                            "height": 18,
                            "sizeUnits": "px",
                            "appearance": "monochrome",
                            "title": "Monochrome SVG tooltip"
                        }
                    ]
                },
                {
                    "tag": "div",
                    "content": [
                        "Jitendex rp-less ruby: ",
                        {
                            "tag": "ruby",
                            "content": [
                                "日本",
                                { "tag": "rt", "content": "にほん" },
                                "語",
                                { "tag": "rt", "content": "ご" }
                            ]
                        },
                        " and standard ruby: ",
                        {
                            "tag": "ruby",
                            "content": [
                                "世界",
                                { "tag": "rp", "content": "(" },
                                { "tag": "rt", "content": "せかい" },
                                { "tag": "rp", "content": ")" }
                            ]
                        }
                    ]
                },
                {
                    "tag": "table",
                    "content": {
                        "tag": "tbody",
                        "content": [
                            {
                                "tag": "tr",
                                "content": [
                                    { "tag": "th", "colSpan": 2, "content": "Inflection Grid" }
                                ]
                            },
                            {
                                "tag": "tr",
                                "content": [
                                    { "tag": "td", "rowSpan": 2, "data": { "col": "key" }, "content": "Pattern" },
                                    { "tag": "td", "content": "世界中を" }
                                ]
                            },
                            {
                                "tag": "tr",
                                "content": [
                                    { "tag": "td", "content": "世界中で" }
                                ]
                            }
                        ]
                    }
                },
                {
                    "tag": "details",
                    "content": [
                        { "tag": "summary", "content": "More examples" },
                        { "tag": "div", "content": "HiddenClosedExampleBody" }
                    ]
                },
                {
                    "tag": "details",
                    "open": true,
                    "content": [
                        { "tag": "summary", "content": "Etymology notes" },
                        { "tag": "div", "content": "InitiallyOpenEtymologyBody" }
                    ]
                },
                {
                    "tag": "div",
                    "content": [
                        "See also: ",
                        {
                            "tag": "a",
                            "href": "?query=世界&primaryReading=せかい",
                            "content": "世界リンク"
                        }
                    ]
                }
            ]
        });

        let term_bank = serde_json::json!([
            ["世界中", "せかいじゅう", "n adv", "", 100, [structured_sekaijuu], 1, "pop"],
            ["世界", "せかい", "n", "", 95, ["the world; society; universe"], 2, "pop"],
            ["食べる", "たべる", "v1 vt", "v1", 90, ["to eat; to consume"], 3, "ichi1"],
            ["橋", "はし", "n", "", 80, ["bridge"], 4, ""],
            ["箸", "はし", "n", "", 75, ["chopsticks"], 5, ""]
        ]);

        let tag_bank = serde_json::json!([
            ["pop", "popular", 1, "popular term", 50],
            ["ichi1", "frequent", 2, "Ichiman 1", 40],
            ["n", "partOfSpeech", 3, "noun (common)", 10],
            ["adv", "partOfSpeech", 4, "adverb", 10],
            ["v1", "partOfSpeech", 5, "Ichidan verb", 10],
            ["vt", "partOfSpeech", 6, "transitive verb", 10],
            ["strokes", "misc", 1, "Stroke count", 0],
            ["grade", "misc", 2, "Grade", 0],
            ["ucs", "code", 3, "Unicode", 0],
            ["heisig", "index", 4, "Heisig RTK", 0],
            ["radical", "class", 5, "Classical radical", 0]
        ]);

        let kanji_bank = serde_json::json!([
            ["世", "セイ セ", "よ", "pop", ["generation", "world"], {"strokes": "5", "grade": "3", "ucs": "4E16", "heisig": "109"}],
            ["界", "カイ", "さかい", "pop", ["world", "boundary"], {"strokes": "9", "grade": "3", "ucs": "754C", "heisig": "256", "radical": "102"}]
        ]);

        let term_meta_bank = serde_json::json!([
            [
                "食べる",
                "pitch",
                {
                    "reading": "たべる",
                    "pitches": [
                        { "position": 2, "devoice": [1, 3], "nasal": [2], "tags": ["標準"] }
                    ]
                }
            ]
        ]);

        let kanji_meta_bank = serde_json::json!([["界", "freq", "412"]]);

        let zip_path = root.join("jitendex_rich.zip");
        write_yomi_zip(
            &zip_path,
            &[
                (
                    "index.json",
                    br#"{"title":"JitendexRich","format":3,"revision":"2026.11"}"#,
                ),
                ("styles.css", styles_css),
                ("assets/mono.svg", mono_svg),
                ("term_bank_1.json", term_bank.to_string().as_bytes()),
                ("tag_bank_1.json", tag_bank.to_string().as_bytes()),
                ("kanji_bank_1.json", kanji_bank.to_string().as_bytes()),
                (
                    "term_meta_bank_1.json",
                    term_meta_bank.to_string().as_bytes(),
                ),
                (
                    "kanji_meta_bank_1.json",
                    kanji_meta_bank.to_string().as_bytes(),
                ),
            ],
        );

        let dic_id =
            yomidbbuilder::process_dictionary(&zip_path, &db_path, &dict_resources).unwrap();

        TempRichFixture {
            root,
            config_dir,
            db_path,
            dic_id,
        }
    }

    #[test]
    fn rich_structured_content_renders_styles_tables_ruby_details_links_images_and_selection() {
        init_no_event_loop();
        let fixture = build_rich_fixture();
        let dict_res_dir = fixture.config_dir.join("dict_resources");

        // 1. Verify stylesheet resolution by `dict-{dic_id}` and `{dict_name}` fallback.
        let resolved_by_id =
            resolve_dictionary_style_path(&dict_res_dir, fixture.dic_id, "JitendexRich")
                .expect("dict-{dic_id}/styles.css should resolve");
        assert!(resolved_by_id.ends_with(format!("dict-{}/styles.css", fixture.dic_id)));

        let fallback_dir = dict_res_dir.join("FallbackNamedDict");
        fs::create_dir_all(&fallback_dir).unwrap();
        fs::write(fallback_dir.join("styles.css"), ".x { color: #00FF00; }").unwrap();
        let resolved_by_name = resolve_dictionary_style_path(&dict_res_dir, 99999, "FallbackNamedDict")
            .expect("{dict_name}/styles.css fallback should resolve");
        assert_eq!(resolved_by_name, fallback_dir.join("styles.css"));

        // 2. Verify monochrome SVG rasterization + tinting via `resvg`.
        let svg_path = dict_res_dir
            .join(format!("dict-{}", fixture.dic_id))
            .join("assets/mono.svg");
        let tinted_pm = load_dictionary_image_pixmap(
            &svg_path,
            Some(24.0),
            Some(24.0),
            Some([56, 189, 248, 255]),
            "#38BDF8",
        )
        .expect("monochrome SVG should rasterize");
        assert_eq!((tinted_pm.width(), tinted_pm.height()), (24, 24));
        let center_px = tinted_pm.pixel(12, 12).unwrap();
        assert!(center_px.alpha() > 200, "center of circle must be opaque");
        assert!(
            center_px.blue() > center_px.red(),
            "monochrome SVG must be tinted to #38BDF8: {:?}",
            center_px
        );

        // 3. Wire full `MementoApp` with `install_popup_navigation` + `install_rich_ui_bridges`.
        let config = ConfigController::load(&fixture.config_dir, "memento.conf").unwrap();
        let app = MementoApp::new().unwrap();
        app.window().set_size(slint::LogicalSize::new(1280., 720.));
        install_popup_navigation(&app);
        install_rich_ui_bridges(&app, &fixture.db_path, &fixture.config_dir, &config);

        let mut dict_ctrl = DictController::open(&fixture.db_path).unwrap();
        let hits = dict_ctrl.search("世界中").to_vec();
        assert_eq!(hits.len(), 2);
        let rows = memento_app::dictionary_result_rows(&hits, &[]);
        set_dictionary_rows(&app, rows);
        app.set_dict_query("世界中".into());
        app.set_dict_reading("せかいじゅう".into());
        app.set_dict_open(true);
        app.show().unwrap();

        let pb = app.global::<PopupBridge>();
        assert!(pb.get_rich_enabled());

        // 4. Verify rich structured glossary renders CSS pseudo-elements, tables, Both ruby styles,
        //    closed `<details>`, and initially-open `<details open>`.
        let sheet = parse_dictionary_css_str(&fs::read_to_string(&resolved_by_id).unwrap());
        let mut initial_states = HashMap::new();
        initial_states.insert("0".to_owned(), false);
        initial_states.insert("1".to_owned(), true);
        let doc = render_structured_glossary_entry(
            &hits[0].raw_glossary,
            &hits[0].glossary,
            Some(sheet),
            &initial_states,
            LegacyGlossaryStyle::Bullet,
            &dict_res_dir,
            fixture.dic_id,
            "JitendexRich",
            440.0,
            true,
        )
        .expect("structured glossary should render");

        assert!(doc.width_px >= 436.0);
        assert!(doc.height_px > 120.0);
        assert!(
            doc.plain_text.contains("日本語") && doc.plain_text.contains("Inflection Grid"),
            "rp-less Jitendex ruby and table header should be laid out: {}",
            doc.plain_text
        );
        assert!(
            doc.plain_text.contains("世界中を") && doc.plain_text.contains("世界中で"),
            "colSpan/rowSpan table cells should be rendered: {}",
            doc.plain_text
        );
        assert!(
            !doc.plain_text.contains("HiddenClosedExampleBody"),
            "closed <details> body must be hidden initially"
        );
        assert!(
            doc.plain_text.contains("InitiallyOpenEtymologyBody"),
            "<details open> body must be visible initially"
        );

        // Verify ruby & title & image hover tooltips exist in hit_zones.
        assert!(doc
            .hit_zones
            .iter()
            .any(|z| z.tooltip.as_deref() == Some("日本: にほん")));
        assert!(doc
            .hit_zones
            .iter()
            .any(|z| z.tooltip.as_deref() == Some("語: ご")));
        assert!(doc
            .hit_zones
            .iter()
            .any(|z| z.tooltip.as_deref() == Some("Usage note tooltip")));
        assert!(doc
            .hit_zones
            .iter()
            .any(|z| z.tooltip.as_deref() == Some("Monochrome SVG tooltip")));

        // 5. Exercise `PopupBridge.render_glossary`, `<details>` toggle (both closed -> open and open -> closed),
        //    hover tooltip, mouse drag selection, double-click selection, and Ctrl+C copy!
        let render_initial = pb.invoke_render_glossary(
            1,
            440.0,
            true,
            pb.get_rich_revision(),
            pb.get_selection_revision(),
        );
        assert!(render_initial.has_rich);
        let h_initial = render_initial.height_px;

        // Open closed `<details>` id "0" -> height grows!
        pb.invoke_toggle_glossary_details(1, "0".into());
        let render_opened = pb.invoke_render_glossary(
            1,
            440.0,
            true,
            pb.get_rich_revision(),
            pb.get_selection_revision(),
        );
        assert!(
            render_opened.height_px > h_initial,
            "opening closed <details> must expand height ({} -> {})",
            h_initial,
            render_opened.height_px
        );

        // Close initially-open `<details open>` id "1" on FIRST click -> height shrinks!
        pb.invoke_toggle_glossary_details(1, "1".into());
        let render_closed_second = pb.invoke_render_glossary(
            1,
            440.0,
            true,
            pb.get_rich_revision(),
            pb.get_selection_revision(),
        );
        assert!(
            render_closed_second.height_px < render_opened.height_px,
            "toggling initially-open <details open> must collapse it on first click ({} -> {})",
            render_opened.height_px,
            render_closed_second.height_px
        );

        // Hover over a tooltip hit-zone via `PopupBridge.glossary_pointer_hover`
        let tip_zone = doc
            .hit_zones
            .iter()
            .find(|z| z.tooltip.as_deref() == Some("Usage note tooltip"))
            .unwrap();
        pb.invoke_glossary_pointer_hover(
            1,
            tip_zone.x + 2.0,
            tip_zone.y + 2.0,
            120.0,
            160.0,
        );
        assert!(pb.get_tooltip_visible());
        assert_eq!(pb.get_tooltip_text().as_str(), "Usage note tooltip");

        // Double-click a word inside the glossary to select it & sync `PopupBridge.selected_glossary_text`
        let world_char = doc
            .selectable_chars
            .windows(5)
            .find(|w| w.iter().map(|c| c.ch).collect::<String>() == "world")
            .expect("word 'world' should exist in selectable_chars");
        pb.invoke_glossary_double_clicked(1, world_char[1].x + 1.0, world_char[1].y + 4.0);
        assert_eq!(pb.get_selected_glossary_text().as_str(), "world");

        let render_selected = pb.invoke_render_glossary(
            1,
            440.0,
            true,
            pb.get_rich_revision(),
            pb.get_selection_revision(),
        );
        assert!(
            render_selected.selection_rects.row_count() > 0,
            "glossary selection must highlight selection_rects"
        );

        // Mouse drag selection across a range of characters + Ctrl+C copy
        let drag_start = &world_char[0];
        let drag_end = &world_char[4];
        pb.invoke_glossary_pointer_down(1, drag_start.x + 0.5, drag_start.y + 4.0);
        pb.invoke_glossary_pointer_drag(1, drag_end.x + drag_end.w + 0.5, drag_end.y + 4.0);
        pb.invoke_glossary_pointer_up(1, drag_end.x + drag_end.w + 0.5, drag_end.y + 4.0);
        assert_eq!(pb.get_selected_glossary_text().as_str(), "world");
        pb.invoke_copy_selected_glossary();
        assert_eq!(pb.get_last_copied_glossary_text().as_str(), "world");

        // 6. Click internal `?query=世界` link -> navigates in-popup with Back/Forward history!
        //    Re-render clean doc state to locate link zone in current cached doc:
        pb.invoke_toggle_glossary_details(1, "0".into());
        pb.invoke_toggle_glossary_details(1, "1".into());
        let _ = pb.invoke_render_glossary(
            1,
            440.0,
            true,
            pb.get_rich_revision(),
            pb.get_selection_revision(),
        );
        let link_zone = doc
            .hit_zones
            .iter()
            .find(|z| {
                matches!(
                    z.action.as_ref(),
                    Some(GlossaryZoneAction::Link(href)) if href.starts_with("?query=世界")
                )
            })
            .expect("internal ?query=世界 link zone must exist");
        assert!(pb.invoke_glossary_cursor_is_pointer(
            1,
            link_zone.x + 2.0,
            link_zone.y + 4.0
        ));
        pb.invoke_glossary_pointer_down(1, link_zone.x + 2.0, link_zone.y + 4.0);
        pb.invoke_glossary_pointer_up(1, link_zone.x + 2.0, link_zone.y + 4.0);

        assert_eq!(app.get_dict_query().as_str(), "世界");
        assert!(pb.get_can_go_back());
        assert!(!pb.get_can_go_forward());
        assert_eq!(pb.get_nav_breadcrumb().as_str(), "From 世界中");

        pb.invoke_nav_back();
        assert_eq!(app.get_dict_query().as_str(), "世界中");
        assert!(!pb.get_can_go_back());
        assert!(pb.get_can_go_forward());

        pb.invoke_nav_forward();
        assert_eq!(app.get_dict_query().as_str(), "世界");
        assert!(pb.get_can_go_back());
    }

    #[test]
    fn per_kanji_headword_kana_toggle_tags_pitch_derivations_audio_menu_and_isolated_panel() {
        init_no_event_loop();
        let fixture = build_rich_fixture();
        let config = ConfigController::load(&fixture.config_dir, "memento.conf").unwrap();
        let app = MementoApp::new().unwrap();
        app.window().set_size(slint::LogicalSize::new(1280., 720.));
        install_popup_navigation(&app);
        install_rich_ui_bridges(&app, &fixture.db_path, &fixture.config_dir, &config);

        let mut dict_ctrl = DictController::open(&fixture.db_path).unwrap();

        // 1. Tag category colors + #220 redundant tag suppression
        assert_eq!(tag_category_hex_color("popular"), [0x02, 0x75, 0xd8, 0xff]);
        assert_eq!(
            tag_category_hex_color("partOfSpeech"),
            [0x56, 0x56, 0x56, 0xff]
        );
        let raw_term_tags = vec![
            DictTag::with_details("pop", "popular", "popular term", 1, 50),
            DictTag::with_details("pop", "popular", "popular term", 1, 50),
        ];
        let raw_def_tags = vec![
            DictTag::with_details("pop", "popular", "popular term", 1, 50),
            DictTag::with_details("n", "partOfSpeech", "noun (common)", 2, 10),
            DictTag::with_details("adv", "partOfSpeech", "adverb", 3, 10),
        ];
        let hits = dict_ctrl.search("世界中").to_vec();
        let (dedup_term, dedup_def) =
            suppress_redundant_tags(&raw_term_tags, &raw_def_tags, &hits[0].raw_glossary);
        assert_eq!(dedup_term.len(), 1);
        assert_eq!(dedup_term[0].name, "pop");
        // Both `pop` (already in term_tags) and `n` (inline `data-sc-class="tag"` in structured content) must be suppressed!
        assert_eq!(dedup_def.len(), 1);
        assert_eq!(dedup_def[0].name, "adv");

        // Load `"世界中"` into popup and verify bridge pills & per-kanji headword characters
        let rows = memento_app::dictionary_result_rows(&hits, &[]);
        set_dictionary_rows(&app, rows);
        app.set_dict_query("世界中".into());
        app.set_dict_reading("せかいじゅう".into());
        app.set_dict_open(true);
        app.show().unwrap();

        let pb = app.global::<PopupBridge>();
        let hw_chars = pb.invoke_headword_chars("世界中と".into());
        assert_eq!(hw_chars.row_count(), 4);
        assert!(hw_chars.row_data(0).unwrap().is_kanji);
        assert!(hw_chars.row_data(1).unwrap().is_kanji);
        assert!(hw_chars.row_data(2).unwrap().is_kanji);
        assert!(!hw_chars.row_data(3).unwrap().is_kanji);

        let term_pills = pb.invoke_term_tag_pills(1, pb.get_rich_revision());
        let def_pills = pb.invoke_def_tag_pills(1, pb.get_rich_revision());
        assert_eq!(term_pills.row_count(), 1);
        assert_eq!(term_pills.row_data(0).unwrap().name.as_str(), "pop");
        assert_eq!(def_pills.row_count(), 1);
        assert_eq!(def_pills.row_data(0).unwrap().name.as_str(), "adv");

        // 2. Click the SECOND kanji (`'界'`, NOT `'世'`) -> opens 3-column Kanji detail view for `'界'`!
        pb.invoke_lookup_kanji_char("世界中".into(), "界".into());
        assert_eq!(app.get_dict_query().as_str(), "界");
        let kdetail = pb.invoke_kanji_detail(1, pb.get_rich_revision());
        assert!(kdetail.valid);
        assert_eq!(kdetail.character.as_str(), "界");
        let meanings: Vec<_> = kdetail.meanings.iter().map(|s| s.to_string()).collect();
        assert_eq!(meanings, vec!["world", "boundary"]);
        let readings: Vec<_> = kdetail.readings.iter().map(|s| s.to_string()).collect();
        assert_eq!(readings, vec!["カイ", "さかい"]);
        let stats: Vec<_> = kdetail
            .stats
            .iter()
            .map(|s| (s.label.to_string(), s.value.to_string()))
            .collect();
        assert!(stats.contains(&("Stroke count".to_owned(), "9".to_owned())));
        assert!(stats.contains(&("Grade".to_owned(), "3".to_owned())));
        assert_eq!(kdetail.codepoints.row_count(), 1);
        assert_eq!(kdetail.codepoints.row_data(0).unwrap().value.as_str(), "754C");
        assert_eq!(kdetail.classifications.row_count(), 1);
        assert_eq!(kdetail.indices.row_count(), 1);
        // Kanji frequency from `term_meta_bank` should also be attached!
        assert_eq!(
            app.get_dict_results()
                .row_data(0)
                .unwrap()
                .frequencies
                .row_data(0)
                .unwrap()
                .value
                .as_str(),
            "412"
        );

        // Navigate back from Kanji view to `"世界中"`
        pb.invoke_nav_back();
        assert_eq!(app.get_dict_query().as_str(), "世界中");

        // 3. Verify `かな` reading-search toggle on `"橋"` (`"はし"`) -> finds both `"橋"` & `"箸"` by reading,
        //    and toggling `かな` again returns via history!
        pb.invoke_activate_glossary_link("?query=橋".into());
        assert_eq!(app.get_dict_query().as_str(), "橋");
        assert_eq!(app.get_dict_results().row_count(), 1);
        pb.invoke_toggle_kana_reading_search("橋".into(), "はし".into());
        assert!(pb.get_kana_reading_active());
        assert_eq!(pb.get_active_kana_reading().as_str(), "はし");
        assert_eq!(
            app.get_dict_results().row_count(),
            2,
            "かな toggle should find both 橋 and 箸 for はし"
        );
        pb.invoke_toggle_kana_reading_search("橋".into(), "はし".into());
        assert!(!pb.get_kana_reading_active());
        assert_eq!(app.get_dict_results().row_count(), 1);

        // 4. Verify deconjugated derivations (#279) AND 1-based `nasal` / `devoice` pitch markers on `"食べて"` -> `"食べる"`!
        let tabete_hits = dict_ctrl.search("食べて").to_vec();
        assert!(!tabete_hits.is_empty());
        assert!(!tabete_hits[0].derivation_descriptions.is_empty());
        let tabete_rows = memento_app::dictionary_result_rows(&tabete_hits, &[]);
        set_dictionary_rows(&app, tabete_rows);
        app.set_dict_query("食べる".into());
        app.set_dict_reading("たべる".into());

        let derivs = pb.invoke_derivation_steps(1, pb.get_rich_revision());
        assert!(derivs.row_count() >= 1, "derivation badges (#279) must be populated");
        assert!(!derivs.row_data(0).unwrap().label.is_empty());

        let pitch_details: Vec<TermPitchDetail> = dict_ctrl.get_pitch_details("食べる", "たべる");
        assert_eq!(pitch_details.len(), 1);
        let rich_pitches = rich_pitch_graphs_from_details(&pitch_details);
        assert_eq!(rich_pitches.len(), 1);
        let mora: Vec<_> = rich_pitches[0].mora.iter().collect();
        assert_eq!(mora.len(), 3);
        assert!(mora[0].devoice && !mora[0].nasal, "1st mora (た) devoice=[1]");
        assert!(mora[1].nasal && !mora[1].devoice, "2nd mora (べ) nasal=[2]");
        assert!(mora[2].devoice && !mora[2].nasal, "3rd mora (る) devoice=[3]");

        let bridge_pitches = pb.invoke_rich_pitch_graphs(1, pb.get_rich_revision());
        assert_eq!(bridge_pitches.row_count(), 1);
        let devoice_img = pb.invoke_devoice_circle_image();
        assert_eq!(devoice_img.size().width, 36);

        // Audio sources model remains backward-compatible
        app.set_popup_audio_sources(
            Rc::new(VecModel::from(vec![PopupAudioSource {
                index: 0,
                name: "NHK Accent".into(),
            }]))
            .into(),
        );
        assert_eq!(app.get_popup_audio_sources().row_count(), 1);

        // 5. Verify independent async `DictionarySearchPanel` worker isolation!
        //    Popup currently holds `"食べる"`. Searching `"世界"` in `DictionarySearchPanel` MUST NOT
        //    overwrite `app.dict_query` or `app.dict_results`, and updating `app.dict_results`
        //    for subtitle hover MUST NOT overwrite `PopupBridge.panel_query` or `panel_results`!
        pb.invoke_panel_search_requested("世界".into());
        let deadline = Instant::now() + Duration::from_secs(3);
        while pb.get_panel_searching() && Instant::now() < deadline {
            i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(10));
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            pb.get_panel_results().row_count() >= 1,
            "async panel worker should deliver results to PopupBridge.panel_results"
        );
        assert_eq!(
            pb.get_panel_results().row_data(0).unwrap().expression.as_str(),
            "世界"
        );
        // Hover popup state MUST still be `"食べる"`!
        assert_eq!(app.get_dict_query().as_str(), "食べる");
        assert_eq!(
            app.get_dict_results().row_data(0).unwrap().expression.as_str(),
            "食べる"
        );

        // Now simulate a new subtitle hover popup (`"橋"`) while panel is open -> panel results stay `"世界"`!
        app.set_dict_search_open(true);
        app.set_selected_token_index(0);
        let hashi_hits = dict_ctrl.search("橋").to_vec();
        let hashi_rows = memento_app::dictionary_result_rows(&hashi_hits, &[]);
        set_dictionary_rows(&app, hashi_rows);
        app.set_dict_query("橋".into());
        assert_eq!(pb.get_panel_query().as_str(), "世界");
        assert_eq!(
            pb.get_panel_results().row_data(0).unwrap().expression.as_str(),
            "世界",
            "subtitle hover popup must never overwrite DictionarySearchPanel results"
        );
    }

    #[test]
    fn data_dir_res_root_latest_cached_doc_width_and_long_headword_lines() {
        init_no_event_loop();
        let seq = FIXTURE_SEQ.fetch_add(1, Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "memento-p4-data-res-{}-{seq}-{nanos}",
            std::process::id()
        ));
        let config_dir = root.join("config");
        let data_dir = root.join("data");
        let data_res_dir = data_dir.join("res");
        fs::create_dir_all(config_dir.join("dict_resources")).unwrap();
        fs::create_dir_all(&data_res_dir).unwrap();
        let db_path = data_dir.join("dictionaries.sqlite");

        // Extract dictionary into `data_dir/res` (leaving `config_dir/dict_resources` empty!)
        let styles_css = br#"span[data-sc-tag="resroot"] { color: #38BDF8; font-weight: bold; }"#;
        let mono_svg = br##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24"><rect x="2" y="2" width="20" height="20" fill="#000000"/></svg>"##;
        let structured = serde_json::json!({
            "type": "structured-content",
            "content": [
                {
                    "tag": "div",
                    "content": [
                        { "tag": "span", "data": { "tag": "resroot" }, "content": "DataDirResBadge" },
                        " long wrapping text for width cache testing across wide and narrow viewports ",
                        {
                            "tag": "img",
                            "path": "icons/box.svg",
                            "width": 20,
                            "height": 20,
                            "sizeUnits": "px",
                            "appearance": "monochrome",
                            "title": "DataResIconTip"
                        },
                        " ",
                        {
                            "tag": "a",
                            "href": "?query=世界",
                            "content": "NarrowTargetLink"
                        }
                    ]
                }
            ]
        });
        let term_bank = serde_json::json!([
            ["寿限無寿限無五劫のすりきれ海砂利水魚の水行末", "じゅげむ", "n", "", 100, [structured], 1, ""],
            ["世界", "せかい", "n", "", 90, ["world"], 2, ""]
        ]);
        let kanji_bank = serde_json::json!([
            ["寿", "ジュ", "ことぶき", "", ["longevity"], {}]
        ]);
        let zip_path = root.join("data_res_dict.zip");
        write_yomi_zip(
            &zip_path,
            &[
                (
                    "index.json",
                    br#"{"title":"DataResDict","format":3,"revision":"1"}"#,
                ),
                ("styles.css", styles_css),
                ("icons/box.svg", mono_svg),
                ("term_bank_1.json", term_bank.to_string().as_bytes()),
                ("kanji_bank_1.json", kanji_bank.to_string().as_bytes()),
            ],
        );
        let dic_id = yomidbbuilder::process_dictionary(&zip_path, &db_path, &data_res_dir).unwrap();
        assert!(!config_dir
            .join("dict_resources")
            .join(format!("dict-{dic_id}"))
            .exists());
        assert!(data_res_dir
            .join(format!("dict-{dic_id}"))
            .join("styles.css")
            .is_file());

        let dirs = memento_app::collect_dictionary_resource_dirs(&config_dir, &db_path);
        assert_eq!(dirs, vec![config_dir.join("dict_resources"), data_res_dir.clone()]);
        assert!(
            memento_app::resolve_dictionary_asset_path_in_dirs(
                &dirs,
                dic_id,
                "DataResDict",
                "icons/box.svg"
            )
            .is_some()
        );

        let config = ConfigController::load(&config_dir, "memento.conf").unwrap();
        let app = MementoApp::new().unwrap();
        app.window().set_size(slint::LogicalSize::new(1280., 720.));
        install_popup_navigation(&app);
        install_rich_ui_bridges(&app, &db_path, &config_dir, &config);

        let mut dict_ctrl = DictController::open(&db_path).unwrap();
        let hits = dict_ctrl
            .search("寿限無寿限無五劫のすりきれ海砂利水魚の水行末")
            .to_vec();
        assert_eq!(hits.len(), 1);
        let rows = memento_app::dictionary_result_rows(&hits, &[]);
        set_dictionary_rows(&app, rows);
        app.set_dict_query("寿限無寿限無五劫のすりきれ海砂利水魚の水行末".into());
        app.set_anki_added_note_id("12345".into());
        app.set_anki_added_result_index(0);
        app.set_dict_open(true);
        app.show().unwrap();

        let pb = app.global::<PopupBridge>();

        // 1. Verify long headword wraps cleanly across multiple lines without dropping any characters
        let hw_lines = pb.invoke_headword_lines(
            "寿限無寿限無五劫のすりきれ海砂利水魚の水行末".into(),
            220.0,
        );
        assert!(
            hw_lines.row_count() >= 3,
            "22-char headword must wrap into >= 3 lines within 220px"
        );
        let rejoined: String = hw_lines
            .iter()
            .flat_map(|line| line.chars.iter().map(|c| c.text.to_string()).collect::<Vec<_>>())
            .collect();
        assert_eq!(rejoined, "寿限無寿限無五劫のすりきれ海砂利水魚の水行末");

        // 2. Verify `data_dir/res` `styles.css` + SVG `icons/box.svg` render AND `latest_cached_doc` tracks exact latest width!
        let wide_render = pb.invoke_render_glossary(
            1,
            460.0,
            true,
            pb.get_rich_revision(),
            pb.get_selection_revision(),
        );
        let narrow_render = pb.invoke_render_glossary(
            1,
            180.0,
            false,
            pb.get_rich_revision(),
            pb.get_selection_revision(),
        );
        assert!(wide_render.has_rich && narrow_render.has_rich);
        assert!(
            narrow_render.height_px > wide_render.height_px,
            "narrow 180px layout must wrap taller than 460px layout ({} vs {})",
            narrow_render.height_px,
            wide_render.height_px
        );
        // Obtain exact hit-zone coordinates at narrow 180px width and verify `PopupBridge` hits the 180px doc (not stale 460px)!
        let narrow_doc = memento_app::render_structured_glossary_entry_in_dirs(
            &hits[0].raw_glossary,
            &hits[0].glossary,
            None,
            &HashMap::new(),
            LegacyGlossaryStyle::Bullet,
            &dirs,
            dic_id,
            "DataResDict",
            180.0,
            false,
        )
        .unwrap();
        let icon_tip_zone = narrow_doc
            .hit_zones
            .iter()
            .find(|z| z.tooltip.as_deref() == Some("DataResIconTip"))
            .expect("SVG image in data_dir/res must produce tooltip hit zone");
        pb.invoke_glossary_pointer_hover(
            1,
            icon_tip_zone.x + 2.0,
            icon_tip_zone.y + 2.0,
            40.0,
            80.0,
        );
        assert!(pb.get_tooltip_visible());
        assert_eq!(pb.get_tooltip_text().as_str(), "DataResIconTip");

        // 3. Verify in-popup navigation clears stale `anki_added_note_id` / `anki_added_result_index`!
        pb.invoke_lookup_kanji_char(
            "寿限無寿限無五劫のすりきれ海砂利水魚の水行末".into(),
            "寿".into(),
        );
        assert_eq!(app.get_dict_query().as_str(), "寿");
        assert_eq!(app.get_anki_added_note_id().as_str(), "");
        assert_eq!(app.get_anki_added_result_index(), -1);

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn color_flag_emojis_multi_link_non_overlap_and_monochrome_svg_mask_cutout() {
        init_no_event_loop();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "memento-rich-emoji-links-svg-{}-{nanos}",
            std::process::id()
        ));
        let res_dir = root.join("res");
        let smk_dir = res_dir.join("dict-5").join("smk8");
        fs::create_dir_all(&smk_dir).unwrap();

        // Exact `smk8/運用-fill.svg` structure using `<mask>` + `<text font-family="sans-serif">`
        let svg_path = smk_dir.join("運用-fill.svg");
        fs::write(
            &svg_path,
            r#"<svg lang='ja' width='256' height='128' viewBox='0 0 256 128'
     xmlns='http://www.w3.org/2000/svg' version='1.1'>
    <mask id='a'>
        <rect width='256' height='128' fill='white'/>
        <text text-anchor='middle' x='50%' y='50%' dy='.35em'
              font-family='sans-serif' font-size='100px'
              fill='black'>運用</text>
    </mask>
    <rect width='256' height='128' ry='20'
          fill='black' mask='url(#a)'/>
</svg>"#,
        )
        .unwrap();

        // 1. Monochrome SVG mask badge must render BOTH opaque tinted pill pixels AND
        //    transparent interior cutout pixels for `運用` (never a solid white rectangle).
        let pixmap = memento_app::load_dictionary_image_pixmap(
            &svg_path,
            Some(30.0),
            Some(15.0),
            Some([248, 250, 252, 255]),
            "#F8FAFC",
        )
        .expect("monochrome mask SVG must rasterize");
        let w = pixmap.width() as usize;
        let h = pixmap.height() as usize;
        let mut opaque_inside = 0usize;
        let mut transparent_cutout_inside = 0usize;
        for y in 2..(h.saturating_sub(2)) {
            for x in 4..(w.saturating_sub(4)) {
                let a = pixmap.pixels()[y * w + x].alpha();
                if a >= 200 {
                    opaque_inside += 1;
                } else if a <= 80 {
                    transparent_cutout_inside += 1;
                }
            }
        }
        assert!(opaque_inside > 20, "badge pill must have opaque pixels ({opaque_inside})");
        assert!(
            transparent_cutout_inside > 10,
            "SVG mask text cutout `運用` must leave transparent pixels inside badge (cutout={transparent_cutout_inside}, opaque={opaque_inside})"
        );

        // 2. Multi-span link line (`⇨あんな・こんな・どんな：こそあど`) must NOT duplicate/overlap
        //    clusters across GlyphRuns sharing the same shaped Run.
        let multi_link_json = r#"[{"type":"structured-content","content":["⇨",{"tag":"span","content":{"tag":"a","href":"?query=あんな&wildcards=off","content":"あんな"}},"・",{"tag":"span","content":{"tag":"a","href":"?query=こんな&wildcards=off","content":"こんな"}},"・",{"tag":"span","content":{"tag":"a","href":"?query=どんな&wildcards=off","content":"どんな"}},"：",{"tag":"span","content":{"tag":"a","href":"?query=こそあど&wildcards=off","content":"こそあど"}}]}]"#;
        let link_doc = memento_app::render_structured_glossary_entry_in_dirs(
            multi_link_json,
            "",
            None,
            &HashMap::new(),
            LegacyGlossaryStyle::Bullet,
            &[res_dir.clone()],
            5,
            "SMK8",
            440.0,
            true,
        )
        .expect("multi-link entry must render");
        assert_eq!(
            link_doc.plain_text, "⇨あんな・こんな・どんな：こそあど",
            "multi-span GlyphRuns must emit each character exactly once without overlapping duplicates"
        );
        let non_nl: Vec<_> = link_doc
            .selectable_chars
            .iter()
            .filter(|c| c.ch != '\n')
            .collect();
        for pair in non_nl.windows(2) {
            assert!(
                pair[1].x >= pair[0].x + 2.0,
                "adjacent characters '{}' (x={}) and '{}' (x={}) must advance monotonically without overlapping",
                pair[0].ch,
                pair[0].x,
                pair[1].ch,
                pair[1].x
            );
        }

        // 3. JMdict flag emoji list markers (`'🇯🇵 '` and `'🇬🇧 '`) must render color emoji bitmaps
        //    (containing real red flag pixels when Noto Color Emoji is installed) rather than cyan `JP`/`GB`.
        let flag_json = r#"[{"type":"structured-content","content":[{"tag":"ul","style":{"listStyleType":"'🇯🇵 '"},"content":[{"tag":"li","content":"あなたが呼べば、彼は来るでしょう。"},{"tag":"li","style":{"listStyleType":"'🇬🇧 '"},"content":"He will come if you call him."}]}]}]"#;
        let flag_doc = memento_app::render_structured_glossary_entry_in_dirs(
            flag_json,
            "",
            None,
            &HashMap::new(),
            LegacyGlossaryStyle::Bullet,
            &[res_dir],
            9,
            "JMdict",
            440.0,
            true,
        )
        .expect("flag emoji entry must render");
        if let Some(rgba) = flag_doc.image.to_rgba8() {
            let red_flag_pixels = rgba
                .as_slice()
                .iter()
                .filter(|px| px.a > 120 && px.r > 150 && px.r > px.g.saturating_add(50) && px.r > px.b.saturating_add(50))
                .count();
            assert!(
                red_flag_pixels > 0,
                "flag emoji markers 🇯🇵/🇬🇧 must render full-color emoji pixels (red_flag_pixels={red_flag_pixels})"
            );
        }

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn custom_popup_css_applies_to_popup_bridge_and_hot_reloads() {
        init_no_event_loop();
        let fixture = build_rich_fixture();
        let css_path = fixture.config_dir.join(memento_app::CUSTOM_POPUP_CSS_FILENAME);
        fs::write(
            &css_path,
            r#"
            :root {
                --popup-bg: #0D1117;
                --popup-radius: 12px;
                --headword-color: #FF79C6;
                --headword-size: 27px;
                --dict-badge-bg: #2563EB;
            }
            "#,
        )
        .unwrap();

        let app = MementoApp::new().unwrap();
        let cfg = ConfigController::new();
        install_rich_ui_bridges(&app, &fixture.db_path, &fixture.config_dir, &cfg);

        let pb = app.global::<PopupBridge>();
        let bg = pb.get_css_popup_bg().to_argb_u8();
        assert_eq!((bg.red, bg.green, bg.blue), (0x0D, 0x11, 0x17));
        assert!((pb.get_css_popup_radius() - 12.0).abs() < 0.01);
        let hw = pb.get_css_headword_color().to_argb_u8();
        assert_eq!((hw.red, hw.green, hw.blue), (0xFF, 0x79, 0xC6));
        assert!((pb.get_css_headword_size() - 27.0).abs() < 0.01);
        let badge = pb.get_css_dict_badge_bg().to_argb_u8();
        assert_eq!((badge.red, badge.green, badge.blue), (0x25, 0x63, 0xEB));

        // Edit popup.css on disk and reload via refresh_rich_ui_bridges.
        let rev_before = pb.get_rich_revision();
        fs::write(
            &css_path,
            r#"
            .popup {
                background-color: #FAF9F6;
                border: 2px solid #D1D5DB;
                border-radius: 6px;
            }
            .headword {
                color: #111827;
                font-size: 25px;
            }
            "#,
        )
        .unwrap();
        memento_app::refresh_rich_ui_bridges(&app, &cfg);

        let bg2 = pb.get_css_popup_bg().to_argb_u8();
        assert_eq!((bg2.red, bg2.green, bg2.blue), (0xFA, 0xF9, 0xF6));
        assert!((pb.get_css_popup_border_width() - 2.0).abs() < 0.01);
        assert!((pb.get_css_popup_radius() - 6.0).abs() < 0.01);
        let hw2 = pb.get_css_headword_color().to_argb_u8();
        assert_eq!((hw2.red, hw2.green, hw2.blue), (0x11, 0x18, 0x27));
        assert!((pb.get_css_headword_size() - 25.0).abs() < 0.01);
        assert!(pb.get_rich_revision() > rev_before);

        // Write split popup-outer.css + 2-column inner popup.css and verify both reload together.
        let outer_path = fixture
            .config_dir
            .join(memento_app::CUSTOM_POPUP_OUTER_CSS_FILENAME);
        fs::write(
            &outer_path,
            r#"
            .popup {
                background: rgba(45, 45, 55, 0.85);
                border-radius: 12px;
                width: 780px;
            }
            "#,
        )
        .unwrap();
        fs::write(
            &css_path,
            r#"
            :root {
                --definition-columns: 2;
                --definition-gap: 10px;
            }
            [data-dictionary="JMdict"] {
                --dict-color: #0084FF;
                --glossary-opacity: 0;
                --glossary-hover-opacity: 1;
            }
            [data-frequency^="JPDB"] {
                background-color: #FF3E3D;
            }
            "#,
        )
        .unwrap();
        memento_app::refresh_rich_ui_bridges(&app, &cfg);

        let bg3 = pb.get_css_popup_bg().to_argb_u8();
        assert_eq!((bg3.red, bg3.green, bg3.blue), (45, 45, 55));
        assert!((pb.get_css_popup_width() - 780.0).abs() < 0.01);
        assert_eq!(pb.get_css_definition_columns(), 2);

        let dstyle = pb.invoke_dict_entry_style("JMdict".into(), false, pb.get_rich_revision());
        let d_bg = dstyle.badge_bg.to_argb_u8();
        assert_eq!((d_bg.red, d_bg.green, d_bg.blue), (0x00, 0x84, 0xFF));
        assert_eq!(dstyle.glossary_opacity, 0.0);
        assert_eq!(dstyle.glossary_hover_opacity, 1.0);

        let fstyle = pb.invoke_freq_badge_style("JPDB".into(), pb.get_rich_revision());
        let f_bg = fstyle.bg.to_argb_u8();
        assert_eq!((f_bg.red, f_bg.green, f_bg.blue), (0xFF, 0x3E, 0x3D));

        // Verify 2-column grid pairing: 3 definitions in one term -> 2 grid rows (2 + 1);
        // 1 definition in second term -> 1 solo full-width row.
        let sample_rows = Rc::new(VecModel::from(vec![
            DictResult { index: 1, term_start: true, term_end: false, expression: "呼ぶ".into(), dict_name: "D1".into(), ..DictResult::default() },
            DictResult { index: 2, term_start: false, term_end: false, expression: "呼ぶ".into(), dict_name: "D2".into(), ..DictResult::default() },
            DictResult { index: 3, term_start: false, term_end: true, expression: "呼ぶ".into(), dict_name: "D3".into(), ..DictResult::default() },
            DictResult { index: 4, term_start: true, term_end: true, expression: "単".into(), dict_name: "D4".into(), ..DictResult::default() },
        ]));
        let term1_pairs = pb.invoke_definition_grid_rows(sample_rows.clone().into(), 1);
        assert_eq!(term1_pairs.row_count(), 2);
        let p0 = term1_pairs.row_data(0).unwrap();
        assert!(p0.has_right && !p0.is_solo && p0.left.index == 1 && p0.right.index == 2);
        let p1 = term1_pairs.row_data(1).unwrap();
        assert!(!p1.has_right && !p1.is_solo && p1.left.index == 3);

        let term2_pairs = pb.invoke_definition_grid_rows(sample_rows.into(), 4);
        assert_eq!(term2_pairs.row_count(), 1);
        let s0 = term2_pairs.row_data(0).unwrap();
        assert!(!s0.has_right && s0.is_solo && s0.left.index == 4);
    }

    fn build_recursive_scan_fixture() -> TempRichFixture {
        let seq = FIXTURE_SEQ.fetch_add(1, Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "memento-v2-recursive-{}-{seq}-{nanos}",
            std::process::id()
        ));
        let config_dir = root.join("config");
        let dict_resources = config_dir.join("dict_resources");
        fs::create_dir_all(&dict_resources).unwrap();
        let db_path = config_dir.join("dictionaries.sqlite");

        let index_daijisen = serde_json::json!({
            "title": "大辞泉",
            "format": 3,
            "revision": "1"
        })
        .to_string();

        let gloss_root = serde_json::json!({
            "type": "structured-content",
            "content": [
                {
                    "tag": "div",
                    "content": "世界の言葉を指す。特に日常会話で広く使われる表現の一つであり、文脈によって様々な意味を持つ。さらに多くの用例が存在するため複数行にわたって詳しく解説される。最後に補足説明が続く。"
                },
                {
                    "tag": "div",
                    "content": [
                        "参照：",
                        {
                            "tag": "a",
                            "href": "?query=リンク先",
                            "content": "リンク先"
                        }
                    ]
                }
            ]
        });
        let gloss_kotoba = serde_json::json!({
            "type": "structured-content",
            "content": [
                {
                    "tag": "div",
                    "content": "文字や音声で思想を伝える手段。"
                }
            ]
        });
        let gloss_moji = serde_json::json!({
            "type": "structured-content",
            "content": [
                {
                    "tag": "div",
                    "content": "言葉を書き表すための記号。"
                }
            ]
        });
        let gloss_link = serde_json::json!({
            "type": "structured-content",
            "content": [
                {
                    "tag": "div",
                    "content": "明示的リンクの移動先。"
                }
            ]
        });

        let terms_daijisen = serde_json::json!([
            ["大辞泉語", "だいじせんご", "n", "", 100, [gloss_root], 1, ""],
            ["言葉", "ことば", "n", "", 95, [gloss_kotoba], 2, ""],
            ["文字", "もじ", "n", "", 90, [gloss_moji], 3, ""],
            ["リンク先", "りんくさき", "n", "", 85, [gloss_link], 4, ""],
            ["世界", "せかい", "n", "", 80, ["地球上のすべての国や地域。"], 5, ""]
        ])
        .to_string();

        let zip1 = root.join("daijisen.zip");
        write_yomi_zip(
            &zip1,
            &[
                ("index.json", index_daijisen.as_bytes()),
                ("term_bank_1.json", terms_daijisen.as_bytes()),
            ],
        );

        let index_jmdict = serde_json::json!({
            "title": "JMdict [2025]",
            "format": 3,
            "revision": "1"
        })
        .to_string();
        let terms_jmdict = serde_json::json!([
            [
                "大辞泉語",
                "だいじせんご",
                "n",
                "",
                70,
                ["line one English\nline two 言葉\nline three details\nline four extra\nline five tail"],
                10,
                ""
            ]
        ])
        .to_string();
        let zip2 = root.join("jmdict.zip");
        write_yomi_zip(
            &zip2,
            &[
                ("index.json", index_jmdict.as_bytes()),
                ("term_bank_1.json", terms_jmdict.as_bytes()),
            ],
        );

        let dic_id =
            yomidbbuilder::process_dictionary(&zip1, &db_path, &dict_resources).unwrap();
        let _ = yomidbbuilder::process_dictionary(&zip2, &db_path, &dict_resources).unwrap();
        TempRichFixture {
            root,
            config_dir,
            db_path,
            dic_id,
        }
    }

    fn wait_for_nested_depth(app: &MementoApp, expected_depth: i32) {
        let pb = app.global::<PopupBridge>();
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(12));
            if pb.get_nested_depth() == expected_depth && !pb.get_nested_searching() {
                return;
            }
            std::thread::sleep(Duration::from_millis(8));
        }
        panic!(
            "timed out waiting for nested_depth={expected_depth}, got depth={}, searching={}",
            pb.get_nested_depth(),
            pb.get_nested_searching()
        );
    }

    #[test]
    fn deliverable1_no_x_close_button_and_escape_closes_popup() {
        init_no_event_loop();
        let fixture = build_recursive_scan_fixture();
        let app = MementoApp::new().unwrap();
        let cfg = ConfigController::new();
        install_popup_navigation(&app);
        install_rich_ui_bridges(&app, &fixture.db_path, &fixture.config_dir, &cfg);
        app.window().set_size(slint::LogicalSize::new(1280., 720.));

        let mut dict = DictController::open(&fixture.db_path).unwrap();
        let hits = dict.search("大辞泉語").to_vec();
        let rows = memento_app::dictionary_result_rows(&hits, &[]);
        set_dictionary_rows(&app, rows);
        app.set_dict_query("大辞泉語".into());
        app.set_dict_open(true);
        app.show().unwrap();

        if std::env::var("SLINT_EMIT_DEBUG_INFO").as_deref() == Ok("1") {
            let close_buttons = ElementQuery::from_root(&app)
                .match_id("YomitanPopup::close-touch")
                .find_all();
            assert!(
                close_buttons.is_empty(),
                "X close button must be completely removed from YomitanPopup"
            );
        }

        // Pressing Escape must dismiss the popup cleanly.
        let esc = slint::SharedString::from(slint::platform::Key::Escape);
        app.window()
            .dispatch_event(slint::platform::WindowEvent::KeyPressed { text: esc.clone() });
        app.window()
            .dispatch_event(slint::platform::WindowEvent::KeyReleased { text: esc });
        assert!(
            !app.get_dict_open(),
            "Escape key must still close the popup after X button removal"
        );
    }

    #[test]
    fn deliverable2_recursive_scan_hit_test_and_soft_wrap_substring_extraction() {
        init_no_event_loop();
        let raw = r#"[{"type":"structured-content","content":"世界の言葉を指す。English text here."}]"#;
        let doc = memento_app::render_structured_glossary_entry_in_dirs(
            raw,
            "",
            None,
            &HashMap::new(),
            LegacyGlossaryStyle::Bullet,
            &[],
            1,
            "大辞泉",
            280.0,
            true,
        )
        .expect("doc should render");

        // Blank right-edge / negative margin must return None on strict hit test.
        assert_eq!(memento_app::strict_char_index_at_point(&doc, -5.0, 5.0), None);
        assert_eq!(
            memento_app::strict_char_index_at_point(&doc, doc.width_px + 20.0, 5.0),
            None
        );

        // Pointing at '言' inside "世界の言葉を指す。" must map strictly to '言' and extract up to '。'.
        let k_idx = doc
            .selectable_chars
            .iter()
            .position(|c| c.ch == '言')
            .expect("must contain 言");
        let cell = &doc.selectable_chars[k_idx];
        let hit = memento_app::strict_char_index_at_point(
            &doc,
            cell.x + cell.w * 0.5,
            cell.y + cell.h * 0.5,
        );
        assert_eq!(hit, Some(k_idx));

        let (sub, indices) =
            memento_app::extract_glossary_scan_substring(&doc.selectable_chars, k_idx, 24)
                .expect("should extract Japanese scan substring");
        assert_eq!(sub, "言葉を指す");
        assert_eq!(indices.len(), sub.chars().count());

        // Pointing at English letter 'E' must NOT extract a Japanese scan substring.
        let e_idx = doc
            .selectable_chars
            .iter()
            .position(|c| c.ch == 'E')
            .expect("must contain E");
        assert_eq!(
            memento_app::extract_glossary_scan_substring(&doc.selectable_chars, e_idx, 24),
            None
        );

        // Narrow width forces soft line wrap inside a long Japanese phrase; extraction must bridge it.
        let wrapped_raw =
            r#"[{"type":"structured-content","content":"日本語辞書の解説文を続けて読む。"}]"#;
        let narrow_doc = memento_app::render_structured_glossary_entry_in_dirs(
            wrapped_raw,
            "",
            None,
            &HashMap::new(),
            LegacyGlossaryStyle::Bullet,
            &[],
            1,
            "大辞泉",
            120.0,
            true,
        )
        .expect("narrow doc should render");
        assert!(
            narrow_doc.line_bottoms.len() >= 2,
            "narrow width must wrap onto multiple lines (got {})",
            narrow_doc.line_bottoms.len()
        );
        let first_ja = narrow_doc
            .selectable_chars
            .iter()
            .position(|c| c.ch == '日')
            .unwrap();
        let (bridged, bridged_indices) =
            memento_app::extract_glossary_scan_substring(&narrow_doc.selectable_chars, first_ja, 24)
                .expect("should bridge soft line wrap");
        assert_eq!(bridged, "日本語辞書の解説文を続けて読む");
        assert_eq!(bridged_indices.len(), bridged.chars().count());
    }

    #[test]
    fn deliverable2_recursive_nested_popup_hover_modifier_middle_click_drag_guard_stacking_and_2col() {
        init_no_event_loop();
        let fixture = build_recursive_scan_fixture();
        let app = MementoApp::new().unwrap();
        let cfg = ConfigController::new();
        cfg.edit_setting("search/method", "0").unwrap(); // Hover
        cfg.edit_setting("search/delay", "120").unwrap();
        cfg.edit_setting("search/modifier", "2").unwrap(); // Shift
        cfg.edit_setting("search/middle-mouse-scan", "false")
            .unwrap();

        install_popup_navigation(&app);
        install_rich_ui_bridges(&app, &fixture.db_path, &fixture.config_dir, &cfg);
        app.window().set_size(slint::LogicalSize::new(1280., 720.));

        let mut dict = DictController::open(&fixture.db_path).unwrap();
        let hits = dict.search("大辞泉語").to_vec();
        let rows = memento_app::dictionary_result_rows(&hits, &[]);
        set_dictionary_rows(&app, rows);
        app.set_dict_query("大辞泉語".into());
        app.set_dict_open(true);
        app.show().unwrap();

        let pb = app.global::<PopupBridge>();
        let rev = pb.get_rich_revision();
        let sel_rev = pb.get_selection_revision();
        let rendered = pb.invoke_render_glossary(1, 360.0, true, rev, sel_rev);
        assert!(rendered.has_rich);

        // Compute exact character coordinates for '言' in row 1's rendered doc.
        let hit0 = &hits[0];
        let ref_doc = memento_app::render_structured_glossary_entry_in_dirs(
            &hit0.raw_glossary,
            &hit0.glossary,
            None,
            &HashMap::new(),
            LegacyGlossaryStyle::Bullet,
            &[],
            hit0.dictionary_id,
            &hit0.dict_name,
            360.0,
            true,
        )
        .unwrap();
        let k_idx = ref_doc
            .selectable_chars
            .iter()
            .position(|c| c.ch == '言')
            .unwrap();
        let k_cell = &ref_doc.selectable_chars[k_idx];
        let kx = k_cell.x + k_cell.w * 0.5;
        let ky = k_cell.y + k_cell.h * 0.5;

        // 1. Hover schedules scan after search_delay_ms; mouse-down MUST cancel it and preserve drag-select.
        pb.invoke_glossary_scan_hover(0, 1, kx, ky, 120.0, 140.0, false, false, false, false);
        assert_eq!(
            pb.get_nested_depth(),
            0,
            "hover must respect search_delay_ms before opening nested popup"
        );
        pb.invoke_glossary_pointer_down(1, kx, ky);
        pb.invoke_glossary_pointer_drag(1, kx + 40.0, ky);
        pb.invoke_glossary_pointer_up(1, kx + 40.0, ky);
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(260));
        assert_eq!(
            pb.get_nested_depth(),
            0,
            "mouse-down for drag-selection must cancel pending recursive hover-scan timer"
        );
        assert!(
            !pb.get_selected_glossary_text().is_empty(),
            "drag-to-select glossary text must remain fully intact"
        );

        // 2. Clean hover triggers async nested lookup for 言葉 + continuous blue highlight on parent.
        pb.invoke_glossary_scan_hover(0, 1, kx, ky, 120.0, 140.0, false, false, false, false);
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(140));
        wait_for_nested_depth(&app, 1);
        assert_eq!(pb.get_nested_headword().as_str(), "言葉");
        assert_eq!(pb.get_nested_breadcrumb().as_str(), "言葉");
        assert!(pb.get_nested_results().row_count() >= 1);
        assert!(!pb.get_nested_locked());

        let parent_hl = pb.invoke_render_glossary(
            1,
            360.0,
            true,
            pb.get_rich_revision(),
            pb.get_selection_revision(),
        );
        assert!(
            parent_hl.scan_highlight_rects.row_count() >= 1,
            "parent glossary must display continuous blue highlight on matched nested word"
        );

        // 3. Recursive depth-2 lookup inside depth-1 nested glossary (文字).
        let nrender = pb.invoke_nested_render_glossary(
            1,
            1,
            320.0,
            true,
            pb.get_nested_revision(),
            pb.get_selection_revision(),
        );
        assert!(nrender.has_rich);
        let kotoba_hits = dict.search("言葉").to_vec();
        let ref_doc_1 = memento_app::render_structured_glossary_entry_in_dirs(
            &kotoba_hits[0].raw_glossary,
            &kotoba_hits[0].glossary,
            None,
            &HashMap::new(),
            LegacyGlossaryStyle::Bullet,
            &[],
            kotoba_hits[0].dictionary_id,
            &kotoba_hits[0].dict_name,
            320.0,
            true,
        )
        .unwrap();
        let m_idx = ref_doc_1
            .selectable_chars
            .iter()
            .position(|c| c.ch == '文')
            .unwrap();
        let m_cell = &ref_doc_1.selectable_chars[m_idx];
        pb.invoke_glossary_scan_hover(
            1,
            1,
            m_cell.x + m_cell.w * 0.5,
            m_cell.y + m_cell.h * 0.5,
            160.0,
            180.0,
            false,
            false,
            false,
            false,
        );
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(140));
        wait_for_nested_depth(&app, 2);
        assert_eq!(pb.get_nested_headword().as_str(), "文字");
        assert_eq!(pb.get_nested_breadcrumb().as_str(), "言葉 › 文字");

        // 4. Pressing Escape pops one nested level at a time without losing the parent popup!
        let esc = slint::SharedString::from(slint::platform::Key::Escape);
        app.window()
            .dispatch_event(slint::platform::WindowEvent::KeyPressed { text: esc.clone() });
        app.window()
            .dispatch_event(slint::platform::WindowEvent::KeyReleased { text: esc.clone() });
        assert_eq!(pb.get_nested_depth(), 1);
        assert_eq!(pb.get_nested_headword().as_str(), "言葉");
        assert!(app.get_dict_open());
        assert_eq!(app.get_dict_query().as_str(), "大辞泉語");

        app.window()
            .dispatch_event(slint::platform::WindowEvent::KeyPressed { text: esc.clone() });
        app.window()
            .dispatch_event(slint::platform::WindowEvent::KeyReleased { text: esc });
        assert_eq!(pb.get_nested_depth(), 0);
        assert!(app.get_dict_open());
        let parent_after_pop = pb.invoke_render_glossary(
            1,
            360.0,
            true,
            pb.get_rich_revision(),
            pb.get_selection_revision(),
        );
        assert_eq!(
            parent_after_pop.scan_highlight_rects.row_count(),
            0,
            "blue scan highlight must clear when child popup is dismissed"
        );

        // 5. Unlocked child dismisses after hover-leave grace period; locked child stays open.
        pb.invoke_glossary_scan_hover(0, 1, kx, ky, 120.0, 140.0, false, false, false, false);
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(140));
        wait_for_nested_depth(&app, 1);
        pb.invoke_glossary_scan_leave(0, 1);
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(320));
        assert_eq!(
            pb.get_nested_depth(),
            0,
            "leaving an unlocked child popup must dismiss it cleanly without closing parent"
        );
        assert!(app.get_dict_open());

        pb.invoke_glossary_scan_hover(0, 1, kx, ky, 120.0, 140.0, false, false, false, false);
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(140));
        wait_for_nested_depth(&app, 1);
        pb.invoke_lock_nested_popup();
        assert!(pb.get_nested_locked());
        pb.invoke_glossary_scan_leave(0, 1);
        pb.invoke_nested_popup_hover_changed(false);
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(350));
        assert_eq!(
            pb.get_nested_depth(),
            1,
            "locked child popup must stay open after hover-leave"
        );
        pb.invoke_dismiss_all_nested_popups();
        assert_eq!(pb.get_nested_depth(), 0);

        // 6. Modifier scan mode (search/method = 1, Shift): hover without Shift does nothing, Shift triggers!
        cfg.edit_setting("search/method", "1").unwrap();
        memento_app::refresh_rich_ui_bridges(&app, &cfg);
        let _ = pb.invoke_render_glossary(
            1,
            360.0,
            true,
            pb.get_rich_revision(),
            pb.get_selection_revision(),
        );
        pb.invoke_glossary_scan_hover(0, 1, kx, ky, 120.0, 140.0, false, false, false, false);
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(260));
        assert_eq!(
            pb.get_nested_depth(),
            0,
            "Modifier mode must not scan when modifier key is not held"
        );
        let shift_text = slint::SharedString::from(slint::platform::Key::Shift);
        assert!(pb.invoke_popup_key_pressed(shift_text, false, true, false, false));
        wait_for_nested_depth(&app, 1);
        assert_eq!(pb.get_nested_headword().as_str(), "言葉");
        pb.invoke_dismiss_all_nested_popups();

        // 7. Middle-mouse scan respects search/middle-mouse-scan toggle and opens locked child.
        pb.invoke_glossary_scan_middle_click(0, 1, kx, ky, 120.0, 140.0);
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(60));
        assert_eq!(
            pb.get_nested_depth(),
            0,
            "middle-mouse scan must be ignored when search/middle-mouse-scan is false"
        );
        cfg.edit_setting("search/middle-mouse-scan", "true")
            .unwrap();
        memento_app::refresh_rich_ui_bridges(&app, &cfg);
        let _ = pb.invoke_render_glossary(
            1,
            360.0,
            true,
            pb.get_rich_revision(),
            pb.get_selection_revision(),
        );
        pb.invoke_glossary_scan_middle_click(0, 1, kx, ky, 120.0, 140.0);
        wait_for_nested_depth(&app, 1);
        assert_eq!(pb.get_nested_headword().as_str(), "言葉");
        assert!(
            pb.get_nested_locked(),
            "middle-click nested scan should open locked so user can interact immediately"
        );
        pb.invoke_dismiss_all_nested_popups();

        // 8. 2-column mode (`--definition-columns: 2`): scanning row 2 (right column) works and highlights only row 2!
        fs::write(
            fixture.config_dir.join(memento_app::CUSTOM_POPUP_CSS_FILENAME),
            ":root { --definition-columns: 2; }",
        )
        .unwrap();
        cfg.edit_setting("search/method", "0").unwrap();
        cfg.edit_setting("search/delay", "0").unwrap();
        memento_app::refresh_rich_ui_bridges(&app, &cfg);
        assert_eq!(pb.get_css_definition_columns(), 2);
        let _ = pb.invoke_render_glossary(
            1,
            240.0,
            true,
            pb.get_rich_revision(),
            pb.get_selection_revision(),
        );
        let r2 = pb.invoke_render_glossary(
            2,
            240.0,
            true,
            pb.get_rich_revision(),
            pb.get_selection_revision(),
        );
        assert!(r2.has_rich, "second column row must render in 2-col layout");
        let hit1 = &hits[1];
        let ref_doc_col2 = memento_app::render_structured_glossary_entry_in_dirs(
            &hit1.raw_glossary,
            &hit1.glossary,
            None,
            &HashMap::new(),
            LegacyGlossaryStyle::Bullet,
            &[],
            hit1.dictionary_id,
            &hit1.dict_name,
            240.0,
            true,
        )
        .unwrap();
        let col2_k_idx = ref_doc_col2
            .selectable_chars
            .iter()
            .position(|c| c.ch == '言')
            .unwrap();
        let c2 = &ref_doc_col2.selectable_chars[col2_k_idx];
        pb.invoke_glossary_scan_hover(
            0,
            2,
            c2.x + c2.w * 0.5,
            c2.y + c2.h * 0.5,
            340.0,
            160.0,
            false,
            false,
            false,
            false,
        );
        wait_for_nested_depth(&app, 1);
        assert_eq!(pb.get_nested_headword().as_str(), "言葉");
        let col1_after = pb.invoke_render_glossary(
            1,
            240.0,
            true,
            pb.get_rich_revision(),
            pb.get_selection_revision(),
        );
        let col2_after = pb.invoke_render_glossary(
            2,
            240.0,
            true,
            pb.get_rich_revision(),
            pb.get_selection_revision(),
        );
        assert_eq!(col1_after.scan_highlight_rects.row_count(), 0);
        assert!(col2_after.scan_highlight_rects.row_count() >= 1);

        // 9. Explicit `?query=` links keep working cleanly.
        assert!(pb.invoke_activate_glossary_link("?query=リンク先".into()));
        assert_eq!(app.get_dict_query().as_str(), "リンク先");
        assert_eq!(
            pb.get_nested_depth(),
            0,
            "top-level navigation must dismiss stale nested popups"
        );
    }

    #[test]
    fn deliverable3_collapsible_definition_line_count_css_and_toggle_state() {
        init_no_event_loop();
        // Template documents --collapsible-definition-line-count and still parses as default theme.
        let tmpl = memento_app::default_custom_popup_css_template();
        assert!(
            tmpl.contains("--collapsible-definition-line-count"),
            "default popup.css template must document --collapsible-definition-line-count"
        );
        assert_eq!(
            memento_app::parse_split_popup_css_theme(
                tmpl,
                memento_app::default_custom_popup_outer_css_template()
            ),
            memento_app::PopupCssTheme::default()
        );

        // CSS parsing: :root default + exact/prefix/contains/suffix overrides + 0/none disable.
        let css = r#"
            :root {
                --collapsible-definition-line-count: 4;
            }
            [data-dictionary="大辞泉"] {
                --collapsible-definition-line-count: 2;
            }
            [data-dictionary^="JMdict"] {
                --collapsible-definition-line-count: 0;
            }
            [data-dictionary*="明鏡"] {
                --collapsible-definition-line-count: none;
            }
            [data-dictionary$="版"] {
                --collapsible-definition-line-count: 5;
            }
        "#;
        let theme = memento_app::parse_popup_css_theme(css);
        assert_eq!(theme.resolve_dict_style("OtherDict").collapsible_line_count, 4);
        assert_eq!(theme.resolve_dict_style("大辞泉").collapsible_line_count, 2);
        assert_eq!(
            theme.resolve_dict_style("JMdict [2025]").collapsible_line_count,
            0
        );
        assert_eq!(
            theme.resolve_dict_style("明鏡国語辞典").collapsible_line_count,
            0
        );
        assert_eq!(theme.resolve_dict_style("広辞苑第七版").collapsible_line_count, 5);

        // End-to-end RichGlossaryRender collapse + toggle + reset on new lookup.
        let fixture = build_recursive_scan_fixture();
        fs::write(
            fixture.config_dir.join(memento_app::CUSTOM_POPUP_CSS_FILENAME),
            css,
        )
        .unwrap();

        let app = MementoApp::new().unwrap();
        let cfg = ConfigController::new();
        install_popup_navigation(&app);
        install_rich_ui_bridges(&app, &fixture.db_path, &fixture.config_dir, &cfg);
        app.window().set_size(slint::LogicalSize::new(1280., 720.));

        let mut dict = DictController::open(&fixture.db_path).unwrap();
        let hits = dict.search("大辞泉語").to_vec();
        let rows = memento_app::dictionary_result_rows(&hits, &[]);
        set_dictionary_rows(&app, rows);
        app.set_dict_query("大辞泉語".into());
        app.set_dict_open(true);
        app.show().unwrap();

        let pb = app.global::<PopupBridge>();
        // Row 1 is 大辞泉 (limit 2 lines, content wraps to >= 4 lines at 260px width).
        let card1 = pb.invoke_render_glossary(
            1,
            260.0,
            true,
            pb.get_rich_revision(),
            pb.get_selection_revision(),
        );
        assert!(card1.has_rich);
        assert!(
            card1.total_lines > 2,
            "大辞泉 test entry should span > 2 lines, got {}",
            card1.total_lines
        );
        assert!(card1.collapsible, "大辞泉 card exceeding 2 lines must be collapsible");
        assert!(
            card1.collapsed_height > 0.0 && card1.collapsed_height < card1.height_px,
            "collapsed height ({}) must be strictly smaller than full height ({})",
            card1.collapsed_height,
            card1.height_px
        );
        assert!(!card1.expanded);

        // Row 2 is JMdict [2025] (limit 0 -> disabled).
        let card2 = pb.invoke_render_glossary(
            2,
            260.0,
            true,
            pb.get_rich_revision(),
            pb.get_selection_revision(),
        );
        assert!(card2.has_rich);
        assert!(
            !card2.collapsible,
            "JMdict card with --collapsible-definition-line-count: 0 must not collapse"
        );

        // Toggle expand -> expanded becomes true; toggle again -> false.
        if std::env::var("SLINT_EMIT_DEBUG_INFO").as_deref() == Ok("1") {
            let toggle_bar = ElementQuery::from_root(&app)
                .match_id("DefinitionEntryBody::collapse-toggle-bar")
                .find_first()
                .expect("collapsed card must render collapse-toggle-bar in Slint UI");
            assert!(toggle_bar.size().height >= 20.0);
            let h_collapsed = ElementQuery::from_root(&app)
                .match_id("DefinitionEntryBody::rich-surface")
                .find_first()
                .expect("rich-surface must exist")
                .size()
                .height;
            let expand_touch = ElementQuery::from_root(&app)
                .match_id("DefinitionEntryBody::expand-touch")
                .find_first()
                .expect("expand-touch must exist");
            expand_touch.mock_single_click(slint::platform::PointerEventButton::Left);
            let h_expanded = ElementQuery::from_root(&app)
                .match_id("DefinitionEntryBody::rich-surface")
                .find_first()
                .expect("rich-surface must exist after expand")
                .size()
                .height;
            assert!(
                h_expanded > h_collapsed + 5.0,
                "clicking Show more must expand rich-surface height ({h_collapsed} -> {h_expanded})"
            );
            // Collapse back via click so subsequent checks start from collapsed state.
            let collapse_touch = ElementQuery::from_root(&app)
                .match_id("DefinitionEntryBody::expand-touch")
                .find_first()
                .expect("expand-touch must still exist for Show less");
            collapse_touch.mock_single_click(slint::platform::PointerEventButton::Left);
        }
        pb.invoke_toggle_definition_expanded(0, 1);
        let card1_exp = pb.invoke_render_glossary(
            1,
            260.0,
            true,
            pb.get_rich_revision(),
            pb.get_selection_revision(),
        );
        assert!(card1_exp.expanded);
        pb.invoke_toggle_definition_expanded(0, 1);
        let card1_col = pb.invoke_render_glossary(
            1,
            260.0,
            true,
            pb.get_rich_revision(),
            pb.get_selection_revision(),
        );
        assert!(!card1_col.expanded);

        // Expand again, then run a fresh lookup -> expand state must reset to collapsed!
        pb.invoke_toggle_definition_expanded(0, 1);
        let fresh_rows = memento_app::dictionary_result_rows(&hits, &[]);
        set_dictionary_rows(&app, fresh_rows);
        let card1_reset = pb.invoke_render_glossary(
            1,
            260.0,
            true,
            pb.get_rich_revision(),
            pb.get_selection_revision(),
        );
        assert!(
            !card1_reset.expanded,
            "per-card expand state must reset on a new lookup"
        );
    }

    #[test]
    fn deliverable4_anki_update_hook_first_term_ready_font_fallbacks_and_subtitle_line_spacing() {
        init_no_event_loop();
        let fixture = build_recursive_scan_fixture();
        let app = MementoApp::new().unwrap();
        let cfg = ConfigController::new();
        install_popup_navigation(&app);
        install_rich_ui_bridges(&app, &fixture.db_path, &fixture.config_dir, &cfg);
        app.window().set_size(slint::LogicalSize::new(1280., 720.));

        let pb = app.global::<PopupBridge>();

        // 1. first-term-ready hook fires on top-level term lookup, NOT on kanji-only lookups.
        let ready_events: Rc<std::cell::RefCell<Vec<(String, String)>>> =
            Rc::new(std::cell::RefCell::new(Vec::new()));
        let ready_cl = ready_events.clone();
        pb.on_first_term_ready(move |expr, reading| {
            ready_cl
                .borrow_mut()
                .push((expr.to_string(), reading.to_string()));
        });

        let mut dict = DictController::open(&fixture.db_path).unwrap();
        let hits = dict.search("大辞泉語").to_vec();
        let rows = memento_app::dictionary_result_rows(&hits, &[]);
        set_dictionary_rows(&app, rows);
        assert_eq!(
            ready_events.borrow().as_slice(),
            &[("大辞泉語".to_owned(), "だいじせんご".to_owned())],
            "first-term-ready must emit once for the first term result"
        );

        // Kanji-only rows must NOT trigger first-term-ready.
        ready_events.borrow_mut().clear();
        let kanji_row = DictResult {
            index: 1,
            expression: "世".into(),
            reading: "セイ".into(),
            is_kanji: true,
            term_start: true,
            term_end: true,
            ..Default::default()
        };
        set_dictionary_rows(&app, vec![kanji_row]);
        assert!(
            ready_events.borrow().is_empty(),
            "kanji-only lookup must not emit first-term-ready"
        );

        // History navigation Back restore must NOT re-emit first-term-ready.
        let rows_for_nav = memento_app::dictionary_result_rows(&hits, &[]);
        set_dictionary_rows(&app, rows_for_nav);
        app.set_dict_query("大辞泉語".into());
        app.set_dict_open(true);
        assert!(pb.invoke_activate_glossary_link("?query=リンク先".into()));
        assert!(pb.get_can_go_back());
        ready_events.borrow_mut().clear();
        pb.invoke_nav_back();
        assert_eq!(app.get_dict_query().as_str(), "大辞泉語");
        assert!(
            ready_events.borrow().is_empty(),
            "nav-back history restore must not re-emit first-term-ready"
        );

        // Restore term rows for Anki update button test.
        let rows = memento_app::dictionary_result_rows(&hits, &[]);
        set_dictionary_rows(&app, rows);
        app.set_anki_enabled(true);
        app.set_dict_query("大辞泉語".into());
        app.set_dict_open(true);

        // 2. Duplicate Update existing note button + row-anki-note-ids + lossless 64-bit note ID helper.
        let update_calls: Rc<std::cell::RefCell<Vec<(i32, i32, String, String, String)>>> =
            Rc::new(std::cell::RefCell::new(Vec::new()));
        let update_cl = update_calls.clone();
        pb.on_update_anki_note_requested(move |row_idx, note_id, expr, reading, gloss| {
            update_cl.borrow_mut().push((
                row_idx,
                note_id,
                expr.to_string(),
                reading.to_string(),
                gloss.to_string(),
            ));
        });

        let profile = memento_app::controller::config::AnkiNoteConfig {
            enabled: true,
            endpoint: memento_net::anki::AnkiEndpoint::default(),
            deck: "Japanese".to_owned(),
            model: "Sentence".to_owned(),
            field_templates: HashMap::new(),
            tags: vec!["memento".to_owned()],
            duplicate_policy: 1, // DifferentDeck -> marks duplicate
            screenshot_type: 0,
            audio_pad_start: 0.0,
            audio_pad_end: 0.0,
            audio_normalize: false,
            audio_db: -20.0,
            newline_replacer: "<br>".to_owned(),
        };
        memento_app::mark_popup_term_anki_added(&app, 0, 123_456_789, &profile, false);
        assert_eq!(pb.get_row_anki_duplicates().row_data(0), Some(true));
        assert_eq!(pb.get_row_anki_note_ids().row_data(0), Some(123_456_789));
        assert_eq!(
            memento_app::popup_row_anki_note_id_i64(&app, 0),
            Some(123_456_789)
        );
        app.show().unwrap();

        if std::env::var("SLINT_EMIT_DEBUG_INFO").as_deref() == Ok("1") {
            let update_btn = ElementQuery::from_root(&app)
                .match_id("YomitanPopup::update-anki-touch")
                .find_first()
                .expect("Update existing note button must render for duplicate rows with note_id > 0");
            update_btn.mock_single_click(slint::platform::PointerEventButton::Left);
            let calls = update_calls.borrow();
            assert_eq!(calls.len(), 1);
            assert_eq!(calls[0].0, 0);
            assert_eq!(calls[0].1, 123_456_789);
            assert_eq!(calls[0].2, "大辞泉語");
            assert_eq!(calls[0].3, "だいじせんご");
        }

        // Large 13-digit AnkiConnect epoch-ms note ID must clamp positive (i32::MAX) in Slint
        // while staying lossless via popup_row_anki_note_id_i64!
        let large_nid: i64 = 1_732_000_000_123;
        memento_app::mark_popup_term_anki_added(&app, 0, large_nid, &profile, false);
        assert_eq!(pb.get_row_anki_note_ids().row_data(0), Some(i32::MAX));
        assert_eq!(
            memento_app::popup_row_anki_note_id_i64(&app, 0),
            Some(large_nid)
        );

        // 3. Interface search-*-font fallbacks apply when popup.css doesn't override, and yield when popup.css does.
        cfg.edit_setting(
            "interface/search-expression-font",
            "IPAexMincho,21,-1,5,75,0,0,0,0,0",
        )
        .unwrap();
        cfg.edit_setting(
            "interface/search-reading-font",
            "IPAexGothic,11,-1,5,50,0,0,0,0,0",
        )
        .unwrap();
        cfg.edit_setting(
            "interface/search-conj-font",
            "DejaVu Sans,10,-1,5,50,0,0,0,0,0",
        )
        .unwrap();
        cfg.edit_setting(
            "interface/search-tag-font",
            "Liberation Sans,9,-1,5,75,0,0,0,0,0",
        )
        .unwrap();
        cfg.edit_setting(
            "interface/search-glossary-font",
            "Source Han Sans JP,12,-1,5,50,0,0,0,0,0",
        )
        .unwrap();
        cfg.edit_setting(
            "interface/search-kanji-font",
            "KanjiStrokeOrders,36,-1,5,50,0,0,0,0,0",
        )
        .unwrap();
        memento_app::refresh_rich_ui_bridges(&app, &cfg);

        assert_eq!(pb.get_css_headword_font_family().as_str(), "IPAexMincho");
        assert!((pb.get_css_headword_size() - 28.0).abs() < 0.2);
        assert_eq!(pb.get_css_reading_font_family().as_str(), "IPAexGothic");
        assert_eq!(pb.get_css_conj_font_family().as_str(), "DejaVu Sans");
        assert_eq!(pb.get_css_tag_font_family().as_str(), "Liberation Sans");
        assert_eq!(pb.get_css_font_family().as_str(), "Source Han Sans JP");
        assert!((pb.get_css_glossary_font_size() - 16.0).abs() < 0.2);
        assert_eq!(pb.get_css_kanji_font_family().as_str(), "KanjiStrokeOrders");
        assert!((pb.get_css_kanji_font_size() - 48.0).abs() < 0.2);

        // Explicit popup.css overrides must beat interface font fallbacks.
        fs::write(
            fixture.config_dir.join(memento_app::CUSTOM_POPUP_CSS_FILENAME),
            r#"
            :root { --font-family: "OverrideGlobal"; --glossary-font-size: 18px; }
            .headword { font-family: "OverrideHeadword"; font-size: 31px; }
            "#,
        )
        .unwrap();
        memento_app::refresh_rich_ui_bridges(&app, &cfg);
        assert_eq!(pb.get_css_font_family().as_str(), "OverrideGlobal");
        assert!((pb.get_css_glossary_font_size() - 18.0).abs() < 0.1);
        assert_eq!(pb.get_css_headword_font_family().as_str(), "OverrideHeadword");
        assert!((pb.get_css_headword_size() - 31.0).abs() < 0.1);

        // 4. Subtitle line spacing (interface/sub-line-spacing) increases rendered subtitle token height.
        let sb = app.global::<memento_app::SubtitleOverlayBridge>();
        let sample_word = memento_app::SubtitleWord {
            index: 0,
            text: "世界".into(),
            reading: "せかい".into(),
            query: "世界".into(),
        };
        cfg.edit_setting("interface/sub-line-spacing", "0.0")
            .unwrap();
        memento_app::refresh_rich_ui_bridges(&app, &cfg);
        let m0 = sb.invoke_render_token(
            sample_word.clone(),
            36.0,
            true,
            false,
            sb.get_style_revision(),
            sb.get_highlight_revision(),
        );
        cfg.edit_setting("interface/sub-line-spacing", "14.0")
            .unwrap();
        memento_app::refresh_rich_ui_bridges(&app, &cfg);
        let m14 = sb.invoke_render_token(
            sample_word,
            36.0,
            true,
            false,
            sb.get_style_revision(),
            sb.get_highlight_revision(),
        );
        assert!(
            m14.height_px >= m0.height_px + 13.0,
            "subtitle line-spacing=14 must increase rendered token height (m0={}, m14={})",
            m0.height_px,
            m14.height_px
        );
        assert!(
            m14.glyph_y_px >= m0.glyph_y_px + 6.0,
            "subtitle line-spacing must center glyphs cleanly (y0={}, y14={})",
            m0.glyph_y_px,
            m14.glyph_y_px
        );
    }
}

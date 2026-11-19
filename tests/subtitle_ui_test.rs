use memento_app::{apply_subtitle_furigana, publish_active_subtitle, MementoApp};
use slint::{ComponentHandle, Model};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    mpsc, Arc,
};

#[test]
fn outside_click_clears_selection_even_before_a_lookup_opens() {
    use std::{cell::Cell, rc::Rc};
    use slint::platform::{PointerEventButton, WindowEvent};
    i_slint_backend_testing::init_no_event_loop();
    let app = MementoApp::new().unwrap();
    app.window().set_size(slint::LogicalSize::new(1280., 720.));
    app.set_duration(120.);
    app.show().unwrap();
    assert_eq!(app.get_video_width(), 1280, "closed drawers must not reserve video space");
    assert_eq!(app.get_video_height(), 690, "closed drawers must not enlarge the window");
    let cancelled = Rc::new(Cell::new(0));
    let fullscreen = Rc::new(Cell::new(0));
    let toggles = fullscreen.clone();
    app.on_toggle_fullscreen(move || toggles.set(toggles.get() + 1));
    let counter = cancelled.clone();
    app.on_hover_word(move |word, _, _, _, _, _| {
        if word.is_empty() { counter.set(counter.get() + 1); }
    });
    let click = |x, y| {
        let position = slint::LogicalPosition::new(x, y);
        app.window().dispatch_event(WindowEvent::PointerPressed {
            position, button: PointerEventButton::Left,
        });
        app.window().dispatch_event(WindowEvent::PointerReleased {
            position, button: PointerEventButton::Left,
        });
    };
    for popup_open in [false, true] {
        app.set_selected_token_index(2);
        app.set_selected_token_count(3);
        app.set_dict_open(popup_open);
        let before = cancelled.get();
        click(50., 100.);
        assert_eq!(app.get_selected_token_index(), -1);
        assert_eq!(app.get_selected_token_count(), 1);
        assert!(!app.get_dict_open());
        assert!(cancelled.get() > before, "pending lookups must be cancelled");
    }
    assert_eq!(fullscreen.get(), 1, "double-clicking video must still toggle fullscreen");
    let (requests, _worker) = mpsc::channel();
    let epoch = Arc::new(AtomicU64::new(0));
    publish_active_subtitle(&app, "世界中", &requests, &epoch);
    memento_app::set_dictionary_rows(&app, vec![memento_app::DictResult {
        expression: "世界中".into(), reading: "せかいじゅう".into(),
        term_start: true, term_end: true, selected: true,
        ..Default::default()
    }]);
    app.set_dict_open(true);
    app.set_selected_token_index(0);
    // A click on its padding must preserve the term for audio and Anki.
    click(403., 200.);
    assert!(app.get_dict_open(), "popup padding click; video size={}x{}", app.get_video_width(), app.get_video_height());
    assert_eq!(app.get_selected_token_index(), 0);
    // Clicking the popup's top-right corner must not close it (the overlapping X close button was removed).
    click(848., 178.);
    assert!(app.get_dict_open(), "popup top-right must no longer have an X close button");
    assert_eq!(app.get_selected_token_index(), 0);
    // Pressing Escape must clear the selection as well as hide the popup.
    let esc = slint::SharedString::from(slint::platform::Key::Escape);
    app.window().dispatch_event(WindowEvent::KeyPressed { text: esc.clone() });
    app.window().dispatch_event(WindowEvent::KeyReleased { text: esc });
    assert!(!app.get_dict_open());
    assert_eq!(app.get_selected_token_index(), -1);

    app.set_selected_token_index(0);
    click(40., 685.);
    assert_eq!(app.get_selected_token_index(), -1, "OSC clicks must clear selection");

    // Opening a menu also clears the selection without swallowing the click.
    app.set_selected_token_index(0);
    click(30., 15.);
    assert_eq!(app.get_selected_token_index(), -1);
    assert!(app.get_menu_open());
    app.hide().unwrap();
}

#[test]
fn startup_furigana_preserves_base_text_selection_and_toggle_invalidates_old_work() {
    i_slint_backend_testing::init_no_event_loop();
    let app = MementoApp::new().unwrap();
    app.window().set_size(slint::LogicalSize::new(1280., 720.));
    app.set_furigana_visible(true);
    let (requests, worker) = mpsc::channel();
    let epoch = Arc::new(AtomicU64::new(0));
    let text = "世界中を旅行する";
    publish_active_subtitle(&app, text, &requests, &epoch);
    assert!(app.get_subtitle_furigana_pending());
    let first = worker.try_recv().unwrap();
    assert_eq!(first.1, text);
    app.set_selected_token_index(0);
    app.set_dict_query("世界中".into());
    app.set_dict_open(true);
    apply_subtitle_furigana(
        &app,
        text,
        vec![
            ("世界".into(), "せかい".into()),
            ("中".into(), "じゅう".into()),
            ("を".into(), "".into()),
            ("旅行".into(), "りょこう".into()),
            ("する".into(), "".into()),
        ],
    );
    assert!(!app.get_subtitle_furigana_pending());
    assert_eq!(app.get_selected_token_index(), 0);
    assert_eq!(app.get_selected_token_count(), 2);
    assert_eq!(
        app.get_active_sub_words()
            .iter()
            .map(|w| w.text.to_string())
            .collect::<String>(),
        text
    );
    assert_eq!(
        app.get_active_sub_words()
            .row_data(1)
            .unwrap()
            .query
            .as_str(),
        "中を旅行する"
    );

    app.set_furigana_visible(false);
    publish_active_subtitle(&app, text, &requests, &epoch);
    assert!(!app.get_subtitle_furigana_pending());
    assert!(epoch.load(Ordering::Acquire) > first.0);
    assert!(worker.try_recv().is_err());
    assert!(app
        .get_active_sub_words()
        .iter()
        .all(|w| w.reading.is_empty()));
    app.set_furigana_visible(true);
    publish_active_subtitle(&app, text, &requests, &epoch);
    assert!(app.get_subtitle_furigana_pending());
    assert!(worker.try_recv().unwrap().0 > first.0);

    publish_active_subtitle(&app, "", &requests, &epoch);
    assert!(!app.get_subtitle_furigana_pending());
    drop(worker);
    publish_active_subtitle(&app, text, &requests, &epoch);
    assert!(
        !app.get_subtitle_furigana_pending(),
        "a stopped worker must not hold playback paused"
    );
}

mod rich_subtitle_phase4_tests {
    use super::*;
    use memento_app::controller::config::ConfigController;
    use memento_app::controller::dict::SearchHit;
    use memento_app::{
        dictionary_result_rows, install_popup_navigation, install_rich_ui_bridges,
        set_dictionary_rows, subtitle_cluster_at_point, subtitle_lines_from_words,
        subtitle_words_from_parts, SubtitleOverlayBridge, SubtitleWord,
    };
    use memento_setting::keys as raw;
    use slint::platform::{Key, PointerEventButton, WindowEvent};
    use std::cell::RefCell;
    use std::fs;
    use std::path::PathBuf;
    use std::rc::Rc;
    use std::sync::atomic::AtomicUsize;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    static SUB_FIXTURE_SEQ: AtomicUsize = AtomicUsize::new(1);

    struct TempSubConfig {
        dir: PathBuf,
    }

    impl TempSubConfig {
        fn new() -> Self {
            let seq = SUB_FIXTURE_SEQ.fetch_add(1, Ordering::Relaxed);
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let dir = std::env::temp_dir().join(format!(
                "memento-p4-rich-sub-{}-{seq}-{nanos}",
                std::process::id()
            ));
            fs::create_dir_all(dir.join("dict_resources")).unwrap();
            Self { dir }
        }
    }

    impl Drop for TempSubConfig {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }

    #[test]
    fn vector_stroked_subtitles_render_and_hit_test_exact_per_glyph_clusters() {
        i_slint_backend_testing::init_no_event_loop();
        let fixture = TempSubConfig::new();
        let config = ConfigController::load(&fixture.dir, "memento.conf").unwrap();
        let db_path = fixture.dir.join("dictionaries.sqlite");

        let app = MementoApp::new().unwrap();
        app.window().set_size(slint::LogicalSize::new(1280., 720.));
        app.set_duration(120.);
        install_popup_navigation(&app);
        install_rich_ui_bridges(&app, &db_path, &fixture.dir, &config);
        app.show().unwrap();

        let sb = app.global::<SubtitleOverlayBridge>();
        assert!(sb.get_rich_enabled());

        let text = "世界中を旅行する";
        let (requests, _worker) = mpsc::channel();
        let epoch = Arc::new(AtomicU64::new(0));
        publish_active_subtitle(&app, text, &requests, &epoch);
        apply_subtitle_furigana(
            &app,
            text,
            vec![
                ("世界".into(), "せかい".into()),
                ("中".into(), "じゅう".into()),
                ("を".into(), "".into()),
                ("旅行".into(), "りょこう".into()),
                ("する".into(), "".into()),
            ],
        );

        let font_px = app.get_video_height() as f32 * app.get_subtitle_scale();
        let words: Vec<SubtitleWord> = app.get_active_sub_words().iter().collect();
        assert_eq!(words.len(), 5);
        assert_eq!(words[0].text.as_str(), "世界");

        // 1. Verify vector-stroked `parley` + `resvg::tiny_skia` rasterization on token `"世界"`
        let metrics = sb.invoke_render_token(words[0].clone(), font_px, false, false, 0, 0);
        assert!(metrics.width_px > font_px * 1.5, "2-kanji token must have shaped width");
        assert!(metrics.height_px >= font_px);
        let img_size = metrics.rendered.size();
        assert!(img_size.width > 40 && img_size.height > 30);

        // Also verify furigana increases rendered height cleanly
        let furi_metrics = sb.invoke_render_token(words[0].clone(), font_px, true, false, 0, 0);
        assert!(
            furi_metrics.height_px > metrics.height_px,
            "furigana token must reserve top ruby height ({} vs {})",
            furi_metrics.height_px,
            metrics.height_px
        );

        // 2. Verify `subtitle_cluster_at_point` distinguishes `'世'` (char 0) vs `'界'` (char 1) inside `"世界"`!
        let probe_y = metrics.glyph_y_px + metrics.glyph_height_px * 0.5;
        let left_x = metrics.width_px * 0.22;
        let right_x = metrics.width_px * 0.78;
        let (b0, c0) = subtitle_cluster_at_point(&words[0], font_px, false, left_x, probe_y)
            .expect("left half of 世界 should resolve cluster");
        let (b1, c1) = subtitle_cluster_at_point(&words[0], font_px, false, right_x, probe_y)
            .expect("right half of 世界 should resolve cluster");
        assert_eq!((b0, c0), (0, 0), "left side must hit '世' (byte 0, char 0)");
        assert_eq!(
            (b1, c1),
            ("世".len(), 1),
            "right side must hit '界' (byte 3, char 1)"
        );
        assert_eq!(
            sb.invoke_cluster_char_offset(words[0].clone(), font_px, false, left_x, probe_y),
            0
        );
        assert_eq!(
            sb.invoke_cluster_char_offset(words[0].clone(), font_px, false, right_x, probe_y),
            1
        );

        // 3. Verify multi-row wrapping still works cleanly with shaped `parley` widths
        let wrapped = subtitle_lines_from_words(
            subtitle_words_from_parts(
                "これはとても長い日本語の字幕テキストで複数行に折り返されます",
                vec![
                    ("これは".into(), "".into()),
                    ("とても".into(), "".into()),
                    ("長い".into(), "ながい".into()),
                    ("日本語の".into(), "にほんごの".into()),
                    ("字幕テキストで".into(), "".into()),
                    ("複数行に".into(), "ふくすうぎょうに".into()),
                    ("折り返されます".into(), "おりかえされます".into()),
                ],
            ),
            220.0,
            font_px,
            true,
        );
        assert!(wrapped.len() >= 2, "narrow max_width_px must wrap into multiple rows");

        // 4. Dispatch real window pointer clicks onto `'世'` vs `'界'` inside token 0 (`"世界"`)!
        let looked_up: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
        let looked_up_cb = looked_up.clone();
        let app_weak = app.as_weak();
        app.on_lookup_word(move |q| {
            looked_up_cb.borrow_mut().push(q.to_string());
            if let Some(app) = app_weak.upgrade() {
                let expr = if q.starts_with("世界") {
                    "世界中"
                } else {
                    "界"
                };
                let hits = vec![SearchHit {
                    expression: expr.to_owned(),
                    reading: "かい".to_owned(),
                    glossary: "world".to_owned(),
                    tags: String::new(),
                    score: 10,
                    is_kanji: false,
                    notes: String::new(),
                    example_ja: String::new(),
                    example_en: String::new(),
                    dict_name: "TestDict".to_owned(),
                    rich_glossary: Vec::new(),
                    derivation: String::new(),
                    derivation_descriptions: Vec::new(),
                    dictionary_id: 1,
                    rules: String::new(),
                    raw_glossary: r#"["world"]"#.to_owned(),
                    term_tags: String::new(),
                    def_tags: String::new(),
                }];
                let rows = dictionary_result_rows(&hits, &[]);
                set_dictionary_rows(&app, rows);
                app.set_dict_query(expr.into());
                app.set_dict_open(true);
            }
        });

        let all_metrics: Vec<_> = words
            .iter()
            .map(|w| sb.invoke_render_token(w.clone(), font_px, false, false, 0, 0))
            .collect();
        let line_w: f32 = all_metrics.iter().map(|m| m.width_px).sum();
        let line_h: f32 = all_metrics
            .iter()
            .map(|m| m.height_px)
            .fold(0.0f32, f32::max);
        let token0_win_x = 640.0 - line_w * 0.5;
        let token0_win_y = 650.0 - line_h;

        let click_at = |x: f32, y: f32| {
            let pos = slint::LogicalPosition::new(x, y);
            app.window().dispatch_event(WindowEvent::PointerMoved { position: pos });
            app.window().dispatch_event(WindowEvent::PointerPressed {
                position: pos,
                button: PointerEventButton::Left,
            });
            app.window().dispatch_event(WindowEvent::PointerReleased {
                position: pos,
                button: PointerEventButton::Left,
            });
        };

        // Click left half of `"世界"` (`'世'`) -> scans `"世界中を旅行する"`!
        click_at(token0_win_x + left_x, token0_win_y + probe_y);
        assert_eq!(looked_up.borrow().last().map(String::as_str), Some("世界中を旅行する"));

        // Click right half of `"世界"` (`'界'`) -> scans mid-token `"界中を旅行する"` and highlights ONLY `'界'`!
        click_at(token0_win_x + right_x, token0_win_y + probe_y);
        assert_eq!(
            looked_up.borrow().last().map(String::as_str),
            Some("界中を旅行する"),
            "clicking 2nd glyph inside multi-char token must scan starting at '界'"
        );
        let sub_sel_metrics = sb.invoke_render_token(
            words[0].clone(),
            font_px,
            false,
            true,
            sb.get_style_revision(),
            sb.get_highlight_revision(),
        );
        assert!(sub_sel_metrics.has_selection);
        assert!(
            sub_sel_metrics.sel_x_px > metrics.width_px * 0.35,
            "mid-token match on '界' must highlight only the second glyph (sel_x_px={})",
            sub_sel_metrics.sel_x_px
        );
        app.hide().unwrap();
    }

    #[test]
    fn stationary_shift_modifier_and_middle_mouse_scan_trigger_cluster_lookup() {
        i_slint_backend_testing::init_no_event_loop();
        let fixture = TempSubConfig::new();
        let config = ConfigController::load(&fixture.dir, "memento.conf").unwrap();
        // Configure `search/method = 1` (Modifier), `search/modifier = 2` (Shift), `search/middle_mouse = true`
        config
            .edit_setting(&format!("{}/{}", raw::search::GROUP, raw::search::METHOD), "1")
            .unwrap();
        config
            .edit_setting(
                &format!("{}/{}", raw::search::GROUP, raw::search::MODIFIER),
                "2",
            )
            .unwrap();
        config
            .edit_setting(
                &format!("{}/{}", raw::search::GROUP, raw::search::MIDDLE_MOUSE_SCAN),
                "true",
            )
            .unwrap();
        config.save(&fixture.dir, "memento.conf").unwrap();

        let db_path = fixture.dir.join("dictionaries.sqlite");
        let app = MementoApp::new().unwrap();
        app.window().set_size(slint::LogicalSize::new(1280., 720.));
        app.set_duration(120.);
        install_popup_navigation(&app);
        install_rich_ui_bridges(&app, &db_path, &fixture.dir, &config);
        app.show().unwrap();

        let sb = app.global::<SubtitleOverlayBridge>();
        assert!(sb.get_middle_mouse_scan_enabled());

        let text = "世界中を旅行する";
        let (requests, _worker) = mpsc::channel();
        let epoch = Arc::new(AtomicU64::new(0));
        publish_active_subtitle(&app, text, &requests, &epoch);
        apply_subtitle_furigana(
            &app,
            text,
            vec![
                ("世界".into(), "".into()),
                ("中".into(), "".into()),
                ("を".into(), "".into()),
                ("旅行".into(), "".into()),
                ("する".into(), "".into()),
            ],
        );

        let looked_up: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
        let looked_up_cb = looked_up.clone();
        app.on_lookup_word(move |q| {
            looked_up_cb.borrow_mut().push(q.to_string());
        });

        let font_px = app.get_video_height() as f32 * app.get_subtitle_scale();
        let words: Vec<SubtitleWord> = app.get_active_sub_words().iter().collect();
        let all_metrics: Vec<_> = words
            .iter()
            .map(|w| sb.invoke_render_token(w.clone(), font_px, false, false, 0, 0))
            .collect();
        let line_w: f32 = all_metrics.iter().map(|m| m.width_px).sum();
        let line_h: f32 = all_metrics
            .iter()
            .map(|m| m.height_px)
            .fold(0.0f32, f32::max);
        let line_start_x = 640.0 - line_w * 0.5;
        let line_start_y = 650.0 - line_h;

        // Token 3 is `"旅行"`; compute its window X offset:
        let token3_x = line_start_x + all_metrics[0..3].iter().map(|m| m.width_px).sum::<f32>();
        let probe_y = line_start_y
            + all_metrics[3].glyph_y_px
            + all_metrics[3].glyph_height_px * 0.5;

        // 1. Move mouse over `'旅'` (left half of `"旅行"`) WITHOUT Shift held -> NO lookup should fire!
        let pos_tabi = slint::LogicalPosition::new(token3_x + all_metrics[3].width_px * 0.22, probe_y);
        app.window()
            .dispatch_event(WindowEvent::PointerMoved { position: pos_tabi });
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(300));
        assert!(
            looked_up.borrow().is_empty(),
            "Modifier mode must not scan on plain mouse hover without Shift"
        );
        assert_eq!(sb.get_hovered_query().as_str(), "旅行する");

        // 2. Press stationary `Shift` (and `ShiftR`) WITHOUT moving the mouse -> immediately scans `"旅行する"`!
        app.window().dispatch_event(WindowEvent::KeyPressed {
            text: Key::Shift.into(),
        });
        app.window().dispatch_event(WindowEvent::KeyReleased {
            text: Key::Shift.into(),
        });
        assert_eq!(
            looked_up.borrow().last().map(String::as_str),
            Some("旅行する"),
            "stationary Left Shift press over hovered cluster must trigger scan"
        );

        // Move onto `'行'` (right half of `"旅行"`) without Shift -> no scan until stationary Right Shift (`Key::ShiftR`)!
        let pos_kou = slint::LogicalPosition::new(token3_x + all_metrics[3].width_px * 0.78, probe_y);
        app.window()
            .dispatch_event(WindowEvent::PointerMoved { position: pos_kou });
        assert_eq!(looked_up.borrow().len(), 1);
        app.window().dispatch_event(WindowEvent::KeyPressed {
            text: Key::ShiftR.into(),
        });
        app.window().dispatch_event(WindowEvent::KeyReleased {
            text: Key::ShiftR.into(),
        });
        assert_eq!(
            looked_up.borrow().last().map(String::as_str),
            Some("行する"),
            "stationary Right Shift press over 2nd glyph must scan '行する'"
        );

        // 3. Middle-mouse scan (#285): move onto `'界'` in token 0 without Shift and middle-click!
        let pos_kai = slint::LogicalPosition::new(line_start_x + all_metrics[0].width_px * 0.78, probe_y);
        app.window()
            .dispatch_event(WindowEvent::PointerMoved { position: pos_kai });
        assert_eq!(looked_up.borrow().len(), 2);
        app.window().dispatch_event(WindowEvent::PointerPressed {
            position: pos_kai,
            button: PointerEventButton::Middle,
        });
        app.window().dispatch_event(WindowEvent::PointerReleased {
            position: pos_kai,
            button: PointerEventButton::Middle,
        });
        assert_eq!(
            looked_up.borrow().last().map(String::as_str),
            Some("界中を旅行する"),
            "middle-mouse click (#285) must scan exact hovered glyph cluster"
        );

        // When `middle_mouse_scan_enabled` is disabled, middle-click does NOT scan:
        sb.set_middle_mouse_scan_enabled(false);
        let count_before = looked_up.borrow().len();
        app.window().dispatch_event(WindowEvent::PointerPressed {
            position: pos_tabi,
            button: PointerEventButton::Middle,
        });
        app.window().dispatch_event(WindowEvent::PointerReleased {
            position: pos_tabi,
            button: PointerEventButton::Middle,
        });
        assert_eq!(looked_up.borrow().len(), count_before);
        app.hide().unwrap();
    }

    #[test]
    fn custom_subtitle_style_rewraps_lines_and_preserves_focus_and_single_key_dispatch() {
        i_slint_backend_testing::init_no_event_loop();
        let fixture = TempSubConfig::new();
        let config = ConfigController::load(&fixture.dir, "memento.conf").unwrap();
        // Configure heavy stroke (`24.0`),Modifier mode (`Shift`), and save
        config
            .edit_setting(
                &format!("{}/{}", raw::interface::GROUP, raw::interface::subtitle::STROKE),
                "24.0",
            )
            .unwrap();
        config
            .edit_setting(&format!("{}/{}", raw::search::GROUP, raw::search::METHOD), "1")
            .unwrap();
        config
            .edit_setting(
                &format!("{}/{}", raw::search::GROUP, raw::search::MODIFIER),
                "2",
            )
            .unwrap();
        config.save(&fixture.dir, "memento.conf").unwrap();

        let db_path = fixture.dir.join("dictionaries.sqlite");
        let app = MementoApp::new().unwrap();
        app.window().set_size(slint::LogicalSize::new(360., 720.));
        app.set_duration(120.);
        install_popup_navigation(&app);
        install_rich_ui_bridges(&app, &db_path, &fixture.dir, &config);
        app.show().unwrap();

        let sb = app.global::<SubtitleOverlayBridge>();
        let pb = app.global::<memento_app::PopupBridge>();

        let text = "世界中を旅行する物語";
        let (requests, _worker) = mpsc::channel();
        let epoch = Arc::new(AtomicU64::new(0));
        publish_active_subtitle(&app, text, &requests, &epoch);

        // 1. `subtitle_token_width` must match `render_token.width_px` using the bridge's custom stroke style!
        let font_px = app.get_video_height() as f32 * app.get_subtitle_scale();
        let word0 = app.get_active_sub_words().row_data(0).unwrap();
        let rendered0 = sb.invoke_render_token(word0.clone(), font_px, false, false, 0, 0);
        let measured0 = memento_app::subtitle_token_width(&word0, font_px, false);
        assert!(
            (rendered0.width_px - measured0).abs() < 0.01,
            "subtitle_token_width ({measured0}) must use active custom SubtitleRenderStyle ({})",
            rendered0.width_px
        );

        // 2. Verify `should_capture_hover_focus` respects menus, OCR, about, drawers, and active glossary text selection!
        assert!(sb.invoke_should_capture_hover_focus());
        app.set_ocr_mode(true);
        assert!(!sb.invoke_should_capture_hover_focus());
        app.set_ocr_mode(false);
        app.set_about_open(true);
        assert!(!sb.invoke_should_capture_hover_focus());
        app.set_about_open(false);
        app.set_dict_open(true);
        pb.set_selected_glossary_text("selected meaning".into());
        assert!(
            !sb.invoke_should_capture_hover_focus(),
            "active glossary text selection must keep focus for Ctrl+C"
        );
        pb.set_selected_glossary_text("".into());
        app.set_dict_open(false);
        assert!(sb.invoke_should_capture_hover_focus());

        // 3. Verify `popup_key_pressed` forwards stationary Shift to `SubtitleOverlayBridge` AND
        //    `hover_key_pressed` / `popup_key_pressed` reject non-modifiers (including `CapsLock` and `"a"`)
        //    without calling `app.key_pressed` directly (preventing double dispatch!).
        let key_calls = Rc::new(std::cell::Cell::new(0));
        let key_calls_cb = key_calls.clone();
        app.on_key_pressed(move |_, _, _, _, _| {
            key_calls_cb.set(key_calls_cb.get() + 1);
            false
        });
        assert!(!sb.invoke_hover_key_pressed(Key::CapsLock.into(), false, false, false, false));
        assert!(!sb.invoke_hover_key_pressed("a".into(), false, false, false, false));
        assert!(!pb.invoke_popup_key_pressed("a".into(), false, false, false, false));
        assert_eq!(
            key_calls.get(),
            0,
            "child FocusScopes must reject non-modifiers without invoking app.key_pressed directly"
        );

        // Hover a token, then simulate `glossary-focus` receiving `Shift` via `popup_key_pressed` -> stationary Shift still scans!
        let looked_up: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
        let looked_up_cb = looked_up.clone();
        app.on_lookup_word(move |q| looked_up_cb.borrow_mut().push(q.to_string()));
        sb.invoke_pointer_hovered(
            word0,
            font_px,
            false,
            rendered0.width_px * 0.25,
            rendered0.glyph_y_px + 4.0,
            false,
            false,
            false,
            false,
        );
        assert!(looked_up.borrow().is_empty());
        assert!(pb.invoke_popup_key_pressed(Key::Shift.into(), false, true, false, false));
        assert_eq!(
            looked_up.borrow().last().map(String::as_str),
            Some("世界中を旅行する物語"),
            "Shift pressed while glossary-focus holds focus must forward to stationary_modifier_changed"
        );
        app.hide().unwrap();
    }

    #[test]
    fn subtitle_stroke_does_not_inflate_spacing_and_popup_anchor_does_not_chase_mouse() {
        i_slint_backend_testing::init_no_event_loop();
        let fixture = TempSubConfig::new();
        let config = ConfigController::load(&fixture.dir, "memento.conf").unwrap();
        // Match heavy stroke (`15.0`), transparent bg, Modifier mode (`Shift`)
        config
            .edit_setting(
                &format!("{}/{}", raw::interface::GROUP, raw::interface::subtitle::STROKE),
                "15.0",
            )
            .unwrap();
        config
            .edit_setting(
                &format!("{}/{}", raw::interface::GROUP, raw::interface::subtitle::BACKGROUND_COLOR),
                "#00000000",
            )
            .unwrap();
        config
            .edit_setting(&format!("{}/{}", raw::search::GROUP, raw::search::METHOD), "1")
            .unwrap();
        config
            .edit_setting(
                &format!("{}/{}", raw::search::GROUP, raw::search::MODIFIER),
                "2",
            )
            .unwrap();
        config.save(&fixture.dir, "memento.conf").unwrap();

        let word = memento_app::SubtitleWord {
            text: "私".into(),
            reading: "".into(),
            query: "私は違う".into(),
            index: 0,
        };
        let mut no_stroke = memento_app::SubtitleRenderStyle::default();
        no_stroke.stroke_size = 0.0;
        let mut heavy_stroke = memento_app::SubtitleRenderStyle::default();
        heavy_stroke.stroke_size = 15.0;
        let w_no = memento_app::subtitle_token_width_with_style(&word, 40.0, false, &no_stroke);
        let w_heavy = memento_app::subtitle_token_width_with_style(&word, 40.0, false, &heavy_stroke);
        assert!(
            (w_no - w_heavy).abs() < 0.01,
            "heavy subtitle stroke must bleed visually instead of adding gaps between characters (no_stroke={w_no}, heavy_stroke={w_heavy})"
        );

        let db_path = fixture.dir.join("dictionaries.sqlite");
        let app = MementoApp::new().unwrap();
        app.window().set_size(slint::LogicalSize::new(1280., 720.));
        app.set_duration(120.);
        app.set_subtitle_scale(0.04);
        install_popup_navigation(&app);
        install_rich_ui_bridges(&app, &db_path, &fixture.dir, &config);
        app.show().unwrap();

        let (requests, _worker) = mpsc::channel();
        let epoch = Arc::new(AtomicU64::new(0));
        let sentence = "私は違うけど、そんな私を死と呼んでいいの？";
        publish_active_subtitle(&app, sentence, &requests, &epoch);
        assert_eq!(
            app.get_active_sub_lines().row_count(),
            1,
            "21-char Japanese subtitle must fit cleanly on 1 line at 1280px without stroke-gap wrapping"
        );

        let sb = app.global::<SubtitleOverlayBridge>();
        assert_eq!(sb.get_subtitle_bg_color().alpha(), 0);
        let first_word = app.get_active_sub_words().row_data(0).unwrap();
        let second_word = app.get_active_sub_words().row_data(1).unwrap();
        let m0 = sb.invoke_render_token(first_word.clone(), 40.0, false, false, 0, 0);
        assert!(m0.bleed_x_px > 2.0, "heavy stroke must reserve bleed_x_px ({})", m0.bleed_x_px);

        // Multi-character word selection (`私は`, tokens 0..2) must produce a continuous
        // blue bar across adjacent tokens with zero gap or inner corner seams!
        app.set_dict_open(true);
        app.set_selected_token_index(0);
        app.set_selected_token_count(2);
        let sel0 = sb.invoke_render_token(first_word, 40.0, false, true, 0, 1);
        let sel1 = sb.invoke_render_token(second_word, 40.0, false, true, 0, 1);
        assert!(sel0.has_selection && sel1.has_selection);
        assert!(!sel0.sel_join_left && sel0.sel_join_right);
        assert!(sel1.sel_join_left && !sel1.sel_join_right);
        assert!(
            sel0.sel_x_px + sel0.sel_width_px >= sel0.width_px,
            "first selected token must extend flush to its right edge ({} + {} vs {})",
            sel0.sel_x_px,
            sel0.sel_width_px,
            sel0.width_px
        );
        assert!(
            sel1.sel_x_px.abs() < 0.01,
            "continuing selected token must start flush at x=0 ({})",
            sel1.sel_x_px
        );

        // Moving the mouse across subtitles while popup is open (without triggering a new scan)
        // must NOT drag `popup_offset_x` along with the cursor!
        app.set_popup_offset_x(-140.0);
        app.set_pending_popup_offset_x(220.0);
        let probe_y = 720.0 - 62.0 - 20.0;
        for x in [450.0, 620.0, 810.0] {
            app.window().dispatch_event(WindowEvent::PointerMoved {
                position: slint::LogicalPosition::new(x, probe_y),
            });
        }
        assert!(
            (app.get_popup_offset_x() - (-140.0)).abs() < 0.01,
            "moving mouse across subtitles must NOT drag open popup (popup_offset_x={})",
            app.get_popup_offset_x()
        );

        app.hide().unwrap();
    }

    #[test]
    fn native_ass_overlay_anchors_popup_passes_outside_clicks_and_switches_tracks_cleanly() {
        use memento_app::controller::ass::AssOverlayController;
        use memento_ass::MpvAssParams;
        use slint::platform::{PointerEventButton, WindowEvent};
        use std::cell::Cell;

        i_slint_backend_testing::init_no_event_loop();
        let app = MementoApp::new().unwrap();
        app.window().set_size(slint::LogicalSize::new(1280.0, 750.0));
        app.set_duration(120.0);
        app.set_osc_visible(false);
        app.set_osc_fade_ms(0);
        app.show().unwrap();
        assert_eq!(app.get_video_width(), 1280);
        assert_eq!(app.get_video_height(), 720);

        let video_clicks = Rc::new(Cell::new(0));
        let vc = video_clicks.clone();
        app.on_video_pointer_button(move |_, _, btn, pressed, _, _, _, _| {
            if btn == 1 && pressed {
                vc.set(vc.get() + 1);
            }
        });

        let header = "\
[Script Info]
ScriptType: v4.00+
PlayResX: 1280
PlayResY: 720
ScaledBorderAndShadow: yes

[V4+ Styles]
Format: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding
Style: Default,Noto Sans CJK JP,48,&H00FFFFFF,&H000000FF,&H00000000,&H64000000,1,0,0,0,100,100,2,0,1,2,1,2,20,20,20,1
";
        // Two simultaneous speakers: Event 0 near TOP (y=60), Event 1 near BOTTOM (y=660).
        let ass_full = "\
Dialogue: 0,0:00:10.00,0:00:15.00,Default,,0,0,0,,{\\pos(640,80)\\an8}上部テロップ\n\
Dialogue: 0,0:00:10.00,0:00:15.00,Default,,0,0,0,,{\\pos(640,660)\\an2}鳥白島に到着\n";

        let mut ass = AssOverlayController::new();
        let params = MpvAssParams::for_frame(1280, 720);
        ass.sync_from_playback(&app, true, header, ass_full, &params, 12_000, true, 0.4);

        let sb = app.global::<SubtitleOverlayBridge>();
        assert!(sb.get_ass_native_active());
        assert_eq!(sb.get_ass_line_hitboxes().row_count(), 2);
        assert!(app.get_primary_subtitle_effective_visible());

        // Clicking empty center video (far from top y=80 and bottom y=660) MUST pass straight to video-motion!
        let mid_pos = slint::LogicalPosition::new(640.0, 390.0); // 30px menu + 360px video center
        app.window().dispatch_event(WindowEvent::PointerPressed {
            position: mid_pos,
            button: PointerEventButton::Left,
        });
        app.window().dispatch_event(WindowEvent::PointerReleased {
            position: mid_pos,
            button: PointerEventButton::Left,
        });
        assert_eq!(
            video_clicks.get(),
            1,
            "clicks outside ASS line hitboxes must pass through to video-motion"
        );

        // Highlight bottom event ("鳥白島") -> YomitanPopup must anchor ABOVE the bottom line!
        let hb_bottom = sb.get_ass_line_hitboxes().row_data(1).unwrap();
        let hit_bottom = ass
            .hit_test_logical(&app, hb_bottom.x + 20.0, hb_bottom.y + hb_bottom.height * 0.5)
            .expect("hit bottom ASS line");
        assert_eq!(hit_bottom.event_index, 1);
        assert!(hit_bottom.scan_substring.starts_with("鳥白島"));
        ass.apply_lookup_highlight(&app, &hit_bottom, 3);
        app.set_dict_open(true);
        assert_eq!(sb.get_ass_highlight_patches().row_count(), 1);
        assert!(sb.get_ass_highlight_patches().row_data(0).unwrap().has_patch);
        assert!(sb.get_ass_anchor_active());
        assert!(
            app.get_popup_y() + app.get_popup_height() <= sb.get_ass_anchor_top() + 1.0,
            "popup for bottom ASS line must sit above anchor (popup_bottom={}, anchor_top={})",
            app.get_popup_y() + app.get_popup_height(),
            sb.get_ass_anchor_top()
        );

        // Highlight top event ("上部テロップ" at y=80) -> YomitanPopup must flip BELOW the top line!
        let hb_top = sb.get_ass_line_hitboxes().row_data(0).unwrap();
        let hit_top = ass
            .hit_test_logical(&app, hb_top.x + 20.0, hb_top.y + hb_top.height * 0.5)
            .expect("hit top ASS line");
        assert_eq!(hit_top.event_index, 0);
        assert_eq!(hit_top.event_plain_text, "上部テロップ");
        ass.apply_lookup_highlight(&app, &hit_top, 2);
        assert!(
            app.get_popup_y() >= sb.get_ass_anchor_bottom() - 1.0,
            "popup for top-screen ASS line must automatically flip BELOW anchor (popup_y={}, anchor_bottom={})",
            app.get_popup_y(),
            sb.get_ass_anchor_bottom()
        );

        // Ctrl+0 (`subtitles-visible = false`) hides the ASS overlay cleanly.
        app.set_subtitles_visible(false);
        assert!(!app.get_primary_subtitle_effective_visible());
        app.set_subtitles_visible(true);
        assert!(app.get_primary_subtitle_effective_visible());

        // Switching track ASS -> SRT (`native_ass_active = false`) clears all ASS overlay state.
        ass.sync_from_playback(&app, false, "", "", &params, 12_000, true, 0.4);
        assert!(!sb.get_ass_native_active());
        assert_eq!(sb.get_ass_line_hitboxes().row_count(), 0);
        assert_eq!(sb.get_ass_highlight_patches().row_count(), 0);
        assert!(!sb.get_ass_anchor_active());
        assert!(ass.active_lookup().is_none());

        app.hide().unwrap();
    }
}

mod ui_dialogs_settings_and_modifier_tests {
    use super::*;
    use i_slint_backend_testing::ElementQuery;
    use memento_app::controller::config::ConfigController;
    use memento_app::{PopupBridge, SettingsField, SettingsGroup, SettingsWindow, SubEntry};
    use slint::platform::{Key, WindowEvent};
    use slint::VecModel;
    use std::cell::RefCell;
    use std::rc::Rc;

    #[test]
    fn update_dialog_buttons_stay_strictly_inside_card_with_generous_bottom_margin() {
        i_slint_backend_testing::init_no_event_loop();
        let app = MementoApp::new().unwrap();
        app.window().set_size(slint::LogicalSize::new(1280.0, 720.0));
        app.show().unwrap();

        // 1. First-launch Auto Update opt-in dialog (`No` / `Yes`)
        app.set_update_dialog_opt_in(true);
        app.set_update_dialog_title("Auto Update".into());
        app.set_update_dialog_message(
            "Do you want to check for updates on launch?\n\nThis can be changed at any time in Settings → Options → Application.\nThis will query GitHub's releases API when Memento starts."
                .into(),
        );
        app.set_update_dialog_release_tag("".into());
        app.set_update_dialog_release_url("https://github.com/ripose-jp/Memento/releases".into());
        app.set_update_dialog_open(true);

        let assert_buttons_inside_card = |label: &str, button_ids: &[&str]| {
            let card = ElementQuery::from_root(&app)
                .match_id("MementoApp::update-card")
                .find_first()
                .unwrap_or_else(|| panic!("{label}: update-card must exist (run with SLINT_EMIT_DEBUG_INFO=1)"));
            let card_pos = card.absolute_position();
            let card_size = card.size();
            let card_left = card_pos.x;
            let card_right = card_pos.x + card_size.width;
            let card_bottom = card_pos.y + card_size.height;

            let msg = ElementQuery::from_root(&app)
                .match_id("MementoApp::update-message-text")
                .find_first()
                .unwrap_or_else(|| panic!("{label}: update-message-text must exist"));
            let msg_pos = msg.absolute_position();
            let msg_size = msg.size();
            let msg_bottom = msg_pos.y + msg_size.height;
            assert!(
                msg_pos.x >= card_left + 16.0 && msg_pos.x + msg_size.width <= card_right - 16.0,
                "{label}: message text horizontally overflows card (msg={}..{}, card={}..{})",
                msg_pos.x,
                msg_pos.x + msg_size.width,
                card_left,
                card_right
            );

            for &btn_id in button_ids {
                let btn = ElementQuery::from_root(&app)
                    .match_id(btn_id)
                    .find_first()
                    .unwrap_or_else(|| panic!("{label}: {btn_id} must exist"));
                let pos = btn.absolute_position();
                let size = btn.size();
                let btn_right = pos.x + size.width;
                let btn_bottom = pos.y + size.height;
                let bottom_margin = card_bottom - btn_bottom;
                assert!(
                    pos.y >= msg_bottom + 8.0,
                    "{label}: {btn_id} overlaps message text (btn_top={}, msg_bottom={msg_bottom})",
                    pos.y
                );
                assert!(
                    pos.x >= card_left + 16.0 && btn_right <= card_right - 16.0,
                    "{label}: {btn_id} overflows card horizontally (btn={}..{}, card={}..{})",
                    pos.x,
                    btn_right,
                    card_left,
                    card_right
                );
                assert!(
                    bottom_margin >= 16.0,
                    "{label}: {btn_id} must stay strictly inside update-card with >= 16px bottom margin (btn_bottom={btn_bottom}, card_bottom={card_bottom}, margin={bottom_margin})"
                );
                assert!(
                    size.width >= 84.0 && size.height >= 30.0,
                    "{label}: {btn_id} must be well-proportioned ({}x{})",
                    size.width,
                    size.height
                );
            }
        };

        assert_buttons_inside_card(
            "opt-in",
            &["MementoApp::update-btn-no", "MementoApp::update-btn-yes"],
        );

        // Even with a very long multi-paragraph message, update-card must expand cleanly.
        app.set_update_dialog_message(
            "Do you want to check for updates on launch?\n\nThis can be changed at any time in Settings → Options → Application.\nThis will query GitHub's releases API when Memento starts and notify you whenever a newer release tag is published on GitHub Releases."
                .into(),
        );
        assert_buttons_inside_card(
            "opt-in-long",
            &["MementoApp::update-btn-no", "MementoApp::update-btn-yes"],
        );

        // 2. Release-available dialog (`Open GitHub Releases` / `Dismiss`)
        app.set_update_dialog_opt_in(false);
        app.set_update_dialog_title("New Version Available".into());
        app.set_update_dialog_release_tag("v2.1.0".into());
        app.set_update_dialog_message(
            "Memento v2.1.0 is available on GitHub Releases (currently installed: v2.0.0). Visit GitHub Releases to download the latest build and review the changelog."
                .into(),
        );
        assert_buttons_inside_card(
            "release-available",
            &[
                "MementoApp::update-btn-releases",
                "MementoApp::update-btn-dismiss",
            ],
        );

        app.hide().unwrap();
    }

    #[test]
    fn settings_window_multiline_descriptions_stay_strictly_inside_row_and_card() {
        i_slint_backend_testing::init_no_event_loop();
        let cfg = ConfigController::new();

        // Test both Options -> Interface (category 5, with ASS native 2-3 line description)
        // and Options -> Search (category 9, with max-recursion-depth description), plus a
        // synthetic 3+ line stress description at normal and narrow window widths.
        let settings = SettingsWindow::new().unwrap();
        settings.show().unwrap();

        let assert_all_descriptions_contained = |label: &str| {
            let cards = ElementQuery::from_root(&settings)
                .match_id("SettingsSection::settings-card")
                .find_all();
            let rows = ElementQuery::from_root(&settings)
                .match_id("SettingsSection::field-row")
                .find_all();
            let descs = ElementQuery::from_root(&settings)
                .match_id("FieldRow::field-desc")
                .find_all();
            assert!(
                !cards.is_empty() && !rows.is_empty() && !descs.is_empty(),
                "{label}: expected settings-card, field-row, and field-desc elements (cards={}, rows={}, descs={})",
                cards.len(),
                rows.len(),
                descs.len(),
            );

            for (idx, desc) in descs.iter().enumerate() {
                let dp = desc.absolute_position();
                let ds = desc.size();
                let d_top = dp.y;
                let d_bottom = dp.y + ds.height;
                let d_left = dp.x;
                let d_right = dp.x + ds.width;
                assert!(
                    ds.height > 10.0 && ds.width > 100.0,
                    "{label}: desc[{idx}] has degenerate size {}x{}",
                    ds.width,
                    ds.height
                );

                // Must be strictly contained inside its owning FieldRow
                let owner_row = rows.iter().find(|r| {
                    let rp = r.absolute_position();
                    let rs = r.size();
                    rs.height > 0.0 && d_top >= rp.y - 0.5 && d_top < rp.y + rs.height
                });
                let row = owner_row.unwrap_or_else(|| {
                    panic!("{label}: desc[{idx}] top={d_top} has no containing field-row")
                });
                let rp = row.absolute_position();
                let rs = row.size();
                let row_bottom = rp.y + rs.height;
                assert!(
                    d_bottom <= row_bottom - 4.0,
                    "{label}: desc[{idx}] overflows FieldRow bottom (desc_bottom={d_bottom}, row_bottom={row_bottom}, desc_h={}, row_h={})",
                    ds.height,
                    rs.height
                );
                assert!(
                    d_left >= rp.x - 0.5 && d_right <= rp.x + rs.width + 0.5,
                    "{label}: desc[{idx}] overflows FieldRow horizontally (desc={}..{}, row={}..{})",
                    d_left,
                    d_right,
                    rp.x,
                    rp.x + rs.width
                );

                // Must also be strictly contained inside its parent settings-card
                let owner_card = cards.iter().find(|c| {
                    let cp = c.absolute_position();
                    let cs = c.size();
                    d_top >= cp.y - 0.5 && d_top < cp.y + cs.height
                });
                let card = owner_card.unwrap_or_else(|| {
                    panic!("{label}: desc[{idx}] top={d_top} has no containing settings-card")
                });
                let cp = card.absolute_position();
                let cs = card.size();
                let card_bottom = cp.y + cs.height;
                assert!(
                    row_bottom <= card_bottom - 4.0,
                    "{label}: FieldRow overflows settings-card bottom (row_bottom={row_bottom}, card_bottom={card_bottom})"
                );
                assert!(
                    d_bottom <= card_bottom - 8.0,
                    "{label}: desc[{idx}] overflows settings-card bottom (desc_bottom={d_bottom}, card_bottom={card_bottom})"
                );
            }
        };

        for (w, h) in [(940.0f32, 1600.0f32), (760.0, 1600.0)] {
            settings.window().set_size(slint::LogicalSize::new(w, h));
            settings.set_active_category(5);
            settings.set_settings_groups(
                Rc::new(VecModel::from(cfg.category_settings_groups(5))).into(),
            );
            assert_all_descriptions_contained(&format!("interface-{w}x{h}"));

            // Verify the real ASS native description wrapped onto multiple lines and expanded the row
            let descs = ElementQuery::from_root(&settings)
                .match_id("FieldRow::field-desc")
                .find_all();
            assert!(
                descs.len() >= 2,
                "interface-{w}x{h}: expected Theme + Subtitle descriptions visible (got {})",
                descs.len()
            );
            assert!(
                descs.iter().any(|d| d.size().height > 20.0),
                "interface-{w}x{h}: ASS native description should wrap across multiple lines (descs={:?})",
                descs.iter().map(|d| (d.absolute_position(), d.size())).collect::<Vec<_>>()
            );

            // Also verify the Subtitle group specifically at typical 680px window height
            let subtitle_only: Vec<_> = cfg
                .category_settings_groups(5)
                .into_iter()
                .filter(|g| g.title == "Subtitle")
                .collect();
            settings.window().set_size(slint::LogicalSize::new(w, 680.0));
            settings.set_settings_groups(Rc::new(VecModel::from(subtitle_only)).into());
            // Scroll the settings body so the bottom ASS-native description row is inside the 680px viewport
            // (FontPickerControl is a 2-row 260px stacked control so the 10-row Subtitle card scrolls at 680px).
            i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(16));
            let scroll_pos = slint::LogicalPosition::new(w * 0.6, 340.0);
            settings.window().dispatch_event(
                slint::platform::WindowEvent::PointerMoved {
                    position: scroll_pos,
                },
            );
            settings.window().dispatch_event(
                slint::platform::WindowEvent::PointerScrolled {
                    position: scroll_pos,
                    delta_x: 0.0,
                    delta_y: -160.0,
                },
            );
            assert_all_descriptions_contained(&format!("interface-subtitle-only-{w}x680"));
            // Restore scroll position to top before next category/width iteration.
            settings.window().dispatch_event(
                slint::platform::WindowEvent::PointerScrolled {
                    position: scroll_pos,
                    delta_x: 0.0,
                    delta_y: 160.0,
                },
            );

            settings.window().set_size(slint::LogicalSize::new(w, 1600.0));
            settings.set_active_category(9);
            settings.set_settings_groups(
                Rc::new(VecModel::from(cfg.category_settings_groups(9))).into(),
            );
            assert_all_descriptions_contained(&format!("search-{w}x{h}"));
        }

        // Stress test: 1-line, 2-line, and 3+ line descriptions in one card
        let make_field = |key: &str, label: &str, desc: &str, kind: &str| SettingsField {
            key: key.into(),
            label: label.into(),
            description: desc.into(),
            kind: kind.into(),
            value: if kind == "switch" { "true" } else { "4" }.into(),
            choices: Rc::new(VecModel::default()).into(),
            enabled: true,
            visible: true,
            font_family: "".into(),
            font_size: "".into(),
            font_bold: false,
            font_italic: false,
            color_swatch: slint::Color::default(),
            color_a: 255,
            color_r: 0,
            color_g: 0,
            color_b: 0,
            conflict: "".into(),
        };
        let stress_group = SettingsGroup {
            category: 5,
            title: "Description Stress".into(),
            entry_id: "".into(),
            fields: Rc::new(VecModel::from(vec![
                make_field("stress/one", "One line", "Short single-line description.", "switch"),
                make_field(
                    "stress/two",
                    "Two lines",
                    "Preserve full ASS positioning, styling, typesetting, and embedded fonts via libmpv/libass with interactive dictionary lookup and highlighting.",
                    "switch",
                ),
                make_field(
                    "stress/three",
                    "Three lines",
                    "This is an intentionally very long three-line settings description paired with a wide 260px text input control so that it must wrap cleanly across at least three visual lines and expand both its FieldRow and its parent settings card without clipping or overflowing the card border.",
                    "text",
                ),
            ]))
            .into(),
        };
        settings.window().set_size(slint::LogicalSize::new(820.0, 680.0));
        settings.set_active_category(5);
        settings.set_settings_groups(Rc::new(VecModel::from(vec![stress_group])).into());
        assert_all_descriptions_contained("stress-1-2-3-lines");
        let stress_descs = ElementQuery::from_root(&settings)
            .match_id("FieldRow::field-desc")
            .find_all();
        assert_eq!(stress_descs.len(), 3);
        assert!(
            stress_descs[2].size().height > stress_descs[0].size().height * 2.0,
            "3-line description must wrap taller than 1-line ({} vs {})",
            stress_descs[2].size().height,
            stress_descs[0].size().height
        );

        settings.hide().unwrap();
    }

    #[test]
    fn subtitle_drawer_rows_are_compact_and_do_not_overlap() {
        i_slint_backend_testing::init_no_event_loop();
        let app = MementoApp::new().unwrap();
        app.window().set_size(slint::LogicalSize::new(1280.0, 720.0));
        app.set_sub_drawer_open(true);
        let rows: Vec<SubEntry> = (0..25)
            .map(|i| SubEntry {
                id: i,
                start: (i * 2) as f32,
                end: (i * 2 + 1) as f32,
                text: format!("短い字幕行 #{i}").into(),
                selected: false,
                is_active: i == 3,
                search_match: false,
            })
            .collect();
        app.set_subtitle_entries(Rc::new(VecModel::from(rows)).into());
        app.show().unwrap();

        let rendered_rows = ElementQuery::from_root(&app)
            .match_id("SubtitleDrawerPanel::sub-cue-row")
            .find_all();
        assert!(
            rendered_rows.len() >= 18,
            "compact subtitle drawer should fit many cues visibly (got {})",
            rendered_rows.len()
        );
        for (idx, row) in rendered_rows.iter().enumerate() {
            let h = row.size().height;
            assert!(
                (22.0..=28.0).contains(&h),
                "compact single-line cue row[{idx}] height should be ~24px (got {h})"
            );
        }
        let mut sorted: Vec<_> = rendered_rows
            .iter()
            .map(|r| (r.absolute_position().y, r.size().height))
            .collect();
        sorted.sort_by(|a, b| a.0.total_cmp(&b.0));
        for pair in sorted.windows(2) {
            assert!(
                pair[1].0 >= pair[0].0 + pair[0].1 - 0.5,
                "adjacent subtitle drawer rows must not overlap ({pair:?})"
            );
        }

        app.hide().unwrap();
    }

    #[test]
    fn global_bare_modifier_capture_reaches_popup_bridge_when_dict_open() {
        i_slint_backend_testing::init_no_event_loop();
        let app = MementoApp::new().unwrap();
        app.window().set_size(slint::LogicalSize::new(1280.0, 720.0));
        app.show().unwrap();

        let pb = app.global::<PopupBridge>();
        pb.set_rich_enabled(true);
        app.set_dict_open(true);

        let captured_presses: Rc<RefCell<Vec<(String, bool, bool, bool, bool)>>> =
            Rc::new(RefCell::new(Vec::new()));
        let captured_releases: Rc<RefCell<Vec<(bool, bool, bool, bool)>>> =
            Rc::new(RefCell::new(Vec::new()));
        let fallback_presses: Rc<RefCell<usize>> = Rc::new(RefCell::new(0));

        let cp = captured_presses.clone();
        pb.on_popup_key_pressed(move |text, ctrl, shift, alt, meta| {
            cp.borrow_mut().push((text.to_string(), ctrl, shift, alt, meta));
            true
        });
        let cr = captured_releases.clone();
        pb.on_popup_key_released(move |ctrl, shift, alt, meta| {
            cr.borrow_mut().push((ctrl, shift, alt, meta));
        });
        let fp = fallback_presses.clone();
        app.on_key_pressed(move |_, _, _, _, _| {
            *fp.borrow_mut() += 1;
            false
        });

        // Dispatch bare Shift & ShiftR while f-scope has focus and dict_open == true.
        for key in [Key::Shift, Key::ShiftR, Key::Control, Key::Alt, Key::Meta] {
            app.window().dispatch_event(WindowEvent::KeyPressed {
                text: key.into(),
            });
            app.window().dispatch_event(WindowEvent::KeyReleased {
                text: key.into(),
            });
        }

        let presses = captured_presses.borrow();
        assert_eq!(
            presses.len(),
            5,
            "all 5 bare modifiers must reach PopupBridge.popup-key-pressed via f-scope.capture-key-pressed ({presses:?})"
        );
        assert!(presses[0].2, "Key::Shift must report shift=true");
        assert!(presses[1].2, "Key::ShiftR must report shift=true");
        assert!(presses[2].1, "Key::Control must report control=true");
        assert!(presses[3].3, "Key::Alt must report alt=true");
        assert!(presses[4].4, "Key::Meta must report meta=true");
        assert_eq!(
            *fallback_presses.borrow(),
            0,
            "when PopupBridge consumes a glossary modifier press, f-scope key-pressed must not re-fire behind the popup"
        );
        assert_eq!(
            captured_releases.borrow().len(),
            5,
            "bare modifier releases must forward to PopupBridge.popup-key-released"
        );

        app.hide().unwrap();
    }
}

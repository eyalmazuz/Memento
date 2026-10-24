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
    assert!(app.get_dict_open());
    assert_eq!(app.get_selected_token_index(), 0);
    // The close button must clear the selection as well as hide the popup.
    click(848., 178.);
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

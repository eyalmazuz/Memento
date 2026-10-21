use memento_app::{apply_subtitle_furigana, publish_active_subtitle, MementoApp};
use slint::{ComponentHandle, Model};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    mpsc, Arc,
};

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
    assert!(epoch.load(Ordering::Acquire) > first.0);
    assert!(worker.try_recv().is_err());
    assert!(app
        .get_active_sub_words()
        .iter()
        .all(|w| w.reading.is_empty()));
    app.set_furigana_visible(true);
    publish_active_subtitle(&app, text, &requests, &epoch);
    assert!(worker.try_recv().unwrap().0 > first.0);
}

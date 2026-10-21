use i_slint_backend_testing::{init_no_event_loop, ElementQuery};
use memento_app::{DictResult, MementoApp, PopupGlossaryBlock};
use slint::{ComponentHandle, Model, VecModel};
use std::cell::Cell;
use std::rc::Rc;

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
    assert_eq!(app.get_popup_results().row_count(), 2);
    app.invoke_toggle_definition_selection(1);
    app.invoke_toggle_term_collapse(0);
    assert_eq!(app.get_popup_results().row_count(), 1);
    for _ in 0..10 {
        app.invoke_load_more_definition_results(true);
        slint::platform::update_timers_and_animations();
    }
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

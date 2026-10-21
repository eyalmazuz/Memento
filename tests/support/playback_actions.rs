//! Interaction stress driver using the application's models and popup actions.
use memento_app::{
    controller::{
        dict::{DictController, SearchHit, TermFrequency, TermPitchGraph},
        subtitles::subtitle_furigana_parts,
    },
    dictionary_result_rows, install_popup_navigation, popup_pitch_graphs, set_dictionary_rows,
    subtitle_lines_for_app, subtitle_words, subtitle_words_from_parts, MementoApp, PopupFrequency,
};
use memento_player::mpv::MpvInstance;
use slint::{ComponentHandle, Model, VecModel};
use std::{
    cell::Cell,
    rc::Rc,
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

enum ResultMessage {
    Search {
        term: String,
        elapsed: Duration,
        hits: Vec<SearchHit>,
        pitch: Vec<TermPitchGraph>,
        frequencies: Vec<TermFrequency>,
    },
    Furigana {
        text: String,
        elapsed: Duration,
        parts: Vec<(String, String)>,
    },
}

pub struct PlaybackActions {
    results: mpsc::Receiver<ResultMessage>,
    ruby_requests: mpsc::Sender<String>,
    search_pending: Rc<Cell<bool>>,
    next_action: u64,
    random: u64,
    pub searches: usize,
    pub ruby_updates: usize,
    pub seeks: usize,
    pub last_seek: Option<Instant>,
    pending_seek: Option<(Instant, f64, f64)>,
}

impl PlaybackActions {
    pub fn new(app: &MementoApp) -> Self {
        install_popup_navigation(app);
        let (results_tx, results) = mpsc::channel();
        let (search_tx, search_rx) = mpsc::channel::<String>();
        let search_results = results_tx.clone();
        let path = std::env::var_os("MEMENTO_VIDEO_TEST_DICTIONARY")
            .expect("interaction test requires MEMENTO_VIDEO_TEST_DICTIONARY");
        thread::spawn(move || {
            let mut dictionary = DictController::open(path).expect("read-only dictionary database");
            for term in search_rx {
                let start = Instant::now();
                let hits = dictionary.search(&term).to_vec();
                assert!(dictionary.take_search_error().is_none());
                assert!(!hits.is_empty(), "no dictionary results for {term}");
                let head = &hits[0];
                let pitch = dictionary.get_pitch_graphs(&head.expression, &head.reading);
                let frequencies = dictionary.get_frequencies(&head.expression, &head.reading);
                if search_results
                    .send(ResultMessage::Search {
                        term,
                        elapsed: start.elapsed(),
                        hits,
                        pitch,
                        frequencies,
                    })
                    .is_err()
                {
                    break;
                }
            }
        });
        let (ruby_requests, ruby_rx) = mpsc::channel::<String>();
        thread::spawn(move || {
            while let Ok(mut text) = ruby_rx.recv() {
                while let Ok(newer) = ruby_rx.try_recv() {
                    text = newer;
                }
                let start = Instant::now();
                let parts = subtitle_furigana_parts(&text).expect("furigana parser");
                if results_tx
                    .send(ResultMessage::Furigana {
                        text,
                        elapsed: start.elapsed(),
                        parts,
                    })
                    .is_err()
                {
                    break;
                }
            }
        });
        let search_pending = Rc::new(Cell::new(false));
        let pending = search_pending.clone();
        app.on_lookup_word(move |term| {
            pending.set(true);
            search_tx.send(term.to_string()).unwrap();
        });
        let weak = app.as_weak();
        let ruby = ruby_requests.clone();
        app.on_furigana_visible_changed(move |enabled| {
            let app = weak.upgrade().unwrap();
            app.set_furigana_visible(enabled);
            let words = subtitle_words(app.get_active_sub_text().as_str());
            app.set_active_sub_words(Rc::new(VecModel::from(words.clone())).into());
            app.set_active_sub_lines(
                Rc::new(VecModel::from(subtitle_lines_for_app(&app, words))).into(),
            );
            if enabled {
                ruby.send(app.get_active_sub_text().to_string()).unwrap();
            }
        });
        Self {
            results,
            ruby_requests,
            search_pending,
            next_action: 1,
            random: 0x49e3a11,
            searches: 0,
            ruby_updates: 0,
            seeks: 0,
            last_seek: None,
            pending_seek: None,
        }
    }

    pub fn subtitle_changed(&self, app: &MementoApp) {
        if app.get_furigana_visible() {
            self.ruby_requests
                .send(app.get_active_sub_text().to_string())
                .unwrap();
        }
    }

    pub fn tick(
        &mut self,
        app: &MementoApp,
        player: &MpvInstance,
        elapsed: Duration,
    ) -> Vec<String> {
        let mut updates = Vec::new();
        if let Some((started, before, target)) = self.pending_seek {
            let position = player.poll_playback_state().position;
            if (position - before).abs() > 1. || (position - target).abs() < 0.5 {
                updates.push(format!(
                    "seek reached {position:.2}s after {:.1}ms",
                    started.elapsed().as_secs_f64() * 1000.
                ));
                self.pending_seek = None;
            } else {
                assert!(
                    started.elapsed() < Duration::from_secs(2),
                    "seek did not change playback position"
                );
            }
        }
        while let Ok(result) = self.results.try_recv() {
            match result {
                ResultMessage::Search {
                    term,
                    elapsed,
                    hits,
                    pitch,
                    frequencies,
                } => {
                    let mut rows = dictionary_result_rows(&hits, &[]);
                    rows[0].pitch_graphs =
                        Rc::new(VecModel::from(popup_pitch_graphs(pitch))).into();
                    rows[0].frequencies = Rc::new(VecModel::from(
                        frequencies
                            .into_iter()
                            .map(|f| PopupFrequency {
                                dictionary: f.dictionary.into(),
                                value: f.value.into(),
                            })
                            .collect::<Vec<_>>(),
                    ))
                    .into();
                    let count = rows.len();
                    app.set_dict_open(false);
                    set_dictionary_rows(app, rows);
                    app.set_dict_open(true);
                    assert_eq!(app.get_dict_results().row_count(), count);
                    self.search_pending.set(false);
                    self.searches += 1;
                    updates.push(format!(
                        "search {term}: {count} results, worker={:.1}ms",
                        elapsed.as_secs_f64() * 1000.
                    ));
                }
                ResultMessage::Furigana {
                    text,
                    elapsed,
                    parts,
                } => {
                    if app.get_furigana_visible() && app.get_active_sub_text().as_str() == text {
                        let words = subtitle_words_from_parts(&text, parts);
                        assert_eq!(
                            words.iter().map(|w| w.text.as_str()).collect::<String>(),
                            text
                        );
                        assert!(
                            words.iter().any(|w| !w.reading.is_empty()),
                            "no furigana was generated"
                        );
                        app.set_active_sub_words(Rc::new(VecModel::from(words.clone())).into());
                        app.set_active_sub_lines(
                            Rc::new(VecModel::from(subtitle_lines_for_app(app, words))).into(),
                        );
                        self.ruby_updates += 1;
                        updates.push(format!(
                            "furigana ready: worker={:.1}ms",
                            elapsed.as_secs_f64() * 1000.
                        ));
                    }
                }
            }
        }
        if elapsed.as_secs() < self.next_action {
            return updates;
        }
        let action = self.next_action;
        self.next_action += 1;
        match action {
            1 | 10 | 23 => {
                app.invoke_furigana_visible_changed(false);
                assert!(!app.get_furigana_visible());
                updates.push("furigana off".into());
            }
            2 | 11 | 25 => {
                app.invoke_furigana_visible_changed(true);
                assert!(app.get_furigana_visible());
                updates.push("furigana on".into());
            }
            3 | 9 | 13 | 20 | 26 => {
                let term = match action {
                    3 => "迎えに行く",
                    9 => "世界中",
                    13 => "読み込む",
                    20 => "去年",
                    _ => "茶葉",
                };
                app.invoke_lookup_word(term.into());
                updates.push(format!("request {term}"));
            }
            4 | 5 if app.get_dict_results().row_count() > 0 => {
                app.invoke_toggle_term_collapse(0);
                let collapsed = app.get_dict_results().row_data(0).unwrap().collapsed;
                assert_eq!(collapsed, action == 4);
                updates.push(format!("collapsed={collapsed}"));
            }
            6 | 14 | 27 => {
                app.invoke_load_more_definition_results(true);
                updates.push("load next definitions".into());
            }
            7 | 15 => {
                // Exercise the actual scroll view, including its loading callback.
                let position = slint::LogicalPosition::new(640., 180.);
                app.window()
                    .dispatch_event(slint::platform::WindowEvent::PointerMoved { position });
                app.window()
                    .dispatch_event(slint::platform::WindowEvent::PointerScrolled {
                        position,
                        delta_x: 0.,
                        delta_y: -260.,
                    });
                updates.push("scroll popup".into());
            }
            8 | 12 | 18 | 21 | 24 => {
                app.set_dict_open(false);
                let before = player.poll_playback_state().position;
                let target;
                if action == 12 {
                    target = (before - 5.).max(0.);
                    player.seek_relative(-5.).unwrap();
                    updates.push("seek backward 5s".into());
                } else {
                    self.random = self
                        .random
                        .wrapping_mul(6364136223846793005)
                        .wrapping_add(1);
                    let duration = player.poll_playback_state().duration.max(15.);
                    let fraction = (self.random >> 32) as f64 / u32::MAX as f64;
                    target = 1. + fraction * (duration - 10.);
                    player.seek(target).unwrap();
                    updates.push(format!("seek to {target:.2}s"));
                }
                self.seeks += 1;
                self.last_seek = Some(Instant::now());
                self.pending_seek = Some((Instant::now(), before, target));
            }
            16 => {
                player.pause().unwrap();
                app.set_is_playing(false);
                updates.push("pause".into());
            }
            17 => {
                player.play().unwrap();
                app.set_is_playing(true);
                updates.push("resume".into());
            }
            19 | 22 => {
                app.set_subtitles_visible(action == 22);
                let (width, height) = if action == 19 {
                    (960., 540.)
                } else {
                    (1280., 720.)
                };
                app.window()
                    .set_size(slint::LogicalSize::new(width, height));
                updates.push(format!("subtitles visible={}", action == 22));
                updates.push(format!("resize to {width}x{height}"));
            }
            28 if app.get_dict_results().row_count() > 0 => {
                let before = app.get_dict_results().row_data(0).unwrap().selected;
                app.invoke_toggle_definition_selection(0);
                assert_ne!(app.get_dict_results().row_data(0).unwrap().selected, before);
                updates.push("toggle definition selection".into());
            }
            29 => {
                app.set_dict_open(false);
                updates.push("close popup".into());
            }
            _ => {}
        }
        updates
    }

    pub fn verify(&self) {
        assert_eq!(self.searches, 5, "not all term searches completed");
        assert_eq!(self.seeks, 5, "not all seeks completed");
        assert!(self.ruby_updates >= 3, "furigana was not regenerated");
        assert!(
            !self.search_pending.get(),
            "dictionary search never completed"
        );
        assert!(self.pending_seek.is_none(), "last seek did not complete");
    }
}

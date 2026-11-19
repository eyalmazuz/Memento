//! Background worker threads (thumbnail preview with Condvar wakeup, dictionary lookup,
//! furigana, external subtitle parser, MKV EBML extractor), coalesced UI event relay,
//! and bounded graceful shutdown.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use slint::{ComponentHandle, Model as _, Rgba8Pixel, SharedPixelBuffer};

use crate::app::anki_actions::{set_popup_audio_sources, set_popup_term_metadata, PopupAudioState};
use crate::app::preferences::{
    apply_dictionary_order, deconj_matcher_enabled, exact_matcher_enabled, mecab_matcher_enabled,
    search_text_filters,
};
use crate::app::subtitle_io::parse_external_subtitles;
use crate::controller::config::ConfigController;
use crate::controller::dict::{self, DictController, SearchHit};
use crate::controller::subtitles::{self, ExternalSubtitleLoad};
use crate::ui::{self, dictionary_result_rows, set_dictionary_rows, MementoApp, PopupBridge};
use memento_config::Value;
use memento_player::MpvInstance;
use memento_setting::keys as raw;

/// Thread-safe, closeable wrapper around `mpsc::Sender<T>` so UI callbacks holding
/// clones never keep worker channels open past `WorkerPool::shutdown()`.
pub(crate) struct ChannelSlot<T>(Arc<Mutex<Option<std::sync::mpsc::Sender<T>>>>);

impl<T> Clone for ChannelSlot<T> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<T> ChannelSlot<T> {
    pub(crate) fn new(tx: std::sync::mpsc::Sender<T>) -> Self {
        Self(Arc::new(Mutex::new(Some(tx))))
    }

    pub(crate) fn send(&self, value: T) -> bool {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_ref()
            .is_some_and(|tx| tx.send(value).is_ok())
    }

    pub(crate) fn with_sender(&self, f: impl FnOnce(&std::sync::mpsc::Sender<T>)) {
        let guard = self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(tx) = guard.as_ref() {
            f(tx);
        }
    }

    pub(crate) fn close(&self) {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
    }
}

/// Messages delivered from background workers to the Slint UI thread via the single
/// coalesced `UiRelay` (replacing the 4 legacy `thread_local!` global sinks).
pub(crate) enum UiEvent {
    OpenPaths(Vec<PathBuf>),
    ExternalSubtitle {
        generation: u64,
        track_id: i64,
        path: String,
        result: Result<Vec<memento_subtitle::SubtitleEntry>, String>,
    },
    MkvFonts {
        generation: u64,
        fonts: Vec<memento_ass::MkvFontAttachment>,
    },
    MkvTrack {
        generation: u64,
        sid: i64,
        cues: Vec<memento_ass::MkvSubtitleCue>,
    },
}

struct UiRelayInner {
    events: Mutex<VecDeque<UiEvent>>,
    mpv_sync_needed: AtomicBool,
    wakeup_scheduled: AtomicBool,
}

/// Coalesced cross-thread relay into the Slint event loop.
///
/// Multiple rapid `mpv` wakeups or background worker completions coalesce into a
/// single `slint::invoke_from_event_loop` call via `wakeup_scheduled`.
#[derive(Clone)]
pub(crate) struct UiRelay {
    inner: Arc<UiRelayInner>,
}

impl UiRelay {
    pub(crate) fn new() -> Self {
        Self {
            inner: Arc::new(UiRelayInner {
                events: Mutex::new(VecDeque::new()),
                mpv_sync_needed: AtomicBool::new(false),
                wakeup_scheduled: AtomicBool::new(false),
            }),
        }
    }

    pub(crate) fn send_event(&self, event: UiEvent) {
        self.inner
            .events
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push_back(event);
        self.schedule_wakeup();
    }

    pub(crate) fn request_mpv_sync(&self) {
        self.inner.mpv_sync_needed.store(true, Ordering::Release);
        self.schedule_wakeup();
    }

    pub(crate) fn clear_wakeup_flag(&self) {
        self.inner.wakeup_scheduled.store(false, Ordering::Release);
    }

    pub(crate) fn take_mpv_sync_needed(&self) -> bool {
        self.inner.mpv_sync_needed.swap(false, Ordering::AcqRel)
    }

    pub(crate) fn drain_events(&self) -> Vec<UiEvent> {
        let mut queue = self
            .inner
            .events
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        queue.drain(..).collect()
    }

    fn schedule_wakeup(&self) {
        if !self.inner.wakeup_scheduled.swap(true, Ordering::AcqRel) {
            let _ = slint::invoke_from_event_loop(dispatch_active_ui_relay);
        }
    }
}

thread_local! {
    static ACTIVE_UI_DISPATCH: RefCell<Option<Rc<dyn Fn()>>> = const { RefCell::new(None) };
}

fn dispatch_active_ui_relay() {
    let cb = ACTIVE_UI_DISPATCH.with(|slot| slot.borrow().clone());
    if let Some(dispatch) = cb {
        dispatch();
    }
}

/// RAII guard registering the UI-thread dispatcher and guaranteeing it is cleared on
/// exit or unwind so no `Rc` cycles survive past `app.run()`.
pub(crate) struct UiDispatchGuard;

impl UiDispatchGuard {
    pub(crate) fn install(handler: Rc<dyn Fn()>) -> Self {
        ACTIVE_UI_DISPATCH.with(|slot| {
            *slot.borrow_mut() = Some(handler);
        });
        Self
    }
}

impl Drop for UiDispatchGuard {
    fn drop(&mut self) {
        ACTIVE_UI_DISPATCH.with(|slot| {
            *slot.borrow_mut() = None;
        });
    }
}

pub(crate) enum ThumbRequest {
    Seek { path: String, time_sec: f64 },
    Unload,
}

/// Idle timeout after which the thumbnail preview `libmpv` instance is dropped
/// to reclaim 50–150 MB of RAM (lazily recreated on the next timeline hover).
const THUMB_IDLE_TIMEOUT: Duration = Duration::from_secs(40);

struct ThumbMpvState {
    // Declared before `inst` so the software renderer handle drops before `MpvInstance`.
    renderer: memento_player::mpv::MpvSoftwareRenderer,
    inst: MpvInstance,
    wake_signal: Arc<(Mutex<bool>, Condvar)>,
}

pub(crate) struct WordLookupRequest {
    pub(crate) generation: u64,
    pub(crate) query: String,
    pub(crate) order: Vec<i64>,
    pub(crate) source_subtitle: String,
    pub(crate) excluded: Vec<String>,
    pub(crate) exact_enabled: bool,
    pub(crate) deconj_enabled: bool,
    pub(crate) mecab_enabled: bool,
    pub(crate) remove_regex: String,
    pub(crate) replace_lines: bool,
    pub(crate) replace_with: String,
}

pub(crate) struct KanjiLookupRequest {
    pub(crate) generation: u64,
    pub(crate) character: String,
    pub(crate) order: Vec<i64>,
    pub(crate) excluded: Vec<String>,
}

pub(crate) enum LookupWorkerRequest {
    Word(WordLookupRequest),
    Kanji(KanjiLookupRequest),
}

impl LookupWorkerRequest {
    fn generation(&self) -> u64 {
        match self {
            Self::Word(req) => req.generation,
            Self::Kanji(req) => req.generation,
        }
    }
}

pub(crate) struct WordLookupResult {
    pub(crate) hits: Vec<SearchHit>,
    pub(crate) error: Option<String>,
    pub(crate) database_open: bool,
    pub(crate) matched_units: usize,
    pub(crate) headword: String,
    pub(crate) reading: String,
    pub(crate) pitch: String,
    pub(crate) freq: String,
    pub(crate) jpdb: String,
}

pub(crate) enum MkvExtractRequest {
    Fonts {
        generation: u64,
        path: PathBuf,
    },
    Track {
        generation: u64,
        sid: i64,
        track_number: u64,
        path: PathBuf,
    },
}

pub(crate) fn display_word_lookup(
    app: &MementoApp,
    result: WordLookupResult,
    query: &str,
    excluded: &[String],
) {
    app.set_dict_open(false);
    app.set_popup_selected_audio_source(-1);
    app.set_anki_added_note_id("".into());
    app.set_anki_added_result_index(-1);
    if let Some(error) = result.error {
        app.set_status_message(format!("Dictionary search failed: {error}").into());
    } else if result.hits.is_empty() && !result.database_open && !query.trim().is_empty() {
        app.set_status_message("Dictionary database is unavailable".into());
    }
    set_dictionary_rows(app, dictionary_result_rows(&result.hits, excluded));
    app.invoke_reset_definition_scroll();
    app.set_dict_query(result.headword.clone().into());
    app.set_dict_reading(result.reading.into());
    app.set_dict_pitch_accent(result.pitch.into());
    app.set_dict_freq_rank(result.freq.into());
    app.set_dict_jpdb_rank(result.jpdb.into());
    app.set_dict_open(!result.hits.is_empty());

    let mut match_count = 1;
    if !result.headword.is_empty() {
        let start_idx = app.get_selected_token_index();
        if start_idx >= 0 {
            let sub_words = app.get_active_sub_words();
            let target_len = if result.matched_units > 0 {
                result.matched_units
            } else {
                result.headword.encode_utf16().count()
            };
            let mut accumulated = 0;
            let mut count = 0;
            for i in (start_idx as usize)..sub_words.row_count() {
                if let Some(word) = sub_words.row_data(i) {
                    accumulated += word.text.encode_utf16().count();
                    count += 1;
                    if accumulated >= target_len {
                        break;
                    }
                } else {
                    break;
                }
            }
            if count > 0 {
                match_count = count as i32;
            }
        }
    }
    app.set_selected_token_count(match_count);
}

fn wait_for_render_update(
    inst: &MpvInstance,
    wake_signal: &(Mutex<bool>, Condvar),
    timeout: Duration,
) {
    let deadline = Instant::now() + timeout;
    loop {
        if inst.take_render_update() {
            return;
        }
        let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
            return;
        };
        let (lock, cvar) = wake_signal;
        let mut ready = lock.lock().unwrap_or_else(|p| p.into_inner());
        if *ready {
            *ready = false;
            continue;
        }
        let (mut guard, wait_res) = cvar
            .wait_timeout(ready, remaining)
            .unwrap_or_else(|p| p.into_inner());
        *guard = false;
        if wait_res.timed_out() {
            let _ = inst.take_render_update();
            return;
        }
    }
}

/// Owned channels and `JoinHandle`s for all long-lived background worker threads.
/// Calling `shutdown()` closes every channel slot, advances cancellation epochs,
/// and joins workers cleanly within a bounded budget.
pub(crate) struct WorkerPool {
    pub(crate) thumb_tx: ChannelSlot<ThumbRequest>,
    pub(crate) lookup_tx: ChannelSlot<LookupWorkerRequest>,
    pub(crate) furigana_tx: ChannelSlot<(u64, String)>,
    pub(crate) ext_sub_tx: ChannelSlot<(u64, ExternalSubtitleLoad)>,
    pub(crate) mkv_tx: ChannelSlot<MkvExtractRequest>,
    pub(crate) furigana_epoch: Arc<AtomicU64>,
    shared_media_generation: Arc<AtomicU64>,
    lookup_epoch: Arc<AtomicU64>,
    handles: Vec<(&'static str, std::thread::JoinHandle<()>)>,
}

impl WorkerPool {
    pub(crate) fn spawn(
        app: &MementoApp,
        dictionary_database_path: PathBuf,
        shared_media_generation: Arc<AtomicU64>,
        lookup_epoch: Arc<AtomicU64>,
        ui_relay: UiRelay,
    ) -> Self {
        let mut handles = Vec::with_capacity(5);

        // 1. Timeline hover thumbnail preview worker (Condvar-driven, zero sleep-polling,
        //    with 40s idle teardown to reclaim 50–150 MB of libmpv/FFmpeg memory).
        let (thumb_raw_tx, thumb_rx) = std::sync::mpsc::channel::<ThumbRequest>();
        let thumb_tx = ChannelSlot::new(thumb_raw_tx);
        let thumb_worker_app = app.as_weak();
        let thumb_handle = std::thread::spawn(move || {
            let mut thumb_mpv: Option<ThumbMpvState> = None;
            let mut loaded_path = String::new();
            loop {
                let mut req = if thumb_mpv.is_some() {
                    match thumb_rx.recv_timeout(THUMB_IDLE_TIMEOUT) {
                        Ok(req) => req,
                        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                            if let Some(state) = thumb_mpv.take() {
                                drop(state);
                                crate::app::trim_os_heap();
                            }
                            loaded_path.clear();
                            continue;
                        }
                        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                    }
                } else {
                    match thumb_rx.recv() {
                        Ok(req) => req,
                        Err(_) => break,
                    }
                };
                while let Ok(newer) = thumb_rx.try_recv() {
                    req = newer;
                }
                match req {
                    ThumbRequest::Unload => {
                        if let Some(state) = thumb_mpv.take() {
                            drop(state);
                            crate::app::trim_os_heap();
                        }
                        loaded_path.clear();
                        continue;
                    }
                    ThumbRequest::Seek { path, time_sec } => {
                        if path.is_empty() || !Path::new(&path).is_file() {
                            continue;
                        }
                        if thumb_mpv.is_none() {
                            let opts = [
                                memento_player::CliOption {
                                    name: "config".into(),
                                    value: "no".into(),
                                },
                                memento_player::CliOption {
                                    name: "aid".into(),
                                    value: "no".into(),
                                },
                                memento_player::CliOption {
                                    name: "sid".into(),
                                    value: "no".into(),
                                },
                                memento_player::CliOption {
                                    name: "secondary-sid".into(),
                                    value: "no".into(),
                                },
                                memento_player::CliOption {
                                    name: "osc".into(),
                                    value: "no".into(),
                                },
                                memento_player::CliOption {
                                    name: "pause".into(),
                                    value: "yes".into(),
                                },
                                memento_player::CliOption {
                                    name: "ytdl".into(),
                                    value: "no".into(),
                                },
                                memento_player::CliOption {
                                    name: "terminal".into(),
                                    value: "no".into(),
                                },
                                memento_player::CliOption {
                                    name: "hwdec".into(),
                                    value: "no".into(),
                                },
                                memento_player::CliOption {
                                    name: "hr-seek".into(),
                                    value: "no".into(),
                                },
                                memento_player::CliOption {
                                    name: "load-scripts".into(),
                                    value: "no".into(),
                                },
                            ];
                            if let Ok(inst) = MpvInstance::new_with_options(None, &opts) {
                                let wake_signal = Arc::new((Mutex::new(false), Condvar::new()));
                                let wake_clone = wake_signal.clone();
                                inst.set_render_wakeup(move || {
                                    let (lock, cvar) = &*wake_clone;
                                    if let Ok(mut ready) = lock.lock() {
                                        *ready = true;
                                        cvar.notify_one();
                                    }
                                });
                                let renderer = inst.software_renderer();
                                thumb_mpv = Some(ThumbMpvState {
                                    renderer,
                                    inst,
                                    wake_signal,
                                });
                            }
                        }
                        let Some(state) = thumb_mpv.as_ref() else {
                            continue;
                        };
                        if loaded_path != path {
                            if state.inst.load_file(&path).is_err() {
                                continue;
                            }
                            loaded_path = path.clone();
                            wait_for_render_update(
                                &state.inst,
                                &state.wake_signal,
                                Duration::from_millis(160),
                            );
                        }
                        let _ = state.inst.command(&[
                            "seek",
                            &format!("{:.3}", time_sec.max(0.0)),
                            "absolute+keyframes",
                        ]);
                        wait_for_render_update(
                            &state.inst,
                            &state.wake_signal,
                            Duration::from_millis(80),
                        );
                        let mut buffer = SharedPixelBuffer::<Rgba8Pixel>::new(160, 90);
                        if unsafe { state.renderer.render_rgba(160, 90, buffer.make_mut_bytes()) }
                            .is_ok()
                        {
                            let _ = thumb_worker_app.upgrade_in_event_loop(move |app| {
                                app.set_thumbnail_frame(slint::Image::from_rgba8(buffer));
                            });
                        }
                    }
                }
            }
        });
        handles.push(("thumb_worker", thumb_handle));

        // 2. Dictionary & kanji lookup worker (off-UI-thread SQLite queries).
        let (lookup_raw_tx, lookup_rx) = std::sync::mpsc::channel::<LookupWorkerRequest>();
        let lookup_tx = ChannelSlot::new(lookup_raw_tx);
        let worker_epoch = lookup_epoch.clone();
        let lookup_path = dictionary_database_path;
        let worker_app = app.as_weak();
        let lookup_handle = std::thread::spawn(move || {
            let mut database_signature = dict::db_file_fingerprint(&lookup_path);
            let mut dictionary = DictController::open(&lookup_path).unwrap_or_else(|error| {
                eprintln!("Dictionary lookup unavailable: {error}");
                DictController::new()
            });
            while let Ok(mut request) = lookup_rx.recv() {
                while let Ok(newer) = lookup_rx.try_recv() {
                    request = newer;
                }
                if request.generation() != worker_epoch.load(Ordering::Acquire) {
                    continue;
                }
                let signature = dict::db_file_fingerprint(&lookup_path);
                if signature != database_signature {
                    dictionary = DictController::open(&lookup_path).unwrap_or_else(|error| {
                        eprintln!("Dictionary lookup unavailable: {error}");
                        DictController::new()
                    });
                    database_signature = signature;
                }
                match request {
                    LookupWorkerRequest::Word(request) => {
                        dictionary.set_order(&request.order);
                        dictionary.set_exact_enabled(request.exact_enabled);
                        dictionary.set_deconj_enabled(request.deconj_enabled);
                        dictionary.set_mecab_enabled(request.mecab_enabled);
                        dictionary.set_search_text_filters(
                            &request.remove_regex,
                            request.replace_lines,
                            &request.replace_with,
                        );
                        let hits = dictionary.search(&request.query).to_vec();
                        let error = dictionary.take_search_error();
                        let database_open = dictionary.database_path_is_open();
                        let matched_units = dictionary.last_match_units();
                        let headword = hits
                            .first()
                            .map(|hit| hit.expression.clone())
                            .unwrap_or_default();
                        let reading = hits
                            .first()
                            .map(|hit| hit.reading.clone())
                            .unwrap_or_default();
                        let meta_headword = headword.clone();
                        let meta_reading = reading.clone();
                        let mut pitch_terms = Vec::new();
                        for hit in &hits {
                            if !hit.is_kanji
                                && !pitch_terms.iter().any(
                                    |(expression, reading): &(String, String)| {
                                        expression == &hit.expression && reading == &hit.reading
                                    },
                                )
                            {
                                pitch_terms.push((hit.expression.clone(), hit.reading.clone()));
                            }
                        }
                        let result = WordLookupResult {
                            hits,
                            error,
                            database_open,
                            matched_units,
                            headword,
                            reading,
                            pitch: String::new(),
                            freq: String::new(),
                            jpdb: String::new(),
                        };
                        if request.generation != worker_epoch.load(Ordering::Acquire) {
                            continue;
                        }
                        let generation = request.generation;
                        let query = request.query;
                        let source_subtitle = request.source_subtitle;
                        let excluded = request.excluded;
                        let meta_subtitle = source_subtitle.clone();
                        let event_epoch = worker_epoch.clone();
                        let _ = worker_app.upgrade_in_event_loop(move |app| {
                            if generation == event_epoch.load(Ordering::Acquire)
                                && app.get_active_sub_text().as_str() == source_subtitle
                            {
                                display_word_lookup(&app, result, &query, &excluded);
                            }
                        });
                        if !meta_headword.is_empty()
                            && generation == worker_epoch.load(Ordering::Acquire)
                        {
                            let (pitch, freq, jpdb) =
                                dictionary.get_term_meta(&meta_headword, &meta_reading);
                            let mut graphs = Vec::with_capacity(pitch_terms.len());
                            for (expression, reading) in pitch_terms {
                                if generation != worker_epoch.load(Ordering::Acquire) {
                                    break;
                                }
                                let term_graphs =
                                    dictionary.get_pitch_graphs(&expression, &reading);
                                let frequencies = dictionary.get_frequencies(&expression, &reading);
                                graphs.push((expression, reading, term_graphs, frequencies));
                            }
                            if generation != worker_epoch.load(Ordering::Acquire) {
                                continue;
                            }
                            let event_epoch = worker_epoch.clone();
                            let _ = worker_app.upgrade_in_event_loop(move |app| {
                                if generation == event_epoch.load(Ordering::Acquire)
                                    && app.get_active_sub_text().as_str() == meta_subtitle
                                {
                                    app.set_dict_pitch_accent(pitch.into());
                                    app.set_dict_freq_rank(freq.into());
                                    app.set_dict_jpdb_rank(jpdb.into());
                                    for (expression, reading, graph_rows, frequencies) in graphs {
                                        set_popup_term_metadata(
                                            &app,
                                            &expression,
                                            &reading,
                                            graph_rows,
                                            frequencies,
                                        );
                                    }
                                }
                            });
                        }
                    }
                    LookupWorkerRequest::Kanji(request) => {
                        dictionary.set_order(&request.order);
                        let hits = dictionary.search_kanji(&request.character).to_vec();
                        let error = dictionary.take_search_error();
                        let database_open = dictionary.database_path_is_open();
                        let headword = hits
                            .first()
                            .map(|hit| hit.expression.clone())
                            .unwrap_or_default();
                        let reading = hits
                            .first()
                            .map(|hit| hit.reading.clone())
                            .unwrap_or_default();
                        let (pitch, freq, jpdb) = if !headword.is_empty() {
                            dictionary.get_term_meta(&headword, &reading)
                        } else {
                            (String::new(), String::new(), String::new())
                        };
                        if request.generation != worker_epoch.load(Ordering::Acquire) {
                            continue;
                        }
                        let generation = request.generation;
                        let excluded = request.excluded;
                        let event_epoch = worker_epoch.clone();
                        let _ = worker_app.upgrade_in_event_loop(move |app| {
                            if generation != event_epoch.load(Ordering::Acquire) {
                                return;
                            }
                            if let Some(error) = error {
                                app.set_status_message(
                                    format!("Kanji lookup failed: {error}").into(),
                                );
                            } else if hits.is_empty() && !database_open {
                                app.set_status_message("Dictionary database is unavailable".into());
                            }
                            app.set_dict_open(false);
                            set_dictionary_rows(&app, dictionary_result_rows(&hits, &excluded));
                            app.invoke_reset_definition_scroll();
                            app.set_dict_query(headword.into());
                            app.set_dict_reading(reading.into());
                            app.set_dict_pitch_accent(pitch.into());
                            app.set_dict_freq_rank(freq.into());
                            app.set_dict_jpdb_rank(jpdb.into());
                            app.set_dict_open(!hits.is_empty());
                        });
                    }
                }
            }
        });
        handles.push(("lookup_worker", lookup_handle));

        // 3. Furigana parsing worker.
        let (furigana_raw_tx, furigana_rx) = std::sync::mpsc::channel::<(u64, String)>();
        let furigana_tx = ChannelSlot::new(furigana_raw_tx);
        let furigana_epoch = Arc::new(AtomicU64::new(0));
        let furigana_worker_app = app.as_weak();
        let furigana_worker_epoch = furigana_epoch.clone();
        let furigana_handle = std::thread::spawn(move || {
            let mut warned = false;
            while let Ok(mut request) = furigana_rx.recv() {
                while let Ok(newer) = furigana_rx.try_recv() {
                    request = newer;
                }
                if request.0 != furigana_worker_epoch.load(Ordering::Acquire) {
                    continue;
                }
                let result = subtitles::subtitle_furigana_parts(&request.1);
                let report_error = result.is_err() && !warned;
                warned |= report_error;
                let epoch = furigana_worker_epoch.clone();
                let _ = furigana_worker_app.upgrade_in_event_loop(move |app| {
                    if request.0 != epoch.load(Ordering::Acquire)
                        || !app.get_furigana_visible()
                        || app.get_active_sub_text().as_str() != request.1
                    {
                        return;
                    }
                    match result {
                        Ok(parts) => {
                            ui::apply_subtitle_furigana(&app, &request.1, parts);
                        }
                        Err(error) => {
                            app.set_subtitle_furigana_pending(false);
                            if report_error {
                                app.set_status_message(
                                    format!("Furigana unavailable: {error}").into(),
                                );
                            }
                        }
                    }
                });
            }
        });
        handles.push(("furigana_worker", furigana_handle));

        // 4. Async external subtitle parser worker.
        let (ext_sub_raw_tx, ext_sub_rx) =
            std::sync::mpsc::channel::<(u64, ExternalSubtitleLoad)>();
        let ext_sub_tx = ChannelSlot::new(ext_sub_raw_tx);
        let ext_sub_shared_gen = shared_media_generation.clone();
        let ext_sub_relay = ui_relay.clone();
        let ext_sub_handle = std::thread::spawn(move || {
            while let Ok((generation, load)) = ext_sub_rx.recv() {
                if ext_sub_shared_gen.load(Ordering::Acquire) != generation {
                    continue;
                }
                let parsed = parse_external_subtitles(&load.path);
                if ext_sub_shared_gen.load(Ordering::Acquire) != generation {
                    continue;
                }
                let path = load.path.to_string_lossy().into_owned();
                ext_sub_relay.send_event(UiEvent::ExternalSubtitle {
                    generation,
                    track_id: load.track_id,
                    path,
                    result: parsed,
                });
            }
        });
        handles.push(("ext_sub_worker", ext_sub_handle));

        // 5. Async non-blocking MKV EBML worker (embedded fonts + subtitle cues).
        let (mkv_raw_tx, mkv_rx) = std::sync::mpsc::channel::<MkvExtractRequest>();
        let mkv_tx = ChannelSlot::new(mkv_raw_tx);
        let mkv_worker_gen = shared_media_generation.clone();
        let mkv_relay = ui_relay;
        let mkv_handle = std::thread::spawn(move || {
            while let Ok(req) = mkv_rx.recv() {
                match req {
                    MkvExtractRequest::Fonts { generation, path } => {
                        if mkv_worker_gen.load(Ordering::Acquire) != generation {
                            continue;
                        }
                        if let Ok(fonts) = memento_ass::extract_mkv_fonts(&path) {
                            if mkv_worker_gen.load(Ordering::Acquire) == generation
                                && !fonts.is_empty()
                            {
                                mkv_relay.send_event(UiEvent::MkvFonts { generation, fonts });
                            }
                        }
                    }
                    MkvExtractRequest::Track {
                        generation,
                        sid,
                        track_number,
                        path,
                    } => {
                        if mkv_worker_gen.load(Ordering::Acquire) != generation {
                            continue;
                        }
                        if let Ok(extracted) =
                            memento_ass::extract_mkv_subtitle_track(&path, track_number)
                        {
                            if mkv_worker_gen.load(Ordering::Acquire) == generation
                                && !extracted.cues.is_empty()
                            {
                                mkv_relay.send_event(UiEvent::MkvTrack {
                                    generation,
                                    sid,
                                    cues: extracted.cues,
                                });
                            }
                        }
                    }
                }
            }
        });
        handles.push(("mkv_worker", mkv_handle));

        Self {
            thumb_tx,
            lookup_tx,
            furigana_tx,
            ext_sub_tx,
            mkv_tx,
            furigana_epoch,
            shared_media_generation,
            lookup_epoch,
            handles,
        }
    }

    /// Close all worker channels, invalidate in-flight epochs, and join worker threads
    /// within a bounded deadline so shutdown neither leaks threads nor hangs forever.
    pub(crate) fn shutdown(mut self) {
        self.shared_media_generation.fetch_add(1, Ordering::AcqRel);
        self.lookup_epoch.fetch_add(1, Ordering::AcqRel);
        self.furigana_epoch.fetch_add(1, Ordering::AcqRel);

        self.thumb_tx.close();
        self.lookup_tx.close();
        self.furigana_tx.close();
        self.ext_sub_tx.close();
        self.mkv_tx.close();

        let deadline = Instant::now() + Duration::from_secs(2);
        let mut remaining = std::mem::take(&mut self.handles);
        while !remaining.is_empty() {
            let mut still_running = Vec::new();
            for (name, handle) in remaining {
                if handle.is_finished() {
                    if handle.join().is_err() {
                        eprintln!("Worker {name} panicked during shutdown");
                    }
                } else {
                    still_running.push((name, handle));
                }
            }
            if still_running.is_empty() {
                break;
            }
            if Instant::now() >= deadline {
                for (name, _handle) in still_running {
                    eprintln!("Worker {name} did not exit within 2s shutdown budget; detaching");
                }
                break;
            }
            remaining = still_running;
            std::thread::sleep(Duration::from_millis(4));
        }
    }
}

/// Wire dictionary lookup, hover debounce, glossary links, search panel, and kanji lookup.
#[allow(clippy::too_many_arguments)]
pub(crate) fn install_dictionary_lookup_handlers(
    app: &MementoApp,
    config_ctrl: Rc<ConfigController>,
    dict_ctrl: Rc<RefCell<DictController>>,
    popup_audio_cache: Arc<Mutex<PopupAudioState>>,
    lookup_tx: ChannelSlot<LookupWorkerRequest>,
    lookup_epoch: Arc<AtomicU64>,
    hover_cursor: Rc<RefCell<Option<(String, i32)>>>,
) -> Rc<slint::Timer> {
    let hover_timer = Rc::new(slint::Timer::default());
    let hover_pending = Rc::new(RefCell::new(None::<String>));

    let lookup_config = config_ctrl.clone();
    let lookup_sender = lookup_tx.clone();
    let lookup_generation = lookup_epoch.clone();
    let lookup_app = app.as_weak();
    let lookup_audio_cache = popup_audio_cache.clone();
    app.on_lookup_word(move |word| {
        let Some(app) = lookup_app.upgrade() else {
            return;
        };
        set_popup_audio_sources(
            &app,
            &lookup_audio_cache,
            "",
            "",
            lookup_config.audio_source_entries(),
        );
        let order =
            match lookup_config.effective_setting(raw::dictionary::GROUP, raw::dictionary::ORDER) {
                Some(Value::Ints(order)) => order,
                _ => Vec::new(),
            };
        let exact_enabled = exact_matcher_enabled(&lookup_config);
        let deconj_enabled = deconj_matcher_enabled(&lookup_config);
        let mecab_enabled = mecab_matcher_enabled(&lookup_config);
        let (remove_regex, replace_lines, replace_with) = search_text_filters(&lookup_config);
        let generation = lookup_generation.fetch_add(1, Ordering::AcqRel) + 1;
        let _ = lookup_sender.send(LookupWorkerRequest::Word(WordLookupRequest {
            generation,
            query: word.to_string(),
            order,
            source_subtitle: app.get_active_sub_text().to_string(),
            excluded: lookup_config.anki_excluded_glossaries(),
            exact_enabled,
            deconj_enabled,
            mecab_enabled,
            remove_regex,
            replace_lines,
            replace_with,
        }));
    });

    let glossary_link_app = app.as_weak();
    app.on_open_glossary_link(move |href| {
        let Some(app) = glossary_link_app.upgrade() else {
            return;
        };
        if let Some(query_string) = href.strip_prefix('?') {
            if let Some((_, query)) =
                url::form_urlencoded::parse(query_string.as_bytes()).find(|(key, _)| key == "query")
            {
                app.invoke_lookup_word(query.into_owned().into());
            }
        } else if url::Url::parse(href.as_str())
            .is_ok_and(|url| matches!(url.scheme(), "http" | "https"))
        {
            let link = href.to_string();
            let weak = glossary_link_app.clone();
            std::thread::spawn(move || {
                if let Err(error) = webbrowser::open(&link) {
                    let _ = weak.upgrade_in_event_loop(move |app| {
                        app.set_status_message(
                            format!("Could not open dictionary link: {error}").into(),
                        );
                    });
                }
            });
        }
    });

    let hover_app = app.as_weak();
    let hover_config = config_ctrl.clone();
    let hover_timer_callback = hover_timer.clone();
    let hover_pending_callback = hover_pending;
    let hover_cursor_callback = hover_cursor;
    let hover_generation = lookup_epoch.clone();
    app.on_hover_word(move |word, index, control, shift, alt, meta| {
        let Some(app) = hover_app.upgrade() else {
            return;
        };
        hover_timer_callback.stop();
        if word.is_empty() {
            hover_cursor_callback.borrow_mut().take();
            hover_pending_callback.borrow_mut().take();
            hover_generation.fetch_add(1, Ordering::AcqRel);
            if index < 0 {
                ui::clear_rich_bridge_ass_lookup(&app);
            }
            return;
        }
        hover_cursor_callback.replace(Some((word.to_string(), index)));

        let method = match hover_config.effective_setting(raw::search::GROUP, raw::search::METHOD) {
            Some(Value::Int(value)) => {
                i32::try_from(value).unwrap_or(raw::search::METHOD_DEFAULT as i32)
            }
            _ => raw::search::METHOD_DEFAULT as i32,
        };
        let modifier =
            match hover_config.effective_setting(raw::search::GROUP, raw::search::MODIFIER) {
                Some(Value::Int(value)) => {
                    i32::try_from(value).unwrap_or(raw::search::MODIFIER_DEFAULT as i32)
                }
                _ => raw::search::MODIFIER_DEFAULT as i32,
            };
        let modifier_held = match modifier {
            0 => alt,
            1 => control,
            2 => shift,
            3 => meta,
            _ => false,
        };
        if method == 1 && !modifier_held {
            hover_pending_callback.borrow_mut().take();
            hover_generation.fetch_add(1, Ordering::AcqRel);
            return;
        }

        if app.get_dict_open() && app.get_selected_token_index() >= 0 && !modifier_held {
            return;
        }

        hover_generation.fetch_add(1, Ordering::AcqRel);

        let delay_ms = match hover_config.effective_setting(raw::search::GROUP, raw::search::DELAY)
        {
            Some(Value::Int(value)) => value.max(0) as u64,
            _ => raw::search::DELAY_DEFAULT as u64,
        };
        let delay_ms = if modifier_held {
            delay_ms.min(50)
        } else {
            delay_ms
        };
        hover_pending_callback.replace(Some(word.to_string()));
        app.set_selected_token_index(index);
        let pending = hover_pending_callback.clone();
        let weak = app.as_weak();
        hover_timer_callback.start(
            slint::TimerMode::SingleShot,
            Duration::from_millis(delay_ms),
            move || {
                let Some(word) = pending.borrow_mut().take() else {
                    return;
                };
                let Some(app) = weak.upgrade() else {
                    return;
                };
                let index = app.get_selected_token_index();
                if index >= 0
                    && app
                        .get_active_sub_words()
                        .row_data(index as usize)
                        .is_some_and(|token| token.query.as_str() == word)
                {
                    app.set_popup_offset_x(app.get_pending_popup_offset_x());
                    app.invoke_lookup_word(word.into());
                }
            },
        );
    });

    let search_dict_app = app.as_weak();
    let search_dict_config = config_ctrl.clone();
    let search_dict_ctrl = dict_ctrl;
    let search_audio_cache = popup_audio_cache.clone();
    app.on_search_dictionary(move |query| {
        let Some(app) = search_dict_app.upgrade() else {
            return;
        };
        if app.global::<PopupBridge>().get_rich_enabled() {
            app.global::<PopupBridge>()
                .invoke_panel_search_requested(query);
            return;
        }
        set_popup_audio_sources(
            &app,
            &search_audio_cache,
            "",
            "",
            search_dict_config.audio_source_entries(),
        );
        let hits = if query.trim().is_empty() {
            Vec::new()
        } else {
            let Ok(mut dictionary) = search_dict_ctrl.try_borrow_mut() else {
                return;
            };
            apply_dictionary_order(&search_dict_config, &mut dictionary);
            let hits = dictionary.search(&query).to_vec();
            let error = dictionary.take_search_error();
            let database_open = dictionary.database_path_is_open();
            drop(dictionary);
            if let Some(error) = error {
                app.set_status_message(format!("Dictionary search failed: {error}").into());
            } else if hits.is_empty() && !database_open && !query.trim().is_empty() {
                app.set_status_message("Dictionary database is unavailable".into());
            }
            hits
        };
        app.invoke_dismiss_definition();
        set_dictionary_rows(
            &app,
            dictionary_result_rows(&hits, &search_dict_config.anki_excluded_glossaries()),
        );
        app.invoke_reset_definition_scroll();
    });

    let kanji_app = app.as_weak();
    let kanji_config = config_ctrl;
    let kanji_audio_cache = popup_audio_cache;
    let kanji_sender = lookup_tx;
    let kanji_generation = lookup_epoch;
    app.on_lookup_kanji(move |word| {
        let Some(app) = kanji_app.upgrade() else {
            return;
        };
        set_popup_audio_sources(
            &app,
            &kanji_audio_cache,
            "",
            "",
            kanji_config.audio_source_entries(),
        );
        let Some(character) = word.chars().find(|character| {
            ('\u{3400}'..='\u{9fff}').contains(character)
                || ('\u{f900}'..='\u{faff}').contains(character)
                || ('\u{20000}'..='\u{3134f}').contains(character)
        }) else {
            app.set_status_message("No kanji in selected word".into());
            return;
        };
        let order =
            match kanji_config.effective_setting(raw::dictionary::GROUP, raw::dictionary::ORDER) {
                Some(Value::Ints(order)) => order,
                _ => Vec::new(),
            };
        let generation = kanji_generation.fetch_add(1, Ordering::AcqRel) + 1;
        let _ = kanji_sender.send(LookupWorkerRequest::Kanji(KanjiLookupRequest {
            generation,
            character: character.to_string(),
            order,
            excluded: kanji_config.anki_excluded_glossaries(),
        }));
    });

    hover_timer
}

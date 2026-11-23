//! Background worker threads (thumbnail preview with Condvar wakeup, dictionary lookup,
//! furigana, external subtitle parser, MKV EBML extractor), coalesced UI event relay,
//! and bounded graceful shutdown.

use std::cell::RefCell;
use std::collections::hash_map::DefaultHasher;
use std::collections::VecDeque;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
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
use crate::controller::dict::{self, DictController, SearchHit, TermFrequency, TermPitchGraph};
use crate::controller::subtitles::{self, ExternalSubtitleLoad};
use crate::ui::{self, dictionary_result_rows, set_dictionary_rows, SubomiApp, PopupBridge};
use subomi_config::Value;
use subomi_player::MpvInstance;
use subomi_setting::keys as raw;

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
    AddSubtitles(Vec<PathBuf>),
    SystemThemeChanged(subomi_os::SystemColorScheme),
    DictionariesImported {
        imported: usize,
        errors: Vec<String>,
        all_dicts: Vec<crate::controller::dictmanager::InstalledDictionary>,
    },
    ExternalSubtitle {
        generation: u64,
        track_id: i64,
        path: String,
        result: Result<Vec<subomi_subtitle::SubtitleEntry>, String>,
    },
    MkvFonts {
        generation: u64,
        fonts: Vec<subomi_ass::MkvFontAttachment>,
    },
    MkvTrack {
        generation: u64,
        sid: i64,
        cues: Vec<subomi_ass::MkvSubtitleCue>,
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
const THUMB_FRAME_CACHE_CAPACITY: usize = 32;
const LOOKUP_RESULT_CACHE_CAPACITY: usize = 32;
const MAX_THUMB_WIDTH: u32 = 208;
const MAX_THUMB_HEIGHT: u32 = 117;
const DEFAULT_THUMB_WIDTH: u32 = 192;
const DEFAULT_THUMB_HEIGHT: u32 = 108;

/// Fast-decode, low-RAM `libmpv` options for the timeline hover thumbnail worker.
pub(crate) fn thumb_mpv_cli_options() -> Vec<subomi_player::CliOption> {
    [
        ("config", "no"),
        ("aid", "no"),
        ("sid", "no"),
        ("secondary-sid", "no"),
        ("osc", "no"),
        ("pause", "yes"),
        ("ytdl", "no"),
        ("terminal", "no"),
        ("hwdec", "no"),
        ("hr-seek", "no"),
        ("load-scripts", "no"),
        ("vd-lavc-fast", "yes"),
        ("vd-lavc-skiploopfilter", "all"),
        ("vd-lavc-threads", "2"),
        ("sws-fast", "yes"),
        ("demuxer-max-bytes", "4MiB"),
        ("demuxer-max-back-bytes", "1MiB"),
        ("demuxer-readahead-secs", "1"),
    ]
    .into_iter()
    .map(|(name, value)| subomi_player::CliOption {
        name: name.into(),
        value: value.into(),
    })
    .collect()
}

/// Quantize a timeline hover timestamp (seconds) to the nearest non-negative
/// integer second (`NaN`, non-finite, or negative inputs clamp to `0`).
pub(crate) fn quantize_thumb_time_sec(time_sec: f64) -> i64 {
    if !time_sec.is_finite() || time_sec <= 0.0 {
        return 0;
    }
    time_sec.max(0.0).round() as i64
}

pub(crate) type CachedThumbFrame = (u32, u32, SharedPixelBuffer<Rgba8Pixel>);

/// Bounded LRU cache of decoded timeline preview frames (~2–3 MiB at capacity 32).
pub(crate) struct ThumbFrameCache {
    capacity: usize,
    entries: VecDeque<((String, i64), CachedThumbFrame)>,
}

impl Default for ThumbFrameCache {
    fn default() -> Self {
        Self::new(THUMB_FRAME_CACHE_CAPACITY)
    }
}

impl ThumbFrameCache {
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            capacity,
            entries: VecDeque::with_capacity(capacity),
        }
    }

    pub(crate) fn get(&mut self, path: &str, quantized_sec: i64) -> Option<CachedThumbFrame> {
        let pos = self
            .entries
            .iter()
            .position(|((p, s), _)| *s == quantized_sec && p == path)?;
        if pos + 1 < self.entries.len() {
            if let Some(entry) = self.entries.remove(pos) {
                self.entries.push_back(entry);
            }
        }
        self.entries.back().map(|(_, frame)| frame.clone())
    }

    pub(crate) fn insert(
        &mut self,
        path: String,
        quantized_sec: i64,
        frame: CachedThumbFrame,
    ) {
        if self.capacity == 0 {
            return;
        }
        if let Some(pos) = self
            .entries
            .iter()
            .position(|((p, s), _)| *s == quantized_sec && p == &path)
        {
            self.entries.remove(pos);
        } else if self.entries.len() >= self.capacity {
            self.entries.pop_front();
        }
        self.entries.push_back(((path, quantized_sec), frame));
    }

    pub(crate) fn clear(&mut self) {
        self.entries.clear();
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

pub(crate) fn resolve_thumbnail_media_path(path: &str) -> Option<PathBuf> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.starts_with("file://") {
        let decoded = subomi_os::decode_file_uri(trimmed)?;
        return decoded.is_file().then_some(decoded);
    }
    let candidate = PathBuf::from(trimmed);
    candidate.is_file().then_some(candidate)
}

pub(crate) fn compute_thumbnail_dimensions(video_w: u32, video_h: u32) -> (u32, u32) {
    if video_w == 0 || video_h == 0 {
        return (DEFAULT_THUMB_WIDTH, DEFAULT_THUMB_HEIGHT);
    }
    let scale = (MAX_THUMB_WIDTH as f64 / video_w as f64)
        .min(MAX_THUMB_HEIGHT as f64 / video_h as f64);
    let width = ((video_w as f64 * scale).round() as u32).clamp(1, MAX_THUMB_WIDTH);
    let height = ((video_h as f64 * scale).round() as u32).clamp(1, MAX_THUMB_HEIGHT);
    (width, height)
}

struct ThumbMpvState {
    // Declared before `inst` so the software renderer handle drops before `MpvInstance`.
    renderer: subomi_player::mpv::MpvSoftwareRenderer,
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

#[derive(Clone)]
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

pub(crate) type TermMetadataGraphs = Vec<(String, String, Vec<TermPitchGraph>, Vec<TermFrequency>)>;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct LookupCacheKey {
    pub(crate) query: String,
    pub(crate) order_hash: u64,
    pub(crate) options_hash: u64,
}

pub(crate) fn hash_lookup_order(order: &[i64]) -> u64 {
    let mut hasher = DefaultHasher::new();
    order.hash(&mut hasher);
    hasher.finish()
}

pub(crate) fn hash_lookup_options(
    exact_enabled: bool,
    deconj_enabled: bool,
    mecab_enabled: bool,
    remove_regex: &str,
    replace_lines: bool,
    replace_with: &str,
) -> u64 {
    let mut hasher = DefaultHasher::new();
    exact_enabled.hash(&mut hasher);
    deconj_enabled.hash(&mut hasher);
    mecab_enabled.hash(&mut hasher);
    remove_regex.hash(&mut hasher);
    replace_lines.hash(&mut hasher);
    replace_with.hash(&mut hasher);
    hasher.finish()
}

impl LookupCacheKey {
    pub(crate) fn from_request(request: &WordLookupRequest) -> Self {
        Self {
            query: request.query.clone(),
            order_hash: hash_lookup_order(&request.order),
            options_hash: hash_lookup_options(
                request.exact_enabled,
                request.deconj_enabled,
                request.mecab_enabled,
                &request.remove_regex,
                request.replace_lines,
                &request.replace_with,
            ),
        }
    }
}

#[derive(Clone)]
pub(crate) struct CachedWordLookup {
    pub(crate) result: WordLookupResult,
    pub(crate) pitch: String,
    pub(crate) freq: String,
    pub(crate) jpdb: String,
    pub(crate) graphs: TermMetadataGraphs,
}

/// Bounded LRU cache of recent word lookup hits + resolved term metadata inside
/// `lookup_worker` so re-hovering recently scanned words skips SQLite completely.
pub(crate) struct LookupResultCache {
    capacity: usize,
    database_signature: Option<dict::DbFileFingerprint>,
    entries: VecDeque<(LookupCacheKey, CachedWordLookup)>,
}

impl Default for LookupResultCache {
    fn default() -> Self {
        Self::new(LOOKUP_RESULT_CACHE_CAPACITY)
    }
}

impl LookupResultCache {
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            capacity,
            database_signature: None,
            entries: VecDeque::with_capacity(capacity),
        }
    }

    /// Synchronize against the current dictionary database fingerprint, clearing
    /// all cached entries whenever the DB changes. Returns `true` if invalidated.
    pub(crate) fn sync_database_signature(&mut self, signature: dict::DbFileFingerprint) -> bool {
        if self.database_signature.as_ref() != Some(&signature) {
            self.clear();
            self.database_signature = Some(signature);
            true
        } else {
            false
        }
    }

    pub(crate) fn get(&mut self, key: &LookupCacheKey) -> Option<CachedWordLookup> {
        let pos = self.entries.iter().position(|(k, _)| k == key)?;
        if pos + 1 < self.entries.len() {
            if let Some(entry) = self.entries.remove(pos) {
                self.entries.push_back(entry);
            }
        }
        self.entries.back().map(|(_, value)| value.clone())
    }

    pub(crate) fn insert(&mut self, key: LookupCacheKey, value: CachedWordLookup) {
        if self.capacity == 0 {
            return;
        }
        if let Some(pos) = self.entries.iter().position(|(k, _)| k == &key) {
            self.entries.remove(pos);
        } else if self.entries.len() >= self.capacity {
            self.entries.pop_front();
        }
        self.entries.push_back((key, value));
    }

    pub(crate) fn clear(&mut self) {
        self.entries.clear();
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
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
    app: &SubomiApp,
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
        app: &SubomiApp,
        dictionary_database_path: PathBuf,
        shared_media_generation: Arc<AtomicU64>,
        lookup_epoch: Arc<AtomicU64>,
        ui_relay: UiRelay,
    ) -> Self {
        let mut handles = Vec::with_capacity(5);

        // 1. Timeline hover thumbnail preview worker (Condvar-driven, zero sleep-polling,
        //    quantized 32-frame LRU cache, and 40s idle teardown to reclaim libmpv RAM).
        let (thumb_raw_tx, thumb_rx) = std::sync::mpsc::channel::<ThumbRequest>();
        let thumb_tx = ChannelSlot::new(thumb_raw_tx);
        let thumb_worker_app = app.as_weak();
        let thumb_handle = std::thread::spawn(move || {
            let mut thumb_mpv: Option<ThumbMpvState> = None;
            let mut loaded_path = String::new();
            let mut thumb_cache = ThumbFrameCache::default();
            let mut last_served: Option<(String, i64)> = None;
            loop {
                let mut req = if thumb_mpv.is_some() || !thumb_cache.is_empty() {
                    match thumb_rx.recv_timeout(THUMB_IDLE_TIMEOUT) {
                        Ok(req) => req,
                        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                            let had_cached = !thumb_cache.is_empty();
                            thumb_cache.clear();
                            last_served = None;
                            loaded_path.clear();
                            if let Some(state) = thumb_mpv.take() {
                                drop(state);
                                crate::app::trim_os_heap();
                            } else if had_cached {
                                crate::app::trim_os_heap();
                            }
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
                        let had_cached = !thumb_cache.is_empty();
                        thumb_cache.clear();
                        last_served = None;
                        loaded_path.clear();
                        if let Some(state) = thumb_mpv.take() {
                            drop(state);
                            crate::app::trim_os_heap();
                        } else if had_cached {
                            crate::app::trim_os_heap();
                        }
                        continue;
                    }
                    ThumbRequest::Seek { path, time_sec } => {
                        let Some(resolved_path) = resolve_thumbnail_media_path(&path) else {
                            continue;
                        };
                        let resolved_str = resolved_path.to_string_lossy().into_owned();
                        let quantized_sec = quantize_thumb_time_sec(time_sec);

                        if !loaded_path.is_empty() && loaded_path != resolved_str {
                            thumb_cache.clear();
                            last_served = None;
                        }

                        if last_served
                            .as_ref()
                            .is_some_and(|(p, s)| *s == quantized_sec && p == &resolved_str)
                        {
                            continue;
                        }

                        if let Some((thumb_w, thumb_h, buffer)) =
                            thumb_cache.get(&resolved_str, quantized_sec)
                        {
                            last_served = Some((resolved_str, quantized_sec));
                            let _ = thumb_worker_app.upgrade_in_event_loop(move |app| {
                                app.set_preview_box_width(thumb_w as f32);
                                app.set_preview_box_height(thumb_h as f32);
                                app.set_thumbnail_frame(slint::Image::from_rgba8(buffer));
                            });
                            continue;
                        }

                        if thumb_mpv.is_none() {
                            let opts = thumb_mpv_cli_options();
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
                        if loaded_path != resolved_str {
                            if state.inst.load_file(&resolved_str).is_err() {
                                continue;
                            }
                            loaded_path = resolved_str.clone();
                            wait_for_render_update(
                                &state.inst,
                                &state.wake_signal,
                                Duration::from_millis(160),
                            );
                            let _ = state.inst.poll_playback_state();
                        }
                        let _ = state.inst.command(&[
                            "seek",
                            &format!("{:.3}", quantized_sec as f64),
                            "absolute+keyframes",
                        ]);
                        wait_for_render_update(
                            &state.inst,
                            &state.wake_signal,
                            Duration::from_millis(80),
                        );
                        let _ = state.inst.poll_playback_state();
                        let (video_w, video_h) = state.inst.cached_video_dimensions();
                        let (thumb_w, thumb_h) = compute_thumbnail_dimensions(video_w, video_h);
                        let mut buffer = SharedPixelBuffer::<Rgba8Pixel>::new(thumb_w, thumb_h);
                        if unsafe {
                            state
                                .renderer
                                .render_rgba(thumb_w, thumb_h, buffer.make_mut_bytes())
                        }
                        .is_ok()
                        {
                            thumb_cache.insert(
                                resolved_str.clone(),
                                quantized_sec,
                                (thumb_w, thumb_h, buffer.clone()),
                            );
                            last_served = Some((resolved_str, quantized_sec));
                            let _ = thumb_worker_app.upgrade_in_event_loop(move |app| {
                                app.set_preview_box_width(thumb_w as f32);
                                app.set_preview_box_height(thumb_h as f32);
                                app.set_thumbnail_frame(slint::Image::from_rgba8(buffer));
                            });
                        }
                    }
                }
            }
        });
        handles.push(("thumb_worker", thumb_handle));

        // 2. Dictionary & kanji lookup worker (off-UI-thread SQLite queries + 32-entry re-hover LRU).
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
            let mut lookup_cache = LookupResultCache::default();
            lookup_cache.sync_database_signature(database_signature);
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
                    lookup_cache.sync_database_signature(signature);
                }
                match request {
                    LookupWorkerRequest::Word(request) => {
                        let cache_key = LookupCacheKey::from_request(&request);
                        if let Some(cached) = lookup_cache.get(&cache_key) {
                            if request.generation != worker_epoch.load(Ordering::Acquire) {
                                continue;
                            }
                            let cached_result = WordLookupResult {
                                pitch: cached.pitch,
                                freq: cached.freq,
                                jpdb: cached.jpdb,
                                ..cached.result
                            };
                            let graphs = cached.graphs;
                            let generation = request.generation;
                            let query = request.query;
                            let source_subtitle = request.source_subtitle;
                            let excluded = request.excluded;
                            let event_epoch = worker_epoch.clone();
                            let _ = worker_app.upgrade_in_event_loop(move |app| {
                                if generation == event_epoch.load(Ordering::Acquire)
                                    && app.get_active_sub_text().as_str() == source_subtitle
                                {
                                    display_word_lookup(&app, cached_result, &query, &excluded);
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
                            continue;
                        }

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
                        let should_cache = result.error.is_none() && result.database_open;
                        let cached_result_base = should_cache.then(|| result.clone());
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
                            let mut metadata_complete = true;
                            for (expression, reading) in pitch_terms {
                                if generation != worker_epoch.load(Ordering::Acquire) {
                                    metadata_complete = false;
                                    break;
                                }
                                let (term_graphs, frequencies) = dictionary
                                    .get_pitch_graphs_and_frequencies(&expression, &reading);
                                graphs.push((expression, reading, term_graphs, frequencies));
                            }
                            if !metadata_complete
                                || generation != worker_epoch.load(Ordering::Acquire)
                            {
                                continue;
                            }
                            if let Some(base) = cached_result_base {
                                lookup_cache.insert(
                                    cache_key,
                                    CachedWordLookup {
                                        result: base,
                                        pitch: pitch.clone(),
                                        freq: freq.clone(),
                                        jpdb: jpdb.clone(),
                                        graphs: graphs.clone(),
                                    },
                                );
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
                        } else if meta_headword.is_empty() {
                            if let Some(base) = cached_result_base {
                                lookup_cache.insert(
                                    cache_key,
                                    CachedWordLookup {
                                        result: base,
                                        pitch: String::new(),
                                        freq: String::new(),
                                        jpdb: String::new(),
                                        graphs: Vec::new(),
                                    },
                                );
                            }
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
                        if let Ok(fonts) = subomi_ass::extract_mkv_fonts(&path) {
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
                            subomi_ass::extract_mkv_subtitle_track(&path, track_number)
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
    app: &SubomiApp,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compute_thumbnail_dimensions_preserves_aspect_ratio_up_to_208x117() {
        assert_eq!(compute_thumbnail_dimensions(0, 0), (192, 108));
        assert_eq!(compute_thumbnail_dimensions(1920, 1080), (208, 117));
        assert_eq!(compute_thumbnail_dimensions(1440, 1080), (156, 117));
        assert_eq!(compute_thumbnail_dimensions(2560, 1080), (208, 88));
        assert_eq!(compute_thumbnail_dimensions(1080, 1920), (66, 117));
    }

    #[test]
    fn resolve_thumbnail_media_path_accepts_file_uri() {
        let dir = std::env::temp_dir().join(format!(
            "subomi-thumb-uri-{}",
            std::process::id()
        ));
        let _ = std::fs::create_dir_all(&dir);
        let file = dir.join("sample clip.mkv");
        std::fs::write(&file, b"demo").expect("write temp file");
        let uri = format!(
            "file://{}",
            file.to_string_lossy().replace(' ', "%20")
        );
        assert_eq!(resolve_thumbnail_media_path(&uri), Some(file.clone()));
        assert_eq!(
            resolve_thumbnail_media_path(&file.to_string_lossy()),
            Some(file.clone())
        );
        assert_eq!(resolve_thumbnail_media_path(""), None);
        let _ = std::fs::remove_file(&file);
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn thumb_mpv_cli_options_contains_fast_decode_and_low_ram_flags() {
        let opts = thumb_mpv_cli_options();
        let has_opt = |name: &str, value: &str| {
            opts.iter()
                .any(|opt| opt.name == name && opt.value == value)
        };
        for (name, value) in [
            ("config", "no"),
            ("aid", "no"),
            ("sid", "no"),
            ("secondary-sid", "no"),
            ("osc", "no"),
            ("pause", "yes"),
            ("ytdl", "no"),
            ("terminal", "no"),
            ("hwdec", "no"),
            ("hr-seek", "no"),
            ("load-scripts", "no"),
            ("vd-lavc-fast", "yes"),
            ("vd-lavc-skiploopfilter", "all"),
            ("vd-lavc-threads", "2"),
            ("sws-fast", "yes"),
            ("demuxer-max-bytes", "4MiB"),
            ("demuxer-max-back-bytes", "1MiB"),
            ("demuxer-readahead-secs", "1"),
        ] {
            assert!(
                has_opt(name, value),
                "thumb_mpv_cli_options missing {name}={value}"
            );
        }
    }

    #[test]
    fn quantize_thumb_time_sec_rounds_and_clamps_invalid_values() {
        assert_eq!(quantize_thumb_time_sec(f64::NAN), 0);
        assert_eq!(quantize_thumb_time_sec(f64::NEG_INFINITY), 0);
        assert_eq!(quantize_thumb_time_sec(f64::INFINITY), 0);
        assert_eq!(quantize_thumb_time_sec(-12.7), 0);
        assert_eq!(quantize_thumb_time_sec(-0.0), 0);
        assert_eq!(quantize_thumb_time_sec(0.0), 0);
        assert_eq!(quantize_thumb_time_sec(0.49), 0);
        assert_eq!(quantize_thumb_time_sec(0.5), 1);
        assert_eq!(quantize_thumb_time_sec(42.4), 42);
        assert_eq!(quantize_thumb_time_sec(42.6), 43);
    }

    #[test]
    fn thumb_frame_cache_supports_hit_lru_promotion_eviction_at_32_and_clear() {
        let make_frame = |tag: u8| {
            let mut buf = SharedPixelBuffer::<Rgba8Pixel>::new(4, 2);
            buf.make_mut_bytes().fill(tag);
            (4u32, 2u32, buf)
        };

        let mut cache = ThumbFrameCache::default();
        assert!(cache.is_empty());

        for sec in 0..32i64 {
            cache.insert("/ep01.mkv".into(), sec, make_frame(sec as u8));
        }
        assert_eq!(cache.len(), 32);

        // Hit on sec 0 promotes it to MRU so sec 1 becomes LRU.
        let (w, h, buf0) = cache.get("/ep01.mkv", 0).expect("sec 0 must hit");
        assert_eq!((w, h), (4, 2));
        assert_eq!(buf0.as_bytes()[0], 0);

        // Inserting a 33rd frame evicts sec 1 (LRU), NOT promoted sec 0!
        cache.insert("/ep01.mkv".into(), 32, make_frame(32));
        assert_eq!(cache.len(), 32);
        assert!(cache.get("/ep01.mkv", 1).is_none(), "LRU sec 1 must be evicted");
        assert!(cache.get("/ep01.mkv", 0).is_some(), "promoted sec 0 must survive");
        assert!(cache.get("/ep01.mkv", 32).is_some(), "new sec 32 must hit");
        assert!(
            cache.get("/ep02.mkv", 0).is_none(),
            "different resolved path must miss"
        );

        cache.clear();
        assert!(cache.is_empty());
        assert!(cache.get("/ep01.mkv", 0).is_none());
    }

    #[test]
    fn lookup_result_cache_handles_hits_lru_eviction_option_misses_and_db_invalidation() {
        let make_request = |query: &str, order: Vec<i64>, exact: bool, regex: &str| WordLookupRequest {
            generation: 1,
            query: query.into(),
            order,
            source_subtitle: "sub".into(),
            excluded: Vec::new(),
            exact_enabled: exact,
            deconj_enabled: true,
            mecab_enabled: true,
            remove_regex: regex.into(),
            replace_lines: false,
            replace_with: String::new(),
        };
        let make_cached = |word: &str| CachedWordLookup {
            result: WordLookupResult {
                hits: vec![SearchHit::new(word, "よみ", "meaning", "", 10, false)],
                error: None,
                database_open: true,
                matched_units: word.encode_utf16().count(),
                headword: word.into(),
                reading: "よみ".into(),
                pitch: String::new(),
                freq: String::new(),
                jpdb: String::new(),
            },
            pitch: "[0]".into(),
            freq: "#120".into(),
            jpdb: "JPDB 45".into(),
            graphs: vec![(
                word.into(),
                "よみ".into(),
                vec![TermPitchGraph {
                    dictionary: "Kanjium".into(),
                    mora: vec!["よ".into(), "み".into()],
                    position: 0,
                }],
                vec![TermFrequency {
                    dictionary: "JPDB".into(),
                    value: "45".into(),
                }],
            )],
        };

        let mut cache = LookupResultCache::default();
        let sig_v1: dict::DbFileFingerprint = (Some((1024, None)), None);
        let sig_v2: dict::DbFileFingerprint = (Some((2048, None)), None);
        assert!(cache.sync_database_signature(sig_v1));
        assert!(!cache.sync_database_signature(sig_v1));

        // Fill cache to capacity 32.
        let mut keys = Vec::new();
        for i in 0..32usize {
            let req = make_request(&format!("word-{i}"), vec![1, 2], true, "");
            let key = LookupCacheKey::from_request(&req);
            cache.insert(key.clone(), make_cached(&format!("word-{i}")));
            keys.push(key);
        }
        assert_eq!(cache.len(), 32);

        // Hit on keys[0] promotes it to MRU so keys[1] becomes LRU.
        let hit0 = cache.get(&keys[0]).expect("keys[0] must hit");
        assert_eq!(hit0.result.headword, "word-0");
        assert_eq!(hit0.pitch, "[0]");
        assert_eq!(hit0.graphs.len(), 1);

        // Insert 33rd entry -> evicts keys[1], preserves promoted keys[0].
        let key32 = LookupCacheKey::from_request(&make_request("word-32", vec![1, 2], true, ""));
        cache.insert(key32.clone(), make_cached("word-32"));
        assert_eq!(cache.len(), 32);
        assert!(cache.get(&keys[1]).is_none(), "LRU keys[1] must be evicted");
        assert!(cache.get(&keys[0]).is_some(), "promoted keys[0] must survive");
        assert!(cache.get(&key32).is_some(), "newly inserted key32 must hit");

        // Different dictionary order or search options MUST miss!
        let diff_order =
            LookupCacheKey::from_request(&make_request("word-0", vec![2, 1], true, ""));
        let diff_exact =
            LookupCacheKey::from_request(&make_request("word-0", vec![1, 2], false, ""));
        let diff_regex =
            LookupCacheKey::from_request(&make_request("word-0", vec![1, 2], true, r"\(.*\)"));
        assert!(cache.get(&diff_order).is_none(), "different dictionary order must miss");
        assert!(cache.get(&diff_exact).is_none(), "different matcher option must miss");
        assert!(cache.get(&diff_regex).is_none(), "different remove_regex must miss");

        // Changing DB file fingerprint clears every cached entry!
        assert!(cache.sync_database_signature(sig_v2));
        assert!(cache.is_empty());
        assert!(cache.get(&keys[0]).is_none());
        assert!(cache.get(&key32).is_none());
    }
}

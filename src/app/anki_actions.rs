//! Anki note creation, note updating, Anki browser navigation,
//! popup pronunciation audio source resolution, and word audio playback.

use std::cell::{Cell, RefCell};
use std::cmp::Reverse;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use slint::{ComponentHandle, Model as _, VecModel};

use crate::app::preferences::dictionary_order_map;
use crate::controller::anki::AnkiController;
use crate::controller::config::{AnkiNoteConfig, AudioSourceEntry, ConfigController};
use crate::controller::dict::{shared_readonly_db, TermFrequency, TermPitchGraph};
use crate::controller::player::filter_subtitle_text;
use crate::controller::subtitles::SubtitleController;
use crate::controller::{anki, anki_media, audio, media};
use crate::ui::{
    self, popup_pitch_graphs, DictResult, MementoApp, PopupAudioSource, PopupBridge, PopupFrequency,
};
use memento_player::{subtitle_raw_time, subtitle_to_media_time, MpvInstance};
use memento_setting::enums::AudioSourceType;

pub(crate) const AUDIO_STATUS_DEFAULT: i32 = 0;
pub(crate) const AUDIO_STATUS_AVAILABLE: i32 = 1;
pub(crate) const AUDIO_STATUS_FAILED: i32 = 2;
pub(crate) const AUDIO_STATUS_CHECKING: i32 = 3;
const MAX_CACHED_AUDIO_TERMS: usize = 64;
const WARNING_TOAST_DURATION: Duration = Duration::from_secs(4);

static STATUS_TOAST_GENERATION: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug, Default)]
pub(crate) struct CachedTermAudioSources {
    pub(crate) sources: Vec<AudioSourceEntry>,
    pub(crate) statuses: Vec<i32>,
}

#[derive(Default)]
pub(crate) struct PopupAudioState {
    pub(crate) serial: u64,
    pub(crate) expression: String,
    pub(crate) reading: String,
    pub(crate) sources: Vec<AudioSourceEntry>,
    pub(crate) statuses: Vec<i32>,
    pub(crate) per_term: HashMap<(String, String), CachedTermAudioSources>,
}

impl PopupAudioState {
    pub(crate) fn clear_all(&mut self) {
        self.serial = self.serial.wrapping_add(1);
        self.expression.clear();
        self.reading.clear();
        self.sources.clear();
        self.statuses.clear();
        self.per_term.clear();
    }

    fn store_term_cache(
        &mut self,
        expression: &str,
        reading: &str,
        sources: Vec<AudioSourceEntry>,
        statuses: Vec<i32>,
    ) {
        if expression.is_empty() {
            return;
        }
        let key = (expression.to_owned(), reading.to_owned());
        if self.per_term.len() >= MAX_CACHED_AUDIO_TERMS && !self.per_term.contains_key(&key) {
            self.per_term.clear();
        }
        self.per_term.insert(
            key,
            CachedTermAudioSources {
                sources,
                statuses,
            },
        );
    }
}

/// Metadata captured from `RichUiBridge` for each row in the active popup/nested/panel view so
/// SQLite Anki enrichment can match exact `(dic_id, rowid)` entries even when multiple
/// dictionaries share a title or ordering differs.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ActiveRowHitMeta {
    pub(crate) dictionary_id: i64,
    pub(crate) dict_name: String,
    pub(crate) raw_glossary: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AnkiAddTarget {
    Root,
    Nested(i32),
    Panel,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct AnkiAddOverrideState {
    target: AnkiAddTarget,
    audio_source_override: Option<i32>,
}

static ACTIVE_GLOSSARY_SELECTIONS: OnceLock<Mutex<HashMap<(i32, i32), Vec<String>>>> =
    OnceLock::new();
static ACTIVE_ROW_HIT_META: OnceLock<Mutex<HashMap<(i32, i32), Vec<ActiveRowHitMeta>>>> =
    OnceLock::new();

fn active_glossary_selections_map() -> &'static Mutex<HashMap<(i32, i32), Vec<String>>> {
    ACTIVE_GLOSSARY_SELECTIONS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn active_row_hit_meta_map() -> &'static Mutex<HashMap<(i32, i32), Vec<ActiveRowHitMeta>>> {
    ACTIVE_ROW_HIT_META.get_or_init(|| Mutex::new(HashMap::new()))
}

pub(crate) fn set_active_glossary_selections(bridge_id: i32, depth: i32, selections: Vec<String>) {
    let mut map = active_glossary_selections_map()
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    if selections.is_empty() {
        map.remove(&(bridge_id, depth));
    } else {
        if map.len() > 128 && !map.contains_key(&(bridge_id, depth)) {
            map.clear();
        }
        map.insert((bridge_id, depth), selections);
    }
}

pub(crate) fn get_active_glossary_selections(bridge_id: i32, depth: i32) -> Vec<String> {
    active_glossary_selections_map()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get(&(bridge_id, depth))
        .cloned()
        .unwrap_or_default()
}

pub(crate) fn clear_active_glossary_selections(bridge_id: i32, depth: Option<i32>) {
    let mut map = active_glossary_selections_map()
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    match depth {
        Some(d) => {
            map.remove(&(bridge_id, d));
        }
        None => {
            map.retain(|&(id, _), _| id != bridge_id);
        }
    }
}

pub(crate) fn set_active_row_hit_meta(bridge_id: i32, depth: i32, rows: Vec<ActiveRowHitMeta>) {
    let mut map = active_row_hit_meta_map()
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    if rows.is_empty() {
        map.remove(&(bridge_id, depth));
    } else {
        if map.len() > 128 && !map.contains_key(&(bridge_id, depth)) {
            map.clear();
        }
        map.insert((bridge_id, depth), rows);
    }
}

pub(crate) fn get_active_row_hit_meta(bridge_id: i32, depth: i32) -> Vec<ActiveRowHitMeta> {
    active_row_hit_meta_map()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get(&(bridge_id, depth))
        .cloned()
        .unwrap_or_default()
}

pub(crate) fn clear_active_row_hit_meta(bridge_id: i32, depth: Option<i32>) {
    let mut map = active_row_hit_meta_map()
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    match depth {
        Some(d) => {
            map.remove(&(bridge_id, d));
        }
        None => {
            map.retain(|&(id, _), _| id != bridge_id);
        }
    }
}

thread_local! {
    static PENDING_ANKI_ADD_OVERRIDE: Cell<Option<AnkiAddOverrideState>> = const { Cell::new(None) };
    static REGISTERED_AUDIO_TARGETS: RefCell<Vec<AudioInvalidationTarget>> =
        const { RefCell::new(Vec::new()) };
}

struct AudioInvalidationTarget {
    app: slint::Weak<MementoApp>,
    cache: Arc<Mutex<PopupAudioState>>,
    player: Rc<RefCell<Option<audio::AudioController>>>,
}

/// RAII scope guard passing nested/panel target & optional explicit audio source index into
/// `on_add_anki_note` without altering public Slint callback signatures.
pub(crate) struct ScopedAnkiAddOverride {
    prev: Option<AnkiAddOverrideState>,
}

impl ScopedAnkiAddOverride {
    pub(crate) fn new(target: AnkiAddTarget, audio_source_override: Option<i32>) -> Self {
        let prev = PENDING_ANKI_ADD_OVERRIDE.with(|slot| {
            slot.replace(Some(AnkiAddOverrideState {
                target,
                audio_source_override,
            }))
        });
        Self { prev }
    }
}

impl Drop for ScopedAnkiAddOverride {
    fn drop(&mut self) {
        PENDING_ANKI_ADD_OVERRIDE.with(|slot| slot.set(self.prev));
    }
}

fn current_anki_add_override() -> Option<AnkiAddOverrideState> {
    PENDING_ANKI_ADD_OVERRIDE.with(Cell::get)
}

pub(crate) fn register_audio_invalidation_target(
    app: slint::Weak<MementoApp>,
    cache: Arc<Mutex<PopupAudioState>>,
    player: Rc<RefCell<Option<audio::AudioController>>>,
) {
    REGISTERED_AUDIO_TARGETS.with(|slot| {
        let mut targets = slot.borrow_mut();
        targets.retain(|entry| entry.app.upgrade().is_some());
        targets.push(AudioInvalidationTarget { app, cache, player });
    });
}

/// Called by `preferences::sync_audio_preferences` whenever `audio/sources` or
/// `audio/download-timeout-ms` change in Settings (H3.8 / `#1876`).
pub(crate) fn invalidate_audio_caches_for_config_change(timeout_ms: u64) {
    audio::set_download_timeout_ms(timeout_ms);
    REGISTERED_AUDIO_TARGETS.with(|slot| {
        let mut targets = slot.borrow_mut();
        targets.retain(|entry| {
            let Some(app) = entry.app.upgrade() else {
                return false;
            };
            if let Some(ctrl) = entry.player.borrow().as_ref() {
                ctrl.set_download_timeout_ms(timeout_ms);
                ctrl.clear_cache();
            }
            {
                let mut state = entry
                    .cache
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                state.clear_all();
            }
            app.set_popup_audio_sources(Rc::new(VecModel::from(Vec::<PopupAudioSource>::new())).into());
            app.set_popup_audio_source_statuses(Rc::new(VecModel::from(Vec::<i32>::new())).into());
            app.set_popup_selected_audio_source(-1);
            app.set_popup_audio_sources_loading(false);
            true
        });
    });
}

/// Displays a non-blocking warning toast on `MementoApp` for non-fatal Anki media warnings
/// and auto-clears it cleanly after ~4 seconds (C3.2).
pub(crate) fn show_warning_toast(app: &MementoApp, warnings: &[String]) {
    let mut unique = Vec::new();
    for warning in warnings {
        let trimmed = warning.trim();
        if !trimmed.is_empty() && !unique.iter().any(|existing: &String| existing == trimmed) {
            unique.push(trimmed.to_owned());
        }
    }
    if unique.is_empty() {
        return;
    }
    let message = unique.join("; ");
    let generation = STATUS_TOAST_GENERATION
        .fetch_add(1, Ordering::AcqRel)
        .wrapping_add(1);
    app.set_status_toast_is_warning(true);
    app.set_status_toast_text(message.clone().into());
    let weak = app.as_weak();
    slint::Timer::single_shot(WARNING_TOAST_DURATION, move || {
        if STATUS_TOAST_GENERATION.load(Ordering::Acquire) != generation {
            return;
        }
        if let Some(app) = weak.upgrade() {
            if app.get_status_toast_text().as_str() == message {
                app.set_status_toast_text("".into());
                app.set_status_toast_is_warning(false);
            }
        }
    });
}

fn parse_hud_timestamp(raw: &str) -> Option<f64> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let parts: Vec<&str> = trimmed.split(':').collect();
    match parts.as_slice() {
        [mm, ss] => {
            let mins: u64 = mm.trim().parse().ok()?;
            let secs: f64 = ss.trim().parse().ok()?;
            if !secs.is_finite() || secs < 0.0 || secs >= 60.0 {
                return None;
            }
            Some(mins as f64 * 60.0 + secs)
        }
        [hh, mm, ss] => {
            let hours: u64 = hh.trim().parse().ok()?;
            let mins: u64 = mm.trim().parse().ok()?;
            let secs: f64 = ss.trim().parse().ok()?;
            if mins >= 60 || !secs.is_finite() || secs < 0.0 || secs >= 60.0 {
                return None;
            }
            Some(hours as f64 * 3600.0 + mins as f64 * 60.0 + secs)
        }
        _ => None,
    }
}

/// Parses Memento's manual A/B Audio Context HUD pill text (e.g.
/// `"Audio Context: 01:23.45 → 01:28.90"`, `"Audio Context: 1:02:03.45 → 1:02:08.90"`,
/// or open-ended `"Audio Context: 01:23.45 → ..."` using `current_pos`) into `(start, end)`
/// in media playback seconds (H5.2 / `#136`).
pub(crate) fn parse_manual_audio_context_hud(hud_text: &str, current_pos: f64) -> Option<(f64, f64)> {
    let trimmed = hud_text.trim();
    if trimmed.is_empty() {
        return None;
    }
    let body = if let Some(rest) = trimmed.strip_prefix("Audio Context:") {
        rest.trim()
    } else if let Some((_, rest)) = trimmed.split_once(':') {
        // Handle case-insensitive "audio context: ..." while avoiding stripping `HH:MM:SS`
        if trimmed[..trimmed.find(':')?].eq_ignore_ascii_case("audio context") {
            rest.trim()
        } else {
            trimmed
        }
    } else {
        trimmed
    };
    let (lhs, rhs) = body
        .split_once('→')
        .or_else(|| body.split_once("->"))?;
    let a = parse_hud_timestamp(lhs)?;
    let rhs_trimmed = rhs.trim();
    let b = if rhs_trimmed == "..." || rhs_trimmed == "…" {
        if !current_pos.is_finite() || current_pos < 0.0 {
            return None;
        }
        current_pos
    } else {
        parse_hud_timestamp(rhs_trimmed)?
    };
    let (start, end) = if b < a { (b, a) } else { (a, b) };
    if start.is_finite() && end.is_finite() && start >= 0.0 && end > start {
        Some((start, end))
    } else {
        None
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ResolvedAnkiMediaTimings {
    pub(crate) subtitle_start: f64,
    pub(crate) subtitle_end: f64,
    pub(crate) context_start: f64,
    pub(crate) context_end: f64,
}

fn convert_subtitle_range_to_media(
    start: f64,
    end: f64,
    sub_speed: f64,
    sub_delay: f64,
) -> Option<(f64, f64)> {
    if !(end > start) || !start.is_finite() || !end.is_finite() {
        return None;
    }
    let m_start = subtitle_to_media_time(start, sub_speed, sub_delay);
    let m_end = subtitle_to_media_time(end, sub_speed, sub_delay);
    (m_end > m_start).then_some((m_start, m_end))
}

/// Computes effective media-timeline `(subtitle_start, subtitle_end, context_start, context_end)`
/// for `{audio-media}`, `{audio-context}`, and `{video}`:
/// - Applies `memento_player::subtitle_to_media_time(cue_t, sub_speed, sub_delay)` to subtitle
///   cues and multi-subtitle selection ranges (C3.1).
/// - Attaches manual A/B Audio Context HUD timestamps (already in media time) when mining from
///   Manual Search / OCR or when no active subtitle cue timing exists (H5.2 / `#136`).
#[allow(clippy::too_many_arguments)]
pub(crate) fn resolve_anki_media_timings(
    raw_subtitle_range: Option<(f64, f64)>,
    raw_selected_range: Option<(f64, f64)>,
    sub_speed: f64,
    sub_delay: f64,
    manual_hud_range: Option<(f64, f64)>,
    prefer_manual_hud: bool,
) -> ResolvedAnkiMediaTimings {
    let media_sub = raw_subtitle_range
        .and_then(|(s, e)| convert_subtitle_range_to_media(s, e, sub_speed, sub_delay));
    let media_selected = raw_selected_range
        .and_then(|(s, e)| convert_subtitle_range_to_media(s, e, sub_speed, sub_delay));

    if let Some((m_start, m_end)) = manual_hud_range {
        if prefer_manual_hud || media_sub.is_none() {
            return ResolvedAnkiMediaTimings {
                subtitle_start: m_start,
                subtitle_end: m_end,
                context_start: m_start,
                context_end: m_end,
            };
        }
    }

    let (subtitle_start, subtitle_end) = media_sub.unwrap_or((0.0, 0.0));
    let (context_start, context_end) = media_selected
        .or(manual_hud_range)
        .unwrap_or((subtitle_start, subtitle_end));
    ResolvedAnkiMediaTimings {
        subtitle_start,
        subtitle_end,
        context_start,
        context_end,
    }
}

pub(crate) fn set_popup_term_metadata(
    app: &MementoApp,
    expression: &str,
    reading: &str,
    graphs: Vec<TermPitchGraph>,
    frequencies: Vec<TermFrequency>,
) {
    let rows = app.get_dict_results();
    if let Some(index) = (0..rows.row_count()).find(|&index| {
        rows.row_data(index).is_some_and(|row| {
            row.term_start
                && row.expression.as_str() == expression
                && row.reading.as_str() == reading
        })
    }) {
        if let Some(mut row) = rows.row_data(index) {
            row.pitch_graphs = Rc::new(VecModel::from(popup_pitch_graphs(graphs))).into();
            row.frequencies = Rc::new(VecModel::from(
                frequencies
                    .into_iter()
                    .map(|entry| PopupFrequency {
                        dictionary: entry.dictionary.into(),
                        value: entry.value.into(),
                    })
                    .collect::<Vec<_>>(),
            ))
            .into();
            rows.set_row_data(index, row.clone());
            let popup = app.get_popup_results();
            if let Some(popup_index) = (0..popup.row_count()).find(|&popup_index| {
                popup
                    .row_data(popup_index)
                    .is_some_and(|visible| visible.index == row.index)
            }) {
                popup.set_row_data(popup_index, row);
            }
            let pb = app.global::<PopupBridge>();
            pb.set_rich_revision(pb.get_rich_revision().wrapping_add(1));
        }
    }
}

fn push_popup_audio_models(
    app: &MementoApp,
    entries: &[AudioSourceEntry],
    statuses: &[i32],
) {
    let sources: Vec<PopupAudioSource> = entries
        .iter()
        .enumerate()
        .map(|(index, source)| PopupAudioSource {
            name: source.name.clone().into(),
            index: index as i32,
        })
        .collect();
    let aligned_statuses: Vec<i32> = (0..entries.len())
        .map(|idx| statuses.get(idx).copied().unwrap_or(AUDIO_STATUS_DEFAULT))
        .collect();
    app.set_popup_audio_sources(Rc::new(VecModel::from(sources)).into());
    app.set_popup_audio_source_statuses(Rc::new(VecModel::from(aligned_statuses)).into());
}

pub(crate) fn set_popup_audio_sources_with_statuses(
    app: &MementoApp,
    cache: &Arc<Mutex<PopupAudioState>>,
    expression: &str,
    reading: &str,
    entries: Vec<AudioSourceEntry>,
    statuses: Vec<i32>,
    persist_per_term: bool,
) {
    let aligned_statuses: Vec<i32> = (0..entries.len())
        .map(|idx| statuses.get(idx).copied().unwrap_or(AUDIO_STATUS_DEFAULT))
        .collect();
    push_popup_audio_models(app, &entries, &aligned_statuses);
    {
        let mut state = cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.serial = state.serial.wrapping_add(1);
        state.expression = expression.to_owned();
        state.reading = reading.to_owned();
        state.sources = entries.clone();
        state.statuses = aligned_statuses.clone();
        if persist_per_term && !expression.is_empty() {
            state.store_term_cache(expression, reading, entries, aligned_statuses);
        }
    }
    app.set_popup_selected_audio_source(-1);
    app.set_popup_audio_sources_loading(false);
}

pub(crate) fn set_popup_audio_sources(
    app: &MementoApp,
    cache: &Arc<Mutex<PopupAudioState>>,
    expression: &str,
    reading: &str,
    entries: Vec<AudioSourceEntry>,
) {
    let preserved_statuses = {
        let state = cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !expression.is_empty() {
            state
                .per_term
                .get(&(expression.to_owned(), reading.to_owned()))
                .filter(|cached| cached.sources.len() == entries.len())
                .map(|cached| cached.statuses.clone())
        } else {
            None
        }
    }
    .unwrap_or_else(|| vec![AUDIO_STATUS_DEFAULT; entries.len()]);
    set_popup_audio_sources_with_statuses(
        app,
        cache,
        expression,
        reading,
        entries,
        preserved_statuses,
        false,
    );
}

fn update_popup_audio_source_status(
    app: &MementoApp,
    cache: &Arc<Mutex<PopupAudioState>>,
    expression: &str,
    reading: &str,
    source_indices: &[usize],
    status: i32,
) {
    let updated_statuses = {
        let mut state = cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let key = (expression.to_owned(), reading.to_owned());
        if let Some(cached) = state.per_term.get_mut(&key) {
            if cached.statuses.len() < cached.sources.len() {
                cached
                    .statuses
                    .resize(cached.sources.len(), AUDIO_STATUS_DEFAULT);
            }
            for &idx in source_indices {
                if let Some(slot) = cached.statuses.get_mut(idx) {
                    *slot = status;
                }
            }
        }
        if state.expression == expression && state.reading == reading {
            if state.statuses.len() < state.sources.len() {
                let len = state.sources.len();
                state.statuses.resize(len, AUDIO_STATUS_DEFAULT);
            }
            for &idx in source_indices {
                if let Some(slot) = state.statuses.get_mut(idx) {
                    *slot = status;
                }
            }
            Some(state.statuses.clone())
        } else {
            None
        }
    };
    if let Some(statuses) = updated_statuses {
        app.set_popup_audio_source_statuses(Rc::new(VecModel::from(statuses)).into());
    }
}

fn probe_word_audio_sources_with_statuses(
    expression: &str,
    reading: &str,
    configured_sources: &[AudioSourceEntry],
) -> (Vec<AudioSourceEntry>, Vec<i32>) {
    let mut resolved = Vec::new();
    let mut statuses = Vec::new();
    for source in configured_sources {
        let expanded = audio::enumerate_word_audio_sources(
            expression,
            reading,
            std::slice::from_ref(source),
        );
        if expanded.is_empty() {
            resolved.push(source.clone());
            statuses.push(AUDIO_STATUS_FAILED);
            continue;
        }
        let is_json = source.kind == AudioSourceType::AudioSourceTypeJson as i64;
        for entry in expanded {
            let status = if is_json {
                AUDIO_STATUS_AVAILABLE
            } else if let Some(local_path) = entry
                .url
                .strip_prefix("file://")
                .map(Path::new)
                .or_else(|| {
                    (!entry.url.contains("://") && !entry.url.is_empty())
                        .then(|| Path::new(entry.url.as_str()))
                })
            {
                if std::fs::metadata(local_path).is_ok_and(|m| m.is_file() && m.len() > 0) {
                    AUDIO_STATUS_AVAILABLE
                } else {
                    AUDIO_STATUS_FAILED
                }
            } else {
                AUDIO_STATUS_DEFAULT
            };
            resolved.push(entry);
            statuses.push(status);
        }
    }
    (resolved, statuses)
}

fn maybe_auto_play_first_term_audio(app: &MementoApp, expression: &str, reading: &str) {
    if app.get_auto_play_audio_enabled() && !expression.is_empty() {
        app.invoke_play_audio_for_word(expression.into(), reading.into());
    }
}

pub(crate) fn install_auto_play_first_term_audio(app: &MementoApp) {
    let auto_audio_app = app.as_weak();
    app.global::<PopupBridge>()
        .on_first_term_ready(move |expression, reading| {
            if let Some(app) = auto_audio_app.upgrade() {
                maybe_auto_play_first_term_audio(&app, expression.as_str(), reading.as_str());
            }
        });
}

pub(crate) struct PreparedAnkiNoteRequest {
    pub(crate) profile: AnkiNoteConfig,
    pub(crate) context: anki::NoteFieldsContext,
    pub(crate) selected_occurrences: Vec<(String, usize)>,
    pub(crate) selected_hit_meta: Vec<ActiveRowHitMeta>,
    pub(crate) screenshot_request: Option<Result<memento_player::mpv::ScreenshotRequest, String>>,
    pub(crate) video_screenshot_request:
        Option<Result<memento_player::mpv::ScreenshotRequest, String>>,
    pub(crate) media_input: anki_media::NoteMediaInput,
    pub(crate) unavailable: Vec<String>,
    pub(crate) show_tag: bool,
    pub(crate) is_panel_add: bool,
}

fn dict_order_sort_key(dictionary_order: &HashMap<i64, i64>, dic_id: i64) -> (i64, i64) {
    (
        dictionary_order.get(&dic_id).copied().unwrap_or(i64::MAX),
        dic_id,
    )
}

/// Resolves exact SQLite `(dic_id, rowid)` tuples for the checked glossary rows so
/// `populate_note_context_from_sqlite_ordered` exports `{glossary}`, `{frequencies}`, and
/// `{pitch*}` in the user's exact dictionary priority order and checked selection (C3.5).
pub(crate) fn resolve_selected_rowids_from_sqlite(
    db_path: &Path,
    ctx: &anki::NoteFieldsContext,
    selected_occurrences: &[(String, usize)],
    selected_hit_meta: &[ActiveRowHitMeta],
) -> Vec<(i64, i64)> {
    if !ctx.selected_rowids.is_empty() {
        return ctx.selected_rowids.clone();
    }
    if !db_path.is_file() || (selected_occurrences.is_empty() && selected_hit_meta.is_empty()) {
        return Vec::new();
    }
    let Ok(db) = shared_readonly_db(db_path) else {
        return Vec::new();
    };
    let Ok(meta) = db.metadata_snapshot() else {
        return Vec::new();
    };
    let titles = &meta.names;
    let is_kanji = ctx.is_kanji || !ctx.character.is_empty();

    if is_kanji {
        let target_char = if !ctx.character.is_empty() {
            ctx.character.as_str()
        } else {
            ctx.expression.as_str()
        };
        let Ok(mut rows) = db.with_conn(|conn| {
            const SQL: &str = "SELECT rowid, dic_id FROM kanji_bank \
                WHERE dic_id NOT IN (SELECT dic_id FROM dict_disabled) AND char = ?1 \
                ORDER BY dic_id ASC, rowid ASC;";
            let mut stmt = conn.prepare_cached(SQL).map_err(|e| e.to_string())?;
            let mut q = stmt.query([target_char]).map_err(|e| e.to_string())?;
            let mut out = Vec::new();
            while let Some(row) = q.next().map_err(|e| e.to_string())? {
                let rowid: i64 = row.get(0).map_err(|e| e.to_string())?;
                let dic_id: i64 = row.get(1).map_err(|e| e.to_string())?;
                out.push((rowid, dic_id));
            }
            Ok::<_, String>(out)
        }) else {
            return Vec::new();
        };
        rows.sort_by_key(|&(rowid, dic_id)| (dict_order_sort_key(&ctx.dictionary_order, dic_id), rowid));

        let mut used = vec![false; rows.len()];
        let mut out = Vec::new();
        if !selected_hit_meta.is_empty() && selected_hit_meta.iter().all(|m| m.dictionary_id > 0) {
            for want in selected_hit_meta {
                if let Some((idx, &(rowid, dic_id))) = rows
                    .iter()
                    .enumerate()
                    .find(|&(i, &(_, did))| !used[i] && did == want.dictionary_id)
                {
                    used[idx] = true;
                    out.push((dic_id, rowid));
                }
            }
            if out.len() == selected_hit_meta.len() {
                return out;
            }
            out.clear();
        }
        let mut occ_counts: HashMap<String, usize> = HashMap::new();
        let mut indexed: Vec<(String, usize, i64, i64)> = Vec::new();
        for (rowid, dic_id) in rows {
            if meta.disabled.contains(&dic_id) {
                continue;
            }
            let Some(dict_name) = titles.get(&dic_id).cloned() else {
                continue;
            };
            let occ = *occ_counts
                .entry(dict_name.clone())
                .and_modify(|c| *c += 1)
                .or_insert(0);
            indexed.push((dict_name, occ, dic_id, rowid));
        }
        for (want_dict, want_occ) in selected_occurrences {
            if let Some(&(_, _, dic_id, rowid)) = indexed
                .iter()
                .find(|(name, occ, _, _)| name == want_dict && occ == want_occ)
            {
                out.push((dic_id, rowid));
            }
        }
        return out;
    }

    let Ok(mut rows) = db.with_conn(|conn| {
        const SQL: &str = "SELECT rowid, dic_id, glossary, score, sequence \
            FROM term_bank \
            WHERE expression = ?1 \
              AND (reading = ?2 OR (?2 = ?1 AND reading = '')) \
              AND dic_id NOT IN (SELECT dic_id FROM dict_disabled) \
            ORDER BY dic_id ASC, score DESC, sequence ASC, rowid ASC;";
        let mut stmt = conn.prepare_cached(SQL).map_err(|e| e.to_string())?;
        let mut q = stmt
            .query([ctx.expression.as_str(), ctx.reading.as_str()])
            .map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        while let Some(row) = q.next().map_err(|e| e.to_string())? {
            let rowid: i64 = row.get(0).map_err(|e| e.to_string())?;
            let dic_id: i64 = row.get(1).map_err(|e| e.to_string())?;
            let glossary: String = match row.get_ref(2) {
                Ok(memento_dict::sqlite_db::rusqlite::types::ValueRef::Text(bytes)) => {
                    String::from_utf8_lossy(bytes).into_owned()
                }
                Ok(memento_dict::sqlite_db::rusqlite::types::ValueRef::Blob(bytes)) => {
                    String::from_utf8_lossy(bytes).into_owned()
                }
                _ => String::new(),
            };
            let score: i64 = row.get(3).map_err(|e| e.to_string())?;
            let sequence: i64 = row.get(4).map_err(|e| e.to_string())?;
            out.push((rowid, dic_id, glossary, score, sequence));
        }
        Ok::<_, String>(out)
    }) else {
        return Vec::new();
    };
    rows.sort_by_key(|&(rowid, dic_id, _, score, sequence)| {
        (
            dict_order_sort_key(&ctx.dictionary_order, dic_id),
            Reverse(score),
            sequence,
            rowid,
        )
    });

    let mut out = Vec::new();
    if !selected_hit_meta.is_empty() && selected_hit_meta.iter().all(|m| m.dictionary_id > 0) {
        let mut used = vec![false; rows.len()];
        for want in selected_hit_meta {
            let matched = rows
                .iter()
                .enumerate()
                .find(|&(i, (_, did, gloss, _, _))| {
                    !used[i]
                        && *did == want.dictionary_id
                        && !want.raw_glossary.is_empty()
                        && *gloss == want.raw_glossary
                })
                .or_else(|| {
                    rows.iter()
                        .enumerate()
                        .find(|&(i, (_, did, _, _, _))| !used[i] && *did == want.dictionary_id)
                });
            if let Some((idx, &(rowid, dic_id, _, _, _))) = matched {
                used[idx] = true;
                out.push((dic_id, rowid));
            }
        }
        if out.len() == selected_hit_meta.len() {
            return out;
        }
        out.clear();
    }

    let mut occ_counts: HashMap<String, usize> = HashMap::new();
    let mut indexed: Vec<(String, usize, i64, i64)> = Vec::new();
    for (rowid, dic_id, glossary, _, _) in rows {
        if meta.disabled.contains(&dic_id) {
            continue;
        }
        let Some(dict_name) = titles.get(&dic_id).cloned() else {
            continue;
        };
        let has_non_empty_items = match serde_json::from_str::<serde_json::Value>(&glossary) {
            Ok(serde_json::Value::Array(arr)) => arr.iter().any(|item| !item.is_array()),
            Ok(_) | Err(_) => true,
        };
        if !has_non_empty_items {
            continue;
        }
        let occ = *occ_counts
            .entry(dict_name.clone())
            .and_modify(|c| *c += 1)
            .or_insert(0);
        indexed.push((dict_name, occ, dic_id, rowid));
    }
    for (want_dict, want_occ) in selected_occurrences {
        if let Some(&(_, _, dic_id, rowid)) = indexed
            .iter()
            .find(|(name, occ, _, _)| name == want_dict && occ == want_occ)
        {
            out.push((dic_id, rowid));
        }
    }
    out
}

pub(crate) fn prepare_anki_note_request(
    app: &MementoApp,
    definitions: &slint::ModelRc<DictResult>,
    row_idx: usize,
    config: &ConfigController,
    audio_cache: &Arc<Mutex<PopupAudioState>>,
    subtitles: &RefCell<SubtitleController>,
    mpv: &RefCell<Option<MpvInstance>>,
) -> Result<PreparedAnkiNoteRequest, Option<String>> {
    let Some(hit) = definitions.row_data(row_idx) else {
        return Err(None);
    };
    let profile = if hit.is_kanji {
        config.anki_kanji_note_config()
    } else {
        config.anki_profile_note_config()
    };
    let Some(profile) = profile else {
        return Err(Some(
            "Select an Anki profile and configure this note type".to_owned(),
        ));
    };
    if !profile.enabled {
        return Err(Some("Enable Anki integration in Preferences".to_owned()));
    }

    let add_override = current_anki_add_override();
    let pb = app.global::<PopupBridge>();
    let bridge_id = pb.get_bridge_instance_id();
    let (target_depth, is_panel_add, audio_source_override) = match add_override {
        Some(ov) => match ov.target {
            AnkiAddTarget::Root => (0, false, ov.audio_source_override),
            AnkiAddTarget::Nested(depth) => (depth.max(1), false, ov.audio_source_override),
            AnkiAddTarget::Panel => (-1, true, ov.audio_source_override),
        },
        None => {
            let nested_depth = pb.get_nested_depth();
            if nested_depth > 0 {
                (nested_depth, false, None)
            } else {
                (0, false, None)
            }
        }
    };

    let (remove_regex, replace_lines, replace_with) = (
        app.get_search_remove_regex().to_string(),
        app.get_search_replace_lines(),
        app.get_search_replace_with().to_string(),
    );
    let ass_lookup = if is_panel_add {
        None
    } else {
        ui::active_ass_lookup_for_app(app)
    };
    let base_subtitle = if let Some(ref ass_hit) = ass_lookup {
        filter_subtitle_text(
            &ass_hit.event_plain_text,
            &remove_regex,
            replace_lines,
            &replace_with,
        )
    } else {
        app.get_active_sub_text().to_string()
    };
    let mut context = anki::NoteFieldsContext::new(
        hit.expression.to_string(),
        hit.reading.to_string(),
        base_subtitle,
    );
    context.is_kanji = hit.is_kanji;
    context.dictionary_order = dictionary_order_map(config);
    context.reading_as_expression = if hit.is_kanji {
        false
    } else if target_depth < 0 {
        pb.get_panel_row_reading_as_expression()
            .row_data(row_idx)
            .unwrap_or(false)
    } else if target_depth > 0 {
        pb.get_nested_row_reading_as_expression()
            .row_data(row_idx)
            .unwrap_or(false)
    } else {
        pb.get_row_reading_as_expression()
            .row_data(row_idx)
            .unwrap_or(false)
    };

    // Multi-definition persistent `{selection}` (#262 / H5.1): prefer ordered non-empty
    // per-definition selections from `RichUiBridge`, falling back to `selected-glossary-text`.
    let multi_selections: Vec<String> = get_active_glossary_selections(bridge_id, target_depth)
        .into_iter()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .collect();
    if !multi_selections.is_empty() {
        context.selection = multi_selections.join("\n");
        context.selections = multi_selections;
    } else {
        let selected_glossary = pb.get_selected_glossary_text();
        if !selected_glossary.trim().is_empty() {
            context.selection = selected_glossary.to_string();
        }
    }

    // Never assign the scanned subtitle token into `context.selection` (#262);
    // use it purely to locate `cloze_prefix` / `cloze_body` / `cloze_suffix`.
    let (scanned_surface, token_offset_in_sentence) = if let Some(ref ass_hit) = ass_lookup {
        let off = if remove_regex.is_empty() && !replace_lines {
            Some(ass_hit.byte_offset_in_event)
        } else {
            None
        };
        (ass_hit.scanned_surface.clone(), off)
    } else if !is_panel_add {
        let start = app.get_selected_token_index();
        if start >= 0 {
            let words = app.get_active_sub_words();
            let start_idx = start as usize;
            let count = app.get_selected_token_count().max(1) as usize;
            let byte_offset: usize = (0..start_idx)
                .filter_map(|i| words.row_data(i))
                .map(|word| word.text.len())
                .sum();
            let surface: String = (start_idx..start_idx.saturating_add(count))
                .filter_map(|i| words.row_data(i))
                .map(|word| word.text.to_string())
                .collect();
            (surface, Some(byte_offset))
        } else {
            (String::new(), None)
        }
    } else {
        (String::new(), None)
    };

    let (raw_active_range, raw_selected_range, selected_cues, active_pos_in_selected) = {
        let subs = subtitles.borrow();
        let selected_indices = subs.selected_indices();
        let active_indices = subs.active_indices(subs.primary_sid());
        let active_pos = selected_indices
            .iter()
            .position(|idx| active_indices.contains(idx));
        let cues = subs
            .selected_cues()
            .iter()
            .map(|cue| {
                let raw = String::from_utf16_lossy(&cue.text);
                filter_subtitle_text(&raw, &remove_regex, replace_lines, &replace_with)
            })
            .collect::<Vec<_>>();
        let active = subs.active_cue();
        let raw_sub = if let Some(ref ass_hit) = ass_lookup {
            if ass_hit.event_end_s > ass_hit.event_start_s {
                Some((ass_hit.event_start_s, ass_hit.event_end_s))
            } else {
                active
                    .filter(|cue| cue.end > cue.start)
                    .map(|cue| (cue.start, cue.end))
            }
        } else {
            active
                .filter(|cue| cue.end > cue.start)
                .map(|cue| (cue.start, cue.end))
        };
        let raw_sel = subs
            .selected_time_range()
            .filter(|&(start, end)| end > start);
        (raw_sub, raw_sel, cues, active_pos)
    };

    anki::apply_subtitle_cloze(
        &mut context,
        &selected_cues,
        active_pos_in_selected,
        &scanned_surface,
        token_offset_in_sentence,
    );
    context.secondary_subtitle = app.get_secondary_sub_text().to_string();
    context.title = app.get_media_title().to_string();
    context.dictionary_name = hit.dict_name.to_string();

    let all_hit_meta = get_active_row_hit_meta(bridge_id, target_depth);
    let mut occ_by_dict: HashMap<String, usize> = HashMap::new();
    let mut selected = Vec::new();
    let mut selected_occurrences = Vec::new();
    let mut selected_hit_meta = Vec::new();
    for r in 0..definitions.row_count() {
        let Some(row) = definitions.row_data(r) else {
            continue;
        };
        if row.expression == hit.expression
            && row.reading == hit.reading
            && row.is_kanji == hit.is_kanji
        {
            let dict_name = row.dict_name.to_string();
            let occ = *occ_by_dict
                .entry(dict_name.clone())
                .and_modify(|c| *c += 1)
                .or_insert(0);
            if row.selected {
                selected_occurrences.push((dict_name.clone(), occ));
                if let Some(meta_item) = all_hit_meta
                    .get(r)
                    .filter(|m| m.dict_name == dict_name)
                    .cloned()
                {
                    selected_hit_meta.push(meta_item);
                }
                selected.push(row);
            }
        }
    }
    if selected.is_empty() {
        return Err(Some(
            "Select at least one dictionary entry for this term".to_owned(),
        ));
    }
    if selected_hit_meta.len() != selected.len() {
        selected_hit_meta.clear();
    }

    context.glossary_definitions = selected
        .iter()
        .enumerate()
        .map(|(idx, row)| anki::GlossaryDefinition {
            dictionary_id: selected_hit_meta
                .get(idx)
                .map(|m| m.dictionary_id)
                .unwrap_or(0),
            dictionary_name: row.dict_name.to_string(),
            glossary: row.glossary.to_string(),
            tags: row
                .pos_tags
                .split(|character: char| character == ',' || character.is_whitespace())
                .filter(|tag| !tag.is_empty())
                .map(str::to_owned)
                .collect(),
            notes: row.notes.to_string(),
            example_ja: row.example_ja.to_string(),
            example_en: row.example_en.to_string(),
            ..Default::default()
        })
        .collect();
    context.glossary_items = selected
        .iter()
        .map(|row| row.glossary.to_string())
        .collect();
    context.tags = selected
        .iter()
        .flat_map(|row| {
            row.pos_tags
                .split(|character: char| character == ',' || character.is_whitespace())
        })
        .filter(|tag| !tag.is_empty())
        .map(str::to_owned)
        .collect();
    context.tags.sort();
    context.tags.dedup();
    if hit.is_kanji {
        context.character = hit.expression.to_string();
        for component in hit.reading.split(';') {
            let component = component.trim();
            if let Some(on) = component.strip_prefix("On:") {
                context.onyomi = on.trim().to_owned();
            }
            if let Some(kun) = component.strip_prefix("Kun:") {
                context.kunyomi = kun.trim().to_owned();
            }
        }
        context.stroke_count = hit
            .notes
            .split(';')
            .filter_map(|part| {
                let (label, value) = part.split_once(':')?;
                (label.to_ascii_lowercase().contains("stroke") || label.contains("画数"))
                    .then(|| value.trim().to_owned())
            })
            .next()
            .unwrap_or_default();
    }

    let (wants_screenshot, wants_video_screenshot) = anki_media::requested_screenshots(&profile);
    let mut screenshot_request = None;
    let mut video_screenshot_request = None;
    let prefer_manual_hud = is_panel_add
        || (ass_lookup.is_none() && app.get_selected_token_index() < 0);
    let hud_text = app.get_audio_context_hud_text();
    let (media_input, sec_ctx_start, sec_ctx_end) = {
        let player_ref = mpv.borrow();
        let player = player_ref.as_ref();
        // C3.7 (#155): Pass full `EncodeContext` (streaming URL, script-opts, headers, cookies,
        // referrer, user-agent, ytdl-raw-options, external DASH audio track) into `MediaContext`.
        let media_ctx = player
            .and_then(MpvInstance::capture_encode_context)
            .map(media::MediaContext::from);
        let sub_speed = player.map(MpvInstance::get_sub_speed).unwrap_or(1.0);
        let sub_delay = player.map(MpvInstance::get_sub_delay).unwrap_or(0.0);
        let sec_sub_delay = player
            .map(MpvInstance::get_secondary_sub_delay)
            .unwrap_or(0.0);
        let mpv_start = player.map(MpvInstance::get_sub_start).unwrap_or(0.0);
        let mpv_end = player.map(MpvInstance::get_sub_end).unwrap_or(0.0);
        let current_pos = player
            .map(MpvInstance::get_position)
            .unwrap_or_else(|| f64::from(app.get_position()));
        let manual_hud_range = parse_manual_audio_context_hud(hud_text.as_str(), current_pos);

        let raw_sub_range = raw_active_range.or_else(|| (mpv_end > mpv_start).then_some((mpv_start, mpv_end)));
        let timings = resolve_anki_media_timings(
            raw_sub_range,
            raw_selected_range,
            sub_speed,
            sub_delay,
            manual_hud_range,
            prefer_manual_hud,
        );

        for (wanted, with_subs, request) in [
            (wants_screenshot, true, &mut screenshot_request),
            (wants_video_screenshot, false, &mut video_screenshot_request),
        ] {
            if wanted {
                *request = Some(
                    player
                        .ok_or_else(|| "No video is loaded".to_owned())
                        .and_then(|player| player.request_screenshot(with_subs)),
                );
            }
        }
        let sec_start = if timings.context_end > timings.context_start && player.is_some() {
            subtitle_raw_time(timings.context_start, sub_speed, sec_sub_delay)
        } else {
            timings.context_start
        };
        let sec_end = if timings.context_end > timings.context_start && player.is_some() {
            subtitle_raw_time(timings.context_end, sub_speed, sec_sub_delay)
        } else {
            timings.context_end
        };
        let audio_sources = if audio_source_override == Some(-2) {
            // Explicit "Add without audio" right-click menu option.
            Vec::new()
        } else {
            let state = audio_cache
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let matching = state.expression == hit.expression.as_str()
                && state.reading == hit.reading.as_str();
            let mut sources = if matching && !state.sources.is_empty() {
                state.sources.clone()
            } else if let Some(cached) = state
                .per_term
                .get(&(hit.expression.to_string(), hit.reading.to_string()))
            {
                cached.sources.clone()
            } else {
                Vec::new()
            };
            drop(state);
            if sources.is_empty() {
                sources = config.audio_source_entries();
            }
            let selected_src = match audio_source_override {
                Some(idx) if idx >= 0 => idx,
                _ if matching => app.get_popup_selected_audio_source(),
                _ => -1,
            };
            if selected_src >= 0 {
                sources = sources
                    .into_iter()
                    .nth(selected_src as usize)
                    .into_iter()
                    .collect();
            }
            sources
        };
        (
            anki_media::NoteMediaInput {
                media: media_ctx,
                subtitle_start: timings.subtitle_start,
                subtitle_end: timings.subtitle_end,
                context_start: timings.context_start,
                context_end: timings.context_end,
                screenshot: None,
                screenshot_video: None,
                subtitle_id: player.map(MpvInstance::primary_sid).unwrap_or(0),
                audio_sources,
            },
            sec_start,
            sec_end,
        )
    };
    context.secondary_context = anki::secondary_context_for_range(
        subtitles.borrow().secondary_entries(),
        sec_ctx_start,
        sec_ctx_end,
        &context.secondary_subtitle,
    );
    let unavailable = anki::unavailable_profile_markers(&profile);
    let show_tag = config.anki_show_tag();
    Ok(PreparedAnkiNoteRequest {
        profile,
        context,
        selected_occurrences,
        selected_hit_meta,
        screenshot_request,
        video_screenshot_request,
        media_input,
        unavailable,
        show_tag,
        is_panel_add,
    })
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn install_update_anki_note_handler(
    app: &MementoApp,
    config: Rc<ConfigController>,
    audio_cache: Arc<Mutex<PopupAudioState>>,
    subtitles: Rc<RefCell<SubtitleController>>,
    mpv: Rc<RefCell<Option<MpvInstance>>>,
    database_path: PathBuf,
    resources_path: PathBuf,
    config_dir: PathBuf,
    lookup_epoch: Arc<AtomicU64>,
) {
    let update_app = app.as_weak();
    app.global::<PopupBridge>().on_update_anki_note_requested(
        move |row_idx, raw_note_id, _expression, _reading, _glossary_summary| {
            let Some(app) = update_app.upgrade() else {
                return;
            };
            if row_idx < 0 {
                return;
            }
            let row_usize = row_idx as usize;
            let Some(note_id) = ui::popup_row_anki_note_id_i64(&app, row_usize)
                .or_else(|| (raw_note_id > 0).then_some(i64::from(raw_note_id)))
            else {
                app.set_status_message("Could not resolve Anki note ID to update".into());
                return;
            };
            let pb = app.global::<PopupBridge>();
            let nested_depth = pb.get_nested_depth();
            let definitions = if nested_depth > 0 {
                pb.get_nested_results()
            } else {
                app.get_dict_results()
            };
            let prepared = match prepare_anki_note_request(
                &app,
                &definitions,
                row_usize,
                &config,
                &audio_cache,
                &subtitles,
                &mpv,
            ) {
                Ok(prepared) => prepared,
                Err(Some(msg)) => {
                    app.set_status_message(msg.into());
                    return;
                }
                Err(None) => return,
            };
            let PreparedAnkiNoteRequest {
                profile,
                mut context,
                selected_occurrences,
                selected_hit_meta,
                screenshot_request,
                video_screenshot_request,
                mut media_input,
                unavailable,
                show_tag,
                is_panel_add,
            } = prepared;
            app.set_status_message(format!("Updating Anki note #{note_id}…").into());
            let response_app = update_app.clone();
            let response_epoch = lookup_epoch.clone();
            let request_epoch = response_epoch.load(Ordering::Acquire);
            let metadata_path = database_path.clone();
            let res_dir = resources_path.clone();
            let cfg_dir = config_dir.clone();
            std::thread::spawn(move || {
                media_input.screenshot = screenshot_request
                    .and_then(|request| request.ok())
                    .and_then(|request| request.wait().ok());
                media_input.screenshot_video = video_screenshot_request
                    .and_then(|request| request.ok())
                    .and_then(|request| request.wait().ok());
                let controller = AnkiController::new(profile.endpoint());
                let resource_roots = vec![cfg_dir.join("dict_resources"), res_dir];
                let selected_rowids = resolve_selected_rowids_from_sqlite(
                    &metadata_path,
                    &context,
                    &selected_occurrences,
                    &selected_hit_meta,
                );
                context.selected_rowids = selected_rowids.clone();
                let dict_order = context.dictionary_order.clone();
                let _ = anki::populate_note_context_from_sqlite_ordered(
                    &mut context,
                    &metadata_path,
                    &resource_roots,
                    &selected_occurrences,
                    &selected_rowids,
                    &dict_order,
                );
                let result = controller.update_existing_note_with_media_outcome(
                    note_id,
                    &profile,
                    &mut context,
                    media_input,
                    anki::AnkiUpdateMode::OverwriteNonEmpty,
                );
                let updated_profile = profile.clone();
                let _ = response_app.upgrade_in_event_loop(move |app| {
                    let ui_still_relevant = if is_panel_add {
                        app.get_dict_search_open()
                    } else {
                        app.get_dict_open()
                            && response_epoch.load(Ordering::Acquire) == request_epoch
                    };
                    if ui_still_relevant {
                        if let Ok(outcome) = &result {
                            ui::mark_popup_term_anki_updated(
                                &app,
                                row_usize,
                                nested_depth,
                                outcome.note_id,
                                &updated_profile,
                                show_tag,
                            );
                        }
                    }
                    if let Ok(outcome) = &result {
                        if !outcome.warnings.is_empty() {
                            show_warning_toast(&app, &outcome.warnings);
                        }
                    }
                    app.set_status_message(match result {
                        Ok(outcome) if !outcome.warnings.is_empty() => format!(
                            "Updated Anki note #{} (with media warnings)",
                            outcome.note_id
                        )
                        .into(),
                        Ok(outcome) if unavailable.is_empty() => {
                            format!("Updated Anki note #{}", outcome.note_id).into()
                        }
                        Ok(outcome) => {
                            eprintln!(
                                "Anki note #{}: unavailable profile markers left empty: {}",
                                outcome.note_id,
                                unavailable.join(", ")
                            );
                            format!(
                                "Updated Anki note #{}; some fields are empty",
                                outcome.note_id
                            )
                            .into()
                        }
                        Err(error) => {
                            eprintln!("Could not update Anki note #{note_id}: {error}");
                            format!("Could not update Anki note #{note_id}: {error}").into()
                        }
                    });
                });
            });
        },
    );
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn install_anki_and_audio_handlers(
    app: &MementoApp,
    config_ctrl: Rc<ConfigController>,
    popup_audio_cache: Arc<Mutex<PopupAudioState>>,
    sub_ctrl: Rc<RefCell<SubtitleController>>,
    mpv: Rc<RefCell<Option<MpvInstance>>>,
    dictionary_database_path: PathBuf,
    dictionary_resources_path: PathBuf,
    config_dir: PathBuf,
    lookup_epoch: Arc<AtomicU64>,
) {
    let audio_player: Rc<RefCell<Option<audio::AudioController>>> = Rc::new(RefCell::new(None));
    audio::set_download_timeout_ms(config_ctrl.audio_download_timeout_ms());
    register_audio_invalidation_target(
        app.as_weak(),
        popup_audio_cache.clone(),
        audio_player.clone(),
    );

    let source_menu_app = app.as_weak();
    let source_menu_config = config_ctrl.clone();
    let source_menu_cache = popup_audio_cache.clone();
    app.on_popup_audio_sources_requested(move |expression, reading| {
        let Some(app) = source_menu_app.upgrade() else {
            return;
        };
        let expression = expression.to_string();
        let reading = reading.to_string();
        let (already_resolved, cached_term) = {
            let state = source_menu_cache
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let same_term = state.expression == expression && state.reading == reading;
            let resolved = same_term
                && (!state.sources.is_empty() || app.get_popup_audio_sources_loading());
            let cached = if !resolved && !expression.is_empty() {
                state
                    .per_term
                    .get(&(expression.clone(), reading.clone()))
                    .cloned()
            } else {
                None
            };
            (resolved, cached)
        };
        if already_resolved {
            return;
        }
        if let Some(cached) = cached_term {
            set_popup_audio_sources_with_statuses(
                &app,
                &source_menu_cache,
                &expression,
                &reading,
                cached.sources,
                cached.statuses,
                false,
            );
            return;
        }
        let sources = source_menu_config.audio_source_entries();
        let checking_statuses = vec![AUDIO_STATUS_CHECKING; sources.len()];
        set_popup_audio_sources_with_statuses(
            &app,
            &source_menu_cache,
            &expression,
            &reading,
            sources.clone(),
            checking_statuses,
            false,
        );
        app.set_popup_audio_sources_loading(true);
        let serial = source_menu_cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .serial;
        let weak = source_menu_app.clone();
        let cache = source_menu_cache.clone();
        std::thread::spawn(move || {
            let (files, statuses) =
                probe_word_audio_sources_with_statuses(&expression, &reading, &sources);
            let _ = weak.upgrade_in_event_loop(move |app| {
                let still_current = {
                    let state = cache
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    state.serial == serial
                        && state.expression == expression
                        && state.reading == reading
                };
                if still_current {
                    set_popup_audio_sources_with_statuses(
                        &app,
                        &cache,
                        &expression,
                        &reading,
                        files,
                        statuses,
                        true,
                    );
                }
            });
        });
    });

    let audio_app = app.as_weak();
    let audio_config = config_ctrl.clone();
    let playback_audio_cache = popup_audio_cache.clone();
    app.on_play_audio_for_word(move |expression, reading| {
        let Some(app) = audio_app.upgrade() else {
            return;
        };
        let expr_str = expression.to_string();
        let reading_str = reading.to_string();
        let (matching, cached_for_term, mut sources) = {
            let state = playback_audio_cache
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let matching = state.expression == expr_str && state.reading == reading_str;
            if matching && !state.sources.is_empty() {
                (true, None, state.sources.clone())
            } else if let Some(cached) = state
                .per_term
                .get(&(expr_str.clone(), reading_str.clone()))
                .cloned()
            {
                (false, Some(cached), Vec::new())
            } else {
                (matching, None, Vec::new())
            }
        };
        let selected_source = if matching {
            app.get_popup_selected_audio_source()
        } else {
            -1
        };
        if let Some(cached) = cached_for_term {
            sources = cached.sources.clone();
            set_popup_audio_sources_with_statuses(
                &app,
                &playback_audio_cache,
                &expr_str,
                &reading_str,
                cached.sources,
                cached.statuses,
                false,
            );
        } else if sources.is_empty() {
            sources = audio_config.audio_source_entries();
            if !sources.is_empty() {
                let default_statuses = vec![AUDIO_STATUS_DEFAULT; sources.len()];
                set_popup_audio_sources_with_statuses(
                    &app,
                    &playback_audio_cache,
                    &expr_str,
                    &reading_str,
                    sources.clone(),
                    default_statuses,
                    false,
                );
            }
        }
        let total_sources = sources.len();
        let played_indices: Vec<usize> = if selected_source >= 0
            && (selected_source as usize) < total_sources
        {
            vec![selected_source as usize]
        } else {
            (0..total_sources).collect()
        };
        let play_sources = if selected_source >= 0 {
            sources
                .into_iter()
                .nth(selected_source as usize)
                .into_iter()
                .collect()
        } else {
            sources
        };
        if play_sources.is_empty() {
            app.set_status_message("No pronunciation audio source is configured".into());
            return;
        }
        let mut player = audio_player.borrow_mut();
        if player.is_none() {
            match audio::AudioController::new() {
                Ok(controller) => {
                    controller.set_download_timeout_ms(audio_config.audio_download_timeout_ms());
                    *player = Some(controller);
                }
                Err(error) => {
                    app.set_status_message(
                        format!("Pronunciation audio is unavailable: {error}").into(),
                    );
                    return;
                }
            }
        }
        let single_target = selected_source >= 0 || total_sources == 1;
        if single_target {
            update_popup_audio_source_status(
                &app,
                &playback_audio_cache,
                &expr_str,
                &reading_str,
                &played_indices,
                AUDIO_STATUS_CHECKING,
            );
        }
        let weak = app.as_weak();
        let status_cache = playback_audio_cache.clone();
        let cb_expr = expr_str.clone();
        let cb_reading = reading_str.clone();
        let cb_indices = played_indices.clone();
        let result = player.as_ref().unwrap().play_word(
            expr_str,
            reading_str,
            play_sources,
            move |result| {
                let _ = weak.upgrade_in_event_loop(move |app| match result {
                    Ok(()) => {
                        if single_target {
                            update_popup_audio_source_status(
                                &app,
                                &status_cache,
                                &cb_expr,
                                &cb_reading,
                                &cb_indices,
                                AUDIO_STATUS_AVAILABLE,
                            );
                        }
                    }
                    Err(error) => {
                        update_popup_audio_source_status(
                            &app,
                            &status_cache,
                            &cb_expr,
                            &cb_reading,
                            &cb_indices,
                            AUDIO_STATUS_FAILED,
                        );
                        app.set_status_message(
                            format!("Could not play pronunciation: {error}").into(),
                        );
                    }
                });
            },
        );
        if let Err(error) = result {
            update_popup_audio_source_status(
                &app,
                &playback_audio_cache,
                expression.as_str(),
                reading.as_str(),
                &played_indices,
                AUDIO_STATUS_FAILED,
            );
            app.set_status_message(format!("Could not start pronunciation: {error}").into());
        }
    });

    install_auto_play_first_term_audio(app);
    install_update_anki_note_handler(
        app,
        config_ctrl.clone(),
        popup_audio_cache.clone(),
        sub_ctrl.clone(),
        mpv.clone(),
        dictionary_database_path.clone(),
        dictionary_resources_path.clone(),
        config_dir.clone(),
        lookup_epoch.clone(),
    );

    let with_audio_app = app.as_weak();
    app.on_add_to_anki_with_audio(move |row_idx, source_idx| {
        if let Some(app) = with_audio_app.upgrade() {
            let _override_guard =
                ScopedAnkiAddOverride::new(AnkiAddTarget::Root, Some(source_idx));
            app.invoke_add_anki_note(row_idx);
        }
    });

    let note_app = app.as_weak();
    let note_config = config_ctrl.clone();
    let note_audio_cache = popup_audio_cache;
    let note_subtitles = sub_ctrl;
    let note_mpv = mpv;
    let note_database_path = dictionary_database_path;
    let note_resources_path = dictionary_resources_path;
    let note_config_dir = config_dir;
    let note_lookup_epoch = lookup_epoch;
    app.on_add_anki_note(move |index| {
        let Some(app) = note_app.upgrade() else {
            return;
        };
        if index < 0 {
            return;
        }
        app.set_anki_added_note_id("".into());
        app.set_anki_added_result_index(-1);
        let definitions = app.get_dict_results();
        let prepared = match prepare_anki_note_request(
            &app,
            &definitions,
            index as usize,
            &note_config,
            &note_audio_cache,
            &note_subtitles,
            &note_mpv,
        ) {
            Ok(prepared) => prepared,
            Err(Some(msg)) => {
                app.set_status_message(msg.into());
                return;
            }
            Err(None) => return,
        };
        let PreparedAnkiNoteRequest {
            profile,
            mut context,
            selected_occurrences,
            selected_hit_meta,
            screenshot_request,
            video_screenshot_request,
            mut media_input,
            unavailable,
            show_tag,
            is_panel_add,
        } = prepared;
        app.set_status_message("Adding note to Anki…".into());
        let response_app = note_app.clone();
        let response_epoch = note_lookup_epoch.clone();
        let request_epoch = response_epoch.load(Ordering::Acquire);
        let metadata_path = note_database_path.clone();
        let res_dir = note_resources_path.clone();
        let cfg_dir = note_config_dir.clone();
        std::thread::spawn(move || {
            media_input.screenshot = screenshot_request
                .and_then(|request| request.ok())
                .and_then(|request| request.wait().ok());
            media_input.screenshot_video = video_screenshot_request
                .and_then(|request| request.ok())
                .and_then(|request| request.wait().ok());
            let controller = AnkiController::new(profile.endpoint());
            let resource_roots = vec![cfg_dir.join("dict_resources"), res_dir];
            let selected_rowids = resolve_selected_rowids_from_sqlite(
                &metadata_path,
                &context,
                &selected_occurrences,
                &selected_hit_meta,
            );
            context.selected_rowids = selected_rowids.clone();
            let dict_order = context.dictionary_order.clone();
            let _ = anki::populate_note_context_from_sqlite_ordered(
                &mut context,
                &metadata_path,
                &resource_roots,
                &selected_occurrences,
                &selected_rowids,
                &dict_order,
            );
            let result =
                anki_media::prepare_note_media_with_warnings(&profile, &mut context, media_input)
                    .and_then(|_| controller.add_note_from_profile_with_warnings(&profile, &context));
            let added_profile = profile.clone();
            let expire_app = response_app.clone();
            let _ = response_app.upgrade_in_event_loop(move |app| {
                let ui_still_relevant = if is_panel_add {
                    app.get_dict_search_open()
                } else {
                    app.get_dict_open() && response_epoch.load(Ordering::Acquire) == request_epoch
                };
                if ui_still_relevant {
                    if let Ok(outcome) = &result {
                        ui::mark_popup_term_anki_added(
                            &app,
                            index as usize,
                            outcome.note_id,
                            &added_profile,
                            show_tag,
                        );
                        if !is_panel_add {
                            let note_id = outcome.note_id.to_string();
                            app.set_anki_added_note_id(note_id.clone().into());
                            app.set_anki_added_result_index(index);
                            slint::Timer::single_shot(
                                std::time::Duration::from_secs(15),
                                move || {
                                    if let Some(app) = expire_app.upgrade() {
                                        if app.get_anki_added_note_id() == note_id {
                                            app.set_anki_added_note_id("".into());
                                            app.set_anki_added_result_index(-1);
                                        }
                                    }
                                },
                            );
                        }
                    }
                }
                if let Ok(outcome) = &result {
                    if !outcome.warnings.is_empty() {
                        show_warning_toast(&app, &outcome.warnings);
                    }
                }
                app.set_status_message(match result {
                    Ok(outcome) if !outcome.warnings.is_empty() => format!(
                        "Added Anki note {} (with media warnings)",
                        outcome.note_id
                    )
                    .into(),
                    Ok(outcome) if unavailable.is_empty() => {
                        format!("Added Anki note {}", outcome.note_id).into()
                    }
                    Ok(outcome) => {
                        eprintln!(
                            "Anki note {}: unavailable profile markers left empty: {}",
                            outcome.note_id,
                            unavailable.join(", ")
                        );
                        format!(
                            "Added Anki note {}; some fields are empty",
                            outcome.note_id
                        )
                        .into()
                    }
                    Err(error) => {
                        eprintln!("Could not add Anki note: {error}");
                        format!("Could not add Anki note: {error}").into()
                    }
                });
            });
        });
    });

    let browse_app = app.as_weak();
    let browse_config = config_ctrl;
    app.on_browse_anki_note(move |id| {
        let Some(query) = anki::normalize_anki_browse_target(id.as_str()) else {
            return;
        };
        let endpoint = browse_config.anki_endpoint();
        let weak = browse_app.clone();
        std::thread::spawn(move || {
            let result = AnkiController::new(endpoint).client().open_browse(&query);
            if let Err(error) = result {
                let _ = weak.upgrade_in_event_loop(move |app| {
                    app.set_status_message(format!("Could not open Anki browser: {error}").into());
                });
            }
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::preferences::refresh_player_preferences;
    use crate::app::workers::{display_word_lookup, WordLookupResult};
    use crate::controller::dict::SearchHit;
    use crate::ui::{dictionary_result_rows, set_dictionary_rows};

    #[test]
    fn auto_play_audio_triggers_on_first_term_result_when_enabled() {
        i_slint_backend_testing::init_no_event_loop();
        let app = MementoApp::new().unwrap();
        install_auto_play_first_term_audio(&app);
        let cfg = ConfigController::new();
        cfg.edit_setting("search/auto-play-audio", "true").unwrap();
        refresh_player_preferences(&app, &cfg);
        assert!(app.get_auto_play_audio_enabled());

        let played = Rc::new(RefCell::new(Vec::<(String, String)>::new()));
        let played_sink = played.clone();
        app.on_play_audio_for_word(move |expr, reading| {
            played_sink
                .borrow_mut()
                .push((expr.to_string(), reading.to_string()));
        });

        let term = SearchHit::new("食べる", "たべる", "to eat", "v1", 10, false);
        let kanji = SearchHit::new("食", "しょく", "eat", "kanji", 1, true);
        display_word_lookup(
            &app,
            WordLookupResult {
                hits: vec![kanji.clone(), term.clone()],
                error: None,
                database_open: true,
                matched_units: 3,
                headword: "食べる".into(),
                reading: "たべる".into(),
                pitch: String::new(),
                freq: String::new(),
                jpdb: String::new(),
            },
            "食べる",
            &[],
        );
        assert_eq!(
            played.borrow().as_slice(),
            &[("食べる".to_owned(), "たべる".to_owned())]
        );

        played.borrow_mut().clear();
        display_word_lookup(
            &app,
            WordLookupResult {
                hits: vec![kanji],
                error: None,
                database_open: true,
                matched_units: 1,
                headword: "食".into(),
                reading: "しょく".into(),
                pitch: String::new(),
                freq: String::new(),
                jpdb: String::new(),
            },
            "食",
            &[],
        );
        assert!(played.borrow().is_empty());

        played.borrow_mut().clear();
        cfg.edit_setting("search/auto-play-audio", "false").unwrap();
        refresh_player_preferences(&app, &cfg);
        assert!(!app.get_auto_play_audio_enabled());
        display_word_lookup(
            &app,
            WordLookupResult {
                hits: vec![term],
                error: None,
                database_open: true,
                matched_units: 3,
                headword: "食べる".into(),
                reading: "たべる".into(),
                pitch: String::new(),
                freq: String::new(),
                jpdb: String::new(),
            },
            "食べる",
            &[],
        );
        assert!(played.borrow().is_empty());
    }

    #[test]
    fn update_anki_note_requested_resolves_lossless_64bit_id_and_updates_note() {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        i_slint_backend_testing::init_no_event_loop();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let target_note_id: i64 = 1_732_012_345_678;

        let server_actions = Arc::new(Mutex::new(Vec::<serde_json::Value>::new()));
        let server_actions_bg = server_actions.clone();
        let server = std::thread::spawn(move || {
            for _ in 0..3 {
                let (mut stream, _) = listener.accept().unwrap();
                let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(3)));
                let mut buf = Vec::new();
                let mut tmp = [0u8; 4096];
                let mut header_end = None;
                let mut content_len = 0usize;
                loop {
                    let n = stream.read(&mut tmp).unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&tmp[..n]);
                    if header_end.is_none() {
                        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                            let hdr = String::from_utf8_lossy(&buf[..pos]);
                            for line in hdr.lines() {
                                if let Some((k, v)) = line.split_once(':') {
                                    if k.eq_ignore_ascii_case("content-length") {
                                        content_len = v.trim().parse().unwrap_or(0);
                                    }
                                }
                            }
                            header_end = Some(pos + 4);
                        }
                    }
                    if let Some(start) = header_end {
                        if buf.len() >= start + content_len {
                            break;
                        }
                    }
                }
                let start = header_end.unwrap_or(buf.len());
                let body: serde_json::Value =
                    serde_json::from_slice(&buf[start..]).unwrap_or(serde_json::Value::Null);
                let action = body
                    .get("action")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_owned();
                server_actions_bg.lock().unwrap().push(body);
                let reply = match action.as_str() {
                    "notesInfo" => serde_json::json!({
                        "result": [{
                            "noteId": target_note_id,
                            "modelName": "JP Mining",
                            "tags": ["existing-tag"],
                            "fields": {
                                "Expression": { "value": "食べる", "order": 0 },
                                "Sentence": { "value": "", "order": 1 },
                                "Glossary": { "value": "old def", "order": 2 }
                            }
                        }],
                        "error": null
                    }),
                    "updateNoteFields" | "addTags" => serde_json::json!({
                        "result": null,
                        "error": null
                    }),
                    _ => serde_json::json!({ "result": null, "error": "unexpected action" }),
                };
                let payload = reply.to_string();
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    payload.len(),
                    payload
                );
                let _ = stream.write_all(resp.as_bytes());
                let _ = stream.flush();
            }
        });

        let dir = tempfile::TempDir::new().unwrap();
        let cfg = ConfigController::new();
        cfg.edit_setting("anki/enabled", "true").unwrap();
        cfg.edit_setting("anki/profile/host", "127.0.0.1").unwrap();
        cfg.edit_setting("anki/profile/port", &port.to_string())
            .unwrap();
        cfg.edit_setting("anki/profile/term/deck", "Mining")
            .unwrap();
        cfg.edit_setting("anki/profile/term/model", "JP Mining")
            .unwrap();
        cfg.edit_setting(
            "anki/profile/term/fields",
            &serde_json::json!([
                ["Expression", "{expression}"],
                ["Sentence", "{sentence}"],
                ["Glossary", "{glossary}"]
            ])
            .to_string(),
        )
        .unwrap();
        cfg.edit_setting("anki/profile/tags", "memento-updated")
            .unwrap();
        let config_ctrl = Rc::new(cfg);

        let app = MementoApp::new().unwrap();
        app.set_dict_open(true);
        app.set_active_sub_text("今日ご飯を食べる。".into());
        let hit = SearchHit::new("食べる", "たべる", "to eat", "v1", 10, false);
        set_dictionary_rows(&app, dictionary_result_rows(&[hit], &[]));

        let pb = app.global::<PopupBridge>();
        pb.set_row_anki_duplicates(Rc::new(VecModel::from(vec![true])).into());
        pb.set_row_anki_note_ids(Rc::new(VecModel::from(vec![i32::MAX])).into());
        pb.set_row_anki_note_id_strings(
            Rc::new(VecModel::from(vec![slint::SharedString::from(
                target_note_id.to_string(),
            )]))
            .into(),
        );

        let audio_cache = Arc::new(Mutex::new(PopupAudioState::default()));
        let sub_ctrl = Rc::new(RefCell::new(SubtitleController::new()));
        let mpv_slot: Rc<RefCell<Option<MpvInstance>>> = Rc::new(RefCell::new(None));
        let epoch = Arc::new(AtomicU64::new(1));

        install_update_anki_note_handler(
            &app,
            config_ctrl,
            audio_cache,
            sub_ctrl,
            mpv_slot,
            dir.path().join("dictionaries.sqlite"),
            dir.path().join("res"),
            dir.path().to_path_buf(),
            epoch,
        );

        pb.invoke_update_anki_note_requested(
            0,
            i32::MAX,
            "食べる".into(),
            "たべる".into(),
            "to eat".into(),
        );
        assert!(app
            .get_status_message()
            .contains(&target_note_id.to_string()));

        server.join().unwrap();
        let actions = server_actions.lock().unwrap();
        assert_eq!(actions.len(), 3);
        assert_eq!(actions[0]["action"], "notesInfo");
        assert_eq!(actions[0]["params"]["notes"][0], target_note_id);
        assert_eq!(actions[1]["action"], "updateNoteFields");
        assert_eq!(actions[1]["params"]["note"]["id"], target_note_id);
        assert_eq!(
            actions[1]["params"]["note"]["fields"]["Sentence"],
            "今日ご飯を食べる。"
        );
        assert_eq!(actions[2]["action"], "addTags");
        assert_eq!(actions[2]["params"]["notes"][0], target_note_id);
        assert_eq!(actions[2]["params"]["tags"], "memento-updated");
    }

    #[test]
    fn parse_manual_audio_context_hud_handles_mm_ss_hh_mm_ss_and_open_ended() {
        assert_eq!(
            parse_manual_audio_context_hud("Audio Context: 01:02.50 → 01:08.75", 10.0),
            Some((62.5, 68.75))
        );
        assert_eq!(
            parse_manual_audio_context_hud("Audio Context: 01:02:03.25 → 01:02:09.50", 0.0),
            Some((3723.25, 3729.5))
        );
        // Open-ended A -> current playback position.
        assert_eq!(
            parse_manual_audio_context_hud("Audio Context: 00:15.00 → ...", 22.4),
            Some((15.0, 22.4))
        );
        // Reversed open-ended (current_pos < A) swaps cleanly to ascending range.
        assert_eq!(
            parse_manual_audio_context_hud("Audio Context: 00:25.00 → ...", 18.5),
            Some((18.5, 25.0))
        );
        assert_eq!(parse_manual_audio_context_hud("", 5.0), None);
        assert_eq!(parse_manual_audio_context_hud("Audio Context: 00:10.00 → 00:10.00", 5.0), None);
        assert_eq!(parse_manual_audio_context_hud("Not a HUD", 5.0), None);
    }

    #[test]
    fn resolve_anki_media_timings_applies_sub_speed_sub_delay_and_manual_hud() {
        // Subtitle cue 10.0..14.0 with sub_speed=2.0 and sub_delay=-1.5:
        // subtitle_to_media_time(t, 2.0, -1.5) = t * 2.0 - 1.5 -> 18.5..26.5
        // Multi-sub selection 8.0..16.0 -> 14.5..30.5
        let t = resolve_anki_media_timings(
            Some((10.0, 14.0)),
            Some((8.0, 16.0)),
            2.0,
            -1.5,
            None,
            false,
        );
        assert!((t.subtitle_start - 18.5).abs() < 1e-6);
        assert!((t.subtitle_end - 26.5).abs() < 1e-6);
        assert!((t.context_start - 14.5).abs() < 1e-6);
        assert!((t.context_end - 30.5).abs() < 1e-6);

        // When mining from Manual Search / OCR with manual A/B HUD set, use manual media timestamps
        // verbatim without re-applying sub-delay / sub-speed.
        let manual = resolve_anki_media_timings(
            Some((10.0, 14.0)),
            None,
            2.0,
            5.0,
            Some((42.0, 48.5)),
            true,
        );
        assert!((manual.subtitle_start - 42.0).abs() < 1e-6);
        assert!((manual.subtitle_end - 48.5).abs() < 1e-6);
        assert!((manual.context_start - 42.0).abs() < 1e-6);
        assert!((manual.context_end - 48.5).abs() < 1e-6);

        // Also falls back to manual HUD when no active subtitle cue exists at all.
        let fallback = resolve_anki_media_timings(None, None, 1.0, 3.0, Some((5.0, 9.0)), false);
        assert!((fallback.subtitle_start - 5.0).abs() < 1e-6);
        assert!((fallback.subtitle_end - 9.0).abs() < 1e-6);
        assert!((fallback.context_start - 5.0).abs() < 1e-6);
        assert!((fallback.context_end - 9.0).abs() < 1e-6);
    }

    #[test]
    fn encode_context_to_media_context_preserves_full_streaming_fields() {
        let enc = memento_player::EncodeContext {
            path: "https://example.com/watch?v=123".into(),
            audio_id: 2,
            stream_open_filename: "https://cdn.example.com/video.m3u8".into(),
            script_opts: "ytdl_hook-ytdl_path=yt-dlp".into(),
            http_header_fields: vec!["Authorization: Bearer secret".into()],
            referrer: "https://example.com/".into(),
            user_agent: "MementoTest/1.0".into(),
            cookies: true,
            cookies_file: "/tmp/cookies.txt".into(),
            ytdl_raw_options: "cookies-from-browser=firefox".into(),
            external_audio_filename: Some("https://cdn.example.com/audio.m4a".into()),
        };
        let media_ctx = media::MediaContext::from(enc.clone());
        assert_eq!(media_ctx.path, enc.path);
        assert_eq!(media_ctx.audio_id, 2);
        assert_eq!(media_ctx.stream_open_filename, enc.stream_open_filename);
        assert_eq!(media_ctx.script_opts, enc.script_opts);
        assert_eq!(media_ctx.http_header_fields, enc.http_header_fields);
        assert_eq!(media_ctx.referrer, enc.referrer);
        assert_eq!(media_ctx.user_agent, enc.user_agent);
        assert!(media_ctx.cookies);
        assert_eq!(media_ctx.cookies_file, enc.cookies_file);
        assert_eq!(media_ctx.ytdl_raw_options, enc.ytdl_raw_options);
        assert_eq!(media_ctx.external_audio_filename, enc.external_audio_filename);
    }

    #[test]
    fn show_warning_toast_deduplicates_and_sets_warning_state() {
        i_slint_backend_testing::init_no_event_loop();
        let app = MementoApp::new().unwrap();
        show_warning_toast(
            &app,
            &[
                "Audio failed".into(),
                "  ".into(),
                "Audio failed".into(),
                "Screenshot unavailable".into(),
            ],
        );
        assert!(app.get_status_toast_is_warning());
        assert_eq!(
            app.get_status_toast_text().as_str(),
            "Audio failed; Screenshot unavailable"
        );
    }

    #[test]
    fn prepare_anki_note_request_wires_multi_selections_reading_as_expr_and_manual_hud() {
        i_slint_backend_testing::init_no_event_loop();
        let app = MementoApp::new().unwrap();
        let cfg = ConfigController::new();
        cfg.edit_setting("anki/enabled", "true").unwrap();
        cfg.edit_setting("anki/profile/term/deck", "Mining")
            .unwrap();
        cfg.edit_setting("anki/profile/term/model", "JP Mining")
            .unwrap();
        cfg.edit_setting(
            "anki/profile/term/fields",
            &serde_json::json!([
                ["Expression", "{expression}"],
                ["Selection", "{selection}"]
            ])
            .to_string(),
        )
        .unwrap();

        let hit = SearchHit::new("食べる", "たべる", "to eat", "v1", 10, false);
        set_dictionary_rows(&app, dictionary_result_rows(&[hit], &[]));
        let pb = app.global::<PopupBridge>();
        pb.set_bridge_instance_id(9001);
        pb.set_row_reading_as_expression(Rc::new(VecModel::from(vec![true])).into());
        set_active_glossary_selections(9001, 0, vec!["first def".into(), "second def".into()]);
        app.set_audio_context_hud_text("Audio Context: 00:05.00 → 00:09.50".into());
        app.set_selected_token_index(-1);

        let audio_cache = Arc::new(Mutex::new(PopupAudioState::default()));
        let sub_ctrl = RefCell::new(SubtitleController::new());
        let mpv_slot = RefCell::new(None);
        let defs = app.get_dict_results();

        // Explicit `-2` audio_source_override produces empty audio_sources ("Add without audio").
        let prepared = {
            let _guard = ScopedAnkiAddOverride::new(AnkiAddTarget::Root, Some(-2));
            prepare_anki_note_request(&app, &defs, 0, &cfg, &audio_cache, &sub_ctrl, &mpv_slot)
                .unwrap()
        };
        assert!(prepared.context.reading_as_expression);
        assert_eq!(
            prepared.context.selections,
            vec!["first def".to_owned(), "second def".to_owned()]
        );
        assert!(prepared.media_input.audio_sources.is_empty());
        assert!((prepared.media_input.context_start - 5.0).abs() < 1e-6);
        assert!((prepared.media_input.context_end - 9.5).abs() < 1e-6);
        clear_active_glossary_selections(9001, None);
    }

    #[test]
    fn audio_cache_invalidation_clears_popup_state_and_updates_timeout() {
        i_slint_backend_testing::init_no_event_loop();
        let app = MementoApp::new().unwrap();
        let cache = Arc::new(Mutex::new(PopupAudioState::default()));
        let player: Rc<RefCell<Option<audio::AudioController>>> = Rc::new(RefCell::new(None));
        register_audio_invalidation_target(app.as_weak(), cache.clone(), player);

        let src = AudioSourceEntry {
            name: "Local".into(),
            url: "http://127.0.0.1/audio.mp3".into(),
            kind: AudioSourceType::AudioSourceTypeFile as i64,
            md5: String::new(),
        };
        set_popup_audio_sources_with_statuses(
            &app,
            &cache,
            "食べる",
            "たべる",
            vec![src],
            vec![AUDIO_STATUS_AVAILABLE],
            true,
        );
        assert_eq!(app.get_popup_audio_sources().row_count(), 1);
        assert_eq!(
            app.get_popup_audio_source_statuses().row_data(0),
            Some(AUDIO_STATUS_AVAILABLE)
        );

        invalidate_audio_caches_for_config_change(7_500);
        assert_eq!(audio::download_timeout_ms(), 7_500);
        assert_eq!(app.get_popup_audio_sources().row_count(), 0);
        assert_eq!(app.get_popup_audio_source_statuses().row_count(), 0);
        let st = cache.lock().unwrap();
        assert!(st.sources.is_empty());
        assert!(st.per_term.is_empty());
    }
}

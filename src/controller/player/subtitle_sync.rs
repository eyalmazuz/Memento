//! Subtitle list, active-row highlight, cached text filter, and track list sync helpers.

use std::borrow::Cow;
use std::cell::RefCell;
use std::rc::Rc;

use memento_dict::normalize::{
    apply_compiled_remove_regex, apply_replace_lines, compile_remove_regex, Regex,
};
use memento_player::ASR_STREAM_URI;
use slint::{Model as _, VecModel};

use crate::controller::subtitles::{
    ExternalSubtitleLoad, SubtitleController, SubtitleTrackInfo, ASR_TRACK_ID,
};
use crate::{MementoApp, PlayerTrack, SubEntry};

/// Cached subtitle text filter (`search/remove-regex`, `search/replace-lines`,
/// `search/replace-with`) avoiding per-tick regex compilation.
#[derive(Default)]
pub(super) struct CachedSubtitleFilter {
    remove_regex_pattern: String,
    compiled_regex: Option<Regex>,
    replace_lines: bool,
    replace_with: String,
}

impl CachedSubtitleFilter {
    /// Synchronizes filter settings from `app`, returning `true` iff any filter
    /// setting changed since the last tick.
    pub(super) fn sync_from_app(&mut self, app: &MementoApp) -> bool {
        let pattern = app.get_search_remove_regex();
        let replace_lines = app.get_search_replace_lines();
        let replace_with = app.get_search_replace_with();

        let mut changed = false;
        if self.remove_regex_pattern != pattern.as_str() {
            self.remove_regex_pattern.clear();
            self.remove_regex_pattern.push_str(pattern.as_str());
            self.compiled_regex = compile_remove_regex(&self.remove_regex_pattern);
            changed = true;
        }
        if self.replace_lines != replace_lines {
            self.replace_lines = replace_lines;
            changed = true;
        }
        if self.replace_with != replace_with.as_str() {
            self.replace_with.clear();
            self.replace_with.push_str(replace_with.as_str());
            changed = true;
        }
        changed
    }

    pub(super) fn apply(&self, text: &str) -> String {
        let after_regex = apply_compiled_remove_regex(text, self.compiled_regex.as_ref());
        apply_replace_lines(&after_regex, self.replace_lines, &self.replace_with)
    }
}

struct CachedRemoveRegex {
    pattern: String,
    compiled: Option<Regex>,
}

thread_local! {
    /// Cached compiled `search/remove-regex` for free-function `refresh_subtitle_list`
    /// calls so typing in the subtitle search box never recompiles the regex per keystroke.
    static REMOVE_REGEX_CACHE: RefCell<CachedRemoveRegex> = const {
        RefCell::new(CachedRemoveRegex {
            pattern: String::new(),
            compiled: None,
        })
    };

    /// Last known `(model_ptr, row_count, active_indices)` per `VecModel<SubEntry>`
    /// so `refresh_active_rows` runs in O(1) on steady-state 60Hz ticks and O(|prev| + |active|)
    /// on cue transitions, while still handling brand-new models with stale `previous` slices.
    static MODEL_ACTIVE_CACHE: RefCell<Vec<(usize, usize, Vec<usize>)>> = const {
        RefCell::new(Vec::new())
    };
}

fn with_cached_remove_regex<R>(pattern: &str, f: impl FnOnce(Option<&Regex>) -> R) -> R {
    REMOVE_REGEX_CACHE.with(|cell| {
        let mut cache = cell.borrow_mut();
        if cache.pattern != pattern {
            cache.pattern.clear();
            cache.pattern.push_str(pattern);
            cache.compiled = compile_remove_regex(pattern);
        }
        f(cache.compiled.as_ref())
    })
}

pub(super) fn record_model_active(model: &VecModel<SubEntry>, active: &[usize]) {
    let key = model as *const VecModel<SubEntry> as usize;
    let len = model.row_count();
    MODEL_ACTIVE_CACHE.with(|cell| {
        let mut cache = cell.borrow_mut();
        if let Some(entry) = cache.iter_mut().find(|(k, _, _)| *k == key) {
            entry.1 = len;
            entry.2.clear();
            entry.2.extend_from_slice(active);
        } else {
            if cache.len() >= 16 {
                cache.remove(0);
            }
            cache.push((key, len, active.to_vec()));
        }
    });
}

/// Two-pointer sorted membership cursor: turns `O(N * M)` `.contains(&index)`
/// scans into a single `O(N + M)` pass with zero allocation when `indices` is
/// already sorted ascending.
struct SortedIndexCursor<'a> {
    sorted: Cow<'a, [usize]>,
    cursor: usize,
}

impl<'a> SortedIndexCursor<'a> {
    fn new(indices: &'a [usize]) -> Self {
        let sorted = if indices.windows(2).all(|w| w[0] <= w[1]) {
            Cow::Borrowed(indices)
        } else {
            let mut copy = indices.to_vec();
            copy.sort_unstable();
            Cow::Owned(copy)
        };
        Self { sorted, cursor: 0 }
    }

    #[inline]
    fn contains_monotonic(&mut self, index: usize) -> bool {
        while self.cursor < self.sorted.len() && self.sorted[self.cursor] < index {
            self.cursor += 1;
        }
        self.cursor < self.sorted.len() && self.sorted[self.cursor] == index
    }
}

pub fn track_label(id: i64, language: &str, title: &str) -> String {
    let mut label = format!("Track {id}");
    if !language.is_empty() {
        label.push_str(" [");
        label.push_str(language);
        label.push(']');
    }
    if !title.is_empty() {
        label.push_str(" - ");
        label.push_str(title);
    }
    label
}

pub fn clear_primary_subtitle(app: &MementoApp) {
    app.set_active_sub_text("".into());
    app.set_active_sub_words(Rc::new(VecModel::from(Vec::new())).into());
    app.set_active_sub_lines(Rc::new(VecModel::from(Vec::new())).into());
    app.set_subtitle_furigana_pending(false);
    app.invoke_dismiss_definition();
}

fn normalize_search_str(s: &str, ignore_whitespace: bool) -> String {
    if ignore_whitespace {
        s.chars()
            .filter(|c| !c.is_whitespace())
            .flat_map(char::to_lowercase)
            .collect()
    } else {
        s.chars().flat_map(char::to_lowercase).collect()
    }
}

pub fn refresh_subtitle_list(
    app: &MementoApp,
    model: &VecModel<SubEntry>,
    subtitles: &SubtitleController,
) {
    let query = app.get_subtitle_search_query();
    let ignore_whitespace = app.get_subtitle_search_ignore_whitespace();
    let remove_regex = app.get_search_remove_regex();
    let active = subtitles.active_indices(subtitles.primary_sid());
    let selected = subtitles.selected_indices();
    let entries = subtitles.primary_entries();

    with_cached_remove_regex(remove_regex.as_str(), |compiled_regex| {
        let query_trimmed = query.trim();
        let has_query = !query_trimmed.is_empty();

        // When no regex filter is active, delegate matching to `SubtitleController::search`
        // and walk matching / active / selected in O(N) via two-pointer cursors.
        let matching_vec = if has_query && compiled_regex.is_none() {
            subtitles.search(&query, ignore_whitespace)
        } else {
            Vec::new()
        };
        let q_norm = if has_query && compiled_regex.is_some() {
            normalize_search_str(query_trimmed, ignore_whitespace)
        } else {
            String::new()
        };

        let mut match_cursor = SortedIndexCursor::new(&matching_vec);
        let mut active_cursor = SortedIndexCursor::new(active);
        let mut selected_cursor = SortedIndexCursor::new(selected);

        let mut rows = Vec::with_capacity(entries.len());
        for (index, cue) in entries.iter().enumerate() {
            let raw = String::from_utf16_lossy(&cue.text);
            let display = apply_compiled_remove_regex(&raw, compiled_regex);
            let search_match = if compiled_regex.is_some() {
                !q_norm.is_empty()
                    && normalize_search_str(&display, ignore_whitespace).contains(&q_norm)
            } else {
                match_cursor.contains_monotonic(index)
            };
            rows.push(SubEntry {
                id: (index + 1) as i32,
                start: cue.start as f32,
                end: cue.end as f32,
                text: display.into_owned().into(),
                selected: selected_cursor.contains_monotonic(index),
                is_active: active_cursor.contains_monotonic(index),
                search_match,
            });
        }
        model.set_vec(rows);
    });

    record_model_active(model, active);
}

pub fn refresh_subtitle_marks(model: &VecModel<SubEntry>, subtitles: &SubtitleController) {
    let mut selected_cursor = SortedIndexCursor::new(subtitles.selected_indices());
    for index in 0..model.row_count() {
        let selected = selected_cursor.contains_monotonic(index);
        if let Some(mut row) = model.row_data(index) {
            if row.selected != selected {
                row.selected = selected;
                model.set_row_data(index, row);
            }
        }
    }
}

#[inline]
fn slice_contains(slice: &[usize], needle: usize) -> bool {
    if slice.len() <= 8 {
        slice.contains(&needle)
    } else {
        slice.binary_search(&needle).is_ok()
    }
}

/// Update `SubEntry::is_active` highlights incrementally in O(1) on steady-state
/// ticks and O(|previous| + |active|) on transitions, touching only rows whose
/// active state actually changed.
pub fn refresh_active_rows(model: &VecModel<SubEntry>, previous: &[usize], active: &[usize]) {
    let key = model as *const VecModel<SubEntry> as usize;
    let row_count = model.row_count();

    let handled_incrementally = MODEL_ACTIVE_CACHE.with(|cell| {
        let mut cache = cell.borrow_mut();
        let Some(entry) = cache.iter_mut().find(|(k, _, _)| *k == key) else {
            return false;
        };
        if entry.1 != row_count {
            return false;
        }

        // Verify the cached active set still matches the model in O(|cached|)
        // (1-2 rows in practice) so an external `model.set_vec` never goes out of sync.
        let cache_valid = entry
            .2
            .iter()
            .all(|&idx| idx < row_count && model.row_data(idx).is_some_and(|r| r.is_active));
        if !cache_valid {
            return false;
        }

        // Steady-state 60Hz fast path: nothing changed since the previous tick.
        if entry.2.as_slice() == active
            && previous.iter().all(|&idx| {
                slice_contains(active, idx)
                    || idx >= row_count
                    || model.row_data(idx).is_none_or(|r| !r.is_active)
            })
        {
            return true;
        }

        // Clear rows that were active in `entry.2` or `previous` but are no longer in `active`.
        for &idx in entry.2.iter().chain(previous.iter()) {
            if idx < row_count && !slice_contains(active, idx) {
                if let Some(mut row) = model.row_data(idx) {
                    if row.is_active {
                        row.is_active = false;
                        model.set_row_data(idx, row);
                    }
                }
            }
        }
        // Highlight newly active rows.
        for &idx in active {
            if idx < row_count && !slice_contains(&entry.2, idx) {
                if let Some(mut row) = model.row_data(idx) {
                    if !row.is_active {
                        row.is_active = true;
                        model.set_row_data(idx, row);
                    }
                }
            }
        }

        entry.2.clear();
        entry.2.extend_from_slice(active);
        true
    });

    if handled_incrementally {
        return;
    }

    // First encounter with an untracked `VecModel` (or after an external resize):
    // establish exact ground truth once, then cache it so all subsequent calls are O(1).
    let mut active_cursor = SortedIndexCursor::new(active);
    for index in 0..row_count {
        let now_active = active_cursor.contains_monotonic(index);
        if let Some(mut row) = model.row_data(index) {
            if row.is_active != now_active {
                row.is_active = now_active;
                model.set_row_data(index, row);
            }
        }
    }
    record_model_active(model, active);
}

pub fn clear_subtitle_marks_on_advance(
    model: &VecModel<SubEntry>,
    subtitles: &mut SubtitleController,
    previous: &mut Option<(i64, f64)>,
    paused: bool,
) {
    let sid = subtitles.primary_sid();
    if previous.is_some_and(|(previous_sid, _)| previous_sid != sid) {
        *previous = None;
    }
    if let Some(current) = subtitles.active_cue().map(|cue| (sid, cue.start)) {
        let advanced = previous.is_some_and(|previous| previous != current);
        *previous = Some(current);
        if advanced && !paused && !subtitles.selected_indices().is_empty() {
            subtitles.clear_selection();
            refresh_subtitle_marks(model, subtitles);
        }
    }
}

/// Synchronize `PlaybackState` audio & subtitle track lists with `MementoApp`
/// and `SubtitleController`, returning any external subtitle files that must be
/// parsed on a background worker.
pub fn sync_player_tracks(
    app: &MementoApp,
    state: &memento_player::mpv::PlaybackState,
    subtitles: &mut SubtitleController,
    subtitle_rows: &VecModel<SubEntry>,
    previous_audio: &mut Vec<memento_player::AudioTrack>,
    previous_subtitles: &mut Vec<memento_player::SubtitleTrack>,
    asr_mpv_sid: Option<i64>,
) -> Vec<ExternalSubtitleLoad> {
    let audio = &state.audio_tracks;
    if audio != previous_audio {
        let rows: Vec<PlayerTrack> = audio
            .iter()
            .filter_map(|track| {
                Some(PlayerTrack {
                    id: i32::try_from(track.id).ok()?,
                    label: track_label(track.id, &track.language, &track.title).into(),
                    selected: track.selected,
                })
            })
            .collect();
        app.set_audio_tracks(Rc::new(VecModel::from(rows)).into());
        previous_audio.clone_from(audio);
    }
    let ui_aid = i32::try_from(state.audio_id).unwrap_or(0);
    if app.get_audio_id() != ui_aid {
        app.set_audio_id(ui_aid);
    }

    let mut pending_loads = Vec::new();
    let effective_asr_sid = asr_mpv_sid.or_else(|| subtitles.asr_mpv_sid());
    let tracks = &state.subtitle_tracks;
    if tracks != previous_subtitles {
        let ui_tracks: Vec<PlayerTrack> = tracks
            .iter()
            .filter(|track| {
                track.external_path != ASR_STREAM_URI && Some(track.id) != effective_asr_sid
            })
            .filter_map(|track| {
                Some(PlayerTrack {
                    id: i32::try_from(track.id).ok()?,
                    label: track_label(track.id, &track.language, &track.title).into(),
                    selected: track.selected,
                })
            })
            .collect();
        app.set_subtitle_tracks(Rc::new(VecModel::from(ui_tracks)).into());
        let info: Vec<SubtitleTrackInfo> = tracks
            .iter()
            .filter(|track| {
                track.external_path != ASR_STREAM_URI && Some(track.id) != effective_asr_sid
            })
            .map(|track| SubtitleTrackInfo {
                id: track.id,
                external: track.external,
                external_path: track.external_path.clone(),
            })
            .collect();
        pending_loads = subtitles.sync_tracks(&info);
        previous_subtitles.clone_from(tracks);
        refresh_subtitle_list(app, subtitle_rows, subtitles);
    }

    let asr_selected = app.get_asr_track_selected() || app.get_asr_running();
    let raw_primary = (state.position - state.sub_delay) / state.sub_speed.max(f64::EPSILON);
    let primary = if asr_selected {
        effective_asr_sid
            .or_else(|| subtitles.asr_track_id())
            .unwrap_or(ASR_TRACK_ID)
    } else {
        state.primary_sid
    };
    if subtitles.primary_sid() != primary {
        if asr_selected && subtitles.has_asr_track() {
            subtitles.resume_asr_track(raw_primary);
        } else {
            subtitles.set_primary_sid(primary);
        }
        refresh_subtitle_list(app, subtitle_rows, subtitles);
    }
    let secondary = state.secondary_sid;
    if subtitles.secondary_sid() != secondary {
        subtitles.set_secondary_sid(secondary);
    }
    let ui_primary_sid = i32::try_from(state.primary_sid).unwrap_or(0);
    if app.get_primary_sid() != ui_primary_sid {
        app.set_primary_sid(ui_primary_sid);
    }
    let ui_secondary_sid = i32::try_from(secondary).unwrap_or(0);
    if app.get_secondary_sid() != ui_secondary_sid {
        app.set_secondary_sid(ui_secondary_sid);
    }
    pending_loads
}

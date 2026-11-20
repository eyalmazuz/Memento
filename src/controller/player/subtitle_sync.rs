//! Subtitle list, active-row highlight, cached text filter, and track list sync helpers.

use std::borrow::Cow;
use std::cell::RefCell;
use std::rc::Rc;

use memento_dict::normalize::{
    apply_compiled_remove_regex, apply_replace_lines, compile_remove_regex, Regex,
};
use memento_player::{subtitle_to_media_time, ASR_STREAM_URI};
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

struct ModelActiveEntry {
    model_ptr: usize,
    row_count: usize,
    /// Original entry indices currently marked active.
    active_entries: Vec<usize>,
    /// Model row indices corresponding to `active_entries`.
    active_rows: Vec<usize>,
    /// Optional monotonic `model_row -> original_entry_index` table when empty
    /// regex-filtered rows were omitted (`None` on the 1:1 common path).
    row_to_entry: Option<Vec<usize>>,
}

impl ModelActiveEntry {
    #[inline]
    fn entry_to_model_row(&self, entry_idx: usize) -> Option<usize> {
        match self.row_to_entry.as_deref() {
            None => (entry_idx < self.row_count).then_some(entry_idx),
            Some(map) => map.binary_search(&entry_idx).ok(),
        }
    }
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

    /// Last known active-row state per `VecModel<SubEntry>` so `refresh_active_rows`
    /// runs in O(1) on steady-state 60Hz ticks and O(|prev| + |active|) on cue transitions,
    /// while remaining accurate when regex filtering omits empty rows or when a brand-new
    /// model arrives with stale `previous` slices.
    static MODEL_ACTIVE_CACHE: RefCell<Vec<ModelActiveEntry>> = const {
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

fn record_model_active_mapped(
    model: &VecModel<SubEntry>,
    active_entries: &[usize],
    row_to_entry: Option<Vec<usize>>,
) {
    let key = model as *const VecModel<SubEntry> as usize;
    let len = model.row_count();
    let active_rows: Vec<usize> = match row_to_entry.as_deref() {
        None => active_entries
            .iter()
            .copied()
            .filter(|&idx| idx < len)
            .collect(),
        Some(map) => active_entries
            .iter()
            .filter_map(|&idx| map.binary_search(&idx).ok())
            .collect(),
    };
    MODEL_ACTIVE_CACHE.with(|cell| {
        let mut cache = cell.borrow_mut();
        if let Some(entry) = cache.iter_mut().find(|e| e.model_ptr == key) {
            entry.row_count = len;
            entry.active_entries.clear();
            entry.active_entries.extend_from_slice(active_entries);
            entry.active_rows = active_rows;
            entry.row_to_entry = row_to_entry;
        } else {
            if cache.len() >= 16 {
                cache.remove(0);
            }
            cache.push(ModelActiveEntry {
                model_ptr: key,
                row_count: len,
                active_entries: active_entries.to_vec(),
                active_rows,
                row_to_entry,
            });
        }
    });
}

pub(super) fn record_model_active(model: &VecModel<SubEntry>, active: &[usize]) {
    record_model_active_mapped(model, active, None);
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

/// Refresh the primary subtitle list model applying `sub-speed` and `sub-delay`
/// to displayed timestamps (H3.7) and omitting rows stripped completely empty
/// by `search/remove-regex` so blank 24px rows never appear (M1).
pub fn refresh_subtitle_list_with_timing(
    app: &MementoApp,
    model: &VecModel<SubEntry>,
    subtitles: &SubtitleController,
    sub_speed: f64,
    sub_delay: f64,
) {
    let query = app.get_subtitle_search_query();
    let ignore_whitespace = app.get_subtitle_search_ignore_whitespace();
    let remove_regex = app.get_search_remove_regex();
    let active = subtitles.active_indices(subtitles.primary_sid());
    let selected = subtitles.selected_indices();
    let entries = subtitles.primary_entries();

    let row_to_entry = with_cached_remove_regex(remove_regex.as_str(), |compiled_regex| {
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
        let mut kept_entries: Option<Vec<usize>> = None;
        for (index, cue) in entries.iter().enumerate() {
            let raw = String::from_utf16_lossy(&cue.text);
            let display = apply_compiled_remove_regex(&raw, compiled_regex);
            if compiled_regex.is_some() && display.trim().is_empty() {
                // Lazily allocate the mapping only when at least one row is actually dropped.
                if kept_entries.is_none() {
                    kept_entries = Some((0..rows.len()).collect());
                }
                // Advance cursors past the skipped entry index to keep two-pointer sync clean.
                let _ = active_cursor.contains_monotonic(index);
                let _ = selected_cursor.contains_monotonic(index);
                continue;
            }
            if let Some(map) = kept_entries.as_mut() {
                map.push(index);
            }
            let search_match = if compiled_regex.is_some() {
                !q_norm.is_empty()
                    && normalize_search_str(&display, ignore_whitespace).contains(&q_norm)
            } else {
                match_cursor.contains_monotonic(index)
            };
            let start = subtitle_to_media_time(cue.start, sub_speed, sub_delay) as f32;
            let end = subtitle_to_media_time(cue.end, sub_speed, sub_delay) as f32;
            rows.push(SubEntry {
                id: (index + 1) as i32,
                start,
                end,
                text: display.into_owned().into(),
                selected: selected_cursor.contains_monotonic(index),
                is_active: active_cursor.contains_monotonic(index),
                search_match,
            });
        }
        model.set_vec(rows);
        kept_entries
    });

    record_model_active_mapped(model, active, row_to_entry);
}

/// Refresh the secondary subtitle list model applying `sub-speed` and
/// `secondary-sub-delay` to displayed timestamps (H3.7) and omitting rows
/// stripped completely empty by `search/remove-regex` (M1).
pub fn refresh_secondary_subtitle_list_with_timing(
    app: &MementoApp,
    model: &VecModel<SubEntry>,
    subtitles: &SubtitleController,
    sub_speed: f64,
    secondary_sub_delay: f64,
) {
    let query = app.get_subtitle_search_query();
    let ignore_whitespace = app.get_subtitle_search_ignore_whitespace();
    let remove_regex = app.get_search_remove_regex();
    let active = subtitles.active_indices(subtitles.secondary_sid());
    let entries = subtitles.secondary_entries();

    let row_to_entry = with_cached_remove_regex(remove_regex.as_str(), |compiled_regex| {
        let query_trimmed = query.trim();
        let q_norm = if !query_trimmed.is_empty() {
            normalize_search_str(query_trimmed, ignore_whitespace)
        } else {
            String::new()
        };

        let mut active_cursor = SortedIndexCursor::new(active);
        let mut rows = Vec::with_capacity(entries.len());
        let mut kept_entries: Option<Vec<usize>> = None;
        for (index, cue) in entries.iter().enumerate() {
            let raw = String::from_utf16_lossy(&cue.text);
            let display = apply_compiled_remove_regex(&raw, compiled_regex);
            if compiled_regex.is_some() && display.trim().is_empty() {
                if kept_entries.is_none() {
                    kept_entries = Some((0..rows.len()).collect());
                }
                let _ = active_cursor.contains_monotonic(index);
                continue;
            }
            if let Some(map) = kept_entries.as_mut() {
                map.push(index);
            }
            let search_match = !q_norm.is_empty()
                && normalize_search_str(&display, ignore_whitespace).contains(&q_norm);
            let start = subtitle_to_media_time(cue.start, sub_speed, secondary_sub_delay) as f32;
            let end = subtitle_to_media_time(cue.end, sub_speed, secondary_sub_delay) as f32;
            rows.push(SubEntry {
                id: (index + 1) as i32,
                start,
                end,
                text: display.into_owned().into(),
                selected: false,
                is_active: active_cursor.contains_monotonic(index),
                search_match,
            });
        }
        model.set_vec(rows);
        kept_entries
    });

    record_model_active_mapped(model, active, row_to_entry);
}

pub fn refresh_subtitle_list(
    app: &MementoApp,
    model: &VecModel<SubEntry>,
    subtitles: &SubtitleController,
) {
    refresh_subtitle_list_with_timing(app, model, subtitles, 1.0, 0.0);
}

pub fn refresh_subtitle_marks(model: &VecModel<SubEntry>, subtitles: &SubtitleController) {
    let mut selected_cursor = SortedIndexCursor::new(subtitles.selected_indices());
    for model_row in 0..model.row_count() {
        if let Some(mut row) = model.row_data(model_row) {
            let entry_index = usize::try_from(row.id - 1).unwrap_or(model_row);
            let selected = selected_cursor.contains_monotonic(entry_index);
            if row.selected != selected {
                row.selected = selected;
                model.set_row_data(model_row, row);
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
/// active state actually changed. Preserves exact entry-index mapping even when
/// empty regex-filtered rows were omitted from `model`.
pub fn refresh_active_rows(model: &VecModel<SubEntry>, previous: &[usize], active: &[usize]) {
    let key = model as *const VecModel<SubEntry> as usize;
    let row_count = model.row_count();

    let handled_incrementally = MODEL_ACTIVE_CACHE.with(|cell| {
        let mut cache = cell.borrow_mut();
        let Some(entry) = cache.iter_mut().find(|e| e.model_ptr == key) else {
            return false;
        };
        if entry.row_count != row_count {
            return false;
        }

        // Verify the cached active set still matches the model in O(|cached|)
        // (1-2 rows in practice) so an external `model.set_vec` never goes out of sync.
        let cache_valid = entry
            .active_rows
            .iter()
            .all(|&row_idx| row_idx < row_count && model.row_data(row_idx).is_some_and(|r| r.is_active));
        if !cache_valid {
            return false;
        }

        // Steady-state 60Hz fast path: nothing changed since the previous tick.
        if entry.active_entries.as_slice() == active
            && previous.iter().all(|&entry_idx| {
                slice_contains(active, entry_idx)
                    || entry
                        .entry_to_model_row(entry_idx)
                        .is_none_or(|row_idx| model.row_data(row_idx).is_none_or(|r| !r.is_active))
            })
        {
            return true;
        }

        // Clear rows that were active in `entry.active_entries` or `previous` but not in `active`.
        for &entry_idx in entry.active_entries.iter().chain(previous.iter()) {
            if !slice_contains(active, entry_idx) {
                if let Some(row_idx) = entry.entry_to_model_row(entry_idx) {
                    if let Some(mut row) = model.row_data(row_idx) {
                        if row.is_active {
                            row.is_active = false;
                            model.set_row_data(row_idx, row);
                        }
                    }
                }
            }
        }

        // Highlight newly active rows and record updated model row indices.
        let mut next_active_rows = Vec::with_capacity(active.len());
        for &entry_idx in active {
            if let Some(row_idx) = entry.entry_to_model_row(entry_idx) {
                next_active_rows.push(row_idx);
                if !slice_contains(&entry.active_entries, entry_idx) {
                    if let Some(mut row) = model.row_data(row_idx) {
                        if !row.is_active {
                            row.is_active = true;
                            model.set_row_data(row_idx, row);
                        }
                    }
                }
            }
        }

        entry.active_entries.clear();
        entry.active_entries.extend_from_slice(active);
        entry.active_rows = next_active_rows;
        true
    });

    if handled_incrementally {
        return;
    }

    // First encounter with an untracked `VecModel` (or after an external resize):
    // establish exact ground truth once via `row.id - 1` (falling back to row index)
    // and rebuild `row_to_entry` if any rows were filtered out.
    let mut active_cursor = SortedIndexCursor::new(active);
    let mut row_to_entry: Option<Vec<usize>> = None;
    for model_row in 0..row_count {
        if let Some(mut row) = model.row_data(model_row) {
            let entry_idx = usize::try_from(row.id - 1).unwrap_or(model_row);
            if entry_idx != model_row && row_to_entry.is_none() {
                row_to_entry = Some((0..model_row).collect());
            }
            if let Some(map) = row_to_entry.as_mut() {
                map.push(entry_idx);
            }
            let now_active = active_cursor.contains_monotonic(entry_idx);
            if row.is_active != now_active {
                row.is_active = now_active;
                model.set_row_data(model_row, row);
            }
        }
    }
    record_model_active_mapped(model, active, row_to_entry);
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
        refresh_subtitle_list_with_timing(
            app,
            subtitle_rows,
            subtitles,
            state.sub_speed,
            state.sub_delay,
        );
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
        refresh_subtitle_list_with_timing(
            app,
            subtitle_rows,
            subtitles,
            state.sub_speed,
            state.sub_delay,
        );
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::controller::subtitles::create_cue;

    #[test]
    fn subtitle_list_applies_sub_delay_secondary_sub_delay_and_sub_speed() {
        i_slint_backend_testing::init_no_event_loop();
        let app = MementoApp::new().unwrap();
        let mut subs = SubtitleController::new();
        subs.ensure_track(1);
        subs.set_primary_sid(1);
        subs.load_cues(vec![
            create_cue("First", 2.0, 5.0),
            create_cue("Second", 10.0, 12.5),
        ]);
        subs.ensure_track(2);
        subs.set_secondary_sid(2);
        subs.set_track_cues(
            2,
            vec![
                create_cue("Sec1", 1.0, 3.0),
                create_cue("Sec2", 8.0, 9.0),
            ],
        );

        let primary_model = VecModel::<SubEntry>::default();
        // Positive delay + speed multiplier: media_time = cue_time * 1.5 + 2.0
        refresh_subtitle_list_with_timing(&app, &primary_model, &subs, 1.5, 2.0);
        assert_eq!(primary_model.row_count(), 2);
        assert!((primary_model.row_data(0).unwrap().start - 5.0).abs() < 1e-4);
        assert!((primary_model.row_data(0).unwrap().end - 9.5).abs() < 1e-4);
        assert!((primary_model.row_data(1).unwrap().start - 17.0).abs() < 1e-4);
        assert!((primary_model.row_data(1).unwrap().end - 20.75).abs() < 1e-4);

        // Negative delay clamps cleanly at 0.0.
        refresh_subtitle_list_with_timing(&app, &primary_model, &subs, 1.0, -3.0);
        assert!((primary_model.row_data(0).unwrap().start - 0.0).abs() < 1e-4);
        assert!((primary_model.row_data(0).unwrap().end - 2.0).abs() < 1e-4);

        // Secondary subtitle list uses secondary_sub_delay.
        let secondary_model = VecModel::<SubEntry>::default();
        refresh_secondary_subtitle_list_with_timing(&app, &secondary_model, &subs, 1.0, 4.5);
        assert_eq!(secondary_model.row_count(), 2);
        assert!((secondary_model.row_data(0).unwrap().start - 5.5).abs() < 1e-4);
        assert!((secondary_model.row_data(0).unwrap().end - 7.5).abs() < 1e-4);
        assert!((secondary_model.row_data(1).unwrap().start - 12.5).abs() < 1e-4);
        assert!((secondary_model.row_data(1).unwrap().end - 13.5).abs() < 1e-4);
    }

    #[test]
    fn remove_regex_omits_blank_rows_while_preserving_active_and_selected_mapping() {
        i_slint_backend_testing::init_no_event_loop();
        let app = MementoApp::new().unwrap();
        let mut subs = SubtitleController::new();
        subs.ensure_track(1);
        subs.set_primary_sid(1);
        subs.load_cues(vec![
            create_cue("（拍手）", 1.0, 2.0),         // entry 0: stripped completely empty!
            create_cue("（笑）おはよう", 3.0, 4.0),    // entry 1: kept as "おはよう"
            create_cue("   （SE）  ", 5.0, 6.0),      // entry 2: stripped completely empty!
            create_cue("こんばんは", 7.0, 8.0),        // entry 3: kept as "こんばんは"
        ]);
        subs.toggle_select(3);
        app.set_search_remove_regex("（[^）]*）".into());

        let model = VecModel::<SubEntry>::default();
        refresh_subtitle_list_with_timing(&app, &model, &subs, 1.0, 0.0);

        // Only 2 non-empty rows should appear, keeping their original 1-based ids (2 and 4).
        assert_eq!(model.row_count(), 2);
        let r0 = model.row_data(0).unwrap();
        let r1 = model.row_data(1).unwrap();
        assert_eq!(r0.id, 2);
        assert_eq!(r0.text.as_str(), "おはよう");
        assert!(!r0.selected);
        assert_eq!(r1.id, 4);
        assert_eq!(r1.text.as_str(), "こんばんは");
        assert!(r1.selected);

        // Active row highlighting maps original entry index 3 -> model row 1,
        // and gracefully ignores dropped entry index 2.
        refresh_active_rows(&model, &[], &[3]);
        assert!(!model.row_data(0).unwrap().is_active);
        assert!(model.row_data(1).unwrap().is_active);

        refresh_active_rows(&model, &[3], &[2]);
        assert!(!model.row_data(0).unwrap().is_active);
        assert!(!model.row_data(1).unwrap().is_active);

        refresh_active_rows(&model, &[2], &[1]);
        assert!(model.row_data(0).unwrap().is_active);
        assert!(!model.row_data(1).unwrap().is_active);

        // Selection marks also follow `row.id - 1` accurately.
        subs.toggle_select(1);
        refresh_subtitle_marks(&model, &subs);
        assert!(model.row_data(0).unwrap().selected);
        assert!(model.row_data(1).unwrap().selected);

        // Secondary list also omits rows stripped completely empty by remove-regex.
        subs.ensure_track(2);
        subs.set_secondary_sid(2);
        subs.set_track_cues(
            2,
            vec![
                create_cue("（音楽）", 1.0, 2.0),
                create_cue("Hello", 3.0, 4.0),
            ],
        );
        let sec_model = VecModel::<SubEntry>::default();
        refresh_secondary_subtitle_list_with_timing(&app, &sec_model, &subs, 1.0, 0.5);
        assert_eq!(sec_model.row_count(), 1);
        assert_eq!(sec_model.row_data(0).unwrap().id, 2);
        assert_eq!(sec_model.row_data(0).unwrap().text.as_str(), "Hello");
        assert!((sec_model.row_data(0).unwrap().start - 3.5).abs() < 1e-4);
    }
}

//! Subtitle controller connecting memento-subtitle to Slint SubtitlePanel.

pub mod furigana;

pub use furigana::{
    cached_subtitle_furigana_parts, clear_furigana_cache_and_cancel_prewarm,
    prewarm_subtitle_furigana, prewarm_subtitle_furigana_window, segment_subtitle_text,
    subtitle_furigana_parts,
};

use std::cmp::Ordering;
use std::path::PathBuf;

use memento_subtitle::model::{
    self, ActiveCueActionKind, SubtitleExternalPathInput, SubtitleTrackInput, TrackAction,
};
use memento_subtitle::SubtitleEntry;

/// Normalize subtitle cue text: converts CRLF (`\r\n`) and CR (`\r`) line
/// endings to `\n`, collapses redundant empty blank lines inside multi-line
/// cues (e.g. `"あっ。\n\n勝った勝った！"` -> `"あっ。\n勝った勝った！"`),
/// and trims leading/trailing blank lines and whitespace.
pub fn normalize_subtitle_text(text: &str) -> String {
    let stripped = text.trim_end_matches('\0');
    let normalized = stripped.replace("\r\n", "\n").replace('\r', "\n");
    let mut out = String::with_capacity(normalized.len());
    for line in normalized.split('\n') {
        if line.trim().is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(line.trim_end_matches([' ', '\t']));
    }
    out.trim().to_string()
}

/// Timestamp tolerance (seconds) for deduplicating exact duplicate cues.
const DUPLICATE_CUE_TIME_EPSILON: f64 = 0.01 + 1e-9;

/// Validates, normalizes, deduplicates, and stably sorts subtitle cues by start timestamp.
///
/// Discards cues where:
/// - `start < 0.0`
/// - `end <= start`
/// - `!start.is_finite()`
/// - `!end.is_finite()`
/// - normalized text is empty
///
/// Normalizes `\r\n`/`\r` line endings, collapses redundant empty blank lines
/// inside cues, and removes exact duplicate cues (identical trimmed/normalized
/// text with matching `start` and `end` within `0.01s`).
pub fn filter_and_sort_cues<C: AsRef<[SubtitleEntry]>>(cues: C) -> Vec<SubtitleEntry> {
    let slice = cues.as_ref();
    let mut valid: Vec<SubtitleEntry> = slice
        .iter()
        .filter(|c| c.start >= 0.0 && c.end > c.start && c.start.is_finite() && c.end.is_finite())
        .filter_map(|c| {
            let norm = normalize_subtitle_text(&String::from_utf16_lossy(&c.text));
            if norm.is_empty() {
                return None;
            }
            Some(SubtitleEntry {
                text: norm.encode_utf16().collect(),
                start: c.start,
                end: c.end,
            })
        })
        .collect();
    valid.sort_by(|a, b| a.start.partial_cmp(&b.start).unwrap_or(Ordering::Equal));
    let mut deduped: Vec<SubtitleEntry> = Vec::with_capacity(valid.len());
    for cue in valid {
        let is_dup = deduped
            .iter()
            .rev()
            .take_while(|prev| cue.start - prev.start <= DUPLICATE_CUE_TIME_EPSILON)
            .any(|prev| {
                (cue.start - prev.start).abs() <= DUPLICATE_CUE_TIME_EPSILON
                    && (cue.end - prev.end).abs() <= DUPLICATE_CUE_TIME_EPSILON
                    && cue.text == prev.text
            });
        if !is_dup {
            deduped.push(cue);
        }
    }
    deduped
}

/// Helper to create a SubtitleEntry with text encoded as UTF-16 code units.
pub fn create_cue(text: &str, start: f64, end: f64) -> SubtitleEntry {
    SubtitleEntry {
        text: text.encode_utf16().collect(),
        start,
        end,
    }
}

/// A subtitle track reported by mpv. Positive IDs are mpv SIDs; zero
/// means no track; ASR uses [`ASR_TRACK_ID`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubtitleTrackInfo {
    pub id: i64,
    pub external: bool,
    pub external_path: String,
}

/// An external subtitle file which should be parsed for the given SID.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExternalSubtitleLoad {
    pub track_id: i64,
    pub path: PathBuf,
}

/// One decoded cue from mpv's in-memory `sub-lines` property.
#[derive(Clone, Debug, PartialEq)]
pub struct MemorySubtitleCue {
    pub id: i64,
    pub text: Vec<u16>,
    pub start: f64,
    pub end: f64,
}

/// Cues and selection state belonging to one mpv subtitle SID.
#[derive(Clone, Debug)]
pub struct SubtitleTrackModel {
    id: i64,
    external: bool,
    external_path: Option<String>,
    entries: Vec<SubtitleEntry>,
    prefix_max_end: Vec<f64>,
    entries_sorted_by_start: bool,
    selected_indices: Vec<usize>,
    selection_anchor: Option<usize>,
    active_indices: Vec<usize>,
    is_memory_track: bool,
    is_asr_track: bool,
    loaded_complete: bool,
}

impl SubtitleTrackModel {
    fn new(id: i64) -> Self {
        Self {
            id,
            external: false,
            external_path: None,
            entries: Vec::new(),
            prefix_max_end: Vec::new(),
            entries_sorted_by_start: true,
            selected_indices: Vec::new(),
            selection_anchor: None,
            active_indices: Vec::new(),
            is_memory_track: false,
            is_asr_track: false,
            loaded_complete: false,
        }
    }

    pub fn id(&self) -> i64 {
        self.id
    }

    pub fn is_external(&self) -> bool {
        self.external
    }

    pub fn external_path(&self) -> Option<&str> {
        self.external_path.as_deref()
    }

    pub fn entries(&self) -> &[SubtitleEntry] {
        &self.entries
    }

    pub fn selected_indices(&self) -> &[usize] {
        &self.selected_indices
    }

    pub fn active_indices(&self) -> &[usize] {
        &self.active_indices
    }

    pub fn is_memory_track(&self) -> bool {
        self.is_memory_track
    }

    pub fn is_asr_track(&self) -> bool {
        self.is_asr_track
    }

    pub fn is_loaded_complete(&self) -> bool {
        self.loaded_complete
    }

    fn clear_entries(&mut self) {
        self.entries.clear();
        self.prefix_max_end.clear();
        self.entries_sorted_by_start = true;
        self.selected_indices.clear();
        self.selection_anchor = None;
        self.active_indices.clear();
    }

    fn set_entries(&mut self, entries: Vec<SubtitleEntry>) {
        self.entries = entries;
        self.rebuild_time_index();
    }

    fn rebuild_time_index(&mut self) {
        self.prefix_max_end.clear();
        self.prefix_max_end.reserve(self.entries.len());
        let mut max_end = f64::NEG_INFINITY;
        let mut sorted = true;
        let mut prev_start = f64::NEG_INFINITY;
        for entry in &self.entries {
            if !entry.start.is_finite() || entry.start < prev_start {
                sorted = false;
            } else {
                prev_start = entry.start;
            }
            if entry.end > max_end {
                max_end = entry.end;
            }
            self.prefix_max_end.push(max_end);
        }
        self.entries_sorted_by_start = sorted;
    }

    fn insert_entry_at(&mut self, insert_at: usize, entry: SubtitleEntry) {
        let new_start = entry.start;
        let new_end = entry.end;
        self.entries.insert(insert_at, entry);

        if self.entries_sorted_by_start {
            let prev_ok = insert_at == 0 || self.entries[insert_at - 1].start <= new_start;
            let next_ok =
                insert_at + 1 == self.entries.len() || new_start <= self.entries[insert_at + 1].start;
            if !new_start.is_finite() || !prev_ok || !next_ok {
                self.entries_sorted_by_start = false;
            }
        }

        if self.prefix_max_end.len() + 1 == self.entries.len() {
            let prev_max = if insert_at > 0 {
                self.prefix_max_end[insert_at - 1]
            } else {
                f64::NEG_INFINITY
            };
            let cur_max = if new_end > prev_max { new_end } else { prev_max };
            self.prefix_max_end.insert(insert_at, cur_max);
            for slot in &mut self.prefix_max_end[insert_at + 1..] {
                if cur_max > *slot {
                    *slot = cur_max;
                } else {
                    break;
                }
            }
        } else {
            self.rebuild_time_index();
        }
    }

    fn update_entry_in_place(&mut self, index: usize, entry: SubtitleEntry) {
        let new_start = entry.start;
        self.entries[index] = entry;

        if self.entries_sorted_by_start {
            let prev_ok = index == 0 || self.entries[index - 1].start <= new_start;
            let next_ok =
                index + 1 == self.entries.len() || new_start <= self.entries[index + 1].start;
            if !new_start.is_finite() || !prev_ok || !next_ok {
                self.entries_sorted_by_start = false;
            }
        }

        if self.prefix_max_end.len() == self.entries.len() {
            let prev_max = if index > 0 {
                self.prefix_max_end[index - 1]
            } else {
                f64::NEG_INFINITY
            };
            let mut running = prev_max;
            for i in index..self.entries.len() {
                let old = self.prefix_max_end[i];
                let end = self.entries[i].end;
                if end > running {
                    running = end;
                }
                self.prefix_max_end[i] = running;
                if running == old {
                    break;
                }
            }
        } else {
            self.rebuild_time_index();
        }
    }

    fn remove_entry_at(&mut self, remove_at: usize) {
        self.entries.remove(remove_at);
        if self.prefix_max_end.len() == self.entries.len() + 1 {
            let removed_prefix = self.prefix_max_end.remove(remove_at);
            let prev_max = if remove_at > 0 {
                self.prefix_max_end[remove_at - 1]
            } else {
                f64::NEG_INFINITY
            };
            if removed_prefix > prev_max {
                let mut running = prev_max;
                for i in remove_at..self.entries.len() {
                    let old = self.prefix_max_end[i];
                    let end = self.entries[i].end;
                    if end > running {
                        running = end;
                    }
                    self.prefix_max_end[i] = running;
                    if running == old {
                        break;
                    }
                }
            }
        } else {
            self.rebuild_time_index();
        }
    }

    fn replace_memory_entries(&mut self, entries: Vec<SubtitleEntry>) {
        let selected: Vec<_> = self
            .selected_indices
            .iter()
            .filter_map(|&index| self.entries.get(index))
            .collect();
        let anchor = self
            .selection_anchor
            .and_then(|index| self.entries.get(index));
        self.selected_indices = entries
            .iter()
            .enumerate()
            .filter_map(|(index, cue)| selected.contains(&cue).then_some(index))
            .collect();
        self.selection_anchor = anchor
            .and_then(|cue| entries.iter().position(|entry| entry == cue));
        self.set_entries(entries);
    }

    fn update_active_indices(&mut self, raw_time: f64) {
        select_active_indices_into(
            &self.entries,
            &self.prefix_max_end,
            self.entries_sorted_by_start,
            raw_time,
            &mut self.active_indices,
        );
    }
}

/// Local ASR must not share SID 0 with mpv's "no subtitles" selection.
pub const ASR_TRACK_ID: i64 = -1;

/// Tracks subtitle cue lists per mpv SID, including external-file reloads and
/// dynamically supplied in-memory cues. The primary and secondary SIDs remain
/// independent, so their active cues can overlap and be shown simultaneously.
pub struct SubtitleController {
    tracks: Vec<SubtitleTrackModel>,
    asr_cues: Vec<MemorySubtitleCue>,
    asr_mpv_sid: Option<i64>,
    primary_sid: i64,
    secondary_sid: i64,
}

impl Default for SubtitleController {
    fn default() -> Self {
        Self::new()
    }
}

impl SubtitleController {
    pub fn new() -> Self {
        Self {
            tracks: Vec::new(),
            asr_cues: Vec::new(),
            asr_mpv_sid: None,
            primary_sid: 0,
            secondary_sid: 0,
        }
    }

    /// Reconcile per-SID models against mpv's subtitle track list. External
    /// files are requested only when their path changes; callers parse each
    /// returned path and pass the cues to [`Self::set_external_cues`].
    pub fn sync_tracks(&mut self, tracks: &[SubtitleTrackInfo]) -> Vec<ExternalSubtitleLoad> {
        let filtered_tracks: Vec<&SubtitleTrackInfo> = tracks
            .iter()
            .filter(|track| {
                track.external_path != memento_player::ASR_STREAM_URI
                    && Some(track.id) != self.asr_mpv_sid
                    && !self
                        .tracks
                        .iter()
                        .any(|m| m.is_asr_track && m.id == track.id)
            })
            .collect();
        let track_paths: Vec<Vec<u16>> = filtered_tracks
            .iter()
            .map(|track| track.external_path.encode_utf16().collect())
            .collect();
        let track_inputs: Vec<SubtitleTrackInput<'_>> = filtered_tracks
            .iter()
            .zip(&track_paths)
            .map(|(track, path)| {
                SubtitleTrackInput::new(false, track.id, track.external, path.as_slice())
            })
            .collect();
        // The ASR track (whether still at ASR_TRACK_ID or bound to a positive
        // libmpv SID) is managed by AsrController and must survive regular
        // track-list reconciliation, including while another track is selected.
        let model_ids: Vec<i64> = self
            .tracks
            .iter()
            .filter(|track| !track.is_asr_track && track.id > 0)
            .map(|track| track.id)
            .collect();
        let external_path_units: Vec<(i64, Vec<u16>)> = self
            .tracks
            .iter()
            .filter(|track| !track.is_asr_track)
            .filter_map(|track| {
                track
                    .external_path
                    .as_ref()
                    .map(|path| (track.id, path.encode_utf16().collect()))
            })
            .collect();
        let external_paths: Vec<SubtitleExternalPathInput<'_>> = external_path_units
            .iter()
            .map(|(track_id, path)| SubtitleExternalPathInput::new(*track_id, path))
            .collect();
        let plan = model::plan_track_changes(&track_inputs, &model_ids, &external_paths);
        let mut reloads = Vec::new();

        for action in plan.actions {
            match action {
                TrackAction::CreateModel { track_id } => {
                    self.tracks.push(SubtitleTrackModel::new(track_id));
                }
                TrackAction::ReloadExternal {
                    track_id,
                    path,
                    read_file,
                } => {
                    if let Some(track) = self.track_mut(track_id) {
                        let path = String::from_utf16_lossy(path);
                        track.external = true;
                        track.external_path = Some(path.clone());
                        track.clear_entries();
                        track.is_memory_track = false;
                        track.loaded_complete = false;
                        if read_file && !path.is_empty() {
                            reloads.push(ExternalSubtitleLoad {
                                track_id,
                                path: PathBuf::from(path),
                            });
                        }
                    }
                }
                TrackAction::ClearExternal { track_id } => {
                    if let Some(track) = self.track_mut(track_id) {
                        track.external = false;
                        track.external_path = None;
                        track.clear_entries();
                        track.is_memory_track = false;
                        track.loaded_complete = false;
                    }
                }
                TrackAction::RemoveTrack { track_id } => {
                    self.tracks.retain(|track| track.id != track_id);
                }
            }
        }

        reloads
    }

    pub fn tracks(&self) -> &[SubtitleTrackModel] {
        &self.tracks
    }

    pub fn track(&self, sid: i64) -> Option<&SubtitleTrackModel> {
        self.tracks.iter().find(|track| track.id == sid)
    }

    pub fn primary_sid(&self) -> i64 {
        self.primary_sid
    }

    pub fn secondary_sid(&self) -> i64 {
        self.secondary_sid
    }

    pub fn set_primary_sid(&mut self, sid: i64) {
        self.primary_sid = sid.max(0);
    }

    pub fn set_secondary_sid(&mut self, sid: i64) {
        self.secondary_sid = sid.max(0);
    }

    /// Ensure an mpv SID has a subtitle model before receiving asynchronous
    /// memory cues for it.
    pub fn ensure_track(&mut self, sid: i64) -> bool {
        if sid <= 0 {
            return false;
        }
        if self.track(sid).is_none() {
            self.tracks.push(SubtitleTrackModel::new(sid));
        }
        true
    }

    /// Begin an ASR track, using [`ASR_TRACK_ID`] while track creation is
    /// pending (or in unit tests) until [`Self::bind_asr_sid`] binds the new
    /// positive libmpv `sid`.
    pub fn start_asr_track(&mut self) {
        self.asr_mpv_sid = None;
        self.asr_cues.clear();
        self.tracks
            .retain(|track| !track.is_asr_track && track.id != ASR_TRACK_ID);
        let mut model = SubtitleTrackModel::new(ASR_TRACK_ID);
        model.is_asr_track = true;
        model.is_memory_track = true;
        self.tracks.push(model);
        self.primary_sid = ASR_TRACK_ID;
    }

    /// Associate the ASR track model with a real positive libmpv `sid` once
    /// `sub-add` / `sub-create` resolves asynchronously.
    pub fn bind_asr_sid(&mut self, mpv_sid: i64) {
        if mpv_sid <= 0 {
            return;
        }
        self.asr_mpv_sid = Some(mpv_sid);
        let Some(old_id) = self.asr_track_id() else {
            return;
        };
        if old_id != mpv_sid {
            self.tracks
                .retain(|track| track.is_asr_track || track.id != mpv_sid);
            if let Some(asr_track) = self.tracks.iter_mut().find(|track| track.is_asr_track) {
                asr_track.id = mpv_sid;
            }
            if self.primary_sid == old_id || self.primary_sid == ASR_TRACK_ID {
                self.primary_sid = mpv_sid;
            }
        }
    }

    pub fn asr_mpv_sid(&self) -> Option<i64> {
        self.asr_mpv_sid
    }

    pub fn asr_track_id(&self) -> Option<i64> {
        self.tracks
            .iter()
            .find(|track| track.is_asr_track)
            .map(|track| track.id)
    }

    pub fn has_asr_track(&self) -> bool {
        self.tracks.iter().any(|track| track.is_asr_track)
    }

    /// Restore cached cues and select the subtitle at the current media time.
    pub fn resume_asr_track(&mut self, position: f64) -> bool {
        let Some(id) = self.asr_track_id() else {
            return false;
        };
        self.primary_sid = id;
        self.update_sid_position(id, position);
        true
    }

    /// Clear cached ASR cues (e.g. when `aid` switches) while keeping the ASR
    /// track model itself registered and selected.
    pub fn clear_asr_cues(&mut self, position: f64) {
        let _ = position;
        self.asr_cues.clear();
        if let Some(track) = self.tracks.iter_mut().find(|track| track.is_asr_track) {
            track.clear_entries();
            track.is_memory_track = true;
        }
    }

    /// Replace the entire ASR cue cache from a worker snapshot while preserving
    /// any matching multi-row selections and range anchor.
    pub fn set_asr_cues_snapshot(&mut self, cues: Vec<MemorySubtitleCue>, position: f64) -> bool {
        if !self.has_asr_track() {
            return false;
        }
        let (selected_ids, anchor_id) = self.asr_selection();
        self.asr_cues = cues;
        self.rebuild_asr_track(position, &selected_ids, anchor_id);
        true
    }

    pub fn clear_asr_track(&mut self) {
        if self.primary_sid == ASR_TRACK_ID
            || self
                .tracks
                .iter()
                .any(|track| track.is_asr_track && track.id == self.primary_sid)
        {
            self.primary_sid = 0;
        }
        self.asr_mpv_sid = None;
        self.asr_cues.clear();
        self.tracks
            .retain(|track| !track.is_asr_track && track.id != ASR_TRACK_ID);
    }

    /// Insert or update a single ASR cue incrementally (preserving start-time
    /// order, `prefix_max_end`, selection marks, and range anchor without
    /// re-sorting or cloning unrelated cues).
    pub fn add_asr_cue(&mut self, cue: MemorySubtitleCue, position: f64) -> bool {
        let Some(track_idx) = self.tracks.iter().position(|track| track.is_asr_track) else {
            return false;
        };
        if self.tracks[track_idx].entries.len() != self.asr_cues.len() {
            let (selected_ids, anchor_id) = self.asr_selection();
            self.asr_cues.retain(|existing| existing.id != cue.id);
            self.asr_cues.push(cue);
            self.rebuild_asr_track(position, &selected_ids, anchor_id);
            return true;
        }

        let entry = SubtitleEntry {
            text: cue.text.clone(),
            start: cue.start,
            end: cue.end,
        };
        let track = &mut self.tracks[track_idx];

        if let Some(old_pos) = self.asr_cues.iter().position(|existing| existing.id == cue.id) {
            let stays_in_place = (old_pos == 0
                || self.asr_cues[old_pos - 1].start.total_cmp(&cue.start) != Ordering::Greater)
                && (old_pos + 1 == self.asr_cues.len()
                    || cue.start.total_cmp(&self.asr_cues[old_pos + 1].start) != Ordering::Greater);
            if stays_in_place {
                self.asr_cues[old_pos] = cue;
                track.update_entry_in_place(old_pos, entry);
                track.is_memory_track = true;
                track.update_active_indices(position);
                return true;
            }

            let was_selected = track.selected_indices.binary_search(&old_pos).is_ok();
            let was_anchor = track.selection_anchor == Some(old_pos);

            self.asr_cues.remove(old_pos);
            track.remove_entry_at(old_pos);
            track.selected_indices.retain_mut(|idx| {
                if *idx == old_pos {
                    false
                } else {
                    if *idx > old_pos {
                        *idx -= 1;
                    }
                    true
                }
            });
            track.selection_anchor = match track.selection_anchor {
                Some(anchor) if anchor == old_pos => None,
                Some(anchor) if anchor > old_pos => Some(anchor - 1),
                other => other,
            };

            let insert_at = self
                .asr_cues
                .partition_point(|existing| existing.start.total_cmp(&cue.start) != Ordering::Greater);
            self.asr_cues.insert(insert_at, cue);
            track.insert_entry_at(insert_at, entry);
            for idx in &mut track.selected_indices {
                if *idx >= insert_at {
                    *idx += 1;
                }
            }
            if let Some(anchor) = &mut track.selection_anchor {
                if *anchor >= insert_at {
                    *anchor += 1;
                }
            }
            if was_selected {
                if let Err(pos) = track.selected_indices.binary_search(&insert_at) {
                    track.selected_indices.insert(pos, insert_at);
                }
            }
            if was_anchor {
                track.selection_anchor = Some(insert_at);
            }
        } else {
            let insert_at = self
                .asr_cues
                .partition_point(|existing| existing.start.total_cmp(&cue.start) != Ordering::Greater);
            self.asr_cues.insert(insert_at, cue);
            track.insert_entry_at(insert_at, entry);
            for idx in &mut track.selected_indices {
                if *idx >= insert_at {
                    *idx += 1;
                }
            }
            if let Some(anchor) = &mut track.selection_anchor {
                if *anchor >= insert_at {
                    *anchor += 1;
                }
            }
        }

        track.is_memory_track = true;
        track.update_active_indices(position);
        true
    }

    /// Remove a single ASR cue incrementally (preserving `prefix_max_end`,
    /// selection marks, and range anchor without rebuilding the track).
    pub fn remove_asr_cue(&mut self, cue_id: i64, position: f64) -> bool {
        let Some(pos) = self.asr_cues.iter().position(|cue| cue.id == cue_id) else {
            return false;
        };
        let Some(track_idx) = self.tracks.iter().position(|track| track.is_asr_track) else {
            self.asr_cues.remove(pos);
            return true;
        };
        if self.tracks[track_idx].entries.len() != self.asr_cues.len() {
            let (selected_ids, anchor_id) = self.asr_selection();
            self.asr_cues.retain(|cue| cue.id != cue_id);
            self.rebuild_asr_track(position, &selected_ids, anchor_id);
            return true;
        }

        self.asr_cues.remove(pos);
        let track = &mut self.tracks[track_idx];
        track.remove_entry_at(pos);
        track.selected_indices.retain_mut(|idx| {
            if *idx == pos {
                false
            } else {
                if *idx > pos {
                    *idx -= 1;
                }
                true
            }
        });
        track.selection_anchor = match track.selection_anchor {
            Some(anchor) if anchor == pos => None,
            Some(anchor) if anchor > pos => Some(anchor - 1),
            other => other,
        };
        track.is_memory_track = true;
        track.update_active_indices(position);
        true
    }

    fn asr_selection(&self) -> (Vec<i64>, Option<i64>) {
        self.tracks
            .iter()
            .find(|track| track.is_asr_track)
            .map(|track| {
                (
                    track
                        .selected_indices
                        .iter()
                        .filter_map(|index| self.asr_cues.get(*index).map(|cue| cue.id))
                        .collect(),
                    track
                        .selection_anchor
                        .and_then(|index| self.asr_cues.get(index))
                        .map(|cue| cue.id),
                )
            })
            .unwrap_or_default()
    }

    fn rebuild_asr_track(&mut self, position: f64, selected_ids: &[i64], anchor_id: Option<i64>) {
        self.asr_cues.sort_by(|a, b| a.start.total_cmp(&b.start));
        let selected_indices = self
            .asr_cues
            .iter()
            .enumerate()
            .filter_map(|(index, cue)| selected_ids.contains(&cue.id).then_some(index))
            .collect();
        let entries: Vec<_> = self
            .asr_cues
            .iter()
            .map(|cue| SubtitleEntry {
                text: cue.text.clone(),
                start: cue.start,
                end: cue.end,
            })
            .collect();
        let anchor = anchor_id.and_then(|id| self.asr_cues.iter().position(|cue| cue.id == id));
        if let Some(track) = self.tracks.iter_mut().find(|track| track.is_asr_track) {
            track.set_entries(entries);
            track.is_memory_track = true;
            track.selected_indices = selected_indices;
            track.selection_anchor = anchor;
            track.update_active_indices(position);
        }
    }

    pub fn remove_track(&mut self, sid: i64) -> bool {
        let old_len = self.tracks.len();
        self.tracks.retain(|track| track.id != sid);
        if self.primary_sid == sid {
            self.primary_sid = 0;
        }
        if self.secondary_sid == sid {
            self.secondary_sid = 0;
        }
        self.tracks.len() != old_len
    }

    /// Replace cues for the currently selected track. This convenience is
    /// useful for a single-track source before mpv's track list is available;
    /// normal playback should use [`Self::set_track_cues`] with an explicit SID.
    ///
    /// Auto-selects SID `1` only when `primary_sid == 0` (no track selected),
    /// leaving active tracks such as [`ASR_TRACK_ID`] intact.
    pub fn set_entries(&mut self, entries: Vec<SubtitleEntry>) {
        let sid = if self.primary_sid == 0 {
            self.primary_sid = 1;
            1
        } else {
            self.primary_sid
        };
        if self.track(sid).is_none() {
            self.tracks.push(SubtitleTrackModel::new(sid));
        }
        if let Some(track) = self.track_mut(sid) {
            track.set_entries(entries);
            track.selected_indices.clear();
            track.selection_anchor = None;
            track.active_indices.clear();
            if !track.is_asr_track {
                track.is_memory_track = false;
            }
            track.loaded_complete = false;
        }
    }

    pub fn set_cues(&mut self, cues: Vec<SubtitleEntry>) {
        self.set_entries(cues);
    }

    pub fn load_cues<C: AsRef<[SubtitleEntry]>>(&mut self, cues: C) {
        self.set_entries(filter_and_sort_cues(cues));
    }

    pub fn filter_and_sort(&mut self) {
        let was_complete = self
            .track(self.primary_sid)
            .is_some_and(|track| track.loaded_complete);
        let entries = filter_and_sort_cues(self.primary_entries());
        self.set_entries(entries);
        if was_complete {
            if let Some(track) = self.track_mut(self.primary_sid) {
                track.loaded_complete = true;
            }
        }
    }

    pub fn filter_and_sort_cues<C: AsRef<[SubtitleEntry]>>(cues: C) -> Vec<SubtitleEntry> {
        filter_and_sort_cues(cues)
    }

    /// Set an already parsed external or embedded track's complete cue list.
    pub fn set_track_cues(&mut self, sid: i64, cues: Vec<SubtitleEntry>) -> bool {
        let Some(track) = self.track_mut(sid) else {
            return false;
        };
        track.set_entries(filter_and_sort_cues(cues));
        track.selected_indices.clear();
        track.selection_anchor = None;
        track.active_indices.clear();
        track.is_memory_track = false;
        track.loaded_complete = true;
        true
    }

    /// Complete an external parse only if the SID still refers to that path.
    /// This prevents a slow parse from replacing cues after mpv changes a
    /// track's external filename.
    pub fn set_external_cues(
        &mut self,
        sid: i64,
        path: &str,
        cues: Vec<SubtitleEntry>,
    ) -> bool {
        let Some(track) = self.track_mut(sid) else {
            return false;
        };
        if !track.external || track.external_path.as_deref() != Some(path) {
            return false;
        }
        track.set_entries(filter_and_sort_cues(cues));
        track.selected_indices.clear();
        track.selection_anchor = None;
        track.active_indices.clear();
        track.is_memory_track = false;
        track.loaded_complete = true;
        true
    }

    /// Replace the dynamically reported `sub-lines` list for a memory track.
    /// Empty updates clear cues only for a SID previously marked as a memory
    /// track, matching mpv's transient subtitle-line behavior.
    ///
    /// Tracks that are already completely populated (external subtitle files,
    /// upfront-extracted embedded MKV tracks, or ASR tracks) are never
    /// overwritten by transient `sub-lines` updates; only their active row
    /// indices are refreshed.
    pub fn update_memory_cues(
        &mut self,
        sid: i64,
        cues: &[MemorySubtitleCue],
        raw_position: f64,
    ) -> bool {
        let Some(index) = self.track_index(sid) else {
            return false;
        };
        if self.tracks[index].is_asr_track
            || self.tracks[index].external
            || self.tracks[index].loaded_complete
        {
            self.tracks[index].update_active_indices(raw_position);
            return false;
        }
        let (should_update, mark_memory) = model::should_update_memory_track(
            sid,
            true,
            !cues.is_empty(),
            self.tracks[index].is_memory_track,
        );
        if !should_update {
            return false;
        }
        if mark_memory {
            self.tracks[index].is_memory_track = true;
        }

        let candidates: Vec<_> = cues
            .iter()
            .enumerate()
            .map(|(original_index, cue)| model::MementoSubtitleCandidateCueC {
                original_index,
                id_valid: true,
                cue_id: cue.id,
                start_valid: true,
                start: cue.start,
                end_valid: true,
                end: cue.end,
            })
            .collect();
        let sorted_indices = model::filter_and_sort_cues(&candidates);
        let track = &mut self.tracks[index];
        let entries = sorted_indices
            .into_iter()
            .filter_map(|cue_index| {
                let cue = &cues[cue_index];
                let norm = normalize_subtitle_text(&String::from_utf16_lossy(&cue.text));
                if norm.is_empty() {
                    return None;
                }
                Some(SubtitleEntry {
                    text: norm.encode_utf16().collect(),
                    start: cue.start,
                    end: cue.end,
                })
            })
            .collect();
        track.replace_memory_entries(entries);
        track.update_active_indices(raw_position);
        true
    }

    /// Add or select mpv's current transient subtitle cue for one SID.
    ///
    /// When a track is external, ASR-owned, or upfront-loaded (`loaded_complete`),
    /// transient `sub-text` values are never inserted into `track.entries` (so
    /// simultaneous overlapping cues or combined `sub-text` strings do not create
    /// duplicate rows); only `active_indices` is updated from `raw_position`.
    pub fn update_active_cue(
        &mut self,
        sid: i64,
        cue: Option<&SubtitleEntry>,
        raw_position: f64,
    ) -> bool {
        let Some(index) = self.track_index(sid) else {
            return false;
        };
        let Some(cue) = cue else {
            self.tracks[index].active_indices.clear();
            return false;
        };
        let track = &mut self.tracks[index];
        if track.is_asr_track || track.external || track.loaded_complete {
            if cue.text.is_empty() {
                track.active_indices.clear();
            } else {
                track.update_active_indices(raw_position);
            }
            return true;
        }
        let action = model::plan_active_cue(true, true, !cue.text.is_empty(), raw_position);
        if action.action == ActiveCueActionKind::AddAndSelect {
            let norm_text: Vec<u16> =
                normalize_subtitle_text(&String::from_utf16_lossy(&cue.text))
                    .encode_utf16()
                    .collect();
            if norm_text.is_empty() {
                track.active_indices.clear();
                return true;
            }
            let normalized_cue = SubtitleEntry {
                text: norm_text,
                start: cue.start,
                end: cue.end,
            };
            let views: Vec<_> = track
                .entries
                .iter()
                .map(model::SubtitleCueView::from)
                .collect();
            let add_plan = model::plan_add_subtitle(
                &views,
                false,
                &normalized_cue.text,
                normalized_cue.start,
                normalized_cue.end,
            );
            if add_plan.action == model::AddAction::Insert {
                let insert_at = (add_plan.index as usize).min(track.entries.len());
                track.insert_entry_at(insert_at, normalized_cue);
                for index in &mut track.selected_indices {
                    if *index >= insert_at {
                        *index += 1;
                    }
                }
                if let Some(anchor) = &mut track.selection_anchor {
                    if *anchor >= insert_at {
                        *anchor += 1;
                    }
                }
            }
            track.is_memory_track = true;
            track.update_active_indices(action.raw_position);
        } else if action.action == ActiveCueActionKind::ClearSelection {
            track.active_indices.clear();
        }
        true
    }

    /// Update both subtitle lists using mpv's raw subtitle clocks. Boundaries
    /// use Memento's TIME_DELTA and strict interval semantics; overlapping cues
    /// remain active together rather than selecting only the first one.
    ///
    /// Reuses each track's `active_indices` buffer in place for zero steady-state
    /// heap allocations on 60Hz ticks.
    pub fn update_positions(
        &mut self,
        primary_raw_time: f64,
        secondary_raw_time: f64,
    ) -> (Option<usize>, Option<usize>) {
        self.update_sid_position(self.primary_sid, primary_raw_time);
        if self.secondary_sid != self.primary_sid {
            self.update_sid_position(self.secondary_sid, secondary_raw_time);
        }
        (
            self.active_indices(self.primary_sid).first().copied(),
            self.active_indices(self.secondary_sid).first().copied(),
        )
    }

    pub fn update_position(&mut self, position_sec: f64) -> Option<usize> {
        self.update_sid_position(self.primary_sid, position_sec);
        self.active_indices(self.primary_sid).first().copied()
    }

    pub fn primary_entries(&self) -> &[SubtitleEntry] {
        self.entries_for_sid(self.primary_sid)
    }

    pub fn secondary_entries(&self) -> &[SubtitleEntry] {
        self.entries_for_sid(self.secondary_sid)
    }

    pub fn active_indices(&self, sid: i64) -> &[usize] {
        self.track(sid)
            .map(SubtitleTrackModel::active_indices)
            .unwrap_or(&[])
    }

    pub fn primary_active_text(&self) -> String {
        self.active_text(self.primary_sid)
    }

    pub fn secondary_active_text(&self) -> String {
        self.active_text(self.secondary_sid)
    }

    pub fn entries(&self) -> &[SubtitleEntry] {
        self.primary_entries()
    }

    pub fn cues(&self) -> &[SubtitleEntry] {
        self.primary_entries()
    }

    pub fn len(&self) -> usize {
        self.primary_entries().len()
    }

    pub fn is_empty(&self) -> bool {
        self.primary_entries().is_empty()
    }

    pub fn get(&self, index: usize) -> Option<&SubtitleEntry> {
        self.primary_entries().get(index)
    }

    pub fn clear(&mut self) {
        self.tracks.clear();
        self.asr_cues.clear();
        self.asr_mpv_sid = None;
        self.primary_sid = 0;
        self.secondary_sid = 0;
    }

    pub fn toggle_select(&mut self, index: usize) {
        self.set_selected(index, !self.is_selected(index));
    }

    pub fn set_selected(&mut self, index: usize, selected: bool) {
        let Some(track) = self.track_mut(self.primary_sid) else {
            return;
        };
        if index >= track.entries.len() {
            return;
        }
        track.selection_anchor = Some(index);
        if let Some(pos) = track.selected_indices.iter().position(|&i| i == index) {
            if !selected {
                track.selected_indices.remove(pos);
            }
        } else if selected {
            track.selected_indices.push(index);
            track.selected_indices.sort_unstable();
        }
    }

    /// Ordinary clicks toggle a mark; Shift-click adds the inclusive range
    /// from the last clicked cue without losing previously marked subtitles.
    pub fn select_row(&mut self, index: usize, extend_range: bool) {
        if !extend_range {
            self.toggle_select(index);
            return;
        }
        let Some(track) = self.track_mut(self.primary_sid) else {
            return;
        };
        if index >= track.entries.len() {
            return;
        }
        let anchor = *track.selection_anchor.get_or_insert(index);
        track
            .selected_indices
            .extend(anchor.min(index)..=anchor.max(index));
        track.selected_indices.sort_unstable();
        track.selected_indices.dedup();
    }

    /// Rebuild the dragged range from the marks present before the gesture,
    /// so reversing direction shrinks it without discarding earlier marks.
    pub fn select_range(&mut self, first: usize, last: usize, previous: &[usize]) {
        let Some(track) = self.track_mut(self.primary_sid) else {
            return;
        };
        if first >= track.entries.len() || last >= track.entries.len() {
            return;
        }
        track.selection_anchor = Some(first);
        track.selected_indices = previous
            .iter()
            .copied()
            .filter(|index| *index < track.entries.len())
            .collect();
        track
            .selected_indices
            .extend(first.min(last)..=first.max(last));
        track.selected_indices.sort_unstable();
        track.selected_indices.dedup();
    }

    pub fn select_all(&mut self) {
        if let Some(track) = self.track_mut(self.primary_sid) {
            track.selected_indices = (0..track.entries.len()).collect();
            track.selection_anchor = (!track.entries.is_empty()).then_some(0);
        }
    }

    pub fn clear_selection(&mut self) {
        if let Some(track) = self.track_mut(self.primary_sid) {
            track.selected_indices.clear();
            track.selection_anchor = None;
        }
    }

    pub fn is_selected(&self, index: usize) -> bool {
        self.track(self.primary_sid)
            .is_some_and(|track| track.selected_indices.binary_search(&index).is_ok())
    }

    pub fn selected_indices(&self) -> &[usize] {
        self.track(self.primary_sid)
            .map(|track| track.selected_indices.as_slice())
            .unwrap_or(&[])
    }

    pub fn selected_cues(&self) -> Vec<&SubtitleEntry> {
        self.selected_indices()
            .iter()
            .filter_map(|&index| self.primary_entries().get(index))
            .collect()
    }

    /// One continuous context clip, including gaps between marked cues.
    pub fn selected_time_range(&self) -> Option<(f64, f64)> {
        self.selected_cues()
            .into_iter()
            .map(|cue| (cue.start, cue.end))
            .reduce(|(start, end), (next_start, next_end)| {
                (start.min(next_start), end.max(next_end))
            })
    }

    pub fn active_cue(&self) -> Option<&SubtitleEntry> {
        self.active_cue_index()
            .and_then(|index| self.primary_entries().get(index))
    }

    pub fn active_cue_index(&self) -> Option<usize> {
        self.active_indices(self.primary_sid).first().copied()
    }

    pub fn active_cue_text(&self) -> Option<String> {
        self.active_cue()
            .map(|entry| String::from_utf16_lossy(&entry.text))
    }

    pub fn cue_text(&self, index: usize) -> Option<String> {
        self.primary_entries()
            .get(index)
            .map(|entry| String::from_utf16_lossy(&entry.text))
    }

    pub fn search(&self, query: &str, ignore_whitespace: bool) -> Vec<usize> {
        let query_utf16: Vec<u16> = query.encode_utf16().collect();
        let candidates: Vec<&[u16]> = self
            .primary_entries()
            .iter()
            .map(|entry| entry.text.as_slice())
            .collect();
        memento_subtitle::model::find_matching_rows(&candidates, &query_utf16, ignore_whitespace)
    }

    fn track_index(&self, sid: i64) -> Option<usize> {
        self.tracks.iter().position(|track| track.id == sid)
    }

    fn track_mut(&mut self, sid: i64) -> Option<&mut SubtitleTrackModel> {
        self.tracks.iter_mut().find(|track| track.id == sid)
    }

    fn entries_for_sid(&self, sid: i64) -> &[SubtitleEntry] {
        self.track(sid)
            .map(SubtitleTrackModel::entries)
            .unwrap_or(&[])
    }

    fn update_sid_position(&mut self, sid: i64, raw_time: f64) {
        if let Some(track) = self.track_mut(sid) {
            track.update_active_indices(raw_time);
        }
    }

    fn active_text(&self, sid: i64) -> String {
        self.active_indices(sid)
            .iter()
            .filter_map(|&index| self.entries_for_sid(sid).get(index))
            .map(|entry| String::from_utf16_lossy(&entry.text))
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join("\n")
    }
}

fn select_active_indices_into(
    entries: &[SubtitleEntry],
    prefix_max_end: &[f64],
    sorted_by_start: bool,
    raw_time: f64,
    out: &mut Vec<usize>,
) {
    out.clear();
    if !raw_time.is_finite() || entries.is_empty() {
        return;
    }
    let position = raw_time + model::TIME_DELTA;
    if sorted_by_start && prefix_max_end.len() == entries.len() {
        let upper = entries.partition_point(|cue| cue.start < position);
        let lower = prefix_max_end[..upper].partition_point(|&max_end| max_end <= position);
        for (offset, cue) in entries[lower..upper].iter().enumerate() {
            if cue.start < position && position < cue.end {
                out.push(lower + offset);
            }
        }
    } else {
        for (index, cue) in entries.iter().enumerate() {
            if cue.start < position && position < cue.end {
                out.push(index);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        create_cue, MemorySubtitleCue, SubtitleController, SubtitleTrackModel, ASR_TRACK_ID,
    };
    use memento_subtitle::model;

    fn linear_active_indices(track: &SubtitleTrackModel, raw_time: f64) -> Vec<usize> {
        if !raw_time.is_finite() {
            return Vec::new();
        }
        let pos = raw_time + model::TIME_DELTA;
        track
            .entries()
            .iter()
            .enumerate()
            .filter_map(|(i, cue)| (cue.start < pos && pos < cue.end).then_some(i))
            .collect()
    }

    fn mem_cue(id: i64, text: &str, start: f64, end: f64) -> MemorySubtitleCue {
        MemorySubtitleCue {
            id,
            text: text.encode_utf16().collect(),
            start,
            end,
        }
    }

    #[test]
    fn select_active_indices_matches_linear_scan_and_reuses_buffer() {
        let mut ctrl = SubtitleController::new();
        ctrl.set_entries(vec![
            create_cue("Long banner", 1.0, 20.0),
            create_cue("Early", 2.0, 4.0),
            create_cue("Overlap A", 5.0, 9.0),
            create_cue("Overlap B", 6.0, 8.0),
            create_cue("Late", 12.0, 15.0),
        ]);

        for &t in &[
            f64::NAN,
            f64::INFINITY,
            0.0,
            1.0,
            2.5,
            4.0,
            5.5,
            7.0,
            8.5,
            10.0,
            12.0,
            14.9,
            15.0,
            19.5,
            20.0,
            25.0,
        ] {
            ctrl.update_position(t);
            let track = ctrl.track(1).unwrap();
            assert_eq!(
                track.active_indices(),
                linear_active_indices(track, t).as_slice(),
                "mismatch at t={t}"
            );
        }

        // Steady-state ticks reuse the existing `active_indices` allocation.
        ctrl.update_position(7.0);
        let ptr_before = ctrl.track(1).unwrap().active_indices.as_ptr();
        let cap_before = ctrl.track(1).unwrap().active_indices.capacity();
        ctrl.update_positions(2.5, 2.5);
        ctrl.update_positions(13.0, 13.0);
        ctrl.update_positions(7.0, 7.0);
        let track = ctrl.track(1).unwrap();
        assert_eq!(track.active_indices.as_ptr(), ptr_before);
        assert_eq!(track.active_indices.capacity(), cap_before);

        // Unsorted raw `set_entries` still falls back accurately.
        ctrl.set_entries(vec![
            create_cue("Second", 10.0, 12.0),
            create_cue("First", 2.0, 5.0),
        ]);
        ctrl.update_position(3.0);
        assert_eq!(ctrl.active_indices(1), &[1]);
        ctrl.update_position(11.0);
        assert_eq!(ctrl.active_indices(1), &[0]);
    }

    #[test]
    fn set_entries_preserves_asr_primary_sid_and_defaults_zero_to_one() {
        let mut ctrl = SubtitleController::new();
        assert_eq!(ctrl.primary_sid(), 0);
        ctrl.set_entries(vec![create_cue("A", 1.0, 2.0)]);
        assert_eq!(ctrl.primary_sid(), 1);

        ctrl.set_primary_sid(2);
        ctrl.set_entries(vec![create_cue("B", 3.0, 4.0)]);
        assert_eq!(ctrl.primary_sid(), 2);

        ctrl.start_asr_track();
        assert_eq!(ctrl.primary_sid(), ASR_TRACK_ID);
        ctrl.set_entries(vec![
            create_cue("ASR 2", 5.0, 7.0),
            create_cue("ASR 1", 1.0, 3.0),
        ]);
        assert_eq!(ctrl.primary_sid(), ASR_TRACK_ID);
        assert!(ctrl.track(ASR_TRACK_ID).unwrap().is_asr_track());
        assert_eq!(ctrl.primary_entries().len(), 2);

        ctrl.filter_and_sort();
        assert_eq!(ctrl.primary_sid(), ASR_TRACK_ID);
        assert_eq!(ctrl.cue_text(0).as_deref(), Some("ASR 1"));
        assert_eq!(ctrl.cue_text(1).as_deref(), Some("ASR 2"));
    }

    #[test]
    fn incremental_asr_cues_maintain_order_prefix_max_and_selection() {
        let mut ctrl = SubtitleController::new();
        ctrl.start_asr_track();

        // Out-of-order inserts + equal start tie-breaking.
        assert!(ctrl.add_asr_cue(mem_cue(2, "Mid A", 5.0, 10.0), 6.0));
        assert!(ctrl.add_asr_cue(mem_cue(1, "Early", 1.0, 3.0), 6.0));
        assert!(ctrl.add_asr_cue(mem_cue(3, "Mid B", 5.0, 7.0), 6.0));
        assert!(ctrl.add_asr_cue(mem_cue(4, "Late", 12.0, 15.0), 6.0));

        assert_eq!(ctrl.cue_text(0).as_deref(), Some("Early"));
        assert_eq!(ctrl.cue_text(1).as_deref(), Some("Mid A"));
        assert_eq!(ctrl.cue_text(2).as_deref(), Some("Mid B"));
        assert_eq!(ctrl.cue_text(3).as_deref(), Some("Late"));
        assert_eq!(
            ctrl.track(ASR_TRACK_ID).unwrap().prefix_max_end,
            vec![3.0, 10.0, 10.0, 15.0]
        );
        assert_eq!(ctrl.active_indices(ASR_TRACK_ID), &[1, 2]);

        // Select Mid A (index 1, anchor) and Late (index 3).
        ctrl.set_selected(3, true);
        ctrl.set_selected(1, true);
        assert_eq!(ctrl.selected_indices(), &[1, 3]);

        // Update Mid A in place (shorten end from 10.0 -> 6.5): prefix_max_end updates to [3.0, 6.5, 7.0, 15.0].
        assert!(ctrl.add_asr_cue(mem_cue(2, "Mid A revised", 5.0, 6.5), 6.8));
        assert_eq!(ctrl.cue_text(1).as_deref(), Some("Mid A revised"));
        assert_eq!(
            ctrl.track(ASR_TRACK_ID).unwrap().prefix_max_end,
            vec![3.0, 6.5, 7.0, 15.0]
        );
        assert_eq!(ctrl.active_indices(ASR_TRACK_ID), &[2]);
        assert_eq!(ctrl.selected_indices(), &[1, 3]);

        // Move Mid A (id=2) past Late -> becomes index 3; selection & anchor follow id=2!
        assert!(ctrl.add_asr_cue(mem_cue(2, "Mid A moved", 20.0, 22.0), 21.0));
        assert_eq!(ctrl.cue_text(0).as_deref(), Some("Early"));
        assert_eq!(ctrl.cue_text(1).as_deref(), Some("Mid B"));
        assert_eq!(ctrl.cue_text(2).as_deref(), Some("Late"));
        assert_eq!(ctrl.cue_text(3).as_deref(), Some("Mid A moved"));
        assert_eq!(
            ctrl.track(ASR_TRACK_ID).unwrap().prefix_max_end,
            vec![3.0, 7.0, 15.0, 22.0]
        );
        assert_eq!(ctrl.selected_indices(), &[2, 3]);
        assert_eq!(ctrl.active_indices(ASR_TRACK_ID), &[3]);

        // Remove Mid B (id=3, index 1): selection shifts to [1, 2], prefix_max_end updates.
        assert!(ctrl.remove_asr_cue(3, 13.0));
        assert_eq!(
            ctrl.track(ASR_TRACK_ID).unwrap().prefix_max_end,
            vec![3.0, 15.0, 22.0]
        );
        assert_eq!(ctrl.selected_indices(), &[1, 2]);
        assert_eq!(ctrl.active_indices(ASR_TRACK_ID), &[1]);

        // Removing missing id returns false without disturbing state.
        assert!(!ctrl.remove_asr_cue(999, 13.0));
    }
}

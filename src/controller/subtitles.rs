//! Subtitle controller connecting memento-subtitle to Slint SubtitlePanel.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
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
    valid.sort_by(|a, b| {
        a.start.partial_cmp(&b.start).unwrap_or(std::cmp::Ordering::Equal)
    });
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

    fn replace_memory_entries(&mut self, entries: Vec<SubtitleEntry>) {
        let selected: Vec<_> = self.selected_indices.iter()
            .filter_map(|&index| self.entries.get(index)).collect();
        let anchor = self.selection_anchor.and_then(|index| self.entries.get(index));
        self.selected_indices = entries.iter().enumerate()
            .filter_map(|(index, cue)| selected.contains(&cue).then_some(index)).collect();
        self.selection_anchor = anchor.and_then(|cue| entries.iter().position(|entry| entry == cue));
        self.entries = entries;
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
                        track.entries.clear();
                        track.selected_indices.clear();
                        track.selection_anchor = None;
                        track.active_indices.clear();
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
                        track.entries.clear();
                        track.selected_indices.clear();
                        track.selection_anchor = None;
                        track.active_indices.clear();
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
        self.asr_cues.clear();
        self.rebuild_asr_track(position, &[], None);
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

    pub fn add_asr_cue(&mut self, cue: MemorySubtitleCue, position: f64) -> bool {
        if !self.has_asr_track() {
            return false;
        }
        let (selected_ids, anchor_id) = self.asr_selection();
        self.asr_cues.retain(|existing| existing.id != cue.id);
        self.asr_cues.push(cue);
        self.rebuild_asr_track(position, &selected_ids, anchor_id);
        true
    }

    pub fn remove_asr_cue(&mut self, cue_id: i64, position: f64) -> bool {
        let (selected_ids, anchor_id) = self.asr_selection();
        let before = self.asr_cues.len();
        self.asr_cues.retain(|cue| cue.id != cue_id);
        if self.asr_cues.len() == before {
            return false;
        }
        self.rebuild_asr_track(position, &selected_ids, anchor_id);
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
            track.entries = entries;
            track.is_memory_track = true;
            track.selected_indices = selected_indices;
            track.selection_anchor = anchor;
            track.active_indices = select_active_indices(&track.entries, position);
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
    pub fn set_entries(&mut self, entries: Vec<SubtitleEntry>) {
        let sid = self.primary_sid.max(1);
        self.primary_sid = sid;
        if self.track(sid).is_none() {
            self.tracks.push(SubtitleTrackModel::new(sid));
        }
        if let Some(track) = self.track_mut(sid) {
            track.entries = entries;
            track.selected_indices.clear();
            track.selection_anchor = None;
            track.active_indices.clear();
            track.is_memory_track = false;
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
        track.entries = filter_and_sort_cues(cues);
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
        track.entries = filter_and_sort_cues(cues);
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
            self.tracks[index].active_indices =
                select_active_indices(&self.tracks[index].entries, raw_position);
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
        track.active_indices = select_active_indices(&track.entries, raw_position);
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
                track.active_indices = select_active_indices(&track.entries, raw_position);
            }
            return true;
        }
        let action = model::plan_active_cue(
            true,
            true,
            !cue.text.is_empty(),
            raw_position,
        );
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
            let views: Vec<_> = track.entries.iter().map(model::SubtitleCueView::from).collect();
            let add_plan = model::plan_add_subtitle(
                &views,
                false,
                &normalized_cue.text,
                normalized_cue.start,
                normalized_cue.end,
            );
            if add_plan.action == model::AddAction::Insert {
                let insert_at = (add_plan.index as usize).min(track.entries.len());
                track.entries.insert(insert_at, normalized_cue);
                for index in &mut track.selected_indices {
                    if *index >= insert_at { *index += 1; }
                }
                if let Some(anchor) = &mut track.selection_anchor {
                    if *anchor >= insert_at { *anchor += 1; }
                }
            }
            track.is_memory_track = true;
            track.active_indices = select_active_indices(&track.entries, action.raw_position);
        } else if action.action == ActiveCueActionKind::ClearSelection {
            track.active_indices.clear();
        }
        true
    }

    /// Update both subtitle lists using mpv's raw subtitle clocks. Boundaries
    /// use Memento's TIME_DELTA and strict interval semantics; overlapping cues
    /// remain active together rather than selecting only the first one.
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
        if index >= track.entries.len() { return; }
        track.selection_anchor = Some(index);
        if let Some(pos) = track.selected_indices.iter().position(|&i| i == index) {
            if !selected { track.selected_indices.remove(pos); }
        } else if selected {
            track.selected_indices.push(index);
            track.selected_indices.sort_unstable();
        }
    }

    /// Ordinary clicks toggle a mark; Shift-click adds the inclusive range
    /// from the last clicked cue without losing previously marked subtitles.
    pub fn select_row(&mut self, index: usize, extend_range: bool) {
        if !extend_range { self.toggle_select(index); return; }
        let Some(track) = self.track_mut(self.primary_sid) else { return; };
        if index >= track.entries.len() { return; }
        let anchor = *track.selection_anchor.get_or_insert(index);
        track.selected_indices.extend(anchor.min(index)..=anchor.max(index));
        track.selected_indices.sort_unstable();
        track.selected_indices.dedup();
    }

    /// Rebuild the dragged range from the marks present before the gesture,
    /// so reversing direction shrinks it without discarding earlier marks.
    pub fn select_range(&mut self, first: usize, last: usize, previous: &[usize]) {
        let Some(track) = self.track_mut(self.primary_sid) else { return; };
        if first >= track.entries.len() || last >= track.entries.len() { return; }
        track.selection_anchor = Some(first);
        track.selected_indices = previous.iter().copied()
            .filter(|index| *index < track.entries.len()).collect();
        track.selected_indices.extend(first.min(last)..=first.max(last));
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
            .is_some_and(|track| track.selected_indices.contains(&index))
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
        self.selected_cues().into_iter().map(|cue| (cue.start, cue.end))
            .reduce(|(start, end), (next_start, next_end)| (start.min(next_start), end.max(next_end)))
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
            track.active_indices = select_active_indices(&track.entries, raw_time);
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

fn select_active_indices(entries: &[SubtitleEntry], raw_time: f64) -> Vec<usize> {
    if !raw_time.is_finite() {
        return Vec::new();
    }
    let position = raw_time + model::TIME_DELTA;
    entries
        .iter()
        .enumerate()
        .filter_map(|(index, cue)| {
            (cue.start < position && position < cue.end).then_some(index)
        })
        .collect()
}

const FURIGANA_CACHE_CAPACITY: usize = 2048;

struct FuriganaCache {
    entries: HashMap<String, Vec<(String, String)>>,
    order: VecDeque<String>,
}

impl FuriganaCache {
    fn new() -> Self {
        Self {
            entries: HashMap::with_capacity(256),
            order: VecDeque::with_capacity(256),
        }
    }

    fn get(&self, text: &str) -> Option<Vec<(String, String)>> {
        self.entries.get(text).cloned()
    }

    fn contains(&self, text: &str) -> bool {
        self.entries.contains_key(text)
    }

    fn insert(&mut self, text: String, parts: Vec<(String, String)>) {
        if self.entries.contains_key(&text) {
            self.entries.insert(text, parts);
            return;
        }
        while self.entries.len() >= FURIGANA_CACHE_CAPACITY {
            if let Some(oldest) = self.order.pop_front() {
                self.entries.remove(&oldest);
            } else {
                break;
            }
        }
        self.order.push_back(text.clone());
        self.entries.insert(text, parts);
    }
}

static FURIGANA_CACHE: OnceLock<Mutex<FuriganaCache>> = OnceLock::new();

fn furigana_cache() -> &'static Mutex<FuriganaCache> {
    FURIGANA_CACHE.get_or_init(|| Mutex::new(FuriganaCache::new()))
}

/// Pre-tokenize and cache furigana for a subtitle track's lines on a background worker (#345).
pub fn prewarm_subtitle_furigana(lines: Vec<String>) {
    let uncached: Vec<String> = {
        let Ok(guard) = furigana_cache().lock() else {
            return;
        };
        lines
            .into_iter()
            .filter(|line| !line.is_empty() && !guard.contains(line))
            .collect()
    };
    if uncached.is_empty() {
        return;
    }
    std::thread::spawn(move || {
        for line in uncached {
            let _ = subtitle_furigana_parts(&line);
        }
    });
}

/// Tokenize the current subtitle with MeCab and attach approximate readings.
/// Run this on a worker: starting MeCab must never delay video rendering.
pub fn subtitle_furigana_parts(text: &str) -> Result<Vec<(String, String)>, String> {
    if text.is_empty() {
        return Ok(Vec::new());
    }
    if let Ok(guard) = furigana_cache().lock() {
        if let Some(cached) = guard.get(text) {
            return Ok(cached);
        }
    }

    // Substitute non-BMP Joyo variant `𠮟` (U+20B9F) with BMP `叱` (U+53F1) for the
    // MeCab query so dictionaries that only index `叱る` still emit its reading,
    // while `parse_mecab_furigana` restores the exact original `text` slice.
    let query_text;
    let query_ref = if text.contains('𠮟') {
        query_text = text.replace('𠮟', "叱");
        query_text.as_str()
    } else {
        text
    };

    let output = memento_util::mecab::parse_mecab(query_ref)?;
    let parts = parse_mecab_furigana(text, &output)?;

    if let Ok(mut guard) = furigana_cache().lock() {
        guard.insert(text.to_owned(), parts.clone());
    }
    Ok(parts)
}

fn find_mecab_surface_in_remaining(remaining: &str, raw_surface: &str) -> Option<(usize, usize)> {
    if !raw_surface.contains('叱') || !remaining.contains('𠮟') {
        return remaining
            .find(raw_surface)
            .map(|gap| (gap, raw_surface.len()));
    }
    let raw_chars: Vec<char> = raw_surface.chars().collect();
    for (byte_start, _) in remaining.char_indices() {
        let mut tail_chars = remaining[byte_start..].chars();
        let mut matched_bytes = 0;
        let mut ok = true;
        for &rc in &raw_chars {
            match tail_chars.next() {
                Some(tc) if tc == rc || (rc == '叱' && tc == '𠮟') => {
                    matched_bytes += tc.len_utf8();
                }
                _ => {
                    ok = false;
                    break;
                }
            }
        }
        if ok {
            return Some((byte_start, matched_bytes));
        }
    }
    None
}

fn apply_kinship_context_override(
    text: &str,
    offset: usize,
    surface: &str,
    reading: &mut String,
) {
    let prefix = &text[..offset];
    let suffix = &text[offset + surface.len()..];
    let has_honorific_prefix = prefix.ends_with('お') || prefix.ends_with('御');
    let has_honorific_suffix = suffix.starts_with("さん")
        || suffix.starts_with("さま")
        || suffix.starts_with("様")
        || suffix.starts_with("ちゃん");
    if has_honorific_prefix && has_honorific_suffix {
        match (surface, reading.as_str()) {
            ("兄", "あに") => *reading = "にい".to_string(),
            ("父", "ちち") => *reading = "とう".to_string(),
            ("母", "はは") => *reading = "かあ".to_string(),
            ("姉", "あね") => *reading = "ねえ".to_string(),
            _ => {}
        }
    }
}

fn parse_mecab_furigana(text: &str, output: &str) -> Result<Vec<(String, String)>, String> {
    let mut parts = Vec::new();
    let mut offset = 0;
    for line in output.lines() {
        if line == "EOS" || line.is_empty() {
            continue;
        }
        let Some((raw_surface, features)) = line.split_once('\t') else {
            continue;
        };
        if raw_surface.is_empty() {
            continue;
        }
        let remaining = &text[offset..];
        let Some((gap, matched_len)) = find_mecab_surface_in_remaining(remaining, raw_surface)
        else {
            return Err("MeCab token text did not match the subtitle".into());
        };
        if gap > 0 {
            let skipped = &remaining[..gap];
            for token in segment_subtitle_text(skipped) {
                parts.push((token, String::new()));
            }
            offset += gap;
        }
        let surface = &text[offset..offset + matched_len];
        if memento_util::furiganautils::contains_kanji(surface) {
            let fields = memento_util::furiganaparser::split_csv_row(features);
            let raw_reading =
                memento_util::furiganaparser::extract_surface_reading_from_fields(surface, &fields);
            let mut reading = if !raw_reading.is_empty() && raw_reading != "*" {
                let units: Vec<u16> = raw_reading.encode_utf16().collect();
                String::from_utf16_lossy(
                    &memento_util::furiganaparser::katakana_to_hiragana(&units),
                )
            } else {
                String::new()
            };
            apply_kinship_context_override(text, offset, surface, &mut reading);
            for span in memento_util::furiganautils::distribute_furigana(surface, &reading) {
                parts.push(span);
            }
        } else {
            parts.push((surface.to_owned(), String::new()));
        }
        offset += matched_len;
    }
    if offset < text.len() {
        for token in segment_subtitle_text(&text[offset..]) {
            parts.push((token, String::new()));
        }
    }
    if parts
        .iter()
        .map(|(surface, _)| surface.as_str())
        .collect::<String>()
        != text
    {
        return Err("MeCab did not preserve the subtitle text".into());
    }
    Ok(parts)
}

/// Segments a subtitle line into interactive word/token units for Yomitan lookup and hover.
/// Handles Japanese text (Kanji runs, Okurigana/suffixes, particles, whitespace) as well as
/// space-separated languages.
pub fn segment_subtitle_text(text: &str) -> Vec<String> {
    if text.is_empty() {
        return Vec::new();
    }

    let mut tokens = Vec::new();
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;

    let is_kanji = memento_util::furiganautils::is_kanji_char;
    let is_hiragana = |c: char| matches!(c, '\u{3040}'..='\u{309F}');
    let is_katakana = |c: char| matches!(c, '\u{30A0}'..='\u{30FF}');

    while i < chars.len() {
        // Whitespace and punctuation remain separate non-word tokens so a
        // caller can preserve the subtitle's exact visible spacing.
        if chars[i].is_whitespace() || chars[i] == '　' {
            tokens.push(chars[i].to_string());
            i += 1;
            continue;
        }

        if matches!(chars[i], '。' | '、' | '！' | '？' | '…' | '・' | ',' | '.' | '!' | '?' | ';' | ':' | '「' | '」' | '（' | '）') {
            tokens.push(chars[i].to_string());
            i += 1;
            continue;
        }

        // ASCII words, kana runs and kanji compounds are segmented by script;
        // no screenshot-specific vocabulary is embedded in the tokenizer.
        if chars[i].is_ascii_alphanumeric() {
            let mut j = i;
            while j < chars.len() && (chars[j].is_ascii_alphanumeric() || chars[j] == '\'') {
                j += 1;
            }
            tokens.push(chars[i..j].iter().collect());
            i = j;
            continue;
        }

        if is_katakana(chars[i]) {
            let mut j = i;
            while j < chars.len() && (is_katakana(chars[j]) || chars[j] == 'ー') {
                j += 1;
            }
            tokens.push(chars[i..j].iter().collect());
            i = j;
            continue;
        }

        if is_kanji(chars[i]) || (is_hiragana(chars[i]) && matches!(chars[i], 'お' | 'ご')
            && chars.get(i + 1).is_some_and(|c| is_kanji(*c)))
        {
            let mut j = i;
            if is_hiragana(chars[j]) {
                j += 1;
            }
            while j < chars.len()
                && (is_kanji(chars[j])
                    || (j > i && matches!(chars[j], '\u{30F5}' | '\u{30F6}')))
            {
                j += 1;
            }
            if j < chars.len() && is_hiragana(chars[j])
                && matches!(chars[j], 'な' | 'だ' | 'る' | 'い' | 'た')
            {
                j += 1;
            }
            tokens.push(chars[i..j].iter().collect());
            i = j;
            continue;
        }

        if is_hiragana(chars[i]) {
            if i + 1 < chars.len() && is_hiragana(chars[i + 1]) {
                let two_char: String = chars[i..i + 2].iter().collect();
                if matches!(two_char.as_str(), "たち" | "から" | "まで" | "より" | "など" | "けど" | "ので" | "のに" | "である") {
                    tokens.push(two_char);
                    i += 2;
                    continue;
                }
            }
            tokens.push(chars[i].to_string());
            i += 1;
            continue;
        }

        tokens.push(chars[i].to_string());
        i += 1;
    }

    tokens
}

#[cfg(test)]
mod furigana_tests {
    use super::{
        parse_mecab_furigana, prewarm_subtitle_furigana, segment_subtitle_text,
        subtitle_furigana_parts,
    };
    use std::time::Instant;

    #[test]
    fn mecab_readings_preserve_spacing_and_inflected_surface() {
        let text = "昨日 は行った";
        let output = "昨日\t名詞,普通名詞,副詞可能,,,,キノウ,昨日,昨日,キノー\n\
は\t助詞,係助詞,,,,,ハ,は,は,ワ\n\
行っ\t動詞,非自立可能,,,五段-カ行,連用形-促音便,イク,行く,行っ,イッ\n\
た\t助動詞,,,,助動詞-タ,連体形-一般,タ,た,た,タ\nEOS\n";
        let parts = parse_mecab_furigana(text, output).unwrap();
        assert_eq!(
            parts.iter().map(|(surface, _)| surface.as_str()).collect::<String>(),
            text
        );
        assert_eq!(parts.iter().find(|(surface, _)| surface == "昨日").unwrap().1, "きのう");
        assert_eq!(parts.iter().find(|(surface, _)| surface == "行").unwrap().1, "い");
        assert_eq!(parts.iter().find(|(surface, _)| surface == "っ").unwrap().1, "");
    }

    #[test]
    fn ipadic_9col_and_unidic_17col_26col_29col_readings_and_quoted_csv() {
        // IPADic 9-col: 来ない, 見ない, 王さま, 氷, お兄さん, 食べる, 申し込み
        let ipadic_konai = "来\t動詞,自立,*,*,カ変・クル,未然形,来る,コ,コ\n\
ない\t助動詞,*,*,*,特殊・ナイ,基本形,ない,ナイ,ナイ\nEOS\n";
        assert_eq!(
            parse_mecab_furigana("来ない", ipadic_konai).unwrap(),
            vec![("来".into(), "こ".into()), ("ない".into(), "".into())]
        );

        let ipadic_minai = "見\t動詞,自立,*,*,一段,未然形,見る,ミ,ミ\n\
ない\t助動詞,*,*,*,特殊・ナイ,基本形,ない,ナイ,ナイ\nEOS\n";
        assert_eq!(
            parse_mecab_furigana("見ない", ipadic_minai).unwrap(),
            vec![("見".into(), "み".into()), ("ない".into(), "".into())]
        );

        let ipadic_oniisan = "お兄さん\t名詞,一般,*,*,*,*,お兄さん,オニイサン,オニーサン\nEOS\n";
        assert_eq!(
            parse_mecab_furigana("お兄さん", ipadic_oniisan).unwrap(),
            vec![
                ("お".into(), "".into()),
                ("兄".into(), "にい".into()),
                ("さん".into(), "".into()),
            ]
        );

        // UniDic 17-col: lForm at col 6, pron at col 9 (with phonetic ー)
        let unidic17 = "来\t動詞,非自立可能,*,*,カ行変格,未然形-一般,クル,来る,来,コ,来る,クル,和,*,*,*,*\n\
ない\t助動詞,*,*,*,助動詞-ナイ,連体形-一般,ナイ,ない,ない,ナイ,ない,ナイ,和,*,*,*,*\n\
見\t動詞,非自立可能,*,*,上一段-マ行,未然形-一般,ミル,見る,見,ミ,見る,ミル,和,*,*,*,*\n\
ない\t助動詞,*,*,*,助動詞-ナイ,連体形-一般,ナイ,ない,ない,ナイ,ない,ナイ,和,*,*,*,*\n\
王\t名詞,固有名詞,人名,姓,*,*,オウ,オウ,王,オー,王,オー,固,*,*,*,*\n\
さま\t接尾辞,名詞的,一般,*,*,*,サマ,様,さま,サマ,さま,サマ,和,*,*,*,*\n\
氷\t名詞,普通名詞,一般,*,*,*,コオリ,氷,氷,コーリ,氷,コーリ,和,コ濁,基本形,*,*\n\
申し込み\t動詞,一般,*,*,五段-マ行,連用形-一般,モウシコム,申し込む,申し込み,モーシコミ,申し込む,モーシコム,和,*,*,*,*\nEOS\n";
        assert_eq!(
            parse_mecab_furigana("来ない見ない王さま氷申し込み", unidic17).unwrap(),
            vec![
                ("来".into(), "こ".into()),
                ("ない".into(), "".into()),
                ("見".into(), "み".into()),
                ("ない".into(), "".into()),
                ("王".into(), "おう".into()),
                ("さま".into(), "".into()),
                ("氷".into(), "こおり".into()),
                ("申".into(), "もう".into()),
                ("し".into(), "".into()),
                ("込".into(), "こ".into()),
                ("み".into(), "".into()),
            ]
        );

        // UniDic 26-col with quoted commas in fields
        let unidic26 = "王\t名詞,固有名詞,人名,姓,,,オウ,オウ,王,オー,王,オー,固,\"\",\"\",\"\",\"\",オウ,オウ,オウ,オウ,\"1,0\",\"C1,C2\",\"\",120,43\n\
様\t接尾辞,名詞的,一般,,,,サマ,様,様,サマ,様,サマ,和,\"\",\"\",\"\",\"\",サマ,サマ,サマ,サマ,\"\",\"C3\",\"\",395,14\nEOS\n";
        assert_eq!(
            parse_mecab_furigana("王様", unidic26).unwrap(),
            vec![("王".into(), "おう".into()), ("様".into(), "さま".into())]
        );
    }

    #[test]
    fn iteration_marks_and_cjk_ext_b_segment_and_parse_properly() {
        assert_eq!(segment_subtitle_text("人々"), vec!["人々"]);
        assert_eq!(segment_subtitle_text("時々"), vec!["時々"]);
        assert_eq!(segment_subtitle_text("様々"), vec!["様々"]);
        assert_eq!(segment_subtitle_text("𠮟る"), vec!["𠮟る"]);
        assert_eq!(segment_subtitle_text("一ヶ月"), vec!["一ヶ月"]);

        let ext_b_fixture = "人々\t名詞,一般,*,*,*,*,人々,ヒトビト,ヒトビト\n\
𠮟る\t動詞,自立,*,*,五段・ラ行,基本形,𠮟る,シカル,シカル\nEOS\n";
        assert_eq!(
            parse_mecab_furigana("人々𠮟る", ext_b_fixture).unwrap(),
            vec![
                ("人々".into(), "ひとびと".into()),
                ("𠮟".into(), "しか".into()),
                ("る".into(), "".into()),
            ]
        );
    }

    #[test]
    fn live_mecab_accuracy_and_sub_100us_warm_latency() {
        assert_eq!(
            subtitle_furigana_parts("来ない").unwrap(),
            vec![("来".into(), "こ".into()), ("ない".into(), "".into())]
        );
        assert_eq!(
            subtitle_furigana_parts("見ない").unwrap(),
            vec![("見".into(), "み".into()), ("ない".into(), "".into())]
        );
        assert_eq!(
            subtitle_furigana_parts("王さま").unwrap(),
            vec![("王".into(), "おう".into()), ("さま".into(), "".into())]
        );
        assert_eq!(
            subtitle_furigana_parts("王様").unwrap(),
            vec![("王".into(), "おう".into()), ("様".into(), "さま".into())]
        );
        assert_eq!(
            subtitle_furigana_parts("氷").unwrap(),
            vec![("氷".into(), "こおり".into())]
        );
        assert_eq!(
            subtitle_furigana_parts("昨日").unwrap(),
            vec![("昨日".into(), "きのう".into())]
        );
        assert_eq!(
            subtitle_furigana_parts("お兄さん").unwrap(),
            vec![
                ("お".into(), "".into()),
                ("兄".into(), "にい".into()),
                ("さん".into(), "".into()),
            ]
        );
        assert_eq!(
            subtitle_furigana_parts("人々").unwrap(),
            vec![("人々".into(), "ひとびと".into())]
        );
        assert_eq!(
            subtitle_furigana_parts("食べる").unwrap(),
            vec![("食".into(), "た".into()), ("べる".into(), "".into())]
        );
        assert_eq!(
            subtitle_furigana_parts("行った").unwrap(),
            vec![
                ("行".into(), "い".into()),
                ("っ".into(), "".into()),
                ("た".into(), "".into()),
            ]
        );
        assert_eq!(
            subtitle_furigana_parts("𠮟る").unwrap(),
            vec![("𠮟".into(), "しか".into()), ("る".into(), "".into())]
        );
        assert_eq!(
            subtitle_furigana_parts("𠮟る叱る").unwrap(),
            vec![
                ("𠮟".into(), "しか".into()),
                ("る".into(), "".into()),
                ("叱".into(), "しか".into()),
                ("る".into(), "".into()),
            ]
        );

        prewarm_subtitle_furigana(vec!["時々".into(), "様々".into()]);
        let start = Instant::now();
        for _ in 0..20 {
            let parts = subtitle_furigana_parts("来ない").unwrap();
            assert_eq!(parts[0].1, "こ");
        }
        let avg_us = start.elapsed().as_micros() as f64 / 20.0;
        assert!(avg_us < 100.0, "warm furigana lookup took {avg_us:.2}us");
    }
}


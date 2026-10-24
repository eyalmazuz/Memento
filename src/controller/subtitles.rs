//! Subtitle controller connecting memento-subtitle to Slint SubtitlePanel.

use std::path::PathBuf;
use std::io::Write;
use std::process::{Command, Stdio};
use memento_subtitle::model::{
    self, ActiveCueActionKind, SubtitleExternalPathInput, SubtitleTrackInput, TrackAction,
};
use memento_subtitle::SubtitleEntry;

/// Validates and stably sorts subtitle cues by start timestamp.
///
/// Discards cues where:
/// - `start < 0.0`
/// - `end <= start`
/// - `!start.is_finite()`
/// - `!end.is_finite()`
pub fn filter_and_sort_cues<C: AsRef<[SubtitleEntry]>>(cues: C) -> Vec<SubtitleEntry> {
    let slice = cues.as_ref();
    let mut valid: Vec<SubtitleEntry> = slice
        .iter()
        .filter(|c| c.start >= 0.0 && c.end > c.start && c.start.is_finite() && c.end.is_finite())
        .cloned()
        .collect();
    valid.sort_by(|a, b| {
        a.start.partial_cmp(&b.start).unwrap_or(std::cmp::Ordering::Equal)
    });
    valid
}

/// Helper to create a SubtitleEntry with text encoded as UTF-16 code units.
pub fn create_cue(text: &str, start: f64, end: f64) -> SubtitleEntry {
    SubtitleEntry {
        text: text.encode_utf16().collect(),
        start,
        end,
    }
}

/// A subtitle track reported by mpv. Positive IDs are mpv SIDs; zero is
/// reserved by this controller for the application's ASR subtitles.
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
    active_indices: Vec<usize>,
    is_memory_track: bool,
}

impl SubtitleTrackModel {
    fn new(id: i64) -> Self {
        Self {
            id,
            external: false,
            external_path: None,
            entries: Vec::new(),
            selected_indices: Vec::new(),
            active_indices: Vec::new(),
            is_memory_track: false,
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
}

/// Local ASR must not share SID 0 with mpv's "no subtitles" selection.
pub const ASR_TRACK_ID: i64 = -1;

/// Tracks subtitle cue lists per mpv SID, including external-file reloads and
/// dynamically supplied in-memory cues. The primary and secondary SIDs remain
/// independent, so their active cues can overlap and be shown simultaneously.
pub struct SubtitleController {
    tracks: Vec<SubtitleTrackModel>,
    asr_cues: Vec<MemorySubtitleCue>,
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
            primary_sid: 0,
            secondary_sid: 0,
        }
    }

    /// Reconcile per-SID models against mpv's subtitle track list. External
    /// files are requested only when their path changes; callers parse each
    /// returned path and pass the cues to [`Self::set_external_cues`].
    pub fn sync_tracks(&mut self, tracks: &[SubtitleTrackInfo]) -> Vec<ExternalSubtitleLoad> {
        let track_paths: Vec<Vec<u16>> = tracks
            .iter()
            .map(|track| track.external_path.encode_utf16().collect())
            .collect();
        let track_inputs: Vec<SubtitleTrackInput<'_>> = tracks
            .iter()
            .zip(&track_paths)
            .map(|(track, path)| {
                SubtitleTrackInput::new(false, track.id, track.external, path.as_slice())
            })
            .collect();
        // The local ASR track is not in mpv's track list and must survive its
        // regular reconciliation, including while another track is selected.
        let model_ids: Vec<i64> = self.tracks.iter().map(|track| track.id).filter(|id| *id > 0).collect();
        let external_path_units: Vec<(i64, Vec<u16>)> = self
            .tracks
            .iter()
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
                        track.active_indices.clear();
                        track.is_memory_track = false;
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
                        track.active_indices.clear();
                        track.is_memory_track = false;
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

    /// Begin a local ASR track, independent of mpv's positive subtitle SIDs.
    pub fn start_asr_track(&mut self) {
        self.asr_cues.clear();
        self.tracks.retain(|track| track.id != ASR_TRACK_ID);
        self.tracks.push(SubtitleTrackModel::new(ASR_TRACK_ID));
        self.primary_sid = ASR_TRACK_ID;
    }

    /// Restore cached cues and select the subtitle at the current media time.
    pub fn resume_asr_track(&mut self, position: f64) -> bool {
        if self.track(ASR_TRACK_ID).is_none() { return false; }
        self.primary_sid = ASR_TRACK_ID;
        self.update_sid_position(ASR_TRACK_ID, position);
        true
    }

    pub fn clear_asr_track(&mut self) {
        if self.primary_sid == ASR_TRACK_ID { self.primary_sid = 0; }
        self.asr_cues.clear();
        self.tracks.retain(|track| track.id != ASR_TRACK_ID);
    }

    pub fn add_asr_cue(&mut self, cue: MemorySubtitleCue, position: f64) -> bool {
        if self.track(ASR_TRACK_ID).is_none() {
            return false;
        }
        let selected_ids = self.selected_asr_ids();
        self.asr_cues.retain(|existing| existing.id != cue.id);
        self.asr_cues.push(cue);
        self.rebuild_asr_track(position, &selected_ids);
        true
    }

    pub fn remove_asr_cue(&mut self, cue_id: i64, position: f64) -> bool {
        let selected_ids = self.selected_asr_ids();
        let before = self.asr_cues.len();
        self.asr_cues.retain(|cue| cue.id != cue_id);
        if self.asr_cues.len() == before {
            return false;
        }
        self.rebuild_asr_track(position, &selected_ids);
        true
    }

    fn selected_asr_ids(&self) -> Vec<i64> {
        self.track(ASR_TRACK_ID).map(|track| track.selected_indices.iter()
            .filter_map(|index| self.asr_cues.get(*index).map(|cue| cue.id))
            .collect()).unwrap_or_default()
    }

    fn rebuild_asr_track(&mut self, position: f64, selected_ids: &[i64]) {
        self.asr_cues.sort_by(|a, b| a.start.total_cmp(&b.start));
        let selected_indices = self.asr_cues.iter().enumerate()
            .filter_map(|(index, cue)| selected_ids.contains(&cue.id).then_some(index))
            .collect();
        let entries: Vec<_> = self.asr_cues.iter().map(|cue| SubtitleEntry {
            text: cue.text.clone(), start: cue.start, end: cue.end,
        }).collect();
        if let Some(track) = self.track_mut(ASR_TRACK_ID) {
            track.entries = entries;
            track.is_memory_track = true;
            track.selected_indices = selected_indices;
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
            track.active_indices.clear();
            track.is_memory_track = false;
        }
    }

    pub fn set_cues(&mut self, cues: Vec<SubtitleEntry>) {
        self.set_entries(cues);
    }

    pub fn load_cues<C: AsRef<[SubtitleEntry]>>(&mut self, cues: C) {
        self.set_entries(filter_and_sort_cues(cues));
    }

    pub fn filter_and_sort(&mut self) {
        let entries = filter_and_sort_cues(self.primary_entries());
        self.set_entries(entries);
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
        track.active_indices.clear();
        track.is_memory_track = false;
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
        track.active_indices.clear();
        track.is_memory_track = false;
        true
    }

    /// Replace the dynamically reported `sub-lines` list for a memory track.
    /// Empty updates clear cues only for a SID previously marked as a memory
    /// track, matching mpv's transient subtitle-line behavior.
    pub fn update_memory_cues(
        &mut self,
        sid: i64,
        cues: &[MemorySubtitleCue],
        raw_position: f64,
    ) -> bool {
        let Some(index) = self.track_index(sid) else {
            return false;
        };
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
        track.entries.clear();
        track.entries.reserve(sorted_indices.len());
        for cue_index in sorted_indices {
            let cue = &cues[cue_index];
            track.entries.push(SubtitleEntry {
                text: cue.text.clone(),
                start: cue.start,
                end: cue.end,
            });
        }
        track.selected_indices.clear();
        track.active_indices = select_active_indices(&track.entries, raw_position);
        true
    }

    /// Add or select mpv's current transient subtitle cue for one SID.
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
            return false;
        };
        let action = model::plan_active_cue(
            true,
            true,
            !cue.text.is_empty(),
            raw_position,
        );
        let track = &mut self.tracks[index];
        if action.action == ActiveCueActionKind::AddAndSelect {
            let views: Vec<_> = track.entries.iter().map(model::SubtitleCueView::from).collect();
            let add_plan = model::plan_add_subtitle(
                &views,
                false,
                &cue.text,
                cue.start,
                cue.end,
            );
            if add_plan.action == model::AddAction::Insert {
                let insert_at = (add_plan.index as usize).min(track.entries.len());
                track.entries.insert(insert_at, cue.clone());
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
        self.primary_sid = 0;
        self.secondary_sid = 0;
    }

    pub fn toggle_select(&mut self, index: usize) {
        let Some(track) = self.track_mut(self.primary_sid) else {
            return;
        };
        if let Some(pos) = track.selected_indices.iter().position(|&i| i == index) {
            track.selected_indices.remove(pos);
        } else if index < track.entries.len() {
            track.selected_indices.push(index);
            track.selected_indices.sort_unstable();
        }
    }

    pub fn select_all(&mut self) {
        if let Some(track) = self.track_mut(self.primary_sid) {
            track.selected_indices = (0..track.entries.len()).collect();
        }
    }

    pub fn clear_selection(&mut self) {
        if let Some(track) = self.track_mut(self.primary_sid) {
            track.selected_indices.clear();
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

/// Tokenize the current subtitle with MeCab and attach approximate readings.
/// Run this on a worker: starting MeCab must never delay video rendering.
pub fn subtitle_furigana_parts(text: &str) -> Result<Vec<(String, String)>, String> {
    if text.is_empty() { return Ok(Vec::new()); }
    let mut child = Command::new("mecab")
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null())
        .spawn().map_err(|error| format!("MeCab is unavailable: {error}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(text.as_bytes()).map_err(|error| format!("MeCab input failed: {error}"))?;
        if !text.ends_with('\n') { stdin.write_all(b"\n").map_err(|error| error.to_string())?; }
    }
    let output = child.wait_with_output().map_err(|error| format!("MeCab failed: {error}"))?;
    if !output.status.success() { return Err("MeCab could not parse the subtitle".into()); }
    parse_mecab_furigana(text, &String::from_utf8_lossy(&output.stdout))
}

fn parse_mecab_furigana(text: &str, output: &str) -> Result<Vec<(String, String)>, String> {
    let mut parts = Vec::new();
    let mut offset = 0;
    for line in output.lines() {
        if line == "EOS" || line.is_empty() { continue; }
        let Some((surface, features)) = line.split_once('\t') else { continue; };
        if surface.is_empty() { continue; }
        let remaining = &text[offset..];
        let Some(gap) = remaining.find(surface) else {
            return Err("MeCab token text did not match the subtitle".into());
        };
        if gap > 0 {
            let skipped = &remaining[..gap];
            for token in segment_subtitle_text(skipped) { parts.push((token, String::new())); }
            offset += gap;
        }
        let fields: Vec<&str> = features.split(',').collect();
        let is_kana = |value: &&str| !value.is_empty() && *value != "*"
            && value.chars().all(|character| matches!(character, '\u{3040}'..='\u{30ff}'));
        let contains_kanji = surface.chars().any(|character| matches!(character, '\u{3400}'..='\u{9fff}' | '\u{f900}'..='\u{faff}'));
        let reading = if contains_kanji {
            // UniDic exposes surface pronunciation at column 9; IPADic uses
            // column 7. Prefer UniDic's lemma reading for plain kanji words.
            let preferred = if surface.chars().any(|c| matches!(c, '\u{3040}'..='\u{30ff}')) {
                [9, 6, 7]
            } else { [6, 7, 9] };
            preferred.into_iter().filter_map(|index| fields.get(index))
                .find(|candidate| is_kana(candidate))
                .map(|reading| {
                    let units = reading.encode_utf16().collect::<Vec<_>>();
                    String::from_utf16_lossy(&memento_util::furiganaparser::katakana_to_hiragana(&units))
                }).unwrap_or_default()
        } else { String::new() };
        parts.push((surface.to_owned(), reading));
        offset += surface.len();
    }
    if offset < text.len() {
        for token in segment_subtitle_text(&text[offset..]) { parts.push((token, String::new())); }
    }
    if parts.iter().map(|(surface, _)| surface.as_str()).collect::<String>() != text {
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

    let is_kanji = |c: char| matches!(c, '\u{4E00}'..='\u{9FAF}' | '\u{3400}'..='\u{4DBF}');
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
            while j < chars.len() && is_kanji(chars[j]) {
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
    use super::parse_mecab_furigana;

    #[test]
    fn mecab_readings_preserve_spacing_and_inflected_surface() {
        let text = "昨日 は行った";
        let output = "昨日\t名詞,普通名詞,副詞可能,,,,キノウ,昨日,昨日,キノー\n\
は\t助詞,係助詞,,,,,ハ,は,は,ワ\n\
行っ\t動詞,非自立可能,,,五段-カ行,連用形-促音便,イク,行く,行っ,イッ\n\
た\t助動詞,,,,助動詞-タ,連体形-一般,タ,た,た,タ\nEOS\n";
        let parts = parse_mecab_furigana(text, output).unwrap();
        assert_eq!(parts.iter().map(|(surface, _)| surface.as_str()).collect::<String>(), text);
        assert_eq!(parts.iter().find(|(surface, _)| surface == "昨日").unwrap().1, "きのう");
        assert_eq!(parts.iter().find(|(surface, _)| surface == "行っ").unwrap().1, "いっ");
    }
}


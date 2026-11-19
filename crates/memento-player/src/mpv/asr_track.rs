//! Non-blocking ASR subtitle track manager for native memory-subtitle and
//! `memento-asr://` stream-callback backends.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet, VecDeque};

use super::asr_stream::{cue_cmp, format_webvtt_sorted_cues};
use super::event::MpvEvent;
use super::instance::MpvInstance;
use super::node::{cue_valid, id_valid};
use super::types::{
    AsrCue, AsrTrackMode, CommandReplyResult, MpvPropertyValue, ASR_STREAM_URI,
};

/// Unified, non-blocking ASR subtitle track manager supporting both Mode 2
/// (`eyalmazuz/mpv` in-memory subtitle commands via `mpv_command_node_async`) and
/// Mode 1 (stock `libmpv` `mpv_stream_cb_add_ro` `"memento-asr://live.vtt"` + `sub-reload`).
pub struct AsrSubtitleTrack {
    mode: AsrTrackMode,
    pub(super) sid: Option<i64>,
    pub(super) pending_create_req: Option<u64>,
    awaiting_stream_sid: bool,
    abandoned_create_reqs: HashSet<u64>,
    pub(super) cues: Vec<AsrCue>,
    // Mode 1 (stream_cb) state
    pub(super) reload_in_flight: Option<u64>,
    pub(super) reload_dirty: bool,
    pub(super) deselected_by_user: bool,
    // Mode 2 (native memory subtitles) async tracking
    pub(super) pending_cues: VecDeque<AsrCue>,
    pub(super) pending_adds: HashMap<u64, i64>,
    pub(super) mpv_cue_ids: HashMap<i64, i64>,
    pub(super) pending_edits: HashMap<i64, AsrCue>,
    pub(super) pending_removals: HashSet<i64>,
}

impl AsrSubtitleTrack {
    /// Create a new track manager selecting Mode 2 when `mpv.supports_memory_subtitles()`
    /// is true, otherwise Mode 1 (`memento-asr://`).
    pub fn new(mpv: &MpvInstance) -> Self {
        Self::with_mode(if mpv.supports_memory_subtitles() {
            AsrTrackMode::NativeMemory
        } else {
            AsrTrackMode::StreamCallback
        })
    }

    pub fn with_mode(mode: AsrTrackMode) -> Self {
        Self {
            mode,
            sid: None,
            pending_create_req: None,
            awaiting_stream_sid: false,
            abandoned_create_reqs: HashSet::new(),
            cues: Vec::new(),
            reload_in_flight: None,
            reload_dirty: false,
            deselected_by_user: false,
            pending_cues: VecDeque::new(),
            pending_adds: HashMap::new(),
            mpv_cue_ids: HashMap::new(),
            pending_edits: HashMap::new(),
            pending_removals: HashSet::new(),
        }
    }

    pub fn mode(&self) -> AsrTrackMode {
        self.mode
    }

    pub fn sid(&self) -> Option<i64> {
        self.sid
    }

    pub fn is_active(&self) -> bool {
        self.sid.is_some() || self.pending_create_req.is_some() || self.awaiting_stream_sid
    }

    pub fn cues(&self) -> &[AsrCue] {
        &self.cues
    }

    fn reset_local_state(&mut self) {
        self.sid = None;
        self.pending_create_req = None;
        self.awaiting_stream_sid = false;
        self.cues.clear();
        self.reload_in_flight = None;
        self.reload_dirty = false;
        self.deselected_by_user = false;
        self.pending_cues.clear();
        self.pending_adds.clear();
        self.mpv_cue_ids.clear();
        self.pending_edits.clear();
        self.pending_removals.clear();
    }

    fn abandon_pending_create(&mut self) {
        if let Some(old_req) = self.pending_create_req.take() {
            self.abandoned_create_reqs.insert(old_req);
        }
    }

    /// Create/add and select the ASR track in libmpv asynchronously without
    /// blocking the UI or GL render thread.
    pub fn start(&mut self, mpv: &MpvInstance, title: &str, language: &str) -> Result<(), String> {
        if let Some(old_sid) = self.sid {
            let _ = match self.mode {
                AsrTrackMode::NativeMemory => mpv.remove_memory_subtitle_track_async(old_sid),
                AsrTrackMode::StreamCallback => {
                    let id_str = old_sid.to_string();
                    mpv.command_node_async(&["sub-remove", &id_str])
                }
            };
        }
        self.abandon_pending_create();
        self.reset_local_state();
        let req = match self.mode {
            AsrTrackMode::NativeMemory => mpv.create_memory_subtitle_track_async(title, language)?,
            AsrTrackMode::StreamCallback => {
                mpv.set_asr_stream_bytes(format_webvtt_sorted_cues(&[]));
                mpv.add_asr_stream_track_async(title, language)?
            }
        };
        self.pending_create_req = Some(req);
        self.awaiting_stream_sid = self.mode == AsrTrackMode::StreamCallback;
        Ok(())
    }

    fn upsert_sorted_cue(&mut self, cue: AsrCue) {
        // ASR edits usually target the newest cue; scan from the tail first.
        if let Some(old_idx) = self.cues.iter().rposition(|existing| existing.id == cue.id) {
            let left_ok = old_idx == 0 || cue_cmp(&self.cues[old_idx - 1], &cue) != Ordering::Greater;
            let right_ok = old_idx + 1 == self.cues.len()
                || cue_cmp(&cue, &self.cues[old_idx + 1]) != Ordering::Greater;
            if left_ok && right_ok {
                self.cues[old_idx] = cue;
                return;
            }
            self.cues.remove(old_idx);
        }

        if self
            .cues
            .last()
            .is_none_or(|last| cue_cmp(last, &cue) != Ordering::Greater)
        {
            self.cues.push(cue);
        } else {
            let idx = self
                .cues
                .partition_point(|existing| cue_cmp(existing, &cue) == Ordering::Less);
            self.cues.insert(idx, cue);
        }
    }

    fn validate_cue(cue: &AsrCue) -> Result<(), String> {
        if !cue_valid(cue.start, cue.end, &cue.text) {
            return Err("invalid ASR subtitle cue".into());
        }
        Ok(())
    }

    /// Insert or replace a cue and push the update to libmpv asynchronously.
    pub fn add_cue(&mut self, mpv: &MpvInstance, cue: AsrCue) -> Result<(), String> {
        Self::validate_cue(&cue)?;
        self.upsert_sorted_cue(cue.clone());
        match self.mode {
            AsrTrackMode::StreamCallback => {
                mpv.set_asr_stream_bytes(format_webvtt_sorted_cues(&self.cues));
                self.trigger_stream_reload(mpv)
            }
            AsrTrackMode::NativeMemory => {
                self.pending_removals.remove(&cue.id);
                if let Some(&mpv_cue_id) = self.mpv_cue_ids.get(&cue.id) {
                    if let Some(track_id) = self.sid {
                        mpv.edit_memory_subtitle_async(
                            track_id,
                            mpv_cue_id,
                            cue.start,
                            cue.end,
                            &cue.text,
                        )?;
                    }
                    return Ok(());
                }
                if self.pending_adds.values().any(|&local_id| local_id == cue.id) {
                    self.pending_edits.insert(cue.id, cue);
                    return Ok(());
                }
                if let Some(track_id) = self.sid {
                    let req = mpv.add_memory_subtitle_async(
                        track_id,
                        cue.start,
                        cue.end,
                        &cue.text,
                    )?;
                    self.pending_adds.insert(req, cue.id);
                } else if self.pending_create_req.is_some() {
                    self.pending_cues.retain(|c| c.id != cue.id);
                    self.pending_cues.push_back(cue);
                }
                Ok(())
            }
        }
    }

    /// Replace the entire cue list in one batch: validates, deduplicates by ID
    /// (keeping the last entry), sorts once, serializes WebVTT once, and issues
    /// a single `sub-reload` in `StreamCallback` mode.
    pub fn replace_all_cues(
        &mut self,
        mpv: &MpvInstance,
        cues: Vec<AsrCue>,
    ) -> Result<(), String> {
        for cue in &cues {
            Self::validate_cue(cue)?;
        }

        // Deduplicate by ID (last wins) while avoiding hashing when all IDs are unique.
        let mut seen = HashSet::with_capacity(cues.len());
        let mut deduped = Vec::with_capacity(cues.len());
        for cue in cues.into_iter().rev() {
            if seen.insert(cue.id) {
                deduped.push(cue);
            }
        }
        deduped.reverse();
        deduped.sort_by(cue_cmp);

        self.cues = deduped;
        match self.mode {
            AsrTrackMode::StreamCallback => {
                mpv.set_asr_stream_bytes(format_webvtt_sorted_cues(&self.cues));
                self.trigger_stream_reload(mpv)
            }
            AsrTrackMode::NativeMemory => {
                self.pending_cues.clear();
                self.pending_edits.clear();
                self.pending_adds.clear();
                self.pending_removals.clear();
                self.mpv_cue_ids.clear();
                if let Some(track_id) = self.sid {
                    mpv.clear_memory_subtitles_async(track_id)?;
                    for cue in &self.cues {
                        let req = mpv.add_memory_subtitle_async(
                            track_id,
                            cue.start,
                            cue.end,
                            &cue.text,
                        )?;
                        self.pending_adds.insert(req, cue.id);
                    }
                } else if self.pending_create_req.is_some() {
                    self.pending_cues.extend(self.cues.iter().cloned());
                }
                Ok(())
            }
        }
    }

    pub fn edit_cue(&mut self, mpv: &MpvInstance, cue: AsrCue) -> Result<(), String> {
        self.add_cue(mpv, cue)
    }

    pub fn remove_cue(&mut self, mpv: &MpvInstance, cue_id: i64) -> Result<(), String> {
        self.cues.retain(|cue| cue.id != cue_id);
        match self.mode {
            AsrTrackMode::StreamCallback => {
                mpv.set_asr_stream_bytes(format_webvtt_sorted_cues(&self.cues));
                self.trigger_stream_reload(mpv)
            }
            AsrTrackMode::NativeMemory => {
                self.pending_cues.retain(|c| c.id != cue_id);
                self.pending_edits.remove(&cue_id);
                if let Some(mpv_cue_id) = self.mpv_cue_ids.remove(&cue_id) {
                    if let Some(track_id) = self.sid {
                        mpv.remove_memory_subtitle_async(track_id, mpv_cue_id)?;
                    }
                } else if self.pending_adds.values().any(|&local_id| local_id == cue_id) {
                    self.pending_removals.insert(cue_id);
                }
                Ok(())
            }
        }
    }

    pub fn clear(&mut self, mpv: &MpvInstance) -> Result<(), String> {
        self.cues.clear();
        self.pending_cues.clear();
        self.pending_edits.clear();
        match self.mode {
            AsrTrackMode::StreamCallback => {
                mpv.set_asr_stream_bytes(format_webvtt_sorted_cues(&[]));
                self.trigger_stream_reload(mpv)
            }
            AsrTrackMode::NativeMemory => {
                self.pending_adds.clear();
                self.pending_removals.clear();
                self.mpv_cue_ids.clear();
                if let Some(track_id) = self.sid {
                    mpv.clear_memory_subtitles_async(track_id)?;
                }
                Ok(())
            }
        }
    }

    pub fn remove_track(&mut self, mpv: &MpvInstance) -> Result<(), String> {
        if let Some(track_id) = self.sid {
            match self.mode {
                AsrTrackMode::NativeMemory => {
                    mpv.remove_memory_subtitle_track_async(track_id)?;
                }
                AsrTrackMode::StreamCallback => {
                    let id_str = track_id.to_string();
                    mpv.command_node_async(&["sub-remove", &id_str])?;
                }
            }
        }
        self.abandon_pending_create();
        mpv.set_asr_stream_bytes(format_webvtt_sorted_cues(&[]));
        self.reset_local_state();
        Ok(())
    }

    pub fn select(&mut self, mpv: &MpvInstance) -> Result<(), String> {
        self.deselected_by_user = false;
        if let Some(sid) = self.sid {
            mpv.set_sid(sid)?;
            if self.mode == AsrTrackMode::StreamCallback && self.reload_dirty {
                self.trigger_stream_reload(mpv)?;
            }
        }
        Ok(())
    }

    fn trigger_stream_reload(&mut self, mpv: &MpvInstance) -> Result<(), String> {
        if self.sid.is_none() || self.deselected_by_user {
            self.reload_dirty = true;
            return Ok(());
        }
        if self.reload_in_flight.is_some() {
            self.reload_dirty = true;
            return Ok(());
        }
        self.reload_dirty = false;
        let req = mpv.reload_asr_stream_track_async(self.sid)?;
        self.reload_in_flight = Some(req);
        Ok(())
    }

    /// Process an [`MpvEvent`] from `MpvInstance::drain_events`, resolving
    /// async track/cue IDs and flushing queued updates. Returns `Some(sid)`
    /// when the track's positive `sid` newly becomes ready.
    pub fn on_event(
        &mut self,
        mpv: &MpvInstance,
        event: &MpvEvent,
    ) -> Result<Option<i64>, String> {
        match event {
            MpvEvent::EndFile { .. } => {
                self.abandoned_create_reqs.clear();
                self.reset_local_state();
                Ok(None)
            }
            MpvEvent::PropertyChange {
                name,
                value: MpvPropertyValue::Tracks { subtitle, .. },
            } if name == "track-list" && self.mode == AsrTrackMode::StreamCallback => {
                if !self.is_active() {
                    if !self.abandoned_create_reqs.is_empty() {
                        for orphan in subtitle
                            .iter()
                            .filter(|t| t.external && t.external_path == ASR_STREAM_URI && t.id > 0)
                        {
                            let id_str = orphan.id.to_string();
                            let _ = mpv.command_node_async(&["sub-remove", &id_str]);
                        }
                    }
                    return Ok(None);
                }
                if let Some(track) = subtitle
                    .iter()
                    .rfind(|t| t.external && t.external_path == ASR_STREAM_URI && t.id > 0)
                {
                    let became_ready = self.sid.replace(track.id) != Some(track.id);
                    self.awaiting_stream_sid = false;
                    if self.pending_create_req.is_none() && self.reload_in_flight.is_none() {
                        self.deselected_by_user = !track.selected;
                        if !self.deselected_by_user && self.reload_dirty {
                            self.trigger_stream_reload(mpv)?;
                        }
                    } else if track.selected {
                        self.deselected_by_user = false;
                    }
                    if became_ready {
                        return Ok(Some(track.id));
                    }
                }
                Ok(None)
            }
            MpvEvent::PropertyChange { name, value }
                if name == "sid" && self.mode == AsrTrackMode::StreamCallback =>
            {
                if let Some(asr_sid) = self.sid {
                    match value {
                        MpvPropertyValue::Int64(id) if *id == asr_sid => {
                            self.deselected_by_user = false;
                            if self.pending_create_req.is_none()
                                && self.reload_in_flight.is_none()
                                && self.reload_dirty
                            {
                                self.trigger_stream_reload(mpv)?;
                            }
                        }
                        MpvPropertyValue::Int64(_) | MpvPropertyValue::Flag(false)
                            if self.pending_create_req.is_none()
                                && self.reload_in_flight.is_none() =>
                        {
                            self.deselected_by_user = true;
                        }
                        _ => {}
                    }
                }
                Ok(None)
            }
            MpvEvent::CommandReply {
                reply_userdata,
                error,
                result,
            } => {
                if self.abandoned_create_reqs.remove(reply_userdata) {
                    if *error >= 0 {
                        match self.mode {
                            AsrTrackMode::NativeMemory => {
                                if let CommandReplyResult::Int64(orphan_sid) = result {
                                    if id_valid(*orphan_sid) {
                                        let _ = mpv.remove_memory_subtitle_track_async(*orphan_sid);
                                    }
                                }
                            }
                            AsrTrackMode::StreamCallback => {
                                let tracks = mpv.subtitle_tracks();
                                if !self.is_active() {
                                    for orphan in tracks.iter().filter(|t| {
                                        t.external && t.external_path == ASR_STREAM_URI && t.id > 0
                                    }) {
                                        let id_str = orphan.id.to_string();
                                        let _ = mpv.command_node_async(&["sub-remove", &id_str]);
                                    }
                                } else {
                                    // An earlier `start()` completed before our current one:
                                    // remove the earliest duplicate stream track if present.
                                    let matching: Vec<i64> = tracks
                                        .iter()
                                        .filter(|t| {
                                            t.external
                                                && t.external_path == ASR_STREAM_URI
                                                && t.id > 0
                                        })
                                        .map(|t| t.id)
                                        .collect();
                                    if let Some(&first_id) = matching.first() {
                                        if Some(first_id) != self.sid
                                            && (matching.len() > 1
                                                || self.pending_create_req.is_some())
                                        {
                                            let id_str = first_id.to_string();
                                            let _ =
                                                mpv.command_node_async(&["sub-remove", &id_str]);
                                        }
                                    }
                                }
                            }
                        }
                    }
                    return Ok(None);
                }

                if Some(*reply_userdata) == self.pending_create_req {
                    self.pending_create_req = None;
                    if *error < 0 {
                        self.reset_local_state();
                        return Err(format!("ASR subtitle track creation failed ({error})"));
                    }
                    match self.mode {
                        AsrTrackMode::StreamCallback => {
                            let mut became_ready = false;
                            if self.sid.is_none() {
                                if let Some(track) = mpv
                                    .subtitle_tracks()
                                    .into_iter()
                                    .rfind(|t| {
                                        t.external
                                            && t.external_path == ASR_STREAM_URI
                                            && t.id > 0
                                    })
                                {
                                    became_ready = self.sid.replace(track.id) != Some(track.id);
                                    self.awaiting_stream_sid = false;
                                    self.deselected_by_user = !track.selected;
                                }
                            }
                            if self.reload_dirty && self.sid.is_some() && !self.deselected_by_user {
                                self.trigger_stream_reload(mpv)?;
                            }
                            return Ok(became_ready.then_some(self.sid).flatten());
                        }
                        AsrTrackMode::NativeMemory => {
                            let CommandReplyResult::Int64(track_id) = result else {
                                return Err("sub-create returned no track ID".into());
                            };
                            if !id_valid(*track_id) {
                                return Err("sub-create returned invalid track ID".into());
                            }
                            self.sid = Some(*track_id);
                            mpv.set_sid(*track_id)?;
                            while let Some(cue) = self.pending_cues.pop_front() {
                                let req = mpv.add_memory_subtitle_async(
                                    *track_id,
                                    cue.start,
                                    cue.end,
                                    &cue.text,
                                )?;
                                self.pending_adds.insert(req, cue.id);
                            }
                            return Ok(Some(*track_id));
                        }
                    }
                }

                if Some(*reply_userdata) == self.reload_in_flight {
                    self.reload_in_flight = None;
                    if *error < 0 {
                        return Err(format!("sub-reload failed with code {error}"));
                    }
                    if self.reload_dirty && self.sid.is_some() && !self.deselected_by_user {
                        self.trigger_stream_reload(mpv)?;
                    }
                    return Ok(None);
                }

                if let Some(local_id) = self.pending_adds.remove(reply_userdata) {
                    if *error < 0 {
                        self.pending_removals.remove(&local_id);
                        self.pending_edits.remove(&local_id);
                        return Err(format!("add-subtitle failed with code {error}"));
                    }
                    if let CommandReplyResult::Int64(mpv_cue_id) = result {
                        if let Some(track_id) = self.sid {
                            if self.pending_removals.remove(&local_id) {
                                mpv.remove_memory_subtitle_async(track_id, *mpv_cue_id)?;
                            } else {
                                self.mpv_cue_ids.insert(local_id, *mpv_cue_id);
                                if let Some(edited) = self.pending_edits.remove(&local_id) {
                                    mpv.edit_memory_subtitle_async(
                                        track_id,
                                        *mpv_cue_id,
                                        edited.start,
                                        edited.end,
                                        &edited.text,
                                    )?;
                                }
                            }
                        }
                    }
                }
                Ok(None)
            }
            _ => Ok(None),
        }
    }
}

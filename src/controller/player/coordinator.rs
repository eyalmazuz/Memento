//! Event-driven `LivePlayerCoordinator` bridging `MpvInstance`, `AsrSubtitleTrack`,
//! `AsrController`, `SubtitleController`, `AutoPauseState`, and Slint `MementoApp`.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use memento_player::{
    AsrCue, AsrSubtitleTrack, AutoPauseState, MpvEvent, MpvInstance, MpvPropertyValue, TrackType,
    VideoTrack, ASR_STREAM_URI,
};
use slint::VecModel;

use crate::controller::asr::{AsrController, AsrEvent, AsrSettings, SessionStart};
use crate::controller::media::MediaContext;
use crate::controller::subtitles::{
    create_cue, normalize_subtitle_text, ExternalSubtitleLoad, MemorySubtitleCue,
    SubtitleController, ASR_TRACK_ID,
};
use crate::{MementoApp, PlayerTrack, SubEntry};

use super::format_time;
use super::mouse::MouseInputForwarder;
use super::subtitle_sync::{
    clear_primary_subtitle, clear_subtitle_marks_on_advance, record_model_active,
    refresh_active_rows, refresh_subtitle_list_with_timing, sync_player_tracks,
    CachedSubtitleFilter,
};
use super::visibility::{
    compute_mpv_subtitle_visibility_ex, effective_media_title, is_image_subtitle_codec,
};

struct EventBatchSummary {
    saw_tracks_event: bool,
    saw_title_or_path_event: bool,
    saw_sub_timing_event: bool,
}

/// Event-driven coordinator bridging `MpvInstance::drain_events()`,
/// `AsrSubtitleTrack` (`sid > 0`), `AsrController`, `SubtitleController`, and
/// `AutoPauseState`.
pub struct LivePlayerCoordinator {
    state: memento_player::mpv::PlaybackState,
    asr_track: AsrSubtitleTrack,
    auto_pause: AutoPauseState,
    pub mouse: MouseInputForwarder,
    subtitle_filter: CachedSubtitleFilter,
    subtitle_codecs: HashMap<i64, String>,
    previous_audio: Vec<memento_player::AudioTrack>,
    previous_tracks: Vec<memento_player::SubtitleTrack>,
    previous_lines: Vec<memento_player::mpv::SubLine>,
    previous_audio_id: i64,
    previous_primary_sid: i64,
    previous_sub_delay: f64,
    previous_sub_speed: f64,
    previous_context_cue: Option<(i64, f64)>,
    previous_primary_text: String,
    previous_display_primary: String,
    previous_primary_cue_times: (f64, f64),
    previous_secondary_text: String,
    previous_display_secondary: String,
    previous_sub_times: (f64, f64),
    previous_auto_pause: bool,
    previous_primary_mpv_visible: Option<bool>,
    previous_secondary_mpv_visible: Option<bool>,
    previous_video_dimensions: (u32, u32),
    last_highlighted_rows: Vec<usize>,
    observed_media_generation: u64,
    suppress_next_start_file_increment: bool,
    tracks_known_for_file: bool,
    seek_pending_asr_notify: bool,
    autopause_seek_in_flight: bool,
    last_ui_pos: f32,
    last_ui_dur: f32,
    last_ui_vol: f32,
    last_ui_playing: Option<bool>,
    last_ui_time_sec: i64,
    last_ui_dur_sec: i64,
    shutdown_requested: bool,
    last_mpv_fullscreen: bool,
    pending_mpv_fullscreen_change: Option<bool>,
}

impl LivePlayerCoordinator {
    pub fn new(player: &MpvInstance) -> Self {
        let mut coord = Self::with_asr_track(AsrSubtitleTrack::new(player));
        coord.seed_from_player(player);
        coord
    }

    pub fn with_asr_track(asr_track: AsrSubtitleTrack) -> Self {
        Self {
            state: memento_player::mpv::PlaybackState {
                paused: true,
                volume: 100.0,
                volume_max: 100,
                sub_speed: 1.0,
                cursor_autohide: memento_player::mpv::CursorAutohide::Number(1000),
                ..Default::default()
            },
            asr_track,
            auto_pause: AutoPauseState::default(),
            mouse: MouseInputForwarder::new(),
            subtitle_filter: CachedSubtitleFilter::default(),
            subtitle_codecs: HashMap::new(),
            previous_audio: Vec::new(),
            previous_tracks: Vec::new(),
            previous_lines: Vec::new(),
            previous_audio_id: 0,
            previous_primary_sid: 0,
            previous_sub_delay: 0.0,
            previous_sub_speed: 1.0,
            previous_context_cue: None,
            previous_primary_text: String::new(),
            previous_display_primary: String::new(),
            previous_primary_cue_times: (0.0, 0.0),
            previous_secondary_text: String::new(),
            previous_display_secondary: String::new(),
            previous_sub_times: (0.0, 0.0),
            previous_auto_pause: false,
            previous_primary_mpv_visible: None,
            previous_secondary_mpv_visible: None,
            previous_video_dimensions: (0, 0),
            last_highlighted_rows: Vec::new(),
            observed_media_generation: 0,
            suppress_next_start_file_increment: false,
            tracks_known_for_file: false,
            seek_pending_asr_notify: false,
            autopause_seek_in_flight: false,
            last_ui_pos: -1.0,
            last_ui_dur: -1.0,
            last_ui_vol: -1.0,
            last_ui_playing: None,
            last_ui_time_sec: -1,
            last_ui_dur_sec: -1,
            shutdown_requested: false,
            last_mpv_fullscreen: false,
            pending_mpv_fullscreen_change: None,
        }
    }

    /// Seed initial `PlaybackState` from non-blocking `MpvInstance` snapshots
    /// without calling `poll_playback_state()` (so queued events are preserved).
    pub fn seed_from_player(&mut self, player: &MpvInstance) {
        self.state.position = player.get_position();
        self.state.duration = player.get_duration();
        self.state.paused = player.is_paused();
        self.state.fullscreen = player.is_fullscreen();
        self.last_mpv_fullscreen = self.state.fullscreen;
        self.state.volume = player.get_volume();
        self.state.dimensions = player.video_dimensions();
        self.state.audio_tracks = player.audio_tracks();
        self.state.video_tracks = player.video_tracks();
        self.state.subtitle_tracks = player.subtitle_tracks();
        self.state.sub_lines = player.primary_sub_lines();
        self.state.audio_id = player.selected_aid();
        self.state.video_id = player.selected_vid();
        self.state.primary_sid = player.primary_sid();
        self.state.secondary_sid = player.secondary_sid();
        self.state.sub_start = player.get_sub_start();
        self.state.sub_end = player.get_sub_end();
        self.state.sub_delay = player.get_sub_delay();
        self.state.sub_speed = player.get_sub_speed().max(f64::EPSILON);
        self.state.sub_text = normalize_subtitle_text(&player.get_sub_text().unwrap_or_default());
        self.state.secondary_sub_text =
            normalize_subtitle_text(&player.get_secondary_sub_text().unwrap_or_default());
        self.state.secondary_sub_start = player.get_secondary_sub_start();
        self.state.secondary_sub_end = player.get_secondary_sub_end();
        self.state.secondary_sub_delay = player.get_secondary_sub_delay();
        self.state.media_title = player.get_media_title().unwrap_or_default();
        if let Some(ctx) = player.capture_encode_context() {
            self.state.file_loaded = true;
            self.state.path = ctx.path;
            self.tracks_known_for_file = true;
        }
        self.previous_audio_id = self.state.audio_id;
        self.previous_primary_sid = self.state.primary_sid;
        self.previous_sub_delay = self.state.sub_delay;
        self.previous_sub_speed = self.state.sub_speed;
    }

    pub fn playback_state(&self) -> &memento_player::mpv::PlaybackState {
        &self.state
    }

    /// Consume and return whether `libmpv` delivered [`MpvEvent::Shutdown`]
    /// (e.g., user pressed `q`, mpv script invoked `quit`, or backend exited)
    /// so the UI sync loop can exit cleanly (C5.1).
    pub fn take_shutdown_requested(&mut self) -> bool {
        std::mem::replace(&mut self.shutdown_requested, false)
    }

    /// Inspect whether `MpvEvent::Shutdown` has been received without clearing it.
    pub fn is_shutdown_requested(&self) -> bool {
        self.shutdown_requested
    }

    /// Consume and return any external mpv `fullscreen` property change (e.g.,
    /// user pressed `f` in mpv or double-clicked video) for bidirectional
    /// fullscreen sync with the Slint/winit window (H3.1).
    pub fn take_mpv_fullscreen_change(&mut self) -> Option<bool> {
        self.pending_mpv_fullscreen_change.take()
    }

    /// Notify the coordinator when the application window itself changed
    /// fullscreen state (preventing an echo loop when mpv mirrors the flag).
    pub fn note_window_fullscreen_state(&mut self, fullscreen: bool) {
        self.state.fullscreen = fullscreen;
        self.last_mpv_fullscreen = fullscreen;
        self.pending_mpv_fullscreen_change = None;
    }

    /// Current video tracks reported by `mpv`'s `track-list`.
    pub fn video_tracks(&self) -> &[VideoTrack] {
        &self.state.video_tracks
    }

    /// Currently selected `mpv` video track id (`vid`, or `0` when disabled).
    pub fn selected_vid(&self) -> i64 {
        self.state.video_id
    }

    pub fn sub_delay(&self) -> f64 {
        self.state.sub_delay
    }

    pub fn secondary_sub_delay(&self) -> f64 {
        self.state.secondary_sub_delay
    }

    pub fn sub_speed(&self) -> f64 {
        self.state.sub_speed
    }

    pub fn remove_asr_track(&mut self, player: Option<&MpvInstance>) {
        if let Some(player) = player {
            let _ = self.asr_track.remove_track(player);
        }
    }

    pub fn asr_track(&self) -> &AsrSubtitleTrack {
        &self.asr_track
    }

    pub fn asr_track_mut(&mut self) -> &mut AsrSubtitleTrack {
        &mut self.asr_track
    }

    /// Returns true if the active primary subtitle track is ASS/SSA and native
    /// `libmpv`/`libass` rendering (`interface/sub-ass-native`) is enabled.
    pub fn primary_is_native_ass(&self, app: &MementoApp) -> bool {
        let primary_sid = self.state.primary_sid;
        primary_sid > 0
            && app.get_sub_ass_native()
            && !app.get_asr_track_selected()
            && self
                .subtitle_codecs
                .get(&primary_sid)
                .is_some_and(|codec| crate::controller::ass::is_ass_subtitle_codec(codec))
    }

    /// Codec reported by mpv for a subtitle SID (e.g. `"ass"`, `"subrip"`, `"hdmv_pgs_subtitle"`).
    pub fn subtitle_codec(&self, sid: i64) -> Option<&str> {
        self.subtitle_codecs.get(&sid).map(String::as_str)
    }

    pub fn auto_pause_state(&self) -> &AutoPauseState {
        &self.auto_pause
    }

    pub fn reset_auto_pause(&mut self) {
        self.auto_pause = AutoPauseState::default();
    }

    /// Synchronously update primary `sid` tracking when the user explicitly selects
    /// a non-ASR subtitle track (or `"None"`) from Memento's UI menu.
    pub fn on_manual_primary_sid(&mut self, sid: i64) {
        if self.asr_track.sid() != Some(sid) {
            self.asr_track.mark_deselected();
        }
        self.state.primary_sid = sid;
        self.previous_primary_sid = sid;
        self.auto_pause = AutoPauseState::default();
        self.previous_sub_times = (0.0, 0.0);
    }

    /// Mark that the caller initiated an explicit `loadfile` and already bumped
    /// `media_generation`, so the resulting `MpvEvent::StartFile` resets media
    /// state without double-incrementing `media_generation`.
    pub fn mark_explicit_media_load(&mut self, generation: u64) {
        self.observed_media_generation = generation;
        self.suppress_next_start_file_increment = true;
    }

    /// Record an explicit user seek (`seek_to`, `seek_relative`, `sub_seek`) so
    /// `AsrController` immediately reprioritizes and wakes from `Paused`/`Done`.
    pub fn notify_user_seek(
        &mut self,
        asr: &mut AsrController,
        target_pos: Option<f64>,
        duration: f64,
    ) {
        self.autopause_seek_in_flight = false;
        self.seek_pending_asr_notify = true;
        self.auto_pause.reset = true;
        if let Some(pos) = target_pos {
            asr.notify_seek(pos, duration);
        }
    }

    /// Determine language tag for `AsrSubtitleTrack::start` from the selected
    /// audio track, falling back to `"ja"`.
    pub fn selected_audio_language(player: &MpvInstance) -> String {
        let aid = player.selected_aid();
        player
            .audio_tracks()
            .into_iter()
            .find(|t| t.id == aid && !t.language.trim().is_empty())
            .map(|t| t.language)
            .unwrap_or_else(|| "ja".to_owned())
    }

    /// Activate or toggle the native `libmpv` ASR subtitle track (`sid > 0`).
    #[allow(clippy::too_many_arguments)]
    pub fn toggle_or_start_asr(
        &mut self,
        app: &MementoApp,
        player: &MpvInstance,
        asr: &mut AsrController,
        subtitles: &mut SubtitleController,
        sub_model: &VecModel<SubEntry>,
        asr_restart_pending: &Cell<bool>,
        settings_provider: impl FnOnce() -> Result<AsrSettings, String>,
    ) {
        if app.get_asr_track_selected() {
            asr.suspend();
            asr_restart_pending.set(false);
            self.asr_track.mark_deselected();
            let _ = player.set_sid(0);
            self.state.primary_sid = 0;
            self.previous_primary_sid = 0;
            self.auto_pause = AutoPauseState::default();
            self.previous_sub_times = (0.0, 0.0);
            app.set_asr_track_selected(false);
            app.set_asr_running(false);
            app.set_primary_sid(0);
            subtitles.set_primary_sid(0);
            refresh_subtitle_list_with_timing(
                app,
                sub_model,
                subtitles,
                self.state.sub_speed,
                self.state.sub_delay,
            );
            self.sync_highlighted_rows_from(subtitles);
            clear_primary_subtitle(app);
            app.set_status_message("ASR paused; subtitles and model kept in memory".into());
            return;
        }
        let dur = self
            .state
            .duration
            .max(player.get_duration())
            .max(app.get_duration() as f64);
        if dur <= 0.0 {
            app.set_status_message("Open a video before starting ASR subtitles".into());
            return;
        }
        let encode_ctx = player.capture_encode_context();
        let path = if !self.state.path.is_empty() {
            self.state.path.clone()
        } else if let Some(ctx) = encode_ctx.as_ref() {
            ctx.path.clone()
        } else {
            String::new()
        };
        let aid = if self.state.file_loaded {
            self.state.audio_id
        } else {
            player.selected_aid()
        };
        if path.is_empty() || aid <= 0 {
            app.set_status_message("No media loaded or no audio track selected".into());
            return;
        }
        let settings = match settings_provider() {
            Ok(s) => s,
            Err(err) => {
                app.set_status_message(err.into());
                return;
            }
        };
        let gpu_unavailable = settings.backend == crate::controller::asr::AsrBackend::Whisper
            && settings.whisper.use_gpu
            && settings.whisper.gpu_device.max(0) as usize
                >= crate::controller::asr::whisper_gpu_device_count();
        let media = match encode_ctx {
            Some(mut ctx) => {
                if ctx.audio_id <= 0 {
                    ctx.audio_id = aid;
                }
                if ctx.path.is_empty() {
                    ctx.path = path;
                }
                MediaContext::from(ctx)
            }
            None => MediaContext::new(path, aid),
        };
        let pos = player.get_position();
        match asr.start(settings, media, dur, pos) {
            Ok(session) => {
                let lang = Self::selected_audio_language(player);
                let track_res = match session {
                    SessionStart::New => {
                        if player.primary_sid() > 0 || self.state.primary_sid > 0 {
                            let _ = player.set_sid(0);
                        }
                        self.asr_track.start(player, "ASR Subtitles", &lang)
                    }
                    SessionStart::Resumed => {
                        if self.asr_track.is_active() {
                            self.asr_track.select(player)
                        } else {
                            if player.primary_sid() > 0 || self.state.primary_sid > 0 {
                                let _ = player.set_sid(0);
                            }
                            let res = self.asr_track.start(player, "ASR Subtitles", &lang);
                            if res.is_ok() {
                                let asr_cues: Vec<AsrCue> = asr
                                    .current_cues()
                                    .into_iter()
                                    .map(|cue| AsrCue {
                                        id: cue.id as i64,
                                        start: cue.start,
                                        end: cue.end,
                                        text: cue.text,
                                    })
                                    .collect();
                                if !asr_cues.is_empty() {
                                    let _ = self.asr_track.replace_all_cues(player, asr_cues);
                                }
                            }
                            res
                        }
                    }
                };
                if let Err(error) = track_res {
                    if session == SessionStart::Resumed {
                        asr.suspend();
                    } else {
                        asr.cancel();
                    }
                    app.set_status_message(
                        format!("Could not select ASR subtitles: {error}").into(),
                    );
                    return;
                }
                match session {
                    SessionStart::New => subtitles.start_asr_track(),
                    SessionStart::Resumed => {
                        subtitles.resume_asr_track(player.get_sub_raw_time(pos));
                    }
                }
                if let Some(sid) = self.asr_track.sid() {
                    subtitles.bind_asr_sid(sid);
                }
                asr_restart_pending.set(false);
                app.set_asr_track_selected(true);
                app.set_asr_running(true);
                refresh_subtitle_list_with_timing(
                    app,
                    sub_model,
                    subtitles,
                    self.state.sub_speed,
                    self.state.sub_delay,
                );
                self.sync_highlighted_rows_from(subtitles);
                app.set_status_message(
                    if session == SessionStart::Resumed {
                        "ASR resumed with saved subtitles"
                    } else if gpu_unavailable {
                        "Whisper GPU unavailable; transcribing on CPU. Check GPU driver and CUDA/Vulkan build."
                    } else {
                        "ASR subtitles started…"
                    }
                    .into(),
                );
            }
            Err(err) => {
                app.set_status_message(err.into());
            }
        }
    }

    fn sync_highlighted_rows_from(&mut self, subtitles: &SubtitleController) {
        self.last_highlighted_rows.clear();
        self.last_highlighted_rows
            .extend_from_slice(subtitles.active_indices(subtitles.primary_sid()));
    }

    #[allow(clippy::too_many_arguments)]
    fn reset_for_new_media(
        &mut self,
        app: &MementoApp,
        player: &MpvInstance,
        asr: &mut AsrController,
        subtitles: &mut SubtitleController,
        sub_model: &VecModel<SubEntry>,
        asr_restart_pending: &Cell<bool>,
        hover_pause_owned: &Cell<bool>,
        preserve_asr_selection: bool,
    ) {
        let should_restart_asr =
            preserve_asr_selection && app.get_asr_track_selected() && app.get_asr_enabled();
        if should_restart_asr {
            asr.suspend();
        } else {
            asr.cancel();
        }
        if preserve_asr_selection {
            self.asr_track = AsrSubtitleTrack::new(player);
            player.set_asr_stream_cues(&[]);
        } else {
            let _ = self.asr_track.remove_track(player);
        }
        app.set_asr_running(false);
        app.set_asr_track_selected(should_restart_asr);
        asr_restart_pending.set(should_restart_asr);

        self.state.file_loaded = false;
        self.state.playback_ready = false;
        self.tracks_known_for_file = false;
        self.state.audio_id = 0;
        self.state.primary_sid = 0;
        self.state.secondary_sid = 0;
        self.state.video_id = 0;
        self.state.sub_text.clear();
        self.state.sub_ass_extradata.clear();
        self.state.sub_text_ass_full.clear();
        self.state.secondary_sub_text.clear();
        self.state.sub_start = 0.0;
        self.state.sub_end = 0.0;
        self.state.secondary_sub_start = 0.0;
        self.state.secondary_sub_end = 0.0;
        self.state.audio_tracks.clear();
        self.state.video_tracks.clear();
        self.state.subtitle_tracks.clear();
        self.state.sub_lines.clear();

        self.subtitle_codecs.clear();
        self.previous_audio.clear();
        self.previous_tracks.clear();
        self.previous_lines.clear();
        self.previous_audio_id = 0;
        self.previous_primary_sid = 0;
        self.previous_sub_delay = 0.0;
        self.previous_sub_speed = 1.0;
        self.previous_context_cue = None;
        self.previous_primary_text.clear();
        self.previous_display_primary.clear();
        self.previous_primary_cue_times = (0.0, 0.0);
        self.previous_secondary_text.clear();
        self.previous_display_secondary.clear();
        self.previous_sub_times = (0.0, 0.0);
        self.previous_primary_mpv_visible = None;
        self.previous_secondary_mpv_visible = None;
        self.last_highlighted_rows.clear();
        self.seek_pending_asr_notify = false;
        self.autopause_seek_in_flight = false;
        self.auto_pause = AutoPauseState::default();
        self.mouse.release_all(player);
        hover_pause_owned.set(false);

        subtitles.clear();
        sub_model.set_vec(Vec::new());
        record_model_active(sub_model, &[]);
        app.set_subtitle_search_query("".into());
        app.set_audio_tracks(Rc::new(VecModel::<PlayerTrack>::default()).into());
        app.set_subtitle_tracks(Rc::new(VecModel::<PlayerTrack>::default()).into());
        app.set_audio_id(0);
        app.set_primary_sid(0);
        app.set_secondary_sid(0);
        app.set_secondary_sub_text("".into());
        clear_primary_subtitle(app);
    }

    fn flush_autopause(
        &mut self,
        player: &MpvInstance,
        auto_pause_enabled: bool,
        dirty: &mut bool,
    ) {
        if !*dirty {
            return;
        }
        let (s_start, s_end) = if self.state.sub_text.is_empty() {
            (0.0, 0.0)
        } else {
            (self.state.sub_start, self.state.sub_end)
        };
        // Wait if sub-start arrived ahead of sub-end in a split update.
        if (s_start, s_end) != (0.0, 0.0) && s_end <= s_start {
            return;
        }
        *dirty = false;
        let times = (s_start, s_end);
        if times != self.previous_sub_times {
            self.previous_sub_times = times;
            if let Some(seek_time) = self.auto_pause.step(
                auto_pause_enabled,
                times.0,
                times.1,
                self.state.sub_speed.max(f64::EPSILON),
                self.state.sub_delay,
                self.state.paused,
                self.state.duration,
            ) {
                self.autopause_seek_in_flight = true;
                let _ = player.pause();
                let _ = player.seek(seek_time);
            }
        }
    }

    /// Stage 1: Drain and apply all queued `MpvEvent`s in strict arrival order.
    #[allow(clippy::too_many_arguments)]
    fn apply_mpv_events(
        &mut self,
        events: &[MpvEvent],
        app: &MementoApp,
        player: &MpvInstance,
        subtitles: &mut SubtitleController,
        sub_model: &VecModel<SubEntry>,
        asr: &mut AsrController,
        media_generation: &Cell<u64>,
        asr_restart_pending: &Cell<bool>,
        hover_pause_owned: &Cell<bool>,
    ) -> EventBatchSummary {
        let mut sub_times_dirty = false;
        let mut saw_tracks_event = false;
        let mut saw_title_or_path_event = false;
        let mut saw_sub_timing_event = false;

        let auto_pause_enabled = app.get_auto_pause();
        if auto_pause_enabled != self.previous_auto_pause {
            self.previous_auto_pause = auto_pause_enabled;
            self.auto_pause = AutoPauseState::default();
            if auto_pause_enabled
                && !self.state.sub_text.is_empty()
                && self.state.sub_end > self.state.sub_start
            {
                let _ = self.auto_pause.step(
                    true,
                    self.state.sub_start,
                    self.state.sub_end,
                    self.state.sub_speed.max(f64::EPSILON),
                    self.state.sub_delay,
                    self.state.paused,
                    self.state.duration,
                );
                self.previous_sub_times = (self.state.sub_start, self.state.sub_end);
            }
        }

        for event in events {
            match self.asr_track.on_event(player, event) {
                Ok(Some(new_sid)) => {
                    subtitles.bind_asr_sid(new_sid);
                    if app.get_asr_track_selected() {
                        subtitles.set_primary_sid(new_sid);
                        refresh_subtitle_list_with_timing(
                            app,
                            sub_model,
                            subtitles,
                            self.state.sub_speed,
                            self.state.sub_delay,
                        );
                        self.sync_highlighted_rows_from(subtitles);
                    }
                }
                Ok(None) => {}
                Err(error) => {
                    app.set_status_message(error.into());
                }
            }

            match event {
                MpvEvent::Shutdown => {
                    self.shutdown_requested = true;
                }
                MpvEvent::StartFile { .. } => {
                    sub_times_dirty = false;
                    if self.suppress_next_start_file_increment {
                        self.suppress_next_start_file_increment = false;
                    } else {
                        let next = media_generation.get().wrapping_add(1);
                        media_generation.set(next);
                        self.observed_media_generation = next;
                    }
                    self.reset_for_new_media(
                        app,
                        player,
                        asr,
                        subtitles,
                        sub_model,
                        asr_restart_pending,
                        hover_pause_owned,
                        true,
                    );
                    saw_title_or_path_event = true;
                }
                MpvEvent::FileLoaded {
                    video_width,
                    video_height,
                } => {
                    self.state.file_loaded = true;
                    self.state.video_width = *video_width;
                    self.state.video_height = *video_height;
                    sub_times_dirty = false;
                    self.auto_pause = AutoPauseState::default();
                    self.previous_sub_times = (0.0, 0.0);
                    saw_tracks_event = true;
                    saw_title_or_path_event = true;
                }
                MpvEvent::EndFile { .. } => {
                    self.state.file_loaded = false;
                    self.state.playback_ready = false;
                    self.tracks_known_for_file = false;
                    self.state.audio_id = 0;
                    self.state.primary_sid = 0;
                    self.state.secondary_sid = 0;
                    self.state.sub_ass_extradata.clear();
                    self.state.sub_text_ass_full.clear();
                    sub_times_dirty = false;
                    self.auto_pause = AutoPauseState::default();
                }
                MpvEvent::Seek => {
                    self.auto_pause.reset = true;
                    if self.autopause_seek_in_flight {
                        self.autopause_seek_in_flight = false;
                    } else if self.state.file_loaded && self.state.duration > 0.0 {
                        self.seek_pending_asr_notify = true;
                        asr.notify_seek(self.state.position, self.state.duration);
                    }
                }
                MpvEvent::PlaybackRestart => {
                    self.state.playback_ready = true;
                    if self.seek_pending_asr_notify {
                        self.seek_pending_asr_notify = false;
                        asr.notify_seek(self.state.position, self.state.duration);
                    }
                }
                MpvEvent::PropertyChange { name, value } => match name.as_str() {
                    "path" => {
                        match value {
                            MpvPropertyValue::String(s) => self.state.path.clone_from(s),
                            _ => self.state.path.clear(),
                        }
                        saw_title_or_path_event = true;
                    }
                    "media-title" => {
                        match value {
                            MpvPropertyValue::String(s) => self.state.media_title.clone_from(s),
                            _ => self.state.media_title.clear(),
                        }
                        saw_title_or_path_event = true;
                    }
                    "time-pos" => {
                        self.flush_autopause(player, auto_pause_enabled, &mut sub_times_dirty);
                        if let MpvPropertyValue::Double(v) = value {
                            if v.is_finite() {
                                self.state.position = v.max(0.0);
                            }
                        } else if let MpvPropertyValue::Int64(v) = value {
                            self.state.position = (*v as f64).max(0.0);
                        }
                        if self.seek_pending_asr_notify {
                            self.seek_pending_asr_notify = false;
                            asr.notify_seek(self.state.position, self.state.duration);
                        }
                        self.auto_pause.update_position(
                            self.state.position,
                            self.state.sub_speed.max(f64::EPSILON),
                            self.state.sub_delay,
                        );
                    }
                    "duration" => {
                        self.state.duration = match value {
                            MpvPropertyValue::Double(v) if v.is_finite() => v.max(0.0),
                            MpvPropertyValue::Int64(v) => (*v as f64).max(0.0),
                            _ => 0.0,
                        };
                    }
                    "pause" => {
                        if let MpvPropertyValue::Flag(p) = value {
                            self.state.paused = *p;
                        }
                    }
                    "fullscreen" => {
                        if let MpvPropertyValue::Flag(f) = value {
                            self.state.fullscreen = *f;
                            if *f != self.last_mpv_fullscreen {
                                self.last_mpv_fullscreen = *f;
                                self.pending_mpv_fullscreen_change = Some(*f);
                            }
                        }
                    }
                    "mute" => {
                        if let MpvPropertyValue::Flag(m) = value {
                            self.state.mute = *m;
                        }
                    }
                    "volume" => {
                        self.state.volume = match value {
                            MpvPropertyValue::Double(v) if v.is_finite() => *v,
                            MpvPropertyValue::Int64(v) => *v as f64,
                            _ => 100.0,
                        };
                    }
                    "volume-max" => {
                        self.state.volume_max = match value {
                            MpvPropertyValue::Int64(v) => *v,
                            MpvPropertyValue::Double(v) if v.is_finite() => v.round() as i64,
                            _ => 100,
                        };
                    }
                    "dwidth" => {
                        self.state.dimensions.0 = match value {
                            MpvPropertyValue::Int64(v) => u32::try_from(*v).unwrap_or(0),
                            _ => 0,
                        };
                    }
                    "dheight" => {
                        self.state.dimensions.1 = match value {
                            MpvPropertyValue::Int64(v) => u32::try_from(*v).unwrap_or(0),
                            _ => 0,
                        };
                    }
                    "video-params/w" => {
                        self.state.video_width = match value {
                            MpvPropertyValue::Int64(v) => *v,
                            _ => 0,
                        };
                    }
                    "video-params/h" => {
                        self.state.video_height = match value {
                            MpvPropertyValue::Int64(v) => *v,
                            _ => 0,
                        };
                    }
                    "chapter-list" => match value {
                        MpvPropertyValue::Chapters(c) => self.state.chapters.clone_from(c),
                        _ => self.state.chapters.clear(),
                    },
                    "frame-drop-count" => {
                        self.state.frame_drops = match value {
                            MpvPropertyValue::Int64(v) => *v,
                            _ => 0,
                        };
                    }
                    "decoder-frame-drop-count" => {
                        self.state.decoder_drops = match value {
                            MpvPropertyValue::Int64(v) => *v,
                            _ => 0,
                        };
                    }
                    "hwdec-current" => match value {
                        MpvPropertyValue::String(s) => self.state.hwdec_current.clone_from(s),
                        _ => self.state.hwdec_current.clear(),
                    },
                    "aid" => match value {
                        MpvPropertyValue::Int64(v) => self.state.audio_id = *v,
                        MpvPropertyValue::Flag(false) => self.state.audio_id = 0,
                        _ => {}
                    },
                    "secondary-sid" => match value {
                        MpvPropertyValue::Int64(v) => self.state.secondary_sid = *v,
                        MpvPropertyValue::Flag(false) => self.state.secondary_sid = 0,
                        _ => {}
                    },
                    "vid" => {
                        let updated = match value {
                            MpvPropertyValue::Int64(v) => Some(*v),
                            MpvPropertyValue::Flag(false) => Some(0),
                            _ => None,
                        };
                        if let Some(vid) = updated {
                            self.state.video_id = vid;
                            for track in &mut self.state.video_tracks {
                                track.selected = track.id == vid;
                            }
                        }
                    }
                    "track-list" => {
                        saw_tracks_event = true;
                        if let MpvPropertyValue::Tracks {
                            audio,
                            subtitle,
                            all,
                        } = value
                        {
                            if self.state.file_loaded || !all.is_empty() {
                                self.tracks_known_for_file = true;
                            }
                            self.state.audio_tracks.clone_from(audio);
                            self.state.video_tracks = all
                                .iter()
                                .filter(|t| t.track_type == TrackType::Video)
                                .map(|t| VideoTrack {
                                    id: t.id,
                                    title: t.title.clone(),
                                    language: t.language.clone(),
                                    codec: t.codec.clone(),
                                    external: t.external,
                                    default_track: t.default_track,
                                    selected: t.selected,
                                })
                                .collect();
                            self.state.subtitle_tracks.clone_from(subtitle);
                            let mut primary_sid = 0;
                            let mut secondary_sid = 0;
                            let mut audio_id = 0;
                            let mut video_id = 0;
                            self.subtitle_codecs.clear();
                            for t in all {
                                if t.track_type == TrackType::Subtitle && t.id > 0 {
                                    self.subtitle_codecs.insert(t.id, t.codec.clone());
                                }
                                if !t.selected {
                                    continue;
                                }
                                match (t.track_type, t.main_selection) {
                                    (TrackType::Subtitle, 0) if primary_sid == 0 => {
                                        primary_sid = t.id
                                    }
                                    (TrackType::Subtitle, 1) if secondary_sid == 0 => {
                                        secondary_sid = t.id
                                    }
                                    (TrackType::Audio, 0) if audio_id == 0 => audio_id = t.id,
                                    (TrackType::Video, 0) if video_id == 0 => video_id = t.id,
                                    _ => {}
                                }
                            }
                            self.state.primary_sid = primary_sid;
                            self.state.secondary_sid = secondary_sid;
                            self.state.audio_id = audio_id;
                            self.state.video_id = video_id;

                            if let Some(stream_track) = subtitle.iter().rfind(|t| {
                                t.external && t.external_path == ASR_STREAM_URI && t.id > 0
                            }) {
                                subtitles.bind_asr_sid(stream_track.id);
                                if app.get_asr_track_selected() {
                                    if stream_track.selected {
                                        self.previous_primary_sid = stream_track.id;
                                    } else if self.previous_primary_sid == stream_track.id {
                                        asr.suspend();
                                        app.set_asr_running(false);
                                        app.set_asr_track_selected(false);
                                        self.previous_primary_sid = self.state.primary_sid;
                                        self.auto_pause = AutoPauseState::default();
                                        self.previous_sub_times = (0.0, 0.0);
                                        clear_primary_subtitle(app);
                                    }
                                }
                            } else if let Some(asr_sid) = self.asr_track.sid() {
                                if let Some(mem_track) =
                                    subtitle.iter().find(|t| t.id == asr_sid)
                                {
                                    if app.get_asr_track_selected() {
                                        if mem_track.selected {
                                            self.previous_primary_sid = asr_sid;
                                        } else if self.previous_primary_sid == asr_sid {
                                            asr.suspend();
                                            app.set_asr_running(false);
                                            app.set_asr_track_selected(false);
                                            self.previous_primary_sid = self.state.primary_sid;
                                            self.auto_pause = AutoPauseState::default();
                                            self.previous_sub_times = (0.0, 0.0);
                                            clear_primary_subtitle(app);
                                        }
                                    }
                                }
                            }
                        } else {
                            self.state.audio_tracks.clear();
                            self.state.video_tracks.clear();
                            self.state.subtitle_tracks.clear();
                            self.state.primary_sid = 0;
                            self.state.secondary_sid = 0;
                            self.state.audio_id = 0;
                            self.state.video_id = 0;
                            self.subtitle_codecs.clear();
                        }
                    }
                    "sid" => {
                        if let Some(new_sid) = match value {
                            MpvPropertyValue::Int64(v) => Some(*v),
                            MpvPropertyValue::Flag(false) => Some(0),
                            _ => None,
                        } {
                            self.state.primary_sid = new_sid;
                            let asr_sid =
                                self.asr_track.sid().or_else(|| subtitles.asr_mpv_sid());
                            let is_transient_asr_reload_zero = new_sid == 0
                                && app.get_asr_track_selected()
                                && asr_sid.is_some();
                            if !is_transient_asr_reload_zero
                                && new_sid != self.previous_primary_sid
                            {
                                if asr_sid.is_some() && Some(new_sid) == asr_sid {
                                    if !app.get_asr_track_selected() {
                                        app.set_asr_track_selected(true);
                                    }
                                } else if new_sid > 0
                                    && app.get_asr_track_selected()
                                    && asr_sid.is_some()
                                    && Some(new_sid) != asr_sid
                                {
                                    asr.suspend();
                                    app.set_asr_running(false);
                                    app.set_asr_track_selected(false);
                                    clear_primary_subtitle(app);
                                }
                                self.previous_primary_sid = new_sid;
                                self.auto_pause = AutoPauseState::default();
                                self.previous_sub_times = (0.0, 0.0);
                                self.state.sub_start = 0.0;
                                self.state.sub_end = 0.0;
                                self.state.sub_text_ass_full.clear();
                                if new_sid == 0 {
                                    self.state.sub_ass_extradata.clear();
                                }
                                sub_times_dirty = false;
                            }
                        }
                    }
                    "sub-delay" => {
                        let d = match value {
                            MpvPropertyValue::Double(v) if v.is_finite() => *v,
                            MpvPropertyValue::Int64(v) => *v as f64,
                            _ => 0.0,
                        };
                        self.state.sub_delay = d;
                        if (d - self.previous_sub_delay).abs() > 1e-9 {
                            self.previous_sub_delay = d;
                            self.auto_pause = AutoPauseState::default();
                            saw_sub_timing_event = true;
                        }
                    }
                    "sub-speed" => {
                        let s = match value {
                            MpvPropertyValue::Double(v) if v.is_finite() => v.max(f64::EPSILON),
                            MpvPropertyValue::Int64(v) => (*v as f64).max(f64::EPSILON),
                            _ => 1.0,
                        };
                        self.state.sub_speed = s;
                        if (s - self.previous_sub_speed).abs() > 1e-9 {
                            self.previous_sub_speed = s;
                            self.auto_pause = AutoPauseState::default();
                            saw_sub_timing_event = true;
                        }
                    }
                    "secondary-sub-delay" => {
                        self.state.secondary_sub_delay = match value {
                            MpvPropertyValue::Double(v) if v.is_finite() => *v,
                            MpvPropertyValue::Int64(v) => *v as f64,
                            _ => 0.0,
                        };
                    }
                    "sub-lines" => match value {
                        MpvPropertyValue::SubLines(lines) => self.state.sub_lines.clone_from(lines),
                        _ => self.state.sub_lines.clear(),
                    },
                    "sub-start" => {
                        match value {
                            MpvPropertyValue::Double(v) => self.state.sub_start = *v,
                            MpvPropertyValue::Int64(v) => self.state.sub_start = *v as f64,
                            MpvPropertyValue::None => {
                                self.state.sub_start = 0.0;
                                self.state.sub_end = 0.0;
                            }
                            _ => {}
                        }
                        sub_times_dirty = true;
                    }
                    "sub-end" => {
                        match value {
                            MpvPropertyValue::Double(v) => self.state.sub_end = *v,
                            MpvPropertyValue::Int64(v) => self.state.sub_end = *v as f64,
                            MpvPropertyValue::None => {
                                self.state.sub_start = 0.0;
                                self.state.sub_end = 0.0;
                            }
                            _ => {}
                        }
                        sub_times_dirty = true;
                    }
                    "sub-text" => {
                        let raw_text = match value {
                            MpvPropertyValue::String(s) => s.as_str(),
                            MpvPropertyValue::None => "",
                            _ => "",
                        };
                        let owned_fallback;
                        let effective_raw = if matches!(
                            value,
                            MpvPropertyValue::String(_) | MpvPropertyValue::None
                        ) {
                            raw_text
                        } else {
                            owned_fallback = player.get_sub_text().unwrap_or_default();
                            owned_fallback.as_str()
                        };
                        let text = normalize_subtitle_text(effective_raw);
                        if text.is_empty() {
                            self.state.sub_text.clear();
                            self.state.sub_start = 0.0;
                            self.state.sub_end = 0.0;
                        } else {
                            self.state.sub_text = text;
                        }
                        sub_times_dirty = true;
                    }
                    "sub-ass-extradata" => match value {
                        MpvPropertyValue::String(s) => self.state.sub_ass_extradata.clone_from(s),
                        _ => self.state.sub_ass_extradata.clear(),
                    },
                    "sub-text/ass-full" => match value {
                        MpvPropertyValue::String(s) => self.state.sub_text_ass_full.clone_from(s),
                        _ => self.state.sub_text_ass_full.clear(),
                    },
                    "osd-dimensions" => {
                        if let MpvPropertyValue::OsdDimensions(dims) = value {
                            self.state.osd_dimensions = *dims;
                        }
                    }
                    "sub-scale" => {
                        let v = match value {
                            MpvPropertyValue::Double(d) if d.is_finite() => *d,
                            MpvPropertyValue::Int64(i) => *i as f64,
                            _ => 1.0,
                        };
                        self.state.sub_scale = v.max(0.05);
                    }
                    "sub-pos" => {
                        let v = match value {
                            MpvPropertyValue::Double(d) if d.is_finite() => *d,
                            MpvPropertyValue::Int64(i) => *i as f64,
                            _ => 100.0,
                        };
                        self.state.sub_pos = v.clamp(0.0, 150.0);
                    }
                    "sub-font" => {
                        if let MpvPropertyValue::String(s) = value {
                            self.state.sub_font.clone_from(s);
                        }
                    }
                    "sub-ass-override" => {
                        if let MpvPropertyValue::String(s) = value {
                            self.state.sub_ass_override.clone_from(s);
                        }
                    }
                    "sub-scale-signs" => {
                        if let MpvPropertyValue::Flag(v) = value {
                            self.state.sub_scale_signs = *v;
                        }
                    }
                    "sub-use-margins" => {
                        if let MpvPropertyValue::Flag(v) = value {
                            self.state.sub_use_margins = *v;
                        }
                    }
                    "sub-hinting" => {
                        if let MpvPropertyValue::String(s) = value {
                            self.state.sub_hinting.clone_from(s);
                        }
                    }
                    "sub-shaper" => {
                        if let MpvPropertyValue::String(s) = value {
                            self.state.sub_shaper.clone_from(s);
                        }
                    }
                    "secondary-sub-start" => match value {
                        MpvPropertyValue::Double(v) => self.state.secondary_sub_start = *v,
                        MpvPropertyValue::Int64(v) => self.state.secondary_sub_start = *v as f64,
                        MpvPropertyValue::None => {
                            self.state.secondary_sub_start = 0.0;
                            self.state.secondary_sub_end = 0.0;
                        }
                        _ => {}
                    },
                    "secondary-sub-end" => match value {
                        MpvPropertyValue::Double(v) => self.state.secondary_sub_end = *v,
                        MpvPropertyValue::Int64(v) => self.state.secondary_sub_end = *v as f64,
                        MpvPropertyValue::None => {
                            self.state.secondary_sub_start = 0.0;
                            self.state.secondary_sub_end = 0.0;
                        }
                        _ => {}
                    },
                    "secondary-sub-text" => {
                        let owned_fallback;
                        let effective_raw = match value {
                            MpvPropertyValue::String(s) => s.as_str(),
                            MpvPropertyValue::None => "",
                            _ => {
                                owned_fallback = player.get_secondary_sub_text().unwrap_or_default();
                                owned_fallback.as_str()
                            }
                        };
                        let text = normalize_subtitle_text(effective_raw);
                        if text.is_empty() {
                            self.state.secondary_sub_text.clear();
                            self.state.secondary_sub_start = 0.0;
                            self.state.secondary_sub_end = 0.0;
                        } else {
                            self.state.secondary_sub_text = text;
                        }
                    }
                    "sub-visibility" => {
                        if let MpvPropertyValue::Flag(v) = value {
                            self.state.sub_visibility = *v;
                        }
                    }
                    "secondary-sub-visibility" => {
                        if let MpvPropertyValue::Flag(v) = value {
                            self.state.secondary_sub_visibility = *v;
                        }
                    }
                    "cursor-autohide" => {
                        if let MpvPropertyValue::CursorAutohide(v) = value {
                            self.state.cursor_autohide = *v;
                        }
                    }
                    "cursor-autohide-fs-only" => {
                        if let MpvPropertyValue::Flag(v) = value {
                            self.state.cursor_autohide_fs_only = *v;
                        }
                    }
                    _ => {}
                },
                _ => {}
            }
        }

        // Flush any trailing subtitle transition at the end of the drained event batch.
        let eff_times = if self.state.sub_text.is_empty() {
            (0.0, 0.0)
        } else {
            (self.state.sub_start, self.state.sub_end)
        };
        if eff_times != self.previous_sub_times {
            sub_times_dirty = true;
        }
        self.flush_autopause(player, auto_pause_enabled, &mut sub_times_dirty);

        EventBatchSummary {
            saw_tracks_event,
            saw_title_or_path_event,
            saw_sub_timing_event,
        }
    }

    /// Stage 2: Propagate audio track (`aid`) switches to active/selected ASR sessions.
    #[allow(clippy::too_many_arguments)]
    fn sync_audio_track_asr(
        &mut self,
        app: &MementoApp,
        player: &MpvInstance,
        subtitles: &mut SubtitleController,
        sub_model: &VecModel<SubEntry>,
        asr: &mut AsrController,
        asr_restart_pending: &Cell<bool>,
        raw_primary: f64,
    ) {
        if self.state.file_loaded && self.state.audio_id != self.previous_audio_id {
            let had_prev_aid = self.previous_audio_id != 0;
            self.previous_audio_id = self.state.audio_id;
            if (asr.is_active() || app.get_asr_track_selected() || subtitles.has_asr_track())
                && had_prev_aid
            {
                let _ = self.asr_track.clear(player);
                subtitles.clear_asr_cues(raw_primary);
                if self.state.audio_id <= 0 {
                    asr.notify_audio_track_changed(0);
                    asr_restart_pending.set(false);
                    let _ = self.asr_track.remove_track(player);
                    subtitles.clear_asr_track();
                    refresh_subtitle_list_with_timing(
                        app,
                        sub_model,
                        subtitles,
                        self.state.sub_speed,
                        self.state.sub_delay,
                    );
                    self.sync_highlighted_rows_from(subtitles);
                    if app.get_asr_track_selected() {
                        clear_primary_subtitle(app);
                        app.set_asr_track_selected(false);
                        app.set_status_message("Audio track disabled; ASR stopped".into());
                    }
                    app.set_asr_running(false);
                } else if asr.is_active() {
                    asr.notify_audio_track_changed(self.state.audio_id);
                    if app.get_asr_track_selected() {
                        refresh_subtitle_list_with_timing(
                            app,
                            sub_model,
                            subtitles,
                            self.state.sub_speed,
                            self.state.sub_delay,
                        );
                        self.sync_highlighted_rows_from(subtitles);
                        clear_primary_subtitle(app);
                        app.set_asr_running(true);
                    }
                } else if app.get_asr_track_selected() {
                    refresh_subtitle_list_with_timing(
                        app,
                        sub_model,
                        subtitles,
                        self.state.sub_speed,
                        self.state.sub_delay,
                    );
                    self.sync_highlighted_rows_from(subtitles);
                    clear_primary_subtitle(app);
                    app.set_asr_running(false);
                    asr_restart_pending.set(true);
                }
            }
        }
    }

    /// Stage 3: Execute deferred ASR restarts after settings reload or file transition.
    #[allow(clippy::too_many_arguments)]
    fn sync_deferred_asr_restart(
        &mut self,
        app: &MementoApp,
        player: &MpvInstance,
        subtitles: &mut SubtitleController,
        sub_model: &VecModel<SubEntry>,
        asr: &mut AsrController,
        asr_restart_pending: &Cell<bool>,
        asr_settings_provider: &impl Fn() -> Result<AsrSettings, String>,
        pos: f64,
        duration: f64,
        raw_primary: f64,
    ) {
        if !asr_restart_pending.get() {
            return;
        }
        if !app.get_asr_enabled() {
            asr_restart_pending.set(false);
            app.set_asr_track_selected(false);
            app.set_asr_running(false);
        } else if self.state.file_loaded
            && duration > 0.0
            && self.state.audio_id > 0
            && !self.state.path.is_empty()
        {
            match asr_settings_provider() {
                Ok(settings) => {
                    let media = match player.capture_encode_context() {
                        Some(mut ctx) => {
                            if ctx.audio_id <= 0 {
                                ctx.audio_id = self.state.audio_id;
                            }
                            if ctx.path.is_empty() {
                                ctx.path = self.state.path.clone();
                            }
                            MediaContext::from(ctx)
                        }
                        None => MediaContext::new(self.state.path.clone(), self.state.audio_id),
                    };
                    match asr.start(settings, media, duration, pos) {
                        Ok(session) => {
                            asr_restart_pending.set(false);
                            if self.asr_track.sid().is_none()
                                && (player.primary_sid() > 0 || self.state.primary_sid > 0)
                            {
                                let _ = player.set_sid(0);
                            }
                            let lang = Self::selected_audio_language(player);
                            match self.asr_track.start(player, "ASR Subtitles", &lang) {
                                Ok(()) => {
                                    match session {
                                        SessionStart::New => subtitles.start_asr_track(),
                                        SessionStart::Resumed => {
                                            subtitles.resume_asr_track(raw_primary);
                                        }
                                    }
                                    if let Some(sid) = self.asr_track.sid() {
                                        subtitles.bind_asr_sid(sid);
                                    }
                                    refresh_subtitle_list_with_timing(
                                        app,
                                        sub_model,
                                        subtitles,
                                        self.state.sub_speed,
                                        self.state.sub_delay,
                                    );
                                    self.sync_highlighted_rows_from(subtitles);
                                    app.set_asr_track_selected(true);
                                    app.set_asr_running(true);
                                    app.set_status_message("ASR transcribing subtitles…".into());
                                }
                                Err(error) => {
                                    asr.cancel();
                                    app.set_asr_track_selected(false);
                                    app.set_asr_running(false);
                                    app.set_status_message(
                                        format!("Could not restart ASR subtitles: {error}").into(),
                                    );
                                }
                            }
                        }
                        Err(error) => {
                            asr_restart_pending.set(false);
                            app.set_asr_track_selected(false);
                            app.set_asr_running(false);
                            app.set_status_message(
                                format!("Could not restart ASR subtitles: {error}").into(),
                            );
                        }
                    }
                }
                Err(error) => {
                    asr_restart_pending.set(false);
                    app.set_asr_track_selected(false);
                    app.set_asr_running(false);
                    app.set_status_message(format!("Could not reload ASR model: {error}").into());
                }
            }
        } else if self.state.file_loaded
            && duration > 0.0
            && !self.state.path.is_empty()
            && self.state.audio_id <= 0
            && (self.tracks_known_for_file || self.state.playback_ready)
        {
            asr_restart_pending.set(false);
            asr.suspend();
            app.set_asr_track_selected(false);
            app.set_asr_running(false);
            app.set_status_message("No active audio track; ASR paused".into());
        }
    }

    fn sync_title_and_media_path(
        &self,
        app: &MementoApp,
        current_media: &RefCell<Option<String>>,
        saw_title_or_path_event: bool,
    ) {
        if saw_title_or_path_event
            || (!self.state.path.is_empty() && app.get_media_title().is_empty())
        {
            if !self.state.path.is_empty() {
                *current_media.borrow_mut() = Some(self.state.path.clone());
            }
            let title = effective_media_title(&self.state.media_title, &self.state.path);
            if app.get_media_title().as_str() != title {
                app.set_media_title(title.into());
            }
        }
    }

    /// Stage 4: Change-gated Slint UI playback transport, time, and volume updates.
    fn sync_ui_state(
        &mut self,
        app: &MementoApp,
        player: &MpvInstance,
        pos: f64,
        duration: f64,
        paused: bool,
    ) {

        let pos_f32 = pos as f32;
        if (pos_f32 - self.last_ui_pos).abs() > 1e-3 {
            self.last_ui_pos = pos_f32;
            app.set_position(pos_f32);
        }
        let dur_f32 = duration as f32;
        if (dur_f32 - self.last_ui_dur).abs() > 1e-3 {
            self.last_ui_dur = dur_f32;
            app.set_duration(dur_f32);
        }
        let ui_playing = !paused || app.get_pending_playback_generation() >= 0;
        if self.last_ui_playing != Some(ui_playing) {
            self.last_ui_playing = Some(ui_playing);
            app.set_is_playing(ui_playing);
        }
        let ui_vol = if self.state.mute {
            0.0
        } else {
            self.state.volume as f32
        };
        if (ui_vol - self.last_ui_vol).abs() > 0.05 {
            self.last_ui_vol = ui_vol;
            app.set_volume(ui_vol);
        }
        let pos_sec = pos.max(0.0) as i64;
        if pos_sec != self.last_ui_time_sec {
            self.last_ui_time_sec = pos_sec;
            app.set_current_time(format_time(pos).into());
        }
        let dur_sec = duration.max(0.0) as i64;
        if dur_sec != self.last_ui_dur_sec {
            self.last_ui_dur_sec = dur_sec;
            app.set_duration_time(format_time(duration).into());
        }
        if self.previous_video_dimensions != self.state.dimensions {
            self.previous_video_dimensions = self.state.dimensions;
            app.invoke_update_video();
        }
        if !paused && player.pending_render_delay() > std::time::Duration::from_millis(100) {
            app.invoke_video_frame_ready();
        }
    }

    /// Stage 5: Synchronize tracks, subtitle text (with cached regex filter),
    /// ASR deltas, and O(1) active-row highlights.
    #[allow(clippy::too_many_arguments)]
    fn sync_subtitles(
        &mut self,
        app: &MementoApp,
        player: &MpvInstance,
        subtitles: &mut SubtitleController,
        sub_model: &VecModel<SubEntry>,
        asr: &mut AsrController,
        asr_restart_pending: &Cell<bool>,
        saw_tracks_event: bool,
        saw_sub_timing_event: bool,
        pos: f64,
        duration: f64,
        paused: bool,
        raw_primary: f64,
        raw_secondary: f64,
        on_publish_primary_subtitle: &mut impl FnMut(&MementoApp, &str),
    ) -> Vec<ExternalSubtitleLoad> {
        let mut external_loads = Vec::new();
        let mut list_refreshed = false;
        if saw_tracks_event
            || self.state.audio_tracks != self.previous_audio
            || self.state.subtitle_tracks != self.previous_tracks
            || subtitles.primary_sid()
                != (if app.get_asr_track_selected() {
                    subtitles
                        .asr_mpv_sid()
                        .or_else(|| self.asr_track.sid())
                        .or_else(|| subtitles.asr_track_id())
                        .unwrap_or(ASR_TRACK_ID)
                } else {
                    self.state.primary_sid
                })
            || subtitles.secondary_sid() != self.state.secondary_sid
        {
            external_loads = sync_player_tracks(
                app,
                &self.state,
                subtitles,
                sub_model,
                &mut self.previous_audio,
                &mut self.previous_tracks,
                subtitles.asr_mpv_sid().or_else(|| self.asr_track.sid()),
            );
            self.sync_highlighted_rows_from(subtitles);
            list_refreshed = true;
        }

        let primary_sid = self.state.primary_sid;
        let secondary_sid = self.state.secondary_sid;
        if primary_sid > 0 {
            subtitles.ensure_track(primary_sid);
        }
        if secondary_sid > 0 {
            subtitles.ensure_track(secondary_sid);
        }

        if self.state.sub_lines != self.previous_lines {
            let cues: Vec<MemorySubtitleCue> = self
                .state
                .sub_lines
                .iter()
                .map(|line| MemorySubtitleCue {
                    id: line.id,
                    text: line.text.encode_utf16().collect(),
                    start: line.start,
                    end: line.end,
                })
                .collect();
            if subtitles.update_memory_cues(primary_sid, &cues, raw_primary) {
                refresh_subtitle_list_with_timing(
                    app,
                    sub_model,
                    subtitles,
                    self.state.sub_speed,
                    self.state.sub_delay,
                );
                self.sync_highlighted_rows_from(subtitles);
                list_refreshed = true;
            }
            self.previous_lines.clone_from(&self.state.sub_lines);
        }

        // Recompile `remove-regex` ONLY when filter config actually changes, and
        // skip filtering/cloning completely on steady-state 60Hz ticks.
        let filter_changed = self.subtitle_filter.sync_from_app(app);
        if (saw_sub_timing_event || filter_changed) && !list_refreshed {
            refresh_subtitle_list_with_timing(
                app,
                sub_model,
                subtitles,
                self.state.sub_speed,
                self.state.sub_delay,
            );
            self.sync_highlighted_rows_from(subtitles);
        }

        let primary_times = (self.state.sub_start, self.state.sub_end);
        let primary_inputs_changed = filter_changed
            || self.state.sub_text != self.previous_primary_text
            || primary_times != self.previous_primary_cue_times;
        if primary_inputs_changed || self.previous_display_primary != app.get_active_sub_text().as_str()
        {
            let display_primary = if primary_inputs_changed {
                self.subtitle_filter.apply(&self.state.sub_text)
            } else {
                self.previous_display_primary.clone()
            };
            app.invoke_dismiss_definition();
            let previous_count = subtitles.primary_entries().len();
            let cue = (!self.state.sub_text.is_empty() && primary_times.1 > primary_times.0)
                .then(|| create_cue(&self.state.sub_text, primary_times.0, primary_times.1));
            if self.state.sub_text.is_empty() || primary_times.1 > primary_times.0 {
                subtitles.update_active_cue(primary_sid, cue.as_ref(), raw_primary);
                if subtitles.primary_entries().len() != previous_count {
                    refresh_subtitle_list_with_timing(
                        app,
                        sub_model,
                        subtitles,
                        self.state.sub_speed,
                        self.state.sub_delay,
                    );
                    self.sync_highlighted_rows_from(subtitles);
                }
            }
            on_publish_primary_subtitle(app, &display_primary);
            self.previous_primary_text.clone_from(&self.state.sub_text);
            self.previous_display_primary = display_primary;
            self.previous_primary_cue_times = primary_times;
        }

        let secondary_inputs_changed =
            filter_changed || self.state.secondary_sub_text != self.previous_secondary_text;
        if secondary_inputs_changed
            || self.previous_display_secondary != app.get_secondary_sub_text().as_str()
        {
            let display_secondary = if secondary_inputs_changed {
                self.subtitle_filter.apply(&self.state.secondary_sub_text)
            } else {
                self.previous_display_secondary.clone()
            };
            let cue = (!self.state.secondary_sub_text.is_empty()
                && self.state.secondary_sub_end > self.state.secondary_sub_start)
                .then(|| {
                    create_cue(
                        &self.state.secondary_sub_text,
                        self.state.secondary_sub_start,
                        self.state.secondary_sub_end,
                    )
                });
            if self.state.secondary_sub_text.is_empty()
                || self.state.secondary_sub_end > self.state.secondary_sub_start
            {
                subtitles.update_active_cue(secondary_sid, cue.as_ref(), raw_secondary);
            }
            app.set_secondary_sub_text(display_secondary.as_str().into());
            self.previous_secondary_text
                .clone_from(&self.state.secondary_sub_text);
            self.previous_display_secondary = display_secondary;
        }

        if !asr_restart_pending.get() {
            asr.update_position(pos, duration, paused);
            let mut refreshed_subtitles = false;
            asr.poll_with(|event| {
                if self.handle_asr_event(app, player, subtitles, raw_primary, event) {
                    refreshed_subtitles = true;
                }
            });
            if refreshed_subtitles && app.get_asr_track_selected() {
                refresh_subtitle_list_with_timing(
                    app,
                    sub_model,
                    subtitles,
                    self.state.sub_speed,
                    self.state.sub_delay,
                );
                self.sync_highlighted_rows_from(subtitles);
            }
        }

        let selected_sid = subtitles.primary_sid();
        subtitles.update_positions(raw_primary, raw_secondary);
        clear_subtitle_marks_on_advance(
            sub_model,
            subtitles,
            &mut self.previous_context_cue,
            paused,
        );
        let active = subtitles.active_indices(selected_sid);
        refresh_active_rows(sub_model, &self.last_highlighted_rows, active);
        if self.last_highlighted_rows.as_slice() != active {
            self.last_highlighted_rows.clear();
            self.last_highlighted_rows.extend_from_slice(active);
        }

        external_loads
    }

    /// Stage 6: Synchronize hover-pause release and `libmpv` subtitle visibility.
    fn sync_visibility(
        &mut self,
        app: &MementoApp,
        player: &MpvInstance,
        hover_pause_owned: &Cell<bool>,
        paused: bool,
    ) {
        if !app.get_hover_pause_enabled() && hover_pause_owned.replace(false) {
            let _ = player.play();
        }

        let primary_sid = self.state.primary_sid;
        let secondary_sid = self.state.secondary_sid;
        let primary_is_image = primary_sid > 0
            && !app.get_asr_track_selected()
            && self
                .subtitle_codecs
                .get(&primary_sid)
                .is_some_and(|codec| is_image_subtitle_codec(codec));
        let secondary_is_image = secondary_sid > 0
            && self
                .subtitle_codecs
                .get(&secondary_sid)
                .is_some_and(|codec| is_image_subtitle_codec(codec));
        let primary_is_native_ass = self.primary_is_native_ass(app);
        let (want_primary_vis, want_secondary_vis) = compute_mpv_subtitle_visibility_ex(
            app.get_hide_mpv_subtitles(),
            app.get_subtitles_visible(),
            app.get_hide_memento_while_playing(),
            !paused,
            primary_is_image,
            secondary_is_image,
            primary_is_native_ass,
            app.get_subtitle_cursor_show_enabled(),
            app.get_primary_cursor_reveal_active(),
            app.get_secondary_subtitle_cursor_show_enabled(),
            app.get_secondary_cursor_reveal_active(),
        );
        if self.previous_primary_mpv_visible != Some(want_primary_vis) {
            let vis = if want_primary_vis { "yes" } else { "no" };
            if let Err(err) = player.command_async(&["set", "sub-visibility", vis]) {
                eprintln!("Could not update mpv sub-visibility: {err}");
            } else {
                self.previous_primary_mpv_visible = Some(want_primary_vis);
            }
        }
        if self.previous_secondary_mpv_visible != Some(want_secondary_vis) {
            let vis = if want_secondary_vis { "yes" } else { "no" };
            if let Err(err) = player.command_async(&["set", "secondary-sub-visibility", vis]) {
                eprintln!("Could not update mpv secondary-sub-visibility: {err}");
            } else {
                self.previous_secondary_mpv_visible = Some(want_secondary_vis);
            }
        }
    }

    /// Drain all queued `MpvEvent`s in arrival order and `AsrController` deltas,
    /// updating UI models, native `AsrSubtitleTrack`, `AutoPauseState`, and
    /// `libmpv` subtitle visibility. Returns any external subtitle tracks to
    /// parse asynchronously.
    #[allow(clippy::too_many_arguments)]
    pub fn sync(
        &mut self,
        app: &MementoApp,
        player: &MpvInstance,
        subtitles: &mut SubtitleController,
        sub_model: &VecModel<SubEntry>,
        asr: &mut AsrController,
        media_generation: &Cell<u64>,
        current_media: &RefCell<Option<String>>,
        asr_restart_pending: &Cell<bool>,
        hover_pause_owned: &Cell<bool>,
        asr_settings_provider: impl Fn() -> Result<AsrSettings, String>,
        mut on_publish_primary_subtitle: impl FnMut(&MementoApp, &str),
    ) -> Vec<ExternalSubtitleLoad> {
        let events = player.drain_events();

        // Check if external caller (e.g. Close Media) bumped media_generation without StartFile.
        let current_gen = media_generation.get();
        if self.observed_media_generation != current_gen
            && !self.suppress_next_start_file_increment
        {
            self.observed_media_generation = current_gen;
            self.reset_for_new_media(
                app,
                player,
                asr,
                subtitles,
                sub_model,
                asr_restart_pending,
                hover_pause_owned,
                false,
            );
        }

        let summary = self.apply_mpv_events(
            &events,
            app,
            player,
            subtitles,
            sub_model,
            asr,
            media_generation,
            asr_restart_pending,
            hover_pause_owned,
        );
        // Stop immediately when libmpv is shutting down (e.g. `q` / `quit`) so
        // pre-shutdown `duration = 0` events do not flash the empty-state welcome
        // screen and pending ASR cues do not issue `sub-reload` to an uninitialized track.
        if self.shutdown_requested {
            return Vec::new();
        }

        let pos = self.state.position;
        let duration = self.state.duration;
        let paused = self.state.paused;
        let raw_primary = (pos - self.state.sub_delay) / self.state.sub_speed.max(f64::EPSILON);
        let raw_secondary =
            (pos - self.state.secondary_sub_delay) / self.state.sub_speed.max(f64::EPSILON);

        self.sync_title_and_media_path(app, current_media, summary.saw_title_or_path_event);
        self.sync_audio_track_asr(
            app,
            player,
            subtitles,
            sub_model,
            asr,
            asr_restart_pending,
            raw_primary,
        );
        self.sync_deferred_asr_restart(
            app,
            player,
            subtitles,
            sub_model,
            asr,
            asr_restart_pending,
            &asr_settings_provider,
            pos,
            duration,
            raw_primary,
        );
        self.sync_ui_state(app, player, pos, duration, paused);
        let external_loads = self.sync_subtitles(
            app,
            player,
            subtitles,
            sub_model,
            asr,
            asr_restart_pending,
            summary.saw_tracks_event,
            summary.saw_sub_timing_event,
            pos,
            duration,
            paused,
            raw_primary,
            raw_secondary,
            &mut on_publish_primary_subtitle,
        );
        self.sync_visibility(app, player, hover_pause_owned, paused);

        external_loads
    }

    /// Process a single [`AsrEvent`] against both the live `libmpv` [`AsrSubtitleTrack`]
    /// (`sid > 0`) and [`SubtitleController`], returning `true` if the subtitle list
    /// changed and should be refreshed when ASR is selected.
    pub fn handle_asr_event(
        &mut self,
        app: &MementoApp,
        player: &MpvInstance,
        subtitles: &mut SubtitleController,
        raw_primary: f64,
        event: AsrEvent,
    ) -> bool {
        let mut refreshed = false;
        match event {
            AsrEvent::Started { .. } | AsrEvent::WindowStarted { .. } => {
                if app.get_asr_track_selected() && !app.get_asr_running() {
                    app.set_asr_running(true);
                }
            }
            AsrEvent::CueAdded { cue, .. } => {
                let _ = self.asr_track.add_cue(
                    player,
                    AsrCue {
                        id: cue.id as i64,
                        start: cue.start,
                        end: cue.end,
                        text: cue.text.clone(),
                    },
                );
                let mem_cue = MemorySubtitleCue {
                    id: cue.id as i64,
                    text: cue.text.encode_utf16().collect(),
                    start: cue.start,
                    end: cue.end,
                };
                if subtitles.add_asr_cue(mem_cue, raw_primary) {
                    refreshed = true;
                }
            }
            AsrEvent::CueRemoved { cue_id, .. } => {
                let _ = self.asr_track.remove_cue(player, cue_id as i64);
                if subtitles.remove_asr_cue(cue_id as i64, raw_primary) {
                    refreshed = true;
                }
            }
            AsrEvent::CuesSnapshot { cues, .. } => {
                if cues.is_empty() {
                    let _ = self.asr_track.clear(player);
                    subtitles.clear_asr_cues(raw_primary);
                    refreshed = true;
                } else {
                    let asr_cues: Vec<AsrCue> = cues
                        .iter()
                        .map(|cue| AsrCue {
                            id: cue.id as i64,
                            start: cue.start,
                            end: cue.end,
                            text: cue.text.clone(),
                        })
                        .collect();
                    let _ = self.asr_track.replace_all_cues(player, asr_cues);
                    let mem_cues = cues
                        .into_iter()
                        .map(|c| MemorySubtitleCue {
                            id: c.id as i64,
                            text: c.text.encode_utf16().collect(),
                            start: c.start,
                            end: c.end,
                        })
                        .collect();
                    if subtitles.set_asr_cues_snapshot(mem_cues, raw_primary) {
                        refreshed = true;
                    }
                }
            }
            AsrEvent::Completed { cue_count, .. } => {
                app.set_asr_running(false);
                if app.get_asr_track_selected() {
                    app.set_status_message(
                        format!("ASR transcription completed ({cue_count} cues)").into(),
                    );
                }
            }
            AsrEvent::Paused { message, .. } => {
                app.set_asr_running(false);
                app.set_status_message(format!("ASR paused: {message}").into());
            }
            AsrEvent::Failed { message, .. } => {
                app.set_asr_running(false);
                app.set_status_message(format!("ASR error: {message}").into());
            }
            AsrEvent::Canceled { .. } => {
                app.set_asr_running(false);
            }
        }
        refreshed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::controller::subtitles::create_cue;
    use memento_player::{MpvInstance, Track};
    use slint::Model;

    fn make_test_mpv() -> MpvInstance {
        MpvInstance::new_software(None)
            .expect("create headless libmpv instance for coordinator unit test")
    }

    #[test]
    fn shutdown_event_sets_and_consumes_shutdown_requested() {
        i_slint_backend_testing::init_no_event_loop();
        let app = MementoApp::new().unwrap();
        let player = make_test_mpv();
        let mut coord = LivePlayerCoordinator::new(&player);
        let mut subs = SubtitleController::new();
        let sub_model = VecModel::<SubEntry>::default();
        let mut asr = AsrController::new();
        let media_gen = Cell::new(0u64);
        let asr_restart = Cell::new(false);
        let hover_pause = Cell::new(false);

        assert!(!coord.is_shutdown_requested());
        assert!(!coord.take_shutdown_requested());

        coord.apply_mpv_events(
            &[MpvEvent::Shutdown],
            &app,
            &player,
            &mut subs,
            &sub_model,
            &mut asr,
            &media_gen,
            &asr_restart,
            &hover_pause,
        );
        assert!(coord.is_shutdown_requested());
        assert!(coord.take_shutdown_requested());
        assert!(!coord.is_shutdown_requested());
        assert!(!coord.take_shutdown_requested());
    }

    #[test]
    fn mpv_fullscreen_property_surfaces_changes_and_suppresses_window_echoes() {
        i_slint_backend_testing::init_no_event_loop();
        let app = MementoApp::new().unwrap();
        let player = make_test_mpv();
        let mut coord = LivePlayerCoordinator::new(&player);
        let mut subs = SubtitleController::new();
        let sub_model = VecModel::<SubEntry>::default();
        let mut asr = AsrController::new();
        let media_gen = Cell::new(0u64);
        let asr_restart = Cell::new(false);
        let hover_pause = Cell::new(false);

        assert_eq!(coord.take_mpv_fullscreen_change(), None);

        // External mpv fullscreen toggle (`f` key in mpv) surfaces `Some(true)`.
        coord.apply_mpv_events(
            &[MpvEvent::PropertyChange {
                name: "fullscreen".into(),
                value: MpvPropertyValue::Flag(true),
            }],
            &app,
            &player,
            &mut subs,
            &sub_model,
            &mut asr,
            &media_gen,
            &asr_restart,
            &hover_pause,
        );
        assert_eq!(coord.take_mpv_fullscreen_change(), Some(true));
        assert_eq!(coord.take_mpv_fullscreen_change(), None);

        // Window itself exits fullscreen and notes state; matching mpv echo does not re-trigger.
        coord.note_window_fullscreen_state(false);
        coord.apply_mpv_events(
            &[MpvEvent::PropertyChange {
                name: "fullscreen".into(),
                value: MpvPropertyValue::Flag(false),
            }],
            &app,
            &player,
            &mut subs,
            &sub_model,
            &mut asr,
            &media_gen,
            &asr_restart,
            &hover_pause,
        );
        assert_eq!(coord.take_mpv_fullscreen_change(), None);
    }

    #[test]
    fn video_tracks_and_selected_vid_update_from_track_list_and_vid_events() {
        i_slint_backend_testing::init_no_event_loop();
        let app = MementoApp::new().unwrap();
        let player = make_test_mpv();
        let mut coord = LivePlayerCoordinator::new(&player);
        let mut subs = SubtitleController::new();
        let sub_model = VecModel::<SubEntry>::default();
        let mut asr = AsrController::new();
        let media_gen = Cell::new(0u64);
        let asr_restart = Cell::new(false);
        let hover_pause = Cell::new(false);

        let all = vec![
            Track {
                id: 1,
                track_type: TrackType::Video,
                src_id: 1,
                title: "Main 1080p".to_owned(),
                language: "und".to_owned(),
                album_art: false,
                default_track: true,
                selected: true,
                main_selection: 0,
                external: false,
                external_filename: String::new(),
                codec: "hevc".to_owned(),
            },
            Track {
                id: 2,
                track_type: TrackType::Video,
                src_id: 2,
                title: "Commentary".to_owned(),
                language: "ja".to_owned(),
                album_art: false,
                default_track: false,
                selected: false,
                main_selection: -1,
                external: false,
                external_filename: String::new(),
                codec: "h264".to_owned(),
            },
        ];

        coord.apply_mpv_events(
            &[MpvEvent::PropertyChange {
                name: "track-list".into(),
                value: MpvPropertyValue::Tracks {
                    audio: Vec::new(),
                    subtitle: Vec::new(),
                    all,
                },
            }],
            &app,
            &player,
            &mut subs,
            &sub_model,
            &mut asr,
            &media_gen,
            &asr_restart,
            &hover_pause,
        );
        assert_eq!(coord.selected_vid(), 1);
        assert_eq!(coord.video_tracks().len(), 2);
        assert!(coord.video_tracks()[0].selected);
        assert!(!coord.video_tracks()[1].selected);

        coord.apply_mpv_events(
            &[MpvEvent::PropertyChange {
                name: "vid".into(),
                value: MpvPropertyValue::Int64(2),
            }],
            &app,
            &player,
            &mut subs,
            &sub_model,
            &mut asr,
            &media_gen,
            &asr_restart,
            &hover_pause,
        );
        assert_eq!(coord.selected_vid(), 2);
        assert!(!coord.video_tracks()[0].selected);
        assert!(coord.video_tracks()[1].selected);

        // Verify sub-delay change marks timing event and refreshes displayed subtitle timestamps.
        subs.ensure_track(1);
        subs.set_primary_sid(1);
        subs.load_cues(vec![create_cue("Line", 5.0, 7.0)]);
        let summary = coord.apply_mpv_events(
            &[MpvEvent::PropertyChange {
                name: "sub-delay".into(),
                value: MpvPropertyValue::Double(1.5),
            }],
            &app,
            &player,
            &mut subs,
            &sub_model,
            &mut asr,
            &media_gen,
            &asr_restart,
            &hover_pause,
        );
        assert!(summary.saw_sub_timing_event);
        assert!((coord.sub_delay() - 1.5).abs() < 1e-9);
        refresh_subtitle_list_with_timing(
            &app,
            &sub_model,
            &subs,
            coord.sub_speed(),
            coord.sub_delay(),
        );
        assert!((sub_model.row_data(0).unwrap().start - 6.5).abs() < 1e-4);
        assert!((sub_model.row_data(0).unwrap().end - 8.5).abs() < 1e-4);
    }
}

//! The mpv player lifecycle state machine (Qt-free).
//!
//! Behavior source: `MpvPlayer`'s event handlers (mpvplayer.cpp:927-990) —
//! the `m_isFileLoaded` bookkeeping and the `fileStarted`/`fileLoaded`/
//! `fileEnded`/`seeked`/`shutdown` emissions — extended with an explicit
//! lifecycle the C++ leaves implicit in that boolean:
//!
//! | mpv                          | C++ facade concept                  | Machine state |
//! |------------------------------|-------------------------------------|---------------|
//! | idle / uninitialized         | pre-`mpv_initialize`                | [`Phase::Idle`]
//! | START_FILE … FILE_LOADED     | `fileStarted()`, `!isFileLoaded()`  | [`Phase::Loading`]
//! | FILE_LOADED … END_FILE       | `isFileLoaded() == true`            | [`Phase::Playing`] / [`Phase::Paused`]
//! | after END_FILE               | `isFileLoaded() == false`           | [`Phase::Ended`]
//! | SHUTDOWN                     | `emit shutdown()`                   | [`Phase::Shutdown`]
//!
//! `keep-open=yes` (hardcoded option, mpvplayer.cpp:649-657) means END_FILE
//! leaves mpv in a file-less limbo — still paused, still showing the last
//! frame. That is [`Phase::Ended`]; a subsequent START_FILE (playlist
//! continuation) transitions legally from there.
//!
//! Intent vs engine discrimination (the PROGRESS Entry 15 lesson, made
//! structural):
//!
//! - [`Intent::SeekAbsolute`] models `MpvController::seekRequested` — the
//!   *only* user seek intent. C++ emits it only after
//!   `mpv_command_async("seek", …)` succeeds, so an adapter must feed this
//!   intent on queue success only. It sets the intent latch.
//! - [`EngineEvent::EngineSeek`] (`MPV_EVENT_SEEK`) reports that a seek
//!   happened for *any* reason (user, script, auto-pause, stream op) and
//!   carries no intent. It never sets or clears the latch.
//! - [`Intent::Stop`] is the `MpvController::stop()` *request*. The engine
//!   stays authoritative: the lifecycle flip to [`Phase::Ended`] happens on
//!   the engine END_FILE that follows (that is also why END_FILE is only
//!   forbidden when no file is in flight *and* no stop is pending).
//! - Play/pause intents produce no phase change at all — `MpvController::
//!   play()`/`pause()` set the mpv property and the `pause` *property event*
//!   (engine truth) realizes the flip, exactly like
//!   `MpvState::setPause` in C++.
//!
//! The subtitle pause-on-end logic is *not* re-ported: the machine reuses
//! the certified `autopause.rs` functions (`memento_player_autopause_position`
//! on position ticks, `memento_player_autopause_step` on primary subtitle
//! text changes), wired to the same trigger set the C++ `PlayerManager`
//! connects (playermanager.cpp:35-74): file-loaded / sid change / primary
//! subtitle delay or speed change reset the bookkeeping; position ticks feed
//! the correction path; text changes run the pause step whose decision
//! (`MpvState::pause` + clamped `MpvController::seek`) comes back to the
//! adapter as decisions.
//!
//! Ordering note: C++ delivers the PlayerManager hooks over
//! `Qt::QueuedConnection` after a whole `handleMpvEvents()` drain, in signal
//! emission order. This core runs them inline per event, preserving the same
//! relative order; the hooks do not touch the property store, so state
//! results are identical — only instruction timing differs.

use crate::autopause::{
    MementoAutoPauseStateC, memento_player_autopause_position,
    memento_player_autopause_step,
};
use crate::events::{EngineEvent, Event, Intent, MpvEventSource, PropertyChange, SubLine};
use crate::state::{MpvStateCore, Signal, SubtitleChange, q_bound};

/// The high-level playback phase of the player core.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// Constructed; before the adapter reports `mpv_initialize` success
    /// (`MpvPlayer::initialized()`).
    Idle,
    /// `MPV_EVENT_START_FILE` seen, `MPV_EVENT_FILE_LOADED` not yet — the
    /// window in which C++ emitted `fileStarted()` with
    /// `isFileLoaded() == false`.
    Loading,
    /// A file is loaded and mpv reports `pause=false`.
    Playing,
    /// A file is loaded and mpv reports `pause=true`. Explicit user pause,
    /// the startup `pause=yes` option, and keep-open-at-eof are not
    /// distinguished — neither does the engine.
    Paused,
    /// END_FILE received; no file is loaded anymore. The property store
    /// keeps its last values exactly like C++ (`MpvState` has no reset);
    /// unavailable-property notifications are ignored, retaining stale
    /// values.
    Ended,
    /// `MPV_EVENT_SHUTDOWN` — the mpv handle is gone; all further events
    /// are rejected.
    Shutdown,
}

impl Phase {
    /// True while mpv has a usable file — the `MpvPlayer::isFileLoaded()`
    /// view of the state.
    pub fn is_file_loaded(self) -> bool {
        matches!(self, Phase::Playing | Phase::Paused)
    }

    /// The exact state string this phase yields for UI consumption.
    pub fn as_str(self) -> &'static str {
        match self {
            Phase::Idle => "idle",
            Phase::Loading => "loading",
            Phase::Playing => "playing",
            Phase::Paused => "paused",
            Phase::Ended => "ended",
            Phase::Shutdown => "shutdown",
        }
    }
}

impl std::fmt::Display for Phase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The change a transition describes, with its exact state string (task
/// item 3: "the exact state string each transition yields").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transition {
    /// START_FILE → [`Phase::Loading`] (legal from Idle and Ended).
    LoadStart,
    /// FILE_LOADED → [`Phase::Playing`] or [`Phase::Paused`].
    FileLoaded,
    /// END_FILE → [`Phase::Ended`].
    FileEnd,
    /// The pause property flipped while a file is loaded; the payload is
    /// the new paused-ness (`true` → Paused, `false` → Playing). This is
    /// engine-truth-driven, produced by the `pause` property event.
    PauseSet(bool),
    /// `MPV_EVENT_SEEK` — engine notification only.
    EngineSeek,
    /// A seek intent was registered (`seekRequested`).
    SeekRequested,
    /// A stop was requested; the engine END_FILE realizes the flip.
    StopRequested,
    /// The core-initiated auto-pause (pause-on-subtitle-end) fired.
    AutoPause,
    /// `MPV_EVENT_SHUTDOWN` from any non-shutdown phase.
    Shutdown,
}

impl Transition {
    pub fn state_string(self) -> &'static str {
        match self {
            Transition::LoadStart => "load-start",
            Transition::FileLoaded => "file-loaded",
            Transition::FileEnd => "file-end",
            Transition::PauseSet(true) => "paused",
            Transition::PauseSet(false) => "playing",
            Transition::EngineSeek => "engine-seek",
            Transition::SeekRequested => "intent-seek",
            Transition::StopRequested => "stop-requested",
            Transition::AutoPause => "auto-pause",
            Transition::Shutdown => "shutdown",
        }
    }
}

/// Why an input was rejected. Forbidden transitions are surfaced, never
/// silently swallowed: real mpv event sequences can never produce these —
/// a scripted source that does is a misbehaving adapter, and the UI can
/// surface it instead of the C++ silent `default: break;`.
#[derive(Debug, Clone, PartialEq)]
pub enum Rejected {
    /// An event arrived after `MPV_EVENT_SHUTDOWN`; mpv is dead.
    AfterShutdown(Event),
    /// START_FILE while a file is in flight — mpv serializes loads.
    StartFileInFlight(Event),
    /// FILE_LOADED without a preceding START_FILE.
    FileLoadedWithoutStart(Event),
    /// END_FILE with no file in flight and no stop pending.
    EndFileWithoutFile(Event),
}

/// One decision the core produced while handling an input, in order.
#[derive(Debug, Clone, PartialEq)]
pub enum Decision {
    /// A lifecycle transition whose `state_string()` is the UI value.
    Transition(Transition),
    /// The change-gated property notifications (`MpvState` signals).
    Signals(Vec<Signal>),
    /// `MpvPlayer::primarySubtitleLinesChanged(lines)`.
    PrimarySubtitleLines(Vec<SubLine>),
    /// `MpvPlayer::fileLoaded(width, height)` — the FILE_LOADED geometry
    /// probe results supplied by the source adapter.
    FileLoaded { width: i64, height: i64 },
    /// `MpvController::pause()` — auto-pause (pause-on-subtitle-end) asks
    /// the player to pause; the core never touches mpv.
    AutoPauseRequest,
    /// `MpvController::seek(time)` — the auto-pause rewind target,
    /// already clamped into `[0, duration]` by the certified
    /// `memento_player_autopause_step` (`playbackTime(end) - 0.1`). On a
    /// successful queue the adapter closes the loop by feeding back
    /// [`Intent::SeekAbsolute`], which is what sets the intent latch —
    /// mirroring C++ where `MpvController::seek()` *emits* `seekRequested`.
    AutoPauseSeek(f64),
}

/// The output seam for a future UI: consumes decisions as they are produced
/// by [`PlayerCore::drive_with_sink`].
pub trait PlayerDecisionSink {
    fn on_decisions(&mut self, decisions: &[Decision]);
    fn on_rejected(&mut self, _rejected: &Rejected) {}
}

/// A [`PlayerDecisionSink`] that collects everything for inspection.
#[derive(Debug, Default)]
pub struct CollectorSink {
    pub decisions: Vec<Decision>,
    pub rejected: Vec<Rejected>,
}

impl PlayerDecisionSink for CollectorSink {
    fn on_decisions(&mut self, decisions: &[Decision]) {
        self.decisions.extend_from_slice(decisions);
    }

    fn on_rejected(&mut self, rejected: &Rejected) {
        self.rejected.push(rejected.clone());
    }
}

/// The Qt-free player core: an [`MpvStateCore`] property store, a lifecycle
/// [`Phase`], the seek-intent latch, and the auto-pause bookkeeping that
/// reuses the certified `autopause.rs` state struct and functions.
pub struct PlayerCore {
    state: MpvStateCore,
    phase: Phase,
    seek_intent: Option<f64>,
    stop_requested: bool,
    auto_pause: MementoAutoPauseStateC,
    auto_pause_enabled: bool,
}

impl Default for PlayerCore {
    fn default() -> Self {
        Self::new()
    }
}

impl PlayerCore {
    pub fn new() -> Self {
        Self {
            state: MpvStateCore::new(),
            phase: Phase::Idle,
            seek_intent: None,
            stop_requested: false,
            auto_pause: MementoAutoPauseStateC::default(),
            auto_pause_enabled: false,
        }
    }

    // -- Configuration seam (wiring) --------------------------------------

    /// Mirrors `m_context->settings()->behaviorSubtitlePause()`. Defaults to
    /// `false` until wired.
    pub fn set_auto_pause_enabled(&mut self, enabled: bool) {
        self.auto_pause_enabled = enabled;
    }

    pub fn auto_pause_enabled(&self) -> bool {
        self.auto_pause_enabled
    }

    // -- Readers -----------------------------------------------------------

    pub fn phase(&self) -> Phase {
        self.phase
    }

    pub fn state(&self) -> &MpvStateCore {
        &self.state
    }

    /// `MpvPlayer::isFileLoaded()` — the bookkeeping the C++ event handlers
    /// toggle on START_FILE/FILE_LOADED/END_FILE.
    pub fn is_file_loaded(&self) -> bool {
        self.phase.is_file_loaded()
    }

    /// The seek-intent latch (`seekRequested`). Clearing mirrors the
    /// documented "stop/media lifecycle clears the seek state" behavior
    /// (PROGRESS Entry 15): a stop request clears it, and so does the
    /// engine END_FILE that follows. `MPV_EVENT_SEEK` never touches it.
    pub fn seek_intent(&self) -> Option<f64> {
        self.seek_intent
    }

    /// Whether a stop intent is pending (the engine END_FILE/SHUTDOWN will
    /// realize it). Exposed for the wiring layer and tests.
    pub fn stop_requested(&self) -> bool {
        self.stop_requested
    }

    /// Auto-pause bookkeeping, exposed for tests and diagnostics.
    pub fn auto_pause(&self) -> &MementoAutoPauseStateC {
        &self.auto_pause
    }

    // -- Derived decisions (task item 3) ------------------------------------

    /// The consumer-side position clamp semantics:
    /// `qBound(0.0, timePosition, duration)` (asrcontroller.cpp:720).
    /// Raw positions are *never* clamped into the store — C++ stores
    /// whatever mpv delivered, including EOF frames past `duration` — so
    /// bounded use goes through this seam. See [`q_bound`] for the NaN
    /// caveat (NaN collapses to `duration`, a Qt `qBound` implementation
    /// detail).
    pub fn clamped_position(&self) -> f64 {
        self.state.time_position_clamped()
    }

    /// The same clamp for an arbitrary position (e.g. a seek target).
    pub fn clamp_position(&self, position: f64) -> f64 {
        q_bound(0.0, position, self.state.duration())
    }

    /// Renderer-carry state: the current video geometry (`video-params/w|h`
    /// plus the FILE_LOADED probe). Resize/aspect story for the renderer
    /// seam: `MpvRenderer` renders mpv into a framebuffer of the QML item's
    /// size, mpv scales the video into it, and only the *core-visible*
    /// geometry (these two properties) is engine-owned — the item pixel
    /// size stays UI-owned. The renderer needs no other engine state, which
    /// is why `VideoParams` is all the core carries for it.
    pub fn video_params(&self) -> crate::state::VideoParams {
        self.state.video_params()
    }

    /// Duration handling summary: `duration` is engine-delivered only; a
    /// FILE_LOADED without a duration delivery leaves it at its last value
    /// (0 initially), and END_FILE keeps it stale — `MpvState::setDuration`
    /// is only ever fed by the `duration` property event. Boundaries that
    /// must not exceed it clamp via [`Self::clamp_position`].
    pub fn duration(&self) -> f64 {
        self.state.duration()
    }

    // -- Event handling ------------------------------------------------------

    /// Handles one input, producing decisions in order. Forbidden
    /// transitions return [`Rejected`] and mutate nothing.
    pub fn process(&mut self, event: Event) -> Result<Vec<Decision>, Rejected> {
        if self.phase == Phase::Shutdown {
            return Err(Rejected::AfterShutdown(event));
        }
        match event {
            Event::Engine(engine_event) => self.handle_engine(engine_event),
            Event::Intent(intent) => Ok(self.handle_intent(intent)),
        }
    }

    /// Drives one `handleMpvEvents()` drain — polls until the source
    /// reports exhausted, mirroring
    /// `while (m_mpv) { wait; if NONE break; handle; }`. A rejected event
    /// terminates the drain early (a misbehaving source) and is returned;
    /// the C++ loop had no such case (its `switch` had a silent default).
    pub fn drain(&mut self, source: &mut dyn MpvEventSource) -> Result<Vec<Decision>, Rejected> {
        let mut collected = Vec::new();
        while let Some(event) = source.poll() {
            collected.extend(self.process(event)?);
        }
        Ok(collected)
    }

    /// Like [`Self::drain`] but reporting every outcome to a sink and
    /// continuing past rejections. Returns the number of processed events.
    pub fn drive_with_sink(
        &mut self,
        source: &mut dyn MpvEventSource,
        sink: &mut dyn PlayerDecisionSink,
    ) -> usize {
        let mut processed = 0;
        while let Some(event) = source.poll() {
            processed += 1;
            match self.process(event) {
                Ok(decisions) => sink.on_decisions(&decisions),
                Err(rejected) => sink.on_rejected(&rejected),
            }
            if self.phase == Phase::Shutdown {
                break;
            }
        }
        processed
    }

    fn handle_engine(&mut self, event: EngineEvent) -> Result<Vec<Decision>, Rejected> {
        let mut out = Vec::with_capacity(4);
        match event {
            EngineEvent::StartFile => {
                if matches!(self.phase, Phase::Loading | Phase::Playing | Phase::Paused) {
                    return Err(Rejected::StartFileInFlight(Event::Engine(
                        EngineEvent::StartFile,
                    )));
                }
                self.phase = Phase::Loading;
                out.push(Decision::Transition(Transition::LoadStart));
                // C++ order: emit fileStarted(); then the empty sub-lines
                // list (mpvplayer.cpp:948-953).
                out.push(Decision::PrimarySubtitleLines(Vec::new()));
            }
            EngineEvent::FileLoaded { video_width, video_height } => {
                if self.phase != Phase::Loading {
                    return Err(Rejected::FileLoadedWithoutStart(Event::Engine(
                        EngineEvent::FileLoaded { video_width, video_height },
                    )));
                }
                self.phase = if self.state.pause() {
                    Phase::Paused
                } else {
                    Phase::Playing
                };
                out.push(Decision::Transition(Transition::FileLoaded));
                // C++ emits fileLoaded(w, h) with probe results obtained on
                // the handle inside the handler; the probe is adapter work,
                // the emission is core-owned (no libmpv genesis here).
                out.push(Decision::FileLoaded { width: video_width, height: video_height });
                // PlayerManager connects MpvPlayer::fileLoaded →
                // resetAutoPause (playermanager.cpp:35-39).
                self.auto_pause.reset = true;
            }
            EngineEvent::EndFile => {
                if !self.phase.is_file_loaded() && self.phase != Phase::Loading {
                    return Err(Rejected::EndFileWithoutFile(Event::Engine(
                        EngineEvent::EndFile,
                    )));
                }
                self.phase = Phase::Ended;
                self.stop_requested = false;
                // "Stop/media lifecycle clears the seek state" (PROGRESS
                // Entry 15): the lifecycle edge clears the latch.
                self.seek_intent = None;
                out.push(Decision::Transition(Transition::FileEnd));
                out.push(Decision::PrimarySubtitleLines(Vec::new()));
            }
            EngineEvent::EngineSeek => {
                // Not user intent — no latch, no bookkeeping.
                out.push(Decision::Transition(Transition::EngineSeek));
            }
            EngineEvent::Shutdown => {
                self.phase = Phase::Shutdown;
                out.push(Decision::Transition(Transition::Shutdown));
            }
            EngineEvent::Property(property) => match property {
                // Not a property-store concern: the sub-lines snapshot
                // becomes the player-level UI signal directly.
                PropertyChange::SubLines(lines) => {
                    out.push(Decision::PrimarySubtitleLines(lines));
                }
                property => {
                    let signals = self.state.apply_property(property);
                    out.extend(self.auto_pause_hooks(&signals));
                    if !signals.is_empty() {
                        // The pause property event (engine truth) realizes
                        // the playing/paused flip while a file is loaded —
                        // exactly how `MpvState::setPause` backs the QML
                        // bindings in C++.
                        let paused_flip = signals.iter().find_map(|signal| match signal {
                            Signal::PauseChanged(paused) => Some(*paused),
                            _ => None,
                        });
                        out.push(Decision::Signals(signals));
                        if let Some(paused) = paused_flip {
                            if self.phase.is_file_loaded() {
                                self.phase = if paused { Phase::Paused } else { Phase::Playing };
                                out.push(Decision::Transition(Transition::PauseSet(paused)));
                            }
                        }
                    }
                }
            },
        }
        Ok(out)
    }

    fn handle_intent(&mut self, intent: Intent) -> Vec<Decision> {
        let mut out = Vec::with_capacity(2);
        match intent {
            Intent::Play | Intent::Pause => {
                // The adapter has already applied the mpv property (that is
                // what play()/pause() do); the pause *property event* that
                // arrives later flips the phase. No optimistic state.
            }
            Intent::Stop => {
                // MpvController::stop() queues the engine stop; END_FILE
                // realizes the flip. `stop_requested` tolerates that
                // END_FILE and clears the seek latch with the request
                // (PROGRESS Entry 15: stop clears the seek state).
                self.stop_requested = true;
                self.seek_intent = None;
                out.push(Decision::Transition(Transition::StopRequested));
            }
            Intent::SeekAbsolute(time) => {
                self.seek_intent = Some(time);
                out.push(Decision::Transition(Transition::SeekRequested));
            }
        }
        out
    }

    /// The auto-pause wiring for one property delivery, mirroring the
    /// `PlayerManager` connect list (playermanager.cpp:35-74):
    ///
    /// - primary `SubtitleChanged { Delay } / { Speed }` and `SidChanged`
    ///   → reset the bookkeeping;
    /// - `TimePositionChanged` → position correction
    ///   ([`memento_player_autopause_position`]);
    /// - primary `SubtitleChanged { Text }` → the pause step
    ///   ([`memento_player_autopause_step`]), whose decisions are appended.
    ///
    /// All signals here are change-gated by the store, so a hook only runs
    /// when the C++ Qt signal would actually have fired.
    fn auto_pause_hooks(&mut self, signals: &[Signal]) -> Vec<Decision> {
        let mut out = Vec::with_capacity(2);
        let mut reset = false;
        let mut position_tick = false;
        let mut subtitle_text_changed = false;
        for signal in signals {
            match signal {
                Signal::SidChanged(_) => reset = true,
                // PlayerManager resets only on the primary subtitle object's
                // delay/speed signals.
                Signal::SubtitleChanged { secondary: false, change: SubtitleChange::Delay(_) }
                | Signal::SubtitleChanged { secondary: false, change: SubtitleChange::Speed(_) } => {
                    reset = true;
                }
                Signal::TimePositionChanged(_) => position_tick = true,
                Signal::SubtitleChanged { secondary: false, change: SubtitleChange::Text(_) } => {
                    subtitle_text_changed = true;
                }
                _ => {}
            }
        }
        if reset {
            // PlayerManager::resetAutoPause: the reset flag is set; the
            // certified step consumes it on its next run.
            self.auto_pause.reset = true;
        }
        if position_tick {
            let position = self.state.time_position();
            let speed = self.state.subtitle().speed();
            let delay = self.state.subtitle().delay();
            // Safety: `&mut self.auto_pause` is a valid, exclusively
            // borrowed state pointer for the duration of the call.
            unsafe {
                memento_player_autopause_position(
                    &mut self.auto_pause,
                    position,
                    speed,
                    delay,
                );
            }
        }
        if subtitle_text_changed && self.auto_pause_enabled {
            let subtitle = self.state.subtitle();
            let mut seek_time = 0.0;
            // Safety: all parameters are owned temporaries or exclusive
            // borrows; `out_seek_time` points to a local.
            let pause_now = unsafe {
                memento_player_autopause_step(
                    &mut self.auto_pause,
                    self.auto_pause_enabled,
                    subtitle.start_time(),
                    subtitle.end_time(),
                    subtitle.speed(),
                    subtitle.delay(),
                    self.state.pause(),
                    self.state.duration(),
                    &mut seek_time,
                )
            };
            if pause_now {
                // C++: controller->pause(); controller->seek(idealSeekTime);
                out.push(Decision::Transition(Transition::AutoPause));
                out.push(Decision::AutoPauseRequest);
                out.push(Decision::AutoPauseSeek(seek_time));
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{Intent, PropertyName};
    use crate::mpv::TrackType;

    /// Small helper: engine event shorthand.
    fn engine(event: EngineEvent) -> Event {
        Event::Engine(event)
    }

    fn load_started(core: &mut PlayerCore) {
        core.process(engine(EngineEvent::StartFile)).unwrap();
    }

    fn load_file(core: &mut PlayerCore, paused: bool) {
        core.process(engine(EngineEvent::Property(PropertyChange::Pause(paused)))).unwrap();
        core.process(engine(EngineEvent::FileLoaded { video_width: 1920, video_height: 1080 }))
            .unwrap();
    }

    fn loaded_playing_core() -> PlayerCore {
        let mut core = PlayerCore::new();
        load_started(&mut core);
        load_file(&mut core, false);
        core
    }

    fn transcript(decisions: &[Decision]) -> Vec<&'static str> {
        decisions
            .iter()
            .filter_map(|d| match d {
                Decision::Transition(transition) => Some(transition.state_string()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn full_session_transition_trace() {
        let mut core = PlayerCore::new();
        assert_eq!(core.phase().as_str(), "idle");

        load_started(&mut core);
        assert_eq!(core.phase().as_str(), "loading");
        assert!(!core.is_file_loaded());

        load_file(&mut core, false);
        assert_eq!(core.phase().as_str(), "playing");
        assert!(core.is_file_loaded());

        core.process(engine(EngineEvent::Property(PropertyChange::Pause(true)))).unwrap();
        assert_eq!(core.phase().as_str(), "paused");

        core.process(engine(EngineEvent::Property(PropertyChange::Pause(false)))).unwrap();
        assert_eq!(core.phase().as_str(), "playing");

        core.process(engine(EngineEvent::EndFile)).unwrap();
        assert_eq!(core.phase().as_str(), "ended");
        assert!(!core.is_file_loaded());
    }

    #[test]
    fn phase_strings_are_exact() {
        assert_eq!(Phase::Idle.as_str(), "idle");
        assert_eq!(Phase::Loading.as_str(), "loading");
        assert_eq!(Phase::Playing.as_str(), "playing");
        assert_eq!(Phase::Paused.as_str(), "paused");
        assert_eq!(Phase::Ended.as_str(), "ended");
        assert_eq!(Phase::Shutdown.as_str(), "shutdown");
        assert_eq!(Transition::LoadStart.state_string(), "load-start");
        assert_eq!(Transition::FileLoaded.state_string(), "file-loaded");
        assert_eq!(Transition::FileEnd.state_string(), "file-end");
        assert_eq!(Transition::PauseSet(true).state_string(), "paused");
        assert_eq!(Transition::PauseSet(false).state_string(), "playing");
        assert_eq!(Transition::EngineSeek.state_string(), "engine-seek");
        assert_eq!(Transition::SeekRequested.state_string(), "intent-seek");
        assert_eq!(Transition::StopRequested.state_string(), "stop-requested");
        assert_eq!(Transition::AutoPause.state_string(), "auto-pause");
        assert_eq!(Transition::Shutdown.state_string(), "shutdown");
    }

    #[test]
    fn forbidden_transitions_are_rejected() {
        let mut core = PlayerCore::new();
        // FILE_LOADED without START_FILE is forbidden.
        let rejected = core
            .process(engine(EngineEvent::FileLoaded { video_width: 1, video_height: 1 }))
            .unwrap_err();
        assert!(matches!(rejected, Rejected::FileLoadedWithoutStart(_)));

        load_started(&mut core);
        // A second START_FILE while loading is forbidden.
        assert!(matches!(
            core.process(engine(EngineEvent::StartFile)),
            Err(Rejected::StartFileInFlight(_))
        ));

        load_file(&mut core, false);
        // …and also while playing.
        assert!(matches!(
            core.process(engine(EngineEvent::StartFile)),
            Err(Rejected::StartFileInFlight(_))
        ));

        core.process(engine(EngineEvent::EndFile)).unwrap();
        // END_FILE with nothing in flight is forbidden.
        assert!(matches!(
            core.process(engine(EngineEvent::EndFile)),
            Err(Rejected::EndFileWithoutFile(_))
        ));
        // Nothing was corrupted by the rejections.
        assert_eq!(core.phase().as_str(), "ended");
        assert!(!core.is_file_loaded());
        assert_eq!(core.seek_intent(), None);
    }

    #[test]
    fn shutdown_is_terminal_and_rejects_everything() {
        let mut core = loaded_playing_core();
        core.process(engine(EngineEvent::Shutdown)).unwrap();
        assert_eq!(core.phase().as_str(), "shutdown");
        assert!(matches!(
            core.process(engine(EngineEvent::Property(PropertyChange::Duration(99.0)))),
            Err(Rejected::AfterShutdown(_))
        ));
        assert!(matches!(
            core.process(Event::Intent(Intent::SeekAbsolute(0.0))),
            Err(Rejected::AfterShutdown(_))
        ));
        // Even a new session's START_FILE stays rejected: the handle is dead.
        assert!(matches!(
            core.process(engine(EngineEvent::StartFile)),
            Err(Rejected::AfterShutdown(_))
        ));
    }

    #[test]
    fn intent_vs_engine_seek_discrimination() {
        let mut core = loaded_playing_core();

        // Engine seek never touches the intent latch.
        core.process(engine(EngineEvent::EngineSeek)).unwrap();
        assert_eq!(core.seek_intent(), None);

        // Intent registers the latch.
        core.process(Event::Intent(Intent::SeekAbsolute(12.5))).unwrap();
        assert_eq!(core.seek_intent(), Some(12.5));

        // Another engine seek between intents still leaves the latch.
        core.process(engine(EngineEvent::EngineSeek)).unwrap();
        assert_eq!(core.seek_intent(), Some(12.5));

        // A new intent replaces the latch.
        core.process(Event::Intent(Intent::SeekAbsolute(48.0))).unwrap();
        assert_eq!(core.seek_intent(), Some(48.0));

        // And the file lifecycle clears it (PROGRESS Entry 15).
        core.process(engine(EngineEvent::EndFile)).unwrap();
        assert_eq!(core.seek_intent(), None);
    }

    #[test]
    fn stop_intent_records_request_engine_completes_lifecycle() {
        let mut core = loaded_playing_core();
        core.process(Event::Intent(Intent::SeekAbsolute(30.0))).unwrap();
        assert_eq!(core.seek_intent(), Some(30.0));

        // Stop request: latch cleared with the request, phase unchanged.
        let decisions = core.process(Event::Intent(Intent::Stop)).unwrap();
        assert_eq!(transcript(&decisions), ["stop-requested"]);
        assert_eq!(core.phase().as_str(), "playing");
        assert_eq!(core.seek_intent(), None);

        // The engine END_FILE realizes the flip and is tolerate.
        core.process(engine(EngineEvent::EndFile)).unwrap();
        assert_eq!(core.phase().as_str(), "ended");
        assert!(!core.stop_requested());
    }

    #[test]
    fn play_pause_intents_do_not_move_phase() {
        let mut core = loaded_playing_core();
        assert!(transcript(&core.process(Event::Intent(Intent::Pause)).unwrap()).is_empty());
        assert_eq!(core.phase().as_str(), "playing");
        assert!(transcript(&core.process(Event::Intent(Intent::Play)).unwrap()).is_empty());
        assert_eq!(core.phase().as_str(), "playing");

        // The pause property event (engine truth) realizes the flip.
        core.process(engine(EngineEvent::Property(PropertyChange::Pause(true))))
            .unwrap();
        assert_eq!(core.phase().as_str(), "paused");
        // Since the intent kept the recorded pause state false by the
        // engine, the flip is entirely property-driven.
        let flipped = core.state().pause();
        assert!(flipped);
    }

    #[test]
    fn file_loaded_phase_follows_pause_property() {
        // keep-open/startup `pause=yes` arrives before FILE_LOADED.
        let mut core = PlayerCore::new();
        core.process(engine(EngineEvent::Property(PropertyChange::Pause(true))))
            .unwrap();
        core.process(engine(EngineEvent::StartFile)).unwrap();
        // The pause event while *loading* does not move the phase.
        assert_eq!(core.phase().as_str(), "loading");
        core.process(engine(EngineEvent::FileLoaded { video_width: 1, video_height: 1 }))
            .unwrap();
        assert_eq!(core.phase().as_str(), "paused");
    }

    #[test]
    fn file_lifecycle_clears_primary_subtitle_lines() {
        let mut core = PlayerCore::new();
        core.process(engine(EngineEvent::StartFile)).unwrap();
        let decisions = core.process(engine(EngineEvent::FileLoaded { video_width: 1, video_height: 1 })).unwrap();
        assert!(decisions.contains(&Decision::FileLoaded { width: 1, height: 1 }));
        // Replaying the engine seek emits the transition, no latch change.
        core.process(engine(EngineEvent::EndFile)).unwrap();
        let decisions = core.process(engine(EngineEvent::StartFile)).unwrap();
        assert!(decisions.contains(&Decision::PrimarySubtitleLines(Vec::new())));
    }

    #[test]
    fn engine_seek_never_mutates_bookkeeping_or_latch() {
        let mut core = loaded_playing_core();
        core.process(engine(EngineEvent::EngineSeek)).unwrap();
        assert_eq!(core.phase().as_str(), "playing");
        assert!(core.is_file_loaded());
        assert_eq!(core.seek_intent(), None);
    }

    // -- Derived decision tests -------------------------------------------

    #[test]
    fn position_clamp_semantics_at_the_core() {
        let mut core = loaded_playing_core();
        core.process(engine(EngineEvent::Property(PropertyChange::TimePos(-2.0))))
            .unwrap();
        assert_eq!(core.clamped_position(), 0.0);
        core.process(engine(EngineEvent::Property(PropertyChange::Duration(100.0))))
            .unwrap();
        core.process(engine(EngineEvent::Property(PropertyChange::TimePos(150.0))))
            .unwrap();
        assert_eq!(core.state().time_position(), 150.0);
        assert_eq!(core.clamped_position(), 100.0);
        core.process(engine(EngineEvent::Property(PropertyChange::TimePos(42.0))))
            .unwrap();
        assert_eq!(core.clamped_position(), 42.0);
        // Arbitrary target clamping (seek target).
        assert_eq!(core.clamp_position(250.0), 100.0);
        assert_eq!(core.clamp_position(-1.0), 0.0);
        assert_eq!(core.clamp_position(7.0), 7.0);
    }

    #[test]
    fn duration_as_property_and_clamp_only_after_delivery() {
        let mut core = loaded_playing_core();
        // No duration delivered: stale default 0 keeps everything clamped.
        core.process(engine(EngineEvent::Property(PropertyChange::TimePos(5.0))))
            .unwrap();
        assert_eq!(core.duration(), 0.0);
        assert_eq!(core.clamped_position(), 0.0);
        core.process(engine(EngineEvent::Property(PropertyChange::Duration(90.0))))
            .unwrap();
        assert_eq!(core.clamped_position(), 5.0);
        // END_FILE keeps the stale duration (C++ never resets the store).
        core.process(engine(EngineEvent::EndFile)).unwrap();
        assert_eq!(core.duration(), 90.0);
    }

    #[test]
    fn initial_volume_and_speed_application_order_replay() {
        // The initial property batch mpv delivers right after
        // mpv_initialize(), in OBSERVED_PROPERTIES_INIT_ORDER: volume-max
        // precedes volume, sub-delay precedes sub-speed.
        let mut core = PlayerCore::new();
        load_started(&mut core);
        load_file(&mut core, false);
        let events: Vec<Event> = vec![
            engine(EngineEvent::Property(PropertyChange::VolumeMax(130))),
            engine(EngineEvent::Property(PropertyChange::Volume(100))),
            engine(EngineEvent::Property(PropertyChange::VideoWidth(1920))),
            engine(EngineEvent::Property(PropertyChange::VideoHeight(1080))),
            engine(EngineEvent::Property(PropertyChange::SubDelay(0.0))),
            engine(EngineEvent::Property(PropertyChange::SubSpeed(1.0))),
            engine(EngineEvent::Property(PropertyChange::Duration(600.0))),
        ];
        for event in events {
            core.process(event).unwrap();
        }

        // The state is settled in the C++ initial values, applied in the C++
        // observation order.
        assert_eq!(core.state().volume(), 100);
        assert_eq!(core.state().max_volume(), 130);
        assert_eq!(core.state().subtitle().delay(), 0.0);
        assert_eq!(core.state().subtitle().speed(), 1.0);
    }

    #[test]
    fn video_params_carry_to_the_renderer_seam() {
        let mut core = PlayerCore::new();
        // Nothing known: aspect is None.
        assert_eq!(core.video_params().aspect_ratio(), None);
        // FILE_LOADED without START_FILE is forbidden.
        assert!(matches!(
            core.process(engine(EngineEvent::FileLoaded { video_width: 1280, video_height: 720 })),
            Err(Rejected::FileLoadedWithoutStart(_))
        ));
        load_started(&mut core);
        let decisions =
            core.process(engine(EngineEvent::FileLoaded { video_width: 1280, video_height: 720 }))
                .unwrap();
        // The probe results travel as the fileLoaded payload…
        assert!(decisions.contains(&Decision::FileLoaded { width: 1280, height: 720 }));
        // …but the *store* is only updated by the video-params property
        // events (C++ the same: FILE_LOADED emits, video-params/w|h events
        // mutate MpvState).
        assert_eq!(core.video_params(), crate::state::VideoParams::default());
        core.process(engine(EngineEvent::Property(PropertyChange::VideoWidth(1280))))
            .unwrap();
        core.process(engine(EngineEvent::Property(PropertyChange::VideoHeight(720))))
            .unwrap();
        let params = core.video_params();
        assert!(params.is_valid());
        assert!((params.aspect_ratio().unwrap() - 16.0 / 9.0).abs() < 1e-12);
        // video-params property events keep the renderer-carry updated.
        core.process(engine(EngineEvent::Property(PropertyChange::VideoWidth(640)))).unwrap();
        core.process(engine(EngineEvent::Property(PropertyChange::VideoHeight(480)))).unwrap();
        assert!((core.video_params().aspect_ratio().unwrap() - 4.0 / 3.0).abs() < 1e-12);
    }

    // -- Auto-pause integration (reuses certified autopause.rs) -------------

    fn cue(core: &mut PlayerCore, text: &str, start: f64, end: f64) {
        core.process(engine(EngineEvent::Property(PropertyChange::SubText {
            text: text.into(),
            start: Some(start),
            end: Some(end),
        })))
        .unwrap();
    }

    fn position(core: &mut PlayerCore, pos: f64) {
        core.process(engine(EngineEvent::Property(PropertyChange::TimePos(pos))))
            .unwrap();
    }

    #[test]
    fn auto_pause_fires_on_subtitle_end_with_clamped_seek() {
        let mut core = loaded_playing_core();
        core.set_auto_pause_enabled(true);
        core.process(engine(EngineEvent::Property(PropertyChange::Duration(100.0))))
            .unwrap();

        // Subtitle [2.0, 5.0] appears: cue registers, no pause yet.
        let decisions = core
            .process(engine(EngineEvent::Property(PropertyChange::SubText {
                text: "hello".into(),
                start: Some(2.0),
                end: Some(5.0),
            })))
            .unwrap();
        assert!(!transcript(&decisions).contains(&"auto-pause"));
        // Ticks within the cue feed the correction path (no reset from the
        // certified position function — 2.0→3.0 delta is within SEEK_DELTA).
        position(&mut core, 3.0);
        assert!(!core.auto_pause().reset);

        // Cue clears (text ""): the certified step fires the pause and
        // computes the 0.1-rewound, duration-clamped seek.
        let decisions = core
            .process(engine(EngineEvent::Property(PropertyChange::SubText {
                text: String::new(),
                start: Some(0.0),
                end: Some(0.0),
            })))
            .unwrap();
        assert_eq!(transcript(&decisions), ["auto-pause"]);
        assert!(decisions.contains(&Decision::AutoPauseRequest));
        assert!(decisions.contains(&Decision::AutoPauseSeek(4.9)));
        // The certified step's bookkeeping: end latch recorded, cue cleared.
        assert_eq!(core.auto_pause().last_end_time, 5.0);
        assert_eq!(core.auto_pause().end_time, 0.0);
        // The core leaves the engine truth alone: pause application is an
        // adapter action (Decision::AutoPauseRequest).
        assert!(!core.state().pause());
        assert_eq!(core.phase().as_str(), "playing");
    }

    #[test]
    fn auto_pause_disabled_never_fires() {
        let mut core = loaded_playing_core();
        core.process(engine(EngineEvent::Property(PropertyChange::Duration(100.0))))
            .unwrap();
        cue(&mut core, "hello", 2.0, 5.0);
        position(&mut core, 3.0);
        cue(&mut core, "", 0.0, 0.0);
        // Setting remains disabled: the certified step early-returns before
        // any latch is moved (no last_end_time), matching the C++ flag-OFF
        // path which returns before consuming the reset flag.
        assert_eq!(core.auto_pause().last_end_time, -1.0);
        // The file-loaded trigger lingers unconsumed — the C++ flag-OFF path
        // is `if (!behaviorSubtitlePause()) return;` before the reset
        // consumption, so set-true is simply never cleared.
        assert!(core.auto_pause().reset);
    }

    #[test]
    fn auto_pause_reset_triggers_from_the_player_manager_connect_list() {
        let mut core = loaded_playing_core();
        core.set_auto_pause_enabled(true);
        cue(&mut core, "cue", 2.0, 8.0);
        position(&mut core, 3.0);
        assert!(!core.auto_pause().reset);

        // sidChanged resets.
        core.process(engine(EngineEvent::Property(PropertyChange::Sid(3)))).unwrap();
        assert!(core.auto_pause().reset);
        // delay change resets.
        core.process(engine(EngineEvent::Property(PropertyChange::SubDelay(0.5))))
            .unwrap();
        assert!(core.auto_pause().reset);
        // speed change resets.
        core.process(engine(EngineEvent::Property(PropertyChange::SubSpeed(1.5))))
            .unwrap();
        assert!(core.auto_pause().reset);
        // FILE_LOADED resets.
        core.process(engine(EngineEvent::EndFile)).unwrap();
        core.process(engine(EngineEvent::StartFile)).unwrap();
        core.process(engine(EngineEvent::FileLoaded { video_width: 1, video_height: 1 })).unwrap();
        assert!(core.auto_pause().reset);
    }

    #[test]
    fn auto_pause_seek_loop_closes_through_the_intent_feedback() {
        // The seek generated by the core is a command for the adapter; the
        // intent event (seekRequested — emitted after a successful queue)
        // is what sets the latch.
        let mut core = loaded_playing_core();
        core.set_auto_pause_enabled(true);
        core.process(engine(EngineEvent::Property(PropertyChange::Pause(false))))
            .unwrap();
        core.process(engine(EngineEvent::Property(PropertyChange::Duration(100.0))))
            .unwrap();
        // A pause cannot fire while mpv is paused (certified step checks)
        // — engine truth drives it.
        core.process(engine(EngineEvent::Property(PropertyChange::Pause(true))))
            .unwrap();
        cue(&mut core, "hello", 2.0, 5.0);
        // Lift the pause without a text change (pause property only), then
        // position ticks must not fire a step; only a text change does.
        core.process(engine(EngineEvent::Property(PropertyChange::Pause(false))))
            .unwrap();
        position(&mut core, 4.0);
        assert_eq!(core.auto_pause().last_end_time, -1.0);
        // The intent feedback after the adapter queues the autopause seek.
        core.process(Event::Intent(Intent::SeekAbsolute(4.9))).unwrap();
        assert_eq!(core.seek_intent(), Some(4.9));
    }

    // -- Event source replay ------------------------------------------------

    #[test]
    fn scripted_replay_matches_a_real_session() {
        // A real playback session shape: initial property batch, load,
        // ticks, cue + auto-pause, user seek (intent + engine event), and
        // end-file (keep-open).
        let mut core = PlayerCore::new();
        core.set_auto_pause_enabled(true);
        let mut source = crate::events::ScriptedEventSource::new(vec![
            // The initialize-time observation batch.
            engine(EngineEvent::Property(PropertyChange::Unavailable(PropertyName::Duration))),
            engine(EngineEvent::Property(PropertyChange::Pause(true))),
            engine(EngineEvent::Property(PropertyChange::Unavailable(PropertyName::VideoWidth))),
            engine(EngineEvent::Property(PropertyChange::Unavailable(PropertyName::VideoHeight))),
            engine(EngineEvent::Property(PropertyChange::CursorAutohide(
                crate::mpv::CursorAutohide::Number(1000),
            ))),
            engine(EngineEvent::Property(PropertyChange::CursorAutohideFullscreenOnly(false))),
            engine(EngineEvent::Property(PropertyChange::VolumeMax(130))),
            engine(EngineEvent::Property(PropertyChange::Volume(100))),
            // Loading the file.
            engine(EngineEvent::StartFile),
            engine(EngineEvent::Property(PropertyChange::Path("/media/anime.mkv".into()))),
            engine(EngineEvent::Property(PropertyChange::MediaTitle("Episode 1".into()))),
            engine(EngineEvent::Property(PropertyChange::Duration(240.0))),
            engine(EngineEvent::Property(PropertyChange::Chapters(vec![0.0, 120.0]))),
            engine(EngineEvent::Property(PropertyChange::TrackList(vec![
                crate::events::Track {
                    id: 1,
                    track_type: TrackType::Audio,
                    ..Default::default()
                },
                crate::events::Track {
                    id: 2,
                    track_type: TrackType::Subtitle,
                    ..Default::default()
                },
            ]))),
            engine(EngineEvent::Property(PropertyChange::Aid(1))),
            engine(EngineEvent::Property(PropertyChange::Sid(2))),
            engine(EngineEvent::FileLoaded { video_width: 1920, video_height: 1080 }),
            // User presses play (engine truth arrives as the pause event).
            engine(EngineEvent::Property(PropertyChange::Pause(false))),
            // Playback: position ticks.
            engine(EngineEvent::Property(PropertyChange::TimePos(1.0))),
            engine(EngineEvent::Property(PropertyChange::TimePos(3.0))),
            // A cue comes and goes; auto-pause fires...
            engine(EngineEvent::Property(PropertyChange::SubText {
                text: "hibari".into(),
                start: Some(3.5),
                end: Some(6.0),
            })),
            engine(EngineEvent::Property(PropertyChange::TimePos(6.1))),
            engine(EngineEvent::Property(PropertyChange::SubText {
                text: String::new(),
                start: None,
                end: None,
            })),
            // …the player pauses + seeks (intents fed back post-queue).
            Event::Intent(Intent::Pause),
            Event::Intent(Intent::SeekAbsolute(5.9)),
            engine(EngineEvent::Property(PropertyChange::TimePos(5.9))),
            engine(EngineEvent::EngineSeek),
            // User resumes.
            Event::Intent(Intent::Play),
            engine(EngineEvent::Property(PropertyChange::Pause(false))),
            // End of file → keep-open.
            engine(EngineEvent::EndFile),
        ]);

        let decisions = core.drain(&mut source).unwrap();
        let trace = transcript(&decisions);
        assert_eq!(
            trace,
            vec![
                "load-start",
                "file-loaded",
                "playing", // Pause(false) flipped Paused → Playing
                "auto-pause",
                "intent-seek",
                "engine-seek",
                "file-end",
            ]
        );
        // Final: keep-open limbo with engine-settled properties.
        assert_eq!(core.phase().as_str(), "ended");
        assert!(!core.is_file_loaded());
        assert_eq!(core.state().path(), "/media/anime.mkv");
        assert_eq!(core.state().duration(), 240.0);
        assert_eq!(core.state().volume(), 100);
        assert_eq!(core.state().max_volume(), 130);
        assert_eq!(core.state().audio_tracks().len(), 1);
        assert_eq!(core.state().subtitle_tracks().len(), 1);
        assert_eq!(core.state().subtitle().speed(), 1.0);
        assert_eq!(core.state().subtitle().text(), "");
        // The seek intent was cleared with the file lifecycle.
        assert_eq!(core.seek_intent(), None);
    }

    #[test]
    fn drive_with_sink_collects_decisions_and_rejections() {
        let mut core = PlayerCore::new();
        let mut source = crate::events::ScriptedEventSource::new(vec![
            engine(EngineEvent::FileLoaded { video_width: 1, video_height: 1 }),
            engine(EngineEvent::StartFile),
            engine(EngineEvent::EndFile),
        ]);
        let mut sink = CollectorSink::default();
        let processed = core.drive_with_sink(&mut source, &mut sink);
        assert_eq!(processed, 3);
        assert!(matches!(sink.rejected[0], Rejected::FileLoadedWithoutStart(_)));
        assert!(transcript(&sink.decisions).contains(&"load-start"));
        assert!(transcript(&sink.decisions).contains(&"file-end"));
        // The rejected FILE_LOADED never produced its transition; the
        // surviving load ran from Idle to Ended exactly once.
        assert_eq!(core.phase().as_str(), "ended");
        assert_eq!(
            sink.decisions
                .iter()
                .filter(|d| matches!(d, Decision::Transition(Transition::FileLoaded)))
                .count(),
            0
        );
    }

    #[test]
    fn drain_stops_at_first_rejection() {
        let mut core = PlayerCore::new();
        let mut source = crate::events::ScriptedEventSource::new(vec![
            engine(EngineEvent::StartFile),
            engine(EngineEvent::FileLoaded { video_width: 2, video_height: 2 }),
            engine(EngineEvent::StartFile), // forbidden: in-flight load
            engine(EngineEvent::EndFile),   // never processed
        ]);
        let err = core.drain(&mut source).unwrap_err();
        assert!(matches!(err, Rejected::StartFileInFlight(_)));
        // The engine moved into Playing (the load succeeded) before the
        // rejection, and events after it were never consumed.
        assert_eq!(core.phase().as_str(), "playing");
    }
}

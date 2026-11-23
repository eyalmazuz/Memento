//! Main-thread controller facade for the streaming ASR worker.

#[cfg(any(feature = "asr-whisper", feature = "asr-onnx"))]
mod imp {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{mpsc, Arc, Condvar, Mutex};
    use std::thread::{self, JoinHandle};
    use std::time::Instant;

    use crate::controller::media::MediaContext as AsrMedia;

    use super::super::cue_book::SUBTITLE_TIME_DELTA;
    use super::super::engines::resolve_model;
    use super::super::worker::{lock, run_session, Control, JoinOnDrop, Shared};
    use super::super::{AsrCue, AsrEvent, AsrSettings, SessionStart, AVAILABLE};

    const SEEK_JUMP_THRESHOLD_SECONDS: f64 = 2.5;

    /// Main-thread facade for the streaming ASR worker.
    pub struct AsrController {
        shared: Arc<Shared>,
        receiver: mpsc::Receiver<AsrEvent>,
        worker: Option<JoinHandle<()>>,
        closing: bool,
        session: Option<(AsrSettings, AsrMedia)>,
    }

    impl Default for AsrController {
        fn default() -> Self {
            Self::new()
        }
    }

    impl AsrController {
        pub fn new() -> Self {
            let (sender, receiver) = mpsc::channel();
            drop(sender);
            Self {
                shared: Arc::new(Shared {
                    control: Mutex::new(Control::default()),
                    latest_cues: Mutex::new((0, Vec::new())),
                    wake: Condvar::new(),
                    running: AtomicBool::new(false),
                }),
                receiver,
                worker: None,
                closing: false,
                session: None,
            }
        }

        /// Resume a matching retained session, or start one for new media/settings.
        /// Missing or malformed model configuration is returned synchronously so
        /// the caller can present the exact setup action instead of a fake result.
        pub fn start(
            &mut self,
            settings: AsrSettings,
            media: AsrMedia,
            duration: f64,
            position: f64,
        ) -> Result<SessionStart, String> {
            if self.closing {
                return Err("ASR is shutting down.".into());
            }
            if !AVAILABLE {
                return Err("Speech recognition is not included in this build.".into());
            }
            if !settings.enabled {
                return Err("ASR subtitles are disabled in the current settings.".into());
            }
            if media.path.is_empty() {
                return Err("No media is loaded.".into());
            }
            if media.audio_id <= 0 {
                return Err("No active audio track".into());
            }
            if !duration.is_finite() || duration <= 0.0 || !position.is_finite() {
                return Err("The active media has no usable duration or playback position.".into());
            }
            let clamped_pos = position.clamp(0.0, duration);
            if self
                .session
                .as_ref()
                .is_some_and(|(loaded, source)| loaded == &settings && source == &media)
            {
                let mut control = lock(&self.shared.control);
                if control.active && !control.stop {
                    let baseline = control.suspended_at_position.unwrap_or(control.position);
                    let jumped = (clamped_pos - baseline).abs() > SEEK_JUMP_THRESHOLD_SECONDS;
                    let outside_window = is_outside_window(control.current_window, clamped_pos);
                    let worker_paused_or_done =
                        control.paused_on_errors || control.done || control.parked_no_audio;
                    if jumped || worker_paused_or_done {
                        control.generation += 1;
                        control.restart_position = Some(clamped_pos);
                        control.paused_on_errors = false;
                        control.done = false;
                        control.completed_duration = 0.0;
                        if outside_window {
                            if let Some(abort) = &control.current_abort {
                                abort.store(true, Ordering::Release);
                            }
                        }
                    }
                    control.position = clamped_pos;
                    control.duration = duration;
                    control.last_position = Some(clamped_pos);
                    control.last_position_at = Some(Instant::now());
                    control.last_paused = false;
                    control.suspended = false;
                    control.parked_no_audio = false;
                    control.suspended_at_position = None;
                    control.emit_snapshot = true;
                    self.shared.wake.notify_all();
                    return Ok(SessionStart::Resumed);
                }
            }

            let initial_position = if settings.start_from_beginning {
                0.0
            } else {
                clamped_pos
            };

            // Zero-reload media retargeting: when ASR settings are unchanged and a live
            // worker is already holding the loaded Whisper/ONNX model in RAM, retarget
            // `AsrMedia` (`path` / `audio_id`) in-place instead of reloading gigabytes.
            if self
                .session
                .as_ref()
                .is_some_and(|(loaded, source)| loaded == &settings && source != &media)
                && self.worker.as_ref().is_some_and(|w| !w.is_finished())
            {
                let mut control = lock(&self.shared.control);
                if control.active && !control.stop {
                    control.generation += 1;
                    let next_gen = control.generation;
                    control.min_cue_generation = next_gen;
                    control.reset_cues = true;
                    control.emit_snapshot = false;
                    control.media_path = media.path.clone();
                    control.audio_id = media.audio_id;
                    control.duration = duration;
                    control.position = clamped_pos;
                    control.last_position = Some(clamped_pos);
                    control.last_position_at = Some(Instant::now());
                    control.last_paused = false;
                    control.suspended = false;
                    control.parked_no_audio = false;
                    control.suspended_at_position = None;
                    control.paused_on_errors = false;
                    control.done = false;
                    control.completed_duration = 0.0;
                    control.restart_position = Some(initial_position);
                    if let Some(abort) = &control.current_abort {
                        abort.store(true, Ordering::Release);
                    }
                    drop(control);
                    *lock(&self.shared.latest_cues) = (next_gen, Vec::new());
                    self.shared.running.store(true, Ordering::Release);
                    self.shared.wake.notify_all();
                    self.session = Some((settings, media));
                    return Ok(SessionStart::New);
                }
            }

            if lock(&self.shared.control).active {
                self.cancel();
            }
            let model = resolve_model(&settings)?;
            let prev_worker = match self.worker.take() {
                Some(worker) if worker.is_finished() => {
                    let _ = worker.join();
                    None
                }
                other => other,
            };
            let prev_guard = JoinOnDrop(prev_worker);

            let (sender, receiver) = mpsc::channel();
            self.receiver = receiver;
            let shared = Arc::new(Shared {
                control: Mutex::new(Control {
                    stop: false,
                    active: true,
                    suspended: false,
                    parked_no_audio: false,
                    paused_on_errors: false,
                    done: false,
                    completed_duration: 0.0,
                    reset_cues: false,
                    emit_snapshot: false,
                    media_path: media.path.clone(),
                    audio_id: media.audio_id,
                    generation: 0,
                    min_cue_generation: 0,
                    duration,
                    position: clamped_pos,
                    last_position: Some(clamped_pos),
                    last_position_at: Some(Instant::now()),
                    last_paused: false,
                    suspended_at_position: None,
                    current_window: None,
                    restart_position: None,
                    current_abort: None,
                }),
                latest_cues: Mutex::new((0, Vec::new())),
                wake: Condvar::new(),
                running: AtomicBool::new(true),
            });
            self.shared = shared.clone();

            let session = (settings.clone(), media.clone());
            self.worker = Some(
                thread::Builder::new()
                    .name("subomi-asr".into())
                    .spawn(move || {
                        run_session(
                            shared,
                            sender,
                            settings,
                            media,
                            model,
                            initial_position,
                            prev_guard,
                        )
                    })
                    .map_err(|error| format!("Could not start ASR worker: {error}"))?,
            );
            self.session = Some(session);
            Ok(SessionStart::New)
        }

        pub fn matches_settings(&self, settings: &AsrSettings) -> bool {
            lock(&self.shared.control).active
                && self
                    .session
                    .as_ref()
                    .is_some_and(|(loaded, _)| loaded == settings)
        }

        /// Hide ASR without discarding its cues or completed VAD intervals.
        /// Finish an in-flight window so returning never has to transcribe it again;
        /// the worker releases native model weights after its idle timeout.
        pub fn suspend(&mut self) {
            let mut control = lock(&self.shared.control);
            if control.active && !control.stop {
                control.suspended = true;
                if control.suspended_at_position.is_none() {
                    control.suspended_at_position = Some(control.position);
                }
                self.shared.wake.notify_all();
            }
        }

        /// Explicitly reprioritize transcription when the player emits a seek event.
        /// Aborts out-of-range in-flight extraction/transcription while keeping all
        /// completed windows and cues in `CueBook`, and wakes a worker parked after
        /// repeated failures or completion.
        pub fn notify_seek(&mut self, position: f64, duration: f64) {
            if !position.is_finite() {
                return;
            }
            let mut control = lock(&self.shared.control);
            if !control.active || control.stop {
                return;
            }
            if duration.is_finite() && duration > 0.0 {
                control.duration = duration;
            }
            if !control.duration.is_finite() || control.duration <= 0.0 {
                return;
            }
            let clamped_pos = position.clamp(0.0, control.duration);
            control.position = clamped_pos;
            control.last_position = Some(clamped_pos);
            control.last_position_at = Some(Instant::now());
            if control.suspended || control.parked_no_audio {
                return;
            }
            let duration_grew =
                control.done && control.duration > control.completed_duration + SUBTITLE_TIME_DELTA;
            if control.paused_on_errors
                || duration_grew
                || (!control.done && is_outside_window(control.current_window, clamped_pos))
            {
                trigger_seek_restart(&mut control, &self.shared.wake, clamped_pos);
            }
        }

        /// Update playback position and automatically reprioritize when playback jumps
        /// beyond 2.5s outside the currently active transcription window (or while
        /// parked on errors / progressive-stream duration growth).
        pub fn update_position(&mut self, position: f64, duration: f64, paused: bool) {
            if !position.is_finite() || !duration.is_finite() || duration <= 0.0 {
                return;
            }
            let mut control = lock(&self.shared.control);
            if !control.active || control.stop {
                return;
            }
            let now = Instant::now();
            let clamped_pos = position.clamp(0.0, duration);
            let expected = match (control.last_position, control.last_position_at) {
                (Some(prev_pos), Some(prev_at)) => {
                    if paused || control.last_paused {
                        prev_pos
                    } else {
                        prev_pos + now.saturating_duration_since(prev_at).as_secs_f64()
                    }
                }
                _ => control.position,
            };
            let jumped = (clamped_pos - expected).abs() > SEEK_JUMP_THRESHOLD_SECONDS;
            control.position = clamped_pos;
            control.duration = duration;
            control.last_position = Some(clamped_pos);
            control.last_position_at = Some(now);
            control.last_paused = paused;

            if control.suspended || control.parked_no_audio || !jumped {
                return;
            }
            let duration_grew =
                control.done && control.duration > control.completed_duration + SUBTITLE_TIME_DELTA;
            if control.paused_on_errors
                || duration_grew
                || (!control.done && is_outside_window(control.current_window, clamped_pos))
            {
                trigger_seek_restart(&mut control, &self.shared.wake, clamped_pos);
            }
        }

        /// Switch the active audio track (`aid`) on a retained ASR session without
        /// reloading the model. Clears `CueBook` for the old track and restarts
        /// transcription from the current playback position, or parks/suspends the
        /// worker while keeping the model warm until idle timeout if `new_audio_id <= 0`.
        pub fn notify_audio_track_changed(&mut self, new_audio_id: i64) {
            let Some((_, media)) = self.session.as_mut() else {
                return;
            };
            if media.audio_id == new_audio_id {
                return;
            }
            media.audio_id = new_audio_id;
            let mut control = lock(&self.shared.control);
            if !control.active || control.stop {
                drop(control);
                *lock(&self.shared.latest_cues) = (u64::MAX, Vec::new());
                return;
            }
            control.generation += 1;
            let next_gen = control.generation;
            control.min_cue_generation = next_gen;
            control.audio_id = new_audio_id;
            control.reset_cues = true;
            control.paused_on_errors = false;
            control.done = false;
            control.completed_duration = 0.0;
            if let Some(abort) = &control.current_abort {
                abort.store(true, Ordering::Release);
            }
            if new_audio_id <= 0 {
                control.parked_no_audio = true;
                control.restart_position = None;
                drop(control);
                *lock(&self.shared.latest_cues) = (next_gen, Vec::new());
                self.shared.running.store(false, Ordering::Release);
                self.shared.wake.notify_all();
            } else {
                let was_suspended = control.suspended;
                control.parked_no_audio = false;
                control.restart_position = Some(control.position);
                drop(control);
                *lock(&self.shared.latest_cues) = (next_gen, Vec::new());
                if !was_suspended {
                    self.shared.running.store(true, Ordering::Release);
                }
                self.shared.wake.notify_all();
            }
        }

        /// Return a sorted snapshot of the cues currently retained in `CueBook`.
        pub fn current_cues(&self) -> Vec<AsrCue> {
            lock(&self.shared.latest_cues).1.clone()
        }

        pub fn is_running(&self) -> bool {
            self.shared.running.load(Ordering::Acquire)
        }

        /// True while an active audio track session is retained, including when
        /// another subtitle track is selected or all media intervals have already
        /// been transcribed. Reports `false` while parked on `aid <= 0` even though
        /// the background worker remains alive.
        pub fn is_active(&self) -> bool {
            let control = lock(&self.shared.control);
            control.active && !control.parked_no_audio
        }

        /// Drain worker notifications on the UI thread.
        pub fn poll(&mut self) -> Vec<AsrEvent> {
            let (generation, min_cue_generation) = {
                let control = lock(&self.shared.control);
                (control.generation, control.min_cue_generation)
            };
            let mut events = Vec::new();
            while let Ok(event) = self.receiver.try_recv() {
                // CueBook keeps cues across seeks, while track/media resets bump
                // `min_cue_generation` so pre-reset cue deltas/snapshots are dropped.
                let keep = match &event {
                    AsrEvent::CueAdded { .. }
                    | AsrEvent::CueRemoved { .. }
                    | AsrEvent::CuesSnapshot { .. } => event.generation() >= min_cue_generation,
                    _ => event.generation() == generation,
                };
                if keep {
                    events.push(event);
                }
            }
            events
        }

        /// Drain worker notifications and synchronously dispatch them to the
        /// caller, which must update Slint and the local ASR subtitle track here.
        pub fn poll_with(&mut self, mut callback: impl FnMut(AsrEvent)) {
            for event in self.poll() {
                callback(event);
            }
        }

        /// Stop the session without blocking the UI thread. The worker observes
        /// cancellation during audio extraction or Whisper inference and exits.
        pub fn cancel(&mut self) {
            self.session = None;
            *lock(&self.shared.latest_cues) = (u64::MAX, Vec::new());
            let mut control = lock(&self.shared.control);
            if !control.active && control.stop {
                return;
            }
            control.stop = true;
            control.active = false;
            if let Some(abort) = &control.current_abort {
                abort.store(true, Ordering::Release);
            }
            self.shared.running.store(false, Ordering::Release);
            self.shared.wake.notify_all();
        }

        /// Permanently stop accepting sessions, and request cancellation without
        /// waiting for an inference call or a native model destructor on the UI.
        pub fn begin_shutdown(&mut self) {
            self.closing = true;
            self.cancel();
        }

        /// Poll from the event loop until the entire worker, including native
        /// model/CUDA cleanup, has finished. `is_running()` only tracks inference
        /// and becomes false before that cleanup, so it is not a shutdown gate.
        pub fn try_finish_shutdown(&mut self) -> bool {
            self.begin_shutdown();
            if self
                .worker
                .as_ref()
                .is_some_and(|worker| !worker.is_finished())
            {
                return false;
            }
            self.join_worker();
            true
        }

        /// Final shutdown barrier for event-loop errors and controller destruction.
        /// Interactive close uses `try_finish_shutdown` to keep the UI responsive.
        pub fn shutdown(&mut self) {
            self.begin_shutdown();
            self.join_worker();
        }

        fn join_worker(&mut self) {
            if let Some(worker) = self.worker.take() {
                if worker.join().is_err() {
                    eprintln!("ASR worker panicked during shutdown");
                }
            }
        }
    }

    impl Drop for AsrController {
        fn drop(&mut self) {
            // Dropping a JoinHandle detaches the thread. Never let the process
            // unload CUDA while a detached worker is still using/freeing buffers.
            self.shutdown();
        }
    }

    fn is_outside_window(current_window: Option<(f64, f64)>, position: f64) -> bool {
        match current_window {
            Some((start, end)) => position < start - SUBTITLE_TIME_DELTA || position >= end,
            None => true,
        }
    }

    fn trigger_seek_restart(control: &mut Control, wake: &Condvar, position: f64) {
        control.generation += 1;
        control.restart_position = Some(position);
        control.paused_on_errors = false;
        control.done = false;
        control.completed_duration = 0.0;
        if let Some(abort) = &control.current_abort {
            abort.store(true, Ordering::Release);
        }
        wake.notify_all();
    }

    #[cfg(test)]
    mod tests {
        use std::sync::atomic::AtomicUsize;
        use std::time::Duration;

        use subomi_config::config::Config;

        use super::super::super::cue_book::{CueBook, Window, WindowPlan};
        use super::super::super::engines::Transcription;
        use super::super::super::worker::{
            finish_with_error, run_session_loop, wait_for_selection, SegmentContext, WindowError,
            WindowProcessor, WorkerTiming, DEFAULT_IDLE_UNLOAD_TIMEOUT,
        };
        use super::*;

        struct FnProcessor<F> {
            loaded: bool,
            f: F,
        }

        impl<F> FnProcessor<F>
        where
            F: FnMut(
                &AsrMedia,
                &Window,
                &Arc<AtomicBool>,
                &mut SegmentContext<'_>,
            ) -> Result<Transcription, String>,
        {
            fn new(f: F) -> Self {
                Self { loaded: true, f }
            }
        }

        impl<F> WindowProcessor for FnProcessor<F>
        where
            F: FnMut(
                &AsrMedia,
                &Window,
                &Arc<AtomicBool>,
                &mut SegmentContext<'_>,
            ) -> Result<Transcription, String>,
        {
            fn is_model_loaded(&self) -> bool {
                self.loaded
            }

            fn unload_model(&mut self) {
                self.loaded = false;
            }

            fn reset_media_state(&mut self) {}

            fn process_window(
                &mut self,
                media: &AsrMedia,
                window: &Window,
                abort: &Arc<AtomicBool>,
                seg_ctx: &mut SegmentContext<'_>,
            ) -> Result<Transcription, WindowError> {
                self.loaded = true;
                (self.f)(media, window, abort, seg_ctx).map_err(WindowError::Extract)
            }
        }

        fn run_loop_with_fn<F>(
            shared: Arc<Shared>,
            sender: mpsc::Sender<AsrEvent>,
            media: AsrMedia,
            initial_position: f64,
            initial_generation: u64,
            f: F,
        ) where
            F: FnMut(
                &AsrMedia,
                &Window,
                &Arc<AtomicBool>,
                &mut SegmentContext<'_>,
            ) -> Result<Transcription, String>,
        {
            let mut processor = FnProcessor::new(f);
            run_session_loop(
                shared,
                sender,
                media,
                initial_position,
                initial_generation,
                WorkerTiming::default(),
                &mut processor,
            );
        }

        fn cached_session() -> (AsrController, AsrSettings, AsrMedia) {
            let mut controller = AsrController::new();
            let mut settings =
                AsrSettings::from_config("/unused", &Config::default_empty()).unwrap();
            settings.enabled = true;
            let media = AsrMedia::new("/unused/video.mkv", 1);
            controller.session = Some((settings.clone(), media.clone()));
            {
                let mut control = lock(&controller.shared.control);
                control.active = true;
                control.duration = 90.0;
                control.media_path = media.path.clone();
                control.audio_id = media.audio_id;
            }
            (controller, settings, media)
        }

        #[test]
        fn start_rejects_non_positive_audio_id_fast() {
            let mut controller = AsrController::new();
            let mut settings =
                AsrSettings::from_config("/unused", &Config::default_empty()).unwrap();
            settings.enabled = true;
            for bad_aid in [0, -1, -42] {
                let err = controller
                    .start(
                        settings.clone(),
                        AsrMedia::new("/unused/video.mkv", bad_aid),
                        90.0,
                        0.0,
                    )
                    .unwrap_err();
                assert_eq!(err, "No active audio track");
            }
        }

        #[test]
        fn switching_away_keeps_worker_model_progress_and_queued_cues_until_resume() {
            let (mut controller, settings, media) = cached_session();
            let shared = controller.shared.clone();
            let abort = Arc::new(AtomicBool::new(false));
            lock(&shared.control).current_abort = Some(abort.clone());
            controller.suspend();
            assert!(
                !abort.load(Ordering::Acquire),
                "finish the current window instead of redoing VAD"
            );
            let (sender, receiver) = mpsc::channel();
            controller.receiver = receiver;
            let (entered_tx, entered_rx) = mpsc::channel();
            let (resumed_tx, resumed_rx) = mpsc::channel();
            let freed = Arc::new(AtomicBool::new(false));
            let worker_freed = freed.clone();
            controller.worker = Some(thread::spawn(move || {
                struct ModelLifetime(Arc<AtomicBool>);
                impl Drop for ModelLifetime {
                    fn drop(&mut self) {
                        self.0.store(true, Ordering::Release);
                    }
                }
                struct HeldModel(Option<ModelLifetime>);
                impl WindowProcessor for HeldModel {
                    fn is_model_loaded(&self) -> bool {
                        self.0.is_some()
                    }
                    fn unload_model(&mut self) {
                        self.0 = None;
                    }
                    fn reset_media_state(&mut self) {}
                    fn process_window(
                        &mut self,
                        _: &AsrMedia,
                        _: &Window,
                        _: &Arc<AtomicBool>,
                        _: &mut SegmentContext<'_>,
                    ) -> Result<Transcription, WindowError> {
                        Ok(Transcription::Success)
                    }
                }
                let mut model = HeldModel(Some(ModelLifetime(worker_freed)));
                let mut cues = CueBook::default();
                cues.mark_processed(0.0, 30.0);
                cues.add("saved".into(), 1.0, 3.0, 0, &sender);
                entered_tx.send(()).unwrap();
                assert!(wait_for_selection(
                    &shared,
                    DEFAULT_IDLE_UNLOAD_TIMEOUT,
                    &mut model
                ));
                assert_eq!(cues.cues.len(), 1);
                assert!(matches!(
                    cues.plan_window(0.0, 90.0, false),
                    WindowPlan::Extract(Window { start: 30.0, .. })
                ));
                resumed_tx.send(()).unwrap();
                let mut control = lock(&shared.control);
                while !control.stop {
                    control = shared.wake.wait(control).unwrap();
                }
            }));
            entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            assert!(controller.is_active());
            assert!(!freed.load(Ordering::Acquire));
            assert!(matches!(
                controller.poll().as_slice(),
                [AsrEvent::CueAdded { .. }]
            ));
            controller.update_position(1.5, 90.0, false);
            assert!(controller.matches_settings(&settings));
            // No model exists at the fixture path: this can only succeed by reusing
            // the original worker, rather than resolving/loading a model again.
            assert_eq!(
                controller.start(settings, media, 90.0, 1.5).unwrap(),
                SessionStart::Resumed
            );
            resumed_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            assert_eq!(lock(&controller.shared.control).generation, 0);
            assert_eq!(lock(&controller.shared.control).position, 1.5);
            assert!(!freed.load(Ordering::Acquire));
            controller.shutdown();
            assert!(freed.load(Ordering::Acquire));
        }

        #[test]
        fn resuming_suspended_session_at_new_position_jumps_generation() {
            let (mut controller, settings, media) = cached_session();
            let shared = controller.shared.clone();
            let (sender, receiver) = mpsc::channel();
            controller.receiver = receiver;
            {
                let mut control = lock(&shared.control);
                control.position = 5.0;
            }
            controller.suspend();

            let (entered_tx, entered_rx) = mpsc::channel();
            controller.worker = Some(thread::spawn(move || {
                let shared_ref = shared.clone();
                entered_tx.send(()).unwrap();
                run_loop_with_fn(
                    shared_ref,
                    sender,
                    media.clone(),
                    5.0,
                    0,
                    |_m, window, _abort, seg_ctx| {
                        seg_ctx.emit_segment(&format!("at-{:.0}", window.start), 0.5, 2.0);
                        Ok(Transcription::Success)
                    },
                );
            }));
            entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();

            // Playback moved significantly while suspended.
            controller.update_position(75.0, 90.0, false);
            assert_eq!(
                controller
                    .start(settings, AsrMedia::new("/unused/video.mkv", 1), 90.0, 75.0)
                    .unwrap(),
                SessionStart::Resumed
            );

            let control = lock(&controller.shared.control);
            assert_eq!(control.generation, 1);
            drop(control);

            // Wait until worker processes window starting at 75.0.
            let mut saw_75 = false;
            let deadline = Instant::now() + Duration::from_secs(5);
            while Instant::now() < deadline && !saw_75 {
                for event in controller.poll() {
                    if let AsrEvent::WindowStarted {
                        generation: 1,
                        start,
                        ..
                    } = event
                    {
                        if (start - 75.0).abs() < 0.01 {
                            saw_75 = true;
                        }
                    }
                }
                thread::sleep(Duration::from_millis(5));
            }
            assert!(saw_75, "resumed worker did not jump to 75.0");
            controller.shutdown();
        }

        #[test]
        fn cancellation_wakes_a_suspended_worker_and_invalidates_the_cache() {
            let (mut controller, settings, _) = cached_session();
            controller.suspend();
            let shared = controller.shared.clone();
            controller.worker = Some(thread::spawn(move || {
                let mut noop = FnProcessor::new(|_, _, _, _| Ok(Transcription::Success));
                assert!(!wait_for_selection(
                    &shared,
                    DEFAULT_IDLE_UNLOAD_TIMEOUT,
                    &mut noop
                ));
            }));
            controller.cancel();
            controller.join_worker();
            assert!(!controller.matches_settings(&settings));
            assert!(!controller.is_active());
        }

        #[test]
        fn changed_settings_or_source_cannot_resume_the_old_session() {
            for change_source in [false, true] {
                let (mut controller, mut settings, mut media) = cached_session();
                controller.suspend();
                if change_source {
                    media.audio_id = 2;
                } else {
                    settings.whisper.threads += 1;
                }
                // The new session's deliberately missing model must be validated.
                assert!(controller.start(settings, media, 90.0, 30.0).is_err());
                assert!(!controller.is_active());
                assert!(controller.session.is_none());
            }
        }

        #[test]
        fn shutdown_poll_keeps_waiting_until_worker_cleanup_finishes() {
            let mut controller = AsrController::new();
            let abort = Arc::new(AtomicBool::new(false));
            {
                let mut control = lock(&controller.shared.control);
                control.active = true;
                control.current_abort = Some(abort.clone());
            }
            let (entered_tx, entered_rx) = mpsc::channel();
            let (release_tx, release_rx) = mpsc::channel();
            let cleaned = Arc::new(AtomicBool::new(false));
            let worker_cleaned = cleaned.clone();
            let shared = controller.shared.clone();
            controller.worker = Some(thread::spawn(move || {
                let mut control = lock(&shared.control);
                while !control.stop {
                    control = shared.wake.wait(control).unwrap();
                }
                drop(control);
                entered_tx.send(()).unwrap();
                // Simulate a native destructor still freeing buffers after cancel.
                release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
                worker_cleaned.store(true, Ordering::Release);
            }));
            controller.begin_shutdown();
            entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            assert!(abort.load(Ordering::Acquire));
            assert!(!controller.is_running());
            assert!(
                !controller.try_finish_shutdown(),
                "cancel is not worker completion"
            );
            assert!(!cleaned.load(Ordering::Acquire));
            release_tx.send(()).unwrap();
            controller.shutdown();
            assert!(cleaned.load(Ordering::Acquire));
            assert!(controller.worker.is_none());
            assert!(controller.try_finish_shutdown());
        }

        #[test]
        fn dropping_controller_joins_worker_and_does_not_hold_control_lock() {
            let mut controller = AsrController::new();
            lock(&controller.shared.control).active = true;
            let shared = controller.shared.clone();
            let cleaned = Arc::new(AtomicBool::new(false));
            let worker_cleaned = cleaned.clone();
            controller.worker = Some(thread::spawn(move || {
                let mut control = lock(&shared.control);
                while !control.stop {
                    control = shared.wake.wait(control).unwrap();
                }
                // Give a detached worker time to expose the old premature exit.
                thread::sleep(Duration::from_millis(30));
                worker_cleaned.store(true, Ordering::Release);
            }));
            drop(controller);
            assert!(cleaned.load(Ordering::Acquire));
        }

        #[cfg(feature = "asr-whisper")]
        #[test]
        #[ignore = "requires SUBOMI_ASR_TEST_MODEL pointing to a local Whisper model"]
        fn shutdown_during_native_whisper_inference_releases_model() {
            use std::ffi::c_void;
            use std::path::Path;

            use subomi_asr_whisper::whisper_sys;

            use super::super::super::engines::{path_to_cstring, WhisperModel};

            struct AbortContext {
                abort: Arc<AtomicBool>,
                entered: mpsc::Sender<()>,
                signaled: bool,
            }
            unsafe extern "C" fn signal_inference(data: *mut c_void) -> bool {
                let context = &mut *data.cast::<AbortContext>();
                if !context.signaled {
                    context.signaled = true;
                    let _ = context.entered.send(());
                }
                context.abort.load(Ordering::Acquire)
            }
            let path =
                std::env::var_os("SUBOMI_ASR_TEST_MODEL").expect("set SUBOMI_ASR_TEST_MODEL");
            let path = path_to_cstring(Path::new(&path)).unwrap();
            let use_gpu = std::env::var_os("SUBOMI_ASR_TEST_GPU").is_some();
            let mut controller = AsrController::new();
            let abort = Arc::new(AtomicBool::new(false));
            {
                let mut control = lock(&controller.shared.control);
                control.active = true;
                control.current_abort = Some(abort.clone());
            }
            let (entered, receiver) = mpsc::channel();
            let released = Arc::new(AtomicBool::new(false));
            let worker_released = released.clone();
            controller.worker = Some(thread::spawn(move || unsafe {
                let mut params = whisper_sys::whisper_context_default_params();
                params.use_gpu = use_gpu;
                let model = WhisperModel(whisper_sys::whisper_init_from_file_with_params(
                    path.as_ptr(),
                    params,
                ));
                assert!(!model.0.is_null(), "test model could not be loaded");
                let mut context = AbortContext {
                    abort,
                    entered,
                    signaled: false,
                };
                let mut params = whisper_sys::whisper_full_default_params(
                    whisper_sys::whisper_sampling_strategy_WHISPER_SAMPLING_GREEDY,
                );
                params.language = c"ja".as_ptr();
                params.n_threads =
                    thread::available_parallelism().map_or(1, |count| count.get().min(4)) as i32;
                params.print_progress = false;
                params.print_realtime = false;
                params.print_timestamps = false;
                params.abort_callback = Some(signal_inference);
                params.abort_callback_user_data = (&mut context as *mut AbortContext).cast();
                let samples: Vec<f32> = (0..30 * 16000)
                    .map(|i| (i as f32 * 0.1).sin() * 0.1)
                    .collect();
                let result = whisper_sys::whisper_full(
                    model.0,
                    params,
                    samples.as_ptr(),
                    samples.len() as i32,
                );
                assert!(
                    context.abort.load(Ordering::Acquire),
                    "inference exited before cancellation"
                );
                assert_ne!(result, 0, "native inference should report cancellation");
                drop(model);
                worker_released.store(true, Ordering::Release);
            }));
            receiver
                .recv_timeout(Duration::from_secs(60))
                .expect("native inference did not begin");
            controller.begin_shutdown();
            controller.shutdown();
            assert!(
                released.load(Ordering::Acquire),
                "model must be freed before shutdown returns"
            );
        }

        #[test]
        fn seeking_inside_active_window_does_not_abort_or_bump_generation() {
            let mut controller = AsrController::new();
            let abort = Arc::new(AtomicBool::new(false));
            {
                let mut control = lock(&controller.shared.control);
                control.active = true;
                control.duration = 120.0;
                control.position = 2.0;
                control.current_window = Some((0.0, 30.0));
                control.current_abort = Some(abort.clone());
            }
            controller.update_position(15.0, 120.0, false);
            let control = lock(&controller.shared.control);
            assert_eq!(control.generation, 0);
            assert_eq!(control.position, 15.0);
            assert!(control.restart_position.is_none());
            assert!(!abort.load(Ordering::Acquire));
        }

        #[test]
        fn seeking_forward_and_backward_outside_window_aborts_and_preserves_cue_book() {
            let (mut controller, _, media) = cached_session();
            {
                let mut control = lock(&controller.shared.control);
                control.duration = 120.0;
                control.position = 0.0;
            }
            let shared = controller.shared.clone();
            let (sender, receiver) = mpsc::channel();
            controller.receiver = receiver;
            let (in_window_tx, in_window_rx) = mpsc::channel();
            let (allow_abort_tx, allow_abort_rx) = mpsc::channel::<()>();

            controller.worker = Some(thread::spawn(move || {
                let mut first_window_done = false;
                let mut gated_30_once = false;
                run_loop_with_fn(
                    shared,
                    sender,
                    media,
                    0.0,
                    0,
                    |_m, window, abort, seg_ctx| {
                        if (window.start - 0.0).abs() < 0.01 && !first_window_done {
                            first_window_done = true;
                            seg_ctx.emit_segment("window-0", 1.0, 3.0);
                            return Ok(Transcription::Success);
                        }
                        if (window.start - 30.0).abs() < 0.01 && !gated_30_once {
                            gated_30_once = true;
                            let _ = in_window_tx.send(());
                            let _ = allow_abort_rx.recv_timeout(Duration::from_secs(5));
                            while !abort.load(Ordering::Acquire) {
                                thread::sleep(Duration::from_millis(2));
                            }
                            return Ok(Transcription::Canceled);
                        }
                        seg_ctx.emit_segment(&format!("win-{:.0}", window.start), 0.5, 2.0);
                        Ok(Transcription::Success)
                    },
                );
            }));

            // Wait until worker is in window [30, 60] (having completed [0, 30]).
            in_window_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            // Seek forward to 95.0 (outside [30, 60]) via update_position discontinuity.
            controller.update_position(95.0, 120.0, false);
            let _ = allow_abort_tx.send(());

            let deadline = Instant::now() + Duration::from_secs(5);
            let mut saw_95 = false;
            while Instant::now() < deadline && !saw_95 {
                for ev in controller.poll() {
                    if let AsrEvent::CueAdded { cue, .. } = ev {
                        if cue.text == "win-95" {
                            saw_95 = true;
                        }
                    }
                }
                thread::sleep(Duration::from_millis(5));
            }
            assert!(saw_95, "seek forward did not transcribe window at 95.0");
            assert_eq!(lock(&controller.shared.control).generation, 1);

            // Seek backward to 10.0 via notify_seek: completed [0, 30] cue must still exist.
            controller.notify_seek(10.0, 120.0);
            assert_eq!(lock(&controller.shared.control).generation, 2);
            let cues = controller.current_cues();
            assert!(
                cues.iter().any(|c| c.text == "window-0"),
                "completed CueBook window should be preserved across seeks: {cues:?}"
            );
            controller.shutdown();
        }

        #[test]
        fn seeking_wakes_worker_paused_after_three_failures() {
            let (mut controller, _, media) = cached_session();
            {
                let mut control = lock(&controller.shared.control);
                control.duration = 120.0;
                control.position = 0.0;
            }
            let shared = controller.shared.clone();
            let (sender, receiver) = mpsc::channel();
            controller.receiver = receiver;

            controller.worker = Some(thread::spawn(move || {
                run_loop_with_fn(
                    shared,
                    sender,
                    media,
                    0.0,
                    0,
                    |_m, window, _abort, seg_ctx| {
                        if window.start < 50.0 {
                            Ok(Transcription::Failed("synthetic failure".into()))
                        } else {
                            seg_ctx.emit_segment("recovered", 0.5, 2.0);
                            Ok(Transcription::Success)
                        }
                    },
                );
            }));

            // Wait until worker hits Paused after MAX_TRANSCRIPTION_FAILURES.
            let deadline = Instant::now() + Duration::from_secs(5);
            let mut paused = false;
            while Instant::now() < deadline && !paused {
                for ev in controller.poll() {
                    if matches!(ev, AsrEvent::Paused { .. }) {
                        paused = true;
                    }
                }
                thread::sleep(Duration::from_millis(5));
            }
            assert!(paused, "worker did not pause after 3 failures");
            assert!(lock(&controller.shared.control).paused_on_errors);

            // Seek forward past 50.0 -> must wake worker and succeed!
            controller.update_position(65.0, 120.0, false);
            let deadline = Instant::now() + Duration::from_secs(5);
            let mut recovered = false;
            while Instant::now() < deadline && !recovered {
                for ev in controller.poll() {
                    if let AsrEvent::CueAdded { cue, .. } = ev {
                        if cue.text == "recovered" {
                            recovered = true;
                        }
                    }
                }
                thread::sleep(Duration::from_millis(5));
            }
            assert!(recovered, "seek did not wake worker from Paused");
            assert_eq!(lock(&controller.shared.control).generation, 1);
            controller.shutdown();
        }

        #[test]
        fn seeking_wakes_worker_from_done_state() {
            let (mut controller, _, media) = cached_session();
            {
                let mut control = lock(&controller.shared.control);
                control.duration = 25.0;
                control.position = 0.0;
            }
            let shared = controller.shared.clone();
            let (sender, receiver) = mpsc::channel();
            controller.receiver = receiver;

            controller.worker = Some(thread::spawn(move || {
                run_loop_with_fn(
                    shared,
                    sender,
                    media,
                    0.0,
                    0,
                    |_m, window, _abort, seg_ctx| {
                        seg_ctx.emit_segment(&format!("cue-{:.0}", window.start), 0.5, 2.0);
                        Ok(Transcription::Success)
                    },
                );
            }));

            // Wait until initial 25s media completes -> Done state.
            let deadline = Instant::now() + Duration::from_secs(5);
            let mut completed = false;
            while Instant::now() < deadline && !completed {
                for ev in controller.poll() {
                    if matches!(ev, AsrEvent::Completed { generation: 0, .. }) {
                        completed = true;
                    }
                }
                thread::sleep(Duration::from_millis(5));
            }
            assert!(completed, "worker did not reach Done state");

            // Seeking inside an already-completed file with unchanged duration must NOT
            // wake the worker or re-emit `AsrEvent::Completed` (toast spam on seek).
            controller.notify_seek(10.0, 25.0);
            controller.update_position(20.0, 25.0, false);
            thread::sleep(Duration::from_millis(40));
            let stray_events = controller.poll();
            assert!(
                !stray_events
                    .iter()
                    .any(|ev| matches!(ev, AsrEvent::Completed { .. } | AsrEvent::Started { .. })),
                "same-duration seek after completion must not re-trigger Started/Completed: {stray_events:?}"
            );

            // Duration grows (e.g. progressive stream) and user seeks to 40.0.
            controller.notify_seek(40.0, 80.0);
            let deadline = Instant::now() + Duration::from_secs(5);
            let mut woke_with_new_gen = false;
            while Instant::now() < deadline && !woke_with_new_gen {
                for ev in controller.poll() {
                    if let AsrEvent::CueAdded { generation: 1, cue } = ev {
                        if cue.text == "cue-40" {
                            woke_with_new_gen = true;
                        }
                    }
                }
                thread::sleep(Duration::from_millis(5));
            }
            assert!(
                woke_with_new_gen,
                "seek did not wake worker from Done state"
            );
            controller.shutdown();
        }

        #[test]
        fn audio_track_switch_clears_cue_book_and_uses_new_audio_id() {
            let (mut controller, _, media) = cached_session();
            {
                let mut control = lock(&controller.shared.control);
                control.duration = 30.0;
                control.position = 0.0;
            }
            let shared = controller.shared.clone();
            let (sender, receiver) = mpsc::channel();
            controller.receiver = receiver;

            controller.worker = Some(thread::spawn(move || {
                run_loop_with_fn(
                    shared,
                    sender,
                    media,
                    0.0,
                    0,
                    |m, _window, _abort, seg_ctx| {
                        seg_ctx.emit_segment(&format!("aid-{}", m.audio_id), 0.5, 2.0);
                        Ok(Transcription::Success)
                    },
                );
            }));

            let deadline = Instant::now() + Duration::from_secs(5);
            let mut saw_aid1 = false;
            while Instant::now() < deadline && !saw_aid1 {
                for ev in controller.poll() {
                    match ev {
                        AsrEvent::CueAdded { cue, .. } if cue.text == "aid-1" => saw_aid1 = true,
                        AsrEvent::CuesSnapshot { cues, .. } if !cues.is_empty() => {
                            panic!("streaming must not emit non-empty CuesSnapshot per segment");
                        }
                        _ => {}
                    }
                }
                thread::sleep(Duration::from_millis(5));
            }
            assert!(saw_aid1);

            // Switch audio track to aid = 2: clear_all emits CueRemoved + empty CuesSnapshot,
            // followed by incremental CueAdded for aid-2 (no per-segment CuesSnapshot).
            controller.notify_audio_track_changed(2);
            let deadline = Instant::now() + Duration::from_secs(5);
            let mut saw_removed = false;
            let mut saw_clear_snapshot = false;
            let mut saw_aid2 = false;
            while Instant::now() < deadline && !(saw_removed && saw_clear_snapshot && saw_aid2) {
                for ev in controller.poll() {
                    match ev {
                        AsrEvent::CueRemoved { .. } => saw_removed = true,
                        AsrEvent::CuesSnapshot { cues, .. } if cues.is_empty() => {
                            saw_clear_snapshot = true;
                        }
                        AsrEvent::CuesSnapshot { cues, .. } => {
                            panic!(
                                "streaming aid-2 must not emit non-empty CuesSnapshot: {cues:?}"
                            );
                        }
                        AsrEvent::CueAdded { cue, .. } if cue.text == "aid-2" => saw_aid2 = true,
                        _ => {}
                    }
                }
                thread::sleep(Duration::from_millis(5));
            }
            assert!(saw_removed && saw_clear_snapshot && saw_aid2);
            assert_eq!(controller.current_cues().len(), 1);
            assert_eq!(controller.current_cues()[0].text, "aid-2");

            // Switching to audio_id <= 0 parks the worker while keeping the model warm,
            // reports `!is_active()` for disabled audio, and clears `latest_cues` synchronously.
            controller.notify_audio_track_changed(0);
            assert!(
                !controller.is_active(),
                "is_active() must be false when aid <= 0"
            );
            assert!(
                lock(&controller.shared.control).active,
                "underlying worker/model must stay warm when aid <= 0"
            );
            assert!(!controller.is_running());
            assert!(controller.current_cues().is_empty());

            // Cycling back to audio_id = 1 restores `is_active()` & `is_running()` synchronously
            // and resumes transcription immediately without model reload.
            controller.notify_audio_track_changed(1);
            assert!(controller.is_active());
            assert!(controller.is_running());
            let deadline = Instant::now() + Duration::from_secs(5);
            let mut resumed_aid1 = false;
            while Instant::now() < deadline && !resumed_aid1 {
                for ev in controller.poll() {
                    if let AsrEvent::CueAdded { cue, .. } = ev {
                        if cue.text == "aid-1" {
                            resumed_aid1 = true;
                        }
                    }
                }
                thread::sleep(Duration::from_millis(5));
            }
            assert!(
                resumed_aid1,
                "cycling back to aid = 1 must resume warm worker"
            );
            controller.shutdown();
        }

        #[test]
        fn start_retargets_live_worker_across_media_without_reloading_model() {
            let (mut controller, settings, media1) = cached_session();
            {
                let mut control = lock(&controller.shared.control);
                control.duration = 60.0;
                control.position = 0.0;
            }
            let shared = controller.shared.clone();
            let (sender, receiver) = mpsc::channel();
            controller.receiver = receiver;
            let model_dropped = Arc::new(AtomicBool::new(false));
            let worker_dropped = model_dropped.clone();

            controller.worker = Some(thread::spawn(move || {
                struct ModelGuard(Arc<AtomicBool>);
                impl Drop for ModelGuard {
                    fn drop(&mut self) {
                        self.0.store(true, Ordering::Release);
                    }
                }
                let _model = ModelGuard(worker_dropped);
                run_loop_with_fn(
                    shared,
                    sender,
                    media1,
                    0.0,
                    0,
                    |m, window, _abort, seg_ctx| {
                        seg_ctx.emit_segment(
                            &format!("{}:aid{}:{:.0}", m.path, m.audio_id, window.start),
                            0.5,
                            2.0,
                        );
                        Ok(Transcription::Success)
                    },
                );
            }));

            // Wait for first file cue.
            let deadline = Instant::now() + Duration::from_secs(5);
            while controller.current_cues().is_empty() && Instant::now() < deadline {
                let _ = controller.poll();
                thread::sleep(Duration::from_millis(5));
            }
            assert_eq!(
                controller.current_cues()[0].text,
                "/unused/video.mkv:aid1:0"
            );

            // Retarget to a new playlist file (`episode2.mkv`, `aid = 3`) with identical settings.
            // Because `/unused` has no model files on disk, `start()` can ONLY succeed by retargeting
            // the live worker in-place without calling `resolve_model` / `load_runtime_model`!
            let media2 = AsrMedia::new("/unused/episode2.mkv", 3);
            let outcome = controller
                .start(settings.clone(), media2, 120.0, 42.0)
                .expect("media retarget must succeed in 0ms without model reload");
            assert_eq!(outcome, SessionStart::New);
            assert!(
                controller.current_cues().is_empty(),
                "old file cues must clear immediately"
            );
            assert!(
                !model_dropped.load(Ordering::Acquire),
                "model must stay warm in RAM"
            );

            let deadline = Instant::now() + Duration::from_secs(5);
            let mut saw_ep2 = false;
            while Instant::now() < deadline && !saw_ep2 {
                for ev in controller.poll() {
                    if let AsrEvent::CueAdded { cue, .. } = ev {
                        assert!(
                            !cue.text.contains("video.mkv"),
                            "pre-retarget cue leaked across min_cue_generation: {cue:?}"
                        );
                        if cue.text == "/unused/episode2.mkv:aid3:42" {
                            saw_ep2 = true;
                        }
                    }
                }
                thread::sleep(Duration::from_millis(5));
            }
            assert!(
                saw_ep2,
                "retargeted worker did not transcribe episode2.mkv at 42s"
            );
            controller.shutdown();
            assert!(model_dropped.load(Ordering::Acquire));
        }

        #[cfg(feature = "asr-whisper")]
        #[test]
        fn start_hands_stopping_previous_worker_to_new_thread_instead_of_failing_still_stopping() {
            use super::super::super::AsrBackend;

            let dir = tempfile::TempDir::new().unwrap();
            let fake_model = dir.path().join("ggml-tiny.bin");
            std::fs::write(&fake_model, b"invalid-whisper-header").unwrap();

            let (mut controller, mut settings, media) = cached_session();
            settings.backend = AsrBackend::Whisper;
            settings.whisper.model = "custom".into();
            settings.whisper.custom_model = fake_model.to_string_lossy().into_owned();
            settings.whisper.vad_enabled = false;
            // Differ from `cached_session()` settings so `start()` takes the full reload path.
            settings.whisper.threads += 1;

            let prev_shared = controller.shared.clone();
            let prev_finished_cleanup = Arc::new(AtomicBool::new(false));
            let prev_cleanup_flag = prev_finished_cleanup.clone();
            controller.worker = Some(thread::spawn(move || {
                let mut control = lock(&prev_shared.control);
                while !control.stop {
                    control = prev_shared.wake.wait(control).unwrap();
                }
                drop(control);
                // Simulate slow native/CUDA destructor after `cancel()`.
                thread::sleep(Duration::from_millis(80));
                prev_cleanup_flag.store(true, Ordering::Release);
            }));

            // Must NOT return `Err("The previous ASR session is still stopping.")`!
            let result = controller.start(settings, media, 60.0, 0.0).expect(
                "start() must hand off stopping JoinHandle instead of failing on UI thread",
            );
            assert_eq!(result, SessionStart::New);

            // Shutting down the new session must wait until `prev_worker` also finished cleanup.
            controller.shutdown();
            assert!(prev_finished_cleanup.load(Ordering::Acquire));
        }

        #[test]
        fn min_cue_generation_filters_stale_pre_reset_cues_in_poll() {
            let mut controller = AsrController::new();
            let (sender, receiver) = mpsc::channel();
            controller.receiver = receiver;
            {
                let mut control = lock(&controller.shared.control);
                control.generation = 2;
                control.min_cue_generation = 2;
            }
            sender
                .send(AsrEvent::CueAdded {
                    generation: 1,
                    cue: AsrCue {
                        id: 1,
                        text: "stale track cue".into(),
                        start: 1.0,
                        end: 2.0,
                    },
                })
                .unwrap();
            sender
                .send(AsrEvent::CuesSnapshot {
                    generation: 1,
                    cues: vec![AsrCue {
                        id: 1,
                        text: "stale track cue".into(),
                        start: 1.0,
                        end: 2.0,
                    }],
                })
                .unwrap();
            sender
                .send(AsrEvent::CueAdded {
                    generation: 2,
                    cue: AsrCue {
                        id: 1,
                        text: "fresh track cue".into(),
                        start: 1.0,
                        end: 2.0,
                    },
                })
                .unwrap();
            let events = controller.poll();
            assert_eq!(events.len(), 1);
            assert!(
                matches!(&events[0], AsrEvent::CueAdded { generation: 2, cue } if cue.text == "fresh track cue")
            );
        }

        #[test]
        fn queued_cues_survive_seek_generation_change() {
            let mut controller = AsrController::new();
            let (sender, receiver) = mpsc::channel();
            controller.receiver = receiver;
            lock(&controller.shared.control).generation = 1;
            sender
                .send(AsrEvent::CueAdded {
                    generation: 0,
                    cue: AsrCue {
                        id: 1,
                        text: "before seek".into(),
                        start: 1.0,
                        end: 2.0,
                    },
                })
                .unwrap();
            sender
                .send(AsrEvent::CuesSnapshot {
                    generation: 0,
                    cues: vec![AsrCue {
                        id: 1,
                        text: "before seek".into(),
                        start: 1.0,
                        end: 2.0,
                    }],
                })
                .unwrap();
            sender
                .send(AsrEvent::Completed {
                    generation: 0,
                    cue_count: 1,
                })
                .unwrap();
            let events = controller.poll();
            assert_eq!(events.len(), 2);
            assert!(
                matches!(&events[0], AsrEvent::CueAdded { cue, .. } if cue.text == "before seek")
            );
            assert!(matches!(&events[1], AsrEvent::CuesSnapshot { cues, .. } if cues.len() == 1));
        }

        #[test]
        fn idle_unload_reclaims_model_in_done_state_and_reloads_only_for_uncovered_windows() {
            struct ReloadTracker {
                loaded: Arc<AtomicBool>,
                loads: Arc<AtomicUsize>,
                unloads: Arc<AtomicUsize>,
                resets: Arc<AtomicUsize>,
            }
            impl WindowProcessor for ReloadTracker {
                fn is_model_loaded(&self) -> bool {
                    self.loaded.load(Ordering::Acquire)
                }
                fn unload_model(&mut self) {
                    if self.loaded.swap(false, Ordering::AcqRel) {
                        self.unloads.fetch_add(1, Ordering::AcqRel);
                    }
                }
                fn reset_media_state(&mut self) {
                    self.resets.fetch_add(1, Ordering::AcqRel);
                }
                fn process_window(
                    &mut self,
                    _media: &AsrMedia,
                    window: &Window,
                    _abort: &Arc<AtomicBool>,
                    seg_ctx: &mut SegmentContext<'_>,
                ) -> Result<Transcription, WindowError> {
                    if !self.loaded.swap(true, Ordering::AcqRel) {
                        self.loads.fetch_add(1, Ordering::AcqRel);
                    }
                    seg_ctx.emit_segment(&format!("win-{:.0}", window.start), 0.5, 2.0);
                    Ok(Transcription::Success)
                }
            }

            let (mut controller, settings, media) = cached_session();
            {
                let mut control = lock(&controller.shared.control);
                control.duration = 25.0;
                control.position = 0.0;
            }
            let shared = controller.shared.clone();
            let (sender, receiver) = mpsc::channel();
            controller.receiver = receiver;

            let loaded = Arc::new(AtomicBool::new(true));
            let loads = Arc::new(AtomicUsize::new(1));
            let unloads = Arc::new(AtomicUsize::new(0));
            let resets = Arc::new(AtomicUsize::new(0));
            let mut tracker = ReloadTracker {
                loaded: loaded.clone(),
                loads: loads.clone(),
                unloads: unloads.clone(),
                resets: resets.clone(),
            };
            let media_worker = media.clone();
            controller.worker = Some(thread::spawn(move || {
                run_session_loop(
                    shared,
                    sender,
                    media_worker,
                    0.0,
                    0,
                    WorkerTiming {
                        idle_unload_timeout: Duration::from_millis(40),
                        retry_backoff: Duration::from_millis(5),
                    },
                    &mut tracker,
                );
            }));

            // Wait until initial window finishes AND idle unload frees the model.
            let deadline = Instant::now() + Duration::from_secs(5);
            while Instant::now() < deadline && unloads.load(Ordering::Acquire) == 0 {
                let _ = controller.poll();
                thread::sleep(Duration::from_millis(5));
            }
            assert_eq!(unloads.load(Ordering::Acquire), 1);
            assert!(!loaded.load(Ordering::Acquire));
            assert_eq!(controller.current_cues().len(), 1);
            assert_eq!(controller.current_cues()[0].text, "win-0");

            // Suspend and resume on already-covered media: must emit CuesSnapshot and
            // Completed WITHOUT reloading the model!
            controller.suspend();
            assert_eq!(
                controller
                    .start(settings, media, 25.0, 10.0)
                    .expect("resume on cached cues"),
                SessionStart::Resumed
            );
            let deadline = Instant::now() + Duration::from_secs(5);
            let mut saw_resume_snapshot = false;
            let mut saw_resume_completed = false;
            while Instant::now() < deadline && !(saw_resume_snapshot && saw_resume_completed) {
                for ev in controller.poll() {
                    match ev {
                        AsrEvent::CuesSnapshot { cues, .. } if cues.len() == 1 => {
                            saw_resume_snapshot = true;
                        }
                        AsrEvent::Completed { generation: 1, .. } => {
                            saw_resume_completed = true;
                        }
                        _ => {}
                    }
                }
                thread::sleep(Duration::from_millis(5));
            }
            assert!(saw_resume_snapshot && saw_resume_completed);
            assert_eq!(
                loads.load(Ordering::Acquire),
                1,
                "resuming already-covered media must NOT reload the unloaded model"
            );
            assert!(!loaded.load(Ordering::Acquire));

            // Seeking to newly uncovered duration transparently reloads the model.
            controller.notify_seek(40.0, 80.0);
            let deadline = Instant::now() + Duration::from_secs(5);
            let mut saw_40 = false;
            while Instant::now() < deadline && !saw_40 {
                for ev in controller.poll() {
                    if let AsrEvent::CueAdded { cue, .. } = ev {
                        if cue.text == "win-40" {
                            saw_40 = true;
                        }
                    }
                }
                thread::sleep(Duration::from_millis(5));
            }
            assert!(
                saw_40,
                "uncovered seek window must transparently reload model"
            );
            assert_eq!(loads.load(Ordering::Acquire), 2);

            // Switching audio track resets VAD/media state and clears CueBook.
            controller.notify_audio_track_changed(2);
            let deadline = Instant::now() + Duration::from_secs(5);
            while Instant::now() < deadline && resets.load(Ordering::Acquire) == 0 {
                let _ = controller.poll();
                thread::sleep(Duration::from_millis(5));
            }
            assert!(resets.load(Ordering::Acquire) >= 1);
            controller.shutdown();
        }

        #[test]
        fn idle_unload_reclaims_model_when_suspended_and_parked_and_shutdown_exits_promptly() {
            struct SuspendTracker {
                loaded: Arc<AtomicBool>,
                loads: Arc<AtomicUsize>,
                unloads: Arc<AtomicUsize>,
            }
            impl WindowProcessor for SuspendTracker {
                fn is_model_loaded(&self) -> bool {
                    self.loaded.load(Ordering::Acquire)
                }
                fn unload_model(&mut self) {
                    if self.loaded.swap(false, Ordering::AcqRel) {
                        self.unloads.fetch_add(1, Ordering::AcqRel);
                    }
                }
                fn reset_media_state(&mut self) {}
                fn process_window(
                    &mut self,
                    media: &AsrMedia,
                    window: &Window,
                    _abort: &Arc<AtomicBool>,
                    seg_ctx: &mut SegmentContext<'_>,
                ) -> Result<Transcription, WindowError> {
                    if !self.loaded.swap(true, Ordering::AcqRel) {
                        self.loads.fetch_add(1, Ordering::AcqRel);
                    }
                    seg_ctx.emit_segment(
                        &format!("aid{}:{:.0}", media.audio_id, window.start),
                        0.5,
                        2.0,
                    );
                    Ok(Transcription::Success)
                }
            }

            let (mut controller, settings, media) = cached_session();
            {
                let mut control = lock(&controller.shared.control);
                control.duration = 90.0;
                control.position = 0.0;
            }
            controller.suspend();
            let shared = controller.shared.clone();
            let (sender, receiver) = mpsc::channel();
            controller.receiver = receiver;

            let loaded = Arc::new(AtomicBool::new(true));
            let loads = Arc::new(AtomicUsize::new(1));
            let unloads = Arc::new(AtomicUsize::new(0));
            let mut tracker = SuspendTracker {
                loaded: loaded.clone(),
                loads: loads.clone(),
                unloads: unloads.clone(),
            };
            let media_worker = media.clone();
            controller.worker = Some(thread::spawn(move || {
                run_session_loop(
                    shared,
                    sender,
                    media_worker,
                    0.0,
                    0,
                    WorkerTiming {
                        idle_unload_timeout: Duration::from_millis(40),
                        retry_backoff: Duration::from_millis(5),
                    },
                    &mut tracker,
                );
            }));

            // Suspended worker unloads model after idle timeout.
            let deadline = Instant::now() + Duration::from_secs(5);
            while Instant::now() < deadline && unloads.load(Ordering::Acquire) == 0 {
                thread::sleep(Duration::from_millis(5));
            }
            assert_eq!(unloads.load(Ordering::Acquire), 1);
            assert!(!loaded.load(Ordering::Acquire));

            // Resuming with uncovered media transparently reloads and transcribes.
            assert_eq!(
                controller.start(settings, media, 90.0, 0.0).unwrap(),
                SessionStart::Resumed
            );
            let deadline = Instant::now() + Duration::from_secs(5);
            let mut saw_aid1 = false;
            while Instant::now() < deadline && !saw_aid1 {
                for ev in controller.poll() {
                    if let AsrEvent::CueAdded { cue, .. } = ev {
                        if cue.text == "aid1:0" {
                            saw_aid1 = true;
                        }
                    }
                }
                thread::sleep(Duration::from_millis(5));
            }
            assert!(saw_aid1);
            assert_eq!(loads.load(Ordering::Acquire), 2);

            // Parking on aid <= 0 also unloads after idle timeout.
            controller.notify_audio_track_changed(0);
            let deadline = Instant::now() + Duration::from_secs(5);
            while Instant::now() < deadline && unloads.load(Ordering::Acquire) < 2 {
                let _ = controller.poll();
                thread::sleep(Duration::from_millis(5));
            }
            assert!(unloads.load(Ordering::Acquire) >= 2);
            assert!(!loaded.load(Ordering::Acquire));
            controller.shutdown();

            // Verify shutdown while waiting on a long (60s) idle timeout exits immediately.
            let (mut controller2, _, media2) = cached_session();
            controller2.suspend();
            let shared2 = controller2.shared.clone();
            let (sender2, receiver2) = mpsc::channel();
            controller2.receiver = receiver2;
            controller2.worker = Some(thread::spawn(move || {
                let mut processor = FnProcessor::new(|_, _, _, _| Ok(Transcription::Success));
                run_session_loop(
                    shared2,
                    sender2,
                    media2,
                    0.0,
                    0,
                    WorkerTiming {
                        idle_unload_timeout: Duration::from_secs(60),
                        retry_backoff: Duration::from_millis(5),
                    },
                    &mut processor,
                );
            }));
            thread::sleep(Duration::from_millis(20));
            let shutdown_start = Instant::now();
            controller2.shutdown();
            assert!(
                shutdown_start.elapsed() < Duration::from_secs(2),
                "shutdown must wake idle wait immediately instead of waiting 60s"
            );
        }

        #[test]
        fn finish_with_error_uses_latest_generation_so_failures_are_not_dropped() {
            let mut controller = AsrController::new();
            let (sender, receiver) = mpsc::channel();
            controller.receiver = receiver;
            {
                let mut control = lock(&controller.shared.control);
                control.active = true;
                // Simulate a track/file switch that bumped generation during model load.
                control.generation = 3;
            }
            finish_with_error(&controller.shared, &sender, "corrupt model".into());
            let events = controller.poll();
            assert_eq!(events.len(), 1);
            assert!(matches!(
                &events[0],
                AsrEvent::Failed { generation: 3, message } if message == "corrupt model"
            ));
            assert!(!controller.is_active());
        }

        #[test]
        fn transient_failure_retry_backoff_aborts_immediately_on_cancel() {
            let (mut controller, _, media) = cached_session();
            {
                let mut control = lock(&controller.shared.control);
                control.duration = 60.0;
                control.position = 0.0;
            }
            let shared = controller.shared.clone();
            let (sender, receiver) = mpsc::channel();
            controller.receiver = receiver;
            let (failed_once_tx, failed_once_rx) = mpsc::channel();

            controller.worker = Some(thread::spawn(move || {
                let mut sent = false;
                let mut processor = FnProcessor::new(move |_, _, _, _| {
                    if !sent {
                        sent = true;
                        let _ = failed_once_tx.send(());
                    }
                    Ok(Transcription::Failed("transient".into()))
                });
                run_session_loop(
                    shared,
                    sender,
                    media,
                    0.0,
                    0,
                    WorkerTiming {
                        idle_unload_timeout: Duration::from_secs(60),
                        retry_backoff: Duration::from_secs(10),
                    },
                    &mut processor,
                );
            }));

            failed_once_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            // Give worker a moment to enter the 10s retry backoff wait.
            thread::sleep(Duration::from_millis(20));
            let start = Instant::now();
            controller.shutdown();
            assert!(
                start.elapsed() < Duration::from_secs(2),
                "shutdown must abort retry backoff immediately instead of waiting 10s"
            );
        }
    }
}

#[cfg(not(any(feature = "asr-whisper", feature = "asr-onnx")))]
mod imp {
    use crate::controller::media::MediaContext as AsrMedia;

    use super::super::{AsrCue, AsrEvent, AsrSettings, SessionStart};

    #[derive(Default)]
    pub struct AsrController;

    impl AsrController {
        pub fn new() -> Self {
            Self
        }
        pub fn start(
            &mut self,
            _: AsrSettings,
            _: AsrMedia,
            _: f64,
            _: f64,
        ) -> Result<SessionStart, String> {
            Err("Speech recognition is not included in this build.".into())
        }
        pub fn suspend(&mut self) {}
        pub fn matches_settings(&self, _: &AsrSettings) -> bool {
            false
        }
        pub fn cancel(&mut self) {}
        pub fn begin_shutdown(&mut self) {}
        pub fn try_finish_shutdown(&mut self) -> bool {
            true
        }
        pub fn shutdown(&mut self) {}
        pub fn update_position(&mut self, _: f64, _: f64, _: bool) {}
        pub fn notify_seek(&mut self, _: f64, _: f64) {}
        pub fn notify_audio_track_changed(&mut self, _: i64) {}
        pub fn current_cues(&self) -> Vec<AsrCue> {
            Vec::new()
        }
        pub fn poll_with(&mut self, _: impl FnMut(AsrEvent)) {}
        pub fn poll(&mut self) -> Vec<AsrEvent> {
            Vec::new()
        }
        pub fn is_active(&self) -> bool {
            false
        }
        pub fn is_running(&self) -> bool {
            false
        }
    }
}

pub use imp::AsrController;

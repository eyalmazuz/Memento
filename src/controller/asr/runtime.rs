//! Optional background transcription worker.
use super::*;
use crate::controller::media::extract_audio_samples;
#[cfg(feature = "asr-whisper")]
use std::ffi::CString;
#[cfg(feature = "asr-whisper")]
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Instant;

use memento_asr::asrcontroller::{adjust_segment, is_cue_valid};
#[cfg(feature = "asr-onnx")]
use memento_asr::onnx::{onnxcontroller, onnxmodelconfig::OnnxModelConfig};
#[cfg(feature = "asr-whisper")]
use memento_asr::whisper::whispercontroller;
#[cfg(feature = "asr-onnx")]
use memento_asr_onnx::engine as onnx_engine;
#[cfg(feature = "asr-whisper")]
use memento_asr_whisper::{engine as whisper_engine, whisper_sys};

const WORK_WINDOW_SECONDS: f64 = 30.0;
const MIN_WORK_WINDOW_SECONDS: f64 = 0.25;
const SEEK_JUMP_THRESHOLD_SECONDS: f64 = 2.5;
const SUBTITLE_TIME_DELTA: f64 = 0.0001;
const MAX_TRANSCRIPTION_FAILURES: u32 = 3;
// The app targets the same ONNX Runtime 1.22.0 C API as the Rust engine port.
#[cfg(feature = "asr-onnx")]
const ORT_API_VERSION: u32 = 22;

#[derive(Default)]
struct Control {
    stop: bool,
    active: bool,
    suspended: bool,
    parked_no_audio: bool,
    paused_on_errors: bool,
    done: bool,
    reset_cues: bool,
    emit_snapshot: bool,
    media_path: String,
    audio_id: i64,
    generation: u64,
    min_cue_generation: u64,
    duration: f64,
    position: f64,
    last_position: Option<f64>,
    last_position_at: Option<Instant>,
    last_paused: bool,
    suspended_at_position: Option<f64>,
    current_window: Option<(f64, f64)>,
    restart_position: Option<f64>,
    current_abort: Option<Arc<AtomicBool>>,
}

struct Shared {
    control: Mutex<Control>,
    latest_cues: Mutex<(u64, Vec<AsrCue>)>,
    wake: Condvar,
    running: AtomicBool,
}

struct JoinOnDrop(Option<JoinHandle<()>>);

impl JoinOnDrop {
    fn join_now(&mut self) {
        if let Some(handle) = self.0.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for JoinOnDrop {
    fn drop(&mut self) {
        self.join_now();
    }
}

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
                control.restart_position = Some(initial_position);
                if let Some(abort) = &control.current_abort {
                    abort.store(true, Ordering::Release);
                }
                drop(control);
                *lock(&self.shared.latest_cues) = (next_gen, Vec::new());
                self.shared.wake.notify_all();
                self.session = Some((settings, media));
                return Ok(SessionStart::New);
            }
        }

        if self.is_active() {
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
                .name("memento-asr".into())
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
        self.is_active()
            && self
                .session
                .as_ref()
                .is_some_and(|(loaded, _)| loaded == settings)
    }

    /// Hide ASR without discarding its model, cues or completed VAD intervals.
    /// Finish an in-flight window so returning never has to transcribe it again.
    pub fn suspend(&mut self) {
        let mut control = lock(&self.shared.control);
        if control.active && !control.stop {
            control.suspended = true;
            if control.suspended_at_position.is_none() {
                control.suspended_at_position = Some(control.position);
            }
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
        if control.paused_on_errors
            || control.done
            || is_outside_window(control.current_window, clamped_pos)
        {
            trigger_seek_restart(&mut control, &self.shared.wake, clamped_pos);
        }
    }

    /// Update playback position and automatically reprioritize when playback jumps
    /// beyond 2.5s outside the currently active transcription window (or while
    /// parked on errors / completion).
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
        if control.paused_on_errors
            || control.done
            || is_outside_window(control.current_window, clamped_pos)
        {
            trigger_seek_restart(&mut control, &self.shared.wake, clamped_pos);
        }
    }

    /// Switch the active audio track (`aid`) on a retained ASR session without
    /// reloading the model. Clears `CueBook` for the old track and restarts
    /// transcription from the current playback position, or parks/suspends the
    /// worker while keeping the model warm if `new_audio_id <= 0`.
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
            control.parked_no_audio = false;
            control.restart_position = Some(control.position);
            drop(control);
            *lock(&self.shared.latest_cues) = (next_gen, Vec::new());
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

    /// True while a session is retained, including when another subtitle track
    /// is selected or all media intervals have already been transcribed.
    pub fn is_active(&self) -> bool {
        lock(&self.shared.control).active
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
        if self.worker.as_ref().is_some_and(|worker| !worker.is_finished()) {
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
    if let Some(abort) = &control.current_abort {
        abort.store(true, Ordering::Release);
    }
    wake.notify_all();
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[derive(Clone)]
enum ResolvedModel {
    #[cfg(feature = "asr-whisper")]
    Whisper { path: PathBuf, vad_path: PathBuf },
    #[cfg(feature = "asr-onnx")]
    Onnx { path: PathBuf },
}

fn resolve_model(settings: &AsrSettings) -> Result<ResolvedModel, String> {
    let config_dir = settings.config_dir.to_string_lossy();
    match settings.backend {
        #[cfg(feature = "asr-whisper")]
        AsrBackend::Whisper => {
            let selected = if settings.whisper.model.trim() == "custom" {
                settings.whisper.custom_model.trim().to_owned()
            } else {
                whispercontroller::model_filename(settings.whisper.model.trim())
            };
            if selected.is_empty() {
                return Err("No Whisper model is selected. Choose a model in ASR settings.".into());
            }
            let resolved = PathBuf::from(whispercontroller::resolve_model_path(
                &selected,
                &[whispercontroller::MODEL_SUFFIX],
                &config_dir,
            ));
            if !resolved.is_file() {
                return Err(format!(
                    "Whisper model was not found at '{}'. Download the selected model into '{}' or choose an existing model file in Whisper settings.",
                    resolved.display(),
                    whispercontroller::models_directory(&config_dir),
                ));
            }
            let vad_path = if settings.whisper.vad_enabled {
                let resolved = PathBuf::from(whispercontroller::resolve_model_path(
                    settings.whisper.vad_model.trim(),
                    &[whispercontroller::MODEL_SUFFIX],
                    &config_dir,
                ));
                if !resolved.is_file() {
                    return Err(format!(
                        "Whisper VAD model was not found at '{}'. Download that VAD model or disable VAD in Whisper settings.",
                        resolved.display(),
                    ));
                }
                resolved
            } else {
                PathBuf::new()
            };
            Ok(ResolvedModel::Whisper {
                path: resolved,
                vad_path,
            })
        }
        #[cfg(feature = "asr-onnx")]
        AsrBackend::Onnx => {
            let selected = if settings.onnx.model == onnxcontroller::MODEL_CUSTOM {
                settings.onnx.custom_model.trim().to_owned()
            } else {
                onnxcontroller::model_directory(settings.onnx.model.trim(), &config_dir)
            };
            if selected.is_empty() {
                return Err(
                    "No ONNX model is selected. Choose a model directory in ONNX settings.".into(),
                );
            }
            let path = PathBuf::from(selected);
            let (config, error) = OnnxModelConfig::load_from_path(&path);
            if !config.is_valid() {
                let detail = if error.is_empty() {
                    "the directory is incomplete or has no supported model configuration".to_owned()
                } else {
                    error
                };
                return Err(format!(
                    "ONNX model is unavailable at '{}': {detail}. Download the selected model or choose a valid ONNX model directory.",
                    path.display(),
                ));
            }
            Ok(ResolvedModel::Onnx { path })
        }
        #[cfg(not(all(feature = "asr-whisper", feature = "asr-onnx")))]
        _ => Err("The selected speech backend is not included in this build.".into()),
    }
}

#[derive(Clone)]
struct Window {
    start: f64,
    end: f64,
    rewound: bool,
}

enum WindowPlan {
    Done,
    Advance { start: f64, rewound: bool },
    Extract(Window),
}

#[derive(Default)]
struct CueBook {
    cues: Vec<AsrCue>,
    next_id: u64,
    processed: Vec<(f64, f64)>,
}

impl CueBook {
    fn skip_covered_position(&self, mut position: f64) -> f64 {
        loop {
            if let Some((_, end)) = self
                .processed
                .iter()
                .find(|(start, end)| *start <= position + SUBTITLE_TIME_DELTA && position < *end)
            {
                position = *end;
                continue;
            }
            let Some(cue) = self
                .cues
                .iter()
                .find(|cue| cue.start <= position + SUBTITLE_TIME_DELTA && position < cue.end)
            else {
                return position;
            };
            position = cue.end;
        }
    }

    fn next_subtitle_start(&self, position: f64) -> f64 {
        let next_cue = self
            .cues
            .iter()
            .filter(|cue| cue.start > position + SUBTITLE_TIME_DELTA)
            .map(|cue| cue.start)
            .fold(f64::INFINITY, f64::min);
        self.processed
            .iter()
            .filter(|(start, _)| *start > position + SUBTITLE_TIME_DELTA)
            .map(|(start, _)| *start)
            .fold(next_cue, f64::min)
    }

    fn plan_window(&self, start: f64, duration: f64, rewound: bool) -> WindowPlan {
        let start = self.skip_covered_position(start).max(0.0).min(duration);
        if start >= duration - MIN_WORK_WINDOW_SECONDS {
            if !rewound {
                let beginning = self.skip_covered_position(0.0);
                if beginning < duration - MIN_WORK_WINDOW_SECONDS {
                    return WindowPlan::Advance {
                        start: beginning,
                        rewound: true,
                    };
                }
            }
            return WindowPlan::Done;
        }
        let next_start = self.next_subtitle_start(start);
        let mut end = duration.min(start + WORK_WINDOW_SECONDS);
        if next_start.is_finite() {
            end = end.min(next_start);
        }
        if end <= start + MIN_WORK_WINDOW_SECONDS {
            return WindowPlan::Advance {
                start: duration.min(start + MIN_WORK_WINDOW_SECONDS),
                rewound,
            };
        }
        WindowPlan::Extract(Window {
            start,
            end,
            rewound,
        })
    }

    fn add(
        &mut self,
        text: String,
        start: f64,
        end: f64,
        generation: u64,
        sender: &mpsc::Sender<AsrEvent>,
    ) {
        let overlap = |cue: &AsrCue| {
            cue.start < end - SUBTITLE_TIME_DELTA && start + SUBTITLE_TIME_DELTA < cue.end
        };
        let mut index = 0;
        while index < self.cues.len() {
            if overlap(&self.cues[index]) {
                let removed = self.cues.remove(index);
                let _ = sender.send(AsrEvent::CueRemoved {
                    generation,
                    cue_id: removed.id,
                });
            } else {
                index += 1;
            }
        }
        let cue = AsrCue {
            id: self.next_id,
            text,
            start,
            end,
        };
        self.next_id = self.next_id.wrapping_add(1).max(1);
        let insertion = self
            .cues
            .partition_point(|existing| existing.start < cue.start);
        self.cues.insert(insertion, cue.clone());
        let _ = sender.send(AsrEvent::CueAdded { generation, cue });
    }

    fn clear_all(&mut self, generation: u64, sender: &mpsc::Sender<AsrEvent>) {
        for cue in self.cues.drain(..) {
            let _ = sender.send(AsrEvent::CueRemoved {
                generation,
                cue_id: cue.id,
            });
        }
        self.processed.clear();
        let _ = sender.send(AsrEvent::CuesSnapshot {
            generation,
            cues: Vec::new(),
        });
    }
}

struct SegmentContext<'a> {
    generation: u64,
    window_start: f64,
    window_end: f64,
    abort: &'a AtomicBool,
    cues: &'a mut CueBook,
    shared: &'a Shared,
    sender: &'a mpsc::Sender<AsrEvent>,
}

impl SegmentContext<'_> {
    fn emit_segment(&mut self, text: &str, seg_start: f64, seg_end: f64) -> bool {
        if self.abort.load(Ordering::Acquire) {
            return false;
        }
        {
            let control = lock(&self.shared.control);
            if control.stop || !control.active || self.generation < control.min_cue_generation {
                return false;
            }
        }
        let text = text.trim().to_owned();
        let adjusted = adjust_segment(seg_start, seg_end, self.window_start, self.window_end);
        if is_cue_valid(!text.is_empty(), adjusted.start, adjusted.end) {
            self.cues.add(
                text,
                adjusted.start,
                adjusted.end,
                self.generation,
                self.sender,
            );
            let snapshot = self.cues.cues.clone();
            {
                let mut guard = lock(&self.shared.latest_cues);
                if self.generation < guard.0 {
                    return false;
                }
                *guard = (self.generation, snapshot.clone());
            }
            let _ = self.sender.send(AsrEvent::CuesSnapshot {
                generation: self.generation,
                cues: snapshot,
            });
        }
        true
    }
}

#[cfg(feature = "asr-whisper")]
struct WhisperModel(*mut whisper_sys::whisper_context);

#[cfg(feature = "asr-whisper")]
impl Drop for WhisperModel {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { whisper_sys::whisper_free(self.0) };
        }
    }
}

#[cfg(feature = "asr-whisper")]
struct WhisperVad(*mut whisper_sys::whisper_vad_context);

#[cfg(feature = "asr-whisper")]
impl Drop for WhisperVad {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { whisper_sys::whisper_vad_free(self.0) };
        }
    }
}

#[cfg(feature = "asr-whisper")]
impl WhisperVad {
    // Preserve the VAD state as the worker advances through the media. Whisper's
    // built-in VAD path resets it on every whisper_full call (each 30s window).
    fn contains_speech_samples(&self, samples: &[f32], rewind: bool) -> Result<bool, String> {
        if samples.is_empty() {
            return Ok(false);
        }
        if rewind {
            unsafe { whisper_sys::whisper_vad_reset_state(self.0) };
        }
        let count = i32::try_from(samples.len())
            .map_err(|_| "ASR VAD window contains too many audio samples".to_owned())?;
        if !unsafe {
            whisper_sys::whisper_vad_detect_speech_no_reset(self.0, samples.as_ptr(), count)
        } {
            return Err("Whisper VAD could not scan the audio window".into());
        }
        let segments = unsafe {
            whisper_sys::whisper_vad_segments_from_probs(
                self.0,
                whisper_sys::whisper_vad_default_params(),
            )
        };
        if segments.is_null() {
            return Err("Whisper VAD could not identify speech segments".into());
        }
        let count = unsafe { whisper_sys::whisper_vad_segments_n_segments(segments) };
        unsafe { whisper_sys::whisper_vad_free_segments(segments) };
        Ok(count > 0)
    }
}

enum RuntimeModel {
    #[cfg(feature = "asr-whisper")]
    Whisper(
        WhisperModel,
        whisper_engine::TranscribeOptions,
        Option<WhisperVad>,
    ),
    #[cfg(feature = "asr-onnx")]
    Onnx(Arc<onnx_engine::Engine>),
}

fn load_runtime_model(
    settings: &AsrSettings,
    resolved: &ResolvedModel,
) -> Result<RuntimeModel, String> {
    match (settings.backend, resolved) {
        #[cfg(feature = "asr-whisper")]
        (AsrBackend::Whisper, ResolvedModel::Whisper { path, vad_path }) => {
            let path_c = path_to_cstring(path)?;
            let mut params = unsafe { whisper_sys::whisper_context_default_params() };
            params.use_gpu = settings.whisper.use_gpu;
            params.gpu_device = settings.whisper.gpu_device.max(0);
            params.flash_attn = settings.whisper.flash_attention;
            let model =
                unsafe { whisper_sys::whisper_init_from_file_with_params(path_c.as_ptr(), params) };
            if model.is_null() {
                return Err(format!(
                    "Whisper could not load model '{}'. Check that the model file is complete and that enough memory is available.",
                    path.display(),
                ));
            }
            let model = WhisperModel(model);
            let vad = if settings.whisper.vad_enabled {
                let path_c = path_to_cstring(vad_path)?;
                let params = unsafe { whisper_sys::whisper_vad_default_context_params() };
                let context = unsafe {
                    whisper_sys::whisper_vad_init_from_file_with_params(path_c.as_ptr(), params)
                };
                if context.is_null() {
                    return Err(format!(
                        "Whisper could not load VAD model '{}'",
                        vad_path.display()
                    ));
                }
                Some(WhisperVad(context))
            } else {
                None
            };
            Ok(RuntimeModel::Whisper(
                model,
                whisper_engine::TranscribeOptions {
                    language: whisper_engine::LANGUAGE_JAPANESE.to_owned(),
                    threads: settings.whisper.threads,
                    best_of: settings.whisper.best_of,
                    beam_size: settings.whisper.beam_size,
                    // The persistent scanner above handles VAD once for each
                    // unprocessed interval; whisper_full must not scan it again.
                    use_vad: false,
                    vad_model: String::new(),
                    suppress_regex: settings.whisper.suppress_regex.clone(),
                },
                vad,
            ))
        }
        #[cfg(feature = "asr-onnx")]
        (AsrBackend::Onnx, ResolvedModel::Onnx { path }) => {
            let engine = onnx_engine::Engine::create(
                path,
                onnx_engine::Options {
                    use_gpu: settings.onnx.use_gpu,
                    gpu_device: settings.onnx.gpu_device,
                    threads: settings.onnx.threads,
                    punctuation: settings.onnx.punctuation,
                },
            );
            if !engine.is_loaded() {
                return Err(format!(
                    "Could not load ONNX model '{}': {}",
                    path.display(),
                    engine.load_error()
                ));
            }
            engine.initialize(ORT_API_VERSION);
            let error = engine.load_error();
            if !error.is_empty() {
                return Err(format!(
                    "Could not initialize ONNX model '{}': {error}",
                    path.display()
                ));
            }
            Ok(RuntimeModel::Onnx(engine))
        }
        _ => Err("The selected ASR backend and model configuration do not match.".into()),
    }
}

#[cfg(feature = "asr-whisper")]
fn path_to_cstring(path: &Path) -> Result<CString, String> {
    CString::new(path.to_string_lossy().as_bytes()).map_err(|_| {
        format!(
            "Model path '{}' contains an invalid NUL byte.",
            path.display()
        )
    })
}

fn run_session(
    shared: Arc<Shared>,
    sender: mpsc::Sender<AsrEvent>,
    settings: AsrSettings,
    media: AsrMedia,
    resolved: ResolvedModel,
    initial_position: f64,
    mut prev_worker: JoinOnDrop,
) {
    // Wait for any stopping previous worker to release its native/CUDA model buffers
    // on this background thread before allocating the new model.
    prev_worker.join_now();

    let generation = {
        let control = lock(&shared.control);
        if control.stop || !control.active {
            shared.running.store(false, Ordering::Release);
            return;
        }
        control.generation
    };
    let _ = sender.send(AsrEvent::Started { generation });
    let model = match load_runtime_model(&settings, &resolved) {
        Ok(model) => model,
        Err(message) => {
            finish_with_error(&shared, &sender, generation, message);
            return;
        }
    };
    run_session_loop(
        shared,
        sender,
        media,
        initial_position,
        generation,
        |current_media, window, abort, seg_ctx| {
            let samples = extract_audio_samples(current_media, window.start, window.end, abort)?;
            if abort.load(Ordering::Acquire) {
                return Ok(Transcription::Canceled);
            }
            Ok(transcribe_window(
                &model,
                &samples,
                &settings,
                abort,
                seg_ctx,
                window,
            ))
        },
    );
}

fn run_session_loop<F>(
    shared: Arc<Shared>,
    sender: mpsc::Sender<AsrEvent>,
    mut media: AsrMedia,
    initial_position: f64,
    initial_generation: u64,
    mut process_window: F,
) where
    F: FnMut(&AsrMedia, &Window, &Arc<AtomicBool>, &mut SegmentContext<'_>) -> Result<Transcription, String>,
{
    let mut cues = CueBook::default();
    let mut seen_generation = initial_generation;
    let mut start = initial_position;
    let mut rewound = false;
    let mut failures = 0;

    loop {
        if !wait_for_selection(&shared) {
            break;
        }
        let (
            stop,
            currently_parked,
            active_generation,
            duration,
            position,
            restart_position,
            reset_cues,
            new_media_path,
            new_audio_id,
            emit_snapshot,
        ) = {
            let mut control = lock(&shared.control);
            if control.stop || !control.active {
                break;
            }
            let parked = control.suspended || control.parked_no_audio;
            let generation = control.generation;
            let restart = if !parked && generation != seen_generation {
                control.restart_position.take()
            } else {
                None
            };
            let reset_cues = std::mem::replace(&mut control.reset_cues, false);
            let emit_snapshot = if !parked {
                std::mem::replace(&mut control.emit_snapshot, false)
            } else {
                false
            };
            (
                control.stop,
                parked,
                generation,
                control.duration,
                control.position,
                restart,
                reset_cues,
                control.media_path.clone(),
                control.audio_id,
                emit_snapshot,
            )
        };
        if stop {
            break;
        }
        if reset_cues {
            if !new_media_path.is_empty() {
                media.path = new_media_path;
            }
            media.audio_id = new_audio_id;
            cues.clear_all(active_generation, &sender);
            let mut guard = lock(&shared.latest_cues);
            if active_generation >= guard.0 {
                *guard = (active_generation, Vec::new());
            }
        } else if emit_snapshot {
            let _ = sender.send(AsrEvent::CuesSnapshot {
                generation: active_generation,
                cues: cues.cues.clone(),
            });
        }
        if currently_parked {
            shared.running.store(false, Ordering::Release);
            continue;
        }
        if active_generation != seen_generation {
            seen_generation = active_generation;
            start = restart_position.unwrap_or(position).clamp(0.0, duration);
            rewound = false;
            failures = 0;
            let _ = sender.send(AsrEvent::Started {
                generation: seen_generation,
            });
        }

        let plan = cues.plan_window(start, duration, rewound);
        match plan {
            WindowPlan::Done => {
                shared.running.store(false, Ordering::Release);
                let mut control = lock(&shared.control);
                if control.stop
                    || !control.active
                    || control.suspended
                    || control.parked_no_audio
                    || control.generation != seen_generation
                {
                    continue;
                }
                let _ = sender.send(AsrEvent::Completed {
                    generation: seen_generation,
                    cue_count: cues.cues.len(),
                });
                control.done = true;
                while !control.stop
                    && control.active
                    && !control.reset_cues
                    && control.generation == seen_generation
                {
                    control = shared
                        .wake
                        .wait(control)
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                }
                control.done = false;
                continue;
            }
            WindowPlan::Advance {
                start: next,
                rewound: was_rewound,
            } => {
                start = next;
                rewound = was_rewound;
                continue;
            }
            WindowPlan::Extract(window) => {
                start = window.start;
                rewound = window.rewound;
                shared.running.store(true, Ordering::Release);
                let _ = sender.send(AsrEvent::WindowStarted {
                    generation: seen_generation,
                    start: window.start,
                    end: window.end,
                });
                let abort = Arc::new(AtomicBool::new(false));
                {
                    let mut control = lock(&shared.control);
                    if control.stop
                        || control.suspended
                        || control.parked_no_audio
                        || control.generation != seen_generation
                    {
                        continue;
                    }
                    control.current_window = Some((window.start, window.end));
                    control.current_abort = Some(abort.clone());
                }
                let mut seg_ctx = SegmentContext {
                    generation: seen_generation,
                    window_start: window.start,
                    window_end: window.end,
                    abort: &abort,
                    cues: &mut cues,
                    shared: &shared,
                    sender: &sender,
                };
                let status = match process_window(&media, &window, &abort, &mut seg_ctx) {
                    Ok(status) => status,
                    Err(message) => {
                        clear_attempt(&shared, &abort);
                        if is_stale_or_stopped(&shared, seen_generation) {
                            continue;
                        }
                        finish_with_error(&shared, &sender, seen_generation, message);
                        return;
                    }
                };
                clear_attempt(&shared, &abort);
                match status {
                    Transcription::Success => {
                        failures = 0;
                        // Record silent windows too: they have no cues, but VAD
                        // must never run on them a second time this session.
                        cues.processed.push((window.start, window.end));
                        if is_stale_or_stopped(&shared, seen_generation) {
                            continue;
                        }
                        start = window.end;
                    }
                    Transcription::Canceled => {
                        if is_stale_or_stopped(&shared, seen_generation) {
                            continue;
                        }
                        let control = lock(&shared.control);
                        if control.generation == seen_generation && !control.stop {
                            start = control.position.clamp(0.0, control.duration);
                        }
                    }
                    Transcription::Failed(message) => {
                        if is_stale_or_stopped(&shared, seen_generation) {
                            continue;
                        }
                        failures += 1;
                        if failures >= MAX_TRANSCRIPTION_FAILURES {
                            shared.running.store(false, Ordering::Release);
                            let mut control = lock(&shared.control);
                            if control.stop
                                || !control.active
                                || control.generation != seen_generation
                            {
                                continue;
                            }
                            let _ = sender.send(AsrEvent::Paused {
                                generation: seen_generation,
                                message: format!(
                                    "ASR paused after repeated transcription failures: {message}. Seek or change ASR settings to retry."
                                ),
                            });
                            control.paused_on_errors = true;
                            while !control.stop
                                && control.active
                                && control.generation == seen_generation
                            {
                                control = shared
                                    .wake
                                    .wait(control)
                                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                            }
                            control.paused_on_errors = false;
                        } else {
                            start = {
                                let control = lock(&shared.control);
                                control.position.clamp(0.0, control.duration)
                            };
                        }
                    }
                }
            }
        }
    }
    shared.running.store(false, Ordering::Release);
    let generation = lock(&shared.control).generation;
    let _ = sender.send(AsrEvent::Canceled { generation });
    let mut control = lock(&shared.control);
    control.active = false;
    control.current_abort = None;
    control.current_window = None;
}

/// Wait between windows, retaining the worker's model and CueBook on its stack.
fn wait_for_selection(shared: &Shared) -> bool {
    let mut control = lock(&shared.control);
    while control.active
        && !control.stop
        && (control.suspended || control.parked_no_audio)
        && !control.reset_cues
    {
        shared.running.store(false, Ordering::Release);
        control = shared
            .wake
            .wait(control)
            .unwrap_or_else(|error| error.into_inner());
    }
    control.active && !control.stop
}

fn finish_with_error(
    shared: &Shared,
    sender: &mpsc::Sender<AsrEvent>,
    generation: u64,
    message: String,
) {
    shared.running.store(false, Ordering::Release);
    let _ = sender.send(AsrEvent::Failed {
        generation,
        message,
    });
    let mut control = lock(&shared.control);
    control.active = false;
    control.current_abort = None;
    control.current_window = None;
}

fn is_stale_or_stopped(shared: &Shared, generation: u64) -> bool {
    let control = lock(&shared.control);
    control.stop || !control.active || control.generation != generation
}

fn clear_attempt(shared: &Shared, abort: &Arc<AtomicBool>) {
    let mut control = lock(&shared.control);
    if control
        .current_abort
        .as_ref()
        .is_some_and(|current| Arc::ptr_eq(current, abort))
    {
        control.current_abort = None;
        control.current_window = None;
    }
}

enum Transcription {
    Success,
    Canceled,
    Failed(String),
}

fn transcribe_window(
    model: &RuntimeModel,
    samples: &[f32],
    _settings: &AsrSettings,
    abort: &Arc<AtomicBool>,
    seg_ctx: &mut SegmentContext<'_>,
    #[cfg_attr(not(feature = "asr-whisper"), allow(unused_variables))] window: &Window,
) -> Transcription {
    match model {
        #[cfg(feature = "asr-whisper")]
        RuntimeModel::Whisper(model, options, vad) => {
            if let Some(vad) = vad {
                if abort.load(Ordering::Acquire) {
                    return Transcription::Canceled;
                }
                match vad.contains_speech_samples(
                    samples,
                    window.rewound && window.start < SUBTITLE_TIME_DELTA,
                ) {
                    Ok(false) => return Transcription::Success,
                    Ok(true) => {}
                    Err(message) => return Transcription::Failed(message),
                }
            }
            let mut options = options.clone();
            options.threads = _settings.whisper.threads;
            let should_abort = || abort.load(Ordering::Acquire);
            let mut on_segment = |text: &str, start_cs: i64, end_cs: i64| {
                seg_ctx.emit_segment(
                    text,
                    whisper_engine::timestamp_to_seconds(start_cs),
                    whisper_engine::timestamp_to_seconds(end_cs),
                );
            };
            match unsafe {
                whisper_engine::transcribe_samples_safe(
                    model.0,
                    samples,
                    &options,
                    &should_abort,
                    &mut on_segment,
                )
            } {
                whisper_engine::EngineStatus::Success => Transcription::Success,
                whisper_engine::EngineStatus::Canceled => Transcription::Canceled,
                whisper_engine::EngineStatus::Failed { message, .. } => {
                    Transcription::Failed(message)
                }
            }
        }
        #[cfg(feature = "asr-onnx")]
        RuntimeModel::Onnx(engine) => {
            let should_abort = || abort.load(Ordering::Acquire);
            let mut on_segment = |start_sec: f64, end_sec: f64, text: &str| {
                seg_ctx.emit_segment(text, start_sec, end_sec)
            };
            match engine.transcribe_samples(samples, "ja", &should_abort, &mut on_segment) {
                Ok(true) => Transcription::Success,
                Ok(false) => Transcription::Canceled,
                Err(_) if abort.load(Ordering::Acquire) => Transcription::Canceled,
                Err(message) => Transcription::Failed(message),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn cached_session() -> (AsrController, AsrSettings, AsrMedia) {
        let mut controller = AsrController::new();
        let mut settings = AsrSettings::from_config("/unused", &Config::default_empty()).unwrap();
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
        let mut settings = AsrSettings::from_config("/unused", &Config::default_empty()).unwrap();
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
            let _model = ModelLifetime(worker_freed);
            let mut cues = CueBook::default();
            cues.processed.push((0.0, 30.0));
            cues.add("saved".into(), 1.0, 3.0, 0, &sender);
            entered_tx.send(()).unwrap();
            assert!(wait_for_selection(&shared));
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
            run_session_loop(shared_ref, sender, media.clone(), 5.0, 0, |_m, window, _abort, seg_ctx| {
                seg_ctx.emit_segment(
                    &format!("at-{:.0}", window.start),
                    0.5,
                    2.0,
                );
                Ok(Transcription::Success)
            });
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
                if let AsrEvent::WindowStarted { generation: 1, start, .. } = event {
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
            assert!(!wait_for_selection(&shared));
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
    #[ignore = "requires MEMENTO_ASR_TEST_MODEL pointing to a local Whisper model"]
    fn shutdown_during_native_whisper_inference_releases_model() {
        use std::ffi::c_void;
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
        let path = std::env::var_os("MEMENTO_ASR_TEST_MODEL").expect("set MEMENTO_ASR_TEST_MODEL");
        let path = path_to_cstring(Path::new(&path)).unwrap();
        let use_gpu = std::env::var_os("MEMENTO_ASR_TEST_GPU").is_some();
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
            let model =
                WhisperModel(whisper_sys::whisper_init_from_file_with_params(path.as_ptr(), params));
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
            let result =
                whisper_sys::whisper_full(model.0, params, samples.as_ptr(), samples.len() as i32);
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
    fn cue_book_replaces_overlapping_cues_and_plans_sliding_windows() {
        let (sender, receiver) = mpsc::channel();
        let mut cues = CueBook::default();
        cues.add("first".into(), 4.0, 6.0, 0, &sender);
        cues.add("replacement".into(), 5.0, 7.0, 0, &sender);
        assert_eq!(cues.cues.len(), 1);
        assert_eq!(cues.cues[0].text, "replacement");
        assert!(matches!(
            receiver.try_recv(),
            Ok(AsrEvent::CueAdded { .. })
        ));
        assert!(matches!(
            receiver.try_recv(),
            Ok(AsrEvent::CueRemoved { .. })
        ));
        assert!(matches!(
            receiver.try_recv(),
            Ok(AsrEvent::CueAdded { .. })
        ));
        assert!(matches!(
            cues.plan_window(0.0, 70.0, false),
            WindowPlan::Extract(Window {
                start: 0.0,
                end: 5.0,
                ..
            })
        ));
    }

    #[test]
    fn processed_silent_windows_are_never_transcribed_twice() {
        let mut cues = CueBook::default();
        cues.processed.push((0.0, 30.0));
        assert!(matches!(
            cues.plan_window(0.0, 60.0, false),
            WindowPlan::Extract(Window {
                start: 30.0,
                end: 60.0,
                ..
            })
        ));
        cues.processed.push((30.0, 60.0));
        assert!(matches!(
            cues.plan_window(0.0, 60.0, false),
            WindowPlan::Done
        ));
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
            run_session_loop(shared, sender, media, 0.0, 0, |_m, window, abort, seg_ctx| {
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
                seg_ctx.emit_segment(
                    &format!("win-{:.0}", window.start),
                    0.5,
                    2.0,
                );
                Ok(Transcription::Success)
            });
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
            run_session_loop(shared, sender, media, 0.0, 0, |_m, window, _abort, seg_ctx| {
                if window.start < 50.0 {
                    Ok(Transcription::Failed("synthetic failure".into()))
                } else {
                    seg_ctx.emit_segment("recovered", 0.5, 2.0);
                    Ok(Transcription::Success)
                }
            });
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
            run_session_loop(shared, sender, media, 0.0, 0, |_m, window, _abort, seg_ctx| {
                seg_ctx.emit_segment(&format!("cue-{:.0}", window.start), 0.5, 2.0);
                Ok(Transcription::Success)
            });
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
        assert!(woke_with_new_gen, "seek did not wake worker from Done state");
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
            run_session_loop(shared, sender, media, 0.0, 0, |m, _window, _abort, seg_ctx| {
                seg_ctx.emit_segment(&format!("aid-{}", m.audio_id), 0.5, 2.0);
                Ok(Transcription::Success)
            });
        }));

        let deadline = Instant::now() + Duration::from_secs(5);
        let mut saw_aid1 = false;
        while Instant::now() < deadline && !saw_aid1 {
            for ev in controller.poll() {
                if let AsrEvent::CueAdded { cue, .. } = ev {
                    if cue.text == "aid-1" {
                        saw_aid1 = true;
                    }
                }
            }
            thread::sleep(Duration::from_millis(5));
        }
        assert!(saw_aid1);

        // Switch audio track to aid = 2.
        controller.notify_audio_track_changed(2);
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut saw_removed = false;
        let mut saw_snapshot = false;
        let mut saw_aid2 = false;
        while Instant::now() < deadline && !(saw_removed && saw_snapshot && saw_aid2) {
            for ev in controller.poll() {
                match ev {
                    AsrEvent::CueRemoved { .. } => saw_removed = true,
                    AsrEvent::CuesSnapshot { cues, .. } if cues.iter().any(|c| c.text == "aid-2") => {
                        saw_snapshot = true;
                    }
                    AsrEvent::CueAdded { cue, .. } if cue.text == "aid-2" => saw_aid2 = true,
                    _ => {}
                }
            }
            thread::sleep(Duration::from_millis(5));
        }
        assert!(saw_removed && saw_snapshot && saw_aid2);
        assert_eq!(controller.current_cues().len(), 1);
        assert_eq!(controller.current_cues()[0].text, "aid-2");

        // Switching to audio_id <= 0 parks the worker while keeping the model warm
        // and clears `latest_cues` synchronously.
        controller.notify_audio_track_changed(0);
        assert!(controller.is_active(), "session/model must stay warm when aid <= 0");
        assert!(!controller.is_running());
        assert!(controller.current_cues().is_empty());

        // Cycling back to audio_id = 1 resumes transcription immediately without model reload.
        controller.notify_audio_track_changed(1);
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
        assert!(resumed_aid1, "cycling back to aid = 1 must resume warm worker");
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
            run_session_loop(shared, sender, media1, 0.0, 0, |m, window, _abort, seg_ctx| {
                seg_ctx.emit_segment(
                    &format!("{}:aid{}:{:.0}", m.path, m.audio_id, window.start),
                    0.5,
                    2.0,
                );
                Ok(Transcription::Success)
            });
        }));

        // Wait for first file cue.
        let deadline = Instant::now() + Duration::from_secs(5);
        while controller.current_cues().is_empty() && Instant::now() < deadline {
            let _ = controller.poll();
            thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(controller.current_cues()[0].text, "/unused/video.mkv:aid1:0");

        // Retarget to a new playlist file (`episode2.mkv`, `aid = 3`) with identical settings.
        // Because `/unused` has no model files on disk, `start()` can ONLY succeed by retargeting
        // the live worker in-place without calling `resolve_model` / `load_runtime_model`!
        let media2 = AsrMedia::new("/unused/episode2.mkv", 3);
        let outcome = controller
            .start(settings.clone(), media2, 120.0, 42.0)
            .expect("media retarget must succeed in 0ms without model reload");
        assert_eq!(outcome, SessionStart::New);
        assert!(controller.current_cues().is_empty(), "old file cues must clear immediately");
        assert!(!model_dropped.load(Ordering::Acquire), "model must stay warm in RAM");

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
        assert!(saw_ep2, "retargeted worker did not transcribe episode2.mkv at 42s");
        controller.shutdown();
        assert!(model_dropped.load(Ordering::Acquire));
    }

    #[test]
    fn start_hands_stopping_previous_worker_to_new_thread_instead_of_failing_still_stopping() {
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
        let result = controller
            .start(settings, media, 60.0, 0.0)
            .expect("start() must hand off stopping JoinHandle instead of failing on UI thread");
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
        assert!(matches!(&events[0], AsrEvent::CueAdded { generation: 2, cue } if cue.text == "fresh track cue"));
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
        assert!(matches!(&events[0], AsrEvent::CueAdded { cue, .. } if cue.text == "before seek"));
        assert!(matches!(&events[1], AsrEvent::CuesSnapshot { cues, .. } if cues.len() == 1));
    }
}

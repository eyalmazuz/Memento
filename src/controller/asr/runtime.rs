//! Optional background transcription worker.
use super::*;
use crate::controller::media::extract_audio;
use std::ffi::c_void;
#[cfg(feature = "asr-onnx")]
use std::ffi::{c_char, CStr};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
#[cfg(feature = "asr-whisper")]
use std::ffi::CString;

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
    generation: u64,
    duration: f64,
    position: f64,
    restart_position: Option<f64>,
    current_abort: Option<Arc<AtomicBool>>,
}

struct Shared {
    control: Mutex<Control>,
    wake: Condvar,
    running: AtomicBool,
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
        if media.path.is_empty() || media.audio_id < 0 {
            return Err("The loaded media has no selected audio track. Select an audio track and try again.".into());
        }
        if !duration.is_finite() || duration <= 0.0 || !position.is_finite() {
            return Err("The active media has no usable duration or playback position.".into());
        }
        if self.session.as_ref().is_some_and(|(loaded, source)| loaded == &settings && source == &media) {
            let mut control = lock(&self.shared.control);
            if control.active && !control.stop {
                control.position = position;
                control.duration = duration;
                control.suspended = false;
                self.shared.wake.notify_all();
                return Ok(SessionStart::Resumed);
            }
        }
        if self.is_active() { self.cancel(); }
        if let Some(worker) = self.worker.take() {
            if !worker.is_finished() {
                self.worker = Some(worker);
                return Err("The previous ASR session is still stopping.".into());
            }
            let _ = worker.join();
        }
        let model = resolve_model(&settings)?;

        let (sender, receiver) = mpsc::channel();
        self.receiver = receiver;
        let initial_position = if settings.start_from_beginning {
            0.0
        } else {
            position.max(0.0).min(duration)
        };
        let shared = Arc::new(Shared {
            control: Mutex::new(Control {
                stop: false,
                active: true,
                suspended: false,
                generation: 0,
                duration,
                position,
                restart_position: None,
                current_abort: None,
            }),
            wake: Condvar::new(),
            running: AtomicBool::new(true),
        });
        self.shared = shared.clone();

        let session = (settings.clone(), media.clone());
        self.worker = Some(
            thread::Builder::new()
                .name("memento-asr".into())
                .spawn(move || {
                    run_session(shared, sender, settings, media, model, initial_position)
                })
                .map_err(|error| format!("Could not start ASR worker: {error}"))?,
        );
        self.session = Some(session);
        Ok(SessionStart::New)
    }

    pub fn matches_settings(&self, settings: &AsrSettings) -> bool {
        self.is_active() && self.session.as_ref().is_some_and(|(loaded, _)| loaded == settings)
    }

    /// Hide ASR without discarding its model, cues or completed VAD intervals.
    /// Finish an in-flight window so returning never has to transcribe it again.
    pub fn suspend(&mut self) {
        let mut control = lock(&self.shared.control);
        if control.active && !control.stop { control.suspended = true; }
    }

    /// Update playback position without restarting VAD or Whisper on seeks.
    /// The worker transcribes each media interval once, then reuses its cues.
    pub fn update_position(&mut self, position: f64, duration: f64, _paused: bool) {
        if !position.is_finite() || !duration.is_finite() || duration <= 0.0 {
            return;
        }
        let mut control = lock(&self.shared.control);
        if !control.active || control.stop {
            return;
        }
        control.position = position;
        control.duration = duration;
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
        let generation = lock(&self.shared.control).generation;
        let mut events = Vec::new();
        while let Ok(event) = self.receiver.try_recv() {
            // CueBook keeps cues across seeks. Its queued deltas remain valid
            // even if the playback generation changed before the UI polled.
            if matches!(
                event,
                AsrEvent::CueAdded { .. } | AsrEvent::CueRemoved { .. }
            ) || event.generation() == generation
            {
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
}

struct SegmentContext<'a> {
    generation: u64,
    window_start: f64,
    window_end: f64,
    abort: &'a AtomicBool,
    cues: &'a mut CueBook,
    sender: &'a mpsc::Sender<AsrEvent>,
}

unsafe extern "C" fn abort_poll(user_data: *mut c_void) -> bool {
    if user_data.is_null() {
        return false;
    }
    unsafe { &*(user_data as *const AtomicBool) }.load(Ordering::Acquire)
}

#[cfg(feature = "asr-whisper")]
unsafe extern "C" fn whisper_segment(
    text: *const u8,
    text_len: usize,
    start: i64,
    end: i64,
    user_data: *mut c_void,
) {
    if user_data.is_null() || (text_len != 0 && text.is_null()) {
        return;
    }
    let context = unsafe { &mut *(user_data as *mut SegmentContext<'_>) };
    if context.abort.load(Ordering::Acquire) || text_len == 0 {
        return;
    }
    let bytes = unsafe { std::slice::from_raw_parts(text, text_len) };
    let text = String::from_utf8_lossy(bytes).trim().to_owned();
    let adjusted = adjust_segment(
        whisper_engine::timestamp_to_seconds(start),
        whisper_engine::timestamp_to_seconds(end),
        context.window_start,
        context.window_end,
    );
    if is_cue_valid(!text.is_empty(), adjusted.start, adjusted.end) {
        context.cues.add(
            text,
            adjusted.start,
            adjusted.end,
            context.generation,
            context.sender,
        );
    }
}

#[cfg(feature = "asr-onnx")]
struct OnnxCallContext<'a> {
    segments: SegmentContext<'a>,
    failure: Option<String>,
}

#[cfg(feature = "asr-onnx")]
unsafe extern "C" fn onnx_segment(
    user_data: *mut c_void,
    text: *const c_char,
    start: f64,
    end: f64,
) {
    if user_data.is_null() || text.is_null() {
        return;
    }
    let context = unsafe { &mut *(user_data as *mut OnnxCallContext<'_>) };
    if context.segments.abort.load(Ordering::Acquire) {
        return;
    }
    let text = unsafe { CStr::from_ptr(text) }
        .to_string_lossy()
        .trim()
        .to_owned();
    let adjusted = adjust_segment(
        start,
        end,
        context.segments.window_start,
        context.segments.window_end,
    );
    if is_cue_valid(!text.is_empty(), adjusted.start, adjusted.end) {
        context.segments.cues.add(
            text,
            adjusted.start,
            adjusted.end,
            context.segments.generation,
            context.segments.sender,
        );
    }
}

#[cfg(feature = "asr-onnx")]
unsafe extern "C" fn onnx_failure(user_data: *mut c_void, message: *const c_char) {
    if user_data.is_null() || message.is_null() {
        return;
    }
    let context = unsafe { &mut *(user_data as *mut OnnxCallContext<'_>) };
    context.failure = Some(
        unsafe { CStr::from_ptr(message) }
            .to_string_lossy()
            .into_owned(),
    );
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
    fn contains_speech(&self, wav_path: &Path, rewind: bool) -> Result<bool, String> {
        let samples = whisper_engine::read_whisper_wav(wav_path)?;
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
) {
    let generation = lock(&shared.control).generation;
    if is_stale_or_stopped(&shared, generation) {
        return;
    }
    let _ = sender.send(AsrEvent::Started { generation });
    let model = match load_runtime_model(&settings, &resolved) {
        Ok(model) => model,
        Err(message) => {
            finish_with_error(&shared, &sender, generation, message);
            return;
        }
    };
    let mut cues = CueBook::default();
    let mut seen_generation = generation;
    let mut start = initial_position;
    let mut rewound = false;
    let mut failures = 0;

    loop {
        if !wait_for_selection(&shared) { break; }
        let (stop, active_generation, duration, position, restart_position) = {
            let mut control = lock(&shared.control);
            if control.stop || !control.active {
                break;
            }
            let generation = control.generation;
            let restart = if generation != seen_generation {
                control.restart_position.take()
            } else {
                None
            };
            (
                control.stop,
                generation,
                control.duration,
                control.position,
                restart,
            )
        };
        if stop {
            break;
        }
        if active_generation != seen_generation {
            seen_generation = active_generation;
            start = restart_position.unwrap_or(position).max(0.0).min(duration);
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
                let _ = sender.send(AsrEvent::Completed {
                    generation: seen_generation,
                    cue_count: cues.cues.len(),
                });
                let mut control = lock(&shared.control);
                while !control.stop && control.active && control.generation == seen_generation {
                    control = shared
                        .wake
                        .wait(control)
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                }
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
                    if control.stop || control.suspended || control.generation != seen_generation {
                        continue;
                    }
                    control.current_abort = Some(abort.clone());
                }
                let wav = match extract_audio(&media, window.start, window.end, &abort, None) {
                    Ok(wav) => wav,
                    Err(message) => {
                        clear_attempt(&shared, &abort);
                        if is_stale_or_stopped(&shared, seen_generation) {
                            continue;
                        }
                        finish_with_error(&shared, &sender, seen_generation, message);
                        return;
                    }
                };
                if is_stale_or_stopped(&shared, seen_generation) {
                    clear_attempt(&shared, &abort);
                    drop(wav);
                    continue;
                }

                let status = transcribe_window(
                    &model,
                    &wav.path,
                    &settings,
                    &abort,
                    &mut cues,
                    &sender,
                    seen_generation,
                    &window,
                );
                clear_attempt(&shared, &abort);
                drop(wav);
                if is_stale_or_stopped(&shared, seen_generation) {
                    continue;
                }
                match status {
                    Transcription::Success => {
                        failures = 0;
                        // Record silent windows too: they have no cues, but VAD
                        // must never run on them a second time this session.
                        cues.processed.push((window.start, window.end));
                        start = window.end;
                    }
                    Transcription::Canceled => {
                        let control = lock(&shared.control);
                        if control.generation == seen_generation && !control.stop {
                            start = control.position.max(0.0).min(control.duration);
                        }
                    }
                    Transcription::Failed(message) => {
                        failures += 1;
                        if failures >= MAX_TRANSCRIPTION_FAILURES {
                            shared.running.store(false, Ordering::Release);
                            let _ = sender.send(AsrEvent::Paused {
                                generation: seen_generation,
                                message: format!(
                                    "ASR paused after repeated transcription failures: {message}. Seek or change ASR settings to retry."
                                ),
                            });
                            let mut control = lock(&shared.control);
                            while !control.stop
                                && control.active
                                && control.generation == seen_generation
                            {
                                control = shared
                                    .wake
                                    .wait(control)
                                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                            }
                        } else {
                            start = {
                                let control = lock(&shared.control);
                                control.position.max(0.0).min(control.duration)
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
}

/// Wait between windows, retaining the worker's model and CueBook on its stack.
fn wait_for_selection(shared: &Shared) -> bool {
    let mut control = lock(&shared.control);
    while control.active && !control.stop && control.suspended {
        shared.running.store(false, Ordering::Release);
        control = shared.wake.wait(control).unwrap_or_else(|error| error.into_inner());
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
    }
}

enum Transcription {
    Success,
    Canceled,
    Failed(String),
}

fn transcribe_window(
    model: &RuntimeModel,
    wav_path: &Path,
    _settings: &AsrSettings,
    abort: &Arc<AtomicBool>,
    cues: &mut CueBook,
    sender: &mpsc::Sender<AsrEvent>,
    generation: u64,
    window: &Window,
) -> Transcription {
    match model {
        #[cfg(feature = "asr-whisper")]
        RuntimeModel::Whisper(model, options, vad) => {
            if let Some(vad) = vad {
                if abort.load(Ordering::Acquire) {
                    return Transcription::Canceled;
                }
                match vad.contains_speech(
                    wav_path,
                    window.rewound && window.start < SUBTITLE_TIME_DELTA,
                ) {
                    Ok(false) => return Transcription::Success,
                    Ok(true) => {}
                    Err(message) => return Transcription::Failed(message),
                }
            }
            let abort_bridge = whisper_engine::AbortBridge {
                poll: abort_poll,
                user_data: Arc::as_ptr(abort).cast_mut().cast(),
            };
            let mut context = SegmentContext {
                generation,
                window_start: window.start,
                window_end: window.end,
                abort,
                cues,
                sender,
            };
            let segment_bridge = whisper_engine::SegmentBridge {
                callback: Some(whisper_segment),
                user_data: (&mut context as *mut SegmentContext<'_>).cast(),
            };
            let mut options = options.clone();
            options.threads = _settings.whisper.threads;
            match unsafe {
                whisper_engine::transcribe(
                    model.0,
                    wav_path,
                    &options,
                    Some(&abort_bridge),
                    Some(&segment_bridge),
                    None,
                    None,
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
            let mut context = OnnxCallContext {
                segments: SegmentContext {
                    generation,
                    window_start: window.start,
                    window_end: window.end,
                    abort,
                    cues,
                    sender,
                },
                failure: None,
            };
            let context_ptr = (&mut context as *mut OnnxCallContext<'_>).cast::<c_void>();
            let callbacks = onnx_engine::Callbacks {
                abort: Some(abort_poll),
                abort_ctx: Arc::as_ptr(abort).cast_mut().cast(),
                fail: Some(onnx_failure),
                fail_ctx: context_ptr,
                segment: Some(onnx_segment),
                segment_ctx: context_ptr,
            };
            match engine.transcribe(wav_path, "ja", &callbacks) {
                onnx_engine::RESULT_SUCCESS => Transcription::Success,
                onnx_engine::RESULT_CANCELED => Transcription::Canceled,
                _ if abort.load(Ordering::Acquire) => Transcription::Canceled,
                _ => Transcription::Failed(context.failure.unwrap_or_else(|| {
                    "ONNX transcription failed without a diagnostic message.".into()
                })),
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
        }
        (controller, settings, media)
    }

    #[test]
    fn switching_away_keeps_worker_model_progress_and_queued_cues_until_resume() {
        let (mut controller, settings, media) = cached_session();
        let shared = controller.shared.clone();
        let abort = Arc::new(AtomicBool::new(false));
        lock(&shared.control).current_abort = Some(abort.clone());
        controller.suspend();
        assert!(!abort.load(Ordering::Acquire), "finish the current window instead of redoing VAD");
        let (sender, receiver) = mpsc::channel();
        controller.receiver = receiver;
        let (entered_tx, entered_rx) = mpsc::channel();
        let (resumed_tx, resumed_rx) = mpsc::channel();
        let freed = Arc::new(AtomicBool::new(false));
        let worker_freed = freed.clone();
        controller.worker = Some(thread::spawn(move || {
            struct ModelLifetime(Arc<AtomicBool>);
            impl Drop for ModelLifetime {
                fn drop(&mut self) { self.0.store(true, Ordering::Release); }
            }
            let _model = ModelLifetime(worker_freed);
            let mut cues = CueBook::default();
            cues.processed.push((0.0, 30.0));
            cues.add("saved".into(), 1.0, 3.0, 0, &sender);
            entered_tx.send(()).unwrap();
            assert!(wait_for_selection(&shared));
            assert_eq!(cues.cues.len(), 1);
            assert!(matches!(cues.plan_window(0.0, 90.0, false),
                WindowPlan::Extract(Window { start: 30.0, .. })));
            resumed_tx.send(()).unwrap();
            let mut control = lock(&shared.control);
            while !control.stop { control = shared.wake.wait(control).unwrap(); }
        }));
        entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(controller.is_active());
        assert!(!freed.load(Ordering::Acquire));
        assert!(matches!(controller.poll().as_slice(), [AsrEvent::CueAdded { .. }]));
        controller.update_position(75.0, 90.0, false);
        assert!(controller.matches_settings(&settings));
        // No model exists at the fixture path: this can only succeed by reusing
        // the original worker, rather than resolving/loading a model again.
        assert_eq!(controller.start(settings, media, 90.0, 75.0).unwrap(), SessionStart::Resumed);
        resumed_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(lock(&controller.shared.control).generation, 0);
        assert_eq!(lock(&controller.shared.control).position, 75.0);
        assert!(!freed.load(Ordering::Acquire));
        controller.shutdown();
        assert!(freed.load(Ordering::Acquire));
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
            if change_source { media.audio_id = 2; }
            else { settings.whisper.threads += 1; }
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
        assert!(!controller.try_finish_shutdown(), "cancel is not worker completion");
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
            let model = WhisperModel(whisper_sys::whisper_init_from_file_with_params(path.as_ptr(), params));
            assert!(!model.0.is_null(), "test model could not be loaded");
            let mut context = AbortContext { abort, entered, signaled: false };
            let mut params = whisper_sys::whisper_full_default_params(
                whisper_sys::whisper_sampling_strategy_WHISPER_SAMPLING_GREEDY,
            );
            params.language = c"ja".as_ptr();
            params.n_threads = thread::available_parallelism().map_or(1, |count| count.get().min(4)) as i32;
            params.print_progress = false;
            params.print_realtime = false;
            params.print_timestamps = false;
            params.abort_callback = Some(signal_inference);
            params.abort_callback_user_data = (&mut context as *mut AbortContext).cast();
            let samples: Vec<f32> = (0..30 * 16000).map(|i| (i as f32 * 0.1).sin() * 0.1).collect();
            let result = whisper_sys::whisper_full(model.0, params, samples.as_ptr(), samples.len() as i32);
            assert!(context.abort.load(Ordering::Acquire), "inference exited before cancellation");
            assert_ne!(result, 0, "native inference should report cancellation");
            drop(model);
            worker_released.store(true, Ordering::Release);
        }));
        receiver.recv_timeout(Duration::from_secs(60)).expect("native inference did not begin");
        controller.begin_shutdown();
        controller.shutdown();
        assert!(released.load(Ordering::Acquire), "model must be freed before shutdown returns");
    }

    #[test]
    fn cue_book_replaces_overlapping_cues_and_plans_sliding_windows() {
        let (sender, receiver) = mpsc::channel();
        let mut cues = CueBook::default();
        cues.add("first".into(), 4.0, 6.0, 0, &sender);
        cues.add("replacement".into(), 5.0, 7.0, 0, &sender);
        assert_eq!(cues.cues.len(), 1);
        assert_eq!(cues.cues[0].text, "replacement");
        assert!(matches!(receiver.try_recv(), Ok(AsrEvent::CueAdded { .. })));
        assert!(matches!(
            receiver.try_recv(),
            Ok(AsrEvent::CueRemoved { .. })
        ));
        assert!(matches!(receiver.try_recv(), Ok(AsrEvent::CueAdded { .. })));
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
    fn seeking_does_not_restart_vad_or_abort_the_active_window() {
        let mut controller = AsrController::new();
        let abort = Arc::new(AtomicBool::new(false));
        {
            let mut control = lock(&controller.shared.control);
            control.active = true;
            control.duration = 120.0;
            control.current_abort = Some(abort.clone());
        }
        controller.update_position(75.0, 120.0, false);
        let control = lock(&controller.shared.control);
        assert_eq!(control.generation, 0);
        assert_eq!(control.position, 75.0);
        assert!(!abort.load(Ordering::Acquire));
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
            .send(AsrEvent::Completed {
                generation: 0,
                cue_count: 1,
            })
            .unwrap();
        let events = controller.poll();
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], AsrEvent::CueAdded { cue, .. } if cue.text == "before seek"));
    }
}

//! Streaming speech recognition for the Slint application.
//!
//! The controller owns a cancellable background worker and reports status and
//! cue deltas through [`AsrEvent`]. The UI thread drains those events and
//! applies cue deltas to the local ASR track in
//! [`crate::controller::subtitles::SubtitleController`].

use std::ffi::{c_char, c_void, CStr, CString};
use std::fs;
use std::path::{Path, PathBuf};
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};

use memento_config::{config::Config, Value};
use memento_setting::keys::{asr, onnx, whisper};
use memento_player::mpv::ffi as mpv;

use memento_asr::asrcontroller::{adjust_segment, is_cue_valid};
use memento_asr::onnx::{onnxcontroller, onnxmodelconfig::OnnxModelConfig};
use memento_asr::whisper::whispercontroller;
use memento_asr_onnx::engine as onnx_engine;
use memento_asr_whisper::{engine as whisper_engine, whisper_sys as whisper_sys};

const WORK_WINDOW_SECONDS: f64 = 30.0;
const MIN_WORK_WINDOW_SECONDS: f64 = 0.25;
const SUBTITLE_TIME_DELTA: f64 = 0.0001;
const MAX_TRANSCRIPTION_FAILURES: u32 = 3;
// The app targets the same ONNX Runtime 1.22.0 C API as the Rust engine port.
const ORT_API_VERSION: u32 = 22;

static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AsrBackend {
    Whisper,
    Onnx,
}

/// Count GPU devices registered with the ggml build linked into this app.
/// Whisper counts both discrete and integrated GPUs for its device index.
pub fn whisper_gpu_device_count() -> usize {
    unsafe extern "C" {
        fn ggml_backend_dev_count() -> usize;
        fn ggml_backend_dev_get(index: usize) -> *mut c_void;
        fn ggml_backend_dev_type(device: *mut c_void) -> i32;
    }
    (0..unsafe { ggml_backend_dev_count() })
        .filter(|index| {
            let device = unsafe { ggml_backend_dev_get(*index) };
            matches!(unsafe { ggml_backend_dev_type(device) }, 1 | 2)
        })
        .count()
}

#[derive(Clone, Debug)]
pub struct WhisperSettings {
    pub model: String,
    pub custom_model: String,
    pub vad_enabled: bool,
    pub vad_model: String,
    pub use_gpu: bool,
    pub gpu_device: i32,
    pub threads: i32,
    pub best_of: i32,
    pub beam_size: i32,
    pub flash_attention: bool,
    pub suppress_regex: String,
}

#[derive(Clone, Debug)]
pub struct OnnxSettings {
    pub model: String,
    pub custom_model: String,
    pub use_gpu: bool,
    pub gpu_device: i32,
    pub threads: i32,
    pub punctuation: bool,
}

/// ASR settings captured from the existing application config store.
#[derive(Clone, Debug)]
pub struct AsrSettings {
    pub config_dir: PathBuf,
    pub enabled: bool,
    pub backend: AsrBackend,
    pub start_from_beginning: bool,
    pub whisper: WhisperSettings,
    pub onnx: OnnxSettings,
}

impl AsrSettings {
    /// Read the selected backend and model/runtime options from the canonical
    /// config groups. Missing values use the defaults from `memento-setting`.
    pub fn from_config(config_dir: impl Into<PathBuf>, config: &Config) -> Result<Self, String> {
        let backend_default = asr::backend_default(true, true);
        let backend_name = text_value(
            config.get_at(asr::GROUP, asr::BACKEND),
            backend_default,
        );
        let backend = match backend_name.as_str() {
            asr::BACKEND_WHISPER => AsrBackend::Whisper,
            asr::BACKEND_ONNX => AsrBackend::Onnx,
            other => return Err(format!("Unknown ASR backend '{other}'. Choose Whisper or ONNX in ASR settings.")),
        };

        Ok(Self {
            config_dir: config_dir.into(),
            enabled: bool_value(
                config.get_at(asr::GROUP, asr::ENABLED),
                asr::ENABLED_DEFAULT,
            ),
            backend,
            start_from_beginning: bool_value(
                config.get_at(asr::GROUP, asr::START_FROM_BEGINNING),
                asr::START_FROM_BEGINNING_DEFAULT,
            ),
            whisper: WhisperSettings {
                model: text_value(
                    config.get_at(whisper::GROUP, whisper::MODEL),
                    whisper::MODEL_DEFAULT,
                ),
                custom_model: text_value(
                    config.get_at(whisper::GROUP, whisper::CUSTOM_MODEL),
                    whisper::CUSTOM_MODEL_DEFAULT,
                ),
                vad_enabled: bool_value(
                    config.get_at(whisper::GROUP, whisper::VAD_ENABLED),
                    whisper::VAD_ENABLED_DEFAULT,
                ),
                vad_model: text_value(
                    config.get_at(whisper::GROUP, whisper::VAD_MODEL),
                    whisper::VAD_MODEL_DEFAULT,
                ),
                use_gpu: bool_value(
                    config.get_at(whisper::GROUP, whisper::USE_GPU),
                    whisper::USE_GPU_DEFAULT,
                ),
                gpu_device: int_value(
                    config.get_at(whisper::GROUP, whisper::GPU_DEVICE),
                    whisper::GPU_DEVICE_DEFAULT,
                ),
                threads: int_value(
                    config.get_at(whisper::GROUP, whisper::THREADS),
                    whisper::THREADS_DEFAULT,
                ),
                best_of: int_value(
                    config.get_at(whisper::GROUP, whisper::BEST_OF),
                    whisper::BEST_OF_DEFAULT,
                ),
                beam_size: int_value(
                    config.get_at(whisper::GROUP, whisper::BEAM_SIZE),
                    whisper::BEAM_SIZE_DEFAULT,
                ),
                flash_attention: bool_value(
                    config.get_at(whisper::GROUP, whisper::FLASH_ATTN),
                    whisper::FLASH_ATTN_DEFAULT,
                ),
                suppress_regex: text_value(
                    config.get_at(whisper::GROUP, whisper::SUPPRESS_REGEX),
                    whisper::SUPPRESS_REGEX_DEFAULT,
                ),
            },
            onnx: OnnxSettings {
                model: text_value(
                    config.get_at(onnx::GROUP, onnx::MODEL),
                    onnx::MODEL_DEFAULT,
                ),
                custom_model: text_value(
                    config.get_at(onnx::GROUP, onnx::CUSTOM_MODEL),
                    onnx::CUSTOM_MODEL_DEFAULT,
                ),
                use_gpu: bool_value(
                    config.get_at(onnx::GROUP, onnx::USE_GPU),
                    onnx::USE_GPU_DEFAULT,
                ),
                gpu_device: int_value(
                    config.get_at(onnx::GROUP, onnx::GPU_DEVICE),
                    onnx::GPU_DEVICE_DEFAULT,
                ),
                threads: int_value(
                    config.get_at(onnx::GROUP, onnx::THREADS),
                    onnx::THREADS_DEFAULT,
                ),
                punctuation: bool_value(
                    config.get_at(onnx::GROUP, onnx::PUNCTUATION),
                    onnx::PUNCTUATION_DEFAULT,
                ),
            },
        })
    }
}

fn text_value(value: Option<Value>, default: &str) -> String {
    match value {
        Some(Value::Text(value)) => value,
        _ => default.to_owned(),
    }
}

fn bool_value(value: Option<Value>, default: bool) -> bool {
    match value {
        Some(Value::Bool(value)) => value,
        Some(Value::Int(value)) => value != 0,
        Some(Value::Text(value)) if value.eq_ignore_ascii_case("true") || value == "1" => true,
        Some(Value::Text(value)) if value.eq_ignore_ascii_case("false") || value == "0" => false,
        _ => default,
    }
}

fn int_value(value: Option<Value>, default: i32) -> i32 {
    match value {
        Some(Value::Int(value)) => i32::try_from(value).unwrap_or(default),
        Some(Value::Text(value)) => value.parse().unwrap_or(default),
        _ => default,
    }
}

/// An immutable snapshot of the media and selected mpv audio track. Capture it
/// on the UI thread with `MpvInstance::capture_encode_context` before starting.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AsrMedia {
    pub path: String,
    pub audio_id: i64,
}

impl AsrMedia {
    pub fn new(path: impl Into<String>, audio_id: i64) -> Self {
        Self { path: path.into(), audio_id }
    }
}

/// A transcription cue with an ASR-local ID. The UI maps this ID to the ID
/// returned by `MpvInstance::add_memory_subtitle` for incremental replacement.
#[derive(Clone, Debug, PartialEq)]
pub struct AsrCue {
    pub id: u64,
    pub text: String,
    pub start: f64,
    pub end: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub enum AsrEvent {
    Started { generation: u64 },
    WindowStarted { generation: u64, start: f64, end: f64 },
    CueRemoved { generation: u64, cue_id: u64 },
    CueAdded { generation: u64, cue: AsrCue },
    Completed { generation: u64, cue_count: usize },
    Paused { generation: u64, message: String },
    Failed { generation: u64, message: String },
    Canceled { generation: u64 },
}

impl AsrEvent {
    fn generation(&self) -> u64 {
        match self {
            Self::Started { generation }
            | Self::WindowStarted { generation, .. }
            | Self::CueRemoved { generation, .. }
            | Self::CueAdded { generation, .. }
            | Self::Completed { generation, .. }
            | Self::Paused { generation, .. }
            | Self::Failed { generation, .. }
            | Self::Canceled { generation } => *generation,
        }
    }
}

#[derive(Default)]
struct Control {
    stop: bool,
    active: bool,
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
        }
    }

    /// Start a background transcription session for a loaded media file.
    /// Missing or malformed model configuration is returned synchronously so
    /// the caller can present the exact setup action instead of a fake result.
    pub fn start(
        &mut self,
        settings: AsrSettings,
        media: AsrMedia,
        duration: f64,
        position: f64,
    ) -> Result<(), String> {
        if self.is_active() {
            return Err("ASR is already active. Stop it before starting another session.".into());
        }
        if let Some(worker) = self.worker.take() {
            if !worker.is_finished() {
                self.worker = Some(worker);
                return Err("The previous ASR session is still stopping.".into());
            }
            let _ = worker.join();
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

        self.worker = Some(thread::Builder::new()
            .name("memento-asr".into())
            .spawn(move || run_session(shared, sender, settings, media, model, initial_position))
            .map_err(|error| format!("Could not start ASR worker: {error}"))?);
        Ok(())
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

    /// True while the controller is active, including the wait state after it
    /// has reached the end of the file and can resume after a seek.
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
            if matches!(event, AsrEvent::CueAdded { .. } | AsrEvent::CueRemoved { .. })
                || event.generation() == generation {
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
}

impl Drop for AsrController {
    fn drop(&mut self) {
        self.cancel();
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[derive(Clone)]
enum ResolvedModel {
    Whisper { path: PathBuf, vad_path: PathBuf },
    Onnx { path: PathBuf },
}

fn resolve_model(settings: &AsrSettings) -> Result<ResolvedModel, String> {
    let config_dir = settings.config_dir.to_string_lossy();
    match settings.backend {
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
            Ok(ResolvedModel::Whisper { path: resolved, vad_path })
        }
        AsrBackend::Onnx => {
            let selected = if settings.onnx.model == onnxcontroller::MODEL_CUSTOM {
                settings.onnx.custom_model.trim().to_owned()
            } else {
                onnxcontroller::model_directory(settings.onnx.model.trim(), &config_dir)
            };
            if selected.is_empty() {
                return Err("No ONNX model is selected. Choose a model directory in ONNX settings.".into());
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
            if let Some((_, end)) = self.processed.iter().find(|(start, end)|
                *start <= position + SUBTITLE_TIME_DELTA && position < *end) {
                position = *end;
                continue;
            }
            let Some(cue) = self.cues.iter().find(|cue| {
                cue.start <= position + SUBTITLE_TIME_DELTA && position < cue.end
            }) else {
                return position;
            };
            position = cue.end;
        }
    }

    fn next_subtitle_start(&self, position: f64) -> f64 {
        let next_cue = self.cues
            .iter()
            .filter(|cue| cue.start > position + SUBTITLE_TIME_DELTA)
            .map(|cue| cue.start)
            .fold(f64::INFINITY, f64::min);
        self.processed.iter().filter(|(start, _)| *start > position + SUBTITLE_TIME_DELTA)
            .map(|(start, _)| *start).fold(next_cue, f64::min)
    }

    fn plan_window(&self, start: f64, duration: f64, rewound: bool) -> WindowPlan {
        let start = self.skip_covered_position(start).max(0.0).min(duration);
        if start >= duration - MIN_WORK_WINDOW_SECONDS {
            if !rewound {
                let beginning = self.skip_covered_position(0.0);
                if beginning < duration - MIN_WORK_WINDOW_SECONDS {
                    return WindowPlan::Advance { start: beginning, rewound: true };
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
        WindowPlan::Extract(Window { start, end, rewound })
    }

    fn add(&mut self, text: String, start: f64, end: f64, generation: u64, sender: &mpsc::Sender<AsrEvent>) {
        let overlap = |cue: &AsrCue| {
            cue.start < end - SUBTITLE_TIME_DELTA
                && start + SUBTITLE_TIME_DELTA < cue.end
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
        let insertion = self.cues.partition_point(|existing| existing.start < cue.start);
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

struct OnnxCallContext<'a> {
    segments: SegmentContext<'a>,
    failure: Option<String>,
}

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
    let text = unsafe { CStr::from_ptr(text) }.to_string_lossy().trim().to_owned();
    let adjusted = adjust_segment(start, end, context.segments.window_start, context.segments.window_end);
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

unsafe extern "C" fn onnx_failure(user_data: *mut c_void, message: *const c_char) {
    if user_data.is_null() || message.is_null() {
        return;
    }
    let context = unsafe { &mut *(user_data as *mut OnnxCallContext<'_>) };
    context.failure = Some(unsafe { CStr::from_ptr(message) }.to_string_lossy().into_owned());
}

struct WhisperModel(*mut whisper_sys::whisper_context);

impl Drop for WhisperModel {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { whisper_sys::whisper_free(self.0) };
        }
    }
}

struct WhisperVad(*mut whisper_sys::whisper_vad_context);

impl Drop for WhisperVad {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { whisper_sys::whisper_vad_free(self.0) };
        }
    }
}

impl WhisperVad {
    // Preserve the VAD state as the worker advances through the media. Whisper's
    // built-in VAD path resets it on every whisper_full call (each 30s window).
    fn contains_speech(&self, wav_path: &Path, rewind: bool) -> Result<bool, String> {
        let samples = whisper_engine::read_whisper_wav(wav_path)?;
        if samples.is_empty() { return Ok(false); }
        if rewind { unsafe { whisper_sys::whisper_vad_reset_state(self.0) }; }
        let count = i32::try_from(samples.len())
            .map_err(|_| "ASR VAD window contains too many audio samples".to_owned())?;
        if !unsafe { whisper_sys::whisper_vad_detect_speech_no_reset(self.0, samples.as_ptr(), count) } {
            return Err("Whisper VAD could not scan the audio window".into());
        }
        let segments = unsafe {
            whisper_sys::whisper_vad_segments_from_probs(
                self.0, whisper_sys::whisper_vad_default_params(),
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
    Whisper(WhisperModel, whisper_engine::TranscribeOptions, Option<WhisperVad>),
    Onnx(Arc<onnx_engine::Engine>),
}

fn load_runtime_model(settings: &AsrSettings, resolved: &ResolvedModel) -> Result<RuntimeModel, String> {
    match (settings.backend, resolved) {
        (AsrBackend::Whisper, ResolvedModel::Whisper { path, vad_path }) => {
            let path_c = path_to_cstring(path)?;
            let mut params = unsafe { whisper_sys::whisper_context_default_params() };
            params.use_gpu = settings.whisper.use_gpu;
            params.gpu_device = settings.whisper.gpu_device.max(0);
            params.flash_attn = settings.whisper.flash_attention;
            let model = unsafe {
                whisper_sys::whisper_init_from_file_with_params(path_c.as_ptr(), params)
            };
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
                    return Err(format!("Whisper could not load VAD model '{}'", vad_path.display()));
                }
                Some(WhisperVad(context))
            } else { None };
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
                return Err(format!("Could not load ONNX model '{}': {}", path.display(), engine.load_error()));
            }
            engine.initialize(ORT_API_VERSION);
            let error = engine.load_error();
            if !error.is_empty() {
                return Err(format!("Could not initialize ONNX model '{}': {error}", path.display()));
            }
            Ok(RuntimeModel::Onnx(engine))
        }
        _ => Err("The selected ASR backend and model configuration do not match.".into()),
    }
}

fn path_to_cstring(path: &Path) -> Result<CString, String> {
    CString::new(path.to_string_lossy().as_bytes())
        .map_err(|_| format!("Model path '{}' contains an invalid NUL byte.", path.display()))
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
            (control.stop, generation, control.duration, control.position, restart)
        };
        if stop {
            break;
        }
        if active_generation != seen_generation {
            seen_generation = active_generation;
            start = restart_position.unwrap_or(position).max(0.0).min(duration);
            rewound = false;
            failures = 0;
            let _ = sender.send(AsrEvent::Started { generation: seen_generation });
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
                    control = shared.wake.wait(control).unwrap_or_else(|poisoned| poisoned.into_inner());
                }
                continue;
            }
            WindowPlan::Advance { start: next, rewound: was_rewound } => {
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
                    if control.stop || control.generation != seen_generation {
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
                            while !control.stop && control.active && control.generation == seen_generation {
                                control = shared.wake.wait(control).unwrap_or_else(|poisoned| poisoned.into_inner());
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

fn finish_with_error(shared: &Shared, sender: &mpsc::Sender<AsrEvent>, generation: u64, message: String) {
    shared.running.store(false, Ordering::Release);
    let _ = sender.send(AsrEvent::Failed { generation, message });
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
    if control.current_abort.as_ref().is_some_and(|current| Arc::ptr_eq(current, abort)) {
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
    settings: &AsrSettings,
    abort: &Arc<AtomicBool>,
    cues: &mut CueBook,
    sender: &mpsc::Sender<AsrEvent>,
    generation: u64,
    window: &Window,
) -> Transcription {
    match model {
        RuntimeModel::Whisper(model, options, vad) => {
            if let Some(vad) = vad {
                if abort.load(Ordering::Acquire) { return Transcription::Canceled; }
                match vad.contains_speech(wav_path, window.rewound && window.start < SUBTITLE_TIME_DELTA) {
                    Ok(false) => return Transcription::Success,
                    Ok(true) => {},
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
            options.threads = settings.whisper.threads;
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
                whisper_engine::EngineStatus::Failed { message, .. } => Transcription::Failed(message),
            }
        }
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
                _ => Transcription::Failed(context.failure.unwrap_or_else(|| "ONNX transcription failed without a diagnostic message.".into())),
            }
        }
    }
}

struct TempWav {
    path: PathBuf,
    directory: PathBuf,
}

impl Drop for TempWav {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
        let _ = fs::remove_dir(&self.directory);
    }
}

fn create_temp_wav_path() -> Result<TempWav, String> {
    let root = std::env::temp_dir();
    for _ in 0..32 {
        let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
        let directory = root.join(format!("memento-asr-{}-{id}", std::process::id()));
        match fs::create_dir(&directory) {
            Ok(()) => {
                return Ok(TempWav {
                    path: directory.join("window.wav"),
                    directory,
                });
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(format!("Could not create temporary ASR audio directory: {error}")),
        }
    }
    Err("Could not allocate a unique temporary ASR audio directory.".into())
}

fn create_temp_mp4_path() -> Result<TempWav, String> {
    let root = std::env::temp_dir();
    for _ in 0..32 {
        let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
        let directory = root.join(format!("memento-anki-video-{}-{id}", std::process::id()));
        match fs::create_dir(&directory) {
            Ok(()) => return Ok(TempWav { path: directory.join("clip.mp4"), directory }),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(format!("Could not create temporary Anki video directory: {error}")),
        }
    }
    Err("Could not allocate a temporary Anki video directory".into())
}

struct MpvHandle(*mut mpv::mpv_handle);

impl Drop for MpvHandle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { mpv::mpv_terminate_destroy(self.0) };
        }
    }
}

fn mpv_error(status: i32) -> String {
    let message = unsafe { mpv::mpv_error_string(status) };
    if message.is_null() {
        format!("libmpv error {status}")
    } else {
        format!("libmpv error: {}", unsafe { CStr::from_ptr(message) }.to_string_lossy())
    }
}

/// Extract a subtitle or context clip for an Anki note on a background thread.
pub fn extract_clip_wav(media: &AsrMedia, start: f64, end: f64, normalize_db: Option<f64>) -> Result<Vec<u8>, String> {
    let abort = AtomicBool::new(false);
    let wav = extract_audio(media, start, end, &abort, normalize_db)?;
    fs::read(&wav.path).map_err(|error| format!("Could not read extracted note audio: {error}"))
}

/// Encode an Anki video attachment without blocking the UI's mpv instance.
pub fn extract_clip_mp4(
    media: &AsrMedia, subtitle_id: i64, start: f64, end: f64,
    audio: bool, subtitles: bool, normalize_db: Option<f64>,
) -> Result<Vec<u8>, String> {
    if media.path.is_empty() || !start.is_finite() || !end.is_finite() || end <= start {
        return Err("Could not extract Anki video: media path or clip range is invalid".into());
    }
    if audio && media.audio_id <= 0 {
        return Err("Could not extract Anki video: no audio track is selected".into());
    }
    if subtitles && subtitle_id <= 0 {
        return Err("Could not extract Anki video with subtitles: no subtitle track is selected; use {video:subtitles=false} for this card".into());
    }
    let input = CString::new(media.path.as_bytes())
        .map_err(|_| "Media path contains an invalid NUL byte".to_owned())?;
    let temporary = create_temp_mp4_path()?;
    let output = CString::new(temporary.path.to_string_lossy().as_bytes())
        .map_err(|_| "Temporary video path contains an invalid NUL byte".to_owned())?;
    let handle = unsafe { mpv::mpv_create() };
    if handle.is_null() { return Err("Could not create a libmpv video encoding instance".into()); }
    let handle = MpvHandle(handle);
    let options: [(&CStr, &CStr); 9] = [
        (c"ovc", c"libx264"), (c"oac", c"aac"),
        (c"vid", c"auto"),
        (c"aid", if audio { c"auto" } else { c"no" }),
        (c"sid", if subtitles { c"auto" } else { c"no" }),
        (c"secondary-sid", c"no"),
        (c"cover-art-auto", c"no"), (c"keep-open", c"no"), (c"config", c"no"),
    ];
    for (name, value) in options {
        let status = unsafe { mpv::mpv_set_option_string(handle.0, name.as_ptr(), value.as_ptr()) };
        if status < 0 { return Err(format!("Could not configure libmpv video encoding option {}: {}", name.to_string_lossy(), mpv_error(status))); }
    }
    if let Some(db) = normalize_db.filter(|_| audio) {
        if !db.is_finite() { return Err("Invalid Anki video audio normalization dB".into()); }
        let filter = CString::new(format!("loudnorm=I={db:.1}"))
            .map_err(|_| "Invalid Anki video audio filter".to_owned())?;
        let status = unsafe { mpv::mpv_set_option_string(handle.0, c"af".as_ptr(), filter.as_ptr()) };
        if status < 0 { return Err(format!("Could not configure Anki video audio: {}", mpv_error(status))); }
    }
    let ytdl = if media.path.contains("://") { c"yes" } else { c"no" };
    for (name, value) in [(c"ytdl", ytdl), (c"o", output.as_c_str())] {
        let status = unsafe { mpv::mpv_set_option_string(handle.0, name.as_ptr(), value.as_ptr()) };
        if status < 0 { return Err(format!("Could not configure Anki video output: {}", mpv_error(status))); }
    }
    let status = unsafe { mpv::mpv_initialize(handle.0) };
    if status < 0 { return Err(format!("Could not initialize libmpv video encoding: {}", mpv_error(status))); }
    let mut load_options = format!("start={start:.3},end={end:.3}");
    if audio { load_options.push_str(&format!(",aid={}", media.audio_id)); }
    if subtitles { load_options.push_str(&format!(",sid={subtitle_id}")); }
    let load_options = CString::new(load_options).map_err(|_| "Invalid Anki video options".to_owned())?;
    let api_version = unsafe { mpv::mpv_client_api_version() };
    let mut command: Vec<*const c_char> = vec![c"loadfile".as_ptr(), input.as_ptr(), c"replace".as_ptr()];
    if api_version >= ((2 << 16) | 3) { command.push(c"0".as_ptr()); }
    command.extend([load_options.as_ptr(), ptr::null()]);
    let status = unsafe { mpv::mpv_command(handle.0, command.as_mut_ptr()) };
    if status < 0 { return Err(format!("Could not start Anki video encoding: {}", mpv_error(status))); }
    loop {
        let event = unsafe { mpv::mpv_wait_event(handle.0, 0.1) };
        if event.is_null() { return Err("libmpv returned no event while encoding Anki video".into()); }
        match unsafe { (*event).event_id } {
            mpv::MPV_EVENT_QUEUE_OVERFLOW => return Err("libmpv Anki video event queue overflowed".into()),
            mpv::MPV_EVENT_END_FILE => {
                let end_file = unsafe { &*((*event).data as *const mpv::mpv_event_end_file) };
                if end_file.reason == mpv::MPV_END_FILE_REASON_REDIRECT { continue; }
                if end_file.error < 0 {
                    return Err(format!("libmpv could not encode the Anki video: {}", mpv_error(end_file.error)));
                }
                if end_file.reason != mpv::MPV_END_FILE_REASON_EOF {
                    return Err(format!("libmpv ended Anki video encoding early (reason {})", end_file.reason));
                }
                // The MP4 footer is written while the encoder shuts down.
                drop(handle);
                if !temporary.path.is_file() {
                    return Err("libmpv did not create an Anki video clip".into());
                }
                let bytes = fs::read(&temporary.path)
                    .map_err(|error| format!("Could not read encoded Anki video: {error}"))?;
                if bytes.len() < 12 || &bytes[4..8] != b"ftyp" {
                    return Err(format!("libmpv produced an invalid Anki MP4 clip ({} bytes)", bytes.len()));
                }
                return Ok(bytes);
            }
            _ => {}
        }
    }
}

fn extract_audio(media: &AsrMedia, start: f64, end: f64, abort: &AtomicBool, normalize_db: Option<f64>) -> Result<TempWav, String> {
    if abort.load(Ordering::Acquire) {
        return Err("Audio extraction canceled.".into());
    }
    if media.path.is_empty() || media.audio_id < 0 || !start.is_finite() || !end.is_finite() || end <= start {
        return Err("Could not extract ASR audio: media path, selected audio track, or window is invalid.".into());
    }
    let input = CString::new(media.path.as_bytes())
        .map_err(|_| "Media path contains an invalid NUL byte.".to_owned())?;
    let temporary = create_temp_wav_path()?;
    let output = CString::new(temporary.path.to_string_lossy().as_bytes())
        .map_err(|_| "Temporary audio path contains an invalid NUL byte.".to_owned())?;
    let handle = unsafe { mpv::mpv_create() };
    if handle.is_null() {
        return Err("Could not create a libmpv audio extraction instance.".into());
    }
    let handle = MpvHandle(handle);
    let options: [(&CStr, &CStr); 9] = [
        (c"vid", c"no"),
        (c"sid", c"no"),
        (c"secondary-sid", c"no"),
        (c"oac", c"pcm_s16le"),
        (c"audio-samplerate", c"16000"),
        (c"audio-channels", c"mono"),
        (c"cover-art-auto", c"no"),
        (c"keep-open", c"no"),
        (c"config", c"no"),
    ];
    for (name, value) in options {
        let status = unsafe { mpv::mpv_set_option_string(handle.0, name.as_ptr(), value.as_ptr()) };
        if status < 0 {
            return Err(format!("Could not configure libmpv audio extraction: {}", mpv_error(status)));
        }
    }
    if let Some(db) = normalize_db {
        if !db.is_finite() { return Err("Invalid Anki audio normalization dB".into()); }
        let filter = CString::new(format!("loudnorm=I={db:.1}"))
            .map_err(|_| "Invalid Anki audio normalization filter".to_owned())?;
        let status = unsafe { mpv::mpv_set_option_string(handle.0, c"af".as_ptr(), filter.as_ptr()) };
        if status < 0 { return Err(format!("Could not configure note audio normalization: {}", mpv_error(status))); }
    }
    let ytdl = if media.path.contains("://") { c"yes" } else { c"no" };
    let status = unsafe { mpv::mpv_set_option_string(handle.0, c"ytdl".as_ptr(), ytdl.as_ptr()) };
    if status < 0 {
        return Err(format!("Could not configure libmpv media access: {}", mpv_error(status)));
    }
    let status = unsafe { mpv::mpv_set_option_string(handle.0, c"o".as_ptr(), output.as_ptr()) };
    if status < 0 {
        return Err(format!("Could not configure the temporary ASR WAV output: {}", mpv_error(status)));
    }
    let status = unsafe { mpv::mpv_initialize(handle.0) };
    if status < 0 {
        return Err(format!("Could not initialize libmpv audio extraction: {}", mpv_error(status)));
    }
    if abort.load(Ordering::Acquire) {
        return Err("Audio extraction canceled.".into());
    }

    let load_options = CString::new(format!(
        "start={start:.3},end={end:.3},aid={}",
        media.audio_id,
    )).map_err(|_| "Invalid audio extraction options.".to_owned())?;
    let api_version = unsafe { mpv::mpv_client_api_version() };
    // mpv_client_api_version() packs (major << 16) | minor; loadfile gained its
    // <index> argument in client API 2.3.
    let is_api_23 = api_version >= ((2 << 16) | 3);
    let mut command: Vec<*const c_char> = vec![
        c"loadfile".as_ptr(),
        input.as_ptr(),
        c"replace".as_ptr(),
    ];
    if is_api_23 {
        command.push(c"0".as_ptr());
    }
    command.push(load_options.as_ptr());
    command.push(ptr::null());
    let status = unsafe { mpv::mpv_command(handle.0, command.as_mut_ptr()) };
    if status < 0 {
        return Err(format!("Could not start libmpv audio extraction: {}", mpv_error(status)));
    }

    loop {
        if abort.load(Ordering::Acquire) {
            return Err("Audio extraction canceled.".into());
        }
        let event = unsafe { mpv::mpv_wait_event(handle.0, 0.1) };
        if event.is_null() {
            return Err("libmpv returned no event while extracting audio.".into());
        }
        let event_id = unsafe { (*event).event_id };
        if event_id == mpv::MPV_EVENT_NONE {
            continue;
        }
        if event_id == mpv::MPV_EVENT_QUEUE_OVERFLOW {
            return Err("libmpv audio extraction event queue overflowed.".into());
        }
        if event_id == mpv::MPV_EVENT_END_FILE {
            if !temporary.path.is_file() {
                return Err(format!(
                    "libmpv did not create an audio clip for '{}'. Check that the selected audio track is available.",
                    media.path,
                ));
            }
            return Ok(temporary);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires ffmpeg and a libmpv build with MP4 encoding"]
    fn encodes_real_anki_video_clip() {
        let path = std::env::temp_dir().join(format!("memento-anki-video-test-{}.mp4", std::process::id()));
        let output = std::process::Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-f", "lavfi", "-i", "color=c=blue:s=320x180:r=10:d=2", "-c:v", "libx264", "-pix_fmt", "yuv420p"])
            .arg(&path).output().expect("ffmpeg must be installed");
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        let media = AsrMedia::new(path.to_string_lossy().into_owned(), -1);
        let clip = extract_clip_mp4(&media, 0, 0.25, 1.25, false, false, None);
        let _ = fs::remove_file(path);
        let clip = clip.unwrap();
        assert!(clip.len() > 100, "encoded clip was {} bytes", clip.len());
        assert_eq!(&clip[4..8], b"ftyp");
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
        assert!(matches!(receiver.try_recv(), Ok(AsrEvent::CueRemoved { .. })));
        assert!(matches!(receiver.try_recv(), Ok(AsrEvent::CueAdded { .. })));
        assert!(matches!(cues.plan_window(0.0, 70.0, false), WindowPlan::Extract(Window { start: 0.0, end: 5.0, .. })));
    }

    #[test]
    fn processed_silent_windows_are_never_transcribed_twice() {
        let mut cues = CueBook::default();
        cues.processed.push((0.0, 30.0));
        assert!(matches!(cues.plan_window(0.0, 60.0, false),
            WindowPlan::Extract(Window { start: 30.0, end: 60.0, .. })));
        cues.processed.push((30.0, 60.0));
        assert!(matches!(cues.plan_window(0.0, 60.0, false), WindowPlan::Done));
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
        sender.send(AsrEvent::CueAdded { generation: 0, cue: AsrCue {
            id: 1, text: "before seek".into(), start: 1.0, end: 2.0,
        } }).unwrap();
        sender.send(AsrEvent::Completed { generation: 0, cue_count: 1 }).unwrap();
        let events = controller.poll();
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], AsrEvent::CueAdded { cue, .. } if cue.text == "before seek"));
    }
}

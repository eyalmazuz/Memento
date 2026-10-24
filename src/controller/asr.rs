//! Streaming speech recognition for the Slint application.
//!
//! The controller owns a cancellable background worker and reports status and
//! cue deltas through [`AsrEvent`]. The UI thread drains those events and
//! applies cue deltas to the local ASR track in
//! [`crate::controller::subtitles::SubtitleController`].

#[cfg(feature = "asr-whisper")]
use std::ffi::c_void;
use std::path::PathBuf;

use super::media::MediaContext as AsrMedia;
use memento_config::{config::Config, Value};
use memento_setting::keys::{asr, onnx, whisper};

pub const AVAILABLE: bool = cfg!(any(feature = "asr-whisper", feature = "asr-onnx"));

pub fn backend_available(backend: &str) -> bool {
    match backend {
        asr::BACKEND_WHISPER => cfg!(feature = "asr-whisper"),
        asr::BACKEND_ONNX => cfg!(feature = "asr-onnx"),
        _ => false,
    }
}

pub fn default_backend() -> &'static str {
    asr::backend_default(cfg!(feature = "asr-whisper"), cfg!(feature = "asr-onnx"))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AsrBackend {
    Whisper,
    Onnx,
}

/// Count GPU devices registered with the ggml build linked into this app.
/// Whisper counts both discrete and integrated GPUs for its device index.
#[cfg(feature = "asr-whisper")]
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

#[cfg(not(feature = "asr-whisper"))]
pub fn whisper_gpu_device_count() -> usize {
    0
}

#[derive(Clone, Debug, PartialEq, Eq)]
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OnnxSettings {
    pub model: String,
    pub custom_model: String,
    pub use_gpu: bool,
    pub gpu_device: i32,
    pub threads: i32,
    pub punctuation: bool,
}

/// ASR settings captured from the existing application config store.
#[derive(Clone, Debug, PartialEq, Eq)]
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
        if !AVAILABLE {
            return Err("Speech recognition is not included in this build.".into());
        }
        let backend_default = default_backend();
        let mut backend_name = text_value(config.get_at(asr::GROUP, asr::BACKEND), backend_default);
        if matches!(
            backend_name.as_str(),
            asr::BACKEND_WHISPER | asr::BACKEND_ONNX
        ) && !backend_available(&backend_name)
        {
            backend_name = backend_default.to_owned();
        }
        let backend = match backend_name.as_str() {
            asr::BACKEND_WHISPER => AsrBackend::Whisper,
            asr::BACKEND_ONNX => AsrBackend::Onnx,
            other => {
                return Err(format!(
                    "Unknown ASR backend '{other}'. Choose Whisper or ONNX in ASR settings."
                ))
            }
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
                model: text_value(config.get_at(onnx::GROUP, onnx::MODEL), onnx::MODEL_DEFAULT),
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

/// A transcription cue with an ASR-local ID used for incremental replacement
/// in the application's subtitle overlay and sidebar.
#[derive(Clone, Debug, PartialEq)]
pub struct AsrCue {
    pub id: u64,
    pub text: String,
    pub start: f64,
    pub end: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub enum AsrEvent {
    Started {
        generation: u64,
    },
    WindowStarted {
        generation: u64,
        start: f64,
        end: f64,
    },
    CueRemoved {
        generation: u64,
        cue_id: u64,
    },
    CueAdded {
        generation: u64,
        cue: AsrCue,
    },
    Completed {
        generation: u64,
        cue_count: usize,
    },
    Paused {
        generation: u64,
        message: String,
    },
    Failed {
        generation: u64,
        message: String,
    },
    Canceled {
        generation: u64,
    },
}

#[cfg(any(feature = "asr-whisper", feature = "asr-onnx"))]
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionStart {
    New,
    Resumed,
}

#[cfg(any(feature = "asr-whisper", feature = "asr-onnx"))]
mod runtime;
#[cfg(any(feature = "asr-whisper", feature = "asr-onnx"))]
pub use runtime::AsrController;

#[cfg(not(any(feature = "asr-whisper", feature = "asr-onnx")))]
#[derive(Default)]
pub struct AsrController;

#[cfg(not(any(feature = "asr-whisper", feature = "asr-onnx")))]
impl AsrController {
    pub fn new() -> Self {
        Self
    }
    pub fn start(&mut self, _: AsrSettings, _: AsrMedia, _: f64, _: f64) -> Result<SessionStart, String> {
        Err("Speech recognition is not included in this build.".into())
    }
    pub fn suspend(&mut self) {}
    pub fn matches_settings(&self, _: &AsrSettings) -> bool { false }
    pub fn cancel(&mut self) {}
    pub fn begin_shutdown(&mut self) {}
    pub fn try_finish_shutdown(&mut self) -> bool { true }
    pub fn shutdown(&mut self) {}
    pub fn update_position(&mut self, _: f64, _: f64, _: bool) {}
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

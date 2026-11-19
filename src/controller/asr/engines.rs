//! Backend model resolution, loading, VAD state management, and inference.

#[cfg(feature = "asr-whisper")]
use std::ffi::CString;
#[cfg(feature = "asr-whisper")]
use std::path::Path;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

#[cfg(feature = "asr-onnx")]
use memento_asr::onnx::{onnxcontroller, onnxmodelconfig::OnnxModelConfig};
#[cfg(feature = "asr-whisper")]
use memento_asr::whisper::whispercontroller;
#[cfg(feature = "asr-onnx")]
use memento_asr_onnx::engine as onnx_engine;
#[cfg(feature = "asr-whisper")]
use memento_asr_whisper::{engine as whisper_engine, whisper_sys};

use super::cue_book::Window;
#[cfg(feature = "asr-whisper")]
use super::cue_book::SUBTITLE_TIME_DELTA;
#[cfg(feature = "asr-whisper")]
use super::resolve_threads;
use super::worker::SegmentContext;
use super::{AsrBackend, AsrSettings};

// The app targets the same ONNX Runtime 1.22.0 C API as the Rust engine port.
#[cfg(feature = "asr-onnx")]
const ORT_API_VERSION: u32 = 22;

#[derive(Clone)]
pub(super) enum ResolvedModel {
    #[cfg(feature = "asr-whisper")]
    Whisper { path: PathBuf, vad_path: PathBuf },
    #[cfg(feature = "asr-onnx")]
    Onnx { path: PathBuf },
}

pub(super) fn resolve_model(settings: &AsrSettings) -> Result<ResolvedModel, String> {
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
            let selected = if settings.onnx.model.trim() == onnxcontroller::MODEL_CUSTOM {
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

#[cfg(feature = "asr-whisper")]
pub(super) struct WhisperModel(pub(super) *mut whisper_sys::whisper_context);

#[cfg(feature = "asr-whisper")]
impl Drop for WhisperModel {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { whisper_sys::whisper_free(self.0) };
        }
    }
}

#[cfg(feature = "asr-whisper")]
pub(super) fn should_reset_vad(last_vad_end: Option<f64>, window_start: f64) -> bool {
    match last_vad_end {
        Some(prev_end) => (window_start - prev_end).abs() > SUBTITLE_TIME_DELTA,
        None => true,
    }
}

#[cfg(feature = "asr-whisper")]
pub(super) struct WhisperVad {
    ctx: *mut whisper_sys::whisper_vad_context,
    last_end: Option<f64>,
}

#[cfg(feature = "asr-whisper")]
impl Drop for WhisperVad {
    fn drop(&mut self) {
        if !self.ctx.is_null() {
            unsafe { whisper_sys::whisper_vad_free(self.ctx) };
        }
    }
}

#[cfg(feature = "asr-whisper")]
impl WhisperVad {
    fn new(ctx: *mut whisper_sys::whisper_vad_context) -> Self {
        Self {
            ctx,
            last_end: None,
        }
    }

    fn reset_position(&mut self) {
        self.last_end = None;
    }

    // Preserve the Silero VAD LSTM state across contiguous windows, and reset
    // it on any seek, skipped interval, media retarget, or audio track change.
    fn contains_speech_samples(
        &mut self,
        samples: &[f32],
        window_start: f64,
        window_end: f64,
    ) -> Result<bool, String> {
        if samples.is_empty() {
            self.last_end = None;
            return Ok(false);
        }
        let reset = should_reset_vad(self.last_end, window_start);
        // Clear `last_end` before native calls so any error forces a reset next time.
        self.last_end = None;
        if reset {
            unsafe { whisper_sys::whisper_vad_reset_state(self.ctx) };
        }
        let count = i32::try_from(samples.len())
            .map_err(|_| "ASR VAD window contains too many audio samples".to_owned())?;
        if !unsafe {
            whisper_sys::whisper_vad_detect_speech_no_reset(self.ctx, samples.as_ptr(), count)
        } {
            return Err("Whisper VAD could not scan the audio window".into());
        }
        let segments = unsafe {
            whisper_sys::whisper_vad_segments_from_probs(
                self.ctx,
                whisper_sys::whisper_vad_default_params(),
            )
        };
        if segments.is_null() {
            return Err("Whisper VAD could not identify speech segments".into());
        }
        let count = unsafe { whisper_sys::whisper_vad_segments_n_segments(segments) };
        unsafe { whisper_sys::whisper_vad_free_segments(segments) };
        self.last_end = Some(window_end);
        Ok(count > 0)
    }
}

pub(super) enum RuntimeModel {
    #[cfg(feature = "asr-whisper")]
    Whisper(
        WhisperModel,
        whisper_engine::TranscribeOptions,
        Option<WhisperVad>,
    ),
    #[cfg(feature = "asr-onnx")]
    Onnx(Arc<onnx_engine::Engine>),
}

impl RuntimeModel {
    pub(super) fn reset_vad_state(&mut self) {
        #[cfg(feature = "asr-whisper")]
        if let Self::Whisper(_, _, Some(vad)) = self {
            vad.reset_position();
        }
        #[cfg(not(feature = "asr-whisper"))]
        let _ = self;
    }
}

pub(super) fn load_runtime_model(
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
                Some(WhisperVad::new(context))
            } else {
                None
            };
            Ok(RuntimeModel::Whisper(
                model,
                whisper_engine::TranscribeOptions {
                    language: whisper_engine::LANGUAGE_JAPANESE.to_owned(),
                    threads: resolve_threads(settings.whisper.threads),
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
pub(super) fn path_to_cstring(path: &Path) -> Result<CString, String> {
    #[cfg(unix)]
    let bytes = {
        use std::os::unix::ffi::OsStrExt;
        path.as_os_str().as_bytes()
    };
    #[cfg(not(unix))]
    let lossy = path.to_string_lossy();
    #[cfg(not(unix))]
    let bytes = lossy.as_bytes();

    CString::new(bytes).map_err(|_| {
        format!(
            "Model path '{}' contains an invalid NUL byte.",
            path.display()
        )
    })
}

pub(super) enum Transcription {
    Success,
    Canceled,
    Failed(String),
}

pub(super) fn transcribe_window(
    model: &mut RuntimeModel,
    samples: &[f32],
    abort: &Arc<AtomicBool>,
    seg_ctx: &mut SegmentContext<'_>,
    #[cfg_attr(not(feature = "asr-whisper"), allow(unused_variables))] window: &Window,
) -> Transcription {
    match model {
        #[cfg(feature = "asr-whisper")]
        RuntimeModel::Whisper(model, options, vad) => {
            if let Some(vad) = vad.as_mut() {
                if abort.load(Ordering::Acquire) {
                    return Transcription::Canceled;
                }
                match vad.contains_speech_samples(samples, window.start, window.end) {
                    Ok(false) => return Transcription::Success,
                    Ok(true) => {}
                    Err(message) => return Transcription::Failed(message),
                }
            }
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
                    options,
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

    #[cfg(feature = "asr-whisper")]
    #[test]
    fn should_reset_vad_triggers_on_seeks_gaps_and_retargets_but_not_contiguous_windows() {
        assert!(should_reset_vad(None, 0.0));
        assert!(should_reset_vad(None, 30.0));
        assert!(!should_reset_vad(Some(30.0), 30.0));
        assert!(!should_reset_vad(
            Some(30.0),
            30.0 + SUBTITLE_TIME_DELTA * 0.5
        ));
        // Forward seek
        assert!(should_reset_vad(Some(30.0), 75.0));
        // Backward seek to non-zero position
        assert!(should_reset_vad(Some(90.0), 45.0));
        // Rewind to beginning
        assert!(should_reset_vad(Some(90.0), 0.0));
        // Skipped already-covered window
        assert!(should_reset_vad(Some(30.0), 60.0));
    }

    #[cfg(all(unix, feature = "asr-whisper"))]
    #[test]
    fn path_to_cstring_preserves_non_utf8_unix_bytes_and_rejects_nul() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;

        let raw = b"/tmp/whisper-\xff\xfe/model.bin";
        let path = Path::new(OsStr::from_bytes(raw));
        let cstr = path_to_cstring(path).expect("valid non-UTF-8 Unix path");
        assert_eq!(cstr.as_bytes(), raw);

        let with_nul = Path::new(OsStr::from_bytes(b"/tmp/bad\0model.bin"));
        assert!(path_to_cstring(with_nul).is_err());
    }

    #[cfg(feature = "asr-onnx")]
    #[test]
    fn resolve_onnx_custom_model_trims_whitespace() {
        let mut settings =
            AsrSettings::from_config("/unused", &memento_config::config::Config::default_empty())
                .unwrap();
        settings.backend = AsrBackend::Onnx;
        settings.onnx.model = format!("  {}  ", onnxcontroller::MODEL_CUSTOM);
        settings.onnx.custom_model = "   ".into();
        let err = resolve_model(&settings)
            .err()
            .expect("empty custom model must fail");
        assert!(
            err.contains("No ONNX model is selected"),
            "trimmed custom model check should reject empty custom_model: {err}"
        );
    }
}

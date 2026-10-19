//! Port of `asr/whisper/whispercontroller.{h,cpp}` — the model-registry,
//! path-resolution, and URL pure core.
//!
//! The C++ class stays the `QObject` (settings plumbing, `QCoro` backend
//! creation, localized failure strings); only its Qt-filesystem-backed pure
//! methods were converted here. Every semantic contract below is checked
//! against the compiled C++ implementation by
//! `tests/mpv-memory-subtitles/controllers_equal_test.cpp`.

use crate::compat;

/// `WhisperController::MODEL_PREFIX`.
pub const MODEL_PREFIX: &str = "ggml-";
/// `WhisperController::MODEL_SUFFIX`.
pub const MODEL_SUFFIX: &str = ".bin";
/// `WhisperController::WHISPER_MODEL_URL` prefix.
pub const WHISPER_MODEL_URL: &str =
    "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/";
/// `WhisperController::WHISPER_VAD_URL` prefix.
pub const WHISPER_VAD_URL: &str =
    "https://huggingface.co/ggml-org/whisper-vad/resolve/main/";
/// `WhisperController::VAD_SILERO_5`.
pub const VAD_SILERO_5: &str = "ggml-silero-v5.1.2.bin";
/// `WhisperController::VAD_SILERO_6`.
pub const VAD_SILERO_6: &str = "ggml-silero-v6.2.0.bin";

/// `WhisperController::WHISPER_MODEL_DIR` (`models`, under the config dir).
pub const WHISPER_MODEL_DIR: &str = "models";

const MANAGED_MODELS: [&str; 6] = [
    "tiny",
    "base",
    "small",
    "medium",
    "large-v3",
    "large-v3-turbo",
];

/// `WhisperController::isManagedModel`.
pub fn is_managed_model(model: &str) -> bool {
    MANAGED_MODELS.contains(&model)
}

/// `WhisperController::modelFilename`.
pub fn model_filename(model: &str) -> String {
    format!("{MODEL_PREFIX}{model}{MODEL_SUFFIX}")
}

/// `WhisperController::whisperModelUrl` (the exact URL string the C++ code
/// composes and wraps in `QUrl`).
pub fn whisper_model_url(model: &str) -> String {
    format!("{WHISPER_MODEL_URL}{}", model_filename(model))
}

/// `WhisperController::vadModelUrl`: empty for absolute paths and for
/// filenames outside the Silero VAD whitelist; the exact Hugging Face URL
/// otherwise.
pub fn vad_model_url(model_path: &str) -> String {
    if compat::is_absolute(model_path) {
        return String::new();
    }
    let filename = compat::file_name(model_path);
    if filename != VAD_SILERO_5 && filename != VAD_SILERO_6 {
        return String::new();
    }
    format!("{WHISPER_VAD_URL}{filename}")
}

/// `WhisperController::modelsDirectory`.
pub fn models_directory(config_dir: &str) -> String {
    compat::qdir_join(config_dir, WHISPER_MODEL_DIR)
}

/// `WhisperController::modelAvailable`.
pub fn model_available(model: &str, config_dir: &str) -> bool {
    if !is_managed_model(model) {
        return false;
    }
    compat::file_exists(&resolve_model_path(
        &model_filename(model),
        &[MODEL_SUFFIX],
        config_dir,
    ))
}

/// `WhisperController::hasAnyModel`: any readable file entry that ends with
/// the model suffix and is not one of the two Silero VAD files.
pub fn has_any_model(config_dir: &str) -> bool {
    for entry in compat::entry_files_readable(&models_directory(config_dir)) {
        let filename = entry.name;
        if !filename.ends_with(MODEL_SUFFIX)
            || filename == VAD_SILERO_5
            || filename == VAD_SILERO_6
        {
            continue;
        }
        return true;
    }
    false
}

/// `WhisperController::resolveModelPath`.
///
/// Exact contract:
/// - empty input → empty;
/// - absolute input → returned untouched;
/// - candidate list: the raw path, then every suffix-append the path does not
///   already end with (list preserves order, then duplicate-first removal);
/// - the first existing candidate is returned as `modelsDirectory.file(_:n)`
///   composes it (no cleaning);
/// - otherwise a bare filename (no separator) falls back to the *last*
///   (deduplicated) candidate joined into the model directory;
/// - otherwise `QFileInfo::absoluteFilePath()` of the original input.
pub fn resolve_model_path(
    path: &str,
    suffixes: &[&str],
    config_dir: &str,
) -> String {
    if path.is_empty() {
        return String::new();
    }
    if compat::is_absolute(path) {
        return path.to_string();
    }

    let model_dir = models_directory(config_dir);
    let mut filenames: Vec<String> = vec![path.to_string()];
    for suffix in suffixes {
        if !path.ends_with(suffix) {
            filenames.push(format!("{path}{suffix}"));
        }
    }
    dedupe_keep_first(&mut filenames);

    for filename in &filenames {
        let candidate = compat::qdir_join(&model_dir, filename);
        if compat::file_exists(&candidate) {
            return candidate;
        }
    }

    let bare_filename = compat::file_name(path) == path;
    if bare_filename {
        let last = filenames.last().map(String::as_str).unwrap_or(path);
        return compat::qdir_join(&model_dir, last);
    }
    compat::qfileinfo_absolute(path)
}

fn dedupe_keep_first(entries: &mut Vec<String>) {
    let mut seen: Vec<usize> = Vec::new();
    let mut result: Vec<String> = Vec::with_capacity(entries.len());
    for entry in entries.drain(..) {
        if seen.iter().any(|index| result[*index] == entry) {
            continue;
        }
        seen.push(result.len());
        result.push(entry);
    }
    *entries = result;
}

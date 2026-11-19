//! Model-registry, path-resolution, and URL helpers for Whisper ASR.

use std::path::{Component, Path, PathBuf};

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
    if Path::new(model_path).is_absolute() {
        return String::new();
    }
    let filename = file_name(model_path);
    if filename != VAD_SILERO_5 && filename != VAD_SILERO_6 {
        return String::new();
    }
    format!("{WHISPER_VAD_URL}{filename}")
}

/// `WhisperController::modelsDirectory`.
pub fn models_directory(config_dir: &str) -> String {
    join_dir(config_dir, WHISPER_MODEL_DIR)
}

/// `WhisperController::modelAvailable`.
pub fn model_available(model: &str, config_dir: &str) -> bool {
    if !is_managed_model(model) {
        return false;
    }
    Path::new(&resolve_model_path(
        &model_filename(model),
        &[MODEL_SUFFIX],
        config_dir,
    ))
    .exists()
}

/// `WhisperController::hasAnyModel`: any readable file entry that ends with
/// the model suffix and is not one of the two Silero VAD files.
pub fn has_any_model(config_dir: &str) -> bool {
    let Ok(entries) = std::fs::read_dir(models_directory(config_dir)) else {
        return false;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(filename) = name.to_str() else {
            continue;
        };
        if filename.starts_with('.')
            || !filename.ends_with(MODEL_SUFFIX)
            || filename == VAD_SILERO_5
            || filename == VAD_SILERO_6
        {
            continue;
        }
        let path = entry.path();
        if path.is_file() && std::fs::File::open(&path).is_ok() {
            return true;
        }
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
/// - the first existing candidate is returned joined into the model directory;
/// - otherwise a bare filename (no separator) falls back to the *last*
///   (deduplicated) candidate joined into the model directory;
/// - otherwise an absolute cleaned path relative to the current directory.
pub fn resolve_model_path(
    path: &str,
    suffixes: &[&str],
    config_dir: &str,
) -> String {
    if path.is_empty() {
        return String::new();
    }
    if Path::new(path).is_absolute() {
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
        let candidate = join_dir(&model_dir, filename);
        if Path::new(&candidate).exists() {
            return candidate;
        }
    }

    if is_bare_filename(path) {
        let last = filenames.last().map(String::as_str).unwrap_or(path);
        return join_dir(&model_dir, last);
    }
    to_absolute_path(path)
}

fn file_name(path: &str) -> &str {
    path.rsplit(std::path::is_separator).next().unwrap_or(path)
}

fn is_bare_filename(path: &str) -> bool {
    !path.is_empty()
        && !path.chars().any(std::path::is_separator)
        && !matches!(Path::new(path).components().next(), Some(Component::Prefix(_)))
}

fn join_dir(dir: &str, name: &str) -> String {
    if Path::new(name).is_absolute() {
        return name.to_string();
    }
    let base = if dir.is_empty() { "." } else { dir };
    if name.is_empty() {
        let trimmed = base.trim_end_matches(std::path::is_separator);
        return if trimmed.is_empty() {
            std::path::MAIN_SEPARATOR_STR.to_string()
        } else if cfg!(windows) && trimmed.len() == 2 && trimmed.ends_with(':') {
            format!("{trimmed}{}", std::path::MAIN_SEPARATOR)
        } else {
            trimmed.to_string()
        };
    }
    Path::new(base).join(name).to_string_lossy().into_owned()
}

fn to_absolute_path(path: &str) -> String {
    let base = if Path::new(path).is_absolute() {
        PathBuf::from(path)
    } else {
        match std::env::current_dir() {
            Ok(cwd) => cwd.join(path),
            Err(_) => PathBuf::from(path),
        }
    };
    let mut cleaned = PathBuf::new();
    for component in base.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if matches!(cleaned.components().next_back(), Some(Component::Normal(_))) {
                    cleaned.pop();
                } else if cleaned.as_os_str().is_empty()
                    || matches!(
                        cleaned.components().next_back(),
                        Some(Component::ParentDir)
                    )
                {
                    cleaned.push(component);
                }
            }
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
                cleaned.push(component.as_os_str());
            }
        }
    }
    let mut out = cleaned.to_string_lossy().into_owned();
    let had_trailing_sep = path
        .chars()
        .next_back()
        .is_some_and(std::path::is_separator);
    if had_trailing_sep
        && !out.is_empty()
        && !out
            .chars()
            .next_back()
            .is_some_and(std::path::is_separator)
    {
        out.push(std::path::MAIN_SEPARATOR);
    }
    out
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_temp_dir(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "memento_asr_whisper_test_{label}_{}_{nanos}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn vad_url_and_path_resolution() {
        assert_eq!(vad_model_url("/abs/ggml-silero-v5.1.2.bin"), "");
        assert_eq!(vad_model_url("unknown.bin"), "");
        assert_eq!(
            vad_model_url(VAD_SILERO_5),
            format!("{WHISPER_VAD_URL}{VAD_SILERO_5}")
        );

        let root = unique_temp_dir("paths");
        let root_str = root.to_string_lossy().into_owned();
        let models = PathBuf::from(models_directory(&root_str));
        fs::create_dir_all(&models).unwrap();

        assert!(!has_any_model(&root_str));
        fs::write(models.join(VAD_SILERO_5), b"vad").unwrap();
        assert!(!has_any_model(&root_str));

        let model_file = models.join("ggml-tiny.bin");
        fs::write(&model_file, b"model").unwrap();
        assert!(has_any_model(&root_str));
        assert!(model_available("tiny", &root_str));
        assert_eq!(
            resolve_model_path("ggml-tiny", &[MODEL_SUFFIX], &root_str),
            model_file.to_string_lossy()
        );
        assert_eq!(
            resolve_model_path("missing", &[MODEL_SUFFIX], &root_str),
            models.join("missing.bin").to_string_lossy()
        );
        assert_eq!(resolve_model_path("/abs/custom.bin", &[MODEL_SUFFIX], &root_str), "/abs/custom.bin");

        let rel_resolved = resolve_model_path("sub/../custom.bin", &[MODEL_SUFFIX], &root_str);
        assert!(Path::new(&rel_resolved).is_absolute());
        assert!(rel_resolved.ends_with("custom.bin"));
        assert!(!rel_resolved.contains(".."));

        let _ = fs::remove_dir_all(&root);
    }
}

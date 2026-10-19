//! Port of `asr/onnx/onnxcontroller.{h,cpp}` — the preset registry, model
//! directory, and availability pure core.
//!
//! The C++ class stays the `QObject` (settings plumbing, `QCoro` download
//! orchestration, backend creation with localized failure strings); the pure
//! registry/path/availability logic was converted here, reusing the verified
//! [`crate::onnx::onnxmodelconfig`] port for availability. Every contract is
//! cross-checked by `tests/mpv-memory-subtitles/controllers_equal_test.cpp`.

use crate::compat;
use crate::onnx::onnxmodelconfig;

/// `OnnxController::MODEL_CUSTOM` (settings key handling stays in C++, the
/// constant documents the registry vocabulary).
pub const MODEL_CUSTOM: &str = "custom";
/// `OnnxController::ONNX_MODEL_DIR` (`models`, under the config dir).
pub const ONNX_MODEL_DIR: &str = "models";
/// `OnnxController::PRESET_KITSUNE_T06B_JA`.
pub const PRESET_KITSUNE_T06B_JA: &str = "kitsune-transcribe-t0.6b-ja";
/// `OnnxController::PRESET_KITSUNE_P03B_JA`.
pub const PRESET_KITSUNE_P03B_JA: &str = "kitsune-transcribe-p0.3b-ja";

/// Mirror of `OnnxModelFileSpec` (`relativePath` + `url`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OnnxModelFileSpec {
    /// `OnnxModelFileSpec::relativePath`: where the file lands inside the
    /// model directory.
    pub relative_path: String,
    /// `OnnxModelFileSpec::url`: the exact URL string the C++ code composes
    /// (QUrl construction happens C++-side from this string).
    pub url: String,
}

/// `OnnxController::isManagedModel`.
pub fn is_managed_model(model: &str) -> bool {
    model == PRESET_KITSUNE_T06B_JA || model == PRESET_KITSUNE_P03B_JA
}

/// `OnnxController::modelFiles` — row-for-row identical list (the download
/// loop streams these in order).
pub fn model_files(model: &str) -> Vec<OnnxModelFileSpec> {
    if model == PRESET_KITSUNE_T06B_JA {
        let base =
            "https://huggingface.co/Multy123/kitsune-transcribe-t0.6b-ja/resolve/main/";
        return vec![
            spec("config.json", base),
            spec("generation_config.json", base),
            spec("processor_config.json", base),
            spec("tokenizer.json", base),
            spec("decoder_model_int8.onnx", base),
            spec("encoder_model_int8.onnx", base),
        ];
    }

    if model == PRESET_KITSUNE_P03B_JA {
        let base =
            "https://huggingface.co/Multy123/kitsune-transcribe-p0.3b-ja/resolve/main/";
        return vec![
            spec("config.json", base),
            spec("processor_config.json", base),
            spec("tokenizer.json", base),
            spec("model_int8.onnx", base),
        ];
    }

    Vec::new()
}

fn spec(relative_path: &str, base: &str) -> OnnxModelFileSpec {
    OnnxModelFileSpec {
        relative_path: relative_path.to_string(),
        url: format!(
            "{base}{}{relative_path}",
            if relative_path.ends_with(".onnx") { "onnx/" } else { "" }
        ),
    }
}

/// `OnnxController::modelsDirectory`.
pub fn models_directory(config_dir: &str) -> String {
    compat::qdir_join(config_dir, ONNX_MODEL_DIR)
}

/// `OnnxController::modelDirectory`.
pub fn model_directory(model: &str, config_dir: &str) -> String {
    compat::qdir_join(&models_directory(config_dir), model)
}

/// `OnnxController::modelAvailable` (delegates to the verified
/// `OnnxModelConfig` port exactly like the C++ method).
pub fn model_available(model: &str, config_dir: &str) -> bool {
    if model.is_empty() {
        return false;
    }
    let path = if is_managed_model(model) {
        model_directory(model, config_dir)
    } else {
        model.to_string()
    };
    // `loadFromPath` errors are not surfaced by the C++ method; only
    // validity matters here.
    let (config, _error) =
        onnxmodelconfig::OnnxModelConfig::load_from_path(path);
    config.is_valid()
}

/// `OnnxController::hasAnyModel`.
///
/// `selectedModelPath()` is resolved on the C++ side (it reads the Settings)
/// and passed in verbatim; the preset checks, the selected-path validity
/// check, and the readable-subdirectory scan mirror the C++ order.
pub fn has_any_model(selected_model_path: &str, config_dir: &str) -> bool {
    if model_available(PRESET_KITSUNE_T06B_JA, config_dir)
        || model_available(PRESET_KITSUNE_P03B_JA, config_dir)
    {
        return true;
    }

    if !selected_model_path.is_empty() {
        let (config, _error) = onnxmodelconfig::OnnxModelConfig::load_from_path(
            selected_model_path.to_string(),
        );
        if config.is_valid() {
            return true;
        }
    }

    for entry in compat::entry_dirs_readable(&models_directory(config_dir)) {
        let (config, _error) = onnxmodelconfig::OnnxModelConfig::load_from_path(
            entry.absolute_path,
        );
        if config.is_valid() {
            return true;
        }
    }
    false
}

////////////////////////////////////////////////////////////////////////////////
//
// Copyright (c) 2026 Ripose
//
// This file is part of Memento.
//
// Memento is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, version 2 of the License.
//
// Memento is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with Memento.  If not, see <https://www.gnu.org/licenses/>.
//
////////////////////////////////////////////////////////////////////////////////

//! Port of `src/asr/onnx/onnxmodelconfig.{h,cpp}` — C++ `OnnxModelConfig`.
//!
//! Parsed configuration and resolved file paths for an ONNX ASR model
//! directory (or a file inside it).
//!
//! Conversion notes (C++ → Rust):
//! - `loadFromPath(path, error)` maps to `load_from_path(path)`
//!   returning `(config, error)`; an empty `error` means success, and the
//!   (partially filled) config is still returned on failure, exactly like the
//!   C++ out-parameter convention.
//! - `QFileInfo::absoluteFilePath`/`absolutePath` map to a lexical
//!   absolutization that resolves against the current working directory and
//!   collapses `.`/`..` components WITHOUT resolving symlinks (the same
//!   semantics as `QDir::cleanPath`; deliberately unlike
//!   `std::fs::canonicalize`).
//! - JSON numeric reads mirror `QJsonValue::toInteger`/`toInt`/`toDouble`;
//!   `toInteger`/`toInt` accept only whole, in-range numbers (including
//!   lossless 64-bit JSON integers) and otherwise use the supplied default.
//!   `QJsonValue::contains(key)` maps to key presence in the JSON object.
//! - File existence checks mirror `QFileInfo::exists` (any entry, symlinks
//!   followed) and `QFileInfo::isFile` (regular file or symlink to one, i.e.
//!   `Path::is_file`).

use std::fs;
use std::path::{Component, Path, PathBuf};

use crate::audio::melfrontend::MelFrontendConfig;

/// C++: `enum class OnnxAsrArchitecture`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OnnxAsrArchitecture {
    Unknown,
    /// Encoder-Decoder autoregressive ASR (e.g., CohereAsr /
    /// kitsune-transcribe-t0.6b-ja)
    EncoderDecoder,
    /// Encoder-only CTC ASR (e.g., ParakeetForCTC / kitsune-transcribe-p0.3b-ja)
    Ctc,
}

/// C++: `struct OnnxModelConfig`
#[derive(Debug, Clone, PartialEq)]
pub struct OnnxModelConfig {
    pub architecture: OnnxAsrArchitecture,
    pub model_type: String,
    pub model_dir: PathBuf,

    /* ONNX graph paths */
    pub encoder_onnx_path: PathBuf,
    pub decoder_onnx_path: PathBuf,
    pub ctc_onnx_path: PathBuf,
    pub tokenizer_json_path: PathBuf,

    /* Audio frontend parameters */
    pub frontend: MelFrontendConfig,
    pub subsampling_factor: i32,
    pub max_audio_clip_seconds: f64,

    /* Vocabulary & special token IDs */
    pub vocab_size: i64,
    pub eos_token_id: i64,
    pub pad_token_id: i64,
    pub bos_token_id: i64,
    pub no_speech_token_id: i64,
    pub ctc_blank_token_id: i64,
}

impl Default for OnnxModelConfig {
    /// Mirrors the C++ in-class member initializers.
    fn default() -> Self {
        Self {
            architecture: OnnxAsrArchitecture::Unknown,
            model_type: String::new(),
            model_dir: PathBuf::new(),
            encoder_onnx_path: PathBuf::new(),
            decoder_onnx_path: PathBuf::new(),
            ctc_onnx_path: PathBuf::new(),
            tokenizer_json_path: PathBuf::new(),
            frontend: MelFrontendConfig::default(),
            subsampling_factor: 8,
            max_audio_clip_seconds: 35.0,
            vocab_size: 0,
            eos_token_id: 3,
            pad_token_id: 2,
            bos_token_id: 4,
            no_speech_token_id: 1,
            ctc_blank_token_id: -1,
        }
    }
}

/// C++: `QJsonValue::toInteger(defaultValue)` — only whole numbers
/// representable as qint64 convert. In particular a fractional JSON number
/// must not silently truncate, and a JSON integer above 2^53 must not be
/// rounded through f64 before conversion.
fn to_integer(value: Option<&serde_json::Value>, default: i64) -> i64 {
    let Some(serde_json::Value::Number(number)) = value else {
        return default;
    };
    if let Some(integer) = number.as_i64() {
        return integer;
    }
    let Some(double) = number.as_f64() else {
        return default;
    };
    if double.is_finite()
        && double.fract() == 0.0
        && double >= i64::MIN as f64
        && double < 9_223_372_036_854_775_808.0
    {
        double as i64
    } else {
        default
    }
}

/// C++: `QJsonValue::toInt(defaultValue)` rejects non-integral and
/// out-of-i32-range values rather than narrowing a qint64 modulo 2^32.
fn to_int(value: Option<&serde_json::Value>, default: i32) -> i32 {
    i32::try_from(to_integer(value, default as i64)).unwrap_or(default)
}

/// C++: `QJsonValue::toDouble(defaultValue)`.
fn to_double(value: Option<&serde_json::Value>, default: f64) -> f64 {
    match value {
        Some(serde_json::Value::Number(number)) => {
            let double = number.as_f64().unwrap_or(f64::INFINITY);
            if double.is_finite() {
                double
            } else {
                default
            }
        }
        _ => default,
    }
}

/// C++: `QJsonValue::toString()` — empty string when the value is not a
/// string.
fn to_string_qt(value: Option<&serde_json::Value>) -> String {
    value
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
        .to_owned()
}

/// C++: `QJsonObject::contains(key)`.
fn contains(object: &serde_json::Value, key: &str) -> bool {
    object.get(key).is_some()
}

/// C++: `readJsonObject(path)` in the anonymous namespace — an empty object
/// is returned (without any error) when the file cannot be opened or is not a
/// JSON object.
fn read_json_object(path: &Path) -> serde_json::Value {
    fn empty_object() -> serde_json::Value {
        serde_json::Value::Object(serde_json::Map::new())
    }
    let Ok(bytes) = fs::read(path) else {
        return empty_object();
    };
    // QJsonDocument::fromJson skips a leading UTF-8 byte-order mark.
    let bytes: &[u8] = bytes
        .strip_prefix(&[0xEF, 0xBB, 0xBF])
        .unwrap_or_else(|| bytes.as_slice());
    match serde_json::from_slice::<serde_json::Value>(bytes) {
        Ok(root) if root.is_object() => root,
        // Mirrors the C++ `!doc.isObject()` half: failures yield an empty
        // object silently.
        _ => empty_object(),
    }
}

/// C++: `findFirstExistingFile(dir, relativeCandidates)` — the first
/// candidate that exists AND is a regular file (dirs and broken symlinks are
/// skipped), resolved relative to `dir`.
fn find_first_existing_file(dir: &Path, relative_candidates: &[&str]) -> Option<PathBuf> {
    for rel in relative_candidates {
        let full_path = dir.join(rel);
        // C++: QFileInfo::exists(fullPath) && QFileInfo(fullPath).isFile()
        if full_path.is_file() {
            return Some(full_path);
        }
    }
    None
}

/// C++ semantics of `QFileInfo::absoluteFilePath()`: resolve against the
/// current working directory and collapse `.`/`..` components lexically,
/// WITHOUT resolving symlinks (unlike `fs::canonicalize`).
fn absolute_path(path: &Path) -> PathBuf {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        match std::env::current_dir() {
            Ok(cwd) => cwd.join(path),
            // Unreachable in practice (working directory removed): behave
            // like Qt's best-effort fallback and leave the path untouched.
            Err(_) => return path.to_path_buf(),
        }
    };
    let mut cleaned = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                // Lexical "..": pop the previous component when there is one,
                // otherwise keep the ".." (leading ".." / ".." past root,
                // matching QDir::cleanPath).
                if !cleaned.pop() {
                    cleaned.push(component);
                }
            }
            other => cleaned.push(other.as_os_str()),
        }
    }
    cleaned
}

impl OnnxModelConfig {
    /// C++: `isValid()`.
    pub fn is_valid(&self) -> bool {
        self.architecture != OnnxAsrArchitecture::Unknown
            && !self.tokenizer_json_path.as_os_str().is_empty()
    }

    /// C++: `OnnxModelConfig::loadFromPath(path, error)`.
    ///
    /// Returns `(config, error)`; an empty error means success. On failure
    /// the partially filled config is still returned, mirroring the C++
    /// out-parameter convention (`QString &error`).
    ///
    /// Loads and validates model configuration from a directory or file path.
    pub fn load_from_path<P: AsRef<Path>>(path: P) -> (OnnxModelConfig, String) {
        let original = path.as_ref();
        let mut cfg = OnnxModelConfig::default();

        if original.as_os_str().is_empty() {
            let error = "ONNX model path is empty.".to_string();
            return (cfg, error);
        }

        if !original.exists() {
            let error = format!("ONNX model path does not exist: {}", original.display());
            return (cfg, error);
        }

        // C++: QDir dir(info.isDir() ? info.absoluteFilePath() : info.absolutePath())
        let mut dir = if original.is_dir() {
            absolute_path(original)
        } else {
            let info_absolute = absolute_path(original);
            info_absolute
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| info_absolute.clone())
        };

        // If pointed inside an "onnx" subdirectory, check parent directory
        // for config/tokenizer
        if !dir.join("tokenizer.json").exists() && dir_name(&dir) == "onnx" {
            // C++ QDir::cdUp(): one lexical level up; returns false at the
            // filesystem root, which Path::parent() models by returning None.
            let parent_dir = dir.parent().map(Path::to_path_buf);
            match parent_dir {
                Some(parent_dir) if parent_dir.join("tokenizer.json").exists() => {
                    dir = parent_dir;
                }
                _ => {}
            }
        }
        cfg.model_dir = dir.clone();

        cfg.tokenizer_json_path = find_first_existing_file(
            &dir,
            &[
                "tokenizer.json",
                "onnx/tokenizer.json",
            ],
        )
        .unwrap_or_default();
        if cfg.tokenizer_json_path.as_os_str().is_empty() {
            let error = format!(
                "tokenizer.json was not found in {}",
                cfg.model_dir.display()
            );
            return (cfg, error);
        }

        // Locate ONNX graph files (preferring _int8 single-file models when
        // both exist, or user-selected file)
        cfg.encoder_onnx_path = find_first_existing_file(
            &dir,
            &[
                "encoder_model_int8.onnx",
                "onnx/encoder_model_int8.onnx",
                "encoder_model_quantized.onnx",
                "onnx/encoder_model_quantized.onnx",
                "encoder_model.onnx",
                "onnx/encoder_model.onnx",
            ],
        )
        .unwrap_or_default();
        cfg.decoder_onnx_path = find_first_existing_file(
            &dir,
            &[
                "decoder_model_int8.onnx",
                "onnx/decoder_model_int8.onnx",
                "decoder_model_merged_int8.onnx",
                "onnx/decoder_model_merged_int8.onnx",
                "decoder_model_merged_quantized.onnx",
                "onnx/decoder_model_merged_quantized.onnx",
                "decoder_model_quantized.onnx",
                "onnx/decoder_model_quantized.onnx",
                "decoder_model_merged.onnx",
                "onnx/decoder_model_merged.onnx",
                "decoder_model.onnx",
                "onnx/decoder_model.onnx",
            ],
        )
        .unwrap_or_default();
        cfg.ctc_onnx_path = find_first_existing_file(
            &dir,
            &[
                "model_int8.onnx",
                "onnx/model_int8.onnx",
                "model_quantized.onnx",
                "onnx/model_quantized.onnx",
                "model.onnx",
                "onnx/model.onnx",
            ],
        )
        .unwrap_or_default();

        let info_is_file = original.is_file();
        let info_suffix_matches_onnx = suffix_of(original).eq_ignore_ascii_case("onnx");
        if info_is_file && info_suffix_matches_onnx {
            let file_name = original
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            if !file_name.starts_with("encoder_") && !file_name.starts_with("decoder_") {
                cfg.ctc_onnx_path = absolute_path(original);
            }
        }

        // Parse config.json
        let config_json_path = find_first_existing_file(&dir, &["config.json", "onnx/config.json"])
            .unwrap_or_default();
        let root_config_json = if config_json_path.as_os_str().is_empty() {
            // C++: QJsonObject{} for a missing config.json
            serde_json::Value::Object(serde_json::Map::new())
        } else {
            read_json_object(&config_json_path)
        };

        cfg.model_type = to_string_qt(root_config_json.get("model_type"));
        if contains(&root_config_json, "subsampling_factor") {
            cfg.subsampling_factor =
                to_int(root_config_json.get("subsampling_factor"), 8);
        }
        if contains(&root_config_json, "vocab_size") {
            cfg.vocab_size = to_integer(root_config_json.get("vocab_size"), 0);
        }
        if contains(&root_config_json, "eos_token_id") {
            cfg.eos_token_id = to_integer(root_config_json.get("eos_token_id"), 3);
        }
        if contains(&root_config_json, "pad_token_id") {
            cfg.pad_token_id = to_integer(root_config_json.get("pad_token_id"), 2);
        }
        if contains(&root_config_json, "bos_token_id") {
            cfg.bos_token_id = to_integer(root_config_json.get("bos_token_id"), 4);
        }

        if root_config_json
            .get("encoder_config")
            .is_some_and(serde_json::Value::is_object)
        {
            let enc_obj = root_config_json.get("encoder_config").unwrap();
            if contains(enc_obj, "subsampling_factor") {
                cfg.subsampling_factor =
                    to_int(enc_obj.get("subsampling_factor"), cfg.subsampling_factor);
            }
            if contains(enc_obj, "num_mel_bins") {
                cfg.frontend.n_mels = to_int(enc_obj.get("num_mel_bins"), cfg.frontend.n_mels);
            }
        }
        if root_config_json
            .get("decoder_config")
            .is_some_and(serde_json::Value::is_object)
        {
            let dec_obj = root_config_json.get("decoder_config").unwrap();
            if contains(dec_obj, "vocab_size") {
                cfg.vocab_size = to_integer(dec_obj.get("vocab_size"), cfg.vocab_size);
            }
        }

        // Parse generation_config.json if available
        let gen_config_path = find_first_existing_file(
            &dir,
            &[
                "generation_config.json",
                "onnx/generation_config.json",
            ],
        )
        .unwrap_or_default();
        if !gen_config_path.as_os_str().is_empty() {
            let gen_obj = read_json_object(&gen_config_path);
            if contains(&gen_obj, "eos_token_id") {
                cfg.eos_token_id = to_integer(gen_obj.get("eos_token_id"), cfg.eos_token_id);
            }
            if contains(&gen_obj, "pad_token_id") {
                cfg.pad_token_id = to_integer(gen_obj.get("pad_token_id"), cfg.pad_token_id);
            }
            if contains(&gen_obj, "bos_token_id") {
                cfg.bos_token_id = to_integer(gen_obj.get("bos_token_id"), cfg.bos_token_id);
            }
        }

        // Parse processor_config.json or preprocessor_config.json
        let proc_config_path = find_first_existing_file(
            &dir,
            &[
                "processor_config.json",
                "preprocessor_config.json",
                "onnx/processor_config.json",
                "onnx/preprocessor_config.json",
            ],
        )
        .unwrap_or_default();
        if !proc_config_path.as_os_str().is_empty() {
            let proc_root = read_json_object(&proc_config_path);
            // C++: value("feature_extractor").isObject() ? .toObject() : root
            let feat_obj = proc_root
                .get("feature_extractor")
                .filter(|value| value.is_object())
                .unwrap_or(&proc_root);

            if contains(feat_obj, "sampling_rate") {
                cfg.frontend.sample_rate = to_int(feat_obj.get("sampling_rate"), 16000);
            }
            if contains(feat_obj, "n_fft") {
                cfg.frontend.n_fft = to_int(feat_obj.get("n_fft"), 512);
            }
            let win_length_key = contains(feat_obj, "win_length")
                .then_some("win_length")
                .or_else(|| contains(feat_obj, "n_window_size").then_some("n_window_size"));
            if let Some(key) = win_length_key {
                cfg.frontend.win_length = to_int(feat_obj.get(key), 400);
            }
            let hop_length_key = contains(feat_obj, "hop_length")
                .then_some("hop_length")
                .or_else(|| contains(feat_obj, "n_window_stride").then_some("n_window_stride"));
            if let Some(key) = hop_length_key {
                cfg.frontend.hop_length = to_int(feat_obj.get(key), 160);
            }
            let n_mels_key = contains(feat_obj, "feature_size")
                .then_some("feature_size")
                .or_else(|| contains(feat_obj, "num_mel_bins").then_some("num_mel_bins"));
            if let Some(key) = n_mels_key {
                cfg.frontend.n_mels = to_int(feat_obj.get(key), cfg.frontend.n_mels);
            }
            if contains(feat_obj, "preemphasis") {
                cfg.frontend.preemphasis = to_double(feat_obj.get("preemphasis"), 0.97) as f32;
            }
            if contains(feat_obj, "max_audio_clip_s") {
                cfg.max_audio_clip_seconds =
                    to_double(feat_obj.get("max_audio_clip_s"), 35.0);
            }
            if contains(feat_obj, "normalize") {
                let norm_mode = to_string_qt(feat_obj.get("normalize"));
                cfg.frontend.normalize_per_feature = norm_mode == "per_feature";
            }
        }

        // Determine model architecture
        if !cfg.encoder_onnx_path.as_os_str().is_empty()
            && !cfg.decoder_onnx_path.as_os_str().is_empty()
        {
            cfg.architecture = OnnxAsrArchitecture::EncoderDecoder;
        } else if !cfg.ctc_onnx_path.as_os_str().is_empty() {
            cfg.architecture = OnnxAsrArchitecture::Ctc;
            if cfg.ctc_blank_token_id < 0 {
                cfg.ctc_blank_token_id = if cfg.pad_token_id > 0 {
                    cfg.pad_token_id
                } else {
                    cfg.vocab_size
                };
            }
            // NeMo parakeet encoder config quirk: the (root-level) mel size
            // is only consulted when the config really is the 128-bin
            // variant and does not declare an encoder_config.
            if cfg.model_type == "parakeet_ctc"
                && cfg.frontend.n_mels == 128
                && !contains(&root_config_json, "encoder_config")
            {
                cfg.frontend.n_mels = to_int(root_config_json.get("num_mel_bins"), 80);
            }
        } else {
            let error = format!(
                "No ONNX model files (encoder_model*.onnx + decoder_model*.onnx or model*.onnx) found in {}",
                cfg.model_dir.display()
            );
            return (cfg, error);
        }

        (cfg, String::new())
    }
}

/// C++: `QDir::dirName()` — the last component of the directory path (empty
/// for the filesystem root).
fn dir_name(dir: &Path) -> String {
    dir.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// C++: `QFileInfo::suffix()` — the final extension segment of the file name
/// (`None` in Rust's model means the same as the C++ empty suffix, e.g. for
/// leading-dot "hidden" names).
fn suffix_of(path: &Path) -> String {
    path.extension()
        .map(|ext| ext.to_string_lossy().into_owned())
        .unwrap_or_default()
}

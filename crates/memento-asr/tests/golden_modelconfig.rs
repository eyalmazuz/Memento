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

//! Golden tests for `onnx::onnxmodelconfig`, mirroring the C++ unit test
//! `tests/mpv-memory-subtitles/onnx_asr_unit_test.cpp` (function
//! `testOnnxTokenizerAndConfig`, scenarios 2 & 3) plus negative-path
//! coverage of the exact C++ error strings and path-resolution rules.
//!
//! Note on the C++ test layout: scenario 2 reuses the temp dir of the
//! tokenizer scenario, so its `tokenizer.json` is the synthetic fixture
//! produced there. It is recreated here so this scenario is self-contained.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use memento_asr::audio::melfrontend::MelFrontendConfig;
use memento_asr::onnx::onnxmodelconfig::{OnnxAsrArchitecture, OnnxModelConfig};

fn unique_temp_dir(tag: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!(
        "memento-asr-golden-modelconfig-{tag}-{}-{nanos}",
        std::process::id()
    ));
    fs::create_dir_all(&dir).expect("failed to create temporary directory");
    dir
}

fn write_file(path: &Path, contents: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, contents).unwrap();
}

/// Identical JSON fixtures to the C++ test.
const CONFIG_COHERE: &str = r#"{
    "model_type": "cohere_asr",
    "bos_token_id": 13764,
    "eos_token_id": 1,
    "pad_token_id": 0,
    "max_target_positions": 1024,
    "encoder_config": { "subsampling_factor": 8 }
}"#;

const PROCESSOR_COHERE: &str = r#"{
    "feature_extractor": {
        "feature_size": 128,
        "sampling_rate": 16000,
        "hop_length": 160,
        "n_fft": 512,
        "win_length": 400,
        "preemphasis": 0.97
    }
}"#;

const CONFIG_PARAKEET: &str = r#"{
    "model_type": "parakeet_ctc",
    "pad_token_id": 3072,
    "vocab_size": 3073,
    "encoder_config": { "subsampling_factor": 8 }
}"#;

const PROCESSOR_PARAKEET: &str = r#"{
    "feature_extractor": {
        "feature_size": 80,
        "sampling_rate": 16000
    }
}"#;

/// Same synthetic tokenizer.json as the tokenizer golden test builds (the
/// C++ scenario-2 dir already contains it since scenario 1 ran there first).
fn write_synthetic_tokenizer_json(dir: &Path) -> PathBuf {
    let root = serde_json::json!({
        "model": {
            "vocab": {
                "<pad>": 0,
                "<eos>": 1,
                "<unk>": 2,
                "▁": 13764,
                "▁日本": 100,
                "語": 101,
                "<0xE3>": 200,
                "<0x81>": 201,
                "<0x82>": 202,
            }
        },
        "added_tokens": [
            { "id": 4,  "content": "<|startoftranscript|>", "special": true },
            { "id": 5,  "content": "<|pnc|>",               "special": true },
            { "id": 6,  "content": "<|nopnc|>",             "special": true },
            { "id": 7,  "content": "<|startofcontext|>",    "special": true },
            { "id": 9,  "content": "<|noitn|>",             "special": true },
            { "id": 11, "content": "<|notimestamp|>",       "special": true },
            { "id": 13, "content": "<|nodiarize|>",         "special": true },
            { "id": 16, "content": "<|emo:undefined|>",     "special": true },
            { "id": 98, "content": "<|ja|>",                "special": true },
        ],
    });
    let path = dir.join("tokenizer.json");
    fs::write(&path, serde_json::to_vec(&root).unwrap()).unwrap();
    path
}

/// C++ scenario 2: EncoderDecoder config detection
/// (kitsune-transcribe-t0.6b-ja layout).
#[test]
fn test_onnx_model_config_encoder_decoder() {
    let dir = unique_temp_dir("cohere");
    write_synthetic_tokenizer_json(&dir);
    write_file(&dir.join("config.json"), CONFIG_COHERE);
    write_file(&dir.join("processor_config.json"), PROCESSOR_COHERE);
    fs::create_dir(dir.join("onnx")).unwrap();
    write_file(&dir.join("onnx/encoder_model_quantized.onnx"), "dummy");
    write_file(&dir.join("onnx/decoder_model_merged_quantized.onnx"), "dummy");

    let (model_cfg, err) = OnnxModelConfig::load_from_path(dir.as_path());
    assert!(
        err.is_empty(),
        "OnnxModelConfig::loadFromPath (cohere_asr) failed: {err}"
    );
    assert!(model_cfg.is_valid(), "Expected cohere_asr config to be valid");
    assert_eq!(
        model_cfg.architecture,
        OnnxAsrArchitecture::EncoderDecoder,
        "Expected EncoderDecoder architecture"
    );
    assert_eq!(model_cfg.frontend.n_mels, 128, "Expected 128 mel bins");
    assert!(
        model_cfg.bos_token_id == 13764 && model_cfg.eos_token_id == 1,
        "BOS/EOS token IDs mismatch"
    );

    // --- supplemental locks on the same C++ semantics ----------------------

    // Candidate order resolved the onnx/-interleaved quantized variants.
    assert_eq!(model_cfg.model_dir, dir);
    assert_eq!(model_cfg.tokenizer_json_path, dir.join("tokenizer.json"));
    assert_eq!(
        model_cfg.encoder_onnx_path,
        dir.join("onnx/encoder_model_quantized.onnx")
    );
    assert_eq!(
        model_cfg.decoder_onnx_path,
        dir.join("onnx/decoder_model_merged_quantized.onnx")
    );
    assert!(model_cfg.ctc_onnx_path.as_os_str().is_empty());
    assert_eq!(model_cfg.ctc_blank_token_id, -1); // only resolved for Ctc

    // config.json / processor_config.json field mapping with the C++
    // defaults left intact where keys are absent.
    assert_eq!(model_cfg.model_type, "cohere_asr");
    assert_eq!(model_cfg.pad_token_id, 0);
    assert_eq!(model_cfg.vocab_size, 0);
    assert_eq!(model_cfg.subsampling_factor, 8);
    assert_eq!(model_cfg.max_audio_clip_seconds, 35.0);
    // Every processor_config value equals the MelFrontendConfig defaults,
    // and "normalize" is absent so the default stays in effect.
    assert_eq!(model_cfg.frontend, MelFrontendConfig::default());
    assert!(model_cfg.frontend.normalize_per_feature);
}

/// C++ scenario 3: Parakeet CTC config detection
/// (kitsune-transcribe-p0.3b-ja layout) — its own temp directory like the
/// C++ `ctcDir`.
#[test]
fn test_onnx_model_config_parakeet_ctc() {
    let dir = unique_temp_dir("parakeet");
    write_file(
        &dir.join("tokenizer.json"),
        r#"{"model":{"vocab":{"<unk>":0,"a":1,"<pad>":3072}}}"#,
    );
    write_file(&dir.join("config.json"), CONFIG_PARAKEET);
    write_file(&dir.join("processor_config.json"), PROCESSOR_PARAKEET);
    fs::create_dir(dir.join("onnx")).unwrap();
    write_file(&dir.join("onnx/model_quantized.onnx"), "dummy");

    let (ctc_cfg, err) = OnnxModelConfig::load_from_path(dir.as_path());
    assert!(
        err.is_empty(),
        "OnnxModelConfig::loadFromPath (parakeet_ctc) failed: {err}"
    );
    assert!(ctc_cfg.is_valid(), "Expected parakeet_ctc config to be valid");
    assert_eq!(
        ctc_cfg.architecture,
        OnnxAsrArchitecture::Ctc,
        "Expected Ctc architecture"
    );
    assert!(
        ctc_cfg.frontend.n_mels == 80 && ctc_cfg.ctc_blank_token_id == 3072,
        "Expected 80 mel bins and ctcBlankTokenId 3072"
    );

    // --- supplemental locks on the same C++ semantics ----------------------

    // encoder_config is present, so the parakeet_ctc 128-mel root fallback
    // must NOT have run; ctcBlank resolves from pad (> 0), not vocab size.
    assert_eq!(ctc_cfg.model_dir, dir);
    assert_eq!(ctc_cfg.tokenizer_json_path, dir.join("tokenizer.json"));
    assert_eq!(ctc_cfg.ctc_onnx_path, dir.join("onnx/model_quantized.onnx"));
    assert!(ctc_cfg.encoder_onnx_path.as_os_str().is_empty());
    assert!(ctc_cfg.decoder_onnx_path.as_os_str().is_empty());
    assert_eq!(ctc_cfg.model_type, "parakeet_ctc");
    assert_eq!(ctc_cfg.pad_token_id, 3072);
    assert_eq!(ctc_cfg.vocab_size, 3073);
    assert_eq!(ctc_cfg.eos_token_id, 3); // C++ default member initializer
    assert_eq!(ctc_cfg.bos_token_id, 4);
    assert_eq!(ctc_cfg.subsampling_factor, 8);
    assert_eq!(
        ctc_cfg.frontend,
        MelFrontendConfig {
            n_mels: 80,
            ..MelFrontendConfig::default()
        }
    );
}

/// Negative cases: empty/nonexistent paths, missing tokenizer.json, no ONNX
/// model files — all with the exact C++ error strings.
#[test]
fn test_onnx_model_config_negative_paths() {
    // Empty path.
    let (cfg, err) = OnnxModelConfig::load_from_path("");
    assert_eq!(err, "ONNX model path is empty.");
    assert_eq!(cfg.architecture, OnnxAsrArchitecture::Unknown);
    assert!(!cfg.is_valid());

    // Nonexistent path (message carries the path exactly as given).
    let ghost = Path::new("/definitely-not-a-real-memento-model-dir");
    let (cfg, err) = OnnxModelConfig::load_from_path(ghost);
    assert_eq!(
        err,
        format!("ONNX model path does not exist: {}", ghost.display())
    );
    assert_eq!(cfg, OnnxModelConfig::default());

    // A directory containing only model.onnx: the tokenizer check fires
    // before any ONNX file scan.
    let dir = unique_temp_dir("only-model-onnx");
    write_file(&dir.join("model.onnx"), "dummy");
    let (cfg, err) = OnnxModelConfig::load_from_path(dir.as_path());
    assert_eq!(
        err,
        format!("tokenizer.json was not found in {}", dir.display())
    );
    assert_eq!(cfg.architecture, OnnxAsrArchitecture::Unknown);
    assert!(!cfg.is_valid());

    // tokenizer.json present but no ONNX graph files: exact error text, and
    // a directory named "model.onnx" (exists, but not a file) must not
    // satisfy a candidate.
    let dir = unique_temp_dir("no-model-files");
    write_file(&dir.join("tokenizer.json"), "{}");
    fs::create_dir(dir.join("model.onnx")).unwrap();
    let (cfg, err) = OnnxModelConfig::load_from_path(dir.as_path());
    assert_eq!(
        err,
        format!(
            "No ONNX model files (encoder_model*.onnx + decoder_model*.onnx or model*.onnx) found in {}",
            dir.display()
        )
    );
    assert_eq!(cfg.architecture, OnnxAsrArchitecture::Unknown);
    assert_eq!(cfg.tokenizer_json_path, dir.join("tokenizer.json"));
    assert!(!cfg.is_valid());
}

/// The "onnx" parent-hop trick: pointing into an `onnx` subdirectory falls
/// back to the parent dir's config/tokenizer.
#[test]
fn test_onnx_model_config_onnx_subdir_parent_hop() {
    let dir = unique_temp_dir("hop");
    write_file(&dir.join("tokenizer.json"), "{}");
    fs::create_dir(dir.join("onnx")).unwrap();
    write_file(&dir.join("onnx/model.onnx"), "dummy");

    let (cfg, err) = OnnxModelConfig::load_from_path(dir.join("onnx"));
    assert!(
        err.is_empty(),
        "expected the onnx/ parent hop to succeed: {err}"
    );
    assert_eq!(cfg.model_dir, dir);
    assert_eq!(cfg.tokenizer_json_path, dir.join("tokenizer.json"));
    assert_eq!(cfg.ctc_onnx_path, dir.join("onnx/model.onnx"));
    assert_eq!(cfg.architecture, OnnxAsrArchitecture::Ctc);
    assert!(cfg.is_valid());

    // Without tokenizer.json in the parent, the hop is declined; the error
    // refers to the onnx subdirectory itself.
    let dir = unique_temp_dir("hop-declined");
    fs::create_dir(dir.join("onnx")).unwrap();
    write_file(&dir.join("onnx/model.onnx"), "dummy");
    let onnx_dir = dir.join("onnx");
    let (cfg, err) = OnnxModelConfig::load_from_path(&onnx_dir);
    assert_eq!(
        err,
        format!("tokenizer.json was not found in {}", onnx_dir.display())
    );
    assert_eq!(cfg.model_dir, onnx_dir);
    assert_eq!(cfg.architecture, OnnxAsrArchitecture::Unknown);
}

/// A user-selected `.onnx` file takes the CTC slot unless its name starts
/// with "encoder_"/"decoder_" (case-sensitive prefix, case-insensitive
/// suffix check like the C++ code).
#[test]
fn test_onnx_model_config_user_selected_file_override() {
    // Plain file name: overrides the CTC path and selects Ctc.
    let dir = unique_temp_dir("override");
    write_file(&dir.join("tokenizer.json"), "{}");
    write_file(&dir.join("other_model.onnx"), "dummy");
    let (cfg, err) = OnnxModelConfig::load_from_path(dir.join("other_model.onnx"));
    assert!(err.is_empty(), "unexpected error: {err}");
    assert_eq!(cfg.architecture, OnnxAsrArchitecture::Ctc);
    assert_eq!(cfg.ctc_onnx_path, dir.join("other_model.onnx"));
    assert_eq!(cfg.model_dir, dir);

    // Suffix comparison is case-insensitive ("ONNX").
    let dir = unique_temp_dir("override-case");
    write_file(&dir.join("tokenizer.json"), "{}");
    write_file(&dir.join("picked.ONNX"), "dummy");
    let (cfg, err) = OnnxModelConfig::load_from_path(dir.join("picked.ONNX"));
    assert!(err.is_empty(), "unexpected error: {err}");
    assert_eq!(cfg.ctc_onnx_path, dir.join("picked.ONNX"));
    assert_eq!(cfg.architecture, OnnxAsrArchitecture::Ctc);

    // decoder_-prefixed files never take the CTC slot; encoder_model.onnx
    // alone is still an incomplete (encoder-only) graph → exact error.
    let dir = unique_temp_dir("override-decoder");
    write_file(&dir.join("tokenizer.json"), "{}");
    write_file(&dir.join("encoder_model.onnx"), "dummy");
    let encoder_file = dir.join("encoder_model.onnx");
    let (cfg, err) = OnnxModelConfig::load_from_path(&encoder_file);
    assert_eq!(
        err,
        format!(
            "No ONNX model files (encoder_model*.onnx + decoder_model*.onnx or model*.onnx) found in {}",
            dir.display()
        )
    );
    assert_eq!(cfg.encoder_onnx_path, encoder_file);
    assert!(cfg.ctc_onnx_path.as_os_str().is_empty());
    assert_eq!(cfg.architecture, OnnxAsrArchitecture::Unknown);
}

/// Qt's `QJsonValue::toInt`/`toInteger` reject fractional or out-of-range
/// numbers, but preserve 64-bit integral JSON tokens without an f64 roundtrip.
#[test]
fn test_onnx_model_config_json_integer_fallbacks() {
    let dir = unique_temp_dir("json-integer-conversions");
    write_file(&dir.join("tokenizer.json"), "{}");
    write_file(&dir.join("model.onnx"), "dummy");
    write_file(
        &dir.join("config.json"),
        r#"{"model_type":"ctc","subsampling_factor":8.5,
             "vocab_size":9007199254740993,"eos_token_id":1.5,
             "pad_token_id":2147483648}"#,
    );
    write_file(
        &dir.join("processor_config.json"),
        r#"{"feature_extractor":{"sampling_rate":16000.5,
             "n_fft":2147483648,"hop_length":200.0,
             "feature_size":-2147483649}}"#,
    );

    let (cfg, err) = OnnxModelConfig::load_from_path(&dir);
    assert!(err.is_empty(), "unexpected config error: {err}");
    assert_eq!(cfg.architecture, OnnxAsrArchitecture::Ctc);
    assert_eq!(cfg.subsampling_factor, 8);
    assert_eq!(cfg.vocab_size, 9_007_199_254_740_993);
    assert_eq!(cfg.eos_token_id, 3);
    assert_eq!(cfg.pad_token_id, 2_147_483_648);
    assert_eq!(cfg.ctc_blank_token_id, 2_147_483_648);
    assert_eq!(cfg.frontend.sample_rate, 16_000);
    assert_eq!(cfg.frontend.n_fft, 512);
    assert_eq!(cfg.frontend.hop_length, 200);
    assert_eq!(cfg.frontend.n_mels, 128);
}

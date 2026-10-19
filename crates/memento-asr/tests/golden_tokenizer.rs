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

//! Golden tests for `onnx::onnxtokenizer`, mirroring the C++ unit test
//! `tests/mpv-memory-subtitles/onnx_asr_unit_test.cpp` (function
//! `testOnnxTokenizerAndConfig`, scenario 1) plus additional pure-logic
//! coverage of the same semantics the C++ file implements.
//!
//! Documented divergence: for invalid JSON the C++ error text embeds
//! `QJsonParseError::errorString()` (Qt-specific wording, e.g.
//! "invalid buffer; line 2, column 1") while the Rust port embeds the
//! serde_json error text. Only the exact C++ prefix
//! "Invalid JSON in tokenizer.json: " is asserted for those cases.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use memento_asr::onnx::onnxtokenizer::OnnxTokenizer;

fn unique_temp_dir(tag: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!(
        "memento-asr-golden-tokenizer-{tag}-{}-{nanos}",
        std::process::id()
    ));
    fs::create_dir_all(&dir).expect("failed to create temporary directory");
    dir
}

/// The synthetic BPE `tokenizer.json` fixture from the C++ test: a
/// `model.vocab` object built with byte-fallback entries and nine special
/// `added_tokens` (ids 4, 5, 6, 7, 9, 11, 13, 16, 98). QJsonObject insertion
/// order is not semantic and neither is JSON object key order — the loader
/// performs the same two-pass maxId → assignment shape, so any order works.
fn write_synthetic_tokenizer_json(dir: &Path) -> PathBuf {
    let root = serde_json::json!({
        "model": {
            "vocab": {
                "<pad>": 0,
                "<eos>": 1,
                "<unk>": 2,
                "▁": 13764,              // ▁ (bos_token_id)
                "▁日本": 100,             // ▁日本
                "語": 101,                // 語
                "<0xE3>": 200,
                "<0x81>": 201,
                "<0x82>": 202,           // UTF-8 for 'あ'
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

/// C++ scenario 1: `decode({4,100,101,200,201,202,1}, true) == "日本語あ"`
/// and `buildCohereAsrPrompt("ja", true) == {13764,7,4,16,98,98,5,9,11,13}`.
#[test]
fn test_onnx_tokenizer_decode_and_cohere_prompt() {
    let dir = unique_temp_dir("fixture");
    write_synthetic_tokenizer_json(&dir);

    let mut tokenizer = OnnxTokenizer::new();
    if let Err(err) = tokenizer.load(dir.join("tokenizer.json")) {
        panic!("OnnxTokenizer::load failed: {err}");
    }

    // The BPE object vocab contains ids up to 13764 → id_to_token is sized
    // to 13765; the added tokens all land inside that span.
    assert!(tokenizer.is_loaded(), "tokenizer should report loaded state");
    assert_eq!(tokenizer.vocab_size(), 13765);

    let decoded = tokenizer.decode(&[4, 100, 101, 200, 201, 202, 1], true);
    assert_eq!(
        decoded, "日本語あ",
        "OnnxTokenizer::decode UTF-8 / metaspace mismatch"
    );

    let prompt = tokenizer.build_cohere_asr_prompt_with("ja", true);
    let expected_prompt: Vec<i64> = vec![13764, 7, 4, 16, 98, 98, 5, 9, 11, 13];
    assert_eq!(prompt, expected_prompt, "Cohere 10-token decoder prompt mismatch");

    // C++ default arguments `buildCohereAsrPrompt()` ("ja", true).
    assert_eq!(tokenizer.build_cohere_asr_prompt(), expected_prompt);

    // --- supplemental locks on the same C++ semantics ---------------------

    // Fairly early added_tokens overrides and token lookup in both maps.
    assert_eq!(tokenizer.token_to_id("▁", -1), 13764);
    assert_eq!(tokenizer.token_to_id("▁日本", -1), 100);
    assert_eq!(tokenizer.token_to_id("missing-token", -1), -1);
    assert_eq!(tokenizer.id_to_token(98), "<|ja|>"); // added token wins id 98
    assert_eq!(tokenizer.id_to_token(-1), "");
    assert_eq!(tokenizer.id_to_token(13765), "");
    assert!(tokenizer.is_special_token(98));
    assert!(tokenizer.is_special_token(4));
    assert!(!tokenizer.is_special_token(100));

    // Special ids / <|"...|> tokens are only skipped when requested; one
    // leading metaspace space is stripped.
    assert_eq!(tokenizer.decode(&[1], true), "");
    assert_eq!(tokenizer.decode(&[4], false), "<|startoftranscript|>");
    assert_eq!(tokenizer.decode(&[1], false), "<eos>");
    assert_eq!(tokenizer.decode(&[100], true), "日本"); // " 日本" minus one space
    // Size-1 byte payload: lone 0xE3 is invalid UTF-8 → one U+FFFD, matching
    // QString::fromUtf8 (Qt decoder) behavior.
    assert_eq!(tokenizer.decode(&[200], true), "\u{FFFD}");
}

/// Unigram / SentencePiece array vocab: `[["token", score], ...]` plus bare
/// string entries; non-array/non-string entries become the empty token but
/// are still assigned to their index.
#[test]
fn test_onnx_tokenizer_unigram_array_vocab() {
    let dir = unique_temp_dir("unigram");
    let path = dir.join("tokenizer.json");
    fs::write(
        &path,
        r#"{"model":{"vocab":[["x", 0.0], "y", [5, "extra"], 42, null]}}"#,
    )
    .unwrap();

    let mut tokenizer = OnnxTokenizer::new();
    tokenizer.load(&path).expect("load failed");
    assert!(tokenizer.is_loaded());
    assert_eq!(tokenizer.vocab_size(), 5);
    assert_eq!(tokenizer.id_to_token(0), "x"); // array entry: element 0
    assert_eq!(tokenizer.id_to_token(1), "y"); // bare string entry
    assert_eq!(tokenizer.id_to_token(2), "");  // first element not a string
    assert_eq!(tokenizer.id_to_token(3), "");  // not an array or string
    assert_eq!(tokenizer.id_to_token(4), "");  // null
    // Duplicate empty tokens: the last index wins in the token → id map
    // (QHash::insert overwrites).
    assert_eq!(tokenizer.token_to_id("x", -1), 0);
    assert_eq!(tokenizer.token_to_id("y", -1), 1);
    assert_eq!(tokenizer.token_to_id("", -1), 4);
    assert_eq!(tokenizer.decode(&[0], true), "x");
    assert_eq!(tokenizer.decode(&[1], true), "y");
    assert_eq!(tokenizer.decode(&[2, 3, 4], true), ""); // empty tokens skipped
}

/// added_tokens are processed AFTER the vocab: they extend `id_to_token`
/// with an empty fill, override existing entries, keep the old token as a
/// token_to_id alias, and only join the special set when "special" is true.
#[test]
fn test_onnx_tokenizer_added_tokens_extend_override_special() {
    let dir = unique_temp_dir("added");
    let path = dir.join("tokenizer.json");
    fs::write(
        &path,
        r#"{
        "model": { "vocab": { "a": 0 } },
        "added_tokens": [
            {"id": -1, "content": "ignored"},
            {"content": "no id key"},
            {"id": "x", "content": "bad id type"},
            {"id": 5, "content": "<|z|>", "special": true},
            {"id": 0, "content": "b"},
            {"id": 29, "content": "c"}
        ]
    }"#,
    )
    .unwrap();

    let mut tokenizer = OnnxTokenizer::new();
    tokenizer.load(&path).expect("load failed");

    // id 5 → resize to 6 (empty fill), id 0 → "b" overrides "a", id 29 →
    // resize to 30 (empty fill between 6 and 28).
    assert_eq!(tokenizer.id_to_token(0), "b");
    assert_eq!(tokenizer.token_to_id("a", -1), 0); // old alias kept
    assert_eq!(tokenizer.token_to_id("b", -1), 0);
    assert_eq!(tokenizer.id_to_token(5), "<|z|>");
    assert!(tokenizer.is_special_token(5));
    assert!(!tokenizer.is_special_token(0));
    assert!(!tokenizer.is_special_token(29)); // "special" absent → not special
    assert_eq!(tokenizer.id_to_token(29), "c");
    assert_eq!(tokenizer.id_to_token(28), "");
    assert_eq!(tokenizer.vocab_size(), 30);
    assert_eq!(tokenizer.decode(&[28], true), ""); // empty fill tokens skipped
    // id 5 is special → skipped with skip_special_tokens regardless of the
    // <|"...|> text check; kept when not skipping.
    assert_eq!(tokenizer.decode(&[5], true), "");
    assert_eq!(tokenizer.decode(&[5], false), "<|z|>");
}

/// added_tokens alone (no model.vocab) still satisfy the non-empty vocab
/// requirement, mirroring the C++ ordering of the emptiness check.
#[test]
fn test_onnx_tokenizer_added_tokens_alone_are_enough() {
    let dir = unique_temp_dir("added-only");
    let path = dir.join("tokenizer.json");
    fs::write(
        &path,
        r#"{"added_tokens": [{"id": 3, "content": "only", "special": false}]}"#,
    )
    .unwrap();

    let mut tokenizer = OnnxTokenizer::new();
    tokenizer.load(&path).expect("load failed");
    assert_eq!(tokenizer.id_to_token(3), "only");
    assert_eq!(tokenizer.vocab_size(), 4);
    assert_eq!(tokenizer.decode(&[0], true), ""); // empty fill skipped
    assert_eq!(tokenizer.decode(&[3], true), "only");
}

/// The explicit skip list and the `<|"...|>` pattern also apply to tokens
/// that were never registered via added_tokens.
#[test]
fn test_onnx_tokenizer_decode_skip_rules() {
    let dir = unique_temp_dir("skip-rules");
    let path = dir.join("tokenizer.json");
    fs::write(
        &path,
        r#"{"model":{"vocab":{"<pad>":0,"<|x|>":1,"<s>":2,"word":3}}}"#,
    )
    .unwrap();
    let mut tokenizer = OnnxTokenizer::new();
    tokenizer.load(&path).expect("load failed");
    assert!(!tokenizer.is_special_token(0)); // not registered, yet still skipped in decode
    assert_eq!(tokenizer.decode(&[0, 1, 2, 3], true), "word");
    assert_eq!(tokenizer.decode(&[0, 1, 2, 3], false), "<pad><|x|><s>word");

    // Metaspace replacement applies per token; only ONE leading space is
    // removed from the final string (C++ decoded.remove(0, 1)).
    fs::write(&path, r#"{"model":{"vocab":{"▁":0,"▁a":1}}}"#).unwrap();
    tokenizer.load(&path).expect("load failed");
    assert_eq!(tokenizer.decode(&[0, 1], true), " a");
    assert_eq!(tokenizer.decode(&[0, 0, 0], true), "  ");
}

/// <0xXX> byte fallback boundary cases (6-char rule, hex pairs, out-of-range
/// or non-hex middles fall through to plain tokens).
#[test]
fn test_onnx_tokenizer_byte_fallback_boundaries() {
    let dir = unique_temp_dir("byte-fallback");
    let path = dir.join("tokenizer.json");
    fs::write(
        &path,
        r#"{"model":{"vocab":{"<0xff>":0,"<0x100>":1,"<0xG1>":2,"<0A1>":3,"word":4}}}"#,
    )
    .unwrap();
    let mut tokenizer = OnnxTokenizer::new();
    tokenizer.load(&path).expect("load failed");
    assert_eq!(tokenizer.decode(&[0, 4], true), "\u{FFFD}word"); // 0xFF → U+FFFD
    assert_eq!(tokenizer.decode(&[1], true), "<0x100>"); // 7 chars: not a byte token
    assert_eq!(tokenizer.decode(&[2], true), "<0xG1>"); // non-hex middle: plain token
    assert_eq!(tokenizer.decode(&[3], true), "<0A1>"); // missing the "0x" prefix
}

/// The exact C++ error strings; only invalid-JSON text may differ from Qt
/// (documented above), so the prefix alone is asserted there.
#[test]
fn test_onnx_tokenizer_load_error_messages() {
    let dir = unique_temp_dir("errors");
    let mut tokenizer = OnnxTokenizer::new();

    // "Could not open tokenizer.json: %1" (with the passed path).
    let missing = dir.join("missing.json");
    let err = tokenizer.load(&missing).expect_err("should fail to open");
    assert_eq!(
        err,
        format!("Could not open tokenizer.json: {}", missing.display())
    );

    // "No vocabulary entries found in tokenizer.json." — an EMPTY BPE object
    // (added tokens absent) leaves id_to_token empty.
    let path = dir.join("tokenizer.json");
    fs::write(&path, r#"{"model":{"vocab":{}}}"#).unwrap();
    let err = tokenizer.load(&path).expect_err("empty vocab must fail");
    assert_eq!(err, "No vocabulary entries found in tokenizer.json.");

    // Invalid JSON: same C++ message prefix, serde_json detail text.
    fs::write(&path, "not json {").unwrap();
    let err = tokenizer.load(&path).expect_err("invalid json must fail");
    assert!(
        err.starts_with("Invalid JSON in tokenizer.json: "),
        "unexpected error text: {err}"
    );

    // A document that parses but is not an object runs through the same
    // C++ branch (parseError == NoError, !doc.isObject()).
    fs::write(&path, "[1, 2, 3]").unwrap();
    let err = tokenizer.load(&path).expect_err("array root must fail");
    assert!(
        err.starts_with("Invalid JSON in tokenizer.json: "),
        "unexpected error text: {err}"
    );

    // load() clears all state up front: after a failed reload the tokenizer
    // is empty (mirrors the C++ members being cleared first).
    let fixture_dir = unique_temp_dir("errors-clear");
    let valid = write_synthetic_tokenizer_json(&fixture_dir);
    tokenizer.load(&valid).expect("fixture load must succeed");
    assert!(tokenizer.is_loaded());
    fs::write(&valid, "{").unwrap(); // now invalid
    assert!(tokenizer.load(&valid).is_err());
    assert!(!tokenizer.is_loaded());
    assert_eq!(tokenizer.vocab_size(), 0);
    assert_eq!(tokenizer.id_to_token(4), "");
    assert!(!tokenizer.is_special_token(4));
    assert_eq!(tokenizer.decode(&[4], false), "");
}

/// `QJsonValue::toInteger(-1)` leaves invalid IDs unassigned rather than
/// truncating e.g. 1.5 to 1 and changing the emitted transcript.
#[test]
fn test_onnx_tokenizer_rejects_fractional_ids() {
    let dir = unique_temp_dir("fractional-ids");
    let path = dir.join("tokenizer.json");
    fs::write(
        &path,
        r#"{"model":{"vocab":{"good":1,"fractional":1.5}},
             "added_tokens":[{"id":3.5,"content":"wrong","special":true},
                             {"id":2,"content":"right","special":false}]}"#,
    )
    .unwrap();

    let mut tokenizer = OnnxTokenizer::new();
    tokenizer.load(&path).expect("integral vocabulary must load");
    assert_eq!(tokenizer.vocab_size(), 3);
    assert_eq!(tokenizer.token_to_id("fractional", -1), -1);
    assert_eq!(tokenizer.token_to_id("wrong", -1), -1);
    assert_eq!(tokenizer.decode(&[1, 2], true), "goodright");
}

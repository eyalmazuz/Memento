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

//! Port of `src/asr/onnx/onnxtokenizer.{h,cpp}` — C++ `OnnxTokenizer`.
//!
//! Lightweight tokenizer/detokenizer backed by a Hugging Face `tokenizer.json`
//! file. Supports BPE (`model.vocab` object) and Unigram / SentencePiece
//! (`model.vocab` array) vocabularies, Metaspace (`▁`) and `<0xXX>` byte
//! fallback.
//!
//! Conversion notes (C++ → Rust):
//! - `bool load(const QString &path, QString &error)` maps to
//!   `Result<(), String>`; the error messages are byte-for-byte identical to
//!   the C++ ones.
//! - `QHash`/`QSet` map to `HashMap`/`HashSet`. Iteration order of a JSON
//!   object can never affect the result: the BPE branch performs the same
//!   two-pass `maxId` → assignment as the C++ code, and the Unigram branch is
//!   positional.
//! - Qt keeps 64-bit integral JSON numbers exactly; fractional or out-of-range
//!   numeric token IDs use `QJsonValue::toInteger`'s supplied fallback rather
//!   than truncating/saturating through an f64.
//! - `<0xXX>` byte-fallback detection counts characters like `QString::size()`
//!   and requires both remaining characters to be hex digits. (Qt's
//!   `QString::toUInt` is slightly more lenient: it skips leading ASCII
//!   whitespace and accepts a leading sign before the digits. Real Hugging
//!   Face byte-fallback tokens are always `<0xXX>` with `XX` two hex digits,
//!   so the strict check is behavior-identical for every reachable input.)
//! - `QString::fromUtf8(bytes)` maps to `String::from_utf8_lossy`: both
//!   replace invalid UTF-8 sequences with U+FFFD. Only the placement of
//!   replacement characters can differ, and only for byte sequences where the
//!   Qt decoder and the WHATWG decoder disagree; the decoded text of valid
//!   UTF-8 payloads is identical.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;

use crate::onnx::json::{strip_utf8_bom, to_integer, to_string_qt};

/// C++: `QString::fromUtf8("\xE2\x96\x81")` — U+2581 LOWER ONE EIGHTH BLOCK
/// used by SentencePiece / Metaspace.
const METASPACE: &str = "\u{2581}";

/// Qt's `QJsonParseError::errorString()` for `NoError`, used when the document
/// parses but is not an object.
const PARSE_NO_ERROR: &str = "no error occurred";

/// Upper bound on accepted vocabulary token IDs to prevent unbounded
/// allocations from malformed `tokenizer.json` files.
const MAX_TOKEN_ID: usize = 1_000_000;

/// C++: `OnnxTokenizer` (private members mirror the C++ class layout).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct OnnxTokenizer {
    id_to_token: Vec<String>,
    token_to_id: HashMap<String, i64>,
    special_token_ids: HashSet<i64>,
}

#[inline]
fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// C++: `parseByteFallbackToken` in the anonymous namespace.
///
/// Matches `"<0xXX>"` (6 ASCII bytes with two hex digits) without allocating.
#[inline]
fn parse_byte_fallback_token(token: &str) -> Option<u8> {
    let bytes = token.as_bytes();
    if bytes.len() != 6
        || bytes[0] != b'<'
        || bytes[1] != b'0'
        || bytes[2] != b'x'
        || bytes[5] != b'>'
    {
        return None;
    }
    let high = hex_nibble(bytes[3])?;
    let low = hex_nibble(bytes[4])?;
    Some((high << 4) | low)
}

fn validate_token_id(id: i64) -> Result<usize, String> {
    usize::try_from(id)
        .ok()
        .filter(|&index| index <= MAX_TOKEN_ID)
        .ok_or_else(|| {
            format!("Token ID {id} in tokenizer.json exceeds maximum ({MAX_TOKEN_ID}).")
        })
}

impl OnnxTokenizer {
    /// C++: `OnnxTokenizer() = default`.
    pub fn new() -> Self {
        Self::default()
    }

    /// C++: `isLoaded()`.
    pub fn is_loaded(&self) -> bool {
        !self.id_to_token.is_empty()
    }

    /// C++: `vocabSize()`.
    pub fn vocab_size(&self) -> usize {
        self.id_to_token.len()
    }

    /// C++: `OnnxTokenizer::load(tokenizerJsonPath, error)` — `Err(message)`
    /// corresponds to the C++ `false` + populated `error` string.
    ///
    /// Loads vocabulary and special tokens from a Hugging Face
    /// `tokenizer.json` file.
    pub fn load<P: AsRef<Path>>(&mut self, tokenizer_json_path: P) -> Result<(), String> {
        self.id_to_token.clear();
        self.token_to_id.clear();
        self.special_token_ids.clear();

        let path = tokenizer_json_path.as_ref();
        let contents = fs::read(path).map_err(|_| {
            format!("Could not open tokenizer.json: {}", path.display())
        })?;

        // QJsonDocument::fromJson skips a leading UTF-8 byte-order mark without copying.
        let payload = strip_utf8_bom(&contents);
        let root: serde_json::Value = match serde_json::from_slice(payload) {
            Ok(root) => root,
            // Divergence from C++, documented: the C++ embeds
            // `QJsonParseError::errorString()` here (e.g. "invalid buffer;
            // line 2, column 1"). serde_json's Display text is used instead.
            Err(err) => {
                return Err(format!("Invalid JSON in tokenizer.json: {err}"));
            }
        };
        if !root.is_object() {
            // Mirrors the C++ `!doc.isObject()` half of the same branch.
            return Err(format!(
                "Invalid JSON in tokenizer.json: {PARSE_NO_ERROR}"
            ));
        }

        let mut id_to_token: Vec<String> = Vec::new();
        let mut token_to_id: HashMap<String, i64> = HashMap::new();
        let mut special_token_ids: HashSet<i64> = HashSet::new();

        let vocab_value = root
            .get("model")
            .and_then(serde_json::Value::as_object)
            .and_then(|model| model.get("vocab"));

        if let Some(serde_json::Value::Object(vocab_obj)) = vocab_value {
            // BPE object format. Two passes exactly like the C++ code: the
            // first determines maxId, the second assigns; with this shape the
            // JSON object iteration order cannot affect the result.
            let mut max_id: Option<usize> = None;
            for value in vocab_obj.values() {
                let id = to_integer(Some(value), -1);
                if id >= 0 {
                    let index = validate_token_id(id)?;
                    max_id = Some(max_id.map_or(index, |cur| cur.max(index)));
                }
            }
            if let Some(max_index) = max_id {
                let new_len = max_index
                    .checked_add(1)
                    .ok_or_else(|| "Vocabulary size overflow in tokenizer.json.".to_string())?;
                id_to_token.resize(new_len, String::new());
                token_to_id.reserve(vocab_obj.len().min(new_len));
                for (key, value) in vocab_obj {
                    let id = to_integer(Some(value), -1);
                    if let Ok(index) = usize::try_from(id) {
                        if index < id_to_token.len() {
                            id_to_token[index] = key.clone();
                            token_to_id.insert(key.clone(), id);
                        }
                    }
                }
            }
        } else if let Some(serde_json::Value::Array(vocab_arr)) = vocab_value {
            // Unigram / SentencePiece array format: [ ["token", score], ... ]
            let max_vocab_len = MAX_TOKEN_ID
                .checked_add(1)
                .ok_or_else(|| "Vocabulary size overflow in tokenizer.json.".to_string())?;
            if vocab_arr.len() > max_vocab_len {
                return Err(format!(
                    "Vocabulary size {} in tokenizer.json exceeds maximum ({max_vocab_len}).",
                    vocab_arr.len()
                ));
            }
            id_to_token.resize(vocab_arr.len(), String::new());
            token_to_id.reserve(vocab_arr.len());
            for (i, entry) in vocab_arr.iter().enumerate() {
                let token = match entry {
                    // Non-empty array: token is entry[0].toString()
                    // ("[]" when element 0 is not a string).
                    serde_json::Value::Array(elements) => elements
                        .first()
                        .map(|first| to_string_qt(Some(first)))
                        .unwrap_or_default(),
                    serde_json::Value::String(_) => to_string_qt(Some(entry)),
                    // Anything else (number, bool, null, empty array) yields
                    // the empty token — still assigned to the entry index and
                    // inserted into the id map, exactly like the C++ code.
                    _ => String::new(),
                };
                id_to_token[i] = token.clone();
                token_to_id.insert(token, i as i64);
            }
        }

        // Parse added_tokens. These are processed AFTER the vocab so they can
        // extend id_to_token (with empty fill), override entries, and add to
        // the special-token set.
        let added_tokens = root
            .get("added_tokens")
            .and_then(serde_json::Value::as_array);
        for val in added_tokens.into_iter().flatten() {
            let Some(obj) = val.as_object() else {
                continue;
            };
            let id = to_integer(obj.get("id"), -1);
            let content = to_string_qt(obj.get("content"));
            let special = obj
                .get("special")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);
            if id < 0 {
                continue;
            }
            let index = validate_token_id(id)?;
            let required_len = index
                .checked_add(1)
                .ok_or_else(|| "Vocabulary size overflow in tokenizer.json.".to_string())?;
            if required_len > id_to_token.len() {
                id_to_token.resize(required_len, String::new());
            }
            id_to_token[index] = content.clone();
            token_to_id.insert(content, id);
            if special {
                special_token_ids.insert(id);
            }
        }

        if id_to_token.is_empty() {
            return Err("No vocabulary entries found in tokenizer.json.".to_string());
        }

        self.id_to_token = id_to_token;
        self.token_to_id = token_to_id;
        self.special_token_ids = special_token_ids;
        Ok(())
    }

    /// C++: `tokenToId(token, fallback = -1)`.
    pub fn token_to_id(&self, token: &str, fallback: i64) -> i64 {
        self.token_to_id.get(token).copied().unwrap_or(fallback)
    }

    /// Borrowed counterpart to [`Self::id_to_token`], returning `None` for
    /// out-of-range ids without allocating a `String`.
    pub fn id_to_token_str(&self, id: i64) -> Option<&str> {
        usize::try_from(id)
            .ok()
            .and_then(|index| self.id_to_token.get(index))
            .map(String::as_str)
    }

    /// C++: `idToToken(id)` — empty string for out-of-range ids.
    pub fn id_to_token(&self, id: i64) -> String {
        self.id_to_token_str(id).unwrap_or("").to_owned()
    }

    /// C++: `isSpecialToken(id)`.
    pub fn is_special_token(&self, id: i64) -> bool {
        self.special_token_ids.contains(&id)
    }

    /// C++: `decode(ids, skipSpecialTokens)` — detokenize a sequence of token
    /// ids into a UTF-8 string.
    pub fn decode(&self, ids: &[i64], skip_special_tokens: bool) -> String {
        // C++ reserves ids.size() * 4 bytes up front.
        let mut utf8_bytes: Vec<u8> = Vec::with_capacity(ids.len().saturating_mul(4));

        for &id in ids {
            let Some(token) = self.id_to_token_str(id) else {
                continue;
            };
            if skip_special_tokens && self.special_token_ids.contains(&id) {
                continue;
            }
            if token.is_empty() {
                continue;
            }
            if skip_special_tokens
                && ((token.starts_with("<|") && token.ends_with("|>"))
                    || token == "<pad>"
                    || token == "<eos>"
                    || token == "<unk>"
                    || token == "<bos>"
                    || token == "<nospeech>"
                    || token == "<s>"
                    || token == "</s>")
            {
                continue;
            }

            if let Some(raw_byte) = parse_byte_fallback_token(token) {
                utf8_bytes.push(raw_byte);
                continue;
            }

            // Stream U+2581 (METASPACE) -> ASCII space directly without
            // allocating an intermediate `String` per token.
            let mut first = true;
            for part in token.split(METASPACE) {
                if !first {
                    utf8_bytes.push(b' ');
                }
                first = false;
                utf8_bytes.extend_from_slice(part.as_bytes());
            }
        }

        // C++: `if (decoded.startsWith(' ')) decoded.remove(0, 1);` — exactly
        // ONE leading space is removed. Stripping ASCII 0x20 before UTF-8
        // decoding is equivalent and avoids a second String allocation.
        let slice = utf8_bytes.strip_prefix(b" ").unwrap_or(&utf8_bytes);
        // QString::fromUtf8 equivalent. Qt's decoder replaces invalid
        // sequences with U+FFFD, as does from_utf8_lossy.
        String::from_utf8_lossy(slice).into_owned()
    }

    /// C++: `buildCohereAsrPrompt(language = "ja", punctuation = true)`.
    ///
    /// Builds the 10-token decoder prompt for CohereAsr models
    /// ([▁, <|startofcontext|>, <|startoftranscript|>, <|emo:undefined|>,
    ///   <|lang|>, <|lang|>, <|pnc/nopnc|>, <|noitn|>, <|notimestamp|>,
    ///   <|nodiarize|>]).
    pub fn build_cohere_asr_prompt(&self) -> Vec<i64> {
        self.build_cohere_asr_prompt_with("ja", true)
    }

    /// C++: `buildCohereAsrPrompt(language, punctuation)`.
    pub fn build_cohere_asr_prompt_with(&self, language: &str, punctuation: bool) -> Vec<i64> {
        let lang_tag = format!(
            "<|{}|>",
            if language.is_empty() { "ja" } else { language }
        );
        let pnc_tag = if punctuation { "<|pnc|>" } else { "<|nopnc|>" };

        let space_id = self.token_to_id(METASPACE, 13764);
        let start_ctx_id = self.token_to_id("<|startofcontext|>", 7);
        let start_tr_id = self.token_to_id("<|startoftranscript|>", 4);
        let emo_undef_id = self.token_to_id("<|emo:undefined|>", 16);
        let lang_id = self.token_to_id(lang_tag.as_str(), 98);
        let pnc_id = self.token_to_id(pnc_tag, if punctuation { 5 } else { 6 });
        let no_itn_id = self.token_to_id("<|noitn|>", 9);
        let no_ts_id = self.token_to_id("<|notimestamp|>", 11);
        let no_diarize_id = self.token_to_id("<|nodiarize|>", 13);

        vec![
            space_id,
            start_ctx_id,
            start_tr_id,
            emo_undef_id,
            lang_id,
            lang_id,
            pnc_id,
            no_itn_id,
            no_ts_id,
            no_diarize_id,
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_temp_file(label: &str) -> std::path::PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        std::env::temp_dir().join(format!(
            "memento_asr_tok_{label}_{}_{nanos}.json",
            std::process::id()
        ))
    }

    #[test]
    fn parse_byte_fallback_handles_valid_and_invalid_tokens() {
        assert_eq!(parse_byte_fallback_token("<0x00>"), Some(0x00));
        assert_eq!(parse_byte_fallback_token("<0xE3>"), Some(0xE3));
        assert_eq!(parse_byte_fallback_token("<0xff>"), Some(0xFF));
        assert_eq!(parse_byte_fallback_token("<0x100>"), None);
        assert_eq!(parse_byte_fallback_token("<0xG1>"), None);
        assert_eq!(parse_byte_fallback_token("<0A1>"), None);
        assert_eq!(parse_byte_fallback_token("<0xあ>"), None);
    }

    #[test]
    fn load_rejects_oversized_token_ids_and_supports_bom_and_borrowed_lookup() {
        let path = unique_temp_file("bom_and_cap");
        let mut with_bom = vec![0xEF, 0xBB, 0xBF];
        with_bom.extend_from_slice(r#"{"model":{"vocab":{"▁hello":0,"▁world":1}}}"#.as_bytes());
        fs::write(&path, &with_bom).unwrap();

        let mut tok = OnnxTokenizer::new();
        tok.load(&path).expect("BOM-prefixed tokenizer must load");
        assert_eq!(tok.id_to_token_str(0), Some("▁hello"));
        assert_eq!(tok.id_to_token_str(1), Some("▁world"));
        assert_eq!(tok.id_to_token_str(-1), None);
        assert_eq!(tok.id_to_token_str(2), None);
        assert_eq!(tok.decode(&[0, 1], true), "hello world");

        // BPE vocab token ID above MAX_TOKEN_ID must be rejected without OOM.
        fs::write(
            &path,
            format!(r#"{{"model":{{"vocab":{{"huge":{}}}}}}}"#, MAX_TOKEN_ID + 1),
        )
        .unwrap();
        assert!(tok.load(&path).is_err());
        assert!(!tok.is_loaded());

        // added_tokens ID above MAX_TOKEN_ID must also be rejected and leave tokenizer empty.
        fs::write(
            &path,
            format!(
                r#"{{"model":{{"vocab":{{"ok":0}}}},"added_tokens":[{{"id":{},"content":"bad","special":true}}]}}"#,
                MAX_TOKEN_ID + 1
            ),
        )
        .unwrap();
        assert!(tok.load(&path).is_err());
        assert!(!tok.is_loaded());

        let _ = fs::remove_file(&path);
    }
}

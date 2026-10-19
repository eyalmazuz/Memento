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

/// C++: `QString::fromUtf8("\xE2\x96\x81")` — U+2581 LOWER ONE EIGHTH BLOCK
/// used by SentencePiece / Metaspace.
const METASPACE: &str = "\u{2581}";

/// Qt's `QJsonParseError::errorString()` for `NoError`, used when the document
/// parses but is not an object.
const PARSE_NO_ERROR: &str = "no error occurred";

/// C++: `OnnxTokenizer` (private members mirror the C++ class layout).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct OnnxTokenizer {
    id_to_token: Vec<String>,
    token_to_id: HashMap<String, i64>,
    special_token_ids: HashSet<i64>,
}

/// C++: `QJsonValue::toInteger(defaultValue)` — accept only whole,
/// representable integers and retain the precision of 64-bit JSON numbers.
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

/// C++: `QJsonValue::toString()` — empty string when the value is not a
/// string.
fn to_string_qt(value: Option<&serde_json::Value>) -> String {
    value
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
        .to_owned()
}

/// C++: `parseByteFallbackToken` in the anonymous namespace.
///
/// Matches `"<0xXX>"` (length 6, character-based like `QString::size()`).
fn parse_byte_fallback_token(token: &str, out_byte: &mut u8) -> bool {
    let chars: Vec<char> = token.chars().collect();
    if chars.len() != 6 || !token.starts_with("<0x") || !token.ends_with('>') {
        return false;
    }
    // C++: token.mid(3, 2).toUInt(&ok, 16) with the digits at positions 3..4.
    let Some(high) = chars[3].to_digit(16) else {
        return false;
    };
    let Some(low) = chars[4].to_digit(16) else {
        return false;
    };
    let val = high * 16 + low;
    // Guard kept verbatim from the C++ code.
    if val > 0xFF {
        return false;
    }
    *out_byte = val as u8;
    true
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

        // QJsonDocument::fromJson skips a leading UTF-8 byte-order mark.
        let contents = if let Some(rest) = contents.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
            rest.to_vec()
        } else {
            contents
        };
        let root: serde_json::Value = match serde_json::from_slice(&contents) {
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

        let vocab_value = root
            .get("model")
            .and_then(serde_json::Value::as_object)
            .and_then(|model| model.get("vocab"));

        if let Some(serde_json::Value::Object(vocab_obj)) = vocab_value {
            // BPE object format. Two passes exactly like the C++ code: the
            // first determines maxId, the second assigns; with this shape the
            // JSON object iteration order cannot affect the result.
            let mut max_id: i64 = -1;
            for value in vocab_obj.values() {
                let id = to_integer(Some(value), -1);
                if id >= 0 {
                    max_id = max_id.max(id);
                }
            }
            if max_id >= 0 {
                self.id_to_token
                    .resize(max_id as usize + 1, String::new());
                self.token_to_id.reserve(max_id as usize + 1);
                for (key, value) in vocab_obj {
                    let id = to_integer(Some(value), -1);
                    if id >= 0 && (id as usize) < self.id_to_token.len() {
                        self.id_to_token[id as usize] = key.clone();
                        self.token_to_id.insert(key.clone(), id);
                    }
                }
            }
        } else if let Some(serde_json::Value::Array(vocab_arr)) = vocab_value {
            // Unigram / SentencePiece array format: [ ["token", score], ... ]
            self.id_to_token.resize(vocab_arr.len(), String::new());
            self.token_to_id.reserve(vocab_arr.len());
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
                self.id_to_token[i] = token.clone();
                self.token_to_id.insert(token, i as i64);
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
            let index = id as usize;
            if index >= self.id_to_token.len() {
                self.id_to_token.resize(index + 1, String::new());
            }
            self.id_to_token[index] = content.clone();
            self.token_to_id.insert(content, id);
            if special {
                self.special_token_ids.insert(id);
            }
        }

        if self.id_to_token.is_empty() {
            return Err("No vocabulary entries found in tokenizer.json.".to_string());
        }

        Ok(())
    }

    /// C++: `tokenToId(token, fallback = -1)`.
    pub fn token_to_id(&self, token: &str, fallback: i64) -> i64 {
        self.token_to_id.get(token).copied().unwrap_or(fallback)
    }

    /// C++: `idToToken(id)` — empty string for out-of-range ids.
    pub fn id_to_token(&self, id: i64) -> String {
        match usize::try_from(id).ok().and_then(|index| self.id_to_token.get(index)) {
            Some(token) => token.clone(),
            None => String::new(),
        }
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
            let Some(index) = usize::try_from(id).ok().filter(|&index| index < self.id_to_token.len())
            else {
                continue;
            };
            if skip_special_tokens && self.special_token_ids.contains(&id) {
                continue;
            }

            let token = self.id_to_token[index].as_str();
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

            let mut raw_byte = 0u8;
            if parse_byte_fallback_token(token, &mut raw_byte) {
                utf8_bytes.push(raw_byte);
                continue;
            }

            let replaced = token.replace(METASPACE, " ");
            utf8_bytes.extend_from_slice(replaced.as_bytes());
        }

        // QString::fromUtf8 equivalent. Qt's decoder replaces invalid
        // sequences with U+FFFD, as does from_utf8_lossy.
        let decoded = String::from_utf8_lossy(&utf8_bytes).into_owned();
        // C++: `if (decoded.startsWith(' ')) decoded.remove(0, 1);` — exactly
        // ONE leading space is removed.
        match decoded.strip_prefix(' ') {
            Some(rest) => rest.to_owned(),
            None => decoded,
        }
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

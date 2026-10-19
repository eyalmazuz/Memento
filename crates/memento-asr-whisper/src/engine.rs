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

//! Port of `src/asr/whisper/whispermodel.cpp` (`WhisperModel` internals).
//!
//! Faithfulness rules (verified by
//! `tests/mpv-memory-subtitles/whisper_engine_equal_test.cpp`):
//! - every failure string is byte-identical to the C++ `QStringLiteral`s;
//! - the WAV chunk walk, header hex dump (Qt `toHex(' ')` — lowercase, space
//!   separated), sample conversion (`i16 as f32 / 32768.0f`) and all
//!   validation checks mirror `read_whisper_wav` exactly;
//! - `whisper_full_params` is built from
//!   `whisper_full_default_params(strategy)` with exactly the C++ overrides,
//!   including the raw `beamSize > 1` strategy choice and the `max(1, ..)`
//!   clamps;
//! - the segment callback computes `start = max(0, count - n_new)` and walks
//!   `[start, count)` reading text/t0/t1 from the context, exactly like
//!   `handle_new_segment`;
//! - the abort flag is polled at the same points: inside `whisper_full`
//!   through the abort callback and once after a non-zero return.
//!
//! Cross-boundary differences that are intentional and documented:
//! - `qWarning` side effects happen on the C++ side of the bridge (the shim
//!   logs the failure message returned here; a mid-call warning callback is
//!   used for the invalid-suppress-regex warning).
//! - With the C++ validator supplied, `suppressRegex` uses the *same*
//!   `std::regex` pre-validation as the reference and whisper.cpp, including
//!   lookarounds and backreferences (which Rust's `regex` crate rejects).
//!   A rejected pattern is warned about by that validator and omitted.
//!   Standalone callers without that validator use the Rust `regex` crate
//!   to reject malformed patterns before whisper.cpp receives them; some
//!   ECMAScript-only expressions remain unsupported in that path.
//! - `QString::trimmed()` (Unicode whitespace per `QChar::isSpace()`) is
//!   approximated with Rust's `str::trim()` (Unicode `White_Space`); they
//!   differ only for exotic whitespace-only patterns.

use std::cmp::max;
use std::ffi::CString;
use std::os::raw::{c_char, c_int, c_void};
use std::path::Path;

use crate::whisper_sys as sys;

/// C++: `static constexpr const char *LANGUAGE_JAPANESE = "ja";`
pub const LANGUAGE_JAPANESE: &str = "ja";
/// C++: `static constexpr int WAV_HEADER_SIZE = 12;`
const WAV_HEADER_SIZE: usize = 12;
/// C++: `static constexpr int WAV_CHUNK_HEADER_SIZE = 8;`
const WAV_CHUNK_HEADER_SIZE: usize = 8;
/// C++: `static constexpr int WHISPER_TIME_SCALE = 100;`
pub const WHISPER_TIME_SCALE: i64 = 100;
/// C++: `static constexpr quint16 WAV_FORMAT_PCM = 1;`
const WAV_FORMAT_PCM: u16 = 1;
/// C++: `static constexpr quint16 WAV_BITS_PER_SAMPLE = 16;`
const WAV_BITS_PER_SAMPLE: u16 = 16;
/// C++: `static constexpr quint16 WAV_CHANNELS = 1;`
const WAV_CHANNELS: u16 = 1;
/// C++: `constexpr int BYTES_PER_SAMPLE = WAV_BITS_PER_SAMPLE / 8;`
const BYTES_PER_SAMPLE: usize = WAV_BITS_PER_SAMPLE as usize / 8;

/// C++ failure string from `WhisperModel::transcribe` (null model branch).
pub const ERR_MODEL_INIT: &str = "Whisper could not initialize the selected model.";
/// C++ failure string from `WhisperModel::transcribe` (defensive empty
/// samples branch).
pub const ERR_EMPTY_AUDIO: &str = "The extracted audio contains no samples.";

/// C ABI status codes, mirrored in `src/asr/rust/asr_whisper_capi.h`
/// (`AsrWhisperStatus`).
pub const STATUS_SUCCESS: i32 = 0;
pub const STATUS_CANCELED: i32 = 1;
/// C++ behaviour: `qWarning(message)` then `failureCallback(message)` then
/// `AsrTranscriptionResult::Failed`.
pub const STATUS_FAILED: i32 = 2;
/// C++ behaviour: `failureCallback(message)` only (the defensive
/// empty-samples branch, where no `qWarning` preceded the callback).
pub const STATUS_FAILED_WITHOUT_WARNING: i32 = 3;

/// Result of one transcription attempt, mirroring the C++ control flow.
#[derive(Debug, Clone, PartialEq)]
pub enum EngineStatus {
    /// `whisper_full` returned 0.
    Success,
    /// `whisper_full` failed and the abort flag was set.
    Canceled,
    /// Failure with the byte-identical C++ failure string; `warn` tells the
    /// shim whether the C++ side would have emitted `qWarning(message)`
    /// before the failure callback.
    Failed { message: String, warn: bool },
}

impl EngineStatus {
    /// The C ABI status code for this outcome.
    pub fn code(&self) -> i32 {
        match self {
            EngineStatus::Success => STATUS_SUCCESS,
            EngineStatus::Canceled => STATUS_CANCELED,
            EngineStatus::Failed { warn: true, .. } => STATUS_FAILED,
            EngineStatus::Failed { warn: false, .. } => STATUS_FAILED_WITHOUT_WARNING,
        }
    }

    /// The failure message, when this outcome carries one.
    pub fn message(&self) -> Option<&str> {
        match self {
            EngineStatus::Failed { message, .. } => Some(message),
            _ => None,
        }
    }
}

/// C++: `WhisperTranscriptionOptions` fields consumed by `WhisperModel`.
/// Strings mirror the `QString` fields exactly (UTF-8 bytes).
#[derive(Debug, Clone, Default)]
pub struct TranscribeOptions {
    /// `options.common.language`; empty selects [`LANGUAGE_JAPANESE`].
    pub language: String,
    /// `options.threads`.
    pub threads: i32,
    /// `options.bestOf`.
    pub best_of: i32,
    /// `options.beamSize`.
    pub beam_size: i32,
    /// `options.useVad`.
    pub use_vad: bool,
    /// `options.vadModel`.
    pub vad_model: String,
    /// `options.suppressRegex`.
    pub suppress_regex: String,
}

/// Abort flag bridge: the C++ shim forwards to the same `std::atomic_bool`
/// that `handle_abort` reads (no atomic layout assumptions cross the
/// boundary).
#[derive(Copy, Clone)]
pub struct AbortBridge {
    /// Poll function; mirrors `handle_abort` (null userData => false).
    pub poll: unsafe extern "C" fn(*mut c_void) -> bool,
    /// Opaque argument for `poll`.
    pub user_data: *mut c_void,
}

/// Segment bridge: mirrors `SegmentCallbackContext` for the Rust path.
#[derive(Copy, Clone)]
pub struct SegmentBridge {
    /// Called with `(text_utf8, text_len, t0_centiseconds, t1_centiseconds)`.
    pub callback: Option<unsafe extern "C" fn(*const u8, usize, i64, i64, *mut c_void)>,
    /// Opaque argument for `callback`.
    pub user_data: *mut c_void,
}

/// (used by standalone callers without the C++ regex validator).
#[derive(Copy, Clone)]
pub struct WarningBridge {
    /// Called with a NUL-terminated UTF-8 message.
    pub callback: Option<unsafe extern "C" fn(*const c_char, *mut c_void)>,
    /// Opaque argument for `callback`.
    pub user_data: *mut c_void,
}

/// `std::regex` validator bridge: lets the engine run the reference's exact
/// `std::regex` check (the C++ shim implements it byte-for-byte) instead of
/// guessing with a different regex engine. The callback MUST emit the
/// reference `qWarning` itself when it rejects a pattern and return `false`.
#[derive(Copy, Clone)]
pub struct RegexValidateBridge {
    /// Called with `(pattern_bytes, pattern_len, user_data)`; returns true
    /// when `std::regex` accepts the pattern.
    pub validate: unsafe extern "C" fn(*const c_char, usize, *mut c_void) -> bool,
    /// Opaque argument for `validate`.
    pub user_data: *mut c_void,
}

/// C++: `read_u16`
fn read_u16(data: &[u8]) -> u16 {
    u16::from_le_bytes([data[0], data[1]])
}

/// C++: `read_u32`
fn read_u32(data: &[u8]) -> u32 {
    u32::from_le_bytes([data[0], data[1], data[2], data[3]])
}

/// C++: `read_whisper_wav(path, error)` — reads a 16 kHz mono PCM16 WAV into
/// normalized `[-1, 1]` floats. `Err` carries the byte-identical C++ failure
/// string (`qWarning` side effect lives on the C++ side of the bridge).
pub fn read_whisper_wav(path: &Path) -> Result<Vec<f32>, String> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(_) => return Err("Could not open the extracted audio file.".to_string()),
    };

    if bytes.len() < WAV_HEADER_SIZE {
        return Err(format!(
            "The extracted WAV is truncated ({} bytes).",
            bytes.len()
        ));
    }
    if &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        // Qt: QString::fromLatin1(bytes.first(12).toHex(' ')) — lowercase
        // hex, space separated (same as AsrAudio::readPcm16Wav, verified by
        // asr_rust_bridge_test.cpp).
        let header_hex: Vec<String> = bytes[..12].iter().map(|b| format!("{b:02x}")).collect();
        return Err(format!(
            "The extracted audio is not RIFF/WAVE (header {}).",
            header_hex.join(" ")
        ));
    }

    let mut format: u16 = 0;
    let mut channels: u16 = 0;
    let mut sample_rate: u32 = 0;
    let mut block_align: u16 = 0;
    let mut bits_per_sample: u16 = 0;
    let mut pcm_data: Option<&[u8]> = None;

    let mut offset = WAV_HEADER_SIZE;
    while offset + WAV_CHUNK_HEADER_SIZE <= bytes.len() {
        let chunk = &bytes[offset..];
        let id = &chunk[0..4];
        // C++ compares `offset + size` (qsizetype) against
        // `static_cast<quint32>(bytes.size())`; replicate the truncation.
        let size = read_u32(&chunk[4..8]);
        offset += WAV_CHUNK_HEADER_SIZE;

        if offset + size as usize > (bytes.len() as u32) as usize {
            return Err("The extracted WAV file is truncated.".to_string());
        }

        if id == b"fmt " && size >= 16 {
            let fmt = &bytes[offset..];
            format = read_u16(&fmt[0..2]);
            channels = read_u16(&fmt[2..4]);
            sample_rate = read_u32(&fmt[4..8]);
            block_align = read_u16(&fmt[12..14]);
            bits_per_sample = read_u16(&fmt[14..16]);
        } else if id == b"data" {
            // Last "data" chunk wins, mirroring `pcmData = bytes.mid(...)`.
            pcm_data = Some(&bytes[offset..offset + size as usize]);
        }

        offset += size as usize + (size % 2) as usize;
    }

    const EXPECTED_SAMPLE_RATE: u32 = sys::WHISPER_SAMPLE_RATE;
    if format != WAV_FORMAT_PCM
        || channels != WAV_CHANNELS
        || sample_rate != EXPECTED_SAMPLE_RATE
        || block_align != WAV_CHANNELS * BYTES_PER_SAMPLE as u16
        || bits_per_sample != WAV_BITS_PER_SAMPLE
        || pcm_data.map_or(true, |data| data.is_empty())
    {
        return Err("The extracted WAV is not 16 kHz mono PCM16.".to_string());
    }
    let pcm_data = pcm_data.expect("checked as present above");

    if pcm_data.len() < BYTES_PER_SAMPLE {
        return Err("The extracted WAV contains no audio samples.".to_string());
    }

    let samples = pcm_data.len() / BYTES_PER_SAMPLE;
    let mut output = Vec::with_capacity(samples);
    for i in 0..samples {
        let value = read_u16(&pcm_data[i * BYTES_PER_SAMPLE..]) as i16;
        output.push(value as f32 / 32768.0f32);
    }
    Ok(output)
}

/// C++: `timestamp_to_seconds(timestamp)`.
///
/// Kept for mirror completeness and unit-test coverage; the bridge hands raw
/// centiseconds to C++, which performs the identical `double/100.0`
/// conversion for both implementations (see `whispermodel.cpp`).
pub fn timestamp_to_seconds(timestamp: i64) -> f64 {
    timestamp as f64 / WHISPER_TIME_SCALE as f64
}

/// C++: `handle_abort` — forwards to the C++ poll (null userData => false).
unsafe extern "C" fn abort_trampoline(user_data: *mut c_void) -> bool {
    if user_data.is_null() {
        return false;
    }
    let bridge = &*(user_data as *const AbortBridge);
    (bridge.poll)(bridge.user_data)
}

/// C++: `handle_new_segment` — walks the newly appended segments and forwards
/// text + centisecond timestamps to the C++ callback.
unsafe extern "C" fn segment_trampoline(
    ctx: *mut sys::whisper_context,
    _state: *mut sys::whisper_state,
    n_new: c_int,
    user_data: *mut c_void,
) {
    if user_data.is_null() {
        return;
    }
    let bridge = &*(user_data as *const SegmentBridge);
    let Some(callback) = bridge.callback else {
        // C++: `if (context == nullptr || !context->callback) return;`
        return;
    };

    let count = sys::whisper_full_n_segments(ctx);
    let start = max(0, count - n_new);
    let mut i = start;
    while i < count {
        let text = sys::whisper_full_get_segment_text(ctx, i);
        let (text_ptr, text_len) = if text.is_null() {
            (std::ptr::null(), 0usize)
        } else {
            let bytes = std::ffi::CStr::from_ptr(text).to_bytes();
            (bytes.as_ptr(), bytes.len())
        };
        let t0 = sys::whisper_full_get_segment_t0(ctx, i);
        let t1 = sys::whisper_full_get_segment_t1(ctx, i);
        callback(text_ptr, text_len, t0, t1, bridge.user_data);
        i += 1;
    }
}

/// Emit a `qWarning` through the shim (no-op when absent).
fn emit_warning(warning: Option<&WarningBridge>, message: &str) {
    if let Some(bridge) = warning {
        if let Some(callback) = bridge.callback {
            if let Ok(c_message) = CString::new(message) {
                // SAFETY: `callback` is a live C++ function pointer by the
                // bridge contract; it stays valid for the duration of the
                // call. The message outlives the call.
                unsafe {
                    callback(c_message.as_ptr(), bridge.user_data);
                }
            }
        }
    }
}

/// C++: the C-string view of a `QByteArray` / `QString` value: bytes are
/// truncated at the first embedded NUL exactly like a `const char *` handed
/// to whisper.cpp.
fn c_string_from_bytes(bytes: &[u8]) -> CString {
    let prefix = bytes
        .split(|byte| *byte == 0)
        .next()
        .expect("split always yields at least one piece");
    CString::new(prefix).expect("the prefix contains no NUL bytes")
}

/// Validate the optional Whisper suppression pattern and turn accepted bytes
/// into the NUL-terminated pointer expected by whisper.cpp.
///
/// The C++ bridge's `std::regex` validator is authoritative when present:
/// whisper.cpp uses ECMAScript regex syntax (including lookarounds and
/// backreferences), which Rust's `regex` crate does not support. Standalone
/// callers have no C++ validator, so use `regex` as a conservative malformed
/// pattern check in that case.
fn validated_suppress_regex(
    pattern: &str,
    warning: Option<&WarningBridge>,
    regex_validate: Option<&RegexValidateBridge>,
) -> Option<CString> {
    if pattern.trim().is_empty() {
        return None;
    }

    let accepted = if let Some(validator) = regex_validate {
        // SAFETY: the bridge contract guarantees a live callback and the
        // borrowed pattern bytes remain valid for the duration of this call.
        unsafe {
            (validator.validate)(
                pattern.as_ptr() as *const c_char,
                pattern.len(),
                validator.user_data,
            )
        }
    } else {
        match regex::Regex::new(pattern) {
            Ok(_) => true,
            Err(error) => {
                emit_warning(
                    warning,
                    &format!("Invalid whisper suppress regex '{pattern}': {error}"),
                );
                false
            }
        }
    };

    accepted.then(|| c_string_from_bytes(pattern.as_bytes()))
}

/// One transcription attempt, port of the `WhisperModel::transcribe` future
/// body: WAV read, model null check, `whisper_full_params` construction,
/// `whisper_full`, abort/error classification.
///
/// `model` may be null: the C++ code reads the WAV first and only then checks
/// the model pointer, so the WAV failure wins for unreadable audio even when
/// the model failed to load. The caller owns the model lock.
///
/// # Safety
/// `model` must be null or a live `whisper_context` for the duration of the
/// call (and must not be used concurrently — the caller holds the model
/// lock); the bridges must reference valid C++ objects/function pointers for
/// the duration of the call.
pub unsafe fn transcribe(
    model: *mut sys::whisper_context,
    audio_path: &Path,
    options: &TranscribeOptions,
    abort: Option<&AbortBridge>,
    segment: Option<&SegmentBridge>,
    warning: Option<&WarningBridge>,
    regex_validate: Option<&RegexValidateBridge>,
) -> EngineStatus {
    let samples = match read_whisper_wav(audio_path) {
        Ok(samples) if !samples.is_empty() => samples,
        Ok(_) => {
            // Defensive C++ branch: empty result without an error message.
            return EngineStatus::Failed {
                message: ERR_EMPTY_AUDIO.to_string(),
                warn: false,
            };
        }
        Err(message) => {
            if message.is_empty() {
                return EngineStatus::Failed {
                    message: ERR_EMPTY_AUDIO.to_string(),
                    warn: false,
                };
            }
            return EngineStatus::Failed { message, warn: true };
        }
    };

    if model.is_null() {
        return EngineStatus::Failed {
            message: ERR_MODEL_INIT.to_string(),
            warn: true,
        };
    }

    // C++: `options.beamSize > 1 ? BEAM_SEARCH : GREEDY` on the raw option.
    let strategy = if options.beam_size > 1 {
        sys::whisper_sampling_strategy_WHISPER_SAMPLING_BEAM_SEARCH
    } else {
        sys::whisper_sampling_strategy_WHISPER_SAMPLING_GREEDY
    };
    let mut params = sys::whisper_full_default_params(strategy);

    params.n_threads = max(1, options.threads);

    // C++: empty QString => "ja", otherwise `.toUtf8()`.
    let language = if options.language.is_empty() {
        LANGUAGE_JAPANESE.to_string()
    } else {
        options.language.clone()
    };
    let language_c = c_string_from_bytes(language.as_bytes());
    params.language = language_c.as_ptr();

    params.print_progress = false;
    params.print_realtime = false;
    params.print_special = false;
    params.print_timestamps = false;
    params.translate = false;
    params.greedy.best_of = max(1, options.best_of);
    params.beam_search.beam_size = max(1, options.beam_size);

    // Segment callback: C++ always installs the handler and lets it check the
    // std::function; mirror by always installing the trampoline.
    let segment_bridge = SegmentBridge {
        callback: segment.and_then(|bridge| bridge.callback),
        user_data: segment.map_or(std::ptr::null_mut(), |bridge| bridge.user_data),
    };
    params.new_segment_callback = Some(segment_trampoline);
    params.new_segment_callback_user_data =
        &segment_bridge as *const SegmentBridge as *mut c_void;

    // Abort callback: C++ installs `handle_abort` only when the atomic is
    // present; the user data is the poll bridge carrying the atomic pointer.
    let abort_bridge = abort.map(|bridge| AbortBridge {
        poll: bridge.poll,
        user_data: bridge.user_data,
    });
    match &abort_bridge {
        Some(bridge) => {
            params.abort_callback = Some(abort_trampoline);
            params.abort_callback_user_data = bridge as *const AbortBridge as *mut c_void;
        }
        None => {
            params.abort_callback = None;
            params.abort_callback_user_data = std::ptr::null_mut();
        }
    }

    let vad_model_c = if options.use_vad {
        Some(c_string_from_bytes(options.vad_model.as_bytes()))
    } else {
        None
    };
    if let Some(vad_model) = &vad_model_c {
        params.vad = true;
        params.vad_model_path = vad_model.as_ptr();
    }

    // The C++ bridge validates with the reference's std::regex: do not
    // second-guess an accepted ECMAScript pattern with Rust's `regex` crate.
    // Standalone callers without the bridge retain the Rust safety check.
    let suppress_regex_c =
        validated_suppress_regex(&options.suppress_regex, warning, regex_validate);
    if let Some(suppress_regex) = &suppress_regex_c {
        params.suppress_regex = suppress_regex.as_ptr();
    }

    let n_samples = samples.len() as c_int;
    let ret = sys::whisper_full(model, params, samples.as_ptr(), n_samples);

    if ret == 0 {
        return EngineStatus::Success;
    }
    if let Some(bridge) = &abort_bridge {
        // Mirrors `handle_abort` (a live C++ function pointer + user data).
        if (bridge.poll)(bridge.user_data) {
            return EngineStatus::Canceled;
        }
    }
    EngineStatus::Failed {
        message: format!("whisper_full returned error code {ret}."),
        warn: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamp_to_seconds_matches_cpp_division() {
        assert_eq!(timestamp_to_seconds(0), 0.0);
        assert_eq!(timestamp_to_seconds(150), 1.5);
        assert_eq!(
            timestamp_to_seconds(-7) as f64,
            -7.0 / 100.0,
            "negative timestamps divide like C++"
        );
    }

    #[test]
    fn c_string_truncates_at_embedded_nul() {
        assert_eq!(c_string_from_bytes(b"ja").to_bytes(), b"ja");
        assert_eq!(c_string_from_bytes(b"a\0b").to_bytes(), b"a");
        assert_eq!(c_string_from_bytes(b"\0").to_bytes(), b"");
    }

    struct RegexValidatorProbe {
        calls: usize,
        patterns: Vec<Vec<u8>>,
    }

    unsafe extern "C" fn record_and_accept_regex(
        pattern: *const c_char,
        pattern_len: usize,
        user_data: *mut c_void,
    ) -> bool {
        let probe = &mut *(user_data as *mut RegexValidatorProbe);
        probe.calls += 1;
        probe.patterns.push(
            std::slice::from_raw_parts(pattern as *const u8, pattern_len).to_vec(),
        );
        true
    }

    #[test]
    fn cpp_validator_accepts_ecmascript_lookaround_and_backreferences() {
        // Both forms are supported by C++ std::regex's ECMAScript grammar but
        // rejected by Rust's `regex` crate.
        let patterns = ["(?=foo)foo", r"^(foo)\1$"];
        let mut probe = RegexValidatorProbe {
            calls: 0,
            patterns: Vec::new(),
        };
        let validator = RegexValidateBridge {
            validate: record_and_accept_regex,
            user_data: &mut probe as *mut RegexValidatorProbe as *mut c_void,
        };

        for pattern in patterns {
            assert!(regex::Regex::new(pattern).is_err());
            let accepted = validated_suppress_regex(pattern, None, Some(&validator))
                .expect("the C++ validator accepted this ECMAScript pattern");
            assert_eq!(accepted.to_bytes(), pattern.as_bytes());
        }

        assert_eq!(probe.calls, patterns.len());
        assert_eq!(
            probe.patterns,
            patterns
                .iter()
                .map(|pattern| pattern.as_bytes().to_vec())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn read_whisper_wav_rejects_garbage_header() {
        let dir = std::env::temp_dir().join("memento-whisper-engine-tests");
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("garbage.wav");
        std::fs::write(&path, [0xACu8; 32]).expect("write fixture");
        let error = read_whisper_wav(&path).expect_err("garbage must fail");
        assert_eq!(
            error,
            "The extracted audio is not RIFF/WAVE (header ac ac ac ac ac ac ac ac \
             ac ac ac ac)."
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn read_whisper_wav_rejects_missing_file() {
        let error = read_whisper_wav(Path::new("/nonexistent/memento/audio.wav"))
            .expect_err("missing file must fail");
        assert_eq!(error, "Could not open the extracted audio file.");
    }

    #[test]
    fn status_codes_match_c_header() {
        assert_eq!(EngineStatus::Success.code(), 0);
        assert_eq!(EngineStatus::Canceled.code(), 1);
        assert_eq!(
            EngineStatus::Failed {
                message: String::new(),
                warn: true
            }
            .code(),
            2
        );
        assert_eq!(
            EngineStatus::Failed {
                message: String::new(),
                warn: false
            }
            .code(),
            3
        );
    }
}

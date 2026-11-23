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
/// C++: `static constexpr int WHISPER_TIME_SCALE = 100;`
pub const WHISPER_TIME_SCALE: i64 = 100;

/// C++ failure string from `WhisperModel::transcribe` (null model branch).
pub const ERR_MODEL_INIT: &str = "Whisper could not initialize the selected model.";
/// C++ failure string from `WhisperModel::transcribe` (defensive empty
/// samples branch).
pub const ERR_EMPTY_AUDIO: &str = "The extracted audio contains no samples.";

/// Engine status codes for transcription outcomes.
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
    /// The numeric status code for this outcome.
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
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if user_data.is_null() {
            return false;
        }
        let bridge = &*(user_data as *const AbortBridge);
        (bridge.poll)(bridge.user_data)
    }))
    .unwrap_or(true)
}

/// C++: `handle_new_segment` — walks the newly appended segments and forwards
/// text + centisecond timestamps to the C++ callback.
unsafe extern "C" fn segment_trampoline(
    ctx: *mut sys::whisper_context,
    _state: *mut sys::whisper_state,
    n_new: c_int,
    user_data: *mut c_void,
) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if ctx.is_null() || user_data.is_null() {
            return;
        }
        let bridge = &*(user_data as *const SegmentBridge);
        let Some(callback) = bridge.callback else {
            // C++: `if (context == nullptr || !context->callback) return;`
            return;
        };

        let count = sys::whisper_full_n_segments(ctx);
        let start = max(0, count.saturating_sub(max(0, n_new)));
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
    }));
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

/// Detect Rust `regex` syntax that C++11 `std::regex` (ECMAScript) rejects by
/// throwing `std::regex_error` across the whisper.cpp FFI boundary.
///
/// Rejects Rust-only inline flags / named captures (`(?...` other than `(?:`),
/// Unicode property escapes (`\p{...}`, `\P{...}`), and braced hex/Unicode
/// escapes (`\x{...}`, `\u{...}`).
fn has_unsupported_cpp_regex_syntax(pattern: &str) -> bool {
    let bytes = pattern.as_bytes();
    let mut i = 0;
    let mut in_char_class = false;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => {
                if i + 2 < bytes.len()
                    && matches!(bytes[i + 1], b'p' | b'P' | b'x' | b'u')
                    && bytes[i + 2] == b'{'
                {
                    return true;
                }
                i += 2;
                continue;
            }
            b'[' if !in_char_class => {
                in_char_class = true;
                i += 1;
                if i < bytes.len() && bytes[i] == b'^' {
                    i += 1;
                }
                if i < bytes.len() && bytes[i] == b']' {
                    i += 1;
                }
                continue;
            }
            b']' if in_char_class => {
                in_char_class = false;
            }
            b'(' if !in_char_class => {
                if i + 1 < bytes.len() && bytes[i + 1] == b'?' {
                    // ECMAScript std::regex supports `(?:...)` (and lookaheads
                    // `(?=...)`/`(?!...)`, which Rust's `regex` rejects below).
                    // Reject all other `(?` constructs (e.g. `(?i)`, `(?P<...>)`,
                    // `(?<...>)`) so C++ std::regex does not throw across FFI.
                    if bytes.get(i + 2) != Some(&b':') {
                        return true;
                    }
                }
            }
            _ => {}
        }
        i += 1;
    }
    false
}

/// Resolve the whisper.cpp thread count: explicit positive counts are used as
/// requested, while `<= 0` ("auto") defaults to `available_parallelism().min(4)`
/// (fallback `4`) rather than single-threaded execution.
fn resolve_thread_count(threads: i32) -> i32 {
    if threads > 0 {
        threads
    } else {
        std::thread::available_parallelism()
            .map(|n| n.get().min(4) as i32)
            .unwrap_or(4)
    }
}

/// Validate the optional Whisper suppression pattern and turn accepted bytes
/// into the NUL-terminated pointer expected by whisper.cpp.
///
/// The C++ bridge's `std::regex` validator is authoritative when present:
/// whisper.cpp uses ECMAScript regex syntax (including lookarounds and
/// backreferences), which Rust's `regex` crate does not support. Standalone
/// callers have no C++ validator, so use `regex` together with an ECMAScript
/// compatibility guard so Rust-only syntax cannot throw `std::regex_error`
/// across the FFI boundary.
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
    } else if has_unsupported_cpp_regex_syntax(pattern) {
        emit_warning(
            warning,
            &format!(
                "Invalid whisper suppress regex '{pattern}': syntax is unsupported by C++ std::regex"
            ),
        );
        false
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
    let samples = match subomi_asr::audio::wavreader::read_pcm16_wav_path(
        audio_path,
        sys::WHISPER_SAMPLE_RATE,
    ) {
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
    transcribe_samples(
        model,
        &samples,
        options,
        abort,
        segment,
        warning,
        regex_validate,
    )
}

/// Transcribe already-decoded 16 kHz mono `f32` PCM samples using safe Rust
/// closures without intermediate WAV serialization or C callback structs.
///
/// # Safety
/// `model` must be null or a live `whisper_context` for the duration of the
/// call (and must not be used concurrently — the caller holds the model lock).
pub unsafe fn transcribe_samples_safe(
    model: *mut sys::whisper_context,
    samples: &[f32],
    options: &TranscribeOptions,
    should_abort: &dyn Fn() -> bool,
    on_segment: &mut dyn FnMut(&str, i64, i64),
) -> EngineStatus {
    struct AbortCtx<'a> {
        f: &'a dyn Fn() -> bool,
    }
    struct SegmentCtx<'a> {
        f: &'a mut dyn FnMut(&str, i64, i64),
    }

    unsafe extern "C" fn poll_abort(data: *mut c_void) -> bool {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            if data.is_null() {
                return false;
            }
            let ctx = &*(data as *const AbortCtx<'_>);
            (ctx.f)()
        }))
        .unwrap_or(true)
    }

    unsafe extern "C" fn forward_segment(
        text: *const u8,
        text_len: usize,
        t0: i64,
        t1: i64,
        data: *mut c_void,
    ) {
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            if data.is_null() || (text_len != 0 && text.is_null()) {
                return;
            }
            let ctx = &mut *(data as *mut SegmentCtx<'_>);
            let bytes = if text_len == 0 {
                &[]
            } else {
                std::slice::from_raw_parts(text, text_len)
            };
            let text = String::from_utf8_lossy(bytes);
            (ctx.f)(text.as_ref(), t0, t1);
        }));
    }

    let abort_ctx = AbortCtx { f: should_abort };
    let mut seg_ctx = SegmentCtx { f: on_segment };
    let abort_bridge = AbortBridge {
        poll: poll_abort,
        user_data: (&abort_ctx as *const AbortCtx<'_>).cast_mut().cast(),
    };
    let segment_bridge = SegmentBridge {
        callback: Some(forward_segment),
        user_data: (&mut seg_ctx as *mut SegmentCtx<'_>).cast(),
    };
    transcribe_samples(
        model,
        samples,
        options,
        Some(&abort_bridge),
        Some(&segment_bridge),
        None,
        None,
    )
}

/// One transcription attempt over pre-decoded 16 kHz mono `f32` PCM samples.
///
/// `model` may be null: empty-sample checks happen first, then the model pointer
/// check, mirroring [`transcribe`].
///
/// # Safety
/// `model` must be null or a live `whisper_context` for the duration of the
/// call (and must not be used concurrently — the caller holds the model
/// lock); the bridges must reference valid pointers/callbacks for the
/// duration of the call.
pub unsafe fn transcribe_samples(
    model: *mut sys::whisper_context,
    samples: &[f32],
    options: &TranscribeOptions,
    abort: Option<&AbortBridge>,
    segment: Option<&SegmentBridge>,
    warning: Option<&WarningBridge>,
    regex_validate: Option<&RegexValidateBridge>,
) -> EngineStatus {
    if samples.is_empty() {
        return EngineStatus::Failed {
            message: ERR_EMPTY_AUDIO.to_string(),
            warn: false,
        };
    }

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

    params.n_threads = resolve_thread_count(options.threads);

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

    let Ok(n_samples) = c_int::try_from(samples.len()) else {
        return EngineStatus::Failed {
            message: "The extracted audio exceeds Whisper's maximum sample count.".to_string(),
            warn: true,
        };
    };
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
    fn standalone_validator_rejects_rust_only_regex_syntax() {
        let rejected = [
            "(?i)hello",
            "(?m:^foo$)",
            "(?P<word>[a-z]+)",
            "(?<word>[a-z]+)",
            r"\p{Hiragana}+",
            r"\P{ASCII}",
            r"[\p{L}]+",
            r"\u{3042}",
            r"\x{41}",
        ];
        for pattern in rejected {
            assert!(
                regex::Regex::new(pattern).is_ok(),
                "fixture {pattern:?} should be accepted by Rust regex"
            );
            assert!(
                validated_suppress_regex(pattern, None, None).is_none(),
                "Rust-only pattern {pattern:?} must be rejected before FFI"
            );
        }

        let accepted = [
            r"^\[_extra_token_[0-9]+\]$",
            "(?:foo|bar)+",
            r"\(?literal\)",
            r"[(?]",
        ];
        for pattern in accepted {
            let out = validated_suppress_regex(pattern, None, None)
                .unwrap_or_else(|| panic!("ECMAScript-compatible {pattern:?} should be accepted"));
            assert_eq!(out.to_bytes(), pattern.as_bytes());
        }
    }

    #[test]
    fn resolve_thread_count_defaults_to_capped_parallelism_when_non_positive() {
        let auto = resolve_thread_count(0);
        assert!((1..=4).contains(&auto), "auto threads out of range: {auto}");
        assert_eq!(resolve_thread_count(-2), auto);
        assert_eq!(resolve_thread_count(1), 1);
        assert_eq!(resolve_thread_count(6), 6);
    }

    #[test]
    fn transcribe_wav_rejects_garbage_header() {
        let dir = std::env::temp_dir().join("subomi-whisper-engine-tests");
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("garbage.wav");
        std::fs::write(&path, [0xACu8; 32]).expect("write fixture");
        let status = unsafe {
            transcribe(
                std::ptr::null_mut(),
                &path,
                &TranscribeOptions::default(),
                None,
                None,
                None,
                None,
            )
        };
        assert_eq!(
            status,
            EngineStatus::Failed {
                message: "The extracted audio is not RIFF/WAVE (header ac ac ac ac ac ac ac ac \
                 ac ac ac ac)."
                    .to_string(),
                warn: true,
            }
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn transcribe_wav_rejects_missing_file() {
        let status = unsafe {
            transcribe(
                std::ptr::null_mut(),
                Path::new("/nonexistent/subomi/audio.wav"),
                &TranscribeOptions::default(),
                None,
                None,
                None,
                None,
            )
        };
        assert_eq!(
            status,
            EngineStatus::Failed {
                message: "Could not open the extracted audio file.".to_string(),
                warn: true,
            }
        );
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

    #[test]
    fn transcribe_wav_rejects_overflowing_chunk_size() {
        let dir = std::env::temp_dir().join("subomi-whisper-engine-tests");
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("overflow-chunk.wav");
        let mut wav = Vec::new();
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&100u32.to_le_bytes());
        wav.extend_from_slice(b"WAVE");
        wav.extend_from_slice(b"fmt ");
        wav.extend_from_slice(&u32::MAX.to_le_bytes());
        wav.extend_from_slice(&[0u8; 16]);
        std::fs::write(&path, &wav).expect("write fixture");
        let status = unsafe {
            transcribe(
                std::ptr::null_mut(),
                &path,
                &TranscribeOptions::default(),
                None,
                None,
                None,
                None,
            )
        };
        assert_eq!(
            status,
            EngineStatus::Failed {
                message: "The extracted WAV file is truncated.".to_string(),
                warn: true,
            }
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn transcribe_samples_validates_empty_samples_and_null_model() {
        let options = TranscribeOptions::default();
        let empty_status = unsafe {
            transcribe_samples_safe(
                std::ptr::null_mut(),
                &[],
                &options,
                &|| false,
                &mut |_, _, _| {},
            )
        };
        assert_eq!(
            empty_status,
            EngineStatus::Failed {
                message: ERR_EMPTY_AUDIO.to_string(),
                warn: false,
            }
        );

        let null_model_status = unsafe {
            transcribe_samples_safe(
                std::ptr::null_mut(),
                &[0.0f32; 160],
                &options,
                &|| false,
                &mut |_, _, _| {},
            )
        };
        assert_eq!(
            null_model_status,
            EngineStatus::Failed {
                message: ERR_MODEL_INIT.to_string(),
                warn: true,
            }
        );
    }
}

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

//! C ABI bridge over the Whisper engine, consumed by the C++ shim
//! (`src/asr/whisper/whispermodel.cpp` when `MEMENTO_ASR_WHISPER_RUST` is
//! defined) and by the equality harness
//! (`tests/mpv-memory-subtitles/whisper_engine_equal_test.cpp`).
//!
//! The declarations live in `src/asr/rust/asr_whisper_capi.h`; keep both in
//! sync.
//!
//! Design rules (mirroring `memento-asr`'s `capi.rs`):
//! - Buffers handed to C are allocated here and released with
//!   [`asr_whisper_free`] (single allocator, no mismatch); see
//!   [`memento_asr::capi::buf_free`].
//! - Strings cross the boundary as NUL-terminated UTF-8 (`CStr`).
//! - Failure messages are byte-identical to the C++ `QStringLiteral`s.
//! - No panics cross the boundary: the engine call is wrapped in
//!   `catch_unwind` and degrades to a `FAILED` status on an internal panic.

use std::ffi::CStr;
use std::os::raw::{c_char, c_int, c_void};
use std::panic::AssertUnwindSafe;

use memento_asr::capi::{buf_free, cstr_to_path, err_clear, err_write, move_to_bridge};

use crate::engine::{
    self, AbortBridge, EngineStatus, RegexValidateBridge, SegmentBridge, TranscribeOptions,
    WarningBridge, STATUS_FAILED, STATUS_FAILED_WITHOUT_WARNING,
};
use crate::whisper_sys as sys;

/// Abort poll forwarded from C++ (null-safe, mirrors `handle_abort`).
pub type AbortPollFn = Option<unsafe extern "C" fn(*mut c_void) -> bool>;
/// Segment callback forwarded to C++.
pub type SegmentCallbackFn =
    Option<unsafe extern "C" fn(*const u8, usize, i64, i64, *mut c_void)>;
/// Mid-call warning callback forwarded to C++ (`qWarning`).
pub type WarningCallbackFn = Option<unsafe extern "C" fn(*const c_char, *mut c_void)>;
/// `std::regex` validator forwarded to C++ (see `engine` docs); the C++
/// implementation emits the reference warning itself on rejection.
pub type RegexValidateFn =
    Option<unsafe extern "C" fn(*const c_char, usize, *mut c_void) -> bool>;

/// Read a NUL-terminated string as bytes; null becomes an empty string
/// (matching an empty `QString` round-tripped through `toUtf8()`).
unsafe fn read_string(ptr: *const c_char) -> Vec<u8> {
    if ptr.is_null() {
        return Vec::new();
    }
    std::ffi::CStr::from_ptr(ptr).to_bytes().to_vec()
}

/// C ABI: `WhisperModel` constructor body —
/// `whisper_context_default_params()` with `use_gpu`, `gpu_device =
/// max(0, gpu_device)`, `flash_attn`, then `whisper_init_from_file_with_params`.
///
/// Returns the raw `whisper_context *` (null on failure, exactly like the C++
/// future's result) so the C++ `WhisperModel` keeps its
/// `QFuture<whisper_context *>` member and header unchanged. Free with
/// [`asr_whisper_model_free`].
///
/// # Safety
/// `model_path` must be a valid NUL-terminated UTF-8 path.
#[no_mangle]
pub unsafe extern "C" fn asr_whisper_model_create(
    model_path: *const c_char,
    use_gpu: bool,
    gpu_device: c_int,
    flash_attention: bool,
) -> *mut sys::whisper_context {
    if model_path.is_null() {
        return std::ptr::null_mut();
    }
    let mut params = sys::whisper_context_default_params();
    params.use_gpu = use_gpu;
    params.gpu_device = std::cmp::max(0, gpu_device);
    params.flash_attn = flash_attention;
    sys::whisper_init_from_file_with_params(model_path, params)
}

/// C ABI: `WhisperModel` destructor body — `whisper_free(ctx)`. Null is a
/// no-op (the C++ destructor calls `whisper_free(getModel())` which must
/// tolerate a failed model load).
///
/// # Safety
/// `model` must be null or a pointer returned by [`asr_whisper_model_create`].
#[no_mangle]
pub unsafe extern "C" fn asr_whisper_model_free(model: *mut sys::whisper_context) {
    if !model.is_null() {
        sys::whisper_free(model);
    }
}

/// C ABI: one `WhisperModel::transcribe` attempt — WAV decode, parameter
/// construction, `whisper_full`, segment/abort/error handling.
///
/// Returns one of the `ASR_WHISPER_STATUS_*` codes declared in
/// `src/asr/rust/asr_whisper_capi.h`:
/// - `ASR_WHISPER_STATUS_SUCCESS` — `whisper_full` returned 0;
/// - `ASR_WHISPER_STATUS_CANCELED` — `whisper_full` failed while the abort
///   poll was set (no failure message);
/// - `ASR_WHISPER_STATUS_FAILED` — C++ would `qWarning(message)` then call
///   the failure callback with `message` (written to `failure_message`);
/// - `ASR_WHISPER_STATUS_FAILED_WITHOUT_WARNING` — the defensive empty-audio
///   branch: call the failure callback only.
///
/// `failure_message` is filled (NUL-terminated, truncated at
/// `failure_message_capacity - 1`) for both failure statuses. `model` may be
/// null: the WAV decode happens first and its failure wins, exactly like the
/// C++ implementation.
///
/// `regex_validate` is invoked with the raw `suppress_regex` bytes when the
/// trimmed pattern is non-empty, at the exact C++ validation point (after
/// the WAV decode and model check). It is authoritative when present; the
/// Rust `regex` crate is used only as a fallback without this callback.
///
/// # Safety
/// Pointers must satisfy the contracts in the header; callbacks must remain
/// valid until the call returns.
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn asr_whisper_transcribe(
    model: *mut sys::whisper_context,
    audio_path: *const c_char,
    language: *const c_char,
    use_vad: bool,
    vad_model: *const c_char,
    suppress_regex: *const c_char,
    threads: c_int,
    best_of: c_int,
    beam_size: c_int,
    abort_poll: AbortPollFn,
    abort_poll_user_data: *mut c_void,
    segment_callback: SegmentCallbackFn,
    segment_callback_user_data: *mut c_void,
    warning_callback: WarningCallbackFn,
    warning_callback_user_data: *mut c_void,
    regex_validate: RegexValidateFn,
    regex_validate_user_data: *mut c_void,
    failure_message: *mut c_char,
    failure_message_capacity: u32,
) -> i32 {
    err_clear(failure_message, failure_message_capacity);

    if audio_path.is_null() {
        // Defensive: C++ always passes a path; an unopenable path produces
        // this message there too (including the `qWarning` side effect,
        // which the `ASR_WHISPER_STATUS_FAILED` status triggers in the shim).
        err_write(
            failure_message,
            failure_message_capacity,
            "Could not open the extracted audio file.",
        );
        return STATUS_FAILED;
    }
    // Reuse the memento-asr bridge path conversion so both bridges agree on
    // platform path encoding (UTF-8 from the C++ `QString`).
    let audio_path = cstr_to_path(CStr::from_ptr(audio_path));

    let options = TranscribeOptions {
        // `QString::toUtf8()` always yields valid UTF-8, so a lossless
        // conversion is available; keep the exact bytes.
        language: String::from_utf8_lossy(&read_string(language)).into_owned(),
        threads,
        best_of,
        beam_size,
        use_vad,
        vad_model: String::from_utf8_lossy(&read_string(vad_model)).into_owned(),
        suppress_regex: String::from_utf8_lossy(&read_string(suppress_regex)).into_owned(),
    };

    let abort = abort_poll.map(|poll| AbortBridge {
        poll,
        user_data: abort_poll_user_data,
    });
    let segment = SegmentBridge {
        callback: segment_callback,
        user_data: segment_callback_user_data,
    };
    let warning = WarningBridge {
        callback: warning_callback,
        user_data: warning_callback_user_data,
    };
    let regex_validate = regex_validate.map(|validate| RegexValidateBridge {
        validate,
        user_data: regex_validate_user_data,
    });

    let status = match std::panic::catch_unwind(AssertUnwindSafe(|| {
        engine::transcribe(
            model,
            &audio_path,
            &options,
            abort.as_ref(),
            Some(&segment),
            Some(&warning),
            regex_validate.as_ref(),
        )
    })) {
        Ok(status) => status,
        Err(_) => EngineStatus::Failed {
            message: "The Rust Whisper engine panicked while transcribing."
                .to_string(),
            warn: true,
        },
    };

    if let Some(message) = status.message() {
        err_write(failure_message, failure_message_capacity, message);
    }
    debug_assert!(
        status.code() == STATUS_FAILED || status.code() == STATUS_FAILED_WITHOUT_WARNING
            || status.message().is_none(),
        "failure statuses always carry a message"
    );
    status.code()
}

/// C ABI: dumps `whisper_full_default_params(strategy)` field by field into a
/// Rust-allocated NUL-terminated UTF-8 buffer (release with
/// [`asr_whisper_free`]). `strategy` uses the `whisper_sampling_strategy`
/// values (0 = greedy, 1 = beam search); any other value clamps to greedy,
/// matching nothing in C++ because C++ only ever passes 0 or 1.
///
/// The output format is a line-per-field `name=value` listing shared with the
/// C++ dumper in
/// `tests/mpv-memory-subtitles/whisper_engine_equal_test.cpp`; floats print as
/// their 8-digit lowercase hex bit patterns and pointers as `null`/`set`, so
/// any layout drift (wrong field offset/size) fails the equality test loudly.
///
/// # Safety
/// `out` / `out_len` must be valid writable locations.
#[no_mangle]
pub unsafe extern "C" fn asr_whisper_full_default_params_dump(
    strategy: c_int,
    out: *mut *mut u8,
    out_len: *mut u64,
) -> bool {
    let strategy = if strategy == 1 {
        sys::whisper_sampling_strategy_WHISPER_SAMPLING_BEAM_SEARCH
    } else {
        sys::whisper_sampling_strategy_WHISPER_SAMPLING_GREEDY
    };
    let params = sys::whisper_full_default_params(strategy);
    let mut text = dump_full_params(&params).into_bytes();
    text.push(0);
    move_to_bridge(text, out, out_len)
}

/// C ABI: dumps `whisper_context_default_params()` field by field (same
/// format rules as [`asr_whisper_full_default_params_dump`]; release the
/// buffer with [`asr_whisper_free`]).
///
/// # Safety
/// `out` / `out_len` must be valid writable locations.
#[no_mangle]
pub unsafe extern "C" fn asr_whisper_context_default_params_dump(
    out: *mut *mut u8,
    out_len: *mut u64,
) -> bool {
    let params = sys::whisper_context_default_params();
    let mut text = dump_context_params(&params).into_bytes();
    text.push(0);
    move_to_bridge(text, out, out_len)
}

/// C ABI: release a buffer returned by any `asr_whisper_*` out parameter.
/// Null is a no-op. Wraps [`memento_asr::capi::buf_free`] so every ASR Rust
/// buffer allocates and frees through the same bridge.
///
/// # Safety
/// `ptr` must be null or a pointer returned by this module.
#[no_mangle]
pub unsafe extern "C" fn asr_whisper_free(ptr: *mut c_void) {
    buf_free(ptr.cast())
}

/// Canonical dump of a `whisper_full_params` value. Field order matches the
/// C++ struct definition in the fetched `whisper.h`; the C++ dumper in the
/// equality test must produce the identical text.
fn dump_full_params(params: &sys::whisper_full_params) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let _ = writeln!(
        out,
        "sizeof={}",
        std::mem::size_of::<sys::whisper_full_params>()
    );
    let _ = writeln!(out, "strategy={}", params.strategy);
    let _ = writeln!(out, "n_threads={}", params.n_threads);
    let _ = writeln!(out, "n_max_text_ctx={}", params.n_max_text_ctx);
    let _ = writeln!(out, "offset_ms={}", params.offset_ms);
    let _ = writeln!(out, "duration_ms={}", params.duration_ms);
    let _ = writeln!(out, "translate={}", flag(params.translate));
    let _ = writeln!(out, "no_context={}", flag(params.no_context));
    let _ = writeln!(out, "no_timestamps={}", flag(params.no_timestamps));
    let _ = writeln!(out, "single_segment={}", flag(params.single_segment));
    let _ = writeln!(out, "print_special={}", flag(params.print_special));
    let _ = writeln!(out, "print_progress={}", flag(params.print_progress));
    let _ = writeln!(out, "print_realtime={}", flag(params.print_realtime));
    let _ = writeln!(out, "print_timestamps={}", flag(params.print_timestamps));
    let _ = writeln!(out, "token_timestamps={}", flag(params.token_timestamps));
    let _ = writeln!(out, "thold_pt={}", hex_f32(params.thold_pt));
    let _ = writeln!(out, "thold_ptsum={}", hex_f32(params.thold_ptsum));
    let _ = writeln!(out, "max_len={}", params.max_len);
    let _ = writeln!(out, "split_on_word={}", flag(params.split_on_word));
    let _ = writeln!(out, "max_tokens={}", params.max_tokens);
    let _ = writeln!(out, "debug_mode={}", flag(params.debug_mode));
    let _ = writeln!(out, "audio_ctx={}", params.audio_ctx);
    let _ = writeln!(out, "tdrz_enable={}", flag(params.tdrz_enable));
    let _ = writeln!(out, "suppress_regex={}", ptr_state(params.suppress_regex));
    let _ = writeln!(out, "initial_prompt={}", ptr_state(params.initial_prompt));
    let _ = writeln!(
        out,
        "carry_initial_prompt={}",
        flag(params.carry_initial_prompt)
    );
    let _ = writeln!(out, "prompt_tokens={}", ptr_state(params.prompt_tokens));
    let _ = writeln!(out, "prompt_n_tokens={}", params.prompt_n_tokens);
    let _ = writeln!(out, "language={}", ptr_state(params.language));
    let _ = writeln!(out, "detect_language={}", flag(params.detect_language));
    let _ = writeln!(out, "suppress_blank={}", flag(params.suppress_blank));
    let _ = writeln!(out, "suppress_nst={}", flag(params.suppress_nst));
    let _ = writeln!(out, "temperature={}", hex_f32(params.temperature));
    let _ = writeln!(out, "max_initial_ts={}", hex_f32(params.max_initial_ts));
    let _ = writeln!(out, "length_penalty={}", hex_f32(params.length_penalty));
    let _ = writeln!(out, "temperature_inc={}", hex_f32(params.temperature_inc));
    let _ = writeln!(out, "entropy_thold={}", hex_f32(params.entropy_thold));
    let _ = writeln!(out, "logprob_thold={}", hex_f32(params.logprob_thold));
    let _ = writeln!(out, "no_speech_thold={}", hex_f32(params.no_speech_thold));
    let _ = writeln!(out, "greedy.best_of={}", params.greedy.best_of);
    let _ = writeln!(out, "beam_search.beam_size={}", params.beam_search.beam_size);
    let _ = writeln!(
        out,
        "beam_search.patience={}",
        hex_f32(params.beam_search.patience)
    );
    let _ = writeln!(
        out,
        "new_segment_callback={}",
        callback_state(params.new_segment_callback)
    );
    let _ = writeln!(
        out,
        "new_segment_callback_user_data={}",
        ptr_state(params.new_segment_callback_user_data)
    );
    let _ = writeln!(
        out,
        "progress_callback={}",
        callback_state(params.progress_callback)
    );
    let _ = writeln!(
        out,
        "progress_callback_user_data={}",
        ptr_state(params.progress_callback_user_data)
    );
    let _ = writeln!(
        out,
        "encoder_begin_callback={}",
        callback_state(params.encoder_begin_callback)
    );
    let _ = writeln!(
        out,
        "encoder_begin_callback_user_data={}",
        ptr_state(params.encoder_begin_callback_user_data)
    );
    let _ = writeln!(
        out,
        "abort_callback={}",
        callback_state(params.abort_callback)
    );
    let _ = writeln!(
        out,
        "abort_callback_user_data={}",
        ptr_state(params.abort_callback_user_data)
    );
    let _ = writeln!(
        out,
        "logits_filter_callback={}",
        callback_state(params.logits_filter_callback)
    );
    let _ = writeln!(
        out,
        "logits_filter_callback_user_data={}",
        ptr_state(params.logits_filter_callback_user_data)
    );
    let _ = writeln!(out, "grammar_rules={}", ptr_state(params.grammar_rules));
    let _ = writeln!(out, "n_grammar_rules={}", params.n_grammar_rules);
    let _ = writeln!(out, "i_start_rule={}", params.i_start_rule);
    let _ = writeln!(out, "grammar_penalty={}", hex_f32(params.grammar_penalty));
    let _ = writeln!(out, "vad={}", flag(params.vad));
    let _ = writeln!(out, "vad_model_path={}", ptr_state(params.vad_model_path));
    let _ = writeln!(
        out,
        "vad_params.threshold={}",
        hex_f32(params.vad_params.threshold)
    );
    let _ = writeln!(
        out,
        "vad_params.min_speech_duration_ms={}",
        params.vad_params.min_speech_duration_ms
    );
    let _ = writeln!(
        out,
        "vad_params.min_silence_duration_ms={}",
        params.vad_params.min_silence_duration_ms
    );
    let _ = writeln!(
        out,
        "vad_params.max_speech_duration_s={}",
        hex_f32(params.vad_params.max_speech_duration_s)
    );
    let _ = writeln!(
        out,
        "vad_params.speech_pad_ms={}",
        params.vad_params.speech_pad_ms
    );
    let _ = writeln!(
        out,
        "vad_params.samples_overlap={}",
        hex_f32(params.vad_params.samples_overlap)
    );
    out
}

/// Canonical dump of a `whisper_context_params` value (format rules as in
/// [`dump_full_params`]).
fn dump_context_params(params: &sys::whisper_context_params) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let _ = writeln!(
        out,
        "sizeof={}",
        std::mem::size_of::<sys::whisper_context_params>()
    );
    let _ = writeln!(out, "use_gpu={}", flag(params.use_gpu));
    let _ = writeln!(out, "flash_attn={}", flag(params.flash_attn));
    let _ = writeln!(out, "gpu_device={}", params.gpu_device);
    let _ = writeln!(
        out,
        "dtw_token_timestamps={}",
        flag(params.dtw_token_timestamps)
    );
    let _ = writeln!(out, "dtw_aheads_preset={}", params.dtw_aheads_preset);
    let _ = writeln!(out, "dtw_n_top={}", params.dtw_n_top);
    let _ = writeln!(out, "dtw_aheads.n_heads={}", params.dtw_aheads.n_heads);
    let _ = writeln!(out, "dtw_aheads.heads={}", ptr_state(params.dtw_aheads.heads));
    let _ = writeln!(out, "dtw_mem_size={}", params.dtw_mem_size);
    out
}

/// `0` / `1` for booleans, matching the C++ dumper.
fn flag(value: bool) -> u8 {
    if value {
        1
    } else {
        0
    }
}

/// 8-digit lowercase hex bits for floats, matching the C++ dumper.
fn hex_f32(value: f32) -> String {
    format!("{:08x}", value.to_bits())
}

/// `null` / `set` for pointers, matching the C++ dumper (addresses are not
/// comparable across implementations).
fn ptr_state<T>(value: *const T) -> &'static str {
    if value.is_null() {
        "null"
    } else {
        "set"
    }
}

/// `null` / `set` for callback function pointers (bindgen models them as
/// `Option<fn>`), matching the C++ dumper's `!= nullptr` checks.
fn callback_state<T>(value: Option<T>) -> &'static str {
    if value.is_some() {
        "set"
    } else {
        "null"
    }
}

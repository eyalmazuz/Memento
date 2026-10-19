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

//! C ABI bridge for the Rust ONNX ASR engine, consumed by the C++
//! `OnnxSession` shim (`src/asr/onnx/onnxsession.cpp`) when
//! `MEMENTO_ASR_ONNX_RUST` is defined. Declarations live in
//! `src/asr/rust/asr_onnx_capi.h`.
//!
//! Threading contract (mirrors the C++ object):
//! - `create` is synchronous (config + tokenizer discovery).
//! - `initialize` is called by the C++ side on a `QtConcurrent` worker and may
//!   run concurrently with `transcribe` calls, which block until it completes.
//! - `transcribe` may be called from any thread.
//! - `free` must be called only after all in-flight calls completed (the C++
//!   destructor waits for the initialization future first); in-flight
//!   `transcribe` calls additionally hold their own reference, so a raced
//!   `free` cannot cause a use-after-free.
//!
//! Every entry point catches panics so unwinding can never cross the C ABI.

use std::ffi::CStr;
use std::os::raw::{c_char, c_void};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::Arc;

use memento_asr::capi::{cstr_to_path, err_clear, err_write};

use crate::engine::{
    panic_payload_message, Callbacks, Engine, Options, RESULT_FAILED,
};

/// C++ `OnnxSession::OnnxSession(OnnxSessionOptions)` (synchronous part).
///
/// Always stores a session handle in `*out_session` (the C++ object exists
/// even when loading failed). Returns `true` when the configuration and
/// tokenizer loaded, mirroring `m_configValid`; on `false` the load error is
/// written into `err` so the caller can log it exactly like the C++
/// constructor does.
///
/// # Safety
/// `model_path` must be a valid NUL-terminated UTF-8 string; `out_session`
/// must be a valid writable pointer; `err` must be null or point to `err_cap`
/// bytes.
#[no_mangle]
pub unsafe extern "C" fn asr_onnx_session_create(
    model_path: *const c_char,
    use_gpu: bool,
    gpu_device: i32,
    threads: i32,
    punctuation: bool,
    out_session: *mut *mut c_void,
    err: *mut c_char,
    err_cap: u32,
) -> bool {
    err_clear(err, err_cap);
    if model_path.is_null() || out_session.is_null() {
        err_write(
            err,
            err_cap,
            "memento-asr-onnx: null argument to asr_onnx_session_create",
        );
        return false;
    }
    let result = catch_unwind(AssertUnwindSafe(|| {
        let path = cstr_to_path(CStr::from_ptr(model_path));
        let engine = Engine::create(
            &path,
            Options {
                use_gpu,
                gpu_device,
                threads,
                punctuation,
            },
        );
        let config_valid = engine.is_loaded();
        if !config_valid {
            err_write(err, err_cap, &engine.load_error());
        }
        *out_session = Arc::into_raw(engine) as *mut c_void;
        config_valid
    }));
    match result {
        Ok(config_valid) => config_valid,
        Err(payload) => {
            err_write(
                err,
                err_cap,
                &format!(
                    "memento-asr-onnx: {}",
                    panic_payload_message(payload.as_ref())
                ),
            );
            false
        }
    }
}

/// C++ `OnnxSession::initializeSessions()` (run on the caller's worker
/// thread). Idempotent; completion is published to `transcribe` waiters.
///
/// # Safety
/// `session` must be a handle from [`asr_onnx_session_create`].
#[no_mangle]
pub unsafe extern "C" fn asr_onnx_session_initialize(session: *mut c_void, ort_api_version: u32) {
    if session.is_null() {
        return;
    }
    let _ = catch_unwind(AssertUnwindSafe(|| {
        let engine = &*(session as *const Engine);
        engine.initialize(ort_api_version);
    }));
}

/// C++ `OnnxSession::isLoaded()` (`m_configValid`; does not wait for
/// initialization, exactly like the C++ accessor).
///
/// # Safety
/// `session` must be a handle from [`asr_onnx_session_create`].
#[no_mangle]
pub unsafe extern "C" fn asr_onnx_session_is_loaded(session: *mut c_void) -> bool {
    if session.is_null() {
        return false;
    }
    catch_unwind(AssertUnwindSafe(|| {
        let engine = &*(session as *const Engine);
        engine.is_loaded()
    }))
    .unwrap_or(false)
}

/// C++ `OnnxSession::loadError()`: copies the current load error string
/// (construction-stage, or the initialization failure once completed) into
/// `err` and returns the full byte length (truncated copy when longer than
/// `err_cap - 1`).
///
/// # Safety
/// `session` must be a handle from [`asr_onnx_session_create`]; `err` must be
/// null or point to `err_cap` bytes.
#[no_mangle]
pub unsafe extern "C" fn asr_onnx_session_load_error(
    session: *mut c_void,
    err: *mut c_char,
    err_cap: u32,
) -> u32 {
    err_clear(err, err_cap);
    if session.is_null() {
        return 0;
    }
    let result = catch_unwind(AssertUnwindSafe(|| {
        let engine = &*(session as *const Engine);
        engine.load_error()
    }));
    match result {
        Ok(message) => {
            err_write(err, err_cap, &message);
            message.len() as u32
        }
        Err(_) => 0,
    }
}

/// C++ `OnnxSession::transcribeSync(audioPath, options)`: the full
/// transcription pipeline (WAV read, initialization wait, architecture
/// dispatch, callbacks) with abort checks at the C++ positions.
///
/// Returns `0`/`1`/`2` for `AsrTranscriptionResult::Success`/`Failed`/
/// `Canceled` (asserted by the C++ shim).
///
/// # Safety
/// `session` must be a handle from [`asr_onnx_session_create`], `audio_path`
/// and `language` valid NUL-terminated UTF-8 strings. Callback contexts must
/// be valid for the duration of the call; null function pointers mean "no
/// callback".
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn asr_onnx_session_transcribe(
    session: *mut c_void,
    audio_path: *const c_char,
    language: *const c_char,
    abort_fn: Option<unsafe extern "C" fn(ctx: *mut c_void) -> bool>,
    abort_ctx: *mut c_void,
    fail_fn: Option<unsafe extern "C" fn(ctx: *mut c_void, message: *const c_char)>,
    fail_ctx: *mut c_void,
    segment_fn: Option<unsafe extern "C" fn(ctx: *mut c_void, text: *const c_char, start: f64, end: f64)>,
    segment_ctx: *mut c_void,
) -> i32 {
    if session.is_null() || audio_path.is_null() || language.is_null() {
        return RESULT_FAILED;
    }
    let result = catch_unwind(AssertUnwindSafe(|| {
        // `session` is `Arc::into_raw`'s data pointer; `Arc::from_raw`
        // reconstructs the handle's reference. Clone it for this call so in
        // -flight calls survive a racing free, then forget the reconstruction
        // to leave the handle's reference untouched in the raw token.
        let handle: Arc<Engine> = Arc::from_raw(session as *const Engine);
        let engine: Arc<Engine> = Arc::clone(&handle);
        std::mem::forget(handle);
        let path = cstr_to_path(CStr::from_ptr(audio_path));
        let language = String::from_utf8_lossy(CStr::from_ptr(language).to_bytes()).into_owned();
        let callbacks = Callbacks {
            abort: abort_fn,
            abort_ctx,
            fail: fail_fn,
            fail_ctx,
            segment: segment_fn,
            segment_ctx,
        };
        engine.transcribe(&path, &language, &callbacks)
    }));
    match result {
        Ok(code) => code,
        Err(payload) => {
            // convert a panic into the same failure contract the engine uses
            if let Some(fail) = fail_fn {
                if let Ok(message) = std::ffi::CString::new(format!(
                    "ONNX inference error: {}",
                    panic_payload_message(payload.as_ref())
                )) {
                    fail(fail_ctx, message.as_ptr());
                }
            }
            RESULT_FAILED
        }
    }
}

/// C++ `~OnnxSession()` (after the initialization future finished). Null is a
/// no-op.
///
/// # Safety
/// `session` must be a handle from [`asr_onnx_session_create`] that was not
/// freed yet.
#[no_mangle]
pub unsafe extern "C" fn asr_onnx_session_free(session: *mut c_void) {
    if session.is_null() {
        return;
    }
    let _ = catch_unwind(AssertUnwindSafe(|| {
        drop(Arc::from_raw(session as *const Engine));
    }));
}

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

//! C ABI bridge over the `memento-asr` primitives consumed by the C++ ONNX
//! session (`src/asr/onnx/onnxsession.cpp`).
//!
//! Design rules:
//! - Every out buffer handed to C is allocated on the Rust side and MUST be
//!   released with [`asr_rust_free`] (single allocator, no mismatch).
//! - Strings cross the boundary as NUL-terminated UTF-8 (`CStr`).
//! - Error text returned through the WAV reader out-param is byte-identical
//!   to the C++ `AsrAudio::readPcm16Wav` error strings.
//! - The bridge is pass-through only: no buffering, no float rescaling, no
//!   reordering, so results are bit-identical to direct Rust calls.
//! - The functions never panic on caller protocol violations; they return
//!   `false` (and, where an error buffer exists, describe why).

use std::alloc::{alloc, dealloc, Layout};
use std::ffi::CStr;
use std::os::raw::{c_char, c_void};
use std::panic::{catch_unwind, AssertUnwindSafe};

use crate::asrcontroller::{self, AsrControllerCore, AsrWindowDecision};

use crate::audio::melfrontend::{MelFrontend, MelFrontendConfig};
use crate::audio::wavreader;
use crate::onnx::onnxcontroller;
use crate::whisper::whispercontroller;

/// Magic marker stored in the header of every allocation crossing the C
/// boundary, so [`asr_rust_free`] can refuse foreign pointers instead of
/// corrupting the heap.
const BUF_HEADER_MAGIC: u64 = 0x4D454E54_2D415352; // "MENT-ASR"

/// Size of the allocation header (offset of user data inside the allocation).
/// Also the alignment of the whole allocation, which is >= the alignment of
/// every element type that crosses the boundary (f32/i64/u8/u64/f64).
const BUF_HEADER_SIZE: usize = 16;
const BUF_ALIGNMENT: usize = 16;

/// Header written immediately before every bridge-owned buffer.
#[repr(C)]
struct BufHeader {
    magic: u64,
    total_bytes: usize,
}

#[inline]
pub fn len_to_usize(len: u64) -> Option<usize> {
    usize::try_from(len).ok()
}

/// Allocate `len_bytes` of memory with a [`BufHeader`] prefix and return a
/// pointer to the user data region.
///
/// # Safety
/// Returns a pointer that must be freed with [`asr_rust_free`]. Panics
/// (abort) only on allocation failure.
pub unsafe fn buf_alloc(len_bytes: usize) -> *mut u8 {
    debug_assert!(len_bytes > 0);
    let total_bytes = BUF_HEADER_SIZE + len_bytes;
    // BUF_ALIGNMENT (16) >= BufHeader alignment (8): the header write is aligned.
    let layout = Layout::from_size_align(total_bytes, BUF_ALIGNMENT)
        .expect("memento-asr bridge: allocation size overflow");
    let base = alloc(layout);
    if base.is_null() {
        // Mirror Rust's global allocator failure behavior instead of returning
        // a null data region that callers would treat as an empty buffer.
        std::alloc::handle_alloc_error(layout);
    }
    let header = base as *mut BufHeader;
    (*header).magic = BUF_HEADER_MAGIC;
    (*header).total_bytes = total_bytes;
    base.add(BUF_HEADER_SIZE)
}

/// Release a buffer previously returned by this bridge. Null is a no-op.
///
/// # Safety
/// `ptr` must be null or a pointer returned by one of the `asr_rust_*`
/// functions of this module.
pub unsafe fn buf_free(ptr: *mut u8) {
    if ptr.is_null() {
        return;
    }
    let base = ptr.sub(BUF_HEADER_SIZE);
    let header = base as *const BufHeader;
    if (*header).magic != BUF_HEADER_MAGIC {
        // Not a bridge-owned pointer: refuse to touch it (leak, not UB).
        debug_assert!(false, "asr_rust_free called with a foreign pointer");
        std::eprintln!("memento-asr bridge: asr_rust_free called with a foreign pointer");
        return;
    }
    let layout = Layout::from_size_align_unchecked((*header).total_bytes, BUF_ALIGNMENT);
    dealloc(base, layout);
}

/// Move a Rust vector across the boundary as an owned buffer.
///
/// Zero-length vectors are reported through `out_len` with a null data
/// pointer (C++ must check the length before dereferencing).
///
/// # Safety
/// Out-params must be valid, writable, non-overlapping locations.
pub unsafe fn move_to_bridge<T>(vec: Vec<T>, out_ptr: *mut *mut T, out_len: *mut u64) -> bool {
    if out_ptr.is_null() || out_len.is_null() {
        return false;
    }
    let len = vec.len();
    if len == 0 {
        *out_ptr = std::ptr::null_mut();
        *out_len = 0;
        return true;
    }
    let byte_len = match len.checked_mul(std::mem::size_of::<T>()) {
        Some(n) => n,
        None => return false,
    };
    let data = buf_alloc(byte_len) as *mut T;
    std::ptr::copy_nonoverlapping(vec.as_ptr(), data, len);
    *out_ptr = data;
    *out_len = len as u64;
    true
}

/// Zero the error buffer so callers always see a NUL-terminated string.
///
/// # Safety
/// `err` must be null or point to `err_cap` writable bytes.
pub unsafe fn err_clear(err: *mut c_char, err_cap: u32) {
    if !err.is_null() && err_cap > 0 {
        *err = 0;
    }
}

/// Copy `message` into the caller's error buffer, truncating at `err_cap - 1`
/// bytes and always leaving the buffer NUL-terminated.
///
/// # Safety
/// `err` must be null or point to `err_cap` writable bytes.
pub unsafe fn err_write(err: *mut c_char, err_cap: u32, message: &str) {
    if err.is_null() || err_cap == 0 {
        return;
    }
    let cap = err_cap as usize;
    let bytes = message.as_bytes();
    let copy_len = bytes.len().min(cap - 1);
    std::ptr::copy_nonoverlapping(bytes.as_ptr(), err as *mut u8, copy_len);
    *err.add(copy_len) = 0;
}

/// # Safety
/// `samples` must be null when `samples_len == 0`, otherwise point to
/// `samples_len` readable floats.
pub unsafe fn coerce_samples<'a>(samples: *const f32, samples_len: u64) -> Option<&'a [f32]> {
    let num_samples = len_to_usize(samples_len)?;
    if samples.is_null() {
        if num_samples == 0 {
            return Some(&[]);
        }
        return None;
    }
    Some(std::slice::from_raw_parts(samples, num_samples))
}

/// Convert a NUL-terminated C path into a `PathBuf`.
#[allow(unused_variables)]
pub fn cstr_to_path(cstr: &CStr) -> std::path::PathBuf {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        std::path::PathBuf::from(std::ffi::OsStr::from_bytes(cstr.to_bytes()))
    }
    #[cfg(not(unix))]
    {
        // Non-UTF8 bytes are not representable through the C++ side (which
        // passes UTF-8 QString paths), so a lossy round trip is the best
        // available approximation on other platforms.
        let lossy = String::from_utf8_lossy(cstr.to_bytes());
        std::path::PathBuf::from(lossy.as_ref())
    }
}

/// C ABI: `AsrAudio::readPcm16Wav(path, error, expectedSampleRate)`.
///
/// Reads `path` and returns normalized float PCM samples in [-1, 1].
///
/// On success fills `*samples` / `*samples_len` (buffer allocated here;
/// release with [`asr_rust_free`]; length is always > 0) and returns `true`.
/// On failure returns `false`, leaves the outputs as null/0, and writes a
/// NUL-terminated UTF-8 message (byte-identical to the C++ implementation)
/// into `err`, truncated to `err_cap - 1` bytes.
///
/// # Safety
/// `path` must be a valid NUL-terminated string; the out-params must be valid
/// writable locations; `err` must be null or point to `err_cap` bytes.
#[no_mangle]
pub unsafe extern "C" fn asr_rust_read_pcm16_wav(
    path: *const c_char,
    expected_sample_rate: u32,
    samples: *mut *mut f32,
    samples_len: *mut u64,
    err: *mut c_char,
    err_cap: u32,
) -> bool {
    err_clear(err, err_cap);
    if path.is_null() || samples.is_null() || samples_len.is_null() {
        return false;
    }
    match wavreader::read_pcm16_wav_path(cstr_to_path(CStr::from_ptr(path)), expected_sample_rate) {
        Ok(samples_vec) => {
            if samples_vec.is_empty() {
                // The Rust reader only returns Ok for non-empty PCM data, but
                // guard the contract explicitly: empty success is the C++
                // failure sentinel.
                *samples = std::ptr::null_mut();
                *samples_len = 0;
                return false;
            }
            if !move_to_bridge(samples_vec, samples, samples_len) {
                return false;
            }
            true
        }
        Err(message) => {
            *samples = std::ptr::null_mut();
            *samples_len = 0;
            err_write(err, err_cap, &message);
            false
        }
    }
}

/// C ABI: `AsrAudio::MelFrontend::extract(samples, numSamples)` with the
/// `MelFrontendConfig` fields passed individually (avoids a struct ABI).
///
/// On success fills the features / mask out-buffers (allocated here; release
/// each with [`asr_rust_free`]; null + length 0 when empty) and the scalar
/// outputs `out_num_frames` / `out_valid_frames` / `out_n_mels`, returning
/// `true`. Failure (`false`) indicates a caller protocol violation only.
///
/// # Safety
/// `samples` must be null when `samples_len == 0`, otherwise point to
/// `samples_len` readable floats; out-params must be valid writable locations.
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn asr_rust_mel_extract(
    sample_rate: i32,
    n_fft: i32,
    win_length: i32,
    hop_length: i32,
    n_mels: i32,
    preemphasis: f32,
    log_zero_guard: f32,
    norm_epsilon: f32,
    normalize_per_feature: bool,
    samples: *const f32,
    samples_len: u64,
    features: *mut *mut f32,
    features_len: *mut u64,
    masks_i64: *mut *mut i64,
    masks_i64_len: *mut u64,
    masks_u8: *mut *mut u8,
    masks_u8_len: *mut u64,
    out_num_frames: *mut i64,
    out_valid_frames: *mut i64,
    out_n_mels: *mut i64,
) -> bool {
    if features.is_null()
        || features_len.is_null()
        || masks_i64.is_null()
        || masks_i64_len.is_null()
        || masks_u8.is_null()
        || masks_u8_len.is_null()
        || out_num_frames.is_null()
        || out_valid_frames.is_null()
        || out_n_mels.is_null()
    {
        return false;
    }
    let sample_slice = match coerce_samples(samples, samples_len) {
        Some(slice) => slice,
        None => return false,
    };

    let config = MelFrontendConfig {
        sample_rate,
        n_fft,
        win_length,
        hop_length,
        n_mels,
        preemphasis,
        log_zero_guard,
        norm_epsilon,
        normalize_per_feature,
    };
    let frontend = MelFrontend::new(config);
    let output = frontend.extract(sample_slice);

    if !move_to_bridge(output.features, features, features_len) {
        return false;
    }
    if !move_to_bridge(output.attention_mask_int64, masks_i64, masks_i64_len) {
        asr_rust_free((*features).cast());
        *features = std::ptr::null_mut();
        *features_len = 0;
        return false;
    }
    if !move_to_bridge(output.attention_mask_bool, masks_u8, masks_u8_len) {
        asr_rust_free((*features).cast());
        asr_rust_free((*masks_i64).cast());
        *features = std::ptr::null_mut();
        *features_len = 0;
        *masks_i64 = std::ptr::null_mut();
        *masks_i64_len = 0;
        return false;
    }
    *out_num_frames = output.num_frames;
    *out_valid_frames = output.valid_frames;
    *out_n_mels = output.n_mels;
    true
}

/// C ABI: `AsrAudio::MelFrontend::segmentSpeechUtterances(samples, numSamples,
/// sampleRate, maxUtteranceSec)`.
///
/// On success fills four parallel arrays (allocated here; release each with
/// [`asr_rust_free`]) with the span start/end sample indices and start/end
/// times in seconds of every detected speech utterance, sets `*count`, and
/// returns `true`. A count of 0 (silence) is reported as success with null
/// arrays.
///
/// # Safety
/// `samples` must be null when `samples_len == 0`, otherwise point to
/// `samples_len` readable floats; out-params must be valid writable locations.
#[no_mangle]
pub unsafe extern "C" fn asr_rust_segment_speech_utterances(
    samples: *const f32,
    samples_len: u64,
    sample_rate: i32,
    max_utterance_sec: f64,
    out_start_sample: *mut *mut u64,
    out_end_sample: *mut *mut u64,
    out_start_sec: *mut *mut f64,
    out_end_sec: *mut *mut f64,
    count: *mut u64,
) -> bool {
    if out_start_sample.is_null()
        || out_end_sample.is_null()
        || out_start_sec.is_null()
        || out_end_sec.is_null()
        || count.is_null()
    {
        return false;
    }
    let sample_slice = match coerce_samples(samples, samples_len) {
        Some(slice) => slice,
        None => return false,
    };

    let spans =
        MelFrontend::segment_speech_utterances(sample_slice, sample_rate, max_utterance_sec);

    let mut start_samples: Vec<u64> = Vec::with_capacity(spans.len());
    let mut end_samples: Vec<u64> = Vec::with_capacity(spans.len());
    let mut start_secs: Vec<f64> = Vec::with_capacity(spans.len());
    let mut end_secs: Vec<f64> = Vec::with_capacity(spans.len());
    for span in &spans {
        // usize -> u64 is lossless on every supported platform (64-bit).
        start_samples.push(span.start_sample as u64);
        end_samples.push(span.end_sample as u64);
        start_secs.push(span.start_sec);
        end_secs.push(span.end_sec);
    }

    // All four arrays share the same length (`span_count`); the first bridge
    // move reports it through `count`, the rest through scratch locations
    // (non-overlapping with the caller-owned `count`).
    let mut scratch: u64 = 0;
    if !move_to_bridge(start_samples, out_start_sample, count) {
        return false;
    }
    if !move_to_bridge(end_samples, out_end_sample, &mut scratch) {
        return false;
    }
    if !move_to_bridge(start_secs, out_start_sec, &mut scratch) {
        return false;
    }
    if !move_to_bridge(end_secs, out_end_sec, &mut scratch) {
        return false;
    }
    true
}

/// C ABI: release a buffer returned by any of the other `asr_rust_*`
/// functions. Null is a no-op. Freeing a pointer not produced by this bridge
/// is a caller bug; the implementation refuses to free such pointers.
#[no_mangle]
pub unsafe extern "C" fn asr_rust_free(ptr: *mut c_void) {
    buf_free(ptr.cast())
}

// A null pointer is not an empty QString. Empty strings are valid and must
// reach the controller cores unchanged (notably for filename/path fallback).
unsafe fn controller_input<'a>(input: *const c_char) -> Option<&'a str> {
    if input.is_null() {
        return None;
    }
    CStr::from_ptr(input).to_str().ok()
}

fn controller_bool(f: impl FnOnce() -> Option<bool>) -> bool {
    catch_unwind(AssertUnwindSafe(f)).ok().flatten().unwrap_or(false)
}

fn controller_flag(f: impl FnOnce() -> Option<bool>) -> i32 {
    i32::from(controller_bool(f))
}

/// Valid string results, even empty ones, are successful. `move_to_bridge`
/// copies the byte vector to the same tagged allocation as every other ASR
/// bridge output; a panic never crosses the extern "C" boundary.
unsafe fn controller_string(
    out: *mut *mut u8,
    len: *mut u64,
    f: impl FnOnce() -> Option<String>,
) -> bool {
    if out.is_null() || len.is_null() {
        return false;
    }
    *out = std::ptr::null_mut();
    *len = 0;
    catch_unwind(AssertUnwindSafe(|| {
        match f() {
            Some(value) => move_to_bridge(value.into_bytes(), out, len),
            None => false,
        }
    }))
    .unwrap_or(false)
}

#[no_mangle]
pub unsafe extern "C" fn asr_whisper_controller_is_managed_model(model: *const c_char) -> i32 {
    controller_flag(|| Some(whispercontroller::is_managed_model(controller_input(model)?)))
}

#[no_mangle]
pub unsafe extern "C" fn asr_whisper_controller_model_filename(
    model: *const c_char, out: *mut *mut u8, len: *mut u64,
) -> bool {
    controller_string(out, len, || Some(whispercontroller::model_filename(controller_input(model)?)))
}

#[no_mangle]
pub unsafe extern "C" fn asr_whisper_controller_whisper_model_url(
    model: *const c_char, out: *mut *mut u8, len: *mut u64,
) -> bool {
    controller_string(out, len, || Some(whispercontroller::whisper_model_url(controller_input(model)?)))
}

#[no_mangle]
pub unsafe extern "C" fn asr_whisper_controller_vad_model_url(
    model_path: *const c_char, out: *mut *mut u8, len: *mut u64,
) -> bool {
    controller_string(out, len, || Some(whispercontroller::vad_model_url(controller_input(model_path)?)))
}

#[no_mangle]
pub unsafe extern "C" fn asr_whisper_controller_models_directory(
    config_dir: *const c_char, out: *mut *mut u8, len: *mut u64,
) -> bool {
    controller_string(out, len, || Some(whispercontroller::models_directory(controller_input(config_dir)?)))
}

#[no_mangle]
pub unsafe extern "C" fn asr_whisper_controller_model_available(
    model: *const c_char, config_dir: *const c_char,
) -> i32 {
    controller_flag(|| Some(whispercontroller::model_available(controller_input(model)?, controller_input(config_dir)?)))
}

#[no_mangle]
pub unsafe extern "C" fn asr_whisper_controller_has_any_model(config_dir: *const c_char) -> i32 {
    controller_flag(|| Some(whispercontroller::has_any_model(controller_input(config_dir)?)))
}

#[no_mangle]
pub unsafe extern "C" fn asr_whisper_controller_resolve_model_path(
    path: *const c_char,
    suffixes: *const *const c_char,
    count: u32,
    config_dir: *const c_char,
    out: *mut *mut u8,
    len: *mut u64,
) -> bool {
    controller_string(out, len, || {
        let path = controller_input(path)?;
        let config_dir = controller_input(config_dir)?;
        if count != 0 && suffixes.is_null() {
            return None;
        }
        // The caller owns the array and each NUL-terminated string until the
        // call returns. Never form a slice from a null pointer, even for zero.
        let mut parsed = Vec::new();
        if count != 0 {
            if (count as usize) > isize::MAX as usize / std::mem::size_of::<*const c_char>() {
                return None;
            }
            for &suffix in std::slice::from_raw_parts(suffixes, count as usize) {
                parsed.push(controller_input(suffix)?);
            }
        }
        Some(whispercontroller::resolve_model_path(path, &parsed, config_dir))
    })
}

#[no_mangle]
pub unsafe extern "C" fn asr_onnx_controller_is_managed_model(model: *const c_char) -> i32 {
    controller_flag(|| Some(onnxcontroller::is_managed_model(controller_input(model)?)))
}

#[no_mangle]
pub unsafe extern "C" fn asr_onnx_controller_model_files(
    model: *const c_char, out: *mut *mut u8, len: *mut u64,
) -> bool {
    controller_string(out, len, || {
        let files = onnxcontroller::model_files(controller_input(model)?);
        serde_json::to_string(&files).ok()
    })
}

#[no_mangle]
pub unsafe extern "C" fn asr_onnx_controller_models_directory(
    config_dir: *const c_char, out: *mut *mut u8, len: *mut u64,
) -> bool {
    controller_string(out, len, || Some(onnxcontroller::models_directory(controller_input(config_dir)?)))
}

#[no_mangle]
pub unsafe extern "C" fn asr_onnx_controller_model_directory(
    model: *const c_char, config_dir: *const c_char, out: *mut *mut u8, len: *mut u64,
) -> bool {
    controller_string(out, len, || Some(onnxcontroller::model_directory(controller_input(model)?, controller_input(config_dir)?)))
}

#[no_mangle]
pub unsafe extern "C" fn asr_onnx_controller_model_available(
    model: *const c_char, config_dir: *const c_char,
) -> bool {
    controller_bool(|| Some(onnxcontroller::model_available(controller_input(model)?, controller_input(config_dir)?)))
}

#[no_mangle]
pub unsafe extern "C" fn asr_onnx_controller_has_any_model(
    selected: *const c_char, config_dir: *const c_char,
) -> bool {
    controller_bool(|| Some(onnxcontroller::has_any_model(controller_input(selected)?, controller_input(config_dir)?)))
}

// Opaque Rust cue book owned by the C++ AsrController. Non-null handles must
// be valid allocations from `asr_controller_core_new`, used serially and
// released exactly once. Catch panics before they can cross the C boundary;
// invalid/null handles have explicitly defined fallback behavior.
#[no_mangle]
pub extern "C" fn asr_controller_core_new() -> *mut AsrControllerCore {
    catch_unwind(|| Box::into_raw(Box::new(AsrControllerCore::default())))
        .unwrap_or(std::ptr::null_mut())
}

#[no_mangle]
pub unsafe extern "C" fn asr_controller_core_free(core: *mut AsrControllerCore) {
    if !core.is_null() {
        let _ = catch_unwind(AssertUnwindSafe(|| drop(Box::from_raw(core))));
    }
}

#[no_mangle]
pub unsafe extern "C" fn asr_controller_core_len(core: *const AsrControllerCore) -> u64 {
    catch_unwind(|| core.as_ref().map_or(0, |core| core.len() as u64)).unwrap_or(0)
}

#[no_mangle]
pub unsafe extern "C" fn asr_controller_core_clear(core: *mut AsrControllerCore) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        if let Some(core) = core.as_mut() {
            core.clear();
        }
    }));
}

#[no_mangle]
pub unsafe extern "C" fn asr_controller_core_first_overlap(
    core: *const AsrControllerCore, start: f64, end: f64, id: *mut i64,
) -> bool {
    if id.is_null() {
        return false;
    }
    catch_unwind(|| {
        match core.as_ref().and_then(|core| core.first_overlap(start, end)) {
            Some(first) => {
                *id = first;
                true
            }
            None => false,
        }
    }).unwrap_or(false)
}

#[no_mangle]
pub unsafe extern "C" fn asr_controller_core_erase(
    core: *mut AsrControllerCore, id: i64,
) -> bool {
    catch_unwind(AssertUnwindSafe(|| {
        core.as_mut().is_some_and(|core| core.erase(id))
    })).unwrap_or(false)
}

#[no_mangle]
pub unsafe extern "C" fn asr_controller_core_insert(
    core: *mut AsrControllerCore, id: i64, start: f64, end: f64,
) -> bool {
    catch_unwind(AssertUnwindSafe(|| {
        let Some(core) = core.as_mut() else { return false };
        core.insert(id, start, end);
        true
    })).unwrap_or(false)
}

#[no_mangle]
pub unsafe extern "C" fn asr_controller_core_skip_covered_position(
    core: *const AsrControllerCore, position: f64,
) -> f64 {
    catch_unwind(|| {
        core.as_ref().map_or(position, |core| core.skip_covered_position(position))
    }).unwrap_or(position)
}

#[no_mangle]
pub unsafe extern "C" fn asr_controller_core_next_subtitle_start(
    core: *const AsrControllerCore, position: f64,
) -> f64 {
    catch_unwind(|| {
        core.as_ref().map_or(f64::INFINITY, |core| core.next_subtitle_start(position))
    }).unwrap_or(f64::INFINITY)
}

#[no_mangle]
pub extern "C" fn asr_controller_core_seeked(
    position: f64, last: f64, elapsed: f64, paused: bool,
) -> bool {
    catch_unwind(|| asrcontroller::seeked(position, last, elapsed, paused)).unwrap_or(false)
}

#[no_mangle]
pub unsafe extern "C" fn asr_controller_core_plan_window(
    core: *const AsrControllerCore, start: f64, duration: f64, rewound: bool,
) -> AsrWindowDecision {
    catch_unwind(|| {
        core.as_ref().map_or_else(
            || AsrWindowDecision::done(start, rewound),
            |core| core.plan_window(start, duration, rewound),
        )
    }).unwrap_or_else(|_| AsrWindowDecision::done(start, rewound))
}

#[no_mangle]
pub unsafe extern "C" fn asr_controller_core_initial_start(
    core: *const AsrControllerCore,
    start_from_beginning: bool,
    has_consumed_restart: bool,
    restart_position: f64,
    time_position: f64,
    duration: f64,
) -> f64 {
    catch_unwind(|| {
        match core.as_ref() {
            Some(core) => core.initial_start(
                start_from_beginning,
                has_consumed_restart,
                restart_position,
                time_position,
                duration,
            ),
            None => asrcontroller::plan_initial_start(
                None,
                start_from_beginning,
                has_consumed_restart,
                restart_position,
                time_position,
                duration,
            ),
        }
    })
    .unwrap_or(0.0)
}

#[no_mangle]
pub unsafe extern "C" fn asr_controller_core_adjust_segment(
    seg_start: f64,
    seg_end: f64,
    win_start: f64,
    win_end: f64,
    out_start: *mut f64,
    out_end: *mut f64,
) -> bool {
    if out_start.is_null() || out_end.is_null() {
        return false;
    }
    catch_unwind(AssertUnwindSafe(|| {
        let adjusted = asrcontroller::adjust_segment(seg_start, seg_end, win_start, win_end);
        *out_start = adjusted.start;
        *out_end = adjusted.end;
        true
    }))
    .unwrap_or(false)
}

#[no_mangle]
pub extern "C" fn asr_controller_core_cue_valid(
    text_non_empty: bool,
    start: f64,
    end: f64,
) -> bool {
    catch_unwind(|| asrcontroller::is_cue_valid(text_non_empty, start, end)).unwrap_or(false)
}

#[no_mangle]
pub unsafe extern "C" fn asr_controller_core_plan_position_update(
    core: *const AsrControllerCore,
    position: f64,
    duration: f64,
    seeked: bool,
    start_from_beginning: bool,
    running: bool,
    active: bool,
    has_controller: bool,
    retry_paused: bool,
) -> i32 {
    catch_unwind(|| {
        match core.as_ref() {
            Some(core) => core.plan_position_update(
                position,
                duration,
                seeked,
                start_from_beginning,
                running,
                active,
                has_controller,
                retry_paused,
            ) as i32,
            None => asrcontroller::plan_position_update(
                None,
                position,
                duration,
                seeked,
                start_from_beginning,
                running,
                active,
                has_controller,
                retry_paused,
            ) as i32,
        }
    })
    .unwrap_or(asrcontroller::PositionUpdateAction::None as i32)
}

#[no_mangle]
pub unsafe extern "C" fn asr_controller_core_on_failure(
    consecutive_failures: *mut i32,
    time_position: f64,
    duration: f64,
    out_next_start: *mut f64,
) -> bool {
    if consecutive_failures.is_null() || out_next_start.is_null() {
        return false;
    }
    catch_unwind(AssertUnwindSafe(|| {
        let decision = asrcontroller::on_failure(*consecutive_failures, time_position, duration);
        *consecutive_failures = decision.consecutive_failures;
        *out_next_start = decision.next_start;
        decision.action == asrcontroller::FailureAction::PauseAfterMaxFailures
    }))
    .unwrap_or(false)
}

#[cfg(test)]
mod controller_core_ffi_tests {
    use super::*;
    use std::mem::{align_of, size_of};

    #[test]
    fn layout_and_null_contract() {
        assert_eq!(size_of::<AsrWindowDecision>(), 24);
        assert_eq!(align_of::<AsrWindowDecision>(), align_of::<f64>());
        unsafe {
            let mut id = 44;
            let core = std::ptr::null_mut();
            asr_controller_core_free(core);
            assert_eq!(asr_controller_core_len(core), 0);
            asr_controller_core_clear(core);
            assert!(!asr_controller_core_first_overlap(core, 0.0, 1.0, &mut id));
            assert_eq!(id, 44);
            assert!(!asr_controller_core_erase(core, id));
            assert!(!asr_controller_core_insert(core, id, 0.0, 1.0));
            assert_eq!(asr_controller_core_skip_covered_position(core, 2.0), 2.0);
            assert_eq!(asr_controller_core_next_subtitle_start(core, 0.0), f64::INFINITY);
            assert_eq!(asr_controller_core_plan_window(core, 4.0, 20.0, false),
                AsrWindowDecision::done(4.0, false));

            // Initial start with null core
            assert_eq!(asr_controller_core_initial_start(core, false, false, 0.0, 5.0, 20.0), 5.0);
            assert_eq!(asr_controller_core_initial_start(core, true, false, 0.0, 5.0, 20.0), 0.0);

            // Adjust segment null outputs
            let mut out_s = 0.0;
            let mut out_e = 0.0;
            assert!(!asr_controller_core_adjust_segment(1.0, 2.0, 10.0, 20.0, std::ptr::null_mut(), &mut out_e));
            assert!(!asr_controller_core_adjust_segment(1.0, 2.0, 10.0, 20.0, &mut out_s, std::ptr::null_mut()));
            assert!(asr_controller_core_adjust_segment(1.0, 2.0, 10.0, 20.0, &mut out_s, &mut out_e));
            assert_eq!(out_s, 11.0);
            assert_eq!(out_e, 12.0);

            // Cue valid
            assert!(asr_controller_core_cue_valid(true, 1.0, 2.0));
            assert!(!asr_controller_core_cue_valid(false, 1.0, 2.0));

            // Position update with null core
            assert_eq!(
                asr_controller_core_plan_position_update(core, 5.0, 60.0, true, false, false, true, true, false),
                2 // RequestRestartAndSelect
            );

            // On failure null checks and increment
            let mut failures = 1;
            let mut next_start = 0.0;
            assert!(!asr_controller_core_on_failure(std::ptr::null_mut(), 5.0, 20.0, &mut next_start));
            assert!(!asr_controller_core_on_failure(&mut failures, 5.0, 20.0, std::ptr::null_mut()));
            assert!(!asr_controller_core_on_failure(&mut failures, 5.0, 20.0, &mut next_start));
            assert_eq!(failures, 2);
            assert_eq!(next_start, 5.0);
            assert!(asr_controller_core_on_failure(&mut failures, 5.0, 20.0, &mut next_start));
            assert_eq!(failures, 3);
        }
    }

    #[test]
    fn bridge_owns_cues_across_calls() {
        unsafe {
            let core = asr_controller_core_new();
            assert!(!core.is_null());
            assert!(asr_controller_core_insert(core, 5, 0.0, 1.0));
            assert!(asr_controller_core_insert(core, 6, 0.0, 1.0));
            assert_eq!(asr_controller_core_len(core), 2);
            let mut id = -1;
            assert!(asr_controller_core_first_overlap(core, 0.0, 1.0, &mut id));
            assert_eq!(id, 6);
            assert!(asr_controller_core_erase(core, id));
            assert!(asr_controller_core_first_overlap(core, 0.0, 1.0, &mut id));
            assert_eq!(id, 5);
            asr_controller_core_free(core);
        }
    }
}

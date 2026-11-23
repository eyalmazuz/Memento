//! Safe Rust wrapper around `whisper.cpp` inference, Silero VAD scanning, and
//! ggml GPU enumeration.

use std::cmp::max;
use std::ffi::CString;
use std::os::raw::{c_int, c_void};
use std::path::Path;
use std::ptr::NonNull;

use crate::whisper_sys as sys;

/// Default language code passed to `whisper.cpp` when none is specified.
pub const LANGUAGE_JAPANESE: &str = "ja";
/// Centisecond timestamp scale used by `whisper.cpp` segment timestamps.
pub const WHISPER_TIME_SCALE: i64 = 100;

/// Failure string when the input audio slice is empty.
pub const ERR_EMPTY_AUDIO: &str = "The extracted audio contains no samples.";

/// Result of one Whisper transcription attempt.
#[derive(Debug, Clone, PartialEq)]
pub enum EngineStatus {
    /// `whisper_full` returned 0.
    Success,
    /// `whisper_full` stopped because `should_abort` returned `true`.
    Canceled,
    /// Transcription failed with a diagnostic message.
    Failed { message: String, warn: bool },
}

/// Transcription parameters passed to `whisper.cpp`.
#[derive(Debug, Clone, Default)]
pub struct TranscribeOptions {
    /// Language code; empty selects [`LANGUAGE_JAPANESE`].
    pub language: String,
    /// Thread count; `<= 0` picks automatic parallelism capped at 4.
    pub threads: i32,
    pub best_of: i32,
    pub beam_size: i32,
    pub use_vad: bool,
    pub vad_model: String,
    pub suppress_regex: String,
}

/// Convert a whisper.cpp centisecond timestamp into seconds.
pub fn timestamp_to_seconds(timestamp: i64) -> f64 {
    timestamp as f64 / WHISPER_TIME_SCALE as f64
}

/// Convert a filesystem path to a NUL-terminated `CString`, preserving raw
/// non-UTF-8 bytes on Unix.
pub fn path_to_cstring(path: &Path) -> Result<CString, String> {
    #[cfg(unix)]
    let bytes = {
        use std::os::unix::ffi::OsStrExt;
        path.as_os_str().as_bytes()
    };
    #[cfg(not(unix))]
    let lossy = path.to_string_lossy();
    #[cfg(not(unix))]
    let bytes = lossy.as_bytes();

    CString::new(bytes).map_err(|_| {
        format!(
            "Model path '{}' contains an invalid NUL byte.",
            path.display()
        )
    })
}

/// Count discrete and integrated GPU devices registered with the linked ggml build.
pub fn gpu_device_count() -> usize {
    unsafe extern "C" {
        fn ggml_backend_dev_count() -> usize;
        fn ggml_backend_dev_get(index: usize) -> *mut c_void;
        fn ggml_backend_dev_type(device: *mut c_void) -> i32;
    }
    // SAFETY: ggml backend device registry functions are process-global read-only queries.
    unsafe {
        (0..ggml_backend_dev_count())
            .filter(|&index| {
                let device = ggml_backend_dev_get(index);
                !device.is_null() && matches!(ggml_backend_dev_type(device), 1 | 2)
            })
            .count()
    }
}

/// Owned `whisper_context` handle freed automatically on drop.
pub struct WhisperModel {
    ctx: NonNull<sys::whisper_context>,
}

// SAFETY: `WhisperModel` exclusively owns the underlying `whisper_context` and
// requires `&mut self` for inference, so moving it between threads is sound.
unsafe impl Send for WhisperModel {}

impl WhisperModel {
    /// Load a GGML Whisper model from `path`.
    pub fn load(
        path: &Path,
        use_gpu: bool,
        gpu_device: i32,
        flash_attention: bool,
    ) -> Result<Self, String> {
        let path_c = path_to_cstring(path)?;
        // SAFETY: `whisper_context_default_params` returns a plain C struct by value
        // and `path_c` is a valid NUL-terminated path string.
        let ptr = unsafe {
            let mut params = sys::whisper_context_default_params();
            params.use_gpu = use_gpu;
            params.gpu_device = gpu_device.max(0);
            params.flash_attn = flash_attention;
            sys::whisper_init_from_file_with_params(path_c.as_ptr(), params)
        };
        let ctx = NonNull::new(ptr).ok_or_else(|| {
            format!(
                "Whisper could not load model '{}'. Check that the model file is complete and that enough memory is available.",
                path.display(),
            )
        })?;
        Ok(Self { ctx })
    }

    /// Transcribe decoded 16 kHz mono `f32` PCM samples using safe Rust closures.
    pub fn transcribe_samples(
        &mut self,
        samples: &[f32],
        options: &TranscribeOptions,
        should_abort: &dyn Fn() -> bool,
        on_segment: &mut dyn FnMut(&str, i64, i64),
    ) -> EngineStatus {
        if samples.is_empty() {
            return EngineStatus::Failed {
                message: ERR_EMPTY_AUDIO.to_string(),
                warn: false,
            };
        }

        let Ok(n_samples) = c_int::try_from(samples.len()) else {
            return EngineStatus::Failed {
                message: "The extracted audio exceeds Whisper's maximum sample count.".to_string(),
                warn: true,
            };
        };

        struct AbortCtx<'a> {
            f: &'a dyn Fn() -> bool,
        }
        struct SegmentCtx<'a> {
            f: &'a mut dyn FnMut(&str, i64, i64),
        }

        unsafe extern "C" fn abort_trampoline(data: *mut c_void) -> bool {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                if data.is_null() {
                    return false;
                }
                let ctx = &*(data as *const AbortCtx<'_>);
                (ctx.f)()
            }))
            .unwrap_or(true)
        }

        unsafe extern "C" fn segment_trampoline(
            ctx: *mut sys::whisper_context,
            _state: *mut sys::whisper_state,
            n_new: c_int,
            data: *mut c_void,
        ) {
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                if ctx.is_null() || data.is_null() {
                    return;
                }
                let seg_ctx = &mut *(data as *mut SegmentCtx<'_>);
                let count = sys::whisper_full_n_segments(ctx);
                let start = max(0, count.saturating_sub(max(0, n_new)));
                for i in start..count {
                    let text = sys::whisper_full_get_segment_text(ctx, i);
                    let bytes = if text.is_null() {
                        &[][..]
                    } else {
                        std::ffi::CStr::from_ptr(text).to_bytes()
                    };
                    let t0 = sys::whisper_full_get_segment_t0(ctx, i);
                    let t1 = sys::whisper_full_get_segment_t1(ctx, i);
                    let decoded = String::from_utf8_lossy(bytes);
                    (seg_ctx.f)(decoded.as_ref(), t0, t1);
                }
            }));
        }

        let strategy = if options.beam_size > 1 {
            sys::whisper_sampling_strategy_WHISPER_SAMPLING_BEAM_SEARCH
        } else {
            sys::whisper_sampling_strategy_WHISPER_SAMPLING_GREEDY
        };

        let language = if options.language.is_empty() {
            LANGUAGE_JAPANESE
        } else {
            options.language.as_str()
        };
        let language_c = c_string_from_bytes(language.as_bytes());
        let vad_model_c = options
            .use_vad
            .then(|| c_string_from_bytes(options.vad_model.as_bytes()));
        let suppress_regex_c = validated_suppress_regex(&options.suppress_regex);

        let abort_ctx = AbortCtx { f: should_abort };
        let mut seg_ctx = SegmentCtx { f: on_segment };

        // SAFETY: `self.ctx` is a valid exclusively borrowed `whisper_context`,
        // all `CString` buffers and callback contexts live for the entire
        // synchronous `whisper_full` call, and `samples` has length `n_samples`.
        let ret = unsafe {
            let mut params = sys::whisper_full_default_params(strategy);
            params.n_threads = resolve_thread_count(options.threads);
            params.language = language_c.as_ptr();
            params.print_progress = false;
            params.print_realtime = false;
            params.print_special = false;
            params.print_timestamps = false;
            params.translate = false;
            params.greedy.best_of = max(1, options.best_of);
            params.beam_search.beam_size = max(1, options.beam_size);
            params.new_segment_callback = Some(segment_trampoline);
            params.new_segment_callback_user_data = (&mut seg_ctx as *mut SegmentCtx<'_>).cast();
            params.abort_callback = Some(abort_trampoline);
            params.abort_callback_user_data =
                (&abort_ctx as *const AbortCtx<'_>).cast_mut().cast();

            if let Some(vad_model) = &vad_model_c {
                params.vad = true;
                params.vad_model_path = vad_model.as_ptr();
            }
            if let Some(suppress_regex) = &suppress_regex_c {
                params.suppress_regex = suppress_regex.as_ptr();
            }

            sys::whisper_full(self.ctx.as_ptr(), params, samples.as_ptr(), n_samples)
        };

        if ret == 0 {
            return EngineStatus::Success;
        }
        if should_abort() {
            return EngineStatus::Canceled;
        }
        EngineStatus::Failed {
            message: format!("whisper_full returned error code {ret}."),
            warn: true,
        }
    }
}

impl Drop for WhisperModel {
    fn drop(&mut self) {
        // SAFETY: `self.ctx` was created by `whisper_init_from_file_with_params`
        // and is uniquely owned by `self`.
        unsafe { sys::whisper_free(self.ctx.as_ptr()) };
    }
}

/// Owned `whisper_vad_context` handle freed automatically on drop.
pub struct WhisperVadContext {
    ctx: NonNull<sys::whisper_vad_context>,
}

// SAFETY: `WhisperVadContext` exclusively owns the underlying VAD context and
// requires `&mut self` for all operations.
unsafe impl Send for WhisperVadContext {}

impl WhisperVadContext {
    /// Load a Silero VAD GGML model from `path`.
    pub fn load(path: &Path) -> Result<Self, String> {
        let path_c = path_to_cstring(path)?;
        // SAFETY: `path_c` is NUL-terminated and default params are plain C data.
        let ptr = unsafe {
            let params = sys::whisper_vad_default_context_params();
            sys::whisper_vad_init_from_file_with_params(path_c.as_ptr(), params)
        };
        let ctx = NonNull::new(ptr)
            .ok_or_else(|| format!("Whisper could not load VAD model '{}'", path.display()))?;
        Ok(Self { ctx })
    }

    /// Reset internal LSTM state (call on seek, gap, or track switch).
    pub fn reset_state(&mut self) {
        // SAFETY: `self.ctx` is a valid exclusively borrowed VAD context.
        unsafe { sys::whisper_vad_reset_state(self.ctx.as_ptr()) };
    }

    /// Scan `samples` without resetting LSTM state and report whether any speech
    /// segments were detected.
    pub fn detect_speech_no_reset(&mut self, samples: &[f32]) -> Result<bool, String> {
        if samples.is_empty() {
            return Ok(false);
        }
        let count = i32::try_from(samples.len())
            .map_err(|_| "ASR VAD window contains too many audio samples".to_owned())?;
        // SAFETY: `self.ctx` is valid, `samples` has `count` elements, and the
        // returned segments handle is freed before returning.
        unsafe {
            if !sys::whisper_vad_detect_speech_no_reset(self.ctx.as_ptr(), samples.as_ptr(), count)
            {
                return Err("Whisper VAD could not scan the audio window".into());
            }
            let segments = sys::whisper_vad_segments_from_probs(
                self.ctx.as_ptr(),
                sys::whisper_vad_default_params(),
            );
            if segments.is_null() {
                return Err("Whisper VAD could not identify speech segments".into());
            }
            let n_segments = sys::whisper_vad_segments_n_segments(segments);
            sys::whisper_vad_free_segments(segments);
            Ok(n_segments > 0)
        }
    }
}

impl Drop for WhisperVadContext {
    fn drop(&mut self) {
        // SAFETY: `self.ctx` was created by `whisper_vad_init_from_file_with_params`
        // and is uniquely owned by `self`.
        unsafe { sys::whisper_vad_free(self.ctx.as_ptr()) };
    }
}

/// Truncate bytes at the first embedded NUL to build a `CString`.
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
                if i + 1 < bytes.len() && bytes[i + 1] == b'?' && bytes.get(i + 2) != Some(&b':') {
                    return true;
                }
            }
            _ => {}
        }
        i += 1;
    }
    false
}

/// Resolve the whisper.cpp thread count: explicit positive counts are used as
/// requested, while `<= 0` ("auto") defaults to `available_parallelism().min(4)`.
fn resolve_thread_count(threads: i32) -> i32 {
    if threads > 0 {
        threads
    } else {
        std::thread::available_parallelism()
            .map(|n| n.get().min(4) as i32)
            .unwrap_or(4)
    }
}

/// Validate the optional Whisper suppression pattern so Rust-only syntax cannot
/// trigger `std::regex_error` across the `whisper.cpp` FFI boundary.
fn validated_suppress_regex(pattern: &str) -> Option<CString> {
    if pattern.trim().is_empty() || has_unsupported_cpp_regex_syntax(pattern) {
        return None;
    }
    regex::Regex::new(pattern)
        .ok()
        .map(|_| c_string_from_bytes(pattern.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamp_to_seconds_divides_by_centiseconds() {
        assert_eq!(timestamp_to_seconds(0), 0.0);
        assert_eq!(timestamp_to_seconds(150), 1.5);
        assert_eq!(timestamp_to_seconds(-7), -7.0 / 100.0);
    }

    #[test]
    fn c_string_truncates_at_embedded_nul() {
        assert_eq!(c_string_from_bytes(b"ja").to_bytes(), b"ja");
        assert_eq!(c_string_from_bytes(b"a\0b").to_bytes(), b"a");
        assert_eq!(c_string_from_bytes(b"\0").to_bytes(), b"");
    }

    #[test]
    fn validator_rejects_rust_only_regex_syntax() {
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
            assert!(regex::Regex::new(pattern).is_ok());
            assert!(
                validated_suppress_regex(pattern).is_none(),
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
            let out = validated_suppress_regex(pattern)
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
}

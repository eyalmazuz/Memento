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

//! Port of `src/asr/onnx/onnxsession.{h,cpp}` — the C++ `OnnxSession`
//! inference engine.
//!
//! Composition mirrors the C++ implementation statement by statement:
//! - `create()` is the synchronous constructor part (config discovery, model
//!   architecture detection, tokenizer load) and always yields a session
//!   handle, exactly like the C++ object exists even when loading failed.
//! - `initialize()` is `initializeSessions()` (same threading contract: the
//!   C++ shim runs it on a `QtConcurrent` worker and `transcribe` waits for
//!   completion like `QFuture::waitForFinished()`).
//! - `transcribe()` is `transcribeSync()` plus `transcribeEncoderDecoder()` /
//!   `transcribeCtc()`, including every abort-check position, error string,
//!   and numerical operation order.
//!
//! ONNX Runtime access is a hand-written FFI binding over the same
//! `onnxruntime_c_api.h` 1.22.0 the C++ build links (see `ort_sys.rs` and
//! `docs/rust-port-plan.md` for the single-library-instance rationale). The
//! `OrtGetApiBase` symbol resolves against the `libonnxruntime` shared library
//! already linked by the C++ `asrtranscriber` target, so the process contains
//! exactly one ONNX Runtime.
//!
//! Divergences from the C++ behavior (all unreachable in shipped model
//! configurations, listed for review):
//! - `qWarning` side effects from the C++ code paths are not reproduced
//!   (the shim keeps the log calls that C++ performs; the engine itself is
//!   silent, matching the established `memento-asr` bridge precedent).
//! - Where the C++ code reads out-of-bounds (`std::vector::at` aside, raw
//!   pointer arithmetic on malformed shapes), this port returns an error
//!   instead of invoking undefined behavior.
//! - `Ort::Value::GetTensorTypeAndShapeInfo()` / `Session::GetInputTypeInfo`
//!   wrappers in the C++ API leak the returned type-info object; this port
//!   releases them.

use std::ffi::{CStr, CString};
use std::marker::PhantomData;
use std::os::raw::{c_char, c_int, c_void};
use std::path::Path;
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::Duration;

use memento_asr::audio::melfrontend::{AttentionMaskKind, MelFrontend};
use memento_asr::audio::wavreader;
use memento_asr::onnx::onnxmodelconfig::{OnnxAsrArchitecture, OnnxModelConfig};
use memento_asr::onnx::onnxtokenizer::OnnxTokenizer;

use crate::ort_sys::*;

/// C++ `ORT_LOGGING_LEVEL_WARNING`.
const ORT_LOGGING_LEVEL_WARNING: c_int = 2;
/// C++ `GraphOptimizationLevel::ORT_ENABLE_ALL`.
const ORT_ENABLE_ALL: c_int = 99;
/// C++ `ONNX_TENSOR_ELEMENT_DATA_TYPE_FLOAT`.
const ONNX_TENSOR_ELEMENT_DATA_TYPE_FLOAT: c_int = 1;
/// C++ `ONNX_TENSOR_ELEMENT_DATA_TYPE_INT32`.
const ONNX_TENSOR_ELEMENT_DATA_TYPE_INT32: c_int = 6;
/// C++ `ONNX_TENSOR_ELEMENT_DATA_TYPE_INT64`.
const ONNX_TENSOR_ELEMENT_DATA_TYPE_INT64: c_int = 7;
/// C++ `ONNX_TENSOR_ELEMENT_DATA_TYPE_BOOL`.
const ONNX_TENSOR_ELEMENT_DATA_TYPE_BOOL: c_int = 9;
/// C++ `ONNX_TENSOR_ELEMENT_DATA_TYPE_FLOAT16`.
const ONNX_TENSOR_ELEMENT_DATA_TYPE_FLOAT16: c_int = 10;
/// C++ `OrtArenaAllocator`.
const ORT_ARENA_ALLOCATOR: c_int = 1;
/// C++ `OrtMemTypeDefault`.
const ORT_MEM_TYPE_DEFAULT: c_int = 0;

/// Mirror of `AsrTranscriptionResult::Success`.
pub const RESULT_SUCCESS: i32 = 0;
/// Mirror of `AsrTranscriptionResult::Failed`.
pub const RESULT_FAILED: i32 = 1;
/// Mirror of `AsrTranscriptionResult::Canceled`.
pub const RESULT_CANCELED: i32 = 2;

/// Default failure message used by the C++ code when no specific reason is
/// available (WAV path and not-loaded path share the literal per call site).
const WAV_READ_FAILED: &str = "Could not read extracted WAV audio.";
const NOT_LOADED: &str = "ONNX ASR model is not loaded.";
const UNSUPPORTED_ARCHITECTURE: &str = "Unsupported ONNX ASR model architecture.";
const MAX_ONNX_N_FFT: i32 = 65_536;

fn validate_frontend_config(
    frontend: &memento_asr::audio::melfrontend::MelFrontendConfig,
) -> Result<(), String> {
    if frontend.n_fft <= 0 || frontend.n_fft > MAX_ONNX_N_FFT {
        return Err(format!("invalid frontend n_fft ({})", frontend.n_fft));
    }
    if frontend.hop_length <= 0 || frontend.n_mels <= 0 || frontend.sample_rate <= 0 {
        return Err(format!(
            "invalid frontend parameters (sample_rate={}, hop_length={}, n_mels={})",
            frontend.sample_rate, frontend.hop_length, frontend.n_mels
        ));
    }
    Ok(())
}

/// Pure-Rust callbacks for ONNX ASR transcription progress and cancellation.
pub struct Callbacks<'a> {
    /// Polled before each stage and decoder step; returning `true` cancels.
    pub should_abort: &'a dyn Fn() -> bool,
    /// Invoked for each decoded segment `(start_sec, end_sec, text)`.
    /// Returning `false` requests cancellation.
    pub on_segment: &'a mut dyn FnMut(f64, f64, &str) -> bool,
    /// Invoked with a diagnostic message when transcription fails.
    pub on_failure: &'a mut dyn FnMut(&str),
}

impl<'a> Callbacks<'a> {
    pub fn new(
        should_abort: &'a dyn Fn() -> bool,
        on_segment: &'a mut dyn FnMut(f64, f64, &str) -> bool,
        on_failure: &'a mut dyn FnMut(&str),
    ) -> Self {
        Self {
            should_abort,
            on_segment,
            on_failure,
        }
    }
}

/// C++ `OnnxSessionOptions` (minus `modelPath`, which is passed to `create`).
pub struct Options {
    pub use_gpu: bool,
    pub gpu_device: i32,
    pub threads: i32,
    pub punctuation: bool,
}

/// Config + tokenizer staged by [`Engine::create`] and consumed by
/// [`Engine::initialize`] (the C++ constructor/`initializeSessions` split).
struct Staged {
    config: OnnxModelConfig,
    tokenizer: OnnxTokenizer,
}

/// Init-completion state shared between the initializing thread and
/// `transcribe` waiters (the `QFuture::waitForFinished()` analog).
struct InitState {
    done: bool,
    load_error: String,
}

/// Immutable list of NUL-terminated ORT tensor names paired with their stable
/// `*const c_char` pointer array for zero-allocation `OrtApi::Run`.
struct CachedOrtNames {
    _owned: Box<[CString]>,
    ptrs: Box<[*const c_char]>,
}

// SAFETY: `ptrs` points exclusively into the heap buffers owned by `_owned`,
// which are never mutated or reallocated after construction.
unsafe impl Send for CachedOrtNames {}
unsafe impl Sync for CachedOrtNames {}

impl CachedOrtNames {
    fn empty() -> Self {
        Self {
            _owned: Box::new([]),
            ptrs: Box::new([]),
        }
    }

    fn new(names: Vec<CString>) -> Self {
        let owned = names.into_boxed_slice();
        let ptrs = owned
            .iter()
            .map(|name| name.as_ptr())
            .collect::<Vec<_>>()
            .into_boxed_slice();
        Self {
            _owned: owned,
            ptrs,
        }
    }

    fn as_ptrs(&self) -> &[*const c_char] {
        &self.ptrs
    }
}

/// Fully initialized engine state; becomes immutable once stored (mirrors the
/// C++ members after `initializeSessions()` completes).
struct Initialized {
    config: OnnxModelConfig,
    tokenizer: OnnxTokenizer,
    sessions: Sessions,
    encoder_mask_is_bool: bool,
    ctc_mask_is_bool: bool,
    num_decoder_layers: i32,
    num_decoder_heads: i64,
    num_decoder_head_dim: i64,
    frontend: MelFrontend,
    encoder_input_names: CachedOrtNames,
    encoder_output_names: CachedOrtNames,
    decoder_input_names: CachedOrtNames,
    decoder_output_names: CachedOrtNames,
    ctc_input_names: CachedOrtNames,
    ctc_output_names: CachedOrtNames,
}

/// Owned ONNX Runtime session handles (one architecture is populated).
struct Sessions {
    encoder: Option<SessionHandle>,
    decoder: Option<SessionHandle>,
    ctc: Option<SessionHandle>,
}

/// Rust port of the C++ `OnnxSession` object.
pub struct Engine {
    /// C++ `m_options`.
    options: Options,
    /// C++ `m_configValid` (immutable after construction).
    config_valid: bool,
    /// C++ reads `m_config.frontend.sampleRate` in `transcribeSync` before
    /// falling into the init wait; the field is never mutated by
    /// `initializeSessions`, so it is captured at construction.
    sample_rate: i32,
    /// C++ ctor-loaded config and tokenizer, consumed by `initialize()`.
    staged: Mutex<Option<Staged>>,
    /// C++ `m_loaded` / `m_loadError` plus the init-completion flag.
    init_state: Mutex<InitState>,
    init_done: Condvar,
    /// Set when `initialize()` succeeded (equivalent of `m_loaded` in the C++
    /// code, which becomes true exactly when the sessions and frontend exist).
    initialized: OnceLock<Initialized>,
    /// C++ `std::scoped_lock lock(m_mutex)` around the architecture dispatch.
    inference: Mutex<()>,
}

impl Engine {
    /// C++ `OnnxSession::OnnxSession(OnnxSessionOptions)` minus the async
    /// session creation (started separately through `initialize`).
    pub fn create(model_path: &Path, options: Options) -> Arc<Engine> {
        let (config, config_error) = OnnxModelConfig::load_from_path(model_path);
        let mut tokenizer = OnnxTokenizer::new();
        let mut config_valid = false;
        let mut load_error = String::new();
        if !config.is_valid() {
            load_error = if config_error.is_empty() {
                format!("Invalid ONNX ASR model directory: {}", model_path.display())
            } else {
                config_error
            };
        } else if let Err(tokenizer_error) = tokenizer.load(&config.tokenizer_json_path) {
            load_error = tokenizer_error;
        } else {
            config_valid = true;
        }
        let sample_rate = config.frontend.sample_rate;
        Arc::new(Engine {
            options,
            config_valid,
            sample_rate,
            staged: Mutex::new(Some(Staged { config, tokenizer })),
            init_state: Mutex::new(InitState {
                done: false,
                load_error,
            }),
            init_done: Condvar::new(),
            initialized: OnceLock::new(),
            inference: Mutex::new(()),
        })
    }

    /// C++ `m_configValid` (`isLoaded()`).
    pub fn is_loaded(&self) -> bool {
        self.config_valid
    }

    /// C++ `loadError()`: construction-stage error before initialization
    /// completes, initialization error after.
    pub fn load_error(&self) -> String {
        let state = self
            .init_state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.load_error.clone()
    }

    /// C++ `OnnxSession::initializeSessions()` run synchronously; the C++
    /// object invokes this on a background future and `transcribe` waits for
    /// the completion flag, so this function may run concurrently with
    /// `transcribe` calls that are blocked on the wait.
    pub fn initialize(&self, ort_api_version: u32) {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.run_initialize(ort_api_version)
        }));
        let mut state = self
            .init_state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match result {
            Ok(Ok(initialized)) => {
                // Store before publishing completion so waiters never observe
                // "done" without an initialized engine on the success path.
                let _ = self.initialized.set(initialized);
            }
            Ok(Err(message)) => {
                state.load_error = format!("ONNX Runtime initialization failed: {message}");
            }
            Err(payload) => {
                state.load_error = format!(
                    "ONNX Runtime initialization failed: {}",
                    panic_payload_message(payload.as_ref())
                );
            }
        }
        state.done = true;
        drop(state);
        self.init_done.notify_all();
    }

    /// C++ `transcribeSync(audioPath, options)`.
    pub fn transcribe(&self, audio_path: &Path, language: &str, callbacks: &mut Callbacks<'_>) -> i32 {
        if (callbacks.should_abort)() {
            return RESULT_CANCELED;
        }

        let samples = match wavreader::read_pcm16_wav_path(audio_path, self.sample_rate as u32) {
            Ok(samples) if !samples.is_empty() => samples,
            Ok(_) => {
                (callbacks.on_failure)(WAV_READ_FAILED);
                return RESULT_FAILED;
            }
            Err(message) => {
                let message = if message.is_empty() {
                    WAV_READ_FAILED.to_string()
                } else {
                    message
                };
                (callbacks.on_failure)(&message);
                return RESULT_FAILED;
            }
        };

        match self.transcribe_samples(
            &samples,
            language,
            callbacks.should_abort,
            callbacks.on_segment,
        ) {
            Ok(true) => RESULT_SUCCESS,
            Ok(false) => RESULT_CANCELED,
            Err(message) => {
                (callbacks.on_failure)(&message);
                RESULT_FAILED
            }
        }
    }

    /// Transcribe already-decoded mono `f32` PCM samples in memory.
    ///
    /// Returns `Ok(true)` on completion, `Ok(false)` on cancellation (either via
    /// `should_abort` or `on_segment` returning `false`), or `Err(message)` on
    /// configuration/inference failure.
    pub fn transcribe_samples(
        &self,
        samples: &[f32],
        language: &str,
        should_abort: &dyn Fn() -> bool,
        on_segment: &mut dyn FnMut(f64, f64, &str) -> bool,
    ) -> Result<bool, String> {
        if should_abort() {
            return Ok(false);
        }
        if samples.is_empty() {
            return Err(WAV_READ_FAILED.to_string());
        }

        // C++ `if (m_initFuture.isValid()) m_initFuture.waitForFinished();` —
        // the future exists exactly when the constructor succeeded. Poll
        // `should_abort()` via `wait_timeout` so cancellation unblocks waits
        // on slow initialization.
        if self.config_valid {
            const INIT_WAIT_POLL: Duration = Duration::from_millis(25);
            let mut state = self
                .init_state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            while !state.done {
                drop(state);
                if should_abort() {
                    return Ok(false);
                }
                state = self
                    .init_state
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                if state.done {
                    break;
                }
                let (next_state, _) = self
                    .init_done
                    .wait_timeout(state, INIT_WAIT_POLL)
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                state = next_state;
            }
        }

        if should_abort() {
            return Ok(false);
        }

        let Some(initialized) = self.initialized.get() else {
            // C++ `if (!m_loaded || !m_frontend)` with the same fallback.
            let state = self
                .init_state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let message = if state.load_error.is_empty() {
                NOT_LOADED.to_string()
            } else {
                state.load_error.clone()
            };
            return Err(message);
        };

        if should_abort() {
            return Ok(false);
        }

        // C++ `std::scoped_lock lock(m_mutex);` — serializes inference.
        let _guard = self
            .inference
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        let result = match initialized.config.architecture {
            OnnxAsrArchitecture::EncoderDecoder => self.transcribe_encoder_decoder(
                initialized,
                samples,
                language,
                should_abort,
                on_segment,
            ),
            OnnxAsrArchitecture::Ctc => {
                self.transcribe_ctc(initialized, samples, should_abort, on_segment)
            }
            OnnxAsrArchitecture::Unknown => {
                return Err(UNSUPPORTED_ARCHITECTURE.to_string());
            }
        };

        result.map_err(|message| format!("ONNX inference error: {message}"))
    }

    /// C++ `OnnxSession::initializeSessions()`.
    fn run_initialize(&self, ort_api_version: u32) -> Result<Initialized, String> {
        let api = get_api(ort_api_version)?;
        let env = get_env(api)?;

        let session_options = SessionOptionsHandle::create()?;
        let num_threads = if self.options.threads > 0 {
            self.options.threads
        } else {
            0
        };
        session_options.set_intra_op_num_threads(num_threads)?;
        session_options.set_inter_op_num_threads(1)?;
        session_options.set_graph_optimization_level(ORT_ENABLE_ALL)?;

        if self.options.use_gpu {
            let providers = available_providers(api)?;
            if providers
                .iter()
                .any(|provider| provider == "CUDAExecutionProvider")
            {
                let mut cuda_options = OrtCUDAProviderOptions::default();
                cuda_options.device_id = self.options.gpu_device.max(0);
                cuda_options.arena_extend_strategy = ORT_ARENA_EXTEND_STRATEGY_SAME_AS_REQUESTED;
                // C++ wraps the append in try/catch and downgrades a failure
                // to a qWarning; the warning side effect is not reproduced.
                let _ = session_options.append_cuda(&cuda_options);
            } else {
                // C++ qWarning("...built without CUDA..."); not reproduced.
            }
        }

        let allocator = AllocatorHandle::default_options()?;

        let staged = {
            let mut staged = self
                .staged
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            staged
                .take()
                .ok_or_else(|| "ONNX ASR initialization ran twice".to_string())?
        };
        let mut config = staged.config;
        let tokenizer = staged.tokenizer;

        let mut sessions = Sessions {
            encoder: None,
            decoder: None,
            ctc: None,
        };
        let mut encoder_mask_is_bool = false;
        let mut ctc_mask_is_bool = false;
        let mut num_decoder_layers: i32 = 4;
        let mut num_decoder_heads: i64 = 8;
        let mut num_decoder_head_dim: i64 = 128;
        let mut encoder_input_names = CachedOrtNames::empty();
        let mut encoder_output_names = CachedOrtNames::empty();
        let mut decoder_input_names = CachedOrtNames::empty();
        let mut decoder_output_names = CachedOrtNames::empty();
        let mut ctc_input_names = CachedOrtNames::empty();
        let mut ctc_output_names = CachedOrtNames::empty();

        match config.architecture {
            OnnxAsrArchitecture::EncoderDecoder => {
                let encoder = SessionHandle::create(
                    env,
                    &OrtPath::from_path(&config.encoder_onnx_path)?,
                    &session_options,
                )?;
                let decoder = SessionHandle::create(
                    env,
                    &OrtPath::from_path(&config.decoder_onnx_path)?,
                    &session_options,
                )?;

                // Inspect encoder inputs for mel bins and attention_mask element type.
                let encoder_input_count = encoder.input_count()?;
                for index in 0..encoder_input_count {
                    let name = encoder.input_name(index, &allocator)?;
                    let type_info = encoder.input_type_info(index)?;
                    let shape_info = type_info.tensor_shape_info()?;
                    let shape = shape_info.dimensions()?;
                    if name == "input_features" && shape.len() == 3 && shape[2] > 0 {
                        config.frontend.n_mels = shape[2] as i32;
                    } else if name == "attention_mask" {
                        encoder_mask_is_bool =
                            shape_info.element_type()? == ONNX_TENSOR_ELEMENT_DATA_TYPE_BOOL;
                    }
                }

                // Inspect decoder inputs to determine layer count and KV cache
                // dimensions dynamically.
                let mut max_layer_index: i32 = -1;
                let decoder_input_count = decoder.input_count()?;
                for index in 0..decoder_input_count {
                    let name = decoder.input_name(index, &allocator)?;
                    if let Some(suffix) = name.strip_prefix("past_key.") {
                        let layer_index = atoi_saturating(suffix);
                        max_layer_index = max_layer_index.max(layer_index);
                        let type_info = decoder.input_type_info(index)?;
                        let shape = type_info.tensor_shape_info()?.dimensions()?;
                        if shape.len() == 4 {
                            if shape[1] > 0 {
                                num_decoder_heads = shape[1];
                            }
                            if shape[3] > 0 {
                                num_decoder_head_dim = shape[3];
                            }
                        }
                    }
                }
                if max_layer_index >= 0 {
                    num_decoder_layers = max_layer_index + 1;
                }

                // Pre-build NUL-terminated input/output name tables once per session.
                encoder_input_names = CachedOrtNames::new(vec![
                    to_cstring("input_features".to_string())?,
                    to_cstring("attention_mask".to_string())?,
                ]);

                let mut enc_out = Vec::with_capacity(1 + 2 * num_decoder_layers as usize);
                enc_out.push(to_cstring("encoder_attention_mask".to_string())?);
                for layer in 0..num_decoder_layers {
                    enc_out.push(to_cstring(format!("cross_key.{layer}"))?);
                    enc_out.push(to_cstring(format!("cross_value.{layer}"))?);
                }
                encoder_output_names = CachedOrtNames::new(enc_out);

                let mut dec_in = Vec::with_capacity(2 + 4 * num_decoder_layers as usize);
                dec_in.push(to_cstring("input_ids".to_string())?);
                dec_in.push(to_cstring("encoder_attention_mask".to_string())?);
                for layer in 0..num_decoder_layers {
                    dec_in.push(to_cstring(format!("cross_key.{layer}"))?);
                    dec_in.push(to_cstring(format!("cross_value.{layer}"))?);
                }
                for layer in 0..num_decoder_layers {
                    dec_in.push(to_cstring(format!("past_key.{layer}"))?);
                    dec_in.push(to_cstring(format!("past_value.{layer}"))?);
                }
                decoder_input_names = CachedOrtNames::new(dec_in);

                let mut dec_out = Vec::with_capacity(1 + 2 * num_decoder_layers as usize);
                dec_out.push(to_cstring("logits".to_string())?);
                for layer in 0..num_decoder_layers {
                    dec_out.push(to_cstring(format!("present_key.{layer}"))?);
                    dec_out.push(to_cstring(format!("present_value.{layer}"))?);
                }
                decoder_output_names = CachedOrtNames::new(dec_out);

                sessions.encoder = Some(encoder);
                sessions.decoder = Some(decoder);
            }
            OnnxAsrArchitecture::Ctc => {
                let ctc = SessionHandle::create(
                    env,
                    &OrtPath::from_path(&config.ctc_onnx_path)?,
                    &session_options,
                )?;

                let ctc_input_count = ctc.input_count()?;
                for index in 0..ctc_input_count {
                    let name = ctc.input_name(index, &allocator)?;
                    let type_info = ctc.input_type_info(index)?;
                    let shape_info = type_info.tensor_shape_info()?;
                    let shape = shape_info.dimensions()?;
                    if name == "input_features" && shape.len() == 3 && shape[2] > 0 {
                        config.frontend.n_mels = shape[2] as i32;
                    } else if name == "attention_mask" {
                        ctc_mask_is_bool =
                            shape_info.element_type()? == ONNX_TENSOR_ELEMENT_DATA_TYPE_BOOL;
                    }
                }

                ctc_input_names = CachedOrtNames::new(vec![
                    to_cstring("input_features".to_string())?,
                    to_cstring("attention_mask".to_string())?,
                ]);
                let output_count = ctc.output_count()?;
                let mut ctc_out: Vec<CString> = Vec::with_capacity(output_count);
                for index in 0..output_count {
                    ctc_out.push(to_cstring(ctc.output_name(index, &allocator)?)?);
                }
                ctc_output_names = CachedOrtNames::new(ctc_out);

                sessions.ctc = Some(ctc);
            }
            OnnxAsrArchitecture::Unknown => {
                // C++ falls through to frontend creation with no sessions;
                // transcribe then reports "Unsupported ONNX ASR model
                // architecture." exactly like this port.
            }
        }

        // Validate frontend dimensions (allowing arbitrary positive n_fft such as
        // Whisper's n_fft = 400 while rejecting zero/negative/unbounded configs).
        validate_frontend_config(&config.frontend)?;

        // C++ constructs the frontend last, from the (possibly nMels-updated)
        // configuration.
        let frontend = MelFrontend::new(config.frontend);

        Ok(Initialized {
            config,
            tokenizer,
            sessions,
            encoder_mask_is_bool,
            ctc_mask_is_bool,
            num_decoder_layers,
            num_decoder_heads,
            num_decoder_head_dim,
            frontend,
            encoder_input_names,
            encoder_output_names,
            decoder_input_names,
            decoder_output_names,
            ctc_input_names,
            ctc_output_names,
        })
    }

    /// C++ `OnnxSession::transcribeEncoderDecoder(samples, options)`.
    fn transcribe_encoder_decoder(
        &self,
        initialized: &Initialized,
        samples: &[f32],
        language: &str,
        should_abort: &dyn Fn() -> bool,
        on_segment: &mut dyn FnMut(f64, f64, &str) -> bool,
    ) -> Result<bool, String> {
        // Segment the audio window into speech utterances so untimed seq2seq
        // models emit accurately timed subtitle cues.
        let utterances = MelFrontend::segment_speech_utterances(
            samples,
            initialized.config.frontend.sample_rate,
            10.0,
        );
        if utterances.is_empty() {
            // Silence clip — nothing to emit, return Success.
            return Ok(true);
        }

        let mem_info = MemoryInfoHandle::create_cpu()?;

        let prompt_ids = initialized
            .tokenizer
            .build_cohere_asr_prompt_with(language, self.options.punctuation);

        let (Some(encoder), Some(decoder)) =
            (&initialized.sessions.encoder, &initialized.sessions.decoder)
        else {
            return Err("ONNX ASR sessions are not initialized.".to_string());
        };

        let encoder_input_ptrs = initialized.encoder_input_names.as_ptrs();
        let encoder_output_ptrs = initialized.encoder_output_names.as_ptrs();
        let decoder_input_ptrs = initialized.decoder_input_names.as_ptrs();
        let decoder_output_ptrs = initialized.decoder_output_names.as_ptrs();
        let mask_kind = if initialized.encoder_mask_is_bool {
            AttentionMaskKind::Bool
        } else {
            AttentionMaskKind::Int64
        };

        for utterance in &utterances {
            if should_abort() {
                return Ok(false);
            }

            let utterance_samples = &samples[utterance.start_sample..utterance.end_sample];
            let mel = initialized
                .frontend
                .extract_with_mask(utterance_samples, mask_kind);
            if mel.num_frames <= 0 || mel.valid_frames <= 0 {
                continue;
            }

            let feature_shape = [1i64, mel.num_frames, mel.n_mels];
            let mask_shape = [1i64, mel.num_frames];

            let mut encoder_inputs: Vec<ValueHandle> = Vec::with_capacity(2);
            encoder_inputs.push(create_tensor(
                &mem_info,
                mel.features.as_ptr() as *mut c_void,
                mel.features.len() * std::mem::size_of::<f32>(),
                &feature_shape,
                ONNX_TENSOR_ELEMENT_DATA_TYPE_FLOAT,
            )?);
            if initialized.encoder_mask_is_bool {
                encoder_inputs.push(create_tensor(
                    &mem_info,
                    mel.attention_mask_bool.as_ptr() as *mut c_void,
                    mel.attention_mask_bool.len(),
                    &mask_shape,
                    ONNX_TENSOR_ELEMENT_DATA_TYPE_BOOL,
                )?);
            } else {
                encoder_inputs.push(create_tensor(
                    &mem_info,
                    mel.attention_mask_int64.as_ptr() as *mut c_void,
                    mel.attention_mask_int64.len() * std::mem::size_of::<i64>(),
                    &mask_shape,
                    ONNX_TENSOR_ELEMENT_DATA_TYPE_INT64,
                )?);
            }
            let encoder_input_ptrs_slice: Vec<*const OrtValue> =
                encoder_inputs.iter().map(ValueHandle::as_ptr).collect();

            let encoder_outputs = run(
                encoder,
                encoder_input_ptrs,
                &encoder_input_ptrs_slice,
                encoder_output_ptrs,
            )?;

            if should_abort() {
                return Ok(false);
            }

            // Initialize the empty self-attention KV cache [1, heads, 0, headDim]
            // using a non-null, properly aligned dummy pointer that outlives `past_kv`.
            let mut empty_kv_dummy: [f32; 1] = [0.0];
            let empty_kv_shape = [
                1i64,
                initialized.num_decoder_heads,
                0,
                initialized.num_decoder_head_dim,
            ];
            let mut past_kv: Vec<ValueHandle> =
                Vec::with_capacity(2 * initialized.num_decoder_layers as usize);
            for _ in 0..2 * initialized.num_decoder_layers {
                past_kv.push(create_tensor(
                    &mem_info,
                    empty_kv_dummy.as_mut_ptr() as *mut c_void,
                    0,
                    &empty_kv_shape,
                    ONNX_TENSOR_ELEMENT_DATA_TYPE_FLOAT,
                )?);
            }

            let utterance_duration = (utterance.end_sec - utterance.start_sec).max(0.1);
            let max_new_tokens = i32::wrapping_add(
                16,
                double_to_i32_x86(
                    10.0 * utterance_duration.min(initialized.config.max_audio_clip_seconds),
                ),
            );
            if max_new_tokens < 0 {
                // C++ `generatedTokens.reserve(static_cast<size_t>(maxNewTokens))`
                // throws std::length_error for a negative count; the exception
                // text is reproduced so the failure reason stays identical.
                return Err("cannot create std::vector larger than max_size()".to_string());
            }

            let mut step_ids: Vec<i64> = prompt_ids.clone();
            let mut generated_tokens: Vec<i64> = Vec::with_capacity(max_new_tokens as usize);
            let mut decoder_input_ptrs_slice: Vec<*const OrtValue> =
                Vec::with_capacity(1 + encoder_outputs.len() + past_kv.len());

            for _step in 0..max_new_tokens {
                if should_abort() {
                    return Ok(false);
                }

                let ids_shape = [1i64, step_ids.len() as i64];
                let input_ids_tensor = create_tensor(
                    &mem_info,
                    step_ids.as_ptr() as *mut c_void,
                    step_ids.len() * std::mem::size_of::<i64>(),
                    &ids_shape,
                    ONNX_TENSOR_ELEMENT_DATA_TYPE_INT64,
                )?;

                // Pass encoder_outputs raw `*const OrtValue` pointers directly
                // across autoregressive steps instead of recreating tensor views.
                decoder_input_ptrs_slice.clear();
                decoder_input_ptrs_slice.push(input_ids_tensor.as_ptr());
                decoder_input_ptrs_slice.extend(encoder_outputs.iter().map(ValueHandle::as_ptr));
                decoder_input_ptrs_slice.extend(past_kv.iter().map(ValueHandle::as_ptr));

                let mut decoder_outputs = run(
                    decoder,
                    decoder_input_ptrs,
                    &decoder_input_ptrs_slice,
                    decoder_output_ptrs,
                )?;
                drop(input_ids_tensor);

                // Extract logits [1, seqLen, vocabSize] and find the argmax of
                // the last position.
                let logits = decoder_outputs
                    .first()
                    .ok_or_else(|| "decoder returned no outputs".to_string())?;
                let logits_info = logits.type_and_shape()?;
                let logits_type = logits_info.element_type()?;
                let logits_shape = logits_info.dimensions()?;
                let logits_count = logits_info.element_count()?;
                let batch_size = shape_at(&logits_shape, 0)?;
                let seq_len = shape_at(&logits_shape, 1)?;
                let vocab_size = shape_at(&logits_shape, 2)?;
                if batch_size < 1 || seq_len < 1 || vocab_size < 1 {
                    return Err("decoder returned invalid logits shape".to_string());
                }
                let seq_len_usize =
                    usize::try_from(seq_len).map_err(|_| "invalid decoder seq_len")?;
                let vocab_usize =
                    usize::try_from(vocab_size).map_err(|_| "invalid decoder vocab_size")?;
                let logits_index = (seq_len_usize - 1)
                    .checked_mul(vocab_usize)
                    .ok_or_else(|| "invalid logits offset".to_string())?;
                let expected_elements = seq_len_usize
                    .checked_mul(vocab_usize)
                    .ok_or_else(|| "invalid logits element count".to_string())?;
                let logits_data = logits.mutable_data()?;
                // SAFETY: `logits_data` is owned by `logits` (alive until end of loop),
                // and `LogitsTensor::from_raw` checks type, alignment, non-null, and count.
                let logits_tensor = unsafe {
                    LogitsTensor::from_raw(logits_type, logits_data, logits_count, expected_elements)?
                };
                let next_token = logits_tensor.argmax_row(logits_index, vocab_usize)? as i64;

                if next_token == initialized.config.eos_token_id
                    || next_token == initialized.config.no_speech_token_id
                {
                    break;
                }

                generated_tokens.push(next_token);
                if has_periodic_repetition_24(&generated_tokens) {
                    break;
                }

                // Move present_key / present_value into pastKv for the next step.
                past_kv.clear();
                past_kv.extend(decoder_outputs.drain(1..));
                step_ids.clear();
                step_ids.push(next_token);
            }

            if !generated_tokens.is_empty() {
                let text = initialized
                    .tokenizer
                    .decode(&generated_tokens, true)
                    .trim()
                    .to_string();
                if !text.is_empty() && !on_segment(utterance.start_sec, utterance.end_sec, &text) {
                    return Ok(false);
                }
            }
        }

        Ok(true)
    }

    /// C++ `OnnxSession::transcribeCtc(samples, options)`.
    fn transcribe_ctc(
        &self,
        initialized: &Initialized,
        samples: &[f32],
        should_abort: &dyn Fn() -> bool,
        on_segment: &mut dyn FnMut(f64, f64, &str) -> bool,
    ) -> Result<bool, String> {
        let mask_kind = if initialized.ctc_mask_is_bool {
            AttentionMaskKind::Bool
        } else {
            AttentionMaskKind::Int64
        };
        let mel = initialized.frontend.extract_with_mask(samples, mask_kind);
        if mel.num_frames <= 0 || mel.valid_frames <= 0 {
            return Ok(true);
        }

        let mem_info = MemoryInfoHandle::create_cpu()?;

        let Some(ctc) = &initialized.sessions.ctc else {
            return Err("ONNX ASR sessions are not initialized.".to_string());
        };

        let feature_shape = [1i64, mel.num_frames, mel.n_mels];
        let mask_shape = [1i64, mel.num_frames];

        let mut ctc_inputs: Vec<ValueHandle> = Vec::with_capacity(2);
        ctc_inputs.push(create_tensor(
            &mem_info,
            mel.features.as_ptr() as *mut c_void,
            mel.features.len() * std::mem::size_of::<f32>(),
            &feature_shape,
            ONNX_TENSOR_ELEMENT_DATA_TYPE_FLOAT,
        )?);
        if initialized.ctc_mask_is_bool {
            ctc_inputs.push(create_tensor(
                &mem_info,
                mel.attention_mask_bool.as_ptr() as *mut c_void,
                mel.attention_mask_bool.len(),
                &mask_shape,
                ONNX_TENSOR_ELEMENT_DATA_TYPE_BOOL,
            )?);
        } else {
            ctc_inputs.push(create_tensor(
                &mem_info,
                mel.attention_mask_int64.as_ptr() as *mut c_void,
                mel.attention_mask_int64.len() * std::mem::size_of::<i64>(),
                &mask_shape,
                ONNX_TENSOR_ELEMENT_DATA_TYPE_INT64,
            )?);
        }

        let ctc_input_ptrs_slice: Vec<*const OrtValue> =
            ctc_inputs.iter().map(ValueHandle::as_ptr).collect();
        let ctc_outputs = run(
            ctc,
            initialized.ctc_input_names.as_ptrs(),
            &ctc_input_ptrs_slice,
            initialized.ctc_output_names.as_ptrs(),
        )?;

        if should_abort() {
            return Ok(false);
        }

        let logits = ctc_outputs
            .first()
            .ok_or_else(|| "CTC session returned no outputs".to_string())?;
        let logits_info = logits.type_and_shape()?;
        let logits_type = logits_info.element_type()?;
        let logits_shape = logits_info.dimensions()?;
        let logits_count = logits_info.element_count()?;
        let batch_size = shape_at(&logits_shape, 0)?;
        let out_frames = shape_at(&logits_shape, 1)?;
        let vocab_dim = shape_at(&logits_shape, 2)?;
        if batch_size < 1 || out_frames < 0 || vocab_dim < 1 {
            return Err("CTC model returned invalid logits shape".to_string());
        }
        let blank_id =
            if initialized.config.ctc_blank_token_id >= 0 && initialized.config.ctc_blank_token_id < vocab_dim
            {
                initialized.config.ctc_blank_token_id
            } else {
                vocab_dim - 1
            };

        let out_frames_usize =
            usize::try_from(out_frames).map_err(|_| "invalid CTC output frames".to_string())?;
        let vocab_usize =
            usize::try_from(vocab_dim).map_err(|_| "invalid CTC vocab dimension".to_string())?;
        let expected_logits = out_frames_usize
            .checked_mul(vocab_usize)
            .ok_or_else(|| "CTC logits element count overflow".to_string())?;
        let logits_data = logits.mutable_data()?;
        // SAFETY: `logits_data` is owned by `logits` (alive for this function),
        // and `LogitsTensor::from_raw` checks element type, alignment, null, and count.
        let logits_tensor = unsafe {
            LogitsTensor::from_raw(logits_type, logits_data, logits_count, expected_logits)?
        };

        let out_mask = if let Some(mask_val) = ctc_outputs.get(1) {
            let mask_shape_info = mask_val.type_and_shape()?;
            let mask_type = mask_shape_info.element_type()?;
            let mask_count = mask_shape_info.element_count()?;
            let mask_data = mask_val.mutable_data()?;
            // SAFETY: `mask_data` is owned by `ctc_outputs[1]`, and `MaskTensor::from_raw`
            // verifies type (`INT64`/`INT32`/`BOOL`), alignment, null, and count >= out_frames.
            Some(unsafe {
                MaskTensor::from_raw(mask_type, mask_data, mask_count, out_frames_usize)?
            })
        } else {
            None
        };

        if out_frames == 0 {
            return Ok(true);
        }

        // Duration of one subsampled CTC output frame (8 * 160 / 16000 = 0.08 s).
        // C++ multiplies in `int` and then converts; wrapping mirrors the
        // x86-64 release build.
        let frame_sec = (initialized.config.subsampling_factor.wrapping_mul(
            initialized.config.frontend.hop_length,
        )) as f64
            / initialized.config.frontend.sample_rate as f64;
        let total_clip_sec = samples.len() as f64 / initialized.config.frontend.sample_rate as f64;

        // Greedy CTC decoding with frame-level timestamps.
        const SILENCE_SPLIT_FRAMES: i64 = 5; // 5 * 80 ms = 400 ms pause splits utterance
        let mut current_tokens: Vec<i64> = Vec::new();
        let mut cue_start_frame: i64 = -1;
        let mut last_non_blank_frame: i64 = -1;
        let mut prev_token: i64 = -1;

        for frame_idx in 0..out_frames_usize {
            let frame = frame_idx as i64;
            let mut best_id = blank_id;
            if out_mask
                .as_ref()
                .map_or(true, |mask| mask.is_active(frame_idx))
            {
                let row_offset = frame_idx
                    .checked_mul(vocab_usize)
                    .ok_or_else(|| "CTC frame offset overflow".to_string())?;
                best_id = logits_tensor.argmax_row(row_offset, vocab_usize)? as i64;
            }

            if best_id == blank_id {
                if cue_start_frame >= 0 && (frame - last_non_blank_frame) >= SILENCE_SPLIT_FRAMES {
                    if !flush_ctc_cue(
                        initialized,
                        on_segment,
                        &mut current_tokens,
                        &mut cue_start_frame,
                        last_non_blank_frame,
                        frame_sec,
                        total_clip_sec,
                    ) {
                        return Ok(false);
                    }
                }
                prev_token = blank_id;
                continue;
            }

            if best_id != prev_token {
                if cue_start_frame < 0 {
                    cue_start_frame = frame;
                }
                current_tokens.push(best_id);
                last_non_blank_frame = frame;

                let token_str = initialized.tokenizer.id_to_token_str(best_id).unwrap_or("");
                let current_duration = (frame - cue_start_frame + 1) as f64 * frame_sec;
                if current_duration >= 1.0 && ends_with_japanese_sentence_punctuation(token_str)
                {
                    if !flush_ctc_cue(
                        initialized,
                        on_segment,
                        &mut current_tokens,
                        &mut cue_start_frame,
                        last_non_blank_frame,
                        frame_sec,
                        total_clip_sec,
                    ) {
                        return Ok(false);
                    }
                }
            } else {
                last_non_blank_frame = frame;
            }
            prev_token = best_id;
        }

        if !flush_ctc_cue(
            initialized,
            on_segment,
            &mut current_tokens,
            &mut cue_start_frame,
            last_non_blank_frame,
            frame_sec,
            total_clip_sec,
        ) {
            return Ok(false);
        }
        Ok(true)
    }
}

/// C++ `flushCue` lambda inside `transcribeCtc`. Returns `false` if the segment
/// callback requested cancellation.
fn flush_ctc_cue(
    initialized: &Initialized,
    on_segment: &mut dyn FnMut(f64, f64, &str) -> bool,
    current_tokens: &mut Vec<i64>,
    cue_start_frame: &mut i64,
    last_non_blank_frame: i64,
    frame_sec: f64,
    total_clip_sec: f64,
) -> bool {
    if current_tokens.is_empty() || *cue_start_frame < 0 || last_non_blank_frame < *cue_start_frame {
        current_tokens.clear();
        *cue_start_frame = -1;
        return true;
    }
    let text = initialized
        .tokenizer
        .decode(current_tokens, true)
        .trim()
        .to_string();
    let mut keep_going = true;
    if !text.is_empty() {
        let start_sec = 0.0f64.max(*cue_start_frame as f64 * frame_sec - 0.04);
        let end_sec = total_clip_sec.min(
            (start_sec + 0.16).max((last_non_blank_frame + 1) as f64 * frame_sec + 0.08),
        );
        keep_going = on_segment(start_sec, end_sec, &text);
    }
    current_tokens.clear();
    *cue_start_frame = -1;
    keep_going
}

/// C++ `hasPeriodicRepetition24(tokens)`.
fn has_periodic_repetition_24(tokens: &[i64]) -> bool {
    const WINDOW: usize = 24;
    if tokens.len() < WINDOW {
        return false;
    }

    let tail = &tokens[tokens.len() - WINDOW..];
    for period in 1..=WINDOW / 2 {
        let mut periodic = true;
        for index in period..WINDOW {
            if tail[index] != tail[index - period] {
                periodic = false;
                break;
            }
        }
        if periodic {
            return true;
        }
    }
    false
}

/// C++ `endsWithJapaneseSentencePunctuation(text)`.
fn ends_with_japanese_sentence_punctuation(text: &str) -> bool {
    match text.chars().next_back() {
        None => false,
        Some(last) => matches!(
            last,
            '\u{3002}' | '\u{FF01}' | '\u{FF1F}' | '\u{2026}' | '!' | '?'
        ),
    }
}

/// `std::vector::at` semantics: the index check failure message of libstdc++
/// (the standard library the C++ build uses) is reproduced so a malformed
/// model fails with the same reason string.
fn shape_at(shape: &[i64], index: usize) -> Result<i64, String> {
    shape.get(index).copied().ok_or_else(|| {
        format!(
            "vector::_M_range_check: __n (which is {index}) >= this->size() (which is {})",
            shape.len()
        )
    })
}

/// `std::atoi(c_str)` for a token following a validated prefix: parses an
/// optional sign and digits, saturating instead of the C++ undefined behavior
/// on overflow (names carry small layer indices).
fn atoi_saturating(text: &str) -> i32 {
    let bytes = text.as_bytes();
    let mut index = 0usize;
    while index < bytes.len() && bytes[index].is_ascii_whitespace() {
        index += 1;
    }
    let mut negative = false;
    if index < bytes.len() && (bytes[index] == b'+' || bytes[index] == b'-') {
        negative = bytes[index] == b'-';
        index += 1;
    }
    let mut value: i64 = 0;
    while index < bytes.len() && bytes[index].is_ascii_digit() {
        value = (value * 10 + i64::from(bytes[index] - b'0')).min(1 << 40);
        index += 1;
    }
    let signed = if negative { -value } else { value };
    signed.clamp(i32::MIN as i64, i32::MAX as i64) as i32
}

/// `static_cast<int>(double)` as produced by x86-64 `cvttsd2si`: NaN,
/// infinities, and out-of-range values yield `INT_MIN` (the "integer
/// indefinite" result).
fn double_to_i32_x86(value: f64) -> i32 {
    let truncated = value.trunc();
    if !truncated.is_finite() || truncated < i32::MIN as f64 || truncated > i32::MAX as f64 {
        i32::MIN
    } else {
        truncated as i32
    }
}

/// Message extraction for a caught panic payload.
pub(crate) fn panic_payload_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        (*message).to_string()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "unknown panic".to_string()
    }
}

fn to_cstring(name: String) -> Result<CString, String> {
    CString::new(name).map_err(|_| "ONNX Runtime name contains a NUL byte".to_string())
}

/// Platform-native NUL-terminated path buffer for ONNX Runtime C API functions
/// taking `const ORTCHAR_T*` (`u16` on Windows, `c_char` on non-Windows).
struct OrtPath(Vec<OrtChar>);

impl OrtPath {
    fn from_path(path: &Path) -> Result<Self, String> {
        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStrExt;
            let mut wide: Vec<u16> = Vec::new();
            for unit in path.as_os_str().encode_wide() {
                if unit == 0 {
                    return Err("ONNX model path contains a NUL byte".to_string());
                }
                wide.push(unit);
            }
            wide.push(0);
            Ok(Self(wide))
        }
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            let bytes = path.as_os_str().as_bytes();
            if bytes.contains(&0) {
                return Err("ONNX model path contains a NUL byte".to_string());
            }
            let mut chars: Vec<OrtChar> = Vec::with_capacity(bytes.len() + 1);
            chars.extend(bytes.iter().map(|&b| b as OrtChar));
            chars.push(0);
            Ok(Self(chars))
        }
        #[cfg(not(any(unix, windows)))]
        {
            let lossy = path.to_string_lossy();
            let bytes = lossy.as_bytes();
            if bytes.contains(&0) {
                return Err("ONNX model path contains a NUL byte".to_string());
            }
            let mut chars: Vec<OrtChar> = Vec::with_capacity(bytes.len() + 1);
            chars.extend(bytes.iter().map(|&b| b as OrtChar));
            chars.push(0);
            Ok(Self(chars))
        }
    }

    fn as_ptr(&self) -> *const OrtChar {
        self.0.as_ptr()
    }
}

/// RAII wrapper over `OrtEnv*`. The underlying environment is a process-wide
/// singleton in ONNX Runtime (mirroring the C++ `sharedOrtEnv()` function-local
/// static) and is intentionally never released.
struct EnvHandle(*mut OrtEnv);
unsafe impl Send for EnvHandle {}
unsafe impl Sync for EnvHandle {}

static API: OnceLock<ApiHandle> = OnceLock::new();
static ENV: OnceLock<EnvHandle> = OnceLock::new();
static ENV_INIT: Mutex<()> = Mutex::new(());

struct ApiHandle(*const OrtApi);
unsafe impl Send for ApiHandle {}
unsafe impl Sync for ApiHandle {}

/// The parsed `OrtApi` vtable; valid after [`get_api`] succeeded.
fn api() -> &'static OrtApi {
    let handle = API
        .get()
        .expect("memento-asr-onnx: ONNX Runtime API used before initialization");
    unsafe { &*handle.0 }
}

/// C++ `OrtGetApiBase()->GetApi(ORT_API_VERSION)` — the version constant is
/// passed in from the C++ header so the Rust binding can never request a
/// different ABI than the C++ code compiles against.
fn get_api(ort_api_version: u32) -> Result<&'static OrtApi, String> {
    if API.get().is_some() {
        return Ok(api());
    }
    let base = unsafe { OrtGetApiBase() };
    if base.is_null() {
        return Err("ONNX Runtime API base is unavailable".to_string());
    }
    let api_ptr = unsafe { ((*base).GetApi)(ort_api_version) };
    if api_ptr.is_null() {
        return Err(format!(
            "ONNX Runtime library does not support API version {ort_api_version}"
        ));
    }
    let handle = API.get_or_init(|| ApiHandle(api_ptr));
    Ok(unsafe { &*handle.0 })
}

/// C++ `sharedOrtEnv()` — serialized initialization with lock-free `OnceLock`
/// reads so concurrent threads can never create duplicate `OrtEnv` instances.
fn get_env(api: &'static OrtApi) -> Result<*const OrtEnv, String> {
    if let Some(env) = ENV.get() {
        return Ok(env.0);
    }
    let _guard = ENV_INIT
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(env) = ENV.get() {
        return Ok(env.0);
    }
    let log_id = b"MementoOnnxAsr\0";
    let mut env: *mut OrtEnv = std::ptr::null_mut();
    check(unsafe {
        (api.CreateEnv)(
            ORT_LOGGING_LEVEL_WARNING,
            log_id.as_ptr() as *const c_char,
            &mut env,
        )
    })?;
    if env.is_null() {
        return Err("ONNX Runtime CreateEnv returned a null environment".to_string());
    }
    let handle = ENV.get_or_init(|| EnvHandle(env));
    if handle.0 != env {
        unsafe { (api.ReleaseEnv)(env) };
    }
    Ok(handle.0)
}

/// `Ort::ThrowOnError`: null status is success, otherwise the message is
/// extracted and the status is released.
fn check(status: *mut OrtStatus) -> Result<(), String> {
    if status.is_null() {
        return Ok(());
    }
    let api = api();
    let message = unsafe {
        let message_ptr = (api.GetErrorMessage)(status);
        let message = if message_ptr.is_null() {
            String::new()
        } else {
            CStr::from_ptr(message_ptr).to_string_lossy().into_owned()
        };
        (api.ReleaseStatus)(status);
        message
    };
    Err(message)
}

/// RAII guard for the provider array returned by `OrtApi::GetAvailableProviders`.
struct AvailableProvidersGuard {
    api: &'static OrtApi,
    providers: *mut *mut c_char,
    count: c_int,
}

impl AvailableProvidersGuard {
    fn release(mut self) -> Result<(), String> {
        let providers = std::mem::replace(&mut self.providers, std::ptr::null_mut());
        if providers.is_null() {
            return Ok(());
        }
        check(unsafe { (self.api.ReleaseAvailableProviders)(providers, self.count) })
    }
}

impl Drop for AvailableProvidersGuard {
    fn drop(&mut self) {
        if !self.providers.is_null() {
            let status =
                unsafe { (self.api.ReleaseAvailableProviders)(self.providers, self.count) };
            if !status.is_null() {
                unsafe { (self.api.ReleaseStatus)(status) };
            }
        }
    }
}

/// C++ `Ort::GetAvailableProviders()`.
fn available_providers(api: &'static OrtApi) -> Result<Vec<String>, String> {
    let mut providers: *mut *mut c_char = std::ptr::null_mut();
    let mut count: c_int = 0;
    check(unsafe { (api.GetAvailableProviders)(&mut providers, &mut count) })?;
    let guard = AvailableProvidersGuard {
        api,
        providers,
        count,
    };
    let count_usize = count.max(0) as usize;
    let mut result = Vec::with_capacity(count_usize);
    if !providers.is_null() {
        for index in 0..count_usize {
            let provider = unsafe { *providers.add(index) };
            if !provider.is_null() {
                result.push(unsafe { CStr::from_ptr(provider) }.to_string_lossy().into_owned());
            }
        }
    }
    guard.release()?;
    Ok(result)
}

/// C++ `Ort::SessionOptions` (RAII).
struct SessionOptionsHandle(*mut OrtSessionOptions);

impl SessionOptionsHandle {
    fn create() -> Result<Self, String> {
        let mut options: *mut OrtSessionOptions = std::ptr::null_mut();
        check(unsafe { (api().CreateSessionOptions)(&mut options) })?;
        Ok(SessionOptionsHandle(options))
    }

    fn set_intra_op_num_threads(&self, num_threads: i32) -> Result<(), String> {
        check(unsafe { (api().SetIntraOpNumThreads)(self.0, num_threads) })
    }

    fn set_inter_op_num_threads(&self, num_threads: i32) -> Result<(), String> {
        check(unsafe { (api().SetInterOpNumThreads)(self.0, num_threads) })
    }

    fn set_graph_optimization_level(&self, level: c_int) -> Result<(), String> {
        check(unsafe { (api().SetSessionGraphOptimizationLevel)(self.0, level) })
    }

    fn append_cuda(&self, options: &OrtCUDAProviderOptions) -> Result<(), String> {
        check(unsafe { (api().SessionOptionsAppendExecutionProvider_CUDA)(self.0, options) })
    }
}

impl Drop for SessionOptionsHandle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { (api().ReleaseSessionOptions)(self.0) };
        }
    }
}

/// C++ `Ort::AllocatorWithDefaultOptions` (non-owning). The C++ wrapper is
/// `Unowned<OrtAllocator>` — the allocator is owned by ONNX Runtime for the
/// lifetime of the process, so it must never be passed to `ReleaseAllocator`
/// (doing so aborts inside the runtime).
struct AllocatorHandle(*mut OrtAllocator);

impl AllocatorHandle {
    fn default_options() -> Result<Self, String> {
        let mut allocator: *mut OrtAllocator = std::ptr::null_mut();
        check(unsafe { (api().GetAllocatorWithDefaultOptions)(&mut allocator) })?;
        Ok(AllocatorHandle(allocator))
    }
}

/// C++ `Ort::MemoryInfo` (RAII).
struct MemoryInfoHandle(*mut OrtMemoryInfo);

impl MemoryInfoHandle {
    /// C++ `Ort::MemoryInfo::CreateCpu(OrtArenaAllocator, OrtMemTypeDefault)`.
    fn create_cpu() -> Result<Self, String> {
        let mut memory_info: *mut OrtMemoryInfo = std::ptr::null_mut();
        check(unsafe {
            (api().CreateCpuMemoryInfo)(ORT_ARENA_ALLOCATOR, ORT_MEM_TYPE_DEFAULT, &mut memory_info)
        })?;
        Ok(MemoryInfoHandle(memory_info))
    }
}

impl Drop for MemoryInfoHandle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { (api().ReleaseMemoryInfo)(self.0) };
        }
    }
}

/// Accessors shared by the owning (`Ort::Value`'s) and the borrowed
/// (`Ort::TypeInfo`'s) tensor-shape-info wrappers.
trait TensorShapeInfoAccessors {
    fn shape_info_ptr(&self) -> *const OrtTensorTypeAndShapeInfo;

    /// C++ `GetElementType()`.
    fn element_type(&self) -> Result<c_int, String> {
        let mut element_type: c_int = 0;
        check(unsafe {
            (api().GetTensorElementType)(self.shape_info_ptr(), &mut element_type)
        })?;
        Ok(element_type)
    }

    /// C++ `GetShape()`.
    fn dimensions(&self) -> Result<Vec<i64>, String> {
        let mut count: usize = 0;
        check(unsafe { (api().GetDimensionsCount)(self.shape_info_ptr(), &mut count) })?;
        let mut dimensions = vec![0i64; count];
        if count > 0 {
            check(unsafe {
                (api().GetDimensions)(self.shape_info_ptr(), dimensions.as_mut_ptr(), count)
            })?;
        }
        Ok(dimensions)
    }

    /// C++ `GetElementCount()`.
    fn element_count(&self) -> Result<usize, String> {
        let mut count: usize = 0;
        check(unsafe { (api().GetTensorShapeElementCount)(self.shape_info_ptr(), &mut count) })?;
        Ok(count)
    }
}

/// `OrtTensorTypeAndShapeInfo` owned by the caller: the C++ API's
/// `Value::GetTensorTypeAndShapeInfo()` returns an owning
/// `TensorTypeAndShapeInfo`, released here with
/// `ReleaseTensorTypeAndShapeInfo` (the raw-wrapper leak is fixed).
struct TensorShapeInfoHandle(*mut OrtTensorTypeAndShapeInfo);

impl TensorShapeInfoAccessors for TensorShapeInfoHandle {
    fn shape_info_ptr(&self) -> *const OrtTensorTypeAndShapeInfo {
        self.0
    }
}

impl Drop for TensorShapeInfoHandle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { (api().ReleaseTensorTypeAndShapeInfo)(self.0) };
        }
    }
}

/// `OrtTensorTypeAndShapeInfo` borrowed from an `OrtTypeInfo`: the C++ API's
/// `TypeInfo::GetTensorTypeAndShapeInfo()` returns a
/// `ConstTensorTypeAndShapeInfo` — an `Unowned` view owned by the TypeInfo —
/// so it must not be released separately and cannot outlive its parent
/// [`TypeInfoHandle`].
struct BorrowedTensorShapeInfoHandle<'a>(
    *const OrtTensorTypeAndShapeInfo,
    PhantomData<&'a TypeInfoHandle>,
);

impl TensorShapeInfoAccessors for BorrowedTensorShapeInfoHandle<'_> {
    fn shape_info_ptr(&self) -> *const OrtTensorTypeAndShapeInfo {
        self.0
    }
}

/// C++ `Ort::TypeInfo` (RAII; the C++ API view leaks it).
struct TypeInfoHandle(*mut OrtTypeInfo);

impl TypeInfoHandle {
    /// C++ `TypeInfo::GetTensorTypeAndShapeInfo()` (borrowed view; tied to
    /// `&self` via `PhantomData` so it cannot outlive this `TypeInfoHandle`).
    fn tensor_shape_info(&self) -> Result<BorrowedTensorShapeInfoHandle<'_>, String> {
        let mut shape_info: *const OrtTensorTypeAndShapeInfo = std::ptr::null();
        check(unsafe { (api().CastTypeInfoToTensorInfo)(self.0, &mut shape_info) })?;
        if shape_info.is_null() {
            return Err("ONNX Runtime type info is not a tensor".to_string());
        }
        Ok(BorrowedTensorShapeInfoHandle(shape_info, PhantomData))
    }
}

impl Drop for TypeInfoHandle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { (api().ReleaseTypeInfo)(self.0) };
        }
    }
}

/// RAII guard for buffers allocated through an `OrtAllocator` (e.g. session
/// input/output names) so memory is released even on early returns or panics.
struct OrtAllocatedPtr<'a> {
    allocator: &'a AllocatorHandle,
    ptr: *mut c_void,
}

impl<'a> OrtAllocatedPtr<'a> {
    fn new(allocator: &'a AllocatorHandle, ptr: *mut c_void) -> Self {
        Self { allocator, ptr }
    }

    fn free(mut self) -> Result<(), String> {
        let ptr = std::mem::replace(&mut self.ptr, std::ptr::null_mut());
        if ptr.is_null() {
            return Ok(());
        }
        check(unsafe { (api().AllocatorFree)(self.allocator.0, ptr) })
    }
}

impl Drop for OrtAllocatedPtr<'_> {
    fn drop(&mut self) {
        if !self.ptr.is_null() {
            let status = unsafe { (api().AllocatorFree)(self.allocator.0, self.ptr) };
            if !status.is_null() {
                unsafe { (api().ReleaseStatus)(status) };
            }
        }
    }
}

/// C++ `Ort::Session` (RAII). Session runs are internally synchronized by ONNX
/// Runtime; this port additionally serializes them (see `Engine::inference`).
struct SessionHandle(*mut OrtSession);
unsafe impl Send for SessionHandle {}
unsafe impl Sync for SessionHandle {}

impl SessionHandle {
    fn create(
        env: *const OrtEnv,
        path: &OrtPath,
        options: &SessionOptionsHandle,
    ) -> Result<Self, String> {
        let mut session: *mut OrtSession = std::ptr::null_mut();
        check(unsafe { (api().CreateSession)(env, path.as_ptr(), options.0, &mut session) })?;
        Ok(SessionHandle(session))
    }

    /// C++ `Session::GetInputCount()`.
    fn input_count(&self) -> Result<usize, String> {
        let mut count: usize = 0;
        check(unsafe { (api().SessionGetInputCount)(self.0, &mut count) })?;
        Ok(count)
    }

    /// C++ `Session::GetOutputCount()`.
    fn output_count(&self) -> Result<usize, String> {
        let mut count: usize = 0;
        check(unsafe { (api().SessionGetOutputCount)(self.0, &mut count) })?;
        Ok(count)
    }

    /// C++ `Session::GetInputNameAllocated(i, allocator)`.
    fn input_name(&self, index: usize, allocator: &AllocatorHandle) -> Result<String, String> {
        let mut name: *mut c_char = std::ptr::null_mut();
        check(unsafe { (api().SessionGetInputName)(self.0, index, allocator.0, &mut name) })?;
        let guard = OrtAllocatedPtr::new(allocator, name as *mut c_void);
        if name.is_null() {
            return Err("ONNX Runtime returned a null input name".to_string());
        }
        let result = unsafe { CStr::from_ptr(name) }.to_string_lossy().into_owned();
        guard.free()?;
        Ok(result)
    }

    /// C++ `Session::GetOutputNameAllocated(i, allocator)`.
    fn output_name(&self, index: usize, allocator: &AllocatorHandle) -> Result<String, String> {
        let mut name: *mut c_char = std::ptr::null_mut();
        check(unsafe { (api().SessionGetOutputName)(self.0, index, allocator.0, &mut name) })?;
        let guard = OrtAllocatedPtr::new(allocator, name as *mut c_void);
        if name.is_null() {
            return Err("ONNX Runtime returned a null output name".to_string());
        }
        let result = unsafe { CStr::from_ptr(name) }.to_string_lossy().into_owned();
        guard.free()?;
        Ok(result)
    }

    /// C++ `Session::GetInputTypeInfo(i)`.
    fn input_type_info(&self, index: usize) -> Result<TypeInfoHandle, String> {
        let mut type_info: *mut OrtTypeInfo = std::ptr::null_mut();
        check(unsafe { (api().SessionGetInputTypeInfo)(self.0, index, &mut type_info) })?;
        Ok(TypeInfoHandle(type_info))
    }
}

impl Drop for SessionHandle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { (api().ReleaseSession)(self.0) };
        }
    }
}

/// C++ `Ort::Value` (RAII).
struct ValueHandle(*mut OrtValue);
unsafe impl Send for ValueHandle {}
unsafe impl Sync for ValueHandle {}

impl ValueHandle {
    fn as_ptr(&self) -> *const OrtValue {
        self.0
    }

    /// C++ `Value::GetTensorTypeAndShapeInfo()`.
    fn type_and_shape(&self) -> Result<TensorShapeInfoHandle, String> {
        let mut shape_info: *mut OrtTensorTypeAndShapeInfo = std::ptr::null_mut();
        check(unsafe { (api().GetTensorTypeAndShape)(self.0, &mut shape_info) })?;
        Ok(TensorShapeInfoHandle(shape_info))
    }

    /// C++ `Value::GetTensorMutableData<T>()` (the C++ template only changes
    /// the pointer type, never the underlying call).
    fn mutable_data(&self) -> Result<*mut c_void, String> {
        let mut data: *mut c_void = std::ptr::null_mut();
        check(unsafe { (api().GetTensorMutableData)(self.0, &mut data) })?;
        Ok(data)
    }
}

impl Drop for ValueHandle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { (api().ReleaseValue)(self.0) };
        }
    }
}

/// C++ `Ort::Value::CreateTensor<T>(...)` — a view over caller-owned memory
/// with the byte count of `T`.
fn create_tensor(
    memory_info: &MemoryInfoHandle,
    data: *mut c_void,
    byte_count: usize,
    shape: &[i64],
    element_type: c_int,
) -> Result<ValueHandle, String> {
    let mut value: *mut OrtValue = std::ptr::null_mut();
    check(unsafe {
        (api().CreateTensorWithDataAsOrtValue)(
            memory_info.0,
            data,
            byte_count,
            shape.as_ptr(),
            shape.len(),
            element_type,
            &mut value,
        )
    })?;
    Ok(ValueHandle(value))
}

/// C++ `Ort::Session::Run(...)` with `Ort::RunOptions{nullptr}`.
fn run(
    session: &SessionHandle,
    input_names: &[*const c_char],
    inputs: &[*const OrtValue],
    output_names: &[*const c_char],
) -> Result<Vec<ValueHandle>, String> {
    let mut raw_outputs: Vec<*mut OrtValue> = vec![std::ptr::null_mut(); output_names.len()];
    let status = unsafe {
        (api().Run)(
            session.0,
            std::ptr::null(),
            input_names.as_ptr(),
            inputs.as_ptr(),
            inputs.len(),
            output_names.as_ptr(),
            output_names.len(),
            raw_outputs.as_mut_ptr(),
        )
    };
    // Wrap every output slot in RAII handles before checking `status` so any
    // partially written `OrtValue*` outputs are released on error.
    let outputs: Vec<ValueHandle> = raw_outputs.into_iter().map(ValueHandle).collect();
    check(status)?;
    if outputs.iter().any(|value| value.0.is_null()) {
        return Err("ONNX Runtime Run returned a null output tensor".to_string());
    }
    Ok(outputs)
}

fn f16_to_f32(bits: u16) -> f32 {
    let sign = u32::from(bits & 0x8000) << 16;
    let exp = u32::from((bits >> 10) & 0x1F);
    let frac = u32::from(bits & 0x03FF);
    if exp == 0 {
        if frac == 0 {
            return f32::from_bits(sign);
        }
        let mut e = 127u32 - 15 + 1;
        let mut m = frac;
        while (m & 0x0400) == 0 {
            m <<= 1;
            e -= 1;
        }
        return f32::from_bits(sign | (e << 23) | ((m & 0x03FF) << 13));
    }
    if exp == 0x1F {
        if frac == 0 {
            return f32::from_bits(sign | 0x7F80_0000);
        }
        return f32::from_bits(sign | 0x7FC0_0000 | (frac << 13));
    }
    f32::from_bits(sign | ((exp + 112) << 23) | (frac << 13))
}

enum LogitsTensor<'a> {
    F32(&'a [f32]),
    F16(&'a [u16]),
}

impl<'a> LogitsTensor<'a> {
    unsafe fn from_raw(
        element_type: c_int,
        data: *const c_void,
        element_count: usize,
        expected_elements: usize,
    ) -> Result<Self, String> {
        if element_count < expected_elements {
            return Err(format!(
                "logits tensor element count ({element_count}) is smaller than expected ({expected_elements})"
            ));
        }
        if expected_elements == 0 {
            return match element_type {
                ONNX_TENSOR_ELEMENT_DATA_TYPE_FLOAT => Ok(Self::F32(&[])),
                ONNX_TENSOR_ELEMENT_DATA_TYPE_FLOAT16 => Ok(Self::F16(&[])),
                other => Err(format!("unsupported logits tensor element type {other}")),
            };
        }
        if data.is_null() {
            return Err("logits tensor data pointer is null".to_string());
        }
        match element_type {
            ONNX_TENSOR_ELEMENT_DATA_TYPE_FLOAT => {
                if (data as usize) % std::mem::align_of::<f32>() != 0 {
                    return Err("logits tensor f32 pointer is unaligned".to_string());
                }
                Ok(Self::F32(std::slice::from_raw_parts(
                    data as *const f32,
                    expected_elements,
                )))
            }
            ONNX_TENSOR_ELEMENT_DATA_TYPE_FLOAT16 => {
                if (data as usize) % std::mem::align_of::<u16>() != 0 {
                    return Err("logits tensor f16 pointer is unaligned".to_string());
                }
                Ok(Self::F16(std::slice::from_raw_parts(
                    data as *const u16,
                    expected_elements,
                )))
            }
            other => Err(format!("unsupported logits tensor element type {other}")),
        }
    }

    fn argmax_row(&self, row_offset: usize, row_len: usize) -> Result<usize, String> {
        if row_len == 0 {
            return Err("logits row length must be positive".to_string());
        }
        let end = row_offset
            .checked_add(row_len)
            .ok_or_else(|| "logits row offset overflow".to_string())?;
        match self {
            Self::F32(slice) => {
                let row = slice
                    .get(row_offset..end)
                    .ok_or_else(|| "logits row out of bounds".to_string())?;
                let mut best_idx = 0usize;
                let mut max_val = row[0];
                for (idx, &val) in row.iter().enumerate().skip(1) {
                    if val > max_val {
                        max_val = val;
                        best_idx = idx;
                    }
                }
                Ok(best_idx)
            }
            Self::F16(slice) => {
                let row = slice
                    .get(row_offset..end)
                    .ok_or_else(|| "logits row out of bounds".to_string())?;
                let mut best_idx = 0usize;
                let mut max_val = f16_to_f32(row[0]);
                for (idx, &bits) in row.iter().enumerate().skip(1) {
                    let val = f16_to_f32(bits);
                    if val > max_val {
                        max_val = val;
                        best_idx = idx;
                    }
                }
                Ok(best_idx)
            }
        }
    }
}

enum MaskTensor<'a> {
    I64(&'a [i64]),
    I32(&'a [i32]),
    Bool(&'a [u8]),
}

impl<'a> MaskTensor<'a> {
    unsafe fn from_raw(
        element_type: c_int,
        data: *const c_void,
        element_count: usize,
        expected_frames: usize,
    ) -> Result<Self, String> {
        if element_count < expected_frames {
            return Err(format!(
                "CTC mask element count ({element_count}) is smaller than output frames ({expected_frames})"
            ));
        }
        if expected_frames == 0 {
            return match element_type {
                ONNX_TENSOR_ELEMENT_DATA_TYPE_INT64 => Ok(Self::I64(&[])),
                ONNX_TENSOR_ELEMENT_DATA_TYPE_INT32 => Ok(Self::I32(&[])),
                ONNX_TENSOR_ELEMENT_DATA_TYPE_BOOL => Ok(Self::Bool(&[])),
                other => Err(format!("unsupported CTC mask element type {other}")),
            };
        }
        if data.is_null() {
            return Err("CTC mask tensor data pointer is null".to_string());
        }
        match element_type {
            ONNX_TENSOR_ELEMENT_DATA_TYPE_INT64 => {
                if (data as usize) % std::mem::align_of::<i64>() != 0 {
                    return Err("CTC mask i64 pointer is unaligned".to_string());
                }
                Ok(Self::I64(std::slice::from_raw_parts(
                    data as *const i64,
                    expected_frames,
                )))
            }
            ONNX_TENSOR_ELEMENT_DATA_TYPE_INT32 => {
                if (data as usize) % std::mem::align_of::<i32>() != 0 {
                    return Err("CTC mask i32 pointer is unaligned".to_string());
                }
                Ok(Self::I32(std::slice::from_raw_parts(
                    data as *const i32,
                    expected_frames,
                )))
            }
            ONNX_TENSOR_ELEMENT_DATA_TYPE_BOOL => Ok(Self::Bool(std::slice::from_raw_parts(
                data as *const u8,
                expected_frames,
            ))),
            other => Err(format!("unsupported CTC mask element type {other}")),
        }
    }

    fn is_active(&self, frame: usize) -> bool {
        match self {
            Self::I64(slice) => slice.get(frame).is_some_and(|&v| v != 0),
            Self::I32(slice) => slice.get(frame).is_some_and(|&v| v != 0),
            Self::Bool(slice) => slice.get(frame).is_some_and(|&v| v != 0),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn f16_to_f32_decodes_normals_subnormals_and_specials() {
        assert_eq!(f16_to_f32(0x0000), 0.0);
        assert_eq!(f16_to_f32(0x3C00), 1.0);
        assert_eq!(f16_to_f32(0xC000), -2.0);
        assert_eq!(f16_to_f32(0x0001), 2.0f32.powi(-24));
        assert!(f16_to_f32(0x7C00).is_infinite());
        assert!(f16_to_f32(0x7E00).is_nan());
    }

    #[test]
    fn logits_tensor_validates_type_and_bounds_before_reading() {
        let data = [0.1f32, 0.9, 0.2, 0.4];
        let err = unsafe {
            LogitsTensor::from_raw(
                ONNX_TENSOR_ELEMENT_DATA_TYPE_INT64,
                data.as_ptr().cast(),
                4,
                4,
            )
        };
        assert!(err.is_err());

        let short = unsafe {
            LogitsTensor::from_raw(
                ONNX_TENSOR_ELEMENT_DATA_TYPE_FLOAT,
                data.as_ptr().cast(),
                3,
                4,
            )
        };
        assert!(short.is_err());

        let ok = unsafe {
            LogitsTensor::from_raw(
                ONNX_TENSOR_ELEMENT_DATA_TYPE_FLOAT,
                data.as_ptr().cast(),
                4,
                4,
            )
            .unwrap()
        };
        assert_eq!(ok.argmax_row(0, 2).unwrap(), 1);
        assert_eq!(ok.argmax_row(2, 2).unwrap(), 1);

        let fp16_data = [0x3C00u16, 0x4000u16, 0xC000u16]; // 1.0, 2.0, -2.0
        let fp16 = unsafe {
            LogitsTensor::from_raw(
                ONNX_TENSOR_ELEMENT_DATA_TYPE_FLOAT16,
                fp16_data.as_ptr().cast(),
                3,
                3,
            )
            .unwrap()
        };
        assert_eq!(fp16.argmax_row(0, 3).unwrap(), 1);
    }

    #[test]
    fn mask_tensor_validates_type_and_supports_i64_i32_bool() {
        let i64_mask = [1i64, 0, 1];
        let m64 = unsafe {
            MaskTensor::from_raw(
                ONNX_TENSOR_ELEMENT_DATA_TYPE_INT64,
                i64_mask.as_ptr().cast(),
                3,
                3,
            )
            .unwrap()
        };
        assert!(m64.is_active(0));
        assert!(!m64.is_active(1));

        let i32_mask = [0i32, 1];
        let m32 = unsafe {
            MaskTensor::from_raw(
                ONNX_TENSOR_ELEMENT_DATA_TYPE_INT32,
                i32_mask.as_ptr().cast(),
                2,
                2,
            )
            .unwrap()
        };
        assert!(!m32.is_active(0));
        assert!(m32.is_active(1));

        let bool_mask = [1u8, 0];
        let mb = unsafe {
            MaskTensor::from_raw(
                ONNX_TENSOR_ELEMENT_DATA_TYPE_BOOL,
                bool_mask.as_ptr().cast(),
                2,
                2,
            )
            .unwrap()
        };
        assert!(mb.is_active(0));
        assert!(!mb.is_active(1));

        let short = unsafe {
            MaskTensor::from_raw(
                ONNX_TENSOR_ELEMENT_DATA_TYPE_INT64,
                i64_mask.as_ptr().cast(),
                2,
                3,
            )
        };
        assert!(short.is_err());

        let bad_type = unsafe {
            MaskTensor::from_raw(
                ONNX_TENSOR_ELEMENT_DATA_TYPE_FLOAT,
                i64_mask.as_ptr().cast(),
                3,
                3,
            )
        };
        assert!(bad_type.is_err());
    }

    #[test]
    fn transcribe_samples_uses_safe_callbacks_for_abort_and_errors() {
        let engine = Engine::create(
            Path::new("/nonexistent/onnx-asr-model"),
            Options {
                use_gpu: false,
                gpu_device: 0,
                threads: 1,
                punctuation: true,
            },
        );
        assert!(!engine.is_loaded());

        // Abort before inference returns Ok(false) (canceled).
        let mut segments = Vec::new();
        let canceled = engine
            .transcribe_samples(
                &[0.1f32, -0.1],
                "ja",
                &|| true,
                &mut |start, end, text| {
                    segments.push((start, end, text.to_owned()));
                    true
                },
            )
            .expect("abort should not error");
        assert!(!canceled);
        assert!(segments.is_empty());

        // Empty samples return Err without touching ORT.
        let err = engine
            .transcribe_samples(&[], "ja", &|| false, &mut |_, _, _| true)
            .expect_err("empty samples must fail");
        assert_eq!(err, WAV_READ_FAILED);

        // Unloaded model reports failure via Callbacks::on_failure.
        let mut failures = Vec::new();
        let mut on_segment = |_: f64, _: f64, _: &str| true;
        let mut on_failure = |msg: &str| failures.push(msg.to_owned());
        let mut callbacks = Callbacks::new(&|| false, &mut on_segment, &mut on_failure);
        let status = engine.transcribe(Path::new("/nonexistent/audio.wav"), "ja", &mut callbacks);
        assert_eq!(status, RESULT_FAILED);
        assert_eq!(failures.len(), 1);
    }

    #[test]
    fn frontend_validator_accepts_whisper_n_fft_400_and_rejects_zero_or_unbounded() {
        use memento_asr::audio::melfrontend::MelFrontendConfig;
        let whisper_cfg = MelFrontendConfig {
            n_fft: 400,
            win_length: 400,
            hop_length: 160,
            n_mels: 80,
            sample_rate: 16000,
            ..MelFrontendConfig::default()
        };
        assert!(validate_frontend_config(&whisper_cfg).is_ok());
        assert!(validate_frontend_config(&MelFrontendConfig::default()).is_ok());

        for bad_n_fft in [0, -1, MAX_ONNX_N_FFT + 1] {
            assert!(validate_frontend_config(&MelFrontendConfig {
                n_fft: bad_n_fft,
                ..whisper_cfg
            })
            .is_err());
        }
        assert!(validate_frontend_config(&MelFrontendConfig {
            hop_length: 0,
            ..whisper_cfg
        })
        .is_err());
        assert!(validate_frontend_config(&MelFrontendConfig {
            n_mels: 0,
            ..whisper_cfg
        })
        .is_err());
        assert!(validate_frontend_config(&MelFrontendConfig {
            sample_rate: 0,
            ..whisper_cfg
        })
        .is_err());
    }

    #[test]
    fn double_to_i32_x86_handles_nan_infinities_and_bounds() {
        assert_eq!(double_to_i32_x86(f64::NAN), i32::MIN);
        assert_eq!(double_to_i32_x86(f64::INFINITY), i32::MIN);
        assert_eq!(double_to_i32_x86(f64::NEG_INFINITY), i32::MIN);
        assert_eq!(double_to_i32_x86(i32::MAX as f64 + 1.0), i32::MIN);
        assert_eq!(double_to_i32_x86(i32::MIN as f64 - 1.0), i32::MIN);
        assert_eq!(double_to_i32_x86(i32::MAX as f64), i32::MAX);
        assert_eq!(double_to_i32_x86(i32::MIN as f64), i32::MIN);
        assert_eq!(double_to_i32_x86(42.9), 42);
        assert_eq!(double_to_i32_x86(-42.9), -42);
    }

    #[test]
    fn ort_path_encodes_and_rejects_embedded_nul() {
        let encoded = OrtPath::from_path(Path::new("model/encoder.onnx")).unwrap();
        assert_eq!(encoded.0.last(), Some(&0));
        assert!(!encoded.as_ptr().is_null());
        assert!(OrtPath::from_path(Path::new("bad\0path.onnx")).is_err());
    }

    #[test]
    fn cached_ort_names_keeps_valid_c_strings_after_move() {
        let names = CachedOrtNames::new(vec![
            to_cstring("input_ids".to_string()).unwrap(),
            to_cstring("logits".to_string()).unwrap(),
        ]);
        let moved = names;
        let ptrs = moved.as_ptrs();
        assert_eq!(ptrs.len(), 2);
        assert_eq!(
            unsafe { CStr::from_ptr(ptrs[0]) }.to_str().unwrap(),
            "input_ids"
        );
        assert_eq!(
            unsafe { CStr::from_ptr(ptrs[1]) }.to_str().unwrap(),
            "logits"
        );
    }

    #[test]
    fn transcribe_samples_aborts_while_waiting_for_init_done() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let engine = Arc::new(Engine {
            options: Options {
                use_gpu: false,
                gpu_device: 0,
                threads: 1,
                punctuation: true,
            },
            config_valid: true,
            sample_rate: 16000,
            staged: Mutex::new(None),
            init_state: Mutex::new(InitState {
                done: false,
                load_error: String::new(),
            }),
            init_done: Condvar::new(),
            initialized: OnceLock::new(),
            inference: Mutex::new(()),
        });
        let abort = Arc::new(AtomicBool::new(false));
        let abort_trigger = Arc::clone(&abort);
        let trigger = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(40));
            abort_trigger.store(true, Ordering::Release);
        });
        let result = engine.transcribe_samples(
            &[0.1f32, -0.1],
            "ja",
            &|| abort.load(Ordering::Acquire),
            &mut |_, _, _| true,
        );
        trigger.join().unwrap();
        assert_eq!(result, Ok(false));
    }

    #[test]
    fn cuda_provider_options_defaults_to_same_as_requested_arena_strategy() {
        assert_eq!(ORT_ARENA_EXTEND_STRATEGY_SAME_AS_REQUESTED, 1);
        let options = OrtCUDAProviderOptions::default();
        assert_eq!(
            options.arena_extend_strategy,
            ORT_ARENA_EXTEND_STRATEGY_SAME_AS_REQUESTED
        );
        assert_eq!(options.arena_extend_strategy, 1);
    }
}

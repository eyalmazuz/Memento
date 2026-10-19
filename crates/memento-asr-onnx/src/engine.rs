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
use std::os::raw::{c_char, c_int, c_void};
use std::path::Path;
use std::sync::{Arc, Condvar, Mutex, OnceLock};

use memento_asr::audio::melfrontend::MelFrontend;
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
/// C++ `ONNX_TENSOR_ELEMENT_DATA_TYPE_INT64`.
const ONNX_TENSOR_ELEMENT_DATA_TYPE_INT64: c_int = 7;
/// C++ `ONNX_TENSOR_ELEMENT_DATA_TYPE_BOOL`.
const ONNX_TENSOR_ELEMENT_DATA_TYPE_BOOL: c_int = 9;
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

/// Function-pointer callbacks handed in across the C ABI (see
/// `src/asr/rust/asr_onnx_capi.h`). Null function pointers mean "no callback",
/// mirroring the optional `std::function` / `shared_ptr` members of the C++
/// `AsrTranscriptionOptions`.
#[derive(Clone, Copy)]
pub struct Callbacks {
    /// C++ `options.abort && options.abort->load()`.
    pub abort: Option<unsafe extern "C" fn(ctx: *mut c_void) -> bool>,
    pub abort_ctx: *mut c_void,
    /// C++ `options.failureCallback`.
    pub fail: Option<unsafe extern "C" fn(ctx: *mut c_void, message: *const c_char)>,
    pub fail_ctx: *mut c_void,
    /// C++ `options.segmentCallback`.
    pub segment:
        Option<unsafe extern "C" fn(ctx: *mut c_void, text: *const c_char, start: f64, end: f64)>,
    pub segment_ctx: *mut c_void,
}

impl Callbacks {
    fn aborted(&self) -> bool {
        match self.abort {
            Some(check) => unsafe { check(self.abort_ctx) },
            None => false,
        }
    }

    fn fail(&self, message: &str) {
        if let Some(fail) = self.fail {
            if let Ok(c_message) = CString::new(message) {
                unsafe { fail(self.fail_ctx, c_message.as_ptr()) };
            }
        }
    }

    fn segment(&self, text: &str, start: f64, end: f64) {
        if let Some(segment) = self.segment {
            if let Ok(c_text) = CString::new(text) {
                unsafe { segment(self.segment_ctx, c_text.as_ptr(), start, end) };
            }
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
    pub fn transcribe(&self, audio_path: &Path, language: &str, callbacks: &Callbacks) -> i32 {
        if callbacks.aborted() {
            return RESULT_CANCELED;
        }

        let samples = match wavreader::read_pcm16_wav_path(audio_path, self.sample_rate as u32) {
            Ok(samples) if !samples.is_empty() => samples,
            Ok(_) => {
                callbacks.fail(WAV_READ_FAILED);
                return RESULT_FAILED;
            }
            Err(message) => {
                let message = if message.is_empty() {
                    WAV_READ_FAILED.to_string()
                } else {
                    message
                };
                callbacks.fail(&message);
                return RESULT_FAILED;
            }
        };

        if callbacks.aborted() {
            return RESULT_CANCELED;
        }

        // C++ `if (m_initFuture.isValid()) m_initFuture.waitForFinished();` —
        // the future exists exactly when the constructor succeeded.
        if self.config_valid {
            let mut state = self
                .init_state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            while !state.done {
                state = self
                    .init_done
                    .wait(state)
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
            }
        }

        if callbacks.aborted() {
            return RESULT_CANCELED;
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
            drop(state);
            callbacks.fail(&message);
            return RESULT_FAILED;
        };

        if callbacks.aborted() {
            return RESULT_CANCELED;
        }

        // C++ `std::scoped_lock lock(m_mutex);` — serializes inference.
        let _guard = self
            .inference
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        let result = match initialized.config.architecture {
            OnnxAsrArchitecture::EncoderDecoder => self.transcribe_encoder_decoder(
                initialized,
                &samples,
                language,
                callbacks,
            ),
            OnnxAsrArchitecture::Ctc => self.transcribe_ctc(initialized, &samples, callbacks),
            OnnxAsrArchitecture::Unknown => {
                callbacks.fail(UNSUPPORTED_ARCHITECTURE);
                return RESULT_FAILED;
            }
        };

        match result {
            Ok(code) => code,
            Err(message) => {
                // C++ `catch (const std::exception &ex)`: fail("ONNX
                // inference error: %1") then Failed.
                callbacks.fail(&format!("ONNX inference error: {message}"));
                RESULT_FAILED
            }
        }
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

        match config.architecture {
            OnnxAsrArchitecture::EncoderDecoder => {
                let encoder = SessionHandle::create(
                    env,
                    &path_to_cstring(&config.encoder_onnx_path)?,
                    &session_options,
                )?;
                let decoder = SessionHandle::create(
                    env,
                    &path_to_cstring(&config.decoder_onnx_path)?,
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

                sessions.encoder = Some(encoder);
                sessions.decoder = Some(decoder);
            }
            OnnxAsrArchitecture::Ctc => {
                let ctc = SessionHandle::create(
                    env,
                    &path_to_cstring(&config.ctc_onnx_path)?,
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

                sessions.ctc = Some(ctc);
            }
            OnnxAsrArchitecture::Unknown => {
                // C++ falls through to frontend creation with no sessions;
                // transcribe then reports "Unsupported ONNX ASR model
                // architecture." exactly like this port.
            }
        }

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
        })
    }

    /// C++ `OnnxSession::transcribeEncoderDecoder(samples, options)`.
    fn transcribe_encoder_decoder(
        &self,
        initialized: &Initialized,
        samples: &[f32],
        language: &str,
        callbacks: &Callbacks,
    ) -> Result<i32, String> {
        // Segment the audio window into speech utterances so untimed seq2seq
        // models emit accurately timed subtitle cues.
        let utterances = MelFrontend::segment_speech_utterances(
            samples,
            initialized.config.frontend.sample_rate,
            10.0,
        );
        if utterances.is_empty() {
            // Silence clip — nothing to emit, return Success.
            return Ok(RESULT_SUCCESS);
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

        // Encoder output names: ["encoder_attention_mask", "cross_key.0",
        // "cross_value.0", ...].
        let mut encoder_output_names: Vec<CString> = Vec::new();
        encoder_output_names.push(to_cstring("encoder_attention_mask".to_string())?);
        for layer in 0..initialized.num_decoder_layers {
            encoder_output_names.push(to_cstring(format!("cross_key.{layer}"))?);
            encoder_output_names.push(to_cstring(format!("cross_value.{layer}"))?);
        }
        let encoder_output_ptrs = cstring_ptrs(&encoder_output_names);

        // Decoder input names: ["input_ids", "encoder_attention_mask",
        // "cross_key.N", "cross_value.N", ..., "past_key.N", "past_value.N", ...].
        let mut decoder_input_names: Vec<CString> = Vec::new();
        decoder_input_names.push(to_cstring("input_ids".to_string())?);
        decoder_input_names.push(to_cstring("encoder_attention_mask".to_string())?);
        for layer in 0..initialized.num_decoder_layers {
            decoder_input_names.push(to_cstring(format!("cross_key.{layer}"))?);
            decoder_input_names.push(to_cstring(format!("cross_value.{layer}"))?);
        }
        for layer in 0..initialized.num_decoder_layers {
            decoder_input_names.push(to_cstring(format!("past_key.{layer}"))?);
            decoder_input_names.push(to_cstring(format!("past_value.{layer}"))?);
        }
        let decoder_input_ptrs = cstring_ptrs(&decoder_input_names);

        // Decoder output names: ["logits", "present_key.N", "present_value.N", ...].
        let mut decoder_output_names: Vec<CString> = Vec::new();
        decoder_output_names.push(to_cstring("logits".to_string())?);
        for layer in 0..initialized.num_decoder_layers {
            decoder_output_names.push(to_cstring(format!("present_key.{layer}"))?);
            decoder_output_names.push(to_cstring(format!("present_value.{layer}"))?);
        }
        let decoder_output_ptrs = cstring_ptrs(&decoder_output_names);

        let encoder_input_names = [
            to_cstring("input_features".to_string())?,
            to_cstring("attention_mask".to_string())?,
        ];
        let encoder_input_ptrs = cstring_ptrs(&encoder_input_names);

        for utterance in &utterances {
            if callbacks.aborted() {
                return Ok(RESULT_CANCELED);
            }

            let utterance_samples = &samples[utterance.start_sample..utterance.end_sample];
            let mel = initialized.frontend.extract(utterance_samples);
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
                &encoder_input_ptrs,
                &encoder_input_ptrs_slice,
                &encoder_output_ptrs,
            )?;

            if callbacks.aborted() {
                return Ok(RESULT_CANCELED);
            }

            // Initialize the empty self-attention KV cache [1, heads, 0, headDim].
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
                    std::ptr::null_mut(),
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

            for _step in 0..max_new_tokens {
                if callbacks.aborted() {
                    return Ok(RESULT_CANCELED);
                }

                let ids_shape = [1i64, step_ids.len() as i64];
                let mut decoder_inputs: Vec<ValueHandle> =
                    Vec::with_capacity(2 + 4 * initialized.num_decoder_layers as usize);
                decoder_inputs.push(create_tensor(
                    &mem_info,
                    step_ids.as_ptr() as *mut c_void,
                    step_ids.len() * std::mem::size_of::<i64>(),
                    &ids_shape,
                    ONNX_TENSOR_ELEMENT_DATA_TYPE_INT64,
                )?);

                // Share encoder_attention_mask and cross_key/cross_value
                // without copying buffer data.
                for source in &encoder_outputs {
                    let shape_info = source.type_and_shape()?;
                    let element_type = shape_info.element_type()?;
                    let shape = shape_info.dimensions()?;
                    let element_count = shape_info.element_count()?;
                    // Mirrors the C++ branch structure: bool / int64 / (else)
                    // float, with the byte count of the chosen element type.
                    let (tensor_element_type, element_size) = match element_type {
                        ONNX_TENSOR_ELEMENT_DATA_TYPE_BOOL => {
                            (ONNX_TENSOR_ELEMENT_DATA_TYPE_BOOL, 1usize)
                        }
                        ONNX_TENSOR_ELEMENT_DATA_TYPE_INT64 => {
                            (ONNX_TENSOR_ELEMENT_DATA_TYPE_INT64, 8usize)
                        }
                        _ => (ONNX_TENSOR_ELEMENT_DATA_TYPE_FLOAT, 4usize),
                    };
                    decoder_inputs.push(create_tensor(
                        &mem_info,
                        source.mutable_data()?,
                        element_count * element_size,
                        &shape,
                        tensor_element_type,
                    )?);
                }

                for kv in past_kv.drain(..) {
                    decoder_inputs.push(kv);
                }
                let decoder_input_ptrs_slice: Vec<*const OrtValue> =
                    decoder_inputs.iter().map(ValueHandle::as_ptr).collect();

                let mut decoder_outputs = run(
                    decoder,
                    &decoder_input_ptrs,
                    &decoder_input_ptrs_slice,
                    &decoder_output_ptrs,
                )?;
                drop(decoder_inputs);

                // Extract logits [1, seqLen, vocabSize] and find the argmax of
                // the last position.
                let logits = &decoder_outputs[0];
                let logits_shape = logits.type_and_shape()?.dimensions()?;
                let seq_len = shape_at(&logits_shape, 1)?;
                let vocab_size = shape_at(&logits_shape, 2)?;
                if seq_len < 1 || vocab_size < 1 {
                    // The C++ code reads out of bounds on such a shape; this
                    // port fails cleanly instead.
                    return Err("decoder returned invalid logits shape".to_string());
                }
                let logits_index = (seq_len as i128 - 1) * vocab_size as i128;
                let logits_index =
                    usize::try_from(logits_index).map_err(|_| "invalid logits shape")?;
                let logits_data = logits.mutable_data()? as *const f32;
                let last_logits = unsafe { logits_data.add(logits_index) };

                let mut next_token: i64 = 0;
                let mut max_logit = unsafe { *last_logits };
                for value in 1..vocab_size {
                    let logit = unsafe { *last_logits.add(value as usize) };
                    if logit > max_logit {
                        max_logit = logit;
                        next_token = value;
                    }
                }

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
                if !text.is_empty() && callbacks.segment.is_some() {
                    callbacks.segment(&text, utterance.start_sec, utterance.end_sec);
                }
            }
        }

        Ok(RESULT_SUCCESS)
    }

    /// C++ `OnnxSession::transcribeCtc(samples, options)`.
    fn transcribe_ctc(
        &self,
        initialized: &Initialized,
        samples: &[f32],
        callbacks: &Callbacks,
    ) -> Result<i32, String> {
        let mel = initialized.frontend.extract(samples);
        if mel.num_frames <= 0 || mel.valid_frames <= 0 {
            return Ok(RESULT_SUCCESS);
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

        let ctc_input_names = [
            to_cstring("input_features".to_string())?,
            to_cstring("attention_mask".to_string())?,
        ];
        let ctc_input_ptrs = cstring_ptrs(&ctc_input_names);

        let allocator = AllocatorHandle::default_options()?;
        let output_count = ctc.output_count()?;
        let mut ctc_output_names: Vec<CString> = Vec::with_capacity(output_count);
        for index in 0..output_count {
            ctc_output_names.push(to_cstring(ctc.output_name(index, &allocator)?)?);
        }
        let ctc_output_ptrs = cstring_ptrs(&ctc_output_names);

        let ctc_input_ptrs_slice: Vec<*const OrtValue> =
            ctc_inputs.iter().map(ValueHandle::as_ptr).collect();
        let ctc_outputs = run(ctc, &ctc_input_ptrs, &ctc_input_ptrs_slice, &ctc_output_ptrs)?;

        if callbacks.aborted() {
            return Ok(RESULT_CANCELED);
        }

        let logits = &ctc_outputs[0];
        let logits_shape = logits.type_and_shape()?.dimensions()?;
        let out_frames = shape_at(&logits_shape, 1)?;
        let vocab_dim = shape_at(&logits_shape, 2)?;
        let blank_id =
            if initialized.config.ctc_blank_token_id >= 0 && initialized.config.ctc_blank_token_id < vocab_dim
            {
                initialized.config.ctc_blank_token_id
            } else {
                vocab_dim - 1
            };

        let logits_data = logits.mutable_data()? as *const f32;
        let mut out_mask_data: Option<*const i64> = None;
        if ctc_outputs.len() > 1 {
            let mask_shape_info = ctc_outputs[1].type_and_shape()?;
            if mask_shape_info.element_type()? == ONNX_TENSOR_ELEMENT_DATA_TYPE_INT64 {
                out_mask_data = Some(ctc_outputs[1].mutable_data()? as *const i64);
            }
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

        for frame in 0..out_frames {
            let mut best_id = blank_id;
            if out_mask_data.map_or(true, |mask| unsafe { *mask.add(frame as usize) } != 0) {
                let row = unsafe { logits_data.add((frame * vocab_dim) as usize) };
                best_id = 0;
                let mut max_value = unsafe { *row };
                for token in 1..vocab_dim {
                    let value = unsafe { *row.add(token as usize) };
                    if value > max_value {
                        max_value = value;
                        best_id = token;
                    }
                }
            }

            if best_id == blank_id {
                if cue_start_frame >= 0 && (frame - last_non_blank_frame) >= SILENCE_SPLIT_FRAMES {
                    flush_ctc_cue(
                        initialized,
                        callbacks,
                        &mut current_tokens,
                        &mut cue_start_frame,
                        last_non_blank_frame,
                        frame_sec,
                        total_clip_sec,
                    );
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

                let token_string = initialized.tokenizer.id_to_token(best_id);
                let current_duration = (frame - cue_start_frame + 1) as f64 * frame_sec;
                if current_duration >= 1.0 && ends_with_japanese_sentence_punctuation(&token_string)
                {
                    flush_ctc_cue(
                        initialized,
                        callbacks,
                        &mut current_tokens,
                        &mut cue_start_frame,
                        last_non_blank_frame,
                        frame_sec,
                        total_clip_sec,
                    );
                }
            } else {
                last_non_blank_frame = frame;
            }
            prev_token = best_id;
        }

        flush_ctc_cue(
            initialized,
            callbacks,
            &mut current_tokens,
            &mut cue_start_frame,
            last_non_blank_frame,
            frame_sec,
            total_clip_sec,
        );
        Ok(RESULT_SUCCESS)
    }
}

/// C++ `flushCue` lambda inside `transcribeCtc`.
fn flush_ctc_cue(
    initialized: &Initialized,
    callbacks: &Callbacks,
    current_tokens: &mut Vec<i64>,
    cue_start_frame: &mut i64,
    last_non_blank_frame: i64,
    frame_sec: f64,
    total_clip_sec: f64,
) {
    if current_tokens.is_empty() || *cue_start_frame < 0 || last_non_blank_frame < *cue_start_frame {
        current_tokens.clear();
        *cue_start_frame = -1;
        return;
    }
    let text = initialized
        .tokenizer
        .decode(current_tokens, true)
        .trim()
        .to_string();
    if !text.is_empty() && callbacks.segment.is_some() {
        let start_sec = 0.0f64.max(*cue_start_frame as f64 * frame_sec - 0.04);
        let end_sec = total_clip_sec.min(
            (start_sec + 0.16).max((last_non_blank_frame + 1) as f64 * frame_sec + 0.08),
        );
        callbacks.segment(&text, start_sec, end_sec);
    }
    current_tokens.clear();
    *cue_start_frame = -1;
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

/// `static_cast<int>(double)` as produced by x86-64 `cvttsd2si`: NaN and
/// out-of-range values yield `INT_MIN` (the "integer indefinite" result).
fn double_to_i32_x86(value: f64) -> i32 {
    let truncated = value.trunc();
    if truncated < i32::MIN as f64 || truncated > i32::MAX as f64 {
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

fn path_to_cstring(path: &Path) -> Result<CString, String> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        CString::new(path.as_os_str().as_bytes())
            .map_err(|_| "ONNX model path contains a NUL byte".to_string())
    }
    #[cfg(not(unix))]
    {
        CString::new(path.to_string_lossy().as_bytes().to_vec())
            .map_err(|_| "ONNX model path contains a NUL byte".to_string())
    }
}

fn cstring_ptrs(names: &[CString]) -> Vec<*const c_char> {
    names.iter().map(|name| name.as_ptr()).collect()
}

/// RAII wrapper over `OrtEnv*`. The underlying environment is a process-wide
/// singleton in ONNX Runtime (mirroring the C++ `sharedOrtEnv()` function-local
/// static) and is intentionally never released.
struct EnvHandle(*mut OrtEnv);
unsafe impl Send for EnvHandle {}
unsafe impl Sync for EnvHandle {}

static API: OnceLock<ApiHandle> = OnceLock::new();
static ENV: OnceLock<EnvHandle> = OnceLock::new();

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

/// C++ `sharedOrtEnv()`.
fn get_env(_api: &'static OrtApi) -> Result<*const OrtEnv, String> {
    if let Some(env) = ENV.get() {
        return Ok(env.0);
    }
    let log_id = b"MementoOnnxAsr\0";
    let mut env: *mut OrtEnv = std::ptr::null_mut();
    check(unsafe {
        (api().CreateEnv)(
            ORT_LOGGING_LEVEL_WARNING,
            log_id.as_ptr() as *const c_char,
            &mut env,
        )
    })?;
    let handle = ENV.get_or_init(|| EnvHandle(env));
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

/// C++ `Ort::GetAvailableProviders()`.
fn available_providers(_api: &'static OrtApi) -> Result<Vec<String>, String> {
    let api = api();
    let mut providers: *mut *mut c_char = std::ptr::null_mut();
    let mut count: c_int = 0;
    check(unsafe { (api.GetAvailableProviders)(&mut providers, &mut count) })?;
    let count_usize = count.max(0) as usize;
    let mut result = Vec::with_capacity(count_usize);
    for index in 0..count_usize {
        let provider = unsafe { *providers.add(index) };
        if !provider.is_null() {
            result.push(unsafe { CStr::from_ptr(provider) }.to_string_lossy().into_owned());
        }
    }
    check(unsafe { (api.ReleaseAvailableProviders)(providers, count) })?;
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
/// so it must not be released separately.
struct BorrowedTensorShapeInfoHandle(*const OrtTensorTypeAndShapeInfo);

impl TensorShapeInfoAccessors for BorrowedTensorShapeInfoHandle {
    fn shape_info_ptr(&self) -> *const OrtTensorTypeAndShapeInfo {
        self.0
    }
}

/// C++ `Ort::TypeInfo` (RAII; the C++ API view leaks it).
struct TypeInfoHandle(*mut OrtTypeInfo);

impl TypeInfoHandle {
    /// C++ `TypeInfo::GetTensorTypeAndShapeInfo()` (borrowed view; stays
    /// valid as long as this TypeInfoHandle is alive).
    fn tensor_shape_info(&self) -> Result<BorrowedTensorShapeInfoHandle, String> {
        let mut shape_info: *const OrtTensorTypeAndShapeInfo = std::ptr::null_mut();
        check(unsafe { (api().CastTypeInfoToTensorInfo)(self.0, &mut shape_info) })?;
        if shape_info.is_null() {
            return Err("ONNX Runtime type info is not a tensor".to_string());
        }
        Ok(BorrowedTensorShapeInfoHandle(shape_info))
    }
}

impl Drop for TypeInfoHandle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { (api().ReleaseTypeInfo)(self.0) };
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
        path: &CStr,
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
        let result = unsafe { CStr::from_ptr(name) }.to_string_lossy().into_owned();
        check(unsafe { (api().AllocatorFree)(allocator.0, name as *mut c_void) })?;
        Ok(result)
    }

    /// C++ `Session::GetOutputNameAllocated(i, allocator)`.
    fn output_name(&self, index: usize, allocator: &AllocatorHandle) -> Result<String, String> {
        let mut name: *mut c_char = std::ptr::null_mut();
        check(unsafe { (api().SessionGetOutputName)(self.0, index, allocator.0, &mut name) })?;
        let result = unsafe { CStr::from_ptr(name) }.to_string_lossy().into_owned();
        check(unsafe { (api().AllocatorFree)(allocator.0, name as *mut c_void) })?;
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
    let mut outputs: Vec<*mut OrtValue> = vec![std::ptr::null_mut(); output_names.len()];
    check(unsafe {
        (api().Run)(
            session.0,
            std::ptr::null(),
            input_names.as_ptr(),
            inputs.as_ptr(),
            inputs.len(),
            output_names.as_ptr(),
            output_names.len(),
            outputs.as_mut_ptr(),
        )
    })?;
    Ok(outputs.into_iter().map(ValueHandle).collect())
}

//! Port of Memento's ONNX ASR inference engine (`src/asr/onnx`).
//!
//! This crate composes the already-ported primitives from `memento-asr`
//! (model config discovery, tokenizer, Mel frontend, WAV reader) into an
//! inference engine equivalent to the C++ `OnnxSession`, exposed to C++ via a
//! C ABI (see the module added by the ONNX session port).

pub mod capi;
pub mod engine;
pub mod ort_sys;

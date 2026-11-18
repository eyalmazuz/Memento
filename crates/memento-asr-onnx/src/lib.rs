//! Port of Memento's ONNX ASR inference engine (`src/asr/onnx`).
//!
//! This crate composes the already-ported primitives from `memento-asr`
//! (model config discovery, tokenizer, Mel frontend, WAV reader) into an
//! inference engine equivalent to the C++ `OnnxSession`.

pub mod engine;
pub mod ort_sys;

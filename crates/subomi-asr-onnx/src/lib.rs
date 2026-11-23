//! ONNX Runtime speech-to-text inference backend.
//!
//! Composes the primitives from `subomi-asr` (model config discovery,
//! tokenizer, Mel frontend, WAV reader) into an ONNX Runtime inference engine.

pub mod engine;
pub mod ort_sys;

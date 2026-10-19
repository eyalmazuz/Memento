//! Facade staticlib for the Memento ASR Rust engines.
//!
//! The C++ application links exactly one Rust archive. Engine crates are
//! plain rlibs; this staticlib embeds the base crate and every engine exactly
//! once, so linking it next to any C++ object code resolves the `asr_rust_*`,
//! `asr_onnx_*`, and `asr_whisper_*` C ABI symbols without duplicate
//! definitions. Corrosion imports only this crate's static artifact.

// Re-export the engine crates so their code is part of this crate's build
// graph and their #[no_mangle] C ABI exports remain reachable from the
// archive the C++ link consumes.
pub use memento_asr_onnx;
pub use memento_asr_whisper;

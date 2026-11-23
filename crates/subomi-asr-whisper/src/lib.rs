//! Whisper speech-to-text inference backend over `whisper.cpp`.
//!
//! Layout:
//! - [`engine`]: WAV decode, Whisper parameter configuration, segment decoding,
//!   and abort handling.
//! - [`whisper_sys`]: bindgen bindings to `whisper.h` (`build.rs`).

pub mod engine;
pub mod whisper_sys;

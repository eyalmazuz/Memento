//! Rust port of Memento's ASR module (`src/asr`), converted file-by-file to
//! mirror the C++ implementation for review parity.
//!
//! Numerical contract: where the C++ code folds f32/f64 arithmetic (audio
//! frontend, FFT), this crate performs identical operations in identical
//! order and is cross-verified bit-exact against the compiled C++
//! implementation by golden-vector tests before any consumer is switched
//! over. See `docs/rust-port-plan.md` for the module roadmap.

/// Port of `asr/asrcontroller.cpp`'s subtitle cue book and window/seek
/// planning.
pub mod asrcontroller;

pub mod audio {
    /// Port of `asr/audio/melfrontend.{h,cpp}`
    pub mod melfrontend;
    /// Port of `asr/audio/wavreader.{h,cpp}`
    pub mod wavreader;
}

pub mod whisper {
    /// Port of `asr/whisper/whispercontroller.{h,cpp}` — registry/path/URL
    /// pure core (the QObject stays C++).
    pub mod whispercontroller;
}

pub mod onnx {
    /// Port of `asr/onnx/onnxtokenizer.{h,cpp}`
    pub mod onnxtokenizer;
    /// Port of `asr/onnx/onnxmodelconfig.{h,cpp}`
    pub mod onnxmodelconfig;
    /// Port of `asr/onnx/onnxcontroller.{h,cpp}` — preset/dir/availability
    /// pure core.
    pub mod onnxcontroller;
}

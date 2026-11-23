//! Real-time speech recognition pipeline, audio frontend, and ONNX tokenizer.

/// Subtitle segment clamping and cue validation helpers.
pub mod asrcontroller;

pub mod audio {
    /// Log-mel spectrogram frontend for ASR inference.
    pub mod melfrontend;
    /// PCM-16 WAV reader.
    pub mod wavreader;
}

pub mod whisper {
    /// Whisper model registry, path resolution, and URL helpers.
    pub mod whispercontroller;
}

pub mod onnx {
    pub(crate) mod json;
    /// Hugging Face `tokenizer.json` BPE / Unigram tokenizer and detokenizer.
    pub mod onnxtokenizer;
    /// ONNX ASR model configuration discovery and path resolution.
    pub mod onnxmodelconfig;
    /// ONNX model preset registry, directory, and availability helpers.
    pub mod onnxcontroller;
}

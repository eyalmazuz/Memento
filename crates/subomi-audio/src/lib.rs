//! Rust cores for audio source expansion, JSON payloads, download timeouts, and cache decisions.

pub mod audiofiles;
pub mod audioplayer;
pub mod sources;

pub use sources::{
    audio_download_timeout_from_ms, clamp_audio_download_timeout, AudioDownloadConfig,
    DEFAULT_AUDIO_CONNECT_TIMEOUT, DEFAULT_AUDIO_DOWNLOAD_TIMEOUT,
    DEFAULT_AUDIO_DOWNLOAD_TIMEOUT_MS, MAX_AUDIO_DOWNLOAD_TIMEOUT, MAX_AUDIO_DOWNLOAD_TIMEOUT_MS,
    MIN_AUDIO_DOWNLOAD_TIMEOUT, MIN_AUDIO_DOWNLOAD_TIMEOUT_MS,
};

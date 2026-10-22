//! Controllers connecting backend crates to Slint UI models.

pub mod audio;
#[cfg(feature = "ocr")]
pub mod ocr;
pub mod player;
pub mod subtitles;
pub mod dict;
pub mod dictmanager;
pub mod anki;
pub mod anki_media;
pub mod asr;
pub mod media;
pub mod config;

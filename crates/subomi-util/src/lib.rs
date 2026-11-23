//! Japanese furigana, MeCab integration, OCR region mapping, and file utilities.

pub mod furiganautils;
pub use furiganautils::{
    contains_kanji, distribute_furigana_str, is_kanji_char, simplify_surface_str, FuriganaSpan,
};

pub mod furiganaparser;

pub mod mecab;

pub mod ocrutils;

pub mod fileutils;

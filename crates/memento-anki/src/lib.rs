//! Pure Rust Anki note building, marker tokenization, glossary formatting,
//! and AnkiConnect reply validation routines.

pub mod client;
pub mod glossary;
pub mod json;
pub mod note;
pub mod strings;
pub mod tokenizer;

pub use strings::{token_spans, TokenSpan};
pub use tokenizer::{tokenize, Marker, Token};

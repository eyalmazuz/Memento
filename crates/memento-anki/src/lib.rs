//! Pure Rust algorithms from `src/anki` for building Anki notes. The
//! tokenizer's Stage 1 C ABI is the only production-wired Rust path; the
//! metadata C ABI allows Qt/compiled-original parity testing but is not used
//! by `Anki::Note::build`. Glossary and field substitution algorithms are not
//! application-wired either. Strings remain UTF-16 as in QString.
//!
//! The application still uses C++ as its reference implementation. Each
//! Rust-owned ABI buffer/tree has a paired Rust free function.

pub mod strings;
pub mod tokenizer;
pub mod capi;
pub mod json;
pub mod glossary;
pub mod glossary_capi;
pub mod note;
pub mod note_capi;
pub mod client;
pub mod client_capi;

pub use strings::{token_spans, TokenSpan};
pub use tokenizer::{tokenize, Marker, Token};

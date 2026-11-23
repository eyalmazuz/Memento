//! Subtitle text and timing owned by the parser core.

#[derive(Clone, Debug, PartialEq)]
pub struct SubtitleEntry {
    pub text: String,
    pub start: f64,
    pub end: f64,
}

//! Subtitle value objects, model planning, and ASS/SRT/WebVTT file parsing.

pub mod model;
pub mod subtitleentry;
pub mod subtitleparser;

pub use model::{
    collect_overlapping_spans, diff_items, find_first_overlapping_span, find_matching_rows,
    plan_add_subtitle, same_entry, select_position_rows, AddAction, AddPlan, CandidateCue,
    ModelDiffPlan, SubtitleCueView, SubtitleSpan, MAX_NEWLINE_COUNT, MAX_TEXT_LENGTH, TIME_DELTA,
};
pub use subtitleentry::SubtitleEntry;
pub use subtitleparser::{parse_subtitles, Format};

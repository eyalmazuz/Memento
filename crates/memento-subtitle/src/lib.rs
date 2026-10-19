//! Subtitle value objects, file-format parsing and the opt-in Qt boundary.

pub mod subtitleentry;
pub mod subtitleparser;
pub mod model;
pub mod capi;

pub use subtitleentry::SubtitleEntry;
pub use subtitleparser::{parse_subtitles, Format};
pub use model::{
    collect_overlapping_spans, diff_items, find_first_overlapping_span, plan_add_subtitle,
    same_entry, select_position_rows, AddAction, AddPlan, ModelDiffPlan, SubtitleCueView,
    SubtitleSpanC, MAX_NEWLINE_COUNT, MAX_TEXT_LENGTH, TIME_DELTA,
};


//! Full ASS/SSA subtitle support core for Pure-Rust Subomi (Approach B+).
//!
//! Architecture overview:
//! - `mpv` renders ASS/SSA tracks natively into the video framebuffer for 100% pixel parity,
//!   GPU-synced animations, native `{screenshot}` vs `{screenshot-video}`, and embedded fonts.
//! - [`AssLayoutEngine`] wraps the same system `libass` shared library to:
//!   1. Compute exact per-event / per-character screen-space bounding boxes via a single
//!      color-ID-instrumented `ass_render_frame` probe (~0.2 ms on cue change).
//!   2. Hit-test pointer coordinates in O(log N) to drive hover/modifier/middle-click/left-click
//!      dictionary lookup on any positioned or simultaneous ASS line.
//!   3. Render tight cropped RGBA highlight patches (~0.15 ms) so white ASS glyphs + black
//!      outlines sit crisply on top of Subomi's solid `#38a6d6` selection bar.
//! - [`mkv`] provides a fast Pure-Rust EBML Matroska reader for extracting embedded font
//!   attachments (for `ass_add_font`) and complete embedded subtitle tracks (for the Subtitle List).

pub mod engine;
pub mod ffi;
pub mod mkv;
pub mod parser;

pub use engine::{
    AssActiveEvent, AssCharBox, AssFrameLayout, AssHighlightRect, AssHinting, AssHitTestResult,
    AssLayoutEngine, AssOverrideMode, AssShapingLevel, AssVisualLine, MpvAssParams, RgbaPatch,
    ViewportTransform,
};
pub use mkv::{
    extract_mkv_fonts, extract_mkv_subtitle_track, list_mkv_subtitle_tracks,
    ExtractedSubtitleTrack, MkvFontAttachment, MkvSubtitleCodec, MkvSubtitleCue,
    MkvSubtitleTrackInfo,
};
pub use parser::{
    build_probe_script, format_ass_timecode, is_japanese_or_alnum_lookup_char,
    parse_ass_timecode_ms, parse_events_format, parse_script_info, strip_ass_dialogue_text,
    AssDialogueLine, AssScriptInfo, ParsedChar, ParsedEvent, ProbeScript,
};

/// Returns the runtime `libass` version integer (`0xMMmmpp00`, e.g. `0x01705000` for 0.17.5).
pub fn libass_version() -> i32 {
    unsafe { ffi::ass_library_version() }
}

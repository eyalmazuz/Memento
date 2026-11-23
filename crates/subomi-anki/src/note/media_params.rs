//! Media marker argument parsing and clip range calculations.

use crate::tokenizer::Marker;

/// Timing source for media clips.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TimingSource {
    Subtitle,
    Context,
}

/// Parameters used to group `{audio-media}` fields that share one clip.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AudioMediaParams {
    pub source: TimingSource,
    pub normalize: bool,
    pub db: f64,
}

/// Parameters used to group screenshot fields that share one captured image.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScreenshotParams {
    pub max_width: i32,
    pub max_height: i32,
    pub keep_aspect_ratio: bool,
}

/// Parameters used to group `{video}` fields that share one video clip.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VideoParams {
    pub source: TimingSource,
    pub audio: bool,
    pub subtitles: bool,
    pub normalize: bool,
    pub db: f64,
}

fn parse_timing_source(value: &str) -> Option<TimingSource> {
    match value {
        "subtitle" => Some(TimingSource::Subtitle),
        "context" => Some(TimingSource::Context),
        _ => None,
    }
}

/// Every non-empty value is truthy except `"0"` and case-insensitive `"false"`.
fn marker_bool(value: &str) -> bool {
    !value.is_empty() && value != "0" && !value.eq_ignore_ascii_case("false")
}

fn parse_marker_i32(value: &str) -> Option<i32> {
    value.trim().parse::<i32>().ok()
}

fn parse_marker_f64(value: &str) -> Option<f64> {
    value
        .trim()
        .parse::<f64>()
        .ok()
        .filter(|parsed| parsed.is_finite())
}

fn marker_source(marker: &Marker) -> Option<TimingSource> {
    marker
        .arg("source")
        .map(parse_timing_source)
        .unwrap_or(Some(TimingSource::Subtitle))
}

fn marker_db(marker: &Marker, default: f64) -> Option<f64> {
    marker
        .arg("db")
        .map(parse_marker_f64)
        .unwrap_or(Some(default))
}

/// Parses `{audio-media}` marker arguments, using profile defaults when `norm`
/// or `db` is omitted. Returns `None` if `source` or `db` is invalid.
pub fn audio_media_params(
    marker: &Marker,
    profile_normalize: bool,
    profile_db: f64,
) -> Option<AudioMediaParams> {
    Some(AudioMediaParams {
        source: marker_source(marker)?,
        normalize: marker
            .arg("norm")
            .map(marker_bool)
            .unwrap_or(profile_normalize),
        db: marker_db(marker, profile_db)?,
    })
}

/// Parses `{audio-context}` arguments before forcing context timing.
pub fn audio_context_params(
    marker: &Marker,
    profile_normalize: bool,
    profile_db: f64,
) -> Option<AudioMediaParams> {
    let mut params = audio_media_params(marker, profile_normalize, profile_db)?;
    params.source = TimingSource::Context;
    Some(params)
}

/// Parses `{screenshot}` and `{screenshot-video}` sizing arguments.
pub fn screenshot_params(marker: &Marker) -> Option<ScreenshotParams> {
    Some(ScreenshotParams {
        max_width: marker
            .arg("max-width")
            .map(parse_marker_i32)
            .unwrap_or(Some(-1))?,
        max_height: marker
            .arg("max-height")
            .map(parse_marker_i32)
            .unwrap_or(Some(-1))?,
        keep_aspect_ratio: marker.arg("keep-ratio").map(marker_bool).unwrap_or(true),
    })
}

/// Parses `{video}` marker arguments, using profile normalization defaults
/// when `norm` or `db` is omitted.
pub fn video_params(
    marker: &Marker,
    profile_normalize: bool,
    profile_db: f64,
) -> Option<VideoParams> {
    Some(VideoParams {
        source: marker_source(marker)?,
        audio: marker.arg("audio").map(marker_bool).unwrap_or(true),
        subtitles: marker.arg("subtitles").map(marker_bool).unwrap_or(true),
        normalize: marker
            .arg("norm")
            .map(marker_bool)
            .unwrap_or(profile_normalize),
        db: marker_db(marker, profile_db)?,
    })
}

/// Computes padded `[start, end)` clip timing, clamping negative values to `0.0`
/// and returning `None` if the resulting interval is invalid or empty.
pub fn compute_clip_range(
    source: TimingSource,
    sub_start: f64,
    sub_end: f64,
    ctx_start: f64,
    ctx_end: f64,
    pad_start: f64,
    pad_end: f64,
) -> Option<(f64, f64)> {
    let (raw_start, raw_end) = match source {
        TimingSource::Subtitle => (sub_start - pad_start, sub_end + pad_end),
        TimingSource::Context => (ctx_start - pad_start, ctx_end + pad_end),
    };
    let start = if raw_start < 0.0 { 0.0 } else { raw_start };
    let end = if raw_end < 0.0 { 0.0 } else { raw_end };
    (start < end).then_some((start, end))
}

//! Media marker argument parsing and clip range calculations.

use crate::glossary::escape::{equals_ascii, equals_ascii_ignore_case};
use crate::strings::trim_u16;
use crate::tokenizer::Marker;

/// Timing source for media clips.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum TimingSource {
    Subtitle = 0,
    Context = 1,
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

fn marker_arg<'a>(marker: &'a Marker, key: &str) -> Option<&'a [u16]> {
    marker
        .args
        .iter()
        .find_map(|(arg_key, value)| equals_ascii(arg_key, key).then_some(value.as_slice()))
}

fn parse_timing_source(value: &[u16]) -> Option<TimingSource> {
    if equals_ascii(value, "subtitle") {
        Some(TimingSource::Subtitle)
    } else if equals_ascii(value, "context") {
        Some(TimingSource::Context)
    } else {
        None
    }
}

/// `QVariant(QString).toBool()`: every nonempty string is true except `0` and
/// case-insensitive `false`.
fn marker_bool(value: &[u16]) -> bool {
    !value.is_empty() && !equals_ascii(value, "0") && !equals_ascii_ignore_case(value, "false")
}

fn parse_marker_i32(value: &[u16]) -> Option<i32> {
    let value = trim_u16(value);
    let (negative, digits) = match value.first().copied() {
        Some(0x002b) => (false, &value[1..]),
        Some(0x002d) => (true, &value[1..]),
        _ => (false, value),
    };
    if digits.is_empty() {
        return None;
    }

    let limit = if negative {
        i32::MAX as u64 + 1
    } else {
        i32::MAX as u64
    };
    let mut magnitude = 0u64;
    for &unit in digits {
        if !(b'0' as u16..=b'9' as u16).contains(&unit) {
            return None;
        }
        magnitude = magnitude
            .checked_mul(10)?
            .checked_add((unit - b'0' as u16) as u64)?;

        if magnitude > limit {
            return None;
        }
    }

    if negative {
        if magnitude == i32::MAX as u64 + 1 {
            Some(i32::MIN)
        } else {
            Some(-(magnitude as i32))
        }
    } else {
        Some(magnitude as i32)
    }
}

fn parse_marker_f64(value: &[u16]) -> Option<f64> {
    let trimmed = trim_u16(value);
    if trimmed.is_empty() || trimmed.len() > 64 {
        return None;
    }
    let mut buf = [0u8; 64];
    for (dst, &unit) in buf[..trimmed.len()].iter_mut().zip(trimmed) {
        if unit > 0x7f {
            return None;
        }
        *dst = unit as u8;
    }
    let parsed: f64 = std::str::from_utf8(&buf[..trimmed.len()]).ok()?.parse().ok()?;
    parsed.is_finite().then_some(parsed)
}

fn marker_source(marker: &Marker) -> Option<TimingSource> {
    marker_arg(marker, "source")
        .map(parse_timing_source)
        .unwrap_or(Some(TimingSource::Subtitle))
}

fn marker_db(marker: &Marker, default: f64) -> Option<f64> {
    marker_arg(marker, "db")
        .map(parse_marker_f64)
        .unwrap_or(Some(default))
}

/// Parses the `{audio-media}` marker's arguments, using profile defaults when
/// `norm` or `db` is omitted. Invalid `source` or `db` leaves the marker
/// unhandled, matching `getAudioMediaParams` in `notebuilder.cpp`.
pub fn audio_media_params(
    marker: &Marker,
    profile_normalize: bool,
    profile_db: f64,
) -> Option<AudioMediaParams> {
    Some(AudioMediaParams {
        source: marker_source(marker)?,
        normalize: marker_arg(marker, "norm")
            .map(marker_bool)
            .unwrap_or(profile_normalize),
        db: marker_db(marker, profile_db)?,
    })
}

/// Parses `{audio-context}` arguments before forcing context timing. An
/// explicitly invalid `source` remains invalid, as in the C++ call order.
pub fn audio_context_params(
    marker: &Marker,
    profile_normalize: bool,
    profile_db: f64,
) -> Option<AudioMediaParams> {
    let mut params = audio_media_params(marker, profile_normalize, profile_db)?;
    params.source = TimingSource::Context;
    Some(params)
}

/// Parses the `{screenshot}` and `{screenshot-video}` sizing arguments.
pub fn screenshot_params(marker: &Marker) -> Option<ScreenshotParams> {
    Some(ScreenshotParams {
        max_width: marker_arg(marker, "max-width")
            .map(parse_marker_i32)
            .unwrap_or(Some(-1))?,
        max_height: marker_arg(marker, "max-height")
            .map(parse_marker_i32)
            .unwrap_or(Some(-1))?,
        keep_aspect_ratio: marker_arg(marker, "keep-ratio")
            .map(marker_bool)
            .unwrap_or(true),
    })
}

/// Parses the `{video}` marker's arguments, using profile normalization
/// defaults when `norm` or `db` is omitted.
pub fn video_params(
    marker: &Marker,
    profile_normalize: bool,
    profile_db: f64,
) -> Option<VideoParams> {
    Some(VideoParams {
        source: marker_source(marker)?,
        audio: marker_arg(marker, "audio").map(marker_bool).unwrap_or(true),
        subtitles: marker_arg(marker, "subtitles")
            .map(marker_bool)
            .unwrap_or(true),
        normalize: marker_arg(marker, "norm")
            .map(marker_bool)
            .unwrap_or(profile_normalize),
        db: marker_db(marker, profile_db)?,
    })
}

/// Matches `createAudioMediaHelper` / `createVideoHelper` timing computation in `notebuilder.cpp`.
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
    // std::max(raw, 0.0) in C++ keeps an unordered operand (NaN), whereas
    // f64::max discards it and could turn an invalid start into zero.
    let start = if raw_start < 0.0 { 0.0 } else { raw_start };
    let end = if raw_end < 0.0 { 0.0 } else { raw_end };
    if start < end {
        Some((start, end))
    } else {
        None
    }
}

//! Cached libmpv property snapshot and single-pass `mpv_event_property` decoder.

use std::ffi::{c_char, c_int, CStr};
use std::ptr;

use super::extract::{
    extract_chapters_from_node, extract_osd_dimensions_from_node, extract_sub_lines_from_node,
    extract_tracks_from_node,
};
use super::ffi;
use super::node::copy_mpv_string;
use super::types::{
    AudioTrack, CursorAutohide, MpvPropertyValue, OsdDimensions, SubLine, SubtitleTrack, TrackType,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ObservedProperty {
    ChapterList,
    Chapters,
    Path,
    MediaTitle,
    TimePos,
    Duration,
    Pause,
    Fullscreen,
    Mute,
    VolumeMax,
    Volume,
    Dwidth,
    Dheight,
    VideoParamsW,
    VideoParamsH,
    OsdDimensions,
    Aid,
    Sid,
    SecondarySid,
    Vid,
    SubStart,
    SubEnd,
    SubText,
    SubAssExtradata,
    SubTextAssFull,
    SubScale,
    SubPos,
    SubFont,
    SubAssOverride,
    SubScaleSigns,
    SubUseMargins,
    SubHinting,
    SubShaper,
    SubDelay,
    SubSpeed,
    SubVisibility,
    SecondarySubStart,
    SecondarySubEnd,
    SecondarySubText,
    SecondarySubDelay,
    SecondarySubVisibility,
    TrackList,
    SubLines,
    FrameDropCount,
    DecoderFrameDropCount,
    HwdecCurrent,
    CursorAutohide,
    CursorAutohideFsOnly,
}

impl ObservedProperty {
    pub(super) fn from_bytes(name: &[u8]) -> Option<Self> {
        match name {
            b"chapter-list" => Some(Self::ChapterList),
            b"chapters" => Some(Self::Chapters),
            b"path" => Some(Self::Path),
            b"media-title" => Some(Self::MediaTitle),
            b"time-pos" => Some(Self::TimePos),
            b"duration" => Some(Self::Duration),
            b"pause" => Some(Self::Pause),
            b"fullscreen" => Some(Self::Fullscreen),
            b"mute" => Some(Self::Mute),
            b"volume-max" => Some(Self::VolumeMax),
            b"volume" => Some(Self::Volume),
            b"dwidth" => Some(Self::Dwidth),
            b"dheight" => Some(Self::Dheight),
            b"video-params/w" => Some(Self::VideoParamsW),
            b"video-params/h" => Some(Self::VideoParamsH),
            b"osd-dimensions" => Some(Self::OsdDimensions),
            b"aid" => Some(Self::Aid),
            b"sid" => Some(Self::Sid),
            b"secondary-sid" => Some(Self::SecondarySid),
            b"vid" => Some(Self::Vid),
            b"sub-start" => Some(Self::SubStart),
            b"sub-end" => Some(Self::SubEnd),
            b"sub-text" => Some(Self::SubText),
            b"sub-ass-extradata" => Some(Self::SubAssExtradata),
            b"sub-text/ass-full" => Some(Self::SubTextAssFull),
            b"sub-scale" => Some(Self::SubScale),
            b"sub-pos" => Some(Self::SubPos),
            b"sub-font" => Some(Self::SubFont),
            b"sub-ass-override" => Some(Self::SubAssOverride),
            b"sub-scale-signs" => Some(Self::SubScaleSigns),
            b"sub-use-margins" => Some(Self::SubUseMargins),
            b"sub-hinting" => Some(Self::SubHinting),
            b"sub-shaper" => Some(Self::SubShaper),
            b"sub-delay" => Some(Self::SubDelay),
            b"sub-speed" => Some(Self::SubSpeed),
            b"sub-visibility" => Some(Self::SubVisibility),
            b"secondary-sub-start" => Some(Self::SecondarySubStart),
            b"secondary-sub-end" => Some(Self::SecondarySubEnd),
            b"secondary-sub-text" => Some(Self::SecondarySubText),
            b"secondary-sub-delay" => Some(Self::SecondarySubDelay),
            b"secondary-sub-visibility" => Some(Self::SecondarySubVisibility),
            b"track-list" => Some(Self::TrackList),
            b"sub-lines" => Some(Self::SubLines),
            b"frame-drop-count" => Some(Self::FrameDropCount),
            b"decoder-frame-drop-count" => Some(Self::DecoderFrameDropCount),
            b"hwdec-current" => Some(Self::HwdecCurrent),
            b"cursor-autohide" => Some(Self::CursorAutohide),
            b"cursor-autohide-fs-only" => Some(Self::CursorAutohideFsOnly),
            _ => None,
        }
    }
}

pub(super) struct ObservedPropertySpec {
    pub(super) c_name: &'static CStr,
    pub(super) name: &'static str,
    pub(super) kind: ObservedProperty,
    pub(super) format: ffi::mpv_format,
}

macro_rules! prop_spec {
    ($c_name:literal, $name:literal, $kind:ident, $format:expr) => {
        ObservedPropertySpec {
            c_name: $c_name,
            name: $name,
            kind: ObservedProperty::$kind,
            format: $format,
        }
    };
}

pub(super) const PLAYBACK_PROPERTIES: &[ObservedPropertySpec] = &[
    prop_spec!(c"chapter-list", "chapter-list", ChapterList, ffi::MPV_FORMAT_NODE),
    prop_spec!(c"chapters", "chapters", Chapters, ffi::MPV_FORMAT_INT64),
    prop_spec!(c"path", "path", Path, ffi::MPV_FORMAT_STRING),
    prop_spec!(c"media-title", "media-title", MediaTitle, ffi::MPV_FORMAT_STRING),
    prop_spec!(c"time-pos", "time-pos", TimePos, ffi::MPV_FORMAT_DOUBLE),
    prop_spec!(c"duration", "duration", Duration, ffi::MPV_FORMAT_DOUBLE),
    prop_spec!(c"pause", "pause", Pause, ffi::MPV_FORMAT_FLAG),
    prop_spec!(c"fullscreen", "fullscreen", Fullscreen, ffi::MPV_FORMAT_FLAG),
    prop_spec!(c"mute", "mute", Mute, ffi::MPV_FORMAT_FLAG),
    prop_spec!(c"volume-max", "volume-max", VolumeMax, ffi::MPV_FORMAT_INT64),
    prop_spec!(c"volume", "volume", Volume, ffi::MPV_FORMAT_DOUBLE),
    prop_spec!(c"dwidth", "dwidth", Dwidth, ffi::MPV_FORMAT_INT64),
    prop_spec!(c"dheight", "dheight", Dheight, ffi::MPV_FORMAT_INT64),
    prop_spec!(c"video-params/w", "video-params/w", VideoParamsW, ffi::MPV_FORMAT_INT64),
    prop_spec!(c"video-params/h", "video-params/h", VideoParamsH, ffi::MPV_FORMAT_INT64),
    prop_spec!(c"osd-dimensions", "osd-dimensions", OsdDimensions, ffi::MPV_FORMAT_NODE),
    prop_spec!(c"aid", "aid", Aid, ffi::MPV_FORMAT_INT64),
    prop_spec!(c"aid", "aid", Aid, ffi::MPV_FORMAT_FLAG),
    prop_spec!(c"sid", "sid", Sid, ffi::MPV_FORMAT_INT64),
    prop_spec!(c"sid", "sid", Sid, ffi::MPV_FORMAT_FLAG),
    prop_spec!(c"secondary-sid", "secondary-sid", SecondarySid, ffi::MPV_FORMAT_INT64),
    prop_spec!(c"secondary-sid", "secondary-sid", SecondarySid, ffi::MPV_FORMAT_FLAG),
    prop_spec!(c"vid", "vid", Vid, ffi::MPV_FORMAT_INT64),
    prop_spec!(c"vid", "vid", Vid, ffi::MPV_FORMAT_FLAG),
    prop_spec!(c"sub-start", "sub-start", SubStart, ffi::MPV_FORMAT_DOUBLE),
    prop_spec!(c"sub-end", "sub-end", SubEnd, ffi::MPV_FORMAT_DOUBLE),
    prop_spec!(c"sub-text", "sub-text", SubText, ffi::MPV_FORMAT_STRING),
    prop_spec!(c"sub-ass-extradata", "sub-ass-extradata", SubAssExtradata, ffi::MPV_FORMAT_STRING),
    prop_spec!(c"sub-text/ass-full", "sub-text/ass-full", SubTextAssFull, ffi::MPV_FORMAT_STRING),
    prop_spec!(c"sub-scale", "sub-scale", SubScale, ffi::MPV_FORMAT_DOUBLE),
    prop_spec!(c"sub-pos", "sub-pos", SubPos, ffi::MPV_FORMAT_DOUBLE),
    prop_spec!(c"sub-font", "sub-font", SubFont, ffi::MPV_FORMAT_STRING),
    prop_spec!(c"sub-ass-override", "sub-ass-override", SubAssOverride, ffi::MPV_FORMAT_STRING),
    prop_spec!(c"sub-scale-signs", "sub-scale-signs", SubScaleSigns, ffi::MPV_FORMAT_FLAG),
    prop_spec!(c"sub-use-margins", "sub-use-margins", SubUseMargins, ffi::MPV_FORMAT_FLAG),
    prop_spec!(c"sub-hinting", "sub-hinting", SubHinting, ffi::MPV_FORMAT_STRING),
    prop_spec!(c"sub-shaper", "sub-shaper", SubShaper, ffi::MPV_FORMAT_STRING),
    prop_spec!(c"sub-delay", "sub-delay", SubDelay, ffi::MPV_FORMAT_DOUBLE),
    prop_spec!(c"sub-speed", "sub-speed", SubSpeed, ffi::MPV_FORMAT_DOUBLE),
    prop_spec!(c"sub-visibility", "sub-visibility", SubVisibility, ffi::MPV_FORMAT_FLAG),
    prop_spec!(c"secondary-sub-start", "secondary-sub-start", SecondarySubStart, ffi::MPV_FORMAT_DOUBLE),
    prop_spec!(c"secondary-sub-end", "secondary-sub-end", SecondarySubEnd, ffi::MPV_FORMAT_DOUBLE),
    prop_spec!(c"secondary-sub-text", "secondary-sub-text", SecondarySubText, ffi::MPV_FORMAT_STRING),
    prop_spec!(c"secondary-sub-delay", "secondary-sub-delay", SecondarySubDelay, ffi::MPV_FORMAT_DOUBLE),
    prop_spec!(c"secondary-sub-visibility", "secondary-sub-visibility", SecondarySubVisibility, ffi::MPV_FORMAT_FLAG),
    prop_spec!(c"track-list", "track-list", TrackList, ffi::MPV_FORMAT_NODE),
    prop_spec!(c"sub-lines", "sub-lines", SubLines, ffi::MPV_FORMAT_NODE),
    prop_spec!(c"frame-drop-count", "frame-drop-count", FrameDropCount, ffi::MPV_FORMAT_INT64),
    prop_spec!(c"decoder-frame-drop-count", "decoder-frame-drop-count", DecoderFrameDropCount, ffi::MPV_FORMAT_INT64),
    prop_spec!(c"hwdec-current", "hwdec-current", HwdecCurrent, ffi::MPV_FORMAT_STRING),
    prop_spec!(c"cursor-autohide", "cursor-autohide", CursorAutohide, ffi::MPV_FORMAT_INT64),
    prop_spec!(c"cursor-autohide", "cursor-autohide", CursorAutohide, ffi::MPV_FORMAT_STRING),
    prop_spec!(c"cursor-autohide-fs-only", "cursor-autohide-fs-only", CursorAutohideFsOnly, ffi::MPV_FORMAT_FLAG),
];

/// Owned values delivered by mpv's property notifications. Reading these does
/// not wait on mpv's core, which must never block the UI's render thread.
pub struct PlaybackState {
    pub path: String,
    pub media_title: String,
    pub file_loaded: bool,
    pub playback_ready: bool,
    pub position: f64,
    pub duration: f64,
    pub paused: bool,
    pub mute: bool,
    pub volume: f64,
    pub volume_max: i64,
    pub fullscreen: bool,
    pub dimensions: (u32, u32),
    pub video_width: i64,
    pub video_height: i64,
    pub chapters: Vec<f64>,
    pub audio_id: i64,
    pub primary_sid: i64,
    pub secondary_sid: i64,
    pub video_id: i64,
    pub sub_text: String,
    pub sub_ass_extradata: String,
    pub sub_text_ass_full: String,
    pub osd_dimensions: OsdDimensions,
    pub sub_scale: f64,
    pub sub_pos: f64,
    pub sub_font: String,
    pub sub_ass_override: String,
    pub sub_scale_signs: bool,
    pub sub_use_margins: bool,
    pub sub_hinting: String,
    pub sub_shaper: String,
    pub secondary_sub_text: String,
    pub sub_start: f64,
    pub sub_end: f64,
    pub sub_delay: f64,
    pub sub_speed: f64,
    pub sub_visibility: bool,
    pub secondary_sub_start: f64,
    pub secondary_sub_end: f64,
    pub secondary_sub_delay: f64,
    pub secondary_sub_visibility: bool,
    pub audio_tracks: Vec<AudioTrack>,
    pub subtitle_tracks: Vec<SubtitleTrack>,
    pub sub_lines: Vec<SubLine>,
    pub frame_drops: i64,
    pub decoder_drops: i64,
    pub hwdec_current: String,
    pub cursor_autohide: CursorAutohide,
    pub cursor_autohide_fs_only: bool,
}

impl Default for PlaybackState {
    fn default() -> Self {
        Self {
            path: String::new(),
            media_title: String::new(),
            file_loaded: false,
            playback_ready: false,
            position: 0.0,
            duration: 0.0,
            paused: false,
            mute: false,
            volume: 0.0,
            volume_max: 0,
            fullscreen: false,
            dimensions: (0, 0),
            video_width: 0,
            video_height: 0,
            chapters: Vec::new(),
            audio_id: 0,
            primary_sid: 0,
            secondary_sid: 0,
            video_id: 0,
            sub_text: String::new(),
            sub_ass_extradata: String::new(),
            sub_text_ass_full: String::new(),
            osd_dimensions: OsdDimensions::default(),
            sub_scale: 1.0,
            sub_pos: 100.0,
            sub_font: "sans-serif".to_string(),
            sub_ass_override: "scale".to_string(),
            sub_scale_signs: false,
            sub_use_margins: true,
            sub_hinting: "none".to_string(),
            sub_shaper: "complex".to_string(),
            secondary_sub_text: String::new(),
            sub_start: 0.0,
            sub_end: 0.0,
            sub_delay: 0.0,
            sub_speed: 1.0,
            sub_visibility: false,
            secondary_sub_start: 0.0,
            secondary_sub_end: 0.0,
            secondary_sub_delay: 0.0,
            secondary_sub_visibility: false,
            audio_tracks: Vec::new(),
            subtitle_tracks: Vec::new(),
            sub_lines: Vec::new(),
            frame_drops: 0,
            decoder_drops: 0,
            hwdec_current: String::new(),
            cursor_autohide: CursorAutohide::default(),
            cursor_autohide_fs_only: false,
        }
    }
}

#[inline]
fn assign_str(dst: &mut String, src: &str) {
    dst.clear();
    dst.push_str(src);
}

#[inline]
unsafe fn decode_scalar_value(property: &ffi::mpv_event_property) -> MpvPropertyValue {
    let data = property.data;
    match property.format {
        ffi::MPV_FORMAT_NONE => MpvPropertyValue::None,
        ffi::MPV_FORMAT_FLAG if !data.is_null() => {
            MpvPropertyValue::Flag(unsafe { *data.cast::<c_int>() } != 0)
        }
        ffi::MPV_FORMAT_INT64 if !data.is_null() => {
            MpvPropertyValue::Int64(unsafe { *data.cast::<i64>() })
        }
        ffi::MPV_FORMAT_DOUBLE if !data.is_null() => {
            MpvPropertyValue::Double(unsafe { *data.cast::<f64>() })
        }
        ffi::MPV_FORMAT_STRING if !data.is_null() => {
            MpvPropertyValue::String(copy_mpv_string(unsafe { *data.cast::<*const c_char>() }))
        }
        _ => MpvPropertyValue::Other,
    }
}

impl PlaybackState {
    /// Decode `property` ONCE, update cached `PlaybackState`, and return the
    /// typed [`MpvPropertyValue`] for event emission.
    ///
    /// Event storage belongs to mpv and expires at the next `mpv_wait_event`
    /// call; all strings and node contents are copied here.
    pub(super) unsafe fn apply_property_event(
        &mut self,
        kind: Option<ObservedProperty>,
        property: &ffi::mpv_event_property,
    ) -> MpvPropertyValue {
        let data = property.data;
        let Some(kind) = kind else {
            return unsafe { decode_scalar_value(property) };
        };

        // Handle node-backed and custom-decoded properties directly so we never
        // walk `mpv_node` twice.
        match kind {
            ObservedProperty::ChapterList => {
                if property.format == ffi::MPV_FORMAT_NODE && !data.is_null() {
                    let chapters = extract_chapters_from_node(data.cast());
                    self.chapters.clone_from(&chapters);
                    return MpvPropertyValue::Chapters(chapters);
                }
                self.chapters.clear();
                return MpvPropertyValue::None;
            }
            ObservedProperty::TrackList => {
                if property.format == ffi::MPV_FORMAT_NODE && !data.is_null() {
                    let (audio, subtitle, all) = extract_tracks_from_node(data.cast());
                    self.audio_tracks.clone_from(&audio);
                    self.subtitle_tracks.clone_from(&subtitle);
                    let mut primary_sid = 0;
                    let mut secondary_sid = 0;
                    let mut audio_id = 0;
                    let mut video_id = 0;
                    for t in &all {
                        if !t.selected {
                            continue;
                        }
                        match (t.track_type, t.main_selection) {
                            (TrackType::Subtitle, 0) if primary_sid == 0 => primary_sid = t.id,
                            (TrackType::Subtitle, 1) if secondary_sid == 0 => secondary_sid = t.id,
                            (TrackType::Audio, 0) if audio_id == 0 => audio_id = t.id,
                            (TrackType::Video, 0) if video_id == 0 => video_id = t.id,
                            _ => {}
                        }
                    }
                    self.primary_sid = primary_sid;
                    self.secondary_sid = secondary_sid;
                    self.audio_id = audio_id;
                    self.video_id = video_id;
                    return MpvPropertyValue::Tracks {
                        audio,
                        subtitle,
                        all,
                    };
                }
                self.audio_tracks.clear();
                self.subtitle_tracks.clear();
                self.primary_sid = 0;
                self.secondary_sid = 0;
                self.audio_id = 0;
                self.video_id = 0;
                return MpvPropertyValue::None;
            }
            ObservedProperty::SubLines => {
                if property.format == ffi::MPV_FORMAT_NODE && !data.is_null() {
                    let lines = extract_sub_lines_from_node(data.cast());
                    self.sub_lines.clone_from(&lines);
                    return MpvPropertyValue::SubLines(lines);
                }
                self.sub_lines.clear();
                return MpvPropertyValue::None;
            }
            ObservedProperty::OsdDimensions => {
                if property.format == ffi::MPV_FORMAT_NODE && !data.is_null() {
                    let dims = extract_osd_dimensions_from_node(data.cast());
                    self.osd_dimensions = dims;
                    return MpvPropertyValue::OsdDimensions(dims);
                }
                return MpvPropertyValue::None;
            }
            ObservedProperty::CursorAutohide => {
                let int64_val = if property.format == ffi::MPV_FORMAT_INT64 && !data.is_null() {
                    unsafe { *data.cast::<i64>() }
                } else {
                    0
                };
                let str_val = if property.format == ffi::MPV_FORMAT_STRING && !data.is_null() {
                    unsafe { *data.cast::<*const c_char>() }
                } else {
                    ptr::null()
                };
                return match CursorAutohide::decode(property.format, int64_val, str_val) {
                    Some(decoded) => {
                        self.cursor_autohide = decoded;
                        MpvPropertyValue::CursorAutohide(decoded)
                    }
                    None => MpvPropertyValue::None,
                };
            }
            _ => {}
        }

        let value = unsafe { decode_scalar_value(property) };

        let as_opt_f64 = || -> Option<f64> {
            match &value {
                MpvPropertyValue::Double(v) if v.is_finite() => Some(*v),
                MpvPropertyValue::Int64(v) => Some(*v as f64),
                _ => None,
            }
        };
        let as_i64_or_zero = || -> i64 {
            match &value {
                MpvPropertyValue::Int64(v) => *v,
                _ => 0,
            }
        };
        let update_track_id = |slot: &mut i64| match &value {
            MpvPropertyValue::Int64(v) => *slot = *v,
            MpvPropertyValue::Flag(false) => *slot = 0,
            _ => {}
        };

        match kind {
            ObservedProperty::Path => match &value {
                MpvPropertyValue::String(s) => assign_str(&mut self.path, s),
                _ => self.path.clear(),
            },
            ObservedProperty::MediaTitle => match &value {
                MpvPropertyValue::String(s) => assign_str(&mut self.media_title, s),
                _ => self.media_title.clear(),
            },
            // Momentary `MPV_FORMAT_NONE` during seeks / track changes must not
            // clobber `position`, `duration`, or `volume` to default values.
            ObservedProperty::TimePos => {
                if let Some(v) = as_opt_f64() {
                    self.position = v.max(0.0);
                }
            }
            ObservedProperty::Duration => {
                if let Some(v) = as_opt_f64() {
                    self.duration = v.max(0.0);
                }
            }
            ObservedProperty::Volume => {
                if let Some(v) = as_opt_f64() {
                    self.volume = v;
                }
            }
            ObservedProperty::VolumeMax => match &value {
                MpvPropertyValue::Int64(v) => self.volume_max = *v,
                MpvPropertyValue::Double(v) if v.is_finite() => {
                    self.volume_max = v.round() as i64;
                }
                _ => {}
            },
            ObservedProperty::Pause => {
                if let MpvPropertyValue::Flag(paused) = value {
                    self.paused = paused;
                }
            }
            ObservedProperty::Fullscreen => {
                if let MpvPropertyValue::Flag(fs) = value {
                    self.fullscreen = fs;
                }
            }
            ObservedProperty::Mute => {
                if let MpvPropertyValue::Flag(muted) = value {
                    self.mute = muted;
                }
            }
            ObservedProperty::Dwidth => {
                self.dimensions.0 = u32::try_from(as_i64_or_zero()).unwrap_or(0);
            }
            ObservedProperty::Dheight => {
                self.dimensions.1 = u32::try_from(as_i64_or_zero()).unwrap_or(0);
            }
            ObservedProperty::VideoParamsW => self.video_width = as_i64_or_zero(),
            ObservedProperty::VideoParamsH => self.video_height = as_i64_or_zero(),
            ObservedProperty::Aid => update_track_id(&mut self.audio_id),
            ObservedProperty::Sid => {
                let prev = self.primary_sid;
                update_track_id(&mut self.primary_sid);
                if self.primary_sid != prev {
                    self.sub_text_ass_full.clear();
                    if self.primary_sid == 0 {
                        self.sub_ass_extradata.clear();
                    }
                }
            }
            ObservedProperty::SecondarySid => update_track_id(&mut self.secondary_sid),
            ObservedProperty::Vid => update_track_id(&mut self.video_id),
            ObservedProperty::SubText => match &value {
                MpvPropertyValue::String(s) => assign_str(&mut self.sub_text, s.trim()),
                _ => self.sub_text.clear(),
            },
            ObservedProperty::SubAssExtradata => match &value {
                MpvPropertyValue::String(s) => assign_str(&mut self.sub_ass_extradata, s),
                _ => self.sub_ass_extradata.clear(),
            },
            ObservedProperty::SubTextAssFull => match &value {
                MpvPropertyValue::String(s) => assign_str(&mut self.sub_text_ass_full, s),
                _ => self.sub_text_ass_full.clear(),
            },
            ObservedProperty::SubScale => {
                self.sub_scale = as_opt_f64().unwrap_or(1.0).max(0.05);
            }
            ObservedProperty::SubPos => {
                self.sub_pos = as_opt_f64().unwrap_or(100.0).clamp(0.0, 150.0);
            }
            ObservedProperty::SubFont => {
                if let MpvPropertyValue::String(s) = &value {
                    if !s.is_empty() {
                        assign_str(&mut self.sub_font, s);
                    }
                }
            }
            ObservedProperty::SubAssOverride => {
                if let MpvPropertyValue::String(s) = &value {
                    if !s.is_empty() {
                        assign_str(&mut self.sub_ass_override, s);
                    }
                }
            }
            ObservedProperty::SubScaleSigns => {
                if let MpvPropertyValue::Flag(v) = value {
                    self.sub_scale_signs = v;
                }
            }
            ObservedProperty::SubUseMargins => {
                if let MpvPropertyValue::Flag(v) = value {
                    self.sub_use_margins = v;
                }
            }
            ObservedProperty::SubHinting => {
                if let MpvPropertyValue::String(s) = &value {
                    if !s.is_empty() {
                        assign_str(&mut self.sub_hinting, s);
                    }
                }
            }
            ObservedProperty::SubShaper => {
                if let MpvPropertyValue::String(s) = &value {
                    if !s.is_empty() {
                        assign_str(&mut self.sub_shaper, s);
                    }
                }
            }
            ObservedProperty::SecondarySubText => match &value {
                MpvPropertyValue::String(s) => assign_str(&mut self.secondary_sub_text, s.trim()),
                _ => self.secondary_sub_text.clear(),
            },
            ObservedProperty::SubStart => self.sub_start = as_opt_f64().unwrap_or(0.0),
            ObservedProperty::SubEnd => self.sub_end = as_opt_f64().unwrap_or(0.0),
            ObservedProperty::SubDelay => self.sub_delay = as_opt_f64().unwrap_or(0.0),
            ObservedProperty::SubSpeed => {
                self.sub_speed = as_opt_f64().unwrap_or(1.0).max(f64::EPSILON);
            }
            ObservedProperty::SubVisibility => {
                if let MpvPropertyValue::Flag(v) = value {
                    self.sub_visibility = v;
                }
            }
            ObservedProperty::SecondarySubStart => {
                self.secondary_sub_start = as_opt_f64().unwrap_or(0.0);
            }
            ObservedProperty::SecondarySubEnd => {
                self.secondary_sub_end = as_opt_f64().unwrap_or(0.0);
            }
            ObservedProperty::SecondarySubDelay => {
                self.secondary_sub_delay = as_opt_f64().unwrap_or(0.0);
            }
            ObservedProperty::SecondarySubVisibility => {
                if let MpvPropertyValue::Flag(v) = value {
                    self.secondary_sub_visibility = v;
                }
            }
            ObservedProperty::FrameDropCount => self.frame_drops = as_i64_or_zero(),
            ObservedProperty::DecoderFrameDropCount => self.decoder_drops = as_i64_or_zero(),
            ObservedProperty::HwdecCurrent => match &value {
                MpvPropertyValue::String(s) => assign_str(&mut self.hwdec_current, s),
                _ => self.hwdec_current.clear(),
            },
            ObservedProperty::CursorAutohideFsOnly => {
                if let MpvPropertyValue::Flag(v) = value {
                    self.cursor_autohide_fs_only = v;
                }
            }
            ObservedProperty::Chapters
            | ObservedProperty::ChapterList
            | ObservedProperty::TrackList
            | ObservedProperty::SubLines
            | ObservedProperty::OsdDimensions
            | ObservedProperty::CursorAutohide => {}
        }

        value
    }

    #[cfg(test)]
    pub(super) unsafe fn update(&mut self, property: &ffi::mpv_event_property) {
        if property.name.is_null() {
            return;
        }
        let kind = ObservedProperty::from_bytes(unsafe { CStr::from_ptr(property.name) }.to_bytes());
        let _ = unsafe { self.apply_property_event(kind, property) };
    }
}

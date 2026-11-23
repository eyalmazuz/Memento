//! Public data types exposed by `subomi_player::mpv`.

use std::borrow::Cow;
use std::ffi::{c_char, CStr};
use std::fmt;
use std::ops::Deref;

use crate::events::Track;

use super::ffi;

pub const ASR_STREAM_PROTOCOL: &str = "subomi-asr";
pub const ASR_STREAM_URI: &str = "subomi-asr://live.vtt";

/// Track type corresponding to `MpvTrack::Type`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(i32)]
pub enum TrackType {
    #[default]
    None = 0,
    Audio = 1,
    Video = 2,
    Subtitle = 3,
}

impl TrackType {
    pub fn from_bytes(bytes: &[u8]) -> Self {
        match bytes {
            b"audio" => Self::Audio,
            b"video" => Self::Video,
            b"sub" => Self::Subtitle,
            _ => Self::None,
        }
    }
}

/// Owned metadata for an audio track in mpv's `track-list`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioTrack {
    pub id: i64,
    pub language: String,
    pub title: String,
    pub external_filename: String,
    pub external: bool,
    pub default_track: bool,
    pub selected: bool,
}

/// Owned metadata for a video track in mpv's `track-list`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoTrack {
    pub id: i64,
    pub title: String,
    pub language: String,
    pub codec: String,
    pub external: bool,
    pub default_track: bool,
    pub selected: bool,
}

/// Owned metadata for a subtitle track in mpv's `track-list`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubtitleTrack {
    pub id: i64,
    pub src_id: i64,
    pub codec: String,
    pub external_path: String,
    pub language: String,
    pub title: String,
    pub external: bool,
    pub default_track: bool,
    pub selected: bool,
}

/// Snapshot of mpv's `osd-dimensions` property (framebuffer & margin geometry for ASS layout).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OsdDimensions {
    pub w: u32,
    pub h: u32,
    pub mt: i32,
    pub mb: i32,
    pub ml: i32,
    pub mr: i32,
    pub par: f64,
}

impl Default for OsdDimensions {
    fn default() -> Self {
        Self {
            w: 0,
            h: 0,
            mt: 0,
            mb: 0,
            ml: 0,
            mr: 0,
            par: 1.0,
        }
    }
}

/// Snapshot of the currently loaded media and streaming context needed by an
/// off-thread encoder (local files, YouTube/yt-dlp streams, split DASH audio,
/// and HTTP headers/cookies).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EncodeContext {
    pub path: String,
    pub audio_id: i64,
    pub stream_open_filename: String,
    pub script_opts: String,
    pub http_header_fields: Vec<String>,
    pub referrer: String,
    pub user_agent: String,
    pub cookies: bool,
    pub cookies_file: String,
    pub ytdl_raw_options: String,
    pub external_audio_filename: Option<String>,
}

impl EncodeContext {
    /// Returns whether `uri` uses Subomi's internal in-memory ASR stream scheme
    /// (`subomi-asr://`), which must never be treated as a media source URL.
    pub fn is_internal_asr_uri(uri: &str) -> bool {
        uri.trim()
            .split_once("://")
            .is_some_and(|(scheme, _)| scheme.eq_ignore_ascii_case(ASR_STREAM_PROTOCOL))
    }

    /// Resolve the effective media path/URL for off-thread encoding.
    ///
    /// Prefers `stream_open_filename` (e.g. a resolved direct URL or `edl://`
    /// stream rewritten by `ytdl_hook`) when non-empty and not an internal
    /// `subomi-asr://` URI; otherwise falls back to `path`.
    pub fn resolve_media_path<'a>(stream_open_filename: &'a str, path: &'a str) -> &'a str {
        let trimmed = stream_open_filename.trim();
        if !trimmed.is_empty() && !Self::is_internal_asr_uri(trimmed) {
            stream_open_filename
        } else {
            path
        }
    }

    /// Resolve the effective media path/URL for this snapshot.
    pub fn effective_media_path(&self) -> &str {
        Self::resolve_media_path(&self.stream_open_filename, &self.path)
    }

    /// Resolve the `external_filename` of the currently selected audio track,
    /// needed for split DASH / `edl://` external audio streams.
    pub fn select_external_audio(audio_tracks: &[AudioTrack], audio_id: i64) -> Option<String> {
        if audio_id <= 0 {
            return None;
        }
        audio_tracks
            .iter()
            .find(|track| track.id == audio_id)
            .filter(|track| !track.external_filename.trim().is_empty())
            .map(|track| track.external_filename.clone())
    }
}

/// Owned, source-resolution RGBA pixels returned by mpv's `screenshot-raw`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawFrame {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

/// Owned subtitle cue copied from mpv's live `sub-lines` property.
#[derive(Debug, Clone, PartialEq)]
pub struct SubLine {
    pub id: i64,
    pub text: String,
    pub start: f64,
    pub end: f64,
}

/// Cursor autohide state matching mpv's `"cursor-autohide"` property.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorAutohide {
    Number(i64),
    Always,
    Never,
}

impl Default for CursorAutohide {
    fn default() -> Self {
        Self::Number(0)
    }
}

impl CursorAutohide {
    pub fn decode(format: u32, int64_val: i64, str_val: *const c_char) -> Option<Self> {
        if format == ffi::MPV_FORMAT_INT64 {
            Some(Self::Number(int64_val))
        } else if format == ffi::MPV_FORMAT_STRING && !str_val.is_null() {
            let bytes = unsafe { CStr::from_ptr(str_val) }.to_bytes();
            match bytes {
                b"always" => Some(Self::Always),
                b"no" => Some(Self::Never),
                _ => None,
            }
        } else {
            None
        }
    }

    pub fn to_c_params(self) -> (bool, i64) {
        match self {
            Self::Number(timeout) => (timeout == 0, timeout),
            Self::Always => (true, -1),
            Self::Never => (false, -1),
        }
    }
}

/// A subtitle cue managed by [`super::AsrSubtitleTrack`].
#[derive(Debug, Clone, PartialEq)]
pub struct AsrCue {
    pub id: i64,
    pub start: f64,
    pub end: f64,
    pub text: String,
}

/// Active backend mode selected for in-memory ASR subtitles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AsrTrackMode {
    /// Patched `libmpv` (`eyalmazuz/mpv` `15d25e0f`) using `mpv_command_node_async`.
    NativeMemory,
    /// Stock `libmpv` using `mpv_stream_cb_add_ro` (`subomi-asr://live.vtt` + `sub-reload`).
    StreamCallback,
}

/// Decoded payload from an asynchronous `MPV_EVENT_COMMAND_REPLY`.
#[derive(Debug, Clone, PartialEq)]
pub enum CommandReplyResult {
    None,
    Int64(i64),
    Double(f64),
    Flag(bool),
    String(String),
    Other,
}

/// Reason reported by `MPV_EVENT_END_FILE`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndFileReason {
    Eof,
    Stop,
    Quit,
    Error(i32),
    Redirect,
    Unknown(i32),
}

/// Zero-allocation property name wrapper for [`super::MpvEvent::PropertyChange`].
///
/// Observed properties resolve in O(1) via `reply_userdata` to a static `&str`
/// with zero heap allocation, while still supporting `.as_str()`, `Deref<Target = str>`,
/// and `PartialEq<&str>` transparently.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MpvPropertyName(Cow<'static, str>);

impl MpvPropertyName {
    pub const fn borrowed(name: &'static str) -> Self {
        Self(Cow::Borrowed(name))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Deref for MpvPropertyName {
    type Target = str;

    fn deref(&self) -> &str {
        &self.0
    }
}

impl AsRef<str> for MpvPropertyName {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl PartialEq<str> for MpvPropertyName {
    fn eq(&self, other: &str) -> bool {
        self.as_str() == other
    }
}

impl PartialEq<&str> for MpvPropertyName {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}

impl PartialEq<String> for MpvPropertyName {
    fn eq(&self, other: &String) -> bool {
        self.as_str() == other.as_str()
    }
}

impl From<&'static str> for MpvPropertyName {
    fn from(name: &'static str) -> Self {
        Self(Cow::Borrowed(name))
    }
}

impl From<String> for MpvPropertyName {
    fn from(name: String) -> Self {
        Self(Cow::Owned(name))
    }
}

impl fmt::Display for MpvPropertyName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Typed property payload delivered in [`super::MpvEvent::PropertyChange`].
#[derive(Debug, Clone, PartialEq)]
pub enum MpvPropertyValue {
    None,
    Flag(bool),
    Int64(i64),
    Double(f64),
    String(String),
    StringList(Vec<String>),
    Chapters(Vec<f64>),
    Tracks {
        audio: Vec<AudioTrack>,
        subtitle: Vec<SubtitleTrack>,
        all: Vec<Track>,
    },
    SubLines(Vec<SubLine>),
    OsdDimensions(OsdDimensions),
    CursorAutohide(CursorAutohide),
    Other,
}

impl MpvPropertyValue {
    /// Borrow the string payload when this property value is [`Self::String`].
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(s) => Some(s.as_str()),
            _ => None,
        }
    }
}

/// Native display server passed into `mpv_render_context_create` for OpenGL interop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GlDisplayBackend {
    Unknown,
    X11,
    Wayland,
}

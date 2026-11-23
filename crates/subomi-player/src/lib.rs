//! Player-side Rust modules and libmpv bindings for Subomi.

pub mod args;
pub mod autopause;
pub mod controller;
pub mod events;
pub mod mpv;

pub use args::{help_text, parse_args, version_text, ParsedArgs, PlaylistLoad};
pub use autopause::{
    subtitle_playback_time, subtitle_raw_time, subtitle_to_media_time, subtitle_valid_speed,
    AutoPauseState,
};
pub use controller::{
    format_keypress, format_modifiers, is_subtitle_file, CliOption, KeyMapping, KeypadPrefix,
    MouseButton, ScreenshotSpec, ScreenshotValidationError, WheelButton,
};
pub use events::Track;
pub use mpv::{
    format_webvtt_cues, format_webvtt_timestamp, mpv_error_message, AsrCue, AsrSubtitleTrack,
    AsrTrackMode, AudioInstance, AudioTrack, CommandReplyResult, CursorAutohide, EncodeContext,
    EndFileReason, HeadlessEvent, HeadlessMpv, MpvEvent, MpvInstance, MpvPropertyName,
    MpvPropertyValue, OsdDimensions, RawFrame, ScreenshotCapturer, ScreenshotRequest, SubLine,
    SubtitleTrack, TrackType, VideoTrack, WeakStringObserver, ASR_STREAM_PROTOCOL, ASR_STREAM_URI,
};

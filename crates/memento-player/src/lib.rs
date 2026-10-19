//! Player-side Rust modules and the C ABI for the existing mpv instance.
//!
//! The three `mpvstate`/`mpvplayer`-derived modules form the Qt-free player
//! core (endgame workstream: mpv integration):
//!
//! - [`events`] — the event vocabulary + [`events::MpvEventSource`] input
//!   seam (no libmpv genesis inside the core).
//! - [`state`] — the `MpvState`/`MpvSubtitle` property store with
//!   change-gated signal production.
//! - [`machine`] — the lifecycle state machine, intent-vs-engine seek
//!   discrimination, derived decisions (position clamp, duration handling,
//!   renderer-carry) and the `autopause.rs` integration.

mod args;
mod args_capi;
pub mod autopause;
pub mod controller;
pub mod events;
pub mod machine;
pub mod mpv;
pub mod state;

pub use autopause::{
    memento_player_autopause_position, memento_player_autopause_step,
    memento_player_subtitle_playback_time, memento_player_subtitle_raw_time,
    memento_player_subtitle_valid_speed, MementoAutoPauseStateC,
};
pub use controller::{
    format_keypress, format_modifiers, is_subtitle_file, memento_player_format_keypress,
    memento_player_format_modifiers, memento_player_is_subtitle_file,
    memento_player_mouse_button_name, memento_player_parse_cli_options,
    memento_player_validate_raw_screenshot, memento_player_wheel_button,
    parse_cli_options_u16, parse_cli_options_utf8, CliOption, KeyMapping, KeypadPrefix,
    MementoPlayerCliOptionCb, MementoPlayerUtf16Arg, MouseButton, ScreenshotSpec,
    ScreenshotValidationError, WheelButton,
};
pub use mpv::{
    memento_player_command, memento_player_command_async, memento_player_command_ret_i64,
    memento_player_command_ret_raw, memento_player_cue_valid,
    memento_player_decode_cursor_autohide, memento_player_extract_chapter_times,
    memento_player_extract_sub_lines, memento_player_extract_tracks,
    memento_player_f64_list_free, memento_player_id_valid,
    memento_player_sub_lines_free, memento_player_supports_memory_subtitles,
    memento_player_tracks_free, AudioInstance, AudioTrack, CursorAutohide, EncodeContext, ExtractedSubLine,
    ExtractedTrack, MementoF64ListC, MementoSubLineC, MementoSubLineListC, MementoTrackC,
    MementoTrackListC, MpvInstance, RawFrame, SubtitleTrack, TrackType,
};
pub use state::{
    memento_player_state_active_subtitle_track, memento_player_state_create,
    memento_player_state_find_track_by_id, memento_player_state_free,
    memento_player_state_get_duration, memento_player_state_get_mute,
    memento_player_state_get_pause, memento_player_state_get_property,
    memento_player_state_get_speed, memento_player_state_get_sub_delay,
    memento_player_state_get_time_pos, memento_player_state_get_volume,
    memento_player_state_update_property, q_bound, AutoHideType, MpvStateCore, Signal, Subtitle,
    SubtitleChange, VideoParams, PROPERTY_AID_CHANGED, PROPERTY_AUDIO_TRACKS_CHANGED,
    PROPERTY_CHAPTERS_CHANGED, PROPERTY_CURSOR_AUTOHIDE_CHANGED,
    PROPERTY_CURSOR_AUTOHIDE_FS_ONLY_CHANGED, PROPERTY_CURSOR_AUTOHIDE_TIME_CHANGED,
    PROPERTY_CURSOR_AUTOHIDE_TYPE_CHANGED, PROPERTY_DURATION_CHANGED, PROPERTY_FULLSCREEN_CHANGED,
    PROPERTY_MUTE_CHANGED, PROPERTY_NONE, PROPERTY_PATH_CHANGED, PROPERTY_PAUSE_CHANGED,
    PROPERTY_POSITION_CHANGED, PROPERTY_SECONDARY_SID_CHANGED, PROPERTY_SECONDARY_SUB_DELAY_CHANGED,
    PROPERTY_SECONDARY_SUB_TEXT_CHANGED, PROPERTY_SECONDARY_SUB_VISIBILITY_CHANGED,
    PROPERTY_SID_CHANGED, PROPERTY_SPEED_CHANGED, PROPERTY_SUBTITLE_TRACKS_CHANGED,
    PROPERTY_SUB_DELAY_CHANGED, PROPERTY_SUB_TEXT_CHANGED, PROPERTY_SUB_VISIBILITY_CHANGED,
    PROPERTY_TITLE_CHANGED, PROPERTY_TRACKS_CHANGED, PROPERTY_VID_CHANGED,
    PROPERTY_VIDEO_HEIGHT_CHANGED, PROPERTY_VIDEO_PARAMS_CHANGED, PROPERTY_VIDEO_TRACKS_CHANGED,
    PROPERTY_VIDEO_WIDTH_CHANGED, PROPERTY_VOLUME_CHANGED, PROPERTY_VOLUME_MAX_CHANGED,
};
pub use events::{
    EngineEvent, Event, Intent, MpvEventSource, PropertyChange, PropertyName, ScriptedEventSource,
    SubLine, Track, sub_line_from_extracted, track_from_extracted,
};
pub use machine::{
    CollectorSink, Decision, Phase, PlayerCore, PlayerDecisionSink, Rejected, Transition,
};



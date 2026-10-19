//! The property store mirroring `MpvState` (mpvstate.{h,cpp}) and the two
//! `MpvSubtitle` objects (mpvsubtitle.{h,cpp}) it owns, as a Qt-free core.
//!
//! Every setter is *change-gated* exactly like the C++ original: assigning an
//! unchanged value emits nothing (the Qt signal would never fire), while
//! assigning a changed value yields a [`Signal`] describing what a UI would
//! have been notified of. Two deliberate exceptions mirror the C++ source:
//!
//! - `MpvState::setChapters` has **no** change gate — every delivery emits
//!   `chaptersChanged` (mpvstate.cpp:89-93).
//! - `MpvState::setTracks` always clears+refills the three track buckets and
//!   always emits all three "changed" signals.
//!
//! C++ divergence implemented per its *documented* behavior: the
//! `MpvState::setSecondarySubtitle` setter emits `subtitleChanged` for the
//! secondary object (mpvstate.cpp:604) — an obvious copy-paste bug against
//! the doc comment ("Emitted when the secondary subtitle object is
//! changed"). The object-replacement API is unused by every handler (the two
//! `MpvSubtitle` instances live for the player's lifetime), so this core does
//! not port that setter at all and has no secondary-assignment signal to get
//! wrong.

use std::ffi::{c_char, c_int, c_void, CStr, CString};

use crate::autopause::{
    memento_player_subtitle_playback_time, memento_player_subtitle_raw_time,
    memento_player_subtitle_valid_speed,
};
use crate::events::{PropertyChange, Track};
use crate::mpv::{CursorAutohide, TrackType};

// Property change bitflags matching MementoPlayerPropertyFlags
pub const PROPERTY_NONE: u32 = 0;
pub const PROPERTY_POSITION_CHANGED: u32 = 1 << 0;
pub const PROPERTY_DURATION_CHANGED: u32 = 1 << 1;
pub const PROPERTY_PAUSE_CHANGED: u32 = 1 << 2;
pub const PROPERTY_VOLUME_CHANGED: u32 = 1 << 3;
pub const PROPERTY_MUTE_CHANGED: u32 = 1 << 4;
pub const PROPERTY_SPEED_CHANGED: u32 = 1 << 5;
pub const PROPERTY_SUB_DELAY_CHANGED: u32 = 1 << 6;
pub const PROPERTY_TRACKS_CHANGED: u32 = 1 << 7;
pub const PROPERTY_CHAPTERS_CHANGED: u32 = 1 << 8;
pub const PROPERTY_CURSOR_AUTOHIDE_CHANGED: u32 = 1 << 9;
pub const PROPERTY_FULLSCREEN_CHANGED: u32 = 1 << 10;
pub const PROPERTY_TITLE_CHANGED: u32 = 1 << 11;
pub const PROPERTY_PATH_CHANGED: u32 = 1 << 12;
pub const PROPERTY_VIDEO_PARAMS_CHANGED: u32 = 1 << 13;
pub const PROPERTY_AID_CHANGED: u32 = 1 << 14;
pub const PROPERTY_SID_CHANGED: u32 = 1 << 15;
pub const PROPERTY_SECONDARY_SID_CHANGED: u32 = 1 << 16;
pub const PROPERTY_VID_CHANGED: u32 = 1 << 17;
pub const PROPERTY_SUB_TEXT_CHANGED: u32 = 1 << 18;
pub const PROPERTY_SUB_VISIBILITY_CHANGED: u32 = 1 << 19;
pub const PROPERTY_SECONDARY_SUB_DELAY_CHANGED: u32 = 1 << 20;
pub const PROPERTY_SECONDARY_SUB_TEXT_CHANGED: u32 = 1 << 21;
pub const PROPERTY_SECONDARY_SUB_VISIBILITY_CHANGED: u32 = 1 << 22;
pub const PROPERTY_VOLUME_MAX_CHANGED: u32 = 1 << 23;
pub const PROPERTY_AUDIO_TRACKS_CHANGED: u32 = 1 << 24;
pub const PROPERTY_VIDEO_TRACKS_CHANGED: u32 = 1 << 25;
pub const PROPERTY_SUBTITLE_TRACKS_CHANGED: u32 = 1 << 26;
pub const PROPERTY_VIDEO_WIDTH_CHANGED: u32 = 1 << 27;
pub const PROPERTY_VIDEO_HEIGHT_CHANGED: u32 = 1 << 28;
pub const PROPERTY_CURSOR_AUTOHIDE_TIME_CHANGED: u32 = 1 << 29;
pub const PROPERTY_CURSOR_AUTOHIDE_TYPE_CHANGED: u32 = 1 << 30;
pub const PROPERTY_CURSOR_AUTOHIDE_FS_ONLY_CHANGED: u32 = 1 << 31;

/// `MpvState::AutoHideType` — the three forms of mpv's `cursor-autohide`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoHideType {
    Number,
    Always,
    Never,
}

/// `MpvSubtitle` (`mpvsubtitle.h`) — text, cue window, delay/speed and
/// visibility, with change-gated setters that reuse the certified subtitle
/// time math in `autopause.rs` (`playback_time`/`raw_time`/speed validation
/// are *not* re-ported here).
#[derive(Debug, Clone, PartialEq)]
pub struct Subtitle {
    text: String,
    start_time: f64,
    end_time: f64,
    delay: f64,
    speed: f64,
    visible: bool,
}

impl Default for Subtitle {
    /// Member defaults of `mpvsubtitle.h:202-217`: empty text, zero times,
    /// zero delay, speed 1, invisible.
    fn default() -> Self {
        Self {
            text: String::new(),
            start_time: 0.0,
            end_time: 0.0,
            delay: 0.0,
            speed: 1.0,
            visible: false,
        }
    }
}

impl Subtitle {
    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn start_time(&self) -> f64 {
        self.start_time
    }

    pub fn end_time(&self) -> f64 {
        self.end_time
    }

    pub fn delay(&self) -> f64 {
        self.delay
    }

    pub fn speed(&self) -> f64 {
        self.speed
    }

    pub fn visible(&self) -> bool {
        self.visible
    }

    /// `MpvSubtitle::playbackTime` — delegated to the certified
    /// `memento_player_subtitle_playback_time`.
    pub fn playback_time(&self, raw_time: f64) -> f64 {
        memento_player_subtitle_playback_time(raw_time, self.speed, self.delay)
    }

    /// `MpvSubtitle::rawTime` — delegated to the certified
    /// `memento_player_subtitle_raw_time`.
    pub fn raw_time(&self, playback_time: f64) -> f64 {
        memento_player_subtitle_raw_time(playback_time, self.speed, self.delay)
    }

    /// `MpvSubtitle::setSpeed` — non-finite/non-positive values are rejected
    /// via the certified `memento_player_subtitle_valid_speed` (speed stays
    /// at its previous value).
    pub fn set_speed(&mut self, value: f64) -> Option<SubtitleChange> {
        if !memento_player_subtitle_valid_speed(self.speed, value) {
            return None;
        }
        self.speed = value;
        Some(SubtitleChange::Speed(value))
    }

    pub fn set_delay(&mut self, value: f64) -> Option<SubtitleChange> {
        if self.delay == value {
            return None;
        }
        self.delay = value;
        Some(SubtitleChange::Delay(value))
    }

    pub fn set_visible(&mut self, value: bool) -> Option<SubtitleChange> {
        if self.visible == value {
            return None;
        }
        self.visible = value;
        Some(SubtitleChange::Visible(value))
    }

    pub fn set_text(&mut self, value: impl Into<String>) -> Option<SubtitleChange> {
        let value = value.into();
        if self.text == value {
            return None;
        }
        self.text = value;
        Some(SubtitleChange::Text(self.text.clone()))
    }

    pub fn set_start_time(&mut self, value: f64) -> Option<SubtitleChange> {
        if self.start_time == value {
            return None;
        }
        self.start_time = value;
        Some(SubtitleChange::StartTime(value))
    }

    pub fn set_end_time(&mut self, value: f64) -> Option<SubtitleChange> {
        if self.end_time == value {
            return None;
        }
        self.end_time = value;
        Some(SubtitleChange::EndTime(value))
    }

    /// The `sub-text` (or `secondary-sub-text`) handler sequence:
    /// `setStartTime`, then `setEndTime`, then `setText` — three
    /// change-gated emissions in exactly that order (mpvplayer.cpp:383-385).
    pub fn apply_sub_text(
        &mut self,
        text: impl Into<String>,
        start: f64,
        end: f64,
    ) -> Vec<SubtitleChange> {
        let mut changes = Vec::with_capacity(3);
        changes.extend(self.set_start_time(start));
        changes.extend(self.set_end_time(end));
        changes.extend(self.set_text(text));
        changes
    }

    /// The `sid`/`secondary-sid` flag-disabled reset: start and end times
    /// back to 0 and text to `""`, each change-gated. Delay, speed and
    /// visibility are *not* touched (mpvplayer.cpp:294-298, :315-318).
    pub fn reset_for_track_disabled(&mut self) -> Vec<SubtitleChange> {
        self.apply_sub_text("", 0.0, 0.0)
    }
}

/// What changed on one of the two subtitle objects.
#[derive(Debug, Clone, PartialEq)]
pub enum SubtitleChange {
    Text(String),
    StartTime(f64),
    EndTime(f64),
    Delay(f64),
    Speed(f64),
    Visible(bool),
}

/// The `MpvState`/`MpvPlayer` signal a UI would receive for a state change.
///
/// Payloads mirror the Qt signal arguments byte-for-byte; subtitle-object
/// changes are grouped under [`Signal::SubtitleChanged`] with the
/// `secondary` flag standing in for which `MpvSubtitle` instance emitted.
#[derive(Debug, Clone, PartialEq)]
pub enum Signal {
    VideoWidthChanged(i64),
    VideoHeightChanged(i64),
    /// Always emitted on delivery, even for an identical list
    /// (`setChapters` is not change-gated).
    ChaptersChanged(Vec<f64>),
    DurationChanged(f64),
    FullscreenChanged(bool),
    TitleChanged(String),
    PathChanged(String),
    PauseChanged(bool),
    TimePositionChanged(f64),
    AudioTracksChanged,
    VideoTracksChanged,
    SubtitleTracksChanged,
    VolumeChanged(i64),
    MaxVolumeChanged(i64),
    AidChanged(i64),
    SidChanged(i64),
    SecondarySidChanged(i64),
    VidChanged(i64),
    SubtitleChanged { secondary: bool, change: SubtitleChange },
    CursorAutoHideTypeChanged(AutoHideType),
    CursorAutoHideTimeChanged(i64),
    CursorAutoHideFullscreenOnlyChanged(bool),
}

/// Video geometry the renderer consumes (`video-params/w|h` plus the
/// FILE_LOADED probe). `MpvState` initializes both to 0.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct VideoParams {
    pub width: i64,
    pub height: i64,
}

impl VideoParams {
    /// `width / height` as f64, or `None` while either dimension is
    /// non-positive (audio-only or not-yet-known content).
    pub fn aspect_ratio(&self) -> Option<f64> {
        if self.width > 0 && self.height > 0 {
            Some(self.width as f64 / self.height as f64)
        } else {
            None
        }
    }

    pub fn is_valid(&self) -> bool {
        self.width > 0 && self.height > 0
    }
}

/// Emulates Qt's `qBound(min, val, max)` for `f64`, including its NaN
/// behavior: `qMin(max, val)` is `(val < max) ? val : max`, so a NaN `val`
/// collapses to `max` — i.e. **a NaN position clamps to the duration**, not
/// to zero. That is a Qt implementation detail the C++ consumer
/// (`asrcontroller.cpp` `qBound(0.0, timePosition, duration)`) inherits
/// silently; reproduced here and flagged as a flagged quirk rather than
/// "fixed".
pub fn q_bound(min: f64, val: f64, max: f64) -> f64 {
    let upper = if val < max { val } else { max };
    if min < upper { upper } else { min }
}

/// The property bag behind `MpvState`, with change-gated signal production.
#[derive(Debug, Clone, PartialEq)]
pub struct MpvStateCore {
    pub time_pos: f64,
    pub duration: f64,
    pub pause: bool,
    pub volume: i64,
    pub mute: bool,
    pub speed: f64,
    pub sub_delay: f64,
    pub tracks: Vec<Track>,
    pub chapters: Vec<f64>,
    pub cursor_autohide: CursorAutohide,

    pub video_width: i64,
    pub video_height: i64,
    pub fullscreen: bool,
    pub title: String,
    pub path: String,
    pub max_volume: i64,
    pub aid: i64,
    pub sid: i64,
    pub secondary_sid: i64,
    pub vid: i64,
    pub subtitle: Subtitle,
    pub secondary_subtitle: Subtitle,
    pub cursor_auto_hide_type: AutoHideType,
    pub cursor_auto_hide_time: i64,
    pub cursor_auto_hide_fullscreen_only: bool,
    pub audio_tracks: Vec<Track>,
    pub video_tracks: Vec<Track>,
    pub subtitle_tracks: Vec<Track>,

    pub title_c_str: CString,
    pub path_c_str: CString,
    pub sub_text_c_str: CString,
    pub secondary_sub_text_c_str: CString,
}

impl Default for MpvStateCore {
    /// Member initializers of `mpvstate.h:766-839`: zeros everywhere, empty
    /// strings/lists, `pause` false, both subtitles default, auto-hide type
    /// `Number` with a 0 ms timeout.
    fn default() -> Self {
        Self {
            time_pos: 0.0,
            duration: 0.0,
            pause: false,
            volume: 0,
            mute: false,
            speed: 1.0,
            sub_delay: 0.0,
            tracks: Vec::new(),
            chapters: Vec::new(),
            cursor_autohide: CursorAutohide::Number(0),

            video_width: 0,
            video_height: 0,
            fullscreen: false,
            title: String::new(),
            path: String::new(),
            max_volume: 0,
            aid: 0,
            sid: 0,
            secondary_sid: 0,
            vid: 0,
            subtitle: Subtitle::default(),
            secondary_subtitle: Subtitle::default(),
            cursor_auto_hide_type: AutoHideType::Number,
            cursor_auto_hide_time: 0,
            cursor_auto_hide_fullscreen_only: false,
            audio_tracks: Vec::new(),
            video_tracks: Vec::new(),
            subtitle_tracks: Vec::new(),

            title_c_str: CString::new("").unwrap(),
            path_c_str: CString::new("").unwrap(),
            sub_text_c_str: CString::new("").unwrap(),
            secondary_sub_text_c_str: CString::new("").unwrap(),
        }
    }
}

impl MpvStateCore {
    pub fn new() -> Self {
        Self::default()
    }

    // -- Readers ---------------------------------------------------------

    pub fn video_params(&self) -> VideoParams {
        VideoParams { width: self.video_width, height: self.video_height }
    }

    pub fn chapters(&self) -> &[f64] {
        &self.chapters
    }

    pub fn duration(&self) -> f64 {
        self.duration
    }

    pub fn fullscreen(&self) -> bool {
        self.fullscreen
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    pub fn pause(&self) -> bool {
        self.pause
    }

    pub fn time_pos(&self) -> f64 {
        self.time_pos
    }

    pub fn time_position(&self) -> f64 {
        self.time_pos
    }

    /// The `qBound(0.0, timePosition, duration)` clamp consumers apply
    /// before using the raw position (asrcontroller.cpp:720, :922, :1013).
    /// See [`q_bound`] for the NaN caveat.
    pub fn time_position_clamped(&self) -> f64 {
        q_bound(0.0, self.time_pos, self.duration)
    }

    pub fn tracks(&self) -> &[Track] {
        &self.tracks
    }

    pub fn audio_tracks(&self) -> &[Track] {
        &self.audio_tracks
    }

    pub fn video_tracks(&self) -> &[Track] {
        &self.video_tracks
    }

    pub fn subtitle_tracks(&self) -> &[Track] {
        &self.subtitle_tracks
    }

    pub fn volume(&self) -> i64 {
        self.volume
    }

    pub fn max_volume(&self) -> i64 {
        self.max_volume
    }

    pub fn mute(&self) -> bool {
        self.mute
    }

    pub fn speed(&self) -> f64 {
        self.speed
    }

    pub fn sub_delay(&self) -> f64 {
        self.sub_delay
    }

    pub fn cursor_autohide(&self) -> CursorAutohide {
        self.cursor_autohide
    }

    pub fn aid(&self) -> i64 {
        self.aid
    }

    pub fn sid(&self) -> i64 {
        self.sid
    }

    pub fn secondary_sid(&self) -> i64 {
        self.secondary_sid
    }

    pub fn vid(&self) -> i64 {
        self.vid
    }

    pub fn subtitle(&self) -> &Subtitle {
        &self.subtitle
    }

    pub fn secondary_subtitle(&self) -> &Subtitle {
        &self.secondary_subtitle
    }

    pub fn cursor_auto_hide_type(&self) -> AutoHideType {
        self.cursor_auto_hide_type
    }

    pub fn cursor_auto_hide_time(&self) -> i64 {
        self.cursor_auto_hide_time
    }

    pub fn cursor_auto_hide_fullscreen_only(&self) -> bool {
        self.cursor_auto_hide_fullscreen_only
    }

    // -- Track selection lookups -----------------------------------------

    pub fn find_track_by_id(&self, id: i64) -> Option<&Track> {
        self.tracks
            .iter()
            .find(|t| t.id == id)
            .or_else(|| self.audio_tracks.iter().find(|t| t.id == id))
            .or_else(|| self.video_tracks.iter().find(|t| t.id == id))
            .or_else(|| self.subtitle_tracks.iter().find(|t| t.id == id))
    }

    pub fn active_subtitle_track(&self) -> Option<&Track> {
        if self.sid > 0 {
            self.find_track_by_id(self.sid)
        } else {
            self.subtitle_tracks
                .iter()
                .find(|t| t.selected)
                .or_else(|| {
                    self.tracks
                        .iter()
                        .find(|t| t.track_type == TrackType::Subtitle && t.selected)
                })
        }
    }

    pub fn active_secondary_subtitle_track(&self) -> Option<&Track> {
        if self.secondary_sid > 0 {
            self.find_track_by_id(self.secondary_sid)
        } else {
            self.subtitle_tracks
                .iter()
                .find(|t| t.id == self.secondary_sid)
        }
    }

    pub fn active_audio_track(&self) -> Option<&Track> {
        if self.aid > 0 {
            self.find_track_by_id(self.aid)
        } else {
            self.audio_tracks
                .iter()
                .find(|t| t.selected)
                .or_else(|| {
                    self.tracks
                        .iter()
                        .find(|t| t.track_type == TrackType::Audio && t.selected)
                })
        }
    }

    pub fn active_video_track(&self) -> Option<&Track> {
        if self.vid > 0 {
            self.find_track_by_id(self.vid)
        } else {
            self.video_tracks
                .iter()
                .find(|t| t.selected)
                .or_else(|| {
                    self.tracks
                        .iter()
                        .find(|t| t.track_type == TrackType::Video && t.selected)
                })
        }
    }

    pub fn tracks_by_type(&self, track_type: TrackType) -> impl Iterator<Item = &Track> {
        self.tracks.iter().filter(move |t| t.track_type == track_type)
    }

    // -- Writers (each returns the signals a Qt UI would receive) ---------

    fn gated_i64(
        slot: &mut i64,
        value: i64,
        signal: impl Fn(i64) -> Signal,
    ) -> Option<Signal> {
        if *slot == value {
            return None;
        }
        *slot = value;
        Some(signal(*slot))
    }

    fn gated_f64(
        slot: &mut f64,
        value: f64,
        signal: impl Fn(f64) -> Signal,
    ) -> Option<Signal> {
        if *slot == value {
            return None;
        }
        *slot = value;
        Some(signal(*slot))
    }

    fn gated_bool(
        slot: &mut bool,
        value: bool,
        signal: impl Fn(bool) -> Signal,
    ) -> Option<Signal> {
        if *slot == value {
            return None;
        }
        *slot = value;
        Some(signal(*slot))
    }

    pub fn set_video_width(&mut self, value: i64) -> Option<Signal> {
        Self::gated_i64(&mut self.video_width, value, Signal::VideoWidthChanged)
    }

    pub fn set_video_height(&mut self, value: i64) -> Option<Signal> {
        Self::gated_i64(&mut self.video_height, value, Signal::VideoHeightChanged)
    }

    /// `setChapters` — intentionally *not* change-gated (mpvstate.cpp:89-93).
    pub fn set_chapters(&mut self, values: Vec<f64>) -> Option<Signal> {
        self.chapters = values;
        Some(Signal::ChaptersChanged(self.chapters.clone()))
    }

    pub fn set_duration(&mut self, value: f64) -> Option<Signal> {
        Self::gated_f64(&mut self.duration, value, Signal::DurationChanged)
    }

    pub fn set_fullscreen(&mut self, value: bool) -> Option<Signal> {
        Self::gated_bool(&mut self.fullscreen, value, Signal::FullscreenChanged)
    }

    pub fn set_title(&mut self, value: impl Into<String>) -> Option<Signal> {
        let value = value.into();
        if self.title == value {
            return None;
        }
        self.title = value;
        self.title_c_str = CString::new(self.title.as_bytes()).unwrap_or_default();
        Some(Signal::TitleChanged(self.title.clone()))
    }

    pub fn set_path(&mut self, value: impl Into<String>) -> Option<Signal> {
        let value = value.into();
        if self.path == value {
            return None;
        }
        self.path = value;
        self.path_c_str = CString::new(self.path.as_bytes()).unwrap_or_default();
        Some(Signal::PathChanged(self.path.clone()))
    }

    pub fn set_pause(&mut self, value: bool) -> Option<Signal> {
        Self::gated_bool(&mut self.pause, value, Signal::PauseChanged)
    }

    pub fn set_time_position(&mut self, value: f64) -> Option<Signal> {
        Self::gated_f64(&mut self.time_pos, value, Signal::TimePositionChanged)
    }

    pub fn set_volume(&mut self, value: i64) -> Option<Signal> {
        Self::gated_i64(&mut self.volume, value, Signal::VolumeChanged)
    }

    pub fn set_max_volume(&mut self, value: i64) -> Option<Signal> {
        Self::gated_i64(&mut self.max_volume, value, Signal::MaxVolumeChanged)
    }

    pub fn set_mute(&mut self, value: bool) -> bool {
        if self.mute == value {
            false
        } else {
            self.mute = value;
            true
        }
    }

    pub fn set_speed(&mut self, value: f64) -> bool {
        if self.speed == value {
            false
        } else {
            self.speed = value;
            self.subtitle.set_speed(value);
            self.secondary_subtitle.set_speed(value);
            true
        }
    }

    pub fn set_sub_delay(&mut self, value: f64) -> bool {
        if self.sub_delay == value {
            false
        } else {
            self.sub_delay = value;
            self.subtitle.set_delay(value);
            true
        }
    }

    pub fn set_aid(&mut self, value: i64) -> Option<Signal> {
        Self::gated_i64(&mut self.aid, value, Signal::AidChanged)
    }

    pub fn set_sid(&mut self, value: i64) -> Option<Signal> {
        Self::gated_i64(&mut self.sid, value, Signal::SidChanged)
    }

    pub fn set_secondary_sid(&mut self, value: i64) -> Option<Signal> {
        Self::gated_i64(&mut self.secondary_sid, value, Signal::SecondarySidChanged)
    }

    pub fn set_vid(&mut self, value: i64) -> Option<Signal> {
        Self::gated_i64(&mut self.vid, value, Signal::VidChanged)
    }

    pub fn set_cursor_auto_hide_type(&mut self, value: AutoHideType) -> Option<Signal> {
        match value {
            AutoHideType::Number => {
                self.cursor_autohide = CursorAutohide::Number(self.cursor_auto_hide_time)
            }
            AutoHideType::Always => self.cursor_autohide = CursorAutohide::Always,
            AutoHideType::Never => self.cursor_autohide = CursorAutohide::Never,
        }
        if self.cursor_auto_hide_type == value {
            return None;
        }
        self.cursor_auto_hide_type = value;
        Some(Signal::CursorAutoHideTypeChanged(value))
    }

    pub fn set_cursor_auto_hide_time(&mut self, value: i64) -> Option<Signal> {
        self.cursor_autohide = CursorAutohide::Number(value);
        Self::gated_i64(
            &mut self.cursor_auto_hide_time,
            value,
            Signal::CursorAutoHideTimeChanged,
        )
    }

    pub fn set_cursor_auto_hide_fullscreen_only(&mut self, value: bool) -> Option<Signal> {
        Self::gated_bool(
            &mut self.cursor_auto_hide_fullscreen_only,
            value,
            Signal::CursorAutoHideFullscreenOnlyChanged,
        )
    }

    /// `MpvState::setTracks` — replaces all three buckets, partitioning by
    /// track type; `TrackType::None` entries are dropped. The three
    /// "changed" signals are emitted unconditionally, in the C++ order
    /// audio → video → subtitle (mpvstate.cpp:320-322).
    pub fn set_tracks(&mut self, tracks: Vec<Track>) -> Vec<Signal> {
        self.tracks = tracks.clone();
        self.audio_tracks.clear();
        self.video_tracks.clear();
        self.subtitle_tracks.clear();
        for track in tracks {
            match track.track_type {
                TrackType::Audio => self.audio_tracks.push(track),
                TrackType::Video => self.video_tracks.push(track),
                TrackType::Subtitle => self.subtitle_tracks.push(track),
                TrackType::None => {}
            }
        }
        vec![
            Signal::AudioTracksChanged,
            Signal::VideoTracksChanged,
            Signal::SubtitleTracksChanged,
        ]
    }

    /// Applies one decoded property change, returning the change-gated
    /// signals in emission order. `PropertyChange::Unavailable` mutates
    /// nothing — the stale value is retained, matching the C++ handlers'
    /// exact-format matching. `SubLines` is likewise not state: the machine
    /// turns it into the player-level primary-lines signal.
    pub fn apply_property(&mut self, property: PropertyChange) -> Vec<Signal> {
        match property {
            PropertyChange::Chapters(values) => self.set_chapters(values).into_iter().collect(),
            PropertyChange::Duration(value) => self.set_duration(value).into_iter().collect(),
            PropertyChange::Fullscreen(value) => self.set_fullscreen(value).into_iter().collect(),
            PropertyChange::MediaTitle(value) => self.set_title(value).into_iter().collect(),
            PropertyChange::Path(value) => self.set_path(value).into_iter().collect(),
            PropertyChange::Pause(value) => self.set_pause(value).into_iter().collect(),
            PropertyChange::TimePos(value) => {
                self.set_time_position(value).into_iter().collect()
            }
            PropertyChange::TrackList(tracks) => self.set_tracks(tracks),
            PropertyChange::Volume(value) => self.set_volume(value).into_iter().collect(),
            PropertyChange::VolumeMax(value) => self.set_max_volume(value).into_iter().collect(),
            PropertyChange::VideoWidth(value) => {
                self.set_video_width(value).into_iter().collect()
            }
            PropertyChange::VideoHeight(value) => {
                self.set_video_height(value).into_iter().collect()
            }

            PropertyChange::Aid(value) => self.set_aid(value).into_iter().collect(),
            PropertyChange::AidDisabled => {
                self.set_aid(0).into_iter().collect()
            }
            PropertyChange::Sid(value) => self.set_sid(value).into_iter().collect(),
            PropertyChange::SidDisabled => {
                let mut out = self.set_sid(0).into_iter().collect::<Vec<_>>();
                out.extend(
                    self.subtitle.reset_for_track_disabled().into_iter().map(|change| {
                        Signal::SubtitleChanged { secondary: false, change }
                    }),
                );
                self.sub_text_c_str = CString::new("").unwrap();
                out
            }
            PropertyChange::SecondarySid(value) => {
                self.set_secondary_sid(value).into_iter().collect()
            }
            PropertyChange::SecondarySidDisabled => {
                let mut out = self.set_secondary_sid(0).into_iter().collect::<Vec<_>>();
                out.extend(
                    self.secondary_subtitle.reset_for_track_disabled().into_iter().map(
                        |change| Signal::SubtitleChanged { secondary: true, change },
                    ),
                );
                self.secondary_sub_text_c_str = CString::new("").unwrap();
                out
            }
            PropertyChange::Vid(value) => self.set_vid(value).into_iter().collect(),
            PropertyChange::VidDisabled => self.set_vid(0).into_iter().collect(),

            PropertyChange::SubSpeed(value) => {
                self.speed = value;
                let mut out = Vec::with_capacity(2);
                out.extend(self.subtitle.set_speed(value).map(|change| {
                    Signal::SubtitleChanged { secondary: false, change }
                }));
                out.extend(self.secondary_subtitle.set_speed(value).map(|change| {
                    Signal::SubtitleChanged { secondary: true, change }
                }));
                out
            }
            PropertyChange::SubDelay(value) => {
                self.sub_delay = value;
                self.subtitle
                    .set_delay(value)
                    .map(|change| Signal::SubtitleChanged { secondary: false, change })
                    .into_iter()
                    .collect()
            }
            PropertyChange::SecondarySubDelay(value) => self
                .secondary_subtitle
                .set_delay(value)
                .map(|change| Signal::SubtitleChanged { secondary: true, change })
                .into_iter()
                .collect(),
            PropertyChange::SubText { text, start, end } => {
                let out = self
                    .subtitle
                    .apply_sub_text(text, start.unwrap_or(0.0), end.unwrap_or(0.0))
                    .into_iter()
                    .map(|change| Signal::SubtitleChanged { secondary: false, change })
                    .collect();
                self.sub_text_c_str = CString::new(self.subtitle.text().as_bytes()).unwrap_or_default();
                out
            }
            PropertyChange::SecondarySubText { text, start, end } => {
                let out = self
                    .secondary_subtitle
                    .apply_sub_text(text, start.unwrap_or(0.0), end.unwrap_or(0.0))
                    .into_iter()
                    .map(|change| Signal::SubtitleChanged { secondary: true, change })
                    .collect();
                self.secondary_sub_text_c_str = CString::new(self.secondary_subtitle.text().as_bytes()).unwrap_or_default();
                out
            }
            PropertyChange::SubVisibility(value) => self
                .subtitle
                .set_visible(value)
                .map(|change| Signal::SubtitleChanged { secondary: false, change })
                .into_iter()
                .collect(),
            PropertyChange::SecondarySubVisibility(value) => self
                .secondary_subtitle
                .set_visible(value)
                .map(|change| Signal::SubtitleChanged { secondary: true, change })
                .into_iter()
                .collect(),

            PropertyChange::CursorAutohide(autohide) => {
                self.cursor_autohide = autohide;
                match autohide {
                    CursorAutohide::Number(timeout) => {
                        let mut out = self
                            .set_cursor_auto_hide_time(timeout)
                            .into_iter()
                            .collect::<Vec<_>>();
                        out.extend(self.set_cursor_auto_hide_type(AutoHideType::Number));
                        out
                    }
                    CursorAutohide::Always => self
                        .set_cursor_auto_hide_type(AutoHideType::Always)
                        .into_iter()
                        .collect(),
                    CursorAutohide::Never => self
                        .set_cursor_auto_hide_type(AutoHideType::Never)
                        .into_iter()
                        .collect(),
                }
            }
            PropertyChange::CursorAutohideFullscreenOnly(value) => self
                .set_cursor_auto_hide_fullscreen_only(value)
                .into_iter()
                .collect(),

            PropertyChange::Unavailable(_) | PropertyChange::SubLines(_) => Vec::new(),
        }
    }

    // -- Bitflags update methods ------------------------------------------

    pub fn update_time_pos(&mut self, val: f64) -> u32 {
        if self.time_pos == val {
            0
        } else {
            self.time_pos = val;
            PROPERTY_POSITION_CHANGED
        }
    }

    pub fn update_duration(&mut self, val: f64) -> u32 {
        if self.duration == val {
            0
        } else {
            self.duration = val;
            PROPERTY_DURATION_CHANGED
        }
    }

    pub fn update_pause(&mut self, val: bool) -> u32 {
        if self.pause == val {
            0
        } else {
            self.pause = val;
            PROPERTY_PAUSE_CHANGED
        }
    }

    pub fn update_volume(&mut self, val: i64) -> u32 {
        if self.volume == val {
            0
        } else {
            self.volume = val;
            PROPERTY_VOLUME_CHANGED
        }
    }

    pub fn update_volume_max(&mut self, val: i64) -> u32 {
        if self.max_volume == val {
            0
        } else {
            self.max_volume = val;
            PROPERTY_VOLUME_MAX_CHANGED
        }
    }

    pub fn update_mute(&mut self, val: bool) -> u32 {
        if self.mute == val {
            0
        } else {
            self.mute = val;
            PROPERTY_MUTE_CHANGED
        }
    }

    pub fn update_speed(&mut self, val: f64) -> u32 {
        if self.speed == val {
            0
        } else {
            self.speed = val;
            self.subtitle.set_speed(val);
            self.secondary_subtitle.set_speed(val);
            PROPERTY_SPEED_CHANGED
        }
    }

    pub fn update_sub_delay(&mut self, val: f64) -> u32 {
        if self.sub_delay == val {
            0
        } else {
            self.sub_delay = val;
            self.subtitle.set_delay(val);
            PROPERTY_SUB_DELAY_CHANGED
        }
    }

    pub fn update_secondary_sub_delay(&mut self, val: f64) -> u32 {
        if self.secondary_subtitle.delay() == val {
            0
        } else {
            self.secondary_subtitle.set_delay(val);
            PROPERTY_SECONDARY_SUB_DELAY_CHANGED
        }
    }

    pub fn update_tracks(&mut self, tracks: Vec<Track>) -> u32 {
        self.tracks = tracks.clone();
        self.audio_tracks.clear();
        self.video_tracks.clear();
        self.subtitle_tracks.clear();
        for track in tracks {
            match track.track_type {
                TrackType::Audio => self.audio_tracks.push(track),
                TrackType::Video => self.video_tracks.push(track),
                TrackType::Subtitle => self.subtitle_tracks.push(track),
                TrackType::None => {}
            }
        }
        PROPERTY_TRACKS_CHANGED
            | PROPERTY_AUDIO_TRACKS_CHANGED
            | PROPERTY_VIDEO_TRACKS_CHANGED
            | PROPERTY_SUBTITLE_TRACKS_CHANGED
    }

    pub fn update_tracks_node(&mut self, node: *const c_void) -> u32 {
        if node.is_null() {
            return 0;
        }
        let track_list = unsafe { crate::mpv::memento_player_extract_tracks(node as *const _) };
        let mut tracks = Vec::with_capacity(track_list.len);
        for i in 0..track_list.len {
            let item = unsafe { &*track_list.items.add(i) };
            if item.track_type == 0 {
                continue;
            }
            let track = Track {
                id: item.id,
                track_type: match item.track_type {
                    1 => TrackType::Audio,
                    2 => TrackType::Video,
                    3 => TrackType::Subtitle,
                    _ => TrackType::None,
                },
                src_id: item.src_id,
                title: if item.title.is_null() {
                    String::new()
                } else {
                    unsafe { CStr::from_ptr(item.title) }
                        .to_string_lossy()
                        .into_owned()
                },
                language: if item.lang.is_null() {
                    String::new()
                } else {
                    unsafe { CStr::from_ptr(item.lang) }
                        .to_string_lossy()
                        .into_owned()
                },
                album_art: item.albumart,
                default_track: item.default_track,
                selected: item.selected,
                main_selection: item.main_selection,
                external: item.external,
                external_filename: if item.external_filename.is_null() {
                    String::new()
                } else {
                    unsafe { CStr::from_ptr(item.external_filename) }
                        .to_string_lossy()
                        .into_owned()
                },
                codec: if item.codec.is_null() {
                    String::new()
                } else {
                    unsafe { CStr::from_ptr(item.codec) }
                        .to_string_lossy()
                        .into_owned()
                },
            };
            tracks.push(track);
        }
        unsafe { crate::mpv::memento_player_tracks_free(track_list) };
        self.update_tracks(tracks)
    }

    pub fn update_chapters(&mut self, chapters: Vec<f64>) -> u32 {
        self.chapters = chapters;
        PROPERTY_CHAPTERS_CHANGED
    }

    pub fn update_chapters_node(&mut self, node: *const c_void) -> u32 {
        if node.is_null() {
            return 0;
        }
        let list = unsafe { crate::mpv::memento_player_extract_chapter_times(node as *const _) };
        let mut chapters = Vec::with_capacity(list.len);
        for i in 0..list.len {
            chapters.push(unsafe { *list.items.add(i) });
        }
        unsafe { crate::mpv::memento_player_f64_list_free(list) };
        self.update_chapters(chapters)
    }

    pub fn update_cursor_autohide(&mut self, autohide: CursorAutohide) -> u32 {
        self.cursor_autohide = autohide;
        match autohide {
            CursorAutohide::Number(timeout) => {
                let mut flags = 0;
                if self.cursor_auto_hide_time != timeout {
                    self.cursor_auto_hide_time = timeout;
                    flags |= PROPERTY_CURSOR_AUTOHIDE_TIME_CHANGED;
                }
                if self.cursor_auto_hide_type != AutoHideType::Number {
                    self.cursor_auto_hide_type = AutoHideType::Number;
                    flags |= PROPERTY_CURSOR_AUTOHIDE_TYPE_CHANGED;
                }
                if flags != 0 {
                    flags |= PROPERTY_CURSOR_AUTOHIDE_CHANGED;
                }
                flags
            }
            CursorAutohide::Always => {
                if self.cursor_auto_hide_type != AutoHideType::Always {
                    self.cursor_auto_hide_type = AutoHideType::Always;
                    PROPERTY_CURSOR_AUTOHIDE_CHANGED | PROPERTY_CURSOR_AUTOHIDE_TYPE_CHANGED
                } else {
                    0
                }
            }
            CursorAutohide::Never => {
                if self.cursor_auto_hide_type != AutoHideType::Never {
                    self.cursor_auto_hide_type = AutoHideType::Never;
                    PROPERTY_CURSOR_AUTOHIDE_CHANGED | PROPERTY_CURSOR_AUTOHIDE_TYPE_CHANGED
                } else {
                    0
                }
            }
        }
    }

    pub fn update_cursor_autohide_fs_only(&mut self, val: bool) -> u32 {
        if self.cursor_auto_hide_fullscreen_only == val {
            0
        } else {
            self.cursor_auto_hide_fullscreen_only = val;
            PROPERTY_CURSOR_AUTOHIDE_CHANGED | PROPERTY_CURSOR_AUTOHIDE_FS_ONLY_CHANGED
        }
    }

    pub fn update_fullscreen(&mut self, val: bool) -> u32 {
        if self.fullscreen == val {
            0
        } else {
            self.fullscreen = val;
            PROPERTY_FULLSCREEN_CHANGED
        }
    }

    pub fn update_title(&mut self, val: String) -> u32 {
        if self.title == val {
            0
        } else {
            self.title = val;
            self.title_c_str = CString::new(self.title.as_bytes()).unwrap_or_default();
            PROPERTY_TITLE_CHANGED
        }
    }

    pub fn update_path(&mut self, val: String) -> u32 {
        if self.path == val {
            0
        } else {
            self.path = val;
            self.path_c_str = CString::new(self.path.as_bytes()).unwrap_or_default();
            PROPERTY_PATH_CHANGED
        }
    }

    pub fn update_video_width(&mut self, val: i64) -> u32 {
        if self.video_width == val {
            0
        } else {
            self.video_width = val;
            PROPERTY_VIDEO_PARAMS_CHANGED | PROPERTY_VIDEO_WIDTH_CHANGED
        }
    }

    pub fn update_video_height(&mut self, val: i64) -> u32 {
        if self.video_height == val {
            0
        } else {
            self.video_height = val;
            PROPERTY_VIDEO_PARAMS_CHANGED | PROPERTY_VIDEO_HEIGHT_CHANGED
        }
    }

    pub fn update_aid(&mut self, val: i64) -> u32 {
        if self.aid == val {
            0
        } else {
            self.aid = val;
            PROPERTY_AID_CHANGED
        }
    }

    pub fn update_sid(&mut self, val: i64) -> u32 {
        if self.sid == val {
            0
        } else {
            self.sid = val;
            let mut flags = PROPERTY_SID_CHANGED;
            if val == 0 {
                let changes = self.subtitle.reset_for_track_disabled();
                if !changes.is_empty() {
                    flags |= PROPERTY_SUB_TEXT_CHANGED;
                }
            }
            flags
        }
    }

    pub fn update_secondary_sid(&mut self, val: i64) -> u32 {
        if self.secondary_sid == val {
            0
        } else {
            self.secondary_sid = val;
            let mut flags = PROPERTY_SECONDARY_SID_CHANGED;
            if val == 0 {
                let changes = self.secondary_subtitle.reset_for_track_disabled();
                if !changes.is_empty() {
                    flags |= PROPERTY_SECONDARY_SUB_TEXT_CHANGED;
                }
            }
            flags
        }
    }

    pub fn update_vid(&mut self, val: i64) -> u32 {
        if self.vid == val {
            0
        } else {
            self.vid = val;
            PROPERTY_VID_CHANGED
        }
    }

    pub fn update_sub_text(
        &mut self,
        text: String,
        start: Option<f64>,
        end: Option<f64>,
    ) -> u32 {
        let changes = self.subtitle.apply_sub_text(
            text,
            start.unwrap_or(0.0),
            end.unwrap_or(0.0),
        );
        if changes.is_empty() {
            0
        } else {
            PROPERTY_SUB_TEXT_CHANGED
        }
    }

    pub fn update_secondary_sub_text(
        &mut self,
        text: String,
        start: Option<f64>,
        end: Option<f64>,
    ) -> u32 {
        let changes = self.secondary_subtitle.apply_sub_text(
            text,
            start.unwrap_or(0.0),
            end.unwrap_or(0.0),
        );
        if changes.is_empty() {
            0
        } else {
            PROPERTY_SECONDARY_SUB_TEXT_CHANGED
        }
    }

    pub fn update_sub_visibility(&mut self, val: bool) -> u32 {
        if self.subtitle.set_visible(val).is_some() {
            PROPERTY_SUB_VISIBILITY_CHANGED
        } else {
            0
        }
    }

    pub fn update_secondary_sub_visibility(&mut self, val: bool) -> u32 {
        if self.secondary_subtitle.set_visible(val).is_some() {
            PROPERTY_SECONDARY_SUB_VISIBILITY_CHANGED
        } else {
            0
        }
    }

    pub fn update_property_raw(
        &mut self,
        property_name: &str,
        property_format: i32,
        property_data: *const c_void,
    ) -> (bool, u32) {
        if property_data.is_null() && property_format != 0 {
            return (false, 0);
        }

        match property_name {
            "time-pos" | "time_pos" | "position" => {
                if property_format == 5 /* DOUBLE */ {
                    let val = unsafe { *(property_data as *const f64) };
                    (true, self.update_time_pos(val))
                } else {
                    (false, 0)
                }
            }
            "duration" => {
                if property_format == 5 /* DOUBLE */ {
                    let val = unsafe { *(property_data as *const f64) };
                    (true, self.update_duration(val))
                } else {
                    (false, 0)
                }
            }
            "pause" => {
                if property_format == 3 /* FLAG */ {
                    let val = unsafe { *(property_data as *const c_int) } != 0;
                    (true, self.update_pause(val))
                } else if property_format == 4 /* INT64 */ {
                    let val = unsafe { *(property_data as *const i64) } != 0;
                    (true, self.update_pause(val))
                } else {
                    (false, 0)
                }
            }
            "volume" => {
                if property_format == 4 /* INT64 */ {
                    let val = unsafe { *(property_data as *const i64) };
                    (true, self.update_volume(val))
                } else if property_format == 5 /* DOUBLE */ {
                    let val = unsafe { *(property_data as *const f64) } as i64;
                    (true, self.update_volume(val))
                } else {
                    (false, 0)
                }
            }
            "volume-max" | "volume_max" | "max-volume" | "max_volume" => {
                if property_format == 4 /* INT64 */ {
                    let val = unsafe { *(property_data as *const i64) };
                    (true, self.update_volume_max(val))
                } else {
                    (false, 0)
                }
            }
            "mute" => {
                if property_format == 3 /* FLAG */ {
                    let val = unsafe { *(property_data as *const c_int) } != 0;
                    (true, self.update_mute(val))
                } else if property_format == 4 /* INT64 */ {
                    let val = unsafe { *(property_data as *const i64) } != 0;
                    (true, self.update_mute(val))
                } else {
                    (false, 0)
                }
            }
            "speed" => {
                if property_format == 5 /* DOUBLE */ {
                    let val = unsafe { *(property_data as *const f64) };
                    (true, self.update_speed(val))
                } else {
                    (false, 0)
                }
            }
            "sub-speed" | "sub_speed" => {
                if property_format == 5 /* DOUBLE */ {
                    let val = unsafe { *(property_data as *const f64) };
                    (true, self.update_speed(val))
                } else {
                    (false, 0)
                }
            }
            "sub-delay" | "sub_delay" => {
                if property_format == 5 /* DOUBLE */ {
                    let val = unsafe { *(property_data as *const f64) };
                    (true, self.update_sub_delay(val))
                } else {
                    (false, 0)
                }
            }
            "secondary-sub-delay" | "secondary_sub_delay" => {
                if property_format == 5 /* DOUBLE */ {
                    let val = unsafe { *(property_data as *const f64) };
                    (true, self.update_secondary_sub_delay(val))
                } else {
                    (false, 0)
                }
            }
            "track-list" | "track_list" | "tracks" => {
                if property_format == 6 || property_format == 7 /* NODE / NODE_ARRAY */ {
                    (true, self.update_tracks_node(property_data))
                } else {
                    (false, 0)
                }
            }
            "chapter-list" | "chapter_list" | "chapters" => {
                if property_format == 6 || property_format == 7 /* NODE / NODE_ARRAY */ {
                    (true, self.update_chapters_node(property_data))
                } else {
                    (false, 0)
                }
            }
            "cursor-autohide" | "cursor_autohide" => {
                let flags = self.update_cursor_autohide_from_raw(property_format, property_data);
                (true, flags)
            }
            "cursor-autohide-fs-only" | "cursor_autohide_fs_only" => {
                if property_format == 3 /* FLAG */ {
                    let val = unsafe { *(property_data as *const c_int) } != 0;
                    (true, self.update_cursor_autohide_fs_only(val))
                } else {
                    (false, 0)
                }
            }
            "fullscreen" => {
                if property_format == 3 /* FLAG */ {
                    let val = unsafe { *(property_data as *const c_int) } != 0;
                    (true, self.update_fullscreen(val))
                } else {
                    (false, 0)
                }
            }
            "media-title" | "media_title" | "title" => {
                if property_format == 1 || property_format == 2 /* STRING / OSD_STRING */ {
                    if let Some(s) = unsafe { extract_c_string(property_data) } {
                        (true, self.update_title(s))
                    } else {
                        (true, 0)
                    }
                } else {
                    (false, 0)
                }
            }
            "path" => {
                if property_format == 1 || property_format == 2 /* STRING / OSD_STRING */ {
                    if let Some(s) = unsafe { extract_c_string(property_data) } {
                        (true, self.update_path(s))
                    } else {
                        (true, 0)
                    }
                } else {
                    (false, 0)
                }
            }
            "video-params/w" | "video_params/w" | "video-width" | "video_width" => {
                if property_format == 4 /* INT64 */ {
                    let val = unsafe { *(property_data as *const i64) };
                    (true, self.update_video_width(val))
                } else {
                    (false, 0)
                }
            }
            "video-params/h" | "video_params/h" | "video-height" | "video_height" => {
                if property_format == 4 /* INT64 */ {
                    let val = unsafe { *(property_data as *const i64) };
                    (true, self.update_video_height(val))
                } else {
                    (false, 0)
                }
            }
            "aid" => {
                if property_format == 4 /* INT64 */ {
                    let val = unsafe { *(property_data as *const i64) };
                    (true, self.update_aid(val))
                } else if property_format == 3 /* FLAG */ {
                    let enabled = unsafe { *(property_data as *const c_int) } != 0;
                    if !enabled {
                        (true, self.update_aid(0))
                    } else {
                        (true, 0)
                    }
                } else {
                    (false, 0)
                }
            }
            "sid" => {
                if property_format == 4 /* INT64 */ {
                    let val = unsafe { *(property_data as *const i64) };
                    (true, self.update_sid(val))
                } else if property_format == 3 /* FLAG */ {
                    let enabled = unsafe { *(property_data as *const c_int) } != 0;
                    if !enabled {
                        (true, self.update_sid(0))
                    } else {
                        (true, 0)
                    }
                } else {
                    (false, 0)
                }
            }
            "secondary-sid" | "secondary_sid" => {
                if property_format == 4 /* INT64 */ {
                    let val = unsafe { *(property_data as *const i64) };
                    (true, self.update_secondary_sid(val))
                } else if property_format == 3 /* FLAG */ {
                    let enabled = unsafe { *(property_data as *const c_int) } != 0;
                    if !enabled {
                        (true, self.update_secondary_sid(0))
                    } else {
                        (true, 0)
                    }
                } else {
                    (false, 0)
                }
            }
            "vid" => {
                if property_format == 4 /* INT64 */ {
                    let val = unsafe { *(property_data as *const i64) };
                    (true, self.update_vid(val))
                } else if property_format == 3 /* FLAG */ {
                    let enabled = unsafe { *(property_data as *const c_int) } != 0;
                    if !enabled {
                        (true, self.update_vid(0))
                    } else {
                        (true, 0)
                    }
                } else {
                    (false, 0)
                }
            }
            "sub-visibility" | "sub_visibility" => {
                if property_format == 3 /* FLAG */ {
                    let val = unsafe { *(property_data as *const c_int) } != 0;
                    (true, self.update_sub_visibility(val))
                } else {
                    (false, 0)
                }
            }
            "secondary-sub-visibility" | "secondary_sub_visibility" => {
                if property_format == 3 /* FLAG */ {
                    let val = unsafe { *(property_data as *const c_int) } != 0;
                    (true, self.update_secondary_sub_visibility(val))
                } else {
                    (false, 0)
                }
            }
            "sub-text" | "sub_text" => {
                if property_format == 1 || property_format == 2 /* STRING / OSD_STRING */ {
                    if let Some(s) = unsafe { extract_c_string(property_data) } {
                        (true, self.update_sub_text(s, None, None))
                    } else {
                        (true, 0)
                    }
                } else {
                    (false, 0)
                }
            }
            "secondary-sub-text" | "secondary_sub_text" => {
                if property_format == 1 || property_format == 2 /* STRING / OSD_STRING */ {
                    if let Some(s) = unsafe { extract_c_string(property_data) } {
                        (true, self.update_secondary_sub_text(s, None, None))
                    } else {
                        (true, 0)
                    }
                } else {
                    (false, 0)
                }
            }
            _ => (false, 0),
        }
    }

    fn update_cursor_autohide_from_raw(&mut self, format: i32, data: *const c_void) -> u32 {
        if data.is_null() {
            return 0;
        }
        let mut int64_val = 0i64;
        let mut str_val: *const c_char = std::ptr::null();
        if format == 4 /* MPV_FORMAT_INT64 */ {
            int64_val = unsafe { *(data as *const i64) };
        } else if format == 1 || format == 2 /* MPV_FORMAT_STRING / OSD_STRING */ {
            if let Some(ptr) = unsafe { extract_c_str_ptr(data) } {
                str_val = ptr;
            }
        }
        let mut cursor_hidden = false;
        let mut timeout = 0i64;
        if unsafe {
            crate::mpv::memento_player_decode_cursor_autohide(
                format,
                int64_val,
                str_val,
                &mut cursor_hidden,
                &mut timeout,
            )
        } {
            if format == 4 /* MPV_FORMAT_INT64 */ {
                let mut flags = 0;
                if self.cursor_auto_hide_time != timeout {
                    self.cursor_auto_hide_time = timeout;
                    flags |= PROPERTY_CURSOR_AUTOHIDE_TIME_CHANGED;
                }
                if self.cursor_auto_hide_type != AutoHideType::Number {
                    self.cursor_auto_hide_type = AutoHideType::Number;
                    flags |= PROPERTY_CURSOR_AUTOHIDE_TYPE_CHANGED;
                }
                self.cursor_autohide = CursorAutohide::Number(timeout);
                if flags != 0 {
                    flags |= PROPERTY_CURSOR_AUTOHIDE_CHANGED;
                }
                flags
            } else {
                let new_type = if cursor_hidden {
                    AutoHideType::Always
                } else {
                    AutoHideType::Never
                };
                self.cursor_autohide = if cursor_hidden {
                    CursorAutohide::Always
                } else {
                    CursorAutohide::Never
                };
                if self.cursor_auto_hide_type != new_type {
                    self.cursor_auto_hide_type = new_type;
                    PROPERTY_CURSOR_AUTOHIDE_CHANGED | PROPERTY_CURSOR_AUTOHIDE_TYPE_CHANGED
                } else {
                    0
                }
            }
        } else {
            0
        }
    }
}

unsafe fn extract_c_str_ptr(data: *const c_void) -> Option<*const c_char> {
    if data.is_null() {
        return None;
    }
    let ptr_ptr = data as *const *const c_char;
    let s_ptr = *ptr_ptr;
    if !s_ptr.is_null() {
        Some(s_ptr)
    } else {
        Some(data as *const c_char)
    }
}

unsafe fn extract_c_string(data: *const c_void) -> Option<String> {
    if data.is_null() {
        return None;
    }
    let ptr_ptr = data as *const *const c_char;
    let s_ptr = *ptr_ptr;
    if !s_ptr.is_null() {
        if let Ok(cstr) = CStr::from_ptr(s_ptr).to_str() {
            return Some(cstr.to_string());
        }
    }
    let direct_ptr = data as *const c_char;
    if let Ok(cstr) = CStr::from_ptr(direct_ptr).to_str() {
        return Some(cstr.to_string());
    }
    None
}

// -- C ABI functions -----------------------------------------------------

#[no_mangle]
pub extern "C" fn memento_player_state_create() -> *mut MpvStateCore {
    Box::into_raw(Box::new(MpvStateCore::new()))
}

#[no_mangle]
pub unsafe extern "C" fn memento_player_state_free(state: *mut MpvStateCore) {
    if !state.is_null() {
        drop(unsafe { Box::from_raw(state) });
    }
}

#[no_mangle]
pub unsafe extern "C" fn memento_player_state_update_property(
    state: *mut MpvStateCore,
    property_name: *const c_char,
    property_format: i32,
    property_data: *const c_void,
    out_changed_flags: *mut u32,
) -> bool {
    if state.is_null() || property_name.is_null() {
        if !out_changed_flags.is_null() {
            *out_changed_flags = 0;
        }
        return false;
    }
    let name_str = match CStr::from_ptr(property_name).to_str() {
        Ok(s) => s,
        Err(_) => {
            if !out_changed_flags.is_null() {
                *out_changed_flags = 0;
            }
            return false;
        }
    };
    let (handled, flags) = (*state).update_property_raw(name_str, property_format, property_data);
    if !out_changed_flags.is_null() {
        *out_changed_flags = flags;
    }
    handled
}

#[no_mangle]
pub unsafe extern "C" fn memento_player_state_get_property(
    state: *const MpvStateCore,
    property_name: *const c_char,
    property_format: i32,
    out_data: *mut c_void,
) -> bool {
    if state.is_null() || property_name.is_null() || out_data.is_null() {
        return false;
    }
    let name = match CStr::from_ptr(property_name).to_str() {
        Ok(s) => s,
        Err(_) => return false,
    };
    let s = &*state;
    match name {
        "time-pos" | "time_pos" | "position" => {
            if property_format == 5 /* DOUBLE */ {
                *(out_data as *mut f64) = s.time_pos;
                return true;
            }
        }
        "duration" => {
            if property_format == 5 /* DOUBLE */ {
                *(out_data as *mut f64) = s.duration;
                return true;
            }
        }
        "pause" => {
            if property_format == 3 /* FLAG */ {
                *(out_data as *mut c_int) = if s.pause { 1 } else { 0 };
                return true;
            } else if property_format == 4 /* INT64 */ {
                *(out_data as *mut i64) = if s.pause { 1 } else { 0 };
                return true;
            }
        }
        "volume" => {
            if property_format == 4 /* INT64 */ {
                *(out_data as *mut i64) = s.volume;
                return true;
            } else if property_format == 5 /* DOUBLE */ {
                *(out_data as *mut f64) = s.volume as f64;
                return true;
            }
        }
        "volume-max" | "volume_max" | "max-volume" | "max_volume" => {
            if property_format == 4 /* INT64 */ {
                *(out_data as *mut i64) = s.max_volume;
                return true;
            }
        }
        "mute" => {
            if property_format == 3 /* FLAG */ {
                *(out_data as *mut c_int) = if s.mute { 1 } else { 0 };
                return true;
            }
        }
        "speed" => {
            if property_format == 5 /* DOUBLE */ {
                *(out_data as *mut f64) = s.speed;
                return true;
            }
        }
        "sub-delay" | "sub_delay" => {
            if property_format == 5 /* DOUBLE */ {
                *(out_data as *mut f64) = s.sub_delay;
                return true;
            }
        }
        "secondary-sub-delay" | "secondary_sub_delay" => {
            if property_format == 5 /* DOUBLE */ {
                *(out_data as *mut f64) = s.secondary_subtitle.delay();
                return true;
            }
        }
        "aid" => {
            if property_format == 4 /* INT64 */ {
                *(out_data as *mut i64) = s.aid;
                return true;
            }
        }
        "sid" => {
            if property_format == 4 /* INT64 */ {
                *(out_data as *mut i64) = s.sid;
                return true;
            }
        }
        "secondary-sid" | "secondary_sid" => {
            if property_format == 4 /* INT64 */ {
                *(out_data as *mut i64) = s.secondary_sid;
                return true;
            }
        }
        "vid" => {
            if property_format == 4 /* INT64 */ {
                *(out_data as *mut i64) = s.vid;
                return true;
            }
        }
        "fullscreen" => {
            if property_format == 3 /* FLAG */ {
                *(out_data as *mut c_int) = if s.fullscreen { 1 } else { 0 };
                return true;
            }
        }
        "media-title" | "media_title" | "title" => {
            if property_format == 1 /* STRING */ {
                *(out_data as *mut *const c_char) = s.title_c_str.as_ptr();
                return true;
            }
        }
        "path" => {
            if property_format == 1 /* STRING */ {
                *(out_data as *mut *const c_char) = s.path_c_str.as_ptr();
                return true;
            }
        }
        "cursor-autohide" | "cursor_autohide" => {
            if property_format == 4 /* INT64 */ {
                *(out_data as *mut i64) = s.cursor_auto_hide_time;
                return true;
            }
        }
        "cursor-autohide-fs-only" | "cursor_autohide_fs_only" => {
            if property_format == 3 /* FLAG */ {
                *(out_data as *mut c_int) = if s.cursor_auto_hide_fullscreen_only { 1 } else { 0 };
                return true;
            }
        }
        "video-params/w" | "video_params/w" | "video-width" | "video_width" => {
            if property_format == 4 /* INT64 */ {
                *(out_data as *mut i64) = s.video_width;
                return true;
            }
        }
        "video-params/h" | "video_params/h" | "video-height" | "video_height" => {
            if property_format == 4 /* INT64 */ {
                *(out_data as *mut i64) = s.video_height;
                return true;
            }
        }
        "track-list/count" | "tracks/count" => {
            if property_format == 4 /* INT64 */ {
                *(out_data as *mut i64) = s.tracks.len() as i64;
                return true;
            }
        }
        "chapter-list/count" | "chapters/count" => {
            if property_format == 4 /* INT64 */ {
                *(out_data as *mut i64) = s.chapters.len() as i64;
                return true;
            }
        }
        _ => {}
    }
    false
}

#[no_mangle]
pub unsafe extern "C" fn memento_player_state_find_track_by_id(
    state: *const MpvStateCore,
    id: i64,
    out_track_id: *mut i64,
    out_track_type: *mut i32,
) -> bool {
    if state.is_null() {
        return false;
    }
    if let Some(t) = (*state).find_track_by_id(id) {
        if !out_track_id.is_null() {
            *out_track_id = t.id;
        }
        if !out_track_type.is_null() {
            *out_track_type = t.track_type as i32;
        }
        true
    } else {
        false
    }
}

#[no_mangle]
pub unsafe extern "C" fn memento_player_state_active_subtitle_track(
    state: *const MpvStateCore,
    out_track_id: *mut i64,
) -> bool {
    if state.is_null() {
        return false;
    }
    if let Some(t) = (*state).active_subtitle_track() {
        if !out_track_id.is_null() {
            *out_track_id = t.id;
        }
        true
    } else {
        false
    }
}

#[no_mangle]
pub unsafe extern "C" fn memento_player_state_get_time_pos(state: *const MpvStateCore) -> f64 {
    if state.is_null() { 0.0 } else { (*state).time_pos }
}

#[no_mangle]
pub unsafe extern "C" fn memento_player_state_get_duration(state: *const MpvStateCore) -> f64 {
    if state.is_null() { 0.0 } else { (*state).duration }
}

#[no_mangle]
pub unsafe extern "C" fn memento_player_state_get_pause(state: *const MpvStateCore) -> bool {
    if state.is_null() { false } else { (*state).pause }
}

#[no_mangle]
pub unsafe extern "C" fn memento_player_state_get_volume(state: *const MpvStateCore) -> i64 {
    if state.is_null() { 0 } else { (*state).volume }
}

#[no_mangle]
pub unsafe extern "C" fn memento_player_state_get_mute(state: *const MpvStateCore) -> bool {
    if state.is_null() { false } else { (*state).mute }
}

#[no_mangle]
pub unsafe extern "C" fn memento_player_state_get_speed(state: *const MpvStateCore) -> f64 {
    if state.is_null() { 1.0 } else { (*state).speed }
}

#[no_mangle]
pub unsafe extern "C" fn memento_player_state_get_sub_delay(state: *const MpvStateCore) -> f64 {
    if state.is_null() { 0.0 } else { (*state).sub_delay }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{PropertyChange, PropertyName, SubLine};

    fn applied(state: &mut MpvStateCore, prop: PropertyChange) -> Vec<Signal> {
        state.apply_property(prop)
    }

    #[test]
    fn initial_defaults_match_mpvstate_header() {
        let state = MpvStateCore::new();
        assert_eq!(state.video_params(), VideoParams { width: 0, height: 0 });
        assert!(state.chapters().is_empty());
        assert_eq!(state.duration(), 0.0);
        assert!(!state.fullscreen());
        assert_eq!(state.title(), "");
        assert_eq!(state.path(), "");
        assert!(!state.pause());
        assert_eq!(state.time_position(), 0.0);
        assert_eq!(state.time_pos(), 0.0);
        assert_eq!(state.volume(), 0);
        assert!(!state.mute());
        assert_eq!(state.speed(), 1.0);
        assert_eq!(state.sub_delay(), 0.0);
        assert!(state.tracks().is_empty());
        assert!(state.audio_tracks().is_empty());
        assert!(state.video_tracks().is_empty());
        assert!(state.subtitle_tracks().is_empty());
        assert_eq!(state.max_volume(), 0);
        assert_eq!(state.aid(), 0);
        assert_eq!(state.sid(), 0);
        assert_eq!(state.secondary_sid(), 0);
        assert_eq!(state.vid(), 0);
        assert_eq!(state.subtitle(), &Subtitle::default());
        assert_eq!(state.secondary_subtitle(), &Subtitle::default());
        assert_eq!(state.cursor_auto_hide_type(), AutoHideType::Number);
        assert_eq!(state.cursor_auto_hide_time(), 0);
        assert!(!state.cursor_auto_hide_fullscreen_only());
    }

    #[test]
    fn change_gated_setters_suppress_duplicate_signals() {
        let mut state = MpvStateCore::new();
        assert!(state.set_title("a.mkv").is_some());
        assert_eq!(state.set_title("a.mkv"), None);
        assert!(state.set_pause(true).is_some());
        assert_eq!(state.set_pause(true), None);
        assert!(state.set_volume(50).is_some());
        assert_eq!(state.set_volume(50), None);
        assert!(state.set_fullscreen(true).is_some());
        assert_eq!(state.set_fullscreen(true), None);
        assert_eq!(
            state.set_time_position(3.0),
            Some(Signal::TimePositionChanged(3.0))
        );
        assert_eq!(state.set_time_position(3.0), None);
        assert_eq!(state.set_time_position(3.0), None);
    }

    #[test]
    fn chapters_are_not_change_gated() {
        let mut state = MpvStateCore::new();
        let first = state.set_chapters(vec![1.0, 2.0]).unwrap();
        assert_eq!(first, Signal::ChaptersChanged(vec![1.0, 2.0]));
        let again = state.set_chapters(vec![1.0, 2.0]).unwrap();
        assert_eq!(again, Signal::ChaptersChanged(vec![1.0, 2.0]));
        assert_eq!(
            state.set_chapters(vec![]).unwrap(),
            Signal::ChaptersChanged(vec![])
        );
    }

    #[test]
    fn duration_unavailable_notification_retains_stale_value() {
        let mut state = MpvStateCore::new();
        assert!(state.set_duration(120.5).is_some());
        let signals = applied(&mut state, PropertyChange::Unavailable(PropertyName::Duration));
        assert!(signals.is_empty());
        assert_eq!(state.duration(), 120.5);
        assert!(state.set_duration(60.0).is_some());
        assert_eq!(state.duration(), 60.0);
    }

    #[test]
    fn time_position_clamp_matches_qt_qbound_including_nan() {
        assert_eq!(q_bound(0.0, -5.0, 100.0), 0.0);
        assert_eq!(q_bound(0.0, 250.0, 100.0), 100.0);
        assert_eq!(q_bound(0.0, 42.0, 100.0), 42.0);
        assert_eq!(q_bound(0.0, f64::NAN, 100.0), 100.0);
        assert_eq!(q_bound(0.0, 7.0, 0.0), 0.0);

        let mut state = MpvStateCore::new();
        state.set_time_position(-1.0);
        assert_eq!(state.time_position_clamped(), 0.0);
        state.set_duration(90.0);
        state.set_time_position(120.0);
        assert_eq!(state.time_position_clamped(), 90.0);
        state.set_time_position(30.0);
        assert_eq!(state.time_position_clamped(), 30.0);
    }

    #[test]
    fn sub_text_updates_start_end_then_text_in_order() {
        let mut state = MpvStateCore::new();
        let signals = applied(
            &mut state,
            PropertyChange::SubText {
                text: "こんにちは".into(),
                start: Some(1.0),
                end: Some(3.5),
            },
        );
        assert_eq!(
            signals,
            vec![
                Signal::SubtitleChanged {
                    secondary: false,
                    change: SubtitleChange::StartTime(1.0)
                },
                Signal::SubtitleChanged {
                    secondary: false,
                    change: SubtitleChange::EndTime(3.5)
                },
                Signal::SubtitleChanged {
                    secondary: false,
                    change: SubtitleChange::Text("こんにちは".into())
                },
            ]
        );
        let quiet = applied(
            &mut state,
            PropertyChange::SubText {
                text: "こんにちは".into(),
                start: Some(1.0),
                end: Some(3.5),
            },
        );
        assert!(quiet.is_empty());
    }

    #[test]
    fn failed_sub_start_probe_defaults_to_zero() {
        let mut state = MpvStateCore::new();
        let signals = applied(
            &mut state,
            PropertyChange::SubText { text: "cue".into(), start: None, end: None },
        );
        assert_eq!(
            signals,
            vec![
                Signal::SubtitleChanged {
                    secondary: false,
                    change: SubtitleChange::Text("cue".into())
                },
            ]
        );
        assert_eq!(state.subtitle().start_time(), 0.0);
        assert_eq!(state.subtitle().end_time(), 0.0);
        assert_eq!(state.subtitle().text(), "cue");
    }

    #[test]
    fn sid_disabled_resets_primary_text_and_times_not_delay_speed() {
        let mut state = MpvStateCore::new();
        applied(&mut state, PropertyChange::SubDelay(-0.5));
        applied(&mut state, PropertyChange::SubSpeed(1.5));
        applied(
            &mut state,
            PropertyChange::SubText { text: "gone".into(), start: Some(2.0), end: Some(4.0) },
        );
        assert_eq!(state.sid(), 0);
        applied(&mut state, PropertyChange::Sid(3));
        assert_eq!(state.sid(), 3);

        let signals = applied(&mut state, PropertyChange::SidDisabled);
        assert_eq!(
            signals,
            vec![
                Signal::SidChanged(0),
                Signal::SubtitleChanged {
                    secondary: false,
                    change: SubtitleChange::StartTime(0.0)
                },
                Signal::SubtitleChanged {
                    secondary: false,
                    change: SubtitleChange::EndTime(0.0)
                },
                Signal::SubtitleChanged {
                    secondary: false,
                    change: SubtitleChange::Text("".into())
                },
            ]
        );
        assert_eq!(state.subtitle().delay(), -0.5);
        assert_eq!(state.subtitle().speed(), 1.5);
        assert_eq!(state.subtitle().text(), "");
        assert!(applied(&mut state, PropertyChange::SidDisabled).is_empty());
    }

    #[test]
    fn secondary_sid_disabled_resets_secondary_only() {
        let mut state = MpvStateCore::new();
        applied(
            &mut state,
            PropertyChange::SecondarySubText {
                text: "sec".into(),
                start: Some(1.0),
                end: Some(2.0),
            },
        );
        applied(&mut state, PropertyChange::SecondarySid(2));
        let signals = applied(&mut state, PropertyChange::SecondarySidDisabled);
        assert_eq!(
            signals,
            vec![
                Signal::SecondarySidChanged(0),
                Signal::SubtitleChanged {
                    secondary: true,
                    change: SubtitleChange::StartTime(0.0)
                },
                Signal::SubtitleChanged {
                    secondary: true,
                    change: SubtitleChange::EndTime(0.0)
                },
                Signal::SubtitleChanged {
                    secondary: true,
                    change: SubtitleChange::Text("".into())
                },
            ]
        );
        assert_eq!(state.subtitle().text(), "");
        assert_eq!(state.secondary_subtitle().text(), "");
    }

    #[test]
    fn sub_speed_applies_to_both_subtitles_and_validates() {
        let mut state = MpvStateCore::new();
        let signals = applied(&mut state, PropertyChange::SubSpeed(2.0));
        assert_eq!(
            signals,
            vec![
                Signal::SubtitleChanged {
                    secondary: false,
                    change: SubtitleChange::Speed(2.0)
                },
                Signal::SubtitleChanged {
                    secondary: true,
                    change: SubtitleChange::Speed(2.0)
                },
            ]
        );
        assert!(applied(&mut state, PropertyChange::SubSpeed(f64::NAN)).is_empty());
        assert!(applied(&mut state, PropertyChange::SubSpeed(0.0)).is_empty());
        assert!(applied(&mut state, PropertyChange::SubSpeed(2.0)).is_empty());
        assert_eq!(state.subtitle().speed(), 2.0);
        assert_eq!(state.secondary_subtitle().speed(), 2.0);
    }

    #[test]
    fn subtitle_time_math_is_delegated_not_re_ported() {
        let mut subtitle = Subtitle::default();
        subtitle.set_speed(1.25);
        subtitle.set_delay(0.5);
        assert_eq!(subtitle.playback_time(4.0), 4.0 * 1.25 + 0.5);
        assert_eq!(subtitle.raw_time(5.5), (5.5 - 0.5) / 1.25);
    }

    #[test]
    fn track_list_partitions_buckets_and_drops_unknown_types() {
        let mut state = MpvStateCore::new();
        let mk = |id: i64, track_type: TrackType| Track {
            id,
            track_type,
            ..Track::default()
        };
        let signals = applied(
            &mut state,
            PropertyChange::TrackList(vec![
                mk(1, TrackType::Audio),
                mk(2, TrackType::Video),
                mk(3, TrackType::Subtitle),
                mk(4, TrackType::None),
            ]),
        );
        assert_eq!(
            signals,
            vec![
                Signal::AudioTracksChanged,
                Signal::VideoTracksChanged,
                Signal::SubtitleTracksChanged,
            ]
        );
        assert_eq!(
            state.audio_tracks().iter().map(|t| t.id).collect::<Vec<_>>(),
            [1]
        );
        assert_eq!(
            state.video_tracks().iter().map(|t| t.id).collect::<Vec<_>>(),
            [2]
        );
        assert_eq!(
            state.subtitle_tracks().iter().map(|t| t.id).collect::<Vec<_>>(),
            [3]
        );

        let signals = applied(
            &mut state,
            PropertyChange::TrackList(vec![mk(9, TrackType::Subtitle)]),
        );
        assert_eq!(signals.len(), 3);
        assert!(state.audio_tracks().is_empty());
        assert!(state.video_tracks().is_empty());
        assert_eq!(
            state.subtitle_tracks().iter().map(|t| t.id).collect::<Vec<_>>(),
            [9]
        );
    }

    #[test]
    fn video_params_and_aspect_for_the_renderer() {
        let mut state = MpvStateCore::new();
        assert_eq!(state.video_params().aspect_ratio(), None);
        applied(&mut state, PropertyChange::VideoWidth(1920));
        assert_eq!(state.video_params().aspect_ratio(), None);
        applied(&mut state, PropertyChange::VideoHeight(1080));
        let params = state.video_params();
        assert!(params.is_valid());
        assert!((params.aspect_ratio().unwrap() - 16.0 / 9.0).abs() < 1e-12);
        assert_eq!(state.set_video_width(1920), None);
    }

    #[test]
    fn cursor_autohide_number_sets_time_then_type() {
        let mut state = MpvStateCore::new();
        assert_eq!(
            applied(
                &mut state,
                PropertyChange::CursorAutohide(CursorAutohide::Number(1000))
            ),
            vec![Signal::CursorAutoHideTimeChanged(1000)]
        );
        assert_eq!(
            applied(
                &mut state,
                PropertyChange::CursorAutohide(CursorAutohide::Always)
            ),
            vec![Signal::CursorAutoHideTypeChanged(AutoHideType::Always)]
        );
        assert_eq!(
            applied(
                &mut state,
                PropertyChange::CursorAutohide(CursorAutohide::Number(1000))
            ),
            vec![Signal::CursorAutoHideTypeChanged(AutoHideType::Number)]
        );
        assert!(applied(
            &mut state,
            PropertyChange::CursorAutohide(CursorAutohide::Number(1000))
        )
        .is_empty());

        assert_eq!(
            applied(
                &mut state,
                PropertyChange::CursorAutohide(CursorAutohide::Always)
            ),
            vec![Signal::CursorAutoHideTypeChanged(AutoHideType::Always)]
        );
        assert_eq!(state.cursor_auto_hide_time(), 1000);
        assert!(applied(
            &mut state,
            PropertyChange::CursorAutohide(CursorAutohide::Always)
        )
        .is_empty());
        assert_eq!(
            applied(
                &mut state,
                PropertyChange::CursorAutohide(CursorAutohide::Never)
            ),
            vec![Signal::CursorAutoHideTypeChanged(AutoHideType::Never)]
        );
    }

    #[test]
    fn fs_only_autohide_and_misc_properties() {
        let mut state = MpvStateCore::new();
        assert_eq!(
            applied(
                &mut state,
                PropertyChange::CursorAutohideFullscreenOnly(true)
            ),
            vec![Signal::CursorAutoHideFullscreenOnlyChanged(true)]
        );
        assert!(applied(
            &mut state,
            PropertyChange::CursorAutohideFullscreenOnly(true)
        )
        .is_empty());
        assert_eq!(
            applied(&mut state, PropertyChange::MediaTitle("Title".into())),
            vec![Signal::TitleChanged("Title".into())]
        );
        assert_eq!(
            applied(&mut state, PropertyChange::Path("/a.mkv".into())),
            vec![Signal::PathChanged("/a.mkv".into())]
        );
        assert_eq!(
            applied(&mut state, PropertyChange::VolumeMax(130)),
            vec![Signal::MaxVolumeChanged(130)]
        );
        assert_eq!(
            applied(&mut state, PropertyChange::Volume(100)),
            vec![Signal::VolumeChanged(100)]
        );
    }

    #[test]
    fn sub_lines_property_is_not_state() {
        let mut state = MpvStateCore::new();
        let lines = vec![SubLine { id: 1, text: "x".into(), start: 0.0, end: 1.0 }];
        assert!(applied(&mut state, PropertyChange::SubLines(lines)).is_empty());
        assert_eq!(
            applied(&mut state, PropertyChange::Chapters(vec![5.0])),
            vec![Signal::ChaptersChanged(vec![5.0])]
        );
    }

    #[test]
    fn raw_property_updates_and_bitflags() {
        let mut state = MpvStateCore::new();

        // Pause update
        let pause_val: c_int = 1;
        let (handled, flags) = state.update_property_raw(
            "pause",
            3, /* MPV_FORMAT_FLAG */
            &pause_val as *const c_int as *const c_void,
        );
        assert!(handled);
        assert_eq!(flags, PROPERTY_PAUSE_CHANGED);
        assert!(state.pause());

        // Re-delivery returns handled=true but flags=0
        let (_, flags2) = state.update_property_raw(
            "pause",
            3,
            &pause_val as *const c_int as *const c_void,
        );
        assert_eq!(flags2, 0);

        // Time pos update
        let pos_val: f64 = 42.5;
        let (handled, flags) = state.update_property_raw(
            "time-pos",
            5, /* MPV_FORMAT_DOUBLE */
            &pos_val as *const f64 as *const c_void,
        );
        assert!(handled);
        assert_eq!(flags, PROPERTY_POSITION_CHANGED);
        assert_eq!(state.time_pos(), 42.5);

        // Duration update
        let dur_val: f64 = 100.0;
        let (handled, flags) = state.update_property_raw(
            "duration",
            5,
            &dur_val as *const f64 as *const c_void,
        );
        assert!(handled);
        assert_eq!(flags, PROPERTY_DURATION_CHANGED);
        assert_eq!(state.duration(), 100.0);

        // Track lookups
        let t1 = Track {
            id: 1,
            track_type: TrackType::Audio,
            selected: true,
            ..Track::default()
        };
        let t2 = Track {
            id: 2,
            track_type: TrackType::Subtitle,
            selected: true,
            ..Track::default()
        };
        state.update_tracks(vec![t1.clone(), t2.clone()]);
        assert_eq!(state.find_track_by_id(1), Some(&t1));
        assert_eq!(state.find_track_by_id(2), Some(&t2));
        assert_eq!(state.find_track_by_id(999), None);
        assert_eq!(state.active_audio_track(), Some(&t1));
        assert_eq!(state.active_subtitle_track(), Some(&t2));

        // When sid is set explicitly
        state.update_sid(2);
        assert_eq!(state.active_subtitle_track(), Some(&t2));
    }

    #[test]
    fn c_abi_state_lifecycle_and_updates() {
        unsafe {
            let state = memento_player_state_create();
            assert!(!state.is_null());

            let mut flags: u32 = 0;
            let pause_val: c_int = 1;
            let pause_name = CString::new("pause").unwrap();
            let ok = memento_player_state_update_property(
                state,
                pause_name.as_ptr(),
                3,
                &pause_val as *const c_int as *const c_void,
                &mut flags,
            );
            assert!(ok);
            assert_eq!(flags, PROPERTY_PAUSE_CHANGED);
            assert!(memento_player_state_get_pause(state));

            let mut out_pause: c_int = 0;
            let ok_get = memento_player_state_get_property(
                state,
                pause_name.as_ptr(),
                3,
                &mut out_pause as *mut c_int as *mut c_void,
            );
            assert!(ok_get);
            assert_eq!(out_pause, 1);

            let pos_val: f64 = 15.5;
            let pos_name = CString::new("time-pos").unwrap();
            let ok = memento_player_state_update_property(
                state,
                pos_name.as_ptr(),
                5,
                &pos_val as *const f64 as *const c_void,
                &mut flags,
            );
            assert!(ok);
            assert_eq!(flags, PROPERTY_POSITION_CHANGED);
            assert_eq!(memento_player_state_get_time_pos(state), 15.5);

            memento_player_state_free(state);
        }
    }
}

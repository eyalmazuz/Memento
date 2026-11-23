//! Player controller connecting `subomi-player` to Slint `VideoSurface`.

mod coordinator;
mod mouse;
mod subtitle_sync;
mod visibility;

pub use coordinator::LivePlayerCoordinator;
pub use subomi_player::controller::{
    format_keypress, KEY_0, KEY_9, KEY_ALT, KEY_BACKSPACE, KEY_CLEAR, KEY_CONTROL, KEY_DELETE,
    KEY_DOWN, KEY_END, KEY_ENTER, KEY_ESCAPE, KEY_F1, KEY_F30, KEY_HOME, KEY_INSERT, KEY_LEFT,
    KEY_META, KEY_PAGE_DOWN, KEY_PAGE_UP, KEY_PAUSE, KEY_RETURN, KEY_RIGHT, KEY_SHIFT, KEY_UP,
    MOD_ALT, MOD_CONTROL, MOD_KEYPAD, MOD_META, MOD_NONE, MOD_SHIFT,
};
pub use mouse::MouseInputForwarder;
pub use subtitle_sync::{
    clear_primary_subtitle, clear_subtitle_marks_on_advance, refresh_active_rows,
    refresh_secondary_subtitle_list_with_timing, refresh_subtitle_list,
    refresh_subtitle_list_with_timing, refresh_subtitle_marks, sync_player_tracks, track_label,
};
pub use visibility::{
    compute_ass_overlay_visibility, compute_autofit_window_size, compute_mpv_subtitle_visibility,
    compute_mpv_subtitle_visibility_ex, compute_subtitle_overlay_visibility,
    compute_video_render_size, effective_media_title, filter_subtitle_text,
    is_image_subtitle_codec, mpv_cursor_should_hide, quantize_render_dimension,
    subtitle_cursor_zones,
};

/// Trait abstracting over types that can be checked for subtitle file extensions.
pub trait SubtitlePath {
    fn is_subtitle_file(&self) -> bool;
}

impl SubtitlePath for str {
    fn is_subtitle_file(&self) -> bool {
        subomi_player::controller::is_subtitle_file(self)
    }
}

impl SubtitlePath for &str {
    fn is_subtitle_file(&self) -> bool {
        subomi_player::controller::is_subtitle_file(self)
    }
}

impl SubtitlePath for String {
    fn is_subtitle_file(&self) -> bool {
        subomi_player::controller::is_subtitle_file(self)
    }
}

impl SubtitlePath for std::path::Path {
    fn is_subtitle_file(&self) -> bool {
        self.to_str()
            .is_some_and(subomi_player::controller::is_subtitle_file)
    }
}

impl SubtitlePath for std::path::PathBuf {
    fn is_subtitle_file(&self) -> bool {
        self.as_path().is_subtitle_file()
    }
}

/// Checks whether a given path has a supported subtitle file extension.
pub fn is_subtitle_file<P: SubtitlePath + ?Sized>(path: &P) -> bool {
    path.is_subtitle_file()
}

/// Formats seconds into a `"MM:SS"` or `"HH:MM:SS"` string.
pub fn format_time(sec: f64) -> String {
    format_time_internal(sec, false)
}

fn format_time_internal(sec: f64, force_hours: bool) -> String {
    let total = if sec.is_finite() && sec > 0.0 {
        sec as u64
    } else {
        0
    };
    let h = total / 3600;
    let m = (total % 3600) / 60;
    let s = total % 60;
    if force_hours || h > 0 {
        format!("{:02}:{:02}:{:02}", h, m, s)
    } else {
        format!("{:02}:{:02}", m, s)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct PlayerController {
    playing: bool,
    position: f64,
    duration: f64,
    volume: f64,
    mute: bool,
    speed: f64,
}

impl Default for PlayerController {
    fn default() -> Self {
        Self::new()
    }
}

impl PlayerController {
    pub fn new() -> Self {
        Self {
            playing: false,
            position: 0.0,
            duration: 0.0,
            volume: 100.0,
            mute: false,
            speed: 1.0,
        }
    }

    pub fn is_playing(&self) -> bool {
        self.playing
    }

    pub fn playing(&self) -> bool {
        self.playing
    }

    pub fn set_playing(&mut self, playing: bool) {
        self.playing = playing;
    }

    pub fn toggle_play(&mut self) -> bool {
        self.playing = !self.playing;
        self.playing
    }

    pub fn position(&self) -> f64 {
        self.position
    }

    pub fn set_position(&mut self, pos: f64) {
        self.position = pos;
    }

    pub fn duration(&self) -> f64 {
        self.duration
    }

    pub fn set_duration(&mut self, dur: f64) {
        self.duration = dur;
    }

    pub fn volume(&self) -> f64 {
        self.volume
    }

    pub fn set_volume(&mut self, vol: f64) {
        self.volume = vol.clamp(0.0, 100.0);
    }

    pub fn is_muted(&self) -> bool {
        self.mute
    }

    pub fn mute(&self) -> bool {
        self.mute
    }

    pub fn set_mute(&mut self, mute: bool) {
        self.mute = mute;
    }

    pub fn toggle_mute(&mut self) -> bool {
        self.mute = !self.mute;
        self.mute
    }

    pub fn speed(&self) -> f64 {
        self.speed
    }

    pub fn set_speed(&mut self, speed: f64) {
        self.speed = speed;
    }

    pub fn format_time(sec: f64) -> String {
        format_time(sec)
    }

    pub fn time_display(&self) -> String {
        let has_hours = self.duration >= 3600.0 || self.position >= 3600.0;
        format!(
            "{} / {}",
            format_time_internal(self.position, has_hours),
            format_time_internal(self.duration, has_hours)
        )
    }

    pub fn format_keypress(
        key: i32,
        modifiers: i32,
        is_macos: bool,
        fallback: &str,
    ) -> Option<String> {
        subomi_player::controller::format_keypress(key, modifiers, is_macos, fallback)
    }

    pub fn is_subtitle_file<P: SubtitlePath + ?Sized>(path: &P) -> bool {
        is_subtitle_file(path)
    }
}

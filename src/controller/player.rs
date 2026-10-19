//! Player controller connecting memento-player to Slint VideoSurface.

pub use memento_player::controller::{
    format_keypress, QT_ALT_MODIFIER, QT_CONTROL_MODIFIER, QT_KEYPAD_MODIFIER,
    QT_META_MODIFIER, QT_NO_MODIFIER, QT_SHIFT_MODIFIER,
    QT_KEY_0, QT_KEY_9, QT_KEY_ALT, QT_KEY_BACKSPACE, QT_KEY_CLEAR,
    QT_KEY_CONTROL, QT_KEY_DELETE, QT_KEY_DOWN, QT_KEY_END, QT_KEY_ENTER,
    QT_KEY_ESCAPE, QT_KEY_F1, QT_KEY_F30, QT_KEY_HOME, QT_KEY_INSERT,
    QT_KEY_LEFT, QT_KEY_META, QT_KEY_PAGE_DOWN, QT_KEY_PAGE_UP,
    QT_KEY_PAUSE, QT_KEY_RETURN, QT_KEY_RIGHT, QT_KEY_SHIFT, QT_KEY_UP,
};

/// Trait abstracting over types that can be checked for subtitle file extensions.
pub trait SubtitlePath {
    fn is_subtitle_file(&self) -> bool;
}

impl SubtitlePath for str {
    fn is_subtitle_file(&self) -> bool {
        let u16_vec: Vec<u16> = self.encode_utf16().collect();
        memento_player::controller::is_subtitle_file(&u16_vec)
    }
}

impl SubtitlePath for &str {
    fn is_subtitle_file(&self) -> bool {
        let u16_vec: Vec<u16> = self.encode_utf16().collect();
        memento_player::controller::is_subtitle_file(&u16_vec)
    }
}

impl SubtitlePath for String {
    fn is_subtitle_file(&self) -> bool {
        let u16_vec: Vec<u16> = self.encode_utf16().collect();
        memento_player::controller::is_subtitle_file(&u16_vec)
    }
}

impl SubtitlePath for [u16] {
    fn is_subtitle_file(&self) -> bool {
        memento_player::controller::is_subtitle_file(self)
    }
}

impl SubtitlePath for &[u16] {
    fn is_subtitle_file(&self) -> bool {
        memento_player::controller::is_subtitle_file(self)
    }
}

impl<const N: usize> SubtitlePath for [u16; N] {
    fn is_subtitle_file(&self) -> bool {
        memento_player::controller::is_subtitle_file(self)
    }
}

impl<const N: usize> SubtitlePath for &[u16; N] {
    fn is_subtitle_file(&self) -> bool {
        memento_player::controller::is_subtitle_file(*self)
    }
}

impl SubtitlePath for Vec<u16> {
    fn is_subtitle_file(&self) -> bool {
        memento_player::controller::is_subtitle_file(self.as_slice())
    }
}

impl SubtitlePath for std::path::Path {
    fn is_subtitle_file(&self) -> bool {
        if let Some(s) = self.to_str() {
            let u16_vec: Vec<u16> = s.encode_utf16().collect();
            memento_player::controller::is_subtitle_file(&u16_vec)
        } else {
            false
        }
    }
}

/// Checks whether a given path has a supported subtitle file extension.
pub fn is_subtitle_file<P: SubtitlePath + ?Sized>(path: &P) -> bool {
    path.is_subtitle_file()
}

/// Checks whether a UTF-16 code unit slice has a supported subtitle file extension.
pub fn is_subtitle_file_utf16(path: &[u16]) -> bool {
    memento_player::controller::is_subtitle_file(path)
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
        memento_player::controller::format_keypress(key, modifiers, is_macos, fallback)
    }

    pub fn is_subtitle_file<P: SubtitlePath + ?Sized>(path: &P) -> bool {
        is_subtitle_file(path)
    }
}

//! Player controller helpers: subtitle file extension checks, input formatting,
//! screenshot validation, and command-line option types.

// --- Subtitle Extensions ---

pub const SUBTITLE_EXTENSIONS: &[&str] = &[
    "ass", "idx", "lrc", "mks", "pgs", "rt", "scc", "smi", "srt", "ssa", "sub", "sup", "utf",
    "utf-8", "utf8", "vtt",
];

/// Checks whether a file path or bare extension has a supported subtitle extension.
pub fn is_subtitle_file(path: &str) -> bool {
    if path.is_empty() {
        return false;
    }
    let ext = match path.rfind('.') {
        Some(dot_idx) => &path[dot_idx + 1..],
        None => path,
    };
    if ext.len() < 2 || ext.len() > 5 || !ext.is_ascii() {
        return false;
    }
    SUBTITLE_EXTENSIONS
        .iter()
        .any(|known| ext.eq_ignore_ascii_case(known))
}

// --- Key & Modifier Constants ---

pub const MOD_NONE: i32 = 0x0000_0000;
pub const MOD_SHIFT: i32 = 0x0200_0000;
pub const MOD_CONTROL: i32 = 0x0400_0000;
pub const MOD_ALT: i32 = 0x0800_0000;
pub const MOD_META: i32 = 0x1000_0000;
pub const MOD_KEYPAD: i32 = 0x2000_0000;

pub const KEY_ESCAPE: i32 = 0x0100_0000;
pub const KEY_BACKSPACE: i32 = 0x0100_0003;
pub const KEY_RETURN: i32 = 0x0100_0004;
pub const KEY_ENTER: i32 = 0x0100_0005;
pub const KEY_INSERT: i32 = 0x0100_0006;
pub const KEY_DELETE: i32 = 0x0100_0007;
pub const KEY_PAUSE: i32 = 0x0100_0008;
pub const KEY_CLEAR: i32 = 0x0100_000b;
pub const KEY_HOME: i32 = 0x0100_0010;
pub const KEY_END: i32 = 0x0100_0011;
pub const KEY_LEFT: i32 = 0x0100_0012;
pub const KEY_UP: i32 = 0x0100_0013;
pub const KEY_RIGHT: i32 = 0x0100_0014;
pub const KEY_DOWN: i32 = 0x0100_0015;
pub const KEY_PAGE_UP: i32 = 0x0100_0016;
pub const KEY_PAGE_DOWN: i32 = 0x0100_0017;

pub const KEY_SHIFT: i32 = 0x0100_0020;
pub const KEY_CONTROL: i32 = 0x0100_0021;
pub const KEY_META: i32 = 0x0100_0022;
pub const KEY_ALT: i32 = 0x0100_0023;

pub const KEY_F1: i32 = 0x0100_0030;
pub const KEY_F30: i32 = 0x0100_004d;

pub const KEY_0: i32 = 0x30;
pub const KEY_9: i32 = 0x39;

// --- Modifier Formatting ---

/// Formats keyboard modifiers into mpv modifier prefix order: `Shift+Ctrl+Alt+Meta+`.
pub fn format_modifiers(modifiers: i32) -> String {
    let mut s = String::new();
    if (modifiers & MOD_SHIFT) != 0 {
        s.push_str("Shift+");
    }
    if (modifiers & MOD_CONTROL) != 0 {
        s.push_str("Ctrl+");
    }
    if (modifiers & MOD_ALT) != 0 {
        s.push_str("Alt+");
    }
    if (modifiers & MOD_META) != 0 {
        s.push_str("Meta+");
    }
    s
}

// --- Mouse Buttons ---

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
    Back,
    Forward,
}

impl MouseButton {
    pub fn from_button_mask(button: u32) -> Option<Self> {
        match button {
            1 => Some(Self::Left),
            2 => Some(Self::Right),
            4 => Some(Self::Middle),
            8 => Some(Self::Back),
            16 => Some(Self::Forward),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Left => "MOUSE_BTN0",
            Self::Middle => "MOUSE_BTN1",
            Self::Right => "MOUSE_BTN2",
            Self::Back => "MOUSE_BTN7",
            Self::Forward => "MOUSE_BTN8",
        }
    }
}

// --- Wheel Buttons ---

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WheelButton {
    Up,
    Down,
    Left,
    Right,
}

impl WheelButton {
    pub fn from_deltas(delta_x: i32, delta_y: i32) -> Option<Self> {
        if delta_y > 0 {
            Some(Self::Up)
        } else if delta_y < 0 {
            Some(Self::Down)
        } else if delta_x < 0 {
            Some(Self::Left)
        } else if delta_x > 0 {
            Some(Self::Right)
        } else {
            None
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Up => "3",
            Self::Down => "4",
            Self::Left => "5",
            Self::Right => "6",
        }
    }
}

// --- Key Mapping & Formatting ---

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeypadPrefix {
    None,
    Number,
    Navigation,
}

impl KeypadPrefix {
    pub fn for_key(key: i32, modifiers: i32, is_macos: bool) -> Self {
        if is_macos || (modifiers & MOD_KEYPAD) == 0 {
            return Self::None;
        }
        match key {
            KEY_0..=KEY_9 => Self::Number,
            KEY_DELETE
            | KEY_INSERT
            | KEY_HOME
            | KEY_END
            | KEY_CLEAR
            | KEY_PAGE_UP
            | KEY_PAGE_DOWN
            | KEY_RIGHT
            | KEY_LEFT
            | KEY_DOWN
            | KEY_UP
            | KEY_ENTER => Self::Navigation,
            _ => Self::None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "",
            Self::Number => "KP",
            Self::Navigation => "KP_",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyMapping {
    BareModifier,
    Named(&'static str),
    FunctionKey(u32),
    Fallback,
}

impl KeyMapping {
    pub fn from_key(key: i32) -> Self {
        if (KEY_F1..=KEY_F30).contains(&key) {
            return Self::FunctionKey((key - KEY_F1 + 1) as u32);
        }
        match key {
            KEY_SHIFT | KEY_CONTROL | KEY_ALT | KEY_META => Self::BareModifier,
            KEY_LEFT => Self::Named("LEFT"),
            KEY_RIGHT => Self::Named("RIGHT"),
            KEY_UP => Self::Named("UP"),
            KEY_DOWN => Self::Named("DOWN"),
            KEY_ENTER | KEY_RETURN => Self::Named("ENTER"),
            KEY_ESCAPE => Self::Named("ESC"),
            KEY_BACKSPACE => Self::Named("BS"),
            KEY_PAUSE => Self::Named("PAUSE"),
            KEY_DELETE => Self::Named("DEL"),
            KEY_INSERT => Self::Named("INS"),
            KEY_HOME => Self::Named("HOME"),
            KEY_END => Self::Named("END"),
            KEY_CLEAR => Self::Named("DEC"),
            KEY_PAGE_UP => Self::Named("PGUP"),
            KEY_PAGE_DOWN => Self::Named("PGDWN"),
            _ => Self::Fallback,
        }
    }
}

pub fn format_keypress(
    key: i32,
    modifiers: i32,
    is_macos: bool,
    fallback: &str,
) -> Option<String> {
    let mapping = KeyMapping::from_key(key);
    if matches!(mapping, KeyMapping::BareModifier) {
        return None;
    }

    let mut result = format_modifiers(modifiers);
    result.push_str(KeypadPrefix::for_key(key, modifiers, is_macos).as_str());

    match mapping {
        KeyMapping::BareModifier => unreachable!(),
        KeyMapping::Named(name) => result.push_str(name),
        KeyMapping::FunctionKey(num) => {
            use std::fmt::Write;
            let _ = write!(result, "F{num}");
        }
        KeyMapping::Fallback => {
            if fallback.trim().is_empty() {
                return None;
            }
            if (modifiers & MOD_SHIFT) != 0 {
                result.push_str(fallback);
            } else {
                result.push_str(&fallback.to_lowercase());
            }
        }
    }
    Some(result)
}

// --- Screenshot Validation ---

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScreenshotValidationError {
    UnexpectedFormat = 1,
    InvalidDimensions = 2,
    InvalidStride = 3,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScreenshotSpec;

impl ScreenshotSpec {
    #[allow(clippy::too_many_arguments)]
    pub fn validate(
        has_w: bool,
        w: i64,
        has_h: bool,
        h: i64,
        has_stride: bool,
        stride: i64,
        format: Option<&str>,
        has_data: bool,
    ) -> Result<i64, ScreenshotValidationError> {
        if !has_w || !has_h || !has_stride || format != Some("rgba") || !has_data {
            return Err(ScreenshotValidationError::UnexpectedFormat);
        }
        if w <= 0 || h <= 0 || w > i32::MAX as i64 || h > i32::MAX as i64 {
            return Err(ScreenshotValidationError::InvalidDimensions);
        }
        let min_line_size = w.saturating_mul(4);
        if stride.unsigned_abs() < min_line_size as u64 {
            return Err(ScreenshotValidationError::InvalidStride);
        }
        Ok(min_line_size)
    }
}

// --- Command Line Options ---

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliOption {
    pub name: String,
    pub value: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subtitle_file_extension_checks() {
        assert!(is_subtitle_file("video.srt"));
        assert!(is_subtitle_file("video.SRT"));
        assert!(is_subtitle_file("video.Srt"));
        assert!(is_subtitle_file("video.ass"));
        assert!(is_subtitle_file("video.vtt"));
        assert!(is_subtitle_file("video.utf-8"));
        assert!(is_subtitle_file("video.utf8"));
        assert!(is_subtitle_file("video.utf"));
        assert!(is_subtitle_file("video.rt"));
        assert!(is_subtitle_file("video.idx"));
        assert!(is_subtitle_file(".vtt"));
        assert!(is_subtitle_file("srt"));
        assert!(is_subtitle_file("ASS"));
        assert!(is_subtitle_file("utf-8"));

        assert!(!is_subtitle_file("video.mp4"));
        assert!(!is_subtitle_file("video.mkv"));
        assert!(!is_subtitle_file("video.txt"));
        assert!(!is_subtitle_file("video."));
        assert!(!is_subtitle_file(""));
        assert!(!is_subtitle_file("mp4"));
        assert!(!is_subtitle_file("video.ass.mp4"));
    }

    #[test]
    fn modifier_formatting() {
        assert_eq!(format_modifiers(0), "");
        assert_eq!(format_modifiers(MOD_SHIFT), "Shift+");
        assert_eq!(format_modifiers(MOD_CONTROL), "Ctrl+");
        assert_eq!(format_modifiers(MOD_ALT), "Alt+");
        assert_eq!(format_modifiers(MOD_META), "Meta+");
        assert_eq!(format_modifiers(MOD_SHIFT | MOD_CONTROL), "Shift+Ctrl+");
        assert_eq!(
            format_modifiers(MOD_SHIFT | MOD_CONTROL | MOD_ALT | MOD_META),
            "Shift+Ctrl+Alt+Meta+"
        );
        assert_eq!(format_modifiers(MOD_KEYPAD | MOD_SHIFT), "Shift+");
    }

    #[test]
    fn mouse_button_mappings() {
        assert_eq!(MouseButton::from_button_mask(1), Some(MouseButton::Left));
        assert_eq!(MouseButton::from_button_mask(2), Some(MouseButton::Right));
        assert_eq!(MouseButton::from_button_mask(4), Some(MouseButton::Middle));
        assert_eq!(MouseButton::from_button_mask(8), Some(MouseButton::Back));
        assert_eq!(MouseButton::from_button_mask(16), Some(MouseButton::Forward));
        assert_eq!(MouseButton::from_button_mask(0), None);
        assert_eq!(MouseButton::from_button_mask(32), None);

        assert_eq!(MouseButton::Left.as_str(), "MOUSE_BTN0");
        assert_eq!(MouseButton::Middle.as_str(), "MOUSE_BTN1");
        assert_eq!(MouseButton::Right.as_str(), "MOUSE_BTN2");
        assert_eq!(MouseButton::Back.as_str(), "MOUSE_BTN7");
        assert_eq!(MouseButton::Forward.as_str(), "MOUSE_BTN8");
    }

    #[test]
    fn wheel_button_mappings() {
        assert_eq!(WheelButton::from_deltas(0, 15), Some(WheelButton::Up));
        assert_eq!(WheelButton::from_deltas(0, -15), Some(WheelButton::Down));
        assert_eq!(WheelButton::from_deltas(-10, 0), Some(WheelButton::Left));
        assert_eq!(WheelButton::from_deltas(10, 0), Some(WheelButton::Right));
        assert_eq!(WheelButton::from_deltas(0, 0), None);
        assert_eq!(WheelButton::from_deltas(10, 15), Some(WheelButton::Up));
        assert_eq!(WheelButton::from_deltas(-10, -15), Some(WheelButton::Down));

        assert_eq!(WheelButton::Up.as_str(), "3");
        assert_eq!(WheelButton::Down.as_str(), "4");
        assert_eq!(WheelButton::Left.as_str(), "5");
        assert_eq!(WheelButton::Right.as_str(), "6");
    }

    #[test]
    fn keypress_formatting() {
        assert_eq!(format_keypress(KEY_SHIFT, MOD_SHIFT, false, ""), None);
        assert_eq!(format_keypress(KEY_CONTROL, 0, false, ""), None);
        assert_eq!(format_keypress(KEY_ALT, 0, false, ""), None);
        assert_eq!(format_keypress(KEY_META, 0, false, ""), None);

        assert_eq!(format_keypress(KEY_LEFT, 0, false, "").as_deref(), Some("LEFT"));
        assert_eq!(
            format_keypress(KEY_RIGHT, MOD_CONTROL, false, "").as_deref(),
            Some("Ctrl+RIGHT")
        );
        assert_eq!(format_keypress(KEY_ENTER, 0, false, "").as_deref(), Some("ENTER"));
        assert_eq!(format_keypress(KEY_RETURN, 0, false, "").as_deref(), Some("ENTER"));
        assert_eq!(format_keypress(KEY_ESCAPE, 0, false, "").as_deref(), Some("ESC"));
        assert_eq!(format_keypress(KEY_BACKSPACE, 0, false, "").as_deref(), Some("BS"));
        assert_eq!(format_keypress(KEY_DELETE, 0, false, "").as_deref(), Some("DEL"));
        assert_eq!(format_keypress(KEY_CLEAR, 0, false, "").as_deref(), Some("DEC"));
        assert_eq!(format_keypress(KEY_F1, 0, false, "").as_deref(), Some("F1"));
        assert_eq!(
            format_keypress(KEY_F30, MOD_ALT, false, "").as_deref(),
            Some("Alt+F30")
        );

        assert_eq!(
            format_keypress(KEY_0, MOD_KEYPAD, false, "0").as_deref(),
            Some("KP0")
        );
        assert_eq!(
            format_keypress(KEY_DELETE, MOD_KEYPAD, false, "Del").as_deref(),
            Some("KP_DEL")
        );
        assert_eq!(
            format_keypress(KEY_ENTER, MOD_KEYPAD, false, "").as_deref(),
            Some("KP_ENTER")
        );

        assert_eq!(
            format_keypress(KEY_0, MOD_KEYPAD, true, "0").as_deref(),
            Some("0")
        );
        assert_eq!(
            format_keypress(KEY_DELETE, MOD_KEYPAD, true, "Del").as_deref(),
            Some("DEL")
        );

        assert_eq!(format_keypress(0x41, 0, false, "A").as_deref(), Some("a"));
        assert_eq!(
            format_keypress(0x41, MOD_SHIFT, false, "A").as_deref(),
            Some("Shift+A")
        );

        assert_eq!(format_keypress(0x1234, MOD_CONTROL, false, ""), None);
        assert_eq!(format_keypress(0x1234, 0, false, "   "), None);
    }

    #[test]
    fn screenshot_validation() {
        assert_eq!(
            ScreenshotSpec::validate(true, 1920, true, 1080, true, 1920 * 4, Some("rgba"), true),
            Ok(1920 * 4)
        );
        assert_eq!(
            ScreenshotSpec::validate(true, 1920, true, 1080, true, -(1920 * 4), Some("rgba"), true),
            Ok(1920 * 4)
        );
        assert_eq!(
            ScreenshotSpec::validate(false, 1920, true, 1080, true, 1920 * 4, Some("rgba"), true),
            Err(ScreenshotValidationError::UnexpectedFormat)
        );
        assert_eq!(
            ScreenshotSpec::validate(true, 1920, false, 1080, true, 1920 * 4, Some("rgba"), true),
            Err(ScreenshotValidationError::UnexpectedFormat)
        );
        assert_eq!(
            ScreenshotSpec::validate(true, 1920, true, 1080, false, 1920 * 4, Some("rgba"), true),
            Err(ScreenshotValidationError::UnexpectedFormat)
        );
        assert_eq!(
            ScreenshotSpec::validate(true, 1920, true, 1080, true, 1920 * 4, Some("bgra"), true),
            Err(ScreenshotValidationError::UnexpectedFormat)
        );
        assert_eq!(
            ScreenshotSpec::validate(true, 1920, true, 1080, true, 1920 * 4, None, true),
            Err(ScreenshotValidationError::UnexpectedFormat)
        );
        assert_eq!(
            ScreenshotSpec::validate(true, 1920, true, 1080, true, 1920 * 4, Some("rgba"), false),
            Err(ScreenshotValidationError::UnexpectedFormat)
        );
        assert_eq!(
            ScreenshotSpec::validate(true, 0, true, 1080, true, 1920 * 4, Some("rgba"), true),
            Err(ScreenshotValidationError::InvalidDimensions)
        );
        assert_eq!(
            ScreenshotSpec::validate(true, 1920, true, -1, true, 1920 * 4, Some("rgba"), true),
            Err(ScreenshotValidationError::InvalidDimensions)
        );
        assert_eq!(
            ScreenshotSpec::validate(
                true,
                i32::MAX as i64 + 1,
                true,
                1080,
                true,
                1920 * 4,
                Some("rgba"),
                true
            ),
            Err(ScreenshotValidationError::InvalidDimensions)
        );
        assert_eq!(
            ScreenshotSpec::validate(true, 1920, true, 1080, true, 1920 * 4 - 1, Some("rgba"), true),
            Err(ScreenshotValidationError::InvalidStride)
        );
        assert_eq!(
            ScreenshotSpec::validate(true, 1920, true, 1080, true, 0, Some("rgba"), true),
            Err(ScreenshotValidationError::InvalidStride)
        );
    }
}

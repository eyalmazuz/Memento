//! Controller helpers for `MpvController` and `MpvPlayer`: subtitle file extension
//! checks, input formatting, screenshot validation, and command-line option parsing.

use std::ffi::{c_char, c_void, CStr, CString};

// --- Subtitle Extensions ---

pub const SUBTITLE_EXTENSIONS: &[&[u8]] = &[
    b"ass", b"idx", b"lrc", b"mks", b"pgs", b"rt", b"scc", b"smi", b"srt", b"ssa", b"sub",
    b"sup", b"utf", b"utf-8", b"utf8", b"vtt",
];

/// Checks if a file path (as UTF-16 code units) has a subtitle file extension.
///
/// Matches `MpvController::isSubtitleFile(const QString &file)`.
/// In Qt, `file.lastIndexOf('.')` returns -1 when no dot exists;
/// `file.right(file.size() - (-1) - 1)` evaluates to `file.right(file.size())`,
/// checking the whole string as the extension.
pub fn is_subtitle_file(path: &[u16]) -> bool {
    if path.is_empty() {
        return false;
    }

    let ext_slice = match path.iter().rposition(|&unit| unit == b'.' as u16) {
        Some(dot_idx) => &path[dot_idx + 1..],
        None => path,
    };

    if ext_slice.is_empty() || ext_slice.len() > 5 || ext_slice.len() < 2 {
        return false;
    }

    let mut ascii_ext = [0u8; 5];
    for (i, &unit) in ext_slice.iter().enumerate() {
        if unit > 127 {
            return false;
        }
        ascii_ext[i] = (unit as u8).to_ascii_lowercase();
    }

    SUBTITLE_EXTENSIONS.contains(&&ascii_ext[..ext_slice.len()])
}

// --- Qt Key & Modifier Constants ---

pub const QT_NO_MODIFIER: i32 = 0x0000_0000;
pub const QT_SHIFT_MODIFIER: i32 = 0x0200_0000;
pub const QT_CONTROL_MODIFIER: i32 = 0x0400_0000;
pub const QT_ALT_MODIFIER: i32 = 0x0800_0000;
pub const QT_META_MODIFIER: i32 = 0x1000_0000;
pub const QT_KEYPAD_MODIFIER: i32 = 0x2000_0000;

pub const QT_KEY_ESCAPE: i32 = 0x0100_0000;
pub const QT_KEY_BACKSPACE: i32 = 0x0100_0003;
pub const QT_KEY_RETURN: i32 = 0x0100_0004;
pub const QT_KEY_ENTER: i32 = 0x0100_0005;
pub const QT_KEY_INSERT: i32 = 0x0100_0006;
pub const QT_KEY_DELETE: i32 = 0x0100_0007;
pub const QT_KEY_PAUSE: i32 = 0x0100_0008;
pub const QT_KEY_CLEAR: i32 = 0x0100_000b;
pub const QT_KEY_HOME: i32 = 0x0100_0010;
pub const QT_KEY_END: i32 = 0x0100_0011;
pub const QT_KEY_LEFT: i32 = 0x0100_0012;
pub const QT_KEY_UP: i32 = 0x0100_0013;
pub const QT_KEY_RIGHT: i32 = 0x0100_0014;
pub const QT_KEY_DOWN: i32 = 0x0100_0015;
pub const QT_KEY_PAGE_UP: i32 = 0x0100_0016;
pub const QT_KEY_PAGE_DOWN: i32 = 0x0100_0017;

pub const QT_KEY_SHIFT: i32 = 0x0100_0020;
pub const QT_KEY_CONTROL: i32 = 0x0100_0021;
pub const QT_KEY_META: i32 = 0x0100_0022;
pub const QT_KEY_ALT: i32 = 0x0100_0023;

pub const QT_KEY_F1: i32 = 0x0100_0030;
pub const QT_KEY_F30: i32 = 0x0100_004d;

pub const QT_KEY_0: i32 = 0x30;
pub const QT_KEY_9: i32 = 0x39;

// --- Modifier Formatting ---

/// Formats Qt keyboard modifiers into mpv modifier string in exact order: Shift+, Ctrl+, Alt+, Meta+.
pub fn format_modifiers(modifiers: i32) -> String {
    let mut s = String::new();
    if (modifiers & QT_SHIFT_MODIFIER) != 0 {
        s.push_str("Shift+");
    }
    if (modifiers & QT_CONTROL_MODIFIER) != 0 {
        s.push_str("Ctrl+");
    }
    if (modifiers & QT_ALT_MODIFIER) != 0 {
        s.push_str("Alt+");
    }
    if (modifiers & QT_META_MODIFIER) != 0 {
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
    pub fn from_qt(button: u32) -> Option<Self> {
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
        if is_macos || (modifiers & QT_KEYPAD_MODIFIER) == 0 {
            return Self::None;
        }
        match key {
            QT_KEY_0..=QT_KEY_9 => Self::Number,
            QT_KEY_DELETE
            | QT_KEY_INSERT
            | QT_KEY_HOME
            | QT_KEY_END
            | QT_KEY_CLEAR
            | QT_KEY_PAGE_UP
            | QT_KEY_PAGE_DOWN
            | QT_KEY_RIGHT
            | QT_KEY_LEFT
            | QT_KEY_DOWN
            | QT_KEY_UP
            | QT_KEY_ENTER => Self::Navigation,
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
        if (QT_KEY_F1..=QT_KEY_F30).contains(&key) {
            return Self::FunctionKey((key - QT_KEY_F1 + 1) as u32);
        }
        match key {
            QT_KEY_SHIFT | QT_KEY_CONTROL | QT_KEY_ALT | QT_KEY_META => Self::BareModifier,
            QT_KEY_LEFT => Self::Named("LEFT"),
            QT_KEY_RIGHT => Self::Named("RIGHT"),
            QT_KEY_UP => Self::Named("UP"),
            QT_KEY_DOWN => Self::Named("DOWN"),
            QT_KEY_ENTER | QT_KEY_RETURN => Self::Named("ENTER"),
            QT_KEY_ESCAPE => Self::Named("ESC"),
            QT_KEY_BACKSPACE => Self::Named("BS"),
            QT_KEY_PAUSE => Self::Named("PAUSE"),
            QT_KEY_DELETE => Self::Named("DEL"),
            QT_KEY_INSERT => Self::Named("INS"),
            QT_KEY_HOME => Self::Named("HOME"),
            QT_KEY_END => Self::Named("END"),
            QT_KEY_CLEAR => Self::Named("DEC"),
            QT_KEY_PAGE_UP => Self::Named("PGUP"),
            QT_KEY_PAGE_DOWN => Self::Named("PGDWN"),
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
            let _ = write!(result, "F{}", num);
        }
        KeyMapping::Fallback => {
            if fallback.trim().is_empty() {
                return None;
            }
            if (modifiers & QT_SHIFT_MODIFIER) != 0 {
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

pub fn parse_cli_options_u16<'a, I>(args: I) -> Vec<CliOption>
where
    I: IntoIterator<Item = &'a [u16]>,
{
    const PREFIX: &[u16] = &[b'-' as u16, b'-' as u16];
    const OPEN_GROUP: &[u16] = &[b'-' as u16, b'-' as u16, b'{' as u16];
    const CLOSE_GROUP: &[u16] = &[b'-' as u16, b'-' as u16, b'}' as u16];
    const NO_PREFIX: &[u16] = &[b'n' as u16, b'o' as u16, b'-' as u16];
    const EQUALS_NO: &[u16] = &[b'=' as u16, b'n' as u16, b'o' as u16];

    let args: Vec<&'a [u16]> = args.into_iter().collect();
    let mut options = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let arg = args[i];
        if !arg.starts_with(PREFIX) {
            i += 1;
            continue;
        }

        if arg == OPEN_GROUP {
            let mut depth = 1usize;
            while depth > 0 {
                i += 1;
                if i >= args.len() {
                    break;
                }
                if args[i] == OPEN_GROUP {
                    depth += 1;
                } else if args[i] == CLOSE_GROUP {
                    depth -= 1;
                }
            }
            i += 1;
            continue;
        }

        let mut current: &[u16] = &arg[PREFIX.len()..];
        let mut with_no: Vec<u16>;
        if current.starts_with(NO_PREFIX) {
            with_no = Vec::with_capacity(current.len() - NO_PREFIX.len() + EQUALS_NO.len());
            with_no.extend_from_slice(&current[NO_PREFIX.len()..]);
            with_no.extend_from_slice(EQUALS_NO);
            current = &with_no;
        }

        if let Some(sep_idx) = current.iter().position(|&u| u == b'=' as u16) {
            let name = String::from_utf16_lossy(&current[..sep_idx]);
            let value = String::from_utf16_lossy(&current[sep_idx + 1..]);
            options.push(CliOption { name, value });
        }

        i += 1;
    }
    options
}

pub fn parse_cli_options_utf8<I, S>(args: I) -> Vec<CliOption>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let args: Vec<S> = args.into_iter().collect();
    let mut options = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_ref();
        if !arg.starts_with("--") {
            i += 1;
            continue;
        }

        if arg == "--{" {
            let mut depth = 1usize;
            while depth > 0 {
                i += 1;
                if i >= args.len() {
                    break;
                }
                let inner = args[i].as_ref();
                if inner == "--{" {
                    depth += 1;
                } else if inner == "--}" {
                    depth -= 1;
                }
            }
            i += 1;
            continue;
        }

        let mut current: &str = &arg[2..];
        let with_no: String;
        if current.starts_with("no-") {
            with_no = format!("{}=no", &current[3..]);
            current = &with_no;
        }

        if let Some(sep_idx) = current.find('=') {
            let name = current[..sep_idx].to_string();
            let value = current[sep_idx + 1..].to_string();
            options.push(CliOption { name, value });
        }

        i += 1;
    }
    options
}

// --- C ABI Wiring ---

#[repr(C)]
pub struct MementoPlayerUtf16Arg {
    pub data: *const u16,
    pub len: usize,
}

pub type MementoPlayerCliOptionCb = unsafe extern "C" fn(
    userdata: *mut c_void,
    name: *const c_char,
    value: *const c_char,
);

#[no_mangle]
pub unsafe extern "C" fn memento_player_is_subtitle_file(path: *const u16, len: usize) -> bool {
    if path.is_null() || len == 0 {
        return false;
    }
    let slice = std::slice::from_raw_parts(path, len);
    is_subtitle_file(slice)
}

#[no_mangle]
pub unsafe extern "C" fn memento_player_format_modifiers(
    modifiers: i32,
    out_buf: *mut c_char,
    cap: usize,
) -> usize {
    let formatted = format_modifiers(modifiers);
    let bytes = formatted.as_bytes();
    if !out_buf.is_null() && cap > 0 {
        let to_copy = bytes.len().min(cap - 1);
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), out_buf as *mut u8, to_copy);
        *out_buf.add(to_copy) = 0;
    }
    bytes.len()
}

#[no_mangle]
pub extern "C" fn memento_player_mouse_button_name(button: u32) -> *const c_char {
    match MouseButton::from_qt(button) {
        Some(MouseButton::Left) => c"MOUSE_BTN0".as_ptr(),
        Some(MouseButton::Middle) => c"MOUSE_BTN1".as_ptr(),
        Some(MouseButton::Right) => c"MOUSE_BTN2".as_ptr(),
        Some(MouseButton::Back) => c"MOUSE_BTN7".as_ptr(),
        Some(MouseButton::Forward) => c"MOUSE_BTN8".as_ptr(),
        None => std::ptr::null(),
    }
}

#[no_mangle]
pub extern "C" fn memento_player_wheel_button(delta_x: i32, delta_y: i32) -> *const c_char {
    match WheelButton::from_deltas(delta_x, delta_y) {
        Some(WheelButton::Up) => c"3".as_ptr(),
        Some(WheelButton::Down) => c"4".as_ptr(),
        Some(WheelButton::Left) => c"5".as_ptr(),
        Some(WheelButton::Right) => c"6".as_ptr(),
        None => std::ptr::null(),
    }
}

#[no_mangle]
pub unsafe extern "C" fn memento_player_format_keypress(
    key: i32,
    modifiers: i32,
    is_macos: bool,
    fallback_key_seq_utf8: *const c_char,
    fallback_len: usize,
    out_buf: *mut c_char,
    cap: usize,
) -> usize {
    let fallback = if !fallback_key_seq_utf8.is_null() && fallback_len > 0 {
        let bytes = std::slice::from_raw_parts(fallback_key_seq_utf8 as *const u8, fallback_len);
        std::str::from_utf8(bytes).unwrap_or("")
    } else {
        ""
    };

    let Some(formatted) = format_keypress(key, modifiers, is_macos, fallback) else {
        return 0;
    };

    let bytes = formatted.as_bytes();
    if !out_buf.is_null() && cap > 0 {
        let to_copy = bytes.len().min(cap - 1);
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), out_buf as *mut u8, to_copy);
        *out_buf.add(to_copy) = 0;
    }
    bytes.len()
}

#[no_mangle]
pub unsafe extern "C" fn memento_player_validate_raw_screenshot(
    has_w: bool,
    w: i64,
    has_h: bool,
    h: i64,
    has_stride: bool,
    stride: i64,
    format_utf8: *const c_char,
    has_data: bool,
    out_min_line_size: *mut i64,
) -> i32 {
    let format = if !format_utf8.is_null() {
        CStr::from_ptr(format_utf8).to_str().ok()
    } else {
        None
    };

    match ScreenshotSpec::validate(has_w, w, has_h, h, has_stride, stride, format, has_data) {
        Ok(min_line_size) => {
            if !out_min_line_size.is_null() {
                *out_min_line_size = min_line_size;
            }
            0
        }
        Err(e) => e as i32,
    }
}

#[no_mangle]
pub unsafe extern "C" fn memento_player_parse_cli_options(
    args: *const MementoPlayerUtf16Arg,
    count: usize,
    userdata: *mut c_void,
    cb: Option<MementoPlayerCliOptionCb>,
) {
    let Some(cb) = cb else {
        return;
    };
    if args.is_null() || count == 0 {
        return;
    }
    let raw_slices = std::slice::from_raw_parts(args, count);
    let mut u16_slices = Vec::with_capacity(count);
    for item in raw_slices {
        if item.data.is_null() || item.len == 0 {
            u16_slices.push(&[][..]);
        } else {
            u16_slices.push(std::slice::from_raw_parts(item.data, item.len));
        }
    }

    let parsed = parse_cli_options_u16(u16_slices);
    for opt in parsed {
        let clean_name = opt.name.split('\0').next().unwrap_or("");
        let clean_val = opt.value.split('\0').next().unwrap_or("");
        if let (Ok(c_name), Ok(c_val)) = (CString::new(clean_name), CString::new(clean_val)) {
            cb(userdata, c_name.as_ptr(), c_val.as_ptr());
        }
    }
}

// --- Unit Tests ---

#[cfg(test)]
mod tests {
    use super::*;

    fn u16_vec(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    #[test]
    fn subtitle_file_extension_checks() {
        assert!(is_subtitle_file(&u16_vec("video.srt")));
        assert!(is_subtitle_file(&u16_vec("video.SRT")));
        assert!(is_subtitle_file(&u16_vec("video.Srt")));
        assert!(is_subtitle_file(&u16_vec("video.ass")));
        assert!(is_subtitle_file(&u16_vec("video.vtt")));
        assert!(is_subtitle_file(&u16_vec("video.utf-8")));
        assert!(is_subtitle_file(&u16_vec("video.utf8")));
        assert!(is_subtitle_file(&u16_vec("video.utf")));
        assert!(is_subtitle_file(&u16_vec("video.rt")));
        assert!(is_subtitle_file(&u16_vec("video.idx")));
        assert!(is_subtitle_file(&u16_vec(".vtt")));

        // Bare extension without dot: in C++, lastIndexOf('.') is -1,
        // so file.right(size - (-1) - 1) == file.right(size) -> whole string checked!
        assert!(is_subtitle_file(&u16_vec("srt")));
        assert!(is_subtitle_file(&u16_vec("ASS")));
        assert!(is_subtitle_file(&u16_vec("utf-8")));

        // Non-subtitles
        assert!(!is_subtitle_file(&u16_vec("video.mp4")));
        assert!(!is_subtitle_file(&u16_vec("video.mkv")));
        assert!(!is_subtitle_file(&u16_vec("video.txt")));
        assert!(!is_subtitle_file(&u16_vec("video.")));
        assert!(!is_subtitle_file(&u16_vec("")));
        assert!(!is_subtitle_file(&u16_vec("mp4")));
        assert!(!is_subtitle_file(&u16_vec("video.ass.mp4")));
    }

    #[test]
    fn modifier_formatting() {
        assert_eq!(format_modifiers(0), "");
        assert_eq!(format_modifiers(QT_SHIFT_MODIFIER), "Shift+");
        assert_eq!(format_modifiers(QT_CONTROL_MODIFIER), "Ctrl+");
        assert_eq!(format_modifiers(QT_ALT_MODIFIER), "Alt+");
        assert_eq!(format_modifiers(QT_META_MODIFIER), "Meta+");
        assert_eq!(
            format_modifiers(QT_SHIFT_MODIFIER | QT_CONTROL_MODIFIER),
            "Shift+Ctrl+"
        );
        assert_eq!(
            format_modifiers(
                QT_SHIFT_MODIFIER | QT_CONTROL_MODIFIER | QT_ALT_MODIFIER | QT_META_MODIFIER
            ),
            "Shift+Ctrl+Alt+Meta+"
        );
        // KeypadModifier is ignored by format_modifiers
        assert_eq!(
            format_modifiers(QT_KEYPAD_MODIFIER | QT_SHIFT_MODIFIER),
            "Shift+"
        );
    }

    #[test]
    fn mouse_button_mappings() {
        assert_eq!(MouseButton::from_qt(1), Some(MouseButton::Left));
        assert_eq!(MouseButton::from_qt(2), Some(MouseButton::Right));
        assert_eq!(MouseButton::from_qt(4), Some(MouseButton::Middle));
        assert_eq!(MouseButton::from_qt(8), Some(MouseButton::Back));
        assert_eq!(MouseButton::from_qt(16), Some(MouseButton::Forward));
        assert_eq!(MouseButton::from_qt(0), None);
        assert_eq!(MouseButton::from_qt(32), None);

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

        // Y takes precedence over X
        assert_eq!(WheelButton::from_deltas(10, 15), Some(WheelButton::Up));
        assert_eq!(WheelButton::from_deltas(-10, -15), Some(WheelButton::Down));

        assert_eq!(WheelButton::Up.as_str(), "3");
        assert_eq!(WheelButton::Down.as_str(), "4");
        assert_eq!(WheelButton::Left.as_str(), "5");
        assert_eq!(WheelButton::Right.as_str(), "6");
    }

    #[test]
    fn keypress_formatting() {
        // Bare modifiers return None
        assert_eq!(format_keypress(QT_KEY_SHIFT, QT_SHIFT_MODIFIER, false, ""), None);
        assert_eq!(format_keypress(QT_KEY_CONTROL, 0, false, ""), None);
        assert_eq!(format_keypress(QT_KEY_ALT, 0, false, ""), None);
        assert_eq!(format_keypress(QT_KEY_META, 0, false, ""), None);

        // Named keys
        assert_eq!(
            format_keypress(QT_KEY_LEFT, 0, false, "").as_deref(),
            Some("LEFT")
        );
        assert_eq!(
            format_keypress(QT_KEY_RIGHT, QT_CONTROL_MODIFIER, false, "").as_deref(),
            Some("Ctrl+RIGHT")
        );
        assert_eq!(
            format_keypress(QT_KEY_ENTER, 0, false, "").as_deref(),
            Some("ENTER")
        );
        assert_eq!(
            format_keypress(QT_KEY_RETURN, 0, false, "").as_deref(),
            Some("ENTER")
        );
        assert_eq!(
            format_keypress(QT_KEY_ESCAPE, 0, false, "").as_deref(),
            Some("ESC")
        );
        assert_eq!(
            format_keypress(QT_KEY_BACKSPACE, 0, false, "").as_deref(),
            Some("BS")
        );
        assert_eq!(
            format_keypress(QT_KEY_DELETE, 0, false, "").as_deref(),
            Some("DEL")
        );
        assert_eq!(
            format_keypress(QT_KEY_CLEAR, 0, false, "").as_deref(),
            Some("DEC")
        );

        // Function keys F1..F30
        assert_eq!(
            format_keypress(QT_KEY_F1, 0, false, "").as_deref(),
            Some("F1")
        );
        assert_eq!(
            format_keypress(QT_KEY_F30, QT_ALT_MODIFIER, false, "").as_deref(),
            Some("Alt+F30")
        );

        // Keypad prefix on non-macOS
        assert_eq!(
            format_keypress(QT_KEY_0, QT_KEYPAD_MODIFIER, false, "0").as_deref(),
            Some("KP0")
        );
        assert_eq!(
            format_keypress(QT_KEY_DELETE, QT_KEYPAD_MODIFIER, false, "Del").as_deref(),
            Some("KP_DEL")
        );
        assert_eq!(
            format_keypress(QT_KEY_ENTER, QT_KEYPAD_MODIFIER, false, "").as_deref(),
            Some("KP_ENTER")
        );

        // Keypad modifier ignored on macOS
        assert_eq!(
            format_keypress(QT_KEY_0, QT_KEYPAD_MODIFIER, true, "0").as_deref(),
            Some("0")
        );
        assert_eq!(
            format_keypress(QT_KEY_DELETE, QT_KEYPAD_MODIFIER, true, "Del").as_deref(),
            Some("DEL")
        );

        // Fallback casing
        assert_eq!(
            format_keypress(0x41, 0, false, "A").as_deref(),
            Some("a")
        );
        assert_eq!(
            format_keypress(0x41, QT_SHIFT_MODIFIER, false, "A").as_deref(),
            Some("Shift+A")
        );

        // Empty fallback for unmapped key returns None
        assert_eq!(format_keypress(0x1234, QT_CONTROL_MODIFIER, false, ""), None);
        assert_eq!(format_keypress(0x1234, 0, false, "   "), None);

        // Safe null-termination on FFI calls even when truncated
        let mut small_buf = [0o77 as c_char; 4];
        let len = unsafe {
            let fallback_seq = c"";
            memento_player_format_keypress(
                QT_KEY_LEFT,
                QT_CONTROL_MODIFIER,
                false,
                fallback_seq.as_ptr(),
                0,
                small_buf.as_mut_ptr(),
                small_buf.len(),
            )
        };
        assert_eq!(len, "Ctrl+LEFT".len());
        assert_eq!(small_buf[3], 0);
        let s = unsafe { CStr::from_ptr(small_buf.as_ptr()) }.to_str().unwrap();
        assert_eq!(s, "Ctr");

        let mut mod_buf = [0o77 as c_char; 5];
        let mod_len = unsafe {
            memento_player_format_modifiers(
                QT_SHIFT_MODIFIER | QT_CONTROL_MODIFIER,
                mod_buf.as_mut_ptr(),
                mod_buf.len(),
            )
        };
        assert_eq!(mod_len, "Shift+Ctrl+".len());
        assert_eq!(mod_buf[4], 0);
        let s_mod = unsafe { CStr::from_ptr(mod_buf.as_ptr()) }.to_str().unwrap();
        assert_eq!(s_mod, "Shif");
    }

    #[test]
    fn screenshot_validation() {
        let mut min_line = 0i64;
        // Valid RGBA
        assert_eq!(
            ScreenshotSpec::validate(true, 1920, true, 1080, true, 1920 * 4, Some("rgba"), true),
            Ok(1920 * 4)
        );
        // Valid negative stride (bottom-up image)
        assert_eq!(
            ScreenshotSpec::validate(true, 1920, true, 1080, true, -(1920 * 4), Some("rgba"), true),
            Ok(1920 * 4)
        );

        // Missing fields or bad format -> UnexpectedFormat (1)
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

        // Invalid dimensions -> InvalidDimensions (2)
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

        // Invalid stride -> InvalidStride (3)
        assert_eq!(
            ScreenshotSpec::validate(true, 1920, true, 1080, true, 1920 * 4 - 1, Some("rgba"), true),
            Err(ScreenshotValidationError::InvalidStride)
        );
        assert_eq!(
            ScreenshotSpec::validate(true, 1920, true, 1080, true, 0, Some("rgba"), true),
            Err(ScreenshotValidationError::InvalidStride)
        );

        // Via FFI
        unsafe {
            assert_eq!(
                memento_player_validate_raw_screenshot(
                    true,
                    100,
                    true,
                    50,
                    true,
                    400,
                    c"rgba".as_ptr(),
                    true,
                    &mut min_line
                ),
                0
            );
            assert_eq!(min_line, 400);
        }
    }

    #[test]
    fn parse_cli_options() {
        let args = vec![
            "player",
            "--vo=null",
            "--no-config",
            "--no-terminal=yes",
            "--{",
            "--inside=skipped",
            "--no-inside",
            "--}",
            "--ao=alsa",
            "--bare-option-no-equals",
            "media.mkv",
            "--pause=yes",
            "--no-",
        ];

        let expected = vec![
            CliOption {
                name: "vo".to_string(),
                value: "null".to_string(),
            },
            CliOption {
                name: "config".to_string(),
                value: "no".to_string(),
            },
            CliOption {
                name: "terminal".to_string(),
                value: "yes=no".to_string(),
            },
            CliOption {
                name: "ao".to_string(),
                value: "alsa".to_string(),
            },
            CliOption {
                name: "pause".to_string(),
                value: "yes".to_string(),
            },
            CliOption {
                name: "".to_string(),
                value: "no".to_string(),
            },
        ];

        let parsed_utf8 = parse_cli_options_utf8(&args);
        assert_eq!(parsed_utf8, expected);

        let u16_args: Vec<Vec<u16>> = args.iter().map(|&s| u16_vec(s)).collect();
        let u16_slices: Vec<&[u16]> = u16_args.iter().map(Vec::as_slice).collect();
        let parsed_u16 = parse_cli_options_u16(u16_slices);
        assert_eq!(parsed_u16, expected);
    }
}

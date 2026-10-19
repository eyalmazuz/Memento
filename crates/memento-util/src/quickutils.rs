////////////////////////////////////////////////////////////////////////////////
//
// Copyright (c) 2026 Ripose
//
// This file is part of Memento.
//
// Memento is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, version 2 of the License.
//
// Memento is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with Memento.  If not, see <https://www.gnu.org/licenses/>.
//
////////////////////////////////////////////////////////////////////////////////

//! Pure logic ports of `src/quick/coloredimageprovider.cpp` and
//! `src/quick/keytracker.cpp`.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum ImageSourceKind {
    Resource = 1,
    File = 2,
}

impl ImageSourceKind {
    pub fn from_u32(val: u32) -> Option<Self> {
        match val {
            1 => Some(Self::Resource),
            2 => Some(Self::File),
            _ => None,
        }
    }
}

const RESOURCE_PREFIX: [u16; 8] = [
    b'r' as u16, b'e' as u16, b's' as u16, b'o' as u16,
    b'u' as u16, b'r' as u16, b'c' as u16, b'e' as u16,
];
const FILE_PREFIX: [u16; 4] = [
    b'f' as u16, b'i' as u16, b'l' as u16, b'e' as u16,
];
const SLASH: u16 = b'/' as u16;
const COLON_SLASH: [u16; 2] = [b':' as u16, b'/' as u16];
const HASH: u16 = b'#' as u16;

/// Parses an image request ID into source kind, encoded path, and color spec.
///
/// The ID format is `source-type/percent-encoded-path/argb-color`.
/// Returns `None` if `firstSeparator <= 0 || lastSeparator <= firstSeparator`,
/// or if `sourceType` is not `"resource"` or `"file"`.
pub fn parse_image_request_id(id: &[u16]) -> Option<(ImageSourceKind, Vec<u16>, Vec<u16>)> {
    let first_sep = id.iter().position(|&c| c == SLASH)?;
    let last_sep = id.iter().rposition(|&c| c == SLASH)?;

    if first_sep == 0 || last_sep <= first_sep {
        return None;
    }

    let source_type = &id[..first_sep];
    let kind = if source_type == RESOURCE_PREFIX {
        ImageSourceKind::Resource
    } else if source_type == FILE_PREFIX {
        ImageSourceKind::File
    } else {
        return None;
    };

    let encoded_path = id[first_sep + 1..last_sep].to_vec();

    let mut color_spec = Vec::with_capacity(id.len() - last_sep);
    color_spec.push(HASH);
    color_spec.extend_from_slice(&id[last_sep + 1..]);

    Some((kind, encoded_path, color_spec))
}

/// Checks whether a UTF-16 path ends with `.svg` (case-insensitive ASCII).
pub fn is_svg_path(path: &[u16]) -> bool {
    if path.len() < 4 {
        return false;
    }
    let suffix = &path[path.len() - 4..];
    suffix[0] == b'.' as u16
        && (suffix[1] < 128 && (suffix[1] as u8).to_ascii_lowercase() == b's')
        && (suffix[2] < 128 && (suffix[2] as u8).to_ascii_lowercase() == b'v')
        && (suffix[3] < 128 && (suffix[3] as u8).to_ascii_lowercase() == b'g')
}

/// Validates an image source path against its declared source kind.
///
/// Returns `(is_valid, is_svg)`.
pub fn validate_image_source(
    source_kind: u32,
    source_path: &[u16],
    is_file_absolute: bool,
) -> (bool, bool) {
    let is_svg = is_svg_path(source_path);
    let starts_with_colon_slash = source_path.starts_with(&COLON_SLASH);

    let is_valid = match ImageSourceKind::from_u32(source_kind) {
        Some(ImageSourceKind::Resource) => starts_with_colon_slash,
        Some(ImageSourceKind::File) => is_file_absolute && !starts_with_colon_slash,
        None => false,
    };

    (is_valid, is_svg)
}

pub const QT_ALT_MODIFIER: u32 = 0x08000000;
pub const QT_CONTROL_MODIFIER: u32 = 0x04000000;
pub const QT_SHIFT_MODIFIER: u32 = 0x02000000;
pub const QT_META_MODIFIER: u32 = 0x10000000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(i32)]
pub enum SettingModifier {
    Alt = 0,
    Control = 1,
    Shift = 2,
    Super = 3,
}

impl SettingModifier {
    pub fn from_i32(val: i32) -> Option<Self> {
        match val {
            0 => Some(Self::Alt),
            1 => Some(Self::Control),
            2 => Some(Self::Shift),
            3 => Some(Self::Super),
            _ => None,
        }
    }
}

/// Matches `KeyTracker::modifierHeld`.
pub fn modifier_held(setting_modifier: i32, qt_modifiers: u32) -> bool {
    match SettingModifier::from_i32(setting_modifier) {
        Some(SettingModifier::Alt) => (qt_modifiers & QT_ALT_MODIFIER) != 0,
        Some(SettingModifier::Control) => (qt_modifiers & QT_CONTROL_MODIFIER) != 0,
        Some(SettingModifier::Shift) => (qt_modifiers & QT_SHIFT_MODIFIER) != 0,
        Some(SettingModifier::Super) => (qt_modifiers & QT_META_MODIFIER) != 0,
        None => false,
    }
}

/// Format the MeCab option bytes without interpreting Windows' 8.3 paths as
/// UTF-8. `GetShortPathNameA` returns QByteArray bytes, and the C++ code
/// appends those bytes directly even when the local ANSI code page is not UTF-8.
pub fn format_mecab_tagger_arg(
    is_windows: bool,
    is_bundle: bool,
    path_a: &[u8],
    path_b: &[u8],
) -> Vec<u8> {
    if is_windows {
        let mut arg = Vec::with_capacity(8 + path_a.len() + path_b.len());
        arg.extend_from_slice(b" -d ");
        arg.extend_from_slice(path_a);
        arg.extend_from_slice(b" -r ");
        arg.extend_from_slice(path_b);
        arg
    } else if is_bundle {
        let mut arg = Vec::with_capacity(27 + path_a.len() * 2);
        arg.extend_from_slice(b"-r ");
        arg.extend_from_slice(path_a);
        arg.extend_from_slice(b"ipadic/dicrc -d ");
        arg.extend_from_slice(path_a);
        arg.extend_from_slice(b"ipadic");
        arg
    } else {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn to_u16(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    fn from_u16(units: &[u16]) -> String {
        String::from_utf16_lossy(units)
    }

    #[test]
    fn parse_image_request_id_valid_resource() {
        let id = to_u16("resource/%3A%2Ficons%2Ftest.svg/ff00ff00");
        let (kind, encoded_path, color_spec) = parse_image_request_id(&id).unwrap();
        assert_eq!(kind, ImageSourceKind::Resource);
        assert_eq!(from_u16(&encoded_path), "%3A%2Ficons%2Ftest.svg");
        assert_eq!(from_u16(&color_spec), "#ff00ff00");
    }

    #[test]
    fn parse_image_request_id_valid_file() {
        let id = to_u16("file/%2Fhome%2Fuser%2Fpic.png/11223344");
        let (kind, encoded_path, color_spec) = parse_image_request_id(&id).unwrap();
        assert_eq!(kind, ImageSourceKind::File);
        assert_eq!(from_u16(&encoded_path), "%2Fhome%2Fuser%2Fpic.png");
        assert_eq!(from_u16(&color_spec), "#11223344");
    }

    #[test]
    fn parse_image_request_id_invalid_formats() {
        // No slashes
        assert!(parse_image_request_id(&to_u16("noslashes")).is_none());
        // Only one slash (lastSeparator == firstSeparator)
        assert!(parse_image_request_id(&to_u16("file/path")).is_none());
        // Leading slash (firstSeparator == 0)
        assert!(parse_image_request_id(&to_u16("/file/path/color")).is_none());
        // Unknown source type
        assert!(parse_image_request_id(&to_u16("http/path/color")).is_none());
        // Empty
        assert!(parse_image_request_id(&[]).is_none());
    }

    #[test]
    fn is_svg_path_detection() {
        assert!(is_svg_path(&to_u16("image.svg")));
        assert!(is_svg_path(&to_u16("IMAGE.SVG")));
        assert!(is_svg_path(&to_u16("image.SvG")));
        assert!(is_svg_path(&to_u16(".svg")));
        assert!(!is_svg_path(&to_u16("image.png")));
        assert!(!is_svg_path(&to_u16("svg")));
        assert!(!is_svg_path(&to_u16(".sv")));
        assert!(!is_svg_path(&[]));
    }

    #[test]
    fn validate_image_source_resource() {
        let path = to_u16(":/icons/app.svg");
        let (valid, is_svg) = validate_image_source(1, &path, false);
        assert!(valid);
        assert!(is_svg);

        let non_res = to_u16("/icons/app.png");
        let (valid, is_svg) = validate_image_source(1, &non_res, true);
        assert!(!valid);
        assert!(!is_svg);
    }

    #[test]
    fn validate_image_source_file() {
        let abs_path = to_u16("/home/user/image.svg");
        let (valid, is_svg) = validate_image_source(2, &abs_path, true);
        assert!(valid);
        assert!(is_svg);

        // Relative path -> invalid
        let (valid, is_svg) = validate_image_source(2, &abs_path, false);
        assert!(!valid);
        assert!(is_svg);

        // Starts with ":/" -> invalid for file
        let colon_slash_path = to_u16(":/etc/shadow.svg");
        let (valid, is_svg) = validate_image_source(2, &colon_slash_path, true);
        assert!(!valid);
        assert!(is_svg);
    }

    #[test]
    fn modifier_held_cases() {
        assert!(modifier_held(0, QT_ALT_MODIFIER));
        assert!(!modifier_held(0, QT_CONTROL_MODIFIER));
        assert!(modifier_held(0, QT_ALT_MODIFIER | QT_SHIFT_MODIFIER));

        assert!(modifier_held(1, QT_CONTROL_MODIFIER));
        assert!(!modifier_held(1, QT_ALT_MODIFIER));

        assert!(modifier_held(2, QT_SHIFT_MODIFIER));
        assert!(!modifier_held(2, QT_META_MODIFIER));

        assert!(modifier_held(3, QT_META_MODIFIER));
        assert!(!modifier_held(3, QT_SHIFT_MODIFIER));

        // Out of range modifiers
        assert!(!modifier_held(-1, 0xFFFFFFFF));
        assert!(!modifier_held(4, 0xFFFFFFFF));
        assert!(!modifier_held(100, 0xFFFFFFFF));
    }

    #[test]
    fn format_mecab_tagger_arg_cases() {
        // Windows
        assert_eq!(
            format_mecab_tagger_arg(true, false, b"C:/PROGRA~1/ipadic", b"C:/PROGRA~1/ipadic/dicrc"),
            b" -d C:/PROGRA~1/ipadic -r C:/PROGRA~1/ipadic/dicrc"
        );
        // Windows takes precedence if both flags are true
        assert_eq!(
            format_mecab_tagger_arg(true, true, b"C:/ipadic", b"C:/ipadic/dicrc"),
            b" -d C:/ipadic -r C:/ipadic/dicrc"
        );
        // Bundle
        assert_eq!(
            format_mecab_tagger_arg(false, true, b"/app/share/mecab/", b""),
            b"-r /app/share/mecab/ipadic/dicrc -d /app/share/mecab/ipadic"
        );
        assert_eq!(
            format_mecab_tagger_arg(false, true, b"", b""),
            b"-r ipadic/dicrc -d ipadic"
        );
        // Standard Linux
        assert_eq!(
            format_mecab_tagger_arg(false, false, b"/usr/share/mecab", b"/etc/dicrc"),
            b""
        );
        assert_eq!(
            format_mecab_tagger_arg(false, false, b"", b""),
            b""
        );

        // Windows 8.3 paths from GetShortPathNameA may be in an ANSI code
        // page. The bytes must reach MeCab unchanged.
        assert_eq!(
            format_mecab_tagger_arg(true, false, b"C:/\xff/dic", b"C:/\xfe/dicrc"),
            b" -d C:/\xff/dic -r C:/\xfe/dicrc"
        );
    }
}

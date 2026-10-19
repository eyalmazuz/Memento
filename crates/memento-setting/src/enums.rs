//! Rust port of the `Setting` namespace enums of `src/setting/keys.h`
//! (keys.h:26-81, `Q_NAMESPACE` with one `Q_ENUM_NS` per enum).
//!
//! Every C++ enum here declares its discriminants explicitly, so the Rust
//! variants carry those exact values via `#[repr(i32)]`. The C++ value names
//! are preserved verbatim as variant names (fully qualified style, e.g.
//! [`AudioSourceType::AudioSourceTypeFile`]) so the C++ <-> Rust mapping is a
//! mechanical, greppable 1:1. C++ declarations:
//!
//! | C++ enum          | keys.h lines | Rust type          |
//! |-------------------|--------------|--------------------|
//! | `AudioSourceType` | 30-35        | [`AudioSourceType`]|
//! | `Directory`       | 37-45        | [`Directory`]      |
//! | `GlossaryStyle`   | 47-53        | [`GlossaryStyle`]  |
//! | `Language`        | 55-63        | [`Language`]       |
//! | `Modifier`        | 65-72        | [`Modifier`]       |
//! | `SearchMethod`    | 74-79        | [`SearchMethod`]   |

#![allow(non_camel_case_types)]

/// `Setting::AudioSourceType` (keys.h:30-35).
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AudioSourceType {
    /// keys.h:32
    AudioSourceTypeFile = 0,
    /// keys.h:33
    AudioSourceTypeJson = 1,
}

/// `Setting::Directory` (keys.h:37-45).
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Directory {
    /// keys.h:39
    DirectoryCurrent = 0,
    /// keys.h:40
    DirectoryHome = 1,
    /// keys.h:41
    DirectoryMovies = 2,
    /// keys.h:42
    DirectoryDocuments = 3,
    /// keys.h:43
    DirectoryCustom = 4,
}

/// `Setting::GlossaryStyle` (keys.h:47-53).
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GlossaryStyle {
    /// keys.h:49
    GlossaryStyleBullet = 0,
    /// keys.h:50
    GlossaryStyleLineBreak = 1,
    /// keys.h:51
    GlossaryStylePipe = 2,
}

/// `Setting::Language` (keys.h:55-63).
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Language {
    /// keys.h:57
    LanguageSystem = 0,
    /// keys.h:58
    LanguageEnglish = 1,
    /// keys.h:59
    LanguageKorean = 2,
    /// keys.h:60
    LanguageChineseSimplified = 3,
    /// keys.h:61
    LanguageChineseTraditional = 4,
}

/// `Setting::Modifier` (keys.h:65-72).
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Modifier {
    /// keys.h:67
    ModifierAlt = 0,
    /// keys.h:68
    ModifierControl = 1,
    /// keys.h:69
    ModifierShift = 2,
    /// keys.h:70
    ModifierSuper = 3,
}

/// `Setting::SearchMethod` (keys.h:74-79).
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SearchMethod {
    /// keys.h:76
    SearchMethodHover = 0,
    /// keys.h:77
    SearchMethodModifier = 1,
}

/// `Q_ENUM_NS` numeric round trip: swallows the invalid values that a plain
/// `as` cast would happily preserve. `Q_ENUM_NS` itself only does the
/// name/registration side, which stays in C++.
impl TryFrom<i32> for AudioSourceType {
    type Error = i32;

    fn try_from(value: i32) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::AudioSourceTypeFile),
            1 => Ok(Self::AudioSourceTypeJson),
            other => Err(other),
        }
    }
}

impl TryFrom<i32> for Directory {
    type Error = i32;

    fn try_from(value: i32) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::DirectoryCurrent),
            1 => Ok(Self::DirectoryHome),
            2 => Ok(Self::DirectoryMovies),
            3 => Ok(Self::DirectoryDocuments),
            4 => Ok(Self::DirectoryCustom),
            other => Err(other),
        }
    }
}

impl TryFrom<i32> for GlossaryStyle {
    type Error = i32;

    fn try_from(value: i32) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::GlossaryStyleBullet),
            1 => Ok(Self::GlossaryStyleLineBreak),
            2 => Ok(Self::GlossaryStylePipe),
            other => Err(other),
        }
    }
}

impl TryFrom<i32> for Language {
    type Error = i32;

    fn try_from(value: i32) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::LanguageSystem),
            1 => Ok(Self::LanguageEnglish),
            2 => Ok(Self::LanguageKorean),
            3 => Ok(Self::LanguageChineseSimplified),
            4 => Ok(Self::LanguageChineseTraditional),
            other => Err(other),
        }
    }
}

impl TryFrom<i32> for Modifier {
    type Error = i32;

    fn try_from(value: i32) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::ModifierAlt),
            1 => Ok(Self::ModifierControl),
            2 => Ok(Self::ModifierShift),
            3 => Ok(Self::ModifierSuper),
            other => Err(other),
        }
    }
}

impl TryFrom<i32> for SearchMethod {
    type Error = i32;

    fn try_from(value: i32) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::SearchMethodHover),
            1 => Ok(Self::SearchMethodModifier),
            other => Err(other),
        }
    }
}

/// Deterministic numeric projection, mirroring how the C++ side stores these
/// enums into QSettings (`static_cast<int>`).
impl AudioSourceType {
    /// keys.h:30-35 — `static_cast<int>` equivalent.
    pub const fn as_i32(self) -> i32 {
        self as i32
    }
}

impl Directory {
    /// keys.h:37-45 — `static_cast<int>` equivalent.
    pub const fn as_i32(self) -> i32 {
        self as i32
    }
}

impl GlossaryStyle {
    /// keys.h:47-53 — `static_cast<int>` equivalent.
    pub const fn as_i32(self) -> i32 {
        self as i32
    }
}

impl Language {
    /// keys.h:55-63 — `static_cast<int>` equivalent.
    pub const fn as_i32(self) -> i32 {
        self as i32
    }
}

impl Modifier {
    /// keys.h:65-72 — `static_cast<int>` equivalent.
    pub const fn as_i32(self) -> i32 {
        self as i32
    }
}

impl SearchMethod {
    /// keys.h:74-79 — `static_cast<int>` equivalent.
    pub const fn as_i32(self) -> i32 {
        self as i32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// keys.h:30-35 — AudioSourceType declared values, in declaration order.
    #[test]
    fn keys_h_l0030_audio_source_type_values() {
        assert_eq!(AudioSourceType::AudioSourceTypeFile as i32, 0);
        assert_eq!(AudioSourceType::AudioSourceTypeJson as i32, 1);
    }

    /// keys.h:37-45 — Directory declared values, in declaration order.
    #[test]
    fn keys_h_l0037_directory_values() {
        assert_eq!(Directory::DirectoryCurrent as i32, 0);
        assert_eq!(Directory::DirectoryHome as i32, 1);
        assert_eq!(Directory::DirectoryMovies as i32, 2);
        assert_eq!(Directory::DirectoryDocuments as i32, 3);
        assert_eq!(Directory::DirectoryCustom as i32, 4);
    }

    /// keys.h:47-53 — GlossaryStyle declared values, in declaration order.
    #[test]
    fn keys_h_l0047_glossary_style_values() {
        assert_eq!(GlossaryStyle::GlossaryStyleBullet as i32, 0);
        assert_eq!(GlossaryStyle::GlossaryStyleLineBreak as i32, 1);
        assert_eq!(GlossaryStyle::GlossaryStylePipe as i32, 2);
    }

    /// keys.h:55-63 — Language declared values, in declaration order.
    #[test]
    fn keys_h_l0055_language_values() {
        assert_eq!(Language::LanguageSystem as i32, 0);
        assert_eq!(Language::LanguageEnglish as i32, 1);
        assert_eq!(Language::LanguageKorean as i32, 2);
        assert_eq!(Language::LanguageChineseSimplified as i32, 3);
        assert_eq!(Language::LanguageChineseTraditional as i32, 4);
    }

    /// keys.h:65-72 — Modifier declared values, in declaration order.
    #[test]
    fn keys_h_l0065_modifier_values() {
        assert_eq!(Modifier::ModifierAlt as i32, 0);
        assert_eq!(Modifier::ModifierControl as i32, 1);
        assert_eq!(Modifier::ModifierShift as i32, 2);
        assert_eq!(Modifier::ModifierSuper as i32, 3);
    }

    /// keys.h:74-79 — SearchMethod declared values, in declaration order.
    #[test]
    fn keys_h_l0074_search_method_values() {
        assert_eq!(SearchMethod::SearchMethodHover as i32, 0);
        assert_eq!(SearchMethod::SearchMethodModifier as i32, 1);
    }

    /// keys.h — numeric round trip is safe (no silent corrupted variants).
    #[test]
    fn keys_h_try_from_rejects_out_of_range_discriminants() {
        assert_eq!(AudioSourceType::try_from(1), Ok(AudioSourceType::AudioSourceTypeJson));
        assert_eq!(AudioSourceType::try_from(2), Err(2));
        assert_eq!(Directory::try_from(4), Ok(Directory::DirectoryCustom));
        assert_eq!(Directory::try_from(5), Err(5));
        assert_eq!(GlossaryStyle::try_from(2), Ok(GlossaryStyle::GlossaryStylePipe));
        assert_eq!(GlossaryStyle::try_from(3), Err(3));
        assert_eq!(Language::try_from(4), Ok(Language::LanguageChineseTraditional));
        assert_eq!(Language::try_from(-1), Err(-1));
        assert_eq!(Modifier::try_from(3), Ok(Modifier::ModifierSuper));
        assert_eq!(Modifier::try_from(4), Err(4));
        assert_eq!(SearchMethod::try_from(1), Ok(SearchMethod::SearchMethodModifier));
        assert_eq!(SearchMethod::try_from(2), Err(2));
    }
}

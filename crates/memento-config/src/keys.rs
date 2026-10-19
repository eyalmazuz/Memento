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

//! The idiomatic typed key model for the settings the pure-Rust Memento
//! actually uses, mirroring the `Keys` namespace hierarchy of
//! `src/setting/keys.h`:
//!
//! * groups are expressed as one enum per group ([`WindowField`],
//!   [`BehaviorField`], …) instead of stringly-typed `(group, key)` pairs;
//! * every group/key name and every declared default is *sourced from the
//!   certified constants* in `memento-setting` (the Rust port of `keys.h`),
//!   re-exported here as [`raw`]; nothing in this file invents a literal;
//! * the two keys whose C++ defaults depend on Qt (the runtime
//!   `file:// + QStandardPaths::HomeLocation` default of
//!   `Behavior::FILE_OPEN_CUSTOM` and the `QFont` typography defaults) have
//!   no constant default here — [`Key::declared_default`] reports `None`
//!   and the environment-dependent home dir comes in through
//!   [`DefaultContext`], mirroring the "defaults sourced from the same
//!   constants" rule while staying Qt-free;
//! * enums stored as ints in C++ (audio source type, language, glossary
//!   style, …) keep the C++ int encoding on disk and are presented as the
//!   Q enums by `memento_setting::enums` for shell-side use.
//!
//! [`Key::path`] maps a typed key to the store coordinate
//! ([`KeyPath`]): a group *path* (nested groups use `/`, indices join the
//! array group name) and the leaf key name. Root keys (only `version`)
//! live in the synthetic group `"general"`, matching the unnamed top-level
//! section of the legacy INI.

use std::fmt;

pub use memento_setting::enums;
/// The certified constants (Rust port of `src/setting/keys.h`).
pub use memento_setting::keys as raw;
/// The certified pure models (recent files, clamps, backends).
pub use memento_setting::models;

/// Names of the synthetic top-level section holding root keys (`version`,
/// and legacy leftovers such as the v2→v3 migration artefact).
pub const GENERAL_GROUP: &str = "general";

/// A fully resolved store coordinate: group path + leaf key.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct KeyPath {
    /// Group path; nested groups are joined with `/`.
    pub group: String,
    /// Leaf key name.
    pub key: String,
}

impl KeyPath {
    pub fn new(group: impl Into<String>, key: impl Into<String>) -> Self {
        KeyPath {
            group: group.into(),
            key: key.into(),
        }
    }
}

impl fmt::Display for KeyPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.group.is_empty() {
            write!(f, "{}", self.key)
        } else {
            write!(f, "{}/{}", self.group, self.key)
        }
    }
}

/// Runtime inputs for environment-dependent defaults.
///
/// Memento's C++ pulls these from `QStandardPaths`; the pure engine takes
/// them as parameters (no absolute paths are ever constructed here).
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct DefaultContext {
    /// Home directory of the user (key: `Keys::Behavior::FILE_OPEN_CUSTOM`).
    pub home_dir: String,
}

impl DefaultContext {
    pub fn new(home_dir: impl Into<String>) -> Self {
        DefaultContext {
            home_dir: home_dir.into(),
        }
    }
}

macro_rules! field_enum {
    (
        $name:ident,
        $( $variant:ident ( raw = $raw:expr, default = $default:expr ) ),* $(,)?
    ) => {
        impl $name {
            /// All fields of the group, in `keys.h` declaration order.
            pub const ALL: &'static [Self] = &[ $( Self::$variant, )* ];

            /// The exact leaf key name, from the certified constants.
            pub fn raw(self) -> &'static str {
                match self {
                    $( Self::$variant => $raw, )*
                }
            }

            /// The declared default value, from the certified constants;
            /// `None` marks keys whose C++ default is Qt-dependent or
            /// runtime-only.
            pub fn declared_default(
                self,
                ctx: &DefaultContext,
            ) -> Option<crate::value::Value> {
                match self {
                    $( Self::$variant => $default.as_value(ctx), )*
                }
            }

            /// The declared slot type of the field.
            pub fn value_type(
                self,
                ctx: &DefaultContext,
            ) -> crate::value::ValueType {
                match self {
                    $( Self::$variant => $default.value_type(ctx), )*
                }
            }
        }
    };
}

use crate::value::{Value, ValueType};

/// Field of the `[main-window]` group (keys.h `Keys::Window`).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum WindowField {
    SubtitleList,
    Search,
}

field_enum! {
    WindowField,
    SubtitleList(raw = raw::window::SUBTITLE_LIST, default = DefaultFallback::Bool(false)),
    Search(raw = raw::window::SEARCH, default = DefaultFallback::Bool(false)),
}

/// Field of the `[internal]` group (keys.h `Keys::Internal`).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum InternalField {
    AutoUpdateOptInShown,
    WhisperNoModelPromptDismissed,
}

field_enum! {
    InternalField,
    AutoUpdateOptInShown(raw = raw::internal::AUTO_UPDATE_OPT_IN_SHOWN, default = DefaultFallback::Bool(false)),
    WhisperNoModelPromptDismissed(raw = raw::internal::WHISPER_NO_MODEL_PROMPT_DISMISSED, default = DefaultFallback::Bool(false)),
}

/// Field of the `[recent]` group (keys.h `Keys::Recent`).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum RecentField {
    Files,
}

field_enum! {
    RecentField,
    Files(raw = raw::recent::FILES, default = DefaultFallback::Strings),
}

/// Field of the `[subtitle-list]` group (keys.h `Keys::SubtitleList`).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum SubtitleListField {
    IgnoreWhitespace,
    AutoSeek,
}

field_enum! {
    SubtitleListField,
    IgnoreWhitespace(raw = raw::subtitle_list::IGNORE_WHITESPACE, default = DefaultFallback::Bool(raw::subtitle_list::IGNORE_WHITESPACE_DEFAULT)),
    AutoSeek(raw = raw::subtitle_list::AUTO_SEEK, default = DefaultFallback::Bool(raw::subtitle_list::AUTO_SEEK_DEFAULT)),
}

/// Field of the `[audio-srcs/N]` group: one indexed entry of the QSettings
/// array written by `writeAudioSourceSettings` (keys.h `Keys::AudioSource`).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum AudioSourceField {
    Name,
    Url,
    Type,
    Md5,
}

field_enum! {
    AudioSourceField,
    Name(raw = raw::audio_source::NAME, default = DefaultFallback::Str(raw::audio_source::NAME_DEFAULT)),
    Url(raw = raw::audio_source::URL, default = DefaultFallback::Str(raw::audio_source::URL_DEFAULT)),
    Type(raw = raw::audio_source::TYPE, default = DefaultFallback::Int(raw::audio_source::TYPE_DEFAULT as i32)),
    Md5(raw = raw::audio_source::MD5, default = DefaultFallback::Str(raw::audio_source::MD5_DEFAULT)),
}

/// Field of the `[application]` group (keys.h `Keys::Application`).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ApplicationField {
    Language,
    AutoUpdateCheck,
}

field_enum! {
    ApplicationField,
    Language(raw = raw::application::LANGUAGE, default = DefaultFallback::Int(raw::application::LANGUAGE_DEFAULT as i32)),
    AutoUpdateCheck(raw = raw::application::AUTO_UPDATE_CHECK, default = DefaultFallback::Bool(raw::application::AUTO_UPDATE_CHECK_DEFAULT)),
}

/// Field of the `[behavior]` group (keys.h `Keys::Behavior`).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum BehaviorField {
    AutoFit,
    AutoFitPercent,
    OscMpvCursor,
    OscDuration,
    OscFade,
    OscMinMove,
    OscPreviewThumbnails,
    SubtitlePause,
    SubtitleCursorShow,
    SecondarySubtitleCursorShow,
    FileOpenDir,
    FileOpenCustom,
}

/// keys.h:192-194 builds the default at runtime from
/// `QStandardPaths::writableLocation(HomeLocation)`; the pure port takes the
/// home directory as a parameter.
pub fn file_open_custom_default(home_dir: &str) -> Value {
    Value::Text(format!("file://{home_dir}"))
}

field_enum! {
    BehaviorField,
    AutoFit(raw = raw::behavior::AUTO_FIT, default = DefaultFallback::Bool(raw::behavior::AUTO_FIT_DEFAULT)),
    AutoFitPercent(raw = raw::behavior::AUTO_FIT_PERCENT, default = DefaultFallback::Int(raw::behavior::AUTO_FIT_PERCENT_DEFAULT)),
    OscMpvCursor(raw = raw::behavior::OSC_MPV_CURSOR, default = DefaultFallback::Bool(raw::behavior::OSC_MPV_CURSOR_DEFAULT)),
    OscDuration(raw = raw::behavior::OSC_DURATION, default = DefaultFallback::Int(raw::behavior::OSC_DURATION_DEFAULT)),
    OscFade(raw = raw::behavior::OSC_FADE, default = DefaultFallback::Int(raw::behavior::OSC_FADE_DEFAULT)),
    OscMinMove(raw = raw::behavior::OSC_MIN_MOVE, default = DefaultFallback::Int(raw::behavior::OSC_MIN_MOVE_DEFAULT)),
    OscPreviewThumbnails(raw = raw::behavior::OSC_PREVIEW_THUMBNAILS, default = DefaultFallback::Bool(raw::behavior::OSC_PREVIEW_THUMBNAILS_DEFAULT)),
    SubtitlePause(raw = raw::behavior::SUBTITLE_PAUSE, default = DefaultFallback::Bool(raw::behavior::SUBTITLE_PAUSE_DEFAULT)),
    SubtitleCursorShow(raw = raw::behavior::SUBTITLE_CURSOR_SHOW, default = DefaultFallback::Bool(raw::behavior::SUBTITLE_CURSOR_SHOW_DEFAULT)),
    SecondarySubtitleCursorShow(raw = raw::behavior::SECONDARY_SUBTITLE_CURSOR_SHOW, default = DefaultFallback::Bool(raw::behavior::SECONDARY_SUBTITLE_CURSOR_SHOW_DEFAULT)),
    FileOpenDir(raw = raw::behavior::FILE_OPEN_DIR, default = DefaultFallback::Int(raw::behavior::FILE_OPEN_DIR_DEFAULT as i32)),
    FileOpenCustom(raw = raw::behavior::FILE_OPEN_CUSTOM, default = DefaultFallback::None),
}

/// Field of the `[dictionary]` group (keys.h `Keys::Dictionary`).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum DictionaryField {
    Order,
}

field_enum! {
    DictionaryField,
    Order(raw = raw::dictionary::ORDER, default = DefaultFallback::Ints),
}

/// Field of the `[search]` group (keys.h `Keys::Search`). The nested
/// `Keys::Search::Matcher` constants are leaf keys inside the same group in
/// C++ (`beginGroup("search")` + `s.value("exact-matcher", …)`), so the
/// matcher fields live here too — see [`SearchMatcherField`].
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum SearchField {
    MiddleMouseScan,
    Method,
    Delay,
    Modifier,
    HideMpvSubs,
    HideSubs,
    PauseOnHover,
    AutoPlayAudio,
    GlossaryStyle,
    ReplaceNewlines,
    ReplaceNewlinesWith,
    RemoveRegex,
    SubtitleFurigana,
    ShowAnkiGlossaryCheckbox,
}

field_enum! {
    SearchField,
    MiddleMouseScan(raw = raw::search::MIDDLE_MOUSE_SCAN, default = DefaultFallback::Bool(raw::search::MIDDLE_MOUSE_SCAN_DEFAULT)),
    Method(raw = raw::search::METHOD, default = DefaultFallback::Int(raw::search::METHOD_DEFAULT as i32)),
    Delay(raw = raw::search::DELAY, default = DefaultFallback::Int(raw::search::DELAY_DEFAULT)),
    Modifier(raw = raw::search::MODIFIER, default = DefaultFallback::Int(raw::search::MODIFIER_DEFAULT as i32)),
    HideMpvSubs(raw = raw::search::HIDE_MPV_SUBS, default = DefaultFallback::Bool(raw::search::HIDE_MPV_SUBS_DEFAULT)),
    HideSubs(raw = raw::search::HIDE_SUBS, default = DefaultFallback::Bool(raw::search::HIDE_SUBS_DEFAULT)),
    PauseOnHover(raw = raw::search::PAUSE_ON_HOVER, default = DefaultFallback::Bool(raw::search::PAUSE_ON_HOVER_DEFAULT)),
    AutoPlayAudio(raw = raw::search::AUTO_PLAY_AUDIO, default = DefaultFallback::Bool(raw::search::AUTO_PLAY_AUDIO_DEFAULT)),
    GlossaryStyle(raw = raw::search::GLOSSARY_STYLE, default = DefaultFallback::Int(raw::search::GLOSSARY_STYLE_DEFAULT as i32)),
    ReplaceNewlines(raw = raw::search::REPLACE_NEWLINES, default = DefaultFallback::Bool(raw::search::REPLACE_NEWLINES_DEFAULT)),
    ReplaceNewlinesWith(raw = raw::search::REPLACE_NEWLINES_WITH, default = DefaultFallback::Str(raw::search::REPLACE_NEWLINES_WITH_DEFAULT)),
    RemoveRegex(raw = raw::search::REMOVE_REGEX, default = DefaultFallback::Str(raw::search::REMOVE_REGEX_DEFAULT)),
    SubtitleFurigana(raw = raw::search::SUBTITLE_FURIGANA, default = DefaultFallback::Bool(raw::search::SUBTITLE_FURIGANA_DEFAULT)),
    ShowAnkiGlossaryCheckbox(raw = raw::search::SHOW_ANKI_GLOSSARY_CHECKBOX, default = DefaultFallback::Bool(raw::search::SHOW_ANKI_GLOSSARY_CHECKBOX_DEFAULT)),
}

/// Fields of `Keys::Search::Matcher` — leaf keys of the `[search]` group.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum SearchMatcherField {
    Exact,
    Deconj,
    MecabIpadic,
}

field_enum! {
    SearchMatcherField,
    Exact(raw = raw::search::matcher::EXACT, default = DefaultFallback::Bool(raw::search::matcher::EXACT_DEFAULT)),
    Deconj(raw = raw::search::matcher::DECONJ, default = DefaultFallback::Bool(raw::search::matcher::DECONJ_DEFAULT)),
    MecabIpadic(raw = raw::search::matcher::MECAB_IPADIC, default = DefaultFallback::Bool(raw::search::matcher::MECAB_IPADIC_DEFAULT)),
}

/// Field of the `[interface]` group (keys.h `Keys::Interface`; the
/// `Interface::Subtitle` / `Interface::SubtitleList` constants are leaf
/// keys inside this group in C++).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum InterfaceField {
    SystemIcons,
    SearchWindow,
    PopupWidth,
    PopupHeight,
}

field_enum! {
    InterfaceField,
    SystemIcons(raw = raw::interface::SYSTEM_ICONS, default = DefaultFallback::Bool(raw::interface::SYSTEM_ICONS_DEFAULT)),
    SearchWindow(raw = raw::interface::SEARCH_WINDOW, default = DefaultFallback::Bool(raw::interface::SEARCH_WINDOW_DEFAULT)),
    PopupWidth(raw = raw::interface::POPUP_WIDTH, default = DefaultFallback::Int(raw::interface::POPUP_WIDTH_DEFAULT)),
    PopupHeight(raw = raw::interface::POPUP_HEIGHT, default = DefaultFallback::Int(raw::interface::POPUP_HEIGHT_DEFAULT)),
}

/// Interface subtitle fields (keys.h `Keys::Interface::Subtitle`); the
/// `QFont` default is C++-only (see module header).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum InterfaceSubtitleField {
    Font,
    LineSpacing,
    Scale,
    Offset,
    Stroke,
    TextColor,
    BackgroundColor,
    StrokeColor,
    FuriganaRatio,
}

field_enum! {
    InterfaceSubtitleField,
    Font(raw = raw::interface::subtitle::FONT, default = DefaultFallback::None),
    LineSpacing(raw = raw::interface::subtitle::LINE_SPACING, default = DefaultFallback::Real(raw::interface::subtitle::LINE_SPACING_DEFAULT)),
    Scale(raw = raw::interface::subtitle::SCALE, default = DefaultFallback::Real(raw::interface::subtitle::SCALE_DEFAULT)),
    Offset(raw = raw::interface::subtitle::OFFSET, default = DefaultFallback::Real(raw::interface::subtitle::OFFSET_DEFAULT)),
    Stroke(raw = raw::interface::subtitle::STROKE, default = DefaultFallback::Real(raw::interface::subtitle::STROKE_DEFAULT)),
    TextColor(raw = raw::interface::subtitle::TEXT_COLOR, default = DefaultFallback::Str(raw::interface::subtitle::TEXT_COLOR_DEFAULT)),
    BackgroundColor(raw = raw::interface::subtitle::BACKGROUND_COLOR, default = DefaultFallback::Str(raw::interface::subtitle::BACKGROUND_COLOR_DEFAULT)),
    StrokeColor(raw = raw::interface::subtitle::STROKE_COLOR, default = DefaultFallback::Str(raw::interface::subtitle::STROKE_COLOR_DEFAULT)),
    FuriganaRatio(raw = raw::interface::subtitle::FURIGANA_RATIO, default = DefaultFallback::Real(raw::interface::subtitle::FURIGANA_RATIO_DEFAULT)),
}

/// Interface search-result font fields (keys.h 308-367); the `QFont`
/// defaults are C++-only.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum InterfaceSearchFontField {
    Expression,
    Reading,
    Conj,
    Tag,
    Glossary,
    Kanji,
}

field_enum! {
    InterfaceSearchFontField,
    Expression(raw = raw::interface::SEARCH_EXPRESSION_FONT, default = DefaultFallback::None),
    Reading(raw = raw::interface::SEARCH_READING_FONT, default = DefaultFallback::None),
    Conj(raw = raw::interface::SEARCH_CONJ_FONT, default = DefaultFallback::None),
    Tag(raw = raw::interface::SEARCH_TAG_FONT, default = DefaultFallback::None),
    Glossary(raw = raw::interface::SEARCH_GLOSSARY_FONT, default = DefaultFallback::None),
    Kanji(raw = raw::interface::SEARCH_KANJI_FONT, default = DefaultFallback::None),
}

/// Interface subtitle-list fields (keys.h `Keys::Interface::SubtitleList`);
/// font defaults are C++-only.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum InterfaceSubtitleListField {
    Window,
    Timestamps,
    BackgroundColor,
    TextColor,
    HoverColor,
    SelectedColor,
    SelectedHoverColor,
    PrimaryFont,
    SecondaryFont,
}

field_enum! {
    InterfaceSubtitleListField,
    Window(raw = raw::interface::subtitle_list::WINDOW, default = DefaultFallback::Bool(raw::interface::subtitle_list::WINDOW_DEFAULT)),
    Timestamps(raw = raw::interface::subtitle_list::TIMESTAMPS, default = DefaultFallback::Bool(raw::interface::subtitle_list::TIMESTAMPS_DEFAULT)),
    BackgroundColor(raw = raw::interface::subtitle_list::BACKGROUND_COLOR, default = DefaultFallback::Str(raw::interface::subtitle_list::BACKGROUND_COLOR_DEFAULT)),
    TextColor(raw = raw::interface::subtitle_list::TEXT_COLOR, default = DefaultFallback::Str(raw::interface::subtitle_list::TEXT_COLOR_DEFAULT)),
    HoverColor(raw = raw::interface::subtitle_list::HOVER_COLOR, default = DefaultFallback::Str(raw::interface::subtitle_list::HOVER_COLOR_DEFAULT)),
    SelectedColor(raw = raw::interface::subtitle_list::SELECTED_COLOR, default = DefaultFallback::Str(raw::interface::subtitle_list::SELECTED_COLOR_DEFAULT)),
    SelectedHoverColor(raw = raw::interface::subtitle_list::SELECTED_HOVER_COLOR, default = DefaultFallback::Str(raw::interface::subtitle_list::SELECTED_HOVER_COLOR_DEFAULT)),
    PrimaryFont(raw = raw::interface::subtitle_list::PRIMARY_FONT, default = DefaultFallback::None),
    SecondaryFont(raw = raw::interface::subtitle_list::SECONDARY_FONT, default = DefaultFallback::None),
}

/// Field of the `[keybind/profiles/<profile>]` groups (keys.h
/// `Keys::Keybind::Profile`): all string values, read only when the key
/// exists (see `loadKeybindSettings`).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum KeybindProfileField {
    CardAdd,
    CardNext,
    CardPrevious,
    OpenFile,
    OpenUrl,
    OscVisibility,
    ShowSearch,
    ShowSubtitleList,
    StartOcr,
    SubtitleAutoPause,
    SubtitleDecreaseSize,
    SubtitleIncreaseSize,
    SubtitleMoveDown,
    SubtitleMoveUp,
    SubtitleShow,
    SubtitleFurigana,
    SubtitleListCopySelected,
    SubtitleListFind,
    SubtitleListFindNext,
    SubtitleListFindPrevious,
}

field_enum! {
    KeybindProfileField,
    CardAdd(raw = raw::keybind::profile::CARD_ADD, default = DefaultFallback::None),
    CardNext(raw = raw::keybind::profile::CARD_NEXT, default = DefaultFallback::None),
    CardPrevious(raw = raw::keybind::profile::CARD_PREVIOUS, default = DefaultFallback::None),
    OpenFile(raw = raw::keybind::profile::OPEN_FILE, default = DefaultFallback::None),
    OpenUrl(raw = raw::keybind::profile::OPEN_URL, default = DefaultFallback::None),
    OscVisibility(raw = raw::keybind::profile::OSC_VISIBILITY, default = DefaultFallback::None),
    ShowSearch(raw = raw::keybind::profile::SHOW_SEARCH, default = DefaultFallback::None),
    ShowSubtitleList(raw = raw::keybind::profile::SHOW_SUBTITLE_LIST, default = DefaultFallback::None),
    StartOcr(raw = raw::keybind::profile::START_OCR, default = DefaultFallback::None),
    SubtitleAutoPause(raw = raw::keybind::profile::SUBTITLE_AUTO_PAUSE, default = DefaultFallback::None),
    SubtitleDecreaseSize(raw = raw::keybind::profile::SUBTITLE_DECREASE_SIZE, default = DefaultFallback::None),
    SubtitleIncreaseSize(raw = raw::keybind::profile::SUBTITLE_INCREASE_SIZE, default = DefaultFallback::None),
    SubtitleMoveDown(raw = raw::keybind::profile::SUBTITLE_MOVE_DOWN, default = DefaultFallback::None),
    SubtitleMoveUp(raw = raw::keybind::profile::SUBTITLE_MOVE_UP, default = DefaultFallback::None),
    SubtitleShow(raw = raw::keybind::profile::SUBTITLE_SHOW, default = DefaultFallback::None),
    SubtitleFurigana(raw = raw::keybind::profile::SUBTITLE_FURIGANA, default = DefaultFallback::None),
    SubtitleListCopySelected(raw = raw::keybind::profile::SUBTITLE_LIST_COPY_SELECTED, default = DefaultFallback::None),
    SubtitleListFind(raw = raw::keybind::profile::SUBTITLE_LIST_FIND, default = DefaultFallback::None),
    SubtitleListFindNext(raw = raw::keybind::profile::SUBTITLE_LIST_FIND_NEXT, default = DefaultFallback::None),
    SubtitleListFindPrevious(raw = raw::keybind::profile::SUBTITLE_LIST_FIND_PREVIOUS, default = DefaultFallback::None),
}

/// Field of the `[ocr]` group (keys.h `Keys::Ocr`).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum OcrField {
    Enabled,
    UseGpu,
    Model,
}

field_enum! {
    OcrField,
    Enabled(raw = raw::ocr::ENABLED, default = DefaultFallback::Bool(raw::ocr::ENABLED_DEFAULT)),
    UseGpu(raw = raw::ocr::USE_GPU, default = DefaultFallback::Bool(raw::ocr::USE_GPU_DEFAULT)),
    Model(raw = raw::ocr::MODEL, default = DefaultFallback::Str(raw::ocr::MODEL_DEFAULT)),
}

/// Field of the `[asr]` group (keys.h `Keys::Asr`).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum AsrField {
    Enabled,
    Backend,
    StartFromBeginning,
}

field_enum! {
    AsrField,
    Enabled(raw = raw::asr::ENABLED, default = DefaultFallback::Bool(raw::asr::ENABLED_DEFAULT)),
    Backend(raw = raw::asr::BACKEND, default = DefaultFallback::None),
    StartFromBeginning(raw = raw::asr::START_FROM_BEGINNING, default = DefaultFallback::Bool(raw::asr::START_FROM_BEGINNING_DEFAULT)),
}

/// Field of the `[whisper]` group (keys.h `Keys::Whisper`).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum WhisperField {
    Enabled,
    Model,
    CustomModel,
    VadEnabled,
    VadModel,
    UseGpu,
    GpuDevice,
    Threads,
    BestOf,
    BeamSize,
    FlashAttn,
    SuppressRegex,
}

field_enum! {
    WhisperField,
    Enabled(raw = raw::whisper::ENABLED, default = DefaultFallback::Bool(raw::whisper::ENABLED_DEFAULT)),
    Model(raw = raw::whisper::MODEL, default = DefaultFallback::Str(raw::whisper::MODEL_DEFAULT)),
    CustomModel(raw = raw::whisper::CUSTOM_MODEL, default = DefaultFallback::Str(raw::whisper::CUSTOM_MODEL_DEFAULT)),
    VadEnabled(raw = raw::whisper::VAD_ENABLED, default = DefaultFallback::Bool(raw::whisper::VAD_ENABLED_DEFAULT)),
    VadModel(raw = raw::whisper::VAD_MODEL, default = DefaultFallback::Str(raw::whisper::VAD_MODEL_DEFAULT)),
    UseGpu(raw = raw::whisper::USE_GPU, default = DefaultFallback::Bool(raw::whisper::USE_GPU_DEFAULT)),
    GpuDevice(raw = raw::whisper::GPU_DEVICE, default = DefaultFallback::Int(raw::whisper::GPU_DEVICE_DEFAULT)),
    Threads(raw = raw::whisper::THREADS, default = DefaultFallback::Int(raw::whisper::THREADS_DEFAULT)),
    BestOf(raw = raw::whisper::BEST_OF, default = DefaultFallback::Int(raw::whisper::BEST_OF_DEFAULT)),
    BeamSize(raw = raw::whisper::BEAM_SIZE, default = DefaultFallback::Int(raw::whisper::BEAM_SIZE_DEFAULT)),
    FlashAttn(raw = raw::whisper::FLASH_ATTN, default = DefaultFallback::Bool(raw::whisper::FLASH_ATTN_DEFAULT)),
    SuppressRegex(raw = raw::whisper::SUPPRESS_REGEX, default = DefaultFallback::Str(raw::whisper::SUPPRESS_REGEX_DEFAULT)),
}

/// Field of the `[onnx]` group (keys.h `Keys::Onnx`).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum OnnxField {
    Model,
    CustomModel,
    UseGpu,
    GpuDevice,
    Threads,
    Punctuation,
}

field_enum! {
    OnnxField,
    Model(raw = raw::onnx::MODEL, default = DefaultFallback::Str(raw::onnx::MODEL_DEFAULT)),
    CustomModel(raw = raw::onnx::CUSTOM_MODEL, default = DefaultFallback::Str(raw::onnx::CUSTOM_MODEL_DEFAULT)),
    UseGpu(raw = raw::onnx::USE_GPU, default = DefaultFallback::Bool(raw::onnx::USE_GPU_DEFAULT)),
    GpuDevice(raw = raw::onnx::GPU_DEVICE, default = DefaultFallback::Int(raw::onnx::GPU_DEVICE_DEFAULT)),
    Threads(raw = raw::onnx::THREADS, default = DefaultFallback::Int(raw::onnx::THREADS_DEFAULT)),
    Punctuation(raw = raw::onnx::PUNCTUATION, default = DefaultFallback::Bool(raw::onnx::PUNCTUATION_DEFAULT)),
}

/// Internal carrier of the `keys.h` defaults inside [`field_enum!`]: either
/// a concrete value or "no declared default" (Qt-dependent or runtime-only).
#[derive(Copy, Clone, Debug)]
enum DefaultFallback {
    Bool(bool),
    Int(i32),
    Real(f64),
    Str(&'static str),
    /// Declared string list with no constant default.
    Strings,
    /// Declared int list with no constant default.
    Ints,
    None,
}

impl DefaultFallback {
    fn as_value(self, _ctx: &DefaultContext) -> Option<Value> {
        match self {
            DefaultFallback::Bool(v) => Some(Value::Bool(v)),
            DefaultFallback::Int(v) => Some(Value::Int(i64::from(v))),
            DefaultFallback::Real(v) => Some(Value::Real(v)),
            DefaultFallback::Str(s) => Some(Value::Text(s.to_owned())),
            DefaultFallback::Strings | DefaultFallback::Ints | DefaultFallback::None => None,
        }
    }

    fn value_type(self, _ctx: &DefaultContext) -> ValueType {
        match self {
            DefaultFallback::Bool(_) => ValueType::Bool,
            DefaultFallback::Int(_) => ValueType::Int,
            DefaultFallback::Real(_) => ValueType::Real,
            DefaultFallback::Str(_) => ValueType::Text,
            DefaultFallback::Strings => ValueType::Strings,
            DefaultFallback::Ints => ValueType::Ints,
            DefaultFallback::None => ValueType::Text,
        }
    }
}

/// The typed address of one setting.
///
/// Constructing a `Key` cannot produce a wrong group/key pair: the string
/// stored on disk is always derived from a certified
/// [`memento_setting::keys`] constant, profile names come in as parameters
/// (`KeybindProfile`), and audio source array entries are addressed by
/// zero-based index (`AudioSource`), which is how `QSettings` arrays behave
/// (`beginWriteArray` + `setArrayIndex`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Key {
    /// Root `version` key (stored in the `general` section).
    Version,
    Window(WindowField),
    Internal(InternalField),
    Recent(RecentField),
    SubtitleList(SubtitleListField),
    /// One entry of the `audio-srcs` QSettings array.
    AudioSource {
        /// Zero-based array index (C++ `setArrayIndex`). Kept as `u32`.
        index: u32,
        field: AudioSourceField,
    },
    Application(ApplicationField),
    Behavior(BehaviorField),
    Dictionary(DictionaryField),
    Search(SearchField),
    /// `Keys::Search::Matcher` constants (leaf keys of `[search]`).
    SearchMatcher(SearchMatcherField),
    Interface(InterfaceField),
    /// The `Keys::Interface::Subtitle` fields (leaf keys of `[interface]`).
    InterfaceSubtitle(InterfaceSubtitleField),
    /// The search-result font keys (leaf keys of `[interface]`).
    InterfaceSearchFont(InterfaceSearchFontField),
    /// The `Keys::Interface::SubtitleList` fields (leaf keys of
    /// `[interface]`).
    InterfaceSubtitleList(InterfaceSubtitleListField),
    /// `[keybind] current` — the active keybind profile name.
    KeybindCurrent,
    /// A field of one keybind profile
    /// (`[keybind/profiles/<profile>] <field>`).
    KeybindProfile {
        profile: Box<str>,
        field: KeybindProfileField,
    },
    Ocr(OcrField),
    Asr(AsrField),
    Whisper(WhisperField),
    Onnx(OnnxField),
}

impl Key {
    /// The store coordinate of this key.
    #[must_use]
    pub fn path(&self) -> KeyPath {
        match self {
            Key::Version => KeyPath::new(GENERAL_GROUP, raw::version::VERSION),
            Key::Window(f) => KeyPath::new(raw::window::GROUP, f.raw()),
            Key::Internal(f) => KeyPath::new(raw::internal::GROUP, f.raw()),
            Key::Recent(f) => KeyPath::new(raw::recent::GROUP, f.raw()),
            Key::SubtitleList(f) => KeyPath::new(raw::subtitle_list::GROUP, f.raw()),
            Key::AudioSource { index, field } => KeyPath::new(
                format!("{}/{}", raw::audio_source::GROUP, index),
                field.raw(),
            ),
            Key::Application(f) => KeyPath::new(raw::application::GROUP, f.raw()),
            Key::Behavior(f) => KeyPath::new(raw::behavior::GROUP, f.raw()),
            Key::Dictionary(f) => KeyPath::new(raw::dictionary::GROUP, f.raw()),
            Key::Search(f) => KeyPath::new(raw::search::GROUP, f.raw()),
            Key::SearchMatcher(f) => KeyPath::new(raw::search::GROUP, f.raw()),
            Key::Interface(f) => KeyPath::new(raw::interface::GROUP, f.raw()),
            Key::InterfaceSubtitle(f) => KeyPath::new(raw::interface::GROUP, f.raw()),
            Key::InterfaceSearchFont(f) => KeyPath::new(raw::interface::GROUP, f.raw()),
            Key::InterfaceSubtitleList(f) => KeyPath::new(raw::interface::GROUP, f.raw()),
            Key::KeybindCurrent => {
                KeyPath::new(raw::keybind::GROUP, raw::keybind::CURRENT_PROFILE)
            }
            Key::KeybindProfile { profile, field } => KeyPath::new(
                format!(
                    "{}/{}/{}",
                    raw::keybind::GROUP,
                    raw::keybind::profile::GROUP,
                    profile
                ),
                field.raw(),
            ),
            Key::Ocr(f) => KeyPath::new(raw::ocr::GROUP, f.raw()),
            Key::Asr(f) => KeyPath::new(raw::asr::GROUP, f.raw()),
            Key::Whisper(f) => KeyPath::new(raw::whisper::GROUP, f.raw()),
            Key::Onnx(f) => KeyPath::new(raw::onnx::GROUP, f.raw()),
        }
    }

    /// Value type the key declares; drives typed decoding and empty
    /// container disambiguation on load.
    #[must_use]
    pub fn value_type(&self, ctx: &DefaultContext) -> ValueType {
        match self {
            Key::Version => ValueType::Int,
            Key::Window(f) => f.value_type(ctx),
            Key::Internal(f) => f.value_type(ctx),
            Key::Recent(f) => f.value_type(ctx),
            Key::SubtitleList(f) => f.value_type(ctx),
            Key::AudioSource { field, .. } => field.value_type(ctx),
            Key::Application(f) => f.value_type(ctx),
            Key::Behavior(f) => f.value_type(ctx),
            Key::Dictionary(f) => f.value_type(ctx),
            Key::Search(f) => f.value_type(ctx),
            Key::SearchMatcher(f) => f.value_type(ctx),
            Key::Interface(f) => f.value_type(ctx),
            Key::InterfaceSubtitle(f) => f.value_type(ctx),
            Key::InterfaceSearchFont(f) => f.value_type(ctx),
            Key::InterfaceSubtitleList(f) => f.value_type(ctx),
            Key::KeybindCurrent => ValueType::Text,
            Key::KeybindProfile { field, .. } => field.value_type(ctx),
            Key::Ocr(f) => f.value_type(ctx),
            Key::Asr(f) => f.value_type(ctx),
            Key::Whisper(f) => f.value_type(ctx),
            Key::Onnx(f) => f.value_type(ctx),
        }
    }

    /// The store default of the key: `Some` when `keys.h` declares a
    /// constant default (C++-side filesystem / `QFont` defaults excluded,
    /// see the module header), `None` otherwise.
    #[must_use]
    pub fn declared_default(&self, ctx: &DefaultContext) -> Option<Value> {
        match self {
            Key::Version => Some(Value::Int(0)), // loadVersion(): value(..., 0)
            Key::Window(_) => Some(Value::Bool(false)), // C++ reads with literal false default
            Key::Internal(_) => Some(Value::Bool(false)), // same, loadInternalSettings
            Key::Recent(f) => f.declared_default(ctx),
            Key::SubtitleList(f) => f.declared_default(ctx),
            Key::AudioSource { field, .. } => field.declared_default(ctx),
            Key::Application(f) => f.declared_default(ctx),
            Key::Behavior(f) => match f {
                // FILE_OPEN_CUSTOM: "file://" + home directory (keys.h:192-194)
                BehaviorField::FileOpenCustom => Some(file_open_custom_default(&ctx.home_dir)),
                other => other.declared_default(ctx),
            },
            Key::Dictionary(f) => f.declared_default(ctx),
            Key::Search(f) => f.declared_default(ctx),
            Key::SearchMatcher(f) => f.declared_default(ctx),
            Key::Interface(f) => f.declared_default(ctx),
            Key::InterfaceSubtitle(f) => f.declared_default(ctx),
            Key::InterfaceSearchFont(_) => None, // QFont defaults are C++-only
            Key::InterfaceSubtitleList(f) => f.declared_default(ctx),
            Key::KeybindCurrent => None, // read without default
            Key::KeybindProfile { field, .. } => field.declared_default(ctx),
            Key::Ocr(f) => f.declared_default(ctx),
            Key::Asr(f) => f.declared_default(ctx),
            Key::Whisper(f) => f.declared_default(ctx),
            Key::Onnx(f) => f.declared_default(ctx),
        }
    }
}

/// Resolves a raw store coordinate back into a typed [`Key`], if it is part
/// of the key model. Used by the legacy INI loader (type-directed decoding)
/// and the migration engine; unknown coordinates return `None` and stay
/// preserved verbatim on round-trip.
#[must_use]
pub fn lookup(group: &str, raw_key: &str) -> Option<Key> {
    macro_rules! find_field {
        ($t:ty, $variant:ident) => {
            <$t>::ALL
                .iter()
                .copied()
                .find(|f| f.raw() == raw_key)
                .map(Key::$variant)
        };
    }
    if group.eq_ignore_ascii_case(GENERAL_GROUP) {
        return (raw_key == raw::version::VERSION).then_some(Key::Version);
    }
    match group {
        g if g == raw::window::GROUP => find_field!(WindowField, Window),
        g if g == raw::internal::GROUP => find_field!(InternalField, Internal),
        g if g == raw::recent::GROUP => find_field!(RecentField, Recent),
        g if g == raw::subtitle_list::GROUP => {
            find_field!(SubtitleListField, SubtitleList)
        }
        g if g == raw::application::GROUP => {
            find_field!(ApplicationField, Application)
        }
        g if g == raw::behavior::GROUP => find_field!(BehaviorField, Behavior),
        g if g == raw::dictionary::GROUP => find_field!(DictionaryField, Dictionary),
        g if g == raw::search::GROUP => {
            if let Some(key) = find_field!(SearchField, Search) {
                return Some(key);
            }
            find_field!(SearchMatcherField, SearchMatcher)
        }
        g if g == raw::interface::GROUP => {
            if let Some(key) = find_field!(InterfaceField, Interface) {
                return Some(key);
            }
            if let Some(key) = find_field!(InterfaceSubtitleField, InterfaceSubtitle) {
                return Some(key);
            }
            if let Some(key) = find_field!(InterfaceSearchFontField, InterfaceSearchFont) {
                return Some(key);
            }
            find_field!(InterfaceSubtitleListField, InterfaceSubtitleList)
        }
        g if g == raw::ocr::GROUP => find_field!(OcrField, Ocr),
        g if g == raw::asr::GROUP => find_field!(AsrField, Asr),
        g if g == raw::whisper::GROUP => find_field!(WhisperField, Whisper),
        g if g == raw::onnx::GROUP => find_field!(OnnxField, Onnx),
        g if g == raw::keybind::GROUP => {
            (raw_key == raw::keybind::CURRENT_PROFILE).then_some(Key::KeybindCurrent)
        }
        g if audio_index_of(g).is_some() => {
            let field = AudioSourceField::ALL
                .iter()
                .copied()
                .find(|f| f.raw() == raw_key)?;
            Some(Key::AudioSource {
                index: audio_index_of(g).expect("checked above"),
                field,
            })
        }
        g if g.starts_with(KEYBIND_PROFILES_PREFIX) => {
            let field = KeybindProfileField::ALL
                .iter()
                .copied()
                .find(|f| f.raw() == raw_key)?;
            let profile = &g[KEYBIND_PROFILES_PREFIX.len()..];
            if profile.contains('/') {
                return None;
            }
            Some(Key::KeybindProfile {
                profile: profile.to_owned().into_boxed_str(),
                field,
            })
        }
        _ => None,
    }
}

/// `keybind/profiles/`
pub const KEYBIND_PROFILES_PREFIX: &str = "keybind/profiles/";

/// Extracts the array index of an `audio-srcs/<n>` group path.
fn audio_index_of(group: &str) -> Option<u32> {
    let index = group.strip_prefix(raw::audio_source::GROUP)?;
    let index = index.strip_prefix('/')?;
    if index.is_empty() || !index.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    index.parse().ok()
}

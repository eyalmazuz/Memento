//! Settings schema: category enum, choice tables driven by `subomi_setting::enums`,
//! ranges, defaults, value parsing/normalization, and category snapshots.

use std::collections::BTreeMap;
use std::path::Path;

use subomi_audio::{MAX_AUDIO_DOWNLOAD_TIMEOUT_MS, MIN_AUDIO_DOWNLOAD_TIMEOUT_MS};
use subomi_config::config::Config;
use subomi_config::keys::lookup;
use subomi_config::{ArgbColor, FontSpec, KeyPath, Value, ValueType};
use subomi_dict::searchcontroller::{TextReplacementOptions, TextReplacementRule};
use subomi_setting::enums::{
    AudioSourceType, Directory, GlossaryStyle, Language, Modifier, SearchMethod, ThemeMode,
    UpdateChannel,
};
use subomi_setting::keys as raw;

use super::audio_sources::audio_sources;
use super::keybinds::{
    active_keybind_profile_name, keybind_default, keybind_profile_names, keybind_profile_prefix,
    keybind_value, KEYBIND_FIELDS,
};

#[repr(i32)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum SettingsCategory {
    Anki = 0,
    Application = 1,
    Audio = 2,
    Behavior = 3,
    Dictionary = 4,
    Interface = 5,
    Keybind = 6,
    Ocr = 7,
    Asr = 8,
    Search = 9,
}

impl SettingsCategory {
    pub(crate) const fn as_i32(self) -> i32 {
        self as i32
    }

    pub(crate) fn from_i32(category: i32) -> Result<Self, String> {
        match category {
            0 => Ok(Self::Anki),
            1 => Ok(Self::Application),
            2 => Ok(Self::Audio),
            3 => Ok(Self::Behavior),
            4 => Ok(Self::Dictionary),
            5 => Ok(Self::Interface),
            6 => Ok(Self::Keybind),
            7 => Ok(Self::Ocr),
            8 => Ok(Self::Asr),
            9 => Ok(Self::Search),
            _ => Err(format!("unknown settings category {category}")),
        }
    }
}

#[derive(Clone)]
pub(crate) struct ConfigSnapshot {
    pub(crate) values: Vec<(String, String, Option<Value>)>,
    pub(crate) replace_prefixes: Vec<String>,
}

pub(crate) const LANGUAGE_CHOICES: &[(Language, &str)] = &[
    (Language::LanguageSystem, "System"),
    (Language::LanguageEnglish, "English"),
    (Language::LanguageKorean, "한국어 (MTL)"),
    (Language::LanguageChineseSimplified, "简体中文 (MTL)"),
    (Language::LanguageChineseTraditional, "繁體中文 (MTL)"),
];

pub(crate) const AUDIO_SOURCE_TYPE_CHOICES: &[(AudioSourceType, &str)] = &[
    (AudioSourceType::AudioSourceTypeFile, "File"),
    (AudioSourceType::AudioSourceTypeJson, "JSON"),
];

pub(crate) const DIRECTORY_CHOICES: &[(Directory, &str)] = &[
    (Directory::DirectoryCurrent, "Current"),
    (Directory::DirectoryHome, "Home"),
    (Directory::DirectoryMovies, "Movies"),
    (Directory::DirectoryDocuments, "Documents"),
    (Directory::DirectoryCustom, "Custom"),
];

pub(crate) const SEARCH_METHOD_CHOICES: &[(SearchMethod, &str)] = &[
    (SearchMethod::SearchMethodHover, "Hover"),
    (SearchMethod::SearchMethodModifier, "Modifier"),
];

pub(crate) const MODIFIER_CHOICES: &[(Modifier, &str)] = &[
    (Modifier::ModifierAlt, "Alt"),
    (Modifier::ModifierControl, "Control"),
    (Modifier::ModifierShift, "Shift"),
    (Modifier::ModifierSuper, "Super"),
];

pub(crate) const GLOSSARY_STYLE_CHOICES: &[(GlossaryStyle, &str)] = &[
    (GlossaryStyle::GlossaryStyleBullet, "Bullet Points"),
    (GlossaryStyle::GlossaryStyleLineBreak, "Line Breaks"),
    (GlossaryStyle::GlossaryStylePipe, "Pipes"),
];

pub(crate) const THEME_CHOICES: &[(ThemeMode, &str)] = &[
    (ThemeMode::System, "System"),
    (ThemeMode::Dark, "Dark"),
    (ThemeMode::Light, "Light"),
];

pub(crate) const UPDATE_CHANNEL_CHOICES: &[(UpdateChannel, &str)] = &[
    (UpdateChannel::Stable, "Stable"),
    (UpdateChannel::Nightly, "Nightly"),
];

pub(crate) fn default_update_channel_for_build() -> UpdateChannel {
    match option_env!("SUBOMI_BUILD_CHANNEL")
        .map(str::trim)
        .unwrap_or("")
    {
        "nightly" => UpdateChannel::Nightly,
        _ => UpdateChannel::Stable,
    }
}

pub(crate) fn update_channel_label(raw_value: &str) -> &'static str {
    let channel =
        UpdateChannel::from_str_with_default(raw_value, default_update_channel_for_build());
    UPDATE_CHANNEL_CHOICES
        .iter()
        .find(|(item, _)| *item == channel)
        .map_or("Stable", |(_, label)| *label)
}

/// Browse dialog kind for filesystem path settings (C1.1).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum BrowseKind {
    Folder,
    File,
}

/// Metadata describing native Browse button behavior for a path/folder setting.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct SettingBrowseMetadata {
    pub key: &'static str,
    pub kind: BrowseKind,
    pub dialog_title: &'static str,
    pub store_as_file_uri: bool,
}

/// Return folder/file browse metadata for settings that support a native
/// Browse button in Settings UI (including `behavior/file-open-custom`).
pub fn setting_browse_metadata(key: &str) -> Option<SettingBrowseMetadata> {
    match key {
        "behavior/file-open-custom" => Some(SettingBrowseMetadata {
            key: "behavior/file-open-custom",
            kind: BrowseKind::Folder,
            dialog_title: "Choose Custom Open Folder",
            store_as_file_uri: true,
        }),
        "onnx/custom-model" => Some(SettingBrowseMetadata {
            key: "onnx/custom-model",
            kind: BrowseKind::Folder,
            dialog_title: "Choose Custom ONNX Model Directory",
            store_as_file_uri: false,
        }),
        "whisper/custom-model" => Some(SettingBrowseMetadata {
            key: "whisper/custom-model",
            kind: BrowseKind::File,
            dialog_title: "Choose Custom Whisper Model",
            store_as_file_uri: false,
        }),
        "whisper/vad-model" => Some(SettingBrowseMetadata {
            key: "whisper/vad-model",
            kind: BrowseKind::File,
            dialog_title: "Choose Whisper VAD Model",
            store_as_file_uri: false,
        }),
        _ => None,
    }
}

/// Format a path picked from a native file/folder browse dialog for storage
/// under `key` (converting to a `file://` URI for `behavior/file-open-custom`).
pub fn format_browse_selection(key: &str, selected_path: &Path) -> String {
    if setting_browse_metadata(key).is_some_and(|meta| meta.store_as_file_uri) {
        if let Ok(url) = url::Url::from_file_path(selected_path) {
            return url.into();
        }
        return format!("file://{}", selected_path.to_string_lossy());
    }
    selected_path.to_string_lossy().into_owned()
}

pub(crate) fn theme_label(raw_value: &str) -> &'static str {
    let mode = ThemeMode::from_str_lossy(raw_value);
    THEME_CHOICES
        .iter()
        .find(|(item, _)| *item == mode)
        .map_or("System", |(_, label)| *label)
}

pub(crate) fn language_label(raw_value: i64) -> &'static str {
    let variant = i32::try_from(raw_value)
        .ok()
        .and_then(|v| Language::try_from(v).ok())
        .unwrap_or(Language::LanguageSystem);
    LANGUAGE_CHOICES
        .iter()
        .find(|(item, _)| *item == variant)
        .map_or("System", |(_, label)| *label)
}

pub(crate) fn audio_source_type_label(raw_value: i64) -> &'static str {
    let variant = i32::try_from(raw_value)
        .ok()
        .and_then(|v| AudioSourceType::try_from(v).ok())
        .unwrap_or(AudioSourceType::AudioSourceTypeFile);
    AUDIO_SOURCE_TYPE_CHOICES
        .iter()
        .find(|(item, _)| *item == variant)
        .map_or("File", |(_, label)| *label)
}

pub(crate) fn directory_label(raw_value: i64) -> &'static str {
    let variant = i32::try_from(raw_value)
        .ok()
        .and_then(|v| Directory::try_from(v).ok())
        .unwrap_or(Directory::DirectoryMovies);
    DIRECTORY_CHOICES
        .iter()
        .find(|(item, _)| *item == variant)
        .map_or("Movies", |(_, label)| *label)
}

pub(crate) fn search_method_label(raw_value: i64) -> &'static str {
    let variant = i32::try_from(raw_value)
        .ok()
        .and_then(|v| SearchMethod::try_from(v).ok())
        .unwrap_or(SearchMethod::SearchMethodHover);
    SEARCH_METHOD_CHOICES
        .iter()
        .find(|(item, _)| *item == variant)
        .map_or("Hover", |(_, label)| *label)
}

pub(crate) fn modifier_label(raw_value: i64) -> &'static str {
    let variant = i32::try_from(raw_value)
        .ok()
        .and_then(|v| Modifier::try_from(v).ok())
        .unwrap_or(Modifier::ModifierShift);
    MODIFIER_CHOICES
        .iter()
        .find(|(item, _)| *item == variant)
        .map_or("Shift", |(_, label)| *label)
}

pub(crate) fn glossary_style_label(raw_value: i64) -> &'static str {
    let variant = i32::try_from(raw_value)
        .ok()
        .and_then(|v| GlossaryStyle::try_from(v).ok())
        .unwrap_or(GlossaryStyle::GlossaryStyleBullet);
    GLOSSARY_STYLE_CHOICES
        .iter()
        .find(|(item, _)| *item == variant)
        .map_or("Bullet Points", |(_, label)| *label)
}

pub(crate) const INTERFACE_FONT_KEYS: &[&str] = &[
    raw::interface::subtitle::FONT,
    raw::interface::SEARCH_EXPRESSION_FONT,
    raw::interface::SEARCH_READING_FONT,
    raw::interface::SEARCH_CONJ_FONT,
    raw::interface::SEARCH_TAG_FONT,
    raw::interface::SEARCH_GLOSSARY_FONT,
    raw::interface::SEARCH_KANJI_FONT,
    raw::interface::subtitle_list::PRIMARY_FONT,
    raw::interface::subtitle_list::SECONDARY_FONT,
];

pub(crate) fn is_font_key(group: &str, key: &str) -> bool {
    group == raw::interface::GROUP && INTERFACE_FONT_KEYS.contains(&key)
}

pub(crate) fn is_color_key(group: &str, key: &str) -> bool {
    group == raw::interface::GROUP
        && matches!(
            key,
            raw::interface::subtitle::TEXT_COLOR
                | raw::interface::subtitle::BACKGROUND_COLOR
                | raw::interface::subtitle::STROKE_COLOR
                | raw::interface::subtitle_list::BACKGROUND_COLOR
                | raw::interface::subtitle_list::TEXT_COLOR
                | raw::interface::subtitle_list::HOVER_COLOR
                | raw::interface::subtitle_list::SELECTED_COLOR
                | raw::interface::subtitle_list::SELECTED_HOVER_COLOR
        )
}

pub(crate) fn value_text(value: &Value) -> String {
    match value {
        Value::Bool(value) => value.to_string(),
        Value::Int(value) => value.to_string(),
        Value::Real(value) => value.to_string(),
        Value::Text(value) => value.clone(),
        Value::Strings(values) => values.join(", "),
        Value::Ints(values) => values
            .iter()
            .map(i64::to_string)
            .collect::<Vec<_>>()
            .join(", "),
        Value::Map(values) => serde_json::to_string(values).unwrap_or_default(),
    }
}

pub(crate) fn parse_bool(value: &str) -> Result<bool, String> {
    match value {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(format!("expected true or false, received {value:?}")),
    }
}

fn qt_font_default(key: &str) -> Option<Value> {
    let (family, mac_size, other_size, weight) = match key {
        raw::interface::subtitle::FONT => ("Noto Sans CJK JP", 24, 24, 50),
        raw::interface::SEARCH_EXPRESSION_FONT => ("Noto Sans CJK JP", 28, 20, 50),
        raw::interface::SEARCH_READING_FONT | raw::interface::SEARCH_CONJ_FONT => {
            ("Noto Sans CJK JP", 16, 12, 50)
        }
        raw::interface::SEARCH_TAG_FONT => ("Noto Sans CJK JP", 14, 10, 75),
        raw::interface::SEARCH_GLOSSARY_FONT => ("Noto Sans CJK JP", 14, 10, 50),
        raw::interface::SEARCH_KANJI_FONT => ("Kanji Stroke Orders", 156, 108, 50),
        raw::interface::subtitle_list::PRIMARY_FONT => ("Noto Sans CJK JP", 20, 16, 50),
        raw::interface::subtitle_list::SECONDARY_FONT => ("Sans Serif", 20, 16, 50),
        _ => return None,
    };
    let point_size = if cfg!(target_os = "macos") {
        mac_size
    } else {
        other_size
    };
    // Ten-field form accepted by QFont::fromString on both Qt 5 and Qt 6.
    Some(Value::Text(format!(
        "{family},{point_size},-1,5,{weight},0,0,0,0,0"
    )))
}

pub(crate) fn declared_setting_default(config: &Config, group: &str, key: &str) -> Option<Value> {
    if group == raw::behavior::GROUP && key == raw::behavior::FILE_OPEN_CUSTOM {
        let home = if config.context().home_dir.is_empty() {
            std::env::var_os("HOME")
                .or_else(|| std::env::var_os("USERPROFILE"))
                .map(|path| path.to_string_lossy().into_owned())
                .unwrap_or_default()
        } else {
            config.context().home_dir.clone()
        };
        return Some(Value::Text(format!("file://{home}")));
    }
    if group == raw::application::GROUP && key == raw::application::UPDATE_CHANNEL {
        return Some(Value::Text(
            default_update_channel_for_build().as_str().to_owned(),
        ));
    }
    if group == raw::interface::GROUP {
        if let Some(value) = qt_font_default(key) {
            return Some(value);
        }
        match key {
            raw::interface::THEME => {
                return Some(Value::Text(raw::interface::THEME_DEFAULT.to_owned()));
            }
            raw::interface::COMPACT_GLOSSARIES => {
                return Some(Value::Bool(raw::interface::COMPACT_GLOSSARIES_DEFAULT));
            }
            raw::interface::COMPACT_TAGS => {
                return Some(Value::Bool(raw::interface::COMPACT_TAGS_DEFAULT));
            }
            _ => {}
        }
    }
    if group == raw::search::GROUP {
        match key {
            raw::search::HOVER_PAUSE_RESUME => {
                return Some(Value::Bool(raw::search::HOVER_PAUSE_RESUME_DEFAULT));
            }
            raw::search::TEXT_REPLACEMENTS => {
                return Some(Value::Text(
                    raw::search::TEXT_REPLACEMENTS_DEFAULT.to_owned(),
                ));
            }
            raw::search::TEXT_REPLACEMENTS_SEARCH_ORIGINAL => {
                return Some(Value::Bool(
                    raw::search::TEXT_REPLACEMENTS_SEARCH_ORIGINAL_DEFAULT,
                ));
            }
            _ => {}
        }
    }
    if group == raw::audio::GROUP && key == raw::audio::DOWNLOAD_TIMEOUT_MS {
        return Some(Value::Int(i64::from(
            raw::audio::DOWNLOAD_TIMEOUT_MS_DEFAULT,
        )));
    }
    if group.starts_with(&keybind_profile_prefix()) {
        let default = keybind_default(key);
        if !default.is_empty() {
            return Some(Value::Text(default.to_owned()));
        }
    }
    lookup(group, key).and_then(|typed| typed.declared_default(config.context()))
}

pub(crate) fn asr_thread_limit() -> i64 {
    std::thread::available_parallelism()
        .map_or(1, |count| count.get())
        .max(1) as i64
}

pub(crate) fn default_asr_thread_count() -> i64 {
    4.min(asr_thread_limit())
}

pub(crate) fn normalize_setting_value(group: &str, key: &str, value: Value) -> Value {
    match (group, key, value) {
        (raw::whisper::GROUP, raw::whisper::THREADS, Value::Int(requested))
        | (raw::onnx::GROUP, raw::onnx::THREADS, Value::Int(requested)) => {
            Value::Int(if requested <= 0 {
                default_asr_thread_count()
            } else {
                requested.clamp(1, asr_thread_limit())
            })
        }
        (raw::whisper::GROUP, raw::whisper::BEST_OF, Value::Int(value))
        | (raw::whisper::GROUP, raw::whisper::BEAM_SIZE, Value::Int(value)) => {
            Value::Int(value.clamp(1, 10))
        }
        (raw::audio::GROUP, raw::audio::DOWNLOAD_TIMEOUT_MS, Value::Int(value)) => Value::Int(
            value.clamp(
                MIN_AUDIO_DOWNLOAD_TIMEOUT_MS as i64,
                MAX_AUDIO_DOWNLOAD_TIMEOUT_MS as i64,
            ),
        ),
        (raw::interface::GROUP, raw::interface::THEME, Value::Text(raw_theme)) => {
            Value::Text(ThemeMode::from_str_lossy(&raw_theme).as_str().to_owned())
        }
        (_, _, value) => value,
    }
}

/// Parse configured `search/text-replacements` and
/// `search/text-replacements-search-original` into typed [`TextReplacementOptions`].
pub(crate) fn text_replacement_options_from_config(config: &Config) -> TextReplacementOptions {
    let raw_rules = match effective_value(config, raw::search::GROUP, raw::search::TEXT_REPLACEMENTS)
    {
        Value::Text(text) => text,
        _ => config
            .raw_at(raw::search::GROUP, raw::search::TEXT_REPLACEMENTS)
            .unwrap_or(raw::search::TEXT_REPLACEMENTS_DEFAULT)
            .to_owned(),
    };
    let rules = serde_json::from_str::<Vec<TextReplacementRule>>(raw_rules.trim()).unwrap_or_default();
    let search_original = effective_bool(
        config,
        raw::search::GROUP,
        raw::search::TEXT_REPLACEMENTS_SEARCH_ORIGINAL,
    );
    TextReplacementOptions {
        rules,
        search_original,
    }
}

pub(crate) fn normalize_loaded_asr_values(config: &mut Config) {
    for (group, key) in [
        (raw::whisper::GROUP, raw::whisper::THREADS),
        (raw::whisper::GROUP, raw::whisper::BEST_OF),
        (raw::whisper::GROUP, raw::whisper::BEAM_SIZE),
        (raw::onnx::GROUP, raw::onnx::THREADS),
    ] {
        if config.raw_at(group, key).is_none() {
            continue;
        }
        if let Some(value) = config.get_at(group, key) {
            let value = normalize_setting_value(group, key, value);
            let _ = config.put_at(group, key, &value);
        }
    }
}

pub(crate) fn effective_value(config: &Config, group: &str, key: &str) -> Value {
    config
        .get_at(group, key)
        .or_else(|| declared_setting_default(config, group, key))
        .map(|value| normalize_setting_value(group, key, value))
        .unwrap_or_else(|| {
            match lookup(group, key).map(|k| k.value_type(config.context())) {
                Some(ValueType::Bool) => Value::Bool(false),
                Some(ValueType::Int) => Value::Int(0),
                Some(ValueType::Real) => Value::Real(0.0),
                Some(ValueType::Strings) => Value::Strings(Vec::new()),
                Some(ValueType::Ints) => Value::Ints(Vec::new()),
                Some(ValueType::Map) => Value::Map(BTreeMap::new()),
                Some(ValueType::Text) | None => Value::Text(String::new()),
            }
        })
}

pub(crate) fn effective_text(config: &Config, group: &str, key: &str) -> Option<String> {
    match effective_value(config, group, key) {
        Value::Text(value) if !value.is_empty() => Some(value),
        _ => None,
    }
}

pub(crate) fn effective_bool(config: &Config, group: &str, key: &str) -> bool {
    match effective_value(config, group, key) {
        Value::Bool(value) => value,
        Value::Text(value) => value.eq_ignore_ascii_case("true"),
        _ => false,
    }
}

pub(crate) fn effective_i64(config: &Config, group: &str, key: &str) -> i64 {
    match effective_value(config, group, key) {
        Value::Int(value) => value,
        Value::Text(value) => value.parse().unwrap_or_default(),
        _ => 0,
    }
}

pub(crate) fn category_paths(category: SettingsCategory) -> Vec<(&'static str, &'static str)> {
    match category {
        SettingsCategory::Application => vec![
            (raw::application::GROUP, raw::application::LANGUAGE),
            (raw::application::GROUP, raw::application::AUTO_UPDATE_CHECK),
            (raw::application::GROUP, raw::application::UPDATE_CHANNEL),
        ],
        SettingsCategory::Behavior => behavior_paths(),
        SettingsCategory::Dictionary => vec![(raw::dictionary::GROUP, raw::dictionary::ORDER)],
        SettingsCategory::Interface => interface_paths(),
        SettingsCategory::Ocr => ocr_paths(),
        SettingsCategory::Asr => [asr_paths(), whisper_paths(), onnx_paths()].concat(),
        SettingsCategory::Search => search_paths(),
        SettingsCategory::Anki | SettingsCategory::Audio | SettingsCategory::Keybind => Vec::new(),
    }
}

pub(crate) fn path_in_category(category: SettingsCategory, path: &KeyPath) -> bool {
    match category {
        SettingsCategory::Audio => {
            path.group == raw::audio_source::GROUP
                || path
                    .group
                    .starts_with(&format!("{}/", raw::audio_source::GROUP))
                || (path.group == raw::audio::GROUP && path.key == raw::audio::DOWNLOAD_TIMEOUT_MS)
        }
        SettingsCategory::Keybind => {
            (path.group == raw::keybind::GROUP && path.key == raw::keybind::CURRENT_PROFILE)
                || path.group.starts_with(&keybind_profile_prefix())
        }
        _ => category_paths(category)
            .iter()
            .any(|(group, key)| *group == path.group && *key == path.key),
    }
}

pub(crate) fn write_default_value(config: &mut Config, group: &str, key: &str) {
    match declared_setting_default(config, group, key) {
        Some(value) => {
            let _ = config.put_at(group, key, &value);
        }
        None => {
            config.remove_at(group, key);
        }
    }
}

pub(crate) fn write_default_paths(config: &mut Config, paths: Vec<(&'static str, &'static str)>) {
    for (group, key) in paths {
        write_default_value(config, group, key);
    }
}

pub(crate) fn behavior_paths() -> Vec<(&'static str, &'static str)> {
    let group = raw::behavior::GROUP;
    vec![
        (group, raw::behavior::AUTO_FIT),
        (group, raw::behavior::AUTO_FIT_PERCENT),
        (group, raw::behavior::OSC_MPV_CURSOR),
        (group, raw::behavior::OSC_DURATION),
        (group, raw::behavior::OSC_FADE),
        (group, raw::behavior::OSC_MIN_MOVE),
        (group, raw::behavior::OSC_PREVIEW_THUMBNAILS),
        (group, raw::behavior::SUBTITLE_PAUSE),
        (group, raw::behavior::SUBTITLE_CURSOR_SHOW),
        (group, raw::behavior::SECONDARY_SUBTITLE_CURSOR_SHOW),
        (group, raw::behavior::FILE_OPEN_DIR),
        (group, raw::behavior::FILE_OPEN_CUSTOM),
    ]
}

pub(crate) fn interface_paths() -> Vec<(&'static str, &'static str)> {
    let group = raw::interface::GROUP;
    vec![
        (group, raw::interface::THEME),
        (group, raw::interface::COMPACT_GLOSSARIES),
        (group, raw::interface::COMPACT_TAGS),
        (group, raw::interface::subtitle::FONT),
        (group, raw::interface::subtitle::LINE_SPACING),
        (group, raw::interface::subtitle::SCALE),
        (group, raw::interface::subtitle::OFFSET),
        (group, raw::interface::subtitle::STROKE),
        (group, raw::interface::subtitle::TEXT_COLOR),
        (group, raw::interface::subtitle::BACKGROUND_COLOR),
        (group, raw::interface::subtitle::STROKE_COLOR),
        (group, raw::interface::subtitle::FURIGANA_RATIO),
        (group, raw::interface::subtitle::ASS_NATIVE),
        (group, raw::interface::POPUP_WIDTH),
        (group, raw::interface::POPUP_HEIGHT),
        (group, raw::interface::SEARCH_EXPRESSION_FONT),
        (group, raw::interface::SEARCH_READING_FONT),
        (group, raw::interface::SEARCH_CONJ_FONT),
        (group, raw::interface::SEARCH_TAG_FONT),
        (group, raw::interface::SEARCH_GLOSSARY_FONT),
        (group, raw::interface::SEARCH_KANJI_FONT),
        (group, raw::interface::subtitle_list::TIMESTAMPS),
        (group, raw::interface::subtitle_list::BACKGROUND_COLOR),
        (group, raw::interface::subtitle_list::TEXT_COLOR),
        (group, raw::interface::subtitle_list::HOVER_COLOR),
        (group, raw::interface::subtitle_list::SELECTED_COLOR),
        (group, raw::interface::subtitle_list::SELECTED_HOVER_COLOR),
        (group, raw::interface::subtitle_list::PRIMARY_FONT),
        (group, raw::interface::subtitle_list::SECONDARY_FONT),
        (
            raw::subtitle_list::GROUP,
            raw::subtitle_list::IGNORE_WHITESPACE,
        ),
        (raw::subtitle_list::GROUP, raw::subtitle_list::AUTO_SEEK),
    ]
}

pub(crate) fn ocr_paths() -> Vec<(&'static str, &'static str)> {
    vec![
        (raw::ocr::GROUP, raw::ocr::ENABLED),
        (raw::ocr::GROUP, raw::ocr::USE_GPU),
        (raw::ocr::GROUP, raw::ocr::MODEL),
    ]
}

pub(crate) fn asr_paths() -> Vec<(&'static str, &'static str)> {
    vec![
        (raw::asr::GROUP, raw::asr::ENABLED),
        (raw::asr::GROUP, raw::asr::BACKEND),
        (raw::asr::GROUP, raw::asr::START_FROM_BEGINNING),
    ]
}

pub(crate) fn whisper_paths() -> Vec<(&'static str, &'static str)> {
    let group = raw::whisper::GROUP;
    vec![
        (group, raw::whisper::ENABLED),
        (group, raw::whisper::MODEL),
        (group, raw::whisper::CUSTOM_MODEL),
        (group, raw::whisper::VAD_ENABLED),
        (group, raw::whisper::VAD_MODEL),
        (group, raw::whisper::USE_GPU),
        (group, raw::whisper::GPU_DEVICE),
        (group, raw::whisper::THREADS),
        (group, raw::whisper::BEST_OF),
        (group, raw::whisper::BEAM_SIZE),
        (group, raw::whisper::FLASH_ATTN),
        (group, raw::whisper::SUPPRESS_REGEX),
    ]
}

pub(crate) fn onnx_paths() -> Vec<(&'static str, &'static str)> {
    let group = raw::onnx::GROUP;
    vec![
        (group, raw::onnx::MODEL),
        (group, raw::onnx::CUSTOM_MODEL),
        (group, raw::onnx::USE_GPU),
        (group, raw::onnx::GPU_DEVICE),
        (group, raw::onnx::THREADS),
        (group, raw::onnx::PUNCTUATION),
    ]
}

pub(crate) fn search_paths() -> Vec<(&'static str, &'static str)> {
    let group = raw::search::GROUP;
    vec![
        (group, raw::search::matcher::EXACT),
        (group, raw::search::matcher::DECONJ),
        (group, raw::search::matcher::MECAB_IPADIC),
        (group, raw::search::MIDDLE_MOUSE_SCAN),
        (group, raw::search::METHOD),
        (group, raw::search::DELAY),
        (group, raw::search::MODIFIER),
        (group, raw::search::HIDE_MPV_SUBS),
        (group, raw::search::HIDE_SUBS),
        (group, raw::search::PAUSE_ON_HOVER),
        (group, raw::search::HOVER_PAUSE_RESUME),
        (group, raw::search::AUTO_PLAY_AUDIO),
        (group, raw::search::GLOSSARY_STYLE),
        (group, raw::search::REPLACE_NEWLINES),
        (group, raw::search::REPLACE_NEWLINES_WITH),
        (group, raw::search::REMOVE_REGEX),
        (group, raw::search::TEXT_REPLACEMENTS),
        (group, raw::search::TEXT_REPLACEMENTS_SEARCH_ORIGINAL),
        (group, raw::search::SUBTITLE_FURIGANA),
        (group, raw::search::SHOW_ANKI_GLOSSARY_CHECKBOX),
        (group, raw::search::MAX_RECURSION_DEPTH),
    ]
}

pub(crate) fn category_snapshot(config: &Config, category: SettingsCategory) -> ConfigSnapshot {
    if category == SettingsCategory::Audio {
        let sources = audio_sources(config);
        let mut values = vec![
            (
                raw::audio::GROUP.to_owned(),
                raw::audio::DOWNLOAD_TIMEOUT_MS.to_owned(),
                Some(effective_value(
                    config,
                    raw::audio::GROUP,
                    raw::audio::DOWNLOAD_TIMEOUT_MS,
                )),
            ),
            (
                raw::audio_source::GROUP.to_owned(),
                "size".to_owned(),
                Some(Value::Int(sources.len() as i64)),
            ),
        ];
        for (index, source) in sources.iter().enumerate() {
            let group = format!("{}/{}", raw::audio_source::GROUP, index + 1);
            values.extend([
                (
                    group.clone(),
                    raw::audio_source::NAME.to_owned(),
                    Some(Value::Text(source.name.clone())),
                ),
                (
                    group.clone(),
                    raw::audio_source::URL.to_owned(),
                    Some(Value::Text(source.url.clone())),
                ),
                (
                    group.clone(),
                    raw::audio_source::TYPE.to_owned(),
                    Some(Value::Int(source.kind)),
                ),
                (
                    group,
                    raw::audio_source::MD5.to_owned(),
                    Some(Value::Text(source.md5.clone())),
                ),
            ]);
        }
        return ConfigSnapshot {
            values,
            replace_prefixes: vec![format!("{}/", raw::audio_source::GROUP)],
        };
    }

    if category == SettingsCategory::Keybind {
        let mut values = vec![(
            raw::keybind::GROUP.to_owned(),
            raw::keybind::CURRENT_PROFILE.to_owned(),
            Some(Value::Text(active_keybind_profile_name(config))),
        )];
        let prefix = keybind_profile_prefix();
        for name in keybind_profile_names(config) {
            let group = format!("{prefix}{name}");
            for (key, _) in KEYBIND_FIELDS {
                values.push((
                    group.clone(),
                    (*key).to_owned(),
                    Some(Value::Text(keybind_value(config, &group, key))),
                ));
            }
        }
        return ConfigSnapshot {
            values,
            replace_prefixes: vec![prefix],
        };
    }

    let paths = category_paths(category);
    let mut values = paths
        .iter()
        .map(|(group, key)| {
            let stored = config.get_at(group, key);
            let fallback = declared_setting_default(config, group, key);
            let value = stored
                .or(fallback)
                .map(|value| normalize_setting_value(group, key, value));
            ((*group).to_owned(), (*key).to_owned(), value)
        })
        .collect::<Vec<_>>();
    if category == SettingsCategory::Asr {
        let group = raw::asr::GROUP;
        let key = raw::asr::BACKEND;
        if !values.iter().any(|(path_group, path_key, value)| {
            path_group == group && path_key == key && value.is_some()
        }) {
            values.push((
                group.to_owned(),
                key.to_owned(),
                Some(Value::Text(
                    crate::controller::asr::default_backend().to_owned(),
                )),
            ));
        }
    }
    ConfigSnapshot {
        values,
        replace_prefixes: Vec::new(),
    }
}

pub(crate) fn restore_category(config: &mut Config, snapshot: &ConfigSnapshot) {
    for prefix in &snapshot.replace_prefixes {
        for group in config.groups_with_prefix(prefix) {
            config.remove_group(&group);
        }
    }
    for (group, key, value) in &snapshot.values {
        if let Some(value) = value {
            let _ = config.put_at(group, key, value);
        } else {
            config.remove_at(group, key);
        }
    }
}

pub(crate) fn parse_config_value(
    group: &str,
    key: &str,
    value: &str,
    current: Value,
) -> Result<Value, String> {
    if group == raw::application::GROUP && key == raw::application::UPDATE_CHANNEL {
        let channel = UpdateChannel::try_from(value).map_err(|()| {
            format!("application/update-channel must be Stable or Nightly (received {value:?})")
        })?;
        return Ok(Value::Text(channel.as_str().to_owned()));
    }
    if group == raw::interface::GROUP && key == raw::interface::THEME {
        let mode = ThemeMode::try_from(value).map_err(|()| {
            format!("interface/theme must be System, Dark, or Light (received {value:?})")
        })?;
        return Ok(Value::Text(mode.as_str().to_owned()));
    }
    if group == raw::search::GROUP && key == raw::search::TEXT_REPLACEMENTS {
        let trimmed = value.trim();
        let canonical = if trimmed.is_empty() { "[]" } else { trimmed };
        let parsed: Vec<TextReplacementRule> = serde_json::from_str(canonical).map_err(|error| {
            format!("search/text-replacements must be a JSON array of replacement rules: {error}")
        })?;
        let serialized = serde_json::to_string(&parsed)
            .map_err(|error| format!("could not encode search/text-replacements: {error}"))?;
        return Ok(Value::Text(serialized));
    }
    if group == raw::asr::GROUP && key == raw::asr::BACKEND {
        return match value {
            "Whisper" | raw::asr::BACKEND_WHISPER => {
                Ok(Value::Text(raw::asr::BACKEND_WHISPER.to_owned()))
            }
            "ONNX Runtime" | raw::asr::BACKEND_ONNX => {
                Ok(Value::Text(raw::asr::BACKEND_ONNX.to_owned()))
            }
            _ => Err(format!("unknown ASR backend {value:?}")),
        };
    }
    if let Some(choice) = integer_choice(group, key, value) {
        return Ok(Value::Int(choice));
    }
    match current {
        Value::Bool(_) => parse_bool(value).map(Value::Bool),
        Value::Int(_) => {
            let number = value
                .parse::<i64>()
                .map_err(|_| format!("{group}/{key} must be an integer"))?;
            let number = integer_range(group, key)
                .map_or(number, |(minimum, maximum)| number.clamp(minimum, maximum));
            Ok(Value::Int(number))
        }
        Value::Real(_) => {
            let number = value
                .parse::<f64>()
                .ok()
                .filter(|number| number.is_finite())
                .ok_or_else(|| format!("{group}/{key} must be a finite number"))?;
            if let Some((minimum, maximum)) = real_range(group, key) {
                if !(minimum..=maximum).contains(&number) {
                    return Err(format!(
                        "{group}/{key} must be between {minimum} and {maximum}"
                    ));
                }
            }
            Ok(Value::Real(number))
        }
        Value::Text(_) => {
            if is_color_key(group, key) {
                let parsed = ArgbColor::parse(value)?;
                Ok(Value::Text(parsed.to_argb_hex()))
            } else if is_font_key(group, key) {
                let _ = FontSpec::parse(value)?;
                Ok(Value::Text(value.trim().to_owned()))
            } else {
                Ok(Value::Text(value.to_owned()))
            }
        }
        Value::Strings(_) => {
            let strings = value
                .split(',')
                .map(str::trim)
                .filter(|part| !part.is_empty())
                .map(str::to_owned)
                .collect();
            Ok(Value::Strings(strings))
        }
        Value::Ints(_) => {
            let mut ints = Vec::new();
            for part in value
                .split(',')
                .map(str::trim)
                .filter(|part| !part.is_empty())
            {
                ints.push(part.parse::<i64>().map_err(|_| {
                    format!("{group}/{key} must be a comma-separated integer list")
                })?);
            }
            Ok(Value::Ints(ints))
        }
        Value::Map(_) => {
            let parsed: BTreeMap<String, String> = serde_json::from_str(value)
                .map_err(|error| format!("{group}/{key} must be a JSON object: {error}"))?;
            Ok(Value::Map(parsed))
        }
    }
}

fn integer_range(group: &str, key: &str) -> Option<(i64, i64)> {
    match (group, key) {
        (raw::application::GROUP, raw::application::LANGUAGE) => Some((
            i64::from(Language::LanguageSystem.as_i32()),
            i64::from(Language::LanguageChineseTraditional.as_i32()),
        )),
        (raw::behavior::GROUP, raw::behavior::AUTO_FIT_PERCENT) => Some((1, 1000)),
        (raw::behavior::GROUP, raw::behavior::OSC_DURATION) => Some((50, 9999)),
        (raw::behavior::GROUP, raw::behavior::OSC_FADE) => Some((0, 9999)),
        (raw::behavior::GROUP, raw::behavior::OSC_MIN_MOVE) => Some((0, i64::MAX)),
        (raw::behavior::GROUP, raw::behavior::FILE_OPEN_DIR) => Some((
            i64::from(Directory::DirectoryCurrent.as_i32()),
            i64::from(Directory::DirectoryCustom.as_i32()),
        )),
        (raw::interface::GROUP, raw::interface::POPUP_WIDTH)
        | (raw::interface::GROUP, raw::interface::POPUP_HEIGHT) => Some((1, 99999)),
        (raw::search::GROUP, raw::search::METHOD) => Some((
            i64::from(SearchMethod::SearchMethodHover.as_i32()),
            i64::from(SearchMethod::SearchMethodModifier.as_i32()),
        )),
        (raw::search::GROUP, raw::search::DELAY) => Some((0, 99999)),
        (raw::search::GROUP, raw::search::MODIFIER) => Some((
            i64::from(Modifier::ModifierAlt.as_i32()),
            i64::from(Modifier::ModifierSuper.as_i32()),
        )),
        (raw::search::GROUP, raw::search::MAX_RECURSION_DEPTH) => Some((0, 10)),
        (raw::search::GROUP, raw::search::GLOSSARY_STYLE) => Some((
            i64::from(GlossaryStyle::GlossaryStyleBullet.as_i32()),
            i64::from(GlossaryStyle::GlossaryStylePipe.as_i32()),
        )),
        (raw::whisper::GROUP, raw::whisper::THREADS) | (raw::onnx::GROUP, raw::onnx::THREADS) => {
            Some((1, asr_thread_limit()))
        }
        (raw::whisper::GROUP, raw::whisper::BEST_OF)
        | (raw::whisper::GROUP, raw::whisper::BEAM_SIZE) => Some((1, 10)),
        (raw::audio::GROUP, raw::audio::DOWNLOAD_TIMEOUT_MS) => Some((
            MIN_AUDIO_DOWNLOAD_TIMEOUT_MS as i64,
            MAX_AUDIO_DOWNLOAD_TIMEOUT_MS as i64,
        )),
        (group, raw::audio_source::TYPE)
            if group.starts_with(&format!("{}/", raw::audio_source::GROUP)) =>
        {
            Some((
                i64::from(AudioSourceType::AudioSourceTypeFile.as_i32()),
                i64::from(AudioSourceType::AudioSourceTypeJson.as_i32()),
            ))
        }
        _ => None,
    }
}

fn real_range(group: &str, key: &str) -> Option<(f64, f64)> {
    match (group, key) {
        (raw::interface::GROUP, raw::interface::subtitle::SCALE) => Some((0.001, 1.0)),
        (raw::interface::GROUP, raw::interface::subtitle::OFFSET)
        | (raw::interface::GROUP, raw::interface::subtitle::FURIGANA_RATIO) => Some((0.0, 1.0)),
        (raw::interface::GROUP, raw::interface::subtitle::STROKE) => Some((0.0, 1000.0)),
        _ => None,
    }
}

fn integer_choice(group: &str, key: &str, value: &str) -> Option<i64> {
    match (group, key) {
        (raw::application::GROUP, raw::application::LANGUAGE) => LANGUAGE_CHOICES
            .iter()
            .find(|(_, label)| *label == value)
            .map(|(variant, _)| i64::from(variant.as_i32())),
        (raw::behavior::GROUP, raw::behavior::FILE_OPEN_DIR) => DIRECTORY_CHOICES
            .iter()
            .find(|(_, label)| *label == value)
            .map(|(variant, _)| i64::from(variant.as_i32())),
        (raw::search::GROUP, raw::search::METHOD) => SEARCH_METHOD_CHOICES
            .iter()
            .find(|(_, label)| *label == value)
            .map(|(variant, _)| i64::from(variant.as_i32())),
        (raw::search::GROUP, raw::search::MODIFIER) => MODIFIER_CHOICES
            .iter()
            .find(|(_, label)| *label == value)
            .map(|(variant, _)| i64::from(variant.as_i32())),
        (raw::search::GROUP, raw::search::GLOSSARY_STYLE) => GLOSSARY_STYLE_CHOICES
            .iter()
            .find(|(_, label)| *label == value)
            .map(|(variant, _)| i64::from(variant.as_i32())),
        (group, raw::audio_source::TYPE)
            if group.starts_with(&format!("{}/", raw::audio_source::GROUP)) =>
        {
            AUDIO_SOURCE_TYPE_CHOICES
                .iter()
                .find(|(_, label)| *label == value)
                .map(|(variant, _)| i64::from(variant.as_i32()))
        }
        _ => None,
    }
}

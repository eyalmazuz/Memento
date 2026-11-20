//! Settings UI field & category group builders for Slint's SettingsWindow.

use std::collections::HashMap;
use std::rc::Rc;

use memento_config::config::Config;
use memento_config::{ArgbColor, QtFontSpec};
use memento_setting::enums::Directory;
use memento_setting::keys as raw;
use memento_setting::models::{ONNX_MODELS, WHISPER_MODELS};
use slint::{ModelRc, SharedString, VecModel};

use crate::ui::{SettingsField, SettingsGroup};

use super::audio_sources::audio_sources;
use super::keybinds::{
    active_keybind_profile_name, compute_keybind_conflicts, keybind_profile_group,
    keybind_profile_names, keybind_value, visible_keybind_fields,
};
use super::schema::{
    asr_thread_limit, audio_source_type_label, directory_label, effective_bool, effective_i64,
    effective_text, effective_value, glossary_style_label, language_label, modifier_label,
    search_method_label, theme_label, value_text, SettingsCategory, AUDIO_SOURCE_TYPE_CHOICES,
    DIRECTORY_CHOICES, GLOSSARY_STYLE_CHOICES, LANGUAGE_CHOICES, MODIFIER_CHOICES,
    SEARCH_METHOD_CHOICES, THEME_CHOICES,
};

fn strings_model(values: &[String]) -> ModelRc<SharedString> {
    Rc::new(VecModel::from(
        values
            .iter()
            .map(|value| SharedString::from(value.as_str()))
            .collect::<Vec<_>>(),
    ))
    .into()
}

pub(crate) fn field(
    key: &str,
    label: &str,
    kind: &str,
    value: String,
    choices: &[&str],
    enabled: bool,
    visible: bool,
) -> SettingsField {
    SettingsField {
        key: key.into(),
        label: label.into(),
        description: SharedString::default(),
        kind: kind.into(),
        value: value.into(),
        choices: strings_model(
            &choices
                .iter()
                .map(|choice| (*choice).to_owned())
                .collect::<Vec<_>>(),
        ),
        enabled,
        visible,
        font_family: SharedString::default(),
        font_size: SharedString::default(),
        font_bold: false,
        font_italic: false,
        color_swatch: slint::Color::from_argb_u8(255, 255, 255, 255),
        color_a: 255,
        color_r: 255,
        color_g: 255,
        color_b: 255,
        conflict: SharedString::default(),
    }
}

pub(crate) fn font_field(key: &str, label: &str, value: String) -> SettingsField {
    let spec = QtFontSpec::parse(&value)
        .unwrap_or_else(|_| QtFontSpec::new("Noto Sans CJK JP", 12.0, false, false));
    let mut item = field(key, label, "font", value, &[], true, true);
    item.font_family = spec.family.clone().into();
    item.font_size = spec.formatted_size().into();
    item.font_bold = spec.bold;
    item.font_italic = spec.italic;
    item
}

pub(crate) fn color_field(key: &str, label: &str, value: String) -> SettingsField {
    let argb = ArgbColor::parse(&value).unwrap_or_else(|_| ArgbColor::new(255, 255, 255, 255));
    let mut item = field(key, label, "color", value, &[], true, true);
    item.color_swatch = slint::Color::from_argb_u8(argb.a, argb.r, argb.g, argb.b);
    item.color_a = i32::from(argb.a);
    item.color_r = i32::from(argb.r);
    item.color_g = i32::from(argb.g);
    item.color_b = i32::from(argb.b);
    item
}

pub(crate) fn keybind_field(
    key: &str,
    label: &str,
    value: String,
    conflict: String,
) -> SettingsField {
    let mut item = field(key, label, "keybind", value, &[], true, true);
    item.conflict = conflict.into();
    item
}

pub(crate) fn text_field(key: &str, label: &str, value: String) -> SettingsField {
    field(key, label, "text", value, &[], true, true)
}

pub(crate) fn conditional_text_field(
    key: &str,
    label: &str,
    value: String,
    enabled: bool,
    visible: bool,
) -> SettingsField {
    field(key, label, "text", value, &[], enabled, visible)
}

pub(crate) fn switch_field(key: &str, label: &str, value: bool) -> SettingsField {
    field(key, label, "switch", value.to_string(), &[], true, true)
}

pub(crate) fn conditional_switch_field(
    key: &str,
    label: &str,
    value: bool,
    enabled: bool,
    visible: bool,
) -> SettingsField {
    field(
        key,
        label,
        "switch",
        value.to_string(),
        &[],
        enabled,
        visible,
    )
}

pub(crate) fn with_description(mut field: SettingsField, description: &str) -> SettingsField {
    field.description = description.into();
    field
}

pub(crate) fn choice_field(
    key: &str,
    label: &str,
    value: String,
    choices: &[&str],
) -> SettingsField {
    field(key, label, "choice", value, choices, true, true)
}

pub(crate) fn conditional_choice_field(
    key: &str,
    label: &str,
    value: String,
    choices: &[&str],
    enabled: bool,
    visible: bool,
) -> SettingsField {
    field(key, label, "choice", value, choices, enabled, visible)
}

pub(crate) fn group(
    category: SettingsCategory,
    title: &str,
    entry_id: &str,
    fields: Vec<SettingsField>,
) -> SettingsGroup {
    SettingsGroup {
        category: category.as_i32(),
        title: title.into(),
        entry_id: entry_id.into(),
        fields: Rc::new(VecModel::from(fields)).into(),
    }
}

fn choice_labels<T>(table: &[(T, &'static str)]) -> Vec<&'static str> {
    table.iter().map(|(_, label)| *label).collect()
}

pub(crate) fn application_settings_groups(config: &Config) -> Vec<SettingsGroup> {
    let language = effective_i64(config, raw::application::GROUP, raw::application::LANGUAGE);
    let labels = choice_labels(LANGUAGE_CHOICES);
    vec![
        group(
            SettingsCategory::Application,
            "Language",
            "",
            vec![with_description(
                choice_field(
                    "application/language",
                    "Interface language (restart required)",
                    language_label(language).to_owned(),
                    &labels,
                ),
                "Saved for config compatibility. Only English UI translations are currently bundled in this Slint build.",
            )],
        ),
        group(
            SettingsCategory::Application,
            "Update",
            "",
            vec![switch_field(
                "application/auto-update-check",
                "Check for updates on launch",
                effective_bool(
                    config,
                    raw::application::GROUP,
                    raw::application::AUTO_UPDATE_CHECK,
                ),
            )],
        ),
    ]
}

pub(crate) fn audio_settings_groups(config: &Config) -> Vec<SettingsGroup> {
    let type_labels = choice_labels(AUDIO_SOURCE_TYPE_CHOICES);
    let mut groups = vec![group(
        SettingsCategory::Audio,
        "General",
        "",
        vec![with_description(
            text_field(
                "audio/download-timeout-ms",
                "Download timeout (milliseconds)",
                value_text(&effective_value(
                    config,
                    raw::audio::GROUP,
                    raw::audio::DOWNLOAD_TIMEOUT_MS,
                )),
            ),
            "Per-source network timeout for external dictionary pronunciation audio downloads (1000–30000 ms).",
        )],
    )];
    groups.extend(
        audio_sources(config)
            .into_iter()
            .enumerate()
            .map(|(index, source)| {
                let entry_id = (index + 1).to_string();
                let source_group = format!("{}/{}", raw::audio_source::GROUP, entry_id);
                group(
                    SettingsCategory::Audio,
                    &format!("Source {}", index + 1),
                    &entry_id,
                    vec![
                        text_field(
                            &format!("{source_group}/{}", raw::audio_source::NAME),
                            "Source name",
                            source.name,
                        ),
                        text_field(
                            &format!("{source_group}/{}", raw::audio_source::URL),
                            "URL ({expression}, {reading})",
                            source.url,
                        ),
                        choice_field(
                            &format!("{source_group}/{}", raw::audio_source::TYPE),
                            "Source type",
                            audio_source_type_label(source.kind).to_owned(),
                            &type_labels,
                        ),
                        text_field(
                            &format!("{source_group}/{}", raw::audio_source::MD5),
                            "MD5 skip hash",
                            source.md5,
                        ),
                    ],
                )
            }),
    );
    groups
}

pub(crate) fn behavior_settings_groups(config: &Config) -> Vec<SettingsGroup> {
    let group_name = raw::behavior::GROUP;
    let dir = effective_i64(config, group_name, raw::behavior::FILE_OPEN_DIR);
    let dir_labels = choice_labels(DIRECTORY_CHOICES);
    vec![
        group(
            SettingsCategory::Behavior,
            "Player",
            "",
            vec![
                switch_field(
                    "behavior/autofit",
                    "Auto fit media on load",
                    effective_bool(config, group_name, raw::behavior::AUTO_FIT),
                ),
                text_field(
                    "behavior/autofit-percent",
                    "Auto fit media percentage",
                    value_text(&effective_value(
                        config,
                        group_name,
                        raw::behavior::AUTO_FIT_PERCENT,
                    )),
                ),
            ],
        ),
        group(
            SettingsCategory::Behavior,
            "On Screen Controller",
            "",
            vec![
                text_field(
                    "behavior/osc-duration",
                    "Visibility duration milliseconds",
                    value_text(&effective_value(
                        config,
                        group_name,
                        raw::behavior::OSC_DURATION,
                    )),
                ),
                text_field(
                    "behavior/osc-fade",
                    "Fade duration milliseconds",
                    value_text(&effective_value(
                        config,
                        group_name,
                        raw::behavior::OSC_FADE,
                    )),
                ),
                text_field(
                    "behavior/osc-min-move",
                    "Minimum mouse pixel movement",
                    value_text(&effective_value(
                        config,
                        group_name,
                        raw::behavior::OSC_MIN_MOVE,
                    )),
                ),
                switch_field(
                    "behavior/osc-mpv-cursor",
                    "Allow mpv to control cursor visibility",
                    effective_bool(config, group_name, raw::behavior::OSC_MPV_CURSOR),
                ),
                switch_field(
                    "behavior/osc-preview-thumbnails",
                    "Show preview thumbnails",
                    effective_bool(config, group_name, raw::behavior::OSC_PREVIEW_THUMBNAILS),
                ),
            ],
        ),
        group(
            SettingsCategory::Behavior,
            "Subtitles",
            "",
            vec![
                switch_field(
                    "behavior/subtitle-pause",
                    "Automatically pause at the end of subtitles",
                    effective_bool(config, group_name, raw::behavior::SUBTITLE_PAUSE),
                ),
                switch_field(
                    "behavior/subtitle-cursor-show",
                    "Show subtitles when the cursor is at the bottom of the player",
                    effective_bool(config, group_name, raw::behavior::SUBTITLE_CURSOR_SHOW),
                ),
                switch_field(
                    "behavior/subtitle-cursor-show-secondary",
                    "Show secondary subtitles when the cursor is at the top of the player",
                    effective_bool(
                        config,
                        group_name,
                        raw::behavior::SECONDARY_SUBTITLE_CURSOR_SHOW,
                    ),
                ),
            ],
        ),
        group(
            SettingsCategory::Behavior,
            "Open File Directory",
            "",
            vec![
                choice_field(
                    "behavior/file-open-dir",
                    "Open file starting location",
                    directory_label(dir).to_owned(),
                    &dir_labels,
                ),
                with_description(
                    conditional_text_field(
                        "behavior/file-open-custom",
                        "Custom open folder URL",
                        value_text(&effective_value(
                            config,
                            group_name,
                            raw::behavior::FILE_OPEN_CUSTOM,
                        )),
                        dir == i64::from(Directory::DirectoryCustom.as_i32()),
                        true,
                    ),
                    "Folder path or file:// URL used when starting location is set to Custom (supports native folder browse).",
                ),
            ],
        ),
    ]
}

pub(crate) fn dictionary_settings_groups(config: &Config) -> Vec<SettingsGroup> {
    let order = value_text(&effective_value(
        config,
        raw::dictionary::GROUP,
        raw::dictionary::ORDER,
    ));
    vec![group(
        SettingsCategory::Dictionary,
        "Dictionary order",
        "",
        vec![text_field(
            "dictionary/order",
            "Dictionary IDs in priority order",
            order,
        )],
    )]
}

pub(crate) fn interface_settings_groups(config: &Config) -> Vec<SettingsGroup> {
    let group_name = raw::interface::GROUP;
    let theme_labels = choice_labels(THEME_CHOICES);
    let theme_value = value_text(&effective_value(config, group_name, raw::interface::THEME));
    vec![
        group(
            SettingsCategory::Interface,
            "Theme",
            "",
            vec![with_description(
                choice_field(
                    "interface/theme",
                    "Color theme",
                    theme_label(&theme_value).to_owned(),
                    &theme_labels,
                ),
                "Follow system dark/light preference or force Dark or Light appearance.",
            )],
        ),
        group(
            SettingsCategory::Interface,
            "Subtitle",
            "",
            vec![
                font_field(
                    "interface/sub-font",
                    "Font",
                    value_text(&effective_value(
                        config,
                        group_name,
                        raw::interface::subtitle::FONT,
                    )),
                ),
                text_field(
                    "interface/sub-line-spacing",
                    "Line spacing (pixels)",
                    value_text(&effective_value(
                        config,
                        group_name,
                        raw::interface::subtitle::LINE_SPACING,
                    )),
                ),
                text_field(
                    "interface/sub-scale",
                    "Line height as a percentage of window height",
                    value_text(&effective_value(
                        config,
                        group_name,
                        raw::interface::subtitle::SCALE,
                    )),
                ),
                text_field(
                    "interface/sub-offset",
                    "Bottom offset as a percentage of window height",
                    value_text(&effective_value(
                        config,
                        group_name,
                        raw::interface::subtitle::OFFSET,
                    )),
                ),
                text_field(
                    "interface/sub-stroke",
                    "Stroke size",
                    value_text(&effective_value(
                        config,
                        group_name,
                        raw::interface::subtitle::STROKE,
                    )),
                ),
                color_field(
                    "interface/sub-text-color",
                    "Text color",
                    value_text(&effective_value(
                        config,
                        group_name,
                        raw::interface::subtitle::TEXT_COLOR,
                    )),
                ),
                color_field(
                    "interface/sub-bg-color",
                    "Background color",
                    value_text(&effective_value(
                        config,
                        group_name,
                        raw::interface::subtitle::BACKGROUND_COLOR,
                    )),
                ),
                color_field(
                    "interface/sub-stroke-color",
                    "Stroke color",
                    value_text(&effective_value(
                        config,
                        group_name,
                        raw::interface::subtitle::STROKE_COLOR,
                    )),
                ),
                text_field(
                    "interface/sub-furigana-ratio",
                    "Furigana ratio",
                    value_text(&effective_value(
                        config,
                        group_name,
                        raw::interface::subtitle::FURIGANA_RATIO,
                    )),
                ),
                with_description(
                    switch_field(
                        "interface/sub-ass-native",
                        "Render ASS/SSA subtitles natively (mpv/libass)",
                        effective_bool(config, group_name, raw::interface::subtitle::ASS_NATIVE),
                    ),
                    "Preserve full ASS positioning, styling, typesetting, and embedded fonts via libmpv/libass with interactive dictionary lookup and highlighting. Disable to fall back to Memento's plain subtitle bar.",
                ),
            ],
        ),
        group(
            SettingsCategory::Interface,
            "Pop-up",
            "",
            vec![
                switch_field(
                    "interface/compact-glossaries",
                    "Compact glossaries",
                    effective_bool(config, group_name, raw::interface::COMPACT_GLOSSARIES),
                ),
                switch_field(
                    "interface/compact-tags",
                    "Compact tags",
                    effective_bool(config, group_name, raw::interface::COMPACT_TAGS),
                ),
                text_field(
                    "interface/sub-popup-width",
                    "Subtitle pop-up width",
                    value_text(&effective_value(
                        config,
                        group_name,
                        raw::interface::POPUP_WIDTH,
                    )),
                ),
                text_field(
                    "interface/sub-popup-height",
                    "Subtitle pop-up height",
                    value_text(&effective_value(
                        config,
                        group_name,
                        raw::interface::POPUP_HEIGHT,
                    )),
                ),
                font_field(
                    "interface/search-expression-font",
                    "Expression font",
                    value_text(&effective_value(
                        config,
                        group_name,
                        raw::interface::SEARCH_EXPRESSION_FONT,
                    )),
                ),
                font_field(
                    "interface/search-reading-font",
                    "Reading font",
                    value_text(&effective_value(
                        config,
                        group_name,
                        raw::interface::SEARCH_READING_FONT,
                    )),
                ),
                font_field(
                    "interface/search-conj-font",
                    "Conjugation font",
                    value_text(&effective_value(
                        config,
                        group_name,
                        raw::interface::SEARCH_CONJ_FONT,
                    )),
                ),
                font_field(
                    "interface/search-tag-font",
                    "Tag font",
                    value_text(&effective_value(
                        config,
                        group_name,
                        raw::interface::SEARCH_TAG_FONT,
                    )),
                ),
                font_field(
                    "interface/search-glossary-font",
                    "Glossary font",
                    value_text(&effective_value(
                        config,
                        group_name,
                        raw::interface::SEARCH_GLOSSARY_FONT,
                    )),
                ),
                font_field(
                    "interface/search-kanji-font",
                    "Kanji font",
                    value_text(&effective_value(
                        config,
                        group_name,
                        raw::interface::SEARCH_KANJI_FONT,
                    )),
                ),
            ],
        ),
        group(
            SettingsCategory::Interface,
            "Subtitle List",
            "",
            vec![
                switch_field(
                    "interface/sub-list-timestamps",
                    "Show timestamps",
                    effective_bool(
                        config,
                        group_name,
                        raw::interface::subtitle_list::TIMESTAMPS,
                    ),
                ),
                switch_field(
                    "subtitle-list/ignore-whitespace",
                    "Ignore whitespace when matching subtitles",
                    effective_bool(
                        config,
                        raw::subtitle_list::GROUP,
                        raw::subtitle_list::IGNORE_WHITESPACE,
                    ),
                ),
                with_description(
                    switch_field(
                        "subtitle-list/auto-seek",
                        "Auto-seek to matched subtitle",
                        effective_bool(
                            config,
                            raw::subtitle_list::GROUP,
                            raw::subtitle_list::AUTO_SEEK,
                        ),
                    ),
                    "Automatically seek playback to the matched cue when searching in the subtitle list.",
                ),
                color_field(
                    "interface/sub-list-background-color",
                    "Background color",
                    value_text(&effective_value(
                        config,
                        group_name,
                        raw::interface::subtitle_list::BACKGROUND_COLOR,
                    )),
                ),
                color_field(
                    "interface/sub-list-text-color",
                    "Text color",
                    value_text(&effective_value(
                        config,
                        group_name,
                        raw::interface::subtitle_list::TEXT_COLOR,
                    )),
                ),
                color_field(
                    "interface/sub-list-hover-color",
                    "Hover color",
                    value_text(&effective_value(
                        config,
                        group_name,
                        raw::interface::subtitle_list::HOVER_COLOR,
                    )),
                ),
                color_field(
                    "interface/sub-list-selected-color",
                    "Selected color",
                    value_text(&effective_value(
                        config,
                        group_name,
                        raw::interface::subtitle_list::SELECTED_COLOR,
                    )),
                ),
                color_field(
                    "interface/sub-list-selected-hover-color",
                    "Selected hover color",
                    value_text(&effective_value(
                        config,
                        group_name,
                        raw::interface::subtitle_list::SELECTED_HOVER_COLOR,
                    )),
                ),
                font_field(
                    "interface/sub-list-primary-font",
                    "Primary font",
                    value_text(&effective_value(
                        config,
                        group_name,
                        raw::interface::subtitle_list::PRIMARY_FONT,
                    )),
                ),
                font_field(
                    "interface/sub-list-secondary-font",
                    "Secondary font",
                    value_text(&effective_value(
                        config,
                        group_name,
                        raw::interface::subtitle_list::SECONDARY_FONT,
                    )),
                ),
            ],
        ),
    ]
}

pub(crate) fn keybind_settings_groups(config: &Config) -> Vec<SettingsGroup> {
    let names = keybind_profile_names(config);
    let current = active_keybind_profile_name(config);
    let profile_group = keybind_profile_group(&current);
    let conflicts: HashMap<String, String> = compute_keybind_conflicts(config)
        .into_iter()
        .map(|(key, _, others)| (key, format!("Conflicts with: {}", others.join(", "))))
        .collect();
    let choice_refs: Vec<&str> = names.iter().map(String::as_str).collect();
    vec![
        group(
            SettingsCategory::Keybind,
            "Profile",
            "",
            vec![with_description(
                choice_field("keybind/current", "Current profile", current, &choice_refs),
                "Use the header add/delete icons to create or remove keybind profiles.",
            )],
        ),
        group(
            SettingsCategory::Keybind,
            "Keyboard Shortcuts",
            "",
            visible_keybind_fields()
                .into_iter()
                .map(|(key, label)| {
                    let conflict = conflicts.get(key).cloned().unwrap_or_default();
                    keybind_field(
                        &format!("{profile_group}/{key}"),
                        label,
                        keybind_value(config, &profile_group, key),
                        conflict,
                    )
                })
                .collect(),
        ),
    ]
}

pub(crate) fn ocr_settings_groups(config: &Config) -> Vec<SettingsGroup> {
    vec![group(
        SettingsCategory::Ocr,
        "OCR",
        "",
        vec![
            switch_field(
                "ocr/enable",
                "Enabled",
                effective_bool(config, raw::ocr::GROUP, raw::ocr::ENABLED),
            ),
            switch_field(
                "ocr/use-gpu",
                "Use GPU acceleration",
                effective_bool(config, raw::ocr::GROUP, raw::ocr::USE_GPU),
            ),
            text_field(
                "ocr/model",
                "Hugging Face model",
                value_text(&effective_value(config, raw::ocr::GROUP, raw::ocr::MODEL)),
            ),
        ],
    )]
}

pub(crate) fn asr_settings_groups(config: &Config) -> Vec<SettingsGroup> {
    let backend = effective_text(config, raw::asr::GROUP, raw::asr::BACKEND)
        .filter(|name| crate::controller::asr::backend_available(name))
        .unwrap_or_else(|| crate::controller::asr::default_backend().to_owned());
    let mut backend_choices = Vec::new();
    if cfg!(feature = "asr-whisper") {
        backend_choices.push("Whisper");
    }
    if cfg!(feature = "asr-onnx") {
        backend_choices.push("ONNX Runtime");
    }
    let whisper_selected = backend == raw::asr::BACKEND_WHISPER;
    let onnx_selected = backend == raw::asr::BACKEND_ONNX;
    let whisper_model = value_text(&effective_value(
        config,
        raw::whisper::GROUP,
        raw::whisper::MODEL,
    ));
    let onnx_model = value_text(&effective_value(config, raw::onnx::GROUP, raw::onnx::MODEL));
    let vad_enabled = effective_bool(config, raw::whisper::GROUP, raw::whisper::VAD_ENABLED);

    let mut whisper_model_choices: Vec<&str> = WHISPER_MODELS.to_vec();
    if !whisper_model_choices.contains(&whisper_model.as_str()) {
        whisper_model_choices.push(whisper_model.as_str());
    }
    let mut onnx_model_choices: Vec<&str> = ONNX_MODELS.to_vec();
    if !onnx_model_choices.contains(&onnx_model.as_str()) {
        onnx_model_choices.push(onnx_model.as_str());
    }

    let thread_max = asr_thread_limit() as usize;
    let thread_options = (1..=thread_max)
        .map(|count| count.to_string())
        .collect::<Vec<_>>();
    let thread_choices = thread_options
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    let max_threads = thread_max as i64;
    let selected_threads = |group_name, key| {
        let requested = effective_i64(config, group_name, key);
        if requested <= 0 {
            4.min(max_threads)
        } else {
            requested.clamp(1, max_threads)
        }
        .to_string()
    };
    let numeric_choices = ["1", "2", "3", "4", "5", "6", "7", "8", "9", "10"];
    let selected_numeric = |group_name, key| {
        effective_i64(config, group_name, key)
            .clamp(1, 10)
            .to_string()
    };

    let mut groups = vec![group(
        SettingsCategory::Asr,
        "ASR",
        "",
        vec![
            choice_field(
                "asr/backend",
                "Backend",
                if backend == raw::asr::BACKEND_ONNX {
                    "ONNX Runtime".to_owned()
                } else {
                    "Whisper".to_owned()
                },
                &backend_choices,
            ),
            switch_field(
                "asr/enable",
                "Enabled",
                effective_bool(config, raw::asr::GROUP, raw::asr::ENABLED),
            ),
            switch_field(
                "asr/start-from-beginning",
                "Always transcribe from start of media",
                effective_bool(config, raw::asr::GROUP, raw::asr::START_FROM_BEGINNING),
            ),
        ],
    )];

    if whisper_selected {
        groups.push(group(
            SettingsCategory::Asr,
            "Whisper",
            "",
            vec![
                conditional_switch_field(
                    "whisper/use-gpu",
                    "Use GPU",
                    effective_bool(config, raw::whisper::GROUP, raw::whisper::USE_GPU),
                    whisper_selected,
                    whisper_selected,
                ),
                conditional_choice_field(
                    "whisper/model",
                    "Model",
                    whisper_model.clone(),
                    &whisper_model_choices,
                    whisper_selected,
                    whisper_selected,
                ),
                conditional_text_field(
                    "whisper/custom-model",
                    "Custom model path",
                    value_text(&effective_value(
                        config,
                        raw::whisper::GROUP,
                        raw::whisper::CUSTOM_MODEL,
                    )),
                    whisper_selected && whisper_model == "custom",
                    whisper_selected && whisper_model == "custom",
                ),
                conditional_switch_field(
                    "whisper/vad-enable",
                    "Voice activity detection",
                    vad_enabled,
                    whisper_selected,
                    whisper_selected,
                ),
                conditional_text_field(
                    "whisper/vad-model",
                    "VAD model path",
                    value_text(&effective_value(
                        config,
                        raw::whisper::GROUP,
                        raw::whisper::VAD_MODEL,
                    )),
                    whisper_selected && vad_enabled,
                    whisper_selected && vad_enabled,
                ),
                conditional_choice_field(
                    "whisper/threads",
                    "Threads",
                    selected_threads(raw::whisper::GROUP, raw::whisper::THREADS),
                    &thread_choices,
                    whisper_selected,
                    whisper_selected,
                ),
                conditional_choice_field(
                    "whisper/best-of",
                    "Best of",
                    selected_numeric(raw::whisper::GROUP, raw::whisper::BEST_OF),
                    &numeric_choices,
                    whisper_selected,
                    whisper_selected,
                ),
                conditional_choice_field(
                    "whisper/beam-size",
                    "Beam search size",
                    selected_numeric(raw::whisper::GROUP, raw::whisper::BEAM_SIZE),
                    &numeric_choices,
                    whisper_selected,
                    whisper_selected,
                ),
                conditional_switch_field(
                    "whisper/flash-attn",
                    "Flash attention",
                    effective_bool(config, raw::whisper::GROUP, raw::whisper::FLASH_ATTN),
                    whisper_selected,
                    whisper_selected,
                ),
                conditional_text_field(
                    "whisper/suppress-regex",
                    "Suppress regex",
                    value_text(&effective_value(
                        config,
                        raw::whisper::GROUP,
                        raw::whisper::SUPPRESS_REGEX,
                    )),
                    whisper_selected,
                    whisper_selected,
                ),
            ],
        ));
    }

    if onnx_selected {
        groups.push(group(
            SettingsCategory::Asr,
            "ONNX Runtime",
            "",
            vec![
                conditional_switch_field(
                    "onnx/use-gpu",
                    "Use GPU",
                    effective_bool(config, raw::onnx::GROUP, raw::onnx::USE_GPU),
                    onnx_selected,
                    onnx_selected,
                ),
                conditional_choice_field(
                    "onnx/model",
                    "Model",
                    onnx_model.clone(),
                    &onnx_model_choices,
                    onnx_selected,
                    onnx_selected,
                ),
                conditional_text_field(
                    "onnx/custom-model",
                    "Custom model directory",
                    value_text(&effective_value(
                        config,
                        raw::onnx::GROUP,
                        raw::onnx::CUSTOM_MODEL,
                    )),
                    onnx_selected && onnx_model == "custom",
                    onnx_selected && onnx_model == "custom",
                ),
                conditional_choice_field(
                    "onnx/threads",
                    "Threads",
                    selected_threads(raw::onnx::GROUP, raw::onnx::THREADS),
                    &thread_choices,
                    onnx_selected,
                    onnx_selected,
                ),
                conditional_switch_field(
                    "onnx/punctuation",
                    "Punctuation",
                    effective_bool(config, raw::onnx::GROUP, raw::onnx::PUNCTUATION),
                    onnx_selected,
                    onnx_selected,
                ),
            ],
        ));
    }

    groups
}

pub(crate) fn search_settings_groups(config: &Config) -> Vec<SettingsGroup> {
    let group_name = raw::search::GROUP;
    let method = effective_i64(config, group_name, raw::search::METHOD);
    let modifier = effective_i64(config, group_name, raw::search::MODIFIER);
    let glossary_style = effective_i64(config, group_name, raw::search::GLOSSARY_STYLE);
    let method_labels = choice_labels(SEARCH_METHOD_CHOICES);
    let modifier_labels = choice_labels(MODIFIER_CHOICES);
    let glossary_labels = choice_labels(GLOSSARY_STYLE_CHOICES);
    vec![
        group(
            SettingsCategory::Search,
            "Matcher",
            "",
            vec![
                switch_field(
                    "search/exact-matcher",
                    "Exact",
                    effective_bool(config, group_name, raw::search::matcher::EXACT),
                ),
                switch_field(
                    "search/deconj-matcher",
                    "Deconjugation",
                    effective_bool(config, group_name, raw::search::matcher::DECONJ),
                ),
                switch_field(
                    "search/ipadic-matcher",
                    "MeCab IPAdic",
                    effective_bool(config, group_name, raw::search::matcher::MECAB_IPADIC),
                ),
            ],
        ),
        group(
            SettingsCategory::Search,
            "Pop-up",
            "",
            vec![
                choice_field(
                    "search/method",
                    "Search method",
                    search_method_label(method).to_owned(),
                    &method_labels,
                ),
                text_field(
                    "search/delay",
                    "Search hover millisecond delay",
                    value_text(&effective_value(config, group_name, raw::search::DELAY)),
                ),
                choice_field(
                    "search/modifier",
                    "Search modifier key",
                    modifier_label(modifier).to_owned(),
                    &modifier_labels,
                ),
                with_description(
                    text_field(
                        "search/max-recursion-depth",
                        "Maximum child popups (recursive search)",
                        value_text(&effective_value(
                            config,
                            group_name,
                            raw::search::MAX_RECURSION_DEPTH,
                        )),
                    ),
                    "Max depth of nested popups when scanning inside definitions (0 disables)",
                ),
                switch_field(
                    "search/middle-mouse-scan",
                    "Search using middle mouse button",
                    effective_bool(config, group_name, raw::search::MIDDLE_MOUSE_SCAN),
                ),
                choice_field(
                    "search/list-result",
                    "Glossary style",
                    glossary_style_label(glossary_style).to_owned(),
                    &glossary_labels,
                ),
                switch_field(
                    "search/auto-play-audio",
                    "Auto play first result audio",
                    effective_bool(config, group_name, raw::search::AUTO_PLAY_AUDIO),
                ),
                switch_field(
                    "search/show-anki-glossary-checkbox",
                    "Show add to Anki glossary checkbox",
                    effective_bool(config, group_name, raw::search::SHOW_ANKI_GLOSSARY_CHECKBOX),
                ),
            ],
        ),
        group(
            SettingsCategory::Search,
            "Subtitles",
            "",
            vec![
                switch_field(
                    "search/hide-subs",
                    "Hide mpv subtitles while Memento subtitles are visible",
                    effective_bool(config, group_name, raw::search::HIDE_MPV_SUBS),
                ),
                switch_field(
                    "search/hide-bar",
                    "Hide Memento subtitles while playing media",
                    effective_bool(config, group_name, raw::search::HIDE_SUBS),
                ),
                switch_field(
                    "search/hover-pause",
                    "Pause when hovering on subtitles",
                    effective_bool(config, group_name, raw::search::PAUSE_ON_HOVER),
                ),
                switch_field(
                    "search/hover-pause-resume",
                    "Resume playback when leaving subtitles after hover-pause",
                    effective_bool(config, group_name, raw::search::HOVER_PAUSE_RESUME),
                ),
                switch_field(
                    "search/subtitle-furigana",
                    "Show furigana over subtitles (inaccurate)",
                    effective_bool(config, group_name, raw::search::SUBTITLE_FURIGANA),
                ),
                switch_field(
                    "search/replace-lines",
                    "Replace new lines in subtitles",
                    effective_bool(config, group_name, raw::search::REPLACE_NEWLINES),
                ),
                text_field(
                    "search/replace-with",
                    "New line replacement text",
                    value_text(&effective_value(
                        config,
                        group_name,
                        raw::search::REPLACE_NEWLINES_WITH,
                    )),
                ),
                text_field(
                    "search/remove-regex",
                    "Remove subtitle text matching regex",
                    value_text(&effective_value(
                        config,
                        group_name,
                        raw::search::REMOVE_REGEX,
                    )),
                ),
            ],
        ),
        group(
            SettingsCategory::Search,
            "Text Replacements",
            "",
            vec![
                with_description(
                    text_field(
                        "search/text-replacements",
                        "Custom text replacement rules (JSON)",
                        value_text(&effective_value(
                            config,
                            group_name,
                            raw::search::TEXT_REPLACEMENTS,
                        )),
                    ),
                    "JSON array of {\"pattern\", \"replacement\", \"ignore_case\", \"enabled\"} regex rules applied before dictionary lookup.",
                ),
                switch_field(
                    "search/text-replacements-search-original",
                    "Also search original unmodified text",
                    effective_bool(
                        config,
                        group_name,
                        raw::search::TEXT_REPLACEMENTS_SEARCH_ORIGINAL,
                    ),
                ),
            ],
        ),
    ]
}

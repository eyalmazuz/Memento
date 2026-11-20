//! Settings window session, in-place field updates, quick search, draft revert-on-close,
//! theme sync, live subtitle preview, text replacements, profile QoL, and category callbacks.

mod anki;
mod asr_models;
mod custom_css;
mod dictionaries;
mod fonts;

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use memento_config::config::Config;
use memento_config::{ArgbColor, QtFontSpec, Value};
use memento_dict::normalize::Regex;
use memento_dict::searchcontroller::TextReplacementRule;
use memento_os::{
    decode_file_uri, detect_system_color_scheme, open_files_on_main_thread,
    open_folder_on_main_thread, FileDialogKind, FileDialogOptions, SystemColorScheme,
};
use memento_player::MpvInstance;
use memento_setting::enums::ThemeMode;
use memento_setting::keys as raw;
use slint::winit_030::winit::event::WindowEvent;
use slint::winit_030::winit::window::Theme as WinitTheme;
use slint::winit_030::{EventResult, WinitWindowAccessor};
use slint::{ComponentHandle, Model as _, SharedString, VecModel};

use crate::controller;
use crate::controller::config::{
    format_browse_selection, serialize_excluded_glossaries, setting_browse_metadata,
    validate_profile_name, BrowseKind, ConfigController,
};
use crate::controller::dict::DictController;
use crate::controller::dictmanager::DictionaryManager;
use crate::controller::player::{refresh_subtitle_list, LivePlayerCoordinator};
use crate::controller::subtitles::SubtitleController;
use crate::ui::{SettingsField, SettingsGroup, TextReplacementRow};
use crate::{
    apply_dictionary_order, memento_xdg_dir, refresh_player_preferences, setting_enabled,
    AnkiHelpWindow, MementoApp, SettingsWindow, SubEntry,
};

pub(crate) use asr_models::clear_asr_for_settings;
pub(crate) use custom_css::refresh_custom_popup_css;
pub(crate) use dictionaries::refresh_dictionary_settings;

thread_local! {
    /// Cached OS color scheme so repeated refreshes avoid redundant portal/CLI queries
    /// while `WindowEvent::ThemeChanged` updates it live.
    static CACHED_SYSTEM_SCHEME: Cell<Option<SystemColorScheme>> = const { Cell::new(None) };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(i32)]
pub(crate) enum SettingsCategory {
    Anki = 0,
    Application = 1,
    AudioSources = 2,
    Behavior = 3,
    Dictionaries = 4,
    Interface = 5,
    Keybinds = 6,
    Ocr = 7,
    Asr = 8,
    Search = 9,
}

impl SettingsCategory {
    pub(crate) const COUNT: usize = 10;

    pub(crate) const fn as_i32(self) -> i32 {
        self as i32
    }

    pub(crate) const fn as_usize(self) -> usize {
        self as usize
    }

    pub(crate) const fn from_i32(value: i32) -> Option<Self> {
        match value {
            0 => Some(Self::Anki),
            1 => Some(Self::Application),
            2 => Some(Self::AudioSources),
            3 => Some(Self::Behavior),
            4 => Some(Self::Dictionaries),
            5 => Some(Self::Interface),
            6 => Some(Self::Keybinds),
            7 => Some(Self::Ocr),
            8 => Some(Self::Asr),
            9 => Some(Self::Search),
            _ => None,
        }
    }

    pub(crate) fn from_setting_key(key: &str) -> Option<Self> {
        let prefix = key.split('/').next()?;
        match prefix {
            "anki" => Some(Self::Anki),
            "application" => Some(Self::Application),
            "audio-srcs" | "audio" => Some(Self::AudioSources),
            "behavior" => Some(Self::Behavior),
            "dictionary" => Some(Self::Dictionaries),
            "interface" | "subtitle-list" => Some(Self::Interface),
            "keybind" => Some(Self::Keybinds),
            "ocr" => Some(Self::Ocr),
            "asr" | "whisper" | "onnx" => Some(Self::Asr),
            "search" => Some(Self::Search),
            _ => None,
        }
    }
}

fn group_belongs_to_category(category: SettingsCategory, group: &str) -> bool {
    match category {
        SettingsCategory::Application => group == raw::application::GROUP,
        SettingsCategory::AudioSources => {
            group == raw::audio::GROUP
                || group == raw::audio_source::GROUP
                || group.starts_with(&format!("{}/", raw::audio_source::GROUP))
        }
        SettingsCategory::Behavior => group == raw::behavior::GROUP,
        SettingsCategory::Dictionaries => group == raw::dictionary::GROUP,
        SettingsCategory::Interface => {
            group == raw::interface::GROUP || group == raw::subtitle_list::GROUP
        }
        SettingsCategory::Keybinds => {
            group == raw::keybind::GROUP
                || group.starts_with(&format!(
                    "{}/{}/",
                    raw::keybind::GROUP,
                    raw::keybind::profile::GROUP
                ))
        }
        SettingsCategory::Ocr => group == raw::ocr::GROUP,
        SettingsCategory::Asr => {
            group == raw::asr::GROUP || group == raw::whisper::GROUP || group == raw::onnx::GROUP
        }
        SettingsCategory::Search => group == raw::search::GROUP,
        SettingsCategory::Anki => false,
    }
}

// ── Draft Snapshots for Revert-on-Close Without Apply (C5.2) ────────────────

#[derive(Clone, Debug)]
struct ConfigCategoryDraft {
    entries: Vec<(String, String, Value)>,
}

#[derive(Clone, Debug)]
struct AnkiProfileDraft {
    name: String,
    show_tag: bool,
    ex_glos: Vec<String>,
    fields: Vec<(String, String)>,
    term_fields: Vec<(String, String)>,
    kanji_fields: Vec<(String, String)>,
}

#[derive(Clone, Debug)]
struct AnkiDraft {
    enabled: bool,
    current_profile: String,
    profiles: Vec<AnkiProfileDraft>,
}

#[derive(Clone, Debug)]
enum CategoryDraftSnapshot {
    Config(ConfigCategoryDraft),
    Anki(AnkiDraft),
}

fn capture_config_category_draft(
    config: &ConfigController,
    category: SettingsCategory,
) -> ConfigCategoryDraft {
    let doc_text = config.store().render_document();
    let mut entries = Vec::new();
    if let (Ok(raw_doc), Ok(parsed_cfg)) = (
        Config::parse_document_text(&doc_text),
        Config::from_document_text(&doc_text),
    ) {
        for (group, section) in raw_doc.sections {
            if !group_belongs_to_category(category, &group) {
                continue;
            }
            for key in section.keys() {
                if let Some(value) = parsed_cfg.get_at(&group, key) {
                    entries.push((group.clone(), key.clone(), value));
                }
            }
        }
    }
    ConfigCategoryDraft { entries }
}

fn restore_config_category_draft(
    config: &ConfigController,
    category: SettingsCategory,
    draft: &ConfigCategoryDraft,
) {
    {
        let mut store = config.borrow_store_mut();
        let existing_groups: Vec<String> = store
            .groups()
            .into_iter()
            .filter(|g| group_belongs_to_category(category, g))
            .collect();
        for group in existing_groups {
            store.remove_group(&group);
        }
        for (group, key, value) in &draft.entries {
            let _ = store.put_at(group, key, value);
        }
        let rev = store.revision();
        let log = store.changelog().to_vec();
        store.mark_persisted_with(rev, log, |path| {
            group_belongs_to_category(category, &path.group)
        });
    }
    if category == SettingsCategory::Keybinds && config.keybind_profile_names().is_empty() {
        let _ = config.defaults_settings(SettingsCategory::Keybinds.as_i32());
    }
}

fn capture_anki_draft(config: &ConfigController) -> AnkiDraft {
    let enabled = config.anki_enabled();
    let current_profile = config.current_anki_profile();
    let names = config.anki_profile_names();
    let mut profiles = Vec::with_capacity(names.len());

    for name in &names {
        if config.current_anki_profile() != *name {
            let _ = config.edit_setting("anki/profile", name);
        }
        let show_tag = config.anki_show_tag();
        let ex_glos = config.anki_excluded_glossaries();
        let mut fields = Vec::new();
        let mut term_fields = Vec::new();
        let mut kanji_fields = Vec::new();

        for group in config.category_settings_groups(SettingsCategory::Anki.as_i32()) {
            for fi in 0..group.fields.row_count() {
                let Some(field) = group.fields.row_data(fi) else {
                    continue;
                };
                let key = field.key.as_str();
                if matches!(
                    key,
                    "anki/enabled" | "anki/profile/name" | "anki/profile/ex-glos"
                ) {
                    continue;
                }
                if let Some(name) = key.strip_prefix("anki/profile/term/field/") {
                    term_fields.push((name.to_owned(), field.value.to_string()));
                } else if let Some(name) = key.strip_prefix("anki/profile/kanji/field/") {
                    kanji_fields.push((name.to_owned(), field.value.to_string()));
                } else {
                    fields.push((key.to_owned(), field.value.to_string()));
                }
            }
        }

        profiles.push(AnkiProfileDraft {
            name: name.clone(),
            show_tag,
            ex_glos,
            fields,
            term_fields,
            kanji_fields,
        });
    }

    if config.current_anki_profile() != current_profile {
        let _ = config.edit_setting("anki/profile", &current_profile);
    }

    AnkiDraft {
        enabled,
        current_profile,
        profiles,
    }
}

fn restore_anki_draft(config: &ConfigController, draft: &AnkiDraft) {
    // 1. Ensure all snapshot profile names exist with exact casing.
    for profile in &draft.profiles {
        let existing = config.anki_profile_names();
        if existing.iter().any(|name| name == &profile.name) {
            continue;
        }
        if let Some(ci_match) = existing
            .iter()
            .find(|name| name.eq_ignore_ascii_case(&profile.name))
        {
            let _ = config.edit_setting("anki/profile", ci_match);
            let _ = config.edit_setting("anki/profile/name", &profile.name);
        } else {
            let _ = config.add_entry_with_options(
                SettingsCategory::Anki.as_i32(),
                &profile.name,
                false,
            );
        }
    }

    // 2. Remove profiles added since the snapshot.
    for existing in config.anki_profile_names() {
        if !draft.profiles.iter().any(|p| p.name == existing) {
            let _ = config.remove_entry(SettingsCategory::Anki.as_i32(), &existing);
        }
    }

    // 3. Restore each profile's settings & note field mappings.
    for profile in &draft.profiles {
        let _ = config.edit_setting("anki/profile", &profile.name);
        let _ = config.defaults_settings(SettingsCategory::Anki.as_i32());
        let _ = config.edit_setting(
            "anki/profile/show-tag",
            if profile.show_tag { "true" } else { "false" },
        );
        let ex_glos_raw = serialize_excluded_glossaries(&profile.ex_glos);
        let _ = config.edit_setting("anki/profile/ex-glos", &ex_glos_raw);
        for (key, value) in &profile.fields {
            let _ = config.edit_setting(key, value);
        }
        if let Ok(term_json) = serde_json::to_string(&profile.term_fields) {
            let _ = config.edit_setting("anki/profile/term/fields", &term_json);
        }
        if let Ok(kanji_json) = serde_json::to_string(&profile.kanji_fields) {
            let _ = config.edit_setting("anki/profile/kanji/fields", &kanji_json);
        }
    }

    // 4. Restore global Anki enabled toggle & selected profile.
    let _ = config.edit_setting(
        "anki/enabled",
        if draft.enabled { "true" } else { "false" },
    );
    let _ = config.edit_setting("anki/profile", &draft.current_profile);
}

pub(crate) struct SettingsSession {
    pub(crate) window: SettingsWindow,
}

pub(crate) struct SettingsContext {
    pub(crate) app_weak: slint::Weak<MementoApp>,
    pub(crate) config: Rc<ConfigController>,
    pub(crate) dict_ctrl: Rc<RefCell<DictController>>,
    pub(crate) dictionary_manager: Option<Rc<DictionaryManager>>,
    pub(crate) dictionary_database_path: PathBuf,
    pub(crate) cache_dir: PathBuf,
    pub(crate) anki_json_path: PathBuf,
    pub(crate) ocr_available: bool,
    pub(crate) asr_session: Rc<RefCell<controller::asr::AsrController>>,
    pub(crate) subtitles: Rc<RefCell<SubtitleController>>,
    pub(crate) subtitle_model: Rc<VecModel<SubEntry>>,
    pub(crate) asr_restart_pending: Rc<Cell<bool>>,
    pub(crate) mpv: Rc<RefCell<Option<MpvInstance>>>,
    pub(crate) coordinator: Rc<RefCell<Option<LivePlayerCoordinator>>>,
    dirty_model: Rc<VecModel<bool>>,
    draft_snapshots: RefCell<HashMap<SettingsCategory, CategoryDraftSnapshot>>,
    pub(crate) help_window: RefCell<Option<AnkiHelpWindow>>,
    pub(crate) anki_auto_fetch_attempted: Cell<bool>,
    pub(crate) anki_auto_fetch_in_flight: Arc<AtomicBool>,
}

impl SettingsContext {
    /// Captures a clean pre-edit snapshot of `category` before its first unsaved mutation.
    pub(crate) fn snapshot_category_if_clean(&self, category: SettingsCategory) {
        if category == SettingsCategory::Dictionaries {
            return;
        }
        if self.draft_snapshots.borrow().contains_key(&category) {
            return;
        }
        let snapshot = if category == SettingsCategory::Anki {
            CategoryDraftSnapshot::Anki(capture_anki_draft(&self.config))
        } else {
            CategoryDraftSnapshot::Config(capture_config_category_draft(&self.config, category))
        };
        self.draft_snapshots.borrow_mut().insert(category, snapshot);
    }

    pub(crate) fn clear_snapshot(&self, category: SettingsCategory) {
        self.draft_snapshots.borrow_mut().remove(&category);
    }

    pub(crate) fn mark_dirty(&self, category: SettingsCategory) {
        if category == SettingsCategory::Dictionaries {
            return;
        }
        let idx = category.as_usize();
        if self.dirty_model.row_data(idx) != Some(true) {
            self.dirty_model.set_row_data(idx, true);
        }
    }

    pub(crate) fn clear_dirty(&self, category: SettingsCategory) {
        let idx = category.as_usize();
        if self.dirty_model.row_data(idx) != Some(false) {
            self.dirty_model.set_row_data(idx, false);
        }
    }

    pub(crate) fn is_dirty(&self, category: SettingsCategory) -> bool {
        self.dirty_model.row_data(category.as_usize()) == Some(true)
    }

    pub(crate) fn dirty_categories(&self) -> Vec<SettingsCategory> {
        (0..SettingsCategory::COUNT)
            .filter(|&idx| self.dirty_model.row_data(idx) == Some(true))
            .filter_map(|idx| SettingsCategory::from_i32(idx as i32))
            .collect()
    }
}

// ── Theme Resolution & Sync (H2 & #357) ─────────────────────────────────────

pub(crate) const fn resolve_is_light_theme(
    mode: ThemeMode,
    system_scheme: SystemColorScheme,
) -> bool {
    match mode {
        ThemeMode::Light => true,
        ThemeMode::Dark => false,
        ThemeMode::System => matches!(system_scheme, SystemColorScheme::Light),
    }
}

fn effective_is_light_theme(
    config: &ConfigController,
    system_override: Option<SystemColorScheme>,
) -> bool {
    if let Some(scheme) = system_override {
        CACHED_SYSTEM_SCHEME.with(|slot| slot.set(Some(scheme)));
    }
    let mode = config.theme_mode();
    match mode {
        ThemeMode::Light => true,
        ThemeMode::Dark => false,
        ThemeMode::System => {
            let scheme = CACHED_SYSTEM_SCHEME.with(|slot| match slot.get() {
                Some(cached) => cached,
                None => {
                    let detected = detect_system_color_scheme();
                    slot.set(Some(detected));
                    detected
                }
            });
            resolve_is_light_theme(mode, scheme)
        }
    }
}

pub(crate) fn sync_window_theme(
    window: &SettingsWindow,
    config: &ConfigController,
    system_override: Option<SystemColorScheme>,
) -> bool {
    let is_light = effective_is_light_theme(config, system_override);
    if window.get_is_light_theme() != is_light {
        window.set_is_light_theme(is_light);
    }
    is_light
}

pub(crate) fn sync_theme(
    window: &SettingsWindow,
    ctx: &SettingsContext,
    system_override: Option<SystemColorScheme>,
) -> bool {
    let is_light = sync_window_theme(window, &ctx.config, system_override);
    if let Some(help) = ctx.help_window.borrow().as_ref() {
        if help.get_is_light_theme() != is_light {
            help.set_is_light_theme(is_light);
        }
    }
    is_light
}

// ── Live Subtitle Style Preview Sync ────────────────────────────────────────

fn parse_setting_color(config: &ConfigController, key: &str, default_hex: &str) -> slint::Color {
    let raw_hex = match config.effective_setting(raw::interface::GROUP, key) {
        Some(Value::Text(s)) => s,
        _ => default_hex.to_owned(),
    };
    let parsed = ArgbColor::parse(&raw_hex)
        .or_else(|_| ArgbColor::parse(default_hex))
        .unwrap_or_else(|_| ArgbColor::new(255, 255, 255, 255));
    slint::Color::from_argb_u8(parsed.a, parsed.r, parsed.g, parsed.b)
}

pub(crate) fn sync_subtitle_preview(window: &SettingsWindow, config: &ConfigController) {
    let font_raw = match config.effective_setting(
        raw::interface::GROUP,
        raw::interface::subtitle::FONT,
    ) {
        Some(Value::Text(s)) => s,
        _ => String::new(),
    };
    let spec = QtFontSpec::parse(&font_raw)
        .unwrap_or_else(|_| QtFontSpec::new("Noto Sans CJK JP", 24.0, false, false));

    if window.get_preview_sub_font_family().as_str() != spec.family {
        window.set_preview_sub_font_family(spec.family.into());
    }
    let font_size = (spec.point_size as f32).clamp(1.0, 500.0);
    if (window.get_preview_sub_font_size() - font_size).abs() > f32::EPSILON {
        window.set_preview_sub_font_size(font_size);
    }
    if window.get_preview_sub_font_bold() != spec.bold {
        window.set_preview_sub_font_bold(spec.bold);
    }
    if window.get_preview_sub_font_italic() != spec.italic {
        window.set_preview_sub_font_italic(spec.italic);
    }

    let text_color = parse_setting_color(
        config,
        raw::interface::subtitle::TEXT_COLOR,
        raw::interface::subtitle::TEXT_COLOR_DEFAULT,
    );
    if window.get_preview_sub_text_color() != text_color {
        window.set_preview_sub_text_color(text_color);
    }
    let bg_color = parse_setting_color(
        config,
        raw::interface::subtitle::BACKGROUND_COLOR,
        raw::interface::subtitle::BACKGROUND_COLOR_DEFAULT,
    );
    if window.get_preview_sub_bg_color() != bg_color {
        window.set_preview_sub_bg_color(bg_color);
    }
    let stroke_color = parse_setting_color(
        config,
        raw::interface::subtitle::STROKE_COLOR,
        raw::interface::subtitle::STROKE_COLOR_DEFAULT,
    );
    if window.get_preview_sub_stroke_color() != stroke_color {
        window.set_preview_sub_stroke_color(stroke_color);
    }

    let stroke_raw = match config.effective_setting(
        raw::interface::GROUP,
        raw::interface::subtitle::STROKE,
    ) {
        Some(Value::Real(v)) => v as f32,
        Some(Value::Int(v)) => v as f32,
        _ => raw::interface::subtitle::STROKE_DEFAULT as f32,
    };
    let stroke_width = (stroke_raw.max(0.0) / 10.0).clamp(0.0, 4.0);
    if (window.get_preview_sub_stroke_width() - stroke_width).abs() > f32::EPSILON {
        window.set_preview_sub_stroke_width(stroke_width);
    }

    let furigana_ratio = match config.effective_setting(
        raw::interface::GROUP,
        raw::interface::subtitle::FURIGANA_RATIO,
    ) {
        Some(Value::Real(v)) => v as f32,
        Some(Value::Int(v)) => v as f32,
        _ => raw::interface::subtitle::FURIGANA_RATIO_DEFAULT as f32,
    }
    .clamp(0.2, 0.9);
    if (window.get_preview_sub_furigana_ratio() - furigana_ratio).abs() > f32::EPSILON {
        window.set_preview_sub_furigana_ratio(furigana_ratio);
    }
}

// ── Custom Text Replacements Editor Sync & Validation (H6.5) ────────────────

pub(crate) fn validate_text_replacement_regex(pattern: &str) -> Result<(), String> {
    if pattern.is_empty() {
        return Ok(());
    }
    Regex::new(pattern)
        .map(|_| ())
        .map_err(|err| err.to_string())
}

fn validate_all_enabled_text_replacements(config: &ConfigController) -> Result<(), String> {
    let opts = config.text_replacement_options();
    for (idx, rule) in opts.rules.iter().enumerate() {
        if rule.enabled && !rule.pattern.is_empty() {
            validate_text_replacement_regex(&rule.pattern).map_err(|err| {
                format!("Custom text replacement rule {} has invalid regex: {err}", idx + 1)
            })?;
        }
    }
    Ok(())
}

fn rule_to_row(rule: &TextReplacementRule) -> TextReplacementRow {
    TextReplacementRow {
        pattern: rule.pattern.clone().into(),
        replacement: rule.replacement.clone().into(),
        ignore_case: rule.ignore_case,
        enabled: rule.enabled,
    }
}

fn rows_equal(a: &TextReplacementRow, b: &TextReplacementRow) -> bool {
    a.pattern == b.pattern
        && a.replacement == b.replacement
        && a.ignore_case == b.ignore_case
        && a.enabled == b.enabled
}

pub(crate) fn sync_text_replacements(window: &SettingsWindow, config: &ConfigController) {
    let opts = config.text_replacement_options();
    if window.get_text_replacements_search_original() != opts.search_original {
        window.set_text_replacements_search_original(opts.search_original);
    }
    let next_rows: Vec<TextReplacementRow> = opts.rules.iter().map(rule_to_row).collect();
    let current = window.get_text_replacement_rows();
    if current.row_count() == next_rows.len() {
        if let Some(vec_model) = current
            .as_any()
            .downcast_ref::<VecModel<TextReplacementRow>>()
        {
            for (idx, row) in next_rows.into_iter().enumerate() {
                let changed = vec_model
                    .row_data(idx)
                    .is_none_or(|old| !rows_equal(&old, &row));
                if changed {
                    vec_model.set_row_data(idx, row);
                }
            }
            return;
        }
    }
    window.set_text_replacement_rows(Rc::new(VecModel::from(next_rows)).into());
}

fn persist_text_replacement_rules_to_draft(
    ctx: &SettingsContext,
    rules: &[TextReplacementRule],
) -> Result<(), String> {
    let json = serde_json::to_string(rules)
        .map_err(|err| format!("Could not encode text replacement rules: {err}"))?;
    ctx.snapshot_category_if_clean(SettingsCategory::Search);
    ctx.config.edit_setting("search/text-replacements", &json)?;
    ctx.mark_dirty(SettingsCategory::Search);
    Ok(())
}

// ── Keybind Profile Rename & Inline Validation Helpers ──────────────────────

fn rename_keybind_profile_in_config(
    config: &ConfigController,
    old_name: &str,
    new_name: &str,
) -> Result<String, String> {
    let existing = config.keybind_profile_names();
    if !existing.iter().any(|n| n == old_name) {
        return Err(format!("keybind profile {old_name:?} does not exist"));
    }
    let trimmed_new = validate_profile_name(new_name, &existing, false, Some(old_name))?;
    if trimmed_new == old_name {
        return Ok(trimmed_new);
    }

    let prefix = format!(
        "{}/{}/",
        raw::keybind::GROUP,
        raw::keybind::profile::GROUP
    );
    let old_group = format!("{prefix}{old_name}");
    let new_group = format!("{prefix}{trimmed_new}");

    let doc_text = config.store().render_document();
    let mut entries: Vec<(String, Value)> = Vec::new();
    if let (Ok(raw_doc), Ok(parsed_cfg)) = (
        Config::parse_document_text(&doc_text),
        Config::from_document_text(&doc_text),
    ) {
        if let Some(section) = raw_doc.sections.get(&old_group) {
            for key in section.keys() {
                if let Some(value) = parsed_cfg.get_at(&old_group, key) {
                    entries.push((key.clone(), value));
                }
            }
        }
    }

    let mut store = config.borrow_store_mut();
    store.remove_group(&old_group);
    for (key, value) in entries {
        store
            .put_at(&new_group, &key, &value)
            .map_err(|err| format!("{err:?}"))?;
    }
    store
        .put_at(
            raw::keybind::GROUP,
            raw::keybind::CURRENT_PROFILE,
            &Value::Text(trimmed_new.clone()),
        )
        .map_err(|err| format!("{err:?}"))?;
    Ok(trimmed_new)
}

fn set_inline_field_conflict(window: &SettingsWindow, target_key: &str, message: &str) {
    let groups = window.get_settings_groups();
    for gi in 0..groups.row_count() {
        let Some(group) = groups.row_data(gi) else {
            continue;
        };
        let Some(fields_vec) = group
            .fields
            .as_any()
            .downcast_ref::<VecModel<SettingsField>>()
        else {
            continue;
        };
        for fi in 0..fields_vec.row_count() {
            let Some(mut field) = fields_vec.row_data(fi) else {
                continue;
            };
            if field.key.as_str() == target_key {
                field.conflict = message.into();
                fields_vec.set_row_data(fi, field);
                return;
            }
        }
    }
}

fn initial_browse_dir(config: &ConfigController, key: &str) -> Option<PathBuf> {
    let (group, leaf) = key.rsplit_once('/')?;
    let raw_val = match config.effective_setting(group, leaf)? {
        Value::Text(s) => s,
        _ => return None,
    };
    let decoded = decode_file_uri(&raw_val)?;
    if decoded.is_dir() {
        Some(decoded)
    } else if let Some(parent) = decoded.parent() {
        parent.is_dir().then(|| parent.to_path_buf())
    } else {
        None
    }
}

fn revert_unsaved_drafts(window: &SettingsWindow, ctx: &SettingsContext) {
    let pending: Vec<(SettingsCategory, CategoryDraftSnapshot)> =
        ctx.draft_snapshots.borrow_mut().drain().collect();
    for (cat, snapshot) in pending {
        if !ctx.is_dirty(cat) {
            continue;
        }
        match snapshot {
            CategoryDraftSnapshot::Config(cfg_draft) => {
                restore_config_category_draft(&ctx.config, cat, &cfg_draft);
            }
            CategoryDraftSnapshot::Anki(anki_draft) => {
                restore_anki_draft(&ctx.config, &anki_draft);
            }
        }
        ctx.clear_dirty(cat);
    }
    // Always clear any stray dirty flags and reload disk custom CSS buffer.
    for idx in 0..SettingsCategory::COUNT {
        if let Some(cat) = SettingsCategory::from_i32(idx as i32) {
            ctx.clear_dirty(cat);
        }
    }
    // Clear cached OS scheme so next open detects fresh system theme changes.
    CACHED_SYSTEM_SCHEME.with(|slot| slot.set(None));
    refresh_custom_popup_css(window, &ctx.cache_dir);
    refresh_settings(window, &ctx.config);
    sync_theme(window, ctx, None);
    sync_subtitle_preview(window, &ctx.config);
    sync_text_replacements(window, &ctx.config);
    anki::refresh_anki_excluded_dicts(window, ctx.dictionary_manager.as_deref(), &ctx.config);
    anki::reset_auto_fetch_attempt(ctx);
    window.set_anki_autocomplete_open(false);
    window.set_profile_modal_open(false);
    window.set_profile_name_error("".into());
    window.set_drop_hover_active(false);
    window.set_settings_status("".into());
}

/// Formats a Slint `KeyEvent` payload into Memento's canonical shortcut chord
/// string (e.g. `"Ctrl+Shift+C"`, `"F3"`, `"Ctrl+Down"`). Returns `None` for
/// empty or standalone modifier events.
pub(crate) fn format_key_chord(
    text: &str,
    control: bool,
    alt: bool,
    shift: bool,
    meta: bool,
) -> Option<String> {
    if text.is_empty() {
        return None;
    }

    let base = match text {
        "\u{F700}" => Some("Up".to_owned()),
        "\u{F701}" => Some("Down".to_owned()),
        "\u{F702}" => Some("Left".to_owned()),
        "\u{F703}" => Some("Right".to_owned()),
        "\u{F704}" => Some("F1".to_owned()),
        "\u{F705}" => Some("F2".to_owned()),
        "\u{F706}" => Some("F3".to_owned()),
        "\u{F707}" => Some("F4".to_owned()),
        "\u{F708}" => Some("F5".to_owned()),
        "\u{F709}" => Some("F6".to_owned()),
        "\u{F70A}" => Some("F7".to_owned()),
        "\u{F70B}" => Some("F8".to_owned()),
        "\u{F70C}" => Some("F9".to_owned()),
        "\u{F70D}" => Some("F10".to_owned()),
        "\u{F70E}" => Some("F11".to_owned()),
        "\u{F70F}" => Some("F12".to_owned()),
        "\u{F727}" => Some("Insert".to_owned()),
        "\u{F728}" | "\u{007f}" => Some("Delete".to_owned()),
        "\u{F729}" => Some("Home".to_owned()),
        "\u{F72B}" => Some("End".to_owned()),
        "\u{F72C}" => Some("PageUp".to_owned()),
        "\u{F72D}" => Some("PageDown".to_owned()),
        "\u{0008}" => Some("Backspace".to_owned()),
        "\u{0009}" | "\u{0019}" => Some("Tab".to_owned()),
        "\u{000a}" | "\r" => Some("Return".to_owned()),
        " " => Some("Space".to_owned()),
        "\u{001b}" => Some("Escape".to_owned()),
        // Standalone modifier / lock keys in Slint's PUA / control ranges
        "\u{0010}" | "\u{0011}" | "\u{0012}" | "\u{0013}" | "\u{0014}" | "\u{0015}"
        | "\u{0016}" | "\u{0017}" | "\u{0018}" => None,
        other => {
            let mut chars = other.chars();
            let ch = chars.next()?;
            if chars.next().is_some() {
                None
            } else if control && ('\u{0001}'..='\u{001a}').contains(&ch) {
                let letter = (b'A' + (ch as u8 - 1)) as char;
                Some(letter.to_string())
            } else if ('\u{F700}'..='\u{F8FF}').contains(&ch) || ch.is_control() {
                None
            } else if ch.is_ascii_alphabetic() {
                Some(ch.to_ascii_uppercase().to_string())
            } else {
                Some(ch.to_string())
            }
        }
    }?;

    let mut chord = String::new();
    if control {
        chord.push_str("Ctrl+");
    }
    if alt {
        chord.push_str("Alt+");
    }
    if shift {
        chord.push_str("Shift+");
    }
    if meta {
        chord.push_str("Meta+");
    }
    chord.push_str(&base);
    Some(chord)
}

fn fields_equal(a: &SettingsField, b: &SettingsField) -> bool {
    if a.key != b.key
        || a.label != b.label
        || a.description != b.description
        || a.kind != b.kind
        || a.value != b.value
        || a.enabled != b.enabled
        || a.visible != b.visible
        || a.font_family != b.font_family
        || a.font_size != b.font_size
        || a.font_bold != b.font_bold
        || a.font_italic != b.font_italic
        || a.color_swatch != b.color_swatch
        || a.color_a != b.color_a
        || a.color_r != b.color_r
        || a.color_g != b.color_g
        || a.color_b != b.color_b
        || a.conflict != b.conflict
        || a.choices.row_count() != b.choices.row_count()
    {
        return false;
    }
    for idx in 0..a.choices.row_count() {
        if a.choices.row_data(idx) != b.choices.row_data(idx) {
            return false;
        }
    }
    true
}

fn groups_structure_matches(
    current: &slint::ModelRc<SettingsGroup>,
    next: &[SettingsGroup],
) -> bool {
    if current.row_count() != next.len() {
        return false;
    }
    for (gi, next_group) in next.iter().enumerate() {
        let Some(curr_group) = current.row_data(gi) else {
            return false;
        };
        if curr_group.category != next_group.category
            || curr_group.title != next_group.title
            || curr_group.entry_id != next_group.entry_id
            || curr_group.fields.row_count() != next_group.fields.row_count()
        {
            return false;
        }
        if curr_group
            .fields
            .as_any()
            .downcast_ref::<VecModel<SettingsField>>()
            .is_none()
        {
            return false;
        }
        for fi in 0..next_group.fields.row_count() {
            let (Some(cf), Some(nf)) = (curr_group.fields.row_data(fi), next_group.fields.row_data(fi)) else {
                return false;
            };
            if cf.key != nf.key || cf.kind != nf.kind {
                return false;
            }
        }
    }
    true
}

/// Update `settings_groups` IN-PLACE via `VecModel::set_row_data` whenever the
/// group/field structure is unchanged, preserving `MementoSwitch`'s 120ms slide
/// animation and existing widget focus. Falls back to replacing `settings_groups`
/// only on structural changes (category switch, profile change, add/remove entry).
fn sync_groups_in_place(window: &SettingsWindow, next_groups: Vec<SettingsGroup>) {
    let current = window.get_settings_groups();
    if groups_structure_matches(&current, &next_groups) {
        for (gi, next_group) in next_groups.into_iter().enumerate() {
            let Some(curr_group) = current.row_data(gi) else {
                continue;
            };
            let Some(fields_vec) = curr_group
                .fields
                .as_any()
                .downcast_ref::<VecModel<SettingsField>>()
            else {
                continue;
            };
            for fi in 0..next_group.fields.row_count() {
                let Some(new_field) = next_group.fields.row_data(fi) else {
                    continue;
                };
                let changed = fields_vec
                    .row_data(fi)
                    .is_none_or(|old_field| !fields_equal(&old_field, &new_field));
                if changed {
                    fields_vec.set_row_data(fi, new_field);
                }
            }
        }
    } else {
        window.set_settings_groups(Rc::new(VecModel::from(next_groups)).into());
    }
}

fn sync_header_metadata(window: &SettingsWindow, config: &ConfigController) {
    let current_anki = config.current_anki_profile();
    if window.get_current_anki_profile().as_str() != current_anki {
        window.set_current_anki_profile(current_anki.into());
    }
    let show_tag = config.anki_show_tag();
    if window.get_anki_show_tag() != show_tag {
        window.set_anki_show_tag(show_tag);
    }

    let anki_profiles = config.anki_profile_names();
    let existing_profiles = window.get_anki_profile_list();
    let profiles_changed = existing_profiles.row_count() != anki_profiles.len()
        || anki_profiles.iter().enumerate().any(|(i, name)| {
            existing_profiles
                .row_data(i)
                .is_none_or(|cur| cur.as_str() != name.as_str())
        });
    if profiles_changed {
        let model: Vec<SharedString> = anki_profiles.into_iter().map(Into::into).collect();
        window.set_anki_profile_list(Rc::new(VecModel::from(model)).into());
    }

    let current_kb = config.current_keybind_profile();
    if window.get_current_keybind_profile().as_str() != current_kb {
        window.set_current_keybind_profile(current_kb.into());
    }
    let conflicts = config.keybind_conflict_count() as i32;
    if window.get_keybind_conflict_count() != conflicts {
        window.set_keybind_conflict_count(conflicts);
    }
}

fn filtered_settings_groups(
    config: &ConfigController,
    query: &str,
    ocr_built: bool,
    asr_available: bool,
) -> Vec<SettingsGroup> {
    let q = query.trim().to_ascii_lowercase();
    if q.is_empty() {
        return Vec::new();
    }
    let mut results = Vec::new();
    for group in config.settings_groups() {
        if group.category == SettingsCategory::Dictionaries.as_i32() {
            continue;
        }
        if group.category == SettingsCategory::Ocr.as_i32() && !ocr_built {
            continue;
        }
        if group.category == SettingsCategory::Asr.as_i32() && !asr_available {
            continue;
        }
        let group_title_matches = group.title.as_str().to_ascii_lowercase().contains(&q);
        let mut matched_fields = Vec::new();
        for fi in 0..group.fields.row_count() {
            let Some(field) = group.fields.row_data(fi) else {
                continue;
            };
            if !field.visible {
                continue;
            }
            let field_matches = group_title_matches
                || field.label.as_str().to_ascii_lowercase().contains(&q)
                || field.description.as_str().to_ascii_lowercase().contains(&q)
                || field.key.as_str().to_ascii_lowercase().contains(&q);
            if field_matches {
                matched_fields.push(field);
            }
        }
        if !matched_fields.is_empty() {
            results.push(SettingsGroup {
                category: group.category,
                title: group.title,
                entry_id: group.entry_id,
                fields: Rc::new(VecModel::from(matched_fields)).into(),
            });
        }
    }
    results
}

pub(crate) fn refresh_category_settings(
    window: &SettingsWindow,
    config: &ConfigController,
    category: i32,
) {
    sync_window_theme(window, config, None);
    let query = window.get_settings_search_query();
    if !query.trim().is_empty() {
        let matched = filtered_settings_groups(
            config,
            query.as_str(),
            window.get_ocr_built(),
            window.get_asr_available(),
        );
        let needs_fonts = matched.iter().any(|g| {
            (0..g.fields.row_count())
                .filter_map(|i| g.fields.row_data(i))
                .any(|f| f.kind == "font")
        });
        if needs_fonts {
            fonts::ensure_font_families_loaded(window, config);
        }
        sync_groups_in_place(window, matched);
        sync_header_metadata(window, config);
        sync_subtitle_preview(window, config);
        return;
    }

    if category == SettingsCategory::Interface.as_i32() {
        fonts::ensure_font_families_loaded(window, config);
        sync_subtitle_preview(window, config);
    } else if category == SettingsCategory::Search.as_i32() {
        sync_text_replacements(window, config);
    } else if category == SettingsCategory::Anki.as_i32() {
        anki::refresh_anki_excluded_dicts_from_window(window, config);
    }
    sync_groups_in_place(window, config.category_settings_groups(category));
    sync_header_metadata(window, config);
}

pub(crate) fn refresh_settings(window: &SettingsWindow, config: &ConfigController) {
    refresh_category_settings(window, config, window.get_active_category());
}

/// Fast in-place sync after field edits (switch, choice, color, font, keybind):
/// mutates ONLY changed `SettingsField` rows and header metadata without
/// rebuilding `settings_groups` or re-sorting `font_families`.
pub(crate) fn sync_after_edit(window: &SettingsWindow, config: &ConfigController) {
    let query = window.get_settings_search_query();
    let next_groups = if !query.trim().is_empty() {
        filtered_settings_groups(
            config,
            query.as_str(),
            window.get_ocr_built(),
            window.get_asr_available(),
        )
    } else {
        config.category_settings_groups(window.get_active_category())
    };
    sync_groups_in_place(window, next_groups);
    sync_header_metadata(window, config);
}

pub(crate) fn report_settings(
    window: &slint::Weak<SettingsWindow>,
    config: &ConfigController,
    result: Result<(), String>,
) {
    if let Some(window) = window.upgrade() {
        match result {
            Ok(()) => {
                refresh_settings(&window, config);
                window.set_settings_status("Settings updated".into());
            }
            Err(error) => window.set_settings_status(error.into()),
        }
    }
}

fn apply_single_category(
    window: Option<&SettingsWindow>,
    ctx: &SettingsContext,
    category: i32,
) -> Result<(), String> {
    if category == SettingsCategory::Search.as_i32() {
        validate_all_enabled_text_replacements(&ctx.config)?;
    }
    if category == SettingsCategory::Interface.as_i32() {
        if let Some(win) = window {
            custom_css::save_custom_css_on_apply(win, &ctx.cache_dir)?;
        }
    }
    ctx.config
        .apply_settings(category, &ctx.cache_dir, "memento.conf", &ctx.anki_json_path)?;
    if let Some(cat) = SettingsCategory::from_i32(category) {
        ctx.clear_dirty(cat);
        ctx.clear_snapshot(cat);
    }
    if category == SettingsCategory::Interface.as_i32() {
        let is_light = if let Some(win) = window {
            sync_theme(win, ctx, None)
        } else {
            effective_is_light_theme(&ctx.config, None)
        };
        if let Some(app) = ctx.app_weak.upgrade() {
            app.set_is_light_theme(is_light);
        }
    }
    if category == SettingsCategory::Search.as_i32() {
        ctx.dict_ctrl
            .borrow_mut()
            .set_text_replacements(ctx.config.text_replacement_options());
    }
    if let Some(app) = ctx.app_weak.upgrade() {
        refresh_player_preferences(&app, &ctx.config);
        if category == SettingsCategory::Dictionaries.as_i32()
            || category == SettingsCategory::Search.as_i32()
        {
            apply_dictionary_order(&ctx.config, &mut ctx.dict_ctrl.borrow_mut());
        }
        if category == SettingsCategory::Dictionaries.as_i32() {
            if let Some(win) = window {
                refresh_dictionary_settings(win, ctx.dictionary_manager.as_deref(), &ctx.config);
            }
        }
        if category == SettingsCategory::Interface.as_i32()
            || category == SettingsCategory::Search.as_i32()
        {
            refresh_subtitle_list(&app, &ctx.subtitle_model, &ctx.subtitles.borrow());
        }
        app.set_ocr_enabled(
            ctx.ocr_available
                && setting_enabled(
                    &ctx.config,
                    raw::ocr::GROUP,
                    raw::ocr::ENABLED,
                    raw::ocr::ENABLED_DEFAULT,
                ),
        );
        app.set_asr_enabled(
            controller::asr::AVAILABLE
                && setting_enabled(
                    &ctx.config,
                    raw::asr::GROUP,
                    raw::asr::ENABLED,
                    raw::asr::ENABLED_DEFAULT,
                ),
        );
        if category == SettingsCategory::Asr.as_i32() {
            if let Some(win) = window {
                asr_models::refresh_asr_model_status(win, &ctx.config, &ctx.cache_dir);
            }
            let config_dir =
                memento_xdg_dir("XDG_CONFIG_HOME", ".config").unwrap_or_else(|| PathBuf::from("."));
            let unchanged =
                controller::asr::AsrSettings::from_config(config_dir, &ctx.config.store())
                    .is_ok_and(|settings| ctx.asr_session.borrow().matches_settings(&settings));
            if !unchanged {
                clear_asr_for_settings(
                    &app,
                    &ctx.asr_session,
                    &ctx.subtitles,
                    &ctx.subtitle_model,
                    &ctx.asr_restart_pending,
                    Some(&ctx.mpv),
                    Some(&ctx.coordinator),
                );
            }
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn create_settings_session(
    app: &MementoApp,
    config_ctrl: Rc<ConfigController>,
    dict_ctrl: Rc<RefCell<DictController>>,
    dictionary_manager: Option<Rc<DictionaryManager>>,
    dictionary_database_path: PathBuf,
    cache_dir: PathBuf,
    anki_json_path: PathBuf,
    ocr_available: bool,
    asr_session: Rc<RefCell<controller::asr::AsrController>>,
    subtitles: Rc<RefCell<SubtitleController>>,
    subtitle_model: Rc<VecModel<SubEntry>>,
    asr_restart_pending: Rc<Cell<bool>>,
    mpv: Rc<RefCell<Option<MpvInstance>>>,
    coordinator: Rc<RefCell<Option<LivePlayerCoordinator>>>,
) -> Result<SettingsSession, slint::PlatformError> {
    let settings = SettingsWindow::new()?;
    let dirty_model = Rc::new(VecModel::from(vec![false; SettingsCategory::COUNT]));
    settings.set_dirty_categories(dirty_model.clone().into());

    let ctx = Rc::new(SettingsContext {
        app_weak: app.as_weak(),
        config: config_ctrl,
        dict_ctrl,
        dictionary_manager,
        dictionary_database_path,
        cache_dir,
        anki_json_path,
        ocr_available,
        asr_session,
        subtitles,
        subtitle_model,
        asr_restart_pending,
        mpv,
        coordinator,
        dirty_model,
        draft_snapshots: RefCell::new(HashMap::new()),
        help_window: RefCell::new(None),
        anki_auto_fetch_attempted: Cell::new(false),
        anki_auto_fetch_in_flight: Arc::new(AtomicBool::new(false)),
    });

    settings.set_ocr_built(cfg!(feature = "ocr"));
    settings.set_ocr_available(ctx.ocr_available);
    settings.set_asr_available(controller::asr::AVAILABLE);
    sync_theme(&settings, &ctx, None);
    refresh_dictionary_settings(&settings, ctx.dictionary_manager.as_deref(), &ctx.config);
    refresh_settings(&settings, &ctx.config);
    sync_subtitle_preview(&settings, &ctx.config);
    sync_text_replacements(&settings, &ctx.config);
    anki::refresh_anki_excluded_dicts(&settings, ctx.dictionary_manager.as_deref(), &ctx.config);
    refresh_custom_popup_css(&settings, &ctx.cache_dir);
    asr_models::refresh_asr_model_status(&settings, &ctx.config, &ctx.cache_dir);

    // Close / Hide handling — revert any unsaved draft edits before hiding (C5.2)
    let close_req_window = settings.as_weak();
    let close_req_ctx = ctx.clone();
    settings.window().on_close_requested(move || {
        if let Some(win) = close_req_window.upgrade() {
            revert_unsaved_drafts(&win, &close_req_ctx);
            let _ = win.hide();
        }
        slint::CloseRequestResponse::HideWindow
    });
    let close_btn_window = settings.as_weak();
    let close_btn_ctx = ctx.clone();
    settings.on_close(move || {
        if let Some(win) = close_btn_window.upgrade() {
            revert_unsaved_drafts(&win, &close_btn_ctx);
            let _ = win.hide();
        }
    });

    // Category navigation & quick search
    let cat_window = settings.as_weak();
    let cat_ctx = ctx.clone();
    settings.on_category_changed(move |category| {
        if let Some(window) = cat_window.upgrade() {
            refresh_category_settings(&window, &cat_ctx.config, category);
            if category == SettingsCategory::Anki.as_i32() {
                anki::refresh_anki_excluded_dicts(
                    &window,
                    cat_ctx.dictionary_manager.as_deref(),
                    &cat_ctx.config,
                );
                anki::maybe_auto_fetch_decks_and_models(&window, &cat_ctx);
            } else {
                anki::reset_auto_fetch_attempt(&cat_ctx);
            }
            if category == SettingsCategory::Dictionaries.as_i32() {
                refresh_dictionary_settings(
                    &window,
                    cat_ctx.dictionary_manager.as_deref(),
                    &cat_ctx.config,
                );
            } else if category == SettingsCategory::Interface.as_i32() {
                refresh_custom_popup_css(&window, &cat_ctx.cache_dir);
                sync_subtitle_preview(&window, &cat_ctx.config);
            } else if category == SettingsCategory::Asr.as_i32() {
                asr_models::refresh_asr_model_status(&window, &cat_ctx.config, &cat_ctx.cache_dir);
            } else if category == SettingsCategory::Search.as_i32() {
                sync_text_replacements(&window, &cat_ctx.config);
            }
            window.window().request_redraw();
        }
    });

    let search_window = settings.as_weak();
    let search_ctx = ctx.clone();
    settings.on_settings_search_changed(move |_query| {
        if let Some(window) = search_window.upgrade() {
            refresh_category_settings(&window, &search_ctx.config, window.get_active_category());
        }
    });

    // Native File & Folder Browse button (C1.1 — macOS main-thread safe)
    let browse_window = settings.as_weak();
    let browse_ctx = ctx.clone();
    settings.on_browse_folder_setting(move |key| {
        let key_owned = key.to_string();
        let initial_dir = initial_browse_dir(&browse_ctx.config, &key_owned);
        let meta = setting_browse_metadata(&key_owned);
        let is_file = meta.is_some_and(|m| m.kind == BrowseKind::File);

        let win_for_cb = browse_window.clone();
        let key_for_cb = key_owned.clone();
        let dispatch_to_main = |task: Box<dyn FnOnce() + Send>| {
            let _ = slint::invoke_from_event_loop(task);
        };

        if is_file {
            let title = meta
                .map(|m| m.dialog_title.to_owned())
                .unwrap_or_else(|| "Select File".to_owned());
            let opts = FileDialogOptions {
                title,
                kind: FileDialogKind::ArchiveZip,
                multiple: false,
                initial_dir,
            };
            open_files_on_main_thread(opts, dispatch_to_main, move |paths| {
                let Some(picked) = paths.into_iter().next() else {
                    return;
                };
                let formatted = format_browse_selection(&key_for_cb, &picked);
                let _ = win_for_cb.upgrade_in_event_loop(move |win| {
                    win.invoke_setting_edited(key_for_cb.into(), formatted.into());
                });
            });
        } else {
            open_folder_on_main_thread(initial_dir, dispatch_to_main, move |dir| {
                let formatted = format_browse_selection(&key_for_cb, &dir);
                let _ = win_for_cb.upgrade_in_event_loop(move |win| {
                    win.invoke_setting_edited(key_for_cb.into(), formatted.into());
                });
            });
        }
    });

    // Setting edits (in-place row mutation preserves MementoSwitch 120ms animation)
    let edited_window = settings.as_weak();
    let edited_ctx = ctx.clone();
    settings.on_setting_edited(move |key, value| {
        let Some(window) = edited_window.upgrade() else {
            return;
        };
        let target_cat = SettingsCategory::from_setting_key(&key)
            .or_else(|| SettingsCategory::from_i32(window.get_active_category()));
        if let Some(cat) = target_cat {
            edited_ctx.snapshot_category_if_clean(cat);
        }
        match edited_ctx.config.edit_setting(&key, &value) {
            Ok(()) => {
                if let Some(cat) = target_cat {
                    edited_ctx.mark_dirty(cat);
                }
                let needs_sync = matches!(value.as_str(), "true" | "false")
                    || matches!(
                        key.as_str(),
                        "anki/profile"
                            | "anki/profile/ex-glos"
                            | "interface/theme"
                            | "keybind/current"
                            | "behavior/file-open-dir"
                            | "behavior/file-open-custom"
                            | "asr/backend"
                            | "whisper/model"
                            | "whisper/custom-model"
                            | "whisper/vad-model"
                            | "onnx/model"
                            | "onnx/custom-model"
                    )
                    || key.ends_with("/deck")
                    || key.ends_with("/model")
                    || key.starts_with("keybind/profiles/");
                if needs_sync {
                    sync_after_edit(&window, &edited_ctx.config);
                }
                if key == "interface/theme" {
                    sync_theme(&window, &edited_ctx, None);
                }
                if key.starts_with("interface/sub-") {
                    sync_subtitle_preview(&window, &edited_ctx.config);
                }
                if key == "search/subtitle-furigana" {
                    window.set_preview_sub_show_furigana(value == "true");
                }
                if key == "search/text-replacements"
                    || key == "search/text-replacements-search-original"
                {
                    sync_text_replacements(&window, &edited_ctx.config);
                }
                if key == "anki/profile" || key == "anki/profile/ex-glos" {
                    anki::refresh_anki_excluded_dicts(
                        &window,
                        edited_ctx.dictionary_manager.as_deref(),
                        &edited_ctx.config,
                    );
                }
                if (key == "anki/enabled" && value == "true") || key == "anki/profile" {
                    anki::reset_auto_fetch_attempt(&edited_ctx);
                    anki::maybe_auto_fetch_decks_and_models(&window, &edited_ctx);
                }
                if key == "dictionary/order" {
                    refresh_dictionary_settings(
                        &window,
                        edited_ctx.dictionary_manager.as_deref(),
                        &edited_ctx.config,
                    );
                }
                if matches!(
                    key.as_str(),
                    "asr/backend"
                        | "whisper/model"
                        | "whisper/custom-model"
                        | "whisper/vad-enable"
                        | "whisper/vad-model"
                        | "onnx/model"
                        | "onnx/custom-model"
                ) {
                    asr_models::refresh_asr_model_status(
                        &window,
                        &edited_ctx.config,
                        &edited_ctx.cache_dir,
                    );
                }
                window.set_settings_status("Unsaved changes — click Apply".into());
                if matches!(
                    key.as_str(),
                    "anki/profile/term/model" | "anki/profile/kanji/model"
                ) && (0..window.get_available_anki_models().row_count())
                    .filter_map(|index| window.get_available_anki_models().row_data(index))
                    .any(|model| model == value)
                {
                    window.invoke_load_anki_fields(
                        if key.contains("/term/") {
                            "term"
                        } else {
                            "kanji"
                        }
                        .into(),
                    );
                }
            }
            Err(error) => {
                if key.starts_with("keybind/profiles/") {
                    set_inline_field_conflict(&window, &key, &error);
                }
                window.set_settings_status(error.into());
            }
        }
    });

    // Font editing (in-place update, incremental custom font insertion, live subtitle preview)
    let font_window = settings.as_weak();
    let font_ctx = ctx.clone();
    settings.on_font_edited(move |key, family, size, bold, italic| {
        if let Some(window) = font_window.upgrade() {
            font_ctx.snapshot_category_if_clean(SettingsCategory::Interface);
            match font_ctx
                .config
                .edit_font_setting(&key, &family, &size, bold, italic)
            {
                Ok(()) => {
                    font_ctx.mark_dirty(SettingsCategory::Interface);
                    fonts::ensure_font_family_listed(&window, &family);
                    sync_after_edit(&window, &font_ctx.config);
                    if key == "interface/sub-font" {
                        sync_subtitle_preview(&window, &font_ctx.config);
                    }
                    window.set_settings_status("Unsaved changes — click Apply".into());
                }
                Err(error) => window.set_settings_status(error.into()),
            }
        }
    });

    let font_typed_window = settings.as_weak();
    let font_typed_ctx = ctx.clone();
    settings.on_font_size_typed(move |key, family, size, bold, italic| {
        if let Some(window) = font_typed_window.upgrade() {
            font_typed_ctx.snapshot_category_if_clean(SettingsCategory::Interface);
            match font_typed_ctx
                .config
                .edit_font_setting(&key, &family, &size, bold, italic)
            {
                Ok(()) => {
                    font_typed_ctx.mark_dirty(SettingsCategory::Interface);
                    if key == "interface/sub-font" {
                        sync_subtitle_preview(&window, &font_typed_ctx.config);
                    }
                    window.set_settings_status("Unsaved changes — click Apply".into());
                }
                Err(error) => window.set_settings_status(error.into()),
            }
        }
    });

    // Color picker & hex editing (in-place update, live subtitle preview)
    let color_window = settings.as_weak();
    let color_ctx = ctx.clone();
    settings.on_color_edited(move |key, a, r, g, b| {
        if let Some(window) = color_window.upgrade() {
            color_ctx.snapshot_category_if_clean(SettingsCategory::Interface);
            match color_ctx.config.edit_color_setting(&key, a, r, g, b) {
                Ok(()) => {
                    color_ctx.mark_dirty(SettingsCategory::Interface);
                    sync_after_edit(&window, &color_ctx.config);
                    if key.starts_with("interface/sub-") {
                        sync_subtitle_preview(&window, &color_ctx.config);
                    }
                    window.set_settings_status("Unsaved changes — click Apply".into());
                }
                Err(error) => window.set_settings_status(error.into()),
            }
        }
    });

    let color_hex_window = settings.as_weak();
    let color_hex_ctx = ctx.clone();
    settings.on_color_hex_committed(move |key, value| {
        if let Some(window) = color_hex_window.upgrade() {
            color_hex_ctx.snapshot_category_if_clean(SettingsCategory::Interface);
            match color_hex_ctx.config.edit_setting(&key, &value) {
                Ok(()) => {
                    color_hex_ctx.mark_dirty(SettingsCategory::Interface);
                    sync_after_edit(&window, &color_hex_ctx.config);
                    if key.starts_with("interface/sub-") {
                        sync_subtitle_preview(&window, &color_hex_ctx.config);
                    }
                    window.set_settings_status("Unsaved changes — click Apply".into());
                }
                Err(error) => window.set_settings_status(error.into()),
            }
        }
    });

    let picker_hex_window = settings.as_weak();
    settings.on_color_picker_hex_edited(move |hex| {
        let Some(window) = picker_hex_window.upgrade() else {
            return;
        };
        if let Ok(parsed) = ArgbColor::parse(hex.trim()) {
            window.set_color_picker_a(i32::from(parsed.a));
            window.set_color_picker_r(i32::from(parsed.r));
            window.set_color_picker_g(i32::from(parsed.g));
            window.set_color_picker_b(i32::from(parsed.b));
        }
    });

    let open_picker_window = settings.as_weak();
    let open_picker_ctx = ctx.clone();
    settings.on_open_color_picker_requested(move |key, label| {
        let Some(window) = open_picker_window.upgrade() else {
            return;
        };
        let hex = key
            .rsplit_once('/')
            .and_then(|(group, leaf)| open_picker_ctx.config.effective_setting(group, leaf))
            .and_then(|v| match v {
                Value::Text(t) => Some(t),
                _ => None,
            })
            .unwrap_or_else(|| "#FFFFFFFF".to_owned());
        let parsed = ArgbColor::parse(&hex).unwrap_or_else(|_| ArgbColor::new(255, 255, 255, 255));
        window.set_color_picker_key(key);
        window.set_color_picker_label(label);
        window.set_color_picker_a(i32::from(parsed.a));
        window.set_color_picker_r(i32::from(parsed.r));
        window.set_color_picker_g(i32::from(parsed.g));
        window.set_color_picker_b(i32::from(parsed.b));
        window.set_color_picker_hex(parsed.to_argb_hex().into());
        window.set_color_picker_open(true);
    });

    // Keybind capture (in-place update + inline duplicate / reserved chord validation)
    let keybind_window = settings.as_weak();
    let keybind_ctx = ctx.clone();
    settings.on_keybind_captured(move |key, text, control, alt, shift, meta| {
        let Some(window) = keybind_window.upgrade() else {
            return;
        };
        let Some(chord) = format_key_chord(&text, control, alt, shift, meta) else {
            return;
        };
        keybind_ctx.snapshot_category_if_clean(SettingsCategory::Keybinds);
        match keybind_ctx.config.edit_setting(&key, &chord) {
            Ok(()) => {
                keybind_ctx.mark_dirty(SettingsCategory::Keybinds);
                sync_after_edit(&window, &keybind_ctx.config);
                let conflicts = keybind_ctx.config.keybind_conflict_count();
                if conflicts > 0 {
                    window.set_settings_status(
                        format!("Shortcut set to {chord} (warning: duplicate shortcut conflict)")
                            .into(),
                    );
                } else {
                    window.set_settings_status(
                        format!("Shortcut set to {chord} — click Apply").into(),
                    );
                }
            }
            Err(error) => {
                // Surface duplicate-chord or reserved `Ctrl+,`/`Cmd+,` rejection inline
                sync_after_edit(&window, &keybind_ctx.config);
                set_inline_field_conflict(&window, &key, &error);
                window.set_settings_status(error.into());
            }
        }
    });

    // Custom Text Replacements Editor (H6.5 — Settings -> Search)
    let tr_add_window = settings.as_weak();
    let tr_add_ctx = ctx.clone();
    settings.on_add_text_replacement_rule(move || {
        let Some(window) = tr_add_window.upgrade() else {
            return;
        };
        let mut rules = tr_add_ctx.config.text_replacement_options().rules;
        rules.push(TextReplacementRule {
            pattern: String::new(),
            replacement: String::new(),
            ignore_case: false,
            enabled: true,
        });
        match persist_text_replacement_rules_to_draft(&tr_add_ctx, &rules) {
            Ok(()) => {
                sync_text_replacements(&window, &tr_add_ctx.config);
                sync_after_edit(&window, &tr_add_ctx.config);
                window.set_settings_status("Unsaved changes — click Apply".into());
            }
            Err(err) => window.set_settings_status(err.into()),
        }
    });

    let tr_rm_window = settings.as_weak();
    let tr_rm_ctx = ctx.clone();
    settings.on_remove_text_replacement_rule(move |idx| {
        let Some(window) = tr_rm_window.upgrade() else {
            return;
        };
        let mut rules = tr_rm_ctx.config.text_replacement_options().rules;
        let Ok(index) = usize::try_from(idx) else {
            return;
        };
        if index >= rules.len() {
            return;
        }
        rules.remove(index);
        match persist_text_replacement_rules_to_draft(&tr_rm_ctx, &rules) {
            Ok(()) => {
                sync_text_replacements(&window, &tr_rm_ctx.config);
                sync_after_edit(&window, &tr_rm_ctx.config);
                window.set_settings_status("Unsaved changes — click Apply".into());
            }
            Err(err) => window.set_settings_status(err.into()),
        }
    });

    let tr_mv_window = settings.as_weak();
    let tr_mv_ctx = ctx.clone();
    settings.on_move_text_replacement_rule(move |idx, delta| {
        let Some(window) = tr_mv_window.upgrade() else {
            return;
        };
        let mut rules = tr_mv_ctx.config.text_replacement_options().rules;
        let Ok(from) = usize::try_from(idx) else {
            return;
        };
        let target = from as isize + delta.signum() as isize;
        if from >= rules.len() || target < 0 || target as usize >= rules.len() {
            return;
        }
        rules.swap(from, target as usize);
        match persist_text_replacement_rules_to_draft(&tr_mv_ctx, &rules) {
            Ok(()) => {
                sync_text_replacements(&window, &tr_mv_ctx.config);
                sync_after_edit(&window, &tr_mv_ctx.config);
                window.set_settings_status("Unsaved changes — click Apply".into());
            }
            Err(err) => window.set_settings_status(err.into()),
        }
    });

    let tr_upd_window = settings.as_weak();
    let tr_upd_ctx = ctx.clone();
    settings.on_update_text_replacement_rule(move |idx, pattern, replacement, ignore_case, enabled| {
        let Some(window) = tr_upd_window.upgrade() else {
            return;
        };
        let mut rules = tr_upd_ctx.config.text_replacement_options().rules;
        let Ok(index) = usize::try_from(idx) else {
            return;
        };
        if index >= rules.len() {
            return;
        }
        let pat_str = pattern.to_string();
        rules[index] = TextReplacementRule {
            pattern: pat_str.clone(),
            replacement: replacement.to_string(),
            ignore_case,
            enabled,
        };
        match persist_text_replacement_rules_to_draft(&tr_upd_ctx, &rules) {
            Ok(()) => {
                sync_text_replacements(&window, &tr_upd_ctx.config);
                sync_after_edit(&window, &tr_upd_ctx.config);
                match validate_text_replacement_regex(&pat_str) {
                    Ok(()) => window.set_settings_status("Unsaved changes — click Apply".into()),
                    Err(err) => window.set_settings_status(
                        format!("Rule {}: invalid regex — {err}", index + 1).into(),
                    ),
                }
            }
            Err(err) => window.set_settings_status(err.into()),
        }
    });

    let tr_orig_window = settings.as_weak();
    let tr_orig_ctx = ctx.clone();
    settings.on_text_replacements_search_original_changed(move |val| {
        let Some(window) = tr_orig_window.upgrade() else {
            return;
        };
        tr_orig_ctx.snapshot_category_if_clean(SettingsCategory::Search);
        match tr_orig_ctx.config.edit_setting(
            "search/text-replacements-search-original",
            if val { "true" } else { "false" },
        ) {
            Ok(()) => {
                tr_orig_ctx.mark_dirty(SettingsCategory::Search);
                sync_text_replacements(&window, &tr_orig_ctx.config);
                sync_after_edit(&window, &tr_orig_ctx.config);
                window.set_settings_status("Unsaved changes — click Apply".into());
            }
            Err(err) => window.set_settings_status(err.into()),
        }
    });

    // Profile Management QoL (inline validation, copy_current, rename, delete Default)
    let pname_window = settings.as_weak();
    let pname_ctx = ctx.clone();
    settings.on_profile_name_edited(move |category, name, is_rename| {
        let Some(window) = pname_window.upgrade() else {
            return;
        };
        let validation = match SettingsCategory::from_i32(category) {
            Some(SettingsCategory::Anki) => {
                let existing = pname_ctx.config.anki_profile_names();
                let current = pname_ctx.config.current_anki_profile();
                validate_profile_name(
                    &name,
                    &existing,
                    true,
                    is_rename.then_some(current.as_str()),
                )
            }
            Some(SettingsCategory::Keybinds) => {
                let existing = pname_ctx.config.keybind_profile_names();
                let current = pname_ctx.config.current_keybind_profile();
                validate_profile_name(
                    &name,
                    &existing,
                    false,
                    is_rename.then_some(current.as_str()),
                )
            }
            _ => Ok(name.to_string()),
        };
        match validation {
            Ok(_) => window.set_profile_name_error("".into()),
            Err(err) => window.set_profile_name_error(err.into()),
        }
    });

    let add_opts_window = settings.as_weak();
    let add_opts_ctx = ctx.clone();
    settings.on_add_profile_with_options(move |category, name, copy_current| {
        if let Some(cat) = SettingsCategory::from_i32(category) {
            add_opts_ctx.snapshot_category_if_clean(cat);
        }
        let res = add_opts_ctx
            .config
            .add_entry_with_options(category, &name, copy_current);
        if res.is_ok() {
            if let Some(cat) = SettingsCategory::from_i32(category) {
                add_opts_ctx.mark_dirty(cat);
            }
            if category == SettingsCategory::Anki.as_i32() {
                if let Some(win) = add_opts_window.upgrade() {
                    anki::refresh_anki_excluded_dicts(
                        &win,
                        add_opts_ctx.dictionary_manager.as_deref(),
                        &add_opts_ctx.config,
                    );
                }
            }
        }
        report_settings(&add_opts_window, &add_opts_ctx.config, res);
        true
    });

    let rename_window = settings.as_weak();
    let rename_ctx = ctx.clone();
    settings.on_rename_profile(move |category, old_name, new_name| {
        let Some(window) = rename_window.upgrade() else {
            return;
        };
        match SettingsCategory::from_i32(category) {
            Some(SettingsCategory::Anki) => {
                rename_ctx.snapshot_category_if_clean(SettingsCategory::Anki);
                let res = (|| -> Result<(), String> {
                    if rename_ctx.config.current_anki_profile() != old_name.as_str() {
                        rename_ctx.config.edit_setting("anki/profile", &old_name)?;
                    }
                    rename_ctx
                        .config
                        .edit_setting("anki/profile/name", &new_name)
                })();
                if res.is_ok() {
                    rename_ctx.mark_dirty(SettingsCategory::Anki);
                    refresh_settings(&window, &rename_ctx.config);
                    anki::refresh_anki_excluded_dicts(
                        &window,
                        rename_ctx.dictionary_manager.as_deref(),
                        &rename_ctx.config,
                    );
                    window.set_settings_status(
                        format!(
                            "Renamed Anki profile to {} — click Apply",
                            rename_ctx.config.current_anki_profile()
                        )
                        .into(),
                    );
                } else if let Err(err) = res {
                    window.set_settings_status(err.into());
                }
            }
            Some(SettingsCategory::Keybinds) => {
                rename_ctx.snapshot_category_if_clean(SettingsCategory::Keybinds);
                match rename_keybind_profile_in_config(&rename_ctx.config, &old_name, &new_name) {
                    Ok(renamed) => {
                        rename_ctx.mark_dirty(SettingsCategory::Keybinds);
                        refresh_settings(&window, &rename_ctx.config);
                        window.set_settings_status(
                            format!("Renamed keybind profile to {renamed} — click Apply").into(),
                        );
                    }
                    Err(err) => window.set_settings_status(err.into()),
                }
            }
            _ => {}
        }
    });

    // Apply / Reset / Defaults
    let apply_window = settings.as_weak();
    let apply_ctx = ctx.clone();
    settings.on_apply_settings(move |category| {
        let win = apply_window.upgrade();
        let searching = win
            .as_ref()
            .is_some_and(|w| !w.get_settings_search_query().trim().is_empty());
        let categories_to_apply: Vec<i32> = if searching {
            let dirty = apply_ctx.dirty_categories();
            if dirty.is_empty() {
                vec![category]
            } else {
                dirty.into_iter().map(SettingsCategory::as_i32).collect()
            }
        } else {
            vec![category]
        };

        let mut result = Ok(());
        for cat in categories_to_apply {
            if let Err(err) = apply_single_category(win.as_ref(), &apply_ctx, cat) {
                result = Err(err);
                break;
            }
        }
        report_settings(&apply_window, &apply_ctx.config, result);
    });

    let reset_window = settings.as_weak();
    let reset_ctx = ctx.clone();
    settings.on_reset_settings(move |category| {
        let result = reset_ctx.config.reset_settings(
            category,
            &reset_ctx.cache_dir,
            "memento.conf",
            &reset_ctx.anki_json_path,
        );
        if result.is_ok() {
            if let Some(cat) = SettingsCategory::from_i32(category) {
                reset_ctx.clear_dirty(cat);
                reset_ctx.clear_snapshot(cat);
            }
            if category == SettingsCategory::Search.as_i32() {
                reset_ctx
                    .dict_ctrl
                    .borrow_mut()
                    .set_text_replacements(reset_ctx.config.text_replacement_options());
            }
            if let Some(app) = reset_ctx.app_weak.upgrade() {
                refresh_player_preferences(&app, &reset_ctx.config);
                if category == SettingsCategory::Interface.as_i32() {
                    let is_light = effective_is_light_theme(&reset_ctx.config, None);
                    app.set_is_light_theme(is_light);
                }
                app.set_ocr_enabled(
                    reset_ctx.ocr_available
                        && setting_enabled(
                            &reset_ctx.config,
                            raw::ocr::GROUP,
                            raw::ocr::ENABLED,
                            raw::ocr::ENABLED_DEFAULT,
                        ),
                );
                app.set_asr_enabled(
                    controller::asr::AVAILABLE
                        && setting_enabled(
                            &reset_ctx.config,
                            raw::asr::GROUP,
                            raw::asr::ENABLED,
                            raw::asr::ENABLED_DEFAULT,
                        ),
                );
                if category == SettingsCategory::Asr.as_i32() {
                    if let Some(window) = reset_window.upgrade() {
                        asr_models::refresh_asr_model_status(
                            &window,
                            &reset_ctx.config,
                            &reset_ctx.cache_dir,
                        );
                    }
                    clear_asr_for_settings(
                        &app,
                        &reset_ctx.asr_session,
                        &reset_ctx.subtitles,
                        &reset_ctx.subtitle_model,
                        &reset_ctx.asr_restart_pending,
                        Some(&reset_ctx.mpv),
                        Some(&reset_ctx.coordinator),
                    );
                }
                if category == SettingsCategory::Interface.as_i32()
                    || category == SettingsCategory::Search.as_i32()
                {
                    refresh_subtitle_list(
                        &app,
                        &reset_ctx.subtitle_model,
                        &reset_ctx.subtitles.borrow(),
                    );
                }
            }
            if category == SettingsCategory::Dictionaries.as_i32()
                || category == SettingsCategory::Search.as_i32()
            {
                apply_dictionary_order(&reset_ctx.config, &mut reset_ctx.dict_ctrl.borrow_mut());
            }
            if let Some(window) = reset_window.upgrade() {
                if category == SettingsCategory::Dictionaries.as_i32() {
                    refresh_dictionary_settings(
                        &window,
                        reset_ctx.dictionary_manager.as_deref(),
                        &reset_ctx.config,
                    );
                } else if category == SettingsCategory::Interface.as_i32() {
                    refresh_custom_popup_css(&window, &reset_ctx.cache_dir);
                    sync_theme(&window, &reset_ctx, None);
                    sync_subtitle_preview(&window, &reset_ctx.config);
                } else if category == SettingsCategory::Search.as_i32() {
                    sync_text_replacements(&window, &reset_ctx.config);
                } else if category == SettingsCategory::Anki.as_i32() {
                    anki::refresh_anki_excluded_dicts(
                        &window,
                        reset_ctx.dictionary_manager.as_deref(),
                        &reset_ctx.config,
                    );
                }
            }
        }
        report_settings(&reset_window, &reset_ctx.config, result);
    });

    let defaults_window = settings.as_weak();
    let defaults_ctx = ctx.clone();
    settings.on_defaults_settings(move |category| {
        if let Some(cat) = SettingsCategory::from_i32(category) {
            defaults_ctx.snapshot_category_if_clean(cat);
        }
        let result = defaults_ctx.config.defaults_settings(category);
        if result.is_ok() {
            if let Some(cat) = SettingsCategory::from_i32(category) {
                defaults_ctx.mark_dirty(cat);
            }
            if let Some(window) = defaults_window.upgrade() {
                if category == SettingsCategory::Interface.as_i32() {
                    sync_theme(&window, &defaults_ctx, None);
                    sync_subtitle_preview(&window, &defaults_ctx.config);
                } else if category == SettingsCategory::Search.as_i32() {
                    sync_text_replacements(&window, &defaults_ctx.config);
                } else if category == SettingsCategory::Anki.as_i32() {
                    anki::refresh_anki_excluded_dicts(
                        &window,
                        defaults_ctx.dictionary_manager.as_deref(),
                        &defaults_ctx.config,
                    );
                } else if category == SettingsCategory::Asr.as_i32() {
                    asr_models::refresh_asr_model_status(
                        &window,
                        &defaults_ctx.config,
                        &defaults_ctx.cache_dir,
                    );
                }
            }
            if category == SettingsCategory::Asr.as_i32() {
                if let Some(app) = defaults_ctx.app_weak.upgrade() {
                    app.set_asr_enabled(
                        controller::asr::AVAILABLE
                            && setting_enabled(
                                &defaults_ctx.config,
                                raw::asr::GROUP,
                                raw::asr::ENABLED,
                                raw::asr::ENABLED_DEFAULT,
                            ),
                    );
                    clear_asr_for_settings(
                        &app,
                        &defaults_ctx.asr_session,
                        &defaults_ctx.subtitles,
                        &defaults_ctx.subtitle_model,
                        &defaults_ctx.asr_restart_pending,
                        Some(&defaults_ctx.mpv),
                        Some(&defaults_ctx.coordinator),
                    );
                }
            }
        }
        report_settings(&defaults_window, &defaults_ctx.config, result);
    });

    // Entry add / remove / reorder (profiles & audio sources)
    let add_window = settings.as_weak();
    let add_ctx = ctx.clone();
    settings.on_add_entry(move |category, name| {
        if let Some(cat) = SettingsCategory::from_i32(category) {
            add_ctx.snapshot_category_if_clean(cat);
        }
        let res = add_ctx.config.add_entry(category, &name);
        if res.is_ok() {
            if let Some(cat) = SettingsCategory::from_i32(category) {
                add_ctx.mark_dirty(cat);
            }
            if category == SettingsCategory::Anki.as_i32() {
                if let Some(win) = add_window.upgrade() {
                    anki::refresh_anki_excluded_dicts(
                        &win,
                        add_ctx.dictionary_manager.as_deref(),
                        &add_ctx.config,
                    );
                }
            }
        }
        report_settings(&add_window, &add_ctx.config, res);
    });

    let remove_window = settings.as_weak();
    let remove_ctx = ctx.clone();
    settings.on_remove_entry(move |category, name| {
        if let Some(cat) = SettingsCategory::from_i32(category) {
            remove_ctx.snapshot_category_if_clean(cat);
        }
        let res = remove_ctx.config.remove_entry(category, &name);
        if res.is_ok() {
            if let Some(cat) = SettingsCategory::from_i32(category) {
                remove_ctx.mark_dirty(cat);
            }
            if category == SettingsCategory::Anki.as_i32() {
                if let Some(win) = remove_window.upgrade() {
                    anki::refresh_anki_excluded_dicts(
                        &win,
                        remove_ctx.dictionary_manager.as_deref(),
                        &remove_ctx.config,
                    );
                }
            }
        }
        report_settings(&remove_window, &remove_ctx.config, res);
    });

    let move_window = settings.as_weak();
    let move_ctx = ctx.clone();
    settings.on_move_entry(move |category, name, direction| {
        if let Some(cat) = SettingsCategory::from_i32(category) {
            move_ctx.snapshot_category_if_clean(cat);
        }
        let res = move_ctx.config.move_entry(category, &name, direction);
        if res.is_ok() {
            if let Some(cat) = SettingsCategory::from_i32(category) {
                move_ctx.mark_dirty(cat);
            }
        }
        report_settings(&move_window, &move_ctx.config, res);
    });

    // Delegate domain callbacks to focused submodules
    custom_css::wire(&settings, &ctx);
    let dict_drop = dictionaries::wire(&settings, &ctx);
    asr_models::wire(&settings, &ctx);
    anki::wire(&settings, &ctx);

    // Unified winit WindowEvent hook on SettingsWindow:
    // - `ThemeChanged`: live OS Light/Dark sync across SettingsWindow + AnkiHelpWindow
    // - `HoveredFile` / `HoveredFileCancelled` / `DroppedFile`: drag-and-drop batch ZIP import
    // - `Focused(true)`: kick off silent Anki decks/models auto-fetch on reopen
    let winit_window = settings.as_weak();
    let winit_ctx = ctx.clone();
    settings
        .window()
        .on_winit_window_event(move |_winit_win, event| {
            let Some(win) = winit_window.upgrade() else {
                return EventResult::Propagate;
            };
            match event {
                WindowEvent::ThemeChanged(winit_theme) => {
                    let scheme = match winit_theme {
                        WinitTheme::Light => SystemColorScheme::Light,
                        WinitTheme::Dark => SystemColorScheme::Dark,
                    };
                    sync_theme(&win, &winit_ctx, Some(scheme));
                }
                WindowEvent::HoveredFile(path) => {
                    dict_drop.on_hovered_file(&win, path);
                }
                WindowEvent::HoveredFileCancelled => {
                    dict_drop.on_hovered_file_cancelled(&win);
                }
                WindowEvent::DroppedFile(path) => {
                    dict_drop.on_dropped_file(&win, path.clone());
                }
                WindowEvent::Focused(true) => {
                    if win.get_active_category() == SettingsCategory::Anki.as_i32() {
                        anki::maybe_auto_fetch_decks_and_models(&win, &winit_ctx);
                    }
                }
                _ => {}
            }
            EventResult::Propagate
        });

    // Auto-fetch Anki decks & models on initial open if starting on Anki category
    if settings.get_active_category() == SettingsCategory::Anki.as_i32() {
        anki::maybe_auto_fetch_decks_and_models(&settings, &ctx);
    }

    Ok(SettingsSession { window: settings })
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::path::Path;
    use std::rc::Rc;

    use memento_config::Value;
    use memento_os::SystemColorScheme;
    use memento_setting::enums::ThemeMode;
    use memento_setting::keys as raw;
    use slint::{Model as _, VecModel};

    use super::{
        create_settings_session, filtered_settings_groups, format_key_chord,
        refresh_category_settings, resolve_is_light_theme, sync_after_edit,
        validate_text_replacement_regex, SettingsCategory, SettingsSession,
    };
    use crate::controller::asr::AsrController;
    use crate::controller::config::ConfigController;
    use crate::controller::dict::DictController;
    use crate::controller::subtitles::SubtitleController;
    use crate::{InstalledDictionaryRow, MementoApp, SettingsWindow, SubEntry};

    fn make_headless_session(
        config: Rc<ConfigController>,
        dir: &Path,
    ) -> (MementoApp, SettingsSession) {
        i_slint_backend_testing::init_no_event_loop();
        let app = MementoApp::new().unwrap();
        let dict_ctrl = Rc::new(RefCell::new(DictController::new()));
        let asr_session = Rc::new(RefCell::new(AsrController::new()));
        let subtitles = Rc::new(RefCell::new(SubtitleController::new()));
        let subtitle_model = Rc::new(VecModel::<SubEntry>::default());
        let asr_restart = Rc::new(Cell::new(false));
        let mpv = Rc::new(RefCell::new(None));
        let coord = Rc::new(RefCell::new(None));
        let session = create_settings_session(
            &app,
            config,
            dict_ctrl,
            None,
            dir.join("dict.sqlite"),
            dir.to_path_buf(),
            dir.join("anki_connect.json"),
            false,
            asr_session,
            subtitles,
            subtitle_model,
            asr_restart,
            mpv,
            coord,
        )
        .unwrap();
        (app, session)
    }

    #[test]
    fn key_chord_formats_modifiers_letters_and_special_keys() {
        assert_eq!(
            format_key_chord("c", true, false, true, false),
            Some("Ctrl+Shift+C".to_owned())
        );
        assert_eq!(
            format_key_chord("\u{0003}", true, false, false, false),
            Some("Ctrl+C".to_owned())
        );
        assert_eq!(
            format_key_chord("\u{F706}", false, false, false, false),
            Some("F3".to_owned())
        );
        assert_eq!(
            format_key_chord("\u{F701}", true, false, false, false),
            Some("Ctrl+Down".to_owned())
        );
        assert_eq!(
            format_key_chord("\u{007f}", false, false, false, false),
            Some("Delete".to_owned())
        );
        // Standalone left and right modifier keys must be ignored
        assert_eq!(format_key_chord("\u{0011}", true, false, false, false), None);
        assert_eq!(format_key_chord("\u{0015}", false, false, true, false), None);
        assert_eq!(format_key_chord("\u{0016}", true, false, false, false), None);
        assert_eq!(format_key_chord("\u{0018}", false, false, false, true), None);
        assert_eq!(format_key_chord("", false, false, false, false), None);
    }

    #[test]
    fn settings_category_maps_indices_and_key_prefixes_including_subtitle_list() {
        for idx in 0..SettingsCategory::COUNT as i32 {
            let cat = SettingsCategory::from_i32(idx).expect("valid category");
            assert_eq!(cat.as_i32(), idx);
        }
        assert_eq!(SettingsCategory::from_i32(99), None);
        assert_eq!(
            SettingsCategory::from_setting_key("whisper/model"),
            Some(SettingsCategory::Asr)
        );
        assert_eq!(
            SettingsCategory::from_setting_key("interface/sub-font"),
            Some(SettingsCategory::Interface)
        );
        assert_eq!(
            SettingsCategory::from_setting_key("subtitle-list/ignore-whitespace"),
            Some(SettingsCategory::Interface)
        );
        assert_eq!(
            SettingsCategory::from_setting_key("subtitle-list/auto-seek"),
            Some(SettingsCategory::Interface)
        );
        assert_eq!(
            SettingsCategory::from_setting_key("anki/profile/show-tag"),
            Some(SettingsCategory::Anki)
        );
    }

    #[test]
    fn editing_subtitle_list_via_settings_search_marks_interface_dirty() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = Rc::new(ConfigController::new());
        let (_app, session) = make_headless_session(cfg, dir.path());
        let win = &session.window;

        // Active category is Behavior (3), user searches for "auto-seek" and toggles it.
        win.set_active_category(SettingsCategory::Behavior.as_i32());
        win.set_settings_search_query("auto-seek".into());
        win.invoke_settings_search_changed("auto-seek".into());
        win.invoke_setting_edited("subtitle-list/auto-seek".into(), "true".into());

        let dirty = win.get_dirty_categories();
        assert_eq!(
            dirty.row_data(SettingsCategory::Interface.as_usize()),
            Some(true),
            "subtitle-list/* edits must mark Interface dirty"
        );
        assert_eq!(
            dirty.row_data(SettingsCategory::Behavior.as_usize()),
            Some(false),
            "unrelated active category Behavior must NOT be marked dirty"
        );
    }

    #[test]
    fn close_without_apply_reverts_unsaved_draft_edits_across_all_categories() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = Rc::new(ConfigController::new());
        cfg.edit_setting("interface/theme", "dark").unwrap();
        cfg.apply_settings(
            SettingsCategory::Interface.as_i32(),
            dir.path(),
            "memento.conf",
            &dir.path().join("anki_connect.json"),
        )
        .unwrap();

        let (_app, session) = make_headless_session(cfg.clone(), dir.path());
        let win = &session.window;

        // 1. Edit Behavior and Apply it (this MUST persist after close)
        win.invoke_setting_edited("behavior/autofit".into(), "false".into());
        win.invoke_apply_settings(SettingsCategory::Behavior.as_i32());
        assert_eq!(
            cfg.effective_setting(raw::behavior::GROUP, raw::behavior::AUTO_FIT),
            Some(Value::Bool(false))
        );

        // 2. Make UNSAVED edits across Behavior, Interface (theme), Search (text replacements),
        //    Audio Sources, Keybinds (new profile), and Anki (profile + port + ex-glos)
        win.invoke_setting_edited("behavior/osc-duration".into(), "1234".into());
        win.invoke_setting_edited("interface/theme".into(), "Light".into());
        assert!(
            win.get_is_light_theme(),
            "live theme preview should flip immediately"
        );
        win.invoke_add_text_replacement_rule();
        win.invoke_update_text_replacement_rule(0, "ー".into(), "".into(), false, true);
        win.invoke_add_entry(SettingsCategory::AudioSources.as_i32(), "".into());
        assert!(win.invoke_add_profile_with_options(
            SettingsCategory::Keybinds.as_i32(),
            "UnsavedKB".into(),
            false
        ));
        assert!(win.invoke_add_profile_with_options(
            SettingsCategory::Anki.as_i32(),
            "UnsavedAnki".into(),
            false
        ));
        win.invoke_setting_edited("anki/profile/port".into(), "9999".into());
        win.invoke_anki_excluded_dict_toggled("JMdict".into(), true);

        // Verify dirty indicators are set before close
        assert_eq!(
            win.get_dirty_categories()
                .row_data(SettingsCategory::Interface.as_usize()),
            Some(true)
        );
        assert_eq!(
            win.get_dirty_categories()
                .row_data(SettingsCategory::Anki.as_usize()),
            Some(true)
        );

        // 3. Close WITHOUT clicking Apply!
        win.invoke_close();

        // Applied Behavior edit MUST remain!
        assert_eq!(
            cfg.effective_setting(raw::behavior::GROUP, raw::behavior::AUTO_FIT),
            Some(Value::Bool(false))
        );
        // ALL unsaved edits MUST be reverted!
        assert_eq!(
            cfg.effective_setting(raw::behavior::GROUP, raw::behavior::OSC_DURATION),
            Some(Value::Int(raw::behavior::OSC_DURATION_DEFAULT as i64))
        );
        assert_eq!(cfg.theme_mode(), ThemeMode::Dark);
        assert!(
            !win.get_is_light_theme(),
            "theme must revert to Dark on close without Apply"
        );
        assert!(cfg.text_replacement_options().rules.is_empty());
        assert_eq!(cfg.audio_source_entries().len(), 1);
        assert_eq!(cfg.keybind_profile_names(), vec!["Default"]);
        assert_eq!(cfg.current_keybind_profile(), "Default");
        assert_eq!(cfg.anki_profile_names(), vec!["Default"]);
        assert_eq!(cfg.current_anki_profile(), "Default");
        assert_eq!(cfg.anki_endpoint().port, 8765);
        assert!(cfg.anki_excluded_glossaries().is_empty());
        // All dirty indicators cleared
        for idx in 0..SettingsCategory::COUNT {
            assert_eq!(win.get_dirty_categories().row_data(idx), Some(false));
        }
    }

    #[test]
    fn theme_sync_and_live_subtitle_preview_update_dynamically() {
        assert!(resolve_is_light_theme(ThemeMode::Light, SystemColorScheme::Dark));
        assert!(!resolve_is_light_theme(ThemeMode::Dark, SystemColorScheme::Light));
        assert!(resolve_is_light_theme(ThemeMode::System, SystemColorScheme::Light));
        assert!(!resolve_is_light_theme(ThemeMode::System, SystemColorScheme::Dark));
        assert!(!resolve_is_light_theme(ThemeMode::System, SystemColorScheme::Unknown));

        let dir = tempfile::tempdir().unwrap();
        let cfg = Rc::new(ConfigController::new());
        let (_app, session) = make_headless_session(cfg, dir.path());
        let win = &session.window;

        win.invoke_setting_edited("interface/theme".into(), "Light".into());
        assert!(win.get_is_light_theme());
        // Opening Anki Help window inherits light theme and stays synced on theme changes
        win.invoke_anki_help_requested();
        win.invoke_setting_edited("interface/theme".into(), "Dark".into());
        assert!(!win.get_is_light_theme());

        // Live subtitle preview updates on font, size, bold, italic, colors, stroke, furigana
        win.invoke_font_edited(
            "interface/sub-font".into(),
            "Source Han Sans JP".into(),
            "32".into(),
            true,
            true,
        );
        assert_eq!(win.get_preview_sub_font_family().as_str(), "Source Han Sans JP");
        assert_eq!(win.get_preview_sub_font_size(), 32.0);
        assert!(win.get_preview_sub_font_bold());
        assert!(win.get_preview_sub_font_italic());

        win.invoke_color_edited("interface/sub-text-color".into(), 255, 255, 224, 102);
        assert_eq!(
            win.get_preview_sub_text_color(),
            slint::Color::from_argb_u8(255, 255, 224, 102)
        );
        win.invoke_setting_edited("interface/sub-stroke".into(), "25".into());
        assert!((win.get_preview_sub_stroke_width() - 2.5).abs() < 0.01);
        win.invoke_setting_edited("interface/sub-furigana-ratio".into(), "0.6".into());
        assert!((win.get_preview_sub_furigana_ratio() - 0.6).abs() < 0.01);
    }

    #[test]
    fn text_replacements_editor_crud_ordering_and_regex_validation() {
        assert!(validate_text_replacement_regex("（[^）]*）").is_ok());
        assert!(validate_text_replacement_regex("[unclosed").is_err());

        let dir = tempfile::tempdir().unwrap();
        let cfg = Rc::new(ConfigController::new());
        let (_app, session) = make_headless_session(cfg.clone(), dir.path());
        let win = &session.window;

        win.invoke_add_text_replacement_rule();
        win.invoke_add_text_replacement_rule();
        assert_eq!(win.get_text_replacement_rows().row_count(), 2);

        win.invoke_update_text_replacement_rule(0, "foo".into(), "bar".into(), true, true);
        win.invoke_update_text_replacement_rule(1, "[bad".into(), "".into(), false, true);
        assert!(
            win.get_settings_status().as_str().contains("invalid regex"),
            "invalid regex should surface clear status feedback"
        );

        // Apply blocks on enabled invalid regex
        win.invoke_apply_settings(SettingsCategory::Search.as_i32());
        assert!(win.get_settings_status().as_str().contains("invalid regex"));

        // Fix regex, reorder, toggle search_original, and Apply cleanly
        win.invoke_update_text_replacement_rule(1, "baz".into(), "qux".into(), false, true);
        win.invoke_move_text_replacement_rule(1, -1);
        win.invoke_text_replacements_search_original_changed(false);
        assert_eq!(
            win.get_text_replacement_rows().row_data(0).unwrap().pattern,
            "baz"
        );
        assert_eq!(
            win.get_text_replacement_rows().row_data(1).unwrap().pattern,
            "foo"
        );
        assert!(!win.get_text_replacements_search_original());

        win.invoke_remove_text_replacement_rule(1);
        assert_eq!(win.get_text_replacement_rows().row_count(), 1);
        win.invoke_apply_settings(SettingsCategory::Search.as_i32());
        let saved = cfg.text_replacement_options();
        assert_eq!(saved.rules.len(), 1);
        assert_eq!(saved.rules[0].pattern, "baz");
        assert!(!saved.search_original);
    }

    #[test]
    fn anki_ex_glos_checklist_toggle_and_autocomplete_wiring_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = Rc::new(ConfigController::new());
        let (_app, session) = make_headless_session(cfg.clone(), dir.path());
        let win = &session.window;

        // Populate installed dictionaries model to simulate installed Yomitan dicts
        win.set_installed_dictionaries(
            Rc::new(VecModel::from(vec![
                InstalledDictionaryRow {
                    id: 1,
                    title: "JMdict".into(),
                    disabled: false,
                },
                InstalledDictionaryRow {
                    id: 2,
                    title: "Daijirin".into(),
                    disabled: false,
                },
            ]))
            .into(),
        );
        win.invoke_setting_edited("anki/profile/ex-glos".into(), "LegacyDict".into());
        assert_eq!(win.get_anki_excluded_dicts().row_count(), 3);
        let legacy_row = win.get_anki_excluded_dicts().row_data(2).unwrap();
        assert_eq!(legacy_row.name, "LegacyDict");
        assert!(legacy_row.excluded);
        assert!(!legacy_row.installed);

        // Toggle JMdict excluded on & off
        win.invoke_anki_excluded_dict_toggled("JMdict".into(), true);
        assert!(cfg.anki_excluded_glossaries().iter().any(|d| d == "JMdict"));
        let jmdict_row = win.get_anki_excluded_dicts().row_data(0).unwrap();
        assert!(jmdict_row.excluded);
        assert!(jmdict_row.installed);

        win.invoke_anki_excluded_dict_toggled("JMdict".into(), false);
        assert!(!cfg.anki_excluded_glossaries().iter().any(|d| d == "JMdict"));

        // Autocomplete query + accept splices marker into Anki field
        cfg.set_anki_model_fields("term", &["Expression".to_owned()])
            .unwrap();
        refresh_category_settings(win, &cfg, SettingsCategory::Anki.as_i32());
        let field_key = "anki/profile/term/field/Expression";
        win.invoke_anki_field_autocomplete_query(field_key.into(), "{fur".into(), 4);
        assert!(win.get_anki_autocomplete_open());
        assert!(win.get_anki_autocomplete_items().row_count() >= 2);
        win.invoke_anki_field_autocomplete_accepted(field_key.into(), "furigana-plain".into());
        assert!(!win.get_anki_autocomplete_open());
        let note = cfg.anki_profile_note_config().unwrap();
        assert_eq!(
            note.field_templates.get("Expression").map(String::as_str),
            Some("{furigana-plain}")
        );
    }

    #[test]
    fn profile_qol_and_keybind_inline_validation_work_end_to_end() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = Rc::new(ConfigController::new());
        let (_app, session) = make_headless_session(cfg.clone(), dir.path());
        let win = &session.window;

        // Inline duplicate/empty profile validation
        win.invoke_profile_name_edited(SettingsCategory::Anki.as_i32(), "default".into(), false);
        assert!(!win.get_profile_name_error().is_empty());
        win.invoke_profile_name_edited(SettingsCategory::Anki.as_i32(), "Default".into(), true);
        assert!(win.get_profile_name_error().is_empty());
        win.invoke_profile_name_edited(SettingsCategory::Keybinds.as_i32(), "bad/name".into(), false);
        assert!(!win.get_profile_name_error().is_empty());

        // Copy current = false creates fresh Anki profile; rename works; delete Default resets cleanly
        win.invoke_setting_edited("anki/profile/port".into(), "9123".into());
        assert!(win.invoke_add_profile_with_options(
            SettingsCategory::Anki.as_i32(),
            "Fresh".into(),
            false
        ));
        assert_eq!(cfg.current_anki_profile(), "Fresh");
        assert_eq!(cfg.anki_endpoint().port, 8765);
        win.invoke_rename_profile(
            SettingsCategory::Anki.as_i32(),
            "Fresh".into(),
            "Mining".into(),
        );
        assert_eq!(cfg.current_anki_profile(), "Mining");
        win.invoke_remove_entry(SettingsCategory::Anki.as_i32(), "Default".into());
        win.invoke_remove_entry(SettingsCategory::Anki.as_i32(), "Mining".into());
        assert_eq!(cfg.anki_profile_names(), vec!["Default"]);
        assert_eq!(cfg.anki_endpoint().port, 8765);

        // Keybind profile copy_current=false, rename, delete Default, and inline conflict error
        win.set_active_category(SettingsCategory::Keybinds.as_i32());
        refresh_category_settings(win, &cfg, SettingsCategory::Keybinds.as_i32());
        assert!(win.invoke_add_profile_with_options(
            SettingsCategory::Keybinds.as_i32(),
            "CustomKB".into(),
            false
        ));
        win.invoke_rename_profile(
            SettingsCategory::Keybinds.as_i32(),
            "CustomKB".into(),
            "RenamedKB".into(),
        );
        assert_eq!(cfg.current_keybind_profile(), "RenamedKB");

        // Reserved `Ctrl+,` surfaces inline error on field.conflict
        let open_key = "keybind/profiles/RenamedKB/open-file";
        win.invoke_keybind_captured(open_key.into(), ",".into(), true, false, false, false);
        let read_conflict = |w: &SettingsWindow, target: &str| -> String {
            let kb_groups = w.get_settings_groups();
            let shortcuts = kb_groups.row_data(1).unwrap();
            (0..shortcuts.fields.row_count())
                .filter_map(|i| shortcuts.fields.row_data(i))
                .find(|f| f.key == target)
                .map(|f| f.conflict.to_string())
                .unwrap()
        };
        assert!(
            read_conflict(win, open_key).contains("reserved for Settings"),
            "expected reserved inline conflict error, got: {:?}",
            read_conflict(win, open_key)
        );

        // Duplicate chord (`Ctrl+Shift+C` / `Meta+Shift+C` already used by copy-selected) surfaces inline error
        let (use_ctrl, use_meta) = if cfg!(target_os = "macos") {
            (false, true)
        } else {
            (true, false)
        };
        win.invoke_keybind_captured(open_key.into(), "c".into(), use_ctrl, false, true, use_meta);
        assert!(
            read_conflict(win, open_key).contains("already assigned"),
            "expected duplicate inline conflict error, got: {:?}",
            read_conflict(win, open_key)
        );

        // Start OCR hidden when `!cfg!(feature = "ocr")`
        let kb_groups = win.get_settings_groups();
        let shortcuts = kb_groups.row_data(1).unwrap();
        let has_start_ocr = (0..shortcuts.fields.row_count())
            .filter_map(|i| shortcuts.fields.row_data(i))
            .any(|f| f.key.ends_with("/start-ocr"));
        assert_eq!(has_start_ocr, cfg!(feature = "ocr"));

        // Deleting Default and last remaining keybind profile resets cleanly to Default
        win.invoke_remove_entry(SettingsCategory::Keybinds.as_i32(), "Default".into());
        win.invoke_remove_entry(SettingsCategory::Keybinds.as_i32(), "RenamedKB".into());
        assert_eq!(cfg.keybind_profile_names(), vec!["Default"]);
        assert_eq!(cfg.current_keybind_profile(), "Default");
    }

    #[test]
    fn search_max_recursion_depth_refreshes_in_settings_window_across_apply_reset_defaults() {
        i_slint_backend_testing::init_no_event_loop();
        let dir = tempfile::tempdir().unwrap();
        let anki = dir.path().join("anki_connect.json");
        let cfg = ConfigController::new();
        let win = SettingsWindow::new().unwrap();

        let read_depth_field = |w: &SettingsWindow| -> (String, String, String) {
            let groups = w.get_settings_groups();
            for gi in 0..groups.row_count() {
                let g = groups.row_data(gi).unwrap();
                if g.title == "Pop-up" {
                    for fi in 0..g.fields.row_count() {
                        let f = g.fields.row_data(fi).unwrap();
                        if f.key == "search/max-recursion-depth" {
                            return (
                                f.label.to_string(),
                                f.description.to_string(),
                                f.value.to_string(),
                            );
                        }
                    }
                }
            }
            panic!("search/max-recursion-depth missing from Search -> Pop-up group");
        };

        refresh_category_settings(&win, &cfg, 9);
        let (label, desc, val) = read_depth_field(&win);
        assert_eq!(label, "Maximum child popups (recursive search)");
        assert_eq!(
            desc,
            "Max depth of nested popups when scanning inside definitions (0 disables)"
        );
        assert_eq!(val, "4");

        cfg.edit_setting("search/max-recursion-depth", "8").unwrap();
        cfg.apply_settings(9, dir.path(), "memento.conf", &anki)
            .unwrap();
        refresh_category_settings(&win, &cfg, 9);
        assert_eq!(read_depth_field(&win).2, "8");

        cfg.edit_setting("search/max-recursion-depth", "1").unwrap();
        cfg.reset_settings(9, dir.path(), "memento.conf", &anki)
            .unwrap();
        refresh_category_settings(&win, &cfg, 9);
        assert_eq!(read_depth_field(&win).2, "8");

        cfg.defaults_settings(9).unwrap();
        refresh_category_settings(&win, &cfg, 9);
        assert_eq!(read_depth_field(&win).2, "4");
    }

    #[test]
    fn in_place_sync_preserves_settings_groups_model_identity_on_toggle() {
        i_slint_backend_testing::init_no_event_loop();
        let cfg = ConfigController::new();
        let win = SettingsWindow::new().unwrap();
        win.set_active_category(SettingsCategory::Application.as_i32());
        refresh_category_settings(&win, &cfg, SettingsCategory::Application.as_i32());

        let before_groups = win.get_settings_groups();
        let before_ptr = before_groups
            .as_any()
            .downcast_ref::<VecModel<crate::ui::SettingsGroup>>()
            .unwrap() as *const _;

        cfg.edit_setting("application/auto-update-check", "false")
            .unwrap();
        sync_after_edit(&win, &cfg);

        let after_groups = win.get_settings_groups();
        let after_ptr = after_groups
            .as_any()
            .downcast_ref::<VecModel<crate::ui::SettingsGroup>>()
            .unwrap() as *const _;
        assert_eq!(
            before_ptr, after_ptr,
            "in-place edit must NOT replace the settings_groups VecModel"
        );
        let update_group = after_groups.row_data(1).unwrap();
        assert_eq!(update_group.fields.row_data(0).unwrap().value, "false");

        let matched = filtered_settings_groups(&cfg, "recursion", false, false);
        assert!(!matched.is_empty());
    }
}

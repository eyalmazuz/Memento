//! Settings window session, in-place field updates, quick search, and category callbacks.

mod anki;
mod asr_models;
mod custom_css;
mod dictionaries;
mod fonts;

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;

use memento_config::{ArgbColor, Value};
use memento_player::MpvInstance;
use memento_setting::keys as raw;
use slint::{ComponentHandle, Model as _, SharedString, VecModel};

use crate::controller;
use crate::controller::config::ConfigController;
use crate::controller::dict::DictController;
use crate::controller::dictmanager::DictionaryManager;
use crate::controller::player::{refresh_subtitle_list, LivePlayerCoordinator};
use crate::controller::subtitles::SubtitleController;
use crate::ui::{SettingsField, SettingsGroup};
use crate::{
    apply_dictionary_order, memento_xdg_dir, refresh_player_preferences, setting_enabled,
    MementoApp, SettingsWindow, SubEntry,
};

pub(crate) use asr_models::clear_asr_for_settings;
pub(crate) use custom_css::refresh_custom_popup_css;
pub(crate) use dictionaries::refresh_dictionary_settings;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
            "interface" => Some(Self::Interface),
            "keybind" => Some(Self::Keybinds),
            "ocr" => Some(Self::Ocr),
            "asr" | "whisper" | "onnx" => Some(Self::Asr),
            "search" => Some(Self::Search),
            _ => None,
        }
    }
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
}

impl SettingsContext {
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

    pub(crate) fn dirty_categories(&self) -> Vec<SettingsCategory> {
        (0..SettingsCategory::COUNT)
            .filter(|&idx| self.dirty_model.row_data(idx) == Some(true))
            .filter_map(|idx| SettingsCategory::from_i32(idx as i32))
            .collect()
    }
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
        return;
    }

    if category == SettingsCategory::Interface.as_i32() {
        fonts::ensure_font_families_loaded(window, config);
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
fn sync_after_edit(window: &SettingsWindow, config: &ConfigController) {
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
    if category == SettingsCategory::Interface.as_i32() {
        if let Some(win) = window {
            custom_css::save_custom_css_on_apply(win, &ctx.cache_dir)?;
        }
    }
    ctx.config
        .apply_settings(category, &ctx.cache_dir, "memento.conf", &ctx.anki_json_path)?;
    if let Some(cat) = SettingsCategory::from_i32(category) {
        ctx.clear_dirty(cat);
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
    });

    settings.set_ocr_built(cfg!(feature = "ocr"));
    settings.set_ocr_available(ctx.ocr_available);
    settings.set_asr_available(controller::asr::AVAILABLE);
    refresh_settings(&settings, &ctx.config);
    refresh_custom_popup_css(&settings, &ctx.cache_dir);
    refresh_dictionary_settings(&settings, ctx.dictionary_manager.as_deref(), &ctx.config);
    asr_models::refresh_asr_model_status(&settings, &ctx.config, &ctx.cache_dir);

    // Close / Hide handling
    let close_req_window = settings.as_weak();
    settings.window().on_close_requested(move || {
        if let Some(win) = close_req_window.upgrade() {
            let _ = win.hide();
        }
        slint::CloseRequestResponse::HideWindow
    });
    let close_btn_window = settings.as_weak();
    settings.on_close(move || {
        if let Some(win) = close_btn_window.upgrade() {
            let _ = win.hide();
        }
    });

    // Category navigation & quick search
    let cat_window = settings.as_weak();
    let cat_ctx = ctx.clone();
    settings.on_category_changed(move |category| {
        if let Some(window) = cat_window.upgrade() {
            refresh_category_settings(&window, &cat_ctx.config, category);
            if category == SettingsCategory::Dictionaries.as_i32() {
                refresh_dictionary_settings(
                    &window,
                    cat_ctx.dictionary_manager.as_deref(),
                    &cat_ctx.config,
                );
            } else if category == SettingsCategory::Interface.as_i32() {
                refresh_custom_popup_css(&window, &cat_ctx.cache_dir);
            } else if category == SettingsCategory::Asr.as_i32() {
                asr_models::refresh_asr_model_status(&window, &cat_ctx.config, &cat_ctx.cache_dir);
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

    // Setting edits (in-place row mutation preserves MementoSwitch 120ms animation)
    let edited_window = settings.as_weak();
    let edited_ctx = ctx.clone();
    settings.on_setting_edited(move |key, value| {
        let Some(window) = edited_window.upgrade() else {
            return;
        };
        match edited_ctx.config.edit_setting(&key, &value) {
            Ok(()) => {
                if let Some(cat) = SettingsCategory::from_setting_key(&key)
                    .or_else(|| SettingsCategory::from_i32(window.get_active_category()))
                {
                    edited_ctx.mark_dirty(cat);
                }
                let needs_sync = matches!(value.as_str(), "true" | "false")
                    || matches!(
                        key.as_str(),
                        "anki/profile"
                            | "keybind/current"
                            | "behavior/file-open-dir"
                            | "asr/backend"
                            | "whisper/model"
                            | "onnx/model"
                    )
                    || key.ends_with("/deck")
                    || key.ends_with("/model")
                    || key.starts_with("keybind/profiles/");
                if needs_sync {
                    sync_after_edit(&window, &edited_ctx.config);
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
                        | "whisper/vad-enable"
                        | "whisper/vad-model"
                        | "onnx/model"
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
            Err(error) => window.set_settings_status(error.into()),
        }
    });

    // Font editing (in-place update, incremental custom font insertion)
    let font_window = settings.as_weak();
    let font_ctx = ctx.clone();
    settings.on_font_edited(move |key, family, size, bold, italic| {
        if let Some(window) = font_window.upgrade() {
            match font_ctx
                .config
                .edit_font_setting(&key, &family, &size, bold, italic)
            {
                Ok(()) => {
                    font_ctx.mark_dirty(SettingsCategory::Interface);
                    fonts::ensure_font_family_listed(&window, &family);
                    sync_after_edit(&window, &font_ctx.config);
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
            match font_typed_ctx
                .config
                .edit_font_setting(&key, &family, &size, bold, italic)
            {
                Ok(()) => {
                    font_typed_ctx.mark_dirty(SettingsCategory::Interface);
                    window.set_settings_status("Unsaved changes — click Apply".into());
                }
                Err(error) => window.set_settings_status(error.into()),
            }
        }
    });

    // Color picker & hex editing (in-place update, never re-sorts fonts)
    let color_window = settings.as_weak();
    let color_ctx = ctx.clone();
    settings.on_color_edited(move |key, a, r, g, b| {
        if let Some(window) = color_window.upgrade() {
            match color_ctx.config.edit_color_setting(&key, a, r, g, b) {
                Ok(()) => {
                    color_ctx.mark_dirty(SettingsCategory::Interface);
                    sync_after_edit(&window, &color_ctx.config);
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
            match color_hex_ctx.config.edit_setting(&key, &value) {
                Ok(()) => {
                    color_hex_ctx.mark_dirty(SettingsCategory::Interface);
                    sync_after_edit(&window, &color_hex_ctx.config);
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

    // Keybind capture (in-place update)
    let keybind_window = settings.as_weak();
    let keybind_ctx = ctx.clone();
    settings.on_keybind_captured(move |key, text, control, alt, shift, meta| {
        let Some(window) = keybind_window.upgrade() else {
            return;
        };
        let Some(chord) = format_key_chord(&text, control, alt, shift, meta) else {
            return;
        };
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
            Err(error) => window.set_settings_status(error.into()),
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
            }
            if let Some(app) = reset_ctx.app_weak.upgrade() {
                refresh_player_preferences(&app, &reset_ctx.config);
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
            if category == SettingsCategory::Dictionaries.as_i32() {
                if let Some(window) = reset_window.upgrade() {
                    refresh_dictionary_settings(
                        &window,
                        reset_ctx.dictionary_manager.as_deref(),
                        &reset_ctx.config,
                    );
                }
            }
            if category == SettingsCategory::Interface.as_i32() {
                if let Some(window) = reset_window.upgrade() {
                    refresh_custom_popup_css(&window, &reset_ctx.cache_dir);
                }
            }
        }
        report_settings(&reset_window, &reset_ctx.config, result);
    });

    let defaults_window = settings.as_weak();
    let defaults_ctx = ctx.clone();
    settings.on_defaults_settings(move |category| {
        let result = defaults_ctx.config.defaults_settings(category);
        if result.is_ok() {
            if let Some(cat) = SettingsCategory::from_i32(category) {
                defaults_ctx.mark_dirty(cat);
            }
            if category == SettingsCategory::Asr.as_i32() {
                if let Some(window) = defaults_window.upgrade() {
                    asr_models::refresh_asr_model_status(
                        &window,
                        &defaults_ctx.config,
                        &defaults_ctx.cache_dir,
                    );
                }
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
        let res = add_ctx.config.add_entry(category, &name);
        if res.is_ok() {
            if let Some(cat) = SettingsCategory::from_i32(category) {
                add_ctx.mark_dirty(cat);
            }
        }
        report_settings(&add_window, &add_ctx.config, res);
    });

    let remove_window = settings.as_weak();
    let remove_ctx = ctx.clone();
    settings.on_remove_entry(move |category, name| {
        let res = remove_ctx.config.remove_entry(category, &name);
        if res.is_ok() {
            if let Some(cat) = SettingsCategory::from_i32(category) {
                remove_ctx.mark_dirty(cat);
            }
        }
        report_settings(&remove_window, &remove_ctx.config, res);
    });

    let move_window = settings.as_weak();
    let move_ctx = ctx.clone();
    settings.on_move_entry(move |category, name, direction| {
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
    dictionaries::wire(&settings, &ctx);
    asr_models::wire(&settings, &ctx);
    anki::wire(&settings, &ctx);

    Ok(SettingsSession { window: settings })
}

#[cfg(test)]
mod tests {
    use super::{
        filtered_settings_groups, format_key_chord, refresh_category_settings, sync_after_edit,
        SettingsCategory,
    };

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
    fn settings_category_maps_indices_and_key_prefixes() {
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
            SettingsCategory::from_setting_key("anki/profile/show-tag"),
            Some(SettingsCategory::Anki)
        );
    }

    #[test]
    fn search_max_recursion_depth_refreshes_in_settings_window_across_apply_reset_defaults() {
        use crate::controller::config::ConfigController;
        use crate::SettingsWindow;
        use slint::Model;

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
        use crate::controller::config::ConfigController;
        use crate::SettingsWindow;
        use slint::{Model, VecModel};

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

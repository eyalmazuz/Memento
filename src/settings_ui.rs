//! Settings window session, dictionary management rows, and settings callbacks.

use std::cell::{Cell, RefCell};
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use slint::{ComponentHandle, Model as _, SharedString, VecModel};

use memento_asr::onnx::onnxcontroller;
use memento_asr::whisper::whispercontroller;
use memento_config::Value;
use memento_net::download::Downloader;
use memento_player::MpvInstance;
use memento_setting::keys as raw;

use crate::anki_help;
use crate::controller;
use crate::controller::anki::AnkiController;
use crate::controller::config::ConfigController;
use crate::controller::dict::DictController;
use crate::controller::dictmanager::{DictionaryManager, InstalledDictionary};
use crate::controller::player::{
    clear_primary_subtitle, refresh_subtitle_list, LivePlayerCoordinator,
};
use crate::controller::subtitles::SubtitleController;
use crate::ui::AsrModelStatusRow;
use crate::{
    apply_dictionary_order, memento_xdg_dir, refresh_player_preferences, setting_enabled,
    AnkiHelpWindow, InstalledDictionaryRow, MementoApp, SettingsWindow, SubEntry,
};

pub(crate) struct SettingsSession {
    pub(crate) window: SettingsWindow,
    _field_timer: Rc<slint::Timer>,
}

const WHISPER_MANAGED_MODELS: &[&str] = &[
    "tiny",
    "base",
    "small",
    "medium",
    "large-v3",
    "large-v3-turbo",
];

const ONNX_MANAGED_MODELS: &[&str] = &[
    onnxcontroller::PRESET_KITSUNE_T06B_JA,
    onnxcontroller::PRESET_KITSUNE_P03B_JA,
];

const DEFAULT_FONT_FAMILIES: &[&str] = &[
    "Kanji Stroke Orders",
    "Monospace",
    "Noto Sans",
    "Noto Sans CJK JP",
    "Sans Serif",
    "Serif",
];

static SYSTEM_FONT_FAMILIES: OnceLock<Vec<String>> = OnceLock::new();

fn system_font_families() -> &'static [String] {
    SYSTEM_FONT_FAMILIES.get_or_init(|| {
        let mut collection = slint::fontique_08::shared_collection();
        let mut families: Vec<String> = collection.family_names().map(str::to_owned).collect();
        for &default_family in DEFAULT_FONT_FAMILIES {
            families.push(default_family.to_owned());
        }
        families.sort_by_key(|name| name.to_ascii_lowercase());
        families.dedup();
        families
    })
}

fn available_font_families(config: &ConfigController) -> Vec<SharedString> {
    let mut families: Vec<String> = system_font_families().to_vec();
    for configured in config.configured_font_families() {
        if !families.iter().any(|existing| existing == &configured) {
            families.push(configured);
        }
    }
    families.sort_by_key(|name| name.to_ascii_lowercase());
    families.dedup();
    families.into_iter().map(Into::into).collect()
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

fn collect_asr_model_rows(config: &ConfigController, config_dir: &Path) -> Vec<AsrModelStatusRow> {
    let config_dir_str = config_dir.to_string_lossy();
    let backend = config.asr_backend_selected();
    let selected_whisper = config.whisper_selected_model();
    let selected_onnx = config.onnx_selected_model();
    let vad_enabled = config
        .effective_setting(raw::whisper::GROUP, raw::whisper::VAD_ENABLED)
        .is_some_and(|v| matches!(v, Value::Bool(true)));
    let vad_model = config
        .effective_setting(raw::whisper::GROUP, raw::whisper::VAD_MODEL)
        .and_then(|v| match v {
            Value::Text(t) => Some(t),
            _ => None,
        })
        .unwrap_or_else(|| whispercontroller::VAD_SILERO_6.to_owned());

    let mut rows = Vec::new();
    for &model in WHISPER_MANAGED_MODELS {
        let installed = whispercontroller::model_available(model, &config_dir_str);
        let path = whispercontroller::resolve_model_path(
            &whispercontroller::model_filename(model),
            &[whispercontroller::MODEL_SUFFIX],
            &config_dir_str,
        );
        let selected = backend == raw::asr::BACKEND_WHISPER && selected_whisper == model;
        rows.push(AsrModelStatusRow {
            backend: "whisper".into(),
            name: model.into(),
            path: path.into(),
            installed,
            selected,
            status_text: if installed {
                "Installed"
            } else {
                "Not installed"
            }
            .into(),
        });
    }

    if vad_enabled || !whispercontroller::vad_model_url(&vad_model).is_empty() {
        let vad_path = whispercontroller::resolve_model_path(
            &vad_model,
            &[whispercontroller::MODEL_SUFFIX],
            &config_dir_str,
        );
        let vad_installed = !vad_path.is_empty() && Path::new(&vad_path).exists();
        let vad_name = Path::new(&vad_model)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(whispercontroller::VAD_SILERO_6);
        if !whispercontroller::vad_model_url(vad_name).is_empty() {
            rows.push(AsrModelStatusRow {
                backend: "whisper-vad".into(),
                name: vad_name.into(),
                path: vad_path.into(),
                installed: vad_installed,
                selected: backend == raw::asr::BACKEND_WHISPER && vad_enabled,
                status_text: if vad_installed {
                    "Installed"
                } else {
                    "Not installed"
                }
                .into(),
            });
        }
    }

    for &model in ONNX_MANAGED_MODELS {
        let installed = onnxcontroller::model_available(model, &config_dir_str);
        let path = onnxcontroller::model_directory(model, &config_dir_str);
        let selected = backend == raw::asr::BACKEND_ONNX && selected_onnx == model;
        rows.push(AsrModelStatusRow {
            backend: "onnx".into(),
            name: model.into(),
            path: path.into(),
            installed,
            selected,
            status_text: if installed {
                "Installed"
            } else {
                "Not installed"
            }
            .into(),
        });
    }

    rows
}

fn refresh_asr_model_status(
    window: &SettingsWindow,
    config: &ConfigController,
    config_dir: &Path,
) {
    let dir_str = whispercontroller::models_directory(&config_dir.to_string_lossy());
    window.set_asr_models_dir(dir_str.into());
    let rows = collect_asr_model_rows(config, config_dir);
    window.set_asr_model_rows(Rc::new(VecModel::from(rows)).into());
}

fn dictionary_resources_dir(database_path: &Path) -> PathBuf {
    database_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
        .join("res")
}

pub(crate) fn ordered_dictionaries(
    mut dictionaries: Vec<InstalledDictionary>,
    config: &ConfigController,
) -> Vec<InstalledDictionary> {
    let order = match config.effective_setting(raw::dictionary::GROUP, raw::dictionary::ORDER) {
        Some(Value::Ints(order)) => order,
        _ => Vec::new(),
    };
    dictionaries.sort_by_key(|dictionary| {
        (
            order.iter().position(|id| *id == dictionary.id).is_none(),
            order
                .iter()
                .position(|id| *id == dictionary.id)
                .unwrap_or(usize::MAX),
            dictionary.id,
        )
    });
    dictionaries
}

pub(crate) fn dictionary_settings_rows(
    manager: &DictionaryManager,
    config: &ConfigController,
) -> Result<Vec<InstalledDictionaryRow>, String> {
    ordered_dictionaries(
        manager
            .list_dictionaries()
            .map_err(|error| error.to_string())?,
        config,
    )
    .into_iter()
    .map(|dictionary| {
        Ok(InstalledDictionaryRow {
            id: i32::try_from(dictionary.id).map_err(|_| {
                format!(
                    "dictionary ID {} is outside Slint's integer range",
                    dictionary.id
                )
            })?,
            title: dictionary.title.into(),
            disabled: dictionary.disabled,
        })
    })
    .collect()
}

pub(crate) fn refresh_dictionary_settings(
    window: &SettingsWindow,
    manager: Option<&DictionaryManager>,
    config: &ConfigController,
) {
    let rows = manager.map_or_else(
        || Ok(Vec::new()),
        |manager| dictionary_settings_rows(manager, config),
    );
    match rows {
        Ok(rows) => {
            window.set_installed_dictionaries(Rc::new(VecModel::from(rows)).into());
        }
        Err(error) => window.set_settings_status(format!("Dictionary list: {error}").into()),
    }
}

pub(crate) fn save_dictionary_order(
    config: &ConfigController,
    dictionaries: &[InstalledDictionary],
    cache_dir: &Path,
    anki_json_path: &Path,
) -> Result<(), String> {
    let order = dictionaries
        .iter()
        .map(|dictionary| dictionary.id.to_string())
        .collect::<Vec<_>>()
        .join(",");
    config.edit_setting("dictionary/order", &order)?;
    config.apply_settings(4, cache_dir, "memento.conf", anki_json_path)
}

#[allow(dead_code)]
pub(crate) fn mutate_dictionary_database(
    dictionary: &Rc<RefCell<DictController>>,
    database_path: &Path,
    config: &ConfigController,
    operation: impl FnOnce() -> Result<
        Vec<InstalledDictionary>,
        controller::dictmanager::DictionaryManagerError,
    >,
) -> Result<Vec<InstalledDictionary>, String> {
    *dictionary.borrow_mut() = DictController::new();
    let operation = operation().map_err(|error| error.to_string());
    let reopened = DictController::open(database_path)
        .map(|mut replacement| {
            apply_dictionary_order(config, &mut replacement);
            *dictionary.borrow_mut() = replacement;
        })
        .map_err(|error| format!("Could not reopen dictionary database: {error}"));
    match (operation, reopened) {
        (Ok(dictionaries), Ok(())) => Ok(dictionaries),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(error)) => Err(error),
        (Err(operation), Err(reopen)) => Err(format!("{operation}; {reopen}")),
    }
}

pub(crate) fn move_dictionary_order(
    manager: &DictionaryManager,
    config: &ConfigController,
    dictionary: &Rc<RefCell<DictController>>,
    cache_dir: &Path,
    anki_json_path: &Path,
    id: i32,
    direction: i32,
) -> Result<(), String> {
    let mut dictionaries = ordered_dictionaries(
        manager
            .list_dictionaries()
            .map_err(|error| error.to_string())?,
        config,
    );
    let index = dictionaries
        .iter()
        .position(|dictionary| dictionary.id == i64::from(id))
        .ok_or_else(|| format!("dictionary {id} is no longer installed"))?;
    let target = index as isize + direction.signum() as isize;
    if !(0..dictionaries.len() as isize).contains(&target) {
        return Ok(());
    }
    dictionaries.swap(index, target as usize);
    save_dictionary_order(config, &dictionaries, cache_dir, anki_json_path)?;
    apply_dictionary_order(config, &mut dictionary.borrow_mut());
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn finish_dictionary_change(
    window: &SettingsWindow,
    manager: &DictionaryManager,
    config: &ConfigController,
    dictionary: &Rc<RefCell<DictController>>,
    cache_dir: &Path,
    anki_json_path: &Path,
    dictionaries: Vec<InstalledDictionary>,
    persist_order: bool,
) {
    let dictionaries = ordered_dictionaries(dictionaries, config);
    if persist_order {
        if let Err(error) = save_dictionary_order(config, &dictionaries, cache_dir, anki_json_path)
        {
            refresh_dictionary_settings(window, Some(manager), config);
            window.set_settings_status(format!("Could not save dictionary order: {error}").into());
            return;
        }
        apply_dictionary_order(config, &mut dictionary.borrow_mut());
    }
    refresh_dictionary_settings(window, Some(manager), config);
    window.set_settings_status("Dictionary settings updated".into());
}

pub(crate) fn pick_dictionary_archive() -> Option<PathBuf> {
    let output = Command::new("zenity")
        .args([
            "--file-selection",
            "--title=Select Yomitan Dictionary Archive",
            "--file-filter=ZIP Archives | *.zip",
            "--file-filter=All Files | *",
        ])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let path = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    (!path.is_empty()).then(|| PathBuf::from(path))
}

pub(crate) fn refresh_category_settings(
    window: &SettingsWindow,
    config: &ConfigController,
    category: i32,
) {
    window
        .set_settings_groups(Rc::new(VecModel::from(config.category_settings_groups(category))).into());
    window.set_current_anki_profile(config.current_anki_profile().into());
    window.set_anki_show_tag(config.anki_show_tag());
    let anki_profiles: Vec<SharedString> = config
        .anki_profile_names()
        .into_iter()
        .map(Into::into)
        .collect();
    window.set_anki_profile_list(Rc::new(VecModel::from(anki_profiles)).into());
    window.set_current_keybind_profile(config.current_keybind_profile().into());
    window.set_keybind_conflict_count(config.keybind_conflict_count() as i32);
    if category == 5 {
        window.set_font_families(Rc::new(VecModel::from(available_font_families(config))).into());
    }
}

pub(crate) fn refresh_settings(window: &SettingsWindow, config: &ConfigController) {
    refresh_category_settings(window, config, window.get_active_category());
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

pub(crate) fn clear_asr_for_settings(
    app: &MementoApp,
    asr: &Rc<RefCell<controller::asr::AsrController>>,
    subtitles: &Rc<RefCell<SubtitleController>>,
    subtitle_model: &Rc<VecModel<SubEntry>>,
    restart_pending: &Rc<Cell<bool>>,
    mpv: Option<&Rc<RefCell<Option<MpvInstance>>>>,
    coordinator: Option<&Rc<RefCell<Option<LivePlayerCoordinator>>>>,
) {
    let selected = app.get_asr_track_selected() || app.get_asr_running();
    let restart = selected && app.get_asr_enabled();
    asr.borrow_mut().cancel();
    if let Some(coord_rc) = coordinator {
        if let Ok(mut coord_guard) = coord_rc.try_borrow_mut() {
            if let Some(coord) = coord_guard.as_mut() {
                let player_guard = mpv.and_then(|p| p.try_borrow().ok());
                let player_ref = player_guard.as_deref().and_then(Option::as_ref);
                coord.remove_asr_track(player_ref);
            }
        }
    }
    let mut subtitles = subtitles.borrow_mut();
    subtitles.clear_asr_track();
    refresh_subtitle_list(app, subtitle_model, &subtitles);
    app.set_asr_running(false);
    app.set_asr_track_selected(restart);
    restart_pending.set(restart);
    if selected {
        clear_primary_subtitle(app);
    }
}

#[allow(clippy::too_many_arguments)]
fn finish_async_dictionary_mutation(
    window_weak: &slint::Weak<SettingsWindow>,
    manager: &Option<Rc<DictionaryManager>>,
    config: &ConfigController,
    dict_ctrl: &Rc<RefCell<DictController>>,
    db_path: &Path,
    cache_dir: &Path,
    anki_json_path: &Path,
    operation: Result<Vec<InstalledDictionary>, String>,
    persist_order: bool,
    simple_status: &str,
    error_prefix: &str,
) {
    let Some(window) = window_weak.upgrade() else {
        return;
    };
    window.set_dictionary_busy(false);
    window.set_dictionary_progress_text("".into());
    let reopened = DictController::open(db_path)
        .map(|mut replacement| {
            apply_dictionary_order(config, &mut replacement);
            *dict_ctrl.borrow_mut() = replacement;
        })
        .map_err(|error| format!("Could not reopen dictionary database: {error}"));
    let combined = match (operation, reopened) {
        (Ok(dictionaries), Ok(())) => Ok(dictionaries),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(error)) => Err(error),
        (Err(op_err), Err(reopen_err)) => Err(format!("{op_err}; {reopen_err}")),
    };
    match (combined, manager.as_deref()) {
        (Ok(dictionaries), Some(mgr)) => {
            if persist_order {
                finish_dictionary_change(
                    &window,
                    mgr,
                    config,
                    dict_ctrl,
                    cache_dir,
                    anki_json_path,
                    dictionaries,
                    true,
                );
            } else {
                refresh_dictionary_settings(&window, Some(mgr), config);
                window.set_settings_status(simple_status.into());
            }
        }
        (Err(error), _) => {
            refresh_dictionary_settings(&window, manager.as_deref(), config);
            window.set_settings_status(format!("{error_prefix}: {error}").into());
        }
        (Ok(_), None) => {
            window.set_settings_status("Dictionary manager is unavailable".into());
        }
    }
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
    settings.set_ocr_built(cfg!(feature = "ocr"));
    settings.set_ocr_available(ocr_available);
    settings.set_asr_available(controller::asr::AVAILABLE);
    settings.set_font_families(Rc::new(VecModel::from(available_font_families(&config_ctrl))).into());
    refresh_settings(&settings, &config_ctrl);
    refresh_dictionary_settings(&settings, dictionary_manager.as_deref(), &config_ctrl);
    refresh_asr_model_status(&settings, &config_ctrl, &cache_dir);

    // A native Help window on Wayland becomes visible when it is created.
    // Construct it only when Help is requested.
    let help_window = Rc::new(RefCell::new(None::<AnkiHelpWindow>));
    let help_on_request = help_window.clone();
    settings.on_anki_help_requested(move || {
        if help_on_request.borrow().is_none() {
            match AnkiHelpWindow::new() {
                Ok(window) => {
                    anki_help::populate(&window);
                    *help_on_request.borrow_mut() = Some(window);
                }
                Err(error) => {
                    eprintln!("Could not create Anki help: {error}");
                    return;
                }
            }
        }
        if let Some(window) = help_on_request.borrow().as_ref() {
            if let Err(error) = window.show() {
                eprintln!("Could not open Anki help: {error}");
            }
        }
    });

    let category_window = settings.as_weak();
    let category_config = config_ctrl.clone();
    let category_manager = dictionary_manager.clone();
    let category_cache = cache_dir.clone();
    settings.on_category_changed(move |category| {
        if let Some(window) = category_window.upgrade() {
            refresh_category_settings(&window, &category_config, category);
            if category == 4 {
                refresh_dictionary_settings(&window, category_manager.as_deref(), &category_config);
            } else if category == 8 {
                refresh_asr_model_status(&window, &category_config, &category_cache);
            }
            window.window().request_redraw();
        }
    });

    let field_timer = Rc::new(slint::Timer::default());
    let close_settings_weak = settings.as_weak();
    let close_timer_req = field_timer.clone();
    settings.window().on_close_requested(move || {
        close_timer_req.stop();
        if let Some(settings) = close_settings_weak.upgrade() {
            let _ = settings.hide();
        }
        slint::CloseRequestResponse::HideWindow
    });
    let close_settings_btn = settings.as_weak();
    let close_timer_btn = field_timer.clone();
    settings.on_close(move || {
        close_timer_btn.stop();
        if let Some(settings) = close_settings_btn.upgrade() {
            let _ = settings.hide();
        }
    });

    let edited_window = settings.as_weak();
    let edited_config = config_ctrl.clone();
    let edited_manager = dictionary_manager.clone();
    let edited_cache = cache_dir.clone();
    settings.on_setting_edited(move |key, value| {
        if let Some(window) = edited_window.upgrade() {
            match edited_config.edit_setting(&key, &value) {
                Ok(()) => {
                    if matches!(value.as_str(), "true" | "false")
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
                        || key.starts_with("keybind/profiles/")
                    {
                        refresh_settings(&window, &edited_config);
                    }
                    if key == "dictionary/order" {
                        refresh_dictionary_settings(
                            &window,
                            edited_manager.as_deref(),
                            &edited_config,
                        );
                    }
                    if matches!(
                        key.as_str(),
                        "asr/backend" | "whisper/model" | "whisper/vad-enable" | "whisper/vad-model" | "onnx/model"
                    ) {
                        refresh_asr_model_status(&window, &edited_config, &edited_cache);
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
        }
    });

    let font_window = settings.as_weak();
    let font_config = config_ctrl.clone();
    settings.on_font_edited(move |key, family, size, bold, italic| {
        if let Some(window) = font_window.upgrade() {
            match font_config.edit_font_setting(&key, &family, &size, bold, italic) {
                Ok(()) => {
                    refresh_settings(&window, &font_config);
                    window.set_settings_status("Unsaved changes — click Apply".into());
                }
                Err(error) => window.set_settings_status(error.into()),
            }
        }
    });

    let font_typed_window = settings.as_weak();
    let font_typed_config = config_ctrl.clone();
    settings.on_font_size_typed(move |key, family, size, bold, italic| {
        if let Some(window) = font_typed_window.upgrade() {
            match font_typed_config.edit_font_setting(&key, &family, &size, bold, italic) {
                Ok(()) => window.set_settings_status("Unsaved changes — click Apply".into()),
                Err(error) => window.set_settings_status(error.into()),
            }
        }
    });

    let color_window = settings.as_weak();
    let color_config = config_ctrl.clone();
    settings.on_color_edited(move |key, a, r, g, b| {
        if let Some(window) = color_window.upgrade() {
            match color_config.edit_color_setting(&key, a, r, g, b) {
                Ok(()) => {
                    refresh_settings(&window, &color_config);
                    window.set_settings_status("Unsaved changes — click Apply".into());
                }
                Err(error) => window.set_settings_status(error.into()),
            }
        }
    });

    let keybind_window = settings.as_weak();
    let keybind_config = config_ctrl.clone();
    settings.on_keybind_captured(move |key, text, control, alt, shift, meta| {
        let Some(window) = keybind_window.upgrade() else {
            return;
        };
        let Some(chord) = format_key_chord(&text, control, alt, shift, meta) else {
            return;
        };
        match keybind_config.edit_setting(&key, &chord) {
            Ok(()) => {
                refresh_settings(&window, &keybind_config);
                let conflicts = keybind_config.keybind_conflict_count();
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

    let preset_window = settings.as_weak();
    let preset_config = config_ctrl.clone();
    settings.on_apply_anki_preset(move |preset_name| {
        let Some(window) = preset_window.upgrade() else {
            return;
        };
        match preset_config.apply_anki_note_preset(&preset_name) {
            Ok(count) => {
                refresh_category_settings(&window, &preset_config, 0);
                window.set_settings_status(
                    format!("Applied {preset_name} preset ({count} fields) — click Apply to save")
                        .into(),
                );
            }
            Err(error) => window.set_settings_status(error.into()),
        }
    });

    let apply_window = settings.as_weak();
    let apply_config = config_ctrl.clone();
    let apply_cache = cache_dir.clone();
    let apply_anki = anki_json_path.clone();
    let apply_dict = dict_ctrl.clone();
    let apply_app = app.as_weak();
    let apply_manager = dictionary_manager.clone();
    let apply_asr = asr_session.clone();
    let apply_subtitles = subtitles.clone();
    let apply_subtitle_model = subtitle_model.clone();
    let apply_restart = asr_restart_pending.clone();
    let apply_mpv = mpv.clone();
    let apply_coord = coordinator.clone();
    settings.on_apply_settings(move |category| {
        let result =
            apply_config.apply_settings(category, &apply_cache, "memento.conf", &apply_anki);
        if result.is_ok() {
            if let Some(app) = apply_app.upgrade() {
                refresh_player_preferences(&app, &apply_config);
                if category == 4 {
                    apply_dictionary_order(&apply_config, &mut apply_dict.borrow_mut());
                    if let Some(window) = apply_window.upgrade() {
                        refresh_dictionary_settings(
                            &window,
                            apply_manager.as_deref(),
                            &apply_config,
                        );
                    }
                }
                app.set_ocr_enabled(
                    ocr_available
                        && setting_enabled(
                            &apply_config,
                            raw::ocr::GROUP,
                            raw::ocr::ENABLED,
                            raw::ocr::ENABLED_DEFAULT,
                        ),
                );
                app.set_asr_enabled(
                    controller::asr::AVAILABLE
                        && setting_enabled(
                            &apply_config,
                            raw::asr::GROUP,
                            raw::asr::ENABLED,
                            raw::asr::ENABLED_DEFAULT,
                        ),
                );
                if category == 8 {
                    if let Some(window) = apply_window.upgrade() {
                        refresh_asr_model_status(&window, &apply_config, &apply_cache);
                    }
                    let config_dir = memento_xdg_dir("XDG_CONFIG_HOME", ".config")
                        .unwrap_or_else(|| PathBuf::from("."));
                    let unchanged =
                        controller::asr::AsrSettings::from_config(config_dir, &apply_config.store())
                            .is_ok_and(|settings| apply_asr.borrow().matches_settings(&settings));
                    if !unchanged {
                        clear_asr_for_settings(
                            &app,
                            &apply_asr,
                            &apply_subtitles,
                            &apply_subtitle_model,
                            &apply_restart,
                            Some(&apply_mpv),
                            Some(&apply_coord),
                        );
                    }
                }
            }
        }
        report_settings(&apply_window, &apply_config, result);
    });
    let reset_window = settings.as_weak();
    let reset_config = config_ctrl.clone();
    let reset_cache = cache_dir.clone();
    let reset_anki = anki_json_path.clone();
    let reset_dict = dict_ctrl.clone();
    let reset_manager = dictionary_manager.clone();
    let reset_app = app.as_weak();
    let reset_asr = asr_session.clone();
    let reset_subtitles = subtitles.clone();
    let reset_subtitle_model = subtitle_model.clone();
    let reset_restart = asr_restart_pending.clone();
    let reset_mpv = mpv.clone();
    let reset_coord = coordinator.clone();
    settings.on_reset_settings(move |category| {
        let result =
            reset_config.reset_settings(category, &reset_cache, "memento.conf", &reset_anki);
        if result.is_ok() {
            if let Some(app) = reset_app.upgrade() {
                refresh_player_preferences(&app, &reset_config);
                app.set_ocr_enabled(
                    ocr_available
                        && setting_enabled(
                            &reset_config,
                            raw::ocr::GROUP,
                            raw::ocr::ENABLED,
                            raw::ocr::ENABLED_DEFAULT,
                        ),
                );
                app.set_asr_enabled(
                    controller::asr::AVAILABLE
                        && setting_enabled(
                            &reset_config,
                            raw::asr::GROUP,
                            raw::asr::ENABLED,
                            raw::asr::ENABLED_DEFAULT,
                        ),
                );
                if category == 8 {
                    if let Some(window) = reset_window.upgrade() {
                        refresh_asr_model_status(&window, &reset_config, &reset_cache);
                    }
                    clear_asr_for_settings(
                        &app,
                        &reset_asr,
                        &reset_subtitles,
                        &reset_subtitle_model,
                        &reset_restart,
                        Some(&reset_mpv),
                        Some(&reset_coord),
                    );
                }
            }
            if category == 4 {
                apply_dictionary_order(&reset_config, &mut reset_dict.borrow_mut());
                if let Some(window) = reset_window.upgrade() {
                    refresh_dictionary_settings(
                        &window,
                        reset_manager.as_deref(),
                        &reset_config,
                    );
                }
            }
        }
        report_settings(&reset_window, &reset_config, result);
    });
    let defaults_window = settings.as_weak();
    let defaults_config = config_ctrl.clone();
    let defaults_cache = cache_dir.clone();
    let defaults_app = app.as_weak();
    let defaults_asr = asr_session.clone();
    let defaults_subtitles = subtitles.clone();
    let defaults_subtitle_model = subtitle_model.clone();
    let defaults_restart = asr_restart_pending.clone();
    let defaults_mpv = mpv.clone();
    let defaults_coord = coordinator.clone();
    settings.on_defaults_settings(move |category| {
        let result = defaults_config.defaults_settings(category);
        if result.is_ok() && category == 8 {
            if let Some(window) = defaults_window.upgrade() {
                refresh_asr_model_status(&window, &defaults_config, &defaults_cache);
            }
            if let Some(app) = defaults_app.upgrade() {
                app.set_asr_enabled(
                    controller::asr::AVAILABLE
                        && setting_enabled(
                            &defaults_config,
                            raw::asr::GROUP,
                            raw::asr::ENABLED,
                            raw::asr::ENABLED_DEFAULT,
                        ),
                );
                clear_asr_for_settings(
                    &app,
                    &defaults_asr,
                    &defaults_subtitles,
                    &defaults_subtitle_model,
                    &defaults_restart,
                    Some(&defaults_mpv),
                    Some(&defaults_coord),
                );
            }
        }
        report_settings(&defaults_window, &defaults_config, result);
    });
    let add_window = settings.as_weak();
    let add_config = config_ctrl.clone();
    settings.on_add_entry(move |category, name| {
        report_settings(&add_window, &add_config, add_config.add_entry(category, &name));
    });
    let remove_window = settings.as_weak();
    let remove_config = config_ctrl.clone();
    settings.on_remove_entry(move |category, name| {
        report_settings(
            &remove_window,
            &remove_config,
            remove_config.remove_entry(category, &name),
        );
    });
    let move_window = settings.as_weak();
    let move_config = config_ctrl.clone();
    settings.on_move_entry(move |category, name, direction| {
        report_settings(
            &move_window,
            &move_config,
            move_config.move_entry(category, &name, direction),
        );
    });

    struct PendingDictMutation {
        result: Result<Vec<InstalledDictionary>, String>,
        persist_order: bool,
        simple_status: &'static str,
        error_prefix: &'static str,
    }

    let pending_dict_mutation: Arc<Mutex<Option<PendingDictMutation>>> =
        Arc::new(Mutex::new(None));

    let completed_window = settings.as_weak();
    let completed_manager = dictionary_manager.clone();
    let completed_config = config_ctrl.clone();
    let completed_ctrl = dict_ctrl.clone();
    let completed_db_path = dictionary_database_path.clone();
    let completed_cache = cache_dir.clone();
    let completed_anki = anki_json_path.clone();
    let completed_slot = pending_dict_mutation.clone();
    settings.on_dictionary_op_completed(move || {
        let Some(payload) = completed_slot.lock().ok().and_then(|mut guard| guard.take()) else {
            return;
        };
        finish_async_dictionary_mutation(
            &completed_window,
            &completed_manager,
            &completed_config,
            &completed_ctrl,
            &completed_db_path,
            &completed_cache,
            &completed_anki,
            payload.result,
            payload.persist_order,
            payload.simple_status,
            payload.error_prefix,
        );
    });

    let picked_window = settings.as_weak();
    let picked_controller = dict_ctrl.clone();
    let picked_db_path = dictionary_database_path.clone();
    let picked_slot = pending_dict_mutation.clone();
    settings.on_dictionary_archive_picked(move |path_str| {
        let Some(window) = picked_window.upgrade() else {
            return;
        };
        if path_str.is_empty() {
            window.set_dictionary_busy(false);
            window.set_dictionary_progress_text("".into());
            return;
        }
        let archive = PathBuf::from(path_str.as_str());
        let label = archive
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("archive")
            .to_owned();
        window.set_dictionary_progress_text(format!("Importing {label}…").into());
        *picked_controller.borrow_mut() = DictController::new();

        let done_window = picked_window.clone();
        let db_path = picked_db_path.clone();
        let res_path = dictionary_resources_dir(&db_path);
        let done_slot = picked_slot.clone();
        std::thread::spawn(move || {
            let result = DictionaryManager::new(&db_path, &res_path)
                .and_then(|mgr| mgr.add_dictionary(&archive))
                .map_err(|error| error.to_string());
            if let Ok(mut guard) = done_slot.lock() {
                *guard = Some(PendingDictMutation {
                    result,
                    persist_order: true,
                    simple_status: "Dictionary settings updated",
                    error_prefix: "Could not add dictionary",
                });
            }
            let _ = done_window.upgrade_in_event_loop(|win| win.invoke_dictionary_op_completed());
        });
    });

    // Background dictionary add (picker + SQLite import off UI thread)
    let add_dictionary_window = settings.as_weak();
    let add_dictionary_manager = dictionary_manager.clone();
    settings.on_add_dictionary_archive(move || {
        let Some(window) = add_dictionary_window.upgrade() else {
            return;
        };
        if window.get_dictionary_busy() {
            window.set_settings_status("Dictionary operation already in progress".into());
            return;
        }
        if add_dictionary_manager.is_none() {
            window.set_settings_status("Dictionary manager is unavailable".into());
            return;
        }
        window.set_dictionary_busy(true);
        window.set_dictionary_progress_text("Selecting dictionary archive…".into());

        let picker_window = add_dictionary_window.clone();
        std::thread::spawn(move || {
            let path_str = pick_dictionary_archive()
                .map(|path| path.to_string_lossy().into_owned())
                .unwrap_or_default();
            let _ = picker_window
                .upgrade_in_event_loop(move |win| win.invoke_dictionary_archive_picked(path_str.into()));
        });
    });

    // Background dictionary enable/disable
    let toggle_dictionary_window = settings.as_weak();
    let toggle_dictionary_manager = dictionary_manager.clone();
    let toggle_dictionary_controller = dict_ctrl.clone();
    let toggle_dictionary_path = dictionary_database_path.clone();
    let toggle_dictionary_config = config_ctrl.clone();
    let toggle_slot = pending_dict_mutation.clone();
    settings.on_dictionary_enabled_changed(move |id, enabled| {
        let Some(window) = toggle_dictionary_window.upgrade() else {
            return;
        };
        if window.get_dictionary_busy() {
            refresh_dictionary_settings(
                &window,
                toggle_dictionary_manager.as_deref(),
                &toggle_dictionary_config,
            );
            window.set_settings_status("Dictionary operation already in progress".into());
            return;
        }
        if toggle_dictionary_manager.is_none() {
            window.set_settings_status("Dictionary manager is unavailable".into());
            return;
        }
        window.set_dictionary_busy(true);
        window.set_dictionary_progress_text(
            if enabled {
                format!("Enabling dictionary {id}…")
            } else {
                format!("Disabling dictionary {id}…")
            }
            .into(),
        );
        *toggle_dictionary_controller.borrow_mut() = DictController::new();

        let done_window = toggle_dictionary_window.clone();
        let db_path = toggle_dictionary_path.clone();
        let res_path = dictionary_resources_dir(&db_path);
        let done_slot = toggle_slot.clone();
        std::thread::spawn(move || {
            let result = DictionaryManager::new(&db_path, &res_path)
                .and_then(|mgr| {
                    if enabled {
                        mgr.enable_dictionary(i64::from(id))
                    } else {
                        mgr.disable_dictionary(i64::from(id))
                    }
                })
                .map_err(|error| error.to_string());
            if let Ok(mut guard) = done_slot.lock() {
                *guard = Some(PendingDictMutation {
                    result,
                    persist_order: false,
                    simple_status: "Dictionary enabled state updated",
                    error_prefix: "Could not change dictionary state",
                });
            }
            let _ = done_window.upgrade_in_event_loop(|win| win.invoke_dictionary_op_completed());
        });
    });

    // Background dictionary delete
    let delete_dictionary_window = settings.as_weak();
    let delete_dictionary_manager = dictionary_manager.clone();
    let delete_dictionary_controller = dict_ctrl.clone();
    let delete_dictionary_path = dictionary_database_path.clone();
    let delete_slot = pending_dict_mutation.clone();
    settings.on_delete_dictionary(move |id| {
        let Some(window) = delete_dictionary_window.upgrade() else {
            return;
        };
        if window.get_dictionary_busy() {
            window.set_settings_status("Dictionary operation already in progress".into());
            return;
        }
        if delete_dictionary_manager.is_none() {
            window.set_settings_status("Dictionary manager is unavailable".into());
            return;
        }
        window.set_dictionary_busy(true);
        window.set_dictionary_progress_text(format!("Deleting dictionary {id}…").into());
        *delete_dictionary_controller.borrow_mut() = DictController::new();

        let done_window = delete_dictionary_window.clone();
        let db_path = delete_dictionary_path.clone();
        let res_path = dictionary_resources_dir(&db_path);
        let done_slot = delete_slot.clone();
        std::thread::spawn(move || {
            let result = DictionaryManager::new(&db_path, &res_path)
                .and_then(|mgr| mgr.delete_dictionary(i64::from(id)))
                .map_err(|error| error.to_string());
            if let Ok(mut guard) = done_slot.lock() {
                *guard = Some(PendingDictMutation {
                    result,
                    persist_order: true,
                    simple_status: "Dictionary settings updated",
                    error_prefix: "Could not delete dictionary",
                });
            }
            let _ = done_window.upgrade_in_event_loop(|win| win.invoke_dictionary_op_completed());
        });
    });

    let move_dictionary_up_window = settings.as_weak();
    let move_dictionary_up_manager = dictionary_manager.clone();
    let move_dictionary_up_controller = dict_ctrl.clone();
    let move_dictionary_up_config = config_ctrl.clone();
    let move_dictionary_up_cache = cache_dir.clone();
    let move_dictionary_up_anki = anki_json_path.clone();
    settings.on_move_dictionary_up(move |id| {
        let Some(window) = move_dictionary_up_window.upgrade() else {
            return;
        };
        if window.get_dictionary_busy() {
            return;
        }
        let Some(manager) = move_dictionary_up_manager.as_deref() else {
            window.set_settings_status("Dictionary manager is unavailable".into());
            return;
        };
        match move_dictionary_order(
            manager,
            &move_dictionary_up_config,
            &move_dictionary_up_controller,
            &move_dictionary_up_cache,
            &move_dictionary_up_anki,
            id,
            -1,
        ) {
            Ok(()) => {
                refresh_dictionary_settings(
                    &window,
                    Some(manager),
                    &move_dictionary_up_config,
                );
                window.set_settings_status("Dictionary order updated".into());
            }
            Err(error) => {
                window.set_settings_status(format!("Could not move dictionary: {error}").into())
            }
        }
    });
    let move_dictionary_down_window = settings.as_weak();
    let move_dictionary_down_manager = dictionary_manager.clone();
    let move_dictionary_down_controller = dict_ctrl.clone();
    let move_dictionary_down_config = config_ctrl.clone();
    let move_dictionary_down_cache = cache_dir.clone();
    let move_dictionary_down_anki = anki_json_path.clone();
    settings.on_move_dictionary_down(move |id| {
        let Some(window) = move_dictionary_down_window.upgrade() else {
            return;
        };
        if window.get_dictionary_busy() {
            return;
        }
        let Some(manager) = move_dictionary_down_manager.as_deref() else {
            window.set_settings_status("Dictionary manager is unavailable".into());
            return;
        };
        match move_dictionary_order(
            manager,
            &move_dictionary_down_config,
            &move_dictionary_down_controller,
            &move_dictionary_down_cache,
            &move_dictionary_down_anki,
            id,
            1,
        ) {
            Ok(()) => {
                refresh_dictionary_settings(
                    &window,
                    Some(manager),
                    &move_dictionary_down_config,
                );
                window.set_settings_status("Dictionary order updated".into());
            }
            Err(error) => {
                window.set_settings_status(format!("Could not move dictionary: {error}").into())
            }
        }
    });

    // ASR model status refresh & non-blocking model download
    let asr_refresh_window = settings.as_weak();
    let asr_refresh_config = config_ctrl.clone();
    let asr_refresh_cache = cache_dir.clone();
    settings.on_refresh_asr_models(move || {
        if let Some(window) = asr_refresh_window.upgrade() {
            refresh_asr_model_status(&window, &asr_refresh_config, &asr_refresh_cache);
        }
    });

    let asr_download_cancel: Rc<RefCell<Option<Arc<AtomicBool>>>> = Rc::new(RefCell::new(None));
    let cancel_flag_ref = asr_download_cancel.clone();
    let cancel_window = settings.as_weak();
    settings.on_cancel_asr_download(move || {
        if let Some(flag) = cancel_flag_ref.borrow().as_ref() {
            flag.store(true, Ordering::Relaxed);
        }
        if let Some(window) = cancel_window.upgrade() {
            window.set_asr_download_text("Cancelling model download…".into());
        }
    });

    let dl_finished_window = settings.as_weak();
    let dl_finished_config = config_ctrl.clone();
    let dl_finished_cache = cache_dir.clone();
    settings.on_asr_download_finished(move |_ok, status_msg| {
        let Some(win) = dl_finished_window.upgrade() else {
            return;
        };
        win.set_asr_download_active(false);
        win.set_asr_download_progress(0.0);
        win.set_asr_download_text("".into());
        win.set_asr_download_model("".into());
        refresh_asr_model_status(&win, &dl_finished_config, &dl_finished_cache);
        win.set_settings_status(status_msg);
    });

    let dl_window = settings.as_weak();
    let dl_cache = cache_dir.clone();
    let dl_cancel_holder = asr_download_cancel.clone();
    settings.on_download_asr_model(move |backend, model_name| {
        let Some(window) = dl_window.upgrade() else {
            return;
        };
        if window.get_asr_download_active() {
            return;
        }
        let backend = backend.to_string();
        let model_name = model_name.to_string();
        let config_dir_str = dl_cache.to_string_lossy().into_owned();

        let jobs: Vec<(String, PathBuf)> = match backend.as_str() {
            "whisper" => {
                if !whispercontroller::is_managed_model(&model_name) {
                    window.set_settings_status(
                        format!("Unknown Whisper model {model_name:?}").into(),
                    );
                    return;
                }
                let url = whispercontroller::whisper_model_url(&model_name);
                let target = PathBuf::from(whispercontroller::models_directory(&config_dir_str))
                    .join(whispercontroller::model_filename(&model_name));
                vec![(url, target)]
            }
            "whisper-vad" => {
                let url = whispercontroller::vad_model_url(&model_name);
                if url.is_empty() {
                    window.set_settings_status(
                        format!("No download URL for VAD model {model_name:?}").into(),
                    );
                    return;
                }
                let target = PathBuf::from(whispercontroller::models_directory(&config_dir_str))
                    .join(&model_name);
                vec![(url, target)]
            }
            "onnx" => {
                let specs = onnxcontroller::model_files(&model_name);
                if specs.is_empty() {
                    window.set_settings_status(
                        format!("Unknown ONNX model {model_name:?}").into(),
                    );
                    return;
                }
                let model_dir = PathBuf::from(onnxcontroller::model_directory(
                    &model_name,
                    &config_dir_str,
                ));
                specs
                    .into_iter()
                    .map(|spec| (spec.url, model_dir.join(spec.relative_path)))
                    .collect()
            }
            _ => return,
        };

        let cancel_flag = Arc::new(AtomicBool::new(false));
        *dl_cancel_holder.borrow_mut() = Some(cancel_flag.clone());
        window.set_asr_download_active(true);
        window.set_asr_download_model(model_name.clone().into());
        window.set_asr_download_progress(-1.0);
        window.set_asr_download_text(format!("Starting download for {model_name}…").into());

        let worker_window = dl_window.clone();
        std::thread::spawn(move || {
            let downloader = Downloader::default();
            let total_files = jobs.len();
            let mut outcome: Result<(), String> = Ok(());
            for (idx, (url, target)) in jobs.into_iter().enumerate() {
                let file_label = target
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("model")
                    .to_owned();
                let mut last_ui_update = Instant::now() - Duration::from_secs(1);
                let progress_window = worker_window.clone();
                let res = downloader.download(&url, &target, |event| {
                    if cancel_flag.load(Ordering::Relaxed) {
                        return Err(io::Error::new(
                            io::ErrorKind::Interrupted,
                            "Download cancelled",
                        ));
                    }
                    if last_ui_update.elapsed() >= Duration::from_millis(120) {
                        last_ui_update = Instant::now();
                        let total_bytes = event.progress.total;
                        let frac = if total_bytes > 0 {
                            ((idx as f64 + event.progress.fraction) / total_files as f64) as f32
                        } else {
                            -1.0
                        };
                        let text = if total_bytes > 0 {
                            let pct = (event.progress.fraction * 100.0).round() as i64;
                            let mb_s = event.progress.speed_bps as f64 / (1024.0 * 1024.0);
                            format!(
                                "Downloading {file_label} ({}/{total_files}): {pct}% ({mb_s:.1} MB/s)",
                                idx + 1
                            )
                        } else {
                            let mb = event.progress.received as f64 / (1024.0 * 1024.0);
                            format!(
                                "Downloading {file_label} ({}/{total_files}): {mb:.1} MB",
                                idx + 1
                            )
                        };
                        let _ = progress_window.upgrade_in_event_loop(move |win| {
                            win.set_asr_download_progress(frac);
                            win.set_asr_download_text(text.into());
                        });
                    }
                    Ok(())
                });
                if let Err(err) = res {
                    outcome = Err(if cancel_flag.load(Ordering::Relaxed) {
                        "ASR model download cancelled".to_owned()
                    } else {
                        err.to_string()
                    });
                    break;
                }
            }
            let (ok, status_msg) = match outcome {
                Ok(()) => (true, format!("Downloaded ASR model {model_name}")),
                Err(err) => (false, err),
            };
            let _ = worker_window.upgrade_in_event_loop(move |win| {
                win.invoke_asr_download_finished(ok, status_msg.into());
            });
        });
    });

    let test_window = settings.as_weak();
    let test_config = config_ctrl.clone();
    settings.on_test_anki_connection(move || {
        let endpoint = test_config.anki_endpoint();
        let response_window = test_window.clone();
        std::thread::spawn(move || {
            let controller = AnkiController::new(endpoint);
            let result = controller.test_connection().and_then(|version| {
                let decks = controller
                    .client()
                    .deck_names()
                    .map_err(|error| error.to_string())?;
                let models = controller
                    .client()
                    .model_names()
                    .map_err(|error| error.to_string())?;
                Ok((version, decks, models))
            });
            let _ = response_window.upgrade_in_event_loop(move |window| {
                let (ok, text) = match result {
                    Ok((version, decks, models)) => {
                        window.set_available_anki_decks(
                            Rc::new(VecModel::from(
                                decks.into_iter().map(Into::into).collect::<Vec<_>>(),
                            ))
                            .into(),
                        );
                        window.set_available_anki_models(
                            Rc::new(VecModel::from(
                                models.into_iter().map(Into::into).collect::<Vec<_>>(),
                            ))
                            .into(),
                        );
                        (
                            true,
                            format!(
                                "AnkiConnect is running (API version {version}). Decks and models are ready to select."
                            ),
                        )
                    }
                    Err(error) => (
                        false,
                        format!(
                            "Could not reach AnkiConnect: {error}\nMake sure Anki is open and the AnkiConnect add-on is installed."
                        ),
                    ),
                };
                window.set_settings_status(
                    if ok {
                        "Anki connection OK"
                    } else {
                        "Anki connection failed"
                    }
                    .into(),
                );
                window.set_connection_popup_ok(ok);
                window.set_connection_popup_text(text.into());
                window.set_connection_popup_open(true);
            });
        });
    });

    let field_window = settings.as_weak();
    let field_config = config_ctrl.clone();
    let (field_tx, field_rx) =
        std::sync::mpsc::channel::<(String, String, String, Result<Vec<String>, String>)>();
    let pending_field_requests = Rc::new(Cell::new(0usize));
    let timer_for_load = field_timer.clone();
    let pending_for_load = pending_field_requests.clone();
    let field_timer_window = settings.as_weak();
    let field_timer_config = config_ctrl.clone();
    let field_rx = Rc::new(field_rx);
    let rx_for_load = field_rx.clone();
    let pending_for_timer = pending_field_requests.clone();
    let timer_in_tick = field_timer.clone();

    let start_field_timer = move || {
        let w = field_timer_window.clone();
        let cfg = field_timer_config.clone();
        let rx = rx_for_load.clone();
        let pending = pending_for_timer.clone();
        let timer = timer_in_tick.clone();
        timer_for_load.start(
            slint::TimerMode::Repeated,
            Duration::from_millis(100),
            move || {
                let Some(window) = w.upgrade() else {
                    timer.stop();
                    return;
                };
                if !window.window().is_visible() {
                    timer.stop();
                    return;
                }
                while let Ok((section, profile_name, model, result)) = rx.try_recv() {
                    pending.set(pending.get().saturating_sub(1));
                    if cfg.current_anki_profile() != profile_name
                        || cfg.anki_note_model(&section).as_deref() != Some(model.as_str())
                    {
                        continue;
                    }
                    match result {
                        Ok(fields) if !fields.is_empty() => {
                            match cfg.set_anki_model_fields(&section, &fields) {
                                Ok(()) => {
                                    refresh_category_settings(&window, &cfg, 0);
                                    window.set_settings_status(
                                        format!(
                                            "Loaded {} model fields — click Apply to save",
                                            fields.len()
                                        )
                                        .into(),
                                    );
                                }
                                Err(error) => window.set_settings_status(
                                    format!("Could not update Anki fields: {error}").into(),
                                ),
                            }
                        }
                        Ok(_) => window
                            .set_settings_status("The selected Anki model has no fields".into()),
                        Err(error) => window.set_settings_status(
                            format!("Could not load Anki fields: {error}").into(),
                        ),
                    }
                }
                if pending.get() == 0 {
                    timer.stop();
                }
            },
        );
    };

    settings.on_load_anki_fields(move |section| {
        let Some(window) = field_window.upgrade() else {
            return;
        };
        let section = section.to_string();
        let Some(model) = field_config
            .anki_note_model(&section)
            .filter(|model| !model.trim().is_empty())
        else {
            window.set_settings_status("Choose an Anki note model first".into());
            return;
        };
        window.set_settings_status(format!("Loading {model} fields from Anki…").into());
        let endpoint = field_config.anki_endpoint();
        let profile_name = field_config.current_anki_profile();
        let response_sender = field_tx.clone();
        pending_for_load.set(pending_for_load.get() + 1);
        start_field_timer();
        std::thread::spawn(move || {
            let result = AnkiController::new(endpoint)
                .client()
                .field_names(&model)
                .map_err(|error| error.to_string());
            let _ = response_sender.send((section, profile_name, model, result));
        });
    });

    Ok(SettingsSession {
        window: settings,
        _field_timer: field_timer,
    })
}

#[cfg(test)]
mod tests {
    use super::format_key_chord;

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
        // Standalone modifier key must be ignored
        assert_eq!(format_key_chord("\u{0011}", true, false, false, false), None);
        assert_eq!(format_key_chord("", false, false, false, false), None);
    }
}

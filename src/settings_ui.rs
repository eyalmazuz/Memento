//! Settings window session, dictionary management rows, and settings callbacks.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;

use slint::{ComponentHandle, Model as _, VecModel};

use memento_config::Value;
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
use crate::{
    apply_dictionary_order, memento_xdg_dir, refresh_player_preferences, setting_enabled,
    AnkiHelpWindow, InstalledDictionaryRow, MementoApp, SettingsWindow, SubEntry,
};

pub(crate) struct SettingsSession {
    pub(crate) window: SettingsWindow,
    _field_timer: slint::Timer,
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
    window.set_current_keybind_profile(config.current_keybind_profile().into());
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
    refresh_settings(&settings, &config_ctrl);
    refresh_dictionary_settings(&settings, dictionary_manager.as_deref(), &config_ctrl);

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
    settings.on_category_changed(move |category| {
        if let Some(window) = category_window.upgrade() {
            refresh_category_settings(&window, &category_config, category);
            if category == 4 {
                refresh_dictionary_settings(&window, category_manager.as_deref(), &category_config);
            }
            window.window().request_redraw();
        }
    });

    let close_settings_weak = settings.as_weak();
    settings.window().on_close_requested(move || {
        if let Some(settings) = close_settings_weak.upgrade() {
            let _ = settings.hide();
        }
        slint::CloseRequestResponse::HideWindow
    });
    let close_settings_btn = settings.as_weak();
    settings.on_close(move || {
        if let Some(settings) = close_settings_btn.upgrade() {
            let _ = settings.hide();
        }
    });
    let edited_window = settings.as_weak();
    let edited_config = config_ctrl.clone();
    let edited_manager = dictionary_manager.clone();
    settings.on_setting_edited(move |key, value| {
        if let Some(window) = edited_window.upgrade() {
            match edited_config.edit_setting(&key, &value) {
                Ok(()) => {
                    if matches!(value.as_str(), "true" | "false")
                        || matches!(key.as_str(), "anki/profile" | "keybind/current")
                        || key.ends_with("/deck")
                        || key.ends_with("/model")
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
    let add_dictionary_window = settings.as_weak();
    let add_dictionary_manager = dictionary_manager.clone();
    let add_dictionary_controller = dict_ctrl.clone();
    let add_dictionary_path = dictionary_database_path.clone();
    let add_dictionary_config = config_ctrl.clone();
    let add_dictionary_cache = cache_dir.clone();
    let add_dictionary_anki = anki_json_path.clone();
    settings.on_add_dictionary_archive(move || {
        let Some(window) = add_dictionary_window.upgrade() else {
            return;
        };
        let Some(archive) = pick_dictionary_archive() else {
            return;
        };
        let Some(manager) = add_dictionary_manager.as_deref() else {
            window.set_settings_status("Dictionary manager is unavailable".into());
            return;
        };
        let result = mutate_dictionary_database(
            &add_dictionary_controller,
            &add_dictionary_path,
            &add_dictionary_config,
            || manager.add_dictionary(&archive),
        );
        match result {
            Ok(dictionaries) => finish_dictionary_change(
                &window,
                manager,
                &add_dictionary_config,
                &add_dictionary_controller,
                &add_dictionary_cache,
                &add_dictionary_anki,
                dictionaries,
                true,
            ),
            Err(error) => {
                window.set_settings_status(format!("Could not add dictionary: {error}").into())
            }
        }
    });
    let toggle_dictionary_window = settings.as_weak();
    let toggle_dictionary_manager = dictionary_manager.clone();
    let toggle_dictionary_controller = dict_ctrl.clone();
    let toggle_dictionary_path = dictionary_database_path.clone();
    let toggle_dictionary_config = config_ctrl.clone();
    settings.on_dictionary_enabled_changed(move |id, enabled| {
        let Some(window) = toggle_dictionary_window.upgrade() else {
            return;
        };
        let Some(manager) = toggle_dictionary_manager.as_deref() else {
            window.set_settings_status("Dictionary manager is unavailable".into());
            return;
        };
        let result = mutate_dictionary_database(
            &toggle_dictionary_controller,
            &toggle_dictionary_path,
            &toggle_dictionary_config,
            || {
                if enabled {
                    manager.enable_dictionary(i64::from(id))
                } else {
                    manager.disable_dictionary(i64::from(id))
                }
            },
        );
        match result {
            Ok(_) => {
                refresh_dictionary_settings(
                    &window,
                    Some(manager),
                    &toggle_dictionary_config,
                );
                window.set_settings_status("Dictionary enabled state updated".into());
            }
            Err(error) => window.set_settings_status(
                format!("Could not change dictionary state: {error}").into(),
            ),
        }
    });
    let delete_dictionary_window = settings.as_weak();
    let delete_dictionary_manager = dictionary_manager.clone();
    let delete_dictionary_controller = dict_ctrl.clone();
    let delete_dictionary_path = dictionary_database_path.clone();
    let delete_dictionary_config = config_ctrl.clone();
    let delete_dictionary_cache = cache_dir.clone();
    let delete_dictionary_anki = anki_json_path.clone();
    settings.on_delete_dictionary(move |id| {
        let Some(window) = delete_dictionary_window.upgrade() else {
            return;
        };
        let Some(manager) = delete_dictionary_manager.as_deref() else {
            window.set_settings_status("Dictionary manager is unavailable".into());
            return;
        };
        let result = mutate_dictionary_database(
            &delete_dictionary_controller,
            &delete_dictionary_path,
            &delete_dictionary_config,
            || manager.delete_dictionary(i64::from(id)),
        );
        match result {
            Ok(dictionaries) => finish_dictionary_change(
                &window,
                manager,
                &delete_dictionary_config,
                &delete_dictionary_controller,
                &delete_dictionary_cache,
                &delete_dictionary_anki,
                dictionaries,
                true,
            ),
            Err(error) => {
                window.set_settings_status(format!("Could not delete dictionary: {error}").into())
            }
        }
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
    let field_timer = slint::Timer::default();
    let field_timer_window = settings.as_weak();
    let field_timer_config = config_ctrl.clone();
    field_timer.start(
        slint::TimerMode::Repeated,
        std::time::Duration::from_millis(100),
        move || {
            while let Ok((section, profile_name, model, result)) = field_rx.try_recv() {
                let Some(window) = field_timer_window.upgrade() else {
                    continue;
                };
                if field_timer_config.current_anki_profile() != profile_name
                    || field_timer_config.anki_note_model(&section).as_deref()
                        != Some(model.as_str())
                {
                    continue;
                }
                match result {
                    Ok(fields) if !fields.is_empty() => {
                        match field_timer_config.set_anki_model_fields(&section, &fields) {
                            Ok(()) => {
                                refresh_category_settings(&window, &field_timer_config, 0);
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
                    Ok(_) => {
                        window.set_settings_status("The selected Anki model has no fields".into())
                    }
                    Err(error) => window.set_settings_status(
                        format!("Could not load Anki fields: {error}").into(),
                    ),
                }
            }
        },
    );
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

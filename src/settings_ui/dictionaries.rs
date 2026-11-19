//! Installed Yomitan dictionary list, ordering, archive picking, and background SQLite mutations.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use memento_config::Value;
use memento_os::{open_files_blocking, FileDialogOptions};
use memento_setting::keys as raw;
use slint::{ComponentHandle, VecModel};

use crate::controller::config::ConfigController;
use crate::controller::dict::DictController;
use crate::controller::dictmanager::{DictionaryManager, InstalledDictionary};
use crate::{apply_dictionary_order, InstalledDictionaryRow, SettingsWindow};

use super::{SettingsCategory, SettingsContext};

struct PendingDictMutation {
    result: Result<Vec<InstalledDictionary>, String>,
    persist_order: bool,
    simple_status: &'static str,
    error_prefix: &'static str,
}

static NEXT_COMPLETION_TOKEN: AtomicU64 = AtomicU64::new(1);

thread_local! {
    /// UI-thread one-shot completion callbacks so `dict_ctrl` is ALWAYS reopened
    /// on the main thread even if `SettingsWindow` was dropped mid-operation.
    static PENDING_DICT_COMPLETIONS: RefCell<HashMap<u64, Box<dyn FnOnce()>>> =
        RefCell::new(HashMap::new());
}

fn register_completion(callback: impl FnOnce() + 'static) -> u64 {
    let token = NEXT_COMPLETION_TOKEN.fetch_add(1, Ordering::Relaxed);
    PENDING_DICT_COMPLETIONS.with(|map| {
        map.borrow_mut().insert(token, Box::new(callback));
    });
    token
}

fn dispatch_completion(token: u64) {
    let _ = slint::invoke_from_event_loop(move || {
        let cb = PENDING_DICT_COMPLETIONS.with(|map| map.borrow_mut().remove(&token));
        if let Some(cb) = cb {
            cb();
        }
    });
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
    config.apply_settings(
        SettingsCategory::Dictionaries.as_i32(),
        cache_dir,
        "memento.conf",
        anki_json_path,
    )
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

/// Uses XDG Desktop Portal via `memento_os` first (falling back to native CLI pickers),
/// returning `None` cleanly on cancellation.
pub(crate) fn pick_dictionary_archive() -> Option<PathBuf> {
    open_files_blocking(&FileDialogOptions::archive_zip(None))
        .into_iter()
        .next()
}

fn finish_async_dictionary_mutation(
    window_weak: &slint::Weak<SettingsWindow>,
    ctx: &SettingsContext,
    payload: PendingDictMutation,
) {
    // Always reopen `dict_ctrl` even if `SettingsWindow` was dropped while the
    // background SQLite mutation was running.
    let reopened = DictController::open(&ctx.dictionary_database_path)
        .map(|mut replacement| {
            apply_dictionary_order(&ctx.config, &mut replacement);
            *ctx.dict_ctrl.borrow_mut() = replacement;
        })
        .map_err(|error| format!("Could not reopen dictionary database: {error}"));

    let combined = match (payload.result, reopened) {
        (Ok(dictionaries), Ok(())) => Ok(dictionaries),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(error)) => Err(error),
        (Err(op_err), Err(reopen_err)) => Err(format!("{op_err}; {reopen_err}")),
    };

    let order_error = if let Ok(ref dictionaries) = combined {
        if payload.persist_order {
            let ordered = ordered_dictionaries(dictionaries.clone(), &ctx.config);
            match save_dictionary_order(
                &ctx.config,
                &ordered,
                &ctx.cache_dir,
                &ctx.anki_json_path,
            ) {
                Ok(()) => {
                    apply_dictionary_order(&ctx.config, &mut ctx.dict_ctrl.borrow_mut());
                    None
                }
                Err(error) => Some(error),
            }
        } else {
            None
        }
    } else {
        None
    };

    let Some(window) = window_weak.upgrade() else {
        return;
    };
    window.set_dictionary_busy(false);
    window.set_dictionary_progress_text("".into());

    match (combined, ctx.dictionary_manager.as_deref()) {
        (Ok(_), Some(mgr)) => {
            refresh_dictionary_settings(&window, Some(mgr), &ctx.config);
            if let Some(error) = order_error {
                window.set_settings_status(format!("Could not save dictionary order: {error}").into());
            } else {
                window.set_settings_status(payload.simple_status.into());
            }
        }
        (Err(error), _) => {
            refresh_dictionary_settings(&window, ctx.dictionary_manager.as_deref(), &ctx.config);
            window.set_settings_status(format!("{}: {error}", payload.error_prefix).into());
        }
        (Ok(_), None) => {
            window.set_settings_status("Dictionary manager is unavailable".into());
        }
    }
}

fn spawn_dictionary_mutation(
    window: &SettingsWindow,
    window_weak: slint::Weak<SettingsWindow>,
    ctx: &Rc<SettingsContext>,
    slot: &Arc<Mutex<Option<PendingDictMutation>>>,
    progress_text: String,
    persist_order: bool,
    simple_status: &'static str,
    error_prefix: &'static str,
    worker_op: impl FnOnce(&DictionaryManager) -> Result<Vec<InstalledDictionary>, String>
        + Send
        + 'static,
) {
    window.set_dictionary_busy(true);
    window.set_dictionary_progress_text(progress_text.into());
    *ctx.dict_ctrl.borrow_mut() = DictController::new();

    let db_path = ctx.dictionary_database_path.clone();
    let res_path = dictionary_resources_dir(&db_path);
    let done_slot = slot.clone();

    let completion_ctx = ctx.clone();
    let completion_slot = slot.clone();
    let completion_window = window_weak;
    let token = register_completion(move || {
        if let Some(payload) = completion_slot.lock().ok().and_then(|mut g| g.take()) {
            finish_async_dictionary_mutation(&completion_window, &completion_ctx, payload);
        }
    });

    std::thread::spawn(move || {
        let result = DictionaryManager::new(&db_path, &res_path)
            .map_err(|error| error.to_string())
            .and_then(|mgr| worker_op(&mgr));
        if let Ok(mut guard) = done_slot.lock() {
            *guard = Some(PendingDictMutation {
                result,
                persist_order,
                simple_status,
                error_prefix,
            });
        }
        dispatch_completion(token);
    });
}

fn handle_move_dictionary(
    window_weak: &slint::Weak<SettingsWindow>,
    ctx: &SettingsContext,
    id: i32,
    direction: i32,
) {
    let Some(window) = window_weak.upgrade() else {
        return;
    };
    if window.get_dictionary_busy() {
        return;
    }
    let Some(manager) = ctx.dictionary_manager.as_deref() else {
        window.set_settings_status("Dictionary manager is unavailable".into());
        return;
    };
    match move_dictionary_order(
        manager,
        &ctx.config,
        &ctx.dict_ctrl,
        &ctx.cache_dir,
        &ctx.anki_json_path,
        id,
        direction,
    ) {
        Ok(()) => {
            refresh_dictionary_settings(&window, Some(manager), &ctx.config);
            window.set_settings_status("Dictionary order updated".into());
        }
        Err(error) => {
            window.set_settings_status(format!("Could not move dictionary: {error}").into());
        }
    }
}

pub(crate) fn wire(window: &SettingsWindow, ctx: &Rc<SettingsContext>) {
    let pending_dict_mutation: Arc<Mutex<Option<PendingDictMutation>>> =
        Arc::new(Mutex::new(None));

    // Kept connected for compatibility if invoked directly from Slint
    let completed_window = window.as_weak();
    let completed_ctx = ctx.clone();
    let completed_slot = pending_dict_mutation.clone();
    window.on_dictionary_op_completed(move || {
        let Some(payload) = completed_slot.lock().ok().and_then(|mut g| g.take()) else {
            return;
        };
        finish_async_dictionary_mutation(&completed_window, &completed_ctx, payload);
    });

    let picked_window = window.as_weak();
    let picked_ctx = ctx.clone();
    let picked_slot = pending_dict_mutation.clone();
    window.on_dictionary_archive_picked(move |path_str| {
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
        spawn_dictionary_mutation(
            &window,
            picked_window.clone(),
            &picked_ctx,
            &picked_slot,
            format!("Importing {label}…"),
            true,
            "Dictionary settings updated",
            "Could not add dictionary",
            move |mgr| mgr.add_dictionary(&archive).map_err(|e| e.to_string()),
        );
    });

    // Background dictionary add (native portal picker + SQLite import off UI thread)
    let add_window = window.as_weak();
    let add_ctx = ctx.clone();
    window.on_add_dictionary_archive(move || {
        let Some(window) = add_window.upgrade() else {
            return;
        };
        if window.get_dictionary_busy() {
            window.set_settings_status("Dictionary operation already in progress".into());
            return;
        }
        if add_ctx.dictionary_manager.is_none() {
            window.set_settings_status("Dictionary manager is unavailable".into());
            return;
        }
        window.set_dictionary_busy(true);
        window.set_dictionary_progress_text("Selecting archive…".into());

        let picker_window = add_window.clone();
        std::thread::spawn(move || {
            let path_str = pick_dictionary_archive()
                .map(|path| path.to_string_lossy().into_owned())
                .unwrap_or_default();
            let _ = picker_window.upgrade_in_event_loop(move |win| {
                win.invoke_dictionary_archive_picked(path_str.into());
            });
        });
    });

    // Background dictionary enable/disable
    let toggle_window = window.as_weak();
    let toggle_ctx = ctx.clone();
    let toggle_slot = pending_dict_mutation.clone();
    window.on_dictionary_enabled_changed(move |id, enabled| {
        let Some(window) = toggle_window.upgrade() else {
            return;
        };
        if window.get_dictionary_busy() {
            refresh_dictionary_settings(
                &window,
                toggle_ctx.dictionary_manager.as_deref(),
                &toggle_ctx.config,
            );
            window.set_settings_status("Dictionary operation already in progress".into());
            return;
        }
        if toggle_ctx.dictionary_manager.is_none() {
            window.set_settings_status("Dictionary manager is unavailable".into());
            return;
        }
        let progress = if enabled {
            format!("Enabling ID {id}…")
        } else {
            format!("Disabling ID {id}…")
        };
        spawn_dictionary_mutation(
            &window,
            toggle_window.clone(),
            &toggle_ctx,
            &toggle_slot,
            progress,
            false,
            "Dictionary enabled state updated",
            "Could not change dictionary state",
            move |mgr| {
                if enabled {
                    mgr.enable_dictionary(i64::from(id))
                } else {
                    mgr.disable_dictionary(i64::from(id))
                }
                .map_err(|e| e.to_string())
            },
        );
    });

    // Background dictionary delete
    let delete_window = window.as_weak();
    let delete_ctx = ctx.clone();
    let delete_slot = pending_dict_mutation.clone();
    window.on_delete_dictionary(move |id| {
        let Some(window) = delete_window.upgrade() else {
            return;
        };
        if window.get_dictionary_busy() {
            window.set_settings_status("Dictionary operation already in progress".into());
            return;
        }
        if delete_ctx.dictionary_manager.is_none() {
            window.set_settings_status("Dictionary manager is unavailable".into());
            return;
        }
        spawn_dictionary_mutation(
            &window,
            delete_window.clone(),
            &delete_ctx,
            &delete_slot,
            format!("Deleting ID {id}…"),
            true,
            "Dictionary deleted",
            "Could not delete dictionary",
            move |mgr| mgr.delete_dictionary(i64::from(id)).map_err(|e| e.to_string()),
        );
    });

    let up_window = window.as_weak();
    let up_ctx = ctx.clone();
    window.on_move_dictionary_up(move |id| handle_move_dictionary(&up_window, &up_ctx, id, -1));

    let down_window = window.as_weak();
    let down_ctx = ctx.clone();
    window.on_move_dictionary_down(move |id| handle_move_dictionary(&down_window, &down_ctx, id, 1));
}

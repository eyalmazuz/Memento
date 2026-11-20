//! Installed Yomitan dictionary list, ordering, multi-file archive picking,
//! drag-and-drop batch imports, and background SQLite mutations.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use memento_config::Value;
use memento_os::{open_files_on_main_thread, FileDialogOptions};
use memento_setting::keys as raw;
use slint::{ComponentHandle, VecModel};

use crate::controller::config::ConfigController;
use crate::controller::dict::DictController;
use crate::controller::dictmanager::{DictionaryManager, InstalledDictionary};
use crate::{apply_dictionary_order, InstalledDictionaryRow, SettingsWindow};

use super::{anki, SettingsCategory, SettingsContext};

struct PendingDictMutation {
    result: Result<Vec<InstalledDictionary>, String>,
    persist_order: bool,
    status_message: String,
    error_prefix: Option<&'static str>,
}

/// Summary produced by sequential multi-archive dictionary import (`C1.2`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BatchImportOutcome {
    pub(crate) total: usize,
    pub(crate) succeeded: Vec<String>,
    pub(crate) failed: Vec<(String, String)>,
    pub(crate) dictionaries: Option<Vec<InstalledDictionary>>,
}

impl BatchImportOutcome {
    pub(crate) fn format_status_summary(&self) -> String {
        if self.total == 0 {
            return String::new();
        }
        if self.failed.is_empty() {
            if self.succeeded.len() == 1 {
                return format!("Imported {}", self.succeeded[0]);
            }
            return format!("Imported {}/{} dictionaries", self.succeeded.len(), self.total);
        }
        if self.succeeded.is_empty() {
            if self.failed.len() == 1 {
                let (name, err) = &self.failed[0];
                return format!("Could not add dictionary {name}: {err}");
            }
            let (first_name, first_err) = &self.failed[0];
            return format!(
                "Failed to import {} dictionaries ({first_name}: {first_err})",
                self.failed.len()
            );
        }
        let failed_names = self
            .failed
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "Imported {}/{} dictionaries ({} failed: {failed_names})",
            self.succeeded.len(),
            self.total,
            self.failed.len()
        )
    }
}

/// Formats live progress text while importing archive `index` (1-based) of `total`.
pub(crate) fn format_import_progress(index: usize, total: usize, filename: &str) -> String {
    if total > 1 {
        format!("Importing {index}/{total}: {filename}…")
    } else {
        format!("Importing {filename}…")
    }
}

/// Returns `true` if `path` has a `.zip` extension (case-insensitive).
pub(crate) fn is_zip_archive(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("zip"))
}

fn archive_display_name(path: &Path) -> String {
    path.file_name()
        .and_then(|n| n.to_str())
        .filter(|n| !n.is_empty())
        .unwrap_or("archive.zip")
        .to_owned()
}

/// Builds multi-file `FileDialogOptions` for importing one or more Yomitan `.zip` archives.
pub(crate) fn archive_zip_batch_options() -> FileDialogOptions {
    let mut opts = FileDialogOptions::archive_zip(None);
    opts.title = "Select Yomitan Dictionary Archives".to_owned();
    opts.multiple = true;
    opts
}

/// Sequentially imports every archive in `archives`, invoking `on_progress` before
/// each file and continuing remaining files if an individual archive fails.
pub(crate) fn import_dictionary_batch<F, P>(
    archives: &[PathBuf],
    mut import_one: F,
    mut on_progress: P,
) -> BatchImportOutcome
where
    F: FnMut(&Path) -> Result<Vec<InstalledDictionary>, String>,
    P: FnMut(usize, usize, &str),
{
    let total = archives.len();
    let mut succeeded = Vec::new();
    let mut failed = Vec::new();
    let mut latest_dictionaries: Option<Vec<InstalledDictionary>> = None;

    for (idx, archive) in archives.iter().enumerate() {
        let label = archive_display_name(archive);
        on_progress(idx + 1, total, &label);
        match import_one(archive) {
            Ok(dictionaries) => {
                succeeded.push(label);
                latest_dictionaries = Some(dictionaries);
            }
            Err(error) => {
                failed.push((label, error));
            }
        }
    }

    BatchImportOutcome {
        total,
        succeeded,
        failed,
        dictionaries: latest_dictionaries,
    }
}

/// RAII sentinel ensuring `dictionary_busy` clears cleanly if the native picker
/// is cancelled (`memento_os::open_files_on_main_thread` drops the callback
/// without invoking it when the user cancels).
struct PickerCancelGuard {
    window: slint::Weak<SettingsWindow>,
    token: u64,
    handled: bool,
}

impl PickerCancelGuard {
    fn new(window: slint::Weak<SettingsWindow>, token: u64) -> Self {
        Self {
            window,
            token,
            handled: false,
        }
    }

    fn mark_handled(&mut self) {
        self.handled = true;
    }
}

impl Drop for PickerCancelGuard {
    fn drop(&mut self) {
        if !self.handled {
            let win = self.window.clone();
            let token = self.token;
            let _ = slint::invoke_from_event_loop(move || {
                PENDING_DICT_COMPLETIONS.with(|map| {
                    map.borrow_mut().remove(&token);
                });
                if let Some(w) = win.upgrade() {
                    if w.get_dictionary_progress_text().as_str() == "Selecting archives…" {
                        w.set_dictionary_busy(false);
                        w.set_dictionary_progress_text("".into());
                    }
                }
            });
        }
    }
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

/// Shared state coordinating queued multi-archive imports and drag-and-drop bursts.
#[derive(Clone)]
pub(crate) struct DictionaryBatchCoordinator {
    ctx: Rc<SettingsContext>,
    pending_mutation: Arc<Mutex<Option<PendingDictMutation>>>,
    /// Archives queued while another dictionary operation is already running.
    queued_archives: Rc<RefCell<VecDeque<PathBuf>>>,
    /// Coalescing buffer for multi-file `WindowEvent::DroppedFile` bursts.
    dropped_burst: Rc<RefCell<Vec<PathBuf>>>,
    dropped_non_zip: Rc<RefCell<Vec<String>>>,
    burst_flush_scheduled: Rc<Cell<bool>>,
}

impl DictionaryBatchCoordinator {
    fn new(
        ctx: Rc<SettingsContext>,
        pending_mutation: Arc<Mutex<Option<PendingDictMutation>>>,
    ) -> Self {
        Self {
            ctx,
            pending_mutation,
            queued_archives: Rc::new(RefCell::new(VecDeque::new())),
            dropped_burst: Rc::new(RefCell::new(Vec::new())),
            dropped_non_zip: Rc::new(RefCell::new(Vec::new())),
            burst_flush_scheduled: Rc::new(Cell::new(false)),
        }
    }

    pub(crate) fn on_hovered_file(&self, window: &SettingsWindow, path: &Path) {
        if path.as_os_str().is_empty() || is_zip_archive(path) {
            window.set_drop_hover_active(true);
        }
    }

    pub(crate) fn on_hovered_file_cancelled(&self, window: &SettingsWindow) {
        window.set_drop_hover_active(false);
    }

    /// Accepts a single `WindowEvent::DroppedFile` path and coalesces back-to-back
    /// multi-file drops into a single sequential import batch.
    pub(crate) fn on_dropped_file(&self, window: &SettingsWindow, path: PathBuf) {
        window.set_drop_hover_active(false);
        if is_zip_archive(&path) {
            self.dropped_burst.borrow_mut().push(path);
        } else {
            self.dropped_non_zip
                .borrow_mut()
                .push(archive_display_name(&path));
        }

        if !self.burst_flush_scheduled.replace(true) {
            let win_weak = window.as_weak();
            let this = self.clone();
            let token = register_completion(move || {
                this.burst_flush_scheduled.set(false);
                if let Some(win) = win_weak.upgrade() {
                    this.flush_dropped_burst(&win);
                }
            });
            let dispatched = slint::invoke_from_event_loop(move || {
                let cb = PENDING_DICT_COMPLETIONS.with(|map| map.borrow_mut().remove(&token));
                if let Some(cb) = cb {
                    cb();
                }
            });
            if dispatched.is_err() {
                // Headless/no-event-loop fallback: flush immediately.
                let cb = PENDING_DICT_COMPLETIONS.with(|map| map.borrow_mut().remove(&token));
                if let Some(cb) = cb {
                    cb();
                }
            }
        }
    }

    fn flush_dropped_burst(&self, window: &SettingsWindow) {
        let zips: Vec<PathBuf> = self.dropped_burst.borrow_mut().drain(..).collect();
        let skipped: Vec<String> = self.dropped_non_zip.borrow_mut().drain(..).collect();
        if zips.is_empty() {
            if !skipped.is_empty() {
                window.set_settings_status(
                    format!(
                        "Skipped non-ZIP file ({}) — drop Yomitan .zip dictionary archives",
                        skipped.join(", ")
                    )
                    .into(),
                );
            }
            return;
        }
        self.enqueue_or_start_batch(window, zips);
    }

    pub(crate) fn enqueue_or_start_batch(&self, window: &SettingsWindow, archives: Vec<PathBuf>) {
        let archives: Vec<PathBuf> = archives
            .into_iter()
            .filter(|p| !p.as_os_str().is_empty())
            .collect();
        if archives.is_empty() {
            if window.get_dictionary_progress_text().as_str() == "Selecting archives…" {
                window.set_dictionary_busy(false);
                window.set_dictionary_progress_text("".into());
            }
            return;
        }
        if self.ctx.dictionary_manager.is_none() {
            window.set_dictionary_busy(false);
            window.set_dictionary_progress_text("".into());
            window.set_settings_status("Dictionary manager is unavailable".into());
            return;
        }
        let selecting_placeholder =
            window.get_dictionary_progress_text().as_str() == "Selecting archives…";
        if window.get_dictionary_busy() && !selecting_placeholder {
            let added = archives.len();
            self.queued_archives.borrow_mut().extend(archives);
            let pending_count = self.queued_archives.borrow().len();
            window.set_settings_status(
                format!(
                    "Queued {added} dictionary archive(s) ({pending_count} waiting after current import)"
                )
                .into(),
            );
            return;
        }
        self.start_batch_import(window, archives);
    }

    fn start_batch_import(&self, window: &SettingsWindow, archives: Vec<PathBuf>) {
        let total = archives.len();
        let first_label = archive_display_name(&archives[0]);
        let initial_progress = format_import_progress(1, total, &first_label);
        window.set_dictionary_busy(true);
        window.set_dictionary_progress_text(initial_progress.clone().into());
        window.set_settings_status(initial_progress.into());
        *self.ctx.dict_ctrl.borrow_mut() = DictController::new();

        let db_path = self.ctx.dictionary_database_path.clone();
        let res_path = dictionary_resources_dir(&db_path);
        let done_slot = self.pending_mutation.clone();
        let progress_window = window.as_weak();

        let completion_coord = self.clone();
        let completion_window = window.as_weak();
        let token = register_completion(move || {
            if let Some(payload) = completion_coord
                .pending_mutation
                .lock()
                .ok()
                .and_then(|mut g| g.take())
            {
                completion_coord.finish_mutation(&completion_window, payload);
            }
        });

        std::thread::spawn(move || {
            let payload = match DictionaryManager::new(&db_path, &res_path) {
                Ok(mgr) => {
                    let outcome = import_dictionary_batch(
                        &archives,
                        |archive| mgr.add_dictionary(archive).map_err(|e| e.to_string()),
                        |idx, total, label| {
                            let text = format_import_progress(idx, total, label);
                            let _ = progress_window.upgrade_in_event_loop(move |win| {
                                win.set_dictionary_progress_text(text.clone().into());
                                win.set_settings_status(text.into());
                            });
                        },
                    );
                    let summary = outcome.format_status_summary();
                    if outcome.succeeded.is_empty() {
                        PendingDictMutation {
                            result: Err(summary.clone()),
                            persist_order: false,
                            status_message: summary,
                            error_prefix: None,
                        }
                    } else {
                        let dictionaries = match outcome.dictionaries {
                            Some(dicts) => Ok(dicts),
                            None => mgr.list_dictionaries().map_err(|e| e.to_string()),
                        };
                        PendingDictMutation {
                            result: dictionaries,
                            persist_order: true,
                            status_message: summary,
                            error_prefix: Some("Could not add dictionary"),
                        }
                    }
                }
                Err(error) => {
                    let msg = error.to_string();
                    PendingDictMutation {
                        result: Err(msg.clone()),
                        persist_order: false,
                        status_message: msg,
                        error_prefix: Some("Could not add dictionary"),
                    }
                }
            };
            if let Ok(mut guard) = done_slot.lock() {
                *guard = Some(payload);
            }
            dispatch_completion(token);
        });
    }

    fn finish_mutation(
        &self,
        window_weak: &slint::Weak<SettingsWindow>,
        payload: PendingDictMutation,
    ) {
        finish_async_dictionary_mutation(window_weak, &self.ctx, payload);
        if let Some(window) = window_weak.upgrade() {
            let next_batch: Vec<PathBuf> = self.queued_archives.borrow_mut().drain(..).collect();
            if !next_batch.is_empty() {
                self.start_batch_import(&window, next_batch);
            }
        }
    }
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
            replacement.set_text_replacements(ctx.config.text_replacement_options());
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
            anki::refresh_anki_excluded_dicts(&window, Some(mgr), &ctx.config);
            if let Some(error) = order_error {
                window.set_settings_status(format!("Could not save dictionary order: {error}").into());
            } else {
                window.set_settings_status(payload.status_message.into());
            }
        }
        (Err(error), _) => {
            refresh_dictionary_settings(&window, ctx.dictionary_manager.as_deref(), &ctx.config);
            anki::refresh_anki_excluded_dicts(
                &window,
                ctx.dictionary_manager.as_deref(),
                &ctx.config,
            );
            let msg = match payload.error_prefix {
                Some(prefix) => format!("{prefix}: {error}"),
                None => error,
            };
            window.set_settings_status(msg.into());
        }
        (Ok(_), None) => {
            window.set_settings_status("Dictionary manager is unavailable".into());
        }
    }
}

fn spawn_dictionary_mutation(
    window: &SettingsWindow,
    window_weak: slint::Weak<SettingsWindow>,
    coord: &DictionaryBatchCoordinator,
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
    *coord.ctx.dict_ctrl.borrow_mut() = DictController::new();

    let db_path = coord.ctx.dictionary_database_path.clone();
    let res_path = dictionary_resources_dir(&db_path);
    let done_slot = coord.pending_mutation.clone();

    let completion_coord = coord.clone();
    let completion_window = window_weak;
    let token = register_completion(move || {
        if let Some(payload) = completion_coord
            .pending_mutation
            .lock()
            .ok()
            .and_then(|mut g| g.take())
        {
            completion_coord.finish_mutation(&completion_window, payload);
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
                status_message: simple_status.to_owned(),
                error_prefix: Some(error_prefix),
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
            anki::refresh_anki_excluded_dicts(&window, Some(manager), &ctx.config);
            window.set_settings_status("Dictionary order updated".into());
        }
        Err(error) => {
            window.set_settings_status(format!("Could not move dictionary: {error}").into());
        }
    }
}

pub(crate) fn wire(
    window: &SettingsWindow,
    ctx: &Rc<SettingsContext>,
) -> DictionaryBatchCoordinator {
    let pending_dict_mutation: Arc<Mutex<Option<PendingDictMutation>>> =
        Arc::new(Mutex::new(None));
    let coord = DictionaryBatchCoordinator::new(ctx.clone(), pending_dict_mutation);

    // Kept connected for compatibility if invoked directly from Slint
    let completed_window = window.as_weak();
    let completed_coord = coord.clone();
    window.on_dictionary_op_completed(move || {
        let Some(payload) = completed_coord
            .pending_mutation
            .lock()
            .ok()
            .and_then(|mut g| g.take())
        else {
            return;
        };
        completed_coord.finish_mutation(&completed_window, payload);
    });

    // Kept connected for single-path Slint/test invocations
    let picked_window = window.as_weak();
    let picked_coord = coord.clone();
    window.on_dictionary_archive_picked(move |path_str| {
        let Some(window) = picked_window.upgrade() else {
            return;
        };
        if path_str.is_empty() {
            window.set_dictionary_busy(false);
            window.set_dictionary_progress_text("".into());
            return;
        }
        picked_coord.enqueue_or_start_batch(&window, vec![PathBuf::from(path_str.as_str())]);
    });

    // Multi-file dictionary import via macOS-main-thread-safe `memento_os::open_files_on_main_thread`
    let add_window = window.as_weak();
    let add_coord = coord.clone();
    window.on_add_dictionary_archive(move || {
        let Some(window) = add_window.upgrade() else {
            return;
        };
        if window.get_dictionary_busy() {
            window.set_settings_status("Dictionary operation already in progress".into());
            return;
        }
        if add_coord.ctx.dictionary_manager.is_none() {
            window.set_settings_status("Dictionary manager is unavailable".into());
            return;
        }
        window.set_dictionary_busy(true);
        window.set_dictionary_progress_text("Selecting archives…".into());

        let picker_window = add_window.clone();
        let batch_paths_slot: Arc<Mutex<Vec<PathBuf>>> = Arc::new(Mutex::new(Vec::new()));
        let deliver_slot = batch_paths_slot.clone();
        let deliver_coord = add_coord.clone();
        let deliver_window = add_window.clone();
        let token = register_completion(move || {
            let paths = deliver_slot
                .lock()
                .map(|mut g| std::mem::take(&mut *g))
                .unwrap_or_default();
            if let Some(win) = deliver_window.upgrade() {
                deliver_coord.enqueue_or_start_batch(&win, paths);
            }
        });
        let mut cancel_guard = PickerCancelGuard::new(add_window.clone(), token);

        open_files_on_main_thread(
            archive_zip_batch_options(),
            |task| {
                let _ = slint::invoke_from_event_loop(task);
            },
            move |paths| {
                cancel_guard.mark_handled();
                if paths.is_empty() {
                    let _ = picker_window.upgrade_in_event_loop(|win| {
                        win.set_dictionary_busy(false);
                        win.set_dictionary_progress_text("".into());
                    });
                    return;
                }
                if let Ok(mut guard) = batch_paths_slot.lock() {
                    *guard = paths;
                }
                dispatch_completion(token);
            },
        );
    });

    // Background dictionary enable/disable
    let toggle_window = window.as_weak();
    let toggle_coord = coord.clone();
    window.on_dictionary_enabled_changed(move |id, enabled| {
        let Some(window) = toggle_window.upgrade() else {
            return;
        };
        if window.get_dictionary_busy() {
            refresh_dictionary_settings(
                &window,
                toggle_coord.ctx.dictionary_manager.as_deref(),
                &toggle_coord.ctx.config,
            );
            window.set_settings_status("Dictionary operation already in progress".into());
            return;
        }
        if toggle_coord.ctx.dictionary_manager.is_none() {
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
            &toggle_coord,
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
    let delete_coord = coord.clone();
    window.on_delete_dictionary(move |id| {
        let Some(window) = delete_window.upgrade() else {
            return;
        };
        if window.get_dictionary_busy() {
            window.set_settings_status("Dictionary operation already in progress".into());
            return;
        }
        if delete_coord.ctx.dictionary_manager.is_none() {
            window.set_settings_status("Dictionary manager is unavailable".into());
            return;
        }
        spawn_dictionary_mutation(
            &window,
            delete_window.clone(),
            &delete_coord,
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

    coord
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multi_zip_batch_queues_all_archives_continues_on_error_and_reports_summary() {
        let archives = vec![
            PathBuf::from("/tmp/JMdict.zip"),
            PathBuf::from("/tmp/corrupt.zip"),
            PathBuf::from("/tmp/Kanjidic.zip"),
        ];

        assert!(archive_zip_batch_options().multiple);
        assert!(is_zip_archive(Path::new("/tmp/Dict.ZIP")));
        assert!(!is_zip_archive(Path::new("/tmp/video.mkv")));

        let mut progress_log = Vec::new();
        let mut imported_list = Vec::new();
        let outcome = import_dictionary_batch(
            &archives,
            |path| {
                let name = path.file_name().unwrap().to_str().unwrap().to_owned();
                if name == "corrupt.zip" {
                    return Err("invalid zip header".to_owned());
                }
                let next_id = imported_list.len() as i64 + 1;
                imported_list.push(InstalledDictionary {
                    id: next_id,
                    title: name,
                    disabled: false,
                });
                Ok(imported_list.clone())
            },
            |idx, total, label| {
                progress_log.push(format_import_progress(idx, total, label));
            },
        );

        assert_eq!(
            progress_log,
            vec![
                "Importing 1/3: JMdict.zip…",
                "Importing 2/3: corrupt.zip…",
                "Importing 3/3: Kanjidic.zip…",
            ]
        );
        assert_eq!(outcome.total, 3);
        assert_eq!(outcome.succeeded, vec!["JMdict.zip", "Kanjidic.zip"]);
        assert_eq!(
            outcome.failed,
            vec![("corrupt.zip".to_owned(), "invalid zip header".to_owned())]
        );
        assert_eq!(outcome.dictionaries.as_ref().unwrap().len(), 2);
        let summary = outcome.format_status_summary();
        assert!(summary.contains("2/3"), "summary missing count: {summary}");
        assert!(
            summary.contains("corrupt.zip"),
            "summary missing failed archive: {summary}"
        );

        // All-success and all-failure summaries
        let all_ok = BatchImportOutcome {
            total: 2,
            succeeded: vec!["a.zip".into(), "b.zip".into()],
            failed: Vec::new(),
            dictionaries: None,
        };
        assert_eq!(all_ok.format_status_summary(), "Imported 2/2 dictionaries");

        let all_err = BatchImportOutcome {
            total: 2,
            succeeded: Vec::new(),
            failed: vec![
                ("a.zip".into(), "bad archive".into()),
                ("b.zip".into(), "missing index.json".into()),
            ],
            dictionaries: None,
        };
        assert!(all_err.format_status_summary().contains("Failed to import 2"));
    }

    #[test]
    fn drop_hover_state_responds_to_zip_hover_cancel_and_non_zip_drop() {
        use std::cell::Cell;
        use std::collections::HashMap;
        use std::sync::atomic::AtomicBool;
        use crate::controller::asr::AsrController;
        use crate::controller::subtitles::SubtitleController;
        use crate::{MementoApp, SubEntry};

        i_slint_backend_testing::init_no_event_loop();
        let dir = tempfile::tempdir().unwrap();
        let app = MementoApp::new().unwrap();
        let win = SettingsWindow::new().unwrap();
        let ctx = Rc::new(SettingsContext {
            app_weak: app.as_weak(),
            config: Rc::new(ConfigController::new()),
            dict_ctrl: Rc::new(RefCell::new(DictController::new())),
            dictionary_manager: None,
            dictionary_database_path: dir.path().join("dict.sqlite"),
            cache_dir: dir.path().to_path_buf(),
            anki_json_path: dir.path().join("anki_connect.json"),
            ocr_available: false,
            asr_session: Rc::new(RefCell::new(AsrController::new())),
            subtitles: Rc::new(RefCell::new(SubtitleController::new())),
            subtitle_model: Rc::new(VecModel::<SubEntry>::default()),
            asr_restart_pending: Rc::new(Cell::new(false)),
            mpv: Rc::new(RefCell::new(None)),
            coordinator: Rc::new(RefCell::new(None)),
            dirty_model: Rc::new(VecModel::from(vec![false; SettingsCategory::COUNT])),
            draft_snapshots: RefCell::new(HashMap::new()),
            help_window: RefCell::new(None),
            anki_auto_fetch_attempted: Cell::new(false),
            anki_auto_fetch_in_flight: Arc::new(AtomicBool::new(false)),
        });
        let coord = DictionaryBatchCoordinator::new(ctx, Arc::new(Mutex::new(None)));

        assert!(!win.get_drop_hover_active());
        coord.on_hovered_file(&win, Path::new("/tmp/video.mkv"));
        assert!(!win.get_drop_hover_active());
        coord.on_hovered_file(&win, Path::new("/tmp/JMdict.zip"));
        assert!(win.get_drop_hover_active());
        coord.on_hovered_file_cancelled(&win);
        assert!(!win.get_drop_hover_active());

        // Dropping a non-zip file clears hover and reports a clear skipped message
        coord.on_hovered_file(&win, Path::new("/tmp/JMdict.zip"));
        coord.on_dropped_file(&win, PathBuf::from("/tmp/readme.txt"));
        assert!(!win.get_drop_hover_active());
        assert!(win.get_settings_status().as_str().contains("Skipped non-ZIP"));
    }
}

//! Configuration controller connecting `subomi-config`, `subomi-setting`,
//! typed AnkiConnect profiles, and the Slint Settings UI.

mod anki_profiles;
mod audio_sources;
mod keybinds;
mod migration;
mod schema;
mod ui_groups;

use std::cell::RefCell;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};

use subomi_config::config::{ChangelogEntry, Config};
use subomi_config::{ArgbColor, FontSpec, Value};
use subomi_dict::searchcontroller::TextReplacementOptions;
use subomi_net::anki::AnkiEndpoint;
use subomi_setting::enums::ThemeMode;
use subomi_setting::keys as raw;

use crate::ui::SettingsGroup;

pub use anki_profiles::{
    anki_note_preset_fields, build_excluded_glossaries_checklist,
    excluded_glossaries_from_checklist, parse_excluded_glossaries, serialize_excluded_glossaries,
    toggle_excluded_glossary, validate_profile_name, AnkiNoteConfig, AnkiNotePreset,
    ExcludedGlossaryItem,
};
pub use audio_sources::AudioSourceEntry;
pub use keybinds::{
    reserved_shortcut_reason, AudioContextKeybindAction, KEYBIND_CLEAR_AUDIO_CONTEXT,
    KEYBIND_MARK_AUDIO_CONTEXT, KEYBIND_SUB_SCAN_NEXT, KEYBIND_SUB_SCAN_PREV,
    KEYBIND_SUB_SCAN_START, QUIT_SHORTCUT_MACOS, SETTINGS_SHORTCUT_DEFAULT,
    SETTINGS_SHORTCUT_MACOS,
};
pub use migration::is_volatile_cache_key;
pub use schema::{
    format_browse_selection, setting_browse_metadata, BrowseKind, SettingBrowseMetadata,
};

use anki_profiles::{AnkiProfile, AnkiProfilesFile};
use audio_sources::{
    audio_sources, default_audio_source, ensure_default_audio_source, write_audio_sources,
};
use keybinds::{
    active_keybind_profile_name, add_keybind_profile_with_options, compute_keybind_conflicts,
    ensure_default_keybind_profile, keybind_profile_names, keybind_profile_prefix,
    remove_keybind_profile, reset_keybind_profiles, validate_keybind_assignment,
};
use migration::{
    load_config_store, load_config_store_with_fallback, overlay_volatile_keys, strip_volatile_keys,
    MigrationPaths,
};
use schema::{
    asr_paths, behavior_paths, category_snapshot, declared_setting_default, effective_text,
    effective_value, interface_paths, normalize_loaded_asr_values, normalize_setting_value,
    ocr_paths, onnx_paths, parse_config_value, path_in_category, restore_category, search_paths,
    text_replacement_options_from_config, value_text, whisper_paths, write_default_paths,
    write_default_value, SettingsCategory, INTERFACE_FONT_KEYS,
};
use ui_groups::{
    application_settings_groups, asr_settings_groups, audio_settings_groups,
    behavior_settings_groups, dictionary_settings_groups, interface_settings_groups,
    keybind_settings_groups, ocr_settings_groups, search_settings_groups,
};

struct PersistedBaseline {
    target: PathBuf,
    config: Config,
}

struct CacheSplitState {
    dir: PathBuf,
    file_name: String,
    target: PathBuf,
    baseline: Config,
}

enum PendingWrite {
    Config {
        dir: PathBuf,
        file_name: String,
        config: Config,
    },
    AnkiJson {
        path: PathBuf,
        bytes: Vec<u8>,
    },
}

struct AsyncSaverState {
    /// Latest queued write per target file path (coalesces rapid bursts).
    queued: HashMap<PathBuf, PendingWrite>,
    in_flight: usize,
    last_error: Option<String>,
    /// Latest committed `(revision, changelog)` per canonical config path.
    committed_meta: HashMap<PathBuf, (u64, Vec<ChangelogEntry>)>,
    shutdown: bool,
}

/// Lazily spawned background atomic writer so rapid UI setting edits never
/// block the UI thread on `fsync`.
struct BackgroundSaver {
    shared: Arc<(Mutex<AsyncSaverState>, Condvar)>,
    thread: Option<JoinHandle<()>>,
}

impl BackgroundSaver {
    fn spawn() -> Self {
        let shared = Arc::new((
            Mutex::new(AsyncSaverState {
                queued: HashMap::new(),
                in_flight: 0,
                last_error: None,
                committed_meta: HashMap::new(),
                shutdown: false,
            }),
            Condvar::new(),
        ));
        let worker_shared = Arc::clone(&shared);
        let handle = thread::Builder::new()
            .name("subomi-config-save".to_owned())
            .spawn(move || {
                let (lock, cvar) = &*worker_shared;
                loop {
                    let next_job = {
                        let mut state = lock.lock().unwrap_or_else(|e| e.into_inner());
                        while state.queued.is_empty() && !state.shutdown {
                            state = cvar.wait(state).unwrap_or_else(|e| e.into_inner());
                        }
                        if let Some(key) = state.queued.keys().next().cloned() {
                            state.in_flight += 1;
                            let prior_meta = state.committed_meta.get(&key).cloned();
                            state.queued.remove(&key).map(|job| (key, job, prior_meta))
                        } else if state.shutdown {
                            break;
                        } else {
                            None
                        }
                    };

                    let Some((target, job, prior_meta)) = next_job else {
                        continue;
                    };

                    let outcome = match job {
                        PendingWrite::Config {
                            dir,
                            file_name,
                            mut config,
                        } => {
                            if let Some((prev_rev, prev_log)) = prior_meta {
                                if prev_rev > config.revision() {
                                    config.mark_persisted_with(prev_rev, prev_log, |_| false);
                                }
                            }
                            config
                                .save(&dir, &file_name)
                                .map(|()| Some((config.revision(), config.changelog().to_vec())))
                                .map_err(|error| format!("{error:?}"))
                        }
                        PendingWrite::AnkiJson { path, bytes } => {
                            write_atomic_private_file(&path, &bytes).map(|()| None)
                        }
                    };

                    let mut state = lock.lock().unwrap_or_else(|e| e.into_inner());
                    state.in_flight = state.in_flight.saturating_sub(1);
                    match outcome {
                        Ok(Some(meta)) => {
                            state.committed_meta.insert(target, meta);
                        }
                        Ok(None) => {}
                        Err(err) => {
                            state.last_error = Some(err);
                        }
                    }
                    cvar.notify_all();
                }
            })
            .ok();

        Self {
            shared,
            thread: handle,
        }
    }

    fn enqueue(&self, target: PathBuf, job: PendingWrite) -> Result<(), String> {
        if self.thread.is_none() {
            // Fallback synchronously if spawning the helper thread failed.
            return match job {
                PendingWrite::Config {
                    dir,
                    file_name,
                    mut config,
                } => config
                    .save(&dir, &file_name)
                    .map_err(|error| format!("{error:?}")),
                PendingWrite::AnkiJson { path, bytes } => write_atomic_private_file(&path, &bytes),
            };
        }
        let (lock, cvar) = &*self.shared;
        let mut state = lock.lock().unwrap_or_else(|e| e.into_inner());
        state.queued.insert(target, job);
        cvar.notify_all();
        Ok(())
    }

    fn flush(
        &self,
    ) -> (
        Result<(), String>,
        HashMap<PathBuf, (u64, Vec<ChangelogEntry>)>,
    ) {
        let (lock, cvar) = &*self.shared;
        let mut state = lock.lock().unwrap_or_else(|e| e.into_inner());
        while !state.queued.is_empty() || state.in_flight > 0 {
            state = cvar.wait(state).unwrap_or_else(|e| e.into_inner());
        }
        let err = state.last_error.take();
        let meta = std::mem::take(&mut state.committed_meta);
        match err {
            Some(msg) => (Err(msg), meta),
            None => (Ok(()), meta),
        }
    }
}

impl Drop for BackgroundSaver {
    fn drop(&mut self) {
        {
            let (lock, cvar) = &*self.shared;
            let mut state = lock.lock().unwrap_or_else(|e| e.into_inner());
            state.shutdown = true;
            cvar.notify_all();
        }
        if let Some(handle) = self.thread.take() {
            let _ = handle.join();
        }
    }
}

fn clone_config_state(config: &Config) -> Config {
    Config::from_document_text(&config.render_document())
        .unwrap_or_else(|_| Config::default_empty())
}

fn write_atomic_private_file(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let parent = path
        .parent()
        .filter(|dir| !dir.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)
        .map_err(|error| format!("could not create Anki config directory: {error}"))?;
    let mut builder = tempfile::Builder::new();
    builder.prefix(".anki_connect.").suffix(".tmp");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        builder.permissions(fs::Permissions::from_mode(0o600));
    }
    let mut temp = builder
        .tempfile_in(parent)
        .map_err(|error| format!("could not create temporary Anki profiles file: {error}"))?;
    {
        use std::io::Write;
        temp.as_file_mut()
            .write_all(bytes)
            .map_err(|error| format!("could not write Anki profiles: {error}"))?;
        temp.as_file()
            .sync_all()
            .map_err(|error| format!("could not flush Anki profiles: {error}"))?;
    }
    temp.persist(path)
        .map_err(|error| format!("could not save Anki profiles: {}", error.error))?;
    Ok(())
}

pub struct ConfigController {
    store: RefCell<Config>,
    persisted_baseline: RefCell<Option<PersistedBaseline>>,
    cache_split: RefCell<Option<CacheSplitState>>,
    anki_profiles: RefCell<AnkiProfilesFile>,
    async_saver: RefCell<Option<BackgroundSaver>>,
}

impl Default for ConfigController {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for ConfigController {
    fn drop(&mut self) {
        let _ = self.flush_async_saves();
    }
}

impl ConfigController {
    pub fn new() -> Self {
        Self::from_config(Config::default_empty())
    }

    pub fn from_config(mut config: Config) -> Self {
        ensure_default_audio_source(&mut config);
        ensure_default_keybind_profile(&mut config);
        normalize_loaded_asr_values(&mut config);
        Self {
            store: RefCell::new(config),
            persisted_baseline: RefCell::new(None),
            cache_split: RefCell::new(None),
            anki_profiles: RefCell::new(AnkiProfilesFile::default()),
            async_saver: RefCell::new(None),
        }
    }

    pub fn load<P: AsRef<Path>>(config_dir: P, file_name: &str) -> Result<Self, String> {
        let dir = config_dir.as_ref();
        let (fallback_dir, cache_dir, data_dir) = match crate::app::paths::resolve_app_dirs() {
            Some(dirs) if dirs.config_dir == dir => (
                dirs.legacy_config_fallback_dir,
                dirs.use_cache_config_split.then_some(dirs.cache_dir),
                Some(dirs.data_dir),
            ),
            _ => (None, None, None),
        };
        Self::load_with_fallback_and_cache(
            dir,
            file_name,
            fallback_dir.as_deref(),
            cache_dir.as_deref(),
            data_dir.as_deref(),
        )
    }

    /// Load config with explicit Linux XDG cache config split (`#337`):
    /// volatile keys (`recent/files`, `dictionary/order`, `main-window/*`) are
    /// read from and written to `cache_dir/file_name` while persistent settings
    /// stay in `config_dir/file_name`.
    pub fn load_with_cache_split<P: AsRef<Path>, Q: AsRef<Path>>(
        config_dir: P,
        cache_dir: Q,
        file_name: &str,
    ) -> Result<Self, String> {
        Self::load_with_fallback_and_cache(
            config_dir,
            file_name,
            None,
            Some(cache_dir.as_ref()),
            None,
        )
    }

    /// Load config with optional legacy fallback read directory (macOS
    /// `~/.config/subomi`, Windows `%APPDATA%\subomi`), optional Linux XDG
    /// cache split directory (`#337`), and optional data directory for legacy
    /// migrations (`v1→6`).
    pub fn load_with_fallback_and_cache<P: AsRef<Path>>(
        config_dir: P,
        file_name: &str,
        fallback_dir: Option<&Path>,
        cache_dir: Option<&Path>,
        data_dir: Option<&Path>,
    ) -> Result<Self, String> {
        let dir = config_dir.as_ref();
        let effective_cache_dir = cache_dir.filter(|c| *c != dir);
        let migration_paths = MigrationPaths {
            config_dir: Some(dir),
            data_dir,
        };

        if let Some(cdir) = effective_cache_dir {
            let mut cache_store = load_config_store(cdir, file_name)?;
            let mut primary_store = load_config_store_with_fallback(
                dir,
                file_name,
                fallback_dir,
                migration_paths,
                Some(&mut cache_store),
            )?;
            ensure_default_audio_source(&mut primary_store);
            ensure_default_keybind_profile(&mut primary_store);
            normalize_loaded_asr_values(&mut primary_store);

            let mut unified = clone_config_state(&primary_store);
            overlay_volatile_keys(&mut unified, &cache_store);
            let ctrl = Self::from_config(unified);

            let mut primary_baseline = clone_config_state(&primary_store);
            strip_volatile_keys(&mut primary_baseline);
            primary_baseline.mark_persisted_with(
                primary_store.revision(),
                primary_store.changelog().to_vec(),
                |_| true,
            );
            *ctrl.persisted_baseline.borrow_mut() = Some(PersistedBaseline {
                target: dir.join(file_name),
                config: primary_baseline,
            });
            *ctrl.cache_split.borrow_mut() = Some(CacheSplitState {
                dir: cdir.to_path_buf(),
                file_name: file_name.to_owned(),
                target: cdir.join(file_name),
                baseline: cache_store,
            });
            return Ok(ctrl);
        }

        let store = load_config_store_with_fallback(
            dir,
            file_name,
            fallback_dir,
            migration_paths,
            None,
        )?;
        let ctrl = Self::from_config(store);
        let baseline = clone_config_state(&ctrl.store.borrow());
        *ctrl.persisted_baseline.borrow_mut() = Some(PersistedBaseline {
            target: dir.join(file_name),
            config: baseline,
        });
        Ok(ctrl)
    }

    /// Enable or reconfigure Linux XDG cache config splitting (`#337`) on an
    /// existing controller, overlaying any volatile keys already stored in
    /// `cache_dir/file_name`.
    pub fn set_cache_split_dir<P: AsRef<Path>>(
        &self,
        cache_dir: P,
        file_name: &str,
    ) -> Result<(), String> {
        let _ = self.flush_async_saves();
        let cdir = cache_dir.as_ref();
        let cache_store = load_config_store(cdir, file_name)?;
        overlay_volatile_keys(&mut self.store.borrow_mut(), &cache_store);
        *self.cache_split.borrow_mut() = Some(CacheSplitState {
            dir: cdir.to_path_buf(),
            file_name: file_name.to_owned(),
            target: cdir.join(file_name),
            baseline: cache_store,
        });
        Ok(())
    }

    /// Return the active cache config path when Linux XDG cache splitting is enabled.
    pub fn cache_split_path(&self) -> Option<PathBuf> {
        self.cache_split.borrow().as_ref().map(|c| c.target.clone())
    }

    /// Waits for any pending background atomic writes to finish and synchronizes
    /// committed revision/changelog metadata back into the in-memory baseline.
    pub fn flush_async_saves(&self) -> Result<(), String> {
        let result = {
            let saver_ref = self.async_saver.borrow();
            let Some(saver) = saver_ref.as_ref() else {
                return Ok(());
            };
            saver.flush()
        };
        let (status, meta) = result;
        if let Some(baseline) = self.persisted_baseline.borrow_mut().as_mut() {
            if let Some((rev, changelog)) = meta.get(&baseline.target) {
                baseline
                    .config
                    .mark_persisted_with(*rev, changelog.clone(), |_| true);
                self.store
                    .borrow_mut()
                    .mark_persisted_against(&baseline.config, |_| false);
            }
        }
        if let Some(cache) = self.cache_split.borrow_mut().as_mut() {
            if let Some((rev, changelog)) = meta.get(&cache.target) {
                cache
                    .baseline
                    .mark_persisted_with(*rev, changelog.clone(), |_| true);
            }
        }
        status
    }

    fn ensure_baseline_for(&self, config_dir: &Path, settings_file: &str) -> Result<(), String> {
        let target = config_dir.join(settings_file);
        let matches_cached = self
            .persisted_baseline
            .borrow()
            .as_ref()
            .is_some_and(|b| b.target == target);
        if matches_cached {
            return Ok(());
        }
        let mut persisted = load_config_store(config_dir, settings_file)?;
        ensure_default_audio_source(&mut persisted);
        ensure_default_keybind_profile(&mut persisted);
        normalize_loaded_asr_values(&mut persisted);
        if self.cache_split.borrow().is_some() {
            let rev = persisted.revision();
            let log = persisted.changelog().to_vec();
            strip_volatile_keys(&mut persisted);
            persisted.mark_persisted_with(rev, log, |_| true);
        }
        *self.persisted_baseline.borrow_mut() = Some(PersistedBaseline {
            target,
            config: persisted,
        });
        Ok(())
    }

    fn enqueue_async_write(&self, target: PathBuf, job: PendingWrite) -> Result<(), String> {
        let mut saver_slot = self.async_saver.borrow_mut();
        let saver = saver_slot.get_or_insert_with(BackgroundSaver::spawn);
        saver.enqueue(target, job)
    }

    pub fn load_anki_profiles<P: AsRef<Path>>(&self, path: P) -> Result<(), String> {
        let _ = self.flush_async_saves();
        let path = path.as_ref();
        let config = match fs::read_to_string(path) {
            Ok(text) => AnkiProfilesFile::from_json_str(&text)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                AnkiProfilesFile::default()
            }
            Err(error) => return Err(format!("could not read Anki profiles: {error}")),
        };
        *self.anki_profiles.borrow_mut() = config;
        Ok(())
    }

    pub fn save_anki_profiles<P: AsRef<Path>>(&self, path: P) -> Result<(), String> {
        let _ = self.flush_async_saves();
        let text = self.anki_profiles.borrow().to_pretty_json()?;
        write_atomic_private_file(path.as_ref(), text.as_bytes())
    }

    /// Non-blocking atomic save of `anki_connect.json` on a background thread.
    pub fn save_anki_profiles_async<P: AsRef<Path>>(&self, path: P) -> Result<(), String> {
        let text = self.anki_profiles.borrow().to_pretty_json()?;
        let path = path.as_ref().to_path_buf();
        self.enqueue_async_write(
            path.clone(),
            PendingWrite::AnkiJson {
                path,
                bytes: text.into_bytes(),
            },
        )
    }

    pub fn store(&self) -> std::cell::Ref<'_, Config> {
        self.store.borrow()
    }

    pub fn store_mut(&mut self) -> &mut Config {
        self.store.get_mut()
    }

    pub fn borrow_store_mut(&self) -> std::cell::RefMut<'_, Config> {
        self.store.borrow_mut()
    }

    pub fn save(&self, dir: &Path, file_name: &str) -> Result<(), String> {
        let _ = self.flush_async_saves();
        if self.cache_split.borrow().is_some() {
            let mut store = self.store.borrow_mut();
            let mut cache_guard = self.cache_split.borrow_mut();
            let cache = cache_guard.as_mut().expect("cache split initialized");
            let mut cache_to_save = clone_config_state(&cache.baseline);
            strip_volatile_keys(&mut cache_to_save);
            overlay_volatile_keys(&mut cache_to_save, &store);
            cache_to_save
                .save(&cache.dir, &cache.file_name)
                .map_err(|e| format!("{e:?}"))?;
            cache.baseline = clone_config_state(&cache_to_save);

            let mut primary_to_save = clone_config_state(&store);
            strip_volatile_keys(&mut primary_to_save);
            primary_to_save
                .save(dir, file_name)
                .map_err(|e| format!("{e:?}"))?;
            store.mark_persisted_against(&primary_to_save, |_| true);
            *self.persisted_baseline.borrow_mut() = Some(PersistedBaseline {
                target: dir.join(file_name),
                config: clone_config_state(&primary_to_save),
            });
            return Ok(());
        }
        let mut store = self.store.borrow_mut();
        store.save(dir, file_name).map_err(|e| format!("{e:?}"))?;
        *self.persisted_baseline.borrow_mut() = Some(PersistedBaseline {
            target: dir.join(file_name),
            config: clone_config_state(&store),
        });
        Ok(())
    }

    /// Non-blocking atomic save of the full store on a background thread.
    pub fn save_async(&self, dir: &Path, file_name: &str) -> Result<(), String> {
        if self.cache_split.borrow().is_some() {
            let store_ref = self.store.borrow();
            let (cache_target, cache_dir, cache_file, cache_snapshot) = {
                let mut cache_guard = self.cache_split.borrow_mut();
                let cache = cache_guard.as_mut().expect("cache split initialized");
                let mut cache_snap = clone_config_state(&cache.baseline);
                strip_volatile_keys(&mut cache_snap);
                overlay_volatile_keys(&mut cache_snap, &store_ref);
                cache.baseline = clone_config_state(&cache_snap);
                (
                    cache.target.clone(),
                    cache.dir.clone(),
                    cache.file_name.clone(),
                    cache_snap,
                )
            };
            let mut primary_snapshot = clone_config_state(&store_ref);
            strip_volatile_keys(&mut primary_snapshot);
            drop(store_ref);

            let target = dir.join(file_name);
            *self.persisted_baseline.borrow_mut() = Some(PersistedBaseline {
                target: target.clone(),
                config: clone_config_state(&primary_snapshot),
            });
            self.enqueue_async_write(
                target,
                PendingWrite::Config {
                    dir: dir.to_path_buf(),
                    file_name: file_name.to_owned(),
                    config: primary_snapshot,
                },
            )?;
            return self.enqueue_async_write(
                cache_target,
                PendingWrite::Config {
                    dir: cache_dir,
                    file_name: cache_file,
                    config: cache_snapshot,
                },
            );
        }
        let snapshot = clone_config_state(&self.store.borrow());
        let target = dir.join(file_name);
        *self.persisted_baseline.borrow_mut() = Some(PersistedBaseline {
            target: target.clone(),
            config: clone_config_state(&snapshot),
        });
        self.enqueue_async_write(
            target,
            PendingWrite::Config {
                dir: dir.to_path_buf(),
                file_name: file_name.to_owned(),
                config: snapshot,
            },
        )
    }

    // ── Recent files & audio sources ────────────────────────────────────────

    pub fn recent_files(&self) -> Vec<String> {
        self.store.borrow().recent_files().unwrap_or_default()
    }

    pub fn audio_source_entries(&self) -> Vec<AudioSourceEntry> {
        let store = self.store.borrow();
        audio_sources(&store)
    }

    pub fn recent_files_add(&self, path: &str) -> Result<bool, String> {
        self.store
            .borrow_mut()
            .recent_files_add(path)
            .map_err(|e| format!("{e:?}"))
    }

    pub fn add_recent_file(&self, path: &str) -> Result<bool, String> {
        self.recent_files_add(path)
    }

    pub fn recent_files_clear(&self) -> Result<bool, String> {
        self.store
            .borrow_mut()
            .recent_files_clear()
            .map_err(|e| format!("{e:?}"))
    }

    pub fn clear_recent_files(&self) -> Result<bool, String> {
        self.recent_files_clear()
    }

    // ── Profiles, fonts, colors, ASR queries ────────────────────────────────

    pub fn current_anki_profile(&self) -> String {
        self.anki_profiles.borrow().current_profile_name()
    }

    pub fn anki_profile_names(&self) -> Vec<String> {
        self.anki_profiles.borrow().profile_names()
    }

    pub fn current_keybind_profile(&self) -> String {
        active_keybind_profile_name(&self.store.borrow())
    }

    pub fn keybind_profile_names(&self) -> Vec<String> {
        keybind_profile_names(&self.store.borrow())
    }

    /// Validate a candidate new profile name for `SettingsCategory::Anki` (0)
    /// or `SettingsCategory::Keybind` (6) before creation.
    pub fn validate_new_profile_name(&self, category: i32, name: &str) -> Result<String, String> {
        match SettingsCategory::from_i32(category)? {
            SettingsCategory::Anki => {
                validate_profile_name(name, &self.anki_profile_names(), true, None)
            }
            SettingsCategory::Keybind => {
                validate_profile_name(name, &self.keybind_profile_names(), false, None)
            }
            _ => Err(format!("settings category {category} does not use profiles")),
        }
    }

    /// Return the configured text replacement rules + search-original flag (`H6.5`).
    pub fn text_replacement_options(&self) -> TextReplacementOptions {
        text_replacement_options_from_config(&self.store.borrow())
    }

    /// Return the configured UI theme mode (`System`, `Dark`, `Light` — `#357`).
    pub fn theme_mode(&self) -> ThemeMode {
        match self.effective_setting(raw::interface::GROUP, raw::interface::THEME) {
            Some(Value::Text(s)) => ThemeMode::from_str_lossy(&s),
            _ => ThemeMode::System,
        }
    }

    /// Return the configured audio download timeout in milliseconds (`#1876`).
    pub fn audio_download_timeout_ms(&self) -> u64 {
        match self.effective_setting(raw::audio::GROUP, raw::audio::DOWNLOAD_TIMEOUT_MS) {
            Some(Value::Int(v)) => v.max(1000) as u64,
            _ => raw::audio::DOWNLOAD_TIMEOUT_MS_DEFAULT as u64,
        }
    }

    /// Return whether compact glossaries layout (`#220`) is enabled.
    pub fn compact_glossaries(&self) -> bool {
        matches!(
            self.effective_setting(raw::interface::GROUP, raw::interface::COMPACT_GLOSSARIES),
            Some(Value::Bool(true))
        )
    }

    /// Return whether compact dictionary tags (`#220`) is enabled.
    pub fn compact_tags(&self) -> bool {
        matches!(
            self.effective_setting(raw::interface::GROUP, raw::interface::COMPACT_TAGS),
            Some(Value::Bool(true))
        )
    }

    /// Return whether leaving a hover-paused subtitle resumes playback.
    pub fn hover_pause_resume(&self) -> bool {
        !matches!(
            self.effective_setting(raw::search::GROUP, raw::search::HOVER_PAUSE_RESUME),
            Some(Value::Bool(false))
        )
    }

    pub fn anki_endpoint(&self) -> AnkiEndpoint {
        self.anki_profiles
            .borrow()
            .current_profile()
            .map(AnkiProfile::endpoint)
            .unwrap_or_else(|| AnkiProfile::default().endpoint())
    }

    pub fn anki_enabled(&self) -> bool {
        self.anki_profiles.borrow().enabled
    }

    pub fn anki_show_tag(&self) -> bool {
        self.anki_profiles
            .borrow()
            .current_profile()
            .is_some_and(|p| p.show_tag)
    }

    pub fn apply_anki_note_preset(&self, preset_name: &str) -> Result<usize, String> {
        self.anki_profiles
            .borrow_mut()
            .apply_note_preset(preset_name)
    }

    pub fn keybind_conflicts(&self) -> Vec<(String, String, Vec<String>)> {
        compute_keybind_conflicts(&self.store.borrow())
    }

    pub fn keybind_conflict_count(&self) -> usize {
        self.keybind_conflicts().len()
    }

    pub fn edit_font_setting(
        &self,
        key: &str,
        family: &str,
        size: &str,
        bold: bool,
        italic: bool,
    ) -> Result<(), String> {
        let family = family.trim();
        if family.is_empty() {
            return Err("font family cannot be empty".to_owned());
        }
        let point_size = size
            .trim()
            .parse::<f64>()
            .ok()
            .filter(|v| v.is_finite() && *v > 0.0)
            .ok_or_else(|| "font size must be a positive number".to_owned())?;
        let (group, raw_key) = key
            .rsplit_once('/')
            .ok_or_else(|| format!("invalid font setting key: {key}"))?;
        let existing_raw = {
            let store = self.store.borrow();
            match effective_value(&store, group, raw_key) {
                Value::Text(raw) if !raw.is_empty() => Some(raw),
                _ => None,
            }
        };
        let serialized = FontSpec::new(family, point_size, bold, italic)
            .to_csv_string_preserving(existing_raw.as_deref());
        self.edit_setting(key, &serialized)
    }

    pub fn edit_color_setting(
        &self,
        key: &str,
        a: i32,
        r: i32,
        g: i32,
        b: i32,
    ) -> Result<(), String> {
        let color = ArgbColor::new(
            a.clamp(0, 255) as u8,
            r.clamp(0, 255) as u8,
            g.clamp(0, 255) as u8,
            b.clamp(0, 255) as u8,
        );
        self.edit_setting(key, &color.to_argb_hex())
    }

    pub fn configured_font_families(&self) -> Vec<String> {
        let store = self.store.borrow();
        let mut families = Vec::new();
        for key in INTERFACE_FONT_KEYS {
            if let Some(raw) = effective_text(&store, raw::interface::GROUP, key) {
                if let Ok(spec) = FontSpec::parse(&raw) {
                    if !families.contains(&spec.family) {
                        families.push(spec.family);
                    }
                }
            }
        }
        families
    }

    pub fn asr_backend_selected(&self) -> String {
        let store = self.store.borrow();
        effective_text(&store, raw::asr::GROUP, raw::asr::BACKEND)
            .filter(|name| super::asr::backend_available(name))
            .unwrap_or_else(|| super::asr::default_backend().to_owned())
    }

    pub fn whisper_selected_model(&self) -> String {
        let store = self.store.borrow();
        value_text(&effective_value(
            &store,
            raw::whisper::GROUP,
            raw::whisper::MODEL,
        ))
    }

    pub fn onnx_selected_model(&self) -> String {
        let store = self.store.borrow();
        value_text(&effective_value(&store, raw::onnx::GROUP, raw::onnx::MODEL))
    }

    pub fn anki_excluded_glossaries(&self) -> Vec<String> {
        self.anki_profiles
            .borrow()
            .current_profile()
            .map(|p| p.ex_glos.clone())
            .unwrap_or_default()
    }

    /// Structured dictionary checklist for the active Anki profile's `ex-glos`.
    pub fn anki_excluded_glossaries_checklist(
        &self,
        installed_dictionaries: &[String],
    ) -> Vec<ExcludedGlossaryItem> {
        self.anki_profiles
            .borrow()
            .excluded_glossaries_checklist(installed_dictionaries)
    }

    /// Toggle whether `dictionary_name` is excluded from `{glossary}` on the active Anki profile.
    pub fn set_anki_glossary_excluded(
        &self,
        dictionary_name: &str,
        excluded: bool,
    ) -> Result<(), String> {
        self.anki_profiles
            .borrow_mut()
            .set_glossary_excluded(dictionary_name, excluded)
    }

    pub fn anki_note_model(&self, section: &str) -> Option<String> {
        self.anki_profiles
            .borrow()
            .current_profile()?
            .note_section(section)
            .map(|note| note.model.clone())
    }

    pub fn set_anki_model_fields(&self, section: &str, fields: &[String]) -> Result<(), String> {
        self.anki_profiles
            .borrow_mut()
            .set_model_fields(section, fields)
    }

    pub fn anki_profile_note_config(&self) -> Option<AnkiNoteConfig> {
        self.anki_profiles.borrow().note_config("term")
    }

    pub fn anki_kanji_note_config(&self) -> Option<AnkiNoteConfig> {
        self.anki_profiles.borrow().note_config("kanji")
    }

    // ── Setting reads, edits, apply / reset / defaults ──────────────────────

    pub fn effective_setting(&self, group: &str, key: &str) -> Option<Value> {
        let store = self.store.borrow();
        store
            .get_at(group, key)
            .or_else(|| declared_setting_default(&store, group, key))
            .map(|value| normalize_setting_value(group, key, value))
    }

    pub fn get_setting(&self, group: &str, key: &str) -> Option<Value> {
        self.effective_setting(group, key)
    }

    pub fn explicit_setting(&self, group: &str, key: &str) -> Option<Value> {
        self.store.borrow().get_at(group, key)
    }

    pub fn settings_groups(&self) -> Vec<SettingsGroup> {
        let config = self.store.borrow();
        let mut groups = Vec::new();

        groups.extend(self.anki_settings_groups());
        groups.extend(application_settings_groups(&config));
        groups.extend(audio_settings_groups(&config));
        groups.extend(behavior_settings_groups(&config));
        groups.extend(dictionary_settings_groups(&config));
        groups.extend(interface_settings_groups(&config));
        groups.extend(keybind_settings_groups(&config));
        groups.extend(ocr_settings_groups(&config));
        groups.extend(asr_settings_groups(&config));
        groups.extend(search_settings_groups(&config));
        groups
    }

    pub fn edit_setting(&self, key: &str, value: &str) -> Result<(), String> {
        if key.starts_with("anki/") {
            return self.anki_profiles.borrow_mut().edit_setting(key, value);
        }

        let (group, raw_key) = key
            .rsplit_once('/')
            .ok_or_else(|| format!("invalid setting key: {key}"))?;
        if group == raw::keybind::GROUP && raw_key == raw::keybind::CURRENT_PROFILE {
            let profiles = keybind_profile_names(&self.store.borrow());
            if !profiles.iter().any(|name| name == value) {
                return Err(format!("keybind profile {value:?} does not exist"));
            }
        }

        let mut store = self.store.borrow_mut();
        if group.starts_with(&keybind_profile_prefix()) {
            validate_keybind_assignment(&store, group, raw_key, value)?;
        }
        let current = effective_value(&store, group, raw_key);
        let parsed = parse_config_value(group, raw_key, value, current)?;
        store
            .put_at(group, raw_key, &parsed)
            .map(|_| ())
            .map_err(|error| format!("{error:?}"))
    }

    /// Update and persist a single non-Anki setting key (such as a live toggle from
    /// the main window or an internal flag) without committing unrelated unsaved
    /// draft edits from the Options window.
    ///
    /// Reuses the in-memory last-persisted baseline so edits do not re-read
    /// `subomi.conf` from disk. When Linux XDG cache splitting (`#337`) is active,
    /// volatile keys (`recent/files`, `dictionary/order`, `main-window/*`) are
    /// persisted strictly to the cache file.
    pub fn persist_single_setting(
        &self,
        key: &str,
        value: &str,
        config_dir: &Path,
        settings_file: &str,
    ) -> Result<(), String> {
        let _ = self.flush_async_saves();
        let (group, raw_key) = key
            .rsplit_once('/')
            .ok_or_else(|| format!("invalid setting key: {key}"))?;
        let parsed = {
            let mut store = self.store.borrow_mut();
            if group.starts_with(&keybind_profile_prefix()) {
                validate_keybind_assignment(&store, group, raw_key, value)?;
            }
            let current = effective_value(&store, group, raw_key);
            let parsed = parse_config_value(group, raw_key, value, current)?;
            store
                .put_at(group, raw_key, &parsed)
                .map_err(|error| format!("{error:?}"))?;
            parsed
        };
        if is_volatile_cache_key(group, raw_key) && self.cache_split.borrow().is_some() {
            let mut cache_guard = self.cache_split.borrow_mut();
            let cache = cache_guard.as_mut().expect("cache split initialized");
            cache
                .baseline
                .put_at(group, raw_key, &parsed)
                .map_err(|error| format!("{error:?}"))?;
            cache
                .baseline
                .save(&cache.dir, &cache.file_name)
                .map_err(|error| format!("{error:?}"))?;
            let target_group = group.to_owned();
            let target_key = raw_key.to_owned();
            let mut store = self.store.borrow_mut();
            let rev = store.revision();
            let log = store.changelog().to_vec();
            store.mark_persisted_with(rev, log, |path| {
                path.group == target_group && path.key == target_key
            });
            return Ok(());
        }
        self.ensure_baseline_for(config_dir, settings_file)?;
        let mut baseline_guard = self.persisted_baseline.borrow_mut();
        let persisted = &mut baseline_guard
            .as_mut()
            .expect("baseline initialized")
            .config;
        persisted
            .put_at(group, raw_key, &parsed)
            .map_err(|error| format!("{error:?}"))?;
        persisted
            .save(config_dir, settings_file)
            .map_err(|error| format!("{error:?}"))?;
        let target_group = group.to_owned();
        let target_key = raw_key.to_owned();
        self.store
            .borrow_mut()
            .mark_persisted_against(persisted, |path| {
                path.group == target_group && path.key == target_key
            });
        Ok(())
    }

    /// Non-blocking variant of [`Self::persist_single_setting`] that updates the
    /// in-memory store and baseline immediately and writes atomically on a
    /// coalescing background thread.
    pub fn persist_single_setting_async(
        &self,
        key: &str,
        value: &str,
        config_dir: &Path,
        settings_file: &str,
    ) -> Result<(), String> {
        let (group, raw_key) = key
            .rsplit_once('/')
            .ok_or_else(|| format!("invalid setting key: {key}"))?;
        let parsed = {
            let mut store = self.store.borrow_mut();
            if group.starts_with(&keybind_profile_prefix()) {
                validate_keybind_assignment(&store, group, raw_key, value)?;
            }
            let current = effective_value(&store, group, raw_key);
            let parsed = parse_config_value(group, raw_key, value, current)?;
            store
                .put_at(group, raw_key, &parsed)
                .map_err(|error| format!("{error:?}"))?;
            parsed
        };
        if is_volatile_cache_key(group, raw_key) && self.cache_split.borrow().is_some() {
            let (target, cache_dir, cache_file, job_config) = {
                let mut cache_guard = self.cache_split.borrow_mut();
                let cache = cache_guard.as_mut().expect("cache split initialized");
                let mut snapshot = clone_config_state(&cache.baseline);
                snapshot
                    .put_at(group, raw_key, &parsed)
                    .map_err(|error| format!("{error:?}"))?;
                cache
                    .baseline
                    .put_at(group, raw_key, &parsed)
                    .map_err(|error| format!("{error:?}"))?;
                let target_group = group.to_owned();
                let target_key = raw_key.to_owned();
                let mut store = self.store.borrow_mut();
                let rev = store.revision();
                let log = store.changelog().to_vec();
                store.mark_persisted_with(rev, log, |path| {
                    path.group == target_group && path.key == target_key
                });
                (
                    cache.target.clone(),
                    cache.dir.clone(),
                    cache.file_name.clone(),
                    snapshot,
                )
            };
            return self.enqueue_async_write(
                target,
                PendingWrite::Config {
                    dir: cache_dir,
                    file_name: cache_file,
                    config: job_config,
                },
            );
        }
        self.ensure_baseline_for(config_dir, settings_file)?;
        let (target, job_config) = {
            let mut baseline_guard = self.persisted_baseline.borrow_mut();
            let baseline = baseline_guard.as_mut().expect("baseline initialized");
            let mut snapshot = clone_config_state(&baseline.config);
            snapshot
                .put_at(group, raw_key, &parsed)
                .map_err(|error| format!("{error:?}"))?;
            baseline
                .config
                .put_at(group, raw_key, &parsed)
                .map_err(|error| format!("{error:?}"))?;
            let target_group = group.to_owned();
            let target_key = raw_key.to_owned();
            self.store
                .borrow_mut()
                .mark_persisted_against(&baseline.config, |path| {
                    path.group == target_group && path.key == target_key
                });
            (baseline.target.clone(), snapshot)
        };
        self.enqueue_async_write(
            target,
            PendingWrite::Config {
                dir: config_dir.to_path_buf(),
                file_name: settings_file.to_owned(),
                config: job_config,
            },
        )
    }

    pub fn apply_settings(
        &self,
        category: i32,
        config_dir: &Path,
        settings_file: &str,
        anki_json_path: &Path,
    ) -> Result<(), String> {
        let cat = SettingsCategory::from_i32(category)?;
        if cat == SettingsCategory::Anki {
            return self.save_anki_profiles(anki_json_path);
        }
        let _ = self.flush_async_saves();

        let snapshot = category_snapshot(&self.store.borrow(), cat);
        if cat == SettingsCategory::Dictionary && self.cache_split.borrow().is_some() {
            let mut cache_guard = self.cache_split.borrow_mut();
            let cache = cache_guard.as_mut().expect("cache split initialized");
            restore_category(&mut cache.baseline, &snapshot);
            cache
                .baseline
                .save(&cache.dir, &cache.file_name)
                .map_err(|error| format!("{error:?}"))?;
            let mut store = self.store.borrow_mut();
            let rev = store.revision();
            let log = store.changelog().to_vec();
            store.mark_persisted_with(rev, log, |path| path_in_category(cat, path));
            return Ok(());
        }
        self.ensure_baseline_for(config_dir, settings_file)?;
        let mut baseline_guard = self.persisted_baseline.borrow_mut();
        let persisted = &mut baseline_guard
            .as_mut()
            .expect("baseline initialized")
            .config;
        restore_category(persisted, &snapshot);
        persisted
            .save(config_dir, settings_file)
            .map_err(|error| format!("{error:?}"))?;
        self.store
            .borrow_mut()
            .mark_persisted_against(persisted, |path| path_in_category(cat, path));
        Ok(())
    }

    /// Non-blocking variant of [`Self::apply_settings`] for responsive UI applies.
    pub fn apply_settings_async(
        &self,
        category: i32,
        config_dir: &Path,
        settings_file: &str,
        anki_json_path: &Path,
    ) -> Result<(), String> {
        let cat = SettingsCategory::from_i32(category)?;
        if cat == SettingsCategory::Anki {
            return self.save_anki_profiles_async(anki_json_path);
        }
        let snapshot = category_snapshot(&self.store.borrow(), cat);
        if cat == SettingsCategory::Dictionary && self.cache_split.borrow().is_some() {
            let (target, cache_dir, cache_file, job_config) = {
                let mut cache_guard = self.cache_split.borrow_mut();
                let cache = cache_guard.as_mut().expect("cache split initialized");
                let mut job_cfg = clone_config_state(&cache.baseline);
                restore_category(&mut job_cfg, &snapshot);
                restore_category(&mut cache.baseline, &snapshot);
                let mut store = self.store.borrow_mut();
                let rev = store.revision();
                let log = store.changelog().to_vec();
                store.mark_persisted_with(rev, log, |path| path_in_category(cat, path));
                (
                    cache.target.clone(),
                    cache.dir.clone(),
                    cache.file_name.clone(),
                    job_cfg,
                )
            };
            return self.enqueue_async_write(
                target,
                PendingWrite::Config {
                    dir: cache_dir,
                    file_name: cache_file,
                    config: job_config,
                },
            );
        }
        self.ensure_baseline_for(config_dir, settings_file)?;
        let (target, job_config) = {
            let mut baseline_guard = self.persisted_baseline.borrow_mut();
            let baseline = baseline_guard.as_mut().expect("baseline initialized");
            let mut job_cfg = clone_config_state(&baseline.config);
            restore_category(&mut job_cfg, &snapshot);
            restore_category(&mut baseline.config, &snapshot);
            self.store
                .borrow_mut()
                .mark_persisted_against(&baseline.config, |path| path_in_category(cat, path));
            (baseline.target.clone(), job_cfg)
        };
        self.enqueue_async_write(
            target,
            PendingWrite::Config {
                dir: config_dir.to_path_buf(),
                file_name: settings_file.to_owned(),
                config: job_config,
            },
        )
    }

    pub fn reset_settings(
        &self,
        category: i32,
        config_dir: &Path,
        settings_file: &str,
        anki_json_path: &Path,
    ) -> Result<(), String> {
        let cat = SettingsCategory::from_i32(category)?;
        if cat == SettingsCategory::Anki {
            return self.load_anki_profiles(anki_json_path);
        }
        let _ = self.flush_async_saves();

        if cat == SettingsCategory::Dictionary && self.cache_split.borrow().is_some() {
            let snapshot = {
                let cache_guard = self.cache_split.borrow();
                let cache = cache_guard.as_ref().expect("cache split initialized");
                category_snapshot(&cache.baseline, cat)
            };
            let mut store = self.store.borrow_mut();
            restore_category(&mut store, &snapshot);
            let rev = store.revision();
            let log = store.changelog().to_vec();
            store.mark_persisted_with(rev, log, |path| path_in_category(cat, path));
            return Ok(());
        }

        self.ensure_baseline_for(config_dir, settings_file)?;
        let (snapshot, revision, changelog) = {
            let baseline_guard = self.persisted_baseline.borrow();
            let persisted = &baseline_guard
                .as_ref()
                .expect("baseline initialized")
                .config;
            (
                category_snapshot(persisted, cat),
                persisted.revision(),
                persisted.changelog().to_vec(),
            )
        };
        {
            let mut store = self.store.borrow_mut();
            restore_category(&mut store, &snapshot);
            store.mark_persisted_with(revision, changelog, |path| path_in_category(cat, path));
        }
        if cat == SettingsCategory::Audio && audio_sources(&self.store.borrow()).is_empty() {
            self.defaults_settings(category)?;
        } else if cat == SettingsCategory::Keybind
            && keybind_profile_names(&self.store.borrow()).is_empty()
        {
            self.defaults_settings(category)?;
        }
        Ok(())
    }

    pub fn defaults_settings(&self, category: i32) -> Result<(), String> {
        let cat = SettingsCategory::from_i32(category)?;
        if cat == SettingsCategory::Anki {
            self.anki_profiles
                .borrow_mut()
                .reset_current_profile_defaults();
            return Ok(());
        }
        let mut store = self.store.borrow_mut();
        match cat {
            SettingsCategory::Application => {
                write_default_value(
                    &mut store,
                    raw::application::GROUP,
                    raw::application::LANGUAGE,
                );
                write_default_value(
                    &mut store,
                    raw::application::GROUP,
                    raw::application::AUTO_UPDATE_CHECK,
                );
                write_default_value(
                    &mut store,
                    raw::application::GROUP,
                    raw::application::UPDATE_CHANNEL,
                );
            }
            SettingsCategory::Audio => {
                write_audio_sources(&mut store, &[default_audio_source()])?;
                write_default_value(
                    &mut store,
                    raw::audio::GROUP,
                    raw::audio::DOWNLOAD_TIMEOUT_MS,
                );
            }
            SettingsCategory::Behavior => write_default_paths(&mut store, behavior_paths()),
            SettingsCategory::Dictionary => {
                write_default_value(&mut store, raw::dictionary::GROUP, raw::dictionary::ORDER);
            }
            SettingsCategory::Interface => write_default_paths(&mut store, interface_paths()),
            SettingsCategory::Keybind => reset_keybind_profiles(&mut store)?,
            SettingsCategory::Ocr => write_default_paths(&mut store, ocr_paths()),
            SettingsCategory::Asr => {
                write_default_paths(&mut store, asr_paths());
                let backend = super::asr::default_backend();
                store
                    .put_at(
                        raw::asr::GROUP,
                        raw::asr::BACKEND,
                        &Value::Text(backend.to_owned()),
                    )
                    .map_err(|error| format!("{error:?}"))?;
                write_default_paths(&mut store, whisper_paths());
                write_default_paths(&mut store, onnx_paths());
                for (group, key) in [
                    (raw::whisper::GROUP, raw::whisper::THREADS),
                    (raw::onnx::GROUP, raw::onnx::THREADS),
                ] {
                    let value = normalize_setting_value(group, key, Value::Int(0));
                    store
                        .put_at(group, key, &value)
                        .map_err(|error| format!("{error:?}"))?;
                }
            }
            SettingsCategory::Search => write_default_paths(&mut store, search_paths()),
            SettingsCategory::Anki => unreachable!("handled above"),
        }
        Ok(())
    }

    /// Create a new Anki/Keybind profile or Audio source entry. For Anki and
    /// Keybind profiles, `copy_current` controls whether the new profile clones
    /// the active profile (`true`) or starts from factory defaults (`false`).
    pub fn add_entry_with_options(
        &self,
        category: i32,
        name: &str,
        copy_current: bool,
    ) -> Result<(), String> {
        match SettingsCategory::from_i32(category).ok() {
            Some(SettingsCategory::Anki) => self
                .anki_profiles
                .borrow_mut()
                .add_profile_with_options(name, copy_current),
            Some(SettingsCategory::Audio) => {
                let mut store = self.store.borrow_mut();
                let mut sources = audio_sources(&store);
                sources.push(AudioSourceEntry {
                    name: String::new(),
                    url: String::new(),
                    kind: 0,
                    md5: String::new(),
                });
                write_audio_sources(&mut store, &sources)
            }
            Some(SettingsCategory::Keybind) => {
                add_keybind_profile_with_options(&mut self.store.borrow_mut(), name, copy_current)
            }
            _ => Err(format!(
                "settings category {category} does not support entries"
            )),
        }
    }

    pub fn add_entry(&self, category: i32, name: &str) -> Result<(), String> {
        self.add_entry_with_options(category, name, true)
    }

    pub fn remove_entry(&self, category: i32, id: &str) -> Result<(), String> {
        match SettingsCategory::from_i32(category).ok() {
            Some(SettingsCategory::Anki) => self.anki_profiles.borrow_mut().remove_profile(id),
            Some(SettingsCategory::Audio) => {
                let index = id
                    .parse::<usize>()
                    .map_err(|_| format!("invalid audio source id: {id}"))?;
                let mut store = self.store.borrow_mut();
                let mut sources = audio_sources(&store);
                if index == 0 || index > sources.len() {
                    return Err(format!("audio source {id} does not exist"));
                }
                sources.remove(index - 1);
                write_audio_sources(&mut store, &sources)
            }
            Some(SettingsCategory::Keybind) => {
                remove_keybind_profile(&mut self.store.borrow_mut(), id)
            }
            _ => Err(format!(
                "settings category {category} does not support entries"
            )),
        }
    }

    pub fn move_entry(&self, category: i32, id: &str, direction: i32) -> Result<(), String> {
        if SettingsCategory::from_i32(category).ok() != Some(SettingsCategory::Audio) {
            return Err(format!(
                "settings category {category} does not support moving entries"
            ));
        }
        let index = id
            .parse::<usize>()
            .map_err(|_| format!("invalid audio source id: {id}"))?;
        let mut store = self.store.borrow_mut();
        let mut sources = audio_sources(&store);
        if index == 0 || index > sources.len() {
            return Err(format!("audio source {id} does not exist"));
        }
        let from = index as isize - 1;
        let to = from + direction.signum() as isize;
        if to < 0 || to >= sources.len() as isize || to == from {
            return Ok(());
        }
        let source = sources.remove(from as usize);
        sources.insert(to as usize, source);
        write_audio_sources(&mut store, &sources)
    }

    fn anki_settings_groups(&self) -> Vec<SettingsGroup> {
        self.anki_profiles.borrow().settings_groups()
    }

    pub fn category_settings_groups(&self, category: i32) -> Vec<SettingsGroup> {
        let Ok(cat) = SettingsCategory::from_i32(category) else {
            return Vec::new();
        };
        let config = self.store.borrow();
        match cat {
            SettingsCategory::Anki => self.anki_settings_groups(),
            SettingsCategory::Application => application_settings_groups(&config),
            SettingsCategory::Audio => audio_settings_groups(&config),
            SettingsCategory::Behavior => behavior_settings_groups(&config),
            SettingsCategory::Dictionary => dictionary_settings_groups(&config),
            SettingsCategory::Interface => interface_settings_groups(&config),
            SettingsCategory::Keybind => keybind_settings_groups(&config),
            SettingsCategory::Ocr => ocr_settings_groups(&config),
            SettingsCategory::Asr => asr_settings_groups(&config),
            SettingsCategory::Search => search_settings_groups(&config),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEST_DIR_ID: AtomicU64 = AtomicU64::new(0);

    fn temp_dir(label: &str) -> PathBuf {
        let id = NEXT_TEST_DIR_ID.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "subomi-ctrl-cfg-{label}-{}-{id}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn keybind_profile_rejects_slash() {
        let ctrl = ConfigController::new();
        let err = ctrl.add_entry(6, "bad/profile").unwrap_err();
        assert!(err.contains("'/'"), "unexpected error: {err}");
        ctrl.add_entry(6, "ValidProfile").unwrap();
        assert_eq!(ctrl.current_keybind_profile(), "ValidProfile");
    }

    #[test]
    fn save_anki_profiles_writes_atomically_with_private_permissions() {
        let dir = temp_dir("anki-atomic");
        let path = dir.join("anki_connect.json");
        let ctrl = ConfigController::new();
        ctrl.edit_setting("anki/profile/api-key", "super-secret")
            .unwrap();
        ctrl.save_anki_profiles(&path).unwrap();
        assert!(path.is_file());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
        let reloaded = ConfigController::new();
        reloaded.load_anki_profiles(&path).unwrap();
        assert_eq!(reloaded.anki_endpoint().api_key.as_deref(), None);
        ctrl.edit_setting("anki/profile/use-api-key", "true")
            .unwrap();
        ctrl.save_anki_profiles(&path).unwrap();
        reloaded.load_anki_profiles(&path).unwrap();
        assert_eq!(
            reloaded.anki_endpoint().api_key.as_deref(),
            Some("super-secret")
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn anki_profiles_accepts_numeric_port_and_rejects_non_http_schemes() {
        let dir = temp_dir("anki-port-host");
        let path = dir.join("anki_connect.json");
        fs::write(
            &path,
            r#"{"enabled":true,"setProfile":"Default","profiles":[{"name":"Default","host":"127.0.0.1","port":8766}]}"#,
        )
        .unwrap();

        let ctrl = ConfigController::new();
        ctrl.load_anki_profiles(&path).unwrap();
        let ep = ctrl.anki_endpoint();
        assert_eq!(ep.hostname, "http://127.0.0.1");
        assert_eq!(ep.port, 8766);

        // Non-http(s) schemes must be rejected instead of becoming http://ftp://bad.
        assert!(ctrl.edit_setting("anki/profile/host", "ftp://bad").is_err());
        assert!(ctrl.edit_setting("anki/profile/host", "ws://bad").is_err());
        ctrl.edit_setting("anki/profile/host", "https://anki.local")
            .unwrap();
        assert_eq!(ctrl.anki_endpoint().hostname, "https://anki.local");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn anki_body_groups_omit_header_duplicates() {
        use slint::Model;
        let ctrl = ConfigController::new();
        let groups = ctrl.category_settings_groups(0);
        for g in &groups {
            for idx in 0..g.fields.row_count() {
                let f = g.fields.row_data(idx).unwrap();
                assert_ne!(f.key, "anki/profile", "duplicate Profile row in body");
                assert_ne!(
                    f.key, "anki/profile/show-tag",
                    "duplicate Show tag row in body"
                );
            }
        }
    }

    #[test]
    fn apply_settings_and_save_do_not_split_brain_revision_or_changelog() {
        let dir = temp_dir("split-brain");
        let anki = dir.join("anki_connect.json");
        let ctrl = ConfigController::new();
        ctrl.save(&dir, "subomi.conf").unwrap();
        assert_eq!(ctrl.store().revision(), 1);
        assert!(!ctrl.store().is_dirty());

        ctrl.edit_setting("behavior/autofit", "false").unwrap();
        ctrl.apply_settings(3, &dir, "subomi.conf", &anki).unwrap();
        assert_eq!(ctrl.store().revision(), 2);
        assert!(!ctrl.store().is_dirty());

        ctrl.recent_files_add("/tmp/video.mkv").unwrap();
        ctrl.save(&dir, "subomi.conf").unwrap();
        assert_eq!(ctrl.store().revision(), 3);

        let reloaded = ConfigController::load(&dir, "subomi.conf").unwrap();
        assert_eq!(reloaded.store().revision(), 3);
        assert_eq!(reloaded.store().changelog().len(), 3);
        assert_eq!(
            reloaded.effective_setting(raw::behavior::GROUP, raw::behavior::AUTO_FIT),
            Some(Value::Bool(false))
        );
        assert_eq!(reloaded.recent_files(), vec!["/tmp/video.mkv"]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn async_persist_coalesces_and_flushes_atomically() {
        let dir = temp_dir("async-save");
        let ctrl = ConfigController::new();
        ctrl.persist_single_setting_async("behavior/osc-duration", "500", &dir, "subomi.conf")
            .unwrap();
        ctrl.persist_single_setting_async("behavior/osc-duration", "750", &dir, "subomi.conf")
            .unwrap();
        ctrl.flush_async_saves().unwrap();

        let reloaded = ConfigController::load(&dir, "subomi.conf").unwrap();
        assert_eq!(
            reloaded.effective_setting(raw::behavior::GROUP, raw::behavior::OSC_DURATION),
            Some(Value::Int(750))
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn legacy_ini_runs_subomi_setting_migration() {
        let dir = temp_dir("legacy-migrate");
        let conf_path = dir.join("subomi.conf");
        let anki_path = dir.join("anki_connect.json");
        fs::write(&conf_path, "version=2\n[search]\nlist-result=false\n").unwrap();

        let loaded = ConfigController::load(&dir, "subomi.conf").unwrap();
        assert_eq!(
            loaded.effective_setting(raw::search::GROUP, raw::search::GLOSSARY_STYLE),
            Some(Value::Int(1))
        );
        assert_eq!(loaded.store().version().unwrap(), 6);

        loaded
            .apply_settings(1, &dir, "subomi.conf", &anki_path)
            .unwrap();
        assert!(
            !loaded.store().is_dirty(),
            "migrated legacy INI keys persisted to disk must not remain pending after apply_settings"
        );
        assert_eq!(loaded.store().revision(), 1);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn osc_min_move_has_no_artificial_upper_clamp() {
        let ctrl = ConfigController::new();
        ctrl.edit_setting("behavior/osc-min-move", "250").unwrap();
        assert_eq!(
            ctrl.effective_setting(raw::behavior::GROUP, raw::behavior::OSC_MIN_MOVE),
            Some(Value::Int(250))
        );
        ctrl.edit_setting("behavior/osc-min-move", "5000").unwrap();
        assert_eq!(
            ctrl.effective_setting(raw::behavior::GROUP, raw::behavior::OSC_MIN_MOVE),
            Some(Value::Int(5000))
        );
        ctrl.edit_setting("behavior/osc-min-move", "-12").unwrap();
        assert_eq!(
            ctrl.effective_setting(raw::behavior::GROUP, raw::behavior::OSC_MIN_MOVE),
            Some(Value::Int(0))
        );
    }

    #[test]
    fn subtitle_list_behavior_settings_are_wired_and_persisted() {
        let dir = temp_dir("sub-list-prefs");
        let anki = dir.join("anki_connect.json");
        let ctrl = ConfigController::new();

        ctrl.edit_setting("subtitle-list/ignore-whitespace", "true")
            .unwrap();
        ctrl.edit_setting("subtitle-list/auto-seek", "true")
            .unwrap();
        ctrl.apply_settings(5, &dir, "subomi.conf", &anki).unwrap();

        let reloaded = ConfigController::load(&dir, "subomi.conf").unwrap();
        assert_eq!(
            reloaded.effective_setting(
                raw::subtitle_list::GROUP,
                raw::subtitle_list::IGNORE_WHITESPACE
            ),
            Some(Value::Bool(true))
        );
        assert_eq!(
            reloaded.effective_setting(raw::subtitle_list::GROUP, raw::subtitle_list::AUTO_SEEK),
            Some(Value::Bool(true))
        );

        reloaded.defaults_settings(5).unwrap();
        assert_eq!(
            reloaded.effective_setting(
                raw::subtitle_list::GROUP,
                raw::subtitle_list::IGNORE_WHITESPACE
            ),
            Some(Value::Bool(false))
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn font_and_color_editors_preserve_qfont_and_argb_formats() {
        let ctrl = ConfigController::new();
        ctrl.edit_font_setting("interface/sub-font", "Source Han Sans JP", "28", true, true)
            .unwrap();
        assert_eq!(
            ctrl.effective_setting(raw::interface::GROUP, raw::interface::subtitle::FONT),
            Some(Value::Text(
                "Source Han Sans JP,28,-1,5,75,1,0,0,0,0".to_owned()
            ))
        );

        ctrl.edit_color_setting("interface/sub-text-color", 200, 16, 32, 48)
            .unwrap();
        assert_eq!(
            ctrl.effective_setting(raw::interface::GROUP, raw::interface::subtitle::TEXT_COLOR),
            Some(Value::Text("#C8102030".to_owned()))
        );
    }

    #[test]
    fn keybind_conflicts_reject_duplicates_and_reserved_and_detect_loaded_conflicts() {
        let mut ctrl = ConfigController::new();
        assert!(ctrl.keybind_conflicts().is_empty());

        let profile = ctrl.current_keybind_profile();
        // Live edits reject duplicate chords (case- & modifier-order-insensitively).
        let dup_chord = if cfg!(target_os = "macos") {
            "Shift+Cmd+C"
        } else {
            "Shift+Ctrl+C"
        };
        let dup_err = ctrl
            .edit_setting(&format!("keybind/profiles/{profile}/open-file"), dup_chord)
            .unwrap_err();
        assert!(
            dup_err.contains("already assigned"),
            "unexpected duplicate error: {dup_err}"
        );

        // Reserved Settings shortcut (`Ctrl+,` / `Cmd+,`) is rejected.
        for reserved in ["Ctrl+,", "Cmd+,", "Control+Comma"] {
            let err = ctrl
                .edit_setting(&format!("keybind/profiles/{profile}/open-file"), reserved)
                .unwrap_err();
            assert!(
                err.contains("reserved for Settings"),
                "expected reserved Settings error for {reserved}, got: {err}"
            );
        }
        // `Cmd+Q` is reserved on macOS.
        assert_eq!(reserved_shortcut_reason("Cmd+Q", true), Some("Quit"));
        assert_eq!(reserved_shortcut_reason("Cmd+Q", false), None);

        // Clearing a shortcut to empty string is allowed, and assigning a unique chord succeeds.
        ctrl.edit_setting(&format!("keybind/profiles/{profile}/open-file"), "")
            .unwrap();
        ctrl.edit_setting(&format!("keybind/profiles/{profile}/open-file"), "Ctrl+Alt+O")
            .unwrap();

        // Pre-existing duplicates loaded from disk are still detected by `keybind_conflicts()`.
        ctrl.store_mut()
            .put_at(
                &format!("keybind/profiles/{profile}"),
                "open-file",
                &Value::Text(dup_chord.to_owned()),
            )
            .unwrap();
        let conflicts = ctrl.keybind_conflicts();
        assert_eq!(conflicts.len(), 2);
        assert!(conflicts.iter().any(|(k, _, _)| k == "open-file"));
        assert!(conflicts
            .iter()
            .any(|(k, _, _)| k == "sub-list-copy-selected"));
    }

    #[test]
    fn keybind_macos_defaults_audio_context_actions_and_ocr_gating() {
        use super::keybinds::{keybind_default_for_os, visible_keybind_fields_for};

        assert_eq!(keybind_default_for_os(raw::keybind::profile::OPEN_FILE, true), "Cmd+O");
        assert_eq!(keybind_default_for_os(raw::keybind::profile::OPEN_URL, true), "Cmd+Shift+O");
        assert_eq!(keybind_default_for_os(raw::keybind::profile::SUBTITLE_LIST_FIND, true), "Cmd+F");
        assert_eq!(keybind_default_for_os(raw::keybind::profile::SUBTITLE_LIST_FIND_NEXT, true), "Cmd+G");
        assert_eq!(
            keybind_default_for_os(raw::keybind::profile::SUBTITLE_LIST_FIND_PREVIOUS, true),
            "Cmd+Shift+G"
        );
        assert_eq!(keybind_default_for_os(KEYBIND_MARK_AUDIO_CONTEXT, true), "Cmd+B");
        assert_eq!(keybind_default_for_os(KEYBIND_CLEAR_AUDIO_CONTEXT, true), "Cmd+Shift+B");
        assert_eq!(keybind_default_for_os(KEYBIND_MARK_AUDIO_CONTEXT, false), "Ctrl+B");
        assert_eq!(keybind_default_for_os(KEYBIND_CLEAR_AUDIO_CONTEXT, false), "Ctrl+Shift+B");
        assert_eq!(keybind_default_for_os(KEYBIND_SUB_SCAN_START, true), "Cmd+K");
        assert_eq!(keybind_default_for_os(KEYBIND_SUB_SCAN_NEXT, true), "Cmd+Right");
        assert_eq!(keybind_default_for_os(KEYBIND_SUB_SCAN_PREV, true), "Cmd+Left");
        assert_eq!(keybind_default_for_os(KEYBIND_SUB_SCAN_START, false), "Ctrl+K");
        assert_eq!(keybind_default_for_os(KEYBIND_SUB_SCAN_NEXT, false), "Ctrl+Right");
        assert_eq!(keybind_default_for_os(KEYBIND_SUB_SCAN_PREV, false), "Ctrl+Left");

        let without_ocr = visible_keybind_fields_for(false);
        assert!(!without_ocr.iter().any(|(k, _)| *k == raw::keybind::profile::START_OCR));
        assert!(without_ocr.iter().any(|(k, _)| *k == KEYBIND_MARK_AUDIO_CONTEXT));
        assert!(without_ocr.iter().any(|(k, _)| *k == KEYBIND_CLEAR_AUDIO_CONTEXT));
        assert!(without_ocr.iter().any(|(k, _)| *k == KEYBIND_SUB_SCAN_START));
        assert!(without_ocr.iter().any(|(k, _)| *k == KEYBIND_SUB_SCAN_NEXT));
        assert!(without_ocr.iter().any(|(k, _)| *k == KEYBIND_SUB_SCAN_PREV));

        let with_ocr = visible_keybind_fields_for(true);
        assert!(with_ocr.iter().any(|(k, _)| *k == raw::keybind::profile::START_OCR));
        assert!(with_ocr.iter().any(|(k, _)| *k == KEYBIND_SUB_SCAN_START));

        // Default profile must have zero conflicts and allow rebinding to vim-style keys (H/L).
        let ctrl = ConfigController::new();
        assert_eq!(ctrl.keybind_conflict_count(), 0);
        let profile = ctrl.current_keybind_profile();
        ctrl.edit_setting(&format!("keybind/profiles/{profile}/{KEYBIND_SUB_SCAN_START}"), "Alt+S")
            .unwrap();
        ctrl.edit_setting(&format!("keybind/profiles/{profile}/{KEYBIND_SUB_SCAN_NEXT}"), "L")
            .unwrap();
        ctrl.edit_setting(&format!("keybind/profiles/{profile}/{KEYBIND_SUB_SCAN_PREV}"), "H")
            .unwrap();
        assert_eq!(ctrl.keybind_conflict_count(), 0);
    }

    #[test]
    fn new_schema_fields_dead_switch_hiding_and_browse_metadata() {
        use slint::Model;

        let dir = temp_dir("new-schema");
        let anki = dir.join("anki_connect.json");
        let ctrl = ConfigController::new();

        // Exposed in UI groups: theme, compact-glossaries, compact-tags,
        // text-replacements, text-replacements-search-original, hover-pause-resume,
        // audio/download-timeout-ms.
        let all_keys: Vec<String> = ctrl
            .settings_groups()
            .into_iter()
            .flat_map(|g| {
                (0..g.fields.row_count())
                    .filter_map(move |i| g.fields.row_data(i).map(|f| f.key.to_string()))
            })
            .collect();
        for expected in [
            "interface/theme",
            "interface/compact-glossaries",
            "interface/compact-tags",
            "search/text-replacements",
            "search/text-replacements-search-original",
            "search/hover-pause-resume",
            "audio/download-timeout-ms",
            "behavior/file-open-custom",
        ] {
            assert!(
                all_keys.iter().any(|k| k == expected),
                "expected {expected} in UI groups"
            );
        }

        // Dead switches must NOT appear in UI groups, while still loading cleanly from config.
        for hidden in [
            "interface/search-window",
            "interface/sub-list-window",
            "interface/system-icons",
        ] {
            assert!(
                !all_keys.iter().any(|k| k == hidden),
                "dead switch {hidden} must be hidden from UI groups"
            );
        }

        // Theme round-trip & invalid rejection.
        assert_eq!(ctrl.theme_mode(), ThemeMode::System);
        ctrl.edit_setting("interface/theme", "dark").unwrap();
        assert_eq!(ctrl.theme_mode(), ThemeMode::Dark);
        ctrl.edit_setting("interface/theme", "Light").unwrap();
        assert_eq!(ctrl.theme_mode(), ThemeMode::Light);
        assert!(ctrl.edit_setting("interface/theme", "neon").is_err());

        // Compact glossaries/tags & hover-pause-resume.
        assert!(!ctrl.compact_glossaries());
        assert!(ctrl.compact_tags());
        assert!(ctrl.hover_pause_resume());
        ctrl.edit_setting("interface/compact-glossaries", "true").unwrap();
        ctrl.edit_setting("interface/compact-tags", "false").unwrap();
        ctrl.edit_setting("search/hover-pause-resume", "false").unwrap();
        assert!(ctrl.compact_glossaries());
        assert!(!ctrl.compact_tags());
        assert!(!ctrl.hover_pause_resume());

        // Audio download timeout clamping (1000..=30000) & defaults reset.
        assert_eq!(ctrl.audio_download_timeout_ms(), 8000);
        ctrl.edit_setting("audio/download-timeout-ms", "200").unwrap();
        assert_eq!(ctrl.audio_download_timeout_ms(), 1000);
        ctrl.edit_setting("audio/download-timeout-ms", "99999").unwrap();
        assert_eq!(ctrl.audio_download_timeout_ms(), 30000);
        ctrl.defaults_settings(2).unwrap();
        assert_eq!(ctrl.audio_download_timeout_ms(), 8000);

        // Text replacements JSON validation & structured options parsing.
        assert!(ctrl.edit_setting("search/text-replacements", "not-json").is_err());
        ctrl.edit_setting(
            "search/text-replacements",
            r#"[{"pattern":"ー","replacement":"","ignore_case":false,"enabled":true}]"#,
        )
        .unwrap();
        ctrl.edit_setting("search/text-replacements-search-original", "false")
            .unwrap();
        let tr_opts = ctrl.text_replacement_options();
        assert_eq!(tr_opts.rules.len(), 1);
        assert_eq!(tr_opts.rules[0].pattern, "ー");
        assert!(!tr_opts.search_original);

        // Folder browse metadata for behavior/file-open-custom.
        let browse = setting_browse_metadata("behavior/file-open-custom").unwrap();
        assert_eq!(browse.kind, BrowseKind::Folder);
        assert!(browse.store_as_file_uri);
        assert_eq!(
            format_browse_selection("behavior/file-open-custom", Path::new("/tmp/My Anime")),
            "file:///tmp/My%20Anime"
        );

        // Old config containing hidden dead switches still loads without error.
        fs::write(
            dir.join("subomi.conf"),
            "version=6\n[interface]\nsearch-window=true\nsub-list-window=true\nsystem-icons=true\n",
        )
        .unwrap();
        let legacy = ConfigController::load(&dir, "subomi.conf").unwrap();
        assert_eq!(
            legacy.effective_setting(raw::interface::GROUP, raw::interface::SEARCH_WINDOW),
            Some(Value::Bool(true))
        );
        legacy.apply_settings(5, &dir, "subomi.conf", &anki).unwrap();
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn profile_qol_copy_current_delete_default_validation_and_ex_glos_checklist() {
        let ctrl = ConfigController::new();

        // Anki: edit Default, create with copy_current=true vs false.
        ctrl.edit_setting("anki/profile/port", "9999").unwrap();
        ctrl.add_entry_with_options(0, "Cloned", true).unwrap();
        assert_eq!(ctrl.current_anki_profile(), "Cloned");
        assert_eq!(ctrl.anki_endpoint().port, 9999);

        ctrl.add_entry_with_options(0, "Fresh", false).unwrap();
        assert_eq!(ctrl.current_anki_profile(), "Fresh");
        assert_eq!(ctrl.anki_endpoint().port, 8765);

        // Duplicate name validation (case-insensitive).
        assert!(ctrl.validate_new_profile_name(0, "fresh").is_err());
        assert!(ctrl.validate_new_profile_name(0, "   ").is_err());
        assert_eq!(
            ctrl.validate_new_profile_name(0, "  Mining  ").unwrap(),
            "Mining"
        );

        // Deleting Default succeeds when other profiles exist, and deleting the last
        // remaining profile resets cleanly to factory Default!
        ctrl.remove_entry(0, "Default").unwrap();
        ctrl.remove_entry(0, "Cloned").unwrap();
        ctrl.remove_entry(0, "Fresh").unwrap();
        assert_eq!(ctrl.anki_profile_names(), vec!["Default"]);
        assert_eq!(ctrl.current_anki_profile(), "Default");
        assert_eq!(ctrl.anki_endpoint().port, 8765);

        // Keybind: copy_current=true vs false, and deleting last profile resets cleanly.
        ctrl.edit_setting("keybind/profiles/Default/open-file", "Ctrl+Alt+F")
            .unwrap();
        ctrl.add_entry_with_options(6, "KBCloned", true).unwrap();
        assert_eq!(
            ctrl.effective_setting("keybind/profiles/KBCloned", "open-file"),
            Some(Value::Text("Ctrl+Alt+F".to_owned()))
        );
        ctrl.add_entry_with_options(6, "KBFresh", false).unwrap();
        assert_ne!(
            ctrl.effective_setting("keybind/profiles/KBFresh", "open-file"),
            Some(Value::Text("Ctrl+Alt+F".to_owned()))
        );
        assert!(ctrl.validate_new_profile_name(6, "kbfresh").is_err());
        assert!(ctrl.validate_new_profile_name(6, "bad/name").is_err());

        ctrl.remove_entry(6, "Default").unwrap();
        ctrl.remove_entry(6, "KBCloned").unwrap();
        ctrl.remove_entry(6, "KBFresh").unwrap();
        assert_eq!(ctrl.keybind_profile_names(), vec!["Default"]);
        assert_eq!(ctrl.current_keybind_profile(), "Default");
        assert_ne!(
            ctrl.effective_setting("keybind/profiles/Default", "open-file"),
            Some(Value::Text("Ctrl+Alt+F".to_owned()))
        );

        // Anki ex-glos structured checklist round-trip (including commas & uninstalled dicts).
        ctrl.set_anki_glossary_excluded("JMdict, Extra", true)
            .unwrap();
        ctrl.set_anki_glossary_excluded("UninstalledDict", true)
            .unwrap();
        let installed = vec!["JMdict, Extra".to_owned(), "Daijirin".to_owned()];
        let checklist = ctrl.anki_excluded_glossaries_checklist(&installed);
        assert_eq!(
            checklist,
            vec![
                ExcludedGlossaryItem {
                    dictionary_name: "JMdict, Extra".to_owned(),
                    excluded: true,
                },
                ExcludedGlossaryItem {
                    dictionary_name: "Daijirin".to_owned(),
                    excluded: false,
                },
                ExcludedGlossaryItem {
                    dictionary_name: "UninstalledDict".to_owned(),
                    excluded: true,
                },
            ]
        );
        let serialized = serialize_excluded_glossaries(&excluded_glossaries_from_checklist(&checklist));
        assert_eq!(
            parse_excluded_glossaries(&serialized),
            vec!["JMdict, Extra".to_owned(), "UninstalledDict".to_owned()]
        );
        ctrl.set_anki_glossary_excluded("JMdict, Extra", false)
            .unwrap();
        assert_eq!(
            ctrl.anki_excluded_glossaries(),
            vec!["UninstalledDict".to_owned()]
        );
    }

    #[test]
    fn linux_xdg_cache_config_split_and_legacy_fallback_read() {
        let root = temp_dir("xdg-cache-split");
        let cfg_dir = root.join("config/subomi");
        let cache_dir = root.join("cache/subomi");
        let anki = cfg_dir.join("anki_connect.json");

        let ctrl = ConfigController::load_with_cache_split(&cfg_dir, &cache_dir, "subomi.conf")
            .unwrap();
        assert_eq!(ctrl.cache_split_path(), Some(cache_dir.join("subomi.conf")));

        ctrl.edit_setting("behavior/autofit", "false").unwrap();
        ctrl.recent_files_add("/tmp/episode01.mkv").unwrap();
        ctrl.edit_setting("main-window/subtitle-list", "true")
            .unwrap();
        ctrl.edit_setting("dictionary/order", "1, 2").unwrap();
        ctrl.save(&cfg_dir, "subomi.conf").unwrap();

        let primary_text = fs::read_to_string(cfg_dir.join("subomi.conf")).unwrap();
        let cache_text = fs::read_to_string(cache_dir.join("subomi.conf")).unwrap();
        assert!(primary_text.contains("autofit"), "persistent key missing from config");
        assert!(!primary_text.contains("episode01.mkv"), "recent/files leaked into config");
        assert!(!primary_text.contains("[main-window]"), "main-window leaked into config");
        assert!(!primary_text.contains("[dictionary]"), "dictionary/order leaked into config");

        assert!(cache_text.contains("episode01.mkv"), "recent/files missing from cache");
        assert!(cache_text.contains("subtitle-list"), "main-window missing from cache");
        assert!(cache_text.contains("order"), "dictionary/order missing from cache");
        assert!(!cache_text.contains("autofit"), "persistent key leaked into cache");

        // Reload combines both files transparently.
        let reloaded =
            ConfigController::load_with_cache_split(&cfg_dir, &cache_dir, "subomi.conf").unwrap();
        assert_eq!(
            reloaded.effective_setting(raw::behavior::GROUP, raw::behavior::AUTO_FIT),
            Some(Value::Bool(false))
        );
        assert_eq!(reloaded.recent_files(), vec!["/tmp/episode01.mkv"]);
        assert_eq!(
            reloaded.effective_setting(raw::window::GROUP, raw::window::SUBTITLE_LIST),
            Some(Value::Bool(true))
        );
        assert_eq!(
            reloaded.effective_setting(raw::dictionary::GROUP, raw::dictionary::ORDER),
            Some(Value::Ints(vec![1, 2]))
        );

        // Single-setting persist for volatile key writes ONLY to cache, not config.
        reloaded
            .persist_single_setting("main-window/subtitle-list", "false", &cfg_dir, "subomi.conf")
            .unwrap();
        reloaded.edit_setting("dictionary/order", "3, 1").unwrap();
        reloaded
            .apply_settings(4, &cfg_dir, "subomi.conf", &anki)
            .unwrap();
        let reloaded2 =
            ConfigController::load_with_cache_split(&cfg_dir, &cache_dir, "subomi.conf").unwrap();
        assert_eq!(
            reloaded2.effective_setting(raw::window::GROUP, raw::window::SUBTITLE_LIST),
            Some(Value::Bool(false))
        );
        assert_eq!(
            reloaded2.effective_setting(raw::dictionary::GROUP, raw::dictionary::ORDER),
            Some(Value::Ints(vec![3, 1]))
        );

        // Fallback read when OS-native config dir doesn't exist yet (macOS ~/.config / Windows %APPDATA%).
        let native_dir = root.join("Library/Application Support/subomi");
        let fallback_dir = root.join(".config/subomi");
        fs::create_dir_all(&fallback_dir).unwrap();
        fs::write(
            fallback_dir.join("subomi.conf"),
            "version=6\n[behavior]\nautofit=false\n",
        )
        .unwrap();
        let migrated = ConfigController::load_with_fallback_and_cache(
            &native_dir,
            "subomi.conf",
            Some(&fallback_dir),
            None,
            None,
        )
        .unwrap();
        assert_eq!(
            migrated.effective_setting(raw::behavior::GROUP, raw::behavior::AUTO_FIT),
            Some(Value::Bool(false))
        );
        migrated.save(&native_dir, "subomi.conf").unwrap();
        assert!(native_dir.join("subomi.conf").is_file());

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn search_max_recursion_depth_clamps_roundtrips_and_resets_to_default() {
        use slint::Model;
        let dir = temp_dir("max-recursion-depth");
        let anki = dir.join("anki_connect.json");
        let ctrl = ConfigController::new();

        assert_eq!(
            ctrl.effective_setting(raw::search::GROUP, raw::search::MAX_RECURSION_DEPTH),
            Some(Value::Int(raw::search::MAX_RECURSION_DEPTH_DEFAULT as i64))
        );

        let groups = ctrl.category_settings_groups(9);
        let popup_group = groups
            .iter()
            .find(|g| g.title == "Pop-up")
            .expect("Search Pop-up settings group");
        let field = popup_group
            .fields
            .iter()
            .find(|f| f.key == "search/max-recursion-depth")
            .expect("search/max-recursion-depth field");
        assert_eq!(field.label, "Maximum child popups (recursive search)");
        assert_eq!(
            field.description,
            "Max depth of nested popups when scanning inside definitions (0 disables)"
        );
        assert_eq!(field.value, "4");

        assert!(ctrl
            .edit_setting("search/max-recursion-depth", "abc")
            .is_err());
        ctrl.edit_setting("search/max-recursion-depth", "-3")
            .unwrap();
        assert_eq!(
            ctrl.effective_setting(raw::search::GROUP, raw::search::MAX_RECURSION_DEPTH),
            Some(Value::Int(0))
        );
        ctrl.edit_setting("search/max-recursion-depth", "42")
            .unwrap();
        assert_eq!(
            ctrl.effective_setting(raw::search::GROUP, raw::search::MAX_RECURSION_DEPTH),
            Some(Value::Int(10))
        );
        ctrl.edit_setting("search/max-recursion-depth", "6")
            .unwrap();
        ctrl.apply_settings(9, &dir, "subomi.conf", &anki).unwrap();

        let reloaded = ConfigController::load(&dir, "subomi.conf").unwrap();
        assert_eq!(
            reloaded.effective_setting(raw::search::GROUP, raw::search::MAX_RECURSION_DEPTH),
            Some(Value::Int(6))
        );

        reloaded
            .edit_setting("search/max-recursion-depth", "2")
            .unwrap();
        assert_eq!(
            reloaded.effective_setting(raw::search::GROUP, raw::search::MAX_RECURSION_DEPTH),
            Some(Value::Int(2))
        );
        reloaded
            .reset_settings(9, &dir, "subomi.conf", &anki)
            .unwrap();
        assert_eq!(
            reloaded.effective_setting(raw::search::GROUP, raw::search::MAX_RECURSION_DEPTH),
            Some(Value::Int(6))
        );

        reloaded.defaults_settings(9).unwrap();
        assert_eq!(
            reloaded.effective_setting(raw::search::GROUP, raw::search::MAX_RECURSION_DEPTH),
            Some(Value::Int(4))
        );
        reloaded
            .apply_settings(9, &dir, "subomi.conf", &anki)
            .unwrap();
        let after_defaults = ConfigController::load(&dir, "subomi.conf").unwrap();
        assert_eq!(
            after_defaults.effective_setting(raw::search::GROUP, raw::search::MAX_RECURSION_DEPTH),
            Some(Value::Int(4))
        );

        let _ = fs::remove_dir_all(&dir);
    }
}

//! Configuration controller connecting `memento-config`, `memento-setting`,
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

use memento_config::config::{ChangelogEntry, Config};
use memento_config::{ArgbColor, QtFontSpec, Value};
use memento_net::anki::AnkiEndpoint;
use memento_setting::keys as raw;

use crate::ui::SettingsGroup;

pub use anki_profiles::{anki_note_preset_fields, AnkiNoteConfig, AnkiNotePreset};
pub use audio_sources::AudioSourceEntry;

use anki_profiles::{AnkiProfile, AnkiProfilesFile};
use audio_sources::{
    audio_sources, default_audio_source, ensure_default_audio_source, write_audio_sources,
};
use keybinds::{
    active_keybind_profile_name, add_keybind_profile, compute_keybind_conflicts,
    ensure_default_keybind_profile, keybind_profile_names, remove_keybind_profile,
    reset_keybind_profiles,
};
use migration::load_config_store;
use schema::{
    asr_paths, behavior_paths, category_snapshot, declared_setting_default, effective_text,
    effective_value, interface_paths, normalize_loaded_asr_values, normalize_setting_value,
    ocr_paths, onnx_paths, parse_config_value, path_in_category, restore_category, search_paths,
    value_text, whisper_paths, write_default_paths, write_default_value, SettingsCategory,
    INTERFACE_FONT_KEYS,
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
            .name("memento-config-save".to_owned())
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
            anki_profiles: RefCell::new(AnkiProfilesFile::default()),
            async_saver: RefCell::new(None),
        }
    }

    pub fn load<P: AsRef<Path>>(config_dir: P, file_name: &str) -> Result<Self, String> {
        let dir = config_dir.as_ref();
        let store = load_config_store(dir, file_name)?;
        let ctrl = Self::from_config(store);
        let baseline = clone_config_state(&ctrl.store.borrow());
        *ctrl.persisted_baseline.borrow_mut() = Some(PersistedBaseline {
            target: dir.join(file_name),
            config: baseline,
        });
        Ok(ctrl)
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
        let serialized = QtFontSpec::new(family, point_size, bold, italic)
            .to_qt_string_preserving(existing_raw.as_deref());
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
                if let Ok(spec) = QtFontSpec::parse(&raw) {
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
    /// `memento.conf` from disk.
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
            let current = effective_value(&store, group, raw_key);
            let parsed = parse_config_value(group, raw_key, value, current)?;
            store
                .put_at(group, raw_key, &parsed)
                .map_err(|error| format!("{error:?}"))?;
            parsed
        };
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
            let current = effective_value(&store, group, raw_key);
            let parsed = parse_config_value(group, raw_key, value, current)?;
            store
                .put_at(group, raw_key, &parsed)
                .map_err(|error| format!("{error:?}"))?;
            parsed
        };
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
            }
            SettingsCategory::Audio => write_audio_sources(&mut store, &[default_audio_source()])?,
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

    pub fn add_entry(&self, category: i32, name: &str) -> Result<(), String> {
        match SettingsCategory::from_i32(category).ok() {
            Some(SettingsCategory::Anki) => self.anki_profiles.borrow_mut().add_profile(name),
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
                add_keybind_profile(&mut self.store.borrow_mut(), name)
            }
            _ => Err(format!(
                "settings category {category} does not support entries"
            )),
        }
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
            "memento-ctrl-cfg-{label}-{}-{id}",
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
        ctrl.save(&dir, "memento.conf").unwrap();
        assert_eq!(ctrl.store().revision(), 1);
        assert!(!ctrl.store().is_dirty());

        ctrl.edit_setting("behavior/autofit", "false").unwrap();
        ctrl.apply_settings(3, &dir, "memento.conf", &anki).unwrap();
        assert_eq!(ctrl.store().revision(), 2);
        assert!(!ctrl.store().is_dirty());

        ctrl.recent_files_add("/tmp/video.mkv").unwrap();
        ctrl.save(&dir, "memento.conf").unwrap();
        assert_eq!(ctrl.store().revision(), 3);

        let reloaded = ConfigController::load(&dir, "memento.conf").unwrap();
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
        ctrl.persist_single_setting_async("behavior/osc-duration", "500", &dir, "memento.conf")
            .unwrap();
        ctrl.persist_single_setting_async("behavior/osc-duration", "750", &dir, "memento.conf")
            .unwrap();
        ctrl.flush_async_saves().unwrap();

        let reloaded = ConfigController::load(&dir, "memento.conf").unwrap();
        assert_eq!(
            reloaded.effective_setting(raw::behavior::GROUP, raw::behavior::OSC_DURATION),
            Some(Value::Int(750))
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn legacy_ini_runs_memento_setting_migration() {
        let dir = temp_dir("legacy-migrate");
        let conf_path = dir.join("memento.conf");
        let anki_path = dir.join("anki_connect.json");
        fs::write(&conf_path, "version=2\n[search]\nlist-result=false\n").unwrap();

        let loaded = ConfigController::load(&dir, "memento.conf").unwrap();
        assert_eq!(
            loaded.effective_setting(raw::search::GROUP, raw::search::GLOSSARY_STYLE),
            Some(Value::Int(1))
        );
        assert_eq!(loaded.store().version().unwrap(), 6);

        loaded
            .apply_settings(1, &dir, "memento.conf", &anki_path)
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
        ctrl.apply_settings(5, &dir, "memento.conf", &anki).unwrap();

        let reloaded = ConfigController::load(&dir, "memento.conf").unwrap();
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
    fn keybind_conflicts_detect_duplicates_case_and_modifier_order_insensitively() {
        let ctrl = ConfigController::new();
        assert!(ctrl.keybind_conflicts().is_empty());

        let profile = ctrl.current_keybind_profile();
        ctrl.edit_setting(
            &format!("keybind/profiles/{profile}/open-file"),
            "Shift+Ctrl+C",
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
        ctrl.apply_settings(9, &dir, "memento.conf", &anki).unwrap();

        let reloaded = ConfigController::load(&dir, "memento.conf").unwrap();
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
            .reset_settings(9, &dir, "memento.conf", &anki)
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
            .apply_settings(9, &dir, "memento.conf", &anki)
            .unwrap();
        let after_defaults = ConfigController::load(&dir, "memento.conf").unwrap();
        assert_eq!(
            after_defaults.effective_setting(raw::search::GROUP, raw::search::MAX_RECURSION_DEPTH),
            Some(Value::Int(4))
        );

        let _ = fs::remove_dir_all(&dir);
    }
}

//! Configuration controller connecting memento-config.

use std::cell::RefCell;
use std::path::Path;
use std::collections::BTreeMap;
use std::fs;
use std::rc::Rc;
use memento_config::canonical::FORMAT_MAGIC;
use memento_config::config::{Config, RawDocument};
use memento_config::keys::lookup;
use memento_config::{parse_legacy_ini, Value};
use memento_net::anki::AnkiEndpoint;
use memento_setting::keys as raw;
use serde_json::{json, Map, Value as JsonValue};
use slint::{ModelRc, SharedString, VecModel};

use crate::ui::{SettingsField, SettingsGroup};

#[derive(Clone, Debug)]
pub struct AnkiNoteConfig {
    pub enabled: bool,
    pub endpoint: AnkiEndpoint,
    pub deck: String,
    pub model: String,
    pub field_templates: std::collections::HashMap<String, String>,
    pub tags: Vec<String>,
    pub duplicate_policy: i32,
    pub screenshot_type: i32,
    pub audio_pad_start: f64,
    pub audio_pad_end: f64,
    pub audio_normalize: bool,
    pub audio_db: f64,
    pub newline_replacer: String,
}

impl AnkiNoteConfig {
    pub fn endpoint(&self) -> AnkiEndpoint {
        self.endpoint.clone()
    }
}

#[derive(Clone)]
struct ConfigSnapshot {
    values: Vec<(String, String, Option<Value>)>,
    replace_prefixes: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct AudioSourceEntry {
    pub name: String,
    pub url: String,
    pub kind: i64,
    pub md5: String,
}

pub struct ConfigController {
    store: RefCell<Config>,
    anki_profiles: RefCell<JsonValue>,
}

const ANKI_PROFILE_DEFAULT: &str = "Default";
const ANKI_CONFIG_DEFAULT_PORT: &str = "8765";

const KEYBIND_FIELDS: &[(&str, &str)] = &[
    (raw::keybind::profile::CARD_ADD, "Add current expression to Anki"),
    (raw::keybind::profile::CARD_NEXT, "Scroll to next search result"),
    (raw::keybind::profile::CARD_PREVIOUS, "Scroll to previous search result"),
    (raw::keybind::profile::OPEN_FILE, "Open file"),
    (raw::keybind::profile::OPEN_URL, "Open URL"),
    (raw::keybind::profile::OSC_VISIBILITY, "Cycle OSC visibility"),
    (raw::keybind::profile::SHOW_SEARCH, "Show auxiliary search"),
    (raw::keybind::profile::SHOW_SUBTITLE_LIST, "Show subtitle list"),
    (raw::keybind::profile::START_OCR, "Start OCR"),
    (raw::keybind::profile::SUBTITLE_AUTO_PAUSE, "Toggle subtitle auto-pause"),
    (raw::keybind::profile::SUBTITLE_DECREASE_SIZE, "Decrease subtitle size"),
    (raw::keybind::profile::SUBTITLE_INCREASE_SIZE, "Increase subtitle size"),
    (raw::keybind::profile::SUBTITLE_MOVE_DOWN, "Move subtitles down"),
    (raw::keybind::profile::SUBTITLE_MOVE_UP, "Move subtitles up"),
    (raw::keybind::profile::SUBTITLE_SHOW, "Toggle subtitle visibility"),
    (raw::keybind::profile::SUBTITLE_FURIGANA, "Toggle subtitle furigana"),
    (raw::keybind::profile::SUBTITLE_LIST_COPY_SELECTED, "Copy selected subtitle"),
    (raw::keybind::profile::SUBTITLE_LIST_FIND, "Find in subtitle list"),
    (raw::keybind::profile::SUBTITLE_LIST_FIND_NEXT, "Find next in subtitle list"),
    (raw::keybind::profile::SUBTITLE_LIST_FIND_PREVIOUS, "Find previous in subtitle list"),
];

fn keybind_default(key: &str) -> &'static str {
    match key {
        raw::keybind::profile::CARD_ADD => "Ctrl+D",
        raw::keybind::profile::CARD_NEXT => "Ctrl+Down",
        raw::keybind::profile::CARD_PREVIOUS => "Ctrl+Up",
        raw::keybind::profile::OPEN_FILE => "Ctrl+O",
        raw::keybind::profile::OPEN_URL => "Ctrl+U",
        raw::keybind::profile::OSC_VISIBILITY => "Delete",
        raw::keybind::profile::SHOW_SEARCH => "Ctrl+R",
        raw::keybind::profile::SHOW_SUBTITLE_LIST => "Ctrl+E",
        raw::keybind::profile::START_OCR => "Ctrl+L",
        raw::keybind::profile::SUBTITLE_AUTO_PAUSE => "Ctrl+P",
        raw::keybind::profile::SUBTITLE_DECREASE_SIZE => "Ctrl+1",
        raw::keybind::profile::SUBTITLE_INCREASE_SIZE => "Ctrl+2",
        raw::keybind::profile::SUBTITLE_MOVE_DOWN => "Alt+2",
        raw::keybind::profile::SUBTITLE_MOVE_UP => "Alt+1",
        raw::keybind::profile::SUBTITLE_SHOW => "Ctrl+0",
        raw::keybind::profile::SUBTITLE_FURIGANA => "Alt+A",
        raw::keybind::profile::SUBTITLE_LIST_COPY_SELECTED => "Ctrl+Shift+C",
        raw::keybind::profile::SUBTITLE_LIST_FIND => "Ctrl+F",
        raw::keybind::profile::SUBTITLE_LIST_FIND_NEXT => "F3",
        raw::keybind::profile::SUBTITLE_LIST_FIND_PREVIOUS => "Shift+F3",
        _ => "",
    }
}

fn keybind_value(config: &Config, group: &str, key: &str) -> String {
    config
        .get_at(group, key)
        .map(|value| value_text(&value))
        .unwrap_or_else(|| keybind_default(key).to_owned())
}

impl Default for ConfigController {
    fn default() -> Self {
        Self::new()
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
            anki_profiles: RefCell::new(default_anki_config()),
        }
    }

    pub fn load<P: AsRef<Path>>(config_dir: P, file_name: &str) -> Result<Self, String> {
        let store = load_config_store(config_dir.as_ref(), file_name)?;
        Ok(Self::from_config(store))
    }

    pub fn load_anki_profiles<P: AsRef<Path>>(&self, path: P) -> Result<(), String> {
        let path = path.as_ref();
        let config = match fs::read_to_string(path) {
            Ok(text) => {
                let parsed: JsonValue = serde_json::from_str(&text)
                    .map_err(|error| format!("could not parse Anki profiles: {error}"))?;
                normalize_anki_config(parsed)?
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => default_anki_config(),
            Err(error) => return Err(format!("could not read Anki profiles: {error}")),
        };
        *self.anki_profiles.borrow_mut() = config;
        Ok(())
    }

    pub fn save_anki_profiles<P: AsRef<Path>>(&self, path: P) -> Result<(), String> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| format!("could not create Anki config directory: {error}"))?;
        }
        let text = serde_json::to_string_pretty(&*self.anki_profiles.borrow())
            .map_err(|error| format!("could not encode Anki profiles: {error}"))?;
        fs::write(path, text).map_err(|error| format!("could not write Anki profiles: {error}"))
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
        self.store
            .borrow_mut()
            .save(dir, file_name)
            .map_err(|e| format!("{:?}", e))
    }


    // ── Recent files ────────────────────────────────────────────────────────

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
            .map_err(|e| format!("{:?}", e))
    }

    pub fn add_recent_file(&self, path: &str) -> Result<bool, String> {
        self.recent_files_add(path)
    }

    pub fn recent_files_clear(&self) -> Result<bool, String> {
        self.store
            .borrow_mut()
            .recent_files_clear()
            .map_err(|e| format!("{:?}", e))
    }

    pub fn clear_recent_files(&self) -> Result<bool, String> {
        self.recent_files_clear()
    }
}
impl ConfigController {
    pub fn current_anki_profile(&self) -> String {
        let profiles = self.anki_profiles.borrow();
        current_anki_profile_name(&profiles)
    }

    pub fn anki_profile_names(&self) -> Vec<String> {
        anki_profiles(&self.anki_profiles.borrow())
            .into_iter()
            .map(|profile| anki_profile_name(profile).to_owned())
            .collect()
    }

    pub fn current_keybind_profile(&self) -> String {
        let store = self.store.borrow();
        effective_text(&store, raw::keybind::GROUP, raw::keybind::CURRENT_PROFILE)
            .filter(|name| keybind_profile_names(&store).iter().any(|profile| profile == name))
            .unwrap_or_else(|| {
                keybind_profile_names(&store)
                    .into_iter()
                    .next()
                    .unwrap_or_else(|| ANKI_PROFILE_DEFAULT.to_owned())
            })
    }

    pub fn anki_endpoint(&self) -> AnkiEndpoint {
        let profiles = self.anki_profiles.borrow();
        endpoint_from_profile(current_anki_profile(&profiles))
    }

    pub fn anki_enabled(&self) -> bool {
        self.anki_profiles
            .borrow()
            .get("enabled")
            .and_then(JsonValue::as_bool)
            .unwrap_or(false)
    }

    pub fn anki_excluded_glossaries(&self) -> Vec<String> {
        let profiles = self.anki_profiles.borrow();
        current_anki_profile(&profiles)
            .get("ex-glos")
            .and_then(JsonValue::as_array)
            .map(|items| items.iter().filter_map(JsonValue::as_str).map(str::to_owned).collect())
            .unwrap_or_default()
    }

    pub fn anki_note_model(&self, section: &str) -> Option<String> {
        if section != "term" && section != "kanji" { return None; }
        let profiles = self.anki_profiles.borrow();
        current_anki_profile(&profiles)
            .get(section)?
            .get("model")?
            .as_str()
            .map(str::to_owned)
    }

    pub fn set_anki_model_fields(&self, section: &str, fields: &[String]) -> Result<(), String> {
        if section != "term" && section != "kanji" {
            return Err("unknown Anki note type".into());
        }
        let profiles = self.anki_profiles.borrow();
        let note = current_anki_profile(&profiles).get(section)
            .ok_or("Anki note profile is missing")?;
        let existing = json_fields_to_map(note.get("fields").ok_or("Anki fields are missing")?)
            .unwrap_or_default();
        let rows: Vec<JsonValue> = fields.iter().map(|name| {
            let marker = existing.get(name).cloned().unwrap_or_else(|| default_anki_field_marker(section, name));
            json!([name, marker])
        }).collect();
        drop(profiles);
        self.edit_setting(&format!("anki/profile/{section}/fields"), &JsonValue::Array(rows).to_string())
    }

    pub fn anki_profile_note_config(&self) -> Option<AnkiNoteConfig> {
        self.anki_note_config("term")
    }

    pub fn anki_kanji_note_config(&self) -> Option<AnkiNoteConfig> {
        self.anki_note_config("kanji")
    }

    fn anki_note_config(&self, section: &str) -> Option<AnkiNoteConfig> {
        let profiles = self.anki_profiles.borrow();
        let profile = current_anki_profile(&profiles);
        let note = profile.get(section)?;
        let deck = note.get("deck").and_then(JsonValue::as_str)?.to_owned();
        let model = note.get("model").and_then(JsonValue::as_str)?.to_owned();
        let field_templates = json_fields_to_map(note.get("fields")?)?;
        let tags = profile
            .get("tags")
            .and_then(JsonValue::as_array)
            .map(|tags| {
                tags.iter()
                    .filter_map(JsonValue::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        Some(AnkiNoteConfig {
            enabled: profiles
                .get("enabled")
                .and_then(JsonValue::as_bool)
                .unwrap_or(false),
            endpoint: endpoint_from_profile(profile),
            deck,
            model,
            field_templates,
            tags,
            duplicate_policy: profile
                .get("duplicate")
                .and_then(JsonValue::as_i64)
                .unwrap_or(1) as i32,
            screenshot_type: anki_profile_i64(profile, "screenshot", 0) as i32,
            audio_pad_start: anki_profile_real(profile, "audio-pad-start", 0.0),
            audio_pad_end: anki_profile_real(profile, "audio-pad-end", 0.0),
            audio_normalize: anki_profile_bool(profile, "audio-normalize", false),
            audio_db: anki_profile_real(profile, "audio-db", -20.0),
            newline_replacer: anki_profile_string(profile, "newline-replace", "<br>").to_owned(),
        })
    }

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
            return edit_anki_setting(&mut self.anki_profiles.borrow_mut(), key, value);
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

    pub fn apply_settings(
        &self,
        category: i32,
        config_dir: &Path,
        settings_file: &str,
        anki_json_path: &Path,
    ) -> Result<(), String> {
        if category == 0 {
            return self.save_anki_profiles(anki_json_path);
        }
        validate_settings_category(category)?;

        let snapshot = category_snapshot(&self.store.borrow(), category);
        let mut persisted = load_config_store(config_dir, settings_file)?;
        restore_category(&mut persisted, &snapshot);
        persisted
            .save(config_dir, settings_file)
            .map_err(|error| format!("{error:?}"))
    }

    pub fn reset_settings(
        &self,
        category: i32,
        config_dir: &Path,
        settings_file: &str,
        anki_json_path: &Path,
    ) -> Result<(), String> {
        if category == 0 {
            return self.load_anki_profiles(anki_json_path);
        }
        validate_settings_category(category)?;

        let persisted = load_config_store(config_dir, settings_file)?;
        let snapshot = category_snapshot(&persisted, category);
        restore_category(&mut self.store.borrow_mut(), &snapshot);
        if category == 2 && audio_sources(&self.store.borrow()).is_empty() {
            self.defaults_settings(category)?;
        } else if category == 6 && keybind_profile_names(&self.store.borrow()).is_empty() {
            self.defaults_settings(category)?;
        }
        Ok(())
    }

    pub fn defaults_settings(&self, category: i32) -> Result<(), String> {
        if category == 0 {
            let mut profiles = self.anki_profiles.borrow_mut();
            let profile_name = current_anki_profile_name(&profiles);
            if let Some(profile) = find_anki_profile_mut(&mut profiles, &profile_name) {
                reset_anki_profile_defaults(profile);
            }
            return Ok(());
        }
        validate_settings_category(category)?;
        let mut store = self.store.borrow_mut();
        match category {
            1 => {
                write_default_value(&mut store, raw::application::GROUP, raw::application::LANGUAGE);
                write_default_value(
                    &mut store,
                    raw::application::GROUP,
                    raw::application::AUTO_UPDATE_CHECK,
                );
            }
            2 => write_audio_sources(&mut store, &[default_audio_source()])?,
            3 => write_default_paths(&mut store, behavior_paths()),
            4 => write_default_value(
                &mut store,
                raw::dictionary::GROUP,
                raw::dictionary::ORDER,
            ),
            5 => write_default_paths(&mut store, interface_paths()),
            6 => reset_keybind_profiles(&mut store)?,
            7 => write_default_paths(&mut store, ocr_paths()),
            8 => {
                write_default_paths(&mut store, asr_paths());
                let backend = raw::asr::backend_default(true, true);
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
            9 => write_default_paths(&mut store, search_paths()),
            _ => unreachable!("settings category validated"),
        }
        Ok(())
    }

    pub fn add_entry(&self, category: i32, name: &str) -> Result<(), String> {
        match category {
            0 => add_anki_profile(&mut self.anki_profiles.borrow_mut(), name),
            2 => {
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
            6 => add_keybind_profile(&mut self.store.borrow_mut(), name),
            _ => Err(format!("settings category {category} does not support entries")),
        }
    }

    pub fn remove_entry(&self, category: i32, id: &str) -> Result<(), String> {
        match category {
            0 => remove_anki_profile(&mut self.anki_profiles.borrow_mut(), id),
            2 => {
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
            6 => remove_keybind_profile(&mut self.store.borrow_mut(), id),
            _ => Err(format!("settings category {category} does not support entries")),
        }
    }

    pub fn move_entry(
        &self,
        category: i32,
        id: &str,
        direction: i32,
    ) -> Result<(), String> {
        if category != 2 {
            return Err(format!("settings category {category} does not support moving entries"));
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
        anki_settings_groups(&self.anki_profiles.borrow())
    }

    pub fn category_settings_groups(&self, category: i32) -> Vec<SettingsGroup> {
        let config = self.store.borrow();
        match category {
            0 => self.anki_settings_groups(),
            1 => application_settings_groups(&config),
            2 => audio_settings_groups(&config),
            3 => behavior_settings_groups(&config),
            4 => dictionary_settings_groups(&config),
            5 => interface_settings_groups(&config),
            6 => keybind_settings_groups(&config),
            7 => ocr_settings_groups(&config),
            8 => asr_settings_groups(&config),
            9 => search_settings_groups(&config),
            _ => Vec::new(),
        }
    }
}

fn load_config_store(config_dir: &Path, file_name: &str) -> Result<Config, String> {
    let path = config_dir.join(file_name);
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Config::default_empty());
        }
        Err(error) => return Err(format!("could not read settings: {error}")),
    };
    let clean = text.strip_prefix('\u{feff}').unwrap_or(&text).trim_start();
    if clean.starts_with(FORMAT_MAGIC) {
        return Config::load(config_dir, file_name).map_err(|error| format!("{error:?}"));
    }
    let legacy = parse_legacy_ini(&text).map_err(|error| format!("{error:?}"))?;
    Ok(Config::from_document(RawDocument {
        revision: Some(0),
        changelog_rows: Vec::new(),
        sections: legacy.sections,
    }))
}

fn validate_settings_category(category: i32) -> Result<(), String> {
    if (0..=9).contains(&category) {
        Ok(())
    } else {
        Err(format!("unknown settings category {category}"))
    }
}

fn strings_model(values: &[String]) -> ModelRc<SharedString> {
    Rc::new(VecModel::from(
        values
            .iter()
            .map(|value| SharedString::from(value.as_str()))
            .collect::<Vec<_>>(),
    ))
    .into()
}

fn field(
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
    }
}

fn text_field(key: &str, label: &str, value: String) -> SettingsField {
    field(key, label, "text", value, &[], true, true)
}

fn conditional_text_field(
    key: &str,
    label: &str,
    value: String,
    enabled: bool,
    visible: bool,
) -> SettingsField {
    field(key, label, "text", value, &[], enabled, visible)
}

fn switch_field(key: &str, label: &str, value: bool) -> SettingsField {
    field(
        key,
        label,
        "switch",
        value.to_string(),
        &[],
        true,
        true,
    )
}

fn conditional_switch_field(
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

fn choice_field(
    key: &str,
    label: &str,
    value: String,
    choices: &[&str],
) -> SettingsField {
    field(key, label, "choice", value, choices, true, true)
}

fn conditional_choice_field(
    key: &str,
    label: &str,
    value: String,
    choices: &[&str],
    enabled: bool,
    visible: bool,
) -> SettingsField {
    field(key, label, "choice", value, choices, enabled, visible)
}

#[allow(dead_code)]
fn button_field(key: &str, label: &str, button_text: &str) -> SettingsField {
    field(key, label, "button", button_text.to_owned(), &[], true, true)
}

fn group(category: i32, title: &str, entry_id: &str, fields: Vec<SettingsField>) -> SettingsGroup {
    SettingsGroup {
        category,
        title: title.into(),
        entry_id: entry_id.into(),
        fields: Rc::new(VecModel::from(fields)).into(),
    }
}

fn value_text(value: &Value) -> String {
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
    // This ten-field form is accepted by QFont::fromString on both Qt 5 and 6.
    Some(Value::Text(format!(
        "{family},{point_size},-1,5,{weight},0,0,0,0,0"
    )))
}

fn declared_setting_default(config: &Config, group: &str, key: &str) -> Option<Value> {
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
    if group == raw::interface::GROUP {
        if let Some(value) = qt_font_default(key) {
            return Some(value);
        }
    }
    lookup(group, key).and_then(|typed| typed.declared_default(config.context()))
}
fn asr_thread_limit() -> i64 {
    std::thread::available_parallelism()
        .map_or(1, |count| count.get())
        .max(1) as i64
}

fn default_asr_thread_count() -> i64 {
    4.min(asr_thread_limit())
}

fn normalize_setting_value(group: &str, key: &str, value: Value) -> Value {
    match (group, key, value) {
        (raw::whisper::GROUP, raw::whisper::THREADS, Value::Int(requested))
        | (raw::onnx::GROUP, raw::onnx::THREADS, Value::Int(requested)) => Value::Int(
            if requested <= 0 {
                default_asr_thread_count()
            } else {
                requested.clamp(1, asr_thread_limit())
            },
        ),
        (raw::whisper::GROUP, raw::whisper::BEST_OF, Value::Int(value))
        | (raw::whisper::GROUP, raw::whisper::BEAM_SIZE, Value::Int(value)) => {
            Value::Int(value.clamp(1, 10))
        }
        (_, _, value) => value,
    }
}

fn normalize_loaded_asr_values(config: &mut Config) {
    for (group, key) in [
        (raw::whisper::GROUP, raw::whisper::THREADS),
        (raw::whisper::GROUP, raw::whisper::BEST_OF),
        (raw::whisper::GROUP, raw::whisper::BEAM_SIZE),
        (raw::onnx::GROUP, raw::onnx::THREADS),
    ] {
        if let Some(value) = config.get_at(group, key) {
            let value = normalize_setting_value(group, key, value);
            let _ = config.put_at(group, key, &value);
        }
    }
}


fn effective_value(config: &Config, group: &str, key: &str) -> Value {
    config
        .get_at(group, key)
        .or_else(|| declared_setting_default(config, group, key))
        .map(|value| normalize_setting_value(group, key, value))
        .unwrap_or_else(|| Value::Text(String::new()))
}

fn effective_text(config: &Config, group: &str, key: &str) -> Option<String> {
    match effective_value(config, group, key) {
        Value::Text(value) if !value.is_empty() => Some(value),
        _ => None,
    }
}

fn effective_bool(config: &Config, group: &str, key: &str) -> bool {
    match effective_value(config, group, key) {
        Value::Bool(value) => value,
        Value::Text(value) => value.eq_ignore_ascii_case("true"),
        _ => false,
    }
}

fn effective_i64(config: &Config, group: &str, key: &str) -> i64 {
    match effective_value(config, group, key) {
        Value::Int(value) => value,
        Value::Text(value) => value.parse().unwrap_or_default(),
        _ => 0,
    }
}


fn category_paths(category: i32) -> Vec<(&'static str, &'static str)> {
    match category {
        1 => vec![
            (raw::application::GROUP, raw::application::LANGUAGE),
            (raw::application::GROUP, raw::application::AUTO_UPDATE_CHECK),
        ],
        3 => behavior_paths(),
        4 => vec![(raw::dictionary::GROUP, raw::dictionary::ORDER)],
        5 => interface_paths(),
        7 => ocr_paths(),
        8 => [asr_paths(), whisper_paths(), onnx_paths()].concat(),
        9 => search_paths(),
        _ => Vec::new(),
    }
}

fn write_default_value(config: &mut Config, group: &str, key: &str) {
    match declared_setting_default(config, group, key) {
        Some(value) => {
            let _ = config.put_at(group, key, &value);
        }
        None => {
            config.remove_at(group, key);
        }
    }
}

fn write_default_paths(config: &mut Config, paths: Vec<(&'static str, &'static str)>) {
    for (group, key) in paths {
        write_default_value(config, group, key);
    }
}

fn behavior_paths() -> Vec<(&'static str, &'static str)> {
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

fn interface_paths() -> Vec<(&'static str, &'static str)> {
    let group = raw::interface::GROUP;
    vec![
        (group, raw::interface::SYSTEM_ICONS),
        (group, raw::interface::subtitle::FONT),
        (group, raw::interface::subtitle::LINE_SPACING),
        (group, raw::interface::subtitle::SCALE),
        (group, raw::interface::subtitle::OFFSET),
        (group, raw::interface::subtitle::STROKE),
        (group, raw::interface::subtitle::TEXT_COLOR),
        (group, raw::interface::subtitle::BACKGROUND_COLOR),
        (group, raw::interface::subtitle::STROKE_COLOR),
        (group, raw::interface::subtitle::FURIGANA_RATIO),
        (group, raw::interface::POPUP_WIDTH),
        (group, raw::interface::POPUP_HEIGHT),
        (group, raw::interface::SEARCH_WINDOW),
        (group, raw::interface::SEARCH_EXPRESSION_FONT),
        (group, raw::interface::SEARCH_READING_FONT),
        (group, raw::interface::SEARCH_CONJ_FONT),
        (group, raw::interface::SEARCH_TAG_FONT),
        (group, raw::interface::SEARCH_GLOSSARY_FONT),
        (group, raw::interface::SEARCH_KANJI_FONT),
        (group, raw::interface::subtitle_list::WINDOW),
        (group, raw::interface::subtitle_list::TIMESTAMPS),
        (group, raw::interface::subtitle_list::BACKGROUND_COLOR),
        (group, raw::interface::subtitle_list::TEXT_COLOR),
        (group, raw::interface::subtitle_list::HOVER_COLOR),
        (group, raw::interface::subtitle_list::SELECTED_COLOR),
        (group, raw::interface::subtitle_list::SELECTED_HOVER_COLOR),
        (group, raw::interface::subtitle_list::PRIMARY_FONT),
        (group, raw::interface::subtitle_list::SECONDARY_FONT),
    ]
}

fn ocr_paths() -> Vec<(&'static str, &'static str)> {
    vec![
        (raw::ocr::GROUP, raw::ocr::ENABLED),
        (raw::ocr::GROUP, raw::ocr::USE_GPU),
        (raw::ocr::GROUP, raw::ocr::MODEL),
    ]
}

fn asr_paths() -> Vec<(&'static str, &'static str)> {
    vec![
        (raw::asr::GROUP, raw::asr::ENABLED),
        (raw::asr::GROUP, raw::asr::BACKEND),
        (raw::asr::GROUP, raw::asr::START_FROM_BEGINNING),
    ]
}

fn whisper_paths() -> Vec<(&'static str, &'static str)> {
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

fn onnx_paths() -> Vec<(&'static str, &'static str)> {
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

fn search_paths() -> Vec<(&'static str, &'static str)> {
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
        (group, raw::search::AUTO_PLAY_AUDIO),
        (group, raw::search::GLOSSARY_STYLE),
        (group, raw::search::REPLACE_NEWLINES),
        (group, raw::search::REPLACE_NEWLINES_WITH),
        (group, raw::search::REMOVE_REGEX),
        (group, raw::search::SUBTITLE_FURIGANA),
        (group, raw::search::SHOW_ANKI_GLOSSARY_CHECKBOX),
    ]
}

fn audio_source_count(config: &Config) -> usize {
    let stored = match config.get_at(raw::audio_source::GROUP, "size") {
        Some(Value::Int(size)) if size > 0 => size as usize,
        _ => 0,
    };
    let highest_group = config
        .groups_with_prefix(&format!("{}/", raw::audio_source::GROUP))
        .iter()
        .filter_map(|group| {
            group
                .strip_prefix(&format!("{}/", raw::audio_source::GROUP))?
                .parse::<usize>()
                .ok()
        })
        .max()
        .unwrap_or(0);
    stored.max(highest_group)
}

fn audio_sources(config: &Config) -> Vec<AudioSourceEntry> {
    (1..=audio_source_count(config))
        .map(|index| {
            let group = format!("{}/{}", raw::audio_source::GROUP, index);
            AudioSourceEntry {
                name: value_text(&effective_value(config, &group, raw::audio_source::NAME)),
                url: value_text(&effective_value(config, &group, raw::audio_source::URL)),
                kind: effective_i64(config, &group, raw::audio_source::TYPE),
                md5: value_text(&effective_value(config, &group, raw::audio_source::MD5)),
            }
        })
        .collect()
}

fn default_audio_source() -> AudioSourceEntry {
    AudioSourceEntry {
        name: raw::audio_source::NAME_DEFAULT.to_owned(),
        url: raw::audio_source::URL_DEFAULT.to_owned(),
        kind: raw::audio_source::TYPE_DEFAULT as i64,
        md5: raw::audio_source::MD5_DEFAULT.to_owned(),
    }
}

fn write_audio_sources(config: &mut Config, sources: &[AudioSourceEntry]) -> Result<(), String> {
    let groups = config.groups_with_prefix(&format!("{}/", raw::audio_source::GROUP));
    for group in groups {
        config.remove_group(&group);
    }
    config
        .put_at(
            raw::audio_source::GROUP,
            "size",
            &Value::Int(sources.len() as i64),
        )
        .map_err(|error| format!("{error:?}"))?;
    for (index, source) in sources.iter().enumerate() {
        let group = format!("{}/{}", raw::audio_source::GROUP, index + 1);
        for (key, value) in [
            (raw::audio_source::NAME, Value::Text(source.name.clone())),
            (raw::audio_source::URL, Value::Text(source.url.clone())),
            (raw::audio_source::TYPE, Value::Int(source.kind)),
            (raw::audio_source::MD5, Value::Text(source.md5.clone())),
        ] {
            config
                .put_at(&group, key, &value)
                .map_err(|error| format!("{error:?}"))?;
        }
    }
    Ok(())
}

fn ensure_default_audio_source(config: &mut Config) {
    if audio_source_count(config) == 0 {
        let _ = write_audio_sources(config, &[default_audio_source()]);
    }
}

fn keybind_profile_names(config: &Config) -> Vec<String> {
    let prefix = format!("{}/{}/", raw::keybind::GROUP, raw::keybind::profile::GROUP);
    let mut names = config
        .groups_with_prefix(&prefix)
        .into_iter()
        .filter_map(|group| group.strip_prefix(&prefix).map(str::to_owned))
        .filter(|name| !name.contains('/'))
        .collect::<Vec<_>>();
    names.sort_by(|left, right| left.encode_utf16().cmp(right.encode_utf16()));
    names.dedup();
    names
}

fn ensure_default_keybind_profile(config: &mut Config) {
    if keybind_profile_names(config).is_empty() {
        let _ = reset_keybind_profiles(config);
    }
}

fn reset_keybind_profiles(config: &mut Config) -> Result<(), String> {
    let prefix = format!("{}/{}/", raw::keybind::GROUP, raw::keybind::profile::GROUP);
    for group in config.groups_with_prefix(&prefix) {
        config.remove_group(&group);
    }
    let profile = ANKI_PROFILE_DEFAULT;
    config
        .put_at(
            raw::keybind::GROUP,
            raw::keybind::CURRENT_PROFILE,
            &Value::Text(profile.to_owned()),
        )
        .map_err(|error| format!("{error:?}"))?;
    let profile_group = format!("{prefix}{profile}");
    for (key, _) in KEYBIND_FIELDS {
        config
            .put_at(
                &profile_group,
                key,
                &Value::Text(keybind_default(key).to_owned()),
            )
            .map_err(|error| format!("{error:?}"))?;
    }
    Ok(())
}

fn add_keybind_profile(config: &mut Config, requested_name: &str) -> Result<(), String> {
    let name = requested_name.trim();
    if name.is_empty() {
        return Err("profile name cannot be empty".to_owned());
    }
    let names = keybind_profile_names(config);
    if names.iter().any(|existing| existing == name) {
        return Err(format!("keybind profile {name:?} already exists"));
    }
    let current = effective_text(config, raw::keybind::GROUP, raw::keybind::CURRENT_PROFILE)
        .filter(|current| names.iter().any(|name| name == current))
        .or_else(|| names.first().cloned())
        .unwrap_or_default();
    let source_group = format!(
        "{}/{}/{}",
        raw::keybind::GROUP,
        raw::keybind::profile::GROUP,
        current
    );
    let target_group = format!(
        "{}/{}/{}",
        raw::keybind::GROUP,
        raw::keybind::profile::GROUP,
        name
    );
    for (key, _) in KEYBIND_FIELDS {
        let value = config
            .get_at(&source_group, key)
            .unwrap_or_else(|| Value::Text(keybind_default(key).to_owned()));
        config
            .put_at(&target_group, key, &value)
            .map_err(|error| format!("{error:?}"))?;
    }
    config
        .put_at(
            raw::keybind::GROUP,
            raw::keybind::CURRENT_PROFILE,
            &Value::Text(name.to_owned()),
        )
        .map(|_| ())
        .map_err(|error| format!("{error:?}"))
}

fn remove_keybind_profile(config: &mut Config, name: &str) -> Result<(), String> {
    let profile_group = format!(
        "{}/{}/{}",
        raw::keybind::GROUP,
        raw::keybind::profile::GROUP,
        name
    );
    if !keybind_profile_names(config).iter().any(|profile| profile == name) {
        return Err(format!("keybind profile {name:?} does not exist"));
    }
    config.remove_group(&profile_group);
    let mut names = keybind_profile_names(config);
    if names.is_empty() {
        return reset_keybind_profiles(config);
    }
    let current = effective_text(config, raw::keybind::GROUP, raw::keybind::CURRENT_PROFILE);
    if current.as_deref() == Some(name)
        || !current
            .as_ref()
            .is_some_and(|current| names.iter().any(|profile| profile == current))
    {
        names.sort_by(|left, right| left.encode_utf16().cmp(right.encode_utf16()));
        config
            .put_at(
                raw::keybind::GROUP,
                raw::keybind::CURRENT_PROFILE,
                &Value::Text(names[0].clone()),
            )
            .map_err(|error| format!("{error:?}"))?;
    }
    Ok(())
}

fn application_settings_groups(config: &Config) -> Vec<SettingsGroup> {
    let language = effective_i64(config, raw::application::GROUP, raw::application::LANGUAGE);
    let language_label = match language {
        1 => "English",
        2 => "한국어 (MTL)",
        3 => "简体中文 (MTL)",
        4 => "繁體中文 (MTL)",
        _ => "System",
    };
    vec![
        group(
            1,
            "Language",
            "",
            vec![choice_field(
                "application/language",
                "Interface language (restart required)",
                language_label.to_owned(),
                &["System", "English", "한국어 (MTL)", "简体中文 (MTL)", "繁體中文 (MTL)"],
            )],
        ),
        group(
            1,
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

fn audio_settings_groups(config: &Config) -> Vec<SettingsGroup> {
    audio_sources(config)
        .into_iter()
        .enumerate()
        .map(|(index, source)| {
            let entry_id = (index + 1).to_string();
            let source_group = format!("{}/{}", raw::audio_source::GROUP, entry_id);
            group(
                2,
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
                        if source.kind == 1 { "JSON" } else { "File" }.to_owned(),
                        &["File", "JSON"],
                    ),
                    text_field(
                        &format!("{source_group}/{}", raw::audio_source::MD5),
                        "MD5 skip hash",
                        source.md5,
                    ),
                ],
            )
        })
        .collect()
}

fn behavior_settings_groups(config: &Config) -> Vec<SettingsGroup> {
    let group_name = raw::behavior::GROUP;
    let dir = effective_i64(config, group_name, raw::behavior::FILE_OPEN_DIR);
    let dir_label = match dir {
        0 => "Current",
        1 => "Home",
        2 => "Movies",
        3 => "Documents",
        4 => "Custom",
        _ => "Movies",
    };
    vec![
        group(
            3,
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
            3,
            "On Screen Controller",
            "",
            vec![
                text_field(
                    "behavior/osc-duration",
                    "Visibility duration milliseconds",
                    value_text(&effective_value(config, group_name, raw::behavior::OSC_DURATION)),
                ),
                text_field(
                    "behavior/osc-fade",
                    "Fade duration milliseconds",
                    value_text(&effective_value(config, group_name, raw::behavior::OSC_FADE)),
                ),
                text_field(
                    "behavior/osc-min-move",
                    "Minimum mouse pixel movement",
                    value_text(&effective_value(config, group_name, raw::behavior::OSC_MIN_MOVE)),
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
            3,
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
            3,
            "Open File Directory",
            "",
            vec![
                choice_field(
                    "behavior/file-open-dir",
                    "Open file starting location",
                    dir_label.to_owned(),
                    &["Current", "Home", "Movies", "Documents", "Custom"],
                ),
                conditional_text_field(
                    "behavior/file-open-custom",
                    "Custom open folder URL",
                    value_text(&effective_value(
                        config,
                        group_name,
                        raw::behavior::FILE_OPEN_CUSTOM,
                    )),
                    dir == 4,
                    true,
                ),
            ],
        ),
    ]
}

fn dictionary_settings_groups(config: &Config) -> Vec<SettingsGroup> {
    let order = value_text(&effective_value(
        config,
        raw::dictionary::GROUP,
        raw::dictionary::ORDER,
    ));
    vec![group(
        4,
        "Dictionary order",
        "",
        vec![text_field(
            "dictionary/order",
            "Dictionary IDs in priority order",
            order,
        )],
    )]
}

fn interface_settings_groups(config: &Config) -> Vec<SettingsGroup> {
    let group_name = raw::interface::GROUP;
    vec![
        group(
            5,
            "Theme",
            "",
            vec![switch_field(
                "interface/system-icons",
                "Use system icons",
                effective_bool(config, group_name, raw::interface::SYSTEM_ICONS),
            )],
        ),
        group(
            5,
            "Subtitle",
            "",
            vec![
                text_field(
                    "interface/sub-font",
                    "Font (Qt font string)",
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
                    value_text(&effective_value(config, group_name, raw::interface::subtitle::SCALE)),
                ),
                text_field(
                    "interface/sub-offset",
                    "Bottom offset as a percentage of window height",
                    value_text(&effective_value(config, group_name, raw::interface::subtitle::OFFSET)),
                ),
                text_field(
                    "interface/sub-stroke",
                    "Stroke size",
                    value_text(&effective_value(config, group_name, raw::interface::subtitle::STROKE)),
                ),
                text_field(
                    "interface/sub-text-color",
                    "Text color",
                    value_text(&effective_value(
                        config,
                        group_name,
                        raw::interface::subtitle::TEXT_COLOR,
                    )),
                ),
                text_field(
                    "interface/sub-bg-color",
                    "Background color",
                    value_text(&effective_value(
                        config,
                        group_name,
                        raw::interface::subtitle::BACKGROUND_COLOR,
                    )),
                ),
                text_field(
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
            ],
        ),
        group(
            5,
            "Pop-up",
            "",
            vec![
                switch_field(
                    "interface/search-window",
                    "Open search in a separate window",
                    effective_bool(config, group_name, raw::interface::SEARCH_WINDOW),
                ),
                text_field(
                    "interface/sub-popup-width",
                    "Subtitle pop-up width",
                    value_text(&effective_value(config, group_name, raw::interface::POPUP_WIDTH)),
                ),
                text_field(
                    "interface/sub-popup-height",
                    "Subtitle pop-up height",
                    value_text(&effective_value(config, group_name, raw::interface::POPUP_HEIGHT)),
                ),
                text_field(
                    "interface/search-expression-font",
                    "Expression font (Qt font string)",
                    value_text(&effective_value(
                        config,
                        group_name,
                        raw::interface::SEARCH_EXPRESSION_FONT,
                    )),
                ),
                text_field(
                    "interface/search-reading-font",
                    "Reading font (Qt font string)",
                    value_text(&effective_value(
                        config,
                        group_name,
                        raw::interface::SEARCH_READING_FONT,
                    )),
                ),
                text_field(
                    "interface/search-conj-font",
                    "Conjugation font (Qt font string)",
                    value_text(&effective_value(
                        config,
                        group_name,
                        raw::interface::SEARCH_CONJ_FONT,
                    )),
                ),
                text_field(
                    "interface/search-tag-font",
                    "Tag font (Qt font string)",
                    value_text(&effective_value(
                        config,
                        group_name,
                        raw::interface::SEARCH_TAG_FONT,
                    )),
                ),
                text_field(
                    "interface/search-glossary-font",
                    "Glossary font (Qt font string)",
                    value_text(&effective_value(
                        config,
                        group_name,
                        raw::interface::SEARCH_GLOSSARY_FONT,
                    )),
                ),
                text_field(
                    "interface/search-kanji-font",
                    "Kanji font (Qt font string)",
                    value_text(&effective_value(
                        config,
                        group_name,
                        raw::interface::SEARCH_KANJI_FONT,
                    )),
                ),
            ],
        ),
        group(
            5,
            "Subtitle List",
            "",
            vec![
                switch_field(
                    "interface/sub-list-window",
                    "Show subtitle list window",
                    effective_bool(
                        config,
                        group_name,
                        raw::interface::subtitle_list::WINDOW,
                    ),
                ),
                switch_field(
                    "interface/sub-list-timestamps",
                    "Show timestamps",
                    effective_bool(
                        config,
                        group_name,
                        raw::interface::subtitle_list::TIMESTAMPS,
                    ),
                ),
                text_field(
                    "interface/sub-list-background-color",
                    "Background color",
                    value_text(&effective_value(
                        config,
                        group_name,
                        raw::interface::subtitle_list::BACKGROUND_COLOR,
                    )),
                ),
                text_field(
                    "interface/sub-list-text-color",
                    "Text color",
                    value_text(&effective_value(
                        config,
                        group_name,
                        raw::interface::subtitle_list::TEXT_COLOR,
                    )),
                ),
                text_field(
                    "interface/sub-list-hover-color",
                    "Hover color",
                    value_text(&effective_value(
                        config,
                        group_name,
                        raw::interface::subtitle_list::HOVER_COLOR,
                    )),
                ),
                text_field(
                    "interface/sub-list-selected-color",
                    "Selected color",
                    value_text(&effective_value(
                        config,
                        group_name,
                        raw::interface::subtitle_list::SELECTED_COLOR,
                    )),
                ),
                text_field(
                    "interface/sub-list-selected-hover-color",
                    "Selected hover color",
                    value_text(&effective_value(
                        config,
                        group_name,
                        raw::interface::subtitle_list::SELECTED_HOVER_COLOR,
                    )),
                ),
                text_field(
                    "interface/sub-list-primary-font",
                    "Primary font (Qt font string)",
                    value_text(&effective_value(
                        config,
                        group_name,
                        raw::interface::subtitle_list::PRIMARY_FONT,
                    )),
                ),
                text_field(
                    "interface/sub-list-secondary-font",
                    "Secondary font (Qt font string)",
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

fn keybind_settings_groups(config: &Config) -> Vec<SettingsGroup> {
    let names = keybind_profile_names(config);
    let current = effective_text(config, raw::keybind::GROUP, raw::keybind::CURRENT_PROFILE)
        .filter(|current| names.iter().any(|name| name == current))
        .or_else(|| names.first().cloned())
        .unwrap_or_else(|| ANKI_PROFILE_DEFAULT.to_owned());
    let profile_group = format!(
        "{}/{}/{}",
        raw::keybind::GROUP,
        raw::keybind::profile::GROUP,
        current
    );
    let mut groups = vec![group(
        6,
        "Profile",
        "",
        vec![choice_field(
            "keybind/current",
            "Current profile",
            current,
            &names.iter().map(String::as_str).collect::<Vec<_>>(),
        )],
    )];
    groups.push(group(
        6,
        "Keyboard Shortcuts",
        "",
        KEYBIND_FIELDS
            .iter()
            .map(|(key, label)| {
                text_field(
                    &format!("{profile_group}/{key}"),
                    label,
                    keybind_value(config, &profile_group, key),
                )
            })
            .collect(),
    ));
    groups
}

fn ocr_settings_groups(config: &Config) -> Vec<SettingsGroup> {
    vec![group(
        7,
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

fn asr_settings_groups(config: &Config) -> Vec<SettingsGroup> {
    let backend = effective_text(config, raw::asr::GROUP, raw::asr::BACKEND)
        .unwrap_or_else(|| raw::asr::backend_default(true, true).to_owned());
    let whisper_selected = backend == raw::asr::BACKEND_WHISPER;
    let onnx_selected = backend == raw::asr::BACKEND_ONNX;
    let whisper_model = value_text(&effective_value(
        config,
        raw::whisper::GROUP,
        raw::whisper::MODEL,
    ));
    let onnx_model = value_text(&effective_value(
        config,
        raw::onnx::GROUP,
        raw::onnx::MODEL,
    ));
    let vad_enabled = effective_bool(config, raw::whisper::GROUP, raw::whisper::VAD_ENABLED);
    let whisper_models = [
        "tiny",
        "base",
        "small",
        "medium",
        "large-v3",
        "large-v3-turbo",
        "custom",
    ];
    let onnx_models = [
        "kitsune-transcribe-t0.6b-ja",
        "kitsune-transcribe-p0.3b-ja",
        "custom",
    ];
    let whisper_model_choices = if whisper_models.contains(&whisper_model.as_str()) {
        whisper_models.to_vec()
    } else {
        [&whisper_models[..], &[whisper_model.as_str()]].concat()
    };
    let onnx_model_choices = if onnx_models.contains(&onnx_model.as_str()) {
        onnx_models.to_vec()
    } else {
        [&onnx_models[..], &[onnx_model.as_str()]].concat()
    };
    let whisper_model_choices = whisper_model_choices
        .iter()
        .copied()
        .collect::<Vec<_>>();
    let onnx_model_choices = onnx_model_choices.iter().copied().collect::<Vec<_>>();
    let thread_max = asr_thread_limit() as usize;
    let thread_options = (1..=thread_max)
        .map(|count| count.to_string())
        .collect::<Vec<_>>();
    let thread_choices = thread_options
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    let max_threads = thread_max as i64;
    let selected_threads = |group, key| {
        let requested = effective_i64(config, group, key);
        if requested <= 0 {
            4.min(max_threads)
        } else {
            requested.clamp(1, max_threads)
        }
        .to_string()
    };
    let numeric_choices = ["1", "2", "3", "4", "5", "6", "7", "8", "9", "10"];
    let selected_numeric = |group, key| effective_i64(config, group, key).clamp(1, 10).to_string();


    let mut groups = vec![
        group(
            8,
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
                    &["Whisper", "ONNX Runtime"],
                ),
                switch_field(
                    "asr/enable",
                    "Enabled",
                    effective_bool(config, raw::asr::GROUP, raw::asr::ENABLED),
                ),
                switch_field(
                    "asr/start-from-beginning",
                    "Always transcribe from start of media",
                    effective_bool(
                        config,
                        raw::asr::GROUP,
                        raw::asr::START_FROM_BEGINNING,
                    ),
                ),
            ],
        ),
    ];

    if whisper_selected {
        groups.push(group(
            8,
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
            8,
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

fn search_settings_groups(config: &Config) -> Vec<SettingsGroup> {
    let group_name = raw::search::GROUP;
    let method = effective_i64(config, group_name, raw::search::METHOD);
    let modifier = effective_i64(config, group_name, raw::search::MODIFIER);
    let glossary_style = effective_i64(config, group_name, raw::search::GLOSSARY_STYLE);
    vec![
        group(
            9,
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
            9,
            "Pop-up",
            "",
            vec![
                choice_field(
                    "search/method",
                    "Search method",
                    if method == 1 { "Modifier" } else { "Hover" }.to_owned(),
                    &["Hover", "Modifier"],
                ),
                text_field(
                    "search/delay",
                    "Search hover millisecond delay",
                    value_text(&effective_value(config, group_name, raw::search::DELAY)),
                ),
                choice_field(
                    "search/modifier",
                    "Search modifier key",
                    match modifier {
                        0 => "Alt",
                        1 => "Control",
                        3 => "Super",
                        _ => "Shift",
                    }
                    .to_owned(),
                    &["Alt", "Control", "Shift", "Super"],
                ),
                switch_field(
                    "search/middle-mouse-scan",
                    "Search using middle mouse button",
                    effective_bool(config, group_name, raw::search::MIDDLE_MOUSE_SCAN),
                ),
                choice_field(
                    "search/list-result",
                    "Glossary style",
                    match glossary_style {
                        1 => "Line Breaks",
                        2 => "Pipes",
                        _ => "Bullet Points",
                    }
                    .to_owned(),
                    &["Bullet Points", "Line Breaks", "Pipes"],
                ),
                switch_field(
                    "search/auto-play-audio",
                    "Auto play first result audio",
                    effective_bool(config, group_name, raw::search::AUTO_PLAY_AUDIO),
                ),
                switch_field(
                    "search/show-anki-glossary-checkbox",
                    "Show add to Anki glossary checkbox",
                    effective_bool(
                        config,
                        group_name,
                        raw::search::SHOW_ANKI_GLOSSARY_CHECKBOX,
                    ),
                ),
            ],
        ),
        group(
            9,
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
                    value_text(&effective_value(config, group_name, raw::search::REMOVE_REGEX)),
                ),
            ],
        ),
    ]
}

fn default_anki_profile() -> JsonValue {
    json!({
        "name": ANKI_PROFILE_DEFAULT,
        "host": "http://localhost",
        "port": ANKI_CONFIG_DEFAULT_PORT,
        "use-api-key": false,
        "api-key": "",
        "duplicate": 1,
        "newline-replace": "<br>",
        "screenshot": 0,
        "audio-pad-start": 0.0,
        "audio-pad-end": 0.0,
        "audio-normalize": false,
        "audio-db": -20.0,
        "tags": ["memento"],
        "ex-glos": [],
        "term": { "deck": "", "model": "", "fields": [] },
        "kanji": { "deck": "", "model": "", "fields": [] }
    })
}

fn default_anki_config() -> JsonValue {
    json!({
        "enabled": false,
        "setProfile": ANKI_PROFILE_DEFAULT,
        "profiles": [default_anki_profile()]
    })
}

fn ensure_json_object(value: &mut JsonValue) -> &mut Map<String, JsonValue> {
    if !value.is_object() {
        *value = JsonValue::Object(Map::new());
    }
    value.as_object_mut().expect("object set above")
}

fn set_json_default(object: &mut Map<String, JsonValue>, key: &str, default: JsonValue) {
    object.entry(key.to_owned()).or_insert(default);
}

fn normalize_anki_profile(mut profile: JsonValue) -> JsonValue {
    let defaults = default_anki_profile();
    let object = ensure_json_object(&mut profile);
    for (key, default) in defaults.as_object().expect("default object") {
        match (key.as_str(), object.get(key)) {
            ("name", Some(JsonValue::String(_)))
            | ("host", Some(JsonValue::String(_)))
            | ("port", Some(JsonValue::String(_)))
            | ("api-key", Some(JsonValue::String(_)))
            | ("newline-replace", Some(JsonValue::String(_)))
            | ("tags", Some(JsonValue::Array(_)))
            | ("ex-glos", Some(JsonValue::Array(_)))
            | ("term", Some(JsonValue::Object(_)))
            | ("kanji", Some(JsonValue::Object(_)))
            | ("use-api-key", Some(JsonValue::Bool(_)))
            | ("duplicate", Some(JsonValue::Number(_)))
            | ("screenshot", Some(JsonValue::Number(_)))
            | ("audio-pad-start", Some(JsonValue::Number(_)))
            | ("audio-pad-end", Some(JsonValue::Number(_)))
            | ("audio-normalize", Some(JsonValue::Bool(_)))
            | ("audio-db", Some(JsonValue::Number(_))) => {}
            _ => {
                object.insert(key.clone(), default.clone());
            }
        }
    }
    normalize_anki_note_settings(object, "term");
    normalize_anki_note_settings(object, "kanji");

    if let Some(host) = object.get("host").and_then(JsonValue::as_str) {
        let normalized = normalize_anki_hostname(host);
        object.insert("host".to_owned(), JsonValue::String(normalized));
    }
    profile
}

fn normalize_anki_note_settings(profile: &mut Map<String, JsonValue>, key: &str) {
    let mut note = profile
        .remove(key)
        .filter(JsonValue::is_object)
        .unwrap_or_else(|| json!({}));
    let object = ensure_json_object(&mut note);
    set_json_default(object, "deck", JsonValue::String(String::new()));
    set_json_default(object, "model", JsonValue::String(String::new()));
    set_json_default(object, "fields", JsonValue::Array(Vec::new()));
    for field in ["deck", "model"] {
        if !object.get(field).is_some_and(JsonValue::is_string) {
            object.insert(field.to_owned(), JsonValue::String(String::new()));
        }
    }
    let fields = object
        .get("fields")
        .cloned()
        .unwrap_or_else(|| JsonValue::Array(Vec::new()));
    object.insert("fields".to_owned(), normalize_anki_fields(&fields));
    profile.insert(key.to_owned(), note);
}

fn normalize_anki_fields(fields: &JsonValue) -> JsonValue {
    let mut normalized = Vec::new();
    match fields {
        JsonValue::Array(rows) => {
            for row in rows {
                let Some(pair) = row.as_array().filter(|pair| pair.len() == 2) else {
                    continue;
                };
                let (Some(name), Some(value)) = (pair[0].as_str(), pair[1].as_str()) else {
                    continue;
                };
                normalized.push(json!([name, value]));
            }
        }
        JsonValue::Object(fields) => {
            for (name, value) in fields {
                if let Some(value) = value.as_str() {
                    normalized.push(json!([name, value]));
                }
            }
        }
        _ => {}
    }
    JsonValue::Array(normalized)
}

fn normalize_anki_config(mut config: JsonValue) -> Result<JsonValue, String> {
    if !config.is_object() {
        return Err("Anki profiles file must contain a JSON object".to_owned());
    }
    let object = ensure_json_object(&mut config);
    if !object.get("enabled").is_some_and(JsonValue::is_boolean) {
        object.insert("enabled".to_owned(), JsonValue::Bool(false));
    }
    let requested = object
        .get("setProfile")
        .and_then(JsonValue::as_str)
        .unwrap_or(ANKI_PROFILE_DEFAULT)
        .to_owned();
    let raw_profiles = object
        .remove("profiles")
        .and_then(|profiles| profiles.as_array().cloned())
        .unwrap_or_default();
    let mut profiles = raw_profiles
        .into_iter()
        .map(normalize_anki_profile)
        .collect::<Vec<_>>();
    if profiles.is_empty() {
        profiles.push(default_anki_profile());
    }
    profiles.sort_by(|left, right| {
        anki_profile_name(left)
            .encode_utf16()
            .cmp(anki_profile_name(right).encode_utf16())
    });
    let selected = if profiles
        .iter()
        .any(|profile| anki_profile_name(profile) == requested)
    {
        requested
    } else {
        anki_profile_name(&profiles[0]).to_owned()
    };
    let object = ensure_json_object(&mut config);
    object.insert("setProfile".to_owned(), JsonValue::String(selected));
    object.insert("profiles".to_owned(), JsonValue::Array(profiles));
    Ok(config)
}

fn anki_profile_name(profile: &JsonValue) -> &str {
    profile
        .get("name")
        .and_then(JsonValue::as_str)
        .unwrap_or(ANKI_PROFILE_DEFAULT)
}

fn current_anki_profile_name(config: &JsonValue) -> String {
    let requested = config
        .get("setProfile")
        .and_then(JsonValue::as_str)
        .unwrap_or(ANKI_PROFILE_DEFAULT);
    config
        .get("profiles")
        .and_then(JsonValue::as_array)
        .and_then(|profiles| profiles.iter().find(|p| anki_profile_name(p) == requested))
        .map(|profile| anki_profile_name(profile).to_owned())
        .or_else(|| {
            config
                .get("profiles")
                .and_then(JsonValue::as_array)
                .and_then(|profiles| profiles.first())
                .map(|profile| anki_profile_name(profile).to_owned())
        })
        .unwrap_or_else(|| ANKI_PROFILE_DEFAULT.to_owned())
}

fn current_anki_profile(config: &JsonValue) -> &JsonValue {
    let requested = config
        .get("setProfile")
        .and_then(JsonValue::as_str)
        .unwrap_or(ANKI_PROFILE_DEFAULT);
    config
        .get("profiles")
        .and_then(JsonValue::as_array)
        .and_then(|profiles| profiles.iter().find(|profile| anki_profile_name(profile) == requested))
        .or_else(|| config.get("profiles").and_then(JsonValue::as_array).and_then(|p| p.first()))
        .unwrap_or(&JsonValue::Null)
}

fn find_anki_profile_mut<'a>(
    config: &'a mut JsonValue,
    name: &str,
) -> Option<&'a mut JsonValue> {
    config
        .get_mut("profiles")?
        .as_array_mut()?
        .iter_mut()
        .find(|profile| anki_profile_name(profile) == name)
}

fn anki_profiles(config: &JsonValue) -> Vec<&JsonValue> {
    config
        .get("profiles")
        .and_then(JsonValue::as_array)
        .map(|profiles| profiles.iter().collect())
        .unwrap_or_default()
}

fn anki_profile_string<'a>(profile: &'a JsonValue, key: &str, default: &'a str) -> &'a str {
    profile.get(key).and_then(JsonValue::as_str).unwrap_or(default)
}

fn anki_profile_bool(profile: &JsonValue, key: &str, default: bool) -> bool {
    profile.get(key).and_then(JsonValue::as_bool).unwrap_or(default)
}

fn anki_profile_i64(profile: &JsonValue, key: &str, default: i64) -> i64 {
    profile.get(key).and_then(JsonValue::as_i64).unwrap_or(default)
}

fn anki_profile_real(profile: &JsonValue, key: &str, default: f64) -> f64 {
    profile
        .get(key)
        .and_then(JsonValue::as_f64)
        .unwrap_or(default)
}

fn normalize_anki_hostname(host: &str) -> String {
    if host.starts_with("http://") || host.starts_with("https://") {
        host.to_owned()
    } else {
        format!("http://{host}")
    }
}

fn endpoint_from_profile(profile: &JsonValue) -> AnkiEndpoint {
    let use_api_key = anki_profile_bool(profile, "use-api-key", false);
    AnkiEndpoint {
        hostname: normalize_anki_hostname(anki_profile_string(
            profile,
            "host",
            "http://localhost",
        )),
        port: anki_profile_string(profile, "port", ANKI_CONFIG_DEFAULT_PORT)
            .parse::<u16>()
            .unwrap_or(8765),
        api_key: use_api_key.then(|| {
            anki_profile_string(profile, "api-key", "").to_owned()
        }),
        timeout: Some(std::time::Duration::from_millis(5000)),
        use_api_key,
    }
}

fn json_fields_to_map(fields: &JsonValue) -> Option<std::collections::HashMap<String, String>> {
    let mut result = std::collections::HashMap::new();
    match fields {
        JsonValue::Array(rows) => {
            for row in rows {
                let Some(pair) = row.as_array().filter(|pair| pair.len() == 2) else {
                    continue;
                };
                let (Some(name), Some(value)) = (pair[0].as_str(), pair[1].as_str()) else {
                    continue;
                };
                result.insert(name.to_owned(), value.to_owned());
            }
            Some(result)
        }
        JsonValue::Object(fields) => {
            for (name, value) in fields {
                if let Some(value) = value.as_str() {
                    result.insert(name.clone(), value.to_owned());
                }
            }
            Some(result)
        }
        _ => None,
    }
}

#[allow(dead_code)]
fn anki_fields_to_string(fields: &JsonValue) -> String {
    serde_json::to_string(fields).unwrap_or_else(|_| "[]".to_owned())
}

fn default_anki_field_marker(section: &str, field: &str) -> String {
    let normalized: String = field.chars().filter(|ch| ch.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase).collect();
    let marker = if section == "kanji" {
        match normalized.as_str() {
            "character" | "kanji" => "character",
            "onyomi" => "onyomi",
            "kunyomi" => "kunyomi",
            "meaning" | "glossary" | "definition" | "definitions" => "glossary",
            "strokecount" => "stroke-count",
            _ => return String::new(),
        }
    } else {
        match normalized.as_str() {
            "expression" | "word" | "term" => "expression",
            "reading" | "expressionreading" => "reading",
            "furigana" | "expressionfurigana" => "furigana",
            "sentence" | "subtitle" => "sentence",
            "context" => "context",
            "selection" | "textselection" | "textselection12" => "selection",
            "meaning" | "glossary" | "definition" | "definitions" => "glossary",
            "audio" | "wordaudio" | "expressionaudio" => "audio",
            "sentenceaudio" | "audiosentence" | "subtitleaudio" => "audio-media",
            "contextaudio" | "audiocontext" => "audio-context",
            "image" | "picture" | "screenshot" | "snapshot" => "screenshot",
            "pitch" | "pitchaccent" => "pitch",
            "frequency" | "frequencies" => "frequencies",
            _ => return String::new(),
        }
    };
    format!("{{{marker}}}")
}

fn anki_settings_groups(config: &JsonValue) -> Vec<SettingsGroup> {
    let profiles = anki_profiles(config);
    let names = profiles
        .iter()
        .map(|profile| anki_profile_name(profile).to_owned())
        .collect::<Vec<_>>();
    let current_name = current_anki_profile_name(config);
    let profile = current_anki_profile(config);
    let use_api_key = anki_profile_bool(profile, "use-api-key", false);
    let term = profile.get("term").unwrap_or(&JsonValue::Null);
    let kanji = profile.get("kanji").unwrap_or(&JsonValue::Null);
    let duplicate = anki_profile_i64(profile, "duplicate", 1);
    let screenshot = anki_profile_i64(profile, "screenshot", 0);
    vec![
        group(
            0,
            "General",
            "",
            vec![
                switch_field(
                    "anki/enabled",
                    "Enable Anki integration",
                    config.get("enabled").and_then(JsonValue::as_bool).unwrap_or(false),
                ),
                choice_field(
                    "anki/profile",
                    "Current profile",
                    current_name,
                    &names.iter().map(String::as_str).collect::<Vec<_>>(),
                ),
                text_field(
                    "anki/profile/name",
                    "Profile name",
                    anki_profile_name(profile).to_owned(),
                ),
            ],
        ),
        group(
            0,
            "AnkiConnect",
            "",
            vec![
                text_field(
                    "anki/profile/host",
                    "Hostname",
                    anki_profile_string(profile, "host", "http://localhost").to_owned(),
                ),
                text_field(
                    "anki/profile/port",
                    "Port",
                    anki_profile_string(profile, "port", ANKI_CONFIG_DEFAULT_PORT).to_owned(),
                ),
                switch_field(
                    "anki/profile/use-api-key",
                    "Use API key",
                    use_api_key,
                ),
                conditional_text_field(
                    "anki/profile/api-key",
                    "API key",
                    anki_profile_string(profile, "api-key", "").to_owned(),
                    use_api_key,
                    use_api_key,
                ),
            ],
        ),
        group(
            0,
            "Advanced",
            "",
            vec![
                choice_field(
                    "anki/profile/duplicate",
                    "Duplicate policy",
                    match duplicate {
                        0 => "None",
                        2 => "Allowed in same deck",
                        _ => "Allowed in different decks",
                    }
                    .to_owned(),
                    &["None", "Allowed in different decks", "Allowed in same deck"],
                ),
                text_field(
                    "anki/profile/newline-replace",
                    "Replace newlines with",
                    anki_profile_string(profile, "newline-replace", "<br>").to_owned(),
                ),
                choice_field(
                    "anki/profile/screenshot",
                    "Screenshot file type",
                    match screenshot {
                        1 => "PNG",
                        2 => "WebP",
                        _ => "JPEG",
                    }
                    .to_owned(),
                    &["JPEG", "PNG", "WebP"],
                ),
                text_field(
                    "anki/profile/audio-pad-start",
                    "Audio padding start duration (seconds)",
                    anki_profile_real(profile, "audio-pad-start", 0.0).to_string(),
                ),
                text_field(
                    "anki/profile/audio-pad-end",
                    "Audio padding end duration (seconds)",
                    anki_profile_real(profile, "audio-pad-end", 0.0).to_string(),
                ),
                switch_field(
                    "anki/profile/audio-normalize",
                    "Audio media normalization",
                    anki_profile_bool(profile, "audio-normalize", false),
                ),
                text_field(
                    "anki/profile/audio-db",
                    "Audio normalization dB",
                    anki_profile_real(profile, "audio-db", -20.0).to_string(),
                ),
                text_field(
                    "anki/profile/ex-glos",
                    "Exclude dictionaries from glossary (comma-separated)",
                    profile
                        .get("ex-glos")
                        .and_then(JsonValue::as_array)
                        .map(|values| values.iter().filter_map(JsonValue::as_str).collect::<Vec<_>>().join(", "))
                        .unwrap_or_default(),
                ),
            ],
        ),
        group(
            0,
            "Cards",
            "",
            vec![text_field(
                "anki/profile/tags",
                "Tags (space or comma-separated)",
                profile
                    .get("tags")
                    .and_then(JsonValue::as_array)
                    .map(|values| {
                        values
                            .iter()
                            .filter_map(JsonValue::as_str)
                            .collect::<Vec<_>>()
                            .join(" ")
                    })
                    .unwrap_or_default(),
            )],
        ),
        group(
            0,
            "Term Notes",
            "",
            unpack_note_fields("term", term),
        ),
        group(
            0,
            "Kanji Notes",
            "",
            unpack_note_fields("kanji", kanji),
        ),
    ]
}

fn unpack_note_fields(section: &str, note: &JsonValue) -> Vec<SettingsField> {
    let mut fields = vec![
        text_field(
            &format!("anki/profile/{section}/deck"),
            "Deck",
            anki_profile_string(note, "deck", "").to_owned(),
        ),
        text_field(
            &format!("anki/profile/{section}/model"),
            "Model",
            anki_profile_string(note, "model", "").to_owned(),
        ),
    ];

    let mut found_fields = Vec::new();
    if let Some(arr) = note.get("fields").and_then(JsonValue::as_array) {
        for row in arr {
            if let Some(pair) = row.as_array().filter(|p| p.len() == 2) {
                if let (Some(name), Some(template)) = (pair[0].as_str(), pair[1].as_str()) {
                    found_fields.push((name.to_owned(), template.to_owned()));
                }
            }
        }
    }

    for (name, template) in found_fields {
        fields.push(text_field(
            &format!("anki/profile/{section}/field/{name}"),
            &name,
            template,
        ));
    }

    fields
}

fn edit_anki_setting(config: &mut JsonValue, key: &str, value: &str) -> Result<(), String> {
    let setting = key
        .strip_prefix("anki/")
        .ok_or_else(|| format!("invalid Anki setting key: {key}"))?;
    match setting {
        "enabled" => {
            let enabled = parse_bool(value)?;
            ensure_json_object(config).insert("enabled".to_owned(), JsonValue::Bool(enabled));
            return Ok(());
        }
        "profile" => {
            if !anki_profiles(config)
                .iter()
                .any(|profile| anki_profile_name(profile) == value)
            {
                return Err(format!("Anki profile {value:?} does not exist"));
            }
            ensure_json_object(config)
                .insert("setProfile".to_owned(), JsonValue::String(value.to_owned()));
            return Ok(());
        }
        _ => {}
    }

    let relative = setting
        .strip_prefix("profile/")
        .ok_or_else(|| format!("unknown Anki setting: {key}"))?;
    let current_name = current_anki_profile_name(config);
    if relative == "name" {
        let name = value.trim();
        if name.is_empty() {
            return Err("Anki profile name cannot be empty".to_owned());
        }
        if anki_profiles(config)
            .iter()
            .any(|profile| anki_profile_name(profile) == name && name != current_name)
        {
            return Err(format!("Anki profile {name:?} already exists"));
        }
        let profile = find_anki_profile_mut(config, &current_name)
            .ok_or_else(|| format!("Anki profile {current_name:?} does not exist"))?;
        ensure_json_object(profile).insert("name".to_owned(), JsonValue::String(name.to_owned()));
        let object = ensure_json_object(config);
        object.insert("setProfile".to_owned(), JsonValue::String(name.to_owned()));
        sort_anki_profiles(config);
        return Ok(());
    }

    let profile = find_anki_profile_mut(config, &current_name)
        .ok_or_else(|| format!("Anki profile {current_name:?} does not exist"))?;
    edit_anki_profile_field(profile, relative, value)
}

fn edit_anki_profile_field(
    profile: &mut JsonValue,
    relative: &str,
    value: &str,
) -> Result<(), String> {
    let (section, key) = relative
        .split_once('/')
        .map_or(("profile", relative), |(section, key)| (section, key));
    if section == "term" || section == "kanji" {
        let note = profile
            .get_mut(section)
            .and_then(JsonValue::as_object_mut)
            .ok_or_else(|| format!("Anki {section} settings are malformed"))?;
        if let Some(field_name) = key.strip_prefix("field/") {
            let fields_entry = note.entry("fields".to_owned()).or_insert_with(|| JsonValue::Array(Vec::new()));
            if !fields_entry.is_array() {
                *fields_entry = JsonValue::Array(Vec::new());
            }
            let arr = fields_entry.as_array_mut().unwrap();
            let mut found = false;
            for item in arr.iter_mut() {
                if let Some(pair) = item.as_array_mut().filter(|p| p.len() == 2) {
                    if pair[0].as_str() == Some(field_name) {
                        pair[1] = JsonValue::String(value.to_owned());
                        found = true;
                        break;
                    }
                }
            }
            if !found {
                arr.push(json!([field_name, value]));
            }
            return Ok(());
        }

        match key {
            "deck" | "model" => {
                note.insert(key.to_owned(), JsonValue::String(value.to_owned()));
                return Ok(());
            }
            "fields" => {
                let fields: JsonValue = serde_json::from_str(value)
                    .map_err(|error| format!("invalid {section} field templates JSON: {error}"))?;
                if !fields.is_array() && !fields.is_object() {
                    return Err(format!("{section} field templates must be a JSON array or object"));
                }
                note.insert("fields".to_owned(), normalize_anki_fields(&fields));
                return Ok(());
            }
            _ => return Err(format!("unknown Anki {section} setting {key:?}")),
        }
    }

    let object = profile
        .as_object_mut()
        .ok_or_else(|| "Anki profile is malformed".to_owned())?;
    match key {
        "host" => {
            if value.trim().is_empty() {
                return Err("Anki hostname cannot be empty".to_owned());
            }
            let host = normalize_anki_hostname(value.trim());
            if !host.starts_with("http://") && !host.starts_with("https://") {
                return Err("Anki hostname must use http:// or https://".to_owned());
            }
            object.insert("host".to_owned(), JsonValue::String(host));
        }
        "port" => {
            let port = value
                .parse::<u16>()
                .map_err(|_| "Anki port must be between 0 and 65535".to_owned())?;
            object.insert("port".to_owned(), JsonValue::String(port.to_string()));
        }
        "use-api-key" | "audio-normalize" => {
            object.insert(key.to_owned(), JsonValue::Bool(parse_bool(value)?));
        }
        "api-key" | "newline-replace" => {
            object.insert(key.to_owned(), JsonValue::String(value.to_owned()));
        }
        "duplicate" | "screenshot" => {
            let index = match (key, value) {
                ("duplicate", "None") => 0,
                ("duplicate", "Allowed in different decks") => 1,
                ("duplicate", "Allowed in same deck") => 2,
                ("screenshot", "JPEG") => 0,
                ("screenshot", "PNG") => 1,
                ("screenshot", "WebP") => 2,
                _ => value.parse::<i64>().map_err(|_| {
                    format!("invalid Anki {key} option: {value:?}")
                })?,
            };
            if !(0..=2).contains(&index) {
                return Err(format!("Anki {key} must be 0, 1, or 2"));
            }
            object.insert(key.to_owned(), json!(index));
        }
        "audio-pad-start" | "audio-pad-end" | "audio-db" => {
            let number = value
                .parse::<f64>()
                .ok()
                .filter(|number| number.is_finite())
                .ok_or_else(|| format!("Anki {key} must be a finite number"))?;
            object.insert(key.to_owned(), json!(number));
        }
        "tags" => {
            let tags = value
                .split(|ch: char| ch.is_whitespace() || ch == ',')
                .filter(|tag| !tag.is_empty())
                .map(|tag| JsonValue::String(tag.to_owned()))
                .collect();
            object.insert("tags".to_owned(), JsonValue::Array(tags));
        }
        "ex-glos" => {
            let names = value
                .split(',')
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(|name| JsonValue::String(name.to_owned()))
                .collect();
            object.insert("ex-glos".to_owned(), JsonValue::Array(names));
        }
        _ => return Err(format!("unknown Anki profile setting {key:?}")),
    }
    Ok(())
}

fn parse_bool(value: &str) -> Result<bool, String> {
    match value {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(format!("expected true or false, received {value:?}")),
    }
}

fn sort_anki_profiles(config: &mut JsonValue) {
    if let Some(profiles) = config.get_mut("profiles").and_then(JsonValue::as_array_mut) {
        profiles.sort_by(|left, right| {
            anki_profile_name(left)
                .encode_utf16()
                .cmp(anki_profile_name(right).encode_utf16())
        });
    }
}

fn add_anki_profile(config: &mut JsonValue, requested_name: &str) -> Result<(), String> {
    let name = requested_name.trim();
    if name.is_empty() {
        return Err("Anki profile name cannot be empty".to_owned());
    }
    if anki_profiles(config)
        .iter()
        .any(|profile| anki_profile_name(profile) == name)
    {
        return Err(format!("Anki profile {name:?} already exists"));
    }
    let base_name = current_anki_profile_name(config);
    let mut new_profile = find_anki_profile_mut(config, &base_name)
        .cloned()
        .unwrap_or_else(default_anki_profile);
    ensure_json_object(&mut new_profile)
        .insert("name".to_owned(), JsonValue::String(name.to_owned()));
    let profiles = ensure_json_object(config)
        .entry("profiles".to_owned())
        .or_insert_with(|| JsonValue::Array(Vec::new()))
        .as_array_mut()
        .ok_or_else(|| "Anki profiles must be a JSON array".to_owned())?;
    profiles.push(new_profile);
    sort_anki_profiles(config);
    ensure_json_object(config).insert("setProfile".to_owned(), JsonValue::String(name.to_owned()));
    Ok(())
}

fn remove_anki_profile(config: &mut JsonValue, name: &str) -> Result<(), String> {
    {
        let profiles = config
            .get_mut("profiles")
            .and_then(JsonValue::as_array_mut)
            .ok_or_else(|| "Anki profiles must be a JSON array".to_owned())?;
        let index = profiles
            .iter()
            .position(|profile| anki_profile_name(profile) == name)
            .ok_or_else(|| format!("Anki profile {name:?} does not exist"))?;
        profiles.remove(index);
        if profiles.is_empty() {
            profiles.push(default_anki_profile());
        }
        profiles.sort_by(|left, right| {
            anki_profile_name(left)
                .encode_utf16()
                .cmp(anki_profile_name(right).encode_utf16())
        });
    }
    let current = config
        .get("setProfile")
        .and_then(JsonValue::as_str)
        .unwrap_or(ANKI_PROFILE_DEFAULT);
    let selected_still_exists = config
        .get("profiles")
        .and_then(JsonValue::as_array)
        .is_some_and(|profiles| profiles.iter().any(|profile| anki_profile_name(profile) == current));
    if current == name || !selected_still_exists {
        let first = config
            .get("profiles")
            .and_then(JsonValue::as_array)
            .and_then(|profiles| profiles.first())
            .map(|profile| anki_profile_name(profile).to_owned())
            .unwrap_or_else(|| ANKI_PROFILE_DEFAULT.to_owned());
        ensure_json_object(config).insert("setProfile".to_owned(), JsonValue::String(first));
    }
    Ok(())
}

fn clear_field_values(fields: &mut JsonValue) {
    match fields {
        JsonValue::Array(rows) => {
            for row in rows {
                if let Some(pair) = row.as_array_mut() {
                    if pair.len() > 1 {
                        pair[1] = JsonValue::String(String::new());
                    }
                }
            }
        }
        JsonValue::Object(values) => {
            for value in values.values_mut() {
                *value = JsonValue::String(String::new());
            }
        }
        _ => {}
    }
}

fn reset_anki_profile_defaults(profile: &mut JsonValue) {
    let name = anki_profile_name(profile).to_owned();
    let object = ensure_json_object(profile);
    let defaults = default_anki_profile();
    for key in [
        "host",
        "port",
        "use-api-key",
        "api-key",
        "duplicate",
        "newline-replace",
        "screenshot",
        "audio-pad-start",
        "audio-pad-end",
        "audio-normalize",
        "audio-db",
        "tags",
        "ex-glos",
    ] {
        if let Some(value) = defaults.get(key) {
            object.insert(key.to_owned(), value.clone());
        }
    }
    object.insert("name".to_owned(), JsonValue::String(name));
    for key in ["term", "kanji"] {
        if let Some(fields) = object
            .get_mut(key)
            .and_then(JsonValue::as_object_mut)
            .and_then(|note| note.get_mut("fields"))
        {
            clear_field_values(fields);
        }
    }
}

fn category_snapshot(config: &Config, category: i32) -> ConfigSnapshot {
    if category == 2 {
        let sources = audio_sources(config);
        let mut values = vec![(
            raw::audio_source::GROUP.to_owned(),
            "size".to_owned(),
            Some(Value::Int(sources.len() as i64)),
        )];
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

    if category == 6 {
        let mut values = Vec::new();
        values.push((
            raw::keybind::GROUP.to_owned(),
            raw::keybind::CURRENT_PROFILE.to_owned(),
            Some(Value::Text(
                effective_text(config, raw::keybind::GROUP, raw::keybind::CURRENT_PROFILE)
                    .filter(|name| keybind_profile_names(config).iter().any(|item| item == name))
                    .or_else(|| keybind_profile_names(config).first().cloned())
                    .unwrap_or_else(|| ANKI_PROFILE_DEFAULT.to_owned()),
            )),
        ));
        let prefix = format!("{}/{}/", raw::keybind::GROUP, raw::keybind::profile::GROUP);
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
    if category == 8 {
        let group = raw::asr::GROUP;
        let key = raw::asr::BACKEND;
        if !values
            .iter()
            .any(|(path_group, path_key, value)| path_group == group && path_key == key && value.is_some())
        {
            values.push((
                group.to_owned(),
                key.to_owned(),
                Some(Value::Text(raw::asr::backend_default(true, true).to_owned())),
            ));
        }
    }
    ConfigSnapshot {
        values,
        replace_prefixes: Vec::new(),
    }
}

fn restore_category(config: &mut Config, snapshot: &ConfigSnapshot) {
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

fn parse_config_value(
    group: &str,
    key: &str,
    value: &str,
    current: Value,
) -> Result<Value, String> {
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
        Value::Text(_) => Ok(Value::Text(value.to_owned())),
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
            for part in value.split(',').map(str::trim).filter(|part| !part.is_empty()) {
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
        (raw::application::GROUP, raw::application::LANGUAGE) => Some((0, 4)),
        (raw::behavior::GROUP, raw::behavior::AUTO_FIT_PERCENT) => Some((1, 1000)),
        (raw::behavior::GROUP, raw::behavior::OSC_DURATION) => Some((50, 9999)),
        (raw::behavior::GROUP, raw::behavior::OSC_FADE) => Some((0, 9999)),
        (raw::behavior::GROUP, raw::behavior::OSC_MIN_MOVE) => Some((0, 99)),
        (raw::behavior::GROUP, raw::behavior::FILE_OPEN_DIR) => Some((0, 4)),
        (raw::interface::GROUP, raw::interface::POPUP_WIDTH)
        | (raw::interface::GROUP, raw::interface::POPUP_HEIGHT) => Some((1, 99999)),
        (raw::search::GROUP, raw::search::METHOD) => Some((0, 1)),
        (raw::search::GROUP, raw::search::DELAY) => Some((0, 99999)),
        (raw::search::GROUP, raw::search::MODIFIER) => Some((0, 3)),
        (raw::search::GROUP, raw::search::GLOSSARY_STYLE) => Some((0, 2)),
        (raw::whisper::GROUP, raw::whisper::THREADS)
        | (raw::onnx::GROUP, raw::onnx::THREADS) => Some((
            1,
            asr_thread_limit(),
        )),
        (raw::whisper::GROUP, raw::whisper::BEST_OF)
        | (raw::whisper::GROUP, raw::whisper::BEAM_SIZE) => Some((1, 10)),
        (group, raw::audio_source::TYPE)
            if group.starts_with(&format!("{}/", raw::audio_source::GROUP)) =>
        {
            Some((0, 1))
        }
        _ => None,
    }
}
fn real_range(group: &str, key: &str) -> Option<(f64, f64)> {
    match (group, key) {
        (raw::interface::GROUP, raw::interface::subtitle::SCALE) => Some((0.001, 1.0)),
        (raw::interface::GROUP, raw::interface::subtitle::OFFSET)
        | (raw::interface::GROUP, raw::interface::subtitle::FURIGANA_RATIO) => {
            Some((0.0, 1.0))
        }
        (raw::interface::GROUP, raw::interface::subtitle::STROKE) => Some((0.0, 1000.0)),
        _ => None,
    }
}



fn integer_choice(group: &str, key: &str, value: &str) -> Option<i64> {
    let choice = match (group, key, value) {
        (raw::application::GROUP, raw::application::LANGUAGE, "System") => 0,
        (raw::application::GROUP, raw::application::LANGUAGE, "English") => 1,
        (raw::application::GROUP, raw::application::LANGUAGE, "한국어 (MTL)") => 2,
        (raw::application::GROUP, raw::application::LANGUAGE, "简体中文 (MTL)") => 3,
        (raw::application::GROUP, raw::application::LANGUAGE, "繁體中文 (MTL)") => 4,
        (raw::behavior::GROUP, raw::behavior::FILE_OPEN_DIR, "Current") => 0,
        (raw::behavior::GROUP, raw::behavior::FILE_OPEN_DIR, "Home") => 1,
        (raw::behavior::GROUP, raw::behavior::FILE_OPEN_DIR, "Movies") => 2,
        (raw::behavior::GROUP, raw::behavior::FILE_OPEN_DIR, "Documents") => 3,
        (raw::behavior::GROUP, raw::behavior::FILE_OPEN_DIR, "Custom") => 4,
        (raw::search::GROUP, raw::search::METHOD, "Hover") => 0,
        (raw::search::GROUP, raw::search::METHOD, "Modifier") => 1,
        (raw::search::GROUP, raw::search::MODIFIER, "Alt") => 0,
        (raw::search::GROUP, raw::search::MODIFIER, "Control") => 1,
        (raw::search::GROUP, raw::search::MODIFIER, "Shift") => 2,
        (raw::search::GROUP, raw::search::MODIFIER, "Super") => 3,
        (raw::search::GROUP, raw::search::GLOSSARY_STYLE, "Bullet Points") => 0,
        (raw::search::GROUP, raw::search::GLOSSARY_STYLE, "Line Breaks") => 1,
        (raw::search::GROUP, raw::search::GLOSSARY_STYLE, "Pipes") => 2,
        (group, raw::audio_source::TYPE, "File")
            if group.starts_with(&format!("{}/", raw::audio_source::GROUP)) =>
        {
            0
        }
        (group, raw::audio_source::TYPE, "JSON")
            if group.starts_with(&format!("{}/", raw::audio_source::GROUP)) =>
        {
            1
        }
        _ => return None,
    };
    Some(choice)
}

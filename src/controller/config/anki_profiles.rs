//! Typed AnkiConnect profiles (`anki_connect.json`), note presets, validation,
//! and Anki settings groups.

use std::collections::{BTreeMap, HashMap};
use std::time::Duration;

use memento_net::anki::AnkiEndpoint;
use serde_json::{json, Map, Value as JsonValue};

use crate::ui::{SettingsField, SettingsGroup};

use super::schema::{parse_bool, SettingsCategory};
use super::ui_groups::{choice_field, conditional_text_field, group, switch_field, text_field};

pub(crate) const ANKI_PROFILE_DEFAULT: &str = "Default";
const ANKI_DEFAULT_HOST: &str = "http://localhost";
const ANKI_CONFIG_DEFAULT_PORT: &str = "8765";
const ANKI_CONFIG_DEFAULT_PORT_NUM: u16 = 8765;

#[derive(Clone, Debug)]
pub struct AnkiNoteConfig {
    pub enabled: bool,
    pub endpoint: AnkiEndpoint,
    pub deck: String,
    pub model: String,
    pub field_templates: HashMap<String, String>,
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

/// Supported one-click Anki note card presets (#328 / #214).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum AnkiNotePreset {
    Lapis,
    Kiku,
    Senren,
    Animecards,
    Jpmn,
}

impl AnkiNotePreset {
    pub fn from_name(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "lapis" => Some(Self::Lapis),
            "kiku" => Some(Self::Kiku),
            "senren" => Some(Self::Senren),
            "animecards" => Some(Self::Animecards),
            "jpmn" => Some(Self::Jpmn),
            _ => None,
        }
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::Lapis => "Lapis",
            Self::Kiku => "Kiku",
            Self::Senren => "Senren",
            Self::Animecards => "Animecards",
            Self::Jpmn => "JPMN",
        }
    }
}

/// Returns the ordered `(field_name, memento_marker_template)` mapping for a
/// popular Japanese Anki note type preset.
pub fn anki_note_preset_fields(preset: AnkiNotePreset) -> Vec<(&'static str, &'static str)> {
    match preset {
        AnkiNotePreset::Lapis => vec![
            ("Expression", "{expression}"),
            ("ExpressionFurigana", "{furigana-plain}"),
            ("ExpressionReading", "{reading}"),
            ("ExpressionAudio", "{audio | audio-media}"),
            ("SelectionText", "{selection}"),
            ("MainDefinition", "{selection | glossary}"),
            (
                "Sentence",
                "{cloze-prefix}<b>{cloze-body}</b>{cloze-suffix}",
            ),
            ("SentenceFurigana", "{sentence}"),
            ("SentenceAudio", "{audio-context}"),
            ("Picture", "{screenshot-video | screenshot}"),
            ("Glossary", "{glossary}"),
            ("PitchPosition", "{pitch-positions}"),
            ("PitchCategories", "{pitch-categories}"),
            ("Frequency", "{frequencies}"),
            ("FreqSort", "{frequency-harmonic-rank}"),
            ("MiscInfo", "{title}"),
        ],
        AnkiNotePreset::Kiku => vec![
            ("Expression", "{expression}"),
            ("ExpressionFurigana", "{furigana-plain}"),
            ("ExpressionReading", "{reading}"),
            ("ExpressionAudio", "{audio | audio-media}"),
            ("MainDefinition", "{selection | glossary}"),
            (
                "Sentence",
                "{cloze-prefix}<b>{cloze-body}</b>{cloze-suffix}",
            ),
            ("SentenceAudio", "{audio-context}"),
            ("Picture", "{screenshot-video | screenshot}"),
            ("Glossary", "{glossary}"),
            ("PitchPosition", "{pitch-positions}"),
            ("PitchGraph", "{pitch-graphs}"),
            ("PitchCategories", "{pitch-categories}"),
            ("Frequency", "{frequencies}"),
            ("FreqSort", "{frequency-harmonic-rank}"),
            ("MiscInfo", "{title}"),
        ],
        AnkiNotePreset::Senren => vec![
            ("word", "{expression}"),
            ("reading", "{reading}"),
            ("furigana", "{furigana-plain}"),
            ("wordAudio", "{audio | audio-media}"),
            (
                "sentence",
                "{cloze-prefix}<b>{cloze-body}</b>{cloze-suffix}",
            ),
            ("sentenceAudio", "{audio-context}"),
            ("picture", "{screenshot-video | screenshot}"),
            ("definition", "{selection | glossary}"),
            ("glossary", "{glossary}"),
            ("pitchPosition", "{pitch-positions}"),
            ("pitchGraph", "{pitch-graphs}"),
            ("pitchCategory", "{pitch-categories}"),
            ("frequency", "{frequencies}"),
            ("freqSort", "{frequency-harmonic-rank}"),
            ("miscInfo", "{title}"),
        ],
        AnkiNotePreset::Animecards => vec![
            ("Word", "{expression}"),
            ("Word Reading", "{reading}"),
            ("Word Furigana", "{furigana-plain}"),
            ("Word Audio", "{audio | audio-media}"),
            (
                "Sentence",
                "{cloze-prefix}<b>{cloze-body}</b>{cloze-suffix}",
            ),
            ("Sentence Audio", "{audio-context}"),
            ("Picture", "{screenshot-video | screenshot}"),
            ("Definition", "{selection | glossary}"),
            ("Pitch Accent", "{pitch-graphs}"),
            ("Pitch Position", "{pitch-positions}"),
            ("Frequency", "{frequencies}"),
            ("FreqSort", "{frequency-harmonic-rank}"),
            ("Source", "{title}"),
        ],
        AnkiNotePreset::Jpmn => vec![
            ("Word", "{expression}"),
            ("WordReadingHiragana", "{reading}"),
            ("WordFurigana", "{furigana-plain}"),
            ("WordAudio", "{audio | audio-media}"),
            ("PrimaryDefinition", "{selection | glossary}"),
            ("SecondaryDefinition", "{glossary}"),
            (
                "Sentence",
                "{cloze-prefix}<b>{cloze-body}</b>{cloze-suffix}",
            ),
            ("SentenceAudio", "{audio-context}"),
            ("Picture", "{screenshot-video | screenshot}"),
            ("PAOverride", "{pitch-positions}"),
            ("PAGraphs", "{pitch-graphs}"),
            ("PitchCategories", "{pitch-categories}"),
            ("Frequencies", "{frequencies}"),
            ("FreqSort", "{frequency-harmonic-rank}"),
            ("UtilityDictionaries", "{title}"),
        ],
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct AnkiNoteSection {
    pub(crate) deck: String,
    pub(crate) model: String,
    pub(crate) fields: Vec<(String, String)>,
    pub(crate) extra: BTreeMap<String, JsonValue>,
}

impl AnkiNoteSection {
    fn to_json_value(&self) -> JsonValue {
        let mut obj = Map::new();
        for (k, v) in &self.extra {
            obj.insert(k.clone(), v.clone());
        }
        obj.insert("deck".to_owned(), JsonValue::String(self.deck.clone()));
        obj.insert("model".to_owned(), JsonValue::String(self.model.clone()));
        let fields = self
            .fields
            .iter()
            .map(|(name, template)| json!([name, template]))
            .collect();
        obj.insert("fields".to_owned(), JsonValue::Array(fields));
        JsonValue::Object(obj)
    }

    fn from_json(value: Option<&JsonValue>) -> Self {
        let Some(object) = value.and_then(JsonValue::as_object) else {
            return Self::default();
        };
        let deck = object
            .get("deck")
            .and_then(JsonValue::as_str)
            .unwrap_or("")
            .to_owned();
        let model = object
            .get("model")
            .and_then(JsonValue::as_str)
            .unwrap_or("")
            .to_owned();
        let fields = object
            .get("fields")
            .map(parse_anki_fields_value)
            .unwrap_or_default();
        let extra = object
            .iter()
            .filter(|(k, _)| !matches!(k.as_str(), "deck" | "model" | "fields"))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        Self {
            deck,
            model,
            fields,
            extra,
        }
    }

    pub(crate) fn field_templates_map(&self) -> HashMap<String, String> {
        self.fields
            .iter()
            .map(|(name, template)| (name.clone(), template.clone()))
            .collect()
    }

    fn set_field(&mut self, field_name: &str, template: String) {
        if let Some((_, existing)) = self.fields.iter_mut().find(|(name, _)| name == field_name) {
            *existing = template;
        } else {
            self.fields.push((field_name.to_owned(), template));
        }
    }

    fn clear_field_templates(&mut self) {
        for (_, template) in &mut self.fields {
            template.clear();
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct AnkiProfile {
    pub(crate) name: String,
    pub(crate) host: String,
    pub(crate) port: String,
    pub(crate) use_api_key: bool,
    pub(crate) api_key: String,
    pub(crate) duplicate: i64,
    pub(crate) newline_replace: String,
    pub(crate) screenshot: i64,
    pub(crate) audio_pad_start: f64,
    pub(crate) audio_pad_end: f64,
    pub(crate) audio_normalize: bool,
    pub(crate) audio_db: f64,
    pub(crate) tags: Vec<String>,
    pub(crate) show_tag: bool,
    pub(crate) ex_glos: Vec<String>,
    pub(crate) term: AnkiNoteSection,
    pub(crate) kanji: AnkiNoteSection,
    pub(crate) extra: BTreeMap<String, JsonValue>,
}

impl Default for AnkiProfile {
    fn default() -> Self {
        Self {
            name: ANKI_PROFILE_DEFAULT.to_owned(),
            host: ANKI_DEFAULT_HOST.to_owned(),
            port: ANKI_CONFIG_DEFAULT_PORT.to_owned(),
            use_api_key: false,
            api_key: String::new(),
            duplicate: 1,
            newline_replace: "<br>".to_owned(),
            screenshot: 0,
            audio_pad_start: 0.0,
            audio_pad_end: 0.0,
            audio_normalize: false,
            audio_db: -20.0,
            tags: vec!["memento".to_owned()],
            show_tag: false,
            ex_glos: Vec::new(),
            term: AnkiNoteSection::default(),
            kanji: AnkiNoteSection::default(),
            extra: BTreeMap::new(),
        }
    }
}

impl AnkiProfile {
    fn to_json_value(&self) -> JsonValue {
        let mut obj = Map::new();
        for (k, v) in &self.extra {
            obj.insert(k.clone(), v.clone());
        }
        obj.insert("name".to_owned(), JsonValue::String(self.name.clone()));
        obj.insert("host".to_owned(), JsonValue::String(self.host.clone()));
        obj.insert("port".to_owned(), JsonValue::String(self.port.clone()));
        obj.insert("use-api-key".to_owned(), JsonValue::Bool(self.use_api_key));
        obj.insert(
            "api-key".to_owned(),
            JsonValue::String(self.api_key.clone()),
        );
        obj.insert("duplicate".to_owned(), json!(self.duplicate));
        obj.insert(
            "newline-replace".to_owned(),
            JsonValue::String(self.newline_replace.clone()),
        );
        obj.insert("screenshot".to_owned(), json!(self.screenshot));
        obj.insert("audio-pad-start".to_owned(), json!(self.audio_pad_start));
        obj.insert("audio-pad-end".to_owned(), json!(self.audio_pad_end));
        obj.insert(
            "audio-normalize".to_owned(),
            JsonValue::Bool(self.audio_normalize),
        );
        obj.insert("audio-db".to_owned(), json!(self.audio_db));
        obj.insert(
            "tags".to_owned(),
            JsonValue::Array(
                self.tags
                    .iter()
                    .map(|t| JsonValue::String(t.clone()))
                    .collect(),
            ),
        );
        obj.insert("show-tag".to_owned(), JsonValue::Bool(self.show_tag));
        obj.insert(
            "ex-glos".to_owned(),
            JsonValue::Array(
                self.ex_glos
                    .iter()
                    .map(|g| JsonValue::String(g.clone()))
                    .collect(),
            ),
        );
        obj.insert("term".to_owned(), self.term.to_json_value());
        obj.insert("kanji".to_owned(), self.kanji.to_json_value());
        JsonValue::Object(obj)
    }

    fn from_json(value: JsonValue) -> Self {
        let Some(object) = value.as_object() else {
            return Self::default();
        };
        let defaults = Self::default();

        let name = str_field(object, "name", &defaults.name);
        let raw_host = str_field(object, "host", &defaults.host);
        let host = validate_and_normalize_anki_hostname(&raw_host)
            .unwrap_or_else(|_| defaults.host.clone());
        let port = parse_json_port(object.get("port")).unwrap_or_else(|| defaults.port.clone());
        let use_api_key = bool_field(object, "use-api-key", defaults.use_api_key);
        let api_key = str_field(object, "api-key", &defaults.api_key);
        let duplicate = i64_field(object, "duplicate", defaults.duplicate);
        let newline_replace = str_field(object, "newline-replace", &defaults.newline_replace);
        let screenshot = i64_field(object, "screenshot", defaults.screenshot);
        let audio_pad_start = f64_field(object, "audio-pad-start", defaults.audio_pad_start);
        let audio_pad_end = f64_field(object, "audio-pad-end", defaults.audio_pad_end);
        let audio_normalize = bool_field(object, "audio-normalize", defaults.audio_normalize);
        let audio_db = f64_field(object, "audio-db", defaults.audio_db);
        let tags = string_array_field(object, "tags").unwrap_or(defaults.tags);
        let show_tag = bool_field(object, "show-tag", defaults.show_tag);
        let ex_glos = string_array_field(object, "ex-glos").unwrap_or(defaults.ex_glos);
        let term = AnkiNoteSection::from_json(object.get("term"));
        let kanji = AnkiNoteSection::from_json(object.get("kanji"));

        const KNOWN_KEYS: &[&str] = &[
            "name",
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
            "show-tag",
            "ex-glos",
            "term",
            "kanji",
        ];
        let extra = object
            .iter()
            .filter(|(k, _)| !KNOWN_KEYS.contains(&k.as_str()))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();

        Self {
            name,
            host,
            port,
            use_api_key,
            api_key,
            duplicate,
            newline_replace,
            screenshot,
            audio_pad_start,
            audio_pad_end,
            audio_normalize,
            audio_db,
            tags,
            show_tag,
            ex_glos,
            term,
            kanji,
            extra,
        }
    }

    pub(crate) fn note_section(&self, section: &str) -> Option<&AnkiNoteSection> {
        match section {
            "term" => Some(&self.term),
            "kanji" => Some(&self.kanji),
            _ => None,
        }
    }

    pub(crate) fn note_section_mut(&mut self, section: &str) -> Option<&mut AnkiNoteSection> {
        match section {
            "term" => Some(&mut self.term),
            "kanji" => Some(&mut self.kanji),
            _ => None,
        }
    }

    pub(crate) fn endpoint(&self) -> AnkiEndpoint {
        let hostname = validate_and_normalize_anki_hostname(&self.host)
            .unwrap_or_else(|_| ANKI_DEFAULT_HOST.to_owned());
        let port = self
            .port
            .trim()
            .parse::<u16>()
            .unwrap_or(ANKI_CONFIG_DEFAULT_PORT_NUM);
        AnkiEndpoint {
            hostname,
            port,
            api_key: self.use_api_key.then(|| self.api_key.clone()),
            timeout: Some(Duration::from_millis(5000)),
            use_api_key: self.use_api_key,
        }
    }

    pub(crate) fn reset_defaults(&mut self) {
        let name = self.name.clone();
        let mut term = std::mem::take(&mut self.term);
        let mut kanji = std::mem::take(&mut self.kanji);
        let extra = std::mem::take(&mut self.extra);
        term.clear_field_templates();
        kanji.clear_field_templates();
        *self = Self {
            name,
            term,
            kanji,
            extra,
            ..Self::default()
        };
    }
}

#[derive(Clone, Debug)]
pub(crate) struct AnkiProfilesFile {
    pub(crate) enabled: bool,
    pub(crate) set_profile: String,
    pub(crate) profiles: Vec<AnkiProfile>,
    pub(crate) extra: BTreeMap<String, JsonValue>,
}

impl Default for AnkiProfilesFile {
    fn default() -> Self {
        Self {
            enabled: false,
            set_profile: ANKI_PROFILE_DEFAULT.to_owned(),
            profiles: vec![AnkiProfile::default()],
            extra: BTreeMap::new(),
        }
    }
}

impl AnkiProfilesFile {
    pub(crate) fn from_json_str(text: &str) -> Result<Self, String> {
        let parsed: JsonValue = serde_json::from_str(text)
            .map_err(|error| format!("could not parse Anki profiles: {error}"))?;
        Self::from_json_value(parsed)
    }

    pub(crate) fn from_json_value(value: JsonValue) -> Result<Self, String> {
        let JsonValue::Object(mut object) = value else {
            return Err("Anki profiles file must contain a JSON object".to_owned());
        };
        let enabled = object
            .remove("enabled")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let requested = object
            .remove("setProfile")
            .and_then(|v| v.as_str().map(str::to_owned))
            .unwrap_or_else(|| ANKI_PROFILE_DEFAULT.to_owned());
        let raw_profiles = match object.remove("profiles") {
            Some(JsonValue::Array(items)) => items,
            _ => Vec::new(),
        };
        let mut profiles: Vec<AnkiProfile> = raw_profiles
            .into_iter()
            .map(AnkiProfile::from_json)
            .collect();
        if profiles.is_empty() {
            profiles.push(AnkiProfile::default());
        }
        sort_profiles_slice(&mut profiles);
        let set_profile = if profiles.iter().any(|p| p.name == requested) {
            requested
        } else {
            profiles[0].name.clone()
        };
        let extra = object.into_iter().collect();
        Ok(Self {
            enabled,
            set_profile,
            profiles,
            extra,
        })
    }

    pub(crate) fn to_json_value(&self) -> JsonValue {
        let mut obj = Map::new();
        for (k, v) in &self.extra {
            obj.insert(k.clone(), v.clone());
        }
        obj.insert("enabled".to_owned(), JsonValue::Bool(self.enabled));
        obj.insert(
            "setProfile".to_owned(),
            JsonValue::String(self.set_profile.clone()),
        );
        obj.insert(
            "profiles".to_owned(),
            JsonValue::Array(
                self.profiles
                    .iter()
                    .map(AnkiProfile::to_json_value)
                    .collect(),
            ),
        );
        JsonValue::Object(obj)
    }

    pub(crate) fn to_pretty_json(&self) -> Result<String, String> {
        serde_json::to_string_pretty(&self.to_json_value())
            .map_err(|error| format!("could not encode Anki profiles: {error}"))
    }

    pub(crate) fn profile_names(&self) -> Vec<String> {
        self.profiles.iter().map(|p| p.name.clone()).collect()
    }

    pub(crate) fn current_profile_name(&self) -> String {
        self.profiles
            .iter()
            .find(|p| p.name == self.set_profile)
            .or_else(|| self.profiles.first())
            .map(|p| p.name.clone())
            .unwrap_or_else(|| ANKI_PROFILE_DEFAULT.to_owned())
    }

    pub(crate) fn current_profile(&self) -> Option<&AnkiProfile> {
        self.profiles
            .iter()
            .find(|p| p.name == self.set_profile)
            .or_else(|| self.profiles.first())
    }

    pub(crate) fn current_profile_mut(&mut self) -> Option<&mut AnkiProfile> {
        let idx = self
            .profiles
            .iter()
            .position(|p| p.name == self.set_profile)
            .or_else(|| (!self.profiles.is_empty()).then_some(0))?;
        Some(&mut self.profiles[idx])
    }

    pub(crate) fn note_config(&self, section: &str) -> Option<AnkiNoteConfig> {
        let profile = self.current_profile()?;
        let note = profile.note_section(section)?;
        Some(AnkiNoteConfig {
            enabled: self.enabled,
            endpoint: profile.endpoint(),
            deck: note.deck.clone(),
            model: note.model.clone(),
            field_templates: note.field_templates_map(),
            tags: profile.tags.clone(),
            duplicate_policy: profile.duplicate as i32,
            screenshot_type: profile.screenshot as i32,
            audio_pad_start: profile.audio_pad_start,
            audio_pad_end: profile.audio_pad_end,
            audio_normalize: profile.audio_normalize,
            audio_db: profile.audio_db,
            newline_replacer: profile.newline_replace.clone(),
        })
    }

    pub(crate) fn apply_note_preset(&mut self, preset_name: &str) -> Result<usize, String> {
        let preset = AnkiNotePreset::from_name(preset_name)
            .ok_or_else(|| format!("unknown Anki note preset {preset_name:?}"))?;
        let preset_rows = anki_note_preset_fields(preset);
        let profile = self
            .current_profile_mut()
            .ok_or_else(|| "Anki term profile is missing".to_owned())?;
        let updated_rows = apply_preset_to_fields(&profile.term.fields, &preset_rows);
        let count = updated_rows.len();
        profile.term.fields = updated_rows;
        Ok(count)
    }

    pub(crate) fn set_model_fields(
        &mut self,
        section: &str,
        fields: &[String],
    ) -> Result<(), String> {
        if section != "term" && section != "kanji" {
            return Err("unknown Anki note type".into());
        }
        let profile = self
            .current_profile_mut()
            .ok_or_else(|| "Anki note profile is missing".to_owned())?;
        let note = profile
            .note_section_mut(section)
            .ok_or_else(|| "Anki note profile is missing".to_owned())?;
        let existing = note.field_templates_map();
        note.fields = fields
            .iter()
            .map(|name| {
                let marker = existing
                    .get(name)
                    .cloned()
                    .unwrap_or_else(|| default_anki_field_marker(section, name));
                (name.clone(), marker)
            })
            .collect();
        Ok(())
    }

    pub(crate) fn edit_setting(&mut self, key: &str, value: &str) -> Result<(), String> {
        let setting = key
            .strip_prefix("anki/")
            .ok_or_else(|| format!("invalid Anki setting key: {key}"))?;
        match setting {
            "enabled" => {
                self.enabled = parse_bool(value)?;
                return Ok(());
            }
            "profile" => {
                if !self.profiles.iter().any(|p| p.name == value) {
                    return Err(format!("Anki profile {value:?} does not exist"));
                }
                self.set_profile = value.to_owned();
                return Ok(());
            }
            _ => {}
        }

        let relative = setting
            .strip_prefix("profile/")
            .ok_or_else(|| format!("unknown Anki setting: {key}"))?;
        let current_name = self.current_profile_name();
        if relative == "name" {
            let name = value.trim();
            if name.is_empty() {
                return Err("Anki profile name cannot be empty".to_owned());
            }
            if self
                .profiles
                .iter()
                .any(|profile| profile.name == name && name != current_name)
            {
                return Err(format!("Anki profile {name:?} already exists"));
            }
            let profile = self
                .profiles
                .iter_mut()
                .find(|profile| profile.name == current_name)
                .ok_or_else(|| format!("Anki profile {current_name:?} does not exist"))?;
            profile.name = name.to_owned();
            self.set_profile = name.to_owned();
            sort_profiles_slice(&mut self.profiles);
            return Ok(());
        }

        let profile = self
            .profiles
            .iter_mut()
            .find(|profile| profile.name == current_name)
            .ok_or_else(|| format!("Anki profile {current_name:?} does not exist"))?;
        edit_anki_profile_field(profile, relative, value)
    }

    pub(crate) fn add_profile(&mut self, requested_name: &str) -> Result<(), String> {
        let name = requested_name.trim();
        if name.is_empty() {
            return Err("Anki profile name cannot be empty".to_owned());
        }
        if self.profiles.iter().any(|profile| profile.name == name) {
            return Err(format!("Anki profile {name:?} already exists"));
        }
        let mut new_profile = self.current_profile().cloned().unwrap_or_default();
        new_profile.name = name.to_owned();
        self.profiles.push(new_profile);
        sort_profiles_slice(&mut self.profiles);
        self.set_profile = name.to_owned();
        Ok(())
    }

    pub(crate) fn remove_profile(&mut self, name: &str) -> Result<(), String> {
        let index = self
            .profiles
            .iter()
            .position(|profile| profile.name == name)
            .ok_or_else(|| format!("Anki profile {name:?} does not exist"))?;
        self.profiles.remove(index);
        if self.profiles.is_empty() {
            self.profiles.push(AnkiProfile::default());
        }
        sort_profiles_slice(&mut self.profiles);
        let selected_still_exists = self.profiles.iter().any(|p| p.name == self.set_profile);
        if self.set_profile == name || !selected_still_exists {
            self.set_profile = self.profiles[0].name.clone();
        }
        Ok(())
    }

    pub(crate) fn reset_current_profile_defaults(&mut self) {
        if let Some(profile) = self.current_profile_mut() {
            profile.reset_defaults();
        }
    }

    pub(crate) fn settings_groups(&self) -> Vec<SettingsGroup> {
        let default_profile;
        let profile = match self.current_profile() {
            Some(p) => p,
            None => {
                default_profile = AnkiProfile::default();
                &default_profile
            }
        };
        let use_api_key = profile.use_api_key;
        vec![
            group(
                SettingsCategory::Anki,
                "General",
                "",
                vec![
                    switch_field("anki/enabled", "Enable Anki integration", self.enabled),
                    text_field("anki/profile/name", "Profile name", profile.name.clone()),
                ],
            ),
            group(
                SettingsCategory::Anki,
                "AnkiConnect",
                "",
                vec![
                    text_field("anki/profile/host", "Hostname", profile.host.clone()),
                    text_field("anki/profile/port", "Port", profile.port.clone()),
                    switch_field("anki/profile/use-api-key", "Use API key", use_api_key),
                    conditional_text_field(
                        "anki/profile/api-key",
                        "API key",
                        profile.api_key.clone(),
                        use_api_key,
                        use_api_key,
                    ),
                ],
            ),
            group(
                SettingsCategory::Anki,
                "Advanced",
                "",
                vec![
                    choice_field(
                        "anki/profile/duplicate",
                        "Duplicate policy",
                        match profile.duplicate {
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
                        profile.newline_replace.clone(),
                    ),
                    choice_field(
                        "anki/profile/screenshot",
                        "Screenshot file type",
                        match profile.screenshot {
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
                        profile.audio_pad_start.to_string(),
                    ),
                    text_field(
                        "anki/profile/audio-pad-end",
                        "Audio padding end duration (seconds)",
                        profile.audio_pad_end.to_string(),
                    ),
                    switch_field(
                        "anki/profile/audio-normalize",
                        "Audio media normalization",
                        profile.audio_normalize,
                    ),
                    text_field(
                        "anki/profile/audio-db",
                        "Audio normalization dB",
                        profile.audio_db.to_string(),
                    ),
                    text_field(
                        "anki/profile/ex-glos",
                        "Exclude dictionaries from glossary (comma-separated)",
                        profile.ex_glos.join(", "),
                    ),
                ],
            ),
            group(
                SettingsCategory::Anki,
                "Cards",
                "",
                vec![text_field(
                    "anki/profile/tags",
                    "Tags (space or comma-separated)",
                    profile.tags.join(" "),
                )],
            ),
            group(
                SettingsCategory::Anki,
                "Term Notes",
                "",
                unpack_note_fields("term", &profile.term),
            ),
            group(
                SettingsCategory::Anki,
                "Kanji Notes",
                "",
                unpack_note_fields("kanji", &profile.kanji),
            ),
        ]
    }
}

fn unpack_note_fields(section: &str, note: &AnkiNoteSection) -> Vec<SettingsField> {
    let mut fields = vec![
        text_field(
            &format!("anki/profile/{section}/deck"),
            "Deck",
            note.deck.clone(),
        ),
        text_field(
            &format!("anki/profile/{section}/model"),
            "Model",
            note.model.clone(),
        ),
    ];
    for (name, template) in &note.fields {
        fields.push(text_field(
            &format!("anki/profile/{section}/field/{name}"),
            name,
            template.clone(),
        ));
    }
    fields
}

/// Validates and normalizes an AnkiConnect hostname.
///
/// Bare hostnames without a scheme default to `http://`. Any explicit scheme
/// (`<scheme>://...`) other than `http://` or `https://` is rejected.
pub(crate) fn validate_and_normalize_anki_hostname(host: &str) -> Result<String, String> {
    let trimmed = host.trim();
    if trimmed.is_empty() {
        return Err("Anki hostname cannot be empty".to_owned());
    }
    if let Some((scheme, rest)) = trimmed.split_once("://") {
        if rest.trim().is_empty() {
            return Err("Anki hostname cannot be empty".to_owned());
        }
        if scheme.eq_ignore_ascii_case("http") {
            return Ok(format!("http://{rest}"));
        }
        if scheme.eq_ignore_ascii_case("https") {
            return Ok(format!("https://{rest}"));
        }
        return Err("Anki hostname must use http:// or https://".to_owned());
    }
    Ok(format!("http://{trimmed}"))
}

fn edit_anki_profile_field(
    profile: &mut AnkiProfile,
    relative: &str,
    value: &str,
) -> Result<(), String> {
    let (section, key) = relative
        .split_once('/')
        .map_or(("profile", relative), |(section, key)| (section, key));
    if section == "term" || section == "kanji" {
        let note = profile
            .note_section_mut(section)
            .ok_or_else(|| format!("Anki {section} settings are malformed"))?;
        if let Some(field_name) = key.strip_prefix("field/") {
            note.set_field(field_name, value.to_owned());
            return Ok(());
        }
        match key {
            "deck" => {
                note.deck = value.to_owned();
                return Ok(());
            }
            "model" => {
                note.model = value.to_owned();
                return Ok(());
            }
            "fields" => {
                let fields: JsonValue = serde_json::from_str(value)
                    .map_err(|error| format!("invalid {section} field templates JSON: {error}"))?;
                if !fields.is_array() && !fields.is_object() {
                    return Err(format!(
                        "{section} field templates must be a JSON array or object"
                    ));
                }
                note.fields = parse_anki_fields_value(&fields);
                return Ok(());
            }
            _ => return Err(format!("unknown Anki {section} setting {key:?}")),
        }
    }

    match key {
        "host" => {
            profile.host = validate_and_normalize_anki_hostname(value)?;
        }
        "port" => {
            let port = value
                .trim()
                .parse::<u16>()
                .map_err(|_| "Anki port must be between 0 and 65535".to_owned())?;
            profile.port = port.to_string();
        }
        "use-api-key" => profile.use_api_key = parse_bool(value)?,
        "audio-normalize" => profile.audio_normalize = parse_bool(value)?,
        "show-tag" => profile.show_tag = parse_bool(value)?,
        "api-key" => profile.api_key = value.to_owned(),
        "newline-replace" => profile.newline_replace = value.to_owned(),
        "duplicate" | "screenshot" => {
            let index = match (key, value) {
                ("duplicate", "None") => 0,
                ("duplicate", "Allowed in different decks") => 1,
                ("duplicate", "Allowed in same deck") => 2,
                ("screenshot", "JPEG") => 0,
                ("screenshot", "PNG") => 1,
                ("screenshot", "WebP") => 2,
                _ => value
                    .parse::<i64>()
                    .map_err(|_| format!("invalid Anki {key} option: {value:?}"))?,
            };
            if !(0..=2).contains(&index) {
                return Err(format!("Anki {key} must be 0, 1, or 2"));
            }
            if key == "duplicate" {
                profile.duplicate = index;
            } else {
                profile.screenshot = index;
            }
        }
        "audio-pad-start" | "audio-pad-end" | "audio-db" => {
            let number = value
                .parse::<f64>()
                .ok()
                .filter(|number| number.is_finite())
                .ok_or_else(|| format!("Anki {key} must be a finite number"))?;
            match key {
                "audio-pad-start" => profile.audio_pad_start = number,
                "audio-pad-end" => profile.audio_pad_end = number,
                "audio-db" => profile.audio_db = number,
                _ => unreachable!(),
            }
        }
        "tags" => {
            profile.tags = value
                .split(|ch: char| ch.is_whitespace() || ch == ',')
                .filter(|tag| !tag.is_empty())
                .map(str::to_owned)
                .collect();
        }
        "ex-glos" => {
            profile.ex_glos = value
                .split(',')
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(str::to_owned)
                .collect();
        }
        _ => return Err(format!("unknown Anki profile setting {key:?}")),
    }
    Ok(())
}

fn sort_profiles_slice(profiles: &mut [AnkiProfile]) {
    profiles.sort_by(|left, right| left.name.encode_utf16().cmp(right.name.encode_utf16()));
}

fn parse_json_port(value: Option<&JsonValue>) -> Option<String> {
    match value? {
        JsonValue::String(s) => Some(s.clone()),
        JsonValue::Number(n) => n
            .as_u64()
            .and_then(|v| u16::try_from(v).ok())
            .map(|p| p.to_string()),
        _ => None,
    }
}

fn parse_anki_fields_value(fields: &JsonValue) -> Vec<(String, String)> {
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
                normalized.push((name.to_owned(), value.to_owned()));
            }
        }
        JsonValue::Object(fields) => {
            for (name, value) in fields {
                if let Some(value) = value.as_str() {
                    normalized.push((name.clone(), value.to_owned()));
                }
            }
        }
        _ => {}
    }
    normalized
}

fn str_field(object: &Map<String, JsonValue>, key: &str, default: &str) -> String {
    object
        .get(key)
        .and_then(JsonValue::as_str)
        .unwrap_or(default)
        .to_owned()
}

fn bool_field(object: &Map<String, JsonValue>, key: &str, default: bool) -> bool {
    object
        .get(key)
        .and_then(JsonValue::as_bool)
        .unwrap_or(default)
}

fn i64_field(object: &Map<String, JsonValue>, key: &str, default: i64) -> i64 {
    object
        .get(key)
        .and_then(JsonValue::as_i64)
        .unwrap_or(default)
}

fn f64_field(object: &Map<String, JsonValue>, key: &str, default: f64) -> f64 {
    object
        .get(key)
        .and_then(JsonValue::as_f64)
        .filter(|v| v.is_finite())
        .unwrap_or(default)
}

fn string_array_field(object: &Map<String, JsonValue>, key: &str) -> Option<Vec<String>> {
    object.get(key).and_then(JsonValue::as_array).map(|items| {
        items
            .iter()
            .filter_map(JsonValue::as_str)
            .map(str::to_owned)
            .collect()
    })
}

fn normalize_anki_field_name(name: &str) -> String {
    name.chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn apply_preset_to_fields(
    existing_rows: &[(String, String)],
    preset_rows: &[(&'static str, &'static str)],
) -> Vec<(String, String)> {
    if existing_rows.is_empty() {
        return preset_rows
            .iter()
            .map(|(name, marker)| ((*name).to_owned(), (*marker).to_owned()))
            .collect();
    }

    let lookup: HashMap<String, &'static str> = preset_rows
        .iter()
        .map(|(name, marker)| (normalize_anki_field_name(name), *marker))
        .collect();
    let mut matched = 0usize;
    let mapped: Vec<(String, String)> = existing_rows
        .iter()
        .map(|(name, current)| {
            let norm = normalize_anki_field_name(name);
            if let Some(marker) = lookup.get(&norm) {
                matched += 1;
                (name.clone(), (*marker).to_owned())
            } else {
                (name.clone(), current.clone())
            }
        })
        .collect();

    if matched > 0 {
        mapped
    } else {
        preset_rows
            .iter()
            .map(|(name, marker)| ((*name).to_owned(), (*marker).to_owned()))
            .collect()
    }
}

fn default_anki_field_marker(section: &str, field: &str) -> String {
    let normalized = normalize_anki_field_name(field);
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

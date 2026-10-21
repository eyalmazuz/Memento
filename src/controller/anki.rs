//! Anki controller connecting memento-anki and memento-net.

use memento_anki::tokenizer::{tokenize, Marker};
use memento_anki::note::{substitute_markers, MarkerResult};
use memento_net::anki::{AnkiClient, AnkiEndpoint};
use crate::controller::config::AnkiNoteConfig;
use std::collections::HashMap;

/// Duplicate behavior supported by the Memento Anki profile schema.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnkiDuplicatePolicy {
    None,
    DifferentDeck,
    SameDeck,
}

impl TryFrom<i32> for AnkiDuplicatePolicy {
    type Error = NoteBuildError;

    fn try_from(value: i32) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::None),
            1 => Ok(Self::DifferentDeck),
            2 => Ok(Self::SameDeck),
            _ => Err(NoteBuildError::InvalidDuplicatePolicy(value)),
        }
    }
}

impl From<AnkiDuplicatePolicy> for NoteOptions {
    fn from(policy: AnkiDuplicatePolicy) -> Self {
        match policy {
            AnkiDuplicatePolicy::None => Self::default(),
            AnkiDuplicatePolicy::DifferentDeck => Self {
                allow_duplicate: false,
                duplicate_scope: Some("deck".to_owned()),
            },
            AnkiDuplicatePolicy::SameDeck => Self {
                allow_duplicate: true,
                duplicate_scope: None,
            },
        }
    }
}

/// A profile note cannot be built when a field or its required source data is invalid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NoteBuildError {
    NoFieldTemplates,
    InvalidDuplicatePolicy(i32),
    MissingMediaCapture { field: String, marker: String },
    MissingMarkerData { field: String, marker: String },
    InvalidPayload(String),
}

impl std::fmt::Display for NoteBuildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoFieldTemplates => write!(f, "Anki profile has no field templates"),
            Self::InvalidDuplicatePolicy(value) => {
                write!(f, "unsupported Anki duplicate policy: {value}")
            }
            Self::MissingMediaCapture { field, marker } => write!(
                f,
                "Anki field {field:?} uses {{{marker}}}, but media capture failed"
            ),
            Self::MissingMarkerData { field, marker } => write!(
                f,
                "Anki field {field:?} uses {{{marker}}}, but the note context does not provide its source data"
            ),
            Self::InvalidPayload(error) => f.write_str(error),
        }
    }
}

impl std::error::Error for NoteBuildError {}

/// Structured representation of an AnkiConnect note creation payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotePayload {
    pub deck_name: String,
    pub model_name: String,
    pub fields: HashMap<String, String>,
    pub tags: Vec<String>,
    pub options: NoteOptions,
    pub audio: Vec<serde_json::Value>,
    pub picture: Vec<serde_json::Value>,
    pub video: Vec<serde_json::Value>,
}

/// Options controlling note creation in AnkiConnect.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct NoteOptions {
    pub allow_duplicate: bool,
    pub duplicate_scope: Option<String>,
}

impl NotePayload {
    pub fn new(deck_name: impl Into<String>, model_name: impl Into<String>) -> Self {
        Self {
            deck_name: deck_name.into(),
            model_name: model_name.into(),
            fields: HashMap::new(),
            tags: Vec::new(),
            options: NoteOptions::default(),
            audio: Vec::new(),
            picture: Vec::new(),
            video: Vec::new(),
        }
    }

    pub fn with_field(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.fields.insert(name.into(), value.into());
        self
    }

    pub fn with_tag(mut self, tag: impl Into<String>) -> Self {
        self.tags.push(tag.into());
        self
    }

    pub fn with_options(mut self, allow_duplicate: bool, duplicate_scope: Option<String>) -> Self {
        self.options = NoteOptions {
            allow_duplicate,
            duplicate_scope,
        };
        self
    }

    pub fn validate(&self) -> Result<(), String> {
        validate_note_payload(self)
    }

    /// Build the AnkiConnect note without serializing and reparsing JSON.
    pub fn to_value(&self) -> serde_json::Value {
        let options = match &self.options.duplicate_scope {
            Some(scope) => serde_json::json!({ "duplicateScope": scope }),
            None => serde_json::json!({ "allowDuplicate": self.options.allow_duplicate }),
        };
        let mut note = serde_json::json!({
            "deckName": self.deck_name,
            "modelName": self.model_name,
            "fields": self.fields,
            "tags": self.tags,
            "options": options,
        });
        if !self.audio.is_empty() {
            note["audio"] = serde_json::Value::Array(self.audio.clone());
        }
        if !self.picture.is_empty() {
            note["picture"] = serde_json::Value::Array(self.picture.clone());
        }
        if !self.video.is_empty() {
            note["video"] = serde_json::Value::Array(self.video.clone());
        }
        note
    }

    pub fn to_json_string(&self) -> String {
        self.to_value().to_string()
    }

    /// Builds a `memento_anki::json::JsonObject` metadata representation.
    pub fn to_anki_metadata(&self) -> memento_anki::json::JsonObject {
        let deck: Vec<u16> = self.deck_name.encode_utf16().collect();
        let model: Vec<u16> = self.model_name.encode_utf16().collect();
        let tags: Vec<Vec<u16>> = self.tags.iter().map(|t| t.encode_utf16().collect()).collect();
        let mut fields_obj = memento_anki::json::JsonObject::new();
        for (k, v) in &self.fields {
            fields_obj.insert(
                k.encode_utf16().collect(),
                memento_anki::json::JsonValue::String(v.encode_utf16().collect()),
            );
        }
        let policy = if self.options.duplicate_scope.as_deref() == Some("deck") {
            memento_anki::note::DuplicatePolicy::DifferentDeck
        } else if self.options.allow_duplicate {
            memento_anki::note::DuplicatePolicy::SameDeck
        } else {
            memento_anki::note::DuplicatePolicy::None
        };
        memento_anki::note::build_note_metadata(
            deck,
            model,
            tags,
            memento_anki::json::JsonValue::Object(fields_obj),
            policy,
        )
    }
}


/// Validates note creation payload according to AnkiConnect expectations.
pub fn validate_note_payload(payload: &NotePayload) -> Result<(), String> {
    if payload.deck_name.trim().is_empty() {
        return Err("deckName cannot be empty".to_string());
    }
    if payload.model_name.trim().is_empty() {
        return Err("modelName cannot be empty".to_string());
    }
    if payload.fields.is_empty() {
        return Err("fields must not be empty".to_string());
    }
    for (k, _) in &payload.fields {
        if k.trim().is_empty() {
            return Err("field name cannot be empty".to_string());
        }
    }
    let all_empty = payload.fields.values().all(|v| v.trim().is_empty());
    if all_empty && payload.audio.is_empty() && payload.picture.is_empty() && payload.video.is_empty() {
        return Err("at least one field must have non-empty content".to_string());
    }
    match payload.options.duplicate_scope.as_deref() {
        Some(scope) if scope != "deck" => {
            return Err(format!("unsupported duplicateScope: {scope}"));
        }
        Some(_) if payload.options.allow_duplicate => {
            return Err("duplicateScope and allowDuplicate cannot both be set".to_string());
        }
        _ => {}
    }
    Ok(())
}

/// Validates an AnkiConnect note payload JSON object.
pub fn validate_json_payload(json: &str) -> Result<(), String> {
    let value: serde_json::Value =
        serde_json::from_str(json).map_err(|error| format!("invalid JSON: {error}"))?;
    let object = value
        .as_object()
        .ok_or_else(|| "Payload must be a JSON object".to_string())?;

    for name in ["deckName", "modelName"] {
        let value = object
            .get(name)
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| format!("Missing or invalid field: {name}"))?;
        if value.trim().is_empty() {
            return Err(format!("{name} cannot be empty"));
        }
    }

    let fields = object
        .get("fields")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| "Missing or invalid field: fields".to_string())?;
    if fields.is_empty() {
        return Err("fields must not be empty".to_string());
    }
    let mut has_content = false;
    for (name, value) in fields {
        let value = value
            .as_str()
            .ok_or_else(|| "fields must map non-empty names to strings".to_string())?;
        if name.trim().is_empty() {
            return Err("fields must map non-empty names to strings".to_string());
        }
        has_content |= !value.trim().is_empty();
    }
    let has_media = object.get("audio").and_then(serde_json::Value::as_array).is_some_and(|items| !items.is_empty())
        || object.get("picture").and_then(serde_json::Value::as_array).is_some_and(|items| !items.is_empty());
    if !has_content && !has_media {
        return Err("at least one field must have non-empty content".to_string());
    }
    Ok(())
}

// ── Cloze Formatting Helpers ────────────────────────────────────────────────

/// Formats a cloze deletion string: `prefix + {{c{index}::body}} + suffix`.
/// If index is 0, defaults to 1.
pub fn format_cloze(prefix: &str, body: &str, suffix: &str, index: usize) -> String {
    let idx = if index == 0 { 1 } else { index };
    format!("{prefix}{{{{c{idx}::{body}}}}}{suffix}")
}

/// Formats a cloze deletion with an optional hint: `prefix + {{c{index}::body::hint}} + suffix`.
pub fn format_cloze_with_hint(
    prefix: &str,
    body: &str,
    suffix: &str,
    hint: &str,
    index: usize,
) -> String {
    let idx = if index == 0 { 1 } else { index };
    if hint.is_empty() {
        format!("{prefix}{{{{c{idx}::{body}}}}}{suffix}")
    } else {
        format!("{prefix}{{{{c{idx}::{body}::{hint}}}}}{suffix}")
    }
}

/// Finds `target` inside `sentence` and produces a cloze deletion string.
/// If `target` is not found, wraps the whole sentence in cloze deletion.
pub fn create_cloze_sentence(sentence: &str, target: &str, index: usize) -> String {
    let idx = if index == 0 { 1 } else { index };
    if target.is_empty() {
        return sentence.to_string();
    }
    if let Some(pos) = sentence.find(target) {
        let prefix = &sentence[..pos];
        let body = &sentence[pos..pos + target.len()];
        let suffix = &sentence[pos + target.len()..];
        format_cloze(prefix, body, suffix, idx)
    } else {
        format!("{{{{c{idx}::{sentence}}}}}")
    }
}

/// Splits a sentence around a target expression into `(prefix, body, suffix)`.
pub fn split_cloze<'a>(sentence: &'a str, target: &'a str) -> (&'a str, &'a str, &'a str) {
    if target.is_empty() {
        return (sentence, "", "");
    }
    if let Some(pos) = sentence.find(target) {
        let prefix = &sentence[..pos];
        let body = &sentence[pos..pos + target.len()];
        let suffix = &sentence[pos + target.len()..];
        (prefix, body, suffix)
    } else {
        ("", target, sentence)
    }
}

// ── Glossary Formatting Helpers ─────────────────────────────────────────────

/// Rendered glossary output in full, brief, and compact styles.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GlossaryOutput {
    pub glossary: String,
    pub glossary_brief: String,
    pub glossary_compact: String,
}

/// A checked dictionary entry belonging to the selected term.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GlossaryDefinition {
    pub dictionary_id: i64,
    pub dictionary_name: String,
    pub glossary: String,
    pub tags: Vec<String>,
    pub notes: String,
    pub example_ja: String,
    pub example_en: String,
}

fn escape_html(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
        .replace('"', "&quot;").replace('\'', "&#39;")
}

fn definition_html(definition: &GlossaryDefinition) -> String {
    let mut value = escape_html(&definition.glossary).replace('\n', "<br>");
    for (label, text) in [
        ("Example", &definition.example_ja),
        ("Translation", &definition.example_en),
        ("Notes", &definition.notes),
    ] {
        if !text.is_empty() {
            value.push_str(&format!("<br><small>{label}: {}</small>", escape_html(text).replace('\n', "<br>")));
        }
    }
    value
}

fn format_context_glossary(ctx: &NoteFieldsContext) -> GlossaryOutput {
    format_context_glossary_for_dictionary(ctx, None)
}

fn format_context_glossary_for_dictionary(ctx: &NoteFieldsContext, dictionary_id: Option<i64>) -> GlossaryOutput {
    if ctx.glossary_definitions.is_empty() {
        let items: Vec<&str> = ctx.glossary_items.iter().map(String::as_str).collect();
        let tags: Vec<&str> = ctx.tags.iter().map(String::as_str).collect();
        return format_glossary(&items, &ctx.dictionary_name, &tags);
    }
    let selected: Vec<_> = ctx.glossary_definitions.iter()
        .filter(|definition| dictionary_id.is_none_or(|id| definition.dictionary_id == id))
        .collect();
    let names: Vec<Vec<u16>> = selected.iter().map(|d| d.dictionary_name.encode_utf16().collect()).collect();
    let items: Vec<Vec<u16>> = selected.iter().map(|d| definition_html(d).encode_utf16().collect()).collect();
    let item_refs: Vec<Vec<&[u16]>> = items.iter().map(|item| vec![item.as_slice()]).collect();
    let tag_units: Vec<Vec<Vec<u16>>> = selected.iter()
        .map(|d| d.tags.iter().map(|tag| tag.encode_utf16().collect()).collect()).collect();
    let tag_refs: Vec<Vec<&[u16]>> = tag_units.iter().map(|tags| tags.iter().map(Vec::as_slice).collect()).collect();
    let definitions: Vec<memento_anki::note::TermGlossaryDef<'_>> = selected.iter().enumerate().map(|(i, definition)| {
        memento_anki::note::TermGlossaryDef {
            dict_name: &names[i],
            dict_id: definition.dictionary_id,
            tags: &tag_refs[i],
            items: &item_refs[i],
            has_styles: false,
            stylesheet: &[],
        }
    }).collect();
    let out = memento_anki::note::format_term_glossary(&definitions);
    GlossaryOutput {
        glossary: String::from_utf16_lossy(&out.glossary),
        glossary_brief: String::from_utf16_lossy(&out.glossary_brief),
        glossary_compact: String::from_utf16_lossy(&out.glossary_compact),
    }
}

/// Formats glossary definition items into Memento's HTML representation.
pub fn format_glossary(items: &[&str], dict_name: &str, tags: &[&str]) -> GlossaryOutput {
    let dict_name_u16: Vec<u16> = dict_name.encode_utf16().collect();
    let tags_u16: Vec<Vec<u16>> = tags.iter().map(|t| t.encode_utf16().collect()).collect();
    let tags_refs: Vec<&[u16]> = tags_u16.iter().map(|v| v.as_slice()).collect();
    let items_u16: Vec<Vec<u16>> = items.iter().map(|it| it.encode_utf16().collect()).collect();
    let items_refs: Vec<&[u16]> = items_u16.iter().map(|v| v.as_slice()).collect();

    let def = memento_anki::note::TermGlossaryDef {
        dict_name: &dict_name_u16,
        dict_id: 1,
        tags: &tags_refs,
        items: &items_refs,
        has_styles: false,
        stylesheet: &[],
    };
    let out = memento_anki::note::format_term_glossary(&[def]);
    GlossaryOutput {
        glossary: String::from_utf16_lossy(&out.glossary),
        glossary_brief: String::from_utf16_lossy(&out.glossary_brief),
        glossary_compact: String::from_utf16_lossy(&out.glossary_compact),
    }
}

/// Convenience helper to format glossary definitions as an HTML string.
pub fn format_glossary_html(items: &[String], dict_name: &str) -> String {
    let refs: Vec<&str> = items.iter().map(|s| s.as_str()).collect();
    format_glossary(&refs, dict_name, &[]).glossary
}

// ── Note Fields Context & Mapping ───────────────────────────────────────────

/// Source data for populating Memento Anki note fields.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NoteFieldsContext {
    pub expression: String,
    pub reading: String,
    pub sentence: String,
    pub context: String,
    pub selection: String,
    pub secondary_subtitle: String,
    pub title: String,
    pub cloze_prefix: String,
    pub cloze_body: String,
    pub cloze_suffix: String,
    pub glossary_items: Vec<String>,
    pub glossary_definitions: Vec<GlossaryDefinition>,
    pub furigana: String,
    pub furigana_plain: String,
    pub pitch: String,
    pub pitch_positions: String,
    pub pitch_categories: String,
    pub frequencies: String,
    pub frequency_harmonic_rank: String,
    pub frequency_average_rank: String,
    pub character: String,
    pub onyomi: String,
    pub kunyomi: String,
    pub stroke_count: String,
    pub dictionary_name: String,
    /// Term/kanji tags used by `{tags}` and glossary formatting. Anki note
    /// tags come from the selected profile, not this context field.
    pub tags: Vec<String>,
    pub cloze_index: usize,
    /// AnkiConnect attachments populated before submitting a profile note.
    pub audio_attachments: Vec<serde_json::Value>,
    pub picture_attachments: Vec<serde_json::Value>,
    pub video_attachments: Vec<serde_json::Value>,
}

impl NoteFieldsContext {
    pub fn new(
        expression: impl Into<String>,
        reading: impl Into<String>,
        sentence: impl Into<String>,
    ) -> Self {
        let expression = expression.into();
        let reading = reading.into();
        let sentence = sentence.into();
        let (p, b, s) = split_cloze(&sentence, &expression);
        let cloze_prefix = p.to_string();
        let cloze_body = b.to_string();
        let cloze_suffix = s.to_string();
        let (furigana, furigana_plain) = if !reading.is_empty() && reading != expression {
            let surface: Vec<u16> = expression.encode_utf16().collect();
            let pronunciation: Vec<u16> = reading.encode_utf16().collect();
            let pair = memento_anki::note::FuriganaPair { surface: &surface, reading: &pronunciation };
            (
                String::from_utf16_lossy(&memento_anki::note::format_furigana(&[pair], false)),
                String::from_utf16_lossy(&memento_anki::note::format_furigana(&[pair], true)),
            )
        } else { (expression.clone(), expression.clone()) };
        Self {
            reading,
            context: sentence.clone(),
            selection: expression.clone(),
            secondary_subtitle: String::new(),
            title: String::new(),
            sentence,
            cloze_prefix,
            cloze_body,
            cloze_suffix,
            expression,
            glossary_items: Vec::new(),
            glossary_definitions: Vec::new(),
            furigana,
            furigana_plain,
            pitch: String::new(),
            pitch_positions: String::new(),
            pitch_categories: String::new(),
            frequencies: String::new(),
            frequency_harmonic_rank: String::new(),
            frequency_average_rank: String::new(),
            character: String::new(),
            onyomi: String::new(),
            kunyomi: String::new(),
            stroke_count: String::new(),
            dictionary_name: String::new(),
            tags: Vec::new(),
            cloze_index: 1,
            audio_attachments: Vec::new(),
            picture_attachments: Vec::new(),
            video_attachments: Vec::new(),
        }
    }

    pub fn with_glossary_item(mut self, item: impl Into<String>) -> Self {
        self.glossary_items.push(item.into());
        self
    }

    pub fn with_dictionary(mut self, dict: impl Into<String>) -> Self {
        self.dictionary_name = dict.into();
        self
    }

    pub fn with_tag(mut self, tag: impl Into<String>) -> Self {
        self.tags.push(tag.into());
        self
    }

    pub fn with_cloze_index(mut self, index: usize) -> Self {
        self.cloze_index = index;
        self
    }
}

fn marker_is(marker: &Marker, name: &str) -> bool {
    marker.name.iter().copied().eq(name.encode_utf16())
}

fn marker_result(value: &str) -> MarkerResult {
    MarkerResult {
        handled: true,
        media: false,
        text: value.encode_utf16().collect(),
    }
}

fn glossary_dictionary_filter(marker: &Marker) -> Option<i64> {
    marker.args.iter().find_map(|(name, value)| {
        name.iter().copied().eq("dict".encode_utf16())
            .then(|| String::from_utf16_lossy(value).parse::<i64>().ok())
            .flatten()
    })
}
fn common_marker_value<'a>(
    marker: &Marker,
    ctx: &'a NoteFieldsContext,
) -> Option<&'a str> {
    if marker_is(marker, "cloze-prefix") {
        Some(ctx.cloze_prefix.as_str())
    } else if marker_is(marker, "cloze-body") {
        Some(ctx.cloze_body.as_str())
    } else if marker_is(marker, "cloze-suffix") {
        Some(ctx.cloze_suffix.as_str())
    } else if marker_is(marker, "subtitle") || marker_is(marker, "sentence") {
        Some(ctx.sentence.as_str())
    } else if marker_is(marker, "context") {
        Some(ctx.context.as_str())
    } else if marker_is(marker, "selection") {
        Some(ctx.selection.as_str())
    } else if marker_is(marker, "subtitle-2") || marker_is(marker, "sentence-2") || marker_is(marker, "context-2") {
        Some(ctx.secondary_subtitle.as_str())
    } else if marker_is(marker, "title") {
        Some(ctx.title.as_str())
    } else {
        None
    }
}

fn specific_marker_value<'a>(
    marker: &Marker,
    ctx: &'a NoteFieldsContext,
    glossary: &'a GlossaryOutput,
    tags: &'a str,
) -> Option<&'a str> {
    if marker_is(marker, "expression") {
        Some(ctx.expression.as_str())
    } else if marker_is(marker, "reading") {
        Some(ctx.reading.as_str())
    } else if marker_is(marker, "glossary") {
        Some(glossary.glossary.as_str())
    } else if marker_is(marker, "glossary-brief") {
        Some(glossary.glossary_brief.as_str())
    } else if marker_is(marker, "glossary-compact") {
        Some(glossary.glossary_compact.as_str())
    } else if marker_is(marker, "tags") {
        Some(tags)
    } else if marker_is(marker, "tags-brief") {
        Some(tags)
    } else if marker_is(marker, "furigana") {
        Some(ctx.furigana.as_str())
    } else if marker_is(marker, "furigana-plain") {
        Some(ctx.furigana_plain.as_str())
    } else if marker_is(marker, "pitch") || marker_is(marker, "pitch-accents") || marker_is(marker, "pitch-accent") {
        Some(ctx.pitch.as_str())
    } else if marker_is(marker, "pitch-positions") || marker_is(marker, "pitch-position") || marker_is(marker, "pitch-accent-positions") {
        Some(ctx.pitch_positions.as_str())
    } else if marker_is(marker, "pitch-categories") || marker_is(marker, "pitch-accent-categories") {
        Some(ctx.pitch_categories.as_str())
    } else if marker_is(marker, "frequencies") {
        Some(ctx.frequencies.as_str())
    } else if marker_is(marker, "frequency-harmonic-rank") {
        Some(ctx.frequency_harmonic_rank.as_str())
    } else if marker_is(marker, "frequency-average-rank") {
        Some(ctx.frequency_average_rank.as_str())
    } else if marker_is(marker, "character") {
        Some(ctx.character.as_str())
    } else if marker_is(marker, "onyomi") {
        Some(ctx.onyomi.as_str())
    } else if marker_is(marker, "kunyomi") {
        Some(ctx.kunyomi.as_str())
    } else if marker_is(marker, "stroke-count") {
        Some(ctx.stroke_count.as_str())
    } else if FALLBACK_MARKERS.iter().any(|name| marker_is(marker, name)) {
        // Media attachments are inserted by AnkiConnect. Other optional
        // metadata markers remain empty when their source is unavailable,
        // while alternative markers can still supply a value.
        Some("")
    } else {
        None
    }
}

const FALLBACK_MARKERS: &[&str] = &[
    "audio", "audio-media", "audio-context", "sentence-audio", "screenshot", "screenshot-video", "image", "video",
    "clipboard", "frequency-average-occurrence",
    "frequency-harmonic-occurrence", "pitch-graphs", "pitch-graph", "pitch-accent-graphs",
];


/// Maps Anki field templates through the same token parser and ordered
/// alternative-marker substitution used by the C++ note builder.
pub fn map_note_fields(
    field_templates: &HashMap<String, String>,
    ctx: &NoteFieldsContext,
) -> HashMap<String, String> {
    let glossary_out = format_context_glossary(ctx);
    let tags_joined = ctx.tags.join(" ");
    map_note_fields_with_data(field_templates, ctx, &glossary_out, &tags_joined)
}

fn map_note_fields_with_data(
    field_templates: &HashMap<String, String>,
    ctx: &NoteFieldsContext,
    glossary: &GlossaryOutput,
    tags: &str,
) -> HashMap<String, String> {
    let mut result = HashMap::with_capacity(field_templates.len());
    for (field_name, template) in field_templates {
        let input: Vec<u16> = template.encode_utf16().collect();
        let output = substitute_markers(
            &input,
            |marker| {
                common_marker_value(marker, ctx)
                    .map(marker_result)
                    .unwrap_or_default()
            },
            |marker| {
                if (marker_is(marker, "glossary") || marker_is(marker, "glossary-brief") || marker_is(marker, "glossary-compact"))
                    && glossary_dictionary_filter(marker).is_some() {
                    let filtered = format_context_glossary_for_dictionary(ctx, glossary_dictionary_filter(marker));
                    let value = if marker_is(marker, "glossary-brief") {
                        &filtered.glossary_brief
                    } else if marker_is(marker, "glossary-compact") {
                        &filtered.glossary_compact
                    } else {
                        &filtered.glossary
                    };
                    return marker_result(value);
                }
                specific_marker_value(marker, ctx, glossary, tags)
                    .map(marker_result)
                    .unwrap_or_default()
            },
        );
        result.insert(field_name.clone(), String::from_utf16_lossy(&output));
    }
    result
}

fn marker_requires_unavailable_data(
    field: &str,
    marker: &Marker,
) -> Option<NoteBuildError> {
    const KNOWN_MARKERS: &[&str] = &[
        "audio",
        "audio-media",
        "audio-context",
        "sentence-audio",
        "clipboard",
        "cloze-prefix",
        "cloze-body",
        "cloze-suffix",
        "context",
        "context-2",
        "frequency-average-occurrence",
        "frequency-average-rank",
        "frequency-harmonic-occurrence",
        "frequency-harmonic-rank",
        "frequencies",
        "glossary",
        "glossary-brief",
        "glossary-compact",
        "screenshot",
        "screenshot-video",
        "image",
        "sentence",
        "sentence-2",
        "subtitle",
        "subtitle-2",
        "tags",
        "tags-brief",
        "title",
        "video",
        "expression",
        "furigana",
        "furigana-plain",
        "pitch",
        "pitch-accent",
        "pitch-accents",
        "pitch-categories",
        "pitch-accent-categories",
        "pitch-graphs",
        "pitch-graph",
        "pitch-accent-graphs",
        "pitch-positions",
        "pitch-position",
        "pitch-accent-positions",
        "reading",
        "selection",
        "character",
        "kunyomi",
        "onyomi",
        "stroke-count",
    ];

    if !KNOWN_MARKERS.iter().any(|name| marker_is(marker, name)) {
        return Some(NoteBuildError::MissingMarkerData {
            field: field.to_owned(),
            marker: String::from_utf16_lossy(&marker.name),
        });
    }
    None
}

/// Profile markers for which this app currently has no source data. The
/// corresponding fields are left empty when a note is created.
pub fn unavailable_profile_markers(profile: &AnkiNoteConfig) -> Vec<String> {
    let mut names = Vec::new();
    for template in profile.field_templates.values() {
        for token in tokenize(&template.encode_utf16().collect::<Vec<_>>()) {
            for marker in token.markers {
                if FALLBACK_MARKERS.iter().any(|name| marker_is(&marker, name))
                    && !["audio", "audio-media", "audio-context", "sentence-audio", "screenshot", "screenshot-video", "image", "video"]
                        .iter().any(|name| marker_is(&marker, name)) {
                    let name = String::from_utf16_lossy(&marker.name);
                    if !names.contains(&name) {
                        names.push(name);
                    }
                }
            }
        }
    }
    names.sort();
    names
}
fn validate_template_markers(
    field: &str,
    template: &str,
    ctx: &NoteFieldsContext,
    glossary: &GlossaryOutput,
    tags: &str,
) -> Result<(), NoteBuildError> {
    let input: Vec<u16> = template.encode_utf16().collect();
    for token in tokenize(&input) {
        for marker in &token.markers {
            if let Some(value) = common_marker_value(marker, ctx) {
                if !value.is_empty() {
                    break;
                }
                continue;
            }
            if let Some(error) = marker_requires_unavailable_data(field, marker) {
                return Err(error);
            }
            if let Some(value) = specific_marker_value(marker, ctx, glossary, tags) {
                if !value.is_empty() {
                    break;
                }
            }
        }
    }
    Ok(())
}


/// Default note fields mapping when no explicit template is supplied.
pub fn default_note_fields(ctx: &NoteFieldsContext) -> HashMap<String, String> {
    let cloze_str = format_cloze(
        &ctx.cloze_prefix,
        &ctx.cloze_body,
        &ctx.cloze_suffix,
        ctx.cloze_index,
    );
    let glossary_out = format_context_glossary(ctx);

    let mut fields = HashMap::new();
    fields.insert("Expression".to_string(), ctx.expression.clone());
    fields.insert("Reading".to_string(), ctx.reading.clone());
    fields.insert("Glossary".to_string(), glossary_out.glossary);
    fields.insert("Sentence".to_string(), cloze_str);
    fields.insert("ClozePrefix".to_string(), ctx.cloze_prefix.clone());
    fields.insert("ClozeBody".to_string(), ctx.cloze_body.clone());
    fields.insert("ClozeSuffix".to_string(), ctx.cloze_suffix.clone());
    fields
}

/// Builds a complete `NotePayload` from context and optional field templates.
pub fn build_note_payload(
    deck: &str,
    model: &str,
    ctx: &NoteFieldsContext,
    templates: Option<&HashMap<String, String>>,
) -> NotePayload {
    let fields = match templates {
        Some(tmpl) => map_note_fields(tmpl, ctx),
        None => default_note_fields(ctx),
    };
    NotePayload {
        deck_name: deck.to_string(),
        model_name: model.to_string(),
        fields,
        tags: ctx.tags.clone(),
        options: NoteOptions::default(),
        audio: ctx.audio_attachments.clone(),
        picture: ctx.picture_attachments.clone(),
        video: ctx.video_attachments.clone(),
    }
}

/// Builds a real note from the configured term profile. Unlike the legacy
/// context-only convenience builder, this never invents fields when profile
/// templates are absent.
pub fn build_note_payload_from_profile(
    profile: &AnkiNoteConfig,
    ctx: &NoteFieldsContext,
) -> Result<NotePayload, NoteBuildError> {
    if profile.field_templates.is_empty() {
        return Err(NoteBuildError::NoFieldTemplates);
    }
    let policy = AnkiDuplicatePolicy::try_from(profile.duplicate_policy)?;
    let glossary = format_context_glossary(ctx);
    let tags = ctx.tags.join(" ");
    for (field, template) in &profile.field_templates {
        validate_template_markers(field, template, ctx, &glossary, &tags)?;
    }

    let mut text_context = ctx.clone();
    for value in [
        &mut text_context.sentence,
        &mut text_context.context,
        &mut text_context.selection,
        &mut text_context.secondary_subtitle,
        &mut text_context.title,
        &mut text_context.cloze_prefix,
        &mut text_context.cloze_body,
        &mut text_context.cloze_suffix,
    ] {
        *value = value.replace('\n', &profile.newline_replacer);
    }

    let payload = NotePayload {
        deck_name: profile.deck.clone(),
        model_name: profile.model.clone(),
        fields: map_note_fields_with_data(&profile.field_templates, &text_context, &glossary, &tags),
        tags: profile.tags.clone(),
        options: policy.into(),
        audio: ctx.audio_attachments.clone(),
        picture: ctx.picture_attachments.clone(),
        video: ctx.video_attachments.clone(),
    };
    payload
        .validate()
        .map_err(NoteBuildError::InvalidPayload)?;
    Ok(payload)
}

// ── AnkiController ──────────────────────────────────────────────────────────

pub struct AnkiController {
    client: AnkiClient,
}

impl Default for AnkiController {
    fn default() -> Self {
        Self {
            client: AnkiClient::new(AnkiEndpoint::default()),
        }
    }
}

impl AnkiController {
    pub fn new(endpoint: AnkiEndpoint) -> Self {
        Self {
            client: AnkiClient::new(endpoint),
        }
    }

    pub fn client(&self) -> &AnkiClient {
        &self.client
    }

    pub fn endpoint(&self) -> &AnkiEndpoint {
        self.client.endpoint()
    }

    /// Send a configured note to AnkiConnect and return its assigned note id.
    /// Validation happens before the network request so an incomplete profile
    /// cannot create a misleading empty card.
    pub fn add_note(&self, payload: &NotePayload) -> Result<i64, String> {
        payload.validate()?;
        self.client
            .add_note(payload.to_value())
            .map_err(|error| error.to_string())
    }
    /// Builds a note from the current profile's configured field templates.
    pub fn create_note_payload_from_profile(
        &self,
        profile: &AnkiNoteConfig,
        ctx: &NoteFieldsContext,
    ) -> Result<NotePayload, NoteBuildError> {
        build_note_payload_from_profile(profile, ctx)
    }

    /// Builds and sends a profile-backed note without substituting synthetic
    /// fields when the profile templates or source data are unavailable.
    pub fn add_note_from_profile(
        &self,
        profile: &AnkiNoteConfig,
        ctx: &NoteFieldsContext,
    ) -> Result<i64, String> {
        let payload = self
            .create_note_payload_from_profile(profile, ctx)
            .map_err(|error| error.to_string())?;
        self.client
            .add_note(payload.to_value())
            .map_err(|error| error.to_string())
    }


    /// Retrieves AnkiConnect server version.
    pub fn version(&self) -> Result<i32, String> {
        self.client.version().map_err(|e| e.to_string())
    }


    /// Tests connection and returns server version.
    pub fn test_connection(&self) -> Result<i32, String> {
        self.version()
    }

    /// Tests connection against the minimum supported AnkiConnect version.
    pub fn check_connection(&self) -> Result<bool, String> {
        self.client
            .test_connection()
            .map(|_| true)
            .map_err(|e| e.to_string())
    }

    /// Validates an Anki note payload.
    pub fn validate_note_payload(&self, payload: &NotePayload) -> Result<(), String> {
        validate_note_payload(payload)
    }

    /// Maps field templates to note fields using context.
    pub fn map_note_fields(
        &self,
        templates: &HashMap<String, String>,
        ctx: &NoteFieldsContext,
    ) -> HashMap<String, String> {
        map_note_fields(templates, ctx)
    }

    /// Builds a note payload ready for AnkiConnect dispatch.
    pub fn create_note_payload(
        &self,
        deck: &str,
        model: &str,
        ctx: &NoteFieldsContext,
    ) -> NotePayload {
        build_note_payload(deck, model, ctx, None)
    }

    /// Builds a note payload with custom field templates.
    pub fn create_note_payload_with_templates(
        &self,
        deck: &str,
        model: &str,
        ctx: &NoteFieldsContext,
        templates: &HashMap<String, String>,
    ) -> NotePayload {
        build_note_payload(deck, model, ctx, Some(templates))
    }

    /// Formats a cloze expression from components.
    pub fn format_cloze(&self, prefix: &str, body: &str, suffix: &str, index: usize) -> String {
        format_cloze(prefix, body, suffix, index)
    }

    /// Formats glossary HTML from item definitions.
    pub fn format_glossary(&self, items: &[&str], dict_name: &str, tags: &[&str]) -> GlossaryOutput {
        format_glossary(items, dict_name, tags)
    }
}


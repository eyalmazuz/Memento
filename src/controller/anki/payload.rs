//! AnkiConnect note payload construction, validation, serialization, and update merging.

use std::collections::{HashMap, HashSet};

use memento_anki::note::TermGlossaryVariants;
use memento_net::anki::AnkiNoteInfo;

use crate::controller::config::AnkiNoteConfig;

use super::cloze::format_cloze;
use super::context::NoteFieldsContext;
use super::css_glossary::format_context_glossary_selected;
use super::markers::{
    map_note_fields, map_note_fields_with_data, validate_template_markers, CommonTextFields,
    RenderedTemplateData,
};
use super::query::select_primary_key_field;

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

    /// Consumes the payload and builds the AnkiConnect note JSON without cloning
    /// multi-MB `audio`, `picture`, or `video` attachments.
    pub fn into_value(self) -> serde_json::Value {
        let mut options_map = serde_json::Map::with_capacity(1);
        match self.options.duplicate_scope {
            Some(scope) => {
                options_map.insert("duplicateScope".to_owned(), serde_json::Value::String(scope));
            }
            None => {
                options_map.insert(
                    "allowDuplicate".to_owned(),
                    serde_json::Value::Bool(self.options.allow_duplicate),
                );
            }
        }
        let mut fields_map = serde_json::Map::with_capacity(self.fields.len());
        for (k, v) in self.fields {
            fields_map.insert(k, serde_json::Value::String(v));
        }
        let tags = serde_json::Value::Array(
            self.tags
                .into_iter()
                .map(serde_json::Value::String)
                .collect(),
        );
        let mut note = serde_json::Map::with_capacity(8);
        note.insert(
            "deckName".to_owned(),
            serde_json::Value::String(self.deck_name),
        );
        note.insert(
            "modelName".to_owned(),
            serde_json::Value::String(self.model_name),
        );
        note.insert("fields".to_owned(), serde_json::Value::Object(fields_map));
        note.insert("tags".to_owned(), tags);
        note.insert("options".to_owned(), serde_json::Value::Object(options_map));
        if !self.audio.is_empty() {
            note.insert("audio".to_owned(), serde_json::Value::Array(self.audio));
        }
        if !self.picture.is_empty() {
            note.insert("picture".to_owned(), serde_json::Value::Array(self.picture));
        }
        if !self.video.is_empty() {
            note.insert("video".to_owned(), serde_json::Value::Array(self.video));
        }
        serde_json::Value::Object(note)
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

    /// Serializes directly to JSON by borrowing all fields and media attachments
    /// without cloning multi-MB `audio`/`picture`/`video` payloads.
    pub fn to_json_string(&self) -> String {
        let mut buf = Vec::with_capacity(256);
        let write_res = (|| -> serde_json::Result<()> {
            buf.extend_from_slice(br#"{"deckName":"#);
            serde_json::to_writer(&mut buf, &self.deck_name)?;
            buf.extend_from_slice(br#","fields":"#);
            serde_json::to_writer(&mut buf, &self.fields)?;
            buf.extend_from_slice(br#","modelName":"#);
            serde_json::to_writer(&mut buf, &self.model_name)?;
            buf.extend_from_slice(br#","options":"#);
            match &self.options.duplicate_scope {
                Some(scope) => {
                    buf.extend_from_slice(br#"{"duplicateScope":"#);
                    serde_json::to_writer(&mut buf, scope)?;
                    buf.push(b'}');
                }
                None => {
                    buf.extend_from_slice(br#"{"allowDuplicate":"#);
                    serde_json::to_writer(&mut buf, &self.options.allow_duplicate)?;
                    buf.push(b'}');
                }
            }
            buf.extend_from_slice(br#","tags":"#);
            serde_json::to_writer(&mut buf, &self.tags)?;
            if !self.audio.is_empty() {
                buf.extend_from_slice(br#","audio":"#);
                serde_json::to_writer(&mut buf, &self.audio)?;
            }
            if !self.picture.is_empty() {
                buf.extend_from_slice(br#","picture":"#);
                serde_json::to_writer(&mut buf, &self.picture)?;
            }
            if !self.video.is_empty() {
                buf.extend_from_slice(br#","video":"#);
                serde_json::to_writer(&mut buf, &self.video)?;
            }
            buf.push(b'}');
            Ok(())
        })();
        match write_res {
            Ok(()) => String::from_utf8(buf).unwrap_or_else(|_| self.to_value().to_string()),
            Err(_) => self.to_value().to_string(),
        }
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

fn validate_note_payload_parts(
    payload: &NotePayload,
    has_media: bool,
) -> Result<(), String> {
    if payload.deck_name.trim().is_empty() {
        return Err("deckName cannot be empty".to_string());
    }
    if payload.model_name.trim().is_empty() {
        return Err("modelName cannot be empty".to_string());
    }
    if payload.fields.is_empty() {
        return Err("fields must not be empty".to_string());
    }
    for k in payload.fields.keys() {
        if k.trim().is_empty() {
            return Err("field name cannot be empty".to_string());
        }
    }
    let all_empty = payload.fields.values().all(|v| v.trim().is_empty());
    if all_empty && !has_media {
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

/// Validates note creation payload according to AnkiConnect expectations.
pub fn validate_note_payload(payload: &NotePayload) -> Result<(), String> {
    let has_media =
        !payload.audio.is_empty() || !payload.picture.is_empty() || !payload.video.is_empty();
    validate_note_payload_parts(payload, has_media)
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
    let has_media = ["audio", "picture", "video"].iter().any(|key| {
        object
            .get(*key)
            .and_then(serde_json::Value::as_array)
            .is_some_and(|items| !items.is_empty())
    });
    if !has_content && !has_media {
        return Err("at least one field must have non-empty content".to_string());
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
    let glossary_out = format_context_glossary_selected(ctx, None, TermGlossaryVariants::FULL);

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

pub(crate) fn build_note_payload_from_profile_with_data(
    profile: &AnkiNoteConfig,
    ctx: &NoteFieldsContext,
    rendered: &RenderedTemplateData,
    require_captured_media_when_all_fields_empty: bool,
    include_media_attachments: bool,
) -> Result<NotePayload, NoteBuildError> {
    if profile.field_templates.is_empty() {
        return Err(NoteBuildError::NoFieldTemplates);
    }
    let policy = AnkiDuplicatePolicy::try_from(profile.duplicate_policy)?;
    let raw_text_fields = CommonTextFields::raw(ctx, &rendered.clipboard);

    for (field, template) in &profile.field_templates {
        validate_template_markers(field, template, ctx, &raw_text_fields, rendered)?;
    }

    let transformed_fields =
        CommonTextFields::transformed(ctx, &rendered.clipboard, &profile.newline_replacer);
    let fields = map_note_fields_with_data(
        &profile.field_templates,
        ctx,
        &transformed_fields,
        rendered,
    );

    let (audio, picture, video) = if include_media_attachments {
        (
            ctx.audio_attachments.clone(),
            ctx.picture_attachments.clone(),
            ctx.video_attachments.clone(),
        )
    } else {
        (Vec::new(), Vec::new(), Vec::new())
    };

    let payload = NotePayload {
        deck_name: profile.deck.clone(),
        model_name: profile.model.clone(),
        fields,
        tags: profile.tags.clone(),
        options: policy.into(),
        audio,
        picture,
        video,
    };

    if require_captured_media_when_all_fields_empty {
        let has_media = !ctx.audio_attachments.is_empty()
            || !ctx.picture_attachments.is_empty()
            || !ctx.video_attachments.is_empty();
        validate_note_payload_parts(&payload, has_media)
            .map_err(NoteBuildError::InvalidPayload)?;
    } else {
        if payload.deck_name.trim().is_empty() {
            return Err(NoteBuildError::InvalidPayload(
                "deckName cannot be empty".to_string(),
            ));
        }
        if payload.model_name.trim().is_empty() {
            return Err(NoteBuildError::InvalidPayload(
                "modelName cannot be empty".to_string(),
            ));
        }
    }
    Ok(payload)
}

/// Builds a real note from the configured term/kanji profile. Unlike the legacy
/// context-only convenience builder, this never invents fields when profile
/// templates are absent.
pub fn build_note_payload_from_profile(
    profile: &AnkiNoteConfig,
    ctx: &NoteFieldsContext,
) -> Result<NotePayload, NoteBuildError> {
    let rendered = RenderedTemplateData::for_templates(&profile.field_templates, ctx);
    build_note_payload_from_profile_with_data(profile, ctx, &rendered, true, true)
}

/// Owned variant of [`build_note_payload_from_profile`] that moves multi-MB media
/// attachments out of `ctx` without cloning them.
pub fn build_note_payload_from_profile_owned(
    profile: &AnkiNoteConfig,
    mut ctx: NoteFieldsContext,
) -> Result<NotePayload, NoteBuildError> {
    let rendered = RenderedTemplateData::for_templates(&profile.field_templates, &ctx);
    let mut payload =
        build_note_payload_from_profile_with_data(profile, &ctx, &rendered, true, false)?;
    payload.audio = std::mem::take(&mut ctx.audio_attachments);
    payload.picture = std::mem::take(&mut ctx.picture_attachments);
    payload.video = std::mem::take(&mut ctx.video_attachments);
    Ok(payload)
}

/// Controls how fields and media are merged when updating an existing Anki note.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AnkiUpdateMode {
    /// Populate only fields that are currently empty (or whitespace-only) on the existing note.
    FillEmptyOnly,
    /// Practical Yomitan-style default: updates configured fields where the newly built value
    /// (or newly generated media attachment) is non-empty, never wipes an existing populated
    /// field with an empty value, and preserves the first/key sort field if unchanged.
    #[default]
    OverwriteNonEmpty,
    /// Fills empty fields, or appends new non-empty text (separated by the profile newline
    /// replacer / `<br>`) and new media attachments to existing fields, while preserving the
    /// first/key sort field unchanged.
    AppendOrFill,
}

/// Result of merging newly built fields, media attachments, and tags against an existing Anki note.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergedNoteUpdate {
    pub fields: serde_json::Map<String, serde_json::Value>,
    pub audio: Vec<serde_json::Value>,
    pub picture: Vec<serde_json::Value>,
    pub video: Vec<serde_json::Value>,
    pub new_tags: Vec<String>,
}

impl MergedNoteUpdate {
    /// Returns `true` when at least one field or media attachment needs `updateNoteFields`.
    pub fn has_field_or_media_changes(&self) -> bool {
        !self.fields.is_empty()
            || !self.audio.is_empty()
            || !self.picture.is_empty()
            || !self.video.is_empty()
    }
}

fn filter_update_media_attachments(
    attachments: &[serde_json::Value],
    existing: &AnkiNoteInfo,
    mode: AnkiUpdateMode,
    media_target_fields: &mut HashSet<String>,
) -> Vec<serde_json::Value> {
    let mut kept = Vec::new();
    for item in attachments {
        let Some(obj) = item.as_object() else {
            continue;
        };
        let Some(raw_fields) = obj.get("fields").and_then(serde_json::Value::as_array) else {
            continue;
        };
        let mut filtered_fields = Vec::new();
        for field_val in raw_fields {
            let Some(field_name) = field_val.as_str() else {
                continue;
            };
            if !existing.fields.is_empty() && !existing.fields.contains_key(field_name) {
                continue;
            }
            let existing_empty = existing
                .fields
                .get(field_name)
                .is_none_or(|value| value.trim().is_empty());
            if mode == AnkiUpdateMode::FillEmptyOnly && !existing_empty {
                continue;
            }
            media_target_fields.insert(field_name.to_owned());
            filtered_fields.push(serde_json::Value::String(field_name.to_owned()));
        }
        if !filtered_fields.is_empty() {
            let mut updated = obj.clone();
            updated.insert("fields".to_owned(), serde_json::Value::Array(filtered_fields));
            kept.push(serde_json::Value::Object(updated));
        }
    }
    kept
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn merge_note_update_with_attachments(
    existing: &AnkiNoteInfo,
    payload_fields: &HashMap<String, String>,
    payload_tags: &[String],
    payload_audio: &[serde_json::Value],
    payload_picture: &[serde_json::Value],
    payload_video: &[serde_json::Value],
    profile: &AnkiNoteConfig,
    mode: AnkiUpdateMode,
) -> MergedNoteUpdate {
    let key_field = existing
        .sort_field
        .clone()
        .or_else(|| select_primary_key_field(&profile.field_templates));

    let mut media_target_fields = HashSet::new();
    let audio = filter_update_media_attachments(
        payload_audio,
        existing,
        mode,
        &mut media_target_fields,
    );
    let picture = filter_update_media_attachments(
        payload_picture,
        existing,
        mode,
        &mut media_target_fields,
    );
    let video = filter_update_media_attachments(
        payload_video,
        existing,
        mode,
        &mut media_target_fields,
    );

    let mut sorted_fields: Vec<(&String, &String)> = payload_fields.iter().collect();
    sorted_fields.sort_by_key(|(name, _)| *name);

    let sep = if profile.newline_replacer.is_empty() {
        "<br>"
    } else {
        profile.newline_replacer.as_str()
    };

    let mut fields = serde_json::Map::new();
    for (field_name, new_val) in sorted_fields {
        if !existing.fields.is_empty() && !existing.fields.contains_key(field_name) {
            continue;
        }
        let existing_val = existing
            .fields
            .get(field_name)
            .map(String::as_str)
            .unwrap_or("");
        let existing_empty = existing_val.trim().is_empty();
        let new_empty = new_val.trim().is_empty();
        let is_key = key_field.as_deref() == Some(field_name.as_str());
        let has_new_media = media_target_fields.contains(field_name);

        match mode {
            AnkiUpdateMode::FillEmptyOnly => {
                if existing_empty && !new_empty {
                    fields.insert(
                        field_name.clone(),
                        serde_json::Value::String(new_val.clone()),
                    );
                }
            }
            AnkiUpdateMode::OverwriteNonEmpty => {
                if has_new_media {
                    if !new_empty {
                        fields.insert(
                            field_name.clone(),
                            serde_json::Value::String(new_val.clone()),
                        );
                    } else if !existing_empty {
                        // Clear existing media-only field before AnkiConnect appends the
                        // newly generated media attachment so old media is cleanly replaced.
                        fields.insert(
                            field_name.clone(),
                            serde_json::Value::String(String::new()),
                        );
                    }
                } else if !new_empty && new_val != existing_val {
                    // Never wipes existing fields with empty values; skips unchanged fields
                    // (including the first/key sort field) to avoid redundant re-indexing.
                    fields.insert(
                        field_name.clone(),
                        serde_json::Value::String(new_val.clone()),
                    );
                }
            }
            AnkiUpdateMode::AppendOrFill => {
                if !new_empty {
                    if existing_empty {
                        fields.insert(
                            field_name.clone(),
                            serde_json::Value::String(new_val.clone()),
                        );
                    } else if !is_key
                        && existing_val != new_val
                        && !existing_val.contains(new_val.as_str())
                    {
                        fields.insert(
                            field_name.clone(),
                            serde_json::Value::String(format!("{existing_val}{sep}{new_val}")),
                        );
                    }
                }
            }
        }
    }

    let existing_tags_lower: HashSet<String> = existing
        .tags
        .iter()
        .flat_map(|tag| tag.split_whitespace())
        .filter(|tag| !tag.is_empty())
        .map(|tag| tag.to_ascii_lowercase())
        .collect();
    let mut seen_new_tags = HashSet::new();
    let mut new_tags = Vec::new();
    for raw_tag in payload_tags {
        for tag in raw_tag.split_whitespace() {
            if tag.is_empty() {
                continue;
            }
            let lower = tag.to_ascii_lowercase();
            if !existing_tags_lower.contains(&lower) && seen_new_tags.insert(lower) {
                new_tags.push(tag.to_owned());
            }
        }
    }

    MergedNoteUpdate {
        fields,
        audio,
        picture,
        video,
        new_tags,
    }
}

/// Computes the merged field diff, media attachments, and missing tags for updating `existing`
/// with `payload` under `mode`.
pub fn merge_note_update(
    existing: &AnkiNoteInfo,
    payload: &NotePayload,
    profile: &AnkiNoteConfig,
    mode: AnkiUpdateMode,
) -> MergedNoteUpdate {
    merge_note_update_with_attachments(
        existing,
        &payload.fields,
        &payload.tags,
        &payload.audio,
        &payload.picture,
        &payload.video,
        profile,
        mode,
    )
}

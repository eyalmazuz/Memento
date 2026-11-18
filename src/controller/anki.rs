//! Anki controller connecting memento-anki and memento-net.

use crate::controller::audio::sanitize_anki_filename;
use crate::controller::config::AnkiNoteConfig;
use memento_anki::glossary::build_glossary;
use memento_anki::note::{
    build_frequencies_html, find_stroke_count, format_furigana, format_kanji_glossary,
    format_kanji_readings, format_pitch, format_pitch_categories, format_pitch_graphs,
    format_pitch_positions, format_tags, format_term_glossary, frequency_stats,
    is_kifuku_applicable, substitute_markers, FrequencyEntry, FuriganaPair, KanjiDefEntry,
    MarkerResult, PitchCategoryInput, PitchEntry, PitchGraphEntry, StrokeCountStat, TagEntry,
    TermGlossaryDef,
};
use memento_anki::tokenizer::{tokenize, Marker};
use memento_dict::searchcontroller::{self, TagSortItem};
use memento_dict::sqlite_db;
use memento_dict::yomidbbuilder;
use memento_net::anki::{ to_qjson, AnkiClient, AnkiEndpoint, MediaFilePayload };
use memento_subtitle::SubtitleEntry;
use memento_util::furiganautils::simplify_surface;
use memento_util::imageutils::generate_pitch_graph;
use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::path::{Path, PathBuf};

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
    for k in payload.fields.keys() {
        if k.trim().is_empty() {
            return Err("field name cannot be empty".to_string());
        }
    }
    let all_empty = payload.fields.values().all(|v| v.trim().is_empty());
    if all_empty
        && payload.audio.is_empty()
        && payload.picture.is_empty()
        && payload.video.is_empty()
    {
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

// ── Cloze, Subtitle & Furigana Helpers ──────────────────────────────────────

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
    split_cloze_around(sentence, target, None)
}

/// Splits `text` around `target` into `(prefix, body, suffix)`, preferring the
/// occurrence at or closest after `hint_offset` (e.g. the active subtitle cue's
/// byte offset inside a multi-subtitle `{context}`).
pub fn split_cloze_around<'a>(
    text: &'a str,
    target: &'a str,
    hint_offset: Option<usize>,
) -> (&'a str, &'a str, &'a str) {
    if target.is_empty() {
        return (text, "", "");
    }
    let matched_pos = if let Some(raw_offset) = hint_offset {
        let mut offset = raw_offset.min(text.len());
        while offset > 0 && !text.is_char_boundary(offset) {
            offset -= 1;
        }
        text[offset..]
            .find(target)
            .map(|rel| offset + rel)
            .or_else(|| text[..offset].rfind(target))
    } else {
        text.find(target)
    };
    if let Some(pos) = matched_pos {
        let prefix = &text[..pos];
        let body = &text[pos..pos + target.len()];
        let suffix = &text[pos + target.len()..];
        (prefix, body, suffix)
    } else {
        ("", target, text)
    }
}

/// Computes `cloze_prefix`, `cloze_body`, and `cloze_suffix` for single- or
/// multi-subtitle (`selected_cues.len() > 1`) contexts without touching `ctx.selection`.
pub fn apply_subtitle_cloze(
    ctx: &mut NoteFieldsContext,
    selected_cues: &[String],
    active_pos_in_selected: Option<usize>,
    scanned_surface: &str,
    token_offset_in_sentence: Option<usize>,
) {
    if selected_cues.len() > 1 {
        let full_context = selected_cues.join("\n");
        let cue_index = active_pos_in_selected.or_else(|| {
            (!ctx.sentence.is_empty())
                .then(|| selected_cues.iter().position(|cue| cue == &ctx.sentence))
                .flatten()
        });
        let hint_offset = cue_index
            .map(|idx| {
                let cue_start: usize = selected_cues[..idx].iter().map(|cue| cue.len() + 1).sum();
                if let Some(tok_off) = token_offset_in_sentence {
                    if selected_cues[idx] == ctx.sentence && tok_off <= selected_cues[idx].len() {
                        return cue_start + tok_off;
                    }
                }
                cue_start
            })
            .or_else(|| {
                (!ctx.sentence.is_empty())
                    .then(|| full_context.find(&ctx.sentence))
                    .flatten()
            });
        let target = if !scanned_surface.is_empty() && full_context.contains(scanned_surface) {
            scanned_surface
        } else {
            ctx.expression.as_str()
        };
        let (prefix, body, suffix) = split_cloze_around(&full_context, target, hint_offset);
        ctx.cloze_prefix = prefix.to_owned();
        ctx.cloze_body = body.to_owned();
        ctx.cloze_suffix = suffix.to_owned();
        ctx.context = full_context;
    } else {
        if let Some(single) = selected_cues.first() {
            ctx.context = single.clone();
        }
        let target = if !scanned_surface.is_empty() && ctx.sentence.contains(scanned_surface) {
            scanned_surface
        } else {
            ctx.expression.as_str()
        };
        let (prefix, body, suffix) =
            split_cloze_around(&ctx.sentence, target, token_offset_in_sentence);
        ctx.cloze_prefix = prefix.to_owned();
        ctx.cloze_body = body.to_owned();
        ctx.cloze_suffix = suffix.to_owned();
    }
}

/// Computes `{context-2}` from secondary subtitle entries overlapping `[start, end]`,
/// falling back to `fallback` (`{subtitle-2}`) when no secondary cue overlaps.
pub fn secondary_context_for_range(
    secondary_entries: &[SubtitleEntry],
    start: f64,
    end: f64,
    fallback: &str,
) -> String {
    if start.is_finite() && end.is_finite() && end > start {
        let lines: Vec<String> = secondary_entries
            .iter()
            .filter(|cue| cue.start < end && cue.end > start)
            .map(|cue| String::from_utf16_lossy(&cue.text))
            .filter(|text| !text.is_empty())
            .collect();
        if !lines.is_empty() {
            return lines.join("\n");
        }
    }
    fallback.to_owned()
}

fn is_pure_kana(text: &str) -> bool {
    !text.is_empty()
        && text.chars().all(|ch| {
            matches!(
                ch,
                '\u{3040}'..='\u{309F}'
                    | '\u{30A0}'..='\u{30FF}'
                    | '\u{31F0}'..='\u{31FF}'
                    | '\u{FF65}'..='\u{FF9F}'
            )
        })
}

/// Formats `{furigana}` (`<ruby>...</ruby>`) and `{furigana-plain}` (`漢字[かんじ]`),
/// simplifying okurigana via `simplify_surface` and returning plain kana when there is no kanji.
pub fn format_expression_furigana(expression: &str, reading: &str) -> (String, String) {
    if reading.is_empty() || reading == expression || is_pure_kana(expression) {
        return (expression.to_owned(), expression.to_owned());
    }
    let surface: Vec<u16> = expression.encode_utf16().collect();
    let pronunciation: Vec<u16> = reading.encode_utf16().collect();
    let spans = simplify_surface(&surface, &pronunciation);
    let pairs: Vec<FuriganaPair<'_>> = spans
        .iter()
        .map(|span| FuriganaPair {
            surface: &surface[span.surface_start as usize..span.surface_end as usize],
            reading: &pronunciation[span.reading_start as usize..span.reading_end as usize],
        })
        .collect();
    (
        String::from_utf16_lossy(&format_furigana(&pairs, false)),
        String::from_utf16_lossy(&format_furigana(&pairs, true)),
    )
}

/// Reads the current system clipboard text, gracefully returning `""` on
/// headless/Wayland/X11 failure.
pub fn read_clipboard_text() -> String {
    use copypasta::ClipboardProvider;
    std::panic::catch_unwind(|| {
        copypasta::ClipboardContext::new()
            .and_then(|mut ctx| ctx.get_contents())
            .unwrap_or_default()
    })
    .unwrap_or_default()
}

// ── Glossary & Scoped CSS Formatting Helpers ────────────────────────────────

/// Rendered glossary output in full, brief, and compact styles.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GlossaryOutput {
    pub glossary: String,
    pub glossary_brief: String,
    pub glossary_compact: String,
}

/// Structured tag metadata with optional `tag_bank` category and description notes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NoteTag {
    pub name: String,
    pub category: String,
    pub notes: String,
}

/// A checked dictionary entry belonging to the selected term or kanji.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GlossaryDefinition {
    pub dictionary_id: i64,
    pub dictionary_name: String,
    pub glossary: String,
    pub tags: Vec<String>,
    pub notes: String,
    pub example_ja: String,
    pub example_en: String,
    /// Pre-rendered HTML items produced by `memento_anki::glossary::build_glossary`.
    pub rendered_items: Vec<String>,
    /// Individual kanji meanings when building a kanji `{glossary}` card.
    pub kanji_meanings: Vec<String>,
    /// Scoped dictionary CSS loaded from `styles.css`.
    pub stylesheet: String,
    /// Dictionary media files (`<img src="...">`) extracted by `build_glossary`.
    pub media_files: Vec<MediaFilePayload>,
}

fn escape_html(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn escape_html_attr(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn escape_css_string(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\a "),
            '\r' => {}
            _ => out.push(ch),
        }
    }
    out
}

fn strip_css_comments(css: &str) -> String {
    let bytes = css.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    let mut in_quote: Option<u8> = None;
    while i < bytes.len() {
        if let Some(q) = in_quote {
            if bytes[i] == b'\\' && i + 1 < bytes.len() {
                out.push(bytes[i]);
                out.push(bytes[i + 1]);
                i += 2;
                continue;
            }
            if bytes[i] == q {
                in_quote = None;
            }
            out.push(bytes[i]);
            i += 1;
        } else if bytes[i] == b'"' || bytes[i] == b'\'' {
            in_quote = Some(bytes[i]);
            out.push(bytes[i]);
            i += 1;
        } else if i + 1 < bytes.len() && bytes[i] == b'/' && bytes[i + 1] == b'*' {
            i += 2;
            while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                i += 1;
            }
            i = (i + 2).min(bytes.len());
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn find_matching_css_brace(css: &str, open_idx: usize) -> Option<usize> {
    let bytes = css.as_bytes();
    let mut depth = 0usize;
    let mut i = open_idx;
    let mut in_quote: Option<u8> = None;
    while i < bytes.len() {
        if let Some(q) = in_quote {
            if bytes[i] == b'\\' && i + 1 < bytes.len() {
                i += 2;
                continue;
            }
            if bytes[i] == q {
                in_quote = None;
            }
        } else {
            match bytes[i] {
                b'"' | b'\'' => in_quote = Some(bytes[i]),
                b'{' => depth += 1,
                b'}' => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        return Some(i);
                    }
                }
                _ => {}
            }
        }
        i += 1;
    }
    None
}

fn split_top_level_selectors(selector_group: &str) -> Vec<&str> {
    let bytes = selector_group.as_bytes();
    let mut parts = Vec::new();
    let mut start = 0usize;
    let mut paren_depth = 0usize;
    let mut bracket_depth = 0usize;
    let mut in_quote: Option<u8> = None;
    let mut i = 0usize;
    while i < bytes.len() {
        if let Some(q) = in_quote {
            if bytes[i] == b'\\' && i + 1 < bytes.len() {
                i += 2;
                continue;
            }
            if bytes[i] == q {
                in_quote = None;
            }
        } else {
            match bytes[i] {
                b'"' | b'\'' => in_quote = Some(bytes[i]),
                b'(' => paren_depth += 1,
                b')' => paren_depth = paren_depth.saturating_sub(1),
                b'[' => bracket_depth += 1,
                b']' => bracket_depth = bracket_depth.saturating_sub(1),
                b',' if paren_depth == 0 && bracket_depth == 0 => {
                    parts.push(&selector_group[start..i]);
                    start = i + 1;
                }
                _ => {}
            }
        }
        i += 1;
    }
    parts.push(&selector_group[start..]);
    parts
}

fn scope_css_rules(css: &str, scope_selector: &str) -> String {
    let bytes = css.as_bytes();
    let mut out = String::with_capacity(css.len() + 64);
    let mut i = 0usize;
    while i < bytes.len() {
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        if bytes[i] == b'@' {
            // Find next `;` or `{` outside quotes.
            let mut j = i;
            let mut in_quote: Option<u8> = None;
            while j < bytes.len() {
                if let Some(q) = in_quote {
                    if bytes[j] == b'\\' && j + 1 < bytes.len() {
                        j += 2;
                        continue;
                    }
                    if bytes[j] == q {
                        in_quote = None;
                    }
                } else if bytes[j] == b'"' || bytes[j] == b'\'' {
                    in_quote = Some(bytes[j]);
                } else if bytes[j] == b';' || bytes[j] == b'{' {
                    break;
                }
                j += 1;
            }
            if j >= bytes.len() {
                break;
            }
            let header = css[i..j].trim();
            if bytes[j] == b';' {
                // Skip @import and @charset in embedded <style> blocks.
                i = j + 1;
                continue;
            }
            let Some(close) = find_matching_css_brace(css, j) else {
                break;
            };
            let lower = header.to_ascii_lowercase();
            let inner = &css[j + 1..close];
            if lower.starts_with("@media")
                || lower.starts_with("@supports")
                || lower.starts_with("@container")
            {
                let scoped_inner = scope_css_rules(inner, scope_selector);
                if !scoped_inner.trim().is_empty() {
                    out.push_str(header);
                    out.push_str(" { ");
                    out.push_str(&scoped_inner);
                    out.push_str(" } ");
                }
            } else if lower.starts_with("@keyframes")
                || lower.starts_with("@-webkit-keyframes")
                || lower.starts_with("@font-face")
            {
                out.push_str(header);
                out.push_str(" {");
                out.push_str(inner);
                out.push_str("} ");
            }
            i = close + 1;
            continue;
        }

        // Find `{` starting declaration block outside quotes.
        let mut open = i;
        let mut in_quote: Option<u8> = None;
        while open < bytes.len() {
            if let Some(q) = in_quote {
                if bytes[open] == b'\\' && open + 1 < bytes.len() {
                    open += 2;
                    continue;
                }
                if bytes[open] == q {
                    in_quote = None;
                }
            } else if bytes[open] == b'"' || bytes[open] == b'\'' {
                in_quote = Some(bytes[open]);
            } else if bytes[open] == b'{' {
                break;
            }
            open += 1;
        }
        if open >= bytes.len() {
            break;
        }
        let Some(close) = find_matching_css_brace(css, open) else {
            break;
        };
        let selector_group = css[i..open].trim();
        let body = css[open + 1..close].trim();
        let mut scoped_selectors = Vec::new();
        for raw_sel in split_top_level_selectors(selector_group) {
            let sel = raw_sel.trim();
            if sel.is_empty() {
                continue;
            }
            if sel.contains("[data-dictionary") {
                scoped_selectors.push(sel.to_owned());
            } else if matches!(sel, ":root" | ":host" | "html" | "body") {
                scoped_selectors.push(scope_selector.to_owned());
            } else {
                scoped_selectors.push(format!("{scope_selector} {sel}"));
            }
        }
        if !scoped_selectors.is_empty() {
            out.push_str(&scoped_selectors.join(", "));
            out.push_str(" { ");
            out.push_str(body);
            out.push_str(" } ");
        }
        i = close + 1;
    }
    out.trim().to_owned()
}

/// Scopes a dictionary's `styles.css` rules to `[data-dictionary="<dict_name>"]`
/// and neutralizes any closing `</style` sequence.
pub fn scope_dictionary_css(css: &str, dict_name: &str) -> String {
    let stripped = strip_css_comments(css);
    if stripped.trim().is_empty() {
        return String::new();
    }
    let scope_selector = format!("[data-dictionary=\"{}\"]", escape_css_string(dict_name));
    let scoped = scope_css_rules(&stripped, &scope_selector);
    // Neutralize any case-insensitive `</style` sequence inside CSS content.
    let mut safe = String::with_capacity(scoped.len());
    let lower = scoped.to_ascii_lowercase();
    let mut start = 0usize;
    while let Some(rel) = lower[start..].find("</style") {
        let idx = start + rel;
        safe.push_str(&scoped[start..idx]);
        safe.push_str("<\\/style");
        start = idx + "</style".len();
    }
    safe.push_str(&scoped[start..]);
    safe
}

fn definition_html(definition: &GlossaryDefinition) -> String {
    let mut value = escape_html(&definition.glossary).replace('\n', "<br>");
    for (label, text) in [
        ("Example", &definition.example_ja),
        ("Translation", &definition.example_en),
        ("Notes", &definition.notes),
    ] {
        if !text.is_empty() {
            value.push_str(&format!(
                "<br><small>{label}: {}</small>",
                escape_html(text).replace('\n', "<br>")
            ));
        }
    }
    value
}

fn format_context_glossary(ctx: &NoteFieldsContext) -> GlossaryOutput {
    format_context_glossary_for_dictionary(ctx, None)
}

fn format_context_glossary_for_dictionary(
    ctx: &NoteFieldsContext,
    dictionary_id: Option<i64>,
) -> GlossaryOutput {
    let is_kanji = ctx.is_kanji || !ctx.character.is_empty();
    if is_kanji {
        if ctx.glossary_definitions.is_empty() {
            if ctx.glossary_items.is_empty() {
                return GlossaryOutput {
                    glossary: String::new(),
                    glossary_brief: String::new(),
                    glossary_compact: String::new(),
                };
            }
            let dict_u16: Vec<u16> = escape_html_attr(&ctx.dictionary_name).encode_utf16().collect();
            let tags_u16: Vec<Vec<u16>> = ctx
                .tags
                .iter()
                .map(|t| escape_html(t).encode_utf16().collect())
                .collect();
            let tag_refs: Vec<&[u16]> = tags_u16.iter().map(Vec::as_slice).collect();
            let items_u16: Vec<Vec<u16>> = ctx
                .glossary_items
                .iter()
                .map(|m| escape_html(m).encode_utf16().collect())
                .collect();
            let item_refs: Vec<&[u16]> = items_u16.iter().map(Vec::as_slice).collect();
            let html = String::from_utf16_lossy(&format_kanji_glossary(&[KanjiDefEntry {
                dictionary_name: &dict_u16,
                tags: &tag_refs,
                glossary: &item_refs,
            }]));
            return GlossaryOutput {
                glossary: html.clone(),
                glossary_brief: html.clone(),
                glossary_compact: html,
            };
        }
        let selected: Vec<_> = ctx
            .glossary_definitions
            .iter()
            .filter(|def| dictionary_id.is_none_or(|id| def.dictionary_id == id))
            .collect();
        let names: Vec<Vec<u16>> = selected
            .iter()
            .map(|d| escape_html_attr(&d.dictionary_name).encode_utf16().collect())
            .collect();
        let tag_units: Vec<Vec<Vec<u16>>> = selected
            .iter()
            .map(|d| {
                d.tags
                    .iter()
                    .map(|tag| escape_html(tag).encode_utf16().collect())
                    .collect()
            })
            .collect();
        let tag_refs: Vec<Vec<&[u16]>> = tag_units
            .iter()
            .map(|tags| tags.iter().map(Vec::as_slice).collect())
            .collect();
        let meaning_units: Vec<Vec<Vec<u16>>> = selected
            .iter()
            .map(|d| {
                if !d.kanji_meanings.is_empty() {
                    d.kanji_meanings
                        .iter()
                        .map(|m| escape_html(m).encode_utf16().collect())
                        .collect()
                } else {
                    d.glossary
                        .lines()
                        .map(str::trim)
                        .filter(|line| !line.is_empty())
                        .map(|m| escape_html(m).encode_utf16().collect())
                        .collect()
                }
            })
            .collect();
        let meaning_refs: Vec<Vec<&[u16]>> = meaning_units
            .iter()
            .map(|items| items.iter().map(Vec::as_slice).collect())
            .collect();
        let defs: Vec<KanjiDefEntry<'_>> = selected
            .iter()
            .enumerate()
            .map(|(i, _)| KanjiDefEntry {
                dictionary_name: &names[i],
                tags: &tag_refs[i],
                glossary: &meaning_refs[i],
            })
            .collect();
        let html = String::from_utf16_lossy(&format_kanji_glossary(&defs));
        return GlossaryOutput {
            glossary: html.clone(),
            glossary_brief: html.clone(),
            glossary_compact: html,
        };
    }

    if ctx.glossary_definitions.is_empty() {
        let items: Vec<&str> = ctx.glossary_items.iter().map(String::as_str).collect();
        let tags: Vec<&str> = ctx.tags.iter().map(String::as_str).collect();
        return format_glossary(&items, &ctx.dictionary_name, &tags);
    }
    let selected: Vec<_> = ctx
        .glossary_definitions
        .iter()
        .filter(|definition| dictionary_id.is_none_or(|id| definition.dictionary_id == id))
        .collect();
    let names: Vec<Vec<u16>> = selected
        .iter()
        .map(|d| escape_html_attr(&d.dictionary_name).encode_utf16().collect())
        .collect();
    let items: Vec<Vec<Vec<u16>>> = selected
        .iter()
        .map(|d| {
            if !d.rendered_items.is_empty() {
                d.rendered_items
                    .iter()
                    .map(|item| item.encode_utf16().collect())
                    .collect()
            } else {
                vec![definition_html(d).encode_utf16().collect()]
            }
        })
        .collect();
    let item_refs: Vec<Vec<&[u16]>> = items
        .iter()
        .map(|list| list.iter().map(Vec::as_slice).collect())
        .collect();
    let tag_units: Vec<Vec<Vec<u16>>> = selected
        .iter()
        .map(|d| d.tags.iter().map(|tag| tag.encode_utf16().collect()).collect())
        .collect();
    let tag_refs: Vec<Vec<&[u16]>> = tag_units
        .iter()
        .map(|tags| tags.iter().map(Vec::as_slice).collect())
        .collect();
    let style_units: Vec<Vec<u16>> = selected
        .iter()
        .map(|d| d.stylesheet.encode_utf16().collect())
        .collect();
    let definitions: Vec<TermGlossaryDef<'_>> = selected
        .iter()
        .enumerate()
        .map(|(i, definition)| TermGlossaryDef {
            dict_name: &names[i],
            dict_id: if definition.dictionary_id != 0 {
                definition.dictionary_id
            } else {
                (i as i64) + 1
            },
            tags: &tag_refs[i],
            items: &item_refs[i],
            has_styles: !style_units[i].is_empty(),
            stylesheet: &style_units[i],
        })
        .collect();
    let out = format_term_glossary(&definitions);
    GlossaryOutput {
        glossary: String::from_utf16_lossy(&out.glossary),
        glossary_brief: String::from_utf16_lossy(&out.glossary_brief),
        glossary_compact: String::from_utf16_lossy(&out.glossary_compact),
    }
}

/// Formats glossary definition items into Memento's HTML representation.
pub fn format_glossary(items: &[&str], dict_name: &str, tags: &[&str]) -> GlossaryOutput {
    if items.is_empty() {
        return GlossaryOutput {
            glossary: String::new(),
            glossary_brief: String::new(),
            glossary_compact: String::new(),
        };
    }
    let dict_name_u16: Vec<u16> = escape_html_attr(dict_name).encode_utf16().collect();
    let tags_u16: Vec<Vec<u16>> = tags.iter().map(|t| t.encode_utf16().collect()).collect();
    let tags_refs: Vec<&[u16]> = tags_u16.iter().map(|v| v.as_slice()).collect();
    let items_u16: Vec<Vec<u16>> = items.iter().map(|it| it.encode_utf16().collect()).collect();
    let items_refs: Vec<&[u16]> = items_u16.iter().map(|v| v.as_slice()).collect();

    let def = TermGlossaryDef {
        dict_name: &dict_name_u16,
        dict_id: 1,
        tags: &tags_refs,
        items: &items_refs,
        has_styles: false,
        stylesheet: &[],
    };
    let out = format_term_glossary(&[def]);
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
    pub secondary_context: String,
    pub clipboard: Option<String>,
    pub title: String,
    pub cloze_prefix: String,
    pub cloze_body: String,
    pub cloze_suffix: String,
    pub glossary_items: Vec<String>,
    pub glossary_definitions: Vec<GlossaryDefinition>,
    pub furigana: String,
    pub furigana_plain: String,
    pub pitch: String,
    pub pitch_graphs: String,
    pub pitch_positions: String,
    pub pitch_categories: String,
    pub frequencies: String,
    pub frequency_entries: Vec<(String, String)>,
    pub frequency_harmonic_rank: String,
    pub frequency_average_rank: String,
    pub frequency_harmonic_occurrence: String,
    pub frequency_average_occurrence: String,
    pub character: String,
    pub onyomi: String,
    pub kunyomi: String,
    pub stroke_count: String,
    pub dictionary_name: String,
    /// Term/kanji tags used by `{tags}` and glossary formatting. Anki note
    /// tags come from the selected profile, not this context field.
    pub tags: Vec<String>,
    /// Rich tag metadata (including `tag_bank` notes and categories) for `{tags}` vs `{tags-brief}`.
    pub tag_entries: Vec<NoteTag>,
    /// Part-of-speech summary for `{part-of-speech}`.
    pub part_of_speech: String,
    pub is_kanji: bool,
    pub cloze_index: usize,
    /// AnkiConnect attachments populated before submitting a profile note.
    pub audio_attachments: Vec<serde_json::Value>,
    pub picture_attachments: Vec<serde_json::Value>,
    pub video_attachments: Vec<serde_json::Value>,
    /// `(field_name, marker_name)` pairs that succeeded during `prepare_note_media`.
    pub(crate) resolved_media_markers: Vec<(String, String)>,
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
        let (furigana, furigana_plain) = format_expression_furigana(&expression, &reading);
        Self {
            reading,
            context: sentence.clone(),
            // Keep `{selection}` empty unless actual popup glossary text is selected (#262).
            selection: String::new(),
            secondary_subtitle: String::new(),
            secondary_context: String::new(),
            clipboard: None,
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
            pitch_graphs: String::new(),
            pitch_positions: String::new(),
            pitch_categories: String::new(),
            frequencies: String::new(),
            frequency_entries: Vec::new(),
            frequency_harmonic_rank: String::new(),
            frequency_average_rank: String::new(),
            frequency_harmonic_occurrence: String::new(),
            frequency_average_occurrence: String::new(),
            character: String::new(),
            onyomi: String::new(),
            kunyomi: String::new(),
            stroke_count: String::new(),
            dictionary_name: String::new(),
            tags: Vec::new(),
            tag_entries: Vec::new(),
            part_of_speech: String::new(),
            is_kanji: false,
            cloze_index: 1,
            audio_attachments: Vec::new(),
            picture_attachments: Vec::new(),
            video_attachments: Vec::new(),
            resolved_media_markers: Vec::new(),
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

    pub fn with_tag_note(
        mut self,
        name: impl Into<String>,
        category: impl Into<String>,
        notes: impl Into<String>,
    ) -> Self {
        let name = name.into();
        if !self.tags.contains(&name) {
            self.tags.push(name.clone());
        }
        self.tag_entries.push(NoteTag {
            name,
            category: category.into(),
            notes: notes.into(),
        });
        self
    }

    pub fn with_cloze_index(mut self, index: usize) -> Self {
        self.cloze_index = index;
        self
    }
}

// ── SQLite Dictionary Enrichment for Anki Worker ────────────────────────────

struct RawTagInfo {
    category: String,
    order: i64,
    notes: String,
    score: i64,
}

fn encode_base64_bytes(bytes: &[u8]) -> Option<String> {
    let len = memento_util::fileutils::base64_len(bytes.len())?;
    let mut encoded = vec![0u8; len];
    memento_util::fileutils::to_base64(bytes, &mut encoded)?;
    String::from_utf8(encoded).ok()
}

fn safe_dict_subdir(res_root: &Path, dict_name: &str) -> Option<PathBuf> {
    if dict_name.is_empty()
        || dict_name == "."
        || dict_name == ".."
        || dict_name.contains('/')
        || dict_name.contains('\\')
        || dict_name.contains('\0')
    {
        return None;
    }
    Some(res_root.join(dict_name))
}

fn resolve_dictionary_resources(
    resource_roots: &[PathBuf],
    dic_id: i64,
    dict_name: &str,
) -> (Option<PathBuf>, String) {
    let mut found_dir: Option<PathBuf> = None;
    for root in resource_roots {
        if root.as_os_str().is_empty() {
            continue;
        }
        if let Ok(by_id) = yomidbbuilder::dictionary_resource_dir(root, dic_id) {
            if by_id.is_dir() {
                if found_dir.is_none() {
                    found_dir = Some(by_id.clone());
                }
                let style_path = by_id.join("styles.css");
                if let Ok(css) = fs::read_to_string(&style_path) {
                    return (Some(by_id), scope_dictionary_css(&css, dict_name));
                }
            }
        }
        if let Some(by_name) = safe_dict_subdir(root, dict_name) {
            if by_name.is_dir() {
                if found_dir.is_none() {
                    found_dir = Some(by_name.clone());
                }
                let style_path = by_name.join("styles.css");
                if let Ok(css) = fs::read_to_string(&style_path) {
                    return (Some(by_name), scope_dictionary_css(&css, dict_name));
                }
            }
        }
    }
    (found_dir, String::new())
}

fn resolve_sorted_tags(
    tag_bank: &HashMap<(i64, String), RawTagInfo>,
    dic_id: i64,
    raw_space_separated: &str,
) -> Vec<(NoteTag, i64, i64)> {
    let mut seen = HashSet::new();
    let mut collected = Vec::new();
    for token in raw_space_separated.split_whitespace() {
        if token.is_empty() || !seen.insert(token) {
            continue;
        }
        if let Some(info) = tag_bank.get(&(dic_id, token.to_owned())) {
            collected.push((
                NoteTag {
                    name: token.to_owned(),
                    category: info.category.clone(),
                    notes: info.notes.clone(),
                },
                info.order,
                info.score,
            ));
        } else {
            collected.push((
                NoteTag {
                    name: token.to_owned(),
                    category: String::new(),
                    notes: String::new(),
                },
                0,
                0,
            ));
        }
    }
    if collected.len() > 1 {
        let sort_items: Vec<TagSortItem> = collected
            .iter()
            .enumerate()
            .map(|(original_index, (_, order, score))| TagSortItem {
                original_index,
                order: *order,
                score: *score,
            })
            .collect();
        let order_indices = searchcontroller::sort_tags(sort_items);
        order_indices
            .into_iter()
            .map(|idx| collected[idx].clone())
            .collect()
    } else {
        collected
    }
}

fn apply_frequency_rows(
    ctx: &mut NoteFieldsContext,
    titles: &HashMap<i64, String>,
    mut frequency_rows: Vec<sqlite_db::FreqRow>,
) {
    frequency_rows.sort_by_key(|row| row.dic_id);
    ctx.frequency_entries = frequency_rows
        .iter()
        .filter_map(|row| {
            let dict = titles.get(&row.dic_id)?;
            Some((dict.clone(), String::from_utf16_lossy(&row.frequency)))
        })
        .collect();

    let dict_u16: Vec<Vec<u16>> = ctx
        .frequency_entries
        .iter()
        .map(|(d, _)| d.encode_utf16().collect())
        .collect();
    let freq_u16: Vec<Vec<u16>> = ctx
        .frequency_entries
        .iter()
        .map(|(_, f)| f.encode_utf16().collect())
        .collect();
    let entries: Vec<FrequencyEntry<'_>> = dict_u16
        .iter()
        .zip(&freq_u16)
        .map(|(d, f)| FrequencyEntry {
            dictionary: d,
            freq: f,
        })
        .collect();
    ctx.frequencies = String::from_utf16_lossy(&build_frequencies_html(&entries, false, false));
    let (_, harmonic, average) = frequency_stats(&entries);
    ctx.frequency_harmonic_rank = harmonic
        .map(|v| v.to_string())
        .unwrap_or_else(|| "9999999".to_owned());
    ctx.frequency_average_rank = average
        .map(|v| v.to_string())
        .unwrap_or_else(|| "9999999".to_owned());
    ctx.frequency_harmonic_occurrence = harmonic
        .map(|v| v.to_string())
        .unwrap_or_else(|| "0".to_owned());
    ctx.frequency_average_occurrence = average
        .map(|v| v.to_string())
        .unwrap_or_else(|| "0".to_owned());
}

/// Enriches `ctx` directly from the Yomitan SQLite database (`term_bank` / `kanji_bank` /
/// `tag_bank` / `term_meta_bank` / `kanji_meta_bank`) and dictionary `styles.css` + media
/// resources using `memento_anki::glossary::build_glossary`.
pub fn populate_note_context_from_sqlite(
    ctx: &mut NoteFieldsContext,
    db_path: &Path,
    resource_roots: &[PathBuf],
    selected_occurrences: &[(String, usize)],
) -> Result<(), String> {
    if !db_path.is_file() {
        return Ok(());
    }
    let conn = sqlite_db::open_read_only_connection(db_path).map_err(|e| e.to_string())?;

    let mut titles: HashMap<i64, String> = HashMap::new();
    {
        let mut stmt = conn
            .prepare("SELECT dic_id, title FROM directory;")
            .map_err(|e| e.to_string())?;
        let mut rows = stmt.query([]).map_err(|e| e.to_string())?;
        while let Some(row) = rows.next().map_err(|e| e.to_string())? {
            let dic_id: i64 = row.get(0).map_err(|e| e.to_string())?;
            let title: String = row.get(1).map_err(|e| e.to_string())?;
            titles.insert(dic_id, title);
        }
    }

    let mut tag_bank: HashMap<(i64, String), RawTagInfo> = HashMap::new();
    if let Ok(mut stmt) =
        conn.prepare("SELECT dic_id, name, category, ord, notes, score FROM tag_bank;")
    {
        if let Ok(mut rows) = stmt.query([]) {
            while let Ok(Some(row)) = rows.next() {
                if let (Ok(dic_id), Ok(name), Ok(category), Ok(order), Ok(notes), Ok(score)) = (
                    row.get::<_, i64>(0),
                    row.get::<_, String>(1),
                    row.get::<_, String>(2),
                    row.get::<_, i64>(3),
                    row.get::<_, String>(4),
                    row.get::<_, i64>(5),
                ) {
                    tag_bank.insert(
                        (dic_id, name),
                        RawTagInfo {
                            category,
                            order,
                            notes,
                            score,
                        },
                    );
                }
            }
        }
    }

    let is_kanji = ctx.is_kanji || !ctx.character.is_empty();
    if is_kanji {
        let target_char = if !ctx.character.is_empty() {
            ctx.character.clone()
        } else {
            ctx.expression.clone()
        };
        ctx.character = target_char.clone();
        let char_u16: Vec<u16> = target_char.encode_utf16().collect();
        let mut kanji_rows = Vec::new();
        sqlite_db::query_kanji_conn(&conn, &char_u16, &mut kanji_rows)
            .map_err(|e| e.to_string())?;

        let mut occ_counts: HashMap<String, usize> = HashMap::new();
        let mut matched_defs: Vec<(Option<usize>, GlossaryDefinition)> = Vec::new();
        let mut onyomi_parts: Vec<Vec<u16>> = Vec::new();
        let mut kunyomi_parts: Vec<Vec<u16>> = Vec::new();
        let mut stroke_found: Option<String> = None;
        let mut accumulated_tags: Vec<NoteTag> = Vec::new();
        let mut seen_tag_names: HashSet<String> = HashSet::new();

        for row in kanji_rows {
            let Some(dict_name) = titles.get(&row.dic_id).cloned() else {
                continue;
            };
            let occ_idx = *occ_counts.entry(dict_name.clone()).and_modify(|c| *c += 1).or_insert(0);
            let order_pos = if selected_occurrences.is_empty() {
                Some(matched_defs.len())
            } else {
                selected_occurrences
                    .iter()
                    .position(|(sel_name, sel_occ)| sel_name == &dict_name && *sel_occ == occ_idx)
            };
            if order_pos.is_none() {
                continue;
            }

            let onyomi_s = String::from_utf16_lossy(&row.onyomi);
            for part in onyomi_s.split_whitespace() {
                if !part.is_empty() {
                    onyomi_parts.push(part.encode_utf16().collect());
                }
            }
            let kunyomi_s = String::from_utf16_lossy(&row.kunyomi);
            for part in kunyomi_s.split_whitespace() {
                if !part.is_empty() {
                    kunyomi_parts.push(part.encode_utf16().collect());
                }
            }

            let raw_tags = String::from_utf16_lossy(&row.tags);
            let resolved_tags = resolve_sorted_tags(&tag_bank, row.dic_id, &raw_tags);
            for (tag, _, _) in &resolved_tags {
                if seen_tag_names.insert(tag.name.clone()) {
                    accumulated_tags.push(tag.clone());
                }
            }

            if stroke_found.is_none() {
                let raw_stats = String::from_utf16_lossy(&row.stats);
                if let Ok(serde_json::Value::Object(stats_map)) =
                    serde_json::from_str::<serde_json::Value>(&raw_stats)
                {
                    let stat_pairs: Vec<(Vec<u16>, Vec<u16>)> = stats_map
                        .iter()
                        .filter_map(|(stat_key, val)| {
                            let tag_info = tag_bank.get(&(row.dic_id, stat_key.clone()))?;
                            let val_s = match val {
                                serde_json::Value::String(s) => s.clone(),
                                serde_json::Value::Number(n) => n.to_string(),
                                _ => return None,
                            };
                            Some((
                                tag_info.notes.encode_utf16().collect(),
                                val_s.encode_utf16().collect(),
                            ))
                        })
                        .collect();
                    let refs: Vec<StrokeCountStat<'_>> = stat_pairs
                        .iter()
                        .map(|(notes, value)| StrokeCountStat { notes, value })
                        .collect();
                    if let Some(stroke_u16) = find_stroke_count(&refs) {
                        stroke_found = Some(String::from_utf16_lossy(stroke_u16));
                    }
                }
            }

            let meanings: Vec<String> = row
                .meanings
                .iter()
                .map(|m| String::from_utf16_lossy(m))
                .collect();
            matched_defs.push((
                order_pos,
                GlossaryDefinition {
                    dictionary_id: row.dic_id,
                    dictionary_name: dict_name,
                    glossary: meanings.join("\n"),
                    tags: resolved_tags.iter().map(|(t, _, _)| t.name.clone()).collect(),
                    kanji_meanings: meanings,
                    ..Default::default()
                },
            ));
        }

        if !matched_defs.is_empty() {
            matched_defs.sort_by_key(|(pos, _)| pos.unwrap_or(usize::MAX));
            ctx.glossary_definitions = matched_defs.into_iter().map(|(_, def)| def).collect();
            let on_refs: Vec<&[u16]> = onyomi_parts.iter().map(Vec::as_slice).collect();
            let kun_refs: Vec<&[u16]> = kunyomi_parts.iter().map(Vec::as_slice).collect();
            ctx.onyomi = String::from_utf16_lossy(&format_kanji_readings(&on_refs));
            ctx.kunyomi = String::from_utf16_lossy(&format_kanji_readings(&kun_refs));
            if let Some(strokes) = stroke_found.filter(|s| !s.is_empty()) {
                ctx.stroke_count = strokes;
            }
            ctx.tag_entries = accumulated_tags;
            ctx.tags = ctx.tag_entries.iter().map(|t| t.name.clone()).collect();
        }

        const KANJI_FREQ_SQL: &str = "SELECT dic_id, data, type FROM kanji_meta_bank \
             WHERE dic_id NOT IN (SELECT dic_id FROM dict_disabled) AND \
             (expression = ?1 AND mode = 'freq');";
        let mut freq_rows = Vec::new();
        if sqlite_db::get_frequencies_with_sql_conn(
            &conn,
            Some(KANJI_FREQ_SQL),
            &char_u16,
            &[],
            &mut freq_rows,
        )
        .is_ok()
        {
            apply_frequency_rows(ctx, &titles, freq_rows);
        }
        return Ok(());
    }

    // Term card SQLite enrichment
    const TERM_SQL: &str = "SELECT dic_id, glossary, rules, def_tags, term_tags \
        FROM term_bank \
        WHERE expression = ?1 \
          AND (reading = ?2 OR (?2 = ?1 AND reading = '')) \
          AND dic_id NOT IN (SELECT dic_id FROM dict_disabled) \
        ORDER BY dic_id ASC, score DESC, sequence ASC, rowid ASC;";

    let mut occ_counts: HashMap<String, usize> = HashMap::new();
    let mut matched_defs: Vec<(Option<usize>, GlossaryDefinition)> = Vec::new();
    let mut all_rules: Vec<String> = Vec::new();
    let mut term_tag_candidates: Vec<(NoteTag, i64, i64)> = Vec::new();
    let mut def_tag_candidates: Vec<(NoteTag, i64, i64)> = Vec::new();
    let mut seen_term_tags: HashSet<String> = HashSet::new();
    let mut seen_def_tags: HashSet<String> = HashSet::new();
    let mut pos_tags: Vec<String> = Vec::new();

    {
        let mut stmt = conn.prepare(TERM_SQL).map_err(|e| e.to_string())?;
        let mut rows = stmt
            .query(rusqlite_params(&ctx.expression, &ctx.reading))
            .map_err(|e| e.to_string())?;
        while let Some(row) = rows.next().map_err(|e| e.to_string())? {
            let dic_id: i64 = row.get(0).map_err(|e| e.to_string())?;
            let raw_glossary: String = row.get(1).map_err(|e| e.to_string())?;
            let raw_rules: String = row.get(2).map_err(|e| e.to_string())?;
            let raw_def_tags: String = row.get(3).map_err(|e| e.to_string())?;
            let raw_term_tags: String = row.get(4).map_err(|e| e.to_string())?;

            let Some(dict_name) = titles.get(&dic_id).cloned() else {
                continue;
            };

            let parsed_val: serde_json::Value = match serde_json::from_str(&raw_glossary) {
                Ok(val) => val,
                Err(_) => serde_json::Value::Array(vec![serde_json::Value::String(raw_glossary)]),
            };
            let filtered_items: Vec<serde_json::Value> = match parsed_val {
                serde_json::Value::Array(arr) => {
                    arr.into_iter().filter(|item| !item.is_array()).collect()
                }
                other => vec![other],
            };
            if filtered_items.is_empty() {
                continue;
            }

            let occ_idx = *occ_counts.entry(dict_name.clone()).and_modify(|c| *c += 1).or_insert(0);
            let order_pos = if selected_occurrences.is_empty() {
                Some(matched_defs.len())
            } else {
                selected_occurrences
                    .iter()
                    .position(|(sel_name, sel_occ)| sel_name == &dict_name && *sel_occ == occ_idx)
            };
            if order_pos.is_none() {
                continue;
            }

            for rule in raw_rules.split_whitespace() {
                if !rule.is_empty() {
                    all_rules.push(rule.to_owned());
                }
            }

            let resolved_def_tags = resolve_sorted_tags(&tag_bank, dic_id, &raw_def_tags);
            let resolved_term_tags = resolve_sorted_tags(&tag_bank, dic_id, &raw_term_tags);
            for item in &resolved_term_tags {
                if seen_term_tags.insert(item.0.name.clone()) {
                    term_tag_candidates.push(item.clone());
                }
                if item.0.category == "partOfSpeech" && !pos_tags.contains(&item.0.name) {
                    pos_tags.push(item.0.name.clone());
                }
            }
            for item in &resolved_def_tags {
                if seen_def_tags.insert(item.0.name.clone()) {
                    def_tag_candidates.push(item.clone());
                }
                if item.0.category == "partOfSpeech" && !pos_tags.contains(&item.0.name) {
                    pos_tags.push(item.0.name.clone());
                }
            }

            let (res_dir_opt, scoped_css) =
                resolve_dictionary_resources(resource_roots, dic_id, &dict_name);
            let canon_base = res_dir_opt.as_ref().and_then(|dir| fs::canonicalize(dir).ok());
            let basepath_str = canon_base
                .as_ref()
                .or(res_dir_opt.as_ref())
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default();
            let basepath_u16: Vec<u16> = basepath_str.encode_utf16().collect();

            let qjson_defs: Vec<memento_anki::json::JsonValue> =
                filtered_items.iter().map(to_qjson).collect();

            let mut verified_files: HashMap<String, PathBuf> = HashMap::new();
            let (rendered_u16, file_map) =
                build_glossary(&qjson_defs, &basepath_u16, |full_path_u16| {
                    let base = canon_base.as_ref()?;
                    let candidate = PathBuf::from(String::from_utf16(full_path_u16).ok()?);
                    let canon_file = fs::canonicalize(&candidate).ok()?;
                    if !canon_file.starts_with(base) || !canon_file.is_file() {
                        return None;
                    }
                    let file = File::open(&canon_file).ok()?;
                    let md5 = memento_util::fileutils::calculate_md5(file).ok()?;
                    let hex = format!("{:032x}", u128::from_be_bytes(md5));
                    let mut hex_u16 = [0u16; 32];
                    for (idx, b) in hex.bytes().enumerate() {
                        hex_u16[idx] = u16::from(b);
                    }
                    verified_files
                        .insert(String::from_utf16_lossy(full_path_u16), canon_file);
                    Some(hex_u16)
                });

            let mut media_files = Vec::new();
            for (full_path_u16, hash_name_u16) in file_map {
                let key = String::from_utf16_lossy(&full_path_u16);
                let Some(canon_file) = verified_files.get(&key) else {
                    continue;
                };
                let Ok(bytes) = fs::read(canon_file) else {
                    continue;
                };
                let Some(data_base64) = encode_base64_bytes(&bytes) else {
                    continue;
                };
                let raw_name = String::from_utf16_lossy(&hash_name_u16);
                let safe_name = sanitize_anki_filename(&raw_name, ".bin");
                media_files.push(MediaFilePayload {
                    filename: safe_name,
                    data_base64,
                });
            }

            let rendered_items: Vec<String> = rendered_u16
                .into_iter()
                .map(|item| String::from_utf16_lossy(&item))
                .collect();

            matched_defs.push((
                order_pos,
                GlossaryDefinition {
                    dictionary_id: dic_id,
                    dictionary_name: dict_name,
                    glossary: rendered_items.join("\n"),
                    tags: resolved_def_tags
                        .iter()
                        .map(|(t, _, _)| t.name.clone())
                        .collect(),
                    rendered_items,
                    stylesheet: scoped_css,
                    media_files,
                    ..Default::default()
                },
            ));
        }
    }

    if !matched_defs.is_empty() {
        matched_defs.sort_by_key(|(pos, _)| pos.unwrap_or(usize::MAX));
        ctx.glossary_definitions = matched_defs.into_iter().map(|(_, def)| def).collect();

        let chosen_tags = if !term_tag_candidates.is_empty() {
            term_tag_candidates
        } else {
            def_tag_candidates
        };
        if !chosen_tags.is_empty() {
            let sort_items: Vec<TagSortItem> = chosen_tags
                .iter()
                .enumerate()
                .map(|(original_index, (_, order, score))| TagSortItem {
                    original_index,
                    order: *order,
                    score: *score,
                })
                .collect();
            let indices = searchcontroller::sort_tags(sort_items);
            ctx.tag_entries = indices
                .into_iter()
                .map(|idx| chosen_tags[idx].0.clone())
                .collect();
            ctx.tags = ctx.tag_entries.iter().map(|t| t.name.clone()).collect();
        }
        if !pos_tags.is_empty() {
            ctx.part_of_speech = pos_tags.join(", ");
        } else if !all_rules.is_empty() {
            let mut unique_rules = Vec::new();
            for r in &all_rules {
                if !unique_rules.contains(r) {
                    unique_rules.push(r.clone());
                }
            }
            ctx.part_of_speech = unique_rules.join(", ");
        }
    } else {
        // Still resolve dictionary_id on existing UI definitions if present.
        for definition in &mut ctx.glossary_definitions {
            if definition.dictionary_id == 0 {
                if let Some((&id, _)) = titles
                    .iter()
                    .find(|(_, title)| *title == &definition.dictionary_name)
                {
                    definition.dictionary_id = id;
                }
            }
        }
    }

    let expr_u16: Vec<u16> = ctx.expression.encode_utf16().collect();
    let read_u16: Vec<u16> = ctx.reading.encode_utf16().collect();

    let mut freq_rows = Vec::new();
    if sqlite_db::get_frequencies_conn(&conn, &expr_u16, &read_u16, &mut freq_rows).is_ok() {
        apply_frequency_rows(ctx, &titles, freq_rows);
    }

    let mut pitch_rows = Vec::new();
    if sqlite_db::add_pitches_conn(&conn, &expr_u16, &read_u16, &mut pitch_rows).is_ok() {
        pitch_rows.sort_by_key(|row| row.dic_id);
        let pitches_owned: Vec<(Vec<u16>, Vec<u8>, Vec<Vec<u16>>)> = pitch_rows
            .iter()
            .filter_map(|row| {
                let positions: Vec<u8> = row
                    .positions
                    .iter()
                    .filter_map(|p| u8::try_from(*p).ok())
                    .collect();
                (!positions.is_empty() && !row.mora.is_empty()).then(|| {
                    let dict_u16 = titles
                        .get(&row.dic_id)
                        .map(|s| s.encode_utf16().collect())
                        .unwrap_or_default();
                    (dict_u16, positions, row.mora.clone())
                })
            })
            .collect();

        let mora_refs: Vec<Vec<&[u16]>> = pitches_owned
            .iter()
            .map(|(_, _, mora)| mora.iter().map(Vec::as_slice).collect())
            .collect();
        let pitch_entries: Vec<PitchEntry<'_>> = pitches_owned
            .iter()
            .enumerate()
            .map(|(idx, (name, positions, _))| PitchEntry {
                dictionary_name: name,
                positions,
                mora: &mora_refs[idx],
            })
            .collect();

        let fill_u16: Vec<u16> = "rgba(0,0,0,0)".encode_utf16().collect();
        let stroke_u16: Vec<u16> = "currentColor".encode_utf16().collect();
        let svgs_owned: Vec<Vec<Vec<u16>>> = pitches_owned
            .iter()
            .map(|(_, positions, mora)| {
                let mora_len = i32::try_from(mora.len()).unwrap_or(0);
                positions
                    .iter()
                    .filter_map(|&pos| {
                        generate_pitch_graph(
                            mora_len,
                            pos,
                            &fill_u16,
                            &stroke_u16,
                            |n| Some(n.to_string().encode_utf16().collect()),
                            |_| None,
                        )
                    })
                    .collect()
            })
            .collect();
        let svg_refs: Vec<Vec<&[u16]>> = svgs_owned
            .iter()
            .map(|svgs| svgs.iter().map(Vec::as_slice).collect())
            .collect();
        let graph_entries: Vec<PitchGraphEntry<'_>> = pitches_owned
            .iter()
            .enumerate()
            .map(|(idx, (name, positions, _))| PitchGraphEntry {
                dictionary_name: name,
                positions_count: positions.len(),
                svg_graphs: &svg_refs[idx],
            })
            .collect();

        let category_inputs: Vec<PitchCategoryInput<'_>> = pitches_owned
            .iter()
            .map(|(_, positions, mora)| PitchCategoryInput {
                mora_len: mora.len(),
                positions,
            })
            .collect();
        let rule_u16: Vec<Vec<u16>> = if !all_rules.is_empty() {
            all_rules
                .iter()
                .map(|r| r.encode_utf16().collect())
                .collect()
        } else {
            ctx.tags.iter().map(|t| t.encode_utf16().collect()).collect()
        };
        let rule_refs: Vec<&[u16]> = rule_u16.iter().map(Vec::as_slice).collect();
        let can_be_kifuku = is_kifuku_applicable(&rule_refs);

        ctx.pitch = String::from_utf16_lossy(&format_pitch(&pitch_entries));
        ctx.pitch_positions = String::from_utf16_lossy(&format_pitch_positions(&pitch_entries));
        ctx.pitch_graphs = String::from_utf16_lossy(&format_pitch_graphs(&graph_entries));
        ctx.pitch_categories =
            String::from_utf16_lossy(&format_pitch_categories(&category_inputs, can_be_kifuku));
    }

    Ok(())
}

fn rusqlite_params<'a>(expr: &'a str, reading: &'a str) -> [&'a str; 2] {
    [expr, reading]
}

// ── Marker Evaluation & Substitution ────────────────────────────────────────

const MEDIA_MARKERS: &[&str] = &[
    "audio",
    "audio-media",
    "audio-context",
    "sentence-audio",
    "screenshot",
    "screenshot-video",
    "image",
    "video",
];

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
    "part-of-speech",
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

fn marker_arg<'a>(marker: &'a Marker, key: &str) -> Option<&'a [u16]> {
    marker.args.iter().find_map(|(arg_key, value)| {
        arg_key
            .iter()
            .copied()
            .eq(key.encode_utf16())
            .then_some(value.as_slice())
    })
}

fn marker_bool_arg(marker: &Marker, key: &str, default: bool) -> bool {
    let Some(value) = marker_arg(marker, key) else {
        return default;
    };
    if value.is_empty() {
        return false;
    }
    let text = String::from_utf16_lossy(value);
    let trimmed = text.trim();
    !trimmed.is_empty() && trimmed != "0" && !trimmed.eq_ignore_ascii_case("false")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GlossaryDictFilter {
    Unfiltered,
    ById(i64),
    Invalid,
}

fn parse_glossary_dict_filter(marker: &Marker) -> GlossaryDictFilter {
    let Some(raw_val) = marker_arg(marker, "dict") else {
        return GlossaryDictFilter::Unfiltered;
    };
    let Ok(text) = String::from_utf16(raw_val) else {
        return GlossaryDictFilter::Invalid;
    };
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return GlossaryDictFilter::Invalid;
    }
    match trimmed.parse::<i64>() {
        Ok(id) => GlossaryDictFilter::ById(id),
        Err(_) => GlossaryDictFilter::Invalid,
    }
}

fn templates_reference_marker<'a>(
    templates: impl Iterator<Item = &'a String>,
    marker_name: &str,
) -> bool {
    for template in templates {
        for token in tokenize(&template.encode_utf16().collect::<Vec<_>>()) {
            if token.markers.iter().any(|m| marker_is(m, marker_name)) {
                return true;
            }
        }
    }
    false
}

fn resolve_context_tags(ctx: &NoteFieldsContext) -> (String, String) {
    if !ctx.tag_entries.is_empty() {
        let names_u16: Vec<Vec<u16>> = ctx
            .tag_entries
            .iter()
            .map(|t| escape_html(&t.name).encode_utf16().collect())
            .collect();
        let notes_u16: Vec<Vec<u16>> = ctx
            .tag_entries
            .iter()
            .map(|t| escape_html(&t.notes).encode_utf16().collect())
            .collect();
        let entries: Vec<TagEntry<'_>> = names_u16
            .iter()
            .zip(&notes_u16)
            .map(|(name, notes)| TagEntry { name, notes })
            .collect();
        return (
            String::from_utf16_lossy(&format_tags(&entries, false)),
            String::from_utf16_lossy(&format_tags(&entries, true)),
        );
    }
    if !ctx.tags.is_empty() {
        let names_u16: Vec<Vec<u16>> = ctx
            .tags
            .iter()
            .map(|t| escape_html(t).encode_utf16().collect())
            .collect();
        let entries: Vec<TagEntry<'_>> = names_u16
            .iter()
            .map(|name| TagEntry { name, notes: &[] })
            .collect();
        let full = String::from_utf16_lossy(&format_tags(&entries, false));
        let brief = String::from_utf16_lossy(&format_tags(&entries, true));
        return (full, brief);
    }
    (String::new(), String::new())
}

fn resolve_part_of_speech(ctx: &NoteFieldsContext) -> String {
    if !ctx.part_of_speech.is_empty() {
        return ctx.part_of_speech.clone();
    }
    let pos: Vec<&str> = ctx
        .tag_entries
        .iter()
        .filter(|t| t.category == "partOfSpeech")
        .map(|t| t.name.as_str())
        .collect();
    pos.join(", ")
}

fn render_frequencies_marker(marker: &Marker, ctx: &NoteFieldsContext) -> String {
    let value_only = marker_bool_arg(marker, "value-only", false);
    let min_value = marker_bool_arg(marker, "min-value", false);
    if !ctx.frequency_entries.is_empty() {
        let dict_u16: Vec<Vec<u16>> = ctx
            .frequency_entries
            .iter()
            .map(|(d, _)| d.encode_utf16().collect())
            .collect();
        let freq_u16: Vec<Vec<u16>> = ctx
            .frequency_entries
            .iter()
            .map(|(_, f)| f.encode_utf16().collect())
            .collect();
        let entries: Vec<FrequencyEntry<'_>> = dict_u16
            .iter()
            .zip(&freq_u16)
            .map(|(d, f)| FrequencyEntry {
                dictionary: d,
                freq: f,
            })
            .collect();
        return String::from_utf16_lossy(&build_frequencies_html(
            &entries,
            value_only,
            min_value,
        ));
    }
    ctx.frequencies.clone()
}

fn resolve_frequency_scalar<'a>(
    ctx: &'a NoteFieldsContext,
    marker: &Marker,
) -> Option<String> {
    if marker_is(marker, "frequency-harmonic-rank") {
        if !ctx.frequency_harmonic_rank.is_empty() {
            Some(ctx.frequency_harmonic_rank.clone())
        } else {
            Some("9999999".to_owned())
        }
    } else if marker_is(marker, "frequency-average-rank") {
        if !ctx.frequency_average_rank.is_empty() {
            Some(ctx.frequency_average_rank.clone())
        } else {
            Some("9999999".to_owned())
        }
    } else if marker_is(marker, "frequency-harmonic-occurrence") {
        if !ctx.frequency_harmonic_occurrence.is_empty() {
            Some(ctx.frequency_harmonic_occurrence.clone())
        } else if !ctx.frequency_harmonic_rank.is_empty()
            && ctx.frequency_harmonic_rank != "9999999"
        {
            Some(ctx.frequency_harmonic_rank.clone())
        } else {
            Some("0".to_owned())
        }
    } else if marker_is(marker, "frequency-average-occurrence") {
        if !ctx.frequency_average_occurrence.is_empty() {
            Some(ctx.frequency_average_occurrence.clone())
        } else if !ctx.frequency_average_rank.is_empty()
            && ctx.frequency_average_rank != "9999999"
        {
            Some(ctx.frequency_average_rank.clone())
        } else {
            Some("0".to_owned())
        }
    } else {
        None
    }
}

fn common_marker_value<'a>(
    marker: &Marker,
    ctx: &'a NoteFieldsContext,
    clipboard_text: &'a str,
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
    } else if marker_is(marker, "clipboard") {
        Some(clipboard_text)
    } else if marker_is(marker, "subtitle-2") || marker_is(marker, "sentence-2") {
        Some(ctx.secondary_subtitle.as_str())
    } else if marker_is(marker, "context-2") {
        if !ctx.secondary_context.is_empty() {
            Some(ctx.secondary_context.as_str())
        } else {
            Some(ctx.secondary_subtitle.as_str())
        }
    } else if marker_is(marker, "title") {
        Some(ctx.title.as_str())
    } else {
        None
    }
}

fn field_has_attachment(attachments: &[serde_json::Value], field: &str) -> bool {
    attachments.iter().any(|item| {
        item.get("fields")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|fields| fields.iter().any(|f| f.as_str() == Some(field)))
    })
}

fn media_marker_has_attachment(marker: &Marker, field: &str, ctx: &NoteFieldsContext) -> bool {
    let name = String::from_utf16_lossy(&marker.name);
    if !ctx.resolved_media_markers.is_empty() {
        return ctx
            .resolved_media_markers
            .iter()
            .any(|(f, m)| f == field && m == &name);
    }
    match name.as_str() {
        "audio" | "audio-media" | "audio-context" | "sentence-audio" => {
            field_has_attachment(&ctx.audio_attachments, field)
        }
        "screenshot" | "screenshot-video" | "image" => {
            field_has_attachment(&ctx.picture_attachments, field)
        }
        "video" => field_has_attachment(&ctx.video_attachments, field),
        _ => false,
    }
}

fn evaluate_specific_marker(
    field: &str,
    marker: &Marker,
    ctx: &NoteFieldsContext,
    glossary: &GlossaryOutput,
    tags_full: &str,
    tags_brief: &str,
    part_of_speech: &str,
) -> MarkerResult {
    if marker_is(marker, "glossary")
        || marker_is(marker, "glossary-brief")
        || marker_is(marker, "glossary-compact")
    {
        match parse_glossary_dict_filter(marker) {
            GlossaryDictFilter::Invalid => {
                return marker_result("ERROR: Invalid dic-id value");
            }
            GlossaryDictFilter::ById(id) => {
                let filtered = format_context_glossary_for_dictionary(ctx, Some(id));
                let value = if marker_is(marker, "glossary-brief") {
                    &filtered.glossary_brief
                } else if marker_is(marker, "glossary-compact") {
                    &filtered.glossary_compact
                } else {
                    &filtered.glossary
                };
                return marker_result(value);
            }
            GlossaryDictFilter::Unfiltered => {
                let value = if marker_is(marker, "glossary-brief") {
                    &glossary.glossary_brief
                } else if marker_is(marker, "glossary-compact") {
                    &glossary.glossary_compact
                } else {
                    &glossary.glossary
                };
                return marker_result(value);
            }
        }
    }

    if marker_is(marker, "frequencies") {
        return marker_result(&render_frequencies_marker(marker, ctx));
    }
    if let Some(freq_scalar) = resolve_frequency_scalar(ctx, marker) {
        return marker_result(&freq_scalar);
    }

    if MEDIA_MARKERS.iter().any(|name| marker_is(marker, name)) {
        return MarkerResult {
            handled: true,
            media: media_marker_has_attachment(marker, field, ctx),
            text: Vec::new(),
        };
    }

    let text_opt = if marker_is(marker, "expression") {
        Some(ctx.expression.as_str())
    } else if marker_is(marker, "reading") {
        Some(ctx.reading.as_str())
    } else if marker_is(marker, "tags") {
        Some(tags_full)
    } else if marker_is(marker, "tags-brief") {
        Some(tags_brief)
    } else if marker_is(marker, "part-of-speech") {
        Some(part_of_speech)
    } else if marker_is(marker, "furigana") {
        Some(ctx.furigana.as_str())
    } else if marker_is(marker, "furigana-plain") {
        Some(ctx.furigana_plain.as_str())
    } else if marker_is(marker, "pitch")
        || marker_is(marker, "pitch-accents")
        || marker_is(marker, "pitch-accent")
    {
        Some(ctx.pitch.as_str())
    } else if marker_is(marker, "pitch-graphs")
        || marker_is(marker, "pitch-graph")
        || marker_is(marker, "pitch-accent-graphs")
    {
        Some(ctx.pitch_graphs.as_str())
    } else if marker_is(marker, "pitch-positions")
        || marker_is(marker, "pitch-position")
        || marker_is(marker, "pitch-accent-positions")
    {
        Some(ctx.pitch_positions.as_str())
    } else if marker_is(marker, "pitch-categories") || marker_is(marker, "pitch-accent-categories")
    {
        Some(ctx.pitch_categories.as_str())
    } else if marker_is(marker, "character") {
        Some(ctx.character.as_str())
    } else if marker_is(marker, "onyomi") {
        Some(ctx.onyomi.as_str())
    } else if marker_is(marker, "kunyomi") {
        Some(ctx.kunyomi.as_str())
    } else if marker_is(marker, "stroke-count") {
        Some(ctx.stroke_count.as_str())
    } else {
        None
    };

    text_opt.map(marker_result).unwrap_or_default()
}

/// Classification used by `anki_media::prepare_note_media` when resolving
/// `{a | b}` alternative markers containing mixed media and text markers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MarkerEvalKind {
    Media,
    NonEmptyText,
    EmptyText,
    Unknown,
}

pub(crate) fn evaluate_template_marker_kind(
    marker: &Marker,
    ctx: &NoteFieldsContext,
    clipboard_cache: &mut Option<String>,
) -> MarkerEvalKind {
    if MEDIA_MARKERS.iter().any(|name| marker_is(marker, name)) {
        return MarkerEvalKind::Media;
    }
    if !KNOWN_MARKERS.iter().any(|name| marker_is(marker, name)) {
        return MarkerEvalKind::Unknown;
    }
    if marker_is(marker, "clipboard") && clipboard_cache.is_none() {
        *clipboard_cache = Some(
            ctx.clipboard
                .clone()
                .unwrap_or_else(read_clipboard_text),
        );
    }
    let clip_str = clipboard_cache.as_deref().unwrap_or("");
    if let Some(value) = common_marker_value(marker, ctx, clip_str) {
        return if value.is_empty() {
            MarkerEvalKind::EmptyText
        } else {
            MarkerEvalKind::NonEmptyText
        };
    }
    let glossary = format_context_glossary(ctx);
    let (tags_full, tags_brief) = resolve_context_tags(ctx);
    let pos = resolve_part_of_speech(ctx);
    let res = evaluate_specific_marker(
        "",
        marker,
        ctx,
        &glossary,
        &tags_full,
        &tags_brief,
        &pos,
    );
    if !res.handled {
        MarkerEvalKind::Unknown
    } else if res.text.is_empty() {
        MarkerEvalKind::EmptyText
    } else {
        MarkerEvalKind::NonEmptyText
    }
}

/// Maps Anki field templates through the same token parser and ordered
/// alternative-marker substitution used by the C++ note builder.
pub fn map_note_fields(
    field_templates: &HashMap<String, String>,
    ctx: &NoteFieldsContext,
) -> HashMap<String, String> {
    let glossary_out = format_context_glossary(ctx);
    let (tags_full, tags_brief) = resolve_context_tags(ctx);
    let pos = resolve_part_of_speech(ctx);
    let clipboard_text = match &ctx.clipboard {
        Some(text) => text.clone(),
        None if templates_reference_marker(field_templates.values(), "clipboard") => {
            read_clipboard_text()
        }
        None => String::new(),
    };
    map_note_fields_with_data(
        field_templates,
        ctx,
        &glossary_out,
        &tags_full,
        &tags_brief,
        &pos,
        &clipboard_text,
    )
}

fn map_note_fields_with_data(
    field_templates: &HashMap<String, String>,
    ctx: &NoteFieldsContext,
    glossary: &GlossaryOutput,
    tags_full: &str,
    tags_brief: &str,
    part_of_speech: &str,
    clipboard_text: &str,
) -> HashMap<String, String> {
    let mut result = HashMap::with_capacity(field_templates.len());
    for (field_name, template) in field_templates {
        let input: Vec<u16> = template.encode_utf16().collect();
        let output = substitute_markers(
            &input,
            |marker| {
                common_marker_value(marker, ctx, clipboard_text)
                    .map(marker_result)
                    .unwrap_or_default()
            },
            |marker| {
                evaluate_specific_marker(
                    field_name,
                    marker,
                    ctx,
                    glossary,
                    tags_full,
                    tags_brief,
                    part_of_speech,
                )
            },
        );
        result.insert(field_name.clone(), String::from_utf16_lossy(&output));
    }
    result
}

fn marker_requires_unavailable_data(field: &str, marker: &Marker) -> Option<NoteBuildError> {
    if !KNOWN_MARKERS.iter().any(|name| marker_is(marker, name)) {
        return Some(NoteBuildError::MissingMarkerData {
            field: field.to_owned(),
            marker: String::from_utf16_lossy(&marker.name),
        });
    }
    None
}

/// Profile markers for which this app currently has no source data.
pub fn unavailable_profile_markers(_profile: &AnkiNoteConfig) -> Vec<String> {
    Vec::new()
}

fn validate_template_markers(
    field: &str,
    template: &str,
    ctx: &NoteFieldsContext,
    glossary: &GlossaryOutput,
    tags_full: &str,
    tags_brief: &str,
    part_of_speech: &str,
    clipboard_text: &str,
) -> Result<(), NoteBuildError> {
    let input: Vec<u16> = template.encode_utf16().collect();
    for token in tokenize(&input) {
        for marker in &token.markers {
            if let Some(value) = common_marker_value(marker, ctx, clipboard_text) {
                if !value.is_empty() {
                    break;
                }
                continue;
            }
            if let Some(error) = marker_requires_unavailable_data(field, marker) {
                return Err(error);
            }
            let eval = evaluate_specific_marker(
                field,
                marker,
                ctx,
                glossary,
                tags_full,
                tags_brief,
                part_of_speech,
            );
            if eval.media || !eval.text.is_empty() {
                break;
            }
        }
    }
    Ok(())
}

/// Collects deduplicated dictionary media files referenced by glossary markers
/// in `field_templates`, honoring `{a | b}` alternative short-circuiting.
pub fn referenced_dictionary_media_files(
    field_templates: &HashMap<String, String>,
    ctx: &NoteFieldsContext,
) -> Vec<MediaFilePayload> {
    if ctx.glossary_definitions.is_empty() {
        return Vec::new();
    }
    let glossary_out = format_context_glossary(ctx);
    let (tags_full, tags_brief) = resolve_context_tags(ctx);
    let part_of_speech = resolve_part_of_speech(ctx);
    let clipboard_text = match &ctx.clipboard {
        Some(text) => text.clone(),
        None if templates_reference_marker(field_templates.values(), "clipboard") => {
            read_clipboard_text()
        }
        None => String::new(),
    };

    let mut seen_filenames = HashSet::new();
    let mut files = Vec::new();
    let mut sorted_fields: Vec<(&String, &String)> = field_templates.iter().collect();
    sorted_fields.sort_by_key(|(name, _)| *name);

    for (field, template) in sorted_fields {
        for token in tokenize(&template.encode_utf16().collect::<Vec<_>>()) {
            for marker in &token.markers {
                if let Some(value) = common_marker_value(marker, ctx, &clipboard_text) {
                    if !value.is_empty() {
                        break;
                    }
                    continue;
                }
                if marker_is(marker, "glossary")
                    || marker_is(marker, "glossary-brief")
                    || marker_is(marker, "glossary-compact")
                {
                    let filter = parse_glossary_dict_filter(marker);
                    let matching: Vec<_> = ctx
                        .glossary_definitions
                        .iter()
                        .filter(|def| match filter {
                            GlossaryDictFilter::Unfiltered => true,
                            GlossaryDictFilter::ById(id) => def.dictionary_id == id,
                            GlossaryDictFilter::Invalid => false,
                        })
                        .collect();
                    for def in &matching {
                        for media in &def.media_files {
                            if seen_filenames.insert(media.filename.clone()) {
                                files.push(media.clone());
                            }
                        }
                    }
                }
                let eval = evaluate_specific_marker(
                    field,
                    marker,
                    ctx,
                    &glossary_out,
                    &tags_full,
                    &tags_brief,
                    &part_of_speech,
                );
                if eval.media || !eval.text.is_empty() {
                    break;
                }
            }
        }
    }
    files
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

fn build_note_payload_from_profile_inner(
    profile: &AnkiNoteConfig,
    ctx: &NoteFieldsContext,
    require_captured_media_when_all_fields_empty: bool,
) -> Result<NotePayload, NoteBuildError> {
    if profile.field_templates.is_empty() {
        return Err(NoteBuildError::NoFieldTemplates);
    }
    let policy = AnkiDuplicatePolicy::try_from(profile.duplicate_policy)?;
    let glossary = format_context_glossary(ctx);
    let (tags_full, tags_brief) = resolve_context_tags(ctx);
    let part_of_speech = resolve_part_of_speech(ctx);
    let raw_clipboard = match &ctx.clipboard {
        Some(text) => text.clone(),
        None if templates_reference_marker(profile.field_templates.values(), "clipboard") => {
            read_clipboard_text()
        }
        None => String::new(),
    };

    for (field, template) in &profile.field_templates {
        validate_template_markers(
            field,
            template,
            ctx,
            &glossary,
            &tags_full,
            &tags_brief,
            &part_of_speech,
            &raw_clipboard,
        )?;
    }

    let mut text_context = ctx.clone();
    for value in [
        &mut text_context.sentence,
        &mut text_context.context,
        &mut text_context.selection,
        &mut text_context.secondary_subtitle,
        &mut text_context.secondary_context,
        &mut text_context.title,
        &mut text_context.cloze_prefix,
        &mut text_context.cloze_body,
        &mut text_context.cloze_suffix,
    ] {
        *value = value.replace('\n', &profile.newline_replacer);
    }
    let replaced_clipboard = raw_clipboard.replace('\n', &profile.newline_replacer);

    let payload = NotePayload {
        deck_name: profile.deck.clone(),
        model_name: profile.model.clone(),
        fields: map_note_fields_with_data(
            &profile.field_templates,
            &text_context,
            &glossary,
            &tags_full,
            &tags_brief,
            &part_of_speech,
            &replaced_clipboard,
        ),
        tags: profile.tags.clone(),
        options: policy.into(),
        audio: ctx.audio_attachments.clone(),
        picture: ctx.picture_attachments.clone(),
        video: ctx.video_attachments.clone(),
    };
    if require_captured_media_when_all_fields_empty {
        payload.validate().map_err(NoteBuildError::InvalidPayload)?;
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
    build_note_payload_from_profile_inner(profile, ctx, true)
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

    /// Checks whether a profile-backed note can be added according to Anki's
    /// duplicate policy (#340) via `AnkiClient::can_add_notes`.
    pub fn can_add_note_from_profile(
        &self,
        profile: &AnkiNoteConfig,
        ctx: &NoteFieldsContext,
    ) -> Result<bool, String> {
        let has_media_marker = profile.field_templates.values().any(|template| {
            tokenize(&template.encode_utf16().collect::<Vec<_>>())
                .into_iter()
                .any(|token| {
                    token
                        .markers
                        .iter()
                        .any(|m| MEDIA_MARKERS.iter().any(|name| marker_is(m, name)))
                })
        });
        let payload = build_note_payload_from_profile_inner(profile, ctx, !has_media_marker)
            .map_err(|error| error.to_string())?;
        let mut note = payload.to_value();
        if let Some(obj) = note.as_object_mut() {
            obj.remove("audio");
            obj.remove("picture");
            obj.remove("video");
        }
        let results = self
            .client
            .can_add_notes(&[note])
            .map_err(|error| error.to_string())?;
        Ok(results.first().copied().unwrap_or(false))
    }

    /// Builds and sends a profile-backed note, uploading any referenced
    /// dictionary media files via `store_media_files` BEFORE `add_note`.
    pub fn add_note_from_profile(
        &self,
        profile: &AnkiNoteConfig,
        ctx: &NoteFieldsContext,
    ) -> Result<i64, String> {
        let payload = self
            .create_note_payload_from_profile(profile, ctx)
            .map_err(|error| error.to_string())?;
        let dict_files = referenced_dictionary_media_files(&profile.field_templates, ctx);
        if !dict_files.is_empty() {
            self.client
                .store_media_files(&dict_files)
                .map_err(|error| error.to_string())?;
        }
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

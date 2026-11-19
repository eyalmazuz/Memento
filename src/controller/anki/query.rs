//! Anki duplicate inspection queries, browse query formatting, and batched `findNotes` RPCs.

use std::collections::{HashMap, HashSet};

use memento_net::anki::{AnkiAction, AnkiClient};

use crate::controller::config::AnkiNoteConfig;

use super::context::NoteFieldsContext;
use super::markers::{
    map_note_fields, template_references_any_marker, templates_reference_marker,
};
use super::payload::{AnkiDuplicatePolicy, NoteBuildError, NotePayload};

/// Maximum `findNotes` sub-actions packed into a single AnkiConnect `multi` RPC.
const FIND_NOTES_MULTI_BATCH_SIZE: usize = 64;

/// Per-entry Anki duplicate and existing card tag status (#340, #346).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EntryAnkiStatus {
    pub can_add: bool,
    pub is_duplicate: bool,
    pub note_ids: Vec<i64>,
    pub tags: Vec<String>,
    pub browse_query: String,
}

impl EntryAnkiStatus {
    /// Returns the primary existing Anki note ID for this entry, if present.
    pub fn primary_note_id(&self) -> Option<i64> {
        self.note_ids.first().copied()
    }
}

/// Escapes `\` and `"` for safe embedding inside Anki double-quoted search terms.
pub fn escape_anki_search_term(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            _ => out.push(ch),
        }
    }
    out
}

/// Formats one or more positive 64-bit Anki note IDs as `"nid:123"` or `"nid:123 or nid:456"`.
pub fn format_nid_browse_query(note_ids: &[i64]) -> String {
    let mut seen = HashSet::new();
    let mut parts = Vec::new();
    for &id in note_ids {
        if id > 0 && seen.insert(id) {
            parts.push(format!("nid:{id}"));
        }
    }
    parts.join(" or ")
}

/// Normalizes an Anki browse callback argument:
/// - All ASCII digits -> parses positive 64-bit ID into `"nid:{id}"`.
/// - Non-empty query string (e.g. `"nid:123 or nid:456"`) -> passed through trimmed.
pub fn normalize_anki_browse_target(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.bytes().all(|b| b.is_ascii_digit()) {
        let note_id = trimmed.parse::<i64>().ok().filter(|&id| id > 0)?;
        Some(format!("nid:{note_id}"))
    } else {
        Some(trimmed.to_owned())
    }
}

fn canonical_key_field_rank(field_name: &str) -> u8 {
    match field_name.trim().to_ascii_lowercase().as_str() {
        "expression" | "word" | "character" | "kanji" => 0,
        "front" | "term" => 1,
        _ => 2,
    }
}

pub(crate) fn select_primary_key_field(
    field_templates: &HashMap<String, String>,
) -> Option<String> {
    if field_templates.is_empty() {
        return None;
    }
    let mut sorted: Vec<(&String, &String)> = field_templates.iter().collect();
    sorted.sort_by(|a, b| a.0.cmp(b.0));

    if let Some((name, _)) = sorted
        .iter()
        .filter(|(_, tmpl)| matches!(tmpl.trim(), "{expression}" | "{character}"))
        .min_by_key(|(name, _)| (canonical_key_field_rank(name), *name))
    {
        return Some((*name).clone());
    }
    for group in [
        &["expression", "character"][..],
        &["furigana-plain", "furigana"][..],
        &["reading"][..],
    ] {
        if let Some((name, _)) = sorted
            .iter()
            .filter(|(_, tmpl)| template_references_any_marker(tmpl, group))
            .min_by_key(|(name, _)| (canonical_key_field_rank(name), *name))
        {
            return Some((*name).clone());
        }
    }
    sorted.first().map(|(name, _)| (*name).clone())
}

/// Builds the Anki search query used by `findNotes` for `(expression, reading)` under `profile`.
pub fn build_entry_find_query(
    profile: &AnkiNoteConfig,
    expression: &str,
    reading: &str,
) -> Result<String, String> {
    let (_, query) = build_entry_probe_and_query(profile, expression, reading)?;
    Ok(query)
}

pub(crate) struct ProbeProfileMeta {
    pub(crate) policy: AnkiDuplicatePolicy,
    pub(crate) key_field: Option<String>,
    pub(crate) has_character: bool,
}

impl ProbeProfileMeta {
    pub(crate) fn from_profile(profile: &AnkiNoteConfig) -> Result<Self, String> {
        if profile.deck.trim().is_empty() {
            return Err("deckName cannot be empty".to_owned());
        }
        if profile.model.trim().is_empty() {
            return Err("modelName cannot be empty".to_owned());
        }
        if profile.field_templates.is_empty() {
            return Err(NoteBuildError::NoFieldTemplates.to_string());
        }
        let policy = AnkiDuplicatePolicy::try_from(profile.duplicate_policy)
            .map_err(|error| error.to_string())?;
        let key_field = select_primary_key_field(&profile.field_templates);
        let has_character =
            templates_reference_marker(profile.field_templates.values(), "character");
        Ok(Self {
            policy,
            key_field,
            has_character,
        })
    }
}

pub(crate) fn build_entry_probe_and_query_with_meta(
    profile: &AnkiNoteConfig,
    meta: &ProbeProfileMeta,
    expression: &str,
    reading: &str,
) -> (serde_json::Value, String) {
    let mut ctx = NoteFieldsContext::new(expression, reading, "");
    ctx.clipboard = Some(String::new());
    if meta.has_character {
        ctx.is_kanji = true;
        ctx.character = expression.to_owned();
    }

    let mut fields = map_note_fields(&profile.field_templates, &ctx);
    if fields.values().all(|v| v.trim().is_empty()) {
        if let Some(ref k) = meta.key_field {
            if !expression.trim().is_empty() {
                fields.insert(k.clone(), expression.to_owned());
            }
        }
    }

    let field_clause = if let Some((field_name, field_val)) = meta.key_field.as_ref().and_then(|k| {
        fields
            .get(k)
            .filter(|v| !v.trim().is_empty())
            .map(|v| (k, v))
    }) {
        let escaped_val = escape_anki_search_term(field_val);
        if field_name
            .chars()
            .any(|c| c.is_whitespace() || c == '"' || c == ':')
        {
            format!(
                "\"{}\":\"{escaped_val}\"",
                escape_anki_search_term(field_name)
            )
        } else {
            format!("{field_name}:\"{escaped_val}\"")
        }
    } else {
        format!("\"{}\"", escape_anki_search_term(expression))
    };

    let find_query = match meta.policy {
        AnkiDuplicatePolicy::None => field_clause,
        AnkiDuplicatePolicy::DifferentDeck | AnkiDuplicatePolicy::SameDeck => {
            format!(
                "deck:\"{}\" {field_clause}",
                escape_anki_search_term(&profile.deck)
            )
        }
    };

    let payload = NotePayload {
        deck_name: profile.deck.clone(),
        model_name: profile.model.clone(),
        fields,
        tags: profile.tags.clone(),
        options: meta.policy.into(),
        audio: Vec::new(),
        picture: Vec::new(),
        video: Vec::new(),
    };
    (payload.into_value(), find_query)
}

fn build_entry_probe_and_query(
    profile: &AnkiNoteConfig,
    expression: &str,
    reading: &str,
) -> Result<(serde_json::Value, String), String> {
    let meta = ProbeProfileMeta::from_profile(profile)?;
    Ok(build_entry_probe_and_query_with_meta(
        profile, &meta, expression, reading,
    ))
}

fn parse_positive_note_id(value: &serde_json::Value) -> Option<i64> {
    if let Some(id) = value.as_i64() {
        return (id > 0).then_some(id);
    }
    if let Some(id) = value.as_u64() {
        return i64::try_from(id).ok().filter(|&id| id > 0);
    }
    let number = value.as_f64()?;
    if number.is_finite()
        && number.fract() == 0.0
        && (1.0..=(1_u64 << 53) as f64).contains(&number)
    {
        return Some(number as i64);
    }
    None
}

fn parse_find_notes_subresult(value: &serde_json::Value) -> Result<Vec<i64>, String> {
    let arr_val = if let Some(obj) = value.as_object() {
        if let Some(err) = obj.get("error").filter(|v| !v.is_null()) {
            let msg = err.as_str().unwrap_or("AnkiConnect findNotes error");
            return Err(format!("AnkiConnect error: {msg}"));
        }
        obj.get("result")
            .ok_or_else(|| "AnkiConnect multi response item missing result".to_owned())?
    } else {
        value
    };
    let items = arr_val
        .as_array()
        .ok_or_else(|| "AnkiConnect multi findNotes item is not an array".to_owned())?;
    let mut ids = Vec::with_capacity(items.len());
    for item in items {
        let id = parse_positive_note_id(item)
            .ok_or_else(|| "AnkiConnect findNotes returned an invalid note ID".to_owned())?;
        ids.push(id);
    }
    Ok(ids)
}

/// Resolves one or more deduplicated `findNotes` queries against AnkiConnect.
///
/// Uses a direct `findNotes` RPC when only 1 query is needed and batches multiple
/// queries via AnkiConnect `multi` (`AnkiAction::Multi`) to avoid O(N) HTTP roundtrips.
pub(crate) fn batch_find_note_ids(
    client: &AnkiClient,
    queries: &[String],
) -> Result<Vec<Vec<i64>>, String> {
    if queries.is_empty() {
        return Ok(Vec::new());
    }
    if queries.len() == 1 {
        let ids = client
            .find_notes(&queries[0])
            .map_err(|error| error.to_string())?;
        return Ok(vec![ids]);
    }

    let mut all_results = Vec::with_capacity(queries.len());
    for chunk in queries.chunks(FIND_NOTES_MULTI_BATCH_SIZE) {
        if chunk.len() == 1 {
            let ids = client
                .find_notes(&chunk[0])
                .map_err(|error| error.to_string())?;
            all_results.push(ids);
            continue;
        }
        let actions: Vec<serde_json::Value> = chunk
            .iter()
            .map(|query| {
                serde_json::json!({
                    "action": AnkiAction::FindNotes.as_wire(),
                    "params": { "query": query },
                })
            })
            .collect();
        let reply = client
            .request(
                AnkiAction::Multi,
                Some(serde_json::json!({ "actions": actions })),
            )
            .map_err(|error| error.to_string())?;
        let entries = reply
            .as_array()
            .ok_or_else(|| "AnkiConnect multi response result is not an array".to_owned())?;
        if entries.len() != chunk.len() {
            return Err(format!(
                "AnkiConnect multi response length mismatch: expected {}, got {}",
                chunk.len(),
                entries.len()
            ));
        }
        for entry in entries {
            all_results.push(parse_find_notes_subresult(entry)?);
        }
    }
    Ok(all_results)
}

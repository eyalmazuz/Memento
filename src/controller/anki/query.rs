//! Anki duplicate inspection queries, browse query formatting, and batched `findNotes` RPCs.

use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock};

use subomi_net::anki::{AnkiAction, AnkiClient};

use crate::controller::config::AnkiNoteConfig;

use super::context::NoteFieldsContext;
use super::markers::{
    map_note_fields, template_references_any_marker, templates_reference_marker,
};
use super::payload::{AnkiDuplicatePolicy, NoteBuildError, NotePayload};

/// Maximum `findNotes` sub-actions packed into a single AnkiConnect `multi` RPC.
const FIND_NOTES_MULTI_BATCH_SIZE: usize = 64;

/// Maximum cached `(endpoint_url, model)` entries for `modelFieldNames`.
const MAX_MODEL_FIELD_CACHE_ENTRIES: usize = 128;

static MODEL_FIELD_NAMES_CACHE: OnceLock<Mutex<HashMap<(String, String), Vec<String>>>> =
    OnceLock::new();

fn model_field_names_cache() -> &'static Mutex<HashMap<(String, String), Vec<String>>> {
    MODEL_FIELD_NAMES_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Returns cached `modelFieldNames` for `(endpoint_url, model)` without issuing an RPC.
pub fn lookup_cached_model_field_names(endpoint_url: &str, model: &str) -> Option<Vec<String>> {
    if model.trim().is_empty() {
        return None;
    }
    let cache = model_field_names_cache()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    cache
        .get(&(endpoint_url.to_owned(), model.to_owned()))
        .cloned()
}

/// Queries and caches `AnkiClient::model_field_names(model)` per `(endpoint, model)`.
pub fn cached_model_field_names(
    client: &AnkiClient,
    model: &str,
) -> Result<Vec<String>, String> {
    let trimmed = model.trim();
    if trimmed.is_empty() {
        return Err("modelName cannot be empty".to_owned());
    }
    let key = (client.endpoint_url(), trimmed.to_owned());
    {
        let cache = model_field_names_cache()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(fields) = cache.get(&key) {
            return Ok(fields.clone());
        }
    }
    let fields = client
        .model_field_names(trimmed)
        .map_err(|error| error.to_string())?;
    if !fields.is_empty() {
        let mut cache = model_field_names_cache()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if cache.len() >= MAX_MODEL_FIELD_CACHE_ENTRIES && !cache.contains_key(&key) {
            cache.clear();
        }
        cache.insert(key, fields.clone());
    }
    Ok(fields)
}

/// Clears all cached `modelFieldNames` entries (e.g. when Anki profile settings change).
pub fn invalidate_model_field_names_cache() {
    let mut cache = model_field_names_cache()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    cache.clear();
}

/// Resolves the exact first/sort field name of `profile.model` via cached
/// `AnkiClient::model_field_names`, falling back to `select_primary_key_field`
/// only when `modelFieldNames` fails or returns an empty list.
pub fn resolve_model_first_field(
    client: &AnkiClient,
    profile: &AnkiNoteConfig,
) -> Option<String> {
    if let Ok(fields) = cached_model_field_names(client, &profile.model) {
        if let Some(first) = fields.into_iter().find(|f| !f.trim().is_empty()) {
            return Some(first);
        }
    }
    select_primary_key_field(&profile.field_templates)
}

/// Lookup descriptor for batched Anki duplicate checking, optionally flipping a term
/// to export its kana reading as `{expression}` (`readingAsExpression`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EntryAnkiLookup {
    pub expression: String,
    pub reading: String,
    pub reading_as_expression: bool,
}

impl EntryAnkiLookup {
    pub fn new(expression: impl Into<String>, reading: impl Into<String>) -> Self {
        Self {
            expression: expression.into(),
            reading: reading.into(),
            reading_as_expression: false,
        }
    }

    pub fn with_reading_as_expression(mut self, reading_as_expression: bool) -> Self {
        self.reading_as_expression = reading_as_expression;
        self
    }
}

/// Result of checking whether a term can be added in kanji (`expression`) and/or
/// kana (`reading`, i.e. `reading_as_expression = true`) form.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TermAddableStatus {
    pub expression: bool,
    pub reading: bool,
}

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

fn format_field_clause(field_name: &str, field_val: &str) -> String {
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
}

/// Builds the Anki search query used by `findNotes` for `(expression, reading)` under `profile`.
pub fn build_entry_find_query(
    profile: &AnkiNoteConfig,
    expression: &str,
    reading: &str,
) -> Result<String, String> {
    build_entry_find_query_with_options(profile, expression, reading, false)
}

/// Builds the Anki search query used by `findNotes` for `(expression, reading)` under `profile`,
/// optionally treating `reading` as `{expression}` (`reading_as_expression = true`).
pub fn build_entry_find_query_with_options(
    profile: &AnkiNoteConfig,
    expression: &str,
    reading: &str,
    reading_as_expression: bool,
) -> Result<String, String> {
    let (_, query) = build_entry_probe_and_query(profile, expression, reading, reading_as_expression)?;
    Ok(query)
}

/// Builds a `guiBrowse` duplicate query using the exact first/sort field `first_field`
/// (from `modelFieldNames`), adding `OR <first_field>:"<reading>"` whenever
/// `!reading.is_empty() && reading != expression` and the rendered reading value differs.
pub fn build_duplicate_browse_query_with_field(
    profile: &AnkiNoteConfig,
    first_field: &str,
    expression: &str,
    reading: &str,
) -> Result<String, String> {
    let mut meta = ProbeProfileMeta::from_profile(profile)?;
    if !first_field.trim().is_empty() {
        meta.key_field = Some(first_field.to_owned());
    }
    let key_field = meta.key_field.as_deref();
    let render_key_value = |reading_as_expression: bool| -> String {
        let mut ctx = NoteFieldsContext::new(expression, reading, "");
        ctx.clipboard = Some(String::new());
        ctx.reading_as_expression = reading_as_expression;
        if meta.has_character {
            ctx.is_kanji = true;
            ctx.character = expression.to_owned();
        }
        let fields = map_note_fields(&profile.field_templates, &ctx);
        key_field
            .and_then(|k| fields.get(k))
            .filter(|v| !v.trim().is_empty())
            .cloned()
            .unwrap_or_else(|| ctx.effective_expression().to_owned())
    };

    let expr_val = render_key_value(false);
    let expr_clause = match key_field {
        Some(field_name) => format_field_clause(field_name, &expr_val),
        None => format!("\"{}\"", escape_anki_search_term(&expr_val)),
    };

    let mut combined_clause = expr_clause;
    let mut has_or = false;
    if !meta.has_character && !reading.is_empty() && reading != expression {
        let read_val = render_key_value(true);
        if !read_val.trim().is_empty() && read_val != expr_val {
            let read_clause = match key_field {
                Some(field_name) => format_field_clause(field_name, &read_val),
                None => format!("\"{}\"", escape_anki_search_term(&read_val)),
            };
            combined_clause = format!("{combined_clause} OR {read_clause}");
            has_or = true;
        }
    }

    let query = match meta.policy {
        AnkiDuplicatePolicy::None => combined_clause,
        AnkiDuplicatePolicy::DifferentDeck | AnkiDuplicatePolicy::SameDeck => {
            if has_or {
                format!(
                    "deck:\"{}\" ({combined_clause})",
                    escape_anki_search_term(&profile.deck)
                )
            } else {
                format!(
                    "deck:\"{}\" {combined_clause}",
                    escape_anki_search_term(&profile.deck)
                )
            }
        }
    };
    Ok(query)
}

/// Queries and caches `AnkiClient::model_field_names(profile.model)` per `(endpoint, model)`
/// to determine the exact first/sort field for `guiBrowse` duplicate queries, supporting
/// `OR reading` when `expression != reading`.
pub fn build_duplicate_browse_query(
    client: &AnkiClient,
    profile: &AnkiNoteConfig,
    expression: &str,
    reading: &str,
) -> Result<String, String> {
    let first_field = resolve_model_first_field(client, profile).unwrap_or_default();
    build_duplicate_browse_query_with_field(profile, &first_field, expression, reading)
}

pub(crate) struct ProbeProfileMeta {
    pub(crate) policy: AnkiDuplicatePolicy,
    pub(crate) key_field: Option<String>,
    pub(crate) has_character: bool,
}

impl ProbeProfileMeta {
    pub(crate) fn from_profile(profile: &AnkiNoteConfig) -> Result<Self, String> {
        Self::from_profile_with_key_field(profile, None)
    }

    pub(crate) fn from_profile_with_key_field(
        profile: &AnkiNoteConfig,
        exact_first_field: Option<String>,
    ) -> Result<Self, String> {
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
        let key_field = exact_first_field
            .filter(|f| !f.trim().is_empty())
            .or_else(|| select_primary_key_field(&profile.field_templates));
        let has_character =
            templates_reference_marker(profile.field_templates.values(), "character");
        Ok(Self {
            policy,
            key_field,
            has_character,
        })
    }
}

pub(crate) fn build_entry_probe_and_query_with_options(
    profile: &AnkiNoteConfig,
    meta: &ProbeProfileMeta,
    expression: &str,
    reading: &str,
    reading_as_expression: bool,
) -> (serde_json::Value, String) {
    let mut ctx = NoteFieldsContext::new(expression, reading, "");
    ctx.clipboard = Some(String::new());
    ctx.reading_as_expression = reading_as_expression;
    if meta.has_character {
        ctx.is_kanji = true;
        ctx.character = expression.to_owned();
    }

    let effective_expr = ctx.effective_expression().to_owned();
    let mut fields = map_note_fields(&profile.field_templates, &ctx);
    if fields.values().all(|v| v.trim().is_empty()) {
        if let Some(ref k) = meta.key_field {
            if !effective_expr.trim().is_empty() {
                fields.insert(k.clone(), effective_expr.clone());
            }
        }
    }

    let field_clause = if let Some((field_name, field_val)) = meta.key_field.as_ref().and_then(|k| {
        fields
            .get(k)
            .filter(|v| !v.trim().is_empty())
            .map(|v| (k, v))
    }) {
        format_field_clause(field_name, field_val)
    } else {
        format!("\"{}\"", escape_anki_search_term(&effective_expr))
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
    reading_as_expression: bool,
) -> Result<(serde_json::Value, String), String> {
    let meta = ProbeProfileMeta::from_profile(profile)?;
    Ok(build_entry_probe_and_query_with_options(
        profile,
        &meta,
        expression,
        reading,
        reading_as_expression,
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
        // Filter out non-positive or null note IDs cleanly (Yomitan PR #2510).
        if item.is_null()
            || item.as_i64().is_some_and(|n| n <= 0)
            || item
                .as_f64()
                .is_some_and(|n| n.is_finite() && n.fract() == 0.0 && n <= 0.0)
        {
            continue;
        }
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
/// Every sub-action is built via [`AnkiClient::build_multi_action`] so `"key": api_key`
/// is included whenever `use_api_key` is enabled (Yomitan PR #2421).
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
                client.build_multi_action(
                    AnkiAction::FindNotes,
                    Some(serde_json::json!({ "query": query })),
                )
            })
            .collect();
        let reply = client.multi(actions).map_err(|error| error.to_string())?;
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

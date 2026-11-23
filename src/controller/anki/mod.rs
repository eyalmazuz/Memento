//! Anki controller connecting subomi-anki and subomi-net.

mod cloze;
mod context;
mod css_glossary;
mod markers;
mod payload;
mod query;
mod sqlite_enrich;

use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock};

use subomi_net::anki::{AnkiClient, AnkiEndpoint, MediaFilePayload};

use crate::controller::config::AnkiNoteConfig;

pub use self::cloze::{
    apply_subtitle_cloze, create_cloze_sentence, format_cloze, format_cloze_with_hint,
    format_expression_furigana, secondary_context_for_range, split_cloze, split_cloze_around,
};
pub use self::context::{
    filter_subtitle_text, read_clipboard_text, AnkiNoteContext, GlossaryDefinition,
    NoteFieldsContext, NoteTag,
};
pub use self::css_glossary::{
    clear_scoped_css_cache, format_glossary, format_glossary_html, scope_dictionary_css,
    scope_dictionary_css_cached, GlossaryOutput,
};
pub use self::markers::{
    map_note_fields, referenced_dictionary_media_files, unavailable_profile_markers,
};
pub use self::payload::{
    build_note_payload, build_note_payload_from_profile, build_note_payload_from_profile_owned,
    default_note_fields, merge_note_update, validate_json_payload, validate_note_payload,
    AnkiDuplicatePolicy, AnkiUpdateMode, MergedNoteUpdate, NoteBuildError, NoteOptions,
    NotePayload,
};
pub use self::query::{
    build_duplicate_browse_query, build_duplicate_browse_query_with_field, build_entry_find_query,
    build_entry_find_query_with_options, cached_model_field_names, escape_anki_search_term,
    format_nid_browse_query, invalidate_model_field_names_cache, lookup_cached_model_field_names,
    normalize_anki_browse_target, resolve_model_first_field, EntryAnkiLookup, EntryAnkiStatus,
    TermAddableStatus,
};
pub use self::sqlite_enrich::{
    populate_note_context_from_sqlite, populate_note_context_from_sqlite_ordered,
    populate_note_context_from_sqlite_with_hints, SelectedHitHint,
};

pub(crate) use self::context::encode_base64;
pub(crate) use self::markers::{
    evaluate_template_marker_kind, MarkerEvalKind, RenderedTemplateData,
};

use self::markers::referenced_dictionary_media_files_with_data;
use self::payload::{build_note_payload_from_profile_with_data, merge_note_update_with_attachments};
use self::query::{
    batch_find_note_ids, build_entry_probe_and_query_with_options, ProbeProfileMeta,
};

/// Result of creating or updating an Anki note alongside any non-fatal media warnings
/// collected when optional media (`{audio}`, `{audio-media}`, `{video}`, `{screenshot}`,
/// `{screenshot-video}`) failed to capture or no video was loaded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddNoteOutcome {
    pub note_id: i64,
    pub warnings: Vec<String>,
}

const MAX_UPLOADED_DICT_MEDIA_CACHE: usize = 4096;
static UPLOADED_DICT_MEDIA: OnceLock<Mutex<HashSet<(String, String)>>> = OnceLock::new();

fn uploaded_dict_media_cache() -> &'static Mutex<HashSet<(String, String)>> {
    UPLOADED_DICT_MEDIA.get_or_init(|| Mutex::new(HashSet::new()))
}

/// Clears the session cache of content-addressed dictionary media files already
/// stored in Anki via `storeMediaFile`.
pub fn clear_uploaded_media_cache() {
    let mut cache = uploaded_dict_media_cache()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    cache.clear();
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

    /// Owned variant of [`Self::add_note`] that moves media attachments into the
    /// AnkiConnect JSON payload without cloning.
    pub fn add_note_owned(&self, payload: NotePayload) -> Result<i64, String> {
        payload.validate()?;
        self.client
            .add_note(payload.into_value())
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
        let rendered = RenderedTemplateData::for_templates(&profile.field_templates, ctx);
        let payload = build_note_payload_from_profile_with_data(
            profile,
            ctx,
            &rendered,
            !rendered.has_media_marker,
            false,
        )
        .map_err(|error| error.to_string())?;
        let note = payload.into_value();
        let results = self
            .client
            .can_add_notes(&[note])
            .map_err(|error| error.to_string())?;
        Ok(results.first().copied().unwrap_or(false))
    }

    /// Queries AnkiConnect `findNotes` and returns matching 64-bit note IDs.
    pub fn find_note_ids(&self, query: &str) -> Result<Vec<i64>, String> {
        self.client
            .find_notes(query)
            .map_err(|error| error.to_string())
    }

    /// Fetches unique non-empty card tags across `note_ids` via AnkiConnect `notesInfo` (#346),
    /// filtering out non-positive IDs (`<= 0`) before sending `notesInfo` (Yomitan PR #2510).
    pub fn fetch_note_tags(&self, note_ids: &[i64]) -> Result<Vec<String>, String> {
        let mut valid_ids = Vec::with_capacity(note_ids.len());
        let mut seen_ids = HashSet::new();
        for &id in note_ids {
            if id > 0 && seen_ids.insert(id) {
                valid_ids.push(id);
            }
        }
        if valid_ids.is_empty() {
            return Ok(Vec::new());
        }
        let infos = self
            .client
            .notes_info(&valid_ids)
            .map_err(|error| error.to_string())?;
        let mut seen = HashSet::new();
        let mut tags = Vec::new();
        for info in infos {
            for raw_tag in info.tags {
                for tag in raw_tag.split_whitespace() {
                    if !tag.is_empty() && seen.insert(tag.to_owned()) {
                        tags.push(tag.to_owned());
                    }
                }
            }
        }
        Ok(tags)
    }

    /// Queries and caches `AnkiClient::model_field_names(model)` per `(endpoint, model)`.
    pub fn model_field_names(&self, model: &str) -> Result<Vec<String>, String> {
        cached_model_field_names(&self.client, model)
    }

    /// Builds a `guiBrowse` duplicate query using the model's exact first/sort field
    /// (cached via `modelFieldNames`), including `OR reading` when `expression != reading`.
    pub fn build_duplicate_browse_query(
        &self,
        profile: &AnkiNoteConfig,
        expression: &str,
        reading: &str,
    ) -> Result<String, String> {
        build_duplicate_browse_query(&self.client, profile, expression, reading)
    }

    /// Opens the Anki Browser (`guiBrowse`) showing existing duplicate cards for
    /// `(expression, reading)` using the exact first/sort field from cached `modelFieldNames`.
    pub fn open_duplicates(
        &self,
        profile: &AnkiNoteConfig,
        expression: &str,
        reading: &str,
    ) -> Result<Vec<i64>, String> {
        let query = self.build_duplicate_browse_query(profile, expression, reading)?;
        self.client
            .open_browse(&query)
            .map_err(|error| error.to_string())
    }

    /// Checks whether a single term can be added in kanji (`expression`) and/or kana
    /// (`reading`, i.e. `reading_as_expression = true`) form in a single `canAddNotes` RPC,
    /// matching C++ `AnkiClient::noteAddableAsync` (`kanjiKanaButton`).
    pub fn check_term_addable(
        &self,
        profile: &AnkiNoteConfig,
        expression: &str,
        reading: &str,
    ) -> Result<TermAddableStatus, String> {
        let cached_first =
            lookup_cached_model_field_names(&self.client.endpoint_url(), &profile.model)
                .and_then(|fields| fields.into_iter().find(|f| !f.trim().is_empty()));
        let meta = ProbeProfileMeta::from_profile_with_key_field(profile, cached_first)?;
        let (expr_probe, _) =
            build_entry_probe_and_query_with_options(profile, &meta, expression, reading, false);
        let check_reading = !meta.has_character && !reading.is_empty() && reading != expression;
        let mut probes = vec![expr_probe];
        if check_reading {
            let (read_probe, _) =
                build_entry_probe_and_query_with_options(profile, &meta, expression, reading, true);
            probes.push(read_probe);
        }
        let raw = self
            .client
            .can_add_notes(&probes)
            .map_err(|error| error.to_string())?;
        let can_add_expr = if meta.policy == AnkiDuplicatePolicy::SameDeck {
            true
        } else {
            raw.first().copied().unwrap_or(false)
        };
        let can_add_reading = if !check_reading {
            false
        } else if meta.policy == AnkiDuplicatePolicy::SameDeck {
            true
        } else {
            raw.get(1).copied().unwrap_or(false)
        };
        Ok(TermAddableStatus {
            expression: can_add_expr,
            reading: can_add_reading,
        })
    }

    /// Batches `canAddNotes` across `entries` (`(expression, reading)`), resolves matching
    /// `note_ids` via batched AnkiConnect `multi` / `findNotes` (respecting `duplicate_policy`:
    /// `deck:"..."` vs all decks, and `SameDeck` `allow_duplicate = true`), and optionally fetches
    /// unique card tags via a single batched `notesInfo` request when `show_tags` is true
    /// (#340, #346).
    pub fn check_entries_anki_status(
        &self,
        profile: &AnkiNoteConfig,
        entries: &[(String, String)],
        show_tags: bool,
    ) -> Result<Vec<EntryAnkiStatus>, String> {
        let lookups: Vec<EntryAnkiLookup> = entries
            .iter()
            .map(|(expression, reading)| EntryAnkiLookup::new(expression, reading))
            .collect();
        self.check_entries_anki_status_with_lookups(profile, &lookups, show_tags)
    }

    /// Like [`Self::check_entries_anki_status`], accepting [`EntryAnkiLookup`] items so
    /// entries flipped to `reading_as_expression = true` check kana duplicates cleanly.
    pub fn check_entries_anki_status_with_lookups(
        &self,
        profile: &AnkiNoteConfig,
        entries: &[EntryAnkiLookup],
        show_tags: bool,
    ) -> Result<Vec<EntryAnkiStatus>, String> {
        if entries.is_empty() {
            return Ok(Vec::new());
        }
        let cached_first =
            lookup_cached_model_field_names(&self.client.endpoint_url(), &profile.model)
                .and_then(|fields| fields.into_iter().find(|f| !f.trim().is_empty()));
        let used_cached_first = cached_first.is_some();
        let meta = ProbeProfileMeta::from_profile_with_key_field(profile, cached_first)?;
        let policy = meta.policy;
        let mut probes = Vec::with_capacity(entries.len());
        let mut find_queries = Vec::with_capacity(entries.len());
        for entry in entries {
            let (probe, query) = build_entry_probe_and_query_with_options(
                profile,
                &meta,
                &entry.expression,
                &entry.reading,
                entry.reading_as_expression,
            );
            probes.push(probe);
            find_queries.push(query);
        }

        let raw_can_add = self
            .client
            .can_add_notes(&probes)
            .map_err(|error| error.to_string())?;

        let mut can_add_vec = Vec::with_capacity(entries.len());
        let mut unique_queries: Vec<String> = Vec::new();
        let mut unique_query_set: HashSet<&str> = HashSet::new();

        for idx in 0..entries.len() {
            let can_add = if policy == AnkiDuplicatePolicy::SameDeck {
                true
            } else {
                raw_can_add.get(idx).copied().unwrap_or(false)
            };
            can_add_vec.push(can_add);

            if !can_add || (show_tags && policy == AnkiDuplicatePolicy::SameDeck) {
                let query = find_queries[idx].as_str();
                if unique_query_set.insert(query) {
                    unique_queries.push(query.to_owned());
                }
            }
        }

        let found_lists = batch_find_note_ids(&self.client, &unique_queries)?;
        let mut query_cache: HashMap<String, Vec<i64>> =
            HashMap::with_capacity(unique_queries.len());
        for (query, ids) in unique_queries.into_iter().zip(found_lists) {
            query_cache.insert(query, ids);
        }

        let mut note_ids_vec: Vec<Vec<i64>> = vec![Vec::new(); entries.len()];
        for idx in 0..entries.len() {
            let can_add = can_add_vec[idx];
            if !can_add || (show_tags && policy == AnkiDuplicatePolicy::SameDeck) {
                if let Some(ids) = query_cache.get(&find_queries[idx]) {
                    note_ids_vec[idx] = ids.clone();
                }
            }
        }

        // If Anki reported a duplicate (`!can_add`) whose initial `findNotes` returned empty,
        // query & cache `modelFieldNames(model)` to obtain the exact first/sort field (and
        // include `OR reading` when `expression != reading`) so duplicate resolution and
        // `guiBrowse` queries never rely solely on field-name heuristics.
        let unresolved_dup_indices: Vec<usize> = (0..entries.len())
            .filter(|&idx| !can_add_vec[idx] && note_ids_vec[idx].is_empty())
            .collect();
        if !unresolved_dup_indices.is_empty() {
            let exact_first = if used_cached_first {
                meta.key_field.clone()
            } else {
                cached_model_field_names(&self.client, &profile.model)
                    .ok()
                    .and_then(|fields| fields.into_iter().find(|f| !f.trim().is_empty()))
                    .or_else(|| meta.key_field.clone())
            };
            if let Some(first_field) = exact_first {
                let mut retry_queries: Vec<String> = Vec::new();
                let mut retry_set: HashSet<String> = HashSet::new();
                let mut per_idx_retry: Vec<(usize, String)> = Vec::new();
                for idx in unresolved_dup_indices {
                    if let Ok(q) = build_duplicate_browse_query_with_field(
                        profile,
                        &first_field,
                        &entries[idx].expression,
                        &entries[idx].reading,
                    ) {
                        if q != find_queries[idx] {
                            find_queries[idx] = q.clone();
                            if retry_set.insert(q.clone()) {
                                retry_queries.push(q.clone());
                            }
                            per_idx_retry.push((idx, q));
                        }
                    }
                }
                if !retry_queries.is_empty() {
                    if let Ok(retry_lists) = batch_find_note_ids(&self.client, &retry_queries) {
                        let mut retry_map: HashMap<String, Vec<i64>> =
                            HashMap::with_capacity(retry_queries.len());
                        for (q, ids) in retry_queries.into_iter().zip(retry_lists) {
                            retry_map.insert(q, ids);
                        }
                        for (idx, q) in per_idx_retry {
                            if let Some(ids) = retry_map.get(&q) {
                                note_ids_vec[idx] = ids.clone();
                            }
                        }
                    }
                }
            }
        }

        let mut tags_by_nid: HashMap<i64, Vec<String>> = HashMap::new();
        if show_tags {
            let mut all_unique_ids = Vec::new();
            let mut seen_ids = HashSet::new();
            for ids in &note_ids_vec {
                for &id in ids {
                    if id > 0 && seen_ids.insert(id) {
                        all_unique_ids.push(id);
                    }
                }
            }
            if !all_unique_ids.is_empty() {
                let infos = self
                    .client
                    .notes_info(&all_unique_ids)
                    .map_err(|error| error.to_string())?;
                for info in infos {
                    tags_by_nid.insert(info.note_id, info.tags);
                }
            }
        }

        let mut statuses = Vec::with_capacity(entries.len());
        for idx in 0..entries.len() {
            let can_add = can_add_vec[idx];
            let is_duplicate = !can_add;
            let note_ids = std::mem::take(&mut note_ids_vec[idx]);
            let mut tags = Vec::new();
            if show_tags && !note_ids.is_empty() {
                let mut seen_tags = HashSet::new();
                for &nid in &note_ids {
                    if let Some(raw_tags) = tags_by_nid.get(&nid) {
                        for raw_tag in raw_tags {
                            for tag in raw_tag.split_whitespace() {
                                if !tag.is_empty() && seen_tags.insert(tag.to_owned()) {
                                    tags.push(tag.to_owned());
                                }
                            }
                        }
                    }
                }
            }
            let browse_query = if !note_ids.is_empty() {
                format_nid_browse_query(&note_ids)
            } else if is_duplicate {
                find_queries[idx].clone()
            } else {
                String::new()
            };
            statuses.push(EntryAnkiStatus {
                can_add,
                is_duplicate,
                note_ids,
                tags,
                browse_query,
            });
        }
        Ok(statuses)
    }

    fn store_uncached_dict_media(&self, dict_files: Vec<MediaFilePayload>) -> Result<(), String> {
        if dict_files.is_empty() {
            return Ok(());
        }
        let endpoint_key = self.client.endpoint_url();
        let uncached: Vec<MediaFilePayload> = {
            let cache = uploaded_dict_media_cache()
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            dict_files
                .into_iter()
                .filter(|file| {
                    file.filename.is_empty()
                        || !cache.contains(&(endpoint_key.clone(), file.filename.clone()))
                })
                .collect()
        };
        if uncached.is_empty() {
            return Ok(());
        }
        self.client
            .store_media_files(&uncached)
            .map_err(|error| error.to_string())?;
        let mut cache = uploaded_dict_media_cache()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if cache.len().saturating_add(uncached.len()) > MAX_UPLOADED_DICT_MEDIA_CACHE {
            cache.clear();
        }
        for file in uncached {
            if !file.filename.is_empty() {
                cache.insert((endpoint_key.clone(), file.filename));
            }
        }
        Ok(())
    }

    /// Builds and sends a profile-backed note, uploading any referenced
    /// dictionary media files via `store_media_files` BEFORE `add_note`.
    pub fn add_note_from_profile(
        &self,
        profile: &AnkiNoteConfig,
        ctx: &NoteFieldsContext,
    ) -> Result<i64, String> {
        let rendered = RenderedTemplateData::for_templates(&profile.field_templates, ctx);
        let payload =
            build_note_payload_from_profile_with_data(profile, ctx, &rendered, true, true)
                .map_err(|error| error.to_string())?;
        let dict_files =
            referenced_dictionary_media_files_with_data(&profile.field_templates, ctx, &rendered);
        self.store_uncached_dict_media(dict_files)?;
        self.client
            .add_note(payload.into_value())
            .map_err(|error| error.to_string())
    }

    /// Owned variant of [`Self::add_note_from_profile`] that moves multi-MB media
    /// attachments out of `ctx` into AnkiConnect without cloning them.
    pub fn add_note_from_profile_owned(
        &self,
        profile: &AnkiNoteConfig,
        mut ctx: NoteFieldsContext,
    ) -> Result<i64, String> {
        let rendered = RenderedTemplateData::for_templates(&profile.field_templates, &ctx);
        let mut payload =
            build_note_payload_from_profile_with_data(profile, &ctx, &rendered, true, false)
                .map_err(|error| error.to_string())?;
        let dict_files =
            referenced_dictionary_media_files_with_data(&profile.field_templates, &ctx, &rendered);
        self.store_uncached_dict_media(dict_files)?;
        payload.audio = std::mem::take(&mut ctx.audio_attachments);
        payload.picture = std::mem::take(&mut ctx.picture_attachments);
        payload.video = std::mem::take(&mut ctx.video_attachments);
        self.client
            .add_note(payload.into_value())
            .map_err(|error| error.to_string())
    }

    /// Builds and sends a profile-backed note, returning the created note ID together
    /// with any non-fatal media warnings recorded on `ctx.media_warnings`.
    pub fn add_note_from_profile_with_warnings(
        &self,
        profile: &AnkiNoteConfig,
        ctx: &NoteFieldsContext,
    ) -> Result<AddNoteOutcome, String> {
        let note_id = self.add_note_from_profile(profile, ctx)?;
        Ok(AddNoteOutcome {
            note_id,
            warnings: ctx.media_warnings.clone(),
        })
    }

    /// Prepares optional media via `prepare_note_media` (collecting non-fatal warnings
    /// instead of aborting on failed/unavailable media) and submits the note to AnkiConnect.
    pub fn add_note_from_profile_with_media(
        &self,
        profile: &AnkiNoteConfig,
        ctx: &mut NoteFieldsContext,
        media_input: crate::controller::anki_media::NoteMediaInput,
    ) -> Result<AddNoteOutcome, String> {
        let rendered = RenderedTemplateData::for_templates(&profile.field_templates, ctx);
        crate::controller::anki_media::prepare_note_media_with_rendered_and_screenshots(
            profile,
            ctx,
            &rendered,
            media_input,
            crate::controller::anki_media::PendingScreenshots::default(),
        )?;
        let mut payload =
            build_note_payload_from_profile_with_data(profile, ctx, &rendered, true, false)
                .map_err(|error| error.to_string())?;
        let dict_files =
            referenced_dictionary_media_files_with_data(&profile.field_templates, ctx, &rendered);
        self.store_uncached_dict_media(dict_files)?;
        payload.audio = std::mem::take(&mut ctx.audio_attachments);
        payload.picture = std::mem::take(&mut ctx.picture_attachments);
        payload.video = std::mem::take(&mut ctx.video_attachments);
        let note_id = self
            .client
            .add_note(payload.into_value())
            .map_err(|error| error.to_string())?;
        Ok(AddNoteOutcome {
            note_id,
            warnings: ctx.media_warnings.clone(),
        })
    }

    /// Fast owned pipeline used by the UI note-creation worker: renders templates once,
    /// waits on pending/lazy-fallback screenshots in parallel with audio capture & encoding,
    /// skips re-uploading already-stored dictionary assets, and moves media payloads without cloning.
    pub(crate) fn add_note_from_profile_with_media_and_screenshots(
        &self,
        profile: &AnkiNoteConfig,
        mut ctx: NoteFieldsContext,
        media_input: crate::controller::anki_media::NoteMediaInput,
        pending_screenshots: crate::controller::anki_media::PendingScreenshots,
    ) -> Result<AddNoteOutcome, String> {
        let rendered = RenderedTemplateData::for_templates(&profile.field_templates, &ctx);
        crate::controller::anki_media::prepare_note_media_with_rendered_and_screenshots(
            profile,
            &mut ctx,
            &rendered,
            media_input,
            pending_screenshots,
        )?;
        let mut payload =
            build_note_payload_from_profile_with_data(profile, &ctx, &rendered, true, false)
                .map_err(|error| error.to_string())?;
        let dict_files =
            referenced_dictionary_media_files_with_data(&profile.field_templates, &ctx, &rendered);
        self.store_uncached_dict_media(dict_files)?;
        payload.audio = std::mem::take(&mut ctx.audio_attachments);
        payload.picture = std::mem::take(&mut ctx.picture_attachments);
        payload.video = std::mem::take(&mut ctx.video_attachments);
        let note_id = self
            .client
            .add_note(payload.into_value())
            .map_err(|error| error.to_string())?;
        Ok(AddNoteOutcome {
            note_id,
            warnings: ctx.media_warnings,
        })
    }

    /// Updates an existing Anki note (`note_id`) using `profile` and `ctx` according to `mode`.
    ///
    /// Fetches existing fields and tags via `notesInfo`, merges fields and media attachments
    /// cleanly (never wiping populated fields with empty values and preserving the key/sort field
    /// when unchanged), uploads any referenced glossary dictionary media via `storeMediaFile`,
    /// calls `updateNoteFields`, and attaches missing profile tags via `addTags`.
    pub fn update_existing_note(
        &self,
        note_id: i64,
        profile: &AnkiNoteConfig,
        ctx: &NoteFieldsContext,
        mode: AnkiUpdateMode,
    ) -> Result<i64, String> {
        if note_id <= 0 {
            return Err("Anki note ID must be a positive integer".to_owned());
        }
        let rendered = RenderedTemplateData::for_templates(&profile.field_templates, ctx);
        let infos = self
            .client
            .notes_info(&[note_id])
            .map_err(|error| error.to_string())?;
        self.apply_note_update_with_rendered(note_id, profile, ctx, &rendered, infos, mode)
    }

    fn apply_note_update_with_rendered(
        &self,
        note_id: i64,
        profile: &AnkiNoteConfig,
        ctx: &NoteFieldsContext,
        rendered: &RenderedTemplateData,
        infos: Vec<subomi_net::anki::AnkiNoteInfo>,
        mode: AnkiUpdateMode,
    ) -> Result<i64, String> {
        let payload =
            build_note_payload_from_profile_with_data(profile, ctx, rendered, true, false)
                .map_err(|error| error.to_string())?;
        let existing = infos
            .into_iter()
            .find(|info| info.note_id == note_id)
            .ok_or_else(|| format!("Anki note {note_id} was not found"))?;

        let merged = merge_note_update_with_attachments(
            &existing,
            &payload.fields,
            &payload.tags,
            &ctx.audio_attachments,
            &ctx.picture_attachments,
            &ctx.video_attachments,
            profile,
            mode,
        );

        if !merged.fields.is_empty() {
            let dict_files: Vec<MediaFilePayload> = referenced_dictionary_media_files_with_data(
                &profile.field_templates,
                ctx,
                rendered,
            )
            .into_iter()
            .filter(|file| {
                merged.fields.values().any(|val| {
                    val.as_str()
                        .is_some_and(|text| text.contains(&file.filename))
                })
            })
            .collect();
            self.store_uncached_dict_media(dict_files)?;
        }

        if merged.has_field_or_media_changes() {
            self.client
                .update_note_fields(
                    note_id,
                    merged.fields,
                    (!merged.audio.is_empty()).then_some(merged.audio.as_slice()),
                    (!merged.picture.is_empty()).then_some(merged.picture.as_slice()),
                    (!merged.video.is_empty()).then_some(merged.video.as_slice()),
                )
                .map_err(|error| error.to_string())?;
        }

        if !merged.new_tags.is_empty() {
            self.client
                .add_tags(&[note_id], &merged.new_tags.join(" "))
                .map_err(|error| error.to_string())?;
        }

        Ok(note_id)
    }

    /// Captures configured media (`{audio}`, `{audio-media}`, `{audio-context}`, `{screenshot}`,
    /// `{screenshot-video}`, `{video}`) via `prepare_note_media`, then updates the existing note.
    pub fn update_existing_note_with_media(
        &self,
        note_id: i64,
        profile: &AnkiNoteConfig,
        ctx: &mut NoteFieldsContext,
        media_input: crate::controller::anki_media::NoteMediaInput,
        mode: AnkiUpdateMode,
    ) -> Result<i64, String> {
        Ok(self
            .update_existing_note_with_media_outcome(note_id, profile, ctx, media_input, mode)?
            .note_id)
    }

    /// Captures configured media (collecting non-fatal media warnings cleanly) and updates
    /// the existing note, returning both the updated note ID and any non-fatal media warnings.
    pub fn update_existing_note_with_media_outcome(
        &self,
        note_id: i64,
        profile: &AnkiNoteConfig,
        ctx: &mut NoteFieldsContext,
        media_input: crate::controller::anki_media::NoteMediaInput,
        mode: AnkiUpdateMode,
    ) -> Result<AddNoteOutcome, String> {
        self.update_existing_note_with_media_and_screenshots(
            note_id,
            profile,
            ctx,
            media_input,
            crate::controller::anki_media::PendingScreenshots::default(),
            mode,
        )
    }

    /// Fast update pipeline: renders templates once, overlaps AnkiConnect `notesInfo` with
    /// concurrent media capture & deferred screenshot resolution, skips already-uploaded
    /// dictionary media, and pushes merged field/tag updates.
    pub(crate) fn update_existing_note_with_media_and_screenshots(
        &self,
        note_id: i64,
        profile: &AnkiNoteConfig,
        ctx: &mut NoteFieldsContext,
        media_input: crate::controller::anki_media::NoteMediaInput,
        pending_screenshots: crate::controller::anki_media::PendingScreenshots,
        mode: AnkiUpdateMode,
    ) -> Result<AddNoteOutcome, String> {
        if note_id <= 0 {
            return Err("Anki note ID must be a positive integer".to_owned());
        }
        let rendered = RenderedTemplateData::for_templates(&profile.field_templates, ctx);
        let (media_res, infos_res) = std::thread::scope(|scope| {
            let infos_handle = scope.spawn(|| {
                self.client
                    .notes_info(&[note_id])
                    .map_err(|error| error.to_string())
            });
            let media_res =
                crate::controller::anki_media::prepare_note_media_with_rendered_and_screenshots(
                    profile,
                    ctx,
                    &rendered,
                    media_input,
                    pending_screenshots,
                );
            let infos_res = infos_handle
                .join()
                .unwrap_or_else(|_| Err("Anki notesInfo worker panicked".to_owned()));
            (media_res, infos_res)
        });
        media_res?;
        let infos = infos_res?;
        let updated_id =
            self.apply_note_update_with_rendered(note_id, profile, ctx, &rendered, infos, mode)?;
        Ok(AddNoteOutcome {
            note_id: updated_id,
            warnings: ctx.media_warnings.clone(),
        })
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::time::Duration;

    fn read_http_json_body(stream: &mut TcpStream) -> serde_json::Value {
        let mut raw = Vec::new();
        let mut buf = [0u8; 4096];
        let header_end = loop {
            let n = stream.read(&mut buf).unwrap();
            assert!(n > 0, "unexpected EOF reading HTTP headers");
            raw.extend_from_slice(&buf[..n]);
            if let Some(pos) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
                break pos + 4;
            }
        };
        let header_str = String::from_utf8_lossy(&raw[..header_end]);
        let content_length: usize = header_str
            .lines()
            .find_map(|line| {
                let (name, val) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| val.trim().parse::<usize>().ok())
                    .flatten()
            })
            .unwrap_or(0);
        while raw.len() < header_end + content_length {
            let n = stream.read(&mut buf).unwrap();
            if n == 0 {
                break;
            }
            raw.extend_from_slice(&buf[..n]);
        }
        serde_json::from_slice(&raw[header_end..header_end + content_length]).unwrap()
    }

    #[test]
    fn check_entries_anki_status_batches_multiple_find_notes_via_multi() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let mut actions = Vec::new();
            for step in 0..3 {
                let (mut stream, _) = listener.accept().unwrap();
                let body = read_http_json_body(&mut stream);
                actions.push(body["action"].as_str().unwrap().to_owned());
                let reply = match step {
                    0 => serde_json::json!({ "error": null, "result": [false, true, false] })
                        .to_string(),
                    1 => {
                        let sub = body["params"]["actions"].as_array().unwrap();
                        assert_eq!(sub.len(), 2);
                        assert_eq!(sub[0]["action"], "findNotes");
                        assert_eq!(sub[1]["action"], "findNotes");
                        serde_json::json!({
                            "error": null,
                            "result": [
                                [1669909161001_i64],
                                [1669909161002_i64, 1669909161003_i64]
                            ]
                        })
                        .to_string()
                    }
                    _ => serde_json::json!({
                        "error": null,
                        "result": [
                            { "noteId": 1669909161001_i64, "modelName": "Lapis", "tags": ["t1"], "fields": {} },
                            { "noteId": 1669909161002_i64, "modelName": "Lapis", "tags": ["t2"], "fields": {} },
                            { "noteId": 1669909161003_i64, "modelName": "Lapis", "tags": ["t3"], "fields": {} }
                        ]
                    })
                    .to_string(),
                };
                let http = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{reply}",
                    reply.len()
                );
                stream.write_all(http.as_bytes()).unwrap();
            }
            actions
        });

        let profile = AnkiNoteConfig {
            enabled: true,
            endpoint: AnkiEndpoint {
                hostname: "http://127.0.0.1".into(),
                port,
                api_key: None,
                timeout: Some(Duration::from_secs(2)),
                use_api_key: false,
            },
            deck: "Japanese".into(),
            model: "Lapis".into(),
            field_templates: HashMap::from([("Expression".into(), "{expression}".into())]),
            tags: Vec::new(),
            duplicate_policy: 1,
            screenshot_type: 0,
            audio_pad_start: 0.0,
            audio_pad_end: 0.0,
            audio_normalize: false,
            audio_db: -20.0,
            newline_replacer: "<br>".into(),
        };
        let ctrl = AnkiController::new(profile.endpoint());
        let entries = vec![
            ("食べる".to_owned(), "たべる".to_owned()),
            ("未知語".to_owned(), "みちご".to_owned()),
            ("飲む".to_owned(), "のむ".to_owned()),
        ];
        let statuses = ctrl
            .check_entries_anki_status(&profile, &entries, true)
            .unwrap();
        assert_eq!(
            server.join().unwrap(),
            vec!["canAddNotes", "multi", "notesInfo"]
        );
        assert_eq!(statuses[0].note_ids, vec![1669909161001]);
        assert_eq!(statuses[0].tags, vec!["t1"]);
        assert!(statuses[1].can_add);
        assert_eq!(statuses[2].note_ids, vec![1669909161002, 1669909161003]);
        assert_eq!(statuses[2].tags, vec!["t2", "t3"]);
    }

    #[test]
    fn sqlite_enrichment_honors_dictionary_order_and_exact_rowid_matching() {
        let dir = tempfile::Builder::new()
            .prefix("subomi-anki-order-")
            .tempdir()
            .unwrap();
        let db_path = dir.path().join("dictionaries.sqlite");
        subomi_dict::yomidbbuilder::prepare_db(&db_path).unwrap();

        let conn = subomi_dict::sqlite_db::open_read_write_connection(&db_path).unwrap();
        conn.execute(
            "INSERT INTO directory (dic_id, title, format, revision, sequenced) VALUES \
             (1, 'AlphaDict', 3, '1', 1), \
             (2, 'BetaDict', 3, '1', 1);",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO term_bank (dic_id, expression, reading, def_tags, rules, score, glossary, sequence, term_tags) VALUES \
             (1, '橋', 'はし', '', '', 100, '[\"alpha-first\"]', 1, ''), \
             (1, '橋', 'はし', '', '', 100, '[\"alpha-second\"]', 1, ''), \
             (2, '橋', 'はし', '', '', 100, '[\"beta-first\"]', 1, '');",
            [],
        )
        .unwrap();
        let rowid_alpha_second: i64 = conn
            .query_row(
                "SELECT rowid FROM term_bank WHERE dic_id = 1 AND glossary LIKE '%alpha-second%';",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let rowid_beta: i64 = conn
            .query_row(
                "SELECT rowid FROM term_bank WHERE dic_id = 2;",
                [],
                |row| row.get(0),
            )
            .unwrap();
        conn.execute(
            "INSERT INTO term_meta_bank (dic_id, expression, mode, type, data) VALUES \
             (1, '橋', 'freq', 4, '{\"reading\":\"はし\",\"frequency\":500}'), \
             (2, '橋', 'freq', 4, '{\"reading\":\"はし\",\"frequency\":100}');",
            [],
        )
        .unwrap();
        drop(conn);

        // 1. Dictionary order puts BetaDict (2) before AlphaDict (1).
        let mut ctx_ordered = NoteFieldsContext::new("橋", "はし", "橋を渡る。")
            .with_dictionary_order(HashMap::from([(2, 0), (1, 1)]));
        populate_note_context_from_sqlite(&mut ctx_ordered, &db_path, &[], &[]).unwrap();
        assert_eq!(ctx_ordered.glossary_definitions.len(), 3);
        assert_eq!(ctx_ordered.glossary_definitions[0].dictionary_id, 2);
        assert_eq!(ctx_ordered.glossary_definitions[1].dictionary_id, 1);
        assert_eq!(ctx_ordered.frequency_entries[0].0, "BetaDict");
        assert_eq!(ctx_ordered.frequency_entries[1].0, "AlphaDict");

        // 2. Exact `(dic_id, rowid)` selection picks only `(2, rowid_beta)` then `(1, rowid_alpha_second)`.
        let mut ctx_rowids = NoteFieldsContext::new("橋", "はし", "橋を渡る。")
            .with_selected_rowids(vec![(2, rowid_beta), (1, rowid_alpha_second)]);
        populate_note_context_from_sqlite(&mut ctx_rowids, &db_path, &[], &[]).unwrap();
        assert_eq!(ctx_rowids.glossary_definitions.len(), 2);
        assert_eq!(ctx_rowids.glossary_definitions[0].dictionary_id, 2);
        assert_eq!(ctx_rowids.glossary_definitions[0].rowid, Some(rowid_beta));
        assert!(ctx_rowids.glossary_definitions[0].glossary.contains("beta-first"));
        assert_eq!(ctx_rowids.glossary_definitions[1].dictionary_id, 1);
        assert_eq!(
            ctx_rowids.glossary_definitions[1].rowid,
            Some(rowid_alpha_second)
        );
        assert!(ctx_rowids.glossary_definitions[1]
            .glossary
            .contains("alpha-second"));
    }

    #[test]
    fn rendered_template_data_selectively_builds_only_referenced_glossary_variants() {
        let ctx = NoteFieldsContext::new("食べる", "たべる", "食べる。")
            .with_dictionary("JMdict")
            .with_glossary_item("to eat");
        let templates = HashMap::from([("Def".into(), "{glossary-brief}".into())]);
        let rendered = RenderedTemplateData::for_templates(&templates, &ctx);
        assert!(rendered.glossary.glossary.is_empty());
        assert!(!rendered.glossary.glossary_brief.is_empty());
        assert!(rendered.glossary.glossary_compact.is_empty());
    }

    #[test]
    fn note_payload_into_value_and_to_json_string_match_to_value() {
        let mut payload = NotePayload::new("Japanese", "Lapis")
            .with_field("Expression", "食べる")
            .with_tag("subomi")
            .with_options(false, Some("deck".into()));
        payload.audio.push(serde_json::json!({
            "data": "QUFB",
            "filename": "a.mp3",
            "fields": ["Audio"]
        }));
        let from_ref = payload.to_value();
        let from_json: serde_json::Value =
            serde_json::from_str(&payload.to_json_string()).unwrap();
        let from_owned = payload.into_value();
        assert_eq!(from_ref, from_json);
        assert_eq!(from_ref, from_owned);
    }

    #[test]
    fn no_double_html_escaping_and_safe_css_and_cloze_slicing() {
        let ctx = NoteFieldsContext::new("食", "", "食")
            .with_dictionary("A & B <Dict>")
            .with_glossary_item("eat & drink")
            .with_tag_note("tag<1>", "misc", "note & \"quote\"");
        let mapped = map_note_fields(
            &HashMap::from([
                ("Glossary".into(), "{glossary}".into()),
                ("Tags".into(), "{tags}".into()),
            ]),
            &ctx,
        );
        assert!(!mapped["Glossary"].contains("&amp;amp;"));
        assert!(mapped["Glossary"].contains("A &amp; B &lt;Dict&gt;"));
        assert!(!mapped["Tags"].contains("&amp;amp;"));
        assert!(mapped["Tags"].contains("tag&lt;1&gt;: note &amp; &quot;quote&quot;"));

        // Safe CSS scoping when backslash escapes a multi-byte UTF-8 character inside quotes.
        let scoped = scope_dictionary_css(".x::before { content: \"\\日\"; }</STYLE>", "Dict");
        assert!(scoped.contains("[data-dictionary=\"Dict\"] .x::before"));
        assert!(!scoped.to_ascii_lowercase().contains("</style>"));

        // Out-of-bounds `active_pos_in_selected` must not panic.
        let mut cloze_ctx = NoteFieldsContext::new("食べる", "たべる", "食べる。");
        apply_subtitle_cloze(
            &mut cloze_ctx,
            &["一行目".to_owned(), "食べる。".to_owned()],
            Some(99),
            "食べる",
            None,
        );
        assert_eq!(cloze_ctx.cloze_body, "食べる");
    }

    fn test_profile(port: u16, fields: HashMap<String, String>) -> AnkiNoteConfig {
        AnkiNoteConfig {
            enabled: true,
            endpoint: AnkiEndpoint {
                hostname: "http://127.0.0.1".into(),
                port,
                api_key: None,
                timeout: Some(Duration::from_secs(2)),
                use_api_key: false,
            },
            deck: "Japanese".into(),
            model: "Lapis".into(),
            field_templates: fields,
            tags: vec!["subomi".into()],
            duplicate_policy: 1,
            screenshot_type: 0,
            audio_pad_start: 0.0,
            audio_pad_end: 0.0,
            audio_normalize: false,
            audio_db: -20.0,
            newline_replacer: "<br>".into(),
        }
    }

    #[test]
    fn unknown_tokens_anki_cloze_and_literal_braces_stay_verbatim() {
        let ctx = NoteFieldsContext::new("食べる", "たべる", "朝ご飯を食べる。");
        let profile = test_profile(
            8765,
            HashMap::from([
                ("Expression".into(), "{expression}".into()),
                ("Cloze".into(), "{{c1::{cloze-body}}} / {{c2::literal}}".into()),
                ("Custom".into(), "prefix {unknown-marker} {} suffix".into()),
                ("Css".into(), "<style>.a { color: red; }</style>".into()),
                ("Fallback".into(), "{unknown | expression}".into()),
            ]),
        );
        let payload = build_note_payload_from_profile(&profile, &ctx).unwrap();
        assert_eq!(payload.fields["Expression"], "食べる");
        assert_eq!(payload.fields["Cloze"], "{{c1::食べる}} / {{c2::literal}}");
        assert_eq!(
            payload.fields["Custom"],
            "prefix {unknown-marker} {} suffix"
        );
        assert_eq!(payload.fields["Css"], "<style>.a { color: red; }</style>");
        assert_eq!(payload.fields["Fallback"], "食べる");
    }

    #[test]
    fn batch_find_note_ids_multi_includes_api_key_on_every_sub_action() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            for step in 0..2 {
                let (mut stream, _) = listener.accept().unwrap();
                let body = read_http_json_body(&mut stream);
                assert_eq!(body["key"], "secret-api-key");
                let reply = match step {
                    0 => {
                        assert_eq!(body["action"], "canAddNotes");
                        serde_json::json!({ "error": null, "result": [false, false] }).to_string()
                    }
                    _ => {
                        assert_eq!(body["action"], "multi");
                        let actions = body["params"]["actions"].as_array().unwrap();
                        assert_eq!(actions.len(), 2);
                        for sub in actions {
                            assert_eq!(sub["action"], "findNotes");
                            assert_eq!(
                                sub["key"], "secret-api-key",
                                "every multi sub-action must carry apiKey (Yomitan PR #2421)"
                            );
                        }
                        serde_json::json!({
                            "error": null,
                            "result": [[1669909161001_i64], [1669909161002_i64]]
                        })
                        .to_string()
                    }
                };
                let http = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{reply}",
                    reply.len()
                );
                stream.write_all(http.as_bytes()).unwrap();
            }
        });

        let mut profile = test_profile(
            port,
            HashMap::from([("Expression".into(), "{expression}".into())]),
        );
        profile.endpoint.use_api_key = true;
        profile.endpoint.api_key = Some("secret-api-key".into());
        let ctrl = AnkiController::new(profile.endpoint());
        let statuses = ctrl
            .check_entries_anki_status(
                &profile,
                &[
                    ("食べる".to_owned(), "たべる".to_owned()),
                    ("飲む".to_owned(), "のむ".to_owned()),
                ],
                false,
            )
            .unwrap();
        server.join().unwrap();
        assert_eq!(statuses[0].note_ids, vec![1669909161001]);
        assert_eq!(statuses[1].note_ids, vec![1669909161002]);
    }

    #[test]
    fn model_field_names_cached_per_endpoint_and_supports_or_reading_browse_query() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            // Only ONE `modelFieldNames` RPC should arrive even after two browse-query calls!
            let (mut stream, _) = listener.accept().unwrap();
            let body = read_http_json_body(&mut stream);
            assert_eq!(body["action"], "modelFieldNames");
            assert_eq!(body["params"]["modelName"], "CustomSortModel");
            let reply = serde_json::json!({
                "error": null,
                "result": ["TargetWord", "Expression", "Reading"]
            })
            .to_string();
            let http = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{reply}",
                reply.len()
            );
            stream.write_all(http.as_bytes()).unwrap();
        });

        let mut profile = test_profile(
            port,
            HashMap::from([
                ("TargetWord".into(), "{expression}".into()),
                ("Expression".into(), "{furigana}".into()),
                ("Reading".into(), "{reading}".into()),
            ]),
        );
        profile.model = "CustomSortModel".into();
        let ctrl = AnkiController::new(profile.endpoint());

        // First call queries `modelFieldNames` and picks exact first field `TargetWord` + `OR reading`.
        let q1 = ctrl
            .build_duplicate_browse_query(&profile, "食べる", "たべる")
            .unwrap();
        assert_eq!(
            q1,
            "deck:\"Japanese\" (TargetWord:\"食べる\" OR TargetWord:\"たべる\")"
        );

        // Second call uses the `(endpoint, model)` cache (zero extra RPC; server thread already exited!).
        let q2 = ctrl
            .build_duplicate_browse_query(&profile, "やはり", "やはり")
            .unwrap();
        assert_eq!(q2, "deck:\"Japanese\" TargetWord:\"やはり\"");
        server.join().unwrap();
    }

    #[test]
    fn sqlite_enrichment_separates_rank_based_and_occurrence_based_frequency_modes() {
        let dir = tempfile::Builder::new()
            .prefix("subomi-anki-freqmode-")
            .tempdir()
            .unwrap();
        let db_path = dir.path().join("dictionaries.sqlite");
        subomi_dict::yomidbbuilder::prepare_db(&db_path).unwrap();

        let conn = subomi_dict::sqlite_db::open_read_write_connection(&db_path).unwrap();
        subomi_dict::sqlite_db::ensure_frequency_mode_column_conn(&conn).unwrap();
        conn.execute(
            "INSERT INTO directory (dic_id, title, format, revision, sequenced, frequency_mode) VALUES \
             (1, 'RankDictA', 3, '1', 0, 'rank-based'), \
             (2, 'RankDictB', 3, '1', 0, 'rank-based'), \
             (3, 'OccDictA', 3, '1', 0, 'occurrence-based'), \
             (4, 'OccDictB', 3, '1', 0, 'occurrence-based');",
            [],
        )
        .unwrap();
        // Term with BOTH rank-based (100, 300 -> harmonic 150, avg 200)
        // and occurrence-based (5000, 15000 -> harmonic 7500, avg 10000).
        conn.execute(
            "INSERT INTO term_meta_bank (dic_id, expression, mode, type, data) VALUES \
             (1, '食べる', 'freq', 4, '{\"reading\":\"たべる\",\"frequency\":100}'), \
             (2, '食べる', 'freq', 4, '{\"reading\":\"たべる\",\"frequency\":300}'), \
             (3, '食べる', 'freq', 4, '{\"reading\":\"たべる\",\"frequency\":5000}'), \
             (4, '食べる', 'freq', 4, '{\"reading\":\"たべる\",\"frequency\":15000}');",
            [],
        )
        .unwrap();
        // Rank-only term -> `*-occurrence` MUST stay `"0"` (never borrowing rank!).
        conn.execute(
            "INSERT INTO term_meta_bank (dic_id, expression, mode, type, data) VALUES \
             (1, '飲む', 'freq', 4, '{\"reading\":\"のむ\",\"frequency\":250}');",
            [],
        )
        .unwrap();
        // Occurrence-only term -> `*-rank` MUST stay `"9999999"` (never borrowing occurrence!).
        conn.execute(
            "INSERT INTO term_meta_bank (dic_id, expression, mode, type, data) VALUES \
             (3, '走る', 'freq', 4, '{\"reading\":\"はしる\",\"frequency\":4200}');",
            [],
        )
        .unwrap();
        drop(conn);

        let mut mixed = NoteFieldsContext::new("食べる", "たべる", "食べる。");
        populate_note_context_from_sqlite(&mut mixed, &db_path, &[], &[]).unwrap();
        assert_eq!(mixed.frequency_harmonic_rank, "150");
        assert_eq!(mixed.frequency_average_rank, "200");
        assert_eq!(mixed.frequency_harmonic_occurrence, "7500");
        assert_eq!(mixed.frequency_average_occurrence, "10000");

        let mut rank_only = NoteFieldsContext::new("飲む", "のむ", "飲む。");
        populate_note_context_from_sqlite(&mut rank_only, &db_path, &[], &[]).unwrap();
        assert_eq!(rank_only.frequency_harmonic_rank, "250");
        assert_eq!(rank_only.frequency_average_rank, "250");
        assert_eq!(rank_only.frequency_harmonic_occurrence, "0");
        assert_eq!(rank_only.frequency_average_occurrence, "0");

        let mut occ_only = NoteFieldsContext::new("走る", "はしる", "走る。");
        populate_note_context_from_sqlite(&mut occ_only, &db_path, &[], &[]).unwrap();
        assert_eq!(occ_only.frequency_harmonic_rank, "9999999");
        assert_eq!(occ_only.frequency_average_rank, "9999999");
        assert_eq!(occ_only.frequency_harmonic_occurrence, "4200");
        assert_eq!(occ_only.frequency_average_occurrence, "4200");

        // Markers also never cross-pollinate when occurrence fields are left empty manually.
        let mut manual = NoteFieldsContext::new("猫", "ねこ", "猫。");
        manual.frequency_harmonic_rank = "42".into();
        manual.frequency_average_rank = "55".into();
        let mapped = map_note_fields(
            &HashMap::from([
                ("HR".into(), "{frequency-harmonic-rank}".into()),
                ("HO".into(), "{frequency-harmonic-occurrence}".into()),
                ("AR".into(), "{frequency-average-rank}".into()),
                ("AO".into(), "{frequency-average-occurrence}".into()),
            ]),
            &manual,
        );
        assert_eq!(mapped["HR"], "42");
        assert_eq!(mapped["AR"], "55");
        assert_eq!(mapped["HO"], "0");
        assert_eq!(mapped["AO"], "0");
    }

    #[test]
    fn multi_definition_selection_formats_as_list_without_double_br() {
        let profile = test_profile(
            8765,
            HashMap::from([
                ("Expression".into(), "{expression}".into()),
                ("Selection".into(), "{selection}".into()),
            ]),
        );

        // 1. Multi-definition selections render cleanly as `<ul><li>...</li></ul>` without `<br><br>`.
        let ctx_multi = NoteFieldsContext::new("食べる", "たべる", "食べる。").with_selections(vec![
            "to eat\n\n\nmore details\r\n".into(),
            "   ".into(),
            "to live on <food & drink>".into(),
        ]);
        let payload_multi = build_note_payload_from_profile(&profile, &ctx_multi).unwrap();
        assert_eq!(
            payload_multi.fields["Selection"],
            "<ul><li>to eat<br>more details</li><li>to live on &lt;food &amp; drink&gt;</li></ul>"
        );
        assert!(!payload_multi.fields["Selection"].contains("<br><br>"));

        // 2. Legacy single `ctx.selection` stays backwards-compatible and strips double `<br>`.
        let mut ctx_single = NoteFieldsContext::new("食べる", "たべる", "食べる。");
        ctx_single.selection = "line one\r\n\r\nline two\n".into();
        let payload_single = build_note_payload_from_profile(&profile, &ctx_single).unwrap();
        assert_eq!(payload_single.fields["Selection"], "line one<br>line two");
    }

    #[test]
    fn sqlite_enrichment_exports_string_based_exact_pitch_patterns() {
        let dir = tempfile::Builder::new()
            .prefix("subomi-anki-pitch-pat-")
            .tempdir()
            .unwrap();
        let db_path = dir.path().join("dictionaries.sqlite");
        subomi_dict::yomidbbuilder::prepare_db(&db_path).unwrap();

        let conn = subomi_dict::sqlite_db::open_read_write_connection(&db_path).unwrap();
        conn.execute(
            "INSERT INTO directory (dic_id, title, format, revision, sequenced) VALUES (1, 'KanjiumPat', 3, '1', 0);",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO term_meta_bank (dic_id, expression, mode, type, data) VALUES \
             (1, '花火', 'pitch', 4, '{\"reading\":\"はなび\",\"pitches\":[{\"position\":\"HLL\"}]}'), \
             (1, '桜', 'pitch', 4, '{\"reading\":\"さくら\",\"pitches\":[{\"position\":\"LHH\"}]}'), \
             (1, '男', 'pitch', 4, '{\"reading\":\"おとこ\",\"pitches\":[{\"position\":\"LHHL\"}]}');",
            [],
        )
        .unwrap();
        drop(conn);

        let mut hanabi = NoteFieldsContext::new("花火", "はなび", "花火を見る。");
        populate_note_context_from_sqlite(&mut hanabi, &db_path, &[], &[]).unwrap();
        assert_eq!(
            hanabi.pitch_positions,
            "<span class=\"subomi-pitch-position\">[HLL]</span>"
        );
        assert_eq!(hanabi.pitch_categories, "atamadaka");
        assert!(hanabi.pitch.contains("border-right: solid;\">は</span>なび"));
        assert!(hanabi.pitch_graphs.contains("<svg"));
        assert!(hanabi.pitch_graphs.contains("href=\"#change-dot\" x=\"25\" y=\"25\""));

        let mut sakura = NoteFieldsContext::new("桜", "さくら", "桜が咲く。");
        populate_note_context_from_sqlite(&mut sakura, &db_path, &[], &[]).unwrap();
        assert_eq!(
            sakura.pitch_positions,
            "<span class=\"subomi-pitch-position\">[LHH]</span>"
        );
        assert_eq!(sakura.pitch_categories, "heiban");
        assert!(sakura.pitch.contains("さ<span style=\"border-top: solid;\">くら</span>"));

        let mut otoko = NoteFieldsContext::new("男", "おとこ", "男の人。");
        populate_note_context_from_sqlite(&mut otoko, &db_path, &[], &[]).unwrap();
        assert_eq!(
            otoko.pitch_positions,
            "<span class=\"subomi-pitch-position\">[LHHL]</span>"
        );
        assert_eq!(otoko.pitch_categories, "odaka");
        assert!(otoko.pitch.contains("border-right: solid;\">とこ</span>"));
    }

    #[test]
    fn reading_as_expression_exports_kana_and_checks_kana_duplicates() {
        let ctx = NoteFieldsContext::new("食べる", "たべる", "パンを食べる。")
            .with_reading_as_expression(true);
        let mapped = map_note_fields(
            &HashMap::from([
                ("Expression".into(), "{expression}".into()),
                ("Reading".into(), "{reading}".into()),
                ("Furigana".into(), "{furigana}".into()),
                ("FuriganaPlain".into(), "{furigana-plain}".into()),
            ]),
            &ctx,
        );
        assert_eq!(mapped["Expression"], "たべる");
        assert_eq!(mapped["Reading"], "たべる");
        assert_eq!(mapped["Furigana"], "たべる");
        assert_eq!(mapped["FuriganaPlain"], "たべる");

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            for step in 0..3 {
                let (mut stream, _) = listener.accept().unwrap();
                let body = read_http_json_body(&mut stream);
                let reply = match step {
                    0 => {
                        // `check_term_addable`: probes both kanji and kana in one `canAddNotes`.
                        assert_eq!(body["action"], "canAddNotes");
                        let notes = body["params"]["notes"].as_array().unwrap();
                        assert_eq!(notes.len(), 2);
                        assert_eq!(notes[0]["fields"]["Expression"], "食べる");
                        assert_eq!(notes[1]["fields"]["Expression"], "たべる");
                        serde_json::json!({ "error": null, "result": [false, true] }).to_string()
                    }
                    1 => {
                        // `check_entries_anki_status_with_lookups` with `reading_as_expression: true`.
                        assert_eq!(body["action"], "canAddNotes");
                        let notes = body["params"]["notes"].as_array().unwrap();
                        assert_eq!(notes[0]["fields"]["Expression"], "たべる");
                        serde_json::json!({ "error": null, "result": [false] }).to_string()
                    }
                    _ => {
                        assert_eq!(body["action"], "findNotes");
                        assert_eq!(body["params"]["query"], "deck:\"Japanese\" Expression:\"たべる\"");
                        serde_json::json!({ "error": null, "result": [1669909169999_i64] })
                            .to_string()
                    }
                };
                let http = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{reply}",
                    reply.len()
                );
                stream.write_all(http.as_bytes()).unwrap();
            }
        });

        let profile = test_profile(
            port,
            HashMap::from([("Expression".into(), "{expression}".into())]),
        );
        let ctrl = AnkiController::new(profile.endpoint());
        let addable = ctrl
            .check_term_addable(&profile, "食べる", "たべる")
            .unwrap();
        assert_eq!(
            addable,
            TermAddableStatus {
                expression: false,
                reading: true,
            }
        );

        let statuses = ctrl
            .check_entries_anki_status_with_lookups(
                &profile,
                &[EntryAnkiLookup::new("食べる", "たべる").with_reading_as_expression(true)],
                false,
            )
            .unwrap();
        server.join().unwrap();
        assert!(statuses[0].is_duplicate);
        assert_eq!(statuses[0].note_ids, vec![1669909169999]);
    }

    #[test]
    fn cloze_and_furigana_are_utf8_safe_for_cjk_ext_h_and_j() {
        // CJK Extension H (U+31350, 4-byte UTF-8) and Extension J (U+323B0, 4-byte UTF-8).
        let target = "\u{31350}\u{323B0}";
        let sentence = format!("前🔥{target}中{target}後");

        // Every single byte offset (including mid-UTF-8 continuation bytes inside 4-byte chars)
        // must never panic and must reconstruct the original sentence cleanly.
        for offset in 0..=sentence.len() + 5 {
            let (prefix, body, suffix) = split_cloze_around(&sentence, target, Some(offset));
            assert_eq!(body, target, "failed at byte offset {offset}");
            assert_eq!(format!("{prefix}{body}{suffix}"), sentence);
        }

        // Furigana formatting must never split UTF-16 surrogate pairs on CJK Ext H/J.
        let expr_h = format!("\u{31350}べる");
        let (furi, furi_plain) = format_expression_furigana(&expr_h, "たべる");
        assert_eq!(furi, format!("<ruby>\u{31350}<rt>た</rt></ruby>べる"));
        assert_eq!(furi_plain, format!("\u{31350}[た]べる"));
    }

    #[test]
    fn scoped_dictionary_css_caches_per_dict_id_and_raw_css_hash() {
        clear_scoped_css_cache();
        let css1 = ".gloss { color: red; }";
        let css2 = ".gloss { color: blue; }";
        let scoped1 = scope_dictionary_css_cached(42, css1, "Jitendex");
        assert_eq!(
            scoped1,
            "[data-dictionary=\"Jitendex\"] .gloss { color: red; }"
        );
        let len_after_first = css_glossary::scoped_css_cache_len();
        assert!(len_after_first >= 1);

        // Cache hit on same `(dict_id, raw_css_hash)`.
        let scoped1_again = scope_dictionary_css_cached(42, css1, "Jitendex");
        assert_eq!(scoped1_again, scoped1);
        assert_eq!(css_glossary::scoped_css_cache_len(), len_after_first);

        // Different raw CSS hash caches a distinct entry.
        let scoped2 = scope_dictionary_css_cached(42, css2, "Jitendex");
        assert_eq!(
            scoped2,
            "[data-dictionary=\"Jitendex\"] .gloss { color: blue; }"
        );
        assert_eq!(css_glossary::scoped_css_cache_len(), len_after_first + 1);
    }

    #[test]
    fn regression_yomitan_pr2510_filters_non_positive_note_ids_before_notes_info() {
        // All non-positive IDs must short-circuit with zero HTTP requests.
        let ctrl_unused = AnkiController::new(AnkiEndpoint {
            hostname: "http://127.0.0.1".into(),
            port: 1,
            api_key: None,
            timeout: Some(Duration::from_millis(200)),
            use_api_key: false,
        });
        assert!(ctrl_unused.fetch_note_tags(&[0, -1, -999]).unwrap().is_empty());

        // Mixed IDs must strip `<= 0` before calling `notesInfo`.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let body = read_http_json_body(&mut stream);
            assert_eq!(body["action"], "notesInfo");
            assert_eq!(body["params"]["notes"], serde_json::json!([1669909161234_i64]));
            let reply = serde_json::json!({
                "error": null,
                "result": [{
                    "noteId": 1669909161234_i64,
                    "modelName": "Lapis",
                    "tags": ["anime", "leech"],
                    "fields": {}
                }]
            })
            .to_string();
            let http = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{reply}",
                reply.len()
            );
            stream.write_all(http.as_bytes()).unwrap();
        });

        let ctrl = AnkiController::new(AnkiEndpoint {
            hostname: "http://127.0.0.1".into(),
            port,
            api_key: None,
            timeout: Some(Duration::from_secs(2)),
            use_api_key: false,
        });
        let tags = ctrl
            .fetch_note_tags(&[0, -5, 1669909161234, 0, 1669909161234])
            .unwrap();
        server.join().unwrap();
        assert_eq!(tags, vec!["anime", "leech"]);
    }

    #[test]
    fn regression_988_jitendex_part_of_speech_and_692_sentence_preserved() {
        let dir = tempfile::Builder::new()
            .prefix("subomi-anki-reg988-692-")
            .tempdir()
            .unwrap();
        let db_path = dir.path().join("dictionaries.sqlite");
        subomi_dict::yomidbbuilder::prepare_db(&db_path).unwrap();

        let conn = subomi_dict::sqlite_db::open_read_write_connection(&db_path).unwrap();
        conn.execute(
            "INSERT INTO directory (dic_id, title, format, revision, sequenced) VALUES (1, 'Jitendex', 3, '1', 1);",
            [],
        )
        .unwrap();
        // Jitendex entry with EMPTY `def_tags`/`term_tags`/`rules`, embedding POS inside
        // structured-content `data.content = "part-of-speech-info"` (#988).
        let jitendex_glossary = serde_json::json!([
            {
                "type": "structured-content",
                "content": {
                    "tag": "div",
                    "data": { "content": "entry" },
                    "content": [
                        {
                            "tag": "div",
                            "data": { "content": "part-of-speech-info" },
                            "content": [
                                { "tag": "span", "data": { "code": "n" }, "title": "noun (common)", "content": "n" },
                                { "tag": "span", "data": { "code": "vs" }, "title": "suru verb", "content": "vs" }
                            ]
                        },
                        {
                            "tag": "div",
                            "data": { "content": "glossary" },
                            "content": "study"
                        }
                    ]
                }
            }
        ])
        .to_string();
        conn.execute(
            "INSERT INTO term_bank (dic_id, expression, reading, def_tags, rules, score, glossary, sequence, term_tags) \
             VALUES (1, '勉強', 'べんきょう', '', '', 100, ?1, 1, '');",
            [&jitendex_glossary],
        )
        .unwrap();
        drop(conn);

        // Regression #692: `{sentence}` must remain preserved even across multi-cue cloze
        // and SQLite enrichment!
        let original_sentence = "毎日日本語を勉強する。";
        let mut ctx = NoteFieldsContext::new("勉強", "べんきょう", original_sentence);
        apply_subtitle_cloze(
            &mut ctx,
            &[
                "前の行。".to_owned(),
                original_sentence.to_owned(),
                "次の行。".to_owned(),
            ],
            Some(1),
            "勉強",
            None,
        );
        populate_note_context_from_sqlite(&mut ctx, &db_path, &[], &[]).unwrap();

        assert_eq!(ctx.part_of_speech, "n, vs");
        assert_eq!(ctx.sentence, original_sentence);

        let mapped = map_note_fields(
            &HashMap::from([
                ("POS".into(), "{part-of-speech}".into()),
                ("Sentence".into(), "{sentence}".into()),
                ("Context".into(), "{context}".into()),
            ]),
            &ctx,
        );
        assert_eq!(mapped["POS"], "n, vs");
        assert_eq!(mapped["Sentence"], original_sentence);
        assert!(mapped["Context"].contains("前の行。"));
        assert!(mapped["Context"].contains("次の行。"));
    }

    #[test]
    fn add_note_from_profile_with_media_succeeds_with_warnings_when_optional_media_fails() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let body = read_http_json_body(&mut stream);
            assert_eq!(body["action"], "addNote");
            assert_eq!(body["params"]["note"]["fields"]["Expression"], "辞書");
            assert_eq!(body["params"]["note"]["fields"]["Audio"], "");
            assert_eq!(body["params"]["note"]["fields"]["Picture"], "");
            assert!(body["params"]["note"].get("audio").is_none());
            assert!(body["params"]["note"].get("picture").is_none());
            let reply = serde_json::json!({ "error": null, "result": 1669909167777_i64 }).to_string();
            let http = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{reply}",
                reply.len()
            );
            stream.write_all(http.as_bytes()).unwrap();
        });

        let profile = test_profile(
            port,
            HashMap::from([
                ("Expression".into(), "{expression}".into()),
                ("Audio".into(), "{audio}".into()),
                ("Picture".into(), "{screenshot}".into()),
            ]),
        );
        let ctrl = AnkiController::new(profile.endpoint());
        let mut ctx = NoteFieldsContext::new("辞書", "じしょ", "");
        let input = crate::controller::anki_media::NoteMediaInput {
            media: None,
            subtitle_start: 0.0,
            subtitle_end: 0.0,
            context_start: 0.0,
            context_end: 0.0,
            screenshot: None,
            screenshot_video: None,
            subtitle_id: 0,
            audio_sources: Vec::new(),
        };
        let outcome = ctrl
            .add_note_from_profile_with_media(&profile, &mut ctx, input)
            .unwrap();
        server.join().unwrap();
        assert_eq!(outcome.note_id, 1669909167777);
        assert_eq!(outcome.warnings.len(), 2, "warnings: {:?}", outcome.warnings);
    }

    #[test]
    fn check_entries_anki_status_resolves_duplicate_via_model_field_names_when_heuristic_misses() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let mut seen = Vec::new();
            for step in 0..4 {
                let (mut stream, _) = listener.accept().unwrap();
                let body = read_http_json_body(&mut stream);
                seen.push(body["action"].as_str().unwrap().to_owned());
                let reply = match step {
                    0 => {
                        assert_eq!(body["action"], "canAddNotes");
                        serde_json::json!({ "error": null, "result": [false] }).to_string()
                    }
                    1 => {
                        // Heuristic guessed "Expression", which returns empty because Anki's
                        // actual sort field is "SortTarget" (or card is stored under kana).
                        assert_eq!(body["action"], "findNotes");
                        serde_json::json!({ "error": null, "result": [] }).to_string()
                    }
                    2 => {
                        assert_eq!(body["action"], "modelFieldNames");
                        serde_json::json!({
                            "error": null,
                            "result": ["SortTarget", "Expression"]
                        })
                        .to_string()
                    }
                    _ => {
                        assert_eq!(body["action"], "findNotes");
                        assert_eq!(
                            body["params"]["query"],
                            "deck:\"Japanese\" (SortTarget:\"食べる\" OR SortTarget:\"たべる\")"
                        );
                        serde_json::json!({ "error": null, "result": [1669909168888_i64] })
                            .to_string()
                    }
                };
                let http = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{reply}",
                    reply.len()
                );
                stream.write_all(http.as_bytes()).unwrap();
            }
            seen
        });

        let mut profile = test_profile(
            port,
            HashMap::from([
                ("SortTarget".into(), "{expression}".into()),
                ("Expression".into(), "{expression}".into()),
            ]),
        );
        profile.model = "SortTargetModel".into();
        let ctrl = AnkiController::new(profile.endpoint());
        let statuses = ctrl
            .check_entries_anki_status(&profile, &[("食べる".to_owned(), "たべる".to_owned())], false)
            .unwrap();
        assert_eq!(
            server.join().unwrap(),
            vec!["canAddNotes", "findNotes", "modelFieldNames", "findNotes"]
        );
        assert!(statuses[0].is_duplicate);
        assert_eq!(statuses[0].note_ids, vec![1669909168888]);
        assert_eq!(statuses[0].browse_query, "nid:1669909168888");
    }

    #[test]
    fn store_uncached_dict_media_skips_redundant_uploads_on_second_note() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let mut actions = Vec::new();
            for step in 0..3 {
                let (mut stream, _) = listener.accept().unwrap();
                let body = read_http_json_body(&mut stream);
                actions.push(body["action"].as_str().unwrap().to_owned());
                let reply = match step {
                    0 => serde_json::json!({ "error": null, "result": ["deadbeef.png"] }).to_string(),
                    1 => serde_json::json!({ "error": null, "result": 1669909169001_i64 }).to_string(),
                    _ => serde_json::json!({ "error": null, "result": 1669909169002_i64 }).to_string(),
                };
                let http = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{reply}",
                    reply.len()
                );
                stream.write_all(http.as_bytes()).unwrap();
            }
            actions
        });

        let profile = test_profile(
            port,
            HashMap::from([
                ("Expression".into(), "{expression}".into()),
                ("Glossary".into(), "{glossary}".into()),
            ]),
        );
        let mut ctx = NoteFieldsContext::new("食べる", "たべる", "パンを食べる。");
        ctx.glossary_definitions.push(GlossaryDefinition {
            dictionary_id: 1,
            dictionary_name: "Jitendex".into(),
            rendered_items: vec!["<span>eat <img src=\"deadbeef.png\"></span>".into()],
            media_files: vec![MediaFilePayload {
                filename: "deadbeef.png".into(),
                data_base64: "AAAA".into(),
            }],
            ..Default::default()
        });

        let ctrl = AnkiController::new(profile.endpoint());
        assert_eq!(ctrl.add_note_from_profile(&profile, &ctx).unwrap(), 1669909169001);
        // Second note with the same dict asset skips the redundant `multi`/`storeMediaFile` round-trip.
        assert_eq!(ctrl.add_note_from_profile(&profile, &ctx).unwrap(), 1669909169002);
        assert_eq!(server.join().unwrap(), vec!["multi", "addNote", "addNote"]);
    }
}

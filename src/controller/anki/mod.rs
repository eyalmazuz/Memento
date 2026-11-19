//! Anki controller connecting memento-anki and memento-net.

mod cloze;
mod context;
mod css_glossary;
mod markers;
mod payload;
mod query;
mod sqlite_enrich;

use std::collections::{HashMap, HashSet};

use memento_net::anki::{AnkiClient, AnkiEndpoint, MediaFilePayload};

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
    format_glossary, format_glossary_html, scope_dictionary_css, GlossaryOutput,
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
    build_entry_find_query, escape_anki_search_term, format_nid_browse_query,
    normalize_anki_browse_target, EntryAnkiStatus,
};
pub use self::sqlite_enrich::{
    populate_note_context_from_sqlite, populate_note_context_from_sqlite_ordered,
};

pub(crate) use self::context::encode_base64;
pub(crate) use self::markers::{
    evaluate_template_marker_kind, MarkerEvalKind, RenderedTemplateData,
};

use self::markers::referenced_dictionary_media_files_with_data;
use self::payload::{build_note_payload_from_profile_with_data, merge_note_update_with_attachments};
use self::query::{
    batch_find_note_ids, build_entry_probe_and_query_with_meta, ProbeProfileMeta,
};

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

    /// Fetches unique non-empty card tags across `note_ids` via AnkiConnect `notesInfo` (#346).
    pub fn fetch_note_tags(&self, note_ids: &[i64]) -> Result<Vec<String>, String> {
        if note_ids.is_empty() {
            return Ok(Vec::new());
        }
        let infos = self
            .client
            .notes_info(note_ids)
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
        if entries.is_empty() {
            return Ok(Vec::new());
        }
        let meta = ProbeProfileMeta::from_profile(profile)?;
        let policy = meta.policy;
        let mut probes = Vec::with_capacity(entries.len());
        let mut find_queries = Vec::with_capacity(entries.len());
        for (expression, reading) in entries {
            let (probe, query) =
                build_entry_probe_and_query_with_meta(profile, &meta, expression, reading);
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
        if !dict_files.is_empty() {
            self.client
                .store_media_files(&dict_files)
                .map_err(|error| error.to_string())?;
        }
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
        if !dict_files.is_empty() {
            self.client
                .store_media_files(&dict_files)
                .map_err(|error| error.to_string())?;
        }
        payload.audio = std::mem::take(&mut ctx.audio_attachments);
        payload.picture = std::mem::take(&mut ctx.picture_attachments);
        payload.video = std::mem::take(&mut ctx.video_attachments);
        self.client
            .add_note(payload.into_value())
            .map_err(|error| error.to_string())
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
        let payload =
            build_note_payload_from_profile_with_data(profile, ctx, &rendered, true, false)
                .map_err(|error| error.to_string())?;
        let infos = self
            .client
            .notes_info(&[note_id])
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
                &rendered,
            )
            .into_iter()
            .filter(|file| {
                merged.fields.values().any(|val| {
                    val.as_str()
                        .is_some_and(|text| text.contains(&file.filename))
                })
            })
            .collect();
            if !dict_files.is_empty() {
                self.client
                    .store_media_files(&dict_files)
                    .map_err(|error| error.to_string())?;
            }
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
        if note_id <= 0 {
            return Err("Anki note ID must be a positive integer".to_owned());
        }
        crate::controller::anki_media::prepare_note_media(profile, ctx, media_input)?;
        self.update_existing_note(note_id, profile, ctx, mode)
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
    use std::net::TcpListener;
    use std::time::Duration;

    #[test]
    fn check_entries_anki_status_batches_multiple_find_notes_via_multi() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let mut actions = Vec::new();
            for step in 0..3 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut buf = vec![0u8; 65536];
                let n = stream.read(&mut buf).unwrap();
                let req = String::from_utf8_lossy(buf.get(..n).unwrap_or(&buf));
                let body: serde_json::Value =
                    serde_json::from_str(req.split("\r\n\r\n").nth(1).unwrap_or("")).unwrap();
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
            .prefix("memento-anki-order-")
            .tempdir()
            .unwrap();
        let db_path = dir.path().join("dictionaries.sqlite");
        memento_dict::yomidbbuilder::prepare_db(&db_path).unwrap();

        let conn = memento_dict::sqlite_db::open_read_write_connection(&db_path).unwrap();
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
            .with_tag("memento")
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
}

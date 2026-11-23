//! Note fields context, glossary definition records, and shared text/encoding helpers.

use std::collections::HashMap;

use subomi_net::anki::MediaFilePayload;

use super::cloze::{format_expression_furigana, split_cloze};

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
    /// Optional SQLite `rowid` for exact `(dictionary_id, rowid)` matching during enrichment.
    pub rowid: Option<i64>,
    pub glossary: String,
    pub tags: Vec<String>,
    pub notes: String,
    pub example_ja: String,
    pub example_en: String,
    /// Pre-rendered HTML items produced by `subomi_anki::glossary::build_glossary`.
    pub rendered_items: Vec<String>,
    /// Individual kanji meanings when building a kanji `{glossary}` card.
    pub kanji_meanings: Vec<String>,
    /// Scoped dictionary CSS loaded from `styles.css`.
    pub stylesheet: String,
    /// Dictionary media files (`<img src="...">`) extracted by `build_glossary`.
    pub media_files: Vec<MediaFilePayload>,
}

/// Applies Subomi's `search/remove-regex` and `search/replace-lines` (`search/replace-with`)
/// filters to subtitle text. Empty or invalid regex patterns are safely ignored.
pub fn filter_subtitle_text(
    text: &str,
    remove_regex: Option<&str>,
    replace_lines: bool,
    replace_with: &str,
) -> String {
    subomi_dict::normalize::preprocess_search_text(
        text,
        remove_regex.unwrap_or(""),
        replace_lines,
        replace_with,
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

/// Shared base64 encoder used by Anki media capture and dictionary media enrichment.
pub(crate) fn encode_base64(bytes: &[u8]) -> Result<String, String> {
    let len = subomi_util::fileutils::base64_len(bytes.len())
        .ok_or_else(|| "Anki media is too large to encode".to_owned())?;
    let mut encoded = vec![0u8; len];
    subomi_util::fileutils::to_base64(bytes, &mut encoded)
        .ok_or_else(|| "Could not encode Anki media".to_owned())?;
    String::from_utf8(encoded).map_err(|error| error.to_string())
}

/// Convenience alias for [`NoteFieldsContext`].
pub type AnkiNoteContext = NoteFieldsContext;

/// Source data for populating Subomi Anki note fields.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NoteFieldsContext {
    pub expression: String,
    pub reading: String,
    pub sentence: String,
    pub context: String,
    pub selection: String,
    /// Multi-definition glossary selections (`#262`, Yomitan `#2248`), formatted as
    /// `<ul><li>...</li></ul>` when 2+ non-empty entries are present. Takes precedence
    /// over `selection` when non-empty.
    pub selections: Vec<String>,
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
    /// When `true`, exports the kana reading as `{expression}` and adapts `{reading}`,
    /// `{furigana}`, and `{furigana-plain}` accordingly (matching C++ `kanjiKanaButton`).
    pub reading_as_expression: bool,
    pub cloze_index: usize,
    /// Optional `search/remove-regex` pattern applied to subtitle/context/cloze text when building notes.
    pub subtitle_filter_regex: Option<String>,
    /// Optional `search/replace-lines` toggle applied to subtitle/context/cloze text when building notes.
    pub replace_lines: bool,
    /// Replacement string used when `replace_lines` (`search/replace-with`) is enabled.
    pub replace_lines_with: String,
    /// Optional user-configured dictionary display/priority order (`dic_id -> priority`).
    pub dictionary_order: HashMap<i64, i64>,
    /// Optional exact `(dic_id, rowid)` selections honored during SQLite enrichment.
    pub selected_rowids: Vec<(i64, i64)>,
    /// AnkiConnect attachments populated before submitting a profile note.
    pub audio_attachments: Vec<serde_json::Value>,
    pub picture_attachments: Vec<serde_json::Value>,
    pub video_attachments: Vec<serde_json::Value>,
    /// Non-fatal warnings collected when optional media capture fails or no video is loaded.
    pub media_warnings: Vec<String>,
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
            selections: Vec::new(),
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
            reading_as_expression: false,
            cloze_index: 1,
            subtitle_filter_regex: None,
            replace_lines: false,
            replace_lines_with: String::new(),
            dictionary_order: HashMap::new(),
            selected_rowids: Vec::new(),
            audio_attachments: Vec::new(),
            picture_attachments: Vec::new(),
            video_attachments: Vec::new(),
            media_warnings: Vec::new(),
            resolved_media_markers: Vec::new(),
        }
    }

    /// Returns the effective `{expression}` value, substituting `reading` when
    /// `reading_as_expression` is enabled and `reading` is non-empty.
    pub fn effective_expression(&self) -> &str {
        if self.reading_as_expression && !self.reading.is_empty() {
            self.reading.as_str()
        } else {
            self.expression.as_str()
        }
    }

    /// Returns the effective `{reading}` value, matching C++ `notebuilder.cpp::getReading`
    /// when `reading_as_expression` is enabled.
    pub fn effective_reading(&self) -> &str {
        if self.reading_as_expression {
            self.effective_expression()
        } else {
            self.reading.as_str()
        }
    }

    /// Returns the effective `{furigana}` value (plain kana when `reading_as_expression`
    /// is enabled, matching C++ `notebuilder.cpp::getFurigana`).
    pub fn effective_furigana(&self) -> &str {
        if self.reading_as_expression {
            self.effective_expression()
        } else {
            self.furigana.as_str()
        }
    }

    /// Returns the effective `{furigana-plain}` value (plain kana when
    /// `reading_as_expression` is enabled, matching C++ `notebuilder.cpp::getFuriganaPlain`).
    pub fn effective_furigana_plain(&self) -> &str {
        if self.reading_as_expression {
            self.effective_expression()
        } else {
            self.furigana_plain.as_str()
        }
    }

    /// Formats `{selection}` from `self.selections` (or single `self.selection` for
    /// backwards compatibility) via `subomi_anki::note::format_selection`, rendering
    /// multi-definition selections as `<ul><li>...</li></ul>` without double `<br>`.
    pub fn formatted_selection(&self) -> String {
        if !self.selections.is_empty() {
            let refs: Vec<&str> = self.selections.iter().map(String::as_str).collect();
            let formatted = subomi_anki::note::format_selection(&refs);
            if !formatted.is_empty() {
                return formatted;
            }
        }
        if !self.selection.is_empty() {
            return subomi_anki::note::format_selection(&[self.selection.as_str()]);
        }
        String::new()
    }

    /// Applies `search/remove-regex` and `search/replace-lines` to subtitle text fields
    /// (`sentence`, `context`, `cloze_prefix`, `cloze_body`, `cloze_suffix`) in place.
    pub fn apply_subtitle_filter(
        &mut self,
        remove_regex: Option<&str>,
        replace_lines: bool,
        replace_with: &str,
    ) {
        let regex_active = remove_regex.is_some_and(|r| !r.is_empty());
        if !regex_active && !replace_lines {
            return;
        }
        for value in [
            &mut self.sentence,
            &mut self.context,
            &mut self.cloze_prefix,
            &mut self.cloze_body,
            &mut self.cloze_suffix,
        ] {
            *value = filter_subtitle_text(value, remove_regex, replace_lines, replace_with);
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

    pub fn with_dictionary_order(mut self, order: HashMap<i64, i64>) -> Self {
        self.dictionary_order = order;
        self
    }

    pub fn with_selected_rowids(mut self, rowids: Vec<(i64, i64)>) -> Self {
        self.selected_rowids = rowids;
        self
    }

    pub fn with_selection(mut self, selection: impl Into<String>) -> Self {
        let s = selection.into();
        if !s.is_empty() {
            self.selections.push(s.clone());
            if self.selection.is_empty() {
                self.selection = s;
            }
        }
        self
    }

    pub fn with_selections(mut self, selections: Vec<String>) -> Self {
        self.selections = selections;
        self
    }

    pub fn with_reading_as_expression(mut self, enabled: bool) -> Self {
        self.reading_as_expression = enabled;
        self
    }
}

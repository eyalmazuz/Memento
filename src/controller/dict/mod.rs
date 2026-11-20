//! Dictionary controller connecting memento-dict and memento-util.

mod db;
mod glossary;
mod meta;
mod query;

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use memento_dict::deconjugationquerygenerator;
use memento_dict::normalize::{self, Regex};
use memento_dict::searchcontroller::{self, QuerySource};
use memento_dict::sqlite_db;

pub use memento_dict::databasemanager::{
    build_dict_id_style_path, build_style_path, resolve_dictionary_style_path, PitchPosition,
};
pub use memento_dict::normalize::{
    apply_remove_regex, apply_replace_lines, is_cjk_ideograph, preprocess_search_text,
};
pub use memento_dict::searchcontroller::{
    has_wildcard_chars, ClozeSpans, TextReplacementOptions, TextReplacementRule,
};
pub use memento_dict::sqlite_db::{
    db_file_fingerprint, DbFileFingerprint, FileMetaStamp, FrequencyMode, TermFrequencyWithMode,
};

pub use self::db::{
    invalidate_shared_readonly_db, shared_readonly_db, shrink_all_readonly_dbs,
    DictionaryMetadataSnapshot, ReadOnlyDictionaryDb,
};
pub use self::glossary::{
    format_glossary, parse_glossary_entry, parse_glossary_json_cached, parse_rich_glossary,
    shrink_parsed_glossary_cache,
};
pub use self::meta::compare_frequency_values;
pub use self::query::{
    format_cloze_for_term, format_cloze_spans, format_tags, format_tags_with_notes,
    generate_prefix_queries, normalize_nfkc_query, normalize_reading, rank_hits,
};

use self::meta::{
    build_anki_term_metadata_from_parts, compute_anki_frequency_summary,
    format_rank_and_occurrence, format_term_meta_tuple, query_term_frequencies_with_mode_sorted,
    select_term_frequencies, select_term_pitches, select_term_pitches_and_frequencies,
    selected_freqs_to_public,
};
use self::query::{
    build_lookup_queries_with_replacements, database_kanji_candidates, database_search_candidates,
    database_wildcard_candidates, wildcard_pattern_matches, MecabTokenizerFn,
};

/// A dictionary search hit for display, search ranking, and note creation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchHit {
    pub expression: String,
    pub reading: String,
    pub glossary: String,
    pub tags: String,
    pub score: i64,
    pub is_kanji: bool,
    pub notes: String,
    pub example_ja: String,
    pub example_en: String,
    pub dict_name: String,
    pub rich_glossary: Vec<GlossaryBlock>,
    pub derivation: String,
    pub derivation_descriptions: Vec<(String, String)>,
    pub dictionary_id: i64,
    pub rules: String,
    pub raw_glossary: String,
    pub term_tags: String,
    pub def_tags: String,
}

/// A display block retained from Yomitan's structured glossary data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GlossaryBlock {
    pub text: String,
    /// 0: paragraph, 1: heading, 2: list item, 3: link.
    pub kind: i32,
    pub href: String,
}

/// Values used by Anki note field markers for a selected term or kanji.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AnkiTermMetadata {
    pub pitch: String,
    pub pitch_positions: String,
    pub pitch_categories: String,
    pub frequencies: String,
    pub frequency_harmonic_rank: String,
    pub frequency_average_rank: String,
    pub frequency_harmonic_occurrence: String,
    pub frequency_average_occurrence: String,
}

/// Compact pitch graph entry (fields kept strictly intact for UI callers).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TermPitchGraph {
    pub dictionary: String,
    pub mora: Vec<String>,
    pub position: i32,
}

/// Full pitch accent entry including optional exact string pattern (`"LHH"`, `"HLL"`,
/// `"LHHL"` — Yomitan PR #2055), nasalized/devoiced mora indices, and tags.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TermPitchDetail {
    pub dictionary: String,
    pub mora: Vec<String>,
    pub position: i32,
    pub pattern: Option<String>,
    pub nasal: Vec<usize>,
    pub devoice: Vec<usize>,
    pub tags: Vec<String>,
}

impl TermPitchDetail {
    /// Returns the typed [`PitchPosition`] (`Pattern` when an exact string pattern was present,
    /// otherwise `Downstep` for non-negative integer downstep indices).
    #[must_use]
    pub fn pitch_position(&self) -> Option<PitchPosition> {
        if let Some(ref pat) = self.pattern {
            PitchPosition::is_valid_pattern(pat)
                .then(|| PitchPosition::Pattern(pat.to_ascii_uppercase()))
        } else {
            u32::try_from(self.position)
                .ok()
                .map(PitchPosition::Downstep)
        }
    }

    /// Returns the downstep or exact pattern badge text (e.g. `"[2]"` or `"[LHHL]"`).
    #[must_use]
    pub fn badge_text(&self) -> String {
        if let Some(ref pat) = self.pattern {
            if !pat.is_empty() {
                return format!("[{}]", pat.to_ascii_uppercase());
            }
        }
        format!("[{}]", self.position)
    }

    /// Computes per-mora high/low levels and following particle pitch level `(mora_highs, particle_high)`
    /// for both exact string pitch patterns (`"LHH"`, `"HLL"`, `"LHHL"`, `"HLHL"`) and numeric downsteps.
    #[must_use]
    pub fn mora_levels(&self) -> (Vec<bool>, bool) {
        let n = self.mora.len();
        if n == 0 {
            return (Vec::new(), false);
        }
        if let Some(ref pat) = self.pattern {
            if PitchPosition::is_valid_pattern(pat) {
                let levels = PitchPosition::Pattern(pat.clone()).to_levels(n);
                if levels.len() > n {
                    return (levels[..n].to_vec(), levels[n]);
                }
                if !levels.is_empty() {
                    let mut mora_highs = Vec::with_capacity(n);
                    mora_highs.extend_from_slice(&levels);
                    let tail = *levels.last().unwrap_or(&false);
                    mora_highs.resize(n, tail);
                    return (mora_highs, tail);
                }
            }
        }
        let pos = self.position;
        let mut mora_highs = vec![false; n];
        let particle_high = if pos == 0 {
            for h in mora_highs.iter_mut().skip(1) {
                *h = true;
            }
            true
        } else if pos == 1 {
            mora_highs[0] = true;
            false
        } else if pos > 1 {
            let limit = (pos as usize).min(n);
            for h in mora_highs.iter_mut().take(limit).skip(1) {
                *h = true;
            }
            false
        } else {
            false
        };
        (mora_highs, particle_high)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TermFrequency {
    pub dictionary: String,
    pub value: String,
}

impl SearchHit {
    pub fn new(
        expression: impl Into<String>,
        reading: impl Into<String>,
        glossary: impl Into<String>,
        tags: impl Into<String>,
        score: i64,
        is_kanji: bool,
    ) -> Self {
        Self {
            expression: expression.into(),
            reading: reading.into(),
            glossary: glossary.into(),
            tags: tags.into(),
            score,
            is_kanji,
            notes: String::new(),
            example_ja: String::new(),
            example_en: String::new(),
            dict_name: String::new(),
            rich_glossary: Vec::new(),
            derivation: String::new(),
            derivation_descriptions: Vec::new(),
            dictionary_id: 0,
            rules: String::new(),
            raw_glossary: String::new(),
            term_tags: String::new(),
            def_tags: String::new(),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn with_details(
        expression: impl Into<String>,
        reading: impl Into<String>,
        glossary: impl Into<String>,
        tags: impl Into<String>,
        notes: impl Into<String>,
        example_ja: impl Into<String>,
        example_en: impl Into<String>,
        dict_name: impl Into<String>,
        score: i64,
        is_kanji: bool,
    ) -> Self {
        Self {
            expression: expression.into(),
            reading: reading.into(),
            glossary: glossary.into(),
            tags: tags.into(),
            score,
            is_kanji,
            notes: notes.into(),
            example_ja: example_ja.into(),
            example_en: example_en.into(),
            dict_name: dict_name.into(),
            rich_glossary: Vec::new(),
            derivation: String::new(),
            derivation_descriptions: Vec::new(),
            dictionary_id: 0,
            rules: String::new(),
            raw_glossary: String::new(),
            term_tags: String::new(),
            def_tags: String::new(),
        }
    }

    /// Returns a cached parsed `serde_json::Value` for `self.raw_glossary` so UI callers
    /// can use `memento_definition`'s `*_value` APIs without re-parsing JSON.
    pub fn parsed_glossary(&self) -> Option<Arc<serde_json::Value>> {
        parse_glossary_json_cached(&self.raw_glossary)
    }
}

/// A structured tag entry from a dictionary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DictTag {
    pub name: String,
    pub category: String,
    pub notes: String,
    pub order: i64,
    pub score: i64,
}

impl DictTag {
    pub fn new(name: impl Into<String>, order: i64, score: i64) -> Self {
        Self {
            name: name.into(),
            category: String::new(),
            notes: String::new(),
            order,
            score,
        }
    }

    pub fn with_details(
        name: impl Into<String>,
        category: impl Into<String>,
        notes: impl Into<String>,
        order: i64,
        score: i64,
    ) -> Self {
        Self {
            name: name.into(),
            category: category.into(),
            notes: notes.into(),
            order,
            score,
        }
    }
}

/// An in-memory dictionary representation for offline lookups and tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InMemoryDictionary {
    pub name: String,
    pub priority: i64,
    pub entries: Vec<SearchHit>,
}

impl InMemoryDictionary {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            priority: 0,
            entries: Vec::new(),
        }
    }

    pub fn with_priority(name: impl Into<String>, priority: i64) -> Self {
        Self {
            name: name.into(),
            priority,
            entries: Vec::new(),
        }
    }

    pub fn with_entries(name: impl Into<String>, entries: Vec<SearchHit>) -> Self {
        Self {
            name: name.into(),
            priority: 0,
            entries,
        }
    }

    pub fn add_entry(&mut self, entry: SearchHit) {
        self.entries.push(entry);
    }

    pub fn add_entries(&mut self, entries: impl IntoIterator<Item = SearchHit>) {
        self.entries.extend(entries);
    }
}

/// Cloze sentence parts and formatted representation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClozeSentence {
    pub prefix: String,
    pub body: String,
    pub suffix: String,
    pub formatted: String,
}

impl ClozeSentence {
    pub fn new(prefix: String, body: String, suffix: String) -> Self {
        let formatted = format!("{prefix}{{{{c1::{body}}}}}{suffix}");
        Self {
            prefix,
            body,
            suffix,
            formatted,
        }
    }

    pub fn with_cloze_number(prefix: String, body: String, suffix: String, n: usize) -> Self {
        let formatted = format!("{prefix}{{{{c{n}::{body}}}}}{suffix}");
        Self {
            prefix,
            body,
            suffix,
            formatted,
        }
    }

    pub fn with_hint(prefix: String, body: String, suffix: String, hint: &str) -> Self {
        let formatted = format!("{prefix}{{{{c1::{body}::{hint}}}}}{suffix}");
        Self {
            prefix,
            body,
            suffix,
            formatted,
        }
    }
}

/// A failure to open the persisted Yomitan database for read-only search.
#[derive(Debug)]
pub enum DictControllerError {
    MissingDatabase(PathBuf),
    InvalidPath(String),
    Sqlite(sqlite_db::DictError),
}

impl fmt::Display for DictControllerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingDatabase(path) => {
                write!(f, "dictionary database does not exist: {}", path.display())
            }
            Self::InvalidPath(message) => write!(f, "invalid dictionary database path: {message}"),
            Self::Sqlite(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for DictControllerError {}

/// The dictionary search controller.
pub struct DictController {
    dictionaries: Vec<InMemoryDictionary>,
    database: Option<Arc<ReadOnlyDictionaryDb>>,
    recent_hits: Vec<SearchHit>,
    recent_parsed_glossaries: Vec<Option<Arc<serde_json::Value>>>,
    last_search_error: Option<String>,
    last_match_units: usize,
    dictionary_order: HashMap<i64, i64>,
    remove_regex_pattern: String,
    remove_regex: Option<Regex>,
    replace_lines: bool,
    replace_lines_with: String,
    text_replacements: TextReplacementOptions,
    exact_enabled: bool,
    deconj_enabled: bool,
    mecab_enabled: bool,
    mecab_tokenizer: MecabTokenizerFn,
}

impl Default for DictController {
    fn default() -> Self {
        Self::new()
    }
}

impl DictController {
    pub fn new() -> Self {
        Self {
            dictionaries: Vec::new(),
            database: None,
            recent_hits: Vec::new(),
            recent_parsed_glossaries: Vec::new(),
            last_search_error: None,
            last_match_units: 0,
            dictionary_order: HashMap::new(),
            remove_regex_pattern: String::new(),
            remove_regex: None,
            replace_lines: false,
            replace_lines_with: String::new(),
            text_replacements: TextReplacementOptions::default(),
            exact_enabled: true,
            deconj_enabled: true,
            mecab_enabled: false,
            mecab_tokenizer: memento_util::mecab::mecab_tokenize_for_lookup,
        }
    }

    /// Open Memento's persisted Yomitan database read-only, sharing a single
    /// `Arc<ReadOnlyDictionaryDb>` connection + LRU metadata cache per database path.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, DictControllerError> {
        let database = shared_readonly_db(path.as_ref())?;
        Ok(Self {
            dictionaries: Vec::new(),
            database: Some(database),
            recent_hits: Vec::new(),
            recent_parsed_glossaries: Vec::new(),
            last_search_error: None,
            last_match_units: 0,
            dictionary_order: HashMap::new(),
            remove_regex_pattern: String::new(),
            remove_regex: None,
            replace_lines: false,
            replace_lines_with: String::new(),
            text_replacements: TextReplacementOptions::default(),
            exact_enabled: true,
            deconj_enabled: true,
            mecab_enabled: false,
            mecab_tokenizer: memento_util::mecab::mecab_tokenize_for_lookup,
        })
    }

    /// Returns a cloned handle to the shared read-only SQLite database, if opened.
    pub fn shared_database(&self) -> Option<Arc<ReadOnlyDictionaryDb>> {
        self.database.as_ref().map(Arc::clone)
    }

    /// Returns the pre-parsed glossary JSON value aligned with `self.hits()[index]`, if available.
    pub fn parsed_glossary(&self, index: usize) -> Option<Arc<serde_json::Value>> {
        self.recent_parsed_glossaries
            .get(index)
            .and_then(|entry| entry.as_ref().map(Arc::clone))
            .or_else(|| {
                self.recent_hits
                    .get(index)
                    .and_then(SearchHit::parsed_glossary)
            })
    }

    /// Enables or disables exact-prefix query generation (`search/exact-matcher`).
    pub fn set_exact_enabled(&mut self, enabled: bool) {
        self.exact_enabled = enabled;
    }

    /// Returns whether exact-prefix query generation is enabled.
    pub fn exact_enabled(&self) -> bool {
        self.exact_enabled
    }

    /// Enables or disables deconjugation query generation (`search/deconj-matcher`).
    pub fn set_deconj_enabled(&mut self, enabled: bool) {
        self.deconj_enabled = enabled;
    }

    /// Returns whether deconjugation query generation is enabled.
    pub fn deconj_enabled(&self) -> bool {
        self.deconj_enabled
    }

    /// Enables or disables MeCab morphological compound/lemma query generation during [`Self::search`].
    pub fn set_mecab_enabled(&mut self, enabled: bool) {
        self.mecab_enabled = enabled;
    }

    /// Returns whether MeCab morphological query generation is enabled.
    pub fn mecab_enabled(&self) -> bool {
        self.mecab_enabled
    }

    /// Overrides the MeCab lookup tokenizer function (defaults to
    /// [`memento_util::mecab::mecab_tokenize_for_lookup`]).
    pub fn set_mecab_tokenizer(&mut self, tokenizer: MecabTokenizerFn) {
        self.mecab_tokenizer = tokenizer;
    }

    pub fn take_search_error(&mut self) -> Option<String> {
        self.last_search_error.take()
    }

    pub fn database_path_is_open(&self) -> bool {
        self.database.is_some()
    }

    /// Apply the configured dictionary IDs in search-priority order.
    /// Unlisted IDs receive `i64::MAX` so configured dictionaries always precede unlisted ones.
    pub fn set_order(&mut self, order: &[i64]) {
        self.dictionary_order.clear();
        self.dictionary_order.reserve(order.len());
        for (priority, &dictionary_id) in order.iter().enumerate() {
            self.dictionary_order
                .entry(dictionary_id)
                .or_insert(priority as i64);
        }
    }

    /// Configures `search/remove-regex` filtering applied before query lookup.
    pub fn set_remove_regex(&mut self, pattern: &str) {
        if self.remove_regex_pattern == pattern {
            return;
        }
        self.remove_regex_pattern = pattern.to_owned();
        self.remove_regex = normalize::compile_remove_regex(pattern);
    }

    /// Configures `search/replace-lines` and `search/replace-with` filtering applied before query lookup.
    pub fn set_replace_lines(&mut self, enabled: bool, replace_with: &str) {
        self.replace_lines = enabled;
        self.replace_lines_with = replace_with.to_owned();
    }

    /// Configures both `search/remove-regex` and `search/replace-lines` text filters.
    pub fn set_search_text_filters(
        &mut self,
        remove_regex: &str,
        replace_lines: bool,
        replace_with: &str,
    ) {
        self.set_remove_regex(remove_regex);
        self.set_replace_lines(replace_lines, replace_with);
    }

    /// Returns the configured `search/remove-regex` pattern.
    pub fn remove_regex_pattern(&self) -> &str {
        &self.remove_regex_pattern
    }

    /// Returns whether `search/replace-lines` is enabled.
    pub fn replace_lines(&self) -> bool {
        self.replace_lines
    }

    /// Returns the configured `search/replace-with` replacement string.
    pub fn replace_lines_with(&self) -> &str {
        &self.replace_lines_with
    }

    /// Configures Yomitan-compatible custom ordered regex text replacements and
    /// `search_original` toggle (`search/text-replacements`, H6.5).
    pub fn set_text_replacements(&mut self, options: TextReplacementOptions) {
        self.text_replacements = options;
    }

    /// Returns the configured custom text replacement options.
    pub fn text_replacements(&self) -> &TextReplacementOptions {
        &self.text_replacements
    }

    #[inline]
    fn dict_priority(&self, dic_id: i64) -> i64 {
        self.dictionary_order
            .get(&dic_id)
            .copied()
            .unwrap_or(i64::MAX)
    }

    /// Resolves `styles.css` for `dic_id` under `res_dir`, preferring `dict-{dic_id}/styles.css`
    /// and falling back to `{dict_name}/styles.css`.
    pub fn dictionary_style_path(&self, res_dir: &Path, dic_id: i64) -> Option<PathBuf> {
        let dict_name = self
            .database
            .as_ref()
            .and_then(|db| db.metadata_snapshot().ok())
            .and_then(|snap| snap.names.get(&dic_id).cloned())
            .unwrap_or_default();
        resolve_dictionary_style_path(res_dir, dic_id, &dict_name)
    }

    /// Add an in-memory dictionary.
    pub fn add_dictionary(&mut self, dict: InMemoryDictionary) {
        self.dictionaries.push(dict);
    }

    /// Add a single entry to the first dictionary (creates a default dictionary if none exists).
    pub fn add_entry(&mut self, entry: SearchHit) {
        if self.dictionaries.is_empty() {
            self.dictionaries.push(InMemoryDictionary::new("Default"));
        }
        self.dictionaries[0].add_entry(entry);
    }

    /// Add entries to the first dictionary (creates a default dictionary if none exists).
    pub fn add_entries(&mut self, entries: impl IntoIterator<Item = SearchHit>) {
        if self.dictionaries.is_empty() {
            self.dictionaries.push(InMemoryDictionary::new("Default"));
        }
        self.dictionaries[0].add_entries(entries);
    }

    /// Clears all registered in-memory dictionaries and recent hits.
    pub fn clear_dictionaries(&mut self) {
        self.dictionaries.clear();
        self.recent_hits.clear();
        self.recent_parsed_glossaries.clear();
    }

    /// View all registered in-memory dictionaries.
    pub fn dictionaries(&self) -> &[InMemoryDictionary] {
        &self.dictionaries
    }

    /// Mutable view of all registered in-memory dictionaries.
    pub fn dictionaries_mut(&mut self) -> &mut [InMemoryDictionary] {
        &mut self.dictionaries
    }

    /// Normalizes a reading (katakana to hiragana and NFKC normalization).
    pub fn normalize_reading(&self, text: &str) -> String {
        normalize_reading(text)
    }

    /// Generates prefix queries from input text.
    pub fn generate_prefix_queries(&self, text: &str) -> Vec<String> {
        generate_prefix_queries(text)
    }

    /// Formats a list of tags.
    pub fn format_tags(&self, tags: &[DictTag]) -> String {
        format_tags(tags)
    }

    /// Formats a list of tags with notes.
    pub fn format_tags_with_notes(&self, tags: &[DictTag]) -> String {
        format_tags_with_notes(tags)
    }

    /// Formats a cloze sentence for a selected term.
    pub fn format_cloze(&self, sentence: &str, term: &str) -> Option<ClozeSentence> {
        format_cloze_for_term(sentence, term)
    }

    /// Formats a cloze sentence from span offsets and length.
    pub fn format_cloze_spans(&self, sentence: &str, index: i64, surface_len: u64) -> ClozeSentence {
        format_cloze_spans(sentence, index, surface_len)
    }

    /// Computes raw cloze spans using `memento_dict::searchcontroller::cloze_spans`.
    pub fn cloze_spans(&self, text_len: u64, index: i64, surface_len: u64) -> ClozeSpans {
        searchcontroller::cloze_spans(text_len, index, surface_len)
    }

    fn preprocess_query(&self, query: &str) -> String {
        let after_regex = normalize::apply_compiled_remove_regex(query, self.remove_regex.as_ref());
        if self.replace_lines {
            normalize::apply_replace_lines(&after_regex, true, &self.replace_lines_with)
        } else {
            after_regex.into_owned()
        }
    }

    fn run_wildcard_candidates(
        &mut self,
        trimmed: &str,
        full_u16_len: usize,
    ) -> Vec<SearchCandidate> {
        let mut candidates: Vec<SearchCandidate> = Vec::new();
        if let Some(database) = self.database.as_ref() {
            match database.metadata_snapshot() {
                Ok(snapshot) => {
                    match database_wildcard_candidates(
                        database,
                        &snapshot,
                        trimmed,
                        full_u16_len,
                        sqlite_db::DEFAULT_WILDCARD_LIMIT,
                    ) {
                        Ok(db_candidates) => {
                            for candidate in db_candidates {
                                let dict_priority = self.dict_priority(candidate.dic_id);
                                candidates.push(SearchCandidate {
                                    hit: candidate.hit,
                                    parsed_glossary: candidate.parsed_glossary,
                                    is_exact: candidate.is_exact,
                                    prefix_len: candidate.prefix_len,
                                    dict_priority,
                                    dic_id_tiebreak: candidate.dic_id,
                                    original_index: candidates.len(),
                                });
                            }
                        }
                        Err(error) => self.last_search_error = Some(error.to_string()),
                    }
                }
                Err(error) => {
                    self.last_search_error = Some(DictControllerError::Sqlite(error).to_string());
                }
            }
        }

        if !self.dictionaries.is_empty() {
            let cap = sqlite_db::DEFAULT_WILDCARD_LIMIT;
            for (dictionary_index, dict) in self.dictionaries.iter().enumerate() {
                for entry in &dict.entries {
                    if candidates.len() >= cap {
                        break;
                    }
                    let matches = wildcard_pattern_matches(trimmed, &entry.expression)
                        || (!entry.reading.is_empty()
                            && wildcard_pattern_matches(trimmed, &entry.reading));
                    if matches {
                        let mut hit = entry.clone();
                        if hit.dict_name.is_empty() {
                            hit.dict_name = dict.name.clone();
                        }
                        candidates.push(SearchCandidate {
                            hit,
                            parsed_glossary: None,
                            is_exact: true,
                            prefix_len: full_u16_len,
                            dict_priority: dict.priority,
                            dic_id_tiebreak: dictionary_index as i64,
                            original_index: candidates.len(),
                        });
                    }
                }
            }
        }
        candidates
    }

    fn finalize_candidates(&mut self, trimmed: &str, candidates: Vec<SearchCandidate>) -> &[SearchHit] {
        // Accumulate `term.score` across dictionaries per `(expression, reading, is_kanji)` group
        // and rank `expression == cloze_body` ahead of reading-only matches (M2).
        struct GroupRank {
            accumulated_score: i64,
            is_exact: bool,
            max_prefix_len: usize,
            expr_equals_cloze: bool,
            best_dict_priority: i64,
            first_index: usize,
        }

        let mut group_ranks: Vec<GroupRank> = Vec::with_capacity(candidates.len());
        let mut group_ids: HashMap<(&str, &str, bool), usize> =
            HashMap::with_capacity(candidates.len());
        let mut cand_group_ids: Vec<usize> = Vec::with_capacity(candidates.len());
        for c in &candidates {
            let key = (
                c.hit.expression.as_str(),
                c.hit.reading.as_str(),
                c.hit.is_kanji,
            );
            let cloze_body = utf16_prefix_slice(trimmed, c.prefix_len);
            let expr_eq = !cloze_body.is_empty() && c.hit.expression == cloze_body;
            let gid = match group_ids.entry(key) {
                std::collections::hash_map::Entry::Vacant(slot) => {
                    let id = group_ranks.len();
                    group_ranks.push(GroupRank {
                        accumulated_score: c.hit.score,
                        is_exact: c.is_exact,
                        max_prefix_len: c.prefix_len,
                        expr_equals_cloze: expr_eq,
                        best_dict_priority: c.dict_priority,
                        first_index: c.original_index,
                    });
                    slot.insert(id);
                    id
                }
                std::collections::hash_map::Entry::Occupied(slot) => {
                    let id = *slot.get();
                    let g = &mut group_ranks[id];
                    g.accumulated_score = g.accumulated_score.saturating_add(c.hit.score);
                    g.is_exact |= c.is_exact;
                    if c.prefix_len > g.max_prefix_len {
                        g.max_prefix_len = c.prefix_len;
                        g.expr_equals_cloze = expr_eq;
                    } else if c.prefix_len == g.max_prefix_len {
                        g.expr_equals_cloze |= expr_eq;
                    }
                    g.best_dict_priority = g.best_dict_priority.min(c.dict_priority);
                    id
                }
            };
            cand_group_ids.push(gid);
        }
        drop(group_ids);

        let mut indexed: Vec<(usize, SearchCandidate)> =
            cand_group_ids.into_iter().zip(candidates).collect();
        indexed.sort_by(|(ga_id, a), (gb_id, b)| {
            if ga_id == gb_id {
                return a
                    .dict_priority
                    .cmp(&b.dict_priority)
                    .then_with(|| b.hit.score.cmp(&a.hit.score))
                    .then_with(|| a.dic_id_tiebreak.cmp(&b.dic_id_tiebreak))
                    .then_with(|| a.original_index.cmp(&b.original_index));
            }
            let ga = &group_ranks[*ga_id];
            let gb = &group_ranks[*gb_id];
            a.hit
                .is_kanji
                .cmp(&b.hit.is_kanji)
                .then_with(|| gb.is_exact.cmp(&ga.is_exact))
                .then_with(|| gb.max_prefix_len.cmp(&ga.max_prefix_len))
                .then_with(|| gb.expr_equals_cloze.cmp(&ga.expr_equals_cloze))
                .then_with(|| gb.accumulated_score.cmp(&ga.accumulated_score))
                .then_with(|| ga.best_dict_priority.cmp(&gb.best_dict_priority))
                .then_with(|| ga.first_index.cmp(&gb.first_index))
        });

        self.last_match_units = indexed.first().map_or(0, |(_, candidate)| candidate.prefix_len);
        self.recent_hits.reserve(indexed.len());
        self.recent_parsed_glossaries.reserve(indexed.len());
        for (_, candidate) in indexed {
            self.recent_hits.push(candidate.hit);
            self.recent_parsed_glossaries
                .push(candidate.parsed_glossary);
        }

        // Batch-preload `term_meta_bank` rows for the matched term expressions without a second
        // metadata refresh check.
        if let Some(database) = self.database.as_ref() {
            let mut exprs: Vec<String> = Vec::new();
            let mut seen_exprs: HashSet<&str> = HashSet::new();
            for hit in &self.recent_hits {
                if !hit.is_kanji && seen_exprs.insert(hit.expression.as_str()) {
                    exprs.push(hit.expression.clone());
                }
            }
            database.preload_term_meta_no_refresh(&exprs);
        }

        &self.recent_hits
    }

    /// Performs an explicit bounded wildcard search (`*`/`＊` = any chars, `?`/`？` = single char)
    /// across enabled SQLite and in-memory dictionaries (H6.2).
    pub fn search_wildcard(&mut self, query: &str) -> &[SearchHit] {
        self.recent_hits.clear();
        self.recent_parsed_glossaries.clear();
        self.last_match_units = 0;
        self.last_search_error = None;

        let preprocessed = self.preprocess_query(query);
        let trimmed = normalize::trim_leading_whitespace(&preprocessed).trim_end();
        if trimmed.is_empty() {
            return &self.recent_hits;
        }
        let full_u16_len = trimmed.encode_utf16().count();
        let candidates = self.run_wildcard_candidates(trimmed, full_u16_len);
        self.finalize_candidates(trimmed, candidates)
    }

    /// Manual search entry point (e.g. `DictionarySearchPanel`) that strictly dispatches any
    /// query containing `*`, `?`, `＊`, or `？` to [`Self::search_wildcard`], and otherwise runs
    /// standard lookup via [`Self::search`].
    pub fn search_manual(&mut self, query: &str) -> &[SearchHit] {
        let preprocessed = self.preprocess_query(query);
        let trimmed = normalize::trim_leading_whitespace(&preprocessed).trim_end();
        if searchcontroller::has_wildcard_chars(trimmed) {
            self.search_wildcard(query)
        } else {
            self.search(query)
        }
    }

    /// Search persisted terms, exact kanji rows, and registered in-memory entries.
    /// An empty result means the query had no real match; no synthetic fallback is added.
    pub fn search(&mut self, query: &str) -> &[SearchHit] {
        self.recent_hits.clear();
        self.recent_parsed_glossaries.clear();
        self.last_match_units = 0;
        self.last_search_error = None;

        let preprocessed = self.preprocess_query(query);
        let trimmed = normalize::trim_leading_whitespace(&preprocessed).trim_end();
        if trimmed.is_empty() {
            return &self.recent_hits;
        }

        let full_u16_len = trimmed.encode_utf16().count();

        // Dispatch manual wildcard queries (`*`, `?`, `＊`, `？`) to `search_terms_wildcard` (H6.2).
        // If a query contains only `?`/`？` (e.g. a Japanese subtitle sentence ending in `？`)
        // and yields zero wildcard hits, fall through cleanly to standard prefix/deconj search.
        if searchcontroller::has_wildcard_chars(trimmed) {
            let wildcard_candidates = self.run_wildcard_candidates(trimmed, full_u16_len);
            let has_star = trimmed.chars().any(|ch| matches!(ch, '*' | '＊'));
            if !wildcard_candidates.is_empty() || has_star {
                return self.finalize_candidates(trimmed, wildcard_candidates);
            }
        }

        let norm_query = normalize_reading(trimmed);
        let lookups = build_lookup_queries_with_replacements(
            trimmed,
            self.exact_enabled,
            self.deconj_enabled,
            self.mecab_enabled,
            self.mecab_tokenizer,
            &self.text_replacements,
        );

        let mut candidates: Vec<SearchCandidate> = Vec::new();

        if let Some(database) = self.database.as_ref() {
            match database.metadata_snapshot() {
                Ok(snapshot) => {
                    if !lookups.is_empty() {
                        match database_search_candidates(
                            database,
                            &snapshot,
                            trimmed,
                            &norm_query,
                            full_u16_len,
                            &lookups,
                        ) {
                            Ok(db_candidates) => {
                                for candidate in db_candidates {
                                    let dict_priority = self.dict_priority(candidate.dic_id);
                                    candidates.push(SearchCandidate {
                                        hit: candidate.hit,
                                        parsed_glossary: candidate.parsed_glossary,
                                        is_exact: candidate.is_exact,
                                        prefix_len: candidate.prefix_len,
                                        dict_priority,
                                        dic_id_tiebreak: candidate.dic_id,
                                        original_index: candidates.len(),
                                    });
                                }
                            }
                            Err(error) => self.last_search_error = Some(error.to_string()),
                        }
                    }

                    // Always look up the leading kanji character in kanji dictionaries on
                    // multi-character lookups too, independent of `self.exact_enabled`, so a
                    // kanji entry footer is always available below term results (M2).
                    if let Some(first_ch) = trimmed.chars().next() {
                        let is_single_char = trimmed.chars().count() == 1;
                        if normalize::is_cjk_ideograph(first_ch)
                            || (self.exact_enabled && is_single_char)
                        {
                            let mut kanji_buf = [0u8; 4];
                            let kanji_str = first_ch.encode_utf8(&mut kanji_buf);
                            let mut kanji_res = database_kanji_candidates(
                                database,
                                &snapshot,
                                kanji_str,
                                first_ch.len_utf16(),
                                &self.dictionary_order,
                            );
                            if let Ok(ref list) = kanji_res {
                                if list.is_empty() {
                                    let std_ch = normalize::standardize_kanji_char(first_ch);
                                    if std_ch != first_ch {
                                        let mut std_buf = [0u8; 4];
                                        let std_str = std_ch.encode_utf8(&mut std_buf);
                                        kanji_res = database_kanji_candidates(
                                            database,
                                            &snapshot,
                                            std_str,
                                            first_ch.len_utf16(),
                                            &self.dictionary_order,
                                        );
                                    }
                                }
                            }
                            match kanji_res {
                                Ok(kanji_candidates) => {
                                    for candidate in kanji_candidates {
                                        let dict_priority = self.dict_priority(candidate.dic_id);
                                        candidates.push(SearchCandidate {
                                            hit: candidate.hit,
                                            parsed_glossary: candidate.parsed_glossary,
                                            is_exact: is_single_char,
                                            prefix_len: candidate.prefix_len,
                                            dict_priority,
                                            dic_id_tiebreak: candidate.dic_id,
                                            original_index: candidates.len(),
                                        });
                                    }
                                }
                                Err(error) => self.last_search_error = Some(error.to_string()),
                            }
                        }
                    }
                }
                Err(error) => {
                    self.last_search_error = Some(DictControllerError::Sqlite(error).to_string());
                }
            }
        }

        if !self.dictionaries.is_empty() {
            let mut seen_memory_entries = HashSet::new();
            for lookup in &lookups {
                let prefix_trimmed = lookup.key.trim();
                if prefix_trimmed.is_empty() {
                    continue;
                }
                let norm_prefix = normalize_reading(prefix_trimmed);
                let prefix_len = lookup.surface_len;

                for (dictionary_index, dict) in self.dictionaries.iter().enumerate() {
                    for (entry_index, entry) in dict.entries.iter().enumerate() {
                        let entry_key = (dictionary_index, entry_index);
                        if seen_memory_entries.contains(&entry_key) {
                            continue;
                        }
                        if lookup.rule_mask != 0 {
                            let entry_rules_src = if !entry.rules.is_empty() {
                                entry.rules.as_str()
                            } else {
                                entry.tags.as_str()
                            };
                            let entry_mask =
                                deconjugationquerygenerator::parse_rule_mask(entry_rules_src);
                            if !deconjugationquerygenerator::rule_matches_entry(
                                lookup.rule_mask,
                                entry_mask,
                            ) {
                                continue;
                            }
                        }

                        let norm_entry_reading = normalize_reading(&entry.reading);
                        let norm_entry_expr = normalize_reading(&entry.expression);

                        let matches = entry.expression == prefix_trimmed
                            || norm_entry_expr == norm_prefix
                            || (!norm_entry_reading.is_empty()
                                && norm_entry_reading == norm_prefix);

                        if matches {
                            seen_memory_entries.insert(entry_key);
                            let is_exact = (lookup.source == QuerySource::Exact
                                && lookup.rule_mask == 0
                                && lookup.surface_len == full_u16_len)
                                || (lookup.rule_mask == 0
                                    && (entry.expression == trimmed
                                        || norm_entry_expr == norm_query
                                        || (!norm_entry_reading.is_empty()
                                            && norm_entry_reading == norm_query)));
                            let mut hit = entry.clone();
                            if hit.dict_name.is_empty() {
                                hit.dict_name = dict.name.clone();
                            }
                            if lookup.rule_mask != 0 && hit.derivation.is_empty() {
                                let entry_rules_src = if !entry.rules.is_empty() {
                                    entry.rules.as_str()
                                } else {
                                    entry.tags.as_str()
                                };
                                let entry_mask =
                                    deconjugationquerygenerator::parse_rule_mask(entry_rules_src);
                                let (chain, descs) = lookup.derivation_for_entry(entry_mask);
                                hit.derivation = chain;
                                hit.derivation_descriptions = descs;
                            }
                            candidates.push(SearchCandidate {
                                hit,
                                parsed_glossary: None,
                                is_exact,
                                prefix_len,
                                dict_priority: dict.priority,
                                dic_id_tiebreak: dictionary_index as i64,
                                original_index: candidates.len(),
                            });
                        }
                    }
                }
            }
        }

        self.finalize_candidates(trimmed, candidates)
    }

    /// UTF-16 length of the query text matched by the first hit of the last search.
    pub fn last_match_units(&self) -> usize {
        self.last_match_units
    }

    /// Search exact kanji definitions from the persisted dictionaries.
    pub fn search_kanji(&mut self, query: &str) -> &[SearchHit] {
        self.recent_hits.clear();
        self.recent_parsed_glossaries.clear();
        self.last_search_error = None;
        if query.is_empty() {
            return &self.recent_hits;
        }
        let Some(database) = self.database.as_ref() else {
            return &self.recent_hits;
        };

        let snapshot = match database.metadata_snapshot() {
            Ok(snap) => snap,
            Err(error) => {
                self.last_search_error = Some(DictControllerError::Sqlite(error).to_string());
                return &self.recent_hits;
            }
        };

        match database_kanji_candidates(
            database,
            &snapshot,
            query,
            query.encode_utf16().count(),
            &self.dictionary_order,
        ) {
            Ok(mut candidates) => {
                candidates.sort_by_key(|candidate| {
                    (self.dict_priority(candidate.dic_id), candidate.dic_id)
                });
                for candidate in candidates {
                    self.recent_hits.push(candidate.hit);
                    self.recent_parsed_glossaries
                        .push(candidate.parsed_glossary);
                }
            }
            Err(error) => self.last_search_error = Some(error.to_string()),
        }
        &self.recent_hits
    }

    /// Ranks a slice of search hits in place according to query match criteria.
    pub fn rank_hits(&self, query: &str, hits: &mut [SearchHit]) {
        rank_hits(query, hits);
    }

    /// Returns the most recent search results.
    pub fn hits(&self) -> &[SearchHit] {
        &self.recent_hits
    }

    pub fn dictionary_id_for_name(&self, name: &str) -> Option<i64> {
        let snapshot = self.database.as_ref()?.metadata_snapshot().ok()?;
        snapshot
            .names
            .iter()
            .find_map(|(id, title)| (title == name).then_some(*id))
    }

    /// Fetches pitch accent, general frequency rank, and JPDB frequency rank for the term
    /// in a single metadata pass, sorted by `dictionary_order`.
    pub fn get_term_meta(&self, expression: &str, reading: &str) -> (String, String, String) {
        let Some(database) = self.database.as_ref() else {
            return (String::new(), String::new(), String::new());
        };
        let Ok(snapshot) = database.metadata_snapshot() else {
            return (String::new(), String::new(), String::new());
        };
        let raw_rows = database.term_meta_rows_arc_no_refresh(expression);
        if raw_rows.is_empty() {
            return (String::new(), String::new(), String::new());
        }

        let (pitches, freqs) = select_term_pitches_and_frequencies(
            &snapshot,
            &self.dictionary_order,
            &raw_rows,
            expression,
            reading,
        );
        format_term_meta_tuple(reading, &pitches, freqs)
    }

    /// Batched `get_term_meta` across multiple `(expression, reading)` pairs with a single
    /// metadata refresh check and a single batched SQL query for uncached expressions.
    pub fn get_term_meta_batch(&self, terms: &[(&str, &str)]) -> Vec<(String, String, String)> {
        let Some(database) = self.database.as_ref() else {
            return vec![(String::new(), String::new(), String::new()); terms.len()];
        };
        let Ok(snapshot) = database.metadata_snapshot() else {
            return vec![(String::new(), String::new(), String::new()); terms.len()];
        };

        let mut exprs: Vec<String> = Vec::with_capacity(terms.len());
        let mut seen: HashSet<&str> = HashSet::with_capacity(terms.len());
        for &(expr, _) in terms {
            if !expr.is_empty() && seen.insert(expr) {
                exprs.push(expr.to_owned());
            }
        }
        database.preload_term_meta_no_refresh(&exprs);

        terms
            .iter()
            .map(|&(expr, reading)| {
                let raw_rows = database.term_meta_rows_arc_no_refresh(expr);
                if raw_rows.is_empty() {
                    return (String::new(), String::new(), String::new());
                }
                let (pitches, freqs) = select_term_pitches_and_frequencies(
                    &snapshot,
                    &self.dictionary_order,
                    &raw_rows,
                    expr,
                    reading,
                );
                format_term_meta_tuple(reading, &pitches, freqs)
            })
            .collect()
    }

    /// Returns both detailed pitch accents and term frequencies in a single pass over
    /// `term_meta_bank` rows without holding any database lock during JSON parsing.
    pub fn get_pitch_details_and_frequencies(
        &self,
        expression: &str,
        reading: &str,
    ) -> (Vec<TermPitchDetail>, Vec<TermFrequency>) {
        let Some(database) = self.database.as_ref() else {
            return (Vec::new(), Vec::new());
        };
        let Ok(snapshot) = database.metadata_snapshot() else {
            return (Vec::new(), Vec::new());
        };
        let raw_rows = database.term_meta_rows_arc_no_refresh(expression);
        if raw_rows.is_empty() {
            return (Vec::new(), Vec::new());
        }
        let (pitches, freqs) = select_term_pitches_and_frequencies(
            &snapshot,
            &self.dictionary_order,
            &raw_rows,
            expression,
            reading,
        );
        (pitches, selected_freqs_to_public(freqs))
    }

    /// Returns both compact pitch graphs and term frequencies in a single pass.
    pub fn get_pitch_graphs_and_frequencies(
        &self,
        expression: &str,
        reading: &str,
    ) -> (Vec<TermPitchGraph>, Vec<TermFrequency>) {
        let (details, freqs) = self.get_pitch_details_and_frequencies(expression, reading);
        let graphs = details
            .into_iter()
            .map(|detail| TermPitchGraph {
                dictionary: detail.dictionary,
                mora: detail.mora,
                position: detail.position,
            })
            .collect();
        (graphs, freqs)
    }

    /// Returns detailed pitch accents (including `nasal`, `devoice`, and `tags`) sorted by
    /// `dictionary_order` (`i64::MAX` for unlisted dictionaries) with homograph reading filtering.
    pub fn get_pitch_details(&self, expression: &str, reading: &str) -> Vec<TermPitchDetail> {
        let Some(database) = self.database.as_ref() else {
            return Vec::new();
        };
        let Ok(snapshot) = database.metadata_snapshot() else {
            return Vec::new();
        };
        let raw_rows = database.term_meta_rows_arc_no_refresh(expression);
        select_term_pitches(
            &snapshot,
            &self.dictionary_order,
            &raw_rows,
            expression,
            reading,
        )
    }

    /// Return every pitch accent supplied by enabled dictionaries for the popup,
    /// sorted by `dictionary_order` (`i64::MAX` for unlisted dictionaries).
    pub fn get_pitch_graphs(&self, expression: &str, reading: &str) -> Vec<TermPitchGraph> {
        self.get_pitch_details(expression, reading)
            .into_iter()
            .map(|detail| TermPitchGraph {
                dictionary: detail.dictionary,
                mora: detail.mora,
                position: detail.position,
            })
            .collect()
    }

    /// Returns term frequencies sorted by `dictionary_order` (`i64::MAX` for unlisted dictionaries)
    /// with homograph reading filtering.
    pub fn get_frequencies(&self, expression: &str, reading: &str) -> Vec<TermFrequency> {
        let Some(database) = self.database.as_ref() else {
            return Vec::new();
        };
        let Ok(snapshot) = database.metadata_snapshot() else {
            return Vec::new();
        };
        let raw_rows = database.term_meta_rows_arc_no_refresh(expression);
        let items = select_term_frequencies(
            &snapshot,
            &self.dictionary_order,
            &raw_rows,
            expression,
            reading,
        );
        selected_freqs_to_public(items)
    }

    /// Returns term frequencies paired with each dictionary's [`FrequencyMode`]
    /// (`rank-based` vs `occurrence-based`) via Wave 1's `query_term_frequencies_with_mode_conn`,
    /// sorted by `dictionary_order` with homograph reading filtering.
    pub fn get_frequencies_with_mode(
        &self,
        expression: &str,
        reading: &str,
    ) -> Vec<TermFrequencyWithMode> {
        let Some(database) = self.database.as_ref() else {
            return Vec::new();
        };
        if expression.is_empty() || database.refresh_metadata_if_needed().is_err() {
            return Vec::new();
        }
        database
            .with_conn(|conn| {
                query_term_frequencies_with_mode_sorted(
                    conn,
                    &self.dictionary_order,
                    expression,
                    reading,
                )
            })
            .unwrap_or_default()
    }

    /// Returns Kanji frequencies from `kanji_meta_bank` sorted by `dictionary_order`.
    pub fn get_kanji_frequencies(&self, character: &str) -> Vec<TermFrequency> {
        let Some(database) = self.database.as_ref() else {
            return Vec::new();
        };
        if character.is_empty() {
            return Vec::new();
        }
        let Ok(snapshot) = database.metadata_snapshot() else {
            return Vec::new();
        };
        let raw_rows = database.with_conn(|conn| {
            sqlite_db::query_kanji_meta_freq_utf8_conn(conn, character).unwrap_or_default()
        });
        let items = select_term_frequencies(
            &snapshot,
            &self.dictionary_order,
            &raw_rows,
            character,
            "",
        );
        selected_freqs_to_public(items)
    }

    /// Gather all enabled frequency and pitch entries for Anki, querying `term_bank.rules`
    /// for `(expression, reading)` (falling back to `tags` only if no DB row exists) and
    /// separating rank (`"9999999"` default) vs occurrence (`"0"` default) frequency fields.
    pub fn get_anki_term_metadata(
        &self,
        expression: &str,
        reading: &str,
        tags: &[String],
    ) -> AnkiTermMetadata {
        let Some(database) = self.database.as_ref() else {
            return AnkiTermMetadata::default();
        };
        let Ok(snapshot) = database.metadata_snapshot() else {
            return AnkiTermMetadata::default();
        };

        let raw_rows = database.term_meta_rows_arc_no_refresh(expression);
        let (pitch_details, freq_items) = select_term_pitches_and_frequencies(
            &snapshot,
            &self.dictionary_order,
            &raw_rows,
            expression,
            reading,
        );

        let db_rules = database.with_conn(|conn| {
            sqlite_db::query_term_rules_conn(conn, expression, reading)
                .ok()
                .flatten()
        });
        let rule_strings: Vec<String> = match db_rules {
            Some(rules_from_db) => rules_from_db,
            None => tags
                .iter()
                .flat_map(|t| t.split_whitespace().map(str::to_owned))
                .collect(),
        };

        build_anki_term_metadata_from_parts(&freq_items, &pitch_details, &rule_strings)
    }

    /// Gather enabled `kanji_meta_bank` frequency metadata for Anki kanji cards,
    /// separating rank (`"9999999"` default) vs occurrence (`"0"` default) fields.
    pub fn get_anki_kanji_metadata(&self, character: &str) -> AnkiTermMetadata {
        let Some(database) = self.database.as_ref() else {
            return AnkiTermMetadata::default();
        };
        if character.is_empty() {
            return AnkiTermMetadata::default();
        }
        let Ok(snapshot) = database.metadata_snapshot() else {
            return AnkiTermMetadata::default();
        };
        let raw_rows = database.with_conn(|conn| {
            sqlite_db::query_kanji_meta_freq_utf8_conn(conn, character).unwrap_or_default()
        });
        let freq_items =
            select_term_frequencies(&snapshot, &self.dictionary_order, &raw_rows, character, "");
        let freq_summary = compute_anki_frequency_summary(&freq_items);
        let (
            frequency_harmonic_rank,
            frequency_average_rank,
            frequency_harmonic_occurrence,
            frequency_average_occurrence,
        ) = format_rank_and_occurrence(&freq_summary);

        AnkiTermMetadata {
            frequencies: freq_summary.html,
            frequency_harmonic_rank,
            frequency_average_rank,
            frequency_harmonic_occurrence,
            frequency_average_occurrence,
            ..AnkiTermMetadata::default()
        }
    }
}

struct SearchCandidate {
    hit: SearchHit,
    parsed_glossary: Option<Arc<serde_json::Value>>,
    is_exact: bool,
    prefix_len: usize,
    dict_priority: i64,
    dic_id_tiebreak: i64,
    original_index: usize,
}

fn utf16_prefix_slice(s: &str, u16_len: usize) -> &str {
    let mut units = 0usize;
    for (byte_idx, ch) in s.char_indices() {
        if units >= u16_len {
            return &s[..byte_idx];
        }
        units += ch.len_utf16();
    }
    s
}

#[cfg(test)]
mod matcher_and_filter_tests {
    use super::glossary::strip_glossary_markup;
    use super::*;

    fn test_dict_with_noun_and_verb() -> DictController {
        let mut dict = DictController::new();
        let mut noun = SearchHit::new("猫", "ねこ", "cat", "n", 10, false);
        noun.rules = "n".to_owned();
        let mut verb = SearchHit::new("食べる", "たべる", "to eat", "v1", 10, false);
        verb.rules = "v1".to_owned();
        let mut compound = SearchHit::new("猫食", "ねこしょく", "cat food", "n", 5, false);
        compound.rules = "n".to_owned();
        dict.add_entries([noun, verb, compound]);
        dict
    }

    #[test]
    fn strip_glossary_markup_preserves_bare_angle_brackets_and_strips_valid_tags() {
        assert_eq!(strip_glossary_markup("a < b and c > d"), "a < b and c > d");
        assert_eq!(strip_glossary_markup("x < 5"), "x < 5");
        assert_eq!(
            strip_glossary_markup("<p>hello</p><li>item</li>"),
            "hello\n◦ item\n"
        );
        assert_eq!(strip_glossary_markup("foo<br/>bar"), "foo\nbar");
    }

    #[test]
    fn exact_and_deconj_matcher_toggles_gate_queries_while_defaulting_true() {
        let mut dict = test_dict_with_noun_and_verb();
        assert!(dict.exact_enabled());
        assert!(dict.deconj_enabled());
        assert!(!dict.mecab_enabled());

        assert_eq!(
            dict.search("猫").first().map(|h| h.expression.as_str()),
            Some("猫")
        );
        assert_eq!(
            dict.search("食べた").first().map(|h| h.expression.as_str()),
            Some("食べる")
        );

        dict.set_deconj_enabled(false);
        assert!(!dict.deconj_enabled());
        assert_eq!(
            dict.search("猫").first().map(|h| h.expression.as_str()),
            Some("猫")
        );
        assert!(dict.search("食べた").is_empty());

        dict.set_deconj_enabled(true);
        dict.set_exact_enabled(false);
        assert!(!dict.exact_enabled());
        assert!(dict.search("猫").is_empty());
        assert_eq!(
            dict.search("食べた").first().map(|h| h.expression.as_str()),
            Some("食べる")
        );

        dict.set_deconj_enabled(false);
        assert!(dict.search("猫").is_empty());
        assert!(dict.search("食べた").is_empty());
    }

    #[test]
    fn remove_regex_and_replace_lines_preprocess_queries_before_lookup() {
        let mut dict = test_dict_with_noun_and_verb();

        assert!(dict.search("（笑）猫").is_empty());

        dict.set_search_text_filters("（[^）]*）", false, "");
        assert_eq!(dict.remove_regex_pattern(), "（[^）]*）");
        assert!(!dict.replace_lines());
        assert_eq!(
            dict.search("（笑）猫").first().map(|h| h.expression.as_str()),
            Some("猫")
        );

        dict.set_search_text_filters("[unclosed", false, "");
        assert_eq!(
            dict.search("猫").first().map(|h| h.expression.as_str()),
            Some("猫")
        );

        dict.set_search_text_filters("", true, "");
        assert!(dict.replace_lines());
        assert_eq!(dict.replace_lines_with(), "");
        assert_eq!(
            dict.search("猫\n食").first().map(|h| h.expression.as_str()),
            Some("猫食")
        );

        dict.set_search_text_filters("", true, " ");
        assert_eq!(dict.replace_lines_with(), " ");
        assert_eq!(
            dict.search("猫\n食").first().map(|h| h.expression.as_str()),
            Some("猫食")
        );
        dict.set_search_text_filters("", true, "・");
        assert_eq!(
            dict.search("猫\n食").first().map(|h| h.expression.as_str()),
            Some("猫")
        );
    }

    fn create_wave2_test_sqlite_db() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::TempDir::new().unwrap();
        let db_path = dir.path().join("wave2_dict.sqlite");
        let conn = sqlite_db::rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch(
            r#"
            CREATE TABLE directory (
                dic_id INTEGER PRIMARY KEY,
                title TEXT NOT NULL UNIQUE,
                format INTEGER NOT NULL DEFAULT 3,
                revision TEXT NOT NULL DEFAULT '1',
                sequenced INTEGER NOT NULL DEFAULT 0,
                frequency_mode TEXT NOT NULL DEFAULT 'rank-based'
            );
            CREATE TABLE dict_disabled (dic_id INTEGER PRIMARY KEY);
            CREATE TABLE tag_bank (
                dic_id INTEGER NOT NULL,
                name TEXT NOT NULL,
                category TEXT NOT NULL DEFAULT '',
                ord INTEGER NOT NULL DEFAULT 0,
                notes TEXT NOT NULL DEFAULT '',
                score INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY(dic_id, name)
            );
            CREATE TABLE term_bank (
                dic_id INTEGER NOT NULL,
                expression TEXT NOT NULL,
                reading TEXT NOT NULL,
                def_tags TEXT NOT NULL DEFAULT '',
                rules TEXT NOT NULL DEFAULT '',
                score INTEGER NOT NULL DEFAULT 0,
                glossary TEXT NOT NULL DEFAULT '[]',
                sequence INTEGER NOT NULL DEFAULT 0,
                term_tags TEXT NOT NULL DEFAULT ''
            );
            CREATE TABLE kanji_bank (
                dic_id INTEGER NOT NULL,
                char TEXT NOT NULL,
                onyomi TEXT NOT NULL DEFAULT '',
                kunyomi TEXT NOT NULL DEFAULT '',
                tags TEXT NOT NULL DEFAULT '',
                meanings TEXT NOT NULL DEFAULT '[]',
                stats TEXT NOT NULL DEFAULT '{}'
            );
            CREATE TABLE term_meta_bank (
                dic_id INTEGER NOT NULL,
                expression TEXT NOT NULL,
                mode TEXT NOT NULL,
                type INTEGER NOT NULL DEFAULT 0,
                data BLOB
            );
            CREATE TABLE kanji_meta_bank (
                dic_id INTEGER NOT NULL,
                expression TEXT NOT NULL,
                mode TEXT NOT NULL,
                type INTEGER NOT NULL DEFAULT 0,
                data BLOB
            );
            INSERT INTO directory (dic_id, title, format, revision, sequenced, frequency_mode) VALUES
                (1, 'JMdict', 3, '1', 0, 'rank-based'),
                (2, 'Daijisen', 3, '1', 0, 'rank-based'),
                (3, 'KanjiDic', 3, '1', 0, 'rank-based'),
                (4, 'RankFreqA', 3, '1', 0, 'rank-based'),
                (5, 'RankFreqB', 3, '1', 0, 'rank-based'),
                (6, 'OccFreqA', 3, '1', 0, 'occurrence-based'),
                (7, 'OccFreqB', 3, '1', 0, 'occurrence-based'),
                (8, 'Kanjium', 3, '1', 0, 'rank-based');
            INSERT INTO term_bank (dic_id, expression, reading, rules, score, glossary) VALUES
                (1, '食べる', 'たべる', 'v1', 20, '["to eat"]'),
                (2, '食べる', 'たべる', 'v1', 35, '["口に入れてかんで飲み込む"]'),
                (1, '食べる事', 'たべること', 'n', 10, '["eating"]'),
                (1, '学ぶ', 'まなぶ', 'v5', 25, '["to study"]'),
                (1, 'すごい', 'すごい', 'adj-i', 15, '["amazing"]'),
                (1, '橋', 'はし', 'n', 15, '["bridge"]'),
                (1, '端', 'はし', 'n', 30, '["edge"]'),
                (1, 'はし', '', 'n', 5, '["kana match"]');
            INSERT INTO kanji_bank (dic_id, char, onyomi, kunyomi, meanings) VALUES
                (3, '食', 'ショク', 'た.べる', '["eat","food"]'),
                (3, '学', 'ガク', 'まな.ぶ', '["study","learning"]');
            -- Frequency metadata: two rank-based dicts (100, 300) and two occurrence-based dicts (50, 150)
            INSERT INTO term_meta_bank (dic_id, expression, mode, type, data) VALUES
                (4, '食べる', 'freq', 4, CAST('{"reading":"たべる","frequency":{"value":100,"displayValue":"100"}}' AS BLOB)),
                (5, '食べる', 'freq', 4, CAST('{"reading":"たべる","frequency":{"value":300,"displayValue":"300"}}' AS BLOB)),
                (6, '食べる', 'freq', 4, CAST('{"reading":"たべる","frequency":{"value":50,"displayValue":"50"}}' AS BLOB)),
                (7, '食べる', 'freq', 4, CAST('{"reading":"たべる","frequency":{"value":150,"displayValue":"150"}}' AS BLOB));
            -- Pitch metadata: string exact patterns + non-adjacent duplicates within same dict (#2194)
            INSERT INTO term_meta_bank (dic_id, expression, mode, type, data) VALUES
                (8, '食べる', 'pitch', 4, CAST('{"reading":"たべる","pitches":[{"position":"LHL"},{"position":"HLL"},{"position":"LHL"},{"position":2},{"position":0},{"position":2}]}' AS BLOB));
            "#,
        )
        .unwrap();
        drop(conn);
        (dir, db_path)
    }

    #[test]
    fn frequency_mode_separates_rank_and_occurrence_and_sorts_unranked_last() {
        let (_dir, db_path) = create_wave2_test_sqlite_db();
        let dict = DictController::open(&db_path).unwrap();

        let freqs = dict.get_frequencies_with_mode("食べる", "たべる");
        let ranks: Vec<_> = freqs
            .iter()
            .filter(|f| f.frequency_mode == FrequencyMode::RankBased)
            .collect();
        let occs: Vec<_> = freqs
            .iter()
            .filter(|f| f.frequency_mode == FrequencyMode::OccurrenceBased)
            .collect();
        assert_eq!(ranks.len(), 2);
        assert_eq!(occs.len(), 2);

        let meta = dict.get_anki_term_metadata("食べる", "たべる", &[]);
        // Rank-based: (100, 300) -> avg = 200, harmonic = 2 / (1/100 + 1/300) = 150
        assert_eq!(meta.frequency_average_rank, "200");
        assert_eq!(meta.frequency_harmonic_rank, "150");
        // Occurrence-based: (50, 150) -> avg = 100, harmonic = 2 / (1/50 + 1/150) = 75
        assert_eq!(meta.frequency_average_occurrence, "100");
        assert_eq!(meta.frequency_harmonic_occurrence, "75");

        // Unranked (None / <= 0) must ALWAYS sort after ranked values in both modes (#1037/#1684)
        assert_eq!(
            compare_frequency_values(Some(10.0), None, FrequencyMode::RankBased),
            std::cmp::Ordering::Less
        );
        assert_eq!(
            compare_frequency_values(None, Some(10.0), FrequencyMode::RankBased),
            std::cmp::Ordering::Greater
        );
        assert_eq!(
            compare_frequency_values(Some(0.0), Some(500.0), FrequencyMode::RankBased),
            std::cmp::Ordering::Greater
        );
        assert_eq!(
            compare_frequency_values(Some(10.0), Some(100.0), FrequencyMode::RankBased),
            std::cmp::Ordering::Less
        );
        assert_eq!(
            compare_frequency_values(Some(500.0), Some(10.0), FrequencyMode::OccurrenceBased),
            std::cmp::Ordering::Less
        );
        assert_eq!(
            compare_frequency_values(None, Some(1.0), FrequencyMode::OccurrenceBased),
            std::cmp::Ordering::Greater
        );
    }

    #[test]
    fn string_pitch_patterns_and_non_adjacent_pitch_dedup_work_end_to_end() {
        let (_dir, db_path) = create_wave2_test_sqlite_db();
        let dict = DictController::open(&db_path).unwrap();

        let pitches = dict.get_pitch_details("食べる", "たべる");
        // Input had ["LHL", "HLL", "LHL", 2, 0, 2]; non-adjacent duplicate "LHL" and 2 must be deduped (#2194)
        assert_eq!(pitches.len(), 4, "duplicate pitches within dict must be deduplicated");
        assert_eq!(pitches[0].pattern.as_deref(), Some("LHL"));
        assert_eq!(pitches[0].badge_text(), "[LHL]");
        assert_eq!(pitches[0].mora_levels(), (vec![false, true, false], false));
        assert_eq!(pitches[1].pattern.as_deref(), Some("HLL"));
        assert_eq!(pitches[1].badge_text(), "[HLL]");
        assert_eq!(pitches[1].mora_levels(), (vec![true, false, false], false));
        assert_eq!(pitches[2].pattern, None);
        assert_eq!(pitches[2].position, 2);
        assert_eq!(pitches[3].pattern, None);
        assert_eq!(pitches[3].position, 0);

        let anki_meta = dict.get_anki_term_metadata("食べる", "たべる", &[]);
        assert!(
            anki_meta.pitch_positions.contains("LHL")
                || anki_meta.pitch_positions.contains("[2]"),
            "anki pitch positions should include string pattern: {}",
            anki_meta.pitch_positions
        );
        // Duplicate non-adjacent "LHL" must appear at most once in pitch accent output
        assert_eq!(anki_meta.pitch_positions.matches("[LHL]").count(), 1);
    }

    #[test]
    fn wildcard_search_preprocessors_text_replacements_and_kanji_footer() {
        let (_dir, db_path) = create_wave2_test_sqlite_db();
        let mut dict = DictController::open(&db_path).unwrap();

        // 1. Wildcard search via SQLite (*, ?, fullwidth ＊, ？)
        let wild = dict.search("食*");
        assert!(
            wild.iter().any(|h| h.expression == "食べる")
                && wild.iter().any(|h| h.expression == "食べる事"),
            "wildcard search '食*' should match both 食べる and 食べる事, got {:?}",
            wild.iter().map(|h| &h.expression).collect::<Vec<_>>()
        );
        let wild_fw = dict.search("食？？");
        assert!(
            wild_fw.iter().any(|h| h.expression == "食べる"),
            "fullwidth wildcard '食？？' should match 食べる"
        );

        // 2. Japanese preprocessors: kyūjitai -> shinjitai + deconjugation, emphatic collapse, romaji
        let kyu = dict.search("學んだ");
        assert!(
            kyu.iter().any(|h| !h.is_kanji && h.expression == "学ぶ"),
            "kyūjitai conjugated '學んだ' should match '学ぶ'"
        );
        // Kanji footer must also resolve kyūjitai leading char '學' -> '学' below terms!
        assert!(
            kyu.iter().any(|h| h.is_kanji && h.expression == "学"),
            "kanji footer should resolve leading kyūjitai char '學' -> '学'"
        );
        let emph = dict.search("すごーーーい");
        assert!(
            emph.iter().any(|h| h.expression == "すごい"),
            "emphatic sequence 'すごーーーい' should collapse and match 'すごい'"
        );
        let romaji = dict.search("tabeta");
        assert!(
            romaji.iter().any(|h| h.expression == "食べる"),
            "romaji 'tabeta' should convert to hiragana + deconjugate to '食べる'"
        );

        // 3. Kanji footer on multi-char lookup even when `exact_enabled` is false!
        dict.set_exact_enabled(false);
        let multi_no_exact = dict.search("食べた");
        assert!(
            multi_no_exact.iter().any(|h| !h.is_kanji && h.expression == "食べる"),
            "deconjugated term '食べる' should match"
        );
        assert!(
            multi_no_exact.last().is_some_and(|h| h.is_kanji && h.expression == "食"),
            "leading kanji footer '食' must be appended below terms even when exact_enabled == false"
        );
        dict.set_exact_enabled(true);

        // 4. Custom TextReplacementOptions (ordered regex + search_original toggle)
        dict.set_text_replacements(TextReplacementOptions {
            search_original: false,
            rules: vec![TextReplacementRule {
                pattern: "メシ".to_owned(),
                replacement: "食べる".to_owned(),
                ignore_case: false,
                enabled: true,
            }],
        });
        let replaced = dict.search("メシ");
        assert!(
            replaced.iter().any(|h| h.expression == "食べる"),
            "custom text replacement should map メシ -> 食べる"
        );
        // With search_original = false, a rule that rewrites 橋 -> 端 must NOT return 橋
        dict.set_text_replacements(TextReplacementOptions {
            search_original: false,
            rules: vec![TextReplacementRule {
                pattern: "橋".to_owned(),
                replacement: "端".to_owned(),
                ignore_case: false,
                enabled: true,
            }],
        });
        let no_orig = dict.search("橋");
        assert!(no_orig.iter().any(|h| h.expression == "端"));
        assert!(!no_orig.iter().any(|h| h.expression == "橋"));
        dict.set_text_replacements(TextReplacementOptions::default());

        // 5. Score accumulation across dictionaries & expression == cloze_body ahead of reading-only match
        let hashi_hits = dict.search("はし");
        assert_eq!(
            hashi_hits.first().map(|h| h.expression.as_str()),
            Some("はし"),
            "exact expression == cloze_body ('はし', score 5) must rank ahead of reading-only '端' (score 30)"
        );
    }

    #[test]
    fn accumulated_score_across_dictionaries_ranks_multi_dict_group_higher() {
        let mut dict = DictController::new();
        // Group A ("試", "ためし"): score 20 in Dict1 + score 25 in Dict2 = 45 accumulated
        let mut a1 = SearchHit::new("試", "ためし", "trial 1", "n", 20, false);
        a1.dict_name = "Dict1".to_owned();
        let mut a2 = SearchHit::new("試", "ためし", "trial 2", "n", 25, false);
        a2.dict_name = "Dict2".to_owned();
        // Group B ("試", "し"): single dict with score 35 (< 45 accumulated, > 25 max single)
        let mut b1 = SearchHit::new("試", "し", "test", "n", 35, false);
        b1.dict_name = "Dict1".to_owned();
        dict.add_entries([b1, a1, a2]);

        let hits = dict.search("試");
        assert_eq!(hits.len(), 3);
        assert_eq!(
            hits[0].reading, "ためし",
            "group with accumulated score 45 must rank ahead of single-dict score 35"
        );
        assert_eq!(hits[1].reading, "ためし");
        assert_eq!(hits[2].reading, "し");
    }
}

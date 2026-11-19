//! Dictionary controller connecting memento-dict and memento-util.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use memento_dict::databasemanager;
use memento_dict::deconjugationquerygenerator;
use memento_dict::exactquerygenerator;
use memento_dict::mecabquerygenerator;
use memento_dict::normalize::{self, Regex};
use memento_dict::searchcontroller::{
    self, CandidateSearchQuery, QuerySource, TagSortItem,
};
use memento_dict::sqlite_db;
use memento_util::mecab::MecabLookupToken;

type MecabTokenizerFn = fn(&str) -> Result<Vec<MecabLookupToken>, String>;

pub use memento_dict::databasemanager::{
    build_dict_id_style_path, build_style_path, resolve_dictionary_style_path,
};
pub use memento_dict::normalize::{
    apply_remove_regex, apply_replace_lines, preprocess_search_text,
};
pub use memento_dict::searchcontroller::ClozeSpans;
pub use memento_dict::sqlite_db::{db_file_fingerprint, DbFileFingerprint, FileMetaStamp};

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

/// Full pitch accent entry including nasalized/devoiced mora indices and tags.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TermPitchDetail {
    pub dictionary: String,
    pub mora: Vec<String>,
    pub position: i32,
    pub nasal: Vec<usize>,
    pub devoice: Vec<usize>,
    pub tags: Vec<String>,
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

/// Reading normalization using full Unicode NFKC + katakana-to-hiragana folding.
pub fn normalize_reading(text: &str) -> String {
    normalize::normalize_reading_str(text)
}

/// Produces the full Unicode NFKC form of `text` (converting halfwidth katakana,
/// fullwidth ASCII/alphanumeric/punctuation, and combining voiced marks).
pub fn normalize_nfkc_query(text: &str) -> String {
    normalize::normalize_nfkc(text)
}

/// Generates prefix queries from text using `memento_dict::exactquerygenerator::generate_queries`.
pub fn generate_prefix_queries(text: &str) -> Vec<String> {
    let u16_text: Vec<u16> = text.encode_utf16().collect();
    let queries = exactquerygenerator::generate_queries(&u16_text);
    queries
        .into_iter()
        .map(|q| String::from_utf16_lossy(&q.deconj))
        .collect()
}

/// Formats tags by sorting them with `memento_dict::searchcontroller::sort_tags`
/// and joining their names with comma separation.
pub fn format_tags(tags: &[DictTag]) -> String {
    if tags.is_empty() {
        return String::new();
    }
    let items: Vec<TagSortItem> = tags
        .iter()
        .enumerate()
        .map(|(i, t)| TagSortItem {
            original_index: i,
            order: t.order,
            score: t.score,
        })
        .collect();
    let sorted_indices = searchcontroller::sort_tags(items);
    sorted_indices
        .into_iter()
        .map(|i| tags[i].name.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Formats tags with notes (e.g. "name: notes").
pub fn format_tags_with_notes(tags: &[DictTag]) -> String {
    if tags.is_empty() {
        return String::new();
    }
    let items: Vec<TagSortItem> = tags
        .iter()
        .enumerate()
        .map(|(i, t)| TagSortItem {
            original_index: i,
            order: t.order,
            score: t.score,
        })
        .collect();
    let sorted_indices = searchcontroller::sort_tags(items);
    sorted_indices
        .into_iter()
        .map(|i| {
            let t = &tags[i];
            if t.notes.is_empty() {
                t.name.clone()
            } else {
                format!("{}: {}", t.name, t.notes)
            }
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// Slices a sentence into cloze spans matching `memento_dict::searchcontroller::cloze_spans`.
pub fn format_cloze_spans(sentence: &str, index: i64, surface_len: u64) -> ClozeSentence {
    let u16_sentence: Vec<u16> = sentence.encode_utf16().collect();
    let text_len = u16_sentence.len() as u64;
    let spans = searchcontroller::cloze_spans(text_len, index, surface_len);

    let p_start = spans.prefix_start as usize;
    let p_end = (spans.prefix_start + spans.prefix_len) as usize;
    let b_start = spans.body_start as usize;
    let b_end = (spans.body_start + spans.body_len) as usize;
    let s_start = spans.suffix_start as usize;
    let s_end = (spans.suffix_start + spans.suffix_len) as usize;

    let prefix = u16_sentence
        .get(p_start..p_end)
        .map(String::from_utf16_lossy)
        .unwrap_or_default();
    let body = u16_sentence
        .get(b_start..b_end)
        .map(String::from_utf16_lossy)
        .unwrap_or_default();
    let suffix = u16_sentence
        .get(s_start..s_end)
        .map(String::from_utf16_lossy)
        .unwrap_or_default();

    ClozeSentence::new(prefix, body, suffix)
}

/// Formats a cloze sentence for a selected term in a sentence.
pub fn format_cloze_for_term(sentence: &str, term: &str) -> Option<ClozeSentence> {
    let u16_sentence: Vec<u16> = sentence.encode_utf16().collect();
    let u16_term: Vec<u16> = term.encode_utf16().collect();
    if u16_term.is_empty() {
        return None;
    }

    let pos = u16_sentence
        .windows(u16_term.len())
        .position(|w| w == u16_term.as_slice())?;

    Some(format_cloze_spans(
        sentence,
        pos as i64,
        u16_term.len() as u64,
    ))
}

/// Sorts search hits: exact matches first, longest prefix matches, breaking ties by score.
pub fn rank_hits(query: &str, hits: &mut [SearchHit]) {
    let trimmed = query.trim();
    let norm_query = normalize_reading(trimmed);
    let u16_query: Vec<u16> = trimmed.encode_utf16().collect();

    let mut indexed: Vec<(usize, bool, usize)> = hits
        .iter()
        .enumerate()
        .map(|(i, h)| {
            let norm_reading = normalize_reading(&h.reading);
            let normalized_expression = normalize_reading(&h.expression);
            let is_exact = h.expression == trimmed
                || normalized_expression == norm_query
                || (!norm_reading.is_empty() && norm_reading == norm_query);

            let u16_expr: Vec<u16> = h.expression.encode_utf16().collect();
            let prefix_len = if u16_query.starts_with(&u16_expr) {
                u16_expr.len()
            } else if u16_expr.starts_with(&u16_query) {
                u16_query.len()
            } else {
                let u16_read: Vec<u16> = h.reading.encode_utf16().collect();
                if u16_query.starts_with(&u16_read) {
                    u16_read.len()
                } else if u16_read.starts_with(&u16_query) {
                    u16_query.len()
                } else {
                    0
                }
            };

            (i, is_exact, prefix_len)
        })
        .collect();

    indexed.sort_by(|a, b| {
        b.1.cmp(&a.1)
            .then_with(|| b.2.cmp(&a.2))
            .then_with(|| hits[b.0].score.cmp(&hits[a.0].score))
            .then_with(|| a.0.cmp(&b.0))
    });

    let mut new_hits = Vec::with_capacity(hits.len());
    for item in indexed {
        new_hits.push(hits[item.0].clone());
    }
    hits.clone_from_slice(&new_hits);
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

struct DictionaryMetadataCache {
    data_version: Option<i64>,
    fingerprint: sqlite_db::DbFileFingerprint,
    tags: HashMap<(i64, String), DictTag>,
    names: HashMap<i64, String>,
    disabled: HashSet<i64>,
    recent_term_meta: HashMap<String, Vec<sqlite_db::RawTermMetaRow>>,
}

struct ReadOnlyDictionaryDb {
    path: PathBuf,
    conn: Mutex<sqlite_db::rusqlite::Connection>,
    cache: Mutex<DictionaryMetadataCache>,
}

impl ReadOnlyDictionaryDb {
    fn refresh_metadata_if_needed(&self) -> Result<(), sqlite_db::DictError> {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let current_version = sqlite_db::query_data_version_conn(&conn).ok();
        let current_fingerprint = sqlite_db::db_file_fingerprint(&self.path);
        let mut cache = self
            .cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if cache.data_version == current_version && cache.fingerprint == current_fingerprint {
            return Ok(());
        }
        cache.tags = load_dictionary_tags_conn(&conn)?;
        let (names, disabled) = load_dictionary_names_and_disabled_conn(&conn)?;
        cache.names = names;
        cache.disabled = disabled;
        cache.recent_term_meta.clear();
        cache.data_version = current_version;
        cache.fingerprint = current_fingerprint;
        Ok(())
    }

    fn with_cache<R>(&self, f: impl FnOnce(&DictionaryMetadataCache) -> R) -> R {
        let _ = self.refresh_metadata_if_needed();
        let cache = self
            .cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        f(&cache)
    }

    fn preload_term_meta(&self, expressions: &[String]) {
        if expressions.is_empty() {
            return;
        }
        if self.refresh_metadata_if_needed().is_err() {
            return;
        }
        let missing: Vec<String> = {
            let cache = self
                .cache
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            expressions
                .iter()
                .filter(|expr| !cache.recent_term_meta.contains_key(expr.as_str()))
                .cloned()
                .collect()
        };
        if missing.is_empty() {
            return;
        }
        let fetched = {
            let conn = self
                .conn
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            sqlite_db::query_term_meta_batch_conn(&conn, &missing)
        };
        let Ok(rows) = fetched else {
            return;
        };
        let mut cache = self
            .cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // Bound cache size so long sessions never accumulate unbounded RAM.
        if cache.recent_term_meta.len() > 2048 {
            cache.recent_term_meta.clear();
        }
        for expr in &missing {
            cache.recent_term_meta.entry(expr.clone()).or_default();
        }
        for row in rows {
            cache
                .recent_term_meta
                .entry(row.expression.clone())
                .or_default()
                .push(row);
        }
    }

    fn term_meta_rows_for(&self, expression: &str) -> Vec<sqlite_db::RawTermMetaRow> {
        if expression.is_empty() || self.refresh_metadata_if_needed().is_err() {
            return Vec::new();
        }
        {
            let cache = self
                .cache
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(cached) = cache.recent_term_meta.get(expression) {
                return cached.clone();
            }
        }
        let expr_owned = expression.to_owned();
        self.preload_term_meta(std::slice::from_ref(&expr_owned));
        let cache = self
            .cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        cache
            .recent_term_meta
            .get(expression)
            .cloned()
            .unwrap_or_default()
    }
}

/// The dictionary search controller.
pub struct DictController {
    dictionaries: Vec<InMemoryDictionary>,
    database: Option<ReadOnlyDictionaryDb>,
    recent_hits: Vec<SearchHit>,
    last_search_error: Option<String>,
    last_match_units: usize,
    dictionary_order: HashMap<i64, i64>,
    remove_regex_pattern: String,
    remove_regex: Option<Regex>,
    replace_lines: bool,
    replace_lines_with: String,
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
            last_search_error: None,
            last_match_units: 0,
            dictionary_order: HashMap::new(),
            remove_regex_pattern: String::new(),
            remove_regex: None,
            replace_lines: false,
            replace_lines_with: String::new(),
            mecab_enabled: false,
            mecab_tokenizer: memento_util::mecab::mecab_tokenize_for_lookup,
        }
    }

    /// Open Memento's persisted Yomitan database read-only. A missing database
    /// is reported to the caller; this function never creates an empty file.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, DictControllerError> {
        let path = path.as_ref();
        if !path.is_file() {
            return Err(DictControllerError::MissingDatabase(path.to_path_buf()));
        }
        if path.to_str().is_some_and(|s| s.contains('\0')) {
            return Err(DictControllerError::InvalidPath(
                "database path contains interior NUL byte".to_owned(),
            ));
        }

        let conn =
            sqlite_db::open_read_only_connection(path).map_err(DictControllerError::Sqlite)?;
        sqlite_db::validate_schema_conn(&conn).map_err(DictControllerError::Sqlite)?;
        let tags = load_dictionary_tags_conn(&conn).map_err(DictControllerError::Sqlite)?;
        let (names, disabled) =
            load_dictionary_names_and_disabled_conn(&conn).map_err(DictControllerError::Sqlite)?;
        let data_version = sqlite_db::query_data_version_conn(&conn).ok();
        let fingerprint = sqlite_db::db_file_fingerprint(path);

        let database = ReadOnlyDictionaryDb {
            path: path.to_path_buf(),
            conn: Mutex::new(conn),
            cache: Mutex::new(DictionaryMetadataCache {
                data_version,
                fingerprint,
                tags,
                names,
                disabled,
                recent_term_meta: HashMap::new(),
            }),
        };
        Ok(Self {
            dictionaries: Vec::new(),
            database: Some(database),
            recent_hits: Vec::new(),
            last_search_error: None,
            last_match_units: 0,
            dictionary_order: HashMap::new(),
            remove_regex_pattern: String::new(),
            remove_regex: None,
            replace_lines: false,
            replace_lines_with: String::new(),
            mecab_enabled: false,
            mecab_tokenizer: memento_util::mecab::mecab_tokenize_for_lookup,
        })
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
            .and_then(|db| db.with_cache(|cache| cache.names.get(&dic_id).cloned()))
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

    /// Search persisted terms, exact kanji rows, and registered in-memory entries.
    /// An empty result means the query had no real match; no synthetic fallback is added.
    pub fn search(&mut self, query: &str) -> &[SearchHit] {
        self.recent_hits.clear();
        self.last_match_units = 0;
        self.last_search_error = None;

        let after_regex = normalize::apply_compiled_remove_regex(query, self.remove_regex.as_ref());
        let preprocessed = if self.replace_lines {
            normalize::apply_replace_lines(&after_regex, true, &self.replace_lines_with)
        } else {
            after_regex.into_owned()
        };
        let trimmed = preprocessed.trim();
        if trimmed.is_empty() {
            return &self.recent_hits;
        }

        let norm_query = normalize_reading(trimmed);
        let full_u16_len = trimmed.encode_utf16().count();
        let lookups =
            build_lookup_queries(trimmed, self.mecab_enabled, self.mecab_tokenizer);

        struct Candidate {
            hit: SearchHit,
            is_exact: bool,
            prefix_len: usize,
            dict_priority: i64,
            dic_id_tiebreak: i64,
            original_index: usize,
        }

        let mut candidates: Vec<Candidate> = Vec::new();
        let mut seen_database_rows = HashSet::new();

        let database_candidates = self.database.as_ref().map(|database| {
            database_search_candidates(database, trimmed, &norm_query, full_u16_len, &lookups)
        });
        if let Some(Err(error)) = database_candidates.as_ref() {
            self.last_search_error = Some(error.to_string());
        }
        if let Some(Ok(database_candidates)) = database_candidates {
            for candidate in database_candidates {
                if let Some(source_row_id) = candidate.source_row_id {
                    if !seen_database_rows.insert((candidate.dic_id, source_row_id)) {
                        continue;
                    }
                }
                let dict_priority = self.dict_priority(candidate.dic_id);
                candidates.push(Candidate {
                    hit: candidate.hit,
                    is_exact: candidate.is_exact,
                    prefix_len: candidate.prefix_len,
                    dict_priority,
                    dic_id_tiebreak: candidate.dic_id,
                    original_index: candidates.len(),
                });
            }
        }

        let database_kanji_candidates = if trimmed.chars().count() == 1 {
            self.database.as_ref().map(|database| {
                database_kanji_candidates(database, trimmed, full_u16_len, &self.dictionary_order)
            })
        } else {
            None
        };
        if let Some(Err(error)) = database_kanji_candidates.as_ref() {
            self.last_search_error = Some(error.to_string());
        }
        if let Some(Ok(database_kanji_candidates)) = database_kanji_candidates {
            for candidate in database_kanji_candidates {
                let dict_priority = self.dict_priority(candidate.dic_id);
                candidates.push(Candidate {
                    hit: candidate.hit,
                    is_exact: candidate.is_exact,
                    prefix_len: candidate.prefix_len,
                    dict_priority,
                    dic_id_tiebreak: candidate.dic_id,
                    original_index: candidates.len(),
                });
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
                            candidates.push(Candidate {
                                hit,
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

        // Exact matches precede longer prefix matches, with term hits ahead of
        // kanji definitions for a single-character query. Dictionary order and
        // score then break ties while retaining source order.
        candidates.sort_by(|a, b| {
            b.is_exact
                .cmp(&a.is_exact)
                .then_with(|| b.prefix_len.cmp(&a.prefix_len))
                .then_with(|| a.hit.is_kanji.cmp(&b.hit.is_kanji))
                .then_with(|| a.dict_priority.cmp(&b.dict_priority))
                .then_with(|| b.hit.score.cmp(&a.hit.score))
                .then_with(|| a.dic_id_tiebreak.cmp(&b.dic_id_tiebreak))
                .then_with(|| a.original_index.cmp(&b.original_index))
        });

        self.last_match_units = candidates.first().map_or(0, |candidate| candidate.prefix_len);
        self.recent_hits = candidates.into_iter().map(|candidate| candidate.hit).collect();

        // Batch-preload `term_meta_bank` rows for the matched term expressions so
        // immediate hover metadata/pitch/frequency queries complete with zero extra SQL round-trips.
        if let Some(database) = self.database.as_ref() {
            let mut exprs: Vec<String> = Vec::new();
            for hit in &self.recent_hits {
                if !hit.is_kanji && !exprs.iter().any(|e| e == &hit.expression) {
                    exprs.push(hit.expression.clone());
                }
            }
            database.preload_term_meta(&exprs);
        }

        &self.recent_hits
    }

    /// UTF-16 length of the query text matched by the first hit of the last search.
    pub fn last_match_units(&self) -> usize {
        self.last_match_units
    }

    /// Search exact kanji definitions from the persisted dictionaries.
    /// Kanji rows have no score column, so results use configured dictionary priority
    /// (with unlisted dictionaries receiving `i64::MAX`).
    pub fn search_kanji(&mut self, query: &str) -> &[SearchHit] {
        self.recent_hits.clear();
        self.last_search_error = None;
        if query.is_empty() {
            return &self.recent_hits;
        }
        let Some(database) = self.database.as_ref() else {
            return &self.recent_hits;
        };

        match database_kanji_candidates(
            database,
            query,
            query.encode_utf16().count(),
            &self.dictionary_order,
        ) {
            Ok(mut candidates) => {
                candidates.sort_by_key(|candidate| {
                    (self.dict_priority(candidate.dic_id), candidate.dic_id)
                });
                self.recent_hits = candidates.into_iter().map(|candidate| candidate.hit).collect();
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
        self.database.as_ref()?.with_cache(|cache| {
            cache
                .names
                .iter()
                .find_map(|(id, title)| (title == name).then_some(*id))
        })
    }

    /// Fetches pitch accent, general frequency rank, and JPDB frequency rank for the term,
    /// sorted by `dictionary_order` and guarded against homograph reading collisions.
    pub fn get_term_meta(&self, expression: &str, reading: &str) -> (String, String, String) {
        let Some(database) = self.database.as_ref() else {
            return (String::new(), String::new(), String::new());
        };
        let raw_rows = database.term_meta_rows_for(expression);
        if raw_rows.is_empty() {
            return (String::new(), String::new(), String::new());
        }

        let freqs = select_term_frequencies(
            database,
            &self.dictionary_order,
            &raw_rows,
            expression,
            reading,
        );
        let pitches = select_term_pitches(
            database,
            &self.dictionary_order,
            &raw_rows,
            expression,
            reading,
        );

        let mut freq_rank = String::new();
        let mut jpdb_rank = String::new();
        for item in freqs {
            if item.parsed.display.is_empty() {
                continue;
            }
            if item.dictionary.to_ascii_uppercase().contains("JPDB") && jpdb_rank.is_empty() {
                jpdb_rank = item.parsed.display;
            } else if freq_rank.is_empty() {
                freq_rank = format!("{} {}", item.dictionary, item.parsed.display);
            }
            if !freq_rank.is_empty() && !jpdb_rank.is_empty() {
                break;
            }
        }

        let pitch_accent = pitches
            .first()
            .map(|p| {
                if reading.is_empty() {
                    format!("[{}]", p.position)
                } else {
                    format!("{reading} [{}]", p.position)
                }
            })
            .unwrap_or_default();

        (pitch_accent, freq_rank, jpdb_rank)
    }

    /// Batched `get_term_meta` across multiple `(expression, reading)` pairs in a single SQL query.
    pub fn get_term_meta_batch(&self, terms: &[(&str, &str)]) -> Vec<(String, String, String)> {
        if let Some(database) = self.database.as_ref() {
            let mut exprs: Vec<String> = Vec::with_capacity(terms.len());
            for &(expr, _) in terms {
                if !expr.is_empty() && !exprs.iter().any(|e| e == expr) {
                    exprs.push(expr.to_owned());
                }
            }
            database.preload_term_meta(&exprs);
        }
        terms
            .iter()
            .map(|&(expr, reading)| self.get_term_meta(expr, reading))
            .collect()
    }

    /// Returns detailed pitch accents (including `nasal`, `devoice`, and `tags`) sorted by
    /// `dictionary_order` (`i64::MAX` for unlisted dictionaries) with homograph reading filtering.
    pub fn get_pitch_details(&self, expression: &str, reading: &str) -> Vec<TermPitchDetail> {
        let Some(database) = self.database.as_ref() else {
            return Vec::new();
        };
        let raw_rows = database.term_meta_rows_for(expression);
        select_term_pitches(
            database,
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
        let raw_rows = database.term_meta_rows_for(expression);
        select_term_frequencies(
            database,
            &self.dictionary_order,
            &raw_rows,
            expression,
            reading,
        )
        .into_iter()
        .map(|item| TermFrequency {
            dictionary: item.dictionary,
            value: item.parsed.display,
        })
        .collect()
    }

    /// Returns Kanji frequencies from `kanji_meta_bank` sorted by `dictionary_order`.
    pub fn get_kanji_frequencies(&self, character: &str) -> Vec<TermFrequency> {
        let Some(database) = self.database.as_ref() else {
            return Vec::new();
        };
        if character.is_empty() || database.refresh_metadata_if_needed().is_err() {
            return Vec::new();
        }
        let raw_rows = {
            let conn = database
                .conn
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            sqlite_db::query_kanji_meta_freq_utf8_conn(&conn, character).unwrap_or_default()
        };
        select_term_frequencies(database, &self.dictionary_order, &raw_rows, character, "")
            .into_iter()
            .map(|item| TermFrequency {
                dictionary: item.dictionary,
                value: item.parsed.display,
            })
            .collect()
    }

    /// Gather all enabled frequency and pitch entries for Anki, querying `term_bank.rules`
    /// for `(expression, reading)` (falling back to `tags` only if no DB row exists) and
    /// computing true Yomitan harmonic (`N / sum(1/r_i)`) and arithmetic means.
    pub fn get_anki_term_metadata(
        &self,
        expression: &str,
        reading: &str,
        tags: &[String],
    ) -> AnkiTermMetadata {
        let Some(database) = self.database.as_ref() else {
            return AnkiTermMetadata::default();
        };
        if database.refresh_metadata_if_needed().is_err() {
            return AnkiTermMetadata::default();
        }

        let raw_rows = database.term_meta_rows_for(expression);
        let freq_items = select_term_frequencies(
            database,
            &self.dictionary_order,
            &raw_rows,
            expression,
            reading,
        );
        let pitch_details = select_term_pitches(
            database,
            &self.dictionary_order,
            &raw_rows,
            expression,
            reading,
        );

        let (frequencies_html, harmonic, average) = compute_anki_frequency_summary(&freq_items);

        // Group consecutive pitch entries from the same dictionary/mora so `format_pitch`
        // and `format_pitch_positions` match Memento's multi-position grouping.
        let mut grouped_pitches: Vec<(Vec<u16>, Vec<u8>, Vec<Vec<u16>>)> = Vec::new();
        for detail in &pitch_details {
            let Ok(pos_u8) = u8::try_from(detail.position) else {
                continue;
            };
            if detail.mora.is_empty() {
                continue;
            }
            let dict_u16: Vec<u16> = detail.dictionary.encode_utf16().collect();
            let mora_u16: Vec<Vec<u16>> = detail
                .mora
                .iter()
                .map(|m| m.encode_utf16().collect())
                .collect();
            if let Some(last) = grouped_pitches.last_mut() {
                if last.0 == dict_u16 && last.2 == mora_u16 {
                    last.1.push(pos_u8);
                    continue;
                }
            }
            grouped_pitches.push((dict_u16, vec![pos_u8], mora_u16));
        }

        let mora_refs: Vec<Vec<&[u16]>> = grouped_pitches
            .iter()
            .map(|(_, _, mora)| mora.iter().map(Vec::as_slice).collect())
            .collect();
        let pitches: Vec<_> = grouped_pitches
            .iter()
            .enumerate()
            .map(|(index, (name, positions, _))| memento_anki::note::PitchEntry {
                dictionary_name: name,
                positions,
                mora: &mora_refs[index],
            })
            .collect();
        let categories: Vec<_> = grouped_pitches
            .iter()
            .map(|(_, positions, mora)| memento_anki::note::PitchCategoryInput {
                mora_len: mora.len(),
                positions,
            })
            .collect();

        let db_rules = {
            let conn = database
                .conn
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            sqlite_db::query_term_rules_conn(&conn, expression, reading)
                .ok()
                .flatten()
        };
        let rule_strings: Vec<String> = match db_rules {
            Some(rules_from_db) => rules_from_db,
            None => tags
                .iter()
                .flat_map(|t| t.split_whitespace().map(str::to_owned))
                .collect(),
        };
        let rule_units: Vec<Vec<u16>> = rule_strings
            .iter()
            .map(|rule| rule.encode_utf16().collect())
            .collect();
        let rules: Vec<&[u16]> = rule_units.iter().map(Vec::as_slice).collect();
        let can_be_kifuku = memento_anki::note::is_kifuku_applicable(&rules);

        let harmonic_str = harmonic.map(|v| v.to_string()).unwrap_or_default();
        let average_str = average.map(|v| v.to_string()).unwrap_or_default();

        AnkiTermMetadata {
            pitch: String::from_utf16_lossy(&memento_anki::note::format_pitch(&pitches)),
            pitch_positions: String::from_utf16_lossy(&memento_anki::note::format_pitch_positions(
                &pitches,
            )),
            pitch_categories: String::from_utf16_lossy(
                &memento_anki::note::format_pitch_categories(&categories, can_be_kifuku),
            ),
            frequencies: frequencies_html,
            frequency_harmonic_rank: harmonic_str.clone(),
            frequency_average_rank: average_str.clone(),
            frequency_harmonic_occurrence: harmonic_str,
            frequency_average_occurrence: average_str,
        }
    }

    /// Gather enabled `kanji_meta_bank` frequency metadata for Anki kanji cards.
    pub fn get_anki_kanji_metadata(&self, character: &str) -> AnkiTermMetadata {
        let Some(database) = self.database.as_ref() else {
            return AnkiTermMetadata::default();
        };
        if character.is_empty() || database.refresh_metadata_if_needed().is_err() {
            return AnkiTermMetadata::default();
        }
        let raw_rows = {
            let conn = database
                .conn
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            sqlite_db::query_kanji_meta_freq_utf8_conn(&conn, character).unwrap_or_default()
        };
        let freq_items =
            select_term_frequencies(database, &self.dictionary_order, &raw_rows, character, "");
        let (frequencies_html, harmonic, average) = compute_anki_frequency_summary(&freq_items);
        let harmonic_str = harmonic.map(|v| v.to_string()).unwrap_or_default();
        let average_str = average.map(|v| v.to_string()).unwrap_or_default();

        AnkiTermMetadata {
            frequencies: frequencies_html,
            frequency_harmonic_rank: harmonic_str.clone(),
            frequency_average_rank: average_str.clone(),
            frequency_harmonic_occurrence: harmonic_str,
            frequency_average_occurrence: average_str,
            ..AnkiTermMetadata::default()
        }
    }
}

struct SelectedFreqItem {
    dictionary: String,
    parsed: sqlite_db::ParsedFreqUtf8,
}

fn select_term_frequencies(
    database: &ReadOnlyDictionaryDb,
    dictionary_order: &HashMap<i64, i64>,
    raw_rows: &[sqlite_db::RawTermMetaRow],
    expression: &str,
    caller_reading: &str,
) -> Vec<SelectedFreqItem> {
    database.with_cache(|cache| {
        let mut by_dict: HashMap<i64, (bool, Vec<sqlite_db::ParsedFreqUtf8>)> = HashMap::new();
        let mut dict_ids: Vec<i64> = Vec::new();

        for row in raw_rows {
            if row.mode != "freq" || cache.disabled.contains(&row.dic_id) {
                continue;
            }
            if !cache.names.contains_key(&row.dic_id) {
                continue;
            }
            let parsed =
                sqlite_db::parse_freq_blob_utf8(row.blob_type, &row.data, expression, caller_reading);
            let entry = by_dict.entry(row.dic_id).or_insert_with(|| {
                dict_ids.push(row.dic_id);
                (false, Vec::new())
            });
            if parsed.has_reading_key {
                entry.0 = true;
            }
            if parsed.matched {
                entry.1.push(parsed);
            }
        }

        dict_ids.sort_by_key(|&id| {
            (
                dictionary_order.get(&id).copied().unwrap_or(i64::MAX),
                id,
            )
        });

        let mut out = Vec::new();
        for dic_id in dict_ids {
            let Some((saw_reading_key, items)) = by_dict.remove(&dic_id) else {
                continue;
            };
            let Some(dict_name) = cache.names.get(&dic_id) else {
                continue;
            };
            let has_exact = !caller_reading.is_empty()
                && items.iter().any(|item| item.exact_reading_match);
            for parsed in items {
                if !caller_reading.is_empty() {
                    if has_exact && !parsed.exact_reading_match {
                        continue;
                    }
                    if !has_exact && saw_reading_key {
                        continue;
                    }
                }
                out.push(SelectedFreqItem {
                    dictionary: dict_name.clone(),
                    parsed,
                });
            }
        }
        out
    })
}

fn select_term_pitches(
    database: &ReadOnlyDictionaryDb,
    dictionary_order: &HashMap<i64, i64>,
    raw_rows: &[sqlite_db::RawTermMetaRow],
    expression: &str,
    caller_reading: &str,
) -> Vec<TermPitchDetail> {
    database.with_cache(|cache| {
        let mut by_dict: HashMap<i64, (bool, Vec<databasemanager::PitchBlobUtf8Data>)> =
            HashMap::new();
        let mut dict_ids: Vec<i64> = Vec::new();

        for row in raw_rows {
            if row.mode != "pitch" || cache.disabled.contains(&row.dic_id) {
                continue;
            }
            if !cache.names.contains_key(&row.dic_id) {
                continue;
            }
            let null_pos = row.data.iter().position(|&b| b == 0).unwrap_or(row.data.len());
            let parsed = databasemanager::parse_pitch_blob_utf8(
                &row.data[..null_pos],
                expression,
                caller_reading,
            );
            let entry = by_dict.entry(row.dic_id).or_insert_with(|| {
                dict_ids.push(row.dic_id);
                (false, Vec::new())
            });
            if parsed.has_reading_key {
                entry.0 = true;
            }
            if parsed.matched {
                entry.1.push(parsed);
            }
        }

        dict_ids.sort_by_key(|&id| {
            (
                dictionary_order.get(&id).copied().unwrap_or(i64::MAX),
                id,
            )
        });

        let mut out = Vec::new();
        for dic_id in dict_ids {
            let Some((saw_reading_key, items)) = by_dict.remove(&dic_id) else {
                continue;
            };
            let Some(dict_name) = cache.names.get(&dic_id) else {
                continue;
            };
            let has_exact = !caller_reading.is_empty()
                && items.iter().any(|item| item.exact_reading_match);
            for parsed in items {
                if !caller_reading.is_empty() {
                    if has_exact && !parsed.exact_reading_match {
                        continue;
                    }
                    if !has_exact && saw_reading_key {
                        continue;
                    }
                }
                for entry in parsed.entries {
                    if entry.position < 0 || (entry.position as usize) > parsed.mora.len() {
                        continue;
                    }
                    out.push(TermPitchDetail {
                        dictionary: dict_name.clone(),
                        mora: parsed.mora.clone(),
                        position: entry.position,
                        nasal: entry.nasal,
                        devoice: entry.devoice,
                        tags: entry.tags,
                    });
                }
            }
        }
        out
    })
}

fn compute_anki_frequency_summary(
    freq_items: &[SelectedFreqItem],
) -> (String, Option<i64>, Option<i64>) {
    let owned_u16: Vec<(Vec<u16>, Vec<u16>)> = freq_items
        .iter()
        .map(|item| {
            (
                item.dictionary.encode_utf16().collect(),
                item.parsed.display.encode_utf16().collect(),
            )
        })
        .collect();
    let entries: Vec<_> = owned_u16
        .iter()
        .map(|(dict, freq)| memento_anki::note::FrequencyEntry {
            dictionary: dict,
            freq,
        })
        .collect();
    let html = String::from_utf16_lossy(&memento_anki::note::build_frequencies_html(
        &entries, false, false,
    ));

    // Select first positive numeric value per dictionary (in caller priority order)
    // and compute true Yomitan harmonic mean `floor(N / sum(1 / r_i))` and average.
    let mut seen_dicts: HashSet<&str> = HashSet::new();
    let mut numbers: Vec<f64> = Vec::new();
    for item in freq_items {
        if item.dictionary.is_empty() || seen_dicts.contains(item.dictionary.as_str()) {
            continue;
        }
        if let Some(val) = item.parsed.numeric {
            if val.is_finite() && val > 0.0 {
                seen_dicts.insert(item.dictionary.as_str());
                numbers.push(val);
            }
        }
    }

    if numbers.is_empty() {
        return (html, None, None);
    }

    let inv_sum: f64 = numbers.iter().map(|&r| 1.0 / r).sum();
    let harmonic = (inv_sum > 0.0 && inv_sum.is_finite())
        .then(|| ((numbers.len() as f64) / inv_sum).floor() as i64);
    let sum: f64 = numbers.iter().sum();
    let average = sum
        .is_finite()
        .then(|| (sum / (numbers.len() as f64)).floor() as i64);

    (html, harmonic, average)
}

/// One candidate lookup key together with its matched UTF-16 surface length,
/// required Yomitan `rule_mask` (`0` for exact uninflected queries), and derivation chain.
struct LookupQuery {
    key: String,
    deconj_u16: Vec<u16>,
    surface_len: usize,
    rule_mask: u8,
    source: QuerySource,
    derivation: String,
    derivation_descriptions: Vec<(String, String)>,
    rule_derivations: Vec<(u8, String, Vec<(String, String)>)>,
    seq: usize,
}

impl LookupQuery {
    fn derivation_for_entry(&self, entry_rule_mask: u8) -> (String, Vec<(String, String)>) {
        if entry_rule_mask != 0 {
            for (mask, chain, descs) in &self.rule_derivations {
                if (*mask & entry_rule_mask) != 0 {
                    return (chain.clone(), descs.clone());
                }
            }
        }
        (self.derivation.clone(), self.derivation_descriptions.clone())
    }
}

struct DatabaseCandidate {
    hit: SearchHit,
    is_exact: bool,
    prefix_len: usize,
    dic_id: i64,
    source_row_id: Option<i64>,
}

const MAX_DECONJ_PREFIX_CHARS: usize = 36;
const MAX_MECAB_LOOKUP_CHARS: usize = 32;

fn is_ascii_regex_ws(ch: char) -> bool {
    matches!(ch, '\t'..='\r' | ' ')
}

fn slice_mecab_lookup_prefix(trimmed: &str) -> &str {
    let mut end_byte = trimmed.len();
    for (char_idx, (byte_idx, ch)) in trimmed.char_indices().enumerate() {
        if char_idx >= MAX_MECAB_LOOKUP_CHARS {
            end_byte = byte_idx;
            break;
        }
        if char_idx > 0
            && matches!(
                ch,
                '\r' | '\n' | '。' | '、' | '！' | '？' | '「' | '」' | '（' | '）'
            )
        {
            end_byte = byte_idx;
            break;
        }
    }
    &trimmed[..end_byte]
}

fn build_lookup_queries(
    trimmed: &str,
    mecab_enabled: bool,
    mecab_tokenizer: MecabTokenizerFn,
) -> Vec<LookupQuery> {
    let mut raw_lookups: Vec<LookupQuery> = Vec::new();
    let mut seq = 0usize;

    // Collect all non-empty prefix byte boundaries + UTF-16 lengths from longest to shortest.
    let mut prefixes: Vec<(usize, usize, usize)> = Vec::new();
    let mut char_count = 0usize;
    let mut u16_count = 0usize;
    for (byte_idx, ch) in trimmed.char_indices() {
        char_count += 1;
        u16_count += ch.len_utf16();
        prefixes.push((byte_idx + ch.len_utf8(), char_count, u16_count));
    }

    for &(byte_end, chars_len, orig_u16_len) in prefixes.iter().rev() {
        let prefix = &trimmed[..byte_end];
        if prefix.trim().is_empty() {
            continue;
        }

        // 1. Exact prefix variants (`rule_mask = 0`)
        for variant in normalize::expand_query_variants(prefix) {
            let deconj_u16 = if mecab_enabled {
                variant.encode_utf16().collect()
            } else {
                Vec::new()
            };
            raw_lookups.push(LookupQuery {
                key: variant,
                deconj_u16,
                surface_len: orig_u16_len,
                rule_mask: 0,
                source: QuerySource::Exact,
                derivation: String::new(),
                derivation_descriptions: Vec::new(),
                rule_derivations: Vec::new(),
                seq,
            });
            seq += 1;
        }

        if prefix.chars().any(is_ascii_regex_ws) {
            let no_ws: String = prefix.chars().filter(|&c| !is_ascii_regex_ws(c)).collect();
            for variant in normalize::expand_query_variants(&no_ws) {
                let deconj_u16 = if mecab_enabled {
                    variant.encode_utf16().collect()
                } else {
                    Vec::new()
                };
                raw_lookups.push(LookupQuery {
                    key: variant,
                    deconj_u16,
                    surface_len: orig_u16_len,
                    rule_mask: 0,
                    source: QuerySource::Exact,
                    derivation: String::new(),
                    derivation_descriptions: Vec::new(),
                    rule_derivations: Vec::new(),
                    seq,
                });
                seq += 1;
            }
        }

        // 2. Deconjugation candidates (`rule_mask != 0`) preserving `orig_u16_len`
        if chars_len <= MAX_DECONJ_PREFIX_CHARS || byte_end == trimmed.len() {
            let mut deconj_inputs: Vec<String> = Vec::new();
            let mut push_deconj_in = |s: String| {
                if !s.is_empty() && !deconj_inputs.contains(&s) {
                    deconj_inputs.push(s);
                }
            };
            let nfkc = normalize::normalize_nfkc(prefix);
            push_deconj_in(prefix.to_owned());
            push_deconj_in(nfkc.clone());
            push_deconj_in(normalize::kata_to_hira_str(&nfkc));
            for iter_var in normalize::expand_iteration_marks(&nfkc) {
                push_deconj_in(iter_var.clone());
                if iter_var.contains('ー') {
                    for pv in normalize::expand_prolonged_vowels(&iter_var) {
                        push_deconj_in(pv);
                    }
                }
            }

            for deconj_input in deconj_inputs {
                let input_u16: Vec<u16> = deconj_input.encode_utf16().collect();
                for dq in deconjugationquerygenerator::generate_queries_single(&input_u16) {
                    let base = String::from_utf16_lossy(&dq.deconj);
                    for base_variant in normalize::expand_query_variants(&base) {
                        let deconj_u16 = if mecab_enabled {
                            base_variant.encode_utf16().collect()
                        } else {
                            Vec::new()
                        };
                        raw_lookups.push(LookupQuery {
                            key: base_variant,
                            deconj_u16,
                            surface_len: orig_u16_len,
                            rule_mask: dq.rule_mask,
                            source: QuerySource::Deconj,
                            derivation: dq.derivation.clone(),
                            derivation_descriptions: dq.derivation_descriptions.clone(),
                            rule_derivations: dq.rule_derivations.clone(),
                            seq,
                        });
                        seq += 1;
                    }
                }
            }
        }
    }

    // 3. Optional MeCab morphological compound & lemma candidates (`QuerySource::Mecab`).
    // Gracefully falls back to `Exact` + `Deconj` if MeCab is unavailable or returns an error.
    if mecab_enabled {
        let mecab_prefix = slice_mecab_lookup_prefix(trimmed);
        if let Ok(tokens) = (mecab_tokenizer)(mecab_prefix) {
            if !tokens.is_empty() {
                let spans: Vec<mecabquerygenerator::TokenSpan<'_>> = tokens
                    .iter()
                    .map(|t| mecabquerygenerator::TokenSpan {
                        surface: t.surface.as_str(),
                        surface_clean: t.surface_clean.as_str(),
                        lemma: t.lemma.as_str(),
                        alt_lemma: t.alt_lemma.as_deref(),
                        pos: t.pos.as_str(),
                        conj_type: t.conj_type.as_str(),
                    })
                    .collect();

                let mut distinct_mecab_fallbacks: Vec<LookupQuery> = Vec::new();
                for cand in mecabquerygenerator::generate_candidates_from_tokens(&spans) {
                    for variant in normalize::expand_query_variants(&cand.deconj) {
                        let deconj_u16: Vec<u16> = variant.encode_utf16().collect();
                        let covered_by_deconj = raw_lookups.iter().any(|d| {
                            d.source == QuerySource::Deconj
                                && d.key == variant
                                && d.surface_len >= cand.surface_u16_len
                                && cand.rule_mask != 0
                                && (d.rule_mask & cand.rule_mask) == cand.rule_mask
                        });
                        let lq = LookupQuery {
                            key: variant.clone(),
                            deconj_u16: deconj_u16.clone(),
                            surface_len: cand.surface_u16_len,
                            rule_mask: cand.rule_mask,
                            source: QuerySource::Mecab,
                            derivation: String::new(),
                            derivation_descriptions: Vec::new(),
                            rule_derivations: Vec::new(),
                            seq,
                        };
                        if !covered_by_deconj
                            && ((cand.span_tokens >= 2 && cand.rule_mask == 0)
                                || cand.rule_mask != 0)
                        {
                            distinct_mecab_fallbacks.push(LookupQuery {
                                key: variant,
                                deconj_u16,
                                surface_len: cand.surface_u16_len,
                                rule_mask: cand.rule_mask,
                                source: QuerySource::Mecab,
                                derivation: String::new(),
                                derivation_descriptions: Vec::new(),
                                rule_derivations: Vec::new(),
                                seq,
                            });
                        }
                        raw_lookups.push(lq);
                        seq += 1;
                    }
                }

                let descriptors: Vec<CandidateSearchQuery<'_>> = raw_lookups
                    .iter()
                    .enumerate()
                    .map(|(idx, lq)| CandidateSearchQuery {
                        original_index: idx,
                        deconj: lq.deconj_u16.as_slice(),
                        surface_len: lq.surface_len as u64,
                        rule_mask: lq.rule_mask,
                        source: lq.source,
                    })
                    .collect();
                let surviving = searchcontroller::sort_and_filter_queries(descriptors, true);
                let mut filtered: Vec<LookupQuery> = Vec::with_capacity(
                    surviving.len() + distinct_mecab_fallbacks.len(),
                );
                let mut opts: Vec<Option<LookupQuery>> =
                    raw_lookups.into_iter().map(Some).collect();
                for idx in surviving {
                    if let Some(lq) = opts.get_mut(idx).and_then(Option::take) {
                        filtered.push(lq);
                    }
                }
                // Retain MeCab lookups whose `rule_mask` or `surface_len` was not covered by
                // `Deconj` (e.g. honorific `ください` -> `くださる` `v5` where prefix `くださ` only
                // emitted `v1`, and multi-morpheme `exp` compounds with `rule_mask = 0`).
                filtered.extend(distinct_mecab_fallbacks);
                raw_lookups = filtered;
            }
        }
    }

    // Longest surface first; at equal surface length, `Exact` precedes `Deconj` which precedes `Mecab`.
    raw_lookups.sort_by(|a, b| {
        b.surface_len
            .cmp(&a.surface_len)
            .then_with(|| a.source.to_u8().cmp(&b.source.to_u8()))
            .then_with(|| (a.rule_mask != 0).cmp(&(b.rule_mask != 0)))
            .then_with(|| a.seq.cmp(&b.seq))
    });

    // Deduplicate strictly by `(key, rule_mask)` so distinct `rule_mask`s are never collapsed!
    let mut seen_lookups: HashSet<(String, u8)> = HashSet::with_capacity(raw_lookups.len());
    raw_lookups.retain(|lookup| seen_lookups.insert((lookup.key.clone(), lookup.rule_mask)));
    raw_lookups
}

fn load_dictionary_names_and_disabled_conn(
    conn: &sqlite_db::rusqlite::Connection,
) -> Result<(HashMap<i64, String>, HashSet<i64>), sqlite_db::DictError> {
    let mut stmt = conn.prepare_cached(
        "SELECT dic_id, title, EXISTS (SELECT 1 FROM dict_disabled WHERE dict_disabled.dic_id = directory.dic_id) FROM directory;",
    )?;
    let mut names = HashMap::new();
    let mut disabled = HashSet::new();
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let id: i64 = row.get(0)?;
        let title: String = row.get(1)?;
        let is_disabled: i64 = row.get(2)?;
        names.insert(id, title);
        if is_disabled != 0 {
            disabled.insert(id);
        }
    }
    Ok((names, disabled))
}

fn load_dictionary_tags_conn(
    conn: &sqlite_db::rusqlite::Connection,
) -> Result<HashMap<(i64, String), DictTag>, sqlite_db::DictError> {
    let mut stmt = conn.prepare_cached(
        "SELECT dic_id, category, name, ord, notes, score FROM tag_bank \
         ORDER BY dic_id ASC, ord ASC, score DESC;",
    )?;
    let mut tags = HashMap::new();
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let dictionary_id: i64 = row.get(0)?;
        let category: String = row.get(1)?;
        let name: String = row.get(2)?;
        let order: i64 = row.get(3)?;
        let notes: String = row.get(4)?;
        let score: i64 = row.get(5)?;
        tags.insert(
            (dictionary_id, name.clone()),
            DictTag::with_details(name, category, notes, order, score),
        );
    }
    Ok(tags)
}

/// Strips legacy Yomichan nested-array deinflection entries (`[["base", ["rule"]]]`)
/// from a `term_bank.glossary` JSON payload. Returns `None` if no real glossary items remain.
fn clean_raw_glossary(raw_glossary: &str) -> Option<(String, Option<serde_json::Value>)> {
    match serde_json::from_str::<serde_json::Value>(raw_glossary) {
        Ok(serde_json::Value::Array(items)) => {
            let has_nested_array = items.iter().any(serde_json::Value::is_array);
            if !has_nested_array {
                if items.is_empty() {
                    return None;
                }
                return Some((raw_glossary.to_owned(), Some(serde_json::Value::Array(items))));
            }
            let filtered: Vec<serde_json::Value> = items
                .into_iter()
                .filter(|item| !item.is_array())
                .collect();
            if filtered.is_empty() {
                return None;
            }
            let cleaned_val = serde_json::Value::Array(filtered);
            let cleaned_json = serde_json::to_string(&cleaned_val).unwrap_or_default();
            Some((cleaned_json, Some(cleaned_val)))
        }
        Ok(other) => Some((raw_glossary.to_owned(), Some(other))),
        Err(_) => Some((raw_glossary.to_owned(), None)),
    }
}

fn database_search_candidates(
    database: &ReadOnlyDictionaryDb,
    query: &str,
    normalized_query: &str,
    full_u16_len: usize,
    lookups: &[LookupQuery],
) -> Result<Vec<DatabaseCandidate>, DictControllerError> {
    database
        .refresh_metadata_if_needed()
        .map_err(DictControllerError::Sqlite)?;

    // Collect distinct UTF-8 lookup keys in priority order for a single batched SQL query.
    let mut unique_keys: Vec<String> = Vec::with_capacity(lookups.len());
    let mut seen_keys: HashSet<&str> = HashSet::with_capacity(lookups.len());
    for lookup in lookups {
        if !lookup.key.is_empty() && seen_keys.insert(lookup.key.as_str()) {
            unique_keys.push(lookup.key.clone());
        }
    }
    if unique_keys.is_empty() {
        return Ok(Vec::new());
    }

    let batch_rows = {
        let conn = database
            .conn
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        sqlite_db::query_term_details_batch_conn(&conn, &unique_keys)
            .map_err(DictControllerError::Sqlite)?
    };
    if batch_rows.is_empty() {
        return Ok(Vec::new());
    }

    // Precompute `entry_rule_mask` and index rows by `expression` and `reading`.
    let row_masks: Vec<u8> = batch_rows
        .iter()
        .map(|row| deconjugationquerygenerator::parse_rule_mask(&row.rules))
        .collect();
    let mut by_key: HashMap<&str, Vec<usize>> = HashMap::with_capacity(batch_rows.len() * 2);
    for (idx, row) in batch_rows.iter().enumerate() {
        by_key.entry(row.expression.as_str()).or_default().push(idx);
        if !row.reading.is_empty() && row.reading != row.expression {
            by_key.entry(row.reading.as_str()).or_default().push(idx);
        }
    }

    let results = database.with_cache(|cache| {
        let mut results = Vec::new();
        let mut parsed_rows: HashMap<usize, Option<(String, String, String, String, Vec<GlossaryBlock>)>> =
            HashMap::new();

        for lookup in lookups {
            let Some(row_indices) = by_key.get(lookup.key.as_str()) else {
                continue;
            };
            for &row_idx in row_indices {
                let entry_mask = row_masks[row_idx];
                if !deconjugationquerygenerator::rule_matches_entry(lookup.rule_mask, entry_mask) {
                    continue;
                }
                let row = &batch_rows[row_idx];
                let Some(dictionary_name) = cache.names.get(&row.dic_id) else {
                    continue;
                };

                let parsed_entry = parsed_rows.entry(row_idx).or_insert_with(|| {
                    let (cleaned_raw, parsed_val) = clean_raw_glossary(&row.glossary)?;
                    let (glossary, example_ja, example_en, rich_glossary) = match parsed_val.as_ref() {
                        Some(value) => {
                            let (g, ja, en) = parse_glossary_entry_value(value);
                            (g, ja, en, parse_rich_glossary_value(value))
                        }
                        None => (
                            strip_glossary_markup(&cleaned_raw),
                            String::new(),
                            String::new(),
                            parse_rich_glossary(&cleaned_raw),
                        ),
                    };
                    Some((cleaned_raw, glossary, example_ja, example_en, rich_glossary))
                });
                let Some((cleaned_raw, glossary, example_ja, example_en, rich_glossary)) =
                    parsed_entry.as_ref()
                else {
                    continue;
                };

                let norm_expression = normalize_reading(&row.expression);
                let norm_reading = normalize_reading(&row.reading);
                let formatted_tags =
                    format_dictionary_tags(&cache.tags, row.dic_id, &row.def_tags, &row.term_tags);
                let is_exact = (lookup.source == QuerySource::Exact
                    && lookup.rule_mask == 0
                    && lookup.surface_len == full_u16_len)
                    || (lookup.rule_mask == 0
                        && (row.expression == query
                            || norm_expression == normalized_query
                            || (!norm_reading.is_empty() && norm_reading == normalized_query)));

                let (derivation, derivation_descriptions) = if lookup.rule_mask != 0 {
                    lookup.derivation_for_entry(entry_mask)
                } else {
                    (String::new(), Vec::new())
                };

                results.push(DatabaseCandidate {
                    hit: SearchHit {
                        expression: row.expression.clone(),
                        reading: row.reading.clone(),
                        glossary: glossary.clone(),
                        tags: formatted_tags,
                        score: row.score,
                        is_kanji: false,
                        notes: String::new(),
                        example_ja: example_ja.clone(),
                        example_en: example_en.clone(),
                        dict_name: dictionary_name.clone(),
                        rich_glossary: rich_glossary.clone(),
                        derivation,
                        derivation_descriptions,
                        dictionary_id: row.dic_id,
                        rules: row.rules.clone(),
                        raw_glossary: cleaned_raw.clone(),
                        term_tags: row.term_tags.clone(),
                        def_tags: row.def_tags.clone(),
                    },
                    is_exact,
                    prefix_len: lookup.surface_len,
                    dic_id: row.dic_id,
                    source_row_id: Some(row.rowid),
                });
            }
        }
        results
    });

    Ok(results)
}

fn database_kanji_candidates(
    database: &ReadOnlyDictionaryDb,
    query: &str,
    prefix_len: usize,
    dictionary_order: &HashMap<i64, i64>,
) -> Result<Vec<DatabaseCandidate>, DictControllerError> {
    database
        .refresh_metadata_if_needed()
        .map_err(DictControllerError::Sqlite)?;

    let (kanji_rows, kanji_meta_rows) = {
        let conn = database
            .conn
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let rows =
            sqlite_db::query_kanji_utf8_conn(&conn, query).map_err(DictControllerError::Sqlite)?;
        let meta = if rows.is_empty() {
            Vec::new()
        } else {
            sqlite_db::query_kanji_meta_freq_utf8_conn(&conn, query).unwrap_or_default()
        };
        (rows, meta)
    };
    if kanji_rows.is_empty() {
        return Ok(Vec::new());
    }

    let kanji_freqs =
        select_term_frequencies(database, dictionary_order, &kanji_meta_rows, query, "");
    let freq_notes: String = kanji_freqs
        .iter()
        .filter(|item| !item.parsed.display.is_empty())
        .map(|item| format!("{}: {}", item.dictionary, item.parsed.display))
        .collect::<Vec<_>>()
        .join("; ");

    let results = database.with_cache(|cache| {
        let mut results = Vec::with_capacity(kanji_rows.len());
        for row in kanji_rows {
            let Some(dictionary_name) = cache.names.get(&row.dic_id) else {
                continue;
            };
            let glossary = row.meanings.join("\n");
            let stats_notes = format_kanji_stats(&cache.tags, row.dic_id, &row.stats);
            let notes = match (stats_notes.is_empty(), freq_notes.is_empty()) {
                (false, false) => format!("{stats_notes}; {freq_notes}"),
                (false, true) => stats_notes,
                (true, false) => freq_notes.clone(),
                (true, true) => String::new(),
            };
            results.push(DatabaseCandidate {
                hit: SearchHit {
                    expression: query.to_owned(),
                    reading: format_kanji_reading(&row.onyomi, &row.kunyomi),
                    glossary,
                    tags: format_dictionary_tags(&cache.tags, row.dic_id, "", &row.tags),
                    score: 0,
                    is_kanji: true,
                    notes,
                    example_ja: String::new(),
                    example_en: String::new(),
                    dict_name: dictionary_name.clone(),
                    rich_glossary: Vec::new(),
                    derivation: String::new(),
                    derivation_descriptions: Vec::new(),
                    dictionary_id: row.dic_id,
                    rules: String::new(),
                    raw_glossary: row.raw_meanings,
                    term_tags: row.tags,
                    def_tags: String::new(),
                },
                is_exact: true,
                prefix_len,
                dic_id: row.dic_id,
                source_row_id: None,
            });
        }
        results
    });
    Ok(results)
}

fn format_kanji_reading(onyomi: &str, kunyomi: &str) -> String {
    let mut readings = Vec::with_capacity(2);
    if !onyomi.is_empty() {
        readings.push(format!("On: {onyomi}"));
    }
    if !kunyomi.is_empty() {
        readings.push(format!("Kun: {kunyomi}"));
    }
    readings.join("; ")
}

/// Formats `kanji_bank.stats` using `tag.notes` (falling back to `tag.name` when `notes` is empty),
/// sorted deterministically by `(tag.order ASC, tag.score DESC, tag.name ASC)`.
fn format_kanji_stats(
    tags: &HashMap<(i64, String), DictTag>,
    dictionary_id: i64,
    raw_stats: &str,
) -> String {
    let Ok(serde_json::Value::Object(stats)) =
        serde_json::from_str::<serde_json::Value>(raw_stats)
    else {
        return String::new();
    };
    let mut entries: Vec<(&DictTag, String)> = stats
        .iter()
        .filter_map(|(name, value)| {
            let tag = tags.get(&(dictionary_id, name.clone()))?;
            if !matches!(tag.category.as_str(), "index" | "misc" | "class" | "code") {
                return None;
            }
            let value = match value {
                serde_json::Value::String(value) => value.clone(),
                serde_json::Value::Number(value) => value.to_string(),
                serde_json::Value::Bool(value) => value.to_string(),
                serde_json::Value::Null
                | serde_json::Value::Array(_)
                | serde_json::Value::Object(_) => return None,
            };
            (!value.is_empty()).then_some((tag, value))
        })
        .collect();

    entries.sort_by(|(a, _), (b, _)| {
        a.order
            .cmp(&b.order)
            .then_with(|| b.score.cmp(&a.score))
            .then_with(|| a.name.cmp(&b.name))
    });

    entries
        .into_iter()
        .map(|(tag, value)| {
            let label = if tag.notes.is_empty() {
                tag.name.as_str()
            } else {
                tag.notes.as_str()
            };
            format!("{label}: {value}")
        })
        .collect::<Vec<_>>()
        .join("; ")
}

fn format_dictionary_tags(
    tags: &HashMap<(i64, String), DictTag>,
    dictionary_id: i64,
    definition_tags: &str,
    term_tags: &str,
) -> String {
    let mut seen = HashSet::new();
    let mut selected = Vec::new();
    for name in definition_tags
        .split_whitespace()
        .chain(term_tags.split_whitespace())
    {
        if name.is_empty() || !seen.insert(name) {
            continue;
        }
        if let Some(tag) = tags.get(&(dictionary_id, name.to_owned())) {
            selected.push(tag.clone());
        }
    }
    selected.sort_by(|a, b| {
        a.order
            .cmp(&b.order)
            .then_with(|| b.score.cmp(&a.score))
            .then_with(|| a.name.cmp(&b.name))
    });
    selected
        .into_iter()
        .map(|t| t.name)
        .collect::<Vec<_>>()
        .join(", ")
}

fn rich_node_text(node: &serde_json::Value) -> String {
    match node {
        serde_json::Value::String(value) => strip_glossary_markup(value),
        serde_json::Value::Array(items) => items.iter().map(rich_node_text).collect(),
        serde_json::Value::Object(map) => {
            if matches!(
                map.get("tag").and_then(serde_json::Value::as_str),
                Some("rt" | "rp" | "img")
            ) {
                return String::new();
            }
            map.get("content")
                .or_else(|| map.get("text"))
                .map(rich_node_text)
                .unwrap_or_default()
        }
        _ => String::new(),
    }
}

/// Keep headings, list items, and links separate for the popup while the plain
/// glossary remains available for search and Anki fields.
fn parse_rich_glossary_value(value: &serde_json::Value) -> Vec<GlossaryBlock> {
    fn push(blocks: &mut Vec<GlossaryBlock>, text: String, kind: i32, href: &str) {
        let text = text.trim().trim_start_matches(['◦', '•']).trim().to_owned();
        if !text.is_empty() {
            blocks.push(GlossaryBlock {
                text,
                kind,
                href: href.to_owned(),
            });
        }
    }
    fn walk(node: &serde_json::Value, blocks: &mut Vec<GlossaryBlock>) {
        match node {
            serde_json::Value::String(value) => {
                for line in strip_glossary_markup(value).lines() {
                    push(blocks, line.to_owned(), 2, "");
                }
            }
            serde_json::Value::Array(items) => {
                for item in items {
                    walk(item, blocks);
                }
            }
            serde_json::Value::Object(map) => {
                let tag = map.get("tag").and_then(serde_json::Value::as_str).unwrap_or("");
                let data = map.get("data").and_then(serde_json::Value::as_object);
                if data
                    .and_then(|data| data.get("content"))
                    .and_then(serde_json::Value::as_str)
                    == Some("examples")
                {
                    return;
                }
                let Some(content) = map.get("content") else {
                    return;
                };
                match tag {
                    "rt" | "rp" | "img" => {}
                    "li" => push(blocks, rich_node_text(content), 2, ""),
                    "a" => push(
                        blocks,
                        rich_node_text(content),
                        3,
                        map.get("href").and_then(serde_json::Value::as_str).unwrap_or(""),
                    ),
                    "span" => {
                        let has_link = content.as_array().is_some_and(|items| {
                            items.iter().any(|item| {
                                item.get("tag").and_then(serde_json::Value::as_str) == Some("a")
                            })
                        });
                        if has_link {
                            walk(content, blocks);
                        } else {
                            let bold = map
                                .get("style")
                                .and_then(|style| style.get("fontWeight"))
                                .and_then(serde_json::Value::as_str)
                                == Some("bold")
                                || content.as_array().is_some_and(|items| {
                                    items.iter().any(|item| {
                                        item.get("style")
                                            .and_then(|style| style.get("fontWeight"))
                                            .and_then(serde_json::Value::as_str)
                                            == Some("bold")
                                    })
                                });
                            push(blocks, rich_node_text(content), if bold { 1 } else { 0 }, "");
                        }
                    }
                    "div"
                        if data
                            .and_then(|data| data.get("name"))
                            .and_then(serde_json::Value::as_str)
                            == Some("definition") =>
                    {
                        push(blocks, rich_node_text(content), 0, "");
                    }
                    "div" | "ul" | "ol" => walk(content, blocks),
                    _ => {
                        if content.is_string() {
                            push(blocks, rich_node_text(content), 0, "");
                        } else {
                            walk(content, blocks);
                        }
                    }
                }
            }
            _ => {}
        }
    }
    let mut blocks = Vec::new();
    walk(value, &mut blocks);
    blocks
}

pub fn parse_rich_glossary(raw: &str) -> Vec<GlossaryBlock> {
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(raw) {
        parse_rich_glossary_value(&value)
    } else {
        strip_glossary_markup(raw)
            .lines()
            .filter_map(|line| {
                let text = line.trim();
                (!text.is_empty()).then(|| GlossaryBlock {
                    text: text.to_owned(),
                    kind: 0,
                    href: String::new(),
                })
            })
            .collect()
    }
}

pub fn parse_glossary_entry(raw: &str) -> (String, String, String) {
    match serde_json::from_str::<serde_json::Value>(raw) {
        Ok(value) => parse_glossary_entry_value(&value),
        Err(_) => (strip_glossary_markup(raw), String::new(), String::new()),
    }
}

fn parse_glossary_entry_value(value: &serde_json::Value) -> (String, String, String) {
    let items = value
        .as_array()
        .map_or_else(|| vec![value], |items| items.iter().collect());
    let mut senses = Vec::new();
    let mut ex_ja = Vec::new();
    let mut ex_en = Vec::new();

    fn extract_plain_text(node: &serde_json::Value) -> String {
        match node {
            serde_json::Value::String(text) => strip_glossary_markup(text),
            serde_json::Value::Array(items) => items
                .iter()
                .map(extract_plain_text)
                .collect::<Vec<_>>()
                .join(""),
            serde_json::Value::Object(map) => {
                if let Some(tag) = map.get("tag").and_then(serde_json::Value::as_str) {
                    if tag == "rt" || tag == "rp" {
                        return String::new();
                    }
                }
                if let Some(content) = map.get("content") {
                    extract_plain_text(content)
                } else if let Some(text) = map.get("text").and_then(serde_json::Value::as_str) {
                    strip_glossary_markup(text)
                } else {
                    String::new()
                }
            }
            _ => String::new(),
        }
    }

    for item in items {
        if item.is_array() {
            continue;
        }
        match item {
            serde_json::Value::String(text) => {
                for line in text.split('\n') {
                    let trimmed = strip_glossary_markup(line.trim());
                    if !trimmed.is_empty() {
                        if trimmed.starts_with('◦') || trimmed.starts_with('•') {
                            senses.push(trimmed);
                        } else {
                            senses.push(format!("◦ {trimmed}"));
                        }
                    }
                }
            }
            serde_json::Value::Object(map)
                if map.get("type").and_then(serde_json::Value::as_str)
                    == Some("structured-content") =>
            {
                let content_nodes = match map.get("content") {
                    Some(serde_json::Value::Array(arr)) => arr.iter().collect::<Vec<_>>(),
                    Some(single) => vec![single],
                    None => Vec::new(),
                };
                for node in content_nodes {
                    if let serde_json::Value::Object(block) = node {
                        let data_content = block
                            .get("data")
                            .and_then(serde_json::Value::as_object)
                            .and_then(|d| d.get("content"))
                            .and_then(serde_json::Value::as_str);

                        if data_content == Some("examples") {
                            let ex_nodes = match block.get("content") {
                                Some(serde_json::Value::Array(arr)) => arr.iter().collect::<Vec<_>>(),
                                Some(single) => vec![single],
                                None => Vec::new(),
                            };
                            for ex_item in ex_nodes {
                                if let serde_json::Value::Object(ex_obj) = ex_item {
                                    let style_str = ex_obj
                                        .get("style")
                                        .and_then(|s| s.get("listStyleType"))
                                        .and_then(serde_json::Value::as_str)
                                        .unwrap_or("");
                                    let lang = ex_obj
                                        .get("lang")
                                        .and_then(serde_json::Value::as_str)
                                        .unwrap_or("");
                                    let txt = extract_plain_text(ex_item).trim().to_string();
                                    if !txt.is_empty() {
                                        if style_str.contains("🇯🇵") || lang == "ja" {
                                            ex_ja.push(txt);
                                        } else if style_str.contains("🇬🇧") || lang == "en" {
                                            ex_en.push(txt);
                                        } else if txt.chars().any(|c| (c as u32) >= 0x3000) {
                                            ex_ja.push(txt);
                                        } else {
                                            ex_en.push(txt);
                                        }
                                    }
                                }
                            }
                        } else if data_content == Some("glossary")
                            || matches!(
                                block.get("tag").and_then(serde_json::Value::as_str),
                                Some("ul" | "ol")
                            )
                        {
                            let li_nodes = match block.get("content") {
                                Some(serde_json::Value::Array(arr)) => arr.iter().collect::<Vec<_>>(),
                                Some(single) => vec![single],
                                None => Vec::new(),
                            };
                            for li in li_nodes {
                                let txt = extract_plain_text(li).trim().to_string();
                                if !txt.is_empty() {
                                    senses.push(format!("◦ {txt}"));
                                }
                            }
                        } else {
                            let txt = extract_plain_text(node).trim().to_string();
                            if !txt.is_empty() {
                                for line in txt.split('\n') {
                                    let l = line.trim();
                                    if !l.is_empty() {
                                        senses.push(format!("◦ {l}"));
                                    }
                                }
                            }
                        }
                    } else {
                        let txt = extract_plain_text(node).trim().to_string();
                        if !txt.is_empty() {
                            senses.push(format!("◦ {txt}"));
                        }
                    }
                }
            }
            _ => {
                let txt = extract_plain_text(item).trim().to_string();
                if !txt.is_empty() {
                    senses.push(format!("◦ {txt}"));
                }
            }
        }
    }

    (senses.join("\n"), ex_ja.join("\n"), ex_en.join("\n"))
}

pub fn format_glossary(raw: &str) -> String {
    parse_glossary_entry(raw).0
}

fn strip_glossary_markup(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(character) = chars.next() {
        if character != '<' {
            output.push(character);
            continue;
        }

        let mut tag = String::new();
        for character in chars.by_ref() {
            if character == '>' {
                break;
            }
            tag.push(character);
        }
        let tag = tag.trim().to_ascii_lowercase();
        if tag.starts_with("li") {
            if !output.ends_with('\n') {
                output.push('\n');
            }
            output.push_str("◦ ");
        } else if tag.starts_with("br")
            || tag.starts_with("/p")
            || tag.starts_with("/div")
            || tag.starts_with("/li")
        {
            output.push('\n');
        }
    }

    output
        .replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&")
}

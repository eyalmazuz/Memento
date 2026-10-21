//! Dictionary controller connecting memento-dict and memento-util.

use std::collections::{HashMap, HashSet};
use std::ffi::CString;
use std::fmt;
use std::path::{Path, PathBuf};
use std::ptr;

use memento_dict::exactquerygenerator;
use memento_dict::deconjugationquerygenerator;
use memento_dict::searchcontroller::{self, TagSortItem};
use memento_dict::sqlite_db;
use memento_util::furiganaparser;

pub use memento_dict::searchcontroller::ClozeSpans;

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
}

/// A display block retained from Yomitan's structured glossary data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GlossaryBlock {
    pub text: String,
    /// 0: paragraph, 1: heading, 2: list item, 3: link.
    pub kind: i32,
    pub href: String,
}

/// Values used by Anki note field markers for a selected term.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AnkiTermMetadata {
    pub pitch: String,
    pub pitch_positions: String,
    pub pitch_categories: String,
    pub frequencies: String,
    pub frequency_harmonic_rank: String,
    pub frequency_average_rank: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TermPitchGraph {
    pub dictionary: String,
    pub mora: Vec<String>,
    pub position: i32,
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
        }
    }

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

/// Reading normalization using `memento_util::furiganaparser::katakana_to_hiragana`.
pub fn normalize_reading(text: &str) -> String {
    let u16_text: Vec<u16> = text.encode_utf16().collect();
    let normalized = furiganaparser::katakana_to_hiragana(&u16_text);
    String::from_utf16_lossy(&normalized)
}

/// Produces the NFKC form used by `DatabaseManager::queryTerms`.
///
/// The shared furigana normalizer also shifts katakana to hiragana. Restore
/// katakana only for katakana runs so mixed-script queries retain their form.
fn normalize_nfkc_query(text: &str) -> String {
    fn is_combining_mark(character: char) -> bool {
        matches!(
            character,
            '\u{0300}'..='\u{036f}' | '\u{3099}'..='\u{309c}' | '\u{ff9e}'..='\u{ff9f}'
        )
    }

    fn kana_script(character: char) -> Option<bool> {
        match character {
            '\u{3040}'..='\u{309f}' => Some(false),
            '\u{30a0}'..='\u{30ff}' | '\u{ff61}'..='\u{ff9f}' => Some(true),
            _ => None,
        }
    }

    let mut characters = text.chars().peekable();
    let mut chunk = Vec::with_capacity(text.len());
    let mut normalized = Vec::with_capacity(text.len());
    while let Some(first) = characters.next() {
        let script = kana_script(first);
        chunk.clear();
        let mut units = [0; 2];
        chunk.extend_from_slice(first.encode_utf16(&mut units));
        while let Some(&next) = characters.peek() {
            if is_combining_mark(next) || kana_script(next) == script {
                let mut units = [0; 2];
                chunk.extend_from_slice(next.encode_utf16(&mut units));
                characters.next();
            } else {
                break;
            }
        }

        let katakana_run = script != Some(false);
        let nfkc_chunk = furiganaparser::katakana_to_hiragana(&chunk);
        normalized.extend(nfkc_chunk.into_iter().map(|unit| {
            if katakana_run && (0x3041..=0x3096).contains(&unit) {
                unit + 0x60
            } else {
                unit
            }
        }));
    }
    String::from_utf16_lossy(&normalized)
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

struct ReadOnlyDictionaryDb {
    handle: *mut sqlite_db::sqlite3,
    tags: HashMap<(i64, String), DictTag>,
    names: HashMap<i64, String>,
}

impl Drop for ReadOnlyDictionaryDb {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            unsafe { sqlite_db::sqlite3_close_v2(self.handle) };
        }
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
        }
    }

    /// Open Memento's persisted Yomitan database read-only. A missing database
    /// is reported to the caller; this function never creates an empty file.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, DictControllerError> {
        let path = path.as_ref();
        if !path.is_file() {
            return Err(DictControllerError::MissingDatabase(path.to_path_buf()));
        }
        let path_text = path.to_string_lossy();
        let c_path = CString::new(path_text.as_bytes())
            .map_err(|error| DictControllerError::InvalidPath(error.to_string()))?;
        let mut handle = ptr::null_mut();
        const SQLITE_OPEN_READONLY: i32 = 0x0000_0001;
        const SQLITE_OPEN_FULLMUTEX: i32 = 0x0001_0000;
        let status = unsafe {
            sqlite_db::sqlite3_open_v2(
                c_path.as_ptr(),
                &mut handle,
                SQLITE_OPEN_READONLY | SQLITE_OPEN_FULLMUTEX,
                ptr::null(),
            )
        };
        if status != sqlite_db::SQLITE_OK {
            let message = if handle.is_null() {
                "could not open database".to_string()
            } else {
                unsafe { std::ffi::CStr::from_ptr(sqlite_db::sqlite3_errmsg(handle)) }
                    .to_string_lossy()
                    .into_owned()
            };
            if !handle.is_null() {
                unsafe { sqlite_db::sqlite3_close_v2(handle) };
            }
            return Err(DictControllerError::Sqlite(sqlite_db::DictError::Sqlite {
                code: status,
                message,
            }));
        }

        let mut database = ReadOnlyDictionaryDb {
            handle,
            tags: HashMap::new(),
            names: HashMap::new(),
        };
        validate_dictionary_schema(database.handle).map_err(DictControllerError::Sqlite)?;
        database.tags =
            load_dictionary_tags(database.handle).map_err(DictControllerError::Sqlite)?;
        database.names =
            load_dictionary_names(database.handle).map_err(DictControllerError::Sqlite)?;
        Ok(Self {
            dictionaries: Vec::new(),
            database: Some(database),
            recent_hits: Vec::new(),
            last_search_error: None,
            last_match_units: 0,
            dictionary_order: HashMap::new(),
        })
    }

    pub fn take_search_error(&mut self) -> Option<String> {
        self.last_search_error.take()
    }

    pub fn database_path_is_open(&self) -> bool {
        self.database.is_some()
    }

    /// Apply the configured dictionary IDs in search-priority order.
    /// Unlisted IDs use priority zero, matching the C++ dictionary-order map.
    pub fn set_order(&mut self, order: &[i64]) {
        self.dictionary_order.clear();
        self.dictionary_order.reserve(order.len());
        for (priority, &dictionary_id) in order.iter().enumerate() {
            self.dictionary_order
                .entry(dictionary_id)
                .or_insert(priority as i64);
        }
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
        let trimmed = query.trim();
        if trimmed.is_empty() {
            return &self.recent_hits;
        }

        let norm_query = normalize_reading(trimmed);
        let u16_query: Vec<u16> = trimmed.encode_utf16().collect();
        let exact_queries = exactquerygenerator::generate_queries(&u16_query);
        // Prefix queries plus deconjugated base forms (e.g. 残されていた -> 残す).
        // Longest matched surface first so the most specific entries win ties.
        let mut lookups: Vec<LookupQuery> = exact_queries
            .iter()
            .map(|query| LookupQuery { key: query.deconj.clone(), surface_len: query.deconj.len() })
            .collect();
        for query in deconjugationquerygenerator::generate_queries(&u16_query) {
            lookups.push(LookupQuery { key: query.deconj, surface_len: query.surface.len() });
        }
        lookups.sort_by(|a, b| b.surface_len.cmp(&a.surface_len));
        // Exact and deconjugation generators can produce the same lookup key
        // many times. The SQL result is identical; keep the longest surface
        // so ranking and subtitle selection remain stable.
        let mut seen_lookups = HashSet::new();
        lookups.retain(|lookup| seen_lookups.insert(lookup.key.clone()));

        struct Candidate {
            hit: SearchHit,
            is_exact: bool,
            prefix_len: usize,
            dict_priority: i64,
            original_index: usize,
        }

        let mut candidates: Vec<Candidate> = Vec::new();
        let mut seen_database_rows = HashSet::new();

        let database_candidates = self.database.as_ref().map(|database| {
            database_search_candidates(database, trimmed, &norm_query, &lookups)
        });
        if let Some(Err(error)) = database_candidates.as_ref() {
            self.last_search_error = Some(error.to_string());
        }
        if let Some(Ok(database_candidates)) = database_candidates {
            for candidate in database_candidates {
                if let Some(source_row_id) = candidate.source_row_id {
                    if !seen_database_rows.insert((candidate.dict_priority, source_row_id)) {
                        continue;
                    }
                }
                let dict_priority = self
                    .dictionary_order
                    .get(&candidate.dict_priority)
                    .copied()
                    .unwrap_or(0);
                candidates.push(Candidate {
                    hit: candidate.hit,
                    is_exact: candidate.is_exact,
                    prefix_len: candidate.prefix_len,
                    dict_priority,
                    original_index: candidates.len(),
                });
            }
        }

        let database_kanji_candidates = if trimmed.chars().count() == 1 {
            self.database.as_ref().map(|database| {
                database_kanji_candidates(database, trimmed, u16_query.len())
            })
        } else {
            None
        };
        if let Some(Err(error)) = database_kanji_candidates.as_ref() {
            self.last_search_error = Some(error.to_string());
        }
        if let Some(Ok(database_kanji_candidates)) = database_kanji_candidates {
            // This lookup runs once per character; retain repeated rows from a dictionary.
            for candidate in database_kanji_candidates {
                let dict_priority = self
                    .dictionary_order
                    .get(&candidate.dict_priority)
                    .copied()
                    .unwrap_or(0);
                candidates.push(Candidate {
                    hit: candidate.hit,
                    is_exact: candidate.is_exact,
                    prefix_len: candidate.prefix_len,
                    dict_priority,
                    original_index: candidates.len(),
                });
            }
        }

        let mut seen_memory_entries = HashSet::new();
        for eq in &lookups {
            if eq.key.is_empty() {
                continue;
            }
            let prefix_str = String::from_utf16_lossy(&eq.key);
            let prefix_trimmed = prefix_str.trim();
            if prefix_trimmed.is_empty() {
                continue;
            }
            let norm_prefix = normalize_reading(prefix_trimmed);
            let prefix_len = eq.surface_len;

            for (dictionary_index, dict) in self.dictionaries.iter().enumerate() {
                for (entry_index, entry) in dict.entries.iter().enumerate() {
                    let entry_key = (dictionary_index, entry_index);
                    if seen_memory_entries.contains(&entry_key) {
                        continue;
                    }

                    let norm_entry_reading = normalize_reading(&entry.reading);
                    let norm_entry_expr = normalize_reading(&entry.expression);

                    let matches = entry.expression == prefix_trimmed
                        || norm_entry_expr == norm_prefix
                        || (!norm_entry_reading.is_empty() && norm_entry_reading == norm_prefix);

                    if matches {
                        seen_memory_entries.insert(entry_key);
                        let is_exact = entry.expression == trimmed
                            || norm_entry_expr == norm_query
                            || (!norm_entry_reading.is_empty() && norm_entry_reading == norm_query);
                        let mut hit = entry.clone();
                        if hit.dict_name.is_empty() {
                            hit.dict_name = dict.name.clone();
                        }
                        candidates.push(Candidate {
                            hit,
                            is_exact,
                            prefix_len,
                            dict_priority: dict.priority,
                            original_index: candidates.len(),
                        });
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
                .then_with(|| a.original_index.cmp(&b.original_index))
        });

        self.last_match_units = candidates.first().map_or(0, |candidate| candidate.prefix_len);
        self.recent_hits = candidates.into_iter().map(|candidate| candidate.hit).collect();
        &self.recent_hits
    }

    /// UTF-16 length of the query text matched by the first hit of the last search.
    pub fn last_match_units(&self) -> usize {
        self.last_match_units
    }

    /// Search exact kanji definitions from the persisted dictionaries.
    /// Kanji rows have no score column, so results use configured dictionary priority.
    pub fn search_kanji(&mut self, query: &str) -> &[SearchHit] {
        self.recent_hits.clear();
        self.last_search_error = None;
        if query.is_empty() {
            return &self.recent_hits;
        }
        let Some(database) = self.database.as_ref() else {
            return &self.recent_hits;
        };

        match database_kanji_candidates(database, query, query.encode_utf16().count()) {
            Ok(mut candidates) => {
                candidates.sort_by_key(|candidate| {
                    self.dictionary_order
                        .get(&candidate.dict_priority)
                        .copied()
                        .unwrap_or(0)
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
        self.database.as_ref()?.names.iter()
            .find_map(|(id, title)| (title == name).then_some(*id))
    }

    /// Fetches pitch accent, general frequency rank, and JPDB frequency rank for the term.
    pub fn get_term_meta(&self, expression: &str, reading: &str) -> (String, String, String) {
        let Some(database) = self.database.as_ref() else {
            return (String::new(), String::new(), String::new());
        };
        const QUERY: &str =
            "SELECT directory.title, term_meta_bank.mode, term_meta_bank.data \
             FROM term_meta_bank \
             JOIN directory ON directory.dic_id = term_meta_bank.dic_id \
             WHERE term_meta_bank.expression = ?1 \
               AND NOT EXISTS (SELECT 1 FROM dict_disabled WHERE dict_disabled.dic_id = term_meta_bank.dic_id);";

        let Ok(mut statement) = sqlite_db::Statement::prepare(database.handle, QUERY) else {
            return (String::new(), String::new(), String::new());
        };
        let u16_expr: Vec<u16> = expression.encode_utf16().collect();
        if statement.bind_u16_as_utf8(1, &u16_expr).is_err() {
            return (String::new(), String::new(), String::new());
        }

        let mut pitch_accent = String::new();
        let mut freq_rank = String::new();
        let mut jpdb_rank = String::new();

        while let Ok(true) = statement.step() {
            let title = statement.column_text_string(0);
            let mode = statement.column_text_string(1);
            let blob = statement.column_blob_bytes(2);
            let null_pos = blob.iter().position(|&b| b == 0).unwrap_or(blob.len());
            let blob_slice = &blob[..null_pos];
            let Ok(text) = std::str::from_utf8(blob_slice) else { continue };
            let Ok(json) = serde_json::from_str::<serde_json::Value>(text) else { continue };

            if mode == "freq" {
                let reading_match = json.get("reading").and_then(serde_json::Value::as_str)
                    .map_or(true, |r| r == reading || reading.is_empty());
                if !reading_match { continue; }

                let rank_val = if let Some(obj) = json.get("frequency").and_then(serde_json::Value::as_object) {
                    obj.get("displayValue").and_then(serde_json::Value::as_str).map(str::to_owned)
                        .or_else(|| obj.get("value").map(|v| v.to_string()))
                } else if let Some(num) = json.get("frequency") {
                    Some(num.to_string())
                } else {
                    None
                };

                if let Some(val) = rank_val {
                    if title.to_uppercase().contains("JPDB") && jpdb_rank.is_empty() {
                        jpdb_rank = val;
                    } else if freq_rank.is_empty() {
                        freq_rank = format!("{title} {val}");
                    }
                }
            } else if mode == "pitch" && pitch_accent.is_empty() {
                let reading_match = json.get("reading").and_then(serde_json::Value::as_str)
                    .map_or(true, |r| r == reading || reading.is_empty());
                if !reading_match { continue; }

                if let Some(pitches) = json.get("pitches").and_then(serde_json::Value::as_array) {
                    if let Some(first) = pitches.first() {
                        if let Some(pos) = first.get("position").and_then(serde_json::Value::as_i64) {
                            pitch_accent = if reading.is_empty() {
                                format!("[{pos}]")
                            } else {
                                format!("{reading} [{pos}]")
                            };
                        }
                    }
                }
            }
        }

        (pitch_accent, freq_rank, jpdb_rank)
    }

    /// Return every pitch accent supplied by enabled dictionaries for the popup.
    pub fn get_pitch_graphs(&self, expression: &str, reading: &str) -> Vec<TermPitchGraph> {
        let Some(database) = self.database.as_ref() else { return Vec::new() };
        let mut rows = Vec::new();
        if sqlite_db::add_pitches(
            database.handle,
            &expression.encode_utf16().collect::<Vec<_>>(),
            &reading.encode_utf16().collect::<Vec<_>>(),
            &mut rows,
        ).is_err() { return Vec::new() }
        rows.sort_by_key(|row| row.dic_id);
        let mut result = Vec::new();
        for row in rows {
            let Some(dictionary) = database.names.get(&row.dic_id) else { continue };
            let mora: Vec<String> = row.mora.iter().map(|unit| String::from_utf16_lossy(unit)).collect();
            for position in row.positions {
                if position < 0 || position as usize > mora.len() { continue }
                result.push(TermPitchGraph { dictionary: dictionary.clone(), mora: mora.clone(), position });
            }
        }
        result
    }

    pub fn get_frequencies(&self, expression: &str, reading: &str) -> Vec<TermFrequency> {
        let Some(database) = self.database.as_ref() else { return Vec::new() };
        let mut rows = Vec::new();
        if sqlite_db::get_frequencies(
            database.handle,
            &expression.encode_utf16().collect::<Vec<_>>(),
            &reading.encode_utf16().collect::<Vec<_>>(),
            &mut rows,
        ).is_err() { return Vec::new() }
        rows.sort_by_key(|row| row.dic_id);
        rows.into_iter().filter_map(|row| {
            database.names.get(&row.dic_id).map(|dictionary| TermFrequency {
                dictionary: dictionary.clone(),
                value: String::from_utf16_lossy(&row.frequency),
            })
        }).collect()
    }

    /// Gather all enabled frequency and pitch entries for Anki, using the
    /// shared note formatter so field values match Memento's card format.
    pub fn get_anki_term_metadata(&self, expression: &str, reading: &str, tags: &[String]) -> AnkiTermMetadata {
        let Some(database) = self.database.as_ref() else { return AnkiTermMetadata::default() };
        let expression_units: Vec<u16> = expression.encode_utf16().collect();
        let reading_units: Vec<u16> = reading.encode_utf16().collect();
        let mut frequency_rows = Vec::new();
        let mut pitch_rows = Vec::new();
        if sqlite_db::get_frequencies(database.handle, &expression_units, &reading_units, &mut frequency_rows).is_err()
            || sqlite_db::add_pitches(database.handle, &expression_units, &reading_units, &mut pitch_rows).is_err() {
            return AnkiTermMetadata::default();
        }

        let mut titles = HashMap::new();
        if let Ok(mut statement) = sqlite_db::Statement::prepare(database.handle, "SELECT dic_id, title FROM directory") {
            while let Ok(true) = statement.step() {
                titles.insert(statement.column_int64(0), statement.column_text_string(1).encode_utf16().collect::<Vec<_>>());
            }
        }

        frequency_rows.sort_by_key(|row| row.dic_id);
        let frequencies_owned: Vec<_> = frequency_rows.iter().map(|row| {
            (titles.get(&row.dic_id).cloned().unwrap_or_default(), row.frequency.clone())
        }).collect();
        let frequencies: Vec<_> = frequencies_owned.iter().map(|(name, value)| memento_anki::note::FrequencyEntry {
            dictionary: name,
            freq: value,
        }).collect();
        let (_, harmonic, average) = memento_anki::note::frequency_stats(&frequencies);

        let pitches_owned: Vec<_> = pitch_rows.iter().filter_map(|row| {
            let positions: Vec<u8> = row.positions.iter().filter_map(|position| u8::try_from(*position).ok()).collect();
            (!positions.is_empty() && !row.mora.is_empty()).then(|| {
                (titles.get(&row.dic_id).cloned().unwrap_or_default(), positions, row.mora.clone())
            })
        }).collect();
        let mora_refs: Vec<Vec<&[u16]>> = pitches_owned.iter().map(|(_, _, mora)| mora.iter().map(Vec::as_slice).collect()).collect();
        let pitches: Vec<_> = pitches_owned.iter().enumerate().map(|(index, (name, positions, _))| memento_anki::note::PitchEntry {
            dictionary_name: name,
            positions,
            mora: &mora_refs[index],
        }).collect();
        let categories: Vec<_> = pitches_owned.iter().map(|(_, positions, mora)| memento_anki::note::PitchCategoryInput {
            mora_len: mora.len(),
            positions,
        }).collect();
        let rule_units: Vec<Vec<u16>> = tags.iter().map(|tag| tag.encode_utf16().collect()).collect();
        let rules: Vec<&[u16]> = rule_units.iter().map(Vec::as_slice).collect();
        let can_be_kifuku = memento_anki::note::is_kifuku_applicable(&rules);

        AnkiTermMetadata {
            pitch: String::from_utf16_lossy(&memento_anki::note::format_pitch(&pitches)),
            pitch_positions: String::from_utf16_lossy(&memento_anki::note::format_pitch_positions(&pitches)),
            pitch_categories: String::from_utf16_lossy(&memento_anki::note::format_pitch_categories(&categories, can_be_kifuku)),
            frequencies: String::from_utf16_lossy(&memento_anki::note::build_frequencies_html(&frequencies, false, false)),
            frequency_harmonic_rank: harmonic.map(|value| value.to_string()).unwrap_or_default(),
            frequency_average_rank: average.map(|value| value.to_string()).unwrap_or_default(),
        }
    }
}


/// One dictionary lookup key together with the number of UTF-16 units of the
/// original query text it accounts for (the matched surface).
struct LookupQuery {
    key: Vec<u16>,
    surface_len: usize,
}

struct DatabaseCandidate {
    hit: SearchHit,
    is_exact: bool,
    prefix_len: usize,
    dict_priority: i64,
    source_row_id: Option<i64>,
}

fn validate_dictionary_schema(
    database: *mut sqlite_db::sqlite3,
) -> Result<(), sqlite_db::DictError> {
    const REQUIRED_TABLES: [&str; 7] = [
        "directory",
        "dict_disabled",
        "tag_bank",
        "term_bank",
        "term_meta_bank",
        "kanji_bank",
        "kanji_meta_bank",
    ];
    let mut statement =
        sqlite_db::Statement::prepare(database, "SELECT name FROM sqlite_master WHERE type='table';")?;
    let mut tables = HashSet::new();
    while statement.step()? {
        tables.insert(statement.column_text_string(0));
    }
    for table in REQUIRED_TABLES {
        if !tables.contains(table) {
            return Err(sqlite_db::DictError::InvalidSchema(format!(
                "Missing required table '{table}'"
            )));
        }
    }
    Ok(())
}

fn load_dictionary_names(
    database: *mut sqlite_db::sqlite3,
) -> Result<HashMap<i64, String>, sqlite_db::DictError> {
    let mut statement =
        sqlite_db::Statement::prepare(database, "SELECT dic_id, title FROM directory;")?;
    let mut names = HashMap::new();
    while statement.step()? {
        names.insert(statement.column_int64(0), statement.column_text_string(1));
    }
    Ok(names)
}


fn load_dictionary_tags(
    database: *mut sqlite_db::sqlite3,
) -> Result<HashMap<(i64, String), DictTag>, sqlite_db::DictError> {
    let mut statement = sqlite_db::Statement::prepare(
        database,
        "SELECT dic_id, category, name, ord, notes, score FROM tag_bank \
         ORDER BY dic_id ASC, ord ASC, score DESC;",
    )?;
    let mut tags = HashMap::new();
    while statement.step()? {
        let dictionary_id = statement.column_int64(0);
        let category = statement.column_text_string(1);
        let name = statement.column_text_string(2);
        let order = i64::from(statement.column_int(3));
        let notes = statement.column_text_string(4);
        let score = i64::from(statement.column_int(5));
        tags.insert(
            (dictionary_id, name.clone()),
            DictTag::with_details(name, category, notes, order, score),
        );
    }
    Ok(tags)
}

fn database_search_candidates(
    database: &ReadOnlyDictionaryDb,
    query: &str,
    normalized_query: &str,
    lookups: &[LookupQuery],
) -> Result<Vec<DatabaseCandidate>, DictControllerError> {
    const TERM_DETAILS_QUERY: &str =
        "SELECT term_bank.rowid, term_bank.dic_id, term_bank.def_tags, term_bank.term_tags, \
                term_bank.score, term_bank.glossary, directory.title \
         FROM term_bank \
         JOIN directory ON directory.dic_id = term_bank.dic_id \
         WHERE term_bank.expression = ?1 AND term_bank.reading = ?2 \
           AND NOT EXISTS (SELECT 1 FROM dict_disabled \
                           WHERE dict_disabled.dic_id = term_bank.dic_id) \
         ORDER BY term_bank.dic_id ASC, term_bank.sequence ASC;";

    let mut details_statement = sqlite_db::Statement::prepare(database.handle, TERM_DETAILS_QUERY)
        .map_err(DictControllerError::Sqlite)?;
    let mut results = Vec::new();
    for lookup in lookups {
        if lookup.key.is_empty() {
            continue;
        }
        let prefix = String::from_utf16_lossy(&lookup.key);
        if prefix.trim().is_empty() {
            continue;
        }

        let normalized_prefix = normalize_nfkc_query(&prefix);
        let normalized_prefix: Vec<u16> = normalized_prefix.encode_utf16().collect();
        let plan = sqlite_db::TermQueryPlan::new(&lookup.key, &normalized_prefix);
        let mut terms = Vec::new();
        sqlite_db::query_terms(database.handle, &plan, &mut terms)
            .map_err(DictControllerError::Sqlite)?;

        for term in terms {
            let expression = String::from_utf16_lossy(&term.expression);
            let reading = String::from_utf16_lossy(&term.reading);
            details_statement.reset().map_err(DictControllerError::Sqlite)?;
            details_statement
                .bind_u16_as_utf8(1, &term.expression)
                .map_err(DictControllerError::Sqlite)?;
            details_statement
                .bind_u16_as_utf8(2, &term.reading)
                .map_err(DictControllerError::Sqlite)?;

            while details_statement.step().map_err(DictControllerError::Sqlite)? {
                let source_row_id = details_statement.column_int64(0);
                let dictionary_id = details_statement.column_int64(1);
                let def_tags = details_statement.column_text_string(2);
                let term_tags = details_statement.column_text_string(3);
                let score = i64::from(details_statement.column_int(4));
                let raw_glossary = details_statement.column_text_string(5);
                let dictionary_name = details_statement.column_text_string(6);
                let norm_expression = normalize_reading(&expression);
                let norm_reading = normalize_reading(&reading);
                let parsed_glossary = serde_json::from_str::<serde_json::Value>(&raw_glossary);
                let (glossary, example_ja, example_en, rich_glossary) = match parsed_glossary.as_ref() {
                    Ok(value) => {
                        let (glossary, example_ja, example_en) = parse_glossary_entry_value(value);
                        (glossary, example_ja, example_en, parse_rich_glossary_value(value))
                    }
                    Err(_) => (strip_glossary_markup(&raw_glossary), String::new(), String::new(),
                        parse_rich_glossary(&raw_glossary)),
                };
                results.push(DatabaseCandidate {
                    hit: SearchHit {
                        expression: expression.clone(),
                        reading: reading.clone(),
                        glossary,
                        tags: format_dictionary_tags(
                            &database.tags,
                            dictionary_id,
                            &def_tags,
                            &term_tags,
                        ),
                        score,
                        is_kanji: false,
                        notes: String::new(),
                        example_ja,
                        example_en,
                        dict_name: dictionary_name.clone(),
                        rich_glossary,
                    },
                    is_exact: expression == query
                        || norm_expression == normalized_query
                        || (!norm_reading.is_empty() && norm_reading == normalized_query),
                    prefix_len: lookup.surface_len,
                    dict_priority: dictionary_id,
                    source_row_id: Some(source_row_id),
                });
            }
        }
    }
    Ok(results)
}

fn database_kanji_candidates(
    database: &ReadOnlyDictionaryDb,
    query: &str,
    prefix_len: usize,
) -> Result<Vec<DatabaseCandidate>, DictControllerError> {
    let query_units: Vec<u16> = query.encode_utf16().collect();
    let mut kanji_rows = Vec::new();
    sqlite_db::query_kanji(database.handle, &query_units, &mut kanji_rows)
        .map_err(DictControllerError::Sqlite)?;

    let mut results = Vec::with_capacity(kanji_rows.len());
    for row in kanji_rows {
        let Some(dictionary_name) = database.names.get(&row.dic_id) else {
            continue;
        };
        let onyomi = String::from_utf16_lossy(&row.onyomi);
        let kunyomi = String::from_utf16_lossy(&row.kunyomi);
        let raw_tags = String::from_utf16_lossy(&row.tags);
        let raw_stats = String::from_utf16_lossy(&row.stats);
        let glossary = row
            .meanings
            .iter()
            .map(|meaning| String::from_utf16_lossy(meaning))
            .collect::<Vec<_>>()
            .join("\n");
        results.push(DatabaseCandidate {
            hit: SearchHit {
                expression: query.to_owned(),
                reading: format_kanji_reading(&onyomi, &kunyomi),
                glossary,
                tags: format_dictionary_tags(&database.tags, row.dic_id, "", &raw_tags),
                score: 0,
                is_kanji: true,
                notes: format_kanji_stats(&database.tags, row.dic_id, &raw_stats),
                example_ja: String::new(),
                example_en: String::new(),
                dict_name: dictionary_name.clone(),
                rich_glossary: Vec::new(),
            },
            is_exact: true,
            prefix_len,
            dict_priority: row.dic_id,
            source_row_id: None,
        });
    }
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
    stats
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
            (!value.is_empty()).then(|| format!("{}: {value}", tag.name))
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
    format_tags(&selected)
}

fn rich_node_text(node: &serde_json::Value) -> String {
    match node {
        serde_json::Value::String(value) => strip_glossary_markup(value),
        serde_json::Value::Array(items) => items.iter().map(rich_node_text).collect(),
        serde_json::Value::Object(map) => {
            if matches!(map.get("tag").and_then(serde_json::Value::as_str), Some("rt" | "rp" | "img")) {
                return String::new();
            }
            map.get("content").or_else(|| map.get("text")).map(rich_node_text).unwrap_or_default()
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
            blocks.push(GlossaryBlock { text, kind, href: href.to_owned() });
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
                for item in items { walk(item, blocks); }
            }
            serde_json::Value::Object(map) => {
                let tag = map.get("tag").and_then(serde_json::Value::as_str).unwrap_or("");
                let data = map.get("data").and_then(serde_json::Value::as_object);
                if data.and_then(|data| data.get("content")).and_then(serde_json::Value::as_str) == Some("examples") {
                    return;
                }
                let Some(content) = map.get("content") else { return; };
                match tag {
                    "rt" | "rp" | "img" => {},
                    "li" => push(blocks, rich_node_text(content), 2, ""),
                    "a" => push(blocks, rich_node_text(content), 3,
                        map.get("href").and_then(serde_json::Value::as_str).unwrap_or("")),
                    "span" => {
                        let has_link = content.as_array().is_some_and(|items| items.iter().any(|item|
                            item.get("tag").and_then(serde_json::Value::as_str) == Some("a")));
                        if has_link { walk(content, blocks); }
                        else {
                            let bold = map.get("style").and_then(|style| style.get("fontWeight"))
                                .and_then(serde_json::Value::as_str) == Some("bold")
                                || content.as_array().is_some_and(|items| items.iter().any(|item|
                                    item.get("style").and_then(|style| style.get("fontWeight"))
                                        .and_then(serde_json::Value::as_str) == Some("bold")));
                            push(blocks, rich_node_text(content), if bold { 1 } else { 0 }, "");
                        }
                    }
                    "div" if data.and_then(|data| data.get("name")).and_then(serde_json::Value::as_str) == Some("definition") =>
                        push(blocks, rich_node_text(content), 0, ""),
                    "div" | "ul" | "ol" => walk(content, blocks),
                    _ => {
                        if content.is_string() { push(blocks, rich_node_text(content), 0, ""); }
                        else { walk(content, blocks); }
                    }
                }
            }
            _ => {},
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
        strip_glossary_markup(raw).lines().filter_map(|line| {
            let text = line.trim();
            (!text.is_empty()).then(|| GlossaryBlock { text: text.to_owned(), kind: 0, href: String::new() })
        }).collect()
    }
}

pub fn parse_glossary_entry(raw: &str) -> (String, String, String) {
    match serde_json::from_str::<serde_json::Value>(raw) {
        Ok(value) => parse_glossary_entry_value(&value),
        Err(_) => (strip_glossary_markup(raw), String::new(), String::new()),
    }
}

fn parse_glossary_entry_value(value: &serde_json::Value) -> (String, String, String) {
    let items = value.as_array().map_or_else(|| vec![value], |items| items.iter().collect());
    let mut senses = Vec::new();
    let mut ex_ja = Vec::new();
    let mut ex_en = Vec::new();

    fn extract_plain_text(node: &serde_json::Value) -> String {
        match node {
            serde_json::Value::String(text) => strip_glossary_markup(text),
            serde_json::Value::Array(items) => {
                items.iter().map(extract_plain_text).collect::<Vec<_>>().join("")
            }
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
                if map.get("type").and_then(serde_json::Value::as_str) == Some("structured-content") =>
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

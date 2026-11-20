//! Shared read-only SQLite dictionary database pool, metadata snapshots, and bounded LRU cache.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::{Duration, Instant};

use memento_dict::sqlite_db;

use super::{DictControllerError, DictTag};

const REFRESH_THROTTLE: Duration = Duration::from_millis(50);
const TERM_META_LRU_CAPACITY: usize = 2048;
const TERM_META_LRU_EVICT_BATCH: usize = 256;

/// Cheaply cloneable, lock-free snapshot of cached dictionary names, tags, disabled flags,
/// and `frequencyMode` (`rank-based` vs `occurrence-based`).
#[derive(Debug, Clone)]
pub struct DictionaryMetadataSnapshot {
    pub names: Arc<HashMap<i64, String>>,
    pub tags: Arc<HashMap<(i64, String), DictTag>>,
    pub disabled: Arc<HashSet<i64>>,
    pub frequency_modes: Arc<HashMap<i64, sqlite_db::FrequencyMode>>,
}

impl DictionaryMetadataSnapshot {
    /// Returns the `frequencyMode` for `dic_id`, defaulting to [`sqlite_db::FrequencyMode::RankBased`].
    #[inline]
    pub fn frequency_mode_for(&self, dic_id: i64) -> sqlite_db::FrequencyMode {
        self.frequency_modes
            .get(&dic_id)
            .copied()
            .unwrap_or(sqlite_db::FrequencyMode::RankBased)
    }
}

struct TermMetaLru {
    entries: HashMap<String, (u64, Arc<Vec<sqlite_db::RawTermMetaRow>>)>,
    clock: u64,
}

impl TermMetaLru {
    fn new() -> Self {
        Self {
            entries: HashMap::new(),
            clock: 0,
        }
    }

    fn clear(&mut self) {
        self.entries.clear();
        self.clock = 0;
    }

    fn contains(&self, key: &str) -> bool {
        self.entries.contains_key(key)
    }

    fn get(&mut self, key: &str) -> Option<Arc<Vec<sqlite_db::RawTermMetaRow>>> {
        self.clock = self.clock.wrapping_add(1);
        let stamp = self.clock;
        if let Some((last_used, rows)) = self.entries.get_mut(key) {
            *last_used = stamp;
            Some(Arc::clone(rows))
        } else {
            None
        }
    }

    fn insert(&mut self, key: String, rows: Vec<sqlite_db::RawTermMetaRow>) {
        if self.entries.len() >= TERM_META_LRU_CAPACITY && !self.entries.contains_key(&key) {
            self.evict_oldest();
        }
        self.clock = self.clock.wrapping_add(1);
        self.entries.insert(key, (self.clock, Arc::new(rows)));
    }

    fn evict_oldest(&mut self) {
        let mut stamps: Vec<(u64, String)> = self
            .entries
            .iter()
            .map(|(k, (stamp, _))| (*stamp, k.clone()))
            .collect();
        let evict_count = TERM_META_LRU_EVICT_BATCH.min(stamps.len());
        if evict_count == 0 {
            return;
        }
        stamps.select_nth_unstable_by_key(evict_count - 1, |(stamp, _)| *stamp);
        for (_, key) in stamps.into_iter().take(evict_count) {
            self.entries.remove(&key);
        }
    }
}

struct DictionaryMetadataCache {
    data_version: Option<i64>,
    fingerprint: sqlite_db::DbFileFingerprint,
    last_checked: Instant,
    force_refresh: bool,
    tags: Arc<HashMap<(i64, String), DictTag>>,
    names: Arc<HashMap<i64, String>>,
    disabled: Arc<HashSet<i64>>,
    frequency_modes: Arc<HashMap<i64, sqlite_db::FrequencyMode>>,
    recent_term_meta: TermMetaLru,
}

impl DictionaryMetadataCache {
    fn snapshot(&self) -> DictionaryMetadataSnapshot {
        DictionaryMetadataSnapshot {
            names: Arc::clone(&self.names),
            tags: Arc::clone(&self.tags),
            disabled: Arc::clone(&self.disabled),
            frequency_modes: Arc::clone(&self.frequency_modes),
        }
    }
}

/// Shared read-only SQLite connection and bounded metadata/term-meta LRU cache.
pub struct ReadOnlyDictionaryDb {
    path: PathBuf,
    conn: Mutex<sqlite_db::rusqlite::Connection>,
    cache: Mutex<DictionaryMetadataCache>,
}

impl ReadOnlyDictionaryDb {
    fn open_new(path: &Path) -> Result<Self, DictControllerError> {
        let conn =
            sqlite_db::open_read_only_connection(path).map_err(DictControllerError::Sqlite)?;
        sqlite_db::validate_schema_conn(&conn).map_err(DictControllerError::Sqlite)?;
        let tags = load_dictionary_tags_conn(&conn).map_err(DictControllerError::Sqlite)?;
        let (names, disabled) =
            load_dictionary_names_and_disabled_conn(&conn).map_err(DictControllerError::Sqlite)?;
        let frequency_modes = sqlite_db::query_dictionary_frequency_modes_conn(&conn)
            .map_err(DictControllerError::Sqlite)?;
        let data_version = sqlite_db::query_data_version_conn(&conn).ok();
        let fingerprint = sqlite_db::db_file_fingerprint(path);

        Ok(Self {
            path: path.to_path_buf(),
            conn: Mutex::new(conn),
            cache: Mutex::new(DictionaryMetadataCache {
                data_version,
                fingerprint,
                last_checked: Instant::now(),
                force_refresh: false,
                tags: Arc::new(tags),
                names: Arc::new(names),
                disabled: Arc::new(disabled),
                frequency_modes: Arc::new(frequency_modes),
                recent_term_meta: TermMetaLru::new(),
            }),
        })
    }

    /// Returns the filesystem path of the opened SQLite database.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Marks cached metadata dirty so the next operation re-reads SQLite state immediately.
    pub fn invalidate(&self) {
        let mut cache = self
            .cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        cache.force_refresh = true;
    }

    /// Executes `f` while holding the read-only SQLite connection lock.
    pub fn with_conn<R>(&self, f: impl FnOnce(&sqlite_db::rusqlite::Connection) -> R) -> R {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        f(&conn)
    }

    /// Refreshes cached dictionary metadata if the database file/WAL or `PRAGMA data_version`
    /// changed, throttling repeated `PRAGMA data_version` queries when the file fingerprint is
    /// unchanged within [`REFRESH_THROTTLE`].
    pub fn refresh_metadata_if_needed(&self) -> Result<(), sqlite_db::DictError> {
        let current_fingerprint = sqlite_db::db_file_fingerprint(&self.path);
        {
            let cache = self
                .cache
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if !cache.force_refresh
                && cache.fingerprint == current_fingerprint
                && cache.last_checked.elapsed() < REFRESH_THROTTLE
            {
                return Ok(());
            }
        }

        let conn = self
            .conn
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let current_version = sqlite_db::query_data_version_conn(&conn).ok();
        let mut cache = self
            .cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !cache.force_refresh
            && cache.data_version == current_version
            && cache.fingerprint == current_fingerprint
        {
            cache.last_checked = Instant::now();
            return Ok(());
        }
        cache.tags = Arc::new(load_dictionary_tags_conn(&conn)?);
        let (names, disabled) = load_dictionary_names_and_disabled_conn(&conn)?;
        cache.names = Arc::new(names);
        cache.disabled = Arc::new(disabled);
        cache.frequency_modes = Arc::new(sqlite_db::query_dictionary_frequency_modes_conn(&conn)?);
        cache.recent_term_meta.clear();
        cache.data_version = current_version;
        cache.fingerprint = current_fingerprint;
        cache.last_checked = Instant::now();
        cache.force_refresh = false;
        Ok(())
    }

    /// Refreshes metadata if needed and returns an `Arc`-backed snapshot so callers can run
    /// JSON parsing and formatting without holding any mutex lock.
    pub fn metadata_snapshot(&self) -> Result<DictionaryMetadataSnapshot, sqlite_db::DictError> {
        self.refresh_metadata_if_needed()?;
        Ok(self.metadata_snapshot_unchecked())
    }

    /// Returns the current metadata snapshot without triggering another refresh check
    /// (for callers that already refreshed at the start of a batch).
    pub(super) fn metadata_snapshot_unchecked(&self) -> DictionaryMetadataSnapshot {
        let cache = self
            .cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        cache.snapshot()
    }

    /// Batch-loads `term_meta_bank` rows for any expressions not currently in the LRU cache.
    pub fn preload_term_meta(&self, expressions: &[String]) {
        if expressions.is_empty() || self.refresh_metadata_if_needed().is_err() {
            return;
        }
        self.preload_term_meta_no_refresh(expressions);
    }

    pub(super) fn preload_term_meta_no_refresh(&self, expressions: &[String]) {
        if expressions.is_empty() {
            return;
        }
        let missing: Vec<String> = {
            let cache = self
                .cache
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let mut seen = HashSet::with_capacity(expressions.len());
            expressions
                .iter()
                .filter(|expr| {
                    !expr.is_empty()
                        && !cache.recent_term_meta.contains(expr.as_str())
                        && seen.insert(expr.as_str())
                })
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

        let mut grouped: HashMap<String, Vec<sqlite_db::RawTermMetaRow>> =
            HashMap::with_capacity(missing.len());
        for expr in missing {
            grouped.insert(expr, Vec::new());
        }
        for row in rows {
            grouped
                .entry(row.expression.clone())
                .or_default()
                .push(row);
        }

        let mut cache = self
            .cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for (expr, expr_rows) in grouped {
            cache.recent_term_meta.insert(expr, expr_rows);
        }
    }

    /// Fetches cached or newly queried `term_meta_bank` rows for `expression` as a shared `Arc`.
    pub(super) fn term_meta_rows_arc_no_refresh(
        &self,
        expression: &str,
    ) -> Arc<Vec<sqlite_db::RawTermMetaRow>> {
        static EMPTY: OnceLock<Arc<Vec<sqlite_db::RawTermMetaRow>>> = OnceLock::new();
        let empty = || Arc::clone(EMPTY.get_or_init(|| Arc::new(Vec::new())));

        if expression.is_empty() {
            return empty();
        }
        {
            let mut cache = self
                .cache
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(cached) = cache.recent_term_meta.get(expression) {
                return cached;
            }
        }
        let expr_owned = expression.to_owned();
        self.preload_term_meta_no_refresh(std::slice::from_ref(&expr_owned));
        let mut cache = self
            .cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        cache
            .recent_term_meta
            .get(expression)
            .unwrap_or_else(empty)
    }

    /// Fetches `term_meta_bank` rows for `expression` (refreshing metadata if stale).
    pub fn term_meta_rows_for(&self, expression: &str) -> Vec<sqlite_db::RawTermMetaRow> {
        if expression.is_empty() || self.refresh_metadata_if_needed().is_err() {
            return Vec::new();
        }
        (*self.term_meta_rows_arc_no_refresh(expression)).clone()
    }
}

fn canonical_db_key(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn shared_db_registry() -> &'static Mutex<HashMap<PathBuf, Weak<ReadOnlyDictionaryDb>>> {
    static REGISTRY: OnceLock<Mutex<HashMap<PathBuf, Weak<ReadOnlyDictionaryDb>>>> =
        OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Returns a shared `Arc<ReadOnlyDictionaryDb>` for `path`, reusing any live instance
/// already opened in this process (including its warm SQLite connection and LRU cache).
pub fn shared_readonly_db(path: &Path) -> Result<Arc<ReadOnlyDictionaryDb>, DictControllerError> {
    if !path.is_file() {
        return Err(DictControllerError::MissingDatabase(path.to_path_buf()));
    }
    if path.to_str().is_some_and(|s| s.contains('\0')) {
        return Err(DictControllerError::InvalidPath(
            "database path contains interior NUL byte".to_owned(),
        ));
    }

    let key = canonical_db_key(path);
    {
        let registry = shared_db_registry()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(existing) = registry.get(&key).and_then(Weak::upgrade) {
            return Ok(existing);
        }
    }

    let opened = Arc::new(ReadOnlyDictionaryDb::open_new(path)?);
    let mut registry = shared_db_registry()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(existing) = registry.get(&key).and_then(Weak::upgrade) {
        return Ok(existing);
    }
    if registry.len() > 32 {
        registry.retain(|_, weak| weak.strong_count() > 0);
    }
    registry.insert(key, Arc::downgrade(&opened));
    Ok(opened)
}

/// Invalidates any live shared `ReadOnlyDictionaryDb` handle pointing at `path`
/// after a write/mutation (dictionary import, toggle, or deletion).
pub fn invalidate_shared_readonly_db(path: &Path) {
    let key = canonical_db_key(path);
    let existing = {
        let registry = shared_db_registry()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        registry.get(&key).and_then(Weak::upgrade)
    };
    if let Some(db) = existing {
        db.invalidate();
    }
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

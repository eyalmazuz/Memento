//! Pure Rust SQLite Yomichan/Yomitan query engine, batched UTF-8 helpers, and cache validator.

use std::collections::HashSet;
use std::ffi::{c_char, c_int, CStr, CString};
use std::fmt;
use std::path::Path;
use std::time::Duration;

pub use rusqlite;
use rusqlite::types::ValueRef;
use rusqlite::{params, params_from_iter, Connection, OpenFlags, Row};

pub use rusqlite::ffi::{
    sqlite3, sqlite3_errmsg, sqlite3_exec, sqlite3_stmt, SQLITE_DONE, SQLITE_OK, SQLITE_ROW,
};

pub use crate::databasemanager::{PitchEntryDetail, TermQueryKind};

const DEFAULT_BUSY_TIMEOUT: Duration = Duration::from_secs(5);
const STATEMENT_CACHE_CAPACITY: usize = 64;

/// Closes a raw SQLite handle via `sqlite3_close_v2`.
///
/// # Safety
/// `db` must be null or a valid SQLite handle opened by `sqlite3_open_v2`.
pub unsafe fn sqlite3_close_v2(db: *mut sqlite3) -> c_int {
    unsafe extern "C" {
        fn sqlite3_close_v2(db: *mut sqlite3) -> c_int;
    }
    unsafe { sqlite3_close_v2(db) }
}

/// File length and modification timestamp snapshot used to detect external changes.
pub type FileMetaStamp = Option<(u64, Option<std::time::SystemTime>)>;

/// Combined `(main_db, wal_file)` metadata fingerprint for WAL-aware change detection.
pub type DbFileFingerprint = (FileMetaStamp, FileMetaStamp);

/// Returns the `(len, modified)` metadata for both `path` and `<path>-wal`.
///
/// Under SQLite WAL mode, commits from other connections may only update
/// `<path>-wal` without changing the main database file's size or mtime.
#[must_use]
pub fn db_file_fingerprint(path: &Path) -> DbFileFingerprint {
    fn stamp(target: &Path) -> FileMetaStamp {
        std::fs::metadata(target)
            .ok()
            .map(|metadata| (metadata.len(), metadata.modified().ok()))
    }
    let mut wal_os = path.as_os_str().to_os_string();
    wal_os.push("-wal");
    let wal_path = std::path::PathBuf::from(wal_os);
    (stamp(path), stamp(&wal_path))
}

/// Opens a raw SQLite handle via `rusqlite::ffi::sqlite3_open_v2` and applies
/// busy-timeout and read-friendly PRAGMAs (`temp_store = MEMORY`, `synchronous = NORMAL`).
///
/// # Safety
/// `file`, `db`, and `vfs` must satisfy SQLite's `sqlite3_open_v2` preconditions.
pub unsafe fn sqlite3_open_v2(
    file: *const c_char,
    db: *mut *mut sqlite3,
    flags: c_int,
    vfs: *const c_char,
) -> c_int {
    let rc = unsafe { rusqlite::ffi::sqlite3_open_v2(file, db, flags, vfs) };
    if rc == SQLITE_OK && !db.is_null() {
        let handle = unsafe { *db };
        if !handle.is_null() {
            let _ = unsafe { rusqlite::ffi::sqlite3_busy_timeout(handle, 5000) };
            let _ = unsafe {
                rusqlite::ffi::sqlite3_exec(
                    handle,
                    c"PRAGMA temp_store = MEMORY; PRAGMA synchronous = NORMAL;".as_ptr(),
                    None,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                )
            };
        }
    }
    rc
}

/// Opens a read-only `rusqlite::Connection` configured with `busy_timeout(5s)`,
/// `PRAGMA temp_store = MEMORY`, `PRAGMA synchronous = NORMAL`, and statement caching.
pub fn open_read_only_connection(path: &Path) -> Result<Connection, DictError> {
    if path.as_os_str().is_empty() {
        return Err(DictError::Other("Database path is empty".to_owned()));
    }
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_FULL_MUTEX,
    )?;
    configure_read_only_connection(&conn)?;
    Ok(conn)
}

/// Applies busy timeout, in-memory temp store PRAGMAs, and statement cache capacity on a read-only connection.
pub fn configure_read_only_connection(conn: &Connection) -> Result<(), DictError> {
    conn.busy_timeout(DEFAULT_BUSY_TIMEOUT)?;
    conn.set_prepared_statement_cache_capacity(STATEMENT_CACHE_CAPACITY);
    conn.pragma_update(None, "temp_store", "MEMORY")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    Ok(())
}

/// Opens a read-write `rusqlite::Connection` configured with `WAL`,
/// `synchronous = NORMAL`, `temp_store = MEMORY`, and `busy_timeout(5s)`.
pub fn open_read_write_connection(path: &Path) -> Result<Connection, DictError> {
    if path.as_os_str().is_empty() {
        return Err(DictError::Other("Database path is empty".to_owned()));
    }
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE,
    )?;
    configure_read_write_connection(&conn)?;
    Ok(conn)
}

/// Applies WAL, synchronous, temp_store, and busy_timeout PRAGMAs on a writable connection.
pub fn configure_read_write_connection(conn: &Connection) -> Result<(), DictError> {
    conn.busy_timeout(DEFAULT_BUSY_TIMEOUT)?;
    conn.set_prepared_statement_cache_capacity(STATEMENT_CACHE_CAPACITY);
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "temp_store", "MEMORY")?;
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DictError {
    Sqlite { code: i32, message: String },
    InvalidSchema(String),
    Json(String),
    NullPointer,
    Other(String),
}

impl fmt::Display for DictError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Sqlite { code, message } => write!(f, "SQLite error {code}: {message}"),
            Self::InvalidSchema(msg) => write!(f, "Invalid schema: {msg}"),
            Self::Json(msg) => write!(f, "JSON error: {msg}"),
            Self::NullPointer => write!(f, "Null database pointer"),
            Self::Other(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for DictError {}

impl From<rusqlite::Error> for DictError {
    fn from(err: rusqlite::Error) -> Self {
        match err {
            rusqlite::Error::SqliteFailure(ffi_err, msg) => Self::Sqlite {
                code: ffi_err.extended_code,
                message: msg.unwrap_or_else(|| ffi_err.to_string()),
            },
            other => Self::Other(other.to_string()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DictMeta {
    pub id: i64,
    pub title: String,
    pub disabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TableCounts {
    pub directory: i64,
    pub tag_bank: i64,
    pub term_bank: i64,
    pub term_meta_bank: i64,
    pub kanji_bank: i64,
    pub kanji_meta_bank: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheInfo {
    pub dictionaries: Vec<DictMeta>,
    pub table_counts: TableCounts,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TermQueryPlan {
    pub raw: Vec<u16>,
    pub nfkc: Vec<u16>,
    pub hiragana: Vec<u16>,
    pub contains_half: bool,
    pub contains_kata: bool,
    pub query_kind: TermQueryKind,
}

impl TermQueryPlan {
    #[must_use]
    pub fn new(raw: &[u16], nfkc: &[u16]) -> Self {
        let base = crate::databasemanager::plan_term_query(raw, nfkc);
        Self {
            raw: raw.to_vec(),
            nfkc: nfkc.to_vec(),
            hiragana: base.hiragana,
            contains_half: base.contains_half,
            contains_kata: base.contains_kata,
            query_kind: base.query_kind,
        }
    }

    #[must_use]
    pub fn from_base(raw: &[u16], nfkc: &[u16], base: crate::databasemanager::TermQueryPlan) -> Self {
        Self {
            raw: raw.to_vec(),
            nfkc: nfkc.to_vec(),
            hiragana: base.hiragana,
            contains_half: base.contains_half,
            contains_kata: base.contains_kata,
            query_kind: base.query_kind,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TermRow {
    pub expression: Vec<u16>,
    pub reading: Vec<u16>,
}

/// Complete UTF-8 `term_bank` row including `rules` for fast batched lookup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchTermRow {
    pub rowid: i64,
    pub dic_id: i64,
    pub expression: String,
    pub reading: String,
    pub def_tags: String,
    pub term_tags: String,
    pub rules: String,
    pub score: i64,
    pub glossary: String,
    pub sequence: i64,
}

/// Raw UTF-8 metadata row fetched from `term_meta_bank` or `kanji_meta_bank`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawTermMetaRow {
    pub dic_id: i64,
    pub expression: String,
    pub mode: String,
    pub blob_type: i32,
    pub data: Vec<u8>,
}

/// Complete UTF-8 `kanji_bank` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KanjiUtf8Row {
    pub dic_id: i64,
    pub onyomi: String,
    pub kunyomi: String,
    pub tags: String,
    pub meanings: Vec<String>,
    pub raw_meanings: String,
    pub stats: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KanjiRow {
    pub dic_id: i64,
    pub onyomi: Vec<u16>,
    pub kunyomi: Vec<u16>,
    pub tags: Vec<u16>,
    pub meanings: Vec<Vec<u16>>,
    pub stats: Vec<u16>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FreqRow {
    pub dic_id: i64,
    pub frequency: Vec<u16>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PitchRow {
    pub dic_id: i64,
    pub mora: Vec<Vec<u16>>,
    pub positions: Vec<i32>,
    pub nasal: Vec<usize>,
    pub devoice: Vec<usize>,
    pub tags: Vec<String>,
    pub entries: Vec<PitchEntryDetail>,
}

/// Parsed UTF-8 frequency value with homograph reading match info and raw numeric value.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ParsedFreqUtf8 {
    pub matched: bool,
    pub exact_reading_match: bool,
    pub has_reading_key: bool,
    pub display: String,
    pub numeric: Option<f64>,
}

/// RAII statement handle for callers holding a borrowed `*mut sqlite3`.
pub struct Statement {
    stmt: *mut sqlite3_stmt,
    bound_strings: Vec<CString>,
}

impl Statement {
    pub fn prepare(db: *mut sqlite3, sql: &str) -> Result<Self, DictError> {
        if db.is_null() {
            return Err(DictError::NullPointer);
        }
        let c_sql = CString::new(sql).map_err(|e| DictError::Other(e.to_string()))?;
        let mut stmt: *mut sqlite3_stmt = std::ptr::null_mut();
        let rc = unsafe {
            rusqlite::ffi::sqlite3_prepare_v2(
                db,
                c_sql.as_ptr(),
                -1,
                &mut stmt,
                std::ptr::null_mut(),
            )
        };
        if rc != SQLITE_OK || stmt.is_null() {
            if !stmt.is_null() {
                unsafe { rusqlite::ffi::sqlite3_finalize(stmt) };
            }
            let msg = unsafe {
                let err = sqlite3_errmsg(db);
                if err.is_null() {
                    "Unknown error".into()
                } else {
                    CStr::from_ptr(err).to_string_lossy().into_owned()
                }
            };
            return Err(DictError::Sqlite {
                code: rc,
                message: msg,
            });
        }
        Ok(Self {
            stmt,
            bound_strings: Vec::new(),
        })
    }

    pub fn bind_text(&mut self, index: i32, text: &str) -> Result<(), DictError> {
        let c_str = CString::new(text).map_err(|e| DictError::Other(e.to_string()))?;
        let len = c_int::try_from(c_str.as_bytes().len())
            .map_err(|_| DictError::Other("Bound text exceeds c_int::MAX".to_owned()))?;
        let rc =
            unsafe { rusqlite::ffi::sqlite3_bind_text(self.stmt, index, c_str.as_ptr(), len, None) };
        if rc != SQLITE_OK {
            return Err(DictError::Sqlite {
                code: rc,
                message: format!("bind_text failed at index {index}"),
            });
        }
        self.bound_strings.push(c_str);
        Ok(())
    }

    pub fn bind_u16_as_utf8(&mut self, index: i32, text: &[u16]) -> Result<(), DictError> {
        let s = String::from_utf16_lossy(text);
        self.bind_text(index, &s)
    }

    pub fn step(&mut self) -> Result<bool, DictError> {
        let rc = unsafe { rusqlite::ffi::sqlite3_step(self.stmt) };
        if rc == SQLITE_ROW {
            Ok(true)
        } else if rc == SQLITE_DONE {
            Ok(false)
        } else {
            Err(DictError::Sqlite {
                code: rc,
                message: format!("step failed with code {rc}"),
            })
        }
    }

    pub fn reset(&mut self) -> Result<(), DictError> {
        let reset = unsafe { rusqlite::ffi::sqlite3_reset(self.stmt) };
        if reset != SQLITE_OK {
            return Err(DictError::Sqlite {
                code: reset,
                message: "reset failed".into(),
            });
        }
        let clear = unsafe { rusqlite::ffi::sqlite3_clear_bindings(self.stmt) };
        if clear != SQLITE_OK {
            return Err(DictError::Sqlite {
                code: clear,
                message: "clear bindings failed".into(),
            });
        }
        self.bound_strings.clear();
        Ok(())
    }

    #[must_use]
    pub fn column_int(&self, index: i32) -> i32 {
        unsafe { rusqlite::ffi::sqlite3_column_int(self.stmt, index) }
    }

    #[must_use]
    pub fn column_int64(&self, index: i32) -> i64 {
        unsafe { rusqlite::ffi::sqlite3_column_int64(self.stmt, index) }
    }

    #[must_use]
    pub fn column_double(&self, index: i32) -> f64 {
        unsafe { rusqlite::ffi::sqlite3_column_double(self.stmt, index) }
    }

    #[must_use]
    pub fn column_text_string(&self, index: i32) -> String {
        unsafe {
            let ptr = rusqlite::ffi::sqlite3_column_text(self.stmt, index);
            if ptr.is_null() {
                return String::new();
            }
            let len = rusqlite::ffi::sqlite3_column_bytes(self.stmt, index).max(0) as usize;
            let slice = std::slice::from_raw_parts(ptr, len);
            String::from_utf8_lossy(slice).into_owned()
        }
    }

    #[must_use]
    pub fn column_text_u16(&self, index: i32) -> Vec<u16> {
        unsafe {
            let ptr = rusqlite::ffi::sqlite3_column_text(self.stmt, index);
            if ptr.is_null() {
                return Vec::new();
            }
            let len = rusqlite::ffi::sqlite3_column_bytes(self.stmt, index).max(0) as usize;
            let slice = std::slice::from_raw_parts(ptr, len);
            let s = std::str::from_utf8(slice).unwrap_or("");
            s.encode_utf16().collect()
        }
    }

    #[must_use]
    pub fn column_blob_bytes(&self, index: i32) -> Vec<u8> {
        unsafe {
            let ptr = rusqlite::ffi::sqlite3_column_blob(self.stmt, index);
            if ptr.is_null() {
                return Vec::new();
            }
            let len = rusqlite::ffi::sqlite3_column_bytes(self.stmt, index).max(0) as usize;
            std::slice::from_raw_parts(ptr.cast::<u8>(), len).to_vec()
        }
    }

    pub fn query_map<T, F>(&mut self, extract: F) -> QueryIterator<'_, T, F>
    where
        F: FnMut(&Statement) -> Result<T, DictError>,
    {
        QueryIterator::new(self, extract)
    }
}

impl Drop for Statement {
    fn drop(&mut self) {
        if !self.stmt.is_null() {
            unsafe { rusqlite::ffi::sqlite3_finalize(self.stmt) };
            self.stmt = std::ptr::null_mut();
        }
        self.bound_strings.clear();
    }
}

/// Idiomatic query row iterator.
pub struct QueryIterator<'a, T, F>
where
    F: FnMut(&Statement) -> Result<T, DictError>,
{
    stmt: &'a mut Statement,
    extract: F,
    done: bool,
}

impl<'a, T, F> QueryIterator<'a, T, F>
where
    F: FnMut(&Statement) -> Result<T, DictError>,
{
    pub fn new(stmt: &'a mut Statement, extract: F) -> Self {
        Self {
            stmt,
            extract,
            done: false,
        }
    }
}

impl<'a, T, F> Iterator for QueryIterator<'a, T, F>
where
    F: FnMut(&Statement) -> Result<T, DictError>,
{
    type Item = Result<T, DictError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        match self.stmt.step() {
            Ok(true) => Some((self.extract)(self.stmt)),
            Ok(false) => {
                self.done = true;
                None
            }
            Err(e) => {
                self.done = true;
                Some(Err(e))
            }
        }
    }
}

fn row_text_utf8(row: &Row<'_>, idx: usize) -> rusqlite::Result<String> {
    match row.get_ref(idx)? {
        ValueRef::Null => Ok(String::new()),
        ValueRef::Text(bytes) | ValueRef::Blob(bytes) => {
            Ok(String::from_utf8_lossy(bytes).into_owned())
        }
        other => Err(rusqlite::Error::InvalidColumnType(
            idx,
            "text".into(),
            other.data_type(),
        )),
    }
}

fn row_text_u16(row: &Row<'_>, idx: usize) -> rusqlite::Result<Vec<u16>> {
    match row.get_ref(idx)? {
        ValueRef::Null => Ok(Vec::new()),
        ValueRef::Text(bytes) | ValueRef::Blob(bytes) => {
            let s = std::str::from_utf8(bytes).unwrap_or("");
            Ok(s.encode_utf16().collect())
        }
        other => Err(rusqlite::Error::InvalidColumnType(
            idx,
            "text".into(),
            other.data_type(),
        )),
    }
}

fn row_bytes(row: &Row<'_>, idx: usize) -> rusqlite::Result<Vec<u8>> {
    match row.get_ref(idx)? {
        ValueRef::Null => Ok(Vec::new()),
        ValueRef::Blob(bytes) | ValueRef::Text(bytes) => Ok(bytes.to_vec()),
        other => Err(rusqlite::Error::InvalidColumnType(
            idx,
            "blob".into(),
            other.data_type(),
        )),
    }
}

fn query_table_count(conn: &Connection, sql: &str) -> Result<i64, DictError> {
    let mut stmt = conn.prepare_cached(sql)?;
    let count = stmt.query_row([], |row| row.get(0))?;
    Ok(count)
}

/// Validates that all 7 required Yomitan tables exist on `conn`.
pub fn validate_schema_conn(conn: &Connection) -> Result<(), DictError> {
    let mut stmt = conn.prepare_cached("SELECT name FROM sqlite_master WHERE type='table';")?;
    let mut table_names = HashSet::new();
    let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
    for name_res in rows {
        table_names.insert(name_res?);
    }

    const REQUIRED_TABLES: [&str; 7] = [
        "directory",
        "dict_disabled",
        "tag_bank",
        "term_bank",
        "term_meta_bank",
        "kanji_bank",
        "kanji_meta_bank",
    ];

    for &tbl in &REQUIRED_TABLES {
        if !table_names.contains(tbl) {
            return Err(DictError::InvalidSchema(format!(
                "Missing required table '{tbl}'"
            )));
        }
    }
    Ok(())
}

/// Reads `PRAGMA data_version;` using a cached prepared statement.
pub fn query_data_version_conn(conn: &Connection) -> Result<i64, DictError> {
    let mut stmt = conn.prepare_cached("PRAGMA data_version;")?;
    let v = stmt.query_row([], |row| row.get(0))?;
    Ok(v)
}

/// Validates database schema, reads dictionary meta and table counts on a `rusqlite::Connection`.
pub fn init_cache_conn(conn: &Connection) -> Result<CacheInfo, DictError> {
    validate_schema_conn(conn)?;

    let mut dict_stmt = conn.prepare_cached(
        "SELECT dic_id, title, EXISTS (SELECT 1 FROM dict_disabled WHERE dict_disabled.dic_id = directory.dic_id) AS disabled FROM directory;",
    )?;
    let mut dictionaries = Vec::new();
    let dict_rows = dict_stmt.query_map([], |row| {
        Ok(DictMeta {
            id: row.get(0)?,
            title: row.get(1)?,
            disabled: row.get::<_, i64>(2)? != 0,
        })
    })?;
    for d_res in dict_rows {
        dictionaries.push(d_res?);
    }

    let table_counts = TableCounts {
        directory: query_table_count(conn, "SELECT COUNT(*) FROM directory;")?,
        tag_bank: query_table_count(conn, "SELECT COUNT(*) FROM tag_bank;")?,
        term_bank: query_table_count(conn, "SELECT COUNT(*) FROM term_bank;")?,
        term_meta_bank: query_table_count(conn, "SELECT COUNT(*) FROM term_meta_bank;")?,
        kanji_bank: query_table_count(conn, "SELECT COUNT(*) FROM kanji_bank;")?,
        kanji_meta_bank: query_table_count(conn, "SELECT COUNT(*) FROM kanji_meta_bank;")?,
    };

    Ok(CacheInfo {
        dictionaries,
        table_counts,
    })
}

/// Validates database schema, reads dictionary meta and table counts.
pub fn init_cache(db: *mut sqlite3) -> Result<CacheInfo, DictError> {
    if db.is_null() {
        return Err(DictError::NullPointer);
    }
    let conn = unsafe { Connection::from_handle(db) }?;
    init_cache_conn(&conn)
}

macro_rules! term_batch_sql {
    ($placeholders:literal) => {
        concat!(
            "SELECT rowid, dic_id, expression, reading, def_tags, term_tags, rules, score, glossary, sequence \
             FROM term_bank \
             WHERE expression IN (",
            $placeholders,
            ") AND NOT EXISTS (SELECT 1 FROM dict_disabled WHERE dict_disabled.dic_id = term_bank.dic_id) \
             UNION ALL \
             SELECT rowid, dic_id, expression, reading, def_tags, term_tags, rules, score, glossary, sequence \
             FROM term_bank \
             WHERE reading IN (",
            $placeholders,
            ") AND NOT EXISTS (SELECT 1 FROM dict_disabled WHERE dict_disabled.dic_id = term_bank.dic_id);"
        )
    };
}

macro_rules! meta_batch_sql {
    ($placeholders:literal) => {
        concat!(
            "SELECT dic_id, expression, mode, type, data \
             FROM term_meta_bank \
             WHERE expression IN (",
            $placeholders,
            ") AND NOT EXISTS (SELECT 1 FROM dict_disabled WHERE dict_disabled.dic_id = term_meta_bank.dic_id);"
        )
    };
}

const TERM_BATCH_1: &str = term_batch_sql!("?1");
const TERM_BATCH_2: &str = term_batch_sql!("?1,?2");
const TERM_BATCH_4: &str = term_batch_sql!("?1,?2,?3,?4");
const TERM_BATCH_8: &str = term_batch_sql!("?1,?2,?3,?4,?5,?6,?7,?8");
const TERM_BATCH_16: &str =
    term_batch_sql!("?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16");
const TERM_BATCH_32: &str = term_batch_sql!(
    "?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,\
     ?17,?18,?19,?20,?21,?22,?23,?24,?25,?26,?27,?28,?29,?30,?31,?32"
);
const TERM_BATCH_64: &str = term_batch_sql!(
    "?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,\
     ?17,?18,?19,?20,?21,?22,?23,?24,?25,?26,?27,?28,?29,?30,?31,?32,\
     ?33,?34,?35,?36,?37,?38,?39,?40,?41,?42,?43,?44,?45,?46,?47,?48,\
     ?49,?50,?51,?52,?53,?54,?55,?56,?57,?58,?59,?60,?61,?62,?63,?64"
);

const META_BATCH_1: &str = meta_batch_sql!("?1");
const META_BATCH_2: &str = meta_batch_sql!("?1,?2");
const META_BATCH_4: &str = meta_batch_sql!("?1,?2,?3,?4");
const META_BATCH_8: &str = meta_batch_sql!("?1,?2,?3,?4,?5,?6,?7,?8");
const META_BATCH_16: &str =
    meta_batch_sql!("?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16");
const META_BATCH_32: &str = meta_batch_sql!(
    "?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,\
     ?17,?18,?19,?20,?21,?22,?23,?24,?25,?26,?27,?28,?29,?30,?31,?32"
);
const META_BATCH_64: &str = meta_batch_sql!(
    "?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,\
     ?17,?18,?19,?20,?21,?22,?23,?24,?25,?26,?27,?28,?29,?30,?31,?32,\
     ?33,?34,?35,?36,?37,?38,?39,?40,?41,?42,?43,?44,?45,?46,?47,?48,\
     ?49,?50,?51,?52,?53,?54,?55,?56,?57,?58,?59,?60,?61,?62,?63,?64"
);

fn select_batch_bucket(len: usize) -> (usize, &'static str, &'static str) {
    if len <= 1 {
        (1, TERM_BATCH_1, META_BATCH_1)
    } else if len <= 2 {
        (2, TERM_BATCH_2, META_BATCH_2)
    } else if len <= 4 {
        (4, TERM_BATCH_4, META_BATCH_4)
    } else if len <= 8 {
        (8, TERM_BATCH_8, META_BATCH_8)
    } else if len <= 16 {
        (16, TERM_BATCH_16, META_BATCH_16)
    } else if len <= 32 {
        (32, TERM_BATCH_32, META_BATCH_32)
    } else {
        (64, TERM_BATCH_64, META_BATCH_64)
    }
}

/// Fetches all enabled `term_bank` rows (including `rules`) whose `expression` or `reading`
/// matches any key in `keys`, using bucketed `IN (...)` + `prepare_cached` indexed lookups.
pub fn query_term_details_batch_conn(
    conn: &Connection,
    keys: &[String],
) -> Result<Vec<BatchTermRow>, DictError> {
    if keys.is_empty() {
        return Ok(Vec::new());
    }
    let mut seen_rowids: HashSet<i64> = HashSet::new();
    let mut results: Vec<BatchTermRow> = Vec::new();

    for chunk in keys.chunks(64) {
        let (bucket, sql, _) = select_batch_bucket(chunk.len());
        let last = &chunk[chunk.len() - 1];
        let mut stmt = conn.prepare_cached(sql)?;
        let iter_params = (0..bucket).map(|i| chunk.get(i).unwrap_or(last).as_str());
        let mut rows = stmt.query(params_from_iter(iter_params))?;
        while let Some(row) = rows.next()? {
            let rowid: i64 = row.get(0)?;
            if !seen_rowids.insert(rowid) {
                continue;
            }
            results.push(BatchTermRow {
                rowid,
                dic_id: row.get(1)?,
                expression: row_text_utf8(row, 2)?,
                reading: row_text_utf8(row, 3)?,
                def_tags: row_text_utf8(row, 4)?,
                term_tags: row_text_utf8(row, 5)?,
                rules: row_text_utf8(row, 6)?,
                score: row.get(7)?,
                glossary: row_text_utf8(row, 8)?,
                sequence: row.get(9)?,
            });
        }
    }

    results.sort_by_key(|r| (r.dic_id, r.sequence, r.rowid));
    Ok(results)
}

/// Fetches all enabled `term_meta_bank` rows (`freq` and `pitch`) for `expressions`
/// using bucketed `IN (...)` + `prepare_cached`.
pub fn query_term_meta_batch_conn(
    conn: &Connection,
    expressions: &[String],
) -> Result<Vec<RawTermMetaRow>, DictError> {
    if expressions.is_empty() {
        return Ok(Vec::new());
    }
    let mut results: Vec<RawTermMetaRow> = Vec::new();
    for chunk in expressions.chunks(64) {
        let (bucket, _, sql) = select_batch_bucket(chunk.len());
        let last = &chunk[chunk.len() - 1];
        let mut stmt = conn.prepare_cached(sql)?;
        let iter_params = (0..bucket).map(|i| chunk.get(i).unwrap_or(last).as_str());
        let mut rows = stmt.query(params_from_iter(iter_params))?;
        while let Some(row) = rows.next()? {
            results.push(RawTermMetaRow {
                dic_id: row.get(0)?,
                expression: row_text_utf8(row, 1)?,
                mode: row_text_utf8(row, 2)?,
                blob_type: row.get(3)?,
                data: row_bytes(row, 4)?,
            });
        }
    }
    Ok(results)
}

/// Queries `term_bank.rules` for `(expression, reading)` across enabled dictionaries.
/// Returns `Ok(Some(tokens))` if at least one `term_bank` row matched `(expression, reading)`
/// (including V4 kana-only rows where `reading = ''`), or `Ok(None)` if no DB row matched.
pub fn query_term_rules_conn(
    conn: &Connection,
    expression: &str,
    reading: &str,
) -> Result<Option<Vec<String>>, DictError> {
    const SQL: &str = "SELECT rules FROM term_bank \
        WHERE expression = ?1 AND reading = ?2 \
          AND NOT EXISTS (SELECT 1 FROM dict_disabled WHERE dict_disabled.dic_id = term_bank.dic_id);";

    let mut found_row = false;
    let mut tokens: Vec<String> = Vec::new();

    {
        let mut stmt = conn.prepare_cached(SQL)?;
        let mut rows = stmt.query(params![expression, reading])?;
        while let Some(row) = rows.next()? {
            found_row = true;
            let rules_str = row_text_utf8(row, 0)?;
            for tok in rules_str.split_whitespace() {
                if !tok.is_empty() && !tokens.iter().any(|existing| existing == tok) {
                    tokens.push(tok.to_owned());
                }
            }
        }
    }

    if !found_row
        && !reading.is_empty()
        && crate::normalize::normalize_reading_str(expression)
            == crate::normalize::normalize_reading_str(reading)
    {
        let mut stmt = conn.prepare_cached(SQL)?;
        let mut rows = stmt.query(params![expression, ""])?;
        while let Some(row) = rows.next()? {
            found_row = true;
            let rules_str = row_text_utf8(row, 0)?;
            for tok in rules_str.split_whitespace() {
                if !tok.is_empty() && !tokens.iter().any(|existing| existing == tok) {
                    tokens.push(tok.to_owned());
                }
            }
        }
    }

    Ok(found_row.then_some(tokens))
}

/// Prepares and executes the `terms` SQL query on a `rusqlite::Connection`.
pub fn query_terms_conn(
    conn: &Connection,
    plan: &TermQueryPlan,
    results: &mut Vec<TermRow>,
) -> Result<(), DictError> {
    let raw_s = String::from_utf16_lossy(&plan.raw);
    let hira_s = String::from_utf16_lossy(&plan.hiragana);
    let nfkc_s = String::from_utf16_lossy(&plan.nfkc);

    match plan.query_kind {
        TermQueryKind::WithHalfwidth => {
            const SQL: &str = "SELECT DISTINCT expression, reading FROM term_bank \
                WHERE dic_id NOT IN (SELECT dic_id FROM dict_disabled) AND \
                (expression = ?1 OR reading = ?2 OR expression = ?3 OR reading = ?4 OR expression = ?5 OR reading = ?6);";
            let mut stmt = conn.prepare_cached(SQL)?;
            let rows = stmt.query_map(
                params![&raw_s, &raw_s, &hira_s, &hira_s, &nfkc_s, &nfkc_s],
                |row| {
                    Ok(TermRow {
                        expression: row_text_u16(row, 0)?,
                        reading: row_text_u16(row, 1)?,
                    })
                },
            )?;
            for row_res in rows {
                results.push(row_res?);
            }
        }
        TermQueryKind::WithKatakana => {
            const SQL: &str = "SELECT DISTINCT expression, reading FROM term_bank \
                WHERE dic_id NOT IN (SELECT dic_id FROM dict_disabled) AND \
                (expression = ?1 OR reading = ?2 OR expression = ?3 OR reading = ?4);";
            let mut stmt = conn.prepare_cached(SQL)?;
            let rows = stmt.query_map(params![&raw_s, &raw_s, &hira_s, &hira_s], |row| {
                Ok(TermRow {
                    expression: row_text_u16(row, 0)?,
                    reading: row_text_u16(row, 1)?,
                })
            })?;
            for row_res in rows {
                results.push(row_res?);
            }
        }
        TermQueryKind::Plain => {
            const SQL: &str = "SELECT DISTINCT expression, reading FROM term_bank \
                WHERE dic_id NOT IN (SELECT dic_id FROM dict_disabled) AND \
                (expression = ?1 OR reading = ?2);";
            let mut stmt = conn.prepare_cached(SQL)?;
            let rows = stmt.query_map(params![&raw_s, &raw_s], |row| {
                Ok(TermRow {
                    expression: row_text_u16(row, 0)?,
                    reading: row_text_u16(row, 1)?,
                })
            })?;
            for row_res in rows {
                results.push(row_res?);
            }
        }
    }

    Ok(())
}

/// Prepares and executes the `terms` SQL query according to `TermQueryKind`.
pub fn query_terms(
    db: *mut sqlite3,
    plan: &TermQueryPlan,
    results: &mut Vec<TermRow>,
) -> Result<(), DictError> {
    if db.is_null() {
        return Err(DictError::NullPointer);
    }
    let conn = unsafe { Connection::from_handle(db) }?;
    query_terms_conn(&conn, plan, results)
}

/// Executes UTF-8 kanji queries on `conn` via `prepare_cached`.
pub fn query_kanji_utf8_conn(
    conn: &Connection,
    kanji: &str,
) -> Result<Vec<KanjiUtf8Row>, DictError> {
    const QUERY: &str =
        "SELECT dic_id, onyomi, kunyomi, tags, meanings, stats FROM kanji_bank \
         WHERE NOT EXISTS (SELECT 1 FROM dict_disabled WHERE dict_disabled.dic_id = kanji_bank.dic_id) \
           AND char = ?1;";

    let mut stmt = conn.prepare_cached(QUERY)?;
    let mut results = Vec::new();
    let mut rows = stmt.query(params![kanji])?;
    while let Some(row) = rows.next()? {
        let meanings_blob = row_bytes(row, 4)?;
        let raw_meanings = String::from_utf8_lossy(&meanings_blob).into_owned();
        let meanings = crate::databasemanager::json_array_to_utf8_strings(&meanings_blob);
        results.push(KanjiUtf8Row {
            dic_id: row.get(0)?,
            onyomi: row_text_utf8(row, 1)?,
            kunyomi: row_text_utf8(row, 2)?,
            tags: row_text_utf8(row, 3)?,
            meanings,
            raw_meanings,
            stats: row_text_utf8(row, 5)?,
        });
    }
    Ok(results)
}

/// Queries `kanji_meta_bank` (`mode = 'freq'`) in UTF-8 via `prepare_cached`.
pub fn query_kanji_meta_freq_utf8_conn(
    conn: &Connection,
    kanji: &str,
) -> Result<Vec<RawTermMetaRow>, DictError> {
    const QUERY: &str =
        "SELECT dic_id, expression, mode, type, data FROM kanji_meta_bank \
         WHERE NOT EXISTS (SELECT 1 FROM dict_disabled WHERE dict_disabled.dic_id = kanji_meta_bank.dic_id) \
           AND expression = ?1 AND mode = 'freq';";

    let mut stmt = conn.prepare_cached(QUERY)?;
    let mut results = Vec::new();
    let mut rows = stmt.query(params![kanji])?;
    while let Some(row) = rows.next()? {
        results.push(RawTermMetaRow {
            dic_id: row.get(0)?,
            expression: row_text_utf8(row, 1)?,
            mode: row_text_utf8(row, 2)?,
            blob_type: row.get(3)?,
            data: row_bytes(row, 4)?,
        });
    }
    Ok(results)
}

/// Executes kanji queries on a `rusqlite::Connection`.
pub fn query_kanji_conn(
    conn: &Connection,
    kanji: &[u16],
    results: &mut Vec<KanjiRow>,
) -> Result<(), DictError> {
    const QUERY: &str =
        "SELECT dic_id, onyomi, kunyomi, tags, meanings, stats FROM kanji_bank \
         WHERE dic_id NOT IN (SELECT dic_id FROM dict_disabled) AND (char = ?1);";

    let kanji_s = String::from_utf16_lossy(kanji);
    let mut stmt = conn.prepare_cached(QUERY)?;
    let rows = stmt.query_map(params![&kanji_s], |row| {
        let dic_id = row.get(0)?;
        let onyomi = row_text_u16(row, 1)?;
        let kunyomi = row_text_u16(row, 2)?;
        let tags = row_text_u16(row, 3)?;
        let meanings_blob = row_bytes(row, 4)?;
        let meanings = crate::databasemanager::json_array_to_strings(&meanings_blob);
        let stats = row_text_u16(row, 5)?;
        Ok(KanjiRow {
            dic_id,
            onyomi,
            kunyomi,
            tags,
            meanings,
            stats,
        })
    })?;

    for row_res in rows {
        results.push(row_res?);
    }
    Ok(())
}

/// Executes kanji queries and groups definitions.
pub fn query_kanji(
    db: *mut sqlite3,
    kanji: &[u16],
    results: &mut Vec<KanjiRow>,
) -> Result<(), DictError> {
    if db.is_null() {
        return Err(DictError::NullPointer);
    }
    let conn = unsafe { Connection::from_handle(db) }?;
    query_kanji_conn(&conn, kanji, results)
}

/// Queries frequency tables and parses frequency objects on a `rusqlite::Connection`.
pub fn get_frequencies_conn(
    conn: &Connection,
    term: &[u16],
    reading: &[u16],
    results: &mut Vec<FreqRow>,
) -> Result<(), DictError> {
    get_frequencies_with_sql_conn(conn, None, term, reading, results)
}

/// Queries frequency tables and parses frequency objects.
pub fn get_frequencies(
    db: *mut sqlite3,
    term: &[u16],
    reading: &[u16],
    results: &mut Vec<FreqRow>,
) -> Result<(), DictError> {
    get_frequencies_with_sql(db, None, term, reading, results)
}

/// Queries `kanji_meta_bank` frequency rows on a `rusqlite::Connection`.
pub fn get_kanji_frequencies_conn(
    conn: &Connection,
    kanji: &[u16],
    results: &mut Vec<FreqRow>,
) -> Result<(), DictError> {
    const KANJI_FREQ_SQL: &str = "SELECT dic_id, data, type FROM kanji_meta_bank \
         WHERE dic_id NOT IN (SELECT dic_id FROM dict_disabled) AND \
         (expression = ?1 AND mode = 'freq');";
    execute_freq_query(conn, KANJI_FREQ_SQL, kanji, &[], results)
}

/// Queries `kanji_meta_bank` frequency rows on a raw SQLite handle.
pub fn get_kanji_frequencies(
    db: *mut sqlite3,
    kanji: &[u16],
    results: &mut Vec<FreqRow>,
) -> Result<(), DictError> {
    if db.is_null() {
        return Err(DictError::NullPointer);
    }
    let conn = unsafe { Connection::from_handle(db) }?;
    get_kanji_frequencies_conn(&conn, kanji, results)
}

/// Queries exactly the requested metadata table on a `rusqlite::Connection`.
pub fn get_frequencies_with_sql_conn(
    conn: &Connection,
    custom_sql: Option<&str>,
    term: &[u16],
    reading: &[u16],
    results: &mut Vec<FreqRow>,
) -> Result<(), DictError> {
    const DEFAULT_QUERY: &str = "SELECT dic_id, data, type FROM term_meta_bank \
         WHERE dic_id NOT IN (SELECT dic_id FROM dict_disabled) AND \
         (expression = ?1 AND mode = 'freq');";

    let sql = custom_sql.unwrap_or(DEFAULT_QUERY);
    execute_freq_query(conn, sql, term, reading, results)
}

/// Queries exactly the requested metadata table. Term metadata must not be
/// replaced with kanji metadata just because a term has an empty reading.
pub fn get_frequencies_with_sql(
    db: *mut sqlite3,
    custom_sql: Option<&str>,
    term: &[u16],
    reading: &[u16],
    results: &mut Vec<FreqRow>,
) -> Result<(), DictError> {
    if db.is_null() {
        return Err(DictError::NullPointer);
    }
    let conn = unsafe { Connection::from_handle(db) }?;
    get_frequencies_with_sql_conn(&conn, custom_sql, term, reading, results)
}

fn parse_numeric_from_freq_text(text: &str) -> Option<f64> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    match trimmed {
        "★" => return Some(20.0),
        "★★" => return Some(40.0),
        "★★★" => return Some(60.0),
        "★★★★" => return Some(80.0),
        "★★★★★" => return Some(100.0),
        _ => {}
    }
    let bytes = trimmed.as_bytes();
    let mut i = 0usize;
    if i < bytes.len() && (bytes[i] == b'+' || bytes[i] == b'-') {
        i += 1;
    }
    let digits_start = i;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
    }
    if i < bytes.len() && bytes[i] == b'.' {
        i += 1;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
    }
    if i == digits_start || (i == digits_start + 1 && bytes[digits_start] == b'.') {
        return None;
    }
    if i < bytes.len() && (bytes[i] == b'e' || bytes[i] == b'E') {
        let exp_pos = i;
        i += 1;
        if i < bytes.len() && (bytes[i] == b'+' || bytes[i] == b'-') {
            i += 1;
        }
        let exp_digits = i;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
        if i == exp_digits {
            i = exp_pos;
        }
    }
    trimmed[..i]
        .parse::<f64>()
        .ok()
        .filter(|v| v.is_finite())
}

/// Parses a `term_meta_bank` / `kanji_meta_bank` `freq` blob in UTF-8 with strict homograph
/// reading matching and exact numeric extraction.
#[must_use]
pub fn parse_freq_blob_utf8(
    blob_type: i32,
    blob: &[u8],
    expression: &str,
    caller_reading: &str,
) -> ParsedFreqUtf8 {
    match blob_type {
        1 => {
            if blob.len() >= 8 {
                let mut arr = [0u8; 8];
                arr.copy_from_slice(&blob[..8]);
                let val = i64::from_ne_bytes(arr);
                if val >= 0 {
                    return ParsedFreqUtf8 {
                        matched: true,
                        exact_reading_match: false,
                        has_reading_key: false,
                        display: val.to_string(),
                        numeric: (val > 0).then_some(val as f64),
                    };
                }
            }
            ParsedFreqUtf8::default()
        }
        2 => {
            let null_pos = blob.iter().position(|&b| b == 0).unwrap_or(blob.len());
            let Ok(s) = std::str::from_utf8(&blob[..null_pos]) else {
                return ParsedFreqUtf8::default();
            };
            ParsedFreqUtf8 {
                matched: true,
                exact_reading_match: false,
                has_reading_key: false,
                display: s.to_owned(),
                numeric: parse_numeric_from_freq_text(s).filter(|&v| v > 0.0),
            }
        }
        3 => {
            if blob.len() >= 8 {
                let mut arr = [0u8; 8];
                arr.copy_from_slice(&blob[..8]);
                let val = f64::from_ne_bytes(arr);
                if val.is_finite() && val >= 0.0 {
                    return ParsedFreqUtf8 {
                        matched: true,
                        exact_reading_match: false,
                        has_reading_key: false,
                        display: format_g(val, 6),
                        numeric: (val > 0.0).then_some(val),
                    };
                }
            }
            ParsedFreqUtf8::default()
        }
        4 => {
            let null_pos = blob.iter().position(|&b| b == 0).unwrap_or(blob.len());
            let slice = &blob[..null_pos];
            let Ok(serde_json::Value::Object(obj)) = serde_json::from_slice(slice) else {
                return ParsedFreqUtf8::default();
            };

            let meta_reading = obj.get("reading").and_then(|v| v.as_str());
            let has_reading_key = meta_reading.is_some();
            let (matched, exact_reading_match) =
                crate::databasemanager::metadata_reading_matches(
                    expression,
                    caller_reading,
                    meta_reading,
                );
            if !matched {
                return ParsedFreqUtf8 {
                    has_reading_key,
                    ..ParsedFreqUtf8::default()
                };
            }

            let mut display = String::new();
            let mut numeric: Option<f64> = None;

            if let Some(freq_val) = obj.get("frequency") {
                if let Some(n) = freq_val.as_f64() {
                    display = format_g(n, 6);
                    if n.is_finite() && n > 0.0 {
                        numeric = Some(n);
                    }
                } else if let Some(s) = freq_val.as_str() {
                    display = s.to_owned();
                    numeric = parse_numeric_from_freq_text(s).filter(|&v| v > 0.0);
                } else if let Some(nested) = freq_val.as_object() {
                    let nested_disp = nested.get("displayValue").and_then(|v| v.as_str());
                    let nested_num = nested.get("value").and_then(|v| v.as_f64());
                    if let Some(d) = nested_disp {
                        display = d.to_owned();
                    } else if let Some(n) = nested_num {
                        display = format_g(n, 6);
                    } else {
                        return ParsedFreqUtf8 {
                            has_reading_key,
                            ..ParsedFreqUtf8::default()
                        };
                    }
                    numeric = nested_num
                        .filter(|v| v.is_finite() && *v > 0.0)
                        .or_else(|| parse_numeric_from_freq_text(&display).filter(|&v| v > 0.0));
                }
            } else {
                let top_disp = obj.get("displayValue").and_then(|v| v.as_str());
                let top_num = obj.get("value").and_then(|v| v.as_f64());
                if let Some(d) = top_disp {
                    display = d.to_owned();
                } else if let Some(n) = top_num {
                    display = format_g(n, 6);
                }
                numeric = top_num
                    .filter(|v| v.is_finite() && *v > 0.0)
                    .or_else(|| parse_numeric_from_freq_text(&display).filter(|&v| v > 0.0));
            }

            ParsedFreqUtf8 {
                matched: true,
                exact_reading_match,
                has_reading_key,
                display,
                numeric,
            }
        }
        _ => ParsedFreqUtf8::default(),
    }
}

fn execute_freq_query(
    conn: &Connection,
    sql: &str,
    term: &[u16],
    reading: &[u16],
    results: &mut Vec<FreqRow>,
) -> Result<(), DictError> {
    let term_s = String::from_utf16_lossy(term);
    let reading_s = String::from_utf16_lossy(reading);
    let mut stmt = conn.prepare_cached(sql)?;
    let mut rows = stmt.query(params![&term_s])?;
    while let Some(row) = rows.next()? {
        let dic_id: i64 = row.get(0)?;
        let blob = row_bytes(row, 1)?;
        let blob_type: i32 = row.get(2)?;
        let parsed = parse_freq_blob_utf8(blob_type, &blob, &term_s, &reading_s);
        if parsed.matched {
            results.push(FreqRow {
                dic_id,
                frequency: parsed.display.encode_utf16().collect(),
            });
        }
    }
    Ok(())
}

/// Formats a float with `%g` precision 6 (C locale), matching `QString::number(double)`.
pub fn format_g(value: f64, precision: u32) -> String {
    if value.is_nan() {
        return "nan".to_string();
    }
    if value.is_infinite() {
        return if value < 0.0 { "-inf" } else { "inf" }.to_string();
    }
    let precision = if precision == 0 { 1 } else { precision };
    if value == 0.0 {
        return if value.is_sign_negative() { "-0" } else { "0" }.to_string();
    }
    let precise = format!("{:.*e}", (precision - 1) as usize, value);
    let pivot = precise.find('e').expect("Rust e-format always has 'e'");
    let mantissa = &precise[..pivot];
    let exponent: i32 = precise[pivot + 1..].parse().expect("valid exponent");
    if exponent < -4 || exponent >= precision as i32 {
        let (whole, fraction) = mantissa.split_once('.').unwrap_or((mantissa, ""));
        let fraction = fraction.trim_end_matches('0');
        let mantissa = if fraction.is_empty() {
            whole.to_string()
        } else {
            format!("{whole}.{fraction}")
        };
        let sign = if exponent < 0 { '-' } else { '+' };
        format!("{mantissa}e{sign}{:02}", exponent.abs())
    } else {
        let decimals = (precision as i32 - 1 - exponent).max(0) as usize;
        let mut formatted = format!("{:.*}", decimals, value);
        if formatted.contains('.') {
            while formatted.ends_with('0') {
                formatted.pop();
            }
            if formatted.ends_with('.') {
                formatted.pop();
            }
        }
        formatted
    }
}

#[cfg(test)]
fn format_json_number(val: &serde_json::Value) -> String {
    val.as_f64().map_or_else(String::new, |value| format_g(value, 6))
}

/// Queries pitch tables and parses pitch JSON blobs on a `rusqlite::Connection`.
pub fn add_pitches_conn(
    conn: &Connection,
    term: &[u16],
    reading: &[u16],
    results: &mut Vec<PitchRow>,
) -> Result<(), DictError> {
    const QUERY: &str =
        "SELECT dic_id, data FROM term_meta_bank \
         WHERE dic_id NOT IN (SELECT dic_id FROM dict_disabled) AND \
         (expression = ?1 AND mode = 'pitch');";

    let term_s = String::from_utf16_lossy(term);
    let mut stmt = conn.prepare_cached(QUERY)?;
    let mut rows = stmt.query(params![&term_s])?;
    while let Some(row) = rows.next()? {
        let dic_id: i64 = row.get(0)?;
        let blob = row_bytes(row, 1)?;
        let null_pos = blob.iter().position(|&b| b == 0).unwrap_or(blob.len());
        let blob_slice = &blob[..null_pos];

        let parsed = crate::databasemanager::parse_pitch_blob(blob_slice, term, reading);
        if parsed.matched {
            results.push(PitchRow {
                dic_id,
                mora: parsed.mora,
                positions: parsed.positions,
                nasal: parsed.nasal,
                devoice: parsed.devoice,
                tags: parsed.tags,
                entries: parsed.entries,
            });
        }
    }
    Ok(())
}

/// Queries pitch tables and parses pitch JSON blobs.
pub fn add_pitches(
    db: *mut sqlite3,
    term: &[u16],
    reading: &[u16],
    results: &mut Vec<PitchRow>,
) -> Result<(), DictError> {
    if db.is_null() {
        return Err(DictError::NullPointer);
    }
    let conn = unsafe { Connection::from_handle(db) }?;
    add_pitches_conn(&conn, term, reading, results)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        const SCHEMA: &str = "
            CREATE TABLE directory (
                dic_id INTEGER PRIMARY KEY, title TEXT NOT NULL UNIQUE,
                format INTEGER NOT NULL, revision TEXT NOT NULL, sequenced INTEGER NOT NULL
            );
            CREATE TABLE dict_disabled (dic_id INTEGER PRIMARY KEY);
            CREATE TABLE tag_bank (
                dic_id INTEGER NOT NULL, name TEXT NOT NULL, category TEXT NOT NULL,
                ord INTEGER NOT NULL, notes TEXT NOT NULL, score INTEGER NOT NULL,
                PRIMARY KEY(dic_id, name)
            );
            CREATE TABLE term_bank (
                dic_id INTEGER NOT NULL, expression TEXT NOT NULL, reading TEXT NOT NULL,
                def_tags TEXT NOT NULL, rules TEXT NOT NULL, score INTEGER NOT NULL,
                glossary TEXT NOT NULL, sequence INTEGER NOT NULL, term_tags TEXT NOT NULL
            );
            CREATE TABLE term_meta_bank (
                dic_id INTEGER NOT NULL, expression TEXT NOT NULL, mode TEXT NOT NULL,
                type INTEGER NOT NULL, data BLOB
            );
            CREATE TABLE kanji_bank (
                dic_id INTEGER NOT NULL, char TEXT NOT NULL, onyomi TEXT NOT NULL,
                kunyomi TEXT NOT NULL, tags TEXT NOT NULL, meanings TEXT NOT NULL, stats TEXT NOT NULL
            );
            CREATE TABLE kanji_meta_bank (
                dic_id INTEGER NOT NULL, expression TEXT NOT NULL, mode TEXT NOT NULL,
                type INTEGER NOT NULL, data BLOB
            );
        ";
        conn.execute_batch(SCHEMA).unwrap();
        conn
    }

    fn to_u16(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    #[test]
    fn test_init_cache_empty() {
        let db = create_test_db();
        let cache = init_cache_conn(&db).unwrap();
        assert_eq!(cache.dictionaries.len(), 0);
        assert_eq!(cache.table_counts.directory, 0);
        assert_eq!(cache.table_counts.term_bank, 0);
    }

    #[test]
    fn test_init_cache_with_dict() {
        let db = create_test_db();
        db.execute_batch(
            "
            INSERT INTO directory VALUES (1, 'JMdict', 3, 'rev1', 0);
            INSERT INTO tag_bank VALUES (1, 'v1', 'partOfSpeech', 1, 'verb', 0);
            INSERT INTO term_bank VALUES (1, '食べる', 'たべる', '', 'v1', 10, '[\"to eat\"]', 1, '');
            ",
        )
        .unwrap();

        let cache = init_cache_conn(&db).unwrap();
        assert_eq!(cache.dictionaries.len(), 1);
        assert_eq!(cache.dictionaries[0].id, 1);
        assert_eq!(cache.dictionaries[0].title, "JMdict");
        assert!(!cache.dictionaries[0].disabled);
        assert_eq!(cache.table_counts.directory, 1);
        assert_eq!(cache.table_counts.tag_bank, 1);
        assert_eq!(cache.table_counts.term_bank, 1);
    }

    #[test]
    fn test_query_terms_scenarios_and_batch_rules() {
        let db = create_test_db();
        db.execute_batch(
            "
            INSERT INTO directory VALUES (1, 'TestDict', 3, 'rev1', 0);
            INSERT INTO term_bank VALUES (1, '食べる', 'たべる', '', 'v1', 10, '[\"to eat\"]', 1, '');
            INSERT INTO term_bank VALUES (1, 'カタカナ', 'かたかな', '', '', 5, '[\"katakana\"]', 2, '');
            ",
        )
        .unwrap();

        let raw = to_u16("食べる");
        let plan = TermQueryPlan::new(&raw, &raw);
        let mut results = Vec::new();
        query_terms_conn(&db, &plan, &mut results).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].expression, to_u16("食べる"));
        assert_eq!(results[0].reading, to_u16("たべる"));

        let raw_kata = to_u16("かたかな");
        let plan_kata = TermQueryPlan::new(&raw_kata, &raw_kata);
        let mut results_kata = Vec::new();
        query_terms_conn(&db, &plan_kata, &mut results_kata).unwrap();
        assert_eq!(results_kata.len(), 1);
        assert_eq!(results_kata[0].expression, to_u16("カタカナ"));

        let raw_half = to_u16("ｶﾀｶﾅ");
        let nfkc_half = to_u16("カタカナ");
        let plan_half = TermQueryPlan::new(&raw_half, &nfkc_half);
        let mut results_half = Vec::new();
        query_terms_conn(&db, &plan_half, &mut results_half).unwrap();
        assert_eq!(results_half.len(), 1);
        assert_eq!(results_half[0].expression, to_u16("カタカナ"));

        let batch = query_term_details_batch_conn(
            &db,
            &["たべる".to_owned(), "カタカナ".to_owned()],
        )
        .unwrap();
        assert_eq!(batch.len(), 2);
        assert_eq!(batch[0].rules, "v1");
        assert_eq!(batch[1].rules, "");
    }

    #[test]
    fn term_frequencies_never_fall_back_to_kanji_metadata() {
        let db = create_test_db();
        db.execute(
            "INSERT INTO kanji_meta_bank (dic_id, expression, mode, type, data) \
             VALUES (1, '字', 'freq', 2, 'kanji rank');",
            [],
        )
        .unwrap();
        let mut rows = Vec::new();
        get_frequencies_conn(&db, &to_u16("字"), &[], &mut rows).unwrap();
        assert!(rows.is_empty(), "the C++ term query reads only term_meta_bank");

        // Explicit kanji frequency query reads kanji_meta_bank:
        get_kanji_frequencies_conn(&db, &to_u16("字"), &mut rows).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(String::from_utf16_lossy(&rows[0].frequency), "kanji rank");
    }

    #[test]
    fn json_frequency_numbers_use_qjsonvalue_to_double_and_qstring_number() {
        let db = create_test_db();
        db.execute_batch(
            "INSERT INTO term_meta_bank (dic_id, expression, mode, type, data) \
             VALUES (1, '語', 'freq', 4, '{\"frequency\":1234567}'), \
                    (2, '語', 'freq', 4, '{\"frequency\":{\"value\":123456789}}'), \
                    (3, '語', 'freq', 4, '{\"value\":1234567}');",
        )
        .unwrap();
        let mut rows = Vec::new();
        get_frequencies_conn(&db, &to_u16("語"), &[], &mut rows).unwrap();
        assert_eq!(
            rows.iter()
                .map(|row| String::from_utf16(&row.frequency).unwrap())
                .collect::<Vec<_>>(),
            ["1.23457e+06", "1.23457e+08", "1.23457e+06"]
        );
    }

    #[test]
    fn test_query_kanji() {
        let db = create_test_db();
        db.execute_batch(
            "
            INSERT INTO directory VALUES (1, 'KanjiDict', 3, 'rev1', 0);
            INSERT INTO kanji_bank VALUES (1, '食', 'ショク', 'た.べる', 'grade:2', '[\"eat\", \"food\"]', '{\"strokes\": \"9\"}');
            ",
        )
        .unwrap();

        let mut results = Vec::new();
        query_kanji_conn(&db, &to_u16("食"), &mut results).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].dic_id, 1);
        assert_eq!(results[0].onyomi, to_u16("ショク"));
        assert_eq!(results[0].kunyomi, to_u16("た.べる"));
        assert_eq!(results[0].tags, to_u16("grade:2"));
        assert_eq!(results[0].meanings, vec![to_u16("eat"), to_u16("food")]);
        assert_eq!(results[0].stats, to_u16("{\"strokes\": \"9\"}"));
    }

    #[test]
    fn test_get_frequencies_and_pitches_with_nasal_devoice_and_homograph_guard() {
        let db = create_test_db();
        db.execute_batch(
            "
            INSERT INTO directory VALUES (1, 'JMdict', 3, 'rev1', 0);
            INSERT INTO term_meta_bank VALUES (1, '食べる', 'freq', 4, '{\"reading\": \"たべる\", \"frequency\": 120}');
            INSERT INTO term_meta_bank VALUES (1, '食べる', 'pitch', 4, '{\"reading\": \"たべる\", \"pitches\": [{\"position\": 2, \"nasal\": [2], \"devoice\": [1], \"tags\": [\"v1\"]}]}');
            INSERT INTO term_meta_bank VALUES (1, '人気', 'freq', 4, '{\"reading\": \"にんき\", \"frequency\": 300}');
            ",
        )
        .unwrap();

        let mut freqs = Vec::new();
        get_frequencies_conn(&db, &to_u16("食べる"), &to_u16("たべる"), &mut freqs).unwrap();
        assert_eq!(freqs.len(), 1);
        assert_eq!(freqs[0].dic_id, 1);
        assert_eq!(freqs[0].frequency, to_u16("120"));

        let mut homograph_freqs = Vec::new();
        get_frequencies_conn(&db, &to_u16("人気"), &to_u16("ひとけ"), &mut homograph_freqs).unwrap();
        assert!(homograph_freqs.is_empty());
        get_frequencies_conn(&db, &to_u16("人気"), &[], &mut homograph_freqs).unwrap();
        assert!(homograph_freqs.is_empty());

        let mut pitches = Vec::new();
        add_pitches_conn(&db, &to_u16("食べる"), &to_u16("たべる"), &mut pitches).unwrap();
        assert_eq!(pitches.len(), 1);
        assert_eq!(pitches[0].dic_id, 1);
        assert_eq!(pitches[0].mora, vec![to_u16("た"), to_u16("べ"), to_u16("る")]);
        assert_eq!(pitches[0].positions, vec![2]);
        assert_eq!(pitches[0].nasal, vec![2]);
        assert_eq!(pitches[0].devoice, vec![1]);
        assert_eq!(pitches[0].tags, vec!["v1".to_owned()]);
    }

    #[test]
    fn test_statement_reuse_with_static_bindings() {
        let db = create_test_db();
        db.execute_batch(
            "INSERT INTO directory VALUES (1, 'JMdict', 3, 'rev1', 0);
             INSERT INTO term_bank VALUES (1, '食べる', 'たべる', '', '', 10, '[\"to eat\"]', 1, '');
             INSERT INTO term_bank VALUES (1, '飲む', 'のむ', '', '', 10, '[\"to drink\"]', 2, '');",
        )
        .unwrap();

        let raw = unsafe { db.handle() };
        let mut stmt =
            Statement::prepare(raw, "SELECT reading FROM term_bank WHERE expression = ?1;")
                .unwrap();
        stmt.bind_text(1, "食べる").unwrap();
        assert!(stmt.step().unwrap());
        assert_eq!(stmt.column_text_string(0), "たべる");
        assert!(!stmt.step().unwrap());

        stmt.reset().unwrap();
        stmt.bind_text(1, "飲む").unwrap();
        assert!(stmt.step().unwrap());
        assert_eq!(stmt.column_text_string(0), "のむ");
    }

    #[test]
    fn test_format_json_number_and_format_g() {
        assert_eq!(format_g(1234.5, 6), "1234.5");
        assert_eq!(format_g(1234.5678, 6), "1234.57");
        assert_eq!(format_g(100000.0, 6), "100000");
        assert_eq!(format_g(0.00001, 6), "1e-05");
        assert_eq!(format_g(1000000.0, 6), "1e+06");
        assert_eq!(format_g(0.0, 6), "0");

        let val_int = serde_json::json!(42);
        assert_eq!(format_json_number(&val_int), "42");

        let val_float_int = serde_json::json!(42.0);
        assert_eq!(format_json_number(&val_float_int), "42");

        let val_float = serde_json::json!(1234.5678);
        assert_eq!(format_json_number(&val_float), "1234.57");
    }

    #[test]
    fn freq_blob_supports_signed_int_and_float_tags() {
        let db = create_test_db();
        db.execute(
            "INSERT INTO term_meta_bank (dic_id, expression, mode, type, data) VALUES (1, '語', 'freq', 1, ?1);",
            params![420i64.to_ne_bytes().to_vec()],
        )
        .unwrap();
        db.execute(
            "INSERT INTO term_meta_bank (dic_id, expression, mode, type, data) VALUES (2, '語', 'freq', 1, ?1);",
            params![(-5i64).to_ne_bytes().to_vec()],
        )
        .unwrap();
        db.execute(
            "INSERT INTO term_meta_bank (dic_id, expression, mode, type, data) VALUES (3, '語', 'freq', 3, ?1);",
            params![123.45f64.to_ne_bytes().to_vec()],
        )
        .unwrap();

        let mut rows = Vec::new();
        get_frequencies_conn(&db, &to_u16("語"), &[], &mut rows).unwrap();
        let formatted: Vec<(i64, String)> = rows
            .into_iter()
            .map(|r| (r.dic_id, String::from_utf16(&r.frequency).unwrap()))
            .collect();
        assert_eq!(
            formatted,
            vec![(1, "420".to_string()), (3, "123.45".to_string())]
        );
    }
}

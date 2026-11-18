//! Pure-Rust safe Yomichan/Yomitan ZIP importer.
//!
//! Preserves Memento's SQLite schema and binary metadata BLOB layouts while
//! eliminating C `libzip`, `libjson-c`, `nftw` deletion, and raw C-ABI FFI.

use std::ffi::OsStr;
use std::fmt;
use std::fs::{self, File};
use std::io::{BufReader, ErrorKind, Read, Seek, Write};
use std::path::{Component, Path, PathBuf};

use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde_json::Value;
use zip::ZipArchive;

use crate::sqlite_db;

/// Maximum uncompressed size allowed for a single ZIP entry (128 MiB).
pub const MAX_ENTRY_BYTES: u64 = 128 * 1024 * 1024;
/// Maximum cumulative uncompressed bytes allowed across an entire dictionary ZIP (2 GiB).
pub const MAX_TOTAL_BYTES: u64 = 2 * 1024 * 1024 * 1024;

#[derive(Clone, Copy)]
struct SizeLimits {
    max_entry_bytes: u64,
    max_total_bytes: u64,
}

impl Default for SizeLimits {
    fn default() -> Self {
        Self {
            max_entry_bytes: MAX_ENTRY_BYTES,
            max_total_bytes: MAX_TOTAL_BYTES,
        }
    }
}

const CURRENT_SCHEMA_VERSION: i32 = 4;

const CREATE_TABLES_SQL: &str = "
CREATE TABLE directory (
    dic_id INTEGER PRIMARY KEY, title TEXT NOT NULL UNIQUE,
    format INTEGER NOT NULL, revision TEXT NOT NULL, sequenced INTEGER NOT NULL
);
CREATE TRIGGER directory_remove AFTER DELETE ON directory BEGIN
    DELETE FROM dict_disabled WHERE dic_id = old.dic_id;
    DELETE FROM tag_bank WHERE dic_id = old.dic_id;
    DELETE FROM term_bank WHERE dic_id = old.dic_id;
    DELETE FROM term_meta_bank WHERE dic_id = old.dic_id;
    DELETE FROM kanji_bank WHERE dic_id = old.dic_id;
    DELETE FROM kanji_meta_bank WHERE dic_id = old.dic_id;
END;
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

const CREATE_INDEXES_SQL: &str = "
CREATE INDEX IF NOT EXISTS idx_term_bank_exp ON term_bank(expression);
CREATE INDEX IF NOT EXISTS idx_term_bank_reading ON term_bank(reading);
CREATE INDEX IF NOT EXISTS idx_term_bank_combo ON term_bank(expression, reading);
CREATE INDEX IF NOT EXISTS idx_term_meta_exp ON term_meta_bank(expression, mode);
CREATE INDEX IF NOT EXISTS idx_kanji_bank_char ON kanji_bank(char);
CREATE INDEX IF NOT EXISTS idx_kanji_meta_exp ON kanji_meta_bank(expression, mode);
";

const DROP_INDEXES_SQL: &str = "
DROP INDEX IF EXISTS idx_tag_bank_name;
DROP INDEX IF EXISTS idx_term_bank_exp;
DROP INDEX IF EXISTS idx_term_bank_reading;
DROP INDEX IF EXISTS idx_term_bank_combo;
DROP INDEX IF EXISTS idx_term_meta_exp;
DROP INDEX IF EXISTS idx_kanji_bank_char;
DROP INDEX IF EXISTS idx_kanji_meta_exp;
";

const V1_TO_V2: &str = "
ALTER TABLE term_meta_bank ADD type INTEGER NOT NULL DEFAULT 0;
ALTER TABLE kanji_meta_bank ADD type INTEGER NOT NULL DEFAULT 0;
UPDATE term_meta_bank SET type = 4 WHERE mode = 'pitch';
UPDATE kanji_meta_bank SET type = 4 WHERE mode = 'pitch';
UPDATE term_meta_bank SET type = 1 WHERE mode = 'freq';
UPDATE kanji_meta_bank SET type = 1 WHERE mode = 'freq';
PRAGMA user_version = 2;
";

const V2_TO_V3: &str = "
CREATE TABLE dict_disabled (dic_id INTEGER PRIMARY KEY);
DROP TRIGGER directory_remove;
CREATE TRIGGER directory_remove AFTER DELETE ON directory BEGIN
    DELETE FROM tag_bank WHERE dic_id = old.dic_id;
    DELETE FROM term_bank WHERE dic_id = old.dic_id;
    DELETE FROM term_meta_bank WHERE dic_id = old.dic_id;
    DELETE FROM kanji_bank WHERE dic_id = old.dic_id;
    DELETE FROM kanji_meta_bank WHERE dic_id = old.dic_id;
    DELETE FROM dict_disabled WHERE dic_id = old.dic_id;
END;
PRAGMA user_version = 3;
";

const V3_TO_V4: &str = "
UPDATE term_bank SET reading = '' WHERE expression = reading;
PRAGMA user_version = 4;
";

/// Errors produced by Yomichan/Yomitan dictionary validation, import, and removal.
#[derive(Debug)]
pub enum YomiError {
    InvalidPath { path: PathBuf, reason: &'static str },
    InvalidDictionaryId(i64),
    DictionaryNotFound(i64),
    Io {
        operation: &'static str,
        path: PathBuf,
        source: std::io::Error,
    },
    Zip(zip::result::ZipError),
    Json {
        entry: String,
        source: serde_json::Error,
    },
    Sqlite(rusqlite::Error),
    NewerDbVersion(i32),
    UnsupportedFormat(i64),
    MalformedIndex(String),
    MalformedBank {
        bank: &'static str,
        detail: String,
    },
    UnsafeArchiveEntry {
        entry: String,
        reason: &'static str,
    },
    EntryTooLarge {
        entry: String,
        limit: u64,
    },
    ArchiveTooLarge {
        limit: u64,
    },
}

impl fmt::Display for YomiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPath { path, reason } => {
                write!(f, "invalid path '{}': {reason}", path.display())
            }
            Self::InvalidDictionaryId(id) => write!(f, "invalid dictionary ID {id}"),
            Self::DictionaryNotFound(id) => write!(f, "dictionary {id} is not installed"),
            Self::Io {
                operation,
                path,
                source,
            } => write!(f, "failed to {operation} '{}': {source}", path.display()),
            Self::Zip(err) => write!(f, "ZIP archive error: {err}"),
            Self::Json { entry, source } => write!(f, "invalid JSON in '{entry}': {source}"),
            Self::Sqlite(err) => write!(f, "SQLite error: {err}"),
            Self::NewerDbVersion(v) => {
                write!(f, "dictionary database schema version {v} is newer than supported ({CURRENT_SCHEMA_VERSION})")
            }
            Self::UnsupportedFormat(fmt_ver) => {
                write!(f, "unsupported Yomitan dictionary format {fmt_ver} (expected 3)")
            }
            Self::MalformedIndex(msg) => write!(f, "malformed index.json: {msg}"),
            Self::MalformedBank { bank, detail } => write!(f, "malformed {bank} entry: {detail}"),
            Self::UnsafeArchiveEntry { entry, reason } => {
                write!(f, "unsafe archive entry '{entry}': {reason}")
            }
            Self::EntryTooLarge { entry, limit } => {
                write!(f, "archive entry '{entry}' exceeds decompression limit of {limit} bytes")
            }
            Self::ArchiveTooLarge { limit } => {
                write!(f, "archive total uncompressed size exceeds limit of {limit} bytes")
            }
        }
    }
}

impl std::error::Error for YomiError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Zip(err) => Some(err),
            Self::Json { source, .. } => Some(source),
            Self::Sqlite(err) => Some(err),
            _ => None,
        }
    }
}

impl From<rusqlite::Error> for YomiError {
    fn from(err: rusqlite::Error) -> Self {
        Self::Sqlite(err)
    }
}

impl From<zip::result::ZipError> for YomiError {
    fn from(err: zip::result::ZipError) -> Self {
        Self::Zip(err)
    }
}

/// Returns the safe resource directory path for `dic_id`: `{res_dir}/dict-{dic_id}`.
///
/// Resource paths are keyed strictly by integer database ID rather than
/// untrusted `index.json` `"title"` strings to prevent path traversal and
/// arbitrary directory deletion.
pub fn dictionary_resource_dir(res_dir: &Path, dic_id: i64) -> Result<PathBuf, YomiError> {
    if dic_id <= 0 {
        return Err(YomiError::InvalidDictionaryId(dic_id));
    }
    validate_non_empty_path(res_dir)?;
    Ok(res_dir.join(format!("dict-{dic_id}")))
}

fn validate_non_empty_path(path: &Path) -> Result<(), YomiError> {
    if path.as_os_str().is_empty() {
        return Err(YomiError::InvalidPath {
            path: path.to_path_buf(),
            reason: "path is empty",
        });
    }
    Ok(())
}

fn open_writable_db(db_file: &Path) -> Result<Connection, YomiError> {
    validate_non_empty_path(db_file)?;
    sqlite_db::open_read_write_connection(db_file).map_err(|err| match err {
        sqlite_db::DictError::Other(msg) => YomiError::InvalidPath {
            path: db_file.to_path_buf(),
            reason: if msg.contains("empty") {
                "path is empty"
            } else {
                "could not open database"
            },
        },
        sqlite_db::DictError::Sqlite { code, message } => YomiError::Sqlite(
            rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(code), Some(message)),
        ),
        other => YomiError::MalformedIndex(other.to_string()),
    })
}

fn drop_user_tables(tx: &Transaction<'_>) -> Result<(), YomiError> {
    let tables: Vec<String> = {
        let mut stmt = tx.prepare("SELECT name FROM sqlite_master WHERE type = 'table';")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        let mut collected = Vec::new();
        for name_res in rows {
            let name = name_res?;
            if !name.starts_with("sqlite_") {
                collected.push(name);
            }
        }
        collected
    };
    for name in tables {
        let escaped = name.replace('"', "\"\"");
        tx.execute_batch(&format!("DROP TABLE IF EXISTS \"{escaped}\";"))?;
    }
    Ok(())
}

fn prepare_schema(conn: &mut Connection, defer_indexes_if_empty: bool) -> Result<bool, YomiError> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Exclusive)?;
    let version: i32 = tx.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version > CURRENT_SCHEMA_VERSION {
        return Err(YomiError::NewerDbVersion(version));
    }

    let mut deferred_indexes = false;
    match version {
        0 => {
            drop_user_tables(&tx)?;
            tx.execute_batch(CREATE_TABLES_SQL)?;
            if !defer_indexes_if_empty {
                tx.execute_batch(CREATE_INDEXES_SQL)?;
            } else {
                deferred_indexes = true;
            }
            tx.pragma_update(None, "user_version", CURRENT_SCHEMA_VERSION)?;
        }
        1..=3 => {
            if version <= 1 {
                tx.execute_batch(V1_TO_V2)?;
            }
            if version <= 2 {
                tx.execute_batch(V2_TO_V3)?;
            }
            tx.execute_batch(V3_TO_V4)?;
            // Drop legacy redundant idx_tag_bank_name if present and ensure expected indexes exist.
            tx.execute_batch("DROP INDEX IF EXISTS idx_tag_bank_name;")?;
            if !defer_indexes_if_empty {
                tx.execute_batch(CREATE_INDEXES_SQL)?;
            }
        }
        _ => {
            if !defer_indexes_if_empty {
                tx.execute_batch(CREATE_INDEXES_SQL)?;
            }
        }
    }

    if defer_indexes_if_empty && !deferred_indexes {
        let has_dictionaries: bool = tx
            .prepare("SELECT 1 FROM directory LIMIT 1;")?
            .exists([])?;
        if !has_dictionaries {
            tx.execute_batch(DROP_INDEXES_SQL)?;
            deferred_indexes = true;
        }
    }

    tx.commit()?;
    conn.pragma_update(None, "recursive_triggers", true)?;
    Ok(deferred_indexes)
}

/// Initializes or migrates the Yomitan SQLite schema at `db_file`.
pub fn prepare_db(db_file: impl AsRef<Path>) -> Result<(), YomiError> {
    let mut conn = open_writable_db(db_file.as_ref())?;
    let _ = prepare_schema(&mut conn, false)?;
    Ok(())
}

fn sanitize_zip_entry_path(
    entry_name: &str,
    enclosed: Option<PathBuf>,
    is_dir: bool,
) -> Result<PathBuf, YomiError> {
    if entry_name.contains('\0') {
        return Err(YomiError::UnsafeArchiveEntry {
            entry: entry_name.to_owned(),
            reason: "entry path contains NUL byte",
        });
    }
    if entry_name.starts_with('/') || entry_name.starts_with('\\') {
        return Err(YomiError::UnsafeArchiveEntry {
            entry: entry_name.to_owned(),
            reason: "entry path must be relative",
        });
    }
    if entry_name.split(['/', '\\']).any(|seg| seg == "..") {
        return Err(YomiError::UnsafeArchiveEntry {
            entry: entry_name.to_owned(),
            reason: "entry path contains parent traversal",
        });
    }
    let rel = enclosed.ok_or_else(|| YomiError::UnsafeArchiveEntry {
        entry: entry_name.to_owned(),
        reason: "entry path escapes archive root",
    })?;
    let mut clean = PathBuf::new();
    for component in rel.components() {
        match component {
            Component::Normal(part) => clean.push(part),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(YomiError::UnsafeArchiveEntry {
                    entry: entry_name.to_owned(),
                    reason: "entry path contains unsafe component",
                });
            }
        }
    }
    if !is_dir && clean.as_os_str().is_empty() {
        return Err(YomiError::UnsafeArchiveEntry {
            entry: entry_name.to_owned(),
            reason: "file entry path is empty",
        });
    }
    Ok(clean)
}

fn validate_entry_safety<R: Read>(
    entry: &zip::read::ZipFile<'_, R>,
    limits: SizeLimits,
) -> Result<PathBuf, YomiError> {
    let name = entry.name();
    if entry.is_symlink() {
        return Err(YomiError::UnsafeArchiveEntry {
            entry: name.to_owned(),
            reason: "symbolic links are not allowed",
        });
    }
    if let Some(mode) = entry.unix_mode() {
        let file_type = mode & 0o170000;
        if file_type != 0 && file_type != 0o100000 && file_type != 0o040000 {
            return Err(YomiError::UnsafeArchiveEntry {
                entry: name.to_owned(),
                reason: "special file entries are not allowed",
            });
        }
    }
    let rel = sanitize_zip_entry_path(name, entry.enclosed_name(), entry.is_dir())?;
    if !entry.is_dir() && entry.size() > limits.max_entry_bytes {
        return Err(YomiError::EntryTooLarge {
            entry: name.to_owned(),
            limit: limits.max_entry_bytes,
        });
    }
    Ok(rel)
}

fn validate_archive_entries<R: Read + Seek>(
    archive: &mut ZipArchive<R>,
    limits: SizeLimits,
) -> Result<(), YomiError> {
    for index in 0..archive.len() {
        let entry = archive.by_index(index)?;
        let _ = validate_entry_safety(&entry, limits)?;
    }
    Ok(())
}

fn read_json_entry<R: Read + Seek>(
    archive: &mut ZipArchive<R>,
    filename: &str,
    total_bytes: &mut u64,
    limits: SizeLimits,
) -> Result<Option<Value>, YomiError> {
    let entry = match archive.by_name(filename) {
        Ok(entry) => entry,
        Err(zip::result::ZipError::FileNotFound) => return Ok(None),
        Err(err) => return Err(YomiError::Zip(err)),
    };
    let _ = validate_entry_safety(&entry, limits)?;
    if entry.is_dir() {
        return Err(YomiError::UnsafeArchiveEntry {
            entry: filename.to_owned(),
            reason: "expected regular file, found directory",
        });
    }

    let remaining_total = limits.max_total_bytes.saturating_sub(*total_bytes);
    let entry_cap = limits.max_entry_bytes.min(remaining_total);
    let mut buf = Vec::new();
    entry
        .take(entry_cap.saturating_add(1))
        .read_to_end(&mut buf)
        .map_err(|source| YomiError::Io {
            operation: "read archive entry",
            path: PathBuf::from(filename),
            source,
        })?;
    let bytes_read = buf.len() as u64;
    if bytes_read > limits.max_entry_bytes {
        return Err(YomiError::EntryTooLarge {
            entry: filename.to_owned(),
            limit: limits.max_entry_bytes,
        });
    }
    let new_total = total_bytes
        .checked_add(bytes_read)
        .ok_or(YomiError::ArchiveTooLarge {
            limit: limits.max_total_bytes,
        })?;
    if new_total > limits.max_total_bytes {
        return Err(YomiError::ArchiveTooLarge {
            limit: limits.max_total_bytes,
        });
    }
    *total_bytes = new_total;

    let value = serde_json::from_slice(&buf).map_err(|source| YomiError::Json {
        entry: filename.to_owned(),
        source,
    })?;
    Ok(Some(value))
}

fn add_index<R: Read + Seek>(
    archive: &mut ZipArchive<R>,
    tx: &Transaction<'_>,
    total_bytes: &mut u64,
    limits: SizeLimits,
) -> Result<(i64, Option<i64>), YomiError> {
    let Some(index_val) = read_json_entry(archive, "index.json", total_bytes, limits)? else {
        return Err(YomiError::MalformedIndex("missing index.json".to_owned()));
    };
    let obj = index_val
        .as_object()
        .ok_or_else(|| YomiError::MalformedIndex("index.json must be an object".to_owned()))?;

    let title = obj
        .get("title")
        .and_then(Value::as_str)
        .ok_or_else(|| YomiError::MalformedIndex("missing or non-string 'title'".to_owned()))?;
    let format = obj
        .get("format")
        .and_then(Value::as_i64)
        .ok_or_else(|| YomiError::MalformedIndex("missing or non-integer 'format'".to_owned()))?;
    if format != 3 {
        return Err(YomiError::UnsupportedFormat(format));
    }
    let revision = obj
        .get("revision")
        .and_then(Value::as_str)
        .ok_or_else(|| YomiError::MalformedIndex("missing or non-string 'revision'".to_owned()))?;
    let sequenced = match obj.get("sequenced") {
        None => false,
        Some(val) => val.as_bool().ok_or_else(|| {
            YomiError::MalformedIndex("'sequenced' must be a boolean".to_owned())
        })?,
    };

    let replaced_dic_id: Option<i64> = tx
        .query_row(
            "SELECT dic_id FROM directory WHERE title = ?1;",
            params![title],
            |row| row.get(0),
        )
        .optional()?;

    tx.execute(
        "INSERT OR REPLACE INTO directory (title, format, revision, sequenced) VALUES (?1, ?2, ?3, ?4);",
        params![title, format, revision, i64::from(sequenced)],
    )?;
    let dic_id = tx.last_insert_rowid();
    if dic_id <= 0 {
        return Err(YomiError::InvalidDictionaryId(dic_id));
    }
    Ok((dic_id, replaced_dic_id))
}

#[derive(Clone, Copy)]
enum Bank {
    Tag,
    Term,
    TermMeta,
    Kanji,
    KanjiMeta,
}

impl Bank {
    fn prefix(self) -> &'static str {
        match self {
            Self::Tag => "tag_bank_",
            Self::Term => "term_bank_",
            Self::TermMeta => "term_meta_bank_",
            Self::Kanji => "kanji_bank_",
            Self::KanjiMeta => "kanji_meta_bank_",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Tag => "tag_bank",
            Self::Term => "term_bank",
            Self::TermMeta => "term_meta_bank",
            Self::Kanji => "kanji_bank",
            Self::KanjiMeta => "kanji_meta_bank",
        }
    }

    fn insert_sql(self) -> &'static str {
        match self {
            Self::Tag => {
                "INSERT INTO tag_bank (dic_id, name, category, ord, notes, score) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6);"
            }
            Self::Term => {
                "INSERT INTO term_bank (dic_id, expression, reading, def_tags, rules, score, glossary, sequence, term_tags) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9);"
            }
            Self::TermMeta => {
                "INSERT INTO term_meta_bank (dic_id, expression, mode, type, data) \
                 VALUES (?1, ?2, ?3, ?4, ?5);"
            }
            Self::Kanji => {
                "INSERT INTO kanji_bank (dic_id, char, onyomi, kunyomi, tags, meanings, stats) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7);"
            }
            Self::KanjiMeta => {
                "INSERT INTO kanji_meta_bank (dic_id, expression, mode, type, data) \
                 VALUES (?1, ?2, ?3, ?4, ?5);"
            }
        }
    }
}

fn array_str<'a>(items: &'a [Value], index: usize, bank: &'static str) -> Result<&'a str, YomiError> {
    items
        .get(index)
        .and_then(Value::as_str)
        .ok_or_else(|| YomiError::MalformedBank {
            bank,
            detail: format!("element {index} must be a string"),
        })
}

fn array_i64(items: &[Value], index: usize, bank: &'static str) -> Result<i64, YomiError> {
    items
        .get(index)
        .and_then(Value::as_i64)
        .ok_or_else(|| YomiError::MalformedBank {
            bank,
            detail: format!("element {index} must be an integer"),
        })
}

fn with_nul(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len() + 1);
    out.extend_from_slice(bytes);
    out.push(0);
    out
}

fn encode_meta_blob(data: &Value, entry: &str) -> Result<(i64, Option<Vec<u8>>), YomiError> {
    match data {
        Value::Null => Ok((0, None)),
        Value::Number(num) => {
            if let Some(i) = num.as_i64() {
                Ok((1, Some(i.to_ne_bytes().to_vec())))
            } else if let Some(f) = num.as_f64() {
                Ok((3, Some(f.to_ne_bytes().to_vec())))
            } else {
                Err(YomiError::MalformedBank {
                    bank: "meta_bank",
                    detail: format!("unsupported JSON number in '{entry}'"),
                })
            }
        }
        Value::String(s) => Ok((2, Some(with_nul(s.as_bytes())))),
        Value::Object(_) => {
            let encoded = serde_json::to_vec(data).map_err(|source| YomiError::Json {
                entry: entry.to_owned(),
                source,
            })?;
            Ok((4, Some(with_nul(&encoded))))
        }
        Value::Array(_) => {
            let encoded = serde_json::to_vec(data).map_err(|source| YomiError::Json {
                entry: entry.to_owned(),
                source,
            })?;
            Ok((5, Some(with_nul(&encoded))))
        }
        Value::Bool(b) => {
            let val: i32 = i32::from(*b);
            Ok((6, Some(val.to_ne_bytes().to_vec())))
        }
    }
}

fn add_bank<R: Read + Seek>(
    archive: &mut ZipArchive<R>,
    tx: &Transaction<'_>,
    dic_id: i64,
    bank: Bank,
    total_bytes: &mut u64,
    limits: SizeLimits,
) -> Result<(), YomiError> {
    let mut stmt = tx.prepare(bank.insert_sql())?;
    let bank_label = bank.label();
    let prefix = bank.prefix();
    let mut number = 1usize;

    loop {
        let filename = format!("{prefix}{number}.json");
        let Some(root) = read_json_entry(archive, &filename, total_bytes, limits)? else {
            break;
        };
        let items = root.as_array().ok_or_else(|| YomiError::MalformedBank {
            bank: bank_label,
            detail: format!("'{filename}' root must be a JSON array"),
        })?;

        for row_val in items {
            let row = row_val.as_array().ok_or_else(|| YomiError::MalformedBank {
                bank: bank_label,
                detail: format!("row in '{filename}' must be a JSON array"),
            })?;
            match bank {
                Bank::Tag => {
                    if row.len() != 5 {
                        return Err(YomiError::MalformedBank {
                            bank: bank_label,
                            detail: format!("expected 5 elements, got {}", row.len()),
                        });
                    }
                    let name = array_str(row, 0, bank_label)?;
                    let category = array_str(row, 1, bank_label)?;
                    let order = array_i64(row, 2, bank_label)?;
                    let notes = array_str(row, 3, bank_label)?;
                    let score = array_i64(row, 4, bank_label)?;
                    stmt.execute(params![dic_id, name, category, order, notes, score])?;
                }
                Bank::Term => {
                    if row.len() != 8 {
                        return Err(YomiError::MalformedBank {
                            bank: bank_label,
                            detail: format!("expected 8 elements, got {}", row.len()),
                        });
                    }
                    let expression = array_str(row, 0, bank_label)?;
                    let raw_reading = array_str(row, 1, bank_label)?;
                    let reading = if expression == raw_reading { "" } else { raw_reading };
                    let def_tags = match &row[2] {
                        Value::String(s) => s.as_str(),
                        Value::Null => "",
                        _ => {
                            return Err(YomiError::MalformedBank {
                                bank: bank_label,
                                detail: "element 2 (def_tags) must be string or null".to_owned(),
                            });
                        }
                    };
                    let rules = array_str(row, 3, bank_label)?;
                    let score = array_i64(row, 4, bank_label)?;
                    if !row[5].is_array() {
                        return Err(YomiError::MalformedBank {
                            bank: bank_label,
                            detail: "element 5 (glossary) must be an array".to_owned(),
                        });
                    }
                    let glossary = serde_json::to_string(&row[5]).map_err(|source| {
                        YomiError::Json {
                            entry: filename.clone(),
                            source,
                        }
                    })?;
                    let sequence = array_i64(row, 6, bank_label)?;
                    let term_tags = array_str(row, 7, bank_label)?;
                    stmt.execute(params![
                        dic_id,
                        expression,
                        reading,
                        def_tags,
                        rules,
                        score,
                        glossary,
                        sequence,
                        term_tags,
                    ])?;
                }
                Bank::Kanji => {
                    if row.len() != 6 {
                        return Err(YomiError::MalformedBank {
                            bank: bank_label,
                            detail: format!("expected 6 elements, got {}", row.len()),
                        });
                    }
                    let character = array_str(row, 0, bank_label)?;
                    let onyomi = array_str(row, 1, bank_label)?;
                    let kunyomi = array_str(row, 2, bank_label)?;
                    let tags = array_str(row, 3, bank_label)?;
                    if !row[4].is_array() {
                        return Err(YomiError::MalformedBank {
                            bank: bank_label,
                            detail: "element 4 (meanings) must be an array".to_owned(),
                        });
                    }
                    if !row[5].is_object() {
                        return Err(YomiError::MalformedBank {
                            bank: bank_label,
                            detail: "element 5 (stats) must be an object".to_owned(),
                        });
                    }
                    let meanings = serde_json::to_string(&row[4]).map_err(|source| {
                        YomiError::Json {
                            entry: filename.clone(),
                            source,
                        }
                    })?;
                    let stats = serde_json::to_string(&row[5]).map_err(|source| {
                        YomiError::Json {
                            entry: filename.clone(),
                            source,
                        }
                    })?;
                    stmt.execute(params![
                        dic_id,
                        character,
                        onyomi,
                        kunyomi,
                        tags,
                        meanings,
                        stats,
                    ])?;
                }
                Bank::TermMeta | Bank::KanjiMeta => {
                    if row.len() != 3 {
                        return Err(YomiError::MalformedBank {
                            bank: bank_label,
                            detail: format!("expected 3 elements, got {}", row.len()),
                        });
                    }
                    let expression = array_str(row, 0, bank_label)?;
                    let mode = array_str(row, 1, bank_label)?;
                    let (kind, blob) = encode_meta_blob(&row[2], &filename)?;
                    stmt.execute(params![dic_id, expression, mode, kind, blob])?;
                }
            }
        }
        number += 1;
    }

    Ok(())
}

fn is_skipped_resource(rel_path: &Path) -> bool {
    let mut components = rel_path.components();
    let (Some(Component::Normal(first)), None) = (components.next(), components.next()) else {
        return false;
    };
    let Some(name) = first.to_str() else {
        return false;
    };
    if name == "index.json" {
        return true;
    }
    for prefix in [
        "tag_bank_",
        "term_bank_",
        "term_meta_bank_",
        "kanji_bank_",
        "kanji_meta_bank_",
    ] {
        if let Some(rest) = name
            .strip_prefix(prefix)
            .and_then(|s| s.strip_suffix(".json"))
        {
            if rest.starts_with(|c: char| matches!(c, '1'..='9'))
                && rest[1..].bytes().all(|b| b.is_ascii_digit())
            {
                return true;
            }
        }
    }
    false
}

fn extract_resources_to_staging<R: Read + Seek>(
    archive: &mut ZipArchive<R>,
    staging_dir: &Path,
    total_bytes: &mut u64,
    limits: SizeLimits,
) -> Result<(), YomiError> {
    for index in 0..archive.len() {
        let entry = archive.by_index(index)?;
        let entry_name = entry.name().to_owned();
        let rel_path = validate_entry_safety(&entry, limits)?;
        if is_skipped_resource(&rel_path) {
            continue;
        }
        if entry.is_dir() {
            if !rel_path.as_os_str().is_empty() {
                let target_dir = staging_dir.join(&rel_path);
                fs::create_dir_all(&target_dir).map_err(|source| YomiError::Io {
                    operation: "create resource directory",
                    path: target_dir,
                    source,
                })?;
            }
            continue;
        }

        let out_path = staging_dir.join(&rel_path);
        if let Some(parent) = out_path.parent() {
            fs::create_dir_all(parent).map_err(|source| YomiError::Io {
                operation: "create resource parent directory",
                path: parent.to_path_buf(),
                source,
            })?;
        }

        let remaining_total = limits.max_total_bytes.saturating_sub(*total_bytes);
        let entry_cap = limits.max_entry_bytes.min(remaining_total);
        let mut limited = entry.take(entry_cap.saturating_add(1));
        let mut out_file = File::create(&out_path).map_err(|source| YomiError::Io {
            operation: "create resource file",
            path: out_path.clone(),
            source,
        })?;
        let bytes_written = std::io::copy(&mut limited, &mut out_file).map_err(|source| {
            YomiError::Io {
                operation: "write resource file",
                path: out_path.clone(),
                source,
            }
        })?;
        if bytes_written > limits.max_entry_bytes {
            return Err(YomiError::EntryTooLarge {
                entry: entry_name,
                limit: limits.max_entry_bytes,
            });
        }
        let new_total = total_bytes
            .checked_add(bytes_written)
            .ok_or(YomiError::ArchiveTooLarge {
                limit: limits.max_total_bytes,
            })?;
        if new_total > limits.max_total_bytes {
            return Err(YomiError::ArchiveTooLarge {
                limit: limits.max_total_bytes,
            });
        }
        *total_bytes = new_total;
        out_file.flush().map_err(|source| YomiError::Io {
            operation: "flush resource file",
            path: out_path,
            source,
        })?;
    }
    Ok(())
}

fn safe_remove_dict_resource_dir(canon_res_dir: &Path, dic_id: i64) -> Result<(), YomiError> {
    if dic_id <= 0 {
        return Err(YomiError::InvalidDictionaryId(dic_id));
    }
    let dir_name = format!("dict-{dic_id}");
    let target = canon_res_dir.join(&dir_name);
    let meta = match fs::symlink_metadata(&target) {
        Ok(meta) => meta,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(()),
        Err(source) => {
            return Err(YomiError::Io {
                operation: "inspect dictionary resource directory",
                path: target,
                source,
            });
        }
    };
    if meta.file_type().is_symlink() {
        return fs::remove_file(&target).map_err(|source| YomiError::Io {
            operation: "remove dictionary resource symlink",
            path: target,
            source,
        });
    }
    if meta.is_dir() {
        let canon_target = fs::canonicalize(&target).map_err(|source| YomiError::Io {
            operation: "canonicalize dictionary resource directory",
            path: target.clone(),
            source,
        })?;
        if canon_target.parent() != Some(canon_res_dir)
            || canon_target.file_name() != Some(OsStr::new(&dir_name))
        {
            return Err(YomiError::InvalidPath {
                path: canon_target,
                reason: "target escaped resources directory",
            });
        }
        return fs::remove_dir_all(&canon_target).map_err(|source| YomiError::Io {
            operation: "remove dictionary resource directory",
            path: canon_target,
            source,
        });
    }
    fs::remove_file(&target).map_err(|source| YomiError::Io {
        operation: "remove dictionary resource entry",
        path: target,
        source,
    })
}

/// Imports a Yomichan/Yomitan `.zip` dictionary into `db_file` and extracts
/// non-bank resource files under `{res_dir}/dict-{dic_id}`.
pub fn process_dictionary(
    dict_file: impl AsRef<Path>,
    db_file: impl AsRef<Path>,
    res_dir: impl AsRef<Path>,
) -> Result<i64, YomiError> {
    process_dictionary_with_limits(dict_file.as_ref(), db_file.as_ref(), res_dir.as_ref(), SizeLimits::default())
}

fn process_dictionary_with_limits(
    dict_file: &Path,
    db_file: &Path,
    res_dir: &Path,
    limits: SizeLimits,
) -> Result<i64, YomiError> {
    validate_non_empty_path(dict_file)?;
    validate_non_empty_path(db_file)?;
    validate_non_empty_path(res_dir)?;

    let archive_file = File::open(dict_file).map_err(|source| YomiError::Io {
        operation: "open dictionary archive",
        path: dict_file.to_path_buf(),
        source,
    })?;
    let mut archive = ZipArchive::new(BufReader::new(archive_file))?;
    validate_archive_entries(&mut archive, limits)?;

    fs::create_dir_all(res_dir).map_err(|source| YomiError::Io {
        operation: "create resources directory",
        path: res_dir.to_path_buf(),
        source,
    })?;
    let canon_res_dir = fs::canonicalize(res_dir).map_err(|source| YomiError::Io {
        operation: "canonicalize resources directory",
        path: res_dir.to_path_buf(),
        source,
    })?;

    let mut conn = open_writable_db(db_file)?;
    let defer_indexes = prepare_schema(&mut conn, true)?;

    let staging = tempfile::Builder::new()
        .prefix(".memento-import-")
        .tempdir_in(&canon_res_dir)
        .map_err(|source| YomiError::Io {
            operation: "create staging directory",
            path: canon_res_dir.clone(),
            source,
        })?;

    let mut total_bytes = 0u64;
    let import_result = (|| -> Result<(i64, Option<i64>), YomiError> {
        let tx = conn.transaction_with_behavior(TransactionBehavior::Exclusive)?;
        let (dic_id, replaced_dic_id) = add_index(&mut archive, &tx, &mut total_bytes, limits)?;
        for bank in [
            Bank::Tag,
            Bank::Term,
            Bank::TermMeta,
            Bank::Kanji,
            Bank::KanjiMeta,
        ] {
            add_bank(&mut archive, &tx, dic_id, bank, &mut total_bytes, limits)?;
        }
        if defer_indexes {
            tx.execute_batch(CREATE_INDEXES_SQL)?;
        }
        extract_resources_to_staging(&mut archive, staging.path(), &mut total_bytes, limits)?;
        tx.commit()?;
        Ok((dic_id, replaced_dic_id))
    })();
    let (dic_id, replaced_dic_id) = match import_result {
        Ok(ids) => ids,
        Err(err) => {
            if defer_indexes {
                let _ = conn.execute_batch(CREATE_INDEXES_SQL);
            }
            return Err(err);
        }
    };

    safe_remove_dict_resource_dir(&canon_res_dir, dic_id)?;
    if let Some(old_id) = replaced_dic_id {
        if old_id != dic_id {
            safe_remove_dict_resource_dir(&canon_res_dir, old_id)?;
        }
    }

    let target_dir = dictionary_resource_dir(&canon_res_dir, dic_id)?;
    let staging_path = staging.keep();
    if let Err(source) = fs::rename(&staging_path, &target_dir) {
        let _ = fs::remove_dir_all(&staging_path);
        return Err(YomiError::Io {
            operation: "install dictionary resource directory",
            path: target_dir,
            source,
        });
    }

    Ok(dic_id)
}

/// Deletes an installed dictionary row (triggering cascading row cleanup) and
/// removes `{res_dir}/dict-{dic_id}` without ever consulting `directory.title`.
pub fn delete_dictionary(
    dic_id: i64,
    db_file: impl AsRef<Path>,
    res_dir: impl AsRef<Path>,
) -> Result<(), YomiError> {
    if dic_id <= 0 {
        return Err(YomiError::InvalidDictionaryId(dic_id));
    }
    let db_file = db_file.as_ref();
    let res_dir = res_dir.as_ref();
    validate_non_empty_path(db_file)?;
    validate_non_empty_path(res_dir)?;

    let mut conn = open_writable_db(db_file)?;
    let _ = prepare_schema(&mut conn, false)?;
    let deleted = conn.execute("DELETE FROM directory WHERE dic_id = ?1;", params![dic_id])?;
    if deleted == 0 {
        return Err(YomiError::DictionaryNotFound(dic_id));
    }

    if res_dir.exists() {
        let canon_res_dir = fs::canonicalize(res_dir).map_err(|source| YomiError::Io {
            operation: "canonicalize resources directory",
            path: res_dir.to_path_buf(),
            source,
        })?;
        safe_remove_dict_resource_dir(&canon_res_dir, dic_id)?;
    }
    Ok(())
}

/// Marks an installed dictionary as disabled in `dict_disabled`.
pub fn disable_dictionary(dic_id: i64, db_file: impl AsRef<Path>) -> Result<(), YomiError> {
    if dic_id <= 0 {
        return Err(YomiError::InvalidDictionaryId(dic_id));
    }
    let mut conn = open_writable_db(db_file.as_ref())?;
    let _ = prepare_schema(&mut conn, false)?;
    conn.execute(
        "INSERT OR IGNORE INTO dict_disabled (dic_id) VALUES (?1);",
        params![dic_id],
    )?;
    Ok(())
}

/// Removes a dictionary from `dict_disabled`.
pub fn enable_dictionary(dic_id: i64, db_file: impl AsRef<Path>) -> Result<(), YomiError> {
    if dic_id <= 0 {
        return Err(YomiError::InvalidDictionaryId(dic_id));
    }
    let mut conn = open_writable_db(db_file.as_ref())?;
    let _ = prepare_schema(&mut conn, false)?;
    conn.execute(
        "DELETE FROM dict_disabled WHERE dic_id = ?1;",
        params![dic_id],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;
    use zip::write::SimpleFileOptions;
    use zip::ZipWriter;

    fn build_zip(path: &Path, entries: &[(&str, &[u8], Option<u32>)]) {
        let file = File::create(path).unwrap();
        let mut writer = ZipWriter::new(file);
        for &(name, contents, mode) in entries {
            let mut opts = SimpleFileOptions::default();
            if let Some(m) = mode {
                opts = opts.unix_permissions(m);
            }
            writer.start_file(name, opts).unwrap();
            writer.write_all(contents).unwrap();
        }
        writer.finish().unwrap();
    }

    #[test]
    fn malicious_title_cannot_delete_or_write_outside_res_dir() {
        let root = TempDir::new().unwrap();
        let res_dir = root.path().join("res");
        fs::create_dir_all(&res_dir).unwrap();
        let victim_dir = root.path().join("evil");
        fs::create_dir_all(&victim_dir).unwrap();
        let victim_file = victim_dir.join("keep.txt");
        fs::write(&victim_file, b"precious").unwrap();

        let db_path = root.path().join("dictionaries.sqlite");
        let zip_path = root.path().join("malicious_title.zip");
        build_zip(
            &zip_path,
            &[
                (
                    "index.json",
                    br#"{"title":"../../evil","format":3,"revision":"1"}"#,
                    None,
                ),
                ("styles.css", b"body { color: red; }", None),
            ],
        );

        let dic_id = process_dictionary(&zip_path, &db_path, &res_dir).unwrap();
        assert!(dic_id > 0);
        assert!(victim_file.exists(), "outside directory must never be deleted");
        assert_eq!(fs::read(&victim_file).unwrap(), b"precious");

        let safe_dir = dictionary_resource_dir(&res_dir, dic_id).unwrap();
        assert_eq!(
            fs::read(safe_dir.join("styles.css")).unwrap(),
            b"body { color: red; }"
        );

        delete_dictionary(dic_id, &db_path, &res_dir).unwrap();
        assert!(victim_file.exists(), "delete must not follow untrusted title");
        assert!(!safe_dir.exists());
    }

    #[test]
    fn zip_slip_entry_is_rejected_without_touching_disk_or_db() {
        let root = TempDir::new().unwrap();
        let res_dir = root.path().join("res");
        let db_path = root.path().join("dictionaries.sqlite");
        let zip_path = root.path().join("zipslip.zip");
        let escaped_target = root.path().join(".bashrc");

        build_zip(
            &zip_path,
            &[
                (
                    "index.json",
                    br#"{"title":"SlipDict","format":3,"revision":"1"}"#,
                    None,
                ),
                ("../../.bashrc", b"pwned", None),
            ],
        );

        let err = process_dictionary(&zip_path, &db_path, &res_dir).unwrap_err();
        assert!(
            matches!(err, YomiError::UnsafeArchiveEntry { .. }),
            "expected UnsafeArchiveEntry, got {err:?}"
        );
        assert!(!escaped_target.exists());
        assert!(!db_path.exists());
    }

    #[test]
    fn symlink_entry_is_rejected() {
        let root = TempDir::new().unwrap();
        let res_dir = root.path().join("res");
        let db_path = root.path().join("dictionaries.sqlite");
        let zip_path = root.path().join("symlink.zip");

        let file = File::create(&zip_path).unwrap();
        let mut writer = ZipWriter::new(file);
        writer
            .start_file("index.json", SimpleFileOptions::default())
            .unwrap();
        writer
            .write_all(br#"{"title":"SymDict","format":3,"revision":"1"}"#)
            .unwrap();
        writer
            .add_symlink("evil_link", "/etc/passwd", SimpleFileOptions::default())
            .unwrap();
        writer.finish().unwrap();

        let err = process_dictionary(&zip_path, &db_path, &res_dir).unwrap_err();
        assert!(
            matches!(err, YomiError::UnsafeArchiveEntry { .. }),
            "expected UnsafeArchiveEntry, got {err:?}"
        );
        assert!(!db_path.exists());
    }

    #[test]
    fn import_query_disable_enable_and_delete_roundtrip() {
        let root = TempDir::new().unwrap();
        let res_dir = root.path().join("res");
        let db_path = root.path().join("dictionaries.sqlite");
        let zip_path = root.path().join("good.zip");

        build_zip(
            &zip_path,
            &[
                (
                    "index.json",
                    br#"{"title":"TestDict","format":3,"revision":"2026","sequenced":true}"#,
                    None,
                ),
                (
                    "tag_bank_1.json",
                    br#"[["v1","partOfSpeech",1,"Ichidan verb",50]]"#,
                    None,
                ),
                (
                    "term_bank_1.json",
                    r#"[["食べる","たべる","v1","v1",100,["to eat"],1,""],["あ","あ","","",10,["ah"],2,""]]"#.as_bytes(),
                    None,
                ),
                (
                    "term_meta_bank_1.json",
                    r#"[["食べる","freq",{"reading":"たべる","frequency":120}],["食べる","pitch",{"reading":"たべる","pitches":[{"position":2}]}]]"#.as_bytes(),
                    None,
                ),
                (
                    "kanji_bank_1.json",
                    r#"[["食","ショク","た.べる","v1",["eat","food"],{"strokes":"9"}]]"#.as_bytes(),
                    None,
                ),
                (
                    "kanji_meta_bank_1.json",
                    r#"[["食","freq",42]]"#.as_bytes(),
                    None,
                ),
                ("styles.css", b".gloss { margin: 0; }", None),
            ],
        );

        let dic_id = process_dictionary(&zip_path, &db_path, &res_dir).unwrap();
        assert_eq!(dic_id, 1);

        let conn = sqlite_db::open_read_only_connection(&db_path).unwrap();
        let cache = sqlite_db::init_cache_conn(&conn).unwrap();
        assert_eq!(cache.dictionaries.len(), 1);
        assert_eq!(cache.dictionaries[0].title, "TestDict");
        assert!(!cache.dictionaries[0].disabled);
        assert_eq!(cache.table_counts.tag_bank, 1);
        assert_eq!(cache.table_counts.term_bank, 2);
        assert_eq!(cache.table_counts.term_meta_bank, 2);
        assert_eq!(cache.table_counts.kanji_bank, 1);
        assert_eq!(cache.table_counts.kanji_meta_bank, 1);

        // Verify WAL mode is active on the database and redundant idx_tag_bank_name was omitted.
        let journal_mode: String = conn
            .pragma_query_value(None, "journal_mode", |row| row.get(0))
            .unwrap();
        assert_eq!(journal_mode.to_ascii_lowercase(), "wal");
        let idx_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' AND name = 'idx_tag_bank_name';",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(idx_count, 0);

        // Verify query_terms, query_kanji, frequencies, and pitches.
        let raw: Vec<u16> = "食べる".encode_utf16().collect();
        let plan = sqlite_db::TermQueryPlan::new(&raw, &raw);
        let mut terms = Vec::new();
        sqlite_db::query_terms_conn(&conn, &plan, &mut terms).unwrap();
        assert_eq!(terms.len(), 1);
        assert_eq!(String::from_utf16_lossy(&terms[0].reading), "たべる");

        // Expression == reading was normalized to empty string on insert.
        let a_u16: Vec<u16> = "あ".encode_utf16().collect();
        let plan_a = sqlite_db::TermQueryPlan::new(&a_u16, &a_u16);
        let mut terms_a = Vec::new();
        sqlite_db::query_terms_conn(&conn, &plan_a, &mut terms_a).unwrap();
        assert_eq!(terms_a.len(), 1);
        assert!(terms_a[0].reading.is_empty());

        let mut freqs = Vec::new();
        sqlite_db::get_frequencies_conn(
            &conn,
            &raw,
            &"たべる".encode_utf16().collect::<Vec<_>>(),
            &mut freqs,
        )
        .unwrap();
        assert_eq!(freqs.len(), 1);
        assert_eq!(String::from_utf16_lossy(&freqs[0].frequency), "120");

        let mut pitches = Vec::new();
        sqlite_db::add_pitches_conn(
            &conn,
            &raw,
            &"たべる".encode_utf16().collect::<Vec<_>>(),
            &mut pitches,
        )
        .unwrap();
        assert_eq!(pitches.len(), 1);
        assert_eq!(pitches[0].positions, vec![2]);

        let mut kanji = Vec::new();
        sqlite_db::query_kanji_conn(&conn, &"食".encode_utf16().collect::<Vec<_>>(), &mut kanji)
            .unwrap();
        assert_eq!(kanji.len(), 1);

        // Disable & enable
        disable_dictionary(dic_id, &db_path).unwrap();
        let mut disabled_terms = Vec::new();
        sqlite_db::query_terms_conn(&conn, &plan, &mut disabled_terms).unwrap();
        assert!(disabled_terms.is_empty());

        enable_dictionary(dic_id, &db_path).unwrap();
        let mut reenabled_terms = Vec::new();
        sqlite_db::query_terms_conn(&conn, &plan, &mut reenabled_terms).unwrap();
        assert_eq!(reenabled_terms.len(), 1);

        drop(conn);

        // Delete dictionary and confirm cascading cleanup + resource removal
        delete_dictionary(dic_id, &db_path, &res_dir).unwrap();
        assert!(!dictionary_resource_dir(&res_dir, dic_id).unwrap().exists());

        let conn_after = sqlite_db::open_read_only_connection(&db_path).unwrap();
        let cache_after = sqlite_db::init_cache_conn(&conn_after).unwrap();
        assert!(cache_after.dictionaries.is_empty());
        assert_eq!(cache_after.table_counts.term_bank, 0);
        assert_eq!(cache_after.table_counts.tag_bank, 0);
        assert_eq!(cache_after.table_counts.kanji_bank, 0);
    }

    #[test]
    fn large_i64_dictionary_id_roundtrips_without_32bit_truncation() {
        let root = TempDir::new().unwrap();
        let db_path = root.path().join("dictionaries.sqlite");
        let res_dir = root.path().join("res");
        prepare_db(&db_path).unwrap();
        fs::create_dir_all(&res_dir).unwrap();

        let big_id: i64 = i64::from(i32::MAX) + 42;
        let conn = sqlite_db::open_read_write_connection(&db_path).unwrap();
        conn.execute(
            "INSERT INTO directory (dic_id, title, format, revision, sequenced) VALUES (?1, 'BigDict', 3, '1', 0);",
            params![big_id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO term_bank (dic_id, expression, reading, def_tags, rules, score, glossary, sequence, term_tags) \
             VALUES (?1, '猫', 'ねこ', '', '', 10, '[\"cat\"]', 1, '');",
            params![big_id],
        )
        .unwrap();
        drop(conn);

        disable_dictionary(big_id, &db_path).unwrap();
        let ro = sqlite_db::open_read_only_connection(&db_path).unwrap();
        let cache = sqlite_db::init_cache_conn(&ro).unwrap();
        assert_eq!(cache.dictionaries.len(), 1);
        assert_eq!(cache.dictionaries[0].id, big_id);
        assert!(cache.dictionaries[0].disabled);

        enable_dictionary(big_id, &db_path).unwrap();
        let cache2 = sqlite_db::init_cache_conn(&ro).unwrap();
        assert!(!cache2.dictionaries[0].disabled);
        drop(ro);

        let dict_res = dictionary_resource_dir(&res_dir, big_id).unwrap();
        fs::create_dir_all(&dict_res).unwrap();
        fs::write(dict_res.join("styles.css"), b"a{}").unwrap();

        delete_dictionary(big_id, &db_path, &res_dir).unwrap();
        assert!(!dict_res.exists());
    }

    #[test]
    fn additional_zip_traversal_variants_are_rejected() {
        let root = TempDir::new().unwrap();
        let res_dir = root.path().join("res");
        let db_path = root.path().join("dictionaries.sqlite");

        for bad_entry in ["/etc/passwd", "foo/../bar.txt", r"..\..\evil.txt"] {
            let zip_path = root.path().join("bad.zip");
            build_zip(
                &zip_path,
                &[
                    (
                        "index.json",
                        br#"{"title":"BadPathDict","format":3,"revision":"1"}"#,
                        None,
                    ),
                    (bad_entry, b"nope", None),
                ],
            );
            let err = process_dictionary(&zip_path, &db_path, &res_dir).unwrap_err();
            assert!(
                matches!(err, YomiError::UnsafeArchiveEntry { .. }),
                "entry '{bad_entry}' must be rejected, got {err:?}"
            );
        }
        assert!(!db_path.exists());
    }

    #[test]
    fn zip_bomb_exceeding_limits_is_rejected() {
        let root = TempDir::new().unwrap();
        let res_dir = root.path().join("res");
        let db_path = root.path().join("dictionaries.sqlite");
        let zip_path = root.path().join("bomb.zip");

        let huge_payload = vec![b'a'; 2048];
        build_zip(
            &zip_path,
            &[
                (
                    "index.json",
                    br#"{"title":"BombDict","format":3,"revision":"1"}"#,
                    None,
                ),
                ("blob.bin", &huge_payload, None),
            ],
        );

        let err = process_dictionary_with_limits(
            &zip_path,
            &db_path,
            &res_dir,
            SizeLimits {
                max_entry_bytes: 1024,
                max_total_bytes: 4096,
            },
        )
        .unwrap_err();
        assert!(
            matches!(err, YomiError::EntryTooLarge { .. }),
            "expected EntryTooLarge, got {err:?}"
        );
        assert!(!db_path.exists());

        // Now allow single entry of 2048 bytes, but enforce cumulative total limit of 2060 bytes.
        let err_total = process_dictionary_with_limits(
            &zip_path,
            &db_path,
            &res_dir,
            SizeLimits {
                max_entry_bytes: 4096,
                max_total_bytes: 2060,
            },
        )
        .unwrap_err();
        assert!(
            matches!(err_total, YomiError::ArchiveTooLarge { .. }),
            "expected ArchiveTooLarge, got {err_total:?}"
        );
    }

    #[test]
    fn reimporting_same_title_replaces_old_resources_and_rows() {
        let root = TempDir::new().unwrap();
        let res_dir = root.path().join("res");
        let db_path = root.path().join("dictionaries.sqlite");
        let zip1 = root.path().join("v1.zip");
        let zip2 = root.path().join("v2.zip");

        build_zip(
            &zip1,
            &[
                (
                    "index.json",
                    br#"{"title":"SameDict","format":3,"revision":"v1"}"#,
                    None,
                ),
                ("term_bank_1.json", r#"[["古い","ふるい","","",10,["old"],1,""]]"#.as_bytes(), None),
                ("old.css", b"v1", None),
            ],
        );
        build_zip(
            &zip2,
            &[
                (
                    "index.json",
                    br#"{"title":"SameDict","format":3,"revision":"v2"}"#,
                    None,
                ),
                ("term_bank_1.json", r#"[["新しい","あたらしい","","",20,["new"],1,""]]"#.as_bytes(), None),
                ("new.css", b"v2", None),
            ],
        );

        let id1 = process_dictionary(&zip1, &db_path, &res_dir).unwrap();
        let dir1 = dictionary_resource_dir(&res_dir, id1).unwrap();
        assert!(dir1.join("old.css").exists());

        let id2 = process_dictionary(&zip2, &db_path, &res_dir).unwrap();
        let dir2 = dictionary_resource_dir(&res_dir, id2).unwrap();
        assert!(!dir2.join("old.css").exists());
        assert_eq!(fs::read(dir2.join("new.css")).unwrap(), b"v2");
        if id1 != id2 {
            assert!(!dir1.exists());
        }

        let conn = sqlite_db::open_read_only_connection(&db_path).unwrap();
        let cache = sqlite_db::init_cache_conn(&conn).unwrap();
        assert_eq!(cache.dictionaries.len(), 1);
        assert_eq!(cache.table_counts.term_bank, 1);
    }

    #[test]
    fn failed_initial_import_restores_all_schema_indexes() {
        let root = TempDir::new().unwrap();
        let res_dir = root.path().join("res");
        let db_path = root.path().join("dictionaries.sqlite");
        let bad_zip = root.path().join("bad.zip");

        build_zip(
            &bad_zip,
            &[
                (
                    "index.json",
                    br#"{"title":"BadDict","format":3,"revision":"1"}"#,
                    None,
                ),
                (
                    "term_bank_1.json",
                    br#"[["too_few_columns"]]"#,
                    None,
                ),
            ],
        );

        let err = process_dictionary(&bad_zip, &db_path, &res_dir).unwrap_err();
        assert!(matches!(err, YomiError::MalformedBank { .. }));

        let expected_indexes = [
            "idx_kanji_bank_char",
            "idx_kanji_meta_exp",
            "idx_term_bank_combo",
            "idx_term_bank_exp",
            "idx_term_bank_reading",
            "idx_term_meta_exp",
        ];

        let query_indexes = |conn: &Connection| -> Vec<String> {
            let mut stmt = conn
                .prepare("SELECT name FROM sqlite_master WHERE type = 'index' AND name LIKE 'idx_%' ORDER BY name;")
                .unwrap();
            stmt.query_map([], |row| row.get::<_, String>(0))
                .unwrap()
                .map(Result::unwrap)
                .collect()
        };

        let conn = sqlite_db::open_read_only_connection(&db_path).unwrap();
        assert_eq!(query_indexes(&conn), expected_indexes);
        drop(conn);

        // Even if indexes were manually dropped on a v4 DB, prepare_db restores them.
        let rw = sqlite_db::open_read_write_connection(&db_path).unwrap();
        rw.execute_batch(DROP_INDEXES_SQL).unwrap();
        assert!(query_indexes(&rw).is_empty());
        drop(rw);

        prepare_db(&db_path).unwrap();
        let conn2 = sqlite_db::open_read_only_connection(&db_path).unwrap();
        assert_eq!(query_indexes(&conn2), expected_indexes);
    }

    #[test]
    fn meta_blob_int_and_float_frequencies_round_trip() {
        let root = TempDir::new().unwrap();
        let res_dir = root.path().join("res");
        let db_path = root.path().join("dictionaries.sqlite");
        let zip_path = root.path().join("freq.zip");

        build_zip(
            &zip_path,
            &[
                (
                    "index.json",
                    br#"{"title":"FreqDict","format":3,"revision":"1"}"#,
                    None,
                ),
                (
                    "term_meta_bank_1.json",
                    r#"[["食べる","freq",150],["飲む","freq",42.5]]"#.as_bytes(),
                    None,
                ),
            ],
        );

        process_dictionary(&zip_path, &db_path, &res_dir).unwrap();
        let conn = sqlite_db::open_read_only_connection(&db_path).unwrap();

        let mut rows_int = Vec::new();
        sqlite_db::get_frequencies_conn(
            &conn,
            &"食べる".encode_utf16().collect::<Vec<_>>(),
            &[],
            &mut rows_int,
        )
        .unwrap();
        assert_eq!(rows_int.len(), 1);
        assert_eq!(String::from_utf16(&rows_int[0].frequency).unwrap(), "150");

        let mut rows_float = Vec::new();
        sqlite_db::get_frequencies_conn(
            &conn,
            &"飲む".encode_utf16().collect::<Vec<_>>(),
            &[],
            &mut rows_float,
        )
        .unwrap();
        assert_eq!(rows_float.len(), 1);
        assert_eq!(String::from_utf16(&rows_float[0].frequency).unwrap(), "42.5");
    }
}

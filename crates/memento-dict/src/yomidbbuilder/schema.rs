//! SQLite schema definition, index management, and version migrations for Yomitan dictionaries.

use std::path::Path;

use rusqlite::{Connection, Transaction, TransactionBehavior};

use crate::sqlite_db;

use super::{validate_non_empty_path, YomiError};

pub(super) const CURRENT_SCHEMA_VERSION: i32 = 4;

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

// `idx_term_bank_combo` on `(expression, reading)` already covers `expression` prefix lookups,
// so the legacy redundant `idx_term_bank_exp` is omitted and dropped on migration.
// Per-table `dic_id` indexes accelerate cascading dictionary deletion via `directory_remove`.
pub(super) const CREATE_INDEXES_SQL: &str = "
CREATE INDEX IF NOT EXISTS idx_term_bank_reading ON term_bank(reading);
CREATE INDEX IF NOT EXISTS idx_term_bank_combo ON term_bank(expression, reading);
CREATE INDEX IF NOT EXISTS idx_term_bank_dic_id ON term_bank(dic_id);
CREATE INDEX IF NOT EXISTS idx_term_meta_exp ON term_meta_bank(expression, mode);
CREATE INDEX IF NOT EXISTS idx_term_meta_dic_id ON term_meta_bank(dic_id);
CREATE INDEX IF NOT EXISTS idx_kanji_bank_char ON kanji_bank(char);
CREATE INDEX IF NOT EXISTS idx_kanji_bank_dic_id ON kanji_bank(dic_id);
CREATE INDEX IF NOT EXISTS idx_kanji_meta_exp ON kanji_meta_bank(expression, mode);
CREATE INDEX IF NOT EXISTS idx_kanji_meta_dic_id ON kanji_meta_bank(dic_id);
";

pub(super) const DROP_INDEXES_SQL: &str = "
DROP INDEX IF EXISTS idx_tag_bank_name;
DROP INDEX IF EXISTS idx_term_bank_exp;
DROP INDEX IF EXISTS idx_term_bank_reading;
DROP INDEX IF EXISTS idx_term_bank_combo;
DROP INDEX IF EXISTS idx_term_bank_dic_id;
DROP INDEX IF EXISTS idx_term_meta_exp;
DROP INDEX IF EXISTS idx_term_meta_dic_id;
DROP INDEX IF EXISTS idx_kanji_bank_char;
DROP INDEX IF EXISTS idx_kanji_bank_dic_id;
DROP INDEX IF EXISTS idx_kanji_meta_exp;
DROP INDEX IF EXISTS idx_kanji_meta_dic_id;
";

const DROP_LEGACY_REDUNDANT_INDEXES_SQL: &str = "
DROP INDEX IF EXISTS idx_tag_bank_name;
DROP INDEX IF EXISTS idx_term_bank_exp;
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

pub(super) fn open_writable_db(db_file: &Path) -> Result<Connection, YomiError> {
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

pub(super) fn prepare_schema(
    conn: &mut Connection,
    defer_indexes_if_empty: bool,
) -> Result<bool, YomiError> {
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
            tx.execute_batch(DROP_LEGACY_REDUNDANT_INDEXES_SQL)?;
            if !defer_indexes_if_empty {
                tx.execute_batch(CREATE_INDEXES_SQL)?;
            }
        }
        _ => {
            tx.execute_batch(DROP_LEGACY_REDUNDANT_INDEXES_SQL)?;
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

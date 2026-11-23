//! SQLite connection setup, WAL-aware fingerprinting, schema validation, and fast cache init.

use std::collections::HashSet;
use std::path::Path;
use std::time::Duration;

use rusqlite::{Connection, OpenFlags};

use super::{CacheInfo, DictError, DictMeta, FrequencyMode, TableCounts};

const DEFAULT_BUSY_TIMEOUT: Duration = Duration::from_secs(5);
const STATEMENT_CACHE_CAPACITY: usize = 64;

const READ_ONLY_PRAGMAS: &str = "\
    PRAGMA query_only = ON;\
    PRAGMA cache_size = -12288;\
    PRAGMA mmap_size = 33554432;\
    PRAGMA temp_store = MEMORY;\
    PRAGMA synchronous = NORMAL;";

const READ_WRITE_PRAGMAS: &str = "\
    PRAGMA journal_mode = WAL;\
    PRAGMA synchronous = NORMAL;\
    PRAGMA cache_size = -12288;\
    PRAGMA mmap_size = 33554432;\
    PRAGMA temp_store = MEMORY;";

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

/// Opens a read-only `rusqlite::Connection` configured with `busy_timeout(5s)`,
/// 12 MiB page cache, 32 MiB mmap, in-memory temp store, and statement caching.
pub fn open_read_only_connection(path: &Path) -> Result<Connection, DictError> {
    if path.as_os_str().is_empty() {
        return Err(DictError::Other("Database path is empty".to_owned()));
    }
    // `rusqlite::Connection` is `!Sync` and always externally synchronized by callers, so
    // `SQLITE_OPEN_NO_MUTEX` avoids per-statement internal SQLite mutex overhead safely.
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    configure_read_only_connection(&conn)?;
    Ok(conn)
}

/// Applies busy timeout, page cache, mmap, temp store PRAGMAs, and statement cache capacity
/// on a read-only connection.
pub fn configure_read_only_connection(conn: &Connection) -> Result<(), DictError> {
    conn.busy_timeout(DEFAULT_BUSY_TIMEOUT)?;
    conn.set_prepared_statement_cache_capacity(STATEMENT_CACHE_CAPACITY);
    conn.execute_batch(READ_ONLY_PRAGMAS)?;
    Ok(())
}

/// Opens a read-write `rusqlite::Connection` configured with `WAL`,
/// `synchronous = NORMAL`, 12 MiB cache, 32 MiB mmap, `temp_store = MEMORY`,
/// and `busy_timeout(5s)`.
pub fn open_read_write_connection(path: &Path) -> Result<Connection, DictError> {
    if path.as_os_str().is_empty() {
        return Err(DictError::Other("Database path is empty".to_owned()));
    }
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE,
    )?;
    configure_read_write_connection(&conn)?;
    ensure_frequency_mode_column_conn(&conn)?;
    Ok(conn)
}

/// Checks whether `directory` exists and contains `col_name`.
pub(super) fn directory_has_column_conn(
    conn: &Connection,
    col_name: &str,
) -> Result<bool, DictError> {
    let mut stmt = conn.prepare_cached("PRAGMA table_info(directory);")?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let name: String = row.get(1)?;
        if name == col_name {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Automatically adds `directory.frequency_mode` via `ALTER TABLE` when opening an existing
/// writable v1–v4 database that predates Yomitan `frequencyMode` support (H4.4).
pub fn ensure_frequency_mode_column_conn(conn: &Connection) -> Result<(), DictError> {
    let has_directory: bool = conn
        .prepare_cached(
            "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'directory');",
        )?
        .query_row([], |row| row.get::<_, i64>(0))?
        != 0;
    if has_directory && !directory_has_column_conn(conn, "frequency_mode")? {
        conn.execute_batch(
            "ALTER TABLE directory ADD COLUMN frequency_mode TEXT NOT NULL DEFAULT 'rank-based';",
        )?;
    }
    Ok(())
}

/// Applies WAL, synchronous, cache_size, mmap_size, temp_store, and busy_timeout PRAGMAs
/// on a writable connection.
pub fn configure_read_write_connection(conn: &Connection) -> Result<(), DictError> {
    conn.busy_timeout(DEFAULT_BUSY_TIMEOUT)?;
    conn.set_prepared_statement_cache_capacity(STATEMENT_CACHE_CAPACITY);
    conn.execute_batch(READ_WRITE_PRAGMAS)?;
    Ok(())
}

fn query_table_has_rows(conn: &Connection, sql: &str) -> Result<i64, DictError> {
    let mut stmt = conn.prepare_cached(sql)?;
    let exists: i64 = stmt.query_row([], |row| row.get(0))?;
    Ok(exists)
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

/// Releases idle SQLite page-cache and scratch memory on `conn` via `PRAGMA shrink_memory;`.
pub fn shrink_memory_conn(conn: &Connection) -> Result<(), DictError> {
    conn.execute_batch("PRAGMA shrink_memory;")?;
    Ok(())
}

/// Validates database schema, reads dictionary metadata, and checks table non-emptiness
/// in O(1) via `EXISTS (SELECT 1 FROM ... LIMIT 1)` on a `rusqlite::Connection`.
pub fn init_cache_conn(conn: &Connection) -> Result<CacheInfo, DictError> {
    validate_schema_conn(conn)?;

    let has_freq_mode = directory_has_column_conn(conn, "frequency_mode")?;
    let mut dictionaries = Vec::new();
    if has_freq_mode {
        let mut dict_stmt = conn.prepare_cached(
            "SELECT dic_id, title, \
             EXISTS (SELECT 1 FROM dict_disabled WHERE dict_disabled.dic_id = directory.dic_id) AS disabled, \
             COALESCE(frequency_mode, 'rank-based') \
             FROM directory;",
        )?;
        let dict_rows = dict_stmt.query_map([], |row| {
            let mode_str: String = row.get(3)?;
            Ok(DictMeta {
                id: row.get(0)?,
                title: row.get(1)?,
                disabled: row.get::<_, i64>(2)? != 0,
                frequency_mode: FrequencyMode::from_db_str(&mode_str),
            })
        })?;
        for d_res in dict_rows {
            dictionaries.push(d_res?);
        }
    } else {
        let mut dict_stmt = conn.prepare_cached(
            "SELECT dic_id, title, EXISTS (SELECT 1 FROM dict_disabled WHERE dict_disabled.dic_id = directory.dic_id) AS disabled FROM directory;",
        )?;
        let dict_rows = dict_stmt.query_map([], |row| {
            Ok(DictMeta {
                id: row.get(0)?,
                title: row.get(1)?,
                disabled: row.get::<_, i64>(2)? != 0,
                frequency_mode: FrequencyMode::RankBased,
            })
        })?;
        for d_res in dict_rows {
            dictionaries.push(d_res?);
        }
    }

    let table_counts = TableCounts {
        directory: query_table_has_rows(conn, "SELECT EXISTS (SELECT 1 FROM directory LIMIT 1);")?,
        tag_bank: query_table_has_rows(conn, "SELECT EXISTS (SELECT 1 FROM tag_bank LIMIT 1);")?,
        term_bank: query_table_has_rows(conn, "SELECT EXISTS (SELECT 1 FROM term_bank LIMIT 1);")?,
        term_meta_bank: query_table_has_rows(
            conn,
            "SELECT EXISTS (SELECT 1 FROM term_meta_bank LIMIT 1);",
        )?,
        kanji_bank: query_table_has_rows(
            conn,
            "SELECT EXISTS (SELECT 1 FROM kanji_bank LIMIT 1);",
        )?,
        kanji_meta_bank: query_table_has_rows(
            conn,
            "SELECT EXISTS (SELECT 1 FROM kanji_meta_bank LIMIT 1);",
        )?,
    };

    Ok(CacheInfo {
        dictionaries,
        table_counts,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shrink_memory_conn_succeeds_on_temp_sqlite_db_and_preserves_queries() {
        let dir = tempfile::TempDir::new().unwrap();
        let db_path = dir.path().join("shrink_test.sqlite");

        {
            let rw = open_read_write_connection(&db_path).unwrap();
            rw.execute_batch(
                "CREATE TABLE kv (k INTEGER PRIMARY KEY, v TEXT NOT NULL);",
            )
            .unwrap();
            let mut stmt = rw
                .prepare("INSERT INTO kv (k, v) VALUES (?1, ?2);")
                .unwrap();
            for i in 0..500i64 {
                stmt.execute(rusqlite::params![i, format!("payload-{i:04}-{}", "x".repeat(64))])
                    .unwrap();
            }
            drop(stmt);
            shrink_memory_conn(&rw).unwrap();
        }

        let ro = open_read_only_connection(&db_path).unwrap();
        let count_before: i64 = ro
            .query_row("SELECT COUNT(*) FROM kv;", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count_before, 500);

        shrink_memory_conn(&ro).unwrap();

        let count_after: i64 = ro
            .query_row("SELECT COUNT(*) FROM kv;", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count_after, 500);
    }
}

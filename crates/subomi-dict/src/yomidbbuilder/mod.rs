//! Pure-Rust safe Yomichan/Yomitan ZIP importer.
//!
//! Preserves Subomi's SQLite schema and binary metadata BLOB layouts while
//! streaming JSON banks without full-file DOM materialization and coordinating
//! resource directory swaps atomically with the database transaction commit.

mod archive;
mod banks;
mod schema;

use std::fmt;
use std::fs::{self, File};
use std::io::{BufReader, ErrorKind};
use std::path::{Path, PathBuf};

use rusqlite::{params, TransactionBehavior};
use zip::ZipArchive;

pub use self::archive::{MAX_ENTRY_BYTES, MAX_TOTAL_BYTES};
use self::archive::{
    extract_resources_to_staging, safe_remove_dict_resource_dir, validate_archive_entries,
    SizeLimits,
};
use self::banks::{add_bank, add_index, Bank};
use self::schema::{open_writable_db, prepare_schema, CREATE_INDEXES_SQL, CURRENT_SCHEMA_VERSION};

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
                write!(
                    f,
                    "dictionary database schema version {v} is newer than supported ({CURRENT_SCHEMA_VERSION})"
                )
            }
            Self::UnsupportedFormat(fmt_ver) => {
                write!(
                    f,
                    "unsupported Yomitan dictionary format {fmt_ver} (expected 3)"
                )
            }
            Self::MalformedIndex(msg) => write!(f, "malformed index.json: {msg}"),
            Self::MalformedBank { bank, detail } => write!(f, "malformed {bank} entry: {detail}"),
            Self::UnsafeArchiveEntry { entry, reason } => {
                write!(f, "unsafe archive entry '{entry}': {reason}")
            }
            Self::EntryTooLarge { entry, limit } => {
                write!(
                    f,
                    "archive entry '{entry}' exceeds decompression limit of {limit} bytes"
                )
            }
            Self::ArchiveTooLarge { limit } => {
                write!(
                    f,
                    "archive total uncompressed size exceeds limit of {limit} bytes"
                )
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

pub(super) fn validate_non_empty_path(path: &Path) -> Result<(), YomiError> {
    if path.as_os_str().is_empty() {
        return Err(YomiError::InvalidPath {
            path: path.to_path_buf(),
            reason: "path is empty",
        });
    }
    Ok(())
}

/// Initializes or migrates the Yomitan SQLite schema at `db_file`.
pub fn prepare_db(db_file: impl AsRef<Path>) -> Result<(), YomiError> {
    let mut conn = open_writable_db(db_file.as_ref())?;
    let _ = prepare_schema(&mut conn, false)?;
    Ok(())
}

/// Imports a Yomichan/Yomitan `.zip` dictionary into `db_file` and extracts
/// non-bank resource files under `{res_dir}/dict-{dic_id}`.
pub fn process_dictionary(
    dict_file: impl AsRef<Path>,
    db_file: impl AsRef<Path>,
    res_dir: impl AsRef<Path>,
) -> Result<i64, YomiError> {
    process_dictionary_with_limits(
        dict_file.as_ref(),
        db_file.as_ref(),
        res_dir.as_ref(),
        SizeLimits::default(),
    )
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
        .prefix(".subomi-import-")
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

        // Coordinate resource directory swap with the DB commit: move any existing
        // `dict-{dic_id}` aside into a temporary backup folder, rename staging into
        // place before `tx.commit()`, and roll back the filesystem swap if commit fails.
        let target_dir = dictionary_resource_dir(&canon_res_dir, dic_id)?;
        let mut backup_holder: Option<(tempfile::TempDir, PathBuf)> = None;
        match fs::symlink_metadata(&target_dir) {
            Ok(_) => {
                let holder = tempfile::Builder::new()
                    .prefix(".subomi-backup-")
                    .tempdir_in(&canon_res_dir)
                    .map_err(|source| YomiError::Io {
                        operation: "create resource backup directory",
                        path: canon_res_dir.clone(),
                        source,
                    })?;
                let backup_path = holder.path().join("prev");
                fs::rename(&target_dir, &backup_path).map_err(|source| YomiError::Io {
                    operation: "stage existing dictionary resource directory",
                    path: target_dir.clone(),
                    source,
                })?;
                backup_holder = Some((holder, backup_path));
            }
            Err(err) if err.kind() == ErrorKind::NotFound => {}
            Err(source) => {
                return Err(YomiError::Io {
                    operation: "inspect dictionary resource directory",
                    path: target_dir,
                    source,
                });
            }
        }

        let staging_path = staging.keep();
        if let Err(source) = fs::rename(&staging_path, &target_dir) {
            let _ = fs::remove_dir_all(&staging_path);
            if let Some((_holder, backup_path)) = backup_holder.take() {
                let _ = fs::rename(&backup_path, &target_dir);
            }
            return Err(YomiError::Io {
                operation: "install dictionary resource directory",
                path: target_dir,
                source,
            });
        }

        if let Err(err) = tx.commit() {
            let _ = fs::remove_dir_all(&target_dir);
            if let Some((_holder, backup_path)) = backup_holder.take() {
                let _ = fs::rename(&backup_path, &target_dir);
            }
            return Err(YomiError::Sqlite(err));
        }

        // DB commit succeeded; dropping `backup_holder` cleans up any replaced `dict-{dic_id}`.
        drop(backup_holder);
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

    if let Some(old_id) = replaced_dic_id {
        if old_id != dic_id {
            let _ = safe_remove_dict_resource_dir(&canon_res_dir, old_id);
        }
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
    use super::schema::DROP_INDEXES_SQL;
    use super::*;
    use crate::sqlite_db;
    use rusqlite::Connection;
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
        assert!(matches!(err, YomiError::UnsafeArchiveEntry { .. }));
        assert!(!escaped_target.exists());
        assert!(!db_path.exists());
    }

    #[test]
    fn import_accepts_version_without_format_float_ints_nullable_term_tags_and_duplicate_tags() {
        let root = TempDir::new().unwrap();
        let res_dir = root.path().join("res");
        let db_path = root.path().join("dictionaries.sqlite");
        let zip_path = root.path().join("compat.zip");

        build_zip(
            &zip_path,
            &[
                (
                    "index.json",
                    br#"{"title":"CompatDict","version":3.0,"revision":"2026"}"#,
                    None,
                ),
                (
                    "tag_bank_1.json",
                    br#"[["v1","partOfSpeech",1.0,"first",10.0],["v1","partOfSpeech",2.0,"second",20.0]]"#,
                    None,
                ),
                (
                    "term_bank_1.json",
                    r#"[["食べる","たべる",null,"v1",100.0,["to eat"],1.0,null]]"#.as_bytes(),
                    None,
                ),
            ],
        );

        let dic_id = process_dictionary(&zip_path, &db_path, &res_dir).unwrap();
        assert_eq!(dic_id, 1);

        let conn = sqlite_db::open_read_only_connection(&db_path).unwrap();
        let cache = sqlite_db::init_cache_conn(&conn).unwrap();
        assert_eq!(cache.table_counts.tag_bank, 1);
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
                ("term_bank_1.json", br#"[["too_few_columns"]]"#, None),
            ],
        );

        let err = process_dictionary(&bad_zip, &db_path, &res_dir).unwrap_err();
        assert!(matches!(err, YomiError::MalformedBank { .. }));

        let expected_indexes = [
            "idx_kanji_bank_char",
            "idx_kanji_bank_dic_id",
            "idx_kanji_meta_dic_id",
            "idx_kanji_meta_exp",
            "idx_term_bank_combo",
            "idx_term_bank_dic_id",
            "idx_term_bank_reading",
            "idx_term_meta_dic_id",
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

        let rows = sqlite_db::query_term_meta_batch_conn(
            &conn,
            &["食べる".to_owned(), "飲む".to_owned()],
        )
        .unwrap();
        assert_eq!(rows.len(), 2);
        let eat = rows.iter().find(|r| r.expression == "食べる").unwrap();
        let parsed_int = sqlite_db::parse_freq_blob_utf8(eat.blob_type, &eat.data, "食べる", "");
        assert!(parsed_int.matched);
        assert_eq!(parsed_int.display, "150");

        let drink = rows.iter().find(|r| r.expression == "飲む").unwrap();
        let parsed_float =
            sqlite_db::parse_freq_blob_utf8(drink.blob_type, &drink.data, "飲む", "");
        assert!(parsed_float.matched);
        assert_eq!(parsed_float.display, "42.5");
    }

    #[test]
    fn frequency_mode_persists_and_migrates_legacy_v4_databases() {
        let root = TempDir::new().unwrap();
        let res_dir = root.path().join("res");
        let db_path = root.path().join("dictionaries.sqlite");

        // 1. Create a legacy v4 database WITHOUT `directory.frequency_mode`.
        {
            let raw = Connection::open(&db_path).unwrap();
            raw.execute_batch(
                "
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
                INSERT INTO directory VALUES (1, 'LegacyRankDict', 3, '1', 0);
                PRAGMA user_version = 4;
                ",
            )
            .unwrap();
        }

        // Read-only init on unmigrated v4 DB defaults cleanly to RankBased:
        {
            let ro = sqlite_db::open_read_only_connection(&db_path).unwrap();
            let cache = sqlite_db::init_cache_conn(&ro).unwrap();
            assert_eq!(cache.dictionaries.len(), 1);
            assert_eq!(
                cache.dictionaries[0].frequency_mode,
                sqlite_db::FrequencyMode::RankBased
            );
        }

        // Opening an existing v4 DB read-write automatically runs ALTER TABLE for `frequency_mode`:
        {
            let rw = sqlite_db::open_read_write_connection(&db_path).unwrap();
            let mode: String = rw
                .query_row(
                    "SELECT frequency_mode FROM directory WHERE dic_id = 1;",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(mode, "rank-based");
        }

        // Importing an occurrence-based dictionary automatically migrates v4 -> v5:
        let occ_zip = root.path().join("occ.zip");
        build_zip(
            &occ_zip,
            &[
                (
                    "index.json",
                    br#"{"title":"OccFreq","format":3,"revision":"1","frequencyMode":"occurrence-based"}"#,
                    None,
                ),
                (
                    "term_meta_bank_1.json",
                    r#"[["食べる","freq",98765]]"#.as_bytes(),
                    None,
                ),
            ],
        );
        let occ_id = process_dictionary(&occ_zip, &db_path, &res_dir).unwrap();

        let conn = sqlite_db::open_read_only_connection(&db_path).unwrap();
        let cache = sqlite_db::init_cache_conn(&conn).unwrap();
        let legacy = cache
            .dictionaries
            .iter()
            .find(|d| d.title == "LegacyRankDict")
            .unwrap();
        assert_eq!(legacy.frequency_mode, sqlite_db::FrequencyMode::RankBased);
        let occ = cache
            .dictionaries
            .iter()
            .find(|d| d.id == occ_id)
            .unwrap();
        assert_eq!(occ.frequency_mode, sqlite_db::FrequencyMode::OccurrenceBased);

        let modes = sqlite_db::query_dictionary_frequency_modes_conn(&conn).unwrap();
        assert_eq!(modes.get(&1), Some(&sqlite_db::FrequencyMode::RankBased));
        assert_eq!(
            modes.get(&occ_id),
            Some(&sqlite_db::FrequencyMode::OccurrenceBased)
        );

        let freqs =
            sqlite_db::query_term_frequencies_with_mode_conn(&conn, "食べる", "たべる").unwrap();
        assert_eq!(freqs.len(), 1);
        assert_eq!(
            freqs[0].frequency_mode,
            sqlite_db::FrequencyMode::OccurrenceBased
        );
        assert_eq!(
            freqs[0].parsed.frequency_mode,
            sqlite_db::FrequencyMode::OccurrenceBased
        );
        assert_eq!(freqs[0].parsed.display, "98765");
    }

    #[test]
    fn string_pitch_patterns_and_kanji_bank_pr_2377_nulls_and_5_elem_rows_round_trip() {
        let root = TempDir::new().unwrap();
        let res_dir = root.path().join("res");
        let db_path = root.path().join("dictionaries.sqlite");
        let zip_path = root.path().join("pitch_and_kanji.zip");

        build_zip(
            &zip_path,
            &[
                (
                    "index.json",
                    br#"{"title":"PitchAndKanji","format":3,"revision":"1"}"#,
                    None,
                ),
                (
                    "term_meta_bank_1.json",
                    r#"[["雨","pitch",{"reading":"あめ","pitches":[{"position":"LH"},{"position":"HL"},{"position":1}]}]]"#
                        .as_bytes(),
                    None,
                ),
                (
                    "kanji_bank_1.json",
                    r#"[
                        ["漢", null, null, null, ["China"], null],
                        ["字", "ジ", null, "jouyou", ["character"]],
                        ["語", "ゴ", "かた.る", "", null, {}]
                    ]"#
                    .as_bytes(),
                    None,
                ),
            ],
        );

        process_dictionary(&zip_path, &db_path, &res_dir).unwrap();
        let conn = sqlite_db::open_read_only_connection(&db_path).unwrap();

        // String-based exact pitch patterns round-trip through SQLite intact (H6.1 / PR #2055):
        let meta = sqlite_db::query_term_meta_batch_conn(&conn, &["雨".to_owned()]).unwrap();
        assert_eq!(meta.len(), 1);
        let null_pos = meta[0]
            .data
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(meta[0].data.len());
        let parsed = crate::databasemanager::parse_pitch_blob_utf8(
            &meta[0].data[..null_pos],
            "雨",
            "あめ",
        );
        assert!(parsed.matched);
        assert_eq!(parsed.entries.len(), 3);
        assert_eq!(
            parsed.entries[0].pitch_position(),
            Some(sqlite_db::PitchPosition::Pattern("LH".to_owned()))
        );
        assert_eq!(
            parsed.entries[1].pitch_position(),
            Some(sqlite_db::PitchPosition::Pattern("HL".to_owned()))
        );
        assert_eq!(
            parsed.entries[2].pitch_position(),
            Some(sqlite_db::PitchPosition::Downstep(1))
        );

        // PR #2377 kanji_bank nullable fields & 5-element rows:
        let k1 = sqlite_db::query_kanji_utf8_conn(&conn, "漢").unwrap();
        assert_eq!(k1.len(), 1);
        assert_eq!(k1[0].onyomi, "");
        assert_eq!(k1[0].kunyomi, "");
        assert_eq!(k1[0].tags, "");
        assert_eq!(k1[0].meanings, vec!["China"]);
        assert_eq!(k1[0].stats, "{}");

        let k2 = sqlite_db::query_kanji_utf8_conn(&conn, "字").unwrap();
        assert_eq!(k2.len(), 1);
        assert_eq!(k2[0].onyomi, "ジ");
        assert_eq!(k2[0].kunyomi, "");
        assert_eq!(k2[0].meanings, vec!["character"]);
        assert_eq!(k2[0].stats, "{}");

        let k3 = sqlite_db::query_kanji_utf8_conn(&conn, "語").unwrap();
        assert_eq!(k3.len(), 1);
        assert!(k3[0].meanings.is_empty());
        assert_eq!(k3[0].stats, "{}");
    }
}

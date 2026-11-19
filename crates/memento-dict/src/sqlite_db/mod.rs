//! Pure Rust SQLite Yomichan/Yomitan query engine, batched UTF-8 helpers, and cache validator.

mod connection;
mod freq;
mod queries;

use std::fmt;

pub use rusqlite;
pub use rusqlite::ffi::{sqlite3, sqlite3_exec, sqlite3_open_v2, SQLITE_OK};

/// Closes a raw SQLite handle via `sqlite3_close_v2` (retained solely for integration test fixtures).
///
/// # Safety
/// `db` must be null or a valid SQLite handle opened by `sqlite3_open_v2`.
pub unsafe fn sqlite3_close_v2(db: *mut sqlite3) -> std::ffi::c_int {
    unsafe extern "C" {
        fn sqlite3_close_v2(db: *mut sqlite3) -> std::ffi::c_int;
    }
    unsafe { sqlite3_close_v2(db) }
}

pub use crate::databasemanager::PitchEntryDetail;

pub use self::connection::{
    configure_read_only_connection, configure_read_write_connection, db_file_fingerprint,
    init_cache_conn, open_read_only_connection, open_read_write_connection,
    query_data_version_conn, validate_schema_conn, DbFileFingerprint, FileMetaStamp,
};
pub use self::freq::{format_g, parse_freq_blob_utf8, ParsedFreqUtf8};
pub use self::queries::{
    get_kanji_frequencies_conn, query_kanji_meta_freq_utf8_conn, query_kanji_utf8_conn,
    query_term_details_batch_conn, query_term_meta_batch_conn, query_term_rules_conn,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DictError {
    Sqlite { code: i32, message: String },
    InvalidSchema(String),
    Json(String),
    Other(String),
}

impl fmt::Display for DictError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Sqlite { code, message } => write!(f, "SQLite error {code}: {message}"),
            Self::InvalidSchema(msg) => write!(f, "Invalid schema: {msg}"),
            Self::Json(msg) => write!(f, "JSON error: {msg}"),
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

#[cfg(test)]
mod tests {
    use rusqlite::{params, Connection};

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
    fn test_batch_term_and_rules_queries() {
        let db = create_test_db();
        db.execute_batch(
            "
            INSERT INTO directory VALUES (1, 'TestDict', 3, 'rev1', 0);
            INSERT INTO term_bank VALUES (1, '食べる', 'たべる', '', 'v1', 10, '[\"to eat\"]', 1, '');
            INSERT INTO term_bank VALUES (1, 'カタカナ', 'かたかな', '', '', 5, '[\"katakana\"]', 2, '');
            ",
        )
        .unwrap();

        let batch =
            query_term_details_batch_conn(&db, &["たべる".to_owned(), "カタカナ".to_owned()])
                .unwrap();
        assert_eq!(batch.len(), 2);
        assert_eq!(batch[0].expression, "食べる");
        assert_eq!(batch[0].rules, "v1");
        assert_eq!(batch[1].expression, "カタカナ");
        assert_eq!(batch[1].rules, "");
        assert_eq!(
            query_term_rules_conn(&db, "食べる", "たべる").unwrap(),
            Some(vec!["v1".to_owned()])
        );
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
        let term_rows = query_term_meta_batch_conn(&db, &["字".to_owned()]).unwrap();
        assert!(term_rows.is_empty(), "term query reads only term_meta_bank");

        let mut rows = Vec::new();
        get_kanji_frequencies_conn(&db, &to_u16("字"), &mut rows).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(String::from_utf16_lossy(&rows[0].frequency), "kanji rank");
    }

    #[test]
    fn json_frequency_numbers_format_consistently() {
        let db = create_test_db();
        db.execute_batch(
            "INSERT INTO term_meta_bank (dic_id, expression, mode, type, data) \
             VALUES (1, '語', 'freq', 4, '{\"frequency\":1234567}'), \
                    (2, '語', 'freq', 4, '{\"frequency\":{\"value\":123456789}}'), \
                    (3, '語', 'freq', 4, '{\"value\":1234567}');",
        )
        .unwrap();
        let rows = query_term_meta_batch_conn(&db, &["語".to_owned()]).unwrap();
        let displays: Vec<String> = rows
            .iter()
            .map(|r| parse_freq_blob_utf8(r.blob_type, &r.data, "語", "").display)
            .collect();
        assert_eq!(displays, ["1.23457e+06", "1.23457e+08", "1.23457e+06"]);
    }

    #[test]
    fn test_query_kanji_utf8() {
        let db = create_test_db();
        db.execute_batch(
            "
            INSERT INTO directory VALUES (1, 'KanjiDict', 3, 'rev1', 0);
            INSERT INTO kanji_bank VALUES (1, '食', 'ショク', 'た.べる', 'grade:2', '[\"eat\", \"food\"]', '{\"strokes\": \"9\"}');
            ",
        )
        .unwrap();

        let results = query_kanji_utf8_conn(&db, "食").unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].dic_id, 1);
        assert_eq!(results[0].onyomi, "ショク");
        assert_eq!(results[0].kunyomi, "た.べる");
        assert_eq!(results[0].tags, "grade:2");
        assert_eq!(results[0].meanings, vec!["eat", "food"]);
        assert_eq!(results[0].stats, "{\"strokes\": \"9\"}");
    }

    #[test]
    fn test_freq_and_pitch_utf8_homograph_guard() {
        let db = create_test_db();
        db.execute_batch(
            "
            INSERT INTO directory VALUES (1, 'JMdict', 3, 'rev1', 0);
            INSERT INTO term_meta_bank VALUES (1, '食べる', 'freq', 4, '{\"reading\": \"たべる\", \"frequency\": 120}');
            INSERT INTO term_meta_bank VALUES (1, '人気', 'freq', 4, '{\"reading\": \"にんき\", \"frequency\": 300}');
            ",
        )
        .unwrap();

        let rows =
            query_term_meta_batch_conn(&db, &["食べる".to_owned(), "人気".to_owned()]).unwrap();
        let eat = rows.iter().find(|r| r.expression == "食べる").unwrap();
        let parsed = parse_freq_blob_utf8(eat.blob_type, &eat.data, "食べる", "たべる");
        assert!(parsed.matched);
        assert_eq!(parsed.display, "120");

        let ninki = rows.iter().find(|r| r.expression == "人気").unwrap();
        assert!(!parse_freq_blob_utf8(ninki.blob_type, &ninki.data, "人気", "ひとけ").matched);
        assert!(!parse_freq_blob_utf8(ninki.blob_type, &ninki.data, "人気", "").matched);
    }

    #[test]
    fn test_format_g() {
        assert_eq!(format_g(1234.5, 6), "1234.5");
        assert_eq!(format_g(1234.5678, 6), "1234.57");
        assert_eq!(format_g(100000.0, 6), "100000");
        assert_eq!(format_g(0.00001, 6), "1e-05");
        assert_eq!(format_g(1000000.0, 6), "1e+06");
        assert_eq!(format_g(0.0, 6), "0");
    }

    #[test]
    fn freq_blob_supports_signed_int_and_float_tags() {
        let db = create_test_db();
        db.execute(
            "INSERT INTO term_meta_bank (dic_id, expression, mode, type, data) VALUES (1, '語', 'freq', 1, ?1);",
            params![420i64.to_le_bytes().to_vec()],
        )
        .unwrap();
        db.execute(
            "INSERT INTO term_meta_bank (dic_id, expression, mode, type, data) VALUES (2, '語', 'freq', 1, ?1);",
            params![(-5i64).to_le_bytes().to_vec()],
        )
        .unwrap();
        db.execute(
            "INSERT INTO term_meta_bank (dic_id, expression, mode, type, data) VALUES (3, '語', 'freq', 3, ?1);",
            params![123.45f64.to_le_bytes().to_vec()],
        )
        .unwrap();

        let rows = query_term_meta_batch_conn(&db, &["語".to_owned()]).unwrap();
        let formatted: Vec<(i64, String)> = rows
            .into_iter()
            .filter_map(|r| {
                let p = parse_freq_blob_utf8(r.blob_type, &r.data, "語", "");
                p.matched.then_some((r.dic_id, p.display))
            })
            .collect();
        assert_eq!(
            formatted,
            vec![(1, "420".to_string()), (3, "123.45".to_string())]
        );
    }
}

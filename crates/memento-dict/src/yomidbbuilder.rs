//! Yomichan/Yomitan ZIP importer. The on-disk schema, json-c formatting and
//! binary metadata blobs intentionally remain compatible with yomidbbuilder.c.

#[path = "yomidbbuilder_sys.rs"]
mod sys;

use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::io::Write;
use std::ptr;

use sys::*;

const YOMI_ERR_OPENING_DIC: c_int = 1;
const YOMI_ERR_DB: c_int = 2;
const YOMI_ERR_NEWER_VERSION: c_int = 3;
const YOMI_ERR_ADDING_INDEX: c_int = 4;
const YOMI_ERR_ADDING_TAGS: c_int = 5;
const YOMI_ERR_ADDING_TERMS: c_int = 6;
const YOMI_ERR_ADDING_TERMS_META: c_int = 7;
const YOMI_ERR_ADDING_KANJI: c_int = 8;
const YOMI_ERR_ADDING_KANJI_META: c_int = 9;
const YOMI_ERR_DELETE: c_int = 10;
const YOMI_ERR_EXTRACTING_RESOURCES: c_int = 11;
const YOMI_ERR_REMOVING_RESOURCES: c_int = 12;

const STAT_ERR: c_int = -1;
const INVALID_SIZE_ERR: c_int = -2;
const MALLOC_FAILURE_ERR: c_int = -3;
const ZIP_FILE_OPEN_ERR: c_int = -4;
const ZIP_FILE_READ_ERR: c_int = -5;
const UNSUPPORTED_FORMAT_ERR: c_int = -6;
const JSON_WRONG_TYPE_ERR: c_int = -7;
const JSON_MISSING_KEY_ERR: c_int = -8;
const STATEMENT_PREPARE_ERR: c_int = -9;
const STATEMENT_BIND_ERR: c_int = -10;
const STATEMENT_STEP_ERR: c_int = -11;
const DB_NEW_VERSION_ERR: c_int = -12;
const DB_TABLE_DROP_ERR: c_int = -14;
const DB_CREATE_TABLE_ERR: c_int = -15;
const TAG_WRONG_SIZE_ERR: c_int = -16;
const TERM_WRONG_SIZE_ERR: c_int = -17;
const KANJI_WRONG_SIZE_ERR: c_int = -18;
const META_WRONG_SIZE_ERR: c_int = -19;
const PRAGMA_SET_ERR: c_int = -22;
const TRANSACTION_ERR: c_int = -23;
const DB_ALTER_TABLE_ERR: c_int = -24;
const JSON_PARSE_ERROR: c_int = -25;

struct Database(*mut Sqlite3);

impl Drop for Database {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { sqlite3_close_v2(self.0) };
        }
    }
}

struct Archive(*mut Zip);

impl Drop for Archive {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { zip_close(self.0) };
        }
    }
}

struct Statement(*mut SqliteStmt);

impl Drop for Statement {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { sqlite3_finalize(self.0) };
        }
    }
}

struct Json(*mut JsonObject);

impl Drop for Json {
    fn drop(&mut self) {
        unsafe { json_object_put(self.0) };
    }
}

struct ZipEntry(*mut ZipFile);

impl Drop for ZipEntry {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { zip_fclose(self.0) };
        }
    }
}

// sqlite3_exec owns its error string (unlike sqlite3_errmsg).
unsafe fn exec(db: *mut Sqlite3, query: &CStr) -> bool {
    let mut error = ptr::null_mut();
    let status = unsafe { sqlite3_exec(db, query.as_ptr(), None, ptr::null_mut(), &mut error) };
    if !error.is_null() {
        unsafe { sqlite3_free(error.cast()) };
    }
    status == SQLITE_OK
}

unsafe fn statement(db: *mut Sqlite3, sql: &CStr) -> Result<Statement, c_int> {
    let mut result = ptr::null_mut();
    let code = unsafe { sqlite3_prepare_v2(db, sql.as_ptr(), -1, &mut result, ptr::null_mut()) };
    if code != SQLITE_OK {
        drop(Statement(result));
        return Err(STATEMENT_PREPARE_ERR);
    }
    Ok(Statement(result))
}

unsafe fn insert(
    db: *mut Sqlite3,
    sql: &CStr,
    bind: impl FnOnce(*mut SqliteStmt) -> bool,
) -> Result<(), c_int> {
    let stmt = unsafe { statement(db, sql)? };
    if !bind(stmt.0) {
        return Err(STATEMENT_BIND_ERR);
    }
    if unsafe { sqlite3_step(stmt.0) } != SQLITE_DONE {
        return Err(STATEMENT_STEP_ERR);
    }
    Ok(())
}

unsafe fn text(stmt: *mut SqliteStmt, field: c_int, value: *const c_char) -> bool {
    unsafe { sqlite3_bind_text(stmt, field, value, -1, None) == SQLITE_OK }
}

unsafe fn int(stmt: *mut SqliteStmt, field: c_int, value: c_int) -> bool {
    unsafe { sqlite3_bind_int(stmt, field, value) == SQLITE_OK }
}

// A failed DROP is deliberately ignored, as in the original callback. SQLite
// reports errors in the outer SELECT independently of the DROP statements.
unsafe extern "C" fn drop_table(
    database: *mut c_void, count: c_int, names: *mut *mut c_char, _: *mut *mut c_char,
) -> c_int {
    for index in 0..count {
        let name = unsafe { *names.add(index as usize) };
        if name.is_null() || unsafe { CStr::from_ptr(name) }.to_bytes().starts_with(b"sqlite_") {
            continue;
        }
        let sql = unsafe { sqlite3_mprintf(c"DROP TABLE IF EXISTS %Q;".as_ptr(), name) };
        if sql.is_null() {
            return MALLOC_FAILURE_ERR;
        }
        let mut error = ptr::null_mut();
        unsafe { sqlite3_exec(database.cast(), sql, None, ptr::null_mut(), &mut error) };
        unsafe { sqlite3_free(sql.cast()) };
        if !error.is_null() {
            unsafe { sqlite3_free(error.cast()) };
        }
    }
    0
}

const CREATE_SQL: &CStr = c"
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
CREATE INDEX idx_tag_bank_name ON tag_bank(dic_id, name);
CREATE TABLE term_bank (
    dic_id INTEGER NOT NULL, expression TEXT NOT NULL, reading TEXT NOT NULL,
    def_tags TEXT NOT NULL, rules TEXT NOT NULL, score INTEGER NOT NULL,
    glossary TEXT NOT NULL, sequence INTEGER NOT NULL, term_tags TEXT NOT NULL
);
CREATE INDEX idx_term_bank_exp ON term_bank(expression);
CREATE INDEX idx_term_bank_reading ON term_bank(reading);
CREATE INDEX idx_term_bank_combo ON term_bank(expression, reading);
CREATE TABLE term_meta_bank (
    dic_id INTEGER NOT NULL, expression TEXT NOT NULL, mode TEXT NOT NULL,
    type INTEGER NOT NULL, data BLOB
);
CREATE INDEX idx_term_meta_exp ON term_meta_bank(expression, mode);
CREATE TABLE kanji_bank (
    dic_id INTEGER NOT NULL, char TEXT NOT NULL, onyomi TEXT NOT NULL,
    kunyomi TEXT NOT NULL, tags TEXT NOT NULL, meanings TEXT NOT NULL, stats TEXT NOT NULL
);
CREATE INDEX idx_kanji_bank_char ON kanji_bank(char);
CREATE TABLE kanji_meta_bank (
    dic_id INTEGER NOT NULL, expression TEXT NOT NULL, mode TEXT NOT NULL,
    type INTEGER NOT NULL, data BLOB
);
CREATE INDEX idx_kanji_meta_exp ON kanji_meta_bank(expression, mode);
";

const V1_TO_V2: &CStr = c"
ALTER TABLE term_meta_bank ADD type INTEGER NOT NULL DEFAULT 0;
ALTER TABLE kanji_meta_bank ADD type INTEGER NOT NULL DEFAULT 0;
UPDATE term_meta_bank SET type = 4 WHERE mode = 'pitch';
UPDATE kanji_meta_bank SET type = 4 WHERE mode = 'pitch';
UPDATE term_meta_bank SET type = 1 WHERE mode = 'freq';
UPDATE kanji_meta_bank SET type = 1 WHERE mode = 'freq';
PRAGMA user_version = 2;
";

const V2_TO_V3: &CStr = c"
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

const V3_TO_V4: &CStr = c"UPDATE term_bank SET reading = '' WHERE expression = reading;
PRAGMA user_version = 4;";

unsafe fn create_db(db: *mut Sqlite3) -> Result<(), c_int> {
    let mut error = ptr::null_mut();
    let code = unsafe {
        sqlite3_exec(db, c"SELECT name FROM sqlite_master WHERE type='table'".as_ptr(),
                     Some(drop_table), db.cast(), &mut error)
    };
    if !error.is_null() {
        unsafe { sqlite3_free(error.cast()) };
    }
    if code != SQLITE_OK {
        return Err(DB_TABLE_DROP_ERR);
    }
    if !unsafe { exec(db, CREATE_SQL) } {
        return Err(DB_CREATE_TABLE_ERR);
    }
    if !unsafe { exec(db, c"PRAGMA user_version = 4;") } {
        return Err(DB_CREATE_TABLE_ERR);
    }
    Ok(())
}

unsafe fn prepare_db(db: *mut Sqlite3) -> Result<(), c_int> {
    let stmt = unsafe { statement(db, c"PRAGMA user_version;")? };
    if unsafe { sqlite3_step(stmt.0) } != SQLITE_ROW {
        return Err(STATEMENT_STEP_ERR);
    }
    let version = unsafe { sqlite3_column_int(stmt.0, 0) };
    drop(stmt);
    if version > 4 {
        return Err(DB_NEW_VERSION_ERR);
    }
    match version {
        0 => unsafe { create_db(db)? },
        1..=3 => {
            if version <= 1 && !unsafe { exec(db, V1_TO_V2) } {
                return Err(DB_ALTER_TABLE_ERR);
            }
            if version <= 2 && !unsafe { exec(db, V2_TO_V3) } {
                return Err(DB_ALTER_TABLE_ERR);
            }
            if !unsafe { exec(db, V3_TO_V4) } {
                return Err(DB_ALTER_TABLE_ERR);
            }
        }
        _ => {}
    }
    if !unsafe { exec(db, c"PRAGMA recursive_triggers = true;") } {
        return Err(PRAGMA_SET_ERR);
    }
    Ok(())
}

unsafe fn open_db(filename: *const c_char) -> Result<Database, c_int> {
    let mut raw = ptr::null_mut();
    let status = unsafe {
        sqlite3_open_v2(filename, &mut raw, SQLITE_OPEN_READWRITE | SQLITE_OPEN_CREATE, ptr::null())
    };
    let owned = Database(raw);
    if status != SQLITE_OK {
        return Err(YOMI_ERR_DB);
    }
    if let Err(error) = unsafe { prepare_db(owned.0) } {
        return Err(if error == DB_NEW_VERSION_ERR { YOMI_ERR_NEWER_VERSION } else { YOMI_ERR_DB });
    }
    Ok(owned)
}

unsafe fn get_file_json(archive: *mut Zip, filename: &CStr) -> Result<Json, c_int> {
    let mut stat = std::mem::MaybeUninit::<ZipStat>::uninit();
    unsafe { zip_stat_init(stat.as_mut_ptr()) };
    if unsafe { zip_stat(archive, filename.as_ptr(), 0, stat.as_mut_ptr()) } != 0 {
        return Err(STAT_ERR);
    }
    let stat = unsafe { stat.assume_init() };
    if stat.valid & ZIP_STAT_SIZE == 0 {
        return Err(INVALID_SIZE_ERR);
    }
    let length = usize::try_from(stat.size).map_err(|_| MALLOC_FAILURE_ERR)?;
    let buffer_length = length.checked_add(1).ok_or(MALLOC_FAILURE_ERR)?;
    let mut contents = Vec::new();
    contents.try_reserve_exact(buffer_length).map_err(|_| MALLOC_FAILURE_ERR)?;
    contents.resize(buffer_length, 0);
    let file = ZipEntry(unsafe { zip_fopen(archive, filename.as_ptr(), 0) });
    if file.0.is_null() {
        return Err(ZIP_FILE_OPEN_ERR);
    }
    // The C importer performs a single read and treats short reads as errors.
    let read = unsafe { zip_fread(file.0, contents.as_mut_ptr().cast(), stat.size) };
    if read < 0 || read as u64 != stat.size {
        return Err(ZIP_FILE_READ_ERR);
    }
    let tokener = unsafe { json_tokener_new_ex(256) };
    if tokener.is_null() {
        return Err(MALLOC_FAILURE_ERR);
    }
    let parsed = unsafe {
        json_tokener_parse_ex(tokener, contents.as_ptr().cast(), buffer_length as c_int)
    };
    let error = unsafe { json_tokener_get_error(tokener) };
    unsafe { json_tokener_free(tokener) };
    if error != 0 || parsed.is_null() {
        if !parsed.is_null() {
            unsafe { json_object_put(parsed) };
        }
        return Err(JSON_PARSE_ERROR);
    }
    Ok(Json(parsed))
}

unsafe fn object_field(parent: *mut JsonObject, key: &CStr, kind: c_int) -> Result<*mut JsonObject, c_int> {
    let mut value = ptr::null_mut();
    if unsafe { json_object_object_get_ex(parent, key.as_ptr(), &mut value) } == 0 {
        return Err(JSON_MISSING_KEY_ERR);
    }
    if unsafe { json_object_is_type(value, kind) } == 0 {
        return Err(JSON_WRONG_TYPE_ERR);
    }
    Ok(value)
}

unsafe fn array_field(parent: *mut JsonObject, index: usize, kind: c_int) -> Result<*mut JsonObject, c_int> {
    let value = unsafe { json_object_array_get_idx(parent, index) };
    if unsafe { json_object_is_type(value, kind) } == 0 {
        return Err(JSON_WRONG_TYPE_ERR);
    }
    Ok(value)
}

unsafe fn string_field(parent: *mut JsonObject, index: usize) -> Result<*const c_char, c_int> {
    let value = unsafe { array_field(parent, index, JSON_STRING)? };
    Ok(unsafe { json_object_get_string(value) })
}

unsafe fn int_field(parent: *mut JsonObject, index: usize) -> Result<c_int, c_int> {
    let value = unsafe { array_field(parent, index, JSON_INT)? };
    Ok(unsafe { json_object_get_int(value) })
}

unsafe fn add_index(archive: *mut Zip, db: *mut Sqlite3) -> Result<i64, c_int> {
    let index = unsafe { get_file_json(archive, c"index.json")? };
    if unsafe { json_object_is_type(index.0, JSON_OBJECT) } == 0 {
        return Err(JSON_WRONG_TYPE_ERR);
    }
    let title = unsafe { json_object_get_string(object_field(index.0, c"title", JSON_STRING)?) };
    let format = unsafe { json_object_get_int(object_field(index.0, c"format", JSON_INT)?) };
    let revision = unsafe { json_object_get_string(object_field(index.0, c"revision", JSON_STRING)?) };
    let mut sequenced = ptr::null_mut();
    let sequenced = if unsafe { json_object_object_get_ex(index.0, c"sequenced".as_ptr(), &mut sequenced) } != 0 {
        if unsafe { json_object_is_type(sequenced, JSON_BOOLEAN) } == 0 {
            return Err(JSON_WRONG_TYPE_ERR);
        }
        unsafe { json_object_get_boolean(sequenced) }
    } else {
        0
    };
    if format != 3 {
        return Err(UNSUPPORTED_FORMAT_ERR);
    }
    unsafe {
        insert(db, c"INSERT OR REPLACE INTO directory (title, format, revision, sequenced) VALUES (?, ?, ?, ?);", |stmt| {
            text(stmt, 1, title) && int(stmt, 2, format) && text(stmt, 3, revision) && int(stmt, 4, sequenced)
        })?;
        Ok(sqlite3_last_insert_rowid(db))
    }
}

unsafe fn add_tag(db: *mut Sqlite3, item: *mut JsonObject, id: i64) -> Result<(), c_int> {
    if unsafe { json_object_array_length(item) } != 5 {
        return Err(TAG_WRONG_SIZE_ERR);
    }
    let name = unsafe { string_field(item, 0)? };
    let category = unsafe { string_field(item, 1)? };
    let order = unsafe { int_field(item, 2)? };
    let notes = unsafe { string_field(item, 3)? };
    let score = unsafe { int_field(item, 4)? };
    unsafe {
        insert(db, c"INSERT INTO tag_bank (dic_id, name, category, ord, notes, score) VALUES (?, ?, ?, ?, ?, ?);", |stmt| {
            int(stmt, 1, id as c_int) && text(stmt, 2, name) && text(stmt, 3, category)
                && int(stmt, 4, order) && text(stmt, 5, notes) && int(stmt, 6, score)
        })
    }
}

unsafe fn add_term(db: *mut Sqlite3, item: *mut JsonObject, id: i64) -> Result<(), c_int> {
    if unsafe { json_object_array_length(item) } != 8 {
        return Err(TERM_WRONG_SIZE_ERR);
    }
    let expression = unsafe { string_field(item, 0)? };
    let reading = unsafe { string_field(item, 1)? };
    let def_tags_object = unsafe { json_object_array_get_idx(item, 2) };
    let def_tags = match unsafe { json_object_get_type(def_tags_object) } {
        JSON_STRING => unsafe { json_object_get_string(def_tags_object) },
        JSON_NULL => c"".as_ptr(),
        _ => return Err(JSON_WRONG_TYPE_ERR),
    };
    let rules = unsafe { string_field(item, 3)? };
    let score = unsafe { int_field(item, 4)? };
    let glossary = unsafe { array_field(item, 5, JSON_ARRAY)? };
    let glossary = unsafe { json_object_to_json_string(glossary) };
    let sequence = unsafe { int_field(item, 6)? };
    let term_tags = unsafe { string_field(item, 7)? };
    let reading = if unsafe { CStr::from_ptr(expression) } == unsafe { CStr::from_ptr(reading) } {
        c"".as_ptr()
    } else {
        reading
    };
    unsafe {
        insert(db, c"INSERT INTO term_bank (dic_id, expression, reading, def_tags, rules, score, glossary, sequence, term_tags) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?);", |stmt| {
            int(stmt, 1, id as c_int) && text(stmt, 2, expression) && text(stmt, 3, reading)
                && text(stmt, 4, def_tags) && text(stmt, 5, rules) && int(stmt, 6, score)
                && text(stmt, 7, glossary) && int(stmt, 8, sequence) && text(stmt, 9, term_tags)
        })
    }
}

unsafe fn add_kanji(db: *mut Sqlite3, item: *mut JsonObject, id: i64) -> Result<(), c_int> {
    if unsafe { json_object_array_length(item) } != 6 {
        return Err(KANJI_WRONG_SIZE_ERR);
    }
    let character = unsafe { string_field(item, 0)? };
    let onyomi = unsafe { string_field(item, 1)? };
    let kunyomi = unsafe { string_field(item, 2)? };
    let tags = unsafe { string_field(item, 3)? };
    let meanings = unsafe { json_object_to_json_string(array_field(item, 4, JSON_ARRAY)?) };
    let stats = unsafe { json_object_to_json_string(array_field(item, 5, JSON_OBJECT)?) };
    unsafe {
        insert(db, c"INSERT INTO kanji_bank (dic_id, char, onyomi, kunyomi, tags, meanings, stats) VALUES (?, ?, ?, ?, ?, ?, ?);", |stmt| {
            int(stmt, 1, id as c_int) && text(stmt, 2, character) && text(stmt, 3, onyomi)
                && text(stmt, 4, kunyomi) && text(stmt, 5, tags) && text(stmt, 6, meanings)
                && text(stmt, 7, stats)
        })
    }
}

unsafe fn add_meta(db: *mut Sqlite3, item: *mut JsonObject, id: i64, sql: &CStr) -> Result<(), c_int> {
    if unsafe { json_object_array_length(item) } != 3 {
        return Err(META_WRONG_SIZE_ERR);
    }
    let expression = unsafe { string_field(item, 0)? };
    let mode = unsafe { string_field(item, 1)? };
    let data = unsafe { json_object_array_get_idx(item, 2) };
    // json-c's spaced serializer and native-endian scalar layouts are part of
    // the existing metadata BLOB format; don't reserialize through serde_json.
    let int_data: i64;
    let bool_data: c_int;
    let double_data: f64;
    let mut data_length = 0usize;
    let (kind, blob): (c_int, *const c_void) = match unsafe { json_object_get_type(data) } {
        JSON_ARRAY | JSON_OBJECT => {
            let serialized = unsafe {
                json_object_to_json_string_length(data, JSON_C_TO_STRING_SPACED, &mut data_length)
            };
            data_length += 1; // Include the terminal NUL in the BLOB.
            (if unsafe { json_object_get_type(data) } == JSON_ARRAY { 5 } else { 4 }, serialized.cast())
        }
        JSON_INT => {
            int_data = unsafe { json_object_get_int64(data) };
            data_length = std::mem::size_of::<i64>();
            (1, (&int_data as *const i64).cast())
        }
        JSON_BOOLEAN => {
            bool_data = unsafe { json_object_get_boolean(data) };
            data_length = std::mem::size_of::<c_int>();
            (6, (&bool_data as *const c_int).cast())
        }
        JSON_DOUBLE => {
            double_data = unsafe { json_object_get_double(data) };
            data_length = std::mem::size_of::<f64>();
            (3, (&double_data as *const f64).cast())
        }
        JSON_STRING => {
            data_length = unsafe { json_object_get_string_len(data) } as usize + 1;
            (2, unsafe { json_object_get_string(data) }.cast())
        }
        JSON_NULL => (0, ptr::null()),
        _ => return Err(JSON_WRONG_TYPE_ERR),
    };
    unsafe {
        insert(db, sql, |stmt| {
            int(stmt, 1, id as c_int) && text(stmt, 2, expression) && text(stmt, 3, mode)
                && int(stmt, 4, kind)
                && if kind == 0 {
                    sqlite3_bind_null(stmt, 5) == SQLITE_OK
                } else {
                    // sqlite3_bind_blob takes int; the source implicitly narrows size_t.
                    sqlite3_bind_blob(stmt, 5, blob, data_length as c_int, None) == SQLITE_OK
                }
        })
    }
}

#[derive(Clone, Copy)]
enum Bank { Tag, Term, TermMeta, Kanji, KanjiMeta }

impl Bank {
    fn filename(self, mut number: usize, buffer: &mut [u8; 256]) -> &CStr {
        let prefix: &[u8] = match self {
            Self::Tag => b"tag_bank_",
            Self::Term => b"term_bank_",
            Self::TermMeta => b"term_meta_bank_",
            Self::Kanji => b"kanji_bank_",
            Self::KanjiMeta => b"kanji_meta_bank_",
        };
        buffer[..prefix.len()].copy_from_slice(prefix);
        let mut reversed = [0u8; 20]; // decimal digits of any 64-bit usize
        let mut digits = 0;
        loop {
            reversed[digits] = b'0' + (number % 10) as u8;
            digits += 1;
            number /= 10;
            if number == 0 { break; }
        }
        for (index, digit) in reversed[..digits].iter().rev().enumerate() {
            buffer[prefix.len() + index] = *digit;
        }
        let suffix = prefix.len() + digits;
        buffer[suffix..suffix + 6].copy_from_slice(b".json\0");
        CStr::from_bytes_with_nul(&buffer[..suffix + 6]).expect("bank filename has no NUL")
    }
}

unsafe fn add_bank(archive: *mut Zip, db: *mut Sqlite3, id: i64, bank: Bank) -> Result<(), c_int> {
    let mut number = 1usize;
    let mut filename_buffer = [0u8; 256];
    loop {
        let filename = bank.filename(number, &mut filename_buffer);
        if unsafe { zip_name_locate(archive, filename.as_ptr(), 0) } == -1 {
            break;
        }
        let items = unsafe { get_file_json(archive, &filename)? };
        if unsafe { json_object_is_type(items.0, JSON_ARRAY) } == 0 {
            return Err(JSON_WRONG_TYPE_ERR);
        }
        for index in 0..unsafe { json_object_array_length(items.0) } {
            let item = unsafe { json_object_array_get_idx(items.0, index) };
            if unsafe { json_object_is_type(item, JSON_ARRAY) } == 0 {
                return Err(JSON_WRONG_TYPE_ERR);
            }
            unsafe {
                match bank {
                    Bank::Tag => add_tag(db, item, id)?,
                    Bank::Term => add_term(db, item, id)?,
                    Bank::TermMeta => add_meta(db, item, id, c"INSERT INTO term_meta_bank (dic_id, expression, mode, type, data) VALUES (?, ?, ?, ?, ?);")?,
                    Bank::Kanji => add_kanji(db, item, id)?,
                    Bank::KanjiMeta => add_meta(db, item, id, c"INSERT INTO kanji_meta_bank (dic_id, expression, mode, type, data) VALUES (?, ?, ?, ?, ?);")?,
                }
            }
        }
        number += 1;
    }
    Ok(())
}

// POSIX ERE REGEX_SKIP_FILE: the banks' first suffix digit must be nonzero;
// following digits and the literal parentheses in the C character class are
// accepted. In particular a bank inside a directory is NOT skipped.
fn skipped_resource(name: &[u8]) -> bool {
    if name == b"index.json" {
        return true;
    }
    for prefix in [b"tag_bank_".as_slice(), b"term_bank_", b"term_meta_bank_", b"kanji_bank_", b"kanji_meta_bank_"] {
        if let Some(rest) = name.strip_prefix(prefix).and_then(|s| s.strip_suffix(b".json")) {
            if rest.first().is_some_and(|&first| matches!(first, b'1'..=b'9'))
                && rest[1..].iter().all(|&c| c.is_ascii_digit() || matches!(c, b'(' | b')' | b'\\'))
            {
                return true;
            }
        }
    }
    false
}

fn combined_path(start: &[u8], end: &[u8]) -> Result<CString, c_int> {
    let mut bytes = Vec::new();
    bytes.try_reserve(start.len() + end.len() + 2).map_err(|_| MALLOC_FAILURE_ERR)?;
    bytes.extend_from_slice(start);
    if !start.ends_with(b"/") {
        bytes.push(b'/');
    }
    bytes.extend_from_slice(end);
    CString::new(bytes).map_err(|_| YOMI_ERR_EXTRACTING_RESOURCES)
}

#[cfg(unix)]
fn path_from_c(path: &CStr) -> &std::path::Path {
    use std::os::unix::ffi::OsStrExt;
    std::path::Path::new(std::ffi::OsStr::from_bytes(path.to_bytes()))
}

#[cfg(not(unix))]
fn path_from_c(path: &CStr) -> &std::path::Path {
    std::path::Path::new(path.to_str().unwrap_or(""))
}

#[cfg(unix)]
fn make_path(path: &CStr) -> Result<(), c_int> {
    extern "C" {
        fn mkdir(path: *const c_char, mode: u32) -> c_int;
    }
    let bytes = path.to_bytes();
    let Some(first) = bytes.iter().position(|&c| c == b'/') else {
        return Ok(());
    };
    for end in bytes.iter().enumerate().skip(first + 1).filter_map(|(i, &c)| (c == b'/').then_some(i))
        .chain(std::iter::once(bytes.len()))
    {
        let mut prefix = bytes[..end].to_vec();
        prefix.push(0);
        if unsafe { mkdir(prefix.as_ptr().cast(), 0o755) } != 0 {
            let code = std::io::Error::last_os_error().raw_os_error().unwrap_or(1);
            if code != 17 { // EEXIST, including an existing non-directory as in the C importer
                return Err(code);
            }
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn make_path(path: &CStr) -> Result<(), c_int> {
    std::fs::create_dir_all(path_from_c(path)).map_err(|e| e.raw_os_error().unwrap_or(1))
}

#[cfg(unix)]
unsafe extern "C" fn remove_entry(path: *const c_char, _: *const c_void, _: c_int, _: *const c_void) -> c_int {
    if unsafe { remove(path) } == 0 {
        0
    } else {
        std::io::Error::last_os_error().raw_os_error().unwrap_or(1)
    }
}

#[cfg(unix)]
fn remove_path(path: &CStr) -> Result<(), c_int> {
    if unsafe { nftw(path.as_ptr(), remove_entry, 10, 1 | 2 | 8) } == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error().raw_os_error().unwrap_or(1))
    }
}

#[cfg(not(unix))]
fn remove_path(path: &CStr) -> Result<(), c_int> {
    let target = path_from_c(path);
    if target.is_dir() {
        std::fs::remove_dir_all(target)
    } else {
        std::fs::remove_file(target)
    }.map_err(|e| e.raw_os_error().unwrap_or(1))
}

fn remove_if_present(path: &CStr) -> Result<(), c_int> {
    match remove_path(path) {
        Ok(()) | Err(2) => Ok(()), // ENOENT; matches the POSIX resource cleanup
        Err(other) => Err(other),
    }
}

unsafe fn extract_resources(archive: *mut Zip, res_dir: *const c_char) -> Result<(), c_int> {
    let index = unsafe { get_file_json(archive, c"index.json")? };
    let title = unsafe { json_object_get_string(object_field(index.0, c"title", JSON_STRING)?) };
    let base = combined_path(unsafe { CStr::from_ptr(res_dir) }.to_bytes(), unsafe { CStr::from_ptr(title) }.to_bytes())?;
    remove_if_present(&base)?;
    make_path(&base)?;

    let number = unsafe { zip_get_num_entries(archive, 0) };
    for index in 0..number {
        let name = unsafe { zip_get_name(archive, index as u64, 0) };
        if name.is_null() {
            return Err(YOMI_ERR_EXTRACTING_RESOURCES);
        }
        let name = unsafe { CStr::from_ptr(name) }.to_bytes();
        if skipped_resource(name) {
            continue;
        }
        let path = combined_path(base.to_bytes(), name)?;
        if name.ends_with(b"/") {
            // Directory entry creation errors are ignored by the C importer.
            let _ = make_path(&path);
            continue;
        }
        let entry = ZipEntry(unsafe { zip_fopen_index(archive, index as u64, 0) });
        if entry.0.is_null() {
            // The original path returns its current (zero) error code here.
            return Ok(());
        }
        let bytes = path.to_bytes();
        // C truncates at the final slash and calls make_path on that prefix.
        let parent = CString::new(&bytes[..bytes.iter().rposition(|&byte| byte == b'/').unwrap_or(0)])
            .map_err(|_| YOMI_ERR_EXTRACTING_RESOURCES)?;
        make_path(&parent)?;
        let Ok(mut file) = std::fs::File::create(path_from_c(&path)) else {
            // fopen failure likewise exits without assigning a new error code.
            return Ok(());
        };
        let mut buffer = [0u8; 8192];
        loop {
            let read = unsafe { zip_fread(entry.0, buffer.as_mut_ptr().cast(), buffer.len() as u64) };
            if read == 0 { break; }
            if read < 0 { return Err(ZIP_FILE_READ_ERR); }
            // fwrite failures were not checked in the C implementation.
            let _ = file.write_all(&buffer[..read as usize]);
        }
    }
    Ok(())
}

unsafe fn process_dictionary(dict_file: *const c_char, db_file: *const c_char, res_dir: *const c_char) -> c_int {
    let mut zip_error = 0;
    let archive = Archive(unsafe { zip_open(dict_file, ZIP_RDONLY, &mut zip_error) });
    if zip_error != 0 || archive.0.is_null() {
        return YOMI_ERR_OPENING_DIC;
    }
    let db = match unsafe { open_db(db_file) } {
        Ok(db) => db,
        Err(error) => return error,
    };
    let result = (|| -> Result<(), c_int> {
        if !unsafe { exec(db.0, c"BEGIN EXCLUSIVE TRANSACTION;") } {
            return Err(TRANSACTION_ERR);
        }
        let id = unsafe { add_index(archive.0, db.0) }.map_err(|_| YOMI_ERR_ADDING_INDEX)?;
        for (bank, error) in [
            (Bank::Tag, YOMI_ERR_ADDING_TAGS),
            (Bank::Term, YOMI_ERR_ADDING_TERMS),
            (Bank::TermMeta, YOMI_ERR_ADDING_TERMS_META),
            (Bank::Kanji, YOMI_ERR_ADDING_KANJI),
            (Bank::KanjiMeta, YOMI_ERR_ADDING_KANJI_META),
        ] {
            unsafe { add_bank(archive.0, db.0, id, bank) }.map_err(|_| error)?;
        }
        unsafe { extract_resources(archive.0, res_dir) }.map_err(|_| YOMI_ERR_EXTRACTING_RESOURCES)?;
        if !unsafe { exec(db.0, c"COMMIT;") } {
            return Err(TRANSACTION_ERR);
        }
        Ok(())
    })();
    if result.is_err() {
        unsafe { exec(db.0, c"ROLLBACK;") };
    }
    result.err().unwrap_or(0)
}

unsafe fn delete_dictionary(id: i64, db_file: *const c_char, res_dir: *const c_char) -> c_int {
    let db = match unsafe { open_db(db_file) } {
        Ok(db) => db,
        Err(error) => return error,
    };
    let stmt = match unsafe { statement(db.0, c"DELETE FROM directory WHERE (dic_id = ?) RETURNING title;") } {
        Ok(stmt) => stmt,
        Err(_) => return YOMI_ERR_DELETE,
    };
    if unsafe { sqlite3_bind_int(stmt.0, 1, id as c_int) } != SQLITE_OK {
        return YOMI_ERR_DELETE;
    }
    if unsafe { sqlite3_step(stmt.0) } != SQLITE_ROW {
        return YOMI_ERR_DELETE;
    }
    let title = unsafe { sqlite3_column_text(stmt.0, 0) };
    if title.is_null() { return YOMI_ERR_DELETE; }
    let path = match combined_path(unsafe { CStr::from_ptr(res_dir) }.to_bytes(), unsafe { CStr::from_ptr(title.cast()) }.to_bytes()) {
        Ok(path) => path,
        Err(_) => return YOMI_ERR_REMOVING_RESOURCES,
    };
    if remove_if_present(&path).is_err() {
        return YOMI_ERR_REMOVING_RESOURCES;
    }
    if unsafe { sqlite3_step(stmt.0) } != SQLITE_DONE {
        return YOMI_ERR_DELETE;
    }
    0
}

unsafe fn modify_dict_disabled(id: i64, db_file: *const c_char, query: &CStr) -> c_int {
    let db = match unsafe { open_db(db_file) } {
        Ok(db) => db,
        Err(error) => return error,
    };
    match unsafe { insert(db.0, query, |stmt| int(stmt, 1, id as c_int)) } {
        Ok(()) => 0,
        Err(error) => error,
    }
}

/// # Safety
/// `db_file` must point to a NUL-terminated path; `db` may be null, otherwise
/// it must be writable. A successful output handle is closed with sqlite3_close_v2.
#[no_mangle]
pub unsafe extern "C" fn memento_dict_yomi_prepare_db(db_file: *const c_char, db: *mut *mut Sqlite3) -> c_int {
    match unsafe { open_db(db_file) } {
        Ok(owned) => {
            if !db.is_null() {
                unsafe { *db = owned.0 };
                std::mem::forget(owned);
            }
            0
        }
        Err(error) => {
            if !db.is_null() {
                unsafe { *db = ptr::null_mut() };
            }
            error
        }
    }
}

/// # Safety
/// All paths must point to NUL-terminated strings; the resource directory
/// must already exist, matching the original C API contract.
#[no_mangle]
pub unsafe extern "C" fn memento_dict_yomi_process_dictionary(
    dict_file: *const c_char, db_file: *const c_char, res_dir: *const c_char,
) -> c_int {
    unsafe { process_dictionary(dict_file, db_file, res_dir) }
}

/// # Safety
/// `db_file` and `res_dir` must point to NUL-terminated paths.
#[no_mangle]
pub unsafe extern "C" fn memento_dict_yomi_delete_dictionary(
    dic_id: i64, db_file: *const c_char, res_dir: *const c_char,
) -> c_int {
    unsafe { delete_dictionary(dic_id, db_file, res_dir) }
}

/// # Safety
/// `db_file` must point to a NUL-terminated path.
#[no_mangle]
pub unsafe extern "C" fn memento_dict_yomi_disable_dictionary(dic_id: i64, db_file: *const c_char) -> c_int {
    unsafe { modify_dict_disabled(dic_id, db_file, c"INSERT OR IGNORE INTO dict_disabled (dic_id) VALUES (?);") }
}

/// # Safety
/// `db_file` must point to a NUL-terminated path.
#[no_mangle]
pub unsafe extern "C" fn memento_dict_yomi_enable_dictionary(dic_id: i64, db_file: *const c_char) -> c_int {
    unsafe { modify_dict_disabled(dic_id, db_file, c"DELETE FROM dict_disabled WHERE dic_id = ?;") }
}

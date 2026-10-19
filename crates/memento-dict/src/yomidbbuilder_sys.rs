//! Native SQLite, json-c and libzip declarations used by the importer.
//! The archive and JSON objects stay in their original C formats so their
//! serialization and SQLite BLOB representations match existing databases.

use std::ffi::{c_char, c_double, c_int, c_long, c_void};

#[repr(C)]
pub struct Sqlite3 { _opaque: [u8; 0] }
#[repr(C)]
pub struct SqliteStmt { _opaque: [u8; 0] }
#[repr(C)]
pub struct Zip { _opaque: [u8; 0] }
#[repr(C)]
pub struct ZipFile { _opaque: [u8; 0] }
#[repr(C)]
pub struct JsonObject { _opaque: [u8; 0] }
#[repr(C)]
pub struct JsonTokener { _opaque: [u8; 0] }

#[repr(C)]
pub struct ZipStat {
    pub valid: u64,
    pub name: *const c_char,
    pub index: u64,
    pub size: u64,
    pub comp_size: u64,
    #[cfg(unix)]
    pub mtime: c_long,
    #[cfg(windows)]
    pub mtime: i64,
    pub crc: u32,
    pub comp_method: u16,
    pub encryption_method: u16,
    pub flags: u32,
}

pub const SQLITE_OK: c_int = 0;
pub const SQLITE_ROW: c_int = 100;
pub const SQLITE_DONE: c_int = 101;
pub const SQLITE_OPEN_READWRITE: c_int = 0x2;
pub const SQLITE_OPEN_CREATE: c_int = 0x4;
pub const ZIP_RDONLY: c_int = 16;
pub const ZIP_STAT_SIZE: u64 = 0x4;
pub const JSON_NULL: c_int = 0;
pub const JSON_BOOLEAN: c_int = 1;
pub const JSON_DOUBLE: c_int = 2;
pub const JSON_INT: c_int = 3;
pub const JSON_OBJECT: c_int = 4;
pub const JSON_ARRAY: c_int = 5;
pub const JSON_STRING: c_int = 6;
pub const JSON_C_TO_STRING_SPACED: c_int = 1;

#[link(name = "sqlite3")]
extern "C" {
    pub fn sqlite3_open_v2(file: *const c_char, db: *mut *mut Sqlite3, flags: c_int, vfs: *const c_char) -> c_int;
    pub fn sqlite3_close_v2(db: *mut Sqlite3) -> c_int;
    pub fn sqlite3_exec(
        db: *mut Sqlite3, sql: *const c_char,
        callback: Option<unsafe extern "C" fn(*mut c_void, c_int, *mut *mut c_char, *mut *mut c_char) -> c_int>,
        arg: *mut c_void, error: *mut *mut c_char,
    ) -> c_int;
    pub fn sqlite3_free(value: *mut c_void);
    pub fn sqlite3_mprintf(format: *const c_char, ...) -> *mut c_char;
    pub fn sqlite3_prepare_v2(db: *mut Sqlite3, sql: *const c_char, bytes: c_int, stmt: *mut *mut SqliteStmt, tail: *mut *const c_char) -> c_int;
    pub fn sqlite3_step(stmt: *mut SqliteStmt) -> c_int;
    pub fn sqlite3_finalize(stmt: *mut SqliteStmt) -> c_int;
    pub fn sqlite3_column_int(stmt: *mut SqliteStmt, index: c_int) -> c_int;
    pub fn sqlite3_column_text(stmt: *mut SqliteStmt, index: c_int) -> *const u8;
    pub fn sqlite3_last_insert_rowid(db: *mut Sqlite3) -> i64;
    pub fn sqlite3_bind_int(stmt: *mut SqliteStmt, index: c_int, value: c_int) -> c_int;
    pub fn sqlite3_bind_text(stmt: *mut SqliteStmt, index: c_int, value: *const c_char, bytes: c_int, destructor: Option<unsafe extern "C" fn(*mut c_void)>) -> c_int;
    pub fn sqlite3_bind_blob(stmt: *mut SqliteStmt, index: c_int, value: *const c_void, bytes: c_int, destructor: Option<unsafe extern "C" fn(*mut c_void)>) -> c_int;
    pub fn sqlite3_bind_null(stmt: *mut SqliteStmt, index: c_int) -> c_int;
}

#[link(name = "zip")]
extern "C" {
    pub fn zip_open(file: *const c_char, flags: c_int, error: *mut c_int) -> *mut Zip;
    pub fn zip_close(archive: *mut Zip) -> c_int;
    pub fn zip_stat_init(stat: *mut ZipStat);
    pub fn zip_stat(archive: *mut Zip, file: *const c_char, flags: u32, stat: *mut ZipStat) -> c_int;
    pub fn zip_fopen(archive: *mut Zip, file: *const c_char, flags: u32) -> *mut ZipFile;
    pub fn zip_fopen_index(archive: *mut Zip, index: u64, flags: u32) -> *mut ZipFile;
    pub fn zip_fclose(file: *mut ZipFile) -> c_int;
    pub fn zip_fread(file: *mut ZipFile, buffer: *mut c_void, bytes: u64) -> i64;
    pub fn zip_name_locate(archive: *mut Zip, name: *const c_char, flags: u32) -> i64;
    pub fn zip_get_num_entries(archive: *mut Zip, flags: u32) -> i64;
    pub fn zip_get_name(archive: *mut Zip, index: u64, flags: u32) -> *const c_char;
}

#[link(name = "json-c")]
extern "C" {
    pub fn json_tokener_new_ex(depth: c_int) -> *mut JsonTokener;
    pub fn json_tokener_parse_ex(tokener: *mut JsonTokener, input: *const c_char, bytes: c_int) -> *mut JsonObject;
    pub fn json_tokener_get_error(tokener: *mut JsonTokener) -> c_int;
    pub fn json_tokener_free(tokener: *mut JsonTokener);
    pub fn json_object_put(object: *mut JsonObject) -> c_int;
    pub fn json_object_get_type(object: *const JsonObject) -> c_int;
    pub fn json_object_is_type(object: *const JsonObject, kind: c_int) -> c_int;
    pub fn json_object_object_get_ex(object: *const JsonObject, key: *const c_char, result: *mut *mut JsonObject) -> c_int;
    pub fn json_object_array_get_idx(object: *const JsonObject, index: usize) -> *mut JsonObject;
    pub fn json_object_array_length(object: *const JsonObject) -> usize;
    pub fn json_object_get_string(object: *mut JsonObject) -> *const c_char;
    pub fn json_object_get_string_len(object: *const JsonObject) -> c_int;
    pub fn json_object_to_json_string(object: *mut JsonObject) -> *const c_char;
    pub fn json_object_to_json_string_length(object: *mut JsonObject, flags: c_int, len: *mut usize) -> *const c_char;
    pub fn json_object_get_int(object: *const JsonObject) -> c_int;
    pub fn json_object_get_int64(object: *const JsonObject) -> i64;
    pub fn json_object_get_boolean(object: *const JsonObject) -> c_int;
    pub fn json_object_get_double(object: *const JsonObject) -> c_double;
}

#[cfg(unix)]
extern "C" {
    pub fn nftw(
        path: *const c_char,
        callback: unsafe extern "C" fn(*const c_char, *const c_void, c_int, *const c_void) -> c_int,
        descriptors: c_int, flags: c_int,
    ) -> c_int;
    pub fn remove(path: *const c_char) -> c_int;
}

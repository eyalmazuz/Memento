//! System SQLite ABI for the borrowed database query core.
//! The C++ DatabaseManager owns the connection and holds its read lock.

use std::ffi::{c_char, c_int, c_uchar, c_void};

#[repr(C)]
pub struct Sqlite3 {
    _private: [u8; 0],
}

#[repr(C)]
pub struct Statement {
    _private: [u8; 0],
}

pub const OK: c_int = 0;
pub const ROW: c_int = 100;
pub const DONE: c_int = 101;

#[link(name = "sqlite3")]
extern "C" {
    pub fn sqlite3_prepare_v2(
        db: *mut Sqlite3,
        sql: *const c_char,
        bytes: c_int,
        stmt: *mut *mut Statement,
        tail: *mut *const c_char,
    ) -> c_int;
    pub fn sqlite3_finalize(stmt: *mut Statement) -> c_int;
    pub fn sqlite3_bind_text(
        stmt: *mut Statement,
        index: c_int,
        text: *const c_char,
        bytes: c_int,
        destructor: Option<unsafe extern "C" fn(*mut c_void)>,
    ) -> c_int;
    pub fn sqlite3_step(stmt: *mut Statement) -> c_int;
    pub fn sqlite3_column_int(stmt: *mut Statement, column: c_int) -> c_int;
    pub fn sqlite3_column_int64(stmt: *mut Statement, column: c_int) -> i64;
    pub fn sqlite3_column_text(stmt: *mut Statement, column: c_int) -> *const c_uchar;
    pub fn sqlite3_column_blob(stmt: *mut Statement, column: c_int) -> *const c_void;
    pub fn sqlite3_column_bytes(stmt: *mut Statement, column: c_int) -> c_int;
}

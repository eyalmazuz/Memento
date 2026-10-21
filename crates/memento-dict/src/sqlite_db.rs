//! Pure Rust SQLite Yomichan query engine and cache validator.

use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::fmt;

pub use crate::databasemanager::TermQueryKind;

#[allow(non_camel_case_types)]
#[repr(C)]
pub struct sqlite3 {
    _opaque: [u8; 0],
}

#[allow(non_camel_case_types)]
#[repr(C)]
pub struct sqlite3_stmt {
    _opaque: [u8; 0],
}

pub const SQLITE_OK: c_int = 0;
pub const SQLITE_ROW: c_int = 100;
pub const SQLITE_DONE: c_int = 101;

#[inline]
fn sqlite_transient() -> Option<unsafe extern "C" fn(*mut c_void)> {
    unsafe { std::mem::transmute(-1isize) }
}

#[link(name = "sqlite3")]
extern "C" {
    pub fn sqlite3_open_v2(
        file: *const c_char,
        db: *mut *mut sqlite3,
        flags: c_int,
        vfs: *const c_char,
    ) -> c_int;
    pub fn sqlite3_close_v2(db: *mut sqlite3) -> c_int;
    pub fn sqlite3_exec(
        db: *mut sqlite3,
        sql: *const c_char,
        callback: Option<unsafe extern "C" fn(*mut c_void, c_int, *mut *mut c_char, *mut *mut c_char) -> c_int>,
        arg: *mut c_void,
        error: *mut *mut c_char,
    ) -> c_int;
    pub fn sqlite3_prepare_v2(
        db: *mut sqlite3,
        sql: *const c_char,
        bytes: c_int,
        stmt: *mut *mut sqlite3_stmt,
        tail: *mut *const c_char,
    ) -> c_int;
    pub fn sqlite3_step(stmt: *mut sqlite3_stmt) -> c_int;
    pub fn sqlite3_reset(stmt: *mut sqlite3_stmt) -> c_int;
    pub fn sqlite3_clear_bindings(stmt: *mut sqlite3_stmt) -> c_int;
    pub fn sqlite3_finalize(stmt: *mut sqlite3_stmt) -> c_int;
    pub fn sqlite3_bind_text(
        stmt: *mut sqlite3_stmt,
        index: c_int,
        value: *const c_char,
        bytes: c_int,
        destructor: Option<unsafe extern "C" fn(*mut c_void)>,
    ) -> c_int;
    pub fn sqlite3_bind_int(stmt: *mut sqlite3_stmt, index: c_int, value: c_int) -> c_int;
    pub fn sqlite3_bind_int64(stmt: *mut sqlite3_stmt, index: c_int, value: i64) -> c_int;
    pub fn sqlite3_column_int(stmt: *mut sqlite3_stmt, index: c_int) -> c_int;
    pub fn sqlite3_column_int64(stmt: *mut sqlite3_stmt, index: c_int) -> i64;
    pub fn sqlite3_column_double(stmt: *mut sqlite3_stmt, index: c_int) -> f64;
    pub fn sqlite3_column_text(stmt: *mut sqlite3_stmt, index: c_int) -> *const u8;
    pub fn sqlite3_column_blob(stmt: *mut sqlite3_stmt, index: c_int) -> *const c_void;
    pub fn sqlite3_column_bytes(stmt: *mut sqlite3_stmt, index: c_int) -> c_int;
    pub fn sqlite3_errmsg(db: *mut sqlite3) -> *const c_char;
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PitchRow {
    pub dic_id: i64,
    pub mora: Vec<Vec<u16>>,
    pub positions: Vec<i32>,
}

/// RAII statement handle.
pub struct Statement {
    stmt: *mut sqlite3_stmt,
}

impl Statement {
    pub fn prepare(db: *mut sqlite3, sql: &str) -> Result<Self, DictError> {
        if db.is_null() {
            return Err(DictError::NullPointer);
        }
        let c_sql = CString::new(sql).map_err(|e| DictError::Other(e.to_string()))?;
        let mut stmt: *mut sqlite3_stmt = std::ptr::null_mut();
        let rc = unsafe {
            sqlite3_prepare_v2(db, c_sql.as_ptr(), -1, &mut stmt, std::ptr::null_mut())
        };
        if rc != SQLITE_OK || stmt.is_null() {
            let msg = unsafe {
                let err = sqlite3_errmsg(db);
                if err.is_null() {
                    "Unknown error".into()
                } else {
                    CStr::from_ptr(err).to_string_lossy().into_owned()
                }
            };
            return Err(DictError::Sqlite { code: rc, message: msg });
        }
        Ok(Self { stmt })
    }

    pub fn bind_text(&mut self, index: i32, text: &str) -> Result<(), DictError> {
        let c_str = CString::new(text).map_err(|e| DictError::Other(e.to_string()))?;
        let rc = unsafe {
            sqlite3_bind_text(self.stmt, index, c_str.as_ptr(), -1, sqlite_transient())
        };
        if rc != SQLITE_OK {
            return Err(DictError::Sqlite {
                code: rc,
                message: format!("bind_text failed at index {index}"),
            });
        }
        Ok(())
    }

    pub fn bind_u16_as_utf8(&mut self, index: i32, text: &[u16]) -> Result<(), DictError> {
        let s = String::from_utf16_lossy(text);
        self.bind_text(index, &s)
    }

    pub fn step(&mut self) -> Result<bool, DictError> {
        let rc = unsafe { sqlite3_step(self.stmt) };
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
        let reset = unsafe { sqlite3_reset(self.stmt) };
        if reset != SQLITE_OK {
            return Err(DictError::Sqlite { code: reset, message: "reset failed".into() });
        }
        let clear = unsafe { sqlite3_clear_bindings(self.stmt) };
        if clear != SQLITE_OK {
            return Err(DictError::Sqlite { code: clear, message: "clear bindings failed".into() });
        }
        Ok(())
    }

    #[must_use]
    pub fn column_int(&self, index: i32) -> i32 {
        unsafe { sqlite3_column_int(self.stmt, index) }
    }

    #[must_use]
    pub fn column_int64(&self, index: i32) -> i64 {
        unsafe { sqlite3_column_int64(self.stmt, index) }
    }

    #[must_use]
    pub fn column_double(&self, index: i32) -> f64 {
        unsafe { sqlite3_column_double(self.stmt, index) }
    }

    #[must_use]
    pub fn column_text_string(&self, index: i32) -> String {
        unsafe {
            let ptr = sqlite3_column_text(self.stmt, index);
            if ptr.is_null() {
                return String::new();
            }
            let len = sqlite3_column_bytes(self.stmt, index) as usize;
            let slice = std::slice::from_raw_parts(ptr, len);
            String::from_utf8_lossy(slice).into_owned()
        }
    }

    #[must_use]
    pub fn column_text_u16(&self, index: i32) -> Vec<u16> {
        unsafe {
            let ptr = sqlite3_column_text(self.stmt, index);
            if ptr.is_null() {
                return Vec::new();
            }
            let len = sqlite3_column_bytes(self.stmt, index) as usize;
            let slice = std::slice::from_raw_parts(ptr, len);
            let s = std::str::from_utf8(slice).unwrap_or("");
            s.encode_utf16().collect()
        }
    }

    #[must_use]
    pub fn column_blob_bytes(&self, index: i32) -> Vec<u8> {
        unsafe {
            let ptr = sqlite3_column_blob(self.stmt, index);
            if ptr.is_null() {
                return Vec::new();
            }
            let len = sqlite3_column_bytes(self.stmt, index) as usize;
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
            unsafe { sqlite3_finalize(self.stmt) };
            self.stmt = std::ptr::null_mut();
        }
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

/// Validates database schema, reads dictionary meta and table counts.
pub fn init_cache(db: *mut sqlite3) -> Result<CacheInfo, DictError> {
    if db.is_null() {
        return Err(DictError::NullPointer);
    }

    // 1. Validate database schema
    let mut stmt = Statement::prepare(db, "SELECT name FROM sqlite_master WHERE type='table';")?;
    let mut table_names = std::collections::HashSet::new();
    for name_res in stmt.query_map(|s| Ok(s.column_text_string(0))) {
        table_names.insert(name_res?);
    }
    drop(stmt);

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
            return Err(DictError::InvalidSchema(format!("Missing required table '{tbl}'")));
        }
    }

    // 2. Read dictionary meta
    let mut dict_stmt = Statement::prepare(
        db,
        "SELECT dic_id, title, EXISTS (SELECT 1 FROM dict_disabled WHERE dict_disabled.dic_id == directory.dic_id) AS disabled FROM directory;",
    )?;
    let mut dictionaries = Vec::new();
    for d_res in dict_stmt.query_map(|s| {
        Ok(DictMeta {
            id: s.column_int64(0),
            title: s.column_text_string(1),
            disabled: s.column_int(2) != 0,
        })
    }) {
        dictionaries.push(d_res?);
    }
    drop(dict_stmt);

    // 3. Read table counts
    let count_table = |tbl: &str| -> Result<i64, DictError> {
        let sql = format!("SELECT COUNT(*) FROM {tbl};");
        let mut c_stmt = Statement::prepare(db, &sql)?;
        if c_stmt.step()? {
            Ok(c_stmt.column_int64(0))
        } else {
            Ok(0)
        }
    };

    let table_counts = TableCounts {
        directory: count_table("directory")?,
        tag_bank: count_table("tag_bank")?,
        term_bank: count_table("term_bank")?,
        term_meta_bank: count_table("term_meta_bank")?,
        kanji_bank: count_table("kanji_bank")?,
        kanji_meta_bank: count_table("kanji_meta_bank")?,
    };

    Ok(CacheInfo {
        dictionaries,
        table_counts,
    })
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

    let sql = match plan.query_kind {
        TermQueryKind::WithHalfwidth => {
            "SELECT DISTINCT expression, reading FROM term_bank \
             WHERE dic_id NOT IN (SELECT dic_id FROM dict_disabled) AND \
             (expression = ? OR reading = ? OR expression = ? OR reading = ? OR expression = ? OR reading = ?);"
        }
        TermQueryKind::WithKatakana => {
            "SELECT DISTINCT expression, reading FROM term_bank \
             WHERE dic_id NOT IN (SELECT dic_id FROM dict_disabled) AND \
             (expression = ? OR reading = ? OR expression = ? OR reading = ?);"
        }
        TermQueryKind::Plain => {
            "SELECT DISTINCT expression, reading FROM term_bank \
             WHERE dic_id NOT IN (SELECT dic_id FROM dict_disabled) AND \
             (expression = ? OR reading = ?);"
        }
    };

    let mut stmt = Statement::prepare(db, sql)?;

    // Bind parameters
    stmt.bind_u16_as_utf8(1, &plan.raw)?;
    stmt.bind_u16_as_utf8(2, &plan.raw)?;

    if plan.contains_kata {
        stmt.bind_u16_as_utf8(3, &plan.hiragana)?;
        stmt.bind_u16_as_utf8(4, &plan.hiragana)?;
    }

    if plan.contains_half {
        stmt.bind_u16_as_utf8(5, &plan.nfkc)?;
        stmt.bind_u16_as_utf8(6, &plan.nfkc)?;
    }

    for row_res in stmt.query_map(|s| {
        Ok(TermRow {
            expression: s.column_text_u16(0),
            reading: s.column_text_u16(1),
        })
    }) {
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

    const QUERY: &str =
        "SELECT dic_id, onyomi, kunyomi, tags, meanings, stats FROM kanji_bank \
         WHERE dic_id NOT IN (SELECT dic_id FROM dict_disabled) AND (char = ?);";

    let mut stmt = Statement::prepare(db, QUERY)?;
    stmt.bind_u16_as_utf8(1, kanji)?;

    for row_res in stmt.query_map(|s| {
        let dic_id = s.column_int64(0);
        let onyomi = s.column_text_u16(1);
        let kunyomi = s.column_text_u16(2);
        let tags = s.column_text_u16(3);
        let meanings_blob = s.column_blob_bytes(4);
        let meanings = crate::databasemanager::json_array_to_strings(&meanings_blob);
        let stats = s.column_text_u16(5);
        Ok(KanjiRow {
            dic_id,
            onyomi,
            kunyomi,
            tags,
            meanings,
            stats,
        })
    }) {
        results.push(row_res?);
    }

    Ok(())
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

    let default_query = "SELECT dic_id, data, type FROM term_meta_bank \
         WHERE dic_id NOT IN (SELECT dic_id FROM dict_disabled) AND \
         (expression = ? AND mode = 'freq');";

    let sql = custom_sql.unwrap_or(default_query);

    execute_freq_query(db, sql, term, reading, results)
}

fn execute_freq_query(
    db: *mut sqlite3,
    sql: &str,
    term: &[u16],
    reading: &[u16],
    results: &mut Vec<FreqRow>,
) -> Result<(), DictError> {
    let mut stmt = Statement::prepare(db, sql)?;
    stmt.bind_u16_as_utf8(1, term)?;

    while stmt.step()? {
        let dic_id = stmt.column_int64(0);
        let blob_type = stmt.column_int(2);
        let blob = stmt.column_blob_bytes(1);

        let freq_str_opt: Option<Vec<u16>> = match blob_type {
            // YOMI_BLOB_TYPE_INT (1)
            1 => {
                if blob.len() >= 8 {
                    let mut arr = [0u8; 8];
                    arr.copy_from_slice(&blob[..8]);
                    let val = u64::from_ne_bytes(arr);
                    Some(val.to_string().encode_utf16().collect())
                } else {
                    None
                }
            }
            // YOMI_BLOB_TYPE_STRING (2)
            2 => {
                let null_pos = blob.iter().position(|&b| b == 0).unwrap_or(blob.len());
                let s = std::str::from_utf8(&blob[..null_pos]).unwrap_or("");
                Some(s.encode_utf16().collect())
            }
            // YOMI_BLOB_TYPE_OBJECT (4)
            4 => {
                let null_pos = blob.iter().position(|&b| b == 0).unwrap_or(blob.len());
                let slice = &blob[..null_pos];
                if let Ok(serde_json::Value::Object(obj)) = serde_json::from_slice(slice) {
                    let has_reading_key = obj.get("reading").and_then(|v| v.as_str()).is_some();
                    let obj_reading: Vec<u16> = obj
                        .get("reading")
                        .and_then(|v| v.as_str())
                        .map(|s| s.encode_utf16().collect())
                        .unwrap_or_default();

                    let mut freq_kind = 0i32;
                    let mut freq_val_str: Vec<u16> = Vec::new();
                    let mut has_nested_display = false;
                    let mut nested_display_str: Vec<u16> = Vec::new();
                    let mut has_nested_value_num = false;
                    let mut nested_value_num_str: Vec<u16> = Vec::new();

                    if let Some(freq_val) = obj.get("frequency") {
                        if freq_val.is_number() {
                            freq_kind = 1;
                            freq_val_str = format_json_number(freq_val).encode_utf16().collect();
                        } else if let Some(s) = freq_val.as_str() {
                            freq_kind = 2;
                            freq_val_str = s.encode_utf16().collect();
                        } else if let Some(nested) = freq_val.as_object() {
                            freq_kind = 3;
                            if let Some(d) = nested.get("displayValue").and_then(|v| v.as_str()) {
                                has_nested_display = true;
                                nested_display_str = d.encode_utf16().collect();
                            }
                            if let Some(v) = nested.get("value") {
                                if v.is_number() {
                                    has_nested_value_num = true;
                                    nested_value_num_str = format_json_number(v).encode_utf16().collect();
                                }
                            }
                        }
                    }

                    let has_top_display = obj.get("displayValue").and_then(|v| v.as_str()).is_some();
                    let top_display_str: Vec<u16> = obj
                        .get("displayValue")
                        .and_then(|v| v.as_str())
                        .map(|s| s.encode_utf16().collect())
                        .unwrap_or_default();

                    let has_top_value_num = obj.get("value").map(|v| v.is_number()).unwrap_or(false);
                    let top_value_num_str: Vec<u16> = obj
                        .get("value")
                        .filter(|v| v.is_number())
                        .map(|v| format_json_number(v).encode_utf16().collect())
                        .unwrap_or_default();

                    crate::databasemanager::extract_frequency_object(
                        has_reading_key,
                        &obj_reading,
                        reading,
                        freq_kind,
                        &freq_val_str,
                        has_nested_display,
                        &nested_display_str,
                        has_nested_value_num,
                        &nested_value_num_str,
                        has_top_display,
                        &top_display_str,
                        has_top_value_num,
                        &top_value_num_str,
                    )
                } else {
                    None
                }
            }
            _ => None,
        };

        if let Some(freq_str) = freq_str_opt {
            results.push(FreqRow {
                dic_id,
                frequency: freq_str,
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

fn format_json_number(val: &serde_json::Value) -> String {
    // QJsonValue::toDouble() converts even integral JSON values to double.
    // The C++ frequency renderer always passes that double to
    // QString::number(value), whose default precision is six significant
    // digits; displaying the serde_json integer directly changes ranks.
    val.as_f64().map_or_else(String::new, |value| format_g(value, 6))
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

    const QUERY: &str =
        "SELECT dic_id, data FROM term_meta_bank \
         WHERE dic_id NOT IN (SELECT dic_id FROM dict_disabled) AND \
         (expression = ? AND mode = 'pitch');";

    let mut stmt = Statement::prepare(db, QUERY)?;
    stmt.bind_u16_as_utf8(1, term)?;

    while stmt.step()? {
        let dic_id = stmt.column_int64(0);
        let blob = stmt.column_blob_bytes(1);
        let null_pos = blob.iter().position(|&b| b == 0).unwrap_or(blob.len());
        let blob_slice = &blob[..null_pos];

        let parsed = crate::databasemanager::parse_pitch_blob(blob_slice, term, reading);
        if parsed.matched {
            results.push(PitchRow {
                dic_id,
                mora: parsed.mora,
                positions: parsed.positions,
            });
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestDb(*mut sqlite3);

    impl Drop for TestDb {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe { sqlite3_close_v2(self.0) };
            }
        }
    }

    fn create_test_db() -> TestDb {
        let mut db: *mut sqlite3 = std::ptr::null_mut();
        let rc = unsafe {
            sqlite3_open_v2(
                c":memory:".as_ptr(),
                &mut db,
                0x00000002 | 0x00000004, // SQLITE_OPEN_READWRITE | SQLITE_OPEN_CREATE
                std::ptr::null(),
            )
        };
        assert_eq!(rc, SQLITE_OK);

        const SCHEMA: &CStr = c"
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
        let mut err = std::ptr::null_mut();
        let rc = unsafe { sqlite3_exec(db, SCHEMA.as_ptr(), None, std::ptr::null_mut(), &mut err) };
        assert_eq!(rc, SQLITE_OK);
        TestDb(db)
    }

    fn to_u16(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    #[test]
    fn test_init_cache_empty() {
        let db = create_test_db();
        let cache = init_cache(db.0).unwrap();
        assert_eq!(cache.dictionaries.len(), 0);
        assert_eq!(cache.table_counts.directory, 0);
        assert_eq!(cache.table_counts.term_bank, 0);
    }

    #[test]
    fn test_init_cache_with_dict() {
        let db = create_test_db();
        let insert = c"
            INSERT INTO directory VALUES (1, 'JMdict', 3, 'rev1', 0);
            INSERT INTO tag_bank VALUES (1, 'v1', 'partOfSpeech', 1, 'verb', 0);
            INSERT INTO term_bank VALUES (1, '食べる', 'たべる', '', 'v1', 10, '[\"to eat\"]', 1, '');
        ";
        let mut err = std::ptr::null_mut();
        unsafe { sqlite3_exec(db.0, insert.as_ptr(), None, std::ptr::null_mut(), &mut err) };

        let cache = init_cache(db.0).unwrap();
        assert_eq!(cache.dictionaries.len(), 1);
        assert_eq!(cache.dictionaries[0].id, 1);
        assert_eq!(cache.dictionaries[0].title, "JMdict");
        assert!(!cache.dictionaries[0].disabled);
        assert_eq!(cache.table_counts.directory, 1);
        assert_eq!(cache.table_counts.tag_bank, 1);
        assert_eq!(cache.table_counts.term_bank, 1);
    }

    #[test]
    fn test_query_terms_scenarios() {
        let db = create_test_db();
        let insert = c"
            INSERT INTO directory VALUES (1, 'TestDict', 3, 'rev1', 0);
            INSERT INTO term_bank VALUES (1, '食べる', 'たべる', '', '', 10, '[\"to eat\"]', 1, '');
            INSERT INTO term_bank VALUES (1, 'カタカナ', 'かたかな', '', '', 5, '[\"katakana\"]', 2, '');
        ";
        let mut err = std::ptr::null_mut();
        unsafe { sqlite3_exec(db.0, insert.as_ptr(), None, std::ptr::null_mut(), &mut err) };

        // 1. Plain search
        let raw = to_u16("食べる");
        let plan = TermQueryPlan::new(&raw, &raw);
        let mut results = Vec::new();
        query_terms(db.0, &plan, &mut results).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].expression, to_u16("食べる"));
        assert_eq!(results[0].reading, to_u16("たべる"));

        // 2. Katakana -> Hiragana search ("かたかな" finds "カタカナ")
        let raw_kata = to_u16("かたかな");
        let plan_kata = TermQueryPlan::new(&raw_kata, &raw_kata);
        let mut results_kata = Vec::new();
        query_terms(db.0, &plan_kata, &mut results_kata).unwrap();
        assert_eq!(results_kata.len(), 1);
        assert_eq!(results_kata[0].expression, to_u16("カタカナ"));

        // 3. Halfwidth -> Katakana search ("ｶﾀｶﾅ" finds "カタカナ")
        let raw_half = to_u16("ｶﾀｶﾅ");
        let nfkc_half = to_u16("カタカナ");
        let plan_half = TermQueryPlan::new(&raw_half, &nfkc_half);
        let mut results_half = Vec::new();
        query_terms(db.0, &plan_half, &mut results_half).unwrap();
        assert_eq!(results_half.len(), 1);
        assert_eq!(results_half[0].expression, to_u16("カタカナ"));
    }

    #[test]
    fn term_frequencies_never_fall_back_to_kanji_metadata() {
        let db = create_test_db();
        let mut stmt = Statement::prepare(
            db.0,
            "INSERT INTO kanji_meta_bank (dic_id, expression, mode, type, data) \
             VALUES (1, '字', 'freq', 2, 'kanji rank');",
        ).unwrap();
        assert!(!stmt.step().unwrap());
        let mut rows = Vec::new();
        get_frequencies(db.0, &to_u16("字"), &[], &mut rows).unwrap();
        assert!(rows.is_empty(), "the C++ term query reads only term_meta_bank");
    }

    #[test]
    fn json_frequency_numbers_use_qjsonvalue_to_double_and_qstring_number() {
        let db = create_test_db();
        let mut stmt = Statement::prepare(
            db.0,
            "INSERT INTO term_meta_bank (dic_id, expression, mode, type, data) \
             VALUES (1, '語', 'freq', 4, '{\"frequency\":1234567}'), \
                    (2, '語', 'freq', 4, '{\"frequency\":{\"value\":123456789}}'), \
                    (3, '語', 'freq', 4, '{\"value\":1234567}');",
        ).unwrap();
        assert!(!stmt.step().unwrap());
        let mut rows = Vec::new();
        get_frequencies(db.0, &to_u16("語"), &[], &mut rows).unwrap();
        assert_eq!(rows.iter().map(|row| String::from_utf16(&row.frequency).unwrap()).collect::<Vec<_>>(),
                   ["1.23457e+06", "1.23457e+08", "1.23457e+06"]);
    }

    #[test]
    fn test_query_kanji() {
        let db = create_test_db();
        let insert = c"
            INSERT INTO directory VALUES (1, 'KanjiDict', 3, 'rev1', 0);
            INSERT INTO kanji_bank VALUES (1, '食', 'ショク', 'た.べる', 'grade:2', '[\"eat\", \"food\"]', '{\"strokes\": \"9\"}');
        ";
        let mut err = std::ptr::null_mut();
        unsafe { sqlite3_exec(db.0, insert.as_ptr(), None, std::ptr::null_mut(), &mut err) };

        let mut results = Vec::new();
        query_kanji(db.0, &to_u16("食"), &mut results).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].dic_id, 1);
        assert_eq!(results[0].onyomi, to_u16("ショク"));
        assert_eq!(results[0].kunyomi, to_u16("た.べる"));
        assert_eq!(results[0].tags, to_u16("grade:2"));
        assert_eq!(results[0].meanings, vec![to_u16("eat"), to_u16("food")]);
        assert_eq!(results[0].stats, to_u16("{\"strokes\": \"9\"}"));
    }

    #[test]
    fn test_get_frequencies_and_pitches() {
        let db = create_test_db();
        let insert = c"
            INSERT INTO directory VALUES (1, 'JMdict', 3, 'rev1', 0);
            INSERT INTO term_meta_bank VALUES (1, '食べる', 'freq', 4, '{\"reading\": \"たべる\", \"frequency\": 120}');
            INSERT INTO term_meta_bank VALUES (1, '食べる', 'pitch', 4, '{\"reading\": \"たべる\", \"pitches\": [{\"position\": 2}]}');
        ";
        let mut err = std::ptr::null_mut();
        unsafe { sqlite3_exec(db.0, insert.as_ptr(), None, std::ptr::null_mut(), &mut err) };

        let mut freqs = Vec::new();
        get_frequencies(db.0, &to_u16("食べる"), &to_u16("たべる"), &mut freqs).unwrap();
        assert_eq!(freqs.len(), 1);
        assert_eq!(freqs[0].dic_id, 1);
        assert_eq!(freqs[0].frequency, to_u16("120"));

        let mut pitches = Vec::new();
        add_pitches(db.0, &to_u16("食べる"), &to_u16("たべる"), &mut pitches).unwrap();
        assert_eq!(pitches.len(), 1);
        assert_eq!(pitches[0].dic_id, 1);
        assert_eq!(pitches[0].mora, vec![to_u16("た"), to_u16("べ"), to_u16("る")]);
        assert_eq!(pitches[0].positions, vec![2]);
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
}


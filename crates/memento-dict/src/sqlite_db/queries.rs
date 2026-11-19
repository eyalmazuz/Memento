//! Prepared-statement SQLite queries for terms, kanji, and metadata.

use std::collections::HashSet;

use rusqlite::types::ValueRef;
use rusqlite::{params, params_from_iter, Connection, Row};

use super::freq::parse_freq_blob_utf8;
use super::{BatchTermRow, DictError, FreqRow, KanjiUtf8Row, RawTermMetaRow};

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
        && crate::normalize::normalized_readings_eq(expression, reading)
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

/// Queries `kanji_meta_bank` frequency rows on a `rusqlite::Connection`.
pub fn get_kanji_frequencies_conn(
    conn: &Connection,
    kanji: &[u16],
    results: &mut Vec<FreqRow>,
) -> Result<(), DictError> {
    const KANJI_FREQ_SQL: &str = "SELECT dic_id, data, type FROM kanji_meta_bank \
         WHERE dic_id NOT IN (SELECT dic_id FROM dict_disabled) AND \
         (expression = ?1 AND mode = 'freq');";
    let term_s = String::from_utf16_lossy(kanji);
    let mut stmt = conn.prepare_cached(KANJI_FREQ_SQL)?;
    let mut rows = stmt.query(params![&term_s])?;
    while let Some(row) = rows.next()? {
        let dic_id: i64 = row.get(0)?;
        let blob = row_bytes(row, 1)?;
        let blob_type: i32 = row.get(2)?;
        let parsed = parse_freq_blob_utf8(blob_type, &blob, &term_s, "");
        if parsed.matched {
            results.push(FreqRow {
                dic_id,
                frequency: parsed.display.encode_utf16().collect(),
            });
        }
    }
    Ok(())
}

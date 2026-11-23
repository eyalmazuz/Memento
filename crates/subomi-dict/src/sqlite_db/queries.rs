//! Prepared-statement SQLite queries for terms, kanji, and metadata.

use std::collections::{HashMap, HashSet};

use rusqlite::types::ValueRef;
use rusqlite::{params, params_from_iter, Connection, Row};

use super::connection::directory_has_column_conn;
use super::freq::parse_freq_blob_with_mode_utf8;
use super::{
    BatchTermRow, DictError, FrequencyMode, KanjiUtf8Row, RawTermMetaRow, TermFrequencyWithMode,
};

/// Default match cap for wildcard searches (`*`/`?`, `＊`/`？`).
pub const DEFAULT_WILDCARD_LIMIT: usize = 100;
/// Maximum allowed match cap for wildcard searches to keep queries bounded.
pub const MAX_WILDCARD_LIMIT: usize = 500;

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
    query_term_details_batch_strs_conn(conn, keys)
}

/// Generic variant of [`query_term_details_batch_conn`] accepting `&[&str]` or `&[String]`
/// without cloning lookup keys.
pub fn query_term_details_batch_strs_conn<S: AsRef<str>>(
    conn: &Connection,
    keys: &[S],
) -> Result<Vec<BatchTermRow>, DictError> {
    if keys.is_empty() {
        return Ok(Vec::new());
    }
    let mut seen_rowids: HashSet<i64> = HashSet::new();
    let mut results: Vec<BatchTermRow> = Vec::new();

    for chunk in keys.chunks(64) {
        let (bucket, sql, _) = select_batch_bucket(chunk.len());
        let last = chunk[chunk.len() - 1].as_ref();
        let mut stmt = conn.prepare_cached(sql)?;
        let iter_params = (0..bucket).map(|i| chunk.get(i).map_or(last, AsRef::as_ref));
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

/// Loads each dictionary's [`FrequencyMode`] (`rank-based` vs `occurrence-based`) from
/// `directory`, safely falling back to [`FrequencyMode::RankBased`] on legacy read-only DBs
/// that predate the `frequency_mode` column.
pub fn query_dictionary_frequency_modes_conn(
    conn: &Connection,
) -> Result<HashMap<i64, FrequencyMode>, DictError> {
    let mut modes = HashMap::new();
    if directory_has_column_conn(conn, "frequency_mode")? {
        let mut stmt = conn.prepare_cached(
            "SELECT dic_id, COALESCE(frequency_mode, 'rank-based') FROM directory;",
        )?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let dic_id: i64 = row.get(0)?;
            let mode_str = row_text_utf8(row, 1)?;
            modes.insert(dic_id, FrequencyMode::from_db_str(&mode_str));
        }
    } else {
        let mut stmt = conn.prepare_cached("SELECT dic_id FROM directory;")?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let dic_id: i64 = row.get(0)?;
            modes.insert(dic_id, FrequencyMode::RankBased);
        }
    }
    Ok(modes)
}

/// Queries enabled `term_meta_bank` (`mode = 'freq'`) rows for `(expression, caller_reading)`
/// paired with each dictionary's title and [`FrequencyMode`] (`rank-based` vs `occurrence-based`).
pub fn query_term_frequencies_with_mode_conn(
    conn: &Connection,
    expression: &str,
    caller_reading: &str,
) -> Result<Vec<TermFrequencyWithMode>, DictError> {
    if expression.is_empty() {
        return Ok(Vec::new());
    }
    let has_freq_mode = directory_has_column_conn(conn, "frequency_mode")?;
    let sql = if has_freq_mode {
        "SELECT m.dic_id, d.title, COALESCE(d.frequency_mode, 'rank-based'), m.type, m.data \
         FROM term_meta_bank m \
         JOIN directory d ON d.dic_id = m.dic_id \
         WHERE m.expression = ?1 AND m.mode = 'freq' \
           AND NOT EXISTS (SELECT 1 FROM dict_disabled WHERE dict_disabled.dic_id = m.dic_id) \
         ORDER BY m.dic_id ASC;"
    } else {
        "SELECT m.dic_id, d.title, 'rank-based', m.type, m.data \
         FROM term_meta_bank m \
         JOIN directory d ON d.dic_id = m.dic_id \
         WHERE m.expression = ?1 AND m.mode = 'freq' \
           AND NOT EXISTS (SELECT 1 FROM dict_disabled WHERE dict_disabled.dic_id = m.dic_id) \
         ORDER BY m.dic_id ASC;"
    };

    let mut stmt = conn.prepare_cached(sql)?;
    let mut rows = stmt.query(params![expression])?;
    let mut results = Vec::new();
    while let Some(row) = rows.next()? {
        let dic_id: i64 = row.get(0)?;
        let dictionary_title = row_text_utf8(row, 1)?;
        let mode_str = row_text_utf8(row, 2)?;
        let frequency_mode = FrequencyMode::from_db_str(&mode_str);
        let blob_type: i32 = row.get(3)?;
        let data = row_bytes(row, 4)?;
        let parsed = parse_freq_blob_with_mode_utf8(
            blob_type,
            &data,
            expression,
            caller_reading,
            frequency_mode,
        );
        if parsed.matched {
            results.push(TermFrequencyWithMode {
                dic_id,
                dictionary_title,
                frequency_mode,
                parsed,
            });
        }
    }
    Ok(results)
}

/// Computes the exclusive upper UTF-8 bound for a literal prefix so SQLite can use
/// B-tree range scans (`col >= prefix AND col < upper`) alongside `LIKE ... ESCAPE '\'`,
/// safely handling multi-byte UTF-8 and supplementary-plane CJK Extensions B–J (PR #2522).
fn next_prefix_bound(prefix: &str) -> Option<String> {
    for (byte_idx, ch) in prefix.char_indices().rev() {
        let code = ch as u32;
        let next_code = if code == 0xD7FF {
            0xE000
        } else if code < u32::from(char::MAX) {
            code + 1
        } else {
            continue;
        };
        if let Some(next_ch) = char::from_u32(next_code) {
            let mut bound = String::with_capacity(byte_idx + next_ch.len_utf8());
            bound.push_str(&prefix[..byte_idx]);
            bound.push(next_ch);
            return Some(bound);
        }
    }
    None
}

fn read_batch_term_row(row: &Row<'_>) -> rusqlite::Result<BatchTermRow> {
    Ok(BatchTermRow {
        rowid: row.get(0)?,
        dic_id: row.get(1)?,
        expression: row_text_utf8(row, 2)?,
        reading: row_text_utf8(row, 3)?,
        def_tags: row_text_utf8(row, 4)?,
        term_tags: row_text_utf8(row, 5)?,
        rules: row_text_utf8(row, 6)?,
        score: row.get(7)?,
        glossary: row_text_utf8(row, 8)?,
        sequence: row.get(9)?,
    })
}

fn run_wildcard_query_for_pattern(
    conn: &Connection,
    raw_query: &str,
    limit: usize,
    seen_rowids: &mut HashSet<i64>,
    results: &mut Vec<BatchTermRow>,
) -> Result<(), DictError> {
    let like_pattern = crate::searchcontroller::wildcard_to_like_pattern(raw_query);
    if like_pattern.is_empty() {
        return Ok(());
    }
    let literal_prefix = crate::searchcontroller::wildcard_literal_prefix(raw_query);
    // SQLite `LIKE` is ASCII case-insensitive by default; only attach the BINARY range bound
    // when the non-empty prefix has no ASCII letters (e.g. Japanese kana/kanji) so B-tree
    // prefix indexes (`idx_term_bank_combo`, `idx_term_bank_reading`) are used without
    // losing case-insensitive ASCII wildcard matching.
    let prefix_range = if !literal_prefix.is_empty()
        && !literal_prefix.bytes().any(|b| b.is_ascii_alphabetic())
    {
        next_prefix_bound(literal_prefix).map(|upper| (literal_prefix, upper))
    } else {
        None
    };

    let sql_limit = limit as i64;
    if let Some((lower, upper)) = prefix_range {
        const EXPR_PREFIX_SQL: &str = "\
            SELECT rowid, dic_id, expression, reading, def_tags, term_tags, rules, score, glossary, sequence \
            FROM term_bank \
            WHERE expression >= ?2 AND expression < ?3 AND expression LIKE ?1 ESCAPE '\\' \
              AND NOT EXISTS (SELECT 1 FROM dict_disabled WHERE dict_disabled.dic_id = term_bank.dic_id) \
            LIMIT ?4;";
        const READ_PREFIX_SQL: &str = "\
            SELECT rowid, dic_id, expression, reading, def_tags, term_tags, rules, score, glossary, sequence \
            FROM term_bank \
            WHERE reading >= ?2 AND reading < ?3 AND reading LIKE ?1 ESCAPE '\\' \
              AND NOT EXISTS (SELECT 1 FROM dict_disabled WHERE dict_disabled.dic_id = term_bank.dic_id) \
            LIMIT ?4;";

        for sql in [EXPR_PREFIX_SQL, READ_PREFIX_SQL] {
            let mut stmt = conn.prepare_cached(sql)?;
            let mut rows = stmt.query(params![&like_pattern, lower, &upper, sql_limit])?;
            while let Some(row) = rows.next()? {
                let rowid: i64 = row.get(0)?;
                if seen_rowids.insert(rowid) {
                    results.push(read_batch_term_row(row)?);
                }
            }
        }
    } else {
        const EXPR_LIKE_SQL: &str = "\
            SELECT rowid, dic_id, expression, reading, def_tags, term_tags, rules, score, glossary, sequence \
            FROM term_bank \
            WHERE expression LIKE ?1 ESCAPE '\\' \
              AND NOT EXISTS (SELECT 1 FROM dict_disabled WHERE dict_disabled.dic_id = term_bank.dic_id) \
            LIMIT ?2;";
        const READ_LIKE_SQL: &str = "\
            SELECT rowid, dic_id, expression, reading, def_tags, term_tags, rules, score, glossary, sequence \
            FROM term_bank \
            WHERE reading LIKE ?1 ESCAPE '\\' \
              AND NOT EXISTS (SELECT 1 FROM dict_disabled WHERE dict_disabled.dic_id = term_bank.dic_id) \
            LIMIT ?2;";

        for sql in [EXPR_LIKE_SQL, READ_LIKE_SQL] {
            let mut stmt = conn.prepare_cached(sql)?;
            let mut rows = stmt.query(params![&like_pattern, sql_limit])?;
            while let Some(row) = rows.next()? {
                let rowid: i64 = row.get(0)?;
                if seen_rowids.insert(rowid) {
                    results.push(read_batch_term_row(row)?);
                }
            }
        }
    }
    Ok(())
}

/// Performs a bounded wildcard search (`*`/`＊` = any chars, `?`/`？` = single char) across
/// enabled `term_bank.expression` and `term_bank.reading` rows (H6.2).
///
/// Escapes SQL `LIKE` metacharacters (`%`, `_`, `\`), trims leading/trailing whitespace
/// (PR #2506), preserves SQLite B-tree prefix index usage when there is no leading wildcard,
/// and caps results at `limit.min(MAX_WILDCARD_LIMIT)`.
pub fn search_terms_wildcard(
    conn: &Connection,
    query: &str,
    limit: usize,
) -> Result<Vec<BatchTermRow>, DictError> {
    let trimmed = query.trim();
    if trimmed.is_empty() || limit == 0 {
        return Ok(Vec::new());
    }
    let capped_limit = limit.min(MAX_WILDCARD_LIMIT);
    let mut seen_rowids: HashSet<i64> = HashSet::new();
    let mut results: Vec<BatchTermRow> = Vec::new();

    run_wildcard_query_for_pattern(conn, trimmed, capped_limit, &mut seen_rowids, &mut results)?;

    // Also match hiragana readings when the wildcard query contains katakana (e.g. `タベ*`).
    let hira_query = crate::normalize::kata_to_hira_str(trimmed);
    if hira_query != trimmed && results.len() < capped_limit {
        run_wildcard_query_for_pattern(
            conn,
            &hira_query,
            capped_limit,
            &mut seen_rowids,
            &mut results,
        )?;
    }

    results.sort_by_key(|r| (r.dic_id, r.sequence, r.rowid));
    results.truncate(capped_limit);
    Ok(results)
}

/// Alias of [`search_terms_wildcard`] following this module's `*_conn` naming convention.
pub fn search_terms_wildcard_conn(
    conn: &Connection,
    query: &str,
    limit: usize,
) -> Result<Vec<BatchTermRow>, DictError> {
    search_terms_wildcard(conn, query, limit)
}

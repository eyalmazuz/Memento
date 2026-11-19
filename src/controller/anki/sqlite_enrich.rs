//! SQLite dictionary enrichment for Anki cards using the shared read-only dictionary DB.

use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::path::{Path, PathBuf};

use memento_anki::glossary::build_glossary;
use memento_anki::note::{
    build_frequencies_html, find_stroke_count, format_kanji_readings, format_pitch,
    format_pitch_categories, format_pitch_graphs, format_pitch_positions, frequency_stats,
    is_kifuku_applicable, FrequencyEntry, PitchCategoryInput, PitchEntry, PitchGraphEntry,
    StrokeCountStat,
};
use memento_dict::databasemanager;
use memento_dict::searchcontroller::{self, TagSortItem};
use memento_dict::sqlite_db::{self, rusqlite::types::ValueRef};
use memento_dict::yomidbbuilder;
use memento_net::anki::{to_qjson, MediaFilePayload};
use memento_util::imageutils::generate_pitch_graph;

use crate::controller::audio::sanitize_anki_filename;
use crate::controller::dict::{shared_readonly_db, DictTag};

use super::context::{encode_base64, GlossaryDefinition, NoteFieldsContext, NoteTag};
use super::css_glossary::scope_dictionary_css;

#[inline]
fn dict_sort_key(dictionary_order: &HashMap<i64, i64>, dic_id: i64) -> (i64, i64) {
    (
        dictionary_order.get(&dic_id).copied().unwrap_or(i64::MAX),
        dic_id,
    )
}

fn safe_dict_subdir(res_root: &Path, dict_name: &str) -> Option<PathBuf> {
    if dict_name.is_empty()
        || dict_name == "."
        || dict_name == ".."
        || dict_name.contains('/')
        || dict_name.contains('\\')
        || dict_name.contains('\0')
    {
        return None;
    }
    Some(res_root.join(dict_name))
}

fn resolve_dictionary_resources(
    resource_roots: &[PathBuf],
    dic_id: i64,
    dict_name: &str,
) -> (Option<PathBuf>, Option<PathBuf>, String) {
    let mut found_dir: Option<PathBuf> = None;
    for root in resource_roots {
        if root.as_os_str().is_empty() {
            continue;
        }
        if let Ok(by_id) = yomidbbuilder::dictionary_resource_dir(root, dic_id) {
            if by_id.is_dir() {
                if found_dir.is_none() {
                    found_dir = Some(by_id.clone());
                }
                let style_path = by_id.join("styles.css");
                if let Ok(css) = fs::read_to_string(&style_path) {
                    let canon = fs::canonicalize(&by_id).ok();
                    return (Some(by_id), canon, scope_dictionary_css(&css, dict_name));
                }
            }
        }
        if let Some(by_name) = safe_dict_subdir(root, dict_name) {
            if by_name.is_dir() {
                if found_dir.is_none() {
                    found_dir = Some(by_name.clone());
                }
                let style_path = by_name.join("styles.css");
                if let Ok(css) = fs::read_to_string(&style_path) {
                    let canon = fs::canonicalize(&by_name).ok();
                    return (Some(by_name), canon, scope_dictionary_css(&css, dict_name));
                }
            }
        }
    }
    let canon = found_dir.as_ref().and_then(|dir| fs::canonicalize(dir).ok());
    (found_dir, canon, String::new())
}

fn resolve_sorted_tags(
    tag_bank: &HashMap<(i64, String), DictTag>,
    dic_id: i64,
    raw_space_separated: &str,
) -> Vec<(NoteTag, i64, i64)> {
    let mut seen = HashSet::new();
    let mut collected = Vec::new();
    for token in raw_space_separated.split_whitespace() {
        if token.is_empty() || !seen.insert(token) {
            continue;
        }
        if let Some(info) = tag_bank.get(&(dic_id, token.to_owned())) {
            collected.push((
                NoteTag {
                    name: token.to_owned(),
                    category: info.category.clone(),
                    notes: info.notes.clone(),
                },
                info.order,
                info.score,
            ));
        } else {
            collected.push((
                NoteTag {
                    name: token.to_owned(),
                    category: String::new(),
                    notes: String::new(),
                },
                0,
                0,
            ));
        }
    }
    if collected.len() > 1 {
        let sort_items: Vec<TagSortItem> = collected
            .iter()
            .enumerate()
            .map(|(original_index, (_, order, score))| TagSortItem {
                original_index,
                order: *order,
                score: *score,
            })
            .collect();
        let order_indices = searchcontroller::sort_tags(sort_items);
        order_indices
            .into_iter()
            .filter_map(|idx| collected.get(idx).cloned())
            .collect()
    } else {
        collected
    }
}

fn apply_frequency_rows(
    ctx: &mut NoteFieldsContext,
    titles: &HashMap<i64, String>,
    dictionary_order: &HashMap<i64, i64>,
    mut frequency_rows: Vec<sqlite_db::FreqRow>,
) {
    frequency_rows.sort_by_key(|row| dict_sort_key(dictionary_order, row.dic_id));
    ctx.frequency_entries = frequency_rows
        .iter()
        .filter_map(|row| {
            let dict = titles.get(&row.dic_id)?;
            Some((dict.clone(), String::from_utf16_lossy(&row.frequency)))
        })
        .collect();

    let dict_u16: Vec<Vec<u16>> = ctx
        .frequency_entries
        .iter()
        .map(|(d, _)| d.encode_utf16().collect())
        .collect();
    let freq_u16: Vec<Vec<u16>> = ctx
        .frequency_entries
        .iter()
        .map(|(_, f)| f.encode_utf16().collect())
        .collect();
    let entries: Vec<FrequencyEntry<'_>> = dict_u16
        .iter()
        .zip(&freq_u16)
        .map(|(d, f)| FrequencyEntry {
            dictionary: d,
            freq: f,
        })
        .collect();
    ctx.frequencies = String::from_utf16_lossy(&build_frequencies_html(&entries, false, false));
    let (_, harmonic, average) = frequency_stats(&entries);
    ctx.frequency_harmonic_rank = harmonic
        .map(|v| v.to_string())
        .unwrap_or_else(|| "9999999".to_owned());
    ctx.frequency_average_rank = average
        .map(|v| v.to_string())
        .unwrap_or_else(|| "9999999".to_owned());
    ctx.frequency_harmonic_occurrence = harmonic
        .map(|v| v.to_string())
        .unwrap_or_else(|| "0".to_owned());
    ctx.frequency_average_occurrence = average
        .map(|v| v.to_string())
        .unwrap_or_else(|| "0".to_owned());
}

fn row_text(row: &sqlite_db::rusqlite::Row<'_>, idx: usize) -> Result<String, String> {
    match row.get_ref(idx).map_err(|e| e.to_string())? {
        ValueRef::Null => Ok(String::new()),
        ValueRef::Text(bytes) | ValueRef::Blob(bytes) => {
            Ok(String::from_utf8_lossy(bytes).into_owned())
        }
        other => Err(format!(
            "unexpected SQLite column {idx} type: {:?}",
            other.data_type()
        )),
    }
}

fn row_bytes(row: &sqlite_db::rusqlite::Row<'_>, idx: usize) -> Result<Vec<u8>, String> {
    match row.get_ref(idx).map_err(|e| e.to_string())? {
        ValueRef::Null => Ok(Vec::new()),
        ValueRef::Blob(bytes) | ValueRef::Text(bytes) => Ok(bytes.to_vec()),
        other => Err(format!(
            "unexpected SQLite column {idx} type: {:?}",
            other.data_type()
        )),
    }
}

struct KanjiEnrichRow {
    rowid: i64,
    dic_id: i64,
    onyomi: String,
    kunyomi: String,
    tags: String,
    meanings: Vec<String>,
    stats: String,
}

struct TermEnrichRow {
    rowid: i64,
    dic_id: i64,
    glossary: String,
    rules: String,
    def_tags: String,
    term_tags: String,
    score: i64,
    sequence: i64,
}

fn match_selection_order(
    active_rowids: &[(i64, i64)],
    selected_occurrences: &[(String, usize)],
    dic_id: i64,
    rowid: i64,
    dict_name: &str,
    occ_idx: usize,
    fallback_len: usize,
) -> Option<usize> {
    if !active_rowids.is_empty() {
        active_rowids
            .iter()
            .position(|&(sel_dic, sel_row)| sel_dic == dic_id && sel_row == rowid)
    } else if !selected_occurrences.is_empty() {
        selected_occurrences
            .iter()
            .position(|(sel_name, sel_occ)| sel_name == dict_name && *sel_occ == occ_idx)
    } else {
        Some(fallback_len)
    }
}

/// Enriches `ctx` directly from the Yomitan SQLite database (`term_bank` / `kanji_bank` /
/// `tag_bank` / `term_meta_bank` / `kanji_meta_bank`) and dictionary `styles.css` + media
/// resources using `memento_anki::glossary::build_glossary`.
///
/// Reuses the shared read-only SQLite connection and cached metadata snapshot from
/// `crate::controller::dict::shared_readonly_db`, honors `ctx.dictionary_order` when
/// populated, and matches `ctx.selected_rowids` (or `(dictionary_id, rowid)` on
/// `ctx.glossary_definitions`) before falling back to `selected_occurrences`.
pub fn populate_note_context_from_sqlite(
    ctx: &mut NoteFieldsContext,
    db_path: &Path,
    resource_roots: &[PathBuf],
    selected_occurrences: &[(String, usize)],
) -> Result<(), String> {
    let dictionary_order = ctx.dictionary_order.clone();
    let selected_rowids = ctx.selected_rowids.clone();
    populate_note_context_from_sqlite_ordered(
        ctx,
        db_path,
        resource_roots,
        selected_occurrences,
        &selected_rowids,
        &dictionary_order,
    )
}

/// Enriches `ctx` from the shared read-only Yomitan SQLite database while explicitly
/// honoring `selected_rowids` (`(dic_id, rowid)`) and `dictionary_order` (`dic_id -> priority`).
pub fn populate_note_context_from_sqlite_ordered(
    ctx: &mut NoteFieldsContext,
    db_path: &Path,
    resource_roots: &[PathBuf],
    selected_occurrences: &[(String, usize)],
    selected_rowids: &[(i64, i64)],
    dictionary_order: &HashMap<i64, i64>,
) -> Result<(), String> {
    if !db_path.is_file() {
        return Ok(());
    }

    let db = shared_readonly_db(db_path).map_err(|e| e.to_string())?;
    let meta = db.metadata_snapshot().map_err(|e| e.to_string())?;
    let titles = &meta.names;
    let tag_bank = &meta.tags;

    let inferred_rowids: Vec<(i64, i64)> = if selected_rowids.is_empty() {
        let from_ctx: Vec<(i64, i64)> = ctx
            .glossary_definitions
            .iter()
            .filter_map(|def| {
                (def.dictionary_id != 0)
                    .then_some(def.rowid)
                    .flatten()
                    .map(|rowid| (def.dictionary_id, rowid))
            })
            .collect();
        if !from_ctx.is_empty() && from_ctx.len() == ctx.glossary_definitions.len() {
            from_ctx
        } else {
            Vec::new()
        }
    } else {
        Vec::new()
    };
    let active_rowids: &[(i64, i64)] = if !selected_rowids.is_empty() {
        selected_rowids
    } else {
        &inferred_rowids
    };

    let is_kanji = ctx.is_kanji || !ctx.character.is_empty();
    if is_kanji {
        let target_char = if !ctx.character.is_empty() {
            ctx.character.clone()
        } else {
            ctx.expression.clone()
        };
        ctx.character = target_char.clone();
        let char_u16: Vec<u16> = target_char.encode_utf16().collect();

        let (mut kanji_rows, freq_rows) = db.with_conn(|conn| {
            const KANJI_SQL: &str =
                "SELECT rowid, dic_id, onyomi, kunyomi, tags, meanings, stats FROM kanji_bank \
                 WHERE dic_id NOT IN (SELECT dic_id FROM dict_disabled) AND char = ?1 \
                 ORDER BY dic_id ASC, rowid ASC;";
            let mut stmt = conn.prepare_cached(KANJI_SQL).map_err(|e| e.to_string())?;
            let mut rows = stmt
                .query([target_char.as_str()])
                .map_err(|e| e.to_string())?;
            let mut out = Vec::new();
            while let Some(row) = rows.next().map_err(|e| e.to_string())? {
                let rowid: i64 = row.get(0).map_err(|e| e.to_string())?;
                let dic_id: i64 = row.get(1).map_err(|e| e.to_string())?;
                let onyomi = row_text(row, 2)?;
                let kunyomi = row_text(row, 3)?;
                let tags = row_text(row, 4)?;
                let meanings_blob = row_bytes(row, 5)?;
                let meanings = databasemanager::json_array_to_utf8_strings(&meanings_blob);
                let stats = row_text(row, 6)?;
                out.push(KanjiEnrichRow {
                    rowid,
                    dic_id,
                    onyomi,
                    kunyomi,
                    tags,
                    meanings,
                    stats,
                });
            }

            let mut freq_rows = Vec::new();
            let freq_opt = sqlite_db::get_kanji_frequencies_conn(conn, &char_u16, &mut freq_rows)
                .ok()
                .map(|()| freq_rows);
            Ok::<_, String>((out, freq_opt))
        })?;

        kanji_rows.sort_by_key(|row| (dict_sort_key(dictionary_order, row.dic_id), row.rowid));

        let mut occ_counts: HashMap<String, usize> = HashMap::new();
        let mut matched_defs: Vec<(usize, GlossaryDefinition)> = Vec::new();
        let mut onyomi_parts: Vec<Vec<u16>> = Vec::new();
        let mut kunyomi_parts: Vec<Vec<u16>> = Vec::new();
        let mut stroke_found: Option<String> = None;
        let mut accumulated_tags: Vec<NoteTag> = Vec::new();
        let mut seen_tag_names: HashSet<String> = HashSet::new();

        for row in kanji_rows {
            if meta.disabled.contains(&row.dic_id) {
                continue;
            }
            let Some(dict_name) = titles.get(&row.dic_id).cloned() else {
                continue;
            };
            let occ_idx = *occ_counts
                .entry(dict_name.clone())
                .and_modify(|c| *c += 1)
                .or_insert(0);
            let Some(order_pos) = match_selection_order(
                active_rowids,
                selected_occurrences,
                row.dic_id,
                row.rowid,
                &dict_name,
                occ_idx,
                matched_defs.len(),
            ) else {
                continue;
            };

            for part in row.onyomi.split_whitespace() {
                if !part.is_empty() {
                    onyomi_parts.push(part.encode_utf16().collect());
                }
            }
            for part in row.kunyomi.split_whitespace() {
                if !part.is_empty() {
                    kunyomi_parts.push(part.encode_utf16().collect());
                }
            }

            let resolved_tags = resolve_sorted_tags(tag_bank, row.dic_id, &row.tags);
            for (tag, _, _) in &resolved_tags {
                if seen_tag_names.insert(tag.name.clone()) {
                    accumulated_tags.push(tag.clone());
                }
            }

            if stroke_found.is_none() {
                if let Ok(serde_json::Value::Object(stats_map)) =
                    serde_json::from_str::<serde_json::Value>(&row.stats)
                {
                    let stat_pairs: Vec<(Vec<u16>, Vec<u16>)> = stats_map
                        .iter()
                        .filter_map(|(stat_key, val)| {
                            let tag_info = tag_bank.get(&(row.dic_id, stat_key.clone()))?;
                            let val_s = match val {
                                serde_json::Value::String(s) => s.clone(),
                                serde_json::Value::Number(n) => n.to_string(),
                                _ => return None,
                            };
                            Some((
                                tag_info.notes.encode_utf16().collect(),
                                val_s.encode_utf16().collect(),
                            ))
                        })
                        .collect();
                    let refs: Vec<StrokeCountStat<'_>> = stat_pairs
                        .iter()
                        .map(|(notes, value)| StrokeCountStat { notes, value })
                        .collect();
                    if let Some(stroke_u16) = find_stroke_count(&refs) {
                        stroke_found = Some(String::from_utf16_lossy(stroke_u16));
                    }
                }
            }

            matched_defs.push((
                order_pos,
                GlossaryDefinition {
                    dictionary_id: row.dic_id,
                    dictionary_name: dict_name,
                    rowid: Some(row.rowid),
                    glossary: row.meanings.join("\n"),
                    tags: resolved_tags.iter().map(|(t, _, _)| t.name.clone()).collect(),
                    kanji_meanings: row.meanings,
                    ..Default::default()
                },
            ));
        }

        if !matched_defs.is_empty() {
            matched_defs.sort_by_key(|(pos, _)| *pos);
            ctx.glossary_definitions = matched_defs.into_iter().map(|(_, def)| def).collect();
            let on_refs: Vec<&[u16]> = onyomi_parts.iter().map(Vec::as_slice).collect();
            let kun_refs: Vec<&[u16]> = kunyomi_parts.iter().map(Vec::as_slice).collect();
            ctx.onyomi = String::from_utf16_lossy(&format_kanji_readings(&on_refs));
            ctx.kunyomi = String::from_utf16_lossy(&format_kanji_readings(&kun_refs));
            if let Some(strokes) = stroke_found.filter(|s| !s.is_empty()) {
                ctx.stroke_count = strokes;
            }
            ctx.tag_entries = accumulated_tags;
            ctx.tags = ctx.tag_entries.iter().map(|t| t.name.clone()).collect();
        }

        if let Some(freq_rows) = freq_rows {
            apply_frequency_rows(ctx, titles, dictionary_order, freq_rows);
        }
        return Ok(());
    }

    // Term card SQLite enrichment: fetch rows while holding the SQLite connection lock briefly,
    // then perform JSON parsing, CSS scoping, and resource I/O outside the lock.
    let mut term_rows = db.with_conn(|conn| {
        const TERM_SQL: &str = "SELECT rowid, dic_id, glossary, rules, def_tags, term_tags, score, sequence \
            FROM term_bank \
            WHERE expression = ?1 \
              AND (reading = ?2 OR (?2 = ?1 AND reading = '')) \
              AND dic_id NOT IN (SELECT dic_id FROM dict_disabled) \
            ORDER BY dic_id ASC, score DESC, sequence ASC, rowid ASC;";
        let mut stmt = conn.prepare_cached(TERM_SQL).map_err(|e| e.to_string())?;
        let mut rows = stmt
            .query([ctx.expression.as_str(), ctx.reading.as_str()])
            .map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        while let Some(row) = rows.next().map_err(|e| e.to_string())? {
            out.push(TermEnrichRow {
                rowid: row.get(0).map_err(|e| e.to_string())?,
                dic_id: row.get(1).map_err(|e| e.to_string())?,
                glossary: row_text(row, 2)?,
                rules: row_text(row, 3)?,
                def_tags: row_text(row, 4)?,
                term_tags: row_text(row, 5)?,
                score: row.get(6).map_err(|e| e.to_string())?,
                sequence: row.get(7).map_err(|e| e.to_string())?,
            });
        }
        Ok::<_, String>(out)
    })?;

    term_rows.sort_by_key(|row| {
        (
            dict_sort_key(dictionary_order, row.dic_id),
            Reverse(row.score),
            row.sequence,
            row.rowid,
        )
    });

    let mut occ_counts: HashMap<String, usize> = HashMap::new();
    let mut matched_defs: Vec<(usize, GlossaryDefinition)> = Vec::new();
    let mut all_rules: Vec<String> = Vec::new();
    let mut term_tag_candidates: Vec<(NoteTag, i64, i64)> = Vec::new();
    let mut def_tag_candidates: Vec<(NoteTag, i64, i64)> = Vec::new();
    let mut seen_term_tags: HashSet<String> = HashSet::new();
    let mut seen_def_tags: HashSet<String> = HashSet::new();
    let mut pos_tags: Vec<String> = Vec::new();
    let mut resource_cache: HashMap<i64, (Option<PathBuf>, Option<PathBuf>, String)> =
        HashMap::new();

    for row in term_rows {
        if meta.disabled.contains(&row.dic_id) {
            continue;
        }
        let Some(dict_name) = titles.get(&row.dic_id).cloned() else {
            continue;
        };

        let parsed_val: serde_json::Value = match serde_json::from_str(&row.glossary) {
            Ok(val) => val,
            Err(_) => serde_json::Value::Array(vec![serde_json::Value::String(row.glossary)]),
        };
        let filtered_items: Vec<serde_json::Value> = match parsed_val {
            serde_json::Value::Array(arr) => {
                arr.into_iter().filter(|item| !item.is_array()).collect()
            }
            other => vec![other],
        };
        if filtered_items.is_empty() {
            continue;
        }

        let occ_idx = *occ_counts
            .entry(dict_name.clone())
            .and_modify(|c| *c += 1)
            .or_insert(0);
        let Some(order_pos) = match_selection_order(
            active_rowids,
            selected_occurrences,
            row.dic_id,
            row.rowid,
            &dict_name,
            occ_idx,
            matched_defs.len(),
        ) else {
            continue;
        };

        for rule in row.rules.split_whitespace() {
            if !rule.is_empty() {
                all_rules.push(rule.to_owned());
            }
        }

        let resolved_def_tags = resolve_sorted_tags(tag_bank, row.dic_id, &row.def_tags);
        let resolved_term_tags = resolve_sorted_tags(tag_bank, row.dic_id, &row.term_tags);
        for item in &resolved_term_tags {
            if seen_term_tags.insert(item.0.name.clone()) {
                term_tag_candidates.push(item.clone());
            }
            if item.0.category == "partOfSpeech" && !pos_tags.contains(&item.0.name) {
                pos_tags.push(item.0.name.clone());
            }
        }
        for item in &resolved_def_tags {
            if seen_def_tags.insert(item.0.name.clone()) {
                def_tag_candidates.push(item.clone());
            }
            if item.0.category == "partOfSpeech" && !pos_tags.contains(&item.0.name) {
                pos_tags.push(item.0.name.clone());
            }
        }

        let (res_dir_opt, canon_base, scoped_css): &(Option<PathBuf>, Option<PathBuf>, String) =
            &*resource_cache
                .entry(row.dic_id)
                .or_insert_with(|| resolve_dictionary_resources(resource_roots, row.dic_id, &dict_name));
        let basepath_str = canon_base
            .as_ref()
            .or(res_dir_opt.as_ref())
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        let basepath_u16: Vec<u16> = basepath_str.encode_utf16().collect();

        let qjson_defs: Vec<memento_anki::json::JsonValue> =
            filtered_items.iter().map(to_qjson).collect();

        let mut verified_files: HashMap<String, PathBuf> = HashMap::new();
        let (rendered_u16, file_map) =
            build_glossary(&qjson_defs, &basepath_u16, |full_path_u16| {
                let base = canon_base.as_ref()?;
                let candidate = PathBuf::from(String::from_utf16(full_path_u16).ok()?);
                let canon_file = fs::canonicalize(&candidate).ok()?;
                if !canon_file.starts_with(base) || !canon_file.is_file() {
                    return None;
                }
                let file = File::open(&canon_file).ok()?;
                let md5 = memento_util::fileutils::calculate_md5(file).ok()?;
                let hex = format!("{:032x}", u128::from_be_bytes(md5));
                let mut hex_u16 = [0u16; 32];
                for (dst, b) in hex_u16.iter_mut().zip(hex.bytes()) {
                    *dst = u16::from(b);
                }
                verified_files.insert(String::from_utf16_lossy(full_path_u16), canon_file);
                Some(hex_u16)
            });

        let mut media_files = Vec::new();
        for (full_path_u16, hash_name_u16) in file_map {
            let key = String::from_utf16_lossy(&full_path_u16);
            let Some(canon_file) = verified_files.get(&key) else {
                continue;
            };
            let Ok(bytes) = fs::read(canon_file) else {
                continue;
            };
            let Ok(data_base64) = encode_base64(&bytes) else {
                continue;
            };
            let raw_name = String::from_utf16_lossy(&hash_name_u16);
            let safe_name = sanitize_anki_filename(&raw_name, ".bin");
            media_files.push(MediaFilePayload {
                filename: safe_name,
                data_base64,
            });
        }

        let rendered_items: Vec<String> = rendered_u16
            .into_iter()
            .map(|item| String::from_utf16_lossy(&item))
            .collect();

        matched_defs.push((
            order_pos,
            GlossaryDefinition {
                dictionary_id: row.dic_id,
                dictionary_name: dict_name,
                rowid: Some(row.rowid),
                glossary: rendered_items.join("\n"),
                tags: resolved_def_tags
                    .iter()
                    .map(|(t, _, _)| t.name.clone())
                    .collect(),
                rendered_items,
                stylesheet: scoped_css.clone(),
                media_files,
                ..Default::default()
            },
        ));
    }

    if !matched_defs.is_empty() {
        matched_defs.sort_by_key(|(pos, _)| *pos);
        ctx.glossary_definitions = matched_defs.into_iter().map(|(_, def)| def).collect();

        let chosen_tags = if !term_tag_candidates.is_empty() {
            term_tag_candidates
        } else {
            def_tag_candidates
        };
        if !chosen_tags.is_empty() {
            let sort_items: Vec<TagSortItem> = chosen_tags
                .iter()
                .enumerate()
                .map(|(original_index, (_, order, score))| TagSortItem {
                    original_index,
                    order: *order,
                    score: *score,
                })
                .collect();
            let indices = searchcontroller::sort_tags(sort_items);
            ctx.tag_entries = indices
                .into_iter()
                .filter_map(|idx| chosen_tags.get(idx).map(|item| item.0.clone()))
                .collect();
            ctx.tags = ctx.tag_entries.iter().map(|t| t.name.clone()).collect();
        }
        if !pos_tags.is_empty() {
            ctx.part_of_speech = pos_tags.join(", ");
        } else if !all_rules.is_empty() {
            let mut unique_rules = Vec::new();
            for r in &all_rules {
                if !unique_rules.contains(r) {
                    unique_rules.push(r.clone());
                }
            }
            ctx.part_of_speech = unique_rules.join(", ");
        }
    } else {
        // Still resolve dictionary_id on existing UI definitions if present.
        for definition in &mut ctx.glossary_definitions {
            if definition.dictionary_id == 0 {
                if let Some((&id, _)) = titles
                    .iter()
                    .find(|(_, title)| *title == &definition.dictionary_name)
                {
                    definition.dictionary_id = id;
                }
            }
        }
    }

    // Reuse cached `term_meta_bank` rows from the shared read-only DB (both freq & pitch).
    let raw_meta_rows = db.term_meta_rows_for(&ctx.expression);
    let freq_rows: Vec<sqlite_db::FreqRow> = raw_meta_rows
        .iter()
        .filter(|row| row.mode == "freq" && !meta.disabled.contains(&row.dic_id))
        .filter_map(|row| {
            let parsed = sqlite_db::parse_freq_blob_utf8(
                row.blob_type,
                &row.data,
                &ctx.expression,
                &ctx.reading,
            );
            parsed.matched.then(|| sqlite_db::FreqRow {
                dic_id: row.dic_id,
                frequency: parsed.display.encode_utf16().collect(),
            })
        })
        .collect();
    apply_frequency_rows(ctx, titles, dictionary_order, freq_rows);

    let expr_u16: Vec<u16> = ctx.expression.encode_utf16().collect();
    let read_u16: Vec<u16> = ctx.reading.encode_utf16().collect();
    let mut pitch_rows: Vec<sqlite_db::PitchRow> = raw_meta_rows
        .iter()
        .filter(|row| row.mode == "pitch" && !meta.disabled.contains(&row.dic_id))
        .filter_map(|row| {
            let null_pos = row.data.iter().position(|&b| b == 0).unwrap_or(row.data.len());
            let blob_slice = row.data.get(..null_pos).unwrap_or(&row.data);
            let parsed = databasemanager::parse_pitch_blob(blob_slice, &expr_u16, &read_u16);
            parsed.matched.then(|| sqlite_db::PitchRow {
                dic_id: row.dic_id,
                mora: parsed.mora,
                positions: parsed.positions,
                nasal: parsed.nasal,
                devoice: parsed.devoice,
                tags: parsed.tags,
                entries: parsed.entries,
            })
        })
        .collect();

    pitch_rows.sort_by_key(|row| dict_sort_key(dictionary_order, row.dic_id));
    let pitches_owned: Vec<(Vec<u16>, Vec<u8>, Vec<Vec<u16>>)> = pitch_rows
        .iter()
        .filter_map(|row| {
            let positions: Vec<u8> = row
                .positions
                .iter()
                .filter_map(|p| u8::try_from(*p).ok())
                .collect();
            (!positions.is_empty() && !row.mora.is_empty()).then(|| {
                let dict_u16 = titles
                    .get(&row.dic_id)
                    .map(|s| s.encode_utf16().collect())
                    .unwrap_or_default();
                (dict_u16, positions, row.mora.clone())
            })
        })
        .collect();

    let mora_refs: Vec<Vec<&[u16]>> = pitches_owned
        .iter()
        .map(|(_, _, mora)| mora.iter().map(Vec::as_slice).collect())
        .collect();
    let pitch_entries: Vec<PitchEntry<'_>> = pitches_owned
        .iter()
        .enumerate()
        .map(|(idx, (name, positions, _))| PitchEntry {
            dictionary_name: name,
            positions,
            mora: &mora_refs[idx],
        })
        .collect();

    let fill_u16: Vec<u16> = "rgba(0,0,0,0)".encode_utf16().collect();
    let stroke_u16: Vec<u16> = "currentColor".encode_utf16().collect();
    let svgs_owned: Vec<Vec<Vec<u16>>> = pitches_owned
        .iter()
        .map(|(_, positions, mora)| {
            let mora_len = i32::try_from(mora.len()).unwrap_or(0);
            positions
                .iter()
                .filter_map(|&pos| {
                    generate_pitch_graph(
                        mora_len,
                        pos,
                        &fill_u16,
                        &stroke_u16,
                        |n| Some(n.to_string().encode_utf16().collect()),
                        |_| None,
                    )
                })
                .collect()
        })
        .collect();
    let svg_refs: Vec<Vec<&[u16]>> = svgs_owned
        .iter()
        .map(|svgs| svgs.iter().map(Vec::as_slice).collect())
        .collect();
    let graph_entries: Vec<PitchGraphEntry<'_>> = pitches_owned
        .iter()
        .enumerate()
        .map(|(idx, (name, positions, _))| PitchGraphEntry {
            dictionary_name: name,
            positions_count: positions.len(),
            svg_graphs: &svg_refs[idx],
        })
        .collect();

    let category_inputs: Vec<PitchCategoryInput<'_>> = pitches_owned
        .iter()
        .map(|(_, positions, mora)| PitchCategoryInput {
            mora_len: mora.len(),
            positions,
        })
        .collect();
    let rule_u16: Vec<Vec<u16>> = if !all_rules.is_empty() {
        all_rules
            .iter()
            .map(|r| r.encode_utf16().collect())
            .collect()
    } else {
        ctx.tags.iter().map(|t| t.encode_utf16().collect()).collect()
    };
    let rule_refs: Vec<&[u16]> = rule_u16.iter().map(Vec::as_slice).collect();
    let can_be_kifuku = is_kifuku_applicable(&rule_refs);

    ctx.pitch = String::from_utf16_lossy(&format_pitch(&pitch_entries));
    ctx.pitch_positions = String::from_utf16_lossy(&format_pitch_positions(&pitch_entries));
    ctx.pitch_graphs = String::from_utf16_lossy(&format_pitch_graphs(&graph_entries));
    ctx.pitch_categories =
        String::from_utf16_lossy(&format_pitch_categories(&category_inputs, can_be_kifuku));

    Ok(())
}

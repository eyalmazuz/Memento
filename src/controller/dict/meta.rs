//! Lock-free pitch accent and frequency selection, deduplication, and Anki summary formatting.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};

use memento_dict::databasemanager::{self, PitchPosition};
use memento_dict::sqlite_db::{self, FrequencyMode};

use super::db::DictionaryMetadataSnapshot;
use super::{AnkiTermMetadata, TermFrequency, TermPitchDetail};

pub(super) struct SelectedFreqItem {
    pub dictionary: String,
    pub parsed: sqlite_db::ParsedFreqUtf8,
}

/// Separate rank-based vs occurrence-based frequency summaries for Anki note markers
/// (Yomitan PR #2373/#2329) so rank-based and occurrence-based values are never mixed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct AnkiFrequencySummary {
    pub html: String,
    pub rank_harmonic: Option<i64>,
    pub rank_average: Option<i64>,
    pub occurrence_harmonic: Option<i64>,
    pub occurrence_average: Option<i64>,
}

/// Compares two optional numeric frequency values under `mode`, guaranteeing that
/// unranked (`None`, non-positive, or non-finite) entries ALWAYS sort AFTER ranked
/// entries in both `rank-based` (ascending) and `occurrence-based` (descending) modes
/// (Yomitan `#1037` / `#1684`).
#[must_use]
pub fn compare_frequency_values(a: Option<f64>, b: Option<f64>, mode: FrequencyMode) -> Ordering {
    let a_valid = a.filter(|v| v.is_finite() && *v > 0.0);
    let b_valid = b.filter(|v| v.is_finite() && *v > 0.0);
    match (a_valid, b_valid) {
        (Some(av), Some(bv)) => match mode {
            FrequencyMode::RankBased => av.total_cmp(&bv),
            FrequencyMode::OccurrenceBased => bv.total_cmp(&av),
        },
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

/// Derives a standard Tokyo single-downstep integer (`0..=mora_len`) from an exact
/// `H`/`L` pitch pattern string when it matches a canonical Tokyo pattern, or `-1`
/// for non-standard / multi-drop / Kansai patterns (e.g. `"HHH"`, `"HLHL"`).
pub(super) fn derive_downstep_from_pattern(pattern: &str, mora_len: usize) -> i32 {
    if mora_len == 0 || !PitchPosition::is_valid_pattern(pattern) {
        return -1;
    }
    let levels = PitchPosition::Pattern(pattern.to_owned()).to_levels(mora_len);
    if levels.is_empty() {
        return -1;
    }
    let (mora_highs, particle_high) = if levels.len() > mora_len {
        (&levels[..mora_len], levels[mora_len])
    } else {
        let mut padded = levels;
        let tail = *padded.last().unwrap_or(&false);
        padded.resize(mora_len, tail);
        // Check directly on owned vec below.
        let is_heiban = !padded[0] && padded[1..].iter().all(|&h| h) && tail;
        if is_heiban {
            return 0;
        }
        let is_atamadaka = padded[0] && padded[1..].iter().all(|&h| !h) && !tail;
        if is_atamadaka {
            return 1;
        }
        if !padded[0] && !tail {
            if let Some(first_low_after_rise) = padded.iter().enumerate().skip(1).find_map(|(i, &h)| (!h).then_some(i)) {
                if first_low_after_rise > 1
                    && padded[1..first_low_after_rise].iter().all(|&h| h)
                    && padded[first_low_after_rise..].iter().all(|&h| !h)
                {
                    return i32::try_from(first_low_after_rise).unwrap_or(-1);
                }
            }
        }
        return -1;
    };

    if !mora_highs[0] && mora_highs[1..].iter().all(|&h| h) && particle_high {
        return 0;
    }
    if mora_highs[0] && mora_highs[1..].iter().all(|&h| !h) && !particle_high {
        return 1;
    }
    if !mora_highs[0] && !particle_high {
        let drop_pos = mora_highs
            .iter()
            .enumerate()
            .skip(1)
            .find_map(|(i, &h)| (!h).then_some(i))
            .unwrap_or(mora_len);
        if drop_pos > 1
            && mora_highs[1..drop_pos].iter().all(|&h| h)
            && mora_highs[drop_pos..].iter().all(|&h| !h)
        {
            return i32::try_from(drop_pos).unwrap_or(-1);
        }
    }
    -1
}

/// Converts a parsed `databasemanager::PitchEntryDetail` into a `(TermPitchDetail, PitchPosition)`
/// pair if valid for `mora`, supporting both integer downsteps and exact `H`/`L` pattern strings.
fn pitch_entry_to_detail(
    dict_name: &str,
    mora: &[String],
    entry: databasemanager::PitchEntryDetail,
) -> Option<(TermPitchDetail, PitchPosition)> {
    if let Some(pat) = entry.pattern {
        if mora.is_empty() || !PitchPosition::is_valid_pattern(&pat) {
            return None;
        }
        let upper = pat.to_ascii_uppercase();
        let derived_pos = derive_downstep_from_pattern(&upper, mora.len());
        let key = PitchPosition::Pattern(upper.clone());
        return Some((
            TermPitchDetail {
                dictionary: dict_name.to_owned(),
                mora: mora.to_vec(),
                position: derived_pos,
                pattern: Some(upper),
                nasal: entry.nasal,
                devoice: entry.devoice,
                tags: entry.tags,
            },
            key,
        ));
    }
    if entry.position < 0 || (entry.position as usize) > mora.len() {
        return None;
    }
    let key = PitchPosition::Downstep(entry.position as u32);
    Some((
        TermPitchDetail {
            dictionary: dict_name.to_owned(),
            mora: mora.to_vec(),
            position: entry.position,
            pattern: None,
            nasal: entry.nasal,
            devoice: entry.devoice,
            tags: entry.tags,
        },
        key,
    ))
}

/// Shared priority-ordered selector for `term_meta_bank` / `kanji_meta_bank` entries
/// that enforces Yomitan homograph reading filtering outside any database mutex lock.
fn select_ranked_meta_entries<T>(
    snapshot: &DictionaryMetadataSnapshot,
    dictionary_order: &HashMap<i64, i64>,
    raw_rows: &[sqlite_db::RawTermMetaRow],
    mode: &str,
    caller_reading: &str,
    mut parse: impl FnMut(&sqlite_db::RawTermMetaRow) -> (bool, bool, Option<T>),
) -> Vec<(String, T)> {
    let mut by_dict: HashMap<i64, (bool, Vec<(bool, T)>)> = HashMap::new();
    let mut dict_ids: Vec<i64> = Vec::new();

    for row in raw_rows {
        if row.mode != mode
            || snapshot.disabled.contains(&row.dic_id)
            || !snapshot.names.contains_key(&row.dic_id)
        {
            continue;
        }
        let (has_reading_key, exact_reading_match, parsed) = parse(row);
        let entry = by_dict.entry(row.dic_id).or_insert_with(|| {
            dict_ids.push(row.dic_id);
            (false, Vec::new())
        });
        if has_reading_key {
            entry.0 = true;
        }
        if let Some(item) = parsed {
            entry.1.push((exact_reading_match, item));
        }
    }

    dict_ids.sort_by_key(|&id| {
        (
            dictionary_order.get(&id).copied().unwrap_or(i64::MAX),
            id,
        )
    });

    let mut out = Vec::new();
    for dic_id in dict_ids {
        let Some((saw_reading_key, items)) = by_dict.remove(&dic_id) else {
            continue;
        };
        let Some(dict_name) = snapshot.names.get(&dic_id) else {
            continue;
        };
        let has_exact = !caller_reading.is_empty() && items.iter().any(|(exact, _)| *exact);
        for (exact_reading_match, item) in items {
            if !caller_reading.is_empty() {
                if has_exact && !exact_reading_match {
                    continue;
                }
                if !has_exact && saw_reading_key {
                    continue;
                }
            }
            out.push((dict_name.clone(), item));
        }
    }
    out
}

pub(super) fn select_term_frequencies(
    snapshot: &DictionaryMetadataSnapshot,
    dictionary_order: &HashMap<i64, i64>,
    raw_rows: &[sqlite_db::RawTermMetaRow],
    expression: &str,
    caller_reading: &str,
) -> Vec<SelectedFreqItem> {
    select_ranked_meta_entries(
        snapshot,
        dictionary_order,
        raw_rows,
        "freq",
        caller_reading,
        |row| {
            let freq_mode = snapshot.frequency_mode_for(row.dic_id);
            let parsed = sqlite_db::parse_freq_blob_with_mode_utf8(
                row.blob_type,
                &row.data,
                expression,
                caller_reading,
                freq_mode,
            );
            let has_reading_key = parsed.has_reading_key;
            let exact = parsed.exact_reading_match;
            (has_reading_key, exact, parsed.matched.then_some(parsed))
        },
    )
    .into_iter()
    .map(|(dictionary, parsed)| SelectedFreqItem { dictionary, parsed })
    .collect()
}

/// Queries enabled `term_meta_bank` frequency rows directly via Wave 1's
/// `query_term_frequencies_with_mode_conn`, applying dictionary-priority ordering and
/// Yomitan homograph reading filtering while preserving each dictionary's `FrequencyMode`.
pub(super) fn query_term_frequencies_with_mode_sorted(
    conn: &sqlite_db::rusqlite::Connection,
    dictionary_order: &HashMap<i64, i64>,
    expression: &str,
    caller_reading: &str,
) -> Result<Vec<sqlite_db::TermFrequencyWithMode>, sqlite_db::DictError> {
    let raw_items =
        sqlite_db::query_term_frequencies_with_mode_conn(conn, expression, caller_reading)?;
    if raw_items.is_empty() {
        return Ok(Vec::new());
    }

    let mut by_dict: HashMap<i64, (bool, Vec<sqlite_db::TermFrequencyWithMode>)> = HashMap::new();
    let mut dict_ids: Vec<i64> = Vec::new();
    for item in raw_items {
        let entry = by_dict.entry(item.dic_id).or_insert_with(|| {
            dict_ids.push(item.dic_id);
            (false, Vec::new())
        });
        if item.parsed.has_reading_key {
            entry.0 = true;
        }
        entry.1.push(item);
    }

    dict_ids.sort_by_key(|&id| {
        (
            dictionary_order.get(&id).copied().unwrap_or(i64::MAX),
            id,
        )
    });

    let mut out = Vec::new();
    for dic_id in dict_ids {
        let Some((saw_reading_key, items)) = by_dict.remove(&dic_id) else {
            continue;
        };
        let has_exact = !caller_reading.is_empty()
            && items.iter().any(|item| item.parsed.exact_reading_match);
        for item in items {
            if !caller_reading.is_empty() {
                if has_exact && !item.parsed.exact_reading_match {
                    continue;
                }
                if !has_exact && saw_reading_key {
                    continue;
                }
            }
            out.push(item);
        }
    }
    Ok(out)
}

pub(super) fn select_term_pitches(
    snapshot: &DictionaryMetadataSnapshot,
    dictionary_order: &HashMap<i64, i64>,
    raw_rows: &[sqlite_db::RawTermMetaRow],
    expression: &str,
    caller_reading: &str,
) -> Vec<TermPitchDetail> {
    let parsed_list = select_ranked_meta_entries(
        snapshot,
        dictionary_order,
        raw_rows,
        "pitch",
        caller_reading,
        |row| {
            let null_pos = row
                .data
                .iter()
                .position(|&b| b == 0)
                .unwrap_or(row.data.len());
            let parsed = databasemanager::parse_pitch_blob_utf8(
                &row.data[..null_pos],
                expression,
                caller_reading,
            );
            let has_reading_key = parsed.has_reading_key;
            let exact = parsed.exact_reading_match;
            (has_reading_key, exact, parsed.matched.then_some(parsed))
        },
    );

    let mut out = Vec::new();
    // Deduplicate identical pitch positions/patterns within each dictionary (Yomitan #2194).
    let mut seen_per_dict: HashMap<String, HashSet<(Vec<String>, PitchPosition)>> = HashMap::new();
    for (dict_name, parsed) in parsed_list {
        let seen = seen_per_dict.entry(dict_name.clone()).or_default();
        for entry in parsed.entries {
            if let Some((detail, key)) = pitch_entry_to_detail(&dict_name, &parsed.mora, entry) {
                if seen.insert((detail.mora.clone(), key)) {
                    out.push(detail);
                }
            }
        }
    }
    out
}

/// Extracts both pitch details and selected frequency entries in a single pass over `raw_rows`
/// without holding any database mutex lock.
pub(super) fn select_term_pitches_and_frequencies(
    snapshot: &DictionaryMetadataSnapshot,
    dictionary_order: &HashMap<i64, i64>,
    raw_rows: &[sqlite_db::RawTermMetaRow],
    expression: &str,
    caller_reading: &str,
) -> (Vec<TermPitchDetail>, Vec<SelectedFreqItem>) {
    let mut freq_by_dict: HashMap<i64, (bool, Vec<sqlite_db::ParsedFreqUtf8>)> = HashMap::new();
    let mut freq_dict_ids: Vec<i64> = Vec::new();
    let mut pitch_by_dict: HashMap<i64, (bool, Vec<databasemanager::PitchBlobUtf8Data>)> =
        HashMap::new();
    let mut pitch_dict_ids: Vec<i64> = Vec::new();

    for row in raw_rows {
        if snapshot.disabled.contains(&row.dic_id) || !snapshot.names.contains_key(&row.dic_id) {
            continue;
        }
        match row.mode.as_str() {
            "freq" => {
                let freq_mode = snapshot.frequency_mode_for(row.dic_id);
                let parsed = sqlite_db::parse_freq_blob_with_mode_utf8(
                    row.blob_type,
                    &row.data,
                    expression,
                    caller_reading,
                    freq_mode,
                );
                let entry = freq_by_dict.entry(row.dic_id).or_insert_with(|| {
                    freq_dict_ids.push(row.dic_id);
                    (false, Vec::new())
                });
                if parsed.has_reading_key {
                    entry.0 = true;
                }
                if parsed.matched {
                    entry.1.push(parsed);
                }
            }
            "pitch" => {
                let null_pos = row
                    .data
                    .iter()
                    .position(|&b| b == 0)
                    .unwrap_or(row.data.len());
                let parsed = databasemanager::parse_pitch_blob_utf8(
                    &row.data[..null_pos],
                    expression,
                    caller_reading,
                );
                let entry = pitch_by_dict.entry(row.dic_id).or_insert_with(|| {
                    pitch_dict_ids.push(row.dic_id);
                    (false, Vec::new())
                });
                if parsed.has_reading_key {
                    entry.0 = true;
                }
                if parsed.matched {
                    entry.1.push(parsed);
                }
            }
            _ => {}
        }
    }

    let sort_key = |id: &i64| (dictionary_order.get(id).copied().unwrap_or(i64::MAX), *id);
    freq_dict_ids.sort_by_key(sort_key);
    pitch_dict_ids.sort_by_key(sort_key);

    let mut freqs = Vec::new();
    for dic_id in freq_dict_ids {
        let Some((saw_reading_key, items)) = freq_by_dict.remove(&dic_id) else {
            continue;
        };
        let Some(dict_name) = snapshot.names.get(&dic_id) else {
            continue;
        };
        let has_exact =
            !caller_reading.is_empty() && items.iter().any(|item| item.exact_reading_match);
        for parsed in items {
            if !caller_reading.is_empty() {
                if has_exact && !parsed.exact_reading_match {
                    continue;
                }
                if !has_exact && saw_reading_key {
                    continue;
                }
            }
            freqs.push(SelectedFreqItem {
                dictionary: dict_name.clone(),
                parsed,
            });
        }
    }

    let mut pitches = Vec::new();
    for dic_id in pitch_dict_ids {
        let Some((saw_reading_key, items)) = pitch_by_dict.remove(&dic_id) else {
            continue;
        };
        let Some(dict_name) = snapshot.names.get(&dic_id) else {
            continue;
        };
        let has_exact =
            !caller_reading.is_empty() && items.iter().any(|item| item.exact_reading_match);
        // Deduplicate identical pitch positions/patterns within each dictionary (Yomitan #2194).
        let mut seen_in_dict: HashSet<(Vec<String>, PitchPosition)> = HashSet::new();
        for parsed in items {
            if !caller_reading.is_empty() {
                if has_exact && !parsed.exact_reading_match {
                    continue;
                }
                if !has_exact && saw_reading_key {
                    continue;
                }
            }
            for entry in parsed.entries {
                if let Some((detail, key)) = pitch_entry_to_detail(dict_name, &parsed.mora, entry) {
                    if seen_in_dict.insert((detail.mora.clone(), key)) {
                        pitches.push(detail);
                    }
                }
            }
        }
    }

    (pitches, freqs)
}

pub(super) fn format_term_meta_tuple(
    reading: &str,
    pitches: &[TermPitchDetail],
    freqs: Vec<SelectedFreqItem>,
) -> (String, String, String) {
    let mut freq_rank = String::new();
    let mut jpdb_rank = String::new();
    for item in freqs {
        if item.parsed.display.is_empty() {
            continue;
        }
        if item.dictionary.to_ascii_uppercase().contains("JPDB") && jpdb_rank.is_empty() {
            jpdb_rank = item.parsed.display;
        } else if freq_rank.is_empty() {
            freq_rank = format!("{} {}", item.dictionary, item.parsed.display);
        }
        if !freq_rank.is_empty() && !jpdb_rank.is_empty() {
            break;
        }
    }

    let pitch_accent = pitches
        .first()
        .map(|p| {
            let badge = p.badge_text();
            if reading.is_empty() {
                badge
            } else {
                format!("{reading} {badge}")
            }
        })
        .unwrap_or_default();

    (pitch_accent, freq_rank, jpdb_rank)
}

pub(super) fn selected_freqs_to_public(items: Vec<SelectedFreqItem>) -> Vec<TermFrequency> {
    items
        .into_iter()
        .map(|item| TermFrequency {
            dictionary: item.dictionary,
            value: item.parsed.display,
        })
        .collect()
}

fn compute_harmonic_and_average(numbers: &[f64]) -> (Option<i64>, Option<i64>) {
    if numbers.is_empty() {
        return (None, None);
    }
    let inv_sum: f64 = numbers.iter().map(|&r| 1.0 / r).sum();
    let harmonic = (inv_sum > 0.0 && inv_sum.is_finite())
        .then(|| ((numbers.len() as f64) / inv_sum).floor() as i64);
    let sum: f64 = numbers.iter().sum();
    let average = sum
        .is_finite()
        .then(|| (sum / (numbers.len() as f64)).floor() as i64);
    (harmonic, average)
}

pub(super) fn compute_anki_frequency_summary(
    freq_items: &[SelectedFreqItem],
) -> AnkiFrequencySummary {
    let owned_u16: Vec<(Vec<u16>, Vec<u16>)> = freq_items
        .iter()
        .map(|item| {
            (
                item.dictionary.encode_utf16().collect(),
                item.parsed.display.encode_utf16().collect(),
            )
        })
        .collect();
    let entries: Vec<_> = owned_u16
        .iter()
        .map(|(dict, freq)| memento_anki::note::FrequencyEntry {
            dictionary: dict,
            freq,
        })
        .collect();
    let html = String::from_utf16_lossy(&memento_anki::note::build_frequencies_html(
        &entries, false, false,
    ));

    // Select first positive numeric value per dictionary (in caller priority order),
    // keeping `rank-based` and `occurrence-based` dictionaries strictly separate so they
    // are NEVER mixed into the same average/harmonic mean (Yomitan PR #2373/#2329).
    let mut seen_rank_dicts: HashSet<&str> = HashSet::new();
    let mut seen_occurrence_dicts: HashSet<&str> = HashSet::new();
    let mut rank_numbers: Vec<f64> = Vec::new();
    let mut occurrence_numbers: Vec<f64> = Vec::new();

    for item in freq_items {
        if item.dictionary.is_empty() {
            continue;
        }
        let Some(val) = item.parsed.numeric else {
            continue;
        };
        if !val.is_finite() || val <= 0.0 {
            continue;
        }
        match item.parsed.frequency_mode {
            FrequencyMode::RankBased => {
                if seen_rank_dicts.insert(item.dictionary.as_str()) {
                    rank_numbers.push(val);
                }
            }
            FrequencyMode::OccurrenceBased => {
                if seen_occurrence_dicts.insert(item.dictionary.as_str()) {
                    occurrence_numbers.push(val);
                }
            }
        }
    }

    let (rank_harmonic, rank_average) = compute_harmonic_and_average(&rank_numbers);
    let (occurrence_harmonic, occurrence_average) =
        compute_harmonic_and_average(&occurrence_numbers);

    AnkiFrequencySummary {
        html,
        rank_harmonic,
        rank_average,
        occurrence_harmonic,
        occurrence_average,
    }
}

/// Formats rank vs occurrence fields with Yomitan defaults (`"9999999"` for missing rank
/// so unranked entries sort AFTER ranked entries `#1037`/`#1684`, and `"0"` for missing
/// occurrence) without mixing rank-based and occurrence-based values.
pub(super) fn format_rank_and_occurrence(
    summary: &AnkiFrequencySummary,
) -> (String, String, String, String) {
    let harmonic_rank = summary
        .rank_harmonic
        .map(|v| v.to_string())
        .unwrap_or_else(|| "9999999".to_owned());
    let average_rank = summary
        .rank_average
        .map(|v| v.to_string())
        .unwrap_or_else(|| "9999999".to_owned());
    let harmonic_occurrence = summary
        .occurrence_harmonic
        .map(|v| v.to_string())
        .unwrap_or_else(|| "0".to_owned());
    let average_occurrence = summary
        .occurrence_average
        .map(|v| v.to_string())
        .unwrap_or_else(|| "0".to_owned());
    (
        harmonic_rank,
        average_rank,
        harmonic_occurrence,
        average_occurrence,
    )
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum OwnedPitchSpec {
    Downstep(u8),
    Pattern(String),
}

pub(super) fn build_anki_term_metadata_from_parts(
    freq_items: &[SelectedFreqItem],
    pitch_details: &[TermPitchDetail],
    rule_strings: &[String],
) -> AnkiTermMetadata {
    let freq_summary = compute_anki_frequency_summary(freq_items);

    // Group pitches by `(dictionary, mora)` across ALL rows (not just adjacent) and
    // deduplicate identical pitch positions/patterns within each dictionary (Yomitan #2194).
    let mut grouped_pitches: Vec<(Vec<u16>, Vec<OwnedPitchSpec>, Vec<Vec<u16>>)> = Vec::new();
    let mut group_indices: HashMap<(Vec<u16>, Vec<Vec<u16>>), usize> = HashMap::new();
    let mut seen_specs_per_group: Vec<HashSet<OwnedPitchSpec>> = Vec::new();

    for detail in pitch_details {
        if detail.mora.is_empty() {
            continue;
        }
        let spec = if let Some(ref pat) = detail.pattern {
            if !PitchPosition::is_valid_pattern(pat) {
                continue;
            }
            OwnedPitchSpec::Pattern(pat.to_ascii_uppercase())
        } else {
            let Ok(pos_u8) = u8::try_from(detail.position) else {
                continue;
            };
            OwnedPitchSpec::Downstep(pos_u8)
        };

        let dict_u16: Vec<u16> = detail.dictionary.encode_utf16().collect();
        let mora_u16: Vec<Vec<u16>> = detail
            .mora
            .iter()
            .map(|m| m.encode_utf16().collect())
            .collect();
        let group_key = (dict_u16, mora_u16);
        if let Some(&group_idx) = group_indices.get(&group_key) {
            if seen_specs_per_group[group_idx].insert(spec.clone()) {
                grouped_pitches[group_idx].1.push(spec);
            }
        } else {
            let group_idx = grouped_pitches.len();
            let mut seen = HashSet::new();
            seen.insert(spec.clone());
            seen_specs_per_group.push(seen);
            group_indices.insert(group_key.clone(), group_idx);
            grouped_pitches.push((group_key.0, vec![spec], group_key.1));
        }
    }

    let mora_refs: Vec<Vec<&[u16]>> = grouped_pitches
        .iter()
        .map(|(_, _, mora)| mora.iter().map(Vec::as_slice).collect())
        .collect();
    let spec_refs: Vec<Vec<memento_anki::note::PitchSpec<'_>>> = grouped_pitches
        .iter()
        .map(|(_, specs, _)| {
            specs
                .iter()
                .map(|s| match s {
                    OwnedPitchSpec::Downstep(pos) => memento_anki::note::PitchSpec::Downstep(*pos),
                    OwnedPitchSpec::Pattern(pat) => {
                        memento_anki::note::PitchSpec::Pattern(pat.as_str())
                    }
                })
                .collect()
        })
        .collect();
    let pitches: Vec<_> = grouped_pitches
        .iter()
        .enumerate()
        .map(|(index, (name, _, _))| memento_anki::note::PitchEntryEx {
            dictionary_name: name,
            specs: &spec_refs[index],
            mora: &mora_refs[index],
        })
        .collect();

    let rule_units: Vec<Vec<u16>> = rule_strings
        .iter()
        .map(|rule| rule.encode_utf16().collect())
        .collect();
    let rules: Vec<&[u16]> = rule_units.iter().map(Vec::as_slice).collect();
    let can_be_kifuku = memento_anki::note::is_kifuku_applicable(&rules);

    let (
        frequency_harmonic_rank,
        frequency_average_rank,
        frequency_harmonic_occurrence,
        frequency_average_occurrence,
    ) = format_rank_and_occurrence(&freq_summary);

    AnkiTermMetadata {
        pitch: String::from_utf16_lossy(&memento_anki::note::format_pitch_ex(&pitches)),
        pitch_positions: String::from_utf16_lossy(&memento_anki::note::format_pitch_positions_ex(
            &pitches,
        )),
        pitch_categories: String::from_utf16_lossy(
            &memento_anki::note::format_pitch_categories_ex(&pitches, can_be_kifuku),
        ),
        frequencies: freq_summary.html,
        frequency_harmonic_rank,
        frequency_average_rank,
        frequency_harmonic_occurrence,
        frequency_average_occurrence,
    }
}

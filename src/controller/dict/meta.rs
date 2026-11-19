//! Lock-free pitch accent and frequency selection, deduplication, and Anki summary formatting.

use std::collections::{HashMap, HashSet};

use memento_dict::databasemanager;
use memento_dict::sqlite_db;

use super::db::DictionaryMetadataSnapshot;
use super::{AnkiTermMetadata, TermFrequency, TermPitchDetail};

pub(super) struct SelectedFreqItem {
    pub dictionary: String,
    pub parsed: sqlite_db::ParsedFreqUtf8,
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
            let parsed = sqlite_db::parse_freq_blob_utf8(
                row.blob_type,
                &row.data,
                expression,
                caller_reading,
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
    for (dict_name, parsed) in parsed_list {
        for entry in parsed.entries {
            if entry.position < 0 || (entry.position as usize) > parsed.mora.len() {
                continue;
            }
            out.push(TermPitchDetail {
                dictionary: dict_name.clone(),
                mora: parsed.mora.clone(),
                position: entry.position,
                nasal: entry.nasal,
                devoice: entry.devoice,
                tags: entry.tags,
            });
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
                let parsed = sqlite_db::parse_freq_blob_utf8(
                    row.blob_type,
                    &row.data,
                    expression,
                    caller_reading,
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
                if entry.position < 0 || (entry.position as usize) > parsed.mora.len() {
                    continue;
                }
                pitches.push(TermPitchDetail {
                    dictionary: dict_name.clone(),
                    mora: parsed.mora.clone(),
                    position: entry.position,
                    nasal: entry.nasal,
                    devoice: entry.devoice,
                    tags: entry.tags,
                });
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
            if reading.is_empty() {
                format!("[{}]", p.position)
            } else {
                format!("{reading} [{}]", p.position)
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

pub(super) fn compute_anki_frequency_summary(
    freq_items: &[SelectedFreqItem],
) -> (String, Option<i64>, Option<i64>) {
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

    // Select first positive numeric value per dictionary (in caller priority order)
    // and compute true Yomitan harmonic mean `floor(N / sum(1 / r_i))` and average.
    let mut seen_dicts: HashSet<&str> = HashSet::new();
    let mut numbers: Vec<f64> = Vec::new();
    for item in freq_items {
        if item.dictionary.is_empty() || seen_dicts.contains(item.dictionary.as_str()) {
            continue;
        }
        if let Some(val) = item.parsed.numeric {
            if val.is_finite() && val > 0.0 {
                seen_dicts.insert(item.dictionary.as_str());
                numbers.push(val);
            }
        }
    }

    if numbers.is_empty() {
        return (html, None, None);
    }

    let inv_sum: f64 = numbers.iter().map(|&r| 1.0 / r).sum();
    let harmonic = (inv_sum > 0.0 && inv_sum.is_finite())
        .then(|| ((numbers.len() as f64) / inv_sum).floor() as i64);
    let sum: f64 = numbers.iter().sum();
    let average = sum
        .is_finite()
        .then(|| (sum / (numbers.len() as f64)).floor() as i64);

    (html, harmonic, average)
}

/// Formats rank vs occurrence fields with Yomitan defaults (`"9999999"` for missing rank,
/// `"0"` for missing occurrence) instead of conflating both to empty strings.
pub(super) fn format_rank_and_occurrence(
    harmonic: Option<i64>,
    average: Option<i64>,
) -> (String, String, String, String) {
    let harmonic_rank = harmonic
        .map(|v| v.to_string())
        .unwrap_or_else(|| "9999999".to_owned());
    let average_rank = average
        .map(|v| v.to_string())
        .unwrap_or_else(|| "9999999".to_owned());
    let harmonic_occurrence = harmonic
        .map(|v| v.to_string())
        .unwrap_or_else(|| "0".to_owned());
    let average_occurrence = average
        .map(|v| v.to_string())
        .unwrap_or_else(|| "0".to_owned());
    (
        harmonic_rank,
        average_rank,
        harmonic_occurrence,
        average_occurrence,
    )
}

pub(super) fn build_anki_term_metadata_from_parts(
    freq_items: &[SelectedFreqItem],
    pitch_details: &[TermPitchDetail],
    rule_strings: &[String],
) -> AnkiTermMetadata {
    let (frequencies_html, harmonic, average) = compute_anki_frequency_summary(freq_items);

    let mut grouped_pitches: Vec<(Vec<u16>, Vec<u8>, Vec<Vec<u16>>)> = Vec::new();
    for detail in pitch_details {
        let Ok(pos_u8) = u8::try_from(detail.position) else {
            continue;
        };
        if detail.mora.is_empty() {
            continue;
        }
        let dict_u16: Vec<u16> = detail.dictionary.encode_utf16().collect();
        let mora_u16: Vec<Vec<u16>> = detail
            .mora
            .iter()
            .map(|m| m.encode_utf16().collect())
            .collect();
        if let Some(last) = grouped_pitches.last_mut() {
            if last.0 == dict_u16 && last.2 == mora_u16 {
                last.1.push(pos_u8);
                continue;
            }
        }
        grouped_pitches.push((dict_u16, vec![pos_u8], mora_u16));
    }

    let mora_refs: Vec<Vec<&[u16]>> = grouped_pitches
        .iter()
        .map(|(_, _, mora)| mora.iter().map(Vec::as_slice).collect())
        .collect();
    let pitches: Vec<_> = grouped_pitches
        .iter()
        .enumerate()
        .map(|(index, (name, positions, _))| memento_anki::note::PitchEntry {
            dictionary_name: name,
            positions,
            mora: &mora_refs[index],
        })
        .collect();
    let categories: Vec<_> = grouped_pitches
        .iter()
        .map(|(_, positions, mora)| memento_anki::note::PitchCategoryInput {
            mora_len: mora.len(),
            positions,
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
    ) = format_rank_and_occurrence(harmonic, average);

    AnkiTermMetadata {
        pitch: String::from_utf16_lossy(&memento_anki::note::format_pitch(&pitches)),
        pitch_positions: String::from_utf16_lossy(&memento_anki::note::format_pitch_positions(
            &pitches,
        )),
        pitch_categories: String::from_utf16_lossy(&memento_anki::note::format_pitch_categories(
            &categories,
            can_be_kifuku,
        )),
        frequencies: frequencies_html,
        frequency_harmonic_rank,
        frequency_average_rank,
        frequency_harmonic_occurrence,
        frequency_average_occurrence,
    }
}

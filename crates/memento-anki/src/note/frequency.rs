//! Frequency parsing, aggregation stats, and HTML rendering.

use crate::glossary::escape::{append_escaped, push_ascii};

/// Borrowed frequency dictionary entry for aggregate stats and HTML rendering.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrequencyEntry<'a> {
    pub dictionary: &'a [u16],
    pub freq: &'a [u16],
}

/// Matches `parseFrequencyValue(const QString &frequency)` in `notebuilder.cpp`.
/// Matches `^[+-]?([0-9]+([.][0-9]*)?|[.][0-9]+)` at start using ASCII digits
/// and '.', or exact star ratings ★ through ★★★★★.
pub fn parse_frequency_value(frequency: &[u16]) -> Option<f64> {
    if frequency.is_empty() {
        return None;
    }

    let mut idx = 0;
    if frequency[0] == b'+' as u16 || frequency[0] == b'-' as u16 {
        idx += 1;
    }

    let is_ascii_digit = |c: u16| c >= b'0' as u16 && c <= b'9' as u16;

    let mut matched_len = 0;
    if idx < frequency.len() && is_ascii_digit(frequency[idx]) {
        while idx < frequency.len() && is_ascii_digit(frequency[idx]) {
            idx += 1;
        }
        if idx < frequency.len() && frequency[idx] == b'.' as u16 {
            idx += 1;
            while idx < frequency.len() && is_ascii_digit(frequency[idx]) {
                idx += 1;
            }
        }
        matched_len = idx;
    } else if idx < frequency.len()
        && frequency[idx] == b'.' as u16
        && idx + 1 < frequency.len()
        && is_ascii_digit(frequency[idx + 1])
    {
        idx += 2;
        while idx < frequency.len() && is_ascii_digit(frequency[idx]) {
            idx += 1;
        }
        matched_len = idx;
    }

    if matched_len > 0 {
        let s: String = frequency[..matched_len]
            .iter()
            .map(|&c| c as u8 as char)
            .collect();
        if let Ok(val) = s.parse::<f64>() {
            if val.is_finite() {
                return Some(val);
            }
        }
        return None;
    }

    const STAR: u16 = 0x2605;
    match frequency {
        [STAR] => Some(20.0),
        [STAR, STAR] => Some(40.0),
        [STAR, STAR, STAR] => Some(60.0),
        [STAR, STAR, STAR, STAR] => Some(80.0),
        [STAR, STAR, STAR, STAR, STAR] => Some(100.0),
        _ => None,
    }
}

/// Matches `getFrequencyNumbers` in `notebuilder.cpp`.
/// Only selects the first positive frequency (> 0.0) from each dictionary.
/// An empty dictionary first entry is skipped because `previousDictionary` starts empty.
pub fn frequency_numbers(entries: &[FrequencyEntry<'_>]) -> Vec<f64> {
    let mut previous_dictionary: &[u16] = &[];
    let mut numbers = Vec::new();
    for entry in entries {
        if entry.dictionary == previous_dictionary || entry.freq.is_empty() {
            continue;
        }
        if let Some(val) = parse_frequency_value(entry.freq) {
            if val > 0.0 {
                previous_dictionary = entry.dictionary;
                numbers.push(val);
            }
        }
    }
    numbers
}

/// Matches `getFrequencyHarmonic` in `notebuilder.cpp`.
pub fn frequency_harmonic(numbers: &[f64]) -> Option<i64> {
    if numbers.is_empty() {
        return None;
    }
    let mut sum = 0.0;
    for &val in numbers {
        if val != 0.0 {
            sum += 1.0 / val;
        }
    }
    if sum == 0.0 {
        return None;
    }
    let harmonic = (numbers.len() as f64 / sum).floor();
    Some(harmonic as i64)
}

/// Matches `getFrequencyAverage` in `notebuilder.cpp`.
pub fn frequency_average(numbers: &[f64]) -> Option<i64> {
    if numbers.is_empty() {
        return None;
    }
    let sum: f64 = numbers.iter().sum();
    let average = (sum / numbers.len() as f64).floor();
    Some(average as i64)
}

/// Combines frequency numbers, harmonic mean, and average calculation.
pub fn frequency_stats(entries: &[FrequencyEntry<'_>]) -> (Vec<f64>, Option<i64>, Option<i64>) {
    let numbers = frequency_numbers(entries);
    let harmonic = frequency_harmonic(&numbers);
    let average = frequency_average(&numbers);
    (numbers, harmonic, average)
}

/// Matches `buildFrequencies` in `notebuilder.cpp`, HTML-escaping dictionary
/// names and frequency strings.
pub fn build_frequencies_html(
    entries: &[FrequencyEntry<'_>],
    value_only: bool,
    min_value_only: bool,
) -> Vec<u16> {
    if entries.is_empty() {
        return Vec::new();
    }

    const COLON: u16 = b':' as u16;
    const MIN_VALUE_DEFAULT: f64 = 9999999.0;

    if min_value_only {
        let mut min_freq = MIN_VALUE_DEFAULT;
        let mut best: Option<(&[u16], &str, &[u16])> = None;
        for entry in entries {
            if let Some(val) = parse_frequency_value(entry.freq) {
                if val < min_freq {
                    min_freq = val;
                    let sep = if entry.dictionary.last() == Some(&COLON) {
                        " "
                    } else {
                        ": "
                    };
                    best = Some((entry.dictionary, sep, entry.freq));
                }
            }
        }

        let mut out = Vec::new();
        if !value_only {
            push_ascii(&mut out, "<ul><li>");
            if let Some((dict, sep, freq)) = best {
                append_escaped(dict, &mut out, false);
                push_ascii(&mut out, sep);
                append_escaped(freq, &mut out, false);
            }
            push_ascii(&mut out, "</li></ul>");
        } else if let Some((_, _, freq)) = best {
            append_escaped(freq, &mut out, false);
        }
        return out;
    }

    let mut freq_str = Vec::new();
    if !value_only {
        push_ascii(&mut freq_str, "<ul>");
    }

    for entry in entries {
        if !value_only {
            let sep = if entry.dictionary.last() == Some(&COLON) {
                " "
            } else {
                ": "
            };
            push_ascii(&mut freq_str, "<li>");
            append_escaped(entry.dictionary, &mut freq_str, false);
            push_ascii(&mut freq_str, sep);
        }
        append_escaped(entry.freq, &mut freq_str, false);
        if value_only {
            push_ascii(&mut freq_str, "<br>");
        } else {
            push_ascii(&mut freq_str, "</li>");
        }
    }

    if !value_only {
        push_ascii(&mut freq_str, "</ul>");
    }

    freq_str
}

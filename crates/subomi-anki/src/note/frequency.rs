//! Frequency parsing, aggregation stats, and HTML rendering.

use crate::glossary::escape::append_escaped_str;

/// Borrowed frequency dictionary entry for aggregate stats and HTML rendering.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrequencyEntry<'a> {
    pub dictionary: &'a str,
    pub freq: &'a str,
}

/// Parses a leading signed decimal number (`^[+-]?([0-9]+([.][0-9]*)?|[.][0-9]+)`)
/// or an exact 1..=5 star rating (`★` through `★★★★★`).
pub fn parse_frequency_value(frequency: &str) -> Option<f64> {
    let bytes = frequency.as_bytes();
    if bytes.is_empty() {
        return None;
    }

    let mut idx = 0;
    if bytes[0] == b'+' || bytes[0] == b'-' {
        idx += 1;
    }

    let mut matched_len = 0;
    if idx < bytes.len() && bytes[idx].is_ascii_digit() {
        while idx < bytes.len() && bytes[idx].is_ascii_digit() {
            idx += 1;
        }
        if idx < bytes.len() && bytes[idx] == b'.' {
            idx += 1;
            while idx < bytes.len() && bytes[idx].is_ascii_digit() {
                idx += 1;
            }
        }
        matched_len = idx;
    } else if idx + 1 < bytes.len() && bytes[idx] == b'.' && bytes[idx + 1].is_ascii_digit() {
        idx += 2;
        while idx < bytes.len() && bytes[idx].is_ascii_digit() {
            idx += 1;
        }
        matched_len = idx;
    }

    if matched_len > 0 {
        let parsed = frequency[..matched_len].parse::<f64>().ok()?;
        return parsed.is_finite().then_some(parsed);
    }

    match frequency {
        "★" => Some(20.0),
        "★★" => Some(40.0),
        "★★★" => Some(60.0),
        "★★★★" => Some(80.0),
        "★★★★★" => Some(100.0),
        _ => None,
    }
}

/// Selects the first positive numeric frequency (`> 0.0`) from each dictionary
/// group (skipping empty dictionary names).
pub fn frequency_numbers(entries: &[FrequencyEntry<'_>]) -> Vec<f64> {
    let mut previous_dictionary = "";
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

/// Computes the floored harmonic mean of positive frequencies.
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
    Some((numbers.len() as f64 / sum).floor() as i64)
}

/// Computes the floored arithmetic mean of frequencies.
pub fn frequency_average(numbers: &[f64]) -> Option<i64> {
    if numbers.is_empty() {
        return None;
    }
    let sum: f64 = numbers.iter().sum();
    Some((sum / numbers.len() as f64).floor() as i64)
}

/// Combines frequency numbers, harmonic mean, and average calculation.
pub fn frequency_stats(entries: &[FrequencyEntry<'_>]) -> (Vec<f64>, Option<i64>, Option<i64>) {
    let numbers = frequency_numbers(entries);
    let harmonic = frequency_harmonic(&numbers);
    let average = frequency_average(&numbers);
    (numbers, harmonic, average)
}

/// Renders dictionary frequency entries as HTML, escaping dictionary names and values.
pub fn build_frequencies_html(
    entries: &[FrequencyEntry<'_>],
    value_only: bool,
    min_value_only: bool,
) -> String {
    if entries.is_empty() {
        return String::new();
    }

    const MIN_VALUE_DEFAULT: f64 = 9999999.0;

    if min_value_only {
        let mut min_freq = MIN_VALUE_DEFAULT;
        let mut best: Option<(&str, &str, &str)> = None;
        for entry in entries {
            if let Some(val) = parse_frequency_value(entry.freq) {
                if val < min_freq {
                    min_freq = val;
                    let sep = if entry.dictionary.ends_with(':') {
                        " "
                    } else {
                        ": "
                    };
                    best = Some((entry.dictionary, sep, entry.freq));
                }
            }
        }

        let mut out = String::new();
        if !value_only {
            out.push_str("<ul><li>");
            if let Some((dict, sep, freq)) = best {
                append_escaped_str(dict, &mut out);
                out.push_str(sep);
                append_escaped_str(freq, &mut out);
            }
            out.push_str("</li></ul>");
        } else if let Some((_, _, freq)) = best {
            append_escaped_str(freq, &mut out);
        }
        return out;
    }

    let mut freq_str = String::new();
    if !value_only {
        freq_str.push_str("<ul>");
    }

    for entry in entries {
        if !value_only {
            let sep = if entry.dictionary.ends_with(':') {
                " "
            } else {
                ": "
            };
            freq_str.push_str("<li>");
            append_escaped_str(entry.dictionary, &mut freq_str);
            freq_str.push_str(sep);
        }
        append_escaped_str(entry.freq, &mut freq_str);
        if value_only {
            freq_str.push_str("<br>");
        } else {
            freq_str.push_str("</li>");
        }
    }

    if !value_only {
        freq_str.push_str("</ul>");
    }

    freq_str
}

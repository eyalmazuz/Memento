//! Frequency blob decoding and C-locale `%g` float formatting.

use super::FrequencyMode;

/// Parsed UTF-8 frequency value with homograph reading match info, raw numeric value,
/// and dictionary `frequency_mode` (Yomitan PR #2373/#2329).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ParsedFreqUtf8 {
    pub matched: bool,
    pub exact_reading_match: bool,
    pub has_reading_key: bool,
    pub display: String,
    pub numeric: Option<f64>,
    pub frequency_mode: FrequencyMode,
}

fn parse_numeric_from_freq_text(text: &str) -> Option<f64> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    match trimmed {
        "★" => return Some(20.0),
        "★★" => return Some(40.0),
        "★★★" => return Some(60.0),
        "★★★★" => return Some(80.0),
        "★★★★★" => return Some(100.0),
        _ => {}
    }
    let bytes = trimmed.as_bytes();
    let mut i = 0usize;
    if i < bytes.len() && (bytes[i] == b'+' || bytes[i] == b'-') {
        i += 1;
    }
    let digits_start = i;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
    }
    if i < bytes.len() && bytes[i] == b'.' {
        i += 1;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
    }
    if i == digits_start || (i == digits_start + 1 && bytes[digits_start] == b'.') {
        return None;
    }
    if i < bytes.len() && (bytes[i] == b'e' || bytes[i] == b'E') {
        let exp_pos = i;
        i += 1;
        if i < bytes.len() && (bytes[i] == b'+' || bytes[i] == b'-') {
            i += 1;
        }
        let exp_digits = i;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
        if i == exp_digits {
            i = exp_pos;
        }
    }
    trimmed[..i]
        .parse::<f64>()
        .ok()
        .filter(|v| v.is_finite())
}

/// Parses a `term_meta_bank` / `kanji_meta_bank` `freq` blob in UTF-8 with strict homograph
/// reading matching and exact numeric extraction (defaulting `frequency_mode` to `RankBased`).
#[must_use]
pub fn parse_freq_blob_utf8(
    blob_type: i32,
    blob: &[u8],
    expression: &str,
    caller_reading: &str,
) -> ParsedFreqUtf8 {
    parse_freq_blob_with_mode_utf8(
        blob_type,
        blob,
        expression,
        caller_reading,
        FrequencyMode::RankBased,
    )
}

/// Parses a `term_meta_bank` / `kanji_meta_bank` `freq` blob in UTF-8 and tags the result with
/// the owning dictionary's [`FrequencyMode`] (`rank-based` vs `occurrence-based`).
#[must_use]
pub fn parse_freq_blob_with_mode_utf8(
    blob_type: i32,
    blob: &[u8],
    expression: &str,
    caller_reading: &str,
    frequency_mode: FrequencyMode,
) -> ParsedFreqUtf8 {
    match blob_type {
        1 => {
            if blob.len() >= 8 {
                let mut arr = [0u8; 8];
                arr.copy_from_slice(&blob[..8]);
                let val = i64::from_le_bytes(arr);
                if val >= 0 {
                    return ParsedFreqUtf8 {
                        matched: true,
                        exact_reading_match: false,
                        has_reading_key: false,
                        display: val.to_string(),
                        numeric: (val > 0).then_some(val as f64),
                        frequency_mode,
                    };
                }
            }
            ParsedFreqUtf8 {
                frequency_mode,
                ..ParsedFreqUtf8::default()
            }
        }
        2 => {
            let null_pos = blob.iter().position(|&b| b == 0).unwrap_or(blob.len());
            let Ok(s) = std::str::from_utf8(&blob[..null_pos]) else {
                return ParsedFreqUtf8 {
                    frequency_mode,
                    ..ParsedFreqUtf8::default()
                };
            };
            ParsedFreqUtf8 {
                matched: true,
                exact_reading_match: false,
                has_reading_key: false,
                display: s.to_owned(),
                numeric: parse_numeric_from_freq_text(s).filter(|&v| v > 0.0),
                frequency_mode,
            }
        }
        3 => {
            if blob.len() >= 8 {
                let mut arr = [0u8; 8];
                arr.copy_from_slice(&blob[..8]);
                let val = f64::from_le_bytes(arr);
                if val.is_finite() && val >= 0.0 {
                    return ParsedFreqUtf8 {
                        matched: true,
                        exact_reading_match: false,
                        has_reading_key: false,
                        display: format_g(val, 6),
                        numeric: (val > 0.0).then_some(val),
                        frequency_mode,
                    };
                }
            }
            ParsedFreqUtf8 {
                frequency_mode,
                ..ParsedFreqUtf8::default()
            }
        }
        4 => {
            let null_pos = blob.iter().position(|&b| b == 0).unwrap_or(blob.len());
            let slice = &blob[..null_pos];
            let Ok(serde_json::Value::Object(obj)) = serde_json::from_slice(slice) else {
                return ParsedFreqUtf8 {
                    frequency_mode,
                    ..ParsedFreqUtf8::default()
                };
            };

            let meta_reading = obj.get("reading").and_then(|v| v.as_str());
            let has_reading_key = meta_reading.is_some();
            let (matched, exact_reading_match) =
                crate::databasemanager::metadata_reading_matches(
                    expression,
                    caller_reading,
                    meta_reading,
                );
            if !matched {
                return ParsedFreqUtf8 {
                    has_reading_key,
                    frequency_mode,
                    ..ParsedFreqUtf8::default()
                };
            }

            let mut display = String::new();
            let mut numeric: Option<f64> = None;

            if let Some(freq_val) = obj.get("frequency") {
                if let Some(n) = freq_val.as_f64() {
                    display = format_g(n, 6);
                    if n.is_finite() && n > 0.0 {
                        numeric = Some(n);
                    }
                } else if let Some(s) = freq_val.as_str() {
                    display = s.to_owned();
                    numeric = parse_numeric_from_freq_text(s).filter(|&v| v > 0.0);
                } else if let Some(nested) = freq_val.as_object() {
                    let nested_disp = nested.get("displayValue").and_then(|v| v.as_str());
                    let nested_num = nested.get("value").and_then(|v| v.as_f64());
                    if let Some(d) = nested_disp {
                        display = d.to_owned();
                    } else if let Some(n) = nested_num {
                        display = format_g(n, 6);
                    } else {
                        return ParsedFreqUtf8 {
                            has_reading_key,
                            frequency_mode,
                            ..ParsedFreqUtf8::default()
                        };
                    }
                    numeric = nested_num
                        .filter(|v| v.is_finite() && *v > 0.0)
                        .or_else(|| parse_numeric_from_freq_text(&display).filter(|&v| v > 0.0));
                }
            } else {
                let top_disp = obj.get("displayValue").and_then(|v| v.as_str());
                let top_num = obj.get("value").and_then(|v| v.as_f64());
                if let Some(d) = top_disp {
                    display = d.to_owned();
                } else if let Some(n) = top_num {
                    display = format_g(n, 6);
                }
                numeric = top_num
                    .filter(|v| v.is_finite() && *v > 0.0)
                    .or_else(|| parse_numeric_from_freq_text(&display).filter(|&v| v > 0.0));
            }

            ParsedFreqUtf8 {
                matched: true,
                exact_reading_match,
                has_reading_key,
                display,
                numeric,
                frequency_mode,
            }
        }
        _ => ParsedFreqUtf8 {
            frequency_mode,
            ..ParsedFreqUtf8::default()
        },
    }
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

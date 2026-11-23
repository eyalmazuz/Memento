//! External subtitle file decoding (UTF-8, BOM-less UTF-16 LE/BE, lossy UTF-8)
//! and lenient ASS/SSA fallback parsing.

use std::path::Path;

/// Decode subtitle file bytes supporting UTF-8 (with or without BOM),
/// UTF-16 LE/BE (with or without BOM, including CJK leading characters), and
/// lossy UTF-8 fallback.
pub(crate) fn decode_subtitle_bytes(bytes: &[u8]) -> String {
    if let Some(rest) = bytes.strip_prefix(b"\xef\xbb\xbf") {
        return String::from_utf8_lossy(rest).into_owned();
    }
    if let Some(rest) = bytes.strip_prefix(b"\xff\xfe") {
        let u16s: Vec<u16> = rest
            .chunks_exact(2)
            .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
            .collect();
        return String::from_utf16_lossy(&u16s);
    }
    if let Some(rest) = bytes.strip_prefix(b"\xfe\xff") {
        let u16s: Vec<u16> = rest
            .chunks_exact(2)
            .map(|chunk| u16::from_be_bytes([chunk[0], chunk[1]]))
            .collect();
        return String::from_utf16_lossy(&u16s);
    }
    if !bytes.contains(&0) {
        if let Ok(utf8) = std::str::from_utf8(bytes) {
            return utf8.to_owned();
        }
    }
    if bytes.len() >= 2 && bytes.len() % 2 == 0 {
        let sample_len = bytes.len().min(4096) & !1;
        let sample = &bytes[..sample_len];
        let total_pairs = sample.len() / 2;
        let mut le_newlines = 0usize;
        let mut be_newlines = 0usize;
        let mut le_ascii = 0usize;
        let mut be_ascii = 0usize;
        for chunk in sample.chunks_exact(2) {
            if chunk[1] == 0 {
                if matches!(chunk[0], b'\n' | b'\r') {
                    le_newlines += 1;
                }
                if matches!(chunk[0], b'\t' | b'\n' | b'\r' | 0x20..=0x7e) {
                    le_ascii += 1;
                }
            }
            if chunk[0] == 0 {
                if matches!(chunk[1], b'\n' | b'\r') {
                    be_newlines += 1;
                }
                if matches!(chunk[1], b'\t' | b'\n' | b'\r' | 0x20..=0x7e) {
                    be_ascii += 1;
                }
            }
        }
        let looks_like_utf16 = le_newlines > 0
            || be_newlines > 0
            || ((le_ascii + be_ascii) * 4 >= total_pairs && (le_ascii >= 2 || be_ascii >= 2));
        if looks_like_utf16 {
            let u16s: Vec<u16> = if (le_newlines, le_ascii) >= (be_newlines, be_ascii) {
                bytes
                    .chunks_exact(2)
                    .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
                    .collect()
            } else {
                bytes
                    .chunks_exact(2)
                    .map(|chunk| u16::from_be_bytes([chunk[0], chunk[1]]))
                    .collect()
            };
            return String::from_utf16_lossy(&u16s);
        }
    }
    String::from_utf8_lossy(bytes).into_owned()
}

/// Read a subtitle file supporting UTF-8 (with or without BOM), UTF-16 LE/BE, and lossy UTF-8.
pub(crate) fn read_subtitle_file(path: &Path) -> Result<String, String> {
    let bytes = std::fs::read(path)
        .map_err(|error| format!("Could not read {}: {error}", path.display()))?;
    Ok(decode_subtitle_bytes(&bytes))
}

fn parse_lenient_timecode(s: &str) -> Option<f64> {
    let s = s.trim();
    let (h_m_s, frac) = match s.split_once(['.', ',']) {
        Some((front, back)) => {
            let digits: String = back.chars().take_while(|c| c.is_ascii_digit()).collect();
            let val = if digits.is_empty() {
                0.0
            } else {
                digits.parse::<f64>().ok()? / 10f64.powi(digits.len() as i32)
            };
            (front, val)
        }
        None => (s, 0.0),
    };
    let parts: Vec<&str> = h_m_s.split(':').collect();
    match parts.len() {
        3 => {
            let h = parts[0].trim().parse::<f64>().ok()?;
            let m = parts[1].trim().parse::<f64>().ok()?;
            let sec = parts[2].trim().parse::<f64>().ok()?;
            Some(h * 3600.0 + m * 60.0 + sec + frac)
        }
        2 => {
            let m = parts[0].trim().parse::<f64>().ok()?;
            let sec = parts[1].trim().parse::<f64>().ok()?;
            Some(m * 60.0 + sec + frac)
        }
        1 => {
            let sec = parts[0].trim().parse::<f64>().ok()?;
            Some(sec + frac)
        }
        _ => None,
    }
}

fn strip_ass_tags(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_tag = false;
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '{' {
            in_tag = true;
        } else if ch == '}' {
            in_tag = false;
        } else if !in_tag {
            if ch == '\\' && matches!(chars.peek(), Some('N' | 'n')) {
                chars.next();
                out.push('\n');
            } else {
                out.push(ch);
            }
        }
    }
    out
}

pub(crate) fn parse_lenient_subtitles(text: &str) -> Vec<subomi_subtitle::SubtitleEntry> {
    const DIALOGUE_PREFIX: &[u8] = b"dialogue:";
    let mut entries = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        let bytes = trimmed.as_bytes();
        if bytes.len() >= DIALOGUE_PREFIX.len()
            && bytes[..DIALOGUE_PREFIX.len()].eq_ignore_ascii_case(DIALOGUE_PREFIX)
        {
            // SAFETY: `DIALOGUE_PREFIX` is pure ASCII, so `DIALOGUE_PREFIX.len()`
            // is guaranteed to be a valid UTF-8 boundary whenever it matched.
            let rest = trimmed[DIALOGUE_PREFIX.len()..].trim_start();
            let columns: Vec<&str> = rest.split(',').collect();
            if columns.len() >= 3 {
                // Typical ASS Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text
                // Start is index 1, End is index 2, Text is index 9..
                if let (Some(start), Some(end)) = (
                    parse_lenient_timecode(columns[1]),
                    parse_lenient_timecode(columns[2]),
                ) {
                    if start >= 0.0 && end > start {
                        let text_joined = if columns.len() > 9 {
                            columns[9..].join(",")
                        } else if columns.len() > 3 {
                            columns[3..].join(",")
                        } else {
                            String::new()
                        };
                        let clean = strip_ass_tags(&text_joined);
                        let clean_trimmed = clean.trim();
                        if !clean_trimmed.is_empty() {
                            entries.push(subomi_subtitle::SubtitleEntry {
                                text: clean_trimmed.to_owned(),
                                start,
                                end,
                            });
                        }
                    }
                }
            }
        }
    }
    entries.sort_by(|a, b| {
        a.start
            .partial_cmp(&b.start)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    entries
}

/// Read an external subtitle file reported by mpv for one specific SID.
/// Embedded tracks have no path; those are populated from mpv's live cue data.
pub(crate) fn parse_external_subtitles(
    path: &Path,
) -> Result<Vec<subomi_subtitle::SubtitleEntry>, String> {
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let text = read_subtitle_file(path)?;

    let primary_format = match extension.as_str() {
        "ass" | "ssa" => Some(subomi_subtitle::Format::Ass),
        "srt" => Some(subomi_subtitle::Format::Srt),
        "vtt" => Some(subomi_subtitle::Format::Vtt),
        _ => None,
    };
    if let Some(format) = primary_format {
        if let Ok(cues) = subomi_subtitle::parse_subtitles(format, &text) {
            if !cues.is_empty() {
                return Ok(cues);
            }
        }
    }
    for format in [
        subomi_subtitle::Format::Ass,
        subomi_subtitle::Format::Srt,
        subomi_subtitle::Format::Vtt,
    ] {
        if Some(format) == primary_format {
            continue;
        }
        if let Ok(cues) = subomi_subtitle::parse_subtitles(format, &text) {
            if !cues.is_empty() {
                return Ok(cues);
            }
        }
    }
    let fallback_cues = parse_lenient_subtitles(&text);
    if !fallback_cues.is_empty() {
        return Ok(fallback_cues);
    }
    Err(format!("Could not parse subtitle file: {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bomless_utf16_and_utf8_cjk_subtitle_files_decode_accurately() {
        let sample = "一話「旅立ち」\n1\n00:00:01,000 --> 00:00:03,000\n世界を旅する\n";
        let utf16le: Vec<u8> = sample.encode_utf16().flat_map(u16::to_le_bytes).collect();
        let utf16be: Vec<u8> = sample.encode_utf16().flat_map(u16::to_be_bytes).collect();
        assert_eq!(decode_subtitle_bytes(&utf16le), sample);
        assert_eq!(decode_subtitle_bytes(&utf16be), sample);
        assert_eq!(decode_subtitle_bytes(sample.as_bytes()), sample);

        let dir = tempfile::TempDir::new().unwrap();
        let srt_le = dir.path().join("cjk_le.srt");
        let srt_be = dir.path().join("cjk_be.srt");
        let srt_content = "1\n00:00:01,000 --> 00:00:03,000\n一話「旅立ち」\n";
        std::fs::write(
            &srt_le,
            srt_content
                .encode_utf16()
                .flat_map(u16::to_le_bytes)
                .collect::<Vec<u8>>(),
        )
        .unwrap();
        std::fs::write(
            &srt_be,
            srt_content
                .encode_utf16()
                .flat_map(u16::to_be_bytes)
                .collect::<Vec<u8>>(),
        )
        .unwrap();
        let cues_le = parse_external_subtitles(&srt_le).unwrap();
        let cues_be = parse_external_subtitles(&srt_be).unwrap();
        assert_eq!(cues_le.len(), 1);
        assert_eq!(cues_le[0].text, "一話「旅立ち」");
        assert_eq!(cues_be.len(), 1);
        assert_eq!(cues_be[0].text, "一話「旅立ち」");
    }

    #[test]
    fn parse_lenient_subtitles_strips_case_insensitive_dialogue_prefix() {
        let ass = "\
dialogue: 0,0:00:01.00,0:00:03.00,Default,,0,0,0,,{\\i1}lowercase line\\Nsecond\n\
DIALOGUE:0,0:00:04.00,0:00:06.00,Default,,0,0,0,,UPPERCASE, with comma\n\
Dialogue: 0,0:00:07.00,0:00:09.00,Default,,0,0,0,,Mixed case\n";
        let cues = parse_lenient_subtitles(ass);
        assert_eq!(cues.len(), 3);
        assert_eq!(cues[0].text, "lowercase line\nsecond");
        assert_eq!(cues[1].text, "UPPERCASE, with comma");
        assert_eq!(cues[2].text, "Mixed case");
    }
}

//! ASS, SRT and VTT parsing over text decoded by Qt's QTextStream. The file
//! opening, URL handling, BOM detection and text-mode newline conversion stay
//! at the Qt boundary; parser decisions and subtitle values live here.

use std::cmp::Ordering;

use crate::subtitleentry::SubtitleEntry;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Format {
    Ass,
    Srt,
    Vtt,
}

fn strip_ascii_prefix_ignore_case<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    s.get(..prefix.len())
        .filter(|head| head.eq_ignore_ascii_case(prefix))
        .map(|_| &s[prefix.len()..])
}

/// Update ASS vector drawing mode (`\p<scale>`) from override tags inside `{...}`.
/// Drawing mode is active when `<scale> > 0` (e.g. `\p1`, `\p2`, `\p4`) and disabled
/// by `\p0`, bare `\p`, or a non-positive scale. Ignores unrelated tags such as `\pos` or `\pbo`.
fn update_ass_drawing_mode(block: &str, in_drawing: &mut bool) {
    let bytes = block.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'\\' {
            i += 1;
            continue;
        }
        i += 1; // skip '\\'
        if i >= bytes.len() || bytes[i] != b'p' {
            continue;
        }
        let after_p = i + 1;
        if after_p >= bytes.len() || bytes[after_p] == b'\\' {
            // Bare `\p` turns drawing mode off (scale 0).
            *in_drawing = false;
            i = after_p;
            continue;
        }
        let mut j = after_p;
        let mut negative = false;
        if bytes[j] == b'-' {
            negative = true;
            j += 1;
        } else if bytes[j] == b'+' {
            j += 1;
        }
        let digit_start = j;
        let mut has_nonzero = false;
        while j < bytes.len() && bytes[j].is_ascii_digit() {
            if bytes[j] != b'0' {
                has_nonzero = true;
            }
            j += 1;
        }
        if j > digit_start {
            *in_drawing = !negative && has_nonzero;
            i = j;
        } else {
            i = after_p;
        }
    }
}

/// Strip `{...}` ASS override blocks, omit vector drawing payloads while `\p<scale>` (`scale > 0`)
/// is active, and replace `\n`/`\N` with `'\n'`.
fn strip_ass_formatting(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut i = 0;
    let mut in_drawing = false;
    while i < bytes.len() {
        if bytes[i] == b'{' {
            if let Some(rel_end) = input[i + 1..].find('}') {
                let block = &input[i + 1..i + 1 + rel_end];
                update_ass_drawing_mode(block, &mut in_drawing);
                i += rel_end + 2;
                continue;
            }
        }
        if in_drawing {
            if let Some(next_brace) = input[i..].find('{') {
                if next_brace == 0 {
                    // Unclosed '{' while in drawing mode: omit the remainder.
                    break;
                }
                i += next_brace;
            } else {
                break;
            }
            continue;
        }
        if bytes[i] == b'\\' && i + 1 < bytes.len() && matches!(bytes[i + 1], b'n' | b'N') {
            out.push('\n');
            i += 2;
            continue;
        }
        let start = i;
        i += 1;
        while i < bytes.len() && bytes[i] != b'{' && bytes[i] != b'\\' {
            i += 1;
        }
        out.push_str(&input[start..i]);
    }
    out
}

/// Match an SRT style tag at the start of `bytes`:
/// `</?[biu]>`, `{/?[biu]}`, `</?font.*?>` (single-line), or `{\an?[0-9]}` (ASCII digit).
fn match_srt_tag(bytes: &[u8]) -> Option<usize> {
    match *bytes.first()? {
        b'<' => {
            let mut j = 1;
            if bytes.get(j) == Some(&b'/') {
                j += 1;
            }
            if let (Some(&tag), Some(&b'>')) = (bytes.get(j), bytes.get(j + 1)) {
                if matches!(tag, b'b' | b'i' | b'u' | b'B' | b'I' | b'U') {
                    return Some(j + 2);
                }
            }
            if bytes
                .get(j..j + 4)
                .is_some_and(|w| w.eq_ignore_ascii_case(b"font"))
            {
                let mut k = j + 4;
                while k < bytes.len() && bytes[k] != b'\n' {
                    if bytes[k] == b'>' {
                        return Some(k + 1);
                    }
                    k += 1;
                }
            }
            None
        }
        b'{' => {
            let mut j = 1;
            if bytes.get(j) == Some(&b'/') {
                j += 1;
            }
            if let (Some(&tag), Some(&b'}')) = (bytes.get(j), bytes.get(j + 1)) {
                if matches!(tag, b'b' | b'i' | b'u' | b'B' | b'I' | b'U') {
                    return Some(j + 2);
                }
            }
            if bytes.get(1) == Some(&b'\\') && bytes.get(2) == Some(&b'a') {
                let mut k = 3;
                if bytes.get(k) == Some(&b'n') {
                    k += 1;
                }
                if bytes.get(k).is_some_and(u8::is_ascii_digit) && bytes.get(k + 1) == Some(&b'}') {
                    return Some(k + 2);
                }
            }
            None
        }
        _ => None,
    }
}

fn strip_srt_style(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if let Some(skip) = match_srt_tag(&bytes[i..]) {
            i += skip;
            continue;
        }
        let start = i;
        i += 1;
        while i < bytes.len() && bytes[i] != b'<' && bytes[i] != b'{' {
            i += 1;
        }
        out.push_str(&input[start..i]);
    }
    out
}

/// Strip `<...>` WebVTT cue tags.
fn strip_vtt_tags(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'<' {
            if let Some(rel_end) = input[i + 1..].find('>') {
                i += rel_end + 2;
                continue;
            }
        }
        let start = i;
        i += 1;
        while i < bytes.len() && bytes[i] != b'<' {
            i += 1;
        }
        out.push_str(&input[start..i]);
    }
    out
}

fn is_pcre2_ascii_space(ch: char) -> bool {
    matches!(ch, '\t' | '\n' | '\x0b' | '\x0c' | '\r' | ' ')
}

fn is_blank_line(line: &str) -> bool {
    line.chars().all(is_pcre2_ascii_space)
}

fn split_pcre2_whitespace(text: &str) -> Vec<&str> {
    text.split(is_pcre2_ascii_space)
        .filter(|part| !part.is_empty())
        .collect()
}

// QString::toInt converts through Latin-1, trims ASCII whitespace, and treats
// an embedded NUL as the end of its numeric input.
fn qt_to_i32(value: &str) -> Option<i32> {
    let value = value.split_once('\0').map_or(value, |(prefix, _)| prefix);
    value.trim_matches(is_pcre2_ascii_space).parse().ok()
}

fn timecode_to_double(timecode: &str) -> Option<f64> {
    let trimmed = timecode.trim();
    let (hms_part, frac_part) = match trimmed.rfind(['.', ',']) {
        Some(sep_idx) => {
            let before = &trimmed[..sep_idx];
            let after = &trimmed[sep_idx + 1..];
            if before.contains(['.', ',']) || after.contains(':') {
                return None;
            }
            (before, Some(after))
        }
        None => (trimmed, None),
    };

    let fraction = match frac_part {
        None => 0.0,
        Some(frac) => {
            let frac_clean = frac.split_once('\0').map_or(frac, |(prefix, _)| prefix);
            let frac_trimmed = frac_clean.trim_matches(is_pcre2_ascii_space);
            if frac_trimmed.is_empty() || !frac_trimmed.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            match frac_trimmed.len() {
                1 => {
                    let value = frac_trimmed.parse::<i32>().ok()?;
                    value as f64 * 0.1
                }
                2 => {
                    let value = frac_trimmed.parse::<i32>().ok()?;
                    value as f64 * 0.01
                }
                3 => {
                    let value = frac_trimmed.parse::<i32>().ok()?;
                    value as f64 * 0.001
                }
                _ => {
                    let mut frac_val = 0.0f64;
                    let mut scale = 0.1f64;
                    for b in frac_trimmed.bytes().take(9) {
                        frac_val += (b - b'0') as f64 * scale;
                        scale *= 0.1;
                    }
                    frac_val
                }
            }
        }
    };

    let mut parts = hms_part.split(':');
    let p0 = parts.next()?;
    let p1 = parts.next()?;
    let p2 = parts.next();
    if parts.next().is_some() {
        return None;
    }
    let (hours, minutes, seconds) = match p2 {
        Some(s) => (Some(qt_to_i32(p0)?), qt_to_i32(p1)?, qt_to_i32(s)?),
        None => (None, qt_to_i32(p0)?, qt_to_i32(p1)?),
    };
    if !(0..=59).contains(&seconds) || !(0..=59).contains(&minutes) {
        return None;
    }
    // Match the C++ accumulation order, including floating-point rounding.
    let mut value = 0.0;
    value += fraction;
    value += seconds as f64;
    value += (minutes * 60) as f64;
    if let Some(hours) = hours {
        if hours < 0 {
            return None;
        }
        value += hours as f64 * 3600.0;
    }
    Some(value)
}

fn text_lines(content: &str) -> Vec<&str> {
    // QTextStream::readLine consumes a final newline but not an extra empty
    // record; it also strips a CR at the end of each line.
    content.lines().map(|line| line.strip_suffix('\r').unwrap_or(line)).collect()
}

// QString stores unpaired UTF-16 surrogates. Encode them as non-whitespace
// scalar pairs for the string/regex parser and restore the original code
// units in its output. Doubling the marker escapes a literal marker in the
// subtitle, so no valid Unicode character needs to be reserved or lost.
const SURROGATE_MARKER: char = '\u{f0000}';

fn decode_malformed_utf16(text: &[u16]) -> String {
    let mut decoded = String::with_capacity(text.len());
    for unit in char::decode_utf16(text.iter().copied()) {
        match unit {
            Ok(SURROGATE_MARKER) => {
                decoded.push(SURROGATE_MARKER);
                decoded.push(SURROGATE_MARKER);
            }
            Ok(ch) => decoded.push(ch),
            Err(err) => {
                decoded.push(SURROGATE_MARKER);
                decoded.push(char::from_u32(
                    0xe000 + u32::from(err.unpaired_surrogate() - 0xd800),
                ).expect("surrogate offset is a private-use scalar"));
            }
        }
    }
    decoded
}

fn restore_malformed_utf16(entries: &mut [SubtitleEntry]) {
    for entry in entries {
        let decoded = String::from_utf16(&entry.text).expect("parser text is valid UTF-16");
        let mut units = Vec::with_capacity(entry.text.len());
        let mut chars = decoded.chars();
        while let Some(ch) = chars.next() {
            if ch == SURROGATE_MARKER {
                let escaped = chars.next().expect("encoded surrogate or literal marker");
                if escaped == SURROGATE_MARKER {
                    units.extend(SURROGATE_MARKER.encode_utf16(&mut [0; 2]).iter().copied());
                } else {
                    units.push(0xd800 + (escaped as u32 - 0xe000) as u16);
                }
            } else {
                units.extend(ch.encode_utf16(&mut [0; 2]).iter().copied());
            }
        }
        entry.text = units;
    }
}

fn sorted_start(a: &SubtitleEntry, b: &SubtitleEntry) -> Ordering {
    a.start.partial_cmp(&b.start).unwrap_or(Ordering::Equal)
}

fn entry(text: &str, start: f64, end: f64) -> SubtitleEntry {
    SubtitleEntry { text: text.encode_utf16().collect(), start, end }
}

fn parse_ass(lines: &[&str]) -> Result<Vec<SubtitleEntry>, ()> {
    if lines.is_empty() {
        return Err(());
    }
    let has_script_info = lines.iter().take(20).any(|line| {
        let clean = line.trim().trim_start_matches('\u{feff}');
        clean.eq_ignore_ascii_case("[Script Info]")
    });
    let is_events_header = |line: &&str| {
        line.trim()
            .trim_start_matches('\u{feff}')
            .eq_ignore_ascii_case("[Events]")
    };
    if !has_script_info && !lines.iter().any(is_events_header) {
        return Err(());
    }
    let Some(events) = lines.iter().position(is_events_header) else {
        return Ok(Vec::new());
    };
    let mut format_idx = None;
    for (i, line) in lines[events + 1..].iter().enumerate() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            break;
        }
        if strip_ascii_prefix_ignore_case(trimmed, "format:").is_some() {
            format_idx = Some(events + 1 + i);
            break;
        }
    }
    let Some(format_idx) = format_idx else { return Ok(Vec::new()); };
    let format_line = lines[format_idx].trim();
    let spec = strip_ascii_prefix_ignore_case(format_line, "format:")
        .ok_or(())?
        .trim_start();
    let fields: Vec<&str> = spec.split(',').collect();
    let (mut start_at, mut end_at, mut text_at) = (None, None, None);
    for (i, field) in fields.iter().enumerate() {
        match field.trim() {
            f if f.eq_ignore_ascii_case("Start") && start_at.is_some() => return Err(()),
            f if f.eq_ignore_ascii_case("Start") => start_at = Some(i),
            f if f.eq_ignore_ascii_case("End") && end_at.is_some() => return Err(()),
            f if f.eq_ignore_ascii_case("End") => end_at = Some(i),
            f if f.eq_ignore_ascii_case("Text") && text_at.is_some() => return Err(()),
            f if f.eq_ignore_ascii_case("Text") => text_at = Some(i),
            _ => {}
        }
    }
    let (start_at, end_at, text_at) = (start_at.ok_or(())?, end_at.ok_or(())?, text_at.ok_or(())?);
    let mut output = Vec::new();
    for line in &lines[format_idx + 1..] {
        let trimmed = line.trim_start();
        if trimmed.starts_with('[') {
            break;
        }
        let Some(rest) = strip_ascii_prefix_ignore_case(trimmed, "dialogue:") else {
            continue;
        };
        let dialogue = rest
            .strip_prefix(' ')
            .unwrap_or_else(|| rest.trim_start());
        let columns: Vec<&str> = dialogue.split(',').collect();
        if columns.len() < fields.len() { continue; }
        let (Some(start), Some(end)) = (timecode_to_double(columns[start_at]), timecode_to_double(columns[end_at])) else {
            continue;
        };
        if start < 0.0 || end < start { continue; }
        let joined = columns[text_at..].join(",");
        let content = strip_ass_formatting(&joined);
        if !content.trim().is_empty() {
            output.push(entry(&content, start, end));
        }
    }
    output.sort_unstable_by(sorted_start);
    Ok(output)
}

#[derive(Debug)]
struct NumberedEntry {
    position: i32,
    subtitle: SubtitleEntry,
}

fn parse_srt(lines: &[&str]) -> Result<Vec<SubtitleEntry>, ()> {
    let mut index = 0;
    let mut subtitles: Vec<NumberedEntry> = Vec::new();
    while index < lines.len() {
        let mut line = if index == 0 {
            lines[index].trim_start_matches('\u{feff}')
        } else {
            lines[index]
        };
        index += 1;
        while is_blank_line(line) {
            if index == lines.len() {
                break;
            }
            line = lines[index];
            index += 1;
        }
        // QTextStream::atEnd is checked after the position line. A file
        // containing only a position without a following line is ignored.
        if is_blank_line(line) || index == lines.len() { break; }
        let position = line.trim().parse::<u32>().map_err(|_| ())?;
        let position = i32::try_from(position).map_err(|_| ())?;
        let timing = split_pcre2_whitespace(lines[index]);
        index += 1;
        if timing.len() < 3 || timing[1] != "-->" { return Err(()); }
        let start = timecode_to_double(timing[0]).ok_or(())?;
        let end = timecode_to_double(timing[2]).ok_or(())?;
        if start < 0.0 || end < start { return Err(()); }
        if index == lines.len() { return Err(()); }
        let mut text = String::new();
        while index < lines.len() {
            let line = lines[index];
            index += 1;
            if is_blank_line(line) { break; }
            text.push_str(line);
            text.push('\n');
        }
        text.pop();
        let content = strip_srt_style(&text);
        if !content.trim().is_empty() {
            subtitles.push(NumberedEntry { position, subtitle: entry(&content, start, end) });
        }
    }
    subtitles.sort_unstable_by(|a, b| {
        sorted_start(&a.subtitle, &b.subtitle).then_with(|| a.position.cmp(&b.position))
    });
    Ok(subtitles.into_iter().map(|item| item.subtitle).collect())
}

fn parse_vtt(lines: &[&str]) -> Result<Vec<SubtitleEntry>, ()> {
    if lines.is_empty()
        || !lines[0]
            .trim_start_matches('\u{feff}')
            .starts_with("WEBVTT")
    {
        return Err(());
    }
    let mut index = 1;
    while index < lines.len() {
        let line = lines[index];
        index += 1;
        if line.trim().is_empty() { break; }
    }
    let mut output = Vec::new();
    while index < lines.len() {
        let line = lines[index].trim();
        index += 1;
        if line.is_empty() { continue; }
        let tokens = split_pcre2_whitespace(line);
        let first = tokens.first().copied().unwrap_or("");
        if matches!(first, "NOTE" | "STYLE" | "REGION") {
            while index < lines.len() {
                let line = lines[index];
                index += 1;
                if line.trim().is_empty() { break; }
            }
            continue;
        }
        let timing = if tokens.len() >= 3 && tokens[1] == "-->" {
            tokens
        } else {
            if index == lines.len() { return Err(()); }
            let tokens = split_pcre2_whitespace(lines[index]);
            index += 1;
            if tokens.len() < 3 || tokens[1] != "-->" { return Err(()); }
            tokens
        };
        let start = timecode_to_double(timing[0]).ok_or(())?;
        let end = timecode_to_double(timing[2]).ok_or(())?;
        if start < 0.0 || end < start { return Err(()); }
        if index == lines.len() { return Err(()); }
        let mut text = String::new();
        while index < lines.len() {
            let line = lines[index];
            index += 1;
            if is_blank_line(line) { break; }
            text.push_str(line);
            text.push('\n');
        }
        text.pop();
        let content = strip_vtt_tags(&text);
        if !content.trim().is_empty() {
            output.push(entry(&content, start, end));
        }
    }
    output.sort_unstable_by(sorted_start);
    Ok(output)
}

/// Parse the three formats supported by the C++ parser. Qt decodes the file
/// first so its path, BOM and QTextStream behavior remains one implementation.
#[allow(clippy::result_unit_err)]
pub fn parse_subtitles(format: Format, text: &[u16]) -> Result<Vec<SubtitleEntry>, ()> {
    let (decoded, malformed) = match String::from_utf16(text) {
        Ok(decoded) => (decoded, false),
        Err(_) => (decode_malformed_utf16(text), true),
    };
    let lines = text_lines(&decoded);
    let mut entries = match format {
        Format::Ass => parse_ass(&lines),
        Format::Srt => parse_srt(&lines),
        Format::Vtt => parse_vtt(&lines),
    }?;
    if malformed {
        restore_malformed_utf16(&mut entries);
    }
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(format: Format, content: &str) -> Result<Vec<SubtitleEntry>, ()> {
        parse_subtitles(format, &content.encode_utf16().collect::<Vec<_>>())
    }

    #[test]
    fn srt_sorts_equal_starts_by_sequence_number_and_filters_styles() {
        let input = "2\n00:00:01,500 --> 00:00:02,000\n<b>later</b>\n\n1\n00:00:01,500 --> 00:00:02,000\n<i>first</i>\n";
        let result = parse(Format::Srt, input).unwrap();
        assert_eq!(result.len(), 2);
        assert_eq!(String::from_utf16(&result[0].text).unwrap(), "first");
        assert_eq!(String::from_utf16(&result[1].text).unwrap(), "later");
        assert_eq!(result[0].start.to_bits(), 1.5f64.to_bits());
    }

    #[test]
    fn ass_requires_unique_columns_and_preserves_dialogue_commas() {
        let input = "[Script Info]\n[Events]\nFormat: Start,End,Text\n\
            Dialogue: 0:00:01.00,0:00:02.00,{translator note}{\\i1}hello\\Nworld,again{ \\i0 }{}\n\
            [Aegisub Extradata]\n\
            Dialogue: 0:00:03.00,0:00:04.00,not an event\n";
        let result = parse(Format::Ass, input).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(String::from_utf16(&result[0].text).unwrap(), "hello\nworld,again");
        assert!(parse(Format::Ass, "[Script Info]\n[Events]\nFormat: Start,Start,End,Text\n").is_err());
    }

    #[test]
    fn vtt_skips_note_and_rejects_invalid_time() {
        let input = "WEBVTT\n\nNOTE anything\nignored\n\ncaption\n00:01.25 --> 00:02.50\n<b>hello</b>\n";
        let result = parse(Format::Vtt, input).unwrap();
        assert_eq!(String::from_utf16(&result[0].text).unwrap(), "hello");
        assert!(parse(Format::Vtt, "WEBVTT\n\n00:60.00 --> 00:61.00\nbad\n").is_err());
    }

    #[test]
    fn malformed_utf16_in_cue_text_survives_all_formats_and_literal_escape() {
        let cases = [
            (Format::Srt, "1\n00:00:01,250 --> 00:00:02,500\n<b>", "</b>\n"),
            (Format::Ass, "[Script Info]\n[Events]\nFormat: Start,End,Text\nDialogue: 0:00:01.25,0:00:02.50,{\\i1}", "\\Nend\n"),
            (Format::Vtt, "WEBVTT\n\n00:01.25 --> 00:02.50\n<b>", "</b>\n"),
        ];
        for (format, before, after) in cases {
            let mut text: Vec<u16> = before.encode_utf16().collect();
            text.extend([b'A' as u16, 0xd800, 0xdc00, 0xdfff]);
            text.extend(SURROGATE_MARKER.encode_utf16(&mut [0; 2]).iter().copied());
            text.extend(after.encode_utf16());
            let result = parse_subtitles(format, &text).unwrap();
            assert_eq!(result.len(), 1);
            let mut expected = vec![b'A' as u16, 0xd800, 0xdc00, 0xdfff];
            expected.extend(SURROGATE_MARKER.encode_utf16(&mut [0; 2]).iter().copied());
            if format == Format::Ass {
                expected.extend("\nend".encode_utf16());
            }
            assert_eq!(result[0].text, expected);
            assert_eq!(result[0].start.to_bits(), 1.25f64.to_bits());
            assert_eq!(result[0].end.to_bits(), 2.5f64.to_bits());
        }
    }

    #[test]
    fn srt_alignment_override_digits_match_pcre2_ascii_semantics() {
        let input = "1\n00:00:01,00 --> 00:00:02,00\n{\\an١}keep{\\an1}\n";
        let result = parse(Format::Srt, input).unwrap();
        assert_eq!(String::from_utf16(&result[0].text).unwrap(), "{\\an١}keep");
    }

    #[test]
    fn timing_splits_only_pcre2_ascii_whitespace() {
        // QRegularExpression("\\s+") without UCP does not split on NBSP.
        assert!(parse(Format::Srt, "1\n00:00:01,00\u{a0}-->\u{a0}00:00:02,00\ntext\n").is_err());
        assert!(parse(Format::Vtt, "WEBVTT\n\n00:01.00\u{a0}-->\u{a0}00:02.00\ntext\n").is_err());
        assert!(parse(Format::Vtt, "WEBVTT\n\nNOTE\u{a0}meta\nignored\n\n").is_err());
        assert_eq!(parse(Format::Srt, "1\n00:00:01,00\t-->\t00:00:02,00\ntext\n").unwrap().len(), 1);
    }

    #[test]
    fn timecode_integer_fields_match_qt_conversion() {
        // QString::toInt accepts ASCII edge whitespace but not NBSP after
        // converting the field to Latin-1. In SRT, ASCII whitespace inside a
        // timecode splits the timing line before timecodeToDouble; ASS splits
        // columns on commas, reaching QString::toInt directly.
        assert!(parse(
            Format::Srt,
            "1\n00:\u{00a0}00:01,00 --> 00:00:02,00\ncue\n",
        ).is_err());
        assert!(parse(
            Format::Srt,
            "1\n00: 00:01,00 --> 00:00:02,00\ncue\n",
        ).is_err());
        assert!(parse(
            Format::Ass,
            "[Script Info]\n[Events]\nFormat: Start,End,Text\nDialogue: 00:\u{00a0}00:01.00,00:00:02.00,cue\n",
        )
        .unwrap()
        .is_empty());
        assert_eq!(
            parse(
                Format::Ass,
                "[Script Info]\n[Events]\nFormat: Start,End,Text\nDialogue: 00: 00:01.00,00:00:02.00,cue\n",
            )
            .unwrap()
            .len(),
            1,
        );
        // The bounded C++ conversion treats an embedded NUL as a terminator.
        let nul_terminated = parse(
            Format::Srt,
            "1\n00:00:01\u{0000}ignored,00 --> 00:00:02,00\ncue\n",
        ).unwrap();
        assert_eq!(nul_terminated[0].start, 1.0);
    }

    #[test]
    fn srt_and_vtt_strip_utf8_bom() {
        let srt = "\u{feff}1\n00:00:01,000 --> 00:00:02,000\nhello\n";
        let srt_res = parse(Format::Srt, srt).unwrap();
        assert_eq!(srt_res.len(), 1);
        assert_eq!(String::from_utf16(&srt_res[0].text).unwrap(), "hello");

        let vtt = "\u{feff}WEBVTT\n\n00:01.000 --> 00:02.000\nworld\n";
        let vtt_res = parse(Format::Vtt, vtt).unwrap();
        assert_eq!(vtt_res.len(), 1);
        assert_eq!(String::from_utf16(&vtt_res[0].text).unwrap(), "world");
    }

    #[test]
    fn ass_handles_case_insensitive_headers_and_filters_drawing_commands() {
        let input = "[Script Info]\n[Events]\nformat: Start, End, Text\n\
            dialogue: 0:00:01.00,0:00:02.00,{\\pos(10,20)\\pbo-3}keep\n\
            DIALOGUE: 0:00:02.00,0:00:03.00,{\\p1}m 0 0 l 100 0 100 100\n\
            Dialogue: 0:00:03.00,0:00:04.00,{\\p2}m 0 0 l 50 50{\\p0}visible{\\p4}m 1 1\n\
            Dialogue: 0:00:04.00,0:00:05.00,{\\p0}plain\n";
        let res = parse(Format::Ass, input).unwrap();
        assert_eq!(res.len(), 3);
        assert_eq!(String::from_utf16(&res[0].text).unwrap(), "keep");
        assert_eq!(String::from_utf16(&res[1].text).unwrap(), "visible");
        assert_eq!(String::from_utf16(&res[2].text).unwrap(), "plain");
    }

    #[test]
    fn srt_supports_extended_timing_flexible_timecodes_and_whitespace_separators() {
        let input = "\
1\n0:00:01.5 --> 0:00:02.750 X1:100 X2:600 Y1:400 Y2:480\n<font color=\"#fff\">first</font>\n   \n\
2\n00:00:03 --> 00:00:04,500 align:center\n{i}second{/i}\n\t\n";
        let res = parse(Format::Srt, input).unwrap();
        assert_eq!(res.len(), 2);
        assert_eq!(String::from_utf16(&res[0].text).unwrap(), "first");
        assert_eq!(res[0].start, 1.5);
        assert_eq!(res[0].end, 2.75);
        assert_eq!(String::from_utf16(&res[1].text).unwrap(), "second");
        assert_eq!(res[1].start, 3.0);
        assert_eq!(res[1].end, 4.5);
    }
}

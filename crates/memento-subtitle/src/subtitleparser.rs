//! ASS, SRT and VTT parsing over text decoded by Qt's QTextStream. The file
//! opening, URL handling, BOM detection and text-mode newline conversion stay
//! at the Qt boundary; parser decisions and subtitle values live here.

use std::cmp::Ordering;
// Workspace MSRV is 1.77; LazyLock only stabilized in 1.80.
use std::sync::OnceLock;

use regex::Regex;

use crate::subtitleentry::SubtitleEntry;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Format {
    Ass,
    Srt,
    Vtt,
}

fn expression(cell: &'static OnceLock<Regex>, pattern: &'static str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pattern).expect("fixed subtitle regex is valid"))
}

fn ass_style() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    expression(&REGEX, r"\{\\.*?\}")
}

fn ass_newline() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    expression(&REGEX, r"\\[nN]")
}

fn srt_style() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    expression(&REGEX, r"</?[biu]>|\{/?[biu]\}|</?font.*?>|\{\\an?[0-9]\}")
}

fn vtt_tags() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    expression(&REGEX, r"<[^>]*>")
}

// QString::toInt converts through Latin-1, trims ASCII whitespace, and treats
// an embedded NUL as the end of its numeric input.
fn qt_to_i32(value: &str) -> Option<i32> {
    let value = value.split_once('\0').map_or(value, |(prefix, _)| prefix);
    value.trim_matches(|ch: char| ch.is_ascii_whitespace()).parse().ok()
}

fn timecode_to_double(timecode: &str) -> Option<f64> {
    let pieces: Vec<&str> = timecode.trim().split([':', '.', ',']).collect();
    if pieces.len() != 3 && pieces.len() != 4 {
        return None;
    }
    let frac = pieces[pieces.len() - 1];
    let fraction = match frac.encode_utf16().count() {
        2 => {
            let value = qt_to_i32(frac)?;
            if !(0..=99).contains(&value) { return None; }
            value as f64 * 0.01
        }
        3 => {
            let value = qt_to_i32(frac)?;
            if !(0..=999).contains(&value) { return None; }
            value as f64 * 0.001
        }
        _ => return None,
    };
    let seconds = qt_to_i32(pieces[pieces.len() - 2])?;
    let minutes = qt_to_i32(pieces[pieces.len() - 3])?;
    if !(0..=59).contains(&seconds) || !(0..=59).contains(&minutes) {
        return None;
    }
    // Match the C++ accumulation order, including floating-point rounding.
    let mut value = 0.0;
    value += fraction;
    value += seconds as f64;
    value += (minutes * 60) as f64;
    if pieces.len() == 4 {
        let hours = qt_to_i32(pieces[0])?;
        if hours < 0 { return None; }
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
    if !has_script_info && !lines.iter().any(|line| line.trim().eq_ignore_ascii_case("[Events]")) {
        return Err(());
    }
    let Some(events) = lines.iter().position(|line| line.trim().eq_ignore_ascii_case("[Events]")) else {
        return Ok(Vec::new());
    };
    let mut format_idx = None;
    for (i, line) in lines[events + 1..].iter().enumerate() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            break;
        }
        if trimmed.to_ascii_lowercase().starts_with("format:") {
            format_idx = Some(events + 1 + i);
            break;
        }
    }
    let Some(format_idx) = format_idx else { return Ok(Vec::new()); };
    let format_line = lines[format_idx].trim();
    let spec = if let Some(spec) = format_line.strip_prefix("Format: ") {
        spec
    } else if let Some(spec) = format_line.strip_prefix("Format:") {
        spec.trim_start()
    } else {
        return Err(());
    };
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
        let dialogue = if let Some(d) = trimmed.strip_prefix("Dialogue: ") {
            d
        } else if let Some(d) = trimmed.strip_prefix("Dialogue:") {
            d.trim_start()
        } else {
            continue;
        };
        let columns: Vec<&str> = dialogue.split(',').collect();
        if columns.len() < fields.len() { continue; }
        let (Some(start), Some(end)) = (timecode_to_double(columns[start_at]), timecode_to_double(columns[end_at])) else {
            continue;
        };
        if start < 0.0 || end < start { continue; }
        let joined = columns[text_at..].join(",");
        let unstyled = ass_style().replace_all(&joined, "");
        let content = ass_newline().replace_all(&unstyled, "\n");
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
        let mut line = lines[index];
        index += 1;
        while index < lines.len() && line.is_empty() {
            line = lines[index];
            index += 1;
        }
        // QTextStream::atEnd is checked after the position line. A file
        // containing only a position without a following line is ignored.
        if index == lines.len() { break; }
        let position = line.trim().parse::<u32>().map_err(|_| ())?;
        let position = i32::try_from(position).map_err(|_| ())?;
        let timing: Vec<&str> = lines[index].split_ascii_whitespace().collect();
        index += 1;
        if timing.len() != 3 || timing[1] != "-->" { return Err(()); }
        let start = timecode_to_double(timing[0]).ok_or(())?;
        let end = timecode_to_double(timing[2]).ok_or(())?;
        if start < 0.0 || end < start { return Err(()); }
        if index == lines.len() { return Err(()); }
        let mut text = String::new();
        while index < lines.len() {
            let line = lines[index];
            index += 1;
            if line.is_empty() { break; }
            text.push_str(line);
            text.push('\n');
        }
        text.pop();
        let content = srt_style().replace_all(&text, "");
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
    if lines.is_empty() || !lines[0].starts_with("WEBVTT") { return Err(()); }
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
        let first = line.split_ascii_whitespace().next().unwrap_or("");
        if matches!(first, "NOTE" | "STYLE" | "REGION") {
            while index < lines.len() {
                let line = lines[index];
                index += 1;
                if line.trim().is_empty() { break; }
            }
            continue;
        }
        let tokens: Vec<&str> = line.split_ascii_whitespace().collect();
        let timing = if tokens.len() >= 3 && tokens[1] == "-->" {
            tokens
        } else {
            if index == lines.len() { return Err(()); }
            let tokens = lines[index].split_ascii_whitespace().collect::<Vec<_>>();
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
            if line.is_empty() { break; }
            text.push_str(line);
            text.push('\n');
        }
        text.pop();
        let content = vtt_tags().replace_all(&text, "");
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
        let input = "[Script Info]\n[Events]\nFormat: Start,End,Text\nDialogue: 0:00:01.00,0:00:02.00,{\\i1}hello\\Nworld,again\n";
        let result = parse(Format::Ass, input).unwrap();
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
        // converting the field to Latin-1.
        assert!(parse(
            Format::Srt,
            "1\n00:\u{00a0}00:01,00 --> 00:00:02,00\ncue\n",
        ).is_err());
        assert_eq!(
            parse(Format::Srt, "1\n00: 00:01,00 --> 00:00:02,00\ncue\n")
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
}

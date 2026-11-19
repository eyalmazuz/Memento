//! ASS/SSA script header parsing, Dialogue parsing, override-tag filtering, and
//! 24-bit color-ID probe instrumentation.
//!
//! The color-ID probe assigns each visible character a unique `\1c&HBBGGRR&` while
//! stripping colors, alphas, borders, shadows, blurs, fades, and karaoke fills, and
//! preserving every layout-affecting tag (`\pos`, `\move`, `\an`, `\a`, `\org`, `\fn`,
//! `\fe`, `\fs`, `\fscx`, `\fscy`, `\fsp`, `\b`, `\i`, `\u`, `\s`, `\fr*`, `\fa*`,
//! `\q`, `\r`, `\clip`, `\iclip`, and geometry tags inside `\t(...)`).

use std::fmt::Write as _;

/// Maximum number of unique per-character color IDs emitted in a single probe frame.
/// Beyond this cap (e.g. massive sign walls), characters are still kept in `plain_text`
/// with `color_id == 0` so probing stays fast and bounded.
pub const MAX_PROBED_CHARS: usize = 512;

/// Parsed metadata from the `[Script Info]` section of an ASS/SSA script.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AssScriptInfo {
    pub script_type: Option<String>,
    pub play_res_x: Option<u32>,
    pub play_res_y: Option<u32>,
    pub layout_res_x: Option<u32>,
    pub layout_res_y: Option<u32>,
    pub scaled_border_and_shadow: Option<bool>,
    pub wrap_style: Option<u8>,
    pub collisions: Option<String>,
    pub ycbcr_matrix: Option<String>,
}

/// Column mapping parsed from `[Events]` `Format: ...`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventsFormat {
    pub columns: Vec<String>,
    pub layer_idx: Option<usize>,
    pub start_idx: Option<usize>,
    pub end_idx: Option<usize>,
    pub style_idx: Option<usize>,
    pub name_idx: Option<usize>,
    pub margin_l_idx: Option<usize>,
    pub margin_r_idx: Option<usize>,
    pub margin_v_idx: Option<usize>,
    pub effect_idx: Option<usize>,
    pub text_idx: usize,
}

impl Default for EventsFormat {
    fn default() -> Self {
        Self {
            columns: vec![
                "Layer".into(),
                "Start".into(),
                "End".into(),
                "Style".into(),
                "Name".into(),
                "MarginL".into(),
                "MarginR".into(),
                "MarginV".into(),
                "Effect".into(),
                "Text".into(),
            ],
            layer_idx: Some(0),
            start_idx: Some(1),
            end_idx: Some(2),
            style_idx: Some(3),
            name_idx: Some(4),
            margin_l_idx: Some(5),
            margin_r_idx: Some(6),
            margin_v_idx: Some(7),
            effect_idx: Some(8),
            text_idx: 9,
        }
    }
}

/// A parsed `Dialogue:` line from an ASS/SSA `[Events]` section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssDialogueLine {
    pub layer: i32,
    pub start_ms: i64,
    pub end_ms: i64,
    pub style: String,
    pub name: String,
    pub margin_l: i32,
    pub margin_r: i32,
    pub margin_v: i32,
    pub effect: String,
    pub raw_text: String,
}

/// One visible or whitespace character extracted from an ASS `Dialogue:` event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedChar {
    pub ch: char,
    /// UTF-8 byte offset within the parent [`ParsedEvent::plain_text`].
    pub byte_offset: usize,
    /// UTF-16 code unit offset within [`ParsedEvent::plain_text`].
    pub utf16_offset: usize,
    /// UTF-16 code unit length (`1` for BMP, `2` for supplementary plane).
    pub utf16_len: usize,
    /// Unique 24-bit color ID (`1..=MAX_PROBED_CHARS`) used in the probe script,
    /// or `0` for whitespace / capped characters.
    pub color_id: u32,
    /// Index of the explicit `\N` (or `\n` under `WrapStyle: 2`) line segment.
    pub explicit_line_idx: u16,
    /// True if this character is whitespace (` `, `　`, `\t`, `\h`) with no inked pixels.
    pub is_whitespace: bool,
}

/// Parsed structure and metadata of a single active ASS `Dialogue:` event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedEvent {
    pub event_index: usize,
    pub dialogue: AssDialogueLine,
    /// Plain text with override tags & vector drawings stripped (`\N` -> `\n`, `\h` -> ` `).
    pub plain_text: String,
    /// Inline characters belonging to `plain_text` (excludes `\n` line breaks).
    pub chars: Vec<ParsedChar>,
    /// True if the event only contains `\p1..` vector drawing commands and no readable text.
    pub is_drawing_only: bool,
    /// True if the event uses `\move(...)` or geometry-changing tags inside `\t(...)`.
    pub has_animated_geometry: bool,
    /// True if the event has non-zero rotation/shear (`\frz`, `\frx`, `\fry`, `\fax`, `\fay`).
    pub is_rotated_or_sign: bool,
    /// True if the event uses explicit `\pos(...)` or `\move(...)` positioning.
    pub has_explicit_pos: bool,
}

/// Result of instrumenting active ASS dialogue lines for a single layout probe.
#[derive(Debug, Clone)]
pub struct ProbeScript {
    /// Complete ASS script with per-character 24-bit `\1c` color IDs and border/shadow zeroed.
    pub probe_ass: String,
    /// Complete unmodified ASS script (same headers + timestamp-clamped events) for
    /// exact highlight-patch or full-frame rendering.
    pub clean_ass: String,
    /// Parsed `[Script Info]` metadata from `extradata`.
    pub script_info: AssScriptInfo,
    /// Parsed active dialogue events in script order.
    pub events: Vec<ParsedEvent>,
    /// Number of characters assigned unique probe color IDs.
    pub total_probed_chars: usize,
    /// Timestamp (ms) guaranteed to fall within `[start, end)` for all emitted events.
    pub probe_now_ms: i64,
    /// Direct lookup table mapping `(color_id - 1) -> (event_index, char_index_in_event)`.
    pub color_id_map: Vec<(usize, usize)>,
}

/// Decode a 24-bit probe color ID from a `libass` `ASS_Image.color` (`0xRRGGBBAA`).
#[inline]
pub fn decode_color_id(ass_image_color: u32) -> u32 {
    let r = (ass_image_color >> 24) & 0xFF;
    let g = (ass_image_color >> 16) & 0xFF;
    let b = (ass_image_color >> 8) & 0xFF;
    r | (g << 8) | (b << 16)
}

/// Encode a 24-bit probe color ID into an ASS `\1c&HBBGGRR&` override tag value.
#[inline]
pub fn format_color_id_tag(color_id: u32) -> String {
    let r = color_id & 0xFF;
    let g = (color_id >> 8) & 0xFF;
    let b = (color_id >> 16) & 0xFF;
    format!(r"\1c&H{b:02X}{g:02X}{r:02X}&")
}

/// Parse `[Script Info]` fields from an ASS/SSA header or full document.
pub fn parse_script_info(ass_text: &str) -> AssScriptInfo {
    let mut info = AssScriptInfo::default();
    let mut in_section = false;

    for raw_line in ass_text.lines() {
        let line = raw_line.trim().trim_start_matches('\u{feff}');
        if line.is_empty() || line.starts_with(';') {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            in_section = line.eq_ignore_ascii_case("[Script Info]");
            continue;
        }
        if !in_section {
            continue;
        }
        let Some((key, val)) = line.split_once(':') else {
            continue;
        };
        let key = key.trim();
        let val = val.trim();
        if key.eq_ignore_ascii_case("ScriptType") {
            info.script_type = Some(val.to_string());
        } else if key.eq_ignore_ascii_case("PlayResX") {
            info.play_res_x = val.parse().ok();
        } else if key.eq_ignore_ascii_case("PlayResY") {
            info.play_res_y = val.parse().ok();
        } else if key.eq_ignore_ascii_case("LayoutResX") {
            info.layout_res_x = val.parse().ok();
        } else if key.eq_ignore_ascii_case("LayoutResY") {
            info.layout_res_y = val.parse().ok();
        } else if key.eq_ignore_ascii_case("ScaledBorderAndShadow") {
            info.scaled_border_and_shadow = match val.to_ascii_lowercase().as_str() {
                "yes" | "1" | "true" => Some(true),
                "no" | "0" | "false" => Some(false),
                _ => None,
            };
        } else if key.eq_ignore_ascii_case("WrapStyle") {
            info.wrap_style = val.parse().ok();
        } else if key.eq_ignore_ascii_case("Collisions") {
            info.collisions = Some(val.to_string());
        } else if key.eq_ignore_ascii_case("YCbCr Matrix") {
            info.ycbcr_matrix = Some(val.to_string());
        }
    }

    info
}

/// Parse the `Format:` line under `[Events]` (or fall back to the standard ASS v4+ 10-column order).
pub fn parse_events_format(ass_text: &str) -> EventsFormat {
    let mut in_events = false;
    for raw_line in ass_text.lines() {
        let line = raw_line.trim().trim_start_matches('\u{feff}');
        if line.starts_with('[') && line.ends_with(']') {
            in_events = line.eq_ignore_ascii_case("[Events]");
            continue;
        }
        if !in_events {
            continue;
        }
        if let Some(rest) = strip_ascii_prefix_ignore_case(line, "Format:") {
            let cols: Vec<String> = rest
                .split(',')
                .map(|c| c.trim().to_string())
                .filter(|c| !c.is_empty())
                .collect();
            if cols.is_empty() {
                break;
            }
            let find = |names: &[&str]| -> Option<usize> {
                cols.iter()
                    .position(|c| names.iter().any(|n| c.eq_ignore_ascii_case(n)))
            };
            let text_idx = find(&["Text"]).unwrap_or(cols.len() - 1);
            return EventsFormat {
                layer_idx: find(&["Layer", "Marked"]),
                start_idx: find(&["Start"]),
                end_idx: find(&["End"]),
                style_idx: find(&["Style"]),
                name_idx: find(&["Name", "Actor"]),
                margin_l_idx: find(&["MarginL"]),
                margin_r_idx: find(&["MarginR"]),
                margin_v_idx: find(&["MarginV"]),
                effect_idx: find(&["Effect"]),
                text_idx,
                columns: cols,
            };
        }
    }
    EventsFormat::default()
}

/// Parse an ASS timecode (`H:MM:SS.cc` or `HH:MM:SS.cc`, optional 1–3 fractional digits) into ms.
pub fn parse_ass_timecode_ms(s: &str) -> Option<i64> {
    let s = s.trim();
    let mut parts = s.split(':');
    let hours: i64 = parts.next()?.trim().parse().ok()?;
    let mins: i64 = parts.next()?.trim().parse().ok()?;
    let sec_part = parts.next()?.trim();
    if parts.next().is_some() {
        return None;
    }

    let (secs_str, frac_str) = match sec_part.split_once('.') {
        Some((sec, frac)) => (sec, frac),
        None => (sec_part, "0"),
    };
    let secs: i64 = secs_str.parse().ok()?;
    let frac_ms: i64 = match frac_str.len() {
        0 => 0,
        1 => frac_str.parse::<i64>().ok()? * 100,
        2 => frac_str.parse::<i64>().ok()? * 10,
        _ => frac_str.get(..3)?.parse::<i64>().ok()?,
    };

    Some(((hours * 60 + mins) * 60 + secs) * 1000 + frac_ms)
}

/// Format milliseconds into an ASS timecode (`H:MM:SS.cc`, clamped at `0:00:00.00`).
pub fn format_ass_timecode(ms: i64) -> String {
    let cs_total = (ms.max(0) / 10) as u64;
    let cs = cs_total % 100;
    let total_secs = cs_total / 100;
    let secs = total_secs % 60;
    let mins = (total_secs / 60) % 60;
    let hours = total_secs / 3600;
    format!("{hours}:{mins:02}:{secs:02}.{cs:02}")
}

/// Format milliseconds rounding UP to the next centisecond so `end > now_ms` holds strictly.
fn format_ass_timecode_ceil(ms: i64) -> String {
    let cs_total = ((ms.max(0) + 9) / 10) as u64;
    let cs = cs_total % 100;
    let total_secs = cs_total / 100;
    let secs = total_secs % 60;
    let mins = (total_secs / 60) % 60;
    let hours = total_secs / 3600;
    format!("{hours}:{mins:02}:{secs:02}.{cs:02}")
}

/// Parse a single `Dialogue: ...` line according to `format`.
pub fn parse_dialogue_line(line: &str, format: &EventsFormat) -> Option<AssDialogueLine> {
    let rest = strip_ascii_prefix_ignore_case(line.trim_start(), "Dialogue:")?.trim_start();
    let total_cols = format.columns.len().max(format.text_idx + 1);
    let mut fields: Vec<&str> = Vec::with_capacity(total_cols);

    let mut rem = rest;
    for col in 0..total_cols {
        if col == format.text_idx || col + 1 == total_cols {
            fields.push(rem);
            break;
        }
        match rem.split_once(',') {
            Some((head, tail)) => {
                fields.push(head.trim());
                rem = tail;
            }
            None => {
                fields.push(rem.trim());
                break;
            }
        }
    }

    let get = |idx: Option<usize>| -> &str { idx.and_then(|i| fields.get(i).copied()).unwrap_or("") };
    let parse_i32 = |s: &str| -> i32 {
        // SSA v4 `Marked=0` field compatibility.
        let s = s.strip_prefix("Marked=").unwrap_or(s);
        s.trim().parse().unwrap_or(0)
    };

    let raw_text = fields.get(format.text_idx).copied().unwrap_or("").to_string();
    let start_ms = parse_ass_timecode_ms(get(format.start_idx)).unwrap_or(0);
    let end_ms = parse_ass_timecode_ms(get(format.end_idx)).unwrap_or(start_ms + 5_000);

    Some(AssDialogueLine {
        layer: parse_i32(get(format.layer_idx)),
        start_ms,
        end_ms,
        style: get(format.style_idx).to_string(),
        name: get(format.name_idx).to_string(),
        margin_l: parse_i32(get(format.margin_l_idx)),
        margin_r: parse_i32(get(format.margin_r_idx)),
        margin_v: parse_i32(get(format.margin_v_idx)),
        effect: get(format.effect_idx).to_string(),
        raw_text,
    })
}

/// Strip ASS override blocks and `\p1..` vector drawings from dialogue text, producing
/// clean readable text (`\N` -> `\n`, `\n` -> ` `, `\h` -> ` `).
pub fn strip_ass_dialogue_text(raw_text: &str) -> String {
    strip_ass_dialogue_text_with_wrap(raw_text, None)
}

/// Strip ASS override blocks respecting `WrapStyle` (`WrapStyle: 2` makes `\n` a hard break).
pub fn strip_ass_dialogue_text_with_wrap(raw_text: &str, wrap_style: Option<u8>) -> String {
    let mut out = String::with_capacity(raw_text.len());
    let mut drawing_level: u32 = 0;
    let bytes = raw_text.as_bytes();
    let mut i = 0;

    while i < bytes.len() {
        if bytes[i] == b'{' {
            if let Some(rel_end) = raw_text[i + 1..].find('}') {
                let block = &raw_text[i + 1..i + 1 + rel_end];
                for token in split_override_tokens(block) {
                    if let Some(level) = parse_drawing_level(token) {
                        drawing_level = level;
                    }
                }
                i = i + 1 + rel_end + 1;
                continue;
            }
        }

        if drawing_level > 0 {
            let ch = raw_text[i..].chars().next().unwrap();
            i += ch.len_utf8();
            continue;
        }

        if bytes[i] == b'\\' && i + 1 < bytes.len() {
            match bytes[i + 1] {
                b'N' => {
                    out.push('\n');
                    i += 2;
                    continue;
                }
                b'n' => {
                    if wrap_style == Some(2) {
                        out.push('\n');
                    } else {
                        out.push(' ');
                    }
                    i += 2;
                    continue;
                }
                b'h' => {
                    out.push(' ');
                    i += 2;
                    continue;
                }
                _ => {}
            }
        }

        let ch = raw_text[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }

    out
}

/// Returns true if `ch` is a Japanese, CJK, or alphanumeric character worth initiating
/// a dictionary lookup on (filters out standalone punctuation, symbols, and emojis).
pub fn is_japanese_or_alnum_lookup_char(ch: char) -> bool {
    if ch.is_alphanumeric() {
        return true;
    }
    matches!(
        ch as u32,
        0x3040..=0x309F // Hiragana
        | 0x30A0..=0x30FF // Katakana
        | 0x31F0..=0x31FF // Katakana Phonetic Extensions
        | 0x3400..=0x4DBF // CJK Unified Ideographs Extension A
        | 0x4E00..=0x9FFF // CJK Unified Ideographs
        | 0xF900..=0xFAFF // CJK Compatibility Ideographs
        | 0xFF66..=0xFF9F // Halfwidth Katakana
        | 0x3005..=0x3007 // 々 〆 〇
    )
}

/// Build an instrumented probe ASS script and matching clean ASS script from
/// `extradata` (header: `[Script Info]`, `[V4+ Styles]`, optional `[Events]`) and
/// `ass_full` (active `Dialogue:` lines, e.g. from mpv `sub-text/ass-full`).
///
/// If `ass_full` is empty and `extradata` already contains `Dialogue:` lines (such as a full
/// `.ass` document), events active at `now_ms` (`start <= now_ms < end`) are extracted from
/// `extradata` directly.
pub fn build_probe_script(extradata: &str, ass_full: &str, now_ms: i64) -> ProbeScript {
    let script_info = parse_script_info(extradata);
    let format = parse_events_format(extradata);
    let header_prefix = extract_header_up_to_events_format(extradata, &format);

    let dialogue_lines: Vec<AssDialogueLine> = if !ass_full.trim().is_empty() {
        ass_full
            .lines()
            .filter_map(|line| parse_dialogue_line(line, &format))
            .collect()
    } else {
        extract_active_dialogues_from_document(extradata, &format, now_ms)
    };

    let mut probe_ass = String::with_capacity(header_prefix.len() + ass_full.len() * 4 + 256);
    let mut clean_ass = String::with_capacity(header_prefix.len() + ass_full.len() + 256);
    probe_ass.push_str(&header_prefix);
    clean_ass.push_str(&header_prefix);

    let mut events = Vec::with_capacity(dialogue_lines.len());
    let mut color_id_map: Vec<(usize, usize)> = Vec::new();
    let mut next_color_id: u32 = 1;

    for (event_index, dialogue) in dialogue_lines.into_iter().enumerate() {
        // Preserve exact Start/End when now_ms is already inside [start, end) so \move/\t
        // interpolation matches mpv identically. Only widen if centisecond rounding or
        // sub-delay put now_ms outside [start, end).
        let (eff_start_str, eff_end_str) =
            effective_timecode_bounds(dialogue.start_ms, dialogue.end_ms, now_ms);

        let instrumented = instrument_dialogue_text(
            &dialogue.raw_text,
            script_info.wrap_style,
            event_index,
            &mut next_color_id,
            &mut color_id_map,
        );

        write_dialogue_row(
            &mut clean_ass,
            &format,
            &dialogue,
            &eff_start_str,
            &eff_end_str,
            &dialogue.raw_text,
        );
        write_dialogue_row(
            &mut probe_ass,
            &format,
            &dialogue,
            &eff_start_str,
            &eff_end_str,
            &instrumented.probe_text,
        );

        events.push(ParsedEvent {
            event_index,
            dialogue,
            plain_text: instrumented.plain_text,
            chars: instrumented.chars,
            is_drawing_only: instrumented.is_drawing_only,
            has_animated_geometry: instrumented.has_animated_geometry,
            is_rotated_or_sign: instrumented.is_rotated_or_sign,
            has_explicit_pos: instrumented.has_explicit_pos,
        });
    }

    let total_probed_chars = color_id_map.len();
    ProbeScript {
        probe_ass,
        clean_ass,
        script_info,
        events,
        total_probed_chars,
        probe_now_ms: now_ms.max(0),
        color_id_map,
    }
}

/// Extract all `Dialogue:` lines from a complete ASS document that are active at `now_ms`.
pub fn extract_active_dialogues_from_document(
    ass_doc: &str,
    format: &EventsFormat,
    now_ms: i64,
) -> Vec<AssDialogueLine> {
    let mut in_events = false;
    let mut out = Vec::new();
    for raw_line in ass_doc.lines() {
        let trimmed = raw_line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            in_events = trimmed.eq_ignore_ascii_case("[Events]");
            continue;
        }
        if !in_events {
            continue;
        }
        if let Some(d) = parse_dialogue_line(trimmed, format) {
            if d.start_ms <= now_ms && now_ms < d.end_ms {
                out.push(d);
            }
        }
    }
    out
}

fn effective_timecode_bounds(start_ms: i64, end_ms: i64, now_ms: i64) -> (String, String) {
    let now = now_ms.max(0);
    let eff_start = if start_ms <= now {
        start_ms.max(0)
    } else {
        now
    };
    let mut eff_end = if end_ms > now {
        end_ms
    } else {
        now + 1_000
    };
    // Ensure end centisecond is strictly greater than now_ms.
    let end_cs_ms = ((eff_end + 9) / 10) * 10;
    if end_cs_ms <= now {
        eff_end = now + 20;
    }
    (format_ass_timecode(eff_start), format_ass_timecode_ceil(eff_end))
}

fn extract_header_up_to_events_format(extradata: &str, format: &EventsFormat) -> String {
    let mut out = String::with_capacity(extradata.len() + 128);
    let mut in_events = false;
    let mut saw_events_section = false;
    let mut saw_events_format = false;

    for raw_line in extradata.lines() {
        let trimmed = raw_line.trim().trim_start_matches('\u{feff}');
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            in_events = trimmed.eq_ignore_ascii_case("[Events]");
            if in_events {
                saw_events_section = true;
            }
            out.push_str(trimmed);
            out.push('\n');
            continue;
        }
        if in_events {
            if strip_ascii_prefix_ignore_case(trimmed, "Dialogue:").is_some()
                || strip_ascii_prefix_ignore_case(trimmed, "Comment:").is_some()
            {
                // Strip any pre-existing events; callers pass active events separately.
                continue;
            }
            if strip_ascii_prefix_ignore_case(trimmed, "Format:").is_some() {
                saw_events_format = true;
            }
        }
        out.push_str(raw_line.trim_end_matches('\r'));
        out.push('\n');
    }

    if !saw_events_section {
        if !out.ends_with("\n\n") && !out.is_empty() {
            out.push('\n');
        }
        out.push_str("[Events]\n");
    }
    if !saw_events_format {
        out.push_str("Format: ");
        out.push_str(&format.columns.join(", "));
        out.push('\n');
    }
    out
}

fn write_dialogue_row(
    out: &mut String,
    format: &EventsFormat,
    d: &AssDialogueLine,
    start_tc: &str,
    end_tc: &str,
    text: &str,
) {
    out.push_str("Dialogue: ");
    for (idx, col) in format.columns.iter().enumerate() {
        if idx > 0 {
            out.push(',');
        }
        if idx == format.text_idx {
            out.push_str(text);
        } else if Some(idx) == format.layer_idx {
            if col.eq_ignore_ascii_case("Marked") {
                let _ = write!(out, "Marked={}", d.layer);
            } else {
                let _ = write!(out, "{}", d.layer);
            }
        } else if Some(idx) == format.start_idx {
            out.push_str(start_tc);
        } else if Some(idx) == format.end_idx {
            out.push_str(end_tc);
        } else if Some(idx) == format.style_idx {
            out.push_str(&d.style);
        } else if Some(idx) == format.name_idx {
            out.push_str(&d.name);
        } else if Some(idx) == format.margin_l_idx {
            let _ = write!(out, "{}", d.margin_l);
        } else if Some(idx) == format.margin_r_idx {
            let _ = write!(out, "{}", d.margin_r);
        } else if Some(idx) == format.margin_v_idx {
            let _ = write!(out, "{}", d.margin_v);
        } else if Some(idx) == format.effect_idx {
            out.push_str(&d.effect);
        }
    }
    out.push('\n');
}

struct InstrumentedDialogue {
    probe_text: String,
    plain_text: String,
    chars: Vec<ParsedChar>,
    is_drawing_only: bool,
    has_animated_geometry: bool,
    is_rotated_or_sign: bool,
    has_explicit_pos: bool,
}

const PROBE_BASE_TAGS: &str = r"\bord0\shad0\3a&HFF&\4a&HFF&\1a&H00&";

fn instrument_dialogue_text(
    raw_text: &str,
    wrap_style: Option<u8>,
    event_index: usize,
    next_color_id: &mut u32,
    color_id_map: &mut Vec<(usize, usize)>,
) -> InstrumentedDialogue {
    let mut probe_text = String::with_capacity(raw_text.len() * 4 + 64);
    let mut plain_text = String::with_capacity(raw_text.len());
    let mut chars: Vec<ParsedChar> = Vec::new();
    let mut utf16_cursor: usize = 0;
    let mut explicit_line_idx: u16 = 0;

    let mut drawing_level: u32 = 0;
    let mut saw_any_drawing = false;
    let mut has_animated_geometry = false;
    let mut is_rotated_or_sign = false;
    let mut has_explicit_pos = false;

    // Seed border=0, shadow=0, transparent outline/shadow, opaque fill at event start.
    probe_text.push('{');
    probe_text.push_str(PROBE_BASE_TAGS);
    probe_text.push('}');

    let bytes = raw_text.as_bytes();
    let mut i = 0;
    let mut last_non_ws_color_id: u32 = 0;

    while i < bytes.len() {
        if bytes[i] == b'{' {
            if let Some(rel_end) = raw_text[i + 1..].find('}') {
                let block = &raw_text[i + 1..i + 1 + rel_end];
                let filtered = filter_override_block(
                    block,
                    &mut drawing_level,
                    &mut saw_any_drawing,
                    &mut has_animated_geometry,
                    &mut is_rotated_or_sign,
                    &mut has_explicit_pos,
                );
                if !filtered.is_empty() {
                    probe_text.push('{');
                    probe_text.push_str(&filtered);
                    probe_text.push('}');
                }
                i = i + 1 + rel_end + 1;
                continue;
            }
        }

        if drawing_level > 0 {
            let ch = raw_text[i..].chars().next().unwrap();
            probe_text.push(ch);
            i += ch.len_utf8();
            continue;
        }

        if bytes[i] == b'\\' && i + 1 < bytes.len() {
            match bytes[i + 1] {
                b'N' => {
                    plain_text.push('\n');
                    utf16_cursor += 1;
                    explicit_line_idx = explicit_line_idx.saturating_add(1);
                    probe_text.push_str(r"\N");
                    last_non_ws_color_id = 0;
                    i += 2;
                    continue;
                }
                b'n' => {
                    if wrap_style == Some(2) {
                        plain_text.push('\n');
                        utf16_cursor += 1;
                        explicit_line_idx = explicit_line_idx.saturating_add(1);
                    } else {
                        let byte_offset = plain_text.len();
                        plain_text.push(' ');
                        chars.push(ParsedChar {
                            ch: ' ',
                            byte_offset,
                            utf16_offset: utf16_cursor,
                            utf16_len: 1,
                            color_id: 0,
                            explicit_line_idx,
                            is_whitespace: true,
                        });
                        utf16_cursor += 1;
                    }
                    probe_text.push_str(r"\n");
                    last_non_ws_color_id = 0;
                    i += 2;
                    continue;
                }
                b'h' => {
                    let byte_offset = plain_text.len();
                    plain_text.push(' ');
                    chars.push(ParsedChar {
                        ch: ' ',
                        byte_offset,
                        utf16_offset: utf16_cursor,
                        utf16_len: 1,
                        color_id: 0,
                        explicit_line_idx,
                        is_whitespace: true,
                    });
                    utf16_cursor += 1;
                    probe_text.push_str(r"\h");
                    last_non_ws_color_id = 0;
                    i += 2;
                    continue;
                }
                _ => {}
            }
        }

        let ch = raw_text[i..].chars().next().unwrap();
        let byte_offset = plain_text.len();
        let utf16_len = ch.len_utf16();
        plain_text.push(ch);

        let is_ws = ch.is_whitespace();
        let char_idx_in_event = chars.len();

        if is_ws {
            chars.push(ParsedChar {
                ch,
                byte_offset,
                utf16_offset: utf16_cursor,
                utf16_len,
                color_id: 0,
                explicit_line_idx,
                is_whitespace: true,
            });
            probe_text.push(ch);
            last_non_ws_color_id = 0;
        } else if is_combining_or_joiner(ch) && last_non_ws_color_id != 0 {
            // Keep combining marks / variation selectors attached to the preceding
            // grapheme's HarfBuzz shaping cluster with the same color ID.
            chars.push(ParsedChar {
                ch,
                byte_offset,
                utf16_offset: utf16_cursor,
                utf16_len,
                color_id: last_non_ws_color_id,
                explicit_line_idx,
                is_whitespace: false,
            });
            probe_text.push(ch);
        } else if (*next_color_id as usize) <= MAX_PROBED_CHARS {
            let cid = *next_color_id;
            *next_color_id += 1;
            color_id_map.push((event_index, char_idx_in_event));
            chars.push(ParsedChar {
                ch,
                byte_offset,
                utf16_offset: utf16_cursor,
                utf16_len,
                color_id: cid,
                explicit_line_idx,
                is_whitespace: false,
            });
            probe_text.push('{');
            probe_text.push_str(&format_color_id_tag(cid));
            probe_text.push('}');
            probe_text.push(ch);
            last_non_ws_color_id = cid;
        } else {
            // Past probe cap: render transparently so it cannot collide with the last valid ID.
            chars.push(ParsedChar {
                ch,
                byte_offset,
                utf16_offset: utf16_cursor,
                utf16_len,
                color_id: 0,
                explicit_line_idx,
                is_whitespace: false,
            });
            probe_text.push_str(r"{\1a&HFF&}");
            probe_text.push(ch);
            last_non_ws_color_id = 0;
        }

        utf16_cursor += utf16_len;
        i += ch.len_utf8();
    }

    let is_drawing_only = saw_any_drawing && chars.iter().all(|c| c.is_whitespace);

    InstrumentedDialogue {
        probe_text,
        plain_text,
        chars,
        is_drawing_only,
        has_animated_geometry,
        is_rotated_or_sign,
        has_explicit_pos,
    }
}

/// Split the contents of an ASS `{...}` override block into individual `\tag...` tokens,
/// preserving nested parentheses (such as `\t(0,500,\clip(0,0,100,100)\fscx120)`).
fn split_override_tokens(block: &str) -> Vec<&str> {
    let mut tokens = Vec::new();
    let bytes = block.as_bytes();
    let mut start: Option<usize> = None;
    let mut paren_depth: i32 = 0;

    for (idx, &b) in bytes.iter().enumerate() {
        match b {
            b'(' => paren_depth += 1,
            b')' => paren_depth = (paren_depth - 1).max(0),
            b'\\' if paren_depth == 0 => {
                if let Some(s) = start {
                    tokens.push(&block[s..idx]);
                }
                start = Some(idx);
            }
            _ => {}
        }
    }
    if let Some(s) = start {
        tokens.push(&block[s..]);
    }
    tokens
}

fn filter_override_block(
    block: &str,
    drawing_level: &mut u32,
    saw_any_drawing: &mut bool,
    has_animated_geometry: &mut bool,
    is_rotated_or_sign: &mut bool,
    has_explicit_pos: &mut bool,
) -> String {
    let mut out = String::new();
    for token in split_override_tokens(block) {
        let Some(body) = token.strip_prefix('\\') else {
            continue;
        };
        if body.is_empty() {
            continue;
        }

        // Drawing mode toggle: \p<n> (must check after \pos and \pbo).
        if let Some(level) = parse_drawing_level(token) {
            *drawing_level = level;
            out.push_str(token);
            if level > 0 {
                *saw_any_drawing = true;
                // Make vector drawings invisible so they never collide with text color IDs.
                out.push_str(r"\1a&HFF&");
            } else {
                out.push_str(r"\1a&H00&");
            }
            continue;
        }

        // Transform animation: \t(...) (tolerate trailing whitespace before next tag/closing brace)
        let body_trimmed = body.trim_end();
        if body_trimmed.starts_with("t(") && body_trimmed.ends_with(')') {
            let inner = &body_trimmed[2..body_trimmed.len() - 1];
            if let Some(filtered_t) = filter_transform_tag(
                inner,
                has_animated_geometry,
                is_rotated_or_sign,
            ) {
                out.push_str(&filtered_t);
            }
            continue;
        }

        // Style reset: \r or \r<Style>
        if is_reset_tag(body) {
            out.push_str(token);
            out.push_str(PROBE_BASE_TAGS);
            continue;
        }

        // Strip visual-only tags that would interfere with 24-bit color-ID masks.
        if is_stripped_probe_tag(body) {
            continue;
        }

        // Track semantic metadata on kept layout tags.
        if body.starts_with("pos(") {
            *has_explicit_pos = true;
        } else if body.starts_with("move(") {
            *has_explicit_pos = true;
            *has_animated_geometry = true;
        } else if let Some(val) = parse_rotation_or_shear_value(body) {
            if val.abs() > 0.01 {
                *is_rotated_or_sign = true;
            }
        }

        out.push_str(token);
    }
    out
}

fn filter_transform_tag(
    inner: &str,
    has_animated_geometry: &mut bool,
    is_rotated_or_sign: &mut bool,
) -> Option<String> {
    let first_bs = inner.find('\\')?;
    let timing_prefix = &inner[..first_bs];
    let nested_block = &inner[first_bs..];

    let mut kept = String::new();
    for token in split_override_tokens(nested_block) {
        let Some(body) = token.strip_prefix('\\') else {
            continue;
        };
        if body.is_empty() || is_stripped_probe_tag(body) {
            continue;
        }
        if let Some(val) = parse_rotation_or_shear_value(body) {
            if val.abs() > 0.01 {
                *is_rotated_or_sign = true;
            }
        }
        kept.push_str(token);
    }

    if kept.is_empty() {
        None
    } else {
        *has_animated_geometry = true;
        Some(format!(r"\t({timing_prefix}{kept})"))
    }
}

fn parse_drawing_level(token: &str) -> Option<u32> {
    let body = token.strip_prefix('\\')?;
    if !body.starts_with('p') || body.starts_with("pos") || body.starts_with("pbo") {
        return None;
    }
    let rest = body[1..].trim();
    if rest.is_empty() {
        return Some(0);
    }
    let digits_end = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    if digits_end == 0 {
        return None;
    }
    rest[..digits_end].parse().ok()
}

fn is_reset_tag(body: &str) -> bool {
    // Only `\r` exists starting with lowercase `r` in ASS/SSA.
    body.starts_with('r')
}

/// Returns true for ASS tags that only affect paint/color/alpha/border/shadow/blur/fade/karaoke
/// and must be stripped during color-ID hitbox probing.
fn is_stripped_probe_tag(body: &str) -> bool {
    // Longest / multi-char prefixes first.
    const EXACT_PREFIXES: &[&str] = &[
        "xbord", "ybord", "bord", "xshad", "yshad", "shad", "blur", "be", "alpha", "fade(",
        "fad(", "1c", "2c", "3c", "4c", "1a", "2a", "3a", "4a", "kf", "ko", "kt",
    ];
    for prefix in EXACT_PREFIXES {
        if body.starts_with(prefix) {
            return true;
        }
    }

    // Single-letter `\c` (color) must NOT match `\clip`.
    if body.starts_with('c') && !body.starts_with("clip") {
        return true;
    }

    // Single-letter karaoke `\k` / `\K` (followed by digit, sign, whitespace, or end).
    if let Some(rest) = body.strip_prefix('k').or_else(|| body.strip_prefix('K')) {
        if rest.is_empty()
            || rest.starts_with(|c: char| c.is_ascii_digit() || c == '-' || c == '+' || c == ' ')
        {
            return true;
        }
    }

    false
}

fn parse_rotation_or_shear_value(body: &str) -> Option<f64> {
    for prefix in ["frz", "frx", "fry", "fax", "fay", "fr"] {
        if let Some(rest) = body.strip_prefix(prefix) {
            let trimmed = rest.trim();
            if trimmed.is_empty() {
                return Some(0.0);
            }
            let num_end = trimmed
                .find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-' || c == '+'))
                .unwrap_or(trimmed.len());
            if num_end > 0 {
                return trimmed[..num_end].parse().ok();
            }
        }
    }
    None
}

fn is_combining_or_joiner(ch: char) -> bool {
    matches!(
        ch as u32,
        0x0300..=0x036F   // Combining Diacritical Marks
        | 0x200C..=0x200D // ZWNJ / ZWJ
        | 0x3099..=0x309A // Combining Katakana-Hiragana Voiced/Semi-Voiced Sound Mark
        | 0xFE00..=0xFE0F // Variation Selectors
        | 0xE0100..=0xE01EF // Variation Selectors Supplement
    )
}

fn strip_ascii_prefix_ignore_case<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    s.get(..prefix.len())
        .filter(|head| head.eq_ignore_ascii_case(prefix))
        .map(|_| &s[prefix.len()..])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timecode_roundtrip() {
        assert_eq!(parse_ass_timecode_ms("0:00:18.92"), Some(18_920));
        assert_eq!(parse_ass_timecode_ms("1:02:03.45"), Some(3_723_450));
        assert_eq!(parse_ass_timecode_ms("0:00:00.5"), Some(500));
        assert_eq!(parse_ass_timecode_ms("0:00:00.123"), Some(123));
        assert_eq!(format_ass_timecode(18_920), "0:00:18.92");
        assert_eq!(format_ass_timecode(3_723_450), "1:02:03.45");
    }

    #[test]
    fn color_id_encode_decode_roundtrip() {
        for cid in [1u32, 2, 255, 256, 512, 0x123456] {
            let tag = format_color_id_tag(cid);
            let hex = tag.strip_prefix(r"\1c&H").unwrap().strip_suffix('&').unwrap();
            let b = u32::from_str_radix(&hex[0..2], 16).unwrap();
            let g = u32::from_str_radix(&hex[2..4], 16).unwrap();
            let r = u32::from_str_radix(&hex[4..6], 16).unwrap();
            let ass_image_color = (r << 24) | (g << 16) | (b << 8);
            assert_eq!(decode_color_id(ass_image_color), cid);
        }
    }

    #[test]
    fn strips_tags_and_drawings_cleanly() {
        let raw = r"{\pos(480,502)\fscx50\1c&H00FF00&\bord3}🔊『間もなく　鳥白島に{\p1}m 0 0 l 10 10{\p0}\N到着\hいたします』";
        assert_eq!(
            strip_ass_dialogue_text(raw),
            "🔊『間もなく　鳥白島に\n到着 いたします』"
        );
    }

    #[test]
    fn probe_preserves_layout_and_strips_paint_tags() {
        let extradata = "\
[Script Info]
ScriptType: v4.00+
PlayResX: 960
PlayResY: 540
ScaledBorderAndShadow: yes
WrapStyle: 0

[V4+ Styles]
Format: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding
Style: Default,Noto Sans CJK JP,36,&H00FFFFFF,&H000000FF,&H00000000,&H64000000,0,0,0,0,100,100,4,0,1,2,1,2,10,10,10,1
";
        let ass_full = r"Dialogue: 0,0:00:10.00,0:00:15.00,Default,,0,0,0,,{\pos(480,502)\fscx50\bord3\shad2\fad(200,200)\t(0,300,\alpha&H00&\fscx80)\rDefault\frz15}鳥白島, テスト\N二行目";

        let probe = build_probe_script(extradata, ass_full, 12_000);
        assert_eq!(probe.events.len(), 1);
        let ev = &probe.events[0];
        assert_eq!(ev.plain_text, "鳥白島, テスト\n二行目");
        assert!(ev.has_explicit_pos);
        assert!(ev.has_animated_geometry);
        assert!(ev.is_rotated_or_sign);
        assert!(!ev.is_drawing_only);

        // Verify paint tags were stripped while layout tags stayed in probe_ass.
        assert!(probe.probe_ass.contains(r"\pos(480,502)"));
        assert!(probe.probe_ass.contains(r"\fscx50"));
        assert!(probe.probe_ass.contains(r"\t(0,300,\fscx80)"));
        assert!(probe.probe_ass.contains(r"\rDefault\bord0\shad0\3a&HFF&\4a&HFF&\1a&H00&"));
        assert!(!probe.probe_ass.contains(r"\fad("));
        assert!(!probe.probe_ass.contains(r"\bord3"));
        assert!(!probe.probe_ass.contains(r"\shad2"));

        // Verify character UTF-8 & UTF-16 offsets match plain_text exactly.
        for c in &ev.chars {
            let slice = &ev.plain_text[c.byte_offset..];
            assert_eq!(slice.chars().next(), Some(c.ch));
        }
        // Check explicit_line_idx incremented after \N.
        let second_line_char = ev.chars.iter().find(|c| c.ch == '二').unwrap();
        assert_eq!(second_line_char.explicit_line_idx, 1);
    }

    #[test]
    fn parse_custom_events_format_order() {
        let header = "\
[Events]
Format: Marked, Start, End, Style, Actor, MarginL, MarginR, MarginV, Effect, Text
";
        let fmt = parse_events_format(header);
        let d = parse_dialogue_line(
            "Dialogue: Marked=2,0:01:02.03,0:01:05.00,Top,Hero,10,20,30,Banner,Hello, world!",
            &fmt,
        )
        .unwrap();
        assert_eq!(d.layer, 2);
        assert_eq!(d.start_ms, 62_030);
        assert_eq!(d.end_ms, 65_000);
        assert_eq!(d.style, "Top");
        assert_eq!(d.name, "Hero");
        assert_eq!(d.margin_l, 10);
        assert_eq!(d.margin_r, 20);
        assert_eq!(d.margin_v, 30);
        assert_eq!(d.effect, "Banner");
        assert_eq!(d.raw_text, "Hello, world!");
    }

    #[test]
    fn malformed_multibyte_utf8_and_trailing_space_transform_do_not_panic() {
        // Non-ASCII lines straddling "Dialogue:" (9 bytes) and "Format:" (7 bytes).
        let weird_doc = "\
[Events]
A「こんにちは」
Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text
あいうえお
Dialogue: 0,0:00:01.1あ,0:00:05.00,Default,,0,0,0,,{\\t(0,500,\\alpha&H00&) }テスト
";
        assert_eq!(parse_ass_timecode_ms("0:00:01.1あ"), None);
        assert_eq!(strip_ascii_prefix_ignore_case("A「こんにちは」", "Dialogue:"), None);
        assert_eq!(strip_ascii_prefix_ignore_case("あいうえお", "Format:"), None);

        let probe = build_probe_script(weird_doc, "", 2_000);
        assert_eq!(probe.events.len(), 1);
        assert!(!probe.events[0].has_animated_geometry);
        // Trailing-whitespace alpha-only \t(...) must be stripped cleanly in probe_ass.
        assert!(!probe.probe_ass.contains(r"\t("));
        assert!(!probe.probe_ass.contains(r"\alpha"));
    }
}

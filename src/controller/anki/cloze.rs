//! Cloze deletion, subtitle context, and expression furigana formatting helpers.

use memento_anki::note::{format_furigana, FuriganaPair};
use memento_subtitle::SubtitleEntry;
use memento_util::furiganautils::simplify_surface;

use super::context::NoteFieldsContext;

/// Formats a cloze deletion string: `prefix + {{c{index}::body}} + suffix`.
/// If index is 0, defaults to 1.
pub fn format_cloze(prefix: &str, body: &str, suffix: &str, index: usize) -> String {
    let idx = if index == 0 { 1 } else { index };
    format!("{prefix}{{{{c{idx}::{body}}}}}{suffix}")
}

/// Formats a cloze deletion with an optional hint: `prefix + {{c{index}::body::hint}} + suffix`.
pub fn format_cloze_with_hint(
    prefix: &str,
    body: &str,
    suffix: &str,
    hint: &str,
    index: usize,
) -> String {
    let idx = if index == 0 { 1 } else { index };
    if hint.is_empty() {
        format!("{prefix}{{{{c{idx}::{body}}}}}{suffix}")
    } else {
        format!("{prefix}{{{{c{idx}::{body}::{hint}}}}}{suffix}")
    }
}

/// Finds `target` inside `sentence` and produces a cloze deletion string.
/// If `target` is not found, wraps the whole sentence in cloze deletion.
pub fn create_cloze_sentence(sentence: &str, target: &str, index: usize) -> String {
    let idx = if index == 0 { 1 } else { index };
    if target.is_empty() {
        return sentence.to_string();
    }
    if let Some(pos) = sentence.find(target) {
        let end = pos.saturating_add(target.len());
        if let (Some(prefix), Some(body), Some(suffix)) = (
            sentence.get(..pos),
            sentence.get(pos..end),
            sentence.get(end..),
        ) {
            return format_cloze(prefix, body, suffix, idx);
        }
    }
    format!("{{{{c{idx}::{sentence}}}}}")
}

/// Splits a sentence around a target expression into `(prefix, body, suffix)`.
pub fn split_cloze<'a>(sentence: &'a str, target: &'a str) -> (&'a str, &'a str, &'a str) {
    split_cloze_around(sentence, target, None)
}

/// Splits `text` around `target` into `(prefix, body, suffix)`, preferring the
/// occurrence at or closest after `hint_offset` (e.g. the active subtitle cue's
/// byte offset inside a multi-subtitle `{context}`).
pub fn split_cloze_around<'a>(
    text: &'a str,
    target: &'a str,
    hint_offset: Option<usize>,
) -> (&'a str, &'a str, &'a str) {
    if target.is_empty() {
        return (text, "", "");
    }
    let matched_pos = if let Some(raw_offset) = hint_offset {
        let mut offset = raw_offset.min(text.len());
        while offset > 0 && !text.is_char_boundary(offset) {
            offset -= 1;
        }
        let mut enclosing: Option<usize> = None;
        let mut first_after: Option<usize> = None;
        let mut last_before: Option<usize> = None;
        for (pos, _) in text.match_indices(target) {
            let end = pos.saturating_add(target.len());
            if pos <= offset && offset < end {
                enclosing = Some(pos);
                break;
            }
            if pos >= offset {
                if first_after.is_none() {
                    first_after = Some(pos);
                }
            } else {
                last_before = Some(pos);
            }
        }
        enclosing.or(first_after).or(last_before)
    } else {
        text.find(target)
    };
    if let Some(pos) = matched_pos {
        let end = pos.saturating_add(target.len());
        if let (Some(prefix), Some(body), Some(suffix)) = (
            text.get(..pos),
            text.get(pos..end),
            text.get(end..),
        ) {
            return (prefix, body, suffix);
        }
    }
    ("", target, text)
}

/// Computes `cloze_prefix`, `cloze_body`, and `cloze_suffix` for single- or
/// multi-subtitle (`selected_cues.len() > 1`) contexts without touching `ctx.selection`.
pub fn apply_subtitle_cloze(
    ctx: &mut NoteFieldsContext,
    selected_cues: &[String],
    active_pos_in_selected: Option<usize>,
    scanned_surface: &str,
    token_offset_in_sentence: Option<usize>,
) {
    if selected_cues.len() > 1 {
        let full_context = selected_cues.join("\n");
        let cue_index = active_pos_in_selected
            .filter(|&idx| idx < selected_cues.len())
            .or_else(|| {
                (!ctx.sentence.is_empty())
                    .then(|| selected_cues.iter().position(|cue| cue == &ctx.sentence))
                    .flatten()
            });
        let hint_offset = cue_index
            .and_then(|idx| {
                let cue = selected_cues.get(idx)?;
                let cue_start: usize = selected_cues
                    .get(..idx)?
                    .iter()
                    .map(|c| c.len() + 1)
                    .sum();
                if let Some(tok_off) = token_offset_in_sentence {
                    if *cue == ctx.sentence && tok_off <= cue.len() {
                        return Some(cue_start + tok_off);
                    }
                }
                Some(cue_start)
            })
            .or_else(|| {
                (!ctx.sentence.is_empty())
                    .then(|| full_context.find(&ctx.sentence))
                    .flatten()
            });
        let target = if !scanned_surface.is_empty() && full_context.contains(scanned_surface) {
            scanned_surface
        } else {
            ctx.expression.as_str()
        };
        let (prefix, body, suffix) = split_cloze_around(&full_context, target, hint_offset);
        ctx.cloze_prefix = prefix.to_owned();
        ctx.cloze_body = body.to_owned();
        ctx.cloze_suffix = suffix.to_owned();
        ctx.context = full_context;
    } else {
        if let Some(single) = selected_cues.first() {
            ctx.context = single.clone();
        }
        let target = if !scanned_surface.is_empty() && ctx.sentence.contains(scanned_surface) {
            scanned_surface
        } else {
            ctx.expression.as_str()
        };
        let (prefix, body, suffix) =
            split_cloze_around(&ctx.sentence, target, token_offset_in_sentence);
        ctx.cloze_prefix = prefix.to_owned();
        ctx.cloze_body = body.to_owned();
        ctx.cloze_suffix = suffix.to_owned();
    }
}

/// Computes `{context-2}` from secondary subtitle entries overlapping `[start, end]`,
/// falling back to `fallback` (`{subtitle-2}`) when no secondary cue overlaps.
pub fn secondary_context_for_range(
    secondary_entries: &[SubtitleEntry],
    start: f64,
    end: f64,
    fallback: &str,
) -> String {
    if start.is_finite() && end.is_finite() && end > start {
        let lines: Vec<String> = secondary_entries
            .iter()
            .filter(|cue| cue.start < end && cue.end > start)
            .map(|cue| String::from_utf16_lossy(&cue.text))
            .filter(|text| !text.is_empty())
            .collect();
        if !lines.is_empty() {
            return lines.join("\n");
        }
    }
    fallback.to_owned()
}

fn is_pure_kana(text: &str) -> bool {
    !text.is_empty()
        && text.chars().all(|ch| {
            matches!(
                ch,
                '\u{3040}'..='\u{309F}'
                    | '\u{30A0}'..='\u{30FF}'
                    | '\u{31F0}'..='\u{31FF}'
                    | '\u{FF65}'..='\u{FF9F}'
            )
        })
}

/// Formats `{furigana}` (`<ruby>...</ruby>`) and `{furigana-plain}` (`漢字[かんじ]`),
/// simplifying okurigana via `simplify_surface` and returning plain kana when there is no kanji.
pub fn format_expression_furigana(expression: &str, reading: &str) -> (String, String) {
    if reading.is_empty() || reading == expression || is_pure_kana(expression) {
        return (expression.to_owned(), expression.to_owned());
    }
    let surface: Vec<u16> = expression.encode_utf16().collect();
    let pronunciation: Vec<u16> = reading.encode_utf16().collect();
    let spans = simplify_surface(&surface, &pronunciation);
    let pairs: Vec<FuriganaPair<'_>> = spans
        .iter()
        .map(|span| FuriganaPair {
            surface: surface
                .get(span.surface_start as usize..span.surface_end as usize)
                .unwrap_or(&[]),
            reading: pronunciation
                .get(span.reading_start as usize..span.reading_end as usize)
                .unwrap_or(&[]),
        })
        .collect();
    (
        String::from_utf16_lossy(&format_furigana(&pairs, false)),
        String::from_utf16_lossy(&format_furigana(&pairs, true)),
    )
}

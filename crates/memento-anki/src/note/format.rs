//! Furigana, tags, kanji, and term glossary HTML formatting.

use std::collections::BTreeSet;

use crate::glossary::escape::{
    append_escaped, append_escaped_str, append_escaped_style_block, ascii_to_u16, escape_html_str,
    push_ascii,
};
use crate::strings::qt_is_space;

/// Borrowed furigana pair (surface and optional reading).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FuriganaPair<'a> {
    pub surface: &'a [u16],
    pub reading: &'a [u16],
}

/// Matches `getFurigana` and `getFuriganaPlain` pair formatting in `notebuilder.cpp`.
pub fn format_furigana(pairs: &[FuriganaPair<'_>], plain: bool) -> Vec<u16> {
    let cap: usize = pairs
        .iter()
        .map(|p| p.surface.len() + p.reading.len() + if plain { 2 } else { 22 })
        .sum();
    let mut result = Vec::with_capacity(cap);
    for pair in pairs {
        if pair.reading.is_empty() {
            if plain {
                result.extend_from_slice(pair.surface);
            } else {
                append_escaped(pair.surface, &mut result, false);
            }
        } else if plain {
            result.extend_from_slice(pair.surface);
            result.push(b'[' as u16);
            result.extend_from_slice(pair.reading);
            result.push(b']' as u16);
        } else {
            push_ascii(&mut result, "<ruby>");
            append_escaped(pair.surface, &mut result, false);
            push_ascii(&mut result, "<rt>");
            append_escaped(pair.reading, &mut result, false);
            push_ascii(&mut result, "</rt></ruby>");
        }
    }
    result
}

/// Borrowed tag entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TagEntry<'a> {
    pub name: &'a [u16],
    pub notes: &'a [u16],
}

/// Matches `getTags` and `getTagsBrief` in `notebuilder.cpp`, HTML-escaping
/// tag names and notes.
pub fn format_tags(tags: &[TagEntry<'_>], brief: bool) -> Vec<u16> {
    if tags.is_empty() {
        return Vec::new();
    }
    let cap = 9
        + tags
            .iter()
            .map(|t| 11 + t.name.len() + if brief { 0 } else { t.notes.len() })
            .sum::<usize>();
    let mut result = Vec::with_capacity(cap);
    push_ascii(&mut result, "<ul>");
    for tag in tags {
        push_ascii(&mut result, "<li>");
        append_escaped(tag.name, &mut result, false);
        if !brief && !tag.notes.is_empty() {
            push_ascii(&mut result, ": ");
            append_escaped(tag.notes, &mut result, false);
        }
        push_ascii(&mut result, "</li>");
    }
    push_ascii(&mut result, "</ul>");
    result
}

/// Matches `getKunyomi` / `getOnyomi` in `notebuilder.cpp`.
pub fn format_kanji_readings(readings: &[&[u16]]) -> Vec<u16> {
    if readings.is_empty() {
        return Vec::new();
    }
    let cap = readings.iter().map(|r| r.len()).sum::<usize>() + (readings.len() - 1) * 2;
    let mut result = Vec::with_capacity(cap);
    for (i, &reading) in readings.iter().enumerate() {
        if i > 0 {
            push_ascii(&mut result, ", ");
        }
        result.extend_from_slice(reading);
    }
    result
}

/// Borrowed kanji definition entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KanjiDefEntry<'a> {
    pub dictionary_name: &'a [u16],
    pub tags: &'a [&'a [u16]],
    pub glossary: &'a [&'a [u16]],
}

/// Matches `getGlossary(const QList<KanjiDefinition *> &)` in `notebuilder.cpp`,
/// HTML-escaping dictionary names, tags, and plain kanji glossary lines.
pub fn format_kanji_glossary(defs: &[KanjiDefEntry<'_>]) -> Vec<u16> {
    if !defs.iter().any(|def| !def.glossary.is_empty()) {
        return Vec::new();
    }

    let mut glossary = Vec::with_capacity(64);
    push_ascii(&mut glossary, "<ol>");
    for def in defs {
        if def.glossary.is_empty() {
            continue;
        }
        push_ascii(&mut glossary, "<li><i>(");
        for tag in def.tags {
            append_escaped(tag, &mut glossary, false);
            push_ascii(&mut glossary, ", ");
        }
        append_escaped(def.dictionary_name, &mut glossary, false);
        push_ascii(&mut glossary, ")</i></li><ol>");
        for glos in def.glossary {
            push_ascii(&mut glossary, "<li>");
            append_escaped(glos, &mut glossary, false);
            push_ascii(&mut glossary, "</li>");
        }
        push_ascii(&mut glossary, "</ol>");
    }
    push_ascii(&mut glossary, "</ol>");
    glossary
}

/// Borrowed stat tag entry for stroke count lookup.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StrokeCountStat<'a> {
    pub notes: &'a [u16],
    pub value: &'a [u16],
}

/// Matches `getStrokeCount` in `notebuilder.cpp`.
pub fn find_stroke_count<'a>(stats: &[StrokeCountStat<'a>]) -> Option<&'a [u16]> {
    const STROKE_COUNT: [u16; 12] = ascii_to_u16("Stroke count");
    stats
        .iter()
        .find(|stat| stat.notes == STROKE_COUNT.as_slice())
        .map(|stat| stat.value)
}

/// Borrowed definition entry for term glossary formatting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TermGlossaryDef<'a> {
    pub dict_id: i64,
    pub dict_name: &'a [u16],
    pub tags: &'a [&'a [u16]],
    pub items: &'a [&'a [u16]],
    pub has_styles: bool,
    pub stylesheet: &'a [u16],
}

/// Output HTML for term glossary markers.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TermGlossaryOutput {
    pub glossary: Vec<u16>,
    pub glossary_brief: Vec<u16>,
    pub glossary_compact: Vec<u16>,
}

/// Individual HTML variant produced for a term glossary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TermGlossaryVariant {
    Full,
    Brief,
    Compact,
}

/// Selection of term glossary HTML variants to render. Unselected variants are
/// skipped completely without allocating or formatting their HTML.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TermGlossaryVariants {
    pub full: bool,
    pub brief: bool,
    pub compact: bool,
}

impl TermGlossaryVariants {
    pub const ALL: Self = Self {
        full: true,
        brief: true,
        compact: true,
    };
    pub const FULL: Self = Self {
        full: true,
        brief: false,
        compact: false,
    };
    pub const BRIEF: Self = Self {
        full: false,
        brief: true,
        compact: false,
    };
    pub const COMPACT: Self = Self {
        full: false,
        brief: false,
        compact: true,
    };
    pub const NONE: Self = Self {
        full: false,
        brief: false,
        compact: false,
    };

    pub const fn is_empty(self) -> bool {
        !self.full && !self.brief && !self.compact
    }
}

const GLOSSARY_OPEN_HTML: &str = "<div class=\"yomitan-glossary memento-glossary\"><ol>";
const GLOSSARY_CLOSE_HTML: &str = "<style>.memento-glossary {text-align: left;}.memento-glossary img {display: inline-block;}.memento-glossary table {table-layout: auto;border-collapse: collapse;}.memento-glossary th {font-weight: bold;border-style: solid;padding: 0.25em;vertical-align: top;border-width: 1px;border-color: currentColor;}.memento-glossary td {border-style: solid;padding: 0.25em;vertical-align: top;border-width: 1px;border-color: currentColor;}</style></ol></div>";

fn append_term_header(def: &TermGlossaryDef<'_>, target: &mut Vec<u16>) {
    push_ascii(target, "<li data-dictionary=\"");
    append_escaped(def.dict_name, target, false);
    push_ascii(target, "\"><i>(");
    for tag in def.tags {
        append_escaped(tag, target, false);
        push_ascii(target, ", ");
    }
    append_escaped(def.dict_name, target, false);
    push_ascii(target, ")</i> <span>");
}

/// Builds only the selected term glossary HTML variants, skipping allocation
/// and formatting for unselected variants. HTML-escapes dictionary names and
/// tags, and escapes `</style` in embedded dictionary `<style>` blocks.
pub fn format_term_glossary_selected(
    defs: &[TermGlossaryDef<'_>],
    variants: TermGlossaryVariants,
) -> TermGlossaryOutput {
    if defs.is_empty() || variants.is_empty() {
        return TermGlossaryOutput::default();
    }

    let mut glossary = Vec::new();
    let mut glossary_brief = Vec::new();
    let mut glossary_compact = Vec::new();

    if variants.full {
        push_ascii(&mut glossary, GLOSSARY_OPEN_HTML);
    }
    if variants.brief {
        push_ascii(&mut glossary_brief, GLOSSARY_OPEN_HTML);
    }
    if variants.compact {
        push_ascii(&mut glossary_compact, GLOSSARY_OPEN_HTML);
    }

    let mut dictionaries_with_styles = BTreeSet::new();
    let mut shared_header = Vec::new();
    let mut shared_style = Vec::new();

    for def in defs {
        if variants.full && variants.compact {
            shared_header.clear();
            append_term_header(def, &mut shared_header);
            glossary.extend_from_slice(&shared_header);
            glossary_compact.extend_from_slice(&shared_header);
        } else if variants.full {
            append_term_header(def, &mut glossary);
        } else if variants.compact {
            append_term_header(def, &mut glossary_compact);
        }

        for (i, item) in def.items.iter().enumerate() {
            if variants.full {
                glossary.extend_from_slice(item);
            }
            if variants.brief {
                push_ascii(&mut glossary_brief, "<li>");
                glossary_brief.extend_from_slice(item);
                push_ascii(&mut glossary_brief, "</li>");
            }
            if variants.compact {
                if i > 0 {
                    push_ascii(&mut glossary_compact, " | ");
                }
                glossary_compact.extend_from_slice(item);
            }
        }

        if variants.full {
            push_ascii(&mut glossary, "</span></li>");
        }
        if variants.compact {
            push_ascii(&mut glossary_compact, "</span></li>");
        }

        if def.has_styles
            && def.stylesheet.iter().any(|&u| !qt_is_space(u))
            && dictionaries_with_styles.insert(def.dict_id)
        {
            shared_style.clear();
            push_ascii(&mut shared_style, "<style>");
            append_escaped_style_block(def.stylesheet, &mut shared_style);
            push_ascii(&mut shared_style, "</style>");
            if variants.full {
                glossary.extend_from_slice(&shared_style);
            }
            if variants.brief {
                glossary_brief.extend_from_slice(&shared_style);
            }
            if variants.compact {
                glossary_compact.extend_from_slice(&shared_style);
            }
        }
    }

    if variants.full {
        push_ascii(&mut glossary, GLOSSARY_CLOSE_HTML);
    }
    if variants.brief {
        push_ascii(&mut glossary_brief, GLOSSARY_CLOSE_HTML);
    }
    if variants.compact {
        push_ascii(&mut glossary_compact, GLOSSARY_CLOSE_HTML);
    }

    TermGlossaryOutput {
        glossary,
        glossary_brief,
        glossary_compact,
    }
}

/// Normalizes a single glossary selection string into safe HTML:
/// - Normalizes `\r\n` and standalone `\r` line endings into `\n`.
/// - Drops leading/trailing/consecutive empty or whitespace-only lines so no
///   double `<br><br>` or trailing `<br>` is ever emitted (Yomitan `#2248`).
/// - HTML-escapes `<`, `>`, `&`, and `"` in each surviving line (Yomitan PR
///   `#2492` / `#2394`) and joins lines with a single `<br>`.
fn format_single_selection_item(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let normalized = trimmed.replace("\r\n", "\n").replace('\r', "\n");
    let mut out = String::with_capacity(normalized.len());
    let mut emitted_line = false;
    for line in normalized.split('\n') {
        if line.trim().is_empty() {
            continue;
        }
        if emitted_line {
            out.push_str("<br>");
        }
        append_escaped_str(line.trim_end(), &mut out);
        emitted_line = true;
    }
    emitted_line.then_some(out)
}

/// Formats one or more selected glossary definitions for the `{selection}`
/// marker (`#262`, Yomitan `#2248`):
/// - Blank/whitespace-only entries are ignored.
/// - A single surviving selection is emitted cleanly without a `<ul>` wrapper.
/// - Two or more selections are combined into `<ul><li>...</li></ul>`.
/// - Line endings (`\r\n`, `\r`, consecutive blank lines, trailing newlines)
///   are normalized so NO double `<br><br>` is produced, and HTML special
///   characters (`<`, `>`, `&`, `"`) are escaped.
pub fn format_selection(selections: &[&str]) -> String {
    let items: Vec<String> = selections
        .iter()
        .filter_map(|raw| format_single_selection_item(raw))
        .collect();
    match items.len() {
        0 => String::new(),
        1 => items.into_iter().next().unwrap(),
        _ => {
            let cap: usize = 9 + items.iter().map(|s| s.len() + 9).sum::<usize>();
            let mut out = String::with_capacity(cap);
            out.push_str("<ul>");
            for item in items {
                out.push_str("<li>");
                out.push_str(&item);
                out.push_str("</li>");
            }
            out.push_str("</ul>");
            out
        }
    }
}

/// UTF-16 convenience wrapper around [`format_selection`].
pub fn format_selection_u16(selections: &[&[u16]]) -> Vec<u16> {
    let owned: Vec<String> = selections
        .iter()
        .map(|units| String::from_utf16_lossy(units))
        .collect();
    let refs: Vec<&str> = owned.iter().map(String::as_str).collect();
    format_selection(&refs).encode_utf16().collect()
}

/// HTML-escapes `<`, `>`, `&`, and `"` across `(cloze-prefix, cloze-body,
/// cloze-suffix)` (Yomitan PR #2492 / #2394).
pub fn format_cloze_parts_escaped(
    prefix: &str,
    body: &str,
    suffix: &str,
) -> (String, String, String) {
    (
        escape_html_str(prefix),
        escape_html_str(body),
        escape_html_str(suffix),
    )
}

/// Formats an HTML-escaped Anki cloze deletion:
/// `escaped(prefix) + {{c{index}::escaped(body)}} + escaped(suffix)`.
/// Defaults `index == 0` to `1`.
pub fn format_cloze_escaped(prefix: &str, body: &str, suffix: &str, index: usize) -> String {
    let idx = if index == 0 { 1 } else { index };
    let (p, b, s) = format_cloze_parts_escaped(prefix, body, suffix);
    format!("{p}{{{{c{idx}::{b}}}}}{s}")
}

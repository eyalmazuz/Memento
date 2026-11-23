//! Furigana, tags, kanji, selection, cloze, and term glossary HTML formatting.

use std::collections::BTreeSet;

use crate::glossary::escape::{append_escaped_str, append_escaped_style_block, escape_html_str};

/// Borrowed furigana pair (surface and optional reading).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FuriganaPair<'a> {
    pub surface: &'a str,
    pub reading: &'a str,
}

/// Formats furigana pairs either as HTML `<ruby>` tags or plain `漢字[かんじ]` notation.
pub fn format_furigana(pairs: &[FuriganaPair<'_>], plain: bool) -> String {
    let cap: usize = pairs
        .iter()
        .map(|p| p.surface.len() + p.reading.len() + if plain { 2 } else { 22 })
        .sum();
    let mut result = String::with_capacity(cap);
    for pair in pairs {
        if pair.reading.is_empty() {
            if plain {
                result.push_str(pair.surface);
            } else {
                append_escaped_str(pair.surface, &mut result);
            }
        } else if plain {
            result.push_str(pair.surface);
            result.push('[');
            result.push_str(pair.reading);
            result.push(']');
        } else {
            result.push_str("<ruby>");
            append_escaped_str(pair.surface, &mut result);
            result.push_str("<rt>");
            append_escaped_str(pair.reading, &mut result);
            result.push_str("</rt></ruby>");
        }
    }
    result
}

/// Borrowed tag entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TagEntry<'a> {
    pub name: &'a str,
    pub notes: &'a str,
}

/// Formats tags as an HTML `<ul>`, HTML-escaping tag names and optional notes.
pub fn format_tags(tags: &[TagEntry<'_>], brief: bool) -> String {
    if tags.is_empty() {
        return String::new();
    }
    let cap = 9
        + tags
            .iter()
            .map(|t| 11 + t.name.len() + if brief { 0 } else { t.notes.len() })
            .sum::<usize>();
    let mut result = String::with_capacity(cap);
    result.push_str("<ul>");
    for tag in tags {
        result.push_str("<li>");
        append_escaped_str(tag.name, &mut result);
        if !brief && !tag.notes.is_empty() {
            result.push_str(": ");
            append_escaped_str(tag.notes, &mut result);
        }
        result.push_str("</li>");
    }
    result.push_str("</ul>");
    result
}

/// Joins kanji onyomi/kunyomi readings with `", "`.
pub fn format_kanji_readings(readings: &[&str]) -> String {
    readings.join(", ")
}

/// Borrowed kanji definition entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KanjiDefEntry<'a> {
    pub dictionary_name: &'a str,
    pub tags: &'a [&'a str],
    pub glossary: &'a [&'a str],
}

/// Formats kanji definitions as nested `<ol>` HTML, escaping dictionary names,
/// tags, and plain kanji glossary lines.
pub fn format_kanji_glossary(defs: &[KanjiDefEntry<'_>]) -> String {
    if !defs.iter().any(|def| !def.glossary.is_empty()) {
        return String::new();
    }

    let mut glossary = String::with_capacity(64);
    glossary.push_str("<ol>");
    for def in defs {
        if def.glossary.is_empty() {
            continue;
        }
        glossary.push_str("<li><i>(");
        for tag in def.tags {
            append_escaped_str(tag, &mut glossary);
            glossary.push_str(", ");
        }
        append_escaped_str(def.dictionary_name, &mut glossary);
        glossary.push_str(")</i></li><ol>");
        for glos in def.glossary {
            glossary.push_str("<li>");
            append_escaped_str(glos, &mut glossary);
            glossary.push_str("</li>");
        }
        glossary.push_str("</ol>");
    }
    glossary.push_str("</ol>");
    glossary
}

/// Borrowed stat tag entry for stroke count lookup.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StrokeCountStat<'a> {
    pub notes: &'a str,
    pub value: &'a str,
}

/// Finds the first stat entry whose note is `"Stroke count"`.
pub fn find_stroke_count<'a>(stats: &[StrokeCountStat<'a>]) -> Option<&'a str> {
    stats
        .iter()
        .find(|stat| stat.notes == "Stroke count")
        .map(|stat| stat.value)
}

/// Borrowed definition entry for term glossary formatting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TermGlossaryDef<'a> {
    pub dict_id: i64,
    pub dict_name: &'a str,
    pub tags: &'a [&'a str],
    pub items: &'a [&'a str],
    pub has_styles: bool,
    pub stylesheet: &'a str,
}

/// Output HTML for term glossary markers.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TermGlossaryOutput {
    pub glossary: String,
    pub glossary_brief: String,
    pub glossary_compact: String,
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

const GLOSSARY_OPEN_HTML: &str = "<div class=\"yomitan-glossary subomi-glossary\"><ol>";
const GLOSSARY_CLOSE_HTML: &str = "<style>.subomi-glossary {text-align: left;}.subomi-glossary img {display: inline-block;}.subomi-glossary table {table-layout: auto;border-collapse: collapse;}.subomi-glossary th {font-weight: bold;border-style: solid;padding: 0.25em;vertical-align: top;border-width: 1px;border-color: currentColor;}.subomi-glossary td {border-style: solid;padding: 0.25em;vertical-align: top;border-width: 1px;border-color: currentColor;}</style></ol></div>";

fn append_term_header(def: &TermGlossaryDef<'_>, target: &mut String) {
    target.push_str("<li data-dictionary=\"");
    append_escaped_str(def.dict_name, target);
    target.push_str("\"><i>(");
    for tag in def.tags {
        append_escaped_str(tag, target);
        target.push_str(", ");
    }
    append_escaped_str(def.dict_name, target);
    target.push_str(")</i> <span>");
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

    let mut glossary = String::new();
    let mut glossary_brief = String::new();
    let mut glossary_compact = String::new();

    if variants.full {
        glossary.push_str(GLOSSARY_OPEN_HTML);
    }
    if variants.brief {
        glossary_brief.push_str(GLOSSARY_OPEN_HTML);
    }
    if variants.compact {
        glossary_compact.push_str(GLOSSARY_OPEN_HTML);
    }

    let mut dictionaries_with_styles = BTreeSet::new();
    let mut shared_header = String::new();
    let mut shared_style = String::new();

    for def in defs {
        if variants.full && variants.compact {
            shared_header.clear();
            append_term_header(def, &mut shared_header);
            glossary.push_str(&shared_header);
            glossary_compact.push_str(&shared_header);
        } else if variants.full {
            append_term_header(def, &mut glossary);
        } else if variants.compact {
            append_term_header(def, &mut glossary_compact);
        }

        for (i, item) in def.items.iter().enumerate() {
            if variants.full {
                glossary.push_str(item);
            }
            if variants.brief {
                glossary_brief.push_str("<li>");
                glossary_brief.push_str(item);
                glossary_brief.push_str("</li>");
            }
            if variants.compact {
                if i > 0 {
                    glossary_compact.push_str(" | ");
                }
                glossary_compact.push_str(item);
            }
        }

        if variants.full {
            glossary.push_str("</span></li>");
        }
        if variants.compact {
            glossary_compact.push_str("</span></li>");
        }

        if def.has_styles
            && !def.stylesheet.trim().is_empty()
            && dictionaries_with_styles.insert(def.dict_id)
        {
            shared_style.clear();
            shared_style.push_str("<style>");
            append_escaped_style_block(def.stylesheet, &mut shared_style);
            shared_style.push_str("</style>");
            if variants.full {
                glossary.push_str(&shared_style);
            }
            if variants.brief {
                glossary_brief.push_str(&shared_style);
            }
            if variants.compact {
                glossary_compact.push_str(&shared_style);
            }
        }
    }

    if variants.full {
        glossary.push_str(GLOSSARY_CLOSE_HTML);
    }
    if variants.brief {
        glossary_brief.push_str(GLOSSARY_CLOSE_HTML);
    }
    if variants.compact {
        glossary_compact.push_str(GLOSSARY_CLOSE_HTML);
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
///   double `<br><br>` or trailing `<br>` is ever emitted.
/// - HTML-escapes `<`, `>`, `&`, and `"` in each surviving line and joins lines
///   with a single `<br>`.
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

/// Formats one or more selected glossary definitions for the `{selection}` marker:
/// - Blank/whitespace-only entries are ignored.
/// - A single surviving selection is emitted cleanly without a `<ul>` wrapper.
/// - Two or more selections are combined into `<ul><li>...</li></ul>`.
/// - Line endings are normalized without double `<br><br>` and HTML metacharacters
///   are escaped.
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

/// HTML-escapes `<`, `>`, `&`, and `"` across `(cloze-prefix, cloze-body, cloze-suffix)`.
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

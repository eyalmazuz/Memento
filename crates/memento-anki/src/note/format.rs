//! Furigana, tags, kanji, term glossary, and audio URL/filename formatting.

use std::collections::BTreeSet;

use crate::glossary::escape::{
    append_escaped, append_escaped_style_block, ascii_to_u16, push_ascii,
};
use crate::note::metadata::replace_all;
use crate::strings::qt_is_space;

const HEX_UPPER: [u16; 16] = [
    b'0' as u16,
    b'1' as u16,
    b'2' as u16,
    b'3' as u16,
    b'4' as u16,
    b'5' as u16,
    b'6' as u16,
    b'7' as u16,
    b'8' as u16,
    b'9' as u16,
    b'A' as u16,
    b'B' as u16,
    b'C' as u16,
    b'D' as u16,
    b'E' as u16,
    b'F' as u16,
];

/// Borrowed furigana pair (surface and optional reading).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FuriganaPair<'a> {
    pub surface: &'a [u16],
    pub reading: &'a [u16],
}

/// Matches `getFurigana` and `getFuriganaPlain` pair formatting in `notebuilder.cpp`.
pub fn format_furigana(pairs: &[FuriganaPair<'_>], plain: bool) -> Vec<u16> {
    let mut result = Vec::new();
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

/// Matches `getSelection` in `notebuilder.cpp`.
pub fn format_selection(selections: &[&[u16]]) -> Vec<u16> {
    if selections.is_empty() {
        return Vec::new();
    }
    if selections.len() == 1 {
        return selections[0].to_vec();
    }
    let mut result = Vec::new();
    push_ascii(&mut result, "<ul>");
    for &item in selections {
        push_ascii(&mut result, "<li>");
        result.extend_from_slice(item);
        push_ascii(&mut result, "</li>");
    }
    push_ascii(&mut result, "</ul>");
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
    let mut result = Vec::new();
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
    let mut result = Vec::new();
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

    let mut glossary = Vec::new();
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

/// Percent-encodes UTF-16 text (via UTF-8 bytes) keeping only RFC 3986
/// unreserved characters (`A-Z`, `a-z`, `0-9`, `-`, `.`, `_`, `~`).
fn percent_encode_url_component(input: &[u16]) -> Vec<u16> {
    let mut out = Vec::with_capacity(input.len() * 3);
    let mut utf8_buf = [0u8; 4];
    for decoded in char::decode_utf16(input.iter().copied()) {
        let ch = decoded.unwrap_or(char::REPLACEMENT_CHARACTER);
        for &byte in ch.encode_utf8(&mut utf8_buf).as_bytes() {
            if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
                out.push(byte as u16);
            } else {
                out.push(b'%' as u16);
                out.push(HEX_UPPER[(byte >> 4) as usize]);
                out.push(HEX_UPPER[(byte & 0x0f) as usize]);
            }
        }
    }
    out
}

/// Appends a sanitized filename component: strips NUL/control chars and lone
/// surrogates, replaces whitespace and filesystem/Anki/HTML metacharacters
/// with `_`, and neutralizes `..` runs.
fn append_sanitized_filename_part(input: &[u16], out: &mut Vec<u16>) {
    let mut utf16_buf = [0u16; 2];
    for decoded in char::decode_utf16(input.iter().copied()) {
        let Ok(ch) = decoded else {
            continue;
        };
        let cp = ch as u32;
        if cp < 0x20 || cp == 0x7f {
            continue;
        }
        if cp <= 0xffff && qt_is_space(cp as u16) {
            out.push(b'_' as u16);
            continue;
        }
        if matches!(
            ch,
            '/' | '\\'
                | ':'
                | '*'
                | '?'
                | '"'
                | '\''
                | '<'
                | '>'
                | '|'
                | '['
                | ']'
                | '&'
                | '#'
                | '%'
        ) {
            out.push(b'_' as u16);
            continue;
        }
        if ch == '.' && out.last() == Some(&(b'.' as u16)) {
            out.pop();
            out.push(b'_' as u16);
            out.push(b'_' as u16);
            continue;
        }
        out.extend_from_slice(ch.encode_utf16(&mut utf16_buf));
    }
}

/// Matches `createAudio` URL and filename formatting in `notebuilder.cpp`,
/// percent-encoding URL template substitutions and sanitizing the generated
/// Anki media filename.
pub fn format_audio_url_and_filename(
    url: &[u16],
    source_name: &[u16],
    expression: &[u16],
    reading: &[u16],
    reading_as_expression: bool,
) -> (Vec<u16>, Vec<u16>) {
    let expression_val = if reading_as_expression {
        reading
    } else {
        expression
    };
    let reading_val = if reading.is_empty() || reading_as_expression {
        expression_val
    } else {
        reading
    };

    const REPLACE_EXPRESSION: [u16; 12] = ascii_to_u16("{expression}");
    const REPLACE_TERM: [u16; 6] = ascii_to_u16("{term}");
    const REPLACE_READING: [u16; 9] = ascii_to_u16("{reading}");

    let encoded_expression = percent_encode_url_component(expression_val);
    let encoded_reading = percent_encode_url_component(reading_val);

    let mut formatted_url = url.to_vec();
    replace_all(&mut formatted_url, &REPLACE_EXPRESSION, &encoded_expression);
    replace_all(&mut formatted_url, &REPLACE_TERM, &encoded_expression);
    replace_all(&mut formatted_url, &REPLACE_READING, &encoded_reading);

    let mut filename =
        Vec::with_capacity(14 + source_name.len() + reading.len() + expression.len());
    push_ascii(&mut filename, "memento_");
    append_sanitized_filename_part(source_name, &mut filename);
    filename.push(b'_' as u16);
    append_sanitized_filename_part(reading, &mut filename);
    filename.push(b'_' as u16);
    append_sanitized_filename_part(expression, &mut filename);
    while filename.last() == Some(&(b'.' as u16)) {
        filename.pop();
    }
    push_ascii(&mut filename, ".mp3");

    (formatted_url, filename)
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

        if def.has_styles && dictionaries_with_styles.insert(def.dict_id) {
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

/// Builds a single requested term glossary HTML variant without building the
/// other two variants.
pub fn format_term_glossary_variant(
    defs: &[TermGlossaryDef<'_>],
    variant: TermGlossaryVariant,
) -> Vec<u16> {
    match variant {
        TermGlossaryVariant::Full => {
            format_term_glossary_selected(defs, TermGlossaryVariants::FULL).glossary
        }
        TermGlossaryVariant::Brief => {
            format_term_glossary_selected(defs, TermGlossaryVariants::BRIEF).glossary_brief
        }
        TermGlossaryVariant::Compact => {
            format_term_glossary_selected(defs, TermGlossaryVariants::COMPACT).glossary_compact
        }
    }
}

/// Matches `getGlossary(const QList<TermDefinition *> &)` in `notebuilder.cpp`.
pub fn format_term_glossary(defs: &[TermGlossaryDef<'_>]) -> TermGlossaryOutput {
    format_term_glossary_selected(defs, TermGlossaryVariants::ALL)
}

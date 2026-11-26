//! Glossary HTML rendering (with selective variant building) and scoped dictionary CSS.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use subomi_anki::note::{
    format_kanji_glossary, format_term_glossary_selected, KanjiDefEntry, TermGlossaryDef,
    TermGlossaryVariant, TermGlossaryVariants,
};

use super::context::{GlossaryDefinition, NoteFieldsContext};

const MAX_SCOPED_CSS_CACHE_ENTRIES: usize = 256;

#[derive(Clone, Debug)]
struct CachedScopedCss {
    dict_name: String,
    raw_len: usize,
    scoped: String,
}

static SCOPED_CSS_CACHE: OnceLock<Mutex<HashMap<(i64, u64), CachedScopedCss>>> = OnceLock::new();

fn scoped_css_cache() -> &'static Mutex<HashMap<(i64, u64), CachedScopedCss>> {
    SCOPED_CSS_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn fnv1a_64(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// Rendered glossary output in full, brief, and compact styles.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GlossaryOutput {
    pub glossary: String,
    pub glossary_brief: String,
    pub glossary_compact: String,
}

impl GlossaryOutput {
    pub(crate) fn variant(&self, variant: TermGlossaryVariant) -> &str {
        match variant {
            TermGlossaryVariant::Full => &self.glossary,
            TermGlossaryVariant::Brief => &self.glossary_brief,
            TermGlossaryVariant::Compact => &self.glossary_compact,
        }
    }
}

fn escape_html(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn escape_css_string(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\a "),
            '\r' => {}
            _ => out.push(ch),
        }
    }
    out
}

fn strip_css_comments(css: &str) -> String {
    let bytes = css.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    let mut in_quote: Option<u8> = None;
    while i < bytes.len() {
        if let Some(q) = in_quote {
            if bytes[i] == b'\\' && i + 1 < bytes.len() && bytes[i + 1] < 0x80 {
                out.push(bytes[i]);
                out.push(bytes[i + 1]);
                i += 2;
                continue;
            }
            if bytes[i] == q {
                in_quote = None;
            }
            out.push(bytes[i]);
            i += 1;
        } else if bytes[i] == b'"' || bytes[i] == b'\'' {
            in_quote = Some(bytes[i]);
            out.push(bytes[i]);
            i += 1;
        } else if i + 1 < bytes.len() && bytes[i] == b'/' && bytes[i + 1] == b'*' {
            i += 2;
            while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                i += 1;
            }
            i = (i + 2).min(bytes.len());
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn find_matching_css_brace(css: &str, open_idx: usize) -> Option<usize> {
    let bytes = css.as_bytes();
    let mut depth = 0usize;
    let mut i = open_idx;
    let mut in_quote: Option<u8> = None;
    while i < bytes.len() {
        if let Some(q) = in_quote {
            if bytes[i] == b'\\' && i + 1 < bytes.len() && bytes[i + 1] < 0x80 {
                i += 2;
                continue;
            }
            if bytes[i] == q {
                in_quote = None;
            }
        } else {
            match bytes[i] {
                b'"' | b'\'' => in_quote = Some(bytes[i]),
                b'{' => depth += 1,
                b'}' => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        return Some(i);
                    }
                }
                _ => {}
            }
        }
        i += 1;
    }
    None
}

fn split_top_level_selectors(selector_group: &str) -> Vec<&str> {
    let bytes = selector_group.as_bytes();
    let mut parts = Vec::new();
    let mut start = 0usize;
    let mut paren_depth = 0usize;
    let mut bracket_depth = 0usize;
    let mut in_quote: Option<u8> = None;
    let mut i = 0usize;
    while i < bytes.len() {
        if let Some(q) = in_quote {
            if bytes[i] == b'\\' && i + 1 < bytes.len() && bytes[i + 1] < 0x80 {
                i += 2;
                continue;
            }
            if bytes[i] == q {
                in_quote = None;
            }
        } else {
            match bytes[i] {
                b'"' | b'\'' => in_quote = Some(bytes[i]),
                b'(' => paren_depth += 1,
                b')' => paren_depth = paren_depth.saturating_sub(1),
                b'[' => bracket_depth += 1,
                b']' => bracket_depth = bracket_depth.saturating_sub(1),
                b',' if paren_depth == 0 && bracket_depth == 0 => {
                    if let Some(part) = selector_group.get(start..i) {
                        parts.push(part);
                    }
                    start = i + 1;
                }
                _ => {}
            }
        }
        i += 1;
    }
    if let Some(tail) = selector_group.get(start..) {
        parts.push(tail);
    }
    parts
}

fn scope_css_rules(css: &str, scope_selector: &str) -> String {
    let bytes = css.as_bytes();
    let mut out = String::with_capacity(css.len() + 64);
    let mut i = 0usize;
    while i < bytes.len() {
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        if bytes[i] == b'@' {
            // Find next `;` or `{` outside quotes.
            let mut j = i;
            let mut in_quote: Option<u8> = None;
            while j < bytes.len() {
                if let Some(q) = in_quote {
                    if bytes[j] == b'\\' && j + 1 < bytes.len() && bytes[j + 1] < 0x80 {
                        j += 2;
                        continue;
                    }
                    if bytes[j] == q {
                        in_quote = None;
                    }
                } else if bytes[j] == b'"' || bytes[j] == b'\'' {
                    in_quote = Some(bytes[j]);
                } else if bytes[j] == b';' || bytes[j] == b'{' {
                    break;
                }
                j += 1;
            }
            if j >= bytes.len() {
                break;
            }
            let header = css.get(i..j).unwrap_or("").trim();
            if bytes[j] == b';' {
                // Skip @import and @charset in embedded <style> blocks.
                i = j + 1;
                continue;
            }
            let Some(close) = find_matching_css_brace(css, j) else {
                break;
            };
            let lower = header.to_ascii_lowercase();
            let inner = css.get(j + 1..close).unwrap_or("");
            if lower.starts_with("@media")
                || lower.starts_with("@supports")
                || lower.starts_with("@container")
            {
                let scoped_inner = scope_css_rules(inner, scope_selector);
                if !scoped_inner.trim().is_empty() {
                    out.push_str(header);
                    out.push_str(" { ");
                    out.push_str(&scoped_inner);
                    out.push_str(" } ");
                }
            } else if lower.starts_with("@keyframes")
                || lower.starts_with("@-webkit-keyframes")
                || lower.starts_with("@font-face")
            {
                out.push_str(header);
                out.push_str(" {");
                out.push_str(inner);
                out.push_str("} ");
            }
            i = close + 1;
            continue;
        }

        // Find `{` starting declaration block outside quotes.
        let mut open = i;
        let mut in_quote: Option<u8> = None;
        while open < bytes.len() {
            if let Some(q) = in_quote {
                if bytes[open] == b'\\' && open + 1 < bytes.len() && bytes[open + 1] < 0x80 {
                    open += 2;
                    continue;
                }
                if bytes[open] == q {
                    in_quote = None;
                }
            } else if bytes[open] == b'"' || bytes[open] == b'\'' {
                in_quote = Some(bytes[open]);
            } else if bytes[open] == b'{' {
                break;
            }
            open += 1;
        }
        if open >= bytes.len() {
            break;
        }
        let Some(close) = find_matching_css_brace(css, open) else {
            break;
        };
        let selector_group = css.get(i..open).unwrap_or("").trim();
        let body = css.get(open + 1..close).unwrap_or("").trim();
        let mut scoped_selectors = Vec::new();
        for raw_sel in split_top_level_selectors(selector_group) {
            let sel = raw_sel.trim();
            if sel.is_empty() {
                continue;
            }
            if sel.contains("[data-dictionary") {
                scoped_selectors.push(sel.to_owned());
            } else if matches!(sel, ":root" | ":host" | "html" | "body") {
                scoped_selectors.push(scope_selector.to_owned());
            } else {
                scoped_selectors.push(format!("{scope_selector} {sel}"));
            }
        }
        if !scoped_selectors.is_empty() {
            out.push_str(&scoped_selectors.join(", "));
            out.push_str(" { ");
            out.push_str(body);
            out.push_str(" } ");
        }
        i = close + 1;
    }
    out.trim().to_owned()
}

/// Scopes a dictionary's `styles.css` rules to `[data-dictionary="<dict_name>"]`
/// and neutralizes any closing `</style` sequence.
pub fn scope_dictionary_css(css: &str, dict_name: &str) -> String {
    let stripped = strip_css_comments(css);
    if stripped.trim().is_empty() {
        return String::new();
    }
    let scope_selector = format!("[data-dictionary=\"{}\"]", escape_css_string(dict_name));
    let scoped = scope_css_rules(&stripped, &scope_selector);
    // Neutralize any case-insensitive `</style` sequence inside CSS content.
    let mut safe = String::with_capacity(scoped.len());
    let lower = scoped.to_ascii_lowercase();
    let mut start = 0usize;
    while let Some(rel) = lower.get(start..).and_then(|tail| tail.find("</style")) {
        let idx = start + rel;
        if let Some(prefix) = scoped.get(start..idx) {
            safe.push_str(prefix);
        }
        safe.push_str("<\\/style");
        start = idx + "</style".len();
    }
    if let Some(tail) = scoped.get(start..) {
        safe.push_str(tail);
    }
    safe
}

/// Scopes a dictionary's `styles.css` with a process-wide bounded cache keyed by
/// `(dict_id, raw_css_hash)` to avoid repeated scoping latency on Anki note adds
/// (Yomitan `#2512`).
pub fn scope_dictionary_css_cached(dict_id: i64, css: &str, dict_name: &str) -> String {
    if css.trim().is_empty() {
        return String::new();
    }
    let raw_hash = fnv1a_64(css.as_bytes());
    let key = (dict_id, raw_hash);
    {
        let cache = scoped_css_cache()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(entry) = cache.get(&key) {
            if entry.dict_name == dict_name && entry.raw_len == css.len() {
                return entry.scoped.clone();
            }
        }
    }
    let scoped = scope_dictionary_css(css, dict_name);
    {
        let mut cache = scoped_css_cache()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if cache.len() >= MAX_SCOPED_CSS_CACHE_ENTRIES && !cache.contains_key(&key) {
            cache.clear();
        }
        cache.insert(
            key,
            CachedScopedCss {
                dict_name: dict_name.to_owned(),
                raw_len: css.len(),
                scoped: scoped.clone(),
            },
        );
    }
    scoped
}

/// Clears the cached scoped dictionary CSS entries.
pub fn clear_scoped_css_cache() {
    let mut cache = scoped_css_cache()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    cache.clear();
}

#[cfg(test)]
pub(crate) fn scoped_css_cache_count_for_dict(dict_id: i64) -> usize {
    scoped_css_cache()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .keys()
        .filter(|&&(id, _)| id == dict_id)
        .count()
}

fn definition_html(definition: &GlossaryDefinition) -> String {
    let mut value = escape_html(&definition.glossary).replace('\n', "<br>");
    for (label, text) in [
        ("Example", &definition.example_ja),
        ("Translation", &definition.example_en),
        ("Notes", &definition.notes),
    ] {
        if !text.is_empty() {
            value.push_str(&format!(
                "<br><small>{label}: {}</small>",
                escape_html(text).replace('\n', "<br>")
            ));
        }
    }
    value
}

fn fill_shared_variants(mut html: String, variants: TermGlossaryVariants) -> GlossaryOutput {
    let mut remaining =
        usize::from(variants.full) + usize::from(variants.brief) + usize::from(variants.compact);
    if remaining == 0 || html.is_empty() {
        return GlossaryOutput::default();
    }
    let mut take_html = || {
        remaining -= 1;
        if remaining == 0 {
            std::mem::take(&mut html)
        } else {
            html.clone()
        }
    };
    GlossaryOutput {
        glossary: if variants.full {
            take_html()
        } else {
            String::new()
        },
        glossary_brief: if variants.brief {
            take_html()
        } else {
            String::new()
        },
        glossary_compact: if variants.compact {
            take_html()
        } else {
            String::new()
        },
    }
}

pub(crate) fn format_context_glossary_selected(
    ctx: &NoteFieldsContext,
    dictionary_id: Option<i64>,
    variants: TermGlossaryVariants,
) -> GlossaryOutput {
    if variants.is_empty() {
        return GlossaryOutput::default();
    }
    let is_kanji = ctx.is_kanji || !ctx.character.is_empty();
    if is_kanji {
        if ctx.glossary_definitions.is_empty() {
            if ctx.glossary_items.is_empty() {
                return GlossaryOutput::default();
            }
            // `format_kanji_glossary` HTML-escapes dictionary names, tags, and meanings internally.
            let tag_refs: Vec<&str> = ctx.tags.iter().map(String::as_str).collect();
            let item_refs: Vec<&str> = ctx.glossary_items.iter().map(String::as_str).collect();
            let html = format_kanji_glossary(&[KanjiDefEntry {
                dictionary_name: &ctx.dictionary_name,
                tags: &tag_refs,
                glossary: &item_refs,
            }]);
            return fill_shared_variants(html, variants);
        }
        let selected: Vec<_> = ctx
            .glossary_definitions
            .iter()
            .filter(|def| dictionary_id.is_none_or(|id| def.dictionary_id == id))
            .collect();
        let tag_refs: Vec<Vec<&str>> = selected
            .iter()
            .map(|d| d.tags.iter().map(String::as_str).collect())
            .collect();
        let meaning_refs: Vec<Vec<&str>> = selected
            .iter()
            .map(|d| {
                if !d.kanji_meanings.is_empty() {
                    d.kanji_meanings.iter().map(String::as_str).collect()
                } else {
                    d.glossary
                        .lines()
                        .map(str::trim)
                        .filter(|line| !line.is_empty())
                        .collect()
                }
            })
            .collect();
        let defs: Vec<KanjiDefEntry<'_>> = selected
            .iter()
            .enumerate()
            .map(|(i, d)| KanjiDefEntry {
                dictionary_name: &d.dictionary_name,
                tags: &tag_refs[i],
                glossary: &meaning_refs[i],
            })
            .collect();
        let html = format_kanji_glossary(&defs);
        return fill_shared_variants(html, variants);
    }

    if ctx.glossary_definitions.is_empty() {
        let items: Vec<&str> = ctx.glossary_items.iter().map(String::as_str).collect();
        let tags: Vec<&str> = ctx.tags.iter().map(String::as_str).collect();
        return format_glossary_selected(&items, &ctx.dictionary_name, &tags, variants);
    }
    let selected: Vec<_> = ctx
        .glossary_definitions
        .iter()
        .filter(|definition| dictionary_id.is_none_or(|id| definition.dictionary_id == id))
        .collect();
    if selected.is_empty() {
        return GlossaryOutput::default();
    }
    // `format_term_glossary_selected` HTML-escapes `dict_name` and `tags` internally.
    let fallback_html: Vec<Option<String>> = selected
        .iter()
        .map(|d| d.rendered_items.is_empty().then(|| definition_html(d)))
        .collect();
    let item_refs: Vec<Vec<&str>> = selected
        .iter()
        .enumerate()
        .map(|(i, d)| {
            if !d.rendered_items.is_empty() {
                d.rendered_items.iter().map(String::as_str).collect()
            } else {
                vec![fallback_html[i].as_deref().unwrap_or("")]
            }
        })
        .collect();
    let tag_refs: Vec<Vec<&str>> = selected
        .iter()
        .map(|d| d.tags.iter().map(String::as_str).collect())
        .collect();
    let definitions: Vec<TermGlossaryDef<'_>> = selected
        .iter()
        .enumerate()
        .map(|(i, definition)| TermGlossaryDef {
            dict_name: &definition.dictionary_name,
            dict_id: if definition.dictionary_id != 0 {
                definition.dictionary_id
            } else {
                (i as i64) + 1
            },
            tags: &tag_refs[i],
            items: &item_refs[i],
            has_styles: !definition.stylesheet.is_empty(),
            stylesheet: &definition.stylesheet,
        })
        .collect();
    let out = format_term_glossary_selected(&definitions, variants);
    GlossaryOutput {
        glossary: out.glossary,
        glossary_brief: out.glossary_brief,
        glossary_compact: out.glossary_compact,
    }
}

pub(crate) fn format_context_glossary_variant(
    ctx: &NoteFieldsContext,
    dictionary_id: Option<i64>,
    variant: TermGlossaryVariant,
) -> String {
    let variants = match variant {
        TermGlossaryVariant::Full => TermGlossaryVariants::FULL,
        TermGlossaryVariant::Brief => TermGlossaryVariants::BRIEF,
        TermGlossaryVariant::Compact => TermGlossaryVariants::COMPACT,
    };
    let out = format_context_glossary_selected(ctx, dictionary_id, variants);
    match variant {
        TermGlossaryVariant::Full => out.glossary,
        TermGlossaryVariant::Brief => out.glossary_brief,
        TermGlossaryVariant::Compact => out.glossary_compact,
    }
}

fn format_glossary_selected(
    items: &[&str],
    dict_name: &str,
    tags: &[&str],
    variants: TermGlossaryVariants,
) -> GlossaryOutput {
    if items.is_empty() || variants.is_empty() {
        return GlossaryOutput::default();
    }
    let def = TermGlossaryDef {
        dict_name,
        dict_id: 1,
        tags,
        items,
        has_styles: false,
        stylesheet: "",
    };
    let out = format_term_glossary_selected(&[def], variants);
    GlossaryOutput {
        glossary: out.glossary,
        glossary_brief: out.glossary_brief,
        glossary_compact: out.glossary_compact,
    }
}

/// Formats glossary definition items into Subomi's HTML representation.
pub fn format_glossary(items: &[&str], dict_name: &str, tags: &[&str]) -> GlossaryOutput {
    format_glossary_selected(items, dict_name, tags, TermGlossaryVariants::ALL)
}

/// Convenience helper to format glossary definitions as an HTML string.
pub fn format_glossary_html(items: &[String], dict_name: &str) -> String {
    let refs: Vec<&str> = items.iter().map(String::as_str).collect();
    format_glossary_selected(&refs, dict_name, &[], TermGlossaryVariants::FULL).glossary
}

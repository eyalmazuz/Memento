//! Tag category palette colors and redundant-tag suppression for rich dictionary rows.

use std::collections::HashSet;

use memento_definition::structured_content_text_value;

use crate::controller::dict::DictTag;

pub fn tag_category_hex_color(category: &str) -> [u8; 4] {
    match category {
        "name" => [0xb6, 0x32, 0x7a, 0xff],
        "expression" => [0xf0, 0xad, 0x4e, 0xff],
        "popular" => [0x02, 0x75, 0xd8, 0xff],
        "frequent" => [0x5b, 0xc0, 0xde, 0xff],
        "archaism" => [0xd9, 0x53, 0x4f, 0xff],
        "dictionary" => [0xaa, 0x66, 0xcc, 0xff],
        "frequency" => [0x5c, 0xb8, 0x5c, 0xff],
        "partOfSpeech" => [0x56, 0x56, 0x56, 0xff],
        "search" => [0x8a, 0x8a, 0x91, 0xff],
        "pitch-accent-dictionary" => [0x66, 0x40, 0xbe, 0xff],
        _ => [0x8a, 0x8a, 0x91, 0xff],
    }
}

#[allow(dead_code)]
pub(super) fn tag_brush(category: &str) -> slint::Brush {
    let [r, g, b, a] = tag_category_hex_color(category);
    slint::Color::from_argb_u8(a, r, g, b).into()
}

fn normalize_tag_key(name: &str) -> String {
    name.replace('⭐', "★").trim().to_lowercase()
}

/// Extract inline tag badge labels already embedded inside a pre-parsed Yomitan
/// `structured-content` JSON tree (`data-sc-class="tag"`, `data-sc-content="tag"`,
/// `data-sc-content="part-of-speech-info"`, `data-sc-content="sense-tags"`).
pub(super) fn extract_inline_structured_tags_from_value(
    parsed: &serde_json::Value,
) -> HashSet<String> {
    fn walk(node: &serde_json::Value, out: &mut HashSet<String>) {
        match node {
            serde_json::Value::Array(items) => {
                for item in items {
                    walk(item, out);
                }
            }
            serde_json::Value::Object(map) => {
                let data = map.get("data").and_then(serde_json::Value::as_object);
                let is_tag_node = data.is_some_and(|d| {
                    d.get("class").and_then(serde_json::Value::as_str) == Some("tag")
                        || matches!(
                            d.get("content").and_then(serde_json::Value::as_str),
                            Some("tag" | "part-of-speech-info" | "sense-tags")
                        )
                });
                if is_tag_node {
                    if let Some(content) = map.get("content") {
                        let mut txt = String::new();
                        structured_content_text_value(content, &mut txt);
                        let norm = normalize_tag_key(&txt);
                        if !norm.is_empty() {
                            out.insert(norm);
                        }
                    }
                    if let Some(title) = map.get("title").and_then(serde_json::Value::as_str) {
                        let norm_title = normalize_tag_key(title);
                        if !norm_title.is_empty() {
                            out.insert(norm_title);
                        }
                    }
                }
                if let Some(content) = map.get("content") {
                    walk(content, out);
                }
            }
            _ => {}
        }
    }

    let mut set = HashSet::new();
    walk(parsed, &mut set);
    set
}

/// Extract inline tag badge labels already embedded inside a Yomitan `structured-content` JSON
/// payload (`data-sc-class="tag"`, `data-sc-content="tag"`, `data-sc-content="part-of-speech-info"`).
pub fn extract_inline_structured_tags(raw_glossary: &str) -> HashSet<String> {
    if raw_glossary.trim().is_empty() {
        return HashSet::new();
    }
    serde_json::from_str::<serde_json::Value>(raw_glossary)
        .map(|parsed| extract_inline_structured_tags_from_value(&parsed))
        .unwrap_or_default()
}

/// Suppress redundant tags using an already-parsed glossary JSON tree (when available)
/// to avoid redundant JSON parsing.
pub(super) fn suppress_redundant_tags_with_inline(
    term_tags: &[DictTag],
    def_tags: &[DictTag],
    inline_tags: &HashSet<String>,
) -> (Vec<DictTag>, Vec<DictTag>) {
    let mut seen_term = HashSet::new();
    let mut filtered_term = Vec::new();
    for tag in term_tags {
        let display = tag.name.replace('⭐', "★");
        let key = normalize_tag_key(&display);
        if key.is_empty() || !seen_term.insert(key) {
            continue;
        }
        let mut t = tag.clone();
        t.name = display;
        filtered_term.push(t);
    }

    let mut seen_def = seen_term;
    let mut filtered_def = Vec::new();
    for tag in def_tags {
        let display = tag.name.replace('⭐', "★");
        let key = normalize_tag_key(&display);
        let notes_key = normalize_tag_key(&tag.notes);
        if key.is_empty() || !seen_def.insert(key.clone()) {
            continue;
        }
        if inline_tags.contains(&key)
            || (!notes_key.is_empty() && inline_tags.contains(&notes_key))
        {
            continue;
        }
        let mut t = tag.clone();
        t.name = display;
        filtered_def.push(t);
    }

    (filtered_term, filtered_def)
}

/// Suppress redundant tags (#220):
/// - Deduplicates `term_tags` across all rows of the same term group.
/// - Suppresses `def_tags` that duplicate a tag already shown in the term group's `term_tags`
///   or already rendered inline inside `structured-content`.
pub fn suppress_redundant_tags(
    term_tags: &[DictTag],
    def_tags: &[DictTag],
    raw_glossary: &str,
) -> (Vec<DictTag>, Vec<DictTag>) {
    let inline_tags = extract_inline_structured_tags(raw_glossary);
    suppress_redundant_tags_with_inline(term_tags, def_tags, &inline_tags)
}

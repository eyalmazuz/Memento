//! Tag category palette colors and redundant-tag suppression for rich dictionary rows.

use std::collections::HashSet;

use subomi_definition::structured_content_text_value;

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

/// Yomitan-aligned `compactTags` sense filter (H5.3 / Subomi `#220`).
///
/// Always deduplicates `term_tags` and removes `def_tags` already shown in `term_tags` or
/// inline inside `structured-content`. When `compact_tags` is `true`, also tracks tags
/// already emitted on earlier senses within the same entry in `seen_sense_tags` and omits
/// repeated definition tags on subsequent senses.
#[allow(dead_code)]
pub fn suppress_redundant_tags_for_sense(
    term_tags: &[DictTag],
    def_tags: &[DictTag],
    inline_tags: &HashSet<String>,
    seen_sense_tags: &mut HashSet<String>,
    compact_tags: bool,
) -> (Vec<DictTag>, Vec<DictTag>) {
    let (filtered_term, base_def) =
        suppress_redundant_tags_with_inline(term_tags, def_tags, inline_tags);
    if !compact_tags {
        return (filtered_term, base_def);
    }

    let mut compact_def = Vec::with_capacity(base_def.len());
    for tag in base_def {
        let key = normalize_tag_key(&tag.name);
        if key.is_empty() || !seen_sense_tags.insert(key) {
            continue;
        }
        compact_def.push(tag);
    }
    (filtered_term, compact_def)
}

/// Deduplicates term and definition tags across all senses within a single term entry
/// according to Yomitan's `compactTags` setting (`compact_tags: bool`).
#[allow(dead_code)]
pub fn compact_sense_tags(
    term_tags: &[DictTag],
    sense_def_tags: &[Vec<DictTag>],
    sense_inline_tags: &[HashSet<String>],
    compact_tags: bool,
) -> (Vec<DictTag>, Vec<Vec<DictTag>>) {
    let empty_inline = HashSet::new();
    let mut seen_sense_tags = HashSet::new();
    let mut dedup_term = Vec::new();
    let mut per_sense = Vec::with_capacity(sense_def_tags.len());

    for (idx, def_tags) in sense_def_tags.iter().enumerate() {
        let inline = sense_inline_tags.get(idx).unwrap_or(&empty_inline);
        let (t, d) = suppress_redundant_tags_for_sense(
            term_tags,
            def_tags,
            inline,
            &mut seen_sense_tags,
            compact_tags,
        );
        if idx == 0 {
            dedup_term = t;
        }
        per_sense.push(d);
    }
    if sense_def_tags.is_empty() {
        dedup_term = suppress_redundant_tags_with_inline(term_tags, &[], &empty_inline).0;
    }
    (dedup_term, per_sense)
}

/// Compacts comma-separated tag display strings across senses of the same entry.
/// When `compact_tags` is `true`, identical tag names already shown on an earlier sense
/// of the same entry are omitted from subsequent senses.
pub fn compact_tag_names_across_senses(
    sense_tag_strings: &[&str],
    compact_tags: bool,
) -> Vec<String> {
    let mut seen_entry: HashSet<String> = HashSet::new();
    let mut out = Vec::with_capacity(sense_tag_strings.len());

    for raw in sense_tag_strings {
        let mut seen_in_sense: HashSet<String> = HashSet::new();
        let mut kept: Vec<String> = Vec::new();
        for part in raw.split(',') {
            let display = part.replace('⭐', "★");
            let trimmed = display.trim();
            let key = normalize_tag_key(trimmed);
            if key.is_empty() || !seen_in_sense.insert(key.clone()) {
                continue;
            }
            if compact_tags && !seen_entry.insert(key) {
                continue;
            }
            kept.push(trimmed.to_owned());
        }
        out.push(kept.join(", "));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_tags_deduplicates_identical_tags_across_senses_within_entry() {
        let term = vec![DictTag::with_details("common", "popular", "", 0, 10)];
        let sense1 = vec![
            DictTag::with_details("common", "popular", "", 0, 10),
            DictTag::with_details("n", "partOfSpeech", "noun", 1, 0),
            DictTag::with_details("vs", "partOfSpeech", "suru verb", 2, 0),
        ];
        let sense2 = vec![
            DictTag::with_details("n", "partOfSpeech", "noun", 1, 0),
            DictTag::with_details("adj-no", "partOfSpeech", "no-adjective", 3, 0),
        ];
        let sense3 = vec![
            DictTag::with_details("vs", "partOfSpeech", "suru verb", 2, 0),
            DictTag::with_details("adj-no", "partOfSpeech", "no-adjective", 3, 0),
        ];

        // compact_tags = false keeps repeated sense-level tags across senses (while still dropping term dupes)
        let (t_off, d_off) = compact_sense_tags(
            &term,
            &[sense1.clone(), sense2.clone(), sense3.clone()],
            &[],
            false,
        );
        assert_eq!(t_off.len(), 1);
        assert_eq!(
            d_off[0].iter().map(|t| t.name.as_str()).collect::<Vec<_>>(),
            vec!["n", "vs"]
        );
        assert_eq!(
            d_off[1].iter().map(|t| t.name.as_str()).collect::<Vec<_>>(),
            vec!["n", "adj-no"]
        );
        assert_eq!(
            d_off[2].iter().map(|t| t.name.as_str()).collect::<Vec<_>>(),
            vec!["vs", "adj-no"]
        );

        // compact_tags = true suppresses tags already shown on earlier senses
        let (t_on, d_on) = compact_sense_tags(&term, &[sense1, sense2, sense3], &[], true);
        assert_eq!(t_on.len(), 1);
        assert_eq!(
            d_on[0].iter().map(|t| t.name.as_str()).collect::<Vec<_>>(),
            vec!["n", "vs"]
        );
        assert_eq!(
            d_on[1].iter().map(|t| t.name.as_str()).collect::<Vec<_>>(),
            vec!["adj-no"]
        );
        assert!(d_on[2].is_empty());

        let names = compact_tag_names_across_senses(&["n, vs", "n, adj-no", "vs"], true);
        assert_eq!(names, vec!["n, vs", "adj-no", ""]);
    }
}

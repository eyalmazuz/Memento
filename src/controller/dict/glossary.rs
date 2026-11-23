//! Glossary JSON cleaning, shared parsed-Value LRU cache, rich block extraction, and HTML stripping.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use super::GlossaryBlock;

const PARSED_GLOSSARY_CACHE_CAPACITY: usize = 384;
const PARSED_GLOSSARY_CACHE_MAX_BYTES: usize = 8 * 1024 * 1024; // 8 MiB raw JSON equivalent
const PARSED_GLOSSARY_EVICT_BATCH: usize = 96;

type GlossaryCacheKey = ([u8; 16], usize);

#[inline]
fn glossary_cache_key(raw: &str) -> Option<GlossaryCacheKey> {
    if raw.is_empty() {
        return None;
    }
    let digest = subomi_util::fileutils::calculate_md5(raw.as_bytes()).ok()?;
    Some((digest, raw.len()))
}

struct ParsedGlossaryCache {
    entries: HashMap<GlossaryCacheKey, (u64, usize, Arc<serde_json::Value>)>,
    total_raw_bytes: usize,
    clock: u64,
}

impl ParsedGlossaryCache {
    fn new() -> Self {
        Self {
            entries: HashMap::new(),
            total_raw_bytes: 0,
            clock: 0,
        }
    }

    fn get(&mut self, key: &GlossaryCacheKey) -> Option<Arc<serde_json::Value>> {
        self.clock = self.clock.wrapping_add(1);
        let stamp = self.clock;
        if let Some((last_used, _, value)) = self.entries.get_mut(key) {
            *last_used = stamp;
            Some(Arc::clone(value))
        } else {
            None
        }
    }

    fn evict_oldest(&mut self, evict_target: usize) {
        let evict = evict_target.min(self.entries.len());
        if evict == 0 {
            return;
        }
        if evict >= self.entries.len() {
            self.entries.clear();
            self.total_raw_bytes = 0;
            return;
        }
        let mut stamps: Vec<u64> = self.entries.values().map(|(s, _, _)| *s).collect();
        let (_, threshold, _) = stamps.select_nth_unstable(evict - 1);
        let cutoff = *threshold;
        let mut removed_bytes = 0usize;
        self.entries.retain(|_, (s, raw_bytes, _)| {
            if *s > cutoff {
                true
            } else {
                removed_bytes = removed_bytes.saturating_add(*raw_bytes);
                false
            }
        });
        self.total_raw_bytes = self.total_raw_bytes.saturating_sub(removed_bytes);
    }

    fn insert(&mut self, key: GlossaryCacheKey, value: Arc<serde_json::Value>) {
        let raw_bytes = key.1;
        if raw_bytes > PARSED_GLOSSARY_CACHE_MAX_BYTES {
            return;
        }
        self.clock = self.clock.wrapping_add(1);
        let stamp = self.clock;
        if let Some(existing) = self.entries.get_mut(&key) {
            existing.0 = stamp;
            existing.2 = value;
            return;
        }
        while (self.entries.len() >= PARSED_GLOSSARY_CACHE_CAPACITY
            || self.total_raw_bytes.saturating_add(raw_bytes) > PARSED_GLOSSARY_CACHE_MAX_BYTES)
            && !self.entries.is_empty()
        {
            self.evict_oldest(PARSED_GLOSSARY_EVICT_BATCH.max(1));
        }
        self.total_raw_bytes = self.total_raw_bytes.saturating_add(raw_bytes);
        self.entries.insert(key, (stamp, raw_bytes, value));
    }

    fn shrink_to(&mut self, max_entries: usize) {
        if self.entries.len() > max_entries {
            let excess = self.entries.len() - max_entries;
            self.evict_oldest(excess);
        }
        self.entries.shrink_to(max_entries.max(16));
    }
}

fn parsed_glossary_cache() -> &'static Mutex<ParsedGlossaryCache> {
    static CACHE: OnceLock<Mutex<ParsedGlossaryCache>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(ParsedGlossaryCache::new()))
}

/// Shrinks the shared parsed-glossary JSON cache down to at most `max_entries` hottest entries.
pub fn shrink_parsed_glossary_cache(max_entries: usize) {
    let mut cache = parsed_glossary_cache()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    cache.shrink_to(max_entries);
}

pub(super) fn cache_parsed_glossary(raw: &str, value: Arc<serde_json::Value>) {
    let Some(key) = glossary_cache_key(raw) else {
        return;
    };
    let mut cache = parsed_glossary_cache()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    cache.insert(key, value);
}

/// Parses a raw glossary JSON string into a shared `Arc<serde_json::Value>`,
/// reusing cached parses populated during dictionary search so UI rendering can
/// feed `subomi_definition`'s `*_value` APIs without re-parsing JSON.
pub fn parse_glossary_json_cached(raw: &str) -> Option<Arc<serde_json::Value>> {
    let key = glossary_cache_key(raw)?;
    {
        let mut cache = parsed_glossary_cache()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(hit) = cache.get(&key) {
            return Some(hit);
        }
    }
    let parsed = Arc::new(serde_json::from_str::<serde_json::Value>(raw).ok()?);
    let mut cache = parsed_glossary_cache()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    cache.insert(key, Arc::clone(&parsed));
    Some(parsed)
}

/// Strips legacy Yomichan nested-array deinflection entries (`[["base", ["rule"]]]`)
/// from a `term_bank.glossary` JSON payload, checking and populating the shared
/// parsed-glossary cache so warm dictionary lookups skip `serde_json::from_str`
/// and move `raw_glossary` without cloning. Returns `None` if no real items remain.
pub(super) fn clean_raw_glossary_cached(
    raw_glossary: String,
) -> Option<(String, Option<Arc<serde_json::Value>>)> {
    let key = glossary_cache_key(&raw_glossary);
    if let Some(key) = key {
        let mut cache = parsed_glossary_cache()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(hit) = cache.get(&key) {
            return Some((raw_glossary, Some(hit)));
        }
    }
    match serde_json::from_str::<serde_json::Value>(&raw_glossary) {
        Ok(serde_json::Value::Array(items)) => {
            let has_nested_array = items.iter().any(serde_json::Value::is_array);
            if !has_nested_array {
                if items.is_empty() {
                    return None;
                }
                let arc = Arc::new(serde_json::Value::Array(items));
                if let Some(key) = key {
                    let mut cache = parsed_glossary_cache()
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    cache.insert(key, Arc::clone(&arc));
                }
                return Some((raw_glossary, Some(arc)));
            }
            let filtered: Vec<serde_json::Value> = items
                .into_iter()
                .filter(|item| !item.is_array())
                .collect();
            if filtered.is_empty() {
                return None;
            }
            let cleaned_val = serde_json::Value::Array(filtered);
            let cleaned_json = serde_json::to_string(&cleaned_val).unwrap_or_default();
            let arc = Arc::new(cleaned_val);
            cache_parsed_glossary(&cleaned_json, Arc::clone(&arc));
            Some((cleaned_json, Some(arc)))
        }
        Ok(other) => {
            let arc = Arc::new(other);
            if let Some(key) = key {
                let mut cache = parsed_glossary_cache()
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                cache.insert(key, Arc::clone(&arc));
            }
            Some((raw_glossary, Some(arc)))
        }
        Err(_) => Some((raw_glossary, None)),
    }
}

fn rich_node_text(node: &serde_json::Value) -> String {
    match node {
        serde_json::Value::String(value) => strip_glossary_markup(value),
        serde_json::Value::Array(items) => items.iter().map(rich_node_text).collect(),
        serde_json::Value::Object(map) => {
            if matches!(
                map.get("tag").and_then(serde_json::Value::as_str),
                Some("rt" | "rp" | "img")
            ) {
                return String::new();
            }
            map.get("content")
                .or_else(|| map.get("text"))
                .map(rich_node_text)
                .unwrap_or_default()
        }
        _ => String::new(),
    }
}

/// Keep headings, list items, and links separate for the popup while the plain
/// glossary remains available for search and Anki fields.
pub(super) fn parse_rich_glossary_value(value: &serde_json::Value) -> Vec<GlossaryBlock> {
    fn push(blocks: &mut Vec<GlossaryBlock>, text: String, kind: i32, href: &str) {
        let text = text.trim().trim_start_matches(['◦', '•']).trim().to_owned();
        if !text.is_empty() {
            blocks.push(GlossaryBlock {
                text,
                kind,
                href: href.to_owned(),
            });
        }
    }
    fn walk(node: &serde_json::Value, blocks: &mut Vec<GlossaryBlock>) {
        match node {
            serde_json::Value::String(value) => {
                for line in strip_glossary_markup(value).lines() {
                    push(blocks, line.to_owned(), 2, "");
                }
            }
            serde_json::Value::Array(items) => {
                for item in items {
                    walk(item, blocks);
                }
            }
            serde_json::Value::Object(map) => {
                let tag = map
                    .get("tag")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("");
                let data = map.get("data").and_then(serde_json::Value::as_object);
                if data
                    .and_then(|data| data.get("content"))
                    .and_then(serde_json::Value::as_str)
                    == Some("examples")
                {
                    return;
                }
                let Some(content) = map.get("content") else {
                    return;
                };
                match tag {
                    "rt" | "rp" | "img" => {}
                    "li" => push(blocks, rich_node_text(content), 2, ""),
                    "a" => push(
                        blocks,
                        rich_node_text(content),
                        3,
                        map.get("href")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or(""),
                    ),
                    "span" => {
                        let has_link = content.as_array().is_some_and(|items| {
                            items.iter().any(|item| {
                                item.get("tag").and_then(serde_json::Value::as_str) == Some("a")
                            })
                        });
                        if has_link {
                            walk(content, blocks);
                        } else {
                            let bold = map
                                .get("style")
                                .and_then(|style| style.get("fontWeight"))
                                .and_then(serde_json::Value::as_str)
                                == Some("bold")
                                || content.as_array().is_some_and(|items| {
                                    items.iter().any(|item| {
                                        item.get("style")
                                            .and_then(|style| style.get("fontWeight"))
                                            .and_then(serde_json::Value::as_str)
                                            == Some("bold")
                                    })
                                });
                            push(blocks, rich_node_text(content), if bold { 1 } else { 0 }, "");
                        }
                    }
                    "div"
                        if data
                            .and_then(|data| data.get("name"))
                            .and_then(serde_json::Value::as_str)
                            == Some("definition") =>
                    {
                        push(blocks, rich_node_text(content), 0, "");
                    }
                    "div" | "ul" | "ol" => walk(content, blocks),
                    _ => {
                        if content.is_string() {
                            push(blocks, rich_node_text(content), 0, "");
                        } else {
                            walk(content, blocks);
                        }
                    }
                }
            }
            _ => {}
        }
    }
    let mut blocks = Vec::new();
    walk(value, &mut blocks);
    blocks
}

pub fn parse_rich_glossary(raw: &str) -> Vec<GlossaryBlock> {
    if let Some(value) = parse_glossary_json_cached(raw) {
        parse_rich_glossary_value(&value)
    } else {
        strip_glossary_markup(raw)
            .lines()
            .filter_map(|line| {
                let text = line.trim();
                (!text.is_empty()).then(|| GlossaryBlock {
                    text: text.to_owned(),
                    kind: 0,
                    href: String::new(),
                })
            })
            .collect()
    }
}

pub fn parse_glossary_entry(raw: &str) -> (String, String, String) {
    match parse_glossary_json_cached(raw) {
        Some(value) => parse_glossary_entry_value(&value),
        None => (strip_glossary_markup(raw), String::new(), String::new()),
    }
}

pub(super) fn parse_glossary_entry_value(value: &serde_json::Value) -> (String, String, String) {
    let items = value
        .as_array()
        .map_or_else(|| vec![value], |items| items.iter().collect());
    let mut senses = Vec::new();
    let mut ex_ja = Vec::new();
    let mut ex_en = Vec::new();

    fn push_plain_text(node: &serde_json::Value, out: &mut String) {
        match node {
            serde_json::Value::String(text) => {
                if !text.contains('<') && !text.contains('&') {
                    out.push_str(text);
                } else {
                    out.push_str(&strip_glossary_markup(text));
                }
            }
            serde_json::Value::Array(items) => {
                for item in items {
                    push_plain_text(item, out);
                }
            }
            serde_json::Value::Object(map) => {
                if let Some(tag) = map.get("tag").and_then(serde_json::Value::as_str) {
                    if tag == "rt" || tag == "rp" {
                        return;
                    }
                }
                if let Some(content) = map.get("content") {
                    push_plain_text(content, out);
                } else if let Some(text) = map.get("text").and_then(serde_json::Value::as_str) {
                    if !text.contains('<') && !text.contains('&') {
                        out.push_str(text);
                    } else {
                        out.push_str(&strip_glossary_markup(text));
                    }
                }
            }
            _ => {}
        }
    }

    let extract_plain_text = |node: &serde_json::Value| -> String {
        let mut buf = String::new();
        push_plain_text(node, &mut buf);
        buf
    };

    for item in items {
        if item.is_array() {
            continue;
        }
        match item {
            serde_json::Value::String(text) => {
                for line in text.split('\n') {
                    let trimmed = strip_glossary_markup(line.trim());
                    if !trimmed.is_empty() {
                        if trimmed.starts_with('◦') || trimmed.starts_with('•') {
                            senses.push(trimmed);
                        } else {
                            senses.push(format!("◦ {trimmed}"));
                        }
                    }
                }
            }
            serde_json::Value::Object(map)
                if map.get("type").and_then(serde_json::Value::as_str)
                    == Some("structured-content") =>
            {
                let content_nodes = match map.get("content") {
                    Some(serde_json::Value::Array(arr)) => arr.iter().collect::<Vec<_>>(),
                    Some(single) => vec![single],
                    None => Vec::new(),
                };
                for node in content_nodes {
                    if let serde_json::Value::Object(block) = node {
                        let data_content = block
                            .get("data")
                            .and_then(serde_json::Value::as_object)
                            .and_then(|d| d.get("content"))
                            .and_then(serde_json::Value::as_str);

                        if data_content == Some("examples") {
                            let ex_nodes = match block.get("content") {
                                Some(serde_json::Value::Array(arr)) => {
                                    arr.iter().collect::<Vec<_>>()
                                }
                                Some(single) => vec![single],
                                None => Vec::new(),
                            };
                            for ex_item in ex_nodes {
                                if let serde_json::Value::Object(ex_obj) = ex_item {
                                    let style_str = ex_obj
                                        .get("style")
                                        .and_then(|s| s.get("listStyleType"))
                                        .and_then(serde_json::Value::as_str)
                                        .unwrap_or("");
                                    let lang = ex_obj
                                        .get("lang")
                                        .and_then(serde_json::Value::as_str)
                                        .unwrap_or("");
                                    let txt = extract_plain_text(ex_item).trim().to_string();
                                    if !txt.is_empty() {
                                        if style_str.contains("🇯🇵") || lang == "ja" {
                                            ex_ja.push(txt);
                                        } else if style_str.contains("🇬🇧") || lang == "en" {
                                            ex_en.push(txt);
                                        } else if txt.chars().any(|c| (c as u32) >= 0x3000) {
                                            ex_ja.push(txt);
                                        } else {
                                            ex_en.push(txt);
                                        }
                                    }
                                }
                            }
                        } else if data_content == Some("glossary")
                            || matches!(
                                block.get("tag").and_then(serde_json::Value::as_str),
                                Some("ul" | "ol")
                            )
                        {
                            let li_nodes = match block.get("content") {
                                Some(serde_json::Value::Array(arr)) => {
                                    arr.iter().collect::<Vec<_>>()
                                }
                                Some(single) => vec![single],
                                None => Vec::new(),
                            };
                            for li in li_nodes {
                                let txt = extract_plain_text(li).trim().to_string();
                                if !txt.is_empty() {
                                    senses.push(format!("◦ {txt}"));
                                }
                            }
                        } else {
                            let txt = extract_plain_text(node).trim().to_string();
                            if !txt.is_empty() {
                                for line in txt.split('\n') {
                                    let l = line.trim();
                                    if !l.is_empty() {
                                        senses.push(format!("◦ {l}"));
                                    }
                                }
                            }
                        }
                    } else {
                        let txt = extract_plain_text(node).trim().to_string();
                        if !txt.is_empty() {
                            senses.push(format!("◦ {txt}"));
                        }
                    }
                }
            }
            _ => {
                let txt = extract_plain_text(item).trim().to_string();
                if !txt.is_empty() {
                    senses.push(format!("◦ {txt}"));
                }
            }
        }
    }

    (senses.join("\n"), ex_ja.join("\n"), ex_en.join("\n"))
}

pub fn format_glossary(raw: &str) -> String {
    parse_glossary_entry(raw).0
}

fn is_valid_html_tag_inner(inner: &str) -> bool {
    let bytes = inner.as_bytes();
    if bytes.is_empty() || inner.contains('<') {
        return false;
    }
    if bytes.starts_with(b"!--") {
        return inner.ends_with("--");
    }
    let name_start = usize::from(bytes[0] == b'/');
    let Some(&first) = bytes.get(name_start) else {
        return false;
    };
    if !first.is_ascii_alphabetic() {
        return false;
    }
    let rest = &inner[name_start..];
    let name_end = rest
        .find(|c: char| c.is_ascii_whitespace() || c == '/')
        .unwrap_or(rest.len());
    let tag_name = &rest[..name_end];
    !tag_name.is_empty()
        && tag_name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b':')
}

/// Strips valid HTML tags (`<tag...>`, `</tag>`, `<!--...-->`) and decodes basic HTML entities
/// while preserving bare `<` characters (such as `x < 5` or `A < B`) without truncating text.
pub(super) fn strip_glossary_markup(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let mut cursor = 0usize;
    let bytes = text.as_bytes();

    while cursor < bytes.len() {
        let Some(rel_lt) = text[cursor..].find('<') else {
            output.push_str(&text[cursor..]);
            break;
        };
        let lt_pos = cursor + rel_lt;
        output.push_str(&text[cursor..lt_pos]);

        let after_lt = lt_pos + 1;
        if let Some(rel_gt) = text[after_lt..].find('>') {
            let gt_pos = after_lt + rel_gt;
            let inner = &text[after_lt..gt_pos];
            if is_valid_html_tag_inner(inner) {
                let trimmed = inner.trim();
                let is_closing = trimmed.starts_with('/');
                let name_slice = trimmed.strip_prefix('/').unwrap_or(trimmed);
                let name_end = name_slice
                    .find(|c: char| c.is_ascii_whitespace() || c == '/')
                    .unwrap_or(name_slice.len());
                let tag_name = &name_slice[..name_end];
                if !is_closing && tag_name.eq_ignore_ascii_case("li") {
                    if !output.ends_with('\n') {
                        output.push('\n');
                    }
                    output.push_str("◦ ");
                } else if (!is_closing && tag_name.eq_ignore_ascii_case("br"))
                    || (is_closing
                        && (tag_name.eq_ignore_ascii_case("p")
                            || tag_name.eq_ignore_ascii_case("div")
                            || tag_name.eq_ignore_ascii_case("li")))
                {
                    output.push('\n');
                }
                cursor = gt_pos + 1;
                continue;
            }
        }

        output.push('<');
        cursor = after_lt;
    }

    if !output.contains('&') {
        return output;
    }

    output
        .replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&")
}

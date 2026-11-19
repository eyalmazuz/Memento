////////////////////////////////////////////////////////////////////////////////
//
// Copyright (c) 2026 Ripose
//
// This file is part of Memento.
//
// Memento is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, version 2 of the License.
//
// Memento is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with Memento.  If not, see <https://www.gnu.org/licenses/>.
//
////////////////////////////////////////////////////////////////////////////////

//! Structured-content inspection, ruby/details/sibling helpers, and attribute
//! extraction shared by the dictionary popup renderer.

use serde_json::Value;

use crate::css::Renderer;
use crate::element::{Context, ElementRenderState};
use crate::qstring::{equals_ascii_ci, to_lower};

/// Glossary separator display modes.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[repr(i32)]
pub enum LegacyGlossaryStyle {
    #[default]
    Bullet = 0,
    LineBreak = 1,
    Pipe = 2,
    NoSeparator = 3,
}

/// Stable ID and state for a structured `<details>` element.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DetailsState {
    pub id: String,
    pub open: bool,
}

/// Ruby base nodes retain source references, including nested arrays; `rt`
/// readings and `rp` parentheses are removed.
#[derive(Debug, PartialEq)]
pub struct RubyValueParts<'a> {
    pub base: Vec<&'a Value>,
    pub reading: String,
}

/// Escapes `<`, `>`, `&`, and `"` (not apostrophes).
fn append_html_escaped(text: &str, out: &mut String, newlines_as_breaks: bool) {
    let mut start = 0;
    for (index, ch) in text.char_indices() {
        let replacement = match ch {
            '<' => "&lt;",
            '>' => "&gt;",
            '&' => "&amp;",
            '"' => "&quot;",
            '\n' if newlines_as_breaks => "<br>",
            _ => continue,
        };
        out.push_str(&text[start..index]);
        out.push_str(replacement);
        start = index + ch.len_utf8();
    }
    out.push_str(&text[start..]);
}

/// Flatten only transparent direct arrays, skipping null; an object is one
/// element regardless of the type of its own `content` property.
pub fn append_direct_content_values_value<'a>(value: &'a Value, out: &mut Vec<&'a Value>) {
    if let Some(children) = value.as_array() {
        for child in children {
            append_direct_content_values_value(child, out);
        }
    } else if !value.is_null() {
        out.push(value);
    }
}

/// Returns true if a structured `style` object sets `backgroundColor`,
/// `border*`, or `padding*`.
pub fn has_inline_box_style_value(style: &Value) -> bool {
    let Some(map) = style.as_object() else {
        return false;
    };
    map.keys().any(|key| {
        let lowered = to_lower(key);
        lowered == "backgroundcolor"
            || lowered.starts_with("border")
            || lowered.starts_with("padding")
    })
}

/// Returns true if `value` is a `<span>` with string `content` and inline box
/// styling.
pub fn is_inline_box_label_value(value: &Value) -> bool {
    let Some(map) = value.as_object() else {
        return false;
    };
    map.get("tag")
        .and_then(Value::as_str)
        .is_some_and(|tag| equals_ascii_ci(tag, "span"))
        && map.get("content").is_some_and(Value::is_string)
        && map.get("style").is_some_and(has_inline_box_style_value)
}

/// Returns true if `value` contains only inline structured content nodes.
pub fn is_inline_structured_content_value(value: &Value) -> bool {
    if let Some(children) = value.as_array() {
        children.iter().all(is_inline_structured_content_value)
    } else if let Some(map) = value.as_object() {
        let tag = map.get("tag").and_then(Value::as_str).unwrap_or("");
        matches!(
            to_lower(tag).as_str(),
            "a" | "br" | "img" | "rp" | "rt" | "ruby" | "span"
        )
    } else {
        true
    }
}

/// Extracts plain text from structured content, ignoring tags, attributes, and
/// non-text leaves while recursively traversing object `content`.
pub fn structured_content_text_value(value: &Value, out: &mut String) {
    if let Some(text) = value.as_str() {
        out.push_str(text);
    } else if let Some(children) = value.as_array() {
        for child in children {
            structured_content_text_value(child, out);
        }
    } else if let Some(content) = value.get("content") {
        structured_content_text_value(content, out);
    }
}

/// Splits direct ruby children into base nodes and concatenated `rt` reading;
/// nested arrays remain base nodes and `rp` nodes are dropped.
pub fn split_ruby_content_value(value: &Value) -> RubyValueParts<'_> {
    let mut base = Vec::new();
    let mut reading = String::new();
    let children = value
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or(std::slice::from_ref(value));
    for child in children {
        if let Some(map) = child.as_object() {
            if let Some(tag) = map.get("tag").and_then(Value::as_str) {
                if equals_ascii_ci(tag, "rt") {
                    if let Some(content) = map.get("content") {
                        structured_content_text_value(content, &mut reading);
                    }
                    continue;
                }
                if equals_ascii_ci(tag, "rp") {
                    continue;
                }
            }
        }
        base.push(child);
    }
    RubyValueParts { base, reading }
}

/// Returns true if `value` contains any `<ruby>` tag.
pub fn contains_ruby_value(value: &Value) -> bool {
    if let Some(children) = value.as_array() {
        children.iter().any(contains_ruby_value)
    } else if let Some(map) = value.as_object() {
        map.get("tag")
            .and_then(Value::as_str)
            .is_some_and(|tag| equals_ascii_ci(tag, "ruby"))
            || map.get("content").is_some_and(contains_ruby_value)
    } else {
        false
    }
}

/// Returns true if `value` contains any `verticalAlign: super | sub` subtree.
pub fn contains_raised_content_value(value: &Value) -> bool {
    if let Some(children) = value.as_array() {
        children.iter().any(contains_raised_content_value)
    } else if let Some(map) = value.as_object() {
        let alignment = map
            .get("style")
            .and_then(|s| s.get("verticalAlign"))
            .and_then(Value::as_str)
            .unwrap_or("");
        equals_ascii_ci(alignment, "super")
            || equals_ascii_ci(alignment, "sub")
            || map.get("content").is_some_and(contains_raised_content_value)
    } else {
        false
    }
}

/// Returns true if an `<a>` element requires custom rendering (ruby,
/// superscript/subscript, or title tooltip).
pub fn anchor_needs_custom_handling_value(value: &Value, ctx: &Context) -> bool {
    value.get("content").is_some_and(contains_ruby_value)
        || value
            .get("content")
            .is_some_and(contains_raised_content_value)
        || value.get("title").is_some_and(Value::is_string)
        || !ctx.title_tooltip.is_empty()
}

impl Renderer {
    /// Returns true if any top-level entry has `"type": "structured-content"`.
    pub fn contains_structured_content_value(&self, entries: &[Value]) -> bool {
        entries.iter().any(|value| {
            value
                .as_object()
                .and_then(|map| map.get("type"))
                .and_then(Value::as_str)
                == Some("structured-content")
        })
    }

    /// Emits `data-sc-*` attributes in UTF-16 key order.
    pub fn append_structured_data_value(&self, data: &Value, out: &mut String) {
        let Some(map) = data.as_object() else {
            return;
        };
        let mut emit = |key: &str, value: &Value| {
            if let Some(s) = value.as_str() {
                out.push(' ');
                out.push_str(&self.structured_data_attribute_name(key));
                out.push_str("=\"");
                append_html_escaped(s, out, false);
                out.push('"');
            }
        };
        if map.keys().all(|key| key.is_ascii()) {
            for (key, value) in map {
                emit(key, value);
            }
        } else {
            let mut entries: Vec<(&str, &Value)> =
                map.iter().map(|(k, v)| (k.as_str(), v)).collect();
            entries.sort_by(|(a, _), (b, _)| a.encode_utf16().cmp(b.encode_utf16()));
            for (key, value) in entries {
                emit(key, value);
            }
        }
    }

    /// Extracts HTML attributes and tooltip metadata for a structured element.
    pub fn add_element_attributes_value(&self, obj: &Value, state: &mut ElementRenderState) {
        let out = &mut state.attributes;
        if let Some(href) = obj.get("href").and_then(Value::as_str) {
            out.push_str(" href=\"");
            append_html_escaped(href, out, false);
            out.push('"');
        }
        if let Some(data) = obj.get("data") {
            self.append_structured_data_value(data, out);
        }
        for (key, name) in [("colSpan", "colspan"), ("rowSpan", "rowspan")] {
            if let Some(number) = obj.get(key).and_then(Value::as_f64) {
                out.push(' ');
                out.push_str(name);
                out.push_str("=\"");
                out.push_str(&(number as i32).to_string());
                out.push('"');
            }
        }
        if let Some(title) = obj.get("title").and_then(Value::as_str) {
            state.title_tooltip.clear();
            state.title_tooltip.push_str(title);
            out.push_str(" title=\"");
            append_html_escaped(title, out, false);
            out.push('"');
        }
        if let Some(lang) = obj.get("lang").and_then(Value::as_str) {
            out.push_str(" lang=\"");
            append_html_escaped(lang, out, false);
            out.push('"');
        }
    }

    /// Counts direct supported structured child elements (flattening transparent
    /// arrays, without recursing into object `content`).
    pub fn structured_element_child_count_value(&self, value: &Value) -> i64 {
        if let Some(children) = value.as_array() {
            children
                .iter()
                .map(|child| self.structured_element_child_count_value(child))
                .sum()
        } else if let Some(map) = value.as_object() {
            let tag = map.get("tag").and_then(Value::as_str).unwrap_or("");
            i64::from(self.is_supported_structured_tag(&to_lower(tag)))
        } else {
            0
        }
    }

    /// Returns true if any direct child element tag needs a `:last-child` count
    /// under the active stylesheet.
    pub fn needs_structured_element_child_count_value(&self, value: &Value, ctx: &Context) -> bool {
        let Some(styles) = ctx.dictionary_styles.as_ref() else {
            return false;
        };
        if let Some(children) = value.as_array() {
            children
                .iter()
                .any(|child| self.needs_structured_element_child_count_value(child, ctx))
        } else if let Some(map) = value.as_object() {
            let tag = to_lower(map.get("tag").and_then(Value::as_str).unwrap_or(""));
            self.is_supported_structured_tag(&tag) && styles.needs_element_child_count_for_tag(&tag)
        } else {
            false
        }
    }

    /// Allocates the next `<details>` ID and resolves its open/closed state.
    pub fn details_state_value(&self, obj: &Value, ctx: &mut Context) -> DetailsState {
        let id = ctx.next_detail_id.to_string();
        ctx.next_detail_id += 1;
        let open = ctx
            .detail_states
            .get(&id)
            .copied()
            .unwrap_or_else(|| obj.get("open").and_then(Value::as_bool) == Some(true));
        DetailsState { id, open }
    }

    /// Consumes `<details>` IDs in depth-first source order inside an unrendered
    /// closed `<details>` branch.
    pub fn skip_closed_details_content_value(&self, value: &Value, ctx: &mut Context) {
        if let Some(children) = value.as_array() {
            for child in children {
                self.skip_closed_details_content_value(child, ctx);
            }
        } else if let Some(map) = value.as_object() {
            if map
                .get("tag")
                .and_then(Value::as_str)
                .is_some_and(|tag| equals_ascii_ci(tag, "details"))
            {
                ctx.next_detail_id += 1;
            }
            if let Some(content) = map.get("content") {
                self.skip_closed_details_content_value(content, ctx);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::element::ParsedStylesheet;
    use std::sync::Arc;

    fn v(text: &str) -> Value {
        serde_json::from_str(text).expect("valid JSON")
    }

    #[test]
    fn structured_data_attributes_follow_utf16_key_order() {
        let renderer = Renderer::new();
        let raw = r#"{"\ue000":"bmp","\ud83d\ude00":"astral","a":"ascii"}"#;
        let mut html = String::new();
        renderer.append_structured_data_value(&v(raw), &mut html);
        assert_eq!(
            html,
            " data-sc-a=\"ascii\" data-sc-😀=\"astral\" data-sc-\u{e000}=\"bmp\""
        );
    }

    #[test]
    fn attributes_extract_tooltip_and_html_attrs() {
        let renderer = Renderer::new();
        let raw = r#"{"href":"a&b","colSpan":3.8,"rowSpan":2,"title":"<tip>","lang":"ja","data":{"fooBar":"a\"b","ignored":true}}"#;
        let mut state = ElementRenderState::default();
        renderer.add_element_attributes_value(&v(raw), &mut state);
        assert_eq!(
            state.attributes,
            " href=\"a&amp;b\" data-sc-foo-bar=\"a&quot;b\" colspan=\"3\" rowspan=\"2\" title=\"&lt;tip&gt;\" lang=\"ja\""
        );
        assert_eq!(state.title_tooltip, "<tip>");
    }

    #[test]
    fn ruby_and_anchor_scans_preserve_source_depth() {
        let raw = r#"{"tag":"ruby","content":["漢",{"tag":"RP","content":"("},{"tag":"RT","content":["か",{"tag":"span","content":"ん"}]},"字",{"tag":"rt","content":"じ"}]}"#;
        let ruby = v(raw);
        let parts = split_ruby_content_value(&ruby["content"]);
        assert_eq!(parts.reading, "かんじ");
        assert_eq!(parts.base.len(), 2);
        assert!(contains_ruby_value(&ruby));

        let raised_raw =
            r#"{"tag":"a","content":["base",{"tag":"span","style":{"verticalAlign":"SuPeR"}}]}"#;
        assert!(anchor_needs_custom_handling_value(
            &v(raised_raw),
            &Context::default()
        ));
    }

    #[test]
    fn details_ids_and_direct_child_count_work() {
        let renderer = Renderer::new();
        let nodes = v(
            r#"[[null,"text",{"tag":"SPAN","content":"hello"},[{"tag":"details","content":[{"tag":"summary","content":"show"},{"tag":"details","content":"hidden"}]}],{"tag":"future","content":{"tag":"li"}}]]"#,
        );

        let mut direct = Vec::new();
        append_direct_content_values_value(&nodes, &mut direct);
        assert_eq!(direct.len(), 4);
        assert_eq!(renderer.structured_element_child_count_value(&nodes), 2);

        let mut stylesheet = ParsedStylesheet::default();
        stylesheet.uses_element_child_count = true;
        stylesheet.element_child_count_tags.insert("details".into());
        let mut ctx = Context {
            dictionary_styles: Some(Arc::new(stylesheet)),
            ..Context::default()
        };
        assert!(renderer.needs_structured_element_child_count_value(&nodes, &ctx));

        let hidden = v(
            r#"{"tag":"div","content":{"tag":"details","content":{"tag":"DETAILS","content":[]}}}"#,
        );
        renderer.skip_closed_details_content_value(&hidden, &mut ctx);
        assert_eq!(ctx.next_detail_id, 2);
        ctx.detail_states.insert("2".into(), false);
        let details = renderer.details_state_value(&v(r#"{"open":true}"#), &mut ctx);
        assert_eq!(
            details,
            DetailsState {
                id: "2".into(),
                open: false
            }
        );
        assert_eq!(ctx.next_detail_id, 3);
    }

    #[test]
    fn inline_label_predicates_match_transparent_arrays_and_box_styles() {
        let label = v(r#"{"tag":"SpAn","content":"label","style":{"borderLeft":"1px solid"}}"#);
        assert!(is_inline_box_label_value(&label));
        assert!(is_inline_structured_content_value(&v(
            r#"["a",[{"tag":"ruby"},{"tag":"IMG"}]]"#
        )));
        assert!(!is_inline_structured_content_value(&v(
            r#"[{"tag":"span"},{"tag":"div"}]"#
        )));
    }
}

//! Yomitan structured-content HTML and inline CSS rendering.

use std::collections::BTreeSet;
use std::fmt::Write as _;

use serde_json::{Map, Value};

use crate::glossary::escape::{
    append_css_style_value, append_css_url_in_attr, append_escaped, append_number, is_safe_href,
    is_valid_image_rendering, is_valid_size_unit,
};
use crate::glossary::media::add_file;

/// Maximum recursion depth for structured-content arrays and elements.
pub const MAX_STRUCTURED_CONTENT_DEPTH: usize = 64;

/// Returns `true` if `tag` is in the exact lowercase Yomitan structured-content whitelist.
pub fn is_supported_structured_tag(tag: &str) -> bool {
    matches!(
        tag,
        "br" | "ruby"
            | "rt"
            | "rp"
            | "table"
            | "thead"
            | "tbody"
            | "tfoot"
            | "tr"
            | "td"
            | "th"
            | "span"
            | "div"
            | "ol"
            | "ul"
            | "li"
            | "details"
            | "summary"
            | "img"
            | "a"
    )
}

fn append_structured_data_attribute_name(key: &str, out: &mut String) {
    out.push_str("data-sc-");
    for ch in key.chars() {
        if ch.is_uppercase() {
            out.push('-');
            out.extend(ch.to_lowercase());
        } else {
            out.push(ch);
        }
    }
}

/// Converts a structured-content `data` key to an HTML attribute name,
/// prefixing `data-sc-` and kebab-casing uppercase Unicode characters.
pub fn structured_data_attribute_name(key: &str) -> String {
    let mut out = String::with_capacity(key.len() + 8);
    append_structured_data_attribute_name(key, &mut out);
    out
}

fn is_safe_html_attr_name_char(ch: char) -> bool {
    !ch.is_ascii_control()
        && !ch.is_whitespace()
        && !matches!(ch, '"' | '\'' | '/' | '<' | '=' | '>')
}

fn add_structured_data(obj: &Map<String, Value>, out: &mut String) {
    for (key, val) in obj {
        let Some(s) = val.as_str() else {
            continue;
        };
        let mark = out.len();
        out.push(' ');
        let name_start = out.len();
        append_structured_data_attribute_name(key, out);
        if !out[name_start..].chars().all(is_safe_html_attr_name_char) {
            out.truncate(mark);
            continue;
        }
        out.push_str("=\"");
        append_escaped(s, out, false);
        out.push('"');
    }
}

enum StyleProp {
    Str(&'static str),
    Margin(&'static str),
    TextDecorationLine,
}

fn style_prop_for_key(key: &str) -> Option<StyleProp> {
    match key {
        "fontStyle" => Some(StyleProp::Str("font-style")),
        "fontWeight" => Some(StyleProp::Str("font-weight")),
        "fontSize" => Some(StyleProp::Str("font-size")),
        "color" => Some(StyleProp::Str("color")),
        "background" => Some(StyleProp::Str("background")),
        "backgroundColor" => Some(StyleProp::Str("background-color")),
        "textDecorationLine" => Some(StyleProp::TextDecorationLine),
        "textDecorationStyle" => Some(StyleProp::Str("text-decoration-style")),
        "textDecorationColor" => Some(StyleProp::Str("text-decoration-color")),
        "borderColor" => Some(StyleProp::Str("border-color")),
        "borderStyle" => Some(StyleProp::Str("border-style")),
        "borderRadius" => Some(StyleProp::Str("border-radius")),
        "borderWidth" => Some(StyleProp::Str("border-width")),
        "clipPath" => Some(StyleProp::Str("clip-path")),
        "verticalAlign" => Some(StyleProp::Str("vertical-align")),
        "textAlign" => Some(StyleProp::Str("text-align")),
        "textEmphasis" => Some(StyleProp::Str("text-emphasis")),
        "textShadow" => Some(StyleProp::Str("text-shadow")),
        "margin" => Some(StyleProp::Str("margin")),
        "marginTop" => Some(StyleProp::Margin("margin-top")),
        "marginLeft" => Some(StyleProp::Margin("margin-left")),
        "marginRight" => Some(StyleProp::Margin("margin-right")),
        "marginBottom" => Some(StyleProp::Margin("margin-bottom")),
        "padding" => Some(StyleProp::Str("padding")),
        "paddingTop" => Some(StyleProp::Str("padding-top")),
        "paddingLeft" => Some(StyleProp::Str("padding-left")),
        "paddingRight" => Some(StyleProp::Str("padding-right")),
        "paddingBottom" => Some(StyleProp::Str("padding-bottom")),
        "wordBreak" => Some(StyleProp::Str("word-break")),
        "whiteSpace" => Some(StyleProp::Str("white-space")),
        "cursor" => Some(StyleProp::Str("cursor")),
        "listStyleType" => Some(StyleProp::Str("list-style-type")),
        _ => None,
    }
}

fn add_structured_style(obj: &Map<String, Value>, out: &mut String) {
    for (key, val) in obj {
        let Some(prop) = style_prop_for_key(key) else {
            continue;
        };
        match prop {
            StyleProp::Str(css_name) => {
                if let Some(s) = val.as_str() {
                    out.push_str(css_name);
                    out.push_str(": ");
                    append_css_style_value(s, out);
                    out.push(';');
                }
            }
            StyleProp::Margin(css_name) => {
                if let Some(s) = val.as_str() {
                    out.push_str(css_name);
                    out.push_str(": ");
                    append_css_style_value(s, out);
                    out.push(';');
                } else if let Some(n) = val.as_f64() {
                    if n.is_finite() {
                        out.push_str(css_name);
                        out.push_str(": ");
                        append_number(out, n);
                        out.push_str("em;");
                    }
                }
            }
            StyleProp::TextDecorationLine => {
                if let Some(arr) = val.as_array() {
                    out.push_str("text-decoration-line: ");
                    for item in arr {
                        let s = item.as_str().unwrap_or("none");
                        append_css_style_value(s, out);
                        out.push(' ');
                    }
                    out.push(';');
                } else if let Some(s) = val.as_str() {
                    out.push_str("text-decoration-line: ");
                    append_css_style_value(s, out);
                    out.push(';');
                }
            }
        }
    }
}

fn add_structured_img<F>(
    obj: &Map<String, Value>,
    basepath: &str,
    out: &mut String,
    file_map: &mut BTreeSet<(String, String)>,
    md5_calc: &mut F,
) where
    F: FnMut(&str) -> Option<String>,
{
    let collapsible = obj
        .get("collapsible")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let collapsed = obj
        .get("collapsed")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    if collapsible {
        out.push_str("<details");
        if !collapsed {
            out.push_str(" open");
        }
        out.push('>');
    }

    let path = obj.get("path").and_then(Value::as_str).unwrap_or("");
    let filename = add_file(basepath, path, file_map, md5_calc);

    out.push_str("<img src=\"");
    if let Some(ref name) = filename {
        append_escaped(name, out, false);
    }
    out.push('"');

    if let Some(data_obj) = obj.get("data").and_then(Value::as_object) {
        add_structured_data(data_obj, out);
    }

    if let Some(title) = obj.get("title").and_then(Value::as_str) {
        out.push_str(" title=\"");
        append_escaped(title, out, false);
        out.push('"');
    }

    if let Some(alt) = obj.get("alt").and_then(Value::as_str) {
        out.push_str(" alt=\"");
        append_escaped(alt, out, false);
        out.push('"');
    }

    if let Some(desc) = obj.get("description").and_then(Value::as_str) {
        out.push_str(" description=\"");
        append_escaped(desc, out, false);
        out.push('"');
    }

    out.push_str(" style=\"");

    let units = obj
        .get("sizeUnits")
        .and_then(Value::as_str)
        .filter(|u| is_valid_size_unit(u))
        .unwrap_or("px");

    if let Some(w) = obj.get("width").and_then(Value::as_f64) {
        if w.is_finite() {
            out.push_str("width: ");
            append_number(out, w);
            out.push_str(units);
            out.push(';');
        }
    }

    if let Some(h) = obj.get("height").and_then(Value::as_f64) {
        if h.is_finite() {
            out.push_str("height: ");
            append_number(out, h);
            out.push_str(units);
            out.push(';');
        }
    }

    if let Some(rend) = obj.get("imageRendering").and_then(Value::as_str) {
        if is_valid_image_rendering(rend) {
            out.push_str("image-rendering: ");
            out.push_str(rend);
            out.push(';');
        }
    } else if obj
        .get("pixelated")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        out.push_str("image-rendering: pixelated;");
    }

    let is_monochrome = obj
        .get("appearance")
        .and_then(Value::as_str)
        .is_some_and(|a| a == "monochrome");

    if is_monochrome {
        out.push_str("object-fit: fill;");
        out.push_str("object-position: -9999999px 9999999px;");
        out.push_str("background-color: currentColor;");

        if let Some(ref name) = filename {
            out.push_str("-webkit-mask-image: url('");
            append_css_url_in_attr(name, out);
            out.push_str("');");
            out.push_str("-webkit-mask-repeat: no-repeat;");
            out.push_str("-webkit-mask-size: contain;");
            out.push_str("-webkit-mask-position: center;");

            out.push_str("mask-image: url('");
            append_css_url_in_attr(name, out);
            out.push_str("');");
            out.push_str("mask-repeat: no-repeat;");
            out.push_str("mask-size: contain;");
            out.push_str("mask-position: center;");
        }
    } else if obj.get("background").and_then(Value::as_bool) != Some(false) {
        out.push_str("background-color: currentColor;");
    }

    if let Some(va) = obj.get("verticalAlign").and_then(Value::as_str) {
        out.push_str("vertical-align: ");
        append_css_style_value(va, out);
        out.push(';');
    }

    if let Some(border) = obj.get("border").and_then(Value::as_str) {
        out.push_str("border: ");
        append_css_style_value(border, out);
        out.push(';');
    }

    if let Some(br) = obj.get("borderRadius").and_then(Value::as_str) {
        out.push_str("border-radius: ");
        append_css_style_value(br, out);
        out.push(';');
    }

    out.push_str("\">");

    if collapsible {
        out.push_str("</details>");
    }
}

fn add_structured_content_helper_obj<F>(
    obj: &Map<String, Value>,
    basepath: &str,
    out: &mut String,
    file_map: &mut BTreeSet<(String, String)>,
    md5_calc: &mut F,
    depth: usize,
) where
    F: FnMut(&str) -> Option<String>,
{
    let tag = obj.get("tag").and_then(Value::as_str).unwrap_or("");
    if !is_supported_structured_tag(tag) {
        return;
    }

    if tag == "br" {
        out.push_str("<br");
        if let Some(data_obj) = obj.get("data").and_then(Value::as_object) {
            add_structured_data(data_obj, out);
        }
        out.push('>');
    } else if tag == "img" {
        add_structured_img(obj, basepath, out, file_map, md5_calc);
    } else {
        out.push('<');
        out.push_str(tag);

        if obj.get("open").and_then(Value::as_bool).unwrap_or(false) {
            out.push_str(" open");
        }

        if let Some(href) = obj.get("href").and_then(Value::as_str) {
            if is_safe_href(href) {
                out.push_str(" href=\"");
                append_escaped(href, out, false);
                out.push('"');
            }
        }

        if let Some(data_obj) = obj.get("data").and_then(Value::as_object) {
            add_structured_data(data_obj, out);
        }

        if let Some(colspan) = obj.get("colSpan").and_then(Value::as_f64) {
            if colspan.is_finite() {
                let _ = write!(out, " colspan=\"{}\"", colspan as i32);
            }
        }

        if let Some(rowspan) = obj.get("rowSpan").and_then(Value::as_f64) {
            if rowspan.is_finite() {
                let _ = write!(out, " rowspan=\"{}\"", rowspan as i32);
            }
        }

        if let Some(title) = obj.get("title").and_then(Value::as_str) {
            out.push_str(" title=\"");
            append_escaped(title, out, false);
            out.push('"');
        }

        if let Some(lang) = obj.get("lang").and_then(Value::as_str) {
            out.push_str(" lang=\"");
            append_escaped(lang, out, false);
            out.push('"');
        }

        if let Some(style_obj) = obj.get("style").and_then(Value::as_object) {
            out.push_str(" style=\"");
            add_structured_style(style_obj, out);
            out.push('"');
        }

        out.push('>');

        if let Some(content) = obj.get("content") {
            add_structured_content(content, basepath, out, file_map, md5_calc, depth + 1);
        }

        out.push_str("</");
        out.push_str(tag);
        out.push('>');
    }
}

pub(crate) fn add_structured_content<F>(
    val: &Value,
    basepath: &str,
    out: &mut String,
    file_map: &mut BTreeSet<(String, String)>,
    md5_calc: &mut F,
    depth: usize,
) where
    F: FnMut(&str) -> Option<String>,
{
    if depth >= MAX_STRUCTURED_CONTENT_DEPTH {
        return;
    }
    match val {
        Value::String(s) => {
            append_escaped(s, out, true);
        }
        Value::Array(arr) => {
            for item in arr {
                add_structured_content(item, basepath, out, file_map, md5_calc, depth + 1);
            }
        }
        Value::Object(obj) => {
            add_structured_content_helper_obj(obj, basepath, out, file_map, md5_calc, depth);
        }
        _ => {}
    }
}

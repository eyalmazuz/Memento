//! Yomitan structured-content HTML and inline CSS rendering.

use std::collections::BTreeSet;

use crate::glossary::escape::{
    append_css_style_value, append_css_url_in_attr, append_escaped, append_i32_u16,
    append_qstring_number, ascii_to_u16, is_safe_href, is_valid_image_rendering,
    is_valid_size_unit, push_ascii,
};
use crate::glossary::media::add_file;
use crate::json::{JsonObject, JsonValue};
use crate::strings::qt_is_space;

/// Maximum recursion depth for structured-content arrays and elements.
pub const MAX_STRUCTURED_CONTENT_DEPTH: usize = 64;

const KEY_ALT: [u16; 3] = ascii_to_u16("alt");
const KEY_APPEARANCE: [u16; 10] = ascii_to_u16("appearance");
const KEY_BACKGROUND: [u16; 10] = ascii_to_u16("background");
const KEY_BORDER: [u16; 6] = ascii_to_u16("border");
const KEY_BORDER_RADIUS: [u16; 12] = ascii_to_u16("borderRadius");
const KEY_COLLAPSED: [u16; 9] = ascii_to_u16("collapsed");
const KEY_COLLAPSIBLE: [u16; 11] = ascii_to_u16("collapsible");
const KEY_COLSPAN: [u16; 7] = ascii_to_u16("colSpan");
const KEY_CONTENT: [u16; 7] = ascii_to_u16("content");
const KEY_DATA: [u16; 4] = ascii_to_u16("data");
const KEY_DESCRIPTION: [u16; 11] = ascii_to_u16("description");
const KEY_HEIGHT: [u16; 6] = ascii_to_u16("height");
const KEY_HREF: [u16; 4] = ascii_to_u16("href");
const KEY_LANG: [u16; 4] = ascii_to_u16("lang");
const KEY_OPEN: [u16; 4] = ascii_to_u16("open");
const KEY_PATH: [u16; 4] = ascii_to_u16("path");
const KEY_PIXELATED: [u16; 9] = ascii_to_u16("pixelated");
const KEY_RENDERING: [u16; 14] = ascii_to_u16("imageRendering");
const KEY_ROWSPAN: [u16; 7] = ascii_to_u16("rowSpan");
const KEY_STYLE: [u16; 5] = ascii_to_u16("style");
const KEY_TAG: [u16; 3] = ascii_to_u16("tag");
const KEY_TITLE: [u16; 5] = ascii_to_u16("title");
const KEY_UNITS: [u16; 9] = ascii_to_u16("sizeUnits");
const KEY_VERT_ALIGN: [u16; 13] = ascii_to_u16("verticalAlign");
const KEY_WIDTH: [u16; 5] = ascii_to_u16("width");

const STR_MONOCHROME: [u16; 10] = ascii_to_u16("monochrome");
const STR_NONE: [u16; 4] = ascii_to_u16("none");
const STR_PX: [u16; 2] = ascii_to_u16("px");

/// `GlossaryBuilder::isSupportedStructuredTag`: only these exact lowercase
/// tags are permitted, without HTML case folding or UTF-16 normalization.
pub fn is_supported_structured_tag(tag: &[u16]) -> bool {
    const TAGS: [&[u8]; 20] = [
        b"br", b"ruby", b"rt", b"rp", b"table", b"thead", b"tbody", b"tfoot", b"tr", b"td", b"th",
        b"span", b"div", b"ol", b"ul", b"li", b"details", b"summary", b"img", b"a",
    ];
    TAGS.iter().any(|name| {
        tag.len() == name.len()
            && tag
                .iter()
                .zip(*name)
                .all(|(&u, &ascii)| u == u16::from(ascii))
    })
}

fn append_structured_data_attribute_name(key: &[u16], out: &mut Vec<u16>) {
    push_ascii(out, "data-sc-");
    let mut utf16_buf = [0u16; 2];
    for decoded in char::decode_utf16(key.iter().copied()) {
        match decoded {
            Ok(ch) if ch.is_uppercase() => {
                out.push(b'-' as u16);
                for lower in ch.to_lowercase() {
                    out.extend_from_slice(lower.encode_utf16(&mut utf16_buf));
                }
            }
            Ok(ch) => {
                out.extend_from_slice(ch.encode_utf16(&mut utf16_buf));
            }
            Err(unpaired) => {
                out.push(unpaired.unpaired_surrogate());
            }
        }
    }
}

/// Convert a structured data key to an HTML data attribute name
/// (`GlossaryBuilder::structuredDataAttributeName`), prefixing `data-sc-` and
/// kebab-casing uppercase Unicode scalar values via `char::to_lowercase()`.
pub fn structured_data_attribute_name(key: &[u16]) -> Vec<u16> {
    let mut out = Vec::with_capacity(key.len() + 8);
    append_structured_data_attribute_name(key, &mut out);
    out
}

fn is_safe_html_attr_name_unit(unit: u16) -> bool {
    unit > 0x20
        && unit != 0x7f
        && !qt_is_space(unit)
        && !matches!(unit, 0x22 | 0x27 | 0x2f | 0x3c | 0x3d | 0x3e)
}

fn add_structured_data(obj: &JsonObject, out: &mut Vec<u16>) {
    for (key, val) in obj.iter() {
        let Some(s) = val.as_qstring() else {
            continue;
        };
        let mark = out.len();
        out.push(b' ' as u16);
        let name_start = out.len();
        append_structured_data_attribute_name(key, out);
        if !out[name_start..]
            .iter()
            .all(|&u| is_safe_html_attr_name_unit(u))
        {
            out.truncate(mark);
            continue;
        }
        push_ascii(out, "=\"");
        append_escaped(s, out, false);
        out.push(0x22);
    }
}

enum StyleProp {
    Str(&'static str),
    Margin(&'static str),
    TextDecorationLine,
}

fn style_prop_for_key(key: &[u16]) -> Option<StyleProp> {
    if key.len() > 24 {
        return None;
    }
    let mut buf = [0u8; 24];
    for (dst, &unit) in buf.iter_mut().zip(key) {
        if unit > 0x7f {
            return None;
        }
        *dst = unit as u8;
    }
    match &buf[..key.len()] {
        b"fontStyle" => Some(StyleProp::Str("font-style")),
        b"fontWeight" => Some(StyleProp::Str("font-weight")),
        b"fontSize" => Some(StyleProp::Str("font-size")),
        b"color" => Some(StyleProp::Str("color")),
        b"background" => Some(StyleProp::Str("background")),
        b"backgroundColor" => Some(StyleProp::Str("background-color")),
        b"textDecorationLine" => Some(StyleProp::TextDecorationLine),
        b"textDecorationStyle" => Some(StyleProp::Str("text-decoration-style")),
        b"textDecorationColor" => Some(StyleProp::Str("text-decoration-color")),
        b"borderColor" => Some(StyleProp::Str("border-color")),
        b"borderStyle" => Some(StyleProp::Str("border-style")),
        b"borderRadius" => Some(StyleProp::Str("border-radius")),
        b"borderWidth" => Some(StyleProp::Str("border-width")),
        b"clipPath" => Some(StyleProp::Str("clip-path")),
        b"verticalAlign" => Some(StyleProp::Str("vertical-align")),
        b"textAlign" => Some(StyleProp::Str("text-align")),
        b"textEmphasis" => Some(StyleProp::Str("text-emphasis")),
        b"textShadow" => Some(StyleProp::Str("text-shadow")),
        b"margin" => Some(StyleProp::Str("margin")),
        b"marginTop" => Some(StyleProp::Margin("margin-top")),
        b"marginLeft" => Some(StyleProp::Margin("margin-left")),
        b"marginRight" => Some(StyleProp::Margin("margin-right")),
        b"marginBottom" => Some(StyleProp::Margin("margin-bottom")),
        b"padding" => Some(StyleProp::Str("padding")),
        b"paddingTop" => Some(StyleProp::Str("padding-top")),
        b"paddingLeft" => Some(StyleProp::Str("padding-left")),
        b"paddingRight" => Some(StyleProp::Str("padding-right")),
        b"paddingBottom" => Some(StyleProp::Str("padding-bottom")),
        b"wordBreak" => Some(StyleProp::Str("word-break")),
        b"whiteSpace" => Some(StyleProp::Str("white-space")),
        b"cursor" => Some(StyleProp::Str("cursor")),
        b"listStyleType" => Some(StyleProp::Str("list-style-type")),
        _ => None,
    }
}

fn add_structured_style(obj: &JsonObject, out: &mut Vec<u16>) {
    for (key, val) in obj.iter() {
        let Some(prop) = style_prop_for_key(key) else {
            continue;
        };
        match prop {
            StyleProp::Str(css_name) => {
                if let Some(s) = val.as_qstring() {
                    push_ascii(out, css_name);
                    push_ascii(out, ": ");
                    append_css_style_value(s, out);
                    out.push(b';' as u16);
                }
            }
            StyleProp::Margin(css_name) => {
                if let Some(s) = val.as_qstring() {
                    push_ascii(out, css_name);
                    push_ascii(out, ": ");
                    append_css_style_value(s, out);
                    out.push(b';' as u16);
                } else if let JsonValue::Number(n) = val {
                    if n.is_finite() {
                        push_ascii(out, css_name);
                        push_ascii(out, ": ");
                        append_qstring_number(out, *n);
                        push_ascii(out, "em;");
                    }
                }
            }
            StyleProp::TextDecorationLine => {
                if let JsonValue::Array(arr) = val {
                    push_ascii(out, "text-decoration-line: ");
                    for item in arr {
                        let s = item.as_qstring().unwrap_or(STR_NONE.as_slice());
                        append_css_style_value(s, out);
                        out.push(b' ' as u16);
                    }
                    out.push(b';' as u16);
                } else if let Some(s) = val.as_qstring() {
                    push_ascii(out, "text-decoration-line: ");
                    append_css_style_value(s, out);
                    out.push(b';' as u16);
                }
            }
        }
    }
}

fn add_structured_img<F>(
    obj: &JsonObject,
    basepath: &[u16],
    out: &mut Vec<u16>,
    file_map: &mut BTreeSet<(Vec<u16>, Vec<u16>)>,
    md5_calc: &mut F,
) where
    F: FnMut(&[u16]) -> Option<[u16; 32]>,
{
    let collapsible = matches!(
        obj.get(KEY_COLLAPSIBLE.as_slice()),
        Some(JsonValue::Bool(true))
    );
    let collapsed = matches!(
        obj.get(KEY_COLLAPSED.as_slice()),
        Some(JsonValue::Bool(true))
    );

    if collapsible {
        push_ascii(out, "<details");
        if !collapsed {
            push_ascii(out, " open");
        }
        out.push(b'>' as u16);
    }

    let path = obj
        .get(KEY_PATH.as_slice())
        .and_then(JsonValue::as_qstring)
        .unwrap_or(&[]);
    let filename = add_file(basepath, path, file_map, md5_calc);

    push_ascii(out, "<img src=\"");
    if let Some(ref name) = filename {
        append_escaped(name, out, false);
    }
    out.push(0x22);

    if let Some(data_obj) = obj.get(KEY_DATA.as_slice()).and_then(JsonValue::as_object) {
        add_structured_data(data_obj, out);
    }

    if let Some(title) = obj
        .get(KEY_TITLE.as_slice())
        .and_then(JsonValue::as_qstring)
    {
        push_ascii(out, " title=\"");
        append_escaped(title, out, false);
        out.push(0x22);
    }

    if let Some(alt) = obj.get(KEY_ALT.as_slice()).and_then(JsonValue::as_qstring) {
        push_ascii(out, " alt=\"");
        append_escaped(alt, out, false);
        out.push(0x22);
    }

    if let Some(desc) = obj
        .get(KEY_DESCRIPTION.as_slice())
        .and_then(JsonValue::as_qstring)
    {
        push_ascii(out, " description=\"");
        append_escaped(desc, out, false);
        out.push(0x22);
    }

    push_ascii(out, " style=\"");

    let units = obj
        .get(KEY_UNITS.as_slice())
        .and_then(JsonValue::as_qstring)
        .filter(|u| is_valid_size_unit(u))
        .unwrap_or(STR_PX.as_slice());

    if let Some(JsonValue::Number(w)) = obj.get(KEY_WIDTH.as_slice()) {
        if w.is_finite() {
            push_ascii(out, "width: ");
            append_qstring_number(out, *w);
            out.extend_from_slice(units);
            out.push(b';' as u16);
        }
    }

    if let Some(JsonValue::Number(h)) = obj.get(KEY_HEIGHT.as_slice()) {
        if h.is_finite() {
            push_ascii(out, "height: ");
            append_qstring_number(out, *h);
            out.extend_from_slice(units);
            out.push(b';' as u16);
        }
    }

    if let Some(rend) = obj
        .get(KEY_RENDERING.as_slice())
        .and_then(JsonValue::as_qstring)
    {
        if is_valid_image_rendering(rend) {
            push_ascii(out, "image-rendering: ");
            out.extend_from_slice(rend);
            out.push(b';' as u16);
        }
    } else if matches!(
        obj.get(KEY_PIXELATED.as_slice()),
        Some(JsonValue::Bool(true))
    ) {
        push_ascii(out, "image-rendering: pixelated;");
    }

    let appearance = obj
        .get(KEY_APPEARANCE.as_slice())
        .and_then(JsonValue::as_qstring);
    let is_monochrome = appearance == Some(STR_MONOCHROME.as_slice());

    if is_monochrome {
        push_ascii(out, "object-fit: fill;");
        push_ascii(out, "object-position: -9999999px 9999999px;");
        push_ascii(out, "background-color: currentColor;");

        if let Some(ref name) = filename {
            push_ascii(out, "-webkit-mask-image: url('");
            append_css_url_in_attr(name, out);
            push_ascii(out, "');");
            push_ascii(out, "-webkit-mask-repeat: no-repeat;");
            push_ascii(out, "-webkit-mask-size: contain;");
            push_ascii(out, "-webkit-mask-position: center;");

            push_ascii(out, "mask-image: url('");
            append_css_url_in_attr(name, out);
            push_ascii(out, "');");
            push_ascii(out, "mask-repeat: no-repeat;");
            push_ascii(out, "mask-size: contain;");
            push_ascii(out, "mask-position: center;");
        }
    } else if !matches!(
        obj.get(KEY_BACKGROUND.as_slice()),
        Some(JsonValue::Bool(false))
    ) {
        push_ascii(out, "background-color: currentColor;");
    }

    if let Some(va) = obj
        .get(KEY_VERT_ALIGN.as_slice())
        .and_then(JsonValue::as_qstring)
    {
        push_ascii(out, "vertical-align: ");
        append_css_style_value(va, out);
        out.push(b';' as u16);
    }

    if let Some(border) = obj
        .get(KEY_BORDER.as_slice())
        .and_then(JsonValue::as_qstring)
    {
        push_ascii(out, "border: ");
        append_css_style_value(border, out);
        out.push(b';' as u16);
    }

    if let Some(br) = obj
        .get(KEY_BORDER_RADIUS.as_slice())
        .and_then(JsonValue::as_qstring)
    {
        push_ascii(out, "border-radius: ");
        append_css_style_value(br, out);
        out.push(b';' as u16);
    }

    push_ascii(out, "\">");

    if collapsible {
        push_ascii(out, "</details>");
    }
}

fn add_structured_content_helper_obj<F>(
    obj: &JsonObject,
    basepath: &[u16],
    out: &mut Vec<u16>,
    file_map: &mut BTreeSet<(Vec<u16>, Vec<u16>)>,
    md5_calc: &mut F,
    depth: usize,
) where
    F: FnMut(&[u16]) -> Option<[u16; 32]>,
{
    let tag = obj
        .get(KEY_TAG.as_slice())
        .and_then(JsonValue::as_qstring)
        .unwrap_or(&[]);
    if !is_supported_structured_tag(tag) {
        return;
    }

    const TAG_BR: [u16; 2] = ascii_to_u16("br");
    const TAG_IMG: [u16; 3] = ascii_to_u16("img");

    if tag == TAG_BR.as_slice() {
        out.push(b'<' as u16);
        out.extend_from_slice(tag);
        if let Some(data_obj) = obj.get(KEY_DATA.as_slice()).and_then(JsonValue::as_object) {
            add_structured_data(data_obj, out);
        }
        out.push(b'>' as u16);
    } else if tag == TAG_IMG.as_slice() {
        add_structured_img(obj, basepath, out, file_map, md5_calc);
    } else {
        out.push(b'<' as u16);
        out.extend_from_slice(tag);

        if matches!(obj.get(KEY_OPEN.as_slice()), Some(JsonValue::Bool(true))) {
            push_ascii(out, " open");
        }

        if let Some(href) = obj.get(KEY_HREF.as_slice()).and_then(JsonValue::as_qstring) {
            if is_safe_href(href) {
                push_ascii(out, " href=\"");
                append_escaped(href, out, false);
                out.push(0x22);
            }
        }

        if let Some(data_obj) = obj.get(KEY_DATA.as_slice()).and_then(JsonValue::as_object) {
            add_structured_data(data_obj, out);
        }

        if let Some(JsonValue::Number(colspan)) = obj.get(KEY_COLSPAN.as_slice()) {
            if colspan.is_finite() {
                push_ascii(out, " colspan=\"");
                append_i32_u16(out, *colspan as i32);
                out.push(0x22);
            }
        }

        if let Some(JsonValue::Number(rowspan)) = obj.get(KEY_ROWSPAN.as_slice()) {
            if rowspan.is_finite() {
                push_ascii(out, " rowspan=\"");
                append_i32_u16(out, *rowspan as i32);
                out.push(0x22);
            }
        }

        if let Some(title) = obj
            .get(KEY_TITLE.as_slice())
            .and_then(JsonValue::as_qstring)
        {
            push_ascii(out, " title=\"");
            append_escaped(title, out, false);
            out.push(0x22);
        }

        if let Some(lang) = obj.get(KEY_LANG.as_slice()).and_then(JsonValue::as_qstring) {
            push_ascii(out, " lang=\"");
            append_escaped(lang, out, false);
            out.push(0x22);
        }

        if let Some(style_obj) = obj.get(KEY_STYLE.as_slice()).and_then(JsonValue::as_object) {
            push_ascii(out, " style=\"");
            add_structured_style(style_obj, out);
            out.push(0x22);
        }

        out.push(b'>' as u16);

        if let Some(content) = obj.get(KEY_CONTENT.as_slice()) {
            add_structured_content(content, basepath, out, file_map, md5_calc, depth + 1);
        }

        push_ascii(out, "</");
        out.extend_from_slice(tag);
        out.push(b'>' as u16);
    }
}

pub(crate) fn add_structured_content<F>(
    val: &JsonValue,
    basepath: &[u16],
    out: &mut Vec<u16>,
    file_map: &mut BTreeSet<(Vec<u16>, Vec<u16>)>,
    md5_calc: &mut F,
    depth: usize,
) where
    F: FnMut(&[u16]) -> Option<[u16; 32]>,
{
    if depth >= MAX_STRUCTURED_CONTENT_DEPTH {
        return;
    }
    match val {
        JsonValue::String(s) => {
            append_escaped(s, out, true);
        }
        JsonValue::Array(arr) => {
            for item in arr {
                add_structured_content(item, basepath, out, file_map, md5_calc, depth + 1);
            }
        }
        JsonValue::Object(obj) => {
            add_structured_content_helper_obj(obj, basepath, out, file_map, md5_calc, depth);
        }
        _ => {}
    }
}

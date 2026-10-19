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

//! Structured-content traversal and HTML assembly from
//! `structuredrichtext_content.cpp`. Screen/font/color values arrive through
//! `Context`; resource URLs and Qt locale direction arrive through callbacks
//! at the application boundary.

use crate::css::Renderer;
use crate::element::{
    Context, CssDeclarations, ElementLayout, ElementRenderState, LayoutDirection, MarginFlow,
    SiblingState, StructuredList, BOTTOM, TOP,
};
use crate::helpers::{css_font_size_to_pixels, format_pixel_size};
use crate::json::Json;
use crate::qstring::{equals_ascii_ci, qstring_number, to_lower};

/// The three display modes in `Setting::GlossaryStyle` (`setting/keys.h`),
/// plus the no-separator behavior of out-of-range C enum values.
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

/// Ruby base nodes retain source references, including nested arrays, exactly
/// as `splitRubyContent` does; `rt` readings and `rp` parentheses are removed.
#[derive(Debug, PartialEq)]
pub struct RubyParts<'a> {
    pub base: Vec<&'a Json>,
    pub reading: String,
}

/// `QString::toHtmlEscaped()` escapes four characters, not apostrophes.
/// Keeping this as an append avoids copies for unescaped spans of text.
pub(crate) fn append_html_escaped(text: &str, out: &mut String, newlines_as_breaks: bool) {
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

fn append_percent_encoded(text: &str, out: &mut String) {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
            out.push(byte as char);
        } else {
            out.push('%');
            out.push(HEX[(byte >> 4) as usize] as char);
            out.push(HEX[(byte & 15) as usize] as char);
        }
    }
}

/// Flatten only transparent direct arrays, skipping null/undefined; an object
/// is one element regardless of the type of its own `content` property.
pub fn append_direct_content_values<'a>(value: &'a Json, out: &mut Vec<&'a Json>) {
    match value {
        Json::Array(children) => {
            for child in children {
                append_direct_content_values(child, out);
            }
        }
        Json::Null | Json::Missing => {}
        _ => out.push(value),
    }
}

/// `hasInlineBoxStyle` (L113-126).
pub fn has_inline_box_style(style: &Json) -> bool {
    match style {
        Json::Object(properties) => properties.keys().any(|key| {
            let key = to_lower(key);
            key == "backgroundcolor" || key.starts_with("border") || key.starts_with("padding")
        }),
        _ => false,
    }
}

/// `isInlineBoxLabel` (L135-150).
pub fn is_inline_box_label(value: &Json) -> bool {
    value.is_object()
        && equals_ascii_ci(value.get("tag").as_str(), "span")
        && value.get("content").is_string()
        && has_inline_box_style(value.get("style"))
}

/// `isInlineStructuredContent` (L159-186), used for inline-label rows.
pub fn is_inline_structured_content(value: &Json) -> bool {
    match value {
        Json::Array(children) => children.iter().all(is_inline_structured_content),
        Json::Object(_) => matches!(
            to_lower(value.get("tag").as_str()).as_str(),
            "a" | "br" | "img" | "rp" | "rt" | "ruby" | "span"
        ),
        _ => true,
    }
}

/// `structuredContentText` (L1386-1413). Ignores tags, attributes and
/// non-text leaves while recursively traversing object `content`.
pub fn structured_content_text(value: &Json, out: &mut String) {
    match value {
        Json::String(text) => out.push_str(text),
        Json::Array(children) => {
            for child in children {
                structured_content_text(child, out);
            }
        }
        Json::Object(_) => structured_content_text(value.get("content"), out),
        _ => {}
    }
}

/// `splitRubyContent` (L1330-1384). Only direct children that are exactly
/// `rt` or `rp` are special; nested arrays remain base nodes.
pub fn split_ruby_content(value: &Json) -> RubyParts<'_> {
    let mut parts = RubyParts {
        base: Vec::new(),
        reading: String::new(),
    };
    let children = match value {
        Json::Array(children) => children.as_slice(),
        _ => std::slice::from_ref(value),
    };
    for child in children {
        if child.is_object() {
            match child.get("tag").as_str() {
                "rt" => {
                    structured_content_text(child.get("content"), &mut parts.reading);
                    continue;
                }
                "rp" => continue,
                _ => {}
            }
        }
        parts.base.push(child);
    }
    parts
}

/// `containsRuby` (L1415-1448); tag matching here is case-sensitive.
pub fn contains_ruby(value: &Json) -> bool {
    match value {
        Json::Array(children) => children.iter().any(contains_ruby),
        Json::Object(_) => value.get("tag").as_str() == "ruby"
            || contains_ruby(value.get("content")),
        _ => false,
    }
}

/// `containsRaisedContent` (L1450-1483).
pub fn contains_raised_content(value: &Json) -> bool {
    match value {
        Json::Array(children) => children.iter().any(contains_raised_content),
        Json::Object(_) => {
            let alignment = value.get("style").get("verticalAlign").as_str();
            equals_ascii_ci(alignment, "super")
                || equals_ascii_ci(alignment, "sub")
                || contains_raised_content(value.get("content"))
        }
        _ => false,
    }
}

/// `anchorNeedsCustomHandling` (L1485-1497), including inherited titles.
pub fn anchor_needs_custom_handling(value: &Json, ctx: &Context) -> bool {
    contains_ruby(value.get("content"))
        || contains_raised_content(value.get("content"))
        || value.get("title").is_string()
        || !ctx.title_tooltip.is_empty()
}

impl Renderer {
    /// `containsStructuredContent` (L1266-1282), top-level only.
    pub fn contains_structured_content(&self, entries: &[Json]) -> bool {
        entries.iter().any(|value| {
            value.is_object() && value.get("type").as_str() == "structured-content"
        })
    }

    /// `parse` (L192-359). Render both structured and legacy entries in source
    /// order. The caller supplies a fresh context initialized from Qt (font,
    /// colors, parsed dictionary CSS, detail-state overrides and screen) and
    /// resolves every image URL through `structuredImageSource`'s resource /
    /// color-provider seam. `locale_direction` must return
    /// `QLocale(lang).textDirection()` for each explicit element `lang`.
    ///
    /// No URL or locale is guessed when an application callback is required.
    pub fn render_entries(
        &self,
        entries: &[Json],
        style: LegacyGlossaryStyle,
        ctx: &mut Context,
        mut source: impl FnMut(&Json, &Context) -> String,
        mut locale_direction: impl FnMut(&str) -> LayoutDirection,
    ) -> String {
        ctx.needs_element_child_count = ctx.dictionary_styles.as_ref()
            .is_some_and(|styles| styles.uses_element_child_count);
        let contains_structured = self.contains_structured_content(entries);
        let use_bullets = style == LegacyGlossaryStyle::Bullet && !contains_structured;
        let mut out = String::with_capacity(4096);
        out.push_str("<html><head></head><body>");

        for (index, value) in entries.iter().enumerate() {
            if use_bullets {
                out.push_str("<table cellspacing=\"0\" width=\"100%\"><tr><td style=\"vertical-align: top; white-space: nowrap; padding-right: ");
                out.push_str(&format_pixel_size(ctx.parent_font_pixel_size * 0.35));
                out.push_str("px;\">");
                self.append_selection_sentinel('\u{2060}', &mut out);
                self.append_selection_sentinel('\u{2061}', &mut out);
                out.push('\u{2022}'); // `ul` / `disc` fallback marker
                self.append_selection_sentinel('\u{2062}', &mut out);
                out.push_str("</td><td width=\"100%\" style=\"vertical-align: top;\">");
            }

            match value {
                Json::String(text) => append_html_escaped(text, &mut out, true),
                Json::Object(_) => match value.get("type").as_str() {
                    "structured-content" => self.append_structured_children(
                        value.get("content"), ctx, &mut out, &mut source, &mut locale_direction,
                    ),
                    "image" => {
                        let url = source(value, ctx);
                        self.append_legacy_image(value, &url, ctx, &mut out);
                    }
                    "text" => append_html_escaped(value.get("text").as_str(), &mut out, true),
                    _ => {}
                },
                _ => {}
            }
            if use_bullets {
                out.push_str("</td></tr></table>");
            } else if !contains_structured && index + 1 < entries.len() {
                match style {
                    LegacyGlossaryStyle::LineBreak => out.push_str("<br>"),
                    LegacyGlossaryStyle::Pipe => out.push_str(" | "),
                    LegacyGlossaryStyle::Bullet | LegacyGlossaryStyle::NoSeparator => {}
                }
            }
        }
        out.push_str("</body></html>");
        out
    }

    /// `addStructuredData` (L363-379). QJsonObject emits attributes in
    /// QString (UTF-16) key order, which differs from BTreeMap's UTF-8 order
    /// when BMP private-use and supplementary characters are both present.
    pub fn append_structured_data(&self, data: &Json, out: &mut String) {
        if let Json::Object(attributes) = data {
            let mut write_attribute = |key: &str, value: &Json| {
                if let Json::String(value) = value {
                    out.push(' ');
                    out.push_str(&self.structured_data_attribute_name(key));
                    out.push_str("=\"");
                    append_html_escaped(value, out, false);
                    out.push('"');
                }
            };
            if attributes.keys().all(|key| key.is_ascii()) {
                for (key, value) in attributes {
                    write_attribute(key, value);
                }
            } else {
                let mut keys: Vec<_> = attributes.keys().collect();
                keys.sort_unstable_by(|left, right| left.encode_utf16().cmp(right.encode_utf16()));
                for key in keys {
                    write_attribute(key, &attributes[key]);
                }
            }
        }
    }

    /// `addElementAttributes` (L1108-1164), for a layout-resolved element.
    pub fn add_element_attributes(&self, obj: &Json, state: &mut ElementRenderState) {
        let out = &mut state.attributes;
        if let Json::String(href) = obj.get("href") {
            out.push_str(" href=\"");
            append_html_escaped(href, out, false);
            out.push('"');
        }
        self.append_structured_data(obj.get("data"), out);
        for (key, name) in [("colSpan", "colspan"), ("rowSpan", "rowspan")] {
            if let Json::Number(number) = obj.get(key) {
                out.push(' ');
                out.push_str(name);
                out.push_str("=\"");
                out.push_str(&(*number as i32).to_string());
                out.push('"');
            }
        }
        if let Json::String(title) = obj.get("title") {
            state.title_tooltip.clone_from(title);
            out.push_str(" title=\"");
            append_html_escaped(title, out, false);
            out.push('"');
        }
        if let Json::String(lang) = obj.get("lang") {
            out.push_str(" lang=\"");
            append_html_escaped(lang, out, false);
            out.push('"');
        }
    }

    /// `internalLinkHref` (L1499-1548). Qt percent-encodes UTF-8 bytes using
    /// RFC 3986 unreserved characters; an empty value has no query parameter.
    pub fn internal_link_href(
        &self,
        target: &str,
        tooltip: &str,
        tooltip_text: &str,
        tooltip_type: &str,
    ) -> String {
        let mut href = String::from("memento://glossary-link?");
        let mut first = true;
        for (name, value) in [
            ("target", target),
            ("tooltip", tooltip),
            ("tooltipText", tooltip_text),
            ("tooltipType", tooltip_type),
        ] {
            if value.is_empty() {
                continue;
            }
            if !first {
                href.push('&');
            }
            first = false;
            href.push_str(name);
            href.push('=');
            append_percent_encoded(value, &mut href);
        }
        href
    }

    /// `addAnchorStart` (L1550-1577), including Qt's quote substitution in
    /// inline color style and decoration suppression.
    pub fn append_anchor_start(&self, href: &str, color: &str, suppress: bool, out: &mut String) {
        out.push_str("<a href=\"");
        append_html_escaped(href, out, false);
        out.push('"');
        if !color.is_empty() || suppress {
            out.push_str(" style=\"");
            if !color.is_empty() {
                out.push_str("color: ");
                if color.contains('"') {
                    append_html_escaped(&color.replace('"', "'"), out, false);
                } else {
                    append_html_escaped(color, out, false);
                }
                out.push_str("; ");
            }
            out.push_str("text-decoration: none;\"");
        }
        out.push('>');
    }

    /// `addStructuredContentHelper(QString, ctx)` (L387-413), after the
    /// layout renderer has flushed pending vertical margins.
    pub fn append_structured_text(&self, text: &str, ctx: &Context, out: &mut String) {
        if ctx.link_href.is_empty() && ctx.title_tooltip.is_empty() {
            append_html_escaped(text, out, true);
            return;
        }
        if ctx.title_tooltip.is_empty() {
            self.append_anchor_start(&ctx.link_href, "", ctx.suppress_link_decoration, out);
        } else {
            let href = self.internal_link_href(&ctx.link_href, &ctx.title_tooltip, "", "title");
            self.append_anchor_start(
                &href,
                if ctx.link_href.is_empty() { &ctx.text_color } else { "" },
                ctx.suppress_link_decoration,
                out,
            );
        }
        append_html_escaped(text, out, true);
        out.push_str("</a>");
    }

    /// `addImage` (L1192-1255). The source URL was resolved by the caller
    /// through the Qt resource/image-provider seam.
    pub fn append_legacy_image(&self, obj: &Json, source: &str, ctx: &Context, out: &mut String) {
        if let Json::String(title) = obj.get("title") {
            let href = self.internal_link_href("", title, "", "title");
            self.append_anchor_start(&href, &ctx.text_color, false, out);
        }
        out.push_str("<img src=\"");
        append_html_escaped(source, out, false);
        out.push('"');
        for key in ["width", "height"] {
            if let Json::Number(value) = obj.get(key) {
                out.push(' ');
                out.push_str(key);
                out.push_str("=\"");
                out.push_str(&qstring_number(*value));
                out.push('"');
            }
        }
        out.push_str(" style=\"display: inline-table; vertical-align: top;");
        if let Json::String(rendering) = obj.get("imageRendering") {
            out.push_str("image-rendering: ");
            out.push_str(rendering);
            out.push(';');
        }
        out.push_str("\">");
        if obj.get("title").is_string() {
            out.push_str("</a>");
        }
        if let Json::String(description) = obj.get("description") {
            out.push_str("<br>");
            append_html_escaped(description, out, true);
        }
    }

    /// The complete `br` branch (L483-495), after layout has flushed
    /// margins. Registers a selector sibling even though it has no children.
    pub fn append_structured_break(&self, obj: &Json, ctx: &mut Context, out: &mut String) {
        let index = self.structured_element(obj, ctx, false);
        ctx.elements.push(index);
        out.push_str("<br");
        self.append_structured_data(obj.get("data"), out);
        out.push('>');
        ctx.elements.pop();
    }

    /// The custom `a` branch (L461-482) for anchors with ruby, raised content
    /// or inherited titles. Caller checks `anchor_needs_custom_handling` and
    /// flushes margins; `render_children` owns the normal child sibling scope.
    pub fn append_custom_anchor(
        &self,
        obj: &Json,
        ctx: &mut Context,
        out: &mut String,
        render_children: impl FnOnce(&Json, &mut Context, &mut String),
    ) {
        let index = self.structured_element(obj, ctx, false);
        ctx.elements.push(index);
        let old_href = if let Json::String(href) = obj.get("href") {
            Some(std::mem::replace(&mut ctx.link_href, href.clone()))
        } else {
            None
        };
        let old_title = if let Json::String(title) = obj.get("title") {
            Some(std::mem::replace(&mut ctx.title_tooltip, title.clone()))
        } else {
            None
        };
        render_children(obj.get("content"), ctx, out);
        if let Some(title) = old_title {
            ctx.title_tooltip = title;
        }
        if let Some(href) = old_href {
            ctx.link_href = href;
        }
        ctx.elements.pop();
    }

    /// `img` branch (L496-641). Registers the selector before asking the Qt
    /// resource callback for its resolved file/provider URL, just as C++ does.
    pub fn append_structured_image(
        &self,
        obj: &Json,
        mut source: impl FnMut(&Json, &Context) -> String,
        ctx: &mut Context,
        out: &mut String,
    ) {
        let index = self.structured_element(obj, ctx, false);
        ctx.elements.push(index);
        let image_source = source(obj, ctx);

        let image_title = match obj.get("title") {
            Json::String(title) => title.as_str(),
            _ => &ctx.title_tooltip,
        };
        let has_image_title = !image_title.is_empty();
        if has_image_title {
            let href = self.internal_link_href(&ctx.link_href, image_title, "", "title");
            self.append_anchor_start(
                &href,
                if ctx.link_href.is_empty() { &ctx.text_color } else { "" },
                ctx.suppress_link_decoration,
                out,
            );
        }
        out.push_str("<img src=\"");
        append_html_escaped(&image_source, out, false);
        out.push('"');
        self.append_structured_data(obj.get("data"), out);
        for key in ["alt", "description"] {
            if let Json::String(value) = obj.get(key) {
                out.push(' ');
                out.push_str(key);
                out.push_str("=\"");
                append_html_escaped(value, out, false);
                out.push('"');
            }
        }
        let units = match obj.get("sizeUnits") {
            Json::String(value) => value.as_str(),
            _ => "px",
        };
        for key in ["width", "height"] {
            if let Json::Number(value) = obj.get(key) {
                let mut css_size = qstring_number(*value);
                css_size.push_str(units);
                let pixels = css_font_size_to_pixels(
                    &css_size,
                    ctx.screen.as_ref(),
                    ctx.parent_font_pixel_size,
                    ctx.root_font_pixel_size,
                );
                out.push(' ');
                out.push_str(key);
                out.push_str("=\"");
                out.push_str(&format_pixel_size(pixels));
                out.push('"');
            }
        }

        let mut declarations = CssDeclarations::new();
        self.add_matching_css_rules(ctx, &mut declarations, None, None);
        if let Json::String(rendering) = obj.get("imageRendering") {
            self.add_css_declaration("image-rendering", rendering, &mut declarations);
        } else if obj.get("pixelated") == &Json::Bool(true) {
            self.add_css_declaration("image-rendering", "pixelated", &mut declarations);
        }
        if obj.get("background") != &Json::Bool(false) {
            self.add_css_declaration("background-color", "currentColor", &mut declarations);
        }
        for (key, property) in [
            ("verticalAlign", "vertical-align"),
            ("border", "border"),
            ("borderRadius", "border-radius"),
        ] {
            if let Json::String(value) = obj.get(key) {
                self.add_css_declaration(property, value, &mut declarations);
            }
        }
        if !declarations.is_empty() {
            out.push_str(" style=\"");
            self.add_css_declarations(&declarations, out);
            out.push('"');
        }
        out.push('>');
        if has_image_title {
            out.push_str("</a>");
        }
        ctx.elements.pop();
    }

    /// `structuredElementChildCount` (L665-686). Only direct elements and
    /// transparent arrays contribute; the count does not recurse into an
    /// object's `content`.
    pub fn structured_element_child_count(&self, value: &Json) -> i64 {
        match value {
            Json::Array(children) => children.iter()
                .map(|child| self.structured_element_child_count(child)).sum(),
            Json::Object(_) => {
                i64::from(self.is_supported_structured_tag(&to_lower(value.get("tag").as_str())))
            }
            _ => 0,
        }
    }

    /// `needsStructuredElementChildCount` (L688-715). Do not pre-count if
    /// none of the direct child tags needs a `:last-child` count.
    pub fn needs_structured_element_child_count(&self, value: &Json, ctx: &Context) -> bool {
        let Some(styles) = ctx.dictionary_styles.as_ref() else {
            return false;
        };
        match value {
            Json::Array(children) => children.iter()
                .any(|child| self.needs_structured_element_child_count(child, ctx)),
            Json::Object(_) => {
                let tag = to_lower(value.get("tag").as_str());
                self.is_supported_structured_tag(&tag)
                    && styles.needs_element_child_count_for_tag(&tag)
            }
            _ => false,
        }
    }

    /// `detailsState` (L1079-1096). Qt converts QVariantMap override values
    /// via QVariant::toBool; integration supplies those booleans in Context.
    pub fn details_state(&self, obj: &Json, ctx: &mut Context) -> DetailsState {
        let id = ctx.next_detail_id.to_string();
        ctx.next_detail_id += 1;
        let open = ctx.detail_states.get(&id).copied().unwrap_or_else(|| {
            obj.get("open") == &Json::Bool(true)
        });
        DetailsState { id, open }
    }

    /// `detailsLinkHref` (L1098-1106).
    pub fn details_link_href(&self, state: &DetailsState) -> String {
        format!("memento://glossary-details?id={}&open={}", state.id, u8::from(state.open))
    }

    /// `skipClosedDetailsContent` (L746-769), consuming IDs in depth-first
    /// source order for details nested inside an unrendered branch.
    pub fn skip_closed_details_content(&self, value: &Json, ctx: &mut Context) {
        match value {
            Json::Array(children) => {
                for child in children {
                    self.skip_closed_details_content(child, ctx);
                }
            }
            Json::Object(_) => {
                if equals_ascii_ci(value.get("tag").as_str(), "details") {
                    ctx.next_detail_id += 1;
                }
                self.skip_closed_details_content(value.get("content"), ctx);
            }
            _ => {}
        }
    }

    /// `addClosedDetailsContent` (L717-744). Only direct summary children
    /// reach `render_summary`; other branches consume their nested details IDs.
    /// The callback must perform the ordinary layout/selector rendering of a
    /// summary. No body or synthetic replacement is emitted here.
    pub fn append_closed_details_content(
        &self,
        value: &Json,
        ctx: &mut Context,
        out: &mut String,
        render_summary: &mut impl FnMut(&Json, &mut Context, &mut String),
    ) {
        match value {
            Json::Array(children) => {
                for child in children {
                    self.append_closed_details_content(child, ctx, out, render_summary);
                }
            }
            Json::Object(_) if equals_ascii_ci(value.get("tag").as_str(), "summary") => {
                render_summary(value, ctx, out);
            }
            Json::Object(_) => self.skip_closed_details_content(value, ctx),
            _ => {}
        }
    }

    /// `addRuby` (L1289-1328) after layout has flushed pending margins.
    /// Registers the ruby selector itself; `render_base` must add the normal
    /// child sibling scope and render the supplied source nodes.
    pub fn append_ruby(
        &self,
        obj: &Json,
        ctx: &mut Context,
        out: &mut String,
        render_base: impl FnOnce(&[&Json], &mut Context, &mut String),
    ) {
        let index = self.structured_element(obj, ctx, false);
        ctx.elements.push(index);
        let parts = split_ruby_content(obj.get("content"));
        if parts.reading.is_empty() {
            render_base(&parts.base, ctx, out);
            ctx.elements.pop();
            return;
        }
        let mut tooltip_text = String::new();
        for node in &parts.base {
            structured_content_text(node, &mut tooltip_text);
        }
        let href = self.internal_link_href(&ctx.link_href, &parts.reading, &tooltip_text, "ruby");
        self.append_anchor_start(
            &href,
            if ctx.link_href.is_empty() { &ctx.text_color } else { "" },
            ctx.suppress_link_decoration,
            out,
        );
        let old_link_href = std::mem::take(&mut ctx.link_href);
        let old_title_tooltip = std::mem::take(&mut ctx.title_tooltip);
        render_base(&parts.base, ctx, out);
        ctx.title_tooltip = old_title_tooltip;
        ctx.link_href = old_link_href;
        out.push_str("</a>");
        ctx.elements.pop();
    }

    /// `addStructuredContent` and the object dispatcher (L426-646,
    /// L1166-1188). Unsupported tags have no output or child traversal.
    fn append_structured_content(
        &self,
        value: &Json,
        ctx: &mut Context,
        out: &mut String,
        source: &mut impl FnMut(&Json, &Context) -> String,
        locale_direction: &mut impl FnMut(&str) -> LayoutDirection,
    ) {
        match value {
            Json::String(text) => {
                self.flush_pending_vertical_margin(ctx, out);
                self.append_structured_text(text, ctx, out);
            }
            Json::Array(children) => {
                for child in children {
                    self.append_structured_content(child, ctx, out, source, locale_direction);
                }
            }
            Json::Object(_) => {
                let tag = value.get("tag").as_str();
                if !self.is_supported_structured_tag(tag) {
                    return;
                }
                match tag {
                    "ruby" => {
                        self.flush_pending_vertical_margin(ctx, out);
                        self.append_ruby(value, ctx, out, |base, ctx, out| {
                            self.append_ruby_base(base, ctx, out, source, locale_direction);
                        });
                    }
                    "a" if anchor_needs_custom_handling(value, ctx) => {
                        self.flush_pending_vertical_margin(ctx, out);
                        self.append_custom_anchor(value, ctx, out, |content, ctx, out| {
                            self.append_structured_children(
                                content, ctx, out, source, locale_direction,
                            );
                        });
                    }
                    "br" => {
                        self.flush_pending_vertical_margin(ctx, out);
                        self.append_structured_break(value, ctx, out);
                    }
                    "img" => {
                        self.flush_pending_vertical_margin(ctx, out);
                        self.append_structured_image(
                            value, |obj, ctx| source(obj, ctx), ctx, out,
                        );
                    }
                    _ => self.append_structured_element(
                        value, tag, ctx, out, source, locale_direction,
                    ),
                }
            }
            _ => {}
        }
    }

    /// `addStructuredChildren` (L648-663): a separate sibling scope also
    /// flushes any last child's pending margin before leaving the container.
    fn append_structured_children(
        &self,
        value: &Json,
        ctx: &mut Context,
        out: &mut String,
        source: &mut impl FnMut(&Json, &Context) -> String,
        locale_direction: &mut impl FnMut(&str) -> LayoutDirection,
    ) {
        let mut siblings = SiblingState::default();
        if ctx.needs_element_child_count && self.needs_structured_element_child_count(value, ctx) {
            siblings.element_count = self.structured_element_child_count(value);
        }
        ctx.siblings.push(siblings);
        self.append_structured_content(value, ctx, out, source, locale_direction);
        self.flush_pending_vertical_margin(ctx, out);
        ctx.siblings.pop();
    }

    /// Ruby bases are a source-order QJsonArray of references; nested arrays
    /// remain transparent to selectors, like `addStructuredChildren(base)`.
    fn append_ruby_base(
        &self,
        base: &[&Json],
        ctx: &mut Context,
        out: &mut String,
        source: &mut impl FnMut(&Json, &Context) -> String,
        locale_direction: &mut impl FnMut(&str) -> LayoutDirection,
    ) {
        let mut siblings = SiblingState::default();
        if ctx.needs_element_child_count && base.iter().any(|node| {
            self.needs_structured_element_child_count(node, ctx)
        }) {
            siblings.element_count = base.iter()
                .map(|node| self.structured_element_child_count(node))
                .sum();
        }
        ctx.siblings.push(siblings);
        for node in base {
            self.append_structured_content(node, ctx, out, source, locale_direction);
        }
        self.flush_pending_vertical_margin(ctx, out);
        ctx.siblings.pop();
    }

    /// `addClosedDetailsContent` (L717-744). Only direct summaries render,
    /// while nested hidden details consume stable IDs.
    fn render_closed_details_content(
        &self,
        value: &Json,
        ctx: &mut Context,
        out: &mut String,
        source: &mut impl FnMut(&Json, &Context) -> String,
        locale_direction: &mut impl FnMut(&str) -> LayoutDirection,
    ) {
        self.append_closed_details_content(value, ctx, out, &mut |summary, ctx, out| {
            self.append_structured_content(summary, ctx, out, source, locale_direction);
        });
    }

    /// `addInlineLabelContent` (L771-851). The first text-only painted span
    /// gets a compatibility box; only inline remainders use a two-cell row.
    fn render_inline_label_content(
        &self,
        content: &Json,
        ctx: &mut Context,
        out: &mut String,
        source: &mut impl FnMut(&Json, &Context) -> String,
        locale_direction: &mut impl FnMut(&str) -> LayoutDirection,
    ) {
        let mut values = Vec::new();
        append_direct_content_values(content, &mut values);
        if values.is_empty() {
            return;
        }
        let inline_remainder = values[1..].iter().all(|value| is_inline_structured_content(value));
        let has_remainder = values.len() > 1;
        let mut label_spacing = "";
        let mut label_font_pixel_size = ctx.parent_font_pixel_size;
        if inline_remainder && has_remainder {
            let style = values[0].get("style");
            let font_size = style.get("fontSize").as_str();
            if !font_size.is_empty() {
                label_font_pixel_size = css_font_size_to_pixels(
                    font_size, ctx.screen.as_ref(), ctx.parent_font_pixel_size,
                    ctx.root_font_pixel_size,
                );
            }
            label_spacing = style.get(if ctx.text_direction == LayoutDirection::RightToLeft {
                "marginLeft"
            } else {
                "marginRight"
            }).as_str();
            out.push_str("<table cellspacing=\"0\" width=\"100%\"><tr><td style=\"vertical-align: middle; white-space: nowrap;\">");
        }

        let old_force_inline_box = std::mem::replace(&mut ctx.force_inline_box, true);
        let old_suppress_inline_box_spacing = std::mem::replace(
            &mut ctx.suppress_inline_box_spacing, inline_remainder && has_remainder,
        );
        self.append_structured_content(values[0], ctx, out, source, locale_direction);
        ctx.suppress_inline_box_spacing = old_suppress_inline_box_spacing;
        ctx.force_inline_box = old_force_inline_box;

        if inline_remainder && has_remainder {
            self.append_selection_sentinel('\u{2065}', out);
            out.push_str("</td><td width=\"100%\" style=\"vertical-align: top;\">");
            self.append_selection_sentinel('\u{206a}', out);
            self.append_inline_spacer(label_spacing, label_font_pixel_size, ctx, out);
        }
        for value in &values[1..] {
            self.append_structured_content(value, ctx, out, source, locale_direction);
        }
        if inline_remainder && has_remainder {
            out.push_str("</td></tr></table>");
        }
    }

    /// `addCompatibleElementContent` (L853-886).
    fn render_compatible_element_content(
        &self,
        obj: &Json,
        tag: &str,
        ctx: &mut Context,
        out: &mut String,
        source: &mut impl FnMut(&Json, &Context) -> String,
        locale_direction: &mut impl FnMut(&str) -> LayoutDirection,
    ) {
        let content = obj.get("content");
        if matches!(tag, "div" | "td" | "th") {
            let mut values = Vec::new();
            append_direct_content_values(content, &mut values);
            if values.first().is_some_and(|label| is_inline_box_label(label))
                && (tag == "div" || values.len() == 1)
            {
                self.render_inline_label_content(content, ctx, out, source, locale_direction);
                return;
            }
        }
        self.append_structured_content(content, ctx, out, source, locale_direction);
    }

    /// `elementRenderState` (L1038-1077). Qt owns language text direction;
    /// inheriting an absent lang does not invoke the locale callback.
    fn element_render_state(
        &self,
        obj: &Json,
        tag: &str,
        ctx: &mut Context,
        locale_direction: &mut impl FnMut(&str) -> LayoutDirection,
    ) -> ElementRenderState {
        let mut state = ElementRenderState {
            tag: tag.into(),
            output_tag: tag.into(),
            font_pixel_size: ctx.parent_font_pixel_size,
            text_color: ctx.text_color.clone(),
            title_tooltip: ctx.title_tooltip.clone(),
            text_direction: ctx.text_direction,
            painted_background_color: ctx.painted_background_color.clone(),
            text_only_content: obj.get("content").is_string(),
            ..ElementRenderState::default()
        };
        if tag == "summary" && ctx.elements.len() > 1 && !ctx.details.is_empty() {
            let parent_index = ctx.elements[ctx.elements.len() - 2] as usize;
            if ctx.selector_elements[parent_index].tag == "details" {
                state.disclosure_summary = true;
                state.details = ctx.details.last().expect("details stack checked").clone();
            }
        }
        if let Json::String(lang) = obj.get("lang") {
            state.text_direction = locale_direction(lang);
        }
        self.add_element_attributes(obj, &mut state);
        self.resolve_element_styles(obj, ctx, &mut state);
        self.resolve_element_layout(ctx, &mut state);
        self.apply_element_compatibility(ctx, &mut state);
        state
    }

    /// `addStructuredElement` (L888-1036): selector registration, style and
    /// layout resolution, margin collapsing, child context, then closing
    /// frames and trailing margin are kept in the C++ emitter's order.
    fn append_structured_element(
        &self,
        obj: &Json,
        tag: &str,
        ctx: &mut Context,
        out: &mut String,
        source: &mut impl FnMut(&Json, &Context) -> String,
        locale_direction: &mut impl FnMut(&str) -> LayoutDirection,
    ) {
        let is_details = tag == "details";
        let details = if is_details {
            Some(self.details_state(obj, ctx))
        } else {
            None
        };
        let index = self.structured_element(
            obj, ctx, details.as_ref().is_some_and(|details| details.open),
        );
        ctx.elements.push(index);

        let mut state = self.element_render_state(obj, tag, ctx, locale_direction);
        if let Some(details) = details {
            state.details = details;
        }
        let block_element = state.layout != ElementLayout::Inline;
        let collapsible_block = state.margin_flow == MarginFlow::Collapsible;
        let propagates_trailing_child_margin =
            state.margin_flow == MarginFlow::PropagateLastChild;

        if block_element {
            // `add_pending_vertical_margin` reads the screen and root font
            // from Context; moving the active sibling out avoids cloning it.
            let mut siblings = ctx.siblings.pop().expect("structured child scope");
            self.add_pending_vertical_margin(
                &state.box_style.margins[TOP], state.font_pixel_size, ctx, &mut siblings,
            );
            ctx.siblings.push(siblings);
            if !collapsible_block {
                self.flush_pending_vertical_margin(ctx, out);
            }
        } else {
            self.flush_pending_vertical_margin(ctx, out);
        }
        self.append_structured_element_start(&state, ctx, out);

        if state.layout == ElementLayout::List {
            ctx.lists.push(StructuredList {
                tag: tag.into(),
                marker: state.list_marker_type.clone(),
                item: 0,
            });
        }
        if is_details {
            ctx.details.push(state.details.clone());
        }
        std::mem::swap(&mut ctx.parent_font_pixel_size, &mut state.font_pixel_size);
        std::mem::swap(&mut ctx.text_color, &mut state.text_color);
        std::mem::swap(&mut ctx.title_tooltip, &mut state.title_tooltip);
        std::mem::swap(&mut ctx.text_direction, &mut state.text_direction);
        std::mem::swap(&mut ctx.painted_background_color, &mut state.painted_background_color);
        let old_suppress_link_decoration = ctx.suppress_link_decoration;
        ctx.suppress_link_decoration |= state.raised_text;

        let content = obj.get("content");
        let mut child_siblings = SiblingState::default();
        if ctx.needs_element_child_count
            && self.needs_structured_element_child_count(content, ctx)
        {
            child_siblings.element_count = self.structured_element_child_count(content);
        }
        if block_element && collapsible_block {
            self.merge_pending_vertical_margins(
                ctx.siblings.last().expect("parent sibling scope"), &mut child_siblings,
            );
            self.clear_pending_vertical_margins(
                ctx.siblings.last_mut().expect("parent sibling scope"),
            );
        }
        ctx.siblings.push(child_siblings);
        if is_details && !state.details.open {
            self.render_closed_details_content(content, ctx, out, source, locale_direction);
        } else {
            self.render_compatible_element_content(
                obj, tag, ctx, out, source, locale_direction,
            );
        }
        if !block_element || (!collapsible_block && !propagates_trailing_child_margin) {
            self.flush_pending_vertical_margin(ctx, out);
        }
        let completed_children = ctx.siblings.pop().expect("child sibling scope");

        ctx.suppress_link_decoration = old_suppress_link_decoration;
        std::mem::swap(&mut ctx.painted_background_color, &mut state.painted_background_color);
        std::mem::swap(&mut ctx.text_direction, &mut state.text_direction);
        std::mem::swap(&mut ctx.title_tooltip, &mut state.title_tooltip);
        std::mem::swap(&mut ctx.text_color, &mut state.text_color);
        std::mem::swap(&mut ctx.parent_font_pixel_size, &mut state.font_pixel_size);

        if state.layout == ElementLayout::List {
            ctx.lists.pop();
        }
        if is_details {
            ctx.details.pop();
        }
        self.append_structured_element_end(&state, ctx, out);
        if block_element {
            if collapsible_block || propagates_trailing_child_margin {
                self.merge_pending_vertical_margins(
                    &completed_children, ctx.siblings.last_mut().expect("parent sibling scope"),
                );
            }
            let mut siblings = ctx.siblings.pop().expect("parent sibling scope");
            self.add_pending_vertical_margin(
                &state.box_style.margins[BOTTOM], state.font_pixel_size, ctx, &mut siblings,
            );
            ctx.siblings.push(siblings);
        }
        ctx.elements.pop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::element::{
        CssDeclaration, CssPseudoClass, CssPseudoClassSelector, CssPseudoElement, CssRule,
        CssSelectorPart, ParsedStylesheet, SiblingState,
    };
    use std::sync::Arc;

    fn j(text: &str) -> Json {
        Json::parse(text).expect("valid JSON")
    }

    #[test]
    fn legacy_glossary_modes_and_structured_cutover() {
        let renderer = Renderer::new();
        let entries = vec![j("\"One <two>\\n&\\\"'\""), j(r#"{"type":"text","text":"三 & four"}"#)];
        let first = "One &lt;two&gt;<br>&amp;&quot;'";
        let second = "三 &amp; four";
        let body = format!("{first}<br>{second}");
        assert_eq!(
            renderer.render_entries(&entries, LegacyGlossaryStyle::LineBreak, &mut Context::default(), |_, _| unreachable!(), |_| unreachable!()),
            format!("<html><head></head><body>{body}</body></html>"),
        );
        assert_eq!(
            renderer.render_entries(&entries, LegacyGlossaryStyle::Pipe, &mut Context::default(), |_, _| unreachable!(), |_| unreachable!()),
            format!("<html><head></head><body>{first} | {second}</body></html>"),
        );
        let bullet = renderer.render_entries(&entries, LegacyGlossaryStyle::Bullet, &mut Context::default(), |_, _| unreachable!(), |_| unreachable!());
        assert_eq!(bullet.matches("<table cellspacing=\"0\"").count(), 2);
        assert!(bullet.contains("padding-right: 4.2px;"));
        assert!(bullet.contains("\u{2060}\u{2061}•\u{2062}"));
        let mixed = vec![j(r#"{"type":"structured-content","content":"structured"}"#), j("\"plain\"")];
        assert_eq!(
            renderer.render_entries(&mixed, LegacyGlossaryStyle::Bullet, &mut Context::default(), |_, _| unreachable!(), |_| unreachable!()),
            "<html><head></head><body>structuredplain</body></html>",
        );
        assert_eq!(renderer.render_entries(&[], LegacyGlossaryStyle::Bullet, &mut Context::default(), |_, _| unreachable!(), |_| unreachable!()), "<html><head></head><body></body></html>");
    }

    #[test]
    fn structured_data_attributes_follow_qstring_utf16_key_order() {
        let renderer = Renderer::new();
        let object = j(r#"{"\ue000":"bmp","\ud83d\ude00":"astral","a":"ascii"}"#);
        let mut html = String::new();
        renderer.append_structured_data(&object, &mut html);
        assert_eq!(html, " data-sc-a=\"ascii\" data-sc-😀=\"astral\" data-sc-\u{e000}=\"bmp\"");
    }

    #[test]
    fn image_attributes_and_provider_are_explicit() {
        let renderer = Renderer::new();
        let mut ctx = Context::default();
        ctx.text_color = "#123456".into();
        let entry = j(r#"{"type":"image","path":"glyph.png","width":14.5,"height":8,"title":"A & B","description":"under\nline","imageRendering":"pixelated"}"#);
        let html = renderer.render_entries(&[entry], LegacyGlossaryStyle::Pipe, &mut ctx, |image, _| {
            format!("file:///dictionary/{}?a=1&b=2", image.get("path").as_str())
        }, |_| unreachable!());
        assert!(html.contains("<a href=\"memento://glossary-link?tooltip=A%20%26%20B&amp;tooltipType=title\" style=\"color: #123456; text-decoration: none;\">"));
        assert!(html.contains("<img src=\"file:///dictionary/glyph.png?a=1&amp;b=2\" width=\"14.5\" height=\"8\" style=\"display: inline-table; vertical-align: top;image-rendering: pixelated;\">"));
        assert!(html.contains("</a><br>under<br>line</body>"));
    }

    #[test]
    fn structured_image_uses_css_selector_and_image_settings() {
        let renderer = Renderer::new();
        let obj = j(r#"{"tag":"img","path":"sample.svg","alt":"A & B","data":{"fooBar":"yes"},"width":2,"height":1.5,"sizeUnits":"em","pixelated":true,"background":false,"verticalAlign":"middle","border":"1px solid"}"#);
        let mut ctx = Context::default();
        ctx.parent_font_pixel_size = 10.0;
        let mut stylesheet = ParsedStylesheet::default();
        stylesheet.rules.push(CssRule {
            selector: vec![CssSelectorPart {
                tag: "img".into(),
                ..CssSelectorPart::default()
            }],
            declarations: vec![CssDeclaration {
                property: "color".into(),
                value: "red".into(),
            }],
            ..CssRule::default()
        });
        stylesheet.universal_rule_indexes.push(0);
        ctx.dictionary_styles = Some(Arc::new(stylesheet));
        ctx.siblings.push(SiblingState::default());
        let mut out = String::new();
        renderer.append_structured_image(&obj, |_, ctx| {
            assert_eq!(ctx.selector_elements[ctx.elements[0] as usize].tag, "img");
            "image://provider/a?x=1&y=2".into()
        }, &mut ctx, &mut out);
        assert_eq!(out, "<img src=\"image://provider/a?x=1&amp;y=2\" data-sc-foo-bar=\"yes\" alt=\"A &amp; B\" width=\"20\" height=\"15\" style=\"border: 1px solid;color: red;image-rendering: pixelated;vertical-align: middle;\">");
        assert!(ctx.elements.is_empty());
        assert_eq!(ctx.selector_elements[0].previous_sibling, -1);
        assert_eq!(ctx.siblings[0].visited_element_count, 1);
        assert!(ctx.selector_elements[0].classes.contains("gloss-image"));
    }

    #[test]
    fn attributes_links_and_inherited_text() {
        let renderer = Renderer::new();
        let obj = j(r#"{"href":"a&b","colSpan":3.8,"rowSpan":2,"title":"<tip>","lang":"ja","data":{"fooBar":"a\"b","ignored":true}}"#);
        let mut state = ElementRenderState::default();
        renderer.add_element_attributes(&obj, &mut state);
        assert_eq!(state.attributes, " href=\"a&amp;b\" data-sc-foo-bar=\"a&quot;b\" colspan=\"3\" rowspan=\"2\" title=\"&lt;tip&gt;\" lang=\"ja\"");
        assert_eq!(state.title_tooltip, "<tip>");
        assert_eq!(renderer.internal_link_href("雪 /?&", "Hé", "A&B", "ruby"), "memento://glossary-link?target=%E9%9B%AA%20%2F%3F%26&tooltip=H%C3%A9&tooltipText=A%26B&tooltipType=ruby");
        let ctx = Context {
            text_color: "#f00".into(),
            title_tooltip: "A & B".into(),
            ..Context::default()
        };
        let mut out = String::new();
        renderer.append_structured_text("it's <ok>\n", &ctx, &mut out);
        assert_eq!(out, "<a href=\"memento://glossary-link?tooltip=A%20%26%20B&amp;tooltipType=title\" style=\"color: #f00; text-decoration: none;\">it's &lt;ok&gt;<br></a>");
    }

    #[test]
    fn custom_anchor_and_break_register_selector_siblings() {
        let renderer = Renderer::new();
        let anchor = j(r#"{"tag":"a","href":"next?","title":"new &","content":"A <B>"}"#);
        let mut ctx = Context {
            link_href: "prior".into(),
            title_tooltip: "prior title".into(),
            ..Context::default()
        };
        ctx.siblings.push(SiblingState::default());
        assert!(anchor_needs_custom_handling(&anchor, &ctx));
        let mut out = String::new();
        renderer.append_custom_anchor(&anchor, &mut ctx, &mut out, |value, ctx, out| {
            assert_eq!(ctx.elements.len(), 1);
            renderer.append_structured_text(value.as_str(), ctx, out);
        });
        assert_eq!(out, r#"<a href="memento://glossary-link?target=next%3F&amp;tooltip=new%20%26&amp;tooltipType=title">A &lt;B&gt;</a>"#);
        assert_eq!(ctx.link_href, "prior");
        assert_eq!(ctx.title_tooltip, "prior title");
        assert!(ctx.elements.is_empty());
        renderer.append_structured_break(
            &j(r#"{"tag":"br","data":{"testName":"x&y"}}"#),
            &mut ctx,
            &mut out,
        );
        assert!(out.ends_with(r#"<br data-sc-test-name="x&amp;y">"#));
        assert_eq!(ctx.selector_elements[0].tag, "a");
        assert_eq!(ctx.selector_elements[1].previous_sibling, 0);
        assert_eq!(ctx.siblings[0].visited_element_count, 2);
        assert!(ctx.elements.is_empty());
    }

    #[test]
    fn ruby_and_anchor_scans_preserve_source_depth() {
        let renderer = Renderer::new();
        let ruby = j(r#"{"tag":"ruby","content":["漢",{"tag":"rp","content":"("},{"tag":"rt","content":["か",{"tag":"span","content":"ん"}]},"字",{"tag":"rt","content":"じ"}]}"#);
        let parts = split_ruby_content(ruby.get("content"));
        assert_eq!(parts.reading, "かんじ");
        assert_eq!(parts.base.len(), 2);
        assert!(contains_ruby(&ruby));
        assert!(anchor_needs_custom_handling(&j(r#"{"tag":"a","content":["base",{"tag":"span","style":{"verticalAlign":"SuPeR"}}]}"#), &Context::default()));
        let mut ctx = Context {
            link_href: "entry".into(),
            title_tooltip: "old".into(),
            ..Context::default()
        };
        let mut out = String::new();
        renderer.append_ruby(&ruby, &mut ctx, &mut out, |base, ctx, out| {
            assert!(ctx.link_href.is_empty());
            assert!(ctx.title_tooltip.is_empty());
            for node in base {
                structured_content_text(node, out);
            }
        });
        assert_eq!(out, "<a href=\"memento://glossary-link?target=entry&amp;tooltip=%E3%81%8B%E3%82%93%E3%81%98&amp;tooltipText=%E6%BC%A2%E5%AD%97&amp;tooltipType=ruby\">漢字</a>");
        assert_eq!(ctx.link_href, "entry");
        assert_eq!(ctx.title_tooltip, "old");
        let no_reading = j(r#"{"tag":"ruby","content":[{"tag":"rp","content":"("},"字"]}"#);
        out.clear();
        renderer.append_ruby(&no_reading, &mut ctx, &mut out, |base, _, out| {
            assert_eq!(base.len(), 1);
            structured_content_text(base[0], out);
        });
        assert_eq!(out, "字");
        assert_eq!(ctx.selector_elements[0].tag, "ruby");
        assert!(ctx.elements.is_empty());
    }

    #[test]
    fn details_ids_and_direct_child_count_skip_hidden_branches() {
        let renderer = Renderer::new();
        let nodes = j(r#"[[null,"text",{"tag":"SPAN","content":"hello"},[{"tag":"details","content":[{"tag":"summary","content":"show"},{"tag":"details","content":"hidden"}]}],{"tag":"future","content":{"tag":"li"}}]]"#);
        let mut direct = Vec::new();
        append_direct_content_values(&nodes, &mut direct);
        assert_eq!(direct.len(), 4);
        assert_eq!(renderer.structured_element_child_count(&nodes), 2);
        let mut stylesheet = ParsedStylesheet::default();
        stylesheet.uses_element_child_count = true;
        stylesheet.element_child_count_tags.insert("details".into());
        let mut ctx = Context {
            dictionary_styles: Some(Arc::new(stylesheet)),
            ..Context::default()
        };
        assert!(renderer.needs_structured_element_child_count(&nodes, &ctx));
        let closed = j(r#"["ignored",{"tag":"summary","content":"head"},[null,{"tag":"div","content":{"tag":"details","content":{"tag":"DETAILS","content":[]}}}]]"#);
        let mut out = String::new();
        renderer.append_closed_details_content(&closed, &mut ctx, &mut out, &mut |value, _, out| {
            structured_content_text(value, out);
        });
        assert_eq!(out, "head");
        assert_eq!(ctx.next_detail_id, 2);
        ctx.detail_states.insert("2".into(), false);
        let details = renderer.details_state(&j(r#"{"open":true}"#), &mut ctx);
        assert_eq!(details, DetailsState { id: "2".into(), open: false });
        assert_eq!(renderer.details_link_href(&details), "memento://glossary-details?id=2&open=0");
        assert_eq!(ctx.next_detail_id, 3);
    }

    #[test]
    fn inline_label_predicates_match_transparent_arrays_and_box_styles() {
        let label = j(r#"{"tag":"SpAn","content":"label","style":{"borderLeft":"1px solid"}}"#);
        assert!(is_inline_box_label(&label));
        assert!(is_inline_structured_content(&j(r#"["a",[{"tag":"ruby"},{"tag":"IMG"}]]"#)));
        assert!(!is_inline_structured_content(&j(r#"[{"tag":"span"},{"tag":"div"}]"#)));
        assert!(!is_inline_box_label(&j(r#"{"tag":"span","content":["x"],"style":{"borderLeft":"1px"}}"#)));
        assert!(!is_inline_box_label(&j(r#"{"tag":"span","content":"x","style":{"color":"red"}}"#)));
    }

    #[test]
    fn traversal_applies_last_child_css_and_generated_content_after_flattening() {
        let renderer = Renderer::new();
        let mut styles = ParsedStylesheet::default();
        styles.uses_element_child_count = true;
        styles.element_child_count_tags.insert("span".into());
        styles.rules.push(CssRule {
            selector: vec![CssSelectorPart {
                tag: "span".into(),
                pseudo_classes: vec![CssPseudoClassSelector {
                    pseudo_type: CssPseudoClass::LastChild,
                    ..CssPseudoClassSelector::default()
                }],
                ..CssSelectorPart::default()
            }],
            pseudo_element: CssPseudoElement::Before,
            declarations: vec![CssDeclaration {
                property: "content".into(),
                value: "★".into(),
            }],
            ..CssRule::default()
        });
        styles.universal_rule_indexes.push(0);
        let mut ctx = Context {
            dictionary_styles: Some(Arc::new(styles)),
            ..Context::default()
        };
        let entries = [j(r#"{"type":"structured-content","content":["start",{"tag":"span","content":"one"},[null,{"tag":"span","content":"two"}],{"tag":"unsupported","content":"lost"}]}"#)];
        let html = renderer.render_entries(
            &entries, LegacyGlossaryStyle::Bullet, &mut ctx,
            |_, _| unreachable!(), |_| unreachable!(),
        );
        assert_eq!(html, "<html><head></head><body>start<span>one</span><span>★two</span></body></html>");
        assert_eq!(ctx.selector_elements.len(), 2);
        assert_eq!(ctx.selector_elements[0].child_index, 1);
        assert_eq!(ctx.selector_elements[1].child_index, 2);
        assert_eq!(ctx.selector_elements[0].child_count, 2);
        assert_eq!(ctx.selector_elements[1].previous_sibling, 0);
        assert!(ctx.elements.is_empty() && ctx.siblings.is_empty());
    }

    #[test]
    fn traversal_collapses_parent_child_and_sibling_vertical_margins() {
        let renderer = Renderer::new();
        let entries = [j(r#"{"type":"structured-content","content":[{"tag":"div","style":{"marginBottom":"8px"},"content":"one"},{"tag":"div","style":{"marginTop":"4px"},"content":["two",{"tag":"div","style":{"marginBottom":"5px"},"content":"nested"}]}]}"#)];
        let mut ctx = Context::default();
        let html = renderer.render_entries(
            &entries, LegacyGlossaryStyle::Pipe, &mut ctx,
            |_, _| unreachable!(), |_| unreachable!(),
        );
        assert!(html.contains("one</div><div><div style=\"font-size: 1px;line-height: 1px;margin-top: 7px;\">&nbsp;</div>two"));
        assert!(html.contains("nested</div></div><div style=\"font-size: 1px;line-height: 1px;margin-top: 4px;\">&nbsp;</div></body>"));
        assert_eq!(html.matches("margin-top: 7px;").count(), 1);
        assert!(ctx.siblings.is_empty());
    }

    #[test]
    fn traversal_list_markers_and_hidden_items_preserve_numbering() {
        let renderer = Renderer::new();
        let entries = [j(r#"{"type":"structured-content","content":{"tag":"ol","content":[{"tag":"li","content":"first"},{"tag":"li","style":{"listStyleType":"none"},"content":"hidden marker"},{"tag":"li","content":["third",{"tag":"ul","content":{"tag":"li","content":"nested"}}]}]}}"#)];
        let mut ctx = Context::default();
        let html = renderer.render_entries(
            &entries, LegacyGlossaryStyle::Pipe, &mut ctx,
            |_, _| unreachable!(), |_| unreachable!(),
        );
        assert!(html.contains("\u{2060}\u{2061}1.\u{2062}"));
        assert!(html.contains("\u{2060}hidden marker"));
        assert!(html.contains("\u{2060}\u{2061}3.\u{2062}"));
        assert!(html.contains("\u{2060}\u{2061}•\u{2062}"));
        assert_eq!(ctx.lists.len(), 0);
        assert!(ctx.elements.is_empty() && ctx.siblings.is_empty());
    }

    #[test]
    fn traversal_closed_details_skips_hidden_ids_and_uses_overrides() {
        let renderer = Renderer::new();
        let entries = [j(r#"{"type":"structured-content","content":[{"tag":"details","content":[{"tag":"summary","content":"Closed"},{"tag":"div","content":{"tag":"details","content":{"tag":"summary","content":"Nested"}}},[{"tag":"summary","content":"Second"}]]},{"tag":"details","open":false,"content":[{"tag":"summary","content":"Open"},{"tag":"div","content":"visible"}]}]}"#)];
        let mut ctx = Context::default();
        ctx.detail_states.insert("2".into(), true);
        ctx.text_color = "#112233".into();
        let html = renderer.render_entries(
            &entries, LegacyGlossaryStyle::Bullet, &mut ctx,
            |_, _| unreachable!(), |_| unreachable!(),
        );
        assert!(html.contains("id=0&amp;open=0"));
        assert!(html.contains("\u{2063}▸ \u{2064}Closed"));
        assert!(html.contains("\u{2063}▸ \u{2064}Second"));
        assert!(!html.contains("Nested"));
        assert!(html.contains("id=2&amp;open=1"));
        assert!(html.contains("\u{2063}▾ \u{2064}Open"));
        assert!(html.contains("visible"));
        assert_eq!(ctx.next_detail_id, 3);
        assert!(ctx.details.is_empty() && ctx.siblings.is_empty());
        let details: Vec<_> = ctx.selector_elements.iter()
            .filter(|element| element.tag == "details").collect();
        assert_eq!(details.len(), 2);
        assert!(!details[0].attributes.contains_key("open"));
        assert!(details[1].attributes.contains_key("open"));
    }

    #[test]
    fn traversal_inline_label_ruby_and_image_callbacks_preserve_order() {
        let renderer = Renderer::new();
        let entries = [j(r##"{"type":"structured-content","content":[{"tag":"div","lang":"ar","content":[{"tag":"span","style":{"backgroundColor":"#eeeeee","marginLeft":"3px"},"content":"label"},"rest",{"tag":"img","path":"glyph.svg","alt":"x&y"}]},{"tag":"a","href":"entry","content":[{"tag":"ruby","content":["字",{"tag":"rt","content":"じ"}]},{"tag":"span","style":{"verticalAlign":"super"},"content":"up"}]}]}"##)];
        let mut ctx = Context::default();
        ctx.text_color = "#123456".into();
        let mut images = 0;
        let mut locales = 0;
        let html = renderer.render_entries(
            &entries, LegacyGlossaryStyle::Bullet, &mut ctx,
            |image, ctx| {
                assert_eq!(image.get("path").as_str(), "glyph.svg");
                assert_eq!(ctx.selector_elements[*ctx.elements.last().unwrap() as usize].tag, "img");
                images += 1;
                "image://provider/glyph.svg?a=1&b=2".into()
            },
            |lang| {
                assert_eq!(lang, "ar");
                locales += 1;
                LayoutDirection::RightToLeft
            },
        );
        assert_eq!((images, locales), (1, 1));
        assert!(html.contains("\u{2065}</td><td width=\"100%\" style=\"vertical-align: top;\">\u{206a}<span style=\"font-size: 1px;letter-spacing: 3px;\">&nbsp;</span>rest"));
        assert!(html.contains("<img src=\"image://provider/glyph.svg?a=1&amp;b=2\" alt=\"x&amp;y\""));
        assert!(html.contains("target=entry&amp;tooltip=%E3%81%98&amp;tooltipText=%E5%AD%97&amp;tooltipType=ruby"));
        assert!(html.contains("style=\"text-decoration: none;\">up</a>"));
        assert_eq!(ctx.text_direction, LayoutDirection::LeftToRight);
        assert!(ctx.link_href.is_empty() && ctx.elements.is_empty());
    }
}

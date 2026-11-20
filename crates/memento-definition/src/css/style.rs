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

//! Structured-content style resolution and CSS declaration normalization.

use std::borrow::Cow;

use serde_json::Value;

use crate::element::{Context, CssDeclarations, ElementRenderState};
use crate::helpers::{css_font_size_to_pixels, is_built_in_list_marker, normalize_list_marker};
use crate::qstring::{
    equals_ascii_ci, is_qchar_space, qstring_number, starts_with_ascii_ci, to_lower_cow, trimmed,
};

use super::scanners::{has_unsupported_css_function, is_supported_css_property};
use super::values::split_css_tokens;
use super::Renderer;

impl Renderer {
    fn add_spacing_value(
        &self,
        property: &str,
        value: &Value,
        ctx: &mut Context,
        declarations: &mut CssDeclarations,
    ) {
        if let Some(s) = value.as_str() {
            self.add_resolved_css_declaration(property, s, ctx, declarations);
        } else if let Some(n) = value.as_f64() {
            let formatted = format!("{}em", qstring_number(n));
            self.add_resolved_css_declaration(property, &formatted, ctx, declarations);
        }
    }

    /// Iterate only present keys in a structured `style` object while preserving
    /// exact CSS precedence between overlapping shorthand/alias properties
    /// (`background` < `backgroundColor`, `borderRadius` < `clipPath`,
    /// `margin` < `margin{Top,Right,Bottom,Left}`, `wordBreak` < `whiteSpace`).
    pub fn add_structured_style_value(
        &self,
        obj: &Value,
        ctx: &mut Context,
        state: &mut ElementRenderState,
    ) {
        let Some(map) = obj.as_object() else {
            return;
        };
        let mut deferred_bg_color: Option<&str> = None;
        let mut deferred_clip_path: Option<&str> = None;
        let mut deferred_white_space: Option<&str> = None;
        let mut deferred_margin_sides: [Option<&Value>; 4] = [None; 4];

        // Apply any inline custom properties (`--*`) to the active element first
        // so sibling properties in the same `style` object resolve against them.
        if let Some(&elem_idx) = ctx.elements.last() {
            if let Some(element) = ctx.selector_elements.get_mut(elem_idx as usize) {
                for (key, value) in map {
                    if key.starts_with("--") {
                        if let Some(s) = value.as_str() {
                            let val = trimmed(s);
                            if !val.is_empty() {
                                element.custom_properties.insert(key.clone(), val.to_owned());
                            }
                        }
                    }
                }
            }
        }

        for (key, value) in map {
            match key.as_str() {
                "fontSize" => {
                    if let Some(raw) = value.as_str() {
                        let font_size = self.resolve_css_value_cow(raw, ctx);
                        if !font_size.is_empty() {
                            let pixels = css_font_size_to_pixels(
                                &font_size,
                                ctx.screen.as_ref(),
                                ctx.parent_font_pixel_size,
                                ctx.root_font_pixel_size,
                            );
                            if pixels >= 0.0 {
                                state.font_pixel_size = pixels;
                                state.font_size_px = Some(pixels);
                            } else {
                                state.font_size_px = None;
                                self.add_css_declaration(
                                    "font-size",
                                    &font_size,
                                    &mut state.declarations,
                                );
                            }
                        }
                    }
                }
                "textDecorationLine" => {
                    if let Some(items) = value.as_array() {
                        let mut joined = String::new();
                        for item in items {
                            if let Some(s) = item.as_str() {
                                if !joined.is_empty() {
                                    joined.push(' ');
                                }
                                joined.push_str(s);
                            }
                        }
                        self.add_resolved_css_declaration(
                            "text-decoration-line",
                            &joined,
                            ctx,
                            &mut state.declarations,
                        );
                    } else if let Some(s) = value.as_str() {
                        self.add_resolved_css_declaration(
                            "text-decoration-line",
                            s,
                            ctx,
                            &mut state.declarations,
                        );
                    }
                }
                "margin" => {
                    self.add_spacing_value("margin", value, ctx, &mut state.declarations);
                }
                "marginTop" => deferred_margin_sides[0] = Some(value),
                "marginRight" => deferred_margin_sides[1] = Some(value),
                "marginBottom" => deferred_margin_sides[2] = Some(value),
                "marginLeft" => deferred_margin_sides[3] = Some(value),
                "backgroundColor" => {
                    if let Some(s) = value.as_str() {
                        deferred_bg_color = Some(s);
                    }
                }
                "clipPath" => {
                    if let Some(s) = value.as_str() {
                        deferred_clip_path = Some(s);
                    }
                }
                "whiteSpace" => {
                    if let Some(s) = value.as_str() {
                        deferred_white_space = Some(s);
                    }
                }
                _ => {
                    let property = match key.as_str() {
                        "fontStyle" => "font-style",
                        "fontWeight" => "font-weight",
                        "color" => "color",
                        "background" => "background",
                        "textDecorationStyle" => "text-decoration-style",
                        "textDecorationColor" => "text-decoration-color",
                        "borderColor" => "border-color",
                        "borderStyle" => "border-style",
                        "borderRadius" => "border-radius",
                        "borderWidth" => "border-width",
                        "verticalAlign" => "vertical-align",
                        "textAlign" => "text-align",
                        "textEmphasis" => "text-emphasis",
                        "textShadow" => "text-shadow",
                        "padding" => "padding",
                        "paddingTop" => "padding-top",
                        "paddingRight" => "padding-right",
                        "paddingBottom" => "padding-bottom",
                        "paddingLeft" => "padding-left",
                        "wordBreak" => "word-break",
                        "cursor" => "cursor",
                        "listStyleType" => "list-style-type",
                        _ => continue,
                    };
                    if let Some(s) = value.as_str() {
                        self.add_resolved_css_declaration(
                            property,
                            s,
                            ctx,
                            &mut state.declarations,
                        );
                    }
                }
            }
        }

        if let Some(s) = deferred_bg_color {
            self.add_resolved_css_declaration("background-color", s, ctx, &mut state.declarations);
        }
        if let Some(s) = deferred_clip_path {
            self.add_resolved_css_declaration("clip-path", s, ctx, &mut state.declarations);
        }
        const MARGIN_SIDE_PROPS: [&str; 4] = [
            "margin-top",
            "margin-right",
            "margin-bottom",
            "margin-left",
        ];
        for (prop, val) in MARGIN_SIDE_PROPS.iter().zip(deferred_margin_sides) {
            if let Some(v) = val {
                self.add_spacing_value(prop, v, ctx, &mut state.declarations);
            }
        }
        if let Some(s) = deferred_white_space {
            self.add_resolved_css_declaration("white-space", s, ctx, &mut state.declarations);
        }
    }

    /// Resolve matched stylesheet rules and inline `style` declarations for a
    /// structured element.
    pub fn resolve_element_styles_value(
        &self,
        obj: &Value,
        ctx: &mut Context,
        state: &mut ElementRenderState,
    ) {
        const KEY_STYLE: &str = "style";

        if state.tag == "table" {
            self.add_css_declaration("border", "1px solid", &mut state.declarations);
            self.add_css_declaration("border-collapse", "collapse", &mut state.declarations);
        } else if state.tag == "th" || state.tag == "td" {
            self.add_css_declaration("border", "1px solid", &mut state.declarations);
            self.add_css_declaration("border-collapse", "collapse", &mut state.declarations);
            self.add_css_declaration("padding", "5px", &mut state.declarations);
        }

        {
            let ElementRenderState {
                declarations,
                before_content,
                after_content,
                font_size_px,
                ..
            } = state;
            self.add_matching_css_rules_inner(
                ctx,
                declarations,
                Some(before_content),
                Some(after_content),
                Some(font_size_px),
            );
        }

        if let Some(style) = obj.get(KEY_STYLE) {
            if style.is_object() {
                self.add_structured_style_value(style, ctx, state);
            }
        }

        if let Some(pixel_size) = state.font_size_px {
            state.font_pixel_size = pixel_size;
        } else if let Some(value) = state.declarations.get("font-size") {
            let pixel_size = css_font_size_to_pixels(
                value,
                ctx.screen.as_ref(),
                ctx.parent_font_pixel_size,
                ctx.root_font_pixel_size,
            );
            if pixel_size >= 0.0 {
                state.font_pixel_size = pixel_size;
                state.font_size_px = Some(pixel_size);
            }
        }
        if let Some(color) = state.declarations.get("color") {
            state.text_color.clone_from(color);
        }
    }

    /// Add a CSS declaration to a declaration map (`addCssDeclaration`,
    /// L278-433).
    pub fn add_css_declaration(
        &self,
        property: &str,
        value: &str,
        declarations: &mut CssDeclarations,
    ) {
        let name = to_lower_cow(trimmed(property));
        let raw_trimmed = trimmed(value);

        if name.is_empty() || raw_trimmed.is_empty() {
            return;
        }

        if name.as_ref() == "margin" {
            const MAXIMUM_MARGIN_VALUES: usize = 4;

            let mut values: Vec<String> = Vec::with_capacity(4);
            let mut current = String::new();
            let mut in_quote = false;
            let mut escaped = false;
            let mut quote = '\0';
            let mut paren_depth = 0i32;

            for ch in raw_trimmed.chars() {
                if in_quote {
                    current.push(ch);
                    if escaped {
                        escaped = false;
                    } else if ch == '\\' {
                        escaped = true;
                    } else if ch == quote {
                        in_quote = false;
                    }
                } else if escaped {
                    current.push(ch);
                    escaped = false;
                } else if ch == '\\' {
                    current.push(ch);
                    escaped = true;
                } else if ch == '"' || ch == '\'' {
                    in_quote = true;
                    quote = ch;
                    current.push(ch);
                } else if ch == '(' {
                    paren_depth += 1;
                    current.push(ch);
                } else if ch == ')' {
                    paren_depth -= 1;
                    current.push(ch);
                } else if is_qchar_space(ch) && paren_depth == 0 {
                    if !current.is_empty() {
                        values.push(std::mem::take(&mut current));
                    }
                } else {
                    current.push(ch);
                }
            }
            if !current.is_empty() {
                values.push(current);
            }
            if values.is_empty() || values.len() > MAXIMUM_MARGIN_VALUES {
                return;
            }

            let top = values[0].as_str();
            let right = if values.len() > 1 { values[1].as_str() } else { top };
            let bottom = if values.len() > 2 { values[2].as_str() } else { top };
            let left = if values.len() > 3 { values[3].as_str() } else { right };
            self.add_css_declaration("margin-top", top, declarations);
            self.add_css_declaration("margin-right", right, declarations);
            self.add_css_declaration("margin-bottom", bottom, declarations);
            self.add_css_declaration("margin-left", left, declarations);
            return;
        }

        let css_value: Cow<'_, str> = if raw_trimmed.contains('"') {
            Cow::Owned(raw_trimmed.replace('"', "'"))
        } else {
            Cow::Borrowed(raw_trimmed)
        };

        if name.as_ref() == "clip-path" && starts_with_ascii_ci(&css_value, "circle(") {
            declarations.insert(String::from("border-radius"), String::from("50%"));
            return;
        }

        /* Fast-path `!contains('(')` inside `has_unsupported_css_function`. */
        if has_unsupported_css_function(&css_value) {
            return;
        }

        if name.as_ref() == "text-decoration-line" {
            declarations.insert(String::from("text-decoration"), css_value.into_owned());
        } else if name.as_ref() == "content" {
            let content_value = crate::helpers::unquote_css_string(raw_trimmed)
                .unwrap_or_else(|| css_value.into_owned());
            declarations.insert(name.into_owned(), content_value);
        } else if name.as_ref() == "background" {
            if !css_value.contains(' ') && !css_value.contains("url(") {
                declarations.insert(String::from("background-color"), css_value.into_owned());
            }
        } else if name.as_ref() == "list-style" {
            for token in split_css_tokens(raw_trimmed) {
                let marker = normalize_list_marker(&token);
                if marker == "none" || is_built_in_list_marker(&marker) {
                    declarations.insert(String::from("list-style-type"), marker);
                    return;
                }
            }
        } else if name.as_ref() == "word-break" {
            if equals_ascii_ci(&css_value, "keep-all") {
                declarations.insert(String::from("white-space"), String::from("nowrap"));
            } else if equals_ascii_ci(&css_value, "normal") {
                declarations.insert(String::from("white-space"), String::from("normal"));
            }
        } else if is_supported_css_property(name.as_ref()) {
            declarations.insert(name.into_owned(), css_value.into_owned());
        }
    }

    /// Resolve a CSS value before adding it to a declaration map
    /// (`addResolvedCssDeclaration`, L435-447).
    pub fn add_resolved_css_declaration(
        &self,
        property: &str,
        value: &str,
        ctx: &mut Context,
        declarations: &mut CssDeclarations,
    ) {
        let resolved = self.resolve_css_value_cow(value, ctx);
        if resolved.is_empty() {
            return;
        }
        self.add_css_declaration(property, &resolved, declarations);
    }

    /// Add declarations to a CSS style attribute (`addCssDeclarations`,
    /// L449-466).
    pub fn add_css_declarations(&self, declarations: &CssDeclarations, out: &mut String) {
        for (key, value) in declarations {
            /* These are stored as metadata, not handled directly by styles */
            if key == "content" || key == "list-style-type" {
                continue;
            }

            out.push_str(key);
            out.push_str(": ");
            out.push_str(value);
            out.push(';');
        }
    }
}

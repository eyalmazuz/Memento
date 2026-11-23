////////////////////////////////////////////////////////////////////////////////
//
// Copyright (c) 2026 Ripose
//
// This file is part of Subomi.
//
// Subomi is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, version 2 of the License.
//
// Subomi is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with Subomi.  If not, see <https://www.gnu.org/licenses/>.
//
////////////////////////////////////////////////////////////////////////////////

//! Layout classification, CSS box-model resolution (padding, margins, borders,
//! composited background color), and list-marker computation.

use crate::css::text::{format_css_color, parse_css_color};
use crate::css::{split_css_tokens, Renderer};
use crate::element::{
    Context, ElementLayout, ElementRenderState, LayoutDirection, MarginFlow, StructuredList,
    WidthPolicy, BOTTOM, BOX_SIDE_COUNT, LEFT, RIGHT, TOP,
};
use crate::helpers::{
    css_font_size_to_pixels, format_pixel_size, is_built_in_list_marker, normalize_list_marker,
};

const MINIMUM_VISIBLE_PIXELS: f64 = 0.001;
const SIDE_NAMES: [&str; BOX_SIDE_COUNT] = ["top", "right", "bottom", "left"];
const PADDING_PROPERTIES: [&str; BOX_SIDE_COUNT] = [
    "padding-top",
    "padding-right",
    "padding-bottom",
    "padding-left",
];
const MARGIN_PROPERTIES: [&str; BOX_SIDE_COUNT] = [
    "margin-top",
    "margin-right",
    "margin-bottom",
    "margin-left",
];
const BORDER_PROPERTIES: [&str; BOX_SIDE_COUNT] = [
    "border-top",
    "border-right",
    "border-bottom",
    "border-left",
];
const BORDER_WIDTH_PROPERTIES: [&str; BOX_SIDE_COUNT] = [
    "border-top-width",
    "border-right-width",
    "border-bottom-width",
    "border-left-width",
];
const BORDER_STYLE_PROPERTIES: [&str; BOX_SIDE_COUNT] = [
    "border-top-style",
    "border-right-style",
    "border-bottom-style",
    "border-left-style",
];
const BORDER_COLOR_PROPERTIES: [&str; BOX_SIDE_COUNT] = [
    "border-top-color",
    "border-right-color",
    "border-bottom-color",
    "border-left-color",
];

#[derive(Clone, Default)]
struct ParsedBorder {
    width: String,
    style: String,
    color: String,
}

/// Parse border shorthand, retaining the last width/style and concatenating
/// color tokens just as `parseBorderShorthand` does (layout.cpp L93-135).
fn parse_border_shorthand(value: &str) -> ParsedBorder {
    let mut border = ParsedBorder::default();
    for token in split_css_tokens(value) {
        let normalized = token.to_lowercase();
        if matches!(
            normalized.as_str(),
            "none"
                | "hidden"
                | "dotted"
                | "dashed"
                | "solid"
                | "double"
                | "groove"
                | "ridge"
                | "inset"
                | "outset"
        ) {
            border.style = normalized;
        } else if matches!(normalized.as_str(), "thin" | "medium" | "thick")
            || normalized
                .chars()
                .next()
                .is_some_and(|ch| ch.is_ascii_digit() || matches!(ch, '.' | '+' | '-'))
        {
            border.width = token;
        } else {
            if !border.color.is_empty() {
                border.color.push(' ');
            }
            border.color.push_str(&token);
        }
    }
    border
}

fn composite_color(foreground: [u8; 4], background: [u8; 4]) -> Option<[u8; 4]> {
    let fg_a = f64::from(foreground[3]) / 255.0;
    let bg_a = f64::from(background[3]) / 255.0;
    let out_a = fg_a + bg_a * (1.0 - fg_a);
    if out_a <= 0.0 {
        return None;
    }
    let blend = |f: u8, b: u8| -> u8 {
        let v = (f64::from(f) / 255.0 * fg_a + f64::from(b) / 255.0 * bg_a * (1.0 - fg_a)) / out_a;
        (v.clamp(0.0, 1.0) * 255.0).round() as u8
    };
    Some([
        blend(foreground[0], background[0]),
        blend(foreground[1], background[1]),
        blend(foreground[2], background[2]),
        (out_a.clamp(0.0, 1.0) * 255.0).round() as u8,
    ])
}

/// `cssBoxSideValue` (layout.cpp L1259-1332). Splits Qt whitespace at
/// parenthesis depth zero. Invalid side indices or token counts outside 1..=4
/// yield an empty value.
pub fn css_box_side_value(value: &str, side: usize) -> &str {
    if side >= BOX_SIDE_COUNT {
        return "";
    }
    let source = value.trim();
    let mut values = [""; BOX_SIDE_COUNT];
    let mut count = 0usize;
    let mut depth = 0i32;
    let mut start = None;
    for (index, ch) in source.char_indices() {
        if ch == '(' {
            depth += 1;
        } else if ch == ')' {
            depth -= 1;
        }
        if ch.is_whitespace() && depth == 0 {
            if let Some(begin) = start.take() {
                if count == BOX_SIDE_COUNT {
                    return "";
                }
                values[count] = &source[begin..index];
                count += 1;
            }
        } else if start.is_none() {
            start = Some(index);
        }
    }
    if let Some(begin) = start {
        if count == BOX_SIDE_COUNT {
            return "";
        }
        values[count] = &source[begin..];
        count += 1;
    }
    match count {
        1 => values[0],
        2 => {
            if side == TOP || side == BOTTOM {
                values[0]
            } else {
                values[1]
            }
        }
        3 => match side {
            TOP => values[0],
            BOTTOM => values[2],
            _ => values[1],
        },
        BOX_SIDE_COUNT => values[side],
        _ => "",
    }
}

/// `isZeroSpacing` (layout.cpp L1334-1344). Empty values and numeric zero with
/// optional signed decimal zeroes and `[a-z%]+` units count as zero.
pub fn is_zero_spacing(spacing: &str) -> bool {
    let bytes = spacing.trim().as_bytes();
    if bytes.is_empty() {
        return true;
    }
    let mut index = usize::from(matches!(bytes[0], b'+' | b'-'));
    let integer_start = index;
    while index < bytes.len() && bytes[index] == b'0' {
        index += 1;
    }
    let integer_len = index - integer_start;
    let mut decimal_len = 0;
    if bytes.get(index) == Some(&b'.') {
        index += 1;
        let decimal_start = index;
        while index < bytes.len() && bytes[index] == b'0' {
            index += 1;
        }
        decimal_len = index - decimal_start;
    }
    if integer_len == 0 && decimal_len == 0 {
        return false;
    }
    index == bytes.len()
        || bytes[index..]
            .iter()
            .all(|byte| byte.is_ascii_alphabetic() || *byte == b'%')
}

/// `defaultListMarker` (layout.cpp L1359-1370).
pub fn default_list_marker(tag: &str) -> &str {
    match tag {
        "ol" => "1",
        "ul" => "disc",
        _ => "",
    }
}

/// `listMarker` (layout.cpp L1372-1423). A custom marker is emitted literally,
/// while alphabetic built-ins count bijectively (26 -> z, 27 -> aa).
pub fn list_marker(list: &StructuredList, marker: &str) -> String {
    let normalized = normalize_list_marker(marker);
    let selected = if normalized.is_empty() {
        &list.marker
    } else {
        &normalized
    };
    match selected.as_str() {
        "none" => String::new(),
        "disc" => "\u{2022}".into(),
        "circle" => "\u{25e6}".into(),
        "square" => "\u{25aa}".into(),
        "1" => format!("{}.", list.item),
        "a" | "A" => {
            let mut value = list.item;
            // A positive i64 uses at most 14 base-26 letters.
            let mut letters = [0u8; 14];
            let mut used = 0;
            while value > 0 {
                value -= 1;
                let letter = b'a' + (value % 26) as u8;
                letters[used] = if selected == "A" {
                    letter.to_ascii_uppercase()
                } else {
                    letter
                };
                used += 1;
                value /= 26;
            }
            let mut result = String::with_capacity(used + 1);
            for letter in letters[..used].iter().rev() {
                result.push(*letter as char);
            }
            result.push('.');
            result
        }
        _ if is_built_in_list_marker(selected) => list.marker.clone(),
        _ => selected.clone(),
    }
}

impl Renderer {
    /// `resolveElementLayout` (layout.cpp L237-350). Consumes display and
    /// list-specific declarations, advancing the active parent list once per
    /// item even when the item opts out of marker rendering.
    pub fn resolve_element_layout(&self, ctx: &mut Context, state: &mut ElementRenderState) {
        let display = state
            .declarations
            .remove("display")
            .unwrap_or_default()
            .trim()
            .to_lowercase();
        let list_type = state
            .declarations
            .get("list-style-type")
            .map(String::as_str)
            .unwrap_or("");
        state.list_marker_type = normalize_list_marker(if list_type.is_empty() {
            default_list_marker(&state.tag)
        } else {
            list_type
        });
        let is_list = state.tag == "ul" || state.tag == "ol";
        let is_list_item = state.tag == "li" && !ctx.lists.is_empty();
        let inline_list_item = is_list_item && display.eq_ignore_ascii_case("inline");
        let vertical_align = state
            .declarations
            .get("vertical-align")
            .map(String::as_str)
            .unwrap_or("");
        state.raised_text = vertical_align.trim().eq_ignore_ascii_case("super")
            || vertical_align.trim().eq_ignore_ascii_case("sub");
        if is_list_item {
            let list = ctx.lists.last_mut().expect("list presence checked");
            list.item += 1;
            state.list_marker = list_marker(list, list_type);
        }

        let styled_block = matches!(state.tag.as_str(), "div" | "details" | "summary")
            && state.declarations.keys().any(|property| {
                property == "background-color"
                    || property.starts_with("border")
                    || property.starts_with("padding")
            });
        state.padded_span = state.tag == "span"
            && ctx
                .elements
                .last()
                .and_then(|index| ctx.selector_elements.get(*index as usize))
                .is_some_and(|element| {
                    element
                        .attributes
                        .get("data-sc-class")
                        .is_some_and(|value| value == "tag")
                        || element
                            .attributes
                            .get("data-sc-content")
                            .is_some_and(|value| value == "tag" || value == "forms-label")
                });

        if is_list {
            state.layout = ElementLayout::List;
            state.output_tag = "div".into();
        } else if inline_list_item {
            state.layout = ElementLayout::Inline;
            state.output_tag = "span".into();
        } else if is_list_item && !state.list_marker.is_empty() {
            state.layout = ElementLayout::MarkedListItem;
            state.output_tag = "table".into();
            state.width_policy = WidthPolicy::Fill;
        } else if is_list_item {
            state.layout = ElementLayout::MarkerlessListItem;
            state.output_tag = "div".into();
        } else if state.tag == "details" || styled_block {
            state.layout = ElementLayout::Box;
            state.output_tag = "table".into();
            state.width_policy = WidthPolicy::Fill;
        } else if state.tag == "div" || state.tag == "summary" {
            state.layout = ElementLayout::Block;
            state.output_tag = "div".into();
        }
        if is_list || is_list_item {
            state.declarations.remove("list-style-type");
        }
        if state.list_marker_type == "none" {
            state.list_marker_type.clear();
        }
        let width = state
            .declarations
            .get("width")
            .map(String::as_str)
            .unwrap_or("");
        if state.layout == ElementLayout::Box
            && (width.eq_ignore_ascii_case("fit-content") || width.eq_ignore_ascii_case("max-content"))
        {
            state.width_policy = WidthPolicy::FitContent;
            state.declarations.remove("width");
        } else if state.layout == ElementLayout::Box && !width.is_empty() {
            state.width_policy = WidthPolicy::Explicit;
        }
    }

    /// `applyElementCompatibility` (layout.cpp L352-393).
    pub fn apply_element_compatibility(&self, ctx: &Context, state: &mut ElementRenderState) {
        if state.tag == "span" {
            let left = state.declarations.remove("margin-left").unwrap_or_default();
            let right = state
                .declarations
                .remove("margin-right")
                .unwrap_or_default();
            if state.text_direction == LayoutDirection::RightToLeft {
                state.inline_spacing_before = right;
                state.inline_spacing_after = left;
            } else {
                state.inline_spacing_before = left;
                state.inline_spacing_after = right;
            }
            if ctx.force_inline_box && ctx.suppress_inline_box_spacing {
                state.inline_spacing_before.clear();
                state.inline_spacing_after.clear();
                state.declarations.remove("vertical-align");
            }
        }
        if state.layout == ElementLayout::Inline && !ctx.force_inline_box {
            state.margin_flow = MarginFlow::Inline;
            return;
        }
        if state.layout == ElementLayout::Inline {
            state.layout = ElementLayout::Box;
            state.output_tag = "table".into();
            state.width_policy = WidthPolicy::FitContent;
        }
        self.resolve_box_style(ctx, state);
        state.margin_flow = self.margin_flow(state);
    }

    /// `resolveBoxStyle` (layout.cpp L395-685). Longhand overrides shorthand,
    /// with borders resolved in common, side, then side-longhand order.
    pub fn resolve_box_style(&self, ctx: &Context, state: &mut ElementRenderState) {
        let declarations = &mut state.declarations;
        let has_padding = declarations.contains_key("padding")
            || PADDING_PROPERTIES
                .iter()
                .any(|property| declarations.contains_key(*property));
        if has_padding {
            let shorthand = declarations.remove("padding").unwrap_or_default();
            for (side, property) in PADDING_PROPERTIES.iter().enumerate() {
                let longhand = declarations.remove(*property).unwrap_or_default();
                let value = if longhand.is_empty() {
                    css_box_side_value(&shorthand, side)
                } else {
                    &longhand
                };
                let pixels = css_font_size_to_pixels(
                    value,
                    ctx.screen.as_ref(),
                    state.font_pixel_size,
                    ctx.root_font_pixel_size,
                );
                if pixels > MINIMUM_VISIBLE_PIXELS {
                    state.box_style.padding[side] = pixels;
                }
            }
        }
        for (side, property) in MARGIN_PROPERTIES.iter().enumerate() {
            state.box_style.margins[side] = declarations.remove(*property).unwrap_or_default();
        }
        if state.layout == ElementLayout::Block && state.text_only_content {
            let left = std::mem::take(&mut state.box_style.margins[LEFT]);
            let right = std::mem::take(&mut state.box_style.margins[RIGHT]);
            if state.text_direction == LayoutDirection::RightToLeft {
                state.content_inline_spacing_before = right;
                state.content_inline_spacing_after = left;
            } else {
                state.content_inline_spacing_before = left;
                state.content_inline_spacing_after = right;
            }
        }

        if let Some(background) = declarations.remove("background-color") {
            let resolved = parse_css_color(&background).map(|foreground| {
                if foreground[3] < u8::MAX {
                    parse_css_color(&ctx.painted_background_color)
                        .and_then(|parent| composite_color(foreground, parent))
                        .unwrap_or(foreground)
                } else {
                    foreground
                }
            });
            if let Some(color) = resolved {
                state.box_style.background_color = format_css_color(color);
                state
                    .painted_background_color
                    .clone_from(&state.box_style.background_color);
            } else {
                state.box_style.background_color = background;
            }
        }

        let has_border = declarations
            .keys()
            .any(|name| name == "border" || name.starts_with("border-"));
        if has_border {
            let common = parse_border_shorthand(&declarations.remove("border").unwrap_or_default());
            let widths = declarations.remove("border-width").unwrap_or_default();
            let styles = declarations.remove("border-style").unwrap_or_default();
            let colors = declarations.remove("border-color").unwrap_or_default();
            let mut painted_count = 0;
            let mut shared_color = String::new();
            let mut shared_color_matches = true;
            for (side, name) in SIDE_NAMES.iter().enumerate() {
                let mut border = common.clone();
                for (replacement, source) in [
                    (&mut border.width, css_box_side_value(&widths, side)),
                    (&mut border.style, css_box_side_value(&styles, side)),
                    (&mut border.color, css_box_side_value(&colors, side)),
                ] {
                    if !source.is_empty() {
                        replacement.clear();
                        replacement.push_str(source);
                    }
                }
                let side_border = parse_border_shorthand(
                    &declarations
                        .remove(BORDER_PROPERTIES[side])
                        .unwrap_or_default(),
                );
                if !side_border.width.is_empty() {
                    border.width = side_border.width;
                }
                if !side_border.style.is_empty() {
                    border.style = side_border.style;
                }
                if !side_border.color.is_empty() {
                    border.color = side_border.color;
                }
                for (property, replacement) in [
                    (BORDER_WIDTH_PROPERTIES[side], &mut border.width),
                    (BORDER_STYLE_PROPERTIES[side], &mut border.style),
                    (BORDER_COLOR_PROPERTIES[side], &mut border.color),
                ] {
                    if let Some(value) = declarations.remove(property) {
                        *replacement = value;
                    }
                }
                if border.width.is_empty() && border.style.eq_ignore_ascii_case("solid") {
                    border.width = "3px".into();
                }
                let mut width_pixels = css_font_size_to_pixels(
                    &border.width,
                    ctx.screen.as_ref(),
                    state.font_pixel_size,
                    ctx.root_font_pixel_size,
                );
                if border.width.eq_ignore_ascii_case("thin") {
                    width_pixels = 1.0;
                } else if border.width.eq_ignore_ascii_case("medium") {
                    width_pixels = 3.0;
                } else if border.width.eq_ignore_ascii_case("thick") {
                    width_pixels = 5.0;
                }
                if border.color.is_empty() || border.color.eq_ignore_ascii_case("currentcolor") {
                    border.color.clone_from(&state.text_color);
                }
                let resolved = &mut state.box_style.borders[side];
                resolved.color = border.color;
                resolved.style = border.style;
                resolved.width_pixels = width_pixels.max(0.0);
                resolved.painted = resolved.style.eq_ignore_ascii_case("solid")
                    && resolved.width_pixels > MINIMUM_VISIBLE_PIXELS
                    && parse_css_color(&resolved.color).is_some();
                if resolved.painted {
                    painted_count += 1;
                    if shared_color.is_empty() {
                        shared_color.clone_from(&resolved.color);
                    } else if shared_color != resolved.color {
                        shared_color_matches = false;
                    }
                } else if !resolved.style.is_empty()
                    && !resolved.style.eq_ignore_ascii_case("none")
                    && resolved.width_pixels > MINIMUM_VISIBLE_PIXELS
                {
                    state.cell_declarations.insert(
                        format!("border-{name}"),
                        format!(
                            "{}px {} {}",
                            format_pixel_size(resolved.width_pixels),
                            resolved.style,
                            resolved.color
                        ),
                    );
                }
            }
            state.box_style.compact_border_frame = painted_count > 0 && shared_color_matches;
            state.box_style.enabled = state.layout == ElementLayout::Box
                || painted_count > 0
                || !state.box_style.background_color.is_empty()
                || state
                    .box_style
                    .padding
                    .iter()
                    .any(|padding| *padding > MINIMUM_VISIBLE_PIXELS);
        } else {
            state.box_style.enabled = state.layout == ElementLayout::Box
                || !state.box_style.background_color.is_empty()
                || has_padding;
        }
        declarations.remove("border-radius");
        let has_horizontal_margins = !is_zero_spacing(&state.box_style.margins[LEFT])
            || !is_zero_spacing(&state.box_style.margins[RIGHT]);
        if state.layout == ElementLayout::Block && !state.text_only_content && has_horizontal_margins
        {
            state.layout = ElementLayout::Box;
            state.output_tag = "table".into();
            state.width_policy = WidthPolicy::Fill;
            state.box_style.enabled = true;
        }
    }

    /// `marginFlow` (layout.cpp L687-718).
    pub fn margin_flow(&self, state: &ElementRenderState) -> MarginFlow {
        if state.layout == ElementLayout::Inline {
            return MarginFlow::Inline;
        }
        let vertical_padding = state.box_style.padding[TOP] > MINIMUM_VISIBLE_PIXELS
            || state.box_style.padding[BOTTOM] > MINIMUM_VISIBLE_PIXELS;
        let painted_border = state.box_style.borders.iter().any(|border| border.painted);
        if state.box_style.enabled
            && (!state.box_style.background_color.is_empty() || vertical_padding || painted_border)
        {
            MarginFlow::Contained
        } else if state.layout == ElementLayout::MarkedListItem {
            MarginFlow::PropagateLastChild
        } else {
            MarginFlow::Collapsible
        }
    }
}

/// `fontPixelSize` (layout.cpp L1655-1673). Qt's `QFont` values are supplied
/// by the caller, preserving its pixel-size preference over typographic points.
pub fn font_pixel_size(
    pixel_size: i32,
    point_size: f64,
    screen: Option<&crate::element::ScreenMetrics>,
) -> f64 {
    if pixel_size > 0 {
        f64::from(pixel_size)
    } else if point_size > 0.0 {
        point_size * crate::helpers::screen_dpi(screen) / 72.0
    } else {
        16.0
    }
}

/// `cssFontSizeToPixels(QString, QScreen*, QFont)` (layout.cpp L1437-1444):
/// extract Qt's pixel/point size at the boundary and use the same resolved
/// font size for both parent and root-relative units.
pub fn css_font_size_to_pixels_with_font(
    size: &str,
    screen: Option<&crate::element::ScreenMetrics>,
    font_pixel_size_value: i32,
    font_point_size: f64,
) -> f64 {
    let base = font_pixel_size(font_pixel_size_value, font_point_size, screen);
    css_font_size_to_pixels(size, screen, base, base)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::element::{ScreenMetrics, StructuredElement};

    fn element(tag: &str) -> ElementRenderState {
        ElementRenderState {
            tag: tag.into(),
            output_tag: tag.into(),
            font_pixel_size: 16.0,
            ..ElementRenderState::default()
        }
    }

    fn declare(state: &mut ElementRenderState, property: &str, value: &str) {
        state.declarations.insert(property.into(), value.into());
    }

    #[test]
    fn box_shorthand_respects_parentheses_and_css_side_order() {
        for (shorthand, expected) in [
            ("3px", ["3px", "3px", "3px", "3px"]),
            ("1px 2px", ["1px", "2px", "1px", "2px"]),
            ("1px 2px 3px", ["1px", "2px", "3px", "2px"]),
            ("1px 2px 3px 4px", ["1px", "2px", "3px", "4px"]),
            (
                " 1px\tcalc(2px + 3px) 4px ",
                ["1px", "calc(2px + 3px)", "4px", "calc(2px + 3px)"],
            ),
        ] {
            for (side, expected) in expected.into_iter().enumerate() {
                assert_eq!(css_box_side_value(shorthand, side), expected);
            }
        }
        assert_eq!(css_box_side_value("", TOP), "");
        assert_eq!(css_box_side_value("1px 2px 3px 4px 5px", LEFT), "");
        assert_eq!(css_box_side_value("1px", BOX_SIDE_COUNT), "");
        assert_eq!(css_box_side_value("1px\u{00a0}2px", RIGHT), "2px");
    }

    #[test]
    fn zero_spacing_rejects_nonzero_or_non_css_zero() {
        for spacing in ["", " \t", "+0", "-.000", "00.00PX", "0%", "0."] {
            assert!(is_zero_spacing(spacing), "{spacing}");
        }
        for spacing in ["1px", "0.1px", "0e0", "0.0 px", "0_", "+", ".px"] {
            assert!(!is_zero_spacing(spacing), "{spacing}");
        }
    }

    #[test]
    fn list_markers_cover_overrides_and_alphabetic_boundary() {
        assert_eq!(default_list_marker("ol"), "1");
        assert_eq!(default_list_marker("ul"), "disc");
        assert_eq!(default_list_marker("li"), "");
        let mut list = StructuredList {
            tag: "ol".into(),
            marker: "1".into(),
            item: 1,
        };
        assert_eq!(list_marker(&list, ""), "1.");
        list.item = 26;
        assert_eq!(list_marker(&list, "a"), "z.");
        list.item = 27;
        assert_eq!(list_marker(&list, "a"), "aa.");
        list.item = 52;
        assert_eq!(list_marker(&list, "A"), "AZ.");
        list.item = 53;
        assert_eq!(list_marker(&list, "A"), "BA.");
        assert_eq!(list_marker(&list, "\"<next>&\""), "<next>&");
        assert_eq!(list_marker(&list, "none"), "");
        assert_eq!(list_marker(&list, "circle"), "◦");
        assert_eq!(list_marker(&list, "square"), "▪");
        list.marker = "disc".into();
        assert_eq!(list_marker(&list, ""), "•");
    }

    #[test]
    fn resolving_layout_advances_list_and_handles_markerless_inline_and_fit() {
        let renderer = Renderer::new();
        let mut ctx = Context::default();
        ctx.lists.push(StructuredList {
            tag: "ol".into(),
            marker: "a".into(),
            item: 25,
        });
        let mut li = element("li");
        declare(&mut li, "list-style-type", "A");
        renderer.resolve_element_layout(&mut ctx, &mut li);
        assert_eq!(ctx.lists[0].item, 26);
        assert_eq!(li.list_marker, "Z.");
        assert_eq!(li.layout, ElementLayout::MarkedListItem);
        assert_eq!(li.width_policy, WidthPolicy::Fill);
        assert_eq!(li.output_tag, "table");
        assert!(!li.declarations.contains_key("list-style-type"));

        let mut inline = element("li");
        declare(&mut inline, "display", " inline ");
        declare(&mut inline, "list-style-type", "none");
        renderer.resolve_element_layout(&mut ctx, &mut inline);
        assert_eq!(ctx.lists[0].item, 27);
        assert_eq!(inline.layout, ElementLayout::Inline);
        assert_eq!(inline.output_tag, "span");
        assert_eq!(inline.list_marker_type, "");
        let mut markerless = element("li");
        declare(&mut markerless, "list-style-type", "none");
        renderer.resolve_element_layout(&mut ctx, &mut markerless);
        assert_eq!(markerless.layout, ElementLayout::MarkerlessListItem);

        let mut details = element("details");
        declare(&mut details, "width", "fit-content");
        renderer.resolve_element_layout(&mut ctx, &mut details);
        assert_eq!(details.layout, ElementLayout::Box);
        assert_eq!(details.width_policy, WidthPolicy::FitContent);
        assert!(!details.declarations.contains_key("width"));
        let mut explicit = element("details");
        declare(&mut explicit, "width", "24px");
        renderer.resolve_element_layout(&mut ctx, &mut explicit);
        assert_eq!(explicit.width_policy, WidthPolicy::Explicit);
        assert_eq!(explicit.declarations["width"], "24px");
    }

    #[test]
    fn forced_span_box_and_right_to_left_spacing_match_compatibility_rules() {
        let renderer = Renderer::new();
        let ctx = Context {
            force_inline_box: true,
            ..Context::default()
        };
        let mut span = element("span");
        span.text_direction = LayoutDirection::RightToLeft;
        declare(&mut span, "margin-left", "2px");
        declare(&mut span, "margin-right", "4px");
        renderer.apply_element_compatibility(&ctx, &mut span);
        assert_eq!(span.inline_spacing_before, "4px");
        assert_eq!(span.inline_spacing_after, "2px");
        assert_eq!(span.layout, ElementLayout::Box);
        assert_eq!(span.width_policy, WidthPolicy::FitContent);

        let ctx = Context {
            suppress_inline_box_spacing: true,
            ..ctx
        };
        let mut suppressed = element("span");
        declare(&mut suppressed, "margin-left", "2px");
        declare(&mut suppressed, "vertical-align", "super");
        renderer.apply_element_compatibility(&ctx, &mut suppressed);
        assert!(suppressed.inline_spacing_after.is_empty());
        assert!(!suppressed.declarations.contains_key("vertical-align"));
    }

    #[test]
    fn box_style_normalizes_padding_margins_and_border_precedence() {
        let renderer = Renderer::new();
        let mut state = element("div");
        state.layout = ElementLayout::Box;
        state.text_color = "#010203".into();
        state.painted_background_color = "#ffffff".into();
        declare(&mut state, "padding", "1px 2px 3px 4px");
        declare(&mut state, "padding-left", "9px");
        declare(&mut state, "margin-left", "2em");
        declare(&mut state, "border", "2px solid red");
        declare(&mut state, "border-width", "1px 3px");
        declare(&mut state, "border-right", "dotted");
        declare(&mut state, "border-top-color", "#112233");
        declare(&mut state, "border-radius", "3px");
        declare(&mut state, "background-color", "transparent");
        renderer.resolve_box_style(
            &Context {
                painted_background_color: "#ffffff".into(),
                ..Context::default()
            },
            &mut state,
        );
        assert_eq!(state.box_style.padding, [1.0, 2.0, 3.0, 9.0]);
        assert_eq!(state.box_style.margins[LEFT], "2em");
        assert_eq!(state.box_style.background_color, "#ffffff");
        assert_eq!(state.painted_background_color, "#ffffff");
        assert_eq!(state.box_style.borders[TOP].width_pixels, 1.0);
        assert_eq!(state.box_style.borders[TOP].color, "#112233");
        assert!(state.box_style.borders[TOP].painted);
        assert_eq!(state.box_style.borders[RIGHT].width_pixels, 3.0);
        assert!(!state.box_style.borders[RIGHT].painted);
        assert_eq!(state.cell_declarations["border-right"], "3px dotted red");
        assert!(!state.box_style.compact_border_frame);
        assert!(!state.declarations.contains_key("border-radius"));
    }

    #[test]
    fn border_shorthand_parses_ascii_width_style_and_color() {
        let renderer = Renderer::new();
        let mut state = element("div");
        state.layout = ElementLayout::Box;
        declare(&mut state, "border", "3px solid #112233");

        renderer.resolve_box_style(&Context::default(), &mut state);

        assert_eq!(state.box_style.borders[TOP].width_pixels, 3.0);
        assert_eq!(state.box_style.borders[TOP].color, "#112233");
        assert!(state.box_style.borders[TOP].painted);
    }

    #[test]
    fn margin_flow_respects_visible_edges() {
        let renderer = Renderer::new();
        let mut item = element("li");
        item.layout = ElementLayout::MarkedListItem;
        assert_eq!(renderer.margin_flow(&item), MarginFlow::PropagateLastChild);
        item.box_style.enabled = true;
        item.box_style.padding[TOP] = 2.0;
        assert_eq!(renderer.margin_flow(&item), MarginFlow::Contained);
        item.box_style.padding[TOP] = 0.0;
        item.box_style.background_color = "#ffffff".into();
        assert_eq!(renderer.margin_flow(&item), MarginFlow::Contained);
        item.box_style.background_color.clear();
        assert_eq!(renderer.margin_flow(&item), MarginFlow::PropagateLastChild);
    }

    #[test]
    fn font_size_prefers_pixels_else_points_else_browser_default() {
        let screen = ScreenMetrics {
            available_width: 1200,
            available_height: 800,
            logical_dpi: 120.0,
        };
        assert_eq!(font_pixel_size(15, 12.0, Some(&screen)), 15.0);
        assert_eq!(font_pixel_size(-1, 12.0, Some(&screen)), 20.0);
        assert_eq!(font_pixel_size(-1, 12.0, None), 16.0);
        assert_eq!(font_pixel_size(-1, -1.0, Some(&screen)), 16.0);
        assert_eq!(
            css_font_size_to_pixels_with_font("2em", Some(&screen), -1, 12.0),
            40.0
        );
        assert_eq!(
            css_font_size_to_pixels_with_font("1rem", None, 14, 10.0),
            14.0
        );
    }

    #[test]
    fn padded_span_detects_selected_structured_data() {
        let renderer = Renderer::new();
        let mut ctx = Context::default();
        let mut selector = StructuredElement::new();
        selector
            .attributes
            .insert("data-sc-content".into(), "forms-label".into());
        ctx.selector_elements.push(selector);
        ctx.elements.push(0);
        let mut span = element("span");
        renderer.resolve_element_layout(&mut ctx, &mut span);
        assert!(span.padded_span);
    }

    #[test]
    fn text_only_block_moves_horizontal_margins_to_inline_spacers() {
        let renderer = Renderer::new();
        let mut text = element("div");
        text.layout = ElementLayout::Block;
        text.text_only_content = true;
        text.text_direction = LayoutDirection::RightToLeft;
        text.declarations.insert("margin-left".into(), "2px".into());
        text.declarations.insert("margin-right".into(), "3px".into());
        renderer.apply_element_compatibility(&Context::default(), &mut text);
        assert_eq!(text.layout, ElementLayout::Block);
        assert_eq!(text.margin_flow, MarginFlow::Collapsible);
        assert!(!text.box_style.enabled);
        assert_eq!(text.content_inline_spacing_before, "3px");
        assert_eq!(text.content_inline_spacing_after, "2px");
        assert_eq!(text.box_style.margins[LEFT], "");
        assert_eq!(text.box_style.margins[RIGHT], "");

        let mut children = element("div");
        children.layout = ElementLayout::Block;
        children
            .declarations
            .insert("margin-left".into(), "2px".into());
        renderer.apply_element_compatibility(&Context::default(), &mut children);
        assert_eq!(children.layout, ElementLayout::Box);
        assert_eq!(children.width_policy, WidthPolicy::Fill);
        assert!(children.box_style.enabled);
    }
}

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

//! Complete port of `src/definition/structuredrichtext_css.cpp` (L1-1228):
//! CSS matching and Qt rich-text compatibility for structured content.
//!
//! Function to C++ line mapping:
//!
//! | Rust item | structuredrichtext_css.cpp |
//! |---|---|
//! | [`split_css_tokens`] | `splitCssTokens` L38-90 |
//! | [`Renderer::add_structured_style`] | `addStructuredStyle` L92-218 |
//! | [`Renderer::resolve_element_styles`] | `resolveElementStyles` L220-276 |
//! | [`Renderer::add_css_declaration`] | `addCssDeclaration` L278-433 |
//! | [`Renderer::add_resolved_css_declaration`] | `addResolvedCssDeclaration` L435-447 |
//! | [`Renderer::add_css_declarations`] | `addCssDeclarations` L449-466 |
//! | [`Renderer::resolve_css_value`] | `resolveCssValue` L468-502 |
//! | [`Renderer::resolve_css_variables`] | `resolveCssVariables` L504-602 |
//! | [`Renderer::resolve_css_calculations`] | `resolveCssCalculations` L604-658 |
//! | [`Renderer::resolve_color_mix`] | `resolveColorMix` L660-773 |
//! | [`Renderer::resolve_css_calc`] | `resolveCssCalc` L775-798 |
//! | [`Renderer::css_gradient_fallback`] | `cssGradientFallback` L800-829 |
//! | [`Renderer::split_css_arguments`] | `splitCssArguments` L831-878 |
//! | [`Renderer::add_matching_css_rules`] | `addMatchingCssRules` L880-950 |
//! | [`Renderer::css_rule_matches`] | `cssRuleMatches` L952-969 |
//! | [`Renderer::css_rule_matches_at`] | `cssRuleMatchesAt` L971-1054 |
//! | [`Renderer::css_selector_part_matches`] | `cssSelectorPartMatches` L1056-1132 |
//! | [`Renderer::structured_element`] | `structuredElement` L1134-1202 |
//! | [`Renderer::structured_data_attribute_name`] | `structuredDataAttributeName` L1204-1221 |
//! | [`Renderer::is_supported_structured_tag`] | `isSupportedStructuredTag` L1223-1226 |
//! | [`scanners::candidate_rule_indexes_for`] mirrors `DictionaryStyles::candidateRuleIndexes(tag, childIndex)` (dictionarystyles.cpp L81-104) |
//!
//! PORT-NOTE (code-unit offsets): QString indexes/sizes are UTF-16 code
//! units, while Rust `String` carries UTF-8 bytes. Every position produced or
//! consumed here comes from searching for an ASCII needle (`var(`, `calc(`,
//! `in `, `color-mix(`) or scanning for ASCII characters (`(`/`)`/`,`/quote
//! /`%`), so each index is a `char` boundary in UTF-8, and slicing and
//! replacement (`&s[a..b]`, `String::replace_range(a..b+1)`) mirror
//! `QString::sliced`/`replace(pos, count, s)` with lengths composed in the
//! same byte units throughout. Per-function subtleties carry their own
//! `PORT-NOTE` comments (content-quote peeling L391-396,
//! `resolveColorMix`, `$`-anchor behavior of the percentage regexes).

mod scanners;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod tests_engine;

use std::sync::Arc;

use crate::color::Color;
use crate::element::{
    Context, CssAttributeOperator, CssCombinator, CssDeclarations, CssPseudoClass,
    CssPseudoElement, CssRule, CssSelectorPart, ElementRenderState, ParsedStylesheet,
    StructuredElement,
};
use crate::helpers::{
    css_font_size_to_pixels, format_pixel_size, is_built_in_list_marker, normalize_list_marker,
};
use crate::json::Json;
use crate::qstring::{
    c_double, contains_ascii_ci, equals_ascii_ci, find_ascii_ci, is_qchar_space,
    is_qchar_upper, qstring_number, starts_with_ascii_ci, to_lower, to_lower_char, trimmed,
};

use scanners::{css_function_captures, is_supported_css_property, is_supported_css_function};

/**
 * Split a CSS value into whitespace-delimited top-level tokens
 * (`splitCssTokens`, L38-90).
 *
 * Whitespace inside quoted strings and function parentheses is preserved.
 */
pub fn split_css_tokens(value: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut quote = '\0';
    let mut in_quote = false;
    let mut parenthesis_depth = 0i32;

    for ch in trimmed(value).chars() {
        if in_quote {
            current.push(ch);
            if ch == quote {
                in_quote = false;
            }
        } else if ch == '"' || ch == '\'' {
            in_quote = true;
            quote = ch;
            current.push(ch);
        } else if ch == '(' {
            parenthesis_depth += 1;
            current.push(ch);
        } else if ch == ')' {
            parenthesis_depth -= 1;
            current.push(ch);
        } else if is_qchar_space(ch) && parenthesis_depth == 0 {
            if !current.is_empty() {
                tokens.push(std::mem::take(&mut current));
            }
        } else {
            current.push(ch);
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

/* Recursion guards (structuredrichtext_css.cpp L508-509, L606-607) and the
 * color-mix sentinel (L677-678), hoisted to named constants. */
const MAX_VARIABLE_REPLACEMENTS: i32 = 32;
const MAX_CALC_REPLACEMENTS: i32 = 16;
const UNSPECIFIED_PERCENTAGE: f64 = -1.0;

/// Stateless stand-in for the C++ `Renderer` (all its members are constant
/// data; the instance carries no runtime state).
#[derive(Clone, Copy, Debug, Default)]
pub struct Renderer;

impl Renderer {
    pub const fn new() -> Renderer {
        Renderer
    }

    /// Add structured style declarations to the render state
    /// (`addStructuredStyle`, L92-218).
    pub fn add_structured_style(
        &self,
        obj: &Json,
        ctx: &mut Context,
        state: &mut ElementRenderState,
    ) {
        /* Structured style key that changes the inherited font size */
        const KEY_FONT_SIZE: &str = "fontSize";

        /* The `addString`/`addSpacing` lambdas (L100-134). */
        macro_rules! add_string {
            ($key:expr, $property:expr) => {
                if obj.get($key).is_string() {
                    self.add_resolved_css_declaration(
                        $property,
                        &obj.get($key).to_string_value(),
                        ctx,
                        &mut state.declarations,
                    );
                }
            };
        }
        macro_rules! add_spacing {
            ($key:expr, $property:expr) => {
                if obj.get($key).is_string() {
                    self.add_resolved_css_declaration(
                        $property,
                        &obj.get($key).to_string_value(),
                        ctx,
                        &mut state.declarations,
                    );
                } else if obj.get($key).is_double() {
                    /* QString::number(double) is %g with precision 6
                     * (L128-L133). */
                    self.add_resolved_css_declaration(
                        $property,
                        &format!("{}em", qstring_number(obj.get($key).to_double())),
                        ctx,
                        &mut state.declarations,
                    );
                }
            };
        }

        add_string!("fontStyle", "font-style");
        add_string!("fontWeight", "font-weight");
        if obj.get(KEY_FONT_SIZE).is_string() {
            let font_size = self.resolve_css_value(obj.get(KEY_FONT_SIZE).as_str(), ctx);
            if !font_size.is_empty() {
                let pixels = css_font_size_to_pixels(
                    &font_size,
                    ctx.screen.as_ref(),
                    ctx.parent_font_pixel_size,
                    ctx.root_font_pixel_size,
                );
                if pixels >= 0.0 {
                    state.font_pixel_size = pixels;
                    self.add_css_declaration(
                        "font-size",
                        &format!("{}px", format_pixel_size(pixels)),
                        &mut state.declarations,
                    );
                } else {
                    self.add_css_declaration("font-size", &font_size, &mut state.declarations);
                }
            }
        }

        add_string!("color", "color");
        add_string!("background", "background");
        add_string!("backgroundColor", "background-color");
        if obj.get("textDecorationLine").is_array() {
            let mut lines: Vec<String> = Vec::new();
            for value in obj.get("textDecorationLine").as_array() {
                if value.is_string() {
                    lines.push(value.to_string_value());
                }
            }
            self.add_resolved_css_declaration(
                "text-decoration-line",
                &lines.join(" "),
                ctx,
                &mut state.declarations,
            );
        } else {
            add_string!("textDecorationLine", "text-decoration-line");
        }
        add_string!("textDecorationStyle", "text-decoration-style");
        add_string!("textDecorationColor", "text-decoration-color");
        add_string!("borderColor", "border-color");
        add_string!("borderStyle", "border-style");
        add_string!("borderRadius", "border-radius");
        add_string!("borderWidth", "border-width");
        add_string!("clipPath", "clip-path");
        add_string!("verticalAlign", "vertical-align");
        add_string!("textAlign", "text-align");
        add_string!("textEmphasis", "text-emphasis");
        add_string!("textShadow", "text-shadow");
        add_spacing!("margin", "margin");
        add_spacing!("marginTop", "margin-top");
        add_spacing!("marginRight", "margin-right");
        add_spacing!("marginBottom", "margin-bottom");
        add_spacing!("marginLeft", "margin-left");
        add_string!("padding", "padding");
        add_string!("paddingTop", "padding-top");
        add_string!("paddingRight", "padding-right");
        add_string!("paddingBottom", "padding-bottom");
        add_string!("paddingLeft", "padding-left");
        add_string!("wordBreak", "word-break");
        add_string!("whiteSpace", "white-space");
        add_string!("cursor", "cursor");
        add_string!("listStyleType", "list-style-type");
    }

    /// Resolve matched and inline styles for an element
    /// (`resolveElementStyles`, L220-276).
    pub fn resolve_element_styles(
        &self,
        obj: &Json,
        ctx: &mut Context,
        state: &mut ElementRenderState,
    ) {
        /* Structured-content key containing direct style declarations */
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
            /* L244-249: the C++ passes pointers to the before/after content;
             * disjoint field borrows of `state` stand in for them. */
            let ElementRenderState {
                declarations,
                before_content,
                after_content,
                ..
            } = state;
            self.add_matching_css_rules(
                ctx,
                declarations,
                Some(before_content),
                Some(after_content),
            );
        }

        if obj.get(KEY_STYLE).is_object() {
            let style = obj.get(KEY_STYLE).to_object();
            self.add_structured_style(&style, ctx, state);
        }
        if let Some(value) = state.declarations.get("font-size").cloned() {
            let pixel_size = css_font_size_to_pixels(
                &value,
                ctx.screen.as_ref(),
                ctx.parent_font_pixel_size,
                ctx.root_font_pixel_size,
            );
            if pixel_size >= 0.0 {
                state.font_pixel_size = pixel_size;
            }
        }
        if let Some(color) = state.declarations.get("color").cloned() {
            state.text_color = color;
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
        let name = to_lower(trimmed(property));
        let css_value = trimmed(value).to_string();

        if name.is_empty() || css_value.is_empty() {
            return;
        }

        let mut css_value = css_value.replace('"', "'");

        if name == "margin" {
            /* Maximum component count permitted by the CSS margin shorthand */
            const MAXIMUM_MARGIN_VALUES: usize = 4;

            /* L298-346 duplicates the splitCssTokens scanner over the already
             * trimmed value (the C++ does not re-trim here). */
            let mut values: Vec<String> = Vec::new();
            let mut current = String::new();
            let mut in_quote = false;
            let mut quote = '\0';
            let mut paren_depth = 0i32;

            for ch in css_value.chars() {
                if in_quote {
                    current.push(ch);
                    if ch == quote {
                        in_quote = false;
                    }
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
                        values.push(current.clone());
                        current.clear();
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

            let top = values[0].clone();
            let right = if values.len() > 1 { values[1].clone() } else { top.clone() };
            let bottom = if values.len() > 2 { values[2].clone() } else { top.clone() };
            let left = if values.len() > 3 { values[3].clone() } else { right.clone() };
            self.add_css_declaration("margin-top", &top, declarations);
            self.add_css_declaration("margin-right", &right, declarations);
            self.add_css_declaration("margin-bottom", &bottom, declarations);
            self.add_css_declaration("margin-left", &left, declarations);
            return;
        }

        if name == "clip-path"
            && starts_with_ascii_ci(&css_value, "circle(")
        {
            declarations.insert(String::from("border-radius"), String::from("50%"));
            return;
        }

        /* Make sure the function is supported by Qt rich text before using it
         * (L372-382): every `\b([A-Za-z-]+)\s*\(` capture must be a
         * supported function or the declaration is dropped entirely. */
        for function_name in css_function_captures(&css_value) {
            let function_name = to_lower(&function_name);
            if !is_supported_css_function(&function_name) {
                return;
            }
        }

        if name == "text-decoration-line" {
            declarations.insert(String::from("text-decoration"), css_value);
        } else if name == "content" {
            /* Remove quotes before injecting text. */
            //
            // PORT-NOTE (utf16 peeling, L391-396): the C++ guards on
            // `size() >= 2` UTF-16 code units and peels with
            // `sliced(1, size() - 2)`, matching when the front/back code units
            // are a BMP quote. `String` carries UTF-8; both quote marks are
            // single BMP code units and a non-BMP char can never equal one, so
            // peeling one leading/trailing `char` is equivalent to one
            // leading/trailing code unit here (a lone surrogate at either edge
            // cannot exist in Rust string data).
            let chars: Vec<char> = css_value.chars().collect();
            if chars.len() >= 2
                && ((chars[0] == '"' && chars[chars.len() - 1] == '"')
                    || (chars[0] == '\'' && chars[chars.len() - 1] == '\''))
            {
                css_value = chars[1..chars.len() - 1].iter().collect();
            }
            declarations.insert(name, css_value);
        } else if name == "background" {
            if !css_value.contains(' ') && !css_value.contains("url(") {
                declarations.insert(String::from("background-color"), css_value);
            }
        } else if name == "list-style" {
            for token in split_css_tokens(&css_value) {
                let marker = normalize_list_marker(&token);
                if marker == "none" || is_built_in_list_marker(&marker) {
                    declarations.insert(String::from("list-style-type"), marker);
                    return;
                }
            }
        } else if name == "word-break" {
            if equals_ascii_ci(&css_value, "keep-all") {
                declarations.insert(String::from("white-space"), String::from("nowrap"));
            } else if equals_ascii_ci(&css_value, "normal") {
                declarations.insert(String::from("white-space"), String::from("normal"));
            }
        } else if is_supported_css_property(&name) {
            declarations.insert(name, css_value);
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
        let resolved = self.resolve_css_value(value, ctx);
        if resolved.is_empty() {
            return;
        }
        self.add_css_declaration(property, &resolved, declarations);
    }

    /// Add declarations to a CSS style attribute (`addCssDeclarations`,
    /// L449-466).
    ///
    /// PORT-NOTE: `declarations` was a `QMap`, which iterates in sorted key
    /// order; the [`CssDeclarations`] alias models that with a sorted
    /// container, so the emitted order matches exactly.
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

    /// Resolve browser CSS values to values supported by Qt rich text
    /// (`resolveCssValue`, L468-502).
    pub fn resolve_css_value(&self, value: &str, ctx: &mut Context) -> String {
        /* PORT-NOTE: the cache is keyed by the raw (untrimmed) source value,
         * exactly as in C++. */
        if let Some(cached) = ctx.resolved_css_values.get(value) {
            return cached.clone();
        }

        let mut resolved = self.resolve_css_variables(trimmed(value), ctx);
        if resolved.is_empty() {
            ctx.resolved_css_values.insert(value.to_string(), String::new());
            return String::new();
        }

        resolved = self.resolve_css_calculations(&resolved);
        if resolved.is_empty() {
            ctx.resolved_css_values.insert(value.to_string(), String::new());
            return String::new();
        }

        if starts_with_ascii_ci(&resolved, "color-mix(") {
            resolved = self.resolve_color_mix(&resolved);
        } else if contains_ascii_ci(&resolved, "gradient(") {
            resolved = self.css_gradient_fallback(&resolved);
        }
        ctx.resolved_css_values.insert(value.to_string(), resolved.clone());
        resolved
    }

    /// Resolve CSS variable references in a declaration value
    /// (`resolveCssVariables`, L504-602).
    pub fn resolve_css_variables(&self, value: &str, ctx: &Context) -> String {
        /* Character count of the opening "var(" token */
        const CSS_VARIABLE_PREFIX_LENGTH: usize = 4;

        let mut value = String::from(value);
        for _replacement_count in 0..MAX_VARIABLE_REPLACEMENTS {
            let Some(start) = find_ascii_ci(&value, "var(", 0) else {
                return value;
            };

            let mut end: Option<usize> = None;
            let mut depth = 1i32;
            let bytes = value.as_bytes();
            for i in start + CSS_VARIABLE_PREFIX_LENGTH..bytes.len() {
                if bytes[i] == b'(' {
                    depth += 1;
                } else if bytes[i] == b')' {
                    depth -= 1;
                    if depth == 0 {
                        end = Some(i);
                        break;
                    }
                }
            }
            let Some(end) = end else {
                return String::new();
            };

            let arguments =
                self.split_css_arguments(&value[start + CSS_VARIABLE_PREFIX_LENGTH..end]);
            if arguments.is_empty() {
                return String::new();
            }

            let name = to_lower(trimmed(&arguments[0]));
            let fallback = if arguments.len() > 1 {
                arguments[1..].join(",")
            } else {
                String::new()
            };

            let replacement = if name == "--text-color" || name == "--fg" {
                if ctx.glossary_text_color.is_empty() {
                    fallback
                } else {
                    ctx.glossary_text_color.clone()
                }
            } else if name == "--font-size-no-units" {
                format_pixel_size(ctx.root_font_pixel_size)
            } else if name == "--background-color" || name == "--canvas" {
                if ctx.glossary_background_color.is_empty() {
                    fallback
                } else {
                    ctx.glossary_background_color.clone()
                }
            } else {
                fallback
            };

            if replacement.is_empty() {
                return String::new();
            }
            let replacement = self.resolve_css_variables(&replacement, ctx);
            if replacement.is_empty() {
                return String::new();
            }
            value.replace_range(start..end + 1, &replacement);
        }

        String::new()
    }

    /// Resolve supported calc functions embedded in a CSS value
    /// (`resolveCssCalculations`, L604-658).
    pub fn resolve_css_calculations(&self, value: &str) -> String {
        /* Character count of the opening "calc(" token */
        const CSS_CALC_PREFIX_LENGTH: usize = 5;

        let mut value = String::from(value);
        for _replacement_count in 0..MAX_CALC_REPLACEMENTS {
            let Some(start) = find_ascii_ci(&value, "calc(", 0) else {
                return value;
            };

            let mut end: Option<usize> = None;
            let mut depth = 1i32;
            let bytes = value.as_bytes();
            for i in start + CSS_CALC_PREFIX_LENGTH..bytes.len() {
                if bytes[i] == b'(' {
                    depth += 1;
                } else if bytes[i] == b')' {
                    depth -= 1;
                    if depth == 0 {
                        end = Some(i);
                        break;
                    }
                }
            }
            let Some(end) = end else {
                return String::new();
            };

            let replacement = self.resolve_css_calc(&value[start..end + 1]);
            if replacement.is_empty() {
                return String::new();
            }
            value.replace_range(start..end + 1, &replacement);
        }

        String::new()
    }

    /// Resolve a color-mix function to a concrete color (`resolveColorMix`,
    /// L660-773).
    pub fn resolve_color_mix(&self, value: &str) -> String {
        /* Number of colors supported by the compatibility color-mix parser */
        const COLOR_COUNT: usize = 2;

        /* Character count of the opening "color-mix(" token */
        const COLOR_MIX_PREFIX_LENGTH: usize = 10;

        /* Character count of a CSS function's closing parenthesis */
        const CSS_FUNCTION_SUFFIX_LENGTH: usize = 1;

        /* Equal weighting used when neither mixed color specifies a percentage */
        const DEFAULT_COLOR_PERCENTAGE: f64 = 50.0;

        /* Percentage total used to infer one omitted color weight */
        const TOTAL_COLOR_PERCENTAGE: f64 = 100.0;

        if !value.ends_with(')') {
            return String::new();
        }

        /* PORT-NOTE: QString::sliced asserts a non-negative length; this
         * reproduces that precondition defensively for direct calls shorter
         * than "color-mix(" + ")" (unreachable from resolveCssValue, which
         * checks a case-insensitive "color-mix(" start first). */
        if value.len() < COLOR_MIX_PREFIX_LENGTH + CSS_FUNCTION_SUFFIX_LENGTH {
            return String::new();
        }

        let mut arguments = self
            .split_css_arguments(&value[COLOR_MIX_PREFIX_LENGTH
                ..value.len() - CSS_FUNCTION_SUFFIX_LENGTH]);
        if let Some(front) = arguments.first() {
            if starts_with_ascii_ci(front, "in ") {
                arguments.remove(0);
            }
        }
        if arguments.len() != COLOR_COUNT {
            return String::new();
        }

        let mut colors = [None, None];
        let mut percentages = [UNSPECIFIED_PERCENTAGE, UNSPECIFIED_PERCENTAGE];

        for i in 0..COLOR_COUNT {
            let mut color_value = trimmed(&arguments[i]).to_string();
            if let Some((match_start, number)) =
                scanners::match_trailing_percentage(&color_value, true)
            {
                /* captured(1).toDouble(): the capture spans digits and an
                 * optional fraction only, so the C-locale parse always
                 * succeeds (fallback 0.0 mirrors the Qt no-ok toDouble). */
                percentages[i] = c_double(number).unwrap_or(0.0);
                color_value = trimmed(&color_value[..match_start]).to_string();
            }
            let Some(color) = Color::from_string(&color_value) else {
                return String::new();
            };
            colors[i] = Some(color);
        }

        if percentages[0] < 0.0 && percentages[1] < 0.0 {
            percentages[0] = DEFAULT_COLOR_PERCENTAGE;
            percentages[1] = DEFAULT_COLOR_PERCENTAGE;
        } else if percentages[0] < 0.0 {
            percentages[0] = TOTAL_COLOR_PERCENTAGE - percentages[1];
        } else if percentages[1] < 0.0 {
            percentages[1] = TOTAL_COLOR_PERCENTAGE - percentages[0];
        }

        let total = percentages[0] + percentages[1];
        if total <= 0.0 {
            return String::new();
        }
        let first_weight = percentages[0] / total;
        let second_weight = percentages[1] / total;
        let (c0, c1) = (colors[0].unwrap(), colors[1].unwrap());
        /* PORT-NOTE (float seams): QColor channels are float (quint16/65535);
         * the C++ performs the mixing in `double`, then Qt converts each
         * fromRgbF argument to `float`. Mixed in f64 and cast per channel at
         * the call, mirroring exactly. */
        let alpha =
            c0.alpha_f() as f64 * first_weight + c1.alpha_f() as f64 * second_weight;
        if alpha <= 0.0 {
            /* QColor(Qt::transparent).name(QColor::HexArgb) */
            return Color::transparent().name_hex_argb();
        }

        Color::from_rgb_f(
            ((c0.red_f() as f64 * c0.alpha_f() as f64 * first_weight
                + c1.red_f() as f64 * c1.alpha_f() as f64 * second_weight)
                / alpha) as f32,
            ((c0.green_f() as f64 * c0.alpha_f() as f64 * first_weight
                + c1.green_f() as f64 * c1.alpha_f() as f64 * second_weight)
                / alpha) as f32,
            ((c0.blue_f() as f64 * c0.alpha_f() as f64 * first_weight
                + c1.blue_f() as f64 * c1.alpha_f() as f64 * second_weight)
                / alpha) as f32,
            alpha as f32,
        )
        .name_hex_argb()
    }

    /// Resolve a simple calc division to a concrete length
    /// (`resolveCssCalc`, L775-798).
    pub fn resolve_css_calc(&self, value: &str) -> String {
        // The DIVISION regex (L778-782, PCRE2, case-insensitive); it is
        // reproduced verbatim below. Line comments are used so the literal
        // star-slash sequence in "\s*/\s*" cannot terminate a block comment.
        // ^calc\(\s*([+-]?(?:\d+(?:\.\d*)?|\.\d+))([a-z%]+)\s*/\s*
        // ([+-]?(?:\d+(?:\.\d*)?|\.\d+))\s*\)$
        //
        // PORT-NOTE: hand-parsed equivalent (no regex crate); see
        // [`scanners::match_calc_division`].
        let Some((number, unit, denominator)) = scanners::match_calc_division(value) else {
            return String::new();
        };

        if denominator == 0.0 {
            return String::new();
        }
        format!("{}{}", format_pixel_size(number / denominator), unit)
    }

    /// Select a solid-color fallback from a CSS gradient
    /// (`cssGradientFallback`, L800-829).
    pub fn css_gradient_fallback(&self, value: &str) -> String {
        let Some(open) = value.find('(') else {
            return String::new();
        };
        if !value.ends_with(')') {
            return String::new();
        }

        /* value.sliced(open + 1, value.size() - open - 2); the guard mirrors
         * the QString::sliced assert precondition (always satisfied by
         * reachable inputs, which end in ')' after an explicit '('). */
        if open + 1 > value.len().saturating_sub(2) {
            return String::new();
        }
        let arguments = self.split_css_arguments(&value[open + 1..value.len() - 2]);
        let Some(first) = arguments.first() else {
            return String::new();
        };

        let mut color_value = trimmed(first).to_string();
        /* COLOR_STOP regex (L818-820): \s+[0-9]+(?:\.[0-9]+)?%\s*$ */
        if let Some((match_start, _)) =
            scanners::match_trailing_percentage(&color_value, false)
        {
            color_value = trimmed(&color_value[..match_start]).to_string();
        }

        match Color::from_string(&color_value) {
            Some(color) => color.name_hex_argb(),
            None => String::new(),
        }
    }

    /// Split a CSS function argument list at top-level commas
    /// (`splitCssArguments`, L831-878).
    pub fn split_css_arguments(&self, arguments: &str) -> Vec<String> {
        let mut result = Vec::new();
        let mut current = String::new();
        let mut in_quote = false;
        let mut quote = '\0';
        let mut paren_depth = 0i32;

        for ch in arguments.chars() {
            if in_quote {
                current.push(ch);
                if ch == quote {
                    in_quote = false;
                }
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
            } else if ch == ',' && paren_depth == 0 {
                result.push(trimmed(&current).to_string());
                current.clear();
            } else {
                current.push(ch);
            }
        }
        result.push(trimmed(&current).to_string());
        result
    }

    /// Apply matching stylesheet rules to a declaration map
    /// (`addMatchingCssRules`, L880-950).
    pub fn add_matching_css_rules(
        &self,
        ctx: &mut Context,
        declarations: &mut CssDeclarations,
        mut before_content: Option<&mut String>,
        mut after_content: Option<&mut String>,
    ) {
        if ctx.dictionary_styles.is_none() || ctx.elements.is_empty() {
            return;
        }

        /* Cloned from the Arc up front so `ctx` stays freely mutable below. */
        let stylesheet: Arc<ParsedStylesheet> = ctx.dictionary_styles.clone().unwrap();

        let element =
            &ctx.selector_elements[(*ctx.elements.last().unwrap()) as usize];
        let candidate_indexes = scanners::candidate_rule_indexes_for(
            &stylesheet,
            &element.tag,
            element.child_index,
        )
        .to_vec();

        for index in candidate_indexes {
            let rule = &stylesheet.rules[index];
            if !self.css_rule_matches(rule, ctx) {
                continue;
            }
            if rule.pseudo_element != CssPseudoElement::None {
                let content: Option<&mut String> = if rule.pseudo_element
                    == CssPseudoElement::After
                {
                    after_content.as_mut().map(|s| &mut **s)
                } else {
                    before_content.as_mut().map(|s| &mut **s)
                };
                if let Some(content) = content {
                    for declaration in &rule.declarations {
                        if declaration.property == "content" {
                            *content = declaration.value.clone();
                        }
                    }
                }
                continue;
            }

            for declaration in &rule.declarations {
                let mut value = self.resolve_css_value(&declaration.value, ctx);
                if value.is_empty() {
                    continue;
                }
                if declaration.property == "font-size" {
                    let pixel_size = css_font_size_to_pixels(
                        &value,
                        ctx.screen.as_ref(),
                        ctx.parent_font_pixel_size,
                        ctx.root_font_pixel_size,
                    );
                    if pixel_size >= 0.0 {
                        value = format!("{}px", format_pixel_size(pixel_size));
                    }
                }
                self.add_css_declaration(&declaration.property, &value, declarations);
            }
        }
    }

    /// Check if a rule matches the current element stack (`cssRuleMatches`,
    /// L952-969).
    pub fn css_rule_matches(&self, rule: &CssRule, ctx: &Context) -> bool {
        if rule.selector.is_empty() || ctx.elements.is_empty() {
            return false;
        }

        let stack_index = (ctx.elements.len() - 1) as i64;
        self.css_rule_matches_at(
            rule,
            ctx,
            rule.selector.len() as i64 - 1,
            stack_index,
            ctx.elements[stack_index as usize],
        )
    }

    /// Match a selector recursively from a candidate element
    /// (`cssRuleMatchesAt`, L971-1054).
    pub fn css_rule_matches_at(
        &self,
        rule: &CssRule,
        ctx: &Context,
        part_index: i64,
        stack_index: i64,
        selector_element_index: i64,
    ) -> bool {
        let element = &ctx.selector_elements[selector_element_index as usize];
        if part_index < 0
            || !self
                .css_selector_part_matches(&rule.selector[part_index as usize], element)
        {
            return false;
        }
        if part_index == 0 {
            return true;
        }

        let previous_sibling = element.previous_sibling;
        match rule.selector[part_index as usize].combinator {
            CssCombinator::Child => {
                if stack_index <= 0 {
                    return false;
                }
                self.css_rule_matches_at(
                    rule,
                    ctx,
                    part_index - 1,
                    stack_index - 1,
                    ctx.elements[(stack_index - 1) as usize],
                )
            }

            CssCombinator::AdjacentSibling => {
                if previous_sibling < 0 {
                    return false;
                }
                self.css_rule_matches_at(
                    rule,
                    ctx,
                    part_index - 1,
                    stack_index,
                    previous_sibling,
                )
            }

            CssCombinator::GeneralSibling => {
                let mut sibling = previous_sibling;
                while sibling >= 0 {
                    if self
                        .css_rule_matches_at(rule, ctx, part_index - 1, stack_index, sibling)
                    {
                        return true;
                    }
                    sibling = ctx.selector_elements[sibling as usize].previous_sibling;
                }
                false
            }

            CssCombinator::Descendant => {
                let mut i = stack_index - 1;
                while i >= 0 {
                    if self.css_rule_matches_at(
                        rule,
                        ctx,
                        part_index - 1,
                        i,
                        ctx.elements[i as usize],
                    ) {
                        return true;
                    }
                    i -= 1;
                }
                false
            }
        }
    }

    /// Check if a selector part matches an element (`cssSelectorPartMatches`,
    /// L1056-1132).
    ///
    /// PORT-NOTE (hash order): `part.class_names` was a QSet iterated only to
    /// test membership in `element.classes`; the outcome is iteration-order
    /// independent, so the Rust hash set preserves behavior.
    pub fn css_selector_part_matches(
        &self,
        part: &CssSelectorPart,
        element: &StructuredElement,
    ) -> bool {
        if !part.tag.is_empty() && part.tag != element.tag {
            return false;
        }
        for class_name in &part.class_names {
            if !element.classes.contains(class_name) {
                return false;
            }
        }
        for attribute in &part.attributes {
            let Some(value) = element.attributes.get(&attribute.name) else {
                return false;
            };
            match attribute.op {
                CssAttributeOperator::Exists => {}

                CssAttributeOperator::Equals => {
                    if *value != attribute.value {
                        return false;
                    }
                }

                CssAttributeOperator::StartsWith => {
                    /* QString::startsWith: case-sensitive by default. */
                    if !value.starts_with(&attribute.value) {
                        return false;
                    }
                }
            }
        }
        if !part.pseudo_classes.is_empty() && element.child_index <= 0 {
            return false;
        }
        for pseudo in &part.pseudo_classes {
            let matched = match pseudo.pseudo_type {
                CssPseudoClass::FirstChild => element.child_index == 1,

                CssPseudoClass::LastChild => {
                    if element.child_count <= 0 {
                        return false;
                    }
                    element.child_index == element.child_count
                }

                CssPseudoClass::NthChild => {
                    element.child_index == pseudo.child_index as i64
                }
            };

            if if pseudo.negated { matched } else { !matched } {
                return false;
            }
        }
        true
    }

    /// Convert structured data to element attributes (`structuredElement`,
    /// L1134-1202). Returns the index of the element in
    /// `ctx.selector_elements`.
    pub fn structured_element(
        &self,
        obj: &Json,
        ctx: &mut Context,
        details_open: bool,
    ) -> i64 {
        /* Structured-content keys recorded for CSS selector matching */
        const KEY_DATA: &str = "data";
        const KEY_HREF: &str = "href";
        const KEY_LANG: &str = "lang";
        const KEY_TAG: &str = "tag";
        const KEY_TITLE: &str = "title";

        let mut element = StructuredElement::new();
        element.tag = to_lower(&obj.get(KEY_TAG).to_string_value());
        element.classes.insert(format!("gloss-sc-{}", element.tag));
        if element.tag == "details" && details_open {
            element.attributes.insert(String::from("open"), String::new());
        }
        if element.tag == "img" {
            element.classes.insert(String::from("gloss-image"));
            element.classes.insert(String::from("gloss-image-container"));
            element.classes.insert(String::from("gloss-image-link"));
        }

        if obj.get(KEY_DATA).is_object() {
            let data = obj.get(KEY_DATA).to_object();
            /* PORT-NOTE (hash order): QJsonObject::keys() walks its sorted
             * key list, mirrored by the sorted map inside [`Json`]; the
             * resulting attribute map is only ever looked up, never
             * iterated, in this engine. */
            for key in data.keys() {
                if data.get(&key).is_string() {
                    element.attributes.insert(
                        self.structured_data_attribute_name(&key),
                        data.get(&key).to_string_value(),
                    );
                }
            }
        }

        /* Standard attributes copied into the selector-node attribute map */
        for key in [KEY_HREF, KEY_LANG, KEY_TITLE] {
            if obj.get(key).is_string() {
                element
                    .attributes
                    .insert(String::from(key), obj.get(key).to_string_value());
            }
        }

        if let Some(siblings) = ctx.siblings.last_mut() {
            element.previous_sibling = siblings.previous_element;
            siblings.visited_element_count += 1;
            element.child_index = siblings.visited_element_count;
            element.child_count = siblings.element_count;
        }

        let index = ctx.selector_elements.len() as i64;
        ctx.selector_elements.push(element);
        if let Some(siblings) = ctx.siblings.last_mut() {
            siblings.previous_element = index;
        }
        index
    }

    /// Convert a structured data key to an HTML data attribute name
    /// (`structuredDataAttributeName`, L1204-1221).
    pub fn structured_data_attribute_name(&self, key: &str) -> String {
        let mut out = String::from("data-sc-");
        for ch in key.chars() {
            if is_qchar_upper(ch) {
                out.push('-');
                out.push(to_lower_char(ch));
            } else {
                out.push(ch);
            }
        }
        out
    }

    /// Check if a structured content tag is supported
    /// (`isSupportedStructuredTag`, L1223-1226).
    pub fn is_supported_structured_tag(&self, tag: &str) -> bool {
        scanners::is_supported_structured_tag(tag)
    }
}

pub use scanners::candidate_rule_indexes_for;

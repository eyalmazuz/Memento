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

//! CSS value resolution (`var()`, `calc()`, `color-mix()`, gradients) and
//! top-level token / argument splitting.

use std::borrow::Cow;

use crate::color::Color;
use crate::element::Context;
use crate::helpers::format_pixel_size;
use crate::qstring::{
    c_double, contains_ascii_ci, find_ascii_ci_span, is_qchar_space, starts_with_ascii_ci,
    strip_prefix_ascii_ci, to_lower, trimmed,
};

use super::scanners;
use super::Renderer;

/* Recursion / substitution guards (structuredrichtext_css.cpp L508-509,
 * L606-607) and the color-mix sentinel (L677-678). */
const MAX_VARIABLE_REPLACEMENTS: usize = 32;
const MAX_VARIABLE_DEPTH: usize = 32;
const MAX_CALC_REPLACEMENTS: usize = 16;
const UNSPECIFIED_PERCENTAGE: f64 = -1.0;

/// Split a CSS value into whitespace-delimited top-level tokens
/// (`splitCssTokens`, L38-90).
///
/// Whitespace inside quoted strings and function parentheses is preserved.
pub fn split_css_tokens(value: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut quote = '\0';
    let mut in_quote = false;
    let mut escaped = false;
    let mut parenthesis_depth = 0i32;

    for ch in trimmed(value).chars() {
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

impl Renderer {
    /// Borrow-friendly CSS value resolver: fast-paths values without `'('`
    /// before touching `ctx.resolved_css_values` or running any function pass.
    pub(crate) fn resolve_css_value_cow<'a>(
        &self,
        value: &'a str,
        ctx: &mut Context,
    ) -> Cow<'a, str> {
        if !value.contains('(') {
            return Cow::Borrowed(trimmed(value));
        }

        /* PORT-NOTE: the cache is keyed by the raw (untrimmed) source value. */
        if let Some(cached) = ctx.resolved_css_values.get(value) {
            return Cow::Owned(cached.clone());
        }

        let mut resolved = self.resolve_css_variables(trimmed(value), ctx);
        if resolved.is_empty() {
            ctx.resolved_css_values
                .insert(value.to_string(), String::new());
            return Cow::Borrowed("");
        }

        resolved = self.resolve_css_calculations(&resolved);
        if resolved.is_empty() {
            ctx.resolved_css_values
                .insert(value.to_string(), String::new());
            return Cow::Borrowed("");
        }

        if starts_with_ascii_ci(&resolved, "color-mix(") {
            resolved = self.resolve_color_mix(&resolved);
        } else if contains_ascii_ci(&resolved, "gradient(") {
            resolved = self.css_gradient_fallback(&resolved);
        }
        ctx.resolved_css_values
            .insert(value.to_string(), resolved.clone());
        Cow::Owned(resolved)
    }

    /// Resolve browser CSS values to values supported by Qt rich text
    /// (`resolveCssValue`, L468-502).
    pub fn resolve_css_value(&self, value: &str, ctx: &mut Context) -> String {
        self.resolve_css_value_cow(value, ctx).into_owned()
    }

    /// Resolve CSS variable references in a declaration value
    /// (`resolveCssVariables`, L504-602) with a shared total-substitution
    /// budget and explicit recursion depth cap.
    pub fn resolve_css_variables(&self, value: &str, ctx: &Context) -> String {
        // Match the original 32-iteration loop semantics: up to 31 substitutions
        // succeed (32nd pass sees no remaining `var(`), while 32+ abort with "".
        let mut remaining_budget = MAX_VARIABLE_REPLACEMENTS - 1;
        self.resolve_css_variables_inner(value, ctx, &mut remaining_budget, 0)
    }

    fn resolve_css_variables_inner(
        &self,
        value: &str,
        ctx: &Context,
        remaining_budget: &mut usize,
        depth: usize,
    ) -> String {
        if depth > MAX_VARIABLE_DEPTH {
            return String::new();
        }
        if !value.contains('(') {
            return value.to_string();
        }

        let mut value = String::from(value);
        loop {
            let Some((start, prefix_end)) = find_ascii_ci_span(&value, "var(", 0) else {
                return value;
            };
            if *remaining_budget == 0 {
                return String::new();
            }
            *remaining_budget -= 1;

            let mut end: Option<usize> = None;
            let mut paren_depth = 1i32;
            let mut in_quote = false;
            let mut escaped = false;
            let mut quote = '\0';
            for (offset, ch) in value[prefix_end..].char_indices() {
                if in_quote {
                    if escaped {
                        escaped = false;
                    } else if ch == '\\' {
                        escaped = true;
                    } else if ch == quote {
                        in_quote = false;
                    }
                } else if escaped {
                    escaped = false;
                } else if ch == '\\' {
                    escaped = true;
                } else if ch == '"' || ch == '\'' {
                    in_quote = true;
                    quote = ch;
                } else if ch == '(' {
                    paren_depth += 1;
                } else if ch == ')' {
                    paren_depth -= 1;
                    if paren_depth == 0 {
                        end = Some(prefix_end + offset);
                        break;
                    }
                }
            }
            let Some(end) = end else {
                return String::new();
            };

            let arguments = self.split_css_arguments(&value[prefix_end..end]);
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
            let replacement =
                self.resolve_css_variables_inner(&replacement, ctx, remaining_budget, depth + 1);
            if replacement.is_empty() {
                return String::new();
            }
            value.replace_range(start..end + 1, &replacement);
        }
    }

    /// Resolve supported calc functions embedded in a CSS value
    /// (`resolveCssCalculations`, L604-658).
    pub fn resolve_css_calculations(&self, value: &str) -> String {
        if !value.contains('(') {
            return value.to_string();
        }
        let mut value = String::from(value);
        for _replacement_count in 0..MAX_CALC_REPLACEMENTS {
            let Some((start, prefix_end)) = find_ascii_ci_span(&value, "calc(", 0) else {
                return value;
            };

            let mut end: Option<usize> = None;
            let mut depth = 1i32;
            let bytes = value.as_bytes();
            for (i, &b) in bytes.iter().enumerate().skip(prefix_end) {
                if b == b'(' {
                    depth += 1;
                } else if b == b')' {
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

        /* Equal weighting used when neither mixed color specifies a percentage */
        const DEFAULT_COLOR_PERCENTAGE: f64 = 50.0;

        /* Percentage total used to infer one omitted color weight */
        const TOTAL_COLOR_PERCENTAGE: f64 = 100.0;

        let Some(inner) = strip_prefix_ascii_ci(value, "color-mix(").and_then(|s| s.strip_suffix(')'))
        else {
            return String::new();
        };

        let mut arguments = self.split_css_arguments(inner);
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
        let alpha = c0.alpha_f() as f64 * first_weight + c1.alpha_f() as f64 * second_weight;
        if alpha <= 0.0 {
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
        if open + 1 > value.len().saturating_sub(1) {
            return String::new();
        }
        let arguments = self.split_css_arguments(&value[open + 1..value.len() - 1]);
        let Some(first) = arguments.first() else {
            return String::new();
        };

        let mut color_value = trimmed(first).to_string();
        if let Some((match_start, _)) = scanners::match_trailing_percentage(&color_value, false) {
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
        let mut escaped = false;
        let mut quote = '\0';
        let mut paren_depth = 0i32;

        for ch in arguments.chars() {
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
}

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

//! CSS value resolution (`var()`, `calc()`, `color-mix()`, gradients) and
//! top-level token / argument splitting.

use std::borrow::Cow;
use std::sync::Arc;

use crate::element::{Context, CssTheme};
use crate::helpers::format_pixel_size;

use super::scanners;
use super::text::{
    contains_ascii_ci, find_ascii_ci, format_css_color, parse_css_color, parse_css_f64,
    starts_with_ascii_ci, strip_prefix_ascii_ci,
};
use super::Renderer;

/* Recursion / substitution guards (structuredrichtext_css.cpp L508-509,
 * L606-607) and the color-mix sentinel (L677-678). */
const MAX_VARIABLE_REPLACEMENTS: usize = 32;
const MAX_VARIABLE_DEPTH: usize = 32;
const MAX_CALC_REPLACEMENTS: usize = 16;
const UNSPECIFIED_PERCENTAGE: f64 = -1.0;

enum VarResolve {
    Ok(String),
    InvalidOrCycle,
    BudgetExceeded,
}

fn stylesheet_ptr(ctx: &Context) -> usize {
    ctx.dictionary_styles
        .as_ref()
        .map_or(0, |sheet| Arc::as_ptr(sheet) as usize)
}

fn has_element_custom_properties(ctx: &Context) -> bool {
    ctx.elements.iter().any(|&idx| {
        ctx.selector_elements
            .get(idx as usize)
            .is_some_and(|el| !el.custom_properties.is_empty())
    })
}

fn resolve_builtin_palette_color<'a>(
    ctx_color: &'a str,
    ctx_theme: CssTheme,
    active_theme: CssTheme,
    explicit_theme: bool,
    theme_default: fn(CssTheme) -> &'static str,
) -> Option<&'a str> {
    if !ctx_color.is_empty() {
        if explicit_theme
            && active_theme != ctx_theme
            && ctx_color.eq_ignore_ascii_case(theme_default(ctx_theme))
        {
            return Some(theme_default(active_theme));
        }
        return Some(ctx_color);
    }
    if explicit_theme || active_theme == CssTheme::Light {
        return Some(theme_default(active_theme));
    }
    None
}

fn lookup_scoped_custom_property<'a>(
    raw_name: &str,
    lower_name: &str,
    ctx: &'a Context,
    theme: CssTheme,
) -> Option<(&'a str, &'a str)> {
    // 1. Element-scoped custom properties (innermost element -> outermost ancestor).
    for &idx in ctx.elements.iter().rev() {
        if let Some(el) = ctx.selector_elements.get(idx as usize) {
            if let Some((k, val)) = el.custom_properties.get_key_value(raw_name) {
                return Some((k.as_str(), val.as_str()));
            }
            if raw_name != lower_name {
                if let Some((k, val)) = el.custom_properties.get_key_value(lower_name) {
                    return Some((k.as_str(), val.as_str()));
                }
            }
        }
    }
    // 2. Per-dictionary `:root` / `:root[data-theme="..."]` custom properties.
    ctx.dictionary_styles.as_deref().and_then(|sheet| {
        let theme_map = match theme {
            CssTheme::Dark => &sheet.dark_root_variables,
            CssTheme::Light => &sheet.light_root_variables,
        };
        if let Some((k, v)) = theme_map.get_key_value(raw_name) {
            return Some((k.as_str(), v.as_str()));
        }
        if let Some((k, v)) = sheet.root_variables.get_key_value(raw_name) {
            return Some((k.as_str(), v.as_str()));
        }
        if raw_name != lower_name {
            if let Some((k, v)) = theme_map.get_key_value(lower_name) {
                return Some((k.as_str(), v.as_str()));
            }
            if let Some((k, v)) = sheet.root_variables.get_key_value(lower_name) {
                return Some((k.as_str(), v.as_str()));
            }
        }
        None
    })
}

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

    for ch in value.trim().chars() {
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
        } else if ch.is_whitespace() && parenthesis_depth == 0 {
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
            return Cow::Borrowed(value.trim());
        }

        let current_sheet_ptr = stylesheet_ptr(ctx);
        if ctx.cached_css_theme != ctx.theme || ctx.cached_stylesheet_ptr != current_sheet_ptr {
            ctx.resolved_css_values.clear();
            ctx.cached_css_theme = ctx.theme;
            ctx.cached_stylesheet_ptr = current_sheet_ptr;
        }

        let use_cache = !has_element_custom_properties(ctx);
        if use_cache {
            /* PORT-NOTE: the cache is keyed by the raw (untrimmed) source value. */
            if let Some(cached) = ctx.resolved_css_values.get(value) {
                return Cow::Owned(cached.clone());
            }
        }

        let mut resolved = self.resolve_css_variables(value.trim(), ctx);
        if resolved.is_empty() {
            if use_cache {
                ctx.resolved_css_values
                    .insert(value.to_string(), String::new());
            }
            return Cow::Borrowed("");
        }

        resolved = self.resolve_css_calculations(&resolved);
        if resolved.is_empty() {
            if use_cache {
                ctx.resolved_css_values
                    .insert(value.to_string(), String::new());
            }
            return Cow::Borrowed("");
        }

        if starts_with_ascii_ci(&resolved, "color-mix(") {
            resolved = self.resolve_color_mix(&resolved);
        } else if contains_ascii_ci(&resolved, "gradient(") {
            resolved = self.css_gradient_fallback(&resolved);
        }
        if use_cache {
            ctx.resolved_css_values
                .insert(value.to_string(), resolved.clone());
        }
        Cow::Owned(resolved)
    }

    /// Resolve browser CSS values to values supported by Qt rich text
    /// (`resolveCssValue`, L468-502). Uses `ctx.theme` (defaulting to
    /// [`CssTheme::Dark`]).
    pub fn resolve_css_value(&self, value: &str, ctx: &mut Context) -> String {
        self.resolve_css_value_cow(value, ctx).into_owned()
    }

    /// Resolve browser CSS values under an explicit [`CssTheme`] (`Dark` or
    /// `Light`) without mutating `ctx.theme` or polluting `ctx.resolved_css_values`.
    pub fn resolve_css_value_with_theme(
        &self,
        value: &str,
        ctx: &mut Context,
        theme: CssTheme,
    ) -> String {
        if !value.contains('(') {
            return value.trim().to_owned();
        }
        let mut resolved = self.resolve_css_variables_with_theme(value.trim(), ctx, theme);
        if resolved.is_empty() {
            return String::new();
        }
        resolved = self.resolve_css_calculations(&resolved);
        if resolved.is_empty() {
            return String::new();
        }
        if starts_with_ascii_ci(&resolved, "color-mix(") {
            resolved = self.resolve_color_mix(&resolved);
        } else if contains_ascii_ci(&resolved, "gradient(") {
            resolved = self.css_gradient_fallback(&resolved);
        }
        resolved
    }

    /// Resolve CSS variable references in a declaration value
    /// (`resolveCssVariables`, L504-602) with a shared total-substitution
    /// budget, explicit recursion depth cap, and cycle detection.
    pub fn resolve_css_variables(&self, value: &str, ctx: &Context) -> String {
        // Match the original 32-iteration loop semantics: up to 31 substitutions
        // succeed (32nd pass sees no remaining `var(`), while 32+ abort with "".
        let mut remaining_budget = MAX_VARIABLE_REPLACEMENTS - 1;
        let mut active_vars = Vec::new();
        match self.resolve_css_variables_inner(
            value,
            ctx,
            ctx.theme,
            false,
            &mut remaining_budget,
            0,
            &mut active_vars,
        ) {
            VarResolve::Ok(resolved) => resolved,
            VarResolve::InvalidOrCycle | VarResolve::BudgetExceeded => String::new(),
        }
    }

    /// Resolve CSS variable references under an explicit [`CssTheme`] (`Dark`
    /// or `Light`), applying built-in theme palette defaults when `ctx` does
    /// not provide an explicit override color.
    pub fn resolve_css_variables_with_theme(
        &self,
        value: &str,
        ctx: &Context,
        theme: CssTheme,
    ) -> String {
        let mut remaining_budget = MAX_VARIABLE_REPLACEMENTS - 1;
        let mut active_vars = Vec::new();
        match self.resolve_css_variables_inner(
            value,
            ctx,
            theme,
            true,
            &mut remaining_budget,
            0,
            &mut active_vars,
        ) {
            VarResolve::Ok(resolved) => resolved,
            VarResolve::InvalidOrCycle | VarResolve::BudgetExceeded => String::new(),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn resolve_css_variables_inner(
        &self,
        value: &str,
        ctx: &Context,
        theme: CssTheme,
        explicit_theme: bool,
        remaining_budget: &mut usize,
        depth: usize,
        active_vars: &mut Vec<String>,
    ) -> VarResolve {
        if depth > MAX_VARIABLE_DEPTH {
            return VarResolve::BudgetExceeded;
        }
        if !value.contains('(') {
            return VarResolve::Ok(value.to_string());
        }

        let mut value = String::from(value);
        loop {
            let Some((start, prefix_end)) = find_ascii_ci(&value, "var(", 0) else {
                return VarResolve::Ok(value);
            };
            if *remaining_budget == 0 {
                return VarResolve::BudgetExceeded;
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
                return VarResolve::InvalidOrCycle;
            };

            let arguments = self.split_css_arguments(&value[prefix_end..end]);
            if arguments.is_empty() {
                return VarResolve::InvalidOrCycle;
            }

            let raw_name = arguments[0].trim();
            let lower_name = raw_name.to_lowercase();
            let fallback = if arguments.len() > 1 {
                arguments[1..].join(",")
            } else {
                String::new()
            };

            // Try scoped custom properties first (element scope -> dictionary :root scope).
            let resolved_replacement =
                if let Some((matched_key, custom_val)) =
                    lookup_scoped_custom_property(raw_name, &lower_name, ctx, theme)
                {
                    if active_vars.iter().any(|active| active == matched_key) {
                        VarResolve::InvalidOrCycle
                    } else {
                        active_vars.push(matched_key.to_owned());
                        let res = self.resolve_css_variables_inner(
                            custom_val,
                            ctx,
                            theme,
                            explicit_theme,
                            remaining_budget,
                            depth + 1,
                            active_vars,
                        );
                        active_vars.pop();
                        res
                    }
                } else if lower_name == "--text-color" || lower_name == "--fg" {
                    let builtin = resolve_builtin_palette_color(
                        &ctx.glossary_text_color,
                        ctx.theme,
                        theme,
                        explicit_theme,
                        CssTheme::default_text_color,
                    );
                    if let Some(color) = builtin {
                        VarResolve::Ok(color.to_owned())
                    } else {
                        VarResolve::InvalidOrCycle
                    }
                } else if lower_name == "--font-size-no-units" {
                    VarResolve::Ok(format_pixel_size(ctx.root_font_pixel_size))
                } else if lower_name == "--background-color" || lower_name == "--canvas" {
                    let builtin = resolve_builtin_palette_color(
                        &ctx.glossary_background_color,
                        ctx.theme,
                        theme,
                        explicit_theme,
                        CssTheme::default_background_color,
                    );
                    if let Some(color) = builtin {
                        VarResolve::Ok(color.to_owned())
                    } else {
                        VarResolve::InvalidOrCycle
                    }
                } else {
                    VarResolve::InvalidOrCycle
                };

            let replacement = match resolved_replacement {
                VarResolve::Ok(val) if !val.is_empty() => val,
                VarResolve::BudgetExceeded => return VarResolve::BudgetExceeded,
                VarResolve::Ok(_) | VarResolve::InvalidOrCycle => {
                    if fallback.is_empty() {
                        return VarResolve::InvalidOrCycle;
                    }
                    match self.resolve_css_variables_inner(
                        &fallback,
                        ctx,
                        theme,
                        explicit_theme,
                        remaining_budget,
                        depth + 1,
                        active_vars,
                    ) {
                        VarResolve::Ok(val) if !val.is_empty() => val,
                        VarResolve::BudgetExceeded => return VarResolve::BudgetExceeded,
                        VarResolve::Ok(_) | VarResolve::InvalidOrCycle => {
                            return VarResolve::InvalidOrCycle;
                        }
                    }
                }
            };

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
            let Some((start, prefix_end)) = find_ascii_ci(&value, "calc(", 0) else {
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
            let mut color_value = arguments[i].trim().to_string();
            if let Some((match_start, number)) =
                scanners::match_trailing_percentage(&color_value, true)
            {
                percentages[i] = parse_css_f64(number).unwrap_or(0.0);
                color_value = color_value[..match_start].trim().to_string();
            }
            let Some(color) = parse_css_color(&color_value) else {
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
        let a0 = f64::from(c0[3]) / 255.0;
        let a1 = f64::from(c1[3]) / 255.0;
        let alpha = a0 * first_weight + a1 * second_weight;
        if alpha <= 0.0 {
            return format_css_color([0, 0, 0, 0]);
        }
        let mix_ch = |ch0: u8, ch1: u8| -> u8 {
            let v = (f64::from(ch0) / 255.0 * a0 * first_weight
                + f64::from(ch1) / 255.0 * a1 * second_weight)
                / alpha;
            (v.clamp(0.0, 1.0) * 255.0).round() as u8
        };
        format_css_color([
            mix_ch(c0[0], c1[0]),
            mix_ch(c0[1], c1[1]),
            mix_ch(c0[2], c1[2]),
            (alpha.clamp(0.0, 1.0) * 255.0).round() as u8,
        ])
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

        let mut color_value = first.trim().to_string();
        if let Some((match_start, _)) = scanners::match_trailing_percentage(&color_value, false) {
            color_value = color_value[..match_start].trim().to_string();
        }

        parse_css_color(&color_value)
            .map(format_css_color)
            .unwrap_or_default()
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
                result.push(current.trim().to_string());
                current.clear();
            } else {
                current.push(ch);
            }
        }
        result.push(current.trim().to_string());
        result
    }
}

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

//! Native UTF-8 dictionary CSS stylesheet parser producing
//! [`ParsedStylesheet`] directly without UTF-16 round-tripping.
//!
//! Supports CSS comments, top-level and nested at-rule skipping, quote-escape
//! aware scanning, CSS nesting (with explicit nesting-depth and selector-count
//! caps), combinators, attribute selectors (`[attr]`, `[attr=val]`,
//! `[attr^=val]`), pseudo-classes (`:first-child`, `:last-child`,
//! `:nth-child(N)`, `:not(...)`), pseudo-elements (`::before`, `::after`),
//! `!important` stripping with diagnostics, `word-break` mapping, and
//! precomputed tag / child-index candidate rule tables.

use std::borrow::Cow;
use std::collections::HashSet;

use crate::element::{
    CssAttributeOperator, CssAttributeSelector, CssCombinator, CssDeclaration, CssPseudoClass,
    CssPseudoClassSelector, CssPseudoElement, CssRule, CssSelectorPart, CssTheme, ParsedStylesheet,
};
use crate::helpers::unquote_css_string;
use super::text::strip_suffix_ascii_ci;

/// Maximum CSS nesting depth permitted before inner nested blocks are skipped.
const MAX_NESTING_DEPTH: usize = 32;

/// Maximum expanded selector count per nested rule block to prevent exponential
/// selector blowup (`a, b { c, d { ... } }`).
const MAX_EXPANDED_SELECTORS: usize = 256;

#[inline]
fn is_letter(ch: char) -> bool {
    ch.is_alphabetic()
}

#[inline]
fn is_letter_or_number(ch: char) -> bool {
    ch.is_alphanumeric()
}

/// Remove `/* ... */` comments outside quoted strings (preserving backslash-
/// escaped quotes inside strings). Borrows input directly when no `/*` occurs.
fn without_css_comments(css: &str) -> Cow<'_, str> {
    if !css.contains("/*") {
        return Cow::Borrowed(css);
    }
    let bytes = css.as_bytes();
    let len = bytes.len();
    let mut source = String::with_capacity(len);
    let mut copy_start = 0usize;
    let mut i = 0usize;
    let mut in_quote = false;
    let mut escaped = false;
    let mut quote = 0u8;

    while i < len {
        let b = bytes[i];
        if in_quote {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == quote {
                in_quote = false;
            }
            i += 1;
            continue;
        }
        if b == b'"' || b == b'\'' {
            in_quote = true;
            quote = b;
            i += 1;
            continue;
        }
        if b == b'/' && i + 1 < len && bytes[i + 1] == b'*' {
            source.push_str(&css[copy_start..i]);
            i += 2;
            while i + 1 < len && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                i += 1;
            }
            i = if i + 1 < len { i + 2 } else { len };
            copy_start = i;
            continue;
        }
        i += 1;
    }
    source.push_str(&css[copy_start..]);
    Cow::Owned(source)
}

/// Find the byte index of the `}` matching the `{` at `open`, respecting
/// single/double quotes and backslash escapes.
fn matching_css_brace(source: &str, open: usize) -> Option<usize> {
    let bytes = source.as_bytes();
    let mut depth = 1usize;
    let mut in_quote = false;
    let mut escaped = false;
    let mut quote = 0u8;
    let mut i = open + 1;
    while i < bytes.len() {
        let b = bytes[i];
        if in_quote {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == quote {
                in_quote = false;
            }
        } else if b == b'"' || b == b'\'' {
            in_quote = true;
            quote = b;
        } else if b == b'{' {
            depth += 1;
        } else if b == b'}' {
            depth -= 1;
            if depth == 0 {
                return Some(i);
            }
        }
        i += 1;
    }
    None
}

/// Find the next unquoted `{` starting at byte offset `start`.
fn find_unquoted_brace(source: &str, start: usize) -> Option<usize> {
    let bytes = source.as_bytes();
    let mut in_quote = false;
    let mut escaped = false;
    let mut quote = 0u8;
    let mut i = start;
    while i < bytes.len() {
        let b = bytes[i];
        if in_quote {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == quote {
                in_quote = false;
            }
        } else if b == b'"' || b == b'\'' {
            in_quote = true;
            quote = b;
        } else if b == b'{' {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// Skip an at-rule starting at `@` (`start`), returning the byte index of its
/// terminating `;` or matching `}`, or `source.len()` if unterminated.
fn skip_css_at_rule(source: &str, start: usize) -> usize {
    let bytes = source.as_bytes();
    let mut in_quote = false;
    let mut escaped = false;
    let mut quote = 0u8;
    let mut i = start;
    while i < bytes.len() {
        let b = bytes[i];
        if in_quote {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == quote {
                in_quote = false;
            }
        } else if b == b'"' || b == b'\'' {
            in_quote = true;
            quote = b;
        } else if b == b';' {
            return i;
        } else if b == b'{' {
            return matching_css_brace(source, i).unwrap_or(source.len());
        }
        i += 1;
    }
    source.len()
}

fn split_css_selectors(selector: &str) -> Vec<&str> {
    let bytes = selector.as_bytes();
    let mut selectors = Vec::new();
    let mut seg_start = 0usize;
    let mut in_quote = false;
    let mut escaped = false;
    let mut quote = 0u8;
    let mut bracket_depth = 0i32;
    let mut paren_depth = 0i32;

    for (i, &b) in bytes.iter().enumerate() {
        if in_quote {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == quote {
                in_quote = false;
            }
        } else if b == b'"' || b == b'\'' {
            in_quote = true;
            quote = b;
        } else if b == b'[' {
            bracket_depth += 1;
        } else if b == b']' {
            bracket_depth -= 1;
        } else if b == b'(' {
            paren_depth += 1;
        } else if b == b')' {
            paren_depth -= 1;
        } else if b == b',' && bracket_depth == 0 && paren_depth == 0 {
            let trimmed_part = selector[seg_start..i].trim();
            if !trimmed_part.is_empty() {
                selectors.push(trimmed_part);
            }
            seg_start = i + 1;
        }
    }
    let trimmed_part = selector[seg_start..].trim();
    if !trimmed_part.is_empty() {
        selectors.push(trimmed_part);
    }
    selectors
}

fn expand_nested_css_selectors(
    parents: &[&str],
    nested_selector: &str,
    diagnostics: &mut Vec<String>,
) -> Vec<String> {
    let nested_selectors = split_css_selectors(nested_selector);
    let total = parents.len().saturating_mul(nested_selectors.len());
    let mut combined = Vec::with_capacity(total.min(MAX_EXPANDED_SELECTORS));
    'outer: for parent in parents {
        for nested in &nested_selectors {
            if combined.len() >= MAX_EXPANDED_SELECTORS {
                diagnostics.push(
                    "Truncating nested dictionary CSS selectors exceeding expansion limit"
                        .to_owned(),
                );
                break 'outer;
            }
            let trimmed_nested = nested.trim();
            if trimmed_nested.contains('&') {
                let mut expanded = String::with_capacity(trimmed_nested.len() + parent.len());
                for ch in trimmed_nested.chars() {
                    if ch == '&' {
                        expanded.push_str(parent);
                    } else {
                        expanded.push(ch);
                    }
                }
                combined.push(expanded);
            } else {
                let mut expanded = String::with_capacity(parent.len() + 1 + trimmed_nested.len());
                expanded.push_str(parent);
                expanded.push(' ');
                expanded.push_str(trimmed_nested);
                combined.push(expanded);
            }
        }
    }
    combined
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RootVariableScope {
    Unconditional,
    Theme(CssTheme),
}

/// True when `part` targets the virtual document root (`:root`,
/// `:root[data-theme="..."]`, `html[data-theme="..."]`, or bare
/// `[data-theme="..."]`).
pub(crate) fn is_root_selector_part(part: &CssSelectorPart) -> bool {
    if part
        .pseudo_classes
        .iter()
        .any(|p| p.pseudo_type == CssPseudoClass::Root && !p.negated)
    {
        return true;
    }
    part.class_names.is_empty()
        && part.pseudo_classes.is_empty()
        && (part.tag.is_empty() || part.tag == "html")
        && part
            .attributes
            .iter()
            .any(|attr| attr.name.eq_ignore_ascii_case("data-theme"))
}

fn is_root_target_rule(rule: &CssRule) -> bool {
    rule.selector
        .last()
        .is_some_and(is_root_selector_part)
}

fn root_variable_scope_for_rule(rule: &CssRule) -> Option<RootVariableScope> {
    if rule.selector.len() != 1 || rule.pseudo_element != CssPseudoElement::None {
        return None;
    }
    let part = &rule.selector[0];
    if !is_root_selector_part(part) {
        return None;
    }
    if !part.class_names.is_empty() || (!part.tag.is_empty() && part.tag != "html") {
        return None;
    }
    if part
        .pseudo_classes
        .iter()
        .any(|p| p.pseudo_type != CssPseudoClass::Root || p.negated)
    {
        return None;
    }
    if part.attributes.is_empty() {
        return Some(RootVariableScope::Unconditional);
    }
    if part.attributes.len() == 1 {
        let attr = &part.attributes[0];
        if attr.name.eq_ignore_ascii_case("data-theme")
            && attr.op == CssAttributeOperator::Equals
        {
            return CssTheme::from_data_theme(&attr.value).map(RootVariableScope::Theme);
        }
    }
    None
}

fn exact_target_nth_child_index(rule: &CssRule) -> i64 {
    rule.selector
        .last()
        .and_then(|part| {
            part.pseudo_classes.iter().find(|pseudo| {
                pseudo.pseudo_type == CssPseudoClass::NthChild
                    && !pseudo.negated
                    && pseudo.child_index > 0
            })
        })
        .map_or(0, |pseudo| i64::from(pseudo.child_index))
}

fn parse_body(
    selectors: &[&str],
    body: &str,
    rules: &mut Vec<CssRule>,
    diagnostics: &mut Vec<String>,
    order: &mut i32,
    depth: usize,
) {
    let mut declarations_text = String::new();
    let mut last = 0usize;

    if body.contains('{') {
        let bytes = body.as_bytes();
        let mut cursor = 0usize;
        let mut last_top_semicolon: Option<usize> = None;
        let mut in_quote = false;
        let mut escaped = false;
        let mut quote = 0u8;
        let mut paren_depth = 0i32;

        while cursor < bytes.len() {
            let mut found_open: Option<usize> = None;
            let mut idx = cursor;
            while idx < bytes.len() {
                let b = bytes[idx];
                if in_quote {
                    if escaped {
                        escaped = false;
                    } else if b == b'\\' {
                        escaped = true;
                    } else if b == quote {
                        in_quote = false;
                    }
                } else if b == b'"' || b == b'\'' {
                    in_quote = true;
                    quote = b;
                } else if b == b'(' {
                    paren_depth += 1;
                } else if b == b')' {
                    paren_depth -= 1;
                } else if b == b';' && paren_depth == 0 {
                    last_top_semicolon = Some(idx);
                } else if b == b'{' && paren_depth == 0 {
                    found_open = Some(idx);
                    break;
                }
                idx += 1;
            }
            let Some(open) = found_open else {
                break;
            };
            let selector_start = last_top_semicolon
                .filter(|&index| index >= last)
                .map_or(last, |index| index + 1);
            declarations_text.push_str(&body[last..selector_start]);
            let nested_selector = body[selector_start..open].trim();
            let Some(close) = matching_css_brace(body, open) else {
                break;
            };
            if !nested_selector.starts_with('@') {
                if depth >= MAX_NESTING_DEPTH {
                    diagnostics.push(
                        "Ignoring nested dictionary CSS rule exceeding maximum nesting depth"
                            .to_owned(),
                    );
                } else {
                    let combined =
                        expand_nested_css_selectors(selectors, nested_selector, diagnostics);
                    if !combined.is_empty() {
                        let combined_refs: Vec<&str> =
                            combined.iter().map(String::as_str).collect();
                        parse_body(
                            &combined_refs,
                            &body[open + 1..close],
                            rules,
                            diagnostics,
                            order,
                            depth + 1,
                        );
                    }
                }
            }
            cursor = close + 1;
            last = cursor;
            last_top_semicolon = None;
            in_quote = false;
            escaped = false;
            paren_depth = 0;
        }
    }

    let decl_slice: &str = if last == 0 {
        body
    } else {
        declarations_text.push_str(&body[last..]);
        &declarations_text
    };
    let mut declarations = parse_css_declarations(decl_slice, diagnostics);
    if declarations.is_empty() {
        return;
    }
    for (index, selector) in selectors.iter().enumerate() {
        match parse_css_selector(selector) {
            Ok((parts, pseudo_element, specificity)) => {
                rules.push(CssRule {
                    selector: parts,
                    declarations: if index + 1 == selectors.len() {
                        std::mem::take(&mut declarations)
                    } else {
                        declarations.clone()
                    },
                    pseudo_element,
                    specificity,
                    order: *order,
                });
                *order += 1;
            }
            Err(reason) => diagnostics.push(format!(
                "Ignoring unsupported dictionary CSS selector '{selector}': {reason}"
            )),
        }
    }
}

/// Parse a UTF-8 CSS stylesheet into flat rules, compatibility diagnostics,
/// and all candidate rule indexes. Top-level at-rules are skipped cleanly.
pub fn parse_stylesheet(css: &str) -> ParsedStylesheet {
    let source = without_css_comments(css);
    let mut parsed = ParsedStylesheet::default();
    let mut order = 0i32;
    let mut i = 0usize;

    while i < source.len() {
        while let Some(ch) = source[i..].chars().next() {
            if ch.is_whitespace() {
                i += ch.len_utf8();
            } else {
                break;
            }
        }
        if i >= source.len() {
            break;
        }
        if source.as_bytes()[i] == b'@' {
            let end = skip_css_at_rule(&source, i);
            if end >= source.len() {
                break;
            }
            i = end + 1;
            continue;
        }
        let Some(open) = find_unquoted_brace(&source, i) else {
            break;
        };
        let Some(close) = matching_css_brace(&source, open) else {
            break;
        };
        let selectors = split_css_selectors(source[i..open].trim());
        if !selectors.is_empty() {
            parse_body(
                &selectors,
                &source[open + 1..close],
                &mut parsed.rules,
                &mut parsed.diagnostics,
                &mut order,
                0,
            );
        }
        i = close + 1;
    }

    parsed
        .rules
        .sort_by_key(|rule| (rule.specificity, rule.order));

    let mut target_tags = HashSet::new();
    let mut nth_indexes = Vec::with_capacity(parsed.rules.len());
    for (idx, rule) in parsed.rules.iter().enumerate() {
        if let Some(scope) = root_variable_scope_for_rule(rule) {
            let target_map = match scope {
                RootVariableScope::Unconditional => &mut parsed.root_variables,
                RootVariableScope::Theme(CssTheme::Dark) => &mut parsed.dark_root_variables,
                RootVariableScope::Theme(CssTheme::Light) => &mut parsed.light_root_variables,
            };
            for decl in &rule.declarations {
                if decl.property.starts_with("--") {
                    target_map.insert(decl.property.clone(), decl.value.clone());
                }
            }
        }
        if is_root_target_rule(rule) {
            nth_indexes.push(0);
            continue;
        }
        for part in &rule.selector {
            for pseudo in &part.pseudo_classes {
                if pseudo.pseudo_type == CssPseudoClass::LastChild {
                    parsed.uses_element_child_count = true;
                    parsed.element_child_count_tags.insert(part.tag.clone());
                }
            }
        }
        let target_tag = &rule.selector.last().expect("parsed rule has a selector").tag;
        if target_tag.is_empty() {
            parsed.universal_rule_indexes.push(idx);
        } else {
            target_tags.insert(target_tag.clone());
        }
        nth_indexes.push(exact_target_nth_child_index(rule));
    }

    for tag in target_tags {
        let indexes: Vec<usize> = parsed
            .rules
            .iter()
            .enumerate()
            .filter(|(_, rule)| {
                if is_root_target_rule(rule) {
                    return false;
                }
                let target = &rule.selector.last().expect("parsed rule has a selector").tag;
                target.is_empty() || *target == tag
            })
            .map(|(index, _)| index)
            .collect();
        parsed.rule_indexes_by_target_tag.insert(tag, indexes);
    }

    let empty_key = String::new();
    let tag_keys: Vec<String> = std::iter::once(empty_key)
        .chain(parsed.rule_indexes_by_target_tag.keys().cloned())
        .collect();
    for tag in tag_keys {
        let all_indexes: &[usize] = if tag.is_empty() {
            &parsed.universal_rule_indexes
        } else {
            &parsed.rule_indexes_by_target_tag[&tag]
        };
        let mut non_nth = Vec::with_capacity(all_indexes.len());
        let mut child_indexes = HashSet::new();
        for &index in all_indexes {
            let child_index = nth_indexes[index];
            if child_index == 0 {
                non_nth.push(index);
            } else {
                child_indexes.insert(child_index);
            }
        }
        parsed
            .non_nth_rule_indexes_by_target_tag
            .insert(tag.clone(), non_nth);
        if !child_indexes.is_empty() {
            let by_child = parsed
                .rule_indexes_by_target_tag_and_child_index
                .entry(tag)
                .or_default();
            for child_index in child_indexes {
                let indexes = all_indexes
                    .iter()
                    .copied()
                    .filter(|&index| nth_indexes[index] == 0 || nth_indexes[index] == child_index)
                    .collect();
                by_child.insert(child_index, indexes);
            }
        }
    }
    parsed
}

fn flush_selector_part(
    current: &mut String,
    combinator: &mut CssCombinator,
    raw_parts: &mut Vec<String>,
    combinators: &mut Vec<CssCombinator>,
) {
    let part = current.trim();
    if part.is_empty() {
        return;
    }
    raw_parts.push(part.to_owned());
    combinators.push(*combinator);
    current.clear();
    *combinator = CssCombinator::Descendant;
}

fn pseudo_class_token_end(text: &str, start: usize) -> Option<usize> {
    let remaining = text.get(start..)?;
    if let Some(after_prefix) = remaining.strip_prefix(":nth-child(") {
        let rel_close = after_prefix.find(')')?;
        return Some(start + ":nth-child(".len() + rel_close + 1);
    }
    if !remaining.starts_with(':') {
        return None;
    }
    let mut end = start + 1;
    for (offset, ch) in text[start + 1..].char_indices() {
        if is_letter(ch) || ch == '-' {
            end = start + 1 + offset + ch.len_utf8();
        } else {
            break;
        }
    }
    (end != start + 1).then_some(end)
}

fn push_pseudo_class(part: &mut CssSelectorPart, raw: &str, negated: bool) -> bool {
    let mut pseudo = CssPseudoClassSelector {
        negated,
        ..Default::default()
    };
    if raw == ":first-child" {
        pseudo.pseudo_type = CssPseudoClass::FirstChild;
    } else if raw == ":last-child" {
        pseudo.pseudo_type = CssPseudoClass::LastChild;
    } else if raw.eq_ignore_ascii_case(":root") {
        pseudo.pseudo_type = CssPseudoClass::Root;
    } else if let Some(inner) = raw
        .strip_prefix(":nth-child(")
        .and_then(|s| s.strip_suffix(')'))
    {
        let value = inner.trim();
        if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
            return false;
        }
        let Ok(index) = value.parse::<i32>() else {
            return false;
        };
        if index <= 0 {
            return false;
        }
        pseudo.pseudo_type = CssPseudoClass::NthChild;
        pseudo.child_index = index;
    } else {
        return false;
    }
    part.pseudo_classes.push(pseudo);
    true
}

/// Find the byte index of `::` outside quoted strings and `[...]` brackets.
fn find_unquoted_pseudo_element(text: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut in_quote = false;
    let mut escaped = false;
    let mut quote = '\0';
    let mut bracket_depth = 0i32;

    for (idx, ch) in text.char_indices() {
        if in_quote {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == quote {
                in_quote = false;
            }
        } else if ch == '"' || ch == '\'' {
            in_quote = true;
            quote = ch;
        } else if ch == '[' {
            bracket_depth += 1;
        } else if ch == ']' {
            bracket_depth -= 1;
        } else if ch == ':' && bracket_depth == 0 && bytes.get(idx + 1) == Some(&b':') {
            return Some(idx);
        }
    }
    None
}

/// Find the closing `]` matching an attribute selector `[` at `open`,
/// respecting quotes and backslash escapes.
fn find_attribute_close(text: &str, open: usize) -> Option<usize> {
    let mut in_quote = false;
    let mut escaped = false;
    let mut quote = '\0';
    for (offset, ch) in text[open + 1..].char_indices() {
        if in_quote {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == quote {
                in_quote = false;
            }
        } else if ch == '"' || ch == '\'' {
            in_quote = true;
            quote = ch;
        } else if ch == ']' {
            return Some(open + 1 + offset);
        }
    }
    None
}

/// Find the first `=` outside quotes inside an attribute selector body.
fn find_unquoted_equals(attr: &str) -> Option<usize> {
    let mut in_quote = false;
    let mut escaped = false;
    let mut quote = '\0';
    for (idx, ch) in attr.char_indices() {
        if in_quote {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == quote {
                in_quote = false;
            }
        } else if ch == '"' || ch == '\'' {
            in_quote = true;
            quote = ch;
        } else if ch == '=' {
            return Some(idx);
        }
    }
    None
}

/// Parse one UTF-8 CSS selector into `(parts, pseudo_element, specificity)`.
/// On rejection the error is the compatibility diagnostic reason.
pub fn parse_css_selector(
    selector: &str,
) -> Result<(Vec<CssSelectorPart>, CssPseudoElement, i32), String> {
    let mut pseudo_element = CssPseudoElement::None;
    let mut specificity = 0i32;
    let mut normalized = selector.trim().to_owned();

    if let Some(pseudo_start) = find_unquoted_pseudo_element(&normalized) {
        let mut pseudo_end = pseudo_start + 2;
        for (offset, ch) in normalized[pseudo_start + 2..].char_indices() {
            if is_letter(ch) || ch == '-' {
                pseudo_end = pseudo_start + 2 + offset + ch.len_utf8();
            } else {
                break;
            }
        }
        let pseudo = &normalized[pseudo_start..pseudo_end];
        if pseudo == "::before" {
            pseudo_element = CssPseudoElement::Before;
        } else if pseudo == "::after" {
            pseudo_element = CssPseudoElement::After;
        } else {
            return Err(format!("unsupported pseudo-element {pseudo}"));
        }
        normalized.drain(pseudo_start..pseudo_end);
        if find_unquoted_pseudo_element(&normalized).is_some() {
            return Err("multiple pseudo-elements".to_owned());
        }
        specificity += 1;
    }

    let mut raw_parts = Vec::new();
    let mut combinators = Vec::new();
    let mut current = String::new();
    let mut combinator = CssCombinator::Descendant;
    let mut in_quote = false;
    let mut escaped = false;
    let mut quote = '\0';
    let mut bracket_depth = 0i32;
    let mut paren_depth = 0i32;

    for ch in normalized.chars() {
        if in_quote {
            current.push(ch);
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == quote {
                in_quote = false;
            }
        } else if ch == '"' || ch == '\'' {
            in_quote = true;
            quote = ch;
            current.push(ch);
        } else if ch == '[' {
            bracket_depth += 1;
            current.push(ch);
        } else if ch == ']' {
            bracket_depth -= 1;
            current.push(ch);
        } else if ch == '(' {
            paren_depth += 1;
            current.push(ch);
        } else if ch == ')' {
            paren_depth -= 1;
            current.push(ch);
        } else if ch == '>' && bracket_depth == 0 && paren_depth == 0 {
            flush_selector_part(&mut current, &mut combinator, &mut raw_parts, &mut combinators);
            combinator = CssCombinator::Child;
        } else if ch == '+' && bracket_depth == 0 && paren_depth == 0 {
            flush_selector_part(&mut current, &mut combinator, &mut raw_parts, &mut combinators);
            combinator = CssCombinator::AdjacentSibling;
        } else if ch == '~' && bracket_depth == 0 && paren_depth == 0 {
            flush_selector_part(&mut current, &mut combinator, &mut raw_parts, &mut combinators);
            combinator = CssCombinator::GeneralSibling;
        } else if ch.is_whitespace() && bracket_depth == 0 && paren_depth == 0 {
            if !current.trim().is_empty() {
                flush_selector_part(&mut current, &mut combinator, &mut raw_parts, &mut combinators);
            }
        } else {
            current.push(ch);
        }
    }
    flush_selector_part(&mut current, &mut combinator, &mut raw_parts, &mut combinators);
    if raw_parts.is_empty() {
        return Err("empty selector".to_owned());
    }

    let mut parts = Vec::with_capacity(raw_parts.len());
    for (mut raw, combinator) in raw_parts.into_iter().zip(combinators) {
        let mut part = CssSelectorPart {
            combinator,
            ..Default::default()
        };

        while let Some(start) = raw.find('[') {
            let Some(end) = find_attribute_close(&raw, start) else {
                return Err("unterminated attribute selector".to_owned());
            };
            let attr = (&raw[start + 1..end]).trim();
            let mut attribute = CssAttributeSelector::default();
            if let Some(eq_idx) = find_unquoted_equals(attr) {
                let is_starts_with = eq_idx > 0 && attr.as_bytes()[eq_idx - 1] == b'^';
                if is_starts_with {
                    attribute.op = CssAttributeOperator::StartsWith;
                    attribute.name = (&attr[..eq_idx - 1]).trim().to_owned();
                } else {
                    attribute.op = CssAttributeOperator::Equals;
                    attribute.name = attr[..eq_idx].trim().to_owned();
                }
                let raw_val = (&attr[eq_idx + 1..]).trim();
                attribute.value = unquote_css_string(raw_val).unwrap_or_else(|| raw_val.to_owned());
            } else {
                attribute.op = CssAttributeOperator::Exists;
                attribute.name = attr.trim().to_owned();
            }
            if attribute.name.is_empty() {
                return Err("attribute selector without a name".to_owned());
            }
            part.attributes.push(attribute);
            raw.drain(start..=end);
            specificity += 10;
        }

        while let Some(start) = raw.find(":not(") {
            let mut pseudo_start = start + ":not(".len();
            while let Some(ch) = raw[pseudo_start..].chars().next() {
                if ch.is_whitespace() {
                    pseudo_start += ch.len_utf8();
                } else {
                    break;
                }
            }
            let Some(pseudo_end) = pseudo_class_token_end(&raw, pseudo_start) else {
                return Err("unsupported :not() argument".to_owned());
            };
            let mut close_idx = pseudo_end;
            while let Some(ch) = raw[close_idx..].chars().next() {
                if ch.is_whitespace() {
                    close_idx += ch.len_utf8();
                } else {
                    break;
                }
            }
            if raw.as_bytes().get(close_idx) != Some(&b')') {
                return Err("unsupported :not() argument".to_owned());
            }
            let raw_pseudo = &raw[pseudo_start..pseudo_end];
            if !push_pseudo_class(&mut part, raw_pseudo.trim(), true) {
                return Err(format!("unsupported pseudo-class {raw_pseudo}"));
            }
            specificity += 10;
            raw.drain(start..=close_idx);
        }

        while let Some(start) = raw.find(':') {
            let Some(end) = pseudo_class_token_end(&raw, start) else {
                return Err("malformed pseudo-class selector".to_owned());
            };
            let raw_pseudo = &raw[start..end];
            if !push_pseudo_class(&mut part, raw_pseudo.trim(), false) {
                return Err(format!("unsupported pseudo-class {raw_pseudo}"));
            }
            specificity += 10;
            raw.drain(start..end);
        }

        while let Some(start) = raw.find('.') {
            let mut end = start + 1;
            for (offset, ch) in raw[start + 1..].char_indices() {
                if is_letter_or_number(ch) || ch == '-' || ch == '_' {
                    end = start + 1 + offset + ch.len_utf8();
                } else {
                    break;
                }
            }
            if end == start + 1 {
                return Err("class selector without a name".to_owned());
            }
            part.class_names.insert(raw[start + 1..end].to_owned());
            raw.drain(start..end);
            specificity += 10;
        }

        if raw.contains(':') {
            return Err("malformed pseudo-class selector".to_owned());
        }
        if raw.contains('#') {
            return Err("unsupported ID selector".to_owned());
        }
        let remaining = raw.trim();
        if !remaining.is_empty() && remaining != "*" {
            part.tag = remaining.to_lowercase();
            specificity += 1;
        }
        parts.push(part);
    }

    Ok((parts, pseudo_element, specificity))
}

fn flush_declaration(
    property: &mut String,
    value: &mut String,
    in_property: &mut bool,
    declarations: &mut Vec<CssDeclaration>,
    diagnostics: &mut Vec<String>,
) {
    let trimmed_name = property.trim();
    let trimmed_val = value.trim();
    if !trimmed_name.is_empty() && !trimmed_val.is_empty() {
        let is_custom_prop = trimmed_name.starts_with("--");
        let name = if is_custom_prop {
            trimmed_name.to_owned()
        } else {
            trimmed_name.to_lowercase()
        };
        let cleaned_val = if let Some(without_imp) = strip_suffix_ascii_ci(trimmed_val, "!important")
        {
            diagnostics.push(format!(
                "Ignoring !important priority for dictionary CSS property '{name}'"
            ));
            without_imp.trim()
        } else {
            trimmed_val
        };

        if !cleaned_val.is_empty() {
            let css_value = if is_custom_prop {
                cleaned_val.to_owned()
            } else if let Some(unquoted) = unquote_css_string(cleaned_val) {
                if name == "content" || !unquoted.contains('"') {
                    unquoted
                } else {
                    unquoted.replace('"', "'")
                }
            } else if cleaned_val.contains('"') {
                cleaned_val.replace('"', "'")
            } else {
                cleaned_val.to_owned()
            };

            if name == "word-break" {
                if css_value.eq_ignore_ascii_case("keep-all") {
                    declarations.push(CssDeclaration {
                        property: "white-space".to_owned(),
                        value: "nowrap".to_owned(),
                    });
                } else if css_value.eq_ignore_ascii_case("normal") {
                    declarations.push(CssDeclaration {
                        property: "white-space".to_owned(),
                        value: "normal".to_owned(),
                    });
                } else {
                    diagnostics.push(format!(
                        "Ignoring unsupported dictionary CSS word-break value '{css_value}'"
                    ));
                }
            } else {
                declarations.push(CssDeclaration {
                    property: name,
                    value: css_value,
                });
            }
        }
    }
    property.clear();
    value.clear();
    *in_property = true;
}

/// Parse UTF-8 CSS declarations in source order, recording compatibility
/// diagnostics.
pub fn parse_css_declarations(body: &str, diagnostics: &mut Vec<String>) -> Vec<CssDeclaration> {
    let mut declarations = Vec::new();
    let mut property = String::new();
    let mut value = String::new();
    let mut in_property = true;
    let mut in_quote = false;
    let mut escaped = false;
    let mut quote = '\0';
    let mut paren_depth = 0i32;

    for ch in body.chars() {
        if in_quote {
            (if in_property { &mut property } else { &mut value }).push(ch);
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == quote {
                in_quote = false;
            }
        } else if ch == '"' || ch == '\'' {
            in_quote = true;
            quote = ch;
            (if in_property { &mut property } else { &mut value }).push(ch);
        } else if ch == '(' {
            paren_depth += 1;
            (if in_property { &mut property } else { &mut value }).push(ch);
        } else if ch == ')' {
            paren_depth -= 1;
            (if in_property { &mut property } else { &mut value }).push(ch);
        } else if ch == ':' && in_property {
            in_property = false;
        } else if ch == ';' && paren_depth == 0 {
            flush_declaration(
                &mut property,
                &mut value,
                &mut in_property,
                &mut declarations,
                diagnostics,
            );
        } else {
            (if in_property { &mut property } else { &mut value }).push(ch);
        }
    }
    flush_declaration(
        &mut property,
        &mut value,
        &mut in_property,
        &mut declarations,
        diagnostics,
    );
    declarations
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_selectors_precede_parent_rules_then_sort_by_specificity() {
        let parsed = parse_stylesheet(
            "div.x, section { color: red; & > b, em { font-weight: bold } \
             word-break: keep-all!important; }",
        );
        assert_eq!(
            parsed.rules.iter().map(|rule| rule.order).collect::<Vec<_>>(),
            vec![5, 2, 3, 4, 0, 1]
        );
        assert_eq!(parsed.rules[0].specificity, 1);
        assert_eq!(parsed.rules[0].declarations.len(), 2);
        assert_eq!(parsed.rules[0].declarations[1].property, "white-space");
        assert_eq!(parsed.rules[0].declarations[1].value, "nowrap");
        assert_eq!(parsed.rules[1].selector[1].combinator, CssCombinator::Child);
        assert_eq!(parsed.rules[1].selector[1].tag, "b");
        assert_eq!(
            parsed.rules[2].selector[1].combinator,
            CssCombinator::Descendant
        );
        assert_eq!(
            parsed.diagnostics,
            vec!["Ignoring !important priority for dictionary CSS property 'word-break'"]
        );
    }

    #[test]
    fn unsupported_selectors_and_declarations_record_source_order_diagnostics() {
        let parsed = parse_stylesheet(
            "a:hover, a#bad, a::marker, p:not(:nth-child(0)), span \
             { word-break: break-all; color: 'hi' !IMPORTANT; padding: calc(1;2); }",
        );
        assert_eq!(parsed.rules.len(), 1);
        assert_eq!(parsed.rules[0].selector[0].tag, "span");
        assert_eq!(parsed.rules[0].declarations[0].property, "color");
        assert_eq!(parsed.rules[0].declarations[0].value, "hi");
        assert_eq!(parsed.rules[0].declarations[1].value, "calc(1;2)");
        assert_eq!(
            parsed.diagnostics,
            vec![
                "Ignoring unsupported dictionary CSS word-break value 'break-all'",
                "Ignoring !important priority for dictionary CSS property 'color'",
                "Ignoring unsupported dictionary CSS selector 'a:hover': unsupported pseudo-class :hover",
                "Ignoring unsupported dictionary CSS selector 'a#bad': unsupported ID selector",
                "Ignoring unsupported dictionary CSS selector 'a::marker': unsupported pseudo-element ::marker",
                "Ignoring unsupported dictionary CSS selector 'p:not(:nth-child(0))': unsupported pseudo-class :nth-child(0)",
            ]
        );
    }

    #[test]
    fn target_nth_indexes_include_unrestricted_candidates_in_sorted_order() {
        let parsed = parse_stylesheet(
            "*:nth-child(2) {x:a} \
             li:nth-child(1), li:nth-child(3), li:not(:nth-child(2)), ul:last-child li {x:b} \
             li {x:c}",
        );
        assert_eq!(parsed.rules.len(), 6);
        assert_eq!(parsed.universal_rule_indexes, vec![1]);
        assert_eq!(parsed.candidate_rule_indexes("li"), &[0, 1, 2, 3, 4, 5]);
        assert_eq!(
            parsed.candidate_rule_indexes_for_child("li", 1),
            &[0, 2, 4, 5]
        );
        assert_eq!(
            parsed.candidate_rule_indexes_for_child("li", 2),
            &[0, 1, 4, 5]
        );
        assert_eq!(
            parsed.candidate_rule_indexes_for_child("li", 3),
            &[0, 3, 4, 5]
        );
        assert_eq!(parsed.candidate_rule_indexes_for_child("li", 4), &[0, 4, 5]);
        assert_eq!(parsed.candidate_rule_indexes_for_child("p", 2), &[1]);
        assert!(parsed.candidate_rule_indexes_for_child("p", 1).is_empty());
        assert!(parsed.needs_element_child_count_for_tag("ul"));
        assert!(!parsed.needs_element_child_count_for_tag("li"));
        assert_eq!(parsed.element_child_count_tags.len(), 1);
    }

    #[test]
    fn tag_case_is_folded_but_classes_and_attributes_remain_case_sensitive() {
        let parsed = parse_stylesheet(
            "DIV.X[DATA=\"Foo\"]:not(:last-child)::after \
             { WORD-BREAK: NORMAL; content:\"A\" }",
        );
        assert_eq!(parsed.rules.len(), 1);
        let rule = &parsed.rules[0];
        assert_eq!(rule.selector[0].tag, "div");
        assert!(rule.selector[0].class_names.contains("X"));
        assert!(!rule.selector[0].class_names.contains("x"));
        assert_eq!(rule.selector[0].attributes[0].name, "DATA");
        assert_eq!(rule.selector[0].attributes[0].value, "Foo");
        assert_eq!(
            rule.selector[0].pseudo_classes[0].pseudo_type,
            CssPseudoClass::LastChild
        );
        assert!(rule.selector[0].pseudo_classes[0].negated);
        assert_eq!(rule.pseudo_element, CssPseudoElement::After);
        assert_eq!(rule.declarations[0].property, "white-space");
        assert_eq!(rule.declarations[0].value, "normal");
        assert_eq!(rule.declarations[1].value, "A");
        assert!(parsed.needs_element_child_count_for_tag("div"));
        assert!(!parsed.needs_element_child_count_for_tag("DIV"));
    }

    #[test]
    fn comments_at_rules_escaped_quotes_and_braces_in_strings_parse_cleanly() {
        let parsed = parse_stylesheet(
            r#"@import url("a;b.css");
               @media screen { a { content: "{not-a-rule}"; } }
               a[data-x="/*literal*/"]::before { content: "/*literal*/ \"q\" {;}"; }
               a[data-y="a]b^=c"]::after { content: 'don\'t'; }
               a/*removed*/+b { margin: calc(1;2); font-family: "A", "B"; }"#,
        );
        assert_eq!(parsed.rules.len(), 3);
        // Sorted by specificity: a+b (2) before attribute+pseudo-element (12).
        assert_eq!(
            parsed.rules[0].selector[1].combinator,
            CssCombinator::AdjacentSibling
        );
        assert_eq!(parsed.rules[0].declarations[0].value, "calc(1;2)");
        assert_eq!(parsed.rules[0].declarations[1].value, "'A', 'B'");

        assert_eq!(parsed.rules[1].selector[0].attributes[0].value, "/*literal*/");
        assert_eq!(parsed.rules[1].declarations[0].value, "/*literal*/ \"q\" {;}");
        assert_eq!(parsed.rules[1].pseudo_element, CssPseudoElement::Before);

        assert_eq!(parsed.rules[2].selector[0].attributes[0].name, "data-y");
        assert_eq!(
            parsed.rules[2].selector[0].attributes[0].op,
            CssAttributeOperator::Equals
        );
        assert_eq!(parsed.rules[2].selector[0].attributes[0].value, "a]b^=c");
        assert_eq!(parsed.rules[2].declarations[0].value, "don't");
        assert_eq!(parsed.rules[2].pseudo_element, CssPseudoElement::After);
    }

    #[test]
    fn deep_nesting_and_selector_explosion_are_capped() {
        let mut deep = String::new();
        for _ in 0..80 {
            deep.push_str("div { ");
        }
        deep.push_str("color: red;");
        for _ in 0..80 {
            deep.push_str(" }");
        }
        let parsed = parse_stylesheet(&deep);
        assert!(parsed
            .diagnostics
            .iter()
            .any(|d| d.contains("maximum nesting depth")));

        let mut explode = String::new();
        for _ in 0..12 {
            explode.push_str("a, b { ");
        }
        explode.push_str("color: blue;");
        for _ in 0..12 {
            explode.push_str(" }");
        }
        let parsed = parse_stylesheet(&explode);
        assert!(parsed.rules.len() <= MAX_EXPANDED_SELECTORS);
        assert!(parsed
            .diagnostics
            .iter()
            .any(|d| d.contains("expansion limit")));
    }
}

//! Dictionary CSS parsing and selector indexes, corresponding to
//! `dictionarystyles_parser.cpp` and the data model in `dictionarystyles.h`.
//!
//! All CSS text remains UTF-16 code units, including unpaired surrogates: the
//! caller can round-trip these fields through QString without a lossy decode.
//! Rule indexes refer to `rules`, which is sorted by specificity, then source
//! order. The serialized tag-index maps are arrays of `[tag, indexes]` pairs
//! because JSON cannot represent UTF-16 arrays as object keys.

use std::collections::{HashMap, HashSet};

use serde::ser::SerializeSeq;
use serde::{Serialize, Serializer};
use unicode_general_category::{get_general_category, GeneralCategory};

/// A QString-compatible sequence of UTF-16 code units.
pub type CssText = Vec<u16>;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
pub enum CssCombinator {
    #[default]
    Descendant,
    Child,
    AdjacentSibling,
    GeneralSibling,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
pub enum CssAttributeOperator {
    #[default]
    Exists,
    Equals,
    StartsWith,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
pub enum CssPseudoElement {
    #[default]
    None,
    Before,
    After,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
pub enum CssPseudoClass {
    #[default]
    FirstChild,
    LastChild,
    NthChild,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub struct CssAttributeSelector {
    pub name: CssText,
    pub value: CssText,
    pub op: CssAttributeOperator,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub struct CssPseudoClassSelector {
    #[serde(rename = "type")]
    pub r#type: CssPseudoClass,
    /// One-based index for NthChild, zero for the other variants.
    pub child_index: i32,
    pub negated: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub struct CssSelectorPart {
    /// Relationship to the preceding selector part.
    pub combinator: CssCombinator,
    pub tag: CssText,
    pub class_names: HashSet<CssText>,
    pub attributes: Vec<CssAttributeSelector>,
    pub pseudo_classes: Vec<CssPseudoClassSelector>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub struct CssDeclaration {
    pub property: CssText,
    pub value: CssText,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub struct CssRule {
    /// Selector parts in ancestor-to-target order.
    pub selector: Vec<CssSelectorPart>,
    pub declarations: Vec<CssDeclaration>,
    pub pseudo_element: CssPseudoElement,
    pub specificity: i32,
    pub order: i32,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub struct ParsedStylesheet {
    pub rules: Vec<CssRule>,
    pub universal_rule_indexes: Vec<usize>,
    #[serde(serialize_with = "serialize_tag_map")]
    pub rule_indexes_by_target_tag: HashMap<CssText, Vec<usize>>,
    #[serde(serialize_with = "serialize_tag_map")]
    pub non_nth_rule_indexes_by_target_tag: HashMap<CssText, Vec<usize>>,
    #[serde(serialize_with = "serialize_tag_map")]
    pub rule_indexes_by_target_tag_and_child_index: HashMap<CssText, HashMap<usize, Vec<usize>>>,
    pub uses_element_child_count: bool,
    pub element_child_count_tags: HashSet<CssText>,
    pub diagnostics: Vec<CssText>,
}

impl ParsedStylesheet {
    /// CSS candidates in specificity/source order, falling back to universal
    /// rules when no rule has the requested target tag (case-sensitive).
    pub fn candidate_rule_indexes(&self, tag: &[u16]) -> &[usize] {
        self.rule_indexes_by_target_tag
            .get(tag)
            .unwrap_or(&self.universal_rule_indexes)
    }

    /// Candidates for a one-based child index. Exact positive target
    /// `:nth-child()` rules are included only for that index; others remain.
    pub fn candidate_rule_indexes_for_child(&self, tag: &[u16], child_index: usize) -> &[usize] {
        let key = if self.rule_indexes_by_target_tag.contains_key(tag) {
            tag
        } else {
            &[]
        };
        self.rule_indexes_by_target_tag_and_child_index
            .get(key)
            .and_then(|by_child| by_child.get(&child_index))
            .or_else(|| self.non_nth_rule_indexes_by_target_tag.get(key))
            .unwrap_or(&self.universal_rule_indexes)
    }

    /// True if a rule's `:last-child()` may require this tag's sibling count.
    pub fn needs_element_child_count_for_tag(&self, tag: &[u16]) -> bool {
        self.uses_element_child_count
            && (self.element_child_count_tags.contains(&[][..])
                || self.element_child_count_tags.contains(tag))
    }
}

fn serialize_tag_map<S, V>(map: &HashMap<CssText, V>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
    V: Serialize,
{
    let mut seq = serializer.serialize_seq(Some(map.len()))?;
    for entry in map {
        seq.serialize_element(&entry)?;
    }
    seq.end()
}

fn css_text(text: &str) -> CssText {
    text.encode_utf16().collect()
}

fn append_ascii(text: &mut CssText, suffix: &str) {
    text.extend(suffix.encode_utf16());
}

fn diagnostic_with_text(prefix: &str, text: &[u16], suffix: &str) -> CssText {
    let mut result = css_text(prefix);
    result.extend_from_slice(text);
    append_ascii(&mut result, suffix);
    result
}

fn is_space(unit: u16) -> bool {
    // QChar::isSpace / QString::trimmed do not treat NEL (U+0085) as
    // whitespace, unlike Rust's Unicode White_Space predicate.
    unit != 0x0085 && char::from_u32(u32::from(unit)).is_some_and(char::is_whitespace)
}

fn qchar_category(unit: u16) -> Option<GeneralCategory> {
    char::from_u32(u32::from(unit)).map(get_general_category)
}

fn is_letter(unit: u16) -> bool {
    qchar_category(unit).is_some_and(|category| {
        matches!(
            category,
            GeneralCategory::UppercaseLetter
                | GeneralCategory::LowercaseLetter
                | GeneralCategory::TitlecaseLetter
                | GeneralCategory::ModifierLetter
                | GeneralCategory::OtherLetter
        )
    })
}

fn is_letter_or_number(unit: u16) -> bool {
    qchar_category(unit).is_some_and(|category| {
        matches!(
            category,
            GeneralCategory::UppercaseLetter
                | GeneralCategory::LowercaseLetter
                | GeneralCategory::TitlecaseLetter
                | GeneralCategory::ModifierLetter
                | GeneralCategory::OtherLetter
                | GeneralCategory::DecimalNumber
                | GeneralCategory::LetterNumber
                | GeneralCategory::OtherNumber
        )
    })
}

fn trim(text: &[u16]) -> &[u16] {
    let start = text.iter().position(|&ch| !is_space(ch)).unwrap_or(text.len());
    let end = text.iter().rposition(|&ch| !is_space(ch)).map_or(start, |i| i + 1);
    &text[start..end]
}

fn lower(text: &[u16]) -> CssText {
    let mut result = Vec::with_capacity(text.len());
    for unit in std::char::decode_utf16(text.iter().copied()) {
        match unit {
            Ok(ch) => {
                for lowercase in ch.to_lowercase() {
                    let mut buf = [0; 2];
                    result.extend_from_slice(lowercase.encode_utf16(&mut buf));
                }
            }
            Err(surrogate) => result.push(surrogate.unpaired_surrogate()),
        }
    }
    result
}

fn equals_case_insensitive(text: &[u16], ascii: &str) -> bool {
    // The compared CSS keywords are ASCII. Unicode lowercase mappings that
    // expand into multiple code points cannot equal any of these keywords.
    let mut expected = ascii.chars();
    for unit in std::char::decode_utf16(text.iter().copied()) {
        let Ok(ch) = unit else { return false };
        for lower in ch.to_lowercase() {
            if expected.next() != Some(lower) {
                return false;
            }
        }
    }
    expected.next().is_none()
}

fn find_char(text: &[u16], ch: u16, start: usize) -> Option<usize> {
    text.get(start..)?
        .iter()
        .position(|&current| current == ch)
        .map(|offset| start + offset)
}

fn equals_ascii(text: &[u16], ascii: &str) -> bool {
    text.len() == ascii.len()
        && text
            .iter()
            .copied()
            .zip(ascii.bytes())
            .all(|(unit, byte)| unit == u16::from(byte))
}

fn find_ascii(text: &[u16], needle: &str, start: usize) -> Option<usize> {
    text.get(start..)?
        .windows(needle.len())
        .position(|part| equals_ascii(part, needle))
        .map(|offset| start + offset)
}

fn without_css_comments(css: &[u16]) -> CssText {
    let mut source = Vec::with_capacity(css.len());
    let mut in_comment = false;
    let mut in_quote = false;
    let mut escaped = false;
    let mut quote = 0;
    let mut i = 0;
    while i < css.len() {
        let ch = css[i];
        if !in_comment && !in_quote && css.get(i + 1) == Some(&(b'*' as u16)) && ch == b'/' as u16 {
            in_comment = true;
            i += 2;
            continue;
        }
        if in_comment && ch == b'*' as u16 && css.get(i + 1) == Some(&(b'/' as u16)) {
            in_comment = false;
            i += 2;
            continue;
        }
        if in_comment {
            i += 1;
            continue;
        }
        source.push(ch);
        if in_quote {
            if ch == b'\\' as u16 && !escaped {
                escaped = true;
                i += 1;
                continue;
            }
            if ch == quote && !escaped {
                in_quote = false;
            }
            escaped = false;
        } else if ch == b'"' as u16 || ch == b'\'' as u16 {
            in_quote = true;
            quote = ch;
        }
        i += 1;
    }
    source
}

fn matching_css_brace(source: &[u16], open: usize) -> Option<usize> {
    let mut depth = 1;
    let mut in_quote = false;
    let mut escaped = false;
    let mut quote = 0;
    for (i, &ch) in source.iter().enumerate().skip(open + 1) {
        if in_quote {
            if ch == b'\\' as u16 && !escaped {
                escaped = true;
                continue;
            }
            if ch == quote && !escaped {
                in_quote = false;
            }
            escaped = false;
            continue;
        }
        if ch == b'"' as u16 || ch == b'\'' as u16 {
            in_quote = true;
            quote = ch;
        } else if ch == b'{' as u16 {
            depth += 1;
        } else if ch == b'}' as u16 {
            depth -= 1;
            if depth == 0 {
                return Some(i);
            }
        }
    }
    None
}

fn skip_css_at_rule(source: &[u16], start: usize) -> usize {
    let semicolon = find_char(source, b';' as u16, start);
    let brace = find_char(source, b'{' as u16, start);
    if let Some(open) = brace {
        if semicolon.map_or(true, |end| open < end) {
            return matching_css_brace(source, open).unwrap_or(source.len());
        }
    }
    semicolon.unwrap_or(source.len())
}

fn split_css_selectors(selector: &[u16]) -> Vec<CssText> {
    let mut selectors = Vec::new();
    let mut current = CssText::new();
    let mut in_quote = false;
    let mut quote = 0;
    let mut bracket_depth = 0_i32;
    let mut parenthesis_depth = 0_i32;
    for &ch in selector {
        if in_quote {
            current.push(ch);
            if ch == quote {
                in_quote = false;
            }
        } else if ch == b'"' as u16 || ch == b'\'' as u16 {
            in_quote = true;
            quote = ch;
            current.push(ch);
        } else if ch == b'[' as u16 {
            bracket_depth += 1;
            current.push(ch);
        } else if ch == b']' as u16 {
            bracket_depth -= 1;
            current.push(ch);
        } else if ch == b'(' as u16 {
            parenthesis_depth += 1;
            current.push(ch);
        } else if ch == b')' as u16 {
            parenthesis_depth -= 1;
            current.push(ch);
        } else if ch == b',' as u16 && bracket_depth == 0 && parenthesis_depth == 0 {
            if !trim(&current).is_empty() {
                selectors.push(trim(&current).to_vec());
            }
            current.clear();
        } else {
            current.push(ch);
        }
    }
    if !trim(&current).is_empty() {
        selectors.push(trim(&current).to_vec());
    }
    selectors
}

fn expand_nested_css_selectors(parents: &[CssText], nested_selector: &[u16]) -> Vec<CssText> {
    let nested_selectors = split_css_selectors(nested_selector);
    let mut combined = Vec::with_capacity(parents.len() * nested_selectors.len());
    for parent in parents {
        for nested in &nested_selectors {
            if nested.contains(&(b'&' as u16)) {
                let mut expanded = CssText::new();
                for &ch in trim(nested) {
                    if ch == b'&' as u16 {
                        expanded.extend_from_slice(parent);
                    } else {
                        expanded.push(ch);
                    }
                }
                combined.push(expanded);
            } else {
                let mut expanded = Vec::with_capacity(parent.len() + 1 + nested.len());
                expanded.extend_from_slice(parent);
                expanded.push(b' ' as u16);
                expanded.extend_from_slice(nested);
                combined.push(expanded);
            }
        }
    }
    combined
}

fn exact_target_nth_child_index(rule: &CssRule) -> usize {
    rule.selector
        .last()
        .and_then(|part| {
            part.pseudo_classes.iter().find(|pseudo| {
                pseudo.r#type == CssPseudoClass::NthChild
                    && !pseudo.negated
                    && pseudo.child_index > 0
            })
        })
        .map_or(0, |pseudo| pseudo.child_index as usize)
}

fn parse_body(
    selectors: &[CssText],
    body: &[u16],
    rules: &mut Vec<CssRule>,
    diagnostics: &mut Vec<CssText>,
    order: &mut i32,
) {
    let mut declarations_text = CssText::new();
    let mut last = 0;
    let mut i = 0;
    while i < body.len() {
        if body[i] != b'{' as u16 {
            i += 1;
            continue;
        }
        let selector_start = body[..i]
            .iter()
            .rposition(|&ch| ch == b';' as u16)
            .filter(|&index| index >= last)
            .map_or(last, |index| index + 1);
        declarations_text.extend_from_slice(&body[last..selector_start]);
        let nested_selector = trim(&body[selector_start..i]);
        let Some(close) = matching_css_brace(body, i) else {
            break;
        };
        let combined = expand_nested_css_selectors(selectors, nested_selector);
        parse_body(&combined, &body[i + 1..close], rules, diagnostics, order);
        i = close + 1;
        last = i;
    }
    declarations_text.extend_from_slice(&body[last..]);
    let mut declarations = parse_css_declarations(&declarations_text, diagnostics);
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
            Err(reason) => diagnostics.push({
                let mut message = diagnostic_with_text(
                    "Ignoring unsupported dictionary CSS selector '",
                    selector,
                    "': ",
                );
                message.extend_from_slice(&reason);
                message
            }),
        }
    }
}

/// Parse a stylesheet into flat rules, compatibility diagnostics, and all
/// candidate indexes. Top-level at-rules are skipped as in the C++ parser.
pub fn parse_stylesheet(css: &[u16]) -> ParsedStylesheet {
    let source = without_css_comments(css);
    let mut parsed = ParsedStylesheet::default();
    let mut order = 0;
    let mut i = 0;
    while i < source.len() {
        while i < source.len() && is_space(source[i]) {
            i += 1;
        }
        if i >= source.len() {
            break;
        }
        if source[i] == b'@' as u16 {
            let end = skip_css_at_rule(&source, i);
            if end == source.len() {
                break;
            }
            i = end + 1;
            continue;
        }
        let Some(open) = find_char(&source, b'{' as u16, i) else {
            break;
        };
        let Some(close) = matching_css_brace(&source, open) else {
            break;
        };
        let selectors = split_css_selectors(trim(&source[i..open]));
        parse_body(
            &selectors,
            &source[open + 1..close],
            &mut parsed.rules,
            &mut parsed.diagnostics,
            &mut order,
        );
        i = close + 1;
    }

    parsed
        .rules
        .sort_by_key(|rule| (rule.specificity, rule.order));

    let mut target_tags = HashSet::new();
    let mut nth_indexes = Vec::with_capacity(parsed.rules.len());
    for (i, rule) in parsed.rules.iter().enumerate() {
        for part in &rule.selector {
            for pseudo in &part.pseudo_classes {
                if pseudo.r#type == CssPseudoClass::LastChild {
                    parsed.uses_element_child_count = true;
                    parsed.element_child_count_tags.insert(part.tag.clone());
                }
            }
        }
        let target_tag = &rule.selector.last().expect("parsed rule has a selector").tag;
        if target_tag.is_empty() {
            parsed.universal_rule_indexes.push(i);
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
                let target = &rule.selector.last().expect("parsed rule has a selector").tag;
                target.is_empty() || *target == tag
            })
            .map(|(index, _)| index)
            .collect();
        parsed.rule_indexes_by_target_tag.insert(tag, indexes);
    }
    let empty_key = CssText::new();
    for tag in std::iter::once(&empty_key).chain(parsed.rule_indexes_by_target_tag.keys()) {
        let all_indexes = if tag.is_empty() {
            &parsed.universal_rule_indexes
        } else {
            &parsed.rule_indexes_by_target_tag[tag]
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
                .entry(tag.clone())
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
    current: &mut CssText,
    combinator: &mut CssCombinator,
    raw_parts: &mut Vec<CssText>,
    combinators: &mut Vec<CssCombinator>,
) {
    let part = trim(current);
    if part.is_empty() {
        return;
    }
    raw_parts.push(part.to_vec());
    combinators.push(*combinator);
    current.clear();
    *combinator = CssCombinator::Descendant;
}

fn pseudo_class_token_end(text: &[u16], start: usize) -> Option<usize> {
    let remaining = text.get(start..)?;
    if remaining.len() >= ":nth-child(".len()
        && equals_ascii(&remaining[..":nth-child(".len()], ":nth-child(")
    {
        return find_char(text, b')' as u16, start + ":nth-child(".len()).map(|i| i + 1);
    }
    let mut end = start + 1;
    while end < text.len() && (is_letter(text[end]) || text[end] == b'-' as u16) {
        end += 1;
    }
    (end != start + 1).then_some(end)
}

fn push_pseudo_class(part: &mut CssSelectorPart, raw: &[u16], negated: bool) -> bool {
    let mut pseudo = CssPseudoClassSelector {
        negated,
        ..Default::default()
    };
    if equals_ascii(raw, ":first-child") {
        pseudo.r#type = CssPseudoClass::FirstChild;
    } else if equals_ascii(raw, ":last-child") {
        pseudo.r#type = CssPseudoClass::LastChild;
    } else if raw.len() >= ":nth-child(".len()
        && equals_ascii(&raw[..":nth-child(".len()], ":nth-child(")
    {
        let Some(value) = raw[":nth-child(".len()..].strip_suffix(&[b')' as u16]) else {
            return false;
        };
        let value = trim(value);
        if value.is_empty()
            || !value
                .iter()
                .all(|&digit| (b'0' as u16..=b'9' as u16).contains(&digit))
        {
            // QString::toInt() accepts ASCII decimal numerals (not every
            // Unicode character for which QChar::isDigit() is true).
            return false;
        }
        let Some(index) = value.iter().try_fold(0_i32, |acc, &digit| {
            acc.checked_mul(10)?
                .checked_add(i32::from(digit - b'0' as u16))
        }) else {
            return false;
        };
        if index <= 0 {
            return false;
        }
        pseudo.r#type = CssPseudoClass::NthChild;
        pseudo.child_index = index;
    } else {
        return false;
    }
    part.pseudo_classes.push(pseudo);
    true
}

/// Parse one selector. On rejection the error is the exact C++ diagnostic
/// reason (UTF-16), not the surrounding `Ignoring unsupported…` message.
pub fn parse_css_selector(
    selector: &[u16],
) -> Result<(Vec<CssSelectorPart>, CssPseudoElement, i32), CssText> {
    let mut pseudo_element = CssPseudoElement::None;
    let mut specificity = 0;
    let mut normalized = trim(selector).to_vec();
    if let Some(pseudo_start) = find_ascii(&normalized, "::", 0) {
        let mut pseudo_end = pseudo_start + 2;
        while pseudo_end < normalized.len()
            && (is_letter(normalized[pseudo_end]) || normalized[pseudo_end] == b'-' as u16)
        {
            pseudo_end += 1;
        }
        let pseudo = &normalized[pseudo_start..pseudo_end];
        if equals_ascii(pseudo, "::before") {
            pseudo_element = CssPseudoElement::Before;
        } else if equals_ascii(pseudo, "::after") {
            pseudo_element = CssPseudoElement::After;
        } else {
            return Err(diagnostic_with_text("unsupported pseudo-element ", pseudo, ""));
        }
        normalized.drain(pseudo_start..pseudo_end);
        if find_ascii(&normalized, "::", 0).is_some() {
            return Err(css_text("multiple pseudo-elements"));
        }
        specificity += 1;
    }

    let mut raw_parts = Vec::new();
    let mut combinators = Vec::new();
    let mut current = CssText::new();
    let mut combinator = CssCombinator::Descendant;
    let mut in_quote = false;
    let mut quote = 0;
    let mut bracket_depth = 0_i32;
    for &ch in &normalized {
        if in_quote {
            current.push(ch);
            if ch == quote {
                in_quote = false;
            }
        } else if ch == b'"' as u16 || ch == b'\'' as u16 {
            in_quote = true;
            quote = ch;
            current.push(ch);
        } else if ch == b'[' as u16 {
            bracket_depth += 1;
            current.push(ch);
        } else if ch == b']' as u16 {
            bracket_depth -= 1;
            current.push(ch);
        } else if ch == b'>' as u16 && bracket_depth == 0 {
            flush_selector_part(&mut current, &mut combinator, &mut raw_parts, &mut combinators);
            combinator = CssCombinator::Child;
        } else if ch == b'+' as u16 && bracket_depth == 0 {
            flush_selector_part(&mut current, &mut combinator, &mut raw_parts, &mut combinators);
            combinator = CssCombinator::AdjacentSibling;
        } else if ch == b'~' as u16 && bracket_depth == 0 {
            flush_selector_part(&mut current, &mut combinator, &mut raw_parts, &mut combinators);
            combinator = CssCombinator::GeneralSibling;
        } else if is_space(ch) && bracket_depth == 0 {
            if !trim(&current).is_empty() {
                flush_selector_part(&mut current, &mut combinator, &mut raw_parts, &mut combinators);
            }
        } else {
            current.push(ch);
        }
    }
    flush_selector_part(&mut current, &mut combinator, &mut raw_parts, &mut combinators);
    if raw_parts.is_empty() {
        return Err(css_text("empty selector"));
    }

    let mut parts = Vec::with_capacity(raw_parts.len());
    for (mut raw, combinator) in raw_parts.into_iter().zip(combinators) {
        let mut part = CssSelectorPart {
            combinator,
            ..Default::default()
        };
        let mut attr_index = find_char(&raw, b'[' as u16, 0);
        while let Some(start) = attr_index {
            let Some(end) = find_char(&raw, b']' as u16, start) else {
                return Err(css_text("unterminated attribute selector"));
            };
            let attr = trim(&raw[start + 1..end]);
            let mut attribute = CssAttributeSelector::default();
            let operator_index = find_ascii(attr, "^=", 0);
            let operator_index = if let Some(index) = operator_index {
                attribute.op = CssAttributeOperator::StartsWith;
                Some(index)
            } else if let Some(index) = find_char(attr, b'=' as u16, 0) {
                attribute.op = CssAttributeOperator::Equals;
                Some(index)
            } else {
                None
            };
            if let Some(index) = operator_index {
                attribute.name = trim(&attr[..index]).to_vec();
                let operator_length =
                    if attribute.op == CssAttributeOperator::StartsWith { 2 } else { 1 };
                attribute.value = trim(&attr[index + operator_length..]).to_vec();
                if attribute.value.len() >= 2 {
                    let first = attribute.value[0];
                    let last = attribute.value[attribute.value.len() - 1];
                    if (first == b'"' as u16 && last == b'"' as u16)
                        || (first == b'\'' as u16 && last == b'\'' as u16)
                    {
                        attribute.value.remove(0);
                        attribute.value.pop();
                    }
                }
            } else {
                attribute.name = trim(attr).to_vec();
            }
            if attribute.name.is_empty() {
                return Err(css_text("attribute selector without a name"));
            }
            part.attributes.push(attribute);
            raw.drain(start..=end);
            attr_index = find_char(&raw, b'[' as u16, start);
            specificity += 10;
        }

        let mut not_index = find_ascii(&raw, ":not(", 0);
        while let Some(start) = not_index {
            let pseudo_start = start + ":not(".len();
            let Some(pseudo_end) = pseudo_class_token_end(&raw, pseudo_start) else {
                return Err(css_text("unsupported :not() argument"));
            };
            if pseudo_end >= raw.len() || raw[pseudo_end] != b')' as u16 {
                return Err(css_text("unsupported :not() argument"));
            }
            let raw_pseudo = &raw[pseudo_start..pseudo_end];
            if !push_pseudo_class(&mut part, trim(raw_pseudo), true) {
                return Err(diagnostic_with_text("unsupported pseudo-class ", raw_pseudo, ""));
            }
            specificity += 10;
            raw.drain(start..=pseudo_end);
            not_index = find_ascii(&raw, ":not(", 0);
        }

        let mut pseudo_index = find_char(&raw, b':' as u16, 0);
        while let Some(start) = pseudo_index {
            let Some(end) = pseudo_class_token_end(&raw, start) else {
                return Err(css_text("malformed pseudo-class selector"));
            };
            let raw_pseudo = &raw[start..end];
            if !push_pseudo_class(&mut part, trim(raw_pseudo), false) {
                return Err(diagnostic_with_text("unsupported pseudo-class ", raw_pseudo, ""));
            }
            specificity += 10;
            raw.drain(start..end);
            pseudo_index = find_char(&raw, b':' as u16, 0);
        }

        let mut class_index = find_char(&raw, b'.' as u16, 0);
        while let Some(start) = class_index {
            let mut end = start + 1;
            while end < raw.len()
                && (is_letter_or_number(raw[end])
                    || raw[end] == b'-' as u16
                    || raw[end] == b'_' as u16)
            {
                end += 1;
            }
            if end == start + 1 {
                return Err(css_text("class selector without a name"));
            }
            part.class_names.insert(raw[start + 1..end].to_vec());
            raw.drain(start..end);
            specificity += 10;
            class_index = find_char(&raw, b'.' as u16, start);
        }
        if raw.contains(&(b':' as u16)) {
            return Err(css_text("malformed pseudo-class selector"));
        }
        if raw.contains(&(b'#' as u16)) {
            return Err(css_text("unsupported ID selector"));
        }
        let raw = trim(&raw);
        if !raw.is_empty() && raw != &[b'*' as u16] {
            part.tag = lower(raw);
            specificity += 1;
        }
        parts.push(part);
    }
    Ok((parts, pseudo_element, specificity))
}

fn flush_declaration(
    property: &mut CssText,
    value: &mut CssText,
    in_property: &mut bool,
    declarations: &mut Vec<CssDeclaration>,
    diagnostics: &mut Vec<CssText>,
) {
    let trimmed_name = trim(property);
    let mut css_value = trim(value).to_vec();
    if !trimmed_name.is_empty() && !css_value.is_empty() {
        let name = lower(trimmed_name);
        if css_value.len() >= "!important".len()
            && equals_case_insensitive(&css_value[css_value.len() - "!important".len()..], "!important")
        {
            css_value.truncate(css_value.len() - "!important".len());
            let leading = css_value
                .iter()
                .position(|&ch| !is_space(ch))
                .unwrap_or(css_value.len());
            let trimmed_len = trim(&css_value).len();
            css_value.drain(..leading);
            css_value.truncate(trimmed_len);
            diagnostics.push(diagnostic_with_text(
                "Ignoring !important priority for dictionary CSS property '",
                &name,
                "'",
            ));
        }
        if !css_value.is_empty() {
            for ch in &mut css_value {
                if *ch == b'"' as u16 {
                    *ch = b'\'' as u16;
                }
            }
            if css_value.len() >= 2
                && css_value[0] == b'\'' as u16
                && css_value[css_value.len() - 1] == b'\'' as u16
            {
                css_value.remove(0);
                css_value.pop();
            }
            if equals_ascii(&name, "word-break") {
                if equals_case_insensitive(&css_value, "keep-all") {
                    declarations.push(CssDeclaration {
                        property: css_text("white-space"),
                        value: css_text("nowrap"),
                    });
                } else if equals_case_insensitive(&css_value, "normal") {
                    declarations.push(CssDeclaration {
                        property: css_text("white-space"),
                        value: css_text("normal"),
                    });
                } else {
                    diagnostics.push(diagnostic_with_text(
                        "Ignoring unsupported dictionary CSS word-break value '",
                        &css_value,
                        "'",
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

/// Parse declarations in source order, recording compatibility diagnostics.
/// A `word-break` fallback can replace a declaration or omit it entirely.
pub fn parse_css_declarations(body: &[u16], diagnostics: &mut Vec<CssText>) -> Vec<CssDeclaration> {
    let mut declarations = Vec::new();
    let mut property = CssText::new();
    let mut value = CssText::new();
    let mut in_property = true;
    let mut in_quote = false;
    let mut quote = 0;
    let mut paren_depth = 0_i32;
    for &ch in body {
        if in_quote {
            (if in_property { &mut property } else { &mut value }).push(ch);
            if ch == quote {
                in_quote = false;
            }
        } else if ch == b'"' as u16 || ch == b'\'' as u16 {
            in_quote = true;
            quote = ch;
            (if in_property { &mut property } else { &mut value }).push(ch);
        } else if ch == b'(' as u16 {
            paren_depth += 1;
            (if in_property { &mut property } else { &mut value }).push(ch);
        } else if ch == b')' as u16 {
            paren_depth -= 1;
            (if in_property { &mut property } else { &mut value }).push(ch);
        } else if ch == b':' as u16 && in_property {
            in_property = false;
        } else if ch == b';' as u16 && paren_depth == 0 {
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
    fn selector_scanning_treats_combining_marks_as_nonletters() {
        // U+0345 is Other_Alphabetic but a nonspacing mark, not a letter.
        let (parts, _, _) = parse_css_selector(&css_text(".a\u{0345}")).unwrap();
        assert!(parts[0].class_names.contains(&css_text("a")));
        assert_eq!(parts[0].class_names.len(), 1);
        assert_eq!(parts[0].tag, css_text("\u{0345}"));
    }

    #[test]
    fn nested_selectors_precede_parent_rules_then_sort_by_specificity() {
        let parsed = parse_stylesheet(&css_text(
            "div.x, section { color: red; & > b, em { font-weight: bold } \
             word-break: keep-all!important; }",
        ));
        assert_eq!(
            parsed.rules.iter().map(|rule| rule.order).collect::<Vec<_>>(),
            vec![5, 2, 3, 4, 0, 1]
        );
        assert_eq!(parsed.rules[0].specificity, 1);
        assert_eq!(parsed.rules[0].declarations.len(), 2);
        assert_eq!(parsed.rules[0].declarations[1].property, css_text("white-space"));
        assert_eq!(parsed.rules[0].declarations[1].value, css_text("nowrap"));
        assert_eq!(parsed.rules[1].selector[1].combinator, CssCombinator::Child);
        assert_eq!(parsed.rules[1].selector[1].tag, css_text("b"));
        assert_eq!(parsed.rules[2].selector[1].combinator, CssCombinator::Descendant);
        assert_eq!(
            parsed.diagnostics,
            vec![css_text(
                "Ignoring !important priority for dictionary CSS property 'word-break'"
            )]
        );
    }

    #[test]
    fn unsupported_selectors_and_declarations_record_source_order_diagnostics() {
        let parsed = parse_stylesheet(&css_text(
            "a:hover, a#bad, a::marker, p:not(:nth-child(0)), span \
             { word-break: break-all; color: 'hi' !IMPORTANT; padding: calc(1;2); }",
        ));
        assert_eq!(parsed.rules.len(), 1);
        assert_eq!(parsed.rules[0].selector[0].tag, css_text("span"));
        assert_eq!(parsed.rules[0].declarations[0].property, css_text("color"));
        assert_eq!(parsed.rules[0].declarations[0].value, css_text("hi"));
        assert_eq!(parsed.rules[0].declarations[1].value, css_text("calc(1;2)"));
        assert_eq!(
            parsed.diagnostics,
            vec![
                css_text("Ignoring unsupported dictionary CSS word-break value 'break-all'"),
                css_text("Ignoring !important priority for dictionary CSS property 'color'"),
                css_text(
                    "Ignoring unsupported dictionary CSS selector 'a:hover': unsupported pseudo-class :hover"
                ),
                css_text(
                    "Ignoring unsupported dictionary CSS selector 'a#bad': unsupported ID selector"
                ),
                css_text(
                    "Ignoring unsupported dictionary CSS selector 'a::marker': unsupported pseudo-element ::marker"
                ),
                css_text(
                    "Ignoring unsupported dictionary CSS selector 'p:not(:nth-child(0))': unsupported pseudo-class :nth-child(0)"
                ),
            ]
        );
    }

    #[test]
    fn target_nth_indexes_include_unrestricted_candidates_in_sorted_order() {
        let parsed = parse_stylesheet(&css_text(
            "*:nth-child(2) {x:a} \
             li:nth-child(1), li:nth-child(3), li:not(:nth-child(2)), ul:last-child li {x:b} \
             li {x:c}",
        ));
        assert_eq!(parsed.rules.len(), 6);
        assert_eq!(parsed.universal_rule_indexes, vec![1]);
        assert_eq!(parsed.candidate_rule_indexes(&css_text("li")), &[0, 1, 2, 3, 4, 5]);
        assert_eq!(parsed.candidate_rule_indexes_for_child(&css_text("li"), 1), &[0, 2, 4, 5]);
        assert_eq!(parsed.candidate_rule_indexes_for_child(&css_text("li"), 2), &[0, 1, 4, 5]);
        assert_eq!(parsed.candidate_rule_indexes_for_child(&css_text("li"), 3), &[0, 3, 4, 5]);
        assert_eq!(parsed.candidate_rule_indexes_for_child(&css_text("li"), 4), &[0, 4, 5]);
        assert_eq!(parsed.candidate_rule_indexes_for_child(&css_text("p"), 2), &[1]);
        assert!(parsed.candidate_rule_indexes_for_child(&css_text("p"), 1).is_empty());
        assert!(parsed.needs_element_child_count_for_tag(&css_text("ul")));
        assert!(!parsed.needs_element_child_count_for_tag(&css_text("li")));
        assert_eq!(parsed.element_child_count_tags.len(), 1);

        let json = serde_json::to_value(&parsed).expect("UTF-16 tag keys serialize as pairs");
        assert!(json["rule_indexes_by_target_tag"].is_array());
        assert!(json["non_nth_rule_indexes_by_target_tag"].is_array());
        assert!(json["rule_indexes_by_target_tag_and_child_index"].is_array());
    }

    #[test]
    fn tag_case_is_folded_but_classes_and_attributes_remain_case_sensitive() {
        let parsed = parse_stylesheet(&css_text(
            "DIV.X[DATA=\"Foo\"]:not(:last-child)::after \
             { WORD-BREAK: NORMAL; content:\"A\" }",
        ));
        assert_eq!(parsed.rules.len(), 1);
        let rule = &parsed.rules[0];
        assert_eq!(rule.selector[0].tag, css_text("div"));
        assert!(rule.selector[0].class_names.contains(&css_text("X")));
        assert!(!rule.selector[0].class_names.contains(&css_text("x")));
        assert_eq!(rule.selector[0].attributes[0].name, css_text("DATA"));
        assert_eq!(rule.selector[0].attributes[0].value, css_text("Foo"));
        assert_eq!(rule.selector[0].pseudo_classes[0].r#type, CssPseudoClass::LastChild);
        assert!(rule.selector[0].pseudo_classes[0].negated);
        assert_eq!(rule.pseudo_element, CssPseudoElement::After);
        assert_eq!(rule.declarations[0].property, css_text("white-space"));
        assert_eq!(rule.declarations[0].value, css_text("normal"));
        assert_eq!(rule.declarations[1].value, css_text("A"));
        assert!(parsed.needs_element_child_count_for_tag(&css_text("div")));
        assert!(!parsed.needs_element_child_count_for_tag(&css_text("DIV")));
    }

    #[test]
    fn comments_inside_quotes_and_malformed_utf16_survive_parsing() {
        let parsed = parse_stylesheet(&css_text(
            "a[data-x=\"/*literal*/\"]::before {content:\"/*literal*/\"} \
             @media screen {a {content:wrong}} \
             a/*removed*/+b {margin: calc(1;2)}",
        ));
        assert_eq!(parsed.rules.len(), 2);
        assert_eq!(parsed.rules[0].selector[1].combinator, CssCombinator::AdjacentSibling);
        assert_eq!(parsed.rules[0].declarations[0].value, css_text("calc(1;2)"));
        assert_eq!(parsed.rules[1].selector[0].attributes[0].value, css_text("/*literal*/"));
        assert_eq!(parsed.rules[1].declarations[0].value, css_text("/*literal*/"));
        assert_eq!(parsed.rules[1].pseudo_element, CssPseudoElement::Before);

        let mut raw = css_text("a::after {content:'");
        raw.push(0xd800);
        raw.extend(css_text("'}"));
        let parsed = parse_stylesheet(&raw);
        assert_eq!(parsed.rules.len(), 1);
        assert_eq!(parsed.rules[0].declarations[0].value, vec![0xd800]);
    }

    #[test]
    fn nel_is_part_of_a_selector_not_qchar_whitespace() {
        let parsed = parse_stylesheet(&css_text("span\u{0085}{color:red}"));
        assert_eq!(parsed.rules.len(), 1);
        assert_eq!(parsed.rules[0].selector[0].tag, css_text("span\u{0085}"));
        assert!(parsed.candidate_rule_indexes(&css_text("span")).is_empty());
    }
}

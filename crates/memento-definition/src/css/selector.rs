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

//! CSS selector matching and structured-element registration.

use std::sync::Arc;

use serde_json::Value;

use crate::element::{
    Context, CssAttributeOperator, CssCombinator, CssDeclarations, CssPseudoClass,
    CssPseudoElement, CssRule, CssSelectorPart, CssTheme, ParsedStylesheet, StructuredElement,
};
use crate::helpers::{css_font_size_to_pixels, format_pixel_size};
use crate::qstring::{find_ascii_ci_span, is_qchar_upper, to_lower, to_lower_char};

use super::parser::is_root_selector_part;
use super::scanners;
use super::Renderer;

fn root_selector_part_matches(part: &CssSelectorPart, theme: CssTheme) -> bool {
    if !part.tag.is_empty() && part.tag != "html" {
        return false;
    }
    if !part.class_names.is_empty() {
        return false;
    }
    let active_theme = theme.as_data_theme();
    for attribute in &part.attributes {
        if !attribute.name.eq_ignore_ascii_case("data-theme") {
            return false;
        }
        match attribute.op {
            CssAttributeOperator::Exists => {}
            CssAttributeOperator::Equals => {
                if !active_theme.eq_ignore_ascii_case(&attribute.value) {
                    return false;
                }
            }
            CssAttributeOperator::StartsWith => {
                if !active_theme.starts_with(&attribute.value) {
                    return false;
                }
            }
        }
    }
    for pseudo in &part.pseudo_classes {
        let matched = matches!(pseudo.pseudo_type, CssPseudoClass::Root);
        if if pseudo.negated { matched } else { !matched } {
            return false;
        }
    }
    true
}

impl Renderer {
    /// Apply matching stylesheet rules to a declaration map
    /// (`addMatchingCssRules`, L880-950).
    pub fn add_matching_css_rules(
        &self,
        ctx: &mut Context,
        declarations: &mut CssDeclarations,
        before_content: Option<&mut String>,
        after_content: Option<&mut String>,
    ) {
        self.add_matching_css_rules_inner(ctx, declarations, before_content, after_content, None);
    }

    /// Internal rule matcher that borrows `&[usize]` candidate indexes without
    /// cloning and optionally records numeric `font_size_px` directly without
    /// formatting `"<px>px"` and reparsing.
    pub(crate) fn add_matching_css_rules_inner(
        &self,
        ctx: &mut Context,
        declarations: &mut CssDeclarations,
        mut before_content: Option<&mut String>,
        mut after_content: Option<&mut String>,
        mut font_size_px: Option<&mut Option<f64>>,
    ) {
        if ctx.dictionary_styles.is_none() || ctx.elements.is_empty() {
            return;
        }

        /* Cloned from the Arc up front so `ctx` stays freely mutable below. */
        let stylesheet: Arc<ParsedStylesheet> = ctx.dictionary_styles.clone().unwrap();

        let current_elem_idx = (*ctx.elements.last().unwrap()) as usize;
        let candidate_indexes: &[usize] = {
            let element = &ctx.selector_elements[current_elem_idx];
            scanners::candidate_rule_indexes_for(&stylesheet, &element.tag, element.child_index)
        };

        let mut matched_rules: Vec<usize> = Vec::new();
        let mut matched_pseudo_rules: Vec<usize> = Vec::new();

        for &index in candidate_indexes {
            let rule = &stylesheet.rules[index];
            if !self.css_rule_matches(rule, ctx) {
                continue;
            }
            if rule.pseudo_element != CssPseudoElement::None {
                matched_pseudo_rules.push(index);
            } else {
                matched_rules.push(index);
            }
        }

        // First pass: apply scoped custom property (`--*`) declarations from all
        // matching rules in specificity/source order so regular declarations on
        // this element (and its descendants) resolve against them.
        for &index in &matched_rules {
            let rule = &stylesheet.rules[index];
            for declaration in &rule.declarations {
                if declaration.property.starts_with("--") {
                    ctx.selector_elements[current_elem_idx]
                        .custom_properties
                        .insert(declaration.property.clone(), declaration.value.clone());
                }
            }
        }

        for &index in &matched_pseudo_rules {
            let rule = &stylesheet.rules[index];
            let content: Option<&mut String> =
                if rule.pseudo_element == CssPseudoElement::After {
                    after_content.as_mut().map(|s| &mut **s)
                } else {
                    before_content.as_mut().map(|s| &mut **s)
                };
            if let Some(content) = content {
                for declaration in &rule.declarations {
                    if declaration.property == "content" {
                        if find_ascii_ci_span(&declaration.value, "var(", 0).is_some() {
                            let resolved = self.resolve_css_variables(&declaration.value, ctx);
                            if !resolved.is_empty() {
                                *content = crate::helpers::unquote_css_string(&resolved)
                                    .unwrap_or(resolved);
                            }
                        } else {
                            content.clone_from(&declaration.value);
                        }
                    }
                }
            }
        }

        for &index in &matched_rules {
            let rule = &stylesheet.rules[index];
            for declaration in &rule.declarations {
                if declaration.property.starts_with("--") {
                    continue;
                }
                let value = self.resolve_css_value_cow(&declaration.value, ctx);
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
                        if let Some(ref mut slot) = font_size_px {
                            **slot = Some(pixel_size);
                            continue;
                        }
                        let formatted = format!("{}px", format_pixel_size(pixel_size));
                        self.add_css_declaration("font-size", &formatted, declarations);
                        continue;
                    }
                    if let Some(ref mut slot) = font_size_px {
                        **slot = None;
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
            || !self.css_selector_part_matches(&rule.selector[part_index as usize], element)
        {
            return false;
        }
        if part_index == 0 {
            return true;
        }

        let previous_sibling = element.previous_sibling;
        let prev_is_root = part_index == 1 && is_root_selector_part(&rule.selector[0]);
        match rule.selector[part_index as usize].combinator {
            CssCombinator::Child => {
                if prev_is_root {
                    let at_root = stack_index == 0
                        || (stack_index == 1
                            && ctx.selector_elements[ctx.elements[0] as usize].tag
                                == "memento-dict");
                    return at_root && root_selector_part_matches(&rule.selector[0], ctx.theme);
                }
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
                    if self.css_rule_matches_at(rule, ctx, part_index - 1, stack_index, sibling) {
                        return true;
                    }
                    sibling = ctx.selector_elements[sibling as usize].previous_sibling;
                }
                false
            }

            CssCombinator::Descendant => {
                if prev_is_root {
                    return root_selector_part_matches(&rule.selector[0], ctx.theme);
                }
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
        for pseudo in &part.pseudo_classes {
            let matched = match pseudo.pseudo_type {
                CssPseudoClass::FirstChild => {
                    if element.child_index <= 0 {
                        return false;
                    }
                    element.child_index == 1
                }

                CssPseudoClass::LastChild => {
                    if element.child_index <= 0 || element.child_count <= 0 {
                        return false;
                    }
                    element.child_index == element.child_count
                }

                CssPseudoClass::NthChild => {
                    if element.child_index <= 0 {
                        return false;
                    }
                    element.child_index == pseudo.child_index as i64
                }

                CssPseudoClass::Root => false,
            };

            if if pseudo.negated { matched } else { !matched } {
                return false;
            }
        }
        true
    }

    /// Convert structured element JSON into selector-engine attributes and
    /// push it onto `ctx.selector_elements`, returning its index.
    pub fn structured_element_value(
        &self,
        obj: &Value,
        ctx: &mut Context,
        details_open: bool,
    ) -> i64 {
        const KEY_DATA: &str = "data";
        const KEY_HREF: &str = "href";
        const KEY_LANG: &str = "lang";
        const KEY_TAG: &str = "tag";
        const KEY_TITLE: &str = "title";

        let mut element = StructuredElement::new();
        let tag_str = obj.get(KEY_TAG).and_then(Value::as_str).unwrap_or("");
        element.tag = to_lower(tag_str);
        element.classes.insert(format!("gloss-sc-{}", element.tag));
        if element.tag == "details" && details_open {
            element
                .attributes
                .insert(String::from("open"), String::new());
        }
        if element.tag == "img" {
            element.classes.insert(String::from("gloss-image"));
            element
                .classes
                .insert(String::from("gloss-image-container"));
            element.classes.insert(String::from("gloss-image-link"));
        }

        if let Some(data) = obj.get(KEY_DATA).and_then(Value::as_object) {
            for (key, value) in data {
                if let Some(s) = value.as_str() {
                    element
                        .attributes
                        .insert(self.structured_data_attribute_name(key), s.to_owned());
                }
            }
        }

        /* Standard attributes copied into the selector-node attribute map */
        for key in [KEY_HREF, KEY_LANG, KEY_TITLE] {
            if let Some(s) = obj.get(key).and_then(Value::as_str) {
                element
                    .attributes
                    .insert(String::from(key), s.to_owned());
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
        let mut out = String::with_capacity(8 + key.len());
        out.push_str("data-sc-");
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

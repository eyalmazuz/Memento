/* Tests for the selector engine: addMatchingCssRules (L880-950),
 * cssRuleMatches (L952-969), cssRuleMatchesAt (L971-1054),
 * cssSelectorPartMatches (L1056-1132), structuredElement (L1134-1202).
 *
 * Expectations were derived line-by-line from
 * src/definition/structuredrichtext_css.cpp. */

use crate::css::{Context, Renderer};
use crate::element::{
    CssAttributeOperator, CssAttributeSelector, CssCombinator, CssDeclaration, CssPseudoClass,
    CssPseudoClassSelector, CssPseudoElement, CssRule, CssSelectorPart, ParsedStylesheet,
    SiblingState, StructuredElement,
};
use crate::json::Json;
use std::sync::Arc;

fn part(
    combinator: CssCombinator,
    tag: &str,
    attributes: Vec<CssAttributeSelector>,
    pseudo_classes: Vec<CssPseudoClassSelector>,
) -> CssSelectorPart {
    CssSelectorPart {
        combinator,
        tag: tag.to_string(),
        class_names: std::collections::HashSet::<String>::new(),
        attributes,
        pseudo_classes,
    }
}

fn attr(name: &str, value: &str, op: CssAttributeOperator) -> CssAttributeSelector {
    CssAttributeSelector { name: name.to_string(), value: value.to_string(), op }
}

fn sel() -> CssSelectorPart {
    part(CssCombinator::Descendant, "", Vec::new(), Vec::new())
}

fn rule_with_color(selector: Vec<CssSelectorPart>) -> CssRule {
    CssRule {
        selector,
        declarations: vec![CssDeclaration {
            property: "color".to_string(),
            value: "green".to_string(),
        }],
        pseudo_element: CssPseudoElement::None,
        specificity: 0,
        order: 0,
    }
}

fn simple_element() -> StructuredElement {
    StructuredElement {
        tag: "span".to_string(),
        child_index: 1,
        ..StructuredElement::new()
    }
}

fn context_with_stack(elements: &[StructuredElement]) -> Context {
    let mut ctx = Context::default();
    ctx.selector_elements = elements.to_vec();
    ctx.elements = (0..elements.len() as i64).collect();
    ctx
}

/* --- cssSelectorPartMatches (L1056-1132) ------------------------------------ */

#[test]
fn selector_part_tag_and_classes_must_all_match() {
    let renderer = Renderer::new();
    let element = simple_element();

    // Empty tag matches anything (L1061-1063).
    assert!(renderer.css_selector_part_matches(&sel(), &element));
    // Exact tag (L1061-1063).
    assert!(renderer.css_selector_part_matches(
        &part(CssCombinator::Descendant, "span", vec![], vec![]),
        &element
    ));
    // Different tag rejects (L1062-1064).
    assert!(!renderer.css_selector_part_matches(
        &part(CssCombinator::Descendant, "div", vec![], vec![]),
        &element
    ));
}

#[test]
fn selector_part_requires_every_class_and_attribute() {
    let renderer = Renderer::new();
    let mut element = simple_element();
    element.classes.insert("gloss-sc-span".to_string());
    element.classes.insert("gloss-image".to_string());
    element.attributes.insert("lang".to_string(), "ja".to_string());

    // All classNames must be present (L1065-1070).
    let both = CssSelectorPart {
        combinator: CssCombinator::Descendant,
        tag: String::new(),
        class_names: ["gloss-sc-span", "gloss-image", "gloss-text"].iter().map(|c| c.to_string()).collect(),
        attributes: Vec::new(),
        pseudo_classes: Vec::new(),
    };
    assert!(!renderer.css_selector_part_matches(&both, &element));

    let present = CssSelectorPart {
        class_names: ["gloss-sc-span", "gloss-image"].iter().map(|c| c.to_string()).collect(),
        ..CssSelectorPart::default()
    };
    assert!(renderer.css_selector_part_matches(&present, &element));

    // Attribute operators (L1071-1107).
    let found = part(
        CssCombinator::Descendant,
        "",
        vec![attr("lang", "", CssAttributeOperator::Exists)],
        vec![],
    );
    assert!(renderer.css_selector_part_matches(&found, &element));
    let missing = part(
        CssCombinator::Descendant,
        "",
        vec![attr("href", "", CssAttributeOperator::Exists)],
        vec![],
    );
    assert!(!renderer.css_selector_part_matches(&missing, &element));

    let equals = part(
        CssCombinator::Descendant,
        "",
        vec![attr("lang", "ja", CssAttributeOperator::Equals)],
        vec![],
    );
    assert!(renderer.css_selector_part_matches(&equals, &element));
    let not_equals = part(
        CssCombinator::Descendant,
        "",
        vec![attr("lang", "ja-JP", CssAttributeOperator::Equals)],
        vec![],
    );
    assert!(!renderer.css_selector_part_matches(&not_equals, &element));

    // StartsWith mirrors QString::startsWith: case-sensitive (L1092-1097).
    let starts = part(
        CssCombinator::Descendant,
        "",
        vec![attr("lang", "ja", CssAttributeOperator::StartsWith)],
        vec![],
    );
    assert!(renderer.css_selector_part_matches(&starts, &element));
    let case_sensitive = part(
        CssCombinator::Descendant,
        "",
        vec![attr("lang", "JA", CssAttributeOperator::StartsWith)],
        vec![],
    );
    assert!(!renderer.css_selector_part_matches(&case_sensitive, &element));
}

#[test]
fn selector_part_pseudo_classes_use_child_indices() {
    let renderer = Renderer::new();
    let mut element = simple_element();
    element.child_index = 1;
    element.child_count = 3;

    let mut childless = simple_element();
    childless.child_index = 0;

    // Pseudo-classes require child_index > 0 (L1108-1111).
    assert!(!renderer.css_selector_part_matches(
        &part(
            CssCombinator::Descendant,
            "",
            vec![],
            vec![CssPseudoClassSelector::default()],
        ),
        &childless
    ));

    // :first-child at index 1 (L1113-1118).
    assert!(renderer.css_selector_part_matches(
        &part(
            CssCombinator::Descendant,
            "",
            vec![],
            vec![CssPseudoClassSelector {
                pseudo_type: CssPseudoClass::FirstChild,
                ..CssPseudoClassSelector::default()
            }],
        ),
        &element
    ));

    // :last-child (L1120-1127) needs childCount > 0 and the exact index.
    let mut last = simple_element();
    last.child_index = 3;
    last.child_count = 3;
    assert!(renderer.css_selector_part_matches(
        &part(
            CssCombinator::Descendant,
            "",
            vec![],
            vec![CssPseudoClassSelector {
                pseudo_type: CssPseudoClass::LastChild,
                ..CssPseudoClassSelector::default()
            }],
        ),
        &last
    ));
    // Same childCount with an intermediate index does not match.
    assert!(!renderer.css_selector_part_matches(
        &part(
            CssCombinator::Descendant,
            "",
            vec![],
            vec![CssPseudoClassSelector {
                pseudo_type: CssPseudoClass::LastChild,
                ..CssPseudoClassSelector::default()
            }],
        ),
        &element
    ));

    // :nth-child(2) on a second child (L1128-1134).
    let mut second = simple_element();
    second.child_index = 2;
    assert!(renderer.css_selector_part_matches(
        &part(
            CssCombinator::Descendant,
            "",
            vec![],
            vec![CssPseudoClassSelector {
                pseudo_type: CssPseudoClass::NthChild,
                child_index: 2,
                negated: false,
            }],
        ),
        &second
    ));

    // negated ? matched : !matched (L1136-1141): a passing plain test
    // succeeds, so :first-child matches the first child.
    assert!(renderer.css_selector_part_matches(
        &part(
            CssCombinator::Descendant,
            "",
            vec![],
            vec![CssPseudoClassSelector {
                pseudo_type: CssPseudoClass::FirstChild,
                negated: false,
                ..CssPseudoClassSelector::default()
            }],
        ),
        &element
    ));
    // A negated test on a passing condition fails: :not(:first-child)
    // rejects the first child (matched = true)...
    assert!(!renderer.css_selector_part_matches(
        &part(
            CssCombinator::Descendant,
            "",
            vec![],
            vec![CssPseudoClassSelector {
                pseudo_type: CssPseudoClass::FirstChild,
                negated: true,
                ..CssPseudoClassSelector::default()
            }],
        ),
        &element
    ));
    // ...and succeeds on the second child (matched = false), like
    // :not(:nth-child(1)).
    assert!(renderer.css_selector_part_matches(
        &part(
            CssCombinator::Descendant,
            "",
            vec![],
            vec![CssPseudoClassSelector {
                pseudo_type: CssPseudoClass::NthChild,
                child_index: 1,
                negated: true,
            }],
        ),
        &second
    ));
}

/* --- cssRuleMatchesAt (L971-1054) ------------------------------------------- */

#[test]
fn selector_matches_walk_the_combinator_chain() {
    let renderer = Renderer::new();

    // Stack: div > li(child 2) > span(child 1) (Child combinator enters
    // L1012-1027).
    let div = StructuredElement { tag: "div".to_string(), ..StructuredElement::new() };
    let li = StructuredElement {
        tag: "li".to_string(),
        child_index: 2,
        ..StructuredElement::new()
    };
    let stack = vec![div, li, simple_element()];
    let ctx = context_with_stack(&stack);

    let rule_child = rule_with_color(vec![
        part(CssCombinator::Child, "div", vec![], vec![]),
        part(CssCombinator::Child, "li", vec![], vec![]),
        part(CssCombinator::Descendant, "span", vec![], vec![]),
    ]);
    assert!(renderer.css_rule_matches(&rule_child, &ctx));

    // The bare target part alone (parts beyond 0 report the element stack).
    assert!(renderer.css_rule_matches_at(&rule_child, &ctx, 0, 0, 0));

    // "p > span" fails: div/li do not match (L1009-1011 + L1010-1024).
    let rule_parent_p = rule_with_color(vec![
        part(CssCombinator::Child, "p", vec![], vec![]),
        part(CssCombinator::Child, "span", vec![], vec![]),
    ]);
    assert!(!renderer.css_rule_matches(&rule_parent_p, &ctx));

    // An empty selector never matches (L954-956).
    assert!(!renderer.css_rule_matches(&rule_with_color(Vec::new()), &ctx));
}

#[test]
fn sibling_combinators_follow_previous_sibling_links() {
    let renderer = Renderer::new();

    // Children of one container: li[0](prev -1), b[1](prev 0), li[2](prev 1).
    let first = StructuredElement {
        tag: "li".to_string(),
        child_index: 1,
        previous_sibling: -1,
        ..StructuredElement::new()
    };
    let second = StructuredElement {
        tag: "b".to_string(),
        child_index: 2,
        previous_sibling: 0,
        ..StructuredElement::new()
    };
    let third = StructuredElement {
        tag: "li".to_string(),
        child_index: 3,
        previous_sibling: 1,
        ..StructuredElement::new()
    };
    let mut ctx_b = context_with_stack(&[first.clone(), second.clone(), third.clone()]);
    // cssRuleMatches evaluates rule.selector.back() at elements.back();
    // the target here is b (index 1).
    ctx_b.elements = vec![1];

    // PORT-NOTE (model): each selector part carries the combinator that
    // links it to the *previous* part, so the target part holds the
    // adjacent/general relationship.
    // AdjacentSibling "li + b" (L1025-1040): b's previous sibling is li[0],
    // and part.children fail when the link itself does not match.
    let rule_adjacent = rule_with_color(vec![
        part(CssCombinator::Descendant, "li", vec![], vec![]),
        part(CssCombinator::AdjacentSibling, "b", vec![], vec![]),
    ]);
    assert!(renderer.css_rule_matches(&rule_adjacent, &ctx_b));
    // "span + b" fails: the sole previous sibling of b is li.
    let rule_wrong_tag = rule_with_color(vec![
        part(CssCombinator::Descendant, "span", vec![], vec![]),
        part(CssCombinator::AdjacentSibling, "b", vec![], vec![]),
    ]);
    assert!(!renderer.css_rule_matches(&rule_wrong_tag, &ctx_b));

    // GeneralSibling "li ~ li" for the third element walks back
    // (L1041-1056) past b[1] to li[0]. The target here is the third
    // element (index 2).
    let rule_general = rule_with_color(vec![
        part(CssCombinator::Descendant, "li", vec![], vec![]),
        part(CssCombinator::GeneralSibling, "li", vec![], vec![]),
    ]);
    let mut ctx_third = context_with_stack(&[first.clone(), second.clone(), third.clone()]);
    ctx_third.elements = vec![2];
    assert!(renderer.css_rule_matches(&rule_general, &ctx_third));

    // A Child combinator cannot skip the missing parent tags.
    let rule_child = rule_with_color(vec![
        part(CssCombinator::Descendant, "ul", vec![], vec![]),
        part(CssCombinator::Child, "li", vec![], vec![]),
    ]);
    assert!(!renderer.css_rule_matches(&rule_child, &ctx_b));

    // AdjacentSibling without a previous sibling fails (L1031-1033).
    let mut ctx_first = context_with_stack(&[first.clone()]);
    ctx_first.elements = vec![0];
    let rule_orphan = rule_with_color(vec![
        part(CssCombinator::AdjacentSibling, "li", vec![], vec![]),
        part(CssCombinator::Descendant, "li", vec![], vec![]),
    ]);
    assert!(!renderer.css_rule_matches(&rule_orphan, &ctx_first));
}

#[test]
fn descendant_combinator_scans_all_ancestors() {
    let renderer = Renderer::new();

    // Stack: li > b > span; "li span" must match through Descendant
    // scanning (L1041-1056 → i over the stack).
    let li = StructuredElement {
        tag: "li".to_string(),
        child_index: 1,
        child_count: 1,
        ..StructuredElement::new()
    };
    let b = StructuredElement { tag: "b".to_string(), ..StructuredElement::new() };
    let ctx = context_with_stack(&[li, b, simple_element()]);

    let rule_descendant = rule_with_color(vec![
        part(CssCombinator::Descendant, "li", vec![], vec![]),
        part(CssCombinator::Descendant, "span", vec![], vec![]),
    ]);
    assert!(renderer.css_rule_matches(&rule_descendant, &ctx));

    let rule_wrong_tag = rule_with_color(vec![
        part(CssCombinator::Descendant, "ul", vec![], vec![]),
        part(CssCombinator::Descendant, "span", vec![], vec![]),
    ]);
    assert!(!renderer.css_rule_matches(&rule_wrong_tag, &ctx));
}

/* --- structuredElement (L1134-1202) ------------------------------------------ */

#[test]
fn structured_element_records_attributes_and_siblings() {
    let renderer = Renderer::new();
    let mut ctx = Context::default();
    ctx.siblings.push(SiblingState {
        previous_element: 5,
        visited_element_count: 0,
        element_count: 4,
        ..SiblingState::default()
    });

    // L1134-1202: data keys become data-sc- attributes (strings only),
    // href/lang/title copy in, classes gain gloss-sc-tag, details keeps
    // "open" only while detailsOpen.
    let json = Json::object([
        ("tag", Json::string("DETAILS")),
        ("data", Json::object([("myKey", Json::string("value")), ("number", Json::Number(3.0))])),
        ("lang", Json::string("ja")),
        ("href", Json::string("file://a.html")),
    ]);

    let index = renderer.structured_element(&json, &mut ctx, true);
    assert_eq!(index, 0);

    let element = &ctx.selector_elements[0];
    assert_eq!(element.tag, "details");
    assert!(element.attributes.contains_key("open"), "detailsOpen adds open");
    assert!(
        element.attributes.contains_key("data-sc-my-key"),
        "data keys hyphenate; got {:?}",
        element.attributes
    );
    assert!(!element.attributes.contains_key("data-sc-number"), "non-string data skipped");
    assert_eq!(element.attributes.get("href").map(String::as_str), Some("file://a.html"));
    assert_eq!(element.attributes.get("lang").map(String::as_str), Some("ja"));
    assert!(element.classes.contains("gloss-sc-details"));

    // Sibling bookkeeping (L1189-1198): childIndex is pre-incremented
    // ++siblings.visitedElementCount, childCount from the container.
    assert_eq!(element.child_index, 1);
    assert_eq!(element.child_count, 4);
    assert_eq!(element.previous_sibling, 5);
    assert_eq!(ctx.siblings[0].visited_element_count, 1);

    // The next element's previousSibling is this element's index (L1199-1204).
    let empty = Json::object([("tag", Json::string("span"))]);
    let second = renderer.structured_element(&empty, &mut ctx, false);
    assert_eq!(second, 1);
    let second_element = &ctx.selector_elements[1];
    assert_eq!(second_element.previous_sibling, 0);
    assert_eq!(second_element.child_index, 2);
    assert!(second_element.classes.contains("gloss-sc-span"));
}

#[test]
fn img_elements_gain_the_image_classes() {
    let renderer = Renderer::new();
    let mut ctx = Context::default();
    let json = Json::object([("tag", Json::string("img"))]);
    renderer.structured_element(&json, &mut ctx, false);
    let element = &ctx.selector_elements[0];
    assert!(element.classes.contains("gloss-image"));
    assert!(element.classes.contains("gloss-image-container"));
    assert!(element.classes.contains("gloss-image-link"));
}

/* --- addMatchingCssRules (L880-950) ------------------------------------------- */

#[test]
fn matching_rules_resolve_and_route_content_to_pseudo_elements() {
    let renderer = Renderer::new();

    // Single plain rule: span { color: green }.
    let stylesheet = ParsedStylesheet {
        rules: vec![rule_with_color(vec![sel_tag("span")])],
        universal_rule_indexes: vec![0],
        ..ParsedStylesheet::default()
    };

    let mut ctx = Context::default();
    ctx.dictionary_styles = Some(Arc::new(stylesheet));
    ctx.selector_elements = vec![simple_element()];
    ctx.elements = vec![0];

    let mut declarations = crate::element::CssDeclarations::new();
    let mut before = String::new();
    let mut after = String::new();
    renderer.add_matching_css_rules(
        &mut ctx,
        &mut declarations,
        Some(&mut before),
        Some(&mut after),
    );

    // The rule's declaration flows through resolveCssValue + addCssDeclaration.
    assert_eq!(declarations.get("color").map(String::as_str), Some("green"));
    assert!(before.is_empty() && after.is_empty());

    // Pseudo-element rules write the "content" declaration to before/after
    // (L908-923) and never the declaration set.
    let stylesheet = ParsedStylesheet {
        rules: vec![
            CssRule {
                selector: vec![sel_tag("span")],
                declarations: vec![CssDeclaration {
                    property: "content".to_string(),
                    value: "before".to_string(),
                }],
                pseudo_element: CssPseudoElement::Before,
                specificity: 0,
                order: 0,
            },
            CssRule {
                selector: vec![sel_tag("span")],
                declarations: vec![CssDeclaration {
                    property: "content".to_string(),
                    value: "after".to_string(),
                }],
                pseudo_element: CssPseudoElement::After,
                specificity: 0,
                order: 0,
            },
        ],
        universal_rule_indexes: vec![0, 1],
        ..ParsedStylesheet::default()
    };
    let mut ctx = Context::default();
    ctx.dictionary_styles = Some(Arc::new(stylesheet));
    ctx.selector_elements = vec![simple_element()];
    ctx.elements = vec![0];

    let mut declarations = crate::element::CssDeclarations::new();
    let mut before = String::new();
    let mut after = String::new();
    renderer.add_matching_css_rules(
        &mut ctx,
        &mut declarations,
        Some(&mut before),
        Some(&mut after),
    );
    assert_eq!(before, "before");
    assert_eq!(after, "after");
    assert!(declarations.is_empty());

    // font-size declarations are re-anchored to px (L924-932).
    let stylesheet = ParsedStylesheet {
        rules: vec![CssRule {
            selector: vec![sel_tag("span")],
            declarations: vec![CssDeclaration {
                property: "font-size".to_string(),
                value: "1.5rem".to_string(),
            }],
            pseudo_element: CssPseudoElement::None,
            specificity: 0,
            order: 0,
        }],
        universal_rule_indexes: vec![0],
        ..ParsedStylesheet::default()
    };
    let mut ctx = Context::default();
    ctx.dictionary_styles = Some(Arc::new(stylesheet));
    ctx.selector_elements = vec![simple_element()];
    ctx.elements = vec![0];
    let mut declarations = crate::element::CssDeclarations::new();
    renderer.add_matching_css_rules(&mut ctx, &mut declarations, None, None);
    assert_eq!(declarations.get("font-size").map(String::as_str), Some("18px"));

    // An empty element stack bails out before touching anything (L884-886).
    let mut ctx = Context::default();
    ctx.dictionary_styles = Some(Arc::new(ParsedStylesheet::default()));
    ctx.elements.clear();
    let mut declarations = crate::element::CssDeclarations::new();
    renderer.add_matching_css_rules(&mut ctx, &mut declarations, None, None);
    assert!(declarations.is_empty());

    // No dictionary styles bails out too (L884-886).
    let mut ctx = Context::default();
    ctx.dictionary_styles = None;
    let mut declarations = crate::element::CssDeclarations::new();
    renderer.add_matching_css_rules(&mut ctx, &mut declarations, None, None);
    assert!(declarations.is_empty());
}

fn sel_tag(tag: &str) -> CssSelectorPart {
    CssSelectorPart {
        combinator: CssCombinator::Descendant,
        tag: tag.to_string(),
        ..CssSelectorPart::default()
    }
}


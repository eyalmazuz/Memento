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

//! Renderer state shared by the CSS engine, layout compatibility emitter and
//! layout-independent content fragments. Qt-owned inputs enter as values at
//! the integration seam (screen metrics, resolved resource URLs, font sizes).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

/// QMap<QString, QString> standing in for `Renderer::CssDeclarations`.
/// QMap iterates in sorted key order, which `addCssDeclarations`
/// (structuredrichtext_css.cpp L449-466) relies on for its style output.
///
/// PORT-NOTE: QMap orders by QString comparison; for the ASCII property names
/// this engine stores, BTreeMap's UTF-8 byte order matches exactly.
pub type CssDeclarations = BTreeMap<String, String>;

/// Physical metrics of the current screen, standing in for `QScreen` at the
/// integration seam (SEAM: built by the caller from `QScreen::availableGeometry().size()`
/// and `QScreen::logicalDotsPerInch`; `None` mirrors the C++ null-screen path).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ScreenMetrics {
    pub available_width: i32,
    pub available_height: i32,
    pub logical_dpi: f64,
}

/// Active color theme for CSS custom property resolution and
/// `:root[data-theme="..."]` matching. Defaults to [`CssTheme::Dark`] so
/// existing callers preserve dark-theme behavior.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash)]
pub enum CssTheme {
    #[default]
    Dark,
    Light,
}

impl CssTheme {
    /// Returns the CSS `data-theme` attribute value (`"dark"` or `"light"`).
    pub const fn as_data_theme(self) -> &'static str {
        match self {
            Self::Dark => "dark",
            Self::Light => "light",
        }
    }

    /// Parses a `data-theme` attribute value (`"dark"` / `"light"`, ASCII case-insensitive).
    pub fn from_data_theme(value: &str) -> Option<Self> {
        let trimmed = value.trim();
        if trimmed.eq_ignore_ascii_case("dark") {
            Some(Self::Dark)
        } else if trimmed.eq_ignore_ascii_case("light") {
            Some(Self::Light)
        } else {
            None
        }
    }

    /// Default built-in `--text-color` / `--fg` color for this theme.
    pub const fn default_text_color(self) -> &'static str {
        match self {
            Self::Dark => "#f8fafc",
            Self::Light => "#1c2024",
        }
    }

    /// Default built-in `--background-color` / `--canvas` color for this theme.
    pub const fn default_background_color(self) -> &'static str {
        match self {
            Self::Dark => "#1c2024",
            Self::Light => "#ffffff",
        }
    }
}

/// Structured content element used for CSS selector matching
/// (structuredrichtext_p.h L155-174).
///
/// PORT-NOTE (hash order): `attributes` was QHash and `classes` QSet; this
/// engine only performs lookups and inserts, never ordered iteration, so the
/// Rust hash containers preserve behavior.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StructuredElement {
    /// The element tag (lowercase once built by `structuredElement`).
    pub tag: String,

    /// Attributes used by stylesheet selectors.
    pub attributes: HashMap<String, String>,

    /// Generated classes used by stylesheet selectors.
    pub classes: HashSet<String>,

    /// Scoped CSS custom properties (`--*`) declared on this element and
    /// inherited by its descendants.
    pub custom_properties: HashMap<String, String>,

    /// Index of the previous element sibling in selectorElements, or -1
    /// (qsizetype semantics preserved with i64).
    pub previous_sibling: i64,

    /// One-based element-child index among siblings (0 when unset).
    pub child_index: i64,

    /// Total element-child count in this element's parent (0 when unset).
    pub child_count: i64,
}

impl StructuredElement {
    pub fn new() -> StructuredElement {
        StructuredElement {
            previous_sibling: -1,
            ..StructuredElement::default()
        }
    }
}

/// Element sibling state for the current content container
/// (structuredrichtext_p.h L179-196).
/// Pending positive and negative CSS margins are stored separately so sibling
/// margins collapse as max(positive) + min(negative).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SiblingState {
    /// Index of the most recently rendered element child in selectorElements.
    pub previous_element: i64,

    /// Number of element children encountered in this sibling group.
    pub visited_element_count: i64,

    /// Total number of element children in this sibling group.
    pub element_count: i64,
    /// Largest positive vertical margin awaiting emission.
    pub pending_positive_margin_pixels: f64,

    /// Smallest negative vertical margin awaiting emission.
    pub pending_negative_margin_pixels: f64,
}

impl Default for SiblingState {
    fn default() -> Self {
        Self {
            previous_element: -1,
            visited_element_count: 0,
            element_count: 0,
            pending_positive_margin_pixels: 0.0,
            pending_negative_margin_pixels: 0.0,
        }
    }
}

/// Parent list and its current one-based item index.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct StructuredList {
    pub tag: String,
    pub marker: String,
    pub item: i64,
}

/// Direction of inline spacing and list-item marker indentation.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum LayoutDirection {
    #[default]
    LeftToRight,
    RightToLeft,
}

/// Qt-compatible layout selected for an element.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ElementLayout {
    #[default]
    Inline,
    Block,
    List,
    MarkedListItem,
    MarkerlessListItem,
    Box,
}

/// Width behavior used by a compatibility frame.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum WidthPolicy {
    #[default]
    Natural,
    Fill,
    FitContent,
    Explicit,
}

/// Vertical margin behavior of the resolved element.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum MarginFlow {
    #[default]
    Inline,
    Collapsible,
    Contained,
    PropagateLastChild,
}

/// CSS side order: top, right, bottom, left.
pub const TOP: usize = 0;
pub const RIGHT: usize = 1;
pub const BOTTOM: usize = 2;
pub const LEFT: usize = 3;
pub const BOX_SIDE_COUNT: usize = 4;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct BorderSide {
    pub color: String,
    pub style: String,
    pub width_pixels: f64,
    pub painted: bool,
}

/// Normalized CSS box used by the Qt table-backed compatibility renderer.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BoxStyle {
    pub margins: [String; BOX_SIDE_COUNT],
    pub padding: [f64; BOX_SIDE_COUNT],
    pub borders: [BorderSide; BOX_SIDE_COUNT],
    pub background_color: String,
    pub enabled: bool,
    pub compact_border_frame: bool,
}

/// Current context of the renderer (`structuredrichtext_p.h` L326-413).
///
/// The font and dictionary resource path are provided at the application
/// boundary. Layout requires only the already-resolved parent/root pixel sizes
/// and the current screen's metrics. When a Qt screen is absent, integration
/// supplies the primary screen's metrics (or the 96 DPI fallback applies).
///
/// PORT-NOTE (hash order): `resolved_css_values` was QHash; it is a pure
/// key-value cache (lookup by the original value string, stored values never
/// iterated), so a HashMap preserves behavior.
pub struct Context {
    /// The current screen (None mirrors the C++ null screen).
    pub screen: Option<ScreenMetrics>,

    /// Font size in pixels of the root element.
    pub root_font_pixel_size: f64,

    /// Font size in pixels of the parent element.
    pub parent_font_pixel_size: f64,

    /// Text color of the parent element.
    pub text_color: String,

    /// Global glossary text color used by palette CSS variables.
    pub glossary_text_color: String,

    /// Global glossary background color used by palette CSS variables.
    pub glossary_background_color: String,

    /// Active popup color theme (`Dark` by default) used for `:root[data-theme="..."]`
    /// matching and built-in theme palette defaults.
    pub theme: CssTheme,

    /// Background painted by the glossary canvas.
    pub background_color: String,

    /// Nearest painted background around the current content.
    pub painted_background_color: String,

    /// CSS rules parsed from the dictionary stylesheet.
    pub dictionary_styles: Option<Arc<ParsedStylesheet>>,

    /// Storage containing selector elements for the current render.
    pub selector_elements: Vec<StructuredElement>,

    /// Stack of indexes into selectorElements (qsizetype values).
    pub elements: Vec<i64>,

    /// Stack of sibling state for structured content containers.
    pub siblings: Vec<SiblingState>,
    /// Stack of parent lists, including the next item index.
    pub lists: Vec<StructuredList>,

    /// Stack of enclosing details controls.
    pub details: Vec<crate::content::DetailsState>,

    /// True when selector matching requires total element-child counts.
    pub needs_element_child_count: bool,

    /// Inherited layout direction.
    pub text_direction: LayoutDirection,

    /// Force an inline label into a compatibility frame.
    pub force_inline_box: bool,

    /// A wrapper cell owns the forced box's side spacing.
    pub suppress_inline_box_spacing: bool,

    /// CSS values resolved during this render, keyed by source value.
    pub resolved_css_values: HashMap<String, String>,

    /// Theme under which `resolved_css_values` entries were populated.
    pub(crate) cached_css_theme: CssTheme,

    /// Raw pointer identity of the `ParsedStylesheet` under which
    /// `resolved_css_values` entries were populated.
    pub(crate) cached_stylesheet_ptr: usize,

    /// Inherited href of the current structured anchor.
    pub link_href: String,

    /// Inherited title tooltip of the current structured element.
    pub title_tooltip: String,

    /// Raised link fragments must not inherit an underline.
    pub suppress_link_decoration: bool,

    /// QML open-state overrides after QVariant::toBool conversion, keyed by ID.
    pub detail_states: HashMap<String, bool>,

    /// Next stable depth-first details ID.
    pub next_detail_id: usize,
}

impl Default for Context {
    fn default() -> Self {
        Context {
            screen: None,
            // structuredrichtext_p.h L344-346: both default to 12.0 px.
            root_font_pixel_size: 12.0,
            parent_font_pixel_size: 12.0,
            text_color: String::new(),
            glossary_text_color: String::new(),
            background_color: String::new(),
            painted_background_color: String::new(),
            glossary_background_color: String::new(),
            theme: CssTheme::Dark,
            dictionary_styles: None,
            selector_elements: Vec::new(),
            elements: Vec::new(),
            siblings: Vec::new(),
            resolved_css_values: HashMap::new(),
            cached_css_theme: CssTheme::Dark,
            cached_stylesheet_ptr: 0,
            lists: Vec::new(),
            details: Vec::new(),
            needs_element_child_count: false,
            text_direction: LayoutDirection::LeftToRight,
            force_inline_box: false,
            suppress_inline_box_spacing: false,
            link_href: String::new(),
            title_tooltip: String::new(),
            suppress_link_decoration: false,
            detail_states: HashMap::new(),
            next_detail_id: 0,
        }
    }
}

impl Context {
    /// Creates a new rendering context initialized for `theme`, populating
    /// built-in glossary text and background palette colors.
    pub fn with_theme(theme: CssTheme) -> Self {
        let fg = theme.default_text_color().to_owned();
        let bg = theme.default_background_color().to_owned();
        Self {
            text_color: fg.clone(),
            glossary_text_color: fg,
            glossary_background_color: bg.clone(),
            background_color: bg.clone(),
            painted_background_color: bg,
            theme,
            cached_css_theme: theme,
            ..Self::default()
        }
    }

    /// Switches the active theme on this context, clearing any cached resolved
    /// CSS values and updating built-in palette colors unless custom overrides
    /// were explicitly set.
    pub fn set_theme(&mut self, theme: CssTheme) {
        let previous = self.theme;
        self.theme = theme;
        self.resolved_css_values.clear();
        self.cached_css_theme = theme;
        if self.glossary_text_color.is_empty()
            || self
                .glossary_text_color
                .eq_ignore_ascii_case(previous.default_text_color())
        {
            self.glossary_text_color = theme.default_text_color().to_owned();
        }
        if self.glossary_background_color.is_empty()
            || self
                .glossary_background_color
                .eq_ignore_ascii_case(previous.default_background_color())
        {
            self.glossary_background_color = theme.default_background_color().to_owned();
        }
    }
}

/// Resolved state for one structured element (`structuredrichtext_p.h`
/// L418-500). Defaults mirror the C++ member initializers.
pub struct ElementRenderState {
    /// Original structured content tag used for selector matching.
    pub tag: String,
    /// Qt-compatible emitted HTML tag.
    pub output_tag: String,

    /// HTML attributes emitted on the outer element.
    pub attributes: String,

    /// Title tooltip inherited by child content.
    pub title_tooltip: String,

    /// CSS declarations emitted on the outer element.
    pub declarations: CssDeclarations,

    /// Declarations moved to the table-backed content cell.
    pub cell_declarations: CssDeclarations,

    /// Normalized CSS box.
    pub box_style: BoxStyle,

    pub layout: ElementLayout,
    pub width_policy: WidthPolicy,
    pub margin_flow: MarginFlow,
    /// Resolved font size inherited by child content.
    pub font_pixel_size: f64,

    /// Explicit numeric font size resolved from matched CSS rules or inline
    /// `style.fontSize`, avoiding formatting to `"<px>px"` and reparsing.
    pub font_size_px: Option<f64>,

    /// Resolved text color inherited by child content.
    pub text_color: String,
    pub painted_background_color: String,
    pub text_direction: LayoutDirection,

    /// Generated ::before content resolved with the element styles.
    pub before_content: String,

    /// Generated ::after content resolved with the element styles.
    pub after_content: String,
    pub list_marker: String,
    pub list_marker_type: String,
    pub padded_span: bool,
    pub text_only_content: bool,
    pub details: crate::content::DetailsState,
    pub disclosure_summary: bool,
    pub raised_text: bool,
    pub content_inline_spacing_before: String,
    pub content_inline_spacing_after: String,
    pub inline_spacing_before: String,
    pub inline_spacing_after: String,
}

impl Default for ElementRenderState {
    fn default() -> Self {
        ElementRenderState {
            tag: String::new(),
            output_tag: String::new(),
            attributes: String::new(),
            title_tooltip: String::new(),
            declarations: CssDeclarations::new(),
            cell_declarations: CssDeclarations::new(),
            box_style: BoxStyle::default(),
            layout: ElementLayout::Inline,
            width_policy: WidthPolicy::Natural,
            margin_flow: MarginFlow::Inline,
            font_pixel_size: 0.0,
            font_size_px: None,
            text_color: String::new(),
            painted_background_color: String::new(),
            text_direction: LayoutDirection::LeftToRight,
            before_content: String::new(),
            after_content: String::new(),
            list_marker: String::new(),
            list_marker_type: String::new(),
            padded_span: false,
            text_only_content: false,
            details: crate::content::DetailsState {
                id: String::new(),
                open: false,
            },
            disclosure_summary: false,
            raised_text: false,
            content_inline_spacing_before: String::new(),
            content_inline_spacing_after: String::new(),
            inline_spacing_before: String::new(),
            inline_spacing_after: String::new(),
        }
    }
}

/// Relationship between a selector part and the preceding part
/// (dictionarystyles.h L45-51).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum CssCombinator {
    #[default]
    Descendant,
    Child,
    AdjacentSibling,
    GeneralSibling,
}

/// Attribute selector comparison supported by the CSS matcher
/// (dictionarystyles.h L56-61).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum CssAttributeOperator {
    #[default]
    Exists,
    Equals,
    StartsWith,
}

/// Pseudo-element target for generated-content rules
/// (dictionarystyles.h L66-71).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum CssPseudoElement {
    #[default]
    None,
    Before,
    After,
}

/// Supported pseudo-class condition (dictionarystyles.h L76-81).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum CssPseudoClass {
    #[default]
    FirstChild,
    LastChild,
    NthChild,
    Root,
}

/// A single CSS attribute selector (dictionarystyles.h L86-96).
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CssAttributeSelector {
    /// Attribute name to test.
    pub name: String,

    /// Attribute value used by value-based operators.
    pub value: String,

    /// Attribute comparison performed by the selector.
    pub op: CssAttributeOperator,
}

/// A single CSS pseudo-class selector (dictionarystyles.h L101-111).
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CssPseudoClassSelector {
    /// Pseudo-class condition to test.
    pub pseudo_type: CssPseudoClass,

    /// Exact one-based child index for :nth-child(), or 0 otherwise.
    pub child_index: i32,

    /// True when this pseudo-class came from :not(...).
    pub negated: bool,
}

/// A single simple selector in a CSS selector chain
/// (dictionarystyles.h L116-132).
///
/// PORT-NOTE (hash order): `class_names` was QSet<QString>; only membership
/// (`element.classes.contains`) is observed, so a HashSet preserves behavior.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CssSelectorPart {
    /// Relationship to the preceding selector part.
    pub combinator: CssCombinator,

    /// Optional tag name.
    pub tag: String,

    /// Required class names.
    pub class_names: HashSet<String>,

    /// Attribute selector tests required by this selector part.
    pub attributes: Vec<CssAttributeSelector>,

    /// Pseudo-class selector tests required by this selector part.
    pub pseudo_classes: Vec<CssPseudoClassSelector>,
}

/// One CSS declaration in source order (dictionarystyles.h L137-144).
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CssDeclaration {
    /// Lowercase CSS property name.
    pub property: String,

    /// Trimmed CSS property value.
    pub value: String,
}

/// A parsed CSS rule (dictionarystyles.h L149-165).
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CssRule {
    /// The selector parts, ordered from ancestor to target.
    pub selector: Vec<CssSelectorPart>,

    /// The CSS declarations in source order.
    pub declarations: Vec<CssDeclaration>,

    /// Pseudo-element target for generated-content rules.
    pub pseudo_element: CssPseudoElement,

    /// The selector specificity.
    pub specificity: i32,

    /// The rule's source order.
    pub order: i32,
}

/// Parsed dictionary CSS (dictionarystyles.h L170-196).
///
/// PORT-NOTE (hash order): the index maps were QHash keyed by tag. The
/// accessors below return per-tag slice lists whose order the CSS engine
/// relies on (specificity/```source order, built by the parser); the maps'
/// iteration order itself is never consumed, so HashMap preserves behavior.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ParsedStylesheet {
    /// The parsed CSS rules, sorted by specificity then source order.
    pub rules: Vec<CssRule>,

    /// Rule indexes whose target selector has no tag restriction.
    pub universal_rule_indexes: Vec<usize>,

    /// Ordered candidate rule indexes for each target tag.
    pub rule_indexes_by_target_tag: HashMap<String, Vec<usize>>,

    /// Candidates without a target :nth-child() restriction by tag.
    pub non_nth_rule_indexes_by_target_tag: HashMap<String, Vec<usize>>,

    /// Candidates for an exact target :nth-child() restriction by tag.
    pub rule_indexes_by_target_tag_and_child_index: HashMap<String, HashMap<i64, Vec<usize>>>,

    /// True when any selector needs a total element-child count.
    pub uses_element_child_count: bool,

    /// Tags whose selector parts use :last-child().
    pub element_child_count_tags: HashSet<String>,

    /// Unconditional `:root` custom properties (`--*`) scoped to this stylesheet.
    pub root_variables: HashMap<String, String>,

    /// Dark-theme `:root[data-theme="dark"]` custom properties (`--*`) scoped to
    /// this stylesheet.
    pub dark_root_variables: HashMap<String, String>,

    /// Light-theme `:root[data-theme="light"]` custom properties (`--*`) scoped
    /// to this stylesheet.
    pub light_root_variables: HashMap<String, String>,

    /// CSS compatibility limitations recorded while parsing.
    pub diagnostics: Vec<String>,
}

impl ParsedStylesheet {
    /// Looks up a stylesheet-scoped `:root` custom property for `theme`,
    /// preferring `:root[data-theme="<theme>"]` over bare `:root`.
    pub fn root_variable(&self, name: &str, theme: CssTheme) -> Option<&str> {
        let theme_map = match theme {
            CssTheme::Dark => &self.dark_root_variables,
            CssTheme::Light => &self.light_root_variables,
        };
        if let Some(val) = theme_map.get(name) {
            return Some(val.as_str());
        }
        if let Some(val) = self.root_variables.get(name) {
            return Some(val.as_str());
        }
        let lower = crate::qstring::to_lower(name);
        if lower != name {
            if let Some(val) = theme_map.get(&lower) {
                return Some(val.as_str());
            }
            if let Some(val) = self.root_variables.get(&lower) {
                return Some(val.as_str());
            }
        }
        None
    }

    /// Returns the merged `:root` custom property table effective for `theme`.
    pub fn effective_root_variables(&self, theme: CssTheme) -> HashMap<String, String> {
        let mut merged = self.root_variables.clone();
        let theme_map = match theme {
            CssTheme::Dark => &self.dark_root_variables,
            CssTheme::Light => &self.light_root_variables,
        };
        for (key, value) in theme_map {
            merged.insert(key.clone(), value.clone());
        }
        merged
    }
    /// Ordered CSS rule candidates for an element tag
    /// (dictionarystyles.cpp L71-79).
    pub fn candidate_rule_indexes(&self, tag: &str) -> &[usize] {
        self.rule_indexes_by_target_tag
            .get(tag)
            .unwrap_or(&self.universal_rule_indexes)
    }

    /// Candidates for an element tag and one-based child index
    /// (dictionarystyles.cpp L81-104). Rules with an exact positive target
    /// :nth-child() selector are only included for their matching index; the
    /// returned list keeps stylesheet source order.
    pub fn candidate_rule_indexes_for_child(&self, tag: &str, child_index: i64) -> &[usize] {
        // candidateTagKey (dictionarystyles.cpp L37-42): the tag itself when
        // indexed, otherwise the shared universal "" key.
        let key = if self.rule_indexes_by_target_tag.contains_key(tag) {
            tag
        } else {
            ""
        };
        if let Some(by_child) = self.rule_indexes_by_target_tag_and_child_index.get(key) {
            if let Some(indexes) = by_child.get(&child_index) {
                return indexes;
            }
        }
        self.non_nth_rule_indexes_by_target_tag
            .get(key)
            .unwrap_or(&self.universal_rule_indexes)
    }

    /// True when a tag can need a total sibling element count
    /// (dictionarystyles.cpp L106-112).
    pub fn needs_element_child_count_for_tag(&self, tag: &str) -> bool {
        self.uses_element_child_count
            && (self.element_child_count_tags.contains("")
                || self.element_child_count_tags.contains(tag))
    }
}

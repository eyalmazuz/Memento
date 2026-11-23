//! Standalone Rust structured-content CSS & layout engine for Subomi.
//!
//! Provides a native UTF-8 CSS stylesheet parser ([`parse_stylesheet`]), CSS
//! selector matching and value resolution ([`Renderer`]), CSS Color 4 parsing
//! ([`parse_css_color`]), box-model layout calculation, and structured-content
//! helpers operating directly on [`serde_json::Value`].

pub mod content;
pub mod css;
pub mod element;
pub mod helpers;
pub mod layout;

pub use content::{
    anchor_needs_custom_handling_value, append_direct_content_values_value,
    contains_raised_content_value, contains_ruby_value, has_inline_box_style_value,
    is_inline_box_label_value, is_inline_structured_content_value, split_ruby_content_value,
    structured_content_text_value, DetailsState, LegacyGlossaryStyle, RubyValueParts,
};
pub use css::{format_css_color, parse_css_color, parse_stylesheet, split_css_tokens, Renderer};
pub use element::{
    BorderSide, BoxStyle, Context, CssAttributeOperator, CssAttributeSelector, CssCombinator,
    CssDeclaration, CssDeclarations, CssPseudoClass, CssPseudoClassSelector, CssPseudoElement,
    CssRule, CssSelectorPart, CssTheme, ElementLayout, ElementRenderState, LayoutDirection,
    MarginFlow, ParsedStylesheet, ScreenMetrics, SiblingState, StructuredElement, StructuredList,
    WidthPolicy,
};
pub use layout::{
    css_box_side_value, css_font_size_to_pixels_with_font, default_list_marker, font_pixel_size,
    is_zero_spacing, list_marker,
};

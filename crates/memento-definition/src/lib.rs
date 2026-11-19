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

//! Standalone Rust structured-content CSS & layout engine for Memento.
//!
//! Provides a native UTF-8 CSS stylesheet parser ([`parse_stylesheet`]), CSS
//! selector matching and value resolution ([`Renderer`]), box-model layout
//! calculation, and structured-content helpers operating directly on
//! [`serde_json::Value`].

pub mod color;
pub mod content;
pub mod css;
pub mod element;
pub mod helpers;
pub mod layout;
pub mod qstring;

pub use color::Color;
pub use content::{
    anchor_needs_custom_handling_value, append_direct_content_values_value,
    contains_raised_content_value, contains_ruby_value, has_inline_box_style_value,
    is_inline_box_label_value, is_inline_structured_content_value, split_ruby_content_value,
    structured_content_text_value, DetailsState, LegacyGlossaryStyle, RubyValueParts,
};
pub use css::{parse_stylesheet, split_css_tokens, Renderer};
pub use element::{
    BorderSide, BoxStyle, Context, CssAttributeOperator, CssAttributeSelector, CssCombinator,
    CssDeclaration, CssDeclarations, CssPseudoClass, CssPseudoClassSelector, CssPseudoElement,
    CssRule, CssSelectorPart, ElementLayout, ElementRenderState, LayoutDirection, MarginFlow,
    ParsedStylesheet, ScreenMetrics, SiblingState, StructuredElement, StructuredList, WidthPolicy,
};
pub use layout::{
    css_box_side_value, css_font_size_to_pixels_with_font, default_list_marker, font_pixel_size,
    is_zero_spacing, list_marker,
};

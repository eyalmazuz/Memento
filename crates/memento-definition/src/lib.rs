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

//! Standalone Rust port of `structuredrichtext_css.cpp`, the layout routines
//! in `structuredrichtext_layout.cpp`, and the structured-content renderer in
//! `structuredrichtext_content.cpp`. Qt-owned screen, style, color, detail,
//! image-resource and locale inputs enter through the C ABI.
//!
//! Behavior mirror notes:
//! - All text is modelled as [`String`]. QString is UTF-16 internally; when a
//!   C++ algorithm observes UTF-16 code-unit counts or code-unit slicing, the
//!   port tracks UTF-16 units explicitly and says so in a `PORT-NOTE`.
//!   Integration must convert `QString` values (unpaired surrogates excepted,
//!   which never occur in structured content CSS) through UTF-8.
//! - `QMap` output order (sorted keys) is modelled with [`BTreeMap`];
//!   `QHash`/`QSet` order is never depended on by this engine, so
//!   [`std::collections::HashMap`]/[`std::collections::HashSet`] are used and
//!   any resulting output is sorted explicitly.

pub mod capi;
pub mod color;
pub mod content;
pub mod css;
pub mod element;
pub mod helpers;
pub mod json;
pub mod layout;
pub mod qstring;

pub use color::Color;
pub use css::{split_css_tokens, Renderer};
pub use content::{
    anchor_needs_custom_handling, append_direct_content_values, contains_raised_content,
    contains_ruby, has_inline_box_style, is_inline_box_label, is_inline_structured_content,
    split_ruby_content, structured_content_text, DetailsState, LegacyGlossaryStyle, RubyParts,
};
pub use element::{
    BorderSide, BoxStyle, Context, CssAttributeOperator, CssAttributeSelector,
    CssCombinator, CssDeclaration, CssDeclarations, CssPseudoClass,
    CssPseudoClassSelector, CssPseudoElement, CssRule, CssSelectorPart,
    ElementLayout, ElementRenderState, LayoutDirection, MarginFlow, ParsedStylesheet,
    ScreenMetrics, SiblingState, StructuredElement, StructuredList, WidthPolicy,
};
pub use layout::{
    css_box_side_value, css_font_size_to_pixels_with_font, default_list_marker,
    font_pixel_size, is_zero_spacing, list_marker,
};
pub use json::Json;

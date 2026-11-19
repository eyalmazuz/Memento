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

//! Structured-content CSS engine split into focused submodules:
//! - [`parser`]: native UTF-8 stylesheet/selector/declaration parser
//! - [`selector`]: CSS selector matching and element registration
//! - [`style`]: structured style application and declaration normalization
//! - [`values`]: CSS value resolution (`var()`, `calc()`, `color-mix()`, gradients)
//! - [`scanners`]: PCRE2-equivalent value scanners and tag/property allowlists

pub mod parser;
pub mod scanners;
pub mod selector;
pub mod style;
pub mod values;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod tests_engine;

#[cfg(test)]
use crate::element::{Context, CssDeclarations};

pub use parser::{parse_css_declarations, parse_css_selector, parse_stylesheet};
pub use scanners::candidate_rule_indexes_for;
pub use values::split_css_tokens;

/// Stateless CSS/layout resolution engine for structured dictionary content.
#[derive(Clone, Copy, Debug, Default)]
pub struct Renderer;

impl Renderer {
    pub const fn new() -> Renderer {
        Renderer
    }
}

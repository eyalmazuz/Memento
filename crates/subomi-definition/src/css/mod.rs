//! Structured-content CSS engine split into focused submodules:
//! - [`parser`]: native UTF-8 stylesheet/selector/declaration parser
//! - [`selector`]: CSS selector matching and element registration
//! - [`style`]: structured style application and declaration normalization
//! - [`values`]: CSS value resolution (`var()`, `calc()`, `color-mix()`, gradients)
//! - [`scanners`]: CSS function/value scanners and tag/property allowlists
//! - [`text`]: CSS string, numeric formatting, and CSS Color 4 helpers

pub mod parser;
pub mod scanners;
pub mod selector;
pub mod style;
pub mod text;
pub mod values;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod tests_engine;

#[cfg(test)]
use crate::element::{Context, CssDeclarations};

pub use parser::{parse_css_declarations, parse_css_selector, parse_stylesheet};
pub use scanners::candidate_rule_indexes_for;
pub use text::{format_css_color, parse_css_color};
pub use values::split_css_tokens;

/// Stateless CSS/layout resolution engine for structured dictionary content.
#[derive(Clone, Copy, Debug, Default)]
pub struct Renderer;

impl Renderer {
    pub const fn new() -> Renderer {
        Renderer
    }
}

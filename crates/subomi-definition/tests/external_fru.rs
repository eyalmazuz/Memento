use subomi_definition::{
    parse_stylesheet, Context, CssTheme, ParsedStylesheet, Renderer, StructuredElement,
};
use std::sync::Arc;

/// Regression guard: external crates (such as the root `subomi` crate's
/// `src/ui/glossary_render.rs`) construct `Context`, `ParsedStylesheet`, and
/// `StructuredElement` via struct update syntax (`..Default::default()`), which
/// requires all fields on those structs to remain `pub`.
#[test]
fn external_crate_struct_update_syntax_compiles_for_public_structs() {
    let sheet = Arc::new(parse_stylesheet(
        ":root { --accent: #38bdf8; } :root[data-theme=\"light\"] { --accent: #0284c7; }",
    ));
    let mut ctx = Context {
        glossary_text_color: "#f8fafc".to_owned(),
        dictionary_styles: Some(sheet),
        ..Context::default()
    };
    let elem = StructuredElement {
        tag: "span".to_owned(),
        ..StructuredElement::new()
    };
    let empty_sheet = ParsedStylesheet {
        ..ParsedStylesheet::default()
    };
    assert!(empty_sheet.rules.is_empty());
    assert_eq!(elem.tag, "span");

    let renderer = Renderer::new();
    assert_eq!(renderer.resolve_css_value("var(--accent)", &mut ctx), "#38bdf8");
    assert_eq!(
        renderer.resolve_css_value_with_theme("var(--accent)", &mut ctx, CssTheme::Light),
        "#0284c7"
    );
}

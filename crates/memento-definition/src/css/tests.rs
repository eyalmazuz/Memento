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

//! Declaration-level behavior tests derived from
//! `structuredrichtext_css.cpp` by simulating the C++ line by line; C++ line
//! numbers are cited in each test.

use std::collections::BTreeMap;

use super::*;
use crate::element::Context;

fn decls() -> CssDeclarations {
    BTreeMap::new()
}

fn map(entries: &[(&str, &str)]) -> CssDeclarations {
    entries
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

/* --- splitCssTokens (L38-90) --------------------------------------------- */

#[test]
fn tokens_split_on_toplevel_whitespace_and_trim_ends() {
    // L46 iterates the trimmed value; L72-79 flushes on outer whitespace.
    assert_eq!(split_css_tokens("  red   12px  "), vec!["red", "12px"]);
    assert_eq!(split_css_tokens(""), Vec::<String>::new());
    assert_eq!(split_css_tokens("   "), Vec::<String>::new());
}

#[test]
fn tokens_preserve_quotes_and_parens() {
    // L48-71: whitespace inside quotes or parentheses is not a delimiter.
    assert_eq!(
        split_css_tokens("\"hello world\" 2"),
        vec!["\"hello world\"", "2"]
    );
    assert_eq!(
        split_css_tokens("rgb(1, 2, 3) red"),
        vec!["rgb(1, 2, 3)", "red"]
    );
    assert_eq!(
        split_css_tokens("'rgb(1, 2)' x"),
        vec!["'rgb(1, 2)'", "x"]
    );
}

#[test]
fn tokens_unbalanced_quote_runs_to_end() {
    // L48-55: the quote never closes, so the rest is one token (L85-88).
    assert_eq!(split_css_tokens("a \"b c"), vec!["a", "\"b c"]);
}

#[test]
fn tokens_empty_adjacent_groups_drop_empty_current() {
    // L74-78 only flushes a non-empty current.
    assert_eq!(split_css_tokens("a  \t  b"), vec!["a", "b"]);
}

/* --- addCssDeclaration (L278-433) ----------------------------------------- */

#[test]
fn declaration_trims_name_and_value() {
    // L283-289: name/value trimming; "font-size" is a supported property
    // (structuredrichtext_p.h L1370) so the else-if branch (L429-432) applies.
    let mut out = decls();
    Renderer::new().add_css_declaration("  FONT-SIZE ", "  14px ", &mut out);
    assert_eq!(out, map(&[("font-size", "14px")]));
}

#[test]
fn declaration_rejects_empty_name_or_value() {
    // L286-289.
    let mut out = decls();
    Renderer::new().add_css_declaration("   ", "14px", &mut out);
    assert_eq!(out, BTreeMap::new());
    Renderer::new().add_css_declaration("color", "   ", &mut out);
    assert_eq!(out, BTreeMap::new());
}

#[test]
fn declaration_replaces_double_quotes_with_single() {
    // L291 runs before all property branches.
    let mut out = decls();
    Renderer::new().add_css_declaration("font-family", "\"Extra Bold\"", &mut out);
    assert_eq!(out, map(&[("font-family", "'Extra Bold'")]));
}

#[test]
fn unsupported_properties_and_unknown_names_are_dropped() {
    // L429-432: only m_supportedCssProperties pass.
    let mut out = decls();
    Renderer::new().add_css_declaration("tab-size", "4", &mut out);
    assert_eq!(out, BTreeMap::new());
    // Supported by Qt rich text:
    let mut out = decls();
    Renderer::new().add_css_declaration("text-transform", "uppercase", &mut out);
    assert_eq!(out, map(&[("text-transform", "uppercase")]));
    // Last write wins, QMap overwrite semantics:
    Renderer::new().add_css_declaration("text-transform", "lowercase", &mut out);
    assert_eq!(out, map(&[("text-transform", "lowercase")]));
}

#[test]
fn margin_shorthand_expands_to_four_sides() {
    // L293-362. 1 value: top/right/bottom/left all from values[0].
    let mut out = decls();
    Renderer::new().add_css_declaration("margin", "1px", &mut out);
    assert_eq!(
        out,
        map(&[
            ("margin-bottom", "1px"),
            ("margin-left", "1px"),
            ("margin-right", "1px"),
            ("margin-top", "1px"),
        ])
    );

    // L353-356: right=values[1], bottom=values[2] else top, left=values[3]
    // else right.
    let mut out = decls();
    Renderer::new().add_css_declaration("margin", "1px 2px", &mut out);
    assert_eq!(
        out,
        map(&[
            ("margin-bottom", "1px"),
            ("margin-left", "2px"),
            ("margin-right", "2px"),
            ("margin-top", "1px"),
        ])
    );

    let mut out = decls();
    Renderer::new().add_css_declaration("margin", " 1px  2px  3px ", &mut out);
    assert_eq!(
        out,
        map(&[
            ("margin-bottom", "3px"),
            ("margin-left", "2px"),
            ("margin-right", "2px"),
            ("margin-top", "1px"),
        ])
    );

    let mut out = decls();
    Renderer::new().add_css_declaration("margin", "1px 2px 3px 4px", &mut out);
    assert_eq!(
        out,
        map(&[
            ("margin-bottom", "3px"),
            ("margin-left", "4px"),
            ("margin-right", "2px"),
            ("margin-top", "1px"),
        ])
    );
}

#[test]
fn margin_shorthand_rejects_more_than_four_values() {
    // L347-351: size() > 4 -> return; nothing is added.
    let mut out = decls();
    Renderer::new().add_css_declaration("margin", "1px 2px 3px 4px 5px", &mut out);
    assert_eq!(out, BTreeMap::new());
}

#[test]
fn margin_uses_parenthesis_aware_tokens() {
    // The margin scanner (L298-346) keeps "rgb(...)" as one token.
    let mut out = decls();
    Renderer::new().add_css_declaration("margin", "rgb(1, 2, 3)", &mut out);
    // "rgb" is a supported function, so each side stores the whole token.
    assert_eq!(
        out,
        map(&[
            ("margin-bottom", "rgb(1, 2, 3)"),
            ("margin-left", "rgb(1, 2, 3)"),
            ("margin-right", "rgb(1, 2, 3)"),
            ("margin-top", "rgb(1, 2, 3)"),
        ])
    );
}

#[test]
fn margin_sides_with_unsupported_functions_are_skipped() {
    // L372-382 runs inside each expanded side; "url" is unsupported, so only
    // the plain top/bottom survive.
    let mut out = decls();
    Renderer::new().add_css_declaration("margin", "10px url(x)", &mut out);
    // top = "10px", right = "url(x)" (dropped), bottom = top, left = right
    assert_eq!(
        out,
        map(&[("margin-bottom", "10px"), ("margin-top", "10px")])
    );
}

#[test]
fn clip_path_circle_maps_to_border_radius() {
    // L364-369, case-insensitive prefix on the replaced value.
    let mut out = decls();
    Renderer::new().add_css_declaration("clip-path", "Circle(50%)", &mut out);
    assert_eq!(out, map(&[("border-radius", "50%")]));
    // Also via a differently-cased property name.
    let mut out = decls();
    Renderer::new().add_css_declaration("CLIP-PATH", "circle(30px)", &mut out);
    assert_eq!(out, map(&[("border-radius", "50%")]));
    // A non-circle clip-path that is not supported: falls through to L429 and
    // "clip-path" is not in the supported set -> dropped.
    let mut out = decls();
    Renderer::new().add_css_declaration("clip-path", "inset(10px)", &mut out);
    assert_eq!(out, BTreeMap::new());
}

#[test]
fn unsupported_functions_drop_the_whole_declaration() {
    // L372-382: any unsupported function name found by the regex drops the
    // value (L378-380); "var", "url", "attr", "calc" are all unsupported.
    for value in [
        "var(--x) rgb(1,2,3)",
        "rgb(1,2,3) url(x.png)",
        "top attr(data)",
        "calc(1px + 2px)",
    ] {
        let mut out = decls();
        Renderer::new().add_css_declaration("margin-top", value, &mut out);
        assert_eq!(out, BTreeMap::new(), "value {value} should be dropped");
    }
    // Supported functions pass.
    let mut out = decls();
    Renderer::new().add_css_declaration("margin-top", "rgb(1, 2, 3)", &mut out);
    assert_eq!(out, map(&[("margin-top", "rgb(1, 2, 3)")]));
}

#[test]
fn function_regex_is_word_boundary_and_pcre_ascii_aware() {
    // `\b([A-Za-z-]+)\s*\(` (structuredrichtext_p.h L1384-1387):
    // - "abc94-(" yields a PCRE2 \b before the '-' that follows [A-Za-z0-9],
    //   capturing "-".
    let mut out = decls();
    Renderer::new().add_css_declaration("margin-top", "abc94-(", &mut out);
    assert_eq!(out, BTreeMap::new());
    // - digits do not start a capture ("94x(" must not capture anything).
    let mut out = decls();
    Renderer::new().add_css_declaration("margin-top", "94x(4)", &mut out);
    assert_eq!(out, map(&[("margin-top", "94x(4)")]));
}

#[test]
fn text_decoration_line_maps_to_text_decoration() {
    // L384-386.
    let mut out = decls();
    Renderer::new().add_css_declaration("text-decoration-line", "underline  line-through", &mut out);
    assert_eq!(out, map(&[("text-decoration", "underline  line-through")]));
}

#[test]
fn content_peels_one_pair_of_quotes() {
    // L388-398: the doubled quotes were replaced (L291) before peeling.
    let mut out = decls();
    Renderer::new().add_css_declaration("content", "\"hello\"", &mut out);
    assert_eq!(out, map(&[("content", "hello")]));

    let mut out = decls();
    Renderer::new().add_css_declaration("content", "'world'", &mut out);
    assert_eq!(out, map(&[("content", "world")]));

    // Unquoted values keep all characters; doubles were replaced first
    // (L291), so the leading backslash prevents peeling even though the value
    // ends in one quote.
    let mut out = decls();
    Renderer::new().add_css_declaration("content", "\\2014 \'x\"", &mut out);
    assert_eq!(out, map(&[("content", "\\2014 \'x\'")]));

    // A lone quote does not peel.
    let mut out = decls();
    Renderer::new().add_css_declaration("content", "'x", &mut out);
    assert_eq!(out, map(&[("content", "'x")]));
}

#[test]
fn background_promotes_single_token_colors_only() {
    // L399-405: needs no space and no url(.
    let mut out = decls();
    Renderer::new().add_css_declaration("background", "red", &mut out);
    assert_eq!(out, map(&[("background-color", "red")]));

    let mut out = decls();
    Renderer::new().add_css_declaration("background", "red blue", &mut out);
    assert_eq!(out, BTreeMap::new());

    let mut out = decls();
    Renderer::new().add_css_declaration("background", "url(x.png)", &mut out);
    assert_eq!(out, BTreeMap::new());
}

#[test]
fn list_style_takes_the_first_marker_token() {
    // L406-417: normalizeListMarker + isBuiltInListMarker from the layout port
    // (structuredrichtext_layout.cpp L1346-1435).
    let mut out = decls();
    Renderer::new().add_css_declaration("list-style", "square inside", &mut out);
    assert_eq!(out, map(&[("list-style-type", "square")]));

    let mut out = decls();
    Renderer::new().add_css_declaration("list-style", "'disc'", &mut out);
    assert_eq!(out, map(&[("list-style-type", "disc")]));

    // "none" is accepted before the built-in check (L411).
    let mut out = decls();
    Renderer::new().add_css_declaration("list-style", "'none' disc", &mut out);
    assert_eq!(out, map(&[("list-style-type", "none")]));

    // No built-in marker token -> nothing is declared (the list-style branch
    // never falls into the supported-property branch).
    let mut out = decls();
    Renderer::new().add_css_declaration("list-style", "decimal leading", &mut out);
    assert_eq!(out, BTreeMap::new());
}

#[test]
fn word_break_sets_white_space() {
    // L418-428 compared case-insensitively.
    let mut out = decls();
    Renderer::new().add_css_declaration("word-break", "KEEP-ALL", &mut out);
    assert_eq!(out, map(&[("white-space", "nowrap")]));

    let mut out = decls();
    Renderer::new().add_css_declaration("word-break", "Normal", &mut out);
    assert_eq!(out, map(&[("white-space", "normal")]));

    let mut out = decls();
    Renderer::new().add_css_declaration("word-break", "break-all", &mut out);
    assert_eq!(out, BTreeMap::new());
}

/* --- addResolvedCssDeclaration (L435-447) ---------------------------------- */

#[test]
fn resolved_declaration_skips_when_resolution_fails() {
    // resolveCssVariables returns "" for an empty fallback (L589-591); with
    // no glossary text color and no fallback, "var(--fg)" is discarded.
    let renderer = Renderer::new();
    let mut ctx = Context::default();
    let mut out = decls();
    renderer.add_resolved_css_declaration("color", "var(--fg)", &mut ctx, &mut out);
    assert_eq!(out, BTreeMap::new());

    // With a fallback the declaration is added after resolution.
    let mut out = decls();
    renderer.add_resolved_css_declaration(
        "color",
        "var(--fg, blue)",
        &mut ctx,
        &mut out,
    );
    assert_eq!(out, map(&[("color", "blue")]));
}

/* --- addCssDeclarations (L449-466) ----------------------------------------- */

#[test]
fn declarations_render_sorted_and_skip_metadata() {
    // QMap iterates sorted keys (PORT-NOTE: BTreeMap mirrors it); "content"
    // and "list-style-type" are metadata and skipped (L456-459).
    let mut out = map(&[
        ("color", "red"),
        ("font-size", "12px"),
        ("content", "x"),
        ("list-style-type", "disc"),
        ("border", "1px solid"),
    ]);
    let mut text = String::new();
    Renderer::new().add_css_declarations(&out, &mut text);
    assert_eq!(text, "border: 1px solid;color: red;font-size: 12px;");
    let _ = &mut out;
}

/* --- resolveCssValue (L468-502) + resolveCssVariables (L504-602) ---------- */

#[test]
fn resolve_value_passthrough_and_cache() {
    let renderer = Renderer::new();
    let mut ctx = Context::default();
    // No '(' -> fast-paths without allocating/polluting resolved_css_values.
    assert_eq!(renderer.resolve_css_value(" 10px red ", &mut ctx), "10px red");
    assert!(!ctx.resolved_css_values.contains_key(" 10px red "));
    // Values with '(' populate the cache keyed by the raw (untrimmed) input.
    assert_eq!(
        renderer.resolve_css_value(" var(--missing, 10px) ", &mut ctx),
        "10px"
    );
    assert!(ctx.resolved_css_values.contains_key(" var(--missing, 10px) "));
    // A second call returns the cached result.
    assert_eq!(
        renderer.resolve_css_value(" var(--missing, 10px) ", &mut ctx),
        "10px"
    );
}

#[test]
fn resolve_variables_named_palette_variables() {
    let renderer = Renderer::new();
    let mut ctx = Context::default();
    ctx.glossary_text_color = "#112233".to_string();
    ctx.glossary_background_color = "#445566".to_string();

    // L568-572: --text-color / --fg resolve to the glossary text color.
    assert_eq!(renderer.resolve_css_value("var(--text-color)", &mut ctx), "#112233");
    assert_eq!(renderer.resolve_css_value("var(--fg)", &mut ctx), "#112233");

    // L574-577: --font-size-no-units formats the root font size.
    assert_eq!(
        renderer.resolve_css_value("var(--font-size-no-units)", &mut ctx),
        "12"
    );
    // L578-583: --background-color / --canvas.
    assert_eq!(
        renderer.resolve_css_value("var(--background-color)", &mut ctx),
        "#445566"
    );
    assert_eq!(renderer.resolve_css_value("var(--canvas)", &mut ctx), "#445566");
}

#[test]
fn resolve_variables_fallbacks() {
    let renderer = Renderer::new();
    let mut ctx = Context::default();

    // Unknown variable: the fallback (L584-587).
    assert_eq!(
        renderer.resolve_css_value("var(--unknown, blue)", &mut ctx),
        "blue"
    );
    // Additional comma arguments join back (L563-565).
    assert_eq!(
        renderer.resolve_css_value("var(--unknown, 1px, solid)", &mut ctx),
        "1px,solid"
    );
    // Unknown with no fallback -> "" (L589-592).
    assert_eq!(renderer.resolve_css_value("var(--unknown)", &mut ctx), "");
    // Named variable with empty glossary color falls back (L568-572).
    assert_eq!(renderer.resolve_css_value("var(--text-color, black)", &mut ctx), "black");
    // Search is case-insensitive (L518-522).
    assert_eq!(renderer.resolve_css_value("VAR(--unknown, x)", &mut ctx), "x");
}

#[test]
fn resolve_variables_nested_and_recursive() {
    let renderer = Renderer::new();
    let mut ctx = Context::default();
    ctx.glossary_text_color = "#abc123".to_string();

    // L593-597: the replacement itself is resolved recursively.
    assert_eq!(
        renderer.resolve_css_value("var(--x, var(--text-color))", &mut ctx),
        "#abc123"
    );
    // Cascading fallbacks from multiple comma arguments (L563-565).
    assert_eq!(
        renderer.resolve_css_value("var(--x, var(--x, 1px))", &mut ctx),
        "1px"
    );
}

#[test]
fn resolve_variables_empty_replacement_fails() {
    let renderer = Renderer::new();
    let mut ctx = Context::default();
    // Nested empty fallback: the recursion at L593-597 yields "" and the
    // first call aborts (L594-596).
    assert_eq!(renderer.resolve_css_value("var(--x, var(--x))", &mut ctx), "");
    // Unbalanced var() -> "" (L544-546).
    assert_eq!(renderer.resolve_css_value("var(--x", &mut ctx), "");
}

#[test]
fn resolve_variables_fail_after_the_loop_budget() {
    let renderer = Renderer::new();
    let mut ctx = Context::default();
    // MAX_REPLACEMENTS = 32 (L509): nested fallbacks resolve through the
    // recursion (L593), so only *sibling* var() calls consume the loop
    // budget; after 32 substitutions the loop exits with "" (L600-601).
    // Sibling "var(--x, red)" calls are consumed one per pass.
    let value = "var(--x, red) ".repeat(33);
    let value = value.trim_end();
    assert_eq!(renderer.resolve_css_value(value, &mut ctx), "");
    // With exactly 32 siblings the 32 loop passes are all consumed by
    // substitutions, so control falls out of the loop and returns "" (the
    // "no more var(" check happens at the top of an iteration, L512-516).
    let value = "var(--x, red) ".repeat(32);
    let value = value.trim_end();
    assert_eq!(renderer.resolve_css_value(value, &mut ctx), "");
    // 31 siblings finish substituting inside the budget; the 32nd pass
    // observes no "var(" and returns the fully resolved value.
    let value = "var(--x, red) ".repeat(31);
    let value = value.trim_end();
    assert_eq!(renderer.resolve_css_value(value, &mut ctx), "red ".repeat(31).trim_end());

    // Deep nesting beyond MAX_VARIABLE_DEPTH / remaining budget fails cleanly without stack overflow.
    let mut deep = String::from("red");
    for _ in 0..40 {
        deep = format!("var(--x, {deep})");
    }
    assert_eq!(renderer.resolve_css_variables(&deep, &ctx), "");

    // Branching var() expansion shares a single remaining budget (no exponential blowup).
    let mut branching = String::from("var(--x, red)");
    for _ in 0..8 {
        branching = format!("var(--x, {branching} {branching})");
    }
    assert_eq!(renderer.resolve_css_variables(&branching, &ctx), "");
}

/* --- resolveCssCalculations (L604-658) + resolveCssCalc (L775-798) -------- */

#[test]
fn resolve_calculations_simple_and_repeated() {
    let renderer = Renderer::new();
    // "calc(12px / 2)" -> formatPixelSize(6.0) + "px" = "6px" (L647-654,
    // L790-797).
    assert_eq!(renderer.resolve_css_calculations("calc(12px / 2)"), "6px");
    // Several calculations resolve independently (each loop pass, L616-655).
    assert_eq!(
        renderer.resolve_css_calculations("calc(10px/2) calc(30px/3)"),
        "5px 10px"
    );
    // Values without calc pass through (L622-623).
    assert_eq!(renderer.resolve_css_calculations("10px"), "10px");
    // Unbalanced paren -> "" (L643-644).
    assert_eq!(renderer.resolve_css_calculations("calc(1px / 2"), "");
    // Case-insensitive "calc(" (L616-620).
    assert_eq!(renderer.resolve_css_calculations("CALC(12px / 2)"), "6px");
    // Nested calc: the whole expression is scanned, the inner "calc(" match
    // starts after "calc(" so the depth-scan slice cannot be parsed -> "".
    assert_eq!(renderer.resolve_css_calculations("calc(calc(2px) / 4)"), "");
}

#[test]
fn resolve_calc_division_details() {
    let renderer = Renderer::new();
    // Captures: number, unit, denominator (L790-797).
    assert_eq!(renderer.resolve_css_calc("calc(1.5px / 3)"), "0.5px");
    // formatPixelSize rounds to three decimals then trims (layout.cpp
    // L1692-1703).
    assert_eq!(renderer.resolve_css_calc("calc(1px / 3)"), "0.333px");
    // Unit case is preserved from the match.
    assert_eq!(renderer.resolve_css_calc("calc(10PX / 4)"), "2.5PX");
    // Percent units.
    assert_eq!(renderer.resolve_css_calc("calc(5% / 2)"), "2.5%");
    // Negative and unsigned fractions.
    assert_eq!(renderer.resolve_css_calc("calc(-10px / 2)"), "-5px");
    assert_eq!(renderer.resolve_css_calc("calc(.5ex / 2)"), "0.25ex");
    assert_eq!(renderer.resolve_css_calc("calc(+10px/5)"), "2px");
    // The regex requires the unit digits to butt against the number: after
    // "5." the only continuation is ([a-z%]+), which cannot start at the
    // space in "5. px"; PCRE backtracking over the shorter "5" match cannot
    // consume "." either, so the match fails (L779-782).
    assert_eq!(renderer.resolve_css_calc("calc(5. px / 2)"), "");
    // Zero denominator -> "" (L790-792).
    assert_eq!(renderer.resolve_css_calc("calc(10px / 0)"), "");
    assert_eq!(renderer.resolve_css_calc("calc(10px / -0)"), "");
    // Bad shapes -> "" (match fails, L784-787).
    assert_eq!(renderer.resolve_css_calc("calc(1px + 2px)"), "");
    assert_eq!(renderer.resolve_css_calc("calc(10 px / 2)"), "");
    assert_eq!(renderer.resolve_css_calc("calc(10px / 2) "), "");
    assert_eq!(renderer.resolve_css_calc("calc(10px / 2))"), "");
    assert_eq!(renderer.resolve_css_calc("calc(  / 2)"), "");
    assert_eq!(renderer.resolve_css_calc(""), "");
}

#[test]
fn resolve_calculations_nested_var_then_calc_order() {
    // resolveCssValue order: variables first (L478), then calculations
    // (L486-489); a calc inside a var fallback resolves through the variable.
    let renderer = Renderer::new();
    let mut ctx = Context::default();
    assert_eq!(
        renderer.resolve_css_value("calc(1px / var(--d, 2))", &mut ctx),
        "0.5px"
    );
}

/* --- resolveColorMix (L660-773) -------------------------------------------- */

#[test]
fn color_mix_equal_weights() {
    let renderer = Renderer::new();
    // 50/50 mix of pure red and pure blue, both opaque:
    // alpha = 1, red = (1*1*0.5 + 0)/1 = 0.5 -> 0x80, blue likewise 0.5,
    // green 0 -> name(HexArgb) = "#ff800080" (L743-772).
    assert_eq!(
        renderer.resolve_color_mix("color-mix(in srgb, red, blue)"),
        "#ff800080"
    );
    // The "in" argument may be missing; case-insensitive prefix (L693-697).
    assert_eq!(renderer.resolve_color_mix("color-mix(red, blue)"), "#ff800080");
    assert_eq!(
        renderer.resolve_color_mix("color-mix(IN srgb, red, blue)"),
        "#ff800080"
    );
}

#[test]
fn color_mix_percentages() {
    let renderer = Renderer::new();
    // "red 75%": PERCENTAGE matches at capturedStart 3 with capture "75"
    // (L709-711); the other color gets 100 - 75 = 25 (L738-741).
    // red channel = 0.75 -> 0.75*65535+0.5 truncated = 49151 (0xBFFF) -> 0xBF;
    // blue = 0.25*65535+0.5 -> 16384 (0x4000) -> 0x40.
    assert_eq!(
        renderer.resolve_color_mix("color-mix(red 75%, blue)"),
        "#ffbf0040"
    );
    // Percentage on the second argument only: 100 - 25 = 75 for red.
    assert_eq!(
        renderer.resolve_color_mix("color-mix(red, blue 25%)"),
        "#ffbf0040"
    );
    // Extra whitespace before the percentage still matches (\s+ greedy,
    // leftmost capturedStart) (L713-721).
    assert_eq!(
        renderer.resolve_color_mix("color-mix(red  50%, blue  50%)"),
        "#ff800080"
    );
    // A percentage without preceding whitespace is not a percentage match
    // (\s+ requires one), so "red75%" is an invalid color -> "" (L713-721,
    // L722-726).
    assert_eq!(
        renderer.resolve_color_mix("color-mix(red75%, blue)"),
        ""
    );
}

#[test]
fn color_mix_alpha_channel() {
    let renderer = Renderer::new();
    // transparent red (alpha 0) mixed half-half with opaque blue:
    // alpha = 0*0.5 + 1*0.5 = 0.5; pre-multiplied red = 0/0.5 -> normalized
    // red = 1 -> "#80ff0000".
    assert_eq!(
        renderer.resolve_color_mix("color-mix(transparent, red)"),
        "#80ff0000"
    );
    // alpha <= 0 short-circuits to QColor(Qt::transparent).name(HexArgb)
    // (L750-756).
    assert_eq!(
        renderer.resolve_color_mix("color-mix(transparent 100%, red)"),
        "#00000000"
    );
}

#[test]
fn color_mix_failures() {
    let renderer = Renderer::new();
    assert_eq!(renderer.resolve_color_mix("color-mix(red, blue, green)"), "");
    assert_eq!(renderer.resolve_color_mix("color-mix(in srgb, red)"), "");
    assert_eq!(renderer.resolve_color_mix("color-mix(notacolor, blue)"), "");
    assert_eq!(renderer.resolve_color_mix("color-mix(in srgb, red, blue"), "");
    // Explicit percentages of 0+0 leave total 0 -> "" (L693-700, L743-747).
    assert_eq!(
        renderer.resolve_color_mix("color-mix(red 0%, blue 0%)"),
        ""
    );
    // Non-ASCII UTF-8 strings shorter or crossing byte offset 10 must never panic.
    assert_eq!(renderer.resolve_color_mix("日本語日本語)"), "");
    assert_eq!(renderer.resolve_color_mix("あ)"), "");
    assert_eq!(renderer.resolve_color_mix("color-mix)"), "");
}

#[test]
fn structured_style_iterates_present_keys_preserves_precedence_and_numeric_font_size() {
    let renderer = Renderer::new();
    let mut ctx = Context::default();
    ctx.parent_font_pixel_size = 16.0;
    ctx.root_font_pixel_size = 16.0;

    let style_val: serde_json::Value = serde_json::json!({
        "fontSize": "1.5em",
        "background": "red",
        "backgroundColor": "blue",
        "borderRadius": "4px",
        "clipPath": "circle(50%)",
        "margin": "1px",
        "marginTop": 2.0,
        "wordBreak": "keep-all",
        "whiteSpace": "normal",
        "textDecorationLine": ["underline", "line-through"]
    });
    let mut state = crate::element::ElementRenderState::default();
    renderer.add_structured_style_value(&style_val, &mut ctx, &mut state);
    assert_eq!(state.font_pixel_size, 24.0);
    assert_eq!(state.font_size_px, Some(24.0));
    assert_eq!(state.declarations.get("background-color").map(String::as_str), Some("blue"));
    assert_eq!(state.declarations.get("border-radius").map(String::as_str), Some("50%"));
    assert_eq!(state.declarations.get("margin-top").map(String::as_str), Some("2em"));
    assert_eq!(state.declarations.get("margin-bottom").map(String::as_str), Some("1px"));
    assert_eq!(state.declarations.get("white-space").map(String::as_str), Some("normal"));
    assert_eq!(
        state.declarations.get("text-decoration").map(String::as_str),
        Some("underline line-through")
    );
}

/* --- resolveCssValue -> gradient path (L496-499, L800-829) ----------------- */

#[test]
fn gradient_fallback_uses_the_first_color_stop() {
    let renderer = Renderer::new();
    // First argument "red" -> name(HexArgb) = "#ffff0000" (L808-828).
    assert_eq!(
        renderer.css_gradient_fallback("linear-gradient(red, blue)"),
        "#ffff0000"
    );
    assert_eq!(
        renderer.css_gradient_fallback("LINEAR-GRADIENT(blue, red)"),
        "#ff0000ff"
    );
    // A trailing percentage stop is stripped before parsing (L818-825).
    assert_eq!(
        renderer.css_gradient_fallback("linear-gradient(red 50%, blue)"),
        "#ffff0000"
    );
    // arguments.front() is "to right", which is not a color -> "" (L807-828).
    assert_eq!(
        renderer.css_gradient_fallback("linear-gradient(to right, red)"),
        ""
    );
    // Unsupported first color -> "".
    assert_eq!(
        renderer.css_gradient_fallback("linear-gradient(mixcolor, blue)"),
        ""
    );
    assert_eq!(renderer.css_gradient_fallback("red"), "");
    assert_eq!(renderer.css_gradient_fallback("linear-gradient(red, blue"), "");
    // Single-stop gradient must not chop the final character of the color name.
    assert_eq!(
        renderer.css_gradient_fallback("linear-gradient(red)"),
        "#ffff0000"
    );
    assert_eq!(
        renderer.css_gradient_fallback("radial-gradient(#123456)"),
        "#ff123456"
    );
    assert_eq!(renderer.css_gradient_fallback("linear-gradient()"), "");
    // Multi-byte UTF-8 at the end of gradient arguments must never panic or slice mid-scalar.
    assert_eq!(
        renderer.css_gradient_fallback("linear-gradient(red, 日本語)"),
        "#ffff0000"
    );
    assert_eq!(
        renderer.css_gradient_fallback("linear-gradient(red 25%, \"青色𠮟\")"),
        "#ffff0000"
    );
    assert_eq!(
        renderer.css_gradient_fallback("linear-gradient(日本語)"),
        ""
    );
}

#[test]
fn escaped_css_quotes_and_multibyte_utf8_values_survive_token_and_argument_scanners() {
    let renderer = Renderer::new();
    assert_eq!(
        split_css_tokens(r#""日本語 \"引用\" テスト" 12px 'かな \'𠮟\' 字'"#),
        vec![r#""日本語 \"引用\" テスト""#, "12px", r#"'かな \'𠮟\' 字'"#]
    );
    assert_eq!(
        split_css_tokens(r#""escaped backslash \\" next"#),
        vec![r#""escaped backslash \\""#, "next"]
    );
    assert_eq!(
        renderer.split_css_arguments(r#"a, "b, \"c, 日本語\"", 'd, \'e, 𠮟\''"#),
        vec!["a", r#""b, \"c, 日本語\"""#, r#"'d, \'e, 𠮟\''"#]
    );

    let mut out = decls();
    renderer.add_css_declaration("content", r#""語彙 \"例\" 𠮟""#, &mut out);
    assert_eq!(out, map(&[("content", "語彙 \"例\" 𠮟")]));

    let mut out = decls();
    renderer.add_css_declaration("content", r#"'don\'t — 日本語'"#, &mut out);
    assert_eq!(out, map(&[("content", "don't — 日本語")]));

    // Unclosed quote ending in an escaped quote must not peel outer delimiters.
    let mut out = decls();
    renderer.add_css_declaration("content", r#"'x\'"#, &mut out);
    assert_eq!(out, map(&[("content", r#"'x\'"#)]));

    let mut ctx = Context::default();
    assert_eq!(
        renderer.resolve_css_value(r#"var(--日本語, "値)テスト")"#, &mut ctx),
        r#""値)テスト""#
    );
    assert_eq!(
        renderer.resolve_css_value(r#"var(--x, "a\")b")"#, &mut ctx),
        r#""a\")b""#
    );
}

#[test]
fn resolve_value_routes_color_mix_and_gradient() {
    let renderer = Renderer::new();
    let mut ctx = Context::default();
    // startsWith("color-mix(") wins over the gradient check (L492-496).
    assert_eq!(
        renderer.resolve_css_value("color-mix(in srgb, red, blue)", &mut ctx),
        "#ff800080"
    );
    // contains("gradient(") is case-insensitive (L496-499).
    assert_eq!(
        renderer.resolve_css_value("linear-gradient(red, blue)", &mut ctx),
        "#ffff0000"
    );
}

/* --- splitCssArguments (L831-878) ------------------------------------------ */

#[test]
fn arguments_split_at_top_level_commas_and_trim() {
    let renderer = Renderer::new();
    // L866-871: quotes and parentheses hide commas; each piece is trimmed
    // (L868).
    assert_eq!(
        renderer.split_css_arguments("a, (b, c), 'd,e'"),
        vec!["a", "(b, c)", "'d,e'"]
    );
    // The last argument is always emitted, even empty (L876-877).
    assert_eq!(renderer.split_css_arguments(""), vec![""]);
    assert_eq!(renderer.split_css_arguments("a,"), vec!["a", ""]);
    assert_eq!(
        renderer.split_css_arguments("rgb(1, 2), 3"),
        vec!["rgb(1, 2)", "3"]
    );
}

/* --- structuredDataAttributeName (L1204-1221) ------------------------------- */

#[test]
fn data_attribute_name_hyphenates_uppercase_runs() {
    let renderer = Renderer::new();
    assert_eq!(renderer.structured_data_attribute_name("myKey"), "data-sc-my-key");
    // Each uppercase char contributes '-'+lower, including the first one.
    assert_eq!(
        renderer.structured_data_attribute_name("HTTPS"),
        "data-sc--h-t-t-p-s"
    );
    // Lowercase and non-letters pass through.
    assert_eq!(renderer.structured_data_attribute_name("a1_2b"), "data-sc-a1_2b");
    // QChar::isUpper() is Unicode Lu on a single UTF-16 code unit, not the
    // broader uppercase property or scalar-aware supplementary casing.
    assert_eq!(renderer.structured_data_attribute_name("ⅠⒶ"), "data-sc-ⅠⒶ");
    assert_eq!(renderer.structured_data_attribute_name("𐐀"), "data-sc-𐐀");
}

/* --- isSupportedStructuredTag (L1223-1226) ---------------------------------- */

#[test]
fn supported_tags_match_the_cpp_qset() {
    let renderer = Renderer::new();
    for tag in ["br", "ruby", "rp", "rt", "table", "thead", "tbody", "tfoot", "tr", "td", "th",
                "span", "div", "ol", "ul", "li", "details", "summary", "img", "a"] {
        assert!(renderer.is_supported_structured_tag(tag), "{tag}");
    }
    for tag in ["", "blink", "SPAN", "tablee"] {
        assert!(!renderer.is_supported_structured_tag(tag), "{tag}");
    }
}

/* --- Scoped :root custom properties, element vars & Light/Dark theme -------- */

#[test]
fn scoped_root_custom_properties_nested_vars_fallbacks_and_cycles() {
    use crate::css::parse_stylesheet;
    use crate::element::CssTheme;
    use std::sync::Arc;

    let renderer = Renderer::new();
    let sheet = Arc::new(parse_stylesheet(
        r#"
        :root {
            --primary: #38bdf8;
            --border-spec: 1px solid var(--primary);
            --double-primary: var(--primary) var(--primary);
            --half-size: calc(16px / var(--divisor, 2));
            --divisor: 4;
            --self-cycle: var(--self-cycle);
            --cycle-a: var(--cycle-b);
            --cycle-b: var(--cycle-a);
            --MixedCase: #abcdef;
        }
        "#,
    ));
    assert!(
        sheet.diagnostics.is_empty(),
        "expected no diagnostics for :root custom vars, got {:?}",
        sheet.diagnostics
    );

    let mut ctx = Context {
        dictionary_styles: Some(sheet),
        ..Context::default()
    };
    assert_eq!(ctx.theme, CssTheme::Dark);

    // Direct and nested custom var resolution + valid diamond expansion.
    assert_eq!(renderer.resolve_css_value("var(--primary)", &mut ctx), "#38bdf8");
    assert_eq!(
        renderer.resolve_css_value("var(--border-spec)", &mut ctx),
        "1px solid #38bdf8"
    );
    assert_eq!(
        renderer.resolve_css_value("var(--double-primary)", &mut ctx),
        "#38bdf8 #38bdf8"
    );
    assert_eq!(renderer.resolve_css_value("var(--half-size)", &mut ctx), "4px");
    assert_eq!(renderer.resolve_css_value("var(--MixedCase)", &mut ctx), "#abcdef");

    // Undefined var uses fallback; defined var ignores fallback.
    assert_eq!(
        renderer.resolve_css_value("var(--missing, var(--primary))", &mut ctx),
        "#38bdf8"
    );
    assert_eq!(
        renderer.resolve_css_value("var(--primary, #000000)", &mut ctx),
        "#38bdf8"
    );

    // Direct and mutual cycles terminate safely and trigger outer fallback if provided.
    assert_eq!(renderer.resolve_css_value("var(--self-cycle)", &mut ctx), "");
    assert_eq!(
        renderer.resolve_css_value("var(--self-cycle, #123456)", &mut ctx),
        "#123456"
    );
    assert_eq!(renderer.resolve_css_value("var(--cycle-a)", &mut ctx), "");
    assert_eq!(
        renderer.resolve_css_value("var(--cycle-a, #654321)", &mut ctx),
        "#654321"
    );
}

#[test]
fn theme_switching_resolves_root_data_theme_and_builtin_palette() {
    use crate::css::parse_stylesheet;
    use crate::element::{CssTheme, StructuredElement};
    use std::sync::Arc;

    let renderer = Renderer::new();
    // Place :root[data-theme="..."] BEFORE bare :root to verify higher specificity wins.
    let sheet = Arc::new(parse_stylesheet(
        r#"
        :root[data-theme="light"] {
            --accent: #0284c7;
            --surface: #f8fafc;
        }
        :root[data-theme="dark"] {
            --accent: #7dd3fc;
        }
        :root {
            --accent: #999999;
            --surface: #1e293b;
            --text-alias: var(--text-color);
            --bg-alias: var(--background-color);
        }
        :root[data-theme="light"] span {
            font-weight: bold;
        }
        :root[data-theme="dark"] span {
            font-style: italic;
        }
        span {
            color: var(--accent);
            background-color: var(--surface);
        }
        "#,
    ));
    assert!(sheet.diagnostics.is_empty(), "{:?}", sheet.diagnostics);

    // Default theme is Dark.
    let mut ctx = Context::with_theme(CssTheme::Dark);
    ctx.dictionary_styles = Some(Arc::clone(&sheet));
    assert_eq!(
        renderer.resolve_css_value("var(--text-color)", &mut ctx),
        CssTheme::Dark.default_text_color()
    );
    assert_eq!(
        renderer.resolve_css_value("var(--fg)", &mut ctx),
        CssTheme::Dark.default_text_color()
    );
    assert_eq!(
        renderer.resolve_css_value("var(--background-color)", &mut ctx),
        CssTheme::Dark.default_background_color()
    );
    assert_eq!(
        renderer.resolve_css_value("var(--canvas)", &mut ctx),
        CssTheme::Dark.default_background_color()
    );
    assert_eq!(renderer.resolve_css_value("var(--accent)", &mut ctx), "#7dd3fc");
    assert_eq!(renderer.resolve_css_value("var(--surface)", &mut ctx), "#1e293b");
    assert_eq!(
        renderer.resolve_css_value("var(--text-alias)", &mut ctx),
        CssTheme::Dark.default_text_color()
    );

    // Explicit theme parameter switches both custom vars and built-in palette without mutating ctx.
    assert_eq!(
        renderer.resolve_css_value_with_theme("var(--accent)", &mut ctx, CssTheme::Light),
        "#0284c7"
    );
    assert_eq!(
        renderer.resolve_css_value_with_theme("var(--surface)", &mut ctx, CssTheme::Light),
        "#f8fafc"
    );
    assert_eq!(
        renderer.resolve_css_value_with_theme("var(--text-color)", &mut ctx, CssTheme::Light),
        CssTheme::Light.default_text_color()
    );
    assert_eq!(
        renderer.resolve_css_value_with_theme("var(--background-color)", &mut ctx, CssTheme::Light),
        CssTheme::Light.default_background_color()
    );

    // Selector matching under Dark vs Light theme.
    let mut span = StructuredElement::new();
    span.tag = "span".to_owned();
    ctx.selector_elements.push(span);
    ctx.elements.push(0);

    let mut dark_decls = CssDeclarations::new();
    renderer.add_matching_css_rules(&mut ctx, &mut dark_decls, None, None);
    assert_eq!(dark_decls.get("color").map(String::as_str), Some("#7dd3fc"));
    assert_eq!(
        dark_decls.get("background-color").map(String::as_str),
        Some("#1e293b")
    );
    assert_eq!(dark_decls.get("font-style").map(String::as_str), Some("italic"));
    assert!(!dark_decls.contains_key("font-weight"));

    // Switch context to Light theme (must invalidate cache and switch matched rules + vars).
    ctx.set_theme(CssTheme::Light);
    let mut light_decls = CssDeclarations::new();
    renderer.add_matching_css_rules(&mut ctx, &mut light_decls, None, None);
    assert_eq!(light_decls.get("color").map(String::as_str), Some("#0284c7"));
    assert_eq!(
        light_decls.get("background-color").map(String::as_str),
        Some("#f8fafc")
    );
    assert_eq!(light_decls.get("font-weight").map(String::as_str), Some("bold"));
    assert!(!light_decls.contains_key("font-style"));
    assert_eq!(
        renderer.resolve_css_value("var(--bg-alias)", &mut ctx),
        CssTheme::Light.default_background_color()
    );
}

#[test]
fn element_scoped_custom_properties_inherit_and_do_not_leak_to_siblings() {
    use crate::css::parse_stylesheet;
    use crate::element::StructuredElement;
    use std::sync::Arc;

    let renderer = Renderer::new();
    let sheet = Arc::new(parse_stylesheet(
        r#"
        :root { --tone: #aaaaaa; }
        div.card {
            color: var(--tone);
            --tone: #10b981;
        }
        span.inner { color: var(--tone); }
        span.override { --tone: #f43f5e; color: var(--tone); }
        "#,
    ));

    let mut ctx = Context {
        dictionary_styles: Some(sheet),
        ..Context::default()
    };

    // Push div.card (declares --tone after color: var(--tone)).
    let mut div = StructuredElement::new();
    div.tag = "div".to_owned();
    div.classes.insert("card".to_owned());
    ctx.selector_elements.push(div);
    ctx.elements.push(0);

    let mut div_decls = CssDeclarations::new();
    renderer.add_matching_css_rules(&mut ctx, &mut div_decls, None, None);
    assert_eq!(div_decls.get("color").map(String::as_str), Some("#10b981"));
    assert!(!div_decls.contains_key("--tone"));

    // Child span.inner inherits div.card's --tone.
    let mut inner = StructuredElement::new();
    inner.tag = "span".to_owned();
    inner.classes.insert("inner".to_owned());
    ctx.selector_elements.push(inner);
    ctx.elements.push(1);
    let mut inner_decls = CssDeclarations::new();
    renderer.add_matching_css_rules(&mut ctx, &mut inner_decls, None, None);
    assert_eq!(inner_decls.get("color").map(String::as_str), Some("#10b981"));
    ctx.elements.pop();

    // Child span.override overrides --tone locally.
    let mut over = StructuredElement::new();
    over.tag = "span".to_owned();
    over.classes.insert("override".to_owned());
    ctx.selector_elements.push(over);
    ctx.elements.push(2);
    let mut over_decls = CssDeclarations::new();
    renderer.add_matching_css_rules(&mut ctx, &mut over_decls, None, None);
    assert_eq!(over_decls.get("color").map(String::as_str), Some("#f43f5e"));
    ctx.elements.pop();

    // Pop div.card: a sibling span.inner outside div.card sees :root's --tone again.
    ctx.elements.pop();
    let mut outside = StructuredElement::new();
    outside.tag = "span".to_owned();
    outside.classes.insert("inner".to_owned());
    ctx.selector_elements.push(outside);
    ctx.elements.push(3);
    let mut outside_decls = CssDeclarations::new();
    renderer.add_matching_css_rules(&mut ctx, &mut outside_decls, None, None);
    assert_eq!(outside_decls.get("color").map(String::as_str), Some("#aaaaaa"));
}

#[test]
fn root_custom_properties_are_strictly_isolated_across_dictionaries() {
    use crate::css::parse_stylesheet;
    use std::sync::Arc;

    let renderer = Renderer::new();
    let dict_a = Arc::new(parse_stylesheet(
        r#"
        :root { --dict-accent: #ff0000; --shared-name: #111111; }
        :root[data-theme="light"] { --dict-accent: #aa0000; }
        "#,
    ));
    let dict_b = Arc::new(parse_stylesheet(
        r#"
        :root { --shared-name: #222222; }
        "#,
    ));

    // Separate contexts never leak across dictionaries.
    let mut ctx_a = Context {
        dictionary_styles: Some(Arc::clone(&dict_a)),
        ..Context::default()
    };
    let mut ctx_b = Context {
        dictionary_styles: Some(Arc::clone(&dict_b)),
        ..Context::default()
    };

    assert_eq!(renderer.resolve_css_value("var(--dict-accent)", &mut ctx_a), "#ff0000");
    assert_eq!(renderer.resolve_css_value("var(--shared-name)", &mut ctx_a), "#111111");

    assert_eq!(renderer.resolve_css_value("var(--dict-accent)", &mut ctx_b), "");
    assert_eq!(
        renderer.resolve_css_value("var(--dict-accent, #00ff00)", &mut ctx_b),
        "#00ff00"
    );
    assert_eq!(renderer.resolve_css_value("var(--shared-name)", &mut ctx_b), "#222222");

    // Reusing the same Context after swapping stylesheets also invalidates cache cleanly.
    ctx_a.dictionary_styles = Some(dict_b);
    assert_eq!(
        renderer.resolve_css_value("var(--dict-accent, #00ff00)", &mut ctx_a),
        "#00ff00"
    );
    assert_eq!(renderer.resolve_css_value("var(--shared-name)", &mut ctx_a), "#222222");
}

#[test]
fn nested_root_theme_blocks_and_color_mix_with_custom_vars() {
    use crate::css::parse_stylesheet;
    use crate::element::{CssTheme, StructuredElement};
    use std::sync::Arc;

    let renderer = Renderer::new();
    let sheet = Arc::new(parse_stylesheet(
        r#"
        :root {
            --base: #ff0000;
            --mix-with: #0000ff;
            &[data-theme="light"] {
                --base: #00ff00;
            }
            span {
                color: color-mix(in srgb, var(--base) 50%, var(--mix-with));
            }
        }
        "#,
    ));
    assert!(sheet.diagnostics.is_empty(), "{:?}", sheet.diagnostics);

    let mut ctx = Context::with_theme(CssTheme::Dark);
    ctx.dictionary_styles = Some(sheet);
    let mut span = StructuredElement::new();
    span.tag = "span".to_owned();
    ctx.selector_elements.push(span);
    ctx.elements.push(0);

    let mut dark_decls = CssDeclarations::new();
    renderer.add_matching_css_rules(&mut ctx, &mut dark_decls, None, None);
    assert_eq!(dark_decls.get("color").map(String::as_str), Some("#ff800080"));

    ctx.set_theme(CssTheme::Light);
    let mut light_decls = CssDeclarations::new();
    renderer.add_matching_css_rules(&mut ctx, &mut light_decls, None, None);
    assert_eq!(light_decls.get("color").map(String::as_str), Some("#ff008080"));
}

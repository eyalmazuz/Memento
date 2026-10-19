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
    // No var/calc/color-mix/gradient: returned untouched (L514-526).
    assert_eq!(renderer.resolve_css_value("10px red", &mut ctx), "10px red");
    // Raw (untrimmed) value is the cache key (L472, L500).
    assert!(ctx.resolved_css_values.contains_key("10px red"));
    // A second call returns the cached result.
    assert_eq!(renderer.resolve_css_value("10px red", &mut ctx), "10px red");
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

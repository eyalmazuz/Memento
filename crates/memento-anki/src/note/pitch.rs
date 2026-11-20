//! Pitch accent classification, text/position/SVG formatting.

use std::fmt::Write as _;

use crate::glossary::escape::{append_escaped, append_escaped_str, append_u8_u16, push_ascii};

/// Matches `isKifukuApplicable` in `notebuilder.cpp`.
pub fn is_kifuku_applicable(rules: &[&[u16]]) -> bool {
    const VS: [u16; 2] = [b'v' as u16, b's' as u16];
    const V1: [u16; 2] = [b'v' as u16, b'1' as u16];
    const V5: [u16; 2] = [b'v' as u16, b'5' as u16];
    const VK: [u16; 2] = [b'v' as u16, b'k' as u16];
    const VZ: [u16; 2] = [b'v' as u16, b'z' as u16];
    const ADJ_I: [u16; 5] = [
        b'a' as u16,
        b'd' as u16,
        b'j' as u16,
        b'-' as u16,
        b'i' as u16,
    ];
    let mut can_be_kifuku = false;
    for &rule in rules {
        if rule == VS {
            return false;
        } else if rule == V1 || rule == V5 || rule == VK || rule == VZ || rule == ADJ_I {
            can_be_kifuku = true;
        }
    }
    can_be_kifuku
}

/// Borrowed pitch entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PitchEntry<'a> {
    pub dictionary_name: &'a [u16],
    pub positions: &'a [u8],
    pub mora: &'a [&'a [u16]],
}

/// Matches `getPitch` in `notebuilder.cpp`, HTML-escaping dictionary names and mora.
pub fn format_pitch(pitches: &[PitchEntry<'_>]) -> Vec<u16> {
    if pitches.is_empty() {
        return Vec::new();
    }

    let multiple_dicts = pitches.len() > 1;
    let mut pitch = Vec::new();
    push_ascii(&mut pitch, "<span class=\"memento-pitch\">");

    if multiple_dicts {
        push_ascii(&mut pitch, "<ul>");
    }

    for p in pitches {
        let multiple_pitches = p.positions.len() > 1;

        if multiple_dicts {
            push_ascii(&mut pitch, "<li><i>");
            append_escaped(p.dictionary_name, &mut pitch, false);
            push_ascii(&mut pitch, "</i>");
        }
        if multiple_pitches {
            push_ascii(&mut pitch, "<ol>");
        }

        for &pos in p.positions {
            if p.mora.is_empty() {
                continue;
            }

            if multiple_pitches {
                push_ascii(&mut pitch, "<li style=\"padding: 2px 0px 2px 0px;\">");
            }

            match pos {
                0 => {
                    append_escaped(p.mora[0], &mut pitch, false);
                    if p.mora.len() > 1 {
                        push_ascii(&mut pitch, "<span style=\"border-top: solid;\">");
                        for m in &p.mora[1..] {
                            append_escaped(m, &mut pitch, false);
                        }
                        push_ascii(&mut pitch, "</span>");
                    }
                }
                1 => {
                    push_ascii(
                        &mut pitch,
                        "<span style=\"border-top: solid; border-right: solid;\">",
                    );
                    append_escaped(p.mora[0], &mut pitch, false);
                    push_ascii(&mut pitch, "</span>");
                    for m in &p.mora[1..] {
                        append_escaped(m, &mut pitch, false);
                    }
                }
                _ => {
                    let pos_usize = pos as usize;
                    append_escaped(p.mora[0], &mut pitch, false);

                    let limit = pos_usize.min(p.mora.len());
                    if limit > 1 {
                        let has_middle = p.mora[1..limit].iter().any(|m| !m.is_empty());
                        if has_middle {
                            push_ascii(
                                &mut pitch,
                                "<span style=\"border-top: solid; border-right: solid;\">",
                            );
                            for m in &p.mora[1..limit] {
                                append_escaped(m, &mut pitch, false);
                            }
                            push_ascii(&mut pitch, "</span>");
                        }
                    }

                    if pos_usize < p.mora.len() {
                        for m in &p.mora[pos_usize..] {
                            append_escaped(m, &mut pitch, false);
                        }
                    }
                }
            }

            if multiple_pitches {
                push_ascii(&mut pitch, "</li>");
            }
        }

        if multiple_pitches {
            push_ascii(&mut pitch, "</ol>");
        }
        if multiple_dicts {
            push_ascii(&mut pitch, "</li>");
        }
    }

    if multiple_dicts {
        push_ascii(&mut pitch, "</ul>");
    }
    push_ascii(&mut pitch, "</span>");
    pitch
}

/// Matches `getPitchPositions` in `notebuilder.cpp`, HTML-escaping dictionary names.
pub fn format_pitch_positions(pitches: &[PitchEntry<'_>]) -> Vec<u16> {
    if pitches.is_empty() {
        return Vec::new();
    }

    let multiple_dicts = pitches.len() > 1;
    let mut pitch_pos = Vec::new();
    push_ascii(&mut pitch_pos, "<span class=\"memento-pitch-position\">");

    if multiple_dicts {
        push_ascii(&mut pitch_pos, "<ul>");
    }

    for p in pitches {
        let multiple_pitches = p.positions.len() > 1;

        if multiple_dicts {
            push_ascii(&mut pitch_pos, "<li><i>");
            append_escaped(p.dictionary_name, &mut pitch_pos, false);
            push_ascii(&mut pitch_pos, "</i>");
        }
        if multiple_pitches {
            push_ascii(&mut pitch_pos, "<ol>");
        }

        for &pos in p.positions {
            if p.mora.is_empty() {
                continue;
            }

            if multiple_pitches {
                push_ascii(&mut pitch_pos, "<li>");
            }

            pitch_pos.push(b'[' as u16);
            append_u8_u16(&mut pitch_pos, pos);
            pitch_pos.push(b']' as u16);

            if multiple_pitches {
                push_ascii(&mut pitch_pos, "</li>");
            }
        }

        if multiple_pitches {
            push_ascii(&mut pitch_pos, "</ol>");
        }
        if multiple_dicts {
            push_ascii(&mut pitch_pos, "</li>");
        }
    }

    if multiple_dicts {
        push_ascii(&mut pitch_pos, "</ul>");
    }
    push_ascii(&mut pitch_pos, "</span>");
    pitch_pos
}

/// Borrowed pitch graph entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PitchGraphEntry<'a> {
    pub dictionary_name: &'a [u16],
    pub positions_count: usize,
    pub svg_graphs: &'a [&'a [u16]],
}

/// Matches `getPitchGraphs` in `notebuilder.cpp`, HTML-escaping dictionary names.
pub fn format_pitch_graphs(pitches: &[PitchGraphEntry<'_>]) -> Vec<u16> {
    if pitches.is_empty() {
        return Vec::new();
    }

    let multiple_dicts = pitches.len() > 1;
    let mut pitch_graph = Vec::new();
    push_ascii(&mut pitch_graph, "<span class=\"memento-pitch-graph\">");

    if multiple_dicts {
        push_ascii(&mut pitch_graph, "<ul>");
    }

    for p in pitches {
        let multiple_pitches = p.positions_count > 1;

        if multiple_dicts {
            push_ascii(&mut pitch_graph, "<li><i>");
            append_escaped(p.dictionary_name, &mut pitch_graph, false);
            push_ascii(&mut pitch_graph, "</i>");
        }
        if multiple_pitches {
            push_ascii(&mut pitch_graph, "<ol>");
        }

        for &svg in p.svg_graphs {
            if multiple_pitches {
                push_ascii(&mut pitch_graph, "<li>");
            }
            pitch_graph.extend_from_slice(svg);
            if multiple_pitches {
                push_ascii(&mut pitch_graph, "</li>");
            }
        }

        if multiple_pitches {
            push_ascii(&mut pitch_graph, "</ol>");
        }
        if multiple_dicts {
            push_ascii(&mut pitch_graph, "</li>");
        }
    }

    if multiple_dicts {
        push_ascii(&mut pitch_graph, "</ul>");
    }
    push_ascii(&mut pitch_graph, "</span>");
    pitch_graph
}

/// Pitch accent category classification.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(u8)]
pub enum PitchCategory {
    Heiban = 1,
    Kifuku = 2,
    Atamadaka = 3,
    Odaka = 4,
    Nakadaka = 5,
}

impl PitchCategory {
    pub const fn as_str(&self) -> &'static str {
        match self {
            PitchCategory::Heiban => "heiban",
            PitchCategory::Kifuku => "kifuku",
            PitchCategory::Atamadaka => "atamadaka",
            PitchCategory::Odaka => "odaka",
            PitchCategory::Nakadaka => "nakadaka",
        }
    }
}

/// Matches classification inside `getPitchCategories` in `notebuilder.cpp`.
pub fn classify_pitch(mora_len: usize, pos: u8, can_be_kifuku: bool) -> Option<PitchCategory> {
    if mora_len == 0 {
        return None;
    }
    match pos {
        0 => Some(PitchCategory::Heiban),
        1 => Some(if can_be_kifuku {
            PitchCategory::Kifuku
        } else {
            PitchCategory::Atamadaka
        }),
        _ => {
            if mora_len == pos as usize {
                Some(PitchCategory::Odaka)
            } else if can_be_kifuku {
                Some(PitchCategory::Kifuku)
            } else {
                Some(PitchCategory::Nakadaka)
            }
        }
    }
}

/// Input entry for pitch category classification.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PitchCategoryInput<'a> {
    pub mora_len: usize,
    pub positions: &'a [u8],
}

/// Matches `getPitchCategories` classification in `notebuilder.cpp`.
/// Returns unique categories in encounter order.
pub fn classify_pitch_categories(
    pitches: &[PitchCategoryInput<'_>],
    can_be_kifuku: bool,
) -> Vec<PitchCategory> {
    let mut categories = Vec::with_capacity(5);
    for p in pitches {
        for &pos in p.positions {
            if let Some(cat) = classify_pitch(p.mora_len, pos, can_be_kifuku) {
                if !categories.contains(&cat) {
                    categories.push(cat);
                }
            }
        }
    }
    categories
}

/// Matches `formatPitchCategories` in `notebuilder.cpp`:
/// Formats unique pitch categories as comma-separated string.
pub fn format_pitch_categories(
    pitches: &[PitchCategoryInput<'_>],
    can_be_kifuku: bool,
) -> Vec<u16> {
    let categories = classify_pitch_categories(pitches, can_be_kifuku);
    if categories.is_empty() {
        return Vec::new();
    }
    let mut result = Vec::new();
    for (i, cat) in categories.iter().enumerate() {
        if i > 0 {
            result.push(b',' as u16);
        }
        push_ascii(&mut result, cat.as_str());
    }
    result
}

/// Default SVG dot fill color used by [`format_pitch_graphs_ex`].
pub const DEFAULT_PITCH_GRAPH_FILL: &str = "rgba(0,0,0,0)";

/// Default SVG stroke/dot color used by [`format_pitch_graphs_ex`].
pub const DEFAULT_PITCH_GRAPH_STROKE: &str = "currentColor";

/// Generalized pitch accent specification supporting both numeric downstep
/// indices (`0` = heiban, `1` = atamadaka, `2..=N` = nakadaka/odaka) and exact
/// `H`/`L` pattern strings such as `"LHH"`, `"HLL"`, `"LHHL"`, `"LHL"`
/// (Yomitan PR #2055).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PitchSpec<'a> {
    Downstep(u8),
    Pattern(&'a str),
}

/// Generalized borrowed pitch entry supporting both numeric downstep indices
/// and exact `H`/`L` string pitch patterns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PitchEntryEx<'a> {
    pub dictionary_name: &'a [u16],
    pub specs: &'a [PitchSpec<'a>],
    pub mora: &'a [&'a [u16]],
}

/// Parses a trimmed `H`/`L` pitch pattern string into per-position boolean high
/// flags (`true` = H, `false` = L), returning `None` if empty or containing any
/// non-`H`/`L` character.
fn parse_pitch_pattern(pattern: &str) -> Option<Vec<bool>> {
    let trimmed = pattern.trim();
    if trimmed.is_empty() {
        return None;
    }
    let mut levels = Vec::with_capacity(trimmed.len());
    for b in trimmed.bytes() {
        match b {
            b'H' | b'h' => levels.push(true),
            b'L' | b'l' => levels.push(false),
            _ => return None,
        }
    }
    Some(levels)
}

/// Resolves `spec` over `mora_len` morae into `(mora_highs, particle_high)`.
///
/// For `PitchSpec::Pattern`:
/// - If `pattern.len() > mora_len` (e.g. `"LHHL"` for a 3-mora word), the first
///   `mora_len` characters describe the word's morae and `pattern[mora_len]`
///   describes the following particle (distinguishing odaka `"LHHL"` from
///   heiban `"LHHH"` / `"LHH"`).
/// - If `pattern.len() == mora_len` (e.g. `"LHH"`, `"HLL"`, `"LHL"`), the
///   particle continues at the pitch level of the final mora.
/// - If `pattern.len() < mora_len`, the final pitch level extends across the
///   remaining morae and following particle.
fn resolve_pitch_levels(mora_len: usize, spec: PitchSpec<'_>) -> Option<(Vec<bool>, bool)> {
    if mora_len == 0 {
        return None;
    }
    match spec {
        PitchSpec::Downstep(pos) => {
            let mut highs = vec![false; mora_len];
            let particle_high = match pos {
                0 => {
                    for h in highs.iter_mut().skip(1) {
                        *h = true;
                    }
                    true
                }
                1 => {
                    highs[0] = true;
                    false
                }
                _ => {
                    let limit = (pos as usize).min(mora_len);
                    for h in highs.iter_mut().take(limit).skip(1) {
                        *h = true;
                    }
                    false
                }
            };
            Some((highs, particle_high))
        }
        PitchSpec::Pattern(raw) => {
            let levels = parse_pitch_pattern(raw)?;
            if levels.len() > mora_len {
                let mora_highs = levels[..mora_len].to_vec();
                let particle_high = levels[mora_len];
                Some((mora_highs, particle_high))
            } else {
                let mut mora_highs = Vec::with_capacity(mora_len);
                mora_highs.extend_from_slice(&levels);
                let tail = *levels.last().unwrap();
                mora_highs.resize(mora_len, tail);
                Some((mora_highs, tail))
            }
        }
    }
}

/// Generates an inline SVG pitch-accent graph for a numeric downstep or exact
/// `H`/`L` pitch pattern, matching Memento's SVG pitch graph geometry.
pub fn generate_pitch_graph_svg(
    mora_len: usize,
    spec: PitchSpec<'_>,
    fill: &str,
    stroke: &str,
) -> Option<String> {
    let (mora_highs, particle_high) = resolve_pitch_levels(mora_len, spec)?;
    let mora_i32 = i32::try_from(mora_len).ok()?;
    let width = mora_i32.checked_add(1)?.checked_mul(50)?;

    let mut safe_fill = String::with_capacity(fill.len());
    append_escaped_str(fill, &mut safe_fill);
    let mut safe_stroke = String::with_capacity(stroke.len());
    append_escaped_str(stroke, &mut safe_stroke);

    let mut svg = String::with_capacity(512);
    let _ = write!(
        svg,
        "<svg xmlns=\"http://www.w3.org/2000/svg\" height=\"2em\" viewBox=\"0 0 {width} 100\" style=\"display: inline-block;\">\
         <defs>\
         <g id=\"black-dot\"><circle cx=\"0\" cy=\"0\" r=\"15\" style=\"fill: {safe_stroke}; stroke: {safe_stroke}; stroke-width: 5px;\"/></g>\
         <g id=\"white-dot\"><circle cx=\"0\" cy=\"0\" r=\"15\" style=\"fill: {safe_fill}; stroke: {safe_stroke}; stroke-width: 5px;\"/></g>\
         <g id=\"change-dot\"><circle cx=\"0\" cy=\"0\" r=\"15\" style=\"fill: {safe_fill}; stroke: {safe_stroke}; stroke-width: 5px;\"/>\
         <circle cx=\"0\" cy=\"0\" r=\"5\" style=\"fill: {safe_fill}; stroke: {safe_stroke}; stroke-width: 5px;\"/></g>\
         </defs>"
    );

    let mut lines = String::new();
    let mut dots = String::new();
    let level_at = |idx: usize| -> bool {
        if idx < mora_highs.len() {
            mora_highs[idx]
        } else {
            particle_high
        }
    };

    for i in 0..mora_len {
        let i_i32 = i as i32;
        let x1 = 25 + 50 * i_i32;
        let x2 = 25 + 50 * (i_i32 + 1);
        let h1 = level_at(i);
        let h2 = level_at(i + 1);
        let y1 = if h1 { 25 } else { 75 };
        let y2 = if h2 { 25 } else { 75 };

        let (lx1, ly1, lx2, ly2) = if h1 == h2 {
            (x1 + 15, y1, x2 - 15, y2)
        } else if h1 && !h2 {
            (x1 + 10, y1 + 10, x2 - 10, y2 - 10)
        } else {
            (x1 + 10, y1 - 10, x2 - 10, y2 + 10)
        };

        if !lines.is_empty() {
            lines.push('\n');
        }
        let _ = write!(
            lines,
            "<path d=\"M{lx1} {ly1} L{lx2} {ly2}\" style=\"stroke: {safe_stroke}; stroke-width: 5px;\"/>"
        );

        let dot_id = if h1 && !h2 { "change-dot" } else { "black-dot" };
        if !dots.is_empty() {
            dots.push('\n');
        }
        let _ = write!(dots, "<use href=\"#{dot_id}\" x=\"{x1}\" y=\"{y1}\"/>");
    }

    let particle_x = 25 + 50 * mora_i32;
    let particle_y = if particle_high { 25 } else { 75 };
    if !dots.is_empty() {
        dots.push('\n');
    }
    let _ = write!(
        dots,
        "<use href=\"#white-dot\" x=\"{particle_x}\" y=\"{particle_y}\"/>"
    );

    svg.push_str(&lines);
    svg.push_str(&dots);
    svg.push_str("</svg>");
    Some(svg)
}

fn append_pitch_mora_html(
    mora: &[&[u16]],
    mora_highs: &[bool],
    particle_high: bool,
    out: &mut Vec<u16>,
) {
    let n = mora.len();
    let mut i = 0;
    while i < n {
        if !mora_highs[i] {
            append_escaped(mora[i], out, false);
            i += 1;
            continue;
        }
        let start = i;
        while i < n && mora_highs[i] {
            i += 1;
        }
        let end = i;
        let next_high = if end < n {
            mora_highs[end]
        } else {
            particle_high
        };
        let has_downstep = !next_high;
        if mora[start..end].iter().any(|m| !m.is_empty()) {
            if has_downstep {
                push_ascii(out, "<span style=\"border-top: solid; border-right: solid;\">");
            } else {
                push_ascii(out, "<span style=\"border-top: solid;\">");
            }
            for m in &mora[start..end] {
                append_escaped(m, out, false);
            }
            push_ascii(out, "</span>");
        }
    }
}

/// Formats generalized pitch entries (numeric downsteps and exact `H`/`L`
/// patterns) as overline+downstep HTML, escaping dictionary names and mora.
pub fn format_pitch_ex(pitches: &[PitchEntryEx<'_>]) -> Vec<u16> {
    if pitches.is_empty() {
        return Vec::new();
    }

    let multiple_dicts = pitches.len() > 1;
    let mut pitch = Vec::new();
    push_ascii(&mut pitch, "<span class=\"memento-pitch\">");

    if multiple_dicts {
        push_ascii(&mut pitch, "<ul>");
    }

    for p in pitches {
        let resolved_specs: Vec<(Vec<bool>, bool)> = p
            .specs
            .iter()
            .filter_map(|&spec| resolve_pitch_levels(p.mora.len(), spec))
            .collect();
        let multiple_pitches = resolved_specs.len() > 1;

        if multiple_dicts {
            push_ascii(&mut pitch, "<li><i>");
            append_escaped(p.dictionary_name, &mut pitch, false);
            push_ascii(&mut pitch, "</i>");
        }
        if multiple_pitches {
            push_ascii(&mut pitch, "<ol>");
        }

        for (mora_highs, particle_high) in &resolved_specs {
            if multiple_pitches {
                push_ascii(&mut pitch, "<li style=\"padding: 2px 0px 2px 0px;\">");
            }
            append_pitch_mora_html(p.mora, mora_highs, *particle_high, &mut pitch);
            if multiple_pitches {
                push_ascii(&mut pitch, "</li>");
            }
        }

        if multiple_pitches {
            push_ascii(&mut pitch, "</ol>");
        }
        if multiple_dicts {
            push_ascii(&mut pitch, "</li>");
        }
    }

    if multiple_dicts {
        push_ascii(&mut pitch, "</ul>");
    }
    push_ascii(&mut pitch, "</span>");
    pitch
}

/// Formats generalized pitch positions (`[0]`, `[LHH]`, `[LHHL]`, ...) as HTML,
/// escaping dictionary names.
pub fn format_pitch_positions_ex(pitches: &[PitchEntryEx<'_>]) -> Vec<u16> {
    if pitches.is_empty() {
        return Vec::new();
    }

    let multiple_dicts = pitches.len() > 1;
    let mut pitch_pos = Vec::new();
    push_ascii(&mut pitch_pos, "<span class=\"memento-pitch-position\">");

    if multiple_dicts {
        push_ascii(&mut pitch_pos, "<ul>");
    }

    for p in pitches {
        let valid_specs: Vec<PitchSpec<'_>> = p
            .specs
            .iter()
            .copied()
            .filter(|&spec| resolve_pitch_levels(p.mora.len(), spec).is_some())
            .collect();
        let multiple_pitches = valid_specs.len() > 1;

        if multiple_dicts {
            push_ascii(&mut pitch_pos, "<li><i>");
            append_escaped(p.dictionary_name, &mut pitch_pos, false);
            push_ascii(&mut pitch_pos, "</i>");
        }
        if multiple_pitches {
            push_ascii(&mut pitch_pos, "<ol>");
        }

        for spec in valid_specs {
            if multiple_pitches {
                push_ascii(&mut pitch_pos, "<li>");
            }
            pitch_pos.push(b'[' as u16);
            match spec {
                PitchSpec::Downstep(pos) => append_u8_u16(&mut pitch_pos, pos),
                PitchSpec::Pattern(raw) => {
                    for b in raw.trim().bytes() {
                        pitch_pos.push(b.to_ascii_uppercase() as u16);
                    }
                }
            }
            pitch_pos.push(b']' as u16);
            if multiple_pitches {
                push_ascii(&mut pitch_pos, "</li>");
            }
        }

        if multiple_pitches {
            push_ascii(&mut pitch_pos, "</ol>");
        }
        if multiple_dicts {
            push_ascii(&mut pitch_pos, "</li>");
        }
    }

    if multiple_dicts {
        push_ascii(&mut pitch_pos, "</ul>");
    }
    push_ascii(&mut pitch_pos, "</span>");
    pitch_pos
}

/// Formats generalized pitch entries as inline SVG pitch graphs using explicit
/// `fill` and `stroke` CSS colors, escaping dictionary names.
pub fn format_pitch_graphs_ex_styled(
    pitches: &[PitchEntryEx<'_>],
    fill: &str,
    stroke: &str,
) -> Vec<u16> {
    if pitches.is_empty() {
        return Vec::new();
    }

    let multiple_dicts = pitches.len() > 1;
    let mut pitch_graph = Vec::new();
    push_ascii(&mut pitch_graph, "<span class=\"memento-pitch-graph\">");

    if multiple_dicts {
        push_ascii(&mut pitch_graph, "<ul>");
    }

    for p in pitches {
        let svgs: Vec<String> = p
            .specs
            .iter()
            .filter_map(|&spec| generate_pitch_graph_svg(p.mora.len(), spec, fill, stroke))
            .collect();
        let multiple_pitches = svgs.len() > 1;

        if multiple_dicts {
            push_ascii(&mut pitch_graph, "<li><i>");
            append_escaped(p.dictionary_name, &mut pitch_graph, false);
            push_ascii(&mut pitch_graph, "</i>");
        }
        if multiple_pitches {
            push_ascii(&mut pitch_graph, "<ol>");
        }

        for svg in &svgs {
            if multiple_pitches {
                push_ascii(&mut pitch_graph, "<li>");
            }
            pitch_graph.extend(svg.encode_utf16());
            if multiple_pitches {
                push_ascii(&mut pitch_graph, "</li>");
            }
        }

        if multiple_pitches {
            push_ascii(&mut pitch_graph, "</ol>");
        }
        if multiple_dicts {
            push_ascii(&mut pitch_graph, "</li>");
        }
    }

    if multiple_dicts {
        push_ascii(&mut pitch_graph, "</ul>");
    }
    push_ascii(&mut pitch_graph, "</span>");
    pitch_graph
}

/// Formats generalized pitch entries as inline SVG pitch graphs using Memento's
/// default transparent fill and `currentColor` stroke, escaping dictionary names.
pub fn format_pitch_graphs_ex(pitches: &[PitchEntryEx<'_>]) -> Vec<u16> {
    format_pitch_graphs_ex_styled(pitches, DEFAULT_PITCH_GRAPH_FILL, DEFAULT_PITCH_GRAPH_STROKE)
}

/// Classifies a single [`PitchSpec`] (`Downstep` or exact `H`/`L` `Pattern`)
/// into its [`PitchCategory`], distinguishing heiban (`"LHH"`, `"LHHH"`) from
/// odaka (`"LHHL"` on 3 morae, `"LHL"` on 2 morae).
pub fn classify_pitch_spec(
    mora_len: usize,
    spec: PitchSpec<'_>,
    can_be_kifuku: bool,
) -> Option<PitchCategory> {
    if mora_len == 0 {
        return None;
    }
    match spec {
        PitchSpec::Downstep(pos) => classify_pitch(mora_len, pos, can_be_kifuku),
        PitchSpec::Pattern(_) => {
            let (mora_highs, particle_high) = resolve_pitch_levels(mora_len, spec)?;
            let mut first_downstep: Option<usize> = None;
            for i in 0..mora_len {
                let next_high = if i + 1 < mora_len {
                    mora_highs[i + 1]
                } else {
                    particle_high
                };
                if mora_highs[i] && !next_high {
                    first_downstep = Some(i + 1);
                    break;
                }
            }
            match first_downstep {
                None => Some(PitchCategory::Heiban),
                Some(1) => Some(if can_be_kifuku {
                    PitchCategory::Kifuku
                } else {
                    PitchCategory::Atamadaka
                }),
                Some(pos) => {
                    if pos == mora_len {
                        Some(PitchCategory::Odaka)
                    } else if can_be_kifuku {
                        Some(PitchCategory::Kifuku)
                    } else {
                        Some(PitchCategory::Nakadaka)
                    }
                }
            }
        }
    }
}

/// Returns unique [`PitchCategory`] values in encounter order for generalized
/// pitch entries.
pub fn classify_pitch_categories_ex(
    pitches: &[PitchEntryEx<'_>],
    can_be_kifuku: bool,
) -> Vec<PitchCategory> {
    let mut categories = Vec::with_capacity(5);
    for p in pitches {
        for &spec in p.specs {
            if let Some(cat) = classify_pitch_spec(p.mora.len(), spec, can_be_kifuku) {
                if !categories.contains(&cat) {
                    categories.push(cat);
                }
            }
        }
    }
    categories
}

/// Formats unique pitch categories for generalized pitch entries as a
/// comma-separated UTF-16 string.
pub fn format_pitch_categories_ex(
    pitches: &[PitchEntryEx<'_>],
    can_be_kifuku: bool,
) -> Vec<u16> {
    let categories = classify_pitch_categories_ex(pitches, can_be_kifuku);
    if categories.is_empty() {
        return Vec::new();
    }
    let mut result = Vec::new();
    for (i, cat) in categories.iter().enumerate() {
        if i > 0 {
            result.push(b',' as u16);
        }
        push_ascii(&mut result, cat.as_str());
    }
    result
}

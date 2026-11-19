//! Pitch accent classification, text/position/SVG formatting.

use crate::glossary::escape::{append_escaped, push_ascii};

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
            push_ascii(&mut pitch_pos, &pos.to_string());
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

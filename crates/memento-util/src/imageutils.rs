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

//! Inline SVG pitch-accent graph generation.

use std::fmt::Write;

/// Generates an inline SVG pitch-accent graph directly as UTF-8.
///
/// Returns `None` if `mora_size` overflows the SVG `viewBox` width arithmetic.
pub fn generate_pitch_graph_str(
    mora_size: i32,
    pos: u8,
    fill: &str,
    stroke: &str,
) -> Option<String> {
    let width = mora_size.checked_add(1)?.checked_mul(50)?;
    let mut svg = String::with_capacity(512);
    let _ = write!(
        svg,
        "<svg xmlns=\"http://www.w3.org/2000/svg\" height=\"2em\" viewBox=\"0 0 {width} 100\" style=\"display: inline-block;\">\
         <defs>\
         <g id=\"black-dot\"><circle cx=\"0\" cy=\"0\" r=\"15\" style=\"fill: {stroke}; stroke: {stroke}; stroke-width: 5px;\"/></g>\
         <g id=\"white-dot\"><circle cx=\"0\" cy=\"0\" r=\"15\" style=\"fill: {fill}; stroke: {stroke}; stroke-width: 5px;\"/></g>\
         <g id=\"change-dot\"><circle cx=\"0\" cy=\"0\" r=\"15\" style=\"fill: {fill}; stroke: {stroke}; stroke-width: 5px;\"/>\
         <circle cx=\"0\" cy=\"0\" r=\"5\" style=\"fill: {fill}; stroke: {stroke}; stroke-width: 5px;\"/></g>\
         </defs>"
    );

    let mut lines = String::new();
    let mut dots = String::new();
    let push_line = |buf: &mut String, x1: i32, y1: i32, x2: i32, y2: i32| {
        if !buf.is_empty() {
            buf.push('\n');
        }
        let _ = write!(
            buf,
            "<path d=\"M{x1} {y1} L{x2} {y2}\" style=\"stroke: {stroke}; stroke-width: 5px;\"/>"
        );
    };
    let push_dot = |buf: &mut String, id: &str, x: i32, y: i32| {
        if !buf.is_empty() {
            buf.push('\n');
        }
        let _ = write!(buf, "<use href=\"#{id}\" x=\"{x}\" y=\"{y}\"/>");
    };

    let mut x1 = 25;
    let mut x2 = 75;
    let (mut y1, mut y2) = if pos == 1 { (25, 75) } else { (75, 25) };
    let (initial_y_start, initial_y_end) = if pos == 1 {
        (y1 + 10, y2 - 10)
    } else {
        (y1 - 10, y2 + 10)
    };
    push_line(&mut lines, x1 + 10, initial_y_start, x2 - 10, initial_y_end);
    push_dot(
        &mut dots,
        if pos == 1 { "change-dot" } else { "black-dot" },
        x1,
        y1,
    );

    if pos == 0 || pos == 1 {
        for i in 1..mora_size {
            x1 = x2;
            y1 = y2;
            x2 = 25 + 50 * (i + 1);
            y2 = if pos == 1 { 75 } else { 25 };
            push_line(&mut lines, x1 + 15, y1, x2 - 15, y2);
            push_dot(&mut dots, "black-dot", x1, y1);
        }
    } else {
        let mut i = 1;
        while i < i32::from(pos) - 1 {
            x1 = x2;
            y1 = y2;
            x2 = 25 + 50 * (i + 1);
            y2 = 25;
            push_line(&mut lines, x1 + 15, y1, x2 - 15, y2);
            push_dot(&mut dots, "black-dot", x1, y1);
            i += 1;
        }
        x1 = x2;
        y1 = y2;
        x2 = 25 + 50 * (i + 1);
        y2 = 75;
        push_line(&mut lines, x1 + 10, y1 + 10, x2 - 10, y2 - 10);
        push_dot(&mut dots, "change-dot", x1, y1);
        for i in i32::from(pos)..mora_size {
            x1 = x2;
            y1 = y2;
            x2 = 25 + 50 * (i + 1);
            y2 = 75;
            push_line(&mut lines, x1 + 15, y1, x2 - 15, y2);
            push_dot(&mut dots, "black-dot", x1, y1);
        }
    }
    push_dot(&mut dots, "white-dot", x2, y2);
    svg.push_str(&lines);
    svg.push_str(&dots);
    svg.push_str("</svg>");
    Some(svg)
}

/// Legacy UTF-16 wrapper around [`generate_pitch_graph_str`].
///
/// Preserved for existing UTF-16 callers; new callers should prefer
/// [`generate_pitch_graph_str`] directly.
pub fn generate_pitch_graph(
    mora_size: i32,
    pos: u8,
    fill: &[u16],
    stroke: &[u16],
    _localize: impl Fn(i32) -> Option<Vec<u16>>,
    _unicode_digit: impl Fn(u16) -> Option<u8>,
) -> Option<Vec<u16>> {
    let fill_str = String::from_utf16_lossy(fill);
    let stroke_str = String::from_utf16_lossy(stroke);
    let svg = generate_pitch_graph_str(mora_size, pos, &fill_str, &stroke_str)?;
    Some(svg.encode_utf16().collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generates_heiban_atamadaka_and_nakadaka_svgs() {
        let heiban = generate_pitch_graph_str(3, 0, "rgba(0,0,0,0)", "currentColor").unwrap();
        assert!(heiban.starts_with(
            "<svg xmlns=\"http://www.w3.org/2000/svg\" height=\"2em\" viewBox=\"0 0 200 100\""
        ));
        assert!(heiban.contains("style=\"fill: rgba(0,0,0,0); stroke: currentColor; stroke-width: 5px;\""));
        assert!(heiban.contains("<use href=\"#black-dot\" x=\"25\" y=\"75\"/>"));
        assert!(heiban.contains("<use href=\"#black-dot\" x=\"75\" y=\"25\"/>"));
        assert!(heiban.contains("<use href=\"#black-dot\" x=\"125\" y=\"25\"/>"));
        assert!(heiban.contains("<use href=\"#white-dot\" x=\"175\" y=\"25\"/>"));
        assert!(heiban.ends_with("</svg>"));

        let atamadaka = generate_pitch_graph_str(3, 1, "#fff", "#000").unwrap();
        assert!(atamadaka.contains("<use href=\"#change-dot\" x=\"25\" y=\"25\"/>"));
        assert!(atamadaka.contains("<use href=\"#black-dot\" x=\"75\" y=\"75\"/>"));
        assert!(atamadaka.contains("<use href=\"#white-dot\" x=\"175\" y=\"75\"/>"));

        let nakadaka = generate_pitch_graph_str(3, 2, "#fff", "#000").unwrap();
        assert!(nakadaka.contains("<use href=\"#black-dot\" x=\"25\" y=\"75\"/>"));
        assert!(nakadaka.contains("<use href=\"#change-dot\" x=\"75\" y=\"25\"/>"));
        assert!(nakadaka.contains("<use href=\"#black-dot\" x=\"125\" y=\"75\"/>"));
        assert!(nakadaka.contains("<use href=\"#white-dot\" x=\"175\" y=\"75\"/>"));
    }

    #[test]
    fn utf16_wrapper_matches_utf8_builder_and_checks_overflow() {
        let fill: Vec<u16> = "rgba(0,0,0,0)".encode_utf16().collect();
        let stroke: Vec<u16> = "currentColor".encode_utf16().collect();
        let utf16 = generate_pitch_graph(2, 2, &fill, &stroke, |_| None, |_| None).unwrap();
        let utf8 = generate_pitch_graph_str(2, 2, "rgba(0,0,0,0)", "currentColor").unwrap();
        assert_eq!(String::from_utf16(&utf16).unwrap(), utf8);

        assert!(generate_pitch_graph_str(i32::MAX, 0, "#fff", "#000").is_none());
    }
}

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

//! Pure UTF-16 pitch graph generation and QImage resize decision planning.
//! QString is a sequence of UTF-16 units, including possibly unpaired
//! surrogates; never decode the caller's color strings as UTF-8.

const HEADER: &str = concat!(
    "<svg xmlns=\"http://www.w3.org/2000/svg\" height=\"2em\" viewBox=\"0 0 %1 100\" style=\"display: inline-block;\">",
    "<defs>",
    "<g id=\"black-dot\">",
    "<circle cx=\"0\" cy=\"0\" r=\"15\" style=\"fill: %3; stroke: %3; stroke-width: 5px;\"/>",
    "</g>",
    "<g id=\"white-dot\">",
    "<circle cx=\"0\" cy=\"0\" r=\"15\" style=\"fill: %2; stroke: %3; stroke-width: 5px;\"/>",
    "</g>",
    "<g id=\"change-dot\">",
    "<circle cx=\"0\" cy=\"0\" r=\"15\" style=\"fill: %2; stroke: %3; stroke-width: 5px;\"/>",
    "<circle cx=\"0\" cy=\"0\" r=\"5\" style=\"fill: %2; stroke: %3; stroke-width: 5px;\"/>",
    "</g>",
    "</defs>"
);
const TRAILER: &[u16] = &[b'<' as u16, b'/' as u16, b's' as u16,
                          b'v' as u16, b'g' as u16, b'>' as u16];
const BLACK_DOT: &str = "<use href=\"#black-dot\" x=\"%1\" y=\"%2\"/>";
const WHITE_DOT: &str = "<use href=\"#white-dot\" x=\"%1\" y=\"%2\"/>";
const CHANGE_DOT: &str = "<use href=\"#change-dot\" x=\"%1\" y=\"%2\"/>";
const LINE: &str = "<path d=\"M%2 %3 L%4 %5\" style=\"stroke: %1; stroke-width: 5px;\"/>";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum ResizeOp {
    NoScale = 0,
    ScaleKeepAspect = 1,
    ScaleToWidth = 2,
    ScaleToHeight = 3,
    ScaleIgnoreAspect = 4,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(C)]
pub struct ResizePlan {
    pub target_width: i32,
    pub target_height: i32,
    pub operation: ResizeOp,
}

pub fn resize_valid(max_width: i32, max_height: i32) -> bool {
    (max_width == -1 || max_width > 0) && (max_height == -1 || max_height > 0)
}

/// Mirrors the old decision tree, not Qt's pixel scaling: Qt must still
/// compute aspect-ratio rounding, interpolation and image encoding.
pub fn resize_plan(
    original_width: i32,
    original_height: i32,
    max_width: i32,
    max_height: i32,
    keep_aspect_ratio: bool,
) -> Option<ResizePlan> {
    if !resize_valid(max_width, max_height) || original_width <= 0 || original_height <= 0 {
        return None;
    }
    let target_width = if max_width == -1 { original_width } else { max_width.min(original_width) };
    let target_height = if max_height == -1 { original_height } else { max_height.min(original_height) };
    let operation = if keep_aspect_ratio {
        if max_width != -1 && max_height != -1 {
            ResizeOp::ScaleKeepAspect
        } else if max_width != -1 {
            ResizeOp::ScaleToWidth
        } else if max_height != -1 {
            ResizeOp::ScaleToHeight
        } else {
            ResizeOp::NoScale
        }
    } else if max_width != -1 || max_height != -1 || target_width != original_width || target_height != original_height {
        ResizeOp::ScaleIgnoreAspect
    } else {
        ResizeOp::NoScale
    };
    Some(ResizePlan { target_width, target_height, operation })
}

fn placeholder(
    text: &[u16], i: usize, unicode_digit: &dyn Fn(u16) -> Option<u8>,
) -> Option<(u8, bool, usize)> {
    fn digit(ch: u16, unicode_digit: &dyn Fn(u16) -> Option<u8>) -> Option<u8> {
        if (b'0' as u16..=b'9' as u16).contains(&ch) {
            Some((ch - b'0' as u16) as u8)
        } else if ch >= 128 {
            unicode_digit(ch).filter(|&value| value <= 9)
        } else {
            None
        }
    }
    if text.get(i) != Some(&(b'%' as u16)) {
        return None;
    }
    let localized = text.get(i + 1) == Some(&(b'L' as u16));
    let digit_at = i + 1 + usize::from(localized);
    let mut number = digit(*text.get(digit_at)?, unicode_digit)?;
    let mut length = digit_at + 1 - i;
    if let Some(&next) = text.get(digit_at + 1) {
        if let Some(value) = digit(next, unicode_digit) {
            number = number * 10 + value;
            length += 1;
        }
    }
    Some((number, localized, length))
}

/// Reusable buffers avoid allocating one QString-equivalent for every `.arg`.
/// Each call substitutes *all* occurrences of the lowest-numbered placeholder
/// in the current result, including placeholders inserted by earlier calls.
struct ArgBuffer<'a> {
    text: Vec<u16>,
    scratch: Vec<u16>,
    unicode_digit: &'a dyn Fn(u16) -> Option<u8>,
}

impl<'a> ArgBuffer<'a> {
    fn new(template: &str, unicode_digit: &'a dyn Fn(u16) -> Option<u8>) -> Self {
        Self { text: template.encode_utf16().collect(), scratch: Vec::new(), unicode_digit }
    }

    fn lowest(&self) -> Option<(u8, bool)> {
        let mut lowest = None;
        let mut has_localized = false;
        let mut i = 0;
        while i < self.text.len() {
            if let Some((number, localized, length)) = placeholder(&self.text, i, self.unicode_digit) {
                if lowest.map_or(true, |lowest| number < lowest) {
                    lowest = Some(number);
                    has_localized = localized;
                } else if lowest == Some(number) {
                    has_localized |= localized;
                }
                i += length;
            } else {
                i += 1;
            }
        }
        lowest.map(|number| (number, has_localized))
    }

    fn arg(&mut self, normal: &[u16], localized: &[u16]) -> Option<()> {
        let (lowest, _) = match self.lowest() {
            Some(value) => value,
            None => return Some(()),
        };
        let mut required = self.text.len();
        let mut i = 0;
        while i < self.text.len() {
            if let Some((number, use_locale, length)) = placeholder(&self.text, i, self.unicode_digit) {
                if number == lowest {
                    required = required.checked_sub(length)?
                        .checked_add(if use_locale { localized.len() } else { normal.len() })?;
                    i += length;
                    continue;
                }
            }
            i += 1;
        }
        if required > isize::MAX as usize / std::mem::size_of::<u16>() {
            return None;
        }
        self.scratch.clear();
        self.scratch.try_reserve(required).ok()?;
        let mut i = 0;
        while i < self.text.len() {
            if let Some((number, use_locale, length)) = placeholder(&self.text, i, self.unicode_digit) {
                if number == lowest {
                    self.scratch.extend_from_slice(if use_locale { localized } else { normal });
                    i += length;
                    continue;
                }
            }
            self.scratch.push(self.text[i]);
            i += 1;
        }
        std::mem::swap(&mut self.text, &mut self.scratch);
        Some(())
    }

    fn string(&mut self, value: &[u16]) -> Option<()> {
        self.arg(value, value)
    }

    fn number(&mut self, number: i32, localize: &impl Fn(i32) -> Option<Vec<u16>>) -> Option<()> {
        let Some((_, needs_locale)) = self.lowest() else {
            return Some(());
        };
        let mut digits = [0u16; 11];
        let mut offset = digits.len();
        let mut absolute = i64::from(number).unsigned_abs();
        loop {
            offset -= 1;
            digits[offset] = b'0' as u16 + (absolute % 10) as u16;
            absolute /= 10;
            if absolute == 0 {
                break;
            }
        }
        if number < 0 {
            offset -= 1;
            digits[offset] = b'-' as u16;
        }
        let decimal = &digits[offset..];
        if needs_locale {
            let localized = localize(number)?;
            self.arg(decimal, &localized)
        } else {
            self.arg(decimal, decimal)
        }
    }
}

fn append(buffer: &mut Vec<u16>, value: &[u16]) -> Option<()> {
    let count = buffer.len().checked_add(value.len())?;
    if count > isize::MAX as usize / std::mem::size_of::<u16>() {
        return None;
    }
    buffer.try_reserve(value.len()).ok()?;
    buffer.extend_from_slice(value);
    Some(())
}

fn append_part(buffer: &mut Vec<u16>, item: &[u16]) -> Option<()> {
    if !buffer.is_empty() {
        append(buffer, &[b'\n' as u16])?;
    }
    append(buffer, item)
}

fn line<'a>(
    work: &'a mut ArgBuffer<'_>, template: &[u16],
    x1: i32, y1: i32, x2: i32, y2: i32,
    localize: &impl Fn(i32) -> Option<Vec<u16>>,
) -> Option<&'a [u16]> {
    work.text.clear();
    append(&mut work.text, template)?;
    work.number(x1, localize)?;
    work.number(y1, localize)?;
    work.number(x2, localize)?;
    work.number(y2, localize)?;
    Some(&work.text)
}

fn dot<'a>(
    work: &'a mut ArgBuffer<'_>, template: &str, x: i32, y: i32,
    localize: &impl Fn(i32) -> Option<Vec<u16>>,
) -> Option<&'a [u16]> {
    work.text.clear();
    if template.len() > isize::MAX as usize / std::mem::size_of::<u16>() {
        return None;
    }
    work.text.try_reserve(template.len()).ok()?;
    work.text.extend(template.bytes().map(u16::from));
    work.number(x, localize)?;
    work.number(y, localize)?;
    Some(&work.text)
}

/// Generates the entire legacy graph in UTF-16. `localize` supplies QLocale
/// decimal formatting only for numeric `%L` markers; `unicode_digit` supplies
/// QChar::digitValue only when Qt's compatibility mode permits non-ASCII
/// placeholder digits. Returns None only on overflow or allocation failure.
pub fn generate_pitch_graph(
    mora_size: i32,
    pos: u8,
    fill: &[u16],
    stroke: &[u16],
    localize: impl Fn(i32) -> Option<Vec<u16>>,
    unicode_digit: impl Fn(u16) -> Option<u8>,
) -> Option<Vec<u16>> {
    // The legacy viewBox uses signed 32-bit arithmetic; no arbitrary bound
    // applies below that overflow threshold.
    let width = mora_size.checked_add(1)?.checked_mul(50)?;
    let mut graph = ArgBuffer::new(HEADER, &unicode_digit);
    graph.number(width, &localize)?;
    graph.string(fill)?;
    graph.string(stroke)?;
    let mut line_template = ArgBuffer::new(LINE, &unicode_digit);
    line_template.string(stroke)?;

    let mut lines = Vec::new();
    let mut dots = Vec::new();
    let mut line_work = ArgBuffer {
        text: Vec::new(), scratch: Vec::new(), unicode_digit: &unicode_digit,
    };
    let mut dot_work = ArgBuffer {
        text: Vec::new(), scratch: Vec::new(), unicode_digit: &unicode_digit,
    };
    let mut x1 = 25;
    let mut x2 = 75;
    let (mut y1, mut y2) = if pos == 1 { (25, 75) } else { (75, 25) };
    let initial_y_start = if pos == 1 { y1 + 10 } else { y1 - 10 };
    let initial_y_end = if pos == 1 { y2 - 10 } else { y2 + 10 };
    append_part(&mut lines, line(&mut line_work, &line_template.text,
                                 x1 + 10, initial_y_start, x2 - 10, initial_y_end,
                                 &localize)?)?;
    append_part(&mut dots, dot(&mut dot_work,
                               if pos == 1 { CHANGE_DOT } else { BLACK_DOT },
                               x1, y1, &localize)?)?;

    if pos == 0 || pos == 1 {
        for i in 1..mora_size {
            x1 = x2;
            y1 = y2;
            x2 = 25 + 50 * (i + 1);
            y2 = if pos == 1 { 75 } else { 25 };
            append_part(&mut lines, line(&mut line_work, &line_template.text,
                                         x1 + 15, y1, x2 - 15, y2, &localize)?)?;
            append_part(&mut dots, dot(&mut dot_work, BLACK_DOT, x1, y1, &localize)?)?;
        }
    } else {
        let mut i = 1;
        while i < i32::from(pos) - 1 {
            x1 = x2;
            y1 = y2;
            x2 = 25 + 50 * (i + 1);
            y2 = 25;
            append_part(&mut lines, line(&mut line_work, &line_template.text,
                                         x1 + 15, y1, x2 - 15, y2, &localize)?)?;
            append_part(&mut dots, dot(&mut dot_work, BLACK_DOT, x1, y1, &localize)?)?;
            i += 1;
        }
        x1 = x2;
        y1 = y2;
        x2 = 25 + 50 * (i + 1);
        y2 = 75;
        append_part(&mut lines, line(&mut line_work, &line_template.text,
                                     x1 + 10, y1 + 10, x2 - 10, y2 - 10, &localize)?)?;
        append_part(&mut dots, dot(&mut dot_work, CHANGE_DOT, x1, y1, &localize)?)?;
        for i in i32::from(pos)..mora_size {
            x1 = x2;
            y1 = y2;
            x2 = 25 + 50 * (i + 1);
            y2 = 75;
            append_part(&mut lines, line(&mut line_work, &line_template.text,
                                         x1 + 15, y1, x2 - 15, y2, &localize)?)?;
            append_part(&mut dots, dot(&mut dot_work, BLACK_DOT, x1, y1, &localize)?)?;
        }
    }
    append_part(&mut dots, dot(&mut dot_work, WHITE_DOT, x2, y2, &localize)?)?;
    append(&mut graph.text, &lines)?;
    append(&mut graph.text, &dots)?;
    append(&mut graph.text, TRAILER)?;
    Some(graph.text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arg_replaces_lowest_even_when_inserted_by_color() {
        let digits = |_| None;
        let mut text = ArgBuffer::new("%1 %2 %L2 %3", &digits);
        text.number(5, &|n| Some(format!("[{n}]").encode_utf16().collect())).unwrap();
        text.string(&"%L2😀".encode_utf16().collect::<Vec<_>>()).unwrap();
        text.number(7, &|n| Some(format!("[{n}]").encode_utf16().collect())).unwrap();
        assert_eq!(String::from_utf16(&text.text).unwrap(), "5 [7]😀 [7]😀 %3");
    }

    #[test]
    fn unicode_digits_follow_qt_compatibility_mode() {
        let enabled = |ch| (ch == 0x0661).then_some(1);
        let mut text = ArgBuffer::new("%\u{0661} %1", &enabled);
        text.string(&[b'X' as u16]).unwrap();
        assert_eq!(String::from_utf16(&text.text).unwrap(), "X X");

        let disabled = |_| None;
        let mut text = ArgBuffer::new("%\u{0661} %1", &disabled);
        text.string(&[b'X' as u16]).unwrap();
        assert_eq!(String::from_utf16(&text.text).unwrap(), "%\u{0661} X");
    }

    #[test]
    fn resize_plan_preserves_scaling_even_if_dimensions_are_unchanged() {
        assert_eq!(resize_plan(40, 30, 40, -1, true).unwrap().operation, ResizeOp::ScaleToWidth);
        assert_eq!(resize_plan(40, 30, -1, -1, false).unwrap().operation, ResizeOp::NoScale);
        assert_eq!(resize_plan(40, 30, -1, 5, false).unwrap().target_width, 40);
        assert!(resize_plan(40, 30, 0, 5, false).is_none());
    }
}

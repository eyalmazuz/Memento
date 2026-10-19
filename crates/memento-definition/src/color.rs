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

//! QColor-compatible value type standing in at the integration seam, used by
//! `resolveColorMix` and `cssGradientFallback`
//! (structuredrichtext_css.cpp L660-829).
//!
//! Qt stores each channel as a quint16 (0..=65535) and exposes `redF()` &c. as
//! float quint16/65535. `name(HexArgb)` prints each channel with `>> 8` in
//! lowercase hex; `fromString` parses SVG color names, the documented hex
//! shapes, `transparent`, and (Qt 6.8 CSS parser) CSS function notations.
//!
//! PORT-NOTE (coverage): this port accepts the SVG named color table, hex
//! `#RGB` / `#RRGGBB` / `#AARRGGBB` / `#RRRGGGGBBBB` / `#AAAARRRRGGGGBBBB`,
//! `transparent` (case-insensitive), and CSS `rgb/rgba/hsl/hsla/hsv/hsva`
//! with comma- or space-separated components, optional `/ alpha`, and
//! `%`/`deg` units (matching CSS Color 4 forms). It rejects `hwb`,
//! `oklab`, `oklch`, `color()` that some Qt 6.8+ builds parse; dictionary CSS
//! in use does not rely on them. A small superset is accepted for separators
//! (comma lists may carry a slash alpha).
//!
//! PORT-NOTE (ordering): the name table is a sorted slice searched with
//! binary search — Qt's static table order is never observable.

use crate::qstring::{c_double, is_pcre_space};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Color {
    pub red: u16,
    pub green: u16,
    pub blue: u16,
    pub alpha: u16,
}

impl Color {
    pub fn rgb(red: u8, green: u8, blue: u8) -> Color {
        Color::rgba64(u16::MAX, scale8(red), scale8(green), scale8(blue))
    }

    pub fn rgba64(alpha: u16, red: u16, green: u16, blue: u16) -> Color {
        Color { alpha, red, green, blue }
    }

    /// QColor::redF() & friends: float quint16/65535.
    pub fn red_f(&self) -> f32 {
        f32::from(self.red) / 65535.0
    }

    pub fn green_f(&self) -> f32 {
        f32::from(self.green) / 65535.0
    }

    pub fn blue_f(&self) -> f32 {
        f32::from(self.blue) / 65535.0
    }

    pub fn alpha_f(&self) -> f32 {
        f32::from(self.alpha) / 65535.0
    }

    /// QColor::fromRgbF(float..., spec): clamps and quantizes through
    /// qRound65535f into the quint16 channels.
    pub fn from_rgb_f(red_f: f32, green_f: f32, blue_f: f32, alpha_f: f32) -> Color {
        Color {
            alpha: qround65535(alpha_f),
            red: qround65535(red_f),
            green: qround65535(green_f),
            blue: qround65535(blue_f),
        }
    }

    /// QColor::name(QColor::HexArgb): "#aarrggbb", lowercase, channels >> 8.
    pub fn name_hex_argb(&self) -> String {
        format!(
            "#{:02x}{:02x}{:02x}{:02x}",
            self.alpha >> 8,
            self.red >> 8,
            self.green >> 8,
            self.blue >> 8
        )
    }

    /// QColor::name(QColor::HexRgb): "#rrggbb", lowercase, channels >> 8.
    pub fn name_hex_rgb(&self) -> String {
        format!(
            "#{:02x}{:02x}{:02x}",
            self.red >> 8,
            self.green >> 8,
            self.blue >> 8
        )
    }

    /// QColor(Qt::transparent).
    pub fn transparent() -> Color {
        Color { red: 0, green: 0, blue: 0, alpha: 0 }
    }

    /// QColor::fromString.
    pub fn from_string(name: &str) -> Option<Color> {
        let trimmed = name.trim();
        if trimmed.is_empty() {
            return None;
        }
        if let Some(hex) = trimmed.strip_prefix('#') {
            return parse_hex(hex);
        }
        if trimmed.eq_ignore_ascii_case("transparent") {
            return Some(Color::transparent());
        }
        if let Some(color) = lookup_named(trimmed) {
            return Some(color);
        }
        parse_function_color(trimmed)
    }
}

fn scale8(value: u8) -> u16 {
    // 8-bit channels enter through the exact 257 multiplier.
    u16::from(value) * 257
}

/// Qt's qRound65535f: clamp to [0,1] then truncate v*65535+0.5.
fn qround65535(value: f32) -> u16 {
    if value <= 0.0 {
        0
    } else if value >= 1.0 {
        u16::MAX
    } else {
        let scaled = value * f32::from(u16::MAX) + 0.5;
        scaled as u16
    }
}

fn hex_digit(ch: char) -> Option<u16> {
    ch.to_digit(16).map(|digit| digit as u16)
}

/// Qt's documented hex shapes; alphabetic digits in any case.
///
/// Qt parses every byte-sized channel through `setRgb`-style widening
/// (`c.red = r * 0x101`, i.e. the byte replicated into both halves of the
/// quint16 channel) and, when the shape carries no alpha digits, defaults
/// alpha to opaque (`setRgb`'s default `alpha = 255` → 0xFFFF). Only the
/// 4/8-digit-per-channel shapes enter raw 16-bit values directly.
fn parse_hex(hex: &str) -> Option<Color> {
    let digits: Option<Vec<u16>> = hex.chars().map(hex_digit).collect();
    let digits = digits?;
    /* Byte widened into a quint16 channel: byte << 8 | byte. */
    let widen = |byte: u16| byte * 0x101;
    match digits.len() {
        3 => {
            let expand = |d: u16| d * 0x1111;
            Some(Color {
                alpha: u16::MAX,
                red: expand(digits[0]),
                green: expand(digits[1]),
                blue: expand(digits[2]),
            })
        }
        6 => Some(Color {
            alpha: u16::MAX,
            red: widen((digits[0] << 4) | digits[1]),
            green: widen((digits[2] << 4) | digits[3]),
            blue: widen((digits[4] << 4) | digits[5]),
        }),
        8 => Some(Color {
            alpha: widen((digits[0] << 4) | digits[1]),
            red: widen((digits[2] << 4) | digits[3]),
            green: widen((digits[4] << 4) | digits[5]),
            blue: widen((digits[6] << 4) | digits[7]),
        }),
        12 => Some(Color {
            alpha: u16::MAX,
            red: (digits[0] << 12) | (digits[1] << 8) | (digits[2] << 4) | digits[3],
            green: (digits[4] << 12) | (digits[5] << 8) | (digits[6] << 4) | digits[7],
            blue: (digits[8] << 12) | (digits[9] << 8) | (digits[10] << 4) | digits[11],
        }),
        16 => Some(Color {
            alpha: (digits[0] << 12) | (digits[1] << 8) | (digits[2] << 4) | digits[3],
            red: (digits[4] << 12) | (digits[5] << 8) | (digits[6] << 4) | digits[7],
            green: (digits[8] << 12) | (digits[9] << 8) | (digits[10] << 4) | digits[11],
            blue: (digits[12] << 12) | (digits[13] << 8) | (digits[14] << 4) | digits[15],
        }),
        _ => None,
    }
}

fn lookup_named(name: &str) -> Option<Color> {
    // Names are ASCII in the Qt table; fold exactly as Qt's lookup does.
    if !name.is_ascii() {
        return None;
    }
    let lowered = name.to_ascii_lowercase();
    let position = SORTED_NAMES
        .binary_search_by(|entry| entry.0.cmp(lowered.as_str()))
        .ok()?;
    let (r, g, b) = SORTED_NAMES[position].1;
    Some(Color::rgb(r, g, b))
}

/// The Qt/W3C SVG color name table (including the grey aliases), sorted by
/// name for binary search with the exact-fold comparison above.
const SORTED_NAMES: &[(&str, (u8, u8, u8))] = &[
    ("aliceblue", (0xf0, 0xf8, 0xff)),
    ("antiquewhite", (0xfa, 0xeb, 0xd7)),
    ("aqua", (0x00, 0xff, 0xff)),
    ("aquamarine", (0x7f, 0xff, 0xd4)),
    ("azure", (0xf0, 0xff, 0xff)),
    ("beige", (0xf5, 0xf5, 0xdc)),
    ("bisque", (0xff, 0xe4, 0xc4)),
    ("black", (0x00, 0x00, 0x00)),
    ("blanchedalmond", (0xff, 0xeb, 0xcd)),
    ("blue", (0x00, 0x00, 0xff)),
    ("blueviolet", (0x8a, 0x2b, 0xe2)),
    ("brown", (0xa5, 0x2a, 0x2a)),
    ("burlywood", (0xde, 0xb8, 0x87)),
    ("cadetblue", (0x5f, 0x9e, 0xa0)),
    ("chartreuse", (0x7f, 0xff, 0x00)),
    ("chocolate", (0xd2, 0x69, 0x1e)),
    ("coral", (0xff, 0x7f, 0x50)),
    ("cornflowerblue", (0x64, 0x95, 0xed)),
    ("cornsilk", (0xff, 0xf8, 0xdc)),
    ("crimson", (0xdc, 0x14, 0x3c)),
    ("cyan", (0x00, 0xff, 0xff)),
    ("darkblue", (0x00, 0x00, 0x8b)),
    ("darkcyan", (0x00, 0x8b, 0x8b)),
    ("darkgoldenrod", (0xb8, 0x86, 0x0b)),
    ("darkgray", (0xa9, 0xa9, 0xa9)),
    ("darkgreen", (0x00, 0x64, 0x00)),
    ("darkgrey", (0xa9, 0xa9, 0xa9)),
    ("darkkhaki", (0xbd, 0xb7, 0x6b)),
    ("darkmagenta", (0x8b, 0x00, 0x8b)),
    ("darkolivegreen", (0x55, 0x6b, 0x2f)),
    ("darkorange", (0xff, 0x8c, 0x00)),
    ("darkorchid", (0x99, 0x32, 0xcc)),
    ("darkred", (0x8b, 0x00, 0x00)),
    ("darksalmon", (0xe9, 0x96, 0x7a)),
    ("darkseagreen", (0x8f, 0xbc, 0x8f)),
    ("darkslateblue", (0x48, 0x3d, 0x8b)),
    ("darkslategray", (0x2f, 0x4f, 0x4f)),
    ("darkslategrey", (0x2f, 0x4f, 0x4f)),
    ("darkturquoise", (0x00, 0xce, 0xd1)),
    ("darkviolet", (0x94, 0x00, 0xd3)),
    ("deeppink", (0xff, 0x14, 0x93)),
    ("deepskyblue", (0x00, 0xbf, 0xff)),
    ("dimgray", (0x69, 0x69, 0x69)),
    ("dimgrey", (0x69, 0x69, 0x69)),
    ("dodgerblue", (0x1e, 0x90, 0xff)),
    ("firebrick", (0xb2, 0x22, 0x22)),
    ("floralwhite", (0xff, 0xfa, 0xf0)),
    ("forestgreen", (0x22, 0x8b, 0x22)),
    ("fuchsia", (0xff, 0x00, 0xff)),
    ("gainsboro", (0xdc, 0xdc, 0xdc)),
    ("ghostwhite", (0xf8, 0xf8, 0xff)),
    ("gold", (0xff, 0xd7, 0x00)),
    ("goldenrod", (0xda, 0xa5, 0x20)),
    ("gray", (0x80, 0x80, 0x80)),
    ("green", (0x00, 0x80, 0x00)),
    ("greenyellow", (0xad, 0xff, 0x2f)),
    ("grey", (0x80, 0x80, 0x80)),
    ("honeydew", (0xf0, 0xff, 0xf0)),
    ("hotpink", (0xff, 0x69, 0xb4)),
    ("indianred", (0xcd, 0x5c, 0x5c)),
    ("indigo", (0x4b, 0x00, 0x82)),
    ("ivory", (0xff, 0xff, 0xf0)),
    ("khaki", (0xf0, 0xe6, 0x8c)),
    ("lavender", (0xe6, 0xe6, 0xfa)),
    ("lavenderblush", (0xff, 0xf0, 0xf5)),
    ("lawngreen", (0x7c, 0xfc, 0x00)),
    ("lemonchiffon", (0xff, 0xfa, 0xcd)),
    ("lightblue", (0xad, 0xd8, 0xe6)),
    ("lightcoral", (0xf0, 0x80, 0x80)),
    ("lightcyan", (0xe0, 0xff, 0xff)),
    ("lightgoldenrodyellow", (0xfa, 0xfa, 0xd2)),
    ("lightgray", (0xd3, 0xd3, 0xd3)),
    ("lightgreen", (0x90, 0xee, 0x90)),
    ("lightgrey", (0xd3, 0xd3, 0xd3)),
    ("lightpink", (0xff, 0xb6, 0xc1)),
    ("lightsalmon", (0xff, 0xa0, 0x7a)),
    ("lightseagreen", (0x20, 0xb2, 0xaa)),
    ("lightskyblue", (0x87, 0xce, 0xfa)),
    ("lightslategray", (0x77, 0x88, 0x99)),
    ("lightslategrey", (0x77, 0x88, 0x99)),
    ("lightsteelblue", (0xb0, 0xc4, 0xde)),
    ("lightyellow", (0xff, 0xff, 0xe0)),
    ("lime", (0x00, 0xff, 0x00)),
    ("limegreen", (0x32, 0xcd, 0x32)),
    ("linen", (0xfa, 0xf0, 0xe6)),
    ("magenta", (0xff, 0x00, 0xff)),
    ("maroon", (0x80, 0x00, 0x00)),
    ("mediumaquamarine", (0x66, 0xcd, 0xaa)),
    ("mediumblue", (0x00, 0x00, 0xcd)),
    ("mediumorchid", (0xba, 0x55, 0xd3)),
    ("mediumpurple", (0x93, 0x70, 0xdb)),
    ("mediumseagreen", (0x3c, 0xb3, 0x71)),
    ("mediumslateblue", (0x7b, 0x68, 0xee)),
    ("mediumspringgreen", (0x00, 0xfa, 0x9a)),
    ("mediumturquoise", (0x48, 0xd1, 0xcc)),
    ("mediumvioletred", (0xc7, 0x15, 0x85)),
    ("midnightblue", (0x19, 0x19, 0x70)),
    ("mintcream", (0xf5, 0xff, 0xfa)),
    ("mistyrose", (0xff, 0xe4, 0xe1)),
    ("moccasin", (0xff, 0xe4, 0xb5)),
    ("navajowhite", (0xff, 0xde, 0xad)),
    ("navy", (0x00, 0x00, 0x80)),
    ("oldlace", (0xfd, 0xf5, 0xe6)),
    ("olive", (0x80, 0x80, 0x00)),
    ("olivedrab", (0x6b, 0x8e, 0x23)),
    ("orange", (0xff, 0xa5, 0x00)),
    ("orangered", (0xff, 0x45, 0x00)),
    ("orchid", (0xda, 0x70, 0xd6)),
    ("palegoldenrod", (0xee, 0xe8, 0xaa)),
    ("palegreen", (0x98, 0xfb, 0x98)),
    ("paleturquoise", (0xaf, 0xee, 0xee)),
    ("palevioletred", (0xdb, 0x70, 0x93)),
    ("papayawhip", (0xff, 0xef, 0xd5)),
    ("peachpuff", (0xff, 0xda, 0xb9)),
    ("peru", (0xcd, 0x85, 0x3f)),
    ("pink", (0xff, 0xc0, 0xcb)),
    ("plum", (0xdd, 0xa0, 0xdd)),
    ("powderblue", (0xb0, 0xe0, 0xe6)),
    ("purple", (0x80, 0x00, 0x80)),
    ("rebeccapurple", (0x66, 0x33, 0x99)),
    ("red", (0xff, 0x00, 0x00)),
    ("rosybrown", (0xbc, 0x8f, 0x8f)),
    ("royalblue", (0x41, 0x69, 0xe1)),
    ("saddlebrown", (0x8b, 0x45, 0x13)),
    ("salmon", (0xfa, 0x80, 0x72)),
    ("sandybrown", (0xf4, 0xa4, 0x60)),
    ("seagreen", (0x2e, 0x8b, 0x57)),
    ("seashell", (0xff, 0xf5, 0xee)),
    ("sienna", (0xa0, 0x52, 0x2d)),
    ("silver", (0xc0, 0xc0, 0xc0)),
    ("skyblue", (0x87, 0xce, 0xeb)),
    ("slateblue", (0x6a, 0x5a, 0xcd)),
    ("slategray", (0x70, 0x80, 0x90)),
    ("slategrey", (0x70, 0x80, 0x90)),
    ("snow", (0xff, 0xfa, 0xfa)),
    ("springgreen", (0x00, 0xff, 0x7f)),
    ("steelblue", (0x46, 0x82, 0xb4)),
    ("tan", (0xd2, 0xb4, 0x8c)),
    ("teal", (0x00, 0x80, 0x80)),
    ("thistle", (0xd8, 0xbf, 0xd8)),
    ("tomato", (0xff, 0x63, 0x47)),
    ("turquoise", (0x40, 0xe0, 0xd0)),
    ("violet", (0xee, 0x82, 0xee)),
    ("wheat", (0xf5, 0xde, 0xb3)),
    ("white", (0xff, 0xff, 0xff)),
    ("whitesmoke", (0xf5, 0xf5, 0xf5)),
    ("yellow", (0xff, 0xff, 0x00)),
    ("yellowgreen", (0x9a, 0xcd, 0x32)),
];

// --- CSS function colors (Qt 6.8 CSS color parser subset) --------------------

/// (name in lowercase, '(' text ')') when the text is exactly a function call.
fn split_function_body(text: &str) -> Option<(String, &str)> {
    let open = text.find('(')?;
    if open == 0 {
        return None;
    }
    let name = text[..open].trim();
    if name.is_empty() || !name.chars().all(|ch| ch.is_ascii_alphabetic() || ch == '-') {
        return None;
    }
    if !text.ends_with(')') {
        return None;
    }
    Some((name.to_ascii_lowercase(), &text[open + 1..text.len() - 1]))
}

/// One parsed function component: the numeric value, with the `%` (or `deg`)
/// unit marker still attached as a flag.
#[derive(Clone, Copy)]
struct Component {
    value: f64,
    is_percentage: bool,
}

fn parse_components(text: &str) -> Option<Vec<Component>> {
    let raw = if text.contains(',') {
        text.split(',').collect::<Vec<_>>()
    } else {
        // Space-separated syntax, with an optional `/` before the alpha.
        text.split(|ch: char| is_pcre_space(ch) || ch == '/')
            .filter(|piece| !piece.is_empty())
            .collect::<Vec<_>>()
    };
    let mut components = Vec::with_capacity(raw.len());
    for piece in raw {
        let numbers = piece.trim();
        let (number_part, is_percentage) = if let Some(without) = numbers.strip_suffix('%') {
            (without.trim_end(), true)
        } else if let Some(without) = strip_suffix_case_inv_deg(numbers) {
            (without, false)
        } else {
            (numbers, false)
        };
        let value = c_double(number_part)?;
        components.push(Component { value, is_percentage });
    }
    if components.is_empty() {
        return None;
    }
    Some(components)
}

/// `hsl(120deg)` and friends: a bare `deg` unit suffix on the hue.
fn strip_suffix_case_inv_deg(text: &str) -> Option<&str> {
    if text.len() >= 3 {
        let (head, tail) = text.split_at(text.len() - 3);
        if tail.eq_ignore_ascii_case("deg") {
            return Some(head);
        }
    }
    None
}

fn parse_function_color(text: &str) -> Option<Color> {
    let (name, body) = split_function_body(text)?;
    let components = parse_components(body)?;
    match name.as_str() {
        "rgb" | "rgba" => rgb_color(&components),
        "hsl" | "hsla" => hsl_color(&components),
        "hsv" | "hsva" | "hsb" | "hsba" => hsv_color(&components),
        _ => None,
    }
}

fn channel(value: &Component, scale: f64) -> f32 {
    let fraction = if value.is_percentage {
        value.value / 100.0
    } else {
        clamp01(value.value / scale)
    };
    // QColor::fromRgbF receives floats; Qt converts the double argument to
    // float at the call site.
    fraction.clamp(0.0, 1.0) as f32
}

fn alpha_component(components: &[Component]) -> f32 {
    components
        .get(3)
        .map(|value| {
            let fraction = if value.is_percentage {
                value.value / 100.0
            } else {
                value.value
            };
            clamp01(fraction) as f32
        })
        .unwrap_or(1.0)
}

fn rgb_color(components: &[Component]) -> Option<Color> {
    if !(3..=4).contains(&components.len()) {
        return None;
    }
    Some(Color::from_rgb_f(
        channel(&components[0], 255.0),
        channel(&components[1], 255.0),
        channel(&components[2], 255.0),
        alpha_component(components),
    ))
}

fn hue_component(components: &[Component]) -> f64 {
    // CSS degrees; the `deg` suffix is folded away, percentages (invalid in
    // CSS) are tolerated as raw values like Qt's permissive parser.
    let hue = components[0].value.rem_euclid(360.0);
    hue
}

fn hsl_color(components: &[Component]) -> Option<Color> {
    if !(3..=4).contains(&components.len()) {
        return None;
    }
    let hue = hue_component(components);
    let saturation = component_fraction(&components[1]);
    let lightness = component_fraction(&components[2]);
    let (red, green, blue) = hsl_to_rgb(hue, saturation, lightness);
    Some(Color::from_rgb_f(
        red as f32,
        green as f32,
        blue as f32,
        alpha_component(components),
    ))
}

fn hsv_color(components: &[Component]) -> Option<Color> {
    if !(3..=4).contains(&components.len()) {
        return None;
    }
    let hue = hue_component(components);
    let saturation = component_fraction(&components[1]);
    let brightness = component_fraction(&components[2]);
    let (red, green, blue) = hsv_to_rgb(hue, saturation, brightness);
    Some(Color::from_rgb_f(
        red as f32,
        green as f32,
        blue as f32,
        alpha_component(components),
    ))
}

/// Percentage values divide by 100; plain values clamp directly (0..=1).
fn component_fraction(component: &Component) -> f64 {
    if component.is_percentage {
        component.value / 100.0
    } else {
        component.value
    }
}

fn clamp01(value: f64) -> f64 {
    value.clamp(0.0, 1.0)
}

/// CSS HSL -> RGB (hue degrees normalized, s and l in 0..=1).
fn hsl_to_rgb(h_deg: f64, saturation: f64, lightness: f64) -> (f64, f64, f64) {
    let saturation = clamp01(saturation);
    let lightness = clamp01(lightness);
    let chroma = (1.0 - (2.0 * lightness - 1.0).abs()) * saturation;
    let hue_point = h_deg / 60.0;
    let x = chroma * (1.0 - (hue_point.rem_euclid(2.0) - 1.0).abs());
    let (r1, g1, b1) = match hue_point {
        hue if hue < 1.0 => (chroma, x, 0.0),
        hue if hue < 2.0 => (x, chroma, 0.0),
        hue if hue < 3.0 => (0.0, chroma, x),
        hue if hue < 4.0 => (0.0, x, chroma),
        hue if hue < 5.0 => (x, 0.0, chroma),
        _ => (chroma, 0.0, x),
    };
    let offset = lightness - chroma / 2.0;
    (r1 + offset, g1 + offset, b1 + offset)
}

/// CSS HSV -> RGB.
fn hsv_to_rgb(h_deg: f64, saturation: f64, brightness: f64) -> (f64, f64, f64) {
    let saturation = clamp01(saturation);
    let brightness = clamp01(brightness);
    let chroma = brightness * saturation;
    let hue_point = h_deg / 60.0;
    let x = chroma * (1.0 - (hue_point.rem_euclid(2.0) - 1.0).abs());
    let (r1, g1, b1) = match hue_point {
        hue if hue < 1.0 => (chroma, x, 0.0),
        hue if hue < 2.0 => (x, chroma, 0.0),
        hue if hue < 3.0 => (0.0, chroma, x),
        hue if hue < 4.0 => (0.0, x, chroma),
        hue if hue < 5.0 => (x, 0.0, chroma),
        _ => (chroma, 0.0, x),
    };
    let offset = brightness - chroma;
    (r1 + offset, g1 + offset, b1 + offset)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn named_colors_and_hex_shapes_parse() {
        assert_eq!(Color::from_string("red"), Some(Color::rgb(255, 0, 0)));
        assert_eq!(Color::from_string("RED"), Some(Color::rgb(255, 0, 0)));
        assert_eq!(Color::from_string("transparent"), Some(Color::transparent()));
        assert_eq!(Color::from_string("TRANSPARENT"), Some(Color::transparent()));
        assert_eq!(Color::from_string("notacolor"), None);
        assert_eq!(Color::from_string(""), None);

        let green = Color::from_string("#00FF00").unwrap();
        assert_eq!(green.green, u16::MAX);

        // QColor::fromString("#RRGGBB") is opaque (alpha 65535), so
        // name(HexArgb) shows an alpha layer.
        assert_eq!(
            Color::from_string("#ff0000").map(|c| c.name_hex_argb()),
            Some("#ffff0000".to_string())
        );

        let tinted = Color::from_string("#80ff0000").unwrap();
        // Both channels widen byte * 0x101 (byte replicated into both
        // halves of the quint16).
        assert_eq!(tinted.alpha >> 8, 0x80);
        assert_eq!(tinted.red >> 8, 0xff);

        // #RGB nibbles replicate into every byte.
        assert_eq!(
            Color::from_string("#0ff").map(|c| c.name_hex_rgb()),
            Some("#00ffff".to_string())
        );

        // 12-digit channel shape keeps 16-bit precision exactly.
        assert_eq!(
            Color::from_string("#ffff00000000").map(|c| c.name_hex_rgb()),
            Some("#ff0000".to_string())
        );

        assert_eq!(
            Color::from_string(" rebeccapurple ").map(|c| c.name_hex_rgb()),
            Some("#663399".to_string())
        );
        assert_eq!(
            Color::from_string("grey").map(|c| c.name_hex_rgb()),
            Some("#808080".to_string())
        );
    }

    #[test]
    fn float_channels_round_through_quint16() {
        // 0.5 * 65535 + 0.5 = 32768 -> >> 8 = 0x80 for one channel.
        let color = Color::from_rgb_f(0.0, 0.5, 1.0, 0.5);
        assert_eq!(color.name_hex_argb(), "#800080ff");
    }

    #[test]
    fn hex_argb_names_are_lowercase_channel_wise() {
        let mixed = Color::rgba64(0x4012, 0x3456, 0x789a, 0xbcde);
        assert_eq!(mixed.name_hex_argb(), "#403478bc");
    }

    #[test]
    fn rgb_function_colors_parse() {
        assert_eq!(Color::from_string("rgb(255,0,0)"), Some(Color::rgb(255, 0, 0)));
        assert_eq!(
            Color::from_string("rgb( 100%  0% 0% )"),
            Some(Color::rgb(255, 0, 0))
        );
        let half = Color::from_string("rgba(0, 0, 255, 0.5)").unwrap();
        assert_eq!(half.blue, u16::MAX);
        assert_eq!(half.alpha >> 8, 0x80);
        assert!(Color::from_string("rgb(0,0)").is_none());
        assert!(Color::from_string("rgb(a,b,c)").is_none());
        assert!(Color::from_string("rgb(255,0,0").is_none());
        assert!(Color::from_string("oklch(0.5 0.1 180)").is_none());
    }

    #[test]
    fn hsl_and_hsv_function_colors_parse() {
        // hsl(120deg, 100%, 50%) is lime.
        assert_eq!(
            Color::from_string("hsl(120, 100%, 50%)").map(|c| c.name_hex_rgb()),
            Some("#00ff00".to_string())
        );
        assert_eq!(
            Color::from_string("hsl(0deg, 0%, 100%)").map(|c| c.name_hex_rgb()),
            Some("#ffffff".to_string())
        );
        assert_eq!(
            Color::from_string("hsv(0, 100%, 100%)").map(|c| c.name_hex_rgb()),
            Some("#ff0000".to_string())
        );
        assert!(Color::from_string("hsl(nope, 0%, 0%)").is_none());
        // Negative hues wrap the same way CSS computes them (Qt normalizes
        // hue into [0, 360) with rem_euclid before converting): -240 % 360
        // -> 120deg, i.e. green.
        assert_eq!(
            Color::from_string("hsl(-240, 100%, 50%)").map(|c| c.name_hex_rgb()),
            Some("#00ff00".to_string())
        );
    }
}

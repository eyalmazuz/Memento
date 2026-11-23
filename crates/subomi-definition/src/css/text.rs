//! Idiomatic Rust CSS string, numeric formatting, and CSS Color 4 (`[u8; 4]`)
//! parsing helpers backed by `svgtypes`.

use std::str::FromStr;

/// Parse a finite CSS floating-point literal (rejecting `NaN`/`inf` keywords).
#[inline]
pub fn parse_css_f64(text: &str) -> Option<f64> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    let val: f64 = trimmed.parse().ok()?;
    val.is_finite().then_some(val)
}

/// Format a CSS number cleanly (removing redundant trailing decimal zeros).
pub fn format_css_number(value: f64) -> String {
    if !value.is_finite() {
        return "0".to_owned();
    }
    let abs = value.abs();
    if abs == 0.0 {
        return if value.is_sign_negative() {
            "-0".to_owned()
        } else {
            "0".to_owned()
        };
    }
    let mut s = format!("{value:.6}");
    if s.contains('.') {
        while s.ends_with('0') {
            s.pop();
        }
        if s.ends_with('.') {
            s.pop();
        }
    }
    s
}

/// ASCII case-insensitive prefix strip.
#[inline]
pub fn strip_prefix_ascii_ci<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    if text.len() >= prefix.len()
        && text.is_char_boundary(prefix.len())
        && text[..prefix.len()].eq_ignore_ascii_case(prefix)
    {
        Some(&text[prefix.len()..])
    } else {
        None
    }
}

/// ASCII case-insensitive suffix strip.
#[inline]
pub fn strip_suffix_ascii_ci<'a>(text: &'a str, suffix: &str) -> Option<&'a str> {
    let split = text.len().checked_sub(suffix.len())?;
    if text.is_char_boundary(split) && text[split..].eq_ignore_ascii_case(suffix) {
        Some(&text[..split])
    } else {
        None
    }
}

/// ASCII case-insensitive prefix test.
#[inline]
pub fn starts_with_ascii_ci(text: &str, prefix: &str) -> bool {
    strip_prefix_ascii_ci(text, prefix).is_some()
}

/// Find the first ASCII case-insensitive occurrence of `needle` at or after byte `from`.
pub fn find_ascii_ci(text: &str, needle: &str, from: usize) -> Option<(usize, usize)> {
    if needle.is_empty() {
        return (from <= text.len()).then_some((from, from));
    }
    if from > text.len() || text.len() - from < needle.len() {
        return None;
    }
    let needle_bytes = needle.as_bytes();
    let hay = text.as_bytes();
    let limit = hay.len() - needle_bytes.len();
    for pos in from..=limit {
        if text.is_char_boundary(pos)
            && text.is_char_boundary(pos + needle_bytes.len())
            && hay[pos..pos + needle_bytes.len()].eq_ignore_ascii_case(needle_bytes)
        {
            return Some((pos, pos + needle_bytes.len()));
        }
    }
    None
}

/// ASCII case-insensitive substring test.
#[inline]
pub fn contains_ascii_ci(text: &str, needle: &str) -> bool {
    find_ascii_ci(text, needle, 0).is_some()
}

/// Parse a CSS Color 3/4 string (`#rgb`, `#rgba`, `#rrggbb`, `#rrggbbaa`,
/// CSS/SVG named colors, `transparent`, `rgb()`, `rgba()`, `hsl()`, `hsla()`,
/// including CSS Color 4 space-and-slash alpha notation) into `[r, g, b, a]`.
pub fn parse_css_color(input: &str) -> Option<[u8; 4]> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.eq_ignore_ascii_case("transparent") {
        return Some([0, 0, 0, 0]);
    }
    if trimmed.eq_ignore_ascii_case("rebeccapurple") {
        return Some([102, 51, 153, 255]);
    }
    if let Ok(c) = svgtypes::Color::from_str(trimmed) {
        return Some([c.red, c.green, c.blue, c.alpha]);
    }
    parse_css4_functional_color(trimmed)
}

/// Format an RGBA color as standard CSS lowercase hex (`#rrggbb` when opaque,
/// `#rrggbbaa` when translucent).
pub fn format_css_color([r, g, b, a]: [u8; 4]) -> String {
    if a == 255 {
        format!("#{r:02x}{g:02x}{b:02x}")
    } else {
        format!("#{r:02x}{g:02x}{b:02x}{a:02x}")
    }
}

fn parse_alpha_component(raw: &str) -> Option<u8> {
    let s = raw.trim();
    let ratio = if let Some(pct) = s.strip_suffix('%') {
        parse_css_f64(pct)? / 100.0
    } else {
        parse_css_f64(s)?
    };
    Some((ratio.clamp(0.0, 1.0) * 255.0).round() as u8)
}

fn parse_rgb_component(raw: &str) -> Option<u8> {
    let s = raw.trim();
    let v = if let Some(pct) = s.strip_suffix('%') {
        parse_css_f64(pct)?.clamp(0.0, 100.0) * 2.55
    } else {
        parse_css_f64(s)?.clamp(0.0, 255.0)
    };
    Some(v.round() as u8)
}

fn parse_hue_degrees(raw: &str) -> Option<f64> {
    let s = raw.trim();
    let deg = if let Some(v) = strip_suffix_ascii_ci(s, "deg") {
        parse_css_f64(v)?
    } else if let Some(v) = strip_suffix_ascii_ci(s, "turn") {
        parse_css_f64(v)? * 360.0
    } else if let Some(v) = strip_suffix_ascii_ci(s, "rad") {
        parse_css_f64(v)?.to_degrees()
    } else if let Some(v) = strip_suffix_ascii_ci(s, "grad") {
        parse_css_f64(v)? * 0.9
    } else {
        parse_css_f64(s)?
    };
    Some(deg.rem_euclid(360.0))
}

fn hsl_to_rgb_u8(hue_deg: f64, s: f64, l: f64) -> [u8; 3] {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let h = hue_deg / 60.0;
    let x = c * (1.0 - (h % 2.0 - 1.0).abs());
    let (r1, g1, b1) = if h < 1.0 {
        (c, x, 0.0)
    } else if h < 2.0 {
        (x, c, 0.0)
    } else if h < 3.0 {
        (0.0, c, x)
    } else if h < 4.0 {
        (0.0, x, c)
    } else if h < 5.0 {
        (x, 0.0, c)
    } else {
        (c, 0.0, x)
    };
    let m = l - c * 0.5;
    [
        ((r1 + m).clamp(0.0, 1.0) * 255.0).round() as u8,
        ((g1 + m).clamp(0.0, 1.0) * 255.0).round() as u8,
        ((b1 + m).clamp(0.0, 1.0) * 255.0).round() as u8,
    ]
}

/// Fallback parser for CSS Color 4 functional forms not covered by `svgtypes`
/// (space-separated channels with `/ alpha`, percentage alpha, and `deg`/`turn`/`rad` hues).
fn parse_css4_functional_color(text: &str) -> Option<[u8; 4]> {
    let open = text.find('(')?;
    let inner = text.get(open + 1..text.len().checked_sub(1)?)?;
    if !text.ends_with(')') {
        return None;
    }
    let func = text[..open].trim();

    let (channels_part, alpha_part) = if let Some((lhs, rhs)) = inner.split_once('/') {
        (lhs, Some(rhs))
    } else {
        (inner, None)
    };

    let mut parts: Vec<&str> = if channels_part.contains(',') {
        channels_part.split(',').map(str::trim).filter(|s| !s.is_empty()).collect()
    } else {
        channels_part.split_whitespace().collect()
    };

    let alpha_str = if let Some(a) = alpha_part {
        if parts.len() != 3 {
            return None;
        }
        Some(a.trim())
    } else if parts.len() == 4 {
        Some(parts.pop().unwrap())
    } else if parts.len() == 3 {
        None
    } else {
        return None;
    };

    let a = match alpha_str {
        Some(raw) => parse_alpha_component(raw)?,
        None => 255,
    };

    if func.eq_ignore_ascii_case("rgb") || func.eq_ignore_ascii_case("rgba") {
        let r = parse_rgb_component(parts[0])?;
        let g = parse_rgb_component(parts[1])?;
        let b = parse_rgb_component(parts[2])?;
        Some([r, g, b, a])
    } else if func.eq_ignore_ascii_case("hsl") || func.eq_ignore_ascii_case("hsla") {
        let h = parse_hue_degrees(parts[0])?;
        let s = parse_css_f64(parts[1].strip_suffix('%')?)?.clamp(0.0, 100.0) / 100.0;
        let l = parse_css_f64(parts[2].strip_suffix('%')?)?.clamp(0.0, 100.0) / 100.0;
        let [r, g, b] = hsl_to_rgb_u8(h, s, l);
        Some([r, g, b, a])
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::{format_css_color, parse_css_color};

    #[test]
    fn parses_standard_css_colors_and_css4_alpha() {
        assert_eq!(parse_css_color("red"), Some([255, 0, 0, 255]));
        assert_eq!(parse_css_color("rebeccapurple"), Some([102, 51, 153, 255]));
        assert_eq!(parse_css_color("transparent"), Some([0, 0, 0, 0]));
        assert_eq!(parse_css_color("#0ff"), Some([0, 255, 255, 255]));
        assert_eq!(parse_css_color("#ff000080"), Some([255, 0, 0, 128]));
        assert_eq!(parse_css_color("rgb(255 0 0 / 50%)"), Some([255, 0, 0, 128]));
        assert_eq!(parse_css_color("rgba(0, 0, 255, 0.5)"), Some([0, 0, 255, 128]));
        assert_eq!(parse_css_color("hsl(120deg 100% 50% / 50%)"), Some([0, 255, 0, 128]));
        assert_eq!(format_css_color([255, 0, 0, 255]), "#ff0000");
        assert_eq!(format_css_color([255, 0, 0, 128]), "#ff000080");
    }
}

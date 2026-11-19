//! HTML, CSS, URL, and numeric formatting helpers for Anki glossary building.

use crate::strings::qt_is_space;

const HEX_UPPER: [u16; 16] = [
    b'0' as u16,
    b'1' as u16,
    b'2' as u16,
    b'3' as u16,
    b'4' as u16,
    b'5' as u16,
    b'6' as u16,
    b'7' as u16,
    b'8' as u16,
    b'9' as u16,
    b'A' as u16,
    b'B' as u16,
    b'C' as u16,
    b'D' as u16,
    b'E' as u16,
    b'F' as u16,
];

pub(crate) fn push_ascii(out: &mut Vec<u16>, s: &str) {
    out.extend(s.as_bytes().iter().map(|&b| b as u16));
}

pub(crate) const fn ascii_to_u16<const N: usize>(s: &str) -> [u16; N] {
    let bytes = s.as_bytes();
    let mut out = [0u16; N];
    let mut i = 0;
    while i < N {
        out[i] = bytes[i] as u16;
        i += 1;
    }
    out
}

pub(crate) fn equals_ascii(value: &[u16], expected: &str) -> bool {
    value.len() == expected.len()
        && value
            .iter()
            .zip(expected.bytes())
            .all(|(&unit, byte)| unit == byte as u16)
}

pub(crate) fn equals_ascii_ignore_case(value: &[u16], expected: &str) -> bool {
    value.len() == expected.len()
        && value.iter().zip(expected.bytes()).all(|(&unit, byte)| {
            let lower = if (b'A' as u16..=b'Z' as u16).contains(&unit) {
                unit + (b'a' - b'A') as u16
            } else {
                unit
            };
            lower == byte.to_ascii_lowercase() as u16
        })
}

fn starts_with_ascii_ignore_case(value: &[u16], prefix: &str) -> bool {
    value.len() >= prefix.len() && equals_ascii_ignore_case(&value[..prefix.len()], prefix)
}

/// Appends HTML-escaped UTF-16 code units, matching `QString::toHtmlEscaped`
/// (`<`, `>`, `&`, `"`), optionally converting literal LF to `<br>`.
pub(crate) fn append_escaped(input: &[u16], output: &mut Vec<u16>, newlines_as_breaks: bool) {
    // QString::toHtmlEscaped escapes precisely <, >, &, and ". It leaves
    // apostrophes, lone UTF-16 surrogates, newlines and all other units alone.
    for &unit in input {
        match unit {
            0x3c => output.extend_from_slice(&[0x26, 0x6c, 0x74, 0x3b]), // &lt;
            0x3e => output.extend_from_slice(&[0x26, 0x67, 0x74, 0x3b]), // &gt;
            0x26 => output.extend_from_slice(&[0x26, 0x61, 0x6d, 0x70, 0x3b]), // &amp;
            0x22 => output.extend_from_slice(&[0x26, 0x71, 0x75, 0x6f, 0x74, 0x3b]), // &quot;
            0x0a if newlines_as_breaks => output.extend_from_slice(&[0x3c, 0x62, 0x72, 0x3e]), // <br>
            _ => output.push(unit),
        }
    }
}

/// `GlossaryBuilder::escapeHtml`, i.e. `QString::toHtmlEscaped` over raw
/// QString code units. The replacement is *not* applied again to entities.
pub fn escape_html(input: &[u16]) -> Vec<u16> {
    let mut output = Vec::with_capacity(input.len());
    append_escaped(input, &mut output, false);
    output
}

/// The text path in `buildGlossary`, `addText` and both string variants of
/// `addStructuredContentHelper`: HTML escape first, then replace LF with
/// `<br>` (neither CR nor Unicode line separators become breaks).
pub fn escape_html_lines(input: &[u16]) -> Vec<u16> {
    let mut output = Vec::with_capacity(input.len());
    append_escaped(input, &mut output, true);
    output
}

/// Sanitizes a single CSS property value emitted inside a double-quoted HTML
/// `style="..."` attribute: strips ASCII control chars and `;`/`{`/`}`
/// declaration/block delimiters, converts `"` to `'`, and HTML-escapes `<`,
/// `>`, and `&`.
pub(crate) fn append_css_style_value(input: &[u16], output: &mut Vec<u16>) {
    for &unit in input {
        if unit < 0x20 || unit == 0x7f || matches!(unit, 0x3b | 0x7b | 0x7d) {
            continue;
        }
        match unit {
            0x22 => output.push(0x27),                                   // " -> '
            0x3c => output.extend_from_slice(&[0x26, 0x6c, 0x74, 0x3b]), // &lt;
            0x3e => output.extend_from_slice(&[0x26, 0x67, 0x74, 0x3b]), // &gt;
            0x26 => output.extend_from_slice(&[0x26, 0x61, 0x6d, 0x70, 0x3b]), // &amp;
            _ => output.push(unit),
        }
    }
}

/// Escapes a URL placed inside single-quoted CSS `url('...')` within an HTML
/// `style="..."` attribute so it cannot break out of `url(...)`, quotes, or
/// the surrounding attribute.
pub(crate) fn append_css_url_in_attr(input: &[u16], output: &mut Vec<u16>) {
    for &unit in input {
        if unit < 0x20 || unit == 0x7f {
            continue;
        }
        if matches!(
            unit,
            0x20 | 0x22 | 0x26 | 0x27 | 0x28 | 0x29 | 0x3b | 0x3c | 0x3e | 0x5c
        ) {
            let b = unit as u8;
            output.push(b'%' as u16);
            output.push(HEX_UPPER[(b >> 4) as usize]);
            output.push(HEX_UPPER[(b & 0x0f) as usize]);
        } else {
            output.push(unit);
        }
    }
}

/// Appends CSS inside `<style>...</style>`, escaping any ASCII
/// case-insensitive `</style` sequence to `<\/style` so untrusted dictionary
/// stylesheets cannot close the `<style>` tag and inject HTML/script.
pub(crate) fn append_escaped_style_block(css: &[u16], output: &mut Vec<u16>) {
    let mut i = 0;
    while i < css.len() {
        if starts_with_ascii_ignore_case(&css[i..], "</style") {
            output.push(b'<' as u16);
            output.push(b'\\' as u16);
            output.push(b'/' as u16);
            i += 2;
        } else {
            output.push(css[i]);
            i += 1;
        }
    }
}

/// Allowlist for CSS `image-rendering` keyword values.
pub(crate) fn is_valid_image_rendering(value: &[u16]) -> bool {
    const ALLOWED: [&str; 6] = [
        "auto",
        "pixelated",
        "crisp-edges",
        "smooth",
        "high-quality",
        "-webkit-optimize-contrast",
    ];
    ALLOWED
        .iter()
        .any(|&kw| equals_ascii_ignore_case(value, kw))
}

/// Allowlist for CSS length units used with image `sizeUnits`.
pub(crate) fn is_valid_size_unit(value: &[u16]) -> bool {
    const ALLOWED: [&str; 9] = ["px", "em", "rem", "%", "pt", "ex", "ch", "vh", "vw"];
    ALLOWED
        .iter()
        .any(|&unit| equals_ascii_ignore_case(value, unit))
}

/// Rejects dangerous URI schemes (`javascript:`, `vbscript:`, `data:`) in
/// structured-content `<a href="...">` links.
pub(crate) fn is_safe_href(href: &[u16]) -> bool {
    let trimmed_start = href
        .iter()
        .position(|&u| u > 0x20 && u != 0x7f && !qt_is_space(u))
        .unwrap_or(href.len());
    let rest = &href[trimmed_start..];
    !starts_with_ascii_ignore_case(rest, "javascript:")
        && !starts_with_ascii_ignore_case(rest, "vbscript:")
        && !starts_with_ascii_ignore_case(rest, "data:")
}

/// Format double matching Qt's `QString::number(val)` (format 'g', precision 6).
pub fn format_g(value: f64, precision: u32) -> String {
    if value.is_nan() {
        return "nan".to_string();
    }
    if value.is_infinite() {
        return if value < 0.0 { "-inf" } else { "inf" }.to_string();
    }
    let precision = if precision == 0 { 1 } else { precision };
    if value == 0.0 {
        return "0".to_string();
    }
    let precise = format!("{:.*e}", (precision - 1) as usize, value);
    let pivot = precise.find('e').expect("Rust e-format always has 'e'");
    let mantissa = &precise[..pivot];
    let exponent: i32 = precise[pivot + 1..].parse().expect("valid exponent");
    if exponent < -4 || exponent >= precision as i32 {
        let (whole, fraction) = mantissa.split_once('.').unwrap_or((mantissa, ""));
        let fraction = fraction.trim_end_matches('0');
        let mantissa = if fraction.is_empty() {
            whole.to_string()
        } else {
            format!("{whole}.{fraction}")
        };
        let sign = if exponent < 0 { '-' } else { '+' };
        format!("{mantissa}e{sign}{:02}", exponent.abs())
    } else {
        let decimals = (precision as i32 - 1 - exponent).max(0) as usize;
        let mut formatted = format!("{:.*}", decimals, value);
        if formatted.contains('.') {
            while formatted.ends_with('0') {
                formatted.pop();
            }
            if formatted.ends_with('.') {
                formatted.pop();
            }
        }
        formatted
    }
}

pub fn qstring_number(value: f64) -> Vec<u16> {
    format_g(value, 6).encode_utf16().collect()
}

pub fn qstring_number_i32(value: i32) -> Vec<u16> {
    value.to_string().encode_utf16().collect()
}

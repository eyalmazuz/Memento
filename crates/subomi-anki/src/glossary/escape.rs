//! HTML, CSS, URL, and numeric formatting helpers for Anki glossary building.

use std::fmt::Write as _;

const HEX_UPPER: &[u8; 16] = b"0123456789ABCDEF";

/// Appends HTML-escaped UTF-8 text (`<`, `>`, `&`, `"` -> `&lt;`, `&gt;`,
/// `&amp;`, `&quot;`) into `output`, optionally turning `\n` into `<br>`.
pub(crate) fn append_escaped(input: &str, output: &mut String, newlines_as_breaks: bool) {
    let mut start = 0;
    for (idx, ch) in input.char_indices() {
        let replacement = match ch {
            '<' => "&lt;",
            '>' => "&gt;",
            '&' => "&amp;",
            '"' => "&quot;",
            '\n' if newlines_as_breaks => "<br>",
            _ => continue,
        };
        output.push_str(&input[start..idx]);
        output.push_str(replacement);
        start = idx + ch.len_utf8();
    }
    output.push_str(&input[start..]);
}

/// Appends HTML-escaped UTF-8 text (`<`, `>`, `&`, `"` -> `&lt;`, `&gt;`,
/// `&amp;`, `&quot;`) into `output`.
pub fn append_escaped_str(input: &str, output: &mut String) {
    append_escaped(input, output, false);
}

/// Escapes `<`, `>`, `&`, and `"` in a UTF-8 string into HTML entities.
pub fn escape_html_str(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    append_escaped_str(input, &mut out);
    out
}

/// Sanitizes a single CSS property value emitted inside a double-quoted HTML
/// `style="..."` attribute: strips ASCII control chars and `;`/`{`/`}`
/// declaration/block delimiters, converts `"` to `'`, and HTML-escapes `<`,
/// `>`, and `&`.
pub(crate) fn append_css_style_value(input: &str, output: &mut String) {
    for ch in input.chars() {
        if ch.is_ascii_control() || matches!(ch, ';' | '{' | '}') {
            continue;
        }
        match ch {
            '"' => output.push('\''),
            '<' => output.push_str("&lt;"),
            '>' => output.push_str("&gt;"),
            '&' => output.push_str("&amp;"),
            _ => output.push(ch),
        }
    }
}

/// Escapes a URL placed inside single-quoted CSS `url('...')` within an HTML
/// `style="..."` attribute so it cannot break out of `url(...)`, quotes, or
/// the surrounding attribute.
pub(crate) fn append_css_url_in_attr(input: &str, output: &mut String) {
    for ch in input.chars() {
        if ch.is_ascii_control() {
            continue;
        }
        if matches!(
            ch,
            ' ' | '"' | '&' | '\'' | '(' | ')' | ';' | '<' | '>' | '\\'
        ) {
            let b = ch as u8;
            output.push('%');
            output.push(HEX_UPPER[(b >> 4) as usize] as char);
            output.push(HEX_UPPER[(b & 0x0f) as usize] as char);
        } else {
            output.push(ch);
        }
    }
}

/// Appends CSS inside `<style>...</style>`, escaping any ASCII
/// case-insensitive `</style` sequence to `<\/style` so untrusted dictionary
/// stylesheets cannot close the `<style>` tag and inject HTML/script.
pub(crate) fn append_escaped_style_block(css: &str, output: &mut String) {
    const NEEDLE: &[u8] = b"</style";
    let bytes = css.as_bytes();
    let mut start = 0;
    let mut i = 0;
    while i + NEEDLE.len() <= bytes.len() {
        if bytes[i..i + NEEDLE.len()].eq_ignore_ascii_case(NEEDLE) {
            output.push_str(&css[start..i]);
            output.push_str("<\\/");
            start = i + 2;
            i += 2;
        } else {
            i += 1;
        }
    }
    output.push_str(&css[start..]);
}

/// Allowlist for CSS `image-rendering` keyword values.
pub(crate) fn is_valid_image_rendering(value: &str) -> bool {
    const ALLOWED: [&str; 6] = [
        "auto",
        "pixelated",
        "crisp-edges",
        "smooth",
        "high-quality",
        "-webkit-optimize-contrast",
    ];
    ALLOWED.iter().any(|&kw| value.eq_ignore_ascii_case(kw))
}

/// Allowlist for CSS length units used with image `sizeUnits`.
pub(crate) fn is_valid_size_unit(value: &str) -> bool {
    const ALLOWED: [&str; 9] = ["px", "em", "rem", "%", "pt", "ex", "ch", "vh", "vw"];
    ALLOWED.iter().any(|&unit| value.eq_ignore_ascii_case(unit))
}

/// Rejects dangerous URI schemes (`javascript:`, `vbscript:`, `data:`) in
/// structured-content `<a href="...">` links.
pub(crate) fn is_safe_href(href: &str) -> bool {
    let rest = href.trim_start_matches(|c: char| c.is_ascii_control() || c.is_whitespace());
    let has_prefix = |prefix: &str| {
        rest.get(..prefix.len())
            .is_some_and(|head| head.eq_ignore_ascii_case(prefix))
    };
    !has_prefix("javascript:") && !has_prefix("vbscript:") && !has_prefix("data:")
}

/// Formats a floating-point number in compact general notation (`%g`) with
/// significant-digit precision `precision`, stripping trailing zeroes.
pub fn format_g(value: f64, precision: u32) -> String {
    if value.is_nan() {
        return "nan".to_string();
    }
    if value.is_infinite() {
        return if value < 0.0 { "-inf" } else { "inf" }.to_string();
    }
    let precision = precision.max(1);
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
        let mut out = String::with_capacity(mantissa.len() + 5);
        out.push_str(whole);
        if !fraction.is_empty() {
            out.push('.');
            out.push_str(fraction);
        }
        let sign = if exponent < 0 { '-' } else { '+' };
        let _ = write!(out, "e{sign}{:02}", exponent.abs());
        out
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

/// Appends `format_g(value, 6)` directly into `out`.
pub(crate) fn append_number(out: &mut String, value: f64) {
    out.push_str(&format_g(value, 6));
}

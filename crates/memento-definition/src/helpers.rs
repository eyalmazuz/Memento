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

//! Renderer helpers defined in `structuredrichtext_layout.cpp` (their home is
//! that file's port) that `structuredrichtext_css.cpp` calls:
//! `normalizeListMarker` / `isBuiltInListMarker` (L1346-1435),
//! `cssFontSizeToPixels` (L1447-1653), `screenDpi` (L1675-1690) and
//! `formatPixelSize` (L1692-1703).
//!
//! SEAM: `screenDpi(None)` returns the 96.0 default directly; the C++
//! additionally falls back to `QGuiApplication::primaryScreen()`, which
//! integration provides by supplying [`ScreenMetrics`].

use crate::element::ScreenMetrics;
use crate::qstring::{c_double, is_qchar_digit, to_lower};

/// External pixel-width constants used by the layout port
/// (structuredrichtext_p.h L54-63); re-declared here for the helpers below
/// that are documented against them.
pub const PIXELS_PER_POINT: f64 = 72.0; // POINTS_IN_INCH, layout.cpp L1659
pub const DEFAULT_SCREEN_DPI: f64 = 96.0; // layout.cpp L1678

/// `Renderer::screenDpi` (structuredrichtext_layout.cpp L1675-1690) over the
/// value-struct screen. The primary-screen fallback is provided by
/// integration; `None` yields the default DPI.
pub fn screen_dpi(screen: Option<&ScreenMetrics>) -> f64 {
    match screen {
        Some(metrics) if metrics.logical_dpi > 0.0 => metrics.logical_dpi,
        _ => DEFAULT_SCREEN_DPI,
    }
}

/// `Renderer::formatPixelSize` (structuredrichtext_layout.cpp L1692-1703).
pub fn format_pixel_size(size: f64) -> String {
    let mut formatted = crate::qstring::qstring_number_f(size, 3);
    while formatted.ends_with('0') {
        formatted.pop();
    }
    if formatted.ends_with('.') {
        formatted.pop();
    }
    formatted
}

/// `Renderer::normalizeListMarker` (structuredrichtext_layout.cpp
/// L1346-1357).
///
/// PORT-NOTE: the quote peeling observes UTF-16 code-unit sizes in Qt
/// (`size() >= 2` plus front/back). Both quote marks are BMP, so peeling one
/// leading/trailing `char` is equivalent; the `>= 2` guard distinguishes a
/// non-BMP single character only through its first code unit being a
/// surrogate (never equal to a quote), which `chars()` handles identically.
pub fn normalize_list_marker(marker: &str) -> String {
    let trimmed = crate::qstring::trimmed(marker).replace('"', "'");
    let chars: Vec<char> = trimmed.chars().collect();
    if chars.len() >= 2
        && ((chars[0] == '\'' && chars[chars.len() - 1] == '\'')
            || (chars[0] == '"' && chars[chars.len() - 1] == '"'))
    {
        // The front/back checks already exclude a lone non-BMP character from
        // taking this branch, so char slicing matches the code-unit slicing.
        trimmed[1..trimmed.len() - 1].to_string()
    } else {
        trimmed
    }
}

/// `Renderer::isBuiltInListMarker` (structuredrichtext_layout.cpp
/// L1425-1435).
pub fn is_built_in_list_marker(marker: &str) -> bool {
    let normalized = normalize_list_marker(marker);
    normalized.is_empty()
        || normalized == "disc"
        || normalized == "circle"
        || normalized == "square"
        || normalized == "1"
        || normalized == "a"
        || normalized == "A"
}

/// `Renderer::cssFontSizeToPixels` (structuredrichtext_layout.cpp
/// L1447-1653).
///
/// PORT-NOTE (digit scan): the C++ scans UTF-16 code units while
/// `QChar::isDigit()[Nd]`, `'-'`, `'+'` or `'.'`; the port scans `char`s with
/// [`is_qchar_digit`]. For non-ASCII Nd digits the resulting prefix is
/// rejected by Qt's C-locale double parser either way, so both paths end in
/// the `-1.0` fallthrough.
/// PORT-NOTE (whole-string fast path): `normalized.toDouble(&ok)` accepts an
/// exponent; the explicit scan path below never produces one because the
/// keywords are excluded first and `{digits,+-,.}` scanning stops at `e`.
pub fn css_font_size_to_pixels(
    size: &str,
    screen: Option<&ScreenMetrics>,
    parent_font_pixel_size: f64,
    root_font_pixel_size: f64,
) -> f64 {
    let normalized = to_lower(crate::qstring::trimmed(size));
    if normalized.is_empty() {
        return -1.0;
    }

    if normalized == "inherit" || normalized == "unset" || normalized == "revert"
        || normalized == "revert-layer"
    {
        return parent_font_pixel_size;
    }
    if normalized == "initial" || normalized == "medium" {
        return root_font_pixel_size;
    }
    if normalized == "xx-small" {
        return root_font_pixel_size * 3.0 / 5.0;
    }
    if normalized == "x-small" {
        return root_font_pixel_size * 3.0 / 4.0;
    }
    if normalized == "small" {
        return root_font_pixel_size * 8.0 / 9.0;
    }
    if normalized == "large" {
        return root_font_pixel_size * 6.0 / 5.0;
    }
    if normalized == "x-large" {
        return root_font_pixel_size * 3.0 / 2.0;
    }
    if normalized == "xx-large" {
        return root_font_pixel_size * 2.0;
    }
    if normalized == "xxx-large" {
        return root_font_pixel_size * 3.0;
    }
    if normalized == "smaller" {
        return parent_font_pixel_size / 1.2;
    }
    if normalized == "larger" {
        return parent_font_pixel_size * 1.2;
    }

    // QString::toDouble succeeds on the whole string -> the number spans
    // everything and the (trimmed) remainder is the unit.
    let whole = c_double(&normalized);
    let (_number, unit_start) = if whole.is_some() {
        (whole.unwrap(), normalized.len())
    } else {
        let mut prefix_end = 0usize;
        for (byte_index, ch) in normalized.char_indices() {
            if is_qchar_digit(ch) || ch == '-' || ch == '+' || ch == '.' {
                prefix_end = byte_index + ch.len_utf8();
            } else {
                break;
            }
        }
        if prefix_end == 0 {
            return -1.0;
        }
        (0.0, prefix_end)
    };
    let number_text = &normalized[..unit_start];
    let Some(numeric_value) = c_double(number_text) else {
        return -1.0;
    };
    if !numeric_value.is_finite() {
        return -1.0;
    }

    let unit = crate::qstring::trimmed(&normalized[unit_start..]);
    if unit.is_empty() {
        return if numeric_value == 0.0 { 0.0 } else { -1.0 };
    }
    if unit == "px" {
        return numeric_value;
    }
    if unit == "%" {
        return parent_font_pixel_size * numeric_value / 100.0;
    }
    if unit == "em" {
        return parent_font_pixel_size * numeric_value;
    }
    if unit == "rem" {
        return root_font_pixel_size * numeric_value;
    }
    if unit == "ex" {
        return parent_font_pixel_size * numeric_value / 2.0;
    }
    if unit == "rex" {
        return root_font_pixel_size * numeric_value / 2.0;
    }
    if unit == "ch" {
        return parent_font_pixel_size * numeric_value / 2.0;
    }
    if unit == "rch" {
        return root_font_pixel_size * numeric_value / 2.0;
    }
    if unit == "cap" {
        return parent_font_pixel_size * numeric_value * 0.7;
    }
    if unit == "rcap" {
        return root_font_pixel_size * numeric_value * 0.7;
    }
    if unit == "ic" {
        return parent_font_pixel_size * numeric_value;
    }
    if unit == "ric" {
        return root_font_pixel_size * numeric_value;
    }
    if unit == "lh" {
        return parent_font_pixel_size * numeric_value * 1.2;
    }
    if unit == "rlh" {
        return root_font_pixel_size * numeric_value * 1.2;
    }

    if let Some(metrics) = screen {
        if unit == "vw" || unit == "svw" || unit == "lvw" || unit == "dvw"
            || unit == "vi" || unit == "svi" || unit == "lvi" || unit == "dvi"
        {
            return f64::from(metrics.available_width) * numeric_value / 100.0;
        }
        if unit == "vh" || unit == "svh" || unit == "lvh" || unit == "dvh"
            || unit == "vb" || unit == "svb" || unit == "lvb" || unit == "dvb"
        {
            return f64::from(metrics.available_height) * numeric_value / 100.0;
        }
        if unit == "vmin" || unit == "svmin" || unit == "lvmin" || unit == "dvmin" {
            return f64::from(metrics.available_width.min(metrics.available_height))
                * numeric_value
                / 100.0;
        }
        if unit == "vmax" || unit == "svmax" || unit == "lvmax" || unit == "dvmax" {
            return f64::from(metrics.available_width.max(metrics.available_height))
                * numeric_value
                / 100.0;
        }
    }

    let dpi = screen_dpi(screen);
    if unit == "pt" {
        return numeric_value * dpi / 72.0;
    }
    if unit == "pc" {
        return numeric_value * dpi / 6.0;
    }
    if unit == "in" {
        return numeric_value * dpi;
    }
    if unit == "cm" {
        return numeric_value * dpi / 2.54;
    }
    if unit == "mm" {
        return numeric_value * dpi / 25.4;
    }
    if unit == "q" {
        return numeric_value * dpi / 101.6;
    }

    -1.0
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCREEN: Option<&ScreenMetrics> = Some(&ScreenMetrics {
        available_width: 1000,
        available_height: 500,
        logical_dpi: 96.0,
    });

    #[test]
    fn list_markers_normalize_and_classify() {
        assert_eq!(normalize_list_marker("  \"disc\"  "), "disc");
        assert_eq!(normalize_list_marker("'a b'"), "a b");
        assert_eq!(normalize_list_marker("  1  "), "1");
        assert!(is_built_in_list_marker(""));
        assert!(is_built_in_list_marker("\"disc\""));
        assert!(!is_built_in_list_marker("decimal"));
    }

    #[test]
    fn font_sizes_convert_per_unit() {
        // Values cross-checked against layout.cpp L1447-1653.
        assert_eq!(css_font_size_to_pixels("16px", SCREEN, 12.0, 12.0), 16.0);
        // 12 * 1.5 (parent 12) is the exact double 18.0.
        assert_eq!(css_font_size_to_pixels("1.5em", SCREEN, 12.0, 12.0), 18.0);
        assert_eq!(css_font_size_to_pixels("120%", SCREEN, 12.0, 12.0), 14.4);
        assert_eq!(css_font_size_to_pixels("1.5rem", SCREEN, 12.0, 12.0), 18.0);
        assert_eq!(css_font_size_to_pixels("-12px", SCREEN, 12.0, 12.0), -12.0);
        assert_eq!(css_font_size_to_pixels("+1.5rem", SCREEN, 12.0, 12.0), 18.0);
        assert_eq!(css_font_size_to_pixels("2ex", SCREEN, 12.0, 12.0), 12.0);
        assert_eq!(css_font_size_to_pixels("2ch", SCREEN, 12.0, 12.0), 12.0);
        // C++ L1576: parentFontPixelSize * numericValue * 0.7 evaluated left
        // to right; (12*2)*0.7 rounds to 16.799999999999997, not 16.8.
        assert_eq!(css_font_size_to_pixels("2cap", SCREEN, 12.0, 12.0), 16.799999999999997);
        assert_eq!(css_font_size_to_pixels("2lh", SCREEN, 12.0, 12.0), 12.0 * 2.0 * 1.2);
    }

    #[test]
    fn font_size_keywords_follow_the_root_and_parent() {
        let root = 12.0;
        let parent = 18.0;
        assert_eq!(css_font_size_to_pixels("inherit", SCREEN, parent, root), parent);
        assert_eq!(css_font_size_to_pixels("medium", SCREEN, parent, root), root);
        assert_eq!(css_font_size_to_pixels("small", SCREEN, parent, root), root * 8.0 / 9.0);
        assert_eq!(css_font_size_to_pixels("xx-large", SCREEN, parent, root), root * 2.0);
        assert_eq!(css_font_size_to_pixels("smaller", SCREEN, parent, root), parent / 1.2);
        assert_eq!(css_font_size_to_pixels("larger", SCREEN, parent, root), parent * 1.2);
    }

    #[test]
    fn viewport_units_use_the_screen_geometry() {
        assert_eq!(css_font_size_to_pixels("12vw", SCREEN, 12.0, 12.0), 120.0);
        assert_eq!(css_font_size_to_pixels("12vh", SCREEN, 12.0, 12.0), 60.0);
        assert_eq!(css_font_size_to_pixels("12vmin", SCREEN, 12.0, 12.0), 60.0);
        assert_eq!(css_font_size_to_pixels("12vmax", SCREEN, 12.0, 12.0), 120.0);
        assert_eq!(css_font_size_to_pixels("12vmin", None, 12.0, 12.0), -1.0);
    }

    #[test]
    fn physical_units_use_the_default_dpi() {
        // screen None -> 96.0 dpi fallback (layout.cpp L1675-1690).
        assert_eq!(css_font_size_to_pixels("12pt", None, 12.0, 12.0), 12.0 * 96.0 / 72.0);
        assert_eq!(css_font_size_to_pixels("1in", None, 12.0, 12.0), 96.0);
        assert_eq!(css_font_size_to_pixels("1cm", None, 12.0, 12.0), 96.0 / 2.54);
        // C++ L1645/L1651: numericValue * dpi / 25.4 (and /101.6) evaluated
        // left to right; (25.4*96)/25.4 rounds to 95.99999999999999, not 96.
        assert_eq!(css_font_size_to_pixels("25.4mm", None, 12.0, 12.0), 95.99999999999999);
        assert_eq!(css_font_size_to_pixels("101.6q", None, 12.0, 12.0), 95.99999999999999);
    }

    #[test]
    fn unsupported_font_size_values_fail() {
        assert_eq!(css_font_size_to_pixels("12px ", SCREEN, 12.0, 12.0), 12.0);
        assert_eq!(css_font_size_to_pixels("12", SCREEN, 12.0, 12.0), -1.0);
        assert_eq!(css_font_size_to_pixels("0", SCREEN, 12.0, 12.0), 0.0);
        assert_eq!(css_font_size_to_pixels("", SCREEN, 12.0, 12.0), -1.0);
        assert_eq!(css_font_size_to_pixels("abc", SCREEN, 12.0, 12.0), -1.0);
        assert_eq!(css_font_size_to_pixels("12abc", SCREEN, 12.0, 12.0), -1.0);
        assert_eq!(css_font_size_to_pixels("q", SCREEN, 12.0, 12.0), -1.0);
        assert_eq!(css_font_size_to_pixels(" 1.5rem", SCREEN, 12.0, 12.0), 18.0);
    }

    #[test]
    fn pixel_sizes_format_trimmed() {
        assert_eq!(format_pixel_size(5.0), "5");
        assert_eq!(format_pixel_size(5.125), "5.125");
        assert_eq!(format_pixel_size(-0.0), "-0");
        assert_eq!(format_pixel_size(100.0), "100");
        assert_eq!(format_pixel_size(12.0 * 8.0 / 9.0), "10.667");
        assert_eq!(format_pixel_size(96.0 / 2.54), "37.795");
    }
}

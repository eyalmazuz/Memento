//! Popup theme CSS parser, custom CSS cascade preprocessor, and per-dictionary
//! `StylesheetCache` backed directly by `memento_definition::parse_stylesheet`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use memento_definition::{self as mdef, Color as DefColor, ParsedStylesheet};
pub use memento_definition::CssTheme;
use slint::ComponentHandle;

use super::{MementoApp, PopupBridge};

/// Parse raw UTF-8 CSS text into a `memento_definition::ParsedStylesheet`.
pub fn parse_dictionary_css_str(css: &str) -> Arc<ParsedStylesheet> {
    Arc::new(mdef::parse_stylesheet(css))
}

pub const CUSTOM_POPUP_CSS_FILENAME: &str = "popup.css";
pub const CUSTOM_POPUP_OUTER_CSS_FILENAME: &str = "popup-outer.css";

pub fn default_custom_popup_css_template() -> &'static str {
    r#"/* Memento Popup CSS (~/.config/memento/popup.css)
 *
 * Customize inner typography, pitch headword colors, 2-column card layout,
 * per-dictionary badge/tint colors, hover-to-reveal dictionaries, frequency
 * pill colors, and structured-content glossary rules.
 */

:root {
    --text-color: #F8FAFC;
    --muted-color: #94A3B8;
    --headword-color: #FFFFFF;
    --headword-size: 23px;
    --reading-color: #94A3B8;
    --reading-size: 12px;
    --font-family: "Noto Sans CJK JP";
    --glossary-font-size: 14.5px;
    --link-color: #93C5FD;
    --divider-color: #30373D;
    --term-divider-color: #52616B;

    --freq-bg: #07513E;
    --freq-color: #9CE5C7;
    --freq-font-size: 10px;
    --pitch-color: #B9A1E8;
    --pitch-dict-bg: #6640BE;
    --dict-badge-bg: #6D28D9;
    --dict-badge-color: #EDE9FE;
    --anki-tag-bg: #162B3D;
    --anki-tag-border: #38BDF888;
    --anki-tag-color: #BAE6FD;

    /* Set to 2 (with --popup-width >= 600px in popup-outer.css) for a 2-column card grid */
    --definition-columns: 1;
    --definition-columns-min-width: 560px;
    --definition-gap: 8px;

    /* Clamp long definitions to N lines with a "Show more" toggle (0 or none to disable) */
    /* --collapsible-definition-line-count: 5; */
}

/* Example: Color headwords by pitch accent category
:root {
    --pitch-heiban-color: #59b2ff;
    --pitch-atamadaka-color: #ff6666;
    --pitch-nakadaka-color: #ff9b54;
    --pitch-odaka-color: #67e47d;
    --pitch-kifuku-color: #af85f4;
}
*/

/* Example: Per-dictionary badge color, subtle card tint, collapsible lines & hover-to-reveal JMdict
[data-dictionary="デジタル大辞泉"] {
    --dict-color: rgb(170, 0, 0);
    --dict-bg-opacity: 0.08;
    --collapsible-definition-line-count: 4;
}
[data-dictionary*="JMdict"] {
    --dict-color: rgb(0, 132, 255);
    --dict-bg-opacity: 0.04;
    --glossary-opacity: 0;
    --glossary-hover-opacity: 1;
    --glossary-solo-opacity: 1;
    --collapsible-definition-line-count: none;
}
*/

/* Example: Per-frequency-dictionary pill colors
[data-frequency^="JPDB"] { background-color: #ff3e3d; color: #ffffff; }
[data-frequency="CC100"]   { background-color: #6238fa; color: #ffffff; }
*/

/* Example: Hide UI sections or remove link underlines
.term-tags { display: none; }
.pitch-dict { display: none; }
.kanji-extra { display: none; }
.derivations { display: compact; }
a { text-decoration: none; }
*/
"#
}

pub fn default_custom_popup_outer_css_template() -> &'static str {
    r#"/* Memento Popup Outer CSS (~/.config/memento/popup-outer.css)
 *
 * Controls the floating popup card container: translucent RGBA background,
 * border, corner radius, drop shadow, padding, and optional width/height.
 */

.popup {
    background: #1C2024;
    border: 1px solid #384046;
    border-radius: 8px;
    box-shadow: 0 4px 24px #000000DD;
    padding: 12px;
    /* Uncomment to widen the popup (e.g. for 2-column layout):
    width: 780px;
    height: 520px;
    */
}
"#
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CssAttrMatchOp {
    Exists,
    Equals,
    StartsWith,
    Contains,
    EndsWith,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CssAttrMatcher {
    pub op: CssAttrMatchOp,
    pub value: String,
}

impl CssAttrMatcher {
    pub fn matches(&self, target: &str) -> bool {
        match self.op {
            CssAttrMatchOp::Exists => !target.is_empty(),
            CssAttrMatchOp::Equals => target == self.value,
            CssAttrMatchOp::StartsWith => target.starts_with(&self.value),
            CssAttrMatchOp::Contains => target.contains(&self.value),
            CssAttrMatchOp::EndsWith => target.ends_with(&self.value),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct DictStyleRule {
    pub matcher: CssAttrMatcher,
    pub badge_bg: Option<[u8; 4]>,
    pub badge_color: Option<[u8; 4]>,
    pub bg_opacity: Option<f32>,
    pub card_bg: Option<[u8; 4]>,
    pub card_border: Option<[u8; 4]>,
    pub glossary_opacity: Option<f32>,
    pub glossary_hover_opacity: Option<f32>,
    pub glossary_solo_opacity: Option<f32>,
    pub collapsible_line_count: Option<usize>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FreqStyleRule {
    pub matcher: CssAttrMatcher,
    pub bg: Option<[u8; 4]>,
    pub color: Option<[u8; 4]>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PitchDictStyleRule {
    pub matcher: CssAttrMatcher,
    pub bg: [u8; 4],
}

#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedDictStyle {
    pub badge_bg: [u8; 4],
    pub badge_color: [u8; 4],
    pub card_bg: [u8; 4],
    pub card_border: [u8; 4],
    pub card_border_width: f32,
    pub card_radius: f32,
    pub card_padding: f32,
    pub glossary_opacity: f32,
    pub glossary_hover_opacity: f32,
    pub glossary_solo_opacity: f32,
    pub collapsible_line_count: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct InterfaceFontFallback {
    pub family: String,
    pub size_px: f32,
    pub bold: bool,
    pub italic: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct PopupInterfaceFonts {
    pub expression: Option<InterfaceFontFallback>,
    pub reading: Option<InterfaceFontFallback>,
    pub conj: Option<InterfaceFontFallback>,
    pub tag: Option<InterfaceFontFallback>,
    pub glossary: Option<InterfaceFontFallback>,
    pub kanji: Option<InterfaceFontFallback>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PopupCssExplicitOverrides {
    pub font_family: bool,
    pub glossary_font_size: bool,
    pub headword_font_family: bool,
    pub headword_size: bool,
    pub reading_font_family: bool,
    pub reading_size: bool,
    pub conj_font_family: bool,
    pub conj_font_size: bool,
    pub tag_font_family: bool,
    pub tag_font_size: bool,
    pub kanji_font_family: bool,
    pub kanji_font_size: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PopupCssTheme {
    // Outer popup frame
    pub popup_bg: [u8; 4],
    pub popup_border: [u8; 4],
    pub popup_border_width: f32,
    pub popup_radius: f32,
    pub popup_padding: f32,
    pub popup_shadow_blur: f32,
    pub popup_shadow_color: [u8; 4],
    pub popup_width: f32,
    pub popup_height: f32,

    // Typography & core colors
    pub text_color: [u8; 4],
    pub muted_color: [u8; 4],
    pub headword_color: [u8; 4],
    pub headword_size: f32,
    pub reading_color: [u8; 4],
    pub reading_size: f32,
    pub font_family: String,
    pub headword_font_family: String,
    pub reading_font_family: String,
    pub conj_font_family: String,
    pub conj_font_size: f32,
    pub tag_font_family: String,
    pub tag_font_size: f32,
    pub kanji_font_family: String,
    pub kanji_font_size: f32,
    pub glossary_font_size: f32,
    pub link_color: [u8; 4],
    pub divider_color: [u8; 4],
    pub term_divider_color: [u8; 4],

    // Badges & tags
    pub freq_bg: [u8; 4],
    pub freq_color: [u8; 4],
    pub freq_font_size: f32,
    pub pitch_color: [u8; 4],
    pub pitch_dict_bg: [u8; 4],
    pub dict_badge_bg: [u8; 4],
    pub dict_badge_color: [u8; 4],
    pub anki_tag_bg: [u8; 4],
    pub anki_tag_border: [u8; 4],
    pub anki_tag_color: [u8; 4],

    // Definition layout & cards
    pub definition_columns: i32,
    pub definition_columns_min_width: f32,
    pub definition_gap: f32,
    pub collapsible_definition_line_count: usize,
    pub card_bg: [u8; 4],
    pub card_border: [u8; 4],
    pub card_border_width: f32,
    pub card_radius: f32,
    pub card_padding: f32,

    // Active CSS theme & Yomitan-aligned compact glossary mode (H2 / H5.3)
    pub css_theme: CssTheme,
    pub compact_glossaries: bool,

    // Visibility & UI toggles
    pub show_term_tags: bool,
    pub show_pitch_dict: bool,
    pub show_kanji_extra: bool,
    pub compact_derivations: bool,

    // Headword pitch category colors
    pub pitch_heiban_color: Option<[u8; 4]>,
    pub pitch_atamadaka_color: Option<[u8; 4]>,
    pub pitch_nakadaka_color: Option<[u8; 4]>,
    pub pitch_odaka_color: Option<[u8; 4]>,
    pub pitch_kifuku_color: Option<[u8; 4]>,

    // Per-dictionary / per-frequency / per-pitch-dict dynamic rules
    pub dict_color_opacity: f32,
    pub dict_rules: Vec<DictStyleRule>,
    pub freq_rules: Vec<FreqStyleRule>,
    pub pitch_dict_rules: Vec<PitchDictStyleRule>,
}

impl Default for PopupCssTheme {
    fn default() -> Self {
        Self {
            popup_bg: [0x1C, 0x20, 0x24, 0xFF],
            popup_border: [0x38, 0x40, 0x46, 0xFF],
            popup_border_width: 1.0,
            popup_radius: 8.0,
            popup_padding: 12.0,
            popup_shadow_blur: 24.0,
            popup_shadow_color: [0x00, 0x00, 0x00, 0xDD],
            popup_width: 0.0,
            popup_height: 0.0,
            text_color: [0xF8, 0xFA, 0xFC, 0xFF],
            muted_color: [0x94, 0xA3, 0xB8, 0xFF],
            headword_color: [0xFF, 0xFF, 0xFF, 0xFF],
            headword_size: 23.0,
            reading_color: [0x94, 0xA3, 0xB8, 0xFF],
            reading_size: 12.0,
            font_family: "Noto Sans CJK JP".to_owned(),
            headword_font_family: String::new(),
            reading_font_family: String::new(),
            conj_font_family: String::new(),
            conj_font_size: 11.0,
            tag_font_family: String::new(),
            tag_font_size: 11.0,
            kanji_font_family: String::new(),
            kanji_font_size: 0.0,
            glossary_font_size: 14.5,
            link_color: [0x93, 0xC5, 0xFD, 0xFF],
            divider_color: [0x30, 0x37, 0x3D, 0xFF],
            term_divider_color: [0x52, 0x61, 0x6B, 0xFF],
            freq_bg: [0x07, 0x51, 0x3E, 0xFF],
            freq_color: [0x9C, 0xE5, 0xC7, 0xFF],
            freq_font_size: 10.0,
            pitch_color: [0xB9, 0xA1, 0xE8, 0xFF],
            pitch_dict_bg: [0x66, 0x40, 0xBE, 0xFF],
            dict_badge_bg: [0x6D, 0x28, 0xD9, 0xFF],
            dict_badge_color: [0xED, 0xE9, 0xFE, 0xFF],
            anki_tag_bg: [0x16, 0x2B, 0x3D, 0xFF],
            anki_tag_border: [0x38, 0xBD, 0xF8, 0x88],
            anki_tag_color: [0xBA, 0xE6, 0xFD, 0xFF],
            definition_columns: 1,
            definition_columns_min_width: 560.0,
            definition_gap: 8.0,
            collapsible_definition_line_count: 0,
            card_bg: [0, 0, 0, 0],
            card_border: [0, 0, 0, 0],
            card_border_width: 0.0,
            card_radius: 0.0,
            card_padding: 0.0,
            css_theme: CssTheme::Dark,
            compact_glossaries: false,
            show_term_tags: true,
            show_pitch_dict: true,
            show_kanji_extra: true,
            compact_derivations: false,
            pitch_heiban_color: None,
            pitch_atamadaka_color: None,
            pitch_nakadaka_color: None,
            pitch_odaka_color: None,
            pitch_kifuku_color: None,
            dict_color_opacity: 1.0,
            dict_rules: Vec::new(),
            freq_rules: Vec::new(),
            pitch_dict_rules: Vec::new(),
        }
    }
}

impl PopupCssTheme {
    /// Built-in light-theme palette defaults (`#357`).
    pub fn light_default() -> Self {
        Self {
            popup_bg: [0xFF, 0xFF, 0xFF, 0xFF],
            popup_border: [0xD0, 0xD7, 0xDE, 0xFF],
            popup_shadow_color: [0x00, 0x00, 0x00, 0x33],
            text_color: [0x1C, 0x20, 0x24, 0xFF],
            muted_color: [0x57, 0x60, 0x6A, 0xFF],
            headword_color: [0x11, 0x18, 0x27, 0xFF],
            reading_color: [0x57, 0x60, 0x6A, 0xFF],
            link_color: [0x09, 0x69, 0xDA, 0xFF],
            divider_color: [0xD0, 0xD7, 0xDE, 0xFF],
            term_divider_color: [0xAF, 0xB8, 0xC1, 0xFF],
            freq_bg: [0xDA, 0xFB, 0xE1, 0xFF],
            freq_color: [0x11, 0x63, 0x29, 0xFF],
            pitch_color: [0x66, 0x39, 0xBA, 0xFF],
            pitch_dict_bg: [0x66, 0x40, 0xBE, 0xFF],
            dict_badge_bg: [0x6D, 0x28, 0xD9, 0xFF],
            dict_badge_color: [0xFF, 0xFF, 0xFF, 0xFF],
            anki_tag_bg: [0xDD, 0xF4, 0xFF, 0xFF],
            anki_tag_border: [0x54, 0xAE, 0xFF, 0x99],
            anki_tag_color: [0x09, 0x69, 0xDA, 0xFF],
            css_theme: CssTheme::Light,
            ..Self::default()
        }
    }

    /// Returns default popup styling for `css_theme` (`Dark` or `Light`).
    pub fn default_for_theme(css_theme: CssTheme) -> Self {
        match css_theme {
            CssTheme::Dark => Self::default(),
            CssTheme::Light => Self::light_default(),
        }
    }

    /// Returns `true` when this popup theme uses the light color scheme.
    pub fn is_light_theme(&self) -> bool {
        self.css_theme == CssTheme::Light
    }

    /// Theme & compact-layout cache discriminant so rendered glossary caches invalidate
    /// immediately on dark/light theme or compact-glossaries toggling.
    #[allow(dead_code)]
    pub fn render_cache_key(&self) -> (CssTheme, bool) {
        (self.css_theme, self.compact_glossaries)
    }

    pub fn with_interface_font_fallbacks(
        &self,
        overrides: &PopupCssExplicitOverrides,
        fonts: &PopupInterfaceFonts,
    ) -> Self {
        let mut out = self.clone();
        if let Some(ref f) = fonts.glossary {
            if !overrides.font_family {
                out.font_family = f.family.clone();
            }
            if !overrides.glossary_font_size {
                out.glossary_font_size = f.size_px;
            }
        }
        if let Some(ref f) = fonts.expression {
            if !overrides.headword_font_family && !overrides.font_family {
                out.headword_font_family = f.family.clone();
            }
            if !overrides.headword_size {
                out.headword_size = f.size_px;
            }
        }
        if let Some(ref f) = fonts.reading {
            if !overrides.reading_font_family && !overrides.font_family {
                out.reading_font_family = f.family.clone();
            }
            if !overrides.reading_size {
                out.reading_size = f.size_px;
            }
        }
        if let Some(ref f) = fonts.conj {
            if !overrides.conj_font_family && !overrides.font_family {
                out.conj_font_family = f.family.clone();
            }
            if !overrides.conj_font_size {
                out.conj_font_size = f.size_px;
            }
        }
        if let Some(ref f) = fonts.tag {
            if !overrides.tag_font_family && !overrides.font_family {
                out.tag_font_family = f.family.clone();
            }
            if !overrides.tag_font_size {
                out.tag_font_size = f.size_px;
            }
        }
        if let Some(ref f) = fonts.kanji {
            if !overrides.kanji_font_family && !overrides.font_family {
                out.kanji_font_family = f.family.clone();
            }
            if !overrides.kanji_font_size {
                out.kanji_font_size = f.size_px;
            }
        }
        if out.headword_font_family.is_empty() {
            out.headword_font_family = out.font_family.clone();
        }
        if out.reading_font_family.is_empty() {
            out.reading_font_family = out.font_family.clone();
        }
        if out.conj_font_family.is_empty() {
            out.conj_font_family = out.font_family.clone();
        }
        if out.tag_font_family.is_empty() {
            out.tag_font_family = out.font_family.clone();
        }
        if out.kanji_font_family.is_empty() {
            out.kanji_font_family = out.font_family.clone();
        }
        out
    }

    pub fn resolve_headword_pitch_color(&self, category: Option<&str>) -> [u8; 4] {
        match category {
            Some("heiban") => self.pitch_heiban_color.unwrap_or(self.headword_color),
            Some("atamadaka") => self.pitch_atamadaka_color.unwrap_or(self.headword_color),
            Some("nakadaka") => self.pitch_nakadaka_color.unwrap_or(self.headword_color),
            Some("odaka") => self.pitch_odaka_color.unwrap_or(self.headword_color),
            Some("kifuku") => self.pitch_kifuku_color.unwrap_or(self.headword_color),
            _ => self.headword_color,
        }
    }

    pub fn resolve_dict_style(&self, dict_name: &str) -> ResolvedDictStyle {
        let mut badge_bg = self.dict_badge_bg;
        let mut badge_color = self.dict_badge_color;
        let mut card_bg = self.card_bg;
        let mut card_border = self.card_border;
        let mut bg_opacity = 0.0f32;
        let mut explicit_card_bg = false;
        let mut glossary_opacity = 1.0f32;
        let mut glossary_hover_opacity = 1.0f32;
        let mut glossary_solo_opacity = 1.0f32;
        let mut collapsible_line_count = self.collapsible_definition_line_count;

        for rule in &self.dict_rules {
            if !rule.matcher.matches(dict_name) {
                continue;
            }
            if let Some(c) = rule.badge_bg {
                badge_bg = c;
            }
            if let Some(c) = rule.badge_color {
                badge_color = c;
            }
            if let Some(op) = rule.bg_opacity {
                bg_opacity = op;
            }
            if let Some(c) = rule.card_bg {
                card_bg = c;
                explicit_card_bg = true;
            }
            if let Some(c) = rule.card_border {
                card_border = c;
            }
            if let Some(o) = rule.glossary_opacity {
                glossary_opacity = o;
            }
            if let Some(o) = rule.glossary_hover_opacity {
                glossary_hover_opacity = o;
            }
            if let Some(o) = rule.glossary_solo_opacity {
                glossary_solo_opacity = o;
            }
            if let Some(lines) = rule.collapsible_line_count {
                collapsible_line_count = lines;
            }
        }

        // Blend subtle per-dictionary tint when `--dict-bg-opacity` > 0 and card_bg wasn't overridden.
        let tint_alpha = (bg_opacity * self.dict_color_opacity).clamp(0.0, 1.0);
        if !explicit_card_bg && tint_alpha > 0.001 {
            if card_bg[3] == 0 {
                // Translucent overlay tint so glassy outer popup backgrounds remain translucent!
                card_bg = [
                    badge_bg[0],
                    badge_bg[1],
                    badge_bg[2],
                    (tint_alpha * 255.0).round().clamp(0.0, 255.0) as u8,
                ];
            } else {
                let mix = |a: u8, b: u8| -> u8 {
                    (f32::from(a) * (1.0 - tint_alpha) + f32::from(b) * tint_alpha)
                        .round()
                        .clamp(0.0, 255.0) as u8
                };
                card_bg = [
                    mix(card_bg[0], badge_bg[0]),
                    mix(card_bg[1], badge_bg[1]),
                    mix(card_bg[2], badge_bg[2]),
                    card_bg[3],
                ];
            }
        }

        ResolvedDictStyle {
            badge_bg,
            badge_color,
            card_bg,
            card_border,
            card_border_width: self.card_border_width,
            card_radius: self.card_radius,
            card_padding: self.card_padding,
            glossary_opacity,
            glossary_hover_opacity,
            glossary_solo_opacity,
            collapsible_line_count,
        }
    }

    pub fn resolve_freq_style(&self, freq_dict: &str) -> ([u8; 4], [u8; 4]) {
        let mut bg = self.freq_bg;
        let mut color = self.freq_color;
        for rule in &self.freq_rules {
            if !rule.matcher.matches(freq_dict) {
                continue;
            }
            if let Some(c) = rule.bg {
                bg = c;
            }
            if let Some(c) = rule.color {
                color = c;
            }
        }
        (bg, color)
    }

    pub fn resolve_pitch_dict_bg(&self, pitch_dict: &str) -> [u8; 4] {
        let mut bg = self.pitch_dict_bg;
        for rule in &self.pitch_dict_rules {
            if rule.matcher.matches(pitch_dict) {
                bg = rule.bg;
            }
        }
        bg
    }
}

fn strip_css_comments(css: &str) -> String {
    let mut out = String::with_capacity(css.len());
    let bytes = css.as_bytes();
    let mut i = 0usize;
    let mut in_quote: Option<u8> = None;
    while i < bytes.len() {
        let b = bytes[i];
        if let Some(q) = in_quote {
            if b == b'\\' && i + 1 < bytes.len() {
                out.push(b as char);
                let next_ch = css[i + 1..].chars().next().unwrap();
                out.push(next_ch);
                i += 1 + next_ch.len_utf8();
                continue;
            }
            if b == q {
                in_quote = None;
            }
            let ch = css[i..].chars().next().unwrap();
            out.push(ch);
            i += ch.len_utf8();
            continue;
        }
        if b == b'"' || b == b'\'' {
            in_quote = Some(b);
            out.push(b as char);
            i += 1;
            continue;
        }
        if b == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'*' {
            i += 2;
            while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                i += 1;
            }
            if i + 1 < bytes.len() {
                i += 2;
            } else {
                break;
            }
            continue;
        }
        // Strip JS-style `// ...` comments while preserving URLs like `https://` / `memento://`.
        if b == b'/'
            && i + 1 < bytes.len()
            && bytes[i + 1] == b'/'
            && (i == 0 || bytes[i - 1] != b':')
        {
            i += 2;
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        let ch = css[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

pub(super) fn clean_css_font_family(raw: &str) -> String {
    let first = raw.split(',').next().unwrap_or("").trim();
    let unquoted = first
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .or_else(|| first.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')))
        .unwrap_or(first)
        .trim();
    unquoted.to_owned()
}

pub(super) fn parse_css_theme_color(value: &str) -> Option<[u8; 4]> {
    let v = value.trim();
    if v.is_empty() {
        return None;
    }
    if v.eq_ignore_ascii_case("transparent") {
        return Some([0, 0, 0, 0]);
    }
    // Standard CSS #RGBA and #RRGGBBAA (DefColor::from_string uses Qt #AARRGGBB for 8-digit hex).
    if let Some(hex) = v.strip_prefix('#') {
        if hex.len() == 4 && hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            let nibble =
                |idx: usize| u8::from_str_radix(&hex[idx..idx + 1], 16).ok().map(|n| n * 17);
            return Some([nibble(0)?, nibble(1)?, nibble(2)?, nibble(3)?]);
        }
        if hex.len() == 8 && hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            let byte = |idx: usize| u8::from_str_radix(&hex[idx..idx + 2], 16).ok();
            return Some([byte(0)?, byte(2)?, byte(4)?, byte(6)?]);
        }
    }
    DefColor::from_string(v).map(|c| {
        [
            ((u32::from(c.red) + 128) / 257) as u8,
            ((u32::from(c.green) + 128) / 257) as u8,
            ((u32::from(c.blue) + 128) / 257) as u8,
            ((u32::from(c.alpha) + 128) / 257) as u8,
        ]
    })
}

fn parse_css_length_px(value: &str, base_px: f32) -> Option<f32> {
    let lower = value.trim().to_ascii_lowercase();
    if lower.is_empty() {
        return None;
    }
    let parsed = if lower == "0" {
        Some(0.0)
    } else if let Some(num) = lower.strip_suffix("px") {
        num.trim().parse::<f32>().ok()
    } else if let Some(num) = lower.strip_suffix("rem") {
        num.trim().parse::<f32>().ok().map(|n| n * base_px)
    } else if let Some(num) = lower.strip_suffix("em") {
        num.trim().parse::<f32>().ok().map(|n| n * base_px)
    } else if let Some(num) = lower.strip_suffix("pt") {
        num.trim().parse::<f32>().ok().map(|n| n * (96.0 / 72.0))
    } else if let Some(num) = lower.strip_suffix('%') {
        num.trim().parse::<f32>().ok().map(|n| base_px * (n / 100.0))
    } else {
        None
    }?;
    (parsed.is_finite() && parsed >= 0.0).then_some(parsed)
}

pub(super) fn parse_css_unit_float(value: &str) -> Option<f32> {
    let t = value.trim();
    if let Some(pct) = t.strip_suffix('%') {
        pct.trim()
            .parse::<f32>()
            .ok()
            .filter(|v| v.is_finite())
            .map(|v| (v / 100.0).clamp(0.0, 1.0))
    } else {
        t.parse::<f32>()
            .ok()
            .filter(|v| v.is_finite())
            .map(|v| v.clamp(0.0, 1.0))
    }
}

fn parse_css_border_shorthand(value: &str, base_px: f32) -> (Option<f32>, Option<[u8; 4]>) {
    let mut width = None;
    let mut color = None;
    for tok in mdef::split_css_tokens(value) {
        let lower = tok.to_ascii_lowercase();
        if lower == "none" {
            width = Some(0.0);
            continue;
        }
        if matches!(
            lower.as_str(),
            "solid" | "dashed" | "dotted" | "double" | "groove" | "ridge" | "inset" | "outset"
        ) {
            continue;
        }
        if width.is_none() {
            if let Some(w) = parse_css_length_px(&tok, base_px) {
                width = Some(w);
                continue;
            }
        }
        if color.is_none() {
            if let Some(c) = parse_css_theme_color(&tok) {
                color = Some(c);
            }
        }
    }
    (width, color)
}

fn parse_css_box_shadow(value: &str, base_px: f32) -> (Option<f32>, Option<[u8; 4]>) {
    let trimmed = value.trim();
    if trimmed.eq_ignore_ascii_case("none") {
        return (Some(0.0), Some([0, 0, 0, 0]));
    }
    // Use first shadow if comma-separated (ignoring commas inside rgba(...))
    let mut first_end = trimmed.len();
    let mut depth = 0i32;
    for (idx, ch) in trimmed.char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => depth = (depth - 1).max(0),
            ',' if depth == 0 => {
                first_end = idx;
                break;
            }
            _ => {}
        }
    }
    let first = &trimmed[..first_end];
    let mut lengths = Vec::new();
    let mut color = None;
    for tok in mdef::split_css_tokens(first) {
        if tok.eq_ignore_ascii_case("inset") {
            continue;
        }
        if let Some(px) = parse_css_length_px(&tok, base_px) {
            lengths.push(px);
        } else if color.is_none() {
            if let Some(c) = parse_css_theme_color(&tok) {
                color = Some(c);
            }
        }
    }
    let blur = lengths.get(2).copied().or_else(|| lengths.first().copied());
    (blur, color)
}

fn parse_attr_matcher(selector: &str, attr_name: &str) -> Option<CssAttrMatcher> {
    let mut rest = selector;
    while let Some(open) = rest.find('[') {
        let after = &rest[open + 1..];
        let close = after.find(']')?;
        let inner = after[..close].trim();
        rest = &after[close + 1..];

        // Check if attribute name matches (case-insensitive)
        let (name_part, op_and_val) = if let Some(idx) = inner.find('=') {
            let prefix_len =
                if idx > 0 && matches!(inner.as_bytes()[idx - 1], b'^' | b'*' | b'$') {
                    idx - 1
                } else {
                    idx
                };
            (inner[..prefix_len].trim(), Some(&inner[prefix_len..]))
        } else {
            (inner.trim(), None)
        };

        if !name_part.eq_ignore_ascii_case(attr_name) {
            continue;
        }
        let Some(rhs) = op_and_val else {
            return Some(CssAttrMatcher {
                op: CssAttrMatchOp::Exists,
                value: String::new(),
            });
        };
        let (op, raw_val) = if let Some(v) = rhs.strip_prefix("^=") {
            (CssAttrMatchOp::StartsWith, v.trim())
        } else if let Some(v) = rhs.strip_prefix("*=") {
            (CssAttrMatchOp::Contains, v.trim())
        } else if let Some(v) = rhs.strip_prefix("$=") {
            (CssAttrMatchOp::EndsWith, v.trim())
        } else if let Some(v) = rhs.strip_prefix('=') {
            (CssAttrMatchOp::Equals, v.trim())
        } else {
            continue;
        };
        let unquoted = raw_val
            .strip_prefix('"')
            .and_then(|s| s.strip_suffix('"'))
            .or_else(|| raw_val.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')))
            .unwrap_or(raw_val)
            .replace("\\.", ".");
        return Some(CssAttrMatcher {
            op,
            value: unquoted,
        });
    }
    None
}

fn resolve_css_var_refs(value: &str, vars: &HashMap<String, String>, depth: u8) -> String {
    resolve_css_var_refs_inner(value, vars, depth, false)
}

fn resolve_css_var_refs_for_glossary(
    value: &str,
    vars: &HashMap<String, String>,
    depth: u8,
) -> String {
    resolve_css_var_refs_inner(value, vars, depth, true)
}

fn resolve_css_var_refs_inner(
    value: &str,
    vars: &HashMap<String, String>,
    depth: u8,
    preserve_unknown_fallbacks: bool,
) -> String {
    if depth >= 8 || (!value.contains("var(") && !value.contains("VAR(")) {
        return value.to_owned();
    }
    let mut out = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(pos) = rest.to_ascii_lowercase().find("var(") {
        out.push_str(&rest[..pos]);
        let after = &rest[pos + 4..];
        let mut paren_depth = 1i32;
        let mut close_idx = None;
        for (idx, ch) in after.char_indices() {
            match ch {
                '(' => paren_depth += 1,
                ')' => {
                    paren_depth -= 1;
                    if paren_depth == 0 {
                        close_idx = Some(idx);
                        break;
                    }
                }
                _ => {}
            }
        }
        let Some(end) = close_idx else {
            out.push_str(&rest[pos..]);
            return out;
        };
        let inner = &after[..end];
        let (var_name, fallback) = match inner.find(',') {
            Some(comma) => (inner[..comma].trim(), Some(inner[comma + 1..].trim())),
            None => (inner.trim(), None),
        };
        if let Some(replacement) = vars.get(&var_name.to_ascii_lowercase()) {
            out.push_str(&resolve_css_var_refs_inner(
                replacement,
                vars,
                depth + 1,
                preserve_unknown_fallbacks,
            ));
        } else if preserve_unknown_fallbacks {
            // Preserve `var(--dict-var, fallback)` so `memento_definition::Renderer`
            // can resolve dictionary-scoped `:root` / `:root[data-theme="..."]` variables.
            out.push_str(&rest[pos..pos + 4 + end + 1]);
        } else if let Some(fb) = fallback {
            let lower_name = var_name.to_ascii_lowercase();
            if matches!(
                lower_name.as_str(),
                "--font-size-no-units" | "--text-color" | "--background-color" | "--line-height"
            ) {
                out.push_str(&rest[pos..pos + 4 + end + 1]);
            } else {
                out.push_str(&resolve_css_var_refs_inner(
                    fb,
                    vars,
                    depth + 1,
                    preserve_unknown_fallbacks,
                ));
            }
        } else {
            out.push_str(&rest[pos..pos + 4 + end + 1]);
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    out
}

/// Split a CSS declaration block on top-level `;` only — ignoring semicolons inside
/// `"..."`, `'...'` (with backslash escapes), and parenthesized expressions like `url(...)`.
fn split_css_declarations_quote_aware(body: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let bytes = body.as_bytes();
    let mut start = 0usize;
    let mut i = 0usize;
    let mut in_quote: Option<u8> = None;
    let mut paren_depth = 0i32;

    while i < bytes.len() {
        let b = bytes[i];
        if let Some(q) = in_quote {
            if b == b'\\' && i + 1 < bytes.len() {
                i += 2;
                continue;
            }
            if b == q {
                in_quote = None;
            }
            i += 1;
            continue;
        }
        match b {
            b'"' | b'\'' => in_quote = Some(b),
            b'(' => paren_depth += 1,
            b')' => paren_depth = (paren_depth - 1).max(0),
            b';' if paren_depth == 0 => {
                out.push(&body[start..i]);
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    if start < bytes.len() {
        out.push(&body[start..]);
    }
    out
}

fn parse_flat_declarations(body: &str) -> Vec<(String, String)> {
    let mut decls = Vec::new();
    for raw_decl in split_css_declarations_quote_aware(body) {
        let decl = raw_decl.trim();
        if let Some(colon) = decl.find(':') {
            let prop = decl[..colon].trim().to_ascii_lowercase();
            let mut val = decl[colon + 1..].trim();
            if val.len() >= 10
                && val.as_bytes()[val.len() - 10..].eq_ignore_ascii_case(b"!important")
            {
                val = val[..val.len() - 10].trim();
            }
            if !prop.is_empty() && !val.is_empty() {
                decls.push((prop, val.to_owned()));
            }
        }
    }
    decls
}

/// Classifies a standalone root selector:
/// - `Some(None)` for unconditional `:root` / `html`
/// - `Some(Some(CssTheme::Dark))` for `:root[data-theme="dark"]` / `html[data-theme="dark"]` / `[data-theme="dark"]`
/// - `Some(Some(CssTheme::Light))` for `:root[data-theme="light"]` / `html[data-theme="light"]` / `[data-theme="light"]`
/// - `None` for any non-root or compound selector.
fn classify_root_theme_selector(sel: &str) -> Option<Option<CssTheme>> {
    let trimmed = sel.trim();
    if trimmed.is_empty() || trimmed.contains(char::is_whitespace) {
        return None;
    }
    let lower = trimmed.to_ascii_lowercase();
    if matches!(lower.as_str(), ":root" | "html") {
        return Some(None);
    }
    let base = lower
        .strip_prefix(":root")
        .or_else(|| lower.strip_prefix("html"))
        .unwrap_or(lower.as_str());
    if base.starts_with('[') && base.ends_with(']') {
        let matcher = parse_attr_matcher(trimmed, "data-theme")?;
        if matcher.op == CssAttrMatchOp::Equals {
            return CssTheme::from_data_theme(&matcher.value).map(Some);
        }
    }
    None
}

/// Extracts an explicit `[data-theme="dark"|"light"]` restriction anywhere in `sel`.
fn selector_theme_restriction(sel: &str) -> Option<CssTheme> {
    let matcher = parse_attr_matcher(sel, "data-theme")?;
    if matcher.op == CssAttrMatchOp::Equals {
        CssTheme::from_data_theme(&matcher.value)
    } else {
        None
    }
}

/// Filters `sel` against `active_theme`, returning `None` if scoped to the opposite theme,
/// or stripping a leading matching `:root[data-theme="..."]` / `html[data-theme="..."]` /
/// `[data-theme="..."]` ancestor prefix for inner chrome rule matching.
fn strip_matching_theme_prefix(sel: &str, active_theme: CssTheme) -> Option<&str> {
    let trimmed = sel.trim();
    if let Some(required) = selector_theme_restriction(trimmed) {
        if required != active_theme {
            return None;
        }
    }
    if let Some((first, rest)) = trimmed.split_once(char::is_whitespace) {
        if matches!(
            classify_root_theme_selector(first),
            Some(None) | Some(Some(_))
        ) {
            return Some(rest.trim());
        }
    }
    Some(trimmed)
}

fn combine_nested_selectors(parent: &str, child: &str) -> String {
    let p_trimmed = parent.trim();
    let c_trimmed = child.trim();
    if p_trimmed.is_empty() {
        return c_trimmed.to_owned();
    }
    if !c_trimmed.contains('&')
        && (p_trimmed.eq_ignore_ascii_case(":root") || p_trimmed.eq_ignore_ascii_case("html"))
    {
        return c_trimmed.to_owned();
    }
    let mut combined = Vec::new();
    for p in p_trimmed.split(',') {
        let p = p.trim();
        if p.is_empty() {
            continue;
        }
        for c in c_trimmed.split(',') {
            let c = c.trim();
            if c.is_empty() {
                continue;
            }
            if c.contains('&') {
                combined.push(c.replace('&', p));
            } else {
                combined.push(format!("{p} {c}"));
            }
        }
    }
    combined.join(", ")
}

/// Parse CSS rules recursively, flattening nested CSS blocks (e.g. `:root[data-theme="dark"] { .a { ... } }`
/// or `[data-dictionary="..."] { .gloss-sc-span { ... } }`) into flat `(selector_list, decls)` pairs.
fn parse_css_rules_flattened(
    input: &str,
    parent_selector: &str,
    out: &mut Vec<(String, Vec<(String, String)>)>,
    depth: u8,
) {
    if depth >= 6 {
        return;
    }
    let bytes = input.as_bytes();
    let mut i = 0usize;
    let mut seg_start = 0usize;
    let mut direct_decls_buf = String::new();
    let mut in_quote: Option<u8> = None;
    let mut paren_depth = 0i32;

    while i < bytes.len() {
        let b = bytes[i];
        if let Some(q) = in_quote {
            if b == b'\\' && i + 1 < bytes.len() {
                i += 2;
                continue;
            }
            if b == q {
                in_quote = None;
            }
            i += 1;
            continue;
        }
        match b {
            b'"' | b'\'' => {
                in_quote = Some(b);
                i += 1;
            }
            b'(' => {
                paren_depth += 1;
                i += 1;
            }
            b')' => {
                paren_depth = (paren_depth - 1).max(0);
                i += 1;
            }
            b';' if paren_depth == 0 => {
                if !parent_selector.is_empty() {
                    direct_decls_buf.push_str(&input[seg_start..=i]);
                }
                seg_start = i + 1;
                i += 1;
            }
            b'{' if paren_depth == 0 => {
                let header = input[seg_start..i].trim();
                let body_start = i + 1;
                let mut brace_depth = 1i32;
                let mut inner_quote: Option<u8> = None;
                i += 1;
                while i < bytes.len() && brace_depth > 0 {
                    let ib = bytes[i];
                    if let Some(q) = inner_quote {
                        if ib == b'\\' && i + 1 < bytes.len() {
                            i += 2;
                            continue;
                        }
                        if ib == q {
                            inner_quote = None;
                        }
                        i += 1;
                        continue;
                    }
                    match ib {
                        b'"' | b'\'' => inner_quote = Some(ib),
                        b'{' => brace_depth += 1,
                        b'}' => brace_depth -= 1,
                        _ => {}
                    }
                    i += 1;
                }
                let body_end = if brace_depth == 0 { i - 1 } else { i };
                let body = &input[body_start..body_end];
                if !header.is_empty() && !header.starts_with('@') {
                    let full_sel = combine_nested_selectors(parent_selector, header);
                    parse_css_rules_flattened(body, &full_sel, out, depth + 1);
                }
                seg_start = i;
            }
            _ => i += 1,
        }
    }

    if !parent_selector.is_empty() {
        if seg_start < bytes.len() {
            direct_decls_buf.push_str(&input[seg_start..]);
        }
        let decls = parse_flat_declarations(&direct_decls_buf);
        if !decls.is_empty() {
            out.push((parent_selector.to_owned(), decls));
        }
    }
}

fn parse_css_collapsible_lines(value: &str) -> Option<usize> {
    let lower = value.trim().to_ascii_lowercase();
    if lower.is_empty() {
        return None;
    }
    if matches!(
        lower.as_str(),
        "none" | "0" | "false" | "off" | "unset" | "initial"
    ) {
        return Some(0);
    }
    if let Ok(n) = lower.parse::<usize>() {
        return Some(n.min(200));
    }
    if let Ok(f) = lower.parse::<f32>() {
        if f.is_finite() && f >= 0.0 {
            return Some((f.round() as usize).min(200));
        }
    }
    None
}

fn apply_root_declarations(
    decls: &[(String, String)],
    vars: &HashMap<String, String>,
    theme: &mut PopupCssTheme,
    overrides: &mut PopupCssExplicitOverrides,
) {
    for (prop, raw_val) in decls {
        let val = resolve_css_var_refs(raw_val, vars, 0);
        match prop.as_str() {
            "--popup-bg" | "--background-color" => {
                if let Some(c) = parse_css_theme_color(&val) {
                    theme.popup_bg = c;
                }
            }
            "--popup-border" => {
                if let Some(c) = parse_css_theme_color(&val) {
                    theme.popup_border = c;
                }
            }
            "--popup-border-width" => {
                if let Some(px) = parse_css_length_px(&val, theme.glossary_font_size) {
                    theme.popup_border_width = px.clamp(0.0, 12.0);
                }
            }
            "--popup-radius" => {
                if let Some(px) = parse_css_length_px(&val, theme.glossary_font_size) {
                    theme.popup_radius = px.clamp(0.0, 32.0);
                }
            }
            "--popup-padding" => {
                if let Some(px) = parse_css_length_px(&val, theme.glossary_font_size) {
                    theme.popup_padding = px.clamp(0.0, 40.0);
                }
            }
            "--popup-shadow-blur" => {
                if let Some(px) = parse_css_length_px(&val, theme.glossary_font_size) {
                    theme.popup_shadow_blur = px.clamp(0.0, 64.0);
                }
            }
            "--popup-shadow-color" => {
                if let Some(c) = parse_css_theme_color(&val) {
                    theme.popup_shadow_color = c;
                }
            }
            "--popup-width" => {
                if let Some(px) = parse_css_length_px(&val, theme.glossary_font_size) {
                    theme.popup_width = if px == 0.0 {
                        0.0
                    } else {
                        px.clamp(280.0, 1600.0)
                    };
                }
            }
            "--popup-height" => {
                if let Some(px) = parse_css_length_px(&val, theme.glossary_font_size) {
                    theme.popup_height = if px == 0.0 {
                        0.0
                    } else {
                        px.clamp(200.0, 1200.0)
                    };
                }
            }
            "--text-color" => {
                if let Some(c) = parse_css_theme_color(&val) {
                    theme.text_color = c;
                }
            }
            "--muted-color" => {
                if let Some(c) = parse_css_theme_color(&val) {
                    theme.muted_color = c;
                }
            }
            "--headword-color" => {
                if let Some(c) = parse_css_theme_color(&val) {
                    theme.headword_color = c;
                }
            }
            "--headword-size" => {
                if let Some(px) = parse_css_length_px(&val, theme.glossary_font_size) {
                    theme.headword_size = px.clamp(12.0, 48.0);
                    overrides.headword_size = true;
                }
            }
            "--headword-font-family" => {
                let ff = clean_css_font_family(&val);
                if !ff.is_empty() {
                    theme.headword_font_family = ff;
                    overrides.headword_font_family = true;
                }
            }
            "--reading-color" => {
                if let Some(c) = parse_css_theme_color(&val) {
                    theme.reading_color = c;
                }
            }
            "--reading-size" => {
                if let Some(px) = parse_css_length_px(&val, theme.glossary_font_size) {
                    theme.reading_size = px.clamp(8.0, 28.0);
                    overrides.reading_size = true;
                }
            }
            "--reading-font-family" => {
                let ff = clean_css_font_family(&val);
                if !ff.is_empty() {
                    theme.reading_font_family = ff;
                    overrides.reading_font_family = true;
                }
            }
            "--conj-font-family" => {
                let ff = clean_css_font_family(&val);
                if !ff.is_empty() {
                    theme.conj_font_family = ff;
                    overrides.conj_font_family = true;
                }
            }
            "--conj-font-size" => {
                if let Some(px) = parse_css_length_px(&val, 11.0) {
                    theme.conj_font_size = px.clamp(8.0, 24.0);
                    overrides.conj_font_size = true;
                }
            }
            "--tag-font-family" => {
                let ff = clean_css_font_family(&val);
                if !ff.is_empty() {
                    theme.tag_font_family = ff;
                    overrides.tag_font_family = true;
                }
            }
            "--tag-font-size" => {
                if let Some(px) = parse_css_length_px(&val, 11.0) {
                    theme.tag_font_size = px.clamp(8.0, 22.0);
                    overrides.tag_font_size = true;
                }
            }
            "--kanji-font-family" => {
                let ff = clean_css_font_family(&val);
                if !ff.is_empty() {
                    theme.kanji_font_family = ff;
                    overrides.kanji_font_family = true;
                }
            }
            "--kanji-font-size" => {
                if let Some(px) = parse_css_length_px(&val, theme.headword_size) {
                    theme.kanji_font_size = px.clamp(16.0, 96.0);
                    overrides.kanji_font_size = true;
                }
            }
            "--font-family" => {
                let ff = clean_css_font_family(&val);
                if !ff.is_empty() {
                    theme.font_family = ff;
                    overrides.font_family = true;
                }
            }
            "--glossary-font-size" => {
                if let Some(px) = parse_css_length_px(&val, 14.5) {
                    theme.glossary_font_size = px.clamp(9.0, 32.0);
                    overrides.glossary_font_size = true;
                }
            }
            "--link-color" => {
                if let Some(c) = parse_css_theme_color(&val) {
                    theme.link_color = c;
                }
            }
            "--divider-color" => {
                if let Some(c) = parse_css_theme_color(&val) {
                    theme.divider_color = c;
                    theme.term_divider_color = c;
                }
            }
            "--term-divider-color" => {
                if let Some(c) = parse_css_theme_color(&val) {
                    theme.term_divider_color = c;
                }
            }
            "--freq-bg" => {
                if let Some(c) = parse_css_theme_color(&val) {
                    theme.freq_bg = c;
                }
            }
            "--freq-color" => {
                if let Some(c) = parse_css_theme_color(&val) {
                    theme.freq_color = c;
                }
            }
            "--freq-font-size" => {
                if let Some(px) = parse_css_length_px(&val, 10.0) {
                    theme.freq_font_size = px.clamp(7.0, 18.0);
                }
            }
            "--pitch-color" => {
                if let Some(c) = parse_css_theme_color(&val) {
                    theme.pitch_color = c;
                }
            }
            "--pitch-dict-bg" => {
                if let Some(c) = parse_css_theme_color(&val) {
                    theme.pitch_dict_bg = c;
                }
            }
            "--dict-badge-bg" => {
                if let Some(c) = parse_css_theme_color(&val) {
                    theme.dict_badge_bg = c;
                }
            }
            "--dict-badge-color" => {
                if let Some(c) = parse_css_theme_color(&val) {
                    theme.dict_badge_color = c;
                }
            }
            "--anki-tag-bg" => {
                if let Some(c) = parse_css_theme_color(&val) {
                    theme.anki_tag_bg = c;
                }
            }
            "--anki-tag-border" => {
                if let Some(c) = parse_css_theme_color(&val) {
                    theme.anki_tag_border = c;
                }
            }
            "--anki-tag-color" => {
                if let Some(c) = parse_css_theme_color(&val) {
                    theme.anki_tag_color = c;
                }
            }
            "--definition-columns" => {
                if let Ok(cols) = val.trim().parse::<i32>() {
                    theme.definition_columns = cols.clamp(1, 2);
                }
            }
            "--definition-columns-min-width" => {
                if let Some(px) = parse_css_length_px(&val, theme.glossary_font_size) {
                    theme.definition_columns_min_width = px.clamp(320.0, 1400.0);
                }
            }
            "--definition-gap" => {
                if let Some(px) = parse_css_length_px(&val, theme.glossary_font_size) {
                    theme.definition_gap = px.clamp(0.0, 32.0);
                }
            }
            "--collapsible-definition-line-count" => {
                if let Some(lines) = parse_css_collapsible_lines(&val) {
                    theme.collapsible_definition_line_count = lines;
                }
            }
            "--card-bg" => {
                if let Some(c) = parse_css_theme_color(&val) {
                    theme.card_bg = c;
                }
            }
            "--card-border" => {
                if let Some(c) = parse_css_theme_color(&val) {
                    theme.card_border = c;
                }
            }
            "--card-border-width" => {
                if let Some(px) = parse_css_length_px(&val, theme.glossary_font_size) {
                    theme.card_border_width = px.clamp(0.0, 8.0);
                }
            }
            "--card-radius" => {
                if let Some(px) = parse_css_length_px(&val, theme.glossary_font_size) {
                    theme.card_radius = px.clamp(0.0, 24.0);
                }
            }
            "--card-padding" => {
                if let Some(px) = parse_css_length_px(&val, theme.glossary_font_size) {
                    theme.card_padding = px.clamp(0.0, 32.0);
                }
            }
            "--dict-color-opacity" => {
                if let Some(op) = parse_css_unit_float(&val) {
                    theme.dict_color_opacity = op;
                }
            }
            "--derivations-compact" => {
                theme.compact_derivations = matches!(
                    val.trim().to_ascii_lowercase().as_str(),
                    "true" | "1" | "yes"
                );
            }
            "--compact-glossaries" => {
                theme.compact_glossaries = matches!(
                    val.trim().to_ascii_lowercase().as_str(),
                    "true" | "1" | "yes"
                );
            }
            "--pitch-heiban-color" => {
                theme.pitch_heiban_color = parse_css_theme_color(&val);
            }
            "--pitch-atamadaka-color" => {
                theme.pitch_atamadaka_color = parse_css_theme_color(&val);
            }
            "--pitch-nakadaka-color" => {
                theme.pitch_nakadaka_color = parse_css_theme_color(&val);
            }
            "--pitch-odaka-color" => {
                theme.pitch_odaka_color = parse_css_theme_color(&val);
            }
            "--pitch-kifuku-color" => {
                theme.pitch_kifuku_color = parse_css_theme_color(&val);
            }
            _ => {}
        }
    }
}

fn apply_theme_rules(
    rules: &[(String, Vec<(String, String)>)],
    vars: &HashMap<String, String>,
    active_theme: CssTheme,
    theme: &mut PopupCssTheme,
    overrides: &mut PopupCssExplicitOverrides,
    is_outer: bool,
) {
    // Pass 1: unconditional `:root` / `html` custom properties
    for (selector_list, decls) in rules {
        if selector_list
            .split(',')
            .any(|raw_sel| classify_root_theme_selector(raw_sel) == Some(None))
        {
            apply_root_declarations(decls, vars, theme, overrides);
        }
    }
    // Pass 2: higher-specificity `:root[data-theme="dark"|"light"]` custom properties
    for (selector_list, decls) in rules {
        if selector_list
            .split(',')
            .any(|raw_sel| classify_root_theme_selector(raw_sel) == Some(Some(active_theme)))
        {
            apply_root_declarations(decls, vars, theme, overrides);
        }
    }
    // Pass 3: non-root selectors (ignoring rules scoped to the inactive theme)
    for (selector_list, decls) in rules {
        for raw_sel in selector_list.split(',') {
            if classify_root_theme_selector(raw_sel).is_some() {
                continue;
            }
            let Some(sel) = strip_matching_theme_prefix(raw_sel, active_theme) else {
                continue;
            };
            let lower = sel.to_ascii_lowercase();

            // 2. Outer container selectors (`.popup`, `.yomitan-popup`, `iframe.yomitan-popup`, `body`)
            if matches!(
                lower.as_str(),
                ".popup" | ".yomitan-popup" | "iframe.yomitan-popup" | "body"
            ) {
                for (prop, raw_val) in decls {
                    let val = resolve_css_var_refs(raw_val, vars, 0);
                    match prop.as_str() {
                        "background" | "background-color" => {
                            if let Some(c) = parse_css_theme_color(&val) {
                                // Ignore inner `body { background: transparent; }` when outer CSS is separate
                                if is_outer || lower != "body" || c[3] > 0 {
                                    theme.popup_bg = c;
                                }
                            }
                        }
                        "border" => {
                            let (w, c) = parse_css_border_shorthand(&val, theme.glossary_font_size);
                            if let Some(px) = w {
                                theme.popup_border_width = px.clamp(0.0, 12.0);
                            }
                            if let Some(col) = c {
                                theme.popup_border = col;
                            }
                        }
                        "border-color" => {
                            if let Some(c) = parse_css_theme_color(&val) {
                                theme.popup_border = c;
                            }
                        }
                        "border-width" => {
                            if let Some(px) = parse_css_length_px(&val, theme.glossary_font_size) {
                                theme.popup_border_width = px.clamp(0.0, 12.0);
                            }
                        }
                        "border-radius" => {
                            if let Some(px) = parse_css_length_px(&val, theme.glossary_font_size) {
                                theme.popup_radius = px.clamp(0.0, 32.0);
                            }
                        }
                        "box-shadow" => {
                            let (blur, col) = parse_css_box_shadow(&val, theme.glossary_font_size);
                            if let Some(b) = blur {
                                theme.popup_shadow_blur = b.clamp(0.0, 64.0);
                            }
                            if let Some(c) = col {
                                theme.popup_shadow_color = c;
                            }
                        }
                        "padding" => {
                            if let Some(px) = parse_css_length_px(&val, theme.glossary_font_size) {
                                theme.popup_padding = px.clamp(0.0, 40.0);
                            }
                        }
                        "width" | "max-width" => {
                            if let Some(px) = parse_css_length_px(&val, theme.glossary_font_size) {
                                theme.popup_width =
                                    if px == 0.0 { 0.0 } else { px.clamp(280.0, 1600.0) };
                            }
                        }
                        "height" | "max-height" => {
                            if let Some(px) = parse_css_length_px(&val, theme.glossary_font_size) {
                                theme.popup_height =
                                    if px == 0.0 { 0.0 } else { px.clamp(200.0, 1200.0) };
                            }
                        }
                        "color" => {
                            if let Some(c) = parse_css_theme_color(&val) {
                                theme.text_color = c;
                            }
                        }
                        "font-family" => {
                            let ff = clean_css_font_family(&val);
                            if !ff.is_empty() {
                                theme.font_family = ff;
                                overrides.font_family = true;
                            }
                        }
                        "font-size" => {
                            if let Some(px) = parse_css_length_px(&val, 14.5) {
                                theme.glossary_font_size = px.clamp(9.0, 32.0);
                                overrides.glossary_font_size = true;
                            }
                        }
                        _ => {}
                    }
                }
                continue;
            }

            // 3. Per-pitch-category headword colors (`.headword[data-pitch="heiban"]` etc.)
            if lower.starts_with(".headword[") {
                if let Some(m) = parse_attr_matcher(sel, "data-pitch")
                    .or_else(|| parse_attr_matcher(sel, "data-pronunciation-categories"))
                {
                    for (prop, raw_val) in decls {
                        if matches!(prop.as_str(), "color" | "--headword-text-color") {
                            let val = resolve_css_var_refs(raw_val, vars, 0);
                            if let Some(c) = parse_css_theme_color(&val) {
                                match m.value.to_ascii_lowercase().as_str() {
                                    "heiban" => theme.pitch_heiban_color = Some(c),
                                    "atamadaka" => theme.pitch_atamadaka_color = Some(c),
                                    "nakadaka" => theme.pitch_nakadaka_color = Some(c),
                                    "odaka" => theme.pitch_odaka_color = Some(c),
                                    "kifuku" => theme.pitch_kifuku_color = Some(c),
                                    _ => {}
                                }
                            }
                        }
                    }
                }
                continue;
            }

            // 4. Per-dictionary card/badge/opacity rules (`[data-dictionary="..."]`, `.definition-item[data-dictionary^="..."]`)
            if lower.contains("[data-dictionary") && !lower.contains(' ') {
                if let Some(matcher) = parse_attr_matcher(sel, "data-dictionary") {
                    let mut rule = DictStyleRule {
                        matcher,
                        badge_bg: None,
                        badge_color: None,
                        bg_opacity: None,
                        card_bg: None,
                        card_border: None,
                        glossary_opacity: None,
                        glossary_hover_opacity: None,
                        glossary_solo_opacity: None,
                        collapsible_line_count: None,
                    };
                    let mut has_any = false;
                    for (prop, raw_val) in decls {
                        let val = resolve_css_var_refs(raw_val, vars, 0);
                        match prop.as_str() {
                            "--dict-color" | "--dict-badge-bg" => {
                                if let Some(c) = parse_css_theme_color(&val) {
                                    rule.badge_bg = Some(c);
                                    has_any = true;
                                }
                            }
                            "--dict-text-color" | "--tag-text-color" | "--dict-badge-color" => {
                                if let Some(c) = parse_css_theme_color(&val) {
                                    rule.badge_color = Some(c);
                                    has_any = true;
                                }
                            }
                            "--dict-bg-opacity" => {
                                if let Some(op) = parse_css_unit_float(&val) {
                                    rule.bg_opacity = Some(op);
                                    has_any = true;
                                }
                            }
                            "--card-bg" => {
                                if let Some(c) = parse_css_theme_color(&val) {
                                    rule.card_bg = Some(c);
                                    has_any = true;
                                }
                            }
                            "--card-border" => {
                                if let Some(c) = parse_css_theme_color(&val) {
                                    rule.card_border = Some(c);
                                    has_any = true;
                                }
                            }
                            "--glossary-opacity" => {
                                if let Some(op) = parse_css_unit_float(&val) {
                                    rule.glossary_opacity = Some(op);
                                    has_any = true;
                                }
                            }
                            "--glossary-hover-opacity" => {
                                if let Some(op) = parse_css_unit_float(&val) {
                                    rule.glossary_hover_opacity = Some(op);
                                    has_any = true;
                                }
                            }
                            "--glossary-solo-opacity" => {
                                if let Some(op) = parse_css_unit_float(&val) {
                                    rule.glossary_solo_opacity = Some(op);
                                    has_any = true;
                                }
                            }
                            "--collapsible-definition-line-count" => {
                                if let Some(lines) = parse_css_collapsible_lines(&val) {
                                    rule.collapsible_line_count = Some(lines);
                                    has_any = true;
                                }
                            }
                            _ => {}
                        }
                    }
                    if has_any {
                        theme.dict_rules.push(rule);
                    }
                }
                continue;
            }

            // 5. Per-frequency-dictionary pill rules (`[data-frequency^="JPDB"]`, `.frequency-group-item[data-details^="JPDB"]`)
            if (lower.contains("[data-frequency")
                || (lower.starts_with(".frequency") && lower.contains("[data-details")))
                && !lower.contains(' ')
            {
                if let Some(matcher) = parse_attr_matcher(sel, "data-frequency")
                    .or_else(|| parse_attr_matcher(sel, "data-details"))
                {
                    let mut rule = FreqStyleRule {
                        matcher,
                        bg: None,
                        color: None,
                    };
                    for (prop, raw_val) in decls {
                        let val = resolve_css_var_refs(raw_val, vars, 0);
                        match prop.as_str() {
                            "background"
                            | "background-color"
                            | "--tag-frequency-background-color"
                            | "--freq-bg" => {
                                if let Some(c) = parse_css_theme_color(&val) {
                                    rule.bg = Some(c);
                                }
                            }
                            "color" | "--freq-color" => {
                                if let Some(c) = parse_css_theme_color(&val) {
                                    rule.color = Some(c);
                                }
                            }
                            _ => {}
                        }
                    }
                    if rule.bg.is_some() || rule.color.is_some() {
                        theme.freq_rules.push(rule);
                    }
                }
                continue;
            }

            // 6. Per-pitch-dictionary badge rules (`[data-pitch-dict^="NHK"]`)
            if lower.contains("[data-pitch-dict") && !lower.contains(' ') {
                if let Some(matcher) = parse_attr_matcher(sel, "data-pitch-dict") {
                    for (prop, raw_val) in decls {
                        if matches!(
                            prop.as_str(),
                            "background" | "background-color" | "--tag-color"
                        ) {
                            let val = resolve_css_var_refs(raw_val, vars, 0);
                            if let Some(c) = parse_css_theme_color(&val) {
                                theme.pitch_dict_rules.push(PitchDictStyleRule {
                                    matcher: matcher.clone(),
                                    bg: c,
                                });
                            }
                        }
                    }
                }
                continue;
            }

            // 7. Standard named element & layout class selectors
            match lower.as_str() {
                ".definition-list" => {
                    for (prop, raw_val) in decls {
                        let val = resolve_css_var_refs(raw_val, vars, 0);
                        match prop.as_str() {
                            "columns" | "column-count" => {
                                if let Ok(cols) = val.trim().parse::<i32>() {
                                    theme.definition_columns = cols.clamp(1, 2);
                                }
                            }
                            "gap" => {
                                if let Some(px) =
                                    parse_css_length_px(&val, theme.glossary_font_size)
                                {
                                    theme.definition_gap = px.clamp(0.0, 32.0);
                                }
                            }
                            _ => {}
                        }
                    }
                }
                ".definition-card" | ".definition-item" => {
                    for (prop, raw_val) in decls {
                        let val = resolve_css_var_refs(raw_val, vars, 0);
                        match prop.as_str() {
                            "background" | "background-color" => {
                                if let Some(c) = parse_css_theme_color(&val) {
                                    theme.card_bg = c;
                                }
                            }
                            "border" => {
                                let (w, c) =
                                    parse_css_border_shorthand(&val, theme.glossary_font_size);
                                if let Some(px) = w {
                                    theme.card_border_width = px.clamp(0.0, 8.0);
                                }
                                if let Some(col) = c {
                                    theme.card_border = col;
                                }
                            }
                            "border-color" => {
                                if let Some(c) = parse_css_theme_color(&val) {
                                    theme.card_border = c;
                                }
                            }
                            "border-width" => {
                                if let Some(px) =
                                    parse_css_length_px(&val, theme.glossary_font_size)
                                {
                                    theme.card_border_width = px.clamp(0.0, 8.0);
                                }
                            }
                            "border-radius" => {
                                if let Some(px) =
                                    parse_css_length_px(&val, theme.glossary_font_size)
                                {
                                    theme.card_radius = px.clamp(0.0, 24.0);
                                }
                            }
                            "padding" => {
                                if let Some(px) =
                                    parse_css_length_px(&val, theme.glossary_font_size)
                                {
                                    theme.card_padding = px.clamp(0.0, 32.0);
                                }
                            }
                            _ => {}
                        }
                    }
                }
                ".term-tags" | ".headword-list-tag-list" => {
                    for (prop, raw_val) in decls {
                        if prop == "display" {
                            theme.show_term_tags = !raw_val.trim().eq_ignore_ascii_case("none");
                        }
                    }
                }
                ".kanji-extra" => {
                    for (prop, raw_val) in decls {
                        if prop == "display" {
                            theme.show_kanji_extra = !raw_val.trim().eq_ignore_ascii_case("none");
                        }
                    }
                }
                ".derivations" | ".inflection-rule-chains" => {
                    for (prop, raw_val) in decls {
                        let val = resolve_css_var_refs(raw_val, vars, 0);
                        match prop.as_str() {
                            "display" => {
                                let v = val.trim().to_ascii_lowercase();
                                if v == "compact" || v == "tooltip" {
                                    theme.compact_derivations = true;
                                } else if v == "flex" || v == "block" {
                                    theme.compact_derivations = false;
                                }
                            }
                            "font-family" => {
                                let ff = clean_css_font_family(&val);
                                if !ff.is_empty() {
                                    theme.conj_font_family = ff;
                                    overrides.conj_font_family = true;
                                }
                            }
                            "font-size" => {
                                if let Some(px) = parse_css_length_px(&val, 11.0) {
                                    theme.conj_font_size = px.clamp(8.0, 24.0);
                                    overrides.conj_font_size = true;
                                }
                            }
                            _ => {}
                        }
                    }
                }
                ".headword" | ".headword-term" => {
                    for (prop, raw_val) in decls {
                        let val = resolve_css_var_refs(raw_val, vars, 0);
                        match prop.as_str() {
                            "color" => {
                                if let Some(c) = parse_css_theme_color(&val) {
                                    theme.headword_color = c;
                                }
                            }
                            "font-family" => {
                                let ff = clean_css_font_family(&val);
                                if !ff.is_empty() {
                                    theme.headword_font_family = ff;
                                    overrides.headword_font_family = true;
                                }
                            }
                            "font-size" => {
                                if let Some(px) =
                                    parse_css_length_px(&val, theme.glossary_font_size)
                                {
                                    theme.headword_size = px.clamp(12.0, 48.0);
                                    overrides.headword_size = true;
                                }
                            }
                            _ => {}
                        }
                    }
                }
                ".kanji-headword" => {
                    for (prop, raw_val) in decls {
                        let val = resolve_css_var_refs(raw_val, vars, 0);
                        match prop.as_str() {
                            "font-family" => {
                                let ff = clean_css_font_family(&val);
                                if !ff.is_empty() {
                                    theme.kanji_font_family = ff;
                                    overrides.kanji_font_family = true;
                                }
                            }
                            "font-size" => {
                                if let Some(px) = parse_css_length_px(&val, theme.headword_size) {
                                    theme.kanji_font_size = px.clamp(16.0, 96.0);
                                    overrides.kanji_font_size = true;
                                }
                            }
                            _ => {}
                        }
                    }
                }
                ".reading" | ".headword-reading" => {
                    for (prop, raw_val) in decls {
                        let val = resolve_css_var_refs(raw_val, vars, 0);
                        match prop.as_str() {
                            "color" => {
                                if let Some(c) = parse_css_theme_color(&val) {
                                    theme.reading_color = c;
                                }
                            }
                            "font-family" => {
                                let ff = clean_css_font_family(&val);
                                if !ff.is_empty() {
                                    theme.reading_font_family = ff;
                                    overrides.reading_font_family = true;
                                }
                            }
                            "font-size" => {
                                if let Some(px) =
                                    parse_css_length_px(&val, theme.glossary_font_size)
                                {
                                    theme.reading_size = px.clamp(8.0, 28.0);
                                    overrides.reading_size = true;
                                }
                            }
                            _ => {}
                        }
                    }
                }
                ".glossary" | ".gloss-content" => {
                    for (prop, raw_val) in decls {
                        let val = resolve_css_var_refs(raw_val, vars, 0);
                        match prop.as_str() {
                            "color" => {
                                if let Some(c) = parse_css_theme_color(&val) {
                                    theme.text_color = c;
                                }
                            }
                            "font-size" => {
                                if let Some(px) = parse_css_length_px(&val, 14.5) {
                                    theme.glossary_font_size = px.clamp(9.0, 32.0);
                                    overrides.glossary_font_size = true;
                                }
                            }
                            "font-family" => {
                                let ff = clean_css_font_family(&val);
                                if !ff.is_empty() {
                                    theme.font_family = ff;
                                    overrides.font_family = true;
                                }
                            }
                            _ => {}
                        }
                    }
                }
                "a" => {
                    for (prop, raw_val) in decls {
                        if prop == "color" {
                            let val = resolve_css_var_refs(raw_val, vars, 0);
                            if let Some(c) = parse_css_theme_color(&val) {
                                theme.link_color = c;
                            }
                        }
                    }
                }
                ".divider" | "hr" => {
                    for (prop, raw_val) in decls {
                        if matches!(
                            prop.as_str(),
                            "background" | "background-color" | "color" | "border-color"
                        ) {
                            let val = resolve_css_var_refs(raw_val, vars, 0);
                            if let Some(c) = parse_css_theme_color(&val) {
                                theme.divider_color = c;
                                theme.term_divider_color = c;
                            }
                        }
                    }
                }
                ".frequency" | ".frequency-badge" | ".frequency-group-item" => {
                    for (prop, raw_val) in decls {
                        let val = resolve_css_var_refs(raw_val, vars, 0);
                        match prop.as_str() {
                            "background" | "background-color" => {
                                if let Some(c) = parse_css_theme_color(&val) {
                                    theme.freq_bg = c;
                                }
                            }
                            "color" => {
                                if let Some(c) = parse_css_theme_color(&val) {
                                    theme.freq_color = c;
                                }
                            }
                            "font-size" => {
                                if let Some(px) = parse_css_length_px(&val, 10.0) {
                                    theme.freq_font_size = px.clamp(7.0, 18.0);
                                }
                            }
                            _ => {}
                        }
                    }
                }
                ".pitch" | ".pronunciation" => {
                    for (prop, raw_val) in decls {
                        if matches!(
                            prop.as_str(),
                            "color" | "border-color" | "background" | "background-color"
                        ) {
                            let val = resolve_css_var_refs(raw_val, vars, 0);
                            if let Some(c) = parse_css_theme_color(&val) {
                                theme.pitch_color = c;
                            }
                        }
                    }
                }
                ".pitch-dict" | ".pronunciation-group-tag-list" => {
                    for (prop, raw_val) in decls {
                        let val = resolve_css_var_refs(raw_val, vars, 0);
                        if prop == "display" {
                            theme.show_pitch_dict = !val.trim().eq_ignore_ascii_case("none");
                        } else if matches!(prop.as_str(), "background" | "background-color") {
                            if let Some(c) = parse_css_theme_color(&val) {
                                theme.pitch_dict_bg = c;
                            }
                        }
                    }
                }
                ".dictionary-badge" | ".tag-dictionary" => {
                    for (prop, raw_val) in decls {
                        let val = resolve_css_var_refs(raw_val, vars, 0);
                        match prop.as_str() {
                            "background" | "background-color" => {
                                if let Some(c) = parse_css_theme_color(&val) {
                                    theme.dict_badge_bg = c;
                                }
                            }
                            "color" => {
                                if let Some(c) = parse_css_theme_color(&val) {
                                    theme.dict_badge_color = c;
                                }
                            }
                            "font-family" => {
                                let ff = clean_css_font_family(&val);
                                if !ff.is_empty() {
                                    theme.tag_font_family = ff;
                                    overrides.tag_font_family = true;
                                }
                            }
                            "font-size" => {
                                if let Some(px) = parse_css_length_px(&val, 11.0) {
                                    theme.tag_font_size = px.clamp(8.0, 22.0);
                                    overrides.tag_font_size = true;
                                }
                            }
                            _ => {}
                        }
                    }
                }
                ".anki-tag" => {
                    for (prop, raw_val) in decls {
                        let val = resolve_css_var_refs(raw_val, vars, 0);
                        match prop.as_str() {
                            "background" | "background-color" => {
                                if let Some(c) = parse_css_theme_color(&val) {
                                    theme.anki_tag_bg = c;
                                }
                            }
                            "border" => {
                                let (_, c) =
                                    parse_css_border_shorthand(&val, theme.glossary_font_size);
                                if let Some(col) = c {
                                    theme.anki_tag_border = col;
                                }
                            }
                            "border-color" => {
                                if let Some(c) = parse_css_theme_color(&val) {
                                    theme.anki_tag_border = c;
                                }
                            }
                            "color" => {
                                if let Some(c) = parse_css_theme_color(&val) {
                                    theme.anki_tag_color = c;
                                }
                            }
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
        }
    }
}

fn collect_root_vars(
    rules: &[(String, Vec<(String, String)>)],
    active_theme: CssTheme,
    vars: &mut HashMap<String, String>,
) {
    // Pass 1: unconditional root/container custom properties
    for (selector_list, decls) in rules {
        let applies_unconditional = selector_list.split(',').any(|sel| {
            let trimmed = sel.trim();
            classify_root_theme_selector(trimmed) == Some(None)
                || matches!(
                    trimmed.to_ascii_lowercase().as_str(),
                    "body" | ".popup" | ".yomitan-popup" | "iframe.yomitan-popup"
                )
        });
        if applies_unconditional {
            for (prop, val) in decls {
                if prop.starts_with("--") {
                    vars.insert(prop.clone(), val.clone());
                }
            }
        }
    }
    // Pass 2: active-theme `:root[data-theme="dark"|"light"]` custom properties override bare `:root`
    for (selector_list, decls) in rules {
        let applies_for_theme = selector_list
            .split(',')
            .any(|sel| classify_root_theme_selector(sel) == Some(Some(active_theme)));
        if applies_for_theme {
            for (prop, val) in decls {
                if prop.starts_with("--") {
                    vars.insert(prop.clone(), val.clone());
                }
            }
        }
    }
}

fn parse_popup_css_theme_full_with_theme(
    inner_css: &str,
    outer_css: &str,
    active_theme: CssTheme,
) -> (
    PopupCssTheme,
    PopupCssExplicitOverrides,
    HashMap<String, String>,
    Vec<(String, Vec<(String, String)>)>,
) {
    let mut theme = PopupCssTheme::default_for_theme(active_theme);
    let mut overrides = PopupCssExplicitOverrides::default();
    let mut vars: HashMap<String, String> = HashMap::new();

    let inner_stripped = strip_css_comments(inner_css);
    let mut inner_rules = Vec::new();
    parse_css_rules_flattened(&inner_stripped, "", &mut inner_rules, 0);

    let outer_stripped = strip_css_comments(outer_css);
    let mut outer_rules = Vec::new();
    parse_css_rules_flattened(&outer_stripped, "", &mut outer_rules, 0);

    collect_root_vars(&inner_rules, active_theme, &mut vars);
    collect_root_vars(&outer_rules, active_theme, &mut vars);

    apply_theme_rules(
        &inner_rules,
        &vars,
        active_theme,
        &mut theme,
        &mut overrides,
        false,
    );
    apply_theme_rules(
        &outer_rules,
        &vars,
        active_theme,
        &mut theme,
        &mut overrides,
        true,
    );

    (theme, overrides, vars, inner_rules)
}

pub fn parse_popup_css_theme(css: &str) -> PopupCssTheme {
    parse_popup_css_theme_full_with_theme(css, "", CssTheme::Dark).0
}

#[allow(dead_code)]
pub fn parse_popup_css_theme_with_theme(css: &str, active_theme: CssTheme) -> PopupCssTheme {
    parse_popup_css_theme_full_with_theme(css, "", active_theme).0
}

#[allow(dead_code)]
pub fn parse_popup_css_theme_for_mode(css: &str, is_light_theme: bool) -> PopupCssTheme {
    let theme = if is_light_theme {
        CssTheme::Light
    } else {
        CssTheme::Dark
    };
    parse_popup_css_theme_full_with_theme(css, "", theme).0
}

pub fn parse_split_popup_css_theme(inner_css: &str, outer_css: &str) -> PopupCssTheme {
    parse_popup_css_theme_full_with_theme(inner_css, outer_css, CssTheme::Dark).0
}

#[allow(dead_code)]
pub fn parse_split_popup_css_theme_with_theme(
    inner_css: &str,
    outer_css: &str,
    active_theme: CssTheme,
) -> PopupCssTheme {
    parse_popup_css_theme_full_with_theme(inner_css, outer_css, active_theme).0
}

fn preprocess_user_glossary_css(
    inner_rules: &[(String, Vec<(String, String)>)],
    vars: &HashMap<String, String>,
    active_theme: CssTheme,
) -> String {
    let mut out = String::new();
    for (selector_list, decls) in inner_rules {
        let mut active_selectors: Vec<String> = Vec::new();
        let mut is_root_only = true;
        for raw_sel in selector_list.split(',') {
            let trimmed = raw_sel.trim();
            if trimmed.is_empty() {
                continue;
            }
            if let Some(required_theme) = selector_theme_restriction(trimmed) {
                if required_theme != active_theme {
                    continue;
                }
            }
            if classify_root_theme_selector(trimmed).is_none() {
                is_root_only = false;
            }
            // Normalize Yomitan dictionary container selectors onto our virtual `<memento-dict>` wrapper
            let norm = trimmed
                .replace("li.definition-item[data-dictionary", "[data-dictionary")
                .replace(".definition-item[data-dictionary", "[data-dictionary")
                .replace("li[data-dictionary", "[data-dictionary")
                .replace("LI[data-dictionary", "[data-dictionary")
                .replace("div[data-dictionary", "[data-dictionary")
                .replace("DIV[data-dictionary", "[data-dictionary");
            active_selectors.push(norm);
        }
        if active_selectors.is_empty() || is_root_only {
            continue;
        }
        let norm_sel = active_selectors.join(", ");
        let mut decl_str = String::new();
        for (prop, raw_val) in decls {
            // Preserve element-scoped custom properties (e.g. `[data-dictionary="..."] { --accent: ... }`)
            // so per-dictionary custom properties cascade cleanly inside `memento_definition`
            // without leaking across dictionaries.
            let val = resolve_css_var_refs_for_glossary(raw_val, vars, 0);
            decl_str.push_str(prop);
            decl_str.push_str(": ");
            decl_str.push_str(&val);
            decl_str.push_str("; ");
        }
        if !decl_str.is_empty() {
            out.push_str(&norm_sel);
            out.push_str(" { ");
            out.push_str(&decl_str);
            out.push_str("}\n");
        }
    }
    out
}

pub(super) fn rgba_to_css_color(rgba: [u8; 4]) -> String {
    if rgba[3] == 255 {
        format!("#{:02X}{:02X}{:02X}", rgba[0], rgba[1], rgba[2])
    } else {
        let a = f32::from(rgba[3]) / 255.0;
        format!("rgba({}, {}, {}, {:.3})", rgba[0], rgba[1], rgba[2], a)
    }
}

pub(super) fn rgba_to_slint_color(rgba: [u8; 4]) -> slint::Color {
    slint::Color::from_argb_u8(rgba[3], rgba[0], rgba[1], rgba[2])
}

#[allow(dead_code)]
pub(super) fn apply_popup_css_theme_to_bridge(app: &MementoApp, theme: &PopupCssTheme) {
    let pb = app.global::<PopupBridge>();
    pb.set_css_popup_bg(rgba_to_slint_color(theme.popup_bg));
    pb.set_css_popup_border(rgba_to_slint_color(theme.popup_border));
    pb.set_css_popup_border_width(theme.popup_border_width);
    pb.set_css_popup_radius(theme.popup_radius);
    pb.set_css_popup_padding(theme.popup_padding);
    pb.set_css_popup_shadow_blur(theme.popup_shadow_blur);
    pb.set_css_popup_shadow_color(rgba_to_slint_color(theme.popup_shadow_color));
    pb.set_css_popup_width(theme.popup_width);
    pb.set_css_popup_height(theme.popup_height);
    pb.set_css_text_color(rgba_to_slint_color(theme.text_color));
    pb.set_css_muted_color(rgba_to_slint_color(theme.muted_color));
    pb.set_css_headword_color(rgba_to_slint_color(theme.headword_color));
    pb.set_css_headword_size(theme.headword_size);
    pb.set_css_reading_color(rgba_to_slint_color(theme.reading_color));
    pb.set_css_reading_size(theme.reading_size);
    pb.set_css_font_family(theme.font_family.clone().into());
    pb.set_css_headword_font_family(
        if theme.headword_font_family.is_empty() {
            theme.font_family.as_str()
        } else {
            theme.headword_font_family.as_str()
        }
        .into(),
    );
    pb.set_css_reading_font_family(
        if theme.reading_font_family.is_empty() {
            theme.font_family.as_str()
        } else {
            theme.reading_font_family.as_str()
        }
        .into(),
    );
    pb.set_css_conj_font_family(
        if theme.conj_font_family.is_empty() {
            theme.font_family.as_str()
        } else {
            theme.conj_font_family.as_str()
        }
        .into(),
    );
    pb.set_css_conj_font_size(theme.conj_font_size);
    pb.set_css_tag_font_family(
        if theme.tag_font_family.is_empty() {
            theme.font_family.as_str()
        } else {
            theme.tag_font_family.as_str()
        }
        .into(),
    );
    pb.set_css_tag_font_size(theme.tag_font_size);
    pb.set_css_kanji_font_family(
        if theme.kanji_font_family.is_empty() {
            theme.font_family.as_str()
        } else {
            theme.kanji_font_family.as_str()
        }
        .into(),
    );
    pb.set_css_kanji_font_size(theme.kanji_font_size);
    pb.set_css_glossary_font_size(theme.glossary_font_size);
    pb.set_css_link_color(rgba_to_slint_color(theme.link_color));
    pb.set_css_divider_color(rgba_to_slint_color(theme.divider_color));
    pb.set_css_term_divider_color(rgba_to_slint_color(theme.term_divider_color));
    pb.set_css_freq_bg(rgba_to_slint_color(theme.freq_bg));
    pb.set_css_freq_color(rgba_to_slint_color(theme.freq_color));
    pb.set_css_freq_font_size(theme.freq_font_size);
    pb.set_css_pitch_color(rgba_to_slint_color(theme.pitch_color));
    pb.set_css_pitch_dict_bg(rgba_to_slint_color(theme.pitch_dict_bg));
    pb.set_css_dict_badge_bg(rgba_to_slint_color(theme.dict_badge_bg));
    pb.set_css_dict_badge_color(rgba_to_slint_color(theme.dict_badge_color));
    pb.set_css_anki_tag_bg(rgba_to_slint_color(theme.anki_tag_bg));
    pb.set_css_anki_tag_border(rgba_to_slint_color(theme.anki_tag_border));
    pb.set_css_anki_tag_color(rgba_to_slint_color(theme.anki_tag_color));
    pb.set_css_definition_columns(theme.definition_columns);
    pb.set_css_definition_columns_min_width(theme.definition_columns_min_width);
    pb.set_css_definition_gap(theme.definition_gap);
    pb.set_css_show_term_tags(theme.show_term_tags);
    pb.set_css_show_pitch_dict(theme.show_pitch_dict);
    pb.set_css_show_kanji_extra(theme.show_kanji_extra);
    pb.set_css_compact_derivations(theme.compact_derivations);
}

#[derive(Default)]
pub(super) struct StylesheetCache {
    active_theme: CssTheme,
    user_css_path: PathBuf,
    user_outer_css_path: PathBuf,
    user_css_sig: Option<(Option<SystemTime>, u64)>,
    user_outer_css_sig: Option<(Option<SystemTime>, u64)>,
    user_css_raw: String,
    user_outer_css_raw: String,
    user_css_glossary_preprocessed: String,
    user_css_theme: PopupCssTheme,
    user_css_overrides: PopupCssExplicitOverrides,
    pub(super) user_css_revision: u64,
    user_only_sheet: Option<Arc<ParsedStylesheet>>,
    entries: HashMap<(PathBuf, CssTheme), (Option<SystemTime>, u64, Arc<ParsedStylesheet>)>,
}

impl StylesheetCache {
    #[cfg(test)]
    pub(super) fn new(config_dir: &Path) -> Self {
        Self::new_with_theme(config_dir, CssTheme::Dark)
    }

    pub(super) fn new_with_theme(config_dir: &Path, active_theme: CssTheme) -> Self {
        let mut cache = Self {
            active_theme,
            user_css_path: config_dir.join(CUSTOM_POPUP_CSS_FILENAME),
            user_outer_css_path: config_dir.join(CUSTOM_POPUP_OUTER_CSS_FILENAME),
            user_css_theme: PopupCssTheme::default_for_theme(active_theme),
            ..Self::default()
        };
        cache.force_reload_user_css();
        cache
    }

    #[allow(dead_code)]
    pub(super) fn active_theme(&self) -> CssTheme {
        self.active_theme
    }

    /// Updates the active color theme (`Dark` / `Light`), re-evaluates `:root[data-theme="..."]`
    /// rules, bumps `user_css_revision`, and invalidates cached dictionary stylesheets when changed.
    #[allow(dead_code)]
    pub(super) fn set_theme(&mut self, theme: CssTheme) -> bool {
        if self.active_theme == theme {
            return false;
        }
        self.active_theme = theme;
        self.rebuild_user_css_state();
        self.user_css_revision = self.user_css_revision.wrapping_add(1).max(1);
        self.entries.clear();
        true
    }

    #[allow(dead_code)]
    pub(super) fn set_light_theme(&mut self, is_light: bool) -> bool {
        self.set_theme(if is_light {
            CssTheme::Light
        } else {
            CssTheme::Dark
        })
    }

    pub(super) fn theme(&self) -> &PopupCssTheme {
        &self.user_css_theme
    }

    pub(super) fn overrides(&self) -> &PopupCssExplicitOverrides {
        &self.user_css_overrides
    }

    #[allow(dead_code)]
    pub(super) fn user_css_revision(&self) -> u64 {
        self.user_css_revision
    }

    fn read_file_sig(path: &Path) -> Option<(Option<SystemTime>, u64)> {
        if path.as_os_str().is_empty() {
            return None;
        }
        let meta = std::fs::metadata(path).ok()?;
        Some((meta.modified().ok(), meta.len()))
    }

    pub(super) fn reload_user_css_if_changed(&mut self) -> bool {
        if self.user_css_path.as_os_str().is_empty()
            && self.user_outer_css_path.as_os_str().is_empty()
        {
            return false;
        }
        let inner_sig = Self::read_file_sig(&self.user_css_path);
        let outer_sig = Self::read_file_sig(&self.user_outer_css_path);
        if inner_sig == self.user_css_sig && outer_sig == self.user_outer_css_sig {
            return false;
        }
        self.apply_reloaded_user_css(inner_sig, outer_sig)
    }

    pub(super) fn force_reload_user_css(&mut self) -> bool {
        if self.user_css_path.as_os_str().is_empty()
            && self.user_outer_css_path.as_os_str().is_empty()
        {
            return false;
        }
        let inner_sig = Self::read_file_sig(&self.user_css_path);
        let outer_sig = Self::read_file_sig(&self.user_outer_css_path);
        self.apply_reloaded_user_css(inner_sig, outer_sig)
    }

    fn rebuild_user_css_state(&mut self) {
        if self.user_css_raw.trim().is_empty() && self.user_outer_css_raw.trim().is_empty() {
            self.user_css_theme = PopupCssTheme::default_for_theme(self.active_theme);
            self.user_css_overrides = PopupCssExplicitOverrides::default();
            self.user_css_glossary_preprocessed.clear();
            self.user_only_sheet = None;
        } else {
            let (theme, overrides, vars, inner_rules) = parse_popup_css_theme_full_with_theme(
                &self.user_css_raw,
                &self.user_outer_css_raw,
                self.active_theme,
            );
            let preprocessed =
                preprocess_user_glossary_css(&inner_rules, &vars, self.active_theme);
            self.user_css_theme = theme;
            self.user_css_overrides = overrides;
            self.user_only_sheet = (!preprocessed.trim().is_empty())
                .then(|| parse_dictionary_css_str(&preprocessed));
            self.user_css_glossary_preprocessed = preprocessed;
        }
    }

    fn apply_reloaded_user_css(
        &mut self,
        inner_sig: Option<(Option<SystemTime>, u64)>,
        outer_sig: Option<(Option<SystemTime>, u64)>,
    ) -> bool {
        let inner_raw = if inner_sig.is_some() {
            std::fs::read_to_string(&self.user_css_path).unwrap_or_default()
        } else {
            String::new()
        };
        let outer_raw = if outer_sig.is_some() {
            std::fs::read_to_string(&self.user_outer_css_path).unwrap_or_default()
        } else {
            String::new()
        };
        let changed = inner_sig != self.user_css_sig
            || outer_sig != self.user_outer_css_sig
            || inner_raw != self.user_css_raw
            || outer_raw != self.user_outer_css_raw;
        self.user_css_sig = inner_sig;
        self.user_outer_css_sig = outer_sig;
        if !changed && self.user_css_revision > 0 {
            return false;
        }
        self.user_css_raw = inner_raw;
        self.user_outer_css_raw = outer_raw;
        self.rebuild_user_css_state();
        self.user_css_revision = self.user_css_revision.wrapping_add(1).max(1);
        self.entries.clear();
        true
    }

    pub(super) fn get_for_dictionary(
        &mut self,
        dict_resource_dirs: &[PathBuf],
        dic_id: i64,
        dict_name: &str,
    ) -> Option<Arc<ParsedStylesheet>> {
        self.get_for_dictionary_with_theme(dict_resource_dirs, dic_id, dict_name, self.active_theme)
    }

    pub(super) fn get_for_dictionary_with_theme(
        &mut self,
        dict_resource_dirs: &[PathBuf],
        dic_id: i64,
        dict_name: &str,
        theme: CssTheme,
    ) -> Option<Arc<ParsedStylesheet>> {
        let user_css_for_theme: String;
        let (user_preprocessed, user_only): (&str, Option<Arc<ParsedStylesheet>>) =
            if theme == self.active_theme {
                (
                    self.user_css_glossary_preprocessed.as_str(),
                    self.user_only_sheet.clone(),
                )
            } else if self.user_css_raw.trim().is_empty()
                && self.user_outer_css_raw.trim().is_empty()
            {
                ("", None)
            } else {
                let (_, _, vars, inner_rules) = parse_popup_css_theme_full_with_theme(
                    &self.user_css_raw,
                    &self.user_outer_css_raw,
                    theme,
                );
                user_css_for_theme = preprocess_user_glossary_css(&inner_rules, &vars, theme);
                let only = (!user_css_for_theme.trim().is_empty())
                    .then(|| parse_dictionary_css_str(&user_css_for_theme));
                (user_css_for_theme.as_str(), only)
            };

        let style_path = dict_resource_dirs.iter().find_map(|dir| {
            memento_dict::databasemanager::resolve_dictionary_style_path(dir, dic_id, dict_name)
        });
        let Some(style_path) = style_path else {
            return user_only;
        };
        let mtime = std::fs::metadata(&style_path)
            .ok()
            .and_then(|m| m.modified().ok());
        let cache_key = (style_path.clone(), theme);
        if let Some((cached_mtime, cached_user_rev, sheet)) = self.entries.get(&cache_key) {
            if *cached_mtime == mtime && *cached_user_rev == self.user_css_revision {
                return Some(sheet.clone());
            }
        }
        let dict_css = match std::fs::read_to_string(&style_path) {
            Ok(s) => s,
            Err(_) => return user_only,
        };
        let combined = if user_preprocessed.trim().is_empty() {
            dict_css
        } else {
            format!("{dict_css}\n{user_preprocessed}")
        };
        let parsed = parse_dictionary_css_str(&combined);
        self.entries
            .insert(cache_key, (mtime, self.user_css_revision, parsed.clone()));
        Some(parsed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::glossary_render::render_structured_glossary_entry_with_theme;
    use memento_definition::LegacyGlossaryStyle;

    #[test]
    fn quote_and_url_aware_declaration_splitting_preserves_embedded_semicolons() {
        let css = r#"
            .gloss-sc-span::before {
                content: "a; b";
                color: #112233;
            }
            .gloss-sc-span::after {
                content: 'x; y';
                background-image: url(data:image/svg+xml;charset=utf-8,%3Csvg%3E%3C/svg%3E);
                font-weight: bold;
            }
        "#;
        let mut rules = Vec::new();
        parse_css_rules_flattened(&strip_css_comments(css), "", &mut rules, 0);
        assert_eq!(rules.len(), 2);
        assert_eq!(
            rules[0].1,
            vec![
                ("content".to_owned(), "\"a; b\"".to_owned()),
                ("color".to_owned(), "#112233".to_owned()),
            ]
        );
        assert_eq!(
            rules[1].1,
            vec![
                ("content".to_owned(), "'x; y'".to_owned()),
                (
                    "background-image".to_owned(),
                    "url(data:image/svg+xml;charset=utf-8,%3Csvg%3E%3C/svg%3E)".to_owned()
                ),
                ("font-weight".to_owned(), "bold".to_owned()),
            ]
        );
    }

    #[test]
    fn custom_popup_css_parser_handles_variables_selectors_and_template() {
        // Starter templates must reproduce exact defaults cleanly.
        let from_template = parse_split_popup_css_theme(
            default_custom_popup_css_template(),
            default_custom_popup_outer_css_template(),
        );
        assert_eq!(from_template, PopupCssTheme::default());

        // Custom :root variables (including var() indirection, CSS #RRGGBBAA, comments, !important)
        // and class selectors must override defaults accurately.
        let css = r#"
            /* Theme comment */
            :root {
                --brand: #FF3366;
                --popup-bg: #101418EE !important;
                --popup-border: var(--brand);
                --popup-border-width: 2px;
                --popup-radius: 14px;
                --popup-padding: 18px;
                --font-family: "Noto Serif CJK JP", serif;
                --glossary-font-size: 16px;
                --anki-tag-border: #38BDF8AA;
            }
            .headword {
                color: var(--brand);
                font-size: 28px;
            }
            .reading {
                color: rgb(200, 210, 220);
                font-size: 14px;
            }
            .frequency {
                background-color: #112233;
                color: #AAFFEE;
            }
            .dictionary-badge {
                background: #441188;
                color: #FFFFFF;
            }
            .divider {
                background-color: #556677;
            }
        "#;
        let theme = parse_popup_css_theme(css);
        assert_eq!(theme.popup_bg, [0x10, 0x14, 0x18, 0xEE]);
        assert_eq!(theme.popup_border, [0xFF, 0x33, 0x66, 0xFF]);
        assert_eq!(theme.popup_border_width, 2.0);
        assert_eq!(theme.popup_radius, 14.0);
        assert_eq!(theme.popup_padding, 18.0);
        assert_eq!(theme.font_family, "Noto Serif CJK JP");
        assert_eq!(theme.glossary_font_size, 16.0);
        assert_eq!(theme.anki_tag_border, [0x38, 0xBD, 0xF8, 0xAA]);
        assert_eq!(theme.headword_color, [0xFF, 0x33, 0x66, 0xFF]);
        assert_eq!(theme.headword_size, 28.0);
        assert_eq!(theme.reading_color, [200, 210, 220, 255]);
        assert_eq!(theme.reading_size, 14.0);
        assert_eq!(theme.freq_bg, [0x11, 0x22, 0x33, 0xFF]);
        assert_eq!(theme.freq_color, [0xAA, 0xFF, 0xEE, 0xFF]);
        assert_eq!(theme.dict_badge_bg, [0x44, 0x11, 0x88, 0xFF]);
        assert_eq!(theme.dict_badge_color, [0xFF, 0xFF, 0xFF, 0xFF]);
        assert_eq!(theme.divider_color, [0x55, 0x66, 0x77, 0xFF]);
        assert_eq!(theme.term_divider_color, [0x55, 0x66, 0x77, 0xFF]);
    }

    #[test]
    fn custom_popup_css_glossary_cascade_supports_data_dictionary_and_display_none() {
        let dir = tempfile::TempDir::new().unwrap();
        let css_path = dir.path().join(CUSTOM_POPUP_CSS_FILENAME);
        std::fs::write(
            &css_path,
            r#"
            [data-dictionary^="JMdict"] [data-sc-content="examples"] {
                display: none;
            }
            li[data-dictionary="SpecialDict"] [data-sc-content="note"] {
                display: none;
            }
            "#,
        )
        .unwrap();

        let mut cache = StylesheetCache::new(dir.path());
        let res_dirs: Vec<PathBuf> = Vec::new();
        let raw = r#"[{
            "type": "structured-content",
            "content": [
                {"tag": "div", "data": {"content": "glossary"}, "content": "main meaning"},
                {"tag": "div", "data": {"content": "examples"}, "content": "secret example"},
                {"tag": "div", "data": {"content": "note"}, "content": "extra note"}
            ]
        }]"#;

        let jmdict_sheet = cache.get_for_dictionary(&res_dirs, 1, "JMdict [2025-11-01]");
        let jmdict_doc = render_structured_glossary_entry_with_theme(
            raw,
            "",
            jmdict_sheet,
            &HashMap::new(),
            LegacyGlossaryStyle::Bullet,
            &res_dirs,
            1,
            "JMdict [2025-11-01]",
            420.0,
            true,
            cache.theme(),
        )
        .expect("JMdict doc should render");
        assert!(jmdict_doc.plain_text.contains("main meaning"));
        assert!(
            !jmdict_doc.plain_text.contains("secret example"),
            "display: none under [data-dictionary^=\"JMdict\"] must hide examples, got: {:?}",
            jmdict_doc.plain_text
        );
        assert!(jmdict_doc.plain_text.contains("extra note"));

        let other_sheet = cache.get_for_dictionary(&res_dirs, 2, "SpecialDict");
        let other_doc = render_structured_glossary_entry_with_theme(
            raw,
            "",
            other_sheet,
            &HashMap::new(),
            LegacyGlossaryStyle::Bullet,
            &res_dirs,
            2,
            "SpecialDict",
            420.0,
            true,
            cache.theme(),
        )
        .expect("SpecialDict doc should render");
        assert!(other_doc.plain_text.contains("main meaning"));
        assert!(
            other_doc.plain_text.contains("secret example"),
            "examples must stay visible for non-matching dictionary, got: {:?}",
            other_doc.plain_text
        );
        assert!(
            !other_doc.plain_text.contains("extra note"),
            "li[data-dictionary=\"SpecialDict\"] normalization must hide note, got: {:?}",
            other_doc.plain_text
        );
    }

    #[test]
    fn split_popup_and_outer_css_supports_2col_pitch_dict_colors_freq_and_nested_rules() {
        let outer = r#"
            iframe.yomitan-popup {
                background: rgba(45, 45, 55, 0.85) !important;
                border-radius: 12px !important;
                border: 1px solid rgba(255, 255, 255, 0.1) !important;
                box-shadow: 0 5px 15px rgba(0, 0, 0, 0.3);
                width: 760px;
            }
        "#;
        let inner = r#"
            body {
                background: transparent !important;
                color: #dfdfdf;
            }
            :root {
                --font-family: "UD デジタル 教科書体 NK-R";
                // --font-family: "英椎楷書";
                --definition-columns: 2;
                --definition-gap: 10px;
                --card-border: #333333;
                --card-border-width: 2px;
                --card-radius: 10px;
                --card-padding: 10px;
                --pitch-heiban-color: #59b2ff;
                --pitch-atamadaka-color: #ff6666;
                --pitch-kifuku-color: #af85f4;
            }
            .term-tags { display: none; }
            .pitch-dict { display: none; }
            .kanji-extra { display: none; }
            .derivations { display: compact; }

            [data-dictionary="デジタル大辞泉"] {
                --dict-color: rgb(170, 0, 0);
                --dict-bg-opacity: 0.10;
            }
            [data-dictionary*="JMdict"] {
                --dict-color: rgb(0, 132, 255);
                --dict-bg-opacity: 0.05;
                --glossary-opacity: 0;
                --glossary-hover-opacity: 1;
                --glossary-solo-opacity: 1;
            }
            [data-frequency^="JPDB"] { background-color: #ff3e3d; color: #ffffff; }
            [data-frequency="CC100"] { background-color: #6238fa; color: #ffffff; }
        "#;

        let theme = parse_split_popup_css_theme(inner, outer);
        // Outer translucent background must NOT be clobbered by inner `body { background: transparent }`
        assert_eq!(theme.popup_bg[0..3], [45, 45, 55]);
        assert!((theme.popup_bg[3] as i32 - 217).abs() <= 2);
        assert_eq!(theme.popup_radius, 12.0);
        assert_eq!(theme.popup_border_width, 1.0);
        assert_eq!(theme.popup_shadow_blur, 15.0);
        assert_eq!(theme.popup_width, 760.0);

        // Inner typography, 2-col layout, and visibility flags
        assert_eq!(theme.font_family, "UD デジタル 教科書体 NK-R");
        assert_eq!(theme.text_color, [0xDF, 0xDF, 0xDF, 0xFF]);
        assert_eq!(theme.definition_columns, 2);
        assert_eq!(theme.definition_gap, 10.0);
        assert_eq!(theme.card_border_width, 2.0);
        assert_eq!(theme.card_radius, 10.0);
        assert_eq!(theme.card_padding, 10.0);
        assert!(!theme.show_term_tags);
        assert!(!theme.show_pitch_dict);
        assert!(!theme.show_kanji_extra);
        assert!(theme.compact_derivations);

        // Pitch colors
        assert_eq!(
            theme.resolve_headword_pitch_color(Some("heiban")),
            [0x59, 0xB2, 0xFF, 0xFF]
        );
        assert_eq!(
            theme.resolve_headword_pitch_color(Some("atamadaka")),
            [0xFF, 0x66, 0x66, 0xFF]
        );
        assert_eq!(
            theme.resolve_headword_pitch_color(Some("kifuku")),
            [0xAF, 0x85, 0xF4, 0xFF]
        );

        // Per-dictionary badge + tint + hover-to-reveal
        let daijisen = theme.resolve_dict_style("デジタル大辞泉");
        assert_eq!(daijisen.badge_bg, [170, 0, 0, 255]);
        assert_eq!(daijisen.card_bg[0..3], [170, 0, 0]);
        assert!((daijisen.card_bg[3] as i32 - 26).abs() <= 2);
        assert_eq!(daijisen.glossary_opacity, 1.0);

        let jmdict = theme.resolve_dict_style("JMdict [2025-11-01]");
        assert_eq!(jmdict.badge_bg, [0, 132, 255, 255]);
        assert_eq!(jmdict.glossary_opacity, 0.0);
        assert_eq!(jmdict.glossary_hover_opacity, 1.0);
        assert_eq!(jmdict.glossary_solo_opacity, 1.0);

        // Per-frequency colors
        assert_eq!(
            theme.resolve_freq_style("JPDB").0,
            [0xFF, 0x3E, 0x3D, 0xFF]
        );
        assert_eq!(
            theme.resolve_freq_style("CC100").0,
            [0x62, 0x38, 0xFA, 0xFF]
        );
    }

    #[test]
    fn nested_dictionary_css_can_hide_ruby_rt_furigana() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(
            dir.path().join(CUSTOM_POPUP_CSS_FILENAME),
            r#"
            [data-dictionary="明鏡国語辞典 第三版"] {
                .gloss-sc-ruby[data-sc-class="full"] .gloss-sc-rt {
                    display: none;
                }
            }
            "#,
        )
        .unwrap();

        let mut cache = StylesheetCache::new(dir.path());
        let res_dirs: Vec<PathBuf> = Vec::new();
        let raw = r#"[{
            "type": "structured-content",
            "content": {
                "tag": "ruby",
                "data": { "class": "full" },
                "content": ["漢字", { "tag": "rt", "content": "かんじ" }]
            }
        }]"#;
        let sheet = cache.get_for_dictionary(&res_dirs, 1, "明鏡国語辞典 第三版");
        let doc = render_structured_glossary_entry_with_theme(
            raw,
            "",
            sheet,
            &HashMap::new(),
            LegacyGlossaryStyle::Bullet,
            &res_dirs,
            1,
            "明鏡国語辞典 第三版",
            400.0,
            true,
            cache.theme(),
        )
        .expect("ruby doc should render");
        assert_eq!(doc.plain_text, "漢字");
        // When <rt> is hidden via display: none, it renders as plain text rather than a Ruby InlineBox with tooltip
        assert!(
            doc.hit_zones.is_empty(),
            "hidden <rt> should suppress ruby box tooltip hit zone, got {:?}",
            doc.hit_zones.len()
        );
    }

    #[test]
    fn root_data_theme_dark_and_light_select_distinct_vars_and_compact_glossaries() {
        let css = r#"
            :root[data-theme="light"] {
                --bg: #fafafa;
                --fg: #111111;
                --compact-glossaries: true;
            }
            :root[data-theme="dark"] {
                --bg: #101216;
                --fg: #eef2f6;
            }
            :root {
                --bg: #333333;
                --fg: #cccccc;
                --background-color: var(--bg);
                --text-color: var(--fg);
            }
        "#;

        let dark = parse_popup_css_theme_with_theme(css, CssTheme::Dark);
        assert_eq!(dark.css_theme, CssTheme::Dark);
        assert!(!dark.is_light_theme());
        assert_eq!(dark.popup_bg, [0x10, 0x12, 0x16, 0xFF]);
        assert_eq!(dark.text_color, [0xEE, 0xF2, 0xF6, 0xFF]);
        assert!(!dark.compact_glossaries);
        assert_eq!(dark.render_cache_key(), (CssTheme::Dark, false));

        let light = parse_popup_css_theme_for_mode(css, true);
        assert_eq!(light.css_theme, CssTheme::Light);
        assert!(light.is_light_theme());
        assert_eq!(light.popup_bg, [0xFA, 0xFA, 0xFA, 0xFF]);
        assert_eq!(light.text_color, [0x11, 0x11, 0x11, 0xFF]);
        assert!(light.compact_glossaries);
        assert_eq!(light.render_cache_key(), (CssTheme::Light, true));
    }

    #[test]
    fn stylesheet_cache_scopes_per_dict_custom_vars_and_invalidates_on_theme_toggle() {
        let cfg_dir = tempfile::TempDir::new().unwrap();
        let res_dir = tempfile::TempDir::new().unwrap();

        // Dict 1 defines `--accent` in dark & light themes; Dict 2 defines its OWN `--accent`
        // (or omits it) so Dict 1's variables must NEVER leak into Dict 2 (#1907).
        let dict1_dir = res_dir.path().join("dict-1");
        let dict2_dir = res_dir.path().join("dict-2");
        std::fs::create_dir_all(&dict1_dir).unwrap();
        std::fs::create_dir_all(&dict2_dir).unwrap();
        std::fs::write(
            dict1_dir.join("styles.css"),
            r#"
            :root[data-theme="dark"] { --accent: #ff0000; }
            :root[data-theme="light"] { --accent: #0000ff; }
            span[data-sc-k="hl"] { color: var(--accent); }
            "#,
        )
        .unwrap();
        std::fs::write(
            dict2_dir.join("styles.css"),
            r#"
            :root { --accent: #00aa00; }
            span[data-sc-k="hl"] { color: var(--accent); }
            "#,
        )
        .unwrap();

        let res_dirs = vec![res_dir.path().to_path_buf()];
        let mut cache = StylesheetCache::new_with_theme(cfg_dir.path(), CssTheme::Dark);
        let rev_dark = cache.user_css_revision();
        let sheet1_dark = cache
            .get_for_dictionary(&res_dirs, 1, "DictOne")
            .expect("dict1 sheet");
        let sheet2_dark = cache
            .get_for_dictionary(&res_dirs, 2, "DictTwo")
            .expect("dict2 sheet");
        assert_eq!(
            sheet1_dark.root_variable("--accent", CssTheme::Dark),
            Some("#ff0000")
        );
        assert_eq!(
            sheet2_dark.root_variable("--accent", CssTheme::Dark),
            Some("#00aa00")
        );
        assert_ne!(
            sheet1_dark.root_variable("--accent", CssTheme::Dark),
            sheet2_dark.root_variable("--accent", CssTheme::Dark),
            "per-dict custom properties must not leak across dictionaries"
        );

        // Toggling theme must bump revision and return a freshly resolved theme/stylesheet
        assert!(cache.set_light_theme(true));
        assert_eq!(cache.active_theme(), CssTheme::Light);
        assert!(cache.user_css_revision() > rev_dark);
        let sheet1_light = cache
            .get_for_dictionary(&res_dirs, 1, "DictOne")
            .expect("dict1 light sheet");
        assert_eq!(
            sheet1_light.root_variable("--accent", CssTheme::Light),
            Some("#0000ff")
        );
        // No-op when setting same theme
        assert!(!cache.set_theme(CssTheme::Light));
    }
}

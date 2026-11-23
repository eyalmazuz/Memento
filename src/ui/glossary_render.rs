//! Rich structured-content glossary layout & rasterizer (`subomi-definition` + `parley` + `swash`).
//! Operates directly on `&serde_json::Value` (zero `DefJson` conversion) with physical-DPI
//! pixel-exact rendering and light-on-dark stem contrast preservation.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use subomi_definition::{
    self as mdef,
    element::{BOTTOM, LEFT, RIGHT, TOP},
    parse_css_color, split_ruby_content_value, structured_content_text_value,
    Context as DefContext, CssTheme, ElementLayout, LayoutDirection, LegacyGlossaryStyle,
    ParsedStylesheet, Renderer as DefRenderer, ScreenMetrics, SiblingState,
};
use parley::{
    Alignment, AlignmentOptions, FontFamily, FontStyle, FontWeight, InlineBox, LineHeight,
    PositionedLayoutItem, StyleProperty,
};
use resvg::tiny_skia;
use serde_json::Value as JsonValue;
use slint::{Rgba8Pixel, SharedPixelBuffer};

use super::dict_assets::{
    load_dictionary_image_pixmap_scaled_with_engine, resolve_dictionary_asset_path_in_dirs,
};
use super::popup_css::{
    clean_css_font_family, parse_css_theme_color, parse_css_unit_float, rgba_to_css_color,
    PopupCssTheme,
};
use super::subtitle_layout::is_japanese_lookup_char;
use super::text_engine::{
    blit_alpha_glyph, composite_rgb_over_opaque, pixmap_to_pixel_buffer, quantize_subpixel_x,
    with_shared_text_engine, BorrowFontExt, PositionedMaskGlyph, TextEngine,
    RICH_EMOJI_FALLBACK_STACK,
};
use super::RichSelectionRect;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GlossaryZoneAction {
    Link(String),
    ToggleDetails(String),
}

#[derive(Clone, Debug)]
pub struct GlossaryHitZone {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub action: Option<GlossaryZoneAction>,
    pub tooltip: Option<String>,
}

#[derive(Clone, Debug)]
pub struct GlossaryCharCell {
    pub ch: char,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub line_id: usize,
}

#[derive(Clone)]
pub struct RenderedGlossaryDoc {
    pub width_px: f32,
    pub height_px: f32,
    pub image: slint::Image,
    pub hit_zones: Vec<GlossaryHitZone>,
    pub selectable_chars: Vec<GlossaryCharCell>,
    pub line_bottoms: Vec<f32>,
    pub plain_text: String,
}

/// Thread-safe (`Send`) pre-rasterized glossary payload produced off the UI thread
/// and wrapped into a `RenderedGlossaryDoc` on the UI thread with zero pixel copy.
pub(super) struct RasterizedGlossaryDoc {
    pub(super) width_px: f32,
    pub(super) height_px: f32,
    pub(super) pixels: SharedPixelBuffer<Rgba8Pixel>,
    pub(super) hit_zones: Vec<GlossaryHitZone>,
    pub(super) selectable_chars: Vec<GlossaryCharCell>,
    pub(super) line_bottoms: Vec<f32>,
    pub(super) plain_text: String,
}

impl RasterizedGlossaryDoc {
    pub(super) fn into_rendered_doc(self) -> RenderedGlossaryDoc {
        RenderedGlossaryDoc {
            width_px: self.width_px,
            height_px: self.height_px,
            image: slint::Image::from_rgba8_premultiplied(self.pixels),
            hit_zones: self.hit_zones,
            selectable_chars: self.selectable_chars,
            line_bottoms: self.line_bottoms,
            plain_text: self.plain_text,
        }
    }
}

pub const MAX_NESTED_POPUP_DEPTH: usize = 4;
#[allow(dead_code)]
pub(super) const GLOSSARY_SCAN_MAX_CHARS: usize = 24;
#[allow(dead_code)]
pub(super) const NESTED_DISMISS_GRACE_MS: u64 = 260;

fn is_japanese_scan_start_char(ch: char) -> bool {
    is_japanese_lookup_char(ch) && ch != '\u{30fb}' && ch != '\u{30fc}'
}

fn is_japanese_scan_continue_char(ch: char) -> bool {
    is_japanese_lookup_char(ch) && ch != '\u{30fb}'
}

/// Strict bounding-box hit-test over `RenderedGlossaryDoc::selectable_chars` for
/// hover-scanning inside glossary definitions (returns `None` on blank margins/empty space).
pub fn strict_char_index_at_point(doc: &RenderedGlossaryDoc, x: f32, y: f32) -> Option<usize> {
    if doc.selectable_chars.is_empty() {
        return None;
    }
    let mut best: Option<(usize, bool, f32)> = None;
    for (idx, cell) in doc.selectable_chars.iter().enumerate() {
        if cell.ch == '\n' || cell.w <= 0.0 || cell.h <= 0.0 {
            continue;
        }
        let is_line_end = doc
            .selectable_chars
            .get(idx + 1)
            .is_none_or(|next| next.ch == '\n' || next.line_id != cell.line_id);
        let pad_left = 1.5f32;
        // Slightly widen the right hit-edge of the final visible character on a line so
        // scanning the last word of a glossary line without trailing punctuation (#2232)
        // resolves cleanly while blank margins still return `None`.
        let pad_right = if is_line_end {
            (cell.w * 0.25).clamp(2.0, 4.5)
        } else {
            1.5f32
        };
        let pad_y = 1.5f32;
        if x < cell.x - pad_left
            || x > cell.x + cell.w + pad_right
            || y < cell.y - pad_y
            || y > cell.y + cell.h + pad_y
        {
            continue;
        }
        let strictly_inside =
            x >= cell.x && x <= cell.x + cell.w && y >= cell.y && y <= cell.y + cell.h;
        let cx = cell.x + cell.w * 0.5;
        let cy = cell.y + cell.h * 0.5;
        let dist = (x - cx).hypot(y - cy);
        let is_better = match best {
            None => true,
            Some((_, best_inside, best_dist)) => {
                (strictly_inside && !best_inside)
                    || (strictly_inside == best_inside && dist < best_dist)
            }
        };
        if is_better {
            best = Some((idx, strictly_inside, dist));
        }
    }
    best.map(|(idx, _, _)| idx)
}

/// Extract a contiguous Japanese scan substring (up to `max_chars`) starting at `start_idx`
/// in `selectable_chars`, seamlessly bridging soft line wraps inside the same paragraph while
/// stopping at punctuation, whitespace, non-Japanese characters, or block breaks.
/// Returns `(substring, cell_indices)` where each char maps 1:1 back to its cell index.
pub fn extract_glossary_scan_substring(
    chars: &[GlossaryCharCell],
    start_idx: usize,
    max_chars: usize,
) -> Option<(String, Vec<usize>)> {
    let first = chars.get(start_idx)?;
    if !is_japanese_scan_start_char(first.ch) {
        return None;
    }
    let limit = max_chars.max(1);
    let mut query = String::new();
    let mut indices = Vec::new();
    let mut prev_cell = first;
    query.push(first.ch);
    indices.push(start_idx);

    let mut i = start_idx + 1;
    while i < chars.len() && indices.len() < limit {
        let cell = &chars[i];
        if cell.ch == '\n' {
            // Bridge a single soft visual wrap (`cell.h > 0.0`) within the same paragraph
            // when the next visible character continues the Japanese run without a paragraph gap.
            // Hard line/paragraph breaks emit `h == 0.0` so the final word of a line without
            // trailing punctuation terminates cleanly (#2232).
            if cell.h > 0.0 {
                if let Some(next_cell) = chars.get(i + 1) {
                    let vertical_step = next_cell.y - prev_cell.y;
                    if is_japanese_scan_continue_char(next_cell.ch)
                        && vertical_step >= 0.0
                        && vertical_step <= prev_cell.h.max(14.0) * 1.65
                    {
                        i += 1;
                        continue;
                    }
                }
            }
            break;
        }
        if !is_japanese_scan_continue_char(cell.ch) {
            break;
        }
        query.push(cell.ch);
        indices.push(i);
        prev_cell = cell;
        i += 1;
    }

    (!query.is_empty()).then_some((query, indices))
}

#[allow(dead_code)]
pub(super) fn utf16_units_to_char_count(text: &str, utf16_units: usize) -> usize {
    if utf16_units == 0 {
        return 0;
    }
    let mut acc = 0usize;
    let mut count = 0usize;
    for ch in text.chars() {
        acc += ch.len_utf16();
        count += 1;
        if acc >= utf16_units {
            break;
        }
    }
    count
}

#[allow(dead_code)]
pub(super) fn build_selection_rects_for_cells(
    chars: &[GlossaryCharCell],
    indices: &[usize],
) -> Vec<RichSelectionRect> {
    let mut rects = Vec::new();
    let mut cur: Option<(usize, f32, f32, f32, f32)> = None;
    for &idx in indices {
        let Some(cell) = chars.get(idx) else {
            continue;
        };
        if cell.ch == '\n' || cell.w <= 0.0 {
            continue;
        }
        match cur {
            Some((lid, sx, sy, sw, sh)) if lid == cell.line_id => {
                let nx = sx.min(cell.x);
                let nw = (sx + sw).max(cell.x + cell.w) - nx;
                cur = Some((lid, nx, sy.min(cell.y), nw, sh.max(cell.h)));
            }
            Some((_, sx, sy, sw, sh)) => {
                rects.push(RichSelectionRect {
                    x_px: sx,
                    y_px: sy,
                    w_px: sw,
                    h_px: sh,
                });
                cur = Some((cell.line_id, cell.x, cell.y, cell.w, cell.h));
            }
            None => {
                cur = Some((cell.line_id, cell.x, cell.y, cell.w, cell.h));
            }
        }
    }
    if let Some((_, sx, sy, sw, sh)) = cur {
        rects.push(RichSelectionRect {
            x_px: sx,
            y_px: sy,
            w_px: sw,
            h_px: sh,
        });
    }
    rects
}

fn compute_visual_line_bottoms(bounds: &[(f32, f32)]) -> Vec<f32> {
    if bounds.is_empty() {
        return Vec::new();
    }
    let mut sorted: Vec<(f32, f32)> = bounds
        .iter()
        .copied()
        .filter(|&(top, bottom)| top.is_finite() && bottom.is_finite() && bottom > top)
        .collect();
    sorted.sort_by(|a, b| {
        a.0.partial_cmp(&b.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
    });
    let mut merged: Vec<(f32, f32)> = Vec::new();
    for (top, bottom) in sorted {
        if let Some(last) = merged.last_mut() {
            // Merge horizontally adjacent table cells sharing the same visual row band.
            if (top - last.0).abs() <= 6.0 && top < last.1 - 3.0 {
                last.1 = last.1.max(bottom);
                continue;
            }
        }
        merged.push((top, bottom));
    }
    merged.into_iter().map(|(_, bottom)| bottom).collect()
}

/// Compute `(collapsible, collapsed_height_px, total_lines)` for a rendered glossary card
/// against its resolved `--collapsible-definition-line-count` limit (`0` = disabled).
pub fn compute_collapsible_metrics(
    doc: &RenderedGlossaryDoc,
    line_limit: usize,
) -> (bool, f32, i32) {
    let total_lines = doc.line_bottoms.len() as i32;
    if !doc.height_px.is_finite() || doc.height_px <= 0.0 {
        return (false, 0.0, total_lines);
    }
    if line_limit == 0 || doc.line_bottoms.len() <= line_limit {
        return (false, doc.height_px, total_lines);
    }
    let raw_bottom = doc.line_bottoms[line_limit - 1];
    if !raw_bottom.is_finite() {
        return (false, doc.height_px, total_lines);
    }
    let min_h = 18.0f32.min(doc.height_px);
    let clamped_h = (raw_bottom + 4.0).ceil().clamp(min_h, doc.height_px);
    if clamped_h + 4.0 >= doc.height_px {
        (false, doc.height_px, total_lines)
    } else {
        (true, clamped_h, total_lines)
    }
}

fn parse_css_invert_ratio(filter_value: &str) -> Option<f32> {
    let lower = filter_value.to_ascii_lowercase();
    let pos = lower.find("invert(")?;
    let rest = &lower[pos + 7..];
    let end = rest.find(')')?;
    let arg = rest[..end].trim();
    if arg.is_empty() {
        return Some(1.0);
    }
    parse_css_unit_float(arg)
}

fn invert_pixmap(src: &tiny_skia::Pixmap, ratio: f32) -> Arc<tiny_skia::Pixmap> {
    let ratio = ratio.clamp(0.0, 1.0);
    let mut out = src.clone();
    for px in out.pixels_mut() {
        let a = px.alpha();
        if a == 0 {
            continue;
        }
        let af = f32::from(a) / 255.0;
        let inv_chan = |c: u8| -> u8 {
            let unp = (f32::from(c) / af).clamp(0.0, 255.0);
            let blended = unp * (1.0 - ratio) + (255.0 - unp) * ratio;
            (blended * af).round().clamp(0.0, f32::from(a)) as u8
        };
        if let Some(p) = tiny_skia::PremultipliedColorU8::from_rgba(
            inv_chan(px.red()),
            inv_chan(px.green()),
            inv_chan(px.blue()),
            a,
        ) {
            *px = p;
        }
    }
    Arc::new(out)
}

#[derive(Clone, Debug)]
struct InlineStyleIR {
    font_family: String,
    font_px: f32,
    weight: u16,
    italic: bool,
    underline: bool,
    strikethrough: bool,
    color: [u8; 4],
    pill_bg: Option<[u8; 4]>,
    pill_border: Option<[u8; 4]>,
    link_href: Option<String>,
    tooltip: Option<String>,
}

impl Default for InlineStyleIR {
    fn default() -> Self {
        Self {
            font_family: "Noto Sans CJK JP".to_owned(),
            font_px: 14.5,
            weight: 400,
            italic: false,
            underline: false,
            strikethrough: false,
            color: [241, 245, 249, 255],
            pill_bg: None,
            pill_border: None,
            link_href: None,
            tooltip: None,
        }
    }
}

#[derive(Clone)]
enum InlineAtom {
    Text {
        text: String,
        style: InlineStyleIR,
        selectable: bool,
    },
    Ruby {
        base: String,
        reading: String,
        style: InlineStyleIR,
    },
    Image {
        pixmap: Arc<tiny_skia::Pixmap>,
        w: f32,
        h: f32,
        link_href: Option<String>,
        tooltip: Option<String>,
    },
    DetailsToggle {
        detail_id: String,
        open: bool,
        style: InlineStyleIR,
    },
    LineBreak,
}

#[derive(Clone, Default)]
struct BoxFrameIR {
    padding: [f32; 4],
    margin_top: f32,
    margin_bottom: f32,
    margin_left: f32,
    margin_right: f32,
    bg_color: Option<[u8; 4]>,
    borders: [Option<(f32, [u8; 4])>; 4],
}

#[derive(Clone)]
struct TableCellIR {
    col_span: usize,
    row_span: usize,
    blocks: Vec<BlockIR>,
    frame: BoxFrameIR,
}

#[derive(Clone)]
enum BlockIR {
    Paragraph {
        atoms: Vec<InlineAtom>,
        frame: BoxFrameIR,
        marker: Option<(String, [u8; 4], f32)>,
        summary_detail_id: Option<String>,
    },
    Container {
        blocks: Vec<BlockIR>,
        frame: BoxFrameIR,
        marker: Option<(String, [u8; 4], f32)>,
    },
    Table {
        rows: Vec<Vec<TableCellIR>>,
        frame: BoxFrameIR,
    },
}

fn parse_def_rgba(value: &str, fallback: [u8; 4]) -> [u8; 4] {
    parse_css_color(value).unwrap_or(fallback)
}

fn parse_def_rgba_opt(value: &str) -> Option<[u8; 4]> {
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("transparent") {
        return None;
    }
    parse_css_color(trimmed)
}

fn parse_weight_css(value: Option<&String>, default_weight: u16) -> u16 {
    let Some(v) = value else {
        return default_weight;
    };
    let lower = v.trim().to_ascii_lowercase();
    match lower.as_str() {
        "bold" | "bolder" => 700,
        "normal" => 400,
        "light" | "lighter" => 300,
        _ => lower.parse::<u16>().unwrap_or(default_weight).clamp(100, 900),
    }
}

/// Extract `<ruby>` `(base_text, reading)` pairs supporting both `<rp>` and `<rp>`-less
/// single or multi-segment Yomitan/Jitendex `<ruby>` nodes directly from `&serde_json::Value`.
fn extract_ruby_pairs(ruby_content: &JsonValue) -> Vec<(String, String)> {
    let children = match ruby_content {
        JsonValue::Array(items) => items.as_slice(),
        _ => std::slice::from_ref(ruby_content),
    };
    let mut pairs: Vec<(String, String)> = Vec::new();
    let mut current_base = String::new();
    for child in children {
        if child.is_object() {
            if let Some(tag) = child.get("tag").and_then(JsonValue::as_str) {
                if tag.eq_ignore_ascii_case("rt") {
                    let mut rt_text = String::new();
                    if let Some(content) = child.get("content") {
                        structured_content_text_value(content, &mut rt_text);
                    }
                    pairs.push((std::mem::take(&mut current_base), rt_text));
                    continue;
                }
                if tag.eq_ignore_ascii_case("rp") {
                    continue;
                }
            }
        }
        structured_content_text_value(child, &mut current_base);
    }
    if !current_base.is_empty() {
        pairs.push((current_base, String::new()));
    }
    if pairs.is_empty() {
        let parts = split_ruby_content_value(ruby_content);
        let mut base = String::new();
        for node in &parts.base {
            structured_content_text_value(node, &mut base);
        }
        if !base.is_empty() {
            pairs.push((base, parts.reading));
        }
    }
    pairs
}

struct GlossaryBuilder<'a> {
    engine: &'a mut TextEngine,
    renderer: DefRenderer,
    ctx: DefContext,
    dict_resource_dirs: &'a [PathBuf],
    dic_id: i64,
    dict_name: &'a str,
    default_link_color: [u8; 4],
    is_light_theme: bool,
    compact_glossaries: bool,
    render_scale: f32,
    had_hidden_elements: bool,
}

impl<'a> GlossaryBuilder<'a> {
    fn box_frame_from_state(&self, state: &mdef::ElementRenderState) -> BoxFrameIR {
        let mut borders = [None; 4];
        let default_border_rgba = if self.is_light_theme {
            [148, 163, 184, 220]
        } else {
            [100, 116, 139, 200]
        };
        for side in 0..4 {
            let b = &state.box_style.borders[side];
            if !b.style.eq_ignore_ascii_case("none")
                && !b.style.is_empty()
                && b.width_pixels > 0.05
            {
                let rgba = parse_def_rgba(&b.color, default_border_rgba);
                borders[side] = Some(((b.width_pixels as f32).clamp(1.0, 6.0), rgba));
            }
        }
        let font_px = state.font_size_px.unwrap_or(state.font_pixel_size);
        let margin_px = |side: usize| -> f32 {
            let v = mdef::helpers::css_font_size_to_pixels(
                &state.box_style.margins[side],
                self.ctx.screen.as_ref(),
                font_px,
                self.ctx.root_font_pixel_size,
            );
            if v.is_finite() && v > 0.0 {
                (v as f32).min(24.0)
            } else {
                0.0
            }
        };
        BoxFrameIR {
            padding: [
                state.box_style.padding[TOP] as f32,
                state.box_style.padding[RIGHT] as f32,
                state.box_style.padding[BOTTOM] as f32,
                state.box_style.padding[LEFT] as f32,
            ],
            margin_top: margin_px(TOP),
            margin_right: margin_px(RIGHT),
            margin_bottom: margin_px(BOTTOM),
            margin_left: margin_px(LEFT),
            bg_color: parse_def_rgba_opt(&state.box_style.background_color),
            borders,
        }
    }

    fn inline_style_from_state(
        &self,
        state: &mdef::ElementRenderState,
        inherited: &InlineStyleIR,
    ) -> InlineStyleIR {
        let mut next = inherited.clone();
        let resolved_font_px = state.font_size_px.unwrap_or(state.font_pixel_size);
        if resolved_font_px > 0.0 {
            next.font_px = (resolved_font_px as f32).clamp(9.0, 32.0);
        }
        if let Some(ff) = state.declarations.get("font-family") {
            let cleaned = clean_css_font_family(ff);
            if !cleaned.is_empty() {
                next.font_family = cleaned;
            }
        }
        if !state.text_color.is_empty() {
            next.color = parse_def_rgba(&state.text_color, inherited.color);
        }
        next.weight = parse_weight_css(state.declarations.get("font-weight"), inherited.weight);
        if let Some(fs) = state.declarations.get("font-style") {
            let l = fs.trim().to_ascii_lowercase();
            if l == "italic" || l == "oblique" {
                next.italic = true;
            } else if l == "normal" {
                next.italic = false;
            }
        }
        if let Some(td) = state.declarations.get("text-decoration") {
            let l = td.to_ascii_lowercase();
            if l.contains("none") {
                next.underline = false;
                next.strikethrough = false;
            } else {
                if l.contains("underline") {
                    next.underline = true;
                }
                if l.contains("line-through") {
                    next.strikethrough = true;
                }
            }
        }
        if !state.title_tooltip.is_empty() {
            next.tooltip = Some(state.title_tooltip.clone());
        }
        if !self.ctx.link_href.is_empty() {
            next.link_href = Some(self.ctx.link_href.clone());
        }
        if state.tag == "span" {
            if let Some(bg) = parse_def_rgba_opt(&state.box_style.background_color).or_else(|| {
                state
                    .declarations
                    .get("background-color")
                    .and_then(|s| parse_def_rgba_opt(s))
            }) {
                next.pill_bg = Some(bg);
            }
            for side in 0..4 {
                if state.box_style.borders[side].width_pixels > 0.05 {
                    next.pill_border = Some(parse_def_rgba(
                        &state.box_style.borders[side].color,
                        next.color,
                    ));
                    break;
                }
            }
        }
        next
    }

    fn resolve_image_atom(
        &mut self,
        obj: &JsonValue,
        inherited: &InlineStyleIR,
    ) -> Option<InlineAtom> {
        let index = self
            .renderer
            .structured_element_value(obj, &mut self.ctx, false);
        self.ctx.elements.push(index);

        let mut declarations = mdef::CssDeclarations::new();
        self.renderer
            .add_matching_css_rules(&mut self.ctx, &mut declarations, None, None);
        if declarations
            .get("display")
            .is_some_and(|v| v.trim().eq_ignore_ascii_case("none"))
        {
            self.had_hidden_elements = true;
            self.ctx.elements.pop();
            return None;
        }

        let rel_path = obj.get("path").and_then(JsonValue::as_str).unwrap_or("");
        let units = obj
            .get("sizeUnits")
            .and_then(JsonValue::as_str)
            .filter(|u| !u.is_empty())
            .unwrap_or("px");
        let dim_px = |key: &str, ctx: &DefContext| -> Option<f32> {
            if let Some(v) = obj.get(key).and_then(JsonValue::as_f64) {
                let css_sz = format!("{v}{units}");
                let px = mdef::helpers::css_font_size_to_pixels(
                    &css_sz,
                    ctx.screen.as_ref(),
                    ctx.parent_font_pixel_size,
                    ctx.root_font_pixel_size,
                );
                if px > 0.0 {
                    return Some(px as f32);
                }
            }
            None
        };
        let req_w = dim_px("width", &self.ctx);
        let req_h = dim_px("height", &self.ctx);
        let is_monochrome = obj
            .get("appearance")
            .and_then(JsonValue::as_str)
            .is_some_and(|s| s.eq_ignore_ascii_case("monochrome"));
        let color_str = declarations
            .get("color")
            .cloned()
            .unwrap_or_else(|| self.ctx.text_color.clone());
        let tint = is_monochrome.then(|| parse_def_rgba(&color_str, inherited.color));

        let tooltip = obj
            .get("title")
            .and_then(JsonValue::as_str)
            .filter(|t| !t.is_empty())
            .map(str::to_owned)
            .or_else(|| inherited.tooltip.clone());
        let link_href = (!self.ctx.link_href.is_empty())
            .then(|| self.ctx.link_href.clone())
            .or_else(|| inherited.link_href.clone());

        // Check `filter: invert(...)` across matching stylesheet rules (e.g. dark-mode gaiji images)
        let mut invert_ratio = 0.0f32;
        if let Some(sheet) = self.ctx.dictionary_styles.clone() {
            for rule in &sheet.rules {
                if !self.renderer.css_rule_matches(rule, &self.ctx) {
                    continue;
                }
                for decl in &rule.declarations {
                    if decl.property.eq_ignore_ascii_case("filter")
                        || decl.property.eq_ignore_ascii_case("-webkit-filter")
                    {
                        if let Some(r) = parse_css_invert_ratio(&decl.value) {
                            invert_ratio = r;
                        }
                    }
                }
            }
        }

        self.ctx.elements.pop();

        let asset_path = resolve_dictionary_asset_path_in_dirs(
            self.dict_resource_dirs,
            self.dic_id,
            self.dict_name,
            rel_path,
        )?;
        let (mut pixmap, w, h) = load_dictionary_image_pixmap_scaled_with_engine(
            self.engine,
            &asset_path,
            req_w,
            req_h,
            tint,
            &color_str,
            self.render_scale,
        )?;
        if invert_ratio > 0.001 && !is_monochrome {
            pixmap = invert_pixmap(&pixmap, invert_ratio);
        }
        Some(InlineAtom::Image {
            pixmap,
            w,
            h,
            link_href,
            tooltip,
        })
    }

    fn collect_table_rows(
        &mut self,
        node: &JsonValue,
        inherited: &InlineStyleIR,
        rows_out: &mut Vec<Vec<TableCellIR>>,
    ) {
        match node {
            JsonValue::Array(items) => {
                for item in items {
                    self.collect_table_rows(item, inherited, rows_out);
                }
            }
            JsonValue::Object(_) => {
                let tag = node
                    .get("tag")
                    .and_then(JsonValue::as_str)
                    .unwrap_or("")
                    .to_ascii_lowercase();
                match tag.as_str() {
                    "thead" | "tbody" | "tfoot" => {
                        let idx = self
                            .renderer
                            .structured_element_value(node, &mut self.ctx, false);
                        self.ctx.elements.push(idx);
                        let (mut state, hidden) = self.build_element_state_checked(node, &tag);
                        if hidden {
                            self.ctx.elements.pop();
                            return;
                        }
                        let next_style = self.inline_style_from_state(&state, inherited);
                        std::mem::swap(
                            &mut self.ctx.parent_font_pixel_size,
                            &mut state.font_pixel_size,
                        );
                        std::mem::swap(&mut self.ctx.text_color, &mut state.text_color);
                        self.ctx.siblings.push(SiblingState::default());
                        if let Some(content) = node.get("content") {
                            self.collect_table_rows(content, &next_style, rows_out);
                        }
                        self.ctx.siblings.pop();
                        std::mem::swap(&mut self.ctx.text_color, &mut state.text_color);
                        std::mem::swap(
                            &mut self.ctx.parent_font_pixel_size,
                            &mut state.font_pixel_size,
                        );
                        self.ctx.elements.pop();
                    }
                    "tr" => {
                        let idx = self
                            .renderer
                            .structured_element_value(node, &mut self.ctx, false);
                        self.ctx.elements.push(idx);
                        let (mut state, hidden) = self.build_element_state_checked(node, "tr");
                        if hidden {
                            self.ctx.elements.pop();
                            return;
                        }
                        let row_style = self.inline_style_from_state(&state, inherited);
                        std::mem::swap(
                            &mut self.ctx.parent_font_pixel_size,
                            &mut state.font_pixel_size,
                        );
                        std::mem::swap(&mut self.ctx.text_color, &mut state.text_color);
                        self.ctx.siblings.push(SiblingState::default());
                        let mut cells = Vec::new();
                        if let Some(content) = node.get("content") {
                            self.collect_tr_cells(content, &row_style, &mut cells);
                        }
                        self.ctx.siblings.pop();
                        std::mem::swap(&mut self.ctx.text_color, &mut state.text_color);
                        std::mem::swap(
                            &mut self.ctx.parent_font_pixel_size,
                            &mut state.font_pixel_size,
                        );
                        self.ctx.elements.pop();
                        if !cells.is_empty() {
                            rows_out.push(cells);
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    fn collect_tr_cells(
        &mut self,
        node: &JsonValue,
        inherited: &InlineStyleIR,
        cells_out: &mut Vec<TableCellIR>,
    ) {
        match node {
            JsonValue::Array(items) => {
                for item in items {
                    self.collect_tr_cells(item, inherited, cells_out);
                }
            }
            JsonValue::Object(_) => {
                let tag = node
                    .get("tag")
                    .and_then(JsonValue::as_str)
                    .unwrap_or("")
                    .to_ascii_lowercase();
                if tag != "td" && tag != "th" {
                    return;
                }
                let idx = self
                    .renderer
                    .structured_element_value(node, &mut self.ctx, false);
                self.ctx.elements.push(idx);
                let (mut state, hidden) = self.build_element_state_checked(node, &tag);
                if hidden {
                    self.ctx.elements.pop();
                    return;
                }
                // Ensure table cells retain their default 5px padding & 1px border when resolve_box_style skipped Inline
                self.renderer.resolve_box_style(&self.ctx, &mut state);
                let mut cell_style = self.inline_style_from_state(&state, inherited);
                if tag == "th" && cell_style.weight < 600 {
                    cell_style.weight = 700;
                }
                let mut frame = self.box_frame_from_state(&state);
                for p in &mut frame.padding {
                    if *p < 4.0 {
                        *p = 5.0;
                    }
                }
                let default_table_border = if self.is_light_theme {
                    [203, 213, 225, 220]
                } else {
                    [82, 97, 107, 180]
                };
                for b in &mut frame.borders {
                    if b.is_none() {
                        *b = Some((1.0, default_table_border));
                    }
                }
                if tag == "th" && frame.bg_color.is_none() {
                    frame.bg_color = Some(if self.is_light_theme {
                        [238, 242, 246, 235]
                    } else {
                        [43, 50, 56, 220]
                    });
                }

                let col_span = node
                    .get("colSpan")
                    .and_then(JsonValue::as_f64)
                    .filter(|&n| n >= 1.0)
                    .map_or(1, |n| (n as usize).min(16));
                let row_span = node
                    .get("rowSpan")
                    .and_then(JsonValue::as_f64)
                    .filter(|&n| n >= 1.0)
                    .map_or(1, |n| (n as usize).min(32));

                std::mem::swap(
                    &mut self.ctx.parent_font_pixel_size,
                    &mut state.font_pixel_size,
                );
                std::mem::swap(&mut self.ctx.text_color, &mut state.text_color);
                let blocks = match node.get("content") {
                    Some(content) => self.build_children_blocks(content, &cell_style),
                    None => Vec::new(),
                };
                std::mem::swap(&mut self.ctx.text_color, &mut state.text_color);
                std::mem::swap(
                    &mut self.ctx.parent_font_pixel_size,
                    &mut state.font_pixel_size,
                );
                self.ctx.elements.pop();

                cells_out.push(TableCellIR {
                    col_span,
                    row_span,
                    blocks,
                    frame,
                });
            }
            _ => {}
        }
    }

    fn build_element_state_checked(
        &mut self,
        obj: &JsonValue,
        tag: &str,
    ) -> (mdef::ElementRenderState, bool) {
        let mut state = mdef::ElementRenderState {
            tag: tag.to_owned(),
            output_tag: tag.to_owned(),
            font_pixel_size: self.ctx.parent_font_pixel_size,
            text_color: self.ctx.text_color.clone(),
            title_tooltip: self.ctx.title_tooltip.clone(),
            text_direction: self.ctx.text_direction,
            painted_background_color: self.ctx.painted_background_color.clone(),
            text_only_content: obj.get("content").is_some_and(JsonValue::is_string),
            ..mdef::ElementRenderState::default()
        };
        if tag == "summary" && self.ctx.elements.len() > 1 && !self.ctx.details.is_empty() {
            let parent_idx = self.ctx.elements[self.ctx.elements.len() - 2] as usize;
            if self.ctx.selector_elements[parent_idx].tag == "details" {
                state.disclosure_summary = true;
                state.details = self.ctx.details.last().unwrap().clone();
            }
        }
        self.renderer.add_element_attributes_value(obj, &mut state);
        self.renderer
            .resolve_element_styles_value(obj, &mut self.ctx, &mut state);
        if state
            .declarations
            .get("display")
            .is_some_and(|v| v.trim().eq_ignore_ascii_case("none"))
        {
            self.had_hidden_elements = true;
            return (state, true);
        }
        self.renderer.resolve_element_layout(&mut self.ctx, &mut state);
        self.renderer.apply_element_compatibility(&self.ctx, &mut state);
        (state, false)
    }

    fn build_children_blocks(
        &mut self,
        content: &JsonValue,
        inherited: &InlineStyleIR,
    ) -> Vec<BlockIR> {
        let mut siblings = SiblingState::default();
        if self.ctx.needs_element_child_count
            && self
                .renderer
                .needs_structured_element_child_count_value(content, &self.ctx)
        {
            siblings.element_count = self.renderer.structured_element_child_count_value(content);
        }
        self.ctx.siblings.push(siblings);
        let mut blocks = Vec::new();
        let mut inline_buf: Vec<InlineAtom> = Vec::new();
        self.walk_node(content, inherited, &mut inline_buf, &mut blocks);
        if !inline_buf.is_empty() {
            blocks.push(BlockIR::Paragraph {
                atoms: inline_buf,
                frame: BoxFrameIR::default(),
                marker: None,
                summary_detail_id: None,
            });
        }
        self.ctx.siblings.pop();
        blocks
    }

    fn walk_node(
        &mut self,
        node: &JsonValue,
        inherited: &InlineStyleIR,
        inline_out: &mut Vec<InlineAtom>,
        blocks_out: &mut Vec<BlockIR>,
    ) {
        let flush_inline = |inline_out: &mut Vec<InlineAtom>, blocks_out: &mut Vec<BlockIR>| {
            if !inline_out.is_empty() {
                blocks_out.push(BlockIR::Paragraph {
                    atoms: std::mem::take(inline_out),
                    frame: BoxFrameIR::default(),
                    marker: None,
                    summary_detail_id: None,
                });
            }
        };

        match node {
            JsonValue::String(text) => {
                if !text.is_empty() {
                    let mut parts = text.split('\n').peekable();
                    while let Some(seg) = parts.next() {
                        if !seg.is_empty() {
                            inline_out.push(InlineAtom::Text {
                                text: seg.to_owned(),
                                style: inherited.clone(),
                                selectable: true,
                            });
                        }
                        if parts.peek().is_some() {
                            inline_out.push(InlineAtom::LineBreak);
                        }
                    }
                }
            }
            JsonValue::Array(items) => {
                for item in items {
                    self.walk_node(item, inherited, inline_out, blocks_out);
                }
            }
            JsonValue::Object(_) => {
                let raw_tag = node.get("tag").and_then(JsonValue::as_str).unwrap_or("");
                let tag = raw_tag.to_ascii_lowercase();
                if !self.renderer.is_supported_structured_tag(&tag) {
                    return;
                }
                match tag.as_str() {
                    "rt" | "rp" => {}
                    "br" => {
                        let idx = self
                            .renderer
                            .structured_element_value(node, &mut self.ctx, false);
                        self.ctx.elements.push(idx);
                        inline_out.push(InlineAtom::LineBreak);
                        self.ctx.elements.pop();
                    }
                    "img" => {
                        if let Some(atom) = self.resolve_image_atom(node, inherited) {
                            inline_out.push(atom);
                        }
                    }
                    "ruby" => {
                        let idx = self
                            .renderer
                            .structured_element_value(node, &mut self.ctx, false);
                        self.ctx.elements.push(idx);
                        let (mut state, hidden) = self.build_element_state_checked(node, "ruby");
                        if hidden {
                            self.ctx.elements.pop();
                            return;
                        }
                        // Check if CSS hides `<rt>` inside this `<ruby>` (e.g. `.gloss-sc-rt { display: none; }`).
                        let rt_hidden = if self.ctx.dictionary_styles.is_some() {
                            let mut rt_el = mdef::StructuredElement::new();
                            rt_el.tag = "rt".to_owned();
                            rt_el.classes.insert("gloss-sc-rt".to_owned());
                            rt_el
                                .attributes
                                .insert("data-sc-rt".to_owned(), String::new());
                            let rt_idx = self.ctx.selector_elements.len() as i64;
                            self.ctx.selector_elements.push(rt_el);
                            self.ctx.elements.push(rt_idx);
                            let mut rt_decls = mdef::CssDeclarations::new();
                            self.renderer.add_matching_css_rules(
                                &mut self.ctx,
                                &mut rt_decls,
                                None,
                                None,
                            );
                            self.ctx.elements.pop();
                            rt_decls
                                .get("display")
                                .is_some_and(|v| v.trim().eq_ignore_ascii_case("none"))
                        } else {
                            false
                        };
                        let ruby_style = self.inline_style_from_state(&state, inherited);
                        if let Some(content) = node.get("content") {
                            for (base, reading) in extract_ruby_pairs(content) {
                                if base.is_empty() {
                                    continue;
                                }
                                if reading.is_empty() || rt_hidden {
                                    inline_out.push(InlineAtom::Text {
                                        text: base,
                                        style: ruby_style.clone(),
                                        selectable: true,
                                    });
                                } else {
                                    let mut s = ruby_style.clone();
                                    if s.tooltip.is_none() {
                                        s.tooltip = Some(format!("{base}: {reading}"));
                                    }
                                    inline_out.push(InlineAtom::Ruby {
                                        base,
                                        reading,
                                        style: s,
                                    });
                                }
                            }
                        }
                        let _ = &mut state;
                        self.ctx.elements.pop();
                    }
                    "a" => {
                        let idx = self
                            .renderer
                            .structured_element_value(node, &mut self.ctx, false);
                        self.ctx.elements.push(idx);
                        let (state, hidden) = self.build_element_state_checked(node, "a");
                        if hidden {
                            self.ctx.elements.pop();
                            return;
                        }
                        let old_href = node
                            .get("href")
                            .and_then(JsonValue::as_str)
                            .map(|href| std::mem::replace(&mut self.ctx.link_href, href.to_owned()));
                        let old_title = node
                            .get("title")
                            .and_then(JsonValue::as_str)
                            .map(|title| {
                                std::mem::replace(&mut self.ctx.title_tooltip, title.to_owned())
                            });
                        let mut link_style = self.inline_style_from_state(&state, inherited);
                        if !self.ctx.link_href.is_empty() {
                            link_style.link_href = Some(self.ctx.link_href.clone());
                            if state.declarations.get("color").is_none() {
                                link_style.color = self.default_link_color;
                            }
                            if !self.ctx.suppress_link_decoration
                                && state.declarations.get("text-decoration").is_none()
                            {
                                link_style.underline = true;
                            }
                        }
                        if !state.before_content.is_empty() {
                            inline_out.push(InlineAtom::Text {
                                text: state.before_content.clone(),
                                style: link_style.clone(),
                                selectable: false,
                            });
                        }
                        self.ctx.siblings.push(SiblingState::default());
                        if let Some(content) = node.get("content") {
                            self.walk_node(content, &link_style, inline_out, blocks_out);
                        }
                        self.ctx.siblings.pop();
                        if !state.after_content.is_empty() {
                            inline_out.push(InlineAtom::Text {
                                text: state.after_content.clone(),
                                style: link_style,
                                selectable: false,
                            });
                        }
                        if let Some(t) = old_title {
                            self.ctx.title_tooltip = t;
                        }
                        if let Some(h) = old_href {
                            self.ctx.link_href = h;
                        }
                        self.ctx.elements.pop();
                    }
                    "table" => {
                        let idx = self
                            .renderer
                            .structured_element_value(node, &mut self.ctx, false);
                        self.ctx.elements.push(idx);
                        let (mut state, hidden) = self.build_element_state_checked(node, "table");
                        if hidden {
                            self.ctx.elements.pop();
                            return;
                        }
                        flush_inline(inline_out, blocks_out);
                        self.renderer.resolve_box_style(&self.ctx, &mut state);
                        let table_style = self.inline_style_from_state(&state, inherited);
                        let frame = self.box_frame_from_state(&state);
                        std::mem::swap(
                            &mut self.ctx.parent_font_pixel_size,
                            &mut state.font_pixel_size,
                        );
                        std::mem::swap(&mut self.ctx.text_color, &mut state.text_color);
                        self.ctx.siblings.push(SiblingState::default());
                        let mut rows = Vec::new();
                        if let Some(content) = node.get("content") {
                            self.collect_table_rows(content, &table_style, &mut rows);
                        }
                        self.ctx.siblings.pop();
                        std::mem::swap(&mut self.ctx.text_color, &mut state.text_color);
                        std::mem::swap(
                            &mut self.ctx.parent_font_pixel_size,
                            &mut state.font_pixel_size,
                        );
                        self.ctx.elements.pop();
                        if !rows.is_empty() {
                            blocks_out.push(BlockIR::Table { rows, frame });
                        }
                    }
                    _ => {
                        let is_details = tag == "details";
                        let details = is_details
                            .then(|| self.renderer.details_state_value(node, &mut self.ctx));
                        let idx = self.renderer.structured_element_value(
                            node,
                            &mut self.ctx,
                            details.as_ref().is_some_and(|d| d.open),
                        );
                        self.ctx.elements.push(idx);
                        let (mut state, hidden) = self.build_element_state_checked(node, &tag);
                        if hidden {
                            self.ctx.elements.pop();
                            return;
                        }
                        if let Some(d) = details {
                            state.details = d;
                        }
                        let elem_style = self.inline_style_from_state(&state, inherited);

                        if state.layout == ElementLayout::Inline && tag == "span" {
                            if !state.before_content.is_empty() {
                                inline_out.push(InlineAtom::Text {
                                    text: state.before_content.clone(),
                                    style: elem_style.clone(),
                                    selectable: false,
                                });
                            }
                            std::mem::swap(
                                &mut self.ctx.parent_font_pixel_size,
                                &mut state.font_pixel_size,
                            );
                            std::mem::swap(&mut self.ctx.text_color, &mut state.text_color);
                            std::mem::swap(&mut self.ctx.title_tooltip, &mut state.title_tooltip);
                            self.ctx.siblings.push(SiblingState::default());
                            if let Some(content) = node.get("content") {
                                self.walk_node(content, &elem_style, inline_out, blocks_out);
                            }
                            self.ctx.siblings.pop();
                            std::mem::swap(&mut self.ctx.title_tooltip, &mut state.title_tooltip);
                            std::mem::swap(&mut self.ctx.text_color, &mut state.text_color);
                            std::mem::swap(
                                &mut self.ctx.parent_font_pixel_size,
                                &mut state.font_pixel_size,
                            );
                            if !state.after_content.is_empty() {
                                inline_out.push(InlineAtom::Text {
                                    text: state.after_content.clone(),
                                    style: elem_style,
                                    selectable: false,
                                });
                            }
                            self.ctx.elements.pop();
                            return;
                        }

                        // Block / List / Box / Details / Summary element
                        flush_inline(inline_out, blocks_out);
                        if state.layout == ElementLayout::List {
                            self.ctx.lists.push(mdef::StructuredList {
                                tag: tag.clone(),
                                marker: state.list_marker_type.clone(),
                                item: 0,
                            });
                        }
                        if is_details {
                            self.ctx.details.push(state.details.clone());
                        }

                        let mut frame = self.box_frame_from_state(&state);
                        if is_details && frame.padding == [0.0; 4] {
                            frame.padding = [4.0, 6.0, 4.0, 6.0];
                            if frame.bg_color.is_none() {
                                frame.bg_color = Some(if self.is_light_theme {
                                    [241, 245, 249, 230]
                                } else {
                                    [34, 40, 46, 180]
                                });
                            }
                        }

                        std::mem::swap(
                            &mut self.ctx.parent_font_pixel_size,
                            &mut state.font_pixel_size,
                        );
                        std::mem::swap(&mut self.ctx.text_color, &mut state.text_color);
                        std::mem::swap(&mut self.ctx.title_tooltip, &mut state.title_tooltip);
                        std::mem::swap(
                            &mut self.ctx.painted_background_color,
                            &mut state.painted_background_color,
                        );

                        let child_blocks = if is_details && !state.details.open {
                            let mut summary_blocks = Vec::new();
                            let mut siblings = SiblingState::default();
                            self.ctx.siblings.push(siblings);
                            if let Some(content) = node.get("content") {
                                self.walk_closed_details(
                                    content,
                                    &elem_style,
                                    &mut summary_blocks,
                                );
                            }
                            siblings = self.ctx.siblings.pop().unwrap_or_default();
                            let _ = siblings;
                            summary_blocks
                        } else {
                            let mut inner_siblings = SiblingState::default();
                            if let Some(content) = node.get("content") {
                                if self.ctx.needs_element_child_count
                                    && self
                                        .renderer
                                        .needs_structured_element_child_count_value(
                                            content,
                                            &self.ctx,
                                        )
                                {
                                    inner_siblings.element_count = self
                                        .renderer
                                        .structured_element_child_count_value(content);
                                }
                            }
                            self.ctx.siblings.push(inner_siblings);
                            let mut inner_blocks = Vec::new();
                            let mut inner_inline = Vec::new();
                            if state.disclosure_summary {
                                inner_inline.push(InlineAtom::DetailsToggle {
                                    detail_id: state.details.id.clone(),
                                    open: state.details.open,
                                    style: elem_style.clone(),
                                });
                            }
                            if !state.before_content.is_empty() {
                                inner_inline.push(InlineAtom::Text {
                                    text: state.before_content.clone(),
                                    style: elem_style.clone(),
                                    selectable: false,
                                });
                            }
                            if let Some(content) = node.get("content") {
                                self.walk_node(
                                    content,
                                    &elem_style,
                                    &mut inner_inline,
                                    &mut inner_blocks,
                                );
                            }
                            if !state.after_content.is_empty() {
                                inner_inline.push(InlineAtom::Text {
                                    text: state.after_content.clone(),
                                    style: elem_style.clone(),
                                    selectable: false,
                                });
                            }
                            if !inner_inline.is_empty() {
                                inner_blocks.push(BlockIR::Paragraph {
                                    atoms: inner_inline,
                                    frame: BoxFrameIR::default(),
                                    marker: None,
                                    summary_detail_id: state
                                        .disclosure_summary
                                        .then(|| state.details.id.clone()),
                                });
                            }
                            self.ctx.siblings.pop();
                            inner_blocks
                        };

                        std::mem::swap(
                            &mut self.ctx.painted_background_color,
                            &mut state.painted_background_color,
                        );
                        std::mem::swap(&mut self.ctx.title_tooltip, &mut state.title_tooltip);
                        std::mem::swap(&mut self.ctx.text_color, &mut state.text_color);
                        std::mem::swap(
                            &mut self.ctx.parent_font_pixel_size,
                            &mut state.font_pixel_size,
                        );

                        if state.layout == ElementLayout::List {
                            self.ctx.lists.pop();
                        }
                        if is_details {
                            self.ctx.details.pop();
                        }
                        self.ctx.elements.pop();

                        let marker = (!state.list_marker.is_empty()).then(|| {
                            (
                                state.list_marker.clone(),
                                elem_style.color,
                                elem_style.font_px,
                            )
                        });
                        // Yomitan-aligned compact glossaries: flatten simple inline `<ul>`/`<ol>`
                        // lists into a single inline ` | `-separated flow instead of stacked bullets.
                        if self.compact_glossaries
                            && (tag == "ul" || tag == "ol")
                            && !child_blocks.is_empty()
                        {
                            if let Some(compact_atoms) =
                                try_flatten_compact_list_blocks(&child_blocks, &elem_style)
                            {
                                blocks_out.push(BlockIR::Paragraph {
                                    atoms: compact_atoms,
                                    frame,
                                    marker: None,
                                    summary_detail_id: None,
                                });
                                return;
                            }
                        }
                        if child_blocks.len() == 1 && frame_is_trivial(&frame) && marker.is_none() {
                            blocks_out.extend(child_blocks);
                        } else if !child_blocks.is_empty() {
                            blocks_out.push(BlockIR::Container {
                                blocks: child_blocks,
                                frame,
                                marker,
                            });
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn walk_closed_details(
        &mut self,
        node: &JsonValue,
        inherited: &InlineStyleIR,
        blocks_out: &mut Vec<BlockIR>,
    ) {
        match node {
            JsonValue::Array(items) => {
                for item in items {
                    self.walk_closed_details(item, inherited, blocks_out);
                }
            }
            JsonValue::Object(_)
                if node
                    .get("tag")
                    .and_then(JsonValue::as_str)
                    .is_some_and(|t| t.eq_ignore_ascii_case("summary")) =>
            {
                let mut inline_buf = Vec::new();
                self.walk_node(node, inherited, &mut inline_buf, blocks_out);
                if !inline_buf.is_empty() {
                    blocks_out.push(BlockIR::Paragraph {
                        atoms: inline_buf,
                        frame: BoxFrameIR::default(),
                        marker: None,
                        summary_detail_id: None,
                    });
                }
            }
            JsonValue::Object(_) => {
                self.renderer
                    .skip_closed_details_content_value(node, &mut self.ctx);
            }
            _ => {}
        }
    }
}

fn frame_is_trivial(f: &BoxFrameIR) -> bool {
    f.bg_color.is_none()
        && f.padding == [0.0; 4]
        && f.margin_top == 0.0
        && f.margin_bottom == 0.0
        && f.margin_left == 0.0
        && f.margin_right == 0.0
        && f.borders.iter().all(Option::is_none)
}

fn frame_has_visual_box(f: &BoxFrameIR) -> bool {
    f.bg_color.is_some() || f.borders.iter().any(Option::is_some)
}

/// Flatten simple inline `<li>` children of a `<ul>`/`<ol>` into a single inline
/// ` | `-separated atom stream for Yomitan-aligned `compact_glossaries` layout.
/// Returns `None` if any child contains tables, `<details>`, nested block boxes, or
/// explicit multi-line breaks so complex structured glossaries preserve block layout.
fn try_flatten_compact_list_blocks(
    blocks: &[BlockIR],
    sep_style: &InlineStyleIR,
) -> Option<Vec<InlineAtom>> {
    let mut item_atom_slices: Vec<&[InlineAtom]> = Vec::with_capacity(blocks.len());
    for blk in blocks {
        let atoms: &[InlineAtom] = match blk {
            BlockIR::Paragraph {
                atoms,
                frame,
                summary_detail_id,
                ..
            } if !frame_has_visual_box(frame) && summary_detail_id.is_none() => atoms.as_slice(),
            BlockIR::Container {
                blocks: inner,
                frame,
                ..
            } if !frame_has_visual_box(frame) && inner.len() == 1 => match &inner[0] {
                BlockIR::Paragraph {
                    atoms,
                    frame: inner_frame,
                    summary_detail_id,
                    ..
                } if !frame_has_visual_box(inner_frame) && summary_detail_id.is_none() => {
                    atoms.as_slice()
                }
                _ => return None,
            },
            _ => return None,
        };
        if atoms.iter().any(|a| {
            matches!(
                a,
                InlineAtom::LineBreak | InlineAtom::DetailsToggle { .. }
            )
        }) {
            return None;
        }
        if !atoms.is_empty() {
            item_atom_slices.push(atoms);
        }
    }
    if item_atom_slices.is_empty() {
        return None;
    }
    let mut out = Vec::new();
    for (idx, atoms) in item_atom_slices.into_iter().enumerate() {
        if idx > 0 {
            out.push(InlineAtom::Text {
                text: " | ".to_owned(),
                style: sep_style.clone(),
                selectable: true,
            });
        }
        out.extend(atoms.iter().cloned());
    }
    Some(out)
}

fn push_legacy_text_atoms(text: &str, style: &InlineStyleIR, out: &mut Vec<InlineAtom>) {
    for (li, line) in text.split('\n').enumerate() {
        if li > 0 {
            out.push(InlineAtom::LineBreak);
        }
        if !line.is_empty() {
            out.push(InlineAtom::Text {
                text: line.to_owned(),
                style: style.clone(),
                selectable: true,
            });
        }
    }
}

pub(super) struct PositionedGlossaryImage {
    pub(super) phys_x: f32,
    pub(super) phys_y: f32,
    pub(super) phys_w: f32,
    pub(super) phys_h: f32,
    pub(super) pixmap: Arc<tiny_skia::Pixmap>,
}

pub(super) struct PaintCollector {
    pub(super) render_scale: f32,
    pub(super) default_text_color: [u8; 4],
    pub(super) ruby_color: [u8; 4],
    pub(super) compact_glossaries: bool,
    pub(super) rects: Vec<(f32, f32, f32, f32, [u8; 4])>,
    pub(super) images: Vec<PositionedGlossaryImage>,
    pub(super) mask_glyphs: Vec<PositionedMaskGlyph>,
    hit_zones: Vec<GlossaryHitZone>,
    selectable_chars: Vec<GlossaryCharCell>,
    line_bounds: Vec<(f32, f32)>,
    next_line_id: usize,
}

impl Default for PaintCollector {
    fn default() -> Self {
        Self::new(1.0)
    }
}

impl PaintCollector {
    fn new(render_scale: f32) -> Self {
        Self::with_style(
            render_scale,
            [241, 245, 249, 255],
            [196, 181, 253, 255],
            false,
        )
    }

    fn with_style(
        render_scale: f32,
        default_text_color: [u8; 4],
        ruby_color: [u8; 4],
        compact_glossaries: bool,
    ) -> Self {
        Self {
            render_scale: render_scale.clamp(1.0, 3.0),
            default_text_color,
            ruby_color,
            compact_glossaries,
            rects: Vec::new(),
            images: Vec::new(),
            mask_glyphs: Vec::new(),
            hit_zones: Vec::new(),
            selectable_chars: Vec::new(),
            line_bounds: Vec::new(),
            next_line_id: 0,
        }
    }

    fn spawn_child(&self) -> Self {
        Self::with_style(
            self.render_scale,
            self.default_text_color,
            self.ruby_color,
            self.compact_glossaries,
        )
    }

    fn push_frame(&mut self, x: f32, y: f32, w: f32, h: f32, frame: &BoxFrameIR) {
        if w <= 0.0 || h <= 0.0 {
            return;
        }
        if let Some(bg) = frame.bg_color {
            self.rects.push((x, y, w, h, bg));
        }
        if let Some((bw, col)) = frame.borders[TOP] {
            self.rects.push((x, y, w, bw, col));
        }
        if let Some((bw, col)) = frame.borders[BOTTOM] {
            self.rects.push((x, (y + h - bw).max(y), w, bw, col));
        }
        if let Some((bw, col)) = frame.borders[LEFT] {
            self.rects.push((x, y, bw, h, col));
        }
        if let Some((bw, col)) = frame.borders[RIGHT] {
            self.rects.push(((x + w - bw).max(x), y, bw, h, col));
        }
    }

    fn append_shifted_y(&mut self, other: PaintCollector, dy: f32) {
        let scale = self.render_scale;
        let phys_dy = dy * scale;
        let phys_dy_i32 = phys_dy.round() as i32;
        let line_id_base = self.next_line_id;
        self.next_line_id += other.next_line_id;

        self.rects.reserve(other.rects.len());
        for (rx, ry, rw, rh, col) in other.rects {
            self.rects.push((rx, ry + dy, rw, rh, col));
        }
        self.images.reserve(other.images.len());
        for mut img in other.images {
            img.phys_y += phys_dy;
            self.images.push(img);
        }
        self.mask_glyphs.reserve(other.mask_glyphs.len());
        for mut mg in other.mask_glyphs {
            mg.y += phys_dy_i32;
            self.mask_glyphs.push(mg);
        }
        self.hit_zones.reserve(other.hit_zones.len());
        for mut hz in other.hit_zones {
            hz.y += dy;
            self.hit_zones.push(hz);
        }
        self.selectable_chars.reserve(other.selectable_chars.len());
        for mut sc in other.selectable_chars {
            sc.y += dy;
            sc.line_id += line_id_base;
            self.selectable_chars.push(sc);
        }
        self.line_bounds.reserve(other.line_bounds.len());
        for (top, bottom) in other.line_bounds {
            self.line_bounds.push((top + dy, bottom + dy));
        }
    }
}

/// Per-document memoization cache of each `BlockIR` node's intrinsic unwrapped width
/// (keyed by immutable `&BlockIR` pointer address) so nested tables measure every cell
/// at most once instead of exponentially (`3^depth`).
#[derive(Default)]
struct LayoutMeasureCache {
    intrinsic_w: HashMap<usize, f32>,
}

fn measure_inline_atoms_intrinsic_width(engine: &mut TextEngine, atoms: &[InlineAtom]) -> f32 {
    if atoms.is_empty() {
        return 0.0;
    }

    let mut full_text = String::new();
    let mut spans: Vec<(std::ops::Range<usize>, &InlineStyleIR)> = Vec::new();
    let mut inline_boxes: Vec<InlineBox> = Vec::new();
    let mut next_box_id = 1u64;

    for atom in atoms {
        match atom {
            InlineAtom::Text { text, style, .. } => {
                if text.is_empty() {
                    continue;
                }
                let has_pill = style.pill_bg.is_some() || style.pill_border.is_some();
                let start = full_text.len();
                if has_pill {
                    full_text.push('\u{00A0}');
                }
                full_text.push_str(text);
                if has_pill {
                    full_text.push('\u{00A0}');
                }
                let end = full_text.len();
                spans.push((start..end, style));
                if has_pill {
                    full_text.push(' ');
                }
            }
            InlineAtom::DetailsToggle { open, style, .. } => {
                let start = full_text.len();
                full_text.push(if *open { '▾' } else { '▸' });
                full_text.push(' ');
                let end = full_text.len();
                spans.push((start..end, style));
            }
            InlineAtom::LineBreak => {
                full_text.push('\n');
            }
            InlineAtom::Ruby {
                base,
                reading,
                style,
            } => {
                let base_l = engine.shape_single_line(
                    base,
                    &style.font_family,
                    style.font_px,
                    style.weight,
                    style.italic,
                    style.color,
                );
                let ruby_px = (style.font_px * 0.58).max(8.5);
                let ruby_l = engine.shape_single_line(
                    reading,
                    &style.font_family,
                    ruby_px,
                    style.weight,
                    false,
                    [196, 181, 253, 255],
                );
                let bw = base_l
                    .lines()
                    .next()
                    .map_or(style.font_px, |l| l.metrics().advance);
                let bh = base_l
                    .lines()
                    .next()
                    .map_or(style.font_px * 1.2, |l| l.metrics().line_height);
                let rw = ruby_l.lines().next().map_or(0.0, |l| l.metrics().advance);
                let rh = ruby_l
                    .lines()
                    .next()
                    .map_or(ruby_px * 1.1, |l| l.metrics().line_height);
                let id = next_box_id;
                next_box_id += 1;
                inline_boxes.push(InlineBox {
                    id,
                    index: full_text.len(),
                    width: bw.max(rw) + 2.0,
                    height: rh + bh,
                });
            }
            InlineAtom::Image { w, h, .. } => {
                let id = next_box_id;
                next_box_id += 1;
                inline_boxes.push(InlineBox {
                    id,
                    index: full_text.len(),
                    width: w.max(1.0) + 2.0,
                    height: h.max(1.0) + 2.0,
                });
            }
        }
    }

    let mut builder = engine
        .layout_cx
        .ranged_builder(&mut engine.font_cx, &full_text, 1.0, true);
    builder.push_default(StyleProperty::FontFamily(FontFamily::Source(
        Cow::Borrowed(RICH_EMOJI_FALLBACK_STACK),
    )));
    builder.push_default(StyleProperty::FontSize(14.5));
    builder.push_default(StyleProperty::LineHeight(LineHeight::MetricsRelative(1.18)));
    builder.push_default(StyleProperty::Brush([241, 245, 249, 255]));

    for (range, style) in spans {
        let ff = style.font_family.trim();
        if !ff.is_empty() && !ff.eq_ignore_ascii_case("Noto Sans CJK JP") {
            builder.push(
                StyleProperty::FontFamily(FontFamily::Source(Cow::Owned(format!(
                    "{ff}, {RICH_EMOJI_FALLBACK_STACK}"
                )))),
                range.clone(),
            );
        }
        builder.push(StyleProperty::FontSize(style.font_px), range.clone());
        builder.push(
            StyleProperty::FontWeight(FontWeight::new(style.weight as f32)),
            range.clone(),
        );
        if style.italic {
            builder.push(StyleProperty::FontStyle(FontStyle::Italic), range);
        }
    }
    for ibox in inline_boxes {
        builder.push_inline_box(ibox);
    }

    let mut layout = builder.build(&full_text);
    layout.break_all_lines(None);
    let mut max_line_w = 0.0f32;
    for line in layout.lines() {
        max_line_w = max_line_w.max(line.metrics().advance - line.metrics().trailing_whitespace);
    }
    max_line_w.max(0.0)
}

fn place_table_cells(rows: &[Vec<TableCellIR>]) -> (Vec<PlacedCell<'_>>, usize) {
    let num_rows = rows.len();
    let mut occupied: HashSet<(usize, usize)> = HashSet::new();
    let mut placed: Vec<PlacedCell<'_>> = Vec::new();
    let mut num_cols = 0usize;

    for (r_idx, row) in rows.iter().enumerate() {
        let mut c_idx = 0usize;
        for cell in row {
            while occupied.contains(&(r_idx, c_idx)) {
                c_idx += 1;
            }
            let cs = cell.col_span.max(1);
            let rs = cell.row_span.max(1).min(num_rows - r_idx);
            for dr in 0..rs {
                for dc in 0..cs {
                    occupied.insert((r_idx + dr, c_idx + dc));
                }
            }
            num_cols = num_cols.max(c_idx + cs);
            placed.push(PlacedCell {
                cell,
                row: r_idx,
                col: c_idx,
                col_span: cs,
                row_span: rs,
            });
            c_idx += cs;
        }
    }
    (placed, num_cols)
}

fn measure_block_intrinsic_width(
    engine: &mut TextEngine,
    block: &BlockIR,
    cache: &mut LayoutMeasureCache,
) -> f32 {
    let key = block as *const BlockIR as usize;
    if let Some(&cached) = cache.intrinsic_w.get(&key) {
        return cached;
    }

    let measured = match block {
        BlockIR::Paragraph {
            atoms,
            frame,
            marker,
            ..
        } => {
            let border_l = frame.borders[LEFT].map_or(0.0, |(w, _)| w);
            let border_r = frame.borders[RIGHT].map_or(0.0, |(w, _)| w);
            let marker_w = if let Some((ref m_txt, m_col, m_px)) = *marker {
                let m_layout =
                    engine.shape_single_line(m_txt, "Noto Sans CJK JP", m_px, 600, false, m_col);
                let adv = m_layout
                    .lines()
                    .next()
                    .map_or(m_px, |l| l.metrics().advance)
                    + 6.0;
                adv
            } else {
                0.0
            };
            let para_w = measure_inline_atoms_intrinsic_width(engine, atoms);
            frame.margin_left
                + border_l
                + frame.padding[LEFT]
                + marker_w
                + para_w
                + frame.padding[RIGHT]
                + border_r
                + frame.margin_right
        }
        BlockIR::Container {
            blocks,
            frame,
            marker,
        } => {
            let border_l = frame.borders[LEFT].map_or(0.0, |(w, _)| w);
            let border_r = frame.borders[RIGHT].map_or(0.0, |(w, _)| w);
            let marker_w = if let Some((ref m_txt, m_col, m_px)) = *marker {
                let m_layout =
                    engine.shape_single_line(m_txt, "Noto Sans CJK JP", m_px, 600, false, m_col);
                let adv = m_layout
                    .lines()
                    .next()
                    .map_or(m_px, |l| l.metrics().advance)
                    + 6.0;
                adv
            } else {
                0.0
            };
            let mut max_child_w = 0.0f32;
            for child in blocks {
                max_child_w = max_child_w.max(measure_block_intrinsic_width(engine, child, cache));
            }
            frame.margin_left
                + border_l
                + frame.padding[LEFT]
                + marker_w
                + max_child_w
                + frame.padding[RIGHT]
                + border_r
                + frame.margin_right
        }
        BlockIR::Table { rows, frame } => {
            if rows.is_empty() {
                0.0
            } else {
                let (placed, num_cols) = place_table_cells(rows);
                if num_cols == 0 {
                    0.0
                } else {
                    let mut col_pref = vec![36.0f32; num_cols];
                    for p in &placed {
                        let pad_h = p.cell.frame.padding[LEFT] + p.cell.frame.padding[RIGHT] + 4.0;
                        let mut natural_w = 24.0f32;
                        for blk in &p.cell.blocks {
                            natural_w =
                                natural_w.max(measure_block_intrinsic_width(engine, blk, cache));
                        }
                        let needed = (natural_w + pad_h) / (p.col_span as f32);
                        for dc in 0..p.col_span {
                            col_pref[p.col + dc] = col_pref[p.col + dc].max(needed);
                        }
                    }
                    frame.margin_left + col_pref.iter().sum::<f32>() + frame.margin_right
                }
            }
        }
    };

    cache.intrinsic_w.insert(key, measured);
    measured
}

#[allow(clippy::too_many_arguments)]
fn layout_and_collect_block(
    engine: &mut TextEngine,
    block: &BlockIR,
    x: f32,
    y: f32,
    avail_w: f32,
    collector: &mut PaintCollector,
    measure_only: bool,
    cache: &mut LayoutMeasureCache,
) -> (f32, f32) {
    match block {
        BlockIR::Paragraph {
            atoms,
            frame,
            marker,
            summary_detail_id,
        } => {
            let outer_x = x + frame.margin_left;
            let outer_y = y + frame.margin_top;
            let box_w = (avail_w - frame.margin_left - frame.margin_right).max(24.0);
            let border_l = frame.borders[LEFT].map_or(0.0, |(w, _)| w);
            let border_r = frame.borders[RIGHT].map_or(0.0, |(w, _)| w);
            let border_t = frame.borders[TOP].map_or(0.0, |(w, _)| w);
            let border_b = frame.borders[BOTTOM].map_or(0.0, |(w, _)| w);

            let mut content_x = outer_x + border_l + frame.padding[LEFT];
            let content_y = outer_y + border_t + frame.padding[TOP];
            let mut content_w = (box_w
                - border_l
                - border_r
                - frame.padding[LEFT]
                - frame.padding[RIGHT])
                .max(16.0);

            let mut marker_w = 0.0;
            if let Some((ref m_txt, m_col, m_px)) = *marker {
                let m_layout =
                    engine.shape_single_line(m_txt, "Noto Sans CJK JP", m_px, 600, false, m_col);
                let mw = m_layout
                    .lines()
                    .next()
                    .map_or(m_px, |l| l.metrics().advance)
                    + 6.0;
                marker_w = mw;
                if !measure_only {
                    engine.collect_glossary_glyphs_from_layout(
                        &m_layout,
                        content_x,
                        content_y,
                        false,
                        false,
                        collector,
                    );
                }
                content_x += mw;
                content_w = (content_w - mw).max(16.0);
            }

            let (para_used_w, para_h) = layout_inline_atoms(
                engine,
                atoms,
                content_x,
                content_y,
                content_w,
                summary_detail_id.as_deref(),
                collector,
                measure_only,
            );
            let inner_h = para_h.max(if marker.is_some() { 18.0 } else { 0.0 });
            let box_h = border_t + frame.padding[TOP] + inner_h + frame.padding[BOTTOM] + border_b;
            let used_box_w = if frame.bg_color.is_some() || frame.borders.iter().any(Option::is_some)
            {
                box_w
            } else {
                (border_l
                    + frame.padding[LEFT]
                    + marker_w
                    + para_used_w
                    + frame.padding[RIGHT]
                    + border_r)
                    .min(box_w)
            };

            if !measure_only {
                collector.push_frame(outer_x, outer_y, box_w, box_h, frame);
                if let Some(detail_id) = summary_detail_id {
                    collector.hit_zones.push(GlossaryHitZone {
                        x: outer_x,
                        y: outer_y,
                        w: box_w,
                        h: box_h.max(18.0),
                        action: Some(GlossaryZoneAction::ToggleDetails(detail_id.clone())),
                        tooltip: None,
                    });
                }
            }
            (
                frame.margin_left + used_box_w + frame.margin_right,
                frame.margin_top + box_h + frame.margin_bottom,
            )
        }
        BlockIR::Container {
            blocks,
            frame,
            marker,
        } => {
            let outer_x = x + frame.margin_left;
            let outer_y = y + frame.margin_top;
            let box_w = (avail_w - frame.margin_left - frame.margin_right).max(24.0);
            let border_l = frame.borders[LEFT].map_or(0.0, |(w, _)| w);
            let border_r = frame.borders[RIGHT].map_or(0.0, |(w, _)| w);
            let border_t = frame.borders[TOP].map_or(0.0, |(w, _)| w);
            let border_b = frame.borders[BOTTOM].map_or(0.0, |(w, _)| w);

            let mut content_x = outer_x + border_l + frame.padding[LEFT];
            let content_y = outer_y + border_t + frame.padding[TOP];
            let mut content_w = (box_w
                - border_l
                - border_r
                - frame.padding[LEFT]
                - frame.padding[RIGHT])
                .max(16.0);

            let rect_insert_idx = collector.rects.len();

            let mut marker_w = 0.0;
            if let Some((ref m_txt, m_col, m_px)) = *marker {
                let m_layout =
                    engine.shape_single_line(m_txt, "Noto Sans CJK JP", m_px, 600, false, m_col);
                let mw = m_layout
                    .lines()
                    .next()
                    .map_or(m_px, |l| l.metrics().advance)
                    + 6.0;
                marker_w = mw;
                if !measure_only {
                    engine.collect_glossary_glyphs_from_layout(
                        &m_layout,
                        content_x,
                        content_y,
                        false,
                        false,
                        collector,
                    );
                }
                content_x += mw;
                content_w = (content_w - mw).max(16.0);
            }

            let child_gap = if collector.compact_glossaries { 1.0 } else { 2.0 };
            let mut cur_y = content_y;
            let mut max_child_w = 0.0f32;
            for (idx, child) in blocks.iter().enumerate() {
                if idx > 0 {
                    cur_y += child_gap;
                }
                let (cw, ch) = layout_and_collect_block(
                    engine,
                    child,
                    content_x,
                    cur_y,
                    content_w,
                    collector,
                    measure_only,
                    cache,
                );
                max_child_w = max_child_w.max(cw);
                cur_y += ch;
            }
            let inner_h = (cur_y - content_y).max(0.0);
            let box_h = border_t + frame.padding[TOP] + inner_h + frame.padding[BOTTOM] + border_b;

            if !measure_only && !frame_is_trivial(frame) {
                let mut frame_collector = collector.spawn_child();
                frame_collector.push_frame(outer_x, outer_y, box_w, box_h, frame);
                collector
                    .rects
                    .splice(rect_insert_idx..rect_insert_idx, frame_collector.rects);
            }
            let used_w = border_l
                + frame.padding[LEFT]
                + marker_w
                + max_child_w
                + frame.padding[RIGHT]
                + border_r;
            (
                frame.margin_left + used_w.min(box_w) + frame.margin_right,
                frame.margin_top + box_h + frame.margin_bottom,
            )
        }
        BlockIR::Table { rows, frame } => layout_table_block(
            engine,
            rows,
            frame,
            x,
            y,
            avail_w,
            collector,
            measure_only,
            cache,
        ),
    }
}

#[derive(Clone)]
struct AtomSpanMeta {
    byte_range: std::ops::Range<usize>,
    style: InlineStyleIR,
    selectable: bool,
    detail_toggle: Option<String>,
}

struct RubyBoxItem {
    box_id: u64,
    base: String,
    reading: String,
    style: InlineStyleIR,
    w: f32,
    h: f32,
    base_y: f32,
    ruby_h: f32,
    base_w: f32,
    ruby_w: f32,
    base_layout: parley::Layout<[u8; 4]>,
    ruby_layout: parley::Layout<[u8; 4]>,
}

struct ImageBoxItem {
    box_id: u64,
    pixmap: Arc<tiny_skia::Pixmap>,
    w: f32,
    h: f32,
    link_href: Option<String>,
    tooltip: Option<String>,
}

fn layout_inline_atoms(
    engine: &mut TextEngine,
    atoms: &[InlineAtom],
    origin_x: f32,
    origin_y: f32,
    avail_w: f32,
    summary_detail_id: Option<&str>,
    collector: &mut PaintCollector,
    measure_only: bool,
) -> (f32, f32) {
    if atoms.is_empty() {
        return (0.0, 0.0);
    }

    let mut full_text = String::new();
    let mut spans: Vec<AtomSpanMeta> = Vec::new();
    let mut ruby_boxes: Vec<RubyBoxItem> = Vec::new();
    let mut image_boxes: Vec<ImageBoxItem> = Vec::new();
    let mut inline_boxes: Vec<InlineBox> = Vec::new();
    let mut next_box_id = 1u64;

    for atom in atoms {
        match atom {
            InlineAtom::Text {
                text,
                style,
                selectable,
            } => {
                if text.is_empty() {
                    continue;
                }
                let has_pill = style.pill_bg.is_some() || style.pill_border.is_some();
                let start = full_text.len();
                if has_pill {
                    full_text.push('\u{00A0}');
                }
                full_text.push_str(text);
                if has_pill {
                    full_text.push('\u{00A0}');
                }
                let end = full_text.len();
                spans.push(AtomSpanMeta {
                    byte_range: start..end,
                    style: style.clone(),
                    selectable: *selectable,
                    detail_toggle: None,
                });
                if has_pill {
                    full_text.push(' ');
                }
            }
            InlineAtom::DetailsToggle {
                detail_id,
                open,
                style,
            } => {
                let start = full_text.len();
                full_text.push(if *open { '▾' } else { '▸' });
                full_text.push(' ');
                let end = full_text.len();
                spans.push(AtomSpanMeta {
                    byte_range: start..end,
                    style: style.clone(),
                    selectable: false,
                    detail_toggle: Some(detail_id.clone()),
                });
            }
            InlineAtom::LineBreak => {
                full_text.push('\n');
            }
            InlineAtom::Ruby {
                base,
                reading,
                style,
            } => {
                let base_l = engine.shape_single_line(
                    base,
                    &style.font_family,
                    style.font_px,
                    style.weight,
                    style.italic,
                    style.color,
                );
                let ruby_px = (style.font_px * 0.58).max(8.5);
                let ruby_l = engine.shape_single_line(
                    reading,
                    &style.font_family,
                    ruby_px,
                    style.weight,
                    false,
                    collector.ruby_color,
                );
                let bw = base_l
                    .lines()
                    .next()
                    .map_or(style.font_px, |l| l.metrics().advance);
                let bh = base_l
                    .lines()
                    .next()
                    .map_or(style.font_px * 1.2, |l| l.metrics().line_height);
                let rw = ruby_l.lines().next().map_or(0.0, |l| l.metrics().advance);
                let rh = ruby_l
                    .lines()
                    .next()
                    .map_or(ruby_px * 1.1, |l| l.metrics().line_height);
                let w = bw.max(rw) + 2.0;
                let h = rh + bh;
                let id = next_box_id;
                next_box_id += 1;
                inline_boxes.push(InlineBox {
                    id,
                    index: full_text.len(),
                    width: w,
                    height: h,
                });
                ruby_boxes.push(RubyBoxItem {
                    box_id: id,
                    base: base.clone(),
                    reading: reading.clone(),
                    style: style.clone(),
                    w,
                    h,
                    base_y: rh,
                    ruby_h: rh,
                    base_w: bw,
                    ruby_w: rw,
                    base_layout: base_l,
                    ruby_layout: ruby_l,
                });
            }
            InlineAtom::Image {
                pixmap,
                w,
                h,
                link_href,
                tooltip,
            } => {
                let scale = if *w > avail_w { avail_w / *w } else { 1.0 };
                let dw = (*w * scale).max(1.0);
                let dh = (*h * scale).max(1.0);
                let id = next_box_id;
                next_box_id += 1;
                inline_boxes.push(InlineBox {
                    id,
                    index: full_text.len(),
                    width: dw + 2.0,
                    height: dh + 2.0,
                });
                image_boxes.push(ImageBoxItem {
                    box_id: id,
                    pixmap: pixmap.clone(),
                    w: dw,
                    h: dh,
                    link_href: link_href.clone(),
                    tooltip: tooltip.clone(),
                });
            }
        }
    }

    let mut builder = engine
        .layout_cx
        .ranged_builder(&mut engine.font_cx, &full_text, 1.0, true);
    builder.push_default(StyleProperty::FontFamily(FontFamily::Source(
        Cow::Borrowed(RICH_EMOJI_FALLBACK_STACK),
    )));
    builder.push_default(StyleProperty::FontSize(14.5));
    builder.push_default(StyleProperty::LineHeight(LineHeight::MetricsRelative(1.18)));
    builder.push_default(StyleProperty::Brush(collector.default_text_color));

    for span in &spans {
        let ff = span.style.font_family.trim();
        if !ff.is_empty() && !ff.eq_ignore_ascii_case("Noto Sans CJK JP") {
            builder.push(
                StyleProperty::FontFamily(FontFamily::Source(Cow::Owned(format!(
                    "{ff}, {RICH_EMOJI_FALLBACK_STACK}"
                )))),
                span.byte_range.clone(),
            );
        }
        builder.push(
            StyleProperty::FontSize(span.style.font_px),
            span.byte_range.clone(),
        );
        builder.push(
            StyleProperty::FontWeight(FontWeight::new(span.style.weight as f32)),
            span.byte_range.clone(),
        );
        if span.style.italic {
            builder.push(
                StyleProperty::FontStyle(FontStyle::Italic),
                span.byte_range.clone(),
            );
        }
        builder.push(
            StyleProperty::Brush(span.style.color),
            span.byte_range.clone(),
        );
        if span.style.underline {
            builder.push(StyleProperty::Underline(true), span.byte_range.clone());
        }
        if span.style.strikethrough {
            builder.push(
                StyleProperty::Strikethrough(true),
                span.byte_range.clone(),
            );
        }
    }
    for ibox in inline_boxes {
        builder.push_inline_box(ibox);
    }

    let mut layout = builder.build(&full_text);
    layout.break_all_lines(Some(avail_w.max(16.0)));
    layout.align(
        Some(avail_w.max(16.0)),
        Alignment::Start,
        AlignmentOptions::default(),
    );

    let mut max_line_w = 0.0f32;
    for line in layout.lines() {
        max_line_w = max_line_w.max(line.metrics().advance - line.metrics().trailing_whitespace);
    }
    let total_h = layout.height();
    if measure_only {
        return (max_line_w.max(0.0), total_h);
    }

    let mut byte_to_span = vec![usize::MAX; full_text.len() + 1];
    for (s_idx, span) in spans.iter().enumerate() {
        for b in span.byte_range.start..span.byte_range.end.min(byte_to_span.len()) {
            byte_to_span[b] = s_idx;
        }
    }

    let total_lines = layout.len();
    for (line_idx, line) in layout.lines().enumerate() {
        let line_text_range = line.text_range();
        let line_id = collector.next_line_id;
        collector.next_line_id += 1;
        let line_top = origin_y + line.metrics().min_coord;
        let line_h = (line.metrics().max_coord - line.metrics().min_coord)
            .max(line.metrics().line_height)
            .max(14.0);
        collector.line_bounds.push((line_top, line_top + line_h));
        let baseline = origin_y + line.metrics().baseline;
        let mut prev_run_key: Option<(usize, usize, u64, u32)> = None;
        let mut run_glyph_cursor: usize = 0;

        for item in line.items() {
            match item {
                PositionedLayoutItem::GlyphRun(glyph_run) => {
                    let run = glyph_run.run();
                    let run_font = run.font().clone();
                    let run_key = (
                        run.text_range().start,
                        run.text_range().end,
                        run_font.data.id(),
                        run_font.index,
                    );
                    if prev_run_key != Some(run_key) {
                        prev_run_key = Some(run_key);
                        run_glyph_cursor = 0;
                    }
                    let glyph_start = run_glyph_cursor;
                    let glyph_count = glyph_run.glyphs().count();
                    run_glyph_cursor += glyph_count;
                    let glyph_end = glyph_start + glyph_count;

                    let font_size = run.font_size();
                    let brush = glyph_run.style().brush;
                    let run_ul = glyph_run.style().underline.is_some();
                    let run_st = glyph_run.style().strikethrough.is_some();
                    let synth_italic = run.synthesis().skew().is_some();
                    let is_color_font = engine.font_supports_color(run_font.borrow_font());
                    let mut x_cursor = origin_x + glyph_run.offset();
                    let mut cluster_glyph_idx = 0usize;
                    let mut prev_cluster_in_slice = false;

                    for cluster in run.visual_clusters() {
                        let c_glyph_count = cluster.glyphs().count();
                        let in_slice = if c_glyph_count == 0 {
                            prev_cluster_in_slice || (cluster_glyph_idx == 0 && glyph_start == 0)
                        } else {
                            let matched = cluster_glyph_idx < glyph_end
                                && (cluster_glyph_idx + c_glyph_count) > glyph_start;
                            prev_cluster_in_slice = matched;
                            matched
                        };
                        cluster_glyph_idx += c_glyph_count;
                        if !in_slice {
                            continue;
                        }

                        let range = cluster.text_range();
                        let adv = cluster.advance();
                        let span_meta = byte_to_span
                            .get(range.start)
                            .copied()
                            .filter(|&idx| idx != usize::MAX)
                            .and_then(|idx| spans.get(idx));

                        if let Some(meta) = span_meta {
                            if let Some(bg) = meta.style.pill_bg {
                                collector.rects.push((
                                    x_cursor,
                                    (baseline - font_size * 0.92).max(line_top),
                                    adv + 0.5,
                                    font_size * 1.18,
                                    bg,
                                ));
                            }
                            if let Some(border_col) = meta.style.pill_border {
                                collector.rects.push((
                                    x_cursor,
                                    (baseline - font_size * 0.92).max(line_top),
                                    adv + 0.5,
                                    1.0,
                                    border_col,
                                ));
                                collector.rects.push((
                                    x_cursor,
                                    baseline + font_size * 0.24,
                                    adv + 0.5,
                                    1.0,
                                    border_col,
                                ));
                            }
                            let action = if let Some(ref did) = meta.detail_toggle {
                                Some(GlossaryZoneAction::ToggleDetails(did.clone()))
                            } else if let Some(did) = summary_detail_id {
                                Some(GlossaryZoneAction::ToggleDetails(did.to_owned()))
                            } else {
                                meta.style.link_href.clone().map(GlossaryZoneAction::Link)
                            };
                            if action.is_some() || meta.style.tooltip.is_some() {
                                collector.hit_zones.push(GlossaryHitZone {
                                    x: x_cursor,
                                    y: line_top,
                                    w: adv.max(4.0),
                                    h: line_h,
                                    action,
                                    tooltip: meta.style.tooltip.clone(),
                                });
                            }
                        }

                        if c_glyph_count > 0 {
                            let scale = collector.render_scale;
                            let scaled_font_size = font_size * scale;
                            let mut gx = x_cursor;
                            for g in cluster.glyphs() {
                                let phys_x = (gx + g.x) * scale;
                                let phys_y = (baseline + g.y) * scale;
                                let rendered_color = is_color_font
                                    && engine
                                        .try_render_color_glyph(
                                            run_font.borrow_font(),
                                            g.id as u16,
                                            scaled_font_size,
                                        )
                                        .is_some_and(|cg| {
                                            let cw = cg.pixmap.width() as f32;
                                            let ch = cg.pixmap.height() as f32;
                                            collector.images.push(PositionedGlossaryImage {
                                                phys_x: phys_x + cg.left,
                                                phys_y: phys_y - cg.top,
                                                phys_w: cw,
                                                phys_h: ch,
                                                pixmap: cg.pixmap,
                                            });
                                            true
                                        });
                                if !rendered_color {
                                    let (glyph_x, sub_q) = if scale < 1.75 {
                                        quantize_subpixel_x(phys_x)
                                    } else {
                                        (phys_x.round() as i32, 0)
                                    };
                                    if let Some(ag) = engine.try_render_alpha_glyph(
                                        run_font.borrow_font(),
                                        g.id as u16,
                                        scaled_font_size,
                                        synth_italic,
                                        sub_q,
                                    ) {
                                        collector.mask_glyphs.push(PositionedMaskGlyph {
                                            x: glyph_x + ag.left,
                                            y: phys_y.round() as i32 - ag.top,
                                            glyph: ag,
                                            rgba: brush,
                                        });
                                    }
                                }
                                gx += g.advance;
                            }
                        }
                        if run_ul && adv > 0.0 {
                            collector
                                .rects
                                .push((x_cursor, baseline + 2.0, adv, 1.0, brush));
                        }
                        if run_st && adv > 0.0 {
                            collector
                                .rects
                                .push((x_cursor, baseline - font_size * 0.32, adv, 1.0, brush));
                        }

                        if let Some(slice) = full_text.get(range.clone()) {
                            let visible_chars: Vec<char> = slice
                                .chars()
                                .filter(|&c| c != '\n' && c != '\r')
                                .collect();
                            if !visible_chars.is_empty() {
                                let selectable = span_meta.is_none_or(|m| m.selectable);
                                if selectable {
                                    let char_count = visible_chars.len();
                                    let effective_adv = if adv > 0.0 {
                                        adv
                                    } else {
                                        font_size * 0.45 * (char_count as f32)
                                    };
                                    let per_w = (effective_adv / (char_count as f32)).max(2.0);
                                    for (ci, ch) in visible_chars.into_iter().enumerate() {
                                        let norm_ch = if ch == '\u{00A0}' { ' ' } else { ch };
                                        collector.selectable_chars.push(GlossaryCharCell {
                                            ch: norm_ch,
                                            x: x_cursor + per_w * ci as f32,
                                            y: line_top,
                                            w: per_w,
                                            h: line_h,
                                            line_id,
                                        });
                                    }
                                }
                            }
                        }
                        x_cursor += adv;
                    }
                }
                PositionedLayoutItem::InlineBox(ibox) => {
                    prev_run_key = None;
                    run_glyph_cursor = 0;
                    let bx = origin_x + ibox.x;
                    let by = origin_y + ibox.y;
                    if let Some(rbox) = ruby_boxes.iter().find(|r| r.box_id == ibox.id) {
                        engine.collect_glossary_glyphs_from_layout(
                            &rbox.ruby_layout,
                            bx + (rbox.w - rbox.ruby_w) * 0.5,
                            by,
                            false,
                            false,
                            collector,
                        );
                        engine.collect_glossary_glyphs_from_layout(
                            &rbox.base_layout,
                            bx + (rbox.w - rbox.base_w) * 0.5,
                            by + rbox.base_y,
                            rbox.style.underline,
                            rbox.style.strikethrough,
                            collector,
                        );

                        let base_char_count = rbox.base.chars().count();
                        let per_w = rbox.w / (base_char_count.max(1) as f32);
                        for (ci, ch) in rbox.base.chars().enumerate() {
                            collector.selectable_chars.push(GlossaryCharCell {
                                ch,
                                x: bx + per_w * ci as f32,
                                y: by + rbox.ruby_h,
                                w: per_w.max(2.0),
                                h: (rbox.h - rbox.ruby_h).max(12.0),
                                line_id,
                            });
                        }

                        let action = if let Some(did) = summary_detail_id {
                            Some(GlossaryZoneAction::ToggleDetails(did.to_owned()))
                        } else {
                            rbox.style.link_href.clone().map(GlossaryZoneAction::Link)
                        };
                        collector.hit_zones.push(GlossaryHitZone {
                            x: bx,
                            y: by,
                            w: rbox.w,
                            h: rbox.h,
                            action,
                            tooltip: rbox
                                .style
                                .tooltip
                                .clone()
                                .or_else(|| Some(format!("{}: {}", rbox.base, rbox.reading))),
                        });
                    } else if let Some(img) = image_boxes.iter().find(|im| im.box_id == ibox.id) {
                        let scale = collector.render_scale;
                        collector.images.push(PositionedGlossaryImage {
                            phys_x: (bx + 1.0) * scale,
                            phys_y: (by + 1.0) * scale,
                            phys_w: img.w * scale,
                            phys_h: img.h * scale,
                            pixmap: img.pixmap.clone(),
                        });
                        if img.link_href.is_some() || img.tooltip.is_some() {
                            collector.hit_zones.push(GlossaryHitZone {
                                x: bx,
                                y: by,
                                w: img.w + 2.0,
                                h: img.h + 2.0,
                                action: img.link_href.clone().map(GlossaryZoneAction::Link),
                                tooltip: img.tooltip.clone(),
                            });
                        }
                    }
                }
            }
        }
        if !collector.selectable_chars.is_empty()
            && collector.selectable_chars.last().is_some_and(|c| c.ch != '\n')
        {
            let line_slice = full_text.get(line_text_range.clone()).unwrap_or("");
            let trailing_hard_break = line_slice.ends_with('\n')
                || line_slice.ends_with('\r')
                || full_text
                    .as_bytes()
                    .get(line_text_range.end)
                    .is_some_and(|&b| b == b'\n' || b == b'\r');
            let is_soft_wrap = (line_idx + 1 < total_lines) && !trailing_hard_break;
            collector.selectable_chars.push(GlossaryCharCell {
                ch: '\n',
                x: origin_x + avail_w,
                y: line_top,
                w: 0.0,
                h: if is_soft_wrap { line_h } else { 0.0 },
                line_id,
            });
        }
    }

    (max_line_w.max(0.0), total_h)
}

#[derive(Clone)]
struct PlacedCell<'a> {
    cell: &'a TableCellIR,
    row: usize,
    col: usize,
    col_span: usize,
    row_span: usize,
}

#[allow(clippy::too_many_arguments)]
fn layout_table_block(
    engine: &mut TextEngine,
    rows: &[Vec<TableCellIR>],
    frame: &BoxFrameIR,
    x: f32,
    y: f32,
    avail_w: f32,
    collector: &mut PaintCollector,
    measure_only: bool,
    cache: &mut LayoutMeasureCache,
) -> (f32, f32) {
    let num_rows = rows.len();
    if num_rows == 0 {
        return (0.0, 0.0);
    }

    let (placed, num_cols) = place_table_cells(rows);
    if num_cols == 0 {
        return (0.0, 0.0);
    }

    let outer_x = x + frame.margin_left;
    let outer_y = y + frame.margin_top;
    let max_table_w = (avail_w - frame.margin_left - frame.margin_right).max(40.0);

    // Pass 1: memoized intrinsic width per cell block (each node measured at most once across
    // the entire document, eliminating exponential 3^depth re-layout on nested tables).
    let mut col_pref = vec![36.0f32; num_cols];
    for p in &placed {
        let pad_h = p.cell.frame.padding[LEFT] + p.cell.frame.padding[RIGHT] + 4.0;
        let mut natural_w = 24.0f32;
        for blk in &p.cell.blocks {
            let bw = measure_block_intrinsic_width(engine, blk, cache).min(max_table_w);
            natural_w = natural_w.max(bw);
        }
        let needed = (natural_w + pad_h) / (p.col_span as f32);
        for dc in 0..p.col_span {
            col_pref[p.col + dc] = col_pref[p.col + dc].max(needed);
        }
    }

    let sum_pref: f32 = col_pref.iter().sum();
    let col_widths: Vec<f32> = if sum_pref > max_table_w && sum_pref > 0.0 {
        col_pref
            .iter()
            .map(|w| (w * max_table_w / sum_pref).max(22.0))
            .collect()
    } else {
        col_pref
    };
    let table_w: f32 = col_widths.iter().sum::<f32>().min(max_table_w);

    let mut col_offsets = Vec::with_capacity(num_cols + 1);
    let mut acc_x = 0.0f32;
    for &cw in &col_widths {
        col_offsets.push(acc_x);
        acc_x += cw;
    }
    col_offsets.push(acc_x);

    // Pass 2: single final column-width layout pass — when painting (`!measure_only`), collect
    // each cell directly at its final `inner_x` into a cell-local `PaintCollector` so row heights
    // and paint items are produced together with zero 3rd re-layout pass.
    let mut row_heights = vec![22.0f32; num_rows];
    let mut cell_measured_heights = Vec::with_capacity(placed.len());
    let mut cell_collectors: Vec<PaintCollector> = if measure_only {
        Vec::new()
    } else {
        Vec::with_capacity(placed.len())
    };
    let mut dummy_collector = collector.spawn_child();

    for p in &placed {
        let cx = outer_x + col_offsets[p.col];
        let cw = (col_offsets[p.col + p.col_span] - col_offsets[p.col]).max(18.0);
        let inner_x = cx + p.cell.frame.padding[LEFT] + 1.0;
        let inner_w =
            (cw - p.cell.frame.padding[LEFT] - p.cell.frame.padding[RIGHT] - 2.0).max(12.0);

        let mut ch_sum = 0.0f32;
        if measure_only {
            for (idx, blk) in p.cell.blocks.iter().enumerate() {
                if idx > 0 {
                    ch_sum += if collector.compact_glossaries { 1.0 } else { 2.0 };
                }
                let (_, bh) = layout_and_collect_block(
                    engine,
                    blk,
                    inner_x,
                    ch_sum,
                    inner_w,
                    &mut dummy_collector,
                    true,
                    cache,
                );
                ch_sum += bh;
            }
        } else {
            let mut cell_collector = collector.spawn_child();
            for (idx, blk) in p.cell.blocks.iter().enumerate() {
                if idx > 0 {
                    ch_sum += if collector.compact_glossaries { 1.0 } else { 2.0 };
                }
                let (_, bh) = layout_and_collect_block(
                    engine,
                    blk,
                    inner_x,
                    ch_sum,
                    inner_w,
                    &mut cell_collector,
                    false,
                    cache,
                );
                ch_sum += bh;
            }
            cell_collectors.push(cell_collector);
        }

        let total_ch = ch_sum + p.cell.frame.padding[TOP] + p.cell.frame.padding[BOTTOM] + 2.0;
        cell_measured_heights.push(total_ch);
        if p.row_span == 1 {
            row_heights[p.row] = row_heights[p.row].max(total_ch);
        }
    }
    for (p, &total_ch) in placed.iter().zip(&cell_measured_heights) {
        if p.row_span > 1 {
            let spanned: f32 = row_heights[p.row..p.row + p.row_span].iter().sum();
            if total_ch > spanned {
                let extra = (total_ch - spanned) / (p.row_span as f32);
                for r in p.row..p.row + p.row_span {
                    row_heights[r] += extra;
                }
            }
        }
    }

    let mut row_offsets = Vec::with_capacity(num_rows + 1);
    let mut acc_y = 0.0f32;
    for &rh in &row_heights {
        row_offsets.push(acc_y);
        acc_y += rh;
    }
    row_offsets.push(acc_y);
    let table_h = acc_y;

    if !measure_only {
        collector.push_frame(outer_x, outer_y, table_w, table_h, frame);
        for (p, cell_collector) in placed.iter().zip(cell_collectors) {
            let cx = outer_x + col_offsets[p.col];
            let cy = outer_y + row_offsets[p.row];
            let cw = col_offsets[p.col + p.col_span] - col_offsets[p.col];
            let ch = row_offsets[p.row + p.row_span] - row_offsets[p.row];
            collector.push_frame(cx, cy, cw, ch, &p.cell.frame);
            let inner_y = cy + p.cell.frame.padding[TOP] + 1.0;
            collector.append_shifted_y(cell_collector, inner_y);
        }
    }

    (
        frame.margin_left + table_w + frame.margin_right,
        frame.margin_top + table_h + frame.margin_bottom,
    )
}

/// Build and rasterize a rich structured-content glossary entry into a `RenderedGlossaryDoc`
/// searching an ordered slice of dictionary resource directories (`config_dir/dict_resources`, `data_dir/res`).
#[allow(clippy::too_many_arguments)]
pub fn render_structured_glossary_entry_in_dirs(
    raw_glossary: &str,
    fallback_glossary: &str,
    stylesheet: Option<Arc<ParsedStylesheet>>,
    detail_states: &HashMap<String, bool>,
    legacy_style: LegacyGlossaryStyle,
    dict_resource_dirs: &[PathBuf],
    dic_id: i64,
    dict_name: &str,
    avail_width_px: f32,
    selected: bool,
) -> Option<RenderedGlossaryDoc> {
    render_structured_glossary_entry_in_dirs_with_theme(
        raw_glossary,
        fallback_glossary,
        stylesheet,
        detail_states,
        legacy_style,
        dict_resource_dirs,
        dic_id,
        dict_name,
        avail_width_px,
        selected,
        false,
    )
}

/// Theme-aware variant of [`render_structured_glossary_entry_in_dirs`].
/// When `is_light_theme` is `true`, uses light popup defaults and passes `CssTheme::Light`
/// into `subomi-definition` stylesheet / `:root[data-theme="light"]` variable resolution.
#[allow(dead_code, clippy::too_many_arguments)]
pub fn render_structured_glossary_entry_in_dirs_with_theme(
    raw_glossary: &str,
    fallback_glossary: &str,
    stylesheet: Option<Arc<ParsedStylesheet>>,
    detail_states: &HashMap<String, bool>,
    legacy_style: LegacyGlossaryStyle,
    dict_resource_dirs: &[PathBuf],
    dic_id: i64,
    dict_name: &str,
    avail_width_px: f32,
    selected: bool,
    is_light_theme: bool,
) -> Option<RenderedGlossaryDoc> {
    let css_theme = if is_light_theme {
        CssTheme::Light
    } else {
        CssTheme::Dark
    };
    render_structured_glossary_entry_with_css_theme(
        raw_glossary,
        fallback_glossary,
        stylesheet,
        detail_states,
        legacy_style,
        dict_resource_dirs,
        dic_id,
        dict_name,
        avail_width_px,
        selected,
        css_theme,
        false,
    )
}

/// Build and rasterize a structured-content glossary entry with an explicit [`CssTheme`]
/// (`Dark` or `Light`) and Yomitan-aligned `compact_glossaries` flag.
#[allow(dead_code, clippy::too_many_arguments)]
pub fn render_structured_glossary_entry_with_css_theme(
    raw_glossary: &str,
    fallback_glossary: &str,
    stylesheet: Option<Arc<ParsedStylesheet>>,
    detail_states: &HashMap<String, bool>,
    legacy_style: LegacyGlossaryStyle,
    dict_resource_dirs: &[PathBuf],
    dic_id: i64,
    dict_name: &str,
    avail_width_px: f32,
    selected: bool,
    css_theme: CssTheme,
    compact_glossaries: bool,
) -> Option<RenderedGlossaryDoc> {
    let mut theme = PopupCssTheme::default_for_theme(css_theme);
    theme.compact_glossaries = compact_glossaries;
    render_structured_glossary_entry_with_theme(
        raw_glossary,
        fallback_glossary,
        stylesheet,
        detail_states,
        legacy_style,
        dict_resource_dirs,
        dic_id,
        dict_name,
        avail_width_px,
        selected,
        &theme,
    )
}

/// Quantize a window's logical-to-physical DPI scale factor to `0.25` steps in `[1.0, 3.0]`
/// for true 1:1 physical-pixel glossary rasterization and stable cache keys.
pub(super) fn quantize_glossary_render_scale(window_scale: f32) -> (f32, u16) {
    let raw = if window_scale.is_finite() && window_scale > 0.0 {
        window_scale
    } else {
        1.0
    };
    let clamped = raw.clamp(1.0, 3.0);
    let quarters = (clamped * 4.0).round().clamp(4.0, 12.0) as u16;
    (f32::from(quarters) * 0.25, quarters)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn render_structured_glossary_entry_with_theme(
    raw_glossary: &str,
    fallback_glossary: &str,
    stylesheet: Option<Arc<ParsedStylesheet>>,
    detail_states: &HashMap<String, bool>,
    legacy_style: LegacyGlossaryStyle,
    dict_resource_dirs: &[PathBuf],
    dic_id: i64,
    dict_name: &str,
    avail_width_px: f32,
    selected: bool,
    theme: &PopupCssTheme,
) -> Option<RenderedGlossaryDoc> {
    render_structured_glossary_entry_with_theme_scaled(
        raw_glossary,
        fallback_glossary,
        stylesheet,
        detail_states,
        legacy_style,
        dict_resource_dirs,
        dic_id,
        dict_name,
        avail_width_px,
        selected,
        theme,
        1.0,
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn render_structured_glossary_entry_with_theme_scaled(
    raw_glossary: &str,
    fallback_glossary: &str,
    stylesheet: Option<Arc<ParsedStylesheet>>,
    detail_states: &HashMap<String, bool>,
    legacy_style: LegacyGlossaryStyle,
    dict_resource_dirs: &[PathBuf],
    dic_id: i64,
    dict_name: &str,
    avail_width_px: f32,
    selected: bool,
    theme: &PopupCssTheme,
    render_scale: f32,
) -> Option<RenderedGlossaryDoc> {
    render_structured_glossary_entry_with_value_scaled(
        raw_glossary,
        fallback_glossary,
        None,
        stylesheet,
        detail_states,
        legacy_style,
        dict_resource_dirs,
        dic_id,
        dict_name,
        avail_width_px,
        selected,
        theme,
        render_scale,
    )
}

/// Build and rasterize a rich structured-content glossary entry directly from a pre-parsed
/// `serde_json::Value` (when provided by the caller) or a single `serde_json::from_str` pass
/// using a caller-owned `TextEngine` (UI thread or background `GlossaryRenderWorker`).
#[allow(clippy::too_many_arguments)]
pub(super) fn rasterize_structured_glossary_entry_with_engine(
    engine: &mut TextEngine,
    raw_glossary: &str,
    fallback_glossary: &str,
    parsed_value: Option<&JsonValue>,
    stylesheet: Option<Arc<ParsedStylesheet>>,
    detail_states: &HashMap<String, bool>,
    legacy_style: LegacyGlossaryStyle,
    dict_resource_dirs: &[PathBuf],
    dic_id: i64,
    dict_name: &str,
    avail_width_px: f32,
    selected: bool,
    theme: &PopupCssTheme,
    render_scale: f32,
) -> Option<RasterizedGlossaryDoc> {
    let scale = render_scale.clamp(1.0, 3.0);
    let avail_w = avail_width_px.clamp(120.0, 960.0);
    let mut base_text_color = if selected {
        theme.text_color
    } else {
        theme.muted_color
    };
    let text_color_css = rgba_to_css_color(base_text_color);
    let gloss_text_css = rgba_to_css_color(theme.text_color);
    let bg_color_css = rgba_to_css_color(theme.popup_bg);
    let root_font_px = f64::from(theme.glossary_font_size.clamp(9.0, 32.0));

    let owned_parsed: Option<JsonValue> = if parsed_value.is_some() {
        None
    } else if !raw_glossary.trim().is_empty() {
        serde_json::from_str::<JsonValue>(raw_glossary).ok()
    } else if !fallback_glossary.trim().is_empty() {
        Some(JsonValue::Array(vec![JsonValue::String(
            fallback_glossary.to_owned(),
        )]))
    } else {
        None
    };

    let root_value: &JsonValue = parsed_value.or(owned_parsed.as_ref())?;
    let entries: &[JsonValue] = match root_value {
        JsonValue::Array(items) => items.as_slice(),
        single => std::slice::from_ref(single),
    };
    if entries.is_empty() {
        return None;
    }

    let renderer = DefRenderer::new();
    let contains_structured = renderer.contains_structured_content_value(entries);
    let is_light_theme = theme.is_light_theme();
    let mut ctx = DefContext {
        screen: Some(ScreenMetrics {
            available_width: avail_w as i32,
            available_height: 440,
            logical_dpi: 96.0,
        }),
        root_font_pixel_size: root_font_px,
        parent_font_pixel_size: root_font_px,
        text_color: text_color_css,
        glossary_text_color: gloss_text_css,
        glossary_background_color: bg_color_css.clone(),
        theme: theme.css_theme,
        cached_css_theme: theme.css_theme,
        background_color: bg_color_css.clone(),
        painted_background_color: bg_color_css,
        needs_element_child_count: stylesheet
            .as_ref()
            .is_some_and(|s| s.uses_element_child_count),
        dictionary_styles: stylesheet,
        text_direction: LayoutDirection::LeftToRight,
        detail_states: detail_states.clone(),
        ..DefContext::default()
    };

    // Virtual dictionary container so Yomitan `[data-dictionary="..."]` / `[data-dictionary^="..."]`,
    // `[data-theme="dark"|"light"]`, `[data-compact-glossaries="true"]`, and `.gloss-item` rules
    // match without interfering with bare tag selectors (`div`, `li`) in dictionary `styles.css`.
    let mut dict_root = mdef::StructuredElement::new();
    dict_root.tag = "subomi-dict".to_owned();
    dict_root.classes.insert("gloss-item".to_owned());
    dict_root.classes.insert("yomitan-glossary".to_owned());
    dict_root
        .attributes
        .insert("data-dictionary".to_owned(), dict_name.to_owned());
    dict_root.attributes.insert(
        "data-theme".to_owned(),
        theme.css_theme.as_data_theme().to_owned(),
    );
    if theme.compact_glossaries {
        dict_root
            .attributes
            .insert("data-compact-glossaries".to_owned(), "true".to_owned());
    }
    ctx.selector_elements.push(dict_root);
    ctx.elements.push(0);

    let mut root_inline = InlineStyleIR {
        font_family: theme.font_family.clone(),
        font_px: theme.glossary_font_size.clamp(9.0, 32.0),
        color: base_text_color,
        ..InlineStyleIR::default()
    };

    // Apply any dictionary-root rules (e.g. `[data-dictionary^="JMdict"] { font-size: 16px; }`).
    let mut root_hidden = false;
    if ctx.dictionary_styles.is_some() {
        let mut root_decls = mdef::CssDeclarations::new();
        renderer.add_matching_css_rules(&mut ctx, &mut root_decls, None, None);
        if root_decls
            .get("display")
            .is_some_and(|v| v.trim().eq_ignore_ascii_case("none"))
        {
            root_hidden = true;
        }
        if let Some(fs) = root_decls.get("font-size") {
            let px = mdef::helpers::css_font_size_to_pixels(
                fs,
                ctx.screen.as_ref(),
                ctx.parent_font_pixel_size,
                ctx.root_font_pixel_size,
            );
            if px > 0.0 {
                ctx.parent_font_pixel_size = px;
                root_inline.font_px = (px as f32).clamp(9.0, 32.0);
            }
        }
        if let Some(ff) = root_decls.get("font-family") {
            let cleaned = clean_css_font_family(ff);
            if !cleaned.is_empty() {
                root_inline.font_family = cleaned;
            }
        }
        if let Some(col) = root_decls.get("color") {
            if let Some(rgba) = parse_css_theme_color(col) {
                base_text_color = rgba;
                root_inline.color = rgba;
                ctx.text_color = rgba_to_css_color(rgba);
            }
        }
        root_inline.weight = parse_weight_css(root_decls.get("font-weight"), root_inline.weight);
        if let Some(st) = root_decls.get("font-style") {
            let l = st.trim().to_ascii_lowercase();
            if l == "italic" || l == "oblique" {
                root_inline.italic = true;
            } else if l == "normal" {
                root_inline.italic = false;
            }
        }
    }

    let (top_blocks, had_hidden_elements) = {
        let mut builder = GlossaryBuilder {
            engine,
            renderer,
            ctx,
            dict_resource_dirs,
            dic_id,
            dict_name,
            default_link_color: theme.link_color,
            is_light_theme,
            compact_glossaries: theme.compact_glossaries,
            render_scale: scale,
            had_hidden_elements: root_hidden,
        };

        let mut top_blocks: Vec<BlockIR> = Vec::new();
        let use_pipe_inline = legacy_style == LegacyGlossaryStyle::Pipe
            || (theme.compact_glossaries && !contains_structured);
        let use_bullets = !use_pipe_inline
            && legacy_style == LegacyGlossaryStyle::Bullet
            && !contains_structured;
        let mut pending_pipe_atoms: Vec<InlineAtom> = Vec::new();

        let flush_pending_pipe =
            |pending: &mut Vec<InlineAtom>, blocks_out: &mut Vec<BlockIR>| {
                if !pending.is_empty() {
                    blocks_out.push(BlockIR::Paragraph {
                        atoms: std::mem::take(pending),
                        frame: BoxFrameIR::default(),
                        marker: None,
                        summary_detail_id: None,
                    });
                }
            };

        if !root_hidden {
            for entry in entries {
                match entry {
                    JsonValue::String(text) => {
                        if use_pipe_inline {
                            if !text.is_empty() {
                                if !pending_pipe_atoms.is_empty() {
                                    pending_pipe_atoms.push(InlineAtom::Text {
                                        text: " | ".to_owned(),
                                        style: root_inline.clone(),
                                        selectable: true,
                                    });
                                }
                                push_legacy_text_atoms(text, &root_inline, &mut pending_pipe_atoms);
                            }
                        } else {
                            let mut atoms = Vec::new();
                            push_legacy_text_atoms(text, &root_inline, &mut atoms);
                            if !atoms.is_empty() {
                                top_blocks.push(BlockIR::Paragraph {
                                    atoms,
                                    frame: BoxFrameIR::default(),
                                    marker: use_bullets.then(|| {
                                        ("•".to_owned(), base_text_color, root_inline.font_px)
                                    }),
                                    summary_detail_id: None,
                                });
                            }
                        }
                    }
                    JsonValue::Object(_) => {
                        match entry.get("type").and_then(JsonValue::as_str).unwrap_or("") {
                            "structured-content" => {
                                flush_pending_pipe(&mut pending_pipe_atoms, &mut top_blocks);
                                if let Some(content) = entry.get("content") {
                                    let blocks =
                                        builder.build_children_blocks(content, &root_inline);
                                    top_blocks.extend(blocks);
                                }
                            }
                            "image" => {
                                flush_pending_pipe(&mut pending_pipe_atoms, &mut top_blocks);
                                if let Some(atom) = builder.resolve_image_atom(entry, &root_inline)
                                {
                                    let mut atoms = vec![atom];
                                    if let Some(desc) =
                                        entry.get("description").and_then(JsonValue::as_str)
                                    {
                                        if !desc.is_empty() {
                                            atoms.push(InlineAtom::LineBreak);
                                            atoms.push(InlineAtom::Text {
                                                text: desc.to_owned(),
                                                style: root_inline.clone(),
                                                selectable: true,
                                            });
                                        }
                                    }
                                    top_blocks.push(BlockIR::Paragraph {
                                        atoms,
                                        frame: BoxFrameIR::default(),
                                        marker: None,
                                        summary_detail_id: None,
                                    });
                                }
                            }
                            "text" => {
                                let txt =
                                    entry.get("text").and_then(JsonValue::as_str).unwrap_or("");
                                if !txt.is_empty() {
                                    if use_pipe_inline {
                                        if !pending_pipe_atoms.is_empty() {
                                            pending_pipe_atoms.push(InlineAtom::Text {
                                                text: " | ".to_owned(),
                                                style: root_inline.clone(),
                                                selectable: true,
                                            });
                                        }
                                        push_legacy_text_atoms(
                                            txt,
                                            &root_inline,
                                            &mut pending_pipe_atoms,
                                        );
                                    } else {
                                        let mut atoms = Vec::new();
                                        push_legacy_text_atoms(txt, &root_inline, &mut atoms);
                                        if !atoms.is_empty() {
                                            top_blocks.push(BlockIR::Paragraph {
                                                atoms,
                                                frame: BoxFrameIR::default(),
                                                marker: use_bullets.then(|| {
                                                    (
                                                        "•".to_owned(),
                                                        base_text_color,
                                                        root_inline.font_px,
                                                    )
                                                }),
                                                summary_detail_id: None,
                                            });
                                        }
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                    _ => {}
                }
            }
            flush_pending_pipe(&mut pending_pipe_atoms, &mut top_blocks);
        }
        (top_blocks, builder.had_hidden_elements)
    };

    if top_blocks.is_empty() {
        if had_hidden_elements {
            let empty_pm = tiny_skia::Pixmap::new(1, 1)?;
            return Some(RasterizedGlossaryDoc {
                width_px: 1.0,
                height_px: 1.0,
                pixels: pixmap_to_pixel_buffer(&empty_pm),
                hit_zones: Vec::new(),
                selectable_chars: Vec::new(),
                line_bottoms: Vec::new(),
                plain_text: String::new(),
            });
        }
        return None;
    }

    let ruby_color = if is_light_theme {
        [91, 33, 182, 255]
    } else {
        [196, 181, 253, 255]
    };
    let mut collector = PaintCollector::with_style(
        scale,
        base_text_color,
        ruby_color,
        theme.compact_glossaries,
    );
    let mut layout_cache = LayoutMeasureCache::default();
    let top_block_gap = if theme.compact_glossaries { 2.0 } else { 4.0 };
    let mut cur_y = 2.0f32;
    for (idx, blk) in top_blocks.iter().enumerate() {
        if idx > 0 {
            cur_y += top_block_gap;
        }
        let (_, bh) = layout_and_collect_block(
            engine,
            blk,
            0.0,
            cur_y,
            avail_w,
            &mut collector,
            false,
            &mut layout_cache,
        );
        cur_y += bh;
    }
    let logical_w = avail_w.ceil().clamp(1.0, 1400.0);
    let max_logical_h = (16384.0 / scale).floor().min(8192.0);
    let logical_h = (cur_y + 2.0).ceil().clamp(18.0, max_logical_h);
    let pw = (logical_w * scale).ceil().clamp(1.0, 4200.0) as u32;
    let ph = (logical_h * scale).ceil().clamp(1.0, 16384.0) as u32;
    let mut pixmap = tiny_skia::Pixmap::new(pw, ph)?;

    for &(rx, ry, rw, rh, col) in &collector.rects {
        if let Some(rect) = tiny_skia::Rect::from_xywh(
            (rx * scale).round(),
            (ry * scale).round(),
            (rw * scale).round().max(1.0),
            (rh * scale).round().max(1.0),
        ) {
            let mut paint = tiny_skia::Paint::default();
            paint.set_color_rgba8(col[0], col[1], col[2], col[3]);
            pixmap.fill_rect(rect, &paint, tiny_skia::Transform::default(), None);
        }
    }

    let img_paint = tiny_skia::PixmapPaint {
        opacity: 1.0,
        blend_mode: tiny_skia::BlendMode::SourceOver,
        quality: tiny_skia::FilterQuality::Bilinear,
    };
    for img in &collector.images {
        let src_w = img.pixmap.width().max(1) as f32;
        let src_h = img.pixmap.height().max(1) as f32;
        if (src_w - img.phys_w).abs() <= 0.75 && (src_h - img.phys_h).abs() <= 0.75 {
            pixmap.draw_pixmap(
                img.phys_x.round() as i32,
                img.phys_y.round() as i32,
                (*img.pixmap).as_ref(),
                &img_paint,
                tiny_skia::Transform::default(),
                None,
            );
        } else {
            let sx = img.phys_w / src_w;
            let sy = img.phys_h / src_h;
            let xform =
                tiny_skia::Transform::from_scale(sx, sy).post_translate(img.phys_x, img.phys_y);
            pixmap.draw_pixmap(0, 0, (*img.pixmap).as_ref(), &img_paint, xform, None);
        }
    }

    let card_bg = theme.resolve_dict_style(dict_name).card_bg;
    let backdrop_rgb = composite_rgb_over_opaque(card_bg, theme.popup_bg);
    for mg in &collector.mask_glyphs {
        blit_alpha_glyph(&mut pixmap, mg.x, mg.y, &mg.glyph, mg.rgba, backdrop_rgb);
    }

    while collector
        .selectable_chars
        .last()
        .is_some_and(|c| c.ch == '\n')
    {
        collector.selectable_chars.pop();
    }
    let plain_text: String = collector.selectable_chars.iter().map(|c| c.ch).collect();
    let line_bottoms = compute_visual_line_bottoms(&collector.line_bounds);

    Some(RasterizedGlossaryDoc {
        width_px: pw as f32 / scale,
        height_px: ph as f32 / scale,
        pixels: pixmap_to_pixel_buffer(&pixmap),
        hit_zones: collector.hit_zones,
        selectable_chars: collector.selectable_chars,
        line_bottoms,
        plain_text,
    })
}

/// Build and rasterize a rich structured-content glossary entry directly from a pre-parsed
/// `serde_json::Value` (when provided by the caller) or a single `serde_json::from_str` pass.
#[allow(clippy::too_many_arguments)]
pub(super) fn render_structured_glossary_entry_with_value_scaled(
    raw_glossary: &str,
    fallback_glossary: &str,
    parsed_value: Option<&JsonValue>,
    stylesheet: Option<Arc<ParsedStylesheet>>,
    detail_states: &HashMap<String, bool>,
    legacy_style: LegacyGlossaryStyle,
    dict_resource_dirs: &[PathBuf],
    dic_id: i64,
    dict_name: &str,
    avail_width_px: f32,
    selected: bool,
    theme: &PopupCssTheme,
    render_scale: f32,
) -> Option<RenderedGlossaryDoc> {
    with_shared_text_engine(|engine| {
        rasterize_structured_glossary_entry_with_engine(
            engine,
            raw_glossary,
            fallback_glossary,
            parsed_value,
            stylesheet,
            detail_states,
            legacy_style,
            dict_resource_dirs,
            dic_id,
            dict_name,
            avail_width_px,
            selected,
            theme,
            render_scale,
        )
    })
    .map(RasterizedGlossaryDoc::into_rendered_doc)
}

/// Build and rasterize a rich structured-content glossary entry into a `RenderedGlossaryDoc`
/// with full `subomi-definition` CSS styling, tables, ruby, details, links, images/SVGs, and
/// per-character selection hit-boxes.
#[allow(clippy::too_many_arguments)]
pub fn render_structured_glossary_entry(
    raw_glossary: &str,
    fallback_glossary: &str,
    stylesheet: Option<Arc<ParsedStylesheet>>,
    detail_states: &HashMap<String, bool>,
    legacy_style: LegacyGlossaryStyle,
    dict_resources_dir: &Path,
    dic_id: i64,
    dict_name: &str,
    avail_width_px: f32,
    selected: bool,
) -> Option<RenderedGlossaryDoc> {
    let dirs = [dict_resources_dir.to_path_buf()];
    render_structured_glossary_entry_in_dirs(
        raw_glossary,
        fallback_glossary,
        stylesheet,
        detail_states,
        legacy_style,
        &dirs,
        dic_id,
        dict_name,
        avail_width_px,
        selected,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_pipe_style_and_text_objects_join_inline_and_split_newlines() {
        let raw_strings = r#"["first meaning", "second meaning", "third meaning"]"#;
        let doc = render_structured_glossary_entry_in_dirs(
            raw_strings,
            "",
            None,
            &HashMap::new(),
            LegacyGlossaryStyle::Pipe,
            &[],
            1,
            "JMdict",
            480.0,
            true,
        )
        .expect("pipe legacy glossary should render");
        assert_eq!(doc.plain_text, "first meaning | second meaning | third meaning");
        assert!(
            !doc.plain_text.ends_with('|') && !doc.plain_text.ends_with("| "),
            "pipe glossary must not end with trailing pipe"
        );
        assert_eq!(
            doc.line_bottoms.len(),
            1,
            "short Pipe entries must join inline on one line instead of separate paragraphs"
        );

        let raw_text_objs =
            r#"[{"type":"text","text":"alpha\nbeta"},{"type":"text","text":"gamma"}]"#;
        let doc_text = render_structured_glossary_entry_in_dirs(
            raw_text_objs,
            "",
            None,
            &HashMap::new(),
            LegacyGlossaryStyle::Pipe,
            &[],
            1,
            "JMdict",
            480.0,
            true,
        )
        .expect("legacy text objects should render");
        assert_eq!(doc_text.plain_text, "alpha\nbeta | gamma");
        assert_eq!(
            doc_text.line_bottoms.len(),
            2,
            "embedded '\\n' in legacy text object must split into 2 lines while Pipe joins second object inline"
        );
    }

    #[test]
    fn compact_glossaries_flattens_inline_ul_lists_and_respects_light_theme() {
        let raw_ul = r#"[{
            "type": "structured-content",
            "content": {
                "tag": "ul",
                "content": [
                    {"tag": "li", "content": "one"},
                    {"tag": "li", "content": "two"},
                    {"tag": "li", "content": "three"}
                ]
            }
        }]"#;
        let normal = render_structured_glossary_entry_with_css_theme(
            raw_ul,
            "",
            None,
            &HashMap::new(),
            LegacyGlossaryStyle::Bullet,
            &[],
            1,
            "JMdict",
            480.0,
            true,
            CssTheme::Dark,
            false,
        )
        .expect("normal ul should render");
        let compact = render_structured_glossary_entry_with_css_theme(
            raw_ul,
            "",
            None,
            &HashMap::new(),
            LegacyGlossaryStyle::Bullet,
            &[],
            1,
            "JMdict",
            480.0,
            true,
            CssTheme::Light,
            true,
        )
        .expect("compact light ul should render");

        assert_eq!(compact.plain_text, "one | two | three");
        assert!(
            compact.height_px < normal.height_px,
            "compact glossary height ({}) should be smaller than stacked ul height ({})",
            compact.height_px,
            normal.height_px
        );

        // Verify dark vs light `:root[data-theme="..."]` CSS custom property resolution
        // produces genuinely different rendered text colors.
        let sheet = Arc::new(mdef::parse_stylesheet(
            r#"
            :root[data-theme="dark"] { --accent: #ff0000; }
            :root[data-theme="light"] { --accent: #0000ff; }
            span { color: var(--accent); }
            "#,
        ));
        let raw_var = r#"[{"type":"structured-content","content":{"tag":"span","content":"色"}}]"#;
        let dark_doc = render_structured_glossary_entry_in_dirs_with_theme(
            raw_var,
            "",
            Some(sheet.clone()),
            &HashMap::new(),
            LegacyGlossaryStyle::Bullet,
            &[],
            1,
            "ThemeDict",
            200.0,
            true,
            false,
        )
        .expect("dark themed doc");
        let light_doc = render_structured_glossary_entry_in_dirs_with_theme(
            raw_var,
            "",
            Some(sheet),
            &HashMap::new(),
            LegacyGlossaryStyle::Bullet,
            &[],
            1,
            "ThemeDict",
            200.0,
            true,
            true,
        )
        .expect("light themed doc");
        let dark_buf = dark_doc.image.to_rgba8().unwrap();
        let light_buf = light_doc.image.to_rgba8().unwrap();
        let dark_red_sum: u64 = dark_buf.as_slice().iter().map(|p| u64::from(p.r)).sum();
        let dark_blue_sum: u64 = dark_buf.as_slice().iter().map(|p| u64::from(p.b)).sum();
        let light_red_sum: u64 = light_buf.as_slice().iter().map(|p| u64::from(p.r)).sum();
        let light_blue_sum: u64 = light_buf.as_slice().iter().map(|p| u64::from(p.b)).sum();
        assert!(
            dark_red_sum > dark_blue_sum * 2,
            "dark theme should resolve --accent to red (R={dark_red_sum}, B={dark_blue_sum})"
        );
        assert!(
            light_blue_sum > light_red_sum * 2,
            "light theme should resolve --accent to blue (R={light_red_sum}, B={light_blue_sum})"
        );
    }

    #[test]
    fn last_word_without_trailing_punctuation_scans_cleanly_without_bleeding_across_hard_breaks() {
        // Two separate lines without trailing punctuation (#2232)
        let raw = r#"["食べる", "飲む"]"#;
        let doc = render_structured_glossary_entry_in_dirs(
            raw,
            "",
            None,
            &HashMap::new(),
            LegacyGlossaryStyle::LineBreak,
            &[],
            1,
            "JMdict",
            360.0,
            true,
        )
        .expect("two-line doc should render");

        let ta_idx = doc
            .selectable_chars
            .iter()
            .position(|c| c.ch == '食')
            .expect("must contain 食");
        let ru_idx = doc
            .selectable_chars
            .iter()
            .position(|c| c.ch == 'る')
            .expect("must contain る");
        let ru_cell = &doc.selectable_chars[ru_idx];

        // Hovering the right edge of the last character on line 1 (without trailing punctuation)
        // must still hit 'る', while far blank margin must still return None.
        assert_eq!(
            strict_char_index_at_point(&doc, ru_cell.x + ru_cell.w + 1.8, ru_cell.y + ru_cell.h * 0.5),
            Some(ru_idx)
        );
        assert_eq!(
            strict_char_index_at_point(&doc, ru_cell.x + ru_cell.w + 40.0, ru_cell.y + ru_cell.h * 0.5),
            None
        );

        // Scanning starting at '食' on line 1 must stop at the hard paragraph break and NOT
        // bleed into '飲む' on line 2.
        let (sub, idxs) = extract_glossary_scan_substring(&doc.selectable_chars, ta_idx, 24)
            .expect("must extract line 1 word");
        assert_eq!(sub, "食べる");
        assert_eq!(idxs.len(), 3);

        // Scanning the very last char of the entire doc ('む', no trailing punctuation) must succeed.
        let mu_idx = doc
            .selectable_chars
            .iter()
            .position(|c| c.ch == 'む')
            .expect("must contain む");
        let (last_sub, _) = extract_glossary_scan_substring(&doc.selectable_chars, mu_idx, 24)
            .expect("final char without trailing punctuation must scan");
        assert_eq!(last_sub, "む");
    }
}

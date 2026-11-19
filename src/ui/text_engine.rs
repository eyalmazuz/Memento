//! Shared Parley + Swash + Tiny-Skia text shaping, glyph caching, and
//! direct vector subtitle rasterization engine.

use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::Hash;
use std::sync::{Arc, OnceLock};

use memento_setting::keys as raw;
use parley::{
    Alignment, AlignmentOptions, FontContext, FontFamily, FontStyle, FontWeight, Layout,
    LayoutContext, LineHeight, PositionedLayoutItem, StyleProperty,
};
use resvg::tiny_skia;
use resvg::usvg;
use slint::{Rgba8Pixel, SharedPixelBuffer};

use super::glossary_render::{PaintCollector, PositionedGlossaryImage};
use super::subtitle_layout::active_subtitle_render_style;
use super::SubtitleWord;

const SUBTITLE_CACHE_MAX_ENTRIES: usize = 512;
const EMOJI_CACHE_MAX_ENTRIES: usize = 2048;
const ALPHA_GLYPH_CACHE_MAX_ENTRIES: usize = 32768;

pub(super) const RICH_EMOJI_FALLBACK_STACK: &str =
    "Noto Sans CJK JP, Noto Color Emoji, Apple Color Emoji, Segoe UI Emoji, sans-serif";

#[derive(Clone, Debug, PartialEq)]
pub struct SubtitleRenderStyle {
    pub font_family: String,
    pub text_rgba: [u8; 4],
    pub stroke_rgba: [u8; 4],
    pub bg_rgba: [u8; 4],
    pub stroke_size: f32,
    pub furigana_ratio: f32,
    pub line_spacing: f32,
    pub bold: bool,
    pub italic: bool,
}

impl Default for SubtitleRenderStyle {
    fn default() -> Self {
        Self {
            font_family: "Noto Sans CJK JP".to_owned(),
            text_rgba: [255, 255, 255, 255],
            stroke_rgba: [0, 0, 0, 255],
            bg_rgba: [0, 0, 0, 0],
            stroke_size: 6.0,
            furigana_ratio: 0.4,
            line_spacing: raw::interface::subtitle::LINE_SPACING_DEFAULT as f32,
            bold: true,
            italic: false,
        }
    }
}

#[derive(Clone, Debug)]
struct RegisteredFontInfo {
    family: String,
    #[allow(dead_code)]
    weight: u16,
    #[allow(dead_code)]
    italic: bool,
}

#[derive(Clone)]
pub(super) struct RenderedColorGlyph {
    pub(super) pixmap: Arc<tiny_skia::Pixmap>,
    pub(super) left: f32,
    pub(super) top: f32,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct SubtitleClusterHitbox {
    pub(super) x0: f32,
    pub(super) x1: f32,
    pub(super) byte_offset: usize,
    pub(super) char_offset: usize,
}

#[derive(Clone)]
pub struct ShapedSubtitleToken {
    pub width_px: f32,
    pub height_px: f32,
    pub bleed_x_px: f32,
    pub glyph_x_px: f32,
    pub glyph_y_px: f32,
    pub glyph_width_px: f32,
    pub glyph_height_px: f32,
    /// `(byte_start, byte_end, x_start, advance)` for each cluster in `word.text`.
    pub clusters: Vec<(usize, usize, f32, f32)>,
    /// Precomputed hitboxes for zero-reshaping hover hit-testing in
    /// `subtitle_cluster_at_point_with_style`.
    pub(super) cluster_hitboxes: Vec<SubtitleClusterHitbox>,
    pub image: slint::Image,
}

#[derive(Hash, PartialEq, Eq, Clone)]
struct SubtitleCacheKey {
    text: String,
    reading: String,
    font_tenth_px: i32,
    show_furigana: bool,
    font_family: String,
    text_rgba: [u8; 4],
    stroke_rgba: [u8; 4],
    stroke_tenth: i32,
    furigana_pct: i32,
    line_spacing_tenth: i32,
    bold: bool,
    italic: bool,
}

#[derive(Clone)]
pub(super) struct CachedAlphaGlyph {
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) left: i32,
    pub(super) top: i32,
    pub(super) mask: Arc<[u8]>,
}

pub(super) struct PositionedMaskGlyph {
    pub(super) x: i32,
    pub(super) y: i32,
    pub(super) glyph: CachedAlphaGlyph,
    pub(super) rgba: [u8; 4],
}

/// Bounded stamp-based LRU cache with O(N) batch eviction of the oldest ~12.5%
/// entries (amortized O(1) per insert; eliminates cliff `.clear()` stalls).
struct BoundedLruCache<K, V> {
    entries: HashMap<K, (u64, V)>,
    clock: u64,
    max_entries: usize,
}

impl<K: Eq + Hash, V> BoundedLruCache<K, V> {
    fn new(max_entries: usize) -> Self {
        Self {
            entries: HashMap::new(),
            clock: 0,
            max_entries: max_entries.max(1),
        }
    }

    #[inline]
    fn next_stamp(&mut self) -> u64 {
        self.clock = self.clock.wrapping_add(1);
        self.clock
    }

    fn get(&mut self, key: &K) -> Option<&V> {
        let stamp = self.next_stamp();
        let entry = self.entries.get_mut(key)?;
        entry.0 = stamp;
        Some(&entry.1)
    }

    fn insert(&mut self, key: K, value: V) {
        let stamp = self.next_stamp();
        if let Some(entry) = self.entries.get_mut(&key) {
            entry.0 = stamp;
            entry.1 = value;
            return;
        }
        if self.entries.len() >= self.max_entries {
            self.evict_oldest_batch();
        }
        self.entries.insert(key, (stamp, value));
    }

    fn evict_oldest_batch(&mut self) {
        let drop_count = (self.max_entries / 8).max(1);
        if self.entries.len() <= drop_count {
            self.entries.clear();
            return;
        }
        let mut stamps: Vec<u64> = self.entries.values().map(|(s, _)| *s).collect();
        let pivot = drop_count - 1;
        let (_, threshold, _) = stamps.select_nth_unstable(pivot);
        let cutoff = *threshold;
        self.entries.retain(|_, (s, _)| *s > cutoff);
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.entries.len()
    }

    #[cfg(test)]
    fn contains_key(&self, key: &K) -> bool {
        self.entries.contains_key(key)
    }
}

/// Quantize a physical horizontal coordinate into `(integer_px, quarter_bin)` where
/// `quarter_bin` is in `0..4`, representing fractional offsets `0.0, 0.25, 0.5, 0.75` px.
#[inline]
pub(super) fn quantize_subpixel_x(phys_x: f32) -> (i32, u8) {
    let q = (phys_x * 4.0).round() as i32;
    (q.div_euclid(4), q.rem_euclid(4) as u8)
}

#[inline]
fn srgb_luma(r: u8, g: u8, b: u8) -> f32 {
    (0.2126 * f32::from(r) + 0.7152 * f32::from(g) + 0.0722 * f32::from(b)) / 255.0
}

#[inline]
pub(super) fn composite_rgb_over_opaque(fg: [u8; 4], bg: [u8; 4]) -> [u8; 3] {
    let a = u32::from(fg[3]);
    let inv = 255 - a;
    [
        ((u32::from(fg[0]) * a + u32::from(bg[0]) * inv + 127) / 255) as u8,
        ((u32::from(fg[1]) * a + u32::from(bg[1]) * inv + 127) / 255) as u8,
        ((u32::from(fg[2]) * a + u32::from(bg[2]) * inv + 127) / 255) as u8,
    ]
}

/// Precomputed 17-level coverage LUTs (levels 0..=16) for light-on-dark gamma and stem
/// contrast compensation. Level 0 is identity (dark-on-light); level 16 applies full
/// light-on-dark compensation so thin 11-14px strokes stay crisp without color fringes
/// on transparent glossary surfaces.
fn light_on_dark_coverage_tables() -> &'static [[u8; 256]; 17] {
    static TABLES: OnceLock<[[u8; 256]; 17]> = OnceLock::new();
    TABLES.get_or_init(|| {
        let mut tables = [[0u8; 256]; 17];
        for (level, table) in tables.iter_mut().enumerate() {
            let polarity = level as f32 / 16.0;
            let inv_gamma = 1.0 / (1.0 + 0.48 * polarity);
            let contrast = 0.30 * polarity;
            for (cov, slot) in table.iter_mut().enumerate() {
                if cov == 0 || cov == 255 || level == 0 {
                    *slot = cov as u8;
                } else {
                    let c = cov as f32 / 255.0;
                    let g = c.powf(inv_gamma);
                    let adj = (g + contrast * g * (1.0 - g) * (2.0 * g - 1.0)).clamp(0.0, 1.0);
                    *slot = (adj * 255.0).round() as u8;
                }
            }
        }
        tables
    })
}

#[inline]
fn light_on_dark_lut_level(fg_lum: f32, bg_lum: f32) -> usize {
    if fg_lum <= bg_lum {
        return 0;
    }
    let polarity = ((fg_lum - bg_lum) / 0.85).clamp(0.0, 1.0);
    (polarity * 16.0).round() as usize
}

pub(super) fn blit_alpha_glyph(
    pixmap: &mut tiny_skia::Pixmap,
    x0: i32,
    y0: i32,
    glyph: &CachedAlphaGlyph,
    rgba: [u8; 4],
    backdrop_rgb: [u8; 3],
) {
    let pw = pixmap.width() as i32;
    let ph = pixmap.height() as i32;
    let gw = glyph.width as i32;
    let gh = glyph.height as i32;
    let [sr, sg, sb, sa] = rgba;
    let sa_u = u32::from(sa);
    if sa_u == 0 || gw <= 0 || gh <= 0 {
        return;
    }
    let sy_start = (-y0).max(0);
    let sy_end = (ph - y0).min(gh);
    let sx_start = (-x0).max(0);
    let sx_end = (pw - x0).min(gw);
    if sy_start >= sy_end || sx_start >= sx_end {
        return;
    }
    let tables = light_on_dark_coverage_tables();
    let fg_lum = srgb_luma(sr, sg, sb);
    let base_bg_lum = srgb_luma(backdrop_rgb[0], backdrop_rgb[1], backdrop_rgb[2]);
    let base_lut = &tables[light_on_dark_lut_level(fg_lum, base_bg_lum)];

    let pixels = pixmap.pixels_mut();
    let pw_u = pw as usize;
    let gw_u = gw as usize;
    for sy in sy_start..sy_end {
        let row_off = (y0 + sy) as usize * pw_u;
        let mask_row = (sy as usize) * gw_u;
        for sx in sx_start..sx_end {
            let raw_cov = glyph.mask[mask_row + sx as usize];
            if raw_cov == 0 {
                continue;
            }
            let idx = row_off + (x0 + sx) as usize;
            let dst = pixels[idx];
            let cov = if dst.alpha() == 0 {
                u32::from(base_lut[usize::from(raw_cov)])
            } else {
                let da = u32::from(dst.alpha());
                let inv_da = 255 - da;
                let eff_r = (u32::from(dst.red())
                    + (u32::from(backdrop_rgb[0]) * inv_da + 127) / 255)
                    .min(255) as u8;
                let eff_g = (u32::from(dst.green())
                    + (u32::from(backdrop_rgb[1]) * inv_da + 127) / 255)
                    .min(255) as u8;
                let eff_b = (u32::from(dst.blue())
                    + (u32::from(backdrop_rgb[2]) * inv_da + 127) / 255)
                    .min(255) as u8;
                let eff_lum = srgb_luma(eff_r, eff_g, eff_b);
                u32::from(tables[light_on_dark_lut_level(fg_lum, eff_lum)][usize::from(raw_cov)])
            };
            if cov == 0 {
                continue;
            }
            let alpha = (cov * sa_u + 127) / 255;
            if alpha == 0 {
                continue;
            }
            let inv_a = 255 - alpha;
            let out_a = (alpha + (u32::from(dst.alpha()) * inv_a + 127) / 255).min(255) as u8;
            let out_r = ((u32::from(sr) * alpha + u32::from(dst.red()) * inv_a + 127) / 255)
                .min(u32::from(out_a)) as u8;
            let out_g = ((u32::from(sg) * alpha + u32::from(dst.green()) * inv_a + 127) / 255)
                .min(u32::from(out_a)) as u8;
            let out_b = ((u32::from(sb) * alpha + u32::from(dst.blue()) * inv_a + 127) / 255)
                .min(u32::from(out_a)) as u8;
            if let Some(pm) = tiny_skia::PremultipliedColorU8::from_rgba(out_r, out_g, out_b, out_a)
            {
                pixels[idx] = pm;
            }
        }
    }
}

struct SubtitleLayerGeometry {
    path: Option<tiny_skia::Path>,
    fallback_masks: Vec<PositionedMaskGlyph>,
    color_images: Vec<(f32, f32, Arc<tiny_skia::Pixmap>)>,
    #[cfg(test)]
    monochrome_cluster_texts: Vec<String>,
}

fn append_swash_outline_to_path(
    outline: &swash::scale::outline::Outline,
    origin_x: f32,
    baseline_y: f32,
    pb: &mut tiny_skia::PathBuilder,
) {
    use swash::zeno::{Command, PathData};
    for cmd in outline.path().commands() {
        match cmd {
            Command::MoveTo(p) => pb.move_to(origin_x + p.x, baseline_y - p.y),
            Command::LineTo(p) => pb.line_to(origin_x + p.x, baseline_y - p.y),
            Command::QuadTo(c, p) => pb.quad_to(
                origin_x + c.x,
                baseline_y - c.y,
                origin_x + p.x,
                baseline_y - p.y,
            ),
            Command::CurveTo(c1, c2, p) => pb.cubic_to(
                origin_x + c1.x,
                baseline_y - c1.y,
                origin_x + c2.x,
                baseline_y - c2.y,
                origin_x + p.x,
                baseline_y - p.y,
            ),
            Command::Close => pb.close(),
        }
    }
}

/// Initialize Parley's `FontContext` sharing Slint's process-wide `fontique::Collection`
/// when the Slint platform is active (avoiding a duplicate system font DB scan in RAM),
/// falling back cleanly to `FontContext::new()` in headless unit tests without a platform.
fn shared_parley_font_context() -> FontContext {
    match std::panic::catch_unwind(slint::fontique_08::shared_collection) {
        Ok(collection) => FontContext {
            collection,
            source_cache: slint::fontique_08::fontique::SourceCache::default(),
        },
        Err(_) => FontContext::new(),
    }
}

pub(super) struct TextEngine {
    pub(super) font_cx: FontContext,
    pub(super) layout_cx: LayoutContext<[u8; 4]>,
    scale_cx: swash::scale::ScaleContext,
    pub(super) fontdb: Arc<usvg::fontdb::Database>,
    registered_fonts: HashMap<(u64, u32), RegisteredFontInfo>,
    font_is_color: HashMap<(u64, u32), bool>,
    emoji_cache: BoundedLruCache<(u64, u32, u16, i32), Option<RenderedColorGlyph>>,
    alpha_glyph_cache: BoundedLruCache<(u64, u32, u16, i32, i8, u8), Option<CachedAlphaGlyph>>,
    default_svg_fonts_primed: bool,
    subtitle_cache: BoundedLruCache<SubtitleCacheKey, ShapedSubtitleToken>,
    devoice_circle: Option<slint::Image>,
}

thread_local! {
    static SHARED_TEXT_ENGINE: RefCell<TextEngine> = RefCell::new(TextEngine::new());
}

pub(super) fn with_shared_text_engine<R>(f: impl FnOnce(&mut TextEngine) -> R) -> R {
    SHARED_TEXT_ENGINE.with(|cell| f(&mut cell.borrow_mut()))
}

impl TextEngine {
    pub(super) fn new() -> Self {
        Self {
            font_cx: shared_parley_font_context(),
            layout_cx: LayoutContext::new(),
            scale_cx: swash::scale::ScaleContext::new(),
            fontdb: Arc::new(usvg::fontdb::Database::new()),
            registered_fonts: HashMap::new(),
            font_is_color: HashMap::new(),
            emoji_cache: BoundedLruCache::new(EMOJI_CACHE_MAX_ENTRIES),
            alpha_glyph_cache: BoundedLruCache::new(ALPHA_GLYPH_CACHE_MAX_ENTRIES),
            default_svg_fonts_primed: false,
            subtitle_cache: BoundedLruCache::new(SUBTITLE_CACHE_MAX_ENTRIES),
            devoice_circle: None,
        }
    }

    /// Ensure `self.fontdb` has a real CJK-capable sans-serif font registered and mapped
    /// to SVG generic families (`sans-serif`, `serif`, `monospace`). Called lazily only
    /// when rendering dictionary SVG assets (such as `smk8/運用-fill.svg` which uses
    /// `<text font-family="sans-serif">` inside `<mask>`).
    pub(super) fn ensure_default_svg_fonts(&mut self) {
        if self.default_svg_fonts_primed {
            return;
        }
        self.default_svg_fonts_primed = true;
        let sample = self.shape_single_line("運用あA", "", 16.0, 400, false, [255, 255, 255, 255]);
        let mut primary_family: Option<String> = None;
        for line in sample.lines() {
            for run in line.runs() {
                let info = self.ensure_font_registered(run.font().clone().borrow_font());
                if primary_family.is_none() {
                    primary_family = Some(info.family);
                }
            }
        }
        if let Some(family) = primary_family {
            let db = Arc::make_mut(&mut self.fontdb);
            db.set_sans_serif_family(&family);
            db.set_serif_family(&family);
            db.set_monospace_family(&family);
        }
    }

    pub(super) fn font_supports_color(&mut self, font: &parley::FontData) -> bool {
        let key = (font.data.id(), font.index);
        if let Some(&is_color) = self.font_is_color.get(&key) {
            return is_color;
        }
        let is_color = swash::FontRef::from_index(font.data.data(), font.index as usize)
            .is_some_and(|font_ref| {
                let scaler = self.scale_cx.builder(font_ref).size(16.0).build();
                scaler.has_color_bitmaps() || scaler.has_color_outlines()
            });
        self.font_is_color.insert(key, is_color);
        is_color
    }

    pub(super) fn try_render_color_glyph(
        &mut self,
        font: &parley::FontData,
        glyph_id: u16,
        font_size: f32,
    ) -> Option<RenderedColorGlyph> {
        if !self.font_supports_color(font) {
            return None;
        }
        let size_tenth = (font_size * 10.0).round() as i32;
        let key = (font.data.id(), font.index, glyph_id, size_tenth);
        if let Some(cached) = self.emoji_cache.get(&key) {
            return cached.clone();
        }
        let rendered = (|| -> Option<RenderedColorGlyph> {
            let font_ref = swash::FontRef::from_index(font.data.data(), font.index as usize)?;
            let mut scaler = self
                .scale_cx
                .builder(font_ref)
                .size(font_size.max(6.0))
                .hint(false)
                .build();
            let image = swash::scale::Render::new(&[
                swash::scale::Source::ColorOutline(0),
                swash::scale::Source::ColorBitmap(swash::scale::StrikeWith::BestFit),
            ])
            .render(&mut scaler, glyph_id)?;
            if image.content != swash::scale::image::Content::Color
                || image.placement.width == 0
                || image.placement.height == 0
            {
                return None;
            }
            let w = image.placement.width;
            let h = image.placement.height;
            let mut pixmap = tiny_skia::Pixmap::new(w, h)?;
            for (dst, src) in pixmap.pixels_mut().iter_mut().zip(image.data.chunks_exact(4)) {
                let [r, g, b, a] = [src[0], src[1], src[2], src[3]];
                let pr = (u32::from(r) * u32::from(a) + 127) / 255;
                let pg = (u32::from(g) * u32::from(a) + 127) / 255;
                let pb = (u32::from(b) * u32::from(a) + 127) / 255;
                if let Some(pm) =
                    tiny_skia::PremultipliedColorU8::from_rgba(pr as u8, pg as u8, pb as u8, a)
                {
                    *dst = pm;
                }
            }
            Some(RenderedColorGlyph {
                pixmap: Arc::new(pixmap),
                left: image.placement.left as f32,
                top: image.placement.top as f32,
            })
        })();
        self.emoji_cache.insert(key, rendered.clone());
        rendered
    }

    pub(super) fn try_render_alpha_glyph(
        &mut self,
        font: &parley::FontData,
        glyph_id: u16,
        font_size: f32,
        synthetic_italic: bool,
        subpixel_quarter: u8,
    ) -> Option<CachedAlphaGlyph> {
        let sub_q = subpixel_quarter & 3;
        let size_tenth = (font_size * 10.0).round() as i32;
        let skew_key: i8 = if synthetic_italic { 1 } else { 0 };
        let key = (font.data.id(), font.index, glyph_id, size_tenth, skew_key, sub_q);
        if let Some(cached) = self.alpha_glyph_cache.get(&key) {
            return cached.clone();
        }
        let rendered = (|| -> Option<CachedAlphaGlyph> {
            let font_ref = swash::FontRef::from_index(font.data.data(), font.index as usize)?;
            let mut scaler = self
                .scale_cx
                .builder(font_ref)
                .size(font_size.max(6.0))
                .hint(true)
                .build();
            let mut render = swash::scale::Render::new(&[
                swash::scale::Source::Outline,
                swash::scale::Source::Bitmap(swash::scale::StrikeWith::BestFit),
            ]);
            if sub_q != 0 {
                render.offset(swash::zeno::Vector::new(f32::from(sub_q) * 0.25, 0.0));
            }
            if synthetic_italic {
                render.transform(Some(swash::zeno::Transform::skew(
                    swash::zeno::Angle::from_degrees(-14.0),
                    swash::zeno::Angle::ZERO,
                )));
            }
            let image = render.render(&mut scaler, glyph_id)?;
            if image.content != swash::scale::image::Content::Mask
                || image.placement.width == 0
                || image.placement.height == 0
            {
                return None;
            }
            Some(CachedAlphaGlyph {
                width: image.placement.width,
                height: image.placement.height,
                left: image.placement.left,
                top: image.placement.top,
                mask: Arc::from(image.data),
            })
        })();
        self.alpha_glyph_cache.insert(key, rendered.clone());
        rendered
    }

    pub(super) fn collect_glossary_glyphs_from_layout(
        &mut self,
        layout: &Layout<[u8; 4]>,
        origin_x: f32,
        origin_y: f32,
        underline: bool,
        strikethrough: bool,
        collector: &mut PaintCollector,
    ) {
        let scale = collector.render_scale;
        for line in layout.lines() {
            let baseline = origin_y + line.metrics().baseline;
            for item in line.items() {
                if let PositionedLayoutItem::GlyphRun(glyph_run) = item {
                    let run = glyph_run.run();
                    let run_font = run.font().clone();
                    let font_size = run.font_size();
                    let scaled_font_size = font_size * scale;
                    let brush = glyph_run.style().brush;
                    let is_color_font = self.font_supports_color(run_font.borrow_font());
                    let synth_italic = run.synthesis().skew().is_some();
                    let run_x = origin_x + glyph_run.offset();
                    let mut gx = run_x;
                    for g in glyph_run.positioned_glyphs() {
                        let px = origin_x + g.x;
                        let py = origin_y + g.y;
                        let phys_x = px * scale;
                        let phys_y = py * scale;
                        let rendered_color = is_color_font
                            && self
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
                            if let Some(ag) = self.try_render_alpha_glyph(
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
                        gx = gx.max(px + g.advance);
                    }
                    let adv = (gx - run_x).max(0.0);
                    if underline && adv > 0.0 {
                        collector
                            .rects
                            .push((run_x, baseline + 2.0, adv, 1.0, brush));
                    }
                    if strikethrough && adv > 0.0 {
                        collector
                            .rects
                            .push((run_x, baseline - font_size * 0.32, adv, 1.0, brush));
                    }
                }
            }
        }
    }

    fn ensure_font_registered(&mut self, font: &parley::FontData) -> RegisteredFontInfo {
        let key = (font.data.id(), font.index);
        if let Some(info) = self.registered_fonts.get(&key) {
            return info.clone();
        }
        let db = Arc::make_mut(&mut self.fontdb);
        let ids = db.load_font_source(usvg::fontdb::Source::Binary(Arc::new(font.data.clone())));
        let mut chosen: Option<RegisteredFontInfo> = None;
        for id in ids {
            if let Some(face) = db.face(id) {
                let family = face
                    .families
                    .first()
                    .map(|(name, _)| name.clone())
                    .unwrap_or_else(|| "sans-serif".to_owned());
                let info = RegisteredFontInfo {
                    family,
                    weight: face.weight.0,
                    italic: face.style != usvg::fontdb::Style::Normal,
                };
                self.registered_fonts
                    .insert((font.data.id(), face.index), info.clone());
                if face.index == font.index {
                    chosen = Some(info);
                } else if chosen.is_none() {
                    chosen = Some(info);
                }
            }
        }
        let fallback = chosen.unwrap_or_else(|| RegisteredFontInfo {
            family: "Noto Sans CJK JP".to_owned(),
            weight: 400,
            italic: false,
        });
        self.registered_fonts
            .entry(key)
            .or_insert_with(|| fallback.clone());
        fallback
    }

    pub(super) fn shape_single_line(
        &mut self,
        text: &str,
        font_family: &str,
        font_px: f32,
        weight: u16,
        italic: bool,
        color: [u8; 4],
    ) -> Layout<[u8; 4]> {
        let family_stack = if font_family.trim().is_empty() {
            Cow::Borrowed(RICH_EMOJI_FALLBACK_STACK)
        } else {
            Cow::Owned(format!("{font_family}, {RICH_EMOJI_FALLBACK_STACK}"))
        };
        let mut builder = self
            .layout_cx
            .ranged_builder(&mut self.font_cx, text, 1.0, true);
        builder.push_default(StyleProperty::FontFamily(FontFamily::Source(family_stack)));
        builder.push_default(StyleProperty::FontSize(font_px.max(6.0)));
        builder.push_default(StyleProperty::FontWeight(FontWeight::new(weight as f32)));
        builder.push_default(StyleProperty::FontStyle(if italic {
            FontStyle::Italic
        } else {
            FontStyle::Normal
        }));
        builder.push_default(StyleProperty::LineHeight(LineHeight::MetricsRelative(1.0)));
        builder.push_default(StyleProperty::Brush(color));
        let mut layout = builder.build(text);
        layout.break_all_lines(None);
        layout.align(None, Alignment::Start, AlignmentOptions::default());
        layout
    }

    fn make_subtitle_cache_key(
        word: &SubtitleWord,
        font_px: f32,
        show_furigana: bool,
        style: &SubtitleRenderStyle,
    ) -> SubtitleCacheKey {
        let font_px = font_px.clamp(8.0, 140.0);
        let line_spacing_extra = style.line_spacing.clamp(0.0, 120.0);
        SubtitleCacheKey {
            text: word.text.to_string(),
            reading: if show_furigana {
                word.reading.to_string()
            } else {
                String::new()
            },
            font_tenth_px: (font_px * 10.0).round() as i32,
            show_furigana,
            font_family: style.font_family.clone(),
            text_rgba: style.text_rgba,
            stroke_rgba: style.stroke_rgba,
            stroke_tenth: (style.stroke_size * 10.0).round() as i32,
            furigana_pct: (style.furigana_ratio * 100.0).round() as i32,
            line_spacing_tenth: (line_spacing_extra * 10.0).round() as i32,
            bold: style.bold,
            italic: style.italic,
        }
    }

    /// Measure the logical layout width of a subtitle token using Parley layout metrics
    /// alone (zero pixmap allocation / zero vector rasterization). Matches the exact
    /// `width_px` produced by `shape_and_render_subtitle_token`.
    pub(super) fn measure_subtitle_token_width(
        &mut self,
        word: &SubtitleWord,
        font_px: f32,
        show_furigana: bool,
        style: &SubtitleRenderStyle,
    ) -> Option<f32> {
        if word.text.is_empty() {
            return Some(0.0);
        }
        let key = Self::make_subtitle_cache_key(word, font_px, show_furigana, style);
        if let Some(cached) = self.subtitle_cache.get(&key) {
            return (cached.width_px > 0.0).then_some(cached.width_px);
        }

        let font_px = font_px.clamp(8.0, 140.0);
        let weight = if style.bold { 700 } else { 400 };
        let base_layout = self.shape_single_line(
            word.text.as_str(),
            &style.font_family,
            font_px,
            weight,
            style.italic,
            style.text_rgba,
        );
        let base_width = base_layout
            .lines()
            .next()
            .map(|l| l.metrics().advance)
            .unwrap_or(0.0)
            .max(1.0);

        let furigana_ratio = style.furigana_ratio.clamp(0.25, 0.75);
        let ruby_font_px = (font_px * furigana_ratio).max(8.0);
        let ruby_width = if show_furigana && !word.reading.is_empty() {
            let layout = self.shape_single_line(
                word.reading.as_str(),
                &style.font_family,
                ruby_font_px,
                600,
                style.italic,
                style.text_rgba,
            );
            let adv = layout
                .lines()
                .next()
                .map(|l| l.metrics().advance)
                .unwrap_or(0.0);
            adv
        } else {
            0.0
        };

        let layout_width = base_width.max(ruby_width).ceil().max(1.0);
        (layout_width > 0.0).then_some(layout_width)
    }

    /// Extract vector outlines via `swash` + `tiny_skia::PathBuilder` and color emoji
    /// bitmaps directly from a shaped Parley layout, completely bypassing XML `<svg>`
    /// generation and `usvg::fontdb`.
    fn collect_subtitle_layer_geometry(
        &mut self,
        layout: &Layout<[u8; 4]>,
        #[allow(unused_variables)] source_text: &str,
        origin_x: f32,
        origin_y: f32,
    ) -> SubtitleLayerGeometry {
        let mut path_builder = tiny_skia::PathBuilder::new();
        let mut fallback_masks = Vec::new();
        let mut color_images = Vec::new();
        let mut outline = swash::scale::outline::Outline::new();
        #[cfg(test)]
        let mut monochrome_cluster_texts = Vec::new();

        for line in layout.lines() {
            let baseline = origin_y + line.metrics().baseline;
            let mut prev_run_key: Option<(usize, usize, u64, u32)> = None;
            let mut run_glyph_cursor: usize = 0;

            for item in line.items() {
                match item {
                    PositionedLayoutItem::InlineBox(_) => {
                        prev_run_key = None;
                        run_glyph_cursor = 0;
                    }
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
                        let synth = run.synthesis();
                        let synth_embolden = synth.embolden();
                        let synth_skew = synth.skew();
                        let coords: Vec<i16> = run.normalized_coords().to_vec();
                        let is_color_font = self.font_supports_color(run_font.borrow_font());

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

                            let advance = cluster.advance();
                            if c_glyph_count == 0 {
                                // Zero-glyph continuation (e.g. `\u{FE0F}`, `\u{20E3}` in `1️⃣`/`2️⃣`,
                                // regional indicator second half, ZWJ sequences, or ligature tails):
                                // never emit stray monochrome strokes/fills on top of color emoji.
                                x_cursor += advance;
                                continue;
                            }

                            let rendered_as_color = if is_color_font {
                                let mut gx = x_cursor;
                                let mut any_color = false;
                                for g in cluster.glyphs() {
                                    if let Some(cg) = self.try_render_color_glyph(
                                        run_font.borrow_font(),
                                        g.id as u16,
                                        font_size,
                                    ) {
                                        color_images.push((
                                            gx + g.x + cg.left,
                                            baseline + g.y - cg.top,
                                            cg.pixmap,
                                        ));
                                        any_color = true;
                                    }
                                    gx += g.advance;
                                }
                                any_color
                            } else {
                                false
                            };

                            if !rendered_as_color {
                                #[cfg(test)]
                                {
                                    let range = cluster.text_range();
                                    if let Some(slice) = source_text.get(range) {
                                        if !slice.is_empty()
                                            && !slice.chars().all(|c| c == '\n' || c == '\r')
                                        {
                                            monochrome_cluster_texts.push(slice.to_owned());
                                        }
                                    }
                                }

                                let mut gx = x_cursor;
                                for g in cluster.glyphs() {
                                    let gx_pos = gx + g.x;
                                    let gy_pos = baseline + g.y;
                                    let mut outline_ok = false;
                                    if let Some(font_ref) = swash::FontRef::from_index(
                                        run_font.data.data(),
                                        run_font.index as usize,
                                    ) {
                                        let mut builder = self
                                            .scale_cx
                                            .builder(font_ref)
                                            .size(font_size.max(6.0))
                                            .hint(true);
                                        if !coords.is_empty() {
                                            builder = builder.normalized_coords(&coords);
                                        }
                                        let mut scaler = builder.build();
                                        if scaler.has_outlines()
                                            && scaler.scale_outline_into(g.id as u16, &mut outline)
                                        {
                                            if synth_embolden {
                                                let strength = (font_size * 0.02).clamp(0.2, 1.4);
                                                outline.embolden(strength, strength);
                                            }
                                            if let Some(skew_deg) = synth_skew {
                                                outline.transform(&swash::zeno::Transform::skew(
                                                    swash::zeno::Angle::from_degrees(skew_deg),
                                                    swash::zeno::Angle::ZERO,
                                                ));
                                            }
                                            append_swash_outline_to_path(
                                                &outline,
                                                gx_pos,
                                                gy_pos,
                                                &mut path_builder,
                                            );
                                            outline_ok = true;
                                        }
                                    }
                                    if !outline_ok {
                                        let (glyph_x, sub_q) = quantize_subpixel_x(gx_pos);
                                        if let Some(ag) = self.try_render_alpha_glyph(
                                            run_font.borrow_font(),
                                            g.id as u16,
                                            font_size,
                                            synth_skew.is_some(),
                                            sub_q,
                                        ) {
                                            fallback_masks.push(PositionedMaskGlyph {
                                                x: glyph_x + ag.left,
                                                y: gy_pos.round() as i32 - ag.top,
                                                glyph: ag,
                                                rgba: brush,
                                            });
                                        }
                                    }
                                    gx += g.advance;
                                }
                            }
                            x_cursor += advance;
                        }
                    }
                }
            }
        }

        SubtitleLayerGeometry {
            path: path_builder.finish(),
            fallback_masks,
            color_images,
            #[cfg(test)]
            monochrome_cluster_texts,
        }
    }

    pub(super) fn shape_and_render_subtitle_token(
        &mut self,
        word: &SubtitleWord,
        font_px: f32,
        show_furigana: bool,
        style: &SubtitleRenderStyle,
    ) -> ShapedSubtitleToken {
        let font_px = font_px.clamp(8.0, 140.0);
        let line_spacing_extra = style.line_spacing.clamp(0.0, 120.0);
        let key = Self::make_subtitle_cache_key(word, font_px, show_furigana, style);
        if let Some(cached) = self.subtitle_cache.get(&key) {
            return cached.clone();
        }

        let weight = if style.bold { 700 } else { 400 };
        let base_layout = self.shape_single_line(
            word.text.as_str(),
            &style.font_family,
            font_px,
            weight,
            style.italic,
            style.text_rgba,
        );

        let base_width = base_layout
            .lines()
            .next()
            .map(|l| l.metrics().advance)
            .unwrap_or(0.0)
            .max(1.0);
        let base_height = base_layout
            .lines()
            .next()
            .map(|l| l.metrics().line_height)
            .unwrap_or(font_px * 1.25)
            .max(font_px);

        let stroke_px = if style.stroke_size <= 0.0 || style.stroke_rgba[3] == 0 {
            0.0
        } else {
            (style.stroke_size * (font_px / 48.0)).clamp(1.2, font_px * 0.28)
        };
        // Keep token layout advance equal to the true typographic width so adjacent tokens
        // never develop artificial gaps, while letting strokes/shadows bleed visually into
        // `bleed_x` on each side of the rendered image.
        let bleed_x = if stroke_px > 0.0 {
            (stroke_px * 0.75 + 1.5).ceil()
        } else {
            1.0
        };
        let pad_y = (stroke_px * 0.55).ceil().max(1.0);

        let furigana_ratio = style.furigana_ratio.clamp(0.25, 0.75);
        let ruby_font_px = (font_px * furigana_ratio).max(8.0);
        let ruby_space = if show_furigana {
            (font_px * furigana_ratio.max(0.45) + 3.0).ceil()
        } else {
            0.0
        };

        let (ruby_layout, ruby_width, ruby_height) = if show_furigana && !word.reading.is_empty() {
            let layout = self.shape_single_line(
                word.reading.as_str(),
                &style.font_family,
                ruby_font_px,
                600,
                style.italic,
                style.text_rgba,
            );
            let rw = layout
                .lines()
                .next()
                .map(|l| l.metrics().advance)
                .unwrap_or(0.0);
            let rh = layout
                .lines()
                .next()
                .map(|l| l.metrics().line_height)
                .unwrap_or(ruby_font_px * 1.2);
            (Some(layout), rw, rh)
        } else {
            (None, 0.0, 0.0)
        };

        let layout_width = base_width.max(ruby_width).ceil().max(1.0);
        let half_line_spacing = line_spacing_extra * 0.5;
        let total_height = (ruby_space + base_height + pad_y * 2.0 + line_spacing_extra)
            .ceil()
            .max(2.0);
        let base_origin_x_token = (layout_width - base_width) * 0.5;
        let base_origin_x_pix = bleed_x + base_origin_x_token;
        let base_origin_y = ruby_space + pad_y + half_line_spacing;

        let mut clusters = Vec::new();
        let mut cluster_hitboxes = Vec::new();
        if let Some(line) = base_layout.lines().next() {
            let mut x_pos = line.metrics().offset;
            for run in line.runs() {
                for cluster in run.visual_clusters() {
                    let r = cluster.text_range();
                    let adv = cluster.advance();
                    clusters.push((r.start, r.end, x_pos, adv));
                    let byte_offset = r.start.min(word.text.len());
                    let char_offset = word
                        .text
                        .get(..byte_offset)
                        .map_or(0, |prefix| prefix.chars().count());
                    cluster_hitboxes.push(SubtitleClusterHitbox {
                        x0: x_pos,
                        x1: x_pos + adv,
                        byte_offset,
                        char_offset,
                    });
                    x_pos += adv;
                }
            }
        }

        let base_geom = self.collect_subtitle_layer_geometry(
            &base_layout,
            word.text.as_str(),
            base_origin_x_pix,
            base_origin_y,
        );
        let ruby_geom = ruby_layout.as_ref().map(|r_layout| {
            let r_origin_x_pix = bleed_x + (layout_width - ruby_width) * 0.5;
            let r_origin_y = (ruby_space - ruby_height).max(0.0) + pad_y * 0.5 + half_line_spacing;
            self.collect_subtitle_layer_geometry(
                r_layout,
                word.reading.as_str(),
                r_origin_x_pix,
                r_origin_y,
            )
        });

        let pix_w = ((layout_width + bleed_x * 2.0).ceil() as u32).max(1);
        let pix_h = (total_height.ceil() as u32).max(1);
        let mut pixmap = tiny_skia::Pixmap::new(pix_w, pix_h)
            .unwrap_or_else(|| tiny_skia::Pixmap::new(1, 1).unwrap());

        let ruby_stroke_px = (stroke_px * 0.65).max(if stroke_px > 0.0 { 1.0 } else { 0.0 });
        let mut fill_paint = tiny_skia::Paint::default();
        fill_paint.set_color_rgba8(
            style.text_rgba[0],
            style.text_rgba[1],
            style.text_rgba[2],
            style.text_rgba[3],
        );
        fill_paint.anti_alias = true;

        let mut stroke_paint = tiny_skia::Paint::default();
        stroke_paint.set_color_rgba8(
            style.stroke_rgba[0],
            style.stroke_rgba[1],
            style.stroke_rgba[2],
            style.stroke_rgba[3],
        );
        stroke_paint.anti_alias = true;

        let mut shadow_paint = tiny_skia::Paint::default();
        shadow_paint.set_color_rgba8(0, 0, 0, (style.stroke_rgba[3] / 2).max(80));
        shadow_paint.anti_alias = true;

        let paint_layer = |pixmap: &mut tiny_skia::Pixmap,
                           geom: &SubtitleLayerGeometry,
                           s_width: f32| {
            if let Some(ref path) = geom.path {
                if s_width > 0.0 && style.stroke_rgba[3] > 0 {
                    let stroke = tiny_skia::Stroke {
                        width: s_width,
                        line_cap: tiny_skia::LineCap::Round,
                        line_join: tiny_skia::LineJoin::Round,
                        ..tiny_skia::Stroke::default()
                    };
                    // Crisp subtle drop-shadow pass for StrokeLabel parity
                    pixmap.stroke_path(
                        path,
                        &shadow_paint,
                        &stroke,
                        tiny_skia::Transform::from_translate(1.0, 1.2),
                        None,
                    );
                    // Outer stroke underneath fill (equivalent to SVG `paint-order="stroke"`)
                    pixmap.stroke_path(
                        path,
                        &stroke_paint,
                        &stroke,
                        tiny_skia::Transform::default(),
                        None,
                    );
                }
                pixmap.fill_path(
                    path,
                    &fill_paint,
                    tiny_skia::FillRule::Winding,
                    tiny_skia::Transform::default(),
                    None,
                );
            }
            for mg in &geom.fallback_masks {
                blit_alpha_glyph(pixmap, mg.x, mg.y, &mg.glyph, mg.rgba, [0, 0, 0]);
            }
        };

        if let Some(ref rg) = ruby_geom {
            paint_layer(&mut pixmap, rg, ruby_stroke_px);
        }
        paint_layer(&mut pixmap, &base_geom, stroke_px);

        let has_color = ruby_geom
            .as_ref()
            .is_some_and(|g| !g.color_images.is_empty())
            || !base_geom.color_images.is_empty();
        if has_color {
            let img_paint = tiny_skia::PixmapPaint {
                opacity: 1.0,
                blend_mode: tiny_skia::BlendMode::SourceOver,
                quality: tiny_skia::FilterQuality::Bilinear,
            };
            let ruby_imgs = ruby_geom.map_or_else(Vec::new, |g| g.color_images);
            for (ix, iy, img_pm) in ruby_imgs.into_iter().chain(base_geom.color_images) {
                pixmap.draw_pixmap(
                    ix.round() as i32,
                    iy.round() as i32,
                    (*img_pm).as_ref(),
                    &img_paint,
                    tiny_skia::Transform::default(),
                    None,
                );
            }
        }

        let image = pixmap_to_slint_image(&pixmap);
        let shaped = ShapedSubtitleToken {
            width_px: layout_width,
            height_px: total_height,
            bleed_x_px: bleed_x,
            glyph_x_px: base_origin_x_token,
            glyph_y_px: base_origin_y,
            glyph_width_px: base_width,
            glyph_height_px: base_height,
            clusters,
            cluster_hitboxes,
            image,
        };
        self.subtitle_cache.insert(key, shaped.clone());
        shaped
    }

    pub(super) fn devoice_circle_image(&mut self) -> slint::Image {
        if let Some(ref img) = self.devoice_circle {
            return img.clone();
        }
        let size = 36u32;
        let mut pixmap = tiny_skia::Pixmap::new(size, size).unwrap();
        let mut pb = tiny_skia::PathBuilder::new();
        pb.push_circle(18.0, 18.0, 15.0);
        if let Some(path) = pb.finish() {
            let mut paint = tiny_skia::Paint::default();
            paint.set_color_rgba8(248, 113, 113, 235);
            paint.anti_alias = true;
            let mut stroke = tiny_skia::Stroke {
                width: 2.2,
                line_cap: tiny_skia::LineCap::Round,
                line_join: tiny_skia::LineJoin::Round,
                ..tiny_skia::Stroke::default()
            };
            stroke.dash = tiny_skia::StrokeDash::new(vec![3.2, 3.2], 0.0);
            pixmap.stroke_path(
                &path,
                &paint,
                &stroke,
                tiny_skia::Transform::default(),
                None,
            );
        }
        let img = pixmap_to_slint_image(&pixmap);
        self.devoice_circle = Some(img.clone());
        img
    }
}

pub(super) trait BorrowFontExt {
    fn borrow_font(&self) -> &parley::FontData;
}

impl BorrowFontExt for parley::FontData {
    fn borrow_font(&self) -> &parley::FontData {
        self
    }
}

pub(super) fn pixmap_to_slint_image(pixmap: &tiny_skia::Pixmap) -> slint::Image {
    let buf = SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(
        pixmap.data(),
        pixmap.width(),
        pixmap.height(),
    );
    slint::Image::from_rgba8_premultiplied(buf)
}

/// Exact per-glyph cluster hit-testing on a subtitle token using cached
/// `ShapedSubtitleToken::cluster_hitboxes` (zero Parley re-shaping on mouse move).
pub fn subtitle_cluster_at_point_with_style(
    word: &SubtitleWord,
    font_px: f32,
    show_furigana: bool,
    style: &SubtitleRenderStyle,
    local_x: f32,
    _local_y: f32,
) -> Option<(usize, usize)> {
    if word.text.is_empty() {
        return None;
    }
    SHARED_TEXT_ENGINE.with(|engine| {
        let mut engine = engine.borrow_mut();
        let shaped = engine.shape_and_render_subtitle_token(word, font_px, show_furigana, style);
        if shaped.cluster_hitboxes.is_empty() {
            return Some((0, 0));
        }
        let text_x =
            (local_x - shaped.glyph_x_px).clamp(0.0, (shaped.glyph_width_px - 0.01).max(0.0));
        for hb in &shaped.cluster_hitboxes {
            if hb.x1 > hb.x0 && text_x >= hb.x0 && text_x < hb.x1 {
                return Some((hb.byte_offset, hb.char_offset));
            }
        }
        let mut best: Option<(f32, usize, usize)> = None;
        for hb in &shaped.cluster_hitboxes {
            if hb.x1 <= hb.x0 {
                continue;
            }
            let dist = if text_x < hb.x0 {
                hb.x0 - text_x
            } else if text_x > hb.x1 {
                text_x - hb.x1
            } else {
                0.0
            };
            if best.is_none_or(|(best_dist, _, _)| dist < best_dist) {
                best = Some((dist, hb.byte_offset, hb.char_offset));
            }
        }
        if let Some((_, byte_offset, char_offset)) = best {
            return Some((byte_offset, char_offset));
        }
        Some((0, 0))
    })
}

/// Exact per-glyph cluster hit-testing on a subtitle token using the active
/// `SubtitleRenderStyle`. Returns `(byte_offset_in_token, char_offset_in_token)`.
pub fn subtitle_cluster_at_point(
    word: &SubtitleWord,
    font_px: f32,
    show_furigana: bool,
    local_x: f32,
    local_y: f32,
) -> Option<(usize, usize)> {
    let style = active_subtitle_render_style();
    subtitle_cluster_at_point_with_style(word, font_px, show_furigana, &style, local_x, local_y)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keycap_emoji_continuations_do_not_emit_stray_overlays() {
        let mut engine = TextEngine::new();
        let txt = "1\u{FE0F}\u{20E3}［連体］《2\u{FE0F}\u{20E3}の一語化》";
        let layout = engine.shape_single_line(txt, "", 14.5, 400, false, [248, 250, 252, 255]);
        let geom = engine.collect_subtitle_layer_geometry(&layout, txt, 0.0, 0.0);
        assert_eq!(
            geom.color_images.len(),
            2,
            "1️⃣ and 2️⃣ must each render as exactly one color emoji bitmap"
        );
        for item in &geom.monochrome_cluster_texts {
            assert!(
                !item.contains('\u{20E3}') && !item.contains('\u{FE0F}'),
                "keycap continuation {:?} must never emit stray monochrome strokes/fills on top of color emoji",
                item
            );
        }
        let mono_text: String = geom
            .monochrome_cluster_texts
            .iter()
            .map(String::as_str)
            .collect();
        assert_eq!(mono_text, "［連体］《の一語化》");
        assert!(
            geom.path.is_some(),
            "non-emoji subtitle glyphs must extract direct vector outlines"
        );
    }

    #[test]
    fn bounded_lru_cache_evicts_oldest_without_cliff_clear() {
        let mut cache: BoundedLruCache<u32, u32> = BoundedLruCache::new(16);
        for i in 0..16 {
            cache.insert(i, i * 10);
        }
        // Touch entry 0 so it becomes hottest and must survive eviction.
        assert_eq!(cache.get(&0), Some(&0));
        // Inserting key 16 triggers batch eviction of the oldest ~12.5% (2 entries: 1 and 2).
        cache.insert(16, 160);
        assert!(cache.len() < 16 && cache.len() >= 14);
        assert!(cache.contains_key(&0), "recently accessed entry must survive");
        assert!(cache.contains_key(&16), "newly inserted entry must be present");
        assert!(!cache.contains_key(&1), "oldest untouched entry must be evicted");
    }
}

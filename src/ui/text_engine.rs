//! Shared Parley + Swash + Tiny-Skia text shaping, glyph caching, and
//! direct vector subtitle rasterization engine.

use std::borrow::{Borrow, Cow};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::hash::Hash;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::Duration;

use subomi_setting::keys as raw;
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

const SUBTITLE_CACHE_MAX_ENTRIES: usize = 256;
const SUBTITLE_WIDTH_CACHE_MAX_ENTRIES: usize = 512;
const EMOJI_CACHE_MAX_ENTRIES: usize = 512;
const ALPHA_GLYPH_CACHE_MAX_ENTRIES: usize = 8192;
const IDLE_SUBTITLE_CACHE_MAX_ENTRIES: usize = 64;
const IDLE_SUBTITLE_WIDTH_CACHE_MAX_ENTRIES: usize = 128;
const IDLE_EMOJI_CACHE_MAX_ENTRIES: usize = 64;
const IDLE_ALPHA_GLYPH_CACHE_MAX_ENTRIES: usize = 1024;
const SUBTITLE_PREWARM_MAX_TOKENS: usize = 96;
const SUBTITLE_PREWARM_IDLE_TIMEOUT: Duration = Duration::from_secs(45);

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
    pub has_stroke: bool,
    pub stroke_image: slint::Image,
    pub image: slint::Image,
}

#[derive(Hash, PartialEq, Eq, Clone, Copy)]
struct SubtitleStyleKey {
    font_tenth_px: i32,
    show_furigana: bool,
    text_rgba: [u8; 4],
    stroke_rgba: [u8; 4],
    stroke_tenth: i32,
    furigana_pct: i32,
    line_spacing_tenth: i32,
    bold: bool,
    italic: bool,
}

#[derive(Clone, Eq)]
struct SubtitleCacheKey {
    text: String,
    reading: String,
    font_family: String,
    style: SubtitleStyleKey,
}

struct SubtitleKeyRef<'a> {
    text: &'a str,
    reading: &'a str,
    font_family: &'a str,
    style: SubtitleStyleKey,
}

trait SubtitleCacheKeyLike {
    fn key_parts(&self) -> (&str, &str, &str, SubtitleStyleKey);
}

impl SubtitleCacheKeyLike for SubtitleCacheKey {
    #[inline]
    fn key_parts(&self) -> (&str, &str, &str, SubtitleStyleKey) {
        (
            self.text.as_str(),
            self.reading.as_str(),
            self.font_family.as_str(),
            self.style,
        )
    }
}

impl SubtitleCacheKeyLike for SubtitleKeyRef<'_> {
    #[inline]
    fn key_parts(&self) -> (&str, &str, &str, SubtitleStyleKey) {
        (self.text, self.reading, self.font_family, self.style)
    }
}

impl PartialEq for SubtitleCacheKey {
    #[inline]
    fn eq(&self, other: &Self) -> bool {
        self.key_parts() == other.key_parts()
    }
}

impl Hash for SubtitleCacheKey {
    #[inline]
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.key_parts().hash(state);
    }
}

impl PartialEq for dyn SubtitleCacheKeyLike + '_ {
    #[inline]
    fn eq(&self, other: &Self) -> bool {
        self.key_parts() == other.key_parts()
    }
}

impl Eq for dyn SubtitleCacheKeyLike + '_ {}

impl Hash for dyn SubtitleCacheKeyLike + '_ {
    #[inline]
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.key_parts().hash(state);
    }
}

impl<'a> Borrow<dyn SubtitleCacheKeyLike + 'a> for SubtitleCacheKey {
    #[inline]
    fn borrow(&self) -> &(dyn SubtitleCacheKeyLike + 'a) {
        self
    }
}

impl SubtitleKeyRef<'_> {
    fn to_owned_key(&self) -> SubtitleCacheKey {
        SubtitleCacheKey {
            text: self.text.to_owned(),
            reading: self.reading.to_owned(),
            font_family: self.font_family.to_owned(),
            style: self.style,
        }
    }
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

    fn get<Q: Hash + Eq + ?Sized>(&mut self, key: &Q) -> Option<&V>
    where
        K: Borrow<Q>,
    {
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

    fn shrink_to(&mut self, target_entries: usize) {
        if self.entries.len() > target_entries {
            if target_entries == 0 {
                self.entries.clear();
            } else {
                let drop_count = self.entries.len() - target_entries;
                let mut stamps: Vec<u64> = self.entries.values().map(|(s, _)| *s).collect();
                let (_, threshold, _) = stamps.select_nth_unstable(drop_count - 1);
                let cutoff = *threshold;
                self.entries.retain(|_, (s, _)| *s > cutoff);
            }
        }
        self.entries.shrink_to_fit();
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.entries.len()
    }

    fn contains_key<Q: Hash + Eq + ?Sized>(&self, key: &Q) -> bool
    where
        K: Borrow<Q>,
    {
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

/// Exact rounded division by 255 for `x <= 65_025` (`(x + 127) / 255` without integer division).
#[inline]
pub(super) fn div255(x: u32) -> u32 {
    let t = x + 128;
    (t + (t >> 8)) >> 8
}

#[inline]
pub(super) fn composite_rgb_over_opaque(fg: [u8; 4], bg: [u8; 4]) -> [u8; 3] {
    let a = u32::from(fg[3]);
    let inv = 255 - a;
    [
        div255(u32::from(fg[0]) * a + u32::from(bg[0]) * inv) as u8,
        div255(u32::from(fg[1]) * a + u32::from(bg[1]) * inv) as u8,
        div255(u32::from(fg[2]) * a + u32::from(bg[2]) * inv) as u8,
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
    pixmap: &mut tiny_skia::PixmapMut<'_>,
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
    let opaque_pm = tiny_skia::PremultipliedColorU8::from_rgba(sr, sg, sb, 255);
    let sr_u = u32::from(sr);
    let sg_u = u32::from(sg);
    let sb_u = u32::from(sb);
    let br_u = u32::from(backdrop_rgb[0]);
    let bg_u = u32::from(backdrop_rgb[1]);
    let bb_u = u32::from(backdrop_rgb[2]);

    let pixels = pixmap.pixels_mut();
    let pw_u = pw as usize;
    let gw_u = gw as usize;
    let span_w = (sx_end - sx_start) as usize;
    let dst_x_off = (x0 + sx_start) as usize;
    let mask_x_off = sx_start as usize;

    for sy in sy_start..sy_end {
        let dst_start = (y0 + sy) as usize * pw_u + dst_x_off;
        let mask_start = (sy as usize) * gw_u + mask_x_off;
        let dst_row = &mut pixels[dst_start..dst_start + span_w];
        let mask_row = &glyph.mask[mask_start..mask_start + span_w];

        for (dst_slot, &raw_cov) in dst_row.iter_mut().zip(mask_row.iter()) {
            if raw_cov == 0 {
                continue;
            }
            let dst = *dst_slot;
            if dst.alpha() == 0 {
                let cov = base_lut[usize::from(raw_cov)];
                if cov == 0 {
                    continue;
                }
                if sa == 255 && cov == 255 {
                    if let Some(pm) = opaque_pm {
                        *dst_slot = pm;
                    }
                    continue;
                }
                let alpha = if sa == 255 {
                    u32::from(cov)
                } else {
                    div255(u32::from(cov) * sa_u)
                };
                if alpha == 0 {
                    continue;
                }
                let out_r = div255(sr_u * alpha).min(alpha) as u8;
                let out_g = div255(sg_u * alpha).min(alpha) as u8;
                let out_b = div255(sb_u * alpha).min(alpha) as u8;
                if let Some(pm) =
                    tiny_skia::PremultipliedColorU8::from_rgba(out_r, out_g, out_b, alpha as u8)
                {
                    *dst_slot = pm;
                }
                continue;
            }

            let da = u32::from(dst.alpha());
            let inv_da = 255 - da;
            let eff_r = (u32::from(dst.red()) + div255(br_u * inv_da)).min(255) as u8;
            let eff_g = (u32::from(dst.green()) + div255(bg_u * inv_da)).min(255) as u8;
            let eff_b = (u32::from(dst.blue()) + div255(bb_u * inv_da)).min(255) as u8;
            let eff_lum = srgb_luma(eff_r, eff_g, eff_b);
            let cov = u32::from(tables[light_on_dark_lut_level(fg_lum, eff_lum)][usize::from(raw_cov)]);
            if cov == 0 {
                continue;
            }
            let alpha = if sa == 255 {
                cov
            } else {
                div255(cov * sa_u)
            };
            if alpha == 0 {
                continue;
            }
            if alpha == 255 {
                if let Some(pm) = opaque_pm {
                    *dst_slot = pm;
                }
                continue;
            }
            let inv_a = 255 - alpha;
            let out_a = (alpha + div255(da * inv_a)).min(255) as u8;
            let out_a_u = u32::from(out_a);
            let out_r = div255(sr_u * alpha + u32::from(dst.red()) * inv_a).min(out_a_u) as u8;
            let out_g = div255(sg_u * alpha + u32::from(dst.green()) * inv_a).min(out_a_u) as u8;
            let out_b = div255(sb_u * alpha + u32::from(dst.blue()) * inv_a).min(out_a_u) as u8;
            if let Some(pm) =
                tiny_skia::PremultipliedColorU8::from_rgba(out_r, out_g, out_b, out_a)
            {
                *dst_slot = pm;
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

static CACHED_SHARED_FONT_COLLECTION: OnceLock<slint::fontique_08::fontique::Collection> =
    OnceLock::new();
static SHARED_FONT_SOURCE_CACHE: OnceLock<slint::fontique_08::fontique::SourceCache> =
    OnceLock::new();

fn shared_font_source_cache() -> slint::fontique_08::fontique::SourceCache {
    SHARED_FONT_SOURCE_CACHE
        .get_or_init(slint::fontique_08::fontique::SourceCache::new_shared)
        .clone()
}

/// Capture Slint's process-wide `fontique::Collection` on the UI thread (where
/// `i_slint_core`'s thread-local context is initialized) so background render workers
/// reuse the same system font database and loaded font blobs without re-scanning.
pub(super) fn capture_shared_font_collection() -> Option<slint::fontique_08::fontique::Collection> {
    if let Some(collection) = CACHED_SHARED_FONT_COLLECTION.get() {
        return Some(collection.clone());
    }
    if let Ok(collection) = std::panic::catch_unwind(slint::fontique_08::shared_collection) {
        let _ = CACHED_SHARED_FONT_COLLECTION.set(collection.clone());
        return Some(collection);
    }
    None
}

/// Initialize Parley's `FontContext` sharing Slint's process-wide `fontique::Collection`
/// and a thread-safe shared `SourceCache` when active (avoiding duplicate font DB scans
/// and duplicate CJK font file reads in RAM), falling back cleanly to `FontContext::new()`
/// in headless unit tests without a Slint platform.
fn shared_parley_font_context(
    pre_captured: Option<slint::fontique_08::fontique::Collection>,
) -> FontContext {
    if let Some(collection) = pre_captured.or_else(capture_shared_font_collection) {
        FontContext {
            collection,
            source_cache: shared_font_source_cache(),
        }
    } else {
        FontContext::new()
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
    subtitle_width_cache: BoundedLruCache<SubtitleCacheKey, f32>,
    devoice_circle: Option<slint::Image>,
}

thread_local! {
    static SHARED_TEXT_ENGINE: RefCell<TextEngine> = RefCell::new(TextEngine::new());
}

pub(super) fn with_shared_text_engine<R>(f: impl FnOnce(&mut TextEngine) -> R) -> R {
    SHARED_TEXT_ENGINE.with(|cell| f(&mut cell.borrow_mut()))
}

pub(super) fn shrink_shared_text_engine_caches() {
    SHARED_TEXT_ENGINE.with(|cell| {
        if let Ok(mut engine) = cell.try_borrow_mut() {
            engine.shrink_caches_for_idle();
        }
    });
}

impl TextEngine {
    pub(super) fn new() -> Self {
        Self::new_with_collection(None)
    }

    pub(super) fn new_with_collection(
        collection: Option<slint::fontique_08::fontique::Collection>,
    ) -> Self {
        Self {
            font_cx: shared_parley_font_context(collection),
            layout_cx: LayoutContext::new(),
            scale_cx: swash::scale::ScaleContext::new(),
            fontdb: Arc::new(usvg::fontdb::Database::new()),
            registered_fonts: HashMap::new(),
            font_is_color: HashMap::new(),
            emoji_cache: BoundedLruCache::new(EMOJI_CACHE_MAX_ENTRIES),
            alpha_glyph_cache: BoundedLruCache::new(ALPHA_GLYPH_CACHE_MAX_ENTRIES),
            default_svg_fonts_primed: false,
            subtitle_cache: BoundedLruCache::new(SUBTITLE_CACHE_MAX_ENTRIES),
            subtitle_width_cache: BoundedLruCache::new(SUBTITLE_WIDTH_CACHE_MAX_ENTRIES),
            devoice_circle: None,
        }
    }

    pub(super) fn shrink_caches_for_idle(&mut self) {
        self.alpha_glyph_cache
            .shrink_to(IDLE_ALPHA_GLYPH_CACHE_MAX_ENTRIES);
        self.emoji_cache.shrink_to(IDLE_EMOJI_CACHE_MAX_ENTRIES);
        self.subtitle_cache
            .shrink_to(IDLE_SUBTITLE_CACHE_MAX_ENTRIES);
        self.subtitle_width_cache
            .shrink_to(IDLE_SUBTITLE_WIDTH_CACHE_MAX_ENTRIES);
        self.font_cx.source_cache.prune(2, true);
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
}

enum LazyAlphaScaler<'a> {
    Uninit(&'a mut swash::scale::ScaleContext),
    Ready(swash::scale::Scaler<'a>),
    Failed,
}

/// Per-`GlyphRun` alpha mask cache/rasterizer helper.
/// Warm cache hits pay zero scaler setup; the first cold miss lazily builds a
/// single `swash::Scaler` reused across all subsequent misses in the same run.
pub(super) struct AlphaGlyphRun<'a> {
    font_bytes: &'a [u8],
    font_id: u64,
    font_index: u32,
    font_size: f32,
    size_tenth: i32,
    skew_key: i8,
    synthetic_italic: bool,
    scaler: LazyAlphaScaler<'a>,
    cache: &'a mut BoundedLruCache<(u64, u32, u16, i32, i8, u8), Option<CachedAlphaGlyph>>,
}

impl<'a> AlphaGlyphRun<'a> {
    pub(super) fn render(&mut self, glyph_id: u16, subpixel_quarter: u8) -> Option<CachedAlphaGlyph> {
        let sub_q = subpixel_quarter & 3;
        let key = (
            self.font_id,
            self.font_index,
            glyph_id,
            self.size_tenth,
            self.skew_key,
            sub_q,
        );
        if let Some(cached) = self.cache.get(&key) {
            return cached.clone();
        }
        let synth_italic = self.synthetic_italic;
        let rendered = self
            .ensure_scaler()
            .and_then(|scaler| render_alpha_mask_with_scaler(scaler, glyph_id, sub_q, synth_italic));
        self.cache.insert(key, rendered.clone());
        rendered
    }

    fn ensure_scaler(&mut self) -> Option<&mut swash::scale::Scaler<'a>> {
        if matches!(self.scaler, LazyAlphaScaler::Uninit(_)) {
            let prev = std::mem::replace(&mut self.scaler, LazyAlphaScaler::Failed);
            if let LazyAlphaScaler::Uninit(scale_cx) = prev {
                if let Some(font_ref) =
                    swash::FontRef::from_index(self.font_bytes, self.font_index as usize)
                {
                    let scaler = scale_cx
                        .builder(font_ref)
                        .size(self.font_size.max(6.0))
                        .hint(true)
                        .build();
                    self.scaler = LazyAlphaScaler::Ready(scaler);
                }
            }
        }
        match &mut self.scaler {
            LazyAlphaScaler::Ready(scaler) => Some(scaler),
            _ => None,
        }
    }
}

fn render_alpha_mask_with_scaler(
    scaler: &mut swash::scale::Scaler<'_>,
    glyph_id: u16,
    sub_q: u8,
    synthetic_italic: bool,
) -> Option<CachedAlphaGlyph> {
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
    let image = render.render(scaler, glyph_id)?;
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
}

impl TextEngine {
    pub(super) fn alpha_glyph_run<'a>(
        &'a mut self,
        font: &'a parley::FontData,
        font_size: f32,
        synthetic_italic: bool,
    ) -> AlphaGlyphRun<'a> {
        AlphaGlyphRun {
            font_bytes: font.data.data(),
            font_id: font.data.id(),
            font_index: font.index,
            font_size,
            size_tenth: (font_size * 10.0).round() as i32,
            skew_key: if synthetic_italic { 1 } else { 0 },
            synthetic_italic,
            scaler: LazyAlphaScaler::Uninit(&mut self.scale_cx),
            cache: &mut self.alpha_glyph_cache,
        }
    }

    pub(super) fn try_render_alpha_glyph(
        &mut self,
        font: &parley::FontData,
        glyph_id: u16,
        font_size: f32,
        synthetic_italic: bool,
        subpixel_quarter: u8,
    ) -> Option<CachedAlphaGlyph> {
        self.alpha_glyph_run(font, font_size, synthetic_italic)
            .render(glyph_id, subpixel_quarter)
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
                    if !is_color_font {
                        let mut alpha_run = self.alpha_glyph_run(
                            run_font.borrow_font(),
                            scaled_font_size,
                            synth_italic,
                        );
                        for g in glyph_run.positioned_glyphs() {
                            let px = origin_x + g.x;
                            let py = origin_y + g.y;
                            let phys_x = px * scale;
                            let phys_y = py * scale;
                            let (glyph_x, sub_q) = if scale < 1.75 {
                                quantize_subpixel_x(phys_x)
                            } else {
                                (phys_x.round() as i32, 0)
                            };
                            if let Some(ag) = alpha_run.render(g.id as u16, sub_q) {
                                collector.mask_glyphs.push(PositionedMaskGlyph {
                                    x: glyph_x + ag.left,
                                    y: phys_y.round() as i32 - ag.top,
                                    glyph: ag,
                                    rgba: brush,
                                });
                            }
                            gx = gx.max(px + g.advance);
                        }
                    } else {
                        for g in glyph_run.positioned_glyphs() {
                            let px = origin_x + g.x;
                            let py = origin_y + g.y;
                            let phys_x = px * scale;
                            let phys_y = py * scale;
                            let rendered_color = self
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
        let trimmed = font_family.trim();
        let family_stack =
            if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("Noto Sans CJK JP") {
                Cow::Borrowed(RICH_EMOJI_FALLBACK_STACK)
            } else {
                Cow::Owned(format!("{trimmed}, {RICH_EMOJI_FALLBACK_STACK}"))
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

    fn make_subtitle_key_ref<'a>(
        text: &'a str,
        reading: &'a str,
        font_px: f32,
        show_furigana: bool,
        style: &'a SubtitleRenderStyle,
    ) -> SubtitleKeyRef<'a> {
        let font_px = font_px.clamp(8.0, 140.0);
        let line_spacing_extra = style.line_spacing.clamp(0.0, 120.0);
        SubtitleKeyRef {
            text,
            reading: if show_furigana { reading } else { "" },
            font_family: style.font_family.as_str(),
            style: SubtitleStyleKey {
                font_tenth_px: (font_px * 10.0).round() as i32,
                show_furigana,
                text_rgba: style.text_rgba,
                stroke_rgba: style.stroke_rgba,
                stroke_tenth: (style.stroke_size * 10.0).round() as i32,
                furigana_pct: (style.furigana_ratio * 100.0).round() as i32,
                line_spacing_tenth: (line_spacing_extra * 10.0).round() as i32,
                bold: style.bold,
                italic: style.italic,
            },
        }
    }

    fn make_subtitle_cache_key_parts(
        text: &str,
        reading: &str,
        font_px: f32,
        show_furigana: bool,
        style: &SubtitleRenderStyle,
    ) -> SubtitleCacheKey {
        Self::make_subtitle_key_ref(text, reading, font_px, show_furigana, style).to_owned_key()
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
        let key_ref = Self::make_subtitle_key_ref(
            word.text.as_str(),
            word.reading.as_str(),
            font_px,
            show_furigana,
            style,
        );
        let lookup: &dyn SubtitleCacheKeyLike = &key_ref;
        if let Some(cached) = self.subtitle_cache.get(lookup) {
            return (cached.width_px > 0.0).then_some(cached.width_px);
        }
        if let Some(&cached_w) = self.subtitle_width_cache.get(lookup) {
            return (cached_w > 0.0).then_some(cached_w);
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

        let (_, _, _, layout_width) = self.compute_fitted_subtitle_ruby(
            word.reading.as_str(),
            base_width,
            font_px,
            show_furigana,
            style,
        );
        self.subtitle_width_cache
            .insert(key_ref.to_owned_key(), layout_width);
        (layout_width > 0.0).then_some(layout_width)
    }

    /// Shape furigana for a subtitle token while fitting it tightly over the base kanji
    /// (C++ `SubtitleText.qml:183-216` fit behavior): when nominal ruby exceeds `base_width`,
    /// scale ruby font size down gently (readable floor `0.72x`, minimum `7.0px`) and allow a
    /// modest horizontal overhang (`0.25 * font_px` per side) into `bleed_x` rather than
    /// widening the base token advance and creating jarring gaps in subtitles.
    ///
    /// Returns `(ruby_layout, ruby_width, ruby_height, layout_width)`.
    fn compute_fitted_subtitle_ruby(
        &mut self,
        reading: &str,
        base_width: f32,
        font_px: f32,
        show_furigana: bool,
        style: &SubtitleRenderStyle,
    ) -> (Option<Layout<[u8; 4]>>, f32, f32, f32) {
        const FURIGANA_MIN_FIT_SCALE: f32 = 0.72;
        const FURIGANA_MIN_FONT_PX: f32 = 7.0;

        if !show_furigana || reading.is_empty() {
            return (None, 0.0, 0.0, base_width.ceil().max(1.0));
        }

        let furigana_ratio = style.furigana_ratio.clamp(0.25, 0.75);
        let nominal_ruby_px = (font_px * furigana_ratio).max(8.0);
        let mut layout = self.shape_single_line(
            reading,
            &style.font_family,
            nominal_ruby_px,
            600,
            style.italic,
            style.text_rgba,
        );
        let mut ruby_width = layout
            .lines()
            .next()
            .map(|l| l.metrics().advance)
            .unwrap_or(0.0);
        let mut used_ruby_px = nominal_ruby_px;

        if ruby_width > base_width && ruby_width > 0.0 {
            let fit_scale = (base_width / ruby_width).clamp(FURIGANA_MIN_FIT_SCALE, 1.0);
            let fitted_ruby_px = (nominal_ruby_px * fit_scale)
                .max(FURIGANA_MIN_FONT_PX)
                .min(nominal_ruby_px);
            if (nominal_ruby_px - fitted_ruby_px) > 0.25 {
                used_ruby_px = fitted_ruby_px;
                layout = self.shape_single_line(
                    reading,
                    &style.font_family,
                    used_ruby_px,
                    600,
                    style.italic,
                    style.text_rgba,
                );
                ruby_width = layout
                    .lines()
                    .next()
                    .map(|l| l.metrics().advance)
                    .unwrap_or(0.0);
            }
        }

        let ruby_height = layout
            .lines()
            .next()
            .map(|l| l.metrics().line_height)
            .unwrap_or(used_ruby_px * 1.2);
        let max_ruby_bleed = (font_px * 0.25).max(0.0);
        let layout_width = base_width
            .max(ruby_width - max_ruby_bleed * 2.0)
            .ceil()
            .max(1.0);
        (Some(layout), ruby_width, ruby_height, layout_width)
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

    fn rasterize_subtitle_token(
        &mut self,
        text: &str,
        reading: &str,
        font_px: f32,
        show_furigana: bool,
        style: &SubtitleRenderStyle,
    ) -> RasterizedSubtitleToken {
        let font_px = font_px.clamp(8.0, 140.0);
        let line_spacing_extra = style.line_spacing.clamp(0.0, 120.0);
        let weight = if style.bold { 700 } else { 400 };
        let base_layout = self.shape_single_line(
            text,
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
        // never develop artificial gaps, while letting strokes/shadows and modest ruby
        // overhang bleed visually into `bleed_x` on each side of the rendered image.
        let stroke_bleed_x = if stroke_px > 0.0 {
            (stroke_px * 0.75 + 1.5).ceil()
        } else {
            1.0
        };
        let pad_y = (stroke_px * 0.55).ceil().max(1.0);

        let furigana_ratio = style.furigana_ratio.clamp(0.25, 0.75);
        let ruby_space = if show_furigana {
            (font_px * furigana_ratio.max(0.45) + 3.0).ceil()
        } else {
            0.0
        };

        let (ruby_layout, ruby_width, ruby_height, layout_width) =
            self.compute_fitted_subtitle_ruby(reading, base_width, font_px, show_furigana, style);
        let ruby_overhang_x = ((ruby_width - layout_width) * 0.5).max(0.0).ceil();
        let bleed_x = stroke_bleed_x + ruby_overhang_x;
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
                    let byte_offset = r.start.min(text.len());
                    let char_offset = text
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
            text,
            base_origin_x_pix,
            base_origin_y,
        );
        let ruby_geom = ruby_layout.as_ref().map(|r_layout| {
            let r_origin_x_pix = bleed_x + (layout_width - ruby_width) * 0.5;
            let r_origin_y = (ruby_space - ruby_height).max(0.0) + pad_y * 0.5 + half_line_spacing;
            self.collect_subtitle_layer_geometry(
                r_layout,
                reading,
                r_origin_x_pix,
                r_origin_y,
            )
        });

        let pix_w = ((layout_width + bleed_x * 2.0).ceil() as u32).max(1);
        let pix_h = (total_height.ceil() as u32).max(1);
        let mut fill_pixels = SharedPixelBuffer::<Rgba8Pixel>::new(pix_w, pix_h);

        let has_stroke_paths = base_geom.path.is_some()
            || ruby_geom.as_ref().is_some_and(|g| g.path.is_some());
        let mut stroke_pixels = (stroke_px > 0.0 && style.stroke_rgba[3] > 0 && has_stroke_paths)
            .then(|| SharedPixelBuffer::<Rgba8Pixel>::new(pix_w, pix_h));

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

        // Render outer outlines + drop shadows into a separate pass from glyph fills so
        // the UI can composite all token strokes for a line underneath all token fills
        // (matching libass 2-pass rendering and preventing token i+1's outline from
        // painting over token i's fill at MeCab token boundaries).
        let paint_stroke_layer = |pixmap: &mut tiny_skia::PixmapMut<'_>,
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
            }
        };

        let paint_fill_layer =
            |pixmap: &mut tiny_skia::PixmapMut<'_>, geom: &SubtitleLayerGeometry| {
                if let Some(ref path) = geom.path {
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

        if let Some(ref mut sp_buf) = stroke_pixels {
            if let Some(mut sp) =
                tiny_skia::PixmapMut::from_bytes(sp_buf.make_mut_bytes(), pix_w, pix_h)
            {
                if let Some(ref rg) = ruby_geom {
                    paint_stroke_layer(&mut sp, rg, ruby_stroke_px);
                }
                paint_stroke_layer(&mut sp, &base_geom, stroke_px);
            }
        }

        if let Some(mut fill_pm) =
            tiny_skia::PixmapMut::from_bytes(fill_pixels.make_mut_bytes(), pix_w, pix_h)
        {
            if let Some(ref rg) = ruby_geom {
                paint_fill_layer(&mut fill_pm, rg);
            }
            paint_fill_layer(&mut fill_pm, &base_geom);

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
                    fill_pm.draw_pixmap(
                        ix.round() as i32,
                        iy.round() as i32,
                        (*img_pm).as_ref(),
                        &img_paint,
                        tiny_skia::Transform::default(),
                        None,
                    );
                }
            }
        }

        RasterizedSubtitleToken {
            width_px: layout_width,
            height_px: total_height,
            bleed_x_px: bleed_x,
            glyph_x_px: base_origin_x_token,
            glyph_y_px: base_origin_y,
            glyph_width_px: base_width,
            glyph_height_px: base_height,
            clusters,
            cluster_hitboxes,
            stroke_pixels,
            pixels: fill_pixels,
        }
    }

    pub(super) fn shape_and_render_subtitle_token(
        &mut self,
        word: &SubtitleWord,
        font_px: f32,
        show_furigana: bool,
        style: &SubtitleRenderStyle,
    ) -> ShapedSubtitleToken {
        let key_ref = Self::make_subtitle_key_ref(
            word.text.as_str(),
            word.reading.as_str(),
            font_px,
            show_furigana,
            style,
        );
        let lookup: &dyn SubtitleCacheKeyLike = &key_ref;
        if let Some(cached) = self.subtitle_cache.get(lookup) {
            return cached.clone();
        }
        let shaped = self
            .rasterize_subtitle_token(
                word.text.as_str(),
                word.reading.as_str(),
                font_px,
                show_furigana,
                style,
            )
            .into_shaped();
        self.subtitle_cache
            .insert(key_ref.to_owned_key(), shaped.clone());
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

pub(super) fn pixmap_to_pixel_buffer(
    pixmap: &tiny_skia::Pixmap,
) -> SharedPixelBuffer<Rgba8Pixel> {
    SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(
        pixmap.data(),
        pixmap.width(),
        pixmap.height(),
    )
}

pub(super) fn pixmap_to_slint_image(pixmap: &tiny_skia::Pixmap) -> slint::Image {
    slint::Image::from_rgba8_premultiplied(pixmap_to_pixel_buffer(pixmap))
}

struct RasterizedSubtitleToken {
    width_px: f32,
    height_px: f32,
    bleed_x_px: f32,
    glyph_x_px: f32,
    glyph_y_px: f32,
    glyph_width_px: f32,
    glyph_height_px: f32,
    clusters: Vec<(usize, usize, f32, f32)>,
    cluster_hitboxes: Vec<SubtitleClusterHitbox>,
    stroke_pixels: Option<SharedPixelBuffer<Rgba8Pixel>>,
    pixels: SharedPixelBuffer<Rgba8Pixel>,
}

impl RasterizedSubtitleToken {
    fn into_shaped(self) -> ShapedSubtitleToken {
        let (has_stroke, stroke_image) = match self.stroke_pixels {
            Some(buf) => (true, slint::Image::from_rgba8_premultiplied(buf)),
            None => (false, slint::Image::default()),
        };
        ShapedSubtitleToken {
            width_px: self.width_px,
            height_px: self.height_px,
            bleed_x_px: self.bleed_x_px,
            glyph_x_px: self.glyph_x_px,
            glyph_y_px: self.glyph_y_px,
            glyph_width_px: self.glyph_width_px,
            glyph_height_px: self.glyph_height_px,
            clusters: self.clusters,
            cluster_hitboxes: self.cluster_hitboxes,
            has_stroke,
            stroke_image,
            image: slint::Image::from_rgba8_premultiplied(self.pixels),
        }
    }
}

struct SubtitlePrewarmJob {
    epoch: u64,
    lines: Vec<String>,
    font_px: f32,
    show_furigana: bool,
    style: SubtitleRenderStyle,
    font_collection: Option<slint::fontique_08::fontique::Collection>,
}

struct SubtitlePrewarmWorker {
    slot: Mutex<Option<SubtitlePrewarmJob>>,
    condvar: Condvar,
}

static SUBTITLE_PREWARM_EPOCH: AtomicU64 = AtomicU64::new(0);
static SUBTITLE_PREWARM_WORKER: OnceLock<Arc<SubtitlePrewarmWorker>> = OnceLock::new();

fn subtitle_prewarm_worker() -> &'static Arc<SubtitlePrewarmWorker> {
    SUBTITLE_PREWARM_WORKER.get_or_init(|| {
        let worker = Arc::new(SubtitlePrewarmWorker {
            slot: Mutex::new(None),
            condvar: Condvar::new(),
        });
        let bg = Arc::clone(&worker);
        let _ = std::thread::Builder::new()
            .name("subomi-sub-preraster".into())
            .spawn(move || subtitle_prewarm_worker_loop(bg));
        worker
    })
}

pub(crate) fn cancel_subtitle_token_prewarm() {
    let epoch = SUBTITLE_PREWARM_EPOCH.fetch_add(1, Ordering::AcqRel) + 1;
    if let Some(worker) = SUBTITLE_PREWARM_WORKER.get() {
        if let Ok(mut guard) = worker.slot.lock() {
            if SUBTITLE_PREWARM_EPOCH.load(Ordering::Acquire) == epoch {
                *guard = None;
            }
        }
    }
}

#[cfg(test)]
pub(crate) fn subtitle_token_prewarm_epoch_for_test() -> u64 {
    SUBTITLE_PREWARM_EPOCH.load(Ordering::Acquire)
}

fn subtitle_prewarm_worker_loop(worker: Arc<SubtitlePrewarmWorker>) {
    let mut engine: Option<TextEngine> = None;
    loop {
        let job = {
            let Ok(mut guard) = worker.slot.lock() else {
                return;
            };
            loop {
                if let Some(job) = guard.take() {
                    break job;
                }
                if engine.is_some() {
                    let (next_guard, timeout) = match worker
                        .condvar
                        .wait_timeout(guard, SUBTITLE_PREWARM_IDLE_TIMEOUT)
                    {
                        Ok(res) => res,
                        Err(_) => return,
                    };
                    guard = next_guard;
                    if guard.is_none() && timeout.timed_out() {
                        engine = None;
                        crate::ui::release_os_memory();
                    }
                } else {
                    guard = match worker.condvar.wait(guard) {
                        Ok(g) => g,
                        Err(_) => return,
                    };
                }
            }
        };

        if SUBTITLE_PREWARM_EPOCH.load(Ordering::Acquire) != job.epoch {
            continue;
        }

        let mut seen_tokens: HashSet<(String, String)> = HashSet::new();
        let mut rasterized_total = 0usize;

        for line in job.lines {
            if SUBTITLE_PREWARM_EPOCH.load(Ordering::Acquire) != job.epoch
                || rasterized_total >= SUBTITLE_PREWARM_MAX_TOKENS
            {
                break;
            }
            if line.trim().is_empty() {
                continue;
            }

            let parts: Vec<(String, String)> = if job.show_furigana {
                crate::controller::subtitles::subtitle_furigana_parts(&line).unwrap_or_else(|_| {
                    crate::controller::subtitles::segment_subtitle_text(&line)
                        .into_iter()
                        .map(|tok| (tok, String::new()))
                        .collect()
                })
            } else {
                crate::controller::subtitles::segment_subtitle_text(&line)
                    .into_iter()
                    .map(|tok| (tok, String::new()))
                    .collect()
            };

            if SUBTITLE_PREWARM_EPOCH.load(Ordering::Acquire) != job.epoch {
                break;
            }

            let eng = engine.get_or_insert_with(|| {
                TextEngine::new_with_collection(job.font_collection.clone())
            });
            let mut ready_batch: Vec<(SubtitleCacheKey, RasterizedSubtitleToken)> = Vec::new();
            for (tok, reading) in parts {
                if SUBTITLE_PREWARM_EPOCH.load(Ordering::Acquire) != job.epoch
                    || rasterized_total >= SUBTITLE_PREWARM_MAX_TOKENS
                {
                    break;
                }
                if tok.is_empty() || tok.contains('\n') {
                    continue;
                }
                let eff_reading = if job.show_furigana {
                    reading
                } else {
                    String::new()
                };
                if !seen_tokens.insert((tok.clone(), eff_reading.clone())) {
                    continue;
                }
                let key = TextEngine::make_subtitle_cache_key_parts(
                    &tok,
                    &eff_reading,
                    job.font_px,
                    job.show_furigana,
                    &job.style,
                );
                let rasterized = eng.rasterize_subtitle_token(
                    &tok,
                    &eff_reading,
                    job.font_px,
                    job.show_furigana,
                    &job.style,
                );
                ready_batch.push((key, rasterized));
                rasterized_total += 1;
            }

            if ready_batch.is_empty() {
                continue;
            }
            if SUBTITLE_PREWARM_EPOCH.load(Ordering::Acquire) != job.epoch {
                break;
            }

            let deliver_epoch = job.epoch;
            if slint::invoke_from_event_loop(move || {
                if SUBTITLE_PREWARM_EPOCH.load(Ordering::Acquire) != deliver_epoch {
                    return;
                }
                with_shared_text_engine(|ui_engine| {
                    for (key, rasterized) in ready_batch {
                        if !ui_engine.subtitle_cache.contains_key(&key) {
                            ui_engine.subtitle_cache.insert(key, rasterized.into_shaped());
                        }
                    }
                });
            })
            .is_err()
            {
                break;
            }
        }
    }
}

/// Non-blockingly pre-rasterizes upcoming subtitle lines' tokens on a single reusable
/// background worker and feeds completed `SharedPixelBuffer<Rgba8Pixel>` + cluster metadata
/// into the UI thread's `TextEngine::subtitle_cache` via `slint::invoke_from_event_loop`.
/// Cancels any previous in-flight prewarm generation immediately (e.g. on seek or style change).
pub fn prewarm_subtitle_tokens(
    lines: Vec<String>,
    font_px: f32,
    show_furigana: bool,
    style: SubtitleRenderStyle,
) {
    let epoch = SUBTITLE_PREWARM_EPOCH.fetch_add(1, Ordering::AcqRel) + 1;
    if lines.is_empty() || !font_px.is_finite() || font_px <= 0.0 {
        if let Some(worker) = SUBTITLE_PREWARM_WORKER.get() {
            if let Ok(mut guard) = worker.slot.lock() {
                if SUBTITLE_PREWARM_EPOCH.load(Ordering::Acquire) == epoch {
                    *guard = None;
                }
            }
        }
        return;
    }

    let font_collection = capture_shared_font_collection();
    let worker = subtitle_prewarm_worker();
    if let Ok(mut guard) = worker.slot.lock() {
        if SUBTITLE_PREWARM_EPOCH.load(Ordering::Acquire) == epoch {
            *guard = Some(SubtitlePrewarmJob {
                epoch,
                lines,
                font_px,
                show_furigana,
                style,
                font_collection,
            });
            worker.condvar.notify_one();
        }
    }
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

    #[test]
    fn furigana_scales_down_to_fit_kanji_without_widening_base_gaps() {
        let mut engine = TextEngine::new();
        let style = SubtitleRenderStyle::default();
        let font_px = 40.0;

        let base_word = SubtitleWord {
            text: "心".into(),
            reading: "".into(),
            ..SubtitleWord::default()
        };
        let ruby_word = SubtitleWord {
            text: "心".into(),
            reading: "こころ".into(),
            ..SubtitleWord::default()
        };
        let long_ruby_word = SubtitleWord {
            text: "承".into(),
            reading: "うけたまわ".into(),
            ..SubtitleWord::default()
        };

        let base_tok = engine.rasterize_subtitle_token("心", "", font_px, true, &style);
        let ruby_tok = engine.rasterize_subtitle_token("心", "こころ", font_px, true, &style);
        let measured_ruby_w = engine
            .measure_subtitle_token_width(&ruby_word, font_px, true, &style)
            .expect("measured ruby width");
        let measured_base_w = engine
            .measure_subtitle_token_width(&base_word, font_px, true, &style)
            .expect("measured base width");

        // Modestly wider reading ("こころ" over "心") scales down and fits within base + bleed
        // without creating ugly horizontal gaps between adjacent subtitle tokens.
        assert_eq!(
            ruby_tok.width_px, base_tok.width_px,
            "3-kana furigana over 1 kanji should fit without widening base token advance ({} vs {})",
            ruby_tok.width_px, base_tok.width_px
        );
        assert_eq!(measured_ruby_w, ruby_tok.width_px);
        assert_eq!(measured_base_w, base_tok.width_px);

        // Very long reading ("うけたまわ" over "承") scales down to the readable floor and keeps
        // overhang safely inside `bleed_x_px` without clipping.
        let long_base = engine.rasterize_subtitle_token("承", "", font_px, true, &style);
        let long_tok =
            engine.rasterize_subtitle_token("承", "うけたまわ", font_px, true, &style);
        let measured_long_w = engine
            .measure_subtitle_token_width(&long_ruby_word, font_px, true, &style)
            .expect("measured long ruby width");
        assert_eq!(measured_long_w, long_tok.width_px);
        assert!(
            long_tok.bleed_x_px > long_base.bleed_x_px,
            "overhanging ruby must expand bleed_x_px so glyphs are never clipped ({} vs {})",
            long_tok.bleed_x_px,
            long_base.bleed_x_px
        );
        // Must still be substantially tighter than unscaled 5-kana ruby (~100px at 40px base)
        assert!(
            long_tok.width_px < long_base.width_px * 1.75,
            "scaled ruby layout width ({}) should stay tight over base ({})",
            long_tok.width_px,
            long_base.width_px
        );
    }

    #[test]
    fn subtitle_stroke_and_fill_are_rasterized_into_separate_layers() {
        let mut engine = TextEngine::new();
        let mut style = SubtitleRenderStyle::default();
        style.text_rgba = [255, 255, 255, 255];
        style.stroke_rgba = [0, 0, 0, 255];
        style.stroke_size = 10.0;

        let tok = engine.rasterize_subtitle_token("年", "", 48.0, false, &style);
        let stroke_buf = tok
            .stroke_pixels
            .as_ref()
            .expect("stroked subtitle token must emit a dedicated stroke layer");
        assert_eq!(stroke_buf.width(), tok.pixels.width());
        assert_eq!(stroke_buf.height(), tok.pixels.height());

        // Stroke layer must carry dark outline pixels (including in the horizontal bleed margin),
        // and no white fill pixels.
        let stroke_px_count = stroke_buf
            .as_slice()
            .iter()
            .filter(|p| p.a > 0 && p.r == 0 && p.g == 0 && p.b == 0)
            .count();
        assert!(
            stroke_px_count > 100,
            "stroke layer should contain dark outline pixels (got {stroke_px_count})"
        );

        let bleed_cols = (tok.bleed_x_px.floor() as usize).min(stroke_buf.width() as usize);
        let left_bleed_stroke = (0..stroke_buf.height() as usize)
            .flat_map(|y| (0..bleed_cols).map(move |x| (x, y)))
            .filter(|&(x, y)| {
                let p = stroke_buf.as_slice()[y * stroke_buf.width() as usize + x];
                p.a > 0
            })
            .count();
        assert!(
            left_bleed_stroke > 0,
            "heavy stroke must bleed into left margin of the stroke layer"
        );

        // Fill layer must carry white glyph fill pixels and zero opaque dark stroke pixels,
        // so adjacent tokens' outlines cannot paint over the previous token's white fill.
        let fill_white_count = tok
            .pixels
            .as_slice()
            .iter()
            .filter(|p| p.a == 255 && p.r == 255 && p.g == 255 && p.b == 255)
            .count();
        let fill_dark_count = tok
            .pixels
            .as_slice()
            .iter()
            .filter(|p| p.a > 0 && (p.r < p.a || p.g < p.a || p.b < p.a))
            .count();
        assert!(
            fill_white_count > 100,
            "fill layer must contain white glyph pixels (got {fill_white_count})"
        );
        assert_eq!(
            fill_dark_count, 0,
            "fill layer must not bake dark outline pixels into the foreground pass"
        );

        // Disabling stroke or rasterizing whitespace omits the stroke buffer completely.
        let mut no_stroke = style.clone();
        no_stroke.stroke_size = 0.0;
        assert!(
            engine
                .rasterize_subtitle_token("年", "", 48.0, false, &no_stroke)
                .stroke_pixels
                .is_none(),
            "zero stroke size must omit stroke layer"
        );
        assert!(
            engine
                .rasterize_subtitle_token(" ", "", 48.0, false, &style)
                .stroke_pixels
                .is_none(),
            "whitespace-only token must omit stroke layer"
        );
    }

    #[test]
    fn div255_matches_rounded_division_for_all_u8_products() {
        for x in 0..=65_025u32 {
            assert_eq!(div255(x), (x + 127) / 255, "mismatch at x={x}");
        }
    }

    #[test]
    fn bounded_lru_cache_shrink_to_retains_hottest_entries() {
        let mut cache: BoundedLruCache<u32, u32> = BoundedLruCache::new(32);
        for i in 0..20 {
            cache.insert(i, i * 10);
        }
        // Touch 0 and 1 so they become the hottest.
        assert_eq!(cache.get(&0), Some(&0));
        assert_eq!(cache.get(&1), Some(&10));
        cache.shrink_to(4);
        assert!(cache.len() <= 4);
        assert!(cache.contains_key(&0));
        assert!(cache.contains_key(&1));
        assert!(cache.contains_key(&19));
        assert!(!cache.contains_key(&2));
    }
}

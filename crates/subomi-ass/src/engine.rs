//! Safe `libass` layout/hitbox probing and tight highlight-patch rendering engine.
//!
//! [`AssLayoutEngine`] mirrors `mpv`'s `sd_ass.c` renderer configuration so per-character
//! hitboxes and cropped RGBA highlight patches align pixel-for-pixel with `mpv`'s native
//! ASS rendering.

use std::ffi::{c_char, c_int, CString};
use std::ptr;

use crate::ffi;
use crate::mkv::MkvFontAttachment;
use crate::parser::{
    build_clean_script_with_parsed, build_probe_script_with_parsed, decode_color_id,
    AssDialogueLine, AssScriptInfo, ParsedChar, ParsedEvent, ParsedExtradata,
};

/// Mirrors `mpv`'s `--sub-ass-override` option.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AssOverrideMode {
    No,
    Yes,
    /// `mpv` default: applies `sub-scale` selectively to dialogue lines (unless `sub_scale_signs`).
    #[default]
    Scale,
    Force,
    Strip,
}

impl AssOverrideMode {
    pub fn from_mpv_str(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "no" => Self::No,
            "yes" => Self::Yes,
            "scale" => Self::Scale,
            "force" => Self::Force,
            "strip" => Self::Strip,
            _ => Self::Scale,
        }
    }
}

/// Mirrors `mpv`'s `--sub-ass-hinting` / `ASS_Hinting`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AssHinting {
    #[default]
    None,
    Light,
    Normal,
    Native,
}

impl AssHinting {
    pub fn from_mpv_str(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "light" => Self::Light,
            "normal" => Self::Normal,
            "native" => Self::Native,
            _ => Self::None,
        }
    }

    fn to_ffi(self) -> c_int {
        match self {
            Self::None => ffi::ASS_HINTING_NONE,
            Self::Light => ffi::ASS_HINTING_LIGHT,
            Self::Normal => ffi::ASS_HINTING_NORMAL,
            Self::Native => ffi::ASS_HINTING_NATIVE,
        }
    }
}

/// Mirrors `mpv`'s `--sub-ass-shaper` / `ASS_ShapingLevel`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AssShapingLevel {
    Simple,
    #[default]
    Complex,
}

impl AssShapingLevel {
    pub fn from_mpv_str(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "simple" => Self::Simple,
            _ => Self::Complex,
        }
    }

    fn to_ffi(self) -> c_int {
        match self {
            Self::Simple => ffi::ASS_SHAPING_SIMPLE,
            Self::Complex => ffi::ASS_SHAPING_COMPLEX,
        }
    }
}

/// Renderer parameters mirrored from `mpv` (`osd-dimensions`, `video-params`, `sub-*`).
#[derive(Debug, Clone, PartialEq)]
pub struct MpvAssParams {
    /// Target OSD/video framebuffer width (`osd-dimensions/w`).
    pub frame_w: u32,
    /// Target OSD/video framebuffer height (`osd-dimensions/h`).
    pub frame_h: u32,
    /// Native video storage width (`video-params/w`; falls back to `frame_w` if `0`).
    pub storage_w: u32,
    /// Native video storage height (`video-params/h`; falls back to `frame_h` if `0`).
    pub storage_h: u32,
    /// OSD margins (`osd-dimensions/{mt,mb,ml,mr}`).
    pub margin_top: i32,
    pub margin_bottom: i32,
    pub margin_left: i32,
    pub margin_right: i32,
    /// Mirrors `sub-use-margins` (default `true`).
    pub use_margins: bool,
    /// Mirrors `sub-scale` (default `1.0`).
    pub sub_scale: f64,
    /// Mirrors `sub-pos` (`0..=150`, default `100.0`, mapped to `ass_set_line_position(100 - sub_pos)`).
    pub sub_pos: f64,
    /// Mirrors `sub-font` (default `"sans-serif"`).
    pub sub_font: String,
    /// Mirrors `sub-ass-override` (default `Scale`).
    pub sub_ass_override: AssOverrideMode,
    /// Mirrors `sub-scale-signs` (default `false`).
    pub sub_scale_signs: bool,
    /// Mirrors `sub-ass-hinting` (default `None`).
    pub hinting: AssHinting,
    /// Mirrors `sub-ass-shaper` (default `Complex`).
    pub shaping: AssShapingLevel,
    /// Pixel aspect ratio (`1.0` for square pixels).
    pub pixel_aspect: f64,
}

impl Default for MpvAssParams {
    fn default() -> Self {
        Self {
            frame_w: 1920,
            frame_h: 1080,
            storage_w: 1920,
            storage_h: 1080,
            margin_top: 0,
            margin_bottom: 0,
            margin_left: 0,
            margin_right: 0,
            use_margins: true,
            sub_scale: 1.0,
            sub_pos: 100.0,
            sub_font: "sans-serif".to_string(),
            sub_ass_override: AssOverrideMode::Scale,
            sub_scale_signs: false,
            hinting: AssHinting::None,
            shaping: AssShapingLevel::Complex,
            pixel_aspect: 1.0,
        }
    }
}

impl MpvAssParams {
    /// Convenience constructor for a 1:1 framebuffer with default `mpv` subtitle parameters.
    pub fn for_frame(frame_w: u32, frame_h: u32) -> Self {
        Self {
            frame_w,
            frame_h,
            storage_w: frame_w,
            storage_h: frame_h,
            ..Self::default()
        }
    }

    fn effective_font_scale(&self) -> f64 {
        match self.sub_ass_override {
            AssOverrideMode::No => 1.0,
            _ => self.sub_scale.max(0.05),
        }
    }

    fn effective_line_position(&self) -> f64 {
        match self.sub_ass_override {
            AssOverrideMode::No => 0.0,
            _ => 100.0 - self.sub_pos,
        }
    }

    fn effective_override_bits(&self) -> c_int {
        match self.sub_ass_override {
            AssOverrideMode::No => ffi::ASS_OVERRIDE_DEFAULT,
            AssOverrideMode::Yes
            | AssOverrideMode::Scale
            | AssOverrideMode::Force
            | AssOverrideMode::Strip => {
                if self.sub_scale_signs {
                    ffi::ASS_OVERRIDE_DEFAULT
                } else {
                    ffi::ASS_OVERRIDE_BIT_SELECTIVE_FONT_SCALE
                }
            }
        }
    }
}

/// Exact `image-fit: contain` coordinate mapping between a Slint `video-area` in logical
/// coordinates and `libass` framebuffer pixels (`frame_w × frame_h`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ViewportTransform {
    pub area_w: f32,
    pub area_h: f32,
    pub frame_w: u32,
    pub frame_h: u32,
    pub offset_x: f32,
    pub offset_y: f32,
    /// Logical UI pixels per framebuffer pixel.
    pub scale: f32,
}

impl ViewportTransform {
    pub fn contain(area_w: f32, area_h: f32, frame_w: u32, frame_h: u32) -> Self {
        if area_w <= 0.0 || area_h <= 0.0 || frame_w == 0 || frame_h == 0 {
            return Self {
                area_w,
                area_h,
                frame_w,
                frame_h,
                offset_x: 0.0,
                offset_y: 0.0,
                scale: 1.0,
            };
        }
        let fw = frame_w as f32;
        let fh = frame_h as f32;
        let scale = (area_w / fw).min(area_h / fh);
        let offset_x = (area_w - fw * scale) * 0.5;
        let offset_y = (area_h - fh * scale) * 0.5;
        Self {
            area_w,
            area_h,
            frame_w,
            frame_h,
            offset_x,
            offset_y,
            scale,
        }
    }

    /// Map a pointer coordinate in `video-area` logical pixels into framebuffer pixels.
    /// Returns `None` if the pointer is outside the letterboxed video frame.
    #[inline]
    pub fn logical_to_frame(&self, lx: f32, ly: f32) -> Option<(f32, f32)> {
        if self.scale <= 0.0 || self.frame_w == 0 || self.frame_h == 0 {
            return None;
        }
        let fx = (lx - self.offset_x) / self.scale;
        let fy = (ly - self.offset_y) / self.scale;
        if fx >= 0.0 && fy >= 0.0 && fx <= self.frame_w as f32 && fy <= self.frame_h as f32 {
            Some((fx, fy))
        } else {
            None
        }
    }

    /// Map a framebuffer pixel point into `video-area` logical pixels.
    #[inline]
    pub fn frame_to_logical(&self, fx: f32, fy: f32) -> (f32, f32) {
        (self.offset_x + fx * self.scale, self.offset_y + fy * self.scale)
    }

    /// Map a framebuffer pixel rectangle into `video-area` logical pixels `(x, y, w, h)`.
    #[inline]
    pub fn frame_rect_to_logical(&self, rect: AssHighlightRect) -> (f32, f32, f32, f32) {
        (
            self.offset_x + rect.x * self.scale,
            self.offset_y + rect.y * self.scale,
            rect.w * self.scale,
            rect.h * self.scale,
        )
    }
}

/// Axis-aligned rectangle in `libass` framebuffer pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AssHighlightRect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl AssHighlightRect {
    pub fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }

    #[inline]
    pub fn right(&self) -> f32 {
        self.x + self.w
    }

    #[inline]
    pub fn bottom(&self) -> f32 {
        self.y + self.h
    }

    #[inline]
    pub fn contains(&self, px: f32, py: f32) -> bool {
        px >= self.x && px <= self.right() && py >= self.y && py <= self.bottom()
    }

    pub fn union(self, other: Self) -> Self {
        let x0 = self.x.min(other.x);
        let y0 = self.y.min(other.y);
        let x1 = self.right().max(other.right());
        let y1 = self.bottom().max(other.bottom());
        Self {
            x: x0,
            y: y0,
            w: (x1 - x0).max(0.0),
            h: (y1 - y0).max(0.0),
        }
    }

    /// Expand by `pad_x` / `pad_y` and clamp to `[0..frame_w, 0..frame_h]`.
    pub fn padded_clamped(self, pad_x: f32, pad_y: f32, frame_w: u32, frame_h: u32) -> Self {
        let x0 = (self.x - pad_x).max(0.0);
        let y0 = (self.y - pad_y).max(0.0);
        let x1 = (self.right() + pad_x).min(frame_w as f32);
        let y1 = (self.bottom() + pad_y).min(frame_h as f32);
        Self {
            x: x0,
            y: y0,
            w: (x1 - x0).max(0.0),
            h: (y1 - y0).max(0.0),
        }
    }
}

/// Screen-space geometry and text offsets of a single character in an active ASS event.
#[derive(Debug, Clone, PartialEq)]
pub struct AssCharBox {
    pub ch: char,
    /// UTF-8 byte offset within the parent [`AssActiveEvent::plain_text`].
    pub byte_offset: usize,
    /// UTF-16 code unit offset within [`AssActiveEvent::plain_text`].
    pub utf16_offset: usize,
    /// UTF-16 code unit length (`1` or `2`).
    pub utf16_len: usize,
    /// Tight inked glyph bounding box in framebuffer pixels (or interpolated for spaces).
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    /// Continuous horizontal hit interval `[hit_x0, hit_x1]` bridging inter-glyph `Spacing`.
    pub hit_x0: f32,
    pub hit_x1: f32,
    /// Visual line index inside [`AssActiveEvent::lines`].
    pub line_index: usize,
    /// True if this character had actual inked pixels in the `libass` probe.
    pub has_ink: bool,
    pub is_whitespace: bool,
}

/// One horizontal visual line within an [`AssActiveEvent`] (split by `\N` or `libass` auto-wrap).
#[derive(Debug, Clone, PartialEq)]
pub struct AssVisualLine {
    pub line_index: usize,
    /// Range `start..end` into [`AssActiveEvent::chars`].
    pub char_start: usize,
    pub char_end: usize,
    /// Tight inked bounds of the line in framebuffer pixels.
    pub ink_rect: AssHighlightRect,
    /// Ergonomically padded hit-test bounds for hover/click detection.
    pub hitbox: AssHighlightRect,
    /// Uniform line top (`ink_rect.y`) and height (`ink_rect.h`) for crisp highlight bars.
    pub baseline_top: f32,
    pub line_height: f32,
}

/// Complete layout & text representation of one active ASS `Dialogue:` event.
#[derive(Debug, Clone, PartialEq)]
pub struct AssActiveEvent {
    pub event_index: usize,
    pub dialogue: AssDialogueLine,
    pub plain_text: String,
    /// Byte offset of `plain_text` inside [`AssFrameLayout::combined_plain_text`].
    pub global_byte_offset: usize,
    /// UTF-16 code unit offset of `plain_text` inside [`AssFrameLayout::combined_plain_text`].
    pub global_utf16_offset: usize,
    pub chars: Vec<AssCharBox>,
    pub lines: Vec<AssVisualLine>,
    /// Union bounding box of all visual lines in this event (if any).
    pub bounds: Option<AssHighlightRect>,
    pub is_drawing_only: bool,
    pub has_animated_geometry: bool,
    pub is_rotated_or_sign: bool,
    pub has_explicit_pos: bool,
}

/// Result of hit-testing a framebuffer coordinate `(fx, fy)` against [`AssFrameLayout`].
#[derive(Debug, Clone, PartialEq)]
pub struct AssHitTestResult {
    pub event_index: usize,
    pub line_index: usize,
    pub char_index: usize,
    pub ch: char,
    pub event_byte_offset: usize,
    pub event_utf16_offset: usize,
    pub global_byte_offset: usize,
    pub global_utf16_offset: usize,
    /// Substring of `event.plain_text` starting at the hit character for dictionary scanning.
    pub scan_substring: String,
    pub char_box: AssCharBox,
    pub line_rect: AssHighlightRect,
}

/// Complete per-frame hitbox layout returned by [`AssLayoutEngine::probe_active_events`].
#[derive(Debug, Clone)]
pub struct AssFrameLayout {
    pub frame_w: u32,
    pub frame_h: u32,
    pub now_ms: i64,
    pub script_info: AssScriptInfo,
    pub events: Vec<AssActiveEvent>,
    /// All non-empty active event texts joined by `\n` (for clipboard/Anki `{subtitle}`).
    pub combined_plain_text: String,
    /// True if any active event has `\move(...)` or geometry-changing `\t(...)` requiring
    /// periodic re-probing while playing.
    pub has_animated_geometry: bool,
    /// Precomputed topmost-first event indices (higher `Layer`, then later script order).
    hit_order: Vec<usize>,
}

impl AssFrameLayout {
    /// Hit-test a coordinate in **framebuffer pixels** (`0..frame_w`, `0..frame_h`) in O(log N)
    /// per overlapping visual line, respecting ASS `Layer` + script draw order (topmost first).
    pub fn hit_test(&self, fx: f32, fy: f32) -> Option<AssHitTestResult> {
        for &ev_idx in &self.hit_order {
            let Some(ev) = self.events.get(ev_idx) else {
                continue;
            };
            if ev.is_drawing_only || ev.lines.is_empty() {
                continue;
            }
            for line in &ev.lines {
                if !line.hitbox.contains(fx, fy) {
                    continue;
                }
                if let Some(hit) = self.hit_test_line(ev, line, fx) {
                    return Some(hit);
                }
            }
        }
        None
    }

    /// Hit-test a pointer coordinate given in Slint `video-area` logical pixels.
    pub fn hit_test_logical(
        &self,
        transform: &ViewportTransform,
        lx: f32,
        ly: f32,
    ) -> Option<AssHitTestResult> {
        let (fx, fy) = transform.logical_to_frame(lx, ly)?;
        self.hit_test(fx, fy)
    }

    fn hit_test_line(
        &self,
        ev: &AssActiveEvent,
        line: &AssVisualLine,
        fx: f32,
    ) -> Option<AssHitTestResult> {
        let slice = &ev.chars[line.char_start..line.char_end];
        if slice.is_empty() {
            return None;
        }

        // Visual lines are strictly non-decreasing along `hit_x1`: binary search in O(log N).
        let rel_idx = slice.partition_point(|c| c.hit_x1 < fx);
        let rel_idx = rel_idx.min(slice.len() - 1);
        let char_index = line.char_start + rel_idx;
        let c = &ev.chars[char_index];

        let scan_substring = ev
            .plain_text
            .get(c.byte_offset..)
            .unwrap_or("")
            .to_string();

        Some(AssHitTestResult {
            event_index: ev.event_index,
            line_index: line.line_index,
            char_index,
            ch: c.ch,
            event_byte_offset: c.byte_offset,
            event_utf16_offset: c.utf16_offset,
            global_byte_offset: ev.global_byte_offset + c.byte_offset,
            global_utf16_offset: ev.global_utf16_offset + c.utf16_offset,
            scan_substring,
            char_box: c.clone(),
            line_rect: line.ink_rect,
        })
    }

    /// Compute continuous solid highlight bar rectangles (one per visual line covered) for a
    /// UTF-16 code unit range `[utf16_start, utf16_end)` inside `event_index`.
    ///
    /// Highlight bars use each visual line's uniform `baseline_top` and `line_height` with
    /// tight aesthetic padding so adjacent kanji/kana/punctuation form a single continuous
    /// selection rectangle with zero inter-character gaps.
    pub fn highlight_rects_for_utf16_span(
        &self,
        event_index: usize,
        utf16_start: usize,
        utf16_end: usize,
    ) -> Vec<AssHighlightRect> {
        if utf16_end <= utf16_start {
            return Vec::new();
        }
        let Some(ev) = self.events.get(event_index) else {
            return Vec::new();
        };

        let mut rects = Vec::new();
        for line in &ev.lines {
            let mut min_x = f32::INFINITY;
            let mut max_x = f32::NEG_INFINITY;
            let mut matched_any = false;

            for c in &ev.chars[line.char_start..line.char_end] {
                let c_end = c.utf16_offset + c.utf16_len;
                if c_end > utf16_start && c.utf16_offset < utf16_end {
                    min_x = min_x.min(c.x);
                    max_x = max_x.max(c.x + c.w);
                    matched_any = true;
                }
            }

            if matched_any && max_x > min_x {
                let pad_x = (line.line_height * 0.06).clamp(2.0, 6.0);
                let pad_y = (line.line_height * 0.08).clamp(2.0, 8.0);
                let raw = AssHighlightRect::new(
                    min_x,
                    line.baseline_top,
                    max_x - min_x,
                    line.line_height,
                );
                rects.push(raw.padded_clamped(pad_x, pad_y, self.frame_w, self.frame_h));
            }
        }
        rects
    }

    /// Convenience variant taking a UTF-8 byte range `[byte_start, byte_end)` inside `event_index`.
    ///
    /// Non-char-boundary indices are safely snapped (`byte_start` downward, `byte_end` upward)
    /// so malformed byte offsets never panic.
    pub fn highlight_rects_for_byte_span(
        &self,
        event_index: usize,
        byte_start: usize,
        byte_end: usize,
    ) -> Vec<AssHighlightRect> {
        let Some(ev) = self.events.get(event_index) else {
            return Vec::new();
        };
        let bs = floor_char_boundary(&ev.plain_text, byte_start);
        let be = ceil_char_boundary(&ev.plain_text, byte_end);
        if be <= bs {
            return Vec::new();
        }
        let utf16_start = ev.plain_text[..bs].encode_utf16().count();
        let utf16_end = utf16_start + ev.plain_text[bs..be].encode_utf16().count();
        self.highlight_rects_for_utf16_span(event_index, utf16_start, utf16_end)
    }

    /// Tight union bounding box of all active events' inked lines in framebuffer pixels.
    pub fn overall_ink_bounds(&self) -> Option<AssHighlightRect> {
        let mut acc: Option<AssHighlightRect> = None;
        for ev in &self.events {
            if let Some(b) = ev.bounds {
                acc = Some(match acc {
                    Some(prev) => prev.union(b),
                    None => b,
                });
            }
        }
        acc
    }
}

/// A cropped RGBA8 subtitle bitmap patch rendered by `libass` (straight alpha RGBA8).
///
/// Positioned at `(x, y)` in framebuffer pixels (`width × height`). Intended to be
/// composited above Subomi's `#38a6d6` selection rectangle so crisp white ASS glyphs
/// and black outlines sit cleanly on top of the solid highlight bar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RgbaPatch {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    /// Straight-alpha RGBA8 pixels (`width * height * 4` bytes).
    pub rgba: Vec<u8>,
    /// True if at least one pixel in `rgba` has non-zero alpha.
    pub has_visible_pixels: bool,
}

impl RgbaPatch {
    /// Convert straight RGBA8 pixels into premultiplied RGBA8 (e.g. for Slint
    /// `Image::from_rgba8_premultiplied`).
    pub fn to_premultiplied_rgba8(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.rgba.len());
        for px in self.rgba.chunks_exact(4) {
            let a = px[3] as u32;
            out.push(((px[0] as u32 * a + 127) / 255) as u8);
            out.push(((px[1] as u32 * a + 127) / 255) as u8);
            out.push(((px[2] as u32 * a + 127) / 255) as u8);
            out.push(px[3]);
        }
        out
    }

    /// Alpha-composite this patch over an opaque RGB8 framebuffer (`frame_w × frame_h × 3`)
    /// at `(self.x, self.y)`. Useful for visual verification and offline frame rendering.
    pub fn composite_over_rgb8(&self, frame_rgb: &mut [u8], frame_w: u32, frame_h: u32) {
        for py in 0..self.height {
            let fy = self.y + py;
            if fy >= frame_h {
                break;
            }
            for px in 0..self.width {
                let fx = self.x + px;
                if fx >= frame_w {
                    break;
                }
                let src_idx = ((py * self.width + px) * 4) as usize;
                let a = self.rgba[src_idx + 3] as u32;
                if a == 0 {
                    continue;
                }
                let dst_idx = ((fy * frame_w + fx) * 3) as usize;
                if dst_idx + 2 >= frame_rgb.len() {
                    continue;
                }
                let inv_a = 255 - a;
                frame_rgb[dst_idx] =
                    ((self.rgba[src_idx] as u32 * a + frame_rgb[dst_idx] as u32 * inv_a + 127)
                        / 255) as u8;
                frame_rgb[dst_idx + 1] =
                    ((self.rgba[src_idx + 1] as u32 * a
                        + frame_rgb[dst_idx + 1] as u32 * inv_a
                        + 127)
                        / 255) as u8;
                frame_rgb[dst_idx + 2] =
                    ((self.rgba[src_idx + 2] as u32 * a
                        + frame_rgb[dst_idx + 2] as u32 * inv_a
                        + 127)
                        / 255) as u8;
            }
        }
    }
}

struct CachedProbe {
    ass_full: String,
    clean_ass: String,
    valid_start_ms: i64,
    valid_end_ms: i64,
    script_info: AssScriptInfo,
    events: Vec<ParsedEvent>,
    total_probed_chars: usize,
    color_id_map_len: usize,
    probe_track: *mut ffi::ASS_Track,
}

/// Owns a `libass` `ASS_Library` + `ASS_Renderer` + cached clean/probe `ASS_Track`s for
/// high-speed hitbox probing and highlight-patch rendering.
pub struct AssLayoutEngine {
    library: *mut ffi::ASS_Library,
    renderer: *mut ffi::ASS_Renderer,
    params: MpvAssParams,
    configured: bool,
    fonts_initialized: bool,
    loaded_font_count: usize,
    cached_extradata_raw: String,
    cached_extradata: Option<ParsedExtradata>,
    cached_clean_ass: String,
    cached_clean_track: *mut ffi::ASS_Track,
    cached_probe: Option<CachedProbe>,
    clean_matches_cached_probe: bool,
    scratch_accum: Vec<f32>,
}

// SAFETY: `AssLayoutEngine` exclusively owns its `ASS_Library`, `ASS_Renderer`, and
// `ASS_Track` pointers with no shared aliasing. `libass` instances may be moved across
// threads as long as concurrent calls are not made (hence `Send`, not `Sync`).
unsafe impl Send for AssLayoutEngine {}

impl AssLayoutEngine {
    /// Initialize a new `libass` library and renderer with default `mpv`-matching settings.
    pub fn new() -> Result<Self, String> {
        unsafe {
            let library = ffi::ass_library_init();
            if library.is_null() {
                return Err("ass_library_init() returned NULL".to_string());
            }
            ffi::subomi_ass_silence_library(library);
            ffi::ass_set_extract_fonts(library, 1);

            let renderer = ffi::ass_renderer_init(library);
            if renderer.is_null() {
                ffi::ass_library_done(library);
                return Err("ass_renderer_init() returned NULL".to_string());
            }
            // Keep reasonable cache limits so frequent hover patch renders stay hot.
            ffi::ass_set_cache_limits(renderer, 4096, 64);

            let mut engine = Self {
                library,
                renderer,
                params: MpvAssParams::default(),
                configured: false,
                fonts_initialized: false,
                loaded_font_count: 0,
                cached_extradata_raw: String::new(),
                cached_extradata: None,
                cached_clean_ass: String::new(),
                cached_clean_track: ptr::null_mut(),
                cached_probe: None,
                clean_matches_cached_probe: false,
                scratch_accum: Vec::new(),
            };
            engine.apply_params_unconditional();
            Ok(engine)
        }
    }

    /// Number of embedded memory fonts currently registered with `ass_add_font`.
    pub fn loaded_font_count(&self) -> usize {
        self.loaded_font_count
    }

    /// Current renderer parameters.
    pub fn params(&self) -> &MpvAssParams {
        &self.params
    }

    /// Replace all in-memory embedded fonts (e.g. extracted from an MKV container).
    ///
    /// Strictly respects `libass`'s `ass_clear_fonts` lifetime contract: all cached tracks
    /// and the active `ASS_Renderer` are destroyed before calling `ass_clear_fonts`, then
    /// rebuilt and reconfigured after registering the new font payloads.
    pub fn load_memory_fonts(&mut self, fonts: &[MkvFontAttachment]) -> Result<(), String> {
        unsafe {
            self.clear_cached_tracks();
            if self.scratch_accum.capacity() > 1024 * 1024 {
                self.scratch_accum = Vec::new();
            }
            if !self.renderer.is_null() {
                ffi::ass_renderer_done(self.renderer);
                self.renderer = ptr::null_mut();
            }
            self.configured = false;
            self.fonts_initialized = false;
            ffi::ass_clear_fonts(self.library);
            self.loaded_font_count = 0;

            for font in fonts {
                if font.data.is_empty() || font.data.len() > i32::MAX as usize {
                    continue;
                }
                let sanitized_name: String = font
                    .file_name
                    .chars()
                    .map(|c| if c == '\0' { '_' } else { c })
                    .collect();
                let c_name = CString::new(sanitized_name)
                    .unwrap_or_else(|_| CString::new("embedded_font.ttf").unwrap());
                ffi::ass_add_font(
                    self.library,
                    c_name.as_ptr(),
                    font.data.as_ptr() as *const c_char,
                    font.data.len() as c_int,
                );
                self.loaded_font_count += 1;
            }

            let renderer = ffi::ass_renderer_init(self.library);
            if renderer.is_null() {
                return Err("ass_renderer_init() failed after load_memory_fonts".to_string());
            }
            ffi::ass_set_cache_limits(renderer, 4096, 64);
            self.renderer = renderer;
            self.apply_params_unconditional();
        }
        Ok(())
    }

    /// Update `libass` renderer configuration if `params` differs from the active state.
    pub fn configure(&mut self, params: &MpvAssParams) {
        if self.configured && self.params == *params {
            return;
        }
        let font_changed = !self.fonts_initialized || self.params.sub_font != params.sub_font;
        self.params = params.clone();
        self.apply_params_internal(font_changed);
    }

    fn apply_params_unconditional(&mut self) {
        self.apply_params_internal(true);
    }

    fn apply_params_internal(&mut self, reinit_fonts: bool) {
        if self.renderer.is_null() {
            return;
        }
        let p = &self.params;
        let fw = p.frame_w.max(1) as c_int;
        let fh = p.frame_h.max(1) as c_int;
        let sw = if p.storage_w > 0 { p.storage_w } else { p.frame_w.max(1) } as c_int;
        let sh = if p.storage_h > 0 { p.storage_h } else { p.frame_h.max(1) } as c_int;

        unsafe {
            ffi::ass_set_frame_size(self.renderer, fw, fh);
            ffi::ass_set_storage_size(self.renderer, sw, sh);
            ffi::ass_set_margins(
                self.renderer,
                p.margin_top,
                p.margin_bottom,
                p.margin_left,
                p.margin_right,
            );
            ffi::ass_set_use_margins(self.renderer, i32::from(p.use_margins));
            ffi::ass_set_pixel_aspect(self.renderer, p.pixel_aspect);
            ffi::ass_set_font_scale(self.renderer, p.effective_font_scale());
            ffi::ass_set_line_position(self.renderer, p.effective_line_position());
            ffi::ass_set_hinting(self.renderer, p.hinting.to_ffi());
            ffi::ass_set_shaper(self.renderer, p.shaping.to_ffi());
            ffi::ass_set_selective_style_override_enabled(
                self.renderer,
                p.effective_override_bits(),
            );

            if reinit_fonts {
                let family = if p.sub_font.trim().is_empty() {
                    "sans-serif"
                } else {
                    p.sub_font.trim()
                };
                let c_family = CString::new(family)
                    .unwrap_or_else(|_| CString::new("sans-serif").unwrap());
                ffi::ass_set_fonts(
                    self.renderer,
                    ptr::null(),
                    c_family.as_ptr(),
                    ffi::ASS_FONTPROVIDER_AUTODETECT,
                    ptr::null(),
                    1,
                );
                self.fonts_initialized = true;
            }
        }
        self.configured = true;
    }

    fn clear_cached_clean_track(&mut self) {
        if !self.cached_clean_track.is_null() {
            unsafe {
                ffi::ass_free_track(self.cached_clean_track);
            }
            self.cached_clean_track = ptr::null_mut();
        }
        self.cached_clean_ass.clear();
        self.clean_matches_cached_probe = false;
    }

    fn clear_cached_probe(&mut self) {
        if let Some(cp) = self.cached_probe.take() {
            if !cp.probe_track.is_null() {
                unsafe {
                    ffi::ass_free_track(cp.probe_track);
                }
            }
        }
        self.clean_matches_cached_probe = false;
    }

    fn clear_cached_tracks(&mut self) {
        self.clear_cached_probe();
        self.clear_cached_clean_track();
    }

    fn ensure_parsed_extradata(&mut self, extradata: &str) -> &ParsedExtradata {
        if self.cached_extradata.is_none() || self.cached_extradata_raw != extradata {
            self.clear_cached_probe();
            self.cached_extradata_raw.clear();
            self.cached_extradata_raw.push_str(extradata);
            self.cached_extradata = Some(ParsedExtradata::parse(extradata));
        }
        self.cached_extradata.as_ref().unwrap()
    }

    fn ensure_clean_track(&mut self, clean_ass: &str) -> Result<*mut ffi::ASS_Track, String> {
        if !self.cached_clean_track.is_null() && self.cached_clean_ass == clean_ass {
            return Ok(self.cached_clean_track);
        }
        self.clear_cached_clean_track();
        let track = self.read_track_from_str(clean_ass)?;
        self.cached_clean_ass = clean_ass.to_string();
        self.cached_clean_track = track;
        Ok(track)
    }

    fn read_track_from_str(&self, ass_text: &str) -> Result<*mut ffi::ASS_Track, String> {
        if ass_text.is_empty() {
            return Err("ass_read_memory() called with empty ASS script".to_string());
        }
        let mut bytes = ass_text.as_bytes().to_vec();
        let track = unsafe {
            ffi::ass_read_memory(
                self.library,
                bytes.as_mut_ptr() as *mut c_char,
                bytes.len(),
                ptr::null(),
            )
        };
        if track.is_null() {
            Err("ass_read_memory() returned NULL".to_string())
        } else {
            Ok(track)
        }
    }

    /// Run a single color-ID-instrumented `ass_render_frame` pass to compute exact
    /// per-event / per-character screen-space hitboxes, AND cache the matching clean
    /// `ASS_Track` (and probe `ASS_Track` for animated re-probes) so subsequent hover
    /// [`render_highlight_patch`] and animated re-probe calls stay fast.
    ///
    /// - `extradata`: ASS header (`[Script Info]`, `[V4+ Styles]`, optional `[Events]`) or full `.ass`.
    /// - `ass_full`: Active `Dialogue:` lines (e.g. from `mpv`'s `sub-text/ass-full`). Pass `""`
    ///   when `extradata` is already a full `.ass` file to extract events active at `now_ms`.
    /// - `now_ms`: Current playback time in milliseconds.
    pub fn probe_active_events(
        &mut self,
        extradata: &str,
        ass_full: &str,
        now_ms: i64,
    ) -> Result<AssFrameLayout, String> {
        if self.renderer.is_null() {
            return Err("libass renderer is not initialized".to_string());
        }
        let now = now_ms.max(0);
        let _ = self.ensure_parsed_extradata(extradata);

        let cache_hit = self.cached_probe.as_ref().is_some_and(|cp| {
            now >= cp.valid_start_ms
                && now < cp.valid_end_ms
                && cp.ass_full == ass_full
        });

        if !cache_hit {
            self.clear_cached_probe();
            let parsed = self.cached_extradata.as_ref().unwrap();
            let probe = build_probe_script_with_parsed(parsed, extradata, ass_full, now_ms);

            // Cache the clean track eagerly (fast & keeps subsequent highlight renders lock-free).
            let _ = self.ensure_clean_track(&probe.clean_ass);

            let probe_track = if probe.total_probed_chars > 0 {
                self.read_track_from_str(&probe.probe_ass)?
            } else {
                ptr::null_mut()
            };

            let (valid_start_ms, valid_end_ms) = if ass_full.trim().is_empty() {
                (now, now.saturating_add(1))
            } else {
                let mut vs = 0i64;
                let mut ve = i64::MAX;
                for ev in &probe.events {
                    if ev.dialogue.start_ms <= now && now < ev.dialogue.end_ms {
                        vs = vs.max(ev.dialogue.start_ms.max(0));
                        ve = ve.min(ev.dialogue.end_ms);
                    } else {
                        vs = now;
                        ve = now.saturating_add(1);
                        break;
                    }
                }
                (vs, ve)
            };

            self.cached_probe = Some(CachedProbe {
                ass_full: ass_full.to_string(),
                clean_ass: probe.clean_ass,
                valid_start_ms,
                valid_end_ms,
                script_info: probe.script_info,
                events: probe.events,
                total_probed_chars: probe.total_probed_chars,
                color_id_map_len: probe.color_id_map.len(),
                probe_track,
            });
            self.clean_matches_cached_probe = !self.cached_clean_track.is_null();
        } else if !self.clean_matches_cached_probe || self.cached_clean_track.is_null() {
            let clean_ass = self.cached_probe.as_ref().unwrap().clean_ass.clone();
            let _ = self.ensure_clean_track(&clean_ass);
            self.clean_matches_cached_probe = !self.cached_clean_track.is_null();
        }

        let cp = self.cached_probe.as_ref().unwrap();
        let mut raw_bboxes: Vec<Option<PixelRect>> = vec![None; cp.color_id_map_len];

        if cp.total_probed_chars > 0 && !cp.probe_track.is_null() {
            let mut change: c_int = 0;
            unsafe {
                let mut img = ffi::ass_render_frame(
                    self.renderer,
                    cp.probe_track,
                    now as libc::c_longlong,
                    &mut change,
                );
                while !img.is_null() {
                    let node = &*img;
                    if node.w > 0
                        && node.h > 0
                        && !node.bitmap.is_null()
                        && node.kind == ffi::IMAGE_TYPE_CHARACTER
                        && (node.color & 0xFF) != 0xFF
                    {
                        let cid = decode_color_id(node.color);
                        if cid >= 1 && (cid as usize) <= raw_bboxes.len() {
                            let idx = (cid - 1) as usize;
                            let rect = PixelRect {
                                min_x: node.dst_x,
                                min_y: node.dst_y,
                                max_x: node.dst_x.saturating_add(node.w),
                                max_y: node.dst_y.saturating_add(node.h),
                            };
                            raw_bboxes[idx] = Some(match raw_bboxes[idx] {
                                Some(prev) => prev.union(rect),
                                None => rect,
                            });
                        }
                    }
                    img = node.next;
                }
            }
        }

        let mut combined_plain_text = String::new();
        let mut global_utf16_cursor: usize = 0;
        let mut active_events = Vec::with_capacity(cp.events.len());
        let mut any_animated = false;

        for pe in &cp.events {
            if pe.has_animated_geometry {
                any_animated = true;
            }
            if !pe.plain_text.is_empty() && !combined_plain_text.is_empty() {
                combined_plain_text.push('\n');
                global_utf16_cursor += 1;
            }
            let global_byte_offset = combined_plain_text.len();
            let global_utf16_offset = global_utf16_cursor;
            if !pe.plain_text.is_empty() {
                combined_plain_text.push_str(&pe.plain_text);
                global_utf16_cursor += pe.plain_text.encode_utf16().count();
            }

            let ev = build_active_event(
                pe,
                &raw_bboxes,
                global_byte_offset,
                global_utf16_offset,
                self.params.frame_w,
                self.params.frame_h,
            );
            active_events.push(ev);
        }

        let mut hit_order: Vec<usize> = (0..active_events.len()).collect();
        // Higher Layer renders on top; on equal Layer, later script order renders on top.
        hit_order.sort_by(|&a, &b| {
            let ea = &active_events[a];
            let eb = &active_events[b];
            eb.dialogue
                .layer
                .cmp(&ea.dialogue.layer)
                .then_with(|| eb.event_index.cmp(&ea.event_index))
        });

        Ok(AssFrameLayout {
            frame_w: self.params.frame_w,
            frame_h: self.params.frame_h,
            now_ms: now,
            script_info: cp.script_info.clone(),
            events: active_events,
            combined_plain_text,
            has_animated_geometry: any_animated,
            hit_order,
        })
    }

    /// Render a tight cropped straight-RGBA8 patch of the currently cached clean ASS track
    /// (loaded by the most recent [`probe_active_events`]) inside `crop_rect`.
    ///
    /// Designed for compositing crisp ASS glyphs + outlines above Subomi's solid `#38a6d6`
    /// hover/lookup highlight bar.
    pub fn render_highlight_patch(
        &mut self,
        crop_rect: AssHighlightRect,
        now_ms: i64,
    ) -> Result<RgbaPatch, String> {
        if self.renderer.is_null() {
            return Err("libass renderer is not initialized".to_string());
        }
        if self.cached_clean_track.is_null() {
            return Err(
                "render_highlight_patch() called before probe_active_events() cached a track"
                    .to_string(),
            );
        }
        let track = self.cached_clean_track;
        Ok(self.render_track_patch(track, crop_rect, now_ms.max(0)))
    }

    /// Render a cropped straight-RGBA8 patch directly from `(extradata, ass_full)` (reusing the
    /// cached parsed extradata and clean track whenever unchanged, without building a probe script).
    pub fn render_clean_patch(
        &mut self,
        extradata: &str,
        ass_full: &str,
        crop_rect: AssHighlightRect,
        now_ms: i64,
    ) -> Result<RgbaPatch, String> {
        if self.renderer.is_null() {
            return Err("libass renderer is not initialized".to_string());
        }
        let now = now_ms.max(0);
        let clean_ass = {
            let parsed = self.ensure_parsed_extradata(extradata);
            build_clean_script_with_parsed(parsed, extradata, ass_full, now_ms)
        };
        let track = self.ensure_clean_track(&clean_ass)?;
        Ok(self.render_track_patch(track, crop_rect, now))
    }

    /// Render the full framebuffer (`frame_w × frame_h`) of `(extradata, ass_full)` as straight
    /// RGBA8. Primarily useful for visual parity verification and fallback software rendering.
    pub fn render_full_frame_rgba(
        &mut self,
        extradata: &str,
        ass_full: &str,
        now_ms: i64,
    ) -> Result<RgbaPatch, String> {
        let full = AssHighlightRect::new(
            0.0,
            0.0,
            self.params.frame_w as f32,
            self.params.frame_h as f32,
        );
        self.render_clean_patch(extradata, ass_full, full, now_ms)
    }

    fn render_track_patch(
        &mut self,
        track: *mut ffi::ASS_Track,
        crop_rect: AssHighlightRect,
        now_ms: i64,
    ) -> RgbaPatch {
        let fw = self.params.frame_w as i32;
        let fh = self.params.frame_h as i32;

        let x0 = (crop_rect.x.floor() as i32).clamp(0, fw);
        let y0 = (crop_rect.y.floor() as i32).clamp(0, fh);
        let x1 = (crop_rect.right().ceil() as i32).clamp(x0, fw);
        let y1 = (crop_rect.bottom().ceil() as i32).clamp(y0, fh);

        let w = (x1 - x0) as u32;
        let h = (y1 - y0) as u32;
        let Some(pixel_count) = (w as usize).checked_mul(h as usize) else {
            return RgbaPatch {
                x: x0 as u32,
                y: y0 as u32,
                width: 0,
                height: 0,
                rgba: Vec::new(),
                has_visible_pixels: false,
            };
        };
        let Some(accum_len) = pixel_count.checked_mul(4) else {
            return RgbaPatch {
                x: x0 as u32,
                y: y0 as u32,
                width: 0,
                height: 0,
                rgba: Vec::new(),
                has_visible_pixels: false,
            };
        };
        if self.renderer.is_null() || w == 0 || h == 0 {
            return RgbaPatch {
                x: x0 as u32,
                y: y0 as u32,
                width: 0,
                height: 0,
                rgba: Vec::new(),
                has_visible_pixels: false,
            };
        }

        let mut change: c_int = 0;
        let first_img = unsafe {
            ffi::ass_render_frame(
                self.renderer,
                track,
                now_ms as libc::c_longlong,
                &mut change,
            )
        };
        if first_img.is_null() {
            return RgbaPatch {
                x: x0 as u32,
                y: y0 as u32,
                width: w,
                height: h,
                rgba: vec![0u8; accum_len],
                has_visible_pixels: false,
            };
        }

        // Accumulate in premultiplied f32 RGBA using the engine's reusable scratch buffer
        // for exact associative Porter-Duff SRC_OVER across shadow -> outline -> character layers.
        if self.scratch_accum.len() < accum_len {
            self.scratch_accum.resize(accum_len, 0.0);
        }
        let accum = &mut self.scratch_accum[..accum_len];
        accum.fill(0.0);
        let mut touched = false;

        unsafe {
            let mut img = first_img;
            while !img.is_null() {
                let node = &*img;
                if node.w > 0 && node.h > 0 && node.stride >= node.w && !node.bitmap.is_null() {
                    let col_alpha = 255 - (node.color & 0xFF);
                    if col_alpha > 0 {
                        let ix0 = node.dst_x.max(x0);
                        let iy0 = node.dst_y.max(y0);
                        let ix1 = node.dst_x.saturating_add(node.w).min(x1);
                        let iy1 = node.dst_y.saturating_add(node.h).min(y1);

                        if ix0 < ix1 && iy0 < iy1 {
                            let sr = ((node.color >> 24) & 0xFF) as f32 / 255.0;
                            let sg = ((node.color >> 16) & 0xFF) as f32 / 255.0;
                            let sb = ((node.color >> 8) & 0xFF) as f32 / 255.0;
                            let ca = col_alpha as f32 / 255.0;
                            let stride = node.stride as isize;

                            for fy in iy0..iy1 {
                                let src_row = (fy - node.dst_y) as isize * stride;
                                let dst_row = ((fy - y0) as usize) * (w as usize);
                                for fx in ix0..ix1 {
                                    let k = *node.bitmap.offset(src_row + (fx - node.dst_x) as isize);
                                    if k == 0 {
                                        continue;
                                    }
                                    touched = true;
                                    let sa = (k as f32 / 255.0) * ca;
                                    let inv_sa = 1.0 - sa;
                                    let off = (dst_row + (fx - x0) as usize) * 4;
                                    accum[off] = sr * sa + accum[off] * inv_sa;
                                    accum[off + 1] = sg * sa + accum[off + 1] * inv_sa;
                                    accum[off + 2] = sb * sa + accum[off + 2] * inv_sa;
                                    accum[off + 3] = sa + accum[off + 3] * inv_sa;
                                }
                            }
                        }
                    }
                }
                img = node.next;
            }
        }

        let mut rgba = vec![0u8; accum_len];
        let mut has_visible_pixels = false;
        if touched {
            for i in 0..pixel_count {
                let off = i * 4;
                let pa = accum[off + 3];
                if pa <= 0.0 {
                    continue;
                }
                let a_u8 = (pa.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
                if a_u8 == 0 {
                    continue;
                }
                has_visible_pixels = true;
                let inv_pa = 1.0 / pa;
                rgba[off] = ((accum[off] * inv_pa).clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
                rgba[off + 1] = ((accum[off + 1] * inv_pa).clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
                rgba[off + 2] = ((accum[off + 2] * inv_pa).clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
                rgba[off + 3] = a_u8;
            }
        }

        RgbaPatch {
            x: x0 as u32,
            y: y0 as u32,
            width: w,
            height: h,
            rgba,
            has_visible_pixels,
        }
    }
}

impl Drop for AssLayoutEngine {
    fn drop(&mut self) {
        // Strict libass destruction order: Track -> Renderer -> Library.
        self.clear_cached_tracks();
        unsafe {
            if !self.renderer.is_null() {
                ffi::ass_renderer_done(self.renderer);
                self.renderer = ptr::null_mut();
            }
            if !self.library.is_null() {
                ffi::ass_library_done(self.library);
                self.library = ptr::null_mut();
            }
        }
    }
}

fn floor_char_boundary(s: &str, idx: usize) -> usize {
    let mut i = idx.min(s.len());
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

fn ceil_char_boundary(s: &str, idx: usize) -> usize {
    let mut i = idx.min(s.len());
    while i < s.len() && !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

#[derive(Debug, Clone, Copy)]
struct PixelRect {
    min_x: i32,
    min_y: i32,
    max_x: i32,
    max_y: i32,
}

impl PixelRect {
    fn union(self, other: Self) -> Self {
        Self {
            min_x: self.min_x.min(other.min_x),
            min_y: self.min_y.min(other.min_y),
            max_x: self.max_x.max(other.max_x),
            max_y: self.max_y.max(other.max_y),
        }
    }

    fn width(self) -> f32 {
        (self.max_x - self.min_x).max(0) as f32
    }

    fn height(self) -> f32 {
        (self.max_y - self.min_y).max(0) as f32
    }

    fn center_x(self) -> f32 {
        (self.min_x as f32 + self.max_x as f32) * 0.5
    }

    fn center_y(self) -> f32 {
        (self.min_y as f32 + self.max_y as f32) * 0.5
    }
}

fn build_active_event(
    pe: &ParsedEvent,
    raw_bboxes: &[Option<PixelRect>],
    global_byte_offset: usize,
    global_utf16_offset: usize,
    frame_w: u32,
    frame_h: u32,
) -> AssActiveEvent {
    // 1. Group characters into visual lines (explicit \N splits + auto-wrap detection).
    let mut line_groups: Vec<Vec<(&ParsedChar, Option<PixelRect>)>> = Vec::new();
    let mut cur_group: Vec<(&ParsedChar, Option<PixelRect>)> = Vec::new();

    let mut cur_explicit_line: Option<u16> = None;
    let mut cur_ink_min_y: i32 = i32::MAX;
    let mut cur_ink_max_y: i32 = i32::MIN;
    let mut cur_first_ink_min_x: Option<i32> = None;
    let mut cur_last_ink_min_x: Option<i32> = None;
    let mut cur_last_ink_max_x: Option<i32> = None;
    let mut cur_ink_y_sum: f32 = 0.0;
    let mut cur_ink_count: usize = 0;

    let reset_line_stats = |min_y: &mut i32,
                            max_y: &mut i32,
                            first_x: &mut Option<i32>,
                            last_min_x: &mut Option<i32>,
                            last_max_x: &mut Option<i32>,
                            y_sum: &mut f32,
                            count: &mut usize| {
        *min_y = i32::MAX;
        *max_y = i32::MIN;
        *first_x = None;
        *last_min_x = None;
        *last_max_x = None;
        *y_sum = 0.0;
        *count = 0;
    };

    for pc in &pe.chars {
        let bbox = if pc.color_id >= 1 && (pc.color_id as usize) <= raw_bboxes.len() {
            raw_bboxes[(pc.color_id - 1) as usize]
        } else {
            None
        };

        let explicit_changed = cur_explicit_line.is_some_and(|prev| prev != pc.explicit_line_idx);
        let auto_wrapped = if !explicit_changed && cur_ink_count > 0 {
            if let Some(b) = bbox {
                let line_h = (cur_ink_max_y - cur_ink_min_y).max(1) as f32;
                let line_cy = cur_ink_y_sum / cur_ink_count as f32;
                let disjoint_vert = b.min_y >= cur_ink_max_y - 1 || b.max_y <= cur_ink_min_y + 1;
                let wrapped_vert = disjoint_vert || (b.center_y() - line_cy).abs() > line_h * 0.65;
                // Require horizontal line-wrap reversal alongside vertical separation so
                // punctuation sitting purely in the ascender/descender band (e.g. apostrophes,
                // quotes, periods, ellipses, underscores) never falsely splits a horizontal line.
                let is_rtl_line = matches!(
                    (cur_first_ink_min_x, cur_last_ink_min_x),
                    (Some(fx), Some(lx)) if cur_ink_count >= 2 && lx < fx
                );
                let moved_to_new_line_x = if is_rtl_line {
                    cur_last_ink_max_x.is_some_and(|last_rx| b.max_x > last_rx)
                } else {
                    let last_cx = match (cur_last_ink_min_x, cur_last_ink_max_x) {
                        (Some(lx), Some(rx)) => Some((lx as f32 + rx as f32) * 0.5),
                        _ => None,
                    };
                    last_cx.is_some_and(|lcx| b.center_x() <= lcx)
                };
                moved_to_new_line_x && wrapped_vert
            } else {
                false
            }
        } else {
            false
        };

        if (explicit_changed || auto_wrapped) && !cur_group.is_empty() {
            line_groups.push(std::mem::take(&mut cur_group));
            reset_line_stats(
                &mut cur_ink_min_y,
                &mut cur_ink_max_y,
                &mut cur_first_ink_min_x,
                &mut cur_last_ink_min_x,
                &mut cur_last_ink_max_x,
                &mut cur_ink_y_sum,
                &mut cur_ink_count,
            );
        }

        cur_explicit_line = Some(pc.explicit_line_idx);
        if let Some(b) = bbox {
            cur_ink_min_y = cur_ink_min_y.min(b.min_y);
            cur_ink_max_y = cur_ink_max_y.max(b.max_y);
            if cur_first_ink_min_x.is_none() {
                cur_first_ink_min_x = Some(b.min_x);
            }
            cur_last_ink_min_x = Some(b.min_x);
            cur_last_ink_max_x = Some(b.max_x);
            cur_ink_y_sum += b.center_y();
            cur_ink_count += 1;
        }
        cur_group.push((pc, bbox));
    }
    if !cur_group.is_empty() {
        line_groups.push(cur_group);
    }

    // 2. For each visual line with inked glyphs, interpolate whitespace/uninked char boxes
    //    and compute midpoint-bridged horizontal hit intervals.
    let mut all_chars: Vec<AssCharBox> = Vec::new();
    let mut visual_lines: Vec<AssVisualLine> = Vec::new();
    let mut overall_bounds: Option<AssHighlightRect> = None;

    for group in line_groups {
        let mut ink_min_x = i32::MAX;
        let mut ink_min_y = i32::MAX;
        let mut ink_max_x = i32::MIN;
        let mut ink_max_y = i32::MIN;
        let mut ink_w_sum = 0.0f32;
        let mut ink_count = 0usize;

        for (_, bbox) in &group {
            if let Some(b) = bbox {
                ink_min_x = ink_min_x.min(b.min_x);
                ink_min_y = ink_min_y.min(b.min_y);
                ink_max_x = ink_max_x.max(b.max_x);
                ink_max_y = ink_max_y.max(b.max_y);
                ink_w_sum += b.width();
                ink_count += 1;
            }
        }

        if ink_count == 0 {
            continue;
        }

        let line_index = visual_lines.len();
        let line_top = ink_min_y as f32;
        let line_height = (ink_max_y - ink_min_y).max(1) as f32;
        let avg_w = (ink_w_sum / ink_count as f32).max(4.0);
        let pad_x = (line_height * 0.12).clamp(4.0, 12.0);
        let pad_y = (line_height * 0.14).clamp(4.0, 14.0);

        let ink_rect = AssHighlightRect::new(
            ink_min_x as f32,
            line_top,
            (ink_max_x - ink_min_x).max(1) as f32,
            line_height,
        );
        let hitbox = ink_rect.padded_clamped(pad_x, pad_y, frame_w, frame_h);

        let char_start = all_chars.len();
        let group_len = group.len();

        // Build per-char tight rectangles (interpolating whitespace / missing glyphs cleanly).
        let mut line_chars: Vec<AssCharBox> = Vec::with_capacity(group_len);
        for (i, (pc, bbox)) in group.iter().enumerate() {
            let (x, y, w, h, has_ink) = if let Some(b) = bbox {
                (b.min_x as f32, b.min_y as f32, b.width().max(1.0), b.height().max(1.0), true)
            } else {
                // Find nearest inked neighbors left and right on this visual line.
                let prev_ink = group[..i].iter().enumerate().rev().find_map(|(idx, (_, bb))| bb.map(|b| (idx, b)));
                let next_ink = group[i + 1..].iter().enumerate().find_map(|(rel, (_, bb))| bb.map(|b| (i + 1 + rel, b)));

                match (prev_ink, next_ink) {
                    (Some((pi, pb)), Some((ni, nb))) => {
                        let gap_slots = (ni - pi - 1).max(1) as f32;
                        let slot_idx = (i - pi - 1) as f32;
                        let gap_start = pb.max_x as f32;
                        let gap_end = nb.min_x as f32;
                        if gap_end > gap_start {
                            let step = (gap_end - gap_start) / gap_slots;
                            (gap_start + slot_idx * step, line_top, step.max(1.0), line_height, false)
                        } else {
                            (gap_start, line_top, 1.0, line_height, false)
                        }
                    }
                    (None, Some((ni, nb))) => {
                        let slots_before = (ni - i) as f32;
                        let est_w = avg_w * 0.55;
                        let x = (nb.min_x as f32 - slots_before * est_w).max(0.0);
                        (x, line_top, est_w, line_height, false)
                    }
                    (Some((pi, pb)), None) => {
                        let slots_after = (i - pi - 1) as f32;
                        let est_w = avg_w * 0.55;
                        let x = pb.max_x as f32 + slots_after * est_w;
                        (x, line_top, est_w, line_height, false)
                    }
                    (None, None) => (ink_min_x as f32, line_top, avg_w, line_height, false),
                }
            };

            line_chars.push(AssCharBox {
                ch: pc.ch,
                byte_offset: pc.byte_offset,
                utf16_offset: pc.utf16_offset,
                utf16_len: pc.utf16_len,
                x,
                y,
                w,
                h,
                hit_x0: x,
                hit_x1: x + w,
                line_index,
                has_ink,
                is_whitespace: pc.is_whitespace,
            });
        }

        // Sort characters by horizontal center if RTL/reordered, preserving order on ties,
        // then partition inter-character gaps at midpoints so hit-testing is O(log N) and gapless.
        let is_monotonic = line_chars
            .windows(2)
            .all(|pair| pair[0].x + pair[0].w * 0.5 <= pair[1].x + pair[1].w * 0.5);
        if !is_monotonic {
            line_chars.sort_by(|a, b| {
                let ca = a.x + a.w * 0.5;
                let cb = b.x + b.w * 0.5;
                ca.total_cmp(&cb)
            });
        }

        let n = line_chars.len();
        for idx in 0..n {
            if idx == 0 {
                line_chars[idx].hit_x0 = hitbox.x.min(line_chars[idx].x);
            }
            if idx + 1 < n {
                let right_cur = line_chars[idx].x + line_chars[idx].w;
                let left_next = line_chars[idx + 1].x;
                let raw_mid = if right_cur <= left_next {
                    (right_cur + left_next) * 0.5
                } else {
                    let center_cur = line_chars[idx].x + line_chars[idx].w * 0.5;
                    let center_next = line_chars[idx + 1].x + line_chars[idx + 1].w * 0.5;
                    (center_cur + center_next) * 0.5
                };
                // Enforce the non-decreasing `hit_x1` invariant required by `partition_point`.
                let mid = raw_mid.max(line_chars[idx].hit_x0);
                line_chars[idx].hit_x1 = mid;
                line_chars[idx + 1].hit_x0 = mid;
            } else {
                line_chars[idx].hit_x1 = hitbox
                    .right()
                    .max(line_chars[idx].x + line_chars[idx].w)
                    .max(line_chars[idx].hit_x0);
            }
        }

        all_chars.extend(line_chars);
        let char_end = all_chars.len();

        overall_bounds = Some(match overall_bounds {
            Some(prev) => prev.union(ink_rect),
            None => ink_rect,
        });

        visual_lines.push(AssVisualLine {
            line_index,
            char_start,
            char_end,
            ink_rect,
            hitbox,
            baseline_top: line_top,
            line_height,
        });
    }

    AssActiveEvent {
        event_index: pe.event_index,
        dialogue: pe.dialogue.clone(),
        plain_text: pe.plain_text.clone(),
        global_byte_offset,
        global_utf16_offset,
        chars: all_chars,
        lines: visual_lines,
        bounds: overall_bounds,
        is_drawing_only: pe.is_drawing_only,
        has_animated_geometry: pe.has_animated_geometry,
        is_rotated_or_sign: pe.is_rotated_or_sign,
        has_explicit_pos: pe.has_explicit_pos,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_HEADER: &str = "\
[Script Info]
ScriptType: v4.00+
PlayResX: 960
PlayResY: 540
ScaledBorderAndShadow: yes
WrapStyle: 0

[V4+ Styles]
Format: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding
Style: Default,Noto Sans CJK JP,36,&H00FFFFFF,&H000000FF,&H00000000,&H64000000,0,0,0,0,100,100,4,0,1,2,1,2,10,10,10,1
";

    #[test]
    fn viewport_transform_contain_letterbox_and_pillarbox() {
        // 1920x1080 video inside 1920x1200 area -> letterbox top/bottom by 60px.
        let vt = ViewportTransform::contain(1920.0, 1200.0, 1920, 1080);
        assert!((vt.scale - 1.0).abs() < 1e-5);
        assert!((vt.offset_x - 0.0).abs() < 1e-5);
        assert!((vt.offset_y - 60.0).abs() < 1e-5);
        assert_eq!(vt.logical_to_frame(500.0, 10.0), None);
        let (fx, fy) = vt.logical_to_frame(500.0, 160.0).unwrap();
        assert!((fx - 500.0).abs() < 1e-4 && (fy - 100.0).abs() < 1e-4);

        // 1920x1080 video inside 1280x540 area -> pillarbox left/right by 160px, scale 0.5.
        let vt2 = ViewportTransform::contain(1280.0, 540.0, 1920, 1080);
        assert!((vt2.scale - 0.5).abs() < 1e-5);
        assert!((vt2.offset_x - 160.0).abs() < 1e-5);
        assert!((vt2.offset_y - 0.0).abs() < 1e-5);
        let (lx, ly) = vt2.frame_to_logical(960.0, 540.0);
        assert!((lx - 640.0).abs() < 1e-4 && (ly - 270.0).abs() < 1e-4);
    }

    #[test]
    fn probe_multi_event_collision_and_highlight_patch() {
        let mut engine = AssLayoutEngine::new().expect("libass init");
        engine.configure(&MpvAssParams::for_frame(1920, 1080));

        let ass_full = "\
Dialogue: 0,0:00:10.00,0:00:15.00,Default,,0,0,0,,{\\pos(480,502)\\fscx50}鳥白島に到着\n\
Dialogue: 0,0:00:10.00,0:00:15.00,Default,,0,0,0,,{\\an8}上部テロップ\n\
Dialogue: 0,0:00:10.00,0:00:15.00,Default,,0,0,0,,{\\p1}m 0 0 l 100 0 100 100 0 100{\\p0}\n";

        let layout = engine
            .probe_active_events(SAMPLE_HEADER, ass_full, 12_000)
            .expect("probe");

        assert_eq!(layout.events.len(), 3);
        assert!(!layout.events[0].is_drawing_only);
        assert!(!layout.events[1].is_drawing_only);
        assert!(layout.events[2].is_drawing_only);
        assert!(layout.events[2].lines.is_empty());

        // Event 0 is near the bottom; Event 1 (\an8) is near the top.
        let b0 = layout.events[0].bounds.expect("event 0 bounds");
        let b1 = layout.events[1].bounds.expect("event 1 bounds");
        assert!(b0.y > 800.0, "expected bottom event y > 800, got {}", b0.y);
        assert!(b1.y < 200.0, "expected top \\an8 event y < 200, got {}", b1.y);

        // Hit-test the center of '鳥' (first char of event 0) and verify scan_substring.
        let c0 = &layout.events[0].chars[0];
        let hit = layout
            .hit_test(c0.x + c0.w * 0.5, c0.y + c0.h * 0.5)
            .expect("hit on 鳥");
        assert_eq!(hit.event_index, 0);
        assert_eq!(hit.ch, '鳥');
        assert_eq!(hit.scan_substring, "鳥白島に到着");

        // Hit-test the midpoint gap between '鳥' and '白' (Spacing=4 -> 8px gap at 1080p).
        let c1 = &layout.events[0].chars[1];
        let gap_x = (c0.x + c0.w + c1.x) * 0.5 - 0.25;
        let gap_hit = layout
            .hit_test(gap_x, layout.events[0].lines[0].baseline_top + 10.0)
            .expect("gapless hit between 鳥 and 白");
        assert_eq!(gap_hit.ch, '鳥');

        // Compute highlight bar for "鳥白島" (3 UTF-16 code units) and render cropped patch.
        let hl_rects = layout.highlight_rects_for_utf16_span(0, 0, 3);
        assert_eq!(hl_rects.len(), 1);
        let patch = engine
            .render_highlight_patch(hl_rects[0], 12_000)
            .expect("patch render");
        assert!(patch.width > 20 && patch.height > 20);
        assert!(patch.has_visible_pixels);
    }

    #[test]
    fn load_memory_fonts_empty_and_reload_is_safe() {
        let mut engine = AssLayoutEngine::new().expect("libass init");
        engine.configure(&MpvAssParams::for_frame(1280, 720));
        let _ = engine
            .probe_active_events(
                SAMPLE_HEADER,
                "Dialogue: 0,0:00:01.00,0:00:05.00,Default,,0,0,0,,テスト",
                2_000,
            )
            .unwrap();
        // Reloading memory fonts must cleanly drop cached track & recreate renderer.
        engine.load_memory_fonts(&[]).expect("clear fonts");
        assert_eq!(engine.loaded_font_count(), 0);
        let layout = engine
            .probe_active_events(
                SAMPLE_HEADER,
                "Dialogue: 0,0:00:01.00,0:00:05.00,Default,,0,0,0,,テスト",
                2_000,
            )
            .unwrap();
        assert_eq!(layout.events[0].lines.len(), 1);

        // Non-UTF-8-boundary byte offsets in highlight_rects_for_byte_span must snap safely.
        // "テスト" is 9 bytes (3 bytes per katakana char).
        let snapped = layout.highlight_rects_for_byte_span(0, 1, 5);
        assert_eq!(snapped.len(), 1);
        assert!(engine.read_track_from_str("").is_err());
    }

    #[test]
    fn punctuation_does_not_falsely_split_line_and_hit_x1_stays_monotonic() {
        let make_char = |ch: char, idx: usize, cid: u32| ParsedChar {
            ch,
            byte_offset: idx,
            utf16_offset: idx,
            utf16_len: 1,
            color_id: cid,
            explicit_line_idx: 0,
            is_whitespace: false,
        };
        let make_dialogue = |text: &str| AssDialogueLine {
            layer: 0,
            start_ms: 0,
            end_ms: 5_000,
            style: "Default".to_string(),
            name: String::new(),
            margin_l: 0,
            margin_r: 0,
            margin_v: 0,
            effect: String::new(),
            raw_text: text.to_string(),
        };

        // 1. "won't": lowercase 'w','o','n' sit in x-height [966..990], apostrophe '\'' sits in
        //    ascender zone [950..960] (vertically disjoint!), 't' spans [952..990].
        //    Must stay on ONE visual line (no false wrap split!).
        let text = "won't";
        let chars: Vec<ParsedChar> = text
            .chars()
            .enumerate()
            .map(|(i, ch)| make_char(ch, i, (i + 1) as u32))
            .collect();
        let pe = ParsedEvent {
            event_index: 0,
            dialogue: make_dialogue(text),
            plain_text: text.to_string(),
            chars,
            is_drawing_only: false,
            has_animated_geometry: false,
            is_rotated_or_sign: false,
            has_explicit_pos: false,
        };
        let bboxes = vec![
            Some(PixelRect { min_x: 100, min_y: 966, max_x: 120, max_y: 990 }), // w
            Some(PixelRect { min_x: 122, min_y: 966, max_x: 138, max_y: 990 }), // o
            Some(PixelRect { min_x: 140, min_y: 966, max_x: 156, max_y: 990 }), // n
            Some(PixelRect { min_x: 158, min_y: 950, max_x: 164, max_y: 960 }), // '
            Some(PixelRect { min_x: 166, min_y: 952, max_x: 178, max_y: 990 }), // t
        ];
        let ev = build_active_event(&pe, &bboxes, 0, 0, 1920, 1080);
        assert_eq!(ev.lines.len(), 1, "apostrophe must not falsely split 'won't' into 2 lines");

        // 2. Overlapping / kerned glyphs ("f's"): 'f' [10..30] (c=20), '\'' [26..28] (c=27),
        //    's' [22..34] (c=28). `hit_x1` must be strictly non-decreasing so `partition_point`
        //    always hits every character at its center!
        let text2 = "f's";
        let chars2: Vec<ParsedChar> = text2
            .chars()
            .enumerate()
            .map(|(i, ch)| make_char(ch, i, (i + 1) as u32))
            .collect();
        let pe2 = ParsedEvent {
            event_index: 0,
            dialogue: make_dialogue(text2),
            plain_text: text2.to_string(),
            chars: chars2,
            is_drawing_only: false,
            has_animated_geometry: false,
            is_rotated_or_sign: false,
            has_explicit_pos: false,
        };
        let bboxes2 = vec![
            Some(PixelRect { min_x: 10, min_y: 100, max_x: 30, max_y: 140 }), // f
            Some(PixelRect { min_x: 26, min_y: 100, max_x: 28, max_y: 112 }), // '
            Some(PixelRect { min_x: 22, min_y: 115, max_x: 34, max_y: 140 }), // s
        ];
        let ev2 = build_active_event(&pe2, &bboxes2, 0, 0, 1920, 1080);
        assert_eq!(ev2.lines.len(), 1);
        for pair in ev2.chars.windows(2) {
            assert!(
                pair[0].hit_x1 <= pair[1].hit_x1,
                "hit_x1 must be monotonic: {} > {}",
                pair[0].hit_x1,
                pair[1].hit_x1
            );
            assert!(pair[0].hit_x0 <= pair[0].hit_x1);
        }
        let layout = AssFrameLayout {
            frame_w: 1920,
            frame_h: 1080,
            now_ms: 1_000,
            script_info: AssScriptInfo::default(),
            events: vec![ev2],
            combined_plain_text: text2.to_string(),
            has_animated_geometry: false,
            hit_order: vec![0],
        };
        for c in &layout.events[0].chars {
            let hit = layout
                .hit_test(c.x + c.w * 0.5, 120.0)
                .expect("center hit");
            assert_eq!(hit.ch, c.ch);
        }
    }

    #[test]
    fn animated_probe_cache_tracks_movement_and_clean_patch_works() {
        let mut engine = AssLayoutEngine::new().expect("libass init");
        engine.configure(&MpvAssParams::for_frame(1280, 720));

        let ass_full = "Dialogue: 0,0:00:05.00,0:00:10.00,Default,,0,0,0,,{\\move(100,300,700,300,0,5000)}移動テスト";
        let l1 = engine
            .probe_active_events(SAMPLE_HEADER, ass_full, 5_500)
            .expect("probe t1");
        let l2 = engine
            .probe_active_events(SAMPLE_HEADER, ass_full, 9_500)
            .expect("probe t2 (cached probe_track)");
        assert!(l1.has_animated_geometry && l2.has_animated_geometry);
        let b1 = l1.events[0].bounds.expect("b1");
        let b2 = l2.events[0].bounds.expect("b2");
        assert!(
            b2.x > b1.x + 100.0,
            "expected \\move to advance x across cached probes: b1.x={}, b2.x={}",
            b1.x,
            b2.x
        );

        let patch = engine
            .render_clean_patch(SAMPLE_HEADER, ass_full, b2, 9_500)
            .expect("render_clean_patch");
        assert!(patch.has_visible_pixels);
    }
}

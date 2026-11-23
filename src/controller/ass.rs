//! Runtime controller bridging `subomi-ass` (`libass` layout probing, tight
//! highlight-patch rendering, and MKV EBML extraction) with Slint's
//! `SubtitleOverlayBridge` and `SubomiApp`.

use std::collections::HashSet;
use std::path::Path;
use std::rc::Rc;
use std::time::{Duration, Instant};

use subomi_ass::{
    AssCharBox, AssFrameLayout, AssHighlightRect, AssHinting, AssLayoutEngine, AssOverrideMode,
    AssShapingLevel, MkvFontAttachment, MpvAssParams, RgbaPatch, ViewportTransform,
};
use subomi_player::mpv::PlaybackState;
use slint::{ComponentHandle, Model, Rgba8Pixel, SharedPixelBuffer, VecModel};

use crate::controller::subtitles::{cached_subtitle_furigana_parts, prewarm_subtitle_furigana};
use crate::ui::{AssHighlightPatch, AssLineHitbox, AssRubyItem, SubomiApp, SubtitleOverlayBridge};

/// Minimum interval between animated-geometry (`\move` / geometry `\t`) layout re-probes (~15 Hz).
const ANIMATED_PROBE_INTERVAL: Duration = Duration::from_millis(66);
/// Poll interval while waiting for background MeCab prewarm to populate ASS furigana cache.
const FURIGANA_CACHE_POLL_INTERVAL: Duration = Duration::from_millis(50);
/// Stop re-polling furigana cache for the same revision after this window (e.g. MeCab unavailable).
const FURIGANA_CACHE_POLL_TIMEOUT: Duration = Duration::from_secs(2);

/// Returns true if an `mpv` subtitle track codec is ASS/SSA.
pub fn is_ass_subtitle_codec(codec: &str) -> bool {
    matches!(codec.trim().to_ascii_lowercase().as_str(), "ass" | "ssa")
}

/// Returns true if `path` has a Matroska container extension (`.mkv`, `.mka`, `.mks`, `.mk3d`).
pub fn is_matroska_path(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| {
            matches!(
                ext.to_ascii_lowercase().as_str(),
                "mkv" | "mka" | "mks" | "mk3d"
            )
        })
}

/// Build `libass` renderer parameters matching `mpv`'s live `PlaybackState`.
pub fn mpv_ass_params_from_playback(
    state: &PlaybackState,
    fallback_frame_size: (u32, u32),
) -> MpvAssParams {
    let osd = state.osd_dimensions;
    let (frame_w, frame_h) = if osd.w > 0 && osd.h > 0 {
        (osd.w, osd.h)
    } else if fallback_frame_size.0 > 0 && fallback_frame_size.1 > 0 {
        fallback_frame_size
    } else if state.dimensions.0 > 0 && state.dimensions.1 > 0 {
        state.dimensions
    } else if state.video_width > 0 && state.video_height > 0 {
        (state.video_width as u32, state.video_height as u32)
    } else {
        (1920, 1080)
    };

    let (storage_w, storage_h) = if state.video_width > 0 && state.video_height > 0 {
        (state.video_width as u32, state.video_height as u32)
    } else if state.dimensions.0 > 0 && state.dimensions.1 > 0 {
        state.dimensions
    } else {
        (frame_w, frame_h)
    };

    MpvAssParams {
        frame_w,
        frame_h,
        storage_w,
        storage_h,
        margin_top: osd.mt,
        margin_bottom: osd.mb,
        margin_left: osd.ml,
        margin_right: osd.mr,
        use_margins: state.sub_use_margins,
        sub_scale: state.sub_scale.max(0.05),
        sub_pos: state.sub_pos.clamp(0.0, 150.0),
        sub_font: if state.sub_font.trim().is_empty() {
            "sans-serif".to_string()
        } else {
            state.sub_font.clone()
        },
        sub_ass_override: AssOverrideMode::from_mpv_str(&state.sub_ass_override),
        sub_scale_signs: state.sub_scale_signs,
        hinting: AssHinting::from_mpv_str(&state.sub_hinting),
        shaping: AssShapingLevel::from_mpv_str(&state.sub_shaper),
        pixel_aspect: if osd.par.is_finite() && osd.par > 0.0 {
            osd.par
        } else {
            1.0
        },
    }
}

/// Convert a straight-alpha [`RgbaPatch`] into a Slint [`slint::Image`].
pub fn rgba_patch_to_slint_image(patch: &RgbaPatch) -> Option<slint::Image> {
    if patch.width == 0 || patch.height == 0 || !patch.has_visible_pixels {
        return None;
    }
    let expected = (patch.width as usize)
        .saturating_mul(patch.height as usize)
        .saturating_mul(4);
    if patch.rgba.len() != expected {
        return None;
    }
    let buf =
        SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(&patch.rgba, patch.width, patch.height);
    Some(slint::Image::from_rgba8(buf))
}

/// Compute `Alt+A` furigana ruby items mapped precisely over each kanji run's `libass`
/// character boxes, suppressing rotated/sheared signs, tiny text, and `\p1` drawings.
pub fn build_ass_ruby_items(
    layout: &AssFrameLayout,
    transform: &ViewportTransform,
    furigana_ratio: f32,
    mut parts_provider: impl FnMut(&str) -> Option<Vec<(String, String)>>,
) -> Vec<AssRubyItem> {
    let mut items = Vec::new();
    let ratio = furigana_ratio.clamp(0.25, 0.75);
    let min_line_h = (layout.frame_h as f32) * 0.02;

    for ev in &layout.events {
        if ev.is_drawing_only
            || ev.is_rotated_or_sign
            || ev.lines.is_empty()
            || ev.plain_text.trim().is_empty()
        {
            continue;
        }
        let Some(parts) = parts_provider(&ev.plain_text) else {
            continue;
        };
        let mut cursor_byte = 0usize;
        for (surface, reading) in parts {
            let surf_len = surface.len();
            if surf_len == 0 {
                continue;
            }
            let Some(rel) = ev
                .plain_text
                .get(cursor_byte..)
                .and_then(|s| s.find(&surface))
            else {
                continue;
            };
            let b0 = cursor_byte + rel;
            let b1 = b0 + surf_len;
            cursor_byte = b1;

            if reading.trim().is_empty() {
                continue;
            }

            // Group matched characters by visual line so multi-line runs never span wildly.
            for line in &ev.lines {
                if line.line_height < min_line_h {
                    continue;
                }
                let mut min_x = f32::INFINITY;
                let mut max_x = f32::NEG_INFINITY;
                let line_chars = ev.chars.get(line.char_start..line.char_end).unwrap_or(&[]);
                for c in line_chars {
                    if c.byte_offset >= b0 && c.byte_offset < b1 && c.has_ink {
                        min_x = min_x.min(c.x);
                        max_x = max_x.max(c.x + c.w);
                    }
                }
                if !min_x.is_finite() || max_x <= min_x {
                    continue;
                }
                let (lx, ly, lw, lh) = transform.frame_rect_to_logical(AssHighlightRect::new(
                    min_x,
                    line.baseline_top,
                    max_x - min_x,
                    line.line_height,
                ));
                let font_px = (lh * ratio).clamp(9.0, 26.0);
                let est_ruby_h = font_px * 1.25 + 2.0;
                let ruby_y = (ly - est_ruby_h - 2.0).max(0.0);
                items.push(AssRubyItem {
                    text: reading.clone().into(),
                    x: lx,
                    y: ruby_y,
                    width: lw.max(font_px),
                    font_size: font_px,
                });
            }
        }
    }
    items
}

/// Active pointer hit on a rendered ASS character, ready for dictionary scanning.
#[derive(Debug, Clone, PartialEq)]
pub struct AssHoveredTarget {
    pub event_index: usize,
    pub line_index: usize,
    pub char_index: usize,
    pub ch: char,
    pub event_byte_offset: usize,
    pub event_utf16_offset: usize,
    pub global_char_index: usize,
    pub scan_substring: String,
    pub event_plain_text: String,
    pub event_start_s: f64,
    pub event_end_s: f64,
    pub line_rect: AssHighlightRect,
    pub char_box: AssCharBox,
}

/// Snapshot of the currently highlighted/looked-up ASS word (used for Anki `{subtitle}`,
/// `{cloze-*}`, `{audio-media}`, and window-resize highlight re-projection).
#[derive(Debug, Clone, PartialEq)]
pub struct AssActiveLookup {
    pub event_index: usize,
    pub event_plain_text: String,
    pub event_start_s: f64,
    pub event_end_s: f64,
    pub scanned_surface: String,
    pub byte_offset_in_event: usize,
    pub utf16_offset_in_event: usize,
    pub matched_utf16_len: usize,
}

/// Owns the lazy `libass` [`AssLayoutEngine`] and synchronizes hitboxes, cropped
/// RGBA highlight patches, popup anchor geometry, and optional furigana with Slint.
pub struct AssOverlayController {
    engine: Option<AssLayoutEngine>,
    active: bool,
    loaded_fonts: Vec<MkvFontAttachment>,
    font_generation: u64,
    applied_font_generation: u64,
    last_probe_key: Option<(String, String, MpvAssParams, u64)>,
    last_animated_probe_at: Option<Instant>,
    last_viewport: Option<ViewportTransform>,
    layout: Option<AssFrameLayout>,
    layout_revision: u64,
    hovered_lines: HashSet<(i32, i32)>,
    active_lookup: Option<AssActiveLookup>,
    last_furigana_key: Option<(u64, bool, u32)>,
    furigana_requested_revision: Option<u64>,
    furigana_request_started_at: Option<Instant>,
    furigana_last_poll_at: Option<Instant>,
    last_logged_error: Option<String>,
}

impl Default for AssOverlayController {
    fn default() -> Self {
        Self::new()
    }
}

fn clear_slint_overlay_state(sb: &SubtitleOverlayBridge, clear_native_active: bool) {
    if clear_native_active {
        sb.set_ass_native_active(false);
    }
    sb.set_ass_line_hitboxes(Rc::new(VecModel::from(Vec::<AssLineHitbox>::new())).into());
    sb.set_ass_highlight_patches(Rc::new(VecModel::from(Vec::<AssHighlightPatch>::new())).into());
    sb.set_ass_ruby_items(Rc::new(VecModel::from(Vec::<AssRubyItem>::new())).into());
    sb.set_ass_anchor_active(false);
    sb.set_ass_hovered(false);
}

impl AssOverlayController {
    pub fn new() -> Self {
        Self {
            engine: None,
            active: false,
            loaded_fonts: Vec::new(),
            font_generation: 0,
            applied_font_generation: 0,
            last_probe_key: None,
            last_animated_probe_at: None,
            last_viewport: None,
            layout: None,
            layout_revision: 0,
            hovered_lines: HashSet::new(),
            active_lookup: None,
            last_furigana_key: None,
            furigana_requested_revision: None,
            furigana_request_started_at: None,
            furigana_last_poll_at: None,
            last_logged_error: None,
        }
    }

    pub fn is_active(&self) -> bool {
        self.active
    }

    pub fn layout(&self) -> Option<&AssFrameLayout> {
        self.layout.as_ref()
    }

    pub fn viewport(&self) -> Option<ViewportTransform> {
        self.last_viewport
    }

    pub fn active_lookup(&self) -> Option<&AssActiveLookup> {
        self.active_lookup.as_ref()
    }

    fn log_error_once(&mut self, context: &str, err: &str) {
        let msg = format!("[subomi-ass] {context}: {err}");
        if self.last_logged_error.as_deref() != Some(msg.as_str()) {
            eprintln!("{msg}");
            self.last_logged_error = Some(msg);
        }
    }

    fn reset_furigana_tracking(&mut self) {
        self.last_furigana_key = None;
        self.furigana_requested_revision = None;
        self.furigana_request_started_at = None;
        self.furigana_last_poll_at = None;
    }

    /// Replace embedded MKV memory fonts (`ass_add_font`) and schedule re-probe on next sync.
    pub fn set_memory_fonts(&mut self, fonts: Vec<MkvFontAttachment>) {
        self.loaded_fonts = fonts;
        self.font_generation = self.font_generation.wrapping_add(1);
        self.last_probe_key = None;
    }

    /// Reset all state when switching or closing media files.
    pub fn clear_for_new_media(&mut self, app: &SubomiApp) {
        self.loaded_fonts.clear();
        self.loaded_fonts.shrink_to_fit();
        self.font_generation = self.font_generation.wrapping_add(1);
        if let Some(engine) = self.engine.as_mut() {
            if let Err(err) = engine.load_memory_fonts(&[]) {
                eprintln!("[subomi-ass] failed to release libass memory fonts: {err}");
            }
            self.applied_font_generation = self.font_generation;
        }
        crate::app::trim_os_heap();
        self.last_probe_key = None;
        self.last_animated_probe_at = None;
        self.layout = None;
        self.layout_revision = self.layout_revision.wrapping_add(1);
        self.hovered_lines.clear();
        self.active_lookup = None;
        self.reset_furigana_tracking();
        self.last_logged_error = None;
        self.active = false;
        let sb = app.global::<SubtitleOverlayBridge>();
        clear_slint_overlay_state(&sb, true);
    }

    /// Synchronize native ASS state, hitboxes, highlight re-projection, and furigana from
    /// the latest playback state.
    #[allow(clippy::too_many_arguments)]
    pub fn sync_from_playback(
        &mut self,
        app: &SubomiApp,
        native_ass_active: bool,
        extradata: &str,
        ass_full: &str,
        params: &MpvAssParams,
        now_ms: i64,
        paused: bool,
        furigana_ratio: f32,
    ) {
        let sb = app.global::<SubtitleOverlayBridge>();
        if !native_ass_active {
            if self.active || sb.get_ass_native_active() {
                self.active = false;
                self.layout = None;
                self.last_probe_key = None;
                self.hovered_lines.clear();
                self.active_lookup = None;
                self.reset_furigana_tracking();
                clear_slint_overlay_state(&sb, true);
            }
            return;
        }

        if !self.active || !sb.get_ass_native_active() {
            self.active = true;
            sb.set_ass_native_active(true);
        }

        let area_w = app.get_video_width().max(1) as f32;
        let area_h = app.get_video_height().max(1) as f32;
        let transform = ViewportTransform::contain(
            area_w,
            area_h,
            params.frame_w.max(1),
            params.frame_h.max(1),
        );
        let viewport_changed = self.last_viewport != Some(transform);
        self.last_viewport = Some(transform);

        let trimmed_full = ass_full.trim();
        if trimmed_full.is_empty() {
            if self.layout.is_some() || sb.get_ass_line_hitboxes().row_count() > 0 {
                self.layout = None;
                self.last_probe_key = None;
                self.last_animated_probe_at = None;
                self.hovered_lines.clear();
                self.active_lookup = None;
                self.reset_furigana_tracking();
                clear_slint_overlay_state(&sb, false);
            }
            return;
        }

        // Borrowed comparison against `last_probe_key` avoids cloning `extradata`,
        // `ass_full`, and `MpvAssParams` on steady-state 60 Hz playback ticks.
        let static_changed = !self.last_probe_key.as_ref().is_some_and(|prev| {
            prev.0 == extradata
                && prev.1 == trimmed_full
                && &prev.2 == params
                && prev.3 == self.font_generation
        });
        let animated_due = !static_changed
            && !paused
            && self
                .layout
                .as_ref()
                .is_some_and(|l| l.has_animated_geometry)
            && self
                .last_animated_probe_at
                .map_or(true, |t| t.elapsed() >= ANIMATED_PROBE_INTERVAL);

        if static_changed || animated_due {
            let cue_text_changed = self
                .last_probe_key
                .as_ref()
                .is_none_or(|prev| prev.1 != trimmed_full || prev.0 != extradata);

            match self.ensure_engine_ready(params) {
                Ok(engine) => match engine.probe_active_events(extradata, trimmed_full, now_ms) {
                    Ok(layout) => {
                        self.last_logged_error = None;
                        self.layout = Some(layout);
                        self.layout_revision = self.layout_revision.wrapping_add(1);
                        if static_changed {
                            self.last_probe_key = Some((
                                extradata.to_string(),
                                trimmed_full.to_string(),
                                params.clone(),
                                self.font_generation,
                            ));
                        }
                        self.last_animated_probe_at = Some(Instant::now());

                        if cue_text_changed {
                            self.hovered_lines.clear();
                            self.active_lookup = None;
                            sb.set_ass_hovered(false);
                            sb.set_ass_highlight_patches(
                                Rc::new(VecModel::from(Vec::<AssHighlightPatch>::new())).into(),
                            );
                            sb.set_ass_anchor_active(false);
                        }
                        self.push_hitboxes(app, &transform);
                        if !cue_text_changed {
                            if let Some(lookup) = self.active_lookup.clone() {
                                self.rebuild_highlight_for_lookup(app, &lookup, &transform);
                            }
                        }
                    }
                    Err(err) => self.log_error_once("probe_active_events failed", &err),
                },
                Err(err) => self.log_error_once("ensure_engine_ready failed", &err),
            }
        } else if viewport_changed && self.layout.is_some() {
            self.push_hitboxes(app, &transform);
            if let Some(lookup) = self.active_lookup.clone() {
                self.rebuild_highlight_for_lookup(app, &lookup, &transform);
            }
            self.reset_furigana_tracking();
        }

        // Optional Alt+A furigana over ASS lines (strictly non-blocking on the UI thread).
        let furigana_visible = app.get_furigana_visible();
        let furi_key = (
            self.layout_revision,
            furigana_visible,
            furigana_ratio.to_bits(),
        );
        if self.last_furigana_key != Some(furi_key) {
            if !furigana_visible {
                self.last_furigana_key = Some(furi_key);
                self.furigana_requested_revision = None;
                self.furigana_request_started_at = None;
                self.furigana_last_poll_at = None;
                if sb.get_ass_ruby_items().row_count() > 0 {
                    sb.set_ass_ruby_items(
                        Rc::new(VecModel::from(Vec::<AssRubyItem>::new())).into(),
                    );
                }
            } else if let Some(layout) = self.layout.as_ref() {
                let already_requested =
                    self.furigana_requested_revision == Some(self.layout_revision);
                let poll_due = !already_requested
                    || self
                        .furigana_last_poll_at
                        .map_or(true, |t| t.elapsed() >= FURIGANA_CACHE_POLL_INTERVAL);
                if poll_due {
                    let mut missing_texts: Vec<String> = Vec::new();
                    let items = build_ass_ruby_items(layout, &transform, furigana_ratio, |text| {
                        if let Some(parts) = cached_subtitle_furigana_parts(text) {
                            Some(parts)
                        } else {
                            if !missing_texts.iter().any(|s| s == text) {
                                missing_texts.push(text.to_owned());
                            }
                            None
                        }
                    });
                    sb.set_ass_ruby_items(Rc::new(VecModel::from(items)).into());
                    if missing_texts.is_empty() {
                        self.last_furigana_key = Some(furi_key);
                        self.furigana_requested_revision = None;
                        self.furigana_request_started_at = None;
                        self.furigana_last_poll_at = None;
                    } else {
                        let now = Instant::now();
                        self.furigana_last_poll_at = Some(now);
                        if !already_requested {
                            self.furigana_requested_revision = Some(self.layout_revision);
                            self.furigana_request_started_at = Some(now);
                            prewarm_subtitle_furigana(missing_texts);
                        } else if self
                            .furigana_request_started_at
                            .is_some_and(|t| t.elapsed() >= FURIGANA_CACHE_POLL_TIMEOUT)
                        {
                            // MeCab unavailable or timed out for this revision: stop polling.
                            self.last_furigana_key = Some(furi_key);
                        }
                    }
                }
            } else {
                self.last_furigana_key = Some(furi_key);
                sb.set_ass_ruby_items(Rc::new(VecModel::from(Vec::<AssRubyItem>::new())).into());
            }
        }
    }

    fn ensure_engine_ready(
        &mut self,
        params: &MpvAssParams,
    ) -> Result<&mut AssLayoutEngine, String> {
        if self.engine.is_none() {
            self.engine = Some(AssLayoutEngine::new()?);
            self.applied_font_generation = u64::MAX;
        }
        let Some(engine) = self.engine.as_mut() else {
            return Err("ASS layout engine failed to initialize".to_owned());
        };
        if self.applied_font_generation != self.font_generation {
            let fonts = std::mem::take(&mut self.loaded_fonts);
            if let Err(err) = engine.load_memory_fonts(&fonts) {
                self.loaded_fonts = fonts;
                return Err(err);
            }
            self.applied_font_generation = self.font_generation;
        }
        engine.configure(params);
        Ok(engine)
    }

    fn push_hitboxes(&self, app: &SubomiApp, transform: &ViewportTransform) {
        let Some(layout) = self.layout.as_ref() else {
            return;
        };
        let mut hitboxes = Vec::new();
        for ev in &layout.events {
            if ev.is_drawing_only {
                continue;
            }
            for line in &ev.lines {
                let (lx, ly, lw, lh) = transform.frame_rect_to_logical(line.hitbox);
                if lw > 0.5 && lh > 0.5 {
                    hitboxes.push(AssLineHitbox {
                        event_index: ev.event_index as i32,
                        line_index: line.line_index as i32,
                        x: lx,
                        y: ly,
                        width: lw,
                        height: lh,
                    });
                }
            }
        }
        app.global::<SubtitleOverlayBridge>()
            .set_ass_line_hitboxes(Rc::new(VecModel::from(hitboxes)).into());
    }

    /// Track per-line hover state cleanly across `for`-loop TouchAreas so moving
    /// directly between two simultaneous ASS lines never flickers `ass-hovered`.
    pub fn on_line_hover_changed(
        &mut self,
        app: &SubomiApp,
        event_idx: i32,
        line_idx: i32,
        hovering: bool,
    ) {
        if hovering {
            self.hovered_lines.insert((event_idx, line_idx));
        } else {
            self.hovered_lines.remove(&(event_idx, line_idx));
        }
        let now_hovered = !self.hovered_lines.is_empty();
        let sb = app.global::<SubtitleOverlayBridge>();
        if sb.get_ass_hovered() != now_hovered {
            sb.set_ass_hovered(now_hovered);
        }
    }

    /// Hit-test a pointer coordinate in `video-area` logical pixels against the active ASS layout.
    pub fn hit_test_logical(&self, app: &SubomiApp, lx: f32, ly: f32) -> Option<AssHoveredTarget> {
        let layout = self.layout.as_ref()?;
        let transform = self.last_viewport.unwrap_or_else(|| {
            ViewportTransform::contain(
                app.get_video_width().max(1) as f32,
                app.get_video_height().max(1) as f32,
                layout.frame_w.max(1),
                layout.frame_h.max(1),
            )
        });
        let hit = layout.hit_test_logical(&transform, lx, ly)?;
        let ev = layout.events.get(hit.event_index)?;
        let global_char_index = layout
            .combined_plain_text
            .get(..hit.global_byte_offset)
            .map_or(hit.char_index, |prefix| prefix.chars().count());

        Some(AssHoveredTarget {
            event_index: hit.event_index,
            line_index: hit.line_index,
            char_index: hit.char_index,
            ch: hit.ch,
            event_byte_offset: hit.event_byte_offset,
            event_utf16_offset: hit.event_utf16_offset,
            global_char_index,
            scan_substring: hit.scan_substring,
            event_plain_text: ev.plain_text.clone(),
            event_start_s: ev.dialogue.start_ms as f64 / 1000.0,
            event_end_s: ev.dialogue.end_ms as f64 / 1000.0,
            line_rect: hit.line_rect,
            char_box: hit.char_box,
        })
    }

    /// Render the solid `#38a6d6` highlight bar(s) + crisp `libass` glyph patch(es) for a
    /// matched dictionary scan on `target`, and anchor `YomitanPopup` & `hover-bridge` to it.
    pub fn apply_lookup_highlight(
        &mut self,
        app: &SubomiApp,
        target: &AssHoveredTarget,
        matched_utf16_len: usize,
    ) {
        let matched_len = matched_utf16_len.max(1);
        let scanned_surface = slice_utf16_prefix(
            target
                .event_plain_text
                .get(target.event_byte_offset..)
                .unwrap_or(&target.scan_substring),
            matched_len,
        );
        let lookup = AssActiveLookup {
            event_index: target.event_index,
            event_plain_text: target.event_plain_text.clone(),
            event_start_s: target.event_start_s,
            event_end_s: target.event_end_s,
            scanned_surface,
            byte_offset_in_event: target.event_byte_offset,
            utf16_offset_in_event: target.event_utf16_offset,
            matched_utf16_len: matched_len,
        };
        let transform = self.last_viewport.unwrap_or_else(|| {
            let (fw, fh) = self
                .layout
                .as_ref()
                .map_or((1920, 1080), |l| (l.frame_w.max(1), l.frame_h.max(1)));
            ViewportTransform::contain(
                app.get_video_width().max(1) as f32,
                app.get_video_height().max(1) as f32,
                fw,
                fh,
            )
        });
        self.rebuild_highlight_for_lookup(app, &lookup, &transform);
        self.active_lookup = Some(lookup);
    }

    fn rebuild_highlight_for_lookup(
        &mut self,
        app: &SubomiApp,
        lookup: &AssActiveLookup,
        transform: &ViewportTransform,
    ) {
        let Some(layout) = self.layout.as_ref() else {
            return;
        };
        let rects = layout.highlight_rects_for_utf16_span(
            lookup.event_index,
            lookup.utf16_offset_in_event,
            lookup
                .utf16_offset_in_event
                .saturating_add(lookup.matched_utf16_len),
        );
        if rects.is_empty() {
            self.clear_lookup_highlight(app);
            return;
        }

        let now_ms = layout.now_ms;
        let mut patches = Vec::with_capacity(rects.len());
        let mut union_rect: Option<AssHighlightRect> = None;

        for rect in rects {
            union_rect = Some(match union_rect {
                Some(prev) => prev.union(rect),
                None => rect,
            });
            let (lx, ly, lw, lh) = transform.frame_rect_to_logical(rect);
            let rendered_patch = self
                .engine
                .as_mut()
                .and_then(|eng| eng.render_highlight_patch(rect, now_ms).ok());

            if let Some(patch) = rendered_patch {
                if let Some(img) = rgba_patch_to_slint_image(&patch) {
                    let (px, py, pw, ph) = transform.frame_rect_to_logical(AssHighlightRect::new(
                        patch.x as f32,
                        patch.y as f32,
                        patch.width as f32,
                        patch.height as f32,
                    ));
                    patches.push(AssHighlightPatch {
                        x: lx,
                        y: ly,
                        width: lw,
                        height: lh,
                        patch_x: px,
                        patch_y: py,
                        patch_width: pw,
                        patch_height: ph,
                        glyph_patch: img,
                        has_patch: true,
                    });
                    continue;
                }
            }
            patches.push(AssHighlightPatch {
                x: lx,
                y: ly,
                width: lw,
                height: lh,
                patch_x: lx,
                patch_y: ly,
                patch_width: lw,
                patch_height: lh,
                glyph_patch: slint::Image::default(),
                has_patch: false,
            });
        }

        let sb = app.global::<SubtitleOverlayBridge>();
        sb.set_ass_highlight_patches(Rc::new(VecModel::from(patches)).into());
        if let Some(anchor) = union_rect {
            let (ax, ay, aw, ah) = transform.frame_rect_to_logical(anchor);
            sb.set_ass_anchor_x(ax.round());
            sb.set_ass_anchor_top(ay.round());
            sb.set_ass_anchor_bottom((ay + ah).round());
            sb.set_ass_anchor_width(aw.round());
            sb.set_ass_anchor_active(true);
        }
    }

    /// Clear active ASS highlight bars, popup anchor, and stored lookup state.
    pub fn clear_lookup_highlight(&mut self, app: &SubomiApp) {
        self.active_lookup = None;
        let sb = app.global::<SubtitleOverlayBridge>();
        if sb.get_ass_highlight_patches().row_count() > 0 {
            sb.set_ass_highlight_patches(
                Rc::new(VecModel::from(Vec::<AssHighlightPatch>::new())).into(),
            );
        }
        if sb.get_ass_anchor_active() {
            sb.set_ass_anchor_active(false);
        }
    }
}

fn slice_utf16_prefix(s: &str, max_u16: usize) -> String {
    let mut out = String::new();
    let mut count = 0usize;
    for ch in s.chars() {
        let len = ch.len_utf16();
        if count + len > max_u16 {
            break;
        }
        out.push(ch);
        count += len;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codec_and_matroska_path_classifiers() {
        assert!(is_ass_subtitle_codec("ass"));
        assert!(is_ass_subtitle_codec("ASS"));
        assert!(is_ass_subtitle_codec("ssa"));
        assert!(!is_ass_subtitle_codec("subrip"));
        assert!(!is_ass_subtitle_codec("webvtt"));
        assert!(!is_ass_subtitle_codec("hdmv_pgs_subtitle"));

        assert!(is_matroska_path(Path::new("/tmp/ep01.mkv")));
        assert!(is_matroska_path(Path::new("/tmp/ep01.MKV")));
        assert!(is_matroska_path(Path::new("/tmp/subs.mks")));
        assert!(!is_matroska_path(Path::new("/tmp/ep01.mp4")));
        assert!(!is_matroska_path(Path::new("/tmp/ep01.ass")));
    }

    #[test]
    fn mpv_ass_params_uses_osd_dimensions_and_defaults() {
        let mut st = PlaybackState::default();
        st.video_width = 1920;
        st.video_height = 1080;
        st.osd_dimensions = subomi_player::OsdDimensions {
            w: 1920,
            h: 1080,
            mt: 0,
            mb: 0,
            ml: 0,
            mr: 0,
            par: 1.0,
        };
        st.sub_scale = 1.25;
        st.sub_scale_signs = true;
        st.sub_pos = 90.0;
        st.sub_font = "Noto Sans CJK JP".to_string();

        let p = mpv_ass_params_from_playback(&st, (1280, 720));
        assert_eq!((p.frame_w, p.frame_h), (1920, 1080));
        assert_eq!((p.storage_w, p.storage_h), (1920, 1080));
        assert!((p.sub_scale - 1.25).abs() < 1e-6);
        assert!(p.sub_scale_signs);
        assert!((p.sub_pos - 90.0).abs() < 1e-6);
        assert_eq!(p.sub_font, "Noto Sans CJK JP");
        assert_eq!(p.sub_ass_override, AssOverrideMode::Scale);
    }

    #[test]
    fn slice_utf16_prefix_handles_supplementary_emoji_and_kanji() {
        assert_eq!(slice_utf16_prefix("鳥白島に到着", 3), "鳥白島");
        // 🔊 is 2 UTF-16 code units.
        assert_eq!(slice_utf16_prefix("🔊まもなく", 2), "🔊");
        assert_eq!(slice_utf16_prefix("🔊まもなく", 4), "🔊まも");
    }
}

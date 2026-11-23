//! Pure subtitle visibility, autofit, cursor-autohide, title, and render-size helpers.

use std::path::Path;

/// Returns true if an `mpv` subtitle track codec is bitmap/image-based rather
/// than text-based (meaning Subomi's text overlay cannot render it).
pub fn is_image_subtitle_codec(codec: &str) -> bool {
    matches!(
        codec.trim().to_ascii_lowercase().as_str(),
        "dvd_subtitle"
            | "hdmv_pgs_subtitle"
            | "pgssub"
            | "dvb_subtitle"
            | "dvb_teletext"
            | "xsub"
    )
}

/// Compute `(sub-visibility, secondary-sub-visibility)` for `libmpv` without
/// toggling visibility during silence gaps between text cues.
///
/// Precedence:
/// 1. Explicit `Show Subtitles` off (`!subtitles_visible`) always hides both
///    tracks — `subtitle-cursor-show` never overrides it.
/// 2. When cursor-show is enabled for a track and the cursor is outside its
///    reveal zone, mpv hides that track even when `hide_mpv_subtitles == false`.
/// 3. Otherwise native ASS / bitmap image tracks render via mpv, and plain text
///    tracks render via mpv whenever Subomi's overlay does not own text.
#[allow(clippy::too_many_arguments)]
pub fn compute_mpv_subtitle_visibility_ex(
    hide_mpv_subtitles: bool,
    subtitles_visible: bool,
    hide_while_playing: bool,
    playing: bool,
    primary_is_image: bool,
    secondary_is_image: bool,
    primary_is_native_ass: bool,
    primary_cursor_show_enabled: bool,
    primary_cursor_reveal: bool,
    secondary_cursor_show_enabled: bool,
    secondary_cursor_reveal: bool,
) -> (bool, bool) {
    if !subtitles_visible {
        return (false, false);
    }

    let overlay_owns_text = hide_mpv_subtitles && !(hide_while_playing && playing);

    let primary_visible = if primary_cursor_show_enabled && !primary_cursor_reveal {
        false
    } else if primary_is_native_ass || primary_is_image {
        true
    } else {
        !overlay_owns_text
    };

    let secondary_visible = if secondary_cursor_show_enabled && !secondary_cursor_reveal {
        false
    } else if secondary_is_image {
        true
    } else {
        !overlay_owns_text
    };

    (primary_visible, secondary_visible)
}

/// Convenience wrapper for [`compute_mpv_subtitle_visibility_ex`] with native ASS
/// and cursor-show reveal disabled.
pub fn compute_mpv_subtitle_visibility(
    hide_mpv_subtitles: bool,
    subtitles_visible: bool,
    hide_while_playing: bool,
    playing: bool,
    primary_is_image: bool,
    secondary_is_image: bool,
) -> (bool, bool) {
    compute_mpv_subtitle_visibility_ex(
        hide_mpv_subtitles,
        subtitles_visible,
        hide_while_playing,
        playing,
        primary_is_image,
        secondary_is_image,
        false,
        false,
        false,
        false,
        false,
    )
}

/// Apply Subomi's configured `search/remove-regex`, `search/replace-lines`, and
/// `search/replace-with` text filters to subtitle text.
pub fn filter_subtitle_text(
    text: &str,
    remove_regex: &str,
    replace_lines: bool,
    replace_with: &str,
) -> String {
    subomi_dict::normalize::preprocess_search_text(text, remove_regex, replace_lines, replace_with)
}

/// Compute the auto-fitted logical window size `(width, height)` for a newly loaded
/// video (`behavior/autofit` and `behavior/autofit-percent`).
#[allow(clippy::too_many_arguments)]
pub fn compute_autofit_window_size(
    video_width: u32,
    video_height: u32,
    autofit_enabled: bool,
    autofit_percent: i32,
    is_fullscreen: bool,
    is_maximized: bool,
    drawer_visible: bool,
    drawer_width: f32,
    max_bounds: (f32, f32),
) -> Option<(f32, f32)> {
    if !autofit_enabled || is_fullscreen || is_maximized || video_width == 0 || video_height == 0 {
        return None;
    }
    let scale_factor = (autofit_percent.clamp(1, 1000) as f32) / 100.0;
    let extra_width = if drawer_visible && drawer_width.is_finite() && drawer_width > 0.0 {
        drawer_width
    } else {
        0.0
    };
    let req_w = (video_width as f32) * scale_factor + extra_width;
    let req_h = (video_height as f32) * scale_factor;
    if !req_w.is_finite() || !req_h.is_finite() || req_w <= 0.0 || req_h <= 0.0 {
        return None;
    }
    let (max_w, max_h) = max_bounds;
    let fit_scale = if max_w.is_finite() && max_w > 0.0 && max_h.is_finite() && max_h > 0.0 {
        (max_w / req_w).min(max_h / req_h).min(1.0)
    } else {
        1.0
    };
    let final_w = (req_w * fit_scale).round().max(1.0);
    let final_h = (req_h * fit_scale).round().max(1.0);
    Some((final_w, final_h))
}

/// Classify a pointer Y position inside the video area into `(in_top_zone, in_bottom_zone)`
/// for `behavior/subtitle-cursor-show-secondary` (top 25%) and
/// `behavior/subtitle-cursor-show` (bottom 25%).
pub fn subtitle_cursor_zones(pointer_y: f32, video_height: f32) -> (bool, bool) {
    if !pointer_y.is_finite()
        || !video_height.is_finite()
        || video_height <= 0.0
        || pointer_y < 0.0
        || pointer_y > video_height
    {
        return (false, false);
    }
    let ratio = pointer_y / video_height;
    (ratio <= 0.25, ratio >= 0.75)
}

/// Compute effective subtitle overlay visibility accounting for `subtitle-cursor-show`.
///
/// Precedence: `subtitle-cursor-show` NEVER overrides explicit `Show Subtitles`
/// off (`!subtitles_visible`) or `hide-subomi-while-playing` while playing.
pub fn compute_subtitle_overlay_visibility(
    has_text: bool,
    subtitles_visible: bool,
    hide_while_playing: bool,
    is_playing: bool,
    cursor_show_enabled: bool,
    cursor_in_zone: bool,
) -> bool {
    if !has_text || !subtitles_visible || (hide_while_playing && is_playing) {
        return false;
    }
    !cursor_show_enabled || cursor_in_zone
}

/// Compute effective visibility of the interactive ASS hover/selection overlay
/// (`ass-overlay`), enforcing `subtitles_visible`, `hide-subomi-while-playing`,
/// and `subtitle-cursor-show` precedence.
pub fn compute_ass_overlay_visibility(
    ass_native_active: bool,
    subtitles_visible: bool,
    hide_while_playing: bool,
    is_playing: bool,
    cursor_show_enabled: bool,
    cursor_in_zone: bool,
) -> bool {
    if !ass_native_active || !subtitles_visible || (hide_while_playing && is_playing) {
        return false;
    }
    !cursor_show_enabled || cursor_in_zone
}

/// Compute whether `mpv`'s `cursor-autohide` policy wants the video cursor hidden when
/// `behavior/osc-mpv-cursor` is enabled.
pub fn mpv_cursor_should_hide(
    autohide: subomi_player::mpv::CursorAutohide,
    fs_only: bool,
    is_fullscreen: bool,
    idle_elapsed_ms: u64,
    overlay_interactive: bool,
) -> bool {
    use subomi_player::mpv::CursorAutohide;
    if overlay_interactive {
        return false;
    }
    if fs_only && !is_fullscreen {
        return false;
    }
    match autohide {
        CursorAutohide::Never => false,
        CursorAutohide::Always => true,
        CursorAutohide::Number(ms) => {
            if ms < 0 {
                false
            } else {
                idle_elapsed_ms >= ms as u64
            }
        }
    }
}

/// Choose the display title from `mpv`'s `media-title`, falling back to the
/// filename component of `path`.
pub fn effective_media_title(media_title: &str, path: &str) -> String {
    let trimmed = media_title.trim();
    if !trimmed.is_empty() {
        return trimmed.to_owned();
    }
    if path.is_empty() {
        return String::new();
    }
    Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or(path)
        .to_owned()
}

/// Round a positive pixel dimension up to a quantization bucket (e.g. 64px) to
/// avoid per-pixel `glTexImage2D` reallocations during window drag-resize.
pub fn quantize_render_dimension(value: u32, bucket: u32) -> u32 {
    let bucket = bucket.max(1);
    let v = value.max(1);
    ((v + bucket - 1) / bucket) * bucket
}

/// Compute target render FBO/buffer dimensions for the current viewport.
pub fn compute_video_render_size(
    viewport_logical_w: i32,
    viewport_logical_h: i32,
    scale_factor: f32,
    video_dims: (u32, u32),
    opengl_active: bool,
) -> (u32, u32) {
    if viewport_logical_w <= 0 || viewport_logical_h <= 0 {
        return (0, 0);
    }
    let scale = if scale_factor.is_finite() && scale_factor > 0.0 {
        scale_factor
    } else {
        1.0
    };
    let viewport_w = ((viewport_logical_w as f32 * scale).round() as u32).max(1);
    let viewport_h = ((viewport_logical_h as f32 * scale).round() as u32).max(1);
    let (vw, vh) = video_dims;
    if opengl_active {
        if vw > 0 && vh > 0 {
            let ratio = (viewport_w as f64 / vw as f64).min(viewport_h as f64 / vh as f64);
            let ideal_w = ((vw as f64 * ratio).round() as u32).max(1);
            let quant_w = quantize_render_dimension(ideal_w, 64).min(8192);
            let quant_h =
                ((quant_w as f64 * vh as f64 / vw as f64).round() as u32).clamp(1, 8192);
            (quant_w, quant_h)
        } else {
            (
                quantize_render_dimension(viewport_w, 64).min(8192),
                quantize_render_dimension(viewport_h, 64).min(8192),
            )
        }
    } else if vw > 0 && vh > 0 {
        let ratio = (viewport_w as f64 / vw as f64)
            .min(viewport_h as f64 / vh as f64)
            .min(1.0);
        (
            ((vw as f64 * ratio).round() as u32).max(1),
            ((vh as f64 * ratio).round() as u32).max(1),
        )
    } else {
        (viewport_w, viewport_h)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_zones_use_25_percent_top_and_bottom_bands() {
        assert_eq!(subtitle_cursor_zones(0.0, 1000.0), (true, false));
        assert_eq!(subtitle_cursor_zones(250.0, 1000.0), (true, false));
        assert_eq!(subtitle_cursor_zones(251.0, 1000.0), (false, false));
        assert_eq!(subtitle_cursor_zones(500.0, 1000.0), (false, false));
        assert_eq!(subtitle_cursor_zones(749.0, 1000.0), (false, false));
        assert_eq!(subtitle_cursor_zones(750.0, 1000.0), (false, true));
        assert_eq!(subtitle_cursor_zones(1000.0, 1000.0), (false, true));
        assert_eq!(subtitle_cursor_zones(-1.0, 1000.0), (false, false));
        assert_eq!(subtitle_cursor_zones(1001.0, 1000.0), (false, false));
        assert_eq!(subtitle_cursor_zones(100.0, 0.0), (false, false));
    }

    #[test]
    fn cursor_show_never_overrides_show_subtitles_off_or_hide_while_playing() {
        // Explicit Show Subtitles OFF (`subtitles_visible = false`) wins even in zone.
        assert!(!compute_subtitle_overlay_visibility(
            true, false, false, false, true, true
        ));
        assert!(!compute_ass_overlay_visibility(
            true, false, false, false, true, true
        ));

        // `hide-subomi-while-playing` while playing wins even in zone (both plain & ASS overlay).
        assert!(!compute_subtitle_overlay_visibility(
            true, true, true, true, true, true
        ));
        assert!(!compute_ass_overlay_visibility(
            true, true, true, true, true, true
        ));
        // Paused + in zone -> visible.
        assert!(compute_subtitle_overlay_visibility(
            true, true, true, false, true, true
        ));
        assert!(compute_ass_overlay_visibility(
            true, true, true, false, true, true
        ));
        // Paused + outside zone with cursor-show -> hidden.
        assert!(!compute_subtitle_overlay_visibility(
            true, true, true, false, true, false
        ));
        assert!(!compute_ass_overlay_visibility(
            true, true, true, false, true, false
        ));
        // Without cursor-show, ASS overlay still respects `hide-subomi-while-playing`.
        assert!(!compute_ass_overlay_visibility(
            true, true, true, true, false, false
        ));
        assert!(compute_ass_overlay_visibility(
            true, true, true, false, false, false
        ));
        assert!(!compute_ass_overlay_visibility(
            false, true, false, false, false, true
        ));
    }

    #[test]
    fn mpv_visibility_enforces_show_subtitles_off_and_secondary_cursor_show_when_mpv_renders() {
        // Explicit Show Subtitles OFF hides mpv subtitles even when cursor reveal is active.
        assert_eq!(
            compute_mpv_subtitle_visibility_ex(
                false, false, false, true, false, false, false, true, true, true, true
            ),
            (false, false)
        );
        assert_eq!(
            compute_mpv_subtitle_visibility_ex(
                true, false, false, true, false, false, true, true, true, true, true
            ),
            (false, false)
        );

        // `hide_mpv_subtitles == false`: secondary-subtitle-cursor-show MUST hide secondary
        // mpv subtitles when cursor is outside the top zone, and show when inside.
        assert_eq!(
            compute_mpv_subtitle_visibility_ex(
                false, true, false, true, false, false, false, false, false, true, false
            ),
            (true, false)
        );
        assert_eq!(
            compute_mpv_subtitle_visibility_ex(
                false, true, false, true, false, false, false, false, false, true, true
            ),
            (true, true)
        );

        // `hide_mpv_subtitles == false`: primary-subtitle-cursor-show hides primary when outside zone.
        assert_eq!(
            compute_mpv_subtitle_visibility_ex(
                false, true, false, true, false, false, false, true, false, false, false
            ),
            (false, true)
        );
        assert_eq!(
            compute_mpv_subtitle_visibility_ex(
                false, true, false, true, false, false, false, true, true, false, false
            ),
            (true, true)
        );

        // Classic hide_while_playing && playing hands off to mpv when cursor-show is disabled.
        assert_eq!(
            compute_mpv_subtitle_visibility(true, true, true, true, false, false),
            (true, true)
        );
    }
}

//! Player controller connecting memento-player to Slint VideoSurface.

pub use memento_player::controller::{
    format_keypress, QT_ALT_MODIFIER, QT_CONTROL_MODIFIER, QT_KEYPAD_MODIFIER,
    QT_META_MODIFIER, QT_NO_MODIFIER, QT_SHIFT_MODIFIER,
    QT_KEY_0, QT_KEY_9, QT_KEY_ALT, QT_KEY_BACKSPACE, QT_KEY_CLEAR,
    QT_KEY_CONTROL, QT_KEY_DELETE, QT_KEY_DOWN, QT_KEY_END, QT_KEY_ENTER,
    QT_KEY_ESCAPE, QT_KEY_F1, QT_KEY_F30, QT_KEY_HOME, QT_KEY_INSERT,
    QT_KEY_LEFT, QT_KEY_META, QT_KEY_PAGE_DOWN, QT_KEY_PAGE_UP,
    QT_KEY_PAUSE, QT_KEY_RETURN, QT_KEY_RIGHT, QT_KEY_SHIFT, QT_KEY_UP,
};

/// Trait abstracting over types that can be checked for subtitle file extensions.
pub trait SubtitlePath {
    fn is_subtitle_file(&self) -> bool;
}

impl SubtitlePath for str {
    fn is_subtitle_file(&self) -> bool {
        let u16_vec: Vec<u16> = self.encode_utf16().collect();
        memento_player::controller::is_subtitle_file(&u16_vec)
    }
}

impl SubtitlePath for &str {
    fn is_subtitle_file(&self) -> bool {
        let u16_vec: Vec<u16> = self.encode_utf16().collect();
        memento_player::controller::is_subtitle_file(&u16_vec)
    }
}

impl SubtitlePath for String {
    fn is_subtitle_file(&self) -> bool {
        let u16_vec: Vec<u16> = self.encode_utf16().collect();
        memento_player::controller::is_subtitle_file(&u16_vec)
    }
}

impl SubtitlePath for [u16] {
    fn is_subtitle_file(&self) -> bool {
        memento_player::controller::is_subtitle_file(self)
    }
}

impl SubtitlePath for &[u16] {
    fn is_subtitle_file(&self) -> bool {
        memento_player::controller::is_subtitle_file(self)
    }
}

impl<const N: usize> SubtitlePath for [u16; N] {
    fn is_subtitle_file(&self) -> bool {
        memento_player::controller::is_subtitle_file(self)
    }
}

impl<const N: usize> SubtitlePath for &[u16; N] {
    fn is_subtitle_file(&self) -> bool {
        memento_player::controller::is_subtitle_file(*self)
    }
}

impl SubtitlePath for Vec<u16> {
    fn is_subtitle_file(&self) -> bool {
        memento_player::controller::is_subtitle_file(self.as_slice())
    }
}

impl SubtitlePath for std::path::Path {
    fn is_subtitle_file(&self) -> bool {
        if let Some(s) = self.to_str() {
            let u16_vec: Vec<u16> = s.encode_utf16().collect();
            memento_player::controller::is_subtitle_file(&u16_vec)
        } else {
            false
        }
    }
}

/// Checks whether a given path has a supported subtitle file extension.
pub fn is_subtitle_file<P: SubtitlePath + ?Sized>(path: &P) -> bool {
    path.is_subtitle_file()
}

/// Checks whether a UTF-16 code unit slice has a supported subtitle file extension.
pub fn is_subtitle_file_utf16(path: &[u16]) -> bool {
    memento_player::controller::is_subtitle_file(path)
}

/// Formats seconds into a `"MM:SS"` or `"HH:MM:SS"` string.
pub fn format_time(sec: f64) -> String {
    format_time_internal(sec, false)
}

fn format_time_internal(sec: f64, force_hours: bool) -> String {
    let total = if sec.is_finite() && sec > 0.0 {
        sec as u64
    } else {
        0
    };
    let h = total / 3600;
    let m = (total % 3600) / 60;
    let s = total % 60;
    if force_hours || h > 0 {
        format!("{:02}:{:02}:{:02}", h, m, s)
    } else {
        format!("{:02}:{:02}", m, s)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct PlayerController {
    playing: bool,
    position: f64,
    duration: f64,
    volume: f64,
    mute: bool,
    speed: f64,
}

impl Default for PlayerController {
    fn default() -> Self {
        Self::new()
    }
}

impl PlayerController {
    pub fn new() -> Self {
        Self {
            playing: false,
            position: 0.0,
            duration: 0.0,
            volume: 100.0,
            mute: false,
            speed: 1.0,
        }
    }

    pub fn is_playing(&self) -> bool {
        self.playing
    }

    pub fn playing(&self) -> bool {
        self.playing
    }

    pub fn set_playing(&mut self, playing: bool) {
        self.playing = playing;
    }

    pub fn toggle_play(&mut self) -> bool {
        self.playing = !self.playing;
        self.playing
    }

    pub fn position(&self) -> f64 {
        self.position
    }

    pub fn set_position(&mut self, pos: f64) {
        self.position = pos;
    }

    pub fn duration(&self) -> f64 {
        self.duration
    }

    pub fn set_duration(&mut self, dur: f64) {
        self.duration = dur;
    }

    pub fn volume(&self) -> f64 {
        self.volume
    }

    pub fn set_volume(&mut self, vol: f64) {
        self.volume = vol.clamp(0.0, 100.0);
    }

    pub fn is_muted(&self) -> bool {
        self.mute
    }

    pub fn mute(&self) -> bool {
        self.mute
    }

    pub fn set_mute(&mut self, mute: bool) {
        self.mute = mute;
    }

    pub fn toggle_mute(&mut self) -> bool {
        self.mute = !self.mute;
        self.mute
    }

    pub fn speed(&self) -> f64 {
        self.speed
    }

    pub fn set_speed(&mut self, speed: f64) {
        self.speed = speed;
    }

    pub fn format_time(sec: f64) -> String {
        format_time(sec)
    }

    pub fn time_display(&self) -> String {
        let has_hours = self.duration >= 3600.0 || self.position >= 3600.0;
        format!(
            "{} / {}",
            format_time_internal(self.position, has_hours),
            format_time_internal(self.duration, has_hours)
        )
    }

    pub fn format_keypress(
        key: i32,
        modifiers: i32,
        is_macos: bool,
        fallback: &str,
    ) -> Option<String> {
        memento_player::controller::format_keypress(key, modifiers, is_macos, fallback)
    }

    pub fn is_subtitle_file<P: SubtitlePath + ?Sized>(path: &P) -> bool {
        is_subtitle_file(path)
    }
}

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::Path;
use std::rc::Rc;

use memento_player::{
    format_modifiers, AsrCue, AsrSubtitleTrack, AutoPauseState, MouseButton, MpvEvent, MpvInstance,
    MpvPropertyValue, TrackType, WheelButton, ASR_STREAM_URI,
};
use slint::{Model as _, VecModel};

use crate::controller::asr::{AsrController, AsrEvent, AsrSettings, SessionStart};
use crate::controller::media::MediaContext;
use crate::controller::subtitles::{
    create_cue, normalize_subtitle_text, ExternalSubtitleLoad, MemorySubtitleCue,
    SubtitleController, SubtitleTrackInfo, ASR_TRACK_ID,
};
use crate::{MementoApp, PlayerTrack, SubEntry};

/// Returns true if an `mpv` subtitle track codec is bitmap/image-based rather
/// than text-based (meaning Memento's text overlay cannot render it).
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
/// Honors `Ctrl+0` (`subtitles_visible == false`), `behavior/subtitle-cursor-show`
/// reveal zones, native ASS/SSA rendering (`primary_is_native_ass`), bitmap/image
/// tracks, and `search/hide-bar` (`hide_while_playing`).
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
    let primary_visible = if primary_is_native_ass {
        if primary_cursor_show_enabled {
            primary_cursor_reveal
        } else {
            subtitles_visible
        }
    } else if !subtitles_visible && !(primary_cursor_show_enabled && primary_cursor_reveal) {
        false
    } else if primary_is_image {
        true
    } else {
        let overlay_owns_text = hide_mpv_subtitles && !(hide_while_playing && playing);
        !overlay_owns_text
    };

    let secondary_visible =
        if !subtitles_visible && !(secondary_cursor_show_enabled && secondary_cursor_reveal) {
            false
        } else if secondary_is_image {
            true
        } else {
            let overlay_owns_text = hide_mpv_subtitles && !(hide_while_playing && playing);
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

/// Apply Memento's configured `search/remove-regex`, `search/replace-lines`, and
/// `search/replace-with` text filters to subtitle text.
pub fn filter_subtitle_text(
    text: &str,
    remove_regex: &str,
    replace_lines: bool,
    replace_with: &str,
) -> String {
    crate::controller::dict::preprocess_search_text(text, remove_regex, replace_lines, replace_with)
}

/// Compute the auto-fitted logical window size `(width, height)` for a newly loaded
/// video (`behavior/autofit` and `behavior/autofit-percent`).
///
/// Returns `None` when autofit is disabled, the window is fullscreen or maximized,
/// or video dimensions are not yet known (`0`). When a side drawer is visible, its
/// width is preserved alongside the scaled video width, and the combined size is
/// uniformly clamped within `max_bounds`.
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
/// for `behavior/subtitle-cursor-show-secondary` (top 20%) and
/// `behavior/subtitle-cursor-show` (bottom 20%).
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
    (ratio <= 0.20, ratio >= 0.80)
}

/// Compute effective subtitle overlay visibility accounting for `subtitle-cursor-show`.
pub fn compute_subtitle_overlay_visibility(
    has_text: bool,
    subtitles_visible: bool,
    hide_while_playing: bool,
    is_playing: bool,
    cursor_show_enabled: bool,
    cursor_in_zone: bool,
) -> bool {
    if !has_text {
        return false;
    }
    if cursor_show_enabled {
        cursor_in_zone
    } else {
        subtitles_visible && !(hide_while_playing && is_playing)
    }
}

/// Compute whether `mpv`'s `cursor-autohide` policy wants the video cursor hidden when
/// `behavior/osc-mpv-cursor` is enabled.
pub fn mpv_cursor_should_hide(
    autohide: memento_player::mpv::CursorAutohide,
    fs_only: bool,
    is_fullscreen: bool,
    idle_elapsed_ms: u64,
    overlay_interactive: bool,
) -> bool {
    use memento_player::mpv::CursorAutohide;
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
///
/// When OpenGL is active, removes the `1.0` upscale cap so `libmpv`'s GPU
/// scalers upscale directly to physical HiDPI viewport pixels, and quantizes
/// FBO dimensions in 64px buckets while preserving exact video aspect ratio.
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
            let quant_h = ((quant_w as f64 * vh as f64 / vw as f64).round() as u32).clamp(1, 8192);
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

/// Stateful pointer/wheel forwarder from Slint's video area into `libmpv`
/// (`mouse`, `keydown`/`keyup` `MOUSE_BTN*`, and wheel buttons `3..6`) for
/// `mpv` scripts such as `uosc`.
#[derive(Debug, Default)]
pub struct MouseInputForwarder {
    last_pos: Option<(i32, i32)>,
    pressed_buttons: HashMap<u32, String>,
}

impl MouseInputForwarder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn modifiers_mask(control: bool, shift: bool, alt: bool, meta: bool) -> i32 {
        let mut mods = QT_NO_MODIFIER;
        if shift {
            mods |= QT_SHIFT_MODIFIER;
        }
        if control {
            mods |= QT_CONTROL_MODIFIER;
        }
        if alt {
            mods |= QT_ALT_MODIFIER;
        }
        if meta {
            mods |= QT_META_MODIFIER;
        }
        mods
    }

    pub fn map_to_render_coords(
        x: f32,
        y: f32,
        viewport_w: f32,
        viewport_h: f32,
        scale_factor: f32,
        render_size: (u32, u32),
    ) -> (i32, i32) {
        let (rw, rh) = render_size;
        if viewport_w > 0.0 && viewport_h > 0.0 && rw > 0 && rh > 0 {
            let fit = (viewport_w / rw as f32).min(viewport_h / rh as f32);
            if fit > 0.0 && fit.is_finite() {
                let drawn_w = rw as f32 * fit;
                let drawn_h = rh as f32 * fit;
                let off_x = (viewport_w - drawn_w) * 0.5;
                let off_y = (viewport_h - drawn_h) * 0.5;
                let mx = ((x - off_x) / fit).round().clamp(0.0, rw as f32) as i32;
                let my = ((y - off_y) / fit).round().clamp(0.0, rh as f32) as i32;
                return (mx, my);
            }
        }
        let scale = if scale_factor.is_finite() && scale_factor > 0.0 {
            scale_factor
        } else {
            1.0
        };
        ((x * scale).round() as i32, (y * scale).round() as i32)
    }

    pub fn record_move(&mut self, x: i32, y: i32) -> Option<[String; 3]> {
        if self.last_pos == Some((x, y)) {
            return None;
        }
        self.last_pos = Some((x, y));
        Some(["mouse".to_owned(), x.to_string(), y.to_string()])
    }

    pub fn send_move(&mut self, player: &MpvInstance, x: i32, y: i32) {
        if let Some(cmd) = self.record_move(x, y) {
            let _ = player.command_async(&[&cmd[0], &cmd[1], &cmd[2]]);
        }
    }

    pub fn record_button(
        &mut self,
        x: i32,
        y: i32,
        qt_button: u32,
        modifiers: i32,
        pressed: bool,
    ) -> Vec<Vec<String>> {
        let Some(button) = MouseButton::from_qt(qt_button) else {
            return Vec::new();
        };
        let mut commands = Vec::with_capacity(3);
        self.last_pos = Some((x, y));
        commands.push(vec!["mouse".to_owned(), x.to_string(), y.to_string()]);
        let key = format!("{}{}", format_modifiers(modifiers), button.as_str());
        if pressed {
            if let Some(prev_key) = self.pressed_buttons.remove(&qt_button) {
                commands.push(vec!["keyup".to_owned(), prev_key]);
            }
            self.pressed_buttons.insert(qt_button, key.clone());
            commands.push(vec!["keydown".to_owned(), key]);
        } else {
            let release_key = self.pressed_buttons.remove(&qt_button).unwrap_or(key);
            commands.push(vec!["keyup".to_owned(), release_key]);
        }
        commands
    }

    pub fn send_button(
        &mut self,
        player: &MpvInstance,
        x: i32,
        y: i32,
        qt_button: u32,
        modifiers: i32,
        pressed: bool,
    ) {
        for cmd in self.record_button(x, y, qt_button, modifiers, pressed) {
            let refs: Vec<&str> = cmd.iter().map(String::as_str).collect();
            let _ = player.command_async(&refs);
        }
    }

    pub fn record_release_all(&mut self) -> Vec<Vec<String>> {
        let mut keys: Vec<(u32, String)> = self.pressed_buttons.drain().collect();
        keys.sort_by_key(|(btn, _)| *btn);
        keys.into_iter()
            .map(|(_, key)| vec!["keyup".to_owned(), key])
            .collect()
    }

    pub fn release_all(&mut self, player: &MpvInstance) {
        for cmd in self.record_release_all() {
            let refs: Vec<&str> = cmd.iter().map(String::as_str).collect();
            let _ = player.command_async(&refs);
        }
    }

    pub fn record_wheel(&mut self, x: i32, y: i32, delta_x: f32, delta_y: f32) -> Option<[String; 4]> {
        let dx = if delta_x.abs() >= 0.5 {
            delta_x.round() as i32
        } else if delta_x != 0.0 {
            delta_x.signum() as i32
        } else {
            0
        };
        let dy = if delta_y.abs() >= 0.5 {
            delta_y.round() as i32
        } else if delta_y != 0.0 {
            delta_y.signum() as i32
        } else {
            0
        };
        let wheel = WheelButton::from_deltas(dx, dy)?;
        self.last_pos = Some((x, y));
        Some([
            "mouse".to_owned(),
            x.to_string(),
            y.to_string(),
            wheel.as_str().to_owned(),
        ])
    }

    pub fn send_wheel(&mut self, player: &MpvInstance, x: i32, y: i32, delta_x: f32, delta_y: f32) {
        if let Some(cmd) = self.record_wheel(x, y, delta_x, delta_y) {
            let _ = player.command_async(&[&cmd[0], &cmd[1], &cmd[2], &cmd[3]]);
        }
    }
}

pub fn track_label(id: i64, language: &str, title: &str) -> String {
    let mut label = format!("Track {id}");
    if !language.is_empty() {
        label.push_str(&format!(" [{language}]"));
    }
    if !title.is_empty() {
        label.push_str(" - ");
        label.push_str(title);
    }
    label
}

pub fn clear_primary_subtitle(app: &MementoApp) {
    app.set_active_sub_text("".into());
    app.set_active_sub_words(Rc::new(VecModel::from(Vec::new())).into());
    app.set_active_sub_lines(Rc::new(VecModel::from(Vec::new())).into());
    app.set_subtitle_furigana_pending(false);
    app.invoke_dismiss_definition();
}

pub fn refresh_subtitle_list(
    app: &MementoApp,
    model: &VecModel<SubEntry>,
    subtitles: &SubtitleController,
) {
    let query = app.get_subtitle_search_query();
    let ignore_whitespace = app.get_subtitle_search_ignore_whitespace();
    let remove_regex = app.get_search_remove_regex();
    let compiled_regex = memento_dict::normalize::compile_remove_regex(remove_regex.as_str());
    let matching = if query.trim().is_empty() {
        Vec::new()
    } else if compiled_regex.is_some() {
        let q_norm: String = if ignore_whitespace {
            query
                .trim()
                .chars()
                .filter(|c| !c.is_whitespace())
                .flat_map(char::to_lowercase)
                .collect()
        } else {
            query.trim().chars().flat_map(char::to_lowercase).collect()
        };
        if q_norm.is_empty() {
            Vec::new()
        } else {
            subtitles
                .primary_entries()
                .iter()
                .enumerate()
                .filter_map(|(idx, cue)| {
                    let raw = String::from_utf16_lossy(&cue.text);
                    let filtered =
                        memento_dict::normalize::apply_compiled_remove_regex(&raw, compiled_regex.as_ref());
                    let haystack: String = if ignore_whitespace {
                        filtered
                            .chars()
                            .filter(|c| !c.is_whitespace())
                            .flat_map(char::to_lowercase)
                            .collect()
                    } else {
                        filtered.chars().flat_map(char::to_lowercase).collect()
                    };
                    haystack.contains(&q_norm).then_some(idx)
                })
                .collect()
        }
    } else {
        subtitles.search(&query, ignore_whitespace)
    };
    let active = subtitles.active_indices(subtitles.primary_sid());
    let rows: Vec<SubEntry> = subtitles
        .primary_entries()
        .iter()
        .enumerate()
        .map(|(index, cue)| {
            let raw = String::from_utf16_lossy(&cue.text);
            let display =
                memento_dict::normalize::apply_compiled_remove_regex(&raw, compiled_regex.as_ref());
            SubEntry {
                id: (index + 1) as i32,
                start: cue.start as f32,
                end: cue.end as f32,
                text: display.into_owned().into(),
                selected: subtitles.is_selected(index),
                is_active: active.contains(&index),
                search_match: matching.contains(&index),
            }
        })
        .collect();
    model.set_vec(rows);
}

pub fn refresh_subtitle_marks(model: &VecModel<SubEntry>, subtitles: &SubtitleController) {
    for index in 0..model.row_count() {
        if let Some(mut row) = model.row_data(index) {
            let selected = subtitles.is_selected(index);
            if row.selected != selected {
                row.selected = selected;
                model.set_row_data(index, row);
            }
        }
    }
}

pub fn refresh_active_rows(model: &VecModel<SubEntry>, _previous: &[usize], active: &[usize]) {
    for index in 0..model.row_count() {
        if let Some(mut row) = model.row_data(index) {
            let now_active = active.contains(&index);
            if row.is_active != now_active {
                row.is_active = now_active;
                model.set_row_data(index, row);
            }
        }
    }
}

pub fn clear_subtitle_marks_on_advance(
    model: &VecModel<SubEntry>,
    subtitles: &mut SubtitleController,
    previous: &mut Option<(i64, f64)>,
    paused: bool,
) {
    let sid = subtitles.primary_sid();
    if previous.is_some_and(|(previous_sid, _)| previous_sid != sid) {
        *previous = None;
    }
    // Keep the last cue across gaps. Cue times, rather than row indices or
    // text, distinguish repeated lines without reacting to ASR insertions.
    if let Some(current) = subtitles.active_cue().map(|cue| (sid, cue.start)) {
        let advanced = previous.is_some_and(|previous| previous != current);
        *previous = Some(current);
        if advanced && !paused && !subtitles.selected_indices().is_empty() {
            subtitles.clear_selection();
            refresh_subtitle_marks(model, subtitles);
        }
    }
}

/// Synchronize `PlaybackState` audio & subtitle track lists with `MementoApp`
/// and `SubtitleController`, returning any external subtitle files that must be
/// parsed on a background worker.
pub fn sync_player_tracks(
    app: &MementoApp,
    state: &memento_player::mpv::PlaybackState,
    subtitles: &mut SubtitleController,
    subtitle_rows: &VecModel<SubEntry>,
    previous_audio: &mut Vec<memento_player::AudioTrack>,
    previous_subtitles: &mut Vec<memento_player::SubtitleTrack>,
    asr_mpv_sid: Option<i64>,
) -> Vec<ExternalSubtitleLoad> {
    let audio = &state.audio_tracks;
    if audio != previous_audio {
        let rows: Vec<PlayerTrack> = audio
            .iter()
            .filter_map(|track| {
                Some(PlayerTrack {
                    id: i32::try_from(track.id).ok()?,
                    label: track_label(track.id, &track.language, &track.title).into(),
                    selected: track.selected,
                })
            })
            .collect();
        app.set_audio_tracks(Rc::new(VecModel::from(rows)).into());
        previous_audio.clone_from(audio);
    }
    let ui_aid = i32::try_from(state.audio_id).unwrap_or(0);
    if app.get_audio_id() != ui_aid {
        app.set_audio_id(ui_aid);
    }

    let mut pending_loads = Vec::new();
    let effective_asr_sid = asr_mpv_sid.or_else(|| subtitles.asr_mpv_sid());
    let tracks = &state.subtitle_tracks;
    if tracks != previous_subtitles {
        let ui_tracks: Vec<PlayerTrack> = tracks
            .iter()
            .filter(|track| {
                track.external_path != ASR_STREAM_URI && Some(track.id) != effective_asr_sid
            })
            .filter_map(|track| {
                Some(PlayerTrack {
                    id: i32::try_from(track.id).ok()?,
                    label: track_label(track.id, &track.language, &track.title).into(),
                    selected: track.selected,
                })
            })
            .collect();
        app.set_subtitle_tracks(Rc::new(VecModel::from(ui_tracks)).into());
        let info: Vec<SubtitleTrackInfo> = tracks
            .iter()
            .filter(|track| {
                track.external_path != ASR_STREAM_URI && Some(track.id) != effective_asr_sid
            })
            .map(|track| SubtitleTrackInfo {
                id: track.id,
                external: track.external,
                external_path: track.external_path.clone(),
            })
            .collect();
        pending_loads = subtitles.sync_tracks(&info);
        previous_subtitles.clone_from(tracks);
        refresh_subtitle_list(app, subtitle_rows, subtitles);
    }

    let asr_selected = app.get_asr_track_selected() || app.get_asr_running();
    let raw_primary = (state.position - state.sub_delay) / state.sub_speed.max(f64::EPSILON);
    let primary = if asr_selected {
        effective_asr_sid
            .or_else(|| subtitles.asr_track_id())
            .unwrap_or(ASR_TRACK_ID)
    } else {
        state.primary_sid
    };
    if subtitles.primary_sid() != primary {
        if asr_selected && subtitles.has_asr_track() {
            subtitles.resume_asr_track(raw_primary);
        } else {
            subtitles.set_primary_sid(primary);
        }
        refresh_subtitle_list(app, subtitle_rows, subtitles);
    }
    let secondary = state.secondary_sid;
    if subtitles.secondary_sid() != secondary {
        subtitles.set_secondary_sid(secondary);
    }
    let ui_primary_sid = i32::try_from(state.primary_sid).unwrap_or(0);
    if app.get_primary_sid() != ui_primary_sid {
        app.set_primary_sid(ui_primary_sid);
    }
    let ui_secondary_sid = i32::try_from(secondary).unwrap_or(0);
    if app.get_secondary_sid() != ui_secondary_sid {
        app.set_secondary_sid(ui_secondary_sid);
    }
    pending_loads
}

/// Event-driven coordinator bridging `MpvInstance::drain_events()`,
/// `AsrSubtitleTrack` (`sid > 0`), `AsrController`, `SubtitleController`, and
/// `AutoPauseState`.
pub struct LivePlayerCoordinator {
    state: memento_player::mpv::PlaybackState,
    asr_track: AsrSubtitleTrack,
    auto_pause: AutoPauseState,
    pub mouse: MouseInputForwarder,
    subtitle_codecs: HashMap<i64, String>,
    previous_audio: Vec<memento_player::AudioTrack>,
    previous_tracks: Vec<memento_player::SubtitleTrack>,
    previous_lines: Vec<memento_player::mpv::SubLine>,
    previous_audio_id: i64,
    previous_primary_sid: i64,
    previous_sub_delay: f64,
    previous_sub_speed: f64,
    previous_context_cue: Option<(i64, f64)>,
    previous_primary_text: String,
    previous_primary_cue_times: (f64, f64),
    previous_secondary_text: String,
    previous_sub_times: (f64, f64),
    previous_auto_pause: bool,
    previous_primary_mpv_visible: Option<bool>,
    previous_secondary_mpv_visible: Option<bool>,
    previous_video_dimensions: (u32, u32),
    observed_media_generation: u64,
    suppress_next_start_file_increment: bool,
    tracks_known_for_file: bool,
    seek_pending_asr_notify: bool,
    autopause_seek_in_flight: bool,
    last_ui_pos: f32,
    last_ui_dur: f32,
    last_ui_vol: f32,
    last_ui_playing: Option<bool>,
    last_ui_time_sec: i64,
    last_ui_dur_sec: i64,
}

impl LivePlayerCoordinator {
    pub fn new(player: &MpvInstance) -> Self {
        let mut coord = Self::with_asr_track(AsrSubtitleTrack::new(player));
        coord.seed_from_player(player);
        coord
    }

    pub fn with_asr_track(asr_track: AsrSubtitleTrack) -> Self {
        Self {
            state: memento_player::mpv::PlaybackState {
                paused: true,
                volume: 100.0,
                volume_max: 100,
                sub_speed: 1.0,
                cursor_autohide: memento_player::mpv::CursorAutohide::Number(1000),
                ..Default::default()
            },
            asr_track,
            auto_pause: AutoPauseState::default(),
            mouse: MouseInputForwarder::new(),
            subtitle_codecs: HashMap::new(),
            previous_audio: Vec::new(),
            previous_tracks: Vec::new(),
            previous_lines: Vec::new(),
            previous_audio_id: 0,
            previous_primary_sid: 0,
            previous_sub_delay: 0.0,
            previous_sub_speed: 1.0,
            previous_context_cue: None,
            previous_primary_text: String::new(),
            previous_primary_cue_times: (0.0, 0.0),
            previous_secondary_text: String::new(),
            previous_sub_times: (0.0, 0.0),
            previous_auto_pause: false,
            previous_primary_mpv_visible: None,
            previous_secondary_mpv_visible: None,
            previous_video_dimensions: (0, 0),
            observed_media_generation: 0,
            suppress_next_start_file_increment: false,
            tracks_known_for_file: false,
            seek_pending_asr_notify: false,
            autopause_seek_in_flight: false,
            last_ui_pos: -1.0,
            last_ui_dur: -1.0,
            last_ui_vol: -1.0,
            last_ui_playing: None,
            last_ui_time_sec: -1,
            last_ui_dur_sec: -1,
        }
    }

    /// Seed initial `PlaybackState` from non-blocking `MpvInstance` snapshots
    /// without calling `poll_playback_state()` (so queued events are preserved).
    pub fn seed_from_player(&mut self, player: &MpvInstance) {
        self.state.position = player.get_position();
        self.state.duration = player.get_duration();
        self.state.paused = player.is_paused();
        self.state.volume = player.get_volume();
        self.state.dimensions = player.video_dimensions();
        self.state.audio_tracks = player.audio_tracks();
        self.state.subtitle_tracks = player.subtitle_tracks();
        self.state.sub_lines = player.primary_sub_lines();
        self.state.audio_id = player.selected_aid();
        self.state.primary_sid = player.primary_sid();
        self.state.secondary_sid = player.secondary_sid();
        self.state.sub_start = player.get_sub_start();
        self.state.sub_end = player.get_sub_end();
        self.state.sub_delay = player.get_sub_delay();
        self.state.sub_speed = player.get_sub_speed().max(f64::EPSILON);
        self.state.sub_text = normalize_subtitle_text(&player.get_sub_text().unwrap_or_default());
        self.state.secondary_sub_text =
            normalize_subtitle_text(&player.get_secondary_sub_text().unwrap_or_default());
        self.state.secondary_sub_start = player.get_secondary_sub_start();
        self.state.secondary_sub_end = player.get_secondary_sub_end();
        self.state.secondary_sub_delay = player.get_secondary_sub_delay();
        self.state.media_title = player.get_media_title().unwrap_or_default();
        if let Some(ctx) = player.capture_encode_context() {
            self.state.file_loaded = true;
            self.state.path = ctx.path;
            self.tracks_known_for_file = true;
        }
        self.previous_audio_id = self.state.audio_id;
        self.previous_primary_sid = self.state.primary_sid;
        self.previous_sub_delay = self.state.sub_delay;
        self.previous_sub_speed = self.state.sub_speed;
    }

    pub fn playback_state(&self) -> &memento_player::mpv::PlaybackState {
        &self.state
    }

    pub fn remove_asr_track(&mut self, player: Option<&MpvInstance>) {
        if let Some(player) = player {
            let _ = self.asr_track.remove_track(player);
        }
    }

    pub fn asr_track(&self) -> &AsrSubtitleTrack {
        &self.asr_track
    }

    pub fn asr_track_mut(&mut self) -> &mut AsrSubtitleTrack {
        &mut self.asr_track
    }

    /// Returns true if the active primary subtitle track is ASS/SSA and native
    /// `libmpv`/`libass` rendering (`interface/sub-ass-native`) is enabled.
    pub fn primary_is_native_ass(&self, app: &MementoApp) -> bool {
        let primary_sid = self.state.primary_sid;
        primary_sid > 0
            && app.get_sub_ass_native()
            && !app.get_asr_track_selected()
            && self
                .subtitle_codecs
                .get(&primary_sid)
                .is_some_and(|codec| crate::controller::ass::is_ass_subtitle_codec(codec))
    }

    /// Codec reported by mpv for a subtitle SID (e.g. `"ass"`, `"subrip"`, `"hdmv_pgs_subtitle"`).
    pub fn subtitle_codec(&self, sid: i64) -> Option<&str> {
        self.subtitle_codecs.get(&sid).map(String::as_str)
    }

    pub fn auto_pause_state(&self) -> &AutoPauseState {
        &self.auto_pause
    }

    pub fn reset_auto_pause(&mut self) {
        self.auto_pause = AutoPauseState::default();
    }

    /// Synchronously update primary `sid` tracking when the user explicitly selects
    /// a non-ASR subtitle track (or `"None"`) from Memento's UI menu, preventing a
    /// queued intermediate `track-list` event from suspending a subsequent rapid
    /// ASR toggle-on.
    pub fn on_manual_primary_sid(&mut self, sid: i64) {
        self.state.primary_sid = sid;
        self.previous_primary_sid = sid;
        self.auto_pause = AutoPauseState::default();
        self.previous_sub_times = (0.0, 0.0);
    }

    /// Mark that the caller initiated an explicit `loadfile` and already bumped
    /// `media_generation`, so the resulting `MpvEvent::StartFile` resets media
    /// state without double-incrementing `media_generation`.
    pub fn mark_explicit_media_load(&mut self, generation: u64) {
        self.observed_media_generation = generation;
        self.suppress_next_start_file_increment = true;
    }

    /// Record an explicit user seek (`seek_to`, `seek_relative`, `sub_seek`) so
    /// `AsrController` immediately reprioritizes and wakes from `Paused`/`Done`.
    pub fn notify_user_seek(
        &mut self,
        asr: &mut AsrController,
        target_pos: Option<f64>,
        duration: f64,
    ) {
        self.autopause_seek_in_flight = false;
        self.seek_pending_asr_notify = true;
        self.auto_pause.reset = true;
        if let Some(pos) = target_pos {
            asr.notify_seek(pos, duration);
        }
    }

    /// Determine language tag for `AsrSubtitleTrack::start` from the selected
    /// audio track, falling back to `"ja"`.
    pub fn selected_audio_language(player: &MpvInstance) -> String {
        let aid = player.selected_aid();
        player
            .audio_tracks()
            .into_iter()
            .find(|t| t.id == aid && !t.language.trim().is_empty())
            .map(|t| t.language)
            .unwrap_or_else(|| "ja".to_owned())
    }

    /// Activate or toggle the native `libmpv` ASR subtitle track (`sid > 0`).
    pub fn toggle_or_start_asr(
        &mut self,
        app: &MementoApp,
        player: &MpvInstance,
        asr: &mut AsrController,
        subtitles: &mut SubtitleController,
        sub_model: &VecModel<SubEntry>,
        asr_restart_pending: &Cell<bool>,
        settings_provider: impl FnOnce() -> Result<AsrSettings, String>,
    ) {
        if app.get_asr_track_selected() {
            asr.suspend();
            asr_restart_pending.set(false);
            let _ = player.set_sid(0);
            self.state.primary_sid = 0;
            self.previous_primary_sid = 0;
            self.auto_pause = AutoPauseState::default();
            self.previous_sub_times = (0.0, 0.0);
            app.set_asr_track_selected(false);
            app.set_asr_running(false);
            app.set_primary_sid(0);
            subtitles.set_primary_sid(0);
            refresh_subtitle_list(app, sub_model, subtitles);
            clear_primary_subtitle(app);
            app.set_status_message("ASR paused; subtitles and model kept in memory".into());
            return;
        }
        let dur = self
            .state
            .duration
            .max(player.get_duration())
            .max(app.get_duration() as f64);
        if dur <= 0.0 {
            app.set_status_message("Open a video before starting ASR subtitles".into());
            return;
        }
        let path = if !self.state.path.is_empty() {
            self.state.path.clone()
        } else if let Some(ctx) = player.capture_encode_context() {
            ctx.path
        } else {
            String::new()
        };
        let aid = if self.state.file_loaded {
            self.state.audio_id
        } else {
            player.selected_aid()
        };
        if path.is_empty() || aid <= 0 {
            app.set_status_message("No media loaded or no audio track selected".into());
            return;
        }
        let settings = match settings_provider() {
            Ok(s) => s,
            Err(err) => {
                app.set_status_message(err.into());
                return;
            }
        };
        let gpu_unavailable = settings.backend == crate::controller::asr::AsrBackend::Whisper
            && settings.whisper.use_gpu
            && settings.whisper.gpu_device.max(0) as usize
                >= crate::controller::asr::whisper_gpu_device_count();
        let media = MediaContext::new(path, aid);
        let pos = player.get_position();
        match asr.start(settings, media, dur, pos) {
            Ok(session) => {
                let lang = Self::selected_audio_language(player);
                let track_res = match session {
                    SessionStart::New => {
                        if player.primary_sid() > 0 || self.state.primary_sid > 0 {
                            let _ = player.set_sid(0);
                        }
                        self.asr_track.start(player, "ASR Subtitles", &lang)
                    }
                    SessionStart::Resumed => {
                        if self.asr_track.is_active() {
                            self.asr_track.select(player)
                        } else {
                            if player.primary_sid() > 0 || self.state.primary_sid > 0 {
                                let _ = player.set_sid(0);
                            }
                            let res = self.asr_track.start(player, "ASR Subtitles", &lang);
                            if res.is_ok() {
                                for cue in asr.current_cues() {
                                    let _ = self.asr_track.add_cue(
                                        player,
                                        AsrCue {
                                            id: cue.id as i64,
                                            start: cue.start,
                                            end: cue.end,
                                            text: cue.text,
                                        },
                                    );
                                }
                            }
                            res
                        }
                    }
                };
                if let Err(error) = track_res {
                    if session == SessionStart::Resumed {
                        asr.suspend();
                    } else {
                        asr.cancel();
                    }
                    app.set_status_message(
                        format!("Could not select ASR subtitles: {error}").into(),
                    );
                    return;
                }
                match session {
                    SessionStart::New => subtitles.start_asr_track(),
                    SessionStart::Resumed => {
                        subtitles.resume_asr_track(player.get_sub_raw_time(pos));
                    }
                }
                if let Some(sid) = self.asr_track.sid() {
                    subtitles.bind_asr_sid(sid);
                }
                asr_restart_pending.set(false);
                app.set_asr_track_selected(true);
                app.set_asr_running(true);
                refresh_subtitle_list(app, sub_model, subtitles);
                app.set_status_message(
                    if session == SessionStart::Resumed {
                        "ASR resumed with saved subtitles"
                    } else if gpu_unavailable {
                        "Whisper GPU unavailable; transcribing on CPU. Check GPU driver and CUDA/Vulkan build."
                    } else {
                        "ASR subtitles started…"
                    }
                    .into(),
                );
            }
            Err(err) if err.contains("still stopping") => {
                let _ = self.asr_track.remove_track(player);
                subtitles.clear_asr_track();
                refresh_subtitle_list(app, sub_model, subtitles);
                app.set_asr_track_selected(true);
                app.set_asr_running(false);
                asr_restart_pending.set(true);
                app.set_status_message(
                    "Reloading ASR for the current media and settings…".into(),
                );
            }
            Err(err) => {
                app.set_status_message(err.into());
            }
        }
    }

    fn reset_for_new_media(
        &mut self,
        app: &MementoApp,
        player: &MpvInstance,
        asr: &mut AsrController,
        subtitles: &mut SubtitleController,
        sub_model: &VecModel<SubEntry>,
        asr_restart_pending: &Cell<bool>,
        hover_pause_owned: &Cell<bool>,
        preserve_asr_selection: bool,
    ) {
        let should_restart_asr =
            preserve_asr_selection && app.get_asr_track_selected() && app.get_asr_enabled();
        if should_restart_asr {
            asr.suspend();
        } else {
            asr.cancel();
        }
        if preserve_asr_selection {
            self.asr_track = AsrSubtitleTrack::new(player);
            player.set_asr_stream_cues(&[]);
        } else {
            let _ = self.asr_track.remove_track(player);
        }
        app.set_asr_running(false);
        app.set_asr_track_selected(should_restart_asr);
        asr_restart_pending.set(should_restart_asr);

        self.state.file_loaded = false;
        self.state.playback_ready = false;
        self.tracks_known_for_file = false;
        self.state.audio_id = 0;
        self.state.primary_sid = 0;
        self.state.secondary_sid = 0;
        self.state.video_id = 0;
        self.state.sub_text.clear();
        self.state.sub_ass_extradata.clear();
        self.state.sub_text_ass_full.clear();
        self.state.secondary_sub_text.clear();
        self.state.sub_start = 0.0;
        self.state.sub_end = 0.0;
        self.state.secondary_sub_start = 0.0;
        self.state.secondary_sub_end = 0.0;
        self.state.audio_tracks.clear();
        self.state.subtitle_tracks.clear();
        self.state.sub_lines.clear();

        self.subtitle_codecs.clear();
        self.previous_audio.clear();
        self.previous_tracks.clear();
        self.previous_lines.clear();
        self.previous_audio_id = 0;
        self.previous_primary_sid = 0;
        self.previous_sub_delay = 0.0;
        self.previous_sub_speed = 1.0;
        self.previous_context_cue = None;
        self.previous_primary_text.clear();
        self.previous_primary_cue_times = (0.0, 0.0);
        self.previous_secondary_text.clear();
        self.previous_sub_times = (0.0, 0.0);
        self.previous_primary_mpv_visible = None;
        self.previous_secondary_mpv_visible = None;
        self.seek_pending_asr_notify = false;
        self.autopause_seek_in_flight = false;
        self.auto_pause = AutoPauseState::default();
        self.mouse.release_all(player);
        hover_pause_owned.set(false);

        subtitles.clear();
        sub_model.set_vec(Vec::new());
        app.set_subtitle_search_query("".into());
        app.set_audio_tracks(Rc::new(VecModel::<PlayerTrack>::default()).into());
        app.set_subtitle_tracks(Rc::new(VecModel::<PlayerTrack>::default()).into());
        app.set_audio_id(0);
        app.set_primary_sid(0);
        app.set_secondary_sid(0);
        app.set_secondary_sub_text("".into());
        clear_primary_subtitle(app);
    }

    /// Drain all queued `MpvEvent`s in arrival order and `AsrController` deltas,
    /// updating UI models, native `AsrSubtitleTrack`, `AutoPauseState`, and
    /// `libmpv` subtitle visibility. Returns any external subtitle tracks to
    /// parse asynchronously.
    ///
    /// Never calls `poll_playback_state()`, guaranteeing zero dropped `CommandReply`
    /// or lifecycle events.
    #[allow(clippy::too_many_arguments)]
    pub fn sync(
        &mut self,
        app: &MementoApp,
        player: &MpvInstance,
        subtitles: &mut SubtitleController,
        sub_model: &VecModel<SubEntry>,
        asr: &mut AsrController,
        media_generation: &Cell<u64>,
        current_media: &RefCell<Option<String>>,
        asr_restart_pending: &Cell<bool>,
        hover_pause_owned: &Cell<bool>,
        asr_settings_provider: impl Fn() -> Result<AsrSettings, String>,
        mut on_publish_primary_subtitle: impl FnMut(&MementoApp, &str),
    ) -> Vec<ExternalSubtitleLoad> {
        let events = player.drain_events();

        // Check if external caller (e.g. Close Media) bumped media_generation without StartFile.
        let current_gen = media_generation.get();
        if self.observed_media_generation != current_gen && !self.suppress_next_start_file_increment {
            self.observed_media_generation = current_gen;
            self.reset_for_new_media(
                app,
                player,
                asr,
                subtitles,
                sub_model,
                asr_restart_pending,
                hover_pause_owned,
                false,
            );
        }

        let mut sub_times_dirty = false;
        let mut saw_tracks_event = false;
        let mut saw_title_or_path_event = false;

        let auto_pause_enabled = app.get_auto_pause();
        if auto_pause_enabled != self.previous_auto_pause {
            self.previous_auto_pause = auto_pause_enabled;
            self.auto_pause = AutoPauseState::default();
            if auto_pause_enabled
                && !self.state.sub_text.is_empty()
                && self.state.sub_end > self.state.sub_start
            {
                let _ = self.auto_pause.step(
                    true,
                    self.state.sub_start,
                    self.state.sub_end,
                    self.state.sub_speed.max(f64::EPSILON),
                    self.state.sub_delay,
                    self.state.paused,
                    self.state.duration,
                );
                self.previous_sub_times = (self.state.sub_start, self.state.sub_end);
            }
        }

        let flush_autopause = |auto_pause: &mut AutoPauseState,
                               previous_times: &mut (f64, f64),
                               autopause_seek_in_flight: &mut bool,
                               dirty: &mut bool,
                               s_start: f64,
                               s_end: f64,
                               s_speed: f64,
                               s_delay: f64,
                               paused: bool,
                               dur: f64| {
            if !*dirty {
                return;
            }
            // Wait if sub-start arrived ahead of sub-end in a split update.
            if (s_start, s_end) != (0.0, 0.0) && s_end <= s_start {
                return;
            }
            *dirty = false;
            let times = (s_start, s_end);
            if times != *previous_times {
                *previous_times = times;
                if let Some(seek_time) = auto_pause.step(
                    auto_pause_enabled,
                    times.0,
                    times.1,
                    s_speed,
                    s_delay,
                    paused,
                    dur,
                ) {
                    *autopause_seek_in_flight = true;
                    let _ = player.pause();
                    let _ = player.seek(seek_time);
                }
            }
        };

        for event in &events {
            match self.asr_track.on_event(player, event) {
                Ok(Some(new_sid)) => {
                    subtitles.bind_asr_sid(new_sid);
                    if app.get_asr_track_selected() {
                        subtitles.set_primary_sid(new_sid);
                        refresh_subtitle_list(app, sub_model, subtitles);
                    }
                }
                Ok(None) => {}
                Err(error) => {
                    app.set_status_message(error.into());
                }
            }

            match event {
                MpvEvent::StartFile { .. } => {
                    sub_times_dirty = false;
                    if self.suppress_next_start_file_increment {
                        self.suppress_next_start_file_increment = false;
                    } else {
                        let next = media_generation.get().wrapping_add(1);
                        media_generation.set(next);
                        self.observed_media_generation = next;
                    }
                    self.reset_for_new_media(
                        app,
                        player,
                        asr,
                        subtitles,
                        sub_model,
                        asr_restart_pending,
                        hover_pause_owned,
                        true,
                    );
                    saw_title_or_path_event = true;
                }
                MpvEvent::FileLoaded {
                    video_width,
                    video_height,
                } => {
                    self.state.file_loaded = true;
                    self.state.video_width = *video_width;
                    self.state.video_height = *video_height;
                    sub_times_dirty = false;
                    self.auto_pause = AutoPauseState::default();
                    self.previous_sub_times = (0.0, 0.0);
                    saw_tracks_event = true;
                    saw_title_or_path_event = true;
                }
                MpvEvent::EndFile { .. } => {
                    self.state.file_loaded = false;
                    self.state.playback_ready = false;
                    self.tracks_known_for_file = false;
                    self.state.audio_id = 0;
                    self.state.primary_sid = 0;
                    self.state.secondary_sid = 0;
                    self.state.sub_ass_extradata.clear();
                    self.state.sub_text_ass_full.clear();
                    sub_times_dirty = false;
                    self.auto_pause = AutoPauseState::default();
                }
                MpvEvent::Seek => {
                    self.auto_pause.reset = true;
                    if self.autopause_seek_in_flight {
                        self.autopause_seek_in_flight = false;
                    } else if self.state.file_loaded && self.state.duration > 0.0 {
                        self.seek_pending_asr_notify = true;
                        asr.notify_seek(self.state.position, self.state.duration);
                    }
                }
                MpvEvent::PlaybackRestart => {
                    self.state.playback_ready = true;
                    if self.seek_pending_asr_notify {
                        self.seek_pending_asr_notify = false;
                        asr.notify_seek(self.state.position, self.state.duration);
                    }
                }
                MpvEvent::PropertyChange { name, value } => match name.as_str() {
                    "path" => {
                        self.state.path = match value {
                            MpvPropertyValue::String(s) => s.clone(),
                            _ => String::new(),
                        };
                        saw_title_or_path_event = true;
                    }
                    "media-title" => {
                        self.state.media_title = match value {
                            MpvPropertyValue::String(s) => s.clone(),
                            _ => String::new(),
                        };
                        saw_title_or_path_event = true;
                    }
                    "time-pos" => {
                        let (eff_sub_start, eff_sub_end) = if self.state.sub_text.is_empty() {
                            (0.0, 0.0)
                        } else {
                            (self.state.sub_start, self.state.sub_end)
                        };
                        flush_autopause(
                            &mut self.auto_pause,
                            &mut self.previous_sub_times,
                            &mut self.autopause_seek_in_flight,
                            &mut sub_times_dirty,
                            eff_sub_start,
                            eff_sub_end,
                            self.state.sub_speed.max(f64::EPSILON),
                            self.state.sub_delay,
                            self.state.paused,
                            self.state.duration,
                        );
                        if let MpvPropertyValue::Double(v) = value {
                            self.state.position = v.max(0.0);
                        } else if let MpvPropertyValue::Int64(v) = value {
                            self.state.position = (*v as f64).max(0.0);
                        }
                        if self.seek_pending_asr_notify {
                            self.seek_pending_asr_notify = false;
                            asr.notify_seek(self.state.position, self.state.duration);
                        }
                        self.auto_pause.update_position(
                            self.state.position,
                            self.state.sub_speed.max(f64::EPSILON),
                            self.state.sub_delay,
                        );
                    }
                    "duration" => {
                        self.state.duration = match value {
                            MpvPropertyValue::Double(v) => v.max(0.0),
                            MpvPropertyValue::Int64(v) => (*v as f64).max(0.0),
                            _ => 0.0,
                        };
                    }
                    "pause" => {
                        if let MpvPropertyValue::Flag(p) = value {
                            self.state.paused = *p;
                        }
                    }
                    "fullscreen" => {
                        if let MpvPropertyValue::Flag(f) = value {
                            self.state.fullscreen = *f;
                        }
                    }
                    "mute" => {
                        if let MpvPropertyValue::Flag(m) = value {
                            self.state.mute = *m;
                        }
                    }
                    "volume" => {
                        self.state.volume = match value {
                            MpvPropertyValue::Double(v) => *v,
                            MpvPropertyValue::Int64(v) => *v as f64,
                            _ => 100.0,
                        };
                    }
                    "volume-max" => {
                        self.state.volume_max = match value {
                            MpvPropertyValue::Int64(v) => *v,
                            MpvPropertyValue::Double(v) => v.round() as i64,
                            _ => 100,
                        };
                    }
                    "dwidth" => {
                        self.state.dimensions.0 = match value {
                            MpvPropertyValue::Int64(v) => u32::try_from(*v).unwrap_or(0),
                            _ => 0,
                        };
                    }
                    "dheight" => {
                        self.state.dimensions.1 = match value {
                            MpvPropertyValue::Int64(v) => u32::try_from(*v).unwrap_or(0),
                            _ => 0,
                        };
                    }
                    "video-params/w" => {
                        self.state.video_width = match value {
                            MpvPropertyValue::Int64(v) => *v,
                            _ => 0,
                        };
                    }
                    "video-params/h" => {
                        self.state.video_height = match value {
                            MpvPropertyValue::Int64(v) => *v,
                            _ => 0,
                        };
                    }
                    "chapter-list" => {
                        self.state.chapters = match value {
                            MpvPropertyValue::Chapters(c) => c.clone(),
                            _ => Vec::new(),
                        };
                    }
                    "frame-drop-count" => {
                        self.state.frame_drops = match value {
                            MpvPropertyValue::Int64(v) => *v,
                            _ => 0,
                        };
                    }
                    "decoder-frame-drop-count" => {
                        self.state.decoder_drops = match value {
                            MpvPropertyValue::Int64(v) => *v,
                            _ => 0,
                        };
                    }
                    "hwdec-current" => {
                        self.state.hwdec_current = match value {
                            MpvPropertyValue::String(s) => s.clone(),
                            _ => String::new(),
                        };
                    }
                    "aid" => match value {
                        MpvPropertyValue::Int64(v) => self.state.audio_id = *v,
                        MpvPropertyValue::Flag(false) => self.state.audio_id = 0,
                        _ => {}
                    },
                    "secondary-sid" => match value {
                        MpvPropertyValue::Int64(v) => self.state.secondary_sid = *v,
                        MpvPropertyValue::Flag(false) => self.state.secondary_sid = 0,
                        _ => {}
                    },
                    "vid" => match value {
                        MpvPropertyValue::Int64(v) => self.state.video_id = *v,
                        MpvPropertyValue::Flag(false) => self.state.video_id = 0,
                        _ => {}
                    },
                    "track-list" => {
                        saw_tracks_event = true;
                        if let MpvPropertyValue::Tracks {
                            audio,
                            subtitle,
                            all,
                        } = value
                        {
                            if self.state.file_loaded || !all.is_empty() {
                                self.tracks_known_for_file = true;
                            }
                            self.state.audio_tracks = audio.clone();
                            self.state.subtitle_tracks = subtitle.clone();
                            self.state.primary_sid = all
                                .iter()
                                .find(|t| {
                                    t.track_type == TrackType::Subtitle
                                        && t.selected
                                        && t.main_selection == 0
                                })
                                .map_or(0, |t| t.id);
                            self.state.secondary_sid = all
                                .iter()
                                .find(|t| {
                                    t.track_type == TrackType::Subtitle
                                        && t.selected
                                        && t.main_selection == 1
                                })
                                .map_or(0, |t| t.id);
                            self.state.audio_id = all
                                .iter()
                                .find(|t| {
                                    t.track_type == TrackType::Audio
                                        && t.selected
                                        && t.main_selection == 0
                                })
                                .map_or(0, |t| t.id);
                            self.state.video_id = all
                                .iter()
                                .find(|t| {
                                    t.track_type == TrackType::Video
                                        && t.selected
                                        && t.main_selection == 0
                                })
                                .map_or(0, |t| t.id);
                            self.subtitle_codecs = all
                                .iter()
                                .filter(|t| t.track_type == TrackType::Subtitle && t.id > 0)
                                .map(|t| (t.id, t.codec.clone()))
                                .collect();
                            if let Some(stream_track) = subtitle.iter().rfind(|t| {
                                t.external && t.external_path == ASR_STREAM_URI && t.id > 0
                            }) {
                                subtitles.bind_asr_sid(stream_track.id);
                                if app.get_asr_track_selected() {
                                    if stream_track.selected {
                                        self.previous_primary_sid = stream_track.id;
                                    } else if self.previous_primary_sid == stream_track.id {
                                        asr.suspend();
                                        app.set_asr_running(false);
                                        app.set_asr_track_selected(false);
                                        self.previous_primary_sid = self.state.primary_sid;
                                        self.auto_pause = AutoPauseState::default();
                                        self.previous_sub_times = (0.0, 0.0);
                                        clear_primary_subtitle(app);
                                    }
                                }
                            } else if let Some(asr_sid) = self.asr_track.sid() {
                                if let Some(mem_track) =
                                    subtitle.iter().find(|t| t.id == asr_sid)
                                {
                                    if app.get_asr_track_selected() {
                                        if mem_track.selected {
                                            self.previous_primary_sid = asr_sid;
                                        } else if self.previous_primary_sid == asr_sid {
                                            asr.suspend();
                                            app.set_asr_running(false);
                                            app.set_asr_track_selected(false);
                                            self.previous_primary_sid = self.state.primary_sid;
                                            self.auto_pause = AutoPauseState::default();
                                            self.previous_sub_times = (0.0, 0.0);
                                            clear_primary_subtitle(app);
                                        }
                                    }
                                }
                            }
                        } else {
                            self.state.audio_tracks.clear();
                            self.state.subtitle_tracks.clear();
                            self.state.primary_sid = 0;
                            self.state.secondary_sid = 0;
                            self.state.audio_id = 0;
                            self.state.video_id = 0;
                            self.subtitle_codecs.clear();
                        }
                    }
                    "sid" => {
                        if let Some(new_sid) = match value {
                            MpvPropertyValue::Int64(v) => Some(*v),
                            MpvPropertyValue::Flag(false) => Some(0),
                            _ => None,
                        } {
                            self.state.primary_sid = new_sid;
                            let asr_sid =
                                self.asr_track.sid().or_else(|| subtitles.asr_mpv_sid());
                            // Ignore transient `sid -> 0` demuxer reopen bounces during `sub-reload`
                            // while ASR is active; genuine `sid=no` deselection is handled via `track-list`.
                            let is_transient_asr_reload_zero = new_sid == 0
                                && app.get_asr_track_selected()
                                && asr_sid.is_some();
                            if !is_transient_asr_reload_zero
                                && new_sid != self.previous_primary_sid
                            {
                                if asr_sid.is_some() && Some(new_sid) == asr_sid {
                                    if !app.get_asr_track_selected() {
                                        app.set_asr_track_selected(true);
                                    }
                                } else if new_sid > 0
                                    && app.get_asr_track_selected()
                                    && asr_sid.is_some()
                                    && Some(new_sid) != asr_sid
                                {
                                    // User switched to a different subtitle track via mpv keybindings.
                                    asr.suspend();
                                    app.set_asr_running(false);
                                    app.set_asr_track_selected(false);
                                    clear_primary_subtitle(app);
                                }
                                self.previous_primary_sid = new_sid;
                                self.auto_pause = AutoPauseState::default();
                                self.previous_sub_times = (0.0, 0.0);
                                self.state.sub_start = 0.0;
                                self.state.sub_end = 0.0;
                                self.state.sub_text_ass_full.clear();
                                if new_sid == 0 {
                                    self.state.sub_ass_extradata.clear();
                                }
                                sub_times_dirty = false;
                            }
                        }
                    }
                    "sub-delay" => {
                        let d = match value {
                            MpvPropertyValue::Double(v) => *v,
                            MpvPropertyValue::Int64(v) => *v as f64,
                            _ => 0.0,
                        };
                        self.state.sub_delay = d;
                        if (d - self.previous_sub_delay).abs() > 1e-9 {
                            self.previous_sub_delay = d;
                            self.auto_pause = AutoPauseState::default();
                        }
                    }
                    "sub-speed" => {
                        let s = match value {
                            MpvPropertyValue::Double(v) => v.max(f64::EPSILON),
                            MpvPropertyValue::Int64(v) => (*v as f64).max(f64::EPSILON),
                            _ => 1.0,
                        };
                        self.state.sub_speed = s;
                        if (s - self.previous_sub_speed).abs() > 1e-9 {
                            self.previous_sub_speed = s;
                            self.auto_pause = AutoPauseState::default();
                        }
                    }
                    "secondary-sub-delay" => {
                        self.state.secondary_sub_delay = match value {
                            MpvPropertyValue::Double(v) => *v,
                            MpvPropertyValue::Int64(v) => *v as f64,
                            _ => 0.0,
                        };
                    }
                    "sub-lines" => {
                        self.state.sub_lines = match value {
                            MpvPropertyValue::SubLines(lines) => lines.clone(),
                            _ => Vec::new(),
                        };
                    }
                    "sub-start" => {
                        match value {
                            MpvPropertyValue::Double(v) => self.state.sub_start = *v,
                            MpvPropertyValue::Int64(v) => self.state.sub_start = *v as f64,
                            MpvPropertyValue::None => {
                                self.state.sub_start = 0.0;
                                self.state.sub_end = 0.0;
                            }
                            _ => {}
                        }
                        sub_times_dirty = true;
                    }
                    "sub-end" => {
                        match value {
                            MpvPropertyValue::Double(v) => self.state.sub_end = *v,
                            MpvPropertyValue::Int64(v) => self.state.sub_end = *v as f64,
                            MpvPropertyValue::None => {
                                self.state.sub_start = 0.0;
                                self.state.sub_end = 0.0;
                            }
                            _ => {}
                        }
                        sub_times_dirty = true;
                    }
                    "sub-text" => {
                        let raw_text = match value {
                            MpvPropertyValue::String(s) => s.clone(),
                            MpvPropertyValue::None => String::new(),
                            _ => player.get_sub_text().unwrap_or_default(),
                        };
                        let text = normalize_subtitle_text(&raw_text);
                        if text.is_empty() {
                            self.state.sub_text.clear();
                            self.state.sub_start = 0.0;
                            self.state.sub_end = 0.0;
                        } else {
                            self.state.sub_text = text;
                        }
                        sub_times_dirty = true;
                    }
                    "sub-ass-extradata" => {
                        self.state.sub_ass_extradata = match value {
                            MpvPropertyValue::String(s) => s.clone(),
                            _ => String::new(),
                        };
                    }
                    "sub-text/ass-full" => {
                        self.state.sub_text_ass_full = match value {
                            MpvPropertyValue::String(s) => s.clone(),
                            _ => String::new(),
                        };
                    }
                    "osd-dimensions" => {
                        if let MpvPropertyValue::OsdDimensions(dims) = value {
                            self.state.osd_dimensions = *dims;
                        }
                    }
                    "sub-scale" => {
                        let v = match value {
                            MpvPropertyValue::Double(d) => *d,
                            MpvPropertyValue::Int64(i) => *i as f64,
                            _ => 1.0,
                        };
                        self.state.sub_scale = v.max(0.05);
                    }
                    "sub-pos" => {
                        let v = match value {
                            MpvPropertyValue::Double(d) => *d,
                            MpvPropertyValue::Int64(i) => *i as f64,
                            _ => 100.0,
                        };
                        self.state.sub_pos = v.clamp(0.0, 150.0);
                    }
                    "sub-font" => {
                        if let MpvPropertyValue::String(s) = value {
                            if !s.is_empty() {
                                self.state.sub_font = s.clone();
                            }
                        }
                    }
                    "sub-ass-override" => {
                        if let MpvPropertyValue::String(s) = value {
                            if !s.is_empty() {
                                self.state.sub_ass_override = s.clone();
                            }
                        }
                    }
                    "sub-scale-signs" => {
                        if let MpvPropertyValue::Flag(v) = value {
                            self.state.sub_scale_signs = *v;
                        }
                    }
                    "sub-use-margins" => {
                        if let MpvPropertyValue::Flag(v) = value {
                            self.state.sub_use_margins = *v;
                        }
                    }
                    "sub-hinting" => {
                        if let MpvPropertyValue::String(s) = value {
                            if !s.is_empty() {
                                self.state.sub_hinting = s.clone();
                            }
                        }
                    }
                    "sub-shaper" => {
                        if let MpvPropertyValue::String(s) = value {
                            if !s.is_empty() {
                                self.state.sub_shaper = s.clone();
                            }
                        }
                    }
                    "secondary-sub-start" => {
                        match value {
                            MpvPropertyValue::Double(v) => self.state.secondary_sub_start = *v,
                            MpvPropertyValue::Int64(v) => self.state.secondary_sub_start = *v as f64,
                            MpvPropertyValue::None => {
                                self.state.secondary_sub_start = 0.0;
                                self.state.secondary_sub_end = 0.0;
                            }
                            _ => {}
                        }
                    }
                    "secondary-sub-end" => {
                        match value {
                            MpvPropertyValue::Double(v) => self.state.secondary_sub_end = *v,
                            MpvPropertyValue::Int64(v) => self.state.secondary_sub_end = *v as f64,
                            MpvPropertyValue::None => {
                                self.state.secondary_sub_start = 0.0;
                                self.state.secondary_sub_end = 0.0;
                            }
                            _ => {}
                        }
                    }
                    "secondary-sub-text" => {
                        let raw_text = match value {
                            MpvPropertyValue::String(s) => s.clone(),
                            MpvPropertyValue::None => String::new(),
                            _ => player.get_secondary_sub_text().unwrap_or_default(),
                        };
                        let text = normalize_subtitle_text(&raw_text);
                        if text.is_empty() {
                            self.state.secondary_sub_text.clear();
                            self.state.secondary_sub_start = 0.0;
                            self.state.secondary_sub_end = 0.0;
                        } else {
                            self.state.secondary_sub_text = text;
                        }
                    }
                    "sub-visibility" => {
                        if let MpvPropertyValue::Flag(v) = value {
                            self.state.sub_visibility = *v;
                        }
                    }
                    "secondary-sub-visibility" => {
                        if let MpvPropertyValue::Flag(v) = value {
                            self.state.secondary_sub_visibility = *v;
                        }
                    }
                    "cursor-autohide" => {
                        if let MpvPropertyValue::CursorAutohide(v) = value {
                            self.state.cursor_autohide = *v;
                        }
                    }
                    "cursor-autohide-fs-only" => {
                        if let MpvPropertyValue::Flag(v) = value {
                            self.state.cursor_autohide_fs_only = *v;
                        }
                    }
                    _ => {}
                },
                _ => {}
            }
        }

        // Flush any trailing subtitle transition at the end of the drained event batch.
        let (eff_sub_start, eff_sub_end) = if self.state.sub_text.is_empty() {
            (0.0, 0.0)
        } else {
            (self.state.sub_start, self.state.sub_end)
        };
        if (eff_sub_start, eff_sub_end) != self.previous_sub_times {
            sub_times_dirty = true;
        }
        flush_autopause(
            &mut self.auto_pause,
            &mut self.previous_sub_times,
            &mut self.autopause_seek_in_flight,
            &mut sub_times_dirty,
            eff_sub_start,
            eff_sub_end,
            self.state.sub_speed.max(f64::EPSILON),
            self.state.sub_delay,
            self.state.paused,
            self.state.duration,
        );

        let pos = self.state.position;
        let duration = self.state.duration;
        let paused = self.state.paused;
        let raw_primary = (pos - self.state.sub_delay) / self.state.sub_speed.max(f64::EPSILON);
        let raw_secondary =
            (pos - self.state.secondary_sub_delay) / self.state.sub_speed.max(f64::EPSILON);

        if saw_title_or_path_event
            || (!self.state.path.is_empty() && app.get_media_title().is_empty())
        {
            if !self.state.path.is_empty() {
                *current_media.borrow_mut() = Some(self.state.path.clone());
            }
            let title = effective_media_title(&self.state.media_title, &self.state.path);
            if app.get_media_title().as_str() != title {
                app.set_media_title(title.into());
            }
        }

        // Handle audio track (`aid`) changes for active or selected ASR sessions.
        if self.state.file_loaded && self.state.audio_id != self.previous_audio_id {
            let had_prev_aid = self.previous_audio_id != 0;
            self.previous_audio_id = self.state.audio_id;
            if (asr.is_active() || app.get_asr_track_selected() || subtitles.has_asr_track())
                && had_prev_aid
            {
                let _ = self.asr_track.clear(player);
                subtitles.clear_asr_cues(raw_primary);
                if self.state.audio_id <= 0 {
                    asr.notify_audio_track_changed(0);
                    asr_restart_pending.set(false);
                    let _ = self.asr_track.remove_track(player);
                    subtitles.clear_asr_track();
                    refresh_subtitle_list(app, sub_model, subtitles);
                    if app.get_asr_track_selected() {
                        clear_primary_subtitle(app);
                        app.set_asr_track_selected(false);
                        app.set_status_message("Audio track disabled; ASR stopped".into());
                    }
                    app.set_asr_running(false);
                } else if asr.is_active() {
                    asr.notify_audio_track_changed(self.state.audio_id);
                    if app.get_asr_track_selected() {
                        refresh_subtitle_list(app, sub_model, subtitles);
                        clear_primary_subtitle(app);
                        app.set_asr_running(true);
                    }
                } else if app.get_asr_track_selected() {
                    refresh_subtitle_list(app, sub_model, subtitles);
                    clear_primary_subtitle(app);
                    app.set_asr_running(false);
                    asr_restart_pending.set(true);
                }
            }
        }

        // Handle deferred ASR restart (settings reload or playlist/file transition).
        if asr_restart_pending.get() {
            if !app.get_asr_enabled() {
                asr_restart_pending.set(false);
                app.set_asr_track_selected(false);
                app.set_asr_running(false);
            } else if self.state.file_loaded
                && duration > 0.0
                && self.state.audio_id > 0
                && !self.state.path.is_empty()
            {
                match asr_settings_provider() {
                    Ok(settings) => {
                        let media = MediaContext::new(self.state.path.clone(), self.state.audio_id);
                        match asr.start(settings, media, duration, pos) {
                            Ok(session) => {
                                asr_restart_pending.set(false);
                                if self.asr_track.sid().is_none()
                                    && (player.primary_sid() > 0 || self.state.primary_sid > 0)
                                {
                                    let _ = player.set_sid(0);
                                }
                                let lang = Self::selected_audio_language(player);
                                match self.asr_track.start(player, "ASR Subtitles", &lang) {
                                    Ok(()) => {
                                        match session {
                                            SessionStart::New => subtitles.start_asr_track(),
                                            SessionStart::Resumed => {
                                                subtitles.resume_asr_track(raw_primary);
                                            }
                                        }
                                        if let Some(sid) = self.asr_track.sid() {
                                            subtitles.bind_asr_sid(sid);
                                        }
                                        refresh_subtitle_list(app, sub_model, subtitles);
                                        app.set_asr_track_selected(true);
                                        app.set_asr_running(true);
                                        app.set_status_message(
                                            "ASR transcribing subtitles…".into(),
                                        );
                                    }
                                    Err(error) => {
                                        asr.cancel();
                                        app.set_asr_track_selected(false);
                                        app.set_asr_running(false);
                                        app.set_status_message(
                                            format!("Could not restart ASR subtitles: {error}")
                                                .into(),
                                        );
                                    }
                                }
                            }
                            Err(error) if error.contains("still stopping") => {}
                            Err(error) => {
                                asr_restart_pending.set(false);
                                app.set_asr_track_selected(false);
                                app.set_asr_running(false);
                                app.set_status_message(
                                    format!("Could not restart ASR subtitles: {error}").into(),
                                );
                            }
                        }
                    }
                    Err(error) => {
                        asr_restart_pending.set(false);
                        app.set_asr_track_selected(false);
                        app.set_asr_running(false);
                        app.set_status_message(
                            format!("Could not reload ASR model: {error}").into(),
                        );
                    }
                }
            } else if self.state.file_loaded
                && duration > 0.0
                && !self.state.path.is_empty()
                && self.state.audio_id <= 0
                && (self.tracks_known_for_file || self.state.playback_ready)
            {
                asr_restart_pending.set(false);
                asr.suspend();
                app.set_asr_track_selected(false);
                app.set_asr_running(false);
                app.set_status_message("No active audio track; ASR paused".into());
            }
        }

        // Change-gated UI playback properties (avoids redundant property dirtying when paused).
        let pos_f32 = pos as f32;
        if (pos_f32 - self.last_ui_pos).abs() > 1e-3 {
            self.last_ui_pos = pos_f32;
            app.set_position(pos_f32);
        }
        let dur_f32 = duration as f32;
        if (dur_f32 - self.last_ui_dur).abs() > 1e-3 {
            self.last_ui_dur = dur_f32;
            app.set_duration(dur_f32);
        }
        let ui_playing = !paused || app.get_pending_playback_generation() >= 0;
        if self.last_ui_playing != Some(ui_playing) {
            self.last_ui_playing = Some(ui_playing);
            app.set_is_playing(ui_playing);
        }
        let ui_vol = if self.state.mute {
            0.0
        } else {
            self.state.volume as f32
        };
        if (ui_vol - self.last_ui_vol).abs() > 0.05 {
            self.last_ui_vol = ui_vol;
            app.set_volume(ui_vol);
        }
        let pos_sec = pos.max(0.0) as i64;
        if pos_sec != self.last_ui_time_sec {
            self.last_ui_time_sec = pos_sec;
            app.set_current_time(format_time(pos).into());
        }
        let dur_sec = duration.max(0.0) as i64;
        if dur_sec != self.last_ui_dur_sec {
            self.last_ui_dur_sec = dur_sec;
            app.set_duration_time(format_time(duration).into());
        }
        if self.previous_video_dimensions != self.state.dimensions {
            self.previous_video_dimensions = self.state.dimensions;
            app.invoke_update_video();
        }
        if !paused && player.pending_render_delay() > std::time::Duration::from_millis(100) {
            app.invoke_video_frame_ready();
        }

        let mut external_loads = Vec::new();
        if saw_tracks_event
            || self.state.audio_tracks != self.previous_audio
            || self.state.subtitle_tracks != self.previous_tracks
            || subtitles.primary_sid()
                != (if app.get_asr_track_selected() {
                    subtitles
                        .asr_mpv_sid()
                        .or_else(|| self.asr_track.sid())
                        .or_else(|| subtitles.asr_track_id())
                        .unwrap_or(ASR_TRACK_ID)
                } else {
                    self.state.primary_sid
                })
            || subtitles.secondary_sid() != self.state.secondary_sid
        {
            external_loads = sync_player_tracks(
                app,
                &self.state,
                subtitles,
                sub_model,
                &mut self.previous_audio,
                &mut self.previous_tracks,
                subtitles.asr_mpv_sid().or_else(|| self.asr_track.sid()),
            );
        }

        let primary_sid = self.state.primary_sid;
        let secondary_sid = self.state.secondary_sid;
        if primary_sid > 0 {
            subtitles.ensure_track(primary_sid);
        }
        if secondary_sid > 0 {
            subtitles.ensure_track(secondary_sid);
        }

        let current_lines = &self.state.sub_lines;
        if current_lines != &self.previous_lines {
            let cues: Vec<MemorySubtitleCue> = current_lines
                .iter()
                .map(|line| MemorySubtitleCue {
                    id: line.id,
                    text: line.text.encode_utf16().collect(),
                    start: line.start,
                    end: line.end,
                })
                .collect();
            if subtitles.update_memory_cues(primary_sid, &cues, raw_primary) {
                refresh_subtitle_list(app, sub_model, subtitles);
            }
            self.previous_lines.clone_from(current_lines);
        }

        // Primary subtitle events come natively from libmpv for BOTH standard
        // tracks and the live ASR subtitle track (sid > 0) — zero event skipping!
        let remove_regex = app.get_search_remove_regex();
        let replace_lines = app.get_search_replace_lines();
        let replace_with = app.get_search_replace_with();
        let primary_text = self.state.sub_text.clone();
        let display_primary = filter_subtitle_text(
            &primary_text,
            remove_regex.as_str(),
            replace_lines,
            replace_with.as_str(),
        );
        let primary_times = (self.state.sub_start, self.state.sub_end);
        if primary_text != self.previous_primary_text
            || primary_times != self.previous_primary_cue_times
            || display_primary != app.get_active_sub_text().as_str()
        {
            app.invoke_dismiss_definition();
            let previous_count = subtitles.primary_entries().len();
            let cue = (!primary_text.is_empty() && primary_times.1 > primary_times.0)
                .then(|| create_cue(&primary_text, primary_times.0, primary_times.1));
            if primary_text.is_empty() || primary_times.1 > primary_times.0 {
                subtitles.update_active_cue(primary_sid, cue.as_ref(), raw_primary);
                if subtitles.primary_entries().len() != previous_count {
                    refresh_subtitle_list(app, sub_model, subtitles);
                }
            }
            on_publish_primary_subtitle(app, &display_primary);
            self.previous_primary_text = primary_text;
            self.previous_primary_cue_times = primary_times;
        }

        let secondary_text = self.state.secondary_sub_text.clone();
        let display_secondary = filter_subtitle_text(
            &secondary_text,
            remove_regex.as_str(),
            replace_lines,
            replace_with.as_str(),
        );
        if secondary_text != self.previous_secondary_text
            || display_secondary != app.get_secondary_sub_text().as_str()
        {
            let cue = (!secondary_text.is_empty()
                && self.state.secondary_sub_end > self.state.secondary_sub_start)
                .then(|| {
                    create_cue(
                        &secondary_text,
                        self.state.secondary_sub_start,
                        self.state.secondary_sub_end,
                    )
                });
            if secondary_text.is_empty()
                || self.state.secondary_sub_end > self.state.secondary_sub_start
            {
                subtitles.update_active_cue(secondary_sid, cue.as_ref(), raw_secondary);
            }
            app.set_secondary_sub_text(display_secondary.into());
            self.previous_secondary_text = secondary_text;
        }

        // Drain background ASR worker events and feed both AsrSubtitleTrack (libmpv)
        // and SubtitleController (sidebar drawer & context selection).
        if !asr_restart_pending.get() {
            asr.update_position(pos, duration, paused);
            let mut refreshed_subtitles = false;
            asr.poll_with(|event| {
                if self.handle_asr_event(app, player, subtitles, raw_primary, event) {
                    refreshed_subtitles = true;
                }
            });
            if refreshed_subtitles && app.get_asr_track_selected() {
                refresh_subtitle_list(app, sub_model, subtitles);
            }
        }

        let selected_sid = subtitles.primary_sid();
        let before_active = subtitles.active_indices(selected_sid).to_vec();
        subtitles.update_positions(raw_primary, raw_secondary);
        clear_subtitle_marks_on_advance(sub_model, subtitles, &mut self.previous_context_cue, paused);
        refresh_active_rows(sub_model, &before_active, subtitles.active_indices(selected_sid));

        if !app.get_hover_pause_enabled() && hover_pause_owned.replace(false) {
            let _ = player.play();
        }

        // Update mpv subtitle visibility without flashing during cue gaps,
        // preserving native ASS/SSA rendering, bitmap secondary tracks, Ctrl+0,
        // and cursor-show reveal zones.
        let primary_is_image = primary_sid > 0
            && !app.get_asr_track_selected()
            && self
                .subtitle_codecs
                .get(&primary_sid)
                .is_some_and(|codec| is_image_subtitle_codec(codec));
        let secondary_is_image = secondary_sid > 0
            && self
                .subtitle_codecs
                .get(&secondary_sid)
                .is_some_and(|codec| is_image_subtitle_codec(codec));
        let primary_is_native_ass = self.primary_is_native_ass(app);
        let (want_primary_vis, want_secondary_vis) = compute_mpv_subtitle_visibility_ex(
            app.get_hide_mpv_subtitles(),
            app.get_subtitles_visible(),
            app.get_hide_memento_while_playing(),
            !paused,
            primary_is_image,
            secondary_is_image,
            primary_is_native_ass,
            app.get_subtitle_cursor_show_enabled(),
            app.get_primary_cursor_reveal_active(),
            app.get_secondary_subtitle_cursor_show_enabled(),
            app.get_secondary_cursor_reveal_active(),
        );
        if self.previous_primary_mpv_visible != Some(want_primary_vis) {
            let vis = if want_primary_vis { "yes" } else { "no" };
            if let Err(err) = player.command_async(&["set", "sub-visibility", vis]) {
                eprintln!("Could not update mpv sub-visibility: {err}");
            } else {
                self.previous_primary_mpv_visible = Some(want_primary_vis);
            }
        }
        if self.previous_secondary_mpv_visible != Some(want_secondary_vis) {
            let vis = if want_secondary_vis { "yes" } else { "no" };
            if let Err(err) = player.command_async(&["set", "secondary-sub-visibility", vis]) {
                eprintln!("Could not update mpv secondary-sub-visibility: {err}");
            } else {
                self.previous_secondary_mpv_visible = Some(want_secondary_vis);
            }
        }

        external_loads
    }

    /// Process a single [`AsrEvent`] against both the live `libmpv` [`AsrSubtitleTrack`]
    /// (`sid > 0`) and [`SubtitleController`], returning `true` if the subtitle list
    /// changed and should be refreshed when ASR is selected.
    pub fn handle_asr_event(
        &mut self,
        app: &MementoApp,
        player: &MpvInstance,
        subtitles: &mut SubtitleController,
        raw_primary: f64,
        event: AsrEvent,
    ) -> bool {
        let mut refreshed = false;
        match event {
            AsrEvent::Started { .. } | AsrEvent::WindowStarted { .. } => {
                if app.get_asr_track_selected() && !app.get_asr_running() {
                    app.set_asr_running(true);
                }
            }
            AsrEvent::CueAdded { cue, .. } => {
                let _ = self.asr_track.add_cue(
                    player,
                    AsrCue {
                        id: cue.id as i64,
                        start: cue.start,
                        end: cue.end,
                        text: cue.text.clone(),
                    },
                );
                let mem_cue = MemorySubtitleCue {
                    id: cue.id as i64,
                    text: cue.text.encode_utf16().collect(),
                    start: cue.start,
                    end: cue.end,
                };
                if subtitles.add_asr_cue(mem_cue, raw_primary) {
                    refreshed = true;
                }
            }
            AsrEvent::CueRemoved { cue_id, .. } => {
                let _ = self.asr_track.remove_cue(player, cue_id as i64);
                if subtitles.remove_asr_cue(cue_id as i64, raw_primary) {
                    refreshed = true;
                }
            }
            AsrEvent::CuesSnapshot { cues, .. } => {
                if cues.is_empty() {
                    let _ = self.asr_track.clear(player);
                    subtitles.clear_asr_cues(raw_primary);
                    refreshed = true;
                } else {
                    let _ = self.asr_track.clear(player);
                    for cue in &cues {
                        let _ = self.asr_track.add_cue(
                            player,
                            AsrCue {
                                id: cue.id as i64,
                                start: cue.start,
                                end: cue.end,
                                text: cue.text.clone(),
                            },
                        );
                    }
                    let mem_cues = cues
                        .into_iter()
                        .map(|c| MemorySubtitleCue {
                            id: c.id as i64,
                            text: c.text.encode_utf16().collect(),
                            start: c.start,
                            end: c.end,
                        })
                        .collect();
                    if subtitles.set_asr_cues_snapshot(mem_cues, raw_primary) {
                        refreshed = true;
                    }
                }
            }
            AsrEvent::Completed { cue_count, .. } => {
                app.set_asr_running(false);
                if app.get_asr_track_selected() {
                    app.set_status_message(
                        format!("ASR transcription completed ({cue_count} cues)").into(),
                    );
                }
            }
            AsrEvent::Paused { message, .. } => {
                app.set_asr_running(false);
                app.set_status_message(format!("ASR paused: {message}").into());
            }
            AsrEvent::Failed { message, .. } => {
                app.set_asr_running(false);
                app.set_status_message(format!("ASR error: {message}").into());
            }
            AsrEvent::Canceled { .. } => {
                app.set_asr_running(false);
            }
        }
        refreshed
    }
}

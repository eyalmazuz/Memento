//! Window chrome, monitor bounds probing, fullscreen synchronization,
//! OSC auto-hide timer, pointer/mouse forwarding, and chapter marks.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

use slint::{ComponentHandle, Model as _};

use crate::controller::player::{
    mpv_cursor_should_hide, subtitle_cursor_zones, LivePlayerCoordinator, MouseInputForwarder,
};
use crate::platform_renderer::{PlatformRendererConfig, RendererOverrides, TargetOs};
use crate::platform_window::{FullscreenAction, PlatformFullscreenController};
use crate::ui::{ChapterMark, MementoApp};
use memento_player::MpvInstance;

#[cfg(target_os = "windows")]
pub(crate) fn native_window_hwnd(window: &slint::Window) -> Option<isize> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let handle = window.window_handle();
    let Ok(win) = handle.window_handle() else {
        return None;
    };
    match win.as_raw() {
        RawWindowHandle::Win32(win32) => Some(win32.hwnd.get()),
        _ => None,
    }
}

pub(crate) fn native_window_display_pointers(
    window: &slint::Window,
) -> (Option<*mut std::ffi::c_void>, Option<*mut std::ffi::c_void>) {
    use raw_window_handle::{HasDisplayHandle, RawDisplayHandle};
    let handle = window.window_handle();
    let Ok(display) = handle.display_handle() else {
        return (None, None);
    };
    match display.as_raw() {
        RawDisplayHandle::Xlib(xlib) => (xlib.display.map(|p| p.as_ptr()), None),
        RawDisplayHandle::Wayland(wl) => (None, Some(wl.display.as_ptr())),
        _ => (None, None),
    }
}

#[cfg(target_os = "linux")]
fn query_x11_screen_size(window: &slint::Window) -> Option<(u32, u32)> {
    let (Some(dpy), _) = native_window_display_pointers(window) else {
        return None;
    };
    unsafe {
        let handle = libc::dlopen(c"libX11.so.6".as_ptr(), libc::RTLD_LAZY | libc::RTLD_NOLOAD);
        if handle.is_null() {
            return None;
        }
        let default_screen_sym = libc::dlsym(handle, c"XDefaultScreen".as_ptr());
        let display_width_sym = libc::dlsym(handle, c"XDisplayWidth".as_ptr());
        let display_height_sym = libc::dlsym(handle, c"XDisplayHeight".as_ptr());
        let result = if !default_screen_sym.is_null()
            && !display_width_sym.is_null()
            && !display_height_sym.is_null()
        {
            let x_default_screen: unsafe extern "C" fn(*mut std::ffi::c_void) -> libc::c_int =
                std::mem::transmute(default_screen_sym);
            let x_display_width: unsafe extern "C" fn(
                *mut std::ffi::c_void,
                libc::c_int,
            ) -> libc::c_int = std::mem::transmute(display_width_sym);
            let x_display_height: unsafe extern "C" fn(
                *mut std::ffi::c_void,
                libc::c_int,
            ) -> libc::c_int = std::mem::transmute(display_height_sym);
            let screen = x_default_screen(dpy);
            let w = x_display_width(dpy, screen);
            let h = x_display_height(dpy, screen);
            (w > 0 && h > 0).then_some((w as u32, h as u32))
        } else {
            None
        };
        libc::dlclose(handle);
        result
    }
}

#[cfg(target_os = "linux")]
fn parse_drm_mode_dimensions(modes_contents: &str) -> Option<(u32, u32)> {
    let first = modes_contents.lines().next()?.trim();
    let (w_str, h_str) = first.split_once('x')?;
    let w = w_str.parse::<u32>().ok()?;
    let h = h_str
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect::<String>()
        .parse::<u32>()
        .ok()?;
    (w > 0 && h > 0).then_some((w, h))
}

#[cfg(target_os = "linux")]
fn query_drm_monitor_size() -> Option<(u32, u32)> {
    let entries = std::fs::read_dir("/sys/class/drm").ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !name.starts_with("card") || !name.contains('-') {
            continue;
        }
        let status = std::fs::read_to_string(path.join("status")).unwrap_or_default();
        if status.trim() != "connected" {
            continue;
        }
        let enabled = std::fs::read_to_string(path.join("enabled")).unwrap_or_default();
        if !enabled.trim().is_empty() && enabled.trim() != "enabled" {
            continue;
        }
        if let Ok(modes) = std::fs::read_to_string(path.join("modes")) {
            if let Some(dims) = parse_drm_mode_dimensions(&modes) {
                return Some(dims);
            }
        }
    }
    None
}

#[cfg(target_os = "macos")]
fn query_macos_main_display_size() -> Option<(u32, u32)> {
    unsafe {
        let main_id_sym = libc::dlsym(libc::RTLD_DEFAULT, c"CGMainDisplayID".as_ptr());
        let width_sym = libc::dlsym(libc::RTLD_DEFAULT, c"CGDisplayPixelsWide".as_ptr());
        let height_sym = libc::dlsym(libc::RTLD_DEFAULT, c"CGDisplayPixelsHigh".as_ptr());
        if main_id_sym.is_null() || width_sym.is_null() || height_sym.is_null() {
            return None;
        }
        let cg_main_display_id: unsafe extern "C" fn() -> u32 = std::mem::transmute(main_id_sym);
        let cg_display_pixels_wide: unsafe extern "C" fn(u32) -> usize =
            std::mem::transmute(width_sym);
        let cg_display_pixels_high: unsafe extern "C" fn(u32) -> usize =
            std::mem::transmute(height_sym);
        let id = cg_main_display_id();
        let w = cg_display_pixels_wide(id);
        let h = cg_display_pixels_high(id);
        (w > 0 && h > 0).then_some((w as u32, h as u32))
    }
}

/// Probe the current monitor's physical pixel bounds (`MonitorRect`), falling back
/// cleanly to the current window bounds when running headless or on Wayland without DRM info.
pub(crate) fn query_window_monitor_physical(window: &slint::Window) -> memento_os::MonitorRect {
    let pos = window.position();
    let size = window.size();
    let scale = f64::from(window.scale_factor().max(0.5));

    #[cfg(target_os = "windows")]
    if let Some(monitor) =
        native_window_hwnd(window).and_then(|hwnd| memento_os::query_window_monitor(hwnd).ok())
    {
        return monitor;
    }

    #[cfg(target_os = "linux")]
    if let Some((w, h)) = query_x11_screen_size(window).or_else(query_drm_monitor_size) {
        return memento_os::MonitorRect::new(0, 0, w, h, scale);
    }

    #[cfg(target_os = "macos")]
    if let Some((w, h)) = query_macos_main_display_size() {
        return memento_os::MonitorRect::new(0, 0, w, h, scale);
    }

    memento_os::MonitorRect::new(pos.x, pos.y, size.width.max(1), size.height.max(1), scale)
}

/// Probe the current monitor's logical bounds (`MonitorRect`) for Slint `LogicalSize` auto-fit.
pub(crate) fn query_window_monitor_logical(window: &slint::Window) -> memento_os::MonitorRect {
    let phys = query_window_monitor_physical(window);
    let scale = phys.sanitized_scale();
    // On macOS, `CGDisplayPixelsWide`/`High` already report logical points; on Windows/Linux
    // monitor bounds are physical pixels and must be divided by `scale`.
    #[cfg(target_os = "macos")]
    let (lw, lh, lx, ly) = (phys.width, phys.height, phys.x, phys.y);
    #[cfg(not(target_os = "macos"))]
    let (lw, lh, lx, ly) = (
        ((phys.width as f64) / scale).round().max(1.0) as u32,
        ((phys.height as f64) / scale).round().max(1.0) as u32,
        ((phys.x as f64) / scale).round() as i32,
        ((phys.y as f64) / scale).round() as i32,
    );
    memento_os::MonitorRect::new(lx, ly, lw, lh, scale)
}

pub(crate) fn chapter_marks_for_duration(chapters: &[f64], duration: f64) -> Vec<ChapterMark> {
    if duration <= 0.0 {
        return Vec::new();
    }
    chapters
        .iter()
        .enumerate()
        .filter(|(_, &time)| time >= 0.0 && time <= duration)
        .map(|(index, &time)| ChapterMark {
            time: time as f32,
            title: format!("Chapter {}", index + 1).into(),
        })
        .collect()
}

pub(crate) fn chapter_title_at_time(app: &MementoApp, time_sec: f32) -> String {
    let marks = app.get_chapter_marks();
    let mut best_title = String::new();
    let mut best_time = -1.0f32;
    for i in 0..marks.row_count() {
        if let Some(mark) = marks.row_data(i) {
            if mark.time <= time_sec + 0.05 && mark.time >= best_time {
                best_time = mark.time;
                best_title = if mark.title.is_empty() {
                    format!("Chapter {}", i + 1)
                } else {
                    mark.title.to_string()
                };
            }
        }
    }
    if !best_title.is_empty() {
        return best_title;
    }
    let chapters = app.get_chapters();
    for i in (0..chapters.row_count()).rev() {
        if let Some(ch_time) = chapters.row_data(i) {
            if ch_time <= time_sec + 0.05 {
                return format!("Chapter {}", i + 1);
            }
        }
    }
    String::new()
}

pub(crate) struct WindowChromeHandles {
    pub(crate) osc_timer: Rc<slint::Timer>,
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn install_window_chrome_handlers(
    app: &MementoApp,
    mpv: Rc<RefCell<Option<MpvInstance>>>,
    coordinator: Rc<RefCell<Option<LivePlayerCoordinator>>>,
    current_render_size: Rc<Cell<(u32, u32)>>,
    renderer_plan: &PlatformRendererConfig,
    renderer_overrides: &RendererOverrides,
) -> WindowChromeHandles {
    // The video surface is a Slint window, so mpv's fullscreen key cannot
    // fullscreen it. Keep the OSC timer and fullscreen cursor auto-hide in the UI layer.
    let last_pointer_activity = Rc::new(Cell::new(Instant::now()));
    let osc_timer = Rc::new(slint::Timer::default());

    let arm_osc_timer: Rc<dyn Fn(&MementoApp)> = {
        let timer = osc_timer.clone();
        let coord_rc = coordinator.clone();
        let activity = last_pointer_activity.clone();
        Rc::new(move |app: &MementoApp| {
            let duration_ms = app.get_osc_duration_ms().max(50) as u64;
            let use_mpv_cursor = app.get_osc_mpv_cursor();
            let mode = if use_mpv_cursor {
                slint::TimerMode::Repeated
            } else {
                slint::TimerMode::SingleShot
            };
            let timer_app = app.as_weak();
            let timer_coord = coord_rc.clone();
            let timer_activity = activity.clone();
            timer.start(mode, Duration::from_millis(duration_ms), move || {
                let Some(app) = timer_app.upgrade() else {
                    return;
                };
                let win_fs = app.window().is_fullscreen();
                let fullscreen = win_fs || app.get_is_fullscreen();
                if win_fs && !app.get_is_fullscreen() {
                    app.set_is_fullscreen(true);
                }
                let vis_mode = app.get_osc_visibility_mode();
                if vis_mode == 1 {
                    app.set_osc_visible(true);
                } else if vis_mode == 2 {
                    app.set_osc_visible(false);
                } else if app.get_duration() > 0.0
                    && !app.get_menu_open()
                    && !app.get_dict_open()
                    && !app.get_osc_hovered()
                {
                    app.set_osc_visible(false);
                }
                let overlay_interactive = app.get_menu_open()
                    || app.get_dict_open()
                    || app.get_osc_hovered()
                    || app.get_ocr_mode()
                    || app.get_about_open()
                    || app.get_url_dialog_open()
                    || app.get_update_dialog_open();
                if app.get_osc_mpv_cursor() {
                    let hide = timer_coord
                        .try_borrow()
                        .ok()
                        .and_then(|guard| {
                            guard.as_ref().map(|coord| {
                                let st = coord.playback_state();
                                let elapsed_ms = timer_activity.get().elapsed().as_millis() as u64;
                                mpv_cursor_should_hide(
                                    st.cursor_autohide,
                                    st.cursor_autohide_fs_only,
                                    fullscreen,
                                    elapsed_ms,
                                    overlay_interactive,
                                )
                            })
                        })
                        .unwrap_or(false);
                    if app.get_cursor_hidden() != hide {
                        app.set_cursor_hidden(hide);
                    }
                } else if fullscreen && !overlay_interactive {
                    app.set_cursor_hidden(true);
                }
            });
        })
    };

    arm_osc_timer(app);

    let pointer_app = app.as_weak();
    let pointer_arm_osc = arm_osc_timer.clone();
    let pointer_mpv = mpv.clone();
    let pointer_coord = coordinator.clone();
    let pointer_render_size = current_render_size.clone();
    let pointer_activity = last_pointer_activity.clone();
    let last_pointer = Rc::new(Cell::new(None::<(f32, f32)>));
    app.on_pointer_moved(move |x, y| {
        let Some(app) = pointer_app.upgrade() else {
            return;
        };
        let (in_top, in_bottom) = subtitle_cursor_zones(y, app.get_video_height() as f32);
        if app.get_cursor_in_top_sub_zone() != in_top {
            app.set_cursor_in_top_sub_zone(in_top);
        }
        if app.get_cursor_in_bottom_sub_zone() != in_bottom {
            app.set_cursor_in_bottom_sub_zone(in_bottom);
        }
        if !app.get_dict_open()
            && !app.get_menu_open()
            && !app.get_ocr_mode()
            && !app.get_about_open()
            && !app.get_url_dialog_open()
            && !app.get_update_dialog_open()
        {
            if let (Ok(player_ref), Ok(mut coord_ref)) =
                (pointer_mpv.try_borrow(), pointer_coord.try_borrow_mut())
            {
                if let (Some(player), Some(coord)) = (player_ref.as_ref(), coord_ref.as_mut()) {
                    let (mx, my) = MouseInputForwarder::map_to_render_coords(
                        x,
                        y,
                        app.get_video_width() as f32,
                        app.get_video_height() as f32,
                        app.window().scale_factor(),
                        pointer_render_size.get(),
                    );
                    coord.mouse.send_move(player, mx, my);
                }
            }
        }
        let minimum = app.get_osc_min_move().max(0) as f32;
        let last = last_pointer.get();
        if last.is_some_and(|(old_x, old_y)| (x - old_x).hypot(y - old_y) <= minimum) {
            return;
        }
        last_pointer.set(Some((x, y)));
        pointer_activity.set(Instant::now());
        if app.get_cursor_hidden() {
            app.set_cursor_hidden(false);
        }
        if app.get_osc_visibility_mode() != 2 {
            app.set_osc_visible(true);
        }
        pointer_arm_osc(&app);
    });

    let button_app = app.as_weak();
    let button_mpv = mpv.clone();
    let button_coord = coordinator.clone();
    let button_render_size = current_render_size.clone();
    app.on_video_pointer_button(move |x, y, qt_button, pressed, control, shift, alt, meta| {
        let Some(app) = button_app.upgrade() else {
            return;
        };
        let (Ok(player_ref), Ok(mut coord_ref)) =
            (button_mpv.try_borrow(), button_coord.try_borrow_mut())
        else {
            return;
        };
        let (Some(player), Some(coord)) = (player_ref.as_ref(), coord_ref.as_mut()) else {
            return;
        };
        if qt_button <= 0 {
            coord.mouse.release_all(player);
            return;
        }
        let (mx, my) = MouseInputForwarder::map_to_render_coords(
            x,
            y,
            app.get_video_width() as f32,
            app.get_video_height() as f32,
            app.window().scale_factor(),
            button_render_size.get(),
        );
        let modifiers = MouseInputForwarder::modifiers_mask(control, shift, alt, meta);
        coord
            .mouse
            .send_button(player, mx, my, qt_button as u32, modifiers, pressed);
    });

    let wheel_app = app.as_weak();
    let wheel_mpv = mpv;
    let wheel_coord = coordinator;
    let wheel_render_size = current_render_size;
    app.on_video_wheel(move |x, y, delta_x, delta_y| {
        let Some(app) = wheel_app.upgrade() else {
            return;
        };
        let (Ok(player_ref), Ok(mut coord_ref)) =
            (wheel_mpv.try_borrow(), wheel_coord.try_borrow_mut())
        else {
            return;
        };
        let (Some(player), Some(coord)) = (player_ref.as_ref(), coord_ref.as_mut()) else {
            return;
        };
        let (mx, my) = MouseInputForwarder::map_to_render_coords(
            x,
            y,
            app.get_video_width() as f32,
            app.get_video_height() as f32,
            app.window().scale_factor(),
            wheel_render_size.get(),
        );
        coord.mouse.send_wheel(player, mx, my, delta_x, delta_y);
    });

    let activity_app = app.as_weak();
    let activity_arm_osc = arm_osc_timer;
    let activity_pointer_ts = last_pointer_activity;
    app.on_osc_activity(move || {
        let Some(app) = activity_app.upgrade() else {
            return;
        };
        activity_pointer_ts.set(Instant::now());
        if app.get_cursor_hidden() {
            app.set_cursor_hidden(false);
        }
        if app.get_osc_visibility_mode() != 2 {
            app.set_osc_visible(true);
        }
        activity_arm_osc(&app);
    });

    let weak_fs = app.as_weak();
    let fullscreen_ctrl = Rc::new(RefCell::new(PlatformFullscreenController::for_platform(
        TargetOs::current(),
        renderer_plan.primary,
        renderer_overrides.disable_windows_borderless_workaround,
    )));
    #[cfg(target_os = "windows")]
    let win32_fs_snapshot: Rc<RefCell<Option<memento_os::Win32WindowStyleSnapshot>>> =
        Rc::new(RefCell::new(None));
    app.on_toggle_fullscreen(move || {
        let Some(app) = weak_fs.upgrade() else {
            return;
        };
        let mut fs = fullscreen_ctrl.borrow_mut();
        // Sync directly from the OS window's fullscreen state so external WM exits
        // (e.g. Alt+F11 / compositor shortcut) never leave `fs` stuck thinking it's fullscreen.
        fs.sync_external_native_fullscreen(app.window().is_fullscreen());
        let pos = app.window().position();
        let size = app.window().size();
        let current_rect = memento_os::WindowRect::new(pos.x, pos.y, size.width, size.height);
        let currently_maximized = app.window().is_maximized();
        let monitor = query_window_monitor_physical(app.window());

        let action = fs.toggle_fullscreen(current_rect, currently_maximized, monitor);
        let next = match &action {
            FullscreenAction::NoOp => fs.is_fullscreen(),
            FullscreenAction::SetNativeFullscreen { fullscreen } => {
                app.window().set_fullscreen(*fullscreen);
                app.set_window_bottom_margin(0.0);
                *fullscreen
            }
            FullscreenAction::EnterWindowsBorderless {
                bottom_margin_px, ..
            } => {
                #[cfg(target_os = "windows")]
                if let Some(hwnd) = native_window_hwnd(app.window()) {
                    let mut snap = win32_fs_snapshot.borrow_mut();
                    if let Err(err) = crate::platform_window::apply_windows_fullscreen_action(
                        hwnd, &action, &mut snap,
                    ) {
                        eprintln!("Windows borderless fullscreen enter failed: {err}");
                        app.window().set_fullscreen(true);
                    }
                } else {
                    app.window().set_fullscreen(true);
                }
                #[cfg(not(target_os = "windows"))]
                app.window().set_fullscreen(true);
                app.set_window_bottom_margin(*bottom_margin_px as f32);
                true
            }
            FullscreenAction::ExitWindowsBorderless {
                bottom_margin_px, ..
            } => {
                #[cfg(target_os = "windows")]
                if let Some(hwnd) = native_window_hwnd(app.window()) {
                    let mut snap = win32_fs_snapshot.borrow_mut();
                    if let Err(err) = crate::platform_window::apply_windows_fullscreen_action(
                        hwnd, &action, &mut snap,
                    ) {
                        eprintln!("Windows borderless fullscreen exit failed: {err}");
                        app.window().set_fullscreen(false);
                    }
                } else {
                    app.window().set_fullscreen(false);
                }
                #[cfg(not(target_os = "windows"))]
                app.window().set_fullscreen(false);
                app.set_window_bottom_margin(*bottom_margin_px as f32);
                false
            }
        };
        app.set_is_fullscreen(next);
        app.set_cursor_hidden(false);
        if app.get_osc_visibility_mode() != 2 {
            app.set_osc_visible(true);
        }
        app.invoke_osc_activity();
    });

    WindowChromeHandles { osc_timer }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::preferences::{format_recent_file_label, refresh_player_preferences};
    use crate::controller::config::ConfigController;
    use crate::ui::{subtitle_lines_for_app, SubtitleWord};
    use slint::platform::{PointerEventButton, WindowEvent};
    use slint::VecModel;

    #[test]
    fn yomitan_popup_anchors_to_hovered_word_clamps_above_or_below_primary_subtitles_and_secondary_stays_at_top(
    ) {
        i_slint_backend_testing::init_no_event_loop();
        let app = MementoApp::new().unwrap();
        app.window().set_size(slint::LogicalSize::new(1280., 720.));
        app.set_subtitles_visible(true);
        app.set_active_sub_text("生前に世界中を旅行して集めた遺品が".into());
        app.set_secondary_sub_text("Top secondary subtitle line".into());
        let words = vec![
            SubtitleWord {
                text: "生前".into(),
                reading: "せいぜん".into(),
                query: "生前".into(),
                index: 0,
            },
            SubtitleWord {
                text: "に".into(),
                reading: "".into(),
                query: "に".into(),
                index: 1,
            },
            SubtitleWord {
                text: "世界".into(),
                reading: "せかい".into(),
                query: "世界".into(),
                index: 2,
            },
        ];
        let lines = subtitle_lines_for_app(&app, words.clone());
        app.set_active_sub_words(Rc::new(VecModel::from(words)).into());
        app.set_active_sub_lines(Rc::new(VecModel::from(lines)).into());
        app.set_dict_open(true);
        app.show().unwrap();

        assert_eq!(app.get_secondary_subtitle_y(), 12.0);
        let short_sec_h = app.get_secondary_subtitle_height();
        app.set_secondary_sub_text("Hi".into());
        assert!(
            (app.get_secondary_subtitle_height() - short_sec_h).abs() <= 4.0,
            "Short secondary subtitle must stay single-line without wrapping (short_sec_h={short_sec_h}, two_char_h={})",
            app.get_secondary_subtitle_height()
        );

        let sub_top = app.get_primary_subtitle_y();
        let sub_bottom = sub_top + app.get_primary_subtitle_height();
        let popup_top = app.get_popup_y();
        let popup_bottom = popup_top + app.get_popup_height();
        assert!(
            popup_bottom <= sub_top,
            "popup above subtitles must not overlap primary subtitles (popup_bottom={popup_bottom}, sub_top={sub_top})"
        );

        let default_x = app.get_popup_x();
        app.set_popup_offset_x(180.0);
        assert!(app.get_popup_x() > default_x);
        app.set_popup_offset_x(5000.0);
        assert!(app.get_popup_x() + app.get_popup_width() <= 1280.0 - 20.0 + 0.5);
        app.set_popup_offset_x(-5000.0);
        assert!(app.get_popup_x() >= 20.0 - 0.5);
        app.set_popup_offset_x(0.0);

        app.set_subtitle_offset(0.85);
        let top_sub_y = app.get_primary_subtitle_y();
        let top_sub_bottom = top_sub_y + app.get_primary_subtitle_height();
        let flipped_popup_y = app.get_popup_y();
        assert!(
            flipped_popup_y >= top_sub_bottom,
            "popup must flip below top-positioned primary subtitles (flipped_popup_y={flipped_popup_y}, top_sub_bottom={top_sub_bottom})"
        );
        let _ = sub_bottom;
    }

    #[test]
    fn about_modal_click_shield_url_modal_volume_max_mute_chapters_and_recent_titles() {
        i_slint_backend_testing::init_no_event_loop();
        let app = MementoApp::new().unwrap();
        app.window().set_size(slint::LogicalSize::new(1280., 720.));
        app.set_about_open(true);
        app.show().unwrap();

        let click = |x, y| {
            let position = slint::LogicalPosition::new(x, y);
            app.window().dispatch_event(WindowEvent::PointerPressed {
                position,
                button: PointerEventButton::Left,
            });
            app.window().dispatch_event(WindowEvent::PointerReleased {
                position,
                button: PointerEventButton::Left,
            });
        };

        click(640., 360.);
        assert!(
            app.get_about_open(),
            "About modal click-shield must prevent inner clicks from closing"
        );
        click(50., 50.);
        assert!(
            !app.get_about_open(),
            "Clicking outside About card must close modal"
        );

        app.set_volume_max(150.0);
        app.set_volume(135.0);
        app.set_is_muted(true);
        assert_eq!(app.get_volume_max(), 150.0);
        assert_eq!(app.get_volume(), 135.0);
        assert!(app.get_is_muted());

        let marks = chapter_marks_for_duration(&[0.0, 90.0, 300.0], 600.0);
        assert_eq!(marks.len(), 3);
        assert_eq!(marks[1].title.as_str(), "Chapter 2");
        app.set_chapter_marks(Rc::new(VecModel::from(marks)).into());
        assert_eq!(chapter_title_at_time(&app, 45.0), "Chapter 1");
        assert_eq!(chapter_title_at_time(&app, 120.0), "Chapter 2");
        assert_eq!(chapter_title_at_time(&app, 450.0), "Chapter 3");

        assert_eq!(
            format_recent_file_label("Frieren Episode 1", "/home/user/Videos/ep01.mkv"),
            "Frieren Episode 1 — /home/user/Videos/ep01.mkv"
        );
        assert_eq!(
            format_recent_file_label("", "/home/user/Videos/ep01.mkv"),
            "ep01.mkv — /home/user/Videos/ep01.mkv"
        );
    }

    #[test]
    fn cursor_reveal_zones_osc_fade_and_osc_mpv_cursor_bindings_work_in_headless_ui() {
        i_slint_backend_testing::init_no_event_loop();
        let cfg = ConfigController::new();
        cfg.edit_setting("behavior/osc-fade", "0").unwrap();
        cfg.edit_setting("behavior/osc-mpv-cursor", "true").unwrap();
        cfg.edit_setting("behavior/subtitle-cursor-show", "true")
            .unwrap();
        cfg.edit_setting("behavior/subtitle-cursor-show-secondary", "true")
            .unwrap();

        let app = MementoApp::new().unwrap();
        app.window().set_size(slint::LogicalSize::new(1280., 720.));
        refresh_player_preferences(&app, &cfg);
        assert_eq!(app.get_osc_fade_ms(), 0);
        assert!(app.get_osc_mpv_cursor());
        assert!(app.get_subtitle_cursor_show_enabled());
        assert!(app.get_secondary_subtitle_cursor_show_enabled());

        app.set_osc_visible(false);
        assert_eq!(app.get_osc_opacity(), 0.0);
        assert_eq!(app.get_menu_bar_opacity(), 0.0);
        app.set_osc_visible(true);
        assert_eq!(app.get_osc_opacity(), 1.0);
        assert_eq!(app.get_menu_bar_opacity(), 1.0);

        app.set_active_sub_text("Primary line".into());
        app.set_secondary_sub_text("Secondary line".into());
        app.set_subtitles_visible(false);
        app.set_hide_memento_while_playing(true);
        app.set_is_playing(true);
        app.show().unwrap();

        app.window().dispatch_event(WindowEvent::PointerMoved {
            position: slint::LogicalPosition::new(640., 360.),
        });
        assert!(!app.get_cursor_in_top_sub_zone());
        assert!(!app.get_cursor_in_bottom_sub_zone());
        assert!(!app.get_primary_subtitle_effective_visible());
        assert!(!app.get_secondary_subtitle_effective_visible());

        app.window().dispatch_event(WindowEvent::PointerMoved {
            position: slint::LogicalPosition::new(640., 610.),
        });
        assert!(app.get_cursor_in_bottom_sub_zone());
        assert!(!app.get_cursor_in_top_sub_zone());
        assert!(app.get_primary_subtitle_effective_visible());
        assert!(!app.get_secondary_subtitle_effective_visible());

        app.window().dispatch_event(WindowEvent::PointerMoved {
            position: slint::LogicalPosition::new(640., 660.),
        });
        assert!(app.get_primary_subtitle_effective_visible());
        assert!(!app.get_secondary_subtitle_effective_visible());

        app.set_osc_visible(false);
        app.window().dispatch_event(WindowEvent::PointerMoved {
            position: slint::LogicalPosition::new(640., 660.),
        });
        assert!(app.get_cursor_in_bottom_sub_zone());
        assert!(app.get_primary_subtitle_effective_visible());
        assert!(!app.get_secondary_subtitle_effective_visible());

        app.set_osc_visible(true);
        app.window().dispatch_event(WindowEvent::PointerMoved {
            position: slint::LogicalPosition::new(640., 80.),
        });
        assert!(app.get_cursor_in_top_sub_zone());
        assert!(!app.get_cursor_in_bottom_sub_zone());
        assert!(!app.get_primary_subtitle_effective_visible());
        assert!(app.get_secondary_subtitle_effective_visible());
        app.hide().unwrap();
    }
}

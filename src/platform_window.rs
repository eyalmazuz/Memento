//! Cross-platform window fullscreen state machine & Windows OpenGL borderless
//! `+1px` workaround (`Main.qml` parity, enhanced for multi-monitor layouts).
//!
//! Upstream `Main.qml` prevents Windows OpenGL exclusive-fullscreen DWM
//! flickering/hitching by:
//! 1. Saving prior maximized state and windowed geometry (`saved_maximized`, `saved_geometry`).
//! 2. Unmaximizing if maximized (`showNormal()`).
//! 3. Switching to a frameless window spanning `(monitor_x, monitor_y, monitor_w, monitor_h + 1)`.
//! 4. Applying `bottom_margin_px = 1` on the root split view so the extra offscreen pixel
//!    does not distort or clip visible UI/video content.
//! 5. On exit, restoring window chrome first, clearing `bottom_margin_px = 0`,
//!    restoring `saved_geometry`, and finally re-maximizing if `saved_maximized` was true.

use subomi_os::{
    compute_windows_borderless_fullscreen_rect, MonitorRect, WindowRect,
    WINDOWS_OPENGL_FULLSCREEN_EXTRA_HEIGHT_PX,
};

use crate::platform_renderer::{PlatformRendererKind, TargetOs};

/// Strategy used when toggling fullscreen mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FullscreenStrategy {
    /// Standard OS/window-manager fullscreen (`window.set_fullscreen(bool)`).
    NativeWindowFullscreen,
    /// Windows OpenGL workaround: frameless `(monitor_x, monitor_y, monitor_w, monitor_h + 1)`
    /// with `bottom_margin_px = 1`.
    WindowsBorderlessPlusOne,
}

/// Select the fullscreen strategy for the current OS and active video renderer.
pub const fn select_fullscreen_strategy(
    os: TargetOs,
    renderer: PlatformRendererKind,
    disable_windows_workaround: bool,
) -> FullscreenStrategy {
    if matches!(os, TargetOs::Windows)
        && matches!(renderer, PlatformRendererKind::OpenGl)
        && !disable_windows_workaround
    {
        FullscreenStrategy::WindowsBorderlessPlusOne
    } else {
        FullscreenStrategy::NativeWindowFullscreen
    }
}

/// Action emitted by [`PlatformFullscreenController`] for the host window layer
/// (Slint / winit / Win32) to execute.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FullscreenAction {
    /// State was already in the requested mode; nothing to do.
    NoOp,
    /// Toggle native OS fullscreen (`bottom_margin_px` stays `0`).
    SetNativeFullscreen { fullscreen: bool },
    /// Enter or resize Windows borderless `+1px` OpenGL fullscreen.
    EnterWindowsBorderless {
        /// True when the window was maximized before entering and should be
        /// restored to normal before applying frameless bounds (`root.showNormal()`).
        unmaximize_first: bool,
        /// Frameless window flag (`Qt.FramelessWindowHint` / stripped Win32 frame styles).
        frameless: bool,
        /// Target window rectangle `(monitor_x, monitor_y, monitor_w, monitor_h + 1)`.
        target_rect: WindowRect,
        /// Bottom layout margin (`1px`) compensating for the `+1px` height extension.
        bottom_margin_px: u32,
    },
    /// Exit Windows borderless `+1px` OpenGL fullscreen and restore saved state.
    ExitWindowsBorderless {
        /// Re-enable standard window frame chrome first (`root.flags = Qt.Window`).
        restore_decorations: bool,
        /// Reset bottom layout margin back to `0`.
        bottom_margin_px: u32,
        /// Previously saved normal windowed geometry (if known).
        restore_rect: Option<WindowRect>,
        /// Re-maximize the window after restoring normal geometry if it was
        /// maximized prior to entering fullscreen.
        restore_maximized: bool,
    },
}

/// Pure, cross-platform fullscreen state machine supporting both native
/// fullscreen and Windows `+1px` borderless OpenGL fullscreen across
/// multi-monitor virtual desktops.
#[derive(Debug, Clone, PartialEq)]
pub struct PlatformFullscreenController {
    strategy: FullscreenStrategy,
    is_fullscreen: bool,
    saved_geometry: Option<WindowRect>,
    saved_maximized: bool,
    active_monitor: Option<MonitorRect>,
    bottom_margin_px: u32,
    decorations_enabled: bool,
}

impl PlatformFullscreenController {
    pub const fn new(strategy: FullscreenStrategy) -> Self {
        Self {
            strategy,
            is_fullscreen: false,
            saved_geometry: None,
            saved_maximized: false,
            active_monitor: None,
            bottom_margin_px: 0,
            decorations_enabled: true,
        }
    }

    pub const fn for_platform(
        os: TargetOs,
        renderer: PlatformRendererKind,
        disable_windows_workaround: bool,
    ) -> Self {
        Self::new(select_fullscreen_strategy(
            os,
            renderer,
            disable_windows_workaround,
        ))
    }

    pub const fn strategy(&self) -> FullscreenStrategy {
        self.strategy
    }

    pub const fn is_fullscreen(&self) -> bool {
        self.is_fullscreen
    }

    pub const fn saved_geometry(&self) -> Option<WindowRect> {
        self.saved_geometry
    }

    pub const fn saved_maximized(&self) -> bool {
        self.saved_maximized
    }

    pub const fn active_monitor(&self) -> Option<MonitorRect> {
        self.active_monitor
    }

    pub const fn bottom_margin_px(&self) -> u32 {
        self.bottom_margin_px
    }

    pub const fn decorations_enabled(&self) -> bool {
        self.decorations_enabled
    }

    /// Enter fullscreen mode. Idempotent if already fullscreen (preserves the
    /// original `saved_geometry` and `saved_maximized` snapshot).
    pub fn enter_fullscreen(
        &mut self,
        current_normal_geometry: WindowRect,
        currently_maximized: bool,
        monitor: MonitorRect,
    ) -> FullscreenAction {
        if self.is_fullscreen {
            return FullscreenAction::NoOp;
        }
        self.is_fullscreen = true;
        self.active_monitor = Some(monitor);

        match self.strategy {
            FullscreenStrategy::NativeWindowFullscreen => {
                self.saved_geometry = Some(current_normal_geometry);
                self.saved_maximized = currently_maximized;
                self.bottom_margin_px = 0;
                FullscreenAction::SetNativeFullscreen { fullscreen: true }
            }
            FullscreenStrategy::WindowsBorderlessPlusOne => {
                self.saved_geometry = Some(current_normal_geometry);
                self.saved_maximized = currently_maximized;
                self.decorations_enabled = false;
                self.bottom_margin_px = WINDOWS_OPENGL_FULLSCREEN_EXTRA_HEIGHT_PX;
                let target_rect = compute_windows_borderless_fullscreen_rect(monitor);
                FullscreenAction::EnterWindowsBorderless {
                    unmaximize_first: currently_maximized,
                    frameless: true,
                    target_rect,
                    bottom_margin_px: WINDOWS_OPENGL_FULLSCREEN_EXTRA_HEIGHT_PX,
                }
            }
        }
    }

    /// Exit fullscreen mode and produce the restoration action. Idempotent if
    /// already windowed.
    pub fn exit_fullscreen(&mut self) -> FullscreenAction {
        if !self.is_fullscreen {
            return FullscreenAction::NoOp;
        }
        self.is_fullscreen = false;
        self.active_monitor = None;
        let saved_geometry = self.saved_geometry.take();
        let saved_maximized = std::mem::take(&mut self.saved_maximized);
        self.bottom_margin_px = 0;
        self.decorations_enabled = true;

        match self.strategy {
            FullscreenStrategy::NativeWindowFullscreen => {
                FullscreenAction::SetNativeFullscreen { fullscreen: false }
            }
            FullscreenStrategy::WindowsBorderlessPlusOne => {
                FullscreenAction::ExitWindowsBorderless {
                    restore_decorations: true,
                    bottom_margin_px: 0,
                    restore_rect: saved_geometry,
                    restore_maximized: saved_maximized,
                }
            }
        }
    }

    /// Toggle between windowed and fullscreen states.
    pub fn toggle_fullscreen(
        &mut self,
        current_normal_geometry: WindowRect,
        currently_maximized: bool,
        monitor: MonitorRect,
    ) -> FullscreenAction {
        if self.is_fullscreen {
            self.exit_fullscreen()
        } else {
            self.enter_fullscreen(current_normal_geometry, currently_maximized, monitor)
        }
    }

    /// Update the active borderless `+1px` target if the monitor's geometry or
    /// resolution changes while fullscreen, WITHOUT overwriting `saved_geometry`
    /// or `saved_maximized`.
    pub fn on_monitor_geometry_changed(&mut self, monitor: MonitorRect) -> FullscreenAction {
        if !self.is_fullscreen || self.strategy != FullscreenStrategy::WindowsBorderlessPlusOne {
            return FullscreenAction::NoOp;
        }
        if self.active_monitor == Some(monitor) {
            return FullscreenAction::NoOp;
        }
        self.active_monitor = Some(monitor);
        FullscreenAction::EnterWindowsBorderless {
            unmaximize_first: false,
            frameless: true,
            target_rect: compute_windows_borderless_fullscreen_rect(monitor),
            bottom_margin_px: WINDOWS_OPENGL_FULLSCREEN_EXTRA_HEIGHT_PX,
        }
    }

    /// Synchronize controller state when the OS/WM exits fullscreen externally
    /// (e.g. via Alt+Tab or compositor shortcut in native fullscreen mode).
    pub fn sync_external_native_fullscreen(&mut self, os_is_fullscreen: bool) {
        if self.strategy == FullscreenStrategy::NativeWindowFullscreen {
            self.is_fullscreen = os_is_fullscreen;
            if !os_is_fullscreen {
                self.saved_geometry = None;
                self.saved_maximized = false;
                self.active_monitor = None;
            }
        }
    }
}

/// Compute auto-fit windowed geometry centered on the current monitor's
/// virtual-desktop bounds (port of `Main.qml::updateWindowSize`).
///
/// Returns `None` if the window is currently fullscreen or maximized, or if the
/// requested video dimensions are zero.
pub fn compute_auto_fit_window_rect(
    video_width: u32,
    video_height: u32,
    sidebar_width: u32,
    auto_fit_percent: u32,
    monitor: MonitorRect,
    is_fullscreen: bool,
    is_maximized: bool,
) -> Option<WindowRect> {
    if is_fullscreen || is_maximized || video_width == 0 || video_height == 0 {
        return None;
    }
    if monitor.width == 0 || monitor.height == 0 {
        return None;
    }
    let percent = auto_fit_percent.clamp(1, 1000) as f64 / 100.0;
    let req_w = (video_width.saturating_add(sidebar_width) as f64) * percent;
    let req_h = (video_height as f64) * percent;
    let max_w = monitor.width as f64;
    let max_h = monitor.height as f64;
    let scale = (max_w / req_w).min(max_h / req_h).min(1.0);
    let final_w = (req_w * scale).round().max(1.0) as u32;
    let final_h = (req_h * scale).round().max(1.0) as u32;
    let offset_x = ((max_w - final_w as f64) / 2.0).round() as i32;
    let offset_y = ((max_h - final_h as f64) / 2.0).round() as i32;
    Some(WindowRect::new(
        monitor.x.saturating_add(offset_x),
        monitor.y.saturating_add(offset_y),
        final_w,
        final_h,
    ))
}

/// Apply a [`FullscreenAction`] to a Win32 `HWND` using `subomi-os`'s Win32
/// borderless hooks (available when compiled for Windows).
#[cfg(target_os = "windows")]
pub fn apply_windows_fullscreen_action(
    hwnd: isize,
    action: &FullscreenAction,
    win32_snapshot: &mut Option<subomi_os::Win32WindowStyleSnapshot>,
) -> Result<(), String> {
    match action {
        FullscreenAction::NoOp | FullscreenAction::SetNativeFullscreen { .. } => Ok(()),
        FullscreenAction::EnterWindowsBorderless { target_rect, .. } => {
            let monitor = MonitorRect::new(
                target_rect.x,
                target_rect.y,
                target_rect.width,
                target_rect
                    .height
                    .saturating_sub(WINDOWS_OPENGL_FULLSCREEN_EXTRA_HEIGHT_PX),
                1.0,
            );
            let (snapshot, _applied) =
                subomi_os::enter_windows_borderless_fullscreen(hwnd, Some(monitor))?;
            if win32_snapshot.is_none() {
                *win32_snapshot = Some(snapshot);
            }
            Ok(())
        }
        FullscreenAction::ExitWindowsBorderless { .. } => {
            if let Some(snapshot) = win32_snapshot.take() {
                subomi_os::exit_windows_borderless_fullscreen(hwnd, &snapshot)?;
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_fit_percent_supports_full_1_to_1000_range_within_monitor_bounds() {
        let monitor = MonitorRect::new(0, 0, 1920, 1080, 1.0);

        // 5% scales down below the old 10% floor.
        let tiny = compute_auto_fit_window_rect(1000, 500, 0, 5, monitor, false, false).unwrap();
        assert_eq!((tiny.width, tiny.height), (50, 25));

        // 0% clamps cleanly to 1%.
        let min_clamped =
            compute_auto_fit_window_rect(1000, 500, 0, 0, monitor, false, false).unwrap();
        assert_eq!((min_clamped.width, min_clamped.height), (10, 5));

        // 200% upscales small SD video beyond 100% up to monitor bounds.
        let upscaled =
            compute_auto_fit_window_rect(640, 360, 0, 200, monitor, false, false).unwrap();
        assert_eq!((upscaled.width, upscaled.height), (1280, 720));

        // 1000% and >1000% upscale tiny video and still clamp cleanly to monitor bounds.
        let large_monitor = MonitorRect::new(0, 0, 3840, 2160, 1.0);
        let at_1000 =
            compute_auto_fit_window_rect(100, 50, 0, 1000, large_monitor, false, false).unwrap();
        assert_eq!((at_1000.width, at_1000.height), (1000, 500));
        let above_1000 =
            compute_auto_fit_window_rect(100, 50, 0, 2500, large_monitor, false, false).unwrap();
        assert_eq!(above_1000, at_1000);

        let monitor_bounded =
            compute_auto_fit_window_rect(640, 360, 0, 1000, monitor, false, false).unwrap();
        assert_eq!((monitor_bounded.width, monitor_bounded.height), (1920, 1080));
    }
}

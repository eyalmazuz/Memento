//! Windows borderless `+1px` OpenGL fullscreen helpers and Win32 FFI hooks.
//!
//! On Windows, native or exact-screen-sized OpenGL fullscreen triggers DWM
//! exclusive-flip glitches, hitching, and flicker. Extending the borderless
//! window height by 1 pixel (`monitor_h + 1`) while applying a matching
//! `bottom_margin_px = 1` keeps the window out of the broken fullscreen flip
//! path while presenting pixel-exact content on the active monitor (including
//! multi-monitor setups with arbitrary `(monitor_x, monitor_y)` origins).

/// Extra pixel added to the window height in Windows borderless OpenGL
/// fullscreen to avoid DWM OpenGL fullscreen glitches and hitching.
pub const WINDOWS_OPENGL_FULLSCREEN_EXTRA_HEIGHT_PX: u32 = 1;

/// Win32 `WS_MAXIMIZE` window style bit (`0x0100_0000`).
pub const WIN32_WS_MAXIMIZE: u32 = 0x0100_0000;

/// Win32 standard window-frame style bits stripped in borderless mode:
/// `WS_CAPTION | WS_THICKFRAME | WS_MINIMIZEBOX | WS_MAXIMIZEBOX | WS_SYSMENU | WS_MAXIMIZE`.
pub const WIN32_BORDERLESS_STRIP_STYLE_MASK: u32 =
    0x00C0_0000 | 0x0004_0000 | 0x0002_0000 | 0x0001_0000 | 0x0008_0000 | WIN32_WS_MAXIMIZE;

/// Win32 extended window-frame style bits stripped in borderless mode:
/// `WS_EX_DLGMODALFRAME | WS_EX_CLIENTEDGE | WS_EX_STATICEDGE`.
pub const WIN32_BORDERLESS_STRIP_EX_STYLE_MASK: u32 = 0x0000_0001 | 0x0000_0200 | 0x0002_0000;

/// Window geometry in virtual-desktop coordinates.
///
/// `x` and `y` may be negative on multi-monitor desktops where a monitor sits
/// to the left of or above the primary display.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct WindowRect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

impl WindowRect {
    pub const fn new(x: i32, y: i32, width: u32, height: u32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    pub const fn is_empty(&self) -> bool {
        self.width == 0 || self.height == 0
    }
}

/// Monitor bounds in virtual-desktop coordinates along with its DPI scale.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MonitorRect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub scale_factor: f64,
}

impl MonitorRect {
    pub const fn new(x: i32, y: i32, width: u32, height: u32, scale_factor: f64) -> Self {
        Self {
            x,
            y,
            width,
            height,
            scale_factor,
        }
    }

    pub fn sanitized_scale(&self) -> f64 {
        if self.scale_factor.is_finite() && self.scale_factor > 0.0 {
            self.scale_factor
        } else {
            1.0
        }
    }
}

impl Default for MonitorRect {
    fn default() -> Self {
        Self {
            x: 0,
            y: 0,
            width: 1920,
            height: 1080,
            scale_factor: 1.0,
        }
    }
}

/// Win32 window chrome and placement snapshot saved before entering borderless
/// fullscreen so exiting restores exact style, position, size, and maximized state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Win32WindowStyleSnapshot {
    pub style: u32,
    pub ex_style: u32,
    pub was_maximized: bool,
    pub normal_rect: WindowRect,
}

/// Strip titlebar, sizing border, maximize bit, and modal/edge styles for frameless mode.
pub const fn strip_win32_window_frame_styles(style: u32, ex_style: u32) -> (u32, u32) {
    (
        style & !WIN32_BORDERLESS_STRIP_STYLE_MASK,
        ex_style & !WIN32_BORDERLESS_STRIP_EX_STYLE_MASK,
    )
}

/// Mask `WS_MAXIMIZE` out of a saved Win32 style before applying `SetWindowPos`
/// and `ShowWindow(SW_SHOWMAXIMIZED)` during borderless fullscreen exit so Win32
/// does not observe a conflicting maximized style bit prior to restoring bounds.
pub const fn restore_win32_normal_window_style(saved_style: u32) -> u32 {
    saved_style & !WIN32_WS_MAXIMIZE
}

/// Compute the `(monitor_x, monitor_y, monitor_w, monitor_h + 1)` target rect
/// for Windows borderless OpenGL fullscreen.
pub const fn compute_windows_borderless_fullscreen_rect(monitor: MonitorRect) -> WindowRect {
    let width = if monitor.width == 0 { 1 } else { monitor.width };
    let height = if monitor.height == 0 {
        1
    } else {
        monitor.height
    };
    WindowRect {
        x: monitor.x,
        y: monitor.y,
        width,
        height: height.saturating_add(WINDOWS_OPENGL_FULLSCREEN_EXTRA_HEIGHT_PX),
    }
}

#[cfg(target_os = "windows")]
mod win32_ffi {
    pub type Hwnd = isize;
    pub type Hmonitor = isize;
    pub type Bool = i32;

    pub const GWL_STYLE: i32 = -16;
    pub const GWL_EXSTYLE: i32 = -20;

    pub const WS_EX_TOPMOST: u32 = 0x0000_0008;

    pub const SW_SHOWNORMAL: i32 = 1;
    pub const SW_SHOWMAXIMIZED: i32 = 3;

    pub const MONITOR_DEFAULTTONEAREST: u32 = 2;

    pub const HWND_TOP: Hwnd = 0;
    pub const HWND_TOPMOST: Hwnd = -1;
    pub const HWND_NOTOPMOST: Hwnd = -2;

    pub const SWP_NOSIZE: u32 = 0x0001;
    pub const SWP_NOMOVE: u32 = 0x0002;
    pub const SWP_NOZORDER: u32 = 0x0004;
    pub const SWP_FRAMECHANGED: u32 = 0x0020;
    pub const SWP_SHOWWINDOW: u32 = 0x0040;
    pub const SWP_NOOWNERZORDER: u32 = 0x0200;

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    pub struct Point {
        pub x: i32,
        pub y: i32,
    }

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    pub struct Rect {
        pub left: i32,
        pub top: i32,
        pub right: i32,
        pub bottom: i32,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct MonitorInfo {
        pub cb_size: u32,
        pub rc_monitor: Rect,
        pub rc_work: Rect,
        pub dw_flags: u32,
    }

    impl Default for MonitorInfo {
        fn default() -> Self {
            Self {
                cb_size: std::mem::size_of::<Self>() as u32,
                rc_monitor: Rect::default(),
                rc_work: Rect::default(),
                dw_flags: 0,
            }
        }
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct WindowPlacement {
        pub length: u32,
        pub flags: u32,
        pub show_cmd: u32,
        pub pt_min_position: Point,
        pub pt_max_position: Point,
        pub rc_normal_position: Rect,
    }

    impl Default for WindowPlacement {
        fn default() -> Self {
            Self {
                length: std::mem::size_of::<Self>() as u32,
                flags: 0,
                show_cmd: 0,
                pt_min_position: Point::default(),
                pt_max_position: Point::default(),
                rc_normal_position: Rect::default(),
            }
        }
    }

    #[link(name = "user32")]
    extern "system" {
        pub fn MonitorFromWindow(hwnd: Hwnd, dw_flags: u32) -> Hmonitor;
        pub fn GetMonitorInfoW(hmonitor: Hmonitor, lpmi: *mut MonitorInfo) -> Bool;
        pub fn GetWindowPlacement(hwnd: Hwnd, lpwndpl: *mut WindowPlacement) -> Bool;
        pub fn GetWindowRect(hwnd: Hwnd, lp_rect: *mut Rect) -> Bool;
        pub fn GetWindowLongW(hwnd: Hwnd, n_index: i32) -> i32;
        pub fn SetWindowLongW(hwnd: Hwnd, n_index: i32, dw_new_long: i32) -> i32;
        pub fn ShowWindow(hwnd: Hwnd, n_cmd_show: i32) -> Bool;
        pub fn SetWindowPos(
            hwnd: Hwnd,
            hwnd_insert_after: Hwnd,
            x: i32,
            y: i32,
            cx: i32,
            cy: i32,
            u_flags: u32,
        ) -> Bool;
        pub fn IsZoomed(hwnd: Hwnd) -> Bool;
    }

    pub fn rect_to_window_rect(rect: Rect) -> super::WindowRect {
        let width = rect.right.saturating_sub(rect.left).max(0) as u32;
        let height = rect.bottom.saturating_sub(rect.top).max(0) as u32;
        super::WindowRect::new(rect.left, rect.top, width, height)
    }

    pub fn validate_hwnd(hwnd: isize) -> Result<Hwnd, String> {
        if hwnd == 0 {
            Err("Win32 HWND must be non-null".to_owned())
        } else {
            Ok(hwnd)
        }
    }
}

/// Query the monitor bounds containing `hwnd` (multi-monitor aware).
#[cfg(target_os = "windows")]
pub fn query_window_monitor(hwnd: isize) -> Result<MonitorRect, String> {
    let hwnd = win32_ffi::validate_hwnd(hwnd)?;
    unsafe {
        let hmonitor = win32_ffi::MonitorFromWindow(hwnd, win32_ffi::MONITOR_DEFAULTTONEAREST);
        if hmonitor == 0 {
            return Err("MonitorFromWindow returned null monitor".to_owned());
        }
        let mut info = win32_ffi::MonitorInfo::default();
        if win32_ffi::GetMonitorInfoW(hmonitor, &mut info) == 0 {
            return Err("GetMonitorInfoW failed".to_owned());
        }
        let rect = win32_ffi::rect_to_window_rect(info.rc_monitor);
        Ok(MonitorRect::new(
            rect.x,
            rect.y,
            rect.width,
            rect.height,
            1.0,
        ))
    }
}

/// Query the monitor bounds containing `hwnd` (non-Windows stub).
#[cfg(not(target_os = "windows"))]
pub fn query_window_monitor(_hwnd: isize) -> Result<MonitorRect, String> {
    Err("Win32 monitor query is only supported on Windows".to_owned())
}

/// Capture the current Win32 window styles, maximized state, and normal
/// geometry before switching to borderless fullscreen.
#[cfg(target_os = "windows")]
pub fn capture_win32_window_snapshot(hwnd: isize) -> Result<Win32WindowStyleSnapshot, String> {
    let hwnd = win32_ffi::validate_hwnd(hwnd)?;
    unsafe {
        let style = win32_ffi::GetWindowLongW(hwnd, win32_ffi::GWL_STYLE) as u32;
        let ex_style = win32_ffi::GetWindowLongW(hwnd, win32_ffi::GWL_EXSTYLE) as u32;
        let mut placement = win32_ffi::WindowPlacement::default();
        let has_placement = win32_ffi::GetWindowPlacement(hwnd, &mut placement) != 0;
        let was_maximized = win32_ffi::IsZoomed(hwnd) != 0
            || (has_placement && placement.show_cmd == win32_ffi::SW_SHOWMAXIMIZED as u32);

        // When not maximized, prefer GetWindowRect (screen coordinates matching
        // SetWindowPos) rather than rcNormalPosition (workspace coordinates
        // shifted by top/left Windows taskbars). When maximized, capture
        // rcNormalPosition as the initial fallback (refreshed after ShowWindow).
        let mut normal_rect = if !was_maximized {
            let mut raw_rect = win32_ffi::Rect::default();
            let screen_rect = if win32_ffi::GetWindowRect(hwnd, &mut raw_rect) != 0 {
                win32_ffi::rect_to_window_rect(raw_rect)
            } else {
                WindowRect::default()
            };
            if !screen_rect.is_empty() {
                screen_rect
            } else if has_placement {
                win32_ffi::rect_to_window_rect(placement.rc_normal_position)
            } else {
                WindowRect::default()
            }
        } else if has_placement {
            win32_ffi::rect_to_window_rect(placement.rc_normal_position)
        } else {
            WindowRect::default()
        };
        if normal_rect.is_empty() {
            let mut raw_rect = win32_ffi::Rect::default();
            if win32_ffi::GetWindowRect(hwnd, &mut raw_rect) != 0 {
                normal_rect = win32_ffi::rect_to_window_rect(raw_rect);
            }
        }
        Ok(Win32WindowStyleSnapshot {
            style,
            ex_style,
            was_maximized,
            normal_rect,
        })
    }
}

/// Capture the current Win32 window snapshot (non-Windows stub).
#[cfg(not(target_os = "windows"))]
pub fn capture_win32_window_snapshot(_hwnd: isize) -> Result<Win32WindowStyleSnapshot, String> {
    Err("Win32 window style snapshot is only supported on Windows".to_owned())
}

/// Enter Windows borderless `+1px` OpenGL fullscreen on `hwnd`, returning the
/// pre-fullscreen style/placement snapshot and the applied target rectangle.
#[cfg(target_os = "windows")]
pub fn enter_windows_borderless_fullscreen(
    hwnd: isize,
    monitor: Option<MonitorRect>,
) -> Result<(Win32WindowStyleSnapshot, WindowRect), String> {
    let hwnd = win32_ffi::validate_hwnd(hwnd)?;
    let mut snapshot = capture_win32_window_snapshot(hwnd)?;
    let target_monitor = match monitor {
        Some(m) => m,
        None => query_window_monitor(hwnd)?,
    };
    let target_rect = compute_windows_borderless_fullscreen_rect(target_monitor);
    let (frameless_style, frameless_ex_style) =
        strip_win32_window_frame_styles(snapshot.style, snapshot.ex_style);

    unsafe {
        if snapshot.was_maximized {
            let _ = win32_ffi::ShowWindow(hwnd, win32_ffi::SW_SHOWNORMAL);
            // Refresh normal_rect in screen coordinates after unmaximizing
            // (matching Main.qml: showNormal() before reading savedGeometry).
            let mut raw_rect = win32_ffi::Rect::default();
            if win32_ffi::GetWindowRect(hwnd, &mut raw_rect) != 0 {
                let screen_rect = win32_ffi::rect_to_window_rect(raw_rect);
                if !screen_rect.is_empty() {
                    snapshot.normal_rect = screen_rect;
                }
            }
        }
        let _ = win32_ffi::SetWindowLongW(hwnd, win32_ffi::GWL_STYLE, frameless_style as i32);
        let _ = win32_ffi::SetWindowLongW(hwnd, win32_ffi::GWL_EXSTYLE, frameless_ex_style as i32);
        let ok = win32_ffi::SetWindowPos(
            hwnd,
            win32_ffi::HWND_TOP,
            target_rect.x,
            target_rect.y,
            target_rect.width as i32,
            target_rect.height as i32,
            win32_ffi::SWP_FRAMECHANGED | win32_ffi::SWP_NOOWNERZORDER | win32_ffi::SWP_SHOWWINDOW,
        );
        if ok == 0 {
            let restored_style = restore_win32_normal_window_style(snapshot.style);
            let _ = win32_ffi::SetWindowLongW(hwnd, win32_ffi::GWL_STYLE, restored_style as i32);
            let _ =
                win32_ffi::SetWindowLongW(hwnd, win32_ffi::GWL_EXSTYLE, snapshot.ex_style as i32);
            let _ = win32_ffi::SetWindowPos(
                hwnd,
                0,
                0,
                0,
                0,
                0,
                win32_ffi::SWP_NOMOVE
                    | win32_ffi::SWP_NOSIZE
                    | win32_ffi::SWP_NOZORDER
                    | win32_ffi::SWP_NOOWNERZORDER
                    | win32_ffi::SWP_FRAMECHANGED,
            );
            if snapshot.was_maximized {
                let _ = win32_ffi::ShowWindow(hwnd, win32_ffi::SW_SHOWMAXIMIZED);
            }
            return Err("SetWindowPos failed entering Windows borderless fullscreen".to_owned());
        }
    }
    Ok((snapshot, target_rect))
}

/// Enter Windows borderless `+1px` OpenGL fullscreen (non-Windows stub).
#[cfg(not(target_os = "windows"))]
pub fn enter_windows_borderless_fullscreen(
    _hwnd: isize,
    _monitor: Option<MonitorRect>,
) -> Result<(Win32WindowStyleSnapshot, WindowRect), String> {
    Err("Win32 borderless fullscreen is only supported on Windows".to_owned())
}

/// Exit Windows borderless `+1px` OpenGL fullscreen and restore window chrome,
/// position, dimensions, and prior maximized state from `snapshot`.
#[cfg(target_os = "windows")]
pub fn exit_windows_borderless_fullscreen(
    hwnd: isize,
    snapshot: &Win32WindowStyleSnapshot,
) -> Result<(), String> {
    let hwnd = win32_ffi::validate_hwnd(hwnd)?;
    let restored_style = restore_win32_normal_window_style(snapshot.style);
    let insert_after = if (snapshot.ex_style & win32_ffi::WS_EX_TOPMOST) != 0 {
        win32_ffi::HWND_TOPMOST
    } else {
        win32_ffi::HWND_NOTOPMOST
    };
    unsafe {
        let prev_style = win32_ffi::GetWindowLongW(hwnd, win32_ffi::GWL_STYLE);
        let prev_ex_style = win32_ffi::GetWindowLongW(hwnd, win32_ffi::GWL_EXSTYLE);
        let _ = win32_ffi::SetWindowLongW(hwnd, win32_ffi::GWL_STYLE, restored_style as i32);
        let _ = win32_ffi::SetWindowLongW(hwnd, win32_ffi::GWL_EXSTYLE, snapshot.ex_style as i32);
        let rect = snapshot.normal_rect;
        let ok = if !rect.is_empty() {
            win32_ffi::SetWindowPos(
                hwnd,
                insert_after,
                rect.x,
                rect.y,
                rect.width as i32,
                rect.height as i32,
                win32_ffi::SWP_FRAMECHANGED
                    | win32_ffi::SWP_NOOWNERZORDER
                    | win32_ffi::SWP_SHOWWINDOW,
            )
        } else {
            win32_ffi::SetWindowPos(
                hwnd,
                insert_after,
                0,
                0,
                0,
                0,
                win32_ffi::SWP_NOMOVE
                    | win32_ffi::SWP_NOSIZE
                    | win32_ffi::SWP_FRAMECHANGED
                    | win32_ffi::SWP_NOOWNERZORDER
                    | win32_ffi::SWP_SHOWWINDOW,
            )
        };
        if ok == 0 {
            let _ = win32_ffi::SetWindowLongW(hwnd, win32_ffi::GWL_STYLE, prev_style);
            let _ = win32_ffi::SetWindowLongW(hwnd, win32_ffi::GWL_EXSTYLE, prev_ex_style);
            let _ = win32_ffi::SetWindowPos(
                hwnd,
                0,
                0,
                0,
                0,
                0,
                win32_ffi::SWP_NOMOVE
                    | win32_ffi::SWP_NOSIZE
                    | win32_ffi::SWP_NOZORDER
                    | win32_ffi::SWP_NOOWNERZORDER
                    | win32_ffi::SWP_FRAMECHANGED,
            );
            return Err(
                "SetWindowPos failed restoring windowed geometry from borderless fullscreen"
                    .to_owned(),
            );
        }
        if snapshot.was_maximized {
            let _ = win32_ffi::ShowWindow(hwnd, win32_ffi::SW_SHOWMAXIMIZED);
        }
    }
    Ok(())
}

/// Exit Windows borderless `+1px` OpenGL fullscreen (non-Windows stub).
#[cfg(not(target_os = "windows"))]
pub fn exit_windows_borderless_fullscreen(
    _hwnd: isize,
    _snapshot: &Win32WindowStyleSnapshot,
) -> Result<(), String> {
    Err("Win32 borderless fullscreen restore is only supported on Windows".to_owned())
}

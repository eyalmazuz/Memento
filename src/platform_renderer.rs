////////////////////////////////////////////////////////////////////////////////
//
// Copyright (c) 2026 Ripose
//
// This file is part of Memento.
//
// Memento is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, version 2 of the License.
//
// Memento is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with Memento.  If not, see <https://www.gnu.org/licenses/>.
//
////////////////////////////////////////////////////////////////////////////////

//! Unified cross-platform video renderer selection and capability planning.
//!
//! Coordinates backend selection across macOS (Metal via IOSurface/CGL or
//! `CAMetalLayer`, with OpenGL and Software fallbacks), Linux (Wayland/X11
//! OpenGL with `GL_RGBA16F` -> `GL_RGBA8` FBOs and Software fallback), and
//! Windows (OpenGL with `+1px` borderless fullscreen workaround and Software
//! fallback), honoring CLI arguments, environment variables, and `mpv.conf`.

use std::fmt;
use std::path::Path;

use crate::graphics_device::{
    configured_hwdec_from_args, configured_option_from_args, detect_linux_display_server,
    is_software_gl_renderer, read_mpv_conf_renderer_hints, LinuxDisplayServer,
    MpvConfRendererHints,
};
use crate::video_metal::{
    is_metal_device_available, resolve_macos_metal_hwdec, MetalPresentationMode,
};
use memento_player::mpv::{compatible_opengl_hwdec_for_session, compatible_software_hwdec};

/// Supported operating system families for platform renderer selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TargetOs {
    Linux,
    MacOs,
    Windows,
    Other,
}

impl TargetOs {
    pub const fn current() -> Self {
        #[cfg(target_os = "linux")]
        {
            Self::Linux
        }
        #[cfg(target_os = "macos")]
        {
            Self::MacOs
        }
        #[cfg(target_os = "windows")]
        {
            Self::Windows
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
        {
            Self::Other
        }
    }
}

/// Active display server / windowing system.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DisplayServer {
    Wayland,
    X11,
    Cocoa,
    Win32,
    Headless,
    Unknown,
}

impl DisplayServer {
    pub fn detect_current() -> Self {
        match TargetOs::current() {
            TargetOs::MacOs => Self::Cocoa,
            TargetOs::Windows => Self::Win32,
            TargetOs::Linux => match detect_linux_display_server() {
                LinuxDisplayServer::Wayland | LinuxDisplayServer::WaylandWithX11Fallback => {
                    Self::Wayland
                }
                LinuxDisplayServer::X11 => Self::X11,
                LinuxDisplayServer::Headless => Self::Headless,
            },
            TargetOs::Other => Self::Unknown,
        }
    }

    pub const fn is_wayland(self) -> bool {
        matches!(self, Self::Wayland)
    }
}

/// Selected video presentation backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PlatformRendererKind {
    /// Hardware OpenGL texture/FBO presentation (`glow` + `libmpv` OpenGL render context).
    OpenGl,
    /// macOS Metal presentation (`IOSurface` CGL->Metal bridge or `CAMetalLayer`).
    Metal,
    /// CPU software fallback (`libmpv` `sw` render API -> Slint `SharedPixelBuffer`).
    Software,
}

impl PlatformRendererKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OpenGl => "opengl",
            Self::Metal => "metal",
            Self::Software => "software",
        }
    }

    pub const fn is_hardware_accelerated(self) -> bool {
        matches!(self, Self::OpenGl | Self::Metal)
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "gl" | "opengl" | "egl" | "glx" => Some(Self::OpenGl),
            "metal" | "mtl" | "moltenvk" | "macvk" => Some(Self::Metal),
            "sw" | "software" | "cpu" => Some(Self::Software),
            _ => None,
        }
    }
}

impl fmt::Display for PlatformRendererKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Target surface color format preference for FBO/IOSurface allocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VideoColorFormat {
    /// 16-bit half-float RGBA (`GL_RGBA16F` / `MTLPixelFormatRGBA16Float`).
    Rgba16Float,
    /// 8-bit normalized RGBA (`GL_RGBA8` / CPU RGBA8).
    Rgba8Unorm,
    /// 8-bit normalized BGRA (`kCVPixelFormatType_32BGRA` / `MTLPixelFormatBGRA8Unorm`).
    Bgra8Unorm,
}

/// Probed platform and graphics capabilities used to choose the renderer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoRendererCapabilities {
    pub os: TargetOs,
    pub display_server: DisplayServer,
    pub opengl_available: bool,
    pub opengl_rgba16f_supported: bool,
    pub metal_available: bool,
    pub iosurface_available: bool,
    pub edr_supported: bool,
    pub gpu_renderer_name: Option<String>,
    pub nvidia_driver_available: bool,
    pub vaapi_available: bool,
}

impl VideoRendererCapabilities {
    pub fn for_macos(metal_available: bool, opengl_available: bool) -> Self {
        Self {
            os: TargetOs::MacOs,
            display_server: DisplayServer::Cocoa,
            opengl_available,
            opengl_rgba16f_supported: opengl_available,
            metal_available,
            iosurface_available: metal_available,
            edr_supported: false,
            gpu_renderer_name: None,
            nvidia_driver_available: false,
            vaapi_available: false,
        }
    }

    pub fn for_linux(
        display_server: DisplayServer,
        opengl_available: bool,
        opengl_rgba16f_supported: bool,
    ) -> Self {
        Self {
            os: TargetOs::Linux,
            display_server,
            opengl_available,
            opengl_rgba16f_supported,
            metal_available: false,
            iosurface_available: false,
            edr_supported: false,
            gpu_renderer_name: None,
            nvidia_driver_available: false,
            vaapi_available: false,
        }
    }

    pub fn for_windows(opengl_available: bool, opengl_rgba16f_supported: bool) -> Self {
        Self {
            os: TargetOs::Windows,
            display_server: DisplayServer::Win32,
            opengl_available,
            opengl_rgba16f_supported,
            metal_available: false,
            iosurface_available: false,
            edr_supported: false,
            gpu_renderer_name: None,
            nvidia_driver_available: false,
            vaapi_available: false,
        }
    }

    /// Probe current runtime platform capabilities without creating a window.
    pub fn detect_current() -> Self {
        let os = TargetOs::current();
        let display_server = DisplayServer::detect_current();
        let metal_available = os == TargetOs::MacOs && is_metal_device_available();
        let opengl_available = display_server != DisplayServer::Headless;
        #[cfg(target_os = "linux")]
        let nvidia_driver_available = std::fs::read_dir("/proc/driver/nvidia/gpus")
            .is_ok_and(|mut gpus| gpus.any(|entry| entry.is_ok()))
            && std::path::Path::new("/dev/nvidiactl").exists();
        #[cfg(not(target_os = "linux"))]
        let nvidia_driver_available = false;

        #[cfg(target_os = "linux")]
        let vaapi_available = std::path::Path::new("/dev/dri/renderD128").exists();
        #[cfg(not(target_os = "linux"))]
        let vaapi_available = false;

        Self {
            os,
            display_server,
            opengl_available,
            opengl_rgba16f_supported: opengl_available,
            metal_available,
            iosurface_available: metal_available,
            edr_supported: false,
            gpu_renderer_name: None,
            nvidia_driver_available,
            vaapi_available,
        }
    }
}

/// User/environment/CLI/`mpv.conf` overrides that influence backend selection.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RendererOverrides {
    /// Explicit renderer request (e.g., `MEMENTO_RENDERER=metal|opengl|software`
    /// or `SLINT_BACKEND=winit-software`).
    pub explicit_renderer: Option<PlatformRendererKind>,
    /// Global `--hwdec=...` CLI override.
    pub cli_hwdec: Option<String>,
    /// Global `hwdec=...` from `mpv.conf`.
    pub mpv_conf_hwdec: Option<String>,
    /// Global `gpu-api=...` from CLI or `mpv.conf`.
    pub gpu_api: Option<String>,
    /// Global `vo=...` from CLI or `mpv.conf`.
    pub vo: Option<String>,
    /// Global `gpu-context=...` from CLI or `mpv.conf`.
    pub gpu_context: Option<String>,
    /// Force 8-bit `GL_RGBA8` FBOs instead of probing `GL_RGBA16F`.
    pub force_rgba8: bool,
    /// Prefer HDR / 16-bit half-float Metal surfaces when EDR is supported.
    pub prefer_hdr: bool,
    /// Opt out of the Windows borderless `+1px` OpenGL fullscreen workaround.
    pub disable_windows_borderless_workaround: bool,
    /// True when `LIBGL_ALWAYS_SOFTWARE=1` is set in the environment.
    pub libgl_always_software: bool,
}

impl RendererOverrides {
    /// Build overrides from `mpv.conf` hints, CLI arguments, and an injectable
    /// environment variable lookup (fully deterministic for unit tests).
    pub fn from_hints_args_and_env<'a>(
        hints: MpvConfRendererHints,
        cli_args: impl IntoIterator<Item = &'a str>,
        env: impl Fn(&str) -> Option<String>,
    ) -> Self {
        let args: Vec<&str> = cli_args.into_iter().collect();
        let cli_hwdec = configured_hwdec_from_args(args.iter().copied());
        let cli_gpu_api = configured_option_from_args(args.iter().copied(), "gpu-api");
        let cli_vo = configured_option_from_args(args.iter().copied(), "vo");
        let cli_gpu_context = configured_option_from_args(args.iter().copied(), "gpu-context");

        let gpu_api = cli_gpu_api.or(hints.gpu_api);
        let vo = cli_vo.or(hints.vo);
        let gpu_context = cli_gpu_context.or(hints.gpu_context);

        let env_renderer = env("MEMENTO_RENDERER").and_then(|v| PlatformRendererKind::parse(&v));
        let slint_software = env("SLINT_BACKEND")
            .is_some_and(|v| v.to_ascii_lowercase().contains("software"));
        let vo_software = vo
            .as_deref()
            .is_some_and(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "sw" | "software"));

        let explicit_renderer = env_renderer
            .or_else(|| slint_software.then_some(PlatformRendererKind::Software))
            .or_else(|| vo_software.then_some(PlatformRendererKind::Software))
            .or_else(|| {
                gpu_api
                    .as_deref()
                    .and_then(PlatformRendererKind::parse)
            });

        let libgl_always_software = env("LIBGL_ALWAYS_SOFTWARE")
            .is_some_and(|v| matches!(v.trim(), "1" | "true" | "yes"));
        let force_rgba8 = env("MEMENTO_GL_FORCE_RGBA8")
            .is_some_and(|v| matches!(v.trim(), "1" | "true" | "yes"));
        let prefer_hdr = env("MEMENTO_PREFER_HDR")
            .is_some_and(|v| matches!(v.trim(), "1" | "true" | "yes"));
        let disable_windows_borderless_workaround = env("MEMENTO_DISABLE_WIN_BORDERLESS_FS")
            .is_some_and(|v| matches!(v.trim(), "1" | "true" | "yes"));

        Self {
            explicit_renderer,
            cli_hwdec,
            mpv_conf_hwdec: hints.hwdec,
            gpu_api,
            vo,
            gpu_context,
            force_rgba8,
            prefer_hdr,
            disable_windows_borderless_workaround,
            libgl_always_software,
        }
    }

    /// Build overrides from `<config_dir>/mpv.conf`, CLI arguments, and process env.
    pub fn from_config_dir_and_args<'a>(
        config_dir: &Path,
        cli_args: impl IntoIterator<Item = &'a str>,
    ) -> Self {
        let hints = read_mpv_conf_renderer_hints(config_dir);
        Self::from_hints_args_and_env(hints, cli_args, |key| std::env::var(key).ok())
    }

    /// Effective configured `hwdec` string (`CLI` > `mpv.conf` > `"auto-safe"`).
    pub fn requested_hwdec(&self) -> &str {
        self.cli_hwdec
            .as_deref()
            .or(self.mpv_conf_hwdec.as_deref())
            .unwrap_or("auto-safe")
    }
}

/// Concrete renderer plan chosen for the current OS, display server, and overrides.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlatformRendererConfig {
    pub primary: PlatformRendererKind,
    pub fallback_chain: Vec<PlatformRendererKind>,
    pub preferred_fbo_formats: Vec<VideoColorFormat>,
    pub effective_hwdec: String,
    pub use_advanced_control: bool,
    pub windows_borderless_fullscreen: bool,
    pub metal_mode: Option<MetalPresentationMode>,
    pub reason: String,
}

impl PlatformRendererConfig {
    /// Select the optimal platform renderer configuration from probed
    /// [`VideoRendererCapabilities`] and [`RendererOverrides`].
    pub fn select(caps: &VideoRendererCapabilities, overrides: &RendererOverrides) -> Self {
        let requested_hwdec = overrides.requested_hwdec();

        // 1. Explicit Software override always wins on every OS.
        if overrides.explicit_renderer == Some(PlatformRendererKind::Software) {
            return Self::software_config(
                requested_hwdec,
                "explicit software renderer override (SLINT_BACKEND / MEMENTO_RENDERER / vo=sw)",
            );
        }

        match caps.os {
            TargetOs::MacOs => Self::select_macos(caps, overrides, requested_hwdec),
            TargetOs::Windows => Self::select_windows(caps, overrides, requested_hwdec),
            TargetOs::Linux | TargetOs::Other => Self::select_linux(caps, overrides, requested_hwdec),
        }
    }

    /// Advance to the next fallback backend if `failed` could not initialize at runtime.
    pub fn next_fallback(&self, failed: PlatformRendererKind) -> Option<PlatformRendererKind> {
        let pos = self.fallback_chain.iter().position(|&k| k == failed)?;
        self.fallback_chain.get(pos + 1).copied()
    }

    fn select_macos(
        caps: &VideoRendererCapabilities,
        overrides: &RendererOverrides,
        requested_hwdec: &str,
    ) -> Self {
        let force_gl = overrides.explicit_renderer == Some(PlatformRendererKind::OpenGl);
        if !force_gl && caps.metal_available {
            let prefer_native_layer = overrides.gpu_api.as_deref().is_some_and(|v| {
                matches!(
                    v.trim().to_ascii_lowercase().as_str(),
                    "vulkan" | "moltenvk" | "metal"
                )
            }) || overrides.gpu_context.as_deref().is_some_and(|v| {
                matches!(v.trim().to_ascii_lowercase().as_str(), "macvk" | "moltenvk")
            }) || !caps.iosurface_available;

            let metal_mode = if prefer_native_layer {
                MetalPresentationMode::NativeMetalLayer
            } else {
                MetalPresentationMode::IoSurfaceOpenGlBridge
            };

            let mut fallback_chain = vec![PlatformRendererKind::Metal];
            if caps.opengl_available {
                fallback_chain.push(PlatformRendererKind::OpenGl);
            }
            fallback_chain.push(PlatformRendererKind::Software);

            let preferred_fbo_formats = if overrides.prefer_hdr && caps.edr_supported {
                vec![VideoColorFormat::Rgba16Float, VideoColorFormat::Bgra8Unorm]
            } else {
                vec![VideoColorFormat::Bgra8Unorm]
            };

            let effective_hwdec = resolve_macos_metal_hwdec(metal_mode, Some(requested_hwdec));
            let reason = match metal_mode {
                MetalPresentationMode::IoSurfaceOpenGlBridge => {
                    "macOS Metal IOSurface/CGL bridge selected with OpenGL/Software fallback"
                }
                MetalPresentationMode::NativeMetalLayer => {
                    "macOS native CAMetalLayer/MoltenVK selected per gpu-api/gpu-context override"
                }
            };

            return Self {
                primary: PlatformRendererKind::Metal,
                fallback_chain,
                preferred_fbo_formats,
                effective_hwdec,
                use_advanced_control: false,
                windows_borderless_fullscreen: false,
                metal_mode: Some(metal_mode),
                reason: reason.to_owned(),
            };
        }

        if caps.opengl_available {
            let formats = opengl_fbo_formats(caps.opengl_rgba16f_supported, overrides.force_rgba8);
            let effective_hwdec = match caps.gpu_renderer_name.as_deref() {
                Some(name) => compatible_opengl_hwdec_for_session(requested_hwdec, name, false),
                None => requested_hwdec.to_owned(),
            };
            let reason = if force_gl {
                "macOS OpenGL explicitly requested; Software fallback enabled"
            } else {
                "macOS Metal unavailable; falling back to OpenGL with Software fallback"
            };
            return Self {
                primary: PlatformRendererKind::OpenGl,
                fallback_chain: vec![PlatformRendererKind::OpenGl, PlatformRendererKind::Software],
                preferred_fbo_formats: formats,
                effective_hwdec,
                use_advanced_control: true,
                windows_borderless_fullscreen: false,
                metal_mode: None,
                reason: reason.to_owned(),
            };
        }

        Self::software_config(
            requested_hwdec,
            "macOS Metal and OpenGL unavailable; falling back to Software renderer",
        )
    }

    fn select_linux(
        caps: &VideoRendererCapabilities,
        overrides: &RendererOverrides,
        requested_hwdec: &str,
    ) -> Self {
        if !caps.opengl_available {
            return Self::software_config(
                requested_hwdec,
                "Linux OpenGL unavailable (headless or no GL context); using Software fallback",
            );
        }

        let is_wayland = caps.display_server.is_wayland();
        let renderer_name = caps.gpu_renderer_name.as_deref().unwrap_or("");
        let effective_hwdec = if overrides.libgl_always_software
            || (!renderer_name.is_empty() && is_software_gl_renderer(renderer_name))
        {
            compatible_software_hwdec(requested_hwdec)
        } else if !renderer_name.is_empty() {
            compatible_opengl_hwdec_for_session(requested_hwdec, renderer_name, is_wayland)
        } else if is_wayland && caps.nvidia_driver_available {
            compatible_opengl_hwdec_for_session(requested_hwdec, "NVIDIA", true)
        } else {
            requested_hwdec.to_owned()
        };

        let formats = opengl_fbo_formats(caps.opengl_rgba16f_supported, overrides.force_rgba8);
        let reason = if overrides.explicit_renderer == Some(PlatformRendererKind::Metal) {
            "Metal is macOS-only; using Linux OpenGL with Software fallback"
        } else if is_wayland {
            "Linux Wayland OpenGL (EGL) selected with GL_RGBA16F->GL_RGBA8 FBOs and Software fallback"
        } else {
            "Linux X11 OpenGL selected with GL_RGBA16F->GL_RGBA8 FBOs and Software fallback"
        };

        Self {
            primary: PlatformRendererKind::OpenGl,
            fallback_chain: vec![PlatformRendererKind::OpenGl, PlatformRendererKind::Software],
            preferred_fbo_formats: formats,
            effective_hwdec,
            use_advanced_control: true,
            windows_borderless_fullscreen: false,
            metal_mode: None,
            reason: reason.to_owned(),
        }
    }

    fn select_windows(
        caps: &VideoRendererCapabilities,
        overrides: &RendererOverrides,
        requested_hwdec: &str,
    ) -> Self {
        if !caps.opengl_available {
            return Self::software_config(
                requested_hwdec,
                "Windows OpenGL unavailable; using Software fallback",
            );
        }

        let effective_hwdec = match caps.gpu_renderer_name.as_deref() {
            Some(name) if !name.is_empty() => {
                compatible_opengl_hwdec_for_session(requested_hwdec, name, false)
            }
            _ => requested_hwdec.to_owned(),
        };
        let formats = opengl_fbo_formats(caps.opengl_rgba16f_supported, overrides.force_rgba8);
        let windows_borderless_fullscreen = !overrides.disable_windows_borderless_workaround;
        let reason = if windows_borderless_fullscreen {
            "Windows OpenGL selected with +1px borderless fullscreen workaround and Software fallback"
        } else {
            "Windows OpenGL selected (borderless fullscreen workaround disabled) with Software fallback"
        };

        Self {
            primary: PlatformRendererKind::OpenGl,
            fallback_chain: vec![PlatformRendererKind::OpenGl, PlatformRendererKind::Software],
            preferred_fbo_formats: formats,
            effective_hwdec,
            use_advanced_control: true,
            windows_borderless_fullscreen,
            metal_mode: None,
            reason: reason.to_owned(),
        }
    }

    fn software_config(requested_hwdec: &str, reason: &str) -> Self {
        Self {
            primary: PlatformRendererKind::Software,
            fallback_chain: vec![PlatformRendererKind::Software],
            preferred_fbo_formats: vec![VideoColorFormat::Rgba8Unorm],
            effective_hwdec: compatible_software_hwdec(requested_hwdec),
            use_advanced_control: false,
            windows_borderless_fullscreen: false,
            metal_mode: None,
            reason: reason.to_owned(),
        }
    }
}

fn opengl_fbo_formats(rgba16f_supported: bool, force_rgba8: bool) -> Vec<VideoColorFormat> {
    if rgba16f_supported && !force_rgba8 {
        vec![VideoColorFormat::Rgba16Float, VideoColorFormat::Rgba8Unorm]
    } else {
        vec![VideoColorFormat::Rgba8Unorm]
    }
}

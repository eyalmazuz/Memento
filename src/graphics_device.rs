//! Match an explicitly selected NVIDIA decoder to its OpenGL device on Linux,
//! and inspect Linux display servers, `mpv.conf` renderer hints, and GPU
//! decoder capabilities before graphics initialization.

use std::path::{Path, PathBuf};

#[cfg(target_os = "linux")]
const DEVICE_OVERRIDES: &[&str] = &[
    "DRI_PRIME",
    "__NV_PRIME_RENDER_OFFLOAD",
    "__NV_PRIME_RENDER_OFFLOAD_PROVIDER",
    "__GLX_VENDOR_LIBRARY_NAME",
    "__EGL_VENDOR_LIBRARY_FILENAMES",
    "__EGL_VENDOR_LIBRARY_DIRS",
    "LIBGL_ALWAYS_SOFTWARE",
    "SLINT_BACKEND",
];

#[cfg(target_os = "linux")]
const EGL_VENDOR_DIRS: &[&str] = &[
    "/etc/glvnd/egl_vendor.d",
    "/usr/share/glvnd/egl_vendor.d",
    "/usr/local/share/glvnd/egl_vendor.d",
    "/run/opengl-driver/share/glvnd/egl_vendor.d",
    "/usr/lib/x86_64-linux-gnu/GL/glvnd/egl_vendor.d",
    "/usr/lib/aarch64-linux-gnu/GL/glvnd/egl_vendor.d",
];

/// Linux display server detected from environment variables.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LinuxDisplayServer {
    Wayland,
    WaylandWithX11Fallback,
    X11,
    Headless,
}

/// Preferred OpenGL context creation API on Linux.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LinuxGlContextApi {
    /// EGL (required on Wayland and for VAAPI zero-copy on Intel/AMD).
    Egl,
    /// GLX (legacy X11 path, also supports NVIDIA VDPAU / GLX interop).
    Glx,
}

/// Planned Linux graphics device selection before graphics/libmpv init.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinuxGraphicsDevicePlan {
    pub display_server: LinuxDisplayServer,
    pub preferred_gl_api: LinuxGlContextApi,
    pub request_nvidia_prime_offload: bool,
    pub nvidia_egl_vendor_file: Option<PathBuf>,
    pub force_software_gl: bool,
}

/// Renderer/decoder-relevant global options extracted from `mpv.conf` before
/// creating a libmpv handle.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MpvConfRendererHints {
    pub hwdec: Option<String>,
    pub vo: Option<String>,
    pub gpu_api: Option<String>,
    pub gpu_context: Option<String>,
}

/// Detect the active Linux display server from an injectable environment lookup.
pub fn detect_linux_display_server_with(
    env: impl Fn(&str) -> Option<String>,
) -> LinuxDisplayServer {
    let wayland = env("WAYLAND_DISPLAY").is_some_and(|v| !v.trim().is_empty());
    let x11 = env("DISPLAY").is_some_and(|v| !v.trim().is_empty());
    let session = env("XDG_SESSION_TYPE")
        .map(|v| v.trim().to_ascii_lowercase())
        .unwrap_or_default();

    if wayland && x11 {
        if session == "x11" {
            LinuxDisplayServer::X11
        } else {
            LinuxDisplayServer::WaylandWithX11Fallback
        }
    } else if wayland {
        LinuxDisplayServer::Wayland
    } else if x11 {
        LinuxDisplayServer::X11
    } else {
        LinuxDisplayServer::Headless
    }
}

/// Detect the active Linux display server from the current process environment.
pub fn detect_linux_display_server() -> LinuxDisplayServer {
    detect_linux_display_server_with(|key| std::env::var(key).ok())
}

/// Classify whether a `GL_RENDERER` string identifies a CPU software rasterizer.
pub fn is_software_gl_renderer(renderer: &str) -> bool {
    let lower = renderer.to_ascii_lowercase();
    lower.contains("llvmpipe")
        || lower.contains("softpipe")
        || lower.contains("swrast")
        || lower.contains("swiftshader")
        || lower.contains("lavapipe")
        || lower.contains("microsoft basic render")
}

/// Classify whether a `GL_RENDERER` string identifies an NVIDIA GPU.
pub fn is_nvidia_gl_renderer(renderer: &str) -> bool {
    let lower = renderer.to_ascii_lowercase();
    lower.contains("nvidia") || lower.contains("geforce") || lower.contains("quadro")
}

/// Classify whether a `GL_RENDERER` string typically uses VAAPI on Linux (Intel/AMD/Mesa).
pub fn is_vaapi_capable_renderer(renderer: &str) -> bool {
    if is_software_gl_renderer(renderer) || is_nvidia_gl_renderer(renderer) {
        return false;
    }
    let lower = renderer.to_ascii_lowercase();
    lower.contains("intel")
        || lower.contains("amd")
        || lower.contains("radeon")
        || lower.contains("mesa")
}

/// Determine whether a configured `hwdec` string explicitly starts with a
/// direct NVIDIA CUDA/GL decoder (`nvdec` or `cuda`) requiring PRIME offload.
pub fn decoder_requests_direct_nvidia(decoder: &str) -> bool {
    decoder
        .split(',')
        .next()
        .is_some_and(|s| matches!(s.trim(), "nvdec" | "cuda"))
}

/// Pure Linux graphics-device decision planner (testable without probing `/dev`).
pub fn plan_linux_graphics_device(
    decoder: &str,
    display_server: LinuxDisplayServer,
    nvidia_driver_available: bool,
    device_overridden: bool,
    libgl_always_software: bool,
    nvidia_egl_vendor_file: Option<PathBuf>,
) -> LinuxGraphicsDevicePlan {
    let request_nvidia_prime_offload = nvidia_driver_available
        && !device_overridden
        && !libgl_always_software
        && decoder_requests_direct_nvidia(decoder);

    let preferred_gl_api = match display_server {
        LinuxDisplayServer::Wayland | LinuxDisplayServer::WaylandWithX11Fallback => {
            LinuxGlContextApi::Egl
        }
        LinuxDisplayServer::X11 => {
            // Prefer EGL on X11 when VAAPI is selected (required by libmpv for
            // direct VAAPI zero-copy), and GLX for direct NVIDIA/VDPAU interop.
            if request_nvidia_prime_offload
                || decoder
                    .split(',')
                    .next()
                    .is_some_and(|s| matches!(s.trim(), "nvdec" | "cuda" | "vdpau"))
            {
                LinuxGlContextApi::Glx
            } else {
                LinuxGlContextApi::Egl
            }
        }
        LinuxDisplayServer::Headless => LinuxGlContextApi::Egl,
    };

    LinuxGraphicsDevicePlan {
        display_server,
        preferred_gl_api,
        request_nvidia_prime_offload,
        nvidia_egl_vendor_file,
        force_software_gl: libgl_always_software,
    }
}

#[cfg(target_os = "linux")]
fn find_nvidia_egl_vendor_file() -> Option<PathBuf> {
    find_nvidia_egl_vendor_file_in(EGL_VENDOR_DIRS.iter().map(Path::new))
}

pub fn find_nvidia_egl_vendor_file_in<'a>(
    dirs: impl IntoIterator<Item = &'a Path>,
) -> Option<PathBuf> {
    let dirs: Vec<&Path> = dirs.into_iter().collect();
    for dir in &dirs {
        let preferred = dir.join("10_nvidia.json");
        if preferred.is_file() {
            return Some(preferred);
        }
    }
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        let mut matches: Vec<PathBuf> = entries
            .filter_map(|entry| entry.ok().map(|e| e.path()))
            .filter(|path| {
                path.is_file()
                    && path.extension().is_some_and(|ext| ext == "json")
                    && path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .is_some_and(|name| name.to_ascii_lowercase().contains("nvidia"))
            })
            .collect();
        matches.sort();
        if let Some(first) = matches.into_iter().next() {
            return Some(first);
        }
    }
    None
}

fn strip_unquoted_comment(line: &str) -> &str {
    let mut in_single = false;
    let mut in_double = false;
    let mut escaped = false;
    for (idx, ch) in line.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match ch {
            '\\' if in_double => escaped = true,
            '\'' if !in_double => in_single = !in_single,
            '"' if !in_single => in_double = !in_double,
            '#' if !in_single && !in_double => return &line[..idx],
            _ => {}
        }
    }
    line
}

fn unquote_conf_value(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.len() >= 2
        && ((trimmed.starts_with('"') && trimmed.ends_with('"'))
            || (trimmed.starts_with('\'') && trimmed.ends_with('\'')))
    {
        trimmed[1..trimmed.len() - 1].trim().to_owned()
    } else {
        trimmed.to_owned()
    }
}

/// Parse global `hwdec`, `vo`, `gpu-api`, and `gpu-context` hints from `mpv.conf`
/// without initializing a libmpv instance.
pub fn parse_mpv_conf_renderer_hints(contents: &str) -> MpvConfRendererHints {
    let mut in_global_section = true;
    let mut hints = MpvConfRendererHints::default();
    for raw_line in contents.lines() {
        let line = strip_unquoted_comment(raw_line).trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            in_global_section = line == "[default]";
            continue;
        }
        if !in_global_section {
            continue;
        }
        let option = line.strip_prefix("--").unwrap_or(line);
        match option {
            "no-hwdec" => {
                hints.hwdec = Some("no".to_owned());
                continue;
            }
            "hwdec" => {
                hints.hwdec = Some("yes".to_owned());
                continue;
            }
            _ => {}
        }
        if let Some((key, value)) = option.split_once('=') {
            let parsed = unquote_conf_value(value);
            if parsed.is_empty() {
                continue;
            }
            match key.trim() {
                "hwdec" => hints.hwdec = Some(parsed),
                "vo" => hints.vo = Some(parsed),
                "gpu-api" => hints.gpu_api = Some(parsed),
                "gpu-context" => hints.gpu_context = Some(parsed),
                _ => {}
            }
        }
    }
    hints
}

/// Read renderer hints from `<config_dir>/mpv.conf` if present.
pub fn read_mpv_conf_renderer_hints(config_dir: &Path) -> MpvConfRendererHints {
    std::fs::read_to_string(config_dir.join("mpv.conf"))
        .map(|contents| parse_mpv_conf_renderer_hints(&contents))
        .unwrap_or_default()
}

/// Lightweight pre-init parser for `mpv.conf` extracting global `hwdec=...`
/// without constructing an `mpv_handle` before `exec()`.
pub fn parse_hwdec_from_mpv_conf_str(contents: &str) -> Option<String> {
    parse_mpv_conf_renderer_hints(contents).hwdec
}

/// Read global `hwdec` directly from `<config_dir>/mpv.conf` before creating
/// any graphics context or libmpv instance.
pub fn configured_hwdec_from_mpv_conf(config_dir: &Path) -> Option<String> {
    let contents = std::fs::read_to_string(config_dir.join("mpv.conf")).ok()?;
    parse_hwdec_from_mpv_conf_str(&contents)
}

/// Extract a global `--<key>=<val>` CLI override outside `--{ ... --}` groups.
pub fn configured_option_from_args<'a>(
    args: impl IntoIterator<Item = &'a str>,
    key: &str,
) -> Option<String> {
    let prefix = format!("--{key}=");
    let mut depth = 0usize;
    let mut configured = None;
    for arg in args.into_iter().skip(1) {
        if arg == "--" && depth == 0 {
            break;
        }
        if arg == "--{" {
            depth += 1;
            continue;
        }
        if arg == "--}" {
            depth = depth.saturating_sub(1);
            continue;
        }
        if depth > 0 {
            continue;
        }
        if let Some(val) = arg.strip_prefix(&prefix) {
            let unquoted = unquote_conf_value(val);
            if !unquoted.is_empty() {
                configured = Some(unquoted);
            }
        }
    }
    configured
}

/// Extract global `--hwdec=...` / `--hwdec` / `--no-hwdec` CLI overrides outside `--{ ... --}`.
pub fn configured_hwdec_from_args<'a>(args: impl IntoIterator<Item = &'a str>) -> Option<String> {
    let mut depth = 0usize;
    let mut configured = None;
    for arg in args.into_iter().skip(1) {
        if arg == "--" && depth == 0 {
            break;
        }
        if arg == "--{" {
            depth += 1;
            continue;
        }
        if arg == "--}" {
            depth = depth.saturating_sub(1);
            continue;
        }
        if depth > 0 {
            continue;
        }
        if arg == "--no-hwdec" {
            configured = Some("no".to_owned());
        } else if arg == "--hwdec" {
            configured = Some("yes".to_owned());
        } else if let Some(val) = arg.strip_prefix("--hwdec=") {
            let unquoted = unquote_conf_value(val);
            if !unquoted.is_empty() {
                configured = Some(unquoted);
            }
        }
    }
    configured
}

#[cfg(target_os = "linux")]
fn needs_nvidia_offload(decoder: &str, driver_available: bool, overridden: bool) -> bool {
    driver_available && !overridden && decoder_requests_direct_nvidia(decoder)
}

/// Inspect CLI options and `<config_dir>/mpv.conf` before initializing libmpv
/// or Slint, re-execing with NVIDIA PRIME offload (X11 GLX and Wayland EGL)
/// when `hwdec` explicitly starts with `nvdec` or `cuda`.
pub fn select_for_config_dir(config_dir: &Path) {
    let cli_args: Vec<String> = std::env::args().collect();
    let cli_hwdec = configured_hwdec_from_args(cli_args.iter().map(String::as_str));
    let decoder = cli_hwdec
        .or_else(|| configured_hwdec_from_mpv_conf(config_dir))
        .unwrap_or_else(|| "auto-safe".to_owned());
    select_for_decoder(&decoder);
}

/// Run before creating any windows or GL contexts. PRIME must be selected
/// before loading the graphics libraries. Re-exec the same process with that
/// environment instead of changing process-global variables after mpv has
/// started worker threads. No second player process/window is created.
pub fn select_for_decoder(decoder: &str) {
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::process::CommandExt;
        let overridden = DEVICE_OVERRIDES
            .iter()
            .any(|name| std::env::var_os(name).is_some());
        if !needs_nvidia_offload(decoder, true, overridden) {
            return;
        }
        let driver_available = std::fs::read_dir("/proc/driver/nvidia/gpus")
            .is_ok_and(|mut gpus| gpus.any(|entry| entry.is_ok()))
            && std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open("/dev/nvidiactl")
                .is_ok();
        if !driver_available {
            return;
        }
        let Ok(executable) = std::env::current_exe() else {
            return;
        };
        let egl_vendor = find_nvidia_egl_vendor_file();
        let display_server = detect_linux_display_server();
        if matches!(
            display_server,
            LinuxDisplayServer::Wayland | LinuxDisplayServer::WaylandWithX11Fallback
        ) && egl_vendor.is_none()
        {
            eprintln!(
                "Video device: NVIDIA EGL vendor json not found; keeping default Wayland device for hwdec={decoder}"
            );
            return;
        }
        eprintln!("Video device: selecting NVIDIA for configured hwdec={decoder}");
        // These standard driver overrides also prevent a second re-exec.
        let mut command = std::process::Command::new(executable);
        command
            .args(std::env::args_os().skip(1))
            .env("__NV_PRIME_RENDER_OFFLOAD", "1")
            .env("__GLX_VENDOR_LIBRARY_NAME", "nvidia");
        if let Some(egl_vendor) = egl_vendor {
            command.env("__EGL_VENDOR_LIBRARY_FILENAMES", egl_vendor);
        }
        let error = command.exec();
        eprintln!("Could not select the decoder's graphics device: {error}");
    }
    #[cfg(not(target_os = "linux"))]
    let _ = decoder;
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn only_direct_nvidia_decoding_requests_automatic_offload() {
        assert!(needs_nvidia_offload("nvdec", true, false));
        assert!(needs_nvidia_offload(" nvdec,auto", true, false));
        assert!(needs_nvidia_offload("cuda", true, false));
        for decoder in [
            "auto",
            "auto-safe",
            "auto-copy-safe",
            "nvdec-copy",
            "cuda-copy",
            "no",
            "vaapi",
            "auto,nvdec",
        ] {
            assert!(!needs_nvidia_offload(decoder, true, false), "{decoder}");
        }
        assert!(!needs_nvidia_offload("nvdec", false, false));
        assert!(!needs_nvidia_offload("nvdec", true, true));
    }

    #[test]
    fn mpv_conf_parser_handles_comments_quotes_and_profiles() {
        let conf = r#"
            # hwdec=no
            vo=gpu
            gpu-api=opengl
            hwdec = "nvdec" # direct decode
            [extension.gif]
            hwdec=no
            gpu-api=vulkan
            [default]
            --hwdec='nvdec,vaapi'
        "#;
        let hints = parse_mpv_conf_renderer_hints(conf);
        assert_eq!(hints.hwdec.as_deref(), Some("nvdec,vaapi"));
        assert_eq!(hints.vo.as_deref(), Some("gpu"));
        assert_eq!(hints.gpu_api.as_deref(), Some("opengl"));
        assert_eq!(
            parse_hwdec_from_mpv_conf_str(conf).as_deref(),
            Some("nvdec,vaapi")
        );

        let profile_only = "[protocol.http]\nhwdec=nvdec\n";
        assert_eq!(parse_hwdec_from_mpv_conf_str(profile_only), None);

        let disabled = "hwdec=nvdec\nno-hwdec\n";
        assert_eq!(
            parse_hwdec_from_mpv_conf_str(disabled).as_deref(),
            Some("no")
        );
    }

    #[test]
    fn cli_hwdec_parser_ignores_grouped_options() {
        assert_eq!(
            configured_hwdec_from_args(["subomi", "--{", "--hwdec=nvdec", "--}", "a.mkv"]),
            None
        );
        assert_eq!(
            configured_hwdec_from_args(["subomi", "--hwdec=nvdec", "a.mkv"]).as_deref(),
            Some("nvdec")
        );
        assert_eq!(
            configured_hwdec_from_args(["subomi", "--hwdec=nvdec", "--no-hwdec"]).as_deref(),
            Some("no")
        );
        assert_eq!(
            configured_hwdec_from_args(["subomi", "--hwdec", "a.mkv"]).as_deref(),
            Some("yes")
        );
        assert_eq!(
            configured_hwdec_from_args(["subomi", "--hwdec=vaapi", "--", "--hwdec=nvdec"])
                .as_deref(),
            Some("vaapi")
        );
        assert_eq!(
            configured_hwdec_from_args(["subomi", "--", "--hwdec=nvdec"]),
            None
        );
    }

    #[test]
    fn discovers_nvidia_egl_vendor_json_preferring_10_nvidia() {
        let dir = tempfile::TempDir::new().unwrap();
        let subdir = dir.path().join("glvnd");
        std::fs::create_dir_all(&subdir).unwrap();
        let fallback = subdir.join("20_nvidia_custom.json");
        std::fs::write(&fallback, "{}").unwrap();
        assert_eq!(
            find_nvidia_egl_vendor_file_in([subdir.as_path()]).as_deref(),
            Some(fallback.as_path())
        );

        let preferred = subdir.join("10_nvidia.json");
        std::fs::write(&preferred, "{}").unwrap();
        assert_eq!(
            find_nvidia_egl_vendor_file_in([subdir.as_path()]).as_deref(),
            Some(preferred.as_path())
        );
    }
}

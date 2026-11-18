//! Match an explicitly selected NVIDIA decoder to its OpenGL device on Linux.

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
];

#[cfg(target_os = "linux")]
fn find_nvidia_egl_vendor_file() -> Option<PathBuf> {
    find_nvidia_egl_vendor_file_in(EGL_VENDOR_DIRS.iter().map(Path::new))
}

fn find_nvidia_egl_vendor_file_in<'a>(dirs: impl IntoIterator<Item = &'a Path>) -> Option<PathBuf> {
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

#[allow(dead_code)]
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

#[allow(dead_code)]
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

/// Lightweight pre-init parser for `mpv.conf` extracting global `hwdec=...`
/// without constructing an `mpv_handle` before `exec()`.
#[allow(dead_code)]
pub fn parse_hwdec_from_mpv_conf_str(contents: &str) -> Option<String> {
    let mut in_global_section = true;
    let mut configured = None;
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
        if option == "no-hwdec" {
            configured = Some("no".to_owned());
            continue;
        }
        if option == "hwdec" {
            configured = Some("yes".to_owned());
            continue;
        }
        if let Some((key, value)) = option.split_once('=') {
            if key.trim() == "hwdec" {
                let parsed = unquote_conf_value(value);
                if !parsed.is_empty() {
                    configured = Some(parsed);
                }
            }
        }
    }
    configured
}

/// Read global `hwdec` directly from `<config_dir>/mpv.conf` before creating
/// any graphics context or libmpv instance.
#[allow(dead_code)]
pub fn configured_hwdec_from_mpv_conf(config_dir: &Path) -> Option<String> {
    let contents = std::fs::read_to_string(config_dir.join("mpv.conf")).ok()?;
    parse_hwdec_from_mpv_conf_str(&contents)
}

/// Extract global `--hwdec=...` / `--hwdec` / `--no-hwdec` CLI overrides outside `--{ ... --}`.
#[allow(dead_code)]
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
    driver_available
        && !overridden
        && decoder
            .split(',')
            .next()
            .is_some_and(|s| s.trim() == "nvdec")
}

/// Inspect CLI options and `<config_dir>/mpv.conf` before initializing libmpv
/// or Slint, re-execing with NVIDIA PRIME offload (X11 GLX and Wayland EGL)
/// when `hwdec` explicitly starts with `nvdec`.
#[allow(dead_code)]
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
        eprintln!("Video device: selecting NVIDIA for configured hwdec={decoder}");
        // These standard driver overrides also prevent a second re-exec.
        let mut command = std::process::Command::new(executable);
        command
            .args(std::env::args_os().skip(1))
            .env("__NV_PRIME_RENDER_OFFLOAD", "1")
            .env("__GLX_VENDOR_LIBRARY_NAME", "nvidia");
        if let Some(egl_vendor) = find_nvidia_egl_vendor_file() {
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
        for decoder in [
            "auto",
            "auto-safe",
            "auto-copy-safe",
            "nvdec-copy",
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
            hwdec = "nvdec" # direct decode
            [extension.gif]
            hwdec=no
            [default]
            --hwdec='nvdec,vaapi'
        "#;
        assert_eq!(
            parse_hwdec_from_mpv_conf_str(conf).as_deref(),
            Some("nvdec,vaapi")
        );

        let profile_only = "[protocol.http]\nhwdec=nvdec\n";
        assert_eq!(parse_hwdec_from_mpv_conf_str(profile_only), None);

        let disabled = "hwdec=nvdec\nno-hwdec\n";
        assert_eq!(parse_hwdec_from_mpv_conf_str(disabled).as_deref(), Some("no"));
    }

    #[test]
    fn cli_hwdec_parser_ignores_grouped_options() {
        assert_eq!(
            configured_hwdec_from_args(["memento", "--{", "--hwdec=nvdec", "--}", "a.mkv"]),
            None
        );
        assert_eq!(
            configured_hwdec_from_args(["memento", "--hwdec=nvdec", "a.mkv"]).as_deref(),
            Some("nvdec")
        );
        assert_eq!(
            configured_hwdec_from_args(["memento", "--hwdec=nvdec", "--no-hwdec"]).as_deref(),
            Some("no")
        );
        assert_eq!(
            configured_hwdec_from_args(["memento", "--hwdec", "a.mkv"]).as_deref(),
            Some("yes")
        );
        assert_eq!(
            configured_hwdec_from_args(["memento", "--hwdec=vaapi", "--", "--hwdec=nvdec"])
                .as_deref(),
            Some("vaapi")
        );
        assert_eq!(
            configured_hwdec_from_args(["memento", "--", "--hwdec=nvdec"]),
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

//! Match an explicitly selected NVIDIA decoder to its OpenGL device on Linux.

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
fn needs_nvidia_offload(decoder: &str, driver_available: bool, overridden: bool) -> bool {
    driver_available
        && !overridden
        && decoder
            .split(',')
            .next()
            .is_some_and(|s| s.trim() == "nvdec")
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
        let error = std::process::Command::new(executable)
            .args(std::env::args_os().skip(1))
            .env("__NV_PRIME_RENDER_OFFLOAD", "1")
            .env("__GLX_VENDOR_LIBRARY_NAME", "nvidia")
            .exec();
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
}

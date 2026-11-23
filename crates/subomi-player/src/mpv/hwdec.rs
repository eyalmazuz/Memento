//! Hardware-decoder compatibility rules for OpenGL and software rendering.

fn is_software_gl_renderer_name(renderer: &str) -> bool {
    let lower = renderer.to_ascii_lowercase();
    lower.contains("llvmpipe")
        || lower.contains("softpipe")
        || lower.contains("swrast")
        || lower.contains("swiftshader")
        || lower.contains("lavapipe")
        || lower.contains("microsoft basic render")
}

/// Compute a safe `hwdec` string for an OpenGL `mpv_render_context` given the
/// configured decoder list and `GL_RENDERER` name (assuming X11/non-Wayland).
pub fn compatible_opengl_hwdec(configured: &str, renderer: &str) -> String {
    compatible_opengl_hwdec_for_session(configured, renderer, false)
}

/// Compute a safe `hwdec` string for an OpenGL `mpv_render_context` taking into
/// account the `GL_RENDERER` vendor/type and whether Wayland EGL is active.
pub fn compatible_opengl_hwdec_for_session(
    configured: &str,
    renderer: &str,
    is_wayland: bool,
) -> String {
    if is_software_gl_renderer_name(renderer) {
        return compatible_software_hwdec(configured);
    }
    let is_nvidia = renderer.to_ascii_lowercase().contains("nvidia");
    if is_nvidia {
        if is_wayland {
            return configured
                .split(',')
                .map(|decoder| match decoder.trim() {
                    "auto-safe" => "nvdec-copy,auto-copy-safe",
                    "auto" | "yes" => "nvdec-copy,auto-copy",
                    "nvdec" => "nvdec-copy",
                    "cuda" => "cuda-copy",
                    "vdpau" => "vdpau-copy",
                    _ => decoder,
                })
                .collect::<Vec<_>>()
                .join(",");
        }
        return configured
            .split(',')
            .map(|decoder| match decoder.trim() {
                "auto-safe" => "nvdec-copy,auto-copy-safe",
                _ => decoder,
            })
            .collect::<Vec<_>>()
            .join(",");
    }
    configured
        .split(',')
        .map(|decoder| match decoder.trim() {
            "nvdec" => "nvdec-copy",
            "cuda" => "cuda-copy",
            "vdpau" => "vdpau-copy",
            _ => decoder,
        })
        .collect::<Vec<_>>()
        .join(",")
}

/// Convert direct GPU-interop decoders to copyback equivalents for software
/// rendering or software-rasterized GL contexts (`llvmpipe`, `swrast`, etc.).
pub fn compatible_software_hwdec(configured: &str) -> String {
    configured
        .split(',')
        .map(|decoder| match decoder.trim() {
            "auto-safe" => "auto-copy-safe",
            "auto" | "yes" => "auto-copy",
            "nvdec" => "nvdec-copy",
            "cuda" => "cuda-copy",
            "vaapi" => "vaapi-copy",
            "vdpau" => "vdpau-copy",
            "d3d11va" => "d3d11va-copy",
            "videotoolbox" => "videotoolbox-copy",
            "vulkan" => "vulkan-copy",
            _ => decoder,
        })
        .collect::<Vec<_>>()
        .join(",")
}

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

use std::collections::HashMap;
use std::path::PathBuf;

use memento_app::graphics_device::{
    configured_option_from_args, decoder_requests_direct_nvidia, detect_linux_display_server_with,
    is_nvidia_gl_renderer, is_software_gl_renderer, is_vaapi_capable_renderer,
    parse_mpv_conf_renderer_hints, plan_linux_graphics_device, LinuxDisplayServer,
    LinuxGlContextApi,
};
use memento_app::platform_renderer::{
    DisplayServer, PlatformRendererConfig, PlatformRendererKind, RendererOverrides, TargetOs,
    VideoColorFormat, VideoRendererCapabilities,
};
use memento_app::platform_window::{
    compute_auto_fit_window_rect, select_fullscreen_strategy, FullscreenAction, FullscreenStrategy,
    PlatformFullscreenController,
};
use memento_app::video_gl::{
    clamp_fbo_dimensions, plan_video_fbo_formats, should_reallocate_fbo, validate_fbo_dimensions,
    VideoFboFormat,
};
use memento_app::video_metal::{
    clamp_metal_drawable_size, plan_metal_mpv_options, resolve_macos_metal_hwdec,
    sanitize_contents_scale, select_metal_pixel_format, IoSurfaceDescriptor, MetalFrameSlot,
    MetalLayerGeometry, MetalPixelFormat, MetalPresentationMode, MetalSurfaceRing,
    MetalTextureCache, CV_PIXEL_FORMAT_32_BGRA, CV_PIXEL_FORMAT_64_RGBA_HALF,
    METAL_FRAME_SLOT_COUNT, MTL_MAX_TEXTURE_DIMENSION_2D, MTL_PIXEL_FORMAT_BGRA8_UNORM,
    MTL_PIXEL_FORMAT_RGBA16_FLOAT,
};
use memento_os::{
    compute_windows_borderless_fullscreen_rect, strip_win32_window_frame_styles, MonitorRect,
    WindowRect, WIN32_BORDERLESS_STRIP_EX_STYLE_MASK, WIN32_BORDERLESS_STRIP_STYLE_MASK,
    WINDOWS_OPENGL_FULLSCREEN_EXTRA_HEIGHT_PX,
};
use memento_player::mpv::{
    compatible_opengl_hwdec, compatible_opengl_hwdec_for_session, compatible_software_hwdec,
    GlDisplayBackend, MpvInstance,
};

fn env_from<'a>(pairs: &[(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
    let map: HashMap<&str, &str> = pairs.iter().copied().collect();
    move |key: &str| map.get(key).map(|v| (*v).to_owned())
}

// ── 1. Unified Platform Renderer Selection ──────────────────────────────────

#[test]
fn macos_prefers_metal_with_opengl_and_software_fallbacks() {
    let caps = VideoRendererCapabilities::for_macos(true, true);
    let cfg = PlatformRendererConfig::select(&caps, &RendererOverrides::default());

    assert_eq!(cfg.primary, PlatformRendererKind::Metal);
    assert_eq!(
        cfg.fallback_chain,
        vec![
            PlatformRendererKind::Metal,
            PlatformRendererKind::OpenGl,
            PlatformRendererKind::Software,
        ]
    );
    assert_eq!(
        cfg.metal_mode,
        Some(MetalPresentationMode::IoSurfaceOpenGlBridge)
    );
    assert_eq!(cfg.preferred_fbo_formats, vec![VideoColorFormat::Bgra8Unorm]);
    assert_eq!(cfg.effective_hwdec, "videotoolbox,auto-safe");
    assert!(!cfg.windows_borderless_fullscreen);

    assert_eq!(
        cfg.next_fallback(PlatformRendererKind::Metal),
        Some(PlatformRendererKind::OpenGl)
    );
    assert_eq!(
        cfg.next_fallback(PlatformRendererKind::OpenGl),
        Some(PlatformRendererKind::Software)
    );
    assert_eq!(cfg.next_fallback(PlatformRendererKind::Software), None);
}

#[test]
fn macos_falls_back_to_opengl_when_metal_unavailable_or_overridden() {
    let no_metal = VideoRendererCapabilities::for_macos(false, true);
    let cfg = PlatformRendererConfig::select(&no_metal, &RendererOverrides::default());
    assert_eq!(cfg.primary, PlatformRendererKind::OpenGl);
    assert_eq!(
        cfg.fallback_chain,
        vec![PlatformRendererKind::OpenGl, PlatformRendererKind::Software]
    );
    assert_eq!(
        cfg.preferred_fbo_formats,
        vec![VideoColorFormat::Rgba16Float, VideoColorFormat::Rgba8Unorm]
    );
    assert_eq!(cfg.metal_mode, None);

    let with_metal = VideoRendererCapabilities::for_macos(true, true);
    let gl_override = RendererOverrides {
        explicit_renderer: Some(PlatformRendererKind::OpenGl),
        ..Default::default()
    };
    let cfg_gl = PlatformRendererConfig::select(&with_metal, &gl_override);
    assert_eq!(cfg_gl.primary, PlatformRendererKind::OpenGl);

    let neither = VideoRendererCapabilities::for_macos(false, false);
    let cfg_sw = PlatformRendererConfig::select(&neither, &RendererOverrides::default());
    assert_eq!(cfg_sw.primary, PlatformRendererKind::Software);
    assert_eq!(cfg_sw.fallback_chain, vec![PlatformRendererKind::Software]);
}

#[test]
fn macos_moltenvk_and_edr_overrides_select_native_layer_and_16f_format() {
    let mut caps = VideoRendererCapabilities::for_macos(true, true);
    caps.edr_supported = true;
    let overrides = RendererOverrides {
        gpu_api: Some("vulkan".to_owned()),
        prefer_hdr: true,
        ..Default::default()
    };
    let cfg = PlatformRendererConfig::select(&caps, &overrides);
    assert_eq!(cfg.primary, PlatformRendererKind::Metal);
    assert_eq!(
        cfg.metal_mode,
        Some(MetalPresentationMode::NativeMetalLayer)
    );
    assert_eq!(
        cfg.preferred_fbo_formats,
        vec![VideoColorFormat::Rgba16Float, VideoColorFormat::Bgra8Unorm]
    );
    assert_eq!(cfg.effective_hwdec, "videotoolbox");
}

#[test]
fn linux_wayland_and_x11_select_opengl_with_rgba16f_fallback_and_safe_hwdec() {
    let mut wayland_nv = VideoRendererCapabilities::for_linux(DisplayServer::Wayland, true, true);
    wayland_nv.gpu_renderer_name = Some("NVIDIA GeForce RTX 4070".to_owned());
    let cfg = PlatformRendererConfig::select(&wayland_nv, &RendererOverrides::default());
    assert_eq!(cfg.primary, PlatformRendererKind::OpenGl);
    assert_eq!(
        cfg.fallback_chain,
        vec![PlatformRendererKind::OpenGl, PlatformRendererKind::Software]
    );
    assert_eq!(
        cfg.preferred_fbo_formats,
        vec![VideoColorFormat::Rgba16Float, VideoColorFormat::Rgba8Unorm]
    );
    assert_eq!(cfg.effective_hwdec, "nvdec-copy,auto-copy-safe");
    assert!(cfg.use_advanced_control);
    assert!(!cfg.windows_borderless_fullscreen);

    let mut x11_intel = VideoRendererCapabilities::for_linux(DisplayServer::X11, true, false);
    x11_intel.gpu_renderer_name = Some("Mesa Intel UHD 630".to_owned());
    let cfg_intel = PlatformRendererConfig::select(&x11_intel, &RendererOverrides::default());
    assert_eq!(cfg_intel.primary, PlatformRendererKind::OpenGl);
    assert_eq!(
        cfg_intel.preferred_fbo_formats,
        vec![VideoColorFormat::Rgba8Unorm]
    );
    assert_eq!(cfg_intel.effective_hwdec, "auto-safe");

    // Requesting Metal on Linux gracefully uses Linux OpenGL.
    let metal_req = RendererOverrides {
        explicit_renderer: Some(PlatformRendererKind::Metal),
        ..Default::default()
    };
    let cfg_linux_metal = PlatformRendererConfig::select(&x11_intel, &metal_req);
    assert_eq!(cfg_linux_metal.primary, PlatformRendererKind::OpenGl);

    // Headless Linux without OpenGL falls back cleanly to Software.
    let headless = VideoRendererCapabilities::for_linux(DisplayServer::Headless, false, false);
    let cfg_sw = PlatformRendererConfig::select(&headless, &RendererOverrides::default());
    assert_eq!(cfg_sw.primary, PlatformRendererKind::Software);
    assert_eq!(cfg_sw.effective_hwdec, "auto-copy-safe");
}

#[test]
fn windows_selects_opengl_with_borderless_fullscreen_workaround_and_opt_out() {
    let caps = VideoRendererCapabilities::for_windows(true, true);
    let cfg = PlatformRendererConfig::select(&caps, &RendererOverrides::default());
    assert_eq!(cfg.primary, PlatformRendererKind::OpenGl);
    assert!(cfg.windows_borderless_fullscreen);
    assert_eq!(
        cfg.preferred_fbo_formats,
        vec![VideoColorFormat::Rgba16Float, VideoColorFormat::Rgba8Unorm]
    );

    let opt_out = RendererOverrides {
        disable_windows_borderless_workaround: true,
        force_rgba8: true,
        ..Default::default()
    };
    let cfg_opt_out = PlatformRendererConfig::select(&caps, &opt_out);
    assert_eq!(cfg_opt_out.primary, PlatformRendererKind::OpenGl);
    assert!(!cfg_opt_out.windows_borderless_fullscreen);
    assert_eq!(
        cfg_opt_out.preferred_fbo_formats,
        vec![VideoColorFormat::Rgba8Unorm]
    );

    let sw_caps = VideoRendererCapabilities::for_windows(false, false);
    let cfg_sw = PlatformRendererConfig::select(&sw_caps, &RendererOverrides::default());
    assert_eq!(cfg_sw.primary, PlatformRendererKind::Software);
    assert!(!cfg_sw.windows_borderless_fullscreen);
}

#[test]
fn renderer_overrides_respect_env_cli_and_mpv_conf_precedence() {
    let hints = parse_mpv_conf_renderer_hints(
        r#"
        hwdec=vaapi
        gpu-api=opengl
        [profile.test]
        hwdec=no
        "#,
    );
    let overrides = RendererOverrides::from_hints_args_and_env(
        hints,
        ["memento", "--hwdec=nvdec-copy", "--{", "--hwdec=no", "--}", "video.mkv"],
        env_from(&[("SLINT_BACKEND", "winit-software")]),
    );
    assert_eq!(
        overrides.explicit_renderer,
        Some(PlatformRendererKind::Software)
    );
    assert_eq!(overrides.requested_hwdec(), "nvdec-copy");

    let caps = VideoRendererCapabilities::for_linux(DisplayServer::Wayland, true, true);
    let cfg = PlatformRendererConfig::select(&caps, &overrides);
    assert_eq!(cfg.primary, PlatformRendererKind::Software);
    assert_eq!(cfg.fallback_chain, vec![PlatformRendererKind::Software]);
}

// ── 2. macOS Metal Layer Geometry, Options & Triple-Buffered IOSurface Ring ─

#[test]
fn metal_layer_geometry_handles_hidpi_scale_clamping_and_validation() {
    let retina =
        MetalLayerGeometry::from_logical(1280.0, 720.0, 2.0, MetalPixelFormat::Bgra8Unorm).unwrap();
    assert_eq!(retina.drawable_size(), (2560, 1440));
    assert_eq!(retina.contents_scale, 2.0);
    assert!(!retina.wants_extended_dynamic_range);
    assert!(retina.framebuffer_only);

    let fractional =
        MetalLayerGeometry::from_logical(1000.0, 500.0, 1.5, MetalPixelFormat::Rgba16Float)
            .unwrap();
    assert_eq!(fractional.drawable_size(), (1500, 750));
    assert!(fractional.wants_extended_dynamic_range);

    assert_eq!(sanitize_contents_scale(f64::NAN), 1.0);
    assert_eq!(sanitize_contents_scale(-2.0), 1.0);
    assert_eq!(sanitize_contents_scale(16.0), 8.0);
    assert_eq!(sanitize_contents_scale(0.1), 0.5);

    // Oversized drawable clamps to Metal's 16384 limit preserving 16:9 aspect ratio.
    assert_eq!(
        clamp_metal_drawable_size(32_768, 18_432),
        (MTL_MAX_TEXTURE_DIMENSION_2D, 9_216)
    );
    let clamped =
        MetalLayerGeometry::from_logical(10_000.0, 5_000.0, 4.0, MetalPixelFormat::Bgra8Unorm)
            .unwrap();
    assert_eq!(clamped.drawable_size(), (16_384, 8_192));

    assert!(MetalLayerGeometry::from_logical(0.0, 720.0, 2.0, MetalPixelFormat::Bgra8Unorm).is_err());
    assert!(
        MetalLayerGeometry::from_logical(f64::NAN, 720.0, 2.0, MetalPixelFormat::Bgra8Unorm).is_err()
    );
    assert!(
        MetalLayerGeometry::from_physical_pixels(0, 1080, 2.0, MetalPixelFormat::Bgra8Unorm).is_err()
    );

    let same =
        MetalLayerGeometry::from_logical(1280.0, 720.0, 2.0, MetalPixelFormat::Bgra8Unorm).unwrap();
    assert!(!retina.needs_reconfigure(&same));
    assert!(retina.needs_reconfigure(&fractional));
}

#[test]
fn metal_pixel_formats_and_iosurface_descriptors_match_upstream_and_align_rows() {
    assert_eq!(
        select_metal_pixel_format(false, true),
        MetalPixelFormat::Bgra8Unorm
    );
    assert_eq!(
        select_metal_pixel_format(true, false),
        MetalPixelFormat::Bgra8Unorm
    );
    assert_eq!(
        select_metal_pixel_format(true, true),
        MetalPixelFormat::Rgba16Float
    );

    // Odd width 1281px * 4 bytes = 5124 -> 16-byte aligned to 5136 bytes/row.
    let bgra = IoSurfaceDescriptor::for_size(1281, 720, MetalPixelFormat::Bgra8Unorm).unwrap();
    assert_eq!(bgra.bytes_per_element, 4);
    assert_eq!(bgra.bytes_per_row, 5136);
    assert_eq!(bgra.bytes_per_row % 16, 0);
    assert_eq!(bgra.alloc_size, 5136 * 720);
    assert_eq!(bgra.cv_pixel_format, CV_PIXEL_FORMAT_32_BGRA);
    assert_eq!(bgra.mtl_pixel_format, MTL_PIXEL_FORMAT_BGRA8_UNORM);

    let hdr = IoSurfaceDescriptor::for_size(1920, 1080, MetalPixelFormat::Rgba16Float).unwrap();
    assert_eq!(hdr.bytes_per_element, 8);
    assert_eq!(hdr.bytes_per_row, 1920 * 8);
    assert_eq!(hdr.cv_pixel_format, CV_PIXEL_FORMAT_64_RGBA_HALF);
    assert_eq!(hdr.mtl_pixel_format, MTL_PIXEL_FORMAT_RGBA16_FLOAT);

    assert!(IoSurfaceDescriptor::for_size(0, 1080, MetalPixelFormat::Bgra8Unorm).is_err());
    assert!(
        IoSurfaceDescriptor::for_size(MTL_MAX_TEXTURE_DIMENSION_2D + 1, 1080, MetalPixelFormat::Bgra8Unorm)
            .is_err()
    );
}

#[test]
fn metal_mpv_option_planner_handles_iosurface_bridge_moltenvk_and_hwdec_overrides() {
    let bridge = plan_metal_mpv_options(
        MetalPresentationMode::IoSurfaceOpenGlBridge,
        Some("auto-safe"),
        false,
    );
    assert!(bridge.requires_cgl_worker);
    assert!(bridge.requires_iosurface_bridge);
    assert!(!bridge.requires_cametal_layer);
    assert!(bridge.options.contains(&("vo".into(), "libmpv".into())));
    assert!(bridge
        .options
        .contains(&("hwdec".into(), "videotoolbox,auto-safe".into())));

    let native = plan_metal_mpv_options(MetalPresentationMode::NativeMetalLayer, None, true);
    assert!(!native.requires_cgl_worker);
    assert!(native.requires_cametal_layer);
    assert!(native.options.contains(&("vo".into(), "gpu-next".into())));
    assert!(native
        .options
        .contains(&("gpu-api".into(), "vulkan".into())));
    assert!(native
        .options
        .contains(&("gpu-context".into(), "macvk".into())));
    assert!(native
        .options
        .contains(&("target-colorspace-hint".into(), "yes".into())));

    // Explicit hwdec=no is respected; Linux/Windows decoders adapt to videotoolbox.
    assert_eq!(
        resolve_macos_metal_hwdec(MetalPresentationMode::IoSurfaceOpenGlBridge, Some("no")),
        "no"
    );
    assert_eq!(
        resolve_macos_metal_hwdec(
            MetalPresentationMode::IoSurfaceOpenGlBridge,
            Some("nvdec,vaapi")
        ),
        "videotoolbox,videotoolbox"
    );
}

#[test]
fn metal_triple_buffer_ring_retires_old_generations_and_caches_textures() {
    let mut ring: MetalSurfaceRing<u64> = MetalSurfaceRing::new();
    let mut next_surface_id = 100u64;

    assert!(ring.set_pixel_size(1920, 1080));
    assert!(!ring.set_pixel_size(1920, 1080)); // duplicate resize is a no-op
    assert_eq!(ring.pending_resize(), Some((1920, 1080)));

    ring.recreate_generation(
        1920,
        1080,
        |_w, _h, idx| -> Result<MetalFrameSlot<u64>, &'static str> {
            next_surface_id += 1;
            Ok(MetalFrameSlot {
                surface: next_surface_id,
                gl_texture: 10 + idx as u32,
                gl_framebuffer: 20 + idx as u32,
                serial: 0,
            })
        },
        |_| {},
    )
    .unwrap();

    assert_eq!(ring.active_slot_count(), METAL_FRAME_SLOT_COUNT);
    assert_eq!(ring.retired_generation_count(), 0);
    assert!(ring.latest_published().is_none());

    // Round-robin across 3 slots with monotonic frame serials.
    let mut cache: MetalTextureCache<u64, String> = MetalTextureCache::new();
    let mut created_textures = 0usize;
    let mut expected_indices = vec![0usize, 1, 2, 0];
    for (step, expected_idx) in expected_indices.drain(..).enumerate() {
        let (idx, size) = ring.begin_write_slot().unwrap();
        assert_eq!(idx, expected_idx);
        assert_eq!(size, (1920, 1080));
        let serial = ring.publish_slot(idx).unwrap();
        assert_eq!(serial, step as u64 + 1);

        let (slot, latest_size) = ring.latest_published().unwrap();
        assert_eq!(latest_size, (1920, 1080));
        let tex = cache
            .get_or_create(slot.surface, |s| -> Result<String, ()> {
                created_textures += 1;
                Ok(format!("mtl_tex_{s}"))
            })
            .unwrap();
        assert_eq!(tex, &format!("mtl_tex_{}", slot.surface));
    }
    // 4 frames across 3 slots -> only 3 MTLTextures allocated (4th hit cache!).
    assert_eq!(created_textures, METAL_FRAME_SLOT_COUNT);
    assert_eq!(cache.len(), METAL_FRAME_SLOT_COUNT);

    // Partial failure during resize must clean up partial slots and preserve active generation.
    let mut cleaned = Vec::new();
    let fail = ring.recreate_generation(
        2560,
        1440,
        |_w, _h, idx| -> Result<MetalFrameSlot<u64>, &'static str> {
            if idx == 2 {
                return Err("IOSurface allocation failed");
            }
            next_surface_id += 1;
            Ok(MetalFrameSlot {
                surface: next_surface_id,
                gl_texture: 0,
                gl_framebuffer: 0,
                serial: 0,
            })
        },
        |partial| cleaned.push(partial.surface),
    );
    assert!(fail.is_err());
    assert_eq!(cleaned.len(), 2);
    assert_eq!(ring.active_size(), Some((1920, 1080)));
    assert_eq!(ring.retired_generation_count(), 0);

    // Successful resize retires the first generation rather than freeing it mid-flight.
    assert!(ring.set_pixel_size(2560, 1440));
    ring.recreate_generation(
        2560,
        1440,
        |_w, _h, idx| -> Result<MetalFrameSlot<u64>, &'static str> {
            next_surface_id += 1;
            Ok(MetalFrameSlot {
                surface: next_surface_id,
                gl_texture: 30 + idx as u32,
                gl_framebuffer: 40 + idx as u32,
                serial: 0,
            })
        },
        |_| {},
    )
    .unwrap();
    assert_eq!(ring.active_size(), Some((2560, 1440)));
    assert_eq!(ring.retired_generation_count(), 1);
    assert!(ring.latest_published().is_none());

    // Shutdown drains BOTH active (3) and retired (3) slots = 6 total.
    let drained = ring.drain_for_shutdown();
    assert_eq!(drained.len(), METAL_FRAME_SLOT_COUNT * 2);
    assert_eq!(ring.active_slot_count(), 0);
    assert_eq!(ring.retired_generation_count(), 0);

    let mut destroyed_cache_entries = 0usize;
    cache.clear_with(|_, _| destroyed_cache_entries += 1);
    assert_eq!(destroyed_cache_entries, METAL_FRAME_SLOT_COUNT);
    assert!(cache.is_empty());
}

// ── 3. Windows Borderless Fullscreen +1px Multi-Monitor State Machine ───────

#[test]
fn fullscreen_strategy_only_enables_windows_borderless_plus_one_for_windows_opengl() {
    assert_eq!(
        select_fullscreen_strategy(TargetOs::Windows, PlatformRendererKind::OpenGl, false),
        FullscreenStrategy::WindowsBorderlessPlusOne
    );
    assert_eq!(
        select_fullscreen_strategy(TargetOs::Windows, PlatformRendererKind::OpenGl, true),
        FullscreenStrategy::NativeWindowFullscreen
    );
    assert_eq!(
        select_fullscreen_strategy(TargetOs::Windows, PlatformRendererKind::Software, false),
        FullscreenStrategy::NativeWindowFullscreen
    );
    assert_eq!(
        select_fullscreen_strategy(TargetOs::Linux, PlatformRendererKind::OpenGl, false),
        FullscreenStrategy::NativeWindowFullscreen
    );
    assert_eq!(
        select_fullscreen_strategy(TargetOs::MacOs, PlatformRendererKind::Metal, false),
        FullscreenStrategy::NativeWindowFullscreen
    );
}

#[test]
fn windows_borderless_fullscreen_enter_exit_restore_across_multi_monitor_offsets() {
    let mut ctrl =
        PlatformFullscreenController::for_platform(TargetOs::Windows, PlatformRendererKind::OpenGl, false);
    assert!(!ctrl.is_fullscreen());
    assert_eq!(ctrl.bottom_margin_px(), 0);
    assert!(ctrl.decorations_enabled());

    // Secondary monitor positioned to the LEFT and slightly ABOVE primary monitor.
    let left_monitor = MonitorRect::new(-2560, -120, 2560, 1440, 1.25);
    let saved_window = WindowRect::new(-2400, 40, 1280, 720);

    let enter = ctrl.enter_fullscreen(saved_window, false, left_monitor);
    assert_eq!(
        enter,
        FullscreenAction::EnterWindowsBorderless {
            unmaximize_first: false,
            frameless: true,
            target_rect: WindowRect::new(-2560, -120, 2560, 1441),
            bottom_margin_px: WINDOWS_OPENGL_FULLSCREEN_EXTRA_HEIGHT_PX,
        }
    );
    assert!(ctrl.is_fullscreen());
    assert_eq!(ctrl.bottom_margin_px(), 1);
    assert!(!ctrl.decorations_enabled());
    assert_eq!(ctrl.saved_geometry(), Some(saved_window));
    assert!(!ctrl.saved_maximized());

    // Duplicate enter must be NoOp and must NOT overwrite saved_geometry.
    let dup = ctrl.enter_fullscreen(WindowRect::new(0, 0, 100, 100), true, MonitorRect::default());
    assert_eq!(dup, FullscreenAction::NoOp);
    assert_eq!(ctrl.saved_geometry(), Some(saved_window));
    assert!(!ctrl.saved_maximized());

    // Monitor hotplug / DPI change while fullscreen updates target without clobbering saved_geometry.
    let right_4k = MonitorRect::new(1920, 0, 3840, 2160, 2.0);
    let moved = ctrl.on_monitor_geometry_changed(right_4k);
    assert_eq!(
        moved,
        FullscreenAction::EnterWindowsBorderless {
            unmaximize_first: false,
            frameless: true,
            target_rect: WindowRect::new(1920, 0, 3840, 2161),
            bottom_margin_px: 1,
        }
    );
    assert_eq!(ctrl.saved_geometry(), Some(saved_window));

    // Exit restores window chrome, resets bottom margin, and restores original multi-monitor rect.
    let exit = ctrl.exit_fullscreen();
    assert_eq!(
        exit,
        FullscreenAction::ExitWindowsBorderless {
            restore_decorations: true,
            bottom_margin_px: 0,
            restore_rect: Some(saved_window),
            restore_maximized: false,
        }
    );
    assert!(!ctrl.is_fullscreen());
    assert_eq!(ctrl.bottom_margin_px(), 0);
    assert!(ctrl.decorations_enabled());
    assert_eq!(ctrl.saved_geometry(), None);

    // Duplicate exit is NoOp.
    assert_eq!(ctrl.exit_fullscreen(), FullscreenAction::NoOp);
}

#[test]
fn windows_borderless_fullscreen_preserves_maximized_state_and_win32_style_helpers() {
    let mut ctrl =
        PlatformFullscreenController::for_platform(TargetOs::Windows, PlatformRendererKind::OpenGl, false);
    let primary = MonitorRect::new(0, 0, 1920, 1080, 1.0);
    let normal_before_max = WindowRect::new(100, 120, 1280, 720);

    let enter = ctrl.toggle_fullscreen(normal_before_max, true, primary);
    assert_eq!(
        enter,
        FullscreenAction::EnterWindowsBorderless {
            unmaximize_first: true,
            frameless: true,
            target_rect: compute_windows_borderless_fullscreen_rect(primary),
            bottom_margin_px: 1,
        }
    );
    assert!(ctrl.saved_maximized());

    let exit = ctrl.toggle_fullscreen(WindowRect::new(0, 0, 1920, 1081), false, primary);
    assert_eq!(
        exit,
        FullscreenAction::ExitWindowsBorderless {
            restore_decorations: true,
            bottom_margin_px: 0,
            restore_rect: Some(normal_before_max),
            restore_maximized: true,
        }
    );

    // Verify Win32 style stripping clears frame chrome while preserving visibility flags.
    let (style, ex_style) = strip_win32_window_frame_styles(0x10CF_0000, 0x0004_0301);
    assert_eq!(style & WIN32_BORDERLESS_STRIP_STYLE_MASK, 0);
    assert_eq!(ex_style & WIN32_BORDERLESS_STRIP_EX_STYLE_MASK, 0);
}

#[test]
fn auto_fit_window_centers_on_multi_monitor_and_skips_fullscreen_or_maximized() {
    let secondary = MonitorRect::new(-1920, 0, 1920, 1080, 1.0);
    let rect = compute_auto_fit_window_rect(1280, 720, 320, 100, secondary, false, false).unwrap();
    // 1280 + 320 = 1600 wide centered in [-1920, 0] -> x = -1920 + 160 = -1760
    assert_eq!(rect, WindowRect::new(-1760, 180, 1600, 720));

    // Oversized 4K video scales down proportionally to fit 1080p monitor.
    let scaled = compute_auto_fit_window_rect(3840, 2160, 0, 100, secondary, false, false).unwrap();
    assert_eq!(scaled, WindowRect::new(-1920, 0, 1920, 1080));

    // Must not resize while fullscreen or maximized.
    assert!(compute_auto_fit_window_rect(1280, 720, 0, 100, secondary, true, false).is_none());
    assert!(compute_auto_fit_window_rect(1280, 720, 0, 100, secondary, false, true).is_none());
}

// ── 4. Linux OpenGL / Wayland / X11 / Hardware Decode Hardening ─────────────

#[test]
fn linux_display_server_and_graphics_device_planning_cover_wayland_x11_nvidia_and_vaapi() {
    assert_eq!(
        detect_linux_display_server_with(env_from(&[("WAYLAND_DISPLAY", "wayland-0")])),
        LinuxDisplayServer::Wayland
    );
    assert_eq!(
        detect_linux_display_server_with(env_from(&[
            ("WAYLAND_DISPLAY", "wayland-0"),
            ("DISPLAY", ":0"),
            ("XDG_SESSION_TYPE", "wayland"),
        ])),
        LinuxDisplayServer::WaylandWithX11Fallback
    );
    assert_eq!(
        detect_linux_display_server_with(env_from(&[
            ("WAYLAND_DISPLAY", "wayland-0"),
            ("DISPLAY", ":0"),
            ("XDG_SESSION_TYPE", "x11"),
        ])),
        LinuxDisplayServer::X11
    );
    assert_eq!(
        detect_linux_display_server_with(env_from(&[("DISPLAY", ":1")])),
        LinuxDisplayServer::X11
    );
    assert_eq!(
        detect_linux_display_server_with(env_from(&[])),
        LinuxDisplayServer::Headless
    );

    assert!(decoder_requests_direct_nvidia("nvdec"));
    assert!(decoder_requests_direct_nvidia("cuda,auto"));
    assert!(!decoder_requests_direct_nvidia("nvdec-copy"));
    assert!(!decoder_requests_direct_nvidia("auto-safe"));

    // Direct nvdec on X11 with NVIDIA driver -> PRIME offload + GLX preference.
    let nv_x11 = plan_linux_graphics_device(
        "nvdec",
        LinuxDisplayServer::X11,
        true,
        false,
        false,
        Some(PathBuf::from("/usr/share/glvnd/egl_vendor.d/10_nvidia.json")),
    );
    assert!(nv_x11.request_nvidia_prime_offload);
    assert_eq!(nv_x11.preferred_gl_api, LinuxGlContextApi::Glx);

    // Wayland always prefers EGL; LIBGL_ALWAYS_SOFTWARE disables PRIME offload.
    let vaapi_wl = plan_linux_graphics_device(
        "vaapi",
        LinuxDisplayServer::Wayland,
        true,
        false,
        false,
        None,
    );
    assert!(!vaapi_wl.request_nvidia_prime_offload);
    assert_eq!(vaapi_wl.preferred_gl_api, LinuxGlContextApi::Egl);

    let sw_override = plan_linux_graphics_device(
        "nvdec",
        LinuxDisplayServer::X11,
        true,
        false,
        true,
        None,
    );
    assert!(!sw_override.request_nvidia_prime_offload);
    assert!(sw_override.force_software_gl);

    assert!(is_software_gl_renderer("llvmpipe (LLVM 18.1.8, 256 bits)"));
    assert!(is_software_gl_renderer("Google SwiftShader"));
    assert!(!is_software_gl_renderer("NVIDIA GeForce RTX 4080"));
    assert!(is_nvidia_gl_renderer("NVIDIA GeForce RTX 4080"));
    assert!(is_vaapi_capable_renderer("Mesa Intel(R) Iris(R) Xe Graphics"));
    assert!(is_vaapi_capable_renderer("AMD Radeon RX 7800 XT"));
    assert!(!is_vaapi_capable_renderer("llvmpipe"));

    assert_eq!(
        configured_option_from_args(
            ["memento", "--gpu-api=opengl", "--{", "--gpu-api=vulkan", "--}", "a.mkv"],
            "gpu-api"
        )
        .as_deref(),
        Some("opengl")
    );
}

#[test]
fn video_gl_fbo_format_plan_dimension_validation_and_resize_hysteresis() {
    assert_eq!(
        plan_video_fbo_formats(true),
        vec![VideoFboFormat::Rgba16F, VideoFboFormat::Rgba8]
    );
    assert_eq!(plan_video_fbo_formats(false), vec![VideoFboFormat::Rgba8]);

    assert_eq!(validate_fbo_dimensions(1920, 1080, Some(16384)), Ok((1920, 1080)));
    assert!(validate_fbo_dimensions(0, 1080, Some(16384)).is_err());
    assert!(validate_fbo_dimensions(1920, 0, Some(16384)).is_err());
    assert!(validate_fbo_dimensions(u32::MAX, 1080, None).is_err());
    assert!(validate_fbo_dimensions(16385, 1080, Some(16384)).is_err());

    assert_eq!(clamp_fbo_dimensions(8192, 4096, 4096), (4096, 2048));
    assert_eq!(clamp_fbo_dimensions(1920, 1080, 16384), (1920, 1080));

    // Initial allocation needed when target missing.
    assert!(should_reallocate_fbo(false, (0, 0), (1280, 720)));
    // Zero request never triggers allocation.
    assert!(!should_reallocate_fbo(false, (0, 0), (0, 0)));
    // Exact size match does not reallocate.
    assert!(!should_reallocate_fbo(true, (1280, 720), (1280, 720)));
    // Growing always reallocates immediately.
    assert!(should_reallocate_fbo(true, (1280, 720), (1282, 720)));
    // Small shrink preserving aspect stays within 64px hysteresis window.
    assert!(!should_reallocate_fbo(true, (1280, 720), (1248, 702)));
    // Large shrink >=64px reallocates to reclaim VRAM.
    assert!(should_reallocate_fbo(true, (1280, 720), (1200, 675)));
    // Aspect-ratio shift reallocates even for small pixel deltas.
    assert!(should_reallocate_fbo(true, (1280, 720), (1260, 720)));
}

#[test]
fn mpv_hwdec_compatibility_and_render_context_lifecycle_guard_against_cuda_hazards() {
    // Non-NVIDIA hardware GL rewrites NVIDIA/VDPAU direct decoders to copyback.
    assert_eq!(compatible_opengl_hwdec("nvdec", "Mesa Intel UHD 630"), "nvdec-copy");
    assert_eq!(compatible_opengl_hwdec("cuda,vaapi", "AMD Radeon RX"), "cuda-copy,vaapi");
    assert_eq!(compatible_opengl_hwdec("vdpau", "Mesa Intel UHD 630"), "vdpau-copy");
    assert_eq!(compatible_opengl_hwdec("vaapi", "Mesa Intel UHD 630"), "vaapi");

    // NVIDIA default auto-safe uses standalone copyback CUDA context to avoid
    // cuCtxPushCurrent(decoder->cuda_ctx) failures on shared GUI GL contexts.
    assert_eq!(
        compatible_opengl_hwdec_for_session("auto-safe", "NVIDIA GeForce RTX 4090", false),
        "nvdec-copy,auto-copy-safe"
    );
    // Explicit nvdec opt-in is preserved on X11, but converted to nvdec-copy on Wayland EGL.
    assert_eq!(
        compatible_opengl_hwdec_for_session("nvdec", "NVIDIA GeForce RTX 4090", false),
        "nvdec"
    );
    assert_eq!(
        compatible_opengl_hwdec_for_session("nvdec", "NVIDIA GeForce RTX 4090", true),
        "nvdec-copy"
    );
    assert_eq!(
        compatible_opengl_hwdec_for_session("cuda", "NVIDIA GeForce RTX 4090", true),
        "cuda-copy"
    );

    // Software GL & CPU software renderer convert all direct GPU decoders to copyback.
    assert_eq!(
        compatible_opengl_hwdec_for_session("vaapi,nvdec", "llvmpipe (LLVM 18)", false),
        "vaapi-copy,nvdec-copy"
    );
    assert_eq!(
        compatible_software_hwdec("auto-safe"),
        "auto-copy-safe"
    );

    // Live MpvInstance lifecycle: invalid GL dimensions rejected, inactive GL rejected,
    // fallback/deactivate are idempotent, and Drop cleanly frees render_context before mpv core.
    let dir = tempfile::TempDir::new().unwrap();
    std::fs::write(dir.path().join("input.conf"), "").unwrap();
    std::fs::write(dir.path().join("mpv.conf"), "ao=null\nhwdec=no\n").unwrap();
    let player = MpvInstance::new_software(Some(dir.path())).unwrap();
    assert!(!player.is_opengl_active());
    assert_eq!(player.active_display_backend(), GlDisplayBackend::Unknown);
    assert!(!player.is_advanced_control_active());
    assert!(player.render_opengl_with_format(1, 0, 720, 0).is_err());
    assert!(player.render_opengl_with_format(1, 1280, 0, 0).is_err());
    assert!(player.render_opengl_with_format(1, 1280, 720, 0).is_err());
    assert!(player.fallback_to_software().is_ok());
    player.deactivate_opengl();
    let _ = player.pump_render_update();
    drop(player);
}

#[cfg(target_os = "linux")]
#[test]
fn live_egl_opengl_state_guard_fbo_resize_and_mpv_render_context_teardown() {
    use glow::HasContext;
    use memento_app::video_gl::{
        create_video_fbo_texture, create_video_fbo_texture_with_preference, with_saved_state,
    };
    use std::ffi::{c_char, c_int, c_void, CStr};
    use std::ptr;

    unsafe {
        let Ok(egl) = libloading::Library::new("libEGL.so.1") else {
            return;
        };
        let Ok(display_fn) = egl.get::<unsafe extern "C" fn(u32, *mut c_void, *const isize) -> *mut c_void>(
            b"eglGetPlatformDisplay\0",
        ) else {
            return;
        };
        let Ok(initialize) =
            egl.get::<unsafe extern "C" fn(*mut c_void, *mut c_int, *mut c_int) -> u32>(
                b"eglInitialize\0",
            )
        else {
            return;
        };
        let Ok(bind_api) = egl.get::<unsafe extern "C" fn(u32) -> u32>(b"eglBindAPI\0") else {
            return;
        };
        let Ok(choose) = egl.get::<unsafe extern "C" fn(
            *mut c_void,
            *const c_int,
            *mut *mut c_void,
            c_int,
            *mut c_int,
        ) -> u32>(b"eglChooseConfig\0") else {
            return;
        };
        let Ok(create_ctx) = egl.get::<unsafe extern "C" fn(
            *mut c_void,
            *mut c_void,
            *mut c_void,
            *const c_int,
        ) -> *mut c_void>(b"eglCreateContext\0") else {
            return;
        };
        let Ok(create_pbuf) = egl.get::<unsafe extern "C" fn(
            *mut c_void,
            *mut c_void,
            *const c_int,
        ) -> *mut c_void>(b"eglCreatePbufferSurface\0") else {
            return;
        };
        let Ok(make_current) = egl.get::<unsafe extern "C" fn(
            *mut c_void,
            *mut c_void,
            *mut c_void,
            *mut c_void,
        ) -> u32>(b"eglMakeCurrent\0") else {
            return;
        };
        let Ok(get_proc) =
            egl.get::<unsafe extern "C" fn(*const c_char) -> *const c_void>(b"eglGetProcAddress\0")
        else {
            return;
        };
        let Ok(destroy_ctx) =
            egl.get::<unsafe extern "C" fn(*mut c_void, *mut c_void) -> u32>(b"eglDestroyContext\0")
        else {
            return;
        };
        let Ok(destroy_surf) =
            egl.get::<unsafe extern "C" fn(*mut c_void, *mut c_void) -> u32>(b"eglDestroySurface\0")
        else {
            return;
        };
        let Ok(terminate) = egl.get::<unsafe extern "C" fn(*mut c_void) -> u32>(b"eglTerminate\0")
        else {
            return;
        };

        let display = display_fn(0x31DD /* EGL_PLATFORM_SURFACELESS_MESA */, ptr::null_mut(), ptr::null());
        if display.is_null() || initialize(display, ptr::null_mut(), ptr::null_mut()) == 0 {
            return;
        }
        if bind_api(0x30A2 /* EGL_OPENGL_API */) == 0 {
            terminate(display);
            return;
        }
        let mut config = ptr::null_mut();
        let mut count = 0;
        let ok = choose(
            display,
            [0x3033, 1, 0x3040, 8, 0x3038].as_ptr(),
            &mut config,
            1,
            &mut count,
        );
        if ok == 0 || count == 0 {
            terminate(display);
            return;
        }
        let context = create_ctx(display, config, ptr::null_mut(), [0x3098, 3, 0x30FB, 3, 0x3038].as_ptr());
        let surface = create_pbuf(display, config, [0x3057, 64, 0x3056, 64, 0x3038].as_ptr());
        if context.is_null() || surface.is_null() || make_current(display, surface, surface, context) == 0 {
            if !context.is_null() {
                destroy_ctx(display, context);
            }
            if !surface.is_null() {
                destroy_surf(display, surface);
            }
            terminate(display);
            return;
        }

        let gl = glow::Context::from_loader_function_cstr(|name: &CStr| get_proc(name.as_ptr()));

        // 1. Test FBO creation (RGBA16F preferred -> RGBA8 fallback), resize, and explicit RGBA8.
        let target = create_video_fbo_texture(&gl, None, None, 640, 360).unwrap();
        assert!(
            target.internal_format == glow::RGBA16F as i32
                || target.internal_format == glow::RGBA8 as i32
        );
        let resized = create_video_fbo_texture(
            &gl,
            Some(target.texture),
            Some(target.framebuffer),
            1280,
            720,
        )
        .unwrap();
        assert_eq!(resized.texture, target.texture);
        assert_eq!(resized.framebuffer, target.framebuffer);

        let forced_8 =
            create_video_fbo_texture_with_preference(&gl, None, None, 320, 180, false).unwrap();
        assert_eq!(forced_8.internal_format, glow::RGBA8 as i32);

        let max_tex = gl.get_parameter_i32(glow::MAX_TEXTURE_SIZE) as u32;
        assert!(create_video_fbo_texture(&gl, None, None, max_tex.saturating_add(1), 64).is_err());

        // 2. Test with_saved_state resets GL to libmpv defaults and restores every UI binding.
        let tex0 = gl.create_texture().unwrap();
        let tex1 = gl.create_texture().unwrap();
        let pbo = gl.create_buffer().unwrap();
        gl.bind_framebuffer(glow::FRAMEBUFFER, Some(resized.framebuffer));
        gl.active_texture(glow::TEXTURE0);
        gl.bind_texture(glow::TEXTURE_2D, Some(tex0));
        gl.active_texture(glow::TEXTURE1);
        gl.bind_texture(glow::TEXTURE_2D, Some(tex1));
        gl.bind_buffer(glow::PIXEL_UNPACK_BUFFER, Some(pbo));
        gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 1);
        gl.pixel_store_i32(glow::UNPACK_ROW_LENGTH, 64);
        gl.viewport(10, 20, 320, 240);
        gl.scissor(5, 6, 100, 80);
        gl.enable(glow::BLEND);
        gl.enable(glow::SCISSOR_TEST);
        gl.enable(glow::STENCIL_TEST);
        gl.enable(glow::DEPTH_TEST);
        gl.enable(glow::CULL_FACE);
        gl.color_mask(false, true, false, true);

        with_saved_state(&gl, || {
            // Inside callback, libmpv sees clean OpenGL defaults.
            assert!(!gl.is_enabled(glow::BLEND));
            assert!(!gl.is_enabled(glow::SCISSOR_TEST));
            assert!(!gl.is_enabled(glow::STENCIL_TEST));
            assert!(!gl.is_enabled(glow::DEPTH_TEST));
            assert!(!gl.is_enabled(glow::CULL_FACE));
            let mut mask = [0i32; 4];
            gl.get_parameter_i32_slice(glow::COLOR_WRITEMASK, &mut mask);
            assert_eq!(mask, [1, 1, 1, 1]);
            assert_eq!(gl.get_parameter_i32(glow::PIXEL_UNPACK_BUFFER_BINDING), 0);
            assert_eq!(gl.get_parameter_i32(glow::UNPACK_ROW_LENGTH), 0);

            // Simulate libmpv mutating bindings and state aggressively.
            gl.bind_framebuffer(glow::FRAMEBUFFER, None);
            gl.active_texture(glow::TEXTURE0);
            gl.bind_texture(glow::TEXTURE_2D, None);
            gl.viewport(0, 0, 1280, 720);
            gl.scissor(0, 0, 1280, 720);
            gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 8);
            gl.pixel_store_i32(glow::UNPACK_ROW_LENGTH, 128);
            gl.enable(glow::BLEND);
            gl.disable(glow::DITHER);
            gl.color_mask(true, false, true, false);
        });

        // After callback, every UI renderer binding and state bit is restored verbatim.
        assert_eq!(
            gl.get_parameter_i32(glow::DRAW_FRAMEBUFFER_BINDING) as u32,
            resized.framebuffer.0.get()
        );
        assert_eq!(gl.get_parameter_i32(glow::ACTIVE_TEXTURE) as u32, glow::TEXTURE1);
        assert_eq!(
            gl.get_parameter_i32(glow::TEXTURE_BINDING_2D) as u32,
            tex1.0.get()
        );
        gl.active_texture(glow::TEXTURE0);
        assert_eq!(
            gl.get_parameter_i32(glow::TEXTURE_BINDING_2D) as u32,
            tex0.0.get()
        );
        gl.active_texture(glow::TEXTURE1);
        assert_eq!(
            gl.get_parameter_i32(glow::PIXEL_UNPACK_BUFFER_BINDING) as u32,
            pbo.0.get()
        );
        assert_eq!(gl.get_parameter_i32(glow::UNPACK_ALIGNMENT), 1);
        assert_eq!(gl.get_parameter_i32(glow::UNPACK_ROW_LENGTH), 64);
        let mut vp = [0i32; 4];
        gl.get_parameter_i32_slice(glow::VIEWPORT, &mut vp);
        assert_eq!(vp, [10, 20, 320, 240]);
        let mut sc = [0i32; 4];
        gl.get_parameter_i32_slice(glow::SCISSOR_BOX, &mut sc);
        assert_eq!(sc, [5, 6, 100, 80]);
        assert!(gl.is_enabled(glow::BLEND));
        assert!(gl.is_enabled(glow::SCISSOR_TEST));
        assert!(gl.is_enabled(glow::STENCIL_TEST));
        assert!(gl.is_enabled(glow::DEPTH_TEST));
        assert!(gl.is_enabled(glow::CULL_FACE));
        let mut restored_mask = [0i32; 4];
        gl.get_parameter_i32_slice(glow::COLOR_WRITEMASK, &mut restored_mask);
        assert_eq!(restored_mask, [0, 1, 0, 1]);

        // 3. Activate real libmpv OpenGL render context inside with_saved_state (proving
        //    even dirty UI PBO/unpack/scissor/stencil state won't poison libmpv's FBO probe!),
        //    render into FBO, deactivate inside with_saved_state, and drop cleanly.
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join("input.conf"), "").unwrap();
        std::fs::write(dir.path().join("mpv.conf"), "ao=null\nhwdec=no\n").unwrap();
        let player = MpvInstance::new_software(Some(dir.path())).unwrap();
        let proc_fn = *get_proc;
        with_saved_state(&gl, || {
            player.activate_opengl_owned_with_display(
                move |name: &CStr| proc_fn(name.as_ptr()),
                None,
                None,
            )
        })
        .unwrap();
        assert!(player.is_opengl_active());
        assert!(player.is_advanced_control_active());
        let renderer_name = gl.get_parameter_string(glow::RENDERER);
        player.configure_opengl_decoder(&renderer_name).unwrap();
        with_saved_state(&gl, || {
            let _ = player.pump_render_update();
            player
                .render_opengl_with_format(
                    resized.framebuffer.0.get(),
                    1280,
                    720,
                    resized.internal_format,
                )
                .unwrap();
        });
        with_saved_state(&gl, || {
            player.deactivate_opengl();
        });
        assert!(!player.is_opengl_active());
        drop(player);

        gl.bind_framebuffer(glow::FRAMEBUFFER, None);
        gl.bind_buffer(glow::PIXEL_UNPACK_BUFFER, None);
        gl.delete_buffer(pbo);
        gl.delete_texture(tex0);
        gl.delete_texture(tex1);
        gl.delete_framebuffer(resized.framebuffer);
        gl.delete_texture(resized.texture);
        gl.delete_framebuffer(forced_8.framebuffer);
        gl.delete_texture(forced_8.texture);
        make_current(display, ptr::null_mut(), ptr::null_mut(), ptr::null_mut());
        destroy_ctx(display, context);
        destroy_surf(display, surface);
        terminate(display);
    }
}

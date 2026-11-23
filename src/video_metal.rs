//! macOS Metal video renderer bridge, IOSurface triple-buffered frame ring,
//! `CAMetalLayer` HiDPI geometry planning, and `mpv` Metal/MoltenVK options.
//!
//! Architecture parity with upstream C++ `MpvMetalRenderer` (`mpvmetalrenderer.mm`):
//! - Primary `IoSurfaceOpenGlBridge` mode: renders `libmpv` frames into a
//!   triple-buffered ring (`METAL_FRAME_SLOT_COUNT = 3`) of `IOSurface` objects
//!   bound to OpenGL `GL_TEXTURE_RECTANGLE` FBOs via `CGLTexImageIOSurface2D`,
//!   retains retired resize generations across a bounded grace window so Metal never
//!   observes an in-flight `IOSurface` being destroyed without leaking across resizes,
//!   and wraps each published `IOSurface` in a cached `MTLTexture`
//!   (`MTLPixelFormatBGRA8Unorm` or `MTLPixelFormatRGBA16Float`).
//! - Optional `NativeMetalLayer` mode: configures a `CAMetalLayer` with HiDPI
//!   `contentsScale`, clamped drawable dimensions, EDR/HDR pixel formats, and
//!   `mpv` `vo=gpu-next` / `gpu-api=vulkan` / `gpu-context=macvk` (MoltenVK)
//!   options when direct layer presentation is selected.
//!
//! All geometry, pixel-format, ring-buffer, cache, and option-planning logic is
//! platform-independent so it compiles and is thoroughly tested on Linux as well.

use std::collections::VecDeque;

/// Triple-buffered IOSurface slot count matching upstream `MpvMetalRenderer`.
pub const METAL_FRAME_SLOT_COUNT: usize = 3;

/// Number of published frames a retired resize generation is kept so in-flight
/// Metal/GPU commands finish sampling it before reclamation.
pub const METAL_RETIRED_GENERATION_GRACE_FRAMES: u64 = METAL_FRAME_SLOT_COUNT as u64;

/// Maximum number of retired resize generations retained concurrently so rapid
/// resize storms remain bounded even before new frames publish.
pub const METAL_MAX_RETIRED_GENERATIONS: usize = 2;

/// Maximum 2D texture dimension supported by Apple Silicon / modern Metal GPUs.
pub const MTL_MAX_TEXTURE_DIMENSION_2D: u32 = 16_384;

/// CoreVideo 32-bit BGRA FourCC (`'BGRA'`), matching `kCVPixelFormatType_32BGRA`.
pub const CV_PIXEL_FORMAT_32_BGRA: u32 = 0x4247_5241;

/// CoreVideo 64-bit half-float RGBA FourCC (`'RGhA'`), matching `kCVPixelFormatType_64RGBAHalf`.
pub const CV_PIXEL_FORMAT_64_RGBA_HALF: u32 = 0x5247_6841;

/// `MTLPixelFormatBGRA8Unorm` numeric enum value from Apple's Metal headers.
pub const MTL_PIXEL_FORMAT_BGRA8_UNORM: usize = 80;

/// `MTLPixelFormatRGBA16Float` numeric enum value from Apple's Metal headers.
pub const MTL_PIXEL_FORMAT_RGBA16_FLOAT: usize = 115;

/// `MTLTextureUsageShaderRead` bitmask value.
pub const MTL_TEXTURE_USAGE_SHADER_READ: usize = 0x0001;

/// `GL_TEXTURE_RECTANGLE` target required by `CGLTexImageIOSurface2D`.
pub const GL_TEXTURE_RECTANGLE_MACOS: u32 = 0x84F5;

/// `GL_BGRA` external format for `CGLTexImageIOSurface2D`.
pub const GL_BGRA_MACOS: u32 = 0x80E1;

/// `GL_UNSIGNED_INT_8_8_8_8_REV` pixel type for `kCVPixelFormatType_32BGRA`.
pub const GL_UNSIGNED_INT_8_8_8_8_REV_MACOS: u32 = 0x8367;

/// `GL_RGBA8` internal format constant.
pub const GL_RGBA8_MACOS: i32 = 0x8058;

/// `GL_RGBA16F` internal format constant.
pub const GL_RGBA16F_MACOS: i32 = 0x881A;

/// `GL_RGBA` external format constant.
pub const GL_RGBA_MACOS: u32 = 0x1908;

/// `GL_HALF_FLOAT` pixel type constant.
pub const GL_HALF_FLOAT_MACOS: u32 = 0x140B;

/// Minimum allowed HiDPI `contentsScale`.
pub const MIN_METAL_CONTENTS_SCALE: f64 = 0.5;

/// Maximum allowed HiDPI `contentsScale`.
pub const MAX_METAL_CONTENTS_SCALE: f64 = 8.0;

/// Metal / IOSurface pixel formats supported by Subomi's macOS bridge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MetalPixelFormat {
    /// Standard 8-bit sRGB/SDR BGRA (`kCVPixelFormatType_32BGRA` / `MTLPixelFormatBGRA8Unorm`).
    /// Matches upstream `mpvmetalrenderer.mm`.
    Bgra8Unorm,
    /// 16-bit half-float RGBA (`kCVPixelFormatType_64RGBAHalf` / `MTLPixelFormatRGBA16Float`)
    /// for 10-bit/HDR/anti-banding presentation when Extended Dynamic Range is enabled.
    Rgba16Float,
}

impl MetalPixelFormat {
    pub const fn mtl_pixel_format(self) -> usize {
        match self {
            Self::Bgra8Unorm => MTL_PIXEL_FORMAT_BGRA8_UNORM,
            Self::Rgba16Float => MTL_PIXEL_FORMAT_RGBA16_FLOAT,
        }
    }

    pub const fn cv_pixel_format(self) -> u32 {
        match self {
            Self::Bgra8Unorm => CV_PIXEL_FORMAT_32_BGRA,
            Self::Rgba16Float => CV_PIXEL_FORMAT_64_RGBA_HALF,
        }
    }

    pub const fn bytes_per_element(self) -> u32 {
        match self {
            Self::Bgra8Unorm => 4,
            Self::Rgba16Float => 8,
        }
    }

    pub const fn gl_internal_format(self) -> i32 {
        match self {
            Self::Bgra8Unorm => GL_RGBA8_MACOS,
            Self::Rgba16Float => GL_RGBA16F_MACOS,
        }
    }

    pub const fn gl_format(self) -> u32 {
        match self {
            Self::Bgra8Unorm => GL_BGRA_MACOS,
            Self::Rgba16Float => GL_RGBA_MACOS,
        }
    }

    pub const fn gl_type(self) -> u32 {
        match self {
            Self::Bgra8Unorm => GL_UNSIGNED_INT_8_8_8_8_REV_MACOS,
            Self::Rgba16Float => GL_HALF_FLOAT_MACOS,
        }
    }

    pub const fn supports_extended_dynamic_range(self) -> bool {
        matches!(self, Self::Rgba16Float)
    }
}

/// Select the Metal/IOSurface pixel format based on HDR/16-bit preference and display EDR support.
pub const fn select_metal_pixel_format(
    prefer_hdr_or_16f: bool,
    edr_supported: bool,
) -> MetalPixelFormat {
    if prefer_hdr_or_16f && edr_supported {
        MetalPixelFormat::Rgba16Float
    } else {
        MetalPixelFormat::Bgra8Unorm
    }
}

/// Sanitize a HiDPI scale factor into `[MIN_METAL_CONTENTS_SCALE, MAX_METAL_CONTENTS_SCALE]`.
pub fn sanitize_contents_scale(scale: f64) -> f64 {
    if !scale.is_finite() || scale <= 0.0 {
        1.0
    } else {
        scale.clamp(MIN_METAL_CONTENTS_SCALE, MAX_METAL_CONTENTS_SCALE)
    }
}

/// Clamp physical texture dimensions to Metal's 2D texture limits while preserving aspect ratio.
pub fn clamp_metal_drawable_size(width: u32, height: u32) -> (u32, u32) {
    let w = width.max(1);
    let h = height.max(1);
    let max_dim = w.max(h);
    if max_dim <= MTL_MAX_TEXTURE_DIMENSION_2D {
        return (w, h);
    }
    let ratio = MTL_MAX_TEXTURE_DIMENSION_2D as f64 / max_dim as f64;
    let clamped_w = ((w as f64 * ratio).round() as u32).clamp(1, MTL_MAX_TEXTURE_DIMENSION_2D);
    let clamped_h = ((h as f64 * ratio).round() as u32).clamp(1, MTL_MAX_TEXTURE_DIMENSION_2D);
    (clamped_w, clamped_h)
}

/// Resolved `CAMetalLayer` / Metal drawable sizing and format configuration.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MetalLayerGeometry {
    pub logical_width: f64,
    pub logical_height: f64,
    pub contents_scale: f64,
    pub drawable_width: u32,
    pub drawable_height: u32,
    pub pixel_format: MetalPixelFormat,
    pub wants_extended_dynamic_range: bool,
    pub framebuffer_only: bool,
}

impl MetalLayerGeometry {
    /// Compute HiDPI-aware Metal layer geometry from logical points and `contentsScale`.
    pub fn from_logical(
        logical_width: f64,
        logical_height: f64,
        contents_scale: f64,
        pixel_format: MetalPixelFormat,
    ) -> Result<Self, String> {
        if !logical_width.is_finite()
            || !logical_height.is_finite()
            || logical_width <= 0.0
            || logical_height <= 0.0
        {
            return Err("Metal layer logical dimensions must be finite and positive".to_owned());
        }
        let scale = sanitize_contents_scale(contents_scale);
        let raw_w = (logical_width * scale).round();
        let raw_h = (logical_height * scale).round();
        let raw_w_u32 = if raw_w >= u32::MAX as f64 {
            u32::MAX
        } else {
            (raw_w as u32).max(1)
        };
        let raw_h_u32 = if raw_h >= u32::MAX as f64 {
            u32::MAX
        } else {
            (raw_h as u32).max(1)
        };
        let (drawable_width, drawable_height) = clamp_metal_drawable_size(raw_w_u32, raw_h_u32);
        Ok(Self {
            logical_width,
            logical_height,
            contents_scale: scale,
            drawable_width,
            drawable_height,
            pixel_format,
            wants_extended_dynamic_range: pixel_format.supports_extended_dynamic_range(),
            framebuffer_only: true,
        })
    }

    /// Compute Metal layer geometry directly from physical pixel dimensions and `contentsScale`.
    pub fn from_physical_pixels(
        pixel_width: u32,
        pixel_height: u32,
        contents_scale: f64,
        pixel_format: MetalPixelFormat,
    ) -> Result<Self, String> {
        if pixel_width == 0 || pixel_height == 0 {
            return Err("Metal physical pixel dimensions must be positive".to_owned());
        }
        let scale = sanitize_contents_scale(contents_scale);
        let (drawable_width, drawable_height) =
            clamp_metal_drawable_size(pixel_width, pixel_height);
        Ok(Self {
            logical_width: drawable_width as f64 / scale,
            logical_height: drawable_height as f64 / scale,
            contents_scale: scale,
            drawable_width,
            drawable_height,
            pixel_format,
            wants_extended_dynamic_range: pixel_format.supports_extended_dynamic_range(),
            framebuffer_only: true,
        })
    }

    /// Physical drawable size `(width, height)`.
    pub const fn drawable_size(&self) -> (u32, u32) {
        (self.drawable_width, self.drawable_height)
    }

    /// Whether transitioning from `self` to `next` requires updating `CAMetalLayer`
    /// or recreating IOSurface backing frames.
    pub fn needs_reconfigure(&self, next: &Self) -> bool {
        self.drawable_width != next.drawable_width
            || self.drawable_height != next.drawable_height
            || (self.contents_scale - next.contents_scale).abs() > 1e-4
            || self.pixel_format != next.pixel_format
            || self.wants_extended_dynamic_range != next.wants_extended_dynamic_range
            || self.framebuffer_only != next.framebuffer_only
    }
}

/// Validated `IOSurfaceCreate` property descriptor (16-byte row alignment).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IoSurfaceDescriptor {
    pub width: u32,
    pub height: u32,
    pub bytes_per_element: u32,
    pub bytes_per_row: u32,
    pub alloc_size: usize,
    pub cv_pixel_format: u32,
    pub mtl_pixel_format: usize,
    pub pixel_format: MetalPixelFormat,
}

impl IoSurfaceDescriptor {
    pub fn for_size(
        width: u32,
        height: u32,
        pixel_format: MetalPixelFormat,
    ) -> Result<Self, String> {
        if width == 0 || height == 0 {
            return Err("IOSurface dimensions must be positive".to_owned());
        }
        if width > MTL_MAX_TEXTURE_DIMENSION_2D || height > MTL_MAX_TEXTURE_DIMENSION_2D {
            return Err(format!(
                "IOSurface dimensions {width}x{height} exceed Metal limit {MTL_MAX_TEXTURE_DIMENSION_2D}"
            ));
        }
        let bpe = pixel_format.bytes_per_element();
        let unaligned_row = width
            .checked_mul(bpe)
            .ok_or_else(|| "IOSurface row stride overflow".to_owned())?;
        // Align bytes-per-row to 16 bytes for optimal Metal / IOSurface DMA transfers.
        let bytes_per_row = unaligned_row
            .checked_add(15)
            .map(|v| v & !15)
            .ok_or_else(|| "IOSurface aligned row stride overflow".to_owned())?;
        let alloc_size = (bytes_per_row as usize)
            .checked_mul(height as usize)
            .ok_or_else(|| "IOSurface allocation size overflow".to_owned())?;

        Ok(Self {
            width,
            height,
            bytes_per_element: bpe,
            bytes_per_row,
            alloc_size,
            cv_pixel_format: pixel_format.cv_pixel_format(),
            mtl_pixel_format: pixel_format.mtl_pixel_format(),
            pixel_format,
        })
    }
}

/// How macOS presents `libmpv` video into a Metal-backed UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MetalPresentationMode {
    /// Upstream Subomi's `MpvMetalRenderer` architecture: `vo=libmpv` renders
    /// on a dedicated CGL 4.1 Core Profile worker into 3x `IOSurface` targets,
    /// which are wrapped zero-copy as `MTLTexture`s for the UI scene graph.
    IoSurfaceOpenGlBridge,
    /// Direct `CAMetalLayer` / MoltenVK presentation via `vo=gpu-next` +
    /// `gpu-api=vulkan` + `gpu-context=macvk` when embedded into a native layer.
    NativeMetalLayer,
}

/// Planned `libmpv` option pairs and capability requirements for macOS Metal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetalMpvOptionPlan {
    pub mode: MetalPresentationMode,
    pub options: Vec<(String, String)>,
    pub requires_cgl_worker: bool,
    pub requires_iosurface_bridge: bool,
    pub requires_cametal_layer: bool,
}

/// Plan `libmpv` initialization options for macOS Metal rendering while honoring
/// explicit user `hwdec` overrides (such as `hwdec=no` or `hwdec=videotoolbox-copy`).
pub fn plan_metal_mpv_options(
    mode: MetalPresentationMode,
    configured_hwdec: Option<&str>,
    prefer_hdr: bool,
) -> MetalMpvOptionPlan {
    let hwdec = resolve_macos_metal_hwdec(mode, configured_hwdec);
    match mode {
        MetalPresentationMode::IoSurfaceOpenGlBridge => {
            let mut options = vec![
                ("vo".to_owned(), "libmpv".to_owned()),
                ("hwdec".to_owned(), hwdec),
                ("hwdec-codecs".to_owned(), "all".to_owned()),
                ("video-timing-offset".to_owned(), "0".to_owned()),
            ];
            if prefer_hdr {
                options.push(("target-colorspace-hint".to_owned(), "yes".to_owned()));
            }
            MetalMpvOptionPlan {
                mode,
                options,
                requires_cgl_worker: true,
                requires_iosurface_bridge: true,
                requires_cametal_layer: false,
            }
        }
        MetalPresentationMode::NativeMetalLayer => {
            let mut options = vec![
                ("vo".to_owned(), "gpu-next".to_owned()),
                ("gpu-api".to_owned(), "vulkan".to_owned()),
                ("gpu-context".to_owned(), "macvk".to_owned()),
                ("hwdec".to_owned(), hwdec),
                ("hwdec-codecs".to_owned(), "all".to_owned()),
            ];
            if prefer_hdr {
                options.push(("target-colorspace-hint".to_owned(), "yes".to_owned()));
            }
            MetalMpvOptionPlan {
                mode,
                options,
                requires_cgl_worker: false,
                requires_iosurface_bridge: false,
                requires_cametal_layer: true,
            }
        }
    }
}

/// Resolve the optimal `hwdec` string for macOS Metal while preserving explicit user choices.
pub fn resolve_macos_metal_hwdec(
    mode: MetalPresentationMode,
    configured_hwdec: Option<&str>,
) -> String {
    let trimmed = configured_hwdec.map(str::trim).unwrap_or("");
    if trimmed.is_empty() || trimmed == "auto-safe" || trimmed == "auto" || trimmed == "yes" {
        match mode {
            MetalPresentationMode::IoSurfaceOpenGlBridge => "videotoolbox,auto-safe".to_owned(),
            MetalPresentationMode::NativeMetalLayer => "videotoolbox".to_owned(),
        }
    } else {
        // Rewrite Linux/Windows-only direct decoders if a shared mpv.conf is copied to macOS.
        let adapted: Vec<&str> = trimmed
            .split(',')
            .map(|entry| match entry.trim() {
                "nvdec" | "cuda" | "vaapi" | "vdpau" | "d3d11va" | "dxva2" => "videotoolbox",
                other => other,
            })
            .collect();
        adapted.join(",")
    }
}

/// One IOSurface-backed render target slot owned by the GL producer worker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetalFrameSlot<S> {
    pub surface: S,
    pub gl_texture: u32,
    pub gl_framebuffer: u32,
    pub serial: u64,
}

#[derive(Debug)]
struct RetiredGeneration<S> {
    retired_at_serial: u64,
    slots: Vec<MetalFrameSlot<S>>,
}

/// Platform-independent state machine for upstream Subomi's triple-buffered
/// IOSurface producer ring (`MpvMetalRenderer::Impl`).
///
/// Guarantees:
/// - Allocates `METAL_FRAME_SLOT_COUNT` (3) targets per active generation.
/// - Retains retired resize generations across a bounded grace window
///   ([`METAL_RETIRED_GENERATION_GRACE_FRAMES`] frames, capped at
///   [`METAL_MAX_RETIRED_GENERATIONS`]) so Metal never samples an `IOSurface`
///   freed while GPU commands are in flight, without leaking unbounded memory
///   across resize storms.
/// - Assigns monotonic frame serials on publication.
#[derive(Debug)]
pub struct MetalSurfaceRing<S> {
    requested_size: Option<(u32, u32)>,
    active_size: Option<(u32, u32)>,
    slots: Vec<MetalFrameSlot<S>>,
    retired_generations: VecDeque<RetiredGeneration<S>>,
    latest_frame: Option<usize>,
    write_frame: Option<usize>,
    next_serial: u64,
    render_requested: bool,
    stopping: bool,
}

impl<S> Default for MetalSurfaceRing<S> {
    fn default() -> Self {
        Self::new()
    }
}

impl<S> MetalSurfaceRing<S> {
    pub fn new() -> Self {
        Self {
            requested_size: None,
            active_size: None,
            slots: Vec::new(),
            retired_generations: VecDeque::new(),
            latest_frame: None,
            write_frame: None,
            next_serial: 1,
            render_requested: true,
            stopping: false,
        }
    }

    /// Request a new physical backing size. Returns `true` if the size changed
    /// (and marks a render requested, matching `MpvMetalRenderer::Impl::setPixelSize`).
    pub fn set_pixel_size(&mut self, width: u32, height: u32) -> bool {
        let next = if width == 0 || height == 0 {
            None
        } else {
            Some(clamp_metal_drawable_size(width, height))
        };
        if self.requested_size == next {
            return false;
        }
        self.requested_size = next;
        self.render_requested = true;
        true
    }

    pub fn request_render(&mut self) {
        self.render_requested = true;
    }

    pub fn take_render_request(&mut self) -> bool {
        let pending = self.render_requested;
        self.render_requested = false;
        pending
    }

    pub fn request_stop(&mut self) {
        self.stopping = true;
    }

    pub const fn is_stopping(&self) -> bool {
        self.stopping
    }

    pub const fn active_size(&self) -> Option<(u32, u32)> {
        self.active_size
    }

    pub const fn requested_size(&self) -> Option<(u32, u32)> {
        self.requested_size
    }

    /// Returns `Some((w, h))` if the worker should allocate a new generation.
    pub fn pending_resize(&self) -> Option<(u32, u32)> {
        match self.requested_size {
            Some(size) if Some(size) != self.active_size => Some(size),
            _ => None,
        }
    }

    pub fn active_slot_count(&self) -> usize {
        self.slots.len()
    }

    pub fn retired_generation_count(&self) -> usize {
        self.retired_generations.len()
    }

    /// Recreate the active triple-buffered generation for `(width, height)`.
    ///
    /// If slot creation fails midway, previously created slots from the failed
    /// attempt are passed to `cleanup_partial` and the existing active
    /// generation remains untouched.
    pub fn recreate_generation<E>(
        &mut self,
        width: u32,
        height: u32,
        mut create_slot: impl FnMut(u32, u32, usize) -> Result<MetalFrameSlot<S>, E>,
        mut cleanup_partial: impl FnMut(MetalFrameSlot<S>),
    ) -> Result<(), E> {
        let (w, h) = clamp_metal_drawable_size(width, height);
        let mut new_slots = Vec::with_capacity(METAL_FRAME_SLOT_COUNT);
        for index in 0..METAL_FRAME_SLOT_COUNT {
            match create_slot(w, h, index) {
                Ok(slot) => new_slots.push(slot),
                Err(error) => {
                    for partial in new_slots {
                        cleanup_partial(partial);
                    }
                    return Err(error);
                }
            }
        }
        if !self.slots.is_empty() {
            let old = std::mem::replace(&mut self.slots, new_slots);
            self.retired_generations.push_back(RetiredGeneration {
                retired_at_serial: self.next_serial,
                slots: old,
            });
            while self.retired_generations.len() > METAL_MAX_RETIRED_GENERATIONS {
                if let Some(evicted) = self.retired_generations.pop_front() {
                    for slot in evicted.slots {
                        cleanup_partial(slot);
                    }
                }
            }
        } else {
            self.slots = new_slots;
        }
        self.active_size = Some((w, h));
        self.latest_frame = None;
        self.write_frame = None;
        Ok(())
    }

    /// Select the next write slot in the producer ring (`(write_frame + 1) % len`).
    pub fn begin_write_slot(&mut self) -> Option<(usize, (u32, u32))> {
        if self.slots.is_empty() {
            return None;
        }
        let active_size = self.active_size?;
        let next_idx = match self.write_frame {
            Some(prev) => (prev + 1) % self.slots.len(),
            None => 0,
        };
        self.write_frame = Some(next_idx);
        Some((next_idx, active_size))
    }

    pub fn slot(&self, index: usize) -> Option<&MetalFrameSlot<S>> {
        self.slots.get(index)
    }

    pub fn slot_mut(&mut self, index: usize) -> Option<&mut MetalFrameSlot<S>> {
        self.slots.get_mut(index)
    }

    /// Publish a completed frame slot with the next monotonic serial number.
    pub fn publish_slot(&mut self, index: usize) -> Option<u64> {
        let slot = self.slots.get_mut(index)?;
        let serial = self.next_serial;
        self.next_serial = self.next_serial.saturating_add(1);
        slot.serial = serial;
        self.latest_frame = Some(index);
        Some(serial)
    }

    /// Borrow the most recently published frame slot and its backing pixel size.
    pub fn latest_published(&self) -> Option<(&MetalFrameSlot<S>, (u32, u32))> {
        let index = self.latest_frame?;
        let size = self.active_size?;
        let slot = self.slots.get(index)?;
        Some((slot, size))
    }

    /// Reclaim retired resize generations that are no longer in flight on the GPU.
    ///
    /// Pops generations from the front while the retired queue exceeds
    /// [`METAL_MAX_RETIRED_GENERATIONS`] or at least
    /// [`METAL_RETIRED_GENERATION_GRACE_FRAMES`] frames have been published since
    /// retirement, returning their slots for GL/IOSurface cleanup and
    /// [`MetalTextureCache`] eviction.
    pub fn reclaim_retired(&mut self) -> Vec<MetalFrameSlot<S>> {
        let mut reclaimed = Vec::new();
        while let Some(front) = self.retired_generations.front() {
            let over_cap = self.retired_generations.len() > METAL_MAX_RETIRED_GENERATIONS;
            let past_grace = self.next_serial.saturating_sub(front.retired_at_serial)
                >= METAL_RETIRED_GENERATION_GRACE_FRAMES;
            if over_cap || past_grace {
                if let Some(mut generation) = self.retired_generations.pop_front() {
                    reclaimed.append(&mut generation.slots);
                }
            } else {
                break;
            }
        }
        reclaimed
    }

    /// Drain all active and retired frame slots during worker shutdown.
    ///
    /// Call only AFTER `mpv_render_context_free()` has completed on the worker
    /// thread and Metal presentation has ceased.
    pub fn drain_for_shutdown(&mut self) -> Vec<MetalFrameSlot<S>> {
        let mut all = Vec::new();
        all.append(&mut self.slots);
        for mut generation in self.retired_generations.drain(..) {
            all.append(&mut generation.slots);
        }
        self.active_size = None;
        self.latest_frame = None;
        self.write_frame = None;
        all
    }
}

/// Render-thread cache mapping published IOSurface handles to Metal texture views
/// (parity with `MpvMetalRenderer::Impl::m_metalTextures`).
#[derive(Debug)]
pub struct MetalTextureCache<S, T> {
    entries: Vec<(S, T)>,
}

impl<S, T> Default for MetalTextureCache<S, T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<S, T> MetalTextureCache<S, T> {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Look up the cached Metal texture for `surface`, or create and cache one.
    pub fn get_or_create<E>(
        &mut self,
        surface: S,
        create: impl FnOnce(S) -> Result<T, E>,
    ) -> Result<&T, E>
    where
        S: Copy + PartialEq,
    {
        if let Some(pos) = self.entries.iter().position(|(s, _)| *s == surface) {
            return Ok(&self.entries[pos].1);
        }
        let texture = create(surface)?;
        self.entries.push((surface, texture));
        Ok(&self.entries.last().expect("just pushed").1)
    }

    /// Evict every cached entry whose surface matches `should_evict`, invoking
    /// `destroy` for each evicted `(surface, texture)` pair and returning the
    /// number of evicted entries.
    pub fn evict_where(
        &mut self,
        mut should_evict: impl FnMut(&S) -> bool,
        mut destroy: impl FnMut(S, T),
    ) -> usize {
        let mut evicted = 0usize;
        let mut i = 0usize;
        while i < self.entries.len() {
            if should_evict(&self.entries[i].0) {
                let (surface, texture) = self.entries.swap_remove(i);
                destroy(surface, texture);
                evicted += 1;
            } else {
                i += 1;
            }
        }
        evicted
    }

    /// Evict all cached Metal textures whose backing surface is listed in `surfaces`.
    pub fn evict_surfaces(&mut self, surfaces: &[S], destroy: impl FnMut(S, T)) -> usize
    where
        S: PartialEq,
    {
        if surfaces.is_empty() {
            return 0;
        }
        self.evict_where(|s| surfaces.contains(s), destroy)
    }

    /// Evict all cached Metal textures backed by any reclaimed slot in `retired`.
    pub fn evict_retired_slots(
        &mut self,
        retired: &[MetalFrameSlot<S>],
        destroy: impl FnMut(S, T),
    ) -> usize
    where
        S: PartialEq,
    {
        if retired.is_empty() {
            return 0;
        }
        self.evict_where(|s| retired.iter().any(|slot| &slot.surface == s), destroy)
    }

    /// Destroy all cached Metal texture wrappers during shutdown.
    pub fn clear_with(&mut self, mut destroy: impl FnMut(S, T)) {
        for (surface, texture) in self.entries.drain(..) {
            destroy(surface, texture);
        }
    }
}

/// Detect whether a system Metal GPU device is available at runtime.
#[cfg(not(target_os = "macos"))]
pub fn is_metal_device_available() -> bool {
    false
}

/// Native macOS CoreFoundation / IOSurface / CGL / Metal / QuartzCore C-ABI bridge.
#[cfg(target_os = "macos")]
pub mod native {
    use super::{
        IoSurfaceDescriptor, MetalLayerGeometry, MetalPixelFormat, GL_TEXTURE_RECTANGLE_MACOS,
        MTL_TEXTURE_USAGE_SHADER_READ,
    };
    use std::ffi::{c_char, c_int, c_void};
    use std::ptr;

    pub type CFAllocatorRef = *const c_void;
    pub type CFDictionaryRef = *const c_void;
    pub type CFNumberRef = *const c_void;
    pub type CFStringRef = *const c_void;
    pub type IOSurfaceRef = *mut c_void;
    pub type CGLContextObj = *mut c_void;
    pub type CGLPixelFormatObj = *mut c_void;
    pub type CGLError = i32;
    pub type Id = *mut c_void;
    pub type Class = *const c_void;
    pub type Sel = *const c_void;

    const K_CF_NUMBER_SINT32_TYPE: isize = 3;
    const K_CGL_PFA_ACCELERATED: u32 = 73;
    const K_CGL_PFA_ALLOW_OFFLINE_RENDERERS: u32 = 96;
    const K_CGL_PFA_OPENGL_PROFILE: u32 = 99;
    const K_CGL_OGLP_VERSION_4_1_CORE: u32 = 0x4100;
    const K_CGL_OGLP_VERSION_3_2_CORE: u32 = 0x3200;

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct CGSize {
        width: f64,
        height: f64,
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        static kCFTypeDictionaryKeyCallBacks: c_void;
        static kCFTypeDictionaryValueCallBacks: c_void;
        fn CFNumberCreate(
            allocator: CFAllocatorRef,
            the_type: isize,
            value_ptr: *const c_void,
        ) -> CFNumberRef;
        fn CFDictionaryCreate(
            allocator: CFAllocatorRef,
            keys: *const *const c_void,
            values: *const *const c_void,
            num_values: isize,
            key_callbacks: *const c_void,
            value_callbacks: *const c_void,
        ) -> CFDictionaryRef;
        fn CFRelease(cf: *const c_void);
    }

    #[link(name = "IOSurface", kind = "framework")]
    extern "C" {
        static kIOSurfaceWidth: CFStringRef;
        static kIOSurfaceHeight: CFStringRef;
        static kIOSurfaceBytesPerElement: CFStringRef;
        static kIOSurfaceBytesPerRow: CFStringRef;
        static kIOSurfacePixelFormat: CFStringRef;
        fn IOSurfaceCreate(properties: CFDictionaryRef) -> IOSurfaceRef;
    }

    #[link(name = "OpenGL", kind = "framework")]
    extern "C" {
        fn CGLChoosePixelFormat(
            attribs: *const u32,
            pix: *mut CGLPixelFormatObj,
            npix: *mut c_int,
        ) -> CGLError;
        fn CGLDestroyPixelFormat(pix: CGLPixelFormatObj) -> CGLError;
        fn CGLCreateContext(
            pix: CGLPixelFormatObj,
            share: CGLContextObj,
            ctx: *mut CGLContextObj,
        ) -> CGLError;
        fn CGLDestroyContext(ctx: CGLContextObj) -> CGLError;
        fn CGLSetCurrentContext(ctx: CGLContextObj) -> CGLError;
        fn CGLGetCurrentContext() -> CGLContextObj;
        fn CGLTexImageIOSurface2D(
            ctx: CGLContextObj,
            target: u32,
            internal_format: u32,
            width: c_int,
            height: c_int,
            format: u32,
            pixel_type: u32,
            io_surface: IOSurfaceRef,
            plane: u32,
        ) -> CGLError;
    }

    #[link(name = "Metal", kind = "framework")]
    extern "C" {
        fn MTLCreateSystemDefaultDevice() -> Id;
    }

    #[link(name = "QuartzCore", kind = "framework")]
    extern "C" {}

    #[link(name = "objc")]
    extern "C" {
        fn objc_getClass(name: *const c_char) -> Class;
        fn sel_registerName(name: *const c_char) -> Sel;
        fn objc_msgSend();
        fn objc_release(obj: Id);
        fn objc_autoreleasePoolPush() -> *mut c_void;
        fn objc_autoreleasePoolPop(pool: *mut c_void);
    }

    struct AutoreleasePoolGuard(*mut c_void);

    impl AutoreleasePoolGuard {
        unsafe fn push() -> Self {
            Self(objc_autoreleasePoolPush())
        }
    }

    impl Drop for AutoreleasePoolGuard {
        fn drop(&mut self) {
            unsafe {
                objc_autoreleasePoolPop(self.0);
            }
        }
    }

    /// Owned `IOSurfaceRef` released via `CFRelease` on drop.
    #[derive(Debug)]
    pub struct IoSurfaceHandle {
        raw: IOSurfaceRef,
    }

    unsafe impl Send for IoSurfaceHandle {}

    impl IoSurfaceHandle {
        pub fn as_raw(&self) -> IOSurfaceRef {
            self.raw
        }
    }

    impl Drop for IoSurfaceHandle {
        fn drop(&mut self) {
            if !self.raw.is_null() {
                unsafe { CFRelease(self.raw.cast()) };
                self.raw = ptr::null_mut();
            }
        }
    }

    /// Owned OpenGL 4.1 Core Profile (with 3.2 Core fallback) CGL offscreen context
    /// for the macOS IOSurface producer worker thread.
    #[derive(Debug)]
    pub struct MacOsCglOffscreenContext {
        ctx: CGLContextObj,
    }

    unsafe impl Send for MacOsCglOffscreenContext {}

    impl MacOsCglOffscreenContext {
        pub fn new() -> Result<Self, String> {
            for profile in [K_CGL_OGLP_VERSION_4_1_CORE, K_CGL_OGLP_VERSION_3_2_CORE] {
                let attribs: [u32; 5] = [
                    K_CGL_PFA_OPENGL_PROFILE,
                    profile,
                    K_CGL_PFA_ACCELERATED,
                    K_CGL_PFA_ALLOW_OFFLINE_RENDERERS,
                    0,
                ];
                let mut pix: CGLPixelFormatObj = ptr::null_mut();
                let mut npix: c_int = 0;
                let err = unsafe { CGLChoosePixelFormat(attribs.as_ptr(), &mut pix, &mut npix) };
                if err != 0 || pix.is_null() {
                    continue;
                }
                let mut ctx: CGLContextObj = ptr::null_mut();
                let create_err = unsafe { CGLCreateContext(pix, ptr::null_mut(), &mut ctx) };
                unsafe {
                    let _ = CGLDestroyPixelFormat(pix);
                }
                if create_err == 0 && !ctx.is_null() {
                    return Ok(Self { ctx });
                }
            }
            Err("CGLCreateContext failed for OpenGL Core Profile on macOS".to_owned())
        }

        pub fn make_current(&self) -> Result<(), String> {
            let err = unsafe { CGLSetCurrentContext(self.ctx) };
            if err == 0 {
                Ok(())
            } else {
                Err(format!("CGLSetCurrentContext failed with code {err}"))
            }
        }

        pub fn done_current(&self) {
            unsafe {
                let _ = CGLSetCurrentContext(ptr::null_mut());
            }
        }

        pub fn as_raw(&self) -> CGLContextObj {
            self.ctx
        }
    }

    impl Drop for MacOsCglOffscreenContext {
        fn drop(&mut self) {
            if !self.ctx.is_null() {
                unsafe {
                    if CGLGetCurrentContext() == self.ctx {
                        let _ = CGLSetCurrentContext(ptr::null_mut());
                    }
                    let _ = CGLDestroyContext(self.ctx);
                }
                self.ctx = ptr::null_mut();
            }
        }
    }

    /// Create an `IOSurface` from a validated [`IoSurfaceDescriptor`].
    pub fn create_iosurface(desc: &IoSurfaceDescriptor) -> Result<IoSurfaceHandle, String> {
        unsafe {
            let width_val = desc.width as i32;
            let height_val = desc.height as i32;
            let bpe_val = desc.bytes_per_element as i32;
            let bpr_val = desc.bytes_per_row as i32;
            let fmt_val = desc.cv_pixel_format as i32;

            let n_width = CFNumberCreate(
                ptr::null(),
                K_CF_NUMBER_SINT32_TYPE,
                (&width_val as *const i32).cast(),
            );
            let n_height = CFNumberCreate(
                ptr::null(),
                K_CF_NUMBER_SINT32_TYPE,
                (&height_val as *const i32).cast(),
            );
            let n_bpe = CFNumberCreate(
                ptr::null(),
                K_CF_NUMBER_SINT32_TYPE,
                (&bpe_val as *const i32).cast(),
            );
            let n_bpr = CFNumberCreate(
                ptr::null(),
                K_CF_NUMBER_SINT32_TYPE,
                (&bpr_val as *const i32).cast(),
            );
            let n_fmt = CFNumberCreate(
                ptr::null(),
                K_CF_NUMBER_SINT32_TYPE,
                (&fmt_val as *const i32).cast(),
            );

            let numbers = [n_width, n_height, n_bpe, n_bpr, n_fmt];
            if numbers.iter().any(|n| n.is_null()) {
                for num in numbers {
                    if !num.is_null() {
                        CFRelease(num.cast());
                    }
                }
                return Err("CFNumberCreate failed for IOSurface properties".to_owned());
            }

            let keys = [
                kIOSurfaceWidth.cast(),
                kIOSurfaceHeight.cast(),
                kIOSurfaceBytesPerElement.cast(),
                kIOSurfaceBytesPerRow.cast(),
                kIOSurfacePixelFormat.cast(),
            ];
            let values = [
                n_width.cast(),
                n_height.cast(),
                n_bpe.cast(),
                n_bpr.cast(),
                n_fmt.cast(),
            ];
            let dict = CFDictionaryCreate(
                ptr::null(),
                keys.as_ptr(),
                values.as_ptr(),
                keys.len() as isize,
                &kCFTypeDictionaryKeyCallBacks,
                &kCFTypeDictionaryValueCallBacks,
            );

            for num in numbers {
                CFRelease(num.cast());
            }
            if dict.is_null() {
                return Err("CFDictionaryCreate failed for IOSurface properties".to_owned());
            }
            let surface = IOSurfaceCreate(dict);
            CFRelease(dict.cast());
            if surface.is_null() {
                Err(format!(
                    "IOSurfaceCreate failed for {}x{}",
                    desc.width, desc.height
                ))
            } else {
                Ok(IoSurfaceHandle { raw: surface })
            }
        }
    }

    /// Bind an `IOSurface` to the currently bound `GL_TEXTURE_RECTANGLE` on the
    /// current CGL context (parity with `MpvMetalRenderer::Impl::createFrame`).
    pub fn bind_iosurface_to_gl_texture_rectangle(
        surface: &IoSurfaceHandle,
        width: u32,
        height: u32,
        pixel_format: MetalPixelFormat,
    ) -> Result<(), String> {
        unsafe {
            let ctx = CGLGetCurrentContext();
            if ctx.is_null() {
                return Err("No current CGL context for CGLTexImageIOSurface2D".to_owned());
            }
            let err = CGLTexImageIOSurface2D(
                ctx,
                GL_TEXTURE_RECTANGLE_MACOS,
                pixel_format.gl_internal_format() as u32,
                width as c_int,
                height as c_int,
                pixel_format.gl_format(),
                pixel_format.gl_type(),
                surface.as_raw(),
                0,
            );
            if err == 0 {
                Ok(())
            } else {
                Err(format!("CGLTexImageIOSurface2D failed with CGLError {err}"))
            }
        }
    }

    /// Create a zero-copy `id<MTLTexture>` view backed by `surface` on `mtl_device`.
    ///
    /// # Safety
    /// `mtl_device` must be a valid Objective-C `id<MTLDevice>` pointer.
    /// Caller owns the returned retained `id<MTLTexture>` and must release it.
    pub unsafe fn create_metal_texture_from_iosurface(
        mtl_device: Id,
        surface: &IoSurfaceHandle,
        width: u32,
        height: u32,
        pixel_format: MetalPixelFormat,
    ) -> Result<Id, String> {
        if mtl_device.is_null() {
            return Err("MTLDevice pointer is null".to_owned());
        }
        // Drain the autoreleased MTLTextureDescriptor even on non-AppKit threads.
        // `newTextureWithDescriptor:iosurface:plane:` returns a +1 retained object
        // owned by the caller, so popping the pool preserves `texture`.
        let _pool = AutoreleasePoolGuard::push();
        let desc_cls = objc_getClass(c"MTLTextureDescriptor".as_ptr());
        if desc_cls.is_null() {
            return Err("MTLTextureDescriptor Objective-C class unavailable".to_owned());
        }
        let sel_desc = sel_registerName(
            c"texture2DDescriptorWithPixelFormat:width:height:mipmapped:".as_ptr(),
        );
        let msg_desc: extern "C" fn(Class, Sel, usize, usize, usize, i8) -> Id =
            std::mem::transmute(objc_msgSend as *const ());
        let descriptor = msg_desc(
            desc_cls,
            sel_desc,
            pixel_format.mtl_pixel_format(),
            width as usize,
            height as usize,
            0,
        );
        if descriptor.is_null() {
            return Err("Failed to allocate MTLTextureDescriptor".to_owned());
        }
        let sel_set_usage = sel_registerName(c"setUsage:".as_ptr());
        let msg_set_usage: extern "C" fn(Id, Sel, usize) =
            std::mem::transmute(objc_msgSend as *const ());
        msg_set_usage(descriptor, sel_set_usage, MTL_TEXTURE_USAGE_SHADER_READ);

        let sel_new_tex = sel_registerName(c"newTextureWithDescriptor:iosurface:plane:".as_ptr());
        let msg_new_tex: extern "C" fn(Id, Sel, Id, IOSurfaceRef, usize) -> Id =
            std::mem::transmute(objc_msgSend as *const ());
        let texture = msg_new_tex(mtl_device, sel_new_tex, descriptor, surface.as_raw(), 0);
        if texture.is_null() {
            Err("MTLDevice newTextureWithDescriptor:iosurface:plane: returned nil".to_owned())
        } else {
            Ok(texture)
        }
    }

    /// Apply HiDPI `contentsScale`, `drawableSize`, `pixelFormat`, `framebufferOnly`,
    /// and EDR settings onto a `CAMetalLayer`.
    ///
    /// # Safety
    /// `ca_metal_layer` must be a valid Objective-C `CAMetalLayer *` pointer.
    pub unsafe fn configure_ca_metal_layer(
        ca_metal_layer: Id,
        geometry: &MetalLayerGeometry,
    ) -> Result<(), String> {
        if ca_metal_layer.is_null() {
            return Err("CAMetalLayer pointer is null".to_owned());
        }
        let sel_scale = sel_registerName(c"setContentsScale:".as_ptr());
        let msg_scale: extern "C" fn(Id, Sel, f64) = std::mem::transmute(objc_msgSend as *const ());
        msg_scale(ca_metal_layer, sel_scale, geometry.contents_scale);

        let sel_size = sel_registerName(c"setDrawableSize:".as_ptr());
        let msg_size: extern "C" fn(Id, Sel, CGSize) =
            std::mem::transmute(objc_msgSend as *const ());
        msg_size(
            ca_metal_layer,
            sel_size,
            CGSize {
                width: geometry.drawable_width as f64,
                height: geometry.drawable_height as f64,
            },
        );

        let sel_fmt = sel_registerName(c"setPixelFormat:".as_ptr());
        let msg_fmt: extern "C" fn(Id, Sel, usize) = std::mem::transmute(objc_msgSend as *const ());
        msg_fmt(
            ca_metal_layer,
            sel_fmt,
            geometry.pixel_format.mtl_pixel_format(),
        );

        let sel_fb_only = sel_registerName(c"setFramebufferOnly:".as_ptr());
        let msg_fb_only: extern "C" fn(Id, Sel, i8) =
            std::mem::transmute(objc_msgSend as *const ());
        msg_fb_only(
            ca_metal_layer,
            sel_fb_only,
            i8::from(geometry.framebuffer_only),
        );

        let sel_edr = sel_registerName(c"setWantsExtendedDynamicRangeContent:".as_ptr());
        let msg_edr: extern "C" fn(Id, Sel, i8) = std::mem::transmute(objc_msgSend as *const ());
        msg_edr(
            ca_metal_layer,
            sel_edr,
            i8::from(geometry.wants_extended_dynamic_range),
        );
        Ok(())
    }

    /// Release a retained Objective-C / Metal object pointer via `objc_release`.
    ///
    /// # Safety
    /// `obj` must be null or a valid retained Objective-C object.
    pub unsafe fn release_metal_object(obj: Id) {
        if !obj.is_null() {
            objc_release(obj);
        }
    }

    pub fn probe_system_metal_device() -> bool {
        unsafe {
            let device = MTLCreateSystemDefaultDevice();
            if device.is_null() {
                false
            } else {
                objc_release(device);
                true
            }
        }
    }
}

/// Detect whether a system Metal GPU device is available at runtime.
#[cfg(target_os = "macos")]
pub fn is_metal_device_available() -> bool {
    native::probe_system_metal_device()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_slot(next_surface_id: &mut u64, idx: usize) -> MetalFrameSlot<u64> {
        *next_surface_id += 1;
        MetalFrameSlot {
            surface: *next_surface_id,
            gl_texture: (*next_surface_id as u32) * 10 + idx as u32,
            gl_framebuffer: (*next_surface_id as u32) * 100 + idx as u32,
            serial: 0,
        }
    }

    #[test]
    fn resize_storm_bounds_retired_generations_and_evicts_oldest() {
        let mut ring: MetalSurfaceRing<u64> = MetalSurfaceRing::new();
        let mut next_id = 0u64;
        let mut evicted_surfaces = Vec::new();

        // Initial generation (surfaces 1..=3).
        ring.recreate_generation(
            640,
            360,
            |_w, _h, idx| Ok::<_, ()>(make_slot(&mut next_id, idx)),
            |slot| evicted_surfaces.push(slot.surface),
        )
        .unwrap();
        assert_eq!(ring.retired_generation_count(), 0);
        assert!(evicted_surfaces.is_empty());

        // 5 consecutive resizes with zero frames published between them.
        for step in 1..=5u32 {
            ring.recreate_generation(
                640 + step * 16,
                360 + step * 9,
                |_w, _h, idx| Ok::<_, ()>(make_slot(&mut next_id, idx)),
                |slot| evicted_surfaces.push(slot.surface),
            )
            .unwrap();
            assert!(
                ring.retired_generation_count() <= METAL_MAX_RETIRED_GENERATIONS,
                "retired count exceeded cap at step {step}"
            );
        }

        // 5 retirements with cap=2 -> oldest 3 generations (surfaces 1..=9) evicted FIFO.
        assert_eq!(
            ring.retired_generation_count(),
            METAL_MAX_RETIRED_GENERATIONS
        );
        assert_eq!(evicted_surfaces, vec![1, 2, 3, 4, 5, 6, 7, 8, 9]);

        // Shutdown drains active (3) + remaining 2 retired generations (6) = 9 slots.
        let mut drained: Vec<u64> = ring
            .drain_for_shutdown()
            .into_iter()
            .map(|s| s.surface)
            .collect();
        drained.sort_unstable();
        assert_eq!(drained, vec![10, 11, 12, 13, 14, 15, 16, 17, 18]);
    }

    #[test]
    fn reclaim_retired_waits_for_grace_frames_and_reclaims_staggered_generations() {
        let mut ring: MetalSurfaceRing<u64> = MetalSurfaceRing::new();
        let mut next_id = 0u64;

        // Generation 0 (surfaces 1, 2, 3).
        ring.recreate_generation(
            1280,
            720,
            |_w, _h, idx| Ok::<_, ()>(make_slot(&mut next_id, idx)),
            |_| {},
        )
        .unwrap();
        let (idx, _) = ring.begin_write_slot().unwrap();
        assert_eq!(ring.publish_slot(idx), Some(1));

        // Resize -> generation 0 retired at next_serial=2.
        ring.recreate_generation(
            1600,
            900,
            |_w, _h, idx| Ok::<_, ()>(make_slot(&mut next_id, idx)),
            |_| {},
        )
        .unwrap();
        assert_eq!(ring.retired_generation_count(), 1);
        assert!(ring.reclaim_retired().is_empty());

        // Publish grace - 1 frames on generation 1; generation 0 must still be kept.
        for _ in 0..(METAL_RETIRED_GENERATION_GRACE_FRAMES - 1) {
            let (idx, _) = ring.begin_write_slot().unwrap();
            ring.publish_slot(idx).unwrap();
            assert!(ring.reclaim_retired().is_empty());
        }
        assert_eq!(ring.retired_generation_count(), 1);

        // Resize again -> generation 1 retired at next_serial=4 while generation 0 waits 1 more frame.
        ring.recreate_generation(
            1920,
            1080,
            |_w, _h, idx| Ok::<_, ()>(make_slot(&mut next_id, idx)),
            |_| {},
        )
        .unwrap();
        assert_eq!(ring.retired_generation_count(), 2);

        // Publishing 1 frame satisfies generation 0's grace window, leaving generation 1 intact.
        let (idx, _) = ring.begin_write_slot().unwrap();
        ring.publish_slot(idx).unwrap();
        let reclaimed_gen0: Vec<u64> = ring
            .reclaim_retired()
            .into_iter()
            .map(|s| s.surface)
            .collect();
        assert_eq!(reclaimed_gen0, vec![1, 2, 3]);
        assert_eq!(ring.retired_generation_count(), 1);

        // Publishing 2 more frames satisfies generation 1's grace window.
        for _ in 0..(METAL_RETIRED_GENERATION_GRACE_FRAMES - 1) {
            let (idx, _) = ring.begin_write_slot().unwrap();
            ring.publish_slot(idx).unwrap();
        }
        let reclaimed_gen1: Vec<u64> = ring
            .reclaim_retired()
            .into_iter()
            .map(|s| s.surface)
            .collect();
        assert_eq!(reclaimed_gen1, vec![4, 5, 6]);
        assert_eq!(ring.retired_generation_count(), 0);
        assert!(ring.reclaim_retired().is_empty());
    }

    #[test]
    fn texture_cache_evict_retired_slots_surfaces_and_predicate() {
        let mut ring: MetalSurfaceRing<u64> = MetalSurfaceRing::new();
        let mut cache: MetalTextureCache<u64, String> = MetalTextureCache::new();
        let mut next_id = 9u64;

        // Populate generation 0 (surfaces 10, 11, 12) and cache all 3 textures.
        ring.recreate_generation(
            1280,
            720,
            |_w, _h, idx| Ok::<_, ()>(make_slot(&mut next_id, idx)),
            |_| {},
        )
        .unwrap();
        for _ in 0..METAL_FRAME_SLOT_COUNT {
            let (idx, _) = ring.begin_write_slot().unwrap();
            ring.publish_slot(idx).unwrap();
            let surface = ring.latest_published().unwrap().0.surface;
            cache
                .get_or_create(surface, |s| Ok::<_, ()>(format!("tex_{s}")))
                .unwrap();
        }
        assert_eq!(cache.len(), METAL_FRAME_SLOT_COUNT);

        // Resize to generation 1 (surfaces 13, 14, 15) and publish enough frames to reclaim gen 0.
        ring.recreate_generation(
            1920,
            1080,
            |_w, _h, idx| Ok::<_, ()>(make_slot(&mut next_id, idx)),
            |_| {},
        )
        .unwrap();
        for _ in 0..METAL_RETIRED_GENERATION_GRACE_FRAMES {
            let (idx, _) = ring.begin_write_slot().unwrap();
            ring.publish_slot(idx).unwrap();
            let surface = ring.latest_published().unwrap().0.surface;
            cache
                .get_or_create(surface, |s| Ok::<_, ()>(format!("tex_{s}")))
                .unwrap();
        }
        assert_eq!(cache.len(), METAL_FRAME_SLOT_COUNT * 2);

        let reclaimed = ring.reclaim_retired();
        assert_eq!(reclaimed.len(), METAL_FRAME_SLOT_COUNT);

        let mut destroyed = Vec::new();
        let evicted = cache.evict_retired_slots(&reclaimed, |s, t| destroyed.push((s, t)));
        assert_eq!(evicted, METAL_FRAME_SLOT_COUNT);
        assert_eq!(cache.len(), METAL_FRAME_SLOT_COUNT);
        destroyed.sort_by_key(|(s, _)| *s);
        assert_eq!(
            destroyed,
            vec![
                (10, "tex_10".to_owned()),
                (11, "tex_11".to_owned()),
                (12, "tex_12".to_owned()),
            ]
        );

        // Active generation textures remain cached without re-creation.
        for surface in [13u64, 14, 15] {
            let tex = cache
                .get_or_create(surface, |_| -> Result<String, &'static str> {
                    Err("unexpected cache miss")
                })
                .unwrap();
            assert_eq!(tex, &format!("tex_{surface}"));
        }

        // Empty slice and unmatched surfaces are no-ops.
        assert_eq!(cache.evict_retired_slots(&[], |_, _| {}), 0);
        assert_eq!(cache.evict_surfaces(&[999], |_, _| {}), 0);

        // Direct surface and predicate evictions remove only matching entries.
        let mut evicted_ids = Vec::new();
        assert_eq!(
            cache.evict_surfaces(&[13, 999], |s, _| evicted_ids.push(s)),
            1
        );
        assert_eq!(evicted_ids, vec![13]);
        assert_eq!(cache.len(), 2);

        assert_eq!(
            cache.evict_where(|s| *s == 15, |s, _| evicted_ids.push(s)),
            1
        );
        assert_eq!(evicted_ids, vec![13, 15]);
        assert_eq!(cache.len(), 1);
    }
}

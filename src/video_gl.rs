//! State handoff between the UI renderer and libmpv's OpenGL renderer.

use glow::HasContext;

/// Preferred color attachment format candidates for libmpv video FBOs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VideoFboFormat {
    /// 16-bit half-float RGBA (`GL_RGBA16F`) to eliminate 8-bit color banding (#236).
    Rgba16F,
    /// Standard 8-bit normalized RGBA (`GL_RGBA8`) fallback.
    Rgba8,
}

impl VideoFboFormat {
    pub const fn gl_internal_format(self) -> i32 {
        match self {
            Self::Rgba16F => glow::RGBA16F as i32,
            Self::Rgba8 => glow::RGBA8 as i32,
        }
    }

    pub const fn gl_pixel_type(self) -> u32 {
        match self {
            Self::Rgba16F => glow::HALF_FLOAT,
            Self::Rgba8 => glow::UNSIGNED_BYTE,
        }
    }
}

/// Pure, platform-independent FBO format fallback plan.
///
/// When `prefer_16f` is `true`, tries `GL_RGBA16F` first and falls back to
/// `GL_RGBA8` if float color attachments are unsupported or incomplete.
pub fn plan_video_fbo_formats(prefer_16f: bool) -> Vec<VideoFboFormat> {
    if prefer_16f {
        vec![VideoFboFormat::Rgba16F, VideoFboFormat::Rgba8]
    } else {
        vec![VideoFboFormat::Rgba8]
    }
}

/// Validate video FBO dimensions against positive `i32` bounds and an optional
/// `GL_MAX_TEXTURE_SIZE` limit.
pub fn validate_fbo_dimensions(
    width: u32,
    height: u32,
    max_texture_size: Option<u32>,
) -> Result<(i32, i32), String> {
    let w = i32::try_from(width)
        .ok()
        .filter(|&v| v > 0)
        .ok_or_else(|| "video FBO width must be positive and fit in i32".to_owned())?;
    let h = i32::try_from(height)
        .ok()
        .filter(|&v| v > 0)
        .ok_or_else(|| "video FBO height must be positive and fit in i32".to_owned())?;
    if let Some(limit) = max_texture_size.filter(|&v| v > 0) {
        if width > limit || height > limit {
            return Err(format!(
                "video FBO dimensions {width}x{height} exceed GL_MAX_TEXTURE_SIZE ({limit})"
            ));
        }
    }
    Ok((w, h))
}

/// Clamp requested FBO dimensions to `GL_MAX_TEXTURE_SIZE` while preserving aspect ratio.
#[allow(dead_code)] // Included directly as a private module in tests/video_render_timing_test.rs.
pub fn clamp_fbo_dimensions(width: u32, height: u32, max_texture_size: u32) -> (u32, u32) {
    let w = width.max(1);
    let h = height.max(1);
    let limit = max_texture_size.max(1);
    let max_dim = w.max(h);
    if max_dim <= limit {
        return (w, h);
    }
    let scale = limit as f64 / max_dim as f64;
    let clamped_w = ((w as f64 * scale).round() as u32).clamp(1, limit);
    let clamped_h = ((h as f64 * scale).round() as u32).clamp(1, limit);
    (clamped_w, clamped_h)
}

/// Decide whether an existing FBO of `current` size should be reallocated for
/// `requested` size, using shrink hysteresis (64px) and aspect-ratio drift checks.
#[allow(dead_code)] // Included directly as a private module in tests/video_render_timing_test.rs.
pub fn should_reallocate_fbo(has_target: bool, current: (u32, u32), requested: (u32, u32)) -> bool {
    if !has_target || current.0 == 0 || current.1 == 0 {
        return requested.0 > 0 && requested.1 > 0;
    }
    if requested.0 == 0 || requested.1 == 0 {
        return false;
    }
    if current == requested {
        return false;
    }
    if requested.0 > current.0 || requested.1 > current.1 {
        return true;
    }
    let aspect_diff = (u64::from(requested.0) * u64::from(current.1))
        .abs_diff(u64::from(current.0) * u64::from(requested.1));
    if aspect_diff > u64::from(current.0.max(requested.0)) * 2 {
        return true;
    }
    requested.0 + 64 <= current.0 || requested.1 + 64 <= current.1
}

/// Allocated video FBO/texture target and its GL internal format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoFboTarget {
    pub texture: glow::NativeTexture,
    pub framebuffer: glow::NativeFramebuffer,
    pub internal_format: i32,
}

/// Cached OpenGL context version capabilities to avoid redundant version checks
/// on every video frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GlCapabilities {
    pub has_gl3: bool,
    pub has_srgb: bool,
    pub has_samplers: bool,
}

impl GlCapabilities {
    pub fn from_context(gl: &glow::Context) -> Self {
        let ver = gl.version();
        let has_gl3 = !ver.is_embedded || ver.major >= 3;
        let has_srgb = !ver.is_embedded && ver.major >= 3;
        let has_samplers = if ver.is_embedded {
            ver.major >= 3
        } else {
            ver.major > 3 || (ver.major == 3 && ver.minor >= 3)
        };
        Self {
            has_gl3,
            has_srgb,
            has_samplers,
        }
    }
}

struct FboBindingGuard<'a> {
    gl: &'a glow::Context,
    has_gl3: bool,
    old_draw_fb: i32,
    old_read_fb: i32,
    old_texture: i32,
    old_unpack_buffer: i32,
}

impl<'a> FboBindingGuard<'a> {
    unsafe fn save(gl: &'a glow::Context, has_gl3: bool) -> Self {
        let old_draw_fb = if has_gl3 {
            gl.get_parameter_i32(glow::DRAW_FRAMEBUFFER_BINDING)
        } else {
            gl.get_parameter_i32(glow::FRAMEBUFFER_BINDING)
        };
        let old_read_fb = if has_gl3 {
            gl.get_parameter_i32(glow::READ_FRAMEBUFFER_BINDING)
        } else {
            old_draw_fb
        };
        let old_texture = gl.get_parameter_i32(glow::TEXTURE_BINDING_2D);
        let old_unpack_buffer = if has_gl3 {
            gl.get_parameter_i32(glow::PIXEL_UNPACK_BUFFER_BINDING)
        } else {
            0
        };
        Self {
            gl,
            has_gl3,
            old_draw_fb,
            old_read_fb,
            old_texture,
            old_unpack_buffer,
        }
    }
}

impl Drop for FboBindingGuard<'_> {
    fn drop(&mut self) {
        unsafe {
            if self.has_gl3 {
                self.gl.bind_buffer(
                    glow::PIXEL_UNPACK_BUFFER,
                    std::num::NonZeroU32::new(self.old_unpack_buffer as u32)
                        .map(glow::NativeBuffer),
                );
                self.gl.bind_framebuffer(
                    glow::DRAW_FRAMEBUFFER,
                    std::num::NonZeroU32::new(self.old_draw_fb as u32).map(glow::NativeFramebuffer),
                );
                self.gl.bind_framebuffer(
                    glow::READ_FRAMEBUFFER,
                    std::num::NonZeroU32::new(self.old_read_fb as u32).map(glow::NativeFramebuffer),
                );
            } else {
                self.gl.bind_framebuffer(
                    glow::FRAMEBUFFER,
                    std::num::NonZeroU32::new(self.old_draw_fb as u32).map(glow::NativeFramebuffer),
                );
            }
            self.gl.bind_texture(
                glow::TEXTURE_2D,
                std::num::NonZeroU32::new(self.old_texture as u32).map(glow::NativeTexture),
            );
        }
    }
}

/// Allocate or resize a color-renderable video texture and FBO, preferring
/// `GL_RGBA16F` to eliminate 8-bit color banding (#236) and falling back to
/// `GL_RGBA8` if float color attachments are unsupported.
///
/// The caller must have `gl`'s context current. Preserves existing 2D texture,
/// pixel-unpack buffer, and draw/read framebuffer bindings.
pub fn create_video_fbo_texture(
    gl: &glow::Context,
    existing_texture: Option<glow::NativeTexture>,
    existing_framebuffer: Option<glow::NativeFramebuffer>,
    width: u32,
    height: u32,
) -> Result<VideoFboTarget, String> {
    create_video_fbo_texture_with_preference(
        gl,
        existing_texture,
        existing_framebuffer,
        width,
        height,
        true,
    )
}

/// Like [`create_video_fbo_texture`], with explicit control over whether
/// `GL_RGBA16F` is attempted before `GL_RGBA8`.
pub fn create_video_fbo_texture_with_preference(
    gl: &glow::Context,
    existing_texture: Option<glow::NativeTexture>,
    existing_framebuffer: Option<glow::NativeFramebuffer>,
    width: u32,
    height: u32,
    prefer_16f: bool,
) -> Result<VideoFboTarget, String> {
    create_video_fbo_texture_with_caps(
        gl,
        GlCapabilities::from_context(gl),
        existing_texture,
        existing_framebuffer,
        width,
        height,
        prefer_16f,
        None,
    )
}

/// Like [`create_video_fbo_texture_with_preference`], using pre-queried
/// [`GlCapabilities`] and an optional cached `GL_MAX_TEXTURE_SIZE` limit.
///
/// If `max_texture_size` is `None`, queries `GL_MAX_TEXTURE_SIZE` from `gl`.
/// Restores prior GL bindings via an RAII guard even on error or unwind.
#[allow(clippy::too_many_arguments)]
pub fn create_video_fbo_texture_with_caps(
    gl: &glow::Context,
    caps: GlCapabilities,
    existing_texture: Option<glow::NativeTexture>,
    existing_framebuffer: Option<glow::NativeFramebuffer>,
    width: u32,
    height: u32,
    prefer_16f: bool,
    max_texture_size: Option<u32>,
) -> Result<VideoFboTarget, String> {
    unsafe {
        let max_tex_limit = match max_texture_size {
            Some(limit) => Some(limit),
            None => {
                let max_tex = gl.get_parameter_i32(glow::MAX_TEXTURE_SIZE);
                u32::try_from(max_tex).ok().filter(|&v| v > 0)
            }
        };
        let (w, h) = validate_fbo_dimensions(width, height, max_tex_limit)?;

        let guard = FboBindingGuard::save(gl, caps.has_gl3);

        let texture = match existing_texture {
            Some(tex) => tex,
            None => gl.create_texture()?,
        };
        let framebuffer = match existing_framebuffer {
            Some(fb) => fb,
            None => match gl.create_framebuffer() {
                Ok(fb) => fb,
                Err(error) => {
                    if existing_texture.is_none() {
                        gl.delete_texture(texture);
                    }
                    return Err(error);
                }
            },
        };

        gl.bind_texture(glow::TEXTURE_2D, Some(texture));
        gl.tex_parameter_i32(
            glow::TEXTURE_2D,
            glow::TEXTURE_MIN_FILTER,
            glow::LINEAR as i32,
        );
        gl.tex_parameter_i32(
            glow::TEXTURE_2D,
            glow::TEXTURE_MAG_FILTER,
            glow::LINEAR as i32,
        );
        gl.tex_parameter_i32(
            glow::TEXTURE_2D,
            glow::TEXTURE_WRAP_S,
            glow::CLAMP_TO_EDGE as i32,
        );
        gl.tex_parameter_i32(
            glow::TEXTURE_2D,
            glow::TEXTURE_WRAP_T,
            glow::CLAMP_TO_EDGE as i32,
        );
        gl.bind_framebuffer(glow::FRAMEBUFFER, Some(framebuffer));
        if caps.has_gl3 && guard.old_unpack_buffer != 0 {
            gl.bind_buffer(glow::PIXEL_UNPACK_BUFFER, None);
        }

        let mut last_status = 0u32;
        let mut last_error = glow::NO_ERROR;
        for fmt in plan_video_fbo_formats(prefer_16f) {
            let internal_format = fmt.gl_internal_format();
            let pixel_type = fmt.gl_pixel_type();
            while gl.get_error() != glow::NO_ERROR {}
            gl.tex_image_2d(
                glow::TEXTURE_2D,
                0,
                internal_format,
                w,
                h,
                0,
                glow::RGBA,
                pixel_type,
                glow::PixelUnpackData::Slice(None),
            );
            let tex_err = gl.get_error();
            if tex_err != glow::NO_ERROR {
                last_error = tex_err;
                while gl.get_error() != glow::NO_ERROR {}
                continue;
            }
            gl.framebuffer_texture_2d(
                glow::FRAMEBUFFER,
                glow::COLOR_ATTACHMENT0,
                glow::TEXTURE_2D,
                Some(texture),
                0,
            );
            last_status = gl.check_framebuffer_status(glow::FRAMEBUFFER);
            let fb_err = gl.get_error();
            if last_status == glow::FRAMEBUFFER_COMPLETE && fb_err == glow::NO_ERROR {
                return Ok(VideoFboTarget {
                    texture,
                    framebuffer,
                    internal_format,
                });
            }
            if fb_err != glow::NO_ERROR {
                last_error = fb_err;
            }
            while gl.get_error() != glow::NO_ERROR {}
        }

        if existing_framebuffer.is_none() {
            gl.delete_framebuffer(framebuffer);
        }
        if existing_texture.is_none() {
            gl.delete_texture(texture);
        }
        Err(format!(
            "video framebuffer is incomplete: status=0x{last_status:x} gl_error=0x{last_error:x}"
        ))
    }
}

struct BlendGuard<'a> {
    gl: &'a glow::Context,
    was_enabled: bool,
}

impl<'a> BlendGuard<'a> {
    unsafe fn disable(gl: &'a glow::Context) -> Self {
        let was_enabled = gl.is_enabled(glow::BLEND);
        gl.disable(glow::BLEND);
        Self { gl, was_enabled }
    }
}

impl Drop for BlendGuard<'_> {
    fn drop(&mut self) {
        unsafe {
            if self.was_enabled {
                self.gl.enable(glow::BLEND);
            } else {
                self.gl.disable(glow::BLEND);
            }
        }
    }
}

/// libmpv requires blending disabled on entry. UI renderers can leave it on
/// even after an otherwise empty flush. Restore the caller's setting afterward.
/// The caller must keep this GL context current throughout the operation.
pub fn without_blending<T>(gl: &glow::Context, render: impl FnOnce() -> T) -> T {
    let _guard = unsafe { BlendGuard::disable(gl) };
    render()
}

struct GlStateGuard<'a> {
    gl: &'a glow::Context,
    caps: GlCapabilities,
    old_draw_framebuffer: i32,
    old_read_framebuffer: i32,
    old_active_texture: u32,
    old_texture_active: i32,
    old_sampler_active: i32,
    old_texture0: Option<i32>,
    old_sampler0: Option<i32>,
    old_program: i32,
    old_vertex_array: i32,
    old_array_buffer: i32,
    old_element_array_buffer: i32,
    old_pixel_unpack_buffer: i32,
    old_pixel_pack_buffer: i32,
    old_blend_enabled: bool,
    old_scissor_enabled: bool,
    old_scissor: [i32; 4],
    old_stencil_enabled: bool,
    old_depth_enabled: bool,
    old_cull_enabled: bool,
    old_dither_enabled: bool,
    old_srgb_enabled: bool,
    old_color_mask: [i32; 4],
    old_blend: [i32; 4],
    old_blend_eq_rgb: i32,
    old_blend_eq_alpha: i32,
    old_unpack_alignment: i32,
    old_pack_alignment: i32,
    old_unpack_row_length: i32,
    old_unpack_skip_pixels: i32,
    old_unpack_skip_rows: i32,
    old_viewport: [i32; 4],
}

impl<'a> GlStateGuard<'a> {
    unsafe fn enter(gl: &'a glow::Context, caps: GlCapabilities) -> Self {
        let has_gl3 = caps.has_gl3;
        let has_srgb = caps.has_srgb;
        let has_samplers = caps.has_samplers;

        let old_draw_framebuffer = if has_gl3 {
            gl.get_parameter_i32(glow::DRAW_FRAMEBUFFER_BINDING)
        } else {
            gl.get_parameter_i32(glow::FRAMEBUFFER_BINDING)
        };
        let old_read_framebuffer = if has_gl3 {
            gl.get_parameter_i32(glow::READ_FRAMEBUFFER_BINDING)
        } else {
            old_draw_framebuffer
        };

        let raw_active_texture = gl.get_parameter_i32(glow::ACTIVE_TEXTURE);
        let old_active_texture = if raw_active_texture >= glow::TEXTURE0 as i32 {
            raw_active_texture as u32
        } else {
            glow::TEXTURE0
        };
        let old_texture_active = gl.get_parameter_i32(glow::TEXTURE_BINDING_2D);
        let old_sampler_active = if has_samplers {
            gl.get_parameter_i32(glow::SAMPLER_BINDING)
        } else {
            0
        };
        let (old_texture0, old_sampler0) = if old_active_texture != glow::TEXTURE0 {
            gl.active_texture(glow::TEXTURE0);
            let tex0 = gl.get_parameter_i32(glow::TEXTURE_BINDING_2D);
            let samp0 = if has_samplers {
                gl.get_parameter_i32(glow::SAMPLER_BINDING)
            } else {
                0
            };
            gl.active_texture(old_active_texture);
            (Some(tex0), Some(samp0))
        } else {
            (None, None)
        };

        let old_program = gl.get_parameter_i32(glow::CURRENT_PROGRAM);
        let old_vertex_array = if has_gl3 {
            gl.get_parameter_i32(glow::VERTEX_ARRAY_BINDING)
        } else {
            0
        };
        let old_array_buffer = gl.get_parameter_i32(glow::ARRAY_BUFFER_BINDING);
        let old_element_array_buffer = gl.get_parameter_i32(glow::ELEMENT_ARRAY_BUFFER_BINDING);
        let old_pixel_unpack_buffer = if has_gl3 {
            gl.get_parameter_i32(glow::PIXEL_UNPACK_BUFFER_BINDING)
        } else {
            0
        };
        let old_pixel_pack_buffer = if has_gl3 {
            gl.get_parameter_i32(glow::PIXEL_PACK_BUFFER_BINDING)
        } else {
            0
        };

        let old_blend_enabled = gl.is_enabled(glow::BLEND);
        let old_scissor_enabled = gl.is_enabled(glow::SCISSOR_TEST);
        let mut old_scissor = [0; 4];
        gl.get_parameter_i32_slice(glow::SCISSOR_BOX, &mut old_scissor);
        let old_stencil_enabled = gl.is_enabled(glow::STENCIL_TEST);
        let old_depth_enabled = gl.is_enabled(glow::DEPTH_TEST);
        let old_cull_enabled = gl.is_enabled(glow::CULL_FACE);
        let old_dither_enabled = gl.is_enabled(glow::DITHER);
        let old_srgb_enabled = if has_srgb {
            gl.is_enabled(glow::FRAMEBUFFER_SRGB)
        } else {
            false
        };

        let mut old_color_mask = [1i32; 4];
        gl.get_parameter_i32_slice(glow::COLOR_WRITEMASK, &mut old_color_mask);

        let old_blend = [
            gl.get_parameter_i32(glow::BLEND_SRC_RGB),
            gl.get_parameter_i32(glow::BLEND_DST_RGB),
            gl.get_parameter_i32(glow::BLEND_SRC_ALPHA),
            gl.get_parameter_i32(glow::BLEND_DST_ALPHA),
        ];
        let old_blend_eq_rgb = gl.get_parameter_i32(glow::BLEND_EQUATION_RGB);
        let old_blend_eq_alpha = gl.get_parameter_i32(glow::BLEND_EQUATION_ALPHA);

        let old_unpack_alignment = gl.get_parameter_i32(glow::UNPACK_ALIGNMENT);
        let old_pack_alignment = gl.get_parameter_i32(glow::PACK_ALIGNMENT);
        let old_unpack_row_length = if has_gl3 {
            gl.get_parameter_i32(glow::UNPACK_ROW_LENGTH)
        } else {
            0
        };
        let old_unpack_skip_pixels = if has_gl3 {
            gl.get_parameter_i32(glow::UNPACK_SKIP_PIXELS)
        } else {
            0
        };
        let old_unpack_skip_rows = if has_gl3 {
            gl.get_parameter_i32(glow::UNPACK_SKIP_ROWS)
        } else {
            0
        };

        let mut old_viewport = [0; 4];
        gl.get_parameter_i32_slice(glow::VIEWPORT, &mut old_viewport);

        // Establish libmpv's expected OpenGL defaults before invoking libmpv.
        if old_blend_enabled {
            gl.disable(glow::BLEND);
        }
        if old_scissor_enabled {
            gl.disable(glow::SCISSOR_TEST);
        }
        if old_stencil_enabled {
            gl.disable(glow::STENCIL_TEST);
        }
        if old_depth_enabled {
            gl.disable(glow::DEPTH_TEST);
        }
        if old_cull_enabled {
            gl.disable(glow::CULL_FACE);
        }
        if has_srgb && old_srgb_enabled {
            gl.disable(glow::FRAMEBUFFER_SRGB);
        }
        if old_color_mask != [1, 1, 1, 1] {
            gl.color_mask(true, true, true, true);
        }
        if has_gl3 {
            if old_pixel_unpack_buffer != 0 {
                gl.bind_buffer(glow::PIXEL_UNPACK_BUFFER, None);
            }
            if old_pixel_pack_buffer != 0 {
                gl.bind_buffer(glow::PIXEL_PACK_BUFFER, None);
            }
            if old_unpack_row_length != 0 {
                gl.pixel_store_i32(glow::UNPACK_ROW_LENGTH, 0);
            }
            if old_unpack_skip_pixels != 0 {
                gl.pixel_store_i32(glow::UNPACK_SKIP_PIXELS, 0);
            }
            if old_unpack_skip_rows != 0 {
                gl.pixel_store_i32(glow::UNPACK_SKIP_ROWS, 0);
            }
        }
        if old_unpack_alignment != 4 {
            gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 4);
        }
        if old_pack_alignment != 4 {
            gl.pixel_store_i32(glow::PACK_ALIGNMENT, 4);
        }
        if has_samplers {
            if old_sampler_active != 0 {
                let unit = old_active_texture - glow::TEXTURE0;
                gl.bind_sampler(unit, None);
            }
            if old_sampler0.is_some_and(|s| s != 0) {
                gl.bind_sampler(0, None);
            }
        }

        Self {
            gl,
            caps,
            old_draw_framebuffer,
            old_read_framebuffer,
            old_active_texture,
            old_texture_active,
            old_sampler_active,
            old_texture0,
            old_sampler0,
            old_program,
            old_vertex_array,
            old_array_buffer,
            old_element_array_buffer,
            old_pixel_unpack_buffer,
            old_pixel_pack_buffer,
            old_blend_enabled,
            old_scissor_enabled,
            old_scissor,
            old_stencil_enabled,
            old_depth_enabled,
            old_cull_enabled,
            old_dither_enabled,
            old_srgb_enabled,
            old_color_mask,
            old_blend,
            old_blend_eq_rgb,
            old_blend_eq_alpha,
            old_unpack_alignment,
            old_pack_alignment,
            old_unpack_row_length,
            old_unpack_skip_pixels,
            old_unpack_skip_rows,
            old_viewport,
        }
    }
}

impl Drop for GlStateGuard<'_> {
    fn drop(&mut self) {
        unsafe {
            let gl = self.gl;
            let has_gl3 = self.caps.has_gl3;
            let has_srgb = self.caps.has_srgb;
            let has_samplers = self.caps.has_samplers;

            // Restore all UI renderer GL bindings and state cleanly.
            if has_gl3 {
                gl.bind_framebuffer(
                    glow::DRAW_FRAMEBUFFER,
                    std::num::NonZeroU32::new(self.old_draw_framebuffer as u32)
                        .map(glow::NativeFramebuffer),
                );
                gl.bind_framebuffer(
                    glow::READ_FRAMEBUFFER,
                    std::num::NonZeroU32::new(self.old_read_framebuffer as u32)
                        .map(glow::NativeFramebuffer),
                );
            } else {
                gl.bind_framebuffer(
                    glow::FRAMEBUFFER,
                    std::num::NonZeroU32::new(self.old_draw_framebuffer as u32)
                        .map(glow::NativeFramebuffer),
                );
            }
            if let Some(tex0) = self.old_texture0 {
                gl.active_texture(glow::TEXTURE0);
                gl.bind_texture(
                    glow::TEXTURE_2D,
                    std::num::NonZeroU32::new(tex0 as u32).map(glow::NativeTexture),
                );
                if has_samplers {
                    if let Some(samp0) = self.old_sampler0 {
                        gl.bind_sampler(
                            0,
                            std::num::NonZeroU32::new(samp0 as u32).map(glow::NativeSampler),
                        );
                    }
                }
            }
            gl.active_texture(self.old_active_texture);
            gl.bind_texture(
                glow::TEXTURE_2D,
                std::num::NonZeroU32::new(self.old_texture_active as u32).map(glow::NativeTexture),
            );
            if has_samplers {
                let unit = self.old_active_texture - glow::TEXTURE0;
                gl.bind_sampler(
                    unit,
                    std::num::NonZeroU32::new(self.old_sampler_active as u32)
                        .map(glow::NativeSampler),
                );
            }
            gl.use_program(
                std::num::NonZeroU32::new(self.old_program as u32).map(glow::NativeProgram),
            );
            if has_gl3 {
                gl.bind_vertex_array(
                    std::num::NonZeroU32::new(self.old_vertex_array as u32)
                        .map(glow::NativeVertexArray),
                );
            }
            gl.bind_buffer(
                glow::ARRAY_BUFFER,
                std::num::NonZeroU32::new(self.old_array_buffer as u32).map(glow::NativeBuffer),
            );
            if !has_gl3 || self.old_vertex_array != 0 {
                gl.bind_buffer(
                    glow::ELEMENT_ARRAY_BUFFER,
                    std::num::NonZeroU32::new(self.old_element_array_buffer as u32)
                        .map(glow::NativeBuffer),
                );
            }
            if has_gl3 {
                gl.bind_buffer(
                    glow::PIXEL_UNPACK_BUFFER,
                    std::num::NonZeroU32::new(self.old_pixel_unpack_buffer as u32)
                        .map(glow::NativeBuffer),
                );
                gl.bind_buffer(
                    glow::PIXEL_PACK_BUFFER,
                    std::num::NonZeroU32::new(self.old_pixel_pack_buffer as u32)
                        .map(glow::NativeBuffer),
                );
                gl.pixel_store_i32(glow::UNPACK_ROW_LENGTH, self.old_unpack_row_length);
                gl.pixel_store_i32(glow::UNPACK_SKIP_PIXELS, self.old_unpack_skip_pixels);
                gl.pixel_store_i32(glow::UNPACK_SKIP_ROWS, self.old_unpack_skip_rows);
            }
            gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, self.old_unpack_alignment);
            gl.pixel_store_i32(glow::PACK_ALIGNMENT, self.old_pack_alignment);
            gl.viewport(
                self.old_viewport[0],
                self.old_viewport[1],
                self.old_viewport[2],
                self.old_viewport[3],
            );
            gl.scissor(
                self.old_scissor[0],
                self.old_scissor[1],
                self.old_scissor[2],
                self.old_scissor[3],
            );
            if self.old_blend_enabled {
                gl.enable(glow::BLEND);
            } else {
                gl.disable(glow::BLEND);
            }
            if self.old_scissor_enabled {
                gl.enable(glow::SCISSOR_TEST);
            } else {
                gl.disable(glow::SCISSOR_TEST);
            }
            if self.old_stencil_enabled {
                gl.enable(glow::STENCIL_TEST);
            } else {
                gl.disable(glow::STENCIL_TEST);
            }
            if self.old_depth_enabled {
                gl.enable(glow::DEPTH_TEST);
            } else {
                gl.disable(glow::DEPTH_TEST);
            }
            if self.old_cull_enabled {
                gl.enable(glow::CULL_FACE);
            } else {
                gl.disable(glow::CULL_FACE);
            }
            if self.old_dither_enabled {
                gl.enable(glow::DITHER);
            } else {
                gl.disable(glow::DITHER);
            }
            if has_srgb {
                if self.old_srgb_enabled {
                    gl.enable(glow::FRAMEBUFFER_SRGB);
                } else {
                    gl.disable(glow::FRAMEBUFFER_SRGB);
                }
            }
            gl.color_mask(
                self.old_color_mask[0] != 0,
                self.old_color_mask[1] != 0,
                self.old_color_mask[2] != 0,
                self.old_color_mask[3] != 0,
            );
            gl.blend_func_separate(
                self.old_blend[0] as u32,
                self.old_blend[1] as u32,
                self.old_blend[2] as u32,
                self.old_blend[3] as u32,
            );
            gl.blend_equation_separate(
                self.old_blend_eq_rgb as u32,
                self.old_blend_eq_alpha as u32,
            );
        }
    }
}

/// Like [`with_saved_state`], using pre-queried [`GlCapabilities`] and an RAII
/// state guard that restores all bindings and blend state even on unwind.
pub fn with_saved_state_caps<T>(
    gl: &glow::Context,
    caps: GlCapabilities,
    action: impl FnOnce() -> T,
) -> T {
    let _guard = unsafe { GlStateGuard::enter(gl, caps) };
    action()
}

/// Preserve the UI renderer's GL state around `mpv_render_context_render` and
/// `mpv_render_context_update`, while establishing libmpv's expected OpenGL
/// default state on entry (`render_gl.h`: BLEND/SCISSOR/STENCIL/DEPTH/CULL_FACE
/// disabled, full color write mask, clean PBO/unpack pixel store state).
///
/// With `MPV_RENDER_PARAM_ADVANCED_CONTROL`, `mpv_render_context_update` can
/// itself bind PBOs/textures or read back a GPU screenshot, so both update and
/// render calls must run inside this guard.
/// The caller must have this GL context current.
pub fn with_saved_state<T>(gl: &glow::Context, action: impl FnOnce() -> T) -> T {
    with_saved_state_caps(gl, GlCapabilities::from_context(gl), action)
}

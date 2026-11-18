//! State handoff between the UI renderer and libmpv's OpenGL renderer.

use glow::HasContext;

/// Allocated video FBO/texture target and its GL internal format.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoFboTarget {
    pub texture: glow::NativeTexture,
    pub framebuffer: glow::NativeFramebuffer,
    pub internal_format: i32,
}

/// Allocate or resize a color-renderable video texture and FBO, preferring
/// `GL_RGBA16F` to eliminate 8-bit color banding (#236) and falling back to
/// `GL_RGBA8` if float color attachments are unsupported.
///
/// The caller must have `gl`'s context current. Preserves existing 2D texture
/// and draw/read framebuffer bindings.
#[allow(dead_code)]
pub fn create_video_fbo_texture(
    gl: &glow::Context,
    existing_texture: Option<glow::NativeTexture>,
    existing_framebuffer: Option<glow::NativeFramebuffer>,
    width: u32,
    height: u32,
) -> Result<VideoFboTarget, String> {
    let w = i32::try_from(width)
        .ok()
        .filter(|&v| v > 0)
        .ok_or_else(|| "video FBO width must be positive and fit in i32".to_owned())?;
    let h = i32::try_from(height)
        .ok()
        .filter(|&v| v > 0)
        .ok_or_else(|| "video FBO height must be positive and fit in i32".to_owned())?;

    unsafe {
        let old_draw_fb = gl.get_parameter_i32(glow::DRAW_FRAMEBUFFER_BINDING);
        let old_read_fb = gl.get_parameter_i32(glow::READ_FRAMEBUFFER_BINDING);
        let old_texture = gl.get_parameter_i32(glow::TEXTURE_BINDING_2D);

        let restore = || {
            gl.bind_framebuffer(
                glow::DRAW_FRAMEBUFFER,
                std::num::NonZeroU32::new(old_draw_fb as u32).map(glow::NativeFramebuffer),
            );
            gl.bind_framebuffer(
                glow::READ_FRAMEBUFFER,
                std::num::NonZeroU32::new(old_read_fb as u32).map(glow::NativeFramebuffer),
            );
            gl.bind_texture(
                glow::TEXTURE_2D,
                std::num::NonZeroU32::new(old_texture as u32).map(glow::NativeTexture),
            );
        };

        let texture = match existing_texture {
            Some(tex) => tex,
            None => match gl.create_texture() {
                Ok(tex) => tex,
                Err(error) => {
                    restore();
                    return Err(error);
                }
            },
        };
        let framebuffer = match existing_framebuffer {
            Some(fb) => fb,
            None => match gl.create_framebuffer() {
                Ok(fb) => fb,
                Err(error) => {
                    if existing_texture.is_none() {
                        gl.delete_texture(texture);
                    }
                    restore();
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

        let mut last_status = 0u32;
        for (internal_format, pixel_type) in [
            (glow::RGBA16F as i32, glow::HALF_FLOAT),
            (glow::RGBA8 as i32, glow::UNSIGNED_BYTE),
        ] {
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
            if last_status == glow::FRAMEBUFFER_COMPLETE && gl.get_error() == glow::NO_ERROR {
                restore();
                return Ok(VideoFboTarget {
                    texture,
                    framebuffer,
                    internal_format,
                });
            }
            while gl.get_error() != glow::NO_ERROR {}
        }

        if existing_framebuffer.is_none() {
            gl.delete_framebuffer(framebuffer);
        }
        if existing_texture.is_none() {
            gl.delete_texture(texture);
        }
        restore();
        Err(format!("video framebuffer is incomplete: 0x{last_status:x}"))
    }
}

/// libmpv requires blending disabled on entry. UI renderers can leave it on
/// even after an otherwise empty flush. Restore the caller's setting afterward.
/// The caller must keep this GL context current throughout the operation.
pub fn without_blending<T>(gl: &glow::Context, render: impl FnOnce() -> T) -> T {
    unsafe {
        let enabled = gl.is_enabled(glow::BLEND);
        gl.disable(glow::BLEND);
        let result = render();
        if enabled {
            gl.enable(glow::BLEND);
        } else {
            gl.disable(glow::BLEND);
        }
        result
    }
}

/// Preserve the UI's GL bindings around both render and update calls. With
/// advanced control, update can itself draw/read back a GPU screenshot.
/// The caller must have this GL context current.
pub fn with_saved_state<T>(gl: &glow::Context, action: impl FnOnce() -> T) -> T {
    unsafe {
        let old_draw_framebuffer = gl.get_parameter_i32(glow::DRAW_FRAMEBUFFER_BINDING);
        let old_read_framebuffer = gl.get_parameter_i32(glow::READ_FRAMEBUFFER_BINDING);
        let old_texture = gl.get_parameter_i32(glow::TEXTURE_BINDING_2D);
        let old_active_texture = gl.get_parameter_i32(glow::ACTIVE_TEXTURE);
        let old_program = gl.get_parameter_i32(glow::CURRENT_PROGRAM);
        let old_vertex_array = gl.get_parameter_i32(glow::VERTEX_ARRAY_BINDING);
        let old_array_buffer = gl.get_parameter_i32(glow::ARRAY_BUFFER_BINDING);
        let old_element_array_buffer = gl.get_parameter_i32(glow::ELEMENT_ARRAY_BUFFER_BINDING);
        let old_scissor_enabled = gl.is_enabled(glow::SCISSOR_TEST);
        let mut old_scissor = [0; 4];
        gl.get_parameter_i32_slice(glow::SCISSOR_BOX, &mut old_scissor);
        let old_stencil_enabled = gl.is_enabled(glow::STENCIL_TEST);
        let old_blend = [
            gl.get_parameter_i32(glow::BLEND_SRC_RGB),
            gl.get_parameter_i32(glow::BLEND_DST_RGB),
            gl.get_parameter_i32(glow::BLEND_SRC_ALPHA),
            gl.get_parameter_i32(glow::BLEND_DST_ALPHA),
        ];
        let old_blend_eq_rgb = gl.get_parameter_i32(glow::BLEND_EQUATION_RGB);
        let old_blend_eq_alpha = gl.get_parameter_i32(glow::BLEND_EQUATION_ALPHA);
        let old_unpack_alignment = gl.get_parameter_i32(glow::UNPACK_ALIGNMENT);
        let mut old_viewport = [0; 4];
        gl.get_parameter_i32_slice(glow::VIEWPORT, &mut old_viewport);

        let result = without_blending(gl, action);

        gl.bind_framebuffer(
            glow::DRAW_FRAMEBUFFER,
            std::num::NonZeroU32::new(old_draw_framebuffer as u32).map(glow::NativeFramebuffer),
        );
        gl.bind_framebuffer(
            glow::READ_FRAMEBUFFER,
            std::num::NonZeroU32::new(old_read_framebuffer as u32).map(glow::NativeFramebuffer),
        );
        gl.active_texture(old_active_texture as u32);
        gl.bind_texture(
            glow::TEXTURE_2D,
            std::num::NonZeroU32::new(old_texture as u32).map(glow::NativeTexture),
        );
        gl.use_program(std::num::NonZeroU32::new(old_program as u32).map(glow::NativeProgram));
        gl.bind_vertex_array(
            std::num::NonZeroU32::new(old_vertex_array as u32).map(glow::NativeVertexArray),
        );
        gl.bind_buffer(
            glow::ARRAY_BUFFER,
            std::num::NonZeroU32::new(old_array_buffer as u32).map(glow::NativeBuffer),
        );
        gl.bind_buffer(
            glow::ELEMENT_ARRAY_BUFFER,
            std::num::NonZeroU32::new(old_element_array_buffer as u32).map(glow::NativeBuffer),
        );
        gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, old_unpack_alignment);
        gl.viewport(
            old_viewport[0],
            old_viewport[1],
            old_viewport[2],
            old_viewport[3],
        );
        gl.scissor(
            old_scissor[0],
            old_scissor[1],
            old_scissor[2],
            old_scissor[3],
        );
        if old_scissor_enabled {
            gl.enable(glow::SCISSOR_TEST);
        } else {
            gl.disable(glow::SCISSOR_TEST);
        }
        if old_stencil_enabled {
            gl.enable(glow::STENCIL_TEST);
        } else {
            gl.disable(glow::STENCIL_TEST);
        }
        gl.blend_func_separate(
            old_blend[0] as u32,
            old_blend[1] as u32,
            old_blend[2] as u32,
            old_blend[3] as u32,
        );
        gl.blend_equation_separate(old_blend_eq_rgb as u32, old_blend_eq_alpha as u32);
        result
    }
}

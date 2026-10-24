//! State handoff between the UI renderer and libmpv's OpenGL renderer.

use glow::HasContext;

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
        let old_framebuffer = gl.get_parameter_i32(glow::FRAMEBUFFER_BINDING);
        let old_texture = gl.get_parameter_i32(glow::TEXTURE_BINDING_2D);
        let old_active_texture = gl.get_parameter_i32(glow::ACTIVE_TEXTURE);
        let old_program = gl.get_parameter_i32(glow::CURRENT_PROGRAM);
        let old_vertex_array = gl.get_parameter_i32(glow::VERTEX_ARRAY_BINDING);
        let old_scissor_enabled = gl.is_enabled(glow::SCISSOR_TEST);
        let mut old_scissor = [0; 4];
        gl.get_parameter_i32_slice(glow::SCISSOR_BOX, &mut old_scissor);
        let old_blend = [
            gl.get_parameter_i32(glow::BLEND_SRC_RGB),
            gl.get_parameter_i32(glow::BLEND_DST_RGB),
            gl.get_parameter_i32(glow::BLEND_SRC_ALPHA),
            gl.get_parameter_i32(glow::BLEND_DST_ALPHA),
        ];
        let mut old_viewport = [0; 4];
        gl.get_parameter_i32_slice(glow::VIEWPORT, &mut old_viewport);
        let result = without_blending(gl, action);
        gl.bind_framebuffer(
            glow::FRAMEBUFFER,
            std::num::NonZeroU32::new(old_framebuffer as u32).map(glow::NativeFramebuffer),
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
        gl.blend_func_separate(
            old_blend[0] as u32,
            old_blend[1] as u32,
            old_blend[2] as u32,
            old_blend[3] as u32,
        );
        result
    }
}

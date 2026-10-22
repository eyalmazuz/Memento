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

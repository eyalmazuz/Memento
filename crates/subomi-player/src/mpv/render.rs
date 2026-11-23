//! Software and OpenGL `mpv_render_context` management and pixel rendering.

use std::ffi::{c_char, c_int, c_void, CStr};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::ptr::{self, NonNull};
use std::sync::{Arc, Mutex};

use super::ffi;
use super::types::GlDisplayBackend;

/// A render-only handle for a video worker. Its owner must join the worker
/// before dropping the originating `MpvInstance`.
pub struct MpvSoftwareRenderer {
    pub(super) render_lock: Arc<Mutex<RenderContext>>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum RenderApi {
    Software,
    OpenGl,
}

enum GlProcResolverInner {
    /// Borrowed synchronously during `mpv_render_context_create`, then cleared.
    Scoped(*const dyn Fn(&CStr) -> *const c_void),
    /// Owned `'static` resolver valid for the lifetime of the GL render context.
    Owned(Box<dyn Fn(&CStr) -> *const c_void + Send + Sync>),
    Cleared,
}

pub struct GlProcResolver {
    inner: Mutex<GlProcResolverInner>,
}
unsafe impl Send for GlProcResolver {}
unsafe impl Sync for GlProcResolver {}

impl GlProcResolver {
    pub(super) fn scoped(resolver: &dyn Fn(&CStr) -> *const c_void) -> Self {
        let ptr: *const (dyn Fn(&CStr) -> *const c_void + '_) = resolver;
        // SAFETY: `ScopedResolverClear` resets this pointer to `Cleared` before
        // `create_opengl_context_with_options` returns, so the borrowed
        // trait object is never dereferenced after the borrow ends.
        let erased: *const (dyn Fn(&CStr) -> *const c_void + 'static) =
            unsafe { std::mem::transmute(ptr) };
        Self {
            inner: Mutex::new(GlProcResolverInner::Scoped(erased)),
        }
    }

    pub(super) fn owned(resolver: impl Fn(&CStr) -> *const c_void + Send + Sync + 'static) -> Self {
        Self {
            inner: Mutex::new(GlProcResolverInner::Owned(Box::new(resolver))),
        }
    }

    pub(super) fn clear_scoped(&self) {
        let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if matches!(*guard, GlProcResolverInner::Scoped(_)) {
            *guard = GlProcResolverInner::Cleared;
        }
    }

    fn resolve(&self, name: &CStr) -> *mut c_void {
        let guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        match &*guard {
            GlProcResolverInner::Scoped(ptr) => {
                if ptr.is_null() {
                    ptr::null_mut()
                } else {
                    unsafe { (&**ptr)(name).cast_mut() }
                }
            }
            GlProcResolverInner::Owned(func) => func(name).cast_mut(),
            GlProcResolverInner::Cleared => ptr::null_mut(),
        }
    }
}

struct ScopedResolverClear(*mut GlProcResolver);
impl Drop for ScopedResolverClear {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { &*self.0 }.clear_scoped();
        }
    }
}

pub(super) struct RenderContext {
    pub(super) pointer: Option<NonNull<ffi::mpv_render_context>>,
    pub(super) api: RenderApi,
    pub(super) gl_proc_resolver: Option<NonNull<GlProcResolver>>,
    pub(super) display_backend: GlDisplayBackend,
    pub(super) advanced_control: bool,
    pub(super) owner_thread: Option<std::thread::ThreadId>,
}

unsafe impl Send for RenderContext {}

impl RenderContext {
    pub(super) fn free_context(&mut self) {
        if let Some(context) = self.pointer.take() {
            unsafe {
                // Always detach the update callback first so libmpv worker threads
                // stop posting render notifications before VO teardown begins.
                ffi::mpv_render_context_set_update_callback(
                    context.as_ptr(),
                    None,
                    ptr::null_mut(),
                );
                // When MPV_RENDER_PARAM_ADVANCED_CONTROL is enabled and we are on
                // the owning GL render thread, drain any pending core requests
                // (e.g., GPU screenshot / texture alloc) before tearing down the VO.
                if self.api == RenderApi::OpenGl
                    && self.advanced_control
                    && self.owner_thread == Some(std::thread::current().id())
                {
                    let _ = ffi::mpv_render_context_update(context.as_ptr());
                }
                ffi::mpv_render_context_free(context.as_ptr());
            }
        }
        // Only drop the GL proc resolver AFTER mpv_render_context_free returns,
        // because libmpv may still resolve teardown symbols during VO shutdown.
        if let Some(resolver) = self.gl_proc_resolver.take() {
            drop(unsafe { Box::from_raw(resolver.as_ptr()) });
        }
        self.display_backend = GlDisplayBackend::Unknown;
        self.advanced_control = false;
        self.owner_thread = None;
    }
}

impl MpvSoftwareRenderer {
    /// Render the current video frame into a caller-supplied tightly packed
    /// RGBA8 pixel slice. Synchronized against [`super::MpvInstance`] teardown
    /// by `render_lock`, so calling this after shutdown cleanly returns `Err`.
    pub fn render_rgba(
        &self,
        width: u32,
        height: u32,
        pixels: &mut [u8],
    ) -> Result<(), String> {
        let guard = self
            .render_lock
            .lock()
            .map_err(|_| "mpv render lock poisoned")?;
        if guard.api != RenderApi::Software {
            return Err("software rendering is inactive".into());
        }
        let pointer = guard.pointer.ok_or("mpv render context is unavailable")?;
        render_rgba_context(pointer.as_ptr(), width, height, pixels)
    }
}

pub(super) unsafe extern "C" fn resolve_gl_proc(
    ctx: *mut c_void,
    name: *const c_char,
) -> *mut c_void {
    catch_unwind(AssertUnwindSafe(|| {
        if ctx.is_null() || name.is_null() {
            return ptr::null_mut();
        }
        let resolver = unsafe { &*(ctx as *const GlProcResolver) };
        resolver.resolve(unsafe { CStr::from_ptr(name) })
    }))
    .unwrap_or(ptr::null_mut())
}

pub(super) fn create_software_context(
    handle: *mut ffi::mpv_handle,
) -> Result<NonNull<ffi::mpv_render_context>, String> {
    let mut context = ptr::null_mut();
    let mut params = [
        ffi::mpv_render_param {
            type_: ffi::MPV_RENDER_PARAM_API_TYPE,
            data: c"sw".as_ptr().cast_mut().cast(),
        },
        ffi::mpv_render_param {
            type_: ffi::MPV_RENDER_PARAM_INVALID,
            data: ptr::null_mut(),
        },
    ];
    let status =
        unsafe { ffi::mpv_render_context_create(&mut context, handle, params.as_mut_ptr()) };
    if status < 0 {
        if !context.is_null() {
            unsafe { ffi::mpv_render_context_free(context) };
        }
        return Err(format!(
            "mpv software render context failed with code {status}"
        ));
    }
    NonNull::new(context).ok_or_else(|| "mpv software render context was null".into())
}

/// Create an OpenGL `mpv_render_context` with heap-allocated [`GlProcResolver`]
/// and optional X11 or Wayland display handles for hardware decoding interop.
pub fn create_opengl_context_with_display(
    handle: *mut ffi::mpv_handle,
    resolver: GlProcResolver,
    is_scoped: bool,
    x11_display: Option<*mut c_void>,
    wl_display: Option<*mut c_void>,
) -> Result<(NonNull<ffi::mpv_render_context>, NonNull<GlProcResolver>), String> {
    create_opengl_context_with_options(handle, resolver, is_scoped, x11_display, wl_display, true)
}

/// Create an OpenGL `mpv_render_context` with explicit control over
/// `MPV_RENDER_PARAM_ADVANCED_CONTROL`.
pub fn create_opengl_context_with_options(
    handle: *mut ffi::mpv_handle,
    resolver: GlProcResolver,
    is_scoped: bool,
    x11_display: Option<*mut c_void>,
    wl_display: Option<*mut c_void>,
    advanced_control_enabled: bool,
) -> Result<(NonNull<ffi::mpv_render_context>, NonNull<GlProcResolver>), String> {
    let non_null_resolver = NonNull::from(Box::leak(Box::new(resolver)));
    let resolver_ptr = non_null_resolver.as_ptr();
    let _clear_guard = is_scoped.then_some(ScopedResolverClear(resolver_ptr));

    let mut init = ffi::mpv_opengl_init_params {
        get_proc_address: Some(resolve_gl_proc),
        get_proc_address_ctx: resolver_ptr.cast(),
    };
    let mut context = ptr::null_mut();
    let mut advanced_control: c_int = c_int::from(advanced_control_enabled);
    let mut params = Vec::with_capacity(5);
    params.push(ffi::mpv_render_param {
        type_: ffi::MPV_RENDER_PARAM_API_TYPE,
        data: c"opengl".as_ptr().cast_mut().cast(),
    });
    params.push(ffi::mpv_render_param {
        type_: ffi::MPV_RENDER_PARAM_OPENGL_INIT_PARAMS,
        data: (&mut init as *mut ffi::mpv_opengl_init_params).cast(),
    });
    if advanced_control_enabled {
        params.push(ffi::mpv_render_param {
            type_: ffi::MPV_RENDER_PARAM_ADVANCED_CONTROL,
            data: (&mut advanced_control as *mut c_int).cast(),
        });
    }
    if let Some(display) = x11_display.filter(|p| !p.is_null()) {
        params.push(ffi::mpv_render_param {
            type_: ffi::MPV_RENDER_PARAM_X11_DISPLAY,
            data: display,
        });
    } else if let Some(display) = wl_display.filter(|p| !p.is_null()) {
        params.push(ffi::mpv_render_param {
            type_: ffi::MPV_RENDER_PARAM_WL_DISPLAY,
            data: display,
        });
    }
    params.push(ffi::mpv_render_param {
        type_: ffi::MPV_RENDER_PARAM_INVALID,
        data: ptr::null_mut(),
    });

    let status =
        unsafe { ffi::mpv_render_context_create(&mut context, handle, params.as_mut_ptr()) };
    if status < 0 {
        if !context.is_null() {
            unsafe { ffi::mpv_render_context_free(context) };
        }
        drop(_clear_guard);
        drop(unsafe { Box::from_raw(resolver_ptr) });
        return Err(format!(
            "mpv OpenGL render context failed with code {status}"
        ));
    }
    let Some(non_null_ctx) = NonNull::new(context) else {
        drop(_clear_guard);
        drop(unsafe { Box::from_raw(resolver_ptr) });
        return Err("mpv OpenGL render context was null".into());
    };
    Ok((non_null_ctx, non_null_resolver))
}

pub(super) fn render_rgba_context(
    render: *mut ffi::mpv_render_context,
    width: u32,
    height: u32,
    pixels: &mut [u8],
) -> Result<(), String> {
    let needed = usize::try_from(width)
        .ok()
        .and_then(|w| w.checked_mul(4))
        .and_then(|stride| stride.checked_mul(height as usize))
        .ok_or("mpv render dimensions overflow buffer size")?;
    if width == 0
        || height == 0
        || width > i32::MAX as u32
        || height > i32::MAX as u32
        || pixels.len() < needed
        || (pixels.as_ptr() as usize) % 4 != 0
    {
        return Err("mpv render requires positive dimensions and an aligned RGBA8 buffer".into());
    }
    let mut size = [width as c_int, height as c_int];
    let mut stride = width as usize * 4;
    let mut params = [
        ffi::mpv_render_param {
            type_: ffi::MPV_RENDER_PARAM_SW_SIZE,
            data: size.as_mut_ptr().cast(),
        },
        ffi::mpv_render_param {
            type_: ffi::MPV_RENDER_PARAM_SW_FORMAT,
            data: c"rgb0".as_ptr().cast_mut().cast(),
        },
        ffi::mpv_render_param {
            type_: ffi::MPV_RENDER_PARAM_SW_STRIDE,
            data: (&mut stride as *mut usize).cast(),
        },
        ffi::mpv_render_param {
            type_: ffi::MPV_RENDER_PARAM_SW_POINTER,
            data: pixels.as_mut_ptr().cast(),
        },
        ffi::mpv_render_param {
            type_: ffi::MPV_RENDER_PARAM_INVALID,
            data: ptr::null_mut(),
        },
    ];
    let status = unsafe { ffi::mpv_render_context_render(render, params.as_mut_ptr()) };
    if status < 0 {
        return Err(format!("mpv software render failed with code {status}"));
    }
    // rgb0's fourth channel is undefined, not alpha. Slint expects fully
    // opaque pixels; slice strictly to `..needed` so trailing caller bytes
    // are untouched.
    const ALPHA_MASK_128: u128 = u128::from_ne_bytes([
        0, 0, 0, 0xFF, 0, 0, 0, 0xFF, 0, 0, 0, 0xFF, 0, 0, 0, 0xFF,
    ]);
    let (prefix, chunks, suffix) = unsafe { pixels[..needed].align_to_mut::<u128>() };
    for pixel in prefix.chunks_exact_mut(4) {
        pixel[3] = 0xFF;
    }
    for chunk in chunks {
        *chunk |= ALPHA_MASK_128;
    }
    for pixel in suffix.chunks_exact_mut(4) {
        pixel[3] = 0xFF;
    }
    Ok(())
}

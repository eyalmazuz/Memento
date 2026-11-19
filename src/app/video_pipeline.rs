//! OpenGL and software video rendering pipelines, deferred GL texture retirement,
//! single-guard per-frame GL state handoff, and optional performance instrumentation.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

use glow::HasContext;
use slint::{ComponentHandle, Model as _, Rgba8Pixel, SharedPixelBuffer};

use crate::app::window_chrome::native_window_display_pointers;
use crate::controller::player::compute_video_render_size;
use crate::platform_renderer::{PlatformRendererConfig, PlatformRendererKind, RendererOverrides};
use crate::ui::MementoApp;
use crate::video_gl::{self, GlCapabilities};
use memento_player::MpvInstance;

pub(crate) struct VideoRenderWorker {
    stop: std::sync::mpsc::SyncSender<(u32, u32)>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Drop for VideoRenderWorker {
    fn drop(&mut self) {
        let _ = self.stop.send((0, 0));
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[derive(Default)]
pub(crate) struct VideoTimings {
    pub(crate) paint_started: Option<Instant>,
    pub(crate) paint_popup_rows: usize,
    pub(crate) slowest_paint_popup_rows: usize,
    pub(crate) paint_max: Duration,
    pub(crate) video_max: Duration,
    pub(crate) queue_max: Duration,
    pub(crate) wake_max: Duration,
    pub(crate) sync_max: Duration,
    pub(crate) sync_gap_max: Duration,
    pub(crate) present_max: Duration,
    pub(crate) frames: u32,
}

impl VideoTimings {
    pub(crate) fn finish_paint(&mut self) {
        if let Some(started) = self.paint_started.take() {
            let elapsed = started.elapsed();
            if elapsed > self.paint_max {
                self.paint_max = elapsed;
                self.slowest_paint_popup_rows = self.paint_popup_rows;
            }
        }
    }

    pub(crate) fn report(
        &mut self,
        state: &memento_player::mpv::PlaybackState,
        drops_changed: bool,
        verbose: bool,
        pending: Duration,
    ) {
        if (verbose && self.frames > 0)
            || drops_changed
            || self.paint_max.as_millis() > 40
            || self.queue_max.as_millis() > 40
        {
            eprintln!(
                "[video] frames={} drops={} decoder-drops={} hwdec={} paused={} queue-max={:.1}ms mpv-max={:.1}ms draw-max={:.1}ms draw-popup-rows={} wake-max={:.1}ms sync-max={:.1}ms sync-gap={:.1}ms pending={:.1}ms present-max={:.1}ms",
                self.frames,
                state.frame_drops,
                state.decoder_drops,
                if state.hwdec_current.is_empty() {
                    "no"
                } else {
                    &state.hwdec_current
                },
                state.paused,
                self.queue_max.as_secs_f64() * 1000.0,
                self.video_max.as_secs_f64() * 1000.0,
                self.paint_max.as_secs_f64() * 1000.0,
                self.slowest_paint_popup_rows,
                self.wake_max.as_secs_f64() * 1000.0,
                self.sync_max.as_secs_f64() * 1000.0,
                self.sync_gap_max.as_secs_f64() * 1000.0,
                pending.as_secs_f64() * 1000.0,
                self.present_max.as_secs_f64() * 1000.0,
            );
        }
        *self = Self::default();
    }
}

// Includes time spent updating/layouting properties before BeforeRendering,
// which the renderer's draw timing cannot see. Drop also covers early returns.
pub(crate) struct SyncTiming {
    pub(crate) started: Instant,
    pub(crate) timings: Rc<RefCell<VideoTimings>>,
}

impl Drop for SyncTiming {
    fn drop(&mut self) {
        let elapsed = self.started.elapsed();
        let mut timings = self.timings.borrow_mut();
        timings.sync_max = timings.sync_max.max(elapsed);
    }
}

struct FrameStepOutcome {
    rendered: bool,
    frame_ready: bool,
}

struct GlVideoSurface {
    gl: glow::Context,
    caps: GlCapabilities,
    max_texture_size: Option<u32>,
    texture: Option<glow::NativeTexture>,
    framebuffer: Option<glow::NativeFramebuffer>,
    internal_format: i32,
    prefer_16f: bool,
    size: (u32, u32),
}

impl GlVideoSurface {
    fn from_context(gl: glow::Context, player: &MpvInstance, prefer_16f: bool) -> Self {
        let caps = GlCapabilities::from_context(&gl);
        let (renderer, version, max_texture_size) = unsafe {
            let renderer = gl.get_parameter_string(glow::RENDERER);
            let version = gl.get_parameter_string(glow::VERSION);
            let max_tex = gl.get_parameter_i32(glow::MAX_TEXTURE_SIZE);
            (
                renderer,
                version,
                u32::try_from(max_tex).ok().filter(|&v| v > 0),
            )
        };
        eprintln!("OpenGL video device: {renderer}; {version}");
        if let Err(error) = player.configure_opengl_decoder(&renderer) {
            eprintln!("Could not configure video decoder interop: {error}");
        }
        Self {
            gl,
            caps,
            max_texture_size,
            texture: None,
            framebuffer: None,
            internal_format: glow::RGBA8 as i32,
            prefer_16f,
            size: (0, 0),
        }
    }

    fn should_reallocate(&self, requested: (u32, u32)) -> bool {
        video_gl::should_reallocate_fbo(
            self.texture.is_some() && self.framebuffer.is_some(),
            self.size,
            requested,
        )
    }

    fn texture_info(&self) -> Option<(std::num::NonZeroU32, u32, u32)> {
        self.texture
            .map(|texture| (texture.0, self.size.0, self.size.1))
    }

    /// Perform `take_render_update`, optional FBO reallocation, and `render_opengl_with_format`
    /// under a SINGLE `with_saved_state_caps` guard per video frame (~110 -> ~55 GL calls/frame).
    fn update_and_render(
        &mut self,
        player: &MpvInstance,
        size: (u32, u32),
        force_pending: bool,
    ) -> Result<FrameStepOutcome, String> {
        let has_update = player.has_render_update();
        let needs_realloc = self.should_reallocate(size);
        if !force_pending && !has_update && !needs_realloc {
            return Ok(FrameStepOutcome {
                rendered: false,
                frame_ready: false,
            });
        }

        let gl = &self.gl;
        let caps = self.caps;
        let prefer_16f = self.prefer_16f;
        let max_texture_size = self.max_texture_size;
        let texture = &mut self.texture;
        let framebuffer = &mut self.framebuffer;
        let internal_format = &mut self.internal_format;
        let cur_size = &mut self.size;

        video_gl::with_saved_state_caps(gl, caps, || {
            let frame_ready = if has_update {
                player.take_render_update()
            } else {
                false
            };
            if !force_pending && !frame_ready && !needs_realloc {
                return Ok(FrameStepOutcome {
                    rendered: false,
                    frame_ready: false,
                });
            }
            if video_gl::should_reallocate_fbo(
                texture.is_some() && framebuffer.is_some(),
                *cur_size,
                size,
            ) {
                let target = video_gl::create_video_fbo_texture_with_caps(
                    gl,
                    caps,
                    *texture,
                    *framebuffer,
                    size.0,
                    size.1,
                    prefer_16f,
                    max_texture_size,
                )?;
                *texture = Some(target.texture);
                *framebuffer = Some(target.framebuffer);
                *internal_format = target.internal_format;
                *cur_size = size;
            }
            if let Some(fb) = *framebuffer {
                player.render_opengl_with_format(
                    fb.0.get(),
                    cur_size.0,
                    cur_size.1,
                    *internal_format,
                )?;
            }
            Ok(FrameStepOutcome {
                rendered: true,
                frame_ready,
            })
        })
    }

    fn destroy(mut self) {
        unsafe {
            if let Some(framebuffer) = self.framebuffer.take() {
                self.gl.delete_framebuffer(framebuffer);
            }
            if let Some(texture) = self.texture.take() {
                self.gl.delete_texture(texture);
            }
        }
    }
}

#[cfg(target_os = "linux")]
fn build_owned_gl_proc_resolver(
    scoped: &dyn Fn(&std::ffi::CStr) -> *const std::ffi::c_void,
    prefer_wayland_egl: bool,
) -> Option<impl Fn(&std::ffi::CStr) -> *const std::ffi::c_void + Send + Sync + 'static> {
    type GetProcFn = unsafe extern "C" fn(*const std::ffi::c_char) -> *const std::ffi::c_void;
    type GetCtxFn = unsafe extern "C" fn() -> *mut std::ffi::c_void;

    unsafe fn noload_sym(
        libs: &[&std::ffi::CStr],
        sym: &std::ffi::CStr,
    ) -> *const std::ffi::c_void {
        for lib in libs {
            let handle = unsafe { libc::dlopen(lib.as_ptr(), libc::RTLD_LAZY | libc::RTLD_NOLOAD) };
            if !handle.is_null() {
                let ptr = unsafe { libc::dlsym(handle, sym.as_ptr()) };
                unsafe { libc::dlclose(handle) };
                if !ptr.is_null() {
                    return ptr;
                }
            }
        }
        std::ptr::null()
    }

    let lookup_meta = |sym: &std::ffi::CStr, libs: &[&std::ffi::CStr]| -> *const std::ffi::c_void {
        let direct = scoped(sym);
        if !direct.is_null() {
            direct
        } else {
            unsafe { noload_sym(libs, sym) }
        }
    };

    let egl_libs: &[&std::ffi::CStr] = &[c"libEGL.so.1", c"libEGL.so"];
    let glx_libs: &[&std::ffi::CStr] = &[c"libGLX.so.0", c"libGL.so.1", c"libGLX.so"];

    let try_egl = || -> Option<GetProcFn> {
        let ctx_fn_ptr = lookup_meta(c"eglGetCurrentContext", egl_libs);
        let proc_fn_ptr = lookup_meta(c"eglGetProcAddress", egl_libs);
        if proc_fn_ptr.is_null() {
            return None;
        }
        if !ctx_fn_ptr.is_null() {
            let get_ctx: GetCtxFn = unsafe { std::mem::transmute(ctx_fn_ptr) };
            if unsafe { get_ctx() }.is_null() && !prefer_wayland_egl {
                return None;
            }
        } else if !prefer_wayland_egl {
            return None;
        }
        Some(unsafe { std::mem::transmute(proc_fn_ptr) })
    };

    let try_glx = || -> Option<GetProcFn> {
        let ctx_fn_ptr = lookup_meta(c"glXGetCurrentContext", glx_libs);
        let mut proc_fn_ptr = lookup_meta(c"glXGetProcAddressARB", glx_libs);
        if proc_fn_ptr.is_null() {
            proc_fn_ptr = lookup_meta(c"glXGetProcAddress", glx_libs);
        }
        if proc_fn_ptr.is_null() {
            return None;
        }
        if !ctx_fn_ptr.is_null() {
            let get_ctx: GetCtxFn = unsafe { std::mem::transmute(ctx_fn_ptr) };
            if unsafe { get_ctx() }.is_null() {
                return None;
            }
        }
        Some(unsafe { std::mem::transmute(proc_fn_ptr) })
    };

    let loader = if prefer_wayland_egl {
        try_egl().or_else(try_glx)
    } else {
        try_glx().or_else(try_egl)
    }?;

    Some(move |name: &std::ffi::CStr| unsafe {
        let ptr = loader(name.as_ptr());
        if !ptr.is_null() {
            ptr
        } else {
            libc::dlsym(libc::RTLD_DEFAULT, name.as_ptr())
        }
    })
}

fn video_render_size(app: &MementoApp, player: &MpvInstance, opengl_active: bool) -> (u32, u32) {
    compute_video_render_size(
        app.get_video_width(),
        app.get_video_height(),
        app.window().scale_factor(),
        player.cached_video_dimensions(),
        opengl_active,
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn install_video_pipeline(
    app: &MementoApp,
    mpv: Rc<RefCell<Option<MpvInstance>>>,
    video_timings: Rc<RefCell<VideoTimings>>,
    pending_playback_frame: Rc<Cell<Option<i32>>>,
    current_render_size: Rc<Cell<(u32, u32)>>,
    renderer_plan: &PlatformRendererConfig,
    renderer_overrides: &RendererOverrides,
    trigger_initial_loads: Rc<dyn Fn()>,
    verbose_video_timings: bool,
) -> VideoRenderWorker {
    let gl_surface = Rc::new(RefCell::new(None::<GlVideoSurface>));
    // Retired GL surfaces queued on GL -> Software fallback so textures/FBOs are
    // NEVER deleted while FemtoVG is still about to paint the current frame's
    // `BorrowedOpenGLTexture`. Drained on the next `BeforeRendering` once
    // `video_frame` has been cleared, or unconditionally on `RenderingTeardown`.
    let retired_gl_surfaces: Rc<RefCell<Vec<GlVideoSurface>>> = Rc::new(RefCell::new(Vec::new()));
    let retired_frame_cleared = Rc::new(Cell::new(true));

    let gl_pending = Rc::new(Cell::new(true));
    let gl_target_size = Rc::new(Cell::new((0u32, 0u32)));
    let gl_ready_texture = Rc::new(Cell::new(None::<(std::num::NonZeroU32, u32, u32)>));
    let gl_mpv = mpv.clone();
    let notifier_surface = gl_surface;
    let notifier_retired = retired_gl_surfaces;
    let notifier_retired_cleared = retired_frame_cleared.clone();
    let notifier_pending = gl_pending.clone();
    let notifier_size = gl_target_size.clone();
    let notifier_render_size = current_render_size.clone();
    let notifier_texture = gl_ready_texture.clone();
    let notifier_app = app.as_weak();
    let notifier_timings = video_timings.clone();
    let notifier_pending_frame = pending_playback_frame;
    let notifier_trigger_initial = trigger_initial_loads.clone();
    let gl_prefer_16f = !renderer_overrides.force_rgba8;

    if renderer_plan.primary == PlatformRendererKind::Software {
        eprintln!("Video renderer: libmpv software (per startup renderer plan)");
        trigger_initial_loads();
    } else if let Err(error) = app
        .window()
        .set_rendering_notifier(move |state, graphics_api| {
            let slint::GraphicsAPI::NativeOpenGL { get_proc_address } = graphics_api else {
                if matches!(state, slint::RenderingState::RenderingSetup) {
                    notifier_trigger_initial();
                }
                return;
            };
            match state {
                slint::RenderingState::RenderingSetup => {
                    {
                        let player_ref = gl_mpv.borrow();
                        if let Some(player) = player_ref.as_ref() {
                            let gl = unsafe {
                                glow::Context::from_loader_function_cstr(*get_proc_address)
                            };
                            let (x11_display, wl_display) = notifier_app
                                .upgrade()
                                .map(|app| native_window_display_pointers(app.window()))
                                .unwrap_or((None, None));
                            let gl_res = video_gl::with_saved_state(&gl, || {
                                #[cfg(target_os = "linux")]
                                {
                                    if let Some(owned_resolver) = build_owned_gl_proc_resolver(
                                        *get_proc_address,
                                        wl_display.is_some(),
                                    ) {
                                        player.activate_opengl_owned_with_display(
                                            owned_resolver,
                                            x11_display,
                                            wl_display,
                                        )
                                    } else {
                                        player.activate_opengl_with_display(
                                            *get_proc_address,
                                            x11_display,
                                            wl_display,
                                        )
                                    }
                                }
                                #[cfg(not(target_os = "linux"))]
                                {
                                    player.activate_opengl_with_display(
                                        *get_proc_address,
                                        x11_display,
                                        wl_display,
                                    )
                                }
                            });
                            match gl_res {
                                Ok(()) => {
                                    *notifier_surface.borrow_mut() = Some(
                                        GlVideoSurface::from_context(gl, player, gl_prefer_16f),
                                    );
                                    notifier_pending.set(true);
                                    eprintln!("Video renderer: libmpv OpenGL");
                                }
                                Err(error) => eprintln!(
                                    "OpenGL video unavailable; using software rendering: {error}"
                                ),
                            }
                        }
                    }
                    notifier_trigger_initial();
                }
                slint::RenderingState::BeforeRendering => {
                    // Drain deferred retired GL surfaces only after `video_frame` has been
                    // cleared so FemtoVG never samples a deleted GL texture handle.
                    if notifier_retired_cleared.get() {
                        let mut retired = notifier_retired.borrow_mut();
                        for old in retired.drain(..) {
                            old.destroy();
                        }
                    }
                    {
                        let mut timings = notifier_timings.borrow_mut();
                        timings.paint_started = Some(Instant::now());
                        timings.paint_popup_rows = notifier_app
                            .upgrade()
                            .filter(|app| app.get_dict_open())
                            .map_or(0, |app| app.get_popup_results().row_count());
                    }
                    let size = notifier_size.get();
                    if size == (0, 0) {
                        let player_ref = gl_mpv.borrow();
                        let surface_ref = notifier_surface.borrow();
                        if let (Some(player), Some(surface)) =
                            (player_ref.as_ref(), surface_ref.as_ref())
                        {
                            if player.has_render_update() {
                                let flags = video_gl::with_saved_state_caps(
                                    &surface.gl,
                                    surface.caps,
                                    || player.pump_render_update(),
                                );
                                if flags != 0 {
                                    notifier_pending.set(true);
                                }
                            }
                        }
                        return;
                    }
                    let player_ref = gl_mpv.borrow();
                    let Some(player) = player_ref.as_ref() else {
                        return;
                    };
                    let mut surface_ref = notifier_surface.borrow_mut();
                    let Some(surface) = surface_ref.as_mut() else {
                        return;
                    };
                    let force_pending = notifier_pending.replace(false);
                    let render_started = Instant::now();
                    match surface.update_and_render(player, size, force_pending) {
                        Ok(FrameStepOutcome {
                            rendered: false, ..
                        }) => {
                            return;
                        }
                        Ok(FrameStepOutcome {
                            rendered: true,
                            frame_ready,
                        }) => {
                            notifier_render_size.set(surface.size);
                            let texture = surface.texture_info();
                            if notifier_texture.replace(texture) != texture {
                                // Publishing an Image inside the rendering notifier
                                // re-enters Slint's window RefCell. Do it after rendering.
                                let _ = notifier_app
                                    .upgrade_in_event_loop(|app| app.invoke_update_video());
                            }
                            let mut timings = notifier_timings.borrow_mut();
                            timings.frames += 1;
                            timings.video_max = timings.video_max.max(render_started.elapsed());
                            if frame_ready {
                                timings.queue_max =
                                    timings.queue_max.max(player.render_update_delay());
                            }
                        }
                        Err(error) => {
                            eprintln!(
                                "OpenGL video failed; switching to software rendering: {error}"
                            );
                            notifier_texture.set(None);
                            notifier_render_size.set((0, 0));
                            // Defer `glDeleteTextures`/`glDeleteFramebuffers` until after
                            // `on_update_video` clears `app.video_frame` so FemtoVG never
                            // samples a deleted texture during the current frame paint!
                            if let Some(old_surface) = surface_ref.take() {
                                let fallback_res = video_gl::with_saved_state_caps(
                                    &old_surface.gl,
                                    old_surface.caps,
                                    || player.fallback_to_software(),
                                );
                                if let Err(fallback_error) = fallback_res {
                                    eprintln!("Software video fallback failed: {fallback_error}");
                                }
                                notifier_retired_cleared.set(false);
                                notifier_retired.borrow_mut().push(old_surface);
                            } else if let Err(fallback_error) = player.fallback_to_software() {
                                eprintln!("Software video fallback failed: {fallback_error}");
                            }
                            let _ =
                                notifier_app.upgrade_in_event_loop(|app| app.invoke_update_video());
                        }
                    }
                }
                slint::RenderingState::AfterRendering => {
                    if notifier_surface.borrow().is_some() {
                        if let Some(player) = gl_mpv.borrow().as_ref() {
                            player.report_render_swap();
                        }
                    }
                    notifier_timings.borrow_mut().finish_paint();
                    if verbose_video_timings {
                        let presented_at = Instant::now();
                        let timings = notifier_timings.clone();
                        // Only schedule the zero-delay presentation probe when verbose video
                        // diagnostics (`MEMENTO_VIDEO_STATS`) are explicitly enabled.
                        slint::Timer::single_shot(Duration::ZERO, move || {
                            let mut timings = timings.borrow_mut();
                            timings.present_max = timings.present_max.max(presented_at.elapsed());
                        });
                    }
                    if notifier_texture.get().is_some() {
                        if let Some(generation) = notifier_pending_frame.take() {
                            // This runs after Slint returns from drawing/presenting,
                            // never while its rendering/window RefCells are borrowed.
                            let _ = notifier_app.upgrade_in_event_loop(move |app| {
                                app.invoke_video_load_presented(generation);
                            });
                        }
                    }
                }
                slint::RenderingState::RenderingTeardown => {
                    notifier_texture.set(None);
                    notifier_render_size.set((0, 0));
                    let surface = notifier_surface.borrow_mut().take();
                    if let Some(player) = gl_mpv.borrow().as_ref() {
                        if let Some(ref surface) = surface {
                            video_gl::with_saved_state_caps(&surface.gl, surface.caps, || {
                                player.deactivate_opengl();
                            });
                        } else {
                            player.deactivate_opengl();
                        }
                    }
                    if let Some(surface) = surface {
                        surface.destroy();
                    }
                    for old in notifier_retired.borrow_mut().drain(..) {
                        old.destroy();
                    }
                    notifier_retired_cleared.set(true);
                }
                _ => {}
            }
        })
    {
        eprintln!("OpenGL video integration unavailable; using software rendering: {error}");
        trigger_initial_loads();
    }

    // Fallback CPU renderer runs off the UI thread and is inactive while GL
    // rendering is available. Bounded channels discard stale frames.
    let (render_request_tx, render_request_rx) = std::sync::mpsc::sync_channel::<(u32, u32)>(1);
    let (render_frame_tx, render_frame_rx) =
        std::sync::mpsc::sync_channel::<SharedPixelBuffer<Rgba8Pixel>>(1);
    let software_frame_app = app.as_weak();
    let renderer = mpv.borrow().as_ref().map(MpvInstance::software_renderer);
    let render_thread = renderer.map(move |renderer| {
        std::thread::spawn(move || {
            let mut frame_pool: [Option<SharedPixelBuffer<Rgba8Pixel>>; 3] = [None, None, None];
            let mut pool_idx = 0usize;
            while let Ok(mut size) = render_request_rx.recv() {
                if size == (0, 0) {
                    break;
                }
                while let Ok(newer) = render_request_rx.try_recv() {
                    size = newer;
                }
                if size == (0, 0) {
                    break;
                }
                let mut buffer = match frame_pool[pool_idx].take() {
                    Some(buf) if buf.width() == size.0 && buf.height() == size.1 => buf,
                    _ => SharedPixelBuffer::<Rgba8Pixel>::new(size.0, size.1),
                };
                // VideoRenderWorker joins before the mpv instance can be dropped.
                if let Err(error) =
                    unsafe { renderer.render_rgba(size.0, size.1, buffer.make_mut_bytes()) }
                {
                    eprintln!("Cannot render video frame: {error}");
                    frame_pool[pool_idx] = Some(buffer);
                    continue;
                }
                if render_frame_tx.try_send(buffer.clone()).is_ok() {
                    let _ =
                        software_frame_app.upgrade_in_event_loop(|app| app.invoke_update_video());
                }
                frame_pool[pool_idx] = Some(buffer);
                pool_idx = (pool_idx + 1) % frame_pool.len();
            }
        })
    });
    let render_worker = VideoRenderWorker {
        stop: render_request_tx.clone(),
        thread: render_thread,
    };

    let video_app = app.as_weak();
    let video_mpv = mpv.clone();
    let video_requests = render_request_tx;
    let video_gl_pending = gl_pending;
    let video_gl_target_size = gl_target_size;
    let video_gl_ready_texture = gl_ready_texture;
    let video_current_render_size = current_render_size;
    let video_retired_cleared = retired_frame_cleared;
    let mut published_gl_texture = None;
    let mut rendered_size = (0, 0);
    app.on_update_video(move || {
        let Some(app) = video_app.upgrade() else {
            return;
        };
        if app.get_video_width() <= 0 || app.get_video_height() <= 0 {
            video_gl_target_size.set((0, 0));
            video_current_render_size.set((0, 0));
            if published_gl_texture.take().is_some() {
                app.set_video_frame(slint::Image::default());
            }
            video_retired_cleared.set(true);
            return;
        }
        let Ok(player_ref) = video_mpv.try_borrow() else {
            return;
        };
        let Some(player) = player_ref.as_ref() else {
            return;
        };
        if player.is_opengl_active() {
            rendered_size = (0, 0);
            while render_frame_rx.try_recv().is_ok() {}
            if let Some(texture) = video_gl_ready_texture.get() {
                if published_gl_texture != Some(texture) {
                    app.set_video_frame(unsafe {
                        slint::BorrowedOpenGLTextureBuilder::new_gl_2d_rgba_texture(
                            texture.0,
                            [texture.1, texture.2].into(),
                        )
                        .origin(slint::BorrowedOpenGLTextureOrigin::TopLeft)
                        .build()
                    });
                    published_gl_texture = Some(texture);
                }
            }
            let size = video_render_size(&app, player, true);
            if video_gl_target_size.replace(size) != size {
                video_gl_pending.set(true);
                app.window().request_redraw();
            }
            if player.has_render_update() {
                app.window().request_redraw();
            }
            return;
        }
        if published_gl_texture.take().is_some() {
            app.set_video_frame(slint::Image::default());
        }
        // Safe for next `BeforeRendering` to reclaim any retired GL texture/FBO now that
        // `app.video_frame` no longer references a `BorrowedOpenGLTexture`.
        video_retired_cleared.set(true);

        let mut latest_frame = None;
        while let Ok(frame) = render_frame_rx.try_recv() {
            latest_frame = Some(frame);
        }
        if let Some(frame) = latest_frame {
            app.set_video_frame(slint::Image::from_rgba8(frame));
        }
        let (mut target_w, mut target_h) = video_render_size(&app, player, false);
        // mpv's software API converts and scales on one CPU thread. Cap
        // fallback output area so a 1080p window does not upload 8 MB on
        // every frame if GL cannot be used.
        let area = u64::from(target_w) * u64::from(target_h);
        if area > 1280 * 720 {
            let ratio = ((1280 * 720) as f64 / area as f64).sqrt();
            target_w = ((target_w as f64 * ratio).round() as u32).max(1);
            target_h = ((target_h as f64 * ratio).round() as u32).max(1);
        }
        video_current_render_size.set((target_w, target_h));

        if rendered_size == (target_w, target_h) && !player.take_render_update() {
            return;
        }
        if video_requests.try_send((target_w, target_h)).is_ok() {
            rendered_size = (target_w, target_h);
        }
    });

    let ready_app = app.as_weak();
    let ready_mpv = mpv.clone();
    let ready_timings = video_timings;
    app.on_video_frame_ready(move || {
        let Some(app) = ready_app.upgrade() else {
            return;
        };
        let Ok(player_ref) = ready_mpv.try_borrow() else {
            return;
        };
        let Some(player) = player_ref.as_ref() else {
            return;
        };
        let mut timings = ready_timings.borrow_mut();
        timings.wake_max = timings.wake_max.max(player.pending_render_delay());
        drop(timings);
        if player.is_opengl_active() {
            // A frame changes the shared texture, not the scene geometry.
            // Don't evaluate video/subtitle preferred sizes on every wakeup.
            app.window().request_redraw();
        } else {
            app.invoke_update_video();
        }
    });

    // mpv wakes Slint as soon as a frame is ready. Coalescing happens in the
    // player callback; there is no frame-rate polling timer in the GL path.
    let frame_ready_app = app.as_weak();
    if let Some(player) = mpv.borrow().as_ref() {
        player.set_render_wakeup(move || {
            let _ = frame_ready_app.upgrade_in_event_loop(|app| app.invoke_video_frame_ready());
        });
    }
    app.invoke_update_video();

    render_worker
}

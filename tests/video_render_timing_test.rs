//! Optional headless playback test using the actual Slint/FemtoVG window.
//! It includes scene rendering, but not desktop/compositor presentation.
//! Supply MEMENTO_VIDEO_TEST_MEDIA to test a file.
#![cfg(target_os = "linux")]

use glow::HasContext;
use memento_app::{DictResult, MementoApp, PopupGlossaryBlock, SubtitleLine, SubtitleWord};
use memento_player::mpv::{ffi, MpvInstance};
use slint::{ComponentHandle, Model};
use std::ffi::{c_char, c_int, c_void, CStr};
use std::ptr;
use std::rc::Rc;
use std::time::{Duration, Instant};

#[path = "support/headless_slint.rs"]
mod headless_slint;
#[path = "support/playback_actions.rs"]
mod playback_actions;
#[path = "support/startup_subtitles.rs"]
mod startup_subtitles;

fn change_subtitle(app: &MementoApp, index: usize) {
    let lines = [
        &[
            ("生前", "せいぜん"),
            ("に", ""),
            ("世界中", "せかいじゅう"),
            ("を", ""),
            ("旅行", "りょこう"),
            ("して", ""),
            ("集めた", "あつめた"),
            ("遺品", "いひん"),
        ][..],
        &[
            ("港", "みなと"),
            ("まで", ""),
            ("迎え", "むかえ"),
            ("に", ""),
            ("行く", "いく"),
            ("つもりだったんだけど", ""),
        ][..],
        &[
            ("去年", "きょねん"),
            ("までは", ""),
            ("一日", "いちにち"),
            ("だって", ""),
            ("休み", "やすみ"),
            ("はなかったのに", ""),
        ][..],
        &[
            ("鳥白島", "とりしろじま"),
            ("に", ""),
            ("来ませんか", "きませんか"),
            ("？", ""),
        ][..],
    ];
    let parts = lines[index % lines.len()];
    let text: String = parts.iter().map(|(text, _)| *text).collect();
    let words: Vec<_> = parts
        .iter()
        .enumerate()
        .map(|(index, (text, reading))| SubtitleWord {
            text: (*text).into(),
            reading: (*reading).into(),
            query: (*text).into(),
            index: index as i32,
        })
        .collect();
    app.set_active_sub_text(text.into());
    app.set_active_sub_words(Rc::new(slint::VecModel::from(words.clone())).into());
    app.set_active_sub_lines(
        Rc::new(slint::VecModel::from(vec![SubtitleLine {
            words: Rc::new(slint::VecModel::from(words)).into(),
        }]))
        .into(),
    );
}

fn open_dictionary(app: &MementoApp) {
    let rows: Vec<_> = (0..8)
        .map(|index| DictResult {
            index: index + 1,
            expression: "迎えに行く".into(),
            reading: "むかえにいく".into(),
            dict_name: format!("辞書 {}", index + 1).into(),
            selected: true,
            term_start: index == 0,
            term_end: index == 7,
            glossary_blocks: Rc::new(slint::VecModel::from(vec![PopupGlossaryBlock {
                text: "相手を自分の元へ迎える目的で出かけること。出かけて行って相手を迎えること。"
                    .repeat(4)
                    .into(),
                kind: 2,
                href: "".into(),
            }]))
            .into(),
            ..Default::default()
        })
        .collect();
    let rows = Rc::new(slint::VecModel::from(rows));
    app.set_dict_results(rows.clone().into());
    let count = memento_app::next_popup_batch_end(&rows.clone().into(), 0);
    app.set_popup_results(
        Rc::new(slint::VecModel::from(
            (0..count)
                .filter_map(|index| rows.row_data(index))
                .collect::<Vec<_>>(),
        ))
        .into(),
    );
    app.set_popup_loaded_count(count as i32);
    app.set_dict_open(true);
}

struct TestGlContext {
    display: *mut c_void,
    surface: *mut c_void,
    context: *mut c_void,
    make_current: unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void, *mut c_void) -> u32,
    get_proc: unsafe extern "C" fn(*const c_char) -> *const c_void,
}

unsafe impl i_slint_renderer_femtovg::opengl::OpenGLInterface for TestGlContext {
    fn ensure_current(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let ok =
            unsafe { (self.make_current)(self.display, self.surface, self.surface, self.context) };
        if ok != 0 {
            Ok(())
        } else {
            Err("eglMakeCurrent failed".into())
        }
    }
    fn swap_buffers(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        Ok(())
    }
    fn resize(
        &self,
        _: std::num::NonZeroU32,
        _: std::num::NonZeroU32,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        Ok(())
    }
    fn get_proc_address(&self, name: &CStr) -> *const c_void {
        unsafe { (self.get_proc)(name.as_ptr()) }
    }
}

#[test]
#[ignore = "requires EGL surfaceless OpenGL; run explicitly for full-window playback timing"]
fn opengl_frames_and_async_state_keep_up_with_1080p_video() {
    // Load EGL dynamically so normal builds/tests do not require its SDK.
    unsafe {
        let egl = libloading::Library::new("libEGL.so.1").unwrap();
        let display_fn = egl
            .get::<unsafe extern "C" fn(u32, *mut c_void, *const isize) -> *mut c_void>(
                b"eglGetPlatformDisplay\0",
            )
            .unwrap();
        let initialize = egl
            .get::<unsafe extern "C" fn(*mut c_void, *mut c_int, *mut c_int) -> u32>(
                b"eglInitialize\0",
            )
            .unwrap();
        let bind_api = egl
            .get::<unsafe extern "C" fn(u32) -> u32>(b"eglBindAPI\0")
            .unwrap();
        let choose = egl
            .get::<unsafe extern "C" fn(
                *mut c_void,
                *const c_int,
                *mut *mut c_void,
                c_int,
                *mut c_int,
            ) -> u32>(b"eglChooseConfig\0")
            .unwrap();
        let create =
            egl.get::<unsafe extern "C" fn(
                *mut c_void,
                *mut c_void,
                *mut c_void,
                *const c_int,
            ) -> *mut c_void>(b"eglCreateContext\0")
                .unwrap();
        let make_current = egl
            .get::<unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void, *mut c_void) -> u32>(
                b"eglMakeCurrent\0",
            )
            .unwrap();
        let get_proc = egl
            .get::<unsafe extern "C" fn(*const c_char) -> *const c_void>(b"eglGetProcAddress\0")
            .unwrap();
        let destroy = egl
            .get::<unsafe extern "C" fn(*mut c_void, *mut c_void) -> u32>(b"eglDestroyContext\0")
            .unwrap();
        let create_surface = egl
            .get::<unsafe extern "C" fn(*mut c_void, *mut c_void, *const c_int) -> *mut c_void>(
                b"eglCreatePbufferSurface\0",
            )
            .unwrap();
        let destroy_surface = egl
            .get::<unsafe extern "C" fn(*mut c_void, *mut c_void) -> u32>(b"eglDestroySurface\0")
            .unwrap();
        let terminate = egl
            .get::<unsafe extern "C" fn(*mut c_void) -> u32>(b"eglTerminate\0")
            .unwrap();
        let display = display_fn(
            0x31DD, /* EGL_PLATFORM_SURFACELESS_MESA */
            ptr::null_mut(),
            ptr::null(),
        );
        assert!(!display.is_null());
        assert_ne!(initialize(display, ptr::null_mut(), ptr::null_mut()), 0);
        assert_ne!(bind_api(0x30A2 /* EGL_OPENGL_API */), 0);
        let mut config = ptr::null_mut();
        let mut count = 0;
        assert_ne!(
            choose(
                display,
                [
                    0x3033, 1, // EGL_SURFACE_TYPE, EGL_PBUFFER_BIT
                    0x3040, 8,      // EGL_RENDERABLE_TYPE, EGL_OPENGL_BIT
                    0x3038, // EGL_NONE
                ]
                .as_ptr(),
                &mut config,
                1,
                &mut count
            ),
            0
        );
        assert!(count > 0);
        let context = create(
            display,
            config,
            ptr::null_mut(),
            [
                0x3098, 3, 0x30FB, 3, 0x3038, // OpenGL 3.3
            ]
            .as_ptr(),
        );
        assert!(!context.is_null());
        let surface = create_surface(
            display,
            config,
            [0x3057, 1280, 0x3056, 720, 0x3038].as_ptr(),
        );
        assert!(!surface.is_null());
        assert_ne!(make_current(display, surface, surface, context), 0);
        let resolver = |name: &CStr| get_proc(name.as_ptr());
        let gl = glow::Context::from_loader_function_cstr(resolver);
        eprintln!(
            "Test GL device: {}",
            gl.get_parameter_string(glow::RENDERER)
        );
        let texture = gl.create_texture().unwrap();
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
        gl.tex_image_2d(
            glow::TEXTURE_2D,
            0,
            glow::RGBA8 as i32,
            1920,
            1080,
            0,
            glow::RGBA,
            glow::UNSIGNED_BYTE,
            glow::PixelUnpackData::Slice(None),
        );
        let fbo = gl.create_framebuffer().unwrap();
        gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo));
        gl.framebuffer_texture_2d(
            glow::FRAMEBUFFER,
            glow::COLOR_ATTACHMENT0,
            glow::TEXTURE_2D,
            Some(texture),
            0,
        );
        assert_eq!(
            gl.check_framebuffer_status(glow::FRAMEBUFFER),
            glow::FRAMEBUFFER_COMPLETE
        );
        gl.bind_framebuffer(glow::FRAMEBUFFER, None);
        let (app, window) = headless_slint::create_window(TestGlContext {
            display,
            surface,
            context,
            make_current: *make_current,
            get_proc: *get_proc,
        });
        memento_app::prepare_text_fonts();
        app.set_video_frame(
            slint::BorrowedOpenGLTextureBuilder::new_gl_2d_rgba_texture(
                texture.0,
                [1920, 1080].into(),
            )
            .origin(slint::BorrowedOpenGLTextureOrigin::TopLeft)
            .build(),
        );
        app.set_is_playing(true);
        app.set_furigana_visible(true);
        // Warm up the real scene/font caches before timing playback.
        window.renderer.render().unwrap();
        gl.finish();
        let directory =
            std::env::temp_dir().join(format!("memento-gl-test-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join("input.conf"), "").unwrap();
        std::fs::write(
            directory.join("mpv.conf"),
            "ao=null\nhwdec=no\nmsg-level=all=warn\n",
        )
        .unwrap();
        let player = MpvInstance::new_software(Some(&directory)).unwrap();
        player.activate_opengl(&resolver).unwrap();
        let (wake, events) = std::sync::mpsc::channel();
        player.set_render_wakeup(move || {
            let _ = wake.send(());
        });
        let source = std::env::var("MEMENTO_VIDEO_TEST_MEDIA").unwrap_or_else(|_| {
            "av://lavfi:testsrc2=size=1920x1080:rate=24000/1001,format=yuv420p10le".into()
        });
        player.load_file(&source).unwrap();
        // Match application startup: prepare the first decoded frame and the
        // playback metadata in the real UI before starting the audio clock.
        let ready_deadline = Instant::now() + Duration::from_secs(10);
        let mut first_frame = false;
        let mut ready_since = None;
        loop {
            assert!(
                Instant::now() < ready_deadline,
                "first video frame did not load"
            );
            let _ = events.recv_timeout(Duration::from_millis(20));
            while events.try_recv().is_ok() {}
            if player.take_render_update() {
                player.render_opengl(fbo.0.get(), 1920, 1080).unwrap();
                first_frame = true;
            }
            let state = player.poll_playback_state();
            app.set_position(state.position as f32);
            app.set_duration(state.duration as f32);
            app.set_current_time(format!("{:.0}", state.position).into());
            let ready = first_frame && state.playback_ready && state.dimensions.0 > 0;
            drop(state);
            window.renderer.render().unwrap();
            gl.finish();
            if ready {
                let since = ready_since.get_or_insert_with(Instant::now);
                if since.elapsed() >= Duration::from_millis(100) {
                    break;
                }
            }
        }
        // Read the paused baseline directly: property notifications can lag
        // behind the final redraw, especially while the playback clock is off.
        let mut startup_drops: i64 = -1;
        assert_eq!(
            ffi::mpv_get_property(
                player.raw_handle(),
                c"frame-drop-count".as_ptr(),
                ffi::MPV_FORMAT_INT64,
                (&mut startup_drops as *mut i64).cast()
            ),
            0
        );
        eprintln!("Prepared first frame while paused; startup drops={startup_drops}");
        player.play().unwrap();
        let started = Instant::now();
        let seconds = std::env::var("MEMENTO_VIDEO_TEST_SECONDS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(5);
        let deadline = started + Duration::from_secs(seconds);
        let mut last_poll = started;
        let mut rendered = 0;
        let mut render_time = Duration::ZERO;
        let mut longest_render = Duration::ZERO;
        let mut longest_poll = Duration::ZERO;
        let mut ui_time = Duration::ZERO;
        let mut longest_ui = Duration::ZERO;
        let mut drawn = 0;
        let mut subtitle_index = 0;
        let mut popup_opened = false;
        let with_popup = std::env::var_os("MEMENTO_VIDEO_TEST_POPUP").is_some();
        let mut actions = std::env::var_os("MEMENTO_VIDEO_TEST_INTERACTIONS")
            .map(|_| playback_actions::PlaybackActions::new(&app));
        let mut startup_subtitles = std::env::var_os("MEMENTO_VIDEO_TEST_STARTUP_FURIGANA")
            .map(|_| startup_subtitles::StartupSubtitles::new());
        assert!(
            actions.is_none() || startup_subtitles.is_none(),
            "startup test must leave furigana on without interactions"
        );
        if actions.is_some() {
            assert!(seconds >= 30, "interaction run needs at least 30 seconds");
        }
        let mut last_drops = startup_drops;
        let mut accumulated_drops = 0;
        while Instant::now() < deadline {
            let _ = events.recv_timeout(Duration::from_millis(20));
            while events.try_recv().is_ok() {}
            let ui_started = Instant::now();
            slint::platform::update_timers_and_animations();
            let mut updates = actions.as_mut().map_or_else(Vec::new, |actions| {
                actions.tick(&app, &player, started.elapsed())
            });
            if let Some(subtitles) = &mut startup_subtitles {
                updates.extend(subtitles.tick(&app, &player.poll_playback_state()));
            }
            let next_subtitle = started.elapsed().as_secs() as usize / 2 + 1;
            let changed_subtitle = next_subtitle != subtitle_index;
            if changed_subtitle && startup_subtitles.is_none() {
                change_subtitle(&app, next_subtitle - 1);
                subtitle_index = next_subtitle;
                if let Some(actions) = &actions {
                    actions.subtitle_changed(&app);
                }
                updates.push("subtitle changed".into());
            }
            let opening_popup = with_popup
                && actions.is_none()
                && !popup_opened
                && started.elapsed().as_secs() >= 3;
            if opening_popup {
                open_dictionary(&app);
                popup_opened = true;
                updates.push("popup opened".into());
            }
            let update_time = ui_started.elapsed();
            let video_ready = player.take_render_update();
            if video_ready {
                let start = Instant::now();
                player.render_opengl(fbo.0.get(), 1920, 1080).unwrap();
                // Include GPU completion in this headless measurement.
                gl.finish();
                let elapsed = start.elapsed();
                render_time += elapsed;
                longest_render = longest_render.max(elapsed);
                rendered += 1;
            }
            if video_ready || !updates.is_empty() || window.redraw_requested.get() {
                window.redraw_requested.set(false);
                let ui_started = Instant::now();
                window.renderer.render().unwrap();
                gl.finish();
                let ui_elapsed = ui_started.elapsed() + update_time;
                ui_time += ui_elapsed;
                longest_ui = longest_ui.max(ui_elapsed);
                drawn += 1;
                if !updates.is_empty() || ui_elapsed > Duration::from_millis(40) {
                    eprintln!("[interaction {:.1}s] {}: update+draw={:.2}ms (update={:.2}ms, popup-rows={})",
                        started.elapsed().as_secs_f64(), updates.join("; "), ui_elapsed.as_secs_f64() * 1000.,
                        update_time.as_secs_f64() * 1000., app.get_popup_results().row_count());
                }
            }
            if last_poll.elapsed() >= Duration::from_millis(100) {
                let start = Instant::now();
                let state = player.poll_playback_state();
                app.set_position(state.position as f32);
                app.set_duration(state.duration as f32);
                app.set_current_time(format!("{:.0}", state.position).into());
                if state.frame_drops > last_drops {
                    let added = state.frame_drops - last_drops;
                    accumulated_drops += added;
                    eprintln!(
                        "[drops {:.1}s] +{added}, paused={}, since-seek={:?}",
                        started.elapsed().as_secs_f64(),
                        state.paused,
                        actions
                            .as_ref()
                            .and_then(|a| a.last_seek.map(|t| t.elapsed()))
                    );
                }
                last_drops = state.frame_drops;
                longest_poll = longest_poll.max(start.elapsed());
                last_poll = Instant::now();
            }
        }
        player.command(&["set", "pause", "yes"]).unwrap();
        let mut drops: i64 = -1;
        assert_eq!(
            ffi::mpv_get_property(
                player.raw_handle(),
                c"frame-drop-count".as_ptr(),
                ffi::MPV_FORMAT_INT64,
                (&mut drops as *mut i64).cast()
            ),
            0
        );
        accumulated_drops += (drops - last_drops).max(0);
        eprintln!("{} frames in {:.2}s; drops={}; render mean={:.2}ms max={:.2}ms; state poll max={:.2}ms",
            rendered, started.elapsed().as_secs_f64(), drops,
            render_time.as_secs_f64() * 1000.0 / rendered.max(1) as f64,
            longest_render.as_secs_f64() * 1000.0, longest_poll.as_secs_f64() * 1000.0);
        eprintln!(
            "Slint scene draw mean={:.2}ms max={:.2}ms",
            ui_time.as_secs_f64() * 1000.0 / drawn.max(1) as f64,
            longest_ui.as_secs_f64() * 1000.0
        );
        if let Some(actions) = &actions {
            actions.verify();
            eprintln!("Completed {} searches, {} seeks, {} furigana updates; accumulated drops={accumulated_drops}",
                actions.searches, actions.seeks, actions.ruby_updates);
        }
        if let Some(subtitles) = &startup_subtitles {
            eprintln!(
                "Startup: {} subtitle notifications, {} furigana updates",
                subtitles.publications, subtitles.ruby_updates
            );
            assert!(
                subtitles.ruby_updates >= 3,
                "test media needs changing Japanese subtitles"
            );
            if !app.get_active_sub_text().is_empty() {
                window.renderer.render().unwrap();
                let visible = app.window().take_snapshot().unwrap();
                app.set_subtitles_visible(false);
                window.renderer.render().unwrap();
                let hidden = app.window().take_snapshot().unwrap();
                app.set_subtitles_visible(true);
                window.renderer.render().unwrap();
                let restored = app.window().take_snapshot().unwrap();
                // Count newly visible white glyph pixels, not arbitrary damage:
                // an invalid cached layer can change the background while still
                // losing the text. Check restoration after toggling as well.
                for (label, pixels) in [("initial", &visible), ("restored", &restored)] {
                    let glyph_pixels = pixels.as_bytes().chunks_exact(4)
                        .zip(hidden.as_bytes().chunks_exact(4))
                        .filter(|(a, b)| {
                            a[0] > 220 && a[1] > 220 && a[2] > 220
                                && (0..3).any(|i| a[i].saturating_sub(b[i]) > 40)
                        }).count();
                    assert!(glyph_pixels > 1000,
                        "{label} subtitle layer has no visible glyphs ({glyph_pixels} pixels)");
                }
            }
        }
        if let Some(path) = std::env::var_os("MEMENTO_VIDEO_TEST_SCREENSHOT") {
            if app.get_dict_results().row_count() > 0 {
                app.set_dict_open(true);
            }
            window.renderer.render().unwrap();
            let pixels = app.window().take_snapshot().unwrap();
            image::save_buffer(
                path,
                pixels.as_bytes(),
                pixels.width(),
                pixels.height(),
                image::ColorType::Rgba8,
            )
            .unwrap();
        }
        app.hide().unwrap();
        drop(app);
        drop(window);
        player.deactivate_opengl();
        drop(player);
        gl.delete_framebuffer(fbo);
        gl.delete_texture(texture);
        make_current(display, ptr::null_mut(), ptr::null_mut(), ptr::null_mut());
        destroy(display, context);
        destroy_surface(display, surface);
        terminate(display);
        std::fs::remove_dir_all(directory).unwrap();
        assert!(
            rendered >= seconds * 20,
            "renderer did not keep up with 24fps video"
        );
        if actions.is_some() {
            assert_eq!(
                accumulated_drops, 0,
                "frames dropped during interactions (seeks reset mpv's counter)"
            );
        } else {
            assert_eq!(drops, startup_drops, "frames dropped during timed playback");
        }
    }
}

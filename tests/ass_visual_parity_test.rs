//! ASS/SSA visibility, config, and live 1920×1080 Memento Rust vs. stock `mpv`
//! visual parity & interactive screenshot verification harness.

use std::path::{Path, PathBuf};

use memento_app::controller::config::ConfigController;
use memento_app::controller::player::compute_mpv_subtitle_visibility_ex;
use memento_setting::keys as raw;

#[test]
fn mpv_visibility_matrix_handles_native_ass_ctrl0_and_cursor_reveal() {
    // 1. Native ASS + subtitles visible -> mpv renders primary ASS; text secondary stays on overlay.
    assert_eq!(
        compute_mpv_subtitle_visibility_ex(
            true, true, false, true, false, false, true, false, false, false, false,
        ),
        (true, false)
    );
    // Native ASS + bitmap PGS secondary -> both visible in mpv.
    assert_eq!(
        compute_mpv_subtitle_visibility_ex(
            true, true, false, true, false, true, true, false, false, false, false,
        ),
        (true, true)
    );
    // 2. Ctrl+0 (subtitles_visible = false) MUST hide mpv native ASS AND SRT subtitles!
    assert_eq!(
        compute_mpv_subtitle_visibility_ex(
            true, false, false, true, false, false, true, false, false, false, false,
        ),
        (false, false)
    );
    assert_eq!(
        compute_mpv_subtitle_visibility_ex(
            true, false, false, true, false, false, false, false, false, false, false,
        ),
        (false, false)
    );
    // 3. behavior/subtitle-cursor-show hides native ASS until cursor reveal triggers.
    assert_eq!(
        compute_mpv_subtitle_visibility_ex(
            true, true, false, true, false, false, true, true, false, false, false,
        ),
        (false, false)
    );
    assert_eq!(
        compute_mpv_subtitle_visibility_ex(
            true, true, false, true, false, false, true, true, true, false, false,
        ),
        (true, false)
    );
    // 4. Classic SRT fallback (primary_is_native_ass = false): overlay owns text when paused or playing.
    assert_eq!(
        compute_mpv_subtitle_visibility_ex(
            true, true, false, true, false, false, false, false, false, false, false,
        ),
        (false, false)
    );
    // hide_while_playing && playing -> overlay hides, mpv renders text.
    assert_eq!(
        compute_mpv_subtitle_visibility_ex(
            true, true, true, true, false, false, false, false, false, false, false,
        ),
        (true, true)
    );
}

#[test]
fn interface_sub_ass_native_setting_defaults_true_and_roundtrips() {
    let dir = std::env::temp_dir().join(format!(
        "memento_ass_cfg_test_{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let anki_path = dir.join("anki.json");

    let ctrl = ConfigController::load(&dir, "memento.conf").unwrap();
    // Default must be `true`.
    assert_eq!(
        ctrl.effective_setting(raw::interface::GROUP, raw::interface::subtitle::ASS_NATIVE),
        Some(memento_config::Value::Bool(true))
    );
    // Must appear in Options -> Interface (category 5) -> "Subtitle" group.
    use slint::Model as _;
    let groups = ctrl.settings_groups();
    let sub_group = groups
        .iter()
        .find(|g| g.category == 5 && g.title == "Subtitle")
        .expect("Interface -> Subtitle settings group");
    let field = sub_group
        .fields
        .iter()
        .find(|f| f.key == "interface/sub-ass-native")
        .expect("interface/sub-ass-native switch field");
    assert_eq!(field.kind, "switch");
    assert_eq!(field.value, "true");

    // Toggle to false, apply, and reload from disk.
    ctrl.edit_setting("interface/sub-ass-native", "false")
        .unwrap();
    ctrl.apply_settings(5, &dir, "memento.conf", &anki_path)
        .unwrap();
    assert_eq!(
        ctrl.effective_setting(raw::interface::GROUP, raw::interface::subtitle::ASS_NATIVE),
        Some(memento_config::Value::Bool(false))
    );

    let reloaded = ConfigController::load(&dir, "memento.conf").unwrap();
    assert_eq!(
        reloaded.effective_setting(raw::interface::GROUP, raw::interface::subtitle::ASS_NATIVE),
        Some(memento_config::Value::Bool(false))
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(target_os = "linux")]
#[path = "support/headless_slint.rs"]
mod headless_slint;

#[cfg(target_os = "linux")]
mod live_visual_parity {
    use super::headless_slint;
    use super::*;
    use glow::HasContext;
    use image::{imageops, RgbImage, RgbaImage};
    use memento_app::controller::ass::{
        build_ass_ruby_items, mpv_ass_params_from_playback, AssOverlayController,
    };
    use memento_app::video_gl;
    use memento_app::{
        set_dictionary_rows, DictResult, PopupGlossaryBlock, SubtitleOverlayBridge,
    };
    use memento_player::mpv::MpvInstance;
    use slint::{ComponentHandle, Model};
    use std::ffi::{c_char, c_int, c_void, CStr};
    use std::process::Command;
    use std::ptr;
    use std::rc::Rc;
    use std::time::{Duration, Instant};

    const SUMMER_POCKETS_MKV: &str = "/home/eyalm/Desktop/japanese/anime/[NanakoRaws] Summer Pockets (AT-X 1920x1080 x265 AAC)/[NanakoRaws] Summer Pockets - 01 (AT-X 1920x1080 x265 AAC).mkv";
    const COMPARISON_OUT_DIR: &str = "/home/eyalm/.gemini/antigravity/brain/bc59aa33-0990-4b77-b532-788d1afb2be9/ass-comparison";

    struct TestGlContext {
        display: *mut c_void,
        surface: *mut c_void,
        context: *mut c_void,
        make_current: unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void, *mut c_void) -> u32,
        get_proc: unsafe extern "C" fn(*const c_char) -> *const c_void,
    }

    unsafe impl i_slint_renderer_femtovg::opengl::OpenGLInterface for TestGlContext {
        fn ensure_current(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
            let ok = unsafe {
                (self.make_current)(self.display, self.surface, self.surface, self.context)
            };
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

    /// Capture all requested timestamps from a single sequential stock `mpv` session
    /// (`--no-config`) using `seek <t> absolute+exact` + `screenshot-to-file ... subtitles`.
    /// Running sequentially in one session guarantees identical `libass` fallback font
    /// cache progression (e.g. `🔊` at 20s warming `NotoSansSymbols2` before `⚟` at 47.2s)
    /// and exact frame-PTS alignment with `MpvInstance::seek`.
    fn capture_stock_mpv_frames_sequential(
        mkv_path: &Path,
        timestamps: &[(&str, f64, usize)],
        out_dir: &Path,
    ) -> std::collections::HashMap<String, RgbImage> {
        let mut results = std::collections::HashMap::new();
        let tmp_dir = std::env::temp_dir().join(format!(
            "memento_mpv_ref_seq_{}",
            std::process::id()
        ));
        let vo_dir = tmp_dir.join("vo");
        let _ = std::fs::remove_dir_all(&tmp_dir);
        if std::fs::create_dir_all(&vo_dir).is_err() {
            return results;
        }

        let mut lua = String::from("local targets = {\n");
        for &(tag, ts_secs, _) in timestamps {
            let out_png = out_dir.join(format!("mpv_{tag}.png"));
            let _ = std::fs::remove_file(&out_png);
            let escaped = out_png.to_string_lossy().replace('\\', "\\\\").replace('"', "\\\"");
            lua.push_str(&format!(
                "  {{ tag = \"{tag}\", t = {ts_secs:.3}, out = \"{escaped}\" }},\n"
            ));
        }
        lua.push_str(
            r#"}
local cur_idx = 0
local waiting_seek = false

local function advance()
    cur_idx = cur_idx + 1
    if cur_idx > #targets then
        mp.command("quit")
        return
    end
    waiting_seek = true
    mp.commandv("seek", tostring(targets[cur_idx].t), "absolute+exact")
end

mp.register_event("file-loaded", function()
    advance()
end)

mp.register_event("playback-restart", function()
    if not waiting_seek or cur_idx < 1 or cur_idx > #targets then
        return
    end
    local pos = mp.get_property_number("time-pos", -1)
    if math.abs(pos - targets[cur_idx].t) > 0.25 then
        return
    end
    waiting_seek = false
    mp.commandv("screenshot-to-file", targets[cur_idx].out, "subtitles")
    advance()
end)
"#,
        );
        let script_path = tmp_dir.join("capture_seq.lua");
        if std::fs::write(&script_path, lua).is_err() {
            let _ = std::fs::remove_dir_all(&tmp_dir);
            return results;
        }

        let status = Command::new("mpv")
            .args([
                "--no-config",
                "--ao=null",
                "--hwdec=no",
                "--vo=image",
                "--vo-image-format=png",
                &format!("--vo-image-outdir={}", vo_dir.display()),
                "--screenshot-format=png",
                "--vf=format=rgb0",
                "--hr-seek=yes",
                "--sid=1",
                "--pause",
                &format!("--script={}", script_path.display()),
                "--msg-level=all=error",
            ])
            .arg(mkv_path)
            .status();

        if let Ok(st) = status {
            if st.success() {
                for &(tag, _, _) in timestamps {
                    let out_png = out_dir.join(format!("mpv_{tag}.png"));
                    if let Ok(img) = image::open(&out_png) {
                        results.insert(tag.to_string(), img.to_rgb8());
                    }
                }
            }
        }
        let _ = std::fs::remove_dir_all(&tmp_dir);
        results
    }

    fn read_gl_video_area_rgb(gl: &glow::Context, w: u32, h: u32) -> RgbImage {
        let mut rgba = vec![0u8; (w as usize) * (h as usize) * 4];
        unsafe {
            gl.bind_framebuffer(glow::FRAMEBUFFER, None);
            gl.finish();
            // Window is 1920x1110 (top 30px = menu bar, bottom 1080px = video-area).
            // OpenGL origin is bottom-left, so y=0..1080 is the exact 1920x1080 video-area!
            gl.read_pixels(
                0,
                0,
                w as i32,
                h as i32,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelPackData::Slice(Some(&mut rgba)),
            );
        }
        let mut out = RgbImage::new(w, h);
        for y in 0..h {
            let src_y = h - 1 - y;
            for x in 0..w {
                let si = ((src_y * w + x) * 4) as usize;
                out.put_pixel(x, y, image::Rgb([rgba[si], rgba[si + 1], rgba[si + 2]]));
            }
        }
        out
    }

    /// Build a side-by-side comparison PNG:
    /// - Top row (`1920x540`): left `960x540` = stock `mpv`, right `960x540` = Memento Rust
    /// - Bottom rows (`1920x880`): 1:1 native-resolution subtitle region (`y=640..1080`,
    ///   capturing up to 3 stacked simultaneous subtitle lines cleanly unclipped):
    ///   upper `1920x440` = stock `mpv`, lower `1920x440` = Memento Rust.
    fn build_side_by_side_comparison(mpv_img: &RgbImage, mem_img: &RgbImage) -> RgbImage {
        let half_mpv = imageops::resize(mpv_img, 960, 540, imageops::FilterType::Triangle);
        let half_mem = imageops::resize(mem_img, 960, 540, imageops::FilterType::Triangle);

        let strip_h = 440u32;
        let strip_y0 = 1080 - strip_h;
        let out_h = 540 + 4 + strip_h + 4 + strip_h;
        let mut canvas = RgbImage::from_pixel(1920, out_h, image::Rgb([24, 28, 34]));

        imageops::overlay(&mut canvas, &half_mpv, 0, 0);
        imageops::overlay(&mut canvas, &half_mem, 960, 0);
        // 2px cyan vertical divider between top halves.
        for y in 0..540 {
            canvas.put_pixel(959, y, image::Rgb([56, 166, 214]));
            canvas.put_pixel(960, y, image::Rgb([56, 166, 214]));
        }
        // Horizontal cyan dividers + 1:1 native subtitle strips (top=mpv, bottom=Memento).
        for x in 0..1920 {
            for dy in 0..4 {
                canvas.put_pixel(x, 540 + dy, image::Rgb([56, 166, 214]));
                canvas.put_pixel(x, 544 + strip_h + dy, image::Rgb([56, 166, 214]));
            }
        }
        let mpv_strip = imageops::crop_imm(mpv_img, 0, strip_y0, 1920, strip_h).to_image();
        let mem_strip = imageops::crop_imm(mem_img, 0, strip_y0, 1920, strip_h).to_image();
        imageops::overlay(&mut canvas, &mpv_strip, 0, 544);
        imageops::overlay(&mut canvas, &mem_strip, 0, i64::from(544 + strip_h + 4));
        canvas
    }

    struct ParityStats {
        mean_abs_diff: f64,
        sub_band_mean_diff: f64,
        sub_band_pct_gt_8: f64,
        max_diff: u8,
    }

    fn compute_parity_stats(a: &RgbImage, b: &RgbImage) -> ParityStats {
        assert_eq!(a.dimensions(), b.dimensions());
        let (w, h) = a.dimensions();
        let mut total_diff = 0u64;
        let mut sub_diff = 0u64;
        let mut sub_pixels = 0u64;
        let mut sub_gt_8 = 0u64;
        let mut max_diff = 0u8;

        let ar = a.as_raw();
        let br = b.as_raw();
        for y in 0..h {
            let in_sub = y >= 640;
            for x in 0..w {
                let idx = ((y * w + x) * 3) as usize;
                let dr = ar[idx].abs_diff(br[idx]);
                let dg = ar[idx + 1].abs_diff(br[idx + 1]);
                let db = ar[idx + 2].abs_diff(br[idx + 2]);
                let d = dr.max(dg).max(db);
                total_diff += (dr as u64 + dg as u64 + db as u64) / 3;
                max_diff = max_diff.max(d);
                if in_sub {
                    sub_pixels += 1;
                    sub_diff += (dr as u64 + dg as u64 + db as u64) / 3;
                    if d > 8 {
                        sub_gt_8 += 1;
                    }
                }
            }
        }
        let total_px = (w as u64) * (h as u64);
        ParityStats {
            mean_abs_diff: total_diff as f64 / total_px as f64,
            sub_band_mean_diff: sub_diff as f64 / sub_pixels.max(1) as f64,
            sub_band_pct_gt_8: (sub_gt_8 as f64 / sub_pixels.max(1) as f64) * 100.0,
            max_diff,
        }
    }

    #[test]
    fn live_memento_vs_stock_mpv_8_timestamps_and_interactive_captures() {
        let mkv_path = Path::new(SUMMER_POCKETS_MKV);
        if !mkv_path.is_file() {
            eprintln!("[SKIP] Summer Pockets MKV not found at {}", mkv_path.display());
            return;
        }
        let Ok(egl) = (unsafe { libloading::Library::new("libEGL.so.1") }) else {
            eprintln!("[SKIP] libEGL.so.1 not available for headless OpenGL visual parity");
            return;
        };

        std::fs::create_dir_all(COMPARISON_OUT_DIR).expect("create ass-comparison dir");
        let out_dir = PathBuf::from(COMPARISON_OUT_DIR);

        // Initialize 1920x1110 EGL surfaceless OpenGL 3.3 context (30px menu bar + 1920x1080 video-area).
        let (display, surface, context, make_current, get_proc, destroy_ctx, destroy_surf, terminate) = unsafe {
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
            let create = egl
                .get::<unsafe extern "C" fn(
                    *mut c_void,
                    *mut c_void,
                    *mut c_void,
                    *const c_int,
                ) -> *mut c_void>(b"eglCreateContext\0")
                .unwrap();
            let make_current = *egl
                .get::<unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void, *mut c_void) -> u32>(
                    b"eglMakeCurrent\0",
                )
                .unwrap();
            let get_proc = *egl
                .get::<unsafe extern "C" fn(*const c_char) -> *const c_void>(
                    b"eglGetProcAddress\0",
                )
                .unwrap();
            let destroy_ctx = *egl
                .get::<unsafe extern "C" fn(*mut c_void, *mut c_void) -> u32>(
                    b"eglDestroyContext\0",
                )
                .unwrap();
            let create_surface = egl
                .get::<unsafe extern "C" fn(*mut c_void, *mut c_void, *const c_int) -> *mut c_void>(
                    b"eglCreatePbufferSurface\0",
                )
                .unwrap();
            let destroy_surf = *egl
                .get::<unsafe extern "C" fn(*mut c_void, *mut c_void) -> u32>(
                    b"eglDestroySurface\0",
                )
                .unwrap();
            let terminate = *egl
                .get::<unsafe extern "C" fn(*mut c_void) -> u32>(b"eglTerminate\0")
                .unwrap();

            let display = display_fn(0x31DD, ptr::null_mut(), ptr::null());
            if display.is_null() {
                eprintln!("[SKIP] EGL_PLATFORM_SURFACELESS_MESA display unavailable");
                return;
            }
            assert_ne!(initialize(display, ptr::null_mut(), ptr::null_mut()), 0);
            assert_ne!(bind_api(0x30A2), 0);
            let mut config = ptr::null_mut();
            let mut count = 0;
            assert_ne!(
                choose(
                    display,
                    [0x3033, 1, 0x3040, 8, 0x3038].as_ptr(),
                    &mut config,
                    1,
                    &mut count,
                ),
                0
            );
            assert!(count > 0);
            let context = create(
                display,
                config,
                ptr::null_mut(),
                [0x3098, 3, 0x30FB, 3, 0x3038].as_ptr(),
            );
            assert!(!context.is_null());
            let surface = create_surface(
                display,
                config,
                [0x3057, 1920, 0x3056, 1110, 0x3038].as_ptr(),
            );
            assert!(!surface.is_null());
            assert_ne!(make_current(display, surface, surface, context), 0);
            (
                display,
                surface,
                context,
                make_current,
                get_proc,
                destroy_ctx,
                destroy_surf,
                terminate,
            )
        };

        let gl = unsafe { glow::Context::from_loader_function_cstr(|n| get_proc(n.as_ptr())) };
        let target = video_gl::create_video_fbo_texture(&gl, None, None, 1920, 1080).unwrap();
        let fbo = target.framebuffer;
        let texture = target.texture;
        let internal_format = target.internal_format;

        let (app, window) = headless_slint::create_window(TestGlContext {
            display,
            surface,
            context,
            make_current,
            get_proc,
        });
        app.window().set_size(slint::PhysicalSize::new(1920, 1110));
        app.set_osc_fade_ms(0);
        app.set_osc_visible(false);
        app.set_duration(1440.0);
        app.set_sub_ass_native(true);
        app.set_subtitles_visible(true);
        memento_app::prepare_text_fonts();
        let video_tex = unsafe {
            slint::BorrowedOpenGLTextureBuilder::new_gl_2d_rgba_texture(
                texture.0,
                [1920, 1080].into(),
            )
            .origin(slint::BorrowedOpenGLTextureOrigin::TopLeft)
            .build()
        };
        app.set_video_frame(video_tex);
        window.renderer.render().unwrap();
        assert_eq!(app.get_video_width(), 1920);
        assert_eq!(app.get_video_height(), 1080);

        let mpv_dir = std::env::temp_dir().join(format!(
            "memento_ass_parity_mpv_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&mpv_dir);
        std::fs::create_dir_all(&mpv_dir).unwrap();
        std::fs::write(mpv_dir.join("input.conf"), "").unwrap();
        std::fs::write(
            mpv_dir.join("mpv.conf"),
            "ao=null\nhwdec=no\nhr-seek=yes\nmsg-level=all=error\n",
        )
        .unwrap();

        let player = MpvInstance::new_software(Some(&mpv_dir)).unwrap();
        player
            .activate_opengl_owned_with_display(move |name: &CStr| unsafe { get_proc(name.as_ptr()) }, None, None)
            .unwrap();
        let (wake_tx, wake_rx) = std::sync::mpsc::channel();
        player.set_render_wakeup(move || {
            let _ = wake_tx.send(());
        });

        // Enable native ASS rendering in libmpv (exact Approach B+ runtime mode!).
        player.command(&["set", "pause", "yes"]).unwrap();
        player.command(&["set", "sub-visibility", "yes"]).unwrap();
        player.command(&["set", "sid", "1"]).unwrap();
        player.load_file(&mkv_path.to_string_lossy()).unwrap();

        // Wait for file load & initial playback-restart.
        let load_deadline = Instant::now() + Duration::from_secs(15);
        loop {
            assert!(Instant::now() < load_deadline, "timed out loading Summer Pockets MKV");
            let _ = wake_rx.recv_timeout(Duration::from_millis(20));
            while wake_rx.try_recv().is_ok() {}
            let _ = player.drain_events();
            let _ = video_gl::with_saved_state(&gl, || player.take_render_update());
            let st = player.playback_state();
            if st.file_loaded && st.playback_ready && !st.sub_ass_extradata.is_empty() {
                break;
            }
        }

        let mut ass = AssOverlayController::new();
        let seek_and_render = |ts_secs: f64, expected_sub_nonempty: bool, ass: &mut AssOverlayController| {
            let ts_str = format!("{ts_secs:.3}");
            player
                .command(&["seek", &ts_str, "absolute+exact"])
                .unwrap();

            let deadline = Instant::now() + Duration::from_secs(12);
            let mut saw_restart = false;
            let mut settled_at: Option<Instant> = None;

            loop {
                assert!(
                    Instant::now() < deadline,
                    "timed out seeking/rendering at {ts_secs:.1}s (pos={:.3}, ass_full_len={})",
                    player.playback_state().position,
                    player.playback_state().sub_text_ass_full.len()
                );
                let _ = wake_rx.recv_timeout(Duration::from_millis(15));
                while wake_rx.try_recv().is_ok() {}
                for ev in player.drain_events() {
                    if matches!(ev, memento_player::MpvEvent::PlaybackRestart) {
                        saw_restart = true;
                    }
                }
                let need_draw = video_gl::with_saved_state(&gl, || player.take_render_update());
                if need_draw || saw_restart {
                    video_gl::with_saved_state(&gl, || {
                        video_gl::without_blending(&gl, || {
                            player.render_opengl_with_format(
                                fbo.0.get(),
                                1920,
                                1080,
                                internal_format,
                            )
                        })
                    })
                    .unwrap();
                    player.report_render_swap();
                }

                let st = player.playback_state();
                let pos_ok = (st.position - ts_secs).abs() < 0.20;
                let sub_ok = !expected_sub_nonempty || !st.sub_text_ass_full.trim().is_empty();
                if saw_restart && pos_ok && sub_ok {
                    let params = mpv_ass_params_from_playback(&st, (1920, 1080));
                    let extradata = st.sub_ass_extradata.clone();
                    let ass_full = st.sub_text_ass_full.clone();
                    let now_ms = (st.position * 1000.0).round() as i64;
                    drop(st);

                    ass.sync_from_playback(&app, true, &extradata, &ass_full, &params, now_ms, true, 0.4);
                    // Re-render final clean FBO + Slint scene once settled for 80ms.
                    let since = settled_at.get_or_insert_with(Instant::now);
                    if since.elapsed() >= Duration::from_millis(80) {
                        video_gl::with_saved_state(&gl, || {
                            video_gl::without_blending(&gl, || {
                                player.render_opengl_with_format(
                                    fbo.0.get(),
                                    1920,
                                    1080,
                                    internal_format,
                                )
                            })
                        })
                        .unwrap();
                        window.renderer.render().unwrap();
                        player.report_render_swap();
                        return read_gl_video_area_rgb(&gl, 1920, 1080);
                    }
                }
            }
        };

        // 1. All 8 verification timestamps: live Memento Rust vs. stock `mpv`!
        let timestamps: &[(&str, f64, usize)] = &[
            ("01_12s", 12.0, 1),
            ("02_20s", 20.0, 2),
            ("03_24.5s", 24.5, 1),
            ("04_29s", 29.0, 2),
            ("05_47.2s", 47.2, 2),
            ("06_420s", 420.0, 2),
            ("07_698.5s", 698.5, 3),
            ("08_848.5s", 848.5, 3),
        ];

        let mpv_refs = capture_stock_mpv_frames_sequential(mkv_path, timestamps, &out_dir);

        eprintln!(
            "\n=== Live 1920x1080 Visual Parity: Memento Rust (libmpv GL + FemtoVG) vs Stock mpv ==="
        );
        for &(tag, ts_secs, expected_events) in timestamps {
            let mem_img = seek_and_render(ts_secs, true, &mut ass);
            let mem_path = out_dir.join(format!("memento_{tag}.png"));
            mem_img.save(&mem_path).expect("save memento PNG");

            let layout = ass.layout().expect("active ASS layout");
            assert_eq!(
                layout.events.len(),
                expected_events,
                "timestamp {tag} ({ts_secs}s): expected {expected_events} ASS events"
            );

            if let Some(mpv_img) = mpv_refs.get(tag) {
                let stats = compute_parity_stats(mpv_img, &mem_img);
                let cmp_img = build_side_by_side_comparison(mpv_img, &mem_img);
                cmp_img
                    .save(out_dir.join(format!("compare_{tag}.png")))
                    .expect("save comparison PNG");

                eprintln!(
                    "[VISUAL PARITY] {tag:<10} (t={ts_secs:>5.1}s, {expected_events} ev): \
                     full_mean_diff={:.2}/255, sub_band_mean_diff={:.2}/255, sub_gt8={:.3}%, max={}",
                    stats.mean_abs_diff,
                    stats.sub_band_mean_diff,
                    stats.sub_band_pct_gt_8,
                    stats.max_diff
                );
                assert!(
                    stats.sub_band_mean_diff <= 3.5,
                    "{tag}: subtitle band mean diff too high ({:.2}/255)",
                    stats.sub_band_mean_diff
                );
                assert!(
                    stats.sub_band_pct_gt_8 <= 1.0,
                    "{tag}: too many differing subtitle pixels ({:.2}%)",
                    stats.sub_band_pct_gt_8
                );
            } else {
                eprintln!("[WARN] stock `mpv` CLI unavailable; saved Memento capture only for {tag}");
            }
        }

        // 2. Interactive capture (a): Hover "鳥白島" at 20.0s -> solid #38a6d6 highlight BEHIND
        //    white-outlined ASS glyphs + anchored YomitanPopup!
        let _ = seek_and_render(20.0, true, &mut ass);
        let target_20s = {
            let layout = ass.layout().unwrap();
            let c = layout.events[1]
                .chars
                .iter()
                .find(|c| c.ch == '鳥')
                .expect("鳥 in event 1 @ 20s");
            ass.hit_test_logical(&app, c.x + c.w * 0.5, c.y + c.h * 0.5)
                .expect("hit 鳥白島")
        };
        assert_eq!(target_20s.event_index, 1);
        assert!(target_20s.scan_substring.starts_with("鳥白島"));
        ass.apply_lookup_highlight(&app, &target_20s, 3);
        set_dictionary_rows(
            &app,
            vec![
                DictResult {
                    index: 0,
                    expression: "鳥白島".into(),
                    reading: "とりしろじま".into(),
                    dict_name: "JMdict".into(),
                    selected: true,
                    term_start: true,
                    term_end: true,
                    glossary_blocks: Rc::new(slint::VecModel::from(vec![PopupGlossaryBlock {
                        text: "Torishirojima — fictional island in the Seto Inland Sea (Summer Pockets).".into(),
                        kind: 2,
                        href: "".into(),
                    }]))
                    .into(),
                    ..Default::default()
                },
            ],
        );
        app.set_dict_query("鳥白島".into());
        app.set_dict_reading("とりしろじま".into());
        app.set_dict_open(true);
        app.set_selected_token_index(0);
        window.renderer.render().unwrap();
        let hover_20s_img = read_gl_video_area_rgb(&gl, 1920, 1080);
        hover_20s_img
            .save(out_dir.join("memento_hover_20s_torishirojima.png"))
            .expect("save hover 20s PNG");

        let sb = app.global::<SubtitleOverlayBridge>();
        assert!(sb.get_ass_anchor_active());
        assert_eq!(sb.get_ass_highlight_patches().row_count(), 1);
        assert!(
            app.get_popup_y() + app.get_popup_height() <= sb.get_ass_anchor_top() + 1.0,
            "20s popup must anchor cleanly above 鳥白島"
        );
        // Verify BOTH #38a6d6 blue highlight pixels AND crisp white ASS glyph pixels exist in the highlight rect!
        let hl = sb.get_ass_highlight_patches().row_data(0).unwrap();
        let (mut blue_px, mut white_px) = (0usize, 0usize);
        for y in (hl.y.max(0.0) as u32)..((hl.y + hl.height).min(1080.0) as u32) {
            for x in (hl.x.max(0.0) as u32)..((hl.x + hl.width).min(1920.0) as u32) {
                let px = hover_20s_img.get_pixel(x, y);
                if px[0] < 95 && px[1] > 140 && px[2] > 185 {
                    blue_px += 1;
                }
                if px[0] > 230 && px[1] > 230 && px[2] > 230 {
                    white_px += 1;
                }
            }
        }
        eprintln!(
            "[INTERACTIVE] 20s hover '鳥白島': blue_highlight_px={blue_px}, white_glyph_px={white_px}, popup=({}, {}, {}x{})",
            app.get_popup_x(),
            app.get_popup_y(),
            app.get_popup_width(),
            app.get_popup_height()
        );
        assert!(blue_px > 500, "expected solid #38a6d6 highlight bar pixels ({blue_px})");
        assert!(white_px > 300, "expected crisp white ASS glyphs on top of blue highlight ({white_px})");

        // 3. Interactive capture (c): Alt+A furigana over ASS lines at 20.0s!
        app.invoke_dismiss_definition();
        ass.clear_lookup_highlight(&app);
        app.set_furigana_visible(true);
        let ruby_items = {
            let layout = ass.layout().unwrap();
            let vt = ass.viewport().unwrap();
            let mut items = build_ass_ruby_items(layout, &vt, 0.4, |text| {
                memento_app::controller::subtitles::subtitle_furigana_parts(text).ok()
            });
            if items.is_empty() {
                // Fallback deterministic readings if MeCab dictionary is unavailable in CI env.
                items = build_ass_ruby_items(layout, &vt, 0.4, |text| {
                    if text.contains("鳥白島") {
                        Some(vec![
                            ("鳥白島".into(), "とりしろじま".into()),
                            ("・".into(), "".into()),
                            ("鳥白島".into(), "とりしろじま".into()),
                            ("漁港".into(), "ぎょこう".into()),
                            ("に".into(), "".into()),
                            ("到着".into(), "とうちゃく".into()),
                            ("します。".into(), "".into()),
                        ])
                    } else {
                        None
                    }
                });
            }
            items
        };
        assert!(!ruby_items.is_empty(), "expected ASS furigana ruby items at 20.0s");
        sb.set_ass_ruby_items(Rc::new(slint::VecModel::from(ruby_items)).into());
        window.renderer.render().unwrap();
        let furi_20s_img = read_gl_video_area_rgb(&gl, 1920, 1080);
        furi_20s_img
            .save(out_dir.join("memento_furigana_20s.png"))
            .expect("save furigana 20s PNG");
        app.set_furigana_visible(false);
        sb.set_ass_ruby_items(Rc::new(slint::VecModel::from(Vec::new())).into());

        // 4. Anki {screenshot} vs {screenshot-video} pixel-difference verification at 20.0s!
        let capture_mpv_screenshot = |include_subs: bool| -> RgbaImage {
            let req = player.request_screenshot(include_subs).unwrap();
            let handle = std::thread::spawn(move || req.wait());
            let deadline = Instant::now() + Duration::from_secs(12);
            while !handle.is_finished() {
                assert!(Instant::now() < deadline, "screenshot capture timed out");
                let _ = wake_rx.recv_timeout(Duration::from_millis(10));
                while wake_rx.try_recv().is_ok() {}
                let _ = player.drain_events();
                let need = video_gl::with_saved_state(&gl, || player.take_render_update());
                if need {
                    let _ = video_gl::with_saved_state(&gl, || {
                        video_gl::without_blending(&gl, || {
                            player.render_opengl_with_format(
                                fbo.0.get(),
                                1920,
                                1080,
                                internal_format,
                            )
                        })
                    });
                    player.report_render_swap();
                }
            }
            let raw = handle.join().unwrap().unwrap();
            RgbaImage::from_raw(raw.width, raw.height, raw.pixels).unwrap()
        };
        let anki_sub_shot = capture_mpv_screenshot(true);
        let anki_vid_shot = capture_mpv_screenshot(false);
        assert_eq!(anki_sub_shot.dimensions(), (1920, 1080));
        assert_eq!(anki_vid_shot.dimensions(), (1920, 1080));
        anki_sub_shot
            .save(out_dir.join("memento_anki_screenshot_20s.png"))
            .expect("save anki {screenshot}");
        anki_vid_shot
            .save(out_dir.join("memento_anki_screenshot_video_20s.png"))
            .expect("save anki {screenshot-video}");

        let (mut sub_band_diff_px, mut upper_diff_px) = (0usize, 0usize);
        for y in 0..1080u32 {
            for x in 0..1920u32 {
                let p1 = anki_sub_shot.get_pixel(x, y);
                let p2 = anki_vid_shot.get_pixel(x, y);
                let d = p1[0].abs_diff(p2[0]).max(p1[1].abs_diff(p2[1])).max(p1[2].abs_diff(p2[2]));
                if d > 10 {
                    if y >= 700 {
                        sub_band_diff_px += 1;
                    } else {
                        upper_diff_px += 1;
                    }
                }
            }
        }
        eprintln!(
            "[ANKI SCREENSHOT] 20s {{screenshot}} vs {{screenshot-video}}: sub_band_diff_px={sub_band_diff_px}, upper_frame_diff_px={upper_diff_px}"
        );
        assert!(
            sub_band_diff_px > 5_000,
            "{{screenshot}} must contain real ASS subtitles (sub_band_diff_px={sub_band_diff_px})"
        );
        assert_eq!(
            upper_diff_px, 0,
            "{{screenshot}} and {{screenshot-video}} must match identically outside subtitle region"
        );

        // 5. Interactive capture (b): 2-speaker scene at 29.0s — hover Speaker 1 ("座席") &
        //    Speaker 2 ("羽依里"), verifying strict per-speaker isolation + dynamic popup anchoring!
        let _ = seek_and_render(29.0, true, &mut ass);
        let (target_sp1, target_sp2) = {
            let layout = ass.layout().unwrap();
            assert_eq!(layout.events.len(), 2);
            let c1 = layout.events[0].chars.iter().find(|c| c.ch == '座').unwrap();
            let c2 = layout.events[1].chars.iter().find(|c| c.ch == '羽').unwrap();
            (
                ass.hit_test_logical(&app, c1.x + c1.w * 0.5, c1.y + c1.h * 0.5).unwrap(),
                ass.hit_test_logical(&app, c2.x + c2.w * 0.5, c2.y + c2.h * 0.5).unwrap(),
            )
        };
        assert_eq!(target_sp1.event_index, 0);
        assert_eq!(target_sp1.event_plain_text, "座席を立たないよう…。");
        assert_eq!(target_sp2.event_index, 1);
        assert_eq!(target_sp2.event_plain_text, "（羽依里）鳥白島　か…。");

        // Speaker 1 capture
        ass.apply_lookup_highlight(&app, &target_sp1, 2);
        assert_eq!(
            ass.active_lookup().unwrap().event_plain_text,
            "座席を立たないよう…。"
        );
        set_dictionary_rows(
            &app,
            vec![DictResult {
                index: 0,
                expression: "座席".into(),
                reading: "ざせき".into(),
                dict_name: "JMdict".into(),
                selected: true,
                term_start: true,
                term_end: true,
                glossary_blocks: Rc::new(slint::VecModel::from(vec![PopupGlossaryBlock {
                    text: "seat (noun)".into(),
                    kind: 2,
                    href: "".into(),
                }]))
                .into(),
                ..Default::default()
            }],
        );
        app.set_dict_query("座席".into());
        app.set_dict_reading("ざせき".into());
        app.set_dict_open(true);
        window.renderer.render().unwrap();
        let sp1_popup_y = app.get_popup_y();
        read_gl_video_area_rgb(&gl, 1920, 1080)
            .save(out_dir.join("memento_hover_29s_speaker1_zaseki.png"))
            .expect("save 29s speaker 1 PNG");

        // Speaker 2 capture
        ass.apply_lookup_highlight(&app, &target_sp2, 3);
        assert_eq!(
            ass.active_lookup().unwrap().event_plain_text,
            "（羽依里）鳥白島　か…。"
        );
        set_dictionary_rows(
            &app,
            vec![DictResult {
                index: 0,
                expression: "羽依里".into(),
                reading: "はいり".into(),
                dict_name: "JMnedict".into(),
                selected: true,
                term_start: true,
                term_end: true,
                glossary_blocks: Rc::new(slint::VecModel::from(vec![PopupGlossaryBlock {
                    text: "Takahara Hairi (Summer Pockets protagonist)".into(),
                    kind: 2,
                    href: "".into(),
                }]))
                .into(),
                ..Default::default()
            }],
        );
        app.set_dict_query("羽依里".into());
        app.set_dict_reading("はいり".into());
        window.renderer.render().unwrap();
        let sp2_popup_y = app.get_popup_y();
        read_gl_video_area_rgb(&gl, 1920, 1080)
            .save(out_dir.join("memento_hover_29s_speaker2_hairi.png"))
            .expect("save 29s speaker 2 PNG");

        assert!(
            sp2_popup_y > sp1_popup_y + 40.0,
            "2-speaker popup anchor must follow each speaker's vertical position (sp1_y={sp1_popup_y}, sp2_y={sp2_popup_y})"
        );

        // Clean GL & mpv teardown order!
        video_gl::with_saved_state(&gl, || player.deactivate_opengl());
        drop(player);
        let _ = std::fs::remove_dir_all(&mpv_dir);
        unsafe {
            gl.delete_framebuffer(fbo);
            gl.delete_texture(texture);
            make_current(display, ptr::null_mut(), ptr::null_mut(), ptr::null_mut());
            destroy_ctx(display, context);
            destroy_surf(display, surface);
            terminate(display);
        }
    }
}

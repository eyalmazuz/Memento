//! libmpv bindings for the movie player's renderer-backed handle and an
//! independent audio-only pronunciation handle.

pub mod ffi;

mod asr_stream;
mod asr_track;
mod audio_instance;
mod commands;
mod event;
mod extract;
mod hwdec;
mod instance;
mod node;
mod playback_state;
mod render;
mod screenshot;
mod types;
mod wakeup;

pub use asr_stream::{format_webvtt_cues, format_webvtt_timestamp};
pub use asr_track::AsrSubtitleTrack;
pub use audio_instance::AudioInstance;
pub use event::MpvEvent;
pub use ffi::mpv_handle;
pub use hwdec::{
    compatible_opengl_hwdec, compatible_opengl_hwdec_for_session, compatible_software_hwdec,
};
pub use instance::MpvInstance;
pub use playback_state::PlaybackState;
pub use render::{
    create_opengl_context_with_display, create_opengl_context_with_options, GlProcResolver,
    MpvSoftwareRenderer,
};
pub use screenshot::ScreenshotRequest;
pub use types::{
    AsrCue, AsrTrackMode, AudioTrack, CommandReplyResult, CursorAutohide, EncodeContext,
    EndFileReason, GlDisplayBackend, MpvPropertyName, MpvPropertyValue, OsdDimensions, RawFrame,
    SubLine, SubtitleTrack, TrackType, VideoTrack, ASR_STREAM_PROTOCOL, ASR_STREAM_URI,
};

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::ffi::{c_char, c_int, c_void, CStr, CString};
    use std::path::Path;
    use std::ptr;
    use std::sync::atomic::Ordering;
    use std::sync::{mpsc, Arc, RwLock};

    use super::asr_stream::{
        stream_cb_close, stream_cb_open_ro, stream_cb_read, stream_cb_seek, stream_cb_size,
    };
    use super::extract::{
        extract_all_tracks_from_node, extract_chapters_from_node, extract_string_list_from_node,
        extract_sub_lines_from_node, extract_tracks_from_node,
    };
    use super::node::{
        command_name, copy_mpv_string, cue_valid, id_valid, supports_memory_subtitles,
        MAX_TEXT_BYTES,
    };
    use super::playback_state::PLAYBACK_PROPERTIES;
    use super::render::resolve_gl_proc;
    use super::screenshot::{get_sub_visibility, set_sub_visibility, SubVisibilityGuard};
    use super::wakeup::RenderNotification;
    use super::*;
    use crate::controller::CliOption;

    #[test]
    fn hybrid_gpu_decoder_uses_copyback_without_overriding_other_choices() {
        assert_eq!(compatible_opengl_hwdec("nvdec", "Mesa Intel UHD 630"), "nvdec-copy");
        assert_eq!(compatible_opengl_hwdec("nvdec,vaapi", "AMD Radeon"), "nvdec-copy,vaapi");
        assert_eq!(compatible_opengl_hwdec("cuda,vaapi", "AMD Radeon"), "cuda-copy,vaapi");
        assert_eq!(compatible_opengl_hwdec("nvdec", "NVIDIA GeForce"), "nvdec");
        for choice in ["no", "auto", "auto-copy-safe", "nvdec-copy", "vaapi"] {
            assert_eq!(compatible_opengl_hwdec(choice, "Mesa Intel UHD 630"), choice);
        }
    }

    #[test]
    fn nvidia_shared_gl_and_wayland_avoid_direct_cuda_interop_hazards() {
        assert_eq!(
            compatible_opengl_hwdec_for_session("auto-safe", "NVIDIA GeForce RTX 4080", false),
            "nvdec-copy,auto-copy-safe"
        );
        assert_eq!(
            compatible_opengl_hwdec_for_session("nvdec", "NVIDIA GeForce RTX 4080", true),
            "nvdec-copy"
        );
        assert_eq!(
            compatible_opengl_hwdec_for_session("auto", "NVIDIA GeForce RTX 4080", true),
            "nvdec-copy,auto-copy"
        );
        assert_eq!(
            compatible_opengl_hwdec_for_session("yes", "NVIDIA GeForce RTX 4080", true),
            "nvdec-copy,auto-copy"
        );
        assert_eq!(
            compatible_opengl_hwdec_for_session("cuda,auto-safe", "NVIDIA GeForce RTX 4080", true),
            "cuda-copy,nvdec-copy,auto-copy-safe"
        );
        assert_eq!(
            compatible_opengl_hwdec_for_session("auto-safe", "llvmpipe (LLVM 18.1.8)", false),
            "auto-copy-safe"
        );
        assert_eq!(
            compatible_software_hwdec("nvdec,vaapi,auto-safe"),
            "nvdec-copy,vaapi-copy,auto-copy-safe"
        );
    }

    #[test]
    fn render_wakeup_coalesces_and_rearms_after_consumption() {
        let notification = Arc::new(RenderNotification::default());
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let observed = calls.clone();
        assert!(notification
            .wake
            .set(Box::new(move || {
                observed.fetch_add(1, Ordering::Relaxed);
            }))
            .is_ok());
        let source = notification.clone();
        std::thread::spawn(move || {
            for _ in 0..100 {
                source.notify();
            }
        })
        .join()
        .unwrap();
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        assert!(notification.take().is_some());
        assert!(notification.take().is_none());
        notification.notify();
        assert_eq!(calls.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn playback_events_own_text_and_retain_transient_numeric_values() {
        let mut state = PlaybackState {
            position: 42.5,
            duration: 120.0,
            volume: 85.0,
            ..Default::default()
        };
        let mut text = CString::new(" subtitle ").unwrap();
        let mut pointer = text.as_ptr();
        let mut event = ffi::mpv_event_property {
            name: c"sub-text".as_ptr(),
            format: ffi::MPV_FORMAT_STRING,
            data: (&mut pointer as *mut *const c_char).cast(),
        };
        unsafe { state.update(&event) };
        text = CString::new("reused event storage").unwrap();
        assert_eq!(state.sub_text, "subtitle");
        assert_eq!(text.to_bytes(), b"reused event storage");
        event.format = ffi::MPV_FORMAT_NONE;
        event.data = ptr::null_mut();
        unsafe { state.update(&event) };
        assert!(state.sub_text.is_empty());
        event.name = c"sub-speed".as_ptr();
        unsafe { state.update(&event) };
        assert_eq!(state.sub_speed, 1.0);
        // Momentary MPV_FORMAT_NONE on time-pos / duration / volume must not clobber cached values.
        for name in [c"time-pos", c"duration", c"volume"] {
            event.name = name.as_ptr();
            unsafe { state.update(&event) };
        }
        assert_eq!(state.position, 42.5);
        assert_eq!(state.duration, 120.0);
        assert_eq!(state.volume, 85.0);
    }

    #[test]
    fn cue_and_id_bounds_validation() {
        assert!(cue_valid(-0.0, 0.001, "hello"));
        assert!(cue_valid(0.0, f64::from(i32::MAX) / 1000.0, ""));
        for (start, end) in [
            (f64::NAN, 1.0),
            (0.0, f64::INFINITY),
            (f64::NEG_INFINITY, 1.0),
            (-0.001, 1.0),
            (0.0, 0.0),
            (1.0, 1.0),
            (2.0, 1.0),
            (0.0, (f64::from(i32::MAX) / 1000.0) + 0.001),
        ] {
            assert!(!cue_valid(start, end, "hello"));
        }
        assert!(!cue_valid(0.0, 1.0, "hi\0there"));
        let at_limit = "x".repeat(MAX_TEXT_BYTES);
        assert!(cue_valid(0.0, 1.0, &at_limit));
        let over_limit = "x".repeat(MAX_TEXT_BYTES + 1);
        assert!(!cue_valid(0.0, 1.0, &over_limit));

        for id in [i64::MIN, -1, 0, i64::from(i32::MAX) + 1, i64::MAX] {
            assert!(!id_valid(id));
        }
        for id in [1, i64::from(i32::MAX)] {
            assert!(id_valid(id));
        }
        assert!(!unsafe { supports_memory_subtitles(ptr::null_mut()) });
    }

    #[test]
    fn command_name_requires_a_string_in_the_name_map_entry() {
        let mut name: ffi::mpv_node = unsafe { std::mem::zeroed() };
        name.format = ffi::MPV_FORMAT_STRING;
        name.u.string = c"sub-create".as_ptr().cast_mut();
        let mut key = c"name".as_ptr().cast_mut();
        let mut fields = ffi::mpv_node_list {
            num: 1,
            values: &mut name,
            keys: &mut key,
        };
        let mut entry: ffi::mpv_node = unsafe { std::mem::zeroed() };
        entry.format = ffi::MPV_FORMAT_NODE_MAP;
        entry.u.list = &mut fields;
        assert_eq!(unsafe { command_name(&entry) }, Some(&b"sub-create"[..]));
    }

    #[test]
    fn synthetic_track_chapter_and_sub_line_extraction() {
        assert!(extract_tracks_from_node(ptr::null()).2.is_empty());
        assert!(extract_chapters_from_node(ptr::null()).is_empty());
        assert!(extract_sub_lines_from_node(ptr::null()).is_empty());

        let mut keys = [
            c"id".as_ptr().cast_mut(),
            c"type".as_ptr().cast_mut(),
            c"title".as_ptr().cast_mut(),
            c"lang".as_ptr().cast_mut(),
            c"selected".as_ptr().cast_mut(),
        ];
        let mut values = [
            ffi::mpv_node {
                format: ffi::MPV_FORMAT_INT64,
                u: ffi::mpv_node__bindgen_ty_1 { int64: 1 },
            },
            ffi::mpv_node {
                format: ffi::MPV_FORMAT_STRING,
                u: ffi::mpv_node__bindgen_ty_1 {
                    string: c"audio".as_ptr().cast_mut(),
                },
            },
            ffi::mpv_node {
                format: ffi::MPV_FORMAT_STRING,
                u: ffi::mpv_node__bindgen_ty_1 {
                    string: c"Stereo".as_ptr().cast_mut(),
                },
            },
            ffi::mpv_node {
                format: ffi::MPV_FORMAT_STRING,
                u: ffi::mpv_node__bindgen_ty_1 {
                    string: c"ja".as_ptr().cast_mut(),
                },
            },
            ffi::mpv_node {
                format: ffi::MPV_FORMAT_FLAG,
                u: ffi::mpv_node__bindgen_ty_1 { flag: 1 },
            },
        ];
        let mut map = ffi::mpv_node_list {
            num: keys.len() as c_int,
            keys: keys.as_mut_ptr(),
            values: values.as_mut_ptr(),
        };
        let mut entry = ffi::mpv_node {
            format: ffi::MPV_FORMAT_NODE_MAP,
            u: ffi::mpv_node__bindgen_ty_1 { list: &mut map },
        };
        let mut root_list = ffi::mpv_node_list {
            num: 1,
            keys: ptr::null_mut(),
            values: &mut entry,
        };
        let root = ffi::mpv_node {
            format: ffi::MPV_FORMAT_NODE_ARRAY,
            u: ffi::mpv_node__bindgen_ty_1 {
                list: &mut root_list,
            },
        };
        let (audio, subtitle, all) = extract_tracks_from_node(&root);
        assert_eq!(audio.len(), 1);
        assert!(subtitle.is_empty());
        assert_eq!(all.len(), 1);
        assert_eq!(audio[0].title, "Stereo");
        assert_eq!(audio[0].language, "ja");
        assert!(audio[0].selected);
    }

    #[test]
    fn webvtt_formats_sorted_cues_without_bom_and_sanitizes_blank_lines() {
        let cues = vec![
            AsrCue {
                id: 2,
                start: 12.5,
                end: 15.0,
                text: "Second\r\n\r\nLine".into(),
            },
            AsrCue {
                id: 1,
                start: 1.2345,
                end: 3.0,
                text: "First\0 cue".into(),
            },
        ];
        let vtt = String::from_utf8(format_webvtt_cues(&cues)).unwrap();
        assert!(!vtt.starts_with('\u{feff}'));
        assert_eq!(
            vtt,
            "WEBVTT\n\n\
             00:00:01.235 --> 00:00:03.000\n\
             First cue\n\n\
             00:00:12.500 --> 00:00:15.000\n\
             Second\nLine\n\n"
        );
        assert_eq!(format_webvtt_timestamp(3661.042), "01:01:01.042");
    }

    #[test]
    fn stream_cb_callbacks_enforce_bounds_and_catch_panics() {
        let shared: Arc<RwLock<Arc<[u8]>>> = Arc::new(RwLock::new(Arc::from(
            b"WEBVTT\n\n00:00:01.000 --> 00:00:02.000\nhi\n\n".as_slice(),
        )));
        let raw = Arc::into_raw(shared.clone());

        let mut info: ffi::mpv_stream_cb_info = unsafe { std::mem::zeroed() };
        let bad_uri = CString::new("http://example.com").unwrap();
        assert_eq!(
            unsafe {
                stream_cb_open_ro(
                    raw.cast_mut().cast(),
                    bad_uri.as_ptr().cast_mut(),
                    &mut info,
                )
            },
            ffi::MPV_ERROR_LOADING_FAILED
        );

        let good_uri = CString::new(ASR_STREAM_URI).unwrap();
        assert_eq!(
            unsafe {
                stream_cb_open_ro(
                    raw.cast_mut().cast(),
                    good_uri.as_ptr().cast_mut(),
                    &mut info,
                )
            },
            0
        );
        let cookie = info.cookie;
        assert!(!cookie.is_null());

        let total = unsafe { stream_cb_size(cookie) };
        assert!(total > 8);
        assert_eq!(unsafe { stream_cb_seek(cookie, 0) }, 0);
        assert!(unsafe { stream_cb_seek(cookie, -1) } < 0);
        assert!(unsafe { stream_cb_seek(cookie, total + 1) } < 0);

        let mut buf = vec![0u8; 6];
        assert_eq!(
            unsafe { stream_cb_read(cookie, buf.as_mut_ptr().cast(), 6) },
            6
        );
        assert_eq!(&buf, b"WEBVTT");
        assert_eq!(unsafe { stream_cb_seek(cookie, total) }, total);
        assert_eq!(
            unsafe { stream_cb_read(cookie, buf.as_mut_ptr().cast(), 6) },
            0
        );

        unsafe { stream_cb_close(cookie) };
        drop(unsafe { Arc::from_raw(raw) });
    }

    #[test]
    fn gl_proc_resolver_via_ctx_clears_scoped_and_catches_panics() {
        let called = Cell::new(false);
        let resolver_fn = |name: &CStr| -> *const c_void {
            called.set(true);
            if name.to_bytes() == b"panic_me" {
                panic!("boom inside GL resolver");
            }
            if name.to_bytes() == b"glGetString" {
                0x1234usize as *const c_void
            } else {
                ptr::null()
            }
        };
        let boxed = Box::into_raw(Box::new(GlProcResolver::scoped(&resolver_fn)));
        assert_eq!(
            unsafe { resolve_gl_proc(boxed.cast(), c"glGetString".as_ptr()) },
            0x1234usize as *mut c_void
        );
        assert!(called.get());
        assert_eq!(
            unsafe { resolve_gl_proc(boxed.cast(), c"panic_me".as_ptr()) },
            ptr::null_mut()
        );
        unsafe { &*boxed }.clear_scoped();
        assert_eq!(
            unsafe { resolve_gl_proc(boxed.cast(), c"glGetString".as_ptr()) },
            ptr::null_mut()
        );
        drop(unsafe { Box::from_raw(boxed) });
    }

    #[test]
    fn pause_unavailable_format_none_does_not_force_paused_true() {
        let mut state = PlaybackState {
            paused: false,
            ..Default::default()
        };
        let event = ffi::mpv_event_property {
            name: c"pause".as_ptr(),
            format: ffi::MPV_FORMAT_NONE,
            data: ptr::null_mut(),
        };
        unsafe { state.update(&event) };
        assert!(!state.paused);

        let mut flag: c_int = 1;
        let event_yes = ffi::mpv_event_property {
            name: c"pause".as_ptr(),
            format: ffi::MPV_FORMAT_FLAG,
            data: (&mut flag as *mut c_int).cast(),
        };
        unsafe { state.update(&event_yes) };
        assert!(state.paused);
    }

    #[test]
    fn mode2_asr_track_state_machine_buffers_and_cleans_up_orphans() {
        let player = MpvInstance::new_software(None).unwrap();
        let mut asr = AsrSubtitleTrack::with_mode(AsrTrackMode::NativeMemory);
        asr.pending_create_req = Some(99);
        asr.pending_cues.push_back(AsrCue {
            id: 1,
            start: 1.0,
            end: 2.0,
            text: "queued".into(),
        });
        assert!(asr.is_active());
        assert!(asr.sid().is_none());

        let err = asr
            .on_event(
                &player,
                &MpvEvent::CommandReply {
                    reply_userdata: 99,
                    error: -1,
                    result: CommandReplyResult::None,
                },
            )
            .unwrap_err();
        assert!(err.contains("creation failed"), "{err}");
        assert!(!asr.is_active());

        // Calling remove_track while pending_create_req is in flight must track and
        // ignore/clean up the late reply instead of leaving an orphan.
        asr.pending_create_req = Some(101);
        asr.remove_track(&player).unwrap();
        assert!(!asr.is_active());
        let res = asr
            .on_event(
                &player,
                &MpvEvent::CommandReply {
                    reply_userdata: 101,
                    error: 0,
                    result: CommandReplyResult::Int64(7),
                },
            )
            .unwrap();
        assert_eq!(res, None);
        assert_eq!(asr.sid(), None);
    }

    #[test]
    fn mode1_asr_track_defers_reload_until_create_completes_and_resets_on_shutdown() {
        let player = MpvInstance::new_software(None).unwrap();
        let mut asr = AsrSubtitleTrack::with_mode(AsrTrackMode::StreamCallback);
        asr.pending_create_req = Some(42);
        asr.sid = Some(3);

        // Adding a cue while `sub-add` (`pending_create_req`) is still in flight
        // must queue `reload_dirty` instead of firing `sub-reload` early.
        asr.add_cue(
            &player,
            AsrCue {
                id: 1,
                start: 1.0,
                end: 2.0,
                text: "hello".into(),
            },
        )
        .unwrap();
        assert!(asr.reload_dirty);
        assert!(asr.reload_in_flight.is_none());

        // Shutdown clears local track state so late ASR updates cannot reload.
        asr.on_event(&player, &MpvEvent::Shutdown).unwrap();
        assert!(!asr.is_active());
        assert_eq!(asr.sid(), None);
        assert!(asr.reload_in_flight.is_none());
    }

    #[test]
    fn cli_option_helpers_preserve_copy_mpv_string() {
        let dir = std::env::temp_dir().join(format!("memento-cli-opt-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("input.conf"), "").unwrap();
        std::fs::write(dir.join("mpv.conf"), "ao=null\nhwdec=no\n").unwrap();
        let player = MpvInstance::new_with_options(
            Some(Path::new(&dir)),
            &[CliOption {
                name: "hwdec".into(),
                value: "vaapi".into(),
            }],
        )
        .unwrap();
        assert_eq!(player.configured_hwdec(), "vaapi");
        let raw = unsafe { ffi::mpv_get_property_string(player.raw_handle(), c"hwdec".as_ptr()) };
        let val = copy_mpv_string(raw);
        if !raw.is_null() {
            unsafe { ffi::mpv_free(raw.cast()) };
        }
        assert_eq!(val, "vaapi");
        let (tx, _rx) = mpsc::channel();
        player.set_wakeup_callback(move || {
            let _ = tx.send(());
        });
        drop(player);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sub_visibility_raii_guard_restores_on_ok_err_and_panic_and_skips_video_only() {
        // 1. subtitles == true, initial == false: enables, then restores on drop (normal, error, panic).
        let state = Cell::new(false);
        let events = std::cell::RefCell::new(Vec::<&'static str>::new());
        {
            let _guard = SubVisibilityGuard::acquire(
                true,
                || {
                    events.borrow_mut().push("get");
                    Some(state.get())
                },
                || {
                    events.borrow_mut().push("enable");
                    state.set(true);
                    true
                },
                |orig| {
                    events.borrow_mut().push("restore");
                    state.set(orig);
                },
            );
            assert!(state.get(), "sub-visibility must be temporarily enabled");
        }
        assert!(!state.get(), "sub-visibility must be restored on normal drop");
        assert_eq!(*events.borrow(), vec!["get", "enable", "restore"]);

        // Error early-return path
        let res: Result<(), &str> = (|| {
            let _guard = SubVisibilityGuard::acquire(
                true,
                || Some(state.get()),
                || {
                    state.set(true);
                    true
                },
                |orig| state.set(orig),
            );
            assert!(state.get());
            Err("capture failed")
        })();
        assert!(res.is_err());
        assert!(!state.get(), "sub-visibility must be restored on error");

        // Panic unwind path
        let state_arc = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let state_for_panic = state_arc.clone();
        let panic_res = std::panic::catch_unwind(move || {
            let _guard = SubVisibilityGuard::acquire(
                true,
                || Some(state_for_panic.load(Ordering::SeqCst)),
                || {
                    state_for_panic.store(true, Ordering::SeqCst);
                    true
                },
                |orig| state_for_panic.store(orig, Ordering::SeqCst),
            );
            assert!(state_for_panic.load(Ordering::SeqCst));
            panic!("simulated screenshot panic");
        });
        assert!(panic_res.is_err());
        assert!(
            !state_arc.load(Ordering::SeqCst),
            "sub-visibility must be restored on panic unwind"
        );

        // 2. subtitles == true, initial == true: must NOT call enable or restore.
        let called = Cell::new(0usize);
        {
            let _guard = SubVisibilityGuard::acquire(
                true,
                || Some(true),
                || {
                    called.set(called.get() + 1);
                    true
                },
                |_| called.set(called.get() + 10),
            );
        }
        assert_eq!(called.get(), 0);

        // 3. subtitles == false (video-only & OCR): must NOT touch sub-visibility at all.
        let touched = Cell::new(0usize);
        {
            let _guard = SubVisibilityGuard::acquire(
                false,
                || {
                    touched.set(touched.get() + 1);
                    Some(false)
                },
                || {
                    touched.set(touched.get() + 10);
                    true
                },
                |_| touched.set(touched.get() + 100),
            );
        }
        assert_eq!(touched.get(), 0);
    }

    #[test]
    fn mpv_screenshot_temporarily_toggles_sub_visibility_only_when_subtitles_requested() {
        let player = MpvInstance::new_software(None).unwrap();
        let wait_for_sub_vis_event = |expected: Option<bool>| -> Vec<bool> {
            let mut seen = Vec::new();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
            while std::time::Instant::now() < deadline {
                for ev in player.drain_events() {
                    if let MpvEvent::PropertyChange {
                        name,
                        value: MpvPropertyValue::Flag(v),
                    } = ev
                    {
                        if name == "sub-visibility" {
                            seen.push(v);
                        }
                    }
                }
                if match expected {
                    Some(want) => seen.last() == Some(&want),
                    None => !seen.is_empty(),
                } {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            seen
        };

        // Drain the initial observed sub-visibility=false notification.
        let initial = wait_for_sub_vis_event(Some(false));
        assert_eq!(initial.last(), Some(&false));

        // Default MpvInstance initializes sub-visibility = no.
        assert_eq!(
            unsafe { get_sub_visibility(player.raw_handle()) },
            Some(false)
        );

        // 1. Video-only screenshot (subtitles == false) must NEVER touch sub-visibility.
        let video_res = player.request_screenshot(false).unwrap().wait();
        assert!(
            video_res.is_err(),
            "no video loaded -> screenshot should error"
        );
        std::thread::sleep(std::time::Duration::from_millis(30));
        let video_events: Vec<bool> = player
            .drain_events()
            .into_iter()
            .filter_map(|ev| match ev {
                MpvEvent::PropertyChange {
                    name,
                    value: MpvPropertyValue::Flag(v),
                } if name == "sub-visibility" => Some(v),
                _ => None,
            })
            .collect();
        assert!(
            video_events.is_empty(),
            "video-only screenshot must never touch sub-visibility, got {video_events:?}"
        );
        assert_eq!(
            unsafe { get_sub_visibility(player.raw_handle()) },
            Some(false)
        );

        // 2. Subtitle screenshot (subtitles == true) when sub-visibility == false:
        //    must temporarily enable sub-visibility and unconditionally restore to false on error.
        //    (libmpv may coalesce the rapid true -> false change into a single event with final value false.)
        let sub_res = player.request_screenshot(true).unwrap().wait();
        assert!(
            sub_res.is_err(),
            "no video loaded -> screenshot should error"
        );
        // Immediately after wait() returns, sub-visibility must already be restored to false.
        assert_eq!(
            unsafe { get_sub_visibility(player.raw_handle()) },
            Some(false),
            "sub-visibility must be restored immediately after subtitle screenshot error"
        );
        let sub_vis_transitions = wait_for_sub_vis_event(Some(false));
        assert!(
            !sub_vis_transitions.is_empty() && sub_vis_transitions.last() == Some(&false),
            "expected sub-visibility property notification ending in false, got {sub_vis_transitions:?}"
        );

        // 3. Synchronous software screenshot_raw_rgba(true) also restores sub-visibility on error.
        assert!(player.screenshot_raw_rgba(true).is_err());
        assert_eq!(
            unsafe { get_sub_visibility(player.raw_handle()) },
            Some(false)
        );

        // 4. When sub-visibility is already yes, subtitle screenshot leaves it yes without toggling.
        assert!(unsafe { set_sub_visibility(player.raw_handle(), true) });
        let _ = wait_for_sub_vis_event(Some(true));
        assert!(player.request_screenshot(true).unwrap().wait().is_err());
        assert_eq!(
            unsafe { get_sub_visibility(player.raw_handle()) },
            Some(true)
        );
        std::thread::sleep(std::time::Duration::from_millis(30));
        let unchanged_events: Vec<bool> = player
            .drain_events()
            .into_iter()
            .filter_map(|ev| match ev {
                MpvEvent::PropertyChange {
                    name,
                    value: MpvPropertyValue::Flag(v),
                } if name == "sub-visibility" => Some(v),
                _ => None,
            })
            .collect();
        assert!(
            unchanged_events.is_empty(),
            "already-visible sub-visibility should not be toggled, got {unchanged_events:?}"
        );
    }

    #[test]
    fn encode_context_effective_media_path_and_external_audio_resolution() {
        // Default produces clean zero/empty values.
        let def = EncodeContext::default();
        assert!(def.path.is_empty());
        assert_eq!(def.audio_id, 0);
        assert!(def.stream_open_filename.is_empty());
        assert!(def.http_header_fields.is_empty());
        assert!(!def.cookies);
        assert_eq!(def.external_audio_filename, None);

        // Prefers stream_open_filename over path when non-empty and not memento-asr://.
        let yt = EncodeContext {
            path: "https://www.youtube.com/watch?v=dQw4w9WgXcQ".into(),
            stream_open_filename: "edl://%20https://rr1---sn.googlevideo.com/videoplayback".into(),
            ..Default::default()
        };
        assert_eq!(
            yt.effective_media_path(),
            "edl://%20https://rr1---sn.googlevideo.com/videoplayback"
        );

        // Empty or whitespace-only stream_open_filename falls back to path.
        let local = EncodeContext {
            path: "/media/anime/ep01.mkv".into(),
            stream_open_filename: String::new(),
            ..Default::default()
        };
        assert_eq!(local.effective_media_path(), "/media/anime/ep01.mkv");
        assert_eq!(
            EncodeContext::resolve_media_path("   ", "/media/anime/ep01.mkv"),
            "/media/anime/ep01.mkv"
        );

        // Internal memento-asr:// URIs (any casing) must fall back to path.
        let asr = EncodeContext {
            path: "/media/anime/ep01.mkv".into(),
            stream_open_filename: ASR_STREAM_URI.into(),
            ..Default::default()
        };
        assert_eq!(asr.effective_media_path(), "/media/anime/ep01.mkv");
        assert_eq!(
            EncodeContext::resolve_media_path("MEMENTO-ASR://custom.vtt", "/media/ep02.mkv"),
            "/media/ep02.mkv"
        );

        // External audio selection for split DASH / edl:// streams.
        let tracks = vec![
            AudioTrack {
                id: 1,
                language: "ja".into(),
                title: "Embedded AAC".into(),
                external_filename: String::new(),
                external: false,
                default_track: true,
                selected: false,
            },
            AudioTrack {
                id: 2,
                language: "ja".into(),
                title: "YouTube DASH Audio".into(),
                external_filename: "https://rr1---sn.googlevideo.com/videoplayback?mime=audio"
                    .into(),
                external: true,
                default_track: false,
                selected: true,
            },
            AudioTrack {
                id: 3,
                language: "en".into(),
                title: "External EDL Audio".into(),
                external_filename: "edl://%12https://cdn.example/audio.m4a".into(),
                external: true,
                default_track: false,
                selected: false,
            },
        ];
        assert_eq!(
            EncodeContext::select_external_audio(&tracks, 2),
            Some("https://rr1---sn.googlevideo.com/videoplayback?mime=audio".into())
        );
        assert_eq!(
            EncodeContext::select_external_audio(&tracks, 3),
            Some("edl://%12https://cdn.example/audio.m4a".into())
        );
        // Embedded audio track (empty external_filename) returns None and never grabs another track.
        assert_eq!(EncodeContext::select_external_audio(&tracks, 1), None);
        // Disabled or missing audio_id returns None.
        assert_eq!(EncodeContext::select_external_audio(&tracks, 0), None);
        assert_eq!(EncodeContext::select_external_audio(&tracks, -1), None);
        assert_eq!(EncodeContext::select_external_audio(&tracks, 99), None);
    }

    #[test]
    fn mpv_captures_full_streaming_encode_context_and_http_headers() {
        // Test synthetic string-list extraction for http-header-fields.
        let mut hdr_nodes = [
            ffi::mpv_node {
                format: ffi::MPV_FORMAT_STRING,
                u: ffi::mpv_node__bindgen_ty_1 {
                    string: c"Authorization: Bearer abc,def".as_ptr().cast_mut(),
                },
            },
            ffi::mpv_node {
                format: ffi::MPV_FORMAT_STRING,
                u: ffi::mpv_node__bindgen_ty_1 {
                    string: c"X-Custom: value".as_ptr().cast_mut(),
                },
            },
        ];
        let mut hdr_list = ffi::mpv_node_list {
            num: hdr_nodes.len() as c_int,
            keys: ptr::null_mut(),
            values: hdr_nodes.as_mut_ptr(),
        };
        let hdr_root = ffi::mpv_node {
            format: ffi::MPV_FORMAT_NODE_ARRAY,
            u: ffi::mpv_node__bindgen_ty_1 {
                list: &mut hdr_list,
            },
        };
        assert_eq!(
            extract_string_list_from_node(&hdr_root),
            vec!["Authorization: Bearer abc,def", "X-Custom: value"]
        );

        // Verify live MpvInstance captures CLI streaming options into EncodeContext.
        let dir =
            std::env::temp_dir().join(format!("memento-stream-ctx-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("input.conf"), "").unwrap();
        std::fs::write(dir.join("mpv.conf"), "ao=null\n").unwrap();

        let player = MpvInstance::new_with_options(
            Some(Path::new(&dir)),
            &[
                CliOption {
                    name: "script-opts".into(),
                    value: "ytdl_hook-ytdl_path=/usr/bin/yt-dlp".into(),
                },
                CliOption {
                    name: "http-header-fields-append".into(),
                    value: "X-Test: one".into(),
                },
                CliOption {
                    name: "http-header-fields-append".into(),
                    value: "X-Other: two".into(),
                },
                CliOption {
                    name: "referrer".into(),
                    value: "https://example.com/watch".into(),
                },
                CliOption {
                    name: "user-agent".into(),
                    value: "MementoTestAgent/1.0".into(),
                },
                CliOption {
                    name: "cookies".into(),
                    value: "yes".into(),
                },
                CliOption {
                    name: "cookies-file".into(),
                    value: "/tmp/memento-cookies.txt".into(),
                },
                CliOption {
                    name: "ytdl-raw-options".into(),
                    value: "cookies-from-browser=firefox".into(),
                },
            ],
        )
        .unwrap();

        // Wait for all initial observed property notifications to flush before injecting synthetic loaded state.
        let init_deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        let mut saw_initial_last = false;
        while !saw_initial_last && std::time::Instant::now() < init_deadline {
            for ev in player.drain_events() {
                if matches!(ev, MpvEvent::PropertyChange { name, .. } if name == "cursor-autohide-fs-only")
                {
                    saw_initial_last = true;
                }
            }
            if !saw_initial_last {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        }
        assert!(saw_initial_last, "initial observed properties did not flush");

        // No file loaded yet -> capture_encode_context is None.
        assert!(player.capture_encode_context().is_none());

        // Populate loaded media & split external audio state as delivered by property events.
        {
            let mut st = player.playback.borrow_mut();
            st.file_loaded = true;
            st.path = "https://www.youtube.com/watch?v=test".into();
            st.stream_open_filename = "https://rr1.googlevideo.com/videoplayback".into();
            st.audio_id = 2;
            st.audio_tracks = vec![
                AudioTrack {
                    id: 1,
                    language: "ja".into(),
                    title: "Internal".into(),
                    external_filename: String::new(),
                    external: false,
                    default_track: false,
                    selected: false,
                },
                AudioTrack {
                    id: 2,
                    language: "ja".into(),
                    title: "DASH Audio".into(),
                    external_filename: "https://rr1.googlevideo.com/audioplayback".into(),
                    external: true,
                    default_track: true,
                    selected: true,
                },
            ];
        }

        let ctx = player
            .capture_encode_context()
            .expect("loaded media should produce EncodeContext");
        assert_eq!(ctx.path, "https://www.youtube.com/watch?v=test");
        assert_eq!(ctx.audio_id, 2);
        assert_eq!(
            ctx.stream_open_filename,
            "https://rr1.googlevideo.com/videoplayback"
        );
        assert_eq!(
            ctx.effective_media_path(),
            "https://rr1.googlevideo.com/videoplayback"
        );
        assert!(
            ctx.script_opts.contains("ytdl_hook-ytdl_path=/usr/bin/yt-dlp"),
            "unexpected script_opts: {}",
            ctx.script_opts
        );
        assert_eq!(
            ctx.http_header_fields,
            vec!["X-Test: one".to_owned(), "X-Other: two".to_owned()]
        );
        assert_eq!(ctx.referrer, "https://example.com/watch");
        assert_eq!(ctx.user_agent, "MementoTestAgent/1.0");
        assert!(ctx.cookies);
        assert_eq!(ctx.cookies_file, "/tmp/memento-cookies.txt");
        assert!(
            ctx.ytdl_raw_options.contains("cookies-from-browser=firefox"),
            "unexpected ytdl_raw_options: {}",
            ctx.ytdl_raw_options
        );
        assert_eq!(
            ctx.external_audio_filename.as_deref(),
            Some("https://rr1.googlevideo.com/audioplayback")
        );

        // Internal memento-asr:// URI in stream_open_filename is stripped on capture.
        player.playback.borrow_mut().stream_open_filename = ASR_STREAM_URI.into();
        let ctx_asr = player.capture_encode_context().unwrap();
        assert!(ctx_asr.stream_open_filename.is_empty());
        assert_eq!(
            ctx_asr.effective_media_path(),
            "https://www.youtube.com/watch?v=test"
        );

        // Runtime property changes (e.g. from ytdl_hook / set / change-list commands) are observed live.
        player
            .command_async(&["set", "referrer", "https://updated.example/ref"])
            .unwrap();
        player
            .command_async(&["set", "user-agent", "UpdatedAgent/2.0"])
            .unwrap();
        player.command_async(&["set", "cookies", "no"]).unwrap();
        player
            .command_async(&["change-list", "http-header-fields", "append", "X-Live: three"])
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while std::time::Instant::now() < deadline {
            let _ = player.drain_events();
            let st = player.playback_state();
            if st.referrer == "https://updated.example/ref"
                && st.user_agent == "UpdatedAgent/2.0"
                && !st.cookies
                && st.http_header_fields.iter().any(|h| h == "X-Live: three")
            {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let ctx_updated = player.capture_encode_context().unwrap();
        assert_eq!(ctx_updated.referrer, "https://updated.example/ref");
        assert_eq!(ctx_updated.user_agent, "UpdatedAgent/2.0");
        assert!(!ctx_updated.cookies);
        assert_eq!(
            ctx_updated.http_header_fields,
            vec![
                "X-Test: one".to_owned(),
                "X-Other: two".to_owned(),
                "X-Live: three".to_owned()
            ]
        );

        drop(player);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn mpv_instance_sub_to_media_time_roundtrips_with_sub_raw_time() {
        let player = MpvInstance::new_software(None).unwrap();
        {
            let mut st = player.playback.borrow_mut();
            st.sub_speed = 1.25;
            st.sub_delay = -0.5;
            st.secondary_sub_delay = 1.75;
        }
        let cue = 16.0;
        let media = player.sub_to_media_time(cue);
        assert!((media - 19.5).abs() < 1e-12);
        assert!((player.get_sub_raw_time(media) - cue).abs() < 1e-12);

        let sec_media = player.secondary_sub_to_media_time(cue);
        assert!((sec_media - 21.75).abs() < 1e-12);
        assert!((player.get_secondary_sub_raw_time(sec_media) - cue).abs() < 1e-12);
    }

    #[test]
    fn fullscreen_property_is_observed_and_bidirectional_via_set_fullscreen() {
        assert!(
            PLAYBACK_PROPERTIES
                .iter()
                .any(|spec| spec.name == "fullscreen" && spec.format == ffi::MPV_FORMAT_FLAG),
            "fullscreen must be observed in PLAYBACK_PROPERTIES"
        );

        let player = MpvInstance::new_software(None).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(30));
        let _ = player.drain_events();
        assert!(!player.is_fullscreen());

        player.set_fullscreen(true).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        let mut saw_true_event = false;
        while std::time::Instant::now() < deadline {
            for ev in player.drain_events() {
                if matches!(
                    ev,
                    MpvEvent::PropertyChange {
                        name,
                        value: MpvPropertyValue::Flag(true),
                    } if name == "fullscreen"
                ) {
                    saw_true_event = true;
                }
            }
            if player.is_fullscreen() && saw_true_event {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(player.is_fullscreen(), "expected fullscreen == true");
        assert!(saw_true_event, "expected fullscreen PropertyChange(true)");

        player.set_fullscreen(false).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while std::time::Instant::now() < deadline {
            let _ = player.drain_events();
            if !player.is_fullscreen() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(!player.is_fullscreen(), "expected fullscreen == false");
    }

    #[test]
    fn double_left_click_mouse_btn0_cycles_mpv_fullscreen_once() {
        let player = MpvInstance::new_software(None).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(30));
        let _ = player.drain_events();
        assert!(!player.is_fullscreen());

        // Forward two rapid MOUSE_BTN0 clicks exactly as MouseInputForwarder does.
        for _ in 0..2 {
            player.command_async(&["mouse", "100", "100"]).unwrap();
            player.command_async(&["keydown", "MOUSE_BTN0"]).unwrap();
            player.command_async(&["keyup", "MOUSE_BTN0"]).unwrap();
        }

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        let mut transitions = Vec::new();
        while std::time::Instant::now() < deadline {
            for ev in player.drain_events() {
                if let MpvEvent::PropertyChange {
                    name,
                    value: MpvPropertyValue::Flag(f),
                } = ev
                {
                    if name == "fullscreen" {
                        transitions.push(f);
                    }
                }
            }
            if !transitions.is_empty() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(
            transitions,
            vec![true],
            "double MOUSE_BTN0 must cycle mpv fullscreen once to true"
        );
        assert!(player.is_fullscreen());
    }

    #[test]
    fn video_tracks_and_vid_extraction_and_observation() {
        let mut vid_keys = [
            c"id".as_ptr().cast_mut(),
            c"type".as_ptr().cast_mut(),
            c"title".as_ptr().cast_mut(),
            c"lang".as_ptr().cast_mut(),
            c"codec".as_ptr().cast_mut(),
            c"external".as_ptr().cast_mut(),
            c"default".as_ptr().cast_mut(),
            c"selected".as_ptr().cast_mut(),
            c"main-selection".as_ptr().cast_mut(),
        ];
        let mut vid_values = [
            ffi::mpv_node {
                format: ffi::MPV_FORMAT_INT64,
                u: ffi::mpv_node__bindgen_ty_1 { int64: 2 },
            },
            ffi::mpv_node {
                format: ffi::MPV_FORMAT_STRING,
                u: ffi::mpv_node__bindgen_ty_1 {
                    string: c"video".as_ptr().cast_mut(),
                },
            },
            ffi::mpv_node {
                format: ffi::MPV_FORMAT_STRING,
                u: ffi::mpv_node__bindgen_ty_1 {
                    string: c"1080p HEVC".as_ptr().cast_mut(),
                },
            },
            ffi::mpv_node {
                format: ffi::MPV_FORMAT_STRING,
                u: ffi::mpv_node__bindgen_ty_1 {
                    string: c"jpn".as_ptr().cast_mut(),
                },
            },
            ffi::mpv_node {
                format: ffi::MPV_FORMAT_STRING,
                u: ffi::mpv_node__bindgen_ty_1 {
                    string: c"hevc".as_ptr().cast_mut(),
                },
            },
            ffi::mpv_node {
                format: ffi::MPV_FORMAT_FLAG,
                u: ffi::mpv_node__bindgen_ty_1 { flag: 0 },
            },
            ffi::mpv_node {
                format: ffi::MPV_FORMAT_FLAG,
                u: ffi::mpv_node__bindgen_ty_1 { flag: 1 },
            },
            ffi::mpv_node {
                format: ffi::MPV_FORMAT_FLAG,
                u: ffi::mpv_node__bindgen_ty_1 { flag: 1 },
            },
            ffi::mpv_node {
                format: ffi::MPV_FORMAT_INT64,
                u: ffi::mpv_node__bindgen_ty_1 { int64: 0 },
            },
        ];
        let mut vid_map = ffi::mpv_node_list {
            num: vid_keys.len() as c_int,
            keys: vid_keys.as_mut_ptr(),
            values: vid_values.as_mut_ptr(),
        };
        let mut vid_entry = ffi::mpv_node {
            format: ffi::MPV_FORMAT_NODE_MAP,
            u: ffi::mpv_node__bindgen_ty_1 {
                list: &mut vid_map,
            },
        };
        let mut root_list = ffi::mpv_node_list {
            num: 1,
            keys: ptr::null_mut(),
            values: &mut vid_entry,
        };
        let mut root = ffi::mpv_node {
            format: ffi::MPV_FORMAT_NODE_ARRAY,
            u: ffi::mpv_node__bindgen_ty_1 {
                list: &mut root_list,
            },
        };

        let extracted = extract_all_tracks_from_node(&root);
        assert_eq!(extracted.video.len(), 1);
        assert_eq!(
            extracted.video[0],
            VideoTrack {
                id: 2,
                title: "1080p HEVC".into(),
                language: "jpn".into(),
                codec: "hevc".into(),
                external: false,
                default_track: true,
                selected: true,
            }
        );

        let mut state = PlaybackState::default();
        let event = ffi::mpv_event_property {
            name: c"track-list".as_ptr(),
            format: ffi::MPV_FORMAT_NODE,
            data: (&mut root as *mut ffi::mpv_node).cast(),
        };
        unsafe { state.update(&event) };
        assert_eq!(state.video_tracks.len(), 1);
        assert_eq!(state.video_tracks[0].codec, "hevc");
        assert_eq!(state.video_id, 2);

        // Direct `vid` property updates work for both INT64 and FLAG(false).
        let mut new_vid: i64 = 5;
        let vid_event = ffi::mpv_event_property {
            name: c"vid".as_ptr(),
            format: ffi::MPV_FORMAT_INT64,
            data: (&mut new_vid as *mut i64).cast(),
        };
        unsafe { state.update(&vid_event) };
        assert_eq!(state.video_id, 5);

        let mut flag_no: c_int = 0;
        let vid_off = ffi::mpv_event_property {
            name: c"vid".as_ptr(),
            format: ffi::MPV_FORMAT_FLAG,
            data: (&mut flag_no as *mut c_int).cast(),
        };
        unsafe { state.update(&vid_off) };
        assert_eq!(state.video_id, 0);

        // Live MpvInstance accessors & async set_vid
        let player = MpvInstance::new_software(None).unwrap();
        assert_eq!(player.selected_vid(), 0);
        assert!(player.video_tracks().is_empty());
        player.set_vid(1).unwrap();
        player.set_vid(0).unwrap();
    }
}

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
    SubLine, SubtitleTrack, TrackType, ASR_STREAM_PROTOCOL, ASR_STREAM_URI,
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
        extract_chapters_from_node, extract_sub_lines_from_node, extract_tracks_from_node,
    };
    use super::node::{
        command_name, copy_mpv_string, cue_valid, id_valid, supports_memory_subtitles,
        MAX_TEXT_BYTES,
    };
    use super::render::resolve_gl_proc;
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
}

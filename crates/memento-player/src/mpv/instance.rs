//! `MpvInstance` lifecycle, event draining, render context management, and `Drop`.

use std::cell::{Cell, Ref, RefCell};
use std::collections::VecDeque;
use std::ffi::{c_int, c_void, CStr, CString};
use std::path::Path;
use std::ptr::{self, NonNull};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::SyncSender;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use crate::controller::CliOption;

use super::asr_stream::stream_cb_open_ro;
use super::event::MpvEvent;
use super::extract::extract_string_list_from_node;
use super::ffi;
use super::hwdec::{compatible_opengl_hwdec_for_session, compatible_software_hwdec};
use super::node::{copy_mpv_string, supports_memory_subtitles, OwnedNode};
use super::playback_state::{ObservedProperty, PlaybackState, PLAYBACK_PROPERTIES};
use super::render::{
    create_opengl_context_with_options, create_software_context, render_rgba_context,
    GlProcResolver, MpvSoftwareRenderer, RenderApi, RenderContext,
};
use super::screenshot::{start_screenshot_worker, CaptureJob, RawMpvHandle};
use super::types::{CommandReplyResult, EndFileReason, GlDisplayBackend, MpvPropertyName};
use super::wakeup::{on_mpv_wakeup, on_render_update, EventNotification, RenderNotification};

/// An initialized mpv instance. It starts with a software render context and
/// can switch to OpenGL after the UI has created its graphics context.
pub struct MpvInstance {
    pub(super) handle: *mut ffi::mpv_handle,
    pub(super) render_lock: Arc<Mutex<RenderContext>>,
    // mpv invokes update/wakeup callbacks from internal worker threads.
    // Raw pointers handed to libmpv come from `Arc::into_raw` (avoiding `Box`
    // `noalias` UB) and are reclaimed via `Arc::from_raw` in `Drop` only after
    // unregistering callbacks and calling `mpv_terminate_destroy`.
    render_pending: Arc<RenderNotification>,
    render_pending_raw: Option<*const RenderNotification>,
    event_pending: Arc<EventNotification>,
    event_pending_raw: Option<*const EventNotification>,
    pub(super) playback: RefCell<PlaybackState>,
    pub(super) queued_events: RefCell<VecDeque<MpvEvent>>,
    configured_hwdec: String,
    pub(super) memory_subtitles_supported: bool,
    pub(super) asr_stream: Arc<RwLock<Arc<[u8]>>>,
    asr_stream_raw: Option<*const RwLock<Arc<[u8]>>>,
    pub(super) next_reply_id: AtomicU64,
    pub(super) screenshot_jobs: Option<SyncSender<CaptureJob>>,
    screenshot_closing: Arc<AtomicBool>,
    screenshot_wake_handle: Arc<Mutex<Option<RawMpvHandle>>>,
    screenshot_thread: Option<std::thread::JoinHandle<()>>,
    render_update_delay: Cell<Duration>,
}
unsafe impl Send for MpvInstance {}

impl MpvInstance {
    /// Effective decoder preference, including the user's mpv.conf overrides.
    pub fn configured_hwdec(&self) -> &str {
        &self.configured_hwdec
    }

    /// Initialize mpv and a software fallback before the first media load.
    pub fn new_software(config_dir: Option<&Path>) -> Result<Self, String> {
        Self::create(config_dir, &[])
    }

    /// Initialize mpv with global CLI `--<opt>=<val>` overrides.
    pub fn new_with_options(
        config_dir: Option<&Path>,
        cli_options: &[CliOption],
    ) -> Result<Self, String> {
        Self::create(config_dir, cli_options)
    }

    fn create(config_dir: Option<&Path>, cli_options: &[CliOption]) -> Result<Self, String> {
        let handle = unsafe { ffi::mpv_create() };
        if handle.is_null() {
            return Err("Failed to create mpv instance".into());
        }

        let opt = |name: &CStr, value: &CStr| unsafe {
            ffi::mpv_set_option_string(handle, name.as_ptr(), value.as_ptr())
        };
        for (name, value) in [
            (c"config", c"yes"),
            (c"input-default-bindings", c"yes"),
            (c"keep-open", c"yes"),
            (c"screenshot-directory", c"~~desktop/"),
            (c"terminal", c"yes"),
            (c"ytdl", c"yes"),
            (c"pause", c"yes"),
            (c"sub-visibility", c"no"),
            (c"hwdec", c"auto-safe"),
            (c"hwdec-codecs", c"all"),
            (c"framedrop", c"vo"),
            (c"video-timing-offset", c"0"),
            (c"vd-lavc-threads", c"0"),
        ] {
            opt(name, value);
        }
        if let Some(dir) = config_dir {
            if let Ok(dir_c) = CString::new(dir.to_string_lossy().as_bytes()) {
                opt(c"config-dir", &dir_c);
            }
            let input_conf = dir.join("input.conf");
            if let Ok(input_c) = CString::new(input_conf.to_string_lossy().as_bytes()) {
                opt(c"input-conf", &input_c);
            }
        }

        fn is_pre_init_only(name: &str) -> bool {
            matches!(
                name,
                "config"
                    | "config-dir"
                    | "input-conf"
                    | "input-test"
                    | "load-scripts"
                    | "msg-level"
                    | "v"
                    | "dump-stats"
                    | "gpu-debug"
            ) || name == "script"
                || name.starts_with("scripts")
                || name.starts_with("script-opts")
        }

        fn parse_list_action(name: &str) -> Option<(&str, &'static str)> {
            if is_pre_init_only(name) {
                return None;
            }
            match name {
                "glsl-shader" => return Some(("glsl-shaders", "append")),
                "sub-file" => return Some(("sub-files", "append")),
                "audio-file" => return Some(("audio-files", "append")),
                "external-file" => return Some(("external-files", "append")),
                _ => {}
            }
            for (suffix, op) in [
                ("-append", "append"),
                ("-add", "add"),
                ("-pre", "pre"),
                ("-clr", "clr"),
                ("-del", "del"),
                ("-remove", "remove"),
                ("-toggle", "toggle"),
            ] {
                if let Some(prop) = name.strip_suffix(suffix).filter(|p| !p.is_empty()) {
                    return Some((prop, op));
                }
            }
            None
        }

        // Pre-init pass: apply scalar and pre-init-only CLI options.
        for cli_opt in cli_options {
            if cli_opt.name == "vo" || parse_list_action(&cli_opt.name).is_some() {
                continue;
            }
            if let (Ok(name_c), Ok(val_c)) = (
                CString::new(cli_opt.name.as_bytes()),
                CString::new(cli_opt.value.as_bytes()),
            ) {
                let mut status = opt(&name_c, &val_c);
                if status == ffi::MPV_ERROR_OPTION_ERROR && cli_opt.value == "yes" {
                    status = opt(&name_c, c"");
                }
                if status < 0 {
                    eprintln!(
                        "mpv CLI option --{}={} ignored ({status})",
                        cli_opt.name, cli_opt.value
                    );
                }
            }
        }
        let status = opt(c"vo", c"libmpv");
        if status < 0 {
            unsafe { ffi::mpv_terminate_destroy(handle) };
            return Err(format!("mpv libmpv video option failed with code {status}"));
        }

        let status = unsafe { ffi::mpv_initialize(handle) };
        if status < 0 {
            unsafe { ffi::mpv_terminate_destroy(handle) };
            return Err(format!("mpv initialization failed with code {status}"));
        }

        // Post-init pass: re-apply scalar options so CLI overrides `mpv.conf`,
        // and apply list-modifying options ONCE via `change-list`.
        for cli_opt in cli_options {
            if cli_opt.name == "vo" || is_pre_init_only(&cli_opt.name) {
                continue;
            }
            if let Some((prop, action)) = parse_list_action(&cli_opt.name) {
                let val = if action == "clr" && cli_opt.value == "yes" {
                    ""
                } else {
                    cli_opt.value.as_str()
                };
                if let (Ok(prop_c), Ok(act_c), Ok(val_c)) = (
                    CString::new(prop.as_bytes()),
                    CString::new(action.as_bytes()),
                    CString::new(val.as_bytes()),
                ) {
                    let mut cmd = [
                        c"change-list".as_ptr(),
                        prop_c.as_ptr(),
                        act_c.as_ptr(),
                        val_c.as_ptr(),
                        ptr::null(),
                    ];
                    let status = unsafe { ffi::mpv_command(handle, cmd.as_mut_ptr()) };
                    if status < 0 {
                        eprintln!(
                            "mpv CLI list option --{}={} ignored ({status})",
                            cli_opt.name, cli_opt.value
                        );
                    }
                }
                continue;
            }
            if let (Ok(name_c), Ok(val_c)) = (
                CString::new(cli_opt.name.as_bytes()),
                CString::new(cli_opt.value.as_bytes()),
            ) {
                let _ = unsafe {
                    ffi::mpv_set_property_string(handle, name_c.as_ptr(), val_c.as_ptr())
                };
            }
        }

        let read_string_prop = |name: &CStr| unsafe {
            let value = ffi::mpv_get_property_string(handle, name.as_ptr());
            let text = copy_mpv_string(value);
            if !value.is_null() {
                ffi::mpv_free(value.cast());
            }
            text
        };
        let read_flag_prop = |name: &CStr| unsafe {
            let mut flag: c_int = 0;
            let status = ffi::mpv_get_property(
                handle,
                name.as_ptr(),
                ffi::MPV_FORMAT_FLAG,
                (&mut flag as *mut c_int).cast(),
            );
            status >= 0 && flag != 0
        };
        let read_string_list_prop = |name: &CStr| unsafe {
            let mut node = OwnedNode::new();
            let status = ffi::mpv_get_property(
                handle,
                name.as_ptr(),
                ffi::MPV_FORMAT_NODE,
                (&mut node.0 as *mut ffi::mpv_node).cast(),
            );
            if status < 0 {
                Vec::new()
            } else {
                extract_string_list_from_node(&node.0)
            }
        };

        let configured_hwdec = read_string_prop(c"hwdec");
        let initial_script_opts = read_string_prop(c"script-opts");
        let initial_http_header_fields = read_string_list_prop(c"http-header-fields");
        let initial_referrer = read_string_prop(c"referrer");
        let initial_user_agent = read_string_prop(c"user-agent");
        let initial_cookies = read_flag_prop(c"cookies");
        let initial_cookies_file = read_string_prop(c"cookies-file");
        let initial_ytdl_raw_options = read_string_prop(c"ytdl-raw-options");
        let initial_fullscreen = read_flag_prop(c"fullscreen");
        let memory_subtitles_supported = unsafe { supports_memory_subtitles(handle) };

        let asr_stream: Arc<RwLock<Arc<[u8]>>> =
            Arc::new(RwLock::new(Arc::from(b"WEBVTT\n\n".as_slice())));
        let asr_stream_raw = Arc::into_raw(asr_stream.clone());
        let stream_status = unsafe {
            ffi::mpv_stream_cb_add_ro(
                handle,
                c"memento-asr".as_ptr(),
                asr_stream_raw.cast_mut().cast(),
                Some(stream_cb_open_ro),
            )
        };
        if stream_status < 0 {
            unsafe { ffi::mpv_terminate_destroy(handle) };
            drop(unsafe { Arc::from_raw(asr_stream_raw) });
            return Err(format!(
                "mpv_stream_cb_add_ro failed with code {stream_status}"
            ));
        }

        let event_pending = Arc::new(EventNotification::default());
        let event_pending_raw = Arc::into_raw(event_pending.clone());
        unsafe {
            ffi::mpv_set_wakeup_callback(
                handle,
                Some(on_mpv_wakeup),
                event_pending_raw.cast_mut().cast(),
            );
        }

        for (id, spec) in PLAYBACK_PROPERTIES.iter().enumerate() {
            let status = unsafe {
                ffi::mpv_observe_property(handle, id as u64 + 1, spec.c_name.as_ptr(), spec.format)
            };
            if status < 0 {
                eprintln!(
                    "Could not observe mpv property {}: {status}",
                    spec.name
                );
            }
        }

        let context = match create_software_context(handle) {
            Ok(context) => context,
            Err(error) => {
                unsafe {
                    ffi::mpv_set_wakeup_callback(handle, None, ptr::null_mut());
                    ffi::mpv_terminate_destroy(handle);
                }
                drop(unsafe { Arc::from_raw(event_pending_raw) });
                drop(unsafe { Arc::from_raw(asr_stream_raw) });
                return Err(error);
            }
        };

        let render_pending = Arc::new(RenderNotification::default());
        let render_pending_raw = Arc::into_raw(render_pending.clone());
        unsafe {
            ffi::mpv_render_context_set_update_callback(
                context.as_ptr(),
                Some(on_render_update),
                render_pending_raw.cast_mut().cast(),
            );
        }

        let worker = match start_screenshot_worker(handle) {
            Ok(worker) => worker,
            Err(error) => {
                unsafe {
                    ffi::mpv_render_context_set_update_callback(
                        context.as_ptr(),
                        None,
                        ptr::null_mut(),
                    );
                    ffi::mpv_render_context_free(context.as_ptr());
                    ffi::mpv_set_wakeup_callback(handle, None, ptr::null_mut());
                    ffi::mpv_terminate_destroy(handle);
                }
                drop(unsafe { Arc::from_raw(render_pending_raw) });
                drop(unsafe { Arc::from_raw(event_pending_raw) });
                drop(unsafe { Arc::from_raw(asr_stream_raw) });
                return Err(error);
            }
        };

        Ok(Self {
            handle,
            render_lock: Arc::new(Mutex::new(RenderContext {
                pointer: Some(context),
                api: RenderApi::Software,
                gl_proc_resolver: None,
                display_backend: GlDisplayBackend::Unknown,
                advanced_control: false,
                owner_thread: None,
            })),
            render_pending,
            render_pending_raw: Some(render_pending_raw),
            event_pending,
            event_pending_raw: Some(event_pending_raw),
            playback: RefCell::new(PlaybackState {
                paused: true,
                volume: 100.0,
                volume_max: 100,
                fullscreen: initial_fullscreen,
                sub_speed: 1.0,
                script_opts: initial_script_opts,
                http_header_fields: initial_http_header_fields,
                referrer: initial_referrer,
                user_agent: initial_user_agent,
                cookies: initial_cookies,
                cookies_file: initial_cookies_file,
                ytdl_raw_options: initial_ytdl_raw_options,
                ..Default::default()
            }),
            queued_events: RefCell::new(VecDeque::new()),
            configured_hwdec,
            memory_subtitles_supported,
            asr_stream,
            asr_stream_raw: Some(asr_stream_raw),
            next_reply_id: AtomicU64::new(1),
            screenshot_jobs: Some(worker.jobs),
            screenshot_closing: worker.closing,
            screenshot_wake_handle: worker.wake_handle,
            screenshot_thread: Some(worker.thread),
            render_update_delay: Cell::new(Duration::ZERO),
        })
    }

    /// Adapt `hwdec` cleanly for the active GL renderer and display backend
    /// while preserving explicit user choices.
    pub fn configure_opengl_decoder(&self, renderer: &str) -> Result<(), String> {
        let is_wayland = self.active_display_backend() == GlDisplayBackend::Wayland;
        let compatible =
            compatible_opengl_hwdec_for_session(&self.configured_hwdec, renderer, is_wayland);
        if compatible != self.configured_hwdec {
            self.command_async(&["set", "hwdec", &compatible])?;
            eprintln!(
                "Video decoder compatibility: {} -> {compatible} ({renderer}, wayland={is_wayland})",
                self.configured_hwdec
            );
        }
        Ok(())
    }

    /// Return the native display backend bound to the active OpenGL render context.
    pub fn active_display_backend(&self) -> GlDisplayBackend {
        self.render_lock
            .lock()
            .map(|guard| guard.display_backend)
            .unwrap_or(GlDisplayBackend::Unknown)
    }

    /// Return whether `MPV_RENDER_PARAM_ADVANCED_CONTROL` is active on the current render context.
    pub fn is_advanced_control_active(&self) -> bool {
        self.render_lock
            .lock()
            .is_ok_and(|guard| guard.advanced_control && guard.pointer.is_some())
    }

    /// Register a lightweight event-loop wakeup once. Called from mpv's threads;
    /// it must only enqueue work, never call mpv or render synchronously.
    pub fn set_render_wakeup(&self, wake: impl Fn() + Send + Sync + 'static) {
        if self.render_pending.wake.set(Box::new(wake)).is_ok() && self.has_render_update() {
            self.render_pending.trigger_wake();
        }
    }

    pub fn has_render_update(&self) -> bool {
        self.render_pending.pending.load(Ordering::Acquire)
    }

    /// Register a lightweight coalesced wakeup callback via `mpv_set_wakeup_callback`.
    pub fn set_wakeup_callback(&self, wake: impl Fn() + Send + Sync + 'static) {
        if self.event_pending.wake.set(Box::new(wake)).is_ok()
            && self.event_pending.pending.load(Ordering::Acquire)
        {
            if let Some(wake) = self.event_pending.wake.get() {
                wake();
            }
        }
    }

    pub fn has_pending_events(&self) -> bool {
        self.event_pending.pending.load(Ordering::Acquire)
            || !self.queued_events.borrow().is_empty()
    }

    pub(super) fn decode_one_event(&self, raw: &ffi::mpv_event) -> Option<MpvEvent> {
        let mut state = self.playback.borrow_mut();
        match raw.event_id {
            ffi::MPV_EVENT_START_FILE => {
                state.file_loaded = false;
                state.playback_ready = false;
                state.audio_id = 0;
                state.primary_sid = 0;
                state.secondary_sid = 0;
                state.video_id = 0;
                state.stream_open_filename.clear();
                state.sub_ass_extradata.clear();
                state.sub_text_ass_full.clear();
                let playlist_entry_id =
                    unsafe { raw.data.cast::<ffi::mpv_event_start_file>().as_ref() }
                        .map_or(0, |e| e.playlist_entry_id);
                Some(MpvEvent::StartFile { playlist_entry_id })
            }
            ffi::MPV_EVENT_FILE_LOADED => {
                state.file_loaded = true;
                let mut w = state.video_width;
                let mut h = state.video_height;
                if w == 0 && h == 0 && !self.is_opengl_active() {
                    let mut probed_w: i64 = 0;
                    let mut probed_h: i64 = 0;
                    unsafe {
                        if ffi::mpv_get_property(
                            self.handle,
                            c"video-params/w".as_ptr(),
                            ffi::MPV_FORMAT_INT64,
                            (&mut probed_w as *mut i64).cast(),
                        ) >= 0
                        {
                            w = probed_w;
                        }
                        if ffi::mpv_get_property(
                            self.handle,
                            c"video-params/h".as_ptr(),
                            ffi::MPV_FORMAT_INT64,
                            (&mut probed_h as *mut i64).cast(),
                        ) >= 0
                        {
                            h = probed_h;
                        }
                    }
                    state.video_width = w;
                    state.video_height = h;
                }
                Some(MpvEvent::FileLoaded {
                    video_width: w,
                    video_height: h,
                })
            }
            ffi::MPV_EVENT_END_FILE => {
                state.file_loaded = false;
                state.playback_ready = false;
                state.audio_id = 0;
                state.primary_sid = 0;
                state.secondary_sid = 0;
                state.video_id = 0;
                state.stream_open_filename.clear();
                state.sub_ass_extradata.clear();
                state.sub_text_ass_full.clear();
                let (reason, playlist_entry_id) =
                    unsafe { raw.data.cast::<ffi::mpv_event_end_file>().as_ref() }.map_or(
                        (EndFileReason::Unknown(-1), 0),
                        |info| {
                            let reason = match info.reason {
                                ffi::MPV_END_FILE_REASON_EOF => EndFileReason::Eof,
                                ffi::MPV_END_FILE_REASON_STOP => EndFileReason::Stop,
                                ffi::MPV_END_FILE_REASON_QUIT => EndFileReason::Quit,
                                ffi::MPV_END_FILE_REASON_ERROR => {
                                    EndFileReason::Error(info.error)
                                }
                                ffi::MPV_END_FILE_REASON_REDIRECT => EndFileReason::Redirect,
                                other => EndFileReason::Unknown(other as i32),
                            };
                            (reason, info.playlist_entry_id)
                        },
                    );
                Some(MpvEvent::EndFile {
                    reason,
                    playlist_entry_id,
                })
            }
            ffi::MPV_EVENT_PLAYBACK_RESTART => {
                state.playback_ready = true;
                Some(MpvEvent::PlaybackRestart)
            }
            ffi::MPV_EVENT_SEEK => Some(MpvEvent::Seek),
            ffi::MPV_EVENT_SHUTDOWN => Some(MpvEvent::Shutdown),
            ffi::MPV_EVENT_QUEUE_OVERFLOW => Some(MpvEvent::QueueOverflow),
            ffi::MPV_EVENT_COMMAND_REPLY => {
                let error = raw.error;
                let reply_userdata = raw.reply_userdata;
                if error < 0 && reply_userdata == 0 {
                    eprintln!(
                        "mpv command failed: {} ({error})",
                        copy_mpv_string(unsafe { ffi::mpv_error_string(error) })
                    );
                }
                let result = unsafe { raw.data.cast::<ffi::mpv_event_command>().as_ref() }
                    .map_or(CommandReplyResult::None, |cmd| {
                        let node = &cmd.result;
                        match node.format {
                            ffi::MPV_FORMAT_NONE => CommandReplyResult::None,
                            ffi::MPV_FORMAT_INT64 => {
                                CommandReplyResult::Int64(unsafe { node.u.int64 })
                            }
                            ffi::MPV_FORMAT_DOUBLE => {
                                CommandReplyResult::Double(unsafe { node.u.double_ })
                            }
                            ffi::MPV_FORMAT_FLAG => {
                                CommandReplyResult::Flag(unsafe { node.u.flag != 0 })
                            }
                            ffi::MPV_FORMAT_STRING => {
                                CommandReplyResult::String(copy_mpv_string(unsafe {
                                    node.u.string
                                }))
                            }
                            _ => CommandReplyResult::Other,
                        }
                    });
                Some(MpvEvent::CommandReply {
                    reply_userdata,
                    error,
                    result,
                })
            }
            ffi::MPV_EVENT_PROPERTY_CHANGE => {
                let property = unsafe { raw.data.cast::<ffi::mpv_event_property>().as_ref() }?;
                // O(1) dispatch via `reply_userdata` registered in `mpv_observe_property`:
                // zero `CStr::from_ptr`, zero `String` allocation for property names.
                let (prop_name, kind) = if let Some(spec) = raw
                    .reply_userdata
                    .checked_sub(1)
                    .and_then(|idx| usize::try_from(idx).ok())
                    .and_then(|idx| PLAYBACK_PROPERTIES.get(idx))
                {
                    (MpvPropertyName::borrowed(spec.name), Some(spec.kind))
                } else if !property.name.is_null() {
                    let bytes = unsafe { CStr::from_ptr(property.name) }.to_bytes();
                    let kind = ObservedProperty::from_bytes(bytes);
                    (MpvPropertyName::from(copy_mpv_string(property.name)), kind)
                } else {
                    (MpvPropertyName::borrowed(""), None)
                };
                let value = unsafe { state.apply_property_event(kind, property) };
                Some(MpvEvent::PropertyChange {
                    name: prop_name,
                    value,
                })
            }
            _ => None,
        }
    }

    pub(super) fn push_queued_event(&self, event: MpvEvent) {
        const MAX_QUEUED_EVENTS: usize = 4096;
        let mut queued = self.queued_events.borrow_mut();
        if queued.len() >= MAX_QUEUED_EVENTS {
            if let Some(drop_idx) = queued.iter().position(|ev| {
                matches!(
                    ev,
                    MpvEvent::PropertyChange { name, .. }
                        if name != "track-list"
                            && name != "sid"
                            && name != "aid"
                            && name != "vid"
                            && name != "fullscreen"
                            && name != "sub-ass-extradata"
                            && name != "sub-text/ass-full"
                            && name != "osd-dimensions"
                )
            }) {
                queued.remove(drop_idx);
            } else {
                queued.pop_front();
            }
        }
        queued.push_back(event);
    }

    /// Borrow the cached [`PlaybackState`] without calling `mpv_wait_event` or
    /// draining queued events.
    pub fn playback_state(&self) -> Ref<'_, PlaybackState> {
        self.playback.borrow()
    }

    /// Drain all queued libmpv events in strict arrival order via `mpv_wait_event(0.0)`
    /// without collapsing intermediate events. Also updates cached [`PlaybackState`].
    pub fn drain_events(&self) -> Vec<MpvEvent> {
        let mut events: Vec<MpvEvent> = self.queued_events.borrow_mut().drain(..).collect();
        // Clear pending BEFORE polling so any event queued concurrently triggers a fresh wakeup.
        self.event_pending.clear_pending();
        loop {
            let event = unsafe { ffi::mpv_wait_event(self.handle, 0.0) };
            let Some(raw) = (unsafe { event.as_ref() }) else {
                break;
            };
            if raw.event_id == ffi::MPV_EVENT_NONE {
                break;
            }
            if let Some(decoded) = self.decode_one_event(raw) {
                events.push(decoded);
            }
        }
        events
    }

    /// Drain asynchronous events into [`PlaybackState`] without waiting for mpv's core,
    /// preserving decoded events in `queued_events` so mixing `poll_playback_state()`
    /// and [`Self::drain_events`] never drops `CommandReply` / lifecycle / track events.
    pub fn poll_playback_state(&self) -> Ref<'_, PlaybackState> {
        self.event_pending.clear_pending();
        loop {
            let event = unsafe { ffi::mpv_wait_event(self.handle, 0.0) };
            let Some(raw) = (unsafe { event.as_ref() }) else {
                break;
            };
            if raw.event_id == ffi::MPV_EVENT_NONE {
                break;
            }
            if let Some(decoded) = self.decode_one_event(raw) {
                self.push_queued_event(decoded);
            }
        }
        self.playback.borrow()
    }

    pub fn cached_video_dimensions(&self) -> (u32, u32) {
        self.playback.borrow().dimensions
    }

    pub fn render_update_delay(&self) -> Duration {
        self.render_update_delay.get()
    }

    /// Age of an unconsumed frame notification, without calling into mpv.
    pub fn pending_render_delay(&self) -> Duration {
        self.render_pending.pending_delay()
    }

    /// Consume the notification on the render thread with its GL context current
    /// (or on the software-render thread when the software API is active).
    /// A `true` result means a new frame (or video configuration) needs a redraw.
    pub fn take_render_update(&self) -> bool {
        if !self.has_render_update() {
            return false;
        }
        let Ok(guard) = self.render_lock.try_lock() else {
            // `pending` remains `true`, so mpv will coalesce further update
            // callbacks; re-schedule the caller so the wakeup is never lost.
            self.render_pending.trigger_wake();
            return false;
        };
        let Some(delay) = self.render_pending.take() else {
            return false;
        };
        self.render_update_delay.set(delay);
        let Some(context) = guard.pointer else {
            return false;
        };
        unsafe {
            ffi::mpv_render_context_update(context.as_ptr())
                & u64::from(ffi::MPV_RENDER_UPDATE_FRAME)
                != 0
        }
    }

    /// Unconditionally service `mpv_render_context_update()` on the render thread
    /// with the GL context current, returning the raw `mpv_render_update_flag` bitmask.
    pub fn pump_render_update(&self) -> u64 {
        let Ok(guard) = self.render_lock.try_lock() else {
            if self.has_render_update() {
                self.render_pending.trigger_wake();
            }
            return 0;
        };
        if let Some(delay) = self.render_pending.take() {
            self.render_update_delay.set(delay);
        }
        let Some(context) = guard.pointer else {
            return 0;
        };
        unsafe { ffi::mpv_render_context_update(context.as_ptr()) }
    }

    /// Render directly into caller memory (row-major, top-down RGBA8, tightly packed, opaque).
    pub fn render_rgba(&self, width: u32, height: u32, pixels: &mut [u8]) -> Result<(), String> {
        let guard = self
            .render_lock
            .lock()
            .map_err(|_| "mpv render lock poisoned")?;
        if guard.api != RenderApi::Software {
            return Err("software rendering is inactive".into());
        }
        let context = guard.pointer.ok_or("mpv render context is unavailable")?;
        render_rgba_context(context.as_ptr(), width, height, pixels)
    }

    pub fn software_renderer(&self) -> MpvSoftwareRenderer {
        MpvSoftwareRenderer {
            render_lock: self.render_lock.clone(),
        }
    }

    /// Replace the CPU render context with libmpv's OpenGL renderer.
    pub fn activate_opengl(&self, resolver: &dyn Fn(&CStr) -> *const c_void) -> Result<(), String> {
        self.activate_opengl_with_display(resolver, None, None)
    }

    /// Replace the CPU render context with libmpv's OpenGL renderer while
    /// forwarding native X11 or Wayland display handles for zero-copy hwdec.
    pub fn activate_opengl_with_display(
        &self,
        resolver: &dyn Fn(&CStr) -> *const c_void,
        x11_display: Option<*mut c_void>,
        wl_display: Option<*mut c_void>,
    ) -> Result<(), String> {
        self.activate_opengl_inner(
            GlProcResolver::scoped(resolver),
            true,
            x11_display,
            wl_display,
        )
    }

    /// Like [`Self::activate_opengl_with_display`], retaining an owned `'static`
    /// proc-address resolver for the lifetime of the OpenGL render context.
    pub fn activate_opengl_owned_with_display(
        &self,
        resolver: impl Fn(&CStr) -> *const c_void + Send + Sync + 'static,
        x11_display: Option<*mut c_void>,
        wl_display: Option<*mut c_void>,
    ) -> Result<(), String> {
        self.activate_opengl_inner(
            GlProcResolver::owned(resolver),
            false,
            x11_display,
            wl_display,
        )
    }

    fn activate_opengl_inner(
        &self,
        resolver: GlProcResolver,
        is_scoped: bool,
        x11_display: Option<*mut c_void>,
        wl_display: Option<*mut c_void>,
    ) -> Result<(), String> {
        self.activate_opengl_inner_with_control(resolver, is_scoped, x11_display, wl_display, true)
    }

    /// Like [`Self::activate_opengl_owned_with_display`], with explicit control
    /// over `MPV_RENDER_PARAM_ADVANCED_CONTROL`.
    pub fn activate_opengl_owned_with_options(
        &self,
        resolver: impl Fn(&CStr) -> *const c_void + Send + Sync + 'static,
        x11_display: Option<*mut c_void>,
        wl_display: Option<*mut c_void>,
        advanced_control: bool,
    ) -> Result<(), String> {
        self.activate_opengl_inner_with_control(
            GlProcResolver::owned(resolver),
            false,
            x11_display,
            wl_display,
            advanced_control,
        )
    }

    fn activate_opengl_inner_with_control(
        &self,
        resolver: GlProcResolver,
        is_scoped: bool,
        x11_display: Option<*mut c_void>,
        wl_display: Option<*mut c_void>,
        advanced_control: bool,
    ) -> Result<(), String> {
        let mut guard = self
            .render_lock
            .lock()
            .map_err(|_| "mpv render lock poisoned")?;
        if guard.api == RenderApi::OpenGl {
            return Ok(());
        }
        let display_backend = if x11_display.is_some_and(|p| !p.is_null()) {
            GlDisplayBackend::X11
        } else if wl_display.is_some_and(|p| !p.is_null()) {
            GlDisplayBackend::Wayland
        } else {
            GlDisplayBackend::Unknown
        };
        let _ = self.render_pending.take();
        guard.free_context();
        let gl_result = create_opengl_context_with_options(
            self.handle,
            resolver,
            is_scoped,
            x11_display,
            wl_display,
            advanced_control,
        );
        let (context, resolver_ptr) = match gl_result {
            Ok(res) => res,
            Err(error) => {
                let fallback = create_software_context(self.handle)?;
                guard.pointer = Some(fallback);
                guard.api = RenderApi::Software;
                guard.display_backend = GlDisplayBackend::Unknown;
                guard.advanced_control = false;
                guard.owner_thread = None;
                self.install_render_callback(fallback);
                return Err(error);
            }
        };
        guard.pointer = Some(context);
        guard.api = RenderApi::OpenGl;
        guard.gl_proc_resolver = Some(resolver_ptr);
        guard.display_backend = display_backend;
        guard.advanced_control = advanced_control;
        guard.owner_thread = Some(std::thread::current().id());
        self.install_render_callback(context);
        self.render_pending.notify();
        Ok(())
    }

    /// Free GL resources before the UI destroys the graphics context.
    pub fn deactivate_opengl(&self) {
        if let Ok(mut guard) = self.render_lock.lock() {
            if guard.api == RenderApi::OpenGl {
                if guard.pointer.is_some()
                    && guard.owner_thread.is_some()
                    && guard.owner_thread != Some(std::thread::current().id())
                {
                    eprintln!(
                        "Warning: deactivate_opengl() called from a different thread than the owning GL context thread"
                    );
                }
                let _ = self.render_pending.take();
                guard.free_context();
                guard.api = RenderApi::Software;
            }
        }
    }

    pub fn fallback_to_software(&self) -> Result<(), String> {
        let mut guard = self
            .render_lock
            .lock()
            .map_err(|_| "mpv render lock poisoned")?;
        if guard.api == RenderApi::Software && guard.pointer.is_some() {
            return Ok(());
        }
        let _ = self.render_pending.take();
        guard.free_context();
        let context = create_software_context(self.handle)?;
        guard.pointer = Some(context);
        guard.api = RenderApi::Software;
        guard.display_backend = GlDisplayBackend::Unknown;
        guard.advanced_control = false;
        guard.owner_thread = None;
        self.install_render_callback(context);
        drop(guard);
        let sw_hwdec = compatible_software_hwdec(&self.configured_hwdec);
        if sw_hwdec != self.configured_hwdec {
            let _ = self.command_async(&["set", "hwdec", &sw_hwdec]);
        }
        self.render_pending.notify();
        Ok(())
    }

    pub fn is_opengl_active(&self) -> bool {
        self.render_lock
            .lock()
            .is_ok_and(|guard| guard.api == RenderApi::OpenGl && guard.pointer.is_some())
    }

    /// Draw to an RGBA8 GL framebuffer while the UI's context is current.
    pub fn render_opengl(&self, fbo: u32, width: u32, height: u32) -> Result<(), String> {
        self.render_opengl_with_format(fbo, width, height, 0)
    }

    /// Draw to an OpenGL framebuffer with an explicit internal format.
    pub fn render_opengl_with_format(
        &self,
        fbo: u32,
        width: u32,
        height: u32,
        internal_format: i32,
    ) -> Result<(), String> {
        self.render_opengl_ex(fbo, width, height, internal_format, false, false)
    }

    /// Draw to an OpenGL framebuffer with explicit internal format, vertical
    /// flip control (`MPV_RENDER_PARAM_FLIP_Y`), and target-time pacing.
    pub fn render_opengl_ex(
        &self,
        fbo: u32,
        width: u32,
        height: u32,
        internal_format: i32,
        flip_y: bool,
        block_for_target_time: bool,
    ) -> Result<(), String> {
        if width == 0 || height == 0 || width > i32::MAX as u32 || height > i32::MAX as u32 {
            return Err(format!(
                "mpv OpenGL render requires positive dimensions fitting in i32, got {width}x{height}"
            ));
        }
        let guard = self
            .render_lock
            .lock()
            .map_err(|_| "mpv render lock poisoned")?;
        if guard.api != RenderApi::OpenGl {
            return Err("OpenGL rendering is inactive".into());
        }
        let context = guard.pointer.ok_or("mpv render context is unavailable")?;
        let mut target = ffi::mpv_opengl_fbo {
            fbo: fbo as c_int,
            w: width as c_int,
            h: height as c_int,
            internal_format: internal_format as c_int,
        };
        let mut block_flag: c_int = c_int::from(block_for_target_time);
        let mut flip_flag: c_int = c_int::from(flip_y);
        let mut params = [
            ffi::mpv_render_param {
                type_: ffi::MPV_RENDER_PARAM_OPENGL_FBO,
                data: (&mut target as *mut ffi::mpv_opengl_fbo).cast(),
            },
            ffi::mpv_render_param {
                type_: ffi::MPV_RENDER_PARAM_FLIP_Y,
                data: (&mut flip_flag as *mut c_int).cast(),
            },
            ffi::mpv_render_param {
                type_: ffi::MPV_RENDER_PARAM_BLOCK_FOR_TARGET_TIME,
                data: (&mut block_flag as *mut c_int).cast(),
            },
            ffi::mpv_render_param {
                type_: ffi::MPV_RENDER_PARAM_INVALID,
                data: ptr::null_mut(),
            },
        ];
        let status =
            unsafe { ffi::mpv_render_context_render(context.as_ptr(), params.as_mut_ptr()) };
        if status < 0 {
            Err(format!("mpv OpenGL render failed with code {status}"))
        } else {
            Ok(())
        }
    }

    pub fn report_render_swap(&self) {
        if let Ok(guard) = self.render_lock.lock() {
            if let Some(context) = guard.pointer {
                unsafe { ffi::mpv_render_context_report_swap(context.as_ptr()) };
            }
        }
    }

    fn install_render_callback(&self, context: NonNull<ffi::mpv_render_context>) {
        if let Some(raw) = self.render_pending_raw {
            unsafe {
                ffi::mpv_render_context_set_update_callback(
                    context.as_ptr(),
                    Some(on_render_update),
                    raw.cast_mut().cast(),
                );
            }
        }
    }
}

impl Drop for MpvInstance {
    fn drop(&mut self) {
        // 1. Disconnect the screenshot channel, signal closing, and wake any
        //    blocked screenshot worker immediately so idle weak clients exit
        //    before render-context teardown.
        self.screenshot_jobs.take();
        self.screenshot_closing.store(true, Ordering::Release);
        if let Ok(guard) = self.screenshot_wake_handle.lock() {
            if let Some(weak) = *guard {
                unsafe { ffi::mpv_wakeup(weak.0) };
            }
        }

        // 2. Always free the mpv_render_context (and detach its update callback)
        //    BEFORE joining the screenshot worker and BEFORE calling
        //    mpv_terminate_destroy (required by libmpv render.h).
        let _ = self.render_pending.take();
        if let Ok(mut guard) = self.render_lock.lock() {
            if guard.api == RenderApi::OpenGl && guard.pointer.is_some() {
                eprintln!(
                    "Warning: MpvInstance dropped while OpenGL render context was still active; call deactivate_opengl() while the GL context is current"
                );
            }
            guard.free_context();
        }

        // 3. Wake any waiting weak screenshot client again and join its thread
        //    BEFORE calling mpv_terminate_destroy(self.handle).
        if let Ok(guard) = self.screenshot_wake_handle.lock() {
            if let Some(weak) = *guard {
                unsafe { ffi::mpv_wakeup(weak.0) };
            }
        }
        if let Some(thread) = self.screenshot_thread.take() {
            let _ = thread.join();
        }

        // 4. Unregister wakeup callback and terminate-destroy the core handle.
        if !self.handle.is_null() {
            unsafe {
                ffi::mpv_set_wakeup_callback(self.handle, None, ptr::null_mut());
                ffi::mpv_terminate_destroy(self.handle);
            }
            self.handle = ptr::null_mut();
        }

        // 5. All libmpv worker threads and callbacks are stopped after
        //    mpv_terminate_destroy returns; reclaim callback `Arc` refs now.
        if let Some(raw) = self.render_pending_raw.take() {
            drop(unsafe { Arc::from_raw(raw) });
        }
        if let Some(raw) = self.event_pending_raw.take() {
            drop(unsafe { Arc::from_raw(raw) });
        }
        if let Some(raw) = self.asr_stream_raw.take() {
            drop(unsafe { Arc::from_raw(raw) });
        }
    }
}

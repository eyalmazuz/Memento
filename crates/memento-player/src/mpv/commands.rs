//! Playback, track, subtitle, screenshot, and ASR command methods on [`MpvInstance`].

use std::ffi::{c_char, c_int, CStr, CString};
use std::ptr;
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::sync::Arc;

use crate::autopause::subtitle_raw_time;

use super::asr_stream::format_webvtt_cues;
use super::event::MpvEvent;
use super::ffi;
use super::instance::MpvInstance;
use super::node::{cue_valid, id_valid, OwnedNode};
use super::screenshot::{copy_screenshot_node, screenshot_error, CaptureJob, ScreenshotRequest};
use super::types::{
    AsrCue, AudioTrack, CommandReplyResult, EncodeContext, OsdDimensions, RawFrame, SubLine,
    SubtitleTrack, ASR_STREAM_URI,
};

const MPV_LOADFILE_INDEX_API_VERSION: u64 = (2 << 16) | 3;

impl MpvInstance {
    /// Snapshots copied from mpv's property notifications; these never wait
    /// for the core while the same thread is responsible for rendering.
    pub fn primary_sub_lines(&self) -> Vec<SubLine> {
        self.playback.borrow().sub_lines.clone()
    }

    pub fn audio_tracks(&self) -> Vec<AudioTrack> {
        self.playback.borrow().audio_tracks.clone()
    }

    pub fn subtitle_tracks(&self) -> Vec<SubtitleTrack> {
        self.playback.borrow().subtitle_tracks.clone()
    }

    pub fn supports_memory_subtitles(&self) -> bool {
        self.memory_subtitles_supported
    }

    /// Returns the native decoded dimensions of the current video stream, or
    /// (0, 0) if no video is playing.
    pub fn video_dimensions(&self) -> (u32, u32) {
        self.cached_video_dimensions()
    }

    pub fn capture_encode_context(&self) -> Option<EncodeContext> {
        let state = self.playback.borrow();
        (state.file_loaded && !state.path.is_empty()).then(|| EncodeContext {
            path: state.path.clone(),
            audio_id: state.audio_id,
        })
    }

    /// Queue the capture now, then consume it on a worker so mpv can continue
    /// receiving render updates while it waits for the current video frame.
    pub fn request_screenshot(&self, subtitles: bool) -> Result<ScreenshotRequest, String> {
        let (result, receiver) = mpsc::sync_channel(1);
        self.screenshot_jobs
            .as_ref()
            .ok_or("Player is closing")?
            .try_send(CaptureJob { subtitles, result })
            .map_err(|error| format!("Cannot queue screenshot: {error}"))?;
        Ok(ScreenshotRequest { result: receiver })
    }

    /// Capture the active video at source resolution without the software
    /// renderer's viewport scaling or any letterbox bars.
    pub fn screenshot_raw_rgba(&self, subtitles: bool) -> Result<RawFrame, String> {
        if self.is_opengl_active() {
            return Err("OpenGL screenshots must use request_screenshot on a worker".into());
        }
        let mut pointers = [
            c"screenshot-raw".as_ptr(),
            if subtitles {
                c"subtitles".as_ptr()
            } else {
                c"video".as_ptr()
            },
            c"rgba".as_ptr(),
            ptr::null(),
        ];
        let mut result = OwnedNode::new();
        let status =
            unsafe { ffi::mpv_command_ret(self.handle, pointers.as_mut_ptr(), &mut result.0) };
        if status < 0 {
            return Err(screenshot_error(status));
        }

        unsafe { copy_screenshot_node(&result.0) }
    }

    pub fn selected_aid(&self) -> i64 {
        self.playback.borrow().audio_id
    }

    pub fn primary_sid(&self) -> i64 {
        self.playback.borrow().primary_sid
    }

    pub fn secondary_sid(&self) -> i64 {
        self.playback.borrow().secondary_sid
    }

    pub fn get_sub_start(&self) -> f64 {
        self.playback.borrow().sub_start
    }

    pub fn get_sub_end(&self) -> f64 {
        self.playback.borrow().sub_end
    }

    pub fn get_sub_delay(&self) -> f64 {
        self.playback.borrow().sub_delay
    }

    pub fn get_sub_speed(&self) -> f64 {
        self.playback.borrow().sub_speed
    }

    pub fn get_sub_raw_time(&self, playback_time: f64) -> f64 {
        subtitle_raw_time(playback_time, self.get_sub_speed(), self.get_sub_delay())
    }

    pub fn get_secondary_sub_start(&self) -> f64 {
        self.playback.borrow().secondary_sub_start
    }

    pub fn get_secondary_sub_end(&self) -> f64 {
        self.playback.borrow().secondary_sub_end
    }

    pub fn get_secondary_sub_delay(&self) -> f64 {
        self.playback.borrow().secondary_sub_delay
    }

    pub fn get_secondary_sub_speed(&self) -> f64 {
        self.get_sub_speed()
    }

    pub fn get_secondary_sub_raw_time(&self, playback_time: f64) -> f64 {
        subtitle_raw_time(
            playback_time,
            self.get_secondary_sub_speed(),
            self.get_secondary_sub_delay(),
        )
    }

    fn set_track_property(&self, property: &CStr, id: i64) -> Result<(), String> {
        let status = unsafe {
            ffi::mpv_set_property_async(
                self.handle,
                0,
                property.as_ptr(),
                ffi::MPV_FORMAT_INT64,
                (&id as *const i64).cast_mut().cast(),
            )
        };
        if status < 0 {
            Err(format!(
                "mpv set {} property failed with code {status}",
                property.to_string_lossy()
            ))
        } else {
            Ok(())
        }
    }

    pub fn set_aid(&self, id: i64) -> Result<(), String> {
        self.set_track_property(c"aid", id)
    }

    pub fn set_sid(&self, id: i64) -> Result<(), String> {
        self.set_track_property(c"sid", id)
    }

    pub fn set_secondary_sid(&self, id: i64) -> Result<(), String> {
        self.set_track_property(c"secondary-sid", id)
    }

    /// Set mpv's `sub-scale` for native ASS/SSA rendering, enabling `sub-scale-signs`
    /// only when `scale != 1.0` so default `1.0` matches stock `mpv` identically.
    pub fn set_sub_scale(&self, scale: f64) -> Result<(), String> {
        let clamped = scale.clamp(0.1, 5.0);
        let scale_signs = if (clamped - 1.0).abs() > 1e-3 {
            "yes"
        } else {
            "no"
        };
        self.command_async(&["set", "sub-scale-signs", scale_signs])?;
        let val = format!("{clamped:.3}");
        self.command_async(&["set", "sub-scale", &val])
    }

    /// Set mpv's `sub-pos` (`0..=150`, default `100`) for native ASS/SSA vertical offset.
    pub fn set_sub_pos(&self, pos: f64) -> Result<(), String> {
        let clamped = pos.clamp(0.0, 150.0);
        let val = format!("{clamped:.2}");
        self.command_async(&["set", "sub-pos", &val])
    }

    pub fn sub_ass_extradata(&self) -> String {
        self.playback.borrow().sub_ass_extradata.clone()
    }

    pub fn sub_text_ass_full(&self) -> String {
        self.playback.borrow().sub_text_ass_full.clone()
    }

    pub fn osd_dimensions(&self) -> OsdDimensions {
        self.playback.borrow().osd_dimensions
    }

    pub fn sub_scale(&self) -> f64 {
        self.playback.borrow().sub_scale
    }

    pub fn sub_pos(&self) -> f64 {
        self.playback.borrow().sub_pos
    }

    /// Queue a command without blocking the render/UI thread on mpv's core.
    pub fn command_async(&self, args: &[&str]) -> Result<(), String> {
        let c_args: Result<Vec<CString>, _> = args.iter().map(|arg| CString::new(*arg)).collect();
        let c_args = c_args.map_err(|error| error.to_string())?;
        let mut pointers: Vec<*const c_char> = c_args.iter().map(|arg| arg.as_ptr()).collect();
        pointers.push(ptr::null());
        let status = unsafe { ffi::mpv_command_async(self.handle, 0, pointers.as_mut_ptr()) };
        if status < 0 {
            Err(format!("mpv_command_async failed with code {status}"))
        } else {
            Ok(())
        }
    }

    pub fn playlist_prev(&self) -> Result<(), String> {
        self.command_async(&["playlist-prev"])
    }

    pub fn playlist_next(&self) -> Result<(), String> {
        self.command_async(&["playlist-next"])
    }

    pub fn subtitle_seek(&self, count: i32, primary: bool) -> Result<(), String> {
        let count = count.to_string();
        self.command_async(&[
            "sub-seek",
            &count,
            if primary { "primary" } else { "secondary" },
        ])
    }

    /// Dispatch a structured command via `mpv_command_node_async` without
    /// blocking the render/UI thread. Returns the unique `reply_userdata`
    /// matched on `MpvEvent::CommandReply`.
    pub fn command_node_async(&self, args: &[&str]) -> Result<u64, String> {
        let c_args: Result<Vec<CString>, _> = args.iter().map(|arg| CString::new(*arg)).collect();
        let c_args = c_args.map_err(|error| error.to_string())?;
        let mut nodes: Vec<ffi::mpv_node> = c_args
            .iter()
            .map(|s| ffi::mpv_node {
                format: ffi::MPV_FORMAT_STRING,
                u: ffi::mpv_node__bindgen_ty_1 {
                    string: s.as_ptr().cast_mut(),
                },
            })
            .collect();
        let num = c_int::try_from(nodes.len())
            .map_err(|_| "mpv command argument list too long".to_owned())?;
        let mut list = ffi::mpv_node_list {
            num,
            values: nodes.as_mut_ptr(),
            keys: ptr::null_mut(),
        };
        let mut root = ffi::mpv_node {
            format: ffi::MPV_FORMAT_NODE_ARRAY,
            u: ffi::mpv_node__bindgen_ty_1 { list: &mut list },
        };
        let reply_id = self.next_reply_id.fetch_add(1, Ordering::Relaxed);
        let status = unsafe { ffi::mpv_command_node_async(self.handle, reply_id, &mut root) };
        if status < 0 {
            Err(format!(
                "mpv_command_node_async({}) failed with code {status}",
                args.first().copied().unwrap_or("command")
            ))
        } else {
            Ok(reply_id)
        }
    }

    fn wait_command_reply_i64(&self, reply_id: u64, command: &str) -> Result<i64, String> {
        if self.is_opengl_active() {
            return Err(format!(
                "Synchronous wait for `{command}` is unavailable on the OpenGL render thread; use {command}_async or AsrSubtitleTrack"
            ));
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                return Err(format!("mpv {command} timed out waiting for reply"));
            }
            let event = unsafe { ffi::mpv_wait_event(self.handle, remaining.as_secs_f64()) };
            let Some(raw) = (unsafe { event.as_ref() }) else {
                return Err(format!("mpv {command} received null event"));
            };
            if raw.event_id == ffi::MPV_EVENT_NONE {
                continue;
            }
            if let Some(decoded) = self.decode_one_event(raw) {
                match decoded {
                    MpvEvent::CommandReply {
                        reply_userdata,
                        error,
                        result,
                    } if reply_userdata == reply_id => {
                        if error < 0 {
                            return Err(format!("mpv {command} command failed with code {error}"));
                        }
                        return match result {
                            CommandReplyResult::Int64(id) if id_valid(id) => Ok(id),
                            _ => Err(format!("mpv {command} command returned no valid ID")),
                        };
                    }
                    MpvEvent::Shutdown => {
                        self.push_queued_event(decoded);
                        return Err(format!("Player shut down during {command}"));
                    }
                    other => self.push_queued_event(other),
                }
            }
        }
    }

    /// Replace the in-memory WebVTT buffer backing `memento-asr://`.
    pub fn set_asr_stream_bytes(&self, bytes: impl Into<Arc<[u8]>>) {
        let mut guard = self
            .asr_stream
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *guard = bytes.into();
    }

    /// Format sorted ASR cues into BOM-free UTF-8 WebVTT in RAM for `memento-asr://`.
    pub fn set_asr_stream_cues(&self, cues: &[AsrCue]) {
        let bytes = format_webvtt_cues(cues);
        self.set_asr_stream_bytes(bytes);
    }

    /// Add and select the in-memory `memento-asr://live.vtt` subtitle track via `sub-add`.
    pub fn add_asr_stream_track_async(&self, title: &str, language: &str) -> Result<u64, String> {
        if language.is_empty() {
            self.command_node_async(&["sub-add", ASR_STREAM_URI, "select", title])
        } else {
            self.command_node_async(&["sub-add", ASR_STREAM_URI, "select", title, language])
        }
    }

    /// Trigger an asynchronous `sub-reload` on the `memento-asr://` subtitle track.
    pub fn reload_asr_stream_track_async(&self, sid: Option<i64>) -> Result<u64, String> {
        match sid.filter(|&id| id > 0) {
            Some(id) => {
                let id_str = id.to_string();
                self.command_node_async(&["sub-reload", &id_str])
            }
            None => self.command_node_async(&["sub-reload"]),
        }
    }

    fn validate_memory_subtitle_cue(
        &self,
        track_id: i64,
        start: f64,
        end: f64,
        text: &str,
    ) -> Result<(), String> {
        if !self.supports_memory_subtitles() {
            return Err("mpv does not support in-memory subtitles".into());
        }
        if !id_valid(track_id) {
            return Err("invalid in-memory subtitle track ID".into());
        }
        if !cue_valid(start, end, text) {
            return Err("invalid in-memory subtitle cue".into());
        }
        Ok(())
    }

    /// Mode 2 non-blocking `sub-create` via `mpv_command_node_async`.
    pub fn create_memory_subtitle_track_async(
        &self,
        title: &str,
        language: &str,
    ) -> Result<u64, String> {
        if !self.supports_memory_subtitles() {
            return Err("mpv does not support in-memory subtitles".into());
        }
        if self.capture_encode_context().is_none() {
            return Err("no media file is loaded".into());
        }
        self.command_node_async(&["sub-create", title, language])
    }

    /// Mode 2 non-blocking `add-subtitle` via `mpv_command_node_async`.
    pub fn add_memory_subtitle_async(
        &self,
        track_id: i64,
        start: f64,
        end: f64,
        text: &str,
    ) -> Result<u64, String> {
        self.validate_memory_subtitle_cue(track_id, start, end, text)?;
        let track_id = track_id.to_string();
        let start = start.to_string();
        let end = end.to_string();
        self.command_node_async(&["add-subtitle", &track_id, &start, &end, text])
    }

    /// Mode 2 non-blocking `edit-subtitle` via `mpv_command_node_async`.
    pub fn edit_memory_subtitle_async(
        &self,
        track_id: i64,
        cue_id: i64,
        start: f64,
        end: f64,
        text: &str,
    ) -> Result<u64, String> {
        self.validate_memory_subtitle_cue(track_id, start, end, text)?;
        if !id_valid(cue_id) {
            return Err("invalid in-memory subtitle cue ID".into());
        }
        let track_id = track_id.to_string();
        let cue_id = cue_id.to_string();
        let start = start.to_string();
        let end = end.to_string();
        self.command_node_async(&["edit-subtitle", &track_id, &cue_id, &start, &end, text])
    }

    /// Mode 2 non-blocking `remove-subtitle` via `mpv_command_node_async`.
    pub fn remove_memory_subtitle_async(&self, track_id: i64, cue_id: i64) -> Result<u64, String> {
        if !self.supports_memory_subtitles() {
            return Err("mpv does not support in-memory subtitles".into());
        }
        if !id_valid(track_id) || !id_valid(cue_id) {
            return Err("invalid in-memory subtitle ID".into());
        }
        let track_id = track_id.to_string();
        let cue_id = cue_id.to_string();
        self.command_node_async(&["remove-subtitle", &track_id, &cue_id])
    }

    /// Mode 2 non-blocking `clear-subtitles` via `mpv_command_node_async`.
    pub fn clear_memory_subtitles_async(&self, track_id: i64) -> Result<u64, String> {
        if !self.supports_memory_subtitles() {
            return Err("mpv does not support in-memory subtitles".into());
        }
        if !id_valid(track_id) {
            return Err("invalid in-memory subtitle track ID".into());
        }
        let track_id = track_id.to_string();
        self.command_node_async(&["clear-subtitles", &track_id])
    }

    /// Mode 2 non-blocking `sub-remove` via `mpv_command_node_async`.
    pub fn remove_memory_subtitle_track_async(&self, track_id: i64) -> Result<u64, String> {
        if !self.supports_memory_subtitles() {
            return Err("mpv does not support in-memory subtitles".into());
        }
        if !id_valid(track_id) {
            return Err("invalid in-memory subtitle track ID".into());
        }
        let track_id = track_id.to_string();
        self.command_node_async(&["sub-remove", &track_id])
    }

    pub fn create_memory_subtitle_track(
        &self,
        title: &str,
        language: &str,
    ) -> Result<i64, String> {
        let reply_id = self.create_memory_subtitle_track_async(title, language)?;
        self.wait_command_reply_i64(reply_id, "sub-create")
    }

    pub fn add_memory_subtitle(
        &self,
        track_id: i64,
        start: f64,
        end: f64,
        text: &str,
    ) -> Result<i64, String> {
        let reply_id = self.add_memory_subtitle_async(track_id, start, end, text)?;
        self.wait_command_reply_i64(reply_id, "add-subtitle")
    }

    pub fn edit_memory_subtitle(
        &self,
        track_id: i64,
        cue_id: i64,
        start: f64,
        end: f64,
        text: &str,
    ) -> Result<(), String> {
        self.edit_memory_subtitle_async(track_id, cue_id, start, end, text)
            .map(|_| ())
    }

    pub fn remove_memory_subtitle(&self, track_id: i64, cue_id: i64) -> Result<(), String> {
        self.remove_memory_subtitle_async(track_id, cue_id)
            .map(|_| ())
    }

    pub fn clear_memory_subtitles(&self, track_id: i64) -> Result<(), String> {
        self.clear_memory_subtitles_async(track_id).map(|_| ())
    }

    pub fn remove_memory_subtitle_track(&self, track_id: i64) -> Result<(), String> {
        self.remove_memory_subtitle_track_async(track_id)
            .map(|_| ())
    }

    pub fn select_subtitle_track(&self, track_id: i64) -> Result<(), String> {
        self.set_sid(track_id)
    }

    pub fn raw_handle(&self) -> *mut ffi::mpv_handle {
        self.handle
    }

    /// Queues a command. Success means it was accepted; execution errors are
    /// reported through the player event queue, without blocking rendering.
    pub fn command(&self, args: &[&str]) -> Result<(), String> {
        self.command_async(args)
    }

    pub fn load_file(&self, path: &str) -> Result<(), String> {
        self.load_file_with_options(path, false, &[])
    }

    /// Queue `loadfile` with optional `append-play` mode and per-file mpv options.
    pub fn load_file_with_options(
        &self,
        path: &str,
        append: bool,
        options: &[String],
    ) -> Result<(), String> {
        // When `append == true` and media is already loaded, `append-play` queues
        // the file onto the playlist without replacing active playback.
        if !append {
            let mut state = self.playback.borrow_mut();
            state.file_loaded = false;
            state.playback_ready = false;
        }
        let mode = if append { "append-play" } else { "replace" };
        if options.is_empty() {
            self.command(&["loadfile", path, mode])
        } else {
            let joined = options.join(",");
            let api_version = u64::from(unsafe { ffi::mpv_client_api_version() });
            if api_version >= MPV_LOADFILE_INDEX_API_VERSION {
                self.command(&["loadfile", path, mode, "-1", &joined])
            } else {
                self.command(&["loadfile", path, mode, &joined])
            }
        }
    }

    pub fn play(&self) -> Result<(), String> {
        self.command_async(&["set", "pause", "no"])
    }

    pub fn pause(&self) -> Result<(), String> {
        self.command_async(&["set", "pause", "yes"])
    }

    pub fn toggle_pause(&self) -> Result<(), String> {
        self.command_async(&["cycle", "pause"])
    }

    pub fn is_paused(&self) -> bool {
        self.playback.borrow().paused
    }

    pub fn seek(&self, sec: f64) -> Result<(), String> {
        let clamped = if sec.is_finite() { sec.max(0.0) } else { 0.0 };
        let sec_str = format!("{clamped:.3}");
        self.command_async(&["seek", &sec_str, "absolute"])
    }

    pub fn seek_relative(&self, delta: f64) -> Result<(), String> {
        let safe = if delta.is_finite() { delta } else { 0.0 };
        let sec_str = format!("{safe:.3}");
        self.command_async(&["seek", &sec_str, "relative"])
    }

    pub fn set_volume(&self, vol: f64) -> Result<(), String> {
        let max_vol = {
            let vmax = self.playback.borrow().volume_max;
            if vmax > 0 {
                vmax as f64
            } else {
                100.0
            }
        };
        let clamped = if vol.is_finite() {
            vol.clamp(0.0, max_vol)
        } else {
            0.0
        };
        let vol_str = format!("{clamped:.1}");
        self.command_async(&["set", "volume", &vol_str])
    }

    pub fn get_volume(&self) -> f64 {
        self.playback.borrow().volume
    }

    pub fn get_position(&self) -> f64 {
        self.playback.borrow().position
    }

    pub fn get_duration(&self) -> f64 {
        self.playback.borrow().duration
    }

    pub fn get_sub_text(&self) -> Option<String> {
        let state = self.playback.borrow();
        (!state.sub_text.is_empty()).then(|| state.sub_text.clone())
    }

    /// Text decoded by the independently selected secondary subtitle track.
    pub fn get_secondary_sub_text(&self) -> Option<String> {
        let state = self.playback.borrow();
        (!state.secondary_sub_text.is_empty()).then(|| state.secondary_sub_text.clone())
    }

    pub fn get_media_title(&self) -> Option<String> {
        let state = self.playback.borrow();
        (!state.media_title.is_empty()).then(|| state.media_title.clone())
    }
}

//! Independent audio-only libmpv context for pronunciation playback.

use std::ffi::{c_char, CString};
use std::ptr;

use super::ffi;
use super::node::{iter_node_map, OwnedNode};

/// Independent audio-only libmpv context used for pronunciation playback.
///
/// Unlike [`super::MpvInstance`], this context has no video renderer and never
/// shares the movie player's handle. `play_file` matches the new load's
/// `playlist_entry_id` and waits until libmpv reports playback restart, so
/// callers can report completion only after the requested audio has started.
pub struct AudioInstance {
    handle: *mut ffi::mpv_handle,
}

impl AudioInstance {
    /// Creates a silent-window, audio-only mpv instance.
    pub fn new() -> Result<Self, String> {
        let handle = unsafe { ffi::mpv_create() };
        if handle.is_null() {
            return Err("failed to create audio mpv instance".into());
        }

        let initialize = || {
            for (name, value) in [
                (c"config", c"no"),
                (c"terminal", c"no"),
                (c"force-window", c"no"),
                (c"input-terminal", c"no"),
                (c"cover-art-auto", c"no"),
                (c"vid", c"no"),
            ] {
                let status = unsafe {
                    ffi::mpv_set_option_string(handle, name.as_ptr(), value.as_ptr())
                };
                if status < 0 {
                    return Err(format!(
                        "mpv audio option {} failed with code {status}",
                        name.to_string_lossy()
                    ));
                }
            }

            let status = unsafe { ffi::mpv_initialize(handle) };
            if status < 0 {
                return Err(format!("mpv audio initialization failed with code {status}"));
            }
            Ok(())
        };

        if let Err(error) = initialize() {
            unsafe { ffi::mpv_terminate_destroy(handle) };
            return Err(error);
        }
        Ok(Self { handle })
    }

    /// Loads a local audio file and waits for libmpv's playback-restart event
    /// matching this load's `playlist_entry_id`.
    pub fn play_file(&self, path: &std::path::Path) -> Result<(), String> {
        let path = path
            .to_str()
            .ok_or_else(|| format!("audio path is not valid UTF-8: {}", path.display()))?;
        self.discard_pending_events();
        let expected_entry_id = self.loadfile_returning_entry_id(path)?;

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        let mut active_entry_id: Option<i64> = None;
        loop {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                return Err("mpv did not start pronunciation playback within 30 seconds".into());
            }
            let event = unsafe {
                ffi::mpv_wait_event(self.handle, remaining.as_secs_f64().min(0.25))
            };
            let Some(raw) = (unsafe { event.as_ref() }) else {
                return Err("mpv returned a null audio event".into());
            };
            match raw.event_id {
                ffi::MPV_EVENT_START_FILE => {
                    let entry_id = unsafe {
                        raw.data.cast::<ffi::mpv_event_start_file>().as_ref()
                    }
                    .map_or(0, |info| info.playlist_entry_id);
                    if expected_entry_id.is_none_or(|expected| expected == entry_id) {
                        active_entry_id = Some(entry_id);
                    }
                }
                ffi::MPV_EVENT_PLAYBACK_RESTART if active_entry_id.is_some() => return Ok(()),
                ffi::MPV_EVENT_END_FILE => {
                    let entry_id = unsafe {
                        raw.data.cast::<ffi::mpv_event_end_file>().as_ref()
                    }
                    .map_or(0, |info| info.playlist_entry_id);
                    let matches_ours = active_entry_id == Some(entry_id)
                        || expected_entry_id.is_some_and(|expected| expected == entry_id);
                    if matches_ours {
                        return Err("mpv ended pronunciation audio before playback started".into());
                    }
                }
                ffi::MPV_EVENT_SHUTDOWN => {
                    return Err("audio mpv instance shut down before playback started".into())
                }
                _ => {}
            }
        }
    }

    fn loadfile_returning_entry_id(&self, path: &str) -> Result<Option<i64>, String> {
        let path_c = CString::new(path).map_err(|error| error.to_string())?;
        let mut pointers: [*const c_char; 4] = [
            c"loadfile".as_ptr(),
            path_c.as_ptr(),
            c"replace".as_ptr(),
            ptr::null(),
        ];
        let mut result = OwnedNode::new();
        let status = unsafe {
            ffi::mpv_command_ret(self.handle, pointers.as_mut_ptr(), &mut result.0)
        };
        if status < 0 {
            return Err(format!("mpv audio loadfile command failed with code {status}"));
        }
        let entry_id = unsafe { iter_node_map(&result.0) }.and_then(|mut map| {
            map.find_map(|(key, val)| {
                (key == b"playlist_entry_id" && val.format == ffi::MPV_FORMAT_INT64)
                    .then(|| unsafe { val.u.int64 })
            })
        });
        Ok(entry_id)
    }

    fn discard_pending_events(&self) {
        for _ in 0..1024 {
            let event = unsafe { ffi::mpv_wait_event(self.handle, 0.0) };
            if event.is_null() || unsafe { (*event).event_id } == ffi::MPV_EVENT_NONE {
                break;
            }
        }
    }
}

impl Drop for AudioInstance {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            unsafe { ffi::mpv_terminate_destroy(self.handle) };
            self.handle = ptr::null_mut();
        }
    }
}

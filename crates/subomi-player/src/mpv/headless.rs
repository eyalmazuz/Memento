//! Safe RAII wrappers for headless libmpv encoding instances and weak
//! non-blocking property observers.

use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::ptr::{self, NonNull};

use super::ffi;
use super::instance::MpvInstance;

/// Format a libmpv negative status code into a human-readable message.
pub fn mpv_error_message(status: c_int) -> String {
    // SAFETY: `mpv_error_string` returns a static NUL-terminated string or null.
    let message = unsafe { ffi::mpv_error_string(status) };
    if message.is_null() {
        format!("libmpv error {status}")
    } else {
        format!(
            "libmpv error: {}",
            unsafe { CStr::from_ptr(message) }.to_string_lossy()
        )
    }
}

/// Non-blocking observer of string properties backed by a weak libmpv client.
///
/// Always drains with `mpv_wait_event(0.0)` so it never deadlocks against
/// `MPV_RENDER_PARAM_ADVANCED_CONTROL`, and destroys its weak handle on drop.
pub struct WeakStringObserver {
    handle: Option<NonNull<ffi::mpv_handle>>,
}

impl WeakStringObserver {
    /// Create an inactive observer.
    pub fn disabled() -> Self {
        Self { handle: None }
    }

    /// Attach a weak client to `player` and observe `(reply_userdata, property_name)`
    /// string properties.
    pub fn attach(player: &MpvInstance, client_name: &CStr, properties: &[(u64, &CStr)]) -> Self {
        let raw_parent = player.raw_handle();
        if raw_parent.is_null() {
            return Self::disabled();
        }
        // SAFETY: `raw_parent` is a live `mpv_handle` owned by `player`, and
        // `client_name` / property names are valid NUL-terminated C strings.
        let Some(weak) = NonNull::new(unsafe {
            ffi::mpv_create_weak_client(raw_parent, client_name.as_ptr())
        }) else {
            return Self::disabled();
        };
        for &(id, name) in properties {
            unsafe {
                let _ = ffi::mpv_observe_property(
                    weak.as_ptr(),
                    id,
                    name.as_ptr(),
                    ffi::MPV_FORMAT_STRING,
                );
            }
        }
        Self { handle: Some(weak) }
    }

    pub fn is_active(&self) -> bool {
        self.handle.is_some()
    }

    /// Drain pending string property changes without blocking.
    pub fn poll(&mut self, mut on_change: impl FnMut(u64, Option<String>)) {
        let Some(handle) = self.handle else {
            return;
        };
        loop {
            // SAFETY: `handle` is live until `self.close()` runs below.
            let raw = unsafe {
                let event = ffi::mpv_wait_event(handle.as_ptr(), 0.0);
                match event.as_ref() {
                    Some(raw) => raw,
                    None => break,
                }
            };
            match raw.event_id {
                ffi::MPV_EVENT_NONE => break,
                ffi::MPV_EVENT_SHUTDOWN => {
                    self.close();
                    break;
                }
                ffi::MPV_EVENT_PROPERTY_CHANGE => {
                    // SAFETY: For `MPV_EVENT_PROPERTY_CHANGE`, `raw.data` points to an
                    // `mpv_event_property` valid until the next `mpv_wait_event` call.
                    let value = unsafe {
                        let Some(prop) = raw.data.cast::<ffi::mpv_event_property>().as_ref() else {
                            continue;
                        };
                        if prop.format == ffi::MPV_FORMAT_STRING && !prop.data.is_null() {
                            let ptr = *prop.data.cast::<*const c_char>();
                            if !ptr.is_null() {
                                let text = CStr::from_ptr(ptr).to_string_lossy().into_owned();
                                (!text.is_empty()).then_some(text)
                            } else {
                                None
                            }
                        } else {
                            None
                        }
                    };
                    on_change(raw.reply_userdata, value);
                }
                _ => {}
            }
        }
    }

    /// Destroy the weak libmpv handle early (before `MpvInstance::drop`).
    pub fn close(&mut self) {
        if let Some(handle) = self.handle.take() {
            // SAFETY: `handle` was created by `mpv_create_weak_client` and is owned uniquely.
            unsafe { ffi::mpv_destroy(handle.as_ptr()) };
        }
    }
}

impl Drop for WeakStringObserver {
    fn drop(&mut self) {
        self.close();
    }
}

/// Safe event summary returned by [`HeadlessMpv::wait_event`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HeadlessEvent {
    pub event_id: ffi::mpv_event_id,
    pub end_file: Option<(ffi::mpv_end_file_reason, c_int)>,
}

/// Owned headless `mpv_handle` for short-lived clip/audio encoding jobs.
pub struct HeadlessMpv {
    handle: NonNull<ffi::mpv_handle>,
}

impl HeadlessMpv {
    /// Create a fresh uninitialized libmpv handle.
    pub fn create() -> Option<Self> {
        // SAFETY: `mpv_create` takes no arguments and returns a new handle or null.
        NonNull::new(unsafe { ffi::mpv_create() }).map(|handle| Self { handle })
    }

    /// Set a scalar string option before [`Self::initialize`].
    pub fn set_option_string(&mut self, name: &str, value: &str) -> Result<(), String> {
        let name_c = CString::new(name).map_err(|_| format!("invalid option name '{name}'"))?;
        let value_c = CString::new(value.as_bytes())
            .map_err(|_| format!("option '{name}' contains an invalid NUL byte"))?;
        // SAFETY: `self.handle` is live and both CStrings are NUL-terminated.
        let status = unsafe {
            ffi::mpv_set_option_string(self.handle.as_ptr(), name_c.as_ptr(), value_c.as_ptr())
        };
        if status < 0 {
            Err(mpv_error_message(status))
        } else {
            Ok(())
        }
    }

    /// Set a string-list option (`MPV_FORMAT_NODE_ARRAY`) without delimiter
    /// splitting on `:` or `;`.
    pub fn set_string_list_option(&mut self, name: &str, values: &[&str]) -> Result<(), String> {
        if values.is_empty() {
            return Ok(());
        }
        let name_c = CString::new(name).map_err(|_| format!("invalid option name '{name}'"))?;
        let mut c_values = Vec::with_capacity(values.len());
        for value in values {
            let c_val = CString::new(value.as_bytes())
                .map_err(|_| format!("option '{name}' contains an invalid NUL byte"))?;
            c_values.push(c_val);
        }
        let num = c_int::try_from(c_values.len())
            .map_err(|_| format!("too many entries for option '{name}'"))?;

        // SAFETY: All `CString` pointers and `nodes`/`list` remain alive on the
        // stack for the entire synchronous `mpv_set_option` call.
        let status = unsafe {
            let mut nodes: Vec<ffi::mpv_node> = c_values
                .iter()
                .map(|s| {
                    let mut node: ffi::mpv_node = std::mem::zeroed();
                    node.format = ffi::MPV_FORMAT_STRING;
                    node.u.string = s.as_ptr().cast_mut();
                    node
                })
                .collect();
            let mut list = ffi::mpv_node_list {
                num,
                values: nodes.as_mut_ptr(),
                keys: ptr::null_mut(),
            };
            let mut array_node: ffi::mpv_node = std::mem::zeroed();
            array_node.format = ffi::MPV_FORMAT_NODE_ARRAY;
            array_node.u.list = &mut list;
            ffi::mpv_set_option(
                self.handle.as_ptr(),
                name_c.as_ptr(),
                ffi::MPV_FORMAT_NODE,
                (&mut array_node as *mut ffi::mpv_node).cast::<c_void>(),
            )
        };
        if status < 0 {
            Err(mpv_error_message(status))
        } else {
            Ok(())
        }
    }

    /// Finalize option setup and start the libmpv core.
    pub fn initialize(&mut self) -> Result<(), String> {
        // SAFETY: `self.handle` is a live handle owned by `self`.
        let status = unsafe { ffi::mpv_initialize(self.handle.as_ptr()) };
        if status < 0 {
            Err(mpv_error_message(status))
        } else {
            Ok(())
        }
    }

    /// Issue a `loadfile <path> replace [0] <options>` command respecting the
    /// runtime libmpv client API version.
    pub fn loadfile_replace(&mut self, media_path: &str, load_options: &str) -> Result<(), String> {
        let input = CString::new(media_path.as_bytes())
            .map_err(|_| "Media path contains an invalid NUL byte.".to_owned())?;
        let load_options_c = CString::new(load_options.as_bytes())
            .map_err(|_| "Load options contain an invalid NUL byte.".to_owned())?;
        // SAFETY: `mpv_client_api_version` is stateless; all command pointers
        // remain alive until `mpv_command` returns.
        let status = unsafe {
            let api_version = ffi::mpv_client_api_version();
            let mut command: Vec<*const c_char> =
                vec![c"loadfile".as_ptr(), input.as_ptr(), c"replace".as_ptr()];
            if api_version >= ((2 << 16) | 3) {
                command.push(c"0".as_ptr());
            }
            command.extend([load_options_c.as_ptr(), ptr::null()]);
            ffi::mpv_command(self.handle.as_ptr(), command.as_mut_ptr())
        };
        if status < 0 {
            Err(mpv_error_message(status))
        } else {
            Ok(())
        }
    }

    /// Wait up to `timeout_secs` for the next libmpv event.
    pub fn wait_event(&mut self, timeout_secs: f64) -> Option<HeadlessEvent> {
        // SAFETY: `self.handle` is live; the returned `mpv_event` pointer is
        // read synchronously before the next `mpv_wait_event` call.
        unsafe {
            let event = ffi::mpv_wait_event(self.handle.as_ptr(), timeout_secs);
            let raw = event.as_ref()?;
            #[allow(clippy::unnecessary_cast)]
            let end_file = if raw.event_id == ffi::MPV_EVENT_END_FILE {
                raw.data
                    .cast::<ffi::mpv_event_end_file>()
                    .as_ref()
                    .map(|info| (info.reason as ffi::mpv_end_file_reason, info.error))
            } else {
                None
            };
            Some(HeadlessEvent {
                event_id: raw.event_id,
                end_file,
            })
        }
    }
}

impl Drop for HeadlessMpv {
    fn drop(&mut self) {
        // SAFETY: `self.handle` was created by `mpv_create` and is uniquely owned.
        unsafe { ffi::mpv_terminate_destroy(self.handle.as_ptr()) };
    }
}

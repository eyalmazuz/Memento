//! Asynchronous raw RGBA screenshot worker backed by a weak libmpv client.

use std::cell::Cell;
use std::ffi::c_int;
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::sync::{Arc, Mutex};

use super::ffi;
use super::node::{copy_mpv_string, MpvNodeRef};
use super::types::RawFrame;

/// RAII guard that temporarily enables `sub-visibility` during a subtitle
/// screenshot (`subtitles == true`) and unconditionally restores the original
/// value on [`Drop`], even on error or panic.
///
/// When `subtitles == false` (video-only screenshot / OCR), this guard is a
/// complete no-op and never reads or modifies `sub-visibility`.
pub(super) struct SubVisibilityGuard<R: FnOnce(bool)> {
    original: bool,
    restore: Option<R>,
}

impl<R: FnOnce(bool)> SubVisibilityGuard<R> {
    pub(super) fn acquire<G, E>(subtitles: bool, get: G, enable: E, restore: R) -> Self
    where
        G: FnOnce() -> Option<bool>,
        E: FnOnce() -> bool,
    {
        if !subtitles {
            return Self {
                original: false,
                restore: None,
            };
        }
        match get() {
            Some(false) if enable() => Self {
                original: false,
                restore: Some(restore),
            },
            _ => Self {
                original: true,
                restore: None,
            },
        }
    }
}

impl<R: FnOnce(bool)> Drop for SubVisibilityGuard<R> {
    fn drop(&mut self) {
        if let Some(restore) = self.restore.take() {
            restore(self.original);
        }
    }
}

pub(super) unsafe fn get_sub_visibility(handle: *mut ffi::mpv_handle) -> Option<bool> {
    if handle.is_null() {
        return None;
    }
    let mut flag: c_int = 0;
    let status = unsafe {
        ffi::mpv_get_property(
            handle,
            c"sub-visibility".as_ptr(),
            ffi::MPV_FORMAT_FLAG,
            (&mut flag as *mut c_int).cast(),
        )
    };
    (status >= 0).then_some(flag != 0)
}

pub(super) unsafe fn set_sub_visibility(handle: *mut ffi::mpv_handle, visible: bool) -> bool {
    if handle.is_null() {
        return false;
    }
    let mut flag: c_int = c_int::from(visible);
    let status = unsafe {
        ffi::mpv_set_property(
            handle,
            c"sub-visibility".as_ptr(),
            ffi::MPV_FORMAT_FLAG,
            (&mut flag as *mut c_int).cast(),
        )
    };
    status >= 0
}

pub(super) unsafe fn set_sub_visibility_async(handle: *mut ffi::mpv_handle, visible: bool) {
    if handle.is_null() {
        return;
    }
    let flag: c_int = c_int::from(visible);
    let _ = unsafe {
        ffi::mpv_set_property_async(
            handle,
            0,
            c"sub-visibility".as_ptr(),
            ffi::MPV_FORMAT_FLAG,
            (&flag as *const c_int).cast_mut().cast(),
        )
    };
}

/// A queued capture. Wait on a worker, never on the UI/render thread: mpv
/// needs that thread to service its GPU screenshot request even while paused.
pub struct ScreenshotRequest {
    pub(super) result: Receiver<Result<RawFrame, String>>,
}

impl ScreenshotRequest {
    pub fn wait(self) -> Result<RawFrame, String> {
        self.result
            .recv()
            .map_err(|_| "Screenshot worker stopped".to_owned())?
    }
}

/// Thread-safe handle for requesting a screenshot lazily from a background
/// worker (e.g. `{screenshot-video | screenshot}` fallback) without keeping
/// the underlying channel sender alive past `MpvInstance::drop`.
#[derive(Clone)]
pub struct ScreenshotCapturer {
    pub(super) jobs: Arc<Mutex<Option<SyncSender<CaptureJob>>>>,
}

impl ScreenshotCapturer {
    pub fn request_screenshot(&self, subtitles: bool) -> Result<ScreenshotRequest, String> {
        let (result, receiver) = mpsc::sync_channel(1);
        let guard = self
            .jobs
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard
            .as_ref()
            .ok_or("Player is closing")?
            .try_send(CaptureJob { subtitles, result })
            .map_err(|error| format!("Cannot queue screenshot: {error}"))?;
        Ok(ScreenshotRequest { result: receiver })
    }
}

pub(super) struct CaptureJob {
    pub(super) subtitles: bool,
    pub(super) result: SyncSender<Result<RawFrame, String>>,
}

#[derive(Clone, Copy)]
pub(super) struct RawMpvHandle(pub(super) *mut ffi::mpv_handle);
unsafe impl Send for RawMpvHandle {}

/// Created before rendering starts, then owned exclusively by the capture
/// worker. Creating/destroying an mpv client can wait on the core too.
struct ScreenshotClient {
    handle: *mut ffi::mpv_handle,
    closing: Arc<AtomicBool>,
    wake_handle: Arc<Mutex<Option<RawMpvHandle>>>,
}

unsafe impl Send for ScreenshotClient {}

impl ScreenshotClient {
    fn capture(&self, subtitles: bool, id: u64) -> Result<RawFrame, String> {
        if self.closing.load(Ordering::Acquire) {
            return Err("Player closed during screenshot capture".into());
        }
        // When `subtitles == true`, temporarily force `sub-visibility = yes` if
        // currently disabled, and unconditionally restore it on drop (even on
        // error). When `subtitles == false`, never touch `sub-visibility`.
        let async_restore = Cell::new(false);
        let _sub_visibility_guard = SubVisibilityGuard::acquire(
            subtitles,
            || unsafe { get_sub_visibility(self.handle) },
            || unsafe { set_sub_visibility(self.handle, true) },
            |original| unsafe {
                if async_restore.get() || self.closing.load(Ordering::Acquire) {
                    set_sub_visibility_async(self.handle, original);
                } else {
                    set_sub_visibility(self.handle, original);
                }
            },
        );
        let mut args = [
            c"screenshot-raw".as_ptr(),
            if subtitles {
                c"subtitles".as_ptr()
            } else {
                c"video".as_ptr()
            },
            c"rgba".as_ptr(),
            ptr::null(),
        ];
        let status = unsafe { ffi::mpv_command_async(self.handle, id, args.as_mut_ptr()) };
        if status < 0 {
            return Err(screenshot_error(status));
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            if self.closing.load(Ordering::Acquire) {
                async_restore.set(true);
                return Err("Player closed during screenshot capture".into());
            }
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                async_restore.set(true);
                return Err("mpv screenshot timed out waiting for a video frame".into());
            }
            let event = unsafe { ffi::mpv_wait_event(self.handle, remaining.as_secs_f64()) };
            let Some(event) = (unsafe { event.as_ref() }) else {
                async_restore.set(true);
                return Err("mpv returned no screenshot event".into());
            };
            match event.event_id {
                ffi::MPV_EVENT_COMMAND_REPLY if event.reply_userdata == id => {
                    if event.error < 0 {
                        return Err(screenshot_error(event.error));
                    }
                    let result = unsafe { event.data.cast::<ffi::mpv_event_command>().as_ref() }
                        .ok_or("mpv screenshot returned no command result")?;
                    // The event owns the node. Copy pixels before polling again
                    // or destroying the client; do not free the node ourselves.
                    return unsafe { copy_screenshot_node(&result.result) };
                }
                ffi::MPV_EVENT_SHUTDOWN => {
                    async_restore.set(true);
                    return Err("Player closed during screenshot capture".into());
                }
                ffi::MPV_EVENT_QUEUE_OVERFLOW => {
                    async_restore.set(true);
                    return Err("mpv screenshot event queue overflowed".into());
                }
                ffi::MPV_EVENT_NONE if self.closing.load(Ordering::Acquire) => {
                    async_restore.set(true);
                    return Err("Player closed during screenshot capture".into());
                }
                _ => {}
            }
        }
    }
}

impl Drop for ScreenshotClient {
    fn drop(&mut self) {
        if let Ok(mut guard) = self.wake_handle.lock() {
            *guard = None;
        }
        unsafe { ffi::mpv_destroy(self.handle) };
    }
}

pub(super) struct ScreenshotWorkerHandles {
    pub(super) jobs: SyncSender<CaptureJob>,
    pub(super) closing: Arc<AtomicBool>,
    pub(super) wake_handle: Arc<Mutex<Option<RawMpvHandle>>>,
    pub(super) thread: std::thread::JoinHandle<()>,
}

pub(super) fn start_screenshot_worker(
    handle: *mut ffi::mpv_handle,
) -> Result<ScreenshotWorkerHandles, String> {
    let weak_handle =
        unsafe { ffi::mpv_create_weak_client(handle, c"subomi-screenshots".as_ptr()) };
    if weak_handle.is_null() {
        return Err("Could not create mpv screenshot client".into());
    }
    let closing = Arc::new(AtomicBool::new(false));
    let wake_handle = Arc::new(Mutex::new(Some(RawMpvHandle(weak_handle))));
    let client = ScreenshotClient {
        handle: weak_handle,
        closing: closing.clone(),
        wake_handle: wake_handle.clone(),
    };
    let (requests, jobs) = mpsc::sync_channel::<CaptureJob>(8);
    let thread = std::thread::Builder::new()
        .name("subomi-screenshots".into())
        .spawn(move || {
            let mut id = 0u64;
            while let Ok(job) = jobs.recv() {
                if client.closing.load(Ordering::Acquire) {
                    let _ = job
                        .result
                        .send(Err("Player closed during screenshot capture".into()));
                    break;
                }
                id = id.wrapping_add(1);
                let _ = job.result.send(client.capture(job.subtitles, id));
            }
        })
        .map_err(|error| format!("Could not start screenshot worker: {error}"))?;
    Ok(ScreenshotWorkerHandles {
        jobs: requests,
        closing,
        wake_handle,
        thread,
    })
}

pub(super) fn screenshot_error(status: i32) -> String {
    let message = copy_mpv_string(unsafe { ffi::mpv_error_string(status) });
    format!("mpv screenshot-raw failed: {message} ({status})")
}

/// `result` and its nested pointers must remain valid for the whole copy.
pub(super) unsafe fn copy_screenshot_node(result: &ffi::mpv_node) -> Result<RawFrame, String> {
    let node = unsafe { MpvNodeRef::from_ref(result) };
    let Some(fields) = node.as_map() else {
        return Err("mpv screenshot-raw returned no result map".into());
    };
    let mut width = None;
    let mut height = None;
    let mut stride = None;
    let mut is_rgba = false;
    let mut data: Option<&[u8]> = None;
    for (key, value) in fields {
        match key {
            b"w" => width = value.as_i64(),
            b"h" => height = value.as_i64(),
            b"stride" => stride = value.as_i64(),
            b"format" => is_rgba = value.as_str_bytes() == Some(b"rgba"),
            b"data" => data = value.as_byte_array(),
            _ => {}
        }
    }

    let min_line_size = crate::controller::ScreenshotSpec::validate(
        width.is_some(),
        width.unwrap_or(0),
        height.is_some(),
        height.unwrap_or(0),
        stride.is_some(),
        stride.unwrap_or(0),
        is_rgba.then_some("rgba"),
        data.is_some(),
    )
    .map_err(|error| format!("invalid mpv raw screenshot: {error:?}"))?;
    let width = usize::try_from(width.expect("validated width"))
        .map_err(|_| "mpv screenshot width exceeds addressable memory")?;
    let height = usize::try_from(height.expect("validated height"))
        .map_err(|_| "mpv screenshot height exceeds addressable memory")?;
    let stride = stride.expect("validated stride");
    let stride_abs = usize::try_from(stride.unsigned_abs())
        .map_err(|_| "mpv screenshot stride exceeds addressable memory")?;
    let line_size = usize::try_from(min_line_size)
        .map_err(|_| "mpv screenshot row exceeds addressable memory")?;
    let source_len = stride_abs
        .checked_mul(height - 1)
        .and_then(|offset| offset.checked_add(line_size))
        .ok_or("mpv screenshot source dimensions overflow")?;
    let output_len = line_size
        .checked_mul(height)
        .ok_or("mpv screenshot output dimensions overflow")?;
    let source = data.expect("validated screenshot data");
    if source.len() < source_len {
        return Err("mpv screenshot data is shorter than its dimensions and stride".into());
    }

    let mut pixels: Vec<u8> = Vec::new();
    pixels
        .try_reserve_exact(output_len)
        .map_err(|error| format!("could not allocate raw screenshot: {error}"))?;
    for output_row in 0..height {
        let source_row = if stride < 0 {
            height - 1 - output_row
        } else {
            output_row
        };
        let source_offset = source_row * stride_abs;
        pixels.extend_from_slice(&source[source_offset..source_offset + line_size]);
    }
    Ok(RawFrame {
        width: width as u32,
        height: height as u32,
        pixels,
    })
}

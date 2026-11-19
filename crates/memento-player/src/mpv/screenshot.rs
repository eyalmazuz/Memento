//! Asynchronous raw RGBA screenshot worker backed by a weak libmpv client.

use std::ffi::CStr;
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::sync::{Arc, Mutex};

use super::ffi;
use super::node::{copy_mpv_string, iter_node_map};
use super::types::RawFrame;

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
                return Err("Player closed during screenshot capture".into());
            }
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                return Err("mpv screenshot timed out waiting for a video frame".into());
            }
            let event = unsafe { ffi::mpv_wait_event(self.handle, remaining.as_secs_f64()) };
            let Some(event) = (unsafe { event.as_ref() }) else {
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
                    return Err("Player closed during screenshot capture".into())
                }
                ffi::MPV_EVENT_QUEUE_OVERFLOW => {
                    return Err("mpv screenshot event queue overflowed".into())
                }
                ffi::MPV_EVENT_NONE if self.closing.load(Ordering::Acquire) => {
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
        unsafe { ffi::mpv_create_weak_client(handle, c"memento-screenshots".as_ptr()) };
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
        .name("memento-screenshots".into())
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
    let Some(fields) = (unsafe { iter_node_map(result) }) else {
        return Err("mpv screenshot-raw returned no result map".into());
    };
    let mut width = None;
    let mut height = None;
    let mut stride = None;
    let mut is_rgba = false;
    let mut data = None;
    for (key, value) in fields {
        match key {
            b"w" if value.format == ffi::MPV_FORMAT_INT64 => {
                width = Some(unsafe { value.u.int64 });
            }
            b"h" if value.format == ffi::MPV_FORMAT_INT64 => {
                height = Some(unsafe { value.u.int64 });
            }
            b"stride" if value.format == ffi::MPV_FORMAT_INT64 => {
                stride = Some(unsafe { value.u.int64 });
            }
            b"format" if value.format == ffi::MPV_FORMAT_STRING => {
                let format = unsafe { value.u.string };
                is_rgba = !format.is_null()
                    && unsafe { CStr::from_ptr(format) }.to_bytes() == b"rgba";
            }
            b"data" if value.format == ffi::MPV_FORMAT_BYTE_ARRAY => {
                if let Some(bytes) = unsafe { value.u.ba.as_ref() } {
                    data = Some((bytes.data.cast::<u8>() as *const u8, bytes.size));
                }
            }
            _ => {}
        }
    }

    let has_data = data.is_some_and(|(pointer, _)| !pointer.is_null());
    let min_line_size = crate::controller::ScreenshotSpec::validate(
        width.is_some(),
        width.unwrap_or(0),
        height.is_some(),
        height.unwrap_or(0),
        stride.is_some(),
        stride.unwrap_or(0),
        is_rgba.then_some("rgba"),
        has_data,
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
    let (source, source_size) = data.expect("validated screenshot data");
    if source_size < source_len {
        return Err("mpv screenshot data is shorter than its dimensions and stride".into());
    }

    let mut pixels: Vec<u8> = Vec::new();
    pixels
        .try_reserve_exact(output_len)
        .map_err(|error| format!("could not allocate raw screenshot: {error}"))?;
    unsafe { pixels.set_len(output_len) };
    for output_row in 0..height {
        let source_row = if stride < 0 {
            height - 1 - output_row
        } else {
            output_row
        };
        let source_offset = source_row * stride_abs;
        let output_offset = output_row * line_size;
        unsafe {
            ptr::copy_nonoverlapping(
                source.add(source_offset),
                pixels.as_mut_ptr().add(output_offset),
                line_size,
            );
        }
    }
    Ok(RawFrame {
        width: width as u32,
        height: height as u32,
        pixels,
    })
}

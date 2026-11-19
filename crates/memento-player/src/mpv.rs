//! libmpv bindings for the movie player's renderer-backed handle and an
//! independent audio-only pronunciation handle. Both use the same SDK header
//! as the C++ player; audio playback never borrows or replaces the movie handle.

use std::cell::{Cell, Ref, RefCell};
use std::collections::{HashMap, HashSet, VecDeque};
use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::Path;
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::sync::mpsc::{self, Receiver, SyncSender};

use crate::controller::CliOption;
use crate::events::{EngineEvent, PropertyChange, PropertyName, Track};

#[allow(dead_code, non_camel_case_types, non_snake_case, non_upper_case_globals)]
pub mod ffi {
    include!(concat!(env!("OUT_DIR"), "/mpv_bindings.rs"));
}
pub use ffi::mpv_handle;

const MAX_TEXT_BYTES: usize = 1024 * 1024;
const REQUIRED_COMMANDS: u8 = 0b11_1111;

/// Checks the same double/millisecond and QByteArray bounds as
/// `subtitleCueIsValid`. A nonempty text buffer must point to `len` readable
/// bytes; unlike an empty QByteArray, a null pointer with nonzero length is
/// invalid. No C string terminator is required or read.
#[no_mangle]
pub unsafe extern "C" fn memento_player_cue_valid(
    start: f64,
    end: f64,
    text: *const u8,
    len: usize,
) -> bool {
    if !start.is_finite()
        || !end.is_finite()
        || start < 0.0
        || end <= start
        || end * 1000.0 > f64::from(i32::MAX)
        || len > MAX_TEXT_BYTES
        || (len != 0 && text.is_null())
    {
        return false;
    }
    len == 0 || !unsafe { std::slice::from_raw_parts(text, len) }.contains(&0)
}

#[no_mangle]
pub extern "C" fn memento_player_id_valid(id: i64) -> bool {
    id > 0 && id <= i64::from(i32::MAX)
}

/// `handle` is borrowed from the C++ player; `args` must be a valid
/// null-terminated list of valid C strings for the duration of the call.
/// mpv's `const char **` declaration does not modify those input pointers;
/// the C bridge's `const char *const *` preserves constness for its callers.
#[no_mangle]
pub unsafe extern "C" fn memento_player_command(
    handle: *mut mpv_handle,
    args: *const *const c_char,
) -> c_int {
    if handle.is_null() || args.is_null() {
        return ffi::MPV_ERROR_INVALID_PARAMETER;
    }
    unsafe { ffi::mpv_command(handle, args.cast_mut()) }
}

/// Same borrowing contract as `memento_player_command`; the caller-owned mpv
/// handle receives the asynchronous command reply under `reply_userdata`.
#[no_mangle]
pub unsafe extern "C" fn memento_player_command_async(
    handle: *mut mpv_handle,
    reply_userdata: u64,
    args: *const *const c_char,
) -> c_int {
    if handle.is_null() || args.is_null() {
        return ffi::MPV_ERROR_INVALID_PARAMETER;
    }
    unsafe { ffi::mpv_command_async(handle, reply_userdata, args.cast_mut()) }
}

/// Extracts a positive, int-bounded ID from the SDK's returned mpv_node.
/// `out` is set to -1 before every possible failure; mpv-owned node contents
/// are released even when mpv reports an error.
#[no_mangle]
pub unsafe extern "C" fn memento_player_command_ret_i64(
    handle: *mut mpv_handle,
    args: *const *const c_char,
    out: *mut i64,
) -> c_int {
    if out.is_null() {
        return ffi::MPV_ERROR_INVALID_PARAMETER;
    }
    unsafe { *out = -1 };
    if handle.is_null() || args.is_null() {
        return ffi::MPV_ERROR_INVALID_PARAMETER;
    }

    let mut result = OwnedNode::new();
    let status = unsafe { ffi::mpv_command_ret(handle, args.cast_mut(), &mut result.0) };
    if status < 0 {
        return status;
    }
    if result.0.format != ffi::MPV_FORMAT_INT64 {
        return ffi::MPV_ERROR_COMMAND;
    }
    let id = unsafe { result.0.u.int64 };
    if !memento_player_id_valid(id) {
        return ffi::MPV_ERROR_COMMAND;
    }
    unsafe { *out = id };
    status
}

/// Thin forwarder for the two node-returning command sites. Unlike
/// [`memento_player_command_ret_i64`] it performs no post-validation and
/// releases no node contents — identical semantics to calling
/// `mpv_command_ret` directly, so the C++ sites keep their exact warnings,
/// `mpv_free_node_contents` cleanup, and postconditions.
#[no_mangle]
pub unsafe extern "C" fn memento_player_command_ret_raw(
    handle: *mut mpv_handle,
    args: *const *const c_char,
    out: *mut ffi::mpv_node,
) -> c_int {
    if handle.is_null() || args.is_null() || out.is_null() {
        return ffi::MPV_ERROR_INVALID_PARAMETER;
    }
    unsafe { ffi::mpv_command_ret(handle, args.cast_mut(), out) }
}

/// Checks the command list of the *existing* mpv instance, matching the
/// in-memory subtitle feature probe in MpvController. Traverses the returned
/// array once, looking up the `name` field in each command's node map.
#[no_mangle]
pub unsafe extern "C" fn memento_player_supports_memory_subtitles(
    handle: *mut mpv_handle,
) -> bool {
    if handle.is_null() {
        return false;
    }
    let mut result = OwnedNode::new();
    let status = unsafe {
        ffi::mpv_get_property(
            handle,
            c"command-list".as_ptr(),
            ffi::MPV_FORMAT_NODE,
            (&mut result.0 as *mut ffi::mpv_node).cast(),
        )
    };
    if status < 0 || result.0.format != ffi::MPV_FORMAT_NODE_ARRAY {
        return false;
    }
    let commands = unsafe { result.0.u.list.as_ref() };
    let Some(commands) = commands else {
        return false;
    };
    if commands.num <= 0 || commands.values.is_null() {
        return false;
    }

    let mut found = 0u8;
    for index in 0..commands.num as usize {
        let entry = unsafe { &*commands.values.add(index) };
        found |= match unsafe { command_name(entry) } {
            Some(b"sub-create") => 1 << 0,
            Some(b"add-subtitle") => 1 << 1,
            Some(b"edit-subtitle") => 1 << 2,
            Some(b"remove-subtitle") => 1 << 3,
            Some(b"clear-subtitles") => 1 << 4,
            Some(b"sub-remove") => 1 << 5,
            _ => 0,
        };
        if found == REQUIRED_COMMANDS {
            return true;
        }
    }
    false
}

/// A zero-initialized node is MPV_FORMAT_NONE. mpv_free_node_contents is safe
/// for that format, even if an mpv call failed without writing to the node.
struct OwnedNode(ffi::mpv_node);

impl OwnedNode {
    fn new() -> Self {
        Self(unsafe { std::mem::zeroed() })
    }
}

impl Drop for OwnedNode {
    fn drop(&mut self) {
        unsafe { ffi::mpv_free_node_contents(&mut self.0) }
    }
}

/// Helper to iterate through a node array. Returns an iterator over safe `&ffi::mpv_node` references.
/// Returns None if the node is null, not an array, or if `u.list` or `values` is null or `num <= 0`.
unsafe fn iter_node_array<'a>(node: *const ffi::mpv_node) -> Option<impl Iterator<Item = &'a ffi::mpv_node>> {
    if node.is_null() {
        return None;
    }
    let node_ref = unsafe { &*node };
    if node_ref.format != ffi::MPV_FORMAT_NODE_ARRAY {
        return None;
    }
    let list = unsafe { node_ref.u.list.as_ref() }?;
    if list.num <= 0 || list.values.is_null() {
        return None;
    }
    let slice = unsafe { std::slice::from_raw_parts(list.values, list.num as usize) };
    Some(slice.iter())
}

/// Helper to iterate through a node map. Returns an iterator over `(&'a [u8], &'a ffi::mpv_node)`.
/// Returns None if the node is not a map, or if `u.list`, `values`, or `keys` is null or `num <= 0`.
unsafe fn iter_node_map<'a>(node: &'a ffi::mpv_node) -> Option<impl Iterator<Item = (&'a [u8], &'a ffi::mpv_node)>> {
    if node.format != ffi::MPV_FORMAT_NODE_MAP {
        return None;
    }
    let list = unsafe { node.u.list.as_ref() }?;
    if list.num <= 0 || list.values.is_null() || list.keys.is_null() {
        return None;
    }
    let keys_slice = unsafe { std::slice::from_raw_parts(list.keys, list.num as usize) };
    let values_slice = unsafe { std::slice::from_raw_parts(list.values, list.num as usize) };
    Some(keys_slice.iter().zip(values_slice.iter()).filter_map(|(&k, v)| {
        if k.is_null() {
            None
        } else {
            let key_bytes = unsafe { CStr::from_ptr(k) }.to_bytes();
            Some((key_bytes, v))
        }
    }))
}

/// The returned bytes borrow mpv-owned node memory and must not outlive it.
unsafe fn command_name(node: &ffi::mpv_node) -> Option<&[u8]> {
    let map = iter_node_map(node)?;
    for (key, val) in map {
        if key == b"name" {
            if val.format != ffi::MPV_FORMAT_STRING {
                return None;
            }
            let name = unsafe { val.u.string };
            return if name.is_null() {
                None
            } else {
                Some(unsafe { CStr::from_ptr(name) }.to_bytes())
            };
        }
    }
    None
}

/// Track type corresponding to MpvTrack::Type in C++.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(i32)]
pub enum TrackType {
    #[default]
    None = 0,
    Audio = 1,
    Video = 2,
    Subtitle = 3,
}

impl TrackType {
    pub fn from_bytes(bytes: &[u8]) -> Self {
        match bytes {
            b"audio" => Self::Audio,
            b"video" => Self::Video,
            b"sub" => Self::Subtitle,
            _ => Self::None,
        }
    }
}
/// Owned metadata for an audio track in mpv's `track-list`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioTrack {
    pub id: i64,
    pub language: String,
    pub title: String,
    pub external_filename: String,
    pub external: bool,
    pub default_track: bool,
    pub selected: bool,
}

/// Owned metadata for a subtitle track in mpv's `track-list`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubtitleTrack {
    pub id: i64,
    pub external_path: String,
    pub language: String,
    pub title: String,
    pub external: bool,
    pub default_track: bool,
    pub selected: bool,
}

/// Snapshot of the currently loaded media needed by an off-thread encoder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncodeContext {
    pub path: String,
    pub audio_id: i64,
}

/// Owned, source-resolution RGBA pixels returned by mpv's `screenshot-raw`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawFrame {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

/// Idiomatic Rust representation of track properties extracted from an mpv_node.
#[derive(Debug, Clone, PartialEq)]
pub struct ExtractedTrack {
    pub id: i64,
    pub track_type: TrackType,
    pub src_id: i64,
    pub title: *const c_char,
    pub lang: *const c_char,
    pub albumart: bool,
    pub default_track: bool,
    pub selected: bool,
    pub main_selection: i64,
    pub external: bool,
    pub external_filename: *const c_char,
    pub codec: *const c_char,
}

impl Default for ExtractedTrack {
    fn default() -> Self {
        Self {
            id: 0,
            track_type: TrackType::None,
            src_id: 0,
            title: c"".as_ptr(),
            lang: c"".as_ptr(),
            albumart: false,
            default_track: false,
            selected: false,
            main_selection: 0,
            external: false,
            external_filename: c"".as_ptr(),
            codec: c"".as_ptr(),
        }
    }
}

/// C ABI representation of a track item.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MementoTrackC {
    pub id: i64,
    pub track_type: i32,
    pub src_id: i64,
    pub title: *const c_char,
    pub lang: *const c_char,
    pub albumart: bool,
    pub default_track: bool,
    pub selected: bool,
    pub main_selection: i64,
    pub external: bool,
    pub external_filename: *const c_char,
    pub codec: *const c_char,
}

impl From<ExtractedTrack> for MementoTrackC {
    fn from(t: ExtractedTrack) -> Self {
        Self {
            id: t.id,
            track_type: t.track_type as i32,
            src_id: t.src_id,
            title: t.title,
            lang: t.lang,
            albumart: t.albumart,
            default_track: t.default_track,
            selected: t.selected,
            main_selection: t.main_selection,
            external: t.external,
            external_filename: t.external_filename,
            codec: t.codec,
        }
    }
}

/// C ABI list of tracks.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct MementoTrackListC {
    pub items: *const MementoTrackC,
    pub len: usize,
    pub capacity: usize,
}

/// Extracts track properties matching `MpvState::setTracks`.
#[no_mangle]
pub unsafe extern "C" fn memento_player_extract_tracks(
    node: *const ffi::mpv_node,
) -> MementoTrackListC {
    let Some(array_iter) = (unsafe { iter_node_array(node) }) else {
        return MementoTrackListC {
            items: std::ptr::null(),
            len: 0,
            capacity: 0,
        };
    };

    let mut tracks: Vec<MementoTrackC> = Vec::new();
    for entry in array_iter {
        let Some(map_iter) = (unsafe { iter_node_map(entry) }) else {
            continue;
        };

        let mut track = ExtractedTrack::default();
        for (key, val) in map_iter {
            match key {
                b"id" => {
                    if val.format == ffi::MPV_FORMAT_INT64 {
                        track.id = unsafe { val.u.int64 };
                    }
                }
                b"type" => {
                    if val.format == ffi::MPV_FORMAT_STRING {
                        let s = unsafe { val.u.string };
                        if !s.is_null() {
                            let type_bytes = unsafe { CStr::from_ptr(s) }.to_bytes();
                            track.track_type = TrackType::from_bytes(type_bytes);
                        }
                    }
                }
                b"src-id" => {
                    if val.format == ffi::MPV_FORMAT_INT64 {
                        track.src_id = unsafe { val.u.int64 };
                    }
                }
                b"title" => {
                    if val.format == ffi::MPV_FORMAT_STRING {
                        let s = unsafe { val.u.string };
                        track.title = if s.is_null() { c"".as_ptr() } else { s };
                    }
                }
                b"lang" => {
                    if val.format == ffi::MPV_FORMAT_STRING {
                        let s = unsafe { val.u.string };
                        track.lang = if s.is_null() { c"".as_ptr() } else { s };
                    }
                }
                b"albumart" => {
                    if val.format == ffi::MPV_FORMAT_FLAG {
                        track.albumart = unsafe { val.u.flag != 0 };
                    }
                }
                b"default" => {
                    if val.format == ffi::MPV_FORMAT_FLAG {
                        track.default_track = unsafe { val.u.flag != 0 };
                    }
                }
                b"selected" => {
                    if val.format == ffi::MPV_FORMAT_FLAG {
                        track.selected = unsafe { val.u.flag != 0 };
                    }
                }
                b"main-selection" => {
                    if val.format == ffi::MPV_FORMAT_INT64 {
                        track.main_selection = unsafe { val.u.int64 };
                    }
                }
                b"external" => {
                    if val.format == ffi::MPV_FORMAT_FLAG {
                        track.external = unsafe { val.u.flag != 0 };
                    }
                }
                b"external-filename" => {
                    if val.format == ffi::MPV_FORMAT_STRING {
                        let s = unsafe { val.u.string };
                        track.external_filename = if s.is_null() { c"".as_ptr() } else { s };
                    }
                }
                b"codec" => {
                    if val.format == ffi::MPV_FORMAT_STRING {
                        let s = unsafe { val.u.string };
                        track.codec = if s.is_null() { c"".as_ptr() } else { s };
                    }
                }
                _ => {}
            }
        }
        tracks.push(track.into());
    }

    if tracks.is_empty() {
        MementoTrackListC {
            items: std::ptr::null(),
            len: 0,
            capacity: 0,
        }
    } else {
        tracks.shrink_to_fit();
        let len = tracks.len();
        let capacity = tracks.capacity();
        let items = tracks.as_ptr();
        std::mem::forget(tracks);
        MementoTrackListC { items, len, capacity }
    }
}

/// Frees a track list allocated by `memento_player_extract_tracks`.
#[no_mangle]
pub unsafe extern "C" fn memento_player_tracks_free(list: MementoTrackListC) {
    if !list.items.is_null() && list.capacity > 0 {
        drop(unsafe { Vec::from_raw_parts(list.items as *mut MementoTrackC, list.len, list.capacity) });
    }
}

struct OwnedTrackList(MementoTrackListC);

impl OwnedTrackList {
    fn as_slice(&self) -> &[MementoTrackC] {
        if self.0.len == 0 || self.0.items.is_null() {
            &[]
        } else {
            unsafe { std::slice::from_raw_parts(self.0.items, self.0.len) }
        }
    }
}

impl Drop for OwnedTrackList {
    fn drop(&mut self) {
        unsafe { memento_player_tracks_free(self.0) };
    }
}

fn copy_mpv_string(value: *const c_char) -> String {
    if value.is_null() {
        String::new()
    } else {
        unsafe { CStr::from_ptr(value) }.to_string_lossy().into_owned()
    }
}

/// A queued capture. Wait on a worker, never on the UI/render thread: mpv
/// needs that thread to service its GPU screenshot request even while paused.
pub struct ScreenshotRequest {
    result: Receiver<Result<RawFrame, String>>,
}

impl ScreenshotRequest {
    pub fn wait(self) -> Result<RawFrame, String> {
        self.result.recv().map_err(|_| "Screenshot worker stopped".to_owned())?
    }
}

struct CaptureJob {
    subtitles: bool,
    result: SyncSender<Result<RawFrame, String>>,
}

#[derive(Clone, Copy)]
struct RawMpvHandle(*mut ffi::mpv_handle);
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
            if subtitles { c"subtitles".as_ptr() } else { c"video".as_ptr() },
            c"rgba".as_ptr(),
            ptr::null(),
        ];
        let status = unsafe { ffi::mpv_command_async(self.handle, id, args.as_mut_ptr()) };
        if status < 0 { return Err(screenshot_error(status)); }
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
                    if event.error < 0 { return Err(screenshot_error(event.error)); }
                    let result = unsafe { event.data.cast::<ffi::mpv_event_command>().as_ref() }
                        .ok_or("mpv screenshot returned no command result")?;
                    // The event owns the node. Copy pixels before polling again
                    // or destroying the client; do not free the node ourselves.
                    return unsafe { copy_screenshot_node(&result.result) };
                }
                ffi::MPV_EVENT_SHUTDOWN => return Err("Player closed during screenshot capture".into()),
                ffi::MPV_EVENT_QUEUE_OVERFLOW => return Err("mpv screenshot event queue overflowed".into()),
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

struct ScreenshotWorkerHandles {
    jobs: SyncSender<CaptureJob>,
    closing: Arc<AtomicBool>,
    wake_handle: Arc<Mutex<Option<RawMpvHandle>>>,
    thread: std::thread::JoinHandle<()>,
}

fn start_screenshot_worker(handle: *mut ffi::mpv_handle) -> Result<ScreenshotWorkerHandles, String> {
    let weak_handle = unsafe { ffi::mpv_create_weak_client(handle, c"memento-screenshots".as_ptr()) };
    if weak_handle.is_null() { return Err("Could not create mpv screenshot client".into()); }
    let closing = Arc::new(AtomicBool::new(false));
    let wake_handle = Arc::new(Mutex::new(Some(RawMpvHandle(weak_handle))));
    let client = ScreenshotClient {
        handle: weak_handle,
        closing: closing.clone(),
        wake_handle: wake_handle.clone(),
    };
    let (requests, jobs) = mpsc::sync_channel::<CaptureJob>(8);
    let thread = std::thread::Builder::new().name("memento-screenshots".into()).spawn(move || {
        let mut id = 0u64;
        while let Ok(job) = jobs.recv() {
            if client.closing.load(Ordering::Acquire) {
                let _ = job.result.send(Err("Player closed during screenshot capture".into()));
                break;
            }
            // Unique IDs prevent a timed-out command's late reply from being
            // mistaken for the next capture. No render/UI state is borrowed.
            id = id.wrapping_add(1);
            let _ = job.result.send(client.capture(job.subtitles, id));
        }
    }).map_err(|error| format!("Could not start screenshot worker: {error}"))?;
    Ok(ScreenshotWorkerHandles {
        jobs: requests,
        closing,
        wake_handle,
        thread,
    })
}

fn screenshot_error(status: i32) -> String {
    let message = copy_mpv_string(unsafe { ffi::mpv_error_string(status) });
    format!("mpv screenshot-raw failed: {message} ({status})")
}

/// `result` and its nested pointers must remain valid for the whole copy.
unsafe fn copy_screenshot_node(result: &ffi::mpv_node) -> Result<RawFrame, String> {
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

/// C ABI list of f64 values (e.g. chapter timestamps).
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct MementoF64ListC {
    pub items: *const f64,
    pub len: usize,
    pub capacity: usize,
}

/// Extracts chapter time list matching `MpvPlayer` "chapter-list" handler.
#[no_mangle]
pub unsafe extern "C" fn memento_player_extract_chapter_times(
    node: *const ffi::mpv_node,
) -> MementoF64ListC {
    let Some(array_iter) = (unsafe { iter_node_array(node) }) else {
        return MementoF64ListC {
            items: std::ptr::null(),
            len: 0,
            capacity: 0,
        };
    };

    let mut chapters: Vec<f64> = Vec::new();
    for entry in array_iter {
        let Some(map_iter) = (unsafe { iter_node_map(entry) }) else {
            continue;
        };
        for (key, val) in map_iter {
            if key == b"time" && val.format == ffi::MPV_FORMAT_DOUBLE {
                chapters.push(unsafe { val.u.double_ });
            }
        }
    }

    if chapters.is_empty() {
        MementoF64ListC {
            items: std::ptr::null(),
            len: 0,
            capacity: 0,
        }
    } else {
        chapters.shrink_to_fit();
        let len = chapters.len();
        let capacity = chapters.capacity();
        let items = chapters.as_ptr();
        std::mem::forget(chapters);
        MementoF64ListC { items, len, capacity }
    }
}

/// Frees a double list allocated by `memento_player_extract_chapter_times`.
#[no_mangle]
pub unsafe extern "C" fn memento_player_f64_list_free(list: MementoF64ListC) {
    if !list.items.is_null() && list.capacity > 0 {
        drop(unsafe { Vec::from_raw_parts(list.items as *mut f64, list.len, list.capacity) });
    }
}

/// Idiomatic Rust representation of an extracted subtitle line.
#[derive(Debug, Clone, PartialEq)]
pub struct ExtractedSubLine {
    pub id: i64,
    pub text: *const c_char,
    pub start: f64,
    pub end: f64,
}

/// Owned subtitle cue copied from mpv's live `sub-lines` property.
#[derive(Debug, Clone, PartialEq)]
pub struct SubLine {
    pub id: i64,
    pub text: String,
    pub start: f64,
    pub end: f64,
}

/// C ABI representation of a subtitle line.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MementoSubLineC {
    pub id: i64,
    pub text: *const c_char,
    pub start: f64,
    pub end: f64,
}

impl From<ExtractedSubLine> for MementoSubLineC {
    fn from(s: ExtractedSubLine) -> Self {
        Self {
            id: s.id,
            text: s.text,
            start: s.start,
            end: s.end,
        }
    }
}

/// C ABI list of subtitle lines.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct MementoSubLineListC {
    pub items: *const MementoSubLineC,
    pub len: usize,
    pub capacity: usize,
}

/// Extracts subtitle lines matching `MpvPlayer` "sub-lines" handler.
#[no_mangle]
pub unsafe extern "C" fn memento_player_extract_sub_lines(
    node: *const ffi::mpv_node,
) -> MementoSubLineListC {
    let Some(array_iter) = (unsafe { iter_node_array(node) }) else {
        return MementoSubLineListC {
            items: std::ptr::null(),
            len: 0,
            capacity: 0,
        };
    };

    let mut lines: Vec<MementoSubLineC> = Vec::new();
    for entry in array_iter {
        let Some(map_iter) = (unsafe { iter_node_map(entry) }) else {
            continue;
        };

        let mut has_id = false;
        let mut has_text = false;
        let mut has_start = false;
        let mut has_end = false;
        let mut id = 0i64;
        let mut text = c"".as_ptr();
        let mut start = 0.0f64;
        let mut end = 0.0f64;

        for (key, val) in map_iter {
            match key {
                b"id" => {
                    if val.format == ffi::MPV_FORMAT_INT64 {
                        id = unsafe { val.u.int64 };
                        has_id = true;
                    }
                }
                b"text" => {
                    if val.format == ffi::MPV_FORMAT_STRING {
                        let s = unsafe { val.u.string };
                        if !s.is_null() {
                            text = s;
                            has_text = true;
                        }
                    }
                }
                b"start" => {
                    if val.format == ffi::MPV_FORMAT_DOUBLE {
                        start = unsafe { val.u.double_ };
                        has_start = start.is_finite();
                    }
                }
                b"end" => {
                    if val.format == ffi::MPV_FORMAT_DOUBLE {
                        end = unsafe { val.u.double_ };
                        has_end = end.is_finite();
                    }
                }
                _ => {}
            }
        }

        if has_id && has_text && has_start && has_end {
            lines.push(MementoSubLineC { id, text, start, end });
        }
    }

    if lines.is_empty() {
        MementoSubLineListC {
            items: std::ptr::null(),
            len: 0,
            capacity: 0,
        }
    } else {
        lines.shrink_to_fit();
        let len = lines.len();
        let capacity = lines.capacity();
        let items = lines.as_ptr();
        std::mem::forget(lines);
        MementoSubLineListC { items, len, capacity }
    }
}

/// Frees a sub-lines list allocated by `memento_player_extract_sub_lines`.
#[no_mangle]
pub unsafe extern "C" fn memento_player_sub_lines_free(list: MementoSubLineListC) {
    if !list.items.is_null() && list.capacity > 0 {
        drop(unsafe { Vec::from_raw_parts(list.items as *mut MementoSubLineC, list.len, list.capacity) });
    }
}

struct OwnedSubLineList(MementoSubLineListC);

impl OwnedSubLineList {
    fn as_slice(&self) -> &[MementoSubLineC] {
        if self.0.len == 0 || self.0.items.is_null() {
            &[]
        } else {
            unsafe { std::slice::from_raw_parts(self.0.items, self.0.len) }
        }
    }
}

impl Drop for OwnedSubLineList {
    fn drop(&mut self) {
        unsafe { memento_player_sub_lines_free(self.0) };
    }
}

/// Cursor autohide state matching `MpvPlayer` "cursor-autohide" handler.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorAutohide {
    Number(i64),
    Always,
    Never,
}

impl Default for CursorAutohide {
    fn default() -> Self {
        Self::Number(0)
    }
}

impl CursorAutohide {
    pub fn decode(format: u32, int64_val: i64, str_val: *const c_char) -> Option<Self> {
        if format == ffi::MPV_FORMAT_INT64 {
            Some(Self::Number(int64_val))
        } else if format == ffi::MPV_FORMAT_STRING && !str_val.is_null() {
            let bytes = unsafe { CStr::from_ptr(str_val) }.to_bytes();
            match bytes {
                b"always" => Some(Self::Always),
                b"no" => Some(Self::Never),
                _ => None,
            }
        } else {
            None
        }
    }

    pub fn to_c_params(self) -> (bool, i64) {
        match self {
            Self::Number(timeout) => (timeout == 0, timeout),
            Self::Always => (true, -1),
            Self::Never => (false, -1),
        }
    }
}

/// Decodes cursor autohide property event data.
#[no_mangle]
pub unsafe extern "C" fn memento_player_decode_cursor_autohide(
    format: i32,
    int64_val: i64,
    str_val: *const c_char,
    out_cursor_hidden: *mut bool,
    out_timeout: *mut i64,
) -> bool {
    let Some(autohide) = CursorAutohide::decode(format as u32, int64_val, str_val) else {
        return false;
    };
    let (hidden, timeout) = autohide.to_c_params();
    if !out_cursor_hidden.is_null() {
        unsafe { *out_cursor_hidden = hidden };
    }
    if !out_timeout.is_null() {
        unsafe { *out_timeout = timeout };
    }
    true
}

pub const ASR_STREAM_PROTOCOL: &str = "memento-asr";
pub const ASR_STREAM_URI: &str = "memento-asr://live.vtt";

/// A subtitle cue managed by [`AsrSubtitleTrack`].
#[derive(Debug, Clone, PartialEq)]
pub struct AsrCue {
    pub id: i64,
    pub start: f64,
    pub end: f64,
    pub text: String,
}

/// Active backend mode selected for in-memory ASR subtitles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AsrTrackMode {
    /// Patched `libmpv` (`eyalmazuz/mpv` `15d25e0f`) using `mpv_command_node_async`.
    NativeMemory,
    /// Stock `libmpv` using `mpv_stream_cb_add_ro` (`memento-asr://live.vtt` + `sub-reload`).
    StreamCallback,
}

/// Format a non-negative timestamp in seconds as WebVTT `HH:MM:SS.mmm`.
pub fn format_webvtt_timestamp(seconds: f64) -> String {
    let total_ms = if seconds.is_finite() && seconds > 0.0 {
        (seconds * 1000.0).round() as u64
    } else {
        0
    };
    let ms = total_ms % 1000;
    let total_secs = total_ms / 1000;
    let secs = total_secs % 60;
    let mins = (total_secs / 60) % 60;
    let hours = total_secs / 3600;
    format!("{hours:02}:{mins:02}:{secs:02}.{ms:03}")
}

/// Render sorted ASR cues as BOM-free UTF-8 WebVTT in RAM (`WEBVTT\n\n`).
pub fn format_webvtt_cues(cues: &[AsrCue]) -> Vec<u8> {
    let mut sorted: Vec<&AsrCue> = cues
        .iter()
        .filter(|cue| {
            cue.start.is_finite() && cue.end.is_finite() && cue.start >= 0.0 && cue.end > cue.start
        })
        .collect();
    sorted.sort_by(|a, b| {
        a.start
            .total_cmp(&b.start)
            .then_with(|| a.end.total_cmp(&b.end))
            .then_with(|| a.id.cmp(&b.id))
    });

    let mut out = String::from("WEBVTT\n\n");
    for cue in sorted {
        let cleaned = sanitize_webvtt_text(&cue.text);
        if cleaned.is_empty() {
            continue;
        }
        let start_ms = (cue.start.max(0.0) * 1000.0).round() as u64;
        let raw_end_ms = (cue.end.max(0.0) * 1000.0).round() as u64;
        let end_ms = if raw_end_ms > start_ms {
            raw_end_ms
        } else {
            start_ms + 1
        };
        let start_ts = format_webvtt_timestamp_ms(start_ms);
        let end_ts = format_webvtt_timestamp_ms(end_ms);
        out.push_str(&start_ts);
        out.push_str(" --> ");
        out.push_str(&end_ts);
        out.push('\n');
        out.push_str(&cleaned);
        out.push_str("\n\n");
    }
    out.into_bytes()
}

fn format_webvtt_timestamp_ms(total_ms: u64) -> String {
    let ms = total_ms % 1000;
    let total_secs = total_ms / 1000;
    let secs = total_secs % 60;
    let mins = (total_secs / 60) % 60;
    let hours = total_secs / 3600;
    format!("{hours:02}:{mins:02}:{secs:02}.{ms:03}")
}

fn sanitize_webvtt_text(text: &str) -> String {
    let normalized = text
        .replace('\0', "")
        .replace("\r\n", "\n")
        .replace('\r', "\n");
    let lines: Vec<String> = normalized
        .lines()
        .map(str::trim_end)
        .filter(|line| !line.is_empty())
        .map(|line| {
            line.replace('&', "&amp;")
                .replace('<', "&lt;")
                .replace('>', "&gt;")
        })
        .collect();
    lines.join("\n")
}

struct StreamCursor {
    data: Arc<[u8]>,
    pos: usize,
}

unsafe extern "C" fn stream_cb_open_ro(
    user_data: *mut c_void,
    uri: *mut c_char,
    info: *mut ffi::mpv_stream_cb_info,
) -> c_int {
    catch_unwind(AssertUnwindSafe(|| {
        if user_data.is_null() || uri.is_null() || info.is_null() {
            return ffi::MPV_ERROR_LOADING_FAILED;
        }
        let uri_bytes = unsafe { CStr::from_ptr(uri) }.to_bytes();
        if !uri_bytes.starts_with(b"memento-asr://") {
            return ffi::MPV_ERROR_LOADING_FAILED;
        }
        let shared = unsafe { &*(user_data as *const RwLock<Arc<[u8]>>) };
        let snapshot = shared
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        let cursor = Box::into_raw(Box::new(StreamCursor {
            data: snapshot,
            pos: 0,
        }));
        unsafe {
            (*info).cookie = cursor.cast();
            (*info).read_fn = Some(stream_cb_read);
            (*info).seek_fn = Some(stream_cb_seek);
            (*info).size_fn = Some(stream_cb_size);
            (*info).close_fn = Some(stream_cb_close);
            (*info).cancel_fn = None;
        }
        0
    }))
    .unwrap_or(ffi::MPV_ERROR_LOADING_FAILED)
}

unsafe extern "C" fn stream_cb_read(
    cookie: *mut c_void,
    buf: *mut c_char,
    nbytes: u64,
) -> i64 {
    catch_unwind(AssertUnwindSafe(|| {
        if cookie.is_null() || (buf.is_null() && nbytes > 0) {
            return -1;
        }
        if nbytes == 0 {
            return 0;
        }
        let cursor = unsafe { &mut *(cookie as *mut StreamCursor) };
        let remaining = cursor.data.len().saturating_sub(cursor.pos);
        if remaining == 0 {
            return 0;
        }
        let requested = usize::try_from(nbytes).unwrap_or(usize::MAX);
        let to_copy = remaining.min(requested).min(i64::MAX as usize);
        unsafe {
            ptr::copy_nonoverlapping(
                cursor.data.as_ptr().add(cursor.pos),
                buf.cast::<u8>(),
                to_copy,
            );
        }
        cursor.pos += to_copy;
        to_copy as i64
    }))
    .unwrap_or(-1)
}

unsafe extern "C" fn stream_cb_seek(cookie: *mut c_void, offset: i64) -> i64 {
    catch_unwind(AssertUnwindSafe(|| {
        if cookie.is_null() || offset < 0 {
            return i64::from(ffi::MPV_ERROR_GENERIC);
        }
        let cursor = unsafe { &mut *(cookie as *mut StreamCursor) };
        let Ok(target) = usize::try_from(offset) else {
            return i64::from(ffi::MPV_ERROR_GENERIC);
        };
        if target > cursor.data.len() {
            return i64::from(ffi::MPV_ERROR_GENERIC);
        }
        cursor.pos = target;
        offset
    }))
    .unwrap_or(i64::from(ffi::MPV_ERROR_GENERIC))
}

unsafe extern "C" fn stream_cb_size(cookie: *mut c_void) -> i64 {
    catch_unwind(AssertUnwindSafe(|| {
        if cookie.is_null() {
            return i64::from(ffi::MPV_ERROR_UNSUPPORTED);
        }
        let cursor = unsafe { &*(cookie as *const StreamCursor) };
        i64::try_from(cursor.data.len()).unwrap_or(i64::MAX)
    }))
    .unwrap_or(i64::from(ffi::MPV_ERROR_UNSUPPORTED))
}

unsafe extern "C" fn stream_cb_close(cookie: *mut c_void) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        if !cookie.is_null() {
            drop(unsafe { Box::from_raw(cookie as *mut StreamCursor) });
        }
    }));
}

/// Decoded payload from an asynchronous `MPV_EVENT_COMMAND_REPLY`.
#[derive(Debug, Clone, PartialEq)]
pub enum CommandReplyResult {
    None,
    Int64(i64),
    Double(f64),
    Flag(bool),
    String(String),
    Other,
}

/// Reason reported by `MPV_EVENT_END_FILE`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndFileReason {
    Eof,
    Stop,
    Quit,
    Error(i32),
    Redirect,
    Unknown(i32),
}

/// Typed property payload delivered in [`MpvEvent::PropertyChange`].
#[derive(Debug, Clone, PartialEq)]
pub enum MpvPropertyValue {
    None,
    Flag(bool),
    Int64(i64),
    Double(f64),
    String(String),
    Chapters(Vec<f64>),
    Tracks {
        audio: Vec<AudioTrack>,
        subtitle: Vec<SubtitleTrack>,
        all: Vec<Track>,
    },
    SubLines(Vec<SubLine>),
    CursorAutohide(CursorAutohide),
    Other,
}

impl MpvPropertyValue {
    /// Borrow the string payload when this property value is [`Self::String`].
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(s) => Some(s.as_str()),
            _ => None,
        }
    }
}

/// Ordered event drained from `mpv_wait_event(0.0)`.
#[derive(Debug, Clone, PartialEq)]
pub enum MpvEvent {
    StartFile {
        playlist_entry_id: i64,
    },
    FileLoaded {
        video_width: i64,
        video_height: i64,
    },
    EndFile {
        reason: EndFileReason,
        playlist_entry_id: i64,
    },
    PlaybackRestart,
    Seek,
    Shutdown,
    QueueOverflow,
    CommandReply {
        reply_userdata: u64,
        error: i32,
        result: CommandReplyResult,
    },
    PropertyChange {
        name: String,
        value: MpvPropertyValue,
    },
}

impl MpvEvent {
    /// Convert an ordered batch of [`MpvEvent`]s into [`EngineEvent`]s while
    /// tracking rolling `(sub_start, sub_end)` and `(secondary_sub_start, secondary_sub_end)`
    /// across each transition so `PropertyChange::SubText` / `SecondarySubText`
    /// never reads end-of-batch overwritten cue times from `PlaybackState`.
    pub fn to_engine_events(events: &[Self], initial: &PlaybackState) -> Vec<EngineEvent> {
        let mut sub_start = initial.sub_start;
        let mut sub_end = initial.sub_end;
        let mut sec_start = initial.secondary_sub_start;
        let mut sec_end = initial.secondary_sub_end;
        let mut out = Vec::with_capacity(events.len());

        for event in events {
            match event {
                Self::StartFile { .. } | Self::EndFile { .. } => {
                    sub_start = 0.0;
                    sub_end = 0.0;
                    sec_start = 0.0;
                    sec_end = 0.0;
                }
                Self::PropertyChange { name, value } => match (name.as_str(), value) {
                    ("sub-start", MpvPropertyValue::Double(v)) => sub_start = *v,
                    ("sub-start", MpvPropertyValue::Int64(v)) => sub_start = *v as f64,
                    ("sub-start", MpvPropertyValue::None) => sub_start = 0.0,
                    ("sub-end", MpvPropertyValue::Double(v)) => sub_end = *v,
                    ("sub-end", MpvPropertyValue::Int64(v)) => sub_end = *v as f64,
                    ("sub-end", MpvPropertyValue::None) => sub_end = 0.0,
                    ("secondary-sub-start", MpvPropertyValue::Double(v)) => sec_start = *v,
                    ("secondary-sub-start", MpvPropertyValue::Int64(v)) => sec_start = *v as f64,
                    ("secondary-sub-start", MpvPropertyValue::None) => sec_start = 0.0,
                    ("secondary-sub-end", MpvPropertyValue::Double(v)) => sec_end = *v,
                    ("secondary-sub-end", MpvPropertyValue::Int64(v)) => sec_end = *v as f64,
                    ("secondary-sub-end", MpvPropertyValue::None) => sec_end = 0.0,
                    ("sid", MpvPropertyValue::Flag(false)) => {
                        sub_start = 0.0;
                        sub_end = 0.0;
                    }
                    ("secondary-sid", MpvPropertyValue::Flag(false)) => {
                        sec_start = 0.0;
                        sec_end = 0.0;
                    }
                    _ => {}
                },
                _ => {}
            }
            if let Some(engine_ev) =
                event.to_engine_event_with_times(sub_start, sub_end, sec_start, sec_end)
            {
                out.push(engine_ev);
            }
        }
        out
    }

    /// Convert to [`crate::events::EngineEvent`] for driving `machine::PlayerCore`.
    pub fn to_engine_event(&self, playback: &PlaybackState) -> Option<EngineEvent> {
        self.to_engine_event_with_times(
            playback.sub_start,
            playback.sub_end,
            playback.secondary_sub_start,
            playback.secondary_sub_end,
        )
    }

    fn to_engine_event_with_times(
        &self,
        sub_start: f64,
        sub_end: f64,
        secondary_sub_start: f64,
        secondary_sub_end: f64,
    ) -> Option<EngineEvent> {
        match self {
            Self::StartFile { .. } => Some(EngineEvent::StartFile),
            Self::FileLoaded {
                video_width,
                video_height,
            } => Some(EngineEvent::FileLoaded {
                video_width: *video_width,
                video_height: *video_height,
            }),
            Self::EndFile { .. } => Some(EngineEvent::EndFile),
            Self::Seek => Some(EngineEvent::EngineSeek),
            Self::Shutdown => Some(EngineEvent::Shutdown),
            Self::PlaybackRestart | Self::QueueOverflow | Self::CommandReply { .. } => None,
            Self::PropertyChange { name, value } => {
                let change = match (name.as_str(), value) {
                    ("chapter-list", MpvPropertyValue::Chapters(v)) => {
                        PropertyChange::Chapters(v.clone())
                    }
                    ("chapter-list", MpvPropertyValue::None) => {
                        PropertyChange::Unavailable(PropertyName::ChapterList)
                    }
                    ("duration", MpvPropertyValue::Double(v)) => PropertyChange::Duration(*v),
                    ("duration", MpvPropertyValue::None) => {
                        PropertyChange::Unavailable(PropertyName::Duration)
                    }
                    ("fullscreen", MpvPropertyValue::Flag(v)) => PropertyChange::Fullscreen(*v),
                    ("fullscreen", MpvPropertyValue::None) => {
                        PropertyChange::Unavailable(PropertyName::Fullscreen)
                    }
                    ("media-title", MpvPropertyValue::String(v)) => {
                        PropertyChange::MediaTitle(v.clone())
                    }
                    ("media-title", MpvPropertyValue::None) => {
                        PropertyChange::Unavailable(PropertyName::MediaTitle)
                    }
                    ("path", MpvPropertyValue::String(v)) => PropertyChange::Path(v.clone()),
                    ("path", MpvPropertyValue::None) => {
                        PropertyChange::Unavailable(PropertyName::Path)
                    }
                    ("pause", MpvPropertyValue::Flag(v)) => PropertyChange::Pause(*v),
                    ("pause", MpvPropertyValue::None) => {
                        PropertyChange::Unavailable(PropertyName::Pause)
                    }
                    ("time-pos", MpvPropertyValue::Double(v)) => PropertyChange::TimePos(*v),
                    ("time-pos", MpvPropertyValue::None) => {
                        PropertyChange::Unavailable(PropertyName::TimePos)
                    }
                    ("mute", MpvPropertyValue::Flag(v)) => PropertyChange::Mute(*v),
                    ("mute", MpvPropertyValue::None) => {
                        PropertyChange::Unavailable(PropertyName::Mute)
                    }
                    ("track-list", MpvPropertyValue::Tracks { all, .. }) => {
                        PropertyChange::TrackList(all.clone())
                    }
                    ("volume", MpvPropertyValue::Double(v)) => {
                        PropertyChange::Volume(v.round() as i64)
                    }
                    ("volume", MpvPropertyValue::Int64(v)) => PropertyChange::Volume(*v),
                    ("volume", MpvPropertyValue::None) => {
                        PropertyChange::Unavailable(PropertyName::Volume)
                    }
                    ("volume-max", MpvPropertyValue::Int64(v)) => PropertyChange::VolumeMax(*v),
                    ("volume-max", MpvPropertyValue::Double(v)) => {
                        PropertyChange::VolumeMax(v.round() as i64)
                    }
                    ("volume-max", MpvPropertyValue::None) => {
                        PropertyChange::Unavailable(PropertyName::VolumeMax)
                    }
                    ("video-params/w", MpvPropertyValue::Int64(v)) => {
                        PropertyChange::VideoWidth(*v)
                    }
                    ("video-params/w", MpvPropertyValue::None) => {
                        PropertyChange::Unavailable(PropertyName::VideoWidth)
                    }
                    ("video-params/h", MpvPropertyValue::Int64(v)) => {
                        PropertyChange::VideoHeight(*v)
                    }
                    ("video-params/h", MpvPropertyValue::None) => {
                        PropertyChange::Unavailable(PropertyName::VideoHeight)
                    }
                    ("aid", MpvPropertyValue::Int64(v)) => PropertyChange::Aid(*v),
                    ("aid", MpvPropertyValue::Flag(false)) => PropertyChange::AidDisabled,
                    ("aid", MpvPropertyValue::None) => {
                        PropertyChange::Unavailable(PropertyName::Aid)
                    }
                    ("sid", MpvPropertyValue::Int64(v)) => PropertyChange::Sid(*v),
                    ("sid", MpvPropertyValue::Flag(false)) => PropertyChange::SidDisabled,
                    ("sid", MpvPropertyValue::None) => {
                        PropertyChange::Unavailable(PropertyName::Sid)
                    }
                    ("secondary-sid", MpvPropertyValue::Int64(v)) => {
                        PropertyChange::SecondarySid(*v)
                    }
                    ("secondary-sid", MpvPropertyValue::Flag(false)) => {
                        PropertyChange::SecondarySidDisabled
                    }
                    ("secondary-sid", MpvPropertyValue::None) => {
                        PropertyChange::Unavailable(PropertyName::SecondarySid)
                    }
                    ("vid", MpvPropertyValue::Int64(v)) => PropertyChange::Vid(*v),
                    ("vid", MpvPropertyValue::Flag(false)) => PropertyChange::VidDisabled,
                    ("vid", MpvPropertyValue::None) => {
                        PropertyChange::Unavailable(PropertyName::Vid)
                    }
                    ("sub-delay", MpvPropertyValue::Double(v)) => PropertyChange::SubDelay(*v),
                    ("sub-delay", MpvPropertyValue::None) => {
                        PropertyChange::Unavailable(PropertyName::SubDelay)
                    }
                    ("sub-speed", MpvPropertyValue::Double(v)) => PropertyChange::SubSpeed(*v),
                    ("sub-speed", MpvPropertyValue::None) => {
                        PropertyChange::Unavailable(PropertyName::SubSpeed)
                    }
                    ("secondary-sub-delay", MpvPropertyValue::Double(v)) => {
                        PropertyChange::SecondarySubDelay(*v)
                    }
                    ("secondary-sub-delay", MpvPropertyValue::None) => {
                        PropertyChange::Unavailable(PropertyName::SecondarySubDelay)
                    }
                    ("sub-text", MpvPropertyValue::String(text)) => PropertyChange::SubText {
                        text: text.clone(),
                        start: Some(sub_start),
                        end: Some(sub_end),
                    },
                    ("sub-text", MpvPropertyValue::None) => {
                        PropertyChange::Unavailable(PropertyName::SubText)
                    }
                    ("secondary-sub-text", MpvPropertyValue::String(text)) => {
                        PropertyChange::SecondarySubText {
                            text: text.clone(),
                            start: Some(secondary_sub_start),
                            end: Some(secondary_sub_end),
                        }
                    }
                    ("secondary-sub-text", MpvPropertyValue::None) => {
                        PropertyChange::Unavailable(PropertyName::SecondarySubText)
                    }
                    ("sub-lines", MpvPropertyValue::SubLines(lines)) => {
                        PropertyChange::SubLines(
                            lines
                                .iter()
                                .map(|l| crate::events::SubLine {
                                    id: l.id,
                                    text: l.text.clone(),
                                    start: l.start,
                                    end: l.end,
                                })
                                .collect(),
                        )
                    }
                    ("sub-visibility", MpvPropertyValue::Flag(v)) => {
                        PropertyChange::SubVisibility(*v)
                    }
                    ("secondary-sub-visibility", MpvPropertyValue::Flag(v)) => {
                        PropertyChange::SecondarySubVisibility(*v)
                    }
                    ("cursor-autohide", MpvPropertyValue::CursorAutohide(v)) => {
                        PropertyChange::CursorAutohide(*v)
                    }
                    ("cursor-autohide-fs-only", MpvPropertyValue::Flag(v)) => {
                        PropertyChange::CursorAutohideFullscreenOnly(*v)
                    }
                    _ => return None,
                };
                Some(EngineEvent::Property(change))
            }
        }
    }
}

/// An initialized mpv instance. It starts with a software render context and
/// can switch to OpenGL after the UI has created its graphics context.
pub struct MpvInstance {
    handle: *mut ffi::mpv_handle,
    render_lock: Arc<Mutex<RenderContext>>,
    // mpv may call update/wakeup callbacks from worker threads. Dropped only
    // after unregistering them and calling mpv_terminate_destroy.
    render_pending: Box<RenderNotification>,
    event_pending: Box<EventNotification>,
    playback: RefCell<PlaybackState>,
    queued_events: RefCell<Vec<MpvEvent>>,
    configured_hwdec: String,
    memory_subtitles_supported: bool,
    asr_stream: Arc<RwLock<Arc<[u8]>>>,
    asr_stream_raw: Option<*const RwLock<Arc<[u8]>>>,
    next_reply_id: AtomicU64,
    screenshot_jobs: Option<SyncSender<CaptureJob>>,
    screenshot_closing: Arc<AtomicBool>,
    screenshot_wake_handle: Arc<Mutex<Option<RawMpvHandle>>>,
    screenshot_thread: Option<std::thread::JoinHandle<()>>,
    render_update_delay: Cell<std::time::Duration>,
}
unsafe impl Send for MpvInstance {}

/// Owned values delivered by mpv's property notifications. Reading these does
/// not wait on mpv's core, which must never block the UI's render thread.
#[derive(Default)]
pub struct PlaybackState {
    pub path: String,
    pub media_title: String,
    pub file_loaded: bool,
    pub playback_ready: bool,
    pub position: f64,
    pub duration: f64,
    pub paused: bool,
    pub mute: bool,
    pub volume: f64,
    pub volume_max: i64,
    pub fullscreen: bool,
    pub dimensions: (u32, u32),
    pub video_width: i64,
    pub video_height: i64,
    pub chapters: Vec<f64>,
    pub audio_id: i64,
    pub primary_sid: i64,
    pub secondary_sid: i64,
    pub video_id: i64,
    pub sub_text: String,
    pub secondary_sub_text: String,
    pub sub_start: f64,
    pub sub_end: f64,
    pub sub_delay: f64,
    pub sub_speed: f64,
    pub sub_visibility: bool,
    pub secondary_sub_start: f64,
    pub secondary_sub_end: f64,
    pub secondary_sub_delay: f64,
    pub secondary_sub_visibility: bool,
    pub audio_tracks: Vec<AudioTrack>,
    pub subtitle_tracks: Vec<SubtitleTrack>,
    pub sub_lines: Vec<SubLine>,
    pub frame_drops: i64,
    pub decoder_drops: i64,
    pub hwdec_current: String,
    pub cursor_autohide: CursorAutohide,
    pub cursor_autohide_fs_only: bool,
}

const PLAYBACK_PROPERTIES: &[(&CStr, ffi::mpv_format)] = &[
    (c"chapter-list", ffi::MPV_FORMAT_NODE),
    (c"chapters", ffi::MPV_FORMAT_INT64),
    (c"path", ffi::MPV_FORMAT_STRING),
    (c"media-title", ffi::MPV_FORMAT_STRING),
    (c"time-pos", ffi::MPV_FORMAT_DOUBLE),
    (c"duration", ffi::MPV_FORMAT_DOUBLE),
    (c"pause", ffi::MPV_FORMAT_FLAG),
    (c"fullscreen", ffi::MPV_FORMAT_FLAG),
    (c"mute", ffi::MPV_FORMAT_FLAG),
    (c"volume-max", ffi::MPV_FORMAT_INT64),
    (c"volume", ffi::MPV_FORMAT_DOUBLE),
    (c"dwidth", ffi::MPV_FORMAT_INT64),
    (c"dheight", ffi::MPV_FORMAT_INT64),
    (c"video-params/w", ffi::MPV_FORMAT_INT64),
    (c"video-params/h", ffi::MPV_FORMAT_INT64),
    (c"aid", ffi::MPV_FORMAT_INT64),
    (c"aid", ffi::MPV_FORMAT_FLAG),
    (c"sid", ffi::MPV_FORMAT_INT64),
    (c"sid", ffi::MPV_FORMAT_FLAG),
    (c"secondary-sid", ffi::MPV_FORMAT_INT64),
    (c"secondary-sid", ffi::MPV_FORMAT_FLAG),
    (c"vid", ffi::MPV_FORMAT_INT64),
    (c"vid", ffi::MPV_FORMAT_FLAG),
    (c"sub-start", ffi::MPV_FORMAT_DOUBLE),
    (c"sub-end", ffi::MPV_FORMAT_DOUBLE),
    (c"sub-text", ffi::MPV_FORMAT_STRING),
    (c"sub-delay", ffi::MPV_FORMAT_DOUBLE),
    (c"sub-speed", ffi::MPV_FORMAT_DOUBLE),
    (c"sub-visibility", ffi::MPV_FORMAT_FLAG),
    (c"secondary-sub-start", ffi::MPV_FORMAT_DOUBLE),
    (c"secondary-sub-end", ffi::MPV_FORMAT_DOUBLE),
    (c"secondary-sub-text", ffi::MPV_FORMAT_STRING),
    (c"secondary-sub-delay", ffi::MPV_FORMAT_DOUBLE),
    (c"secondary-sub-visibility", ffi::MPV_FORMAT_FLAG),
    (c"track-list", ffi::MPV_FORMAT_NODE),
    (c"sub-lines", ffi::MPV_FORMAT_NODE),
    (c"frame-drop-count", ffi::MPV_FORMAT_INT64),
    (c"decoder-frame-drop-count", ffi::MPV_FORMAT_INT64),
    (c"hwdec-current", ffi::MPV_FORMAT_STRING),
    (c"cursor-autohide", ffi::MPV_FORMAT_INT64),
    (c"cursor-autohide", ffi::MPV_FORMAT_STRING),
    (c"cursor-autohide-fs-only", ffi::MPV_FORMAT_FLAG),
];

fn extract_tracks_from_node(
    node: *const ffi::mpv_node,
) -> (Vec<AudioTrack>, Vec<SubtitleTrack>, Vec<Track>) {
    let tracks = OwnedTrackList(unsafe { memento_player_extract_tracks(node) });
    let items = tracks.as_slice();
    let audio = items
        .iter()
        .filter(|t| t.track_type == TrackType::Audio as i32)
        .map(|t| AudioTrack {
            id: t.id,
            language: copy_mpv_string(t.lang),
            title: copy_mpv_string(t.title),
            external_filename: copy_mpv_string(t.external_filename),
            external: t.external,
            default_track: t.default_track,
            selected: t.selected,
        })
        .collect();
    let subtitle = items
        .iter()
        .filter(|t| t.track_type == TrackType::Subtitle as i32)
        .map(|t| SubtitleTrack {
            id: t.id,
            language: copy_mpv_string(t.lang),
            title: copy_mpv_string(t.title),
            external_path: copy_mpv_string(t.external_filename),
            external: t.external,
            default_track: t.default_track,
            selected: t.selected,
        })
        .collect();
    let all = items
        .iter()
        .map(|t| Track {
            id: t.id,
            track_type: match t.track_type {
                1 => TrackType::Audio,
                2 => TrackType::Video,
                3 => TrackType::Subtitle,
                _ => TrackType::None,
            },
            src_id: t.src_id,
            title: copy_mpv_string(t.title),
            language: copy_mpv_string(t.lang),
            album_art: t.albumart,
            default_track: t.default_track,
            selected: t.selected,
            main_selection: t.main_selection,
            external: t.external,
            external_filename: copy_mpv_string(t.external_filename),
            codec: copy_mpv_string(t.codec),
        })
        .collect();
    (audio, subtitle, all)
}

fn extract_sub_lines_from_node(node: *const ffi::mpv_node) -> Vec<SubLine> {
    let lines = OwnedSubLineList(unsafe { memento_player_extract_sub_lines(node) });
    lines
        .as_slice()
        .iter()
        .map(|line| SubLine {
            id: line.id,
            text: copy_mpv_string(line.text),
            start: line.start,
            end: line.end,
        })
        .collect()
}

fn extract_chapters_from_node(node: *const ffi::mpv_node) -> Vec<f64> {
    let list = unsafe { memento_player_extract_chapter_times(node) };
    let chapters = if list.len == 0 || list.items.is_null() {
        Vec::new()
    } else {
        unsafe { std::slice::from_raw_parts(list.items, list.len) }.to_vec()
    };
    unsafe { memento_player_f64_list_free(list) };
    chapters
}

impl PlaybackState {
    // Event storage belongs to mpv and expires at the next mpv_wait_event call.
    // Copy strings and node contents here, while that storage is still valid.
    unsafe fn update(&mut self, property: &ffi::mpv_event_property) {
        if property.name.is_null() {
            return;
        }
        let name = unsafe { CStr::from_ptr(property.name) }.to_bytes();
        let data = property.data;
        let number = |default| {
            if property.format == ffi::MPV_FORMAT_DOUBLE && !data.is_null() {
                unsafe { *data.cast::<f64>() }
            } else if property.format == ffi::MPV_FORMAT_INT64 && !data.is_null() {
                unsafe { *data.cast::<i64>() as f64 }
            } else {
                default
            }
        };
        let integer = || {
            if property.format == ffi::MPV_FORMAT_INT64 && !data.is_null() {
                unsafe { *data.cast::<i64>() }
            } else {
                0
            }
        };
        let flag = || -> Option<bool> {
            if property.format == ffi::MPV_FORMAT_FLAG && !data.is_null() {
                Some(unsafe { *data.cast::<c_int>() } != 0)
            } else {
                None
            }
        };
        let string = || {
            if property.format == ffi::MPV_FORMAT_STRING && !data.is_null() {
                copy_mpv_string(unsafe { *data.cast::<*const c_char>() })
            } else {
                String::new()
            }
        };
        let update_track_id = |slot: &mut i64| {
            if property.format == ffi::MPV_FORMAT_INT64 && !data.is_null() {
                *slot = unsafe { *data.cast::<i64>() };
            } else if property.format == ffi::MPV_FORMAT_FLAG && !data.is_null() {
                if unsafe { *data.cast::<c_int>() } == 0 {
                    *slot = 0;
                }
            }
        };

        match name {
            b"path" => self.path = string(),
            b"media-title" => self.media_title = string(),
            b"time-pos" => self.position = number(0.0).max(0.0),
            b"duration" => self.duration = number(0.0).max(0.0),
            b"pause" => {
                if let Some(paused) = flag() {
                    self.paused = paused;
                }
            }
            b"fullscreen" => {
                if let Some(fs) = flag() {
                    self.fullscreen = fs;
                }
            }
            b"mute" => {
                if let Some(muted) = flag() {
                    self.mute = muted;
                }
            }
            b"volume" => self.volume = number(100.0),
            b"volume-max" => {
                if property.format == ffi::MPV_FORMAT_INT64 && !data.is_null() {
                    self.volume_max = unsafe { *data.cast::<i64>() };
                } else if property.format == ffi::MPV_FORMAT_DOUBLE && !data.is_null() {
                    self.volume_max = unsafe { *data.cast::<f64>() }.round() as i64;
                }
            }
            b"dwidth" => self.dimensions.0 = u32::try_from(integer()).unwrap_or(0),
            b"dheight" => self.dimensions.1 = u32::try_from(integer()).unwrap_or(0),
            b"video-params/w" => self.video_width = integer(),
            b"video-params/h" => self.video_height = integer(),
            b"chapter-list" => {
                self.chapters = if property.format == ffi::MPV_FORMAT_NODE && !data.is_null() {
                    extract_chapters_from_node(data.cast())
                } else {
                    Vec::new()
                };
            }
            b"aid" => update_track_id(&mut self.audio_id),
            b"sid" => update_track_id(&mut self.primary_sid),
            b"secondary-sid" => update_track_id(&mut self.secondary_sid),
            b"vid" => update_track_id(&mut self.video_id),
            b"sub-text" => self.sub_text = string().trim().to_owned(),
            b"secondary-sub-text" => self.secondary_sub_text = string().trim().to_owned(),
            b"sub-start" => self.sub_start = number(0.0),
            b"sub-end" => self.sub_end = number(0.0),
            b"sub-delay" => self.sub_delay = number(0.0),
            b"sub-speed" => self.sub_speed = number(1.0).max(f64::EPSILON),
            b"sub-visibility" => {
                if let Some(v) = flag() {
                    self.sub_visibility = v;
                }
            }
            b"secondary-sub-start" => self.secondary_sub_start = number(0.0),
            b"secondary-sub-end" => self.secondary_sub_end = number(0.0),
            b"secondary-sub-delay" => self.secondary_sub_delay = number(0.0),
            b"secondary-sub-visibility" => {
                if let Some(v) = flag() {
                    self.secondary_sub_visibility = v;
                }
            }
            b"frame-drop-count" => self.frame_drops = integer(),
            b"decoder-frame-drop-count" => self.decoder_drops = integer(),
            b"hwdec-current" => self.hwdec_current = string(),
            b"cursor-autohide" => {
                let int64_val = if property.format == ffi::MPV_FORMAT_INT64 && !data.is_null() {
                    unsafe { *data.cast::<i64>() }
                } else {
                    0
                };
                let str_val = if property.format == ffi::MPV_FORMAT_STRING && !data.is_null() {
                    unsafe { *data.cast::<*const c_char>() }
                } else {
                    ptr::null()
                };
                if let Some(decoded) = CursorAutohide::decode(property.format, int64_val, str_val) {
                    self.cursor_autohide = decoded;
                }
            }
            b"cursor-autohide-fs-only" => {
                if let Some(v) = flag() {
                    self.cursor_autohide_fs_only = v;
                }
            }
            b"track-list" => {
                if property.format == ffi::MPV_FORMAT_NODE && !data.is_null() {
                    let (audio, subtitle, all) = extract_tracks_from_node(data.cast());
                    self.audio_tracks = audio;
                    self.subtitle_tracks = subtitle;
                    self.primary_sid = all
                        .iter()
                        .find(|t| {
                            t.track_type == TrackType::Subtitle
                                && t.selected
                                && t.main_selection == 0
                        })
                        .map_or(0, |t| t.id);
                    self.secondary_sid = all
                        .iter()
                        .find(|t| {
                            t.track_type == TrackType::Subtitle
                                && t.selected
                                && t.main_selection == 1
                        })
                        .map_or(0, |t| t.id);
                    self.audio_id = all
                        .iter()
                        .find(|t| {
                            t.track_type == TrackType::Audio
                                && t.selected
                                && t.main_selection == 0
                        })
                        .map_or(0, |t| t.id);
                    self.video_id = all
                        .iter()
                        .find(|t| {
                            t.track_type == TrackType::Video
                                && t.selected
                                && t.main_selection == 0
                        })
                        .map_or(0, |t| t.id);
                } else {
                    self.audio_tracks.clear();
                    self.subtitle_tracks.clear();
                    self.primary_sid = 0;
                    self.secondary_sid = 0;
                    self.audio_id = 0;
                    self.video_id = 0;
                }
            }
            b"sub-lines" => {
                self.sub_lines = if property.format == ffi::MPV_FORMAT_NODE && !data.is_null() {
                    extract_sub_lines_from_node(data.cast())
                } else {
                    Vec::new()
                };
            }
            _ => {}
        }
    }
}

#[derive(Default)]
struct EventNotification {
    pending: AtomicBool,
    wake: OnceLock<Box<dyn Fn() + Send + Sync>>,
}

impl EventNotification {
    fn notify(&self) {
        if !self.pending.swap(true, Ordering::AcqRel) {
            if let Some(wake) = self.wake.get() {
                wake();
            }
        }
    }

    fn clear_pending(&self) {
        self.pending.store(false, Ordering::Release);
    }
}

#[derive(Default)]
struct RenderNotification {
    pending: AtomicBool,
    wake: OnceLock<Box<dyn Fn() + Send + Sync>>,
    pending_since: Mutex<Option<std::time::Instant>>,
}

impl RenderNotification {
    fn notify(&self) {
        let should_wake = {
            let mut since = self.pending_since.lock().unwrap_or_else(|error| error.into_inner());
            if !self.pending.swap(true, Ordering::AcqRel) {
                *since = Some(std::time::Instant::now());
                true
            } else { false }
        };
        if should_wake {
            if let Some(wake) = self.wake.get() { wake(); }
        }
    }

    fn take(&self) -> Option<std::time::Duration> {
        let mut since = self.pending_since.lock().unwrap_or_else(|error| error.into_inner());
        self.pending.swap(false, Ordering::AcqRel).then(|| {
            since.take().map_or(std::time::Duration::ZERO, |time| time.elapsed())
        })
    }
}

/// A render-only handle for a video worker. Its owner must join the worker
/// before dropping the originating MpvInstance.
pub struct MpvSoftwareRenderer {
    render_lock: Arc<Mutex<RenderContext>>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RenderApi { Software, OpenGl }

/// Native display server passed into `mpv_render_context_create` for OpenGL interop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GlDisplayBackend {
    Unknown,
    X11,
    Wayland,
}

enum GlProcResolverInner {
    /// Borrowed synchronously during `mpv_render_context_create`, then cleared.
    Scoped(*const dyn Fn(&CStr) -> *const c_void),
    /// Owned `'static` resolver valid for the lifetime of the GL render context.
    Owned(Box<dyn Fn(&CStr) -> *const c_void + Send + Sync>),
    Cleared,
}

pub struct GlProcResolver {
    inner: Mutex<GlProcResolverInner>,
}
unsafe impl Send for GlProcResolver {}
unsafe impl Sync for GlProcResolver {}

impl GlProcResolver {
    fn scoped(resolver: &dyn Fn(&CStr) -> *const c_void) -> Self {
        let ptr: *const (dyn Fn(&CStr) -> *const c_void + '_) = resolver;
        // SAFETY: `ScopedResolverClear` resets this pointer to `Cleared` before
        // `create_opengl_context_with_display` returns, so the borrowed
        // trait object is never dereferenced after `'a` ends.
        let erased: *const (dyn Fn(&CStr) -> *const c_void + 'static) =
            unsafe { std::mem::transmute(ptr) };
        Self {
            inner: Mutex::new(GlProcResolverInner::Scoped(erased)),
        }
    }

    fn owned(resolver: impl Fn(&CStr) -> *const c_void + Send + Sync + 'static) -> Self {
        Self {
            inner: Mutex::new(GlProcResolverInner::Owned(Box::new(resolver))),
        }
    }

    fn clear_scoped(&self) {
        let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if matches!(*guard, GlProcResolverInner::Scoped(_)) {
            *guard = GlProcResolverInner::Cleared;
        }
    }

    fn resolve(&self, name: &CStr) -> *mut c_void {
        let guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        match &*guard {
            GlProcResolverInner::Scoped(ptr) => {
                if ptr.is_null() {
                    ptr::null_mut()
                } else {
                    unsafe { (&**ptr)(name).cast_mut() }
                }
            }
            GlProcResolverInner::Owned(func) => func(name).cast_mut(),
            GlProcResolverInner::Cleared => ptr::null_mut(),
        }
    }
}

struct ScopedResolverClear(*mut GlProcResolver);
impl Drop for ScopedResolverClear {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { &*self.0 }.clear_scoped();
        }
    }
}

struct RenderContext {
    pointer: Option<std::ptr::NonNull<ffi::mpv_render_context>>,
    api: RenderApi,
    gl_proc_resolver: Option<std::ptr::NonNull<GlProcResolver>>,
    display_backend: GlDisplayBackend,
    advanced_control: bool,
    owner_thread: Option<std::thread::ThreadId>,
}

unsafe impl Send for RenderContext {}

impl RenderContext {
    fn free_context(&mut self) {
        if let Some(context) = self.pointer.take() {
            unsafe {
                // Always detach the update callback first so libmpv worker threads
                // stop posting render notifications before VO teardown begins.
                ffi::mpv_render_context_set_update_callback(
                    context.as_ptr(),
                    None,
                    ptr::null_mut(),
                );
                // When MPV_RENDER_PARAM_ADVANCED_CONTROL is enabled and we are on
                // the owning GL render thread, drain any pending core requests
                // (e.g., GPU screenshot / texture alloc) before tearing down the VO.
                if self.api == RenderApi::OpenGl
                    && self.advanced_control
                    && self.owner_thread == Some(std::thread::current().id())
                {
                    let _ = ffi::mpv_render_context_update(context.as_ptr());
                }
                ffi::mpv_render_context_free(context.as_ptr());
            }
        }
        // Only drop the GL proc resolver AFTER mpv_render_context_free returns,
        // because libmpv may still resolve teardown symbols during VO shutdown.
        if let Some(resolver) = self.gl_proc_resolver.take() {
            drop(unsafe { Box::from_raw(resolver.as_ptr()) });
        }
        self.display_backend = GlDisplayBackend::Unknown;
        self.advanced_control = false;
        self.owner_thread = None;
    }
}

impl MpvSoftwareRenderer {
    /// # Safety
    /// The originating `MpvInstance` must remain alive until this call returns.
    /// Join the render worker before dropping that instance.
    pub unsafe fn render_rgba(&self, width: u32, height: u32, pixels: &mut [u8]) -> Result<(), String> {
        let guard = self.render_lock.lock().map_err(|_| "mpv render lock poisoned")?;
        if guard.api != RenderApi::Software { return Err("software rendering is inactive".into()); }
        let pointer = guard.pointer.ok_or("mpv render context is unavailable")?;
        render_rgba_context(pointer.as_ptr(), width, height, pixels)
    }
}

unsafe extern "C" fn resolve_gl_proc(ctx: *mut c_void, name: *const c_char) -> *mut c_void {
    catch_unwind(AssertUnwindSafe(|| {
        if ctx.is_null() || name.is_null() {
            return ptr::null_mut();
        }
        let resolver = unsafe { &*(ctx as *const GlProcResolver) };
        resolver.resolve(unsafe { CStr::from_ptr(name) })
    }))
    .unwrap_or(ptr::null_mut())
}

unsafe extern "C" fn on_render_update(userdata: *mut std::ffi::c_void) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        if !userdata.is_null() {
            unsafe { &*(userdata as *const RenderNotification) }.notify();
        }
    }));
}

unsafe extern "C" fn on_mpv_wakeup(userdata: *mut std::ffi::c_void) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        if !userdata.is_null() {
            unsafe { &*(userdata as *const EventNotification) }.notify();
        }
    }));
}

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
            // Gentle default respecting user `mpv.conf` (and supporting zero-copy GL hwdec).
            (c"hwdec", c"auto-safe"),
            (c"hwdec-codecs", c"all"),
            (c"framedrop", c"vo"),
            // Rendering never sleeps on the UI thread. Ask mpv to notify
            // us at the frame deadline instead of its default 50ms ahead.
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

        // Pre-init pass: apply scalar and pre-init-only CLI options (`--config`, `--script`,
        // `-v`, etc.), skipping runtime list-modifying options (`--glsl-shader`, `*-append`,
        // `*-add`, `*-pre`, `*-toggle`) so they are neither overwritten by `mpv.conf` nor
        // applied twice after `mpv_initialize`.
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

        // Post-init pass: re-apply idempotent scalar options so CLI overrides `mpv.conf`,
        // and apply list-modifying options (`--glsl-shader`, `*-append`, `*-add`, `*-pre`,
        // `*-toggle`) ONCE via `change-list`.
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

        let configured_hwdec = unsafe {
            let value = ffi::mpv_get_property_string(handle, c"hwdec".as_ptr());
            let text = copy_mpv_string(value);
            if !value.is_null() {
                ffi::mpv_free(value.cast());
            }
            text
        };
        let memory_subtitles_supported = unsafe { memento_player_supports_memory_subtitles(handle) };

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

        let mut event_pending = Box::new(EventNotification::default());
        unsafe {
            ffi::mpv_set_wakeup_callback(
                handle,
                Some(on_mpv_wakeup),
                (&mut *event_pending as *mut EventNotification).cast(),
            );
        }

        for (id, (name, format)) in PLAYBACK_PROPERTIES.iter().enumerate() {
            let status = unsafe {
                ffi::mpv_observe_property(handle, id as u64 + 1, name.as_ptr(), *format)
            };
            if status < 0 {
                eprintln!(
                    "Could not observe mpv property {}: {status}",
                    name.to_string_lossy()
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
                drop(unsafe { Arc::from_raw(asr_stream_raw) });
                return Err(error);
            }
        };

        let mut render_pending = Box::new(RenderNotification::default());
        unsafe {
            ffi::mpv_render_context_set_update_callback(
                context.as_ptr(),
                Some(on_render_update),
                (&mut *render_pending as *mut RenderNotification).cast(),
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
            event_pending,
            playback: RefCell::new(PlaybackState {
                paused: true,
                volume: 100.0,
                volume_max: 100,
                sub_speed: 1.0,
                ..Default::default()
            }),
            queued_events: RefCell::new(Vec::new()),
            configured_hwdec,
            memory_subtitles_supported,
            asr_stream,
            asr_stream_raw: Some(asr_stream_raw),
            next_reply_id: AtomicU64::new(1),
            screenshot_jobs: Some(worker.jobs),
            screenshot_closing: worker.closing,
            screenshot_wake_handle: worker.wake_handle,
            screenshot_thread: Some(worker.thread),
            render_update_delay: Cell::new(std::time::Duration::ZERO),
        })
    }

    /// NVDEC's direct CUDA/GL interop requires an NVIDIA GL context and is
    /// fragile over shared Wayland EGL / GUI-toolkit contexts (where FFmpeg's
    /// decoder thread can fail with `cuCtxPushCurrent(decoder->cuda_ctx) failed`
    /// during seeks or context transitions). Adapt `hwdec` cleanly for the
    /// active GL renderer and display backend while preserving explicit user
    /// choices.
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
            if let Some(wake) = self.render_pending.wake.get() { wake(); }
        }
    }

    pub fn has_render_update(&self) -> bool {
        self.render_pending.pending.load(Ordering::Acquire)
    }

    /// Register a lightweight coalesced wakeup callback via `mpv_set_wakeup_callback`.
    /// Called from mpv threads when new events enter the queue; it must only
    /// schedule a drain on the owner thread and never call libmpv directly.
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

    fn decode_one_event(&self, raw: &ffi::mpv_event) -> Option<MpvEvent> {
        let mut state = self.playback.borrow_mut();
        match raw.event_id {
            ffi::MPV_EVENT_START_FILE => {
                state.file_loaded = false;
                state.playback_ready = false;
                state.audio_id = 0;
                state.primary_sid = 0;
                state.secondary_sid = 0;
                state.video_id = 0;
                let playlist_entry_id = unsafe { raw.data.cast::<ffi::mpv_event_start_file>().as_ref() }
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
                let (reason, playlist_entry_id) = unsafe { raw.data.cast::<ffi::mpv_event_end_file>().as_ref() }
                    .map_or((EndFileReason::Unknown(-1), 0), |info| {
                        let reason = match info.reason {
                            ffi::MPV_END_FILE_REASON_EOF => EndFileReason::Eof,
                            ffi::MPV_END_FILE_REASON_STOP => EndFileReason::Stop,
                            ffi::MPV_END_FILE_REASON_QUIT => EndFileReason::Quit,
                            ffi::MPV_END_FILE_REASON_ERROR => EndFileReason::Error(info.error),
                            ffi::MPV_END_FILE_REASON_REDIRECT => EndFileReason::Redirect,
                            other => EndFileReason::Unknown(other as i32),
                        };
                        (reason, info.playlist_entry_id)
                    });
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
                                CommandReplyResult::String(copy_mpv_string(unsafe { node.u.string }))
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
                unsafe { state.update(property) };
                let prop_name = if property.name.is_null() {
                    String::new()
                } else {
                    copy_mpv_string(property.name)
                };
                let data = property.data;
                let value = match prop_name.as_str() {
                    "chapter-list" => {
                        if property.format == ffi::MPV_FORMAT_NODE && !data.is_null() {
                            MpvPropertyValue::Chapters(state.chapters.clone())
                        } else {
                            MpvPropertyValue::None
                        }
                    }
                    "track-list" => {
                        if property.format == ffi::MPV_FORMAT_NODE && !data.is_null() {
                            let (audio, subtitle, all) = extract_tracks_from_node(data.cast());
                            MpvPropertyValue::Tracks {
                                audio,
                                subtitle,
                                all,
                            }
                        } else {
                            MpvPropertyValue::None
                        }
                    }
                    "sub-lines" => {
                        if property.format == ffi::MPV_FORMAT_NODE && !data.is_null() {
                            MpvPropertyValue::SubLines(state.sub_lines.clone())
                        } else {
                            MpvPropertyValue::None
                        }
                    }
                    "cursor-autohide" => {
                        let int64_val =
                            if property.format == ffi::MPV_FORMAT_INT64 && !data.is_null() {
                                unsafe { *data.cast::<i64>() }
                            } else {
                                0
                            };
                        let str_val =
                            if property.format == ffi::MPV_FORMAT_STRING && !data.is_null() {
                                unsafe { *data.cast::<*const c_char>() }
                            } else {
                                ptr::null()
                            };
                        CursorAutohide::decode(property.format, int64_val, str_val)
                            .map_or(MpvPropertyValue::None, MpvPropertyValue::CursorAutohide)
                    }
                    _ => match property.format {
                        ffi::MPV_FORMAT_NONE => MpvPropertyValue::None,
                        ffi::MPV_FORMAT_FLAG if !data.is_null() => {
                            MpvPropertyValue::Flag(unsafe { *data.cast::<c_int>() } != 0)
                        }
                        ffi::MPV_FORMAT_INT64 if !data.is_null() => {
                            MpvPropertyValue::Int64(unsafe { *data.cast::<i64>() })
                        }
                        ffi::MPV_FORMAT_DOUBLE if !data.is_null() => {
                            MpvPropertyValue::Double(unsafe { *data.cast::<f64>() })
                        }
                        ffi::MPV_FORMAT_STRING if !data.is_null() => {
                            MpvPropertyValue::String(copy_mpv_string(unsafe {
                                *data.cast::<*const c_char>()
                            }))
                        }
                        _ => MpvPropertyValue::Other,
                    },
                };
                Some(MpvEvent::PropertyChange {
                    name: prop_name,
                    value,
                })
            }
            _ => None,
        }
    }

    fn push_queued_event(&self, event: MpvEvent) {
        const MAX_QUEUED_EVENTS: usize = 4096;
        let mut queued = self.queued_events.borrow_mut();
        if queued.len() >= MAX_QUEUED_EVENTS {
            if let Some(drop_idx) = queued.iter().position(|ev| {
                matches!(
                    ev,
                    MpvEvent::PropertyChange { name, .. }
                        if name != "track-list" && name != "sid" && name != "aid"
                )
            }) {
                queued.remove(drop_idx);
            } else {
                queued.remove(0);
            }
        }
        queued.push(event);
    }

    /// Borrow the cached [`PlaybackState`] without calling `mpv_wait_event` or
    /// draining queued events.
    pub fn playback_state(&self) -> Ref<'_, PlaybackState> {
        self.playback.borrow()
    }

    /// Drain all queued libmpv events in strict arrival order via `mpv_wait_event(0.0)`
    /// without collapsing intermediate `START_FILE` / `FILE_LOADED` / `COMMAND_REPLY` /
    /// `PROPERTY_CHANGE` events. Also updates cached [`PlaybackState`].
    pub fn drain_events(&self) -> Vec<MpvEvent> {
        let mut events = std::mem::take(&mut *self.queued_events.borrow_mut());
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

    /// Drain all queued libmpv events and convert them into [`EngineEvent`]s
    /// using rolling per-event subtitle cue times (`MpvEvent::to_engine_events`).
    pub fn drain_engine_events(&self) -> Vec<EngineEvent> {
        let initial = {
            let s = self.playback.borrow();
            PlaybackState {
                sub_start: s.sub_start,
                sub_end: s.sub_end,
                secondary_sub_start: s.secondary_sub_start,
                secondary_sub_end: s.secondary_sub_end,
                ..Default::default()
            }
        };
        let events = self.drain_events();
        MpvEvent::to_engine_events(&events, &initial)
    }

    /// Drain asynchronous events into [`PlaybackState`] without waiting for mpv's core,
    /// preserving decoded events in `queued_events` so mixing `poll_playback_state()`
    /// and [`Self::drain_events`] never drops `CommandReply` / lifecycle / track events.
    /// Prefer [`Self::playback_state`] when reading state alongside `drain_events()`.
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

    pub fn render_update_delay(&self) -> std::time::Duration {
        self.render_update_delay.get()
    }

    /// Age of an unconsumed frame notification, without calling into mpv.
    pub fn pending_render_delay(&self) -> std::time::Duration {
        self.render_pending.pending_since.lock().unwrap_or_else(|error| error.into_inner())
            .map_or(std::time::Duration::ZERO, |since| since.elapsed())
    }

    /// Consume the notification on the render thread with its GL context current
    /// (or on the software-render thread when the software API is active).
    /// A `true` result means a new frame (or video configuration) needs a
    /// redraw. Render again on a viewport resize even if this returns false.
    pub fn take_render_update(&self) -> bool {
        if !self.has_render_update() {
            return false;
        }
        let Ok(guard) = self.render_lock.try_lock() else { return false };
        let Some(delay) = self.render_pending.take() else { return false };
        self.render_update_delay.set(delay);
        let Some(context) = guard.pointer else { return false };
        unsafe {
            ffi::mpv_render_context_update(context.as_ptr())
                & u64::from(ffi::MPV_RENDER_UPDATE_FRAME)
                != 0
        }
    }

    /// Unconditionally service `mpv_render_context_update()` on the render thread
    /// with the GL context current, returning the raw `mpv_render_update_flag`
    /// bitmask. Useful under `MPV_RENDER_PARAM_ADVANCED_CONTROL` when servicing
    /// non-frame tasks (such as GPU screenshots or texture allocations) even if
    /// the video viewport is temporarily `(0, 0)`.
    pub fn pump_render_update(&self) -> u64 {
        let Ok(guard) = self.render_lock.try_lock() else {
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

    /// Render directly into caller memory (row-major, top-down RGBA8, tightly
    /// packed, opaque). The buffer must remain exclusively borrowed until
    /// mpv returns. mpv requires at least 4-byte pixel alignment; a 64-byte
    /// aligned buffer and stride are preferred for SIMD performance.
    pub fn render_rgba(&self, width: u32, height: u32, pixels: &mut [u8]) -> Result<(), String> {
        let guard = self.render_lock.lock().map_err(|_| "mpv render lock poisoned")?;
        if guard.api != RenderApi::Software { return Err("software rendering is inactive".into()); }
        let context = guard.pointer.ok_or("mpv render context is unavailable")?;
        render_rgba_context(context.as_ptr(), width, height, pixels)
    }

    pub fn software_renderer(&self) -> MpvSoftwareRenderer {
        MpvSoftwareRenderer {
            render_lock: self.render_lock.clone(),
        }
    }

    /// Replace the slow CPU render context with libmpv's OpenGL renderer.
    /// Call only while the UI's OpenGL context is current. If GL setup fails,
    /// the software context is restored for playback compatibility.
    pub fn activate_opengl(&self, resolver: &dyn Fn(&CStr) -> *const c_void) -> Result<(), String> {
        self.activate_opengl_with_display(resolver, None, None)
    }

    /// Replace the CPU render context with libmpv's OpenGL renderer while
    /// forwarding native X11 or Wayland display handles (`MPV_RENDER_PARAM_X11_DISPLAY`
    /// / `MPV_RENDER_PARAM_WL_DISPLAY`) for zero-copy hardware decoding (`vaapi`/`nvdec`).
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

    /// Like [`Self::activate_opengl_with_display`], but retains an owned `'static`
    /// proc-address resolver for the entire lifetime of the OpenGL render context.
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
    /// over `MPV_RENDER_PARAM_ADVANCED_CONTROL` (e.g., for dedicated offscreen
    /// CGL/Metal producer threads).
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
        let mut guard = self.render_lock.lock().map_err(|_| "mpv render lock poisoned")?;
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
    /// Must be called on the OpenGL render thread while its GL context is current.
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
        let mut guard = self.render_lock.lock().map_err(|_| "mpv render lock poisoned")?;
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
        self.render_lock.lock().is_ok_and(|guard| guard.api == RenderApi::OpenGl && guard.pointer.is_some())
    }

    /// Draw to an RGBA8 GL framebuffer while the UI's context is current.
    pub fn render_opengl(&self, fbo: u32, width: u32, height: u32) -> Result<(), String> {
        self.render_opengl_with_format(fbo, width, height, 0)
    }

    /// Draw to an OpenGL framebuffer with an explicit internal format
    /// (such as `GL_RGBA16F` or `GL_RGBA8`) while the UI's context is current.
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
        let guard = self.render_lock.lock().map_err(|_| "mpv render lock poisoned")?;
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
        let status = unsafe { ffi::mpv_render_context_render(context.as_ptr(), params.as_mut_ptr()) };
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

    fn install_render_callback(&self, context: std::ptr::NonNull<ffi::mpv_render_context>) {
        unsafe {
            ffi::mpv_render_context_set_update_callback(
                context.as_ptr(), Some(on_render_update),
                (&*self.render_pending as *const RenderNotification).cast_mut().cast(),
            );
        }
    }

}

fn is_software_gl_renderer_name(renderer: &str) -> bool {
    let lower = renderer.to_ascii_lowercase();
    lower.contains("llvmpipe")
        || lower.contains("softpipe")
        || lower.contains("swrast")
        || lower.contains("swiftshader")
        || lower.contains("lavapipe")
        || lower.contains("microsoft basic render")
}

/// Compute a safe `hwdec` string for an OpenGL `mpv_render_context` given the
/// configured decoder list and `GL_RENDERER` name (assuming X11/non-Wayland).
pub fn compatible_opengl_hwdec(configured: &str, renderer: &str) -> String {
    compatible_opengl_hwdec_for_session(configured, renderer, false)
}

/// Compute a safe `hwdec` string for an OpenGL `mpv_render_context` taking into
/// account the `GL_RENDERER` vendor/type and whether Wayland EGL is active.
///
/// Key safety rules:
/// - Software GL rasterizers (`llvmpipe`, `softpipe`, `swrast`, `swiftshader`):
///   cannot share zero-copy GPU textures; rewrite direct decoders to `-copy`
///   and `auto-safe` to `auto-copy-safe`.
/// - Non-NVIDIA hardware GL (Intel/AMD/Apple): rewrite `nvdec` -> `nvdec-copy`,
///   `cuda` -> `cuda-copy`, `vdpau` -> `vdpau-copy`, preserving `vaapi`/`auto`.
/// - NVIDIA GL:
///   - When using the default `auto-safe` inside a shared GUI OpenGL context
///     (or any direct `nvdec`/`cuda`/`vdpau` over Wayland EGL), prefer
///     `nvdec-copy,auto-copy-safe` / copyback so FFmpeg's decoder worker uses a
///     standalone CUDA context immune to `cuCtxPushCurrent(decoder->cuda_ctx)`
///     failures during seeks, surface resizes, or context switches.
///   - On X11/GLX, an explicit user opt-in (`hwdec=nvdec`) is preserved.
pub fn compatible_opengl_hwdec_for_session(
    configured: &str,
    renderer: &str,
    is_wayland: bool,
) -> String {
    if is_software_gl_renderer_name(renderer) {
        return compatible_software_hwdec(configured);
    }
    let is_nvidia = renderer.to_ascii_lowercase().contains("nvidia");
    if is_nvidia {
        if is_wayland {
            return configured
                .split(',')
                .map(|decoder| match decoder.trim() {
                    "auto-safe" => "nvdec-copy,auto-copy-safe",
                    "nvdec" => "nvdec-copy",
                    "cuda" => "cuda-copy",
                    "vdpau" => "vdpau-copy",
                    _ => decoder,
                })
                .collect::<Vec<_>>()
                .join(",");
        }
        return configured
            .split(',')
            .map(|decoder| match decoder.trim() {
                "auto-safe" => "nvdec-copy,auto-copy-safe",
                _ => decoder,
            })
            .collect::<Vec<_>>()
            .join(",");
    }
    configured
        .split(',')
        .map(|decoder| match decoder.trim() {
            "nvdec" => "nvdec-copy",
            "cuda" => "cuda-copy",
            "vdpau" => "vdpau-copy",
            _ => decoder,
        })
        .collect::<Vec<_>>()
        .join(",")
}

/// Convert direct GPU-interop decoders to copyback equivalents for software
/// rendering or software-rasterized GL contexts (`llvmpipe`, `swrast`, etc.).
pub fn compatible_software_hwdec(configured: &str) -> String {
    configured
        .split(',')
        .map(|decoder| match decoder.trim() {
            "auto-safe" => "auto-copy-safe",
            "auto" | "yes" => "auto-copy",
            "nvdec" => "nvdec-copy",
            "cuda" => "cuda-copy",
            "vaapi" => "vaapi-copy",
            "vdpau" => "vdpau-copy",
            "d3d11va" => "d3d11va-copy",
            "videotoolbox" => "videotoolbox-copy",
            "vulkan" => "vulkan-copy",
            _ => decoder,
        })
        .collect::<Vec<_>>()
        .join(",")
}

fn create_software_context(handle: *mut ffi::mpv_handle) -> Result<std::ptr::NonNull<ffi::mpv_render_context>, String> {
    let mut context = ptr::null_mut();
    let mut params = [
        ffi::mpv_render_param { type_: ffi::MPV_RENDER_PARAM_API_TYPE, data: c"sw".as_ptr().cast_mut().cast() },
        ffi::mpv_render_param { type_: ffi::MPV_RENDER_PARAM_INVALID, data: ptr::null_mut() },
    ];
    let status = unsafe { ffi::mpv_render_context_create(&mut context, handle, params.as_mut_ptr()) };
    if status < 0 {
        if !context.is_null() { unsafe { ffi::mpv_render_context_free(context) }; }
        return Err(format!("mpv software render context failed with code {status}"));
    }
    std::ptr::NonNull::new(context).ok_or_else(|| "mpv software render context was null".into())
}

/// Create an OpenGL `mpv_render_context` with heap-allocated [`GlProcResolver`]
/// and optional X11 or Wayland display handles for hardware decoding interop.
pub fn create_opengl_context_with_display(
    handle: *mut ffi::mpv_handle,
    resolver: GlProcResolver,
    is_scoped: bool,
    x11_display: Option<*mut c_void>,
    wl_display: Option<*mut c_void>,
) -> Result<(std::ptr::NonNull<ffi::mpv_render_context>, std::ptr::NonNull<GlProcResolver>), String> {
    create_opengl_context_with_options(handle, resolver, is_scoped, x11_display, wl_display, true)
}

/// Create an OpenGL `mpv_render_context` with explicit control over
/// `MPV_RENDER_PARAM_ADVANCED_CONTROL`.
pub fn create_opengl_context_with_options(
    handle: *mut ffi::mpv_handle,
    resolver: GlProcResolver,
    is_scoped: bool,
    x11_display: Option<*mut c_void>,
    wl_display: Option<*mut c_void>,
    advanced_control_enabled: bool,
) -> Result<(std::ptr::NonNull<ffi::mpv_render_context>, std::ptr::NonNull<GlProcResolver>), String> {
    let non_null_resolver = std::ptr::NonNull::from(Box::leak(Box::new(resolver)));
    let resolver_ptr = non_null_resolver.as_ptr();
    let _clear_guard = is_scoped.then_some(ScopedResolverClear(resolver_ptr));

    let mut init = ffi::mpv_opengl_init_params {
        get_proc_address: Some(resolve_gl_proc),
        get_proc_address_ctx: resolver_ptr.cast(),
    };
    let mut context = ptr::null_mut();
    // When enabled, supports GPU screenshots (including hardware frames). All
    // player operations on the UI/render thread are asynchronous or cached;
    // every update callback must be serviced even if no video frame is due.
    let mut advanced_control: c_int = c_int::from(advanced_control_enabled);
    let mut params = Vec::with_capacity(5);
    params.push(ffi::mpv_render_param {
        type_: ffi::MPV_RENDER_PARAM_API_TYPE,
        data: c"opengl".as_ptr().cast_mut().cast(),
    });
    params.push(ffi::mpv_render_param {
        type_: ffi::MPV_RENDER_PARAM_OPENGL_INIT_PARAMS,
        data: (&mut init as *mut ffi::mpv_opengl_init_params).cast(),
    });
    if advanced_control_enabled {
        params.push(ffi::mpv_render_param {
            type_: ffi::MPV_RENDER_PARAM_ADVANCED_CONTROL,
            data: (&mut advanced_control as *mut c_int).cast(),
        });
    }
    if let Some(display) = x11_display.filter(|p| !p.is_null()) {
        params.push(ffi::mpv_render_param {
            type_: ffi::MPV_RENDER_PARAM_X11_DISPLAY,
            data: display,
        });
    } else if let Some(display) = wl_display.filter(|p| !p.is_null()) {
        params.push(ffi::mpv_render_param {
            type_: ffi::MPV_RENDER_PARAM_WL_DISPLAY,
            data: display,
        });
    }
    params.push(ffi::mpv_render_param {
        type_: ffi::MPV_RENDER_PARAM_INVALID,
        data: ptr::null_mut(),
    });

    let status = unsafe { ffi::mpv_render_context_create(&mut context, handle, params.as_mut_ptr()) };
    if status < 0 {
        if !context.is_null() {
            unsafe { ffi::mpv_render_context_free(context) };
        }
        drop(_clear_guard);
        drop(unsafe { Box::from_raw(resolver_ptr) });
        return Err(format!("mpv OpenGL render context failed with code {status}"));
    }
    let Some(non_null_ctx) = std::ptr::NonNull::new(context) else {
        drop(_clear_guard);
        drop(unsafe { Box::from_raw(resolver_ptr) });
        return Err("mpv OpenGL render context was null".into());
    };
    Ok((non_null_ctx, non_null_resolver))
}

fn render_rgba_context(render: *mut ffi::mpv_render_context, width: u32, height: u32, pixels: &mut [u8]) -> Result<(), String> {
        let needed = usize::try_from(width)
            .ok()
            .and_then(|w| w.checked_mul(4))
            .and_then(|stride| stride.checked_mul(height as usize))
            .ok_or("mpv render dimensions overflow buffer size")?;
        if width == 0 || height == 0 || width > i32::MAX as u32 || height > i32::MAX as u32
            || pixels.len() < needed || (pixels.as_ptr() as usize) % 4 != 0
        {
            return Err("mpv render requires positive dimensions and an aligned RGBA8 buffer".into());
        }
        let mut size = [width as c_int, height as c_int];
        let mut stride = width as usize * 4;
        let mut params = [
            ffi::mpv_render_param { type_: ffi::MPV_RENDER_PARAM_SW_SIZE, data: size.as_mut_ptr().cast() },
            ffi::mpv_render_param { type_: ffi::MPV_RENDER_PARAM_SW_FORMAT, data: c"rgb0".as_ptr().cast_mut().cast() },
            ffi::mpv_render_param { type_: ffi::MPV_RENDER_PARAM_SW_STRIDE, data: (&mut stride as *mut usize).cast() },
            ffi::mpv_render_param { type_: ffi::MPV_RENDER_PARAM_SW_POINTER, data: pixels.as_mut_ptr().cast() },
            ffi::mpv_render_param { type_: ffi::MPV_RENDER_PARAM_INVALID, data: ptr::null_mut() },
        ];
        let status = unsafe {
            ffi::mpv_render_context_render(render, params.as_mut_ptr())
        };
        if status < 0 {
            return Err(format!("mpv software render failed with code {status}"));
        }
        // rgb0's fourth channel is undefined, not alpha. Slint expects fully
        // opaque pixels; do not expose stale byte contents as transparency.
        // Fast alpha mask using 128-bit chunks (4 pixels per chunk).
        const ALPHA_MASK_128: u128 = u128::from_ne_bytes([
            0, 0, 0, 0xFF,
            0, 0, 0, 0xFF,
            0, 0, 0, 0xFF,
            0, 0, 0, 0xFF,
        ]);
        let (prefix, chunks, suffix) = unsafe { pixels.align_to_mut::<u128>() };
        for pixel in prefix.chunks_exact_mut(4) {
            pixel[3] = 0xFF;
        }
        for chunk in chunks {
            *chunk |= ALPHA_MASK_128;
        }
        for pixel in suffix.chunks_exact_mut(4) {
            pixel[3] = 0xFF;
        }
        Ok(())
    }

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
            path: state.path.clone(), audio_id: state.audio_id,
        })
    }

    /// Queue the capture now, then consume it on a worker so mpv can continue
    /// receiving render updates while it waits for the current video frame.
    pub fn request_screenshot(&self, subtitles: bool) -> Result<ScreenshotRequest, String> {
        let (result, receiver) = mpsc::sync_channel(1);
        self.screenshot_jobs.as_ref().ok_or("Player is closing")?
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
        let pointers = [
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
        let status = unsafe {
            memento_player_command_ret_raw(self.handle, pointers.as_ptr(), &mut result.0)
        };
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
        (playback_time - self.get_sub_delay()) / self.get_sub_speed()
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
        (playback_time - self.get_secondary_sub_delay()) / self.get_secondary_sub_speed()
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

    /// Queue a command without blocking the render/UI thread on mpv's core.
    pub fn command_async(&self, args: &[&str]) -> Result<(), String> {
        let c_args: Result<Vec<std::ffi::CString>, _> =
            args.iter().map(|arg| std::ffi::CString::new(*arg)).collect();
        let c_args = c_args.map_err(|error| error.to_string())?;
        let mut pointers: Vec<*const c_char> =
            c_args.iter().map(|arg| arg.as_ptr()).collect();
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
                            CommandReplyResult::Int64(id) if memento_player_id_valid(id) => Ok(id),
                            _ => Err(format!("mpv {command} command returned no valid ID")),
                        };
                    }
                    MpvEvent::Shutdown => {
                        self.queued_events.borrow_mut().push(decoded);
                        return Err(format!("Player shut down during {command}"));
                    }
                    other => self.queued_events.borrow_mut().push(other),
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
        text: &CString,
    ) -> Result<(), String> {
        if !self.supports_memory_subtitles() {
            return Err("mpv does not support in-memory subtitles".into());
        }
        if !memento_player_id_valid(track_id) {
            return Err("invalid in-memory subtitle track ID".into());
        }
        if !unsafe {
            memento_player_cue_valid(start, end, text.as_ptr().cast(), text.as_bytes().len())
        } {
            return Err("invalid in-memory subtitle cue".into());
        }
        Ok(())
    }

    /// Mode 2 non-blocking `sub-create` via `mpv_command_node_async`.
    /// Delivers `track_id` (`Int64`) in `MpvEvent::CommandReply`.
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
    /// Delivers `cue_id` (`Int64`) in `MpvEvent::CommandReply`.
    pub fn add_memory_subtitle_async(
        &self,
        track_id: i64,
        start: f64,
        end: f64,
        text: &str,
    ) -> Result<u64, String> {
        let text = CString::new(text).map_err(|error| error.to_string())?;
        self.validate_memory_subtitle_cue(track_id, start, end, &text)?;
        let track_id = track_id.to_string();
        let start = start.to_string();
        let end = end.to_string();
        self.command_node_async(&[
            "add-subtitle",
            &track_id,
            &start,
            &end,
            text.to_str().map_err(|error| error.to_string())?,
        ])
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
        let text = CString::new(text).map_err(|error| error.to_string())?;
        self.validate_memory_subtitle_cue(track_id, start, end, &text)?;
        if !memento_player_id_valid(cue_id) {
            return Err("invalid in-memory subtitle cue ID".into());
        }
        let track_id = track_id.to_string();
        let cue_id = cue_id.to_string();
        let start = start.to_string();
        let end = end.to_string();
        self.command_node_async(&[
            "edit-subtitle",
            &track_id,
            &cue_id,
            &start,
            &end,
            text.to_str().map_err(|error| error.to_string())?,
        ])
    }

    /// Mode 2 non-blocking `remove-subtitle` via `mpv_command_node_async`.
    pub fn remove_memory_subtitle_async(&self, track_id: i64, cue_id: i64) -> Result<u64, String> {
        if !self.supports_memory_subtitles() {
            return Err("mpv does not support in-memory subtitles".into());
        }
        if !memento_player_id_valid(track_id) || !memento_player_id_valid(cue_id) {
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
        if !memento_player_id_valid(track_id) {
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
        if !memento_player_id_valid(track_id) {
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
        {
            let mut state = self.playback.borrow_mut();
            state.file_loaded = false;
            state.playback_ready = false;
        }
        let mode = if append { "append-play" } else { "replace" };
        if options.is_empty() {
            self.command(&["loadfile", path, mode])
        } else {
            let joined = options.join(",");
            self.command(&["loadfile", path, mode, "-1", &joined])
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
        let sec_str = format!("{:.3}", sec.max(0.0));
        self.command_async(&["seek", &sec_str, "absolute"])
    }

    pub fn seek_relative(&self, delta: f64) -> Result<(), String> {
        let sec_str = format!("{:.3}", delta);
        self.command_async(&["seek", &sec_str, "relative"])
    }

    pub fn set_volume(&self, vol: f64) -> Result<(), String> {
        let vol_str = format!("{:.1}", vol.clamp(0.0, 100.0));
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

/// Unified, non-blocking ASR subtitle track manager supporting both Mode 2
/// (`eyalmazuz/mpv` in-memory subtitle commands via `mpv_command_node_async`) and
/// Mode 1 (stock `libmpv` `mpv_stream_cb_add_ro` `"memento-asr://live.vtt"` + `sub-reload`).
pub struct AsrSubtitleTrack {
    mode: AsrTrackMode,
    sid: Option<i64>,
    pending_create_req: Option<u64>,
    cues: Vec<AsrCue>,
    // Mode 1 (stream_cb) state
    reload_in_flight: Option<u64>,
    reload_dirty: bool,
    deselected_by_user: bool,
    // Mode 2 (native memory subtitles) async tracking
    pending_cues: VecDeque<AsrCue>,
    pending_adds: HashMap<u64, i64>,
    mpv_cue_ids: HashMap<i64, i64>,
    pending_edits: HashMap<i64, AsrCue>,
    pending_removals: HashSet<i64>,
}

impl AsrSubtitleTrack {
    /// Create a new track manager selecting Mode 2 when `mpv.supports_memory_subtitles()`
    /// is true, otherwise Mode 1 (`memento-asr://`).
    pub fn new(mpv: &MpvInstance) -> Self {
        Self::with_mode(if mpv.supports_memory_subtitles() {
            AsrTrackMode::NativeMemory
        } else {
            AsrTrackMode::StreamCallback
        })
    }

    pub fn with_mode(mode: AsrTrackMode) -> Self {
        Self {
            mode,
            sid: None,
            pending_create_req: None,
            cues: Vec::new(),
            reload_in_flight: None,
            reload_dirty: false,
            deselected_by_user: false,
            pending_cues: VecDeque::new(),
            pending_adds: HashMap::new(),
            mpv_cue_ids: HashMap::new(),
            pending_edits: HashMap::new(),
            pending_removals: HashSet::new(),
        }
    }

    pub fn mode(&self) -> AsrTrackMode {
        self.mode
    }

    pub fn sid(&self) -> Option<i64> {
        self.sid
    }

    pub fn is_active(&self) -> bool {
        self.sid.is_some() || self.pending_create_req.is_some()
    }

    pub fn cues(&self) -> &[AsrCue] {
        &self.cues
    }

    fn reset_local_state(&mut self) {
        self.sid = None;
        self.pending_create_req = None;
        self.cues.clear();
        self.reload_in_flight = None;
        self.reload_dirty = false;
        self.deselected_by_user = false;
        self.pending_cues.clear();
        self.pending_adds.clear();
        self.mpv_cue_ids.clear();
        self.pending_edits.clear();
        self.pending_removals.clear();
    }

    /// Create/add and select the ASR track in libmpv asynchronously without
    /// blocking the UI or GL render thread.
    pub fn start(&mut self, mpv: &MpvInstance, title: &str, language: &str) -> Result<(), String> {
        if let Some(old_sid) = self.sid {
            let _ = match self.mode {
                AsrTrackMode::NativeMemory => mpv.remove_memory_subtitle_track_async(old_sid),
                AsrTrackMode::StreamCallback => {
                    let id_str = old_sid.to_string();
                    mpv.command_node_async(&["sub-remove", &id_str])
                }
            };
        }
        self.reset_local_state();
        let req = match self.mode {
            AsrTrackMode::NativeMemory => mpv.create_memory_subtitle_track_async(title, language)?,
            AsrTrackMode::StreamCallback => {
                mpv.set_asr_stream_cues(&[]);
                mpv.add_asr_stream_track_async(title, language)?
            }
        };
        self.pending_create_req = Some(req);
        Ok(())
    }

    fn upsert_sorted_cue(&mut self, cue: AsrCue) {
        self.cues.retain(|existing| existing.id != cue.id);
        self.cues.push(cue);
        self.cues.sort_by(|a, b| {
            a.start
                .total_cmp(&b.start)
                .then_with(|| a.end.total_cmp(&b.end))
                .then_with(|| a.id.cmp(&b.id))
        });
    }

    fn validate_cue(cue: &AsrCue) -> Result<(), String> {
        let text_c = CString::new(cue.text.as_str()).map_err(|error| error.to_string())?;
        if !unsafe {
            memento_player_cue_valid(
                cue.start,
                cue.end,
                text_c.as_ptr().cast(),
                text_c.as_bytes().len(),
            )
        } {
            return Err("invalid ASR subtitle cue".into());
        }
        Ok(())
    }

    /// Insert or replace a cue and push the update to libmpv asynchronously.
    pub fn add_cue(&mut self, mpv: &MpvInstance, cue: AsrCue) -> Result<(), String> {
        Self::validate_cue(&cue)?;
        self.upsert_sorted_cue(cue.clone());
        match self.mode {
            AsrTrackMode::StreamCallback => {
                mpv.set_asr_stream_cues(&self.cues);
                self.trigger_stream_reload(mpv)
            }
            AsrTrackMode::NativeMemory => {
                self.pending_removals.remove(&cue.id);
                if let Some(&mpv_cue_id) = self.mpv_cue_ids.get(&cue.id) {
                    if let Some(track_id) = self.sid {
                        mpv.edit_memory_subtitle_async(
                            track_id,
                            mpv_cue_id,
                            cue.start,
                            cue.end,
                            &cue.text,
                        )?;
                    }
                    return Ok(());
                }
                if self.pending_adds.values().any(|&local_id| local_id == cue.id) {
                    self.pending_edits.insert(cue.id, cue);
                    return Ok(());
                }
                if let Some(track_id) = self.sid {
                    let req = mpv.add_memory_subtitle_async(
                        track_id,
                        cue.start,
                        cue.end,
                        &cue.text,
                    )?;
                    self.pending_adds.insert(req, cue.id);
                } else if self.pending_create_req.is_some() {
                    self.pending_cues.retain(|c| c.id != cue.id);
                    self.pending_cues.push_back(cue);
                }
                Ok(())
            }
        }
    }

    pub fn edit_cue(&mut self, mpv: &MpvInstance, cue: AsrCue) -> Result<(), String> {
        self.add_cue(mpv, cue)
    }

    pub fn remove_cue(&mut self, mpv: &MpvInstance, cue_id: i64) -> Result<(), String> {
        self.cues.retain(|cue| cue.id != cue_id);
        match self.mode {
            AsrTrackMode::StreamCallback => {
                mpv.set_asr_stream_cues(&self.cues);
                self.trigger_stream_reload(mpv)
            }
            AsrTrackMode::NativeMemory => {
                self.pending_cues.retain(|c| c.id != cue_id);
                self.pending_edits.remove(&cue_id);
                if let Some(mpv_cue_id) = self.mpv_cue_ids.remove(&cue_id) {
                    if let Some(track_id) = self.sid {
                        mpv.remove_memory_subtitle_async(track_id, mpv_cue_id)?;
                    }
                } else if self.pending_adds.values().any(|&local_id| local_id == cue_id) {
                    self.pending_removals.insert(cue_id);
                }
                Ok(())
            }
        }
    }

    pub fn clear(&mut self, mpv: &MpvInstance) -> Result<(), String> {
        self.cues.clear();
        self.pending_cues.clear();
        self.pending_edits.clear();
        match self.mode {
            AsrTrackMode::StreamCallback => {
                mpv.set_asr_stream_cues(&[]);
                self.trigger_stream_reload(mpv)
            }
            AsrTrackMode::NativeMemory => {
                self.pending_adds.clear();
                self.pending_removals.clear();
                self.mpv_cue_ids.clear();
                if let Some(track_id) = self.sid {
                    mpv.clear_memory_subtitles_async(track_id)?;
                }
                Ok(())
            }
        }
    }

    pub fn remove_track(&mut self, mpv: &MpvInstance) -> Result<(), String> {
        if let Some(track_id) = self.sid {
            match self.mode {
                AsrTrackMode::NativeMemory => {
                    mpv.remove_memory_subtitle_track_async(track_id)?;
                }
                AsrTrackMode::StreamCallback => {
                    let id_str = track_id.to_string();
                    mpv.command_node_async(&["sub-remove", &id_str])?;
                }
            }
        }
        mpv.set_asr_stream_cues(&[]);
        self.reset_local_state();
        Ok(())
    }

    pub fn select(&mut self, mpv: &MpvInstance) -> Result<(), String> {
        self.deselected_by_user = false;
        if let Some(sid) = self.sid {
            mpv.set_sid(sid)?;
            if self.mode == AsrTrackMode::StreamCallback && self.reload_dirty {
                self.trigger_stream_reload(mpv)?;
            }
        }
        Ok(())
    }

    fn trigger_stream_reload(&mut self, mpv: &MpvInstance) -> Result<(), String> {
        if self.sid.is_none() || self.deselected_by_user {
            self.reload_dirty = true;
            return Ok(());
        }
        if self.reload_in_flight.is_some() {
            self.reload_dirty = true;
            return Ok(());
        }
        self.reload_dirty = false;
        let req = mpv.reload_asr_stream_track_async(self.sid)?;
        self.reload_in_flight = Some(req);
        Ok(())
    }

    /// Process an [`MpvEvent`] from `MpvInstance::drain_events`, resolving
    /// async track/cue IDs and flushing queued updates. Returns `Some(sid)`
    /// when the track's positive `sid` newly becomes ready.
    pub fn on_event(
        &mut self,
        mpv: &MpvInstance,
        event: &MpvEvent,
    ) -> Result<Option<i64>, String> {
        match event {
            MpvEvent::EndFile { .. } => {
                self.reset_local_state();
                Ok(None)
            }
            MpvEvent::PropertyChange {
                name,
                value: MpvPropertyValue::Tracks { subtitle, .. },
            } if name == "track-list" && self.mode == AsrTrackMode::StreamCallback => {
                if let Some(track) = subtitle
                    .iter()
                    .rfind(|t| t.external && t.external_path == ASR_STREAM_URI && t.id > 0)
                {
                    let became_ready = self.sid.replace(track.id) != Some(track.id);
                    if self.pending_create_req.is_none() && self.reload_in_flight.is_none() {
                        self.deselected_by_user = !track.selected;
                        if !self.deselected_by_user && self.reload_dirty {
                            self.trigger_stream_reload(mpv)?;
                        }
                    } else if track.selected {
                        self.deselected_by_user = false;
                    }
                    if became_ready {
                        return Ok(Some(track.id));
                    }
                }
                Ok(None)
            }
            MpvEvent::PropertyChange { name, value }
                if name == "sid" && self.mode == AsrTrackMode::StreamCallback =>
            {
                if let Some(asr_sid) = self.sid {
                    match value {
                        MpvPropertyValue::Int64(id) if *id == asr_sid => {
                            self.deselected_by_user = false;
                            if self.pending_create_req.is_none()
                                && self.reload_in_flight.is_none()
                                && self.reload_dirty
                            {
                                self.trigger_stream_reload(mpv)?;
                            }
                        }
                        MpvPropertyValue::Int64(_) | MpvPropertyValue::Flag(false)
                            if self.pending_create_req.is_none()
                                && self.reload_in_flight.is_none() =>
                        {
                            self.deselected_by_user = true;
                        }
                        _ => {}
                    }
                }
                Ok(None)
            }
            MpvEvent::CommandReply {
                reply_userdata,
                error,
                result,
            } => {
                if Some(*reply_userdata) == self.pending_create_req {
                    self.pending_create_req = None;
                    if *error < 0 {
                        self.reset_local_state();
                        return Err(format!("ASR subtitle track creation failed ({error})"));
                    }
                    match self.mode {
                        AsrTrackMode::StreamCallback => {
                            let mut became_ready = false;
                            if self.sid.is_none() {
                                if let Some(track) = mpv
                                    .subtitle_tracks()
                                    .into_iter()
                                    .rfind(|t| {
                                        t.external
                                            && t.external_path == ASR_STREAM_URI
                                            && t.id > 0
                                    })
                                {
                                    became_ready =
                                        self.sid.replace(track.id) != Some(track.id);
                                    self.deselected_by_user = !track.selected;
                                }
                            }
                            if self.reload_dirty && self.sid.is_some() && !self.deselected_by_user {
                                self.trigger_stream_reload(mpv)?;
                            }
                            return Ok(became_ready.then_some(self.sid).flatten());
                        }
                        AsrTrackMode::NativeMemory => {
                            let CommandReplyResult::Int64(track_id) = result else {
                                return Err("sub-create returned no track ID".into());
                            };
                            if !memento_player_id_valid(*track_id) {
                                return Err("sub-create returned invalid track ID".into());
                            }
                            self.sid = Some(*track_id);
                            mpv.set_sid(*track_id)?;
                            while let Some(cue) = self.pending_cues.pop_front() {
                                let req = mpv.add_memory_subtitle_async(
                                    *track_id,
                                    cue.start,
                                    cue.end,
                                    &cue.text,
                                )?;
                                self.pending_adds.insert(req, cue.id);
                            }
                            return Ok(Some(*track_id));
                        }
                    }
                }

                if Some(*reply_userdata) == self.reload_in_flight {
                    self.reload_in_flight = None;
                    if *error < 0 {
                        return Err(format!("sub-reload failed with code {error}"));
                    }
                    if self.reload_dirty && self.sid.is_some() && !self.deselected_by_user {
                        self.trigger_stream_reload(mpv)?;
                    }
                    return Ok(None);
                }

                if let Some(local_id) = self.pending_adds.remove(reply_userdata) {
                    if *error < 0 {
                        self.pending_removals.remove(&local_id);
                        self.pending_edits.remove(&local_id);
                        return Err(format!("add-subtitle failed with code {error}"));
                    }
                    if let CommandReplyResult::Int64(mpv_cue_id) = result {
                        if let Some(track_id) = self.sid {
                            if self.pending_removals.remove(&local_id) {
                                mpv.remove_memory_subtitle_async(track_id, *mpv_cue_id)?;
                            } else {
                                self.mpv_cue_ids.insert(local_id, *mpv_cue_id);
                                if let Some(edited) = self.pending_edits.remove(&local_id) {
                                    mpv.edit_memory_subtitle_async(
                                        track_id,
                                        *mpv_cue_id,
                                        edited.start,
                                        edited.end,
                                        &edited.text,
                                    )?;
                                }
                            }
                        }
                    }
                }
                Ok(None)
            }
            _ => Ok(None),
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

        // 2. Always free the mpv_render_context (and its heap GlProcResolver)
        //    BEFORE joining the screenshot worker and BEFORE calling
        //    mpv_terminate_destroy (required by libmpv render.h), so any
        //    in-flight GPU screenshot either completes or aborts cleanly.
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

        // 5. Protocol callbacks are guaranteed unregistered only after
        //    mpv_terminate_destroy returns; release the user_data Arc ref now.
        if let Some(raw) = self.asr_stream_raw.take() {
            drop(unsafe { Arc::from_raw(raw) });
        }
    }
}
/// Independent audio-only libmpv context used for pronunciation playback.
///
/// Unlike [`MpvInstance`], this context has no video renderer and never shares
/// the movie player's handle. `play_file` waits until libmpv reports playback
/// restart, so callers can report completion only after audio has started.
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

    /// Loads a local audio file and waits for libmpv's playback-restart event.
    pub fn play_file(&self, path: &std::path::Path) -> Result<(), String> {
        let path = path
            .to_str()
            .ok_or_else(|| format!("audio path is not valid UTF-8: {}", path.display()))?;
        self.discard_pending_events();
        self.command(&["loadfile", path, "replace"])?;

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        let mut file_started = false;
        loop {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                return Err("mpv did not start pronunciation playback within 30 seconds".into());
            }
            let event = unsafe {
                ffi::mpv_wait_event(self.handle, remaining.as_secs_f64().min(0.25))
            };
            if event.is_null() {
                return Err("mpv returned a null audio event".into());
            }
            match unsafe { (*event).event_id } {
                ffi::MPV_EVENT_START_FILE => file_started = true,
                ffi::MPV_EVENT_PLAYBACK_RESTART if file_started => return Ok(()),
                ffi::MPV_EVENT_END_FILE if file_started => {
                    return Err("mpv ended pronunciation audio before playback started".into())
                }
                ffi::MPV_EVENT_SHUTDOWN => {
                    return Err("audio mpv instance shut down before playback started".into())
                }
                _ => {}
            }
        }
    }

    fn command(&self, args: &[&str]) -> Result<(), String> {
        let c_args: Result<Vec<std::ffi::CString>, _> =
            args.iter().map(|arg| std::ffi::CString::new(*arg)).collect();
        let c_args = c_args.map_err(|error| error.to_string())?;
        let mut pointers: Vec<*const c_char> =
            c_args.iter().map(|arg| arg.as_ptr()).collect();
        pointers.push(ptr::null());
        let status = unsafe { ffi::mpv_command(self.handle, pointers.as_mut_ptr()) };
        if status < 0 {
            Err(format!("mpv audio loadfile command failed with code {status}"))
        } else {
            Ok(())
        }
    }

    fn discard_pending_events(&self) {
        for _ in 0..1024 {
            let event = unsafe { ffi::mpv_wait_event(self.handle, 0.0) };
            if event.is_null()
                || unsafe { (*event).event_id } == ffi::MPV_EVENT_NONE
            {
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



#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(notification.wake.set(Box::new(move || {
            observed.fetch_add(1, Ordering::Relaxed);
        })).is_ok());
        let source = notification.clone();
        std::thread::spawn(move || {
            for _ in 0..100 { source.notify(); }
        }).join().unwrap();
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        assert!(notification.take().is_some());
        assert!(notification.take().is_none());
        notification.notify();
        assert_eq!(calls.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn playback_events_own_text_and_clear_unavailable_values() {
        let mut state = PlaybackState::default();
        let mut text = CString::new(" subtitle ").unwrap();
        let mut pointer = text.as_ptr();
        let mut event = ffi::mpv_event_property {
            name: c"sub-text".as_ptr(),
            format: ffi::MPV_FORMAT_STRING,
            data: (&mut pointer as *mut *const c_char).cast(),
        };
        unsafe { state.update(&event); }
        text = CString::new("reused event storage").unwrap();
        assert_eq!(state.sub_text, "subtitle");
        assert_eq!(text.to_bytes(), b"reused event storage");
        event.format = ffi::MPV_FORMAT_NONE;
        event.data = ptr::null_mut();
        unsafe { state.update(&event); }
        assert!(state.sub_text.is_empty());
        event.name = c"sub-speed".as_ptr();
        unsafe { state.update(&event); }
        assert_eq!(state.sub_speed, 1.0);
    }

    #[test]
    fn live_frames_wake_renderer_and_deliver_playback_properties() {
        let dir = std::env::temp_dir().join(format!("memento-frame-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("input.conf"), "").unwrap();
        // Small deterministic source: no display, decoder hardware or media
        // downloads required. Exercise real libmpv notifications and rendering.
        let player = MpvInstance::new_software(Some(&dir)).unwrap();
        let (wake, frames) = std::sync::mpsc::channel();
        player.set_render_wakeup(move || { let _ = wake.send(()); });
        player.load_file("av://lavfi:testsrc=size=64x64:rate=24,format=yuv420p").unwrap();
        player.play().unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut pixels = vec![0u32; 64 * 64];
        let pixels = unsafe {
            std::slice::from_raw_parts_mut(pixels.as_mut_ptr().cast::<u8>(), pixels.len() * 4)
        };
        let mut rendered = 0;
        while rendered < 24 && std::time::Instant::now() < deadline {
            frames.recv_timeout(std::time::Duration::from_secs(1))
                .expect("mpv frame wakeup must not get lost");
            while frames.try_recv().is_ok() {}
            if player.take_render_update() {
                player.render_rgba(64, 64, pixels).unwrap();
                if player.poll_playback_state().dimensions == (64, 64) { rendered += 1; }
            }
        }
        assert_eq!(rendered, 24, "real frames should continue without a polling timer");
        {
            let state = player.poll_playback_state();
            assert_eq!(state.dimensions, (64, 64));
            assert!(!state.paused);
            assert!(state.position > 0.0);
        }
        // Stop the render clock before using synchronous diagnostic queries.
        player.command(&["set", "pause", "yes"]).unwrap();
        assert_eq!(player.poll_playback_state().frame_drops, 0);
        for subtitles in [false, true] {
            let frame = player.screenshot_raw_rgba(subtitles)
                .unwrap_or_else(|error| panic!("screenshot (subtitles={subtitles}): {error}"));
            assert_eq!((frame.width, frame.height), (64, 64));
            assert_eq!(frame.pixels.len(), 64 * 64 * 4);
            let request = player.request_screenshot(subtitles).unwrap();
            let capture = std::thread::spawn(move || request.wait());
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while !capture.is_finished() && std::time::Instant::now() < deadline {
                let _ = frames.recv_timeout(std::time::Duration::from_millis(10));
                if player.take_render_update() {
                    player.render_rgba(64, 64, pixels).unwrap();
                }
            }
            assert!(capture.is_finished(), "background screenshot stalled");
            assert_eq!(capture.join().unwrap().unwrap(), frame);
        }
        player.command(&["stop"]).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        while player.poll_playback_state().dimensions != (0, 0) {
            assert!(std::time::Instant::now() < deadline, "stopped video state must clear");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let error = player.request_screenshot(false).unwrap().wait().unwrap_err();
        assert!(error.contains("mpv screenshot-raw failed:"), "{error}");
        drop(player);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn cue_time_and_text_limits_match_qt() {
        let text = b"hello";
        assert!(unsafe { memento_player_cue_valid(-0.0, 0.001, text.as_ptr(), text.len()) });
        assert!(unsafe { memento_player_cue_valid(0.0, f64::from(i32::MAX) / 1000.0, ptr::null(), 0) });
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
            assert!(!unsafe { memento_player_cue_valid(start, end, text.as_ptr(), text.len()) });
        }
        assert!(unsafe { memento_player_cue_valid(0.0, 1.0, text.as_ptr(), text.len()) });
        assert!(!unsafe { memento_player_cue_valid(0.0, 1.0, b"hi\0there".as_ptr(), 8) });
        let at_limit = vec![b'x'; MAX_TEXT_BYTES];
        assert!(unsafe { memento_player_cue_valid(0.0, 1.0, at_limit.as_ptr(), at_limit.len()) });
        assert!(!unsafe { memento_player_cue_valid(0.0, 1.0, at_limit.as_ptr(), at_limit.len() + 1) });
    }

    #[test]
    fn cue_pointer_contract_checks_before_dereferencing() {
        let odd = 1usize as *const u8;
        assert!(unsafe { memento_player_cue_valid(0.0, 1.0, ptr::null(), 0) });
        assert!(unsafe { memento_player_cue_valid(0.0, 1.0, odd, 0) });
        assert!(!unsafe { memento_player_cue_valid(0.0, 1.0, ptr::null(), 1) });
        assert!(!unsafe { memento_player_cue_valid(0.0, 1.0, odd, MAX_TEXT_BYTES + 1) });
    }

    #[test]
    fn id_matches_int_bounds() {
        for id in [i64::MIN, -1, 0, i64::from(i32::MAX) + 1, i64::MAX] {
            assert!(!memento_player_id_valid(id));
        }
        for id in [1, i64::from(i32::MAX)] {
            assert!(memento_player_id_valid(id));
        }
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

        let mut other_key = c"other".as_ptr().cast_mut();
        let mut wrong_key = ffi::mpv_node_list {
            num: 1,
            values: &mut name,
            keys: &mut other_key,
        };
        entry.u.list = &mut wrong_key;
        assert_eq!(unsafe { command_name(&entry) }, None);

        let mut non_string: ffi::mpv_node = unsafe { std::mem::zeroed() };
        non_string.format = ffi::MPV_FORMAT_INT64;
        let mut wrong_value = ffi::mpv_node_list {
            num: 1,
            values: &mut non_string,
            keys: &mut key,
        };
        entry.u.list = &mut wrong_value;
        assert_eq!(unsafe { command_name(&entry) }, None);
        entry.format = ffi::MPV_FORMAT_NODE_ARRAY;
        assert_eq!(unsafe { command_name(&entry) }, None);
    }

    #[test]
    fn null_handle_and_args_never_call_mpv() {
        let odd_args = 1usize as *const *const c_char;
        let odd_handle = 1usize as *mut ffi::mpv_handle;
        assert_eq!(
            unsafe { memento_player_command(ptr::null_mut(), odd_args) },
            ffi::MPV_ERROR_INVALID_PARAMETER
        );
        assert_eq!(
            unsafe { memento_player_command(odd_handle, ptr::null()) },
            ffi::MPV_ERROR_INVALID_PARAMETER
        );
        assert_eq!(
            unsafe { memento_player_command_async(ptr::null_mut(), 7, odd_args) },
            ffi::MPV_ERROR_INVALID_PARAMETER
        );
        let mut out = 17;
        assert_eq!(
            unsafe { memento_player_command_ret_i64(ptr::null_mut(), odd_args, &mut out) },
            ffi::MPV_ERROR_INVALID_PARAMETER
        );
        assert_eq!(out, -1);
        assert_eq!(
            unsafe { memento_player_command_ret_i64(odd_handle, ptr::null(), &mut out) },
            ffi::MPV_ERROR_INVALID_PARAMETER
        );
        assert_eq!(out, -1);
        assert_eq!(
            unsafe { memento_player_command_ret_i64(odd_handle, odd_args, ptr::null_mut()) },
            ffi::MPV_ERROR_INVALID_PARAMETER
        );
        assert!(!unsafe { memento_player_supports_memory_subtitles(ptr::null_mut()) });
    }

    #[test]
    fn cursor_autohide_decoding() {
        let mut hidden = false;
        let mut timeout = 0i64;

        // INT64 format
        assert!(unsafe {
            memento_player_decode_cursor_autohide(
                ffi::MPV_FORMAT_INT64 as i32,
                0,
                ptr::null(),
                &mut hidden,
                &mut timeout,
            )
        });
        assert!(hidden);
        assert_eq!(timeout, 0);

        assert!(unsafe {
            memento_player_decode_cursor_autohide(
                ffi::MPV_FORMAT_INT64 as i32,
                1500,
                ptr::null(),
                &mut hidden,
                &mut timeout,
            )
        });
        assert!(!hidden);
        assert_eq!(timeout, 1500);

        // STRING format: "always"
        assert!(unsafe {
            memento_player_decode_cursor_autohide(
                ffi::MPV_FORMAT_STRING as i32,
                0,
                c"always".as_ptr(),
                &mut hidden,
                &mut timeout,
            )
        });
        assert!(hidden);
        assert_eq!(timeout, -1);

        // STRING format: "no"
        assert!(unsafe {
            memento_player_decode_cursor_autohide(
                ffi::MPV_FORMAT_STRING as i32,
                0,
                c"no".as_ptr(),
                &mut hidden,
                &mut timeout,
            )
        });
        assert!(!hidden);
        assert_eq!(timeout, -1);

        // STRING format: invalid strings
        assert!(!unsafe {
            memento_player_decode_cursor_autohide(
                ffi::MPV_FORMAT_STRING as i32,
                0,
                c"auto".as_ptr(),
                &mut hidden,
                &mut timeout,
            )
        });
        assert!(!unsafe {
            memento_player_decode_cursor_autohide(
                ffi::MPV_FORMAT_STRING as i32,
                0,
                ptr::null(),
                &mut hidden,
                &mut timeout,
            )
        });

        // Other formats
        assert!(!unsafe {
            memento_player_decode_cursor_autohide(
                ffi::MPV_FORMAT_FLAG as i32,
                1,
                ptr::null(),
                &mut hidden,
                &mut timeout,
            )
        });

        // Null out pointers do not crash
        assert!(unsafe {
            memento_player_decode_cursor_autohide(
                ffi::MPV_FORMAT_INT64 as i32,
                500,
                ptr::null(),
                ptr::null_mut(),
                ptr::null_mut(),
            )
        });
    }

    #[test]
    fn track_extraction_empty_or_invalid() {
        assert_eq!(unsafe { memento_player_extract_tracks(ptr::null()) }.len, 0);

        let mut invalid_node: ffi::mpv_node = unsafe { std::mem::zeroed() };
        invalid_node.format = ffi::MPV_FORMAT_INT64;
        assert_eq!(unsafe { memento_player_extract_tracks(&invalid_node) }.len, 0);

        let mut empty_array: ffi::mpv_node = unsafe { std::mem::zeroed() };
        empty_array.format = ffi::MPV_FORMAT_NODE_ARRAY;
        assert_eq!(unsafe { memento_player_extract_tracks(&empty_array) }.len, 0);
    }

    #[test]
    fn chapter_times_extraction_empty_or_invalid() {
        assert_eq!(unsafe { memento_player_extract_chapter_times(ptr::null()) }.len, 0);

        let mut invalid_node: ffi::mpv_node = unsafe { std::mem::zeroed() };
        invalid_node.format = ffi::MPV_FORMAT_INT64;
        assert_eq!(unsafe { memento_player_extract_chapter_times(&invalid_node) }.len, 0);
    }

    #[test]
    fn sub_lines_extraction_empty_or_invalid() {
        assert_eq!(unsafe { memento_player_extract_sub_lines(ptr::null()) }.len, 0);

        let mut invalid_node: ffi::mpv_node = unsafe { std::mem::zeroed() };
        invalid_node.format = ffi::MPV_FORMAT_INT64;
        assert_eq!(unsafe { memento_player_extract_sub_lines(&invalid_node) }.len, 0);
    }

    #[test]
    fn synthetic_track_extraction() {
        // Track 1: Audio track with all fields
        let mut keys_1 = [
            c"id".as_ptr().cast_mut(),
            c"type".as_ptr().cast_mut(),
            c"src-id".as_ptr().cast_mut(),
            c"title".as_ptr().cast_mut(),
            c"lang".as_ptr().cast_mut(),
            c"albumart".as_ptr().cast_mut(),
            c"default".as_ptr().cast_mut(),
            c"selected".as_ptr().cast_mut(),
            c"main-selection".as_ptr().cast_mut(),
            c"external".as_ptr().cast_mut(),
            c"external-filename".as_ptr().cast_mut(),
            c"codec".as_ptr().cast_mut(),
        ];
        let mut values_1 = [
            ffi::mpv_node { format: ffi::MPV_FORMAT_INT64, u: ffi::mpv_node__bindgen_ty_1 { int64: 1 } },
            ffi::mpv_node { format: ffi::MPV_FORMAT_STRING, u: ffi::mpv_node__bindgen_ty_1 { string: c"audio".as_ptr().cast_mut() } },
            ffi::mpv_node { format: ffi::MPV_FORMAT_INT64, u: ffi::mpv_node__bindgen_ty_1 { int64: 101 } },
            ffi::mpv_node { format: ffi::MPV_FORMAT_STRING, u: ffi::mpv_node__bindgen_ty_1 { string: c"Stereo".as_ptr().cast_mut() } },
            ffi::mpv_node { format: ffi::MPV_FORMAT_STRING, u: ffi::mpv_node__bindgen_ty_1 { string: c"en".as_ptr().cast_mut() } },
            ffi::mpv_node { format: ffi::MPV_FORMAT_FLAG, u: ffi::mpv_node__bindgen_ty_1 { flag: 0 } },
            ffi::mpv_node { format: ffi::MPV_FORMAT_FLAG, u: ffi::mpv_node__bindgen_ty_1 { flag: 1 } },
            ffi::mpv_node { format: ffi::MPV_FORMAT_FLAG, u: ffi::mpv_node__bindgen_ty_1 { flag: 1 } },
            ffi::mpv_node { format: ffi::MPV_FORMAT_INT64, u: ffi::mpv_node__bindgen_ty_1 { int64: 0 } },
            ffi::mpv_node { format: ffi::MPV_FORMAT_FLAG, u: ffi::mpv_node__bindgen_ty_1 { flag: 0 } },
            ffi::mpv_node { format: ffi::MPV_FORMAT_STRING, u: ffi::mpv_node__bindgen_ty_1 { string: c"".as_ptr().cast_mut() } },
            ffi::mpv_node { format: ffi::MPV_FORMAT_STRING, u: ffi::mpv_node__bindgen_ty_1 { string: c"aac".as_ptr().cast_mut() } },
        ];
        let mut map_1 = ffi::mpv_node_list {
            num: keys_1.len() as c_int,
            keys: keys_1.as_mut_ptr(),
            values: values_1.as_mut_ptr(),
        };
        let entry_1 = ffi::mpv_node {
            format: ffi::MPV_FORMAT_NODE_MAP,
            u: ffi::mpv_node__bindgen_ty_1 { list: &mut map_1 },
        };

        // Track 2: Non-map entry (should be skipped)
        let entry_2 = ffi::mpv_node {
            format: ffi::MPV_FORMAT_INT64,
            u: ffi::mpv_node__bindgen_ty_1 { int64: 42 },
        };

        // Track 3: Subtitle track with external file
        let mut keys_3 = [
            c"id".as_ptr().cast_mut(),
            c"type".as_ptr().cast_mut(),
            c"title".as_ptr().cast_mut(),
            c"external".as_ptr().cast_mut(),
            c"external-filename".as_ptr().cast_mut(),
        ];
        let mut values_3 = [
            ffi::mpv_node { format: ffi::MPV_FORMAT_INT64, u: ffi::mpv_node__bindgen_ty_1 { int64: 3 } },
            ffi::mpv_node { format: ffi::MPV_FORMAT_STRING, u: ffi::mpv_node__bindgen_ty_1 { string: c"sub".as_ptr().cast_mut() } },
            ffi::mpv_node { format: ffi::MPV_FORMAT_STRING, u: ffi::mpv_node__bindgen_ty_1 { string: c"Subs".as_ptr().cast_mut() } },
            ffi::mpv_node { format: ffi::MPV_FORMAT_FLAG, u: ffi::mpv_node__bindgen_ty_1 { flag: 1 } },
            ffi::mpv_node { format: ffi::MPV_FORMAT_STRING, u: ffi::mpv_node__bindgen_ty_1 { string: c"/path/to/sub.srt".as_ptr().cast_mut() } },
        ];
        let mut map_3 = ffi::mpv_node_list {
            num: keys_3.len() as c_int,
            keys: keys_3.as_mut_ptr(),
            values: values_3.as_mut_ptr(),
        };
        let entry_3 = ffi::mpv_node {
            format: ffi::MPV_FORMAT_NODE_MAP,
            u: ffi::mpv_node__bindgen_ty_1 { list: &mut map_3 },
        };

        let mut array_entries = [entry_1, entry_2, entry_3];
        let mut root_list = ffi::mpv_node_list {
            num: array_entries.len() as c_int,
            keys: ptr::null_mut(),
            values: array_entries.as_mut_ptr(),
        };
        let root = ffi::mpv_node {
            format: ffi::MPV_FORMAT_NODE_ARRAY,
            u: ffi::mpv_node__bindgen_ty_1 { list: &mut root_list },
        };

        let result = unsafe { memento_player_extract_tracks(&root) };
        assert_eq!(result.len, 2);
        let items = unsafe { std::slice::from_raw_parts(result.items, result.len) };

        assert_eq!(items[0].id, 1);
        assert_eq!(items[0].track_type, TrackType::Audio as i32);
        assert_eq!(items[0].src_id, 101);
        assert_eq!(unsafe { CStr::from_ptr(items[0].title) }.to_bytes(), b"Stereo");
        assert_eq!(unsafe { CStr::from_ptr(items[0].lang) }.to_bytes(), b"en");
        assert!(!items[0].albumart);
        assert!(items[0].default_track);
        assert!(items[0].selected);
        assert_eq!(items[0].main_selection, 0);
        assert!(!items[0].external);
        assert_eq!(unsafe { CStr::from_ptr(items[0].codec) }.to_bytes(), b"aac");

        assert_eq!(items[1].id, 3);
        assert_eq!(items[1].track_type, TrackType::Subtitle as i32);
        assert_eq!(unsafe { CStr::from_ptr(items[1].title) }.to_bytes(), b"Subs");
        assert!(items[1].external);
        assert_eq!(
            unsafe { CStr::from_ptr(items[1].external_filename) }.to_bytes(),
            b"/path/to/sub.srt"
        );

        unsafe { memento_player_tracks_free(result) };
    }

    #[test]
    fn synthetic_chapter_times_extraction() {
        let mut key_time = c"time".as_ptr().cast_mut();
        let mut val_time_1 = ffi::mpv_node {
            format: ffi::MPV_FORMAT_DOUBLE,
            u: ffi::mpv_node__bindgen_ty_1 { double_: 12.5 },
        };
        let mut map_1 = ffi::mpv_node_list {
            num: 1,
            keys: &mut key_time,
            values: &mut val_time_1,
        };
        let entry_1 = ffi::mpv_node {
            format: ffi::MPV_FORMAT_NODE_MAP,
            u: ffi::mpv_node__bindgen_ty_1 { list: &mut map_1 },
        };

        let mut val_time_2 = ffi::mpv_node {
            format: ffi::MPV_FORMAT_DOUBLE,
            u: ffi::mpv_node__bindgen_ty_1 { double_: 45.0 },
        };
        let mut map_2 = ffi::mpv_node_list {
            num: 1,
            keys: &mut key_time,
            values: &mut val_time_2,
        };
        let entry_2 = ffi::mpv_node {
            format: ffi::MPV_FORMAT_NODE_MAP,
            u: ffi::mpv_node__bindgen_ty_1 { list: &mut map_2 },
        };

        // Non-map entry
        let entry_3 = ffi::mpv_node {
            format: ffi::MPV_FORMAT_FLAG,
            u: ffi::mpv_node__bindgen_ty_1 { flag: 1 },
        };

        let mut array_entries = [entry_1, entry_2, entry_3];
        let mut root_list = ffi::mpv_node_list {
            num: array_entries.len() as c_int,
            keys: ptr::null_mut(),
            values: array_entries.as_mut_ptr(),
        };
        let root = ffi::mpv_node {
            format: ffi::MPV_FORMAT_NODE_ARRAY,
            u: ffi::mpv_node__bindgen_ty_1 { list: &mut root_list },
        };

        let result = unsafe { memento_player_extract_chapter_times(&root) };
        assert_eq!(result.len, 2);
        let items = unsafe { std::slice::from_raw_parts(result.items, result.len) };
        assert_eq!(items, &[12.5, 45.0]);
        unsafe { memento_player_f64_list_free(result) };
    }

    #[test]
    fn synthetic_sub_lines_extraction() {
        // Line 1: Complete valid line
        let mut keys_1 = [
            c"id".as_ptr().cast_mut(),
            c"text".as_ptr().cast_mut(),
            c"start".as_ptr().cast_mut(),
            c"end".as_ptr().cast_mut(),
        ];
        let mut values_1 = [
            ffi::mpv_node { format: ffi::MPV_FORMAT_INT64, u: ffi::mpv_node__bindgen_ty_1 { int64: 10 } },
            ffi::mpv_node { format: ffi::MPV_FORMAT_STRING, u: ffi::mpv_node__bindgen_ty_1 { string: c"hello world".as_ptr().cast_mut() } },
            ffi::mpv_node { format: ffi::MPV_FORMAT_DOUBLE, u: ffi::mpv_node__bindgen_ty_1 { double_: 1.0 } },
            ffi::mpv_node { format: ffi::MPV_FORMAT_DOUBLE, u: ffi::mpv_node__bindgen_ty_1 { double_: 2.5 } },
        ];
        let mut map_1 = ffi::mpv_node_list {
            num: keys_1.len() as c_int,
            keys: keys_1.as_mut_ptr(),
            values: values_1.as_mut_ptr(),
        };
        let entry_1 = ffi::mpv_node {
            format: ffi::MPV_FORMAT_NODE_MAP,
            u: ffi::mpv_node__bindgen_ty_1 { list: &mut map_1 },
        };

        // Line 2: Missing "id"
        let mut keys_2 = [
            c"text".as_ptr().cast_mut(),
            c"start".as_ptr().cast_mut(),
            c"end".as_ptr().cast_mut(),
        ];
        let mut values_2 = [
            ffi::mpv_node { format: ffi::MPV_FORMAT_STRING, u: ffi::mpv_node__bindgen_ty_1 { string: c"no id".as_ptr().cast_mut() } },
            ffi::mpv_node { format: ffi::MPV_FORMAT_DOUBLE, u: ffi::mpv_node__bindgen_ty_1 { double_: 3.0 } },
            ffi::mpv_node { format: ffi::MPV_FORMAT_DOUBLE, u: ffi::mpv_node__bindgen_ty_1 { double_: 4.0 } },
        ];
        let mut map_2 = ffi::mpv_node_list {
            num: keys_2.len() as c_int,
            keys: keys_2.as_mut_ptr(),
            values: values_2.as_mut_ptr(),
        };
        let entry_2 = ffi::mpv_node {
            format: ffi::MPV_FORMAT_NODE_MAP,
            u: ffi::mpv_node__bindgen_ty_1 { list: &mut map_2 },
        };

        // Line 3: NaN start
        let mut keys_3 = [
            c"id".as_ptr().cast_mut(),
            c"text".as_ptr().cast_mut(),
            c"start".as_ptr().cast_mut(),
            c"end".as_ptr().cast_mut(),
        ];
        let mut values_3 = [
            ffi::mpv_node { format: ffi::MPV_FORMAT_INT64, u: ffi::mpv_node__bindgen_ty_1 { int64: 12 } },
            ffi::mpv_node { format: ffi::MPV_FORMAT_STRING, u: ffi::mpv_node__bindgen_ty_1 { string: c"nan start".as_ptr().cast_mut() } },
            ffi::mpv_node { format: ffi::MPV_FORMAT_DOUBLE, u: ffi::mpv_node__bindgen_ty_1 { double_: f64::NAN } },
            ffi::mpv_node { format: ffi::MPV_FORMAT_DOUBLE, u: ffi::mpv_node__bindgen_ty_1 { double_: 5.0 } },
        ];
        let mut map_3 = ffi::mpv_node_list {
            num: keys_3.len() as c_int,
            keys: keys_3.as_mut_ptr(),
            values: values_3.as_mut_ptr(),
        };
        let entry_3 = ffi::mpv_node {
            format: ffi::MPV_FORMAT_NODE_MAP,
            u: ffi::mpv_node__bindgen_ty_1 { list: &mut map_3 },
        };

        let mut array_entries = [entry_1, entry_2, entry_3];
        let mut root_list = ffi::mpv_node_list {
            num: array_entries.len() as c_int,
            keys: ptr::null_mut(),
            values: array_entries.as_mut_ptr(),
        };
        let root = ffi::mpv_node {
            format: ffi::MPV_FORMAT_NODE_ARRAY,
            u: ffi::mpv_node__bindgen_ty_1 { list: &mut root_list },
        };

        let result = unsafe { memento_player_extract_sub_lines(&root) };
        assert_eq!(result.len, 1);
        let items = unsafe { std::slice::from_raw_parts(result.items, result.len) };
        assert_eq!(items[0].id, 10);
        assert_eq!(unsafe { CStr::from_ptr(items[0].text) }.to_bytes(), b"hello world");
        assert_eq!(items[0].start, 1.0);
        assert_eq!(items[0].end, 2.5);
        unsafe { memento_player_sub_lines_free(result) };
    }

    #[repr(align(4))]
    struct AlignedBytes([u8; 16]);

    #[test]
    fn software_renderer_validates_targets_and_returns_opaque_rgba() {
        let inst = MpvInstance::new_software(None).expect("software renderer must be available");
        let mut pixels = AlignedBytes([0xa5; 16]);
        assert!(inst.render_rgba(0, 1, &mut pixels.0[..4]).is_err());
        assert!(inst.render_rgba(2, 2, &mut pixels.0[..15]).is_err());
        assert!(inst.render_rgba(u32::MAX, u32::MAX, &mut pixels.0).is_err());
        // The aligned base plus one is misaligned on every target architecture.
        assert!(inst.render_rgba(1, 1, &mut pixels.0[1..5]).is_err());
        inst.render_rgba(2, 2, &mut pixels.0).expect("rendering should succeed");
        assert!(pixels.0.chunks_exact(4).all(|pixel| pixel[3] == 255));
        let worker_renderer = inst.software_renderer();
        let worker_pixels = std::thread::spawn(move || {
            let mut pixels = AlignedBytes([0; 16]);
            unsafe { worker_renderer.render_rgba(2, 2, &mut pixels.0) }.unwrap();
            pixels
        }).join().expect("render worker should finish before mpv is dropped");
        assert!(worker_pixels.0.chunks_exact(4).all(|pixel| pixel[3] == 255));
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
        assert!(!state.paused, "MPV_FORMAT_NONE on pause must retain previous value");

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

    struct TestTempDir(std::path::PathBuf);
    impl TestTempDir {
        fn new(prefix: &str) -> Self {
            static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!("{prefix}-{}-{n}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for TestTempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
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
            unsafe { stream_cb_open_ro(raw.cast_mut().cast(), bad_uri.as_ptr().cast_mut(), &mut info) },
            ffi::MPV_ERROR_LOADING_FAILED
        );

        let good_uri = CString::new(ASR_STREAM_URI).unwrap();
        assert_eq!(
            unsafe { stream_cb_open_ro(raw.cast_mut().cast(), good_uri.as_ptr().cast_mut(), &mut info) },
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
        assert_eq!(unsafe { stream_cb_read(cookie, buf.as_mut_ptr().cast(), 6) }, 6);
        assert_eq!(&buf, b"WEBVTT");
        assert_eq!(unsafe { stream_cb_seek(cookie, total) }, total);
        assert_eq!(unsafe { stream_cb_read(cookie, buf.as_mut_ptr().cast(), 6) }, 0);

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
        // Must catch unwinding panic cleanly instead of unwinding across extern "C".
        assert_eq!(
            unsafe { resolve_gl_proc(boxed.cast(), c"panic_me".as_ptr()) },
            ptr::null_mut()
        );
        // Once cleared, no stale reference is ever invoked.
        unsafe { &*boxed }.clear_scoped();
        assert_eq!(
            unsafe { resolve_gl_proc(boxed.cast(), c"glGetString".as_ptr()) },
            ptr::null_mut()
        );
        drop(unsafe { Box::from_raw(boxed) });
    }

    #[test]
    fn default_hwdec_respects_user_mpv_conf() {
        let dir = TestTempDir::new("memento-hwdec-test");
        let default_player = MpvInstance::new_software(Some(dir.path())).unwrap();
        assert_eq!(default_player.configured_hwdec(), "auto-safe");
        drop(default_player);

        std::fs::write(dir.path().join("mpv.conf"), "hwdec=no\n").unwrap();
        let custom_player = MpvInstance::new_software(Some(dir.path())).unwrap();
        assert_eq!(custom_player.configured_hwdec(), "no");
        drop(custom_player);
    }

    #[test]
    fn live_memento_asr_stream_cb_delivers_sid_cues_and_sub_seek() {
        let dir = TestTempDir::new("memento-asr-stream-test");
        std::fs::write(dir.path().join("input.conf"), "").unwrap();
        std::fs::write(dir.path().join("mpv.conf"), "ao=null\nhwdec=no\n").unwrap();

        let player = MpvInstance::new_software(Some(dir.path())).unwrap();
        let (wake_tx, wake_rx) = mpsc::channel();
        player.set_wakeup_callback(move || {
            let _ = wake_tx.send(());
        });

        let mut asr = AsrSubtitleTrack::with_mode(AsrTrackMode::StreamCallback);
        assert_eq!(asr.mode(), AsrTrackMode::StreamCallback);

        player
            .load_file("av://lavfi:testsrc=size=64x64:rate=10:duration=6,format=yuv420p")
            .unwrap();

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut saw_start = false;
        let mut saw_loaded = false;
        let mut ordered_events = Vec::new();
        while !saw_loaded && std::time::Instant::now() < deadline {
            let _ = wake_rx.recv_timeout(std::time::Duration::from_millis(50));
            for ev in player.drain_events() {
                if matches!(ev, MpvEvent::StartFile { .. }) {
                    saw_start = true;
                }
                if matches!(ev, MpvEvent::FileLoaded { .. }) {
                    saw_loaded = true;
                }
                ordered_events.push(ev);
            }
        }
        assert!(saw_start, "expected START_FILE event in order: {ordered_events:?}");
        assert!(saw_loaded, "expected FILE_LOADED event: {ordered_events:?}");
        let start_idx = ordered_events
            .iter()
            .position(|e| matches!(e, MpvEvent::StartFile { .. }))
            .unwrap();
        let loaded_idx = ordered_events
            .iter()
            .position(|e| matches!(e, MpvEvent::FileLoaded { .. }))
            .unwrap();
        assert!(start_idx < loaded_idx, "START_FILE must precede FILE_LOADED");

        // Add the in-memory WebVTT track via `sub-add "memento-asr://live.vtt"`.
        asr.start(&player, "ASR Live", "en").unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while asr.sid().is_none() && std::time::Instant::now() < deadline {
            let _ = wake_rx.recv_timeout(std::time::Duration::from_millis(50));
            for ev in player.drain_events() {
                asr.on_event(&player, &ev).unwrap();
            }
        }
        let sid = asr.sid().expect("memento-asr:// sub-add must assign positive sid");
        assert!(sid > 0);

        // Push cues out of chronological order; WebVTT sorts them in RAM and triggers `sub-reload`.
        asr.add_cue(
            &player,
            AsrCue {
                id: 2,
                start: 3.5,
                end: 5.0,
                text: "Second ASR cue".into(),
            },
        )
        .unwrap();
        asr.add_cue(
            &player,
            AsrCue {
                id: 1,
                start: 0.5,
                end: 2.0,
                text: "First ASR cue".into(),
            },
        )
        .unwrap();

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while (asr.reload_in_flight.is_some() || asr.reload_dirty)
            && std::time::Instant::now() < deadline
        {
            let _ = wake_rx.recv_timeout(std::time::Duration::from_millis(50));
            for ev in player.drain_events() {
                asr.on_event(&player, &ev).unwrap();
            }
        }
        assert!(
            asr.reload_in_flight.is_none() && !asr.reload_dirty,
            "reload_in_flight={:?} reload_dirty={} deselected={}",
            asr.reload_in_flight,
            asr.reload_dirty,
            asr.deselected_by_user
        );

        // Seek into cue 1 and confirm real `sub-text`, `sub-start`, `sub-end`.
        player.command(&["seek", "1.0", "absolute+exact"]).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut saw_sub_text = false;
        let mut saw_sub_start = false;
        let mut saw_sub_end = false;
        while !(saw_sub_text && saw_sub_start && saw_sub_end)
            && std::time::Instant::now() < deadline
        {
            let _ = wake_rx.recv_timeout(std::time::Duration::from_millis(50));
            for ev in player.drain_events() {
                asr.on_event(&player, &ev).unwrap();
                if let MpvEvent::PropertyChange { name, value } = &ev {
                    match (name.as_str(), value) {
                        ("sub-text", v) if v.as_str() == Some("First ASR cue") => {
                            saw_sub_text = true;
                        }
                        ("sub-start", MpvPropertyValue::Double(v)) if (*v - 0.5).abs() < 0.01 => {
                            saw_sub_start = true;
                        }
                        ("sub-end", MpvPropertyValue::Double(v)) if (*v - 2.0).abs() < 0.01 => {
                            saw_sub_end = true;
                        }
                        _ => {}
                    }
                }
            }
        }
        assert!(saw_sub_text, "expected sub-text == 'First ASR cue'");
        assert!(saw_sub_start, "expected sub-start == 0.5");
        assert!(saw_sub_end, "expected sub-end == 2.0");

        // Exercise `sub-seek 1 primary` and verify jump to cue 2.
        player.subtitle_seek(1, true).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut saw_seek_event = false;
        let mut saw_second_cue = false;
        while !(saw_seek_event && saw_second_cue) && std::time::Instant::now() < deadline {
            let _ = wake_rx.recv_timeout(std::time::Duration::from_millis(50));
            for ev in player.drain_events() {
                asr.on_event(&player, &ev).unwrap();
                match &ev {
                    MpvEvent::Seek => saw_seek_event = true,
                    MpvEvent::PropertyChange { name, value }
                        if name == "sub-text" && value.as_str() == Some("Second ASR cue") =>
                    {
                        saw_second_cue = true;
                    }
                    _ => {}
                }
            }
        }
        assert!(saw_seek_event, "expected MPV_EVENT_SEEK after sub-seek");
        assert!(saw_second_cue, "expected sub-text == 'Second ASR cue' after sub-seek");
        assert_eq!(player.primary_sid(), sid);
        assert_eq!(player.get_sub_text().as_deref(), Some("Second ASR cue"));
        assert!((player.get_sub_start() - 3.5).abs() < 0.01);
        assert!((player.get_sub_end() - 5.0).abs() < 0.01);
    }

    #[test]
    fn command_node_async_captures_command_reply_without_blocking() {
        let player = MpvInstance::new_software(None).unwrap();
        let req = player.command_node_async(&["expand-text", "hello-asr"]).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        let mut reply = None;
        while reply.is_none() && std::time::Instant::now() < deadline {
            for ev in player.drain_events() {
                if let MpvEvent::CommandReply {
                    reply_userdata,
                    error,
                    result,
                } = ev
                {
                    if reply_userdata == req {
                        reply = Some((error, result));
                    }
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(
            reply,
            Some((0, CommandReplyResult::String("hello-asr".into())))
        );
    }

    #[test]
    fn mpv_event_to_engine_event_maps_observed_properties_and_lifecycle() {
        let playback = PlaybackState {
            sub_start: 1.5,
            sub_end: 3.5,
            ..Default::default()
        };
        assert_eq!(
            MpvEvent::StartFile { playlist_entry_id: 1 }.to_engine_event(&playback),
            Some(EngineEvent::StartFile)
        );
        assert_eq!(
            MpvEvent::FileLoaded {
                video_width: 1920,
                video_height: 1080
            }
            .to_engine_event(&playback),
            Some(EngineEvent::FileLoaded {
                video_width: 1920,
                video_height: 1080
            })
        );
        assert_eq!(
            MpvEvent::PropertyChange {
                name: "mute".into(),
                value: MpvPropertyValue::Flag(true),
            }
            .to_engine_event(&playback),
            Some(EngineEvent::Property(PropertyChange::Mute(true)))
        );
        assert_eq!(
            MpvEvent::PropertyChange {
                name: "volume-max".into(),
                value: MpvPropertyValue::Int64(130),
            }
            .to_engine_event(&playback),
            Some(EngineEvent::Property(PropertyChange::VolumeMax(130)))
        );
        assert_eq!(
            MpvEvent::PropertyChange {
                name: "sub-text".into(),
                value: MpvPropertyValue::String("こんにちは".into()),
            }
            .to_engine_event(&playback),
            Some(EngineEvent::Property(PropertyChange::SubText {
                text: "こんにちは".into(),
                start: Some(1.5),
                end: Some(3.5),
            }))
        );
    }

    #[test]
    fn drop_joins_screenshot_worker_without_waiting_ten_seconds() {
        let player = MpvInstance::new_software(None).unwrap();
        let pending = player.request_screenshot(false).unwrap();
        let started = std::time::Instant::now();
        drop(player);
        assert!(
            started.elapsed() < std::time::Duration::from_secs(2),
            "MpvInstance::drop blocked waiting for screenshot worker"
        );
        let _ = pending.wait();
    }

    #[test]
    fn mode2_asr_track_state_machine_buffers_cues_until_sub_create_reply() {
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

        // Emulate `sub-create` reply failing cleanly.
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
    }

    #[test]
    fn live_asr_stream_cb_with_preexisting_subtitle_track_binds_sid_2_not_1() {
        let dir = TestTempDir::new("memento-asr-sid2-test");
        std::fs::write(dir.path().join("input.conf"), "").unwrap();
        std::fs::write(dir.path().join("mpv.conf"), "ao=null\nhwdec=no\n").unwrap();
        let existing_vtt = dir.path().join("existing.vtt");
        std::fs::write(
            &existing_vtt,
            "WEBVTT\n\n00:00:00.200 --> 00:00:04.000\nEmbedded track cue\n\n",
        )
        .unwrap();

        let player = MpvInstance::new_software(Some(dir.path())).unwrap();
        let (wake_tx, wake_rx) = mpsc::channel();
        player.set_wakeup_callback(move || {
            let _ = wake_tx.send(());
        });

        player
            .load_file("av://lavfi:testsrc=size=64x64:rate=10:duration=6,format=yuv420p")
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !player.playback_state().file_loaded && std::time::Instant::now() < deadline {
            let _ = wake_rx.recv_timeout(std::time::Duration::from_millis(50));
            let _ = player.drain_events();
        }
        assert!(player.playback_state().file_loaded);

        // Add and select an already-active subtitle track (`sid = 1`) BEFORE ASR starts.
        let vtt_str = existing_vtt.to_string_lossy().into_owned();
        player
            .command_node_async(&["sub-add", &vtt_str, "select", "Existing"])
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while player.primary_sid() != 1 && std::time::Instant::now() < deadline {
            let _ = wake_rx.recv_timeout(std::time::Duration::from_millis(50));
            let _ = player.drain_events();
        }
        assert_eq!(player.primary_sid(), 1);

        // Start ASR track (`sid = 2`) and immediately enqueue a cue while `sub-add` is in flight
        // so `reload_dirty` is set when `CommandReply` arrives ahead of `track-list`.
        let mut asr = AsrSubtitleTrack::with_mode(AsrTrackMode::StreamCallback);
        asr.start(&player, "ASR Live", "ja").unwrap();
        asr.add_cue(
            &player,
            AsrCue {
                id: 1,
                start: 0.5,
                end: 3.0,
                text: "ASR track 2 cue".into(),
            },
        )
        .unwrap();

        let mut emitted_sids = Vec::new();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while (asr.sid() != Some(2)
            || player.primary_sid() != 2
            || asr.reload_in_flight.is_some()
            || asr.reload_dirty)
            && std::time::Instant::now() < deadline
        {
            let _ = wake_rx.recv_timeout(std::time::Duration::from_millis(50));
            // Also exercise mixing `poll_playback_state()` before `drain_events()`!
            let _ = player.poll_playback_state();
            for ev in player.drain_events() {
                if let Some(sid) = asr.on_event(&player, &ev).unwrap() {
                    emitted_sids.push(sid);
                }
            }
        }
        assert_eq!(asr.sid(), Some(2), "ASR sid must resolve to 2, never poisoned with 1");
        assert_eq!(emitted_sids, vec![2], "on_event must emit Some(2) exactly once");
        assert_eq!(player.primary_sid(), 2);

        // Update cues again (`sub-reload 2`) and seek into cue to confirm `sub-text` comes from ASR.
        asr.add_cue(
            &player,
            AsrCue {
                id: 1,
                start: 0.5,
                end: 3.0,
                text: "Updated ASR track 2 cue".into(),
            },
        )
        .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while (asr.reload_in_flight.is_some() || asr.reload_dirty)
            && std::time::Instant::now() < deadline
        {
            let _ = wake_rx.recv_timeout(std::time::Duration::from_millis(50));
            for ev in player.drain_events() {
                asr.on_event(&player, &ev).unwrap();
            }
        }
        player.command(&["seek", "1.0", "absolute+exact"]).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while player.get_sub_text().as_deref() != Some("Updated ASR track 2 cue")
            && std::time::Instant::now() < deadline
        {
            let _ = wake_rx.recv_timeout(std::time::Duration::from_millis(50));
            for ev in player.drain_events() {
                asr.on_event(&player, &ev).unwrap();
            }
        }
        assert_eq!(
            player.get_sub_text().as_deref(),
            Some("Updated ASR track 2 cue")
        );
    }

    #[test]
    fn live_sub_start_sub_end_precede_sub_text_and_drive_autopause_across_cues() {
        use crate::autopause::AutoPauseState;

        let dir = TestTempDir::new("memento-sub-order-test");
        std::fs::write(dir.path().join("input.conf"), "").unwrap();
        std::fs::write(dir.path().join("mpv.conf"), "ao=null\nhwdec=no\n").unwrap();
        let vtt = dir.path().join("cues.vtt");
        std::fs::write(
            &vtt,
            "WEBVTT\n\n\
             00:00:00.400 --> 00:00:01.000\n\
             Cue One\n\n\
             00:00:01.600 --> 00:00:02.200\n\
             Cue Two\n\n",
        )
        .unwrap();

        let player = MpvInstance::new_software(Some(dir.path())).unwrap();
        let (wake_tx, wake_rx) = mpsc::channel();
        player.set_wakeup_callback(move || {
            let _ = wake_tx.send(());
        });
        player
            .load_file("av://lavfi:testsrc=size=64x64:rate=25:duration=4,format=yuv420p")
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !player.playback_state().file_loaded && std::time::Instant::now() < deadline {
            let _ = wake_rx.recv_timeout(std::time::Duration::from_millis(50));
            let _ = player.drain_events();
        }
        let vtt_str = vtt.to_string_lossy().into_owned();
        player
            .command_node_async(&["sub-add", &vtt_str, "select", "Test"])
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while player.primary_sid() != 1 && std::time::Instant::now() < deadline {
            let _ = wake_rx.recv_timeout(std::time::Duration::from_millis(50));
            let _ = player.drain_events();
        }

        // Play continuously from 0.0 across Cue 1 [0.4, 1.0] -> gap [1.0, 1.6] -> Cue 2 [1.6, 2.2]
        // while rendering software frames so mpv advances its clock deterministically.
        player.play().unwrap();
        let mut pixels = AlignedBytes([0; 16]);
        let mut sub_events: Vec<MpvEvent> = Vec::new();
        let mut saw_cue2 = false;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
        while !saw_cue2 && std::time::Instant::now() < deadline {
            let _ = wake_rx.recv_timeout(std::time::Duration::from_millis(20));
            if player.take_render_update() {
                let _ = player.render_rgba(2, 2, &mut pixels.0);
            }
            for ev in player.drain_events() {
                if let MpvEvent::PropertyChange { name, value } = &ev {
                    if matches!(name.as_str(), "sub-start" | "sub-end" | "sub-text") {
                        if name == "sub-text" && value.as_str() == Some("Cue Two") {
                            saw_cue2 = true;
                        }
                        sub_events.push(ev);
                    }
                }
            }
        }
        assert!(saw_cue2, "did not observe Cue Two: {sub_events:?}");

        let mut cur_start = 0.0f64;
        let mut cur_end = 0.0f64;
        let mut saw_cue1_string = false;

        for ev in &sub_events {
            if let MpvEvent::PropertyChange { name, value } = ev {
                match name.as_str() {
                    "sub-start" => {
                        cur_start = match value {
                            MpvPropertyValue::Double(v) => *v,
                            _ => 0.0,
                        };
                    }
                    "sub-end" => {
                        cur_end = match value {
                            MpvPropertyValue::Double(v) => *v,
                            _ => 0.0,
                        };
                    }
                    "sub-text" => {
                        // Must decode as `MpvPropertyValue::String` for downstream player consumers.
                        let MpvPropertyValue::String(text) = value else {
                            panic!("expected MpvPropertyValue::String on sub-text, got {value:?}");
                        };
                        if text == "Cue One" {
                            saw_cue1_string = true;
                            assert!(
                                (cur_start - 0.4).abs() < 0.05 && (cur_end - 1.0).abs() < 0.05,
                                "sub-start/sub-end must arrive BEFORE Cue One sub-text: got ({cur_start}, {cur_end})"
                            );
                        } else if text.is_empty() && cur_end == 0.0 {
                            assert!(
                                cur_start == 0.0 && cur_end == 0.0,
                                "sub-start/sub-end must clear to 0.0 BEFORE empty sub-text: got ({cur_start}, {cur_end})"
                            );
                        } else if text == "Cue Two" {
                            assert!(
                                (cur_start - 1.6).abs() < 0.05 && (cur_end - 2.2).abs() < 0.05,
                                "sub-start/sub-end must arrive BEFORE Cue Two sub-text: got ({cur_start}, {cur_end})"
                            );
                        }
                    }
                    _ => {}
                }
            }
        }
        assert!(saw_cue1_string);

        // Convert the entire multi-cue batch at once via `MpvEvent::to_engine_events` (even when
        // `player.playback_state()` now holds Cue Two's end-of-batch `[1.6, 2.2]`!) and verify
        // rolling `(start, end)` drives `AutoPauseState::step` accurately across Cue 1 -> gap -> Cue 2.
        let engine_events = MpvEvent::to_engine_events(&sub_events, &PlaybackState::default());
        let mut autopause = AutoPauseState::default();
        let mut triggered_seeks = Vec::new();
        for eng_ev in &engine_events {
            if let EngineEvent::Property(PropertyChange::SubText {
                text,
                start: Some(snap_s),
                end: Some(snap_e),
            }) = eng_ev
            {
                if text == "Cue One" {
                    assert!((*snap_s - 0.4).abs() < 0.05 && (*snap_e - 1.0).abs() < 0.05);
                } else if text == "Cue Two" {
                    assert!((*snap_s - 1.6).abs() < 0.05 && (*snap_e - 2.2).abs() < 0.05);
                }
                if let Some(seek_target) =
                    autopause.step(true, *snap_s, *snap_e, 1.0, 0.0, false, 4.0)
                {
                    triggered_seeks.push(seek_target);
                }
            }
        }

        // Cue 1 [0.4, 1.0] -> gap [0.0, 0.0] triggers autopause seek to 1.0 - 0.1 = 0.9;
        // Cue 2 [1.6, 2.2] arms autopause (end_time == 2.2) without prematurely seeking!
        assert_eq!(triggered_seeks.len(), 1, "triggered_seeks={triggered_seeks:?}");
        assert!((triggered_seeks[0] - 0.9).abs() < 0.05);
        assert!((autopause.end_time - 2.2).abs() < 0.05);
    }

    #[test]
    fn live_webvtt_html_entities_and_arrows_round_trip_verbatim() {
        let dir = TestTempDir::new("memento-webvtt-escape-test");
        std::fs::write(dir.path().join("input.conf"), "").unwrap();
        std::fs::write(dir.path().join("mpv.conf"), "ao=null\nhwdec=no\n").unwrap();

        let tricky = "<音楽> 1 < 2 & 3 --> end";
        let formatted = String::from_utf8(format_webvtt_cues(&[AsrCue {
            id: 1,
            start: 0.2,
            end: 3.0,
            text: tricky.into(),
        }]))
        .unwrap();
        assert!(formatted.contains("&lt;音楽&gt; 1 &lt; 2 &amp; 3 --&gt; end"));

        let player = MpvInstance::new_software(Some(dir.path())).unwrap();
        let (wake_tx, wake_rx) = mpsc::channel();
        player.set_wakeup_callback(move || {
            let _ = wake_tx.send(());
        });
        player
            .load_file("av://lavfi:testsrc=size=64x64:rate=10:duration=4,format=yuv420p")
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !player.playback_state().file_loaded && std::time::Instant::now() < deadline {
            let _ = wake_rx.recv_timeout(std::time::Duration::from_millis(50));
            let _ = player.drain_events();
        }

        let mut asr = AsrSubtitleTrack::with_mode(AsrTrackMode::StreamCallback);
        asr.start(&player, "ASR", "ja").unwrap();
        asr.add_cue(
            &player,
            AsrCue {
                id: 1,
                start: 0.2,
                end: 3.0,
                text: tricky.into(),
            },
        )
        .unwrap();

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while (asr.sid().is_none() || asr.reload_in_flight.is_some() || asr.reload_dirty)
            && std::time::Instant::now() < deadline
        {
            let _ = wake_rx.recv_timeout(std::time::Duration::from_millis(50));
            for ev in player.drain_events() {
                asr.on_event(&player, &ev).unwrap();
            }
        }

        player.command(&["seek", "1.0", "absolute+exact"]).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while player.get_sub_text().as_deref() != Some(tricky)
            && std::time::Instant::now() < deadline
        {
            let _ = wake_rx.recv_timeout(std::time::Duration::from_millis(50));
            for ev in player.drain_events() {
                asr.on_event(&player, &ev).unwrap();
            }
        }
        assert_eq!(player.get_sub_text().as_deref(), Some(tricky));
    }

    #[test]
    fn live_switching_from_audio_file_to_silent_video_resets_aid_and_sid_and_cli_options_apply_once() {
        let dir = TestTempDir::new("memento-silent-switch-test");
        std::fs::write(dir.path().join("input.conf"), "").unwrap();
        std::fs::write(
            dir.path().join("mpv.conf"),
            "ao=null\nhwdec=no\nglsl-shaders=/tmp/conf_shader.glsl\n",
        )
        .unwrap();
        let ext_vtt = dir.path().join("sub.vtt");
        std::fs::write(&ext_vtt, "WEBVTT\n\n00:00:00.100 --> 00:00:01.500\nhi\n\n").unwrap();

        let player = MpvInstance::new_with_options(
            Some(dir.path()),
            &[
                CliOption {
                    name: "hwdec".into(),
                    value: "vaapi".into(),
                },
                CliOption {
                    name: "glsl-shaders-append".into(),
                    value: "/tmp/cli_shader.glsl".into(),
                },
                CliOption {
                    name: "glsl-shader".into(),
                    value: "/tmp/cli_alias_shader.glsl".into(),
                },
            ],
        )
        .unwrap();
        // Verify CLI overrides mpv.conf AND list-appending options are applied ONCE (not duplicated).
        assert_eq!(player.configured_hwdec(), "vaapi");
        let shaders = unsafe {
            let raw = ffi::mpv_get_property_string(player.raw_handle(), c"glsl-shaders".as_ptr());
            let s = copy_mpv_string(raw);
            if !raw.is_null() {
                ffi::mpv_free(raw.cast());
            }
            s
        };
        assert_eq!(
            shaders.matches("/tmp/cli_shader.glsl").count(),
            1,
            "glsl-shaders-append must not be applied twice: {shaders}"
        );
        assert_eq!(
            shaders.matches("/tmp/cli_alias_shader.glsl").count(),
            1,
            "glsl-shader must not be applied twice: {shaders}"
        );

        let (wake_tx, wake_rx) = mpsc::channel();
        player.set_wakeup_callback(move || {
            let _ = wake_tx.send(());
        });

        // File 1: audio stream + selected external subtitle track (`aid = 1`, `sid = 1`).
        player
            .load_file("av://lavfi:sine=frequency=440:duration=2")
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while (!player.playback_state().file_loaded || player.selected_aid() != 1)
            && std::time::Instant::now() < deadline
        {
            let _ = wake_rx.recv_timeout(std::time::Duration::from_millis(50));
            let _ = player.drain_events();
        }
        assert_eq!(player.selected_aid(), 1);
        let vtt_str = ext_vtt.to_string_lossy().into_owned();
        player
            .command_node_async(&["sub-add", &vtt_str, "select", "Sub"])
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while player.primary_sid() != 1 && std::time::Instant::now() < deadline {
            let _ = wake_rx.recv_timeout(std::time::Duration::from_millis(50));
            let _ = player.drain_events();
        }
        assert_eq!(player.primary_sid(), 1);
        assert_eq!(player.capture_encode_context().unwrap().audio_id, 1);

        // File 2: silent video with 0 audio tracks and 0 subtitle tracks (`aid=auto`, `sid=auto`).
        player
            .load_file("av://lavfi:testsrc=size=64x64:rate=10:duration=2,format=yuv420p")
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut saw_second_loaded = false;
        while !saw_second_loaded && std::time::Instant::now() < deadline {
            let _ = wake_rx.recv_timeout(std::time::Duration::from_millis(50));
            for ev in player.drain_events() {
                if matches!(ev, MpvEvent::FileLoaded { .. }) {
                    saw_second_loaded = true;
                }
            }
        }
        // Drain any trailing `track-list` property update.
        let settle = std::time::Instant::now() + std::time::Duration::from_millis(150);
        while std::time::Instant::now() < settle {
            let _ = wake_rx.recv_timeout(std::time::Duration::from_millis(25));
            let _ = player.drain_events();
        }
        assert!(saw_second_loaded);
        assert_eq!(player.selected_aid(), 0, "silent video must reset audio_id to 0");
        assert_eq!(player.primary_sid(), 0, "silent video must reset primary_sid to 0");
        assert_eq!(player.secondary_sid(), 0);
        assert_eq!(player.capture_encode_context().unwrap().audio_id, 0);
    }

    #[test]
    fn mode2_clear_resets_pending_adds_preventing_reused_local_id_collision() {
        let player = MpvInstance::new_software(None).unwrap();
        let mut asr = AsrSubtitleTrack::with_mode(AsrTrackMode::NativeMemory);
        asr.sid = Some(3);
        // Simulate pre-clear `add-subtitle` (userdata = 10, local_id = 0) still awaiting reply.
        asr.pending_adds.insert(10, 0);
        asr.cues.push(AsrCue {
            id: 0,
            start: 1.0,
            end: 2.0,
            text: "old track cue".into(),
        });

        // Clear: on stock libmpv `clear_memory_subtitles_async` returns Err because Mode 2 is
        // unsupported, so verify the state-clearing invariant before/independent of IPC dispatch.
        let _ = asr.clear(&player);
        assert!(asr.pending_adds.is_empty(), "clear must reset pending_adds");
        assert!(asr.pending_removals.is_empty(), "clear must reset pending_removals");
        assert!(asr.mpv_cue_ids.is_empty());
        assert!(asr.pending_edits.is_empty());

        // Pre-clear reply (userdata = 10) arriving after `clear-subtitles` must be ignored cleanly,
        // and a post-clear add (userdata = 12, reused local_id = 0) must bind its new mpv_cue_id!
        asr.pending_adds.insert(12, 0);
        asr.on_event(
            &player,
            &MpvEvent::CommandReply {
                reply_userdata: 10,
                error: 0,
                result: CommandReplyResult::Int64(55),
            },
        )
        .unwrap();
        assert!(!asr.mpv_cue_ids.contains_key(&0));

        asr.on_event(
            &player,
            &MpvEvent::CommandReply {
                reply_userdata: 12,
                error: 0,
                result: CommandReplyResult::Int64(56),
            },
        )
        .unwrap();
        assert_eq!(asr.mpv_cue_ids.get(&0), Some(&56));
    }
}


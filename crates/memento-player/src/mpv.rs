//! libmpv bindings for the movie player's renderer-backed handle and an
//! independent audio-only pronunciation handle. Both use the same SDK header
//! as the C++ player; audio playback never borrows or replaces the movie handle.

use std::ffi::{c_char, c_int, CStr, CString};
use std::path::Path;
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};

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

/// An initialized mpv instance using libmpv's software renderer. Rendering
/// writes into caller-owned RGBA8 images so Slint can composite its UI above
/// the video without a native child window covering the top-level surface.
pub struct MpvInstance {
    handle: *mut ffi::mpv_handle,
    render: std::ptr::NonNull<ffi::mpv_render_context>,
    // mpv may call the update callback from another thread. Dropped only
    // after unregistering it and freeing the render context.
    render_pending: Box<AtomicBool>,
}
unsafe impl Send for MpvInstance {}

unsafe extern "C" fn on_render_update(userdata: *mut std::ffi::c_void) {
    if !userdata.is_null() {
        unsafe { &*(userdata as *const AtomicBool) }.store(true, Ordering::Release);
    }
}

impl MpvInstance {
    /// Initialize mpv and its software renderer before the first media load.
    /// Use [`Self::render_rgba`] to draw into an image in the UI's scene graph.
    pub fn new_software(config_dir: Option<&Path>) -> Result<Self, String> {
        Self::create(config_dir)
    }

    fn create(config_dir: Option<&Path>) -> Result<Self, String> {
        let handle = unsafe { ffi::mpv_create() };
        if handle.is_null() {
            return Err("Failed to create mpv instance".into());
        }

        let initialize = || {
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
                (c"profile", c"fast"),
                (c"hwdec", c"auto-safe"),
                (c"hwdec-codecs", c"all"),
                (c"framedrop", c"vo"),
                (c"vd-lavc-threads", c"0"),
                (c"sws-scaler", c"fast-bilinear"),
                (c"sws-fast", c"yes"),
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
            let status = opt(c"vo", c"libmpv");
            if status < 0 {
                return Err(format!("mpv libmpv video option failed with code {status}"));
            }

            let status = unsafe { ffi::mpv_initialize(handle) };
            if status < 0 {
                return Err(format!("mpv initialization failed with code {status}"));
            }

            let mut context = ptr::null_mut();
            let mut params = [
                ffi::mpv_render_param {
                    type_: ffi::MPV_RENDER_PARAM_API_TYPE,
                    data: c"sw".as_ptr().cast_mut().cast(),
                },
                ffi::mpv_render_param {
                    type_: ffi::MPV_RENDER_PARAM_INVALID,
                    data: ptr::null_mut(),
                },
            ];
            let status = unsafe {
                ffi::mpv_render_context_create(&mut context, handle, params.as_mut_ptr())
            };
            if status < 0 {
                if !context.is_null() {
                    unsafe { ffi::mpv_render_context_free(context) };
                }
                return Err(format!("mpv software render context failed with code {status}"));
            }
            let Some(context) = std::ptr::NonNull::new(context) else {
                return Err("mpv software render context was null".into());
            };
            let mut pending = Box::new(AtomicBool::new(true));
            unsafe {
                ffi::mpv_render_context_set_update_callback(
                    context.as_ptr(),
                    Some(on_render_update),
                    (&mut *pending as *mut AtomicBool).cast(),
                );
            }
            Ok((context, pending))
        };

        match initialize() {
            Ok((render, render_pending)) => Ok(Self {
                handle,
                render,
                render_pending,
            }),
            Err(error) => {
                unsafe { ffi::mpv_terminate_destroy(handle) };
                Err(error)
            }
        }
    }

    /// Consume the render callback notification on the UI/render thread.
    /// A `true` result means a new frame (or video configuration) needs a
    /// redraw. Render again on a viewport resize even if this returns false.
    pub fn take_render_update(&self) -> bool {
        if !self.render_pending.swap(false, Ordering::AcqRel) {
            return false;
        }
        unsafe {
            ffi::mpv_render_context_update(self.render.as_ptr())
                & u64::from(ffi::MPV_RENDER_UPDATE_FRAME)
                != 0
        }
    }

    /// Render directly into caller memory (row-major, top-down RGBA8, tightly
    /// packed, opaque). The buffer must remain exclusively borrowed until
    /// mpv returns. mpv requires at least 4-byte pixel alignment; a 64-byte
    /// aligned buffer and stride are preferred for SIMD performance.
    pub fn render_rgba(&self, width: u32, height: u32, pixels: &mut [u8]) -> Result<(), String> {
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
            ffi::mpv_render_context_render(self.render.as_ptr(), params.as_mut_ptr())
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

    fn with_tracks<R>(&self, read: impl FnOnce(&[MementoTrackC]) -> R) -> R {
        let mut node = OwnedNode::new();
        let status = unsafe {
            ffi::mpv_get_property(
                self.handle,
                c"track-list".as_ptr(),
                ffi::MPV_FORMAT_NODE,
                (&mut node.0 as *mut ffi::mpv_node).cast(),
            )
        };
        if status < 0 {
            return read(&[]);
        }
        let tracks = OwnedTrackList(unsafe { memento_player_extract_tracks(&node.0) });
        read(tracks.as_slice())
    }

    /// Read the live primary `sub-lines` property into owned subtitle cues.
    ///
    /// mpv node strings are copied before the node is freed, so returned text
    /// remains valid independently of libmpv's property storage.
    pub fn primary_sub_lines(&self) -> Vec<SubLine> {
        let mut node = OwnedNode::new();
        let status = unsafe {
            ffi::mpv_get_property(
                self.handle,
                c"sub-lines".as_ptr(),
                ffi::MPV_FORMAT_NODE,
                (&mut node.0 as *mut ffi::mpv_node).cast(),
            )
        };
        if status < 0 {
            return Vec::new();
        }

        let lines = OwnedSubLineList(unsafe { memento_player_extract_sub_lines(&node.0) });
        lines
            .as_slice()
            .iter()
            .map(|line| SubLine {
                id: line.id,
                text: unsafe { CStr::from_ptr(line.text) }
                    .to_string_lossy()
                    .into_owned(),
                start: line.start,
                end: line.end,
            })
            .collect()
    }

    pub fn audio_tracks(&self) -> Vec<AudioTrack> {
        self.with_tracks(|tracks| {
            tracks
                .iter()
                .filter(|track| track.track_type == TrackType::Audio as i32)
                .map(|track| AudioTrack {
                    id: track.id,
                    language: copy_mpv_string(track.lang),
                    title: copy_mpv_string(track.title),
                    external_filename: copy_mpv_string(track.external_filename),
                    external: track.external,
                    default_track: track.default_track,
                    selected: track.selected,
                })
                .collect()
        })
    }

    pub fn subtitle_tracks(&self) -> Vec<SubtitleTrack> {
        self.with_tracks(|tracks| {
            tracks
                .iter()
                .filter(|track| track.track_type == TrackType::Subtitle as i32)
                .map(|track| SubtitleTrack {
                    id: track.id,
                    external_path: copy_mpv_string(track.external_filename),
                    language: copy_mpv_string(track.lang),
                    title: copy_mpv_string(track.title),
                    external: track.external,
                    default_track: track.default_track,
                    selected: track.selected,
                })
                .collect()
        })
    }

    pub fn supports_memory_subtitles(&self) -> bool {
        unsafe { memento_player_supports_memory_subtitles(self.handle) }
    }

    /// Returns the native decoded dimensions of the current video stream, or
    /// (0, 0) if no video is playing.
    pub fn video_dimensions(&self) -> (u32, u32) {
        let w = self.get_i64(c"dwidth", 0);
        let h = self.get_i64(c"dheight", 0);
        if w > 0 && h > 0 {
            (w as u32, h as u32)
        } else {
            (0, 0)
        }
    }

    fn get_i64(&self, property: &CStr, default: i64) -> i64 {
        let mut value = default;
        let status = unsafe {
            ffi::mpv_get_property(
                self.handle,
                property.as_ptr(),
                ffi::MPV_FORMAT_INT64,
                (&mut value as *mut i64).cast(),
            )
        };
        if status < 0 { default } else { value }
    }

    fn get_f64(&self, property: &CStr, default: f64) -> f64 {
        let mut value = default;
        let status = unsafe {
            ffi::mpv_get_property(
                self.handle,
                property.as_ptr(),
                ffi::MPV_FORMAT_DOUBLE,
                (&mut value as *mut f64).cast(),
            )
        };
        if status < 0 { default } else { value }
    }

    fn get_string(&self, property: &CStr) -> Option<String> {
        unsafe {
            let value = ffi::mpv_get_property_string(self.handle, property.as_ptr());
            if value.is_null() {
                None
            } else {
                let text = CStr::from_ptr(value).to_string_lossy().into_owned();
                ffi::mpv_free(value.cast());
                Some(text)
            }
        }
    }

    pub fn capture_encode_context(&self) -> Option<EncodeContext> {
        let path = self.get_string(c"path")?;
        if path.is_empty() {
            return None;
        }
        Some(EncodeContext {
            path,
            audio_id: self.selected_aid(),
        })
    }

    /// Capture the active video at source resolution without the software
    /// renderer's viewport scaling or any letterbox bars.
    pub fn screenshot_raw_rgba(&self, subtitles: bool) -> Result<RawFrame, String> {
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
            return Err(format!("mpv screenshot-raw failed with code {status}"));
        }

        let Some(fields) = (unsafe { iter_node_map(&result.0) }) else {
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

    pub fn selected_aid(&self) -> i64 {
        self.get_i64(c"aid", 0)
    }

    pub fn primary_sid(&self) -> i64 {
        self.get_i64(c"sid", 0)
    }

    pub fn secondary_sid(&self) -> i64 {
        self.get_i64(c"secondary-sid", 0)
    }

    pub fn get_sub_start(&self) -> f64 {
        self.get_f64(c"sub-start", 0.0)
    }

    pub fn get_sub_end(&self) -> f64 {
        self.get_f64(c"sub-end", 0.0)
    }

    pub fn get_sub_delay(&self) -> f64 {
        self.get_f64(c"sub-delay", 0.0)
    }

    pub fn get_sub_speed(&self) -> f64 {
        self.get_f64(c"sub-speed", 1.0)
    }

    pub fn get_sub_raw_time(&self, playback_time: f64) -> f64 {
        (playback_time - self.get_sub_delay()) / self.get_sub_speed()
    }

    pub fn get_secondary_sub_start(&self) -> f64 {
        self.get_f64(c"secondary-sub-start", 0.0)
    }

    pub fn get_secondary_sub_end(&self) -> f64 {
        self.get_f64(c"secondary-sub-end", 0.0)
    }

    pub fn get_secondary_sub_delay(&self) -> f64 {
        self.get_f64(c"secondary-sub-delay", 0.0)
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

    fn command_async(&self, args: &[&str]) -> Result<(), String> {
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

    fn command_ret_id(&self, args: &[&str]) -> Result<i64, String> {
        let c_args: Result<Vec<std::ffi::CString>, _> =
            args.iter().map(|arg| std::ffi::CString::new(*arg)).collect();
        let c_args = c_args.map_err(|error| error.to_string())?;
        let mut pointers: Vec<*const c_char> =
            c_args.iter().map(|arg| arg.as_ptr()).collect();
        pointers.push(ptr::null());
        let mut id = -1;
        let status = unsafe {
            memento_player_command_ret_i64(self.handle, pointers.as_ptr(), &mut id)
        };
        if status < 0 {
            Err(format!(
                "mpv {} command failed with code {status}",
                args.first().copied().unwrap_or("command")
            ))
        } else {
            Ok(id)
        }
    }

    fn validate_memory_subtitle_cue(
        &self,
        track_id: i64,
        start: f64,
        end: f64,
        text: &std::ffi::CString,
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

    pub fn create_memory_subtitle_track(
        &self,
        title: &str,
        language: &str,
    ) -> Result<i64, String> {
        if !self.supports_memory_subtitles() {
            return Err("mpv does not support in-memory subtitles".into());
        }
        if self.capture_encode_context().is_none() {
            return Err("no media file is loaded".into());
        }
        self.command_ret_id(&["sub-create", title, language])
    }

    pub fn add_memory_subtitle(
        &self,
        track_id: i64,
        start: f64,
        end: f64,
        text: &str,
    ) -> Result<i64, String> {
        let text = std::ffi::CString::new(text).map_err(|error| error.to_string())?;
        self.validate_memory_subtitle_cue(track_id, start, end, &text)?;
        let track_id = track_id.to_string();
        let start = start.to_string();
        let end = end.to_string();
        self.command_ret_id(&[
            "add-subtitle",
            &track_id,
            &start,
            &end,
            text.to_str().map_err(|error| error.to_string())?,
        ])
    }

    pub fn edit_memory_subtitle(
        &self,
        track_id: i64,
        cue_id: i64,
        start: f64,
        end: f64,
        text: &str,
    ) -> Result<(), String> {
        let text = std::ffi::CString::new(text).map_err(|error| error.to_string())?;
        self.validate_memory_subtitle_cue(track_id, start, end, &text)?;
        if !memento_player_id_valid(cue_id) {
            return Err("invalid in-memory subtitle cue ID".into());
        }
        let track_id = track_id.to_string();
        let cue_id = cue_id.to_string();
        let start = start.to_string();
        let end = end.to_string();
        self.command(&[
            "edit-subtitle",
            &track_id,
            &cue_id,
            &start,
            &end,
            text.to_str().map_err(|error| error.to_string())?,
        ])
    }

    pub fn remove_memory_subtitle(&self, track_id: i64, cue_id: i64) -> Result<(), String> {
        if !self.supports_memory_subtitles() {
            return Err("mpv does not support in-memory subtitles".into());
        }
        if !memento_player_id_valid(track_id) || !memento_player_id_valid(cue_id) {
            return Err("invalid in-memory subtitle ID".into());
        }
        let track_id = track_id.to_string();
        let cue_id = cue_id.to_string();
        self.command(&["remove-subtitle", &track_id, &cue_id])
    }

    pub fn clear_memory_subtitles(&self, track_id: i64) -> Result<(), String> {
        if !self.supports_memory_subtitles() {
            return Err("mpv does not support in-memory subtitles".into());
        }
        if !memento_player_id_valid(track_id) {
            return Err("invalid in-memory subtitle track ID".into());
        }
        let track_id = track_id.to_string();
        self.command(&["clear-subtitles", &track_id])
    }

    pub fn remove_memory_subtitle_track(&self, track_id: i64) -> Result<(), String> {
        if !self.supports_memory_subtitles() {
            return Err("mpv does not support in-memory subtitles".into());
        }
        if !memento_player_id_valid(track_id) {
            return Err("invalid in-memory subtitle track ID".into());
        }
        let track_id = track_id.to_string();
        self.command(&["sub-remove", &track_id])
    }

    pub fn select_subtitle_track(&self, track_id: i64) -> Result<(), String> {
        self.set_sid(track_id)
    }


    pub fn raw_handle(&self) -> *mut ffi::mpv_handle {
        self.handle
    }

    pub fn command(&self, args: &[&str]) -> Result<(), String> {
        let c_args: Result<Vec<std::ffi::CString>, _> = args.iter().map(|s| std::ffi::CString::new(*s)).collect();
        let c_args = c_args.map_err(|e| e.to_string())?;
        let mut pt_args: Vec<*const std::ffi::c_char> = c_args.iter().map(|s| s.as_ptr()).collect();
        pt_args.push(std::ptr::null());

        let err = unsafe { ffi::mpv_command(self.handle, pt_args.as_mut_ptr()) };
        if err < 0 {
            Err(format!("mpv_command failed with code {err}"))
        } else {
            Ok(())
        }
    }

    pub fn load_file(&self, path: &str) -> Result<(), String> {
        self.command(&["loadfile", path, "replace"])
    }

    pub fn play(&self) -> Result<(), String> {
        self.command(&["set", "pause", "no"])
    }

    pub fn pause(&self) -> Result<(), String> {
        self.command(&["set", "pause", "yes"])
    }

    pub fn toggle_pause(&self) -> Result<(), String> {
        self.command(&["cycle", "pause"])
    }

    pub fn is_paused(&self) -> bool {
        let prop = std::ffi::CString::new("pause").unwrap();
        let mut val: std::ffi::c_int = 0;
        let err = unsafe {
            ffi::mpv_get_property(
                self.handle,
                prop.as_ptr(),
                ffi::MPV_FORMAT_FLAG,
                (&mut val as *mut std::ffi::c_int).cast(),
            )
        };
        if err >= 0 {
            val != 0
        } else {
            true
        }
    }

    pub fn seek(&self, sec: f64) -> Result<(), String> {
        let sec_str = format!("{:.3}", sec.max(0.0));
        self.command(&["seek", &sec_str, "absolute"])
    }

    pub fn seek_relative(&self, delta: f64) -> Result<(), String> {
        let sec_str = format!("{:.3}", delta);
        self.command(&["seek", &sec_str, "relative"])
    }


    pub fn set_volume(&self, vol: f64) -> Result<(), String> {
        let vol_str = format!("{:.1}", vol.clamp(0.0, 100.0));
        self.command(&["set", "volume", &vol_str])
    }

    pub fn get_volume(&self) -> f64 {
        let prop = std::ffi::CString::new("volume").unwrap();
        let mut val: f64 = 100.0;
        let err = unsafe {
            ffi::mpv_get_property(
                self.handle,
                prop.as_ptr(),
                ffi::MPV_FORMAT_DOUBLE,
                (&mut val as *mut f64).cast(),
            )
        };
        if err >= 0 { val } else { 100.0 }
    }

    pub fn get_position(&self) -> f64 {
        let prop = std::ffi::CString::new("time-pos").unwrap();
        let mut val: f64 = 0.0;
        let err = unsafe {
            ffi::mpv_get_property(
                self.handle,
                prop.as_ptr(),
                ffi::MPV_FORMAT_DOUBLE,
                (&mut val as *mut f64).cast(),
            )
        };
        if err >= 0 { val.max(0.0) } else { 0.0 }
    }

    pub fn get_duration(&self) -> f64 {
        let prop = std::ffi::CString::new("duration").unwrap();
        let mut val: f64 = 0.0;
        let err = unsafe {
            ffi::mpv_get_property(
                self.handle,
                prop.as_ptr(),
                ffi::MPV_FORMAT_DOUBLE,
                (&mut val as *mut f64).cast(),
            )
        };
        if err >= 0 { val.max(0.0) } else { 0.0 }
    }

    pub fn get_sub_text(&self) -> Option<String> {
        let prop = std::ffi::CString::new("sub-text").unwrap();
        unsafe {
            let ptr = ffi::mpv_get_property_string(self.handle, prop.as_ptr());
            if ptr.is_null() {
                None
            } else {
                let s = std::ffi::CStr::from_ptr(ptr).to_string_lossy().into_owned();
                ffi::mpv_free(ptr.cast());
                let trimmed = s.trim();
                if trimmed.is_empty() {
                    None
                } else {
                    Some(trimmed.to_string())
                }
            }
        }
    }

    /// Text decoded by the independently selected secondary subtitle track.
    pub fn get_secondary_sub_text(&self) -> Option<String> {
        self.get_string(c"secondary-sub-text")
            .filter(|text| !text.trim().is_empty())
    }

    pub fn get_media_title(&self) -> Option<String> {
        let prop = std::ffi::CString::new("media-title").unwrap();
        unsafe {
            let ptr = ffi::mpv_get_property_string(self.handle, prop.as_ptr());
            if ptr.is_null() {
                None
            } else {
                let s = std::ffi::CStr::from_ptr(ptr).to_string_lossy().into_owned();
                ffi::mpv_free(ptr.cast());
                Some(s)
            }
        }
    }
}

impl Drop for MpvInstance {
    fn drop(&mut self) {
        unsafe {
            ffi::mpv_render_context_set_update_callback(
                self.render.as_ptr(),
                None,
                ptr::null_mut(),
            );
            ffi::mpv_render_context_free(self.render.as_ptr());
        }
        if !self.handle.is_null() {
            unsafe { ffi::mpv_terminate_destroy(self.handle) };
            self.handle = ptr::null_mut();
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
    }
}


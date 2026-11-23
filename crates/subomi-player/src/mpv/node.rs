//! Low-level `mpv_node` RAII wrapper, iteration helpers, and validation helpers.

use std::ffi::{c_char, CStr};

use super::ffi;

pub(super) const MAX_TEXT_BYTES: usize = 1024 * 1024;
const REQUIRED_COMMANDS: u8 = 0b11_1111;

/// A zero-initialized node is `MPV_FORMAT_NONE`. `mpv_free_node_contents` is
/// safe for that format even if an mpv call failed without writing to the node.
pub(super) struct OwnedNode(pub(super) ffi::mpv_node);

impl OwnedNode {
    pub(super) fn new() -> Self {
        Self(unsafe { std::mem::zeroed() })
    }
}

impl Drop for OwnedNode {
    fn drop(&mut self) {
        unsafe { ffi::mpv_free_node_contents(&mut self.0) }
    }
}

/// Checks cue time/text bounds matching `subtitleCueIsValid`.
pub(super) fn cue_valid(start: f64, end: f64, text: &str) -> bool {
    let bytes = text.as_bytes();
    if !start.is_finite()
        || !end.is_finite()
        || start < 0.0
        || end <= start
        || end * 1000.0 > f64::from(i32::MAX)
        || bytes.len() > MAX_TEXT_BYTES
    {
        return false;
    }
    !bytes.contains(&0)
}

/// Checks that an mpv track/cue ID fits in a positive 32-bit signed integer.
pub(super) fn id_valid(id: i64) -> bool {
    id > 0 && id <= i64::from(i32::MAX)
}

/// Copy a nullable NUL-terminated C string from mpv storage into an owned `String`.
pub(super) fn copy_mpv_string(value: *const c_char) -> String {
    if value.is_null() {
        String::new()
    } else {
        unsafe { CStr::from_ptr(value) }.to_string_lossy().into_owned()
    }
}

/// Iterate over an `MPV_FORMAT_NODE_ARRAY` node.
///
/// # Safety
/// `node` and its nested pointers must be valid for `'a`.
pub(super) unsafe fn iter_node_array<'a>(
    node: *const ffi::mpv_node,
) -> Option<impl Iterator<Item = &'a ffi::mpv_node>> {
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

/// Iterate over an `MPV_FORMAT_NODE_MAP` node.
///
/// # Safety
/// `node` and its nested pointers must be valid for `'a`.
pub(super) unsafe fn iter_node_map<'a>(
    node: &'a ffi::mpv_node,
) -> Option<impl Iterator<Item = (&'a [u8], &'a ffi::mpv_node)>> {
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

/// Borrow the `"name"` string field from a command-list node entry.
pub(super) unsafe fn command_name(node: &ffi::mpv_node) -> Option<&[u8]> {
    let map = unsafe { iter_node_map(node) }?;
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

/// Probe `command-list` on an initialized mpv handle to detect patched
/// in-memory subtitle command support (`sub-create`, `add-subtitle`, etc.).
pub(super) unsafe fn supports_memory_subtitles(handle: *mut ffi::mpv_handle) -> bool {
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
    let Some(entries) = (unsafe { iter_node_array(&result.0) }) else {
        return false;
    };

    let mut found = 0u8;
    for entry in entries {
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

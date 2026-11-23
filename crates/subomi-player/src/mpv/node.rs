//! Low-level `mpv_node` RAII wrapper, safe typed accessors, and validation helpers.

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

    pub(super) fn as_ref(&self) -> MpvNodeRef<'_> {
        MpvNodeRef(&self.0)
    }
}

impl Drop for OwnedNode {
    fn drop(&mut self) {
        unsafe { ffi::mpv_free_node_contents(&mut self.0) }
    }
}

/// Borrowed view over a valid `mpv_node` whose nested pointers live for `'a`.
///
/// Centralizes tag-checked `mpv_node` union reads so callers (`extract.rs`,
/// `audio_instance.rs`, `node.rs`) never touch raw unions directly.
#[derive(Clone, Copy)]
pub(super) struct MpvNodeRef<'a>(&'a ffi::mpv_node);

impl<'a> MpvNodeRef<'a> {
    /// Wrap a raw `mpv_node` pointer from libmpv.
    ///
    /// # Safety
    /// If non-null, `node` and all nested `mpv_node_list`/string pointers inside
    /// it must be valid for `'a`.
    pub(super) unsafe fn from_ptr(node: *const ffi::mpv_node) -> Option<Self> {
        // SAFETY: Caller guarantees pointer validity for `'a`.
        unsafe { node.as_ref() }.map(Self)
    }

    /// Wrap a reference to a valid `mpv_node`.
    ///
    /// # Safety
    /// All nested `mpv_node_list`/string pointers inside `node` must be valid for `'a`.
    pub(super) unsafe fn from_ref(node: &'a ffi::mpv_node) -> Self {
        Self(node)
    }

    pub(super) fn as_i64(self) -> Option<i64> {
        (self.0.format == ffi::MPV_FORMAT_INT64).then(|| unsafe { self.0.u.int64 })
    }

    pub(super) fn as_f64(self) -> Option<f64> {
        (self.0.format == ffi::MPV_FORMAT_DOUBLE).then(|| unsafe { self.0.u.double_ })
    }

    pub(super) fn as_bool(self) -> Option<bool> {
        (self.0.format == ffi::MPV_FORMAT_FLAG).then(|| unsafe { self.0.u.flag != 0 })
    }

    pub(super) fn as_str_bytes(self) -> Option<&'a [u8]> {
        if self.0.format != ffi::MPV_FORMAT_STRING {
            return None;
        }
        let ptr = unsafe { self.0.u.string };
        (!ptr.is_null()).then(|| unsafe { CStr::from_ptr(ptr) }.to_bytes())
    }

    pub(super) fn as_string(self) -> Option<String> {
        if self.0.format != ffi::MPV_FORMAT_STRING {
            return None;
        }
        let ptr = unsafe { self.0.u.string };
        (!ptr.is_null()).then(|| unsafe { CStr::from_ptr(ptr) }.to_string_lossy().into_owned())
    }

    pub(super) fn as_byte_array(self) -> Option<&'a [u8]> {
        if self.0.format != ffi::MPV_FORMAT_BYTE_ARRAY {
            return None;
        }
        let ba = unsafe { self.0.u.ba.as_ref() }?;
        if ba.data.is_null() {
            return None;
        }
        Some(unsafe { std::slice::from_raw_parts(ba.data.cast::<u8>(), ba.size) })
    }

    pub(super) fn as_array(self) -> Option<impl Iterator<Item = MpvNodeRef<'a>>> {
        if self.0.format != ffi::MPV_FORMAT_NODE_ARRAY {
            return None;
        }
        let list = unsafe { self.0.u.list.as_ref() }?;
        if list.num <= 0 || list.values.is_null() {
            return None;
        }
        let slice = unsafe { std::slice::from_raw_parts(list.values, list.num as usize) };
        Some(slice.iter().map(Self))
    }

    pub(super) fn as_map(self) -> Option<impl Iterator<Item = (&'a [u8], MpvNodeRef<'a>)>> {
        if self.0.format != ffi::MPV_FORMAT_NODE_MAP {
            return None;
        }
        let list = unsafe { self.0.u.list.as_ref() }?;
        if list.num <= 0 || list.values.is_null() || list.keys.is_null() {
            return None;
        }
        let keys_slice = unsafe { std::slice::from_raw_parts(list.keys, list.num as usize) };
        let values_slice = unsafe { std::slice::from_raw_parts(list.values, list.num as usize) };
        Some(keys_slice.iter().zip(values_slice.iter()).filter_map(|(&k, v)| {
            (!k.is_null()).then(|| (unsafe { CStr::from_ptr(k) }.to_bytes(), Self(v)))
        }))
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

/// Borrow the `"name"` string field from a command-list node entry.
pub(super) fn command_name_ref<'a>(node: MpvNodeRef<'a>) -> Option<&'a [u8]> {
    for (key, val) in node.as_map()? {
        if key == b"name" {
            return val.as_str_bytes();
        }
    }
    None
}

#[cfg(test)]
pub(super) unsafe fn command_name(node: &ffi::mpv_node) -> Option<&[u8]> {
    command_name_ref(unsafe { MpvNodeRef::from_ref(node) })
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
    if status < 0 {
        return false;
    }
    let Some(entries) = result.as_ref().as_array() else {
        return false;
    };

    let mut found = 0u8;
    for entry in entries {
        found |= match command_name_ref(entry) {
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

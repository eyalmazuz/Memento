////////////////////////////////////////////////////////////////////////////////
//
// Copyright (c) 2026 Ripose
//
// This file is part of Memento.
//
// Memento is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, version 2 of the License.
//
// Memento is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with Memento.  If not, see <https://www.gnu.org/licenses/>.
//
////////////////////////////////////////////////////////////////////////////////

//! Rust-owned UTF-16 graph output. The C++ caller copies it into QString and
//! frees the buffer exactly once. Qt localization is supplied by a callback
//! only for numeric `%L` placeholders injected into color values.

use std::panic::{catch_unwind, AssertUnwindSafe};

use crate::imageutils::{self, ResizePlan};

/// This function uses no pointers; it is safe to call from C/C++.
#[no_mangle]
pub extern "C" fn memento_util_image_resize_valid(
    max_width: i32,
    max_height: i32,
) -> bool {
    catch_unwind(|| imageutils::resize_valid(max_width, max_height)).unwrap_or(false)
}

/// # Safety
/// `out` must be a valid writable pointer to one ResizePlan.
#[no_mangle]
pub unsafe extern "C" fn memento_util_image_resize_plan(
    original_width: i32,
    original_height: i32,
    max_width: i32,
    max_height: i32,
    keep_aspect_ratio: bool,
    out: *mut ResizePlan,
) -> bool {
    if out.is_null() {
        return false;
    }
    match catch_unwind(|| imageutils::resize_plan(original_width, original_height,
                                                   max_width, max_height, keep_aspect_ratio)) {
        Ok(Some(plan)) => {
            // SAFETY: the caller promises `out` is writable.
            unsafe { out.write(plan) };
            true
        }
        _ => false,
    }
}

/// Locale-aware numeric rendering from the Qt caller. The callback sets
/// `*required`; if capacity is sufficient it also writes that many UTF-16
/// units, without a terminator. On insufficient capacity it writes nothing.
pub type LocalizeImageNumber = unsafe extern "C" fn(
    value: i32,
    out: *mut u16,
    capacity: u64,
    required: *mut u64,
) -> bool;

/// QChar::digitValue for non-ASCII QString::arg placeholders when the Qt
/// compatibility switch enables them. Negative means \"not a decimal digit\".
pub type ImageDigitValue = unsafe extern "C" fn(character: u16) -> i32;

/// One Rust-owned UTF-16 graph. On failure all fields are zero/null. Pass
/// this same structure to `memento_util_image_graph_free` after consuming it.
#[repr(C)]
pub struct ImageGraphBuffer {
    pub data: *mut u16,
    pub len: u64,
    pub capacity: u64,
}

impl ImageGraphBuffer {
    fn empty() -> Self {
        Self { data: std::ptr::null_mut(), len: 0, capacity: 0 }
    }
}

/// # Safety
/// `fill` and `stroke` each point to their stated number of readable UTF-16
/// units (or are null only when length is zero); `out` is independently
/// writable. Neither optional callback may unwind into Rust. On success,
/// `out` owns the returned buffer until `memento_util_image_graph_free`.
#[no_mangle]
pub unsafe extern "C" fn memento_util_image_pitch_graph(
    mora_size: i32,
    pos: u8,
    fill: *const u16,
    fill_len: u64,
    stroke: *const u16,
    stroke_len: u64,
    unicode_digit_value: Option<ImageDigitValue>,
    localize: Option<LocalizeImageNumber>,
    out: *mut ImageGraphBuffer,
) -> bool {
    if out.is_null() {
        return false;
    }
    unsafe { out.write(ImageGraphBuffer::empty()) };
    if fill_len > isize::MAX as u64 / 2 || stroke_len > isize::MAX as u64 / 2 ||
        (fill.is_null() && fill_len != 0) ||
        (stroke.is_null() && stroke_len != 0) {
        return false;
    }
    let fill = if fill_len == 0 { &[] } else {
        unsafe { std::slice::from_raw_parts(fill, fill_len as usize) }
    };
    let stroke = if stroke_len == 0 { &[] } else {
        unsafe { std::slice::from_raw_parts(stroke, stroke_len as usize) }
    };
    let localize_number = |number: i32| -> Option<Vec<u16>> {
        let callback = localize?;
        let mut short = [0u16; 64];
        let mut length = 0u64;
        if !unsafe { callback(number, short.as_mut_ptr(), short.len() as u64, &mut length) } ||
            length > isize::MAX as u64 / 2 {
            return None;
        }
        if length <= short.len() as u64 {
            return Some(short[..length as usize].to_vec());
        }
        let mut wide = Vec::new();
        wide.try_reserve_exact(length as usize).ok()?;
        wide.resize(length as usize, 0);
        let mut actual = 0;
        if !unsafe { callback(number, wide.as_mut_ptr(), length, &mut actual) } || actual != length {
            return None;
        }
        Some(wide)
    };
    let digit_value = |character: u16| -> Option<u8> {
        let callback = unicode_digit_value?;
        let digit = unsafe { callback(character) };
        u8::try_from(digit).ok().filter(|&value| value <= 9)
    };
    match catch_unwind(AssertUnwindSafe(|| {
        imageutils::generate_pitch_graph(mora_size, pos, fill, stroke,
                                         localize_number, digit_value)
    })) {
        Ok(Some(mut graph)) => {
            let Ok(len) = u64::try_from(graph.len()) else { return false };
            let Ok(capacity) = u64::try_from(graph.capacity()) else { return false };
            let data = graph.as_mut_ptr();
            std::mem::forget(graph);
            unsafe { out.write(ImageGraphBuffer { data, len, capacity }) };
            true
        }
        _ => false,
    }
}

/// # Safety
/// `out` must be null or the same writable structure populated by one
/// successful graph call, and must not be freed twice by any other means.
#[no_mangle]
pub unsafe extern "C" fn memento_util_image_graph_free(out: *mut ImageGraphBuffer) {
    if out.is_null() {
        return;
    }
    let graph = std::mem::replace(unsafe { &mut *out }, ImageGraphBuffer::empty());
    if !graph.data.is_null() {
        drop(unsafe { Vec::from_raw_parts(graph.data, graph.len as usize, graph.capacity as usize) });
    }
}

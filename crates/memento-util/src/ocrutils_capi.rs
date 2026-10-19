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

//! C ABI for OCR region mapping (`ocrutils::map_ocr_region`). The output
//! consists only of numeric status and rect coordinates; no Rust-owned
//! pointers survive the call. `false` indicates an invalid output pointer,
//! unrepresentable geometry, or an internal panic.

use std::panic::catch_unwind;

use crate::ocrutils::OcrRegionStatus;
use crate::ocrutils::map_ocr_region;

/// Numeric status and pixel rectangle returned to the C caller. This layout
/// requires a matching C declaration before the application can use it.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct OcrRegionResult {
    pub status: u32,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

impl OcrRegionResult {
    fn empty() -> Self {
        Self { status: 0, x: 0, y: 0, width: 0, height: 0 }
    }
}

/// # Safety
/// `out` must be a valid writable pointer to one `OcrRegionResult`.
#[no_mangle]
pub unsafe extern "C" fn memento_util_ocr_region(
    selection_x: f64,
    selection_y: f64,
    selection_width: f64,
    selection_height: f64,
    player_width: f64,
    player_height: f64,
    image_width: i32,
    image_height: i32,
    out: *mut OcrRegionResult,
) -> bool {
    if out.is_null() {
        return false;
    }
    // SAFETY: the caller promises `out` is writable.
    unsafe { out.write(OcrRegionResult::empty()) };
    match catch_unwind(|| {
        map_ocr_region(
            selection_x,
            selection_y,
            selection_width,
            selection_height,
            player_width,
            player_height,
            image_width,
            image_height,
        )
        .map(|(status, (x, y, width, height))| OcrRegionResult {
            status: match status {
                OcrRegionStatus::Ok => 0,
                OcrRegionStatus::NoVideoRegion => 1,
                OcrRegionStatus::EmptyRegion => 2,
            },
            x,
            y,
            width,
            height,
        })
    }) {
        Ok(Some(result)) => {
            // SAFETY: written and validated above.
            unsafe { out.write(result) };
            true
        }
        _ => false,
    }
}

/// Helper to safely borrow a UTF-16 buffer passed from C/C++.
unsafe fn input_slice<'a>(data: *const u16, len: u64) -> Option<&'a [u16]> {
    let count = usize::try_from(len).ok()?;
    if count > isize::MAX as usize / std::mem::size_of::<u16>() {
        return None;
    }
    if count == 0 {
        return Some(&[]);
    }
    if data.is_null() {
        return None;
    }
    Some(std::slice::from_raw_parts(data, count))
}

/// C ABI for `resolve_venv_python`.
///
/// On success, `*out_path` points to `*out_len` code units allocated by Rust,
/// which the caller must free with [`memento_util_ocr_free_u16`]. Returns
/// `false` if `python_exec_is_set` is true, `venv_len == 0`, `venv_utf16` is null,
/// output pointers are null, or on error.
///
/// # Safety
/// `venv_utf16` must be null with zero length or point to `venv_len` readable
/// UTF-16 code units. `out_path` and `out_len` must be valid, writable,
/// non-overlapping pointers.
#[no_mangle]
pub unsafe extern "C" fn memento_util_resolve_venv_python(
    python_exec_is_set: bool,
    venv_utf16: *const u16,
    venv_len: u64,
    is_windows: bool,
    out_path: *mut *mut u16,
    out_len: *mut u64,
) -> bool {
    if out_path.is_null() || out_len.is_null() {
        return false;
    }
    unsafe {
        *out_path = std::ptr::null_mut();
        *out_len = 0;
    }
    if python_exec_is_set || venv_len == 0 || venv_utf16.is_null() {
        return false;
    }
    let Some(venv_slice) = (unsafe { input_slice(venv_utf16, venv_len) }) else {
        return false;
    };
    match catch_unwind(|| crate::ocrutils::resolve_venv_python(python_exec_is_set, venv_slice, is_windows)) {
        Ok(Some(resolved)) => {
            let Ok(count) = u64::try_from(resolved.len()) else {
                return false;
            };
            let boxed = resolved.into_boxed_slice();
            unsafe {
                *out_path = Box::into_raw(boxed) as *mut u16;
                *out_len = count;
            }
            true
        }
        _ => false,
    }
}

/// Release a UTF-16 buffer allocated by [`memento_util_resolve_venv_python`].
/// Null or zero length is a no-op.
///
/// # Safety
/// If `ptr` is non-null and `len` is non-zero, it must be an outstanding buffer
/// returned by [`memento_util_resolve_venv_python`] with length `len`.
#[no_mangle]
pub unsafe extern "C" fn memento_util_ocr_free_u16(ptr: *mut u16, len: u64) {
    if ptr.is_null() || len == 0 {
        return;
    }
    let _ = catch_unwind(|| {
        if let Ok(count) = usize::try_from(len) {
            drop(Box::from_raw(std::ptr::slice_from_raw_parts_mut(ptr, count)));
        }
    });
}

/// C ABI for checking if the OCR model needs to be recreated.
///
/// Matches `OcrController::model()` lines 206–209 of `src/ocr/ocrcontroller.cpp`:
/// `!has_model || cur_name != new_name || cur_gpu != new_gpu`.
///
/// # Safety
/// If non-null, `cur_name` and `new_name` must point to `cur_len` and `new_len`
/// readable UTF-16 code units respectively.
#[no_mangle]
pub unsafe extern "C" fn memento_util_ocr_should_recreate_model(
    has_model: bool,
    cur_name: *const u16,
    cur_len: u64,
    cur_gpu: bool,
    new_name: *const u16,
    new_len: u64,
    new_gpu: bool,
) -> bool {
    if !has_model {
        return true;
    }
    match catch_unwind(|| {
        let cur_slice = unsafe { input_slice(cur_name, cur_len) };
        let new_slice = unsafe { input_slice(new_name, new_len) };
        match (cur_slice, new_slice) {
            (Some(cur), Some(new)) => {
                crate::ocrutils::should_recreate_model(has_model, cur, cur_gpu, new, new_gpu)
            }
            _ => true,
        }
    }) {
        Ok(res) => res,
        Err(_) => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_venv_python_capi_validation() {
        let mut out_path = std::ptr::null_mut();
        let mut out_len = 0u64;

        unsafe {
            // Null out pointers
            assert!(!memento_util_resolve_venv_python(false, [b'a' as u16].as_ptr(), 1, false, std::ptr::null_mut(), &mut out_len));
            assert!(!memento_util_resolve_venv_python(false, [b'a' as u16].as_ptr(), 1, false, &mut out_path, std::ptr::null_mut()));

            // Already set
            assert!(!memento_util_resolve_venv_python(true, [b'a' as u16].as_ptr(), 1, false, &mut out_path, &mut out_len));
            assert!(out_path.is_null());
            assert_eq!(out_len, 0);

            // Empty length
            assert!(!memento_util_resolve_venv_python(false, [b'a' as u16].as_ptr(), 0, false, &mut out_path, &mut out_len));
            // Null venv ptr
            assert!(!memento_util_resolve_venv_python(false, std::ptr::null(), 5, false, &mut out_path, &mut out_len));

            // Valid resolution
            let venv: Vec<u16> = "/tmp/venv".encode_utf16().collect();
            assert!(memento_util_resolve_venv_python(false, venv.as_ptr(), venv.len() as u64, false, &mut out_path, &mut out_len));
            assert!(!out_path.is_null());
            assert_eq!(out_len, 20);
            let s = String::from_utf16_lossy(std::slice::from_raw_parts(out_path, out_len as usize));
            assert_eq!(s, "/tmp/venv/bin/python");

            // Free
            memento_util_ocr_free_u16(out_path, out_len);
            // Freeing null / zero
            memento_util_ocr_free_u16(std::ptr::null_mut(), 0);
        }
    }

    #[test]
    fn ocr_should_recreate_model_capi_cases() {
        let name_a: Vec<u16> = "model-a".encode_utf16().collect();
        let name_b: Vec<u16> = "model-b".encode_utf16().collect();

        unsafe {
            assert!(memento_util_ocr_should_recreate_model(false, name_a.as_ptr(), name_a.len() as u64, false, name_a.as_ptr(), name_a.len() as u64, false));
            assert!(!memento_util_ocr_should_recreate_model(true, name_a.as_ptr(), name_a.len() as u64, false, name_a.as_ptr(), name_a.len() as u64, false));
            assert!(memento_util_ocr_should_recreate_model(true, name_a.as_ptr(), name_a.len() as u64, false, name_b.as_ptr(), name_b.len() as u64, false));
            assert!(memento_util_ocr_should_recreate_model(true, name_a.as_ptr(), name_a.len() as u64, false, name_a.as_ptr(), name_a.len() as u64, true));

            // Null pointers with 0 len
            assert!(!memento_util_ocr_should_recreate_model(true, std::ptr::null(), 0, true, std::ptr::null(), 0, true));
            assert!(memento_util_ocr_should_recreate_model(true, std::ptr::null(), 0, true, name_a.as_ptr(), name_a.len() as u64, true));
        }
    }
}


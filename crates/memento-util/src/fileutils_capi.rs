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

//! Byte-oriented C ABI for `FileUtils`. Input and output storage belongs to
//! the caller; no Rust allocations need to cross the boundary.

use crate::fileutils::{base64_len, calculate_md5, to_base64};
use std::ffi::c_void;
use std::io::{self, Read};

/// Returns a byte count, zero for EOF, or negative on error. Must not throw.
pub type FileRead = unsafe extern "C" fn(*mut c_void, *mut u8, u64) -> i64;

struct CallbackReader {
    context: *mut c_void,
    callback: FileRead,
}

impl Read for CallbackReader {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        // The hasher always supplies a small fixed-size buffer.
        let count = unsafe {
            (self.callback)(self.context, buffer.as_mut_ptr(), buffer.len() as u64)
        };
        if count < 0 || count as u64 > buffer.len() as u64 {
            return Err(io::Error::other("file read failed"));
        }
        Ok(count as usize)
    }
}

/// Hash bytes read from a caller-owned open handle. On failure `digest` is
/// untouched. The callback and context must remain valid for the whole call.
///
/// # Safety
/// `digest` must point to 16 writable bytes; `read` must safely accept
/// `context`, a writable buffer, and a byte capacity until it returns EOF.
#[no_mangle]
pub unsafe extern "C" fn memento_util_file_md5(
    context: *mut c_void,
    read: Option<FileRead>,
    digest: *mut u8,
) -> bool {
    let (Some(read), false) = (read, digest.is_null()) else {
        return false;
    };
    let Ok(hash) = calculate_md5(CallbackReader {
        context,
        callback: read,
    }) else {
        return false;
    };
    unsafe { std::ptr::copy_nonoverlapping(hash.as_ptr(), digest, hash.len()) };
    true
}

/// Return the exact output size. False means arithmetic overflow or a null
/// output pointer. Empty input requires zero output bytes.
///
/// # Safety
/// `output_len` must be writable.
#[no_mangle]
pub unsafe extern "C" fn memento_util_file_base64_len(
    input_len: u64,
    output_len: *mut u64,
) -> bool {
    if output_len.is_null() {
        return false;
    }
    unsafe { *output_len = 0 };
    let Ok(input_len) = usize::try_from(input_len) else {
        return false;
    };
    let Some(len) = base64_len(input_len) else {
        return false;
    };
    unsafe { *output_len = len as u64 };
    true
}

/// Encode into caller-owned output without allocating a returned buffer.
/// Returns false if output is too small, input length overflows, or a pointer
/// is null for a nonempty buffer. Zero-length buffers may be null.
///
/// # Safety
/// Non-null pointers must denote readable/writable buffers of the specified
/// lengths, and these buffers must not overlap.
#[no_mangle]
pub unsafe extern "C" fn memento_util_file_base64(
    input: *const u8,
    input_len: u64,
    output: *mut u8,
    output_capacity: u64,
) -> bool {
    let (Ok(input_len), Ok(capacity)) = (
        usize::try_from(input_len),
        usize::try_from(output_capacity),
    ) else {
        return false;
    };
    let Some(required) = base64_len(input_len) else {
        return false;
    };
    if required > capacity || (input_len != 0 && input.is_null()) ||
        (required != 0 && output.is_null()) ||
        input_len > isize::MAX as usize || capacity > isize::MAX as usize
    {
        return false;
    }
    // from_raw_parts requires aligned non-null pointers even for empty slices.
    let source = if input_len == 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(input, input_len) }
    };
    let target = if required == 0 {
        &mut []
    } else {
        unsafe { std::slice::from_raw_parts_mut(output, required) }
    };
    to_base64(source, target) == Some(required)
}

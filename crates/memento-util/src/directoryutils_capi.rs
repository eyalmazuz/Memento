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

//! Caller-owned UTF-16 buffer ABI for the directory path normalization.

use std::panic::catch_unwind;
use std::ptr;

/// Determine the required number of UTF-16 units with `(out, capacity) =
/// (null, 0)`, then call again with a buffer of at least that length. No
/// terminator is written. On insufficient capacity, return false, write the
/// required length, and leave the output buffer untouched. On invalid input
/// or panic, return false and set the required length to zero.
///
/// # Safety
/// `required` must be writable and disjoint from both buffers. `input`
/// must be null with zero length, or reference `input_len` readable u16s.
/// `out` must be null with zero capacity, or reference `capacity` writable
/// u16s, disjoint from `input`; neither buffer may alias `required`.
#[no_mangle]
pub unsafe extern "C" fn memento_util_normalized_writable_location(
    input: *const u16,
    input_len: u64,
    out: *mut u16,
    capacity: u64,
    required: *mut u64,
) -> bool {
    if required.is_null() {
        return false;
    }
    *required = 0;
    let Ok(input_len) = usize::try_from(input_len) else {
        return false;
    };
    if input_len > isize::MAX as usize / std::mem::size_of::<u16>()
        || (input_len != 0 && input.is_null())
        || (capacity != 0 && out.is_null())
        || (capacity == 0 && !out.is_null())
    {
        return false;
    }
    let input = if input_len == 0 {
        &[][..]
    } else {
        std::slice::from_raw_parts(input, input_len)
    };
    let Some(max_capacity) = input.len().checked_add(9) else {
        return false;
    };
    if capacity >= max_capacity as u64 && !out.is_null() {
        let output = std::slice::from_raw_parts_mut(out, max_capacity);
        let Ok(Some(written)) = catch_unwind(std::panic::AssertUnwindSafe(|| {
            crate::directoryutils::normalized_writable_location_into(input, output)
        })) else {
            return false;
        };
        *required = written as u64;
        return true;
    }
    // Exact-size requests and short-buffer validation are not on the
    // production path; preserve their no-write-on-failure ABI contract.
    let Ok(result) = catch_unwind(|| crate::directoryutils::normalized_writable_location(input))
    else {
        return false;
    };
    let Ok(result_len) = u64::try_from(result.len()) else {
        return false;
    };
    *required = result_len;
    if capacity == 0 && out.is_null() {
        return true;
    }
    if capacity < result_len {
        return false;
    }
    ptr::copy_nonoverlapping(result.as_ptr(), out, result.len());
    true
}

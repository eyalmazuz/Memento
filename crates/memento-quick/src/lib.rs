//! File-open batching and QML-readiness state. Qt retains the event filter, URL
//! resolution, debounce timer and signal transport; no UTF-16 decoding occurs here.

use std::{mem, slice};

#[derive(Default)]
pub struct MementoQuickState {
    ready: bool,
    pending: Vec<Vec<u16>>,
}

impl MementoQuickState {
    fn queue(&mut self, file: &[u16]) -> bool {
        if file.is_empty() {
            return false;
        }
        self.pending.push(file.to_vec());
        true
    }

    fn take_pending(&mut self) -> Vec<Vec<u16>> {
        self.ready = true;
        mem::take(&mut self.pending)
    }

    fn send_files(&mut self) -> Vec<Vec<u16>> {
        if !self.ready {
            return Vec::new();
        }
        mem::take(&mut self.pending)
    }
}

/// An owned UTF-16 allocation transferred to C++; never interpret it as UTF-8.
#[repr(C)]
pub struct MementoQuickText {
    data: *mut u16,
    len: usize,
    capacity: usize,
}

/// All buffers (including `items`) must be returned together by `files_free`.
#[repr(C)]
pub struct MementoQuickFiles {
    items: *mut MementoQuickText,
    len: usize,
    capacity: usize,
}

impl From<Vec<Vec<u16>>> for MementoQuickFiles {
    fn from(files: Vec<Vec<u16>>) -> Self {
        if files.is_empty() {
            return Self { items: std::ptr::null_mut(), len: 0, capacity: 0 };
        }
        let mut items: Vec<MementoQuickText> = files
            .into_iter()
            .map(|mut text| {
                let item = MementoQuickText {
                    data: text.as_mut_ptr(),
                    len: text.len(),
                    capacity: text.capacity(),
                };
                mem::forget(text);
                item
            })
            .collect();
        let result = Self {
            items: items.as_mut_ptr(),
            len: items.len(),
            capacity: items.capacity(),
        };
        mem::forget(items);
        result
    }
}

#[no_mangle]
pub extern "C" fn memento_quick_new() -> *mut MementoQuickState {
    Box::into_raw(Box::new(MementoQuickState::default()))
}

/// # Safety
/// `state` is null or a pointer returned by `memento_quick_new`, freed only once.
#[no_mangle]
pub unsafe extern "C" fn memento_quick_free(state: *mut MementoQuickState) {
    if !state.is_null() {
        drop(Box::from_raw(state));
    }
}

/// # Safety
/// `state` points to a live state. `file` points to `len` readable UTF-16 code
/// units (or may be null when `len == 0`); neither buffer is concurrently mutated.
#[no_mangle]
pub unsafe extern "C" fn memento_quick_queue(
    state: *mut MementoQuickState,
    file: *const u16,
    len: usize,
) -> bool {
    if state.is_null() || file.is_null() || len == 0 {
        return false;
    }
    (*state).queue(slice::from_raw_parts(file, len))
}

/// # Safety
/// `state` points to a live state and must not be used concurrently. The
/// returned buffers belong to the caller until `memento_quick_files_free`.
#[no_mangle]
pub unsafe extern "C" fn memento_quick_take_pending(
    state: *mut MementoQuickState,
) -> MementoQuickFiles {
    if state.is_null() {
        return Vec::new().into();
    }
    (*state).take_pending().into()
}

/// # Safety
/// Same requirements as `memento_quick_take_pending`. Before readiness this
/// leaves pending files intact, including after the debounce timer expires.
#[no_mangle]
pub unsafe extern "C" fn memento_quick_send_files(
    state: *mut MementoQuickState,
) -> MementoQuickFiles {
    if state.is_null() {
        return Vec::new().into();
    }
    (*state).send_files().into()
}

/// # Safety
/// `files` was returned by `memento_quick_take_pending` or
/// `memento_quick_send_files` and has not already been freed or modified.
#[no_mangle]
pub unsafe extern "C" fn memento_quick_files_free(files: MementoQuickFiles) {
    if files.capacity == 0 {
        return;
    }
    let items = Vec::from_raw_parts(files.items, files.len, files.capacity);
    for text in items {
        if text.capacity != 0 {
            drop(Vec::from_raw_parts(text.data, text.len, text.capacity));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn batches_and_retains_undelivered_startup_files() {
        let mut state = MementoQuickState::default();
        assert!(!state.queue(&[]));
        assert!(state.queue(&[0xd800, 0, 0xdc00]));
        assert!(state.queue(&[0x65e5, 0x672c]));
        assert!(state.send_files().is_empty());
        assert!(state.send_files().is_empty());
        assert_eq!(state.take_pending(), vec![vec![0xd800, 0, 0xdc00], vec![0x65e5, 0x672c]]);
        assert!(state.take_pending().is_empty());
        assert!(state.queue(&[0xdc00]));
        assert!(state.queue(&[0xd83d, 0xde3a]));
        assert_eq!(state.send_files(), vec![vec![0xdc00], vec![0xd83d, 0xde3a]]);
        assert!(state.send_files().is_empty());
    }

    #[test]
    fn ffi_preserves_ill_formed_utf16_and_ownership() {
        unsafe {
            let state = memento_quick_new();
            let units = [0xd800, 0, 0xdc00];
            assert!(memento_quick_queue(state, units.as_ptr(), units.len()));
            assert!(!memento_quick_queue(state, std::ptr::null(), 3));
            let pending = memento_quick_take_pending(state);
            assert_eq!(pending.len, 1);
            let text = &*pending.items;
            assert_eq!(slice::from_raw_parts(text.data, text.len), units);
            memento_quick_files_free(pending);
            let empty = memento_quick_send_files(state);
            assert_eq!(empty.len, 0);
            memento_quick_files_free(empty);
            memento_quick_free(state);
        }
    }
}

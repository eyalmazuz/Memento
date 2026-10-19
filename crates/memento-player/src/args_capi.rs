//! Owned UTF-16 bridge for `MpvController::loadArgs`.

use crate::args::parse_load_args;
use std::panic::catch_unwind;
use std::ptr;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct Utf16 {
    pub data: *const u16,
    pub len: usize,
}

#[repr(C)]
pub struct Arguments {
    pub items: *const Utf16,
    pub len: usize,
}

#[repr(C)]
pub struct File {
    pub filename: Utf16,
    pub options: Arguments,
}

#[repr(C)]
pub struct Files {
    pub items: *mut File,
    pub len: usize,
    pub valid: bool,
    pub error: i32,
}

unsafe fn borrow_utf16(input: Utf16) -> Option<Vec<u16>> {
    if input.len > isize::MAX as usize / 2 ||
        (input.len != 0 && input.data.is_null()) {
        return None;
    }
    if input.len == 0 { return Some(Vec::new()) }
    Some(unsafe { std::slice::from_raw_parts(input.data, input.len) }.to_vec())
}

fn own_utf16(data: Vec<u16>) -> Utf16 {
    let owned = data.into_boxed_slice();
    let len = owned.len();
    Utf16 { data: Box::into_raw(owned).cast::<u16>(), len }
}

unsafe fn free_utf16(item: Utf16) {
    if !item.data.is_null() {
        drop(unsafe { Box::from_raw(ptr::slice_from_raw_parts_mut(item.data.cast_mut(), item.len)) });
    }
}

/// # Safety
/// `args` is a readable array of `len` borrowed UTF-16 slices. The result
/// must be passed to `memento_player_free_args` exactly once, even if invalid.
#[no_mangle]
pub unsafe extern "C" fn memento_player_parse_args(args: *const Utf16, len: usize) -> Files {
    const INVALID: Files = Files { items: ptr::null_mut(), len: 0, valid: false, error: 1 };
    if len > isize::MAX as usize / std::mem::size_of::<Utf16>() ||
        (len != 0 && args.is_null()) {
        return INVALID;
    }
    catch_unwind(|| {
        let input = if len == 0 { &[][..] } else { unsafe { std::slice::from_raw_parts(args, len) } };
        let Some(args) = input.iter().copied().map(|arg| unsafe { borrow_utf16(arg) })
            .collect::<Option<Vec<_>>>() else { return INVALID };
        // Original loadArgs returns false for no arguments before it parses.
        if args.len() <= 1 { return INVALID }
        let loads = match parse_load_args(&args) {
            Ok(loads) => loads,
            Err(crate::args::ParseArgsError::InvalidBrackets) => return INVALID,
            Err(crate::args::ParseArgsError::MaxDepthExceeded) =>
                return Files { error: 2, ..INVALID },
        };
        let files: Box<[File]> = loads.into_iter().map(|load| {
            let options: Box<[Utf16]> = load.options.into_iter().map(own_utf16).collect();
            let options_len = options.len();
            let options = Box::into_raw(options).cast::<Utf16>();
            File {
                filename: own_utf16(load.filename),
                options: Arguments { items: options, len: options_len },
            }
        }).collect();
        let len = files.len();
        Files { items: Box::into_raw(files).cast::<File>(), len, valid: true, error: 0 }
    }).unwrap_or(INVALID)
}

/// # Safety
/// `files` must be a result returned by `memento_player_parse_args` and not
/// previously released. Nested UTF-16 buffers are freed with the outer array.
#[no_mangle]
pub unsafe extern "C" fn memento_player_free_args(files: Files) {
    if files.items.is_null() { return }
    let files = unsafe { Box::from_raw(ptr::slice_from_raw_parts_mut(files.items, files.len)) };
    for file in files.iter() {
        unsafe { free_utf16(file.filename) };
        if !file.options.items.is_null() {
            let options = unsafe { Box::from_raw(ptr::slice_from_raw_parts_mut(
                file.options.items.cast_mut(), file.options.len)) };
            for option in options.iter() { unsafe { free_utf16(*option) } }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_order_and_ownership_survive_the_ffi() {
        let argv: Vec<Vec<u16>> = ["app", "first", "--speed=2", "--{", "--sid=1", "inner", "--}", "last"]
            .iter().map(|s| s.encode_utf16().collect()).collect();
        let borrowed: Vec<_> = argv.iter().map(|value| Utf16 { data: value.as_ptr(), len: value.len() }).collect();
        let files = unsafe { memento_player_parse_args(borrowed.as_ptr(), borrowed.len()) };
        assert!(files.valid);
        assert_eq!(files.len, 3);
        let contents = unsafe { std::slice::from_raw_parts(files.items, files.len) };
        let filenames: Vec<_> = contents.iter().map(|file| String::from_utf16(
            unsafe { std::slice::from_raw_parts(file.filename.data, file.filename.len) }).unwrap()).collect();
        assert_eq!(filenames, ["first", "inner", "last"]);
        assert_eq!(contents[1].options.len, 2);
        assert_eq!(contents[0].options.len, 1);
        unsafe { memento_player_free_args(files) };
    }

    #[test]
    fn malformed_input_is_invalid_without_dereference() {
        let result = unsafe { memento_player_parse_args(ptr::null(), 2) };
        assert!(!result.valid);
        unsafe { memento_player_free_args(result) };
    }
}

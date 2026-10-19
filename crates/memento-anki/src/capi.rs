//! UTF-16 C ABI consumed by the Qt marker tokenizer shell. All returned
//! buffers are Rust-owned; nested text and argument arrays are released by
//! one paired free call, including when C++ token reconstruction throws.
//! Mirrors the ownership/free pattern of `memento-subtitle`'s capi.

use std::panic::catch_unwind;
use std::ptr;

use crate::tokenizer::{tokenize, Token};

#[repr(C)]
pub struct MementoAnkiMarkerArgC
{
    pub key: *mut u16,
    pub key_len: u64,
    pub value: *mut u16,
    pub value_len: u64,
}

#[repr(C)]
pub struct MementoAnkiMarkerC
{
    pub name: *mut u16,
    pub name_len: u64,
    pub args: *mut MementoAnkiMarkerArgC,
    pub args_len: u64,
}

#[repr(C)]
pub struct MementoAnkiTokenC
{
    pub raw: *mut u16,
    pub raw_len: u64,
    pub markers: *mut MementoAnkiMarkerC,
    pub markers_len: u64,
}

#[repr(C)]
pub struct MementoAnkiTokensC
{
    pub tokens: *mut MementoAnkiTokenC,
    pub len: u64,
}

impl MementoAnkiTokensC
{
    fn empty() -> Self
    {
        Self { tokens: ptr::null_mut(), len: 0 }
    }
}

/// Holds every boxed allocation handed to C: unit buffers (`raw`, marker
/// names, arg keys and values), per-token marker arrays and per-marker arg
/// arrays. All get `into_boxed_slice` (length == capacity) so the paired
/// free can reconstruct exact allocations from (pointer, length) pairs.
#[derive(Default)]
struct Owned
{
    u16s: Vec<Box<[u16]>>,
    args_arrays: Vec<Box<[MementoAnkiMarkerArgC]>>,
    marker_arrays: Vec<Box<[MementoAnkiMarkerC]>>,
}

impl Owned
{
    fn push_units(&mut self, units: &[u16]) -> (*mut u16, u64)
    {
        if units.is_empty()
        {
            return (ptr::null_mut(), 0);
        }
        let boxed = units.to_vec().into_boxed_slice();
        let leaked = (boxed.as_ptr() as *mut u16, boxed.len() as u64);
        self.u16s.push(boxed);
        leaked
    }

    fn push_args(&mut self, args: Vec<MementoAnkiMarkerArgC>) -> (*mut MementoAnkiMarkerArgC, u64)
    {
        if args.is_empty()
        {
            return (ptr::null_mut(), 0);
        }
        let boxed = args.into_boxed_slice();
        let leaked = (boxed.as_ptr() as *mut _, boxed.len() as u64);
        self.args_arrays.push(boxed);
        leaked
    }

    fn push_markers(&mut self, markers: Vec<MementoAnkiMarkerC>) -> (*mut MementoAnkiMarkerC, u64)
    {
        if markers.is_empty()
        {
            return (ptr::null_mut(), 0);
        }
        let boxed = markers.into_boxed_slice();
        let leaked = (boxed.as_ptr() as *mut _, boxed.len() as u64);
        self.marker_arrays.push(boxed);
        leaked
    }
}

fn push_token(token: Token, owned: &mut Owned) -> MementoAnkiTokenC
{
    let (raw, raw_len) = owned.push_units(&token.raw);
    let mut markers = Vec::new();
    for marker in token.markers
    {
        let (name, name_len) = owned.push_units(&marker.name);
        let mut args = Vec::new();
        for (key, value) in marker.args
        {
            let (key, key_len) = owned.push_units(&key);
            let (value, value_len) = owned.push_units(&value);
            args.push(MementoAnkiMarkerArgC { key, key_len, value, value_len });
        }
        let (args, args_len) = owned.push_args(args);
        markers.push(MementoAnkiMarkerC { name, name_len, args, args_len });
    }
    let (markers, markers_len) = owned.push_markers(markers);
    MementoAnkiTokenC { raw, raw_len, markers, markers_len }
}

/// # Safety
/// `text` must be null with zero length or a readable `len`-unit UTF-16
/// buffer. `out` must be writable and not alias the text. On success the
/// returned collection belongs to Rust until `memento_anki_free_tokens`.
#[no_mangle]
pub unsafe extern "C" fn memento_anki_tokenize(
    text: *const u16,
    len: u64,
    out: *mut MementoAnkiTokensC,
) -> bool
{
    if out.is_null()
    {
        return false;
    }
    unsafe { out.write(MementoAnkiTokensC::empty()) };
    let Ok(length) = usize::try_from(len) else { return false };
    if length > isize::MAX as usize / std::mem::size_of::<u16>() ||
        (length != 0 && text.is_null())
    {
        return false;
    }
    let source = if length == 0 { &[] } else {
        unsafe { std::slice::from_raw_parts(text, length) }
    };
    // Both the core and the buffer flattening stay inside the catch so no
    // panic can cross the ABI boundary; `out` then remains in the empty
    // state. An empty result is a success with nothing to free.
    let Ok(built) = catch_unwind(|| {
        let tokens = tokenize(source);
        if tokens.is_empty()
        {
            return None;
        }
        let mut owned = Owned::default();
        let mut tokens_c = Vec::new();
        for token in tokens
        {
            tokens_c.push(push_token(token, &mut owned));
        }
        let count = tokens_c.len();
        let result = MementoAnkiTokensC {
            tokens: Box::into_raw(tokens_c.into_boxed_slice()) as *mut MementoAnkiTokenC,
            len: count as u64,
        };
        Some((result, owned))
    }) else
    {
        return false;
    };
    let Some((result, owned)) = built else { return true };
    // Hand the boxed allocations to C; the paired free reconstructs them.
    std::mem::forget(owned);
    unsafe { out.write(result) };
    true
}

/// # Safety
/// `out` must be null or the original writable collection returned by one
/// successful `memento_anki_tokenize`. Other pointers violate the contract.
#[no_mangle]
pub unsafe extern "C" fn memento_anki_free_tokens(out: *mut MementoAnkiTokensC)
{
    if out.is_null() { return; }
    let owned = std::mem::replace(unsafe { &mut *out }, MementoAnkiTokensC::empty());
    if owned.tokens.is_null() { return; }
    let tokens = unsafe {
        Box::from_raw(std::ptr::slice_from_raw_parts_mut(
            owned.tokens, owned.len as usize,
        ))
    };
    for token in tokens
    {
        if !token.raw.is_null()
        {
            drop(unsafe {
                Box::from_raw(std::ptr::slice_from_raw_parts_mut(
                    token.raw, token.raw_len as usize,
                ))
            });
        }
        if token.markers.is_null() { continue; }
        let markers = unsafe {
            Box::from_raw(std::ptr::slice_from_raw_parts_mut(
                token.markers, token.markers_len as usize,
            ))
        };
        for marker in markers
        {
            if !marker.name.is_null()
            {
                drop(unsafe {
                    Box::from_raw(std::ptr::slice_from_raw_parts_mut(
                        marker.name, marker.name_len as usize,
                    ))
                });
            }
            if marker.args.is_null() { continue; }
            let args = unsafe {
                Box::from_raw(std::ptr::slice_from_raw_parts_mut(
                    marker.args, marker.args_len as usize,
                ))
            };
            for arg in args
            {
                if !arg.key.is_null()
                {
                    drop(unsafe {
                        Box::from_raw(std::ptr::slice_from_raw_parts_mut(
                            arg.key, arg.key_len as usize,
                        ))
                    });
                }
                if !arg.value.is_null()
                {
                    drop(unsafe {
                        Box::from_raw(std::ptr::slice_from_raw_parts_mut(
                            arg.value, arg.value_len as usize,
                        ))
                    });
                }
            }
        }
    }
}

#[cfg(test)]
mod tests
{
    use super::*;

    #[test]
    fn nested_utf16_payloads_survive_ffi_and_free_resets_ownership()
    {
        let text: Vec<u16> = "{audio:source=custom,db=-10}{読み:k=😀}"
            .encode_utf16()
            .collect();
        let mut output = MementoAnkiTokensC::empty();
        assert!(unsafe { memento_anki_tokenize(text.as_ptr(), text.len() as u64, &mut output) });
        assert_eq!(output.len, 2);
        let tokens = unsafe { std::slice::from_raw_parts(output.tokens, output.len as usize) };
        assert_eq!(tokens[0].markers_len, 1);
        let first = unsafe { &*tokens[0].markers };
        assert_eq!(first.args_len, 2);
        let args = unsafe { std::slice::from_raw_parts(first.args, first.args_len as usize) };
        assert_eq!(unsafe { std::slice::from_raw_parts(args[1].value, args[1].value_len as usize) },
                   "-10".encode_utf16().collect::<Vec<_>>());
        let second = unsafe { &*tokens[1].markers };
        assert_eq!(unsafe { std::slice::from_raw_parts(second.name, second.name_len as usize) },
                   "読み".encode_utf16().collect::<Vec<_>>());
        let emoji_arg = unsafe { &*second.args };
        assert_eq!(unsafe { std::slice::from_raw_parts(emoji_arg.value, emoji_arg.value_len as usize) },
                   "😀".encode_utf16().collect::<Vec<_>>());
        unsafe { memento_anki_free_tokens(&mut output) };
        assert!(output.tokens.is_null());
        assert_eq!(output.len, 0);
        unsafe { memento_anki_free_tokens(&mut output) };
    }
}

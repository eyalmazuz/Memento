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

//! UTF-16 C ABI for `FuriganaUtils::simplifySurface` and
//! `FuriganaParser::katakanaToHiragana`.
//!
//! Offsets and lengths count UTF-16 code units, like `QString`. A successful
//! nonempty result is owned by this bridge and must be released with
//! [`memento_util_free`] (for FuriganaSpan arrays) or [`memento_util_free_u16`]
//! (for u16 buffers). Isolated surrogates are preserved; only caller
//! protocol errors or an internal failure return `false` with null/zero
//! outputs. No exception may cross the C boundary.

use std::alloc::{alloc, dealloc, handle_alloc_error, Layout};
use std::ffi::{c_char, c_void};
use std::panic::catch_unwind;
use std::ptr;

use crate::FuriganaSpan;

/// View a C UTF-16 buffer without copying it. A null pointer denotes an
/// empty input only when the length is zero.
///
/// # Safety
/// Non-null pointers must reference `len` readable UTF-16 code units.
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

/// C ABI for the complete furigana simplification algorithm. The returned
/// spans contain offsets into the original UTF-16 input buffers; anchor
/// readings use `0..0`. Empty results are reported with null and length zero.
///
/// # Safety
/// Each input pointer must be null with zero length or point to the given
/// number of readable `u16`s. `out` and `out_len` must be valid, writable,
/// non-overlapping locations and must not overlap the input buffers.
#[no_mangle]
pub unsafe extern "C" fn memento_util_simplify_surface(
    surface: *const u16,
    surface_len: u64,
    reading: *const u16,
    reading_len: u64,
    out: *mut *mut FuriganaSpan,
    out_len: *mut u64,
) -> bool {
    if out.is_null() || out_len.is_null() {
        return false;
    }
    *out = ptr::null_mut();
    *out_len = 0;

    let (Some(surface), Some(reading)) =
        (input_slice(surface, surface_len), input_slice(reading, reading_len))
    else {
        return false;
    };
    let Ok(spans) = catch_unwind(|| crate::furiganautils::simplify_surface(surface, reading))
    else {
        return false;
    };
    if spans.is_empty() {
        return true;
    }
    let Ok(count) = u64::try_from(spans.len()) else {
        return false;
    };
    // Free takes just the pointer, so keep the allocation length in the
    // preceding slot instead of maintaining a global allocation registry.
    let Some(slots) = spans.len().checked_add(1) else {
        return false;
    };
    let Ok(layout) = Layout::array::<FuriganaSpan>(slots) else {
        return false;
    };
    let base = alloc(layout) as *mut FuriganaSpan;
    if base.is_null() {
        handle_alloc_error(layout);
    }
    (base as *mut usize).write(spans.len());
    let data = base.add(1);
    ptr::copy_nonoverlapping(spans.as_ptr(), data, spans.len());
    *out = data;
    *out_len = count;
    true
}

/// Release an array returned by [`memento_util_simplify_surface`]. Null is a
/// no-op; no other pointer is valid here (including interior array pointers).
///
/// # Safety
/// `ptr` must be null or an outstanding allocation returned by this bridge.
#[no_mangle]
pub unsafe extern "C" fn memento_util_free(ptr: *mut c_void) {
    if ptr.is_null() {
        return;
    }
    let base = (ptr as *mut FuriganaSpan).sub(1);
    let count = (base as *const usize).read();
    // This layout was checked when allocated; a caller passing any other
    // pointer violates the contract above.
    let layout = Layout::array::<FuriganaSpan>(count + 1).unwrap();
    dealloc(base.cast(), layout);
}

/// C ABI for `FuriganaParser::katakanaToHiragana`.
///
/// On success, `*out` points to `*out_len` code units allocated by Rust,
/// which the caller must free with [`memento_util_free_u16`]. An empty result
/// yields null and length 0, which is valid and freeable. Returns `false` on
/// null output pointers, unrepresentable length, or internal error.
///
/// # Safety
/// `text` must be null with zero length or point to `text_len` readable UTF-16
/// code units. `out` and `out_len` must be valid, writable, non-overlapping
/// locations.
#[no_mangle]
pub unsafe extern "C" fn memento_util_katakana_to_hiragana(
    text: *const u16,
    text_len: u64,
    out: *mut *mut u16,
    out_len: *mut u64,
) -> bool {
    if out.is_null() || out_len.is_null() {
        return false;
    }
    *out = ptr::null_mut();
    *out_len = 0;

    let Some(text_slice) = input_slice(text, text_len) else {
        return false;
    };
    let Ok(converted) = catch_unwind(|| crate::furiganaparser::katakana_to_hiragana(text_slice))
    else {
        return false;
    };
    if converted.is_empty() {
        return true;
    }
    let Ok(count) = u64::try_from(converted.len()) else {
        return false;
    };
    let boxed = converted.into_boxed_slice();
    *out = Box::into_raw(boxed) as *mut u16;
    *out_len = count;
    true
}

/// Release a UTF-16 buffer allocated by [`memento_util_katakana_to_hiragana`].
/// Null or zero length is a no-op.
///
/// # Safety
/// If `ptr` is non-null and `len` is non-zero, it must be an outstanding buffer
/// returned by [`memento_util_katakana_to_hiragana`] with length `len`.
#[no_mangle]
pub unsafe extern "C" fn memento_util_free_u16(ptr: *mut u16, len: u64) {
    if ptr.is_null() || len == 0 {
        return;
    }
    let _ = catch_unwind(|| {
        if let Ok(count) = usize::try_from(len) {
            drop(Box::from_raw(std::ptr::slice_from_raw_parts_mut(ptr, count)));
        }
    });
}

/// A borrowed view of a MeCab lattice node passed from C/C++.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct FuriganaNodeView {
    pub whitespace_utf8_ptr: *const u8,
    pub whitespace_utf8_len: u64,
    pub surface_utf8_ptr: *const u8,
    pub surface_utf8_len: u64,
    pub feature_utf8_ptr: *const u8,
    pub feature_utf8_len: u64,
}

/// An individual parsed furigana token exposed across FFI.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct FuriganaToken {
    pub surface_ptr: *const u16,
    pub surface_len: u64,
    pub reading_ptr: *const u16,
    pub reading_len: u64,
}

/// A collection of parsed furigana tokens owned by Rust.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct FuriganaTokenList {
    pub tokens: *const FuriganaToken,
    pub count: u64,
    pub storage: *mut c_void,
}

struct FuriganaTokenStorage {
    _tokens: Vec<FuriganaToken>,
    _text_buffer: Vec<u16>,
}

/// C ABI for parsing a batch of MeCab nodes into furigana tokens.
///
/// On success, `*out` holds `count` tokens allocated by Rust and must be
/// released with [`memento_util_free_furigana_tokens`].
///
/// # Safety
/// `nodes` must point to `node_count` valid `FuriganaNodeView`s or be null if
/// `node_count == 0`. `out` must be a valid, writable pointer.
#[no_mangle]
pub unsafe extern "C" fn memento_util_parse_furigana_nodes(
    nodes: *const FuriganaNodeView,
    node_count: u64,
    out: *mut FuriganaTokenList,
) -> bool {
    if out.is_null() {
        return false;
    }
    *out = FuriganaTokenList {
        tokens: ptr::null(),
        count: 0,
        storage: ptr::null_mut(),
    };

    let count = match usize::try_from(node_count) {
        Ok(c) => c,
        Err(_) => return false,
    };
    if count > isize::MAX as usize / std::mem::size_of::<FuriganaNodeView>() {
        return false;
    }
    let node_views = if count == 0 {
        &[][..]
    } else {
        if nodes.is_null() {
            return false;
        }
        std::slice::from_raw_parts(nodes, count)
    };

    let Ok(parsed_tokens) = catch_unwind(|| {
        let mut node_slices = Vec::with_capacity(node_views.len());
        for view in node_views {
            let whitespace = if view.whitespace_utf8_len == 0 {
                &[][..]
            } else {
                if view.whitespace_utf8_ptr.is_null() {
                    return Err(());
                }
                let len = usize::try_from(view.whitespace_utf8_len).map_err(|_| ())?;
                std::slice::from_raw_parts(view.whitespace_utf8_ptr, len)
            };

            let surface = if view.surface_utf8_len == 0 {
                &[][..]
            } else {
                if view.surface_utf8_ptr.is_null() {
                    return Err(());
                }
                let len = usize::try_from(view.surface_utf8_len).map_err(|_| ())?;
                std::slice::from_raw_parts(view.surface_utf8_ptr, len)
            };

            let feature = if view.feature_utf8_len == 0 {
                &[][..]
            } else {
                if view.feature_utf8_ptr.is_null() {
                    return Err(());
                }
                let len = usize::try_from(view.feature_utf8_len).map_err(|_| ())?;
                std::slice::from_raw_parts(view.feature_utf8_ptr, len)
            };

            node_slices.push(crate::furiganaparser::FuriganaNodeSlice {
                whitespace,
                surface,
                feature,
            });
        }

        Ok::<_, ()>(crate::furiganaparser::parse_nodes(&node_slices))
    }) else {
        return false;
    };

    let Ok(tokens) = parsed_tokens else {
        return false;
    };

    let Ok(success) = catch_unwind(|| {
        if tokens.is_empty() {
            return true;
        }

        let mut text_buffer = Vec::new();
        struct SpanInfo {
            surface_start: usize,
            surface_len: usize,
            reading_start: usize,
            reading_len: usize,
            has_reading: bool,
        }
        let mut spans = Vec::with_capacity(tokens.len());
        for tok in &tokens {
            let surface_start = text_buffer.len();
            text_buffer.extend_from_slice(&tok.surface);
            let surface_len = tok.surface.len();

            let (reading_start, reading_len, has_reading) = if let Some(r) = &tok.reading {
                let start = text_buffer.len();
                text_buffer.extend_from_slice(r);
                (start, r.len(), true)
            } else {
                (0, 0, false)
            };

            spans.push(SpanInfo {
                surface_start,
                surface_len,
                reading_start,
                reading_len,
                has_reading,
            });
        }

        let abi_tokens: Vec<FuriganaToken> = spans
            .iter()
            .map(|s| {
                let surface_ptr = text_buffer[s.surface_start..].as_ptr();
                let (reading_ptr, reading_len) = if s.has_reading {
                    (text_buffer[s.reading_start..].as_ptr(), s.reading_len as u64)
                } else {
                    (ptr::null(), 0)
                };
                FuriganaToken {
                    surface_ptr,
                    surface_len: s.surface_len as u64,
                    reading_ptr,
                    reading_len,
                }
            })
            .collect();

        let Ok(count) = u64::try_from(abi_tokens.len()) else {
            return false;
        };
        let tokens_ptr = abi_tokens.as_ptr();
        let storage = Box::new(FuriganaTokenStorage {
            _tokens: abi_tokens,
            _text_buffer: text_buffer,
        });

        *out = FuriganaTokenList {
            tokens: tokens_ptr,
            count,
            storage: Box::into_raw(storage) as *mut c_void,
        };
        true
    }) else {
        return false;
    };
    success

}

/// Release a token list allocated by [`memento_util_parse_furigana_nodes`].
///
/// # Safety
/// `out` must be null or point to a `FuriganaTokenList` previously initialized
/// by `memento_util_parse_furigana_nodes`.
#[no_mangle]
pub unsafe extern "C" fn memento_util_free_furigana_tokens(out: *mut FuriganaTokenList) {
    if out.is_null() {
        return;
    }
    let list = &mut *out;
    if !list.storage.is_null() {
        drop(Box::from_raw(list.storage as *mut FuriganaTokenStorage));
    }
    list.tokens = ptr::null();
    list.count = 0;
    list.storage = ptr::null_mut();
}

/// C ABI for `parseImageRequest` ID extraction.
///
/// Splits `id` into `source_kind`, `out_encoded_path`, and `out_color_spec`.
/// On success, `*out_encoded_path` and `*out_color_spec` point to UTF-16
/// buffers that must be released with [`memento_util_free_u16`].
///
/// # Safety
/// `id` must point to `id_len` valid `u16` code units or be null if `id_len == 0`.
/// Output pointers must be valid, writable locations.
#[no_mangle]
pub unsafe extern "C" fn memento_util_parse_image_request_id(
    id: *const u16,
    id_len: u64,
    out_source_kind: *mut u32,
    out_encoded_path: *mut *mut u16,
    out_encoded_path_len: *mut u64,
    out_color_spec: *mut *mut u16,
    out_color_spec_len: *mut u64,
) -> bool {
    if out_source_kind.is_null()
        || out_encoded_path.is_null()
        || out_encoded_path_len.is_null()
        || out_color_spec.is_null()
        || out_color_spec_len.is_null()
    {
        return false;
    }
    *out_source_kind = 0;
    *out_encoded_path = ptr::null_mut();
    *out_encoded_path_len = 0;
    *out_color_spec = ptr::null_mut();
    *out_color_spec_len = 0;

    let Some(id_slice) = input_slice(id, id_len) else {
        return false;
    };

    let Ok(Some((kind, encoded_path, color_spec))) = catch_unwind(|| {
        crate::quickutils::parse_image_request_id(id_slice)
    }) else {
        return false;
    };

    *out_source_kind = kind as u32;

    if encoded_path.is_empty() {
        *out_encoded_path = ptr::null_mut();
        *out_encoded_path_len = 0;
    } else {
        let Ok(len) = u64::try_from(encoded_path.len()) else {
            return false;
        };
        *out_encoded_path_len = len;
        *out_encoded_path = Box::into_raw(encoded_path.into_boxed_slice()) as *mut u16;
    }

    if color_spec.is_empty() {
        *out_color_spec = ptr::null_mut();
        *out_color_spec_len = 0;
    } else {
        let Ok(len) = u64::try_from(color_spec.len()) else {
            if !(*out_encoded_path).is_null() {
                memento_util_free_u16(*out_encoded_path, *out_encoded_path_len);
                *out_encoded_path = ptr::null_mut();
                *out_encoded_path_len = 0;
            }
            return false;
        };
        *out_color_spec_len = len;
        *out_color_spec = Box::into_raw(color_spec.into_boxed_slice()) as *mut u16;
    }

    true
}

/// C ABI for `isValidSource` and `isSvgPath`.
///
/// Sets `*out_is_svg` (if non-null) and returns whether `source_path` is valid.
///
/// # Safety
/// `source_path` must point to `source_path_len` valid `u16` code units or be null
/// if `source_path_len == 0`. `out_is_svg` may be null or a writable `bool` pointer.
#[no_mangle]
pub unsafe extern "C" fn memento_util_validate_image_source(
    source_kind: u32,
    source_path: *const u16,
    source_path_len: u64,
    is_file_absolute: bool,
    out_is_svg: *mut bool,
) -> bool {
    let is_svg_ref = if !out_is_svg.is_null() {
        *out_is_svg = false;
        Some(&mut *out_is_svg)
    } else {
        None
    };

    let Some(path_slice) = input_slice(source_path, source_path_len) else {
        return false;
    };

    let Ok((is_valid, is_svg)) = catch_unwind(|| {
        crate::quickutils::validate_image_source(source_kind, path_slice, is_file_absolute)
    }) else {
        return false;
    };

    if let Some(r) = is_svg_ref {
        *r = is_svg;
    }

    is_valid
}

/// C ABI for `KeyTracker::modifierHeld`.
#[no_mangle]
pub extern "C" fn memento_util_modifier_held(
    setting_modifier: i32,
    qt_modifiers: u32,
) -> bool {
    catch_unwind(|| crate::quickutils::modifier_held(setting_modifier, qt_modifiers))
        .unwrap_or(false)
}

/// Formats MeCab tagger option bytes based on target environment flags and paths.
///
/// On success, `*out` points to a byte buffer allocated by Rust with length
/// `*out_len`, released by [`memento_util_free_string`]. Windows paths from
/// `GetShortPathNameA` are local-codepage bytes, not necessarily UTF-8.
/// If the formatted string is empty, `*out` is null and `*out_len` is 0.
///
/// # Safety
/// Nonempty `path_a` and `path_b` must point to their respective byte counts.
/// `out` and `out_len` must be valid writable pointers.
#[no_mangle]
pub unsafe extern "C" fn memento_util_format_mecab_arg(
    is_windows: bool,
    is_bundle: bool,
    path_a: *const c_char,
    path_a_len: u64,
    path_b: *const c_char,
    path_b_len: u64,
    out: *mut *mut c_char,
    out_len: *mut u64,
) -> bool {
    if out.is_null() || out_len.is_null() {
        return false;
    }
    *out = ptr::null_mut();
    *out_len = 0;

    let input = |ptr: *const c_char, len: u64| -> Option<&[u8]> {
        let count = usize::try_from(len).ok()?;
        if count > isize::MAX as usize {
            return None;
        }
        if count == 0 {
            return Some(&[]);
        }
        if ptr.is_null() {
            return None;
        }
        Some(unsafe { std::slice::from_raw_parts(ptr as *const u8, count) })
    };
    let Some(str_a) = input(path_a, path_a_len) else {
        return false;
    };
    let Some(str_b) = input(path_b, path_b_len) else {
        return false;
    };

    let Ok(formatted) = catch_unwind(|| {
        crate::quickutils::format_mecab_tagger_arg(is_windows, is_bundle, str_a, str_b)
    }) else {
        return false;
    };

    if formatted.is_empty() {
        return true;
    }

    let Ok(count) = u64::try_from(formatted.len()) else {
        return false;
    };
    let boxed = formatted.into_boxed_slice();
    *out = Box::into_raw(boxed) as *mut c_char;
    *out_len = count;
    true
}

/// Release an option byte buffer allocated by [`memento_util_format_mecab_arg`]. Null is a no-op.
///
/// # Safety
/// If `ptr` is non-null and `len` is non-zero, it must be an outstanding buffer
/// returned by [`memento_util_format_mecab_arg`] with length `len`.
#[no_mangle]
pub unsafe extern "C" fn memento_util_free_string(ptr: *mut c_char, len: u64) {
    if ptr.is_null() || len == 0 {
        return;
    }
    let _ = catch_unwind(|| {
        if let Ok(count) = usize::try_from(len) {
            drop(Box::from_raw(std::ptr::slice_from_raw_parts_mut(ptr as *mut u8, count)));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn katakana_capi_roundtrip() {
        let text: Vec<u16> = "カタカナ".encode_utf16().collect();
        let mut out: *mut u16 = ptr::null_mut();
        let mut out_len: u64 = 0;
        unsafe {
            assert!(memento_util_katakana_to_hiragana(
                text.as_ptr(),
                text.len() as u64,
                &mut out,
                &mut out_len,
            ));
            assert_eq!(out_len, 4);
            assert!(!out.is_null());
            let slice = std::slice::from_raw_parts(out, out_len as usize);
            let result = String::from_utf16(slice).unwrap();
            assert_eq!(result, "かたかな");
            memento_util_free_u16(out, out_len);
        }
    }

    #[test]
    fn katakana_capi_empty() {
        let mut out: *mut u16 = ptr::null_mut();
        let mut out_len: u64 = 99;
        unsafe {
            assert!(memento_util_katakana_to_hiragana(
                ptr::null(),
                0,
                &mut out,
                &mut out_len,
            ));
            assert_eq!(out_len, 0);
            assert!(out.is_null());
            memento_util_free_u16(out, out_len);
        }
    }

    #[test]
    fn parse_furigana_nodes_capi_roundtrip() {
        let ws = b" ";
        let surf = "食べる".as_bytes();
        let feat = "動詞,自立,*,*,一段,基本形,食べる,タベル,タベル".as_bytes();
        let node = FuriganaNodeView {
            whitespace_utf8_ptr: ws.as_ptr(),
            whitespace_utf8_len: ws.len() as u64,
            surface_utf8_ptr: surf.as_ptr(),
            surface_utf8_len: surf.len() as u64,
            feature_utf8_ptr: feat.as_ptr(),
            feature_utf8_len: feat.len() as u64,
        };

        let mut list = FuriganaTokenList {
            tokens: ptr::null(),
            count: 0,
            storage: ptr::null_mut(),
        };

        unsafe {
            assert!(memento_util_parse_furigana_nodes(&node, 1, &mut list));
            assert_eq!(list.count, 3);
            let tokens = std::slice::from_raw_parts(list.tokens, list.count as usize);

            // Token 0: whitespace
            let t0_surf = std::slice::from_raw_parts(tokens[0].surface_ptr, tokens[0].surface_len as usize);
            assert_eq!(String::from_utf16_lossy(t0_surf), " ");
            assert!(tokens[0].reading_ptr.is_null());

            // Token 1: 食 (た)
            let t1_surf = std::slice::from_raw_parts(tokens[1].surface_ptr, tokens[1].surface_len as usize);
            assert_eq!(String::from_utf16_lossy(t1_surf), "食");
            assert!(!tokens[1].reading_ptr.is_null());
            let t1_read = std::slice::from_raw_parts(tokens[1].reading_ptr, tokens[1].reading_len as usize);
            assert_eq!(String::from_utf16_lossy(t1_read), "た");

            // Token 2: べる
            let t2_surf = std::slice::from_raw_parts(tokens[2].surface_ptr, tokens[2].surface_len as usize);
            assert_eq!(String::from_utf16_lossy(t2_surf), "べる");
            assert!(tokens[2].reading_ptr.is_null());

            memento_util_free_furigana_tokens(&mut list);
            assert_eq!(list.count, 0);
            assert!(list.tokens.is_null());
            assert!(list.storage.is_null());
        }
    }

    #[test]
    fn parse_image_request_id_capi() {
        let id_u16: Vec<u16> = "resource/%3A%2Ficons%2Ftest.svg/ff00ff00".encode_utf16().collect();
        let mut source_kind = 0u32;
        let mut encoded_path = ptr::null_mut();
        let mut encoded_path_len = 0u64;
        let mut color_spec = ptr::null_mut();
        let mut color_spec_len = 0u64;

        unsafe {
            assert!(memento_util_parse_image_request_id(
                id_u16.as_ptr(),
                id_u16.len() as u64,
                &mut source_kind,
                &mut encoded_path,
                &mut encoded_path_len,
                &mut color_spec,
                &mut color_spec_len,
            ));
            assert_eq!(source_kind, 1);
            let path_slice = std::slice::from_raw_parts(encoded_path, encoded_path_len as usize);
            assert_eq!(String::from_utf16_lossy(path_slice), "%3A%2Ficons%2Ftest.svg");
            let color_slice = std::slice::from_raw_parts(color_spec, color_spec_len as usize);
            assert_eq!(String::from_utf16_lossy(color_slice), "#ff00ff00");

            memento_util_free_u16(encoded_path, encoded_path_len);
            memento_util_free_u16(color_spec, color_spec_len);
        }
    }

    #[test]
    fn validate_image_source_capi() {
        let path_u16: Vec<u16> = ":/icons/test.svg".encode_utf16().collect();
        let mut is_svg = false;
        unsafe {
            let valid = memento_util_validate_image_source(
                1,
                path_u16.as_ptr(),
                path_u16.len() as u64,
                false,
                &mut is_svg,
            );
            assert!(valid);
            assert!(is_svg);
        }
    }

    #[test]
    fn modifier_held_capi() {
        assert!(memento_util_modifier_held(0, 0x08000000));
        assert!(!memento_util_modifier_held(0, 0x04000000));
        assert!(memento_util_modifier_held(1, 0x04000000));
        assert!(memento_util_modifier_held(2, 0x02000000));
        assert!(memento_util_modifier_held(3, 0x10000000));
        assert!(!memento_util_modifier_held(99, 0xFFFFFFFF));
    }

    #[test]
    fn format_mecab_arg_capi() {
        let mut out = ptr::null_mut();
        let mut out_len = 0u64;

        unsafe {
            // Windows
            let p1 = b"C:/PROGRA~1/ipadic";
            let p2 = b"C:/PROGRA~1/ipadic/dicrc";
            assert!(memento_util_format_mecab_arg(
                true,
                false,
                p1.as_ptr() as *const c_char,
                p1.len() as u64,
                p2.as_ptr() as *const c_char,
                p2.len() as u64,
                &mut out,
                &mut out_len,
            ));
            assert!(!out.is_null());
            assert_eq!(out_len, " -d C:/PROGRA~1/ipadic -r C:/PROGRA~1/ipadic/dicrc".len() as u64);
            let s = std::str::from_utf8(std::slice::from_raw_parts(out as *const u8, out_len as usize)).unwrap();
            assert_eq!(s, " -d C:/PROGRA~1/ipadic -r C:/PROGRA~1/ipadic/dicrc");
            memento_util_free_string(out, out_len);

            // GetShortPathNameA returns bytes in the local ANSI code page.
            let ansi_dir = b"C:/\xff/dic";
            let ansi_rc = b"C:/\xfe/dicrc";
            assert!(memento_util_format_mecab_arg(
                true, false,
                ansi_dir.as_ptr() as *const c_char, ansi_dir.len() as u64,
                ansi_rc.as_ptr() as *const c_char, ansi_rc.len() as u64,
                &mut out, &mut out_len,
            ));
            assert_eq!(
                std::slice::from_raw_parts(out as *const u8, out_len as usize),
                b" -d C:/\xff/dic -r C:/\xfe/dicrc"
            );
            memento_util_free_string(out, out_len);

            // Bundle
            let dir = b"/bundle/mecab/";
            assert!(memento_util_format_mecab_arg(
                false,
                true,
                dir.as_ptr() as *const c_char,
                dir.len() as u64,
                ptr::null(),
                0,
                &mut out,
                &mut out_len,
            ));
            assert!(!out.is_null());
            let s = std::str::from_utf8(std::slice::from_raw_parts(out as *const u8, out_len as usize)).unwrap();
            assert_eq!(s, "-r /bundle/mecab/ipadic/dicrc -d /bundle/mecab/ipadic");
            memento_util_free_string(out, out_len);

            // Standard Linux (empty)
            assert!(memento_util_format_mecab_arg(
                false,
                false,
                ptr::null(),
                0,
                ptr::null(),
                0,
                &mut out,
                &mut out_len,
            ));
            assert!(out.is_null());
            assert_eq!(out_len, 0);
            memento_util_free_string(out, out_len);

            // Null out or out_len fails
            assert!(!memento_util_format_mecab_arg(
                true,
                false,
                p1.as_ptr() as *const c_char,
                p1.len() as u64,
                p2.as_ptr() as *const c_char,
                p2.len() as u64,
                ptr::null_mut(),
                &mut out_len,
            ));
        }
    }
}

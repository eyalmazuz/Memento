//! Rust-owned UTF-16 result buffers for the Qt dictionary search shells.

use std::ffi::{c_char, CStr};
use std::panic::catch_unwind;
use std::ptr;

use crate::exactquerygenerator;
use crate::deconjugator::{self, WordForm};
use crate::data::dictionarystyles_parser::parse_stylesheet;
use crate::deconjugationquerygenerator;
use crate::mecabquerygenerator::{self, Node};
use crate::databasemanager;
use crate::searchcontroller;
use crate::sqlite_db;

#[repr(C)]
pub struct DictUtf16C {
    pub data: *mut u16,
    pub len: u64,
    pub capacity: u64,
}

impl DictUtf16C {
    fn empty() -> Self {
        Self { data: ptr::null_mut(), len: 0, capacity: 0 }
    }

    fn from_vec(mut source: Vec<u16>) -> Self {
        if source.is_empty() {
            return Self::empty();
        }
        let result = Self {
            data: source.as_mut_ptr(),
            len: source.len() as u64,
            capacity: source.capacity() as u64,
        };
        std::mem::forget(source);
        result
    }

    unsafe fn free(self) {
        if !self.data.is_null() {
            drop(unsafe { Vec::from_raw_parts(self.data, self.len as usize, self.capacity as usize) });
        }
    }
}

#[repr(C)]
pub struct DictExactQueryC {
    pub deconj: DictUtf16C,
    pub surface: DictUtf16C,
    pub surface_is_deconj: bool,
}

#[repr(C)]
pub struct DictExactQueriesC {
    pub items: *mut DictExactQueryC,
    pub len: u64,
    pub capacity: u64,
}

impl DictExactQueriesC {
    fn empty() -> Self {
        Self { items: ptr::null_mut(), len: 0, capacity: 0 }
    }
}

/// # Safety
/// `text` must refer to `len` readable UTF-16 units (or be null if len=0),
/// and `out` must be writable and nonaliasing. Free the returned collection.
#[no_mangle]
pub unsafe extern "C" fn memento_dict_exact_queries(
    text: *const u16, len: u64, out: *mut DictExactQueriesC,
) -> bool {
    if out.is_null() { return false; }
    unsafe { out.write(DictExactQueriesC::empty()) };
    let Ok(length) = usize::try_from(len) else { return false };
    if length > isize::MAX as usize / std::mem::size_of::<u16>() ||
        (length != 0 && text.is_null()) { return false; }
    let input = if length == 0 { &[] } else {
        unsafe { std::slice::from_raw_parts(text, length) }
    };
    let Ok(queries) = catch_unwind(|| exactquerygenerator::generate_queries(input)) else {
        return false;
    };
    if queries.is_empty() { return true; }
    let mut raw = Vec::with_capacity(queries.len());
    for query in queries {
        let (deconj, surface) = query.into_parts();
        raw.push(DictExactQueryC {
            deconj: DictUtf16C::from_vec(deconj),
            surface_is_deconj: surface.is_none(),
            surface: surface.map_or_else(DictUtf16C::empty, DictUtf16C::from_vec),
        });
    }
    let result = DictExactQueriesC {
        items: raw.as_mut_ptr(), len: raw.len() as u64, capacity: raw.capacity() as u64,
    };
    std::mem::forget(raw);
    unsafe { out.write(result) };
    true
}

/// # Safety
/// `out` must be null, zero-initialized, or returned by the matching Rust
/// function. Passing C++-owned or already-freed nested pointers is invalid.
#[no_mangle]
pub unsafe extern "C" fn memento_dict_exact_queries_free(out: *mut DictExactQueriesC) {
    if out.is_null() { return; }
    let owned = std::mem::replace(unsafe { &mut *out }, DictExactQueriesC::empty());
    if owned.items.is_null() { return; }
    let items = unsafe {
        Vec::from_raw_parts(owned.items, owned.len as usize, owned.capacity as usize)
    };
    for item in items {
        unsafe { item.deconj.free(); item.surface.free(); }
    }
}

const _: () = assert!(std::mem::size_of::<WordForm>() == std::mem::size_of::<u32>());

#[repr(C)]
pub struct DictConjugationC {
    pub base: DictUtf16C,
    pub conjugated: DictUtf16C,
    pub derivations: *mut u32,
    pub derivations_len: u64,
    pub derivations_capacity: u64,
    pub derivation_display: DictUtf16C,
}

#[repr(C)]
pub struct DictConjugationsC {
    pub items: *mut DictConjugationC,
    pub len: u64,
    pub capacity: u64,
}

impl DictConjugationsC {
    fn empty() -> Self {
        Self { items: ptr::null_mut(), len: 0, capacity: 0 }
    }
}

/// # Safety
/// `text` must contain `len` readable UTF-16 units (or be null if len=0);
/// `out` must be writable and nonaliasing. Free results with the paired call.
#[no_mangle]
pub unsafe extern "C" fn memento_dict_deconjugate(
    text: *const u16, len: u64, sentence_mode: bool, query_is_null: bool,
    out: *mut DictConjugationsC,
) -> bool {
    if out.is_null() { return false; }
    unsafe { out.write(DictConjugationsC::empty()) };
    let Ok(length) = usize::try_from(len) else { return false };
    if length > isize::MAX as usize / std::mem::size_of::<u16>() ||
        (length != 0 && text.is_null()) { return false; }
    if query_is_null {
        // `QString()` is null; QStringLiteral("") is non-null. In the
        // original the null value does not match empty conjugation suffixes.
        return length == 0;
    }
    let input = if length == 0 { &[] } else {
        unsafe { std::slice::from_raw_parts(text, length) }
    };
    let Ok(infos) = catch_unwind(|| deconjugator::deconjugate(input, sentence_mode)) else {
        return false;
    };
    if infos.is_empty() { return true; }
    let mut raw = Vec::with_capacity(infos.len());
    for info in infos {
        let mut derivations = info.derivations;
        let (derivation_data, derivations_len, derivations_capacity) =
            if derivations.is_empty() {
                (ptr::null_mut(), 0, 0)
            } else {
                let values = (derivations.as_mut_ptr().cast::<u32>(),
                              derivations.len() as u64, derivations.capacity() as u64);
                std::mem::forget(derivations);
                values
            };
        raw.push(DictConjugationC {
            base: DictUtf16C::from_vec(info.base),
            conjugated: DictUtf16C::from_vec(info.conjugated),
            derivations: derivation_data,
            derivations_len,
            derivations_capacity,
            derivation_display: DictUtf16C::from_vec(info.derivation_display),
        });
    }
    let result = DictConjugationsC {
        items: raw.as_mut_ptr(), len: raw.len() as u64, capacity: raw.capacity() as u64,
    };
    std::mem::forget(raw);
    unsafe { out.write(result) };
    true
}

/// # Safety
/// `out` must be null, zero-initialized or returned by the matching Rust call.
#[no_mangle]
pub unsafe extern "C" fn memento_dict_deconjugations_free(out: *mut DictConjugationsC) {
    if out.is_null() { return; }
    let owned = std::mem::replace(unsafe { &mut *out }, DictConjugationsC::empty());
    if owned.items.is_null() { return; }
    let items = unsafe {
        Vec::from_raw_parts(owned.items, owned.len as usize, owned.capacity as usize)
    };
    for item in items {
        unsafe {
            item.base.free();
            item.conjugated.free();
            item.derivation_display.free();
            if !item.derivations.is_null() {
                drop(Vec::from_raw_parts(item.derivations.cast::<WordForm>(),
                                         item.derivations_len as usize,
                                         item.derivations_capacity as usize));
            }
        }
    }
}

#[repr(C)]
pub struct DictDeconjQueryC {
    pub deconj: DictUtf16C,
    pub surface: DictUtf16C,
    pub derivation_display: DictUtf16C,
    pub rule_mask: u8,
}

#[repr(C)]
pub struct DictDeconjQueriesC {
    pub items: *mut DictDeconjQueryC,
    pub len: u64,
    pub capacity: u64,
}

impl DictDeconjQueriesC {
    fn empty() -> Self {
        Self { items: ptr::null_mut(), len: 0, capacity: 0 }
    }
}

/// # Safety
/// `text` must refer to `len` UTF-16 units or be null for len=0, while
/// `out` must be writable and nonaliasing.
#[no_mangle]
pub unsafe extern "C" fn memento_dict_deconjugation_queries(
    text: *const u16, len: u64, out: *mut DictDeconjQueriesC,
) -> bool {
    if out.is_null() { return false; }
    unsafe { out.write(DictDeconjQueriesC::empty()) };
    let Ok(length) = usize::try_from(len) else { return false };
    if length > isize::MAX as usize / std::mem::size_of::<u16>() ||
        (length != 0 && text.is_null()) { return false; }
    let input = if length == 0 { &[] } else {
        unsafe { std::slice::from_raw_parts(text, length) }
    };
    let Ok(queries) = catch_unwind(|| deconjugationquerygenerator::generate_queries(input)) else {
        return false;
    };
    if queries.is_empty() { return true; }
    let mut raw = Vec::with_capacity(queries.len());
    for query in queries {
        raw.push(DictDeconjQueryC {
            deconj: DictUtf16C::from_vec(query.deconj),
            surface: DictUtf16C::from_vec(query.surface),
            derivation_display: DictUtf16C::from_vec(query.derivation_display),
            rule_mask: query.rule_mask,
        });
    }
    let result = DictDeconjQueriesC {
        items: raw.as_mut_ptr(), len: raw.len() as u64, capacity: raw.capacity() as u64,
    };
    std::mem::forget(raw);
    unsafe { out.write(result) };
    true
}

/// # Safety
/// `out` must be null, zero-initialized or returned by the matching Rust call.
#[no_mangle]
pub unsafe extern "C" fn memento_dict_deconjugation_queries_free(out: *mut DictDeconjQueriesC) {
    if out.is_null() { return; }
    let owned = std::mem::replace(unsafe { &mut *out }, DictDeconjQueriesC::empty());
    if owned.items.is_null() { return; }
    let items = unsafe {
        Vec::from_raw_parts(owned.items, owned.len as usize, owned.capacity as usize)
    };
    for item in items {
        unsafe {
            item.deconj.free();
            item.surface.free();
            item.derivation_display.free();
        }
    }
}

#[repr(C)]
pub struct DictMecabNodeC {
    pub deconj: *const u16,
    pub deconj_len: u64,
    pub surface: *const u16,
    pub surface_len: u64,
    pub surface_clean: *const u16,
    pub surface_clean_len: u64,
    pub next_index: u64,
    pub bnext_index: u64,
}

#[repr(C)]
pub struct DictMecabQueryC {
    pub deconj: DictUtf16C,
    pub surface: DictUtf16C,
}

#[repr(C)]
pub struct DictMecabQueriesC {
    pub items: *mut DictMecabQueryC,
    pub len: u64,
    pub capacity: u64,
}

impl DictMecabQueriesC {
    fn empty() -> Self {
        Self { items: ptr::null_mut(), len: 0, capacity: 0 }
    }
}

unsafe fn borrowed_utf16<'a>(data: *const u16, len: u64) -> Option<&'a [u16]> {
    let size = usize::try_from(len).ok()?;
    if size > isize::MAX as usize / std::mem::size_of::<u16>() { return None; }
    if size == 0 { return Some(&[]); }
    if data.is_null() { return None; }
    Some(unsafe { std::slice::from_raw_parts(data, size) })
}

fn edge(index: u64, size: usize) -> Option<Option<usize>> {
    if index == u64::MAX { return Some(None); }
    let index = usize::try_from(index).ok()?;
    (index < size).then_some(Some(index))
}

/// # Safety
/// `nodes` must contain `nodes_len` readable entries with valid UTF-16
/// buffers. The input may be borrowed only until this function returns.
#[no_mangle]
pub unsafe extern "C" fn memento_dict_mecab_queries(
    nodes: *const DictMecabNodeC, nodes_len: u64, start_index: u64,
    out: *mut DictMecabQueriesC,
) -> bool {
    if out.is_null() { return false; }
    unsafe { out.write(DictMecabQueriesC::empty()) };
    let Ok(length) = usize::try_from(nodes_len) else { return false };
    if length > isize::MAX as usize / std::mem::size_of::<DictMecabNodeC>() ||
        (length != 0 && nodes.is_null()) { return false; }
    let source = if length == 0 { &[] } else {
        unsafe { std::slice::from_raw_parts(nodes, length) }
    };
    let mut borrowed = Vec::with_capacity(length);
    for item in source {
        let (Some(deconj), Some(surface), Some(surface_clean),
             Some(next), Some(bnext)) = (
            unsafe { borrowed_utf16(item.deconj, item.deconj_len) },
            unsafe { borrowed_utf16(item.surface, item.surface_len) },
            unsafe { borrowed_utf16(item.surface_clean, item.surface_clean_len) },
            edge(item.next_index, length), edge(item.bnext_index, length),
        ) else { return false; };
        borrowed.push(Node { deconj, surface, surface_clean, next, bnext });
    }
    let Some(start) = edge(start_index, length) else { return false };
    let Ok(Some(queries)) = catch_unwind(|| mecabquerygenerator::generate_queries(&borrowed, start)) else {
        return false;
    };
    if queries.is_empty() { return true; }
    let mut raw = Vec::with_capacity(queries.len());
    for query in queries {
        raw.push(DictMecabQueryC {
            deconj: DictUtf16C::from_vec(query.deconj),
            surface: DictUtf16C::from_vec(query.surface),
        });
    }
    let result = DictMecabQueriesC {
        items: raw.as_mut_ptr(), len: raw.len() as u64, capacity: raw.capacity() as u64,
    };
    std::mem::forget(raw);
    unsafe { out.write(result) };
    true
}

/// # Safety
/// `out` must be null, zero-initialized or returned by the matching Rust call.
#[no_mangle]
pub unsafe extern "C" fn memento_dict_mecab_queries_free(out: *mut DictMecabQueriesC) {
    if out.is_null() { return; }
    let owned = std::mem::replace(unsafe { &mut *out }, DictMecabQueriesC::empty());
    if owned.items.is_null() { return; }
    let items = unsafe {
        Vec::from_raw_parts(owned.items, owned.len as usize, owned.capacity as usize)
    };
    for item in items {
        unsafe {
            item.deconj.free();
            item.surface.free();
        }
    }
}

#[repr(C)]
pub struct DictBytesC {
    pub data: *mut u8,
    pub len: u64,
    pub capacity: u64,
}

impl DictBytesC {
    fn empty() -> Self {
        Self { data: ptr::null_mut(), len: 0, capacity: 0 }
    }
}

/// # Safety
/// `text` contains `len` readable UTF-16 units (or is null if len=0), and
/// `out` is writable and nonaliasing. Paired byte-free releases the JSON.
#[no_mangle]
pub unsafe extern "C" fn memento_dict_parse_css(
    text: *const u16, len: u64, out: *mut DictBytesC,
) -> bool {
    if out.is_null() { return false; }
    unsafe { out.write(DictBytesC::empty()) };
    let Ok(length) = usize::try_from(len) else { return false };
    if length > isize::MAX as usize / std::mem::size_of::<u16>() ||
        (length != 0 && text.is_null()) { return false; }
    let input = if length == 0 { &[] } else {
        unsafe { std::slice::from_raw_parts(text, length) }
    };
    let Ok(Ok(mut bytes)) = catch_unwind(|| {
        serde_json::to_vec(&parse_stylesheet(input))
    }) else { return false; };
    let result = DictBytesC {
        data: bytes.as_mut_ptr(), len: bytes.len() as u64, capacity: bytes.capacity() as u64,
    };
    std::mem::forget(bytes);
    unsafe { out.write(result) };
    true
}

/// # Safety
/// `out` must be null, zero-initialized, or returned by `memento_dict_parse_css`.
#[no_mangle]
pub unsafe extern "C" fn memento_dict_bytes_free(out: *mut DictBytesC) {
    if out.is_null() { return; }
    let owned = std::mem::replace(unsafe { &mut *out }, DictBytesC::empty());
    if !owned.data.is_null() {
        drop(unsafe { Vec::from_raw_parts(owned.data, owned.len as usize, owned.capacity as usize) });
    }
}

/// Allocated collection of UTF-16 strings for DatabaseManager C ABI.
#[repr(C)]
pub struct DictStringListC {
    pub items: *mut DictUtf16C,
    pub len: u64,
    pub capacity: u64,
}

impl DictStringListC {
    fn empty() -> Self {
        Self { items: ptr::null_mut(), len: 0, capacity: 0 }
    }
}

/// # Safety
/// `query` must point to `len` readable u16 units (or be null if len=0).
/// `out` and `out_len` must be writable. Caller frees `*out` with `memento_dict_free_u16`.
#[no_mangle]
pub unsafe extern "C" fn memento_dict_kata_to_hira(
    query: *const u16, len: u64,
    out: *mut *mut u16, out_len: *mut u64,
) -> bool {
    if out.is_null() || out_len.is_null() { return false; }
    unsafe {
        *out = ptr::null_mut();
        *out_len = 0;
    }
    let Ok(length) = usize::try_from(len) else { return false };
    if length > isize::MAX as usize / std::mem::size_of::<u16>() ||
        (length != 0 && query.is_null()) { return false; }
    let input = if length == 0 { &[] } else {
        unsafe { std::slice::from_raw_parts(query, length) }
    };
    let Ok(result) = catch_unwind(|| databasemanager::kata_to_hira(input)) else {
        return false;
    };
    if result.is_empty() {
        return true;
    }
    let mut boxed = result.into_boxed_slice();
    unsafe {
        *out = boxed.as_mut_ptr();
        *out_len = boxed.len() as u64;
    }
    std::mem::forget(boxed);
    true
}

/// # Safety
/// `data` must have been returned by a Rust allocation of `len` u16 units,
/// or be null / len=0.
#[no_mangle]
pub unsafe extern "C" fn memento_dict_free_u16(data: *mut u16, len: u64) {
    if !data.is_null() && len > 0 {
        if let Ok(size) = usize::try_from(len) {
            drop(unsafe { Box::from_raw(std::slice::from_raw_parts_mut(data, size)) });
        }
    }
}

/// # Safety
/// `reading` must point to `len` readable u16 units (or be null if len=0).
/// `out` must be writable. Caller frees with `memento_dict_string_list_free`.
#[no_mangle]
pub unsafe extern "C" fn memento_dict_split_mora(
    reading: *const u16, len: u64,
    out: *mut DictStringListC,
) -> bool {
    if out.is_null() { return false; }
    unsafe { out.write(DictStringListC::empty()) };
    let Ok(length) = usize::try_from(len) else { return false };
    if length > isize::MAX as usize / std::mem::size_of::<u16>() ||
        (length != 0 && reading.is_null()) { return false; }
    let input = if length == 0 { &[] } else {
        unsafe { std::slice::from_raw_parts(reading, length) }
    };
    let Ok(moras) = catch_unwind(|| databasemanager::split_mora(input)) else {
        return false;
    };
    if moras.is_empty() {
        return true;
    }
    let mut raw: Vec<DictUtf16C> = Vec::with_capacity(moras.len());
    for m in moras {
        raw.push(DictUtf16C::from_vec(m));
    }
    let result = DictStringListC {
        items: raw.as_mut_ptr(),
        len: raw.len() as u64,
        capacity: raw.capacity() as u64,
    };
    std::mem::forget(raw);
    unsafe { out.write(result) };
    true
}

/// # Safety
/// `out` must be null, zero-initialized or returned by `memento_dict_split_mora`.
#[no_mangle]
pub unsafe extern "C" fn memento_dict_string_list_free(out: *mut DictStringListC) {
    if out.is_null() { return; }
    let owned = std::mem::replace(unsafe { &mut *out }, DictStringListC::empty());
    if owned.items.is_null() { return; }
    let items = unsafe {
        Vec::from_raw_parts(owned.items, owned.len as usize, owned.capacity as usize)
    };
    for item in items {
        unsafe { item.free(); }
    }
}

#[no_mangle]
pub extern "C" fn memento_dict_is_step_error(step: i32) -> bool {
    databasemanager::is_step_error(step)
}

/// # Safety
/// All UTF-16 pointers must be readable for their associated length (or null if len=0).
/// `out`, `out_len`, and `out_skip` must be writable. Caller frees `*out` with `memento_dict_free_u16`.
#[no_mangle]
pub unsafe extern "C" fn memento_dict_extract_frequency_object(
    has_reading_key: bool,
    obj_reading: *const u16, obj_reading_len: u64,
    target_reading: *const u16, target_reading_len: u64,
    freq_kind: i32,
    freq_str: *const u16, freq_str_len: u64,
    has_nested_display: bool,
    nested_display: *const u16, nested_display_len: u64,
    has_nested_value_num: bool,
    nested_value_num_str: *const u16, nested_value_num_len: u64,
    has_top_display: bool,
    top_display: *const u16, top_display_len: u64,
    has_top_value_num: bool,
    top_value_num_str: *const u16, top_value_num_len: u64,
    out: *mut *mut u16,
    out_len: *mut u64,
    out_skip: *mut bool,
) -> bool {
    if out.is_null() || out_len.is_null() || out_skip.is_null() {
        return false;
    }
    unsafe {
        *out = ptr::null_mut();
        *out_len = 0;
        *out_skip = false;
    }
    let Some(obj_reading_slice) = (unsafe { borrowed_utf16(obj_reading, obj_reading_len) }) else { return false; };
    let Some(target_reading_slice) = (unsafe { borrowed_utf16(target_reading, target_reading_len) }) else { return false; };
    let Some(freq_str_slice) = (unsafe { borrowed_utf16(freq_str, freq_str_len) }) else { return false; };
    let Some(nested_display_slice) = (unsafe { borrowed_utf16(nested_display, nested_display_len) }) else { return false; };
    let Some(nested_val_slice) = (unsafe { borrowed_utf16(nested_value_num_str, nested_value_num_len) }) else { return false; };
    let Some(top_display_slice) = (unsafe { borrowed_utf16(top_display, top_display_len) }) else { return false; };
    let Some(top_val_slice) = (unsafe { borrowed_utf16(top_value_num_str, top_value_num_len) }) else { return false; };

    let Ok(res) = catch_unwind(|| {
        databasemanager::extract_frequency_object(
            has_reading_key,
            obj_reading_slice,
            target_reading_slice,
            freq_kind,
            freq_str_slice,
            has_nested_display,
            nested_display_slice,
            has_nested_value_num,
            nested_val_slice,
            has_top_display,
            top_display_slice,
            has_top_value_num,
            top_val_slice,
        )
    }) else {
        return false;
    };

    match res {
        None => {
            unsafe { *out_skip = true; }
            true
        }
        Some(vec) => {
            unsafe { *out_skip = false; }
            if vec.is_empty() {
                return true;
            }
            let mut boxed = vec.into_boxed_slice();
            unsafe {
                *out = boxed.as_mut_ptr();
                *out_len = boxed.len() as u64;
            }
            std::mem::forget(boxed);
            true
        }
    }
}

/// # Safety
/// All UTF-16 pointers must be readable for their lengths (or null if len=0).
/// Output pointers must be writable. Caller frees `*hiragana_out` with `memento_dict_free_u16`.
#[no_mangle]
pub unsafe extern "C" fn memento_dict_plan_term_query(
    raw_query: *const u16,
    raw_query_len: u64,
    nfkc_query: *const u16,
    nfkc_query_len: u64,
    hiragana_out: *mut *mut u16,
    hiragana_out_len: *mut u64,
    contains_half_out: *mut bool,
    contains_kata_out: *mut bool,
    query_kind_out: *mut i32,
) -> bool {
    if hiragana_out.is_null()
        || hiragana_out_len.is_null()
        || contains_half_out.is_null()
        || contains_kata_out.is_null()
        || query_kind_out.is_null()
    {
        return false;
    }
    unsafe {
        *hiragana_out = ptr::null_mut();
        *hiragana_out_len = 0;
        *contains_half_out = false;
        *contains_kata_out = false;
        *query_kind_out = 0;
    }

    let Some(raw_slice) = (unsafe { borrowed_utf16(raw_query, raw_query_len) }) else {
        return false;
    };
    let Some(nfkc_slice) = (unsafe { borrowed_utf16(nfkc_query, nfkc_query_len) }) else {
        return false;
    };

    let Ok(plan) = catch_unwind(|| databasemanager::plan_term_query(raw_slice, nfkc_slice)) else {
        return false;
    };

    unsafe {
        *contains_half_out = plan.contains_half;
        *contains_kata_out = plan.contains_kata;
        *query_kind_out = plan.query_kind as i32;
    }

    if !plan.hiragana.is_empty() {
        let mut boxed = plan.hiragana.into_boxed_slice();
        unsafe {
            *hiragana_out = boxed.as_mut_ptr();
            *hiragana_out_len = boxed.len() as u64;
        }
        std::mem::forget(boxed);
    }
    true
}

/// # Safety
/// `jsonstr` must be null or a null-terminated UTF-8 C string.
/// `out` must be writable. Caller frees with `memento_dict_string_list_free`.
#[no_mangle]
pub unsafe extern "C" fn memento_dict_json_array_to_strings(
    jsonstr: *const std::os::raw::c_char,
    out: *mut DictStringListC,
) -> bool {
    if out.is_null() {
        return false;
    }
    unsafe { out.write(DictStringListC::empty()) };
    let bytes = if jsonstr.is_null() {
        &[]
    } else {
        unsafe { std::ffi::CStr::from_ptr(jsonstr).to_bytes() }
    };

    let Ok(strings) = catch_unwind(|| databasemanager::json_array_to_strings(bytes)) else {
        return false;
    };

    if strings.is_empty() {
        return true;
    }

    let mut raw: Vec<DictUtf16C> = Vec::with_capacity(strings.len());
    for s in strings {
        raw.push(DictUtf16C::from_vec(s));
    }
    let result = DictStringListC {
        items: raw.as_mut_ptr(),
        len: raw.len() as u64,
        capacity: raw.capacity() as u64,
    };
    std::mem::forget(raw);
    unsafe { out.write(result) };
    true
}

/// # Safety
/// `json_utf8` must be null or a null-terminated UTF-8 C string.
/// UTF-16 pointers must be readable for their lengths (or null if len=0).
/// Output pointers must be writable. Caller frees `out_mora` with `memento_dict_string_list_free`
/// and `out_positions` with `memento_dict_i32_list_free`.
#[no_mangle]
pub unsafe extern "C" fn memento_dict_parse_pitch_blob(
    json_utf8: *const std::os::raw::c_char,
    term_exp: *const u16,
    term_exp_len: u64,
    term_reading: *const u16,
    term_reading_len: u64,
    out_matched: *mut bool,
    out_mora: *mut DictStringListC,
    out_positions: *mut *mut i32,
    out_positions_len: *mut u64,
) -> bool {
    if out_matched.is_null()
        || out_mora.is_null()
        || out_positions.is_null()
        || out_positions_len.is_null()
    {
        return false;
    }
    unsafe {
        *out_matched = false;
        out_mora.write(DictStringListC::empty());
        *out_positions = ptr::null_mut();
        *out_positions_len = 0;
    }

    let json_bytes = if json_utf8.is_null() {
        &[]
    } else {
        unsafe { std::ffi::CStr::from_ptr(json_utf8).to_bytes() }
    };

    let Some(exp_slice) = (unsafe { borrowed_utf16(term_exp, term_exp_len) }) else {
        return false;
    };
    let Some(reading_slice) = (unsafe { borrowed_utf16(term_reading, term_reading_len) }) else {
        return false;
    };

    let Ok(data) = catch_unwind(|| {
        databasemanager::parse_pitch_blob(json_bytes, exp_slice, reading_slice)
    }) else {
        return false;
    };

    if !data.matched {
        return true;
    }

    unsafe { *out_matched = true };

    if !data.mora.is_empty() {
        let mut raw: Vec<DictUtf16C> = Vec::with_capacity(data.mora.len());
        for m in data.mora {
            raw.push(DictUtf16C::from_vec(m));
        }
        let result = DictStringListC {
            items: raw.as_mut_ptr(),
            len: raw.len() as u64,
            capacity: raw.capacity() as u64,
        };
        std::mem::forget(raw);
        unsafe { out_mora.write(result) };
    }

    if !data.positions.is_empty() {
        let mut boxed = data.positions.into_boxed_slice();
        unsafe {
            *out_positions = boxed.as_mut_ptr();
            *out_positions_len = boxed.len() as u64;
        }
        std::mem::forget(boxed);
    }

    true
}

/// # Safety
/// `data` must have been allocated by Rust with `len` i32 elements, or be null.
#[no_mangle]
pub unsafe extern "C" fn memento_dict_i32_list_free(data: *mut i32, len: u64) {
    if !data.is_null() && len > 0 {
        if let Ok(size) = usize::try_from(len) {
            drop(unsafe { Box::from_raw(std::slice::from_raw_parts_mut(data, size)) });
        }
    }
}

/// # Safety
/// `category` must point to `len` readable u16 units (or be null if len=0).
#[no_mangle]
pub unsafe extern "C" fn memento_dict_classify_kanji_stat_category(
    category: *const u16,
    len: u64,
) -> i32 {
    let Some(slice) = (unsafe { borrowed_utf16(category, len) }) else {
        return databasemanager::KanjiStatCategory::Skip as i32;
    };
    databasemanager::classify_kanji_stat_category(slice) as i32
}

/// # Safety
/// All UTF-16 pointers must be readable for their lengths (or null if len=0).
/// `out` and `out_len` must be writable. Caller frees `*out` with `memento_dict_free_u16`.
#[no_mangle]
pub unsafe extern "C" fn memento_dict_build_style_path(
    resource_path: *const u16,
    resource_path_len: u64,
    dict_name: *const u16,
    dict_name_len: u64,
    out: *mut *mut u16,
    out_len: *mut u64,
) -> bool {
    if out.is_null() || out_len.is_null() {
        return false;
    }
    unsafe {
        *out = ptr::null_mut();
        *out_len = 0;
    }

    let Some(res_slice) = (unsafe { borrowed_utf16(resource_path, resource_path_len) }) else {
        return false;
    };
    let Some(name_slice) = (unsafe { borrowed_utf16(dict_name, dict_name_len) }) else {
        return false;
    };

    let Ok(path) = catch_unwind(|| databasemanager::build_style_path(res_slice, name_slice)) else {
        return false;
    };

    if path.is_empty() {
        return true;
    }

    let mut boxed = path.into_boxed_slice();
    unsafe {
        *out = boxed.as_mut_ptr();
        *out_len = boxed.len() as u64;
    }
    std::mem::forget(boxed);
    true
}


#[repr(C)]
pub struct DictIndicesC {
    pub items: *mut u64,
    pub len: u64,
    pub capacity: u64,
}

impl DictIndicesC {
    fn empty() -> Self {
        Self {
            items: ptr::null_mut(),
            len: 0,
            capacity: 0,
        }
    }

    fn from_vec(mut v: Vec<u64>) -> Self {
        if v.is_empty() {
            return Self::empty();
        }
        let result = Self {
            items: v.as_mut_ptr(),
            len: v.len() as u64,
            capacity: v.capacity() as u64,
        };
        std::mem::forget(v);
        result
    }

    unsafe fn free(self) {
        if !self.items.is_null() {
            drop(unsafe { Vec::from_raw_parts(self.items, self.len as usize, self.capacity as usize) });
        }
    }
}

/// # Safety
/// `out` must be null, zero-initialized or returned by a memento_dict function returning DictIndicesC.
#[no_mangle]
pub unsafe extern "C" fn memento_dict_indices_free(out: *mut DictIndicesC) {
    if out.is_null() { return; }
    let owned = std::mem::replace(unsafe { &mut *out }, DictIndicesC::empty());
    unsafe { owned.free(); }
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct DictSearchQueryInC {
    pub original_index: usize,
    pub deconj: *const u16,
    pub deconj_len: u64,
    pub surface_len: u64,
    pub rule_mask: u8,
    pub source: u8,
}

/// # Safety
/// `queries` must point to `len` instances of `DictSearchQueryInC`.
/// `out` must be writable.
#[no_mangle]
pub unsafe extern "C" fn memento_dict_sort_and_filter_queries(
    queries: *const DictSearchQueryInC,
    len: u64,
    mecab_enabled: bool,
    out: *mut DictIndicesC,
) -> bool {
    if out.is_null() { return false; }
    unsafe { out.write(DictIndicesC::empty()) };
    let Ok(length) = usize::try_from(len) else { return false };
    if length > 0 && queries.is_null() { return false; }

    let raw_slice = if length == 0 { &[] } else {
        unsafe { std::slice::from_raw_parts(queries, length) }
    };

    let mut candidate_queries = Vec::with_capacity(length);
    for item in raw_slice {
        let Some(deconj_slice) = (unsafe { borrowed_utf16(item.deconj, item.deconj_len) }) else {
            return false;
        };
        candidate_queries.push(searchcontroller::CandidateSearchQuery {
            original_index: item.original_index,
            deconj: deconj_slice,
            surface_len: item.surface_len,
            rule_mask: item.rule_mask,
            source: searchcontroller::QuerySource::from_u8(item.source),
        });
    }

    let Ok(indices) = catch_unwind(|| {
        searchcontroller::sort_and_filter_queries(candidate_queries, mecab_enabled)
    }) else {
        return false;
    };

    let indices_u64: Vec<u64> = indices.into_iter().map(|idx| idx as u64).collect();
    unsafe { out.write(DictIndicesC::from_vec(indices_u64)) };
    true
}

/// # Safety
/// `queries` must point to `len` instances of `DictSearchQueryInC`.
/// `out` must be writable.
#[no_mangle]
pub unsafe extern "C" fn memento_dict_sort_queries(
    queries: *const DictSearchQueryInC,
    len: u64,
    out: *mut DictIndicesC,
) -> bool {
    if out.is_null() { return false; }
    unsafe { out.write(DictIndicesC::empty()) };
    let Ok(length) = usize::try_from(len) else { return false };
    if length > 0 && queries.is_null() { return false; }

    let raw_slice = if length == 0 { &[] } else {
        unsafe { std::slice::from_raw_parts(queries, length) }
    };

    let mut candidate_queries = Vec::with_capacity(length);
    for item in raw_slice {
        let Some(deconj_slice) = (unsafe { borrowed_utf16(item.deconj, item.deconj_len) }) else {
            return false;
        };
        candidate_queries.push(searchcontroller::CandidateSearchQuery {
            original_index: item.original_index,
            deconj: deconj_slice,
            surface_len: item.surface_len,
            rule_mask: item.rule_mask,
            source: searchcontroller::QuerySource::from_u8(item.source),
        });
    }

    let Ok(indices) = catch_unwind(move || {
        searchcontroller::sort_queries(&mut candidate_queries);
        candidate_queries.into_iter().map(|q| q.original_index as u64).collect::<Vec<_>>()
    }) else {
        return false;
    };

    unsafe { out.write(DictIndicesC::from_vec(indices)) };
    true
}

/// # Safety
/// `queries` must point to `len` instances of `DictSearchQueryInC`.
/// `out` must be writable.
#[no_mangle]
pub unsafe extern "C" fn memento_dict_filter_duplicate_queries(
    queries: *const DictSearchQueryInC,
    len: u64,
    mecab_enabled: bool,
    out: *mut DictIndicesC,
) -> bool {
    if out.is_null() { return false; }
    unsafe { out.write(DictIndicesC::empty()) };
    let Ok(length) = usize::try_from(len) else { return false };
    if length > 0 && queries.is_null() { return false; }

    let raw_slice = if length == 0 { &[] } else {
        unsafe { std::slice::from_raw_parts(queries, length) }
    };

    let mut candidate_queries = Vec::with_capacity(length);
    for item in raw_slice {
        let Some(deconj_slice) = (unsafe { borrowed_utf16(item.deconj, item.deconj_len) }) else {
            return false;
        };
        candidate_queries.push(searchcontroller::CandidateSearchQuery {
            original_index: item.original_index,
            deconj: deconj_slice,
            surface_len: item.surface_len,
            rule_mask: item.rule_mask,
            source: searchcontroller::QuerySource::from_u8(item.source),
        });
    }

    let Ok(indices) = catch_unwind(move || {
        searchcontroller::filter_duplicates(&mut candidate_queries, mecab_enabled);
        candidate_queries.into_iter().map(|q| q.original_index as u64).collect::<Vec<_>>()
    }) else {
        return false;
    };

    unsafe { out.write(DictIndicesC::from_vec(indices)) };
    true
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct DictClozeSpansC {
    pub prefix_start: u64,
    pub prefix_len: u64,
    pub body_start: u64,
    pub body_len: u64,
    pub suffix_start: u64,
    pub suffix_len: u64,
}

/// # Safety
/// `out` must be writable.
#[no_mangle]
pub unsafe extern "C" fn memento_dict_cloze_spans(
    text_len: u64,
    index: i64,
    surface_len: u64,
    out: *mut DictClozeSpansC,
) -> bool {
    if out.is_null() { return false; }
    let Ok(spans) = catch_unwind(|| {
        searchcontroller::cloze_spans(text_len, index, surface_len)
    }) else {
        return false;
    };

    unsafe {
        out.write(DictClozeSpansC {
            prefix_start: spans.prefix_start,
            prefix_len: spans.prefix_len,
            body_start: spans.body_start,
            body_len: spans.body_len,
            suffix_start: spans.suffix_start,
            suffix_len: spans.suffix_len,
        });
    }
    true
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct DictTermSortItemC {
    pub original_index: u64,
    pub cloze_body_len: u64,
    pub expr_equals_cloze: bool,
    pub expr_len: u64,
    pub conj_explanation_len: u64,
    pub score: i64,
}

/// # Safety
/// `items` must point to `len` instances of `DictTermSortItemC`.
/// `out` must be writable.
#[no_mangle]
pub unsafe extern "C" fn memento_dict_sort_terms(
    items: *const DictTermSortItemC,
    len: u64,
    out: *mut DictIndicesC,
) -> bool {
    if out.is_null() { return false; }
    unsafe { out.write(DictIndicesC::empty()) };
    let Ok(length) = usize::try_from(len) else { return false };
    if length > 0 && items.is_null() { return false; }

    let raw_slice = if length == 0 { &[] } else {
        unsafe { std::slice::from_raw_parts(items, length) }
    };

    let rust_items: Vec<searchcontroller::TermSortItem> = raw_slice
        .iter()
        .map(|it| searchcontroller::TermSortItem {
            original_index: it.original_index as usize,
            cloze_body_len: it.cloze_body_len,
            expr_equals_cloze: it.expr_equals_cloze,
            expr_len: it.expr_len,
            conj_explanation_len: it.conj_explanation_len,
            score: it.score,
        })
        .collect();

    let Ok(indices) = catch_unwind(|| searchcontroller::sort_terms(rust_items)) else {
        return false;
    };

    let indices_u64: Vec<u64> = indices.into_iter().map(|idx| idx as u64).collect();
    unsafe { out.write(DictIndicesC::from_vec(indices_u64)) };
    true
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct DictPriorityEntryC {
    pub dict_id: i64,
    pub priority: i64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct DictPrioritySortItemC {
    pub original_index: u64,
    pub dict_id: i64,
    pub score: i64,
    pub use_score_tiebreak: bool,
}

/// # Safety
/// `priorities` must point to `priorities_len` elements.
/// `items` must point to `items_len` elements.
/// `out` must be writable.
#[no_mangle]
pub unsafe extern "C" fn memento_dict_sort_by_dict_priority(
    priorities: *const DictPriorityEntryC,
    priorities_len: u64,
    missing_default: i64,
    items: *const DictPrioritySortItemC,
    items_len: u64,
    out: *mut DictIndicesC,
) -> bool {
    if out.is_null() { return false; }
    unsafe { out.write(DictIndicesC::empty()) };
    let Ok(prio_len) = usize::try_from(priorities_len) else { return false };
    let Ok(it_len) = usize::try_from(items_len) else { return false };
    if prio_len > 0 && priorities.is_null() { return false; }
    if it_len > 0 && items.is_null() { return false; }

    let prio_slice = if prio_len == 0 { &[] } else {
        unsafe { std::slice::from_raw_parts(priorities, prio_len) }
    };
    let prio_vec: Vec<(i64, i64)> = prio_slice.iter().map(|p| (p.dict_id, p.priority)).collect();

    let items_slice = if it_len == 0 { &[] } else {
        unsafe { std::slice::from_raw_parts(items, it_len) }
    };
    let items_vec: Vec<searchcontroller::PrioritySortItem> = items_slice
        .iter()
        .map(|it| searchcontroller::PrioritySortItem {
            original_index: it.original_index as usize,
            dict_id: it.dict_id,
            score: it.score,
            use_score_tiebreak: it.use_score_tiebreak,
        })
        .collect();

    let Ok(indices) = catch_unwind(|| {
        searchcontroller::sort_by_dict_priority(&prio_vec, missing_default, items_vec)
    }) else {
        return false;
    };

    let indices_u64: Vec<u64> = indices.into_iter().map(|idx| idx as u64).collect();
    unsafe { out.write(DictIndicesC::from_vec(indices_u64)) };
    true
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct DictTagSortItemC {
    pub original_index: u64,
    pub order: i64,
    pub score: i64,
}

/// # Safety
/// `items` must point to `len` instances of `DictTagSortItemC`.
/// `out` must be writable.
#[no_mangle]
pub unsafe extern "C" fn memento_dict_sort_tags(
    items: *const DictTagSortItemC,
    len: u64,
    out: *mut DictIndicesC,
) -> bool {
    if out.is_null() { return false; }
    unsafe { out.write(DictIndicesC::empty()) };
    let Ok(length) = usize::try_from(len) else { return false };
    if length > 0 && items.is_null() { return false; }

    let raw_slice = if length == 0 { &[] } else {
        unsafe { std::slice::from_raw_parts(items, length) }
    };

    let rust_items: Vec<searchcontroller::TagSortItem> = raw_slice
        .iter()
        .map(|it| searchcontroller::TagSortItem {
            original_index: it.original_index as usize,
            order: it.order,
            score: it.score,
        })
        .collect();

    let Ok(indices) = catch_unwind(|| searchcontroller::sort_tags(rust_items)) else {
        return false;
    };

    let indices_u64: Vec<u64> = indices.into_iter().map(|idx| idx as u64).collect();
    unsafe { out.write(DictIndicesC::from_vec(indices_u64)) };
    true
}

/// Plan move within DictionaryInfoModel.
#[no_mangle]
pub extern "C" fn memento_dict_plan_move(
    from: i32,
    to: i32,
    count: i32,
    out_destination: *mut i32,
) -> bool {
    match searchcontroller::plan_move(from, to, count) {
        Some(destination) => {
            if !out_destination.is_null() {
                unsafe { *out_destination = destination; }
            }
            true
        }
        None => false,
    }
}

fn string_list_from_vecs(strings: Vec<Vec<u16>>) -> DictStringListC {
    if strings.is_empty() {
        return DictStringListC::empty();
    }
    let mut raw: Vec<DictUtf16C> = Vec::with_capacity(strings.len());
    for s in strings {
        raw.push(DictUtf16C::from_vec(s));
    }
    let res = DictStringListC {
        items: raw.as_mut_ptr(),
        len: raw.len() as u64,
        capacity: raw.capacity() as u64,
    };
    std::mem::forget(raw);
    res
}

#[repr(C)]
pub struct DictTermRowC {
    pub expression: DictUtf16C,
    pub reading: DictUtf16C,
}

#[repr(C)]
pub struct DictTermRowsC {
    pub items: *mut DictTermRowC,
    pub len: u64,
    pub capacity: u64,
}

impl DictTermRowsC {
    fn empty() -> Self {
        Self { items: ptr::null_mut(), len: 0, capacity: 0 }
    }
}

/// # Safety
/// `out` must be writable and non-null or empty.
#[no_mangle]
pub unsafe extern "C" fn memento_dict_term_rows_free(out: *mut DictTermRowsC) {
    if out.is_null() { return; }
    let owned = std::mem::replace(unsafe { &mut *out }, DictTermRowsC::empty());
    if owned.items.is_null() { return; }
    let items = unsafe {
        Vec::from_raw_parts(owned.items, owned.len as usize, owned.capacity as usize)
    };
    for item in items {
        unsafe {
            item.expression.free();
            item.reading.free();
        }
    }
}

/// # Safety
/// `db` must be a valid open SQLite connection.
/// `raw_query` and `nfkc_query` must point to valid UTF-16 units or be null if len=0.
/// `out` must be writable. Caller frees with `memento_dict_term_rows_free`.
#[no_mangle]
pub unsafe extern "C" fn memento_dict_query_terms(
    db: *mut sqlite_db::sqlite3,
    raw_query: *const u16, raw_query_len: u64,
    nfkc_query: *const u16, nfkc_query_len: u64,
    out: *mut DictTermRowsC,
) -> bool {
    if out.is_null() { return false; }
    unsafe { out.write(DictTermRowsC::empty()) };
    if db.is_null() { return false; }
    let Ok(raw_len) = usize::try_from(raw_query_len) else { return false };
    let Ok(nfkc_len) = usize::try_from(nfkc_query_len) else { return false };
    if (raw_len != 0 && raw_query.is_null()) || (nfkc_len != 0 && nfkc_query.is_null()) {
        return false;
    }
    let raw_slice = if raw_len == 0 { &[] } else {
        unsafe { std::slice::from_raw_parts(raw_query, raw_len) }
    };
    let nfkc_slice = if nfkc_len == 0 { &[] } else {
        unsafe { std::slice::from_raw_parts(nfkc_query, nfkc_len) }
    };

    let plan = sqlite_db::TermQueryPlan::new(raw_slice, nfkc_slice);
    let mut results = Vec::new();
    let Ok(query_res) = catch_unwind(std::panic::AssertUnwindSafe(|| {
        sqlite_db::query_terms(db, &plan, &mut results)
    })) else {
        return false;
    };
    if query_res.is_err() {
        return false;
    }

    let mut raw_items = Vec::with_capacity(results.len());
    for r in results {
        raw_items.push(DictTermRowC {
            expression: DictUtf16C::from_vec(r.expression),
            reading: DictUtf16C::from_vec(r.reading),
        });
    }
    let res = DictTermRowsC {
        items: raw_items.as_mut_ptr(),
        len: raw_items.len() as u64,
        capacity: raw_items.capacity() as u64,
    };
    std::mem::forget(raw_items);
    unsafe { out.write(res) };
    true
}

#[repr(C)]
pub struct DictKanjiRowC {
    pub dic_id: i64,
    pub onyomi: DictUtf16C,
    pub kunyomi: DictUtf16C,
    pub tags: DictUtf16C,
    pub meanings: DictStringListC,
    pub stats: DictUtf16C,
}

#[repr(C)]
pub struct DictKanjiRowsC {
    pub items: *mut DictKanjiRowC,
    pub len: u64,
    pub capacity: u64,
}

impl DictKanjiRowsC {
    fn empty() -> Self {
        Self { items: ptr::null_mut(), len: 0, capacity: 0 }
    }
}

/// # Safety
/// `out` must be writable and non-null or empty.
#[no_mangle]
pub unsafe extern "C" fn memento_dict_kanji_rows_free(out: *mut DictKanjiRowsC) {
    if out.is_null() { return; }
    let owned = std::mem::replace(unsafe { &mut *out }, DictKanjiRowsC::empty());
    if owned.items.is_null() { return; }
    let items = unsafe {
        Vec::from_raw_parts(owned.items, owned.len as usize, owned.capacity as usize)
    };
    for mut item in items {
        unsafe {
            item.onyomi.free();
            item.kunyomi.free();
            item.tags.free();
            memento_dict_string_list_free(&mut item.meanings);
            item.stats.free();
        }
    }
}

/// # Safety
/// `db` must be a valid open SQLite connection.
/// `kanji` must point to `kanji_len` UTF-16 units.
/// `out` must be writable. Caller frees with `memento_dict_kanji_rows_free`.
#[no_mangle]
pub unsafe extern "C" fn memento_dict_query_kanji(
    db: *mut sqlite_db::sqlite3,
    kanji: *const u16, kanji_len: u64,
    out: *mut DictKanjiRowsC,
) -> bool {
    if out.is_null() { return false; }
    unsafe { out.write(DictKanjiRowsC::empty()) };
    if db.is_null() { return false; }
    let Ok(len) = usize::try_from(kanji_len) else { return false };
    if len != 0 && kanji.is_null() { return false; }
    let slice = if len == 0 { &[] } else {
        unsafe { std::slice::from_raw_parts(kanji, len) }
    };

    let mut results = Vec::new();
    let Ok(query_res) = catch_unwind(std::panic::AssertUnwindSafe(|| {
        sqlite_db::query_kanji(db, slice, &mut results)
    })) else {
        return false;
    };
    if query_res.is_err() {
        return false;
    }

    let mut raw_items = Vec::with_capacity(results.len());
    for r in results {
        let meanings_list = string_list_from_vecs(r.meanings);
        raw_items.push(DictKanjiRowC {
            dic_id: r.dic_id,
            onyomi: DictUtf16C::from_vec(r.onyomi),
            kunyomi: DictUtf16C::from_vec(r.kunyomi),
            tags: DictUtf16C::from_vec(r.tags),
            meanings: meanings_list,
            stats: DictUtf16C::from_vec(r.stats),
        });
    }
    let res = DictKanjiRowsC {
        items: raw_items.as_mut_ptr(),
        len: raw_items.len() as u64,
        capacity: raw_items.capacity() as u64,
    };
    std::mem::forget(raw_items);
    unsafe { out.write(res) };
    true
}

#[repr(C)]
pub struct DictFreqRowC {
    pub dic_id: i64,
    pub frequency: DictUtf16C,
}

#[repr(C)]
pub struct DictFreqRowsC {
    pub items: *mut DictFreqRowC,
    pub len: u64,
    pub capacity: u64,
}

impl DictFreqRowsC {
    fn empty() -> Self {
        Self { items: ptr::null_mut(), len: 0, capacity: 0 }
    }
}

/// # Safety
/// `out` must be writable and non-null or empty.
#[no_mangle]
pub unsafe extern "C" fn memento_dict_freq_rows_free(out: *mut DictFreqRowsC) {
    if out.is_null() { return; }
    let owned = std::mem::replace(unsafe { &mut *out }, DictFreqRowsC::empty());
    if owned.items.is_null() { return; }
    let items = unsafe {
        Vec::from_raw_parts(owned.items, owned.len as usize, owned.capacity as usize)
    };
    for item in items {
        unsafe {
            item.frequency.free();
        }
    }
}

/// # Safety
/// `db` must be a valid open SQLite connection.
/// `custom_sql` can be null (default term_meta_bank query used).
/// `term` and `reading` must point to valid UTF-16 units (or null if len=0).
/// `out` must be writable. Caller frees with `memento_dict_freq_rows_free`.
#[no_mangle]
pub unsafe extern "C" fn memento_dict_get_frequencies(
    db: *mut sqlite_db::sqlite3,
    custom_sql: *const c_char,
    term: *const u16, term_len: u64,
    reading: *const u16, reading_len: u64,
    out: *mut DictFreqRowsC,
) -> bool {
    if out.is_null() { return false; }
    unsafe { out.write(DictFreqRowsC::empty()) };
    if db.is_null() { return false; }
    let Ok(t_len) = usize::try_from(term_len) else { return false };
    let Ok(r_len) = usize::try_from(reading_len) else { return false };
    if (t_len != 0 && term.is_null()) || (r_len != 0 && reading.is_null()) {
        return false;
    }
    let term_slice = if t_len == 0 { &[] } else {
        unsafe { std::slice::from_raw_parts(term, t_len) }
    };
    let reading_slice = if r_len == 0 { &[] } else {
        unsafe { std::slice::from_raw_parts(reading, r_len) }
    };
    let sql_opt = if custom_sql.is_null() {
        None
    } else {
        unsafe { CStr::from_ptr(custom_sql) }.to_str().ok()
    };

    let mut results = Vec::new();
    let Ok(query_res) = catch_unwind(std::panic::AssertUnwindSafe(|| {
        sqlite_db::get_frequencies_with_sql(db, sql_opt, term_slice, reading_slice, &mut results)
    })) else {
        return false;
    };
    if query_res.is_err() {
        return false;
    }

    let mut raw_items = Vec::with_capacity(results.len());
    for r in results {
        raw_items.push(DictFreqRowC {
            dic_id: r.dic_id,
            frequency: DictUtf16C::from_vec(r.frequency),
        });
    }
    let res = DictFreqRowsC {
        items: raw_items.as_mut_ptr(),
        len: raw_items.len() as u64,
        capacity: raw_items.capacity() as u64,
    };
    std::mem::forget(raw_items);
    unsafe { out.write(res) };
    true
}

#[repr(C)]
pub struct DictPitchRowC {
    pub dic_id: i64,
    pub mora: DictStringListC,
    pub positions: *mut i32,
    pub positions_len: u64,
    pub positions_capacity: u64,
}

#[repr(C)]
pub struct DictPitchRowsC {
    pub items: *mut DictPitchRowC,
    pub len: u64,
    pub capacity: u64,
}

impl DictPitchRowsC {
    fn empty() -> Self {
        Self { items: ptr::null_mut(), len: 0, capacity: 0 }
    }
}

/// # Safety
/// `out` must be writable and non-null or empty.
#[no_mangle]
pub unsafe extern "C" fn memento_dict_pitch_rows_free(out: *mut DictPitchRowsC) {
    if out.is_null() { return; }
    let owned = std::mem::replace(unsafe { &mut *out }, DictPitchRowsC::empty());
    if owned.items.is_null() { return; }
    let items = unsafe {
        Vec::from_raw_parts(owned.items, owned.len as usize, owned.capacity as usize)
    };
    for mut item in items {
        unsafe {
            memento_dict_string_list_free(&mut item.mora);
            if !item.positions.is_null() {
                drop(Vec::from_raw_parts(item.positions, item.positions_len as usize, item.positions_capacity as usize));
            }
        }
    }
}

/// # Safety
/// `db` must be a valid open SQLite connection.
/// `term` and `reading` must point to valid UTF-16 units (or null if len=0).
/// `out` must be writable. Caller frees with `memento_dict_pitch_rows_free`.
#[no_mangle]
pub unsafe extern "C" fn memento_dict_add_pitches(
    db: *mut sqlite_db::sqlite3,
    term: *const u16, term_len: u64,
    reading: *const u16, reading_len: u64,
    out: *mut DictPitchRowsC,
) -> bool {
    if out.is_null() { return false; }
    unsafe { out.write(DictPitchRowsC::empty()) };
    if db.is_null() { return false; }
    let Ok(t_len) = usize::try_from(term_len) else { return false };
    let Ok(r_len) = usize::try_from(reading_len) else { return false };
    if (t_len != 0 && term.is_null()) || (r_len != 0 && reading.is_null()) {
        return false;
    }
    let term_slice = if t_len == 0 { &[] } else {
        unsafe { std::slice::from_raw_parts(term, t_len) }
    };
    let reading_slice = if r_len == 0 { &[] } else {
        unsafe { std::slice::from_raw_parts(reading, r_len) }
    };

    let mut results = Vec::new();
    let Ok(query_res) = catch_unwind(std::panic::AssertUnwindSafe(|| {
        sqlite_db::add_pitches(db, term_slice, reading_slice, &mut results)
    })) else {
        return false;
    };
    if query_res.is_err() {
        return false;
    }

    let mut raw_items = Vec::with_capacity(results.len());
    for r in results {
        let mora_list = string_list_from_vecs(r.mora);
        let mut pos = r.positions;
        let pos_ptr = pos.as_mut_ptr();
        let pos_len = pos.len() as u64;
        let pos_cap = pos.capacity() as u64;
        std::mem::forget(pos);

        raw_items.push(DictPitchRowC {
            dic_id: r.dic_id,
            mora: mora_list,
            positions: pos_ptr,
            positions_len: pos_len,
            positions_capacity: pos_cap,
        });
    }
    let res = DictPitchRowsC {
        items: raw_items.as_mut_ptr(),
        len: raw_items.len() as u64,
        capacity: raw_items.capacity() as u64,
    };
    std::mem::forget(raw_items);
    unsafe { out.write(res) };
    true
}

#[repr(C)]
pub struct DictMetaC {
    pub id: i64,
    pub title: DictUtf16C,
    pub disabled: bool,
}

#[repr(C)]
pub struct DictTableCountsC {
    pub directory: i64,
    pub tag_bank: i64,
    pub term_bank: i64,
    pub term_meta_bank: i64,
    pub kanji_bank: i64,
    pub kanji_meta_bank: i64,
}

#[repr(C)]
pub struct DictCacheInfoC {
    pub dictionaries: *mut DictMetaC,
    pub dictionaries_len: u64,
    pub dictionaries_capacity: u64,
    pub table_counts: DictTableCountsC,
}

impl DictCacheInfoC {
    fn empty() -> Self {
        Self {
            dictionaries: ptr::null_mut(),
            dictionaries_len: 0,
            dictionaries_capacity: 0,
            table_counts: DictTableCountsC {
                directory: 0, tag_bank: 0, term_bank: 0, term_meta_bank: 0, kanji_bank: 0, kanji_meta_bank: 0,
            },
        }
    }
}

/// # Safety
/// `out` must be writable and non-null or empty.
#[no_mangle]
pub unsafe extern "C" fn memento_dict_cache_info_free(out: *mut DictCacheInfoC) {
    if out.is_null() { return; }
    let owned = std::mem::replace(unsafe { &mut *out }, DictCacheInfoC::empty());
    if owned.dictionaries.is_null() { return; }
    let items = unsafe {
        Vec::from_raw_parts(owned.dictionaries, owned.dictionaries_len as usize, owned.dictionaries_capacity as usize)
    };
    for item in items {
        unsafe {
            item.title.free();
        }
    }
}

/// # Safety
/// `db` must be a valid open SQLite connection.
/// `out` must be writable. Caller frees with `memento_dict_cache_info_free`.
#[no_mangle]
pub unsafe extern "C" fn memento_dict_init_cache(
    db: *mut sqlite_db::sqlite3,
    out: *mut DictCacheInfoC,
) -> bool {
    if out.is_null() { return false; }
    unsafe { out.write(DictCacheInfoC::empty()) };
    if db.is_null() { return false; }

    let Ok(cache_res) = catch_unwind(std::panic::AssertUnwindSafe(|| {
        sqlite_db::init_cache(db)
    })) else {
        return false;
    };
    let Ok(cache_info) = cache_res else {
        return false;
    };

    let mut dicts_raw = Vec::with_capacity(cache_info.dictionaries.len());
    for d in cache_info.dictionaries {
        dicts_raw.push(DictMetaC {
            id: d.id,
            title: DictUtf16C::from_vec(d.title.encode_utf16().collect()),
            disabled: d.disabled,
        });
    }
    let res = DictCacheInfoC {
        dictionaries: dicts_raw.as_mut_ptr(),
        dictionaries_len: dicts_raw.len() as u64,
        dictionaries_capacity: dicts_raw.capacity() as u64,
        table_counts: DictTableCountsC {
            directory: cache_info.table_counts.directory,
            tag_bank: cache_info.table_counts.tag_bank,
            term_bank: cache_info.table_counts.term_bank,
            term_meta_bank: cache_info.table_counts.term_meta_bank,
            kanji_bank: cache_info.table_counts.kanji_bank,
            kanji_meta_bank: cache_info.table_counts.kanji_meta_bank,
        },
    };
    std::mem::forget(dicts_raw);
    unsafe { out.write(res) };
    true
}

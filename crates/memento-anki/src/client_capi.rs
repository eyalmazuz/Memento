//! C ABI bindings for AnkiConfig and AnkiClient helpers.

use std::panic::catch_unwind;
use std::ptr;

use crate::client::{
    aggregate_context, build_browse_query, build_duplicate_query, check_ankiconnect_version,
    extract_fields_from_array_candidates, extract_fields_from_object_candidates, filter_json_strings,
    format_endpoint_url, interpret_can_add_notes, normalize_hostname, profile_lower_bound,
    validate_typed_result, FieldArrayCandidate, FieldObjectCandidate, JsonStringCandidate,
    ReplyMetadata, SubtitleCue, TypedReplyKind, TypedReplyStatus, MIN_ANKICONNECT_VERSION,
};

#[repr(C)]
pub struct MementoAnkiClientTextViewC
{
    pub data: *const u16,
    pub len: u64,
}

#[repr(C)]
pub struct MementoAnkiSubtitleCueC
{
    pub text: *const u16,
    pub text_len: u64,
    pub start: f64,
    pub end: f64,
}

#[repr(C)]
pub struct MementoAnkiFieldArrayCandidateC
{
    pub is_pair_of_strings: bool,
    pub name: MementoAnkiClientTextViewC,
    pub value: MementoAnkiClientTextViewC,
}

#[repr(C)]
pub struct MementoAnkiFieldObjectCandidateC
{
    pub is_string: bool,
    pub key: MementoAnkiClientTextViewC,
    pub value: MementoAnkiClientTextViewC,
}

#[repr(C)]
pub struct MementoAnkiJsonStringCandidateC
{
    pub is_string: bool,
    pub text: MementoAnkiClientTextViewC,
}

#[repr(C)]
pub struct MementoAnkiExtractedFieldC
{
    pub name: MementoAnkiClientTextViewC,
    pub value: MementoAnkiClientTextViewC,
}

#[repr(C)]
pub struct MementoAnkiReplyMetadataC
{
    pub is_null_doc: bool,
    pub is_object: bool,
    pub field_count: i32,
    pub has_error_key: bool,
    pub has_result_key: bool,
    pub error_is_null: bool,
    pub error_str: MementoAnkiClientTextViewC,
}

unsafe fn drop_boxed_slice<T>(data: *mut T, len: u64)
{
    if !data.is_null() && len > 0
    {
        drop(unsafe { Box::from_raw(ptr::slice_from_raw_parts_mut(data, len as usize)) });
    }
}

fn own_slice<T>(items: Vec<T>) -> (*mut T, u64)
{
    if items.is_empty()
    {
        return (ptr::null_mut(), 0);
    }
    let boxed = items.into_boxed_slice();
    let len = boxed.len() as u64;
    (Box::into_raw(boxed) as *mut T, len)
}

/// # Safety
/// `ptr` must be null or allocated by `memento-anki::client_capi` functions with matching `len`.
#[no_mangle]
pub unsafe extern "C" fn memento_anki_client_free_u16(ptr: *mut u16, len: u64)
{
    if !ptr.is_null() && len > 0
    {
        unsafe { drop_boxed_slice(ptr, len) };
    }
}

/// # Safety
/// If `len > 0`, `host` must point to `len` readable `u16`s.
/// `out` and `out_len` must be valid, non-null writable pointers.
#[no_mangle]
pub unsafe extern "C" fn memento_anki_normalize_hostname(
    host: *const u16,
    len: u64,
    out: *mut *mut u16,
    out_len: *mut u64,
) -> bool
{
    if out.is_null() || out_len.is_null()
    {
        return false;
    }
    unsafe {
        *out = ptr::null_mut();
        *out_len = 0;
    }
    if host.is_null() && len > 0
    {
        return false;
    }
    let Ok(Some((ptr, out_l))) = catch_unwind(|| {
        let slice = if len == 0
        {
            &[][..]
        }
        else
        {
            unsafe { std::slice::from_raw_parts(host, len as usize) }
        };
        let res = normalize_hostname(slice);
        Some(own_slice(res))
    }) else {
        return false;
    };
    unsafe {
        *out = ptr;
        *out_len = out_l;
    }
    true
}

/// # Safety
/// `names` must point to `names_len` valid `MementoAnkiClientTextViewC` structs if `names_len > 0`.
/// `target` must point to `target_len` readable `u16`s if `target_len > 0`.
/// `out_insert_index` and `out_exact_match` must be non-null writable pointers.
#[no_mangle]
pub unsafe extern "C" fn memento_anki_profile_lower_bound(
    names: *const MementoAnkiClientTextViewC,
    names_len: u64,
    target: *const u16,
    target_len: u64,
    out_insert_index: *mut u64,
    out_exact_match: *mut bool,
) -> bool
{
    if out_insert_index.is_null() || out_exact_match.is_null()
    {
        return false;
    }
    unsafe {
        *out_insert_index = 0;
        *out_exact_match = false;
    }
    if names.is_null() && names_len > 0
    {
        return false;
    }
    if target.is_null() && target_len > 0
    {
        return false;
    }
    let Ok(Some((insert_idx, exact))) = catch_unwind(|| {
        let target_slice = if target_len == 0
        {
            &[][..]
        }
        else
        {
            unsafe { std::slice::from_raw_parts(target, target_len as usize) }
        };
        let names_views = if names_len == 0
        {
            &[][..]
        }
        else
        {
            unsafe { std::slice::from_raw_parts(names, names_len as usize) }
        };
        let mut rust_names: Vec<&[u16]> = Vec::with_capacity(names_views.len());
        for view in names_views
        {
            if view.data.is_null() && view.len > 0
            {
                return None;
            }
            let s = if view.len == 0
            {
                &[][..]
            }
            else
            {
                unsafe { std::slice::from_raw_parts(view.data, view.len as usize) }
            };
            rust_names.push(s);
        }
        Some(profile_lower_bound(&rust_names, target_slice))
    }) else {
        return false;
    };
    unsafe {
        *out_insert_index = insert_idx as u64;
        *out_exact_match = exact;
    }
    true
}

/// # Safety
/// If `deck_len > 0`, `deck` must point to readable `u16`s.
/// If `query_len > 0`, `query` must point to readable `u16`s.
/// `out` and `out_len` must be non-null writable pointers.
#[no_mangle]
pub unsafe extern "C" fn memento_anki_build_browse_query(
    duplicate_policy: i32,
    deck: *const u16,
    deck_len: u64,
    query: *const u16,
    query_len: u64,
    out: *mut *mut u16,
    out_len: *mut u64,
) -> bool
{
    if out.is_null() || out_len.is_null()
    {
        return false;
    }
    unsafe {
        *out = ptr::null_mut();
        *out_len = 0;
    }
    if deck.is_null() && deck_len > 0
    {
        return false;
    }
    if query.is_null() && query_len > 0
    {
        return false;
    }
    let Ok(Some((ptr, out_l))) = catch_unwind(|| {
        let deck_slice = if deck_len == 0
        {
            &[][..]
        }
        else
        {
            unsafe { std::slice::from_raw_parts(deck, deck_len as usize) }
        };
        let query_slice = if query_len == 0
        {
            &[][..]
        }
        else
        {
            unsafe { std::slice::from_raw_parts(query, query_len as usize) }
        };
        let res = build_browse_query(duplicate_policy, deck_slice, query_slice);
        Some(own_slice(res))
    }) else {
        return false;
    };
    unsafe {
        *out = ptr;
        *out_len = out_l;
    }
    true
}

/// # Safety
/// If `field_key_len > 0`, `field_key` must point to readable `u16`s.
/// If `first_val_len > 0`, `first_val` must point to readable `u16`s.
/// If `has_second_val && second_val_len > 0`, `second_val` must point to readable `u16`s.
/// `out` and `out_len` must be non-null writable pointers.
#[no_mangle]
pub unsafe extern "C" fn memento_anki_build_duplicate_query(
    field_key: *const u16,
    field_key_len: u64,
    first_val: *const u16,
    first_val_len: u64,
    has_second_val: bool,
    second_val: *const u16,
    second_val_len: u64,
    out: *mut *mut u16,
    out_len: *mut u64,
) -> bool
{
    if out.is_null() || out_len.is_null()
    {
        return false;
    }
    unsafe {
        *out = ptr::null_mut();
        *out_len = 0;
    }
    if field_key.is_null() && field_key_len > 0
    {
        return false;
    }
    if first_val.is_null() && first_val_len > 0
    {
        return false;
    }
    if has_second_val && second_val.is_null() && second_val_len > 0
    {
        return false;
    }
    let Ok(Some((ptr, out_l))) = catch_unwind(|| {
        let field_key_slice = if field_key_len == 0
        {
            &[][..]
        }
        else
        {
            unsafe { std::slice::from_raw_parts(field_key, field_key_len as usize) }
        };
        let first_val_slice = if first_val_len == 0
        {
            &[][..]
        }
        else
        {
            unsafe { std::slice::from_raw_parts(first_val, first_val_len as usize) }
        };
        let second_val_slice = if !has_second_val || second_val_len == 0
        {
            &[][..]
        }
        else
        {
            unsafe { std::slice::from_raw_parts(second_val, second_val_len as usize) }
        };
        let res = build_duplicate_query(
            field_key_slice,
            first_val_slice,
            has_second_val,
            second_val_slice,
        );
        Some(own_slice(res))
    }) else {
        return false;
    };
    unsafe {
        *out = ptr;
        *out_len = out_l;
    }
    true
}

/// # Safety
/// If `items_len > 0`, `items` must point to readable `MementoAnkiSubtitleCueC`s.
/// If `selected_rows_len > 0`, `selected_rows` must point to readable `i32`s.
/// `out_text` and `out_text_len` must be non-null writable pointers.
/// `out_raw_start` and `out_raw_end` may be null, or if non-null must be writable.
#[no_mangle]
pub unsafe extern "C" fn memento_anki_aggregate_context(
    items: *const MementoAnkiSubtitleCueC,
    items_len: u64,
    selected_rows: *const i32,
    selected_rows_len: u64,
    out_raw_start: *mut f64,
    out_raw_end: *mut f64,
    out_text: *mut *mut u16,
    out_text_len: *mut u64,
) -> bool
{
    if out_text.is_null() || out_text_len.is_null()
    {
        return false;
    }
    unsafe {
        *out_text = ptr::null_mut();
        *out_text_len = 0;
    }
    if !out_raw_start.is_null()
    {
        unsafe { *out_raw_start = 0.0 };
    }
    if !out_raw_end.is_null()
    {
        unsafe { *out_raw_end = 0.0 };
    }
    if items.is_null() && items_len > 0
    {
        return false;
    }
    if selected_rows.is_null() && selected_rows_len > 0
    {
        return false;
    }

    let Ok(res) = catch_unwind(|| {
        let items_slice = if items_len == 0
        {
            &[][..]
        }
        else
        {
            unsafe { std::slice::from_raw_parts(items, items_len as usize) }
        };
        let mut rust_items = Vec::with_capacity(items_slice.len());
        for item in items_slice
        {
            if item.text.is_null() && item.text_len > 0
            {
                return None;
            }
            let text = if item.text_len == 0
            {
                &[][..]
            }
            else
            {
                unsafe { std::slice::from_raw_parts(item.text, item.text_len as usize) }
            };
            rust_items.push(SubtitleCue {
                text,
                start: item.start,
                end: item.end,
            });
        }
        let rows = if selected_rows_len == 0
        {
            &[][..]
        }
        else
        {
            unsafe { std::slice::from_raw_parts(selected_rows, selected_rows_len as usize) }
        };
        match aggregate_context(&rust_items, rows)
        {
            Ok(opt) => Some(opt),
            Err(()) => None,
        }
    }) else {
        return false;
    };

    let Some(aggregated_opt) = res else {
        return false;
    };

    if let Some(agg) = aggregated_opt
    {
        if !out_raw_start.is_null()
        {
            unsafe { *out_raw_start = agg.raw_start };
        }
        if !out_raw_end.is_null()
        {
            unsafe { *out_raw_end = agg.raw_end };
        }
        let (ptr, len) = own_slice(agg.text);
        unsafe {
            *out_text = ptr;
            *out_text_len = len;
        }
    }
    true
}

/// # Safety
/// If `candidates_len > 0`, `candidates` must point to readable `MementoAnkiFieldArrayCandidateC`s.
/// `out` and `out_len` must be non-null writable pointers.
#[no_mangle]
pub unsafe extern "C" fn memento_anki_extract_fields_from_array(
    candidates: *const MementoAnkiFieldArrayCandidateC,
    candidates_len: u64,
    out: *mut *mut MementoAnkiExtractedFieldC,
    out_len: *mut u64,
) -> bool
{
    if out.is_null() || out_len.is_null()
    {
        return false;
    }
    unsafe {
        *out = ptr::null_mut();
        *out_len = 0;
    }
    if candidates.is_null() && candidates_len > 0
    {
        return false;
    }
    let Ok(Some((ptr, len))) = catch_unwind(|| {
        let raw_candidates = if candidates_len == 0
        {
            &[][..]
        }
        else
        {
            unsafe { std::slice::from_raw_parts(candidates, candidates_len as usize) }
        };

        let mut rust_candidates: Vec<FieldArrayCandidate<'_>> =
            Vec::with_capacity(raw_candidates.len());
        for c in raw_candidates
        {
            if c.is_pair_of_strings
            {
                if (c.name.data.is_null() && c.name.len > 0)
                    || (c.value.data.is_null() && c.value.len > 0)
                {
                    return None;
                }
                let name = if c.name.len == 0
                {
                    &[][..]
                }
                else
                {
                    unsafe { std::slice::from_raw_parts(c.name.data, c.name.len as usize) }
                };
                let value = if c.value.len == 0
                {
                    &[][..]
                }
                else
                {
                    unsafe { std::slice::from_raw_parts(c.value.data, c.value.len as usize) }
                };
                rust_candidates.push(FieldArrayCandidate::Pair { name, value });
            }
            else
            {
                rust_candidates.push(FieldArrayCandidate::Invalid);
            }
        }

        let extracted = extract_fields_from_array_candidates(&rust_candidates);
        let c_extracted: Vec<MementoAnkiExtractedFieldC> = extracted
            .into_iter()
            .map(|f| MementoAnkiExtractedFieldC {
                name: MementoAnkiClientTextViewC {
                    data: f.name.as_ptr(),
                    len: f.name.len() as u64,
                },
                value: MementoAnkiClientTextViewC {
                    data: f.value.as_ptr(),
                    len: f.value.len() as u64,
                },
            })
            .collect();
        Some(own_slice(c_extracted))
    }) else {
        return false;
    };

    unsafe {
        *out = ptr;
        *out_len = len;
    }
    true
}

/// # Safety
/// If `candidates_len > 0`, `candidates` must point to readable `MementoAnkiFieldObjectCandidateC`s.
/// `out` and `out_len` must be non-null writable pointers.
#[no_mangle]
pub unsafe extern "C" fn memento_anki_extract_fields_from_object(
    candidates: *const MementoAnkiFieldObjectCandidateC,
    candidates_len: u64,
    out: *mut *mut MementoAnkiExtractedFieldC,
    out_len: *mut u64,
) -> bool
{
    if out.is_null() || out_len.is_null()
    {
        return false;
    }
    unsafe {
        *out = ptr::null_mut();
        *out_len = 0;
    }
    if candidates.is_null() && candidates_len > 0
    {
        return false;
    }
    let Ok(Some((ptr, len))) = catch_unwind(|| {
        let raw_candidates = if candidates_len == 0
        {
            &[][..]
        }
        else
        {
            unsafe { std::slice::from_raw_parts(candidates, candidates_len as usize) }
        };

        let mut rust_candidates: Vec<FieldObjectCandidate<'_>> =
            Vec::with_capacity(raw_candidates.len());
        for c in raw_candidates
        {
            if c.is_string
            {
                if (c.key.data.is_null() && c.key.len > 0)
                    || (c.value.data.is_null() && c.value.len > 0)
                {
                    return None;
                }
                let key = if c.key.len == 0
                {
                    &[][..]
                }
                else
                {
                    unsafe { std::slice::from_raw_parts(c.key.data, c.key.len as usize) }
                };
                let value = if c.value.len == 0
                {
                    &[][..]
                }
                else
                {
                    unsafe { std::slice::from_raw_parts(c.value.data, c.value.len as usize) }
                };
                rust_candidates.push(FieldObjectCandidate::Entry { key, value });
            }
            else
            {
                rust_candidates.push(FieldObjectCandidate::NonString);
            }
        }

        let extracted = extract_fields_from_object_candidates(&rust_candidates);
        let c_extracted: Vec<MementoAnkiExtractedFieldC> = extracted
            .into_iter()
            .map(|f| MementoAnkiExtractedFieldC {
                name: MementoAnkiClientTextViewC {
                    data: f.name.as_ptr(),
                    len: f.name.len() as u64,
                },
                value: MementoAnkiClientTextViewC {
                    data: f.value.as_ptr(),
                    len: f.value.len() as u64,
                },
            })
            .collect();
        Some(own_slice(c_extracted))
    }) else {
        return false;
    };

    unsafe {
        *out = ptr;
        *out_len = len;
    }
    true
}

/// # Safety
/// `ptr` must be null or allocated by `memento_anki_extract_fields_*` with matching `len`.
#[no_mangle]
pub unsafe extern "C" fn memento_anki_free_extracted_fields(
    ptr: *mut MementoAnkiExtractedFieldC,
    len: u64,
)
{
    if !ptr.is_null() && len > 0
    {
        unsafe { drop_boxed_slice(ptr, len) };
    }
}

/// # Safety
/// If `metadata` is non-null, `metadata.error_str.data` must point to readable `u16`s
/// if `metadata.error_str.len > 0`.
#[no_mangle]
pub unsafe extern "C" fn memento_anki_validate_reply(
    metadata: *const MementoAnkiReplyMetadataC,
) -> i32
{
    if metadata.is_null()
    {
        return 1; // NotJson
    }
    let Ok(status) = catch_unwind(|| {
        let meta_ref = unsafe { &*metadata };
        let error_slice = if meta_ref.error_str.len == 0 || meta_ref.error_str.data.is_null()
        {
            &[][..]
        }
        else
        {
            unsafe {
                std::slice::from_raw_parts(meta_ref.error_str.data, meta_ref.error_str.len as usize)
            }
        };
        let rust_meta = ReplyMetadata {
            is_null_doc: meta_ref.is_null_doc,
            is_object: meta_ref.is_object,
            field_count: meta_ref.field_count,
            has_error_key: meta_ref.has_error_key,
            has_result_key: meta_ref.has_result_key,
            error_is_null: meta_ref.error_is_null,
            error_str: error_slice,
        };
        crate::client::validate_reply_metadata(&rust_meta).to_c_status()
    }) else {
        return 1; // NotJson on unwind
    };
    status
}

/// # Safety
/// `out_min_version` may be null, or if non-null must be a writable pointer.
#[no_mangle]
pub unsafe extern "C" fn memento_anki_check_version(
    result_is_double: bool,
    version_int: i32,
    out_min_version: *mut i32,
) -> i32
{
    if !out_min_version.is_null()
    {
        unsafe { *out_min_version = MIN_ANKICONNECT_VERSION };
    }
    check_ankiconnect_version(result_is_double, version_int).to_c_status()
}

/// # Safety
/// If `hostname_len > 0`, `hostname` must point to readable `u16`s.
/// If `port_len > 0`, `port` must point to readable `u16`s.
/// `out` and `out_len` must be non-null writable pointers.
#[no_mangle]
pub unsafe extern "C" fn memento_anki_format_endpoint_url(
    hostname: *const u16,
    hostname_len: u64,
    port: *const u16,
    port_len: u64,
    out: *mut *mut u16,
    out_len: *mut u64,
) -> bool
{
    if out.is_null() || out_len.is_null()
    {
        return false;
    }
    unsafe {
        *out = ptr::null_mut();
        *out_len = 0;
    }
    if hostname.is_null() && hostname_len > 0
    {
        return false;
    }
    if port.is_null() && port_len > 0
    {
        return false;
    }
    let Ok(Some((ptr, out_l))) = catch_unwind(|| {
        let host_slice = if hostname_len == 0
        {
            &[][..]
        }
        else
        {
            unsafe { std::slice::from_raw_parts(hostname, hostname_len as usize) }
        };
        let port_slice = if port_len == 0
        {
            &[][..]
        }
        else
        {
            unsafe { std::slice::from_raw_parts(port, port_len as usize) }
        };
        let res = format_endpoint_url(host_slice, port_slice);
        Some(own_slice(res))
    }) else {
        return false;
    };
    unsafe {
        *out = ptr;
        *out_len = out_l;
    }
    true
}

/// # Safety
/// If `len > 0`, `flags` must point to `len` readable `bool`s.
/// `out_first` and `out_second` may be null, or if non-null must be writable pointers.
#[no_mangle]
pub unsafe extern "C" fn memento_anki_interpret_can_add_notes(
    flags: *const bool,
    len: u64,
    out_first: *mut bool,
    out_second: *mut bool,
) -> bool
{
    if flags.is_null() && len > 0
    {
        return false;
    }
    let Ok(res) = catch_unwind(|| {
        let flags_slice = if len == 0
        {
            &[][..]
        }
        else
        {
            unsafe { std::slice::from_raw_parts(flags, len as usize) }
        };
        interpret_can_add_notes(flags_slice)
    }) else {
        return false;
    };
    if !out_first.is_null()
    {
        unsafe { *out_first = res.first };
    }
    if !out_second.is_null()
    {
        unsafe { *out_second = res.second };
    }
    true
}

#[no_mangle]
pub extern "C" fn memento_anki_validate_typed_result(
    kind: i32,
    has_reply_error: bool,
    reply_obj_empty: bool,
    result_is_array: bool,
    result_is_double: bool,
    all_elements_match_type: bool,
) -> i32
{
    if has_reply_error
    {
        return TypedReplyStatus::ReplyError.to_c_status();
    }
    let Some(k) = TypedReplyKind::from_c_kind(kind) else {
        return TypedReplyStatus::InvalidKind.to_c_status();
    };
    validate_typed_result(
        k,
        has_reply_error,
        reply_obj_empty,
        result_is_array,
        result_is_double,
        all_elements_match_type,
    )
    .to_c_status()
}

/// # Safety
/// If `candidates_len > 0`, `candidates` must point to readable `MementoAnkiJsonStringCandidateC`s.
/// `out` and `out_len` must be non-null writable pointers.
#[no_mangle]
pub unsafe extern "C" fn memento_anki_filter_json_strings(
    candidates: *const MementoAnkiJsonStringCandidateC,
    candidates_len: u64,
    out: *mut *mut MementoAnkiClientTextViewC,
    out_len: *mut u64,
) -> bool
{
    if out.is_null() || out_len.is_null()
    {
        return false;
    }
    unsafe {
        *out = ptr::null_mut();
        *out_len = 0;
    }
    if candidates.is_null() && candidates_len > 0
    {
        return false;
    }
    let Ok(Some((ptr, len))) = catch_unwind(|| {
        let raw_candidates = if candidates_len == 0
        {
            &[][..]
        }
        else
        {
            unsafe { std::slice::from_raw_parts(candidates, candidates_len as usize) }
        };

        let mut rust_candidates = Vec::with_capacity(raw_candidates.len());
        for c in raw_candidates
        {
            if c.is_string
            {
                if c.text.data.is_null() && c.text.len > 0
                {
                    return None;
                }
                let text = if c.text.len == 0
                {
                    &[][..]
                }
                else
                {
                    unsafe { std::slice::from_raw_parts(c.text.data, c.text.len as usize) }
                };
                rust_candidates.push(JsonStringCandidate {
                    is_string: true,
                    text,
                });
            }
            else
            {
                rust_candidates.push(JsonStringCandidate {
                    is_string: false,
                    text: &[][..],
                });
            }
        }

        let filtered = filter_json_strings(&rust_candidates);
        let c_views: Vec<MementoAnkiClientTextViewC> = filtered
            .into_iter()
            .map(|s| MementoAnkiClientTextViewC {
                data: s.as_ptr(),
                len: s.len() as u64,
            })
            .collect();
        Some(own_slice(c_views))
    }) else {
        return false;
    };

    unsafe {
        *out = ptr;
        *out_len = len;
    }
    true
}

/// # Safety
/// `ptr` must be null or allocated by `memento_anki_filter_json_strings` with matching `len`.
#[no_mangle]
pub unsafe extern "C" fn memento_anki_free_u16_list(
    ptr: *mut MementoAnkiClientTextViewC,
    len: u64,
)
{
    if !ptr.is_null() && len > 0
    {
        unsafe { drop_boxed_slice(ptr, len) };
    }
}

#[cfg(test)]
mod tests
{
    use super::*;

    fn to_u16(s: &str) -> Vec<u16>
    {
        s.encode_utf16().collect()
    }

    fn from_u16_slice(ptr: *const u16, len: u64) -> String
    {
        let slice = unsafe { std::slice::from_raw_parts(ptr, len as usize) };
        String::from_utf16_lossy(slice)
    }

    #[test]
    fn test_capi_normalize_hostname()
    {
        let host = to_u16("127.0.0.1:8765");
        let mut out: *mut u16 = ptr::null_mut();
        let mut out_len: u64 = 0;

        // Valid call
        assert!(unsafe {
            memento_anki_normalize_hostname(
                host.as_ptr(),
                host.len() as u64,
                &mut out,
                &mut out_len,
            )
        });
        assert_eq!(from_u16_slice(out, out_len), "http://127.0.0.1:8765");
        unsafe { memento_anki_client_free_u16(out, out_len) };

        // Null out pointers rejected
        assert!(!unsafe {
            memento_anki_normalize_hostname(
                host.as_ptr(),
                host.len() as u64,
                ptr::null_mut(),
                &mut out_len,
            )
        });
    }

    #[test]
    fn test_capi_profile_lower_bound()
    {
        let a = to_u16("Alpha");
        let b = to_u16("Beta");
        let views = [
            MementoAnkiClientTextViewC { data: a.as_ptr(), len: a.len() as u64 },
            MementoAnkiClientTextViewC { data: b.as_ptr(), len: b.len() as u64 },
        ];

        let target = to_u16("Beta");
        let mut idx: u64 = 0;
        let mut exact: bool = false;
        assert!(unsafe {
            memento_anki_profile_lower_bound(
                views.as_ptr(),
                views.len() as u64,
                target.as_ptr(),
                target.len() as u64,
                &mut idx,
                &mut exact,
            )
        });
        assert_eq!(idx, 1);
        assert!(exact);

        // Null out pointer rejected
        assert!(!unsafe {
            memento_anki_profile_lower_bound(
                views.as_ptr(),
                views.len() as u64,
                target.as_ptr(),
                target.len() as u64,
                ptr::null_mut(),
                &mut exact,
            )
        });
    }

    #[test]
    fn test_capi_build_browse_query()
    {
        let deck = to_u16("Deck");
        let query = to_u16("tag:marked");
        let mut out: *mut u16 = ptr::null_mut();
        let mut out_len: u64 = 0;

        assert!(unsafe {
            memento_anki_build_browse_query(
                1,
                deck.as_ptr(),
                deck.len() as u64,
                query.as_ptr(),
                query.len() as u64,
                &mut out,
                &mut out_len,
            )
        });
        assert_eq!(from_u16_slice(out, out_len), "\"deck:Deck\" (tag:marked)");
        unsafe { memento_anki_client_free_u16(out, out_len) };
    }

    #[test]
    fn test_capi_build_duplicate_query()
    {
        let key = to_u16("Expression");
        let val1 = to_u16("test\\\"val");
        let mut out: *mut u16 = ptr::null_mut();
        let mut out_len: u64 = 0;

        assert!(unsafe {
            memento_anki_build_duplicate_query(
                key.as_ptr(),
                key.len() as u64,
                val1.as_ptr(),
                val1.len() as u64,
                false,
                ptr::null(),
                0,
                &mut out,
                &mut out_len,
            )
        });
        assert_eq!(from_u16_slice(out, out_len), "Expression:\"test\\\\\\\"val\"");
        unsafe { memento_anki_client_free_u16(out, out_len) };
    }

    #[test]
    fn test_capi_aggregate_context()
    {
        let t0 = to_u16("");
        let t1 = to_u16("Sub1");
        let t2 = to_u16("Sub2");
        let cues = [
            MementoAnkiSubtitleCueC { text: t0.as_ptr(), text_len: t0.len() as u64, start: 0.0, end: 1.0 },
            MementoAnkiSubtitleCueC { text: t1.as_ptr(), text_len: t1.len() as u64, start: 1.5, end: 3.0 },
            MementoAnkiSubtitleCueC { text: t2.as_ptr(), text_len: t2.len() as u64, start: 3.5, end: 5.0 },
        ];
        let rows = [2i32, 0, 1];

        let mut start: f64 = 0.0;
        let mut end: f64 = 0.0;
        let mut out: *mut u16 = ptr::null_mut();
        let mut out_len: u64 = 0;

        assert!(unsafe {
            memento_anki_aggregate_context(
                cues.as_ptr(),
                cues.len() as u64,
                rows.as_ptr(),
                rows.len() as u64,
                &mut start,
                &mut end,
                &mut out,
                &mut out_len,
            )
        });
        assert_eq!(start, 0.0); // row 0's start even though text is empty
        assert_eq!(end, 5.0);
        assert_eq!(from_u16_slice(out, out_len), "Sub1\nSub2");
        unsafe { memento_anki_client_free_u16(out, out_len) };

        // Test with null start and end pointers (secondary context)
        let mut out2: *mut u16 = ptr::null_mut();
        let mut out2_len: u64 = 0;
        assert!(unsafe {
            memento_anki_aggregate_context(
                cues.as_ptr(),
                cues.len() as u64,
                rows.as_ptr(),
                rows.len() as u64,
                ptr::null_mut(),
                ptr::null_mut(),
                &mut out2,
                &mut out2_len,
            )
        });
        assert_eq!(from_u16_slice(out2, out2_len), "Sub1\nSub2");
        unsafe { memento_anki_client_free_u16(out2, out2_len) };
    }

    #[test]
    fn test_capi_extract_fields_array()
    {
        let n1 = to_u16("Name1");
        let v1 = to_u16("Val1");
        let candidates = [
            MementoAnkiFieldArrayCandidateC {
                is_pair_of_strings: false,
                name: MementoAnkiClientTextViewC { data: ptr::null(), len: 0 },
                value: MementoAnkiClientTextViewC { data: ptr::null(), len: 0 },
            },
            MementoAnkiFieldArrayCandidateC {
                is_pair_of_strings: true,
                name: MementoAnkiClientTextViewC { data: n1.as_ptr(), len: n1.len() as u64 },
                value: MementoAnkiClientTextViewC { data: v1.as_ptr(), len: v1.len() as u64 },
            },
        ];

        let mut out: *mut MementoAnkiExtractedFieldC = ptr::null_mut();
        let mut out_len: u64 = 0;
        assert!(unsafe {
            memento_anki_extract_fields_from_array(
                candidates.as_ptr(),
                candidates.len() as u64,
                &mut out,
                &mut out_len,
            )
        });
        assert_eq!(out_len, 1);
        let slice = unsafe { std::slice::from_raw_parts(out, out_len as usize) };
        assert_eq!(from_u16_slice(slice[0].name.data, slice[0].name.len), "Name1");
        assert_eq!(from_u16_slice(slice[0].value.data, slice[0].value.len), "Val1");
        unsafe { memento_anki_free_extracted_fields(out, out_len) };
    }

    #[test]
    fn test_capi_extract_fields_object()
    {
        let k1 = to_u16("Key1");
        let v1 = to_u16("Val1");
        let candidates = [
            MementoAnkiFieldObjectCandidateC {
                is_string: true,
                key: MementoAnkiClientTextViewC { data: k1.as_ptr(), len: k1.len() as u64 },
                value: MementoAnkiClientTextViewC { data: v1.as_ptr(), len: v1.len() as u64 },
            },
            MementoAnkiFieldObjectCandidateC {
                is_string: false,
                key: MementoAnkiClientTextViewC { data: k1.as_ptr(), len: k1.len() as u64 },
                value: MementoAnkiClientTextViewC { data: ptr::null(), len: 0 },
            },
        ];

        let mut out: *mut MementoAnkiExtractedFieldC = ptr::null_mut();
        let mut out_len: u64 = 0;
        assert!(unsafe {
            memento_anki_extract_fields_from_object(
                candidates.as_ptr(),
                candidates.len() as u64,
                &mut out,
                &mut out_len,
            )
        });
        assert_eq!(out_len, 1);
        let slice = unsafe { std::slice::from_raw_parts(out, out_len as usize) };
        assert_eq!(from_u16_slice(slice[0].name.data, slice[0].name.len), "Key1");
        assert_eq!(from_u16_slice(slice[0].value.data, slice[0].value.len), "Val1");
        unsafe { memento_anki_free_extracted_fields(out, out_len) };
    }

    #[test]
    fn test_capi_validate_reply()
    {
        // Null metadata -> 1 (NotJson)
        assert_eq!(unsafe { memento_anki_validate_reply(ptr::null()) }, 1);

        let err_text = to_u16("error string");
        let meta = MementoAnkiReplyMetadataC {
            is_null_doc: false,
            is_object: true,
            field_count: 2,
            has_error_key: true,
            has_result_key: true,
            error_is_null: false,
            error_str: MementoAnkiClientTextViewC {
                data: err_text.as_ptr(),
                len: err_text.len() as u64,
            },
        };
        assert_eq!(unsafe { memento_anki_validate_reply(&meta) }, 6); // HasErrorString

        let meta_ok = MementoAnkiReplyMetadataC {
            is_null_doc: false,
            is_object: true,
            field_count: 2,
            has_error_key: true,
            has_result_key: true,
            error_is_null: true,
            error_str: MementoAnkiClientTextViewC { data: ptr::null(), len: 0 },
        };
        assert_eq!(unsafe { memento_anki_validate_reply(&meta_ok) }, 0); // Ok
    }

    #[test]
    fn test_capi_check_version()
    {
        let mut min_ver: i32 = 0;
        assert_eq!(unsafe { memento_anki_check_version(false, 6, &mut min_ver) }, 1);
        assert_eq!(min_ver, 6);

        min_ver = 0;
        assert_eq!(unsafe { memento_anki_check_version(true, 5, &mut min_ver) }, 2);
        assert_eq!(min_ver, 6);

        min_ver = 0;
        assert_eq!(unsafe { memento_anki_check_version(true, 6, &mut min_ver) }, 0);
        assert_eq!(min_ver, 6);

        // Null out_min_version is safe
        assert_eq!(unsafe { memento_anki_check_version(true, 6, ptr::null_mut()) }, 0);
    }

    #[test]
    fn test_capi_format_endpoint_url()
    {
        let host = to_u16("http://127.0.0.1");
        let port = to_u16("8765");
        let mut out: *mut u16 = ptr::null_mut();
        let mut out_len: u64 = 0;

        assert!(unsafe {
            memento_anki_format_endpoint_url(
                host.as_ptr(),
                host.len() as u64,
                port.as_ptr(),
                port.len() as u64,
                &mut out,
                &mut out_len,
            )
        });
        assert_eq!(from_u16_slice(out, out_len), "http://127.0.0.1:8765");
        unsafe { memento_anki_client_free_u16(out, out_len) };

        // Null out rejected
        assert!(!unsafe {
            memento_anki_format_endpoint_url(
                host.as_ptr(),
                host.len() as u64,
                port.as_ptr(),
                port.len() as u64,
                ptr::null_mut(),
                &mut out_len,
            )
        });
    }

    #[test]
    fn test_capi_interpret_can_add_notes()
    {
        let flags = [true, false];
        let mut first: bool = false;
        let mut second: bool = true;

        assert!(unsafe {
            memento_anki_interpret_can_add_notes(flags.as_ptr(), flags.len() as u64, &mut first, &mut second)
        });
        assert!(first);
        assert!(!second);

        // Null flags with len > 0 rejected
        assert!(!unsafe {
            memento_anki_interpret_can_add_notes(ptr::null(), 2, &mut first, &mut second)
        });

        // Null pointers for outputs handled safely
        assert!(unsafe {
            memento_anki_interpret_can_add_notes(flags.as_ptr(), flags.len() as u64, ptr::null_mut(), ptr::null_mut())
        });
    }

    #[test]
    fn test_capi_validate_typed_result()
    {
        // AddMedia
        assert_eq!(
            memento_anki_validate_typed_result(0, false, false, true, false, true),
            0 // Success
        );
        assert_eq!(
            memento_anki_validate_typed_result(0, false, false, true, false, false),
            4 // ElementTypeMismatch
        );

        // OpenBrowse
        assert_eq!(
            memento_anki_validate_typed_result(1, false, true, false, false, false),
            1 // EmptySuccess
        );

        // RequestStringList (element mismatch ignored)
        assert_eq!(
            memento_anki_validate_typed_result(2, false, false, true, false, false),
            0 // Success
        );

        // RequestBoolList
        assert_eq!(
            memento_anki_validate_typed_result(3, false, false, true, false, false),
            4 // ElementTypeMismatch
        );

        // RequestInt
        assert_eq!(
            memento_anki_validate_typed_result(4, false, true, false, false, false),
            5 // EmptyError
        );
        assert_eq!(
            memento_anki_validate_typed_result(4, false, false, false, false, true),
            6 // ResultNotDouble
        );
        assert_eq!(
            memento_anki_validate_typed_result(4, false, false, false, true, true),
            0 // Success
        );

        // Invalid kind
        assert_eq!(
            memento_anki_validate_typed_result(99, false, false, true, false, true),
            7 // InvalidKind
        );
    }

    #[test]
    fn test_capi_filter_json_strings()
    {
        let t1 = to_u16("Tag1");
        let t2 = to_u16("Tag2");
        let candidates = [
            MementoAnkiJsonStringCandidateC {
                is_string: true,
                text: MementoAnkiClientTextViewC { data: t1.as_ptr(), len: t1.len() as u64 },
            },
            MementoAnkiJsonStringCandidateC {
                is_string: false,
                text: MementoAnkiClientTextViewC { data: ptr::null(), len: 0 },
            },
            MementoAnkiJsonStringCandidateC {
                is_string: true,
                text: MementoAnkiClientTextViewC { data: t2.as_ptr(), len: t2.len() as u64 },
            },
        ];

        let mut out: *mut MementoAnkiClientTextViewC = ptr::null_mut();
        let mut out_len: u64 = 0;

        assert!(unsafe {
            memento_anki_filter_json_strings(candidates.as_ptr(), candidates.len() as u64, &mut out, &mut out_len)
        });
        assert_eq!(out_len, 2);
        let slice = unsafe { std::slice::from_raw_parts(out, out_len as usize) };
        assert_eq!(from_u16_slice(slice[0].data, slice[0].len), "Tag1");
        assert_eq!(from_u16_slice(slice[1].data, slice[1].len), "Tag2");

        unsafe { memento_anki_free_u16_list(out, out_len) };
    }
}

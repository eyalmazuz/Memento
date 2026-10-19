//! UTF-16 C ABI consumed by the Qt subtitle parser and model shells. All returned
//! buffers are Rust-owned; nested text and the entry array are released by
//! paired free calls, including when C++ reconstruction throws.

use std::panic::catch_unwind;
use std::ptr;

use crate::{parse_subtitles, Format};

#[repr(C)]
pub struct SubtitleEntryC {
    pub text: *mut u16,
    pub text_len: u64,
    pub text_capacity: u64,
    pub start: f64,
    pub end: f64,
}

#[repr(C)]
pub struct SubtitleEntriesC {
    pub entries: *mut SubtitleEntryC,
    pub len: u64,
    pub capacity: u64,
}

impl SubtitleEntriesC {
    fn empty() -> Self {
        Self { entries: ptr::null_mut(), len: 0, capacity: 0 }
    }
}

/// # Safety
/// `text` must be null with zero length or a readable `len`-unit UTF-16
/// buffer. `out` must be writable and not alias the text. On success the
/// returned collection belongs to Rust until `memento_subtitle_free`.
#[no_mangle]
pub unsafe extern "C" fn memento_subtitle_parse(
    format: u32,
    text: *const u16,
    len: u64,
    out: *mut SubtitleEntriesC,
) -> bool {
    if out.is_null() {
        return false;
    }
    unsafe { out.write(SubtitleEntriesC::empty()) };
    let format = match format {
        0 => Format::Ass,
        1 => Format::Srt,
        2 => Format::Vtt,
        _ => return false,
    };
    let Ok(length) = usize::try_from(len) else { return false };
    if length > isize::MAX as usize / std::mem::size_of::<u16>() ||
        (length != 0 && text.is_null()) {
        return false;
    }
    let source = if length == 0 { &[] } else {
        unsafe { std::slice::from_raw_parts(text, length) }
    };
    let Ok(Ok(entries)) = catch_unwind(|| parse_subtitles(format, source)) else {
        return false;
    };
    if entries.is_empty() {
        return true;
    }
    let mut raw = Vec::with_capacity(entries.len());
    for entry in entries {
        let mut content = entry.text;
        let (text, text_len, text_capacity) = if content.is_empty() {
            (ptr::null_mut(), 0, 0)
        } else {
            let values = (content.as_mut_ptr(), content.len() as u64, content.capacity() as u64);
            std::mem::forget(content);
            values
        };
        raw.push(SubtitleEntryC {
            text, text_len, text_capacity, start: entry.start, end: entry.end,
        });
    }
    let result = SubtitleEntriesC {
        entries: raw.as_mut_ptr(), len: raw.len() as u64, capacity: raw.capacity() as u64,
    };
    std::mem::forget(raw);
    unsafe { out.write(result) };
    true
}

/// # Safety
/// `out` must be null or the original writable collection returned by one
/// successful `memento_subtitle_parse`. Other pointers violate the contract.
#[no_mangle]
pub unsafe extern "C" fn memento_subtitle_free(out: *mut SubtitleEntriesC) {
    if out.is_null() { return; }
    let owned = std::mem::replace(unsafe { &mut *out }, SubtitleEntriesC::empty());
    if owned.entries.is_null() { return; }
    let items = unsafe {
        Vec::from_raw_parts(owned.entries, owned.len as usize, owned.capacity as usize)
    };
    for item in items {
        if !item.text.is_null() {
            drop(unsafe {
                Vec::from_raw_parts(item.text, item.text_len as usize, item.text_capacity as usize)
            });
        }
    }
}

// ---------------------------------------------------------------------------
// SubtitleListModel C ABI
// ---------------------------------------------------------------------------

#[repr(C)]
#[derive(Copy, Clone, Debug)]
pub struct MementoSubtitleCueC {
    pub text: *const u16,
    pub text_len: usize,
    pub start: f64,
    pub end: f64,
}

#[repr(C)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct MementoSubtitleAddPlanC {
    pub action: u32,
    pub index: i64,
}

#[repr(C)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct MementoSubtitleSpanC {
    pub first: usize,
    pub count: usize,
}

#[repr(C)]
pub struct MementoSubtitleSpansC {
    pub spans: *mut MementoSubtitleSpanC,
    pub len: usize,
    pub capacity: usize,
}

impl MementoSubtitleSpansC {
    pub fn empty() -> Self {
        Self {
            spans: ptr::null_mut(),
            len: 0,
            capacity: 0,
        }
    }
}

#[repr(C)]
pub struct MementoSubtitleIndicesC {
    pub indices: *mut usize,
    pub len: usize,
    pub capacity: usize,
}

impl MementoSubtitleIndicesC {
    pub fn empty() -> Self {
        Self {
            indices: ptr::null_mut(),
            len: 0,
            capacity: 0,
        }
    }
}

#[repr(C)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct MementoSubtitleDiffPlanC {
    pub prefix: usize,
    pub suffix: usize,
    pub old_mid_count: usize,
    pub new_mid_count: usize,
}

pub use crate::model::MementoSubtitleCandidateCueC;

unsafe fn cues_to_views<'a>(
    items: *const MementoSubtitleCueC,
    len: usize,
) -> Option<Vec<crate::model::SubtitleCueView<'a>>> {
    if len == 0 {
        return Some(Vec::new());
    }
    if items.is_null() {
        return None;
    }
    let cues = unsafe { std::slice::from_raw_parts(items, len) };
    let mut views = Vec::with_capacity(len);
    for cue in cues {
        let text = if cue.text_len == 0 {
            &[]
        } else {
            if cue.text.is_null() {
                return None;
            }
            unsafe { std::slice::from_raw_parts(cue.text, cue.text_len) }
        };
        views.push(crate::model::SubtitleCueView {
            text,
            start: cue.start,
            end: cue.end,
        });
    }
    Some(views)
}

/// # Safety
/// If `items_len > 0`, `items` must be a valid readable array of `MementoSubtitleCueC`.
/// If `text_len > 0`, `text` must be a valid readable UTF-16 slice.
#[no_mangle]
pub unsafe extern "C" fn memento_subtitle_model_plan_add(
    items: *const MementoSubtitleCueC,
    items_len: usize,
    block_adds: bool,
    text: *const u16,
    text_len: usize,
    start: f64,
    end: f64,
) -> MementoSubtitleAddPlanC {
    let result = catch_unwind(|| {
        let Some(views) = (unsafe { cues_to_views(items, items_len) }) else {
            return MementoSubtitleAddPlanC {
                action: crate::model::AddAction::Reject as u32,
                index: -1,
            };
        };
        let text_slice = if text_len == 0 {
            &[]
        } else {
            if text.is_null() {
                return MementoSubtitleAddPlanC {
                    action: crate::model::AddAction::Reject as u32,
                    index: -1,
                };
            }
            unsafe { std::slice::from_raw_parts(text, text_len) }
        };
        let plan = crate::model::plan_add_subtitle(&views, block_adds, text_slice, start, end);
        MementoSubtitleAddPlanC {
            action: plan.action as u32,
            index: plan.index as i64,
        }
    });
    result.unwrap_or(MementoSubtitleAddPlanC {
        action: crate::model::AddAction::Reject as u32,
        index: -1,
    })
}

/// # Safety
/// If `items_len > 0`, `items` must be a valid readable array of `MementoSubtitleCueC`.
/// `out_span` must be writable.
#[no_mangle]
pub unsafe extern "C" fn memento_subtitle_model_find_first_overlapping_span(
    items: *const MementoSubtitleCueC,
    items_len: usize,
    start: f64,
    end: f64,
    from_row: usize,
    out_span: *mut MementoSubtitleSpanC,
) -> bool {
    if out_span.is_null() {
        return false;
    }
    let result = catch_unwind(|| {
        let Some(views) = (unsafe { cues_to_views(items, items_len) }) else {
            return false;
        };
        if let Some(span) = crate::model::find_first_overlapping_span(&views, start, end, from_row) {
            unsafe {
                out_span.write(MementoSubtitleSpanC {
                    first: span.first,
                    count: span.count,
                });
            }
            true
        } else {
            false
        }
    });
    result.unwrap_or(false)
}

/// # Safety
/// If `items_len > 0`, `items` must be a valid readable array of `MementoSubtitleCueC`.
/// `out` must be a writable non-null pointer to `MementoSubtitleSpansC`.
#[no_mangle]
pub unsafe extern "C" fn memento_subtitle_model_collect_overlapping(
    items: *const MementoSubtitleCueC,
    items_len: usize,
    start: f64,
    end: f64,
    out: *mut MementoSubtitleSpansC,
) {
    if out.is_null() {
        return;
    }
    unsafe { out.write(MementoSubtitleSpansC::empty()) };
    let _ = catch_unwind(|| {
        let Some(views) = (unsafe { cues_to_views(items, items_len) }) else {
            return;
        };
        let spans = crate::model::collect_overlapping_spans(&views, start, end);
        if spans.is_empty() {
            return;
        }
        let mut raw: Vec<MementoSubtitleSpanC> = spans
            .into_iter()
            .map(|s| MementoSubtitleSpanC {
                first: s.first,
                count: s.count,
            })
            .collect();
        let result = MementoSubtitleSpansC {
            spans: raw.as_mut_ptr(),
            len: raw.len(),
            capacity: raw.capacity(),
        };
        std::mem::forget(raw);
        unsafe { out.write(result) };
    });
}

/// # Safety
/// `out` must be null or pointing to a `MementoSubtitleSpansC` returned by `memento_subtitle_model_collect_overlapping`.
#[no_mangle]
pub unsafe extern "C" fn memento_subtitle_model_free_spans(out: *mut MementoSubtitleSpansC) {
    if out.is_null() {
        return;
    }
    let owned = std::mem::replace(unsafe { &mut *out }, MementoSubtitleSpansC::empty());
    if !owned.spans.is_null() && owned.capacity > 0 {
        drop(unsafe { Vec::from_raw_parts(owned.spans, owned.len, owned.capacity) });
    }
}

/// # Safety
/// If `items_len > 0`, `items` must be a valid readable array of `MementoSubtitleCueC`.
/// `out` must be a writable non-null pointer to `MementoSubtitleIndicesC`.
#[no_mangle]
pub unsafe extern "C" fn memento_subtitle_model_select_position(
    items: *const MementoSubtitleCueC,
    items_len: usize,
    position: f64,
    out: *mut MementoSubtitleIndicesC,
) {
    if out.is_null() {
        return;
    }
    unsafe { out.write(MementoSubtitleIndicesC::empty()) };
    let _ = catch_unwind(|| {
        let Some(views) = (unsafe { cues_to_views(items, items_len) }) else {
            return;
        };
        let mut rows = crate::model::select_position_rows(&views, position);
        if rows.is_empty() {
            return;
        }
        let result = MementoSubtitleIndicesC {
            indices: rows.as_mut_ptr(),
            len: rows.len(),
            capacity: rows.capacity(),
        };
        std::mem::forget(rows);
        unsafe { out.write(result) };
    });
}

/// # Safety
/// `out` must be null or pointing to a `MementoSubtitleIndicesC` returned by `memento_subtitle_model_select_position`.
#[no_mangle]
pub unsafe extern "C" fn memento_subtitle_model_free_indices(out: *mut MementoSubtitleIndicesC) {
    if out.is_null() {
        return;
    }
    let owned = std::mem::replace(unsafe { &mut *out }, MementoSubtitleIndicesC::empty());
    if !owned.indices.is_null() && owned.capacity > 0 {
        drop(unsafe { Vec::from_raw_parts(owned.indices, owned.len, owned.capacity) });
    }
}

/// # Safety
/// If `old_len > 0`, `old_items` must be a valid readable array of `MementoSubtitleCueC`.
/// If `new_len > 0`, `new_items` must be a valid readable array of `MementoSubtitleCueC`.
#[no_mangle]
pub unsafe extern "C" fn memento_subtitle_model_diff_items(
    old_items: *const MementoSubtitleCueC,
    old_len: usize,
    new_items: *const MementoSubtitleCueC,
    new_len: usize,
) -> MementoSubtitleDiffPlanC {
    let result = catch_unwind(|| {
        let old_views = (unsafe { cues_to_views(old_items, old_len) }).unwrap_or_default();
        let new_views = (unsafe { cues_to_views(new_items, new_len) }).unwrap_or_default();
        let diff = crate::model::diff_items(&old_views, &new_views);
        MementoSubtitleDiffPlanC {
            prefix: diff.prefix,
            suffix: diff.suffix,
            old_mid_count: diff.old_mid_count,
            new_mid_count: diff.new_mid_count,
        }
    });
    result.unwrap_or(MementoSubtitleDiffPlanC {
        prefix: 0,
        suffix: 0,
        old_mid_count: old_len,
        new_mid_count: new_len,
    })
}

/// # Safety
/// If `len > 0`, `candidates` must be a valid readable array of `MementoSubtitleCandidateCueC`.
/// `out` must be a writable non-null pointer to `MementoSubtitleIndicesC`.
#[no_mangle]
pub unsafe extern "C" fn memento_subtitle_filter_and_sort_cues(
    candidates: *const MementoSubtitleCandidateCueC,
    len: usize,
    out: *mut MementoSubtitleIndicesC,
) {
    if out.is_null() {
        return;
    }
    unsafe { out.write(MementoSubtitleIndicesC::empty()) };
    let _ = catch_unwind(|| {
        if len == 0 || candidates.is_null() {
            return;
        }
        let slice = unsafe { std::slice::from_raw_parts(candidates, len) };
        let mut indices = crate::model::filter_and_sort_cues(slice);
        if indices.is_empty() {
            return;
        }
        let result = MementoSubtitleIndicesC {
            indices: indices.as_mut_ptr(),
            len: indices.len(),
            capacity: indices.capacity(),
        };
        std::mem::forget(indices);
        unsafe { out.write(result) };
    });
}

#[repr(C)]
#[derive(Copy, Clone, Debug)]
pub struct MementoSubtitleTextViewC {
    pub text: *const u16,
    pub text_len: usize,
}

/// # Safety
/// If `candidates_len > 0`, `candidates` must be a valid readable array of `MementoSubtitleTextViewC`.
/// If `query_len > 0`, `query` must be a valid readable UTF-16 buffer.
/// `out_indices` must be a writable non-null pointer to `MementoSubtitleIndicesC`.
#[no_mangle]
pub unsafe extern "C" fn memento_subtitle_model_find(
    candidates: *const MementoSubtitleTextViewC,
    candidates_len: usize,
    query: *const u16,
    query_len: usize,
    ignore_whitespace: bool,
    out_indices: *mut MementoSubtitleIndicesC,
) {
    if out_indices.is_null() {
        return;
    }
    unsafe { out_indices.write(MementoSubtitleIndicesC::empty()) };
    let _ = catch_unwind(|| {
        if candidates_len == 0 || candidates.is_null() {
            return;
        }
        let query_slice = if query_len == 0 {
            &[][..]
        } else {
            if query.is_null() {
                return;
            }
            unsafe { std::slice::from_raw_parts(query, query_len) }
        };

        let cand_slice = unsafe { std::slice::from_raw_parts(candidates, candidates_len) };
        let mut candidate_views = Vec::with_capacity(candidates_len);
        for c in cand_slice {
            let text = if c.text_len == 0 {
                &[][..]
            } else {
                if c.text.is_null() {
                    return;
                }
                unsafe { std::slice::from_raw_parts(c.text, c.text_len) }
            };
            candidate_views.push(text);
        }

        let mut indices = crate::model::find_matching_rows(
            &candidate_views,
            query_slice,
            ignore_whitespace,
        );
        if indices.is_empty() {
            return;
        }
        let result = MementoSubtitleIndicesC {
            indices: indices.as_mut_ptr(),
            len: indices.len(),
            capacity: indices.capacity(),
        };
        std::mem::forget(indices);
        unsafe { out_indices.write(result) };
    });
}

/// # Safety
/// `out` must be null or pointing to a `MementoSubtitleIndicesC` returned by a subtitle C API call.
#[no_mangle]
pub unsafe extern "C" fn memento_subtitle_free_indices(out: *mut MementoSubtitleIndicesC) {
    memento_subtitle_model_free_indices(out);
}

/// # Safety
/// `out_insert_memory_track` may be null or a writable pointer to a bool.
#[no_mangle]
pub unsafe extern "C" fn memento_subtitle_should_update_memory_track(
    track_id: i64,
    has_model: bool,
    lines_non_empty: bool,
    was_memory_track: bool,
    out_insert_memory_track: *mut bool,
) -> bool {
    let (should_update, insert_track) = crate::model::should_update_memory_track(
        track_id,
        has_model,
        lines_non_empty,
        was_memory_track,
    );
    if !out_insert_memory_track.is_null() {
        unsafe { *out_insert_memory_track = insert_track };
    }
    should_update
}

#[repr(C)]
#[derive(Copy, Clone, Debug)]
pub struct MementoSubtitleTrackInC {
    pub is_null: bool,
    pub track_id: i64,
    pub is_external: bool,
    pub external_path: *const u16,
    pub external_path_len: usize,
}

#[repr(C)]
#[derive(Copy, Clone, Debug)]
pub struct MementoSubtitleExternalPathC {
    pub track_id: i64,
    pub path: *const u16,
    pub path_len: usize,
}

#[repr(C)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct MementoSubtitleTrackActionC {
    pub action: u32,
    pub track_id: i64,
    pub external_path: *const u16,
    pub external_path_len: usize,
    pub read_file: bool,
}

#[repr(C)]
pub struct MementoSubtitleTrackPlanC {
    pub actions: *mut MementoSubtitleTrackActionC,
    pub len: usize,
    pub capacity: usize,
}

impl MementoSubtitleTrackPlanC {
    pub fn empty() -> Self {
        Self {
            actions: ptr::null_mut(),
            len: 0,
            capacity: 0,
        }
    }
}

#[repr(C)]
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct MementoSubtitleActiveCuePlanC {
    pub action: u32,
    pub raw_position: f64,
}

pub use crate::model::{
    ActiveCueActionKind, ActiveCuePlan, SubtitleExternalPathInput, SubtitleTrackInput,
    TrackAction, TrackActionKind, TrackUpdatePlan,
};

/// # Safety
/// If `tracks_len > 0`, `tracks` must be readable.
/// If `existing_model_ids_len > 0`, `existing_model_ids` must be readable.
/// If `existing_external_paths_len > 0`, `existing_external_paths` must be readable.
/// `out_plan` must be a writable non-null pointer.
#[no_mangle]
pub unsafe extern "C" fn memento_subtitle_plan_track_changes(
    tracks: *const MementoSubtitleTrackInC,
    tracks_len: usize,
    existing_model_ids: *const i64,
    existing_model_ids_len: usize,
    existing_external_paths: *const MementoSubtitleExternalPathC,
    existing_external_paths_len: usize,
    out_plan: *mut MementoSubtitleTrackPlanC,
) {
    if out_plan.is_null() {
        return;
    }
    unsafe { out_plan.write(MementoSubtitleTrackPlanC::empty()) };
    let _ = catch_unwind(|| {
        let tracks_slice = if tracks_len == 0 || tracks.is_null() {
            &[]
        } else {
            unsafe { std::slice::from_raw_parts(tracks, tracks_len) }
        };

        let mut track_inputs = Vec::with_capacity(tracks_slice.len());
        for t in tracks_slice {
            let path_slice = if t.external_path_len == 0 || t.external_path.is_null() {
                &[][..]
            } else {
                unsafe { std::slice::from_raw_parts(t.external_path, t.external_path_len) }
            };
            track_inputs.push(crate::model::SubtitleTrackInput {
                is_null: t.is_null,
                track_id: t.track_id,
                is_external: t.is_external,
                external_path: path_slice,
            });
        }

        let model_ids_slice = if existing_model_ids_len == 0 || existing_model_ids.is_null() {
            &[]
        } else {
            unsafe { std::slice::from_raw_parts(existing_model_ids, existing_model_ids_len) }
        };

        let ext_paths_slice = if existing_external_paths_len == 0 || existing_external_paths.is_null() {
            &[]
        } else {
            unsafe { std::slice::from_raw_parts(existing_external_paths, existing_external_paths_len) }
        };

        let mut ext_path_inputs = Vec::with_capacity(ext_paths_slice.len());
        for ep in ext_paths_slice {
            let path_slice = if ep.path_len == 0 || ep.path.is_null() {
                &[][..]
            } else {
                unsafe { std::slice::from_raw_parts(ep.path, ep.path_len) }
            };
            ext_path_inputs.push(crate::model::SubtitleExternalPathInput {
                track_id: ep.track_id,
                path: path_slice,
            });
        }

        let plan = crate::model::plan_track_changes(
            &track_inputs,
            model_ids_slice,
            &ext_path_inputs,
        );

        if plan.actions.is_empty() {
            return;
        }

        let mut raw: Vec<MementoSubtitleTrackActionC> = plan
            .actions
            .into_iter()
            .map(|a| match a {
                crate::model::TrackAction::CreateModel { track_id } => MementoSubtitleTrackActionC {
                    action: crate::model::TrackActionKind::CreateModel as u32,
                    track_id,
                    external_path: ptr::null(),
                    external_path_len: 0,
                    read_file: false,
                },
                crate::model::TrackAction::ReloadExternal {
                    track_id,
                    path,
                    read_file,
                } => MementoSubtitleTrackActionC {
                    action: crate::model::TrackActionKind::ReloadExternal as u32,
                    track_id,
                    external_path: if path.is_empty() { ptr::null() } else { path.as_ptr() },
                    external_path_len: path.len(),
                    read_file,
                },
                crate::model::TrackAction::ClearExternal { track_id } => MementoSubtitleTrackActionC {
                    action: crate::model::TrackActionKind::ClearExternal as u32,
                    track_id,
                    external_path: ptr::null(),
                    external_path_len: 0,
                    read_file: false,
                },
                crate::model::TrackAction::RemoveTrack { track_id } => MementoSubtitleTrackActionC {
                    action: crate::model::TrackActionKind::RemoveTrack as u32,
                    track_id,
                    external_path: ptr::null(),
                    external_path_len: 0,
                    read_file: false,
                },
            })
            .collect();

        let result = MementoSubtitleTrackPlanC {
            actions: raw.as_mut_ptr(),
            len: raw.len(),
            capacity: raw.capacity(),
        };
        std::mem::forget(raw);
        unsafe { out_plan.write(result) };
    });
}

/// # Safety
/// `out_plan` must be null or pointing to a `MementoSubtitleTrackPlanC` returned by `memento_subtitle_plan_track_changes`.
#[no_mangle]
pub unsafe extern "C" fn memento_subtitle_free_track_plan(out_plan: *mut MementoSubtitleTrackPlanC) {
    if out_plan.is_null() {
        return;
    }
    let owned = std::mem::replace(unsafe { &mut *out_plan }, MementoSubtitleTrackPlanC::empty());
    if !owned.actions.is_null() && owned.capacity > 0 {
        drop(unsafe { Vec::from_raw_parts(owned.actions, owned.len, owned.capacity) });
    }
}

#[no_mangle]
pub extern "C" fn memento_subtitle_plan_active_cue(
    has_model: bool,
    has_subtitle: bool,
    text_non_empty: bool,
    raw_position: f64,
) -> MementoSubtitleActiveCuePlanC {
    let plan = crate::model::plan_active_cue(has_model, has_subtitle, text_non_empty, raw_position);
    MementoSubtitleActiveCuePlanC {
        action: plan.action as u32,
        raw_position: plan.raw_position,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn to_utf16(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    #[test]
    fn test_capi_plan_add_and_diff() {
        let text1: Vec<u16> = "first".encode_utf16().collect();
        let text2: Vec<u16> = "second".encode_utf16().collect();

        let cue1 = MementoSubtitleCueC {
            text: text1.as_ptr(),
            text_len: text1.len(),
            start: 1.0,
            end: 2.0,
        };

        // Add
        let plan = unsafe {
            memento_subtitle_model_plan_add(
                &cue1,
                1,
                false,
                text2.as_ptr(),
                text2.len(),
                3.0,
                4.0,
            )
        };
        assert_eq!(plan.action, crate::model::AddAction::Insert as u32);
        assert_eq!(plan.index, 1);

        // Diff
        let diff = unsafe {
            memento_subtitle_model_diff_items(
                &cue1,
                1,
                &cue1,
                1,
            )
        };
        assert_eq!(diff.prefix, 1);
        assert_eq!(diff.suffix, 0);
        assert_eq!(diff.old_mid_count, 0);
        assert_eq!(diff.new_mid_count, 0);

        // Select position
        let mut indices = MementoSubtitleIndicesC::empty();
        unsafe {
            memento_subtitle_model_select_position(
                &cue1,
                1,
                1.5,
                &mut indices,
            );
        }
        assert_eq!(indices.len, 1);
        assert!(!indices.indices.is_null());
        assert_eq!(unsafe { *indices.indices }, 0);
        unsafe {
            memento_subtitle_model_free_indices(&mut indices);
        }
        assert_eq!(indices.len, 0);

        // Collect overlapping
        let mut spans = MementoSubtitleSpansC::empty();
        unsafe {
            memento_subtitle_model_collect_overlapping(
                &cue1,
                1,
                0.5,
                1.5,
                &mut spans,
            );
        }
        assert_eq!(spans.len, 1);
        assert!(!spans.spans.is_null());
        assert_eq!(unsafe { (*spans.spans).first }, 0);
        assert_eq!(unsafe { (*spans.spans).count }, 1);
        unsafe {
            memento_subtitle_model_free_spans(&mut spans);
        }
        assert_eq!(spans.len, 0);
    }

    #[test]
    fn test_capi_filter_and_sort_cues() {
        let candidates = [
            MementoSubtitleCandidateCueC {
                original_index: 0,
                id_valid: true,
                cue_id: 1,
                start_valid: true,
                start: 10.0,
                end_valid: true,
                end: 12.0,
            },
            MementoSubtitleCandidateCueC {
                original_index: 1,
                id_valid: false,
                cue_id: 2,
                start_valid: true,
                start: 1.0,
                end_valid: true,
                end: 2.0,
            },
            MementoSubtitleCandidateCueC {
                original_index: 2,
                id_valid: true,
                cue_id: 3,
                start_valid: true,
                start: 3.0,
                end_valid: true,
                end: 5.0,
            },
        ];

        let mut indices = MementoSubtitleIndicesC::empty();
        unsafe {
            memento_subtitle_filter_and_sort_cues(
                candidates.as_ptr(),
                candidates.len(),
                &mut indices,
            );
        }
        assert_eq!(indices.len, 2);
        assert!(!indices.indices.is_null());
        let res = unsafe { std::slice::from_raw_parts(indices.indices, indices.len) };
        assert_eq!(res, &[2, 0]);
        unsafe {
            memento_subtitle_free_indices(&mut indices);
        }
        assert_eq!(indices.len, 0);

        // Null pointer safety
        let mut null_out = MementoSubtitleIndicesC::empty();
        unsafe {
            memento_subtitle_filter_and_sort_cues(std::ptr::null(), 5, &mut null_out);
        }
        assert_eq!(null_out.len, 0);
    }

    #[test]
    fn test_capi_should_update_memory_track() {
        let mut insert = false;
        let update = unsafe {
            memento_subtitle_should_update_memory_track(
                10,
                true,
                true,
                false,
                &mut insert,
            )
        };
        assert!(update);
        assert!(insert);

        // Null pointer for out_insert
        let update2 = unsafe {
            memento_subtitle_should_update_memory_track(
                -1,
                true,
                true,
                false,
                std::ptr::null_mut(),
            )
        };
        assert!(!update2);
    }

    #[test]
    fn test_capi_plan_active_cue() {
        let plan = memento_subtitle_plan_active_cue(false, true, true, 42.0);
        assert_eq!(plan.action, crate::model::ActiveCueActionKind::Noop as u32);
        assert_eq!(plan.raw_position, 0.0);

        let plan = memento_subtitle_plan_active_cue(true, true, false, 42.0);
        assert_eq!(plan.action, crate::model::ActiveCueActionKind::ClearSelection as u32);
        assert_eq!(plan.raw_position, 0.0);

        let plan = memento_subtitle_plan_active_cue(true, true, true, 42.5);
        assert_eq!(plan.action, crate::model::ActiveCueActionKind::AddAndSelect as u32);
        assert_eq!(plan.raw_position, 42.5);
    }

    #[test]
    fn test_capi_plan_track_changes() {
        let path1: Vec<u16> = "external.srt".encode_utf16().collect();
        let track = MementoSubtitleTrackInC {
            is_null: false,
            track_id: 1,
            is_external: true,
            external_path: path1.as_ptr(),
            external_path_len: path1.len(),
        };

        let mut plan = MementoSubtitleTrackPlanC::empty();
        unsafe {
            memento_subtitle_plan_track_changes(
                &track,
                1,
                std::ptr::null(),
                0,
                std::ptr::null(),
                0,
                &mut plan,
            );
        }

        assert_eq!(plan.len, 2);
        assert!(!plan.actions.is_null());
        let actions = unsafe { std::slice::from_raw_parts(plan.actions, plan.len) };
        assert_eq!(actions[0].action, crate::model::TrackActionKind::CreateModel as u32);
        assert_eq!(actions[0].track_id, 1);
        assert_eq!(actions[1].action, crate::model::TrackActionKind::ReloadExternal as u32);
        assert_eq!(actions[1].track_id, 1);
        assert!(actions[1].read_file);
        assert_eq!(actions[1].external_path_len, path1.len());

        unsafe {
            memento_subtitle_free_track_plan(&mut plan);
        }
        assert_eq!(plan.len, 0);
        assert!(plan.actions.is_null());

        // Null pointer safety
        let mut null_plan = MementoSubtitleTrackPlanC::empty();
        unsafe {
            memento_subtitle_plan_track_changes(
                std::ptr::null(),
                10,
                std::ptr::null(),
                5,
                std::ptr::null(),
                2,
                &mut null_plan,
            );
            assert_eq!(null_plan.len, 0);
            memento_subtitle_free_track_plan(&mut null_plan);
            memento_subtitle_free_track_plan(std::ptr::null_mut());
        }
    }

    #[test]
    fn test_capi_model_find() {
        let t1 = to_utf16("Hello World");
        let t2 = to_utf16("Second cue");
        let query = to_utf16("hello");

        let candidates = [
            MementoSubtitleTextViewC { text: t1.as_ptr(), text_len: t1.len() },
            MementoSubtitleTextViewC { text: t2.as_ptr(), text_len: t2.len() },
        ];

        let mut out = MementoSubtitleIndicesC::empty();
        unsafe {
            memento_subtitle_model_find(
                candidates.as_ptr(),
                candidates.len(),
                query.as_ptr(),
                query.len(),
                false,
                &mut out,
            );
        }

        assert_eq!(out.len, 1);
        assert!(!out.indices.is_null());
        assert_eq!(unsafe { *out.indices }, 0);

        unsafe {
            memento_subtitle_model_free_indices(&mut out);
        }
        assert_eq!(out.len, 0);
        assert!(out.indices.is_null());

        // Null pointer safety
        let mut null_out = MementoSubtitleIndicesC::empty();
        unsafe {
            memento_subtitle_model_find(
                std::ptr::null(),
                5,
                query.as_ptr(),
                query.len(),
                false,
                &mut null_out,
            );
            assert_eq!(null_out.len, 0);

            memento_subtitle_model_find(
                candidates.as_ptr(),
                candidates.len(),
                std::ptr::null(),
                5,
                false,
                &mut null_out,
            );
            assert_eq!(null_out.len, 0);

            // Null out pointer shouldn't crash
            memento_subtitle_model_find(
                candidates.as_ptr(),
                candidates.len(),
                query.as_ptr(),
                query.len(),
                false,
                std::ptr::null_mut(),
            );

            memento_subtitle_model_free_indices(std::ptr::null_mut());
        }
    }
}


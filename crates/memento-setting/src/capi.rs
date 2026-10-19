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

//! C ABI surface for `memento-setting`.

use std::ffi::{c_char, CStr};
use crate::migration::MementoMigrationPlanC;

#[repr(C, align(8))]
struct U16AllocHeader {
    len: usize,
    _pad: usize,
}

/// Computes the migration steps required for `version`.
///
/// # Safety
/// If `out` is non-null, it must point to a valid writable [`MementoMigrationPlanC`].
#[no_mangle]
pub unsafe extern "C" fn memento_setting_migration_plan(
    version: u32,
    out: *mut MementoMigrationPlanC,
) {
    if !out.is_null() {
        *out = crate::migration::migration_plan(version);
    }
}

/// Returns the GlossaryStyle discriminant for the version 2 boolean `search/list-result`.
#[no_mangle]
pub extern "C" fn memento_setting_migrate_2_to_3_list_result(list: bool) -> i32 {
    crate::migration::migrate_2_to_3_list_result(list)
}

/// Normalizes a path per `writeablePath` in `migrate5to6()`.
///
/// The returned UTF-16 buffer must be freed with [`memento_setting_free_u16`].
///
/// # Safety
/// `path` must point to `len` readable `u16`s (or be null when `len` is 0).
/// `out` and `out_len` must be writable pointers.
#[no_mangle]
pub unsafe extern "C" fn memento_setting_normalize_writable_path(
    path: *const u16,
    len: u64,
    out: *mut *mut u16,
    out_len: *mut u64,
) -> bool {
    if out.is_null() || out_len.is_null() {
        return false;
    }
    *out = std::ptr::null_mut();
    *out_len = 0;

    let slice = if len == 0 || path.is_null() {
        &[][..]
    } else {
        std::slice::from_raw_parts(path, len as usize)
    };

    let result = crate::migration::normalize_writable_path(slice);
    let res_len = result.len();

    let header_size = std::mem::size_of::<U16AllocHeader>();
    let total_bytes = match res_len
        .checked_mul(std::mem::size_of::<u16>())
        .and_then(|b| b.checked_add(header_size))
    {
        Some(b) => b,
        None => return false,
    };

    let Ok(layout) = std::alloc::Layout::from_size_align(
        total_bytes,
        std::mem::align_of::<U16AllocHeader>(),
    ) else {
        return false;
    };

    let ptr = std::alloc::alloc(layout);
    if ptr.is_null() {
        return false;
    }

    let header_ptr = ptr as *mut U16AllocHeader;
    (*header_ptr).len = res_len;

    let data_ptr = ptr.add(header_size) as *mut u16;
    if res_len > 0 {
        std::ptr::copy_nonoverlapping(result.as_ptr(), data_ptr, res_len);
    }

    *out = data_ptr;
    *out_len = res_len as u64;
    true
}

/// Releases a UTF-16 buffer returned by [`memento_setting_normalize_writable_path`].
///
/// # Safety
/// `ptr` must be null or an allocation returned by [`memento_setting_normalize_writable_path`].
#[no_mangle]
pub unsafe extern "C" fn memento_setting_free_u16(ptr: *mut u16, _len: u64) {
    if ptr.is_null() {
        return;
    }
    let header_size = std::mem::size_of::<U16AllocHeader>();
    let base_ptr = (ptr as *mut u8).sub(header_size);
    let header_ptr = base_ptr as *mut U16AllocHeader;
    let len = (*header_ptr).len;
    let total_bytes = header_size + len * std::mem::size_of::<u16>();
    let layout = std::alloc::Layout::from_size_align_unchecked(
        total_bytes,
        std::mem::align_of::<U16AllocHeader>(),
    );
    std::alloc::dealloc(base_ptr, layout);
}

/// Returns the ASR backend default string ("whisper" or "onnx") based on build defines.
///
/// # Safety
/// `out_ptr` and `out_len` must be writable pointers.
#[no_mangle]
pub unsafe extern "C" fn memento_setting_asr_backend_default(
    whisper_enabled: bool,
    onnx_enabled: bool,
    out_ptr: *mut *const c_char,
    out_len: *mut u64,
) {
    if !out_ptr.is_null() && !out_len.is_null() {
        let (bytes, len) = if !whisper_enabled && onnx_enabled {
            (b"onnx\0".as_ptr() as *const c_char, 4u64)
        } else {
            (b"whisper\0".as_ptr() as *const c_char, 7u64)
        };
        *out_ptr = bytes;
        *out_len = len;
    }
}

// -----------------------------------------------------------------------------
// Models C ABI (AudioSourceModel, KeybindProfileModel, DownloadManager)
// -----------------------------------------------------------------------------

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct MementoSettingTextViewC {
    pub data: *const u16,
    pub len: u64,
}

impl AsRef<[u16]> for MementoSettingTextViewC {
    fn as_ref(&self) -> &[u16] {
        if self.len == 0 || self.data.is_null() {
            &[]
        } else {
            unsafe { std::slice::from_raw_parts(self.data, self.len as usize) }
        }
    }
}

/// Plans moving an audio source row.
///
/// Returns `true` if valid and writes destination row into `out_destination`.
/// Returns `false` if invalid or if `out_destination` is null.
///
/// # Safety
/// If `out_destination` is non-null, it must be a valid writable pointer.
#[no_mangle]
pub unsafe extern "C" fn memento_setting_audio_source_plan_move(
    from: i32,
    to: i32,
    count: i32,
    out_destination: *mut i32,
) -> bool {
    if out_destination.is_null() {
        return false;
    }
    match crate::models::audio_source_plan_move(from, to, count) {
        Some(dest) => {
            *out_destination = dest;
            true
        }
        None => false,
    }
}

/// Performs binary search lower bound over an array of UTF-16 profile names.
///
/// Sets `out_index` to the lower bound index (0..=names_len).
/// If `out_exact_match` is non-null, sets it to whether an exact match was found.
/// Returns `true` on success, or `false` if invalid pointers (e.g. `out_index` is null).
///
/// # Safety
/// `names` must point to `names_len` valid `MementoSettingTextViewC` structs (or be null when `names_len` is 0).
/// `target` must point to `target_len` valid UTF-16 code units (or be null when `target_len` is 0).
/// `out_index` must be a valid writable pointer.
/// `out_exact_match`, if non-null, must be a valid writable pointer.
#[no_mangle]
pub unsafe extern "C" fn memento_setting_profile_lower_bound(
    names: *const MementoSettingTextViewC,
    names_len: u64,
    target: *const u16,
    target_len: u64,
    out_index: *mut u64,
    out_exact_match: *mut bool,
) -> bool {
    if out_index.is_null() {
        return false;
    }
    if names_len > 0 && names.is_null() {
        return false;
    }
    if target_len > 0 && target.is_null() {
        return false;
    }

    let target_slice = if target_len == 0 || target.is_null() {
        &[][..]
    } else {
        std::slice::from_raw_parts(target, target_len as usize)
    };

    let names_slice = if names_len == 0 || names.is_null() {
        &[][..]
    } else {
        std::slice::from_raw_parts(names, names_len as usize)
    };

    let view_of = |item: &MementoSettingTextViewC| -> &[u16] {
        if item.len == 0 || item.data.is_null() {
            &[]
        } else {
            std::slice::from_raw_parts(item.data, item.len as usize)
        }
    };

    let idx = names_slice.partition_point(|item| view_of(item) < target_slice);
    let exact_match = idx < names_slice.len() && view_of(&names_slice[idx]) == target_slice;

    *out_index = idx as u64;
    if !out_exact_match.is_null() {
        *out_exact_match = exact_match;
    }
    true
}

/// Computes download speed in bytes/second from bytes received and elapsed milliseconds.
#[no_mangle]
pub extern "C" fn memento_setting_download_speed(
    received: i64,
    elapsed_ms: i64,
) -> i64 {
    crate::models::download_speed(received, elapsed_ms)
}

/// Evaluates if download progress state changed and writes new progress.
///
/// Returns `true` if any of progress, received, total, or speed changed.
///
/// # Safety
/// If `out_progress` is non-null, it must be a valid writable pointer.
#[no_mangle]
pub unsafe extern "C" fn memento_setting_download_progress_update(
    prev_progress: f64,
    prev_received: i64,
    prev_total: i64,
    prev_speed: i64,
    received: i64,
    total: i64,
    speed: i64,
    out_progress: *mut f64,
) -> bool {
    let (progress, changed) = crate::models::download_progress_update(
        prev_progress,
        prev_received,
        prev_total,
        prev_speed,
        received,
        total,
        speed,
    );
    if !out_progress.is_null() {
        *out_progress = progress;
    }
    changed
}

/// Combined download progress, speed, and change calculation.
///
/// # Safety
/// Output pointers that are non-null must be valid and writable.
#[no_mangle]
pub unsafe extern "C" fn memento_setting_download_progress_calc(
    received: i64,
    total: i64,
    elapsed_ms: i64,
    prev_progress: f64,
    prev_received: i64,
    prev_total: i64,
    prev_speed: i64,
    out_progress: *mut f64,
    out_speed: *mut i64,
    out_changed: *mut bool,
) -> bool {
    let speed = crate::models::download_speed(received, elapsed_ms);
    let (progress, changed) = crate::models::download_progress_update(
        prev_progress,
        prev_received,
        prev_total,
        prev_speed,
        received,
        total,
        speed,
    );
    if !out_progress.is_null() {
        *out_progress = progress;
    }
    if !out_speed.is_null() {
        *out_speed = speed;
    }
    if !out_changed.is_null() {
        *out_changed = changed;
    }
    true
}

/// Evaluates the keybind sequence update state transition.
///
/// Returns a [`crate::models::SequenceUpdate`] discriminant:
/// - `0`: Unchanged (`seq == newSeq`, no signal emitted)
/// - `1`: ConflictKeepOld (conflict in `m_existingBinds`, signal emitted with old value)
/// - `2`: Updated (sequence updated, signal emitted with new value)
#[no_mangle]
pub extern "C" fn memento_setting_plan_keybind_sequence(
    seq_equal: bool,
    is_conflict: bool,
) -> i32 {
    crate::models::plan_keybind_sequence(seq_equal, is_conflict) as i32
}

/// Resolves the ASR backend given compile flags and requested backend string.
///
/// Returns `true` on success and writes static UTF-8 null-terminated pointer and byte length.
///
/// # Safety
/// Output pointers `out_utf8` and `out_len` must be valid writable pointers.
/// `requested` must point to `requested_len` valid UTF-16 code units (or be null if len is 0).
#[no_mangle]
pub unsafe extern "C" fn memento_setting_resolve_asr_backend(
    whisper_compiled: bool,
    onnx_compiled: bool,
    requested: *const u16,
    requested_len: u64,
    out_utf8: *mut *const c_char,
    out_len: *mut u64,
) -> bool {
    if out_utf8.is_null() || out_len.is_null() {
        return false;
    }
    let slice = if requested.is_null() || requested_len == 0 {
        &[][..]
    } else {
        std::slice::from_raw_parts(requested, requested_len as usize)
    };
    let backend = crate::models::resolve_asr_backend(whisper_compiled, onnx_compiled, slice);
    match backend {
        "onnx" => {
            *out_utf8 = b"onnx\0".as_ptr() as *const c_char;
            *out_len = 4;
        }
        _ => {
            *out_utf8 = b"whisper\0".as_ptr() as *const c_char;
            *out_len = 7;
        }
    }
    true
}

/// Validates requested Whisper model. Writes `true` to `out_use_default` if invalid, `false` if valid.
///
/// # Safety
/// `out_use_default` must be a valid writable pointer.
/// `requested` must point to `requested_len` valid UTF-16 code units (or be null if len is 0).
#[no_mangle]
pub unsafe extern "C" fn memento_setting_resolve_whisper_model(
    requested: *const u16,
    requested_len: u64,
    out_use_default: *mut bool,
) -> bool {
    if out_use_default.is_null() {
        return false;
    }
    let slice = if requested.is_null() || requested_len == 0 {
        &[][..]
    } else {
        std::slice::from_raw_parts(requested, requested_len as usize)
    };
    let (_, use_default) = crate::models::resolve_whisper_model(slice);
    *out_use_default = use_default;
    true
}

/// Validates requested ONNX model. Writes `true` to `out_use_default` if invalid, `false` if valid.
///
/// # Safety
/// `out_use_default` must be a valid writable pointer.
/// `requested` must point to `requested_len` valid UTF-16 code units (or be null if len is 0).
#[no_mangle]
pub unsafe extern "C" fn memento_setting_resolve_onnx_model(
    requested: *const u16,
    requested_len: u64,
    out_use_default: *mut bool,
) -> bool {
    if out_use_default.is_null() {
        return false;
    }
    let slice = if requested.is_null() || requested_len == 0 {
        &[][..]
    } else {
        std::slice::from_raw_parts(requested, requested_len as usize)
    };
    let (_, use_default) = crate::models::resolve_onnx_model(slice);
    *out_use_default = use_default;
    true
}

/// Clamps GPU device index to `[0, 15]`.
#[no_mangle]
pub extern "C" fn memento_setting_clamp_gpu_device(value: i32) -> i32 {
    crate::models::clamp_gpu_device(value)
}

/// Computes max threads for Whisper/ONNX given Qt ideal thread count.
#[no_mangle]
pub extern "C" fn memento_setting_whisper_max_threads(ideal_thread_count: i32) -> i32 {
    crate::models::whisper_max_threads(ideal_thread_count)
}

/// Clamps thread count for Whisper/ONNX given requested value and Qt ideal thread count.
#[no_mangle]
pub extern "C" fn memento_setting_clamp_threads(
    value: i32,
    ideal_thread_count: i32,
) -> i32 {
    crate::models::clamp_threads(value, ideal_thread_count)
}

/// Clamps beam size or best_of to `[1, 10]`.
#[no_mangle]
pub extern "C" fn memento_setting_clamp_beam_or_best_of(value: i32) -> i32 {
    crate::models::clamp_beam_or_best_of(value)
}

/// Plans updating a recent files list when prepending `new_val`.
///
/// Returns `true` on success, writes surviving indices into `out_indices` (if non-null),
/// and writes the count of surviving indices to `out_len`.
///
/// # Safety
/// `items` must point to `items_len` valid `MementoSettingTextViewC` structs (or be null when `items_len` is 0).
/// `new_val` must point to `new_val_len` valid UTF-16 code units (or be null when `new_val_len` is 0).
/// If `out_indices` is non-null, it must point to writable memory with capacity for at least `*out_len` elements.
/// `out_len` must be a valid writable pointer.
#[no_mangle]
pub unsafe extern "C" fn memento_setting_plan_recent_files_add(
    items: *const MementoSettingTextViewC,
    items_len: u64,
    new_val: *const u16,
    new_val_len: u64,
    out_indices: *mut u64,
    out_len: *mut u64,
) -> bool {
    if out_len.is_null() {
        return false;
    }
    if items_len > 0 && items.is_null() {
        return false;
    }
    if new_val_len > 0 && new_val.is_null() {
        return false;
    }

    let new_val_slice = if new_val_len == 0 || new_val.is_null() {
        &[][..]
    } else {
        std::slice::from_raw_parts(new_val, new_val_len as usize)
    };

    let items_slice = if items_len == 0 || items.is_null() {
        &[][..]
    } else {
        std::slice::from_raw_parts(items, items_len as usize)
    };

    let plan = crate::models::plan_recent_files_add(items_slice, new_val_slice);

    *out_len = plan.len() as u64;
    if !out_indices.is_null() {
        for (i, &idx) in plan.iter().enumerate() {
            *out_indices.add(i) = idx as u64;
        }
    }

    true
}

// -----------------------------------------------------------------------------
// Keys and Enums query functions for parity testing
// -----------------------------------------------------------------------------

/// Looks up a string constant by name.
///
/// Returns a pointer to a null-terminated static C string, or null if unknown.
///
/// # Safety
/// `name` must be a valid null-terminated C string.
#[no_mangle]
pub unsafe extern "C" fn memento_setting_get_str_constant(name: *const c_char) -> *const c_char {
    if name.is_null() {
        return std::ptr::null();
    }
    let Ok(s) = CStr::from_ptr(name).to_str() else {
        return std::ptr::null();
    };
    match s {

        "Keys::Version::VERSION" => b"version\0".as_ptr() as *const c_char,
        "Keys::Window::GROUP" => b"main-window\0".as_ptr() as *const c_char,
        "Keys::Window::SUBTITLE_LIST" => b"subtitle-list\0".as_ptr() as *const c_char,
        "Keys::Window::SEARCH" => b"search\0".as_ptr() as *const c_char,
        "Keys::Internal::GROUP" => b"internal\0".as_ptr() as *const c_char,
        "Keys::Internal::AUTO_UPDATE_OPT_IN_SHOWN" => b"auto-update-opt-in-shown\0".as_ptr() as *const c_char,
        "Keys::Internal::WHISPER_NO_MODEL_PROMPT_DISMISSED" => b"whisper-no-model-prompt-dismissed\0".as_ptr() as *const c_char,
        "Keys::Recent::GROUP" => b"recent\0".as_ptr() as *const c_char,
        "Keys::Recent::FILES" => b"files\0".as_ptr() as *const c_char,
        "Keys::SubtitleList::GROUP" => b"subtitle-list\0".as_ptr() as *const c_char,
        "Keys::SubtitleList::IGNORE_WHITESPACE" => b"ignore-whitespace\0".as_ptr() as *const c_char,
        "Keys::SubtitleList::AUTO_SEEK" => b"auto-seek\0".as_ptr() as *const c_char,
        "Keys::AudioSource::GROUP" => b"audio-srcs\0".as_ptr() as *const c_char,
        "Keys::AudioSource::NAME" => b"name\0".as_ptr() as *const c_char,
        "Keys::AudioSource::NAME_DEFAULT" => b"JapanesePod101\0".as_ptr() as *const c_char,
        "Keys::AudioSource::URL" => b"url\0".as_ptr() as *const c_char,
        "Keys::AudioSource::URL_DEFAULT" => b"https://assets.languagepod101.com/dictionary/japanese/audiomp3.php?kanji={expression}&kana={reading}\0".as_ptr() as *const c_char,
        "Keys::AudioSource::TYPE" => b"type\0".as_ptr() as *const c_char,
        "Keys::AudioSource::MD5" => b"md5\0".as_ptr() as *const c_char,
        "Keys::AudioSource::MD5_DEFAULT" => b"7e2c2f954ef6051373ba916f000168dc\0".as_ptr() as *const c_char,
        "Keys::Application::GROUP" => b"application\0".as_ptr() as *const c_char,
        "Keys::Application::LANGUAGE" => b"language\0".as_ptr() as *const c_char,
        "Keys::Application::AUTO_UPDATE_CHECK" => b"auto-update-check\0".as_ptr() as *const c_char,
        "Keys::Behavior::GROUP" => b"behavior\0".as_ptr() as *const c_char,
        "Keys::Behavior::AUTO_FIT" => b"autofit\0".as_ptr() as *const c_char,
        "Keys::Behavior::AUTO_FIT_PERCENT" => b"autofit-percent\0".as_ptr() as *const c_char,
        "Keys::Behavior::OSC_MPV_CURSOR" => b"osc-mpv-cursor\0".as_ptr() as *const c_char,
        "Keys::Behavior::OSC_DURATION" => b"osc-duration\0".as_ptr() as *const c_char,
        "Keys::Behavior::OSC_FADE" => b"osc-fade\0".as_ptr() as *const c_char,
        "Keys::Behavior::OSC_MIN_MOVE" => b"osc-min-move\0".as_ptr() as *const c_char,
        "Keys::Behavior::OSC_PREVIEW_THUMBNAILS" => b"osc-preview-thumbnails\0".as_ptr() as *const c_char,
        "Keys::Behavior::SUBTITLE_PAUSE" => b"subtitle-pause\0".as_ptr() as *const c_char,
        "Keys::Behavior::SUBTITLE_CURSOR_SHOW" => b"subtitle-cursor-show\0".as_ptr() as *const c_char,
        "Keys::Behavior::SECONDARY_SUBTITLE_CURSOR_SHOW" => b"subtitle-cursor-show-secondary\0".as_ptr() as *const c_char,
        "Keys::Behavior::FILE_OPEN_DIR" => b"file-open-dir\0".as_ptr() as *const c_char,
        "Keys::Behavior::FILE_OPEN_CUSTOM" => b"file-open-custom\0".as_ptr() as *const c_char,
        "Keys::Dictionary::GROUP" => b"dictionary\0".as_ptr() as *const c_char,
        "Keys::Dictionary::ORDER" => b"order\0".as_ptr() as *const c_char,
        "Keys::Search::GROUP" => b"search\0".as_ptr() as *const c_char,
        "Keys::Search::Matcher::EXACT" => b"exact-matcher\0".as_ptr() as *const c_char,
        "Keys::Search::Matcher::DECONJ" => b"deconj-matcher\0".as_ptr() as *const c_char,
        "Keys::Search::Matcher::MECAB_IPADIC" => b"ipadic-matcher\0".as_ptr() as *const c_char,
        "Keys::Search::MIDDLE_MOUSE_SCAN" => b"middle-mouse-scan\0".as_ptr() as *const c_char,
        "Keys::Search::METHOD" => b"method\0".as_ptr() as *const c_char,
        "Keys::Search::DELAY" => b"delay\0".as_ptr() as *const c_char,
        "Keys::Search::MODIFIER" => b"modifier\0".as_ptr() as *const c_char,
        "Keys::Search::HIDE_MPV_SUBS" => b"hide-subs\0".as_ptr() as *const c_char,
        "Keys::Search::HIDE_SUBS" => b"hide-bar\0".as_ptr() as *const c_char,
        "Keys::Search::PAUSE_ON_HOVER" => b"hover-pause\0".as_ptr() as *const c_char,
        "Keys::Search::AUTO_PLAY_AUDIO" => b"auto-play-audio\0".as_ptr() as *const c_char,
        "Keys::Search::GLOSSARY_STYLE" => b"list-result\0".as_ptr() as *const c_char,
        "Keys::Search::REPLACE_NEWLINES" => b"replace-lines\0".as_ptr() as *const c_char,
        "Keys::Search::REPLACE_NEWLINES_WITH" => b"replace-with\0".as_ptr() as *const c_char,
        "Keys::Search::REPLACE_NEWLINES_WITH_DEFAULT" => b"\0".as_ptr() as *const c_char,
        "Keys::Search::REMOVE_REGEX" => b"remove-regex\0".as_ptr() as *const c_char,
        "Keys::Search::REMOVE_REGEX_DEFAULT" => b"\0".as_ptr() as *const c_char,
        "Keys::Search::SUBTITLE_FURIGANA" => b"subtitle-furigana\0".as_ptr() as *const c_char,
        "Keys::Search::SHOW_ANKI_GLOSSARY_CHECKBOX" => b"show-anki-glossary-checkbox\0".as_ptr() as *const c_char,
        "Keys::Interface::GROUP" => b"interface\0".as_ptr() as *const c_char,
        "Keys::Interface::SYSTEM_ICONS" => b"system-icons\0".as_ptr() as *const c_char,
        "Keys::Interface::Subtitle::FONT" => b"sub-font\0".as_ptr() as *const c_char,
        "Keys::Interface::Subtitle::LINE_SPACING" => b"sub-line-spacing\0".as_ptr() as *const c_char,
        "Keys::Interface::Subtitle::SCALE" => b"sub-scale\0".as_ptr() as *const c_char,
        "Keys::Interface::Subtitle::OFFSET" => b"sub-offset\0".as_ptr() as *const c_char,
        "Keys::Interface::Subtitle::STROKE" => b"sub-stroke\0".as_ptr() as *const c_char,
        "Keys::Interface::Subtitle::TEXT_COLOR" => b"sub-text-color\0".as_ptr() as *const c_char,
        "Keys::Interface::Subtitle::TEXT_COLOR_DEFAULT" => b"#FFFFFFFF\0".as_ptr() as *const c_char,
        "Keys::Interface::Subtitle::BACKGROUND_COLOR" => b"sub-bg-color\0".as_ptr() as *const c_char,
        "Keys::Interface::Subtitle::BACKGROUND_COLOR_DEFAULT" => b"#00000000\0".as_ptr() as *const c_char,
        "Keys::Interface::Subtitle::STROKE_COLOR" => b"sub-stroke-color\0".as_ptr() as *const c_char,
        "Keys::Interface::Subtitle::STROKE_COLOR_DEFAULT" => b"#FF000000\0".as_ptr() as *const c_char,
        "Keys::Interface::Subtitle::FURIGANA_RATIO" => b"sub-furigana-ratio\0".as_ptr() as *const c_char,
        "Keys::Interface::POPUP_WIDTH" => b"sub-popup-width\0".as_ptr() as *const c_char,
        "Keys::Interface::POPUP_HEIGHT" => b"sub-popup-height\0".as_ptr() as *const c_char,
        "Keys::Interface::SEARCH_WINDOW" => b"search-window\0".as_ptr() as *const c_char,
        "Keys::Interface::SEARCH_EXPRESSION_FONT" => b"search-expression-font\0".as_ptr() as *const c_char,
        "Keys::Interface::SEARCH_READING_FONT" => b"search-reading-font\0".as_ptr() as *const c_char,
        "Keys::Interface::SEARCH_CONJ_FONT" => b"search-conj-font\0".as_ptr() as *const c_char,
        "Keys::Interface::SEARCH_TAG_FONT" => b"search-tag-font\0".as_ptr() as *const c_char,
        "Keys::Interface::SEARCH_GLOSSARY_FONT" => b"search-glossary-font\0".as_ptr() as *const c_char,
        "Keys::Interface::SEARCH_KANJI_FONT" => b"search-kanji-font\0".as_ptr() as *const c_char,
        "Keys::Interface::SubtitleList::WINDOW" => b"sub-list-window\0".as_ptr() as *const c_char,
        "Keys::Interface::SubtitleList::TIMESTAMPS" => b"sub-list-timestamps\0".as_ptr() as *const c_char,
        "Keys::Interface::SubtitleList::BACKGROUND_COLOR" => b"sub-list-background-color\0".as_ptr() as *const c_char,
        "Keys::Interface::SubtitleList::BACKGROUND_COLOR_DEFAULT" => b"#FF000000\0".as_ptr() as *const c_char,
        "Keys::Interface::SubtitleList::TEXT_COLOR" => b"sub-list-text-color\0".as_ptr() as *const c_char,
        "Keys::Interface::SubtitleList::TEXT_COLOR_DEFAULT" => b"#FFFFFFFF\0".as_ptr() as *const c_char,
        "Keys::Interface::SubtitleList::HOVER_COLOR" => b"sub-list-hover-color\0".as_ptr() as *const c_char,
        "Keys::Interface::SubtitleList::HOVER_COLOR_DEFAULT" => b"#FF1A3B5C\0".as_ptr() as *const c_char,
        "Keys::Interface::SubtitleList::SELECTED_COLOR" => b"sub-list-selected-color\0".as_ptr() as *const c_char,
        "Keys::Interface::SubtitleList::SELECTED_COLOR_DEFAULT" => b"#FF295A8A\0".as_ptr() as *const c_char,
        "Keys::Interface::SubtitleList::SELECTED_HOVER_COLOR" => b"sub-list-selected-hover-color\0".as_ptr() as *const c_char,
        "Keys::Interface::SubtitleList::SELECTED_HOVER_COLOR_DEFAULT" => b"#FF3F576E\0".as_ptr() as *const c_char,
        "Keys::Interface::SubtitleList::PRIMARY_FONT" => b"sub-list-primary-font\0".as_ptr() as *const c_char,
        "Keys::Interface::SubtitleList::SECONDARY_FONT" => b"sub-list-secondary-font\0".as_ptr() as *const c_char,
        "Keys::Keybind::GROUP" => b"keybind\0".as_ptr() as *const c_char,
        "Keys::Keybind::CURRENT_PROFILE" => b"current\0".as_ptr() as *const c_char,
        "Keys::Keybind::Profile::GROUP" => b"profiles\0".as_ptr() as *const c_char,
        "Keys::Keybind::Profile::CARD_ADD" => b"card-add\0".as_ptr() as *const c_char,
        "Keys::Keybind::Profile::CARD_NEXT" => b"card-next\0".as_ptr() as *const c_char,
        "Keys::Keybind::Profile::CARD_PREVIOUS" => b"card-prev\0".as_ptr() as *const c_char,
        "Keys::Keybind::Profile::OPEN_FILE" => b"open-file\0".as_ptr() as *const c_char,
        "Keys::Keybind::Profile::OPEN_URL" => b"open-url\0".as_ptr() as *const c_char,
        "Keys::Keybind::Profile::OSC_VISIBILITY" => b"osc-visibility\0".as_ptr() as *const c_char,
        "Keys::Keybind::Profile::SHOW_SEARCH" => b"show-search\0".as_ptr() as *const c_char,
        "Keys::Keybind::Profile::SHOW_SUBTITLE_LIST" => b"show-sub-list\0".as_ptr() as *const c_char,
        "Keys::Keybind::Profile::START_OCR" => b"start-ocr\0".as_ptr() as *const c_char,
        "Keys::Keybind::Profile::SUBTITLE_AUTO_PAUSE" => b"sub-auto-pause\0".as_ptr() as *const c_char,
        "Keys::Keybind::Profile::SUBTITLE_DECREASE_SIZE" => b"sub-decrease-size\0".as_ptr() as *const c_char,
        "Keys::Keybind::Profile::SUBTITLE_INCREASE_SIZE" => b"sub-increase-size\0".as_ptr() as *const c_char,
        "Keys::Keybind::Profile::SUBTITLE_MOVE_DOWN" => b"sub-move-down\0".as_ptr() as *const c_char,
        "Keys::Keybind::Profile::SUBTITLE_MOVE_UP" => b"sub-move-up\0".as_ptr() as *const c_char,
        "Keys::Keybind::Profile::SUBTITLE_SHOW" => b"sub-show\0".as_ptr() as *const c_char,
        "Keys::Keybind::Profile::SUBTITLE_FURIGANA" => b"sub-furigana\0".as_ptr() as *const c_char,
        "Keys::Keybind::Profile::SUBTITLE_LIST_COPY_SELECTED" => b"sub-list-copy-selected\0".as_ptr() as *const c_char,
        "Keys::Keybind::Profile::SUBTITLE_LIST_FIND" => b"sub-list-find\0".as_ptr() as *const c_char,
        "Keys::Keybind::Profile::SUBTITLE_LIST_FIND_NEXT" => b"sub-list-find-next\0".as_ptr() as *const c_char,
        "Keys::Keybind::Profile::SUBTITLE_LIST_FIND_PREVIOUS" => b"sub-list-find-prev\0".as_ptr() as *const c_char,
        "Keys::Ocr::GROUP" => b"ocr\0".as_ptr() as *const c_char,
        "Keys::Ocr::ENABLED" => b"enable\0".as_ptr() as *const c_char,
        "Keys::Ocr::USE_GPU" => b"use-gpu\0".as_ptr() as *const c_char,
        "Keys::Ocr::MODEL" => b"model\0".as_ptr() as *const c_char,
        "Keys::Ocr::MODEL_DEFAULT" => b"kha-white/manga-ocr-base\0".as_ptr() as *const c_char,
        "Keys::Asr::GROUP" => b"asr\0".as_ptr() as *const c_char,
        "Keys::Asr::ENABLED" => b"enable\0".as_ptr() as *const c_char,
        "Keys::Asr::BACKEND" => b"backend\0".as_ptr() as *const c_char,
        "Keys::Asr::BACKEND_WHISPER" => b"whisper\0".as_ptr() as *const c_char,
        "Keys::Asr::BACKEND_ONNX" => b"onnx\0".as_ptr() as *const c_char,
        "Keys::Asr::START_FROM_BEGINNING" => b"start-from-beginning\0".as_ptr() as *const c_char,
        "Keys::Whisper::GROUP" => b"whisper\0".as_ptr() as *const c_char,
        "Keys::Whisper::ENABLED" => b"enable\0".as_ptr() as *const c_char,
        "Keys::Whisper::MODEL" => b"model\0".as_ptr() as *const c_char,
        "Keys::Whisper::MODEL_DEFAULT" => b"base\0".as_ptr() as *const c_char,
        "Keys::Whisper::CUSTOM_MODEL" => b"custom-model\0".as_ptr() as *const c_char,
        "Keys::Whisper::CUSTOM_MODEL_DEFAULT" => b"\0".as_ptr() as *const c_char,
        "Keys::Whisper::VAD_ENABLED" => b"vad-enable\0".as_ptr() as *const c_char,
        "Keys::Whisper::VAD_MODEL" => b"vad-model\0".as_ptr() as *const c_char,
        "Keys::Whisper::VAD_MODEL_DEFAULT" => b"ggml-silero-v6.2.0.bin\0".as_ptr() as *const c_char,
        "Keys::Whisper::USE_GPU" => b"use-gpu\0".as_ptr() as *const c_char,
        "Keys::Whisper::GPU_DEVICE" => b"gpu-device\0".as_ptr() as *const c_char,
        "Keys::Whisper::THREADS" => b"threads\0".as_ptr() as *const c_char,
        "Keys::Whisper::BEST_OF" => b"best-of\0".as_ptr() as *const c_char,
        "Keys::Whisper::BEAM_SIZE" => b"beam-size\0".as_ptr() as *const c_char,
        "Keys::Whisper::FLASH_ATTN" => b"flash-attn\0".as_ptr() as *const c_char,
        "Keys::Whisper::SUPPRESS_REGEX" => b"suppress-regex\0".as_ptr() as *const c_char,
        "Keys::Whisper::SUPPRESS_REGEX_DEFAULT" => b"^\\[_extra_token_[0-9]+\\]$\0".as_ptr() as *const c_char,
        "Keys::Onnx::GROUP" => b"onnx\0".as_ptr() as *const c_char,
        "Keys::Onnx::MODEL" => b"model\0".as_ptr() as *const c_char,
        "Keys::Onnx::MODEL_DEFAULT" => b"kitsune-transcribe-t0.6b-ja\0".as_ptr() as *const c_char,
        "Keys::Onnx::CUSTOM_MODEL" => b"custom-model\0".as_ptr() as *const c_char,
        "Keys::Onnx::CUSTOM_MODEL_DEFAULT" => b"\0".as_ptr() as *const c_char,
        "Keys::Onnx::USE_GPU" => b"use-gpu\0".as_ptr() as *const c_char,
        "Keys::Onnx::GPU_DEVICE" => b"gpu-device\0".as_ptr() as *const c_char,
        "Keys::Onnx::THREADS" => b"threads\0".as_ptr() as *const c_char,
        "Keys::Onnx::PUNCTUATION" => b"punctuation\0".as_ptr() as *const c_char,
        _ => std::ptr::null(),
    }
}

/// Looks up a boolean constant by name.
///
/// # Safety
/// `name` must be a valid null-terminated C string, and `out` must be a writable bool pointer.
#[no_mangle]
pub unsafe extern "C" fn memento_setting_get_bool_constant(
    name: *const c_char,
    out: *mut bool,
) -> bool {
    if name.is_null() || out.is_null() {
        return false;
    }
    let Ok(s) = CStr::from_ptr(name).to_str() else {
        return false;
    };
    use crate::keys::*;
    let val = match s {
        "Keys::SubtitleList::IGNORE_WHITESPACE_DEFAULT" => subtitle_list::IGNORE_WHITESPACE_DEFAULT,
        "Keys::SubtitleList::AUTO_SEEK_DEFAULT" => subtitle_list::AUTO_SEEK_DEFAULT,
        "Keys::Application::AUTO_UPDATE_CHECK_DEFAULT" => application::AUTO_UPDATE_CHECK_DEFAULT,
        "Keys::Behavior::AUTO_FIT_DEFAULT" => behavior::AUTO_FIT_DEFAULT,
        "Keys::Behavior::OSC_MPV_CURSOR_DEFAULT" => behavior::OSC_MPV_CURSOR_DEFAULT,
        "Keys::Behavior::OSC_PREVIEW_THUMBNAILS_DEFAULT" => behavior::OSC_PREVIEW_THUMBNAILS_DEFAULT,
        "Keys::Behavior::SUBTITLE_PAUSE_DEFAULT" => behavior::SUBTITLE_PAUSE_DEFAULT,
        "Keys::Behavior::SUBTITLE_CURSOR_SHOW_DEFAULT" => behavior::SUBTITLE_CURSOR_SHOW_DEFAULT,
        "Keys::Behavior::SECONDARY_SUBTITLE_CURSOR_SHOW_DEFAULT" => behavior::SECONDARY_SUBTITLE_CURSOR_SHOW_DEFAULT,
        "Keys::Search::Matcher::EXACT_DEFAULT" => search::matcher::EXACT_DEFAULT,
        "Keys::Search::Matcher::DECONJ_DEFAULT" => search::matcher::DECONJ_DEFAULT,
        "Keys::Search::Matcher::MECAB_IPADIC_DEFAULT" => search::matcher::MECAB_IPADIC_DEFAULT,
        "Keys::Search::MIDDLE_MOUSE_SCAN_DEFAULT" => search::MIDDLE_MOUSE_SCAN_DEFAULT,
        "Keys::Search::HIDE_MPV_SUBS_DEFAULT" => search::HIDE_MPV_SUBS_DEFAULT,
        "Keys::Search::HIDE_SUBS_DEFAULT" => search::HIDE_SUBS_DEFAULT,
        "Keys::Search::PAUSE_ON_HOVER_DEFAULT" => search::PAUSE_ON_HOVER_DEFAULT,
        "Keys::Search::AUTO_PLAY_AUDIO_DEFAULT" => search::AUTO_PLAY_AUDIO_DEFAULT,
        "Keys::Search::REPLACE_NEWLINES_DEFAULT" => search::REPLACE_NEWLINES_DEFAULT,
        "Keys::Search::SUBTITLE_FURIGANA_DEFAULT" => search::SUBTITLE_FURIGANA_DEFAULT,
        "Keys::Search::SHOW_ANKI_GLOSSARY_CHECKBOX_DEFAULT" => search::SHOW_ANKI_GLOSSARY_CHECKBOX_DEFAULT,
        "Keys::Interface::SYSTEM_ICONS_DEFAULT" => interface::SYSTEM_ICONS_DEFAULT,
        "Keys::Interface::SEARCH_WINDOW_DEFAULT" => interface::SEARCH_WINDOW_DEFAULT,
        "Keys::Interface::SubtitleList::WINDOW_DEFAULT" => interface::subtitle_list::WINDOW_DEFAULT,
        "Keys::Interface::SubtitleList::TIMESTAMPS_DEFAULT" => interface::subtitle_list::TIMESTAMPS_DEFAULT,
        "Keys::Ocr::ENABLED_DEFAULT" => ocr::ENABLED_DEFAULT,
        "Keys::Ocr::USE_GPU_DEFAULT" => ocr::USE_GPU_DEFAULT,
        "Keys::Asr::ENABLED_DEFAULT" => asr::ENABLED_DEFAULT,
        "Keys::Asr::START_FROM_BEGINNING_DEFAULT" => asr::START_FROM_BEGINNING_DEFAULT,
        "Keys::Whisper::ENABLED_DEFAULT" => whisper::ENABLED_DEFAULT,
        "Keys::Whisper::VAD_ENABLED_DEFAULT" => whisper::VAD_ENABLED_DEFAULT,
        "Keys::Whisper::USE_GPU_DEFAULT" => whisper::USE_GPU_DEFAULT,
        "Keys::Whisper::FLASH_ATTN_DEFAULT" => whisper::FLASH_ATTN_DEFAULT,
        "Keys::Onnx::USE_GPU_DEFAULT" => onnx::USE_GPU_DEFAULT,
        "Keys::Onnx::PUNCTUATION_DEFAULT" => onnx::PUNCTUATION_DEFAULT,
        _ => return false,
    };
    *out = val;
    true
}

/// Looks up an integer (i32) constant by name.
///
/// # Safety
/// `name` must be a valid null-terminated C string, and `out` must be a writable i32 pointer.
#[no_mangle]
pub unsafe extern "C" fn memento_setting_get_i32_constant(
    name: *const c_char,
    out: *mut i32,
) -> bool {
    if name.is_null() || out.is_null() {
        return false;
    }
    let Ok(s) = CStr::from_ptr(name).to_str() else {
        return false;
    };
    use crate::keys::*;
    let val = match s {
        "Keys::Behavior::AUTO_FIT_PERCENT_DEFAULT" => behavior::AUTO_FIT_PERCENT_DEFAULT,
        "Keys::Behavior::OSC_DURATION_DEFAULT" => behavior::OSC_DURATION_DEFAULT,
        "Keys::Behavior::OSC_FADE_DEFAULT" => behavior::OSC_FADE_DEFAULT,
        "Keys::Behavior::OSC_MIN_MOVE_DEFAULT" => behavior::OSC_MIN_MOVE_DEFAULT,
        "Keys::Search::DELAY_DEFAULT" => search::DELAY_DEFAULT,
        "Keys::Interface::POPUP_WIDTH_DEFAULT" => interface::POPUP_WIDTH_DEFAULT,
        "Keys::Interface::POPUP_HEIGHT_DEFAULT" => interface::POPUP_HEIGHT_DEFAULT,
        "Keys::Whisper::GPU_DEVICE_DEFAULT" => whisper::GPU_DEVICE_DEFAULT,
        "Keys::Whisper::THREADS_DEFAULT" => whisper::THREADS_DEFAULT,
        "Keys::Whisper::BEST_OF_DEFAULT" => whisper::BEST_OF_DEFAULT,
        "Keys::Whisper::BEAM_SIZE_DEFAULT" => whisper::BEAM_SIZE_DEFAULT,
        "Keys::Onnx::GPU_DEVICE_DEFAULT" => onnx::GPU_DEVICE_DEFAULT,
        "Keys::Onnx::THREADS_DEFAULT" => onnx::THREADS_DEFAULT,
        _ => return false,
    };
    *out = val;
    true
}

/// Looks up an unsigned integer (u32) constant by name.
///
/// # Safety
/// `name` must be a valid null-terminated C string, and `out` must be a writable u32 pointer.
#[no_mangle]
pub unsafe extern "C" fn memento_setting_get_u32_constant(
    name: *const c_char,
    out: *mut u32,
) -> bool {
    if name.is_null() || out.is_null() {
        return false;
    }
    let Ok(s) = CStr::from_ptr(name).to_str() else {
        return false;
    };
    use crate::keys::*;
    let val = match s {
        "Keys::Version::CURRENT" => version::CURRENT,
        _ => return false,
    };
    *out = val;
    true
}

/// Looks up a floating-point (f64) constant by name.
///
/// # Safety
/// `name` must be a valid null-terminated C string, and `out` must be a writable f64 pointer.
#[no_mangle]
pub unsafe extern "C" fn memento_setting_get_f64_constant(
    name: *const c_char,
    out: *mut f64,
) -> bool {
    if name.is_null() || out.is_null() {
        return false;
    }
    let Ok(s) = CStr::from_ptr(name).to_str() else {
        return false;
    };
    use crate::keys::*;
    let val = match s {
        "Keys::Interface::Subtitle::LINE_SPACING_DEFAULT" => interface::subtitle::LINE_SPACING_DEFAULT,
        "Keys::Interface::Subtitle::SCALE_DEFAULT" => interface::subtitle::SCALE_DEFAULT,
        "Keys::Interface::Subtitle::OFFSET_DEFAULT" => interface::subtitle::OFFSET_DEFAULT,
        "Keys::Interface::Subtitle::STROKE_DEFAULT" => interface::subtitle::STROKE_DEFAULT,
        "Keys::Interface::Subtitle::FURIGANA_RATIO_DEFAULT" => interface::subtitle::FURIGANA_RATIO_DEFAULT,
        _ => return false,
    };
    *out = val;
    true
}

/// Looks up an enum discriminant value by fully qualified name.
///
/// # Safety
/// `name` must be a valid null-terminated C string, and `out` must be a writable i32 pointer.
#[no_mangle]
pub unsafe extern "C" fn memento_setting_get_enum_variant(
    name: *const c_char,
    out: *mut i32,
) -> bool {
    if name.is_null() || out.is_null() {
        return false;
    }
    let Ok(s) = CStr::from_ptr(name).to_str() else {
        return false;
    };
    use crate::enums::*;
    let val = match s {
        "Setting::AudioSourceType::AudioSourceTypeFile" => AudioSourceType::AudioSourceTypeFile as i32,
        "Setting::AudioSourceType::AudioSourceTypeJson" => AudioSourceType::AudioSourceTypeJson as i32,
        "Setting::Directory::DirectoryCurrent" => Directory::DirectoryCurrent as i32,
        "Setting::Directory::DirectoryHome" => Directory::DirectoryHome as i32,
        "Setting::Directory::DirectoryMovies" => Directory::DirectoryMovies as i32,
        "Setting::Directory::DirectoryDocuments" => Directory::DirectoryDocuments as i32,
        "Setting::Directory::DirectoryCustom" => Directory::DirectoryCustom as i32,
        "Setting::GlossaryStyle::GlossaryStyleBullet" => GlossaryStyle::GlossaryStyleBullet as i32,
        "Setting::GlossaryStyle::GlossaryStyleLineBreak" => GlossaryStyle::GlossaryStyleLineBreak as i32,
        "Setting::GlossaryStyle::GlossaryStylePipe" => GlossaryStyle::GlossaryStylePipe as i32,
        "Setting::Language::LanguageSystem" => Language::LanguageSystem as i32,
        "Setting::Language::LanguageEnglish" => Language::LanguageEnglish as i32,
        "Setting::Language::LanguageKorean" => Language::LanguageKorean as i32,
        "Setting::Language::LanguageChineseSimplified" => Language::LanguageChineseSimplified as i32,
        "Setting::Language::LanguageChineseTraditional" => Language::LanguageChineseTraditional as i32,
        "Setting::Modifier::ModifierAlt" => Modifier::ModifierAlt as i32,
        "Setting::Modifier::ModifierControl" => Modifier::ModifierControl as i32,
        "Setting::Modifier::ModifierShift" => Modifier::ModifierShift as i32,
        "Setting::Modifier::ModifierSuper" => Modifier::ModifierSuper as i32,
        "Setting::SearchMethod::SearchMethodHover" => SearchMethod::SearchMethodHover as i32,
        "Setting::SearchMethod::SearchMethodModifier" => SearchMethod::SearchMethodModifier as i32,
        // Enum-typed defaults in Keys:
        "Keys::AudioSource::TYPE_DEFAULT" => crate::keys::audio_source::TYPE_DEFAULT as i32,
        "Keys::Application::LANGUAGE_DEFAULT" => crate::keys::application::LANGUAGE_DEFAULT as i32,
        "Keys::Behavior::FILE_OPEN_DIR_DEFAULT" => crate::keys::behavior::FILE_OPEN_DIR_DEFAULT as i32,
        "Keys::Search::METHOD_DEFAULT" => crate::keys::search::METHOD_DEFAULT as i32,
        "Keys::Search::MODIFIER_DEFAULT" => crate::keys::search::MODIFIER_DEFAULT as i32,
        "Keys::Search::GLOSSARY_STYLE_DEFAULT" => crate::keys::search::GLOSSARY_STYLE_DEFAULT as i32,
        _ => return false,
    };
    *out = val;
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CString;

    #[test]
    fn test_capi_migration_plan() {
        let mut plan = MementoMigrationPlanC {
            already_current: false,
            newer_version_warning: false,
            run_1_to_2: false,
            run_2_to_3: false,
            run_3_to_4: false,
            run_4_to_5: false,
            run_5_to_6: false,
            target_version: 0,
        };
        unsafe {
            memento_setting_migration_plan(1, &mut plan);
        }
        assert!(plan.run_1_to_2);
        assert_eq!(plan.target_version, 6);
    }

    #[test]
    fn test_capi_migrate_2_to_3() {
        assert_eq!(memento_setting_migrate_2_to_3_list_result(true), 0);
        assert_eq!(memento_setting_migrate_2_to_3_list_result(false), 1);
    }

    #[test]
    fn test_capi_normalize_writable_path() {
        let input: Vec<u16> = "/tmp/memento/memento".encode_utf16().collect();
        let mut out_ptr: *mut u16 = std::ptr::null_mut();
        let mut out_len: u64 = 0;
        unsafe {
            assert!(memento_setting_normalize_writable_path(
                input.as_ptr(),
                input.len() as u64,
                &mut out_ptr,
                &mut out_len
            ));
            assert!(!out_ptr.is_null());
            assert_eq!(out_len, "/tmp/memento/".len() as u64);
            let slice = std::slice::from_raw_parts(out_ptr, out_len as usize);
            assert_eq!(String::from_utf16_lossy(slice), "/tmp/memento/");
            memento_setting_free_u16(out_ptr, out_len);
        }
    }

    #[test]
    fn test_capi_asr_backend_default() {
        let mut ptr: *const c_char = std::ptr::null();
        let mut len: u64 = 0;
        unsafe {
            memento_setting_asr_backend_default(true, true, &mut ptr, &mut len);
            assert_eq!(CStr::from_ptr(ptr).to_str().unwrap(), "whisper");

            memento_setting_asr_backend_default(false, true, &mut ptr, &mut len);
            assert_eq!(CStr::from_ptr(ptr).to_str().unwrap(), "onnx");
        }
    }

    #[test]
    fn test_capi_constants_and_enums() {
        unsafe {
            let name = CString::new("Keys::Window::GROUP").unwrap();
            let val = memento_setting_get_str_constant(name.as_ptr());
            assert_eq!(CStr::from_ptr(val).to_str().unwrap(), "main-window");

            let name = CString::new("Keys::Behavior::AUTO_FIT_DEFAULT").unwrap();
            let mut b = false;
            assert!(memento_setting_get_bool_constant(name.as_ptr(), &mut b));
            assert!(b);

            let name = CString::new("Keys::Version::CURRENT").unwrap();
            let mut u = 0u32;
            assert!(memento_setting_get_u32_constant(name.as_ptr(), &mut u));
            assert_eq!(u, 6);

            let name = CString::new("Setting::GlossaryStyle::GlossaryStyleBullet").unwrap();
            let mut e = -1;
            assert!(memento_setting_get_enum_variant(name.as_ptr(), &mut e));
            assert_eq!(e, 0);
        }
    }

    #[test]
    fn test_capi_models() {
        unsafe {
            // Audio source move
            let mut dest = 0;
            assert!(memento_setting_audio_source_plan_move(0, 2, 4, &mut dest));
            assert_eq!(dest, 3);
            assert!(memento_setting_audio_source_plan_move(3, 1, 4, &mut dest));
            assert_eq!(dest, 1);
            assert!(!memento_setting_audio_source_plan_move(1, 1, 4, &mut dest));
            assert!(!memento_setting_audio_source_plan_move(0, 5, 4, &mut dest));
            assert!(!memento_setting_audio_source_plan_move(0, 1, 4, std::ptr::null_mut()));

            // Profile lower bound
            let name_a: Vec<u16> = "Alpha".encode_utf16().collect();
            let name_b: Vec<u16> = "Beta".encode_utf16().collect();
            let name_d: Vec<u16> = "Delta".encode_utf16().collect();
            let names = [
                MementoSettingTextViewC { data: name_a.as_ptr(), len: name_a.len() as u64 },
                MementoSettingTextViewC { data: name_b.as_ptr(), len: name_b.len() as u64 },
                MementoSettingTextViewC { data: name_d.as_ptr(), len: name_d.len() as u64 },
            ];

            let target_beta: Vec<u16> = "Beta".encode_utf16().collect();
            let mut idx = 0u64;
            let mut exact = false;
            assert!(memento_setting_profile_lower_bound(
                names.as_ptr(),
                names.len() as u64,
                target_beta.as_ptr(),
                target_beta.len() as u64,
                &mut idx,
                &mut exact,
            ));
            assert_eq!(idx, 1);
            assert!(exact);

            let target_c: Vec<u16> = "Charlie".encode_utf16().collect();
            assert!(memento_setting_profile_lower_bound(
                names.as_ptr(),
                names.len() as u64,
                target_c.as_ptr(),
                target_c.len() as u64,
                &mut idx,
                &mut exact,
            ));
            assert_eq!(idx, 2);
            assert!(!exact);

            // Null out_index fails
            assert!(!memento_setting_profile_lower_bound(
                names.as_ptr(),
                names.len() as u64,
                target_c.as_ptr(),
                target_c.len() as u64,
                std::ptr::null_mut(),
                &mut exact,
            ));

            // Download manager
            assert_eq!(memento_setting_download_speed(1000, 500), 2000);
            let mut progress = 0.0;
            let changed = memento_setting_download_progress_update(
                0.0, 0, 0, 0,
                50, 100, 2000,
                &mut progress,
            );
            assert!(changed);
            assert_eq!(progress, 0.5);

            let mut out_p = 0.0;
            let mut out_s = 0i64;
            let mut out_c = false;
            assert!(memento_setting_download_progress_calc(
                50, 100, 500,
                0.5, 50, 100, 2000,
                &mut out_p,
                &mut out_s,
                &mut out_c,
            ));
            assert_eq!(out_p, 0.5);
            assert_eq!(out_s, 100); // 50 * 1000 / 500 = 100
            assert!(out_c); // speed changed from 2000 to 100

            // Keybind sequence plan
            assert_eq!(memento_setting_plan_keybind_sequence(true, false), 0);
            assert_eq!(memento_setting_plan_keybind_sequence(true, true), 0);
            assert_eq!(memento_setting_plan_keybind_sequence(false, true), 1);
            assert_eq!(memento_setting_plan_keybind_sequence(false, false), 2);

            // ASR backend resolution
            let whisper_u16: Vec<u16> = "whisper".encode_utf16().collect();
            let onnx_u16: Vec<u16> = "onnx".encode_utf16().collect();
            let mut out_backend: *const c_char = std::ptr::null();
            let mut out_len = 0u64;

            assert!(memento_setting_resolve_asr_backend(
                true, true,
                whisper_u16.as_ptr(), whisper_u16.len() as u64,
                &mut out_backend, &mut out_len,
            ));
            assert_eq!(CStr::from_ptr(out_backend).to_str().unwrap(), "whisper");
            assert_eq!(out_len, 7);

            assert!(memento_setting_resolve_asr_backend(
                true, true,
                onnx_u16.as_ptr(), onnx_u16.len() as u64,
                &mut out_backend, &mut out_len,
            ));
            assert_eq!(CStr::from_ptr(out_backend).to_str().unwrap(), "onnx");
            assert_eq!(out_len, 4);

            // Whisper model resolution
            let tiny_u16: Vec<u16> = "tiny".encode_utf16().collect();
            let bad_u16: Vec<u16> = "invalid-model".encode_utf16().collect();
            let mut use_default = true;

            assert!(memento_setting_resolve_whisper_model(
                tiny_u16.as_ptr(), tiny_u16.len() as u64,
                &mut use_default,
            ));
            assert!(!use_default);

            assert!(memento_setting_resolve_whisper_model(
                bad_u16.as_ptr(), bad_u16.len() as u64,
                &mut use_default,
            ));
            assert!(use_default);

            // ONNX model resolution
            let onnx_model_u16: Vec<u16> = "kitsune-transcribe-t0.6b-ja".encode_utf16().collect();
            assert!(memento_setting_resolve_onnx_model(
                onnx_model_u16.as_ptr(), onnx_model_u16.len() as u64,
                &mut use_default,
            ));
            assert!(!use_default);

            assert!(memento_setting_resolve_onnx_model(
                bad_u16.as_ptr(), bad_u16.len() as u64,
                &mut use_default,
            ));
            assert!(use_default);

            // Clamping functions
            assert_eq!(memento_setting_clamp_gpu_device(-1), 0);
            assert_eq!(memento_setting_clamp_gpu_device(10), 10);
            assert_eq!(memento_setting_clamp_gpu_device(20), 15);

            assert_eq!(memento_setting_whisper_max_threads(0), 1);
            assert_eq!(memento_setting_whisper_max_threads(8), 8);

            assert_eq!(memento_setting_clamp_threads(0, 8), 4);
            assert_eq!(memento_setting_clamp_threads(10, 8), 8);

            assert_eq!(memento_setting_clamp_beam_or_best_of(0), 1);
            assert_eq!(memento_setting_clamp_beam_or_best_of(5), 5);
            assert_eq!(memento_setting_clamp_beam_or_best_of(15), 10);

            // Recent files add C ABI
            let f1: Vec<u16> = "file1".encode_utf16().collect();
            let f2: Vec<u16> = "file2".encode_utf16().collect();
            let f3: Vec<u16> = "file3".encode_utf16().collect();
            let views = [
                MementoSettingTextViewC { data: f1.as_ptr(), len: f1.len() as u64 },
                MementoSettingTextViewC { data: f2.as_ptr(), len: f2.len() as u64 },
                MementoSettingTextViewC { data: f3.as_ptr(), len: f3.len() as u64 },
            ];

            let mut out_indices = [0u64; 10];
            let mut out_len = 0u64;

            // Prepend new item "file0": surviving indices should be [0, 1, 2]
            let f0: Vec<u16> = "file0".encode_utf16().collect();
            assert!(memento_setting_plan_recent_files_add(
                views.as_ptr(),
                views.len() as u64,
                f0.as_ptr(),
                f0.len() as u64,
                out_indices.as_mut_ptr(),
                &mut out_len,
            ));
            assert_eq!(out_len, 3);
            assert_eq!(&out_indices[..3], &[0, 1, 2]);

            // Query length with null out_indices
            let mut query_len = 0u64;
            assert!(memento_setting_plan_recent_files_add(
                views.as_ptr(),
                views.len() as u64,
                f0.as_ptr(),
                f0.len() as u64,
                std::ptr::null_mut(),
                &mut query_len,
            ));
            assert_eq!(query_len, 3);

            // Prepend existing item "file2": surviving indices should be [0, 2]
            assert!(memento_setting_plan_recent_files_add(
                views.as_ptr(),
                views.len() as u64,
                f2.as_ptr(),
                f2.len() as u64,
                out_indices.as_mut_ptr(),
                &mut out_len,
            ));
            assert_eq!(out_len, 2);
            assert_eq!(&out_indices[..2], &[0, 2]);

            // Empty input
            assert!(memento_setting_plan_recent_files_add(
                std::ptr::null(),
                0,
                f0.as_ptr(),
                f0.len() as u64,
                out_indices.as_mut_ptr(),
                &mut out_len,
            ));
            assert_eq!(out_len, 0);

            // Null out_len fails
            assert!(!memento_setting_plan_recent_files_add(
                views.as_ptr(),
                views.len() as u64,
                f0.as_ptr(),
                f0.len() as u64,
                out_indices.as_mut_ptr(),
                std::ptr::null_mut(),
            ));
        }
    }
}

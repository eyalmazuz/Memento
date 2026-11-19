//! Single-pass WebVTT serializer and in-memory `memento-asr://` `mpv_stream_cb` callbacks.

use std::cmp::Ordering;
use std::ffi::{c_char, c_int, c_void, CStr};
use std::fmt::Write as _;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::ptr;
use std::sync::{Arc, RwLock};

use super::ffi;
use super::types::AsrCue;

#[inline]
pub(super) fn cue_cmp(a: &AsrCue, b: &AsrCue) -> Ordering {
    a.start
        .total_cmp(&b.start)
        .then_with(|| a.end.total_cmp(&b.end))
        .then_with(|| a.id.cmp(&b.id))
}

#[inline]
fn write_webvtt_timestamp_ms(out: &mut String, total_ms: u64) {
    let ms = total_ms % 1000;
    let total_secs = total_ms / 1000;
    let secs = total_secs % 60;
    let mins = (total_secs / 60) % 60;
    let hours = total_secs / 3600;
    let _ = write!(out, "{hours:02}:{mins:02}:{secs:02}.{ms:03}");
}

/// Format a non-negative timestamp in seconds as WebVTT `HH:MM:SS.mmm`.
pub fn format_webvtt_timestamp(seconds: f64) -> String {
    let total_ms = if seconds.is_finite() && seconds > 0.0 {
        (seconds * 1000.0).round() as u64
    } else {
        0
    };
    let mut out = String::with_capacity(12);
    write_webvtt_timestamp_ms(&mut out, total_ms);
    out
}

/// Write sanitized WebVTT cue text directly into `out` in a single pass with
/// zero intermediate allocations. Returns `true` if at least one non-empty line
/// was written.
fn write_sanitized_webvtt_text(out: &mut String, text: &str) -> bool {
    let bytes = text.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    let mut wrote_any = false;

    while i < len {
        let line_start = i;
        while i < len && bytes[i] != b'\n' && bytes[i] != b'\r' {
            i += 1;
        }
        let line = &text[line_start..i];
        if i < len {
            if bytes[i] == b'\r' && i + 1 < len && bytes[i + 1] == b'\n' {
                i += 2;
            } else {
                i += 1;
            }
        }

        // Strip trailing NULs and whitespace so lines that become empty once
        // NULs are removed are dropped identically to `replace('\0', "")` +
        // `trim_end()`.
        let trimmed = line.trim_end_matches(|c: char| c == '\0' || c.is_whitespace());
        if trimmed.is_empty() {
            continue;
        }
        if wrote_any {
            out.push('\n');
        }
        wrote_any = true;
        for ch in trimmed.chars() {
            match ch {
                '\0' => {}
                '&' => out.push_str("&amp;"),
                '<' => out.push_str("&lt;"),
                '>' => out.push_str("&gt;"),
                other => out.push(other),
            }
        }
    }

    wrote_any
}

#[inline]
fn write_webvtt_cue(out: &mut String, cue: &AsrCue) {
    if !(cue.start.is_finite() && cue.end.is_finite() && cue.start >= 0.0 && cue.end > cue.start) {
        return;
    }
    let cue_start = out.len();
    let start_ms = (cue.start.max(0.0) * 1000.0).round() as u64;
    let raw_end_ms = (cue.end.max(0.0) * 1000.0).round() as u64;
    let end_ms = if raw_end_ms > start_ms {
        raw_end_ms
    } else {
        start_ms + 1
    };
    write_webvtt_timestamp_ms(out, start_ms);
    out.push_str(" --> ");
    write_webvtt_timestamp_ms(out, end_ms);
    out.push('\n');
    if write_sanitized_webvtt_text(out, &cue.text) {
        out.push_str("\n\n");
    } else {
        out.truncate(cue_start);
    }
}

/// Fast path for callers that already maintain cues in [`cue_cmp`] order
/// (such as [`super::AsrSubtitleTrack`]): single buffer allocation, no sort.
pub(super) fn format_webvtt_sorted_cues(cues: &[AsrCue]) -> Vec<u8> {
    let mut out = String::with_capacity(8 + cues.len() * 64);
    out.push_str("WEBVTT\n\n");
    for cue in cues {
        write_webvtt_cue(&mut out, cue);
    }
    out.into_bytes()
}

/// Render ASR cues as BOM-free UTF-8 WebVTT in RAM (`WEBVTT\n\n`), sorted in
/// chronological order without extra allocations when the slice is already sorted.
pub fn format_webvtt_cues(cues: &[AsrCue]) -> Vec<u8> {
    if cues.windows(2).all(|w| cue_cmp(&w[0], &w[1]) != Ordering::Greater) {
        return format_webvtt_sorted_cues(cues);
    }

    let mut sorted: Vec<&AsrCue> = cues
        .iter()
        .filter(|cue| {
            cue.start.is_finite() && cue.end.is_finite() && cue.start >= 0.0 && cue.end > cue.start
        })
        .collect();
    sorted.sort_by(|a, b| cue_cmp(a, b));

    let mut out = String::with_capacity(8 + sorted.len() * 64);
    out.push_str("WEBVTT\n\n");
    for cue in sorted {
        write_webvtt_cue(&mut out, cue);
    }
    out.into_bytes()
}

struct StreamCursor {
    data: Arc<[u8]>,
    pos: usize,
}

pub(super) unsafe extern "C" fn stream_cb_open_ro(
    user_data: *mut c_void,
    uri: *mut c_char,
    info: *mut ffi::mpv_stream_cb_info,
) -> c_int {
    catch_unwind(AssertUnwindSafe(|| {
        if user_data.is_null() || uri.is_null() || info.is_null() {
            return ffi::MPV_ERROR_LOADING_FAILED;
        }
        let uri_bytes = unsafe { CStr::from_ptr(uri) }.to_bytes();
        if !uri_bytes.starts_with(b"memento-asr://") {
            return ffi::MPV_ERROR_LOADING_FAILED;
        }
        let shared = unsafe { &*(user_data as *const RwLock<Arc<[u8]>>) };
        let snapshot = shared
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        let cursor = Box::into_raw(Box::new(StreamCursor {
            data: snapshot,
            pos: 0,
        }));
        unsafe {
            (*info).cookie = cursor.cast();
            (*info).read_fn = Some(stream_cb_read);
            (*info).seek_fn = Some(stream_cb_seek);
            (*info).size_fn = Some(stream_cb_size);
            (*info).close_fn = Some(stream_cb_close);
            (*info).cancel_fn = None;
        }
        0
    }))
    .unwrap_or(ffi::MPV_ERROR_LOADING_FAILED)
}

pub(super) unsafe extern "C" fn stream_cb_read(
    cookie: *mut c_void,
    buf: *mut c_char,
    nbytes: u64,
) -> i64 {
    catch_unwind(AssertUnwindSafe(|| {
        if cookie.is_null() || (buf.is_null() && nbytes > 0) {
            return -1;
        }
        if nbytes == 0 {
            return 0;
        }
        let cursor = unsafe { &mut *(cookie as *mut StreamCursor) };
        let remaining = cursor.data.len().saturating_sub(cursor.pos);
        if remaining == 0 {
            return 0;
        }
        let requested = usize::try_from(nbytes).unwrap_or(usize::MAX);
        let to_copy = remaining.min(requested).min(i64::MAX as usize);
        unsafe {
            ptr::copy_nonoverlapping(
                cursor.data.as_ptr().add(cursor.pos),
                buf.cast::<u8>(),
                to_copy,
            );
        }
        cursor.pos += to_copy;
        to_copy as i64
    }))
    .unwrap_or(-1)
}

pub(super) unsafe extern "C" fn stream_cb_seek(cookie: *mut c_void, offset: i64) -> i64 {
    catch_unwind(AssertUnwindSafe(|| {
        if cookie.is_null() || offset < 0 {
            return i64::from(ffi::MPV_ERROR_GENERIC);
        }
        let cursor = unsafe { &mut *(cookie as *mut StreamCursor) };
        let Ok(target) = usize::try_from(offset) else {
            return i64::from(ffi::MPV_ERROR_GENERIC);
        };
        if target > cursor.data.len() {
            return i64::from(ffi::MPV_ERROR_GENERIC);
        }
        cursor.pos = target;
        offset
    }))
    .unwrap_or(i64::from(ffi::MPV_ERROR_GENERIC))
}

pub(super) unsafe extern "C" fn stream_cb_size(cookie: *mut c_void) -> i64 {
    catch_unwind(AssertUnwindSafe(|| {
        if cookie.is_null() {
            return i64::from(ffi::MPV_ERROR_UNSUPPORTED);
        }
        let cursor = unsafe { &*(cookie as *const StreamCursor) };
        i64::try_from(cursor.data.len()).unwrap_or(i64::MAX)
    }))
    .unwrap_or(i64::from(ffi::MPV_ERROR_UNSUPPORTED))
}

pub(super) unsafe extern "C" fn stream_cb_close(cookie: *mut c_void) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        if !cookie.is_null() {
            drop(unsafe { Box::from_raw(cookie as *mut StreamCursor) });
        }
    }));
}

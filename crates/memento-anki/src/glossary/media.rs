//! Safe dictionary media path resolution and legacy `image` glossary rendering.

use std::collections::BTreeSet;

use crate::glossary::escape::{
    append_escaped, ascii_to_u16, is_valid_image_rendering, push_ascii, qstring_number,
};
use crate::json::{JsonObject, JsonValue};
use crate::strings::is_valid_utf16;

const KEY_COLLAPSED: [u16; 9] = ascii_to_u16("collapsed");
const KEY_COLLAPSIBLE: [u16; 11] = ascii_to_u16("collapsible");
const KEY_DESCRIPTION: [u16; 11] = ascii_to_u16("description");
const KEY_HEIGHT: [u16; 6] = ascii_to_u16("height");
const KEY_PATH: [u16; 4] = ascii_to_u16("path");
const KEY_RENDERING: [u16; 14] = ascii_to_u16("imageRendering");
const KEY_TITLE: [u16; 5] = ascii_to_u16("title");
const KEY_WIDTH: [u16; 5] = ascii_to_u16("width");

const MAX_EXTENSION_LEN: usize = 16;

fn is_path_sep(u: u16) -> bool {
    u == b'/' as u16 || u == b'\\' as u16
}

fn is_ascii_alpha(u: u16) -> bool {
    (b'A' as u16..=b'Z' as u16).contains(&u) || (b'a' as u16..=b'z' as u16).contains(&u)
}

fn is_ascii_alnum(u: u16) -> bool {
    is_ascii_alpha(u) || (b'0' as u16..=b'9' as u16).contains(&u)
}

/// Validates and splits a dictionary-relative file path into safe path
/// segments. Rejects empty paths, NUL/ASCII control characters, lone UTF-16
/// surrogates, root/absolute/UNC paths, Windows drive/stream colons, `..`
/// traversal segments, and directory-like trailing `/` or `.` paths.
fn sanitize_relative_path(path: &[u16]) -> Option<Vec<&[u16]>> {
    if path.is_empty() {
        return None;
    }
    if path.iter().any(|&u| u < 0x20 || u == 0x7f) || !is_valid_utf16(path) {
        return None;
    }
    if is_path_sep(path[0]) || is_path_sep(*path.last()?) {
        return None;
    }
    if path.len() >= 2 && path[1] == b':' as u16 && is_ascii_alpha(path[0]) {
        return None;
    }

    const DOT: [u16; 1] = [b'.' as u16];
    const DOT_DOT: [u16; 2] = [b'.' as u16, b'.' as u16];

    let raw_last = path.rsplit(|&u| is_path_sep(u)).next()?;
    if raw_last.is_empty() || raw_last == DOT || raw_last == DOT_DOT {
        return None;
    }

    let mut segments = Vec::new();
    for seg in path.split(|&u| is_path_sep(u)) {
        if seg.is_empty() || seg == DOT {
            continue;
        }
        if seg == DOT_DOT || seg.contains(&(b':' as u16)) {
            return None;
        }
        segments.push(seg);
    }

    if segments.is_empty() {
        return None;
    }
    Some(segments)
}

/// Extracts a clean ASCII alphanumeric extension from the final filename
/// segment using the last `.` (ignoring dotfiles and trailing dots).
fn sanitized_extension(file_name: &[u16]) -> Option<&[u16]> {
    let dot = file_name.iter().rposition(|&u| u == b'.' as u16)?;
    if dot == 0 || dot + 1 >= file_name.len() {
        return None;
    }
    let ext = &file_name[dot + 1..];
    if ext.len() > MAX_EXTENSION_LEN || !ext.iter().all(|&u| is_ascii_alnum(u)) {
        return None;
    }
    Some(ext)
}

/// Safely joins `basepath` and dictionary-relative `path`, computes its MD5
/// hash filename with a sanitized extension, and records `(full_path, hash)`
/// in `file_map`. Returns `None` if `basepath` is empty, `path` is unsafe, or
/// `md5_calc` rejects/cannot hash the file.
pub(crate) fn add_file<F>(
    basepath: &[u16],
    path: &[u16],
    file_map: &mut BTreeSet<(Vec<u16>, Vec<u16>)>,
    md5_calc: &mut F,
) -> Option<Vec<u16>>
where
    F: FnMut(&[u16]) -> Option<[u16; 32]>,
{
    if basepath.is_empty() {
        return None;
    }
    let segments = sanitize_relative_path(path)?;
    let sep = if cfg!(windows) {
        b'\\' as u16
    } else {
        b'/' as u16
    };

    let mut full_path = Vec::with_capacity(basepath.len() + path.len() + 1);
    full_path.extend_from_slice(basepath);
    if !full_path.last().copied().is_some_and(is_path_sep) {
        full_path.push(sep);
    }
    for (idx, seg) in segments.iter().enumerate() {
        if idx > 0 {
            full_path.push(sep);
        }
        full_path.extend_from_slice(seg);
    }

    let hash_hex = md5_calc(&full_path)?;
    if !hash_hex.iter().all(|&u| is_ascii_alnum(u)) {
        return None;
    }

    let ext = segments.last().and_then(|name| sanitized_extension(name));
    let mut hash = Vec::with_capacity(32 + ext.map_or(0, |e| 1 + e.len()));
    hash.extend_from_slice(&hash_hex);
    if let Some(ext) = ext {
        hash.push(b'.' as u16);
        hash.extend_from_slice(ext);
    }

    file_map.insert((full_path, hash.clone()));
    Some(hash)
}

pub(crate) fn add_image<F>(
    obj: &JsonObject,
    basepath: &[u16],
    out: &mut Vec<u16>,
    file_map: &mut BTreeSet<(Vec<u16>, Vec<u16>)>,
    md5_calc: &mut F,
) where
    F: FnMut(&[u16]) -> Option<[u16; 32]>,
{
    let collapsed = match obj.get(KEY_COLLAPSED.as_slice()) {
        Some(JsonValue::Bool(b)) => *b,
        _ => false,
    };
    let collapsible = match obj.get(KEY_COLLAPSIBLE.as_slice()) {
        Some(JsonValue::Bool(b)) => *b,
        _ => true,
    };

    if collapsible {
        push_ascii(out, "<details");
        if !collapsed {
            push_ascii(out, " open");
        }
        out.push(b'>' as u16);
        push_ascii(
            out,
            "<summary style=\"text-decoration: underline;cursor: pointer;\">[Image]</summary>",
        );
    }

    let path = obj
        .get(KEY_PATH.as_slice())
        .and_then(JsonValue::as_qstring)
        .unwrap_or(&[]);
    let filename = add_file(basepath, path, file_map, md5_calc).unwrap_or_default();

    push_ascii(out, "<img src=\"");
    append_escaped(&filename, out, false);
    out.push(0x22);

    if let Some(JsonValue::Number(w)) = obj.get(KEY_WIDTH.as_slice()) {
        if w.is_finite() {
            push_ascii(out, " width=\"");
            out.extend(qstring_number(*w));
            out.push(0x22);
        }
    }

    if let Some(JsonValue::Number(h)) = obj.get(KEY_HEIGHT.as_slice()) {
        if h.is_finite() {
            push_ascii(out, " height=\"");
            out.extend(qstring_number(*h));
            out.push(0x22);
        }
    }

    if let Some(title) = obj
        .get(KEY_TITLE.as_slice())
        .and_then(JsonValue::as_qstring)
    {
        push_ascii(out, " title=\"");
        append_escaped(title, out, false);
        out.push(0x22);
    }

    push_ascii(out, " style=\"display: inline-table; vertical-align: top;");
    if let Some(rend) = obj
        .get(KEY_RENDERING.as_slice())
        .and_then(JsonValue::as_qstring)
    {
        if is_valid_image_rendering(rend) {
            push_ascii(out, "image-rendering: ");
            out.extend_from_slice(rend);
            out.push(b';' as u16);
        }
    }
    push_ascii(out, "\">");

    if let Some(desc) = obj
        .get(KEY_DESCRIPTION.as_slice())
        .and_then(JsonValue::as_qstring)
    {
        push_ascii(out, "<br>");
        append_escaped(desc, out, true);
    }

    if collapsible {
        push_ascii(out, "</details>");
    }
}

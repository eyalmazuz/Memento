//! Safe dictionary media path resolution and legacy `image` glossary rendering.

use std::collections::BTreeSet;

use serde_json::{Map, Value};

use crate::glossary::escape::{append_escaped, append_number, is_valid_image_rendering};

const MAX_EXTENSION_LEN: usize = 16;

fn is_path_sep(ch: char) -> bool {
    ch == '/' || ch == '\\'
}

/// Validates and splits a dictionary-relative file path into safe path
/// segments. Rejects empty paths, NUL/ASCII control characters,
/// root/absolute/UNC paths, Windows drive/stream colons, `..` traversal
/// segments, and directory-like trailing `/` or `.` paths.
fn sanitize_relative_path(path: &str) -> Option<Vec<&str>> {
    if path.is_empty() {
        return None;
    }
    if path.bytes().any(|b| b < 0x20 || b == 0x7f) {
        return None;
    }
    let bytes = path.as_bytes();
    if is_path_sep(bytes[0] as char) || is_path_sep(*bytes.last()? as char) {
        return None;
    }
    if bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic() {
        return None;
    }

    let raw_last = path.rsplit(is_path_sep).next()?;
    if raw_last.is_empty() || raw_last == "." || raw_last == ".." {
        return None;
    }

    let mut segments = Vec::new();
    for seg in path.split(is_path_sep) {
        if seg.is_empty() || seg == "." {
            continue;
        }
        if seg == ".." || seg.contains(':') {
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
fn sanitized_extension(file_name: &str) -> Option<&str> {
    let dot = file_name.rfind('.')?;
    if dot == 0 || dot + 1 >= file_name.len() {
        return None;
    }
    let ext = &file_name[dot + 1..];
    if ext.len() > MAX_EXTENSION_LEN || !ext.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return None;
    }
    Some(ext)
}

/// Safely joins `basepath` and dictionary-relative `path`, computes its MD5
/// hash filename with a sanitized extension, and records `(full_path, hash)`
/// in `file_map`. Returns `None` if `basepath` is empty, `path` is unsafe, or
/// `md5_calc` rejects/cannot hash the file.
pub(crate) fn add_file<F>(
    basepath: &str,
    path: &str,
    file_map: &mut BTreeSet<(String, String)>,
    md5_calc: &mut F,
) -> Option<String>
where
    F: FnMut(&str) -> Option<String>,
{
    if basepath.is_empty() {
        return None;
    }
    let segments = sanitize_relative_path(path)?;
    let sep = if cfg!(windows) { '\\' } else { '/' };

    let mut full_path = String::with_capacity(basepath.len() + path.len() + 1);
    full_path.push_str(basepath);
    if !full_path.ends_with(is_path_sep) {
        full_path.push(sep);
    }
    for (idx, seg) in segments.iter().enumerate() {
        if idx > 0 {
            full_path.push(sep);
        }
        full_path.push_str(seg);
    }

    let hash_hex = md5_calc(&full_path)?;
    if hash_hex.len() != 32 || !hash_hex.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return None;
    }

    let ext = segments.last().and_then(|name| sanitized_extension(name));
    let mut hash = String::with_capacity(32 + ext.map_or(0, |e| 1 + e.len()));
    hash.push_str(&hash_hex);
    if let Some(ext) = ext {
        hash.push('.');
        hash.push_str(ext);
    }

    file_map.insert((full_path, hash.clone()));
    Some(hash)
}

pub(crate) fn add_image<F>(
    obj: &Map<String, Value>,
    basepath: &str,
    out: &mut String,
    file_map: &mut BTreeSet<(String, String)>,
    md5_calc: &mut F,
) where
    F: FnMut(&str) -> Option<String>,
{
    let collapsed = obj
        .get("collapsed")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let collapsible = obj
        .get("collapsible")
        .and_then(Value::as_bool)
        .unwrap_or(true);

    if collapsible {
        out.push_str("<details");
        if !collapsed {
            out.push_str(" open");
        }
        out.push_str(
            "><summary style=\"text-decoration: underline;cursor: pointer;\">[Image]</summary>",
        );
    }

    let path = obj.get("path").and_then(Value::as_str).unwrap_or("");
    let filename = add_file(basepath, path, file_map, md5_calc).unwrap_or_default();

    out.push_str("<img src=\"");
    append_escaped(&filename, out, false);
    out.push('"');

    if let Some(w) = obj.get("width").and_then(Value::as_f64) {
        if w.is_finite() {
            out.push_str(" width=\"");
            append_number(out, w);
            out.push('"');
        }
    }

    if let Some(h) = obj.get("height").and_then(Value::as_f64) {
        if h.is_finite() {
            out.push_str(" height=\"");
            append_number(out, h);
            out.push('"');
        }
    }

    if let Some(title) = obj.get("title").and_then(Value::as_str) {
        out.push_str(" title=\"");
        append_escaped(title, out, false);
        out.push('"');
    }

    out.push_str(" style=\"display: inline-table; vertical-align: top;");
    if let Some(rend) = obj.get("imageRendering").and_then(Value::as_str) {
        if is_valid_image_rendering(rend) {
            out.push_str("image-rendering: ");
            out.push_str(rend);
            out.push(';');
        }
    }
    out.push_str("\">");

    if let Some(desc) = obj.get("description").and_then(Value::as_str) {
        out.push_str("<br>");
        append_escaped(desc, out, true);
    }

    if collapsible {
        out.push_str("</details>");
    }
}

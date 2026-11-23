//! Safe cross-platform native file and folder dialogs via `rfd` (Win32
//! `IFileOpenDialog` on Windows, AppKit `NSOpenPanel` on macOS, XDG Desktop
//! Portal on Unix).

use std::collections::HashSet;
use std::path::PathBuf;

use crate::{FileDialogFilter, FileDialogOptions};

/// Convert glob patterns (e.g. `["*.mkv", "*.srt"]` or `["*"]`) into the bare
/// extension list expected by `rfd::FileDialog::add_filter`.
pub(crate) fn extract_rfd_filter_extensions(filter: &FileDialogFilter) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut extensions = Vec::new();
    for raw in &filter.patterns {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed == "*" || trimmed == "*.*" {
            if seen.insert("*".to_owned()) {
                extensions.push("*".to_owned());
            }
            continue;
        }
        let ext = trimmed
            .strip_prefix("*.")
            .or_else(|| trimmed.strip_prefix('.'))
            .unwrap_or(trimmed);
        if ext.is_empty() || ext.contains('*') || ext.contains('?') {
            continue;
        }
        let lower = ext.to_ascii_lowercase();
        if seen.insert(lower.clone()) {
            extensions.push(lower);
        }
    }
    extensions
}

/// Build an `rfd::FileDialog` configured from [`FileDialogOptions`].
pub(crate) fn build_rfd_dialog(options: &FileDialogOptions) -> rfd::FileDialog {
    let mut dialog = rfd::FileDialog::new();
    if !options.title.is_empty() {
        dialog = dialog.set_title(&options.title);
    }
    if let Some(dir) = options.initial_dir.as_deref() {
        if dir.is_dir() {
            dialog = dialog.set_directory(dir);
        }
    }
    if !options.is_folder() {
        for filter in options.filters() {
            let exts = extract_rfd_filter_extensions(&filter);
            if exts.is_empty() {
                continue;
            }
            let refs: Vec<&str> = exts.iter().map(String::as_str).collect();
            dialog = dialog.add_filter(&filter.label, &refs);
        }
    }
    dialog
}

/// Blocking native file or folder chooser backed by `rfd`.
#[cfg_attr(all(unix, not(target_os = "macos")), allow(dead_code))]
pub(crate) fn open_files_rfd(options: &FileDialogOptions) -> Vec<PathBuf> {
    let dialog = build_rfd_dialog(options);
    if options.is_folder() {
        if options.multiple {
            dialog.pick_folders().unwrap_or_default()
        } else {
            dialog.pick_folder().into_iter().collect()
        }
    } else if options.multiple {
        dialog.pick_files().unwrap_or_default()
    } else {
        dialog.pick_file().into_iter().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn rfd_filter_extensions_normalize_globs_dedup_and_preserve_wildcards() {
        let folder = FileDialogOptions::folder(Some(PathBuf::from("/tmp")));
        assert!(folder.filters().is_empty());

        let zip = FileDialogOptions::archive_zip(None);
        let zip_filters = zip.filters();
        assert_eq!(extract_rfd_filter_extensions(&zip_filters[0]), vec!["zip"]);
        assert_eq!(extract_rfd_filter_extensions(&zip_filters[1]), vec!["*"]);

        let subs = FileDialogOptions::subtitles_only(None);
        let sub_exts = extract_rfd_filter_extensions(&subs.filters()[0]);
        for required in [
            "ass", "ssa", "srt", "vtt", "sub", "idx", "sup", "lrc", "mks", "ttml", "smi", "rt",
        ] {
            assert!(
                sub_exts.iter().any(|e| e == required),
                "missing subtitle extension `{required}` in {sub_exts:?}"
            );
        }
        let unique: HashSet<&str> = sub_exts.iter().map(String::as_str).collect();
        assert_eq!(unique.len(), sub_exts.len());
        assert!(!sub_exts.iter().any(|e| e.contains('*')));

        // Building the dialog itself must succeed without touching any OS UI.
        let _ = build_rfd_dialog(&subs);
        let _ = build_rfd_dialog(&folder);
    }
}

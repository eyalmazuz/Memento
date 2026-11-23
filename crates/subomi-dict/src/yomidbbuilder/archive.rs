//! Safe ZIP archive validation, streaming decompression size limits, and resource extraction.

use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::{ErrorKind, Read, Seek, Write};
use std::path::{Component, Path, PathBuf};

use serde_json::Value;
use zip::ZipArchive;

use super::YomiError;

/// Maximum uncompressed size allowed for a single ZIP entry (128 MiB).
pub const MAX_ENTRY_BYTES: u64 = 128 * 1024 * 1024;
/// Maximum cumulative uncompressed bytes allowed across an entire dictionary ZIP (2 GiB).
pub const MAX_TOTAL_BYTES: u64 = 2 * 1024 * 1024 * 1024;

#[derive(Clone, Copy)]
pub(super) struct SizeLimits {
    pub max_entry_bytes: u64,
    pub max_total_bytes: u64,
}

impl Default for SizeLimits {
    fn default() -> Self {
        Self {
            max_entry_bytes: MAX_ENTRY_BYTES,
            max_total_bytes: MAX_TOTAL_BYTES,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum LimitViolation {
    EntryTooLarge,
    ArchiveTooLarge,
}

/// Streaming reader that enforces both per-entry and cumulative archive byte limits
/// without buffering whole JSON bank files in memory.
pub(super) struct LimitedEntryReader<'a, R> {
    inner: R,
    entry_read: u64,
    total_bytes: &'a mut u64,
    limits: SizeLimits,
    violation: Option<LimitViolation>,
}

impl<'a, R> LimitedEntryReader<'a, R> {
    pub(super) fn new(inner: R, total_bytes: &'a mut u64, limits: SizeLimits) -> Self {
        Self {
            inner,
            entry_read: 0,
            total_bytes,
            limits,
            violation: None,
        }
    }

    pub(super) fn violation(&self) -> Option<LimitViolation> {
        self.violation
    }
}

impl<R: Read> Read for LimitedEntryReader<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.violation.is_some() {
            return Err(std::io::Error::other("decompression size limit exceeded"));
        }
        let n = self.inner.read(buf)?;
        if n == 0 {
            return Ok(0);
        }
        let n_u64 = n as u64;
        let next_entry = self.entry_read.saturating_add(n_u64);
        if next_entry > self.limits.max_entry_bytes {
            self.violation = Some(LimitViolation::EntryTooLarge);
            return Err(std::io::Error::other("entry size limit exceeded"));
        }
        let Some(next_total) = self.total_bytes.checked_add(n_u64) else {
            self.violation = Some(LimitViolation::ArchiveTooLarge);
            return Err(std::io::Error::other("archive size limit exceeded"));
        };
        if next_total > self.limits.max_total_bytes {
            self.violation = Some(LimitViolation::ArchiveTooLarge);
            return Err(std::io::Error::other("archive size limit exceeded"));
        }
        self.entry_read = next_entry;
        *self.total_bytes = next_total;
        Ok(n)
    }
}

pub(super) fn sanitize_zip_entry_path(
    entry_name: &str,
    enclosed: Option<PathBuf>,
    is_dir: bool,
) -> Result<PathBuf, YomiError> {
    if entry_name.contains('\0') {
        return Err(YomiError::UnsafeArchiveEntry {
            entry: entry_name.to_owned(),
            reason: "entry path contains NUL byte",
        });
    }
    if entry_name.starts_with('/') || entry_name.starts_with('\\') {
        return Err(YomiError::UnsafeArchiveEntry {
            entry: entry_name.to_owned(),
            reason: "entry path must be relative",
        });
    }
    if entry_name.split(['/', '\\']).any(|seg| seg == "..") {
        return Err(YomiError::UnsafeArchiveEntry {
            entry: entry_name.to_owned(),
            reason: "entry path contains parent traversal",
        });
    }
    let rel = enclosed.ok_or_else(|| YomiError::UnsafeArchiveEntry {
        entry: entry_name.to_owned(),
        reason: "entry path escapes archive root",
    })?;
    let mut clean = PathBuf::new();
    for component in rel.components() {
        match component {
            Component::Normal(part) => clean.push(part),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(YomiError::UnsafeArchiveEntry {
                    entry: entry_name.to_owned(),
                    reason: "entry path contains unsafe component",
                });
            }
        }
    }
    if !is_dir && clean.as_os_str().is_empty() {
        return Err(YomiError::UnsafeArchiveEntry {
            entry: entry_name.to_owned(),
            reason: "file entry path is empty",
        });
    }
    Ok(clean)
}

pub(super) fn validate_entry_safety<R: Read>(
    entry: &zip::read::ZipFile<'_, R>,
    limits: SizeLimits,
) -> Result<PathBuf, YomiError> {
    let name = entry.name();
    if entry.is_symlink() {
        return Err(YomiError::UnsafeArchiveEntry {
            entry: name.to_owned(),
            reason: "symbolic links are not allowed",
        });
    }
    if let Some(mode) = entry.unix_mode() {
        let file_type = mode & 0o170000;
        if file_type != 0 && file_type != 0o100000 && file_type != 0o040000 {
            return Err(YomiError::UnsafeArchiveEntry {
                entry: name.to_owned(),
                reason: "special file entries are not allowed",
            });
        }
    }
    let rel = sanitize_zip_entry_path(name, entry.enclosed_name(), entry.is_dir())?;
    if !entry.is_dir() && entry.size() > limits.max_entry_bytes {
        return Err(YomiError::EntryTooLarge {
            entry: name.to_owned(),
            limit: limits.max_entry_bytes,
        });
    }
    Ok(rel)
}

pub(super) fn validate_archive_entries<R: Read + Seek>(
    archive: &mut ZipArchive<R>,
    limits: SizeLimits,
) -> Result<(), YomiError> {
    for index in 0..archive.len() {
        let entry = archive.by_index(index)?;
        let _ = validate_entry_safety(&entry, limits)?;
    }
    Ok(())
}

pub(super) fn read_json_entry<R: Read + Seek>(
    archive: &mut ZipArchive<R>,
    filename: &str,
    total_bytes: &mut u64,
    limits: SizeLimits,
) -> Result<Option<Value>, YomiError> {
    let entry = match archive.by_name(filename) {
        Ok(entry) => entry,
        Err(zip::result::ZipError::FileNotFound) => return Ok(None),
        Err(err) => return Err(YomiError::Zip(err)),
    };
    let _ = validate_entry_safety(&entry, limits)?;
    if entry.is_dir() {
        return Err(YomiError::UnsafeArchiveEntry {
            entry: filename.to_owned(),
            reason: "expected regular file, found directory",
        });
    }

    let remaining_total = limits.max_total_bytes.saturating_sub(*total_bytes);
    let entry_cap = limits.max_entry_bytes.min(remaining_total);
    let mut buf = Vec::new();
    entry
        .take(entry_cap.saturating_add(1))
        .read_to_end(&mut buf)
        .map_err(|source| YomiError::Io {
            operation: "read archive entry",
            path: PathBuf::from(filename),
            source,
        })?;
    let bytes_read = buf.len() as u64;
    if bytes_read > limits.max_entry_bytes {
        return Err(YomiError::EntryTooLarge {
            entry: filename.to_owned(),
            limit: limits.max_entry_bytes,
        });
    }
    let new_total = total_bytes
        .checked_add(bytes_read)
        .ok_or(YomiError::ArchiveTooLarge {
            limit: limits.max_total_bytes,
        })?;
    if new_total > limits.max_total_bytes {
        return Err(YomiError::ArchiveTooLarge {
            limit: limits.max_total_bytes,
        });
    }
    *total_bytes = new_total;

    let value = serde_json::from_slice(&buf).map_err(|source| YomiError::Json {
        entry: filename.to_owned(),
        source,
    })?;
    Ok(Some(value))
}

fn is_skipped_resource(rel_path: &Path) -> bool {
    let mut components = rel_path.components();
    let (Some(Component::Normal(first)), None) = (components.next(), components.next()) else {
        return false;
    };
    let Some(name) = first.to_str() else {
        return false;
    };
    if name == "index.json" {
        return true;
    }
    for prefix in [
        "tag_bank_",
        "term_bank_",
        "term_meta_bank_",
        "kanji_bank_",
        "kanji_meta_bank_",
    ] {
        if let Some(rest) = name
            .strip_prefix(prefix)
            .and_then(|s| s.strip_suffix(".json"))
        {
            if rest.starts_with(|c: char| matches!(c, '1'..='9'))
                && rest[1..].bytes().all(|b| b.is_ascii_digit())
            {
                return true;
            }
        }
    }
    false
}

pub(super) fn extract_resources_to_staging<R: Read + Seek>(
    archive: &mut ZipArchive<R>,
    staging_dir: &Path,
    total_bytes: &mut u64,
    limits: SizeLimits,
) -> Result<(), YomiError> {
    for index in 0..archive.len() {
        let entry = archive.by_index(index)?;
        let entry_name = entry.name().to_owned();
        let rel_path = validate_entry_safety(&entry, limits)?;
        if is_skipped_resource(&rel_path) {
            continue;
        }
        if entry.is_dir() {
            if !rel_path.as_os_str().is_empty() {
                let target_dir = staging_dir.join(&rel_path);
                fs::create_dir_all(&target_dir).map_err(|source| YomiError::Io {
                    operation: "create resource directory",
                    path: target_dir,
                    source,
                })?;
            }
            continue;
        }

        let out_path = staging_dir.join(&rel_path);
        if let Some(parent) = out_path.parent() {
            fs::create_dir_all(parent).map_err(|source| YomiError::Io {
                operation: "create resource parent directory",
                path: parent.to_path_buf(),
                source,
            })?;
        }

        let remaining_total = limits.max_total_bytes.saturating_sub(*total_bytes);
        let entry_cap = limits.max_entry_bytes.min(remaining_total);
        let mut limited = entry.take(entry_cap.saturating_add(1));
        let mut out_file = File::create(&out_path).map_err(|source| YomiError::Io {
            operation: "create resource file",
            path: out_path.clone(),
            source,
        })?;
        let bytes_written = std::io::copy(&mut limited, &mut out_file).map_err(|source| {
            YomiError::Io {
                operation: "write resource file",
                path: out_path.clone(),
                source,
            }
        })?;
        if bytes_written > limits.max_entry_bytes {
            return Err(YomiError::EntryTooLarge {
                entry: entry_name,
                limit: limits.max_entry_bytes,
            });
        }
        let new_total = total_bytes
            .checked_add(bytes_written)
            .ok_or(YomiError::ArchiveTooLarge {
                limit: limits.max_total_bytes,
            })?;
        if new_total > limits.max_total_bytes {
            return Err(YomiError::ArchiveTooLarge {
                limit: limits.max_total_bytes,
            });
        }
        *total_bytes = new_total;
        out_file.flush().map_err(|source| YomiError::Io {
            operation: "flush resource file",
            path: out_path,
            source,
        })?;
    }
    Ok(())
}

pub(super) fn safe_remove_dict_resource_dir(
    canon_res_dir: &Path,
    dic_id: i64,
) -> Result<(), YomiError> {
    if dic_id <= 0 {
        return Err(YomiError::InvalidDictionaryId(dic_id));
    }
    let dir_name = format!("dict-{dic_id}");
    let target = canon_res_dir.join(&dir_name);
    let meta = match fs::symlink_metadata(&target) {
        Ok(meta) => meta,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(()),
        Err(source) => {
            return Err(YomiError::Io {
                operation: "inspect dictionary resource directory",
                path: target,
                source,
            });
        }
    };
    if meta.file_type().is_symlink() {
        return fs::remove_file(&target).map_err(|source| YomiError::Io {
            operation: "remove dictionary resource symlink",
            path: target,
            source,
        });
    }
    if meta.is_dir() {
        let canon_target = fs::canonicalize(&target).map_err(|source| YomiError::Io {
            operation: "canonicalize dictionary resource directory",
            path: target.clone(),
            source,
        })?;
        if canon_target.parent() != Some(canon_res_dir)
            || canon_target.file_name() != Some(OsStr::new(&dir_name))
        {
            return Err(YomiError::InvalidPath {
                path: canon_target,
                reason: "target escaped resources directory",
            });
        }
        return fs::remove_dir_all(&canon_target).map_err(|source| YomiError::Io {
            operation: "remove dictionary resource directory",
            path: canon_target,
            source,
        });
    }
    fs::remove_file(&target).map_err(|source| YomiError::Io {
        operation: "remove dictionary resource entry",
        path: target,
        source,
    })
}

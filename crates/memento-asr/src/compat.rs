//! QtCore filesystem-path semantics needed by the controller ports.
//!
//! Everything here mirrors the *observable behavior* of the Qt functions the
//! C++ controllers use (`QDir::filePath`, `QDir::cleanPath`,
//! `QFileInfo::isAbsolute`/`fileName`/`exists`/`isReadable`/
//! `absoluteFilePath`, `QDir::entryInfoList` filters). The behaviors are
//! cross-checked field-by-field by `tests/mpv-memory-subtitles/
//! controllers_equal_test.cpp`, which itself uses the compiled Qt as the
//! ground truth.
//!
//! Unix-only by design: Memento ASR runs on Linux/XDG layouts here
//! (`docs/rust-port-plan.md` scope).

use std::os::unix::fs::MetadataExt;

/// `QFileInfo::isAbsolute()` (Unix: an initial `/`).
#[inline]
pub fn is_absolute(path: &str) -> bool {
    path.starts_with('/')
}

/// `QFileInfo::fileName()`: everything after the last `/` (or `""`).
#[inline]
pub fn file_name(path: &str) -> &str {
    match path.rfind('/') {
        Some(_) => path.rsplit('/').next().unwrap_or(""),
        None => path,
    }
}

/// `QFileInfo::exists()`: true for files and directories (follows symlinks),
/// false for dangling links and errors — matches `stat()`-based Qt behavior.
#[inline]
pub fn file_exists(path: &str) -> bool {
    !path.is_empty() && std::fs::metadata(path).is_ok()
}

/// `QDir::filePath(const QString&)`: an absolute name wins; QDir normalizes
/// its own trailing separator but does not clean the appended name.
pub fn qdir_join(dir: &str, name: &str) -> String {
    if name.starts_with('/') {
        return name.to_string();
    }
    let base = if dir.is_empty() || dir == "." {
        "."
    } else {
        let trimmed = dir.trim_end_matches('/');
        if trimmed.is_empty() { "/" } else { trimmed }
    };
    if name.is_empty() {
        return base.to_string();
    }
    if base == "/" {
        format!("/{name}")
    } else {
        format!("{base}/{name}")
    }
}

/// The process current directory, cleaned (Qt resolves relative directory
/// paths through it; it never has a trailing separator except for `/`).
pub fn current_dir() -> String {
    match std::env::current_dir() {
        Ok(dir) => dir.to_string_lossy().into_owned(),
        Err(_) => {
            // Qt falls back to "/" when the cwd vanished.
            "/".to_string()
        }
    }
}

/// `QDir::cleanPath` on Unix: collapse repeated separators and dot segments;
/// unlike `QFileInfo::absoluteFilePath`, it removes trailing separators.
pub fn qdir_clean_path(path: &str) -> String {
    if path.is_empty() {
        return String::new();
    }

    let absolute = path.starts_with('/');
    let mut components: Vec<&str> = Vec::new();
    for component in path.split('/') {
        if component.is_empty() || component == "." {
            continue;
        }
        if component == ".." {
            if let Some(last) = components.last() {
                if *last != ".." {
                    components.pop();
                    continue;
                }
            }
            if !absolute {
                components.push("..");
            }
        } else {
            components.push(component);
        }
    }

    let joined = components.join("/");
    if absolute {
        format!("/{joined}")
    } else if joined.is_empty() {
        ".".to_string()
    } else {
        joined
    }
}

/// `QFileInfo::absoluteFilePath()` normalizes internal separators and dot
/// segments while preserving a trailing slash (including a doubled root).
pub fn qfileinfo_absolute(path: &str) -> String {
    if path.is_empty() {
        return String::new();
    }
    let absolute = if is_absolute(path) {
        qdir_clean_path(path)
    } else {
        qdir_clean_path(&(current_dir() + "/" + path))
    };
    if path.ends_with('/') && path != "/" {
        absolute + "/"
    } else {
        absolute
    }
}

/// `QFileInfo::isReadable()` on Unix: stat the target and evaluate the read
/// bits for the current identity (owner bit for the owner, group bit for a
/// matching group, otherwise the other bit; root gets "any bit").
pub fn is_readable(path: &str) -> bool {
    let stat = match std::fs::metadata(path) {
        Ok(stat) => stat,
        Err(_) => return false,
    };
    let mode = stat.mode();
    const S_IRUSR: u32 = 0o400;
    const S_IRGRP: u32 = 0o040;
    const S_IROTH: u32 = 0o004;

    if current_euid() == 0 {
        return mode & (S_IRUSR | S_IRGRP | S_IROTH) != 0;
    }
    if stat.uid() == current_euid() {
        return mode & S_IRUSR != 0;
    }
    let gid = stat.gid();
    if gid == current_egid() || current_groups().contains(&gid) {
        return mode & S_IRGRP != 0;
    }
    mode & S_IROTH != 0
}

fn current_euid() -> u32 {
    unsafe { geteuid() }
}

fn current_egid() -> u32 {
    unsafe { getegid() }
}

fn current_groups() -> Vec<u32> {
    // The standard identity set is small on Linux; grow defensively like
    // getpwuid-based code does.
    let mut size = 16i32;
    loop {
        let mut groups = vec![0u32; size as usize];
        let count = unsafe { getgroups(size, groups.as_mut_ptr()) };
        if count < 0 {
            // A missing group list simply limits the group-bit check (as the
            // C++ path would when the identity projection fails).
            return Vec::new();
        }
        if count < size {
            groups.truncate(count.max(0) as usize);
            return groups;
        }
        size *= 2;
    }
}

// Thin libc bindings for the identity checks (linked from the process's
// standard libc, same functions Qt's access machinery uses).
extern "C" {
    fn geteuid() -> u32;
    fn getegid() -> u32;
    fn getgroups(size: i32, list: *mut u32) -> i32;
}

/// One `QDir::entryInfoList` entry relevant to the controller ports.
pub struct EntryInfo {
    /// `QFileInfo::absoluteFilePath()` of the entry.
    pub absolute_path: String,
    /// `QFileInfo::fileName()` of the entry.
    pub name: String,
}

/// `QDir::entryInfoList(QDir::Files | QDir::Readable)` semantics (no
/// `QDir::Hidden`, so dot files are excluded; symlinks count as files when
/// they resolve to one).
pub fn entry_files_readable(dir: &str) -> Vec<EntryInfo> {
    read_dir_filtered(dir, false, true, false)
}

/// `QDir::entryInfoList(QDir::Dirs | QDir::NoDotAndDotDot | QDir::Readable)`
/// semantics (dot files/dirs excluded).
pub fn entry_dirs_readable(dir: &str) -> Vec<EntryInfo> {
    read_dir_filtered(dir, true, false, false)
}

fn read_dir_filtered(
    dir: &str,
    dirs: bool,
    files: bool,
    hidden: bool,
) -> Vec<EntryInfo> {
    let read = match std::fs::read_dir(dir) {
        Ok(read) => read,
        Err(_) => return Vec::new(),
    };
    let mut out = Vec::new();
    for entry in read.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        // QDir::Hidden controls dot entries; NoDotAndDotDot is implicit in
        // read_dir (it never yields "." or "..").
        if !hidden && name.starts_with('.') {
            continue;
        }
        let joined = qdir_join(dir, &name);
        // Follow symlinks exactly like QFileInfo does.
        let stat = match std::fs::metadata(&joined) {
            Ok(stat) => stat,
            Err(_) => continue, // QFileInfo of a dangling link: not Files/Dirs.
        };
        let is_dir = stat.is_dir();
        let is_file = stat.is_file();
        if (dirs && is_dir) || (files && is_file) {
            if !is_readable(&joined) {
                continue;
            }
            out.push(EntryInfo {
                absolute_path: joined,
                name,
            });
        }
    }
    out
}

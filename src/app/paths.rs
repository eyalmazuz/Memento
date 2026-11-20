//! Platform-aware configuration/data/cache path resolution, legacy migration,
//! file-open directory resolution, and child-process helpers.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::app::preferences::setting_text;
use crate::controller::config::ConfigController;
use memento_config::Value;
use memento_setting::keys as raw;

/// Platform family used to resolve OS-native config/data/cache directories.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum PathPlatform {
    Linux,
    MacOs,
    Windows,
}

impl PathPlatform {
    pub(crate) const fn current() -> Self {
        #[cfg(target_os = "macos")]
        {
            Self::MacOs
        }
        #[cfg(target_os = "windows")]
        {
            Self::Windows
        }
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        {
            Self::Linux
        }
    }
}

/// Complete resolved OS-native application directories and fallback/split metadata.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AppDirs {
    pub(crate) config_dir: PathBuf,
    pub(crate) data_dir: PathBuf,
    pub(crate) cache_dir: PathBuf,
    /// Legacy config directory checked automatically on read when `config_dir/memento.conf`
    /// does not exist yet (macOS `~/.config/memento`, Windows `%APPDATA%\memento`).
    pub(crate) legacy_config_fallback_dir: Option<PathBuf>,
    /// True on Linux when volatile keys (`recent/files`, `dictionary/order`, `main-window/*`)
    /// split into `~/.cache/memento/memento.conf` (Memento `#337`).
    pub(crate) use_cache_config_split: bool,
}

/// Resolve a Memento platform directory (`config`, `data`, or `cache`)
/// using an explicit [`PathPlatform`] and injectable environment lookup.
pub(crate) fn memento_platform_dir_with(
    variable: &str,
    home_suffix: &str,
    platform: PathPlatform,
    env: impl Fn(&str) -> Option<OsString>,
) -> Option<PathBuf> {
    let non_empty = |key: &str| env(key).filter(|v| !v.is_empty()).map(PathBuf::from);

    if let Some(explicit) = non_empty(variable) {
        return Some(explicit.join("memento"));
    }

    match platform {
        PathPlatform::Windows => {
            // Align config, data, and cache under %LOCALAPPDATA%\memento (with
            // %APPDATA% fallback if LOCALAPPDATA is absent).
            if let Some(root) = non_empty("LOCALAPPDATA").or_else(|| non_empty("APPDATA")) {
                return Some(root.join("memento"));
            }
            if let Some(profile) = non_empty("USERPROFILE") {
                return Some(profile.join(home_suffix).join("memento"));
            }
        }
        PathPlatform::MacOs => {
            if let Some(home) = non_empty("HOME") {
                return Some(match variable {
                    "XDG_CACHE_HOME" => home.join("Library").join("Caches").join("memento"),
                    _ => home
                        .join("Library")
                        .join("Application Support")
                        .join("memento"),
                });
            }
        }
        PathPlatform::Linux => {}
    }

    non_empty("HOME")
        .or_else(|| non_empty("USERPROFILE"))
        .map(|home| home.join(home_suffix).join("memento"))
}

/// Resolve a Memento XDG/platform directory (`config`, `data`, or `cache`)
/// using an injectable environment lookup (fully deterministic for tests).
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn memento_xdg_dir_with(
    variable: &str,
    home_suffix: &str,
    is_windows: bool,
    env: impl Fn(&str) -> Option<OsString>,
) -> Option<PathBuf> {
    let platform = if is_windows {
        PathPlatform::Windows
    } else {
        PathPlatform::Linux
    };
    memento_platform_dir_with(variable, home_suffix, platform, env)
}

/// Match OS-native config/data/cache locations across Linux (XDG), macOS
/// (`~/Library/Application Support/memento` & `~/Library/Caches/memento`), and
/// Windows (`%LOCALAPPDATA%\memento`).
pub(crate) fn memento_xdg_dir(variable: &str, home_suffix: &str) -> Option<PathBuf> {
    memento_platform_dir_with(variable, home_suffix, PathPlatform::current(), |key| {
        std::env::var_os(key)
    })
}

/// Return the legacy config fallback directory for existing Rust users whose
/// settings predate OS-native path alignment:
/// - macOS: `~/.config/memento`
/// - Windows: `%APPDATA%\memento` (when distinct from `%LOCALAPPDATA%\memento`)
/// - Linux: `None`
pub(crate) fn legacy_config_fallback_dir_for(
    platform: PathPlatform,
    env: impl Fn(&str) -> Option<OsString>,
) -> Option<PathBuf> {
    let non_empty = |key: &str| env(key).filter(|v| !v.is_empty()).map(PathBuf::from);
    match platform {
        PathPlatform::MacOs => non_empty("HOME").map(|home| home.join(".config").join("memento")),
        PathPlatform::Windows => {
            let roaming = non_empty("APPDATA")?.join("memento");
            let local = non_empty("LOCALAPPDATA").map(|p| p.join("memento"));
            if local.as_deref() == Some(roaming.as_path()) {
                None
            } else {
                Some(roaming)
            }
        }
        PathPlatform::Linux => None,
    }
}

/// Return the legacy config fallback directory for the current OS.
#[allow(dead_code)]
pub(crate) fn legacy_config_fallback_dir() -> Option<PathBuf> {
    legacy_config_fallback_dir_for(PathPlatform::current(), |key| std::env::var_os(key))
}

/// Resolve all application directories and legacy/cache-split policies for `platform`.
pub(crate) fn resolve_app_dirs_for(
    platform: PathPlatform,
    env: impl Fn(&str) -> Option<OsString>,
) -> Option<AppDirs> {
    let config_dir = memento_platform_dir_with("XDG_CONFIG_HOME", ".config", platform, &env)?;
    let data_dir = memento_platform_dir_with("XDG_DATA_HOME", ".local/share", platform, &env)?;
    let cache_dir = memento_platform_dir_with("XDG_CACHE_HOME", ".cache", platform, &env)?;
    let legacy_config_fallback_dir =
        legacy_config_fallback_dir_for(platform, &env).filter(|fb| fb != &config_dir);
    let use_cache_config_split = platform == PathPlatform::Linux && cache_dir != config_dir;
    Some(AppDirs {
        config_dir,
        data_dir,
        cache_dir,
        legacy_config_fallback_dir,
        use_cache_config_split,
    })
}

/// Resolve all application directories for the current OS.
#[allow(dead_code)]
pub(crate) fn resolve_app_dirs() -> Option<AppDirs> {
    resolve_app_dirs_for(PathPlatform::current(), |key| std::env::var_os(key))
}

/// Choose the file to read for `file_name`: prefers `config_dir/file_name`,
/// falling back automatically to `fallback_dir/file_name` if the primary file
/// does not exist yet so macOS/Windows users migrating from legacy paths never
/// lose settings.
pub(crate) fn resolve_config_read_path(
    config_dir: &Path,
    file_name: &str,
    fallback_dir: Option<&Path>,
) -> PathBuf {
    let primary = config_dir.join(file_name);
    if primary.is_file() {
        return primary;
    }
    if let Some(fallback) = fallback_dir {
        if fallback != config_dir {
            let legacy = fallback.join(file_name);
            if legacy.is_file() {
                return legacy;
            }
        }
    }
    primary
}

/// Migrate a legacy config file from `legacy_dir` (`~/.cache/memento`) into
/// `config_dir` (`~/.config/memento`) if the destination does not exist yet.
pub(crate) fn migrate_xdg_config_if_needed(
    legacy_dir: &Path,
    config_dir: &Path,
    file_name: &str,
) -> std::io::Result<bool> {
    let legacy_path = legacy_dir.join(file_name);
    let target_path = config_dir.join(file_name);
    if legacy_path == target_path || target_path.exists() || !legacy_path.is_file() {
        return Ok(false);
    }
    std::fs::create_dir_all(config_dir)?;
    let mut source = std::fs::File::open(&legacy_path)?;
    let mut temp = tempfile::NamedTempFile::new_in(config_dir)?;
    std::io::copy(&mut source, temp.as_file_mut())?;
    temp.as_file().sync_all()?;
    match temp.persist_noclobber(&target_path) {
        Ok(_) => Ok(true),
        Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
        Err(error) => Err(error.error),
    }
}

pub(crate) fn format_time(sec: f64) -> String {
    let total = sec.max(0.0) as i64;
    let h = total / 3600;
    let m = (total % 3600) / 60;
    let s = total % 60;
    if h > 0 {
        format!("{h:02}:{m:02}:{s:02}")
    } else {
        format!("{m:02}:{s:02}")
    }
}

pub(crate) fn open_directory_command(path: &Path) -> Command {
    #[cfg(target_os = "macos")]
    let mut cmd = Command::new("open");
    #[cfg(target_os = "windows")]
    let mut cmd = Command::new("explorer");
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let mut cmd = Command::new("xdg-open");
    cmd.arg(path);
    cmd
}

pub(crate) fn spawn_and_reap(mut cmd: Command) -> std::io::Result<u32> {
    let mut child = cmd.spawn()?;
    let pid = child.id();
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(pid)
}

pub(crate) fn parse_xdg_user_dir(contents: &str, key: &str, home: &Path) -> Option<PathBuf> {
    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let Some((lhs, rhs)) = trimmed.split_once('=') else {
            continue;
        };
        if lhs.trim() != key {
            continue;
        }
        let val = rhs.trim().trim_matches('"').trim_matches('\'');
        if val.is_empty() {
            continue;
        }
        if let Some(rest) = val.strip_prefix("$HOME/") {
            return Some(home.join(rest));
        }
        if val == "$HOME" {
            return Some(home.to_path_buf());
        }
        return Some(PathBuf::from(val));
    }
    None
}

pub(crate) fn decode_custom_open_dir(custom: &str, home_dir: Option<&Path>) -> Option<PathBuf> {
    let trimmed = custom.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.starts_with("file://") {
        return memento_os::decode_file_uri(trimmed);
    }
    if let Some(rest) = trimmed.strip_prefix("~/") {
        return home_dir.map(|home| home.join(rest));
    }
    if trimmed == "~" {
        return home_dir.map(Path::to_path_buf);
    }
    Some(PathBuf::from(trimmed))
}

/// Resolve the starting directory for `FileDialogOptions` from `behavior/file-open-dir`
/// (`0=Current`, `1=Home`, `2=Movies`, `3=Documents`, `4=Custom`) and `behavior/file-open-custom`
/// with existence checks and graceful fallbacks.
pub(crate) fn resolve_file_open_dir(
    dir_kind: i64,
    custom: &str,
    home_dir: Option<&Path>,
    current_dir: Option<&Path>,
    videos_dir: Option<&Path>,
    documents_dir: Option<&Path>,
) -> Option<PathBuf> {
    let existing_dir = |candidate: Option<PathBuf>| candidate.filter(|p| p.is_dir());
    let home_fallback = || {
        existing_dir(home_dir.map(Path::to_path_buf))
            .or_else(|| existing_dir(current_dir.map(Path::to_path_buf)))
    };

    match dir_kind {
        0 => existing_dir(current_dir.map(Path::to_path_buf)).or_else(home_fallback),
        1 => home_fallback(),
        2 => existing_dir(videos_dir.map(Path::to_path_buf))
            .or_else(|| existing_dir(home_dir.map(|h| h.join("Videos"))))
            .or_else(|| existing_dir(home_dir.map(|h| h.join("Movies"))))
            .or_else(home_fallback),
        3 => existing_dir(documents_dir.map(Path::to_path_buf))
            .or_else(|| existing_dir(home_dir.map(|h| h.join("Documents"))))
            .or_else(home_fallback),
        4 => existing_dir(decode_custom_open_dir(custom, home_dir)).or_else(home_fallback),
        _ => existing_dir(videos_dir.map(Path::to_path_buf))
            .or_else(|| existing_dir(home_dir.map(|h| h.join("Videos"))))
            .or_else(|| existing_dir(home_dir.map(|h| h.join("Movies"))))
            .or_else(home_fallback),
    }
}

pub(crate) fn resolve_config_file_open_dir(config: &ConfigController) -> Option<PathBuf> {
    let dir_kind =
        match config.effective_setting(raw::behavior::GROUP, raw::behavior::FILE_OPEN_DIR) {
            Some(Value::Int(v)) => v,
            _ => raw::behavior::FILE_OPEN_DIR_DEFAULT as i64,
        };
    let custom = setting_text(
        config,
        raw::behavior::GROUP,
        raw::behavior::FILE_OPEN_CUSTOM,
        "",
    );
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from);
    let current = std::env::current_dir().ok();
    let user_dirs_content = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| home.as_ref().map(|h| h.join(".config")))
        .and_then(|cfg_dir| std::fs::read_to_string(cfg_dir.join("user-dirs.dirs")).ok());
    let videos = std::env::var_os("XDG_VIDEOS_DIR")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            let home_ref = home.as_deref()?;
            user_dirs_content
                .as_deref()
                .and_then(|c| parse_xdg_user_dir(c, "XDG_VIDEOS_DIR", home_ref))
        });
    let documents = std::env::var_os("XDG_DOCUMENTS_DIR")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            let home_ref = home.as_deref()?;
            user_dirs_content
                .as_deref()
                .and_then(|c| parse_xdg_user_dir(c, "XDG_DOCUMENTS_DIR", home_ref))
        });
    resolve_file_open_dir(
        dir_kind,
        &custom,
        home.as_deref(),
        current.as_deref(),
        videos.as_deref(),
        documents.as_deref(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn migrates_legacy_cache_config_only_when_config_missing() {
        let root = tempfile::TempDir::new().unwrap();
        let legacy_dir = root.path().join("cache/memento");
        let config_dir = root.path().join("config/memento");
        std::fs::create_dir_all(&legacy_dir).unwrap();
        std::fs::write(legacy_dir.join("memento.conf"), "legacy=1\n").unwrap();

        assert!(migrate_xdg_config_if_needed(&legacy_dir, &config_dir, "memento.conf").unwrap());
        assert_eq!(
            std::fs::read_to_string(config_dir.join("memento.conf")).unwrap(),
            "legacy=1\n"
        );

        std::fs::write(config_dir.join("memento.conf"), "existing=2\n").unwrap();
        assert!(!migrate_xdg_config_if_needed(&legacy_dir, &config_dir, "memento.conf").unwrap());
        assert_eq!(
            std::fs::read_to_string(config_dir.join("memento.conf")).unwrap(),
            "existing=2\n"
        );
    }

    #[test]
    fn memento_xdg_dir_supports_windows_macos_and_linux_native_paths_with_fallbacks() {
        let env_from = |pairs: &[(&str, &str)]| {
            let map: HashMap<String, OsString> = pairs
                .iter()
                .map(|(k, v)| ((*k).to_owned(), OsString::from(v)))
                .collect();
            move |key: &str| map.get(key).cloned()
        };

        // Windows: config, data, and cache align under %LOCALAPPDATA%\memento with %APPDATA%\memento fallback.
        let win_env = env_from(&[
            ("APPDATA", "C:\\Users\\Alice\\AppData\\Roaming"),
            ("LOCALAPPDATA", "C:\\Users\\Alice\\AppData\\Local"),
            ("USERPROFILE", "C:\\Users\\Alice"),
        ]);
        let win_dirs = resolve_app_dirs_for(PathPlatform::Windows, &win_env).unwrap();
        assert_eq!(
            win_dirs.config_dir,
            PathBuf::from("C:\\Users\\Alice\\AppData\\Local").join("memento")
        );
        assert_eq!(
            win_dirs.data_dir,
            PathBuf::from("C:\\Users\\Alice\\AppData\\Local").join("memento")
        );
        assert_eq!(
            win_dirs.cache_dir,
            PathBuf::from("C:\\Users\\Alice\\AppData\\Local").join("memento")
        );
        assert_eq!(
            win_dirs.legacy_config_fallback_dir,
            Some(PathBuf::from("C:\\Users\\Alice\\AppData\\Roaming").join("memento"))
        );
        assert!(!win_dirs.use_cache_config_split);
        assert_eq!(
            memento_xdg_dir_with("XDG_CONFIG_HOME", ".config", true, &win_env),
            Some(PathBuf::from("C:\\Users\\Alice\\AppData\\Local").join("memento"))
        );

        // Fallback to USERPROFILE when APPDATA/LOCALAPPDATA are absent.
        let profile_only = env_from(&[("USERPROFILE", "C:\\Users\\Bob")]);
        assert_eq!(
            memento_xdg_dir_with("XDG_CONFIG_HOME", ".config", true, &profile_only),
            Some(
                PathBuf::from("C:\\Users\\Bob")
                    .join(".config")
                    .join("memento")
            )
        );

        // Explicit XDG override takes precedence even on Windows.
        let xdg_override = env_from(&[
            ("XDG_CONFIG_HOME", "/custom/cfg"),
            ("APPDATA", "C:\\Users\\Alice\\AppData\\Roaming"),
            ("LOCALAPPDATA", "C:\\Users\\Alice\\AppData\\Local"),
        ]);
        assert_eq!(
            memento_xdg_dir_with("XDG_CONFIG_HOME", ".config", true, &xdg_override),
            Some(PathBuf::from("/custom/cfg/memento"))
        );

        // macOS: ~/Library/Application Support/memento for config/data, ~/Library/Caches/memento
        // for cache, and ~/.config/memento as automatic fallback read directory.
        let mac_env = env_from(&[("HOME", "/Users/alice")]);
        let mac_dirs = resolve_app_dirs_for(PathPlatform::MacOs, &mac_env).unwrap();
        assert_eq!(
            mac_dirs.config_dir,
            PathBuf::from("/Users/alice/Library/Application Support/memento")
        );
        assert_eq!(
            mac_dirs.data_dir,
            PathBuf::from("/Users/alice/Library/Application Support/memento")
        );
        assert_eq!(
            mac_dirs.cache_dir,
            PathBuf::from("/Users/alice/Library/Caches/memento")
        );
        assert_eq!(
            mac_dirs.legacy_config_fallback_dir,
            Some(PathBuf::from("/Users/alice/.config/memento"))
        );
        assert!(!mac_dirs.use_cache_config_split);

        // Linux: XDG layout with cache config split enabled (#337).
        let linux_env = env_from(&[("HOME", "/home/alice")]);
        let linux_dirs = resolve_app_dirs_for(PathPlatform::Linux, &linux_env).unwrap();
        assert_eq!(
            linux_dirs.config_dir,
            PathBuf::from("/home/alice/.config/memento")
        );
        assert_eq!(
            linux_dirs.data_dir,
            PathBuf::from("/home/alice/.local/share/memento")
        );
        assert_eq!(
            linux_dirs.cache_dir,
            PathBuf::from("/home/alice/.cache/memento")
        );
        assert_eq!(linux_dirs.legacy_config_fallback_dir, None);
        assert!(linux_dirs.use_cache_config_split);

        // Fallback read prefers primary file, otherwise reads existing legacy file.
        let root = tempfile::TempDir::new().unwrap();
        let primary_dir = root.path().join("Library/Application Support/memento");
        let legacy_dir = root.path().join(".config/memento");
        std::fs::create_dir_all(&legacy_dir).unwrap();
        std::fs::write(legacy_dir.join("memento.conf"), "legacy=1\n").unwrap();

        assert_eq!(
            resolve_config_read_path(&primary_dir, "memento.conf", Some(&legacy_dir)),
            legacy_dir.join("memento.conf")
        );
        std::fs::create_dir_all(&primary_dir).unwrap();
        std::fs::write(primary_dir.join("memento.conf"), "primary=1\n").unwrap();
        assert_eq!(
            resolve_config_read_path(&primary_dir, "memento.conf", Some(&legacy_dir)),
            primary_dir.join("memento.conf")
        );
    }

    #[test]
    fn file_open_dir_resolution_covers_all_modes_and_fallbacks() {
        let root = tempfile::TempDir::new().unwrap();
        let home = root.path().join("home");
        let current = root.path().join("cwd");
        let videos = home.join("Videos");
        let documents = home.join("Documents");
        let custom = root.path().join("My Anime");
        for d in [&home, &current, &videos, &documents, &custom] {
            std::fs::create_dir_all(d).unwrap();
        }

        assert_eq!(
            resolve_file_open_dir(
                0,
                "",
                Some(&home),
                Some(&current),
                Some(&videos),
                Some(&documents)
            ),
            Some(current.clone())
        );
        assert_eq!(
            resolve_file_open_dir(
                1,
                "",
                Some(&home),
                Some(&current),
                Some(&videos),
                Some(&documents)
            ),
            Some(home.clone())
        );
        assert_eq!(
            resolve_file_open_dir(
                2,
                "",
                Some(&home),
                Some(&current),
                Some(&videos),
                Some(&documents)
            ),
            Some(videos.clone())
        );
        assert_eq!(
            resolve_file_open_dir(
                3,
                "",
                Some(&home),
                Some(&current),
                Some(&videos),
                Some(&documents)
            ),
            Some(documents.clone())
        );
        assert_eq!(
            resolve_file_open_dir(
                4,
                custom.to_str().unwrap(),
                Some(&home),
                Some(&current),
                Some(&videos),
                Some(&documents),
            ),
            Some(custom.clone())
        );
        let file_uri = format!("file://{}", custom.to_str().unwrap().replace(' ', "%20"));
        assert_eq!(
            resolve_file_open_dir(
                4,
                &file_uri,
                Some(&home),
                Some(&current),
                Some(&videos),
                Some(&documents)
            ),
            Some(custom.clone())
        );
        assert_eq!(
            resolve_file_open_dir(4, "~/Videos", Some(&home), Some(&current), None, None),
            Some(videos.clone())
        );
        assert_eq!(
            resolve_file_open_dir(
                4,
                "/nonexistent/memento/dir/xyz",
                Some(&home),
                Some(&current),
                Some(&videos),
                Some(&documents),
            ),
            Some(home.clone()),
            "missing custom directory must fall back cleanly to home"
        );
    }

    #[test]
    fn spawn_and_reap_leaves_zero_defunct_zombies_and_browse_targets_normalize() {
        let pid = spawn_and_reap(Command::new("true")).expect("should spawn /usr/bin/true");
        #[cfg(target_os = "linux")]
        {
            let stat_path = PathBuf::from(format!("/proc/{pid}/stat"));
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
            while stat_path.exists() && std::time::Instant::now() < deadline {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            if let Ok(stat) = std::fs::read_to_string(&stat_path) {
                let state = stat
                    .rsplit_once(')')
                    .and_then(|(_, rest)| rest.split_whitespace().next());
                assert_ne!(
                    state,
                    Some("Z"),
                    "spawned child pid {pid} became a <defunct> zombie: {stat}"
                );
            }
        }

        let cmd = open_directory_command(Path::new("/tmp/memento-cfg"));
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        assert_eq!(cmd.get_program(), "xdg-open");

        assert_eq!(
            crate::controller::anki::normalize_anki_browse_target("1669909161234").as_deref(),
            Some("nid:1669909161234")
        );
        assert_eq!(
            crate::controller::anki::normalize_anki_browse_target("nid:123 or nid:456").as_deref(),
            Some("nid:123 or nid:456")
        );
        assert_eq!(
            crate::controller::anki::normalize_anki_browse_target(""),
            None
        );
        assert_eq!(
            crate::controller::anki::normalize_anki_browse_target("0"),
            None
        );
    }
}

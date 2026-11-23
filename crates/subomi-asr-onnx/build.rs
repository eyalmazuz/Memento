//! Fetch the matching, official ONNX Runtime only when this backend is enabled.
use sha2::{Digest, Sha256};
use std::{
    env, fs,
    io::Cursor,
    path::{Path, PathBuf},
};

const VERSION: &str = "1.22.0";
const PINNED_MAJOR_MINOR: &str = "1.22";
const MIN_ORT_API_VERSION: u32 = 22;

struct OrtLibMatch {
    best_file: PathBuf,
    windows_import_lib: Option<PathBuf>,
    windows_dll: Option<PathBuf>,
    has_pinned_minor: bool,
    has_linker_name: bool,
    has_soname: bool,
}

fn main() {
    println!("cargo:rerun-if-env-changed=ORT_LIB_LOCATION");
    println!("cargo:rerun-if-env-changed=VIRTUAL_ENV");
    println!("cargo:rerun-if-env-changed=CONDA_PREFIX");

    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let target_env = env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();

    let found_dir = if let Some(directory) =
        env::var_os("ORT_LIB_LOCATION").filter(|v| !v.is_empty())
    {
        let directory = PathBuf::from(directory);
        if directory.join("lib").is_dir() {
            directory.join("lib")
        } else {
            directory
        }
    } else if let Some(cached) = probe_cached_runtime(&out, &target_os) {
        cached
    } else if let Some(local) = probe_local_or_system_runtime(&target_os) {
        local
    } else {
        download_runtime().join("lib")
    };

    let found_dir = found_dir
        .canonicalize()
        .expect("ONNX Runtime library directory does not exist");

    let lib_match = find_ort_library_in_dir(&found_dir, &target_os).unwrap_or_else(|| {
        panic!(
            "No ONNX Runtime library found in {} for target OS '{target_os}'.",
            found_dir.display()
        )
    });
    if let Some(api_version) = detect_ort_api_version(&found_dir, &lib_match) {
        assert!(
            api_version >= MIN_ORT_API_VERSION,
            "ONNX Runtime in {} provides API version {api_version}, but subomi-asr-onnx requires ORT API version >= {MIN_ORT_API_VERSION} (ONNX Runtime >= {VERSION}).",
            found_dir.display()
        );
    }
    if target_os == "windows" && target_env == "msvc" && lib_match.windows_import_lib.is_none() {
        panic!(
            "ONNX Runtime in {} is missing import library 'onnxruntime.lib' required to link on Windows MSVC.",
            found_dir.display()
        );
    }

    let ort_link_dir = prepare_ort_link_dir(&out, &found_dir, &target_os, &lib_match);
    if let Some(ref link_dir) = ort_link_dir {
        println!("cargo:rustc-link-search=native={}", link_dir.display());
    }
    println!("cargo:rustc-link-search=native={}", found_dir.display());
    println!("cargo:rustc-link-lib=onnxruntime");
    let lib_dir = ort_link_dir.as_ref().unwrap_or(&found_dir);
    println!("cargo:lib_dir={}", lib_dir.display());
}

fn is_ort_library_filename(name: &str, target_os: &str) -> bool {
    match target_os {
        "macos" => {
            name == "libonnxruntime.dylib"
                || (name.starts_with("libonnxruntime.") && name.ends_with(".dylib"))
        }
        "windows" => {
            name.eq_ignore_ascii_case("onnxruntime.dll")
                || name.eq_ignore_ascii_case("onnxruntime.lib")
        }
        _ => {
            name == "libonnxruntime.so"
                || name == "libonnxruntime.so.1"
                || name.starts_with("libonnxruntime.so.1.")
        }
    }
}

fn file_matches_pinned_minor(path: &Path, name: &str) -> bool {
    if name.contains(PINNED_MAJOR_MINOR) {
        return true;
    }
    path.canonicalize()
        .ok()
        .and_then(|canon| {
            canon
                .file_name()
                .map(|n| n.to_string_lossy().contains(PINNED_MAJOR_MINOR))
        })
        .unwrap_or(false)
}

fn file_specificity_rank(name: &str, target_os: &str) -> u8 {
    match target_os {
        "macos" => {
            if name.starts_with("libonnxruntime.1.") && name.ends_with(".dylib") {
                3
            } else if name == "libonnxruntime.1.dylib" {
                2
            } else {
                1
            }
        }
        "windows" => {
            if name.eq_ignore_ascii_case("onnxruntime.lib") {
                2
            } else {
                1
            }
        }
        _ => {
            if name.starts_with("libonnxruntime.so.1.") {
                3
            } else if name == "libonnxruntime.so.1" {
                2
            } else {
                1
            }
        }
    }
}

fn find_ort_library_in_dir(dir: &Path, target_os: &str) -> Option<OrtLibMatch> {
    let entries = fs::read_dir(dir).ok()?;
    let mut candidates: Vec<(bool, u8, String, PathBuf)> = Vec::new();
    let mut has_pinned_minor = false;
    let mut windows_import_lib: Option<PathBuf> = None;
    let mut windows_dll: Option<PathBuf> = None;

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !is_ort_library_filename(name, target_os) {
            continue;
        }
        let pinned = file_matches_pinned_minor(&path, name);
        if pinned {
            has_pinned_minor = true;
        }
        if target_os == "windows" {
            let canon = path.canonicalize().unwrap_or_else(|_| path.clone());
            if name.eq_ignore_ascii_case("onnxruntime.lib") {
                windows_import_lib = Some(canon);
            } else if name.eq_ignore_ascii_case("onnxruntime.dll") {
                windows_dll = Some(canon);
            }
        }
        let rank = file_specificity_rank(name, target_os);
        candidates.push((pinned, rank, name.to_owned(), path));
    }

    if candidates.is_empty() {
        return None;
    }

    if target_os == "windows" && windows_dll.is_none() {
        if let Some(sibling_dll) = dir.parent().map(|p| p.join("bin/onnxruntime.dll")) {
            if sibling_dll.is_file() {
                windows_dll = Some(sibling_dll.canonicalize().unwrap_or(sibling_dll));
            }
        }
    }

    candidates.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.cmp(&a.1)).then_with(|| b.2.cmp(&a.2)));
    let best_path = &candidates[0].3;
    let best_file = best_path.canonicalize().unwrap_or_else(|_| best_path.clone());

    let (has_linker_name, has_soname) = match target_os {
        "macos" => (
            dir.join("libonnxruntime.dylib").is_file(),
            dir.join("libonnxruntime.1.dylib").is_file()
                || dir.join("libonnxruntime.dylib").is_file(),
        ),
        "windows" => (
            dir.join("onnxruntime.lib").is_file(),
            dir.join("onnxruntime.dll").is_file(),
        ),
        _ => (
            dir.join("libonnxruntime.so").is_file(),
            dir.join("libonnxruntime.so.1").is_file(),
        ),
    };

    Some(OrtLibMatch {
        best_file,
        windows_import_lib,
        windows_dll,
        has_pinned_minor,
        has_linker_name,
        has_soname,
    })
}

fn parse_ort_api_version_header(path: &Path) -> Option<u32> {
    let content = fs::read_to_string(path).ok()?;
    for line in content.lines() {
        let trimmed = line.trim_start();
        let Some(rest) = trimmed.strip_prefix("#define") else {
            continue;
        };
        let mut parts = rest.split_whitespace();
        if parts.next() == Some("ORT_API_VERSION") {
            if let Some(val) = parts.next() {
                let digits: String = val.chars().take_while(|c| c.is_ascii_digit()).collect();
                if let Ok(ver) = digits.parse::<u32>() {
                    return Some(ver);
                }
            }
        }
    }
    None
}

fn parse_major_minor_from_str(s: &str) -> Option<(u32, u32)> {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_digit() {
            let start_maj = i;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            if i < bytes.len() && bytes[i] == b'.' {
                let maj = s[start_maj..i].parse::<u32>().ok();
                i += 1;
                let start_min = i;
                while i < bytes.len() && bytes[i].is_ascii_digit() {
                    i += 1;
                }
                if start_min < i {
                    if let (Some(maj), Ok(min)) = (maj, s[start_min..i].parse::<u32>()) {
                        if maj >= 1 {
                            return Some((maj, min));
                        }
                    }
                }
            }
        } else {
            i += 1;
        }
    }
    None
}

fn api_version_from_major_minor(major: u32, minor: u32) -> u32 {
    if major > 1 {
        MIN_ORT_API_VERSION.max(minor)
    } else {
        minor
    }
}

fn detect_ort_api_version(dir: &Path, lib_match: &OrtLibMatch) -> Option<u32> {
    // 1. Authoritative: `#define ORT_API_VERSION <N>` in sibling C API headers.
    let mut header_candidates = vec![dir.join("include/onnxruntime_c_api.h")];
    if let Some(parent) = dir.parent() {
        header_candidates.push(parent.join("include/onnxruntime_c_api.h"));
        header_candidates.push(parent.join("include/onnxruntime/onnxruntime_c_api.h"));
        header_candidates
            .push(parent.join("include/onnxruntime/core/session/onnxruntime_c_api.h"));
    }
    for header in header_candidates {
        if header.is_file() {
            if let Some(ver) = parse_ort_api_version_header(&header) {
                return Some(ver);
            }
        }
    }

    // 2. Versioned shared library filename (canonicalized symlink target).
    if let Some(file_name) = lib_match.best_file.file_name().and_then(|n| n.to_str()) {
        if let Some((maj, min)) = parse_major_minor_from_str(file_name) {
            return Some(api_version_from_major_minor(maj, min));
        }
    }

    // 3. Python `onnxruntime` package metadata (`../version_info.py`).
    if let Some(parent) = dir.parent() {
        let version_info = parent.join("version_info.py");
        if let Ok(content) = fs::read_to_string(&version_info) {
            for line in content.lines() {
                let trimmed = line.trim();
                if trimmed.starts_with("version") || trimmed.starts_with("__version__") {
                    if let Some((_, rhs)) = trimmed.split_once('=') {
                        if let Some((maj, min)) = parse_major_minor_from_str(rhs) {
                            return Some(api_version_from_major_minor(maj, min));
                        }
                    }
                }
            }
        }
    }

    // 4. Official archive directory name (e.g. `onnxruntime-win-x64-1.22.0/lib`).
    for candidate in [Some(dir), dir.parent()] {
        if let Some(name) = candidate
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
        {
            if name.contains("onnxruntime") {
                if let Some((maj, min)) = parse_major_minor_from_str(name) {
                    return Some(api_version_from_major_minor(maj, min));
                }
            }
        }
    }

    None
}

fn ensure_link_or_copy(target: &Path, link_path: &Path) {
    if let Ok(existing) = fs::read_link(link_path) {
        if existing == target && link_path.is_file() {
            return;
        }
    }
    let file_name = link_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("libonnxruntime");
    let tmp_path = link_path.with_file_name(format!(".tmp-{file_name}-{}", std::process::id()));
    let _ = fs::remove_file(&tmp_path);

    #[cfg(unix)]
    let linked = std::os::unix::fs::symlink(target, &tmp_path).is_ok();
    #[cfg(windows)]
    let linked = std::os::windows::fs::symlink_file(target, &tmp_path).is_ok();
    #[cfg(not(any(unix, windows)))]
    let linked = false;

    if linked {
        if fs::rename(&tmp_path, link_path).is_ok() {
            return;
        }
        let _ = fs::remove_file(&tmp_path);
    }

    let _ = fs::remove_file(link_path);
    #[cfg(unix)]
    if std::os::unix::fs::symlink(target, link_path).is_ok() {
        return;
    }
    #[cfg(windows)]
    if std::os::windows::fs::symlink_file(target, link_path).is_ok() {
        return;
    }
    fs::copy(target, link_path)
        .expect("could not link or copy ONNX Runtime library into OUT_DIR/ort-link");
}

fn prepare_ort_link_dir(
    out: &Path,
    found_dir: &Path,
    target_os: &str,
    lib_match: &OrtLibMatch,
) -> Option<PathBuf> {
    if lib_match.has_linker_name && lib_match.has_soname {
        return None;
    }
    let link_dir = out.join("ort-link");
    fs::create_dir_all(&link_dir).expect("could not create OUT_DIR/ort-link");

    match target_os {
        "macos" => {
            ensure_link_or_copy(&lib_match.best_file, &link_dir.join("libonnxruntime.dylib"));
            ensure_link_or_copy(
                &lib_match.best_file,
                &link_dir.join("libonnxruntime.1.dylib"),
            );
        }
        "windows" => {
            if let Some(ref import_lib) = lib_match.windows_import_lib {
                ensure_link_or_copy(import_lib, &link_dir.join("onnxruntime.lib"));
            }
            if let Some(ref dll) = lib_match.windows_dll {
                ensure_link_or_copy(dll, &link_dir.join("onnxruntime.dll"));
            }
            let providers_shared = found_dir.join("onnxruntime_providers_shared.dll");
            if providers_shared.is_file() {
                let target = providers_shared
                    .canonicalize()
                    .unwrap_or(providers_shared);
                ensure_link_or_copy(
                    &target,
                    &link_dir.join("onnxruntime_providers_shared.dll"),
                );
            }
        }
        _ => {
            ensure_link_or_copy(&lib_match.best_file, &link_dir.join("libonnxruntime.so"));
            ensure_link_or_copy(&lib_match.best_file, &link_dir.join("libonnxruntime.so.1"));
            let providers_shared = found_dir.join("libonnxruntime_providers_shared.so");
            if providers_shared.is_file() {
                let target = providers_shared
                    .canonicalize()
                    .unwrap_or(providers_shared);
                ensure_link_or_copy(
                    &target,
                    &link_dir.join("libonnxruntime_providers_shared.so"),
                );
            }
        }
    }

    Some(link_dir.canonicalize().unwrap_or(link_dir))
}

fn probe_cached_out_dir(out: &Path, target_os: &str) -> Option<PathBuf> {
    let suffix = format!("-{VERSION}");
    let entries = fs::read_dir(out).ok()?;
    let mut dirs: Vec<PathBuf> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name();
            let name = name.to_str()?;
            if name.starts_with("onnxruntime-") && name.ends_with(&suffix) {
                let lib_dir = entry.path().join("lib");
                if find_ort_library_in_dir(&lib_dir, target_os).is_some() {
                    return Some(lib_dir);
                }
            }
            None
        })
        .collect();
    dirs.sort();
    dirs.into_iter().next()
}

fn probe_cached_runtime(out: &Path, target_os: &str) -> Option<PathBuf> {
    if let Some(dir) = probe_cached_out_dir(out, target_os) {
        return Some(dir);
    }
    if let Some(build_dir) = out.ancestors().nth(2) {
        if let Ok(entries) = fs::read_dir(build_dir) {
            let mut siblings: Vec<PathBuf> = entries
                .flatten()
                .filter_map(|entry| {
                    let name = entry.file_name();
                    let name = name.to_str()?;
                    if name.starts_with("subomi-asr-onnx-") {
                        Some(entry.path().join("out"))
                    } else {
                        None
                    }
                })
                .collect();
            siblings.sort();
            for sibling_out in siblings {
                if let Some(dir) = probe_cached_out_dir(&sibling_out, target_os) {
                    return Some(dir);
                }
            }
        }
    }
    None
}

fn parse_python3_version(name: &str) -> Option<(u32, u32)> {
    let rest = name.strip_prefix("python3")?;
    if rest.is_empty() {
        return Some((3, 0));
    }
    let minor_str = rest.strip_prefix('.').unwrap_or(rest);
    let digits: String = minor_str.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        return Some((3, 0));
    }
    let minor = digits.parse::<u32>().ok()?;
    Some((3, minor))
}

fn collect_python_ort_dirs(lib_root: &Path, target_os: &str) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(lib_root) else {
        return Vec::new();
    };
    let mut found: Vec<(bool, (u32, u32), String, PathBuf)> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let Some(py_ver) = parse_python3_version(name) else {
            continue;
        };
        for pkg_subpath in [
            "site-packages/onnxruntime/capi",
            "dist-packages/onnxruntime/capi",
        ] {
            let capi_dir = path.join(pkg_subpath);
            if let Some(lib_match) = find_ort_library_in_dir(&capi_dir, target_os) {
                found.push((
                    lib_match.has_pinned_minor,
                    py_ver,
                    name.to_owned(),
                    capi_dir,
                ));
            }
        }
    }
    found.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then_with(|| b.1.cmp(&a.1))
            .then_with(|| b.2.cmp(&a.2))
            .then_with(|| b.3.cmp(&a.3))
    });
    found.into_iter().map(|(_, _, _, dir)| dir).collect()
}

fn collect_subdirs_sorted(root: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(root) else {
        return Vec::new();
    };
    let mut dirs: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort_by(|a, b| b.cmp(a));
    dirs
}

fn probe_local_or_system_runtime(target_os: &str) -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();

    for sys_dir in [
        "/usr/lib",
        "/usr/lib/x86_64-linux-gnu",
        "/usr/lib/aarch64-linux-gnu",
        "/usr/local/lib",
        "/opt/homebrew/lib",
    ] {
        candidates.push(PathBuf::from(sys_dir));
    }

    if let Some(venv) = env::var_os("VIRTUAL_ENV").filter(|v| !v.is_empty()) {
        let venv = PathBuf::from(venv);
        candidates.extend(collect_python_ort_dirs(&venv.join("lib"), target_os));
        candidates.extend(collect_python_ort_dirs(&venv.join("lib64"), target_os));
        candidates.push(venv.join("Lib/site-packages/onnxruntime/capi"));
    }

    if let Some(conda) = env::var_os("CONDA_PREFIX").filter(|v| !v.is_empty()) {
        let conda = PathBuf::from(conda);
        candidates.push(conda.join("lib"));
        candidates.extend(collect_python_ort_dirs(&conda.join("lib"), target_os));
        candidates.push(conda.join("Lib/site-packages/onnxruntime/capi"));
    }

    if let Some(home) = env::var_os("HOME")
        .or_else(|| env::var_os("USERPROFILE"))
        .filter(|v| !v.is_empty())
    {
        let home = PathBuf::from(home);
        candidates.extend(collect_python_ort_dirs(&home.join(".local/lib"), target_os));
        candidates.extend(collect_python_ort_dirs(
            &home.join(".local/lib64"),
            target_os,
        ));

        for pipx_venvs in [
            home.join(".local/pipx/venvs"),
            home.join(".local/share/pipx/venvs"),
        ] {
            for venv in collect_subdirs_sorted(&pipx_venvs) {
                candidates.extend(collect_python_ort_dirs(&venv.join("lib"), target_os));
                candidates.extend(collect_python_ort_dirs(&venv.join("lib64"), target_os));
            }
        }

        for conda_root in [
            ".conda",
            "miniconda3",
            "anaconda3",
            "miniforge3",
            "mambaforge",
        ] {
            let base = home.join(conda_root);
            candidates.extend(collect_python_ort_dirs(&base.join("lib"), target_os));
            for env_dir in collect_subdirs_sorted(&base.join("envs")) {
                candidates.extend(collect_python_ort_dirs(&env_dir.join("lib"), target_os));
                candidates.push(env_dir.join("lib"));
            }
        }
    }

    for sys_py_root in [
        "/usr/lib",
        "/usr/lib64",
        "/usr/local/lib",
        "/usr/local/lib64",
        "/opt/homebrew/lib",
    ] {
        candidates.extend(collect_python_ort_dirs(Path::new(sys_py_root), target_os));
    }

    let mut fallback: Option<PathBuf> = None;
    for dir in candidates {
        if let Some(lib_match) = find_ort_library_in_dir(&dir, target_os) {
            if target_os == "windows" && lib_match.windows_import_lib.is_none() {
                continue;
            }
            let Some(api_version) = detect_ort_api_version(&dir, &lib_match) else {
                continue;
            };
            if api_version < MIN_ORT_API_VERSION {
                continue;
            }
            if lib_match.has_pinned_minor {
                return Some(dir);
            }
            if fallback.is_none() {
                fallback = Some(dir);
            }
        }
    }
    fallback
}

fn download_runtime() -> PathBuf {
    let target = env::var("TARGET").unwrap();
    let (platform, checksum) = match target.as_str() {
        "x86_64-unknown-linux-gnu" => ("linux-x64", "8344d55f93d5bc5021ce342db50f62079daf39aaafb5d311a451846228be49b3"),
        "aarch64-unknown-linux-gnu" => ("linux-aarch64", "bb76395092d150b52c7092dc6b8f2fe4d80f0f3bf0416d2f269193e347e24702"),
        "aarch64-apple-darwin" => ("osx-arm64", "cab6dcbd77e7ec775390e7b73a8939d45fec3379b017c7cb74f5b204c1a1cc07"),
        "x86_64-apple-darwin" => ("osx-x86_64", "e4ec94a7696de74fb1b12846569aa94e499958af6ffa186022cfde16c9d617f0"),
        "x86_64-pc-windows-msvc" => ("win-x64", "174c616efc0271194488642a72f1a514e01487da4dfe84c49296d66e40ebe0da"),
        "aarch64-pc-windows-msvc" => ("win-arm64", "7008f7ff82f8e7de563a22f2b590e08e706a1289eba606b93de2b56edfb1e04b"),
        _ => panic!("No official ONNX Runtime archive for {target}. Set ORT_LIB_LOCATION to a compatible ONNX Runtime {VERSION} installation."),
    };
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let name = format!("onnxruntime-{platform}-{VERSION}");
    let directory = out.join(&name);
    let complete = directory.join(".download-complete");
    if complete.is_file() {
        return directory;
    }
    let extension = if platform.starts_with("win-") {
        "zip"
    } else {
        "tgz"
    };
    let url = format!(
        "https://github.com/microsoft/onnxruntime/releases/download/v{VERSION}/{name}.{extension}"
    );
    let mut response = ureq::get(&url)
        .call()
        .expect("could not download the official ONNX Runtime archive");
    let archive = response
        .body_mut()
        .with_config()
        .limit(128 * 1024 * 1024)
        .read_to_vec()
        .expect("could not read ONNX Runtime download");
    assert_eq!(
        format!("{:x}", Sha256::digest(&archive)),
        checksum,
        "ONNX Runtime archive checksum mismatch"
    );
    if extension == "zip" {
        zip::ZipArchive::new(Cursor::new(archive))
            .unwrap()
            .extract(&out)
            .expect("could not unpack ONNX Runtime archive");
    } else {
        let reader = flate2::read::GzDecoder::new(Cursor::new(archive));
        tar::Archive::new(reader)
            .unpack(&out)
            .expect("could not unpack ONNX Runtime archive");
    }
    fs::write(complete, VERSION).unwrap();
    directory
}

//! Bind to the mpv SDK selected by CMake (or a local SDK for standalone Cargo).
//! Never use pregenerated bindings: the patched `mpv_command_ret` ABI must
//! come from the very same client.h that the C++ player compiles against.

use std::path::{Path, PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=MPV_INCLUDE_DIR");
    println!("cargo:rerun-if-env-changed=MPV_SDK");

    let (include_dir, library) = select_sdk();
    let header = include_dir.join("mpv/client.h");
    println!("cargo:rerun-if-changed={}", header.display());
    let render_header = include_dir.join("mpv/render.h");
    println!("cargo:rerun-if-changed={}", render_header.display());
    assert!(
        render_header.is_file(),
        "the selected mpv SDK does not contain mpv/render.h"
    );

    let bindings = bindgen::Builder::default()
        .header(header.to_string_lossy().into_owned())
        .header(render_header.to_string_lossy().into_owned())
        .clang_arg(format!("-I{}", include_dir.display()))
        .rust_target(bindgen::RustTarget::stable(77, 0).unwrap())
        // C examples in client.h are not Rust doctests.
        .generate_comments(false)
        .allowlist_type("mpv_.*")
        .allowlist_function("mpv_.*")
        .prepend_enum_name(false)
        .parse_callbacks(Box::new(bindgen::CargoCallbacks::new()))
        .generate()
        .unwrap_or_else(|error| panic!("cannot bind {}: {error}", header.display()));

    let output = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR is set by Cargo"))
        .join("mpv_bindings.rs");
    bindings
        .write_to_file(&output)
        .unwrap_or_else(|error| panic!("cannot write {}: {error}", output.display()));

    // CMake links mpv::mpv itself. Standalone Cargo tests/binaries must link
    // and load the very same library as the SDK header used by bindgen.
    if let Some(library) = library {
        let dir = library.parent().expect("library has a parent directory");
        println!("cargo:rerun-if-changed={}", library.display());
        println!("cargo:rustc-link-search=native={}", dir.display());
        println!("cargo:rustc-link-lib=dylib=mpv");
        if matches!(
            std::env::var("CARGO_CFG_TARGET_OS").as_deref(),
            Ok("linux" | "macos")
        ) {
            println!("cargo:rustc-link-arg=-Wl,-rpath,{}", dir.display());
        }
    }
}

fn select_sdk() -> (PathBuf, Option<PathBuf>) {
    let manifest = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("Cargo manifest"));
    let repo = manifest.parent().and_then(Path::parent).expect("workspace layout");

    if let Some(explicit) = std::env::var_os("MPV_INCLUDE_DIR") {
        let include = PathBuf::from(explicit);
        assert_header(&include, "MPV_INCLUDE_DIR");
        let library = adjacent_library(&include).or_else(|| cached_library_for(repo, &include));
        return (include, library);
    }

    if let Some(prefix) = std::env::var_os("MPV_SDK") {
        let prefix = PathBuf::from(prefix);
        let include = if prefix.join("mpv/client.h").is_file() {
            prefix.clone()
        } else {
            prefix.join("include")
        };
        assert_header(&include, "MPV_SDK");
        let library = adjacent_library(&include).unwrap_or_else(|| {
            panic!("MPV_SDK={} has no libmpv library beside its headers", prefix.display())
        });
        return (include, Some(library));
    }

    for cache in build_caches(repo) {
        println!("cargo:rerun-if-changed={}", cache.display());
        if let Some((include, library)) = read_cached_sdk(&cache) {
            return (include, Some(library));
        }
    }
    panic!("no mpv SDK found: set MPV_INCLUDE_DIR to CMake's mpv_INCLUDE_DIR, or MPV_SDK to a patched libmpv SDK prefix, or configure a local build*/CMakeCache.txt");
}

fn assert_header(include: &Path, source: &str) {
    assert!(
        include.join("mpv/client.h").is_file(),
        "{source}={} does not contain mpv/client.h",
        include.display()
    );
}

fn adjacent_library(include: &Path) -> Option<PathBuf> {
    let prefix = include.parent()?;
    for directory in ["lib", "lib64"] {
        for name in ["libmpv.so", "libmpv.dylib", "libmpv.dll.a", "mpv.lib"] {
            let library = prefix.join(directory).join(name);
            if library.is_file() {
                return Some(library);
            }
        }
    }
    None
}

fn build_caches(repo: &Path) -> Vec<PathBuf> {
    let mut caches: Vec<_> = std::fs::read_dir(repo)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().starts_with("build"))
        .map(|entry| entry.path().join("CMakeCache.txt"))
        .filter(|cache| cache.is_file())
        .collect();
    caches.sort();
    caches
}

fn cached_library_for(repo: &Path, include: &Path) -> Option<PathBuf> {
    for cache in build_caches(repo) {
        println!("cargo:rerun-if-changed={}", cache.display());
        if let Some((cached_include, library)) = read_cached_sdk(&cache) {
            if cached_include == include {
                return Some(library);
            }
        }
    }
    None
}

fn read_cached_sdk(cache: &Path) -> Option<(PathBuf, PathBuf)> {
    let contents = std::fs::read_to_string(cache).ok()?;
    let mut include = None;
    let mut library = None;
    for line in contents.lines() {
        if let Some(path) = line.strip_prefix("mpv_INCLUDE_DIR:PATH=") {
            include = Some(PathBuf::from(path));
        } else if let Some(path) = line.strip_prefix("mpv_LIBRARY:FILEPATH=") {
            library = Some(PathBuf::from(path));
        }
    }
    let (include, library) = (include?, library?);
    (include.join("mpv/client.h").is_file() && library.is_file())
        .then_some((include, library))
}

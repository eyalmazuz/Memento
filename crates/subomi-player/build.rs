//! Discover the installed libmpv SDK without a project-specific build directory.

use std::path::{Path, PathBuf};

fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn main() {
    for name in ["MPV_SDK", "MPV_INCLUDE_DIR", "MPV_LIB_DIR"] {
        println!("cargo:rerun-if-env-changed={name}");
    }
    let include = if let Some(prefix) = env_path("MPV_SDK") {
        let include = if prefix.join("mpv/client.h").is_file() {
            prefix.clone()
        } else {
            prefix.join("include")
        };
        link_sdk(Some(&prefix), &include);
        include
    } else if let Some(include) = env_path("MPV_INCLUDE_DIR") {
        link_sdk(None, &include);
        include
    } else {
        if let Some(lib_dir) = env_path("MPV_LIB_DIR") {
            let lib_name = detect_mpv_lib_name(&lib_dir).unwrap_or("mpv");
            println!("cargo:rustc-link-search=native={}", lib_dir.display());
            println!("cargo:rustc-link-lib={lib_name}");
        }
        let library = pkg_config::Config::new().probe("mpv").unwrap_or_else(|error| {
            panic!("libmpv development files were not found. Install your system's libmpv development package and pkg-config, then run cargo build again. For a custom SDK set MPV_SDK.\n{error}")
        });
        library
            .include_paths
            .into_iter()
            .chain([
                PathBuf::from("/usr/include"),
                PathBuf::from("/usr/local/include"),
            ])
            .find(|path| path.join("mpv/client.h").is_file())
            .expect("libmpv was found, but its mpv/client.h development header is missing")
    };

    let mut bindings = bindgen::Builder::default()
        .clang_arg(format!("-I{}", include.display()))
        .rust_target(bindgen::RustTarget::stable(77, 0).unwrap())
        .generate_comments(false)
        .allowlist_type("mpv_.*")
        .allowlist_function("mpv_.*")
        .prepend_enum_name(false)
        .parse_callbacks(Box::new(bindgen::CargoCallbacks::new()));
    for name in ["client.h", "render.h", "render_gl.h", "stream_cb.h"] {
        let header = include.join("mpv").join(name);
        assert!(
            header.is_file(),
            "missing libmpv header: {}",
            header.display()
        );
        println!("cargo:rerun-if-changed={}", header.display());
        bindings = bindings.header(header.to_string_lossy());
    }
    bindings
        .generate()
        .expect("could not generate libmpv bindings; install libclang")
        .write_to_file(PathBuf::from(std::env::var_os("OUT_DIR").unwrap()).join("mpv_bindings.rs"))
        .expect("could not write libmpv bindings");
}

fn detect_mpv_lib_name(dir: &Path) -> Option<&'static str> {
    const STANDARD_NAMES: &[&str] = &[
        "libmpv.so",
        "libmpv.dylib",
        "libmpv.dll.a",
        "mpv.dll.a",
        "mpv.lib",
        "libmpv.a",
    ];
    if STANDARD_NAMES.iter().any(|name| dir.join(name).is_file()) {
        return Some("mpv");
    }
    if dir.join("libmpv.lib").is_file() {
        return Some("libmpv");
    }
    None
}

fn link_sdk(sdk_root: Option<&Path>, include: &Path) {
    if let Some(directory) = env_path("MPV_LIB_DIR") {
        let lib_name = detect_mpv_lib_name(&directory).unwrap_or("mpv");
        println!("cargo:rustc-link-search=native={}", directory.display());
        println!("cargo:rustc-link-lib={lib_name}");
        return;
    }

    let mut candidates = Vec::new();
    if let Some(root) = sdk_root {
        candidates.push(root.to_path_buf());
        candidates.push(root.join("lib"));
        candidates.push(root.join("lib64"));
        candidates.push(root.join("x86_64"));
    }
    if let Some(parent) = include.parent() {
        candidates.push(parent.to_path_buf());
        candidates.push(parent.join("lib"));
        candidates.push(parent.join("lib64"));
    }

    for directory in candidates {
        if let Some(lib_name) = detect_mpv_lib_name(&directory) {
            println!("cargo:rustc-link-search=native={}", directory.display());
            println!("cargo:rustc-link-lib={lib_name}");
            return;
        }
    }

    pkg_config::Config::new()
        .probe("mpv")
        .expect("cannot find libmpv beside the selected headers; set MPV_LIB_DIR");
}

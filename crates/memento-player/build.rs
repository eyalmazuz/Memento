//! Discover the installed libmpv SDK without a project-specific build directory.

use std::path::{Path, PathBuf};

fn main() {
    for name in ["MPV_SDK", "MPV_INCLUDE_DIR", "MPV_LIB_DIR"] {
        println!("cargo:rerun-if-env-changed={name}");
    }
    let include = if let Some(prefix) = std::env::var_os("MPV_SDK") {
        let prefix = PathBuf::from(prefix);
        let include = if prefix.join("mpv/client.h").is_file() {
            prefix.clone()
        } else {
            prefix.join("include")
        };
        link_sdk(&include);
        include
    } else if let Some(include) = std::env::var_os("MPV_INCLUDE_DIR") {
        let include = PathBuf::from(include);
        link_sdk(&include);
        include
    } else {
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
    for name in ["client.h", "render.h", "render_gl.h"] {
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

fn link_sdk(include: &Path) {
    let library_dir = std::env::var_os("MPV_LIB_DIR")
        .map(PathBuf::from)
        .or_else(|| {
            let prefix = include.parent()?;
            ["lib", "lib64"]
                .into_iter()
                .map(|name| prefix.join(name))
                .find(|dir| {
                    ["libmpv.so", "libmpv.dylib", "libmpv.dll.a", "mpv.lib"]
                        .iter()
                        .any(|name| dir.join(name).is_file())
                })
        });
    if let Some(directory) = library_dir {
        println!("cargo:rustc-link-search=native={}", directory.display());
        println!("cargo:rustc-link-lib=mpv");
    } else {
        pkg_config::Config::new()
            .probe("mpv")
            .expect("cannot find libmpv beside the selected headers; set MPV_LIB_DIR");
    }
}

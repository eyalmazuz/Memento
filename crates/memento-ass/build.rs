//! Discover the installed system `libass` shared library (or `ASS_LIB_DIR` / `ASS_SDK` override).

use std::path::{Path, PathBuf};

fn main() {
    for var in ["ASS_SDK", "ASS_LIB_DIR"] {
        println!("cargo:rerun-if-env-changed={var}");
    }

    if let Some(lib_dir) = std::env::var_os("ASS_LIB_DIR") {
        let dir = PathBuf::from(lib_dir);
        println!("cargo:rustc-link-search=native={}", dir.display());
        println!("cargo:rustc-link-lib=ass");
        return;
    }

    if let Some(sdk) = std::env::var_os("ASS_SDK") {
        let prefix = PathBuf::from(sdk);
        if let Some(dir) = find_lib_dir(&prefix) {
            println!("cargo:rustc-link-search=native={}", dir.display());
            println!("cargo:rustc-link-lib=ass");
            return;
        }
    }

    if let Err(err) = pkg_config::Config::new()
        .atleast_version("0.14.0")
        .probe("libass")
    {
        panic!(
            "libass development package was not found via pkg-config ({err}). \
             Install libass (e.g. libass-dev / libass) or set ASS_LIB_DIR."
        );
    }
}

fn find_lib_dir(prefix: &Path) -> Option<PathBuf> {
    ["lib", "lib64", ""]
        .into_iter()
        .map(|sub| if sub.is_empty() { prefix.to_path_buf() } else { prefix.join(sub) })
        .find(|dir| {
            ["libass.so", "libass.so.9", "libass.dylib", "libass.dll.a", "ass.lib"]
                .iter()
                .any(|name| dir.join(name).is_file())
        })
}

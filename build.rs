use std::ffi::OsStr;
use std::path::{Path, PathBuf};

fn target_profile_dir(out_dir: &Path) -> Option<&Path> {
    out_dir
        .ancestors()
        .find(|dir| dir.file_name() == Some(OsStr::new("build")))
        .and_then(Path::parent)
        .or_else(|| out_dir.ancestors().nth(3))
}

fn main() {
    println!("cargo:rerun-if-env-changed=DEP_ONNXRUNTIME_LIB_DIR");

    // Cargo supplies this metadata only when the optional ONNX crate is built.
    // Keep directly running target/debug/memento working as well as cargo run.
    if let Ok(directory) = std::env::var("DEP_ONNXRUNTIME_LIB_DIR") {
        let target =
            std::env::var("CARGO_CFG_TARGET_OS").expect("CARGO_CFG_TARGET_OS must be set by Cargo");
        if target == "linux" || target == "macos" {
            println!("cargo:rustc-link-arg=-Wl,-rpath,{directory}");
        } else if target == "windows" {
            let out =
                PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR must be set by Cargo"));
            let profile = target_profile_dir(&out)
                .expect("failed to locate Cargo target profile directory from OUT_DIR");
            for entry in std::fs::read_dir(&directory)
                .expect("failed to read DEP_ONNXRUNTIME_LIB_DIR")
                .flatten()
            {
                let source = entry.path();
                if source
                    .extension()
                    .is_some_and(|extension| extension == "dll")
                {
                    std::fs::copy(&source, profile.join(entry.file_name()))
                        .expect("failed to copy ONNX Runtime DLL into target profile directory");
                }
            }
        }
    }
    slint_build::compile("ui/app.slint").expect("failed to compile Slint UI definitions");
}

fn main() {
    // Cargo supplies this metadata only when the optional ONNX crate is built.
    // Keep directly running target/debug/memento working as well as cargo run.
    if let Ok(directory) = std::env::var("DEP_ONNXRUNTIME_LIB_DIR") {
        let target = std::env::var("CARGO_CFG_TARGET_OS").unwrap();
        if target == "linux" || target == "macos" {
            println!("cargo:rustc-link-arg=-Wl,-rpath,{directory}");
        } else if target == "windows" {
            let out = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
            let profile = out.ancestors().nth(3).unwrap();
            for entry in std::fs::read_dir(directory).unwrap().flatten() {
                if entry
                    .path()
                    .extension()
                    .is_some_and(|extension| extension == "dll")
                {
                    std::fs::copy(entry.path(), profile.join(entry.file_name())).unwrap();
                }
            }
        }
    }
    slint_build::compile("ui/app.slint").expect("failed to compile Slint UI definitions");
}

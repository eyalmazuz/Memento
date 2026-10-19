use std::path::PathBuf;

fn main() {
    slint_build::compile("ui/app.slint").expect("failed to compile Slint UI definitions");

    let manifest = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("Cargo manifest"));
    let repo = if manifest.join("build-rust-master").is_dir() || manifest.join("build").is_dir() {
        manifest.clone()
    } else {
        manifest.parent().and_then(std::path::Path::parent).unwrap_or(&manifest).to_path_buf()
    };

    let build_dirs = [
        repo.join("build-rust-master"),
        repo.join("build"),
    ];

    for build_dir in &build_dirs {
        let whisper_dir = build_dir.join("_deps/whispercpp-build/src");
        let ggml_dir = build_dir.join("_deps/whispercpp-build/ggml/src");

        if whisper_dir.join("libwhisper.a").is_file() {
            println!("cargo:rustc-link-search=native={}", whisper_dir.display());
            println!("cargo:rustc-link-search=native={}", ggml_dir.display());
            println!("cargo:rustc-link-arg=-Wl,--whole-archive");
            println!("cargo:rustc-link-arg={}/libwhisper.a", whisper_dir.display());
            println!("cargo:rustc-link-arg={}/libggml.a", ggml_dir.display());
            println!("cargo:rustc-link-arg={}/libggml-base.a", ggml_dir.display());
            println!("cargo:rustc-link-arg={}/libggml-cpu.a", ggml_dir.display());
            println!("cargo:rustc-link-arg=-Wl,--no-whole-archive");
            println!("cargo:rustc-link-arg=-lgomp");
            println!("cargo:rustc-link-arg=-lstdc++");
            break;
        }
    }

    for build_dir in &build_dirs {
        let onnx_dir = build_dir.join("_deps/onnxruntime_prebuilt-src/lib");
        if onnx_dir.join("libonnxruntime.so").is_file() {
            println!("cargo:rustc-link-search=native={}", onnx_dir.display());
            println!("cargo:rustc-link-arg=-L{}", onnx_dir.display());
            println!("cargo:rustc-link-arg=-lonnxruntime");
            println!("cargo:rustc-link-arg=-Wl,-rpath,{}", onnx_dir.display());
            break;
        }
    }
}

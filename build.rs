use std::path::PathBuf;

fn main() {
    slint_build::compile("ui/app.slint").expect("failed to compile Slint UI definitions");
    println!("cargo:rerun-if-env-changed=MEMENTO_WHISPER_BUILD_DIR");
    println!("cargo:rerun-if-env-changed=CUDA_HOME");
    println!("cargo:rerun-if-env-changed=CUDA_PATH");

    let manifest = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("Cargo manifest"));
    let repo = if manifest.join("build-rust-master").is_dir() || manifest.join("build").is_dir() {
        manifest.clone()
    } else {
        manifest.parent().and_then(std::path::Path::parent).unwrap_or(&manifest).to_path_buf()
    };

    let mut build_dirs = Vec::new();
    if let Some(dir) = std::env::var_os("MEMENTO_WHISPER_BUILD_DIR") {
        let dir = PathBuf::from(dir);
        build_dirs.push(if dir.is_absolute() { dir } else { manifest.join(dir) });
    }
    build_dirs.push(repo.join("build-whisper-cuda"));
    build_dirs.push(repo.join("build-rust-master"));
    build_dirs.push(repo.join("build"));

    for build_dir in &build_dirs {
        let whisper_dir = build_dir.join("_deps/whispercpp-build/src");
        let ggml_dir = build_dir.join("_deps/whispercpp-build/ggml/src");
        println!("cargo:rerun-if-changed={}", whisper_dir.join("libwhisper.a").display());

        if whisper_dir.join("libwhisper.a").is_file() {
            let cuda_lib = ggml_dir.join("ggml-cuda/libggml-cuda.a");
            let vulkan_lib = ggml_dir.join("ggml-vulkan/libggml-vulkan.a");
            for library in [
                whisper_dir.join("libwhisper.a"),
                ggml_dir.join("libggml.a"),
                ggml_dir.join("libggml-base.a"),
                ggml_dir.join("libggml-cpu.a"),
            ] {
                println!("cargo:rerun-if-changed={}", library.display());
            }
            println!("cargo:rerun-if-changed={}", cuda_lib.display());
            println!("cargo:rerun-if-changed={}", vulkan_lib.display());
            println!("cargo:rustc-link-search=native={}", whisper_dir.display());
            println!("cargo:rustc-link-search=native={}", ggml_dir.display());
            println!("cargo:rustc-link-arg=-Wl,--whole-archive");
            println!("cargo:rustc-link-arg={}/libwhisper.a", whisper_dir.display());
            println!("cargo:rustc-link-arg={}/libggml.a", ggml_dir.display());
            println!("cargo:rustc-link-arg={}/libggml-base.a", ggml_dir.display());
            println!("cargo:rustc-link-arg={}/libggml-cpu.a", ggml_dir.display());
            if cuda_lib.is_file() {
                println!("cargo:rustc-link-arg={}", cuda_lib.display());
            }
            if vulkan_lib.is_file() {
                println!("cargo:rustc-link-arg={}", vulkan_lib.display());
            }
            println!("cargo:rustc-link-arg=-Wl,--no-whole-archive");
            if cuda_lib.is_file() {
                let cuda_home = std::env::var_os("CUDA_HOME")
                    .or_else(|| std::env::var_os("CUDA_PATH"))
                    .map(PathBuf::from)
                    .or_else(|| ["/usr/local/cuda", "/opt/cuda"].into_iter()
                        .map(PathBuf::from).find(|path| path.is_dir()));
                let mut lib_dirs = Vec::new();
                if let Some(cuda_home) = cuda_home {
                    lib_dirs.extend([cuda_home.join("lib64"), cuda_home.join("lib")]);
                    if let Ok(targets) = std::fs::read_dir(cuda_home.join("targets")) {
                        lib_dirs.extend(targets.filter_map(Result::ok)
                            .map(|entry| entry.path().join("lib")));
                    }
                }
                for lib_dir in lib_dirs {
                    if lib_dir.is_dir() {
                        println!("cargo:rustc-link-search=native={}", lib_dir.display());
                        println!("cargo:rustc-link-arg=-Wl,-rpath,{}", lib_dir.display());
                    }
                }
                println!("cargo:rustc-link-arg=-lcudart");
                println!("cargo:rustc-link-arg=-lcublas");
                println!("cargo:rustc-link-arg=-lcublasLt");
                println!("cargo:rustc-link-arg=-lcuda");
            }
            if vulkan_lib.is_file() {
                println!("cargo:rustc-link-arg=-lvulkan");
            }
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

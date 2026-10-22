//! Build the pinned speech backend through Cargo, only when Whisper is enabled.

use sha2::{Digest, Sha256};
use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};

const VERSION: &str = "1.9.4";
const SHA256: &str = "57e280cee375ab02425b806ad5146b99f6eb9357e3c2b31357c8a6af2e2e44ae";

fn main() {
    for name in [
        "MEMENTO_WHISPER_BUILD_DIR",
        "WHISPER_INCLUDE_DIR",
        "GGML_INCLUDE_DIR",
        "CUDA_HOME",
        "CUDA_PATH",
        "CMAKE_CUDA_ARCHITECTURES",
    ] {
        println!("cargo:rerun-if-env-changed={name}");
    }
    let out = PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    let cuda = std::env::var_os("CARGO_FEATURE_CUDA").is_some();
    let vulkan = std::env::var_os("CARGO_FEATURE_VULKAN").is_some();
    let (lib_dirs, source) = if let Some(build) = std::env::var_os("MEMENTO_WHISPER_BUILD_DIR") {
        let build = PathBuf::from(build).join("_deps/whispercpp-build");
        assert!(
            build.join("src/libwhisper.a").is_file(),
            "MEMENTO_WHISPER_BUILD_DIR does not contain a compiled Whisper backend"
        );
        let cache_path = build.join("CMakeCache.txt");
        println!("cargo:rerun-if-changed={}", cache_path.display());
        let cache = fs::read_to_string(cache_path).unwrap_or_default();
        for (name, enabled) in [("GGML_CUDA", cuda), ("GGML_VULKAN", vulkan)] {
            if cache.contains(&format!("{name}:BOOL=ON")) != enabled {
                panic!("The prepared Whisper build's {name} setting does not match the selected Cargo features");
            }
        }
        // A reused native build may have OpenMP enabled even though Cargo's
        // default backend does not. Link its actual runtime when recorded.
        for line in cache.lines() {
            if line.starts_with("OpenMP_") && line.contains("_LIBRARY:FILEPATH=") {
                if let Some((_, library)) = line.split_once('=') {
                    if Path::new(library).is_file() {
                        println!(
                            "cargo:rustc-link-search=native={}",
                            Path::new(library).parent().unwrap().display()
                        );
                        if let Some(name) = Path::new(library)
                            .file_name()
                            .and_then(|name| name.to_str())
                            .and_then(|name| name.strip_prefix("lib"))
                        {
                            println!("cargo:rustc-link-lib={}", name.split('.').next().unwrap());
                        }
                    }
                }
            }
        }
        let mut dirs = vec![build.join("src"), build.join("ggml/src")];
        if cuda {
            dirs.push(build.join("ggml/src/ggml-cuda"));
        }
        if vulkan {
            dirs.push(build.join("ggml/src/ggml-vulkan"));
        }
        (dirs, None)
    } else {
        let source = download_source(&out);
        let mut config = cmake::Config::new(&source);
        config
            .profile("Release")
            .define("BUILD_SHARED_LIBS", "OFF")
            .define("CMAKE_INSTALL_LIBDIR", "lib")
            .define("WHISPER_BUILD_TESTS", "OFF")
            .define("WHISPER_BUILD_EXAMPLES", "OFF")
            .define("WHISPER_BUILD_SERVER", "OFF")
            .define("GGML_NATIVE", "OFF")
            .define("GGML_OPENMP", "OFF")
            .define("GGML_BLAS", "OFF")
            .define("GGML_METAL", "OFF")
            .define("GGML_ACCELERATE", "OFF")
            .define("GGML_CUDA", if cuda { "ON" } else { "OFF" })
            .define("GGML_VULKAN", if vulkan { "ON" } else { "OFF" });
        // GGML enables AVX2 by default when GGML_NATIVE is off. Match Rust's
        // actual target features instead, so a normal build stays portable.
        let target_features = std::env::var("CARGO_CFG_TARGET_FEATURE").unwrap_or_default();
        for (option, feature) in [
            ("GGML_SSE42", "sse4.2"),
            ("GGML_AVX", "avx"),
            ("GGML_AVX2", "avx2"),
            ("GGML_BMI2", "bmi2"),
            ("GGML_FMA", "fma"),
            ("GGML_F16C", "f16c"),
        ] {
            config.define(
                option,
                if target_features.split(',').any(|value| value == feature) {
                    "ON"
                } else {
                    "OFF"
                },
            );
        }
        if let Some(arch) = std::env::var_os("CMAKE_CUDA_ARCHITECTURES") {
            config.define("CMAKE_CUDA_ARCHITECTURES", arch);
        }
        if cuda {
            if let Some(toolkit) =
                std::env::var_os("CUDA_HOME").or_else(|| std::env::var_os("CUDA_PATH"))
            {
                config.define("CUDAToolkit_ROOT", toolkit);
            }
        }
        let installed = config.build();
        (vec![installed.join("lib")], Some(source))
    };
    for directory in &lib_dirs {
        println!("cargo:rustc-link-search=native={}", directory.display());
    }
    for library in ["whisper", "ggml", "ggml-cpu", "ggml-base"] {
        println!("cargo:rustc-link-lib=static={library}");
    }
    if cuda {
        println!("cargo:rustc-link-lib=static=ggml-cuda");
        let toolkit = std::env::var_os("CUDA_HOME")
            .or_else(|| std::env::var_os("CUDA_PATH"))
            .map(PathBuf::from)
            .or_else(|| {
                ["/opt/cuda", "/usr/local/cuda"]
                    .into_iter()
                    .map(PathBuf::from)
                    .find(|path| path.is_dir())
            });
        if let Some(toolkit) = toolkit {
            for directory in [
                toolkit.join("lib64"),
                toolkit.join("lib"),
                toolkit.join("lib/x64"),
            ] {
                if directory.is_dir() {
                    println!("cargo:rustc-link-search=native={}", directory.display());
                }
            }
        }
        for library in ["cuda", "cudart", "cublas", "cublasLt"] {
            println!("cargo:rustc-link-lib={library}");
        }
    }
    if vulkan {
        println!("cargo:rustc-link-lib=static=ggml-vulkan");
        println!("cargo:rustc-link-lib=vulkan");
    }
    let target = std::env::var("TARGET").unwrap();
    if target.contains("apple") || target.contains("freebsd") {
        println!("cargo:rustc-link-lib=c++");
    } else if !target.contains("msvc") {
        println!("cargo:rustc-link-lib=stdc++");
    }

    // Generate against the exact native source being linked. For an explicit
    // prebuilt backend, use its headers when supplied, otherwise the pinned ABI.
    let include = std::env::var_os("WHISPER_INCLUDE_DIR")
        .map(PathBuf::from)
        .or_else(|| source.as_ref().map(|path| path.join("include")));
    if let Some(include) = include {
        let ggml = std::env::var_os("GGML_INCLUDE_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| include.parent().unwrap().join("ggml/include"));
        bindgen::Builder::default()
            .header(include.join("whisper.h").to_string_lossy())
            .clang_arg(format!("-I{}", include.display()))
            .clang_arg(format!("-I{}", ggml.display()))
            .allowlist_function("whisper_.*")
            .allowlist_type("whisper_.*")
            .allowlist_type("ggml_abort_callback")
            .allowlist_var("WHISPER_.*")
            .derive_default(true)
            .generate()
            .expect("could not generate Whisper bindings")
            .write_to_file(out.join("whisper_bindings.rs"))
            .unwrap();
    } else {
        println!("cargo:rerun-if-changed=src/whisper_bindings_pregenerated.rs");
        fs::copy(
            "src/whisper_bindings_pregenerated.rs",
            out.join("whisper_bindings.rs"),
        )
        .unwrap();
    }
}

fn download_source(out: &Path) -> PathBuf {
    let source = out.join(format!("whisper.cpp-{VERSION}"));
    let complete = out.join("whisper-source-complete");
    if complete.is_file() && source.join("include/whisper.h").is_file() {
        return source;
    }
    let url =
        format!("https://codeload.github.com/ggml-org/whisper.cpp/tar.gz/refs/tags/v{VERSION}");
    let mut response = ureq::get(&url)
        .call()
        .expect("could not download whisper.cpp sources");
    let archive = response
        .body_mut()
        .with_config()
        .limit(64 * 1024 * 1024)
        .read_to_vec()
        .expect("could not read whisper.cpp download");
    assert_eq!(
        format!("{:x}", Sha256::digest(&archive)),
        SHA256,
        "whisper.cpp archive checksum mismatch"
    );
    let reader = flate2::read::GzDecoder::new(Cursor::new(archive));
    tar::Archive::new(reader)
        .unpack(out)
        .expect("could not unpack whisper.cpp sources");
    assert!(source.join("include/whisper.h").is_file());
    fs::write(complete, VERSION).unwrap();
    source
}

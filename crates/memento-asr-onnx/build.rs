//! Fetch the matching, official ONNX Runtime only when this backend is enabled.
use sha2::{Digest, Sha256};
use std::{env, fs, io::Cursor, path::PathBuf};

const VERSION: &str = "1.22.0";

fn main() {
    println!("cargo:rerun-if-env-changed=ORT_LIB_LOCATION");
    let directory = if let Some(directory) = env::var_os("ORT_LIB_LOCATION") {
        let directory = PathBuf::from(directory);
        if directory.join("lib").is_dir() {
            directory.join("lib")
        } else {
            directory
        }
    } else {
        download_runtime().join("lib")
    };
    let directory = directory
        .canonicalize()
        .expect("ONNX Runtime library directory does not exist");
    println!("cargo:rustc-link-search=native={}", directory.display());
    println!("cargo:rustc-link-lib=onnxruntime");
    println!("cargo:lib_dir={}", directory.display());
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

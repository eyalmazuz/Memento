use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;

fn target_profile_dir(out_dir: &Path) -> Option<&Path> {
    out_dir
        .ancestors()
        .find(|dir| dir.file_name() == Some(OsStr::new("build")))
        .and_then(Path::parent)
        .or_else(|| out_dir.ancestors().nth(3))
}

fn resolve_git_head_path(manifest_dir: &Path) -> Option<PathBuf> {
    let dot_git = manifest_dir.join(".git");
    if dot_git.is_dir() {
        let head = dot_git.join("HEAD");
        return head.is_file().then_some(head);
    }
    if dot_git.is_file() {
        let contents = std::fs::read_to_string(&dot_git).ok()?;
        let raw_dir = contents.trim().strip_prefix("gitdir:")?.trim();
        if raw_dir.is_empty() {
            return None;
        }
        let git_dir = PathBuf::from(raw_dir);
        let resolved = if git_dir.is_absolute() {
            git_dir
        } else {
            manifest_dir.join(git_dir)
        };
        let head = resolved.join("HEAD");
        return head.is_file().then_some(head);
    }
    None
}

fn emit_git_hash(manifest_dir: &Path) {
    if let Some(head_path) = resolve_git_head_path(manifest_dir) {
        println!("cargo:rerun-if-changed={}", head_path.display());
    }
    let hash = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .current_dir(manifest_dir)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_hexdigit()))
        .unwrap_or_default();
    println!("cargo:rustc-env=SUBOMI_GIT_HASH={hash}");
}

fn try_compile_windows_icon(manifest_dir: &Path, out_dir: &Path) {
    let res_dir = manifest_dir.join("res");
    let ico_path = res_dir.join("subomi.ico");
    let rc_path = res_dir.join("appicon.rc");

    if ico_path.is_file() {
        println!("cargo:rerun-if-changed={}", ico_path.display());
    }
    if rc_path.is_file() {
        println!("cargo:rerun-if-changed={}", rc_path.display());
    }
    if !ico_path.is_file() {
        println!(
            "cargo:warning=res/subomi.ico not found; skipping Windows application icon embedding"
        );
        return;
    }

    // Generate a self-contained .rc file pointing directly at res/subomi.ico so
    // rc.exe, llvm-rc, and windres resolve the icon regardless of working dir.
    let generated_rc = out_dir.join("subomi_appicon.rc");
    let escaped_ico = ico_path.to_string_lossy().replace('\\', "\\\\");
    let rc_contents = format!("MAINICON ICON \"{escaped_ico}\"\n");
    if let Err(err) = std::fs::write(&generated_rc, rc_contents) {
        println!(
            "cargo:warning=Could not write temporary Windows resource script: {err}; skipping icon embedding"
        );
        return;
    }

    let target_env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    let target_arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    let out_res = out_dir.join("subomi_appicon.res");
    let out_obj = out_dir.join("subomi_appicon.o");

    let run_rc = |tool: &str, out_flag: &str| -> bool {
        Command::new(tool)
            .arg(out_flag)
            .arg(&out_res)
            .arg(&generated_rc)
            .status()
            .is_ok_and(|s| s.success())
            && out_res.is_file()
    };

    let run_windres = |tool: &str| -> bool {
        Command::new(tool)
            .args(["-O", "coff", "-o"])
            .arg(&out_obj)
            .arg(&generated_rc)
            .status()
            .is_ok_and(|s| s.success())
            && out_obj.is_file()
    };

    let mingw_windres = match target_arch.as_str() {
        "x86_64" => "x86_64-w64-mingw32-windres",
        "x86" => "i686-w64-mingw32-windres",
        "aarch64" => "aarch64-w64-mingw32-windres",
        _ => "windres",
    };

    let embedded_artifact = if target_env == "gnu" {
        if run_windres(mingw_windres) || run_windres("windres") {
            Some(out_obj)
        } else if run_rc("llvm-rc", "/FO") || run_rc("rc.exe", "/fo") {
            Some(out_res)
        } else {
            None
        }
    } else if run_rc("rc.exe", "/fo") || run_rc("llvm-rc", "/FO") {
        Some(out_res)
    } else if run_windres(mingw_windres) || run_windres("windres") {
        Some(out_obj)
    } else {
        None
    };

    match embedded_artifact {
        Some(artifact) => {
            println!("cargo:rustc-link-arg-bins={}", artifact.display());
        }
        None => {
            println!(
                "cargo:warning=Could not embed res/subomi.ico (none of rc.exe, llvm-rc, or windres succeeded); continuing without Windows icon resource"
            );
        }
    }
}

fn main() {
    println!("cargo:rerun-if-env-changed=DEP_ONNXRUNTIME_LIB_DIR");

    let manifest_dir = PathBuf::from(
        std::env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR must be set by Cargo"),
    );
    let out_dir =
        PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR must be set by Cargo"));
    let target =
        std::env::var("CARGO_CFG_TARGET_OS").expect("CARGO_CFG_TARGET_OS must be set by Cargo");

    emit_git_hash(&manifest_dir);

    if target == "windows" {
        try_compile_windows_icon(&manifest_dir, &out_dir);
    }

    // Cargo supplies this metadata only when the optional ONNX crate is built.
    // Keep directly running target/debug/subomi working as well as cargo run.
    if let Ok(directory) = std::env::var("DEP_ONNXRUNTIME_LIB_DIR") {
        if target == "linux" || target == "macos" {
            println!("cargo:rustc-link-arg=-Wl,-rpath,{directory}");
        } else if target == "windows" {
            let profile = target_profile_dir(&out_dir)
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

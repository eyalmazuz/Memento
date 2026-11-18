////////////////////////////////////////////////////////////////////////////////
//
// Copyright (c) 2026 Ripose
//
// This file is part of Memento.
//
// Memento is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, version 2 of the License.
//
// Memento is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with Memento.  If not, see <https://www.gnu.org/licenses/>.
//
////////////////////////////////////////////////////////////////////////////////

//! Persistent in-process `libmecab` (`libloading`) tagger with a warm piped
//! `mecab` subprocess daemon fallback.

use std::ffi::{c_char, c_void, CStr};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::{Mutex, OnceLock};

type MecabNew2Fn = unsafe extern "C" fn(*const c_char) -> *mut c_void;
type MecabSparseToStr2Fn =
    unsafe extern "C" fn(*mut c_void, *const c_char, usize) -> *const c_char;
type MecabStrerrorFn = unsafe extern "C" fn(*mut c_void) -> *const c_char;
type MecabDestroyFn = unsafe extern "C" fn(*mut c_void);

/// Dynamically loaded in-process `libmecab` tagger.
///
/// The internal buffer returned by `mecab_sparse_tostr2` belongs to the
/// `mecab_t` instance and is freed only when `mecab_destroy` is called on the
/// owned `tagger` pointer in [`Drop`].
pub struct LibMecabTagger {
    tagger: *mut c_void,
    mecab_sparse_tostr2: MecabSparseToStr2Fn,
    mecab_strerror: MecabStrerrorFn,
    mecab_destroy: MecabDestroyFn,
    _lib: libloading::Library,
}

// Safety: `mecab_t` is only accessed through `&mut self` (guarded by a `Mutex`
// when shared across threads).
unsafe impl Send for LibMecabTagger {}

impl LibMecabTagger {
    /// Load `libmecab` from standard platform library names with default options.
    pub fn new() -> Result<Self, String> {
        let default_arg = crate::quickutils::format_mecab_tagger_arg(false, false, b"", b"");
        Self::new_with_config(None, &default_arg)
    }

    /// Load `libmecab` from an optional explicit library path and raw tagger argument bytes
    /// (such as those produced by [`crate::quickutils::format_mecab_tagger_arg`]).
    pub fn new_with_config(lib_path: Option<&str>, tagger_arg: &[u8]) -> Result<Self, String> {
        let mut candidates: Vec<&str> = Vec::new();
        if let Some(custom) = lib_path {
            if !custom.is_empty() {
                candidates.push(custom);
            }
        }
        #[cfg(target_os = "macos")]
        {
            candidates.extend_from_slice(&[
                "libmecab.dylib",
                "libmecab.2.dylib",
                "/opt/homebrew/lib/libmecab.dylib",
                "/usr/local/lib/libmecab.dylib",
            ]);
        }
        #[cfg(target_os = "windows")]
        {
            candidates.extend_from_slice(&["libmecab.dll", "mecab.dll"]);
        }
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        {
            candidates.extend_from_slice(&[
                "libmecab.so.2",
                "libmecab.so",
                "/usr/lib/libmecab.so.2",
                "/usr/lib/libmecab.so",
                "/usr/lib/x86_64-linux-gnu/libmecab.so.2",
                "/usr/local/lib/libmecab.so.2",
                "/usr/local/lib/libmecab.so",
            ]);
        }

        let mut last_err = String::from("no libmecab candidate paths configured");
        let mut loaded_lib = None;
        for candidate in candidates {
            match unsafe { libloading::Library::new(candidate) } {
                Ok(lib) => {
                    loaded_lib = Some(lib);
                    break;
                }
                Err(err) => {
                    last_err = format!("{candidate}: {err}");
                }
            }
        }

        let lib = loaded_lib.ok_or_else(|| format!("Failed to load libmecab: {last_err}"))?;

        let (mecab_new2, mecab_sparse_tostr2, mecab_strerror, mecab_destroy) = unsafe {
            let new2: libloading::Symbol<MecabNew2Fn> = lib
                .get(b"mecab_new2\0")
                .map_err(|e| format!("missing mecab_new2: {e}"))?;
            let sparse_tostr2: libloading::Symbol<MecabSparseToStr2Fn> = lib
                .get(b"mecab_sparse_tostr2\0")
                .map_err(|e| format!("missing mecab_sparse_tostr2: {e}"))?;
            let strerror: libloading::Symbol<MecabStrerrorFn> = lib
                .get(b"mecab_strerror\0")
                .map_err(|e| format!("missing mecab_strerror: {e}"))?;
            let destroy: libloading::Symbol<MecabDestroyFn> = lib
                .get(b"mecab_destroy\0")
                .map_err(|e| format!("missing mecab_destroy: {e}"))?;
            (*new2, *sparse_tostr2, *strerror, *destroy)
        };

        let mut c_arg: Vec<u8> = tagger_arg.iter().copied().filter(|&b| b != 0).collect();
        c_arg.push(0);

        let tagger = unsafe { (mecab_new2)(c_arg.as_ptr() as *const c_char) };
        if tagger.is_null() {
            let err_msg = unsafe {
                let err_ptr = (mecab_strerror)(std::ptr::null_mut());
                if err_ptr.is_null() {
                    "unknown mecab_new2 error".to_string()
                } else {
                    CStr::from_ptr(err_ptr).to_string_lossy().into_owned()
                }
            };
            return Err(format!("mecab_new2 returned null: {err_msg}"));
        }

        Ok(Self {
            tagger,
            mecab_sparse_tostr2,
            mecab_strerror,
            mecab_destroy,
            _lib: lib,
        })
    }

    /// Parse `text` into MeCab lattice output lines (ending with `EOS\n`).
    /// Processes each `\n`-delimited line individually.
    pub fn parse(&mut self, text: &str) -> Result<String, String> {
        if text.is_empty() {
            return Ok(String::new());
        }
        if self.tagger.is_null() {
            return Err("MeCab tagger pointer is null".into());
        }

        let mut combined = String::with_capacity(text.len() * 8);
        let mut buf: Vec<u8> = Vec::with_capacity(text.len() + 1);
        for raw_line in text.split('\n') {
            let line = raw_line.trim_end_matches('\r');
            if line.is_empty() {
                continue;
            }
            buf.clear();
            if line.as_bytes().contains(&0) {
                buf.extend(line.bytes().filter(|&b| b != 0));
            } else {
                buf.extend_from_slice(line.as_bytes());
            }
            let len = buf.len();
            if len == 0 {
                continue;
            }
            buf.push(0);

            let out_ptr = unsafe {
                (self.mecab_sparse_tostr2)(self.tagger, buf.as_ptr() as *const c_char, len)
            };
            if out_ptr.is_null() {
                let err_msg = unsafe {
                    let err_ptr = (self.mecab_strerror)(self.tagger);
                    if err_ptr.is_null() {
                        "unknown mecab_sparse_tostr2 error".to_string()
                    } else {
                        CStr::from_ptr(err_ptr).to_string_lossy().into_owned()
                    }
                };
                return Err(format!("mecab_sparse_tostr2 failed: {err_msg}"));
            }

            let piece = unsafe { CStr::from_ptr(out_ptr).to_string_lossy() };
            combined.push_str(&piece);
            if !combined.ends_with('\n') {
                combined.push('\n');
            }
        }

        Ok(combined)
    }
}

impl Drop for LibMecabTagger {
    fn drop(&mut self) {
        if !self.tagger.is_null() {
            unsafe {
                (self.mecab_destroy)(self.tagger);
            }
            self.tagger = std::ptr::null_mut();
        }
    }
}

/// Persistent warm piped `mecab` subprocess fallback when `libmecab` cannot be loaded.
pub struct PipedMecabDaemon {
    child: Child,
    stdin: BufWriter<ChildStdin>,
    stdout: BufReader<ChildStdout>,
    args: Vec<String>,
}

impl PipedMecabDaemon {
    /// Spawn a persistent piped `mecab` daemon with default arguments.
    pub fn new() -> Result<Self, String> {
        Self::with_args(Vec::new())
    }

    /// Spawn a persistent piped `mecab` daemon with explicit CLI arguments.
    pub fn with_args(args: Vec<String>) -> Result<Self, String> {
        let (child, stdin, stdout) = Self::spawn_process(&args)?;
        Ok(Self {
            child,
            stdin,
            stdout,
            args,
        })
    }

    fn spawn_process(
        args: &[String],
    ) -> Result<(Child, BufWriter<ChildStdin>, BufReader<ChildStdout>), String> {
        let mut child = Command::new("mecab")
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| format!("MeCab is unavailable: {error}"))?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| "MeCab stdin pipe unavailable".to_string())?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "MeCab stdout pipe unavailable".to_string())?;

        Ok((child, BufWriter::new(stdin), BufReader::new(stdout)))
    }

    fn restart(&mut self) -> Result<(), String> {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let (child, stdin, stdout) = Self::spawn_process(&self.args)?;
        self.child = child;
        self.stdin = stdin;
        self.stdout = stdout;
        Ok(())
    }

    fn parse_single_line(&mut self, line: &str) -> Result<String, String> {
        let clean: String = line
            .chars()
            .filter(|&c| c != '\r' && c != '\n' && c != '\0')
            .collect();
        if clean.is_empty() {
            return Ok(String::new());
        }

        self.stdin
            .write_all(clean.as_bytes())
            .and_then(|()| self.stdin.write_all(b"\n"))
            .and_then(|()| self.stdin.flush())
            .map_err(|e| format!("MeCab stdin write failed: {e}"))?;

        let mut output = String::new();
        let mut row = String::new();
        loop {
            row.clear();
            let bytes_read = self
                .stdout
                .read_line(&mut row)
                .map_err(|e| format!("MeCab stdout read failed: {e}"))?;
            if bytes_read == 0 {
                return Err("MeCab process closed stdout unexpectedly (EOF)".into());
            }
            output.push_str(&row);
            if row.trim_end_matches(&['\r', '\n'][..]) == "EOS" {
                break;
            }
        }
        Ok(output)
    }

    /// Parse `text` line-by-line so embedded `\n` never desynchronizes `EOS` framing,
    /// automatically restarting the child process once if it died.
    pub fn parse(&mut self, text: &str) -> Result<String, String> {
        if text.is_empty() {
            return Ok(String::new());
        }

        let mut combined = String::new();
        for raw_line in text.split('\n') {
            let line = raw_line.trim_end_matches('\r');
            if line.is_empty() {
                continue;
            }
            match self.parse_single_line(line) {
                Ok(piece) => combined.push_str(&piece),
                Err(_) => {
                    self.restart()?;
                    let piece = self.parse_single_line(line)?;
                    combined.push_str(&piece);
                }
            }
        }
        Ok(combined)
    }
}

impl Drop for PipedMecabDaemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

enum MecabBackend {
    InProcess(LibMecabTagger),
    Piped(PipedMecabDaemon),
}

impl MecabBackend {
    fn init() -> Result<Self, String> {
        match LibMecabTagger::new() {
            Ok(tagger) => Ok(Self::InProcess(tagger)),
            Err(_) => PipedMecabDaemon::new().map(Self::Piped),
        }
    }

    fn parse(&mut self, text: &str) -> Result<String, String> {
        match self {
            Self::InProcess(tagger) => tagger.parse(text),
            Self::Piped(daemon) => daemon.parse(text),
        }
    }
}

static GLOBAL_MECAB: OnceLock<Mutex<Option<MecabBackend>>> = OnceLock::new();

/// Parse `text` using the process-wide warm MeCab backend (preferring in-process
/// `libmecab` via `libloading`, falling back to a persistent piped `mecab` daemon).
pub fn parse_mecab(text: &str) -> Result<String, String> {
    if text.is_empty() {
        return Ok(String::new());
    }
    let mutex = GLOBAL_MECAB.get_or_init(|| Mutex::new(MecabBackend::init().ok()));
    let mut guard = mutex
        .lock()
        .map_err(|_| "MeCab mutex poisoned".to_string())?;
    if guard.is_none() {
        *guard = Some(MecabBackend::init()?);
    }
    guard.as_mut().unwrap().parse(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    #[test]
    fn in_process_libmecab_parses_and_drops_cleanly_under_100us() {
        let mut tagger = LibMecabTagger::new().expect("libmecab.so.2 should load on this system");
        // Warm up mmap pages once
        let cues = ["来ない", "見ない", "行った", "王さま", "氷", "お兄さん", "人々", "食べる"];
        for cue in &cues {
            let first = tagger.parse(cue).expect("warm up parse");
            assert!(first.contains("EOS"));
        }

        // Measure warm latency across multiple cues
        let iterations = 10;
        let start = Instant::now();
        for _ in 0..iterations {
            for cue in &cues {
                let out = tagger.parse(cue).expect("warm parse");
                assert!(out.contains("EOS"));
            }
        }
        let avg_micros =
            start.elapsed().as_micros() as f64 / (cues.len() * iterations) as f64;
        assert!(
            avg_micros < 100.0,
            "warm in-process libmecab should take <100us (0.1ms) per cue, took {avg_micros:.2}us"
        );
    }

    #[test]
    fn piped_mecab_daemon_handles_multiline_and_auto_restarts() {
        let mut daemon = PipedMecabDaemon::new().expect("mecab binary should spawn");
        let out = daemon.parse("昨日\n行った").expect("multiline parse");
        assert!(out.contains("昨日\t"));
        assert!(out.contains("行っ\t"));

        // Simulate child crash and verify transparent auto-restart
        let _ = daemon.child.kill();
        let _ = daemon.child.wait();
        let recovered = daemon.parse("食べる").expect("daemon should auto-restart");
        assert!(recovered.contains("食べる\t"));
    }
}

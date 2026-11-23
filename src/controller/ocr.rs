//! Pure-Rust Manga OCR controller backed by an isolated, persistent Python
//! worker process (no `libmocr` C++ library, no `libloading`, zero `unsafe`).
//!
//! Spawning, model loading, and inference happen lazily on a dedicated worker
//! thread started on the first OCR trigger. A single-slot latest-wins queue
//! drops stale frames so 4K RGBA captures never accumulate. After an idle
//! timeout (or on settings change / app exit), the Python child process exits
//! cleanly so 100% of PyTorch RAM and GPU VRAM is reclaimed by the OS.

use std::ffi::OsStr;
use std::fmt;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use subomi_util::ocrutils::{map_ocr_region, resolve_venv_python_str, OcrRegionStatus};

const DEFAULT_OCR_IDLE_UNLOAD_TIMEOUT: Duration = Duration::from_secs(300);
const DAEMON_SHUTDOWN_GRACE: Duration = Duration::from_millis(1500);
const MAX_STDERR_BYTES: usize = 8 * 1024;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Embedded Python worker for `manga-ocr`.
///
/// Protocol:
/// - Duplicates fd 1 for clean binary JSON-line responses and redirects fd 1 to
///   fd 2 before importing PyTorch / `transformers` / `manga_ocr` so library
///   logs, `tqdm` bars, or C `printf` output can never corrupt our protocol.
/// - Emits a single handshake line on startup:
///     `{"status":"ready","device":"gpu"|"cpu"}`
///   or `{"status":"error","kind":"missing_pkg"|"model_init","error":"..."}`.
/// - Reads requests from stdin:
///     `{"op":"read","width":W,"height":H,"len":N}\n` followed by `N` RGBA bytes
///   or `{"op":"quit"}\n`.
/// - Responds with `{"ok":true,"text":"..."}\n` or `{"ok":false,"error":"..."}\n`.
const MANGA_OCR_WORKER_SCRIPT: &str = r#"import json, os, sys

_proto_fd = os.dup(1)
os.dup2(2, 1)
_proto_out = os.fdopen(_proto_fd, "wb", buffering=0)
sys.stdout = sys.stderr

def _send(obj):
    _proto_out.write((json.dumps(obj, ensure_ascii=False) + "\n").encode("utf-8"))
    _proto_out.flush()

def _read_exact(stream, n):
    buf = bytearray()
    while len(buf) < n:
        chunk = stream.read(n - len(buf))
        if not chunk:
            return None
        buf.extend(chunk)
    return bytes(buf)

def _main():
    model_name = sys.argv[1]
    want_gpu = sys.argv[2] == "1"
    try:
        from PIL import Image
        from manga_ocr import MangaOcr
    except Exception as exc:
        _send({"status": "error", "kind": "missing_pkg", "error": f"{type(exc).__name__}: {exc}"})
        return 2

    mocr = None
    device = "cpu"
    if want_gpu:
        try:
            mocr = MangaOcr(pretrained_model_name_or_path=model_name, force_cpu=False)
            device = "gpu"
        except Exception as gpu_exc:
            sys.stderr.write(f"GPU initialization failed ({gpu_exc}); falling back to CPU.\n")
            sys.stderr.flush()
    if mocr is None:
        try:
            mocr = MangaOcr(pretrained_model_name_or_path=model_name, force_cpu=True)
            device = "cpu"
        except Exception as exc:
            _send({"status": "error", "kind": "model_init", "error": f"{type(exc).__name__}: {exc}"})
            return 3

    _send({"status": "ready", "device": device})
    stdin = sys.stdin.buffer
    while True:
        header = stdin.readline()
        if not header:
            break
        try:
            req = json.loads(header.decode("utf-8"))
        except Exception as exc:
            _send({"ok": False, "error": f"invalid request header: {exc}"})
            continue
        op = req.get("op")
        if op == "quit":
            break
        if op != "read":
            _send({"ok": False, "error": f"unknown op: {op}"})
            continue
        w = int(req.get("width", 0))
        h = int(req.get("height", 0))
        n = int(req.get("len", 0))
        if w <= 0 or h <= 0 or n != w * h * 4:
            _send({"ok": False, "error": "invalid RGBA dimensions"})
            continue
        raw = _read_exact(stdin, n)
        if raw is None:
            break
        try:
            img = Image.frombytes("RGBA", (w, h), raw).convert("RGB")
            text = mocr(img)
            _send({"ok": True, "text": "" if text is None else str(text)})
        except Exception as exc:
            _send({"ok": False, "error": f"{type(exc).__name__}: {exc}"})
    return 0

if __name__ == "__main__":
    raise SystemExit(_main())
"#;

/// A candidate Python invocation (`program` plus optional leading arguments,
/// e.g. `py -3` on Windows).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PythonCandidate {
    pub program: PathBuf,
    pub prefix_args: Vec<String>,
}

impl PythonCandidate {
    fn new(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            prefix_args: Vec::new(),
        }
    }

    fn with_args(program: impl Into<PathBuf>, args: &[&str]) -> Self {
        Self {
            program: program.into(),
            prefix_args: args.iter().map(|&arg| arg.to_owned()).collect(),
        }
    }

    fn display(&self) -> String {
        if self.prefix_args.is_empty() {
            self.program.display().to_string()
        } else {
            format!("{} {}", self.program.display(), self.prefix_args.join(" "))
        }
    }
}

/// Build an ordered, deduplicated list of Python interpreters to probe for
/// `manga_ocr`:
/// 1. `SUBOMI_OCR_PYTHON` / `PYTHONEXECUTABLE` explicit overrides (when present)
/// 2. Active `VIRTUAL_ENV` and `CONDA_PREFIX` virtual environments
/// 3. Subomi-local virtual environments (`<data_dir>/ocr-venv`, `<config_dir>/ocr-venv`)
/// 4. System Python launchers on `PATH` (`python3`, `python`; on Windows `python`, `py -3`, `python3`).
pub fn python_candidates_from_env(
    subomi_ocr_python: Option<&OsStr>,
    python_executable: Option<&OsStr>,
    virtual_env: Option<&OsStr>,
    conda_prefix: Option<&OsStr>,
    venv_dirs: &[PathBuf],
    is_windows: bool,
    file_exists: impl Fn(&Path) -> bool,
) -> Vec<PythonCandidate> {
    let mut out = Vec::new();
    let mut push_unique = |candidate: PythonCandidate| {
        if !out.contains(&candidate) {
            out.push(candidate);
        }
    };

    for explicit in [subomi_ocr_python, python_executable].into_iter().flatten() {
        let path = PathBuf::from(explicit);
        if !path.as_os_str().is_empty() && file_exists(&path) {
            push_unique(PythonCandidate::new(path));
        }
    }

    for env_venv in [virtual_env, conda_prefix].into_iter().flatten() {
        if let Some(venv_str) = env_venv.to_str() {
            if let Some(py) = resolve_venv_python_str(false, venv_str, is_windows) {
                let path = PathBuf::from(py);
                if file_exists(&path) {
                    push_unique(PythonCandidate::new(path));
                }
            }
        }
    }

    for dir in venv_dirs {
        if let Some(venv_str) = dir.to_str() {
            if let Some(py) = resolve_venv_python_str(false, venv_str, is_windows) {
                let path = PathBuf::from(py);
                if file_exists(&path) {
                    push_unique(PythonCandidate::new(path));
                }
            }
        }
    }

    if is_windows {
        push_unique(PythonCandidate::new("python"));
        push_unique(PythonCandidate::with_args("py", &["-3"]));
        push_unique(PythonCandidate::new("python3"));
    } else {
        push_unique(PythonCandidate::new("python3"));
        push_unique(PythonCandidate::new("python"));
    }

    out
}

fn default_subomi_venv_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(data_dir) = crate::app::paths::subomi_xdg_dir("XDG_DATA_HOME", ".local/share") {
        dirs.push(data_dir.join("ocr-venv"));
    }
    if let Some(config_dir) = crate::app::paths::subomi_xdg_dir("XDG_CONFIG_HOME", ".config") {
        dirs.push(config_dir.join("ocr-venv"));
    }
    dirs
}

fn discover_python_candidates() -> Vec<PythonCandidate> {
    let subomi_ocr_python = std::env::var_os("SUBOMI_OCR_PYTHON");
    let python_executable = std::env::var_os("PYTHONEXECUTABLE");
    let virtual_env = std::env::var_os("VIRTUAL_ENV");
    let conda_prefix = std::env::var_os("CONDA_PREFIX");
    let venv_dirs = default_subomi_venv_dirs();
    python_candidates_from_env(
        subomi_ocr_python.as_deref(),
        python_executable.as_deref(),
        virtual_env.as_deref(),
        conda_prefix.as_deref(),
        &venv_dirs,
        cfg!(windows),
        |path| path.is_file(),
    )
}

/// Settings needed for one OCR operation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OcrSettings {
    /// Whether OCR is enabled in application settings.
    pub enabled: bool,
    /// A local model path or Hugging Face repo supported by `manga-ocr`.
    pub model: String,
    /// Whether `manga-ocr` may use GPU acceleration (CUDA/MPS) when available.
    pub use_gpu: bool,
}

impl Default for OcrSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            model: "kha-white/manga-ocr-base".to_owned(),
            use_gpu: true,
        }
    }
}

/// OCR selection in normalized video viewport coordinates.
///
/// Coordinates and size are fractions of the viewport, not screenshot pixels.
/// Negative width or height is accepted to support drags in either direction.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OcrRegion {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// Dimensions of the Slint video viewport that received the drag.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OcrViewport {
    pub width: f64,
    pub height: f64,
}

/// Owned source-resolution raw RGBA screenshot pixels, without viewport
/// scaling or UI-composited overlays.
///
/// `stride` is the byte count between row starts. Pixels may include row
/// padding; the OCR crop is copied into a tightly packed buffer before sending
/// to the Manga OCR worker.
#[derive(Clone, Debug)]
pub struct OcrFrame {
    width: u32,
    height: u32,
    stride: usize,
    pixels: Vec<u8>,
}

impl OcrFrame {
    /// Creates a frame from RGBA pixels with an explicit row stride.
    pub fn new(width: u32, height: u32, stride: usize, pixels: Vec<u8>) -> Result<Self, OcrError> {
        if width == 0 || height == 0 || width > i32::MAX as u32 || height > i32::MAX as u32 {
            return Err(OcrError::InvalidFrameDimensions);
        }
        let row_bytes = usize::try_from(width)
            .ok()
            .and_then(|value| value.checked_mul(4))
            .ok_or(OcrError::InvalidFrameDimensions)?;
        if stride < row_bytes {
            return Err(OcrError::InvalidFrameStride);
        }
        let required_bytes = usize::try_from(height - 1)
            .ok()
            .and_then(|rows| rows.checked_mul(stride))
            .and_then(|offset| offset.checked_add(row_bytes))
            .ok_or(OcrError::InvalidFrameDimensions)?;
        if pixels.len() < required_bytes {
            return Err(OcrError::InvalidFrameBuffer);
        }
        Ok(Self {
            width,
            height,
            stride,
            pixels,
        })
    }

    /// Creates a tightly packed frame from the usual RGBA byte layout.
    pub fn from_rgba(width: u32, height: u32, pixels: Vec<u8>) -> Result<Self, OcrError> {
        let stride = usize::try_from(width)
            .ok()
            .and_then(|value| value.checked_mul(4))
            .ok_or(OcrError::InvalidFrameDimensions)?;
        Self::new(width, height, stride, pixels)
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }
}

/// Failure reported by frame validation, region mapping, or the Manga OCR backend.
#[derive(Debug, PartialEq, Eq)]
pub enum OcrError {
    Disabled,
    InvalidViewport,
    InvalidSelection,
    InvalidFrameDimensions,
    InvalidFrameStride,
    InvalidFrameBuffer,
    SelectionOutsideVideo,
    EmptyRegion,
    InvalidModelName,
    ModelInitializationFailed(String),
    InferenceFailed(String),
    NoTextDetected,
    WorkerStartFailed(String),
    WorkerUnavailable,
    RuntimeUnavailable(String),
}

impl fmt::Display for OcrError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Disabled => formatter.write_str("OCR is disabled."),
            Self::InvalidViewport => formatter.write_str("The video viewport is invalid."),
            Self::InvalidSelection => formatter.write_str("No valid region was selected."),
            Self::InvalidFrameDimensions => {
                formatter.write_str("The screenshot dimensions are invalid.")
            }
            Self::InvalidFrameStride => {
                formatter.write_str("The screenshot row stride is invalid.")
            }
            Self::InvalidFrameBuffer => {
                formatter.write_str("The screenshot pixel buffer is incomplete.")
            }
            Self::SelectionOutsideVideo => {
                formatter.write_str("The selected region does not include video.")
            }
            Self::EmptyRegion => formatter.write_str("The selected region is empty."),
            Self::InvalidModelName => {
                formatter.write_str("The configured OCR model name is invalid.")
            }
            Self::ModelInitializationFailed(detail) => {
                write!(formatter, "Could not initialize OCR model: {detail}")
            }
            Self::InferenceFailed(detail) => {
                write!(formatter, "OCR could not read the selected image: {detail}")
            }
            Self::NoTextDetected => formatter.write_str("No text was detected."),
            Self::RuntimeUnavailable(error) => {
                write!(formatter, "Manga OCR runtime unavailable: {error}")
            }
            Self::WorkerStartFailed(error) => {
                write!(formatter, "Could not start OCR worker: {error}")
            }
            Self::WorkerUnavailable => formatter.write_str("The OCR worker is unavailable."),
        }
    }
}

impl std::error::Error for OcrError {}

/// Safe abstraction for a loaded Manga OCR session (real Python daemon or mock
/// backend in unit tests).
trait OcrSession: Send {
    fn read_rgba(&mut self, pixels: &[u8], width: usize, height: usize) -> Result<String, OcrError>;
}

/// Factory that spawns an [`OcrSession`] for a given `(model, use_gpu)` pair.
trait OcrBackend: Send {
    fn spawn_session(&mut self, model: &str, use_gpu: bool)
        -> Result<Box<dyn OcrSession>, OcrError>;
}

/// Production backend backed by an isolated Python `manga-ocr` child process.
struct PythonMangaOcrBackend {
    script: &'static str,
}

impl Default for PythonMangaOcrBackend {
    fn default() -> Self {
        Self {
            script: MANGA_OCR_WORKER_SCRIPT,
        }
    }
}

impl OcrBackend for PythonMangaOcrBackend {
    fn spawn_session(
        &mut self,
        model: &str,
        use_gpu: bool,
    ) -> Result<Box<dyn OcrSession>, OcrError> {
        let candidates = discover_python_candidates();
        spawn_python_session_from_candidates(&candidates, self.script, model, use_gpu)
            .map(|daemon| Box::new(daemon) as Box<dyn OcrSession>)
    }
}

enum HandshakeOutcome {
    Ready,
    MissingPackage(String),
    ModelInitFailed(String),
    StartupFailed(String),
}

fn spawn_python_session_from_candidates(
    candidates: &[PythonCandidate],
    script: &str,
    model: &str,
    use_gpu: bool,
) -> Result<PythonMangaOcrDaemon, OcrError> {
    let mut spawn_errors = Vec::new();
    let mut missing_pkg_errors = Vec::new();

    for candidate in candidates {
        match PythonMangaOcrDaemon::spawn(candidate, script, model, use_gpu) {
            Ok((daemon, HandshakeOutcome::Ready)) => return Ok(daemon),
            Ok((mut daemon, HandshakeOutcome::MissingPackage(detail))) => {
                daemon.shutdown();
                missing_pkg_errors.push(format!("{}: {detail}", candidate.display()));
            }
            Ok((mut daemon, HandshakeOutcome::ModelInitFailed(detail))) => {
                daemon.shutdown();
                return Err(OcrError::ModelInitializationFailed(format!(
                    "`{model}` ({detail})"
                )));
            }
            Ok((mut daemon, HandshakeOutcome::StartupFailed(detail))) => {
                daemon.shutdown();
                spawn_errors.push(format!("{}: {detail}", candidate.display()));
            }
            Err(error) => {
                spawn_errors.push(format!("{}: {error}", candidate.display()));
            }
        }
    }

    if !missing_pkg_errors.is_empty() {
        return Err(OcrError::RuntimeUnavailable(format!(
            "Python was found, but `manga-ocr` (or `Pillow`) is not installed. Run `pip install manga-ocr` or set SUBOMI_OCR_PYTHON to your venv's python. Details: {}",
            missing_pkg_errors.join("; ")
        )));
    }

    Err(OcrError::RuntimeUnavailable(format!(
        "Could not start a Python 3 runtime for manga-ocr. Install Python 3 + `pip install manga-ocr` or set SUBOMI_OCR_PYTHON. Details: {}",
        if spawn_errors.is_empty() {
            "no Python candidates configured".to_owned()
        } else {
            spawn_errors.join("; ")
        }
    )))
}

struct PythonMangaOcrDaemon {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
    stderr_buf: Arc<Mutex<String>>,
    stderr_thread: Option<JoinHandle<()>>,
    exited: bool,
}

impl PythonMangaOcrDaemon {
    fn spawn(
        candidate: &PythonCandidate,
        script: &str,
        model: &str,
        use_gpu: bool,
    ) -> Result<(Self, HandshakeOutcome), String> {
        let mut command = Command::new(&candidate.program);
        for arg in &candidate.prefix_args {
            command.arg(arg);
        }
        command
            .arg("-u")
            .arg("-c")
            .arg(script)
            .arg(model)
            .arg(if use_gpu { "1" } else { "0" })
            .env("PYTHONUTF8", "1")
            .env("PYTHONIOENCODING", "utf-8")
            .env("TOKENIZERS_PARALLELISM", "false")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(CREATE_NO_WINDOW);
        }

        let mut child = command.spawn().map_err(|err| err.to_string())?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| "could not capture child stdin".to_owned())?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "could not capture child stdout".to_owned())?;
        let stderr = child.stderr.take();

        let stderr_buf = Arc::new(Mutex::new(String::new()));
        let stderr_thread = stderr.map(|mut pipe| {
            let sink = Arc::clone(&stderr_buf);
            thread::Builder::new()
                .name("subomi-ocr-stderr".to_owned())
                .spawn(move || {
                    let mut chunk = [0u8; 1024];
                    loop {
                        match pipe.read(&mut chunk) {
                            Ok(0) | Err(_) => break,
                            Ok(n) => {
                                let text = String::from_utf8_lossy(&chunk[..n]);
                                let mut buf =
                                    sink.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                                buf.push_str(&text);
                                if buf.len() > MAX_STDERR_BYTES {
                                    let keep_from = buf.len() - MAX_STDERR_BYTES;
                                    let boundary = buf
                                        .char_indices()
                                        .find(|(idx, _)| *idx >= keep_from)
                                        .map(|(idx, _)| idx)
                                        .unwrap_or(buf.len());
                                    buf.drain(..boundary);
                                }
                            }
                        }
                    }
                })
                .ok()
        });

        let mut daemon = Self {
            child,
            stdin: Some(stdin),
            stdout: BufReader::new(stdout),
            stderr_buf,
            stderr_thread: stderr_thread.flatten(),
            exited: false,
        };

        let outcome = daemon.read_handshake();
        Ok((daemon, outcome))
    }

    fn stderr_tail(&self) -> String {
        let buf = self
            .stderr_buf
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        buf.trim().to_owned()
    }

    fn read_handshake(&mut self) -> HandshakeOutcome {
        let mut line = String::new();
        match self.stdout.read_line(&mut line) {
            Ok(0) => {
                self.reap_after_eof();
                let stderr = self.stderr_tail();
                HandshakeOutcome::StartupFailed(if stderr.is_empty() {
                    "Python worker exited before signaling ready".to_owned()
                } else {
                    stderr
                })
            }
            Err(err) => HandshakeOutcome::StartupFailed(format!("protocol read error: {err}")),
            Ok(_) => {
                let parsed: serde_json::Value = match serde_json::from_str(line.trim()) {
                    Ok(value) => value,
                    Err(err) => {
                        return HandshakeOutcome::StartupFailed(format!(
                            "malformed startup reply ({err}): {}",
                            line.trim()
                        ));
                    }
                };
                match parsed.get("status").and_then(serde_json::Value::as_str) {
                    Some("ready") => HandshakeOutcome::Ready,
                    Some("error") => {
                        let detail = parsed
                            .get("error")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("unknown error")
                            .to_owned();
                        match parsed.get("kind").and_then(serde_json::Value::as_str) {
                            Some("missing_pkg") => HandshakeOutcome::MissingPackage(detail),
                            Some("model_init") => HandshakeOutcome::ModelInitFailed(detail),
                            _ => HandshakeOutcome::StartupFailed(detail),
                        }
                    }
                    _ => HandshakeOutcome::StartupFailed(format!(
                        "unexpected startup reply: {}",
                        line.trim()
                    )),
                }
            }
        }
    }

    fn reap_after_eof(&mut self) {
        if !self.exited {
            let _ = self.child.wait();
            self.exited = true;
        }
        if let Some(handle) = self.stderr_thread.take() {
            let _ = handle.join();
        }
    }

    fn shutdown(&mut self) {
        if let Some(mut stdin) = self.stdin.take() {
            let _ = stdin.write_all(b"{\"op\":\"quit\"}\n");
            let _ = stdin.flush();
            drop(stdin);
        }
        if !self.exited {
            let deadline = Instant::now() + DAEMON_SHUTDOWN_GRACE;
            loop {
                match self.child.try_wait() {
                    Ok(Some(_)) => {
                        self.exited = true;
                        break;
                    }
                    Ok(None) if Instant::now() < deadline => {
                        thread::sleep(Duration::from_millis(10));
                    }
                    _ => {
                        let _ = self.child.kill();
                        let _ = self.child.wait();
                        self.exited = true;
                        break;
                    }
                }
            }
        }
        if let Some(handle) = self.stderr_thread.take() {
            let _ = handle.join();
        }
    }
}

impl OcrSession for PythonMangaOcrDaemon {
    fn read_rgba(&mut self, pixels: &[u8], width: usize, height: usize) -> Result<String, OcrError> {
        let expected_bytes = width
            .checked_mul(height)
            .and_then(|px| px.checked_mul(4))
            .ok_or(OcrError::InvalidFrameDimensions)?;
        if width == 0 || height == 0 || pixels.len() != expected_bytes {
            return Err(OcrError::InvalidFrameBuffer);
        }

        let stdin = self.stdin.as_mut().ok_or(OcrError::WorkerUnavailable)?;
        let header = format!(
            "{{\"op\":\"read\",\"width\":{width},\"height\":{height},\"len\":{expected_bytes}}}\n"
        );
        if let Err(err) = stdin
            .write_all(header.as_bytes())
            .and_then(|()| stdin.write_all(pixels))
            .and_then(|()| stdin.flush())
        {
            self.shutdown();
            return Err(OcrError::WorkerUnavailableWithIo(err));
        }

        let mut line = String::new();
        match self.stdout.read_line(&mut line) {
            Ok(0) => {
                self.reap_after_eof();
                Err(OcrError::WorkerUnavailable)
            }
            Err(_) => {
                self.shutdown();
                Err(OcrError::WorkerUnavailable)
            }
            Ok(_) => {
                let parsed: serde_json::Value =
                    serde_json::from_str(line.trim()).map_err(|err| {
                        OcrError::InferenceFailed(format!("invalid worker reply ({err})"))
                    })?;
                if parsed
                    .get("ok")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false)
                {
                    let text = parsed
                        .get("text")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                        .to_owned();
                    Ok(text)
                } else {
                    let detail = parsed
                        .get("error")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("unknown inference error")
                        .to_owned();
                    Err(OcrError::InferenceFailed(detail))
                }
            }
        }
    }
}

impl OcrError {
    #[allow(non_snake_case)]
    fn WorkerUnavailableWithIo(_err: std::io::Error) -> Self {
        Self::WorkerUnavailable
    }
}

impl Drop for PythonMangaOcrDaemon {
    fn drop(&mut self) {
        self.shutdown();
    }
}

type OcrCallback = Box<dyn FnOnce(Result<String, OcrError>) + Send + 'static>;

struct OcrRequest {
    frame: OcrFrame,
    region: OcrRegion,
    viewport: OcrViewport,
    settings: OcrSettings,
    callback: OcrCallback,
}

/// Bounded 1-slot latest-wins request queue so queued 4K RGBA frames cannot
/// accumulate unboundedly while an OCR pass is running.
struct RequestSlot {
    pending: Option<OcrRequest>,
    closed: bool,
}

impl RequestSlot {
    fn new() -> Self {
        Self {
            pending: None,
            closed: false,
        }
    }

    fn push_latest(&mut self, request: OcrRequest) -> Result<Option<OcrRequest>, OcrError> {
        if self.closed {
            return Err(OcrError::WorkerUnavailable);
        }
        Ok(self.pending.replace(request))
    }

    fn close(&mut self) -> Option<OcrRequest> {
        self.closed = true;
        self.pending.take()
    }
}

struct OcrControllerInner {
    queue: Arc<(Mutex<RequestSlot>, Condvar)>,
    worker: Mutex<Option<JoinHandle<()>>>,
    idle_unload_timeout: Duration,
}

impl Drop for OcrControllerInner {
    fn drop(&mut self) {
        let displaced = {
            let (lock, cvar) = &*self.queue;
            let mut slot = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            let pending = slot.close();
            cvar.notify_all();
            pending
        };
        drop(displaced);

        let handle = self
            .worker
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        if let Some(handle) = handle {
            if handle.thread().id() != thread::current().id() {
                let _ = handle.join();
            }
        }
    }
}

/// Asynchronous OCR controller backed by one lazily initialized persistent
/// Manga OCR worker process.
///
/// Nothing is spawned in [`OcrController::new`]; both the coordinator thread
/// and the Python `manga-ocr` child process start lazily on the first OCR
/// request. After an idle timeout, the child process exits cleanly so PyTorch
/// RAM and GPU VRAM are returned directly to the OS.
#[derive(Clone)]
pub struct OcrController {
    inner: Arc<OcrControllerInner>,
}

impl OcrController {
    /// Creates a lazy OCR controller. The worker thread and Python daemon
    /// initialize on the first [`Self::read_region_async`] call.
    pub fn new() -> Result<Self, OcrError> {
        Ok(Self::from_idle_unload_timeout(
            DEFAULT_OCR_IDLE_UNLOAD_TIMEOUT,
        ))
    }

    fn from_idle_unload_timeout(idle_unload_timeout: Duration) -> Self {
        Self {
            inner: Arc::new(OcrControllerInner {
                queue: Arc::new((Mutex::new(RequestSlot::new()), Condvar::new())),
                worker: Mutex::new(None),
                idle_unload_timeout,
            }),
        }
    }

    #[cfg(test)]
    fn with_idle_unload_timeout(idle_timeout: Duration) -> Self {
        Self::from_idle_unload_timeout(idle_timeout)
    }

    #[cfg(test)]
    fn start_test_worker(
        &self,
        make_worker: impl FnOnce() -> OcrWorker + Send + 'static,
        deliver: impl Fn(OcrCallback, Result<String, OcrError>) + Send + 'static,
    ) {
        let mut handle_slot = self
            .inner
            .worker
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        assert!(handle_slot.is_none(), "test worker already started");
        let queue = Arc::clone(&self.inner.queue);
        let idle_timeout = self.inner.idle_unload_timeout;
        let handle = thread::Builder::new()
            .name("subomi-ocr-test".to_owned())
            .spawn(move || worker_loop_with(queue, idle_timeout, make_worker(), deliver))
            .expect("spawn test OCR worker");
        *handle_slot = Some(handle);
    }

    fn ensure_worker_started(&self) -> Result<(), OcrError> {
        let mut worker = self
            .inner
            .worker
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if worker.is_some() {
            return Ok(());
        }
        let queue = Arc::clone(&self.inner.queue);
        let idle_timeout = self.inner.idle_unload_timeout;
        let handle = thread::Builder::new()
            .name("subomi-ocr".to_owned())
            .spawn(move || worker_loop(queue, idle_timeout))
            .map_err(|error| OcrError::WorkerStartFailed(error.to_string()))?;
        *worker = Some(handle);
        Ok(())
    }

    /// Queues a screenshot region for OCR without blocking the UI thread.
    ///
    /// If a previous capture has not yet been picked up by the worker, it is
    /// superseded and dropped immediately (latest-wins). `frame` must contain
    /// the source-resolution, uncomposited RGBA video screenshot. `viewport` is
    /// the Slint viewport size in the same coordinate space as the normalized
    /// drag, not the screenshot dimensions. The callback is invoked on Slint's
    /// event loop with detected text or a typed error.
    pub fn read_region_async(
        &self,
        frame: OcrFrame,
        region: OcrRegion,
        viewport: OcrViewport,
        settings: OcrSettings,
        callback: impl FnOnce(Result<String, OcrError>) + Send + 'static,
    ) -> Result<(), OcrError> {
        self.ensure_worker_started()?;
        let request = OcrRequest {
            frame,
            region,
            viewport,
            settings,
            callback: Box::new(callback),
        };
        let displaced = {
            let (lock, cvar) = &*self.inner.queue;
            let mut slot = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            let displaced = slot.push_latest(request)?;
            cvar.notify_one();
            displaced
        };
        // Release any stale RGBA frame outside the mutex lock.
        drop(displaced);
        Ok(())
    }
}

/// Waits for the next latest-wins OCR request or queue shutdown.
///
/// When an OCR session is currently loaded (`model_loaded == true`), waits up
/// to `idle_timeout` before invoking `on_idle_unload` **without holding the
/// queue mutex** so new requests or `Drop` shutdown never stall on teardown.
/// After unloading, re-locks and parks indefinitely until a request or shutdown
/// arrives.
fn pop_latest_request_with_idle_unload(
    queue: &(Mutex<RequestSlot>, Condvar),
    model_loaded: bool,
    idle_timeout: Duration,
    mut on_idle_unload: impl FnMut(),
) -> Option<OcrRequest> {
    let (lock, cvar) = queue;
    let mut slot = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut idle_deadline = if model_loaded {
        Instant::now().checked_add(idle_timeout)
    } else {
        None
    };
    loop {
        if slot.closed {
            slot.pending = None;
            return None;
        }
        if let Some(request) = slot.pending.take() {
            return Some(request);
        }
        if let Some(deadline) = idle_deadline {
            let now = Instant::now();
            if now >= deadline {
                drop(slot);
                on_idle_unload();
                idle_deadline = None;
                slot = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                continue;
            }
            let remaining = deadline.saturating_duration_since(now);
            let (next, wait_result) = cvar
                .wait_timeout(slot, remaining)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            slot = next;
            if wait_result.timed_out() && slot.pending.is_none() && !slot.closed {
                drop(slot);
                on_idle_unload();
                idle_deadline = None;
                slot = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            }
        } else {
            slot = cvar
                .wait(slot)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
    }
}

fn deliver_to_slint(callback: OcrCallback, result: Result<String, OcrError>) {
    if let Err(error) = slint::invoke_from_event_loop(move || callback(result)) {
        eprintln!("Could not deliver OCR result to Slint event loop: {error}");
    }
}

fn worker_loop(queue: Arc<(Mutex<RequestSlot>, Condvar)>, idle_timeout: Duration) {
    worker_loop_with(
        queue,
        idle_timeout,
        OcrWorker::new(Box::new(PythonMangaOcrBackend::default())),
        deliver_to_slint,
    );
}

fn worker_loop_with(
    queue: Arc<(Mutex<RequestSlot>, Condvar)>,
    idle_timeout: Duration,
    mut worker: OcrWorker,
    deliver: impl Fn(OcrCallback, Result<String, OcrError>),
) {
    loop {
        let model_loaded = worker.is_model_loaded();
        let Some(request) =
            pop_latest_request_with_idle_unload(&queue, model_loaded, idle_timeout, || {
                worker.unload_model();
            })
        else {
            break;
        };
        let result = worker.read(
            request.frame,
            request.region,
            request.viewport,
            request.settings,
        );
        deliver(request.callback, result);
    }
    worker.unload_model();
}

fn normalize_model_name(raw: &str) -> Result<&str, OcrError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.as_bytes().contains(&0) {
        return Err(OcrError::InvalidModelName);
    }
    Ok(trimmed)
}

struct LoadedModel {
    name: String,
    use_gpu: bool,
    session: Box<dyn OcrSession>,
}

struct OcrWorker {
    model: Option<LoadedModel>,
    backend: Box<dyn OcrBackend>,
}

impl OcrWorker {
    fn new(backend: Box<dyn OcrBackend>) -> Self {
        Self {
            model: None,
            backend,
        }
    }

    fn is_model_loaded(&self) -> bool {
        self.model.is_some()
    }

    /// Terminates the isolated OCR session so the OS reclaims PyTorch RAM and
    /// GPU VRAM immediately.
    fn unload_model(&mut self) -> bool {
        if let Some(model) = self.model.take() {
            drop(model);
            crate::app::trim_os_heap();
            true
        } else {
            false
        }
    }

    fn read(
        &mut self,
        frame: OcrFrame,
        region: OcrRegion,
        viewport: OcrViewport,
        settings: OcrSettings,
    ) -> Result<String, OcrError> {
        if !settings.enabled {
            return Err(OcrError::Disabled);
        }
        let (pixels, width, height) = crop_frame(&frame, region, viewport)?;
        let model_name = normalize_model_name(&settings.model)?.to_owned();

        let was_reused = self
            .model
            .as_ref()
            .is_some_and(|m| m.name == model_name && m.use_gpu == settings.use_gpu);
        let first_attempt = {
            let model = self.model_for(&model_name, settings.use_gpu)?;
            model.session.read_rgba(&pixels, width, height)
        };

        let text = match first_attempt {
            Ok(text) => text,
            // If a previously warm daemon died between requests (e.g. OOM),
            // restart it cleanly once and retry.
            Err(OcrError::WorkerUnavailable) if was_reused => {
                self.model = None;
                let model = self.model_for(&model_name, settings.use_gpu)?;
                model.session.read_rgba(&pixels, width, height)?
            }
            Err(err) => {
                if matches!(err, OcrError::WorkerUnavailable) {
                    self.model = None;
                }
                return Err(err);
            }
        };

        let text = text.trim();
        if text.is_empty() {
            return Err(OcrError::NoTextDetected);
        }
        Ok(text.to_owned())
    }

    fn model_for(&mut self, model_name: &str, use_gpu: bool) -> Result<&mut LoadedModel, OcrError> {
        let model_name = normalize_model_name(model_name)?;
        let needs_recreate = self
            .model
            .as_ref()
            .is_none_or(|model| model.name != model_name || model.use_gpu != use_gpu);
        if needs_recreate {
            self.model = None;
            let session = self.backend.spawn_session(model_name, use_gpu)?;
            self.model = Some(LoadedModel {
                name: model_name.to_owned(),
                use_gpu,
                session,
            });
        }
        self.model
            .as_mut()
            .ok_or_else(|| OcrError::ModelInitializationFailed(model_name.to_owned()))
    }
}

fn crop_frame(
    frame: &OcrFrame,
    region: OcrRegion,
    viewport: OcrViewport,
) -> Result<(Vec<u8>, usize, usize), OcrError> {
    if !viewport.width.is_finite()
        || !viewport.height.is_finite()
        || viewport.width <= 0.0
        || viewport.height <= 0.0
    {
        return Err(OcrError::InvalidViewport);
    }
    if !region.x.is_finite()
        || !region.y.is_finite()
        || !region.width.is_finite()
        || !region.height.is_finite()
        || region.width == 0.0
        || region.height == 0.0
    {
        return Err(OcrError::InvalidSelection);
    }
    let selection_x = region.x * viewport.width;
    let selection_y = region.y * viewport.height;
    let selection_width = region.width * viewport.width;
    let selection_height = region.height * viewport.height;
    if !selection_x.is_finite()
        || !selection_y.is_finite()
        || !selection_width.is_finite()
        || !selection_height.is_finite()
    {
        return Err(OcrError::InvalidSelection);
    }

    let (status, (x, y, width, height)) = map_ocr_region(
        selection_x,
        selection_y,
        selection_width,
        selection_height,
        viewport.width,
        viewport.height,
        frame.width as i32,
        frame.height as i32,
    )
    .ok_or(OcrError::InvalidSelection)?;
    match status {
        OcrRegionStatus::Ok => {}
        OcrRegionStatus::NoVideoRegion => return Err(OcrError::SelectionOutsideVideo),
        OcrRegionStatus::EmptyRegion => return Err(OcrError::EmptyRegion),
    }
    if width <= 0 || height <= 0 {
        return Err(OcrError::EmptyRegion);
    }

    let x = usize::try_from(x).map_err(|_| OcrError::EmptyRegion)?;
    let y = usize::try_from(y).map_err(|_| OcrError::EmptyRegion)?;
    let width = usize::try_from(width).map_err(|_| OcrError::EmptyRegion)?;
    let height = usize::try_from(height).map_err(|_| OcrError::EmptyRegion)?;
    let row_bytes = width
        .checked_mul(4)
        .ok_or(OcrError::InvalidFrameDimensions)?;
    let output_bytes = row_bytes
        .checked_mul(height)
        .ok_or(OcrError::InvalidFrameDimensions)?;
    let mut crop = Vec::with_capacity(output_bytes);
    for row in y..y + height {
        let start = row
            .checked_mul(frame.stride)
            .and_then(|offset| offset.checked_add(x.checked_mul(4)?))
            .ok_or(OcrError::InvalidFrameBuffer)?;
        let end = start
            .checked_add(row_bytes)
            .ok_or(OcrError::InvalidFrameBuffer)?;
        crop.extend_from_slice(
            frame
                .pixels
                .get(start..end)
                .ok_or(OcrError::InvalidFrameBuffer)?,
        );
    }
    Ok((crop, width, height))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc;

    struct MockSession {
        drop_calls: Arc<AtomicUsize>,
        drop_tx: Option<mpsc::Sender<usize>>,
        fail_once: bool,
    }

    impl OcrSession for MockSession {
        fn read_rgba(
            &mut self,
            pixels: &[u8],
            _width: usize,
            _height: usize,
        ) -> Result<String, OcrError> {
            if self.fail_once {
                self.fail_once = false;
                return Err(OcrError::WorkerUnavailable);
            }
            Ok(format!("frame-{}", pixels[0]))
        }
    }

    impl Drop for MockSession {
        fn drop(&mut self) {
            let count = self.drop_calls.fetch_add(1, Ordering::SeqCst) + 1;
            if let Some(tx) = &self.drop_tx {
                let _ = tx.send(count);
            }
        }
    }

    struct MockBackend {
        spawn_calls: Arc<AtomicUsize>,
        drop_calls: Arc<AtomicUsize>,
        drop_tx: Option<mpsc::Sender<usize>>,
        fail_first_read_on_spawn: Option<usize>,
    }

    impl OcrBackend for MockBackend {
        fn spawn_session(
            &mut self,
            _model: &str,
            _use_gpu: bool,
        ) -> Result<Box<dyn OcrSession>, OcrError> {
            let n = self.spawn_calls.fetch_add(1, Ordering::SeqCst) + 1;
            let fail_once = self.fail_first_read_on_spawn == Some(n);
            Ok(Box::new(MockSession {
                drop_calls: Arc::clone(&self.drop_calls),
                drop_tx: self.drop_tx.clone(),
                fail_once,
            }))
        }
    }

    fn test_ocr_request_args(tag: u8) -> (OcrFrame, OcrRegion, OcrViewport, OcrSettings) {
        (
            OcrFrame::from_rgba(1, 1, vec![tag, 0, 0, 255]).unwrap(),
            OcrRegion {
                x: 0.0,
                y: 0.0,
                width: 1.0,
                height: 1.0,
            },
            OcrViewport {
                width: 1.0,
                height: 1.0,
            },
            OcrSettings {
                enabled: true,
                model: "mock/manga-ocr".to_owned(),
                use_gpu: false,
            },
        )
    }

    #[test]
    fn crop_maps_normalized_region_and_honors_row_stride() {
        let mut pixels = vec![0; 24];
        pixels[0..8].copy_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8]);
        pixels[12..20].copy_from_slice(&[9, 10, 11, 12, 13, 14, 15, 16]);
        let frame = OcrFrame::new(2, 2, 12, pixels).unwrap();
        let (crop, width, height) = crop_frame(
            &frame,
            OcrRegion {
                x: 0.5,
                y: 0.5,
                width: 0.5,
                height: 0.5,
            },
            OcrViewport {
                width: 2.0,
                height: 2.0,
            },
        )
        .unwrap();
        assert_eq!((width, height), (1, 1));
        assert_eq!(crop.as_slice(), &[13, 14, 15, 16]);
    }

    #[test]
    fn crop_rejects_letterbox_only_selection() {
        let frame = OcrFrame::from_rgba(16, 9, vec![0; 16 * 9 * 4]).unwrap();
        let error = crop_frame(
            &frame,
            OcrRegion {
                x: 0.0,
                y: 0.0,
                width: 1.0,
                height: 0.1,
            },
            OcrViewport {
                width: 16.0,
                height: 16.0,
            },
        )
        .unwrap_err();
        assert_eq!(error, OcrError::SelectionOutsideVideo);
    }

    #[test]
    fn ocr_controller_new_is_lazy_and_drops_without_spawning_worker() {
        let controller = OcrController::new().unwrap();
        assert!(controller.inner.worker.lock().unwrap().is_none());
        drop(controller);
    }

    #[test]
    fn request_slot_keeps_only_latest_frame_and_rejects_after_close() {
        let mut slot = RequestSlot::new();
        let make_req = |tag: u8| OcrRequest {
            frame: OcrFrame::from_rgba(1, 1, vec![tag, 0, 0, 255]).unwrap(),
            region: OcrRegion {
                x: 0.0,
                y: 0.0,
                width: 1.0,
                height: 1.0,
            },
            viewport: OcrViewport {
                width: 1.0,
                height: 1.0,
            },
            settings: OcrSettings::default(),
            callback: Box::new(|_| {}),
        };

        assert!(slot.push_latest(make_req(1)).unwrap().is_none());
        let displaced_1 = slot.push_latest(make_req(2)).unwrap().unwrap();
        assert_eq!(displaced_1.frame.pixels[0], 1);
        let displaced_2 = slot.push_latest(make_req(3)).unwrap().unwrap();
        assert_eq!(displaced_2.frame.pixels[0], 2);

        let latest = slot.pending.take().unwrap();
        assert_eq!(latest.frame.pixels[0], 3);

        assert!(slot.close().is_none());
        assert!(matches!(
            slot.push_latest(make_req(4)),
            Err(OcrError::WorkerUnavailable)
        ));
    }

    #[test]
    fn normalize_model_name_trims_and_rejects_blank_or_nul() {
        assert_eq!(
            normalize_model_name("  kha-white/manga-ocr-base \t\n").unwrap(),
            "kha-white/manga-ocr-base"
        );
        assert_eq!(
            normalize_model_name("   ").unwrap_err(),
            OcrError::InvalidModelName
        );
        assert_eq!(
            normalize_model_name("bad\0model").unwrap_err(),
            OcrError::InvalidModelName
        );
    }

    #[test]
    fn python_candidates_prioritize_overrides_venvs_local_venvs_and_system_python() {
        let local_venv = PathBuf::from("/home/u/.local/share/subomi/ocr-venv");
        let existing = [
            PathBuf::from("/opt/custom/python"),
            PathBuf::from("/tmp/venv/bin/python"),
            local_venv.join("bin/python"),
        ];
        let candidates = python_candidates_from_env(
            Some(OsStr::new("/opt/custom/python")),
            Some(OsStr::new("/missing/python")),
            Some(OsStr::new("/tmp/venv")),
            None,
            &[local_venv.clone()],
            false,
            |p| existing.iter().any(|e| e == p),
        );
        let names: Vec<String> = candidates.iter().map(PythonCandidate::display).collect();
        assert_eq!(
            names,
            vec![
                "/opt/custom/python",
                "/tmp/venv/bin/python",
                "/home/u/.local/share/subomi/ocr-venv/bin/python",
                "python3",
                "python",
            ]
        );

        let win_candidates = python_candidates_from_env(None, None, None, None, &[], true, |_| false);
        let win_names: Vec<String> = win_candidates.iter().map(PythonCandidate::display).collect();
        assert_eq!(win_names, vec!["python", "py -3", "python3"]);
    }

    #[test]
    fn ocr_worker_unload_model_is_idempotent_and_restarts_on_next_read() {
        let spawns = Arc::new(AtomicUsize::new(0));
        let drops = Arc::new(AtomicUsize::new(0));
        let mut worker = OcrWorker::new(Box::new(MockBackend {
            spawn_calls: Arc::clone(&spawns),
            drop_calls: Arc::clone(&drops),
            drop_tx: None,
            fail_first_read_on_spawn: None,
        }));

        assert!(!worker.is_model_loaded());
        assert!(!worker.unload_model());

        let (frame, region, viewport, settings) = test_ocr_request_args(7);
        assert_eq!(
            worker.read(frame, region, viewport, settings.clone()).unwrap(),
            "frame-7"
        );
        assert!(worker.is_model_loaded());
        assert_eq!(spawns.load(Ordering::SeqCst), 1);
        assert_eq!(drops.load(Ordering::SeqCst), 0);

        assert!(worker.unload_model());
        assert!(!worker.is_model_loaded());
        assert_eq!(drops.load(Ordering::SeqCst), 1);
        assert!(!worker.unload_model());

        let (frame2, region2, viewport2, settings2) = test_ocr_request_args(9);
        assert_eq!(
            worker.read(frame2, region2, viewport2, settings2).unwrap(),
            "frame-9"
        );
        assert_eq!(spawns.load(Ordering::SeqCst), 2);
        drop(worker);
        assert_eq!(drops.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn ocr_worker_recovers_once_when_warm_session_dies_between_requests() {
        let spawns = Arc::new(AtomicUsize::new(0));
        let drops = Arc::new(AtomicUsize::new(0));
        let mut worker = OcrWorker::new(Box::new(MockBackend {
            spawn_calls: Arc::clone(&spawns),
            drop_calls: Arc::clone(&drops),
            drop_tx: None,
            fail_first_read_on_spawn: None,
        }));

        let (f1, r1, v1, s1) = test_ocr_request_args(1);
        assert_eq!(worker.read(f1, r1, v1, s1.clone()).unwrap(), "frame-1");
        assert_eq!(spawns.load(Ordering::SeqCst), 1);

        // Simulate the warm session dying on its next read: replace with a failing session.
        worker.model.as_mut().unwrap().session = Box::new(MockSession {
            drop_calls: Arc::clone(&drops),
            drop_tx: None,
            fail_once: true,
        });

        let (f2, r2, v2, s2) = test_ocr_request_args(2);
        assert_eq!(worker.read(f2, r2, v2, s2).unwrap(), "frame-2");
        assert_eq!(spawns.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn ocr_idle_unload_reclaims_session_after_timeout_and_reloads_on_next_request() {
        let spawns = Arc::new(AtomicUsize::new(0));
        let drops = Arc::new(AtomicUsize::new(0));
        let (drop_tx, drop_rx) = mpsc::channel();

        let controller = OcrController::with_idle_unload_timeout(Duration::from_millis(30));
        let spawns_bg = Arc::clone(&spawns);
        let drops_bg = Arc::clone(&drops);
        controller.start_test_worker(
            move || {
                OcrWorker::new(Box::new(MockBackend {
                    spawn_calls: spawns_bg,
                    drop_calls: drops_bg,
                    drop_tx: Some(drop_tx),
                    fail_first_read_on_spawn: None,
                }))
            },
            |callback, result| callback(result),
        );

        let (res1_tx, res1_rx) = mpsc::channel();
        let (frame1, region1, viewport1, settings1) = test_ocr_request_args(1);
        controller
            .read_region_async(frame1, region1, viewport1, settings1, move |res| {
                let _ = res1_tx.send(res);
            })
            .unwrap();
        assert_eq!(
            res1_rx.recv_timeout(Duration::from_secs(2)).unwrap(),
            Ok("frame-1".to_owned())
        );
        assert_eq!(spawns.load(Ordering::SeqCst), 1);

        let first_drop = drop_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("session should unload after idle timeout");
        assert_eq!(first_drop, 1);

        let (res2_tx, res2_rx) = mpsc::channel();
        let (frame2, region2, viewport2, settings2) = test_ocr_request_args(2);
        controller
            .read_region_async(frame2, region2, viewport2, settings2, move |res| {
                let _ = res2_tx.send(res);
            })
            .unwrap();
        assert_eq!(
            res2_rx.recv_timeout(Duration::from_secs(2)).unwrap(),
            Ok("frame-2".to_owned())
        );
        assert_eq!(spawns.load(Ordering::SeqCst), 2);

        drop(controller);
        assert_eq!(drops.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn ocr_drop_shutdown_wakes_timed_idle_wait_immediately() {
        let spawns = Arc::new(AtomicUsize::new(0));
        let drops = Arc::new(AtomicUsize::new(0));

        let controller = OcrController::with_idle_unload_timeout(Duration::from_secs(60));
        let spawns_bg = Arc::clone(&spawns);
        let drops_bg = Arc::clone(&drops);
        controller.start_test_worker(
            move || {
                OcrWorker::new(Box::new(MockBackend {
                    spawn_calls: spawns_bg,
                    drop_calls: drops_bg,
                    drop_tx: None,
                    fail_first_read_on_spawn: None,
                }))
            },
            |callback, result| callback(result),
        );

        let (res_tx, res_rx) = mpsc::channel();
        let (frame, region, viewport, settings) = test_ocr_request_args(5);
        controller
            .read_region_async(frame, region, viewport, settings, move |res| {
                let _ = res_tx.send(res);
            })
            .unwrap();
        assert_eq!(
            res_rx.recv_timeout(Duration::from_secs(2)).unwrap(),
            Ok("frame-5".to_owned())
        );
        assert_eq!(spawns.load(Ordering::SeqCst), 1);
        assert_eq!(drops.load(Ordering::SeqCst), 0);

        let started = Instant::now();
        drop(controller);
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "Drop shutdown must wake timed idle wait immediately, took {:?}",
            started.elapsed()
        );
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn pop_latest_request_with_idle_unload_releases_mutex_before_unload_callback() {
        let queue = Arc::new((Mutex::new(RequestSlot::new()), Condvar::new()));
        let queue_bg = Arc::clone(&queue);
        let (entered_tx, entered_rx) = mpsc::channel();
        let (resume_tx, resume_rx) = mpsc::channel();
        let unload_calls = Arc::new(AtomicUsize::new(0));
        let unload_calls_bg = Arc::clone(&unload_calls);

        let worker = thread::spawn(move || {
            pop_latest_request_with_idle_unload(
                &queue_bg,
                true,
                Duration::from_millis(20),
                || {
                    assert!(
                        queue_bg.0.try_lock().is_ok(),
                        "queue mutex must be dropped before calling idle unload callback"
                    );
                    unload_calls_bg.fetch_add(1, Ordering::SeqCst);
                    let _ = entered_tx.send(());
                    let _ = resume_rx.recv_timeout(Duration::from_secs(2));
                },
            )
        });

        entered_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("idle unload callback should fire after timeout");

        let (frame, region, viewport, settings) = test_ocr_request_args(42);
        {
            let (lock, cvar) = &*queue;
            let mut slot = lock.lock().unwrap();
            slot.push_latest(OcrRequest {
                frame,
                region,
                viewport,
                settings,
                callback: Box::new(|_| {}),
            })
            .unwrap();
            cvar.notify_one();
        }

        resume_tx.send(()).unwrap();
        let popped = worker.join().unwrap().expect("expected queued request");
        assert_eq!(popped.frame.pixels[0], 42);
        assert_eq!(unload_calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn python_daemon_protocol_isolates_stdout_noise_and_round_trips_rgba_frames() {
        let stub_script = r#"import json, os, sys
_proto_fd = os.dup(1)
os.dup2(2, 1)
_proto_out = os.fdopen(_proto_fd, "wb", buffering=0)
sys.stdout = sys.stderr
print("noisy library stdout banner that must not break protocol")
def _send(obj):
    _proto_out.write((json.dumps(obj, ensure_ascii=False) + "\n").encode("utf-8"))
    _proto_out.flush()
_send({"status": "ready", "device": "cpu"})
stdin = sys.stdin.buffer
while True:
    header = stdin.readline()
    if not header:
        break
    req = json.loads(header.decode("utf-8"))
    if req.get("op") == "quit":
        break
    n = int(req["len"])
    raw = stdin.read(n)
    print("noisy inference log")
    _send({"ok": True, "text": f"日本語:{req['width']}x{req['height']}:{raw[0]}"})
"#;
        let candidates = python_candidates_from_env(None, None, None, None, &[], cfg!(windows), |_| false);
        let mut daemon =
            spawn_python_session_from_candidates(&candidates, stub_script, "stub", false)
                .expect("python3 stub daemon should start");
        let text1 = daemon
            .read_rgba(&[77, 1, 2, 255, 88, 3, 4, 255], 2, 1)
            .expect("first RGBA read");
        assert_eq!(text1, "日本語:2x1:77");
        let text2 = daemon
            .read_rgba(&[99, 1, 2, 255], 1, 1)
            .expect("second RGBA read on persistent daemon");
        assert_eq!(text2, "日本語:1x1:99");
        daemon.shutdown();
    }
}

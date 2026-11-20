//! Manga OCR runtime for normalized player selections.
//!
//! Runtime library loading, model loading, and inference happen lazily on a
//! dedicated worker thread started on the first OCR trigger. A single-slot
//! latest-wins queue drops stale frames so 4K RGBA captures never accumulate.
//! Results are posted back to Slint's event loop so callers can update UI state
//! directly.

use libloading::Library;
use std::ffi::{c_char, c_int, c_void, CStr, CString, OsStr};
use std::fmt;
use std::mem::ManuallyDrop;
use std::path::{Path, PathBuf};
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};

use memento_util::ocrutils::{map_ocr_region, resolve_venv_python_str, OcrRegionStatus};

const MOCR_MODE_RGBA: c_int = 4;
const OCR_WORKER_STACK_SIZE: usize = 8 * 1024 * 1024;

static OCR_WORKER_STARTED: AtomicBool = AtomicBool::new(false);

#[repr(C)]
struct MocrContextOpaque {
    _private: [u8; 0],
}

type MocrInit = unsafe extern "C" fn(*const c_char, c_int) -> *mut MocrContextOpaque;
type MocrDestroy = unsafe extern "C" fn(*mut MocrContextOpaque) -> c_int;
type MocrRead =
    unsafe extern "C" fn(*mut MocrContextOpaque, *const c_void, usize, usize, c_int) -> *mut c_char;
type MocrFree = unsafe extern "C" fn(*mut c_void) -> c_int;

#[cfg(target_os = "macos")]
fn python_library_candidates() -> &'static [&'static str] {
    &[
        "libpython3.14.dylib",
        "libpython3.13.dylib",
        "libpython3.12.dylib",
        "libpython3.11.dylib",
        "libpython3.10.dylib",
        "libpython3.dylib",
    ]
}

#[cfg(all(unix, not(target_os = "macos")))]
fn python_library_candidates() -> &'static [&'static str] {
    &[
        "libpython3.14.so.1.0",
        "libpython3.14.so",
        "libpython3.13.so.1.0",
        "libpython3.13.so",
        "libpython3.12.so.1.0",
        "libpython3.12.so",
        "libpython3.11.so.1.0",
        "libpython3.11.so",
        "libpython3.10.so.1.0",
        "libpython3.10.so",
        "libpython3.so",
    ]
}

const WINDOWS_MOCR_DLL_NAMES: &[&str] = &["libmocr.dll", "mocr.dll"];

/// Build ordered Windows `libmocr.dll` discovery candidates (#135):
/// 1. Executable directory (`<exe_dir>/libmocr.dll`, `<exe_dir>/mocr.dll`, `<exe_dir>/mocr/...`)
/// 2. `%LOCALAPPDATA%\memento` (and `%LOCALAPPDATA%\memento\ocr`)
/// 3. Existing `libmocr.dll` / `mocr.dll` matches on `PATH` (when present)
/// 4. Bare DLL names so Windows `LoadLibraryW` default search order still runs.
pub fn windows_mocr_library_candidates(
    exe_dir: Option<&Path>,
    local_app_data: Option<&Path>,
    path_var: Option<&OsStr>,
    path_exists: impl Fn(&Path) -> bool,
) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    let mut push_unique = |candidate: PathBuf| {
        if !candidates.contains(&candidate) {
            candidates.push(candidate);
        }
    };

    if let Some(dir) = exe_dir.filter(|d| !d.as_os_str().is_empty()) {
        for name in WINDOWS_MOCR_DLL_NAMES {
            push_unique(dir.join(name));
        }
        let nested = dir.join("mocr");
        for name in WINDOWS_MOCR_DLL_NAMES {
            push_unique(nested.join(name));
        }
    }

    if let Some(local) = local_app_data.filter(|d| !d.as_os_str().is_empty()) {
        let memento_dir = local.join("memento");
        for name in WINDOWS_MOCR_DLL_NAMES {
            push_unique(memento_dir.join(name));
        }
        let ocr_dir = memento_dir.join("ocr");
        for name in WINDOWS_MOCR_DLL_NAMES {
            push_unique(ocr_dir.join(name));
        }
    }

    if let Some(raw_path) = path_var.and_then(OsStr::to_str) {
        for entry in raw_path.split(';') {
            let trimmed = entry.trim().trim_matches('"');
            if trimmed.is_empty() {
                continue;
            }
            let dir = Path::new(trimmed);
            for name in WINDOWS_MOCR_DLL_NAMES {
                let candidate = dir.join(name);
                if path_exists(&candidate) {
                    push_unique(candidate);
                }
            }
        }
    }

    for name in WINDOWS_MOCR_DLL_NAMES {
        push_unique(PathBuf::from(name));
    }

    candidates
}

fn format_library_unavailable_message(
    is_windows: bool,
    found_on_disk: &[String],
    errors: &[String],
) -> String {
    let detail = if errors.is_empty() {
        "no library candidates configured".to_owned()
    } else {
        errors.join("; ")
    };
    if is_windows {
        if !found_on_disk.is_empty() {
            format!(
                "libmocr.dll was found at {} but failed to load (verify Python 3 and MSVC/CUDA runtime DLLs are installed and match 64-bit architecture). Details: {detail}",
                found_on_disk.join(", ")
            )
        } else {
            format!(
                "libmocr.dll was not found. Install libmocr (manga-ocr) and place libmocr.dll next to memento.exe, in %LOCALAPPDATA%\\memento, or on PATH. Details: {detail}"
            )
        }
    } else {
        format!(
            "libmocr shared library was not found or could not be loaded. Install libmocr and ensure it is in your library search path. Details: {detail}"
        )
    }
}

struct MocrApi {
    // Keep libmocr loaded until every context and returned text is released,
    // and drop it before any explicitly retained libpython handle.
    _library: Library,
    #[cfg(unix)]
    _python_library: Option<libloading::os::unix::Library>,
    init: MocrInit,
    destroy: MocrDestroy,
    read: MocrRead,
    free: MocrFree,
}

impl MocrApi {
    fn load() -> Result<Self, OcrError> {
        #[cfg(target_os = "linux")]
        let candidates: Vec<PathBuf> = [
            "/usr/local/lib/libmocr.so",
            "/usr/lib/libmocr.so",
            "libmocr.so",
        ]
        .into_iter()
        .map(PathBuf::from)
        .collect();
        #[cfg(target_os = "macos")]
        let candidates: Vec<PathBuf> = [
            "/usr/local/lib/libmocr.dylib",
            "/opt/homebrew/lib/libmocr.dylib",
            "libmocr.dylib",
        ]
        .into_iter()
        .map(PathBuf::from)
        .collect();
        #[cfg(target_os = "windows")]
        let candidates: Vec<PathBuf> = {
            let exe_dir = std::env::current_exe()
                .ok()
                .and_then(|exe| exe.parent().map(Path::to_path_buf));
            let local_app_data = std::env::var_os("LOCALAPPDATA").map(PathBuf::from);
            let path_var = std::env::var_os("PATH");
            windows_mocr_library_candidates(
                exe_dir.as_deref(),
                local_app_data.as_deref(),
                path_var.as_deref(),
                |candidate| candidate.is_file(),
            )
        };
        #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
        let candidates: Vec<PathBuf> = vec![PathBuf::from("libmocr.so")];

        // On Unix, embedded Python C-extensions (e.g. `unicodedata.so`) often do
        // not link directly against `libpython` and expect its C API symbols
        // (such as `PyModule_Type`) to already be globally visible. Retain the
        // first matching `libpython` handle in `MocrApi` with `RTLD_GLOBAL`,
        // while keeping `libmocr` itself `RTLD_LOCAL`.
        #[cfg(unix)]
        let python_library = {
            let mut loaded = None;
            for py_name in python_library_candidates() {
                if let Ok(lib) = unsafe {
                    libloading::os::unix::Library::open(
                        Some(py_name),
                        libloading::os::unix::RTLD_NOW | libloading::os::unix::RTLD_GLOBAL,
                    )
                } {
                    loaded = Some(lib);
                    break;
                }
            }
            loaded
        };

        let mut errors = Vec::new();
        let mut found_on_disk = Vec::new();
        for candidate in candidates {
            let display = candidate.display().to_string();
            let has_explicit_dir = candidate.parent().is_some_and(|p| !p.as_os_str().is_empty());
            if has_explicit_dir {
                if !candidate.is_file() {
                    errors.push(format!("{display}: not found"));
                    continue;
                }
                found_on_disk.push(display.clone());
            }
            // SAFETY: We load only known library names/absolute install paths
            // with RTLD_LOCAL (default `Library::new`), then validate every
            // required symbol before exposing the API.
            let library = match unsafe { Library::new(candidate.as_os_str()) } {
                Ok(library) => library,
                Err(error) => {
                    errors.push(format!("{display}: {error}"));
                    continue;
                }
            };
            // SAFETY: libmocr exports these symbols with the declared C ABI.
            let symbols: Result<_, libloading::Error> = (|| unsafe {
                Ok((
                    *library.get::<MocrInit>(b"mocr_init\0")?,
                    *library.get::<MocrDestroy>(b"mocr_destroy\0")?,
                    *library.get::<MocrRead>(b"mocr_read\0")?,
                    *library.get::<MocrFree>(b"mocr_free\0")?,
                ))
            })();
            match symbols {
                Ok((init, destroy, read, free)) => {
                    return Ok(Self {
                        _library: library,
                        #[cfg(unix)]
                        _python_library: python_library,
                        init,
                        destroy,
                        read,
                        free,
                    });
                }
                Err(error) => errors.push(format!(
                    "{display}: missing/incompatible libmocr symbol: {error}"
                )),
            }
        }
        Err(OcrError::LibraryUnavailable(
            format_library_unavailable_message(cfg!(target_os = "windows"), &found_on_disk, &errors),
        ))
    }
}

/// Settings needed for one OCR operation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OcrSettings {
    /// Whether OCR is enabled in application settings.
    pub enabled: bool,
    /// A local model path, Hugging Face repo, or URL supported by libmocr.
    pub model: String,
    /// Whether libmocr may use CUDA when available.
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
/// padding; the OCR crop is copied into a tightly packed buffer before calling
/// libmocr.
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

/// Failure reported by frame validation, region mapping, or libmocr.
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
    ModelDestructionFailed(i32),
    InferenceFailed,
    InvalidNativeText,
    NativeTextFreeFailed(i32),
    NoTextDetected,
    WorkerStartFailed(String),
    WorkerUnavailable,
    LibraryUnavailable(String),
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
            Self::ModelInitializationFailed(model) => {
                write!(formatter, "Could not initialize OCR model `{model}`.")
            }
            Self::ModelDestructionFailed(status) => {
                write!(
                    formatter,
                    "Could not release the previous OCR model (libmocr status {status})."
                )
            }
            Self::InferenceFailed => {
                formatter.write_str("libmocr could not read the selected image.")
            }
            Self::InvalidNativeText => {
                formatter.write_str("libmocr returned text that was not valid UTF-8.")
            }
            Self::NativeTextFreeFailed(status) => {
                write!(
                    formatter,
                    "Could not release OCR text (libmocr status {status})."
                )
            }
            Self::NoTextDetected => formatter.write_str("No text was detected."),
            Self::LibraryUnavailable(error) => {
                write!(formatter, "Could not load libmocr: {error}")
            }
            Self::WorkerStartFailed(error) => {
                write!(formatter, "Could not start OCR worker: {error}")
            }
            Self::WorkerUnavailable => formatter.write_str("The OCR worker is unavailable."),
        }
    }
}

impl std::error::Error for OcrError {}

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

/// Asynchronous OCR controller backed by one lazily initialized libmocr worker.
///
/// Neither `libpython`/`libmocr` loading nor the 8 MiB worker thread happens in
/// [`OcrController::new`]; both initialize lazily on the first OCR request.
/// Native contexts are created, used, switched, and destroyed exclusively on
/// the worker thread, which shuts down and joins cleanly when the last
/// controller handle drops.
#[derive(Clone)]
pub struct OcrController {
    inner: Arc<OcrControllerInner>,
}

impl OcrController {
    /// Creates a lazy OCR controller. Native libraries and the worker thread
    /// are initialized on the first [`Self::read_region_async`] call.
    pub fn new() -> Result<Self, OcrError> {
        Ok(Self {
            inner: Arc::new(OcrControllerInner {
                queue: Arc::new((Mutex::new(RequestSlot::new()), Condvar::new())),
                worker: Mutex::new(None),
            }),
        })
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
        OCR_WORKER_STARTED.store(true, Ordering::Release);
        let handle = thread::Builder::new()
            .name("memento-ocr".to_owned())
            // libmocr's model initialization needs a larger stack on macOS.
            .stack_size(OCR_WORKER_STACK_SIZE)
            .spawn(move || worker_loop(queue))
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

fn pop_latest_request(queue: &(Mutex<RequestSlot>, Condvar)) -> Option<OcrRequest> {
    let (lock, cvar) = queue;
    let mut slot = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    loop {
        if slot.closed {
            slot.pending = None;
            return None;
        }
        if let Some(request) = slot.pending.take() {
            return Some(request);
        }
        slot = cvar
            .wait(slot)
            .unwrap_or_else(|poisoned| poisoned.into_inner());
    }
}

fn worker_loop(queue: Arc<(Mutex<RequestSlot>, Condvar)>) {
    let mut worker = OcrWorker {
        model: None,
        api: None,
    };
    while let Some(request) = pop_latest_request(&queue) {
        let result = worker.read(
            request.frame,
            request.region,
            request.viewport,
            request.settings,
        );
        if let Err(error) = slint::invoke_from_event_loop(move || (request.callback)(result)) {
            eprintln!("Could not deliver OCR result to Slint event loop: {error}");
        }
    }
}

fn normalize_model_name(raw: &str) -> Result<&str, OcrError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.as_bytes().contains(&0) {
        return Err(OcrError::InvalidModelName);
    }
    Ok(trimmed)
}

struct OcrWorker {
    // Field order is deliberate: the native context must be destroyed before
    // `libmocr` is unloaded, and `libmocr` before `libpython`.
    model: Option<LoadedModel>,
    api: Option<MocrApi>,
}

struct LoadedModel {
    name: String,
    use_gpu: bool,
    context: MocrContext,
}

impl OcrWorker {
    fn ensure_api(&mut self) -> Result<&MocrApi, OcrError> {
        if self.api.is_none() {
            self.api = Some(MocrApi::load()?);
        }
        self.api.as_ref().ok_or_else(|| {
            OcrError::LibraryUnavailable("libmocr runtime is not initialized".to_owned())
        })
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
        let (read, free) = {
            let api = self.ensure_api()?;
            (api.read, api.free)
        };
        let model = self.model_for(&model_name, settings.use_gpu)?;
        let text = model
            .context
            .read_rgba(read, free, &pixels, width, height)?;
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
            if let Some(previous) = self.model.take() {
                previous.context.destroy()?;
            }
            let api = self.ensure_api()?;
            let context = MocrContext::new(api, model_name, use_gpu)?;
            self.model = Some(LoadedModel {
                name: model_name.to_owned(),
                use_gpu,
                context,
            });
        }
        self.model
            .as_mut()
            .ok_or_else(|| OcrError::ModelInitializationFailed(model_name.to_owned()))
    }
}

struct MocrContext {
    pointer: NonNull<MocrContextOpaque>,
    destroy: MocrDestroy,
}

impl MocrContext {
    fn new(api: &MocrApi, model: &str, use_gpu: bool) -> Result<Self, OcrError> {
        let model_str = normalize_model_name(model)?;
        let model_name = CString::new(model_str).map_err(|_| OcrError::InvalidModelName)?;
        let mut pointer = unsafe { (api.init)(model_name.as_ptr(), if use_gpu { 0 } else { 1 }) };
        if pointer.is_null() && use_gpu {
            pointer = unsafe { (api.init)(model_name.as_ptr(), 1) };
        }
        let pointer = NonNull::new(pointer)
            .ok_or_else(|| OcrError::ModelInitializationFailed(model_str.to_owned()))?;
        Ok(Self {
            pointer,
            destroy: api.destroy,
        })
    }

    fn read_rgba(
        &mut self,
        read: MocrRead,
        free: MocrFree,
        pixels: &[u8],
        width: usize,
        height: usize,
    ) -> Result<String, OcrError> {
        let expected_bytes = width
            .checked_mul(height)
            .and_then(|px| px.checked_mul(4))
            .ok_or(OcrError::InvalidFrameDimensions)?;
        if width == 0 || height == 0 || pixels.len() != expected_bytes {
            return Err(OcrError::InvalidFrameBuffer);
        }
        // SAFETY: the context is live, `pixels` stays borrowed and alive for the
        // entire synchronous `mocr_read` call and contains `width * height * 4`
        // RGBA bytes (`MOCR_MODE_RGBA`). `mocr_read` takes a read-only image buffer.
        let output = unsafe {
            (read)(
                self.pointer.as_ptr(),
                pixels.as_ptr().cast(),
                width,
                height,
                MOCR_MODE_RGBA,
            )
        };
        let output = NonNull::new(output).ok_or(OcrError::InferenceFailed)?;
        MocrText::new(output, free).into_string()
    }

    fn destroy(self) -> Result<(), OcrError> {
        let this = ManuallyDrop::new(self);
        // SAFETY: `ManuallyDrop` suppresses `Drop`, so the owned context
        // pointer is released exactly once with the matching destructor.
        let status = unsafe { (this.destroy)(this.pointer.as_ptr()) };
        if status == 0 {
            Ok(())
        } else {
            Err(OcrError::ModelDestructionFailed(status))
        }
    }
}

impl Drop for MocrContext {
    fn drop(&mut self) {
        // SAFETY: this pointer is owned by `MocrContext` and `destroy`
        // suppresses `Drop` via `ManuallyDrop`, so it is destroyed at most once.
        let status = unsafe { (self.destroy)(self.pointer.as_ptr()) };
        if status != 0 {
            eprintln!("Could not release OCR model (libmocr status {status}).");
        }
    }
}

struct MocrText {
    pointer: NonNull<c_char>,
    free: MocrFree,
}

impl MocrText {
    fn new(pointer: NonNull<c_char>, free: MocrFree) -> Self {
        Self { pointer, free }
    }

    fn into_string(self) -> Result<String, OcrError> {
        // Convert before disarming `Drop` so an unwind still frees the pointer.
        // SAFETY: libmocr returns a NUL-terminated string allocated by
        // `mocr_read`; it remains valid until freed below.
        let text = unsafe { CStr::from_ptr(self.pointer.as_ptr()) }
            .to_str()
            .map(str::to_owned)
            .map_err(|_| OcrError::InvalidNativeText);
        let this = ManuallyDrop::new(self);
        // SAFETY: `ManuallyDrop` suppresses `Drop`, so the pointer returned by
        // `mocr_read` is released exactly once with `this.free`.
        let status = unsafe { (this.free)(this.pointer.as_ptr().cast()) };
        let text = text?;
        if status != 0 {
            return Err(OcrError::NativeTextFreeFailed(status));
        }
        Ok(text)
    }
}

impl Drop for MocrText {
    fn drop(&mut self) {
        // SAFETY: this is the pointer returned by `mocr_read` and `into_string`
        // suppresses `Drop` via `ManuallyDrop`, so it is freed at most once.
        let status = unsafe { (self.free)(self.pointer.as_ptr().cast()) };
        if status != 0 {
            eprintln!("Could not release OCR text (libmocr status {status}).");
        }
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

fn resolve_python_executable_from_env(
    python_executable: Option<&OsStr>,
    virtual_env: Option<&OsStr>,
    is_windows: bool,
    file_exists: impl Fn(&Path) -> bool,
) -> Option<String> {
    if python_executable.is_some() {
        return None;
    }
    let virtual_env = virtual_env?.to_str()?;
    let path = resolve_venv_python_str(false, virtual_env, is_windows)?;
    file_exists(Path::new(&path)).then_some(path)
}

#[cfg(target_os = "linux")]
fn process_is_single_threaded() -> bool {
    !OCR_WORKER_STARTED.load(Ordering::Acquire)
        && std::fs::read_dir("/proc/self/task")
            .map(|entries| entries.take(2).count() <= 1)
            .unwrap_or(false)
}

#[cfg(not(target_os = "linux"))]
fn process_is_single_threaded() -> bool {
    cfg!(windows) || !OCR_WORKER_STARTED.load(Ordering::Acquire)
}

/// Probes `VIRTUAL_ENV` and sets `PYTHONEXECUTABLE` once during single-threaded
/// process startup if unset.
///
/// Must be called on the main thread before any worker threads (libmpv, GUI,
/// ASR, or OCR) are spawned. If the process is already multi-threaded, this
/// skips mutating process environment variables to avoid Unix `setenv` UB.
pub fn prepare_python_executable() {
    static INIT: std::sync::Once = std::sync::Once::new();
    INIT.call_once(|| {
        const PYTHON_EXECUTABLE: &str = "PYTHONEXECUTABLE";
        const VIRTUAL_ENV: &str = "VIRTUAL_ENV";

        if !process_is_single_threaded() {
            return;
        }
        let python_executable = std::env::var_os(PYTHON_EXECUTABLE);
        let virtual_env = std::env::var_os(VIRTUAL_ENV);
        let Some(path) = resolve_python_executable_from_env(
            python_executable.as_deref(),
            virtual_env.as_deref(),
            cfg!(windows),
            |candidate| candidate.is_file(),
        ) else {
            return;
        };
        std::env::set_var(PYTHON_EXECUTABLE, path);
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

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

    #[cfg(unix)]
    #[test]
    fn python_library_probes_cover_3_14_through_3_10() {
        let candidates = python_library_candidates();
        for minor in ["3.14", "3.13", "3.12", "3.11", "3.10"] {
            assert!(
                candidates.iter().any(|name| name.contains(minor)),
                "missing libpython {minor} candidate in {candidates:?}"
            );
        }
        assert!(candidates
            .iter()
            .any(|name| name.starts_with("libpython3.")));
    }

    #[test]
    fn mocr_text_uses_owned_free_fn_and_frees_on_drop() {
        static FREE_CALLS: AtomicUsize = AtomicUsize::new(0);
        unsafe extern "C" fn mock_free(ptr: *mut c_void) -> c_int {
            FREE_CALLS.fetch_add(1, Ordering::SeqCst);
            drop(CString::from_raw(ptr.cast::<c_char>()));
            0
        }

        let before = FREE_CALLS.load(Ordering::SeqCst);
        let raw_ok = NonNull::new(CString::new("日本語").unwrap().into_raw()).unwrap();
        let text = MocrText::new(raw_ok, mock_free).into_string().unwrap();
        assert_eq!(text, "日本語");
        assert_eq!(FREE_CALLS.load(Ordering::SeqCst), before + 1);

        let raw_drop = NonNull::new(CString::new("dropped").unwrap().into_raw()).unwrap();
        drop(MocrText::new(raw_drop, mock_free));
        assert_eq!(FREE_CALLS.load(Ordering::SeqCst), before + 2);
    }

    #[test]
    fn resolve_python_executable_from_env_checks_existing_venv_python() {
        assert_eq!(
            resolve_python_executable_from_env(
                Some(OsStr::new("/already/set")),
                Some(OsStr::new("/tmp/venv")),
                false,
                |_| true
            ),
            None
        );
        assert_eq!(
            resolve_python_executable_from_env(None, Some(OsStr::new("/tmp/venv")), false, |_| {
                false
            }),
            None
        );
        assert_eq!(
            resolve_python_executable_from_env(None, Some(OsStr::new("/tmp/venv")), false, |p| {
                p == Path::new("/tmp/venv/bin/python")
            }),
            Some("/tmp/venv/bin/python".to_owned())
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn process_is_single_threaded_is_false_when_background_thread_exists() {
        let ready = Arc::new((Mutex::new(false), Condvar::new()));
        let done = Arc::new((Mutex::new(false), Condvar::new()));
        let ready_bg = Arc::clone(&ready);
        let done_bg = Arc::clone(&done);
        let bg = thread::spawn(move || {
            {
                let (lock, cvar) = &*ready_bg;
                *lock.lock().unwrap() = true;
                cvar.notify_one();
            }
            let (lock, cvar) = &*done_bg;
            let mut finished = lock.lock().unwrap();
            while !*finished {
                finished = cvar.wait(finished).unwrap();
            }
        });
        {
            let (lock, cvar) = &*ready;
            let mut started = lock.lock().unwrap();
            while !*started {
                started = cvar.wait(started).unwrap();
            }
        }
        assert!(
            !process_is_single_threaded(),
            "must refuse process env mutation while other threads are running"
        );
        {
            let (lock, cvar) = &*done;
            *lock.lock().unwrap() = true;
            cvar.notify_one();
        }
        bg.join().unwrap();
    }

    #[test]
    fn windows_libmocr_discovery_checks_exe_dir_localappdata_path_and_explains_missing_dll() {
        let exe_dir = Path::new("/win/ProgramFiles/Memento");
        let local_app_data = Path::new("/win/Users/Alice/AppData/Local");
        let path_env = OsStr::new("/win/tools/ocr;/win/empty");
        let candidates = windows_mocr_library_candidates(
            Some(exe_dir),
            Some(local_app_data),
            Some(path_env),
            |p| p == Path::new("/win/tools/ocr/libmocr.dll"),
        );

        assert_eq!(candidates[0], exe_dir.join("libmocr.dll"));
        assert_eq!(candidates[1], exe_dir.join("mocr.dll"));
        assert!(candidates.contains(&local_app_data.join("memento").join("libmocr.dll")));
        assert!(candidates.contains(&PathBuf::from("/win/tools/ocr/libmocr.dll")));
        assert!(!candidates.contains(&PathBuf::from("/win/empty/libmocr.dll")));
        assert_eq!(
            candidates.last().map(PathBuf::as_path),
            Some(Path::new("mocr.dll"))
        );

        // Priority order: exe dir -> %LOCALAPPDATA%\memento -> PATH -> bare DLL fallback.
        let idx_exe = candidates
            .iter()
            .position(|p| p == &exe_dir.join("libmocr.dll"))
            .unwrap();
        let idx_local = candidates
            .iter()
            .position(|p| p == &local_app_data.join("memento").join("libmocr.dll"))
            .unwrap();
        let idx_path = candidates
            .iter()
            .position(|p| p == Path::new("/win/tools/ocr/libmocr.dll"))
            .unwrap();
        let idx_bare = candidates
            .iter()
            .position(|p| p == Path::new("libmocr.dll"))
            .unwrap();
        assert!(idx_exe < idx_local && idx_local < idx_path && idx_path < idx_bare);

        let missing_msg = format_library_unavailable_message(
            true,
            &[],
            &["libmocr.dll: not found".to_owned()],
        );
        assert!(missing_msg.contains("libmocr.dll"));
        assert!(missing_msg.contains("%LOCALAPPDATA%\\memento"));
        assert!(missing_msg.contains("PATH"));

        let broken_deps_msg = format_library_unavailable_message(
            true,
            &["C:\\Memento\\libmocr.dll".to_owned()],
            &["missing python312.dll".to_owned()],
        );
        assert!(broken_deps_msg.contains("found at C:\\Memento\\libmocr.dll"));
        assert!(broken_deps_msg.contains("Python 3"));
    }
}

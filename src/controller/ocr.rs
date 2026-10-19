//! Manga OCR runtime for normalized player selections.
//!
//! Model loading and inference run on a dedicated worker thread. Its callback
//! is posted to Slint's event loop, so callers can update UI state directly.

use libloading::Library;
use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::fmt;
use std::path::Path;
use std::ptr::NonNull;
use std::sync::mpsc::{self, Sender};
use std::thread;

use memento_util::ocrutils::{map_ocr_region, OcrRegionStatus};

const MOCR_MODE_RGBA: c_int = 4;
const OCR_WORKER_STACK_SIZE: usize = 8 * 1024 * 1024;

#[repr(C)]
struct MocrContextOpaque {
    _private: [u8; 0],
}

type MocrInit = unsafe extern "C" fn(*const c_char, c_int) -> *mut MocrContextOpaque;
type MocrDestroy = unsafe extern "C" fn(*mut MocrContextOpaque) -> c_int;
type MocrRead =
    unsafe extern "C" fn(*mut MocrContextOpaque, *mut c_void, usize, usize, c_int) -> *mut c_char;
type MocrFree = unsafe extern "C" fn(*mut c_void) -> c_int;

struct MocrApi {
    // Keep the library loaded until every context and returned text is released.
    _library: Library,
    init: MocrInit,
    destroy: MocrDestroy,
    read: MocrRead,
    free: MocrFree,
}

impl MocrApi {
    fn load() -> Result<Self, OcrError> {
        #[cfg(target_os = "linux")]
        let candidates = ["/usr/local/lib/libmocr.so", "libmocr.so"];
        #[cfg(target_os = "macos")]
        let candidates = ["/usr/local/lib/libmocr.dylib", "libmocr.dylib"];
        #[cfg(target_os = "windows")]
        let candidates = ["libmocr.dll", "mocr.dll"];
        #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
        let candidates = ["libmocr.so"];

        #[cfg(unix)]
        {
            // On Unix, ensure libpython symbols are exported with RTLD_GLOBAL so that
            // dynamic C-extensions like `unicodedata.so` loaded by Python can resolve PyModule_Type.
            for py_name in [
                "libpython3.14.so.1.0",
                "libpython3.14.so",
                "libpython3.13.so.1.0",
                "libpython3.13.so",
                "libpython3.so",
            ] {
                unsafe {
                    let _ = libloading::os::unix::Library::open(
                        Some(py_name),
                        libloading::os::unix::RTLD_NOW | libloading::os::unix::RTLD_GLOBAL,
                    );
                }
            }
        }

        let mut errors = Vec::new();
        for candidate in candidates {
            // SAFETY: We load only known library names/absolute install paths with RTLD_GLOBAL,
            // then validate every required symbol before exposing the API.
            let library = match (|| unsafe {
                #[cfg(unix)]
                {
                    libloading::os::unix::Library::open(
                        Some(candidate),
                        libloading::os::unix::RTLD_NOW | libloading::os::unix::RTLD_GLOBAL,
                    )
                    .map(Library::from)
                }
                #[cfg(not(unix))]
                {
                    Library::new(candidate)
                }
            })() {
                Ok(library) => library,
                Err(error) => {
                    errors.push(format!("{candidate}: {error}"));
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
                    return Ok(Self { _library: library, init, destroy, read, free });
                }
                Err(error) => errors.push(format!("{candidate}: missing/incompatible libmocr symbol: {error}")),
            }
        }
        Err(OcrError::LibraryUnavailable(errors.join("; ")))
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
    pub fn new(
        width: u32,
        height: u32,
        stride: usize,
        pixels: Vec<u8>,
    ) -> Result<Self, OcrError> {
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
        Ok(Self { width, height, stride, pixels })
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
            Self::InvalidFrameDimensions => formatter.write_str("The screenshot dimensions are invalid."),
            Self::InvalidFrameStride => formatter.write_str("The screenshot row stride is invalid."),
            Self::InvalidFrameBuffer => formatter.write_str("The screenshot pixel buffer is incomplete."),
            Self::SelectionOutsideVideo => formatter.write_str("The selected region does not include video."),
            Self::EmptyRegion => formatter.write_str("The selected region is empty."),
            Self::InvalidModelName => formatter.write_str("The configured OCR model name is invalid."),
            Self::ModelInitializationFailed(model) => {
                write!(formatter, "Could not initialize OCR model `{model}`.")
            }
            Self::ModelDestructionFailed(status) => {
                write!(formatter, "Could not release the previous OCR model (libmocr status {status}).")
            }
            Self::InferenceFailed => formatter.write_str("libmocr could not read the selected image."),
            Self::InvalidNativeText => formatter.write_str("libmocr returned text that was not valid UTF-8."),
            Self::NativeTextFreeFailed(status) => {
                write!(formatter, "Could not release OCR text (libmocr status {status}).")
            }
            Self::NoTextDetected => formatter.write_str("No text was detected."),
            Self::LibraryUnavailable(error) => write!(formatter, "Could not load libmocr: {error}"),
            Self::WorkerStartFailed(error) => write!(formatter, "Could not start OCR worker: {error}"),
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

/// Asynchronous OCR controller backed by one persistent libmocr worker.
///
/// The native context is created, used, switched, and destroyed exclusively on
/// the worker. Results are delivered on Slint's event-loop thread.
#[derive(Clone)]
pub struct OcrController {
    requests: Sender<OcrRequest>,
}

impl OcrController {
    /// Starts the native OCR worker after confirming the optional runtime can
    /// be loaded. Model loading remains lazy on the worker.
    pub fn new() -> Result<Self, OcrError> {
        let api = MocrApi::load()?;
        let (requests, receiver) = mpsc::channel();
        thread::Builder::new()
            .name("memento-ocr".to_owned())
            // libmocr's model initialization needs a larger stack on macOS.
            .stack_size(OCR_WORKER_STACK_SIZE)
            .spawn(move || worker_loop(receiver, api))
            .map_err(|error| OcrError::WorkerStartFailed(error.to_string()))?;
        Ok(Self { requests })
    }

    /// Queues a screenshot region for OCR without blocking the UI thread.
    ///
    /// `frame` must contain the source-resolution, uncomposited RGBA video
    /// screenshot. `viewport` is the Slint viewport size in the same coordinate
    /// space as the normalized drag, not the screenshot dimensions. The
    /// callback is invoked on Slint's event loop with detected text or a typed
    /// error.
    pub fn read_region_async(
        &self,
        frame: OcrFrame,
        region: OcrRegion,
        viewport: OcrViewport,
        settings: OcrSettings,
        callback: impl FnOnce(Result<String, OcrError>) + Send + 'static,
    ) -> Result<(), OcrError> {
        self.requests
            .send(OcrRequest {
                frame,
                region,
                viewport,
                settings,
                callback: Box::new(callback),
            })
            .map_err(|_| OcrError::WorkerUnavailable)
    }
}

fn worker_loop(receiver: mpsc::Receiver<OcrRequest>, api: MocrApi) {
    let mut worker = OcrWorker { api, model: None };
    while let Ok(request) = receiver.recv() {
        let result = worker.read(request.frame, request.region, request.viewport, request.settings);
        if let Err(error) = slint::invoke_from_event_loop(move || (request.callback)(result)) {
            eprintln!("Could not deliver OCR result to Slint event loop: {error}");
        }
    }
}

struct OcrWorker {
    // The native context must be dropped before the library handle.
    model: Option<LoadedModel>,
    api: MocrApi,
}

struct LoadedModel {
    name: String,
    use_gpu: bool,
    context: MocrContext,
}

impl OcrWorker {
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
        let (mut pixels, width, height) = crop_frame(&frame, region, viewport)?;
        let read = self.api.read;
        let free = self.api.free;
        let model = self.model_for(&settings)?;
        let text = model.context.read_rgba(read, free, &mut pixels, width, height)?;
        let text = text.trim();
        if text.is_empty() {
            return Err(OcrError::NoTextDetected);
        }
        Ok(text.to_owned())
    }

    fn model_for(&mut self, settings: &OcrSettings) -> Result<&mut LoadedModel, OcrError> {
        if settings.model.trim().is_empty() || settings.model.as_bytes().contains(&0) {
            return Err(OcrError::InvalidModelName);
        }
        let needs_recreate = self.model.as_ref().map_or(true, |model| {
            model.name != settings.model || model.use_gpu != settings.use_gpu
        });
        if needs_recreate {
            if let Some(previous) = self.model.take() {
                previous.context.destroy()?;
            }
            prepare_python_executable();
            let context = MocrContext::new(&self.api, &settings.model, settings.use_gpu)?;
            self.model = Some(LoadedModel {
                name: settings.model.clone(),
                use_gpu: settings.use_gpu,
                context,
            });
        }
        Ok(self.model.as_mut().expect("model was initialized above"))
    }
}

struct MocrContext {
    pointer: Option<NonNull<MocrContextOpaque>>,
    destroy: MocrDestroy,
}

impl MocrContext {
    fn new(api: &MocrApi, model: &str, use_gpu: bool) -> Result<Self, OcrError> {
        let model_str = if model.trim().is_empty() {
            "kha-white/manga-ocr-base"
        } else {
            model.trim()
        };
        if std::env::var_os("PYTHONEXECUTABLE").is_none() {
            if let Some(venv) = std::env::var_os("VIRTUAL_ENV") {
                let py = std::path::Path::new(&venv).join("bin/python");
                if py.is_file() {
                    std::env::set_var("PYTHONEXECUTABLE", py);
                }
            }
        }
        let model_name = CString::new(model_str).map_err(|_| OcrError::InvalidModelName)?;
        let mut pointer = unsafe { (api.init)(model_name.as_ptr(), if use_gpu { 0 } else { 1 }) };
        if pointer.is_null() && use_gpu {
            pointer = unsafe { (api.init)(model_name.as_ptr(), 1) };
        }
        let pointer = NonNull::new(pointer).ok_or_else(|| {
            OcrError::ModelInitializationFailed(model_str.to_owned())
        })?;
        Ok(Self { pointer: Some(pointer), destroy: api.destroy })
    }

    fn read_rgba(
        &mut self,
        read: MocrRead,
        free: MocrFree,
        pixels: &mut [u8],
        width: usize,
        height: usize,
    ) -> Result<String, OcrError> {
        let context = self.pointer.expect("live OCR context");
        // SAFETY: the context is live, pixels is mutable and contains exactly
        // width * height RGBA bytes, and libmocr accepts mocr_mode_RGBA (4).
        let output = unsafe {
            (read)(context.as_ptr(), pixels.as_mut_ptr().cast(), width, height, MOCR_MODE_RGBA)
        };
        let output = NonNull::new(output).ok_or(OcrError::InferenceFailed)?;
        MocrText::new(output, free).into_string(free)
    }

    fn destroy(mut self) -> Result<(), OcrError> {
        let pointer = self.pointer.take().expect("live OCR context");
        // SAFETY: this wrapper owns the context and clears its pointer before
        // calling the matching libmocr destructor.
        let status = unsafe { (self.destroy)(pointer.as_ptr()) };
        if status == 0 {
            Ok(())
        } else {
            Err(OcrError::ModelDestructionFailed(status))
        }
    }
}

impl Drop for MocrContext {
    fn drop(&mut self) {
        if let Some(pointer) = self.pointer.take() {
            // SAFETY: the pointer is owned by this wrapper and is destroyed at
            // most once. Drop runs on the OCR worker thread.
            let status = unsafe { (self.destroy)(pointer.as_ptr()) };
            if status != 0 {
                eprintln!("Could not release OCR model (libmocr status {status}).");
            }
        }
    }
}

struct MocrText {
    pointer: Option<NonNull<c_char>>,
    free: MocrFree,
}

impl MocrText {
    fn new(pointer: NonNull<c_char>, free: MocrFree) -> Self {
        Self { pointer: Some(pointer), free }
    }

    fn into_string(mut self, free: MocrFree) -> Result<String, OcrError> {
        let pointer = self.pointer.take().expect("live OCR text");
        // SAFETY: libmocr returns a NUL-terminated string allocated by
        // mocr_read; it remains valid until freed below.
        let text = unsafe { CStr::from_ptr(pointer.as_ptr()) }.to_str()
            .map(str::to_owned)
            .map_err(|_| OcrError::InvalidNativeText);
        // SAFETY: pointer came from mocr_read and is released by mocr_free.
        let status = unsafe { free(pointer.as_ptr().cast()) };
        let text = text?;
        if status != 0 {
            return Err(OcrError::NativeTextFreeFailed(status));
        }
        Ok(text)
    }
}

impl Drop for MocrText {
    fn drop(&mut self) {
        if let Some(pointer) = self.pointer.take() {
            // SAFETY: this is the pointer returned by mocr_read, which must be
            // released with mocr_free if text conversion exits early.
            let status = unsafe { (self.free)(pointer.as_ptr().cast()) };
            if status != 0 {
                eprintln!("Could not release OCR text (libmocr status {status}).");
            }
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
    let row_bytes = width.checked_mul(4).ok_or(OcrError::InvalidFrameDimensions)?;
    let output_bytes = row_bytes.checked_mul(height).ok_or(OcrError::InvalidFrameDimensions)?;
    let mut crop = Vec::with_capacity(output_bytes);
    for row in y..y + height {
        let start = row
            .checked_mul(frame.stride)
            .and_then(|offset| offset.checked_add(x.checked_mul(4)?))
            .ok_or(OcrError::InvalidFrameBuffer)?;
        let end = start.checked_add(row_bytes).ok_or(OcrError::InvalidFrameBuffer)?;
        crop.extend_from_slice(frame.pixels.get(start..end).ok_or(OcrError::InvalidFrameBuffer)?);
    }
    Ok((crop, width, height))
}

fn prepare_python_executable() {
    const PYTHON_EXECUTABLE: &str = "PYTHONEXECUTABLE";
    const VIRTUAL_ENV: &str = "VIRTUAL_ENV";

    if std::env::var_os(PYTHON_EXECUTABLE).is_some() {
        return;
    }
    let Some(virtual_env) = std::env::var_os(VIRTUAL_ENV) else {
        return;
    };
    let virtual_env: Vec<u16> = virtual_env.to_string_lossy().encode_utf16().collect();
    let Some(path) = memento_util::ocrutils::resolve_venv_python(
        false,
        &virtual_env,
        cfg!(windows),
    ) else {
        return;
    };
    let Ok(path) = String::from_utf16(&path) else {
        return;
    };
    if Path::new(&path).is_file() {
        std::env::set_var(PYTHON_EXECUTABLE, path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crop_maps_normalized_region_and_honors_row_stride() {
        let mut pixels = vec![0; 24];
        pixels[0..8].copy_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8]);
        pixels[12..20].copy_from_slice(&[9, 10, 11, 12, 13, 14, 15, 16]);
        let frame = OcrFrame::new(2, 2, 12, pixels).unwrap();
        let (crop, width, height) = crop_frame(
            &frame,
            OcrRegion { x: 0.5, y: 0.5, width: 0.5, height: 0.5 },
            OcrViewport { width: 2.0, height: 2.0 },
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
            OcrRegion { x: 0.0, y: 0.0, width: 1.0, height: 0.1 },
            OcrViewport { width: 16.0, height: 16.0 },
        )
        .unwrap_err();
        assert_eq!(error, OcrError::SelectionOutsideVideo);
    }
}

//! Feature combinations must work with settings saved by a different build.
use subomi_app::controller::asr::{AsrSettings, AVAILABLE};
use subomi_config::{
    config::Config,
    keys::{raw, AsrField, Key},
};

#[test]
fn persisted_asr_settings_respect_compiled_backends() {
    for saved in [raw::asr::BACKEND_WHISPER, raw::asr::BACKEND_ONNX] {
        let mut config = Config::default_empty();
        config.set(&Key::Asr(AsrField::Enabled), true).unwrap();
        config.set(&Key::Asr(AsrField::Backend), saved).unwrap();
        let result = AsrSettings::from_config("/unused", &config);
        if !AVAILABLE {
            assert!(
                result.is_err(),
                "a saved enable flag must not activate absent ASR"
            );
            continue;
        }
        let settings = result.unwrap();
        let selected = match settings.backend {
            subomi_app::controller::asr::AsrBackend::Whisper => raw::asr::BACKEND_WHISPER,
            subomi_app::controller::asr::AsrBackend::Onnx => raw::asr::BACKEND_ONNX,
        };
        assert!(subomi_app::controller::asr::backend_available(selected));
        if subomi_app::controller::asr::backend_available(saved) {
            assert_eq!(selected, saved);
        }
        // Falling back for this build must not overwrite the user's preference.
        assert_eq!(
            config.get_at(raw::asr::GROUP, raw::asr::BACKEND),
            Some(subomi_config::Value::Text(saved.into()))
        );
    }
}

#[cfg(feature = "asr-onnx")]
#[test]
fn downloaded_onnx_runtime_exposes_the_required_api() {
    use subomi_asr_onnx::ort_sys::OrtGetApiBase;
    unsafe {
        let base = OrtGetApiBase();
        assert!(!base.is_null());
        assert!(!((*base).GetApi)(22).is_null());
    }
}

#[cfg(feature = "asr-onnx")]
#[test]
fn onnx_asr_feature_links_and_initializes_without_env_overrides() {
    use subomi_app::controller::asr::{backend_available, AsrController, AVAILABLE};
    use subomi_asr_onnx::{
        engine::{Engine, Options},
        ort_sys::{OrtGetApiBase, OrtRunOptions},
    };
    use std::{ffi::CStr, path::Path, ptr};

    assert!(AVAILABLE);
    assert!(backend_available(raw::asr::BACKEND_ONNX));

    unsafe {
        let base = OrtGetApiBase();
        assert!(!base.is_null());
        let version_ptr = ((*base).GetVersionString)();
        assert!(!version_ptr.is_null());
        let version = CStr::from_ptr(version_ptr).to_str().unwrap();
        assert!(
            version.starts_with("1."),
            "unexpected ONNX Runtime version string: {version}"
        );

        let api = ((*base).GetApi)(22);
        assert!(!api.is_null());
        let mut run_options: *mut OrtRunOptions = ptr::null_mut();
        let status = ((*api).CreateRunOptions)(&mut run_options);
        assert!(status.is_null());
        assert!(!run_options.is_null());
        ((*api).ReleaseRunOptions)(run_options);
    }

    let engine = Engine::create(
        Path::new("/nonexistent/onnx-model-dir"),
        Options {
            use_gpu: false,
            gpu_device: 0,
            threads: 1,
            punctuation: true,
        },
    );
    assert!(!engine.is_loaded());
    assert!(!engine.load_error().is_empty());

    let mut controller = AsrController::new();
    assert!(!controller.is_active());
    assert!(!controller.is_running());
    controller.shutdown();
}

#[cfg(feature = "asr-whisper")]
#[test]
fn compiled_whisper_library_exposes_the_required_api() {
    unsafe {
        let version = subomi_asr_whisper::whisper_sys::whisper_version();
        assert!(!version.is_null());
        // The upstream v1.9.6 tag reports "1.9.6-dev" in whisper_version().
        assert_eq!(std::ffi::CStr::from_ptr(version).to_str().unwrap().split('-').next(), Some("1.9.6"));
        let _ = subomi_asr_whisper::whisper_sys::whisper_vad_default_context_params();
    }
}

#[cfg(feature = "asr-whisper")]
#[test]
fn whisper_asr_feature_links_and_initializes_without_env_overrides() {
    use subomi_app::controller::asr::{
        backend_available, whisper_gpu_device_count, AsrController, AVAILABLE,
    };

    assert!(AVAILABLE);
    assert!(backend_available(raw::asr::BACKEND_WHISPER));
    let _gpu_count = whisper_gpu_device_count();

    unsafe {
        let ctx_params = subomi_asr_whisper::whisper_sys::whisper_context_default_params();
        let full_params = subomi_asr_whisper::whisper_sys::whisper_full_default_params(
            subomi_asr_whisper::whisper_sys::whisper_sampling_strategy_WHISPER_SAMPLING_GREEDY,
        );
        let _ = ctx_params.use_gpu;
        assert!(full_params.n_threads >= 0);
    }

    let mut controller = AsrController::new();
    assert!(!controller.is_active());
    assert!(!controller.is_running());
    controller.shutdown();
}

#[cfg(feature = "ocr")]
#[test]
fn ocr_feature_links_and_initializes_cleanly() {
    use subomi_app::controller::ocr::{
        OcrController, OcrError, OcrFrame, OcrRegion, OcrSettings, OcrViewport,
    };

    let settings = OcrSettings::default();
    assert!(settings.enabled);
    assert!(!settings.model.is_empty());

    let frame = OcrFrame::from_rgba(8, 6, vec![0u8; 8 * 6 * 4]).expect("valid RGBA frame");
    assert_eq!(frame.width(), 8);
    assert_eq!(frame.height(), 6);

    assert_eq!(
        OcrFrame::from_rgba(0, 6, Vec::new()).unwrap_err(),
        OcrError::InvalidFrameDimensions
    );
    assert_eq!(
        OcrFrame::from_rgba(4, 4, vec![0u8; 16]).unwrap_err(),
        OcrError::InvalidFrameBuffer
    );

    let region = OcrRegion {
        x: 0.1,
        y: 0.1,
        width: 0.5,
        height: 0.5,
    };
    let viewport = OcrViewport {
        width: 1280.0,
        height: 720.0,
    };
    assert!(region.width > 0.0 && viewport.width > 0.0);

    if let Err(err) = OcrController::new() {
        assert!(
            matches!(err, OcrError::RuntimeUnavailable(_)),
            "unexpected OCR initialization error: {err}"
        );
    }
}

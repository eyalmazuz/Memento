//! Feature combinations must work with settings saved by a different build.
use memento_app::controller::asr::{AsrSettings, AVAILABLE};
use memento_config::{
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
            memento_app::controller::asr::AsrBackend::Whisper => raw::asr::BACKEND_WHISPER,
            memento_app::controller::asr::AsrBackend::Onnx => raw::asr::BACKEND_ONNX,
        };
        assert!(memento_app::controller::asr::backend_available(selected));
        if memento_app::controller::asr::backend_available(saved) {
            assert_eq!(selected, saved);
        }
        // Falling back for this build must not overwrite the user's preference.
        assert_eq!(
            config.get_at(raw::asr::GROUP, raw::asr::BACKEND),
            Some(memento_config::Value::Text(saved.into()))
        );
    }
}

#[cfg(feature = "asr-onnx")]
#[test]
fn downloaded_onnx_runtime_exposes_the_required_api() {
    use memento_asr_onnx::ort_sys::OrtGetApiBase;
    unsafe {
        let base = OrtGetApiBase();
        assert!(!base.is_null());
        assert!(!((*base).GetApi)(22).is_null());
    }
}

#[cfg(feature = "asr-whisper")]
#[test]
fn compiled_whisper_library_exposes_the_required_api() {
    unsafe {
        let version = memento_asr_whisper::whisper_sys::whisper_version();
        assert!(!version.is_null());
        // The upstream v1.9.4 tag reports "1.9.4-dev" in whisper_version().
        assert_eq!(std::ffi::CStr::from_ptr(version).to_str().unwrap().split('-').next(), Some("1.9.4"));
        let _ = memento_asr_whisper::whisper_sys::whisper_vad_default_context_params();
    }
}

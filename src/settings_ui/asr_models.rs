//! Local ASR model status listing, non-blocking downloads, and ASR session reset helpers.

use std::cell::{Cell, RefCell};
use std::io;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use subomi_asr::onnx::onnxcontroller;
use subomi_asr::whisper::whispercontroller;
use subomi_config::Value;
use subomi_net::download::Downloader;
use subomi_player::MpvInstance;
use subomi_setting::keys as raw;
use slint::{ComponentHandle, VecModel};

use crate::controller;
use crate::controller::config::ConfigController;
use crate::controller::player::{
    clear_primary_subtitle, refresh_subtitle_list, LivePlayerCoordinator,
};
use crate::controller::subtitles::SubtitleController;
use crate::ui::AsrModelStatusRow;
use crate::{SubomiApp, SettingsWindow, SubEntry};

use super::SettingsContext;

const WHISPER_MANAGED_MODELS: &[&str] = &[
    "tiny",
    "base",
    "small",
    "medium",
    "large-v3",
    "large-v3-turbo",
];

const ONNX_MANAGED_MODELS: &[&str] = &[
    onnxcontroller::PRESET_KITSUNE_T06B_JA,
    onnxcontroller::PRESET_KITSUNE_P03B_JA,
];

fn collect_asr_model_rows(config: &ConfigController, config_dir: &Path) -> Vec<AsrModelStatusRow> {
    let config_dir_str = config_dir.to_string_lossy();
    let backend = config.asr_backend_selected();
    let selected_whisper = config.whisper_selected_model();
    let selected_onnx = config.onnx_selected_model();
    let vad_enabled = config
        .effective_setting(raw::whisper::GROUP, raw::whisper::VAD_ENABLED)
        .is_some_and(|v| matches!(v, Value::Bool(true)));
    let vad_model = config
        .effective_setting(raw::whisper::GROUP, raw::whisper::VAD_MODEL)
        .and_then(|v| match v {
            Value::Text(t) => Some(t),
            _ => None,
        })
        .unwrap_or_else(|| whispercontroller::VAD_SILERO_6.to_owned());

    let mut rows = Vec::new();
    for &model in WHISPER_MANAGED_MODELS {
        let installed = whispercontroller::model_available(model, &config_dir_str);
        let path = whispercontroller::resolve_model_path(
            &whispercontroller::model_filename(model),
            &[whispercontroller::MODEL_SUFFIX],
            &config_dir_str,
        );
        let selected = backend == raw::asr::BACKEND_WHISPER && selected_whisper == model;
        rows.push(AsrModelStatusRow {
            backend: "whisper".into(),
            name: model.into(),
            path: path.into(),
            installed,
            selected,
            status_text: if installed {
                "Installed"
            } else {
                "Not installed"
            }
            .into(),
        });
    }

    if vad_enabled || !whispercontroller::vad_model_url(&vad_model).is_empty() {
        let vad_path = whispercontroller::resolve_model_path(
            &vad_model,
            &[whispercontroller::MODEL_SUFFIX],
            &config_dir_str,
        );
        let vad_installed = !vad_path.is_empty() && Path::new(&vad_path).exists();
        let vad_name = Path::new(&vad_model)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(whispercontroller::VAD_SILERO_6);
        if !whispercontroller::vad_model_url(vad_name).is_empty() {
            rows.push(AsrModelStatusRow {
                backend: "whisper-vad".into(),
                name: vad_name.into(),
                path: vad_path.into(),
                installed: vad_installed,
                selected: backend == raw::asr::BACKEND_WHISPER && vad_enabled,
                status_text: if vad_installed {
                    "Installed"
                } else {
                    "Not installed"
                }
                .into(),
            });
        }
    }

    for &model in ONNX_MANAGED_MODELS {
        let installed = onnxcontroller::model_available(model, &config_dir_str);
        let path = onnxcontroller::model_directory(model, &config_dir_str);
        let selected = backend == raw::asr::BACKEND_ONNX && selected_onnx == model;
        rows.push(AsrModelStatusRow {
            backend: "onnx".into(),
            name: model.into(),
            path: path.into(),
            installed,
            selected,
            status_text: if installed {
                "Installed"
            } else {
                "Not installed"
            }
            .into(),
        });
    }

    rows
}

pub(crate) fn refresh_asr_model_status(
    window: &SettingsWindow,
    config: &ConfigController,
    config_dir: &Path,
) {
    let dir_str = whispercontroller::models_directory(&config_dir.to_string_lossy());
    window.set_asr_models_dir(dir_str.into());
    let rows = collect_asr_model_rows(config, config_dir);
    window.set_asr_model_rows(Rc::new(VecModel::from(rows)).into());
}

pub(crate) fn clear_asr_for_settings(
    app: &SubomiApp,
    asr: &Rc<RefCell<controller::asr::AsrController>>,
    subtitles: &Rc<RefCell<SubtitleController>>,
    subtitle_model: &Rc<VecModel<SubEntry>>,
    restart_pending: &Rc<Cell<bool>>,
    mpv: Option<&Rc<RefCell<Option<MpvInstance>>>>,
    coordinator: Option<&Rc<RefCell<Option<LivePlayerCoordinator>>>>,
) {
    let selected = app.get_asr_track_selected() || app.get_asr_running();
    let restart = selected && app.get_asr_enabled();
    asr.borrow_mut().cancel();
    if let Some(coord_rc) = coordinator {
        if let Ok(mut coord_guard) = coord_rc.try_borrow_mut() {
            if let Some(coord) = coord_guard.as_mut() {
                let player_guard = mpv.and_then(|p| p.try_borrow().ok());
                let player_ref = player_guard.as_deref().and_then(Option::as_ref);
                coord.remove_asr_track(player_ref);
            }
        }
    }
    let mut subtitles = subtitles.borrow_mut();
    subtitles.clear_asr_track();
    refresh_subtitle_list(app, subtitle_model, &subtitles);
    app.set_asr_running(false);
    app.set_asr_track_selected(restart);
    restart_pending.set(restart);
    if selected {
        clear_primary_subtitle(app);
    }
}

pub(crate) fn wire(window: &SettingsWindow, ctx: &Rc<SettingsContext>) {
    let refresh_window = window.as_weak();
    let refresh_ctx = ctx.clone();
    window.on_refresh_asr_models(move || {
        if let Some(window) = refresh_window.upgrade() {
            refresh_asr_model_status(&window, &refresh_ctx.config, &refresh_ctx.cache_dir);
        }
    });

    let asr_download_cancel: Rc<RefCell<Option<Arc<AtomicBool>>>> = Rc::new(RefCell::new(None));
    let cancel_flag_ref = asr_download_cancel.clone();
    let cancel_window = window.as_weak();
    window.on_cancel_asr_download(move || {
        if let Some(flag) = cancel_flag_ref.borrow().as_ref() {
            flag.store(true, Ordering::Relaxed);
        }
        if let Some(window) = cancel_window.upgrade() {
            window.set_asr_download_text("Cancelling model download…".into());
        }
    });

    let finished_window = window.as_weak();
    let finished_ctx = ctx.clone();
    window.on_asr_download_finished(move |_ok, status_msg| {
        let Some(win) = finished_window.upgrade() else {
            return;
        };
        win.set_asr_download_active(false);
        win.set_asr_download_progress(0.0);
        win.set_asr_download_text("".into());
        win.set_asr_download_model("".into());
        refresh_asr_model_status(&win, &finished_ctx.config, &finished_ctx.cache_dir);
        win.set_settings_status(status_msg);
    });

    let dl_window = window.as_weak();
    let dl_cache = ctx.cache_dir.clone();
    let dl_cancel_holder = asr_download_cancel;
    window.on_download_asr_model(move |backend, model_name| {
        let Some(window) = dl_window.upgrade() else {
            return;
        };
        if window.get_asr_download_active() {
            return;
        }
        let backend = backend.to_string();
        let model_name = model_name.to_string();
        let config_dir_str = dl_cache.to_string_lossy().into_owned();

        let jobs: Vec<(String, PathBuf)> = match backend.as_str() {
            "whisper" => {
                if !whispercontroller::is_managed_model(&model_name) {
                    window.set_settings_status(
                        format!("Unknown Whisper model {model_name:?}").into(),
                    );
                    return;
                }
                let url = whispercontroller::whisper_model_url(&model_name);
                let target = PathBuf::from(whispercontroller::models_directory(&config_dir_str))
                    .join(whispercontroller::model_filename(&model_name));
                vec![(url, target)]
            }
            "whisper-vad" => {
                let url = whispercontroller::vad_model_url(&model_name);
                if url.is_empty() {
                    window.set_settings_status(
                        format!("No download URL for VAD model {model_name:?}").into(),
                    );
                    return;
                }
                let target = PathBuf::from(whispercontroller::models_directory(&config_dir_str))
                    .join(&model_name);
                vec![(url, target)]
            }
            "onnx" => {
                let specs = onnxcontroller::model_files(&model_name);
                if specs.is_empty() {
                    window.set_settings_status(
                        format!("Unknown ONNX model {model_name:?}").into(),
                    );
                    return;
                }
                let model_dir = PathBuf::from(onnxcontroller::model_directory(
                    &model_name,
                    &config_dir_str,
                ));
                specs
                    .into_iter()
                    .map(|spec| (spec.url, model_dir.join(spec.relative_path)))
                    .collect()
            }
            _ => return,
        };

        let cancel_flag = Arc::new(AtomicBool::new(false));
        *dl_cancel_holder.borrow_mut() = Some(cancel_flag.clone());
        window.set_asr_download_active(true);
        window.set_asr_download_model(model_name.clone().into());
        window.set_asr_download_progress(-1.0);
        window.set_asr_download_text(format!("Starting download for {model_name}…").into());

        let worker_window = dl_window.clone();
        std::thread::spawn(move || {
            let downloader = Downloader::default();
            let total_files = jobs.len();
            let mut outcome: Result<(), String> = Ok(());
            for (idx, (url, target)) in jobs.into_iter().enumerate() {
                let file_label = target
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("model")
                    .to_owned();
                let mut last_ui_update: Option<Instant> = None;
                let progress_window = worker_window.clone();
                let res = downloader.download(&url, &target, |event| {
                    if cancel_flag.load(Ordering::Relaxed) {
                        return Err(io::Error::new(
                            io::ErrorKind::Interrupted,
                            "Download cancelled",
                        ));
                    }
                    let should_update = last_ui_update
                        .is_none_or(|prev| prev.elapsed() >= Duration::from_millis(120));
                    if should_update {
                        last_ui_update = Some(Instant::now());
                        let total_bytes = event.progress.total;
                        let frac = if total_bytes > 0 {
                            ((idx as f64 + event.progress.fraction) / total_files as f64) as f32
                        } else {
                            -1.0
                        };
                        let text = if total_bytes > 0 {
                            let pct = (event.progress.fraction * 100.0).round() as i64;
                            let mb_s = event.progress.speed_bps as f64 / (1024.0 * 1024.0);
                            format!(
                                "Downloading {file_label} ({}/{total_files}): {pct}% ({mb_s:.1} MB/s)",
                                idx + 1
                            )
                        } else {
                            let mb = event.progress.received as f64 / (1024.0 * 1024.0);
                            format!(
                                "Downloading {file_label} ({}/{total_files}): {mb:.1} MB",
                                idx + 1
                            )
                        };
                        let _ = progress_window.upgrade_in_event_loop(move |win| {
                            win.set_asr_download_progress(frac);
                            win.set_asr_download_text(text.into());
                        });
                    }
                    Ok(())
                });
                if let Err(err) = res {
                    outcome = Err(if cancel_flag.load(Ordering::Relaxed) {
                        "ASR model download cancelled".to_owned()
                    } else {
                        err.to_string()
                    });
                    break;
                }
            }
            let (ok, status_msg) = match outcome {
                Ok(()) => (true, format!("Downloaded ASR model {model_name}")),
                Err(err) => (false, err),
            };
            let _ = worker_window.upgrade_in_event_loop(move |win| {
                win.invoke_asr_download_finished(ok, status_msg.into());
            });
        });
    });
}

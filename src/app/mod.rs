//! Main application orchestration module for pure-Rust Memento.

pub mod anki_actions;
pub mod clipboard;
pub mod keybinds;
pub mod paths;
pub mod playback;
pub mod preferences;
pub mod subtitle_drawer;
pub mod subtitle_io;
pub mod sync_loop;
pub mod updater;
pub mod video_pipeline;
pub mod window_chrome;
pub mod workers;

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use slint::{ComponentHandle, VecModel};

pub use clipboard::{copy_text_to_clipboard, read_copied_text_or_clipboard};
pub use preferences::{format_recent_file_label, record_recent_media_title};
pub use subtitle_drawer::{
    auto_scroll_subtitle_drawer_if_needed, refresh_secondary_subtitle_list, refresh_subtitle_list,
    scroll_drawer_to_row_accurate,
};

use crate::controller::asr::AsrController;
use crate::controller::ass::AssOverlayController;
use crate::controller::config::ConfigController;
use crate::controller::dict::DictController;
use crate::controller::dictmanager::DictionaryManager;
use crate::controller::player::LivePlayerCoordinator;
use crate::controller::subtitles::SubtitleController;
use crate::graphics_device;
use crate::platform_renderer::{
    PlatformRendererConfig, PlatformRendererKind, RendererOverrides, VideoRendererCapabilities,
};
use crate::settings_ui::SettingsSession;
use crate::ui::{self, MementoApp, SubEntry};
use memento_player::MpvInstance;
use memento_setting::keys as raw;

use self::anki_actions::{install_anki_and_audio_handlers, PopupAudioState};
use self::keybinds::install_keybind_handlers;
use self::paths::{format_time, memento_xdg_dir, migrate_xdg_config_if_needed};
use self::playback::{install_playback_handlers, ASR_SHUTDOWN_TIMEOUT};
use self::preferences::{
    apply_dictionary_order, install_settings_and_preference_handlers, refresh_player_preferences,
    refresh_recent_files, setting_enabled,
};
use self::subtitle_drawer::install_subtitle_selection;
use self::sync_loop::install_sync_loop;
use self::updater::{
    determine_startup_update_action, install_update_and_link_handlers, open_update_opt_in_dialog,
    spawn_update_check, StartupUpdateAction,
};
use self::video_pipeline::{install_video_pipeline, VideoTimings};
use self::window_chrome::install_window_chrome_handlers;
use self::workers::{install_dictionary_lookup_handlers, UiRelay, WorkerPool};

struct AppLogger;
impl log::Log for AppLogger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        !metadata.target().starts_with("icu_")
    }
    fn log(&self, record: &log::Record) {
        if self.enabled(record.metadata()) {
            eprintln!("[{}] {}", record.level(), record.args());
        }
    }
    fn flush(&self) {}
}
static APP_LOGGER: AppLogger = AppLogger;

/// Launch the Memento desktop application.
pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    // libmpv requires the C numeric locale (`.` decimal separator) before initialization.
    // SAFETY: invoked on the main thread before any worker threads or GUI/libmpv init.
    unsafe {
        libc::setlocale(libc::LC_NUMERIC, c"C".as_ptr());
    }
    let raw_args: Vec<String> = std::env::args().collect();
    let parsed_args = memento_player::parse_args(&raw_args)
        .map_err(|err| format!("invalid command-line arguments: {err:?}"))?;
    if parsed_args.show_help {
        print!("{}", memento_player::help_text());
        return Ok(());
    }
    if parsed_args.show_version {
        println!(
            "{}",
            memento_player::version_text(env!("CARGO_PKG_VERSION"))
        );
        return Ok(());
    }
    #[cfg(feature = "ocr")]
    crate::controller::ocr::prepare_python_executable();
    let _ = log::set_logger(&APP_LOGGER);
    log::set_max_level(log::LevelFilter::Warn);

    let config_dir = memento_xdg_dir("XDG_CONFIG_HOME", ".config")
        .ok_or("HOME, XDG_CONFIG_HOME, APPDATA, or USERPROFILE must be set to load settings")?;
    // Pre-select matching GPU offload env vars from CLI `--hwdec=...` / `mpv.conf`
    // BEFORE creating Slint's OpenGL context or initializing libmpv.
    graphics_device::select_for_config_dir(&config_dir);

    // Pass full `raw_args` INCLUDING `argv[0]` because `configured_hwdec_from_args`
    // and `configured_option_from_args` skip `argv[0]` internally.
    let renderer_overrides = RendererOverrides::from_config_dir_and_args(
        &config_dir,
        raw_args.iter().map(String::as_str),
    );
    let renderer_caps = VideoRendererCapabilities::detect_current();
    let renderer_plan = PlatformRendererConfig::select(&renderer_caps, &renderer_overrides);
    eprintln!(
        "Video renderer plan: {} ({})",
        renderer_plan.primary, renderer_plan.reason
    );

    // libmpv shares the UI's context. On desktop platforms prefer desktop GL
    // with the driver's supported version, rather than Slint's GLES 2 default.
    // An explicitly selected Slint backend (including software) still wins.
    if std::env::var_os("SLINT_BACKEND").is_none()
        && renderer_plan.primary != PlatformRendererKind::Software
    {
        slint::BackendSelector::new().require_opengl().select()?;
    }
    let _ = slint::set_xdg_app_id(memento_os::WAYLAND_APP_ID);
    let app = MementoApp::new()?;
    ui::prepare_text_fonts();

    if let Some(cache_dir) = memento_xdg_dir("XDG_CACHE_HOME", ".cache") {
        if let Err(error) = migrate_xdg_config_if_needed(&cache_dir, &config_dir, "memento.conf") {
            eprintln!(
                "Could not migrate player settings from {}: {error}",
                cache_dir.display()
            );
        }
    }
    let data_dir = memento_xdg_dir("XDG_DATA_HOME", ".local/share").ok_or(
        "HOME, XDG_DATA_HOME, LOCALAPPDATA, or USERPROFILE must be set to load dictionaries",
    )?;
    let anki_json_path = config_dir.join("anki_connect.json");
    let config = ConfigController::load(&config_dir, "memento.conf")
        .map_err(|error| format!("could not load player settings: {error}"))?;
    config.load_anki_profiles(&anki_json_path)?;
    let config_ctrl = Rc::new(config);

    let dictionary_database_path = data_dir.join("dictionaries.sqlite");
    let dictionary_resources_path = data_dir.join("res");
    let dictionary_manager =
        match DictionaryManager::new(&dictionary_database_path, &dictionary_resources_path) {
            Ok(manager) => Some(Rc::new(manager)),
            Err(error) => {
                eprintln!("Dictionary manager unavailable: {error}");
                None
            }
        };
    let sub_ctrl = Rc::new(RefCell::new(SubtitleController::new()));
    let mut dictionary = DictController::open(&dictionary_database_path).unwrap_or_else(|error| {
        eprintln!("Dictionary lookup unavailable: {error}");
        DictController::new()
    });
    apply_dictionary_order(&config_ctrl, &mut dictionary);
    let dict_ctrl = Rc::new(RefCell::new(dictionary));

    // mpv renders into a Slint image, never into the toplevel native window:
    // a native child would occlude the OSC, subtitles and dictionary popup.
    let (mpv, coordinator) =
        match MpvInstance::new_with_options(Some(&config_dir), &parsed_args.global_options) {
            Ok(instance) => {
                let coord = LivePlayerCoordinator::new(&instance);
                (
                    Rc::new(RefCell::new(Some(instance))),
                    Rc::new(RefCell::new(Some(coord))),
                )
            }
            Err(err) => {
                eprintln!("Could not initialize video playback: {err}");
                (Rc::new(RefCell::new(None)), Rc::new(RefCell::new(None)))
            }
        };
    let screensaver = Rc::new(RefCell::new(memento_os::Screensaver::new()));
    let video_timings = Rc::new(RefCell::new(VideoTimings::default()));
    let pending_playback_frame = Rc::new(Cell::new(None::<i32>));
    let current_render_size = Rc::new(Cell::new((0u32, 0u32)));
    let verbose_video_timings = std::env::var_os("MEMENTO_VIDEO_STATS").is_some();

    let start_player = mpv.clone();
    let start_app = app.as_weak();
    app.on_video_load_presented(move |generation| {
        let Some(app) = start_app.upgrade() else {
            return;
        };
        if app.get_pending_playback_generation() != generation {
            return;
        }
        if app.get_subtitle_furigana_pending() {
            return;
        }
        app.set_pending_playback_generation(-1);
        if let Some(player) = start_player.borrow().as_ref() {
            if let Err(error) = player.play() {
                app.set_status_message(error.into());
            }
        }
    });

    // Subtitle models & initial UI properties
    let sub_model = Rc::new(VecModel::<SubEntry>::default());
    app.set_subtitle_entries(sub_model.clone().into());
    let secondary_sub_model = Rc::new(VecModel::<SubEntry>::default());
    app.set_secondary_subtitle_entries(secondary_sub_model.clone().into());
    app.set_volume(100.0);
    app.set_volume_max(100.0);
    app.set_is_muted(false);
    app.set_current_time("00:00".into());
    app.set_duration_time("00:00".into());
    app.set_app_version(env!("CARGO_PKG_VERSION").into());
    app.on_format_timestamp(|sec| format_time(sec as f64).into());
    refresh_player_preferences(&app, &config_ctrl);
    refresh_recent_files(&app, &config_ctrl);

    #[cfg(feature = "ocr")]
    let ocr_controller = crate::controller::ocr::OcrController::new();
    #[cfg(feature = "ocr")]
    if let Err(error) = &ocr_controller {
        eprintln!("OCR unavailable: {error}");
    }
    #[cfg(feature = "ocr")]
    let ocr_available = ocr_controller.is_ok();
    #[cfg(not(feature = "ocr"))]
    let ocr_available = false;

    let asr_session = Rc::new(RefCell::new(AsrController::new()));
    let asr_restart_pending = Rc::new(Cell::new(false));
    let closing = Rc::new(Cell::new(false));
    app.set_ocr_enabled(
        ocr_available
            && setting_enabled(
                &config_ctrl,
                raw::ocr::GROUP,
                raw::ocr::ENABLED,
                raw::ocr::ENABLED_DEFAULT,
            ),
    );
    app.set_asr_enabled(
        crate::controller::asr::AVAILABLE
            && setting_enabled(
                &config_ctrl,
                raw::asr::GROUP,
                raw::asr::ENABLED,
                raw::asr::ENABLED_DEFAULT,
            ),
    );

    let settings_session: Rc<RefCell<Option<SettingsSession>>> = Rc::new(RefCell::new(None));
    let pref_saver = install_settings_and_preference_handlers(
        &app,
        config_ctrl.clone(),
        dict_ctrl.clone(),
        dictionary_manager,
        dictionary_database_path.clone(),
        config_dir.clone(),
        anki_json_path,
        ocr_available,
        asr_session.clone(),
        sub_ctrl.clone(),
        sub_model.clone(),
        asr_restart_pending.clone(),
        mpv.clone(),
        coordinator.clone(),
        settings_session.clone(),
    );

    let current_media: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
    let media_generation = Rc::new(Cell::new(0u64));
    let shared_media_generation = Arc::new(AtomicU64::new(0));
    let lookup_epoch = Arc::new(AtomicU64::new(0));
    let hover_cursor = Rc::new(RefCell::new(None::<(String, i32)>));
    let popup_audio_cache: Arc<Mutex<PopupAudioState>> =
        Arc::new(Mutex::new(PopupAudioState::default()));
    let ass_ctrl = Rc::new(RefCell::new(AssOverlayController::new()));
    let ui_relay = UiRelay::new();

    // Spawn all background worker threads (thumb, dict lookup, furigana, ext sub, MKV).
    let workers = WorkerPool::spawn(
        &app,
        dictionary_database_path.clone(),
        shared_media_generation.clone(),
        lookup_epoch.clone(),
        ui_relay.clone(),
    );

    // Wire keybinds, window chrome (OSC / pointer / fullscreen), drawer selection, updates.
    install_keybind_handlers(
        &app,
        mpv.clone(),
        closing.clone(),
        hover_cursor.clone(),
        config_dir.clone(),
    );
    let chrome_handles = install_window_chrome_handlers(
        &app,
        mpv.clone(),
        coordinator.clone(),
        current_render_size.clone(),
        &renderer_plan,
        &renderer_overrides,
    );
    install_subtitle_selection(&app, sub_ctrl.clone(), sub_model.clone());
    install_update_and_link_handlers(
        &app,
        config_ctrl.clone(),
        config_dir.clone(),
        settings_session,
    );

    let cli_keep_paused = parsed_args
        .global_options
        .iter()
        .any(|opt| opt.name == "pause" && matches!(opt.value.trim(), "yes" | "true" | "1"));
    let playback_handles = install_playback_handlers(
        &app,
        mpv.clone(),
        coordinator.clone(),
        sub_ctrl.clone(),
        sub_model.clone(),
        secondary_sub_model.clone(),
        config_ctrl.clone(),
        config_dir.clone(),
        current_media.clone(),
        media_generation.clone(),
        shared_media_generation.clone(),
        asr_session.clone(),
        asr_restart_pending.clone(),
        ass_ctrl.clone(),
        screensaver.clone(),
        closing.clone(),
        workers.thumb_tx.clone(),
        ui_relay.clone(),
        parsed_args.loads,
        cli_keep_paused,
        #[cfg(feature = "ocr")]
        ocr_controller,
    );

    let hover_timer = install_dictionary_lookup_handlers(
        &app,
        config_ctrl.clone(),
        dict_ctrl,
        popup_audio_cache.clone(),
        workers.lookup_tx.clone(),
        lookup_epoch.clone(),
        hover_cursor,
    );

    ui::install_popup_navigation(&app);
    ui::install_rich_ui_bridges(&app, &dictionary_database_path, &config_dir, &config_ctrl);
    ui::bind_rich_bridge_lookup_epoch(&app, lookup_epoch.clone());
    ui::bind_rich_bridge_ass_controller(&app, ass_ctrl.clone());

    install_anki_and_audio_handlers(
        &app,
        config_ctrl.clone(),
        popup_audio_cache,
        sub_ctrl.clone(),
        mpv.clone(),
        dictionary_database_path,
        dictionary_resources_path,
        config_dir,
        lookup_epoch.clone(),
    );

    let sync_handles = install_sync_loop(
        &app,
        mpv.clone(),
        coordinator,
        sub_ctrl,
        sub_model,
        secondary_sub_model,
        media_generation,
        shared_media_generation,
        current_media,
        lookup_epoch,
        asr_session.clone(),
        config_ctrl.clone(),
        asr_restart_pending,
        closing,
        playback_handles.hover_pause_owned,
        workers.furigana_tx.clone(),
        workers.furigana_epoch.clone(),
        video_timings.clone(),
        pending_playback_frame.clone(),
        workers.ext_sub_tx.clone(),
        workers.mkv_tx.clone(),
        ass_ctrl,
        screensaver.clone(),
        ui_relay,
        playback_handles.handle_open_or_drop_paths,
        verbose_video_timings,
    );

    let render_worker = install_video_pipeline(
        &app,
        mpv.clone(),
        video_timings,
        pending_playback_frame,
        current_render_size,
        &renderer_plan,
        &renderer_overrides,
        playback_handles.trigger_initial_loads,
        verbose_video_timings,
    );

    match determine_startup_update_action(&config_ctrl) {
        StartupUpdateAction::ShowOptInPrompt => open_update_opt_in_dialog(&app),
        StartupUpdateAction::RunSilentCheck => spawn_update_check(app.as_weak(), true),
        StartupUpdateAction::Nothing => {}
    }

    let run_result = app.run();

    // ── Deterministic, Bounded Shutdown ─────────────────────────────────────
    sync_handles.housekeeping_timer.stop();
    playback_handles.quit_timer.stop();
    playback_handles.hover_resume_timer.stop();
    chrome_handles.osc_timer.stop();
    hover_timer.stop();
    pref_saver.flush();
    screensaver.borrow_mut().uninhibit();
    drop(sync_handles._dispatch_guard);

    // Close all background worker channels and join worker threads cleanly.
    workers.shutdown();

    // Bounded ASR teardown covering event-loop exits that bypassed `on_quit_app`.
    let mut asr_still_hung = playback_handles.asr_shutdown_timed_out.get();
    if !asr_still_hung {
        let mut asr = asr_session.borrow_mut();
        asr.begin_shutdown();
        let deadline = Instant::now() + ASR_SHUTDOWN_TIMEOUT;
        while !asr.try_finish_shutdown() {
            if Instant::now() >= deadline {
                eprintln!(
                    "ASR shutdown timed out after {}s during exit cleanup",
                    ASR_SHUTDOWN_TIMEOUT.as_secs()
                );
                asr_still_hung = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(15));
        }
    }

    // Join software render thread before `mpv` can drop.
    drop(render_worker);
    // Release libmecab / piped MeCab daemon cleanly on shutdown.
    memento_util::mecab::unload_mecab();

    if asr_still_hung {
        // Avoid hanging forever inside `AsrController::drop` -> `worker.join()` on a stuck native call.
        // Drop the Slint UI and mpv cleanly first, then forget the hung ASR controller.
        drop(mpv);
        std::mem::forget(asr_session);
    }

    run_result?;
    Ok(())
}

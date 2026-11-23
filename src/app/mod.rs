//! Main application orchestration module for pure-Rust Subomi.

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
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use slint::winit_030::{winit, CustomApplicationHandler, EventResult};
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
use crate::ui::{self, SubomiApp, SubEntry};
use subomi_player::MpvInstance;
use subomi_setting::keys as raw;

use self::anki_actions::{install_anki_and_audio_handlers, PopupAudioState};
use self::keybinds::{install_keybind_handlers, AudioContextMarkState};
use self::paths::{format_time, subomi_xdg_dir, migrate_xdg_config_if_needed};
use self::playback::{
    install_playback_handlers, persist_window_panel_state, restore_window_panel_state,
    ASR_SHUTDOWN_TIMEOUT,
};
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
use self::video_pipeline::{install_video_pipeline, VideoParamsObserver, VideoTimings};
use self::window_chrome::install_window_chrome_handlers;
use self::workers::{install_dictionary_lookup_handlers, UiEvent, UiRelay, WorkerPool};

/// Terminate the process without running C/C++ `atexit` static destructors.
///
/// Used only as a last resort after `ASR_SHUTDOWN_TIMEOUT` when a native ASR
/// worker thread is still running inside a GPU backend (e.g. `ggml-vulkan`):
/// returning normally from `main()` would invoke C++ static destructors such as
/// `vk_instance_t` -> `~vk_device_struct()` (`vkDestroyDevice`) while that
/// thread still holds the Vulkan device, deadlocking or crashing on exit.
fn terminate_process_bypassing_atexit(code: i32) -> ! {
    #[cfg(unix)]
    unsafe {
        libc::_exit(code);
    }
    #[cfg(not(unix))]
    std::process::exit(code);
}

/// Format the About-modal / menu version string from `CARGO_PKG_VERSION` + optional `SUBOMI_GIT_HASH` (H1.6).
pub(crate) fn format_app_version_display(pkg_version: &str, git_hash: Option<&str>) -> String {
    match git_hash.map(str::trim).filter(|hash| !hash.is_empty()) {
        Some(hash) => format!("{pkg_version}-{hash}"),
        None => pkg_version.to_owned(),
    }
}

#[derive(Default)]
struct ExternalOpenBridge {
    pending: Vec<PathBuf>,
    relay: Option<UiRelay>,
}

static EXTERNAL_OPEN_BRIDGE: OnceLock<Mutex<ExternalOpenBridge>> = OnceLock::new();

fn external_open_bridge() -> &'static Mutex<ExternalOpenBridge> {
    EXTERNAL_OPEN_BRIDGE.get_or_init(|| Mutex::new(ExternalOpenBridge::default()))
}

/// Enqueue file paths delivered externally by the OS (e.g. macOS Finder "Open With"
/// or Dock icon drop, H1.2) for dispatch into the main UI's `files_dropped` handler.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn enqueue_external_open_paths(paths: Vec<PathBuf>) {
    if paths.is_empty() {
        return;
    }
    let mut bridge = external_open_bridge()
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    if let Some(relay) = bridge.relay.as_ref() {
        relay.send_event(UiEvent::OpenPaths(paths));
    } else {
        for path in paths {
            if !bridge.pending.contains(&path) {
                bridge.pending.push(path);
            }
        }
    }
}

fn attach_external_open_relay(relay: UiRelay) {
    let drained = {
        let mut bridge = external_open_bridge()
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        bridge.relay = Some(relay.clone());
        std::mem::take(&mut bridge.pending)
    };
    if !drained.is_empty() {
        relay.send_event(UiEvent::OpenPaths(drained));
    }
}

fn detach_external_open_relay() {
    let mut bridge = external_open_bridge()
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    bridge.relay = None;
}

/// On macOS, hook `application:openURLs:` and `application:openFile:` on winit's
/// `NSApplicationDelegate` so Finder "Open With" and Dock drops route cleanly into
/// Subomi's `files_dropped` pipeline without extra Objective-C crates (H1.2).
#[cfg(target_os = "macos")]
fn install_macos_open_urls_hook() {
    use std::ffi::{c_char, c_void, CStr};
    use std::sync::Once;

    static HOOK_ONCE: Once = Once::new();
    HOOK_ONCE.call_once(|| {
        type Id = *mut c_void;
        type Sel = *mut c_void;
        type Class = *mut c_void;

        #[link(name = "objc")]
        extern "C" {
            fn objc_getClass(name: *const c_char) -> Class;
            fn object_getClass(obj: Id) -> Class;
            fn sel_registerName(name: *const c_char) -> Sel;
            fn class_addMethod(cls: Class, sel: Sel, imp: *const c_void, types: *const c_char) -> bool;
            fn class_replaceMethod(
                cls: Class,
                sel: Sel,
                imp: *const c_void,
                types: *const c_char,
            ) -> *const c_void;
            fn objc_msgSend();
        }

        unsafe fn msg_id_0(obj: Id, sel: Sel) -> Id {
            let f: unsafe extern "C" fn(Id, Sel) -> Id = std::mem::transmute(objc_msgSend as *const ());
            f(obj, sel)
        }
        unsafe fn msg_usize_0(obj: Id, sel: Sel) -> usize {
            let f: unsafe extern "C" fn(Id, Sel) -> usize = std::mem::transmute(objc_msgSend as *const ());
            f(obj, sel)
        }
        unsafe fn msg_bool_0(obj: Id, sel: Sel) -> bool {
            let f: unsafe extern "C" fn(Id, Sel) -> bool = std::mem::transmute(objc_msgSend as *const ());
            f(obj, sel)
        }
        unsafe fn msg_id_1_usize(obj: Id, sel: Sel, idx: usize) -> Id {
            let f: unsafe extern "C" fn(Id, Sel, usize) -> Id = std::mem::transmute(objc_msgSend as *const ());
            f(obj, sel, idx)
        }
        unsafe fn msg_cstr_0(obj: Id, sel: Sel) -> *const c_char {
            let f: unsafe extern "C" fn(Id, Sel) -> *const c_char = std::mem::transmute(objc_msgSend as *const ());
            f(obj, sel)
        }

        unsafe extern "C" fn handle_open_urls(_self: Id, _cmd: Sel, _app: Id, urls: Id) {
            if urls.is_null() {
                return;
            }
            let sel_count = sel_registerName(c"count".as_ptr());
            let sel_obj_at = sel_registerName(c"objectAtIndex:".as_ptr());
            let sel_is_file_url = sel_registerName(c"isFileURL".as_ptr());
            let sel_path = sel_registerName(c"path".as_ptr());
            let sel_utf8 = sel_registerName(c"UTF8String".as_ptr());
            let count = msg_usize_0(urls, sel_count);
            let mut paths = Vec::with_capacity(count);
            for idx in 0..count {
                let url = msg_id_1_usize(urls, sel_obj_at, idx);
                if url.is_null() || !msg_bool_0(url, sel_is_file_url) {
                    continue;
                }
                let ns_path = msg_id_0(url, sel_path);
                if ns_path.is_null() {
                    continue;
                }
                let utf8 = msg_cstr_0(ns_path, sel_utf8);
                if !utf8.is_null() {
                    let text = CStr::from_ptr(utf8).to_string_lossy();
                    if !text.is_empty() {
                        paths.push(PathBuf::from(text.into_owned()));
                    }
                }
            }
            enqueue_external_open_paths(paths);
        }

        unsafe extern "C" fn handle_open_file(_self: Id, _cmd: Sel, _app: Id, filename: Id) -> bool {
            if filename.is_null() {
                return false;
            }
            let sel_utf8 = sel_registerName(c"UTF8String".as_ptr());
            let utf8 = msg_cstr_0(filename, sel_utf8);
            if utf8.is_null() {
                return false;
            }
            let text = CStr::from_ptr(utf8).to_string_lossy();
            if text.is_empty() {
                return false;
            }
            enqueue_external_open_paths(vec![PathBuf::from(text.into_owned())]);
            true
        }

        unsafe {
            let ns_app_cls = objc_getClass(c"NSApplication".as_ptr());
            if ns_app_cls.is_null() {
                return;
            }
            let ns_app = msg_id_0(ns_app_cls, sel_registerName(c"sharedApplication".as_ptr()));
            if ns_app.is_null() {
                return;
            }
            let delegate = msg_id_0(ns_app, sel_registerName(c"delegate".as_ptr()));
            if delegate.is_null() {
                return;
            }
            let delegate_cls = object_getClass(delegate);
            if delegate_cls.is_null() {
                return;
            }
            let open_urls_sel = sel_registerName(c"application:openURLs:".as_ptr());
            if !class_addMethod(
                delegate_cls,
                open_urls_sel,
                handle_open_urls as *const c_void,
                c"v@:@@".as_ptr(),
            ) {
                class_replaceMethod(
                    delegate_cls,
                    open_urls_sel,
                    handle_open_urls as *const c_void,
                    c"v@:@@".as_ptr(),
                );
            }
            let open_file_sel = sel_registerName(c"application:openFile:".as_ptr());
            if !class_addMethod(
                delegate_cls,
                open_file_sel,
                handle_open_file as *const c_void,
                c"B@:@@".as_ptr(),
            ) {
                class_replaceMethod(
                    delegate_cls,
                    open_file_sel,
                    handle_open_file as *const c_void,
                    c"B@:@@".as_ptr(),
                );
            }
        }
    });
}

struct SubomiWinitAppHandler;

impl CustomApplicationHandler for SubomiWinitAppHandler {
    fn resumed(&mut self, _event_loop: &winit::event_loop::ActiveEventLoop) -> EventResult {
        #[cfg(target_os = "macos")]
        install_macos_open_urls_hook();
        EventResult::Propagate
    }
}

/// Return freed glibc heap pages back to the Linux kernel after dropping large
/// allocations (media decoder buffers, embedded fonts, ASR models, etc.).
#[inline]
pub(crate) fn trim_os_heap() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    unsafe {
        let _ = libc::malloc_trim(0);
    }
}

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

/// Launch the Subomi desktop application.
pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    // libmpv requires the C numeric locale (`.` decimal separator) before initialization.
    // SAFETY: invoked on the main thread before any worker threads or GUI/libmpv init.
    unsafe {
        libc::setlocale(libc::LC_NUMERIC, c"C".as_ptr());
    }
    let raw_args: Vec<String> = std::env::args().collect();
    let parsed_args = subomi_player::parse_args(&raw_args)
        .map_err(|err| format!("invalid command-line arguments: {err:?}"))?;
    if parsed_args.show_help {
        print!("{}", subomi_player::help_text());
        return Ok(());
    }
    if parsed_args.show_version {
        println!(
            "{}",
            subomi_player::version_text(env!("CARGO_PKG_VERSION"))
        );
        return Ok(());
    }
    // H1.4: Augment macOS `.app` bundle `PATH` (Homebrew / MacPorts / local tools)
    // BEFORE OCR/Python probing or `MpvInstance` creation.
    subomi_os::augment_macos_bundle_path();
    #[cfg(feature = "ocr")]
    crate::controller::ocr::prepare_python_executable();
    let _ = log::set_logger(&APP_LOGGER);
    log::set_max_level(log::LevelFilter::Warn);

    let config_dir = subomi_xdg_dir("XDG_CONFIG_HOME", ".config")
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
    // with the driver's supported version, rather than Slint's GLES 2 default,
    // and install the custom winit application handler for macOS Finder/Dock drops (H1.2).
    // An explicitly selected Slint backend (including software) still wins.
    if std::env::var_os("SLINT_BACKEND").is_none() {
        let mut selector = slint::BackendSelector::new()
            .with_winit_custom_application_handler(SubomiWinitAppHandler);
        if renderer_plan.primary != PlatformRendererKind::Software {
            selector = selector.require_opengl();
        }
        selector.select()?;
    }
    let _ = slint::set_xdg_app_id(subomi_os::WAYLAND_APP_ID);
    let app = SubomiApp::new()?;
    ui::prepare_text_fonts();

    if let Some(cache_dir) = subomi_xdg_dir("XDG_CACHE_HOME", ".cache") {
        if let Err(error) = migrate_xdg_config_if_needed(&cache_dir, &config_dir, "subomi.conf") {
            eprintln!(
                "Could not migrate player settings from {}: {error}",
                cache_dir.display()
            );
        }
    }
    let data_dir = subomi_xdg_dir("XDG_DATA_HOME", ".local/share").ok_or(
        "HOME, XDG_DATA_HOME, LOCALAPPDATA, or USERPROFILE must be set to load dictionaries",
    )?;
    let anki_json_path = config_dir.join("anki_connect.json");
    let config = ConfigController::load(&config_dir, "subomi.conf")
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
    // H1.1: Propagate `renderer_plan.effective_hwdec()` (e.g. `videotoolbox-copy` on macOS).
    let mut mpv_options = parsed_args.global_options.clone();
    if !renderer_plan.effective_hwdec().is_empty() {
        mpv_options.push(subomi_player::CliOption {
            name: "hwdec".into(),
            value: renderer_plan.effective_hwdec().to_owned(),
        });
    }
    let (mpv, coordinator) = match MpvInstance::new_with_options(Some(&config_dir), &mpv_options) {
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
    let screensaver = Rc::new(RefCell::new(subomi_os::Screensaver::new()));
    let video_timings = Rc::new(RefCell::new(VideoTimings::default()));
    let pending_playback_frame = Rc::new(Cell::new(None::<i32>));
    let current_render_size = Rc::new(Cell::new((0u32, 0u32)));
    let default_prefer_16f = !renderer_overrides.force_rgba8 && renderer_overrides.prefer_hdr;
    let gl_prefer_16f = Rc::new(Cell::new(default_prefer_16f));
    // Declared AFTER `mpv` (and never captured in `app.on_*` callbacks) so its weak
    // libmpv client is always destroyed before `MpvInstance::drop`.
    let video_params_observer = Rc::new(RefCell::new(
        if !renderer_overrides.force_rgba8
            && !renderer_overrides.prefer_hdr
            && renderer_plan.primary != PlatformRendererKind::Software
        {
            mpv.borrow()
                .as_ref()
                .map_or_else(VideoParamsObserver::disabled, VideoParamsObserver::attach)
        } else {
            VideoParamsObserver::disabled()
        },
    ));
    let verbose_video_timings = std::env::var_os("SUBOMI_VIDEO_STATS").is_some();

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
    app.set_app_version_display(
        format_app_version_display(env!("CARGO_PKG_VERSION"), option_env!("SUBOMI_GIT_HASH"))
            .into(),
    );
    #[cfg(target_os = "macos")]
    app.set_key_quit(crate::controller::config::QUIT_SHORTCUT_MACOS.into());
    app.on_format_timestamp(|sec| format_time(sec as f64).into());
    refresh_player_preferences(&app, &config_ctrl);
    restore_window_panel_state(&app, &config_ctrl);
    refresh_recent_files(&app, &config_ctrl);

    #[cfg(feature = "ocr")]
    let ocr_controller =
        crate::controller::ocr::OcrController::new().map_err(|error| error.to_string());
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
    let playback_dict_manager = dictionary_manager.as_deref().cloned();
    let has_zero_dictionaries = dictionary_manager
        .as_ref()
        .and_then(|manager| manager.list_dictionaries().ok())
        .is_some_and(|dicts| dicts.is_empty());
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

    // LOW: Startup "No Dictionaries Installed" prompt & direct navigation to Dictionaries tab.
    {
        let dismiss_app = app.as_weak();
        app.on_dismiss_no_dicts_dialog(move || {
            if let Some(app) = dismiss_app.upgrade() {
                app.set_no_dicts_dialog_open(false);
            }
        });
        let open_dict_app = app.as_weak();
        let open_dict_session = settings_session.clone();
        app.on_open_dict_settings(move || {
            let Some(app) = open_dict_app.upgrade() else {
                return;
            };
            app.set_no_dicts_dialog_open(false);
            let switched_before_open = if let Ok(guard) = open_dict_session.try_borrow() {
                if let Some(session) = guard.as_ref() {
                    let was_different = session.window.get_active_category() != 4;
                    session.window.set_active_category(4);
                    was_different
                } else {
                    false
                }
            } else {
                false
            };
            app.invoke_open_options();
            if let Ok(guard) = open_dict_session.try_borrow() {
                if let Some(session) = guard.as_ref() {
                    session.set_visible(true);
                    if switched_before_open || session.window.get_active_category() != 4 {
                        session.window.set_active_category(4);
                        session.window.invoke_category_changed(4);
                    }
                }
            }
        });
        if has_zero_dictionaries {
            app.set_no_dicts_dialog_open(true);
        }
    }

    let current_media: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
    let media_generation = Rc::new(Cell::new(0u64));
    let shared_media_generation = Arc::new(AtomicU64::new(0));
    let lookup_epoch = Arc::new(AtomicU64::new(0));
    let hover_cursor = Rc::new(RefCell::new(None::<(String, i32)>));
    let audio_context_marks = Rc::new(RefCell::new(AudioContextMarkState::default()));
    let popup_audio_cache: Arc<Mutex<PopupAudioState>> =
        Arc::new(Mutex::new(PopupAudioState::default()));
    let ass_ctrl = Rc::new(RefCell::new(AssOverlayController::new()));
    let ui_relay = UiRelay::new();

    // H2 & #331: Live OS color-scheme watcher (Linux xdg-desktop-portal / Windows registry).
    {
        let theme_relay = ui_relay.clone();
        subomi_os::spawn_color_scheme_watcher(move |scheme| {
            theme_relay.send_event(UiEvent::SystemThemeChanged(scheme));
        });
    }

    // Spawn all background worker threads (thumb, dict lookup, furigana, ext sub, MKV).
    let workers = WorkerPool::spawn(
        &app,
        dictionary_database_path.clone(),
        shared_media_generation.clone(),
        lookup_epoch.clone(),
        ui_relay.clone(),
    );

    // Wire keybinds, window chrome (OSC / pointer / fullscreen / drag-drop / icon), drawer, updates.
    install_keybind_handlers(
        &app,
        config_ctrl.clone(),
        mpv.clone(),
        closing.clone(),
        hover_cursor.clone(),
        audio_context_marks.clone(),
    );
    let chrome_handles = install_window_chrome_handlers(
        &app,
        config_ctrl.clone(),
        mpv.clone(),
        coordinator.clone(),
        current_render_size.clone(),
        &renderer_plan,
        &renderer_overrides,
    );
    if mpv.borrow().as_ref().is_some_and(MpvInstance::is_fullscreen) {
        app.invoke_toggle_fullscreen();
    }
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
        audio_context_marks,
        playback_dict_manager,
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
        config_dir.clone(),
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
        config_dir.clone(),
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
        ui_relay.clone(),
        playback_handles.handle_open_or_drop_paths,
        playback_handles.handle_add_subtitle_paths,
        gl_prefer_16f.clone(),
        default_prefer_16f,
        video_params_observer.clone(),
        verbose_video_timings,
    );
    attach_external_open_relay(ui_relay);

    let render_worker = install_video_pipeline(
        &app,
        mpv.clone(),
        video_timings,
        pending_playback_frame,
        current_render_size,
        &renderer_plan,
        gl_prefer_16f,
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
    detach_external_open_relay();
    persist_window_panel_state(&app, &config_ctrl, &config_dir);
    sync_handles.housekeeping_timer.stop();
    playback_handles.hover_resume_timer.stop();
    chrome_handles.osc_timer.stop();
    chrome_handles.cursor_timer.stop();
    chrome_handles.drop_timer.stop();
    hover_timer.stop();
    pref_saver.flush();
    screensaver.borrow_mut().uninhibit();
    drop(sync_handles._dispatch_guard);
    video_params_observer.borrow_mut().close();

    // Close all background worker channels and join worker threads cleanly.
    workers.shutdown();

    // Join software render thread before `mpv` can drop.
    drop(render_worker);
    // Release libmecab / piped MeCab daemon cleanly on shutdown.
    subomi_util::mecab::unload_mecab();

    // Bounded ASR teardown after the window is already hidden so closing feels
    // instant while giving an active inference step time to abort and release
    // native GPU buffers cleanly.
    let mut asr_still_hung = false;
    {
        let mut asr = asr_session.borrow_mut();
        asr.begin_shutdown();
        let deadline = Instant::now() + ASR_SHUTDOWN_TIMEOUT;
        while !asr.try_finish_shutdown() {
            if Instant::now() >= deadline {
                eprintln!(
                    "ASR shutdown timed out after {}s; forcing process exit",
                    ASR_SHUTDOWN_TIMEOUT.as_secs()
                );
                asr_still_hung = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(15));
        }
    }

    if asr_still_hung {
        // Leak `asr_session` first so dropping `app` never blocks forever in
        // `AsrController::drop -> worker.join()`, then drop `app` (tearing down
        // OpenGL and finalizing the OCR worker) and `mpv` (saving watch-later
        // resume state) before bypassing C++ `atexit` static destructors.
        std::mem::forget(asr_session);
        drop(app);
        if let Some(player) = mpv.borrow_mut().take() {
            drop(player);
        }
        let _ = std::io::Write::flush(&mut std::io::stdout());
        let _ = std::io::Write::flush(&mut std::io::stderr());
        terminate_process_bypassing_atexit(if run_result.is_ok() { 0 } else { 1 });
    }

    run_result?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_version_display_appends_git_hash_when_present() {
        assert_eq!(
            format_app_version_display("2.0.2", Some("abcdef1")),
            "2.0.2-abcdef1"
        );
        assert_eq!(format_app_version_display("2.0.2", Some("   ")), "2.0.2");
        assert_eq!(format_app_version_display("2.0.2", None), "2.0.2");
    }

    #[test]
    fn external_open_paths_buffer_until_relay_attached() {
        let relay = UiRelay::new();
        let path = PathBuf::from("/tmp/finder-opened.mkv");
        detach_external_open_relay();
        enqueue_external_open_paths(vec![path.clone()]);
        attach_external_open_relay(relay.clone());
        let events = relay.drain_events();
        detach_external_open_relay();
        assert!(events.iter().any(|ev| matches!(
            ev,
            UiEvent::OpenPaths(paths) if paths.contains(&path)
        )));
    }
}

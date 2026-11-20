//! Media loading, playlist/file-drop handling, transport controls, track selection,
//! OCR region capture, timeline hover thumbnails, and bounded graceful quit handling.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use slint::{ComponentHandle, Model as _, VecModel};

use crate::app::clipboard::{copy_text_to_clipboard, read_copied_text_or_clipboard};
use crate::app::keybinds::AudioContextMarkState;
use crate::app::paths::{memento_xdg_dir, resolve_config_file_open_dir};
use crate::app::preferences::{
    clear_recent_titles, record_recent_media, refresh_recent_files, setting_enabled,
};
use crate::app::subtitle_drawer::{refresh_secondary_subtitle_list, refresh_subtitle_list};
use crate::app::subtitle_io::parse_external_subtitles;
use crate::app::window_chrome::chapter_title_at_time;
use crate::app::workers::{ChannelSlot, ThumbRequest, UiEvent, UiRelay};
use crate::controller::asr::{AsrController, AsrSettings};
use crate::controller::ass::AssOverlayController;
use crate::controller::config::ConfigController;
use crate::controller::dictmanager::DictionaryManager;
use crate::controller::player::{
    clear_primary_subtitle, effective_media_title, is_subtitle_file, LivePlayerCoordinator,
};
use crate::controller::subtitles::SubtitleController;
use crate::ui::{self, MementoApp, SubEntry};
use memento_player::{MpvInstance, PlaylistLoad};
use memento_setting::keys as raw;

pub(crate) const ASR_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(3);

/// Complete set of subtitle extensions recognized for drag-and-drop and CLI loads (C5.3).
const SUPPORTED_SUBTITLE_EXTENSIONS: &[&str] = &[
    "ass", "ssa", "srt", "vtt", "sub", "idx", "sup", "lrc", "mks", "ttml", "smi", "rt",
];

pub(crate) fn is_supported_subtitle_path(path: &Path) -> bool {
    if is_subtitle_file(path) {
        return true;
    }
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| {
            SUPPORTED_SUBTITLE_EXTENSIONS
                .iter()
                .any(|known| ext.eq_ignore_ascii_case(known))
        })
}

pub(crate) fn is_supported_subtitle_str(path: &str) -> bool {
    is_supported_subtitle_path(Path::new(path))
}

pub(crate) fn is_dictionary_archive_path(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("zip"))
}

/// Persist `main-window/subtitle-list` and `main-window/search` open state across launches (M3).
pub(crate) fn persist_window_panel_state(
    app: &MementoApp,
    config_ctrl: &ConfigController,
    config_dir: &Path,
) {
    let sub_list_key = format!("{}/{}", raw::window::GROUP, raw::window::SUBTITLE_LIST);
    let search_key = format!("{}/{}", raw::window::GROUP, raw::window::SEARCH);
    let _ = config_ctrl.persist_single_setting(
        &sub_list_key,
        if app.get_sub_drawer_open() {
            "true"
        } else {
            "false"
        },
        config_dir,
        "memento.conf",
    );
    let _ = config_ctrl.persist_single_setting(
        &search_key,
        if app.get_dict_search_open() {
            "true"
        } else {
            "false"
        },
        config_dir,
        "memento.conf",
    );
}

/// Restore `main-window/subtitle-list` and `main-window/search` open state on startup (M3).
pub(crate) fn restore_window_panel_state(app: &MementoApp, config_ctrl: &ConfigController) {
    let sub_list_open = setting_enabled(
        config_ctrl,
        raw::window::GROUP,
        raw::window::SUBTITLE_LIST,
        false,
    );
    let search_open = setting_enabled(config_ctrl, raw::window::GROUP, raw::window::SEARCH, false);
    app.set_sub_drawer_open(sub_list_open);
    app.set_dict_search_open(search_open);
}

fn should_resume_on_subtitle_unhover(config_ctrl: &ConfigController) -> bool {
    setting_enabled(
        config_ctrl,
        raw::search::GROUP,
        raw::search::HOVER_PAUSE_RESUME,
        raw::search::HOVER_PAUSE_RESUME_DEFAULT,
    )
}

/// Unconditionally load picked subtitle paths via `mpv` `sub-add`, NEVER falling
/// back to `loadfile` regardless of filename extension (C5.3).
fn add_subtitle_paths_unconditionally(
    app: &MementoApp,
    player: Option<&MpvInstance>,
    sub_ctrl: &RefCell<SubtitleController>,
    sub_model: &Rc<VecModel<SubEntry>>,
    paths: &[PathBuf],
) {
    for sub_path in paths {
        let sub_str = sub_path.to_string_lossy().into_owned();
        if let Some(player) = player {
            if let Err(error) = player.command(&["sub-add", &sub_str]) {
                app.set_status_message(format!("Could not add subtitles: {error}").into());
            }
            continue;
        }
        if sub_path.is_file() {
            match parse_external_subtitles(sub_path) {
                Ok(cues) => {
                    let mut subtitles = sub_ctrl.borrow_mut();
                    let sid = if subtitles.primary_sid() > 0 {
                        subtitles.primary_sid()
                    } else {
                        subtitles.set_primary_sid(1);
                        app.set_primary_sid(1);
                        1
                    };
                    subtitles.set_external_cues(sid, &sub_str, cues);
                    refresh_subtitle_list(app, sub_model, &subtitles);
                }
                Err(error) => {
                    app.set_status_message(format!("Could not add subtitles: {error}").into());
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn load_playlist_into(
    app: &MementoApp,
    player: &MpvInstance,
    loads: &[PlaylistLoad],
    media_generation: &Cell<u64>,
    shared_media_generation: &AtomicU64,
    coordinator: Option<&RefCell<Option<LivePlayerCoordinator>>>,
    keep_paused: bool,
) -> Result<Option<String>, String> {
    let mut subtitle_paths = Vec::new();
    let mut media_entries = Vec::new();
    for entry in loads {
        if entry.path.is_empty() {
            continue;
        }
        if is_supported_subtitle_str(entry.path.as_str()) {
            subtitle_paths.push(entry.path.clone());
        } else {
            media_entries.push(entry);
        }
    }
    if media_entries.is_empty() {
        for sub_path in &subtitle_paths {
            player.command(&["sub-add", sub_path])?;
        }
        return Ok(None);
    }
    let mut first_media_path: Option<String> = None;
    for entry in media_entries {
        if first_media_path.is_none() {
            player.pause()?;
            let mut opts = entry.options.clone();
            for sub_path in &subtitle_paths {
                opts.push(format!("sub-files-append={sub_path}"));
            }
            player.load_file_with_options(&entry.path, false, &opts)?;
            first_media_path = Some(entry.path.clone());
        } else {
            player.load_file_with_options(&entry.path, true, &entry.options)?;
        }
    }
    let Some(first_path) = first_media_path else {
        return Ok(None);
    };
    let next_gen = media_generation.get().wrapping_add(1);
    media_generation.set(next_gen);
    shared_media_generation.store(next_gen, Ordering::Release);
    if let Some(coord_cell) = coordinator {
        if let Ok(mut guard) = coord_cell.try_borrow_mut() {
            if let Some(coord) = guard.as_mut() {
                coord.mark_explicit_media_load(next_gen);
            }
        }
    }
    if keep_paused {
        app.set_pending_playback_generation(-1);
        app.set_is_playing(false);
    } else if player.is_opengl_active() {
        app.set_pending_playback_generation((next_gen & i32::MAX as u64) as i32);
        app.set_is_playing(true);
    } else {
        app.set_pending_playback_generation(-1);
        player.play()?;
        app.set_is_playing(true);
    }
    let title = effective_media_title("", &first_path);
    app.set_media_title(title.into());
    app.set_secondary_sub_text("".into());
    app.set_thumbnail_frame(slint::Image::default());
    app.set_hover_chapter_title("".into());
    app.invoke_clear_audio_context();
    clear_primary_subtitle(app);
    Ok(Some(first_path))
}

pub(crate) fn load_media_into(
    app: &MementoApp,
    player: &MpvInstance,
    path: &str,
    media_generation: &Cell<u64>,
    shared_media_generation: &AtomicU64,
    coordinator: Option<&RefCell<Option<LivePlayerCoordinator>>>,
) -> Result<(), String> {
    let single = [PlaylistLoad {
        path: path.to_owned(),
        options: Vec::new(),
    }];
    load_playlist_into(
        app,
        player,
        &single,
        media_generation,
        shared_media_generation,
        coordinator,
        false,
    )?;
    Ok(())
}

pub(crate) struct PlaybackHandles {
    pub(crate) trigger_initial_loads: Rc<dyn Fn()>,
    pub(crate) handle_open_or_drop_paths: Rc<dyn Fn(Vec<PathBuf>)>,
    pub(crate) handle_add_subtitle_paths: Rc<dyn Fn(Vec<PathBuf>)>,
    pub(crate) quit_timer: Rc<slint::Timer>,
    pub(crate) hover_resume_timer: Rc<slint::Timer>,
    pub(crate) hover_pause_owned: Rc<Cell<bool>>,
    pub(crate) asr_shutdown_timed_out: Rc<Cell<bool>>,
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn install_playback_handlers(
    app: &MementoApp,
    mpv: Rc<RefCell<Option<MpvInstance>>>,
    coordinator: Rc<RefCell<Option<LivePlayerCoordinator>>>,
    sub_ctrl: Rc<RefCell<SubtitleController>>,
    sub_model: Rc<VecModel<SubEntry>>,
    secondary_sub_model: Rc<VecModel<SubEntry>>,
    config_ctrl: Rc<ConfigController>,
    config_dir: PathBuf,
    current_media: Rc<RefCell<Option<String>>>,
    media_generation: Rc<Cell<u64>>,
    shared_media_generation: Arc<AtomicU64>,
    asr_session: Rc<RefCell<AsrController>>,
    asr_restart_pending: Rc<Cell<bool>>,
    ass_ctrl: Rc<RefCell<AssOverlayController>>,
    screensaver: Rc<RefCell<memento_os::Screensaver>>,
    closing: Rc<Cell<bool>>,
    thumb_tx: ChannelSlot<ThumbRequest>,
    ui_relay: UiRelay,
    audio_context_marks: Rc<RefCell<AudioContextMarkState>>,
    dictionary_manager: Option<DictionaryManager>,
    initial_loads: Vec<PlaylistLoad>,
    cli_keep_paused: bool,
    #[cfg(feature = "ocr")] ocr_controller: Result<crate::controller::ocr::OcrController, String>,
) -> PlaybackHandles {
    let ocr_epoch = Arc::new(AtomicU64::new(0));

    // ── Manual A/B Audio Context Clearing (H5.2 / #136) ─────────────────────
    let clear_audio_marks = audio_context_marks;
    let clear_audio_app = app.as_weak();
    app.on_clear_audio_context(move || {
        clear_audio_marks.borrow_mut().clear();
        if let Some(app) = clear_audio_app.upgrade() {
            if !app.get_audio_context_hud_text().is_empty() {
                app.set_audio_context_hud_text("".into());
            }
        }
    });

    // ── Copy subtitle text on double-click (M2) ─────────────────────────────
    let copy_sub_app = app.as_weak();
    app.on_copy_subtitle_text(move |text| {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return;
        }
        copy_text_to_clipboard(trimmed);
        if let Some(app) = copy_sub_app.upgrade() {
            app.set_copied_subtitle_text(trimmed.into());
            app.set_status_message("Copied subtitle text".into());
        }
    });

    // ── Subtitle hover-pause (respecting `search/hover-pause-resume`) ───────
    let hover_pause_owned = Rc::new(Cell::new(false));
    let hover_resume_timer = Rc::new(slint::Timer::default());
    let hover_player = mpv.clone();
    let hover_config = config_ctrl.clone();
    let hover_app = app.as_weak();
    let hover_owned = hover_pause_owned.clone();
    let resume_timer = hover_resume_timer.clone();
    app.on_subtitle_hover_changed(move |hovering| {
        let Some(app) = hover_app.upgrade() else {
            return;
        };
        if hovering && app.get_hover_pause_enabled() {
            resume_timer.stop();
            if let Some(player) = hover_player.borrow().as_ref() {
                if app.get_is_playing() && !hover_owned.get() && player.pause().is_ok() {
                    hover_owned.set(true);
                }
            }
        } else if hover_owned.get() {
            if !should_resume_on_subtitle_unhover(&hover_config) {
                resume_timer.stop();
                hover_owned.set(false);
                return;
            }
            let weak = hover_app.clone();
            let player = hover_player.clone();
            let owned = hover_owned.clone();
            resume_timer.start(
                slint::TimerMode::SingleShot,
                Duration::from_millis(70),
                move || {
                    if weak.upgrade().is_some() && owned.replace(false) {
                        if let Some(player) = player.borrow().as_ref() {
                            let _ = player.play();
                        }
                    }
                },
            );
        }
    });

    // ── Initial CLI media loads ─────────────────────────────────────────────
    let pending_initial_loads: Rc<RefCell<Option<Vec<PlaylistLoad>>>> = Rc::new(RefCell::new(
        (!initial_loads.is_empty()).then_some(initial_loads),
    ));
    let trigger_initial_loads: Rc<dyn Fn()> = {
        let initial_app = app.as_weak();
        let initial_mpv = mpv.clone();
        let initial_coord = coordinator.clone();
        let initial_generation = media_generation.clone();
        let initial_shared_gen = shared_media_generation.clone();
        let initial_media = current_media.clone();
        let initial_config = config_ctrl.clone();
        let initial_cache = config_dir.clone();
        let initial_loads_cell = pending_initial_loads;
        Rc::new(move || {
            let Some(loads) = initial_loads_cell.borrow_mut().take() else {
                return;
            };
            let app_weak = initial_app.clone();
            let mpv_rc = initial_mpv.clone();
            let coord_rc = initial_coord.clone();
            let gen_rc = initial_generation.clone();
            let shared_gen = initial_shared_gen.clone();
            let media_rc = initial_media.clone();
            let cfg_rc = initial_config.clone();
            let cache_path = initial_cache.clone();
            slint::Timer::single_shot(Duration::ZERO, move || {
                let Some(app) = app_weak.upgrade() else {
                    return;
                };
                let player_ref = mpv_rc.borrow();
                let Some(player) = player_ref.as_ref() else {
                    return;
                };
                match load_playlist_into(
                    &app,
                    player,
                    &loads,
                    &gen_rc,
                    &shared_gen,
                    Some(&coord_rc),
                    cli_keep_paused,
                ) {
                    Ok(Some(first_path)) => {
                        *media_rc.borrow_mut() = Some(first_path.clone());
                        if Path::new(&first_path).is_file() {
                            if let Err(error) =
                                record_recent_media(&app, &cfg_rc, &cache_path, &first_path)
                            {
                                eprintln!("Cannot save recent media: {error}");
                            }
                        }
                    }
                    Ok(None) => {}
                    Err(error) => eprintln!("Cannot open CLI media: {error}"),
                }
            });
        })
    };

    // ── Open / drop files & recent files ────────────────────────────────────
    let handle_open_or_drop_paths: Rc<dyn Fn(Vec<PathBuf>)> = {
        let weak_app = app.as_weak();
        let open_mpv = mpv.clone();
        let open_coord = coordinator.clone();
        let open_sub_ctrl = sub_ctrl.clone();
        let open_sub_model = sub_model.clone();
        let open_sec_sub_model = secondary_sub_model.clone();
        let open_current_media = current_media.clone();
        let open_config = config_ctrl.clone();
        let open_generation = media_generation.clone();
        let open_shared_gen = shared_media_generation.clone();
        let open_cache = config_dir.clone();
        let open_dict_manager = dictionary_manager;
        let open_relay = ui_relay.clone();
        Rc::new(move |paths: Vec<PathBuf>| {
            if paths.is_empty() {
                return;
            }
            let Some(app) = weak_app.upgrade() else {
                return;
            };
            let mut zip_paths = Vec::new();
            let mut media_paths = Vec::new();
            let mut subtitle_paths = Vec::new();
            for path in paths {
                if is_dictionary_archive_path(path.as_path()) {
                    zip_paths.push(path);
                } else if is_supported_subtitle_path(path.as_path()) {
                    subtitle_paths.push(path);
                } else {
                    media_paths.push(path);
                }
            }
            if !zip_paths.is_empty() {
                if let Some(manager) = open_dict_manager.clone() {
                    app.set_status_message(
                        format!("Importing {} dictionary archive(s)…", zip_paths.len()).into(),
                    );
                    let relay = open_relay.clone();
                    std::thread::spawn(move || {
                        let mut imported = 0usize;
                        let mut errors = Vec::new();
                        let mut all_dicts = Vec::new();
                        for archive in zip_paths {
                            match manager.add_dictionary(&archive) {
                                Ok(dicts) => {
                                    imported += 1;
                                    all_dicts = dicts;
                                }
                                Err(err) => {
                                    let label = archive
                                        .file_name()
                                        .and_then(|n| n.to_str())
                                        .unwrap_or("archive");
                                    errors.push(format!("{label}: {err}"));
                                }
                            }
                        }
                        if all_dicts.is_empty() {
                            all_dicts = manager.list_dictionaries().unwrap_or_default();
                        }
                        relay.send_event(UiEvent::DictionariesImported {
                            imported,
                            errors,
                            all_dicts,
                        });
                    });
                } else {
                    app.set_status_message("Dictionary manager is unavailable".into());
                }
            }
            let player_ref = open_mpv.borrow();
            if !media_paths.is_empty() {
                let Some(player) = player_ref.as_ref() else {
                    app.set_status_message("Playback backend unavailable".into());
                    return;
                };
                if media_paths.len() == 1 {
                    let file_str = media_paths[0].to_string_lossy().into_owned();
                    match load_media_into(
                        &app,
                        player,
                        &file_str,
                        &open_generation,
                        &open_shared_gen,
                        Some(&open_coord),
                    ) {
                        Ok(()) => {
                            *open_current_media.borrow_mut() = Some(file_str.clone());
                            open_sub_ctrl.borrow_mut().clear();
                            open_sub_model.set_vec(Vec::new());
                            open_sec_sub_model.set_vec(Vec::new());
                            if let Err(error) =
                                record_recent_media(&app, &open_config, &open_cache, &file_str)
                            {
                                app.set_status_message(
                                    format!("Cannot save recent media: {error}").into(),
                                );
                            }
                        }
                        Err(error) => {
                            app.set_status_message(format!("Cannot open media: {error}").into());
                        }
                    }
                } else {
                    let loads: Vec<PlaylistLoad> = media_paths
                        .iter()
                        .map(|path| PlaylistLoad {
                            path: path.to_string_lossy().into_owned(),
                            options: Vec::new(),
                        })
                        .collect();
                    match load_playlist_into(
                        &app,
                        player,
                        &loads,
                        &open_generation,
                        &open_shared_gen,
                        Some(&open_coord),
                        false,
                    ) {
                        Ok(Some(first_path)) => {
                            *open_current_media.borrow_mut() = Some(first_path.clone());
                            open_sub_ctrl.borrow_mut().clear();
                            open_sub_model.set_vec(Vec::new());
                            open_sec_sub_model.set_vec(Vec::new());
                            if let Err(error) =
                                record_recent_media(&app, &open_config, &open_cache, &first_path)
                            {
                                app.set_status_message(
                                    format!("Cannot save recent media: {error}").into(),
                                );
                            }
                        }
                        Ok(None) => {}
                        Err(error) => {
                            app.set_status_message(format!("Cannot open playlist: {error}").into());
                        }
                    }
                }
            }
            for sub_path in subtitle_paths {
                let sub_str = sub_path.to_string_lossy().into_owned();
                let mut added_to_mpv = false;
                if let Some(player) = player_ref.as_ref() {
                    if app.get_duration() > 0.0 || !media_paths.is_empty() {
                        match player.command(&["sub-add", &sub_str]) {
                            Ok(()) => {
                                added_to_mpv = true;
                            }
                            Err(error) => {
                                app.set_status_message(
                                    format!("Could not add subtitles: {error}").into(),
                                );
                            }
                        }
                    }
                }
                if !added_to_mpv && sub_path.is_file() {
                    match parse_external_subtitles(&sub_path) {
                        Ok(cues) => {
                            let mut subtitles = open_sub_ctrl.borrow_mut();
                            let sid = if subtitles.primary_sid() > 0 {
                                subtitles.primary_sid()
                            } else {
                                subtitles.set_primary_sid(1);
                                app.set_primary_sid(1);
                                1
                            };
                            subtitles.set_external_cues(sid, &sub_str, cues);
                            refresh_subtitle_list(&app, &open_sub_model, &subtitles);
                        }
                        Err(error) => {
                            app.set_status_message(
                                format!("Could not add subtitles: {error}").into(),
                            );
                        }
                    }
                }
            }
        })
    };

    // C5.3: Dedicated Add Subtitle handler that ALWAYS invokes `sub-add`, never `loadfile`.
    let handle_add_subtitle_paths: Rc<dyn Fn(Vec<PathBuf>)> = {
        let weak_app = app.as_weak();
        let add_mpv = mpv.clone();
        let add_sub_ctrl = sub_ctrl.clone();
        let add_sub_model = sub_model.clone();
        Rc::new(move |paths: Vec<PathBuf>| {
            if paths.is_empty() {
                return;
            }
            let Some(app) = weak_app.upgrade() else {
                return;
            };
            let player_ref = add_mpv.borrow();
            add_subtitle_paths_unconditionally(
                &app,
                player_ref.as_ref(),
                &add_sub_ctrl,
                &add_sub_model,
                &paths,
            );
        })
    };

    // C1.1: Route Open File dialog through `open_files_on_main_thread` so macOS
    // runs `NSOpenPanel` strictly on the main AppKit thread while Linux/Windows stay async.
    let open_file_config = config_ctrl.clone();
    let open_file_relay = ui_relay.clone();
    app.on_open_file(move || {
        let start_dir = resolve_config_file_open_dir(&open_file_config);
        let relay = open_file_relay.clone();
        memento_os::open_files_on_main_thread(
            memento_os::FileDialogOptions::media_and_subtitles(start_dir),
            |task| {
                let _ = slint::invoke_from_event_loop(task);
            },
            move |paths| {
                if !paths.is_empty() {
                    relay.send_event(UiEvent::OpenPaths(paths));
                }
            },
        );
    });

    let drop_single = handle_open_or_drop_paths.clone();
    app.on_file_dropped(move |uri_or_path| {
        let paths = memento_os::parse_uri_list(uri_or_path.as_str());
        if !paths.is_empty() {
            drop_single(paths);
        }
    });

    let drop_multi = handle_open_or_drop_paths.clone();
    app.on_files_dropped(move |uris| {
        let mut paths = Vec::new();
        for i in 0..uris.row_count() {
            if let Some(entry) = uris.row_data(i) {
                for parsed in memento_os::parse_uri_list(entry.as_str()) {
                    if !paths.contains(&parsed) {
                        paths.push(parsed);
                    }
                }
            }
        }
        if !paths.is_empty() {
            drop_multi(paths);
        }
    });

    let recent_app = app.as_weak();
    let recent_mpv = mpv.clone();
    let recent_coord = coordinator.clone();
    let recent_subs = sub_ctrl.clone();
    let recent_model = sub_model.clone();
    let recent_sec_model = secondary_sub_model.clone();
    let recent_current = current_media.clone();
    let recent_generation = media_generation.clone();
    let recent_shared_gen = shared_media_generation.clone();
    let recent_config = config_ctrl.clone();
    let recent_cache = config_dir.clone();
    app.on_open_recent(move |path| {
        let Some(app) = recent_app.upgrade() else {
            return;
        };
        let player = recent_mpv.borrow();
        let Some(player) = player.as_ref() else {
            return;
        };
        match load_media_into(
            &app,
            player,
            &path,
            &recent_generation,
            &recent_shared_gen,
            Some(&recent_coord),
        ) {
            Ok(()) => {
                *recent_current.borrow_mut() = Some(path.to_string());
                recent_subs.borrow_mut().clear();
                recent_model.set_vec(Vec::new());
                recent_sec_model.set_vec(Vec::new());
                if let Err(error) = record_recent_media(&app, &recent_config, &recent_cache, &path)
                {
                    app.set_status_message(format!("Cannot save recent media: {error}").into());
                }
            }
            Err(error) => app.set_status_message(format!("Cannot open media: {error}").into()),
        }
    });

    let clear_app = app.as_weak();
    let clear_config = config_ctrl.clone();
    let clear_cache = config_dir.clone();
    app.on_clear_recent(move || {
        clear_recent_titles();
        let result = clear_config
            .clear_recent_files()
            .and_then(|_| clear_config.save(&clear_cache, "memento.conf"));
        if let Some(app) = clear_app.upgrade() {
            match result {
                Ok(()) => refresh_recent_files(&app, &clear_config),
                Err(error) => {
                    app.set_status_message(format!("Cannot clear recents: {error}").into())
                }
            }
        }
    });

    // ── Transport Callbacks ─────────────────────────────────────────────────
    let play_mpv = mpv.clone();
    let play_hover_owned = hover_pause_owned.clone();
    let play_hover_resume = hover_resume_timer.clone();
    let play_app = app.as_weak();
    app.on_toggle_play(move || {
        play_hover_resume.stop();
        play_hover_owned.set(false);
        if let Some(app) = play_app.upgrade() {
            if app.get_pending_playback_generation() >= 0 {
                app.set_pending_playback_generation(-1);
                app.set_is_playing(false);
                return;
            }
        }
        if let Some(mpv_inst) = &*play_mpv.borrow() {
            let _ = mpv_inst.toggle_pause();
        }
    });

    let seek_mpv = mpv.clone();
    let seek_coord = coordinator.clone();
    let seek_asr = asr_session.clone();
    app.on_seek_to(move |sec| {
        if let Some(mpv_inst) = &*seek_mpv.borrow() {
            let target = (sec as f64).max(0.0);
            if mpv_inst.seek(target).is_ok() {
                if let (Ok(mut coord_ref), Ok(mut asr)) =
                    (seek_coord.try_borrow_mut(), seek_asr.try_borrow_mut())
                {
                    if let Some(coord) = coord_ref.as_mut() {
                        coord.notify_user_seek(&mut asr, Some(target), mpv_inst.get_duration());
                    }
                }
            }
        }
    });

    let rel_seek_mpv = mpv.clone();
    let rel_seek_coord = coordinator.clone();
    let rel_seek_asr = asr_session.clone();
    app.on_seek_relative(move |delta| {
        if let Some(mpv_inst) = &*rel_seek_mpv.borrow() {
            let dur = mpv_inst.get_duration();
            let target = (mpv_inst.get_position() + delta as f64).clamp(0.0, dur.max(0.0));
            if mpv_inst.seek_relative(delta as f64).is_ok() {
                if let (Ok(mut coord_ref), Ok(mut asr)) = (
                    rel_seek_coord.try_borrow_mut(),
                    rel_seek_asr.try_borrow_mut(),
                ) {
                    if let Some(coord) = coord_ref.as_mut() {
                        coord.notify_user_seek(&mut asr, Some(target), dur);
                    }
                }
            }
        }
    });

    let sub_seek_mpv = mpv.clone();
    let sub_seek_coord = coordinator.clone();
    let sub_seek_asr = asr_session.clone();
    app.on_sub_seek(move |direction| {
        if let Some(mpv_inst) = &*sub_seek_mpv.borrow() {
            if mpv_inst.subtitle_seek(direction, true).is_ok() {
                if let (Ok(mut coord_ref), Ok(mut asr)) = (
                    sub_seek_coord.try_borrow_mut(),
                    sub_seek_asr.try_borrow_mut(),
                ) {
                    if let Some(coord) = coord_ref.as_mut() {
                        coord.notify_user_seek(&mut asr, None, mpv_inst.get_duration());
                    }
                }
            }
        }
    });

    let prev_mpv = mpv.clone();
    app.on_playlist_prev(move || {
        if let Some(player) = &*prev_mpv.borrow() {
            if let Err(error) = player.playlist_prev() {
                eprintln!("Previous media: {error}");
            }
        }
    });
    let next_mpv = mpv.clone();
    app.on_playlist_next(move || {
        if let Some(player) = &*next_mpv.borrow() {
            if let Err(error) = player.playlist_next() {
                eprintln!("Next media: {error}");
            }
        }
    });

    // ── Track & ASR Selection ───────────────────────────────────────────────
    let video_mpv = mpv.clone();
    let video_app = app.as_weak();
    app.on_select_video_track(move |id| {
        if let Some(player) = &*video_mpv.borrow() {
            match player.set_vid(id.into()) {
                Ok(()) => {
                    if let Some(app) = video_app.upgrade() {
                        app.set_video_id(id);
                    }
                }
                Err(error) => eprintln!("Cannot select video track {id}: {error}"),
            }
        } else if let Some(app) = video_app.upgrade() {
            app.set_video_id(id);
        }
    });

    let audio_mpv = mpv.clone();
    let audio_app = app.as_weak();
    app.on_select_audio_track(move |id| {
        if let Some(player) = &*audio_mpv.borrow() {
            match player.set_aid(id.into()) {
                Ok(()) => {
                    if let Some(app) = audio_app.upgrade() {
                        app.set_audio_id(id);
                    }
                }
                Err(error) => eprintln!("Cannot select audio track {id}: {error}"),
            }
        }
    });

    let primary_mpv = mpv.clone();
    let primary_coord = coordinator.clone();
    let primary_subs = sub_ctrl.clone();
    let primary_sub_model = sub_model.clone();
    let primary_app = app.as_weak();
    let primary_asr = asr_session.clone();
    let primary_asr_restart = asr_restart_pending.clone();
    app.on_select_subtitle_track(move |id| {
        if let Some(player) = &*primary_mpv.borrow() {
            match player.set_sid(id.into()) {
                Ok(()) => {
                    primary_asr.borrow_mut().suspend();
                    primary_asr_restart.set(false);
                    if let Ok(mut coord_ref) = primary_coord.try_borrow_mut() {
                        if let Some(coord) = coord_ref.as_mut() {
                            coord.on_manual_primary_sid(id.into());
                        }
                    }
                    let mut subtitles = primary_subs.borrow_mut();
                    subtitles.set_primary_sid(id.into());
                    if let Some(app) = primary_app.upgrade() {
                        refresh_subtitle_list(&app, &primary_sub_model, &subtitles);
                        app.set_asr_track_selected(false);
                        app.set_asr_running(false);
                        app.set_primary_sid(id);
                        clear_primary_subtitle(&app);
                    }
                }
                Err(error) => eprintln!("Cannot select primary subtitle track {id}: {error}"),
            }
        }
    });

    let secondary_mpv = mpv.clone();
    let secondary_subs = sub_ctrl.clone();
    let secondary_sec_model = secondary_sub_model.clone();
    let secondary_app = app.as_weak();
    app.on_select_secondary_subtitle_track(move |id| {
        if let Some(player) = &*secondary_mpv.borrow() {
            match player.set_secondary_sid(id.into()) {
                Ok(()) => {
                    let mut subtitles = secondary_subs.borrow_mut();
                    subtitles.set_secondary_sid(id.into());
                    if let Some(app) = secondary_app.upgrade() {
                        app.set_secondary_sid(id);
                        refresh_secondary_subtitle_list(&app, &secondary_sec_model, &subtitles);
                        if id <= 0 && app.get_subtitle_drawer_tab() == 1 {
                            app.set_subtitle_drawer_tab(0);
                        }
                    }
                }
                Err(error) => eprintln!("Cannot select secondary subtitle track {id}: {error}"),
            }
        } else {
            let mut subtitles = secondary_subs.borrow_mut();
            subtitles.set_secondary_sid(id.into());
            if let Some(app) = secondary_app.upgrade() {
                app.set_secondary_sid(id);
                refresh_secondary_subtitle_list(&app, &secondary_sec_model, &subtitles);
                if id <= 0 && app.get_subtitle_drawer_tab() == 1 {
                    app.set_subtitle_drawer_tab(0);
                }
            }
        }
    });

    let select_asr_app = app.as_weak();
    let select_asr_mpv = mpv.clone();
    let select_asr_coord = coordinator.clone();
    let select_asr_config = config_ctrl.clone();
    let select_asr_session = asr_session.clone();
    let select_asr_restart = asr_restart_pending.clone();
    let select_asr_sub_ctrl = sub_ctrl.clone();
    let select_asr_sub_model = sub_model.clone();
    app.on_select_asr(move || {
        let Some(app) = select_asr_app.upgrade() else {
            return;
        };
        let Some(player) = &*select_asr_mpv.borrow() else {
            app.set_status_message("Playback backend unavailable".into());
            return;
        };
        let mut coord_ref = select_asr_coord.borrow_mut();
        let Some(coord) = coord_ref.as_mut() else {
            return;
        };
        let mut asr = select_asr_session.borrow_mut();
        let mut subtitles = select_asr_sub_ctrl.borrow_mut();
        coord.toggle_or_start_asr(
            &app,
            player,
            &mut asr,
            &mut subtitles,
            &select_asr_sub_model,
            &select_asr_restart,
            || {
                let cfg_dir = memento_xdg_dir("XDG_CONFIG_HOME", ".config")
                    .unwrap_or_else(|| PathBuf::from("."));
                AsrSettings::from_config(&cfg_dir, &*select_asr_config.store())
            },
        );
    });

    // ── Volume & Mute ───────────────────────────────────────────────────────
    let vol_app = app.as_weak();
    let vol_mpv = mpv.clone();
    app.on_set_volume(move |vol| {
        let max_vol = vol_app
            .upgrade()
            .map(|app| app.get_volume_max().max(100.0))
            .unwrap_or(100.0);
        let clamped = vol.clamp(0.0, max_vol);
        if let Some(app) = vol_app.upgrade() {
            app.set_volume(clamped);
            if clamped > 0.0 && app.get_is_muted() {
                app.set_is_muted(false);
            }
        }
        if let Some(mpv_inst) = &*vol_mpv.borrow() {
            let _ = mpv_inst.set_volume(clamped as f64);
            if clamped > 0.0 {
                let _ = mpv_inst.command_async(&["set", "mute", "no"]);
            }
        }
    });

    let weak_mute = app.as_weak();
    let mute_mpv = mpv.clone();
    app.on_toggle_mute(move || {
        if let Some(app) = weak_mute.upgrade() {
            let next_muted = !app.get_is_muted();
            app.set_is_muted(next_muted);
            if let Some(mpv_inst) = &*mute_mpv.borrow() {
                let _ =
                    mpv_inst.command_async(&["set", "mute", if next_muted { "yes" } else { "no" }]);
            }
        }
    });

    let weak_vup = app.as_weak();
    let vup_mpv = mpv.clone();
    app.on_volume_up(move || {
        if let Some(app) = weak_vup.upgrade() {
            let max_vol = app.get_volume_max().max(100.0);
            let new_vol = (app.get_volume() + 5.0).clamp(0.0, max_vol);
            app.set_volume(new_vol);
            if app.get_is_muted() {
                app.set_is_muted(false);
            }
            if let Some(mpv_inst) = &*vup_mpv.borrow() {
                let _ = mpv_inst.command_async(&["set", "mute", "no"]);
                let _ = mpv_inst.set_volume(new_vol as f64);
            }
        }
    });

    let weak_vdown = app.as_weak();
    let vdown_mpv = mpv.clone();
    app.on_volume_down(move || {
        if let Some(app) = weak_vdown.upgrade() {
            let max_vol = app.get_volume_max().max(100.0);
            let new_vol = (app.get_volume() - 5.0).clamp(0.0, max_vol);
            app.set_volume(new_vol);
            if let Some(mpv_inst) = &*vdown_mpv.borrow() {
                let _ = mpv_inst.set_volume(new_vol as f64);
            }
        }
    });

    // ── URL Open, Reload, Close Media ───────────────────────────────────────
    let weak_url = app.as_weak();
    app.on_open_url(move || {
        let Some(app) = weak_url.upgrade() else {
            return;
        };
        let clip = read_copied_text_or_clipboard();
        let trimmed = clip.trim();
        if trimmed.starts_with("http://")
            || trimmed.starts_with("https://")
            || trimmed.starts_with("rtsp://")
            || trimmed.starts_with("rtmp://")
            || trimmed.starts_with("mms://")
            || trimmed.starts_with("file://")
        {
            app.set_url_dialog_text(trimmed.into());
        }
        app.set_url_dialog_open(true);
    });

    let submit_url_app = app.as_weak();
    let url_mpv = mpv.clone();
    let url_coord = coordinator.clone();
    let url_current = current_media.clone();
    let url_subs = sub_ctrl.clone();
    let url_generation = media_generation.clone();
    let url_shared_gen = shared_media_generation.clone();
    let url_model = sub_model.clone();
    let url_sec_model = secondary_sub_model.clone();
    app.on_open_url_submitted(move |raw_url| {
        let Some(app) = submit_url_app.upgrade() else {
            return;
        };
        app.set_url_dialog_open(false);
        let url = raw_url.trim().to_owned();
        if url.is_empty() {
            return;
        }
        if let Some(player) = &*url_mpv.borrow() {
            match load_media_into(
                &app,
                player,
                &url,
                &url_generation,
                &url_shared_gen,
                Some(&url_coord),
            ) {
                Ok(()) => {
                    *url_current.borrow_mut() = Some(url);
                    url_subs.borrow_mut().clear();
                    url_model.set_vec(Vec::new());
                    url_sec_model.set_vec(Vec::new());
                }
                Err(error) => app.set_status_message(format!("Cannot open URL: {error}").into()),
            }
        } else {
            *url_current.borrow_mut() = Some(url.clone());
            app.set_media_title(url.into());
        }
    });

    let reload_app = app.as_weak();
    let reload_mpv = mpv.clone();
    let reload_coord = coordinator.clone();
    let reload_current = current_media.clone();
    let reload_subs = sub_ctrl.clone();
    let reload_generation = media_generation.clone();
    let reload_shared_gen = shared_media_generation.clone();
    let reload_model = sub_model.clone();
    let reload_sec_model = secondary_sub_model.clone();
    app.on_reload_media(move || {
        let Some(app) = reload_app.upgrade() else {
            return;
        };
        let current = reload_current.borrow().clone();
        let Some(path) = current.as_deref() else {
            return;
        };
        if let Some(player) = &*reload_mpv.borrow() {
            match load_media_into(
                &app,
                player,
                path,
                &reload_generation,
                &reload_shared_gen,
                Some(&reload_coord),
            ) {
                Ok(()) => {
                    reload_subs.borrow_mut().clear();
                    reload_model.set_vec(Vec::new());
                    reload_sec_model.set_vec(Vec::new());
                }
                Err(error) => {
                    app.set_status_message(format!("Cannot reload media: {error}").into())
                }
            }
        }
    });

    let weak_close = app.as_weak();
    let close_mpv = mpv.clone();
    let close_coord = coordinator;
    let close_sub_ctrl = sub_ctrl;
    let close_sub_model = sub_model;
    let close_sec_sub_model = secondary_sub_model;
    let close_current = current_media.clone();
    let close_generation = media_generation;
    let close_shared_gen = shared_media_generation;
    let close_asr = asr_session.clone();
    let close_ass = ass_ctrl;
    let close_screensaver = screensaver.clone();
    let close_thumb_tx = thumb_tx.clone();
    let close_ocr_epoch = ocr_epoch.clone();
    app.on_close_media(move || {
        close_ocr_epoch.fetch_add(1, Ordering::AcqRel);
        {
            let player_ref = close_mpv.borrow();
            if let Some(mpv_inst) = player_ref.as_ref() {
                if let Ok(mut coord_ref) = close_coord.try_borrow_mut() {
                    if let Some(coord) = coord_ref.as_mut() {
                        coord.remove_asr_track(Some(mpv_inst));
                    }
                }
                let _ = mpv_inst.command(&["stop"]);
            }
        }
        // Unload thumbnail preview mpv media so its file handle and decoder buffers release.
        let _ = close_thumb_tx.send(ThumbRequest::Unload);
        close_screensaver.borrow_mut().uninhibit();
        close_asr.borrow_mut().cancel();
        let next_gen = close_generation.get().wrapping_add(1);
        close_generation.set(next_gen);
        close_shared_gen.store(next_gen, Ordering::Release);
        *close_current.borrow_mut() = None;
        close_sub_ctrl.borrow_mut().clear();
        close_sub_model.set_vec(Vec::new());
        close_sec_sub_model.set_vec(Vec::new());
        if let Some(app) = weak_close.upgrade() {
            app.invoke_dismiss_definition();
            app.invoke_clear_audio_context();
            ui::clear_rich_bridge_ass_lookup(&app);
            if let Ok(mut ass) = close_ass.try_borrow_mut() {
                ass.clear_for_new_media(&app);
            }
            app.set_pending_playback_generation(-1);
            app.set_asr_track_selected(false);
            app.set_asr_running(false);
            app.set_media_title("".into());
            app.set_position(0.0);
            app.set_duration(0.0);
            app.set_is_playing(false);
            app.set_active_sub_text("".into());
            app.set_secondary_sub_text("".into());
            app.set_active_sub_words(Rc::new(VecModel::from(Vec::new())).into());
            app.set_active_sub_lines(Rc::new(VecModel::from(Vec::new())).into());
            app.set_chapters(Rc::new(VecModel::from(Vec::new())).into());
            app.set_chapter_marks(Rc::new(VecModel::from(Vec::new())).into());
            app.set_thumbnail_frame(slint::Image::default());
            app.set_hover_chapter_title("".into());
            app.set_selected_token_index(-1);
            app.set_current_time("00:00".into());
            app.set_duration_time("00:00".into());
        }
        crate::app::trim_os_heap();
    });

    // ── Bounded Graceful Quit / Close Window Handler (C5.1 & M3) ────────────
    let quit_timer = Rc::new(slint::Timer::default());
    let asr_shutdown_timed_out = Rc::new(Cell::new(false));
    let quit_poll = quit_timer.clone();
    let quit_asr = asr_session;
    let quit_restart = asr_restart_pending;
    let quit_closing = closing;
    let quit_player = mpv.clone();
    let quit_hover_resume = hover_resume_timer.clone();
    let quit_hover_owned = hover_pause_owned.clone();
    let quit_screensaver = screensaver;
    let quit_timed_out = asr_shutdown_timed_out.clone();
    let quit_config = config_ctrl.clone();
    let quit_config_dir = config_dir;
    let quit_ocr_epoch = ocr_epoch.clone();
    let quit_app = app.as_weak();
    app.on_quit_app(move || {
        if quit_closing.replace(true) {
            return;
        }
        quit_ocr_epoch.fetch_add(1, Ordering::AcqRel);
        quit_screensaver.borrow_mut().uninhibit();
        quit_restart.set(false);
        quit_hover_resume.stop();
        quit_hover_owned.set(false);
        quit_asr.borrow_mut().begin_shutdown();
        if let Some(app) = quit_app.upgrade() {
            persist_window_panel_state(&app, &quit_config, &quit_config_dir);
            app.invoke_dismiss_definition();
            app.set_asr_track_selected(false);
            app.set_asr_running(false);
            app.set_pending_playback_generation(-1);
            app.set_status_message("Closing Memento — stopping speech recognition…".into());
        }
        if let Some(player) = quit_player.borrow().as_ref() {
            let _ = player.command_async(&["set", "pause", "yes"]);
        }
        if quit_asr.borrow_mut().try_finish_shutdown() {
            if let Err(error) = slint::quit_event_loop() {
                eprintln!("Could not close Memento: {error}");
            }
            return;
        }
        let deadline = Instant::now() + ASR_SHUTDOWN_TIMEOUT;
        let asr = quit_asr.clone();
        let stop_timer = quit_poll.clone();
        let timed_out_flag = quit_timed_out.clone();
        quit_poll.start(
            slint::TimerMode::Repeated,
            Duration::from_millis(25),
            move || {
                let done = asr.borrow_mut().try_finish_shutdown();
                let expired = Instant::now() >= deadline;
                if done || expired {
                    if expired && !done {
                        timed_out_flag.set(true);
                        eprintln!(
                            "ASR shutdown timed out after {}s; forcing window close",
                            ASR_SHUTDOWN_TIMEOUT.as_secs()
                        );
                    }
                    stop_timer.stop();
                    if let Err(error) = slint::quit_event_loop() {
                        eprintln!("Could not close Memento: {error}");
                    }
                }
            },
        );
    });

    let close_app = app.as_weak();
    app.window().on_close_requested(move || {
        if let Some(app) = close_app.upgrade() {
            app.invoke_quit_app();
        }
        slint::CloseRequestResponse::KeepWindowShown
    });

    // ── Subtitle Helpers & OCR ──────────────────────────────────────────────
    let visibility_app = app.as_weak();
    app.on_toggle_subtitles(move || {
        if let Some(app) = visibility_app.upgrade() {
            app.invoke_subtitles_visible_changed(!app.get_subtitles_visible());
        }
    });

    // C1.1 & C5.3: Add Subtitle uses main-thread-safe file dialog and sends
    // `UiEvent::AddSubtitles` so `sub-add` is called unconditionally (never `loadfile`).
    let add_sub_config = config_ctrl.clone();
    let add_sub_relay = ui_relay;
    app.on_add_subtitle(move || {
        let start_dir = resolve_config_file_open_dir(&add_sub_config);
        let relay = add_sub_relay.clone();
        memento_os::open_files_on_main_thread(
            memento_os::FileDialogOptions::subtitles_only(start_dir),
            |task| {
                let _ = slint::invoke_from_event_loop(task);
            },
            move |paths| {
                if !paths.is_empty() {
                    relay.send_event(UiEvent::AddSubtitles(paths));
                }
            },
        );
    });

    let delay_mpv = mpv.clone();
    app.on_adjust_sub_delay(move |delta| {
        if let Some(mpv_inst) = &*delay_mpv.borrow() {
            let _ = mpv_inst.command(&["add", "sub-delay", &format!("{delta:.2}")]);
        }
    });

    let ocr_mode_app = app.as_weak();
    let ocr_start_epoch = ocr_epoch.clone();
    app.on_start_ocr(move || {
        if let Some(app) = ocr_mode_app.upgrade() {
            if !app.get_ocr_enabled() {
                app.set_status_message("OCR is disabled or unavailable".into());
            } else if app.get_duration() <= 0.0 {
                app.set_status_message("Open a video before selecting text for OCR".into());
            } else {
                ocr_start_epoch.fetch_add(1, Ordering::AcqRel);
                app.set_status_message("Drag a rectangle around text in the video".into());
                app.set_ocr_mode(true);
            }
        }
    });

    // H3.6: Wire `on_ocr_cancelled` so right-click / Escape / rapid re-trigger
    // cancels any in-flight OCR request cleanly via the atomic `ocr_epoch` guard.
    let cancel_ocr_app = app.as_weak();
    let cancel_ocr_epoch = ocr_epoch.clone();
    app.on_ocr_cancelled(move || {
        cancel_ocr_epoch.fetch_add(1, Ordering::AcqRel);
        if let Some(app) = cancel_ocr_app.upgrade() {
            app.set_ocr_mode(false);
            app.set_status_message("OCR cancelled".into());
        }
    });

    #[cfg(feature = "ocr")]
    {
        use crate::controller::ocr::{OcrFrame, OcrRegion, OcrSettings, OcrViewport};
        use crate::ui::PopupBridge;
        use memento_config::Value;

        let ocr_region_app = app.as_weak();
        let ocr_region_mpv = mpv;
        let ocr_region_config = config_ctrl;
        let ocr_region_epoch = ocr_epoch;
        app.on_ocr_region(move |x, y, width, height| {
            let Some(app) = ocr_region_app.upgrade() else {
                return;
            };
            let req_epoch = ocr_region_epoch.fetch_add(1, Ordering::AcqRel) + 1;
            app.set_status_message("Recognizing selected text…".into());
            let Some(controller) = ocr_controller.as_ref().ok().cloned() else {
                return;
            };
            let Some(player) = &*ocr_region_mpv.borrow() else {
                return;
            };
            // Keep `request_screenshot(false)` so OCR captures video only (not text subtitles).
            let capture = match player.request_screenshot(false) {
                Ok(capture) => capture,
                Err(error) => {
                    app.set_status_message(format!("Cannot capture video: {error}").into());
                    return;
                }
            };
            let defaults = OcrSettings::default();
            let model = match ocr_region_config.effective_setting(raw::ocr::GROUP, raw::ocr::MODEL)
            {
                Some(Value::Text(model)) => model,
                _ => defaults.model,
            };
            let settings = OcrSettings {
                enabled: setting_enabled(
                    &ocr_region_config,
                    raw::ocr::GROUP,
                    raw::ocr::ENABLED,
                    raw::ocr::ENABLED_DEFAULT,
                ),
                model,
                use_gpu: setting_enabled(
                    &ocr_region_config,
                    raw::ocr::GROUP,
                    raw::ocr::USE_GPU,
                    raw::ocr::USE_GPU_DEFAULT,
                ),
            };
            let response_app = ocr_region_app.clone();
            let thread_epoch = ocr_region_epoch.clone();
            let viewport = OcrViewport {
                width: app.get_video_width().into(),
                height: app.get_video_height().into(),
            };
            std::thread::spawn(move || {
                let frame = capture.wait().and_then(|raw| {
                    OcrFrame::from_rgba(raw.width, raw.height, raw.pixels)
                        .map_err(|error| error.to_string())
                });
                if thread_epoch.load(Ordering::Acquire) != req_epoch {
                    return;
                }
                let frame = match frame {
                    Ok(frame) => frame,
                    Err(error) => {
                        let ui_epoch = thread_epoch.clone();
                        let _ = response_app.upgrade_in_event_loop(move |app| {
                            if ui_epoch.load(Ordering::Acquire) == req_epoch {
                                app.set_status_message(
                                    format!("Cannot capture video: {error}").into(),
                                );
                            }
                        });
                        return;
                    }
                };
                let error_app = response_app.clone();
                let cb_epoch = thread_epoch.clone();
                let err_epoch = thread_epoch.clone();
                let result = controller.read_region_async(
                    frame,
                    OcrRegion {
                        x: x.into(),
                        y: y.into(),
                        width: width.into(),
                        height: height.into(),
                    },
                    viewport,
                    settings,
                    move |recognized| {
                        if cb_epoch.load(Ordering::Acquire) != req_epoch {
                            return;
                        }
                        let ui_epoch = cb_epoch.clone();
                        let _ = response_app.upgrade_in_event_loop(move |app| {
                            if ui_epoch.load(Ordering::Acquire) != req_epoch {
                                return;
                            }
                            match recognized {
                                Ok(text) => {
                                    let text = text.trim();
                                    if !text.is_empty() {
                                        app.invoke_dismiss_definition();
                                        app.set_dict_search_open(true);
                                        if app.global::<PopupBridge>().get_rich_enabled() {
                                            app.global::<PopupBridge>()
                                                .invoke_panel_search_requested(text.into());
                                        } else {
                                            app.set_dict_query(text.into());
                                            app.invoke_search_dictionary(text.into());
                                        }
                                        app.set_status_message(format!("OCR: {text}").into());
                                    } else {
                                        app.set_status_message(
                                            "No text recognized in selected region".into(),
                                        );
                                    }
                                }
                                Err(error) => app.set_status_message(error.to_string().into()),
                            }
                        });
                    },
                );
                if let Err(error) = result {
                    let _ = error_app.upgrade_in_event_loop(move |app| {
                        if err_epoch.load(Ordering::Acquire) == req_epoch {
                            app.set_status_message(error.to_string().into());
                        }
                    });
                }
            });
        });
    }

    // ── Timeline Hover Preview ──────────────────────────────────────────────
    let hover_timeline_app = app.as_weak();
    let hover_timeline_media = current_media;
    let hover_thumb_tx = thumb_tx;
    app.on_timeline_hover_moved(move |time_sec| {
        let Some(app) = hover_timeline_app.upgrade() else {
            return;
        };
        let chapter_title = chapter_title_at_time(&app, time_sec);
        if app.get_hover_chapter_title().as_str() != chapter_title {
            app.set_hover_chapter_title(chapter_title.into());
        }
        if app.get_preview_thumbnails_enabled() {
            if let Some(path) = hover_timeline_media.borrow().clone() {
                let _ = hover_thumb_tx.send(ThumbRequest::Seek {
                    path,
                    time_sec: time_sec as f64,
                });
            }
        }
    });

    PlaybackHandles {
        trigger_initial_loads,
        handle_open_or_drop_paths,
        handle_add_subtitle_paths,
        quit_timer,
        hover_resume_timer,
        hover_pause_owned,
        asr_shutdown_timed_out,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supported_subtitle_extensions_include_all_twelve_formats_and_ttml() {
        for ext in [
            "ass", "ssa", "srt", "vtt", "sub", "idx", "sup", "lrc", "mks", "ttml", "smi", "rt",
            "TTML", "SRT",
        ] {
            let p = PathBuf::from(format!("/tmp/episode.{ext}"));
            assert!(
                is_supported_subtitle_path(&p),
                "expected .{ext} to be recognized as a subtitle extension"
            );
        }
        assert!(!is_supported_subtitle_path(Path::new("/tmp/video.mkv")));
        assert!(!is_supported_subtitle_path(Path::new("/tmp/dict.zip")));
        assert!(is_dictionary_archive_path(Path::new("/tmp/jmdict.ZIP")));
    }

    #[test]
    fn add_subtitle_unconditional_never_replaces_media_even_for_unlisted_extension() {
        i_slint_backend_testing::init_no_event_loop();
        let app = MementoApp::new().unwrap();
        let dir = tempfile::TempDir::new().unwrap();
        let custom_sub = dir.path().join("notes.customext");
        std::fs::write(&custom_sub, "1\n00:00:01,000 --> 00:00:03,000\nHello\n").unwrap();
        app.set_media_title("Original Video".into());

        let sub_ctrl = RefCell::new(SubtitleController::new());
        let sub_model = Rc::new(VecModel::<SubEntry>::default());
        app.set_subtitle_entries(sub_model.clone().into());
        add_subtitle_paths_unconditionally(&app, None, &sub_ctrl, &sub_model, &[custom_sub]);
        assert_eq!(app.get_media_title().as_str(), "Original Video");
    }

    #[test]
    fn panel_persistence_roundtrip_and_hover_pause_resume_setting() {
        i_slint_backend_testing::init_no_event_loop();
        let dir = tempfile::TempDir::new().unwrap();
        let cfg = ConfigController::new();
        let app = MementoApp::new().unwrap();
        app.set_sub_drawer_open(true);
        app.set_dict_search_open(true);
        persist_window_panel_state(&app, &cfg, dir.path());

        let app2 = MementoApp::new().unwrap();
        assert!(!app2.get_sub_drawer_open());
        assert!(!app2.get_dict_search_open());
        restore_window_panel_state(&app2, &cfg);
        assert!(app2.get_sub_drawer_open());
        assert!(app2.get_dict_search_open());

        assert!(should_resume_on_subtitle_unhover(&cfg));
        cfg.edit_setting("search/hover-pause-resume", "false")
            .unwrap();
        assert!(!should_resume_on_subtitle_unhover(&cfg));
    }

    #[test]
    fn copy_subtitle_text_clear_audio_context_and_ocr_cancel_handlers_work() {
        i_slint_backend_testing::init_no_event_loop();
        let app = MementoApp::new().unwrap();
        let dir = tempfile::TempDir::new().unwrap();
        let cfg = Rc::new(ConfigController::new());
        let mpv: Rc<RefCell<Option<MpvInstance>>> = Rc::new(RefCell::new(None));
        let coord: Rc<RefCell<Option<LivePlayerCoordinator>>> = Rc::new(RefCell::new(None));
        let sub_ctrl = Rc::new(RefCell::new(SubtitleController::new()));
        let sub_model = Rc::new(VecModel::<SubEntry>::default());
        let sec_model = Rc::new(VecModel::<SubEntry>::default());
        let current_media = Rc::new(RefCell::new(None));
        let media_gen = Rc::new(Cell::new(0u64));
        let shared_gen = Arc::new(AtomicU64::new(0));
        let asr = Rc::new(RefCell::new(AsrController::new()));
        let asr_restart = Rc::new(Cell::new(false));
        let ass = Rc::new(RefCell::new(AssOverlayController::new()));
        let screensaver = Rc::new(RefCell::new(memento_os::Screensaver::new()));
        let closing = Rc::new(Cell::new(false));
        let (tx, _rx) = std::sync::mpsc::channel();
        let thumb_tx = ChannelSlot::new(tx);
        let ui_relay = UiRelay::new();
        let audio_marks = Rc::new(RefCell::new(AudioContextMarkState::default()));

        let _handles = install_playback_handlers(
            &app,
            mpv,
            coord,
            sub_ctrl,
            sub_model,
            sec_model,
            cfg,
            dir.path().to_path_buf(),
            current_media,
            media_gen,
            shared_gen,
            asr,
            asr_restart,
            ass,
            screensaver,
            closing,
            thumb_tx,
            ui_relay,
            audio_marks.clone(),
            None,
            Vec::new(),
            false,
            #[cfg(feature = "ocr")]
            Err("disabled in test".to_owned()),
        );

        // M2: copy subtitle text on double-click
        app.invoke_copy_subtitle_text("  日本語の字幕  ".into());
        assert_eq!(app.get_copied_subtitle_text().as_str(), "日本語の字幕");

        // H5.2: clear audio context resets marks + HUD text
        let hud = audio_marks.borrow_mut().mark_at(10.0);
        app.set_audio_context_hud_text(hud.into());
        assert!(!app.get_audio_context_hud_text().is_empty());
        app.invoke_clear_audio_context();
        assert_eq!(app.get_audio_context_hud_text().as_str(), "");
        assert_eq!(*audio_marks.borrow(), AudioContextMarkState::Empty);

        // H3.6: ocr_cancelled exits ocr_mode and updates status
        app.set_ocr_mode(true);
        app.invoke_ocr_cancelled();
        assert!(!app.get_ocr_mode());
        assert_eq!(app.get_status_message().as_str(), "OCR cancelled");

        // LOW: select_video_track updates video_id cleanly
        app.invoke_select_video_track(2);
        assert_eq!(app.get_video_id(), 2);
    }
}

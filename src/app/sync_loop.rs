//! Event-driven playback synchronization loop, coalesced mpv wakeups,
//! cached MKV extraction & furigana ratio checks, monitor-aware auto-fit,
//! and 200ms housekeeping timer.

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use slint::{ComponentHandle, Model as _, VecModel};

use crate::app::paths::memento_xdg_dir;
use crate::app::playback::persist_window_panel_state;
use crate::app::preferences::{
    cached_furigana_ratio, record_recent_media, record_recent_media_title,
    refresh_player_preferences, refresh_recent_files,
};
use crate::app::subtitle_drawer::{
    auto_scroll_subtitle_drawer_if_needed, refresh_secondary_subtitle_list_with_timing,
    refresh_subtitle_list_with_timing, rerun_active_subtitle_search_silently,
};
use crate::app::video_pipeline::{SyncTiming, VideoParamsObserver, VideoTimings};
use crate::app::window_chrome::{chapter_marks_for_duration, query_window_monitor_logical};
use crate::app::workers::{ChannelSlot, MkvExtractRequest, UiDispatchGuard, UiEvent, UiRelay};
use crate::controller::asr::{AsrController, AsrSettings};
use crate::controller::ass::{
    is_matroska_path, mpv_ass_params_from_playback, AssOverlayController,
};
use crate::controller::config::ConfigController;
use crate::controller::player::{is_image_subtitle_codec, track_label, LivePlayerCoordinator};
use crate::controller::subtitles::{
    create_cue, prewarm_subtitle_furigana, ExternalSubtitleLoad, SubtitleController,
};
use crate::platform_renderer::DisplayServer;
use crate::platform_window::compute_auto_fit_window_rect;
use crate::ui::{
    active_subtitle_style_for_app, prewarm_subtitle_tokens, publish_active_subtitle,
    subtitle_lines_for_app, MementoApp, PlayerTrack, SubEntry, SubtitleOverlayBridge,
};
use memento_player::{MpvInstance, VideoTrack};

fn update_subtitle_bounds_properties(app: &MementoApp, player: &MpvInstance) {
    let ass_native = app
        .global::<SubtitleOverlayBridge>()
        .get_ass_native_active();
    let (can_inc, can_dec, can_up, can_down) = if ass_native {
        let sub_scale = player.sub_scale();
        let sub_pos = player.sub_pos();
        (
            sub_scale < 3.0 - 0.001,
            sub_scale > 0.2 + 0.001,
            sub_pos > 0.0 + 0.1,
            sub_pos < 150.0 - 0.1,
        )
    } else {
        let scale = app.get_subtitle_scale();
        let offset = app.get_subtitle_offset();
        (
            scale < 1.0 - 0.0005,
            scale > 0.001 + 0.0005,
            offset < 1.0 - 0.0005,
            offset > 0.0 + 0.0005,
        )
    };
    if app.get_can_increase_sub_size() != can_inc {
        app.set_can_increase_sub_size(can_inc);
    }
    if app.get_can_decrease_sub_size() != can_dec {
        app.set_can_decrease_sub_size(can_dec);
    }
    if app.get_can_move_sub_up() != can_up {
        app.set_can_move_sub_up(can_up);
    }
    if app.get_can_move_sub_down() != can_down {
        app.set_can_move_sub_down(can_down);
    }
}

/// Auto-clear threshold in 200 ms housekeeping ticks (`7` ≈ 1.4–1.6 s + 150 ms fade).
const STATUS_MESSAGE_CLEAR_TICKS: usize = 7;

pub(crate) struct SyncLoopHandles {
    pub(crate) housekeeping_timer: slint::Timer,
    pub(crate) _dispatch_guard: UiDispatchGuard,
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn install_sync_loop(
    app: &MementoApp,
    mpv: Rc<RefCell<Option<MpvInstance>>>,
    coordinator: Rc<RefCell<Option<LivePlayerCoordinator>>>,
    sub_ctrl: Rc<RefCell<SubtitleController>>,
    sub_model: Rc<VecModel<SubEntry>>,
    secondary_sub_model: Rc<VecModel<SubEntry>>,
    media_generation: Rc<Cell<u64>>,
    shared_media_generation: Arc<AtomicU64>,
    current_media: Rc<RefCell<Option<String>>>,
    lookup_epoch: Arc<AtomicU64>,
    asr_session: Rc<RefCell<AsrController>>,
    config_ctrl: Rc<ConfigController>,
    config_dir: PathBuf,
    asr_restart_pending: Rc<Cell<bool>>,
    closing: Rc<Cell<bool>>,
    hover_pause_owned: Rc<Cell<bool>>,
    furigana_tx: ChannelSlot<(u64, String)>,
    furigana_epoch: Arc<AtomicU64>,
    video_timings: Rc<RefCell<VideoTimings>>,
    pending_playback_frame: Rc<Cell<Option<i32>>>,
    ext_sub_tx: ChannelSlot<(u64, ExternalSubtitleLoad)>,
    mkv_tx: ChannelSlot<MkvExtractRequest>,
    ass_ctrl: Rc<RefCell<AssOverlayController>>,
    screensaver: Rc<RefCell<memento_os::Screensaver>>,
    ui_relay: UiRelay,
    handle_open_or_drop_paths: Rc<dyn Fn(Vec<PathBuf>)>,
    handle_add_subtitle_paths: Rc<dyn Fn(Vec<PathBuf>)>,
    gl_prefer_16f: Rc<Cell<bool>>,
    default_prefer_16f: bool,
    video_params_observer: Rc<RefCell<VideoParamsObserver>>,
    verbose_video_timings: bool,
) -> SyncLoopHandles {
    let run_player_sync: Rc<dyn Fn()> = {
        let sync_app = app.as_weak();
        let sync_mpv = mpv.clone();
        let sync_coord = coordinator.clone();
        let sync_sub_ctrl = sub_ctrl.clone();
        let sync_sub_model = sub_model.clone();
        let sync_sec_sub_model = secondary_sub_model.clone();
        let sync_media_generation = media_generation.clone();
        let sync_shared_gen = shared_media_generation.clone();
        let sync_current_media = current_media;
        let sync_lookup_epoch = lookup_epoch;
        let sync_asr = asr_session.clone();
        let sync_asr_config = config_ctrl.clone();
        let sync_config_dir = config_dir;
        let sync_asr_restart = asr_restart_pending.clone();
        let sync_closing = closing.clone();
        let sync_hover_owned = hover_pause_owned;
        let sync_furigana_tx = furigana_tx;
        let sync_furigana_epoch = furigana_epoch;
        let sync_video_timings = video_timings;
        let sync_pending_frame = pending_playback_frame;
        let sync_ext_sub_tx = ext_sub_tx;
        let sync_mkv_tx = mkv_tx;
        let sync_ass_ctrl = ass_ctrl.clone();
        let sync_screensaver = screensaver;
        let sync_gl_prefer_16f = gl_prefer_16f;
        let sync_video_params = video_params_observer;
        let extracted_mkv_fonts_gen: Cell<Option<u64>> = Cell::new(None);
        let extracted_mkv_tracks: RefCell<HashSet<(u64, i64)>> = RefCell::new(HashSet::new());
        // Cached `(gen, is_local_mkv)` so `media_path.is_file()` (`stat()`) runs AT MOST ONCE per file!
        let mkv_file_probe: Cell<Option<(u64, bool)>> = Cell::new(None);
        // Cached `(gen, primary_sid, secondary_sid, track_count)` to skip MKV work on steady ticks.
        let last_mkv_trigger_key: Cell<Option<(u64, i64, i64, usize)>> = Cell::new(None);
        // Cached `(gen, video_id)` for one-shot SDR vs 10-bit/HDR FBO format updates.
        let last_video_fbo_key: Cell<Option<(u64, i64)>> = Cell::new(None);
        let previous_furigana_visible = Cell::new(app.get_furigana_visible());
        let previous_subtitle_layout: Cell<Option<(i32, i32, u32, bool)>> = Cell::new(None);
        let previous_chapters: RefCell<Vec<f64>> = RefCell::new(Vec::new());
        let previous_chapters_dur = Cell::new(0.0f64);
        let previous_title_pair: RefCell<(String, String)> =
            RefCell::new((String::new(), String::new()));
        let last_recorded_recent: RefCell<(u64, String)> = RefCell::new((0, String::new()));
        let previous_video_tracks: RefCell<Vec<VideoTrack>> = RefCell::new(Vec::new());
        let previous_primary_timing: Cell<(u64, u64)> =
            Cell::new((1.0f64.to_bits(), 0.0f64.to_bits()));
        let previous_sec_state: Cell<(i64, usize, Option<usize>, u64, u64)> =
            Cell::new((0, 0, None, 1.0f64.to_bits(), 0.0f64.to_bits()));
        let previous_cue_counts: Cell<(usize, usize)> = Cell::new((0, 0));
        let previous_playback_active: Cell<Option<bool>> = Cell::new(None);
        let previous_active_scroll: Cell<(bool, i32, Option<usize>, Option<usize>)> =
            Cell::new((false, 0, None, None));
        let previous_prewarm_key: Cell<Option<(u64, i64, usize, i32, u32, bool)>> = Cell::new(None);
        let last_video_report = Cell::new(Instant::now());
        let previous_drops = Cell::new((0i64, 0i64));
        let last_sync = Cell::new(Instant::now());
        let last_autofit_generation: Cell<Option<u64>> = Cell::new(None);

        Rc::new(move || {
            if sync_closing.get() {
                return;
            }
            let sync_started = Instant::now();
            {
                let mut timings = sync_video_timings.borrow_mut();
                timings.sync_gap_max = timings
                    .sync_gap_max
                    .max(sync_started.duration_since(last_sync.get()));
            }
            last_sync.set(sync_started);
            let _sync_timing = SyncTiming {
                started: sync_started,
                timings: sync_video_timings.clone(),
            };
            let Some(app) = sync_app.upgrade() else {
                return;
            };
            let (Ok(player_ref), Ok(mut coord_ref), Ok(mut subtitles), Ok(mut asr)) = (
                sync_mpv.try_borrow(),
                sync_coord.try_borrow_mut(),
                sync_sub_ctrl.try_borrow_mut(),
                sync_asr.try_borrow_mut(),
            ) else {
                return;
            };
            let (Some(player), Some(coord)) = (player_ref.as_ref(), coord_ref.as_mut()) else {
                return;
            };

            let gen_before = sync_media_generation.get();
            let external_loads = coord.sync(
                &app,
                player,
                &mut subtitles,
                &sync_sub_model,
                &mut asr,
                &sync_media_generation,
                &sync_current_media,
                &sync_asr_restart,
                &sync_hover_owned,
                || {
                    let config_dir = memento_xdg_dir("XDG_CONFIG_HOME", ".config")
                        .unwrap_or_else(|| PathBuf::from("."));
                    AsrSettings::from_config(&config_dir, &*sync_asr_config.store())
                },
                |app, primary_text| {
                    sync_lookup_epoch.fetch_add(1, Ordering::AcqRel);
                    sync_furigana_tx.with_sender(|tx| {
                        publish_active_subtitle(app, primary_text, tx, &sync_furigana_epoch);
                    });
                },
            );

            // C5.1: Quit cleanly on `MpvEvent::Shutdown` (e.g. `q`/`Q` or `quit` via mpv).
            if coord.take_shutdown_requested() {
                drop(coord_ref);
                drop(player_ref);
                drop(subtitles);
                drop(asr);
                persist_window_panel_state(&app, &sync_asr_config, &sync_config_dir);
                app.invoke_quit_app();
                if !sync_closing.get() {
                    let _ = slint::quit_event_loop();
                }
                return;
            }

            // H3.1: Consume any external mpv `fullscreen` change to drive after releasing borrows.
            let mpv_fullscreen_change = coord.take_mpv_fullscreen_change();

            let gen_after = sync_media_generation.get();
            let media_changed = gen_after != gen_before;
            if media_changed {
                sync_shared_gen.store(gen_after, Ordering::Release);
                extracted_mkv_fonts_gen.set(None);
                extracted_mkv_tracks.borrow_mut().clear();
                mkv_file_probe.set(None);
                last_mkv_trigger_key.set(None);
                last_video_fbo_key.set(None);
                previous_prewarm_key.set(None);
                if let Ok(mut ass) = sync_ass_ctrl.try_borrow_mut() {
                    ass.clear_for_new_media(&app);
                }
            }
            for load in external_loads {
                let _ = sync_ext_sub_tx.send((gen_after, load));
            }

            let primary_is_native_ass = coord.primary_is_native_ass(&app);
            let state = coord.playback_state();

            // LOW: Sync video tracks & selected video track ID for MenuBar Video submenu.
            {
                let mut prev_vid = previous_video_tracks.borrow_mut();
                if *prev_vid != state.video_tracks {
                    let ui_tracks: Vec<PlayerTrack> = state
                        .video_tracks
                        .iter()
                        .filter_map(|track| {
                            Some(PlayerTrack {
                                id: i32::try_from(track.id).ok()?,
                                label: track_label(track.id, &track.language, &track.title).into(),
                                selected: track.selected,
                            })
                        })
                        .collect();
                    app.set_video_tracks(Rc::new(VecModel::from(ui_tracks)).into());
                    prev_vid.clone_from(&state.video_tracks);
                }
            }
            let ui_vid = i32::try_from(state.video_id).unwrap_or(0);
            if app.get_video_id() != ui_vid {
                app.set_video_id(ui_vid);
            }

            // M3: Record opened URLs, CLI URLs, and playlist auto-advances into `recent/files`
            // on `FileLoaded` (deduplicated per `(gen_after, state.path)`).
            if state.file_loaded && !state.path.is_empty() {
                let is_new_loaded_path = {
                    let mut prev_recent = last_recorded_recent.borrow_mut();
                    if prev_recent.0 != gen_after || prev_recent.1 != state.path {
                        *prev_recent = (gen_after, state.path.clone());
                        true
                    } else {
                        false
                    }
                };
                if is_new_loaded_path {
                    if !state.media_title.is_empty() {
                        record_recent_media_title(&state.path, &state.media_title);
                    }
                    if let Err(error) =
                        record_recent_media(&app, &sync_asr_config, &sync_config_dir, &state.path)
                    {
                        eprintln!("Cannot save recent media: {error}");
                    }
                }
            }

            // Drain non-blocking `video-params/*` observer and switch OpenGL FBO format
            // between `GL_RGBA8` (8-bit SDR: half VRAM & bandwidth) and `GL_RGBA16F`
            // (10-bit / HDR) only on file load or video track/format changes.
            if let Ok(mut obs) = sync_video_params.try_borrow_mut() {
                obs.poll();
                if obs.is_active() {
                    if state.file_loaded && (state.dimensions.0 > 0 || state.video_width > 0) {
                        let fbo_key = (gen_after, state.video_id);
                        let key_changed = last_video_fbo_key.get() != Some(fbo_key);
                        let params_changed = obs.take_dirty();
                        if key_changed || params_changed {
                            last_video_fbo_key.set(Some(fbo_key));
                            let want_16f = obs.prefers_16f();
                            if sync_gl_prefer_16f.replace(want_16f) != want_16f {
                                app.window().request_redraw();
                            }
                        }
                    } else if !state.file_loaded && sync_current_media.borrow().is_none() {
                        last_video_fbo_key.set(None);
                        let _ = obs.take_dirty();
                        if sync_gl_prefer_16f.replace(default_prefer_16f) != default_prefer_16f {
                            app.window().request_redraw();
                        }
                    }
                }
            }

            // Trigger non-blocking MKV EBML extraction ONLY when `(gen, primary_sid, secondary_sid, track_count)`
            // changes, caching the `is_matroska_path && is_file()` check once per generation (zero per-tick `stat()`).
            if state.file_loaded && !state.path.is_empty() {
                let trigger_key = (
                    gen_after,
                    state.primary_sid,
                    state.secondary_sid,
                    state.subtitle_tracks.len(),
                );
                if last_mkv_trigger_key.get() != Some(trigger_key) {
                    last_mkv_trigger_key.set(Some(trigger_key));
                    let is_local_mkv = match mkv_file_probe.get() {
                        Some((g, cached)) if g == gen_after => cached,
                        _ => {
                            let media_path = Path::new(&state.path);
                            let ok = is_matroska_path(media_path) && media_path.is_file();
                            mkv_file_probe.set(Some((gen_after, ok)));
                            ok
                        }
                    };
                    if is_local_mkv {
                        let media_path = Path::new(&state.path);
                        if extracted_mkv_fonts_gen.get() != Some(gen_after) {
                            extracted_mkv_fonts_gen.set(Some(gen_after));
                            let _ = sync_mkv_tx.send(MkvExtractRequest::Fonts {
                                generation: gen_after,
                                path: media_path.to_path_buf(),
                            });
                        }
                        let asr_sid = subtitles.asr_mpv_sid().or_else(|| coord.asr_track().sid());
                        let mut done_tracks = extracted_mkv_tracks.borrow_mut();
                        for sid in [state.primary_sid, state.secondary_sid] {
                            if sid <= 0
                                || Some(sid) == asr_sid
                                || done_tracks.contains(&(gen_after, sid))
                            {
                                continue;
                            }
                            if let Some(track) = state.subtitle_tracks.iter().find(|t| t.id == sid)
                            {
                                if !track.external
                                    && track.src_id > 0
                                    && !is_image_subtitle_codec(&track.codec)
                                {
                                    done_tracks.insert((gen_after, sid));
                                    let _ = sync_mkv_tx.send(MkvExtractRequest::Track {
                                        generation: gen_after,
                                        sid,
                                        track_number: track.src_id as u64,
                                        path: media_path.to_path_buf(),
                                    });
                                }
                            }
                        }
                    }
                }
            }

            // Synchronize native ASS/SSA interactive hitboxes, highlight patches, and optional furigana.
            if let Ok(mut ass) = sync_ass_ctrl.try_borrow_mut() {
                let fallback_dims = if state.dimensions.0 > 0 && state.dimensions.1 > 0 {
                    state.dimensions
                } else {
                    (
                        app.get_video_width().max(1) as u32,
                        app.get_video_height().max(1) as u32,
                    )
                };
                let ass_params = mpv_ass_params_from_playback(state, fallback_dims);
                let raw_sub_s =
                    (state.position - state.sub_delay) / state.sub_speed.max(f64::EPSILON);
                let now_ms = (raw_sub_s.max(0.0) * 1000.0).round() as i64;
                let furigana_ratio = cached_furigana_ratio();
                ass.sync_from_playback(
                    &app,
                    primary_is_native_ass,
                    &state.sub_ass_extradata,
                    &state.sub_text_ass_full,
                    &ass_params,
                    now_ms,
                    state.paused,
                    furigana_ratio,
                );
            }

            // LOW: Keep MenuBar subtitle size/position bounds properties in sync (ASS & non-ASS).
            update_subtitle_bounds_properties(&app, player);

            // H3.5: Auto-fit window size & centering on newly loaded media using real monitor
            // bounds (`1..=1000` percent, positioning guarded on non-Wayland).
            if state.file_loaded && last_autofit_generation.get() != Some(gen_after) {
                let (vw, vh) = if state.dimensions.0 > 0 && state.dimensions.1 > 0 {
                    state.dimensions
                } else if state.video_width > 0 && state.video_height > 0 {
                    (state.video_width as u32, state.video_height as u32)
                } else {
                    (0, 0)
                };
                if vw > 0 && vh > 0 {
                    last_autofit_generation.set(Some(gen_after));
                    if app.get_autofit_enabled() {
                        let sidebar_w = if app.get_dict_search_open() || app.get_sub_drawer_open() {
                            app.get_sidebar_width().max(0.0).round() as u32
                        } else {
                            0
                        };
                        let percent = app.get_autofit_percent().clamp(1, 1000) as u32;
                        let monitor = query_window_monitor_logical(app.window());
                        if let Some(rect) = compute_auto_fit_window_rect(
                            vw,
                            vh,
                            sidebar_w,
                            percent,
                            monitor,
                            app.window().is_fullscreen() || app.get_is_fullscreen(),
                            app.window().is_maximized(),
                        ) {
                            app.window().set_size(slint::LogicalSize::new(
                                rect.width as f32,
                                rect.height as f32,
                            ));
                            if !DisplayServer::detect_current().is_wayland() {
                                app.window().set_position(slint::LogicalPosition::new(
                                    rect.x as f32,
                                    rect.y as f32,
                                ));
                            }
                        }
                    }
                }
            }

            if app.get_is_muted() != state.mute {
                app.set_is_muted(state.mute);
            }
            let effective_vol_max = (state.volume_max.max(100) as f32).clamp(100.0, 1000.0);
            if (app.get_volume_max() - effective_vol_max).abs() > 0.5 {
                app.set_volume_max(effective_vol_max);
            }
            let playback_active = state.file_loaded && !state.paused;
            if previous_playback_active.replace(Some(playback_active)) != Some(playback_active) {
                if let Ok(mut ss) = sync_screensaver.try_borrow_mut() {
                    ss.sync_playback(state.file_loaded, state.paused, "Playing media");
                }
            }
            let prim_sid = subtitles.primary_sid();
            let prim_active = subtitles.active_indices(prim_sid).first().copied();
            let primary_timing = (state.sub_speed.to_bits(), state.sub_delay.to_bits());
            if previous_primary_timing.replace(primary_timing) != primary_timing {
                refresh_subtitle_list_with_timing(
                    &app,
                    &sync_sub_model,
                    &subtitles,
                    state.sub_speed,
                    state.sub_delay,
                );
            }
            let subtitle_overlay_shown = app.get_subtitles_visible()
                && !primary_is_native_ass
                && !(app.get_hide_memento_while_playing() && !state.paused);
            if subtitle_overlay_shown && app.get_video_height() > 0 {
                if let Some(active_idx) = prim_active {
                    let prewarm_key = (
                        gen_after,
                        prim_sid,
                        active_idx,
                        app.get_video_height(),
                        app.get_subtitle_scale().to_bits(),
                        app.get_furigana_visible(),
                    );
                    if previous_prewarm_key.get() != Some(prewarm_key) {
                        previous_prewarm_key.set(Some(prewarm_key));
                        let upcoming: Vec<String> = ((active_idx + 1)..=(active_idx + 3))
                            .filter_map(|idx| subtitles.cue_text(idx))
                            .filter(|line| !line.trim().is_empty())
                            .collect();
                        if !upcoming.is_empty() {
                            let font_px =
                                (app.get_video_height() as f32 * app.get_subtitle_scale()).max(8.0);
                            let style = active_subtitle_style_for_app(&app);
                            prewarm_subtitle_tokens(
                                upcoming,
                                font_px,
                                app.get_furigana_visible(),
                                style,
                            );
                        }
                    }
                }
            }
            let sec_sid = subtitles.secondary_sid();
            let sec_count = subtitles.secondary_entries().len();
            let has_secondary = state.secondary_sid > 0 || sec_count > 0;
            if app.get_has_secondary_subtitles() != has_secondary {
                app.set_has_secondary_subtitles(has_secondary);
            }
            let sec_active = subtitles.active_indices(sec_sid).first().copied();
            let sec_state = (
                sec_sid,
                sec_count,
                sec_active,
                state.sub_speed.to_bits(),
                state.secondary_sub_delay.to_bits(),
            );
            if previous_sec_state.replace(sec_state) != sec_state {
                refresh_secondary_subtitle_list_with_timing(
                    &app,
                    &sync_sec_sub_model,
                    &subtitles,
                    state.sub_speed,
                    state.secondary_sub_delay,
                );
            }
            let cue_counts = (subtitles.primary_entries().len(), sec_count);
            let need_live_search_refresh = previous_cue_counts.replace(cue_counts) != cue_counts
                && !app.get_subtitle_search_query().trim().is_empty();
            let scroll_state = (
                app.get_sub_drawer_open(),
                app.get_subtitle_drawer_tab(),
                prim_active,
                sec_active,
            );
            if previous_active_scroll.replace(scroll_state) != scroll_state {
                auto_scroll_subtitle_drawer_if_needed(&app, &subtitles);
            }
            if !state.path.is_empty() && !state.media_title.is_empty() {
                let mut prev_pair = previous_title_pair.borrow_mut();
                if prev_pair.0 != state.path || prev_pair.1 != state.media_title {
                    *prev_pair = (state.path.clone(), state.media_title.clone());
                    drop(prev_pair);
                    record_recent_media_title(&state.path, &state.media_title);
                    refresh_recent_files(&app, &sync_asr_config);
                }
            }
            {
                let mut prev_ch = previous_chapters.borrow_mut();
                if *prev_ch != state.chapters
                    || (previous_chapters_dur.get() - state.duration).abs() > 0.5
                {
                    *prev_ch = state.chapters.clone();
                    previous_chapters_dur.set(state.duration);
                    let raw_ch: Vec<f32> = state.chapters.iter().map(|&t| t as f32).collect();
                    let marks = chapter_marks_for_duration(&state.chapters, state.duration);
                    app.set_chapters(Rc::new(VecModel::from(raw_ch)).into());
                    app.set_chapter_marks(Rc::new(VecModel::from(marks)).into());
                }
            }

            if app.get_pending_playback_generation() >= 0 && state.playback_ready {
                if player.is_opengl_active() {
                    sync_pending_frame.set(Some(app.get_pending_playback_generation()));
                    app.window().request_redraw();
                } else {
                    app.set_pending_playback_generation(-1);
                    let _ = player.play();
                }
            }
            if last_video_report.get().elapsed() >= Duration::from_secs(5) {
                let drops = (state.frame_drops, state.decoder_drops);
                sync_video_timings.borrow_mut().report(
                    state,
                    drops != previous_drops.get(),
                    verbose_video_timings,
                    player.pending_render_delay(),
                );
                previous_drops.set(drops);
                last_video_report.set(Instant::now());
            }

            if previous_furigana_visible.get() != app.get_furigana_visible() {
                previous_furigana_visible.set(app.get_furigana_visible());
                sync_furigana_tx.with_sender(|tx| {
                    publish_active_subtitle(
                        &app,
                        &app.get_active_sub_text(),
                        tx,
                        &sync_furigana_epoch,
                    );
                });
            }
            let subtitle_layout = (
                app.get_video_width(),
                app.get_video_height(),
                app.get_subtitle_scale().to_bits(),
                app.get_furigana_visible(),
            );
            if previous_subtitle_layout.get() != Some(subtitle_layout) {
                previous_subtitle_layout.set(Some(subtitle_layout));
                let words: Vec<_> = app.get_active_sub_words().iter().collect();
                if !words.is_empty() {
                    let lines = subtitle_lines_for_app(&app, words);
                    app.set_active_sub_lines(Rc::new(VecModel::from(lines)).into());
                }
            }

            // Release all `RefCell` borrows BEFORE invoking re-entrant UI callbacks.
            drop(coord_ref);
            drop(player_ref);
            drop(subtitles);
            drop(asr);

            if media_changed {
                app.invoke_clear_audio_context();
            }
            if need_live_search_refresh {
                rerun_active_subtitle_search_silently(&app);
            }
            if let Some(target_fs) = mpv_fullscreen_change {
                let current_fs = app.window().is_fullscreen() || app.get_is_fullscreen();
                if target_fs != current_fs {
                    app.invoke_toggle_fullscreen();
                }
            }
        })
    };

    // ── Single Coalesced UI Relay Dispatcher ────────────────────────────────
    let dispatch_relay = ui_relay.clone();
    let dispatch_app = app.as_weak();
    let dispatch_ui_gen = media_generation;
    let dispatch_subs = sub_ctrl;
    let dispatch_model = sub_model;
    let dispatch_sec_model = secondary_sub_model;
    let dispatch_coord = coordinator;
    let dispatch_ass = ass_ctrl;
    let dispatch_config = config_ctrl;
    let dispatch_open_paths = handle_open_or_drop_paths;
    let dispatch_add_subtitles = handle_add_subtitle_paths;
    let dispatch_sync = run_player_sync.clone();
    let dispatch_guard = UiDispatchGuard::install(Rc::new(move || {
        // Clear coalesced wakeup flag BEFORE draining so concurrent wakeups re-arm cleanly.
        dispatch_relay.clear_wakeup_flag();
        let events = dispatch_relay.drain_events();
        let mut need_sync = dispatch_relay.take_mpv_sync_needed();

        for event in events {
            match event {
                UiEvent::OpenPaths(paths) => {
                    dispatch_open_paths(paths);
                }
                UiEvent::AddSubtitles(paths) => {
                    dispatch_add_subtitles(paths);
                }
                UiEvent::SystemThemeChanged(_scheme) => {
                    if let Some(app) = dispatch_app.upgrade() {
                        refresh_player_preferences(&app, &dispatch_config);
                    }
                }
                UiEvent::DictionariesImported {
                    imported,
                    errors,
                    all_dicts,
                } => {
                    if let Some(app) = dispatch_app.upgrade() {
                        if !all_dicts.is_empty() {
                            app.set_no_dicts_dialog_open(false);
                        }
                        let msg = if errors.is_empty() {
                            format!("Imported {imported} dictionary archive(s)")
                        } else if imported > 0 {
                            format!(
                                "Imported {imported} dictionary archive(s); failed: {}",
                                errors.join("; ")
                            )
                        } else {
                            format!("Dictionary import failed: {}", errors.join("; "))
                        };
                        app.set_status_message(msg.into());
                    }
                }
                UiEvent::ExternalSubtitle {
                    generation,
                    track_id,
                    path,
                    result,
                } => {
                    let Some(app) = dispatch_app.upgrade() else {
                        continue;
                    };
                    if dispatch_ui_gen.get() != generation {
                        continue;
                    }
                    let (sub_speed, sub_delay, secondary_sub_delay) = {
                        let Ok(mut subtitles) = dispatch_subs.try_borrow_mut() else {
                            continue;
                        };
                        match result {
                            Ok(cues) => {
                                if app.get_furigana_visible() {
                                    let lines: Vec<String> = cues
                                        .iter()
                                        .take(2048)
                                        .map(|cue| String::from_utf16_lossy(&cue.text))
                                        .collect();
                                    prewarm_subtitle_furigana(lines);
                                }
                                subtitles.set_external_cues(track_id, &path, cues);
                            }
                            Err(error) => {
                                subtitles.set_external_cues(track_id, &path, Vec::new());
                                app.set_status_message(error.into());
                            }
                        }
                        let timing = dispatch_coord
                            .try_borrow()
                            .ok()
                            .and_then(|guard| {
                                guard.as_ref().map(|coord| {
                                    let st = coord.playback_state();
                                    let speed = st.sub_speed.max(f64::EPSILON);
                                    let raw_primary = (st.position - st.sub_delay) / speed;
                                    let raw_secondary =
                                        (st.position - st.secondary_sub_delay) / speed;
                                    subtitles.update_positions(raw_primary, raw_secondary);
                                    (st.sub_speed, st.sub_delay, st.secondary_sub_delay)
                                })
                            })
                            .unwrap_or((1.0, 0.0, 0.0));
                        refresh_subtitle_list_with_timing(
                            &app,
                            &dispatch_model,
                            &subtitles,
                            timing.0,
                            timing.1,
                        );
                        refresh_secondary_subtitle_list_with_timing(
                            &app,
                            &dispatch_sec_model,
                            &subtitles,
                            timing.0,
                            timing.2,
                        );
                        timing
                    };
                    let _ = (sub_speed, sub_delay, secondary_sub_delay);
                    rerun_active_subtitle_search_silently(&app);
                }
                UiEvent::MkvFonts { generation, fonts } => {
                    if dispatch_ui_gen.get() != generation {
                        continue;
                    }
                    if let Ok(mut ass) = dispatch_ass.try_borrow_mut() {
                        ass.set_memory_fonts(fonts);
                        need_sync = true;
                    }
                }
                UiEvent::MkvTrack {
                    generation,
                    sid,
                    cues,
                } => {
                    let Some(app) = dispatch_app.upgrade() else {
                        continue;
                    };
                    if dispatch_ui_gen.get() != generation {
                        continue;
                    }
                    let updated = {
                        let Ok(mut subtitles) = dispatch_subs.try_borrow_mut() else {
                            continue;
                        };
                        if subtitles
                            .track(sid)
                            .is_some_and(|t| t.is_external() || t.is_asr_track())
                        {
                            false
                        } else {
                            let entries: Vec<memento_subtitle::SubtitleEntry> = cues
                                .into_iter()
                                .filter_map(|cue| {
                                    if cue.plain_text.trim().is_empty()
                                        || cue.end_ms <= cue.start_ms
                                    {
                                        return None;
                                    }
                                    Some(create_cue(
                                        &cue.plain_text,
                                        cue.start_ms as f64 / 1000.0,
                                        cue.end_ms as f64 / 1000.0,
                                    ))
                                })
                                .collect();
                            if app.get_furigana_visible() {
                                let lines: Vec<String> = entries
                                    .iter()
                                    .take(2048)
                                    .map(|e| String::from_utf16_lossy(&e.text))
                                    .collect();
                                prewarm_subtitle_furigana(lines);
                            }
                            subtitles.ensure_track(sid);
                            subtitles.set_track_cues(sid, entries);
                            let timing = dispatch_coord
                                .try_borrow()
                                .ok()
                                .and_then(|guard| {
                                    guard.as_ref().map(|coord| {
                                        let st = coord.playback_state();
                                        let speed = st.sub_speed.max(f64::EPSILON);
                                        let raw_primary = (st.position - st.sub_delay) / speed;
                                        let raw_secondary =
                                            (st.position - st.secondary_sub_delay) / speed;
                                        subtitles.update_positions(raw_primary, raw_secondary);
                                        (st.sub_speed, st.sub_delay, st.secondary_sub_delay)
                                    })
                                })
                                .unwrap_or((1.0, 0.0, 0.0));
                            refresh_subtitle_list_with_timing(
                                &app,
                                &dispatch_model,
                                &subtitles,
                                timing.0,
                                timing.1,
                            );
                            refresh_secondary_subtitle_list_with_timing(
                                &app,
                                &dispatch_sec_model,
                                &subtitles,
                                timing.0,
                                timing.2,
                            );
                            true
                        }
                    };
                    if updated {
                        rerun_active_subtitle_search_silently(&app);
                    }
                }
            }
        }

        if need_sync {
            dispatch_sync();
        }
    }));

    // Coalesced mpv wakeup callback via `UiRelay::request_mpv_sync` (`AtomicBool` coalescer).
    if let Some(player) = mpv.borrow().as_ref() {
        let wakeup_relay = ui_relay;
        player.set_wakeup_callback(move || {
            wakeup_relay.request_mpv_sync();
        });
    }

    // Lightweight 200ms fallback/housekeeping tick: expires transient status
    // messages and drains background ASR events or active playback without
    // redundant property dirtying when paused & idle.
    let housekeeping_timer = slint::Timer::default();
    let timer_app = app.as_weak();
    let timer_mpv = mpv;
    let timer_asr = asr_session;
    let timer_asr_restart = asr_restart_pending;
    let timer_closing = closing;
    let timer_sync = run_player_sync;
    let mut status_message_ticks = 0usize;
    let mut last_observed_status = slint::SharedString::default();
    let mut last_ui_flags = (
        app.get_subtitles_visible(),
        app.get_hide_mpv_subtitles(),
        app.get_hide_memento_while_playing(),
        app.get_auto_pause(),
        app.get_furigana_visible(),
        app.get_video_width(),
        app.get_video_height(),
        app.get_subtitle_scale().to_bits(),
        app.get_search_remove_regex(),
        app.get_search_replace_lines(),
        app.get_search_replace_with(),
    );
    housekeeping_timer.start(
        slint::TimerMode::Repeated,
        Duration::from_millis(200),
        move || {
            if timer_closing.get() {
                return;
            }
            let Some(app) = timer_app.upgrade() else {
                return;
            };
            let current_status = app.get_status_message();
            if !current_status.is_empty() {
                if current_status != last_observed_status {
                    last_observed_status = current_status;
                    status_message_ticks = 0;
                } else {
                    status_message_ticks += 1;
                    if status_message_ticks >= STATUS_MESSAGE_CLEAR_TICKS {
                        app.set_status_message("".into());
                        last_observed_status = "".into();
                        status_message_ticks = 0;
                    }
                }
            } else {
                last_observed_status = "".into();
                status_message_ticks = 0;
            }

            let ui_flags = (
                app.get_subtitles_visible(),
                app.get_hide_mpv_subtitles(),
                app.get_hide_memento_while_playing(),
                app.get_auto_pause(),
                app.get_furigana_visible(),
                app.get_video_width(),
                app.get_video_height(),
                app.get_subtitle_scale().to_bits(),
                app.get_search_remove_regex(),
                app.get_search_replace_lines(),
                app.get_search_replace_with(),
            );
            let ui_flags_changed = ui_flags != last_ui_flags;
            last_ui_flags = ui_flags;

            let needs_sync = ui_flags_changed
                || timer_asr_restart.get()
                || timer_asr.try_borrow().is_ok_and(|asr| asr.is_active())
                || timer_mpv.try_borrow().is_ok_and(|p| {
                    p.as_ref()
                        .is_some_and(|player| player.has_pending_events() || !player.is_paused())
                });
            if needs_sync {
                timer_sync();
            }
        },
    );

    SyncLoopHandles {
        housekeeping_timer,
        _dispatch_guard: dispatch_guard,
    }
}

mod anki;
mod glossary;
mod nested;
mod state;
mod subtitle;
mod wire;

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use slint::{ComponentHandle, Model as _, VecModel};

use crate::controller::config::ConfigController;
use crate::controller::dict::DictController;
use crate::ui::popup_css::PopupCssTheme;
use crate::ui::popup_model::{
    active_rich_bridge_for_app, build_dictionary_result_rows_grouped, clone_last_grouped_hits,
    dictionary_result_rows, is_kanji_char, popup_pitch_graphs, popup_rows_through,
    set_dictionary_rows, set_last_grouped_hits, suppress_first_term_ready_guard,
};
use crate::ui::subtitle_layout::{set_active_subtitle_render_style, subtitle_lines_for_app};
use crate::ui::text_engine::SubtitleRenderStyle;
use crate::ui::{
    DictResult, MementoApp, PopupBridge, PopupFrequency, SubtitleOverlayBridge, SubtitleWord,
};

use state::{
    apply_kanji_freq_updates_to_app, apply_popup_css_theme_to_bridge, build_row_extras_for_grouped,
    execute_nav_lookup, push_history_capped, AnkiPopupCheckRequest, AnkiPopupCheckResponse,
    DictWorkerRequest, GlossaryPreRenderBatch, GlossaryPreRenderDone, NavLookupRequest,
    NavLookupResponse, NavQueryKind, NestedScanResponse, PanelSearchRequest, PanelSearchResponse,
    PopupHistoryEntry, RichUiMutable, SuppressHistoryResetGuard, FRESH_CHECK_THROTTLE,
    IDLE_DICT_ASSET_CACHE_MAX_BYTES, IDLE_DICT_ASSET_CACHE_MAX_ENTRIES,
    IDLE_GLOSSARY_CACHE_MAX_BYTES, IDLE_GLOSSARY_CACHE_MAX_ENTRIES,
    IDLE_PARSED_GLOSSARY_MAX_ENTRIES, IDLE_POPUP_SHRINK_DELAY,
};
use subtitle::compute_subtitle_lookup_match_u16;

pub use anki::{
    mark_popup_term_anki_added, mark_popup_term_anki_updated, nested_popup_row_anki_note_id_i64,
    popup_row_anki_note_id_i64,
};
pub use wire::install_rich_ui_bridges;

enum PendingAssHighlightAction {
    Apply(crate::controller::ass::AssHoveredTarget, usize),
    Clear,
}

pub(in crate::ui) struct RichUiBridge {
    pub(in crate::ui) instance_id: i32,
    pub(in crate::ui) app_weak: slint::Weak<MementoApp>,
    db_path: PathBuf,
    state: RefCell<RichUiMutable>,
    panel_tx: mpsc::Sender<PanelSearchRequest>,
    panel_res_rx: Arc<Mutex<mpsc::Receiver<PanelSearchResponse>>>,
    panel_epoch: Arc<AtomicU64>,
    anki_tx: mpsc::Sender<AnkiPopupCheckRequest>,
    anki_res_rx: Arc<Mutex<mpsc::Receiver<AnkiPopupCheckResponse>>>,
    anki_nav_epoch: Arc<AtomicU64>,
    nested_anki_nav_epoch: Arc<AtomicU64>,
    root_anki_in_flight: Arc<AtomicU64>,
    nested_anki_in_flight: Arc<AtomicU64>,
    external_lookup_epoch: Arc<Mutex<Option<Arc<AtomicU64>>>>,
    nested_tx: mpsc::Sender<DictWorkerRequest>,
    nested_res_rx: Arc<Mutex<mpsc::Receiver<NestedScanResponse>>>,
    nested_epoch: Arc<AtomicU64>,
    nav_res_rx: Arc<Mutex<mpsc::Receiver<NavLookupResponse>>>,
    nav_epoch: Arc<AtomicU64>,
    glossary_render_tx: Option<mpsc::Sender<GlossaryPreRenderBatch>>,
    glossary_render_res_rx: Arc<Mutex<mpsc::Receiver<GlossaryPreRenderDone>>>,
    glossary_render_epoch: Arc<AtomicU64>,
    /// Strictly `None` in production (real Slint event loops use `Weak::upgrade_in_event_loop`).
    /// Allocated only under `i_slint_backend_testing::init_no_event_loop()` where there is no
    /// event loop proxy and mock-time tests drive async completions via `mock_elapsed_time`.
    no_event_loop_pump: Option<slint::Timer>,
}

pub(super) fn drain_panel_results_for_app(app: &MementoApp) -> bool {
    if let Some(br) = active_rich_bridge_for_app(app) {
        br.drain_panel_results(app)
    } else {
        false
    }
}

pub(super) fn drain_nav_lookup_results_for_app(app: &MementoApp) -> bool {
    if let Some(br) = active_rich_bridge_for_app(app) {
        br.drain_nav_lookup_results(app)
    } else {
        false
    }
}

impl RichUiBridge {
    pub(in crate::ui) fn subtitle_style(&self) -> Option<SubtitleRenderStyle> {
        self.state
            .try_borrow()
            .ok()
            .map(|st| st.settings.subtitle_style.clone())
    }

    pub(super) fn cancel_idle_memory_shrink(&self) {
        if let Ok(st) = self.state.try_borrow() {
            st.idle_shrink_timer.stop();
        }
    }

    pub(super) fn schedule_idle_memory_shrink_if_closed(&self, app: &MementoApp) {
        if app.get_dict_open() {
            self.cancel_idle_memory_shrink();
            return;
        }
        self.glossary_render_epoch.fetch_add(1, Ordering::AcqRel);
        let already_running = self
            .state
            .try_borrow()
            .map(|st| st.idle_shrink_timer.running())
            .unwrap_or(true);
        if already_running {
            return;
        }
        let weak_app = self.app_weak.clone();
        if let Ok(st) = self.state.try_borrow() {
            st.idle_shrink_timer.start(
                slint::TimerMode::SingleShot,
                IDLE_POPUP_SHRINK_DELAY,
                move || {
                    let Some(app) = weak_app.upgrade() else {
                        return;
                    };
                    if app.get_dict_open() {
                        return;
                    }
                    let Some(br) = active_rich_bridge_for_app(&app) else {
                        return;
                    };
                    br.perform_idle_memory_shrink();
                },
            );
        }
    }

    pub(super) fn perform_idle_memory_shrink(&self) {
        self.glossary_render_epoch.fetch_add(1, Ordering::AcqRel);
        {
            let mut st_guard = self.state.borrow_mut();
            let st = &mut *st_guard;
            st.glossary_cache
                .shrink_to(IDLE_GLOSSARY_CACHE_MAX_ENTRIES, IDLE_GLOSSARY_CACHE_MAX_BYTES);
            st.last_rendered_key
                .retain(|_, key| st.glossary_cache.contains_key(key));
            for lvl in &mut st.nested_stack {
                lvl.glossary_cache.clear();
                lvl.last_rendered_key.clear();
            }
        }
        crate::ui::dict_assets::shrink_dict_asset_cache(
            IDLE_DICT_ASSET_CACHE_MAX_ENTRIES,
            IDLE_DICT_ASSET_CACHE_MAX_BYTES,
        );
        crate::controller::dict::shrink_parsed_glossary_cache(IDLE_PARSED_GLOSSARY_MAX_ENTRIES);
        crate::ui::release_os_memory();
    }

    pub(super) fn drain_nav_lookup_results(&self, app: &MementoApp) -> bool {
        let mut latest_root: Option<NavLookupResponse> = None;
        let mut latest_nested: Option<NavLookupResponse> = None;
        if let Ok(guard) = self.nav_res_rx.lock() {
            while let Ok(item) = guard.try_recv() {
                if item.kind.is_nested() {
                    latest_nested = Some(item);
                } else {
                    latest_root = Some(item);
                }
            }
        }
        let mut applied = false;
        if let Some(resp) = latest_root {
            if resp.epoch == self.nav_epoch.load(Ordering::Acquire) {
                self.apply_root_nav_lookup_response(app, resp);
                applied = true;
            }
        }
        if let Some(resp) = latest_nested {
            if resp.epoch == self.nested_epoch.load(Ordering::Acquire) {
                if app.get_dict_open() {
                    self.apply_nested_nav_lookup_response(app, resp);
                    applied = true;
                } else {
                    app.global::<PopupBridge>().set_nested_searching(false);
                }
            }
        }
        applied
    }

    pub(super) fn kick_no_event_loop_pump(&self) {
        let Some(ref pump) = self.no_event_loop_pump else {
            return;
        };
        if pump.running() {
            return;
        }
        let weak_app = self.app_weak.clone();
        pump.start(
            slint::TimerMode::Repeated,
            Duration::from_millis(8),
            move || {
                let Some(app) = weak_app.upgrade() else {
                    return;
                };
                let Some(br) = active_rich_bridge_for_app(&app) else {
                    return;
                };
                let _ = br.drain_panel_results(&app);
                let _ = br.drain_nested_scan_results(&app);
                let _ = br.drain_anki_results(&app);
                let pb = app.global::<PopupBridge>();
                if !pb.get_panel_searching()
                    && !pb.get_nested_searching()
                    && !pb.get_anki_checking()
                {
                    if let Some(ref timer) = br.no_event_loop_pump {
                        timer.stop();
                    }
                }
            },
        );
    }

    pub(super) fn drain_panel_results(&self, app: &MementoApp) -> bool {
        let mut latest: Option<PanelSearchResponse> = None;
        if let Ok(guard) = self.panel_res_rx.lock() {
            while let Ok(item) = guard.try_recv() {
                latest = Some(item);
            }
        }
        let Some(resp) = latest else {
            return false;
        };
        if resp.gen_id != self.panel_epoch.load(Ordering::Acquire) {
            return false;
        }
        // Build DictResult rows WITHOUT touching LAST_GROUPED_HITS or app.dict_results!
        let (rows, _) = build_dictionary_result_rows_grouped(&resp.hits, &resp.excluded);
        let pb = app.global::<PopupBridge>();
        pb.set_panel_results(Rc::new(VecModel::from(rows)).into());
        pb.set_panel_searching(false);
        true
    }

    pub(super) fn ensure_fresh_db_and_config(&self, app: &MementoApp, force: bool) {
        let mut sub_style_update: Option<(bool, SubtitleRenderStyle)> = None;
        let mut reloaded_css_theme: Option<Arc<PopupCssTheme>> = None;
        let mut reloaded_max_depth: Option<usize> = None;
        let mut truncated_nested = false;
        let mut middle_mouse_update: Option<bool> = None;
        {
            let mut st = self.state.borrow_mut();
            if !force
                && st
                    .last_fresh_check
                    .is_some_and(|t| t.elapsed() < FRESH_CHECK_THROTTLE)
            {
                return;
            }
            st.last_fresh_check = Some(Instant::now());

            let prev_sub_style = st.settings.subtitle_style.clone();
            let prev_fonts = st.settings.popup_fonts.clone();
            let prev_glossary_style = st.settings.glossary_style;
            let prev_middle_mouse = st.settings.middle_mouse_scan;

            let config_dir = st.config_dir.clone();
            let config_reloaded = st.settings.reload_if_changed_on_disk(&config_dir);
            let css_changed = st.stylesheets.reload_user_css_if_changed();
            let theme_changed = st.refresh_cached_theme(css_changed);

            if config_reloaded {
                let max_d = st.settings.max_recursion_depth;
                reloaded_max_depth = Some(max_d);
                if st.nested_stack.len() > max_d {
                    st.nested_stack.truncate(max_d);
                    st.nested_body_hover_count = 0;
                    st.pending_nested_anki_adds.retain(|p| p.depth <= max_d);
                    if let Some(top) = st.nested_stack.last_mut() {
                        top.scan_highlight = None;
                    } else {
                        st.root_scan_highlight = None;
                        st.nested_card_hovered = false;
                    }
                    truncated_nested = true;
                }
                if st.settings.middle_mouse_scan != prev_middle_mouse {
                    middle_mouse_update = Some(st.settings.middle_mouse_scan);
                }
                if st.settings.subtitle_style != prev_sub_style {
                    sub_style_update = Some((true, st.settings.subtitle_style.clone()));
                }
            }

            let glossary_visual_changed = css_changed
                || theme_changed
                || st.settings.glossary_style != prev_glossary_style
                || st.settings.popup_fonts != prev_fonts;
            if glossary_visual_changed {
                self.glossary_render_epoch.fetch_add(1, Ordering::AcqRel);
                st.clear_all_glossary_caches();
                reloaded_css_theme = Some(st.effective_popup_theme());
            }

            let sig = crate::controller::dict::db_file_fingerprint(&self.db_path);
            if sig != st.db_sig {
                st.db_sig = sig;
                st.dict_ctrl = DictController::open(&self.db_path).unwrap_or_default();
                st.applied_search_cfg = None;
                st.db_helper.force_refresh();
            } else {
                st.db_helper.refresh_if_needed();
            }
            st.apply_search_config_if_needed();
        }

        if let Some(max_d) = reloaded_max_depth {
            app.global::<PopupBridge>()
                .set_nested_max_depth(max_d as i32);
        }
        if truncated_nested {
            self.cancel_nested_anki_check(app);
            self.sync_nested_popup_to_bridge(app, true);
        }
        if let Some(middle_mouse) = middle_mouse_update {
            app.global::<SubtitleOverlayBridge>()
                .set_middle_mouse_scan_enabled(middle_mouse);
        }
        if let Some(theme) = reloaded_css_theme {
            apply_popup_css_theme_to_bridge(app, &theme);
            let pb = app.global::<PopupBridge>();
            pb.set_rich_revision(pb.get_rich_revision().wrapping_add(1));
            if pb.get_nested_depth() > 0 {
                pb.set_nested_revision(pb.get_nested_revision().wrapping_add(1));
            }
        }
        if let Some((relayout, new_sub_style)) = sub_style_update {
            let bg = new_sub_style.bg_rgba;
            set_active_subtitle_render_style(new_sub_style);
            let sb = app.global::<SubtitleOverlayBridge>();
            sb.set_subtitle_bg_color(slint::Color::from_argb_u8(bg[3], bg[0], bg[1], bg[2]));
            sb.set_style_revision(sb.get_style_revision().wrapping_add(1));
            if relayout {
                let words: Vec<SubtitleWord> = app.get_active_sub_words().iter().collect();
                if !words.is_empty() {
                    let lines = subtitle_lines_for_app(app, words);
                    app.set_active_sub_lines(Rc::new(VecModel::from(lines)).into());
                }
            }
        }
    }

    pub(in crate::ui) fn on_dictionary_rows_updated(&self, app: &MementoApp, rows: &[DictResult]) {
        if rows.is_empty() {
            self.schedule_idle_memory_shrink_if_closed(app);
        } else {
            self.cancel_idle_memory_shrink();
        }
        self.ensure_fresh_db_and_config(app, false);
        let candidate_hits = clone_last_grouped_hits();
        let matches_hits = candidate_hits.len() == rows.len()
            && candidate_hits.iter().zip(rows.iter()).all(|(h, r)| {
                h.expression == r.expression.as_str()
                    && h.reading == r.reading.as_str()
                    && h.dict_name == r.dict_name.as_str()
            });

        self.nav_epoch.fetch_add(1, Ordering::AcqRel);
        self.glossary_render_epoch.fetch_add(1, Ordering::AcqRel);
        self.nested_epoch.fetch_add(1, Ordering::AcqRel);
        self.nested_anki_nav_epoch.fetch_add(1, Ordering::AcqRel);
        self.nested_anki_in_flight.store(0, Ordering::Release);

        let (can_back, can_fwd, crumb, reset_kana, kanji_freq_updates, ass_ctrl, ass_action) = {
            let mut st = self.state.borrow_mut();
            if !st.suppress_history_reset {
                st.back_stack.clear();
                st.forward_stack.clear();
            }
            st.rich_epoch = st.rich_epoch.wrapping_add(1);
            st.row_extras.clear();
            st.glossary_cache.clear();
            st.last_rendered_key.clear();
            st.selection = None;
            st.root_scan_highlight = None;
            st.root_expanded_cards.clear();
            st.root_row_anki_note_ids_i64.clear();
            st.last_active_glossary_row_idx = 0;
            st.nested_stack.clear();
            st.hovered_glossary_target = None;
            st.nested_card_hovered = false;
            st.nested_body_hover_count = 0;
            st.nested_scan_timer.stop();
            st.nested_dismiss_timer.stop();
            st.pending_nested_anki_adds.clear();
            let precomputed_meta = st.pending_root_precomputed_meta.take();

            let mut kanji_freq_updates = Vec::new();
            let mut ass_action: Option<PendingAssHighlightAction> = None;

            if matches_hits {
                st.current_hits = candidate_hits.clone();
                let state_ref = &mut *st;
                let (pre_pitch, pre_kanji) = match precomputed_meta.as_ref() {
                    Some((p, k)) => (Some(p), Some(k)),
                    None => (None, None),
                };
                let (extras, freq_updates) = build_row_extras_for_grouped(
                    &mut state_ref.db_helper,
                    &mut state_ref.dict_ctrl,
                    rows,
                    &candidate_hits,
                    pre_pitch,
                    pre_kanji,
                );
                state_ref.row_extras = extras;
                kanji_freq_updates = freq_updates;

                let pending_sub = st.pending_scan_sub_query.take();
                let has_text_filters =
                    !st.settings.remove_regex.is_empty() || st.settings.replace_lines;
                if let Some(utf16_start) = st.pending_scan_utf16_start.take() {
                    let matched_u16 = compute_subtitle_lookup_match_u16(
                        &mut st.dict_ctrl,
                        candidate_hits.first(),
                        pending_sub.as_deref(),
                        has_text_filters,
                    );
                    st.active_scan_utf16_range = Some((utf16_start, utf16_start + matched_u16));
                    st.pending_ass_scan = None;
                } else if let Some(ass_target) = st.pending_ass_scan.take() {
                    let matched_u16 = compute_subtitle_lookup_match_u16(
                        &mut st.dict_ctrl,
                        candidate_hits.first(),
                        pending_sub.as_deref(),
                        has_text_filters,
                    );
                    ass_action = Some(PendingAssHighlightAction::Apply(ass_target, matched_u16));
                }
            } else {
                st.current_hits.clear();
                st.pending_scan_utf16_start = None;
                st.pending_scan_sub_query = None;
                if st.pending_ass_scan.take().is_some() || rows.is_empty() {
                    ass_action = Some(PendingAssHighlightAction::Clear);
                }
            }

            let can_back = !st.back_stack.is_empty();
            let can_fwd = !st.forward_stack.is_empty();
            let crumb = st
                .back_stack
                .back()
                .map(|entry| format!("From {}", entry.label))
                .unwrap_or_default();
            let reset_kana = !st.suppress_history_reset;
            let ass_ctrl = st.ass_controller.clone();
            (
                can_back,
                can_fwd,
                crumb,
                reset_kana,
                kanji_freq_updates,
                ass_ctrl,
                ass_action,
            )
        };

        apply_kanji_freq_updates_to_app(app, kanji_freq_updates);

        if let (Some(action), Some(ass_ctrl)) = (ass_action, ass_ctrl) {
            if let Ok(mut ctrl) = ass_ctrl.try_borrow_mut() {
                match action {
                    PendingAssHighlightAction::Apply(ass_target, matched_u16) => {
                        ctrl.apply_lookup_highlight(app, &ass_target, matched_u16);
                    }
                    PendingAssHighlightAction::Clear => {
                        ctrl.clear_lookup_highlight(app);
                    }
                }
            }
        }

        let pb = app.global::<PopupBridge>();
        pb.set_can_go_back(can_back);
        pb.set_can_go_forward(can_fwd);
        pb.set_nav_breadcrumb(crumb.into());
        if reset_kana {
            pb.set_kana_reading_active(false);
            pb.set_active_kana_reading("".into());
        }
        if reset_kana
            && app.get_dict_search_open()
            && app.get_selected_token_index() < 0
            && pb.get_panel_query().is_empty()
            && !app.get_dict_query().is_empty()
        {
            pb.set_panel_query(app.get_dict_query());
            pb.set_panel_results(Rc::new(VecModel::from(rows.to_vec())).into());
        }
        pb.set_rich_revision(pb.get_rich_revision().wrapping_add(1));
        pb.set_selection_revision(pb.get_selection_revision().wrapping_add(1));
        self.sync_nested_popup_to_bridge(app, true);

        let sb = app.global::<SubtitleOverlayBridge>();
        sb.set_highlight_revision(sb.get_highlight_revision().wrapping_add(1));

        if matches_hits && !rows.is_empty() {
            self.dispatch_glossary_prerender(app, rows);
        }

        self.dispatch_anki_check_for_rows(app, rows, None);
    }

    fn snapshot_current_popup(&self, app: &MementoApp) -> Option<PopupHistoryEntry> {
        let full = app.get_dict_results();
        if full.row_count() == 0 {
            return None;
        }
        let rows: Vec<DictResult> = (0..full.row_count())
            .filter_map(|i| full.row_data(i))
            .collect();
        let hits = self.state.borrow().current_hits.clone();
        let pb = app.global::<PopupBridge>();
        let label = if !app.get_dict_query().is_empty() {
            app.get_dict_query().to_string()
        } else {
            rows.first()
                .map(|r| r.expression.to_string())
                .unwrap_or_default()
        };
        Some(PopupHistoryEntry {
            label,
            rows,
            hits,
            loaded_count: app.get_popup_loaded_count(),
            headword: app.get_dict_query().to_string(),
            reading: app.get_dict_reading().to_string(),
            pitch_accent: app.get_dict_pitch_accent().to_string(),
            freq_rank: app.get_dict_freq_rank().to_string(),
            jpdb_rank: app.get_dict_jpdb_rank().to_string(),
            kana_reading_active: pb.get_kana_reading_active(),
            active_kana_reading: pb.get_active_kana_reading().to_string(),
        })
    }

    fn restore_history_entry(&self, app: &MementoApp, entry: PopupHistoryEntry) {
        self.cancel_idle_memory_shrink();
        set_last_grouped_hits(entry.hits);
        let _history_guard = SuppressHistoryResetGuard::enter(&self.state);

        let pb = app.global::<PopupBridge>();
        pb.set_kana_reading_active(entry.kana_reading_active);
        pb.set_active_kana_reading(entry.active_kana_reading.into());

        app.set_anki_added_note_id("".into());
        app.set_anki_added_result_index(-1);
        {
            let _first_term_guard = suppress_first_term_ready_guard();
            set_dictionary_rows(app, entry.rows);
        }
        let full = app.get_dict_results();
        let loaded = entry.loaded_count.max(1) as usize;
        app.set_popup_loaded_count(loaded.min(full.row_count()) as i32);
        app.set_popup_results(Rc::new(VecModel::from(popup_rows_through(&full, loaded))).into());
        app.set_dict_query(entry.headword.into());
        app.set_dict_reading(entry.reading.into());
        app.set_dict_pitch_accent(entry.pitch_accent.into());
        app.set_dict_freq_rank(entry.freq_rank.into());
        app.set_dict_jpdb_rank(entry.jpdb_rank.into());
        app.set_dict_open(full.row_count() > 0);
        app.invoke_reset_definition_scroll();
    }

    pub(super) fn navigate_to_term_query(
        &self,
        app: &MementoApp,
        query: &str,
        kana_mode: Option<&str>,
    ) {
        let trimmed = query.trim();
        if trimmed.is_empty() {
            return;
        }
        self.cancel_idle_memory_shrink();
        self.ensure_fresh_db_and_config(app, true);
        let epoch = self.nav_epoch.fetch_add(1, Ordering::AcqRel) + 1;
        let (excluded, search_config) = {
            let st = self.state.borrow();
            (
                st.settings.excluded_glossaries.clone(),
                st.settings.dict_search_config(),
            )
        };
        let req = NavLookupRequest {
            epoch,
            kind: NavQueryKind::RootTerm {
                query: trimmed.to_owned(),
                kana_mode: kana_mode.map(str::to_owned),
            },
            excluded,
            search_config,
        };
        if self.no_event_loop_pump.is_some() {
            let resp = {
                let mut st = self.state.borrow_mut();
                execute_nav_lookup(&mut st.dict_ctrl, req)
            };
            self.apply_root_nav_lookup_response(app, resp);
        } else {
            let _ = self.nested_tx.send(DictWorkerRequest::NavLookup(req));
        }
    }

    pub(super) fn navigate_to_kanji(
        &self,
        app: &MementoApp,
        expression: &str,
        clicked_kanji: &str,
    ) {
        let target_char = clicked_kanji
            .chars()
            .find(|&c| is_kanji_char(c))
            .or_else(|| expression.chars().find(|&c| is_kanji_char(c)));
        let Some(ch) = target_char else {
            app.set_status_message("No kanji in selected word".into());
            return;
        };
        let query = ch.to_string();
        self.cancel_idle_memory_shrink();
        self.ensure_fresh_db_and_config(app, true);
        let epoch = self.nav_epoch.fetch_add(1, Ordering::AcqRel) + 1;
        let (excluded, search_config) = {
            let st = self.state.borrow();
            (
                st.settings.excluded_glossaries.clone(),
                st.settings.dict_search_config(),
            )
        };
        let req = NavLookupRequest {
            epoch,
            kind: NavQueryKind::RootKanji { query },
            excluded,
            search_config,
        };
        if self.no_event_loop_pump.is_some() {
            let resp = {
                let mut st = self.state.borrow_mut();
                execute_nav_lookup(&mut st.dict_ctrl, req)
            };
            self.apply_root_nav_lookup_response(app, resp);
        } else {
            let _ = self.nested_tx.send(DictWorkerRequest::NavLookup(req));
        }
    }

    fn apply_root_nav_lookup_response(&self, app: &MementoApp, resp: NavLookupResponse) {
        match resp.kind {
            NavQueryKind::RootTerm { query, kana_mode } => {
                if resp.hits.is_empty() {
                    app.set_status_message(
                        format!("No dictionary entries found for {query}").into(),
                    );
                    return;
                }
                let current_snap = self.snapshot_current_popup(app);
                {
                    let mut st = self.state.borrow_mut();
                    if let Some(snap) = current_snap {
                        push_history_capped(&mut st.back_stack, snap);
                    }
                    st.forward_stack.clear();
                    st.pending_root_precomputed_meta =
                        Some((resp.term_pitch_details, resp.kanji_frequencies));
                }
                let _history_guard = SuppressHistoryResetGuard::enter(&self.state);

                let pb = app.global::<PopupBridge>();
                if let Some(kana_target) = kana_mode {
                    pb.set_kana_reading_active(true);
                    pb.set_active_kana_reading(kana_target.into());
                } else {
                    pb.set_kana_reading_active(false);
                    pb.set_active_kana_reading("".into());
                }

                let mut rows = dictionary_result_rows(&resp.hits, &resp.excluded);
                for row in &mut rows {
                    if row.term_start && !row.is_kanji {
                        if let Some((_, _, graphs, freqs)) = resp
                            .term_meta_rows
                            .iter()
                            .find(|(e, r, _, _)| {
                                e == row.expression.as_str() && r == row.reading.as_str()
                            })
                        {
                            row.pitch_graphs =
                                Rc::new(VecModel::from(popup_pitch_graphs(graphs.clone()))).into();
                            row.frequencies = Rc::new(VecModel::from(
                                freqs
                                    .iter()
                                    .map(|f| PopupFrequency {
                                        dictionary: f.dictionary.clone().into(),
                                        value: f.value.clone().into(),
                                    })
                                    .collect::<Vec<_>>(),
                            ))
                            .into();
                        }
                    }
                }
                app.set_anki_added_note_id("".into());
                app.set_anki_added_result_index(-1);
                set_dictionary_rows(app, rows);
                app.set_dict_query(resp.headword.into());
                app.set_dict_reading(resp.reading.into());
                app.set_dict_pitch_accent(resp.pitch.into());
                app.set_dict_freq_rank(resp.freq.into());
                app.set_dict_jpdb_rank(resp.jpdb.into());
                app.set_dict_open(true);
                app.invoke_reset_definition_scroll();
            }
            NavQueryKind::RootKanji { query } => {
                if resp.hits.is_empty() {
                    app.set_status_message(format!("No kanji entry found for {query}").into());
                    return;
                }
                let current_snap = self.snapshot_current_popup(app);
                {
                    let mut st = self.state.borrow_mut();
                    if let Some(snap) = current_snap {
                        push_history_capped(&mut st.back_stack, snap);
                    }
                    st.forward_stack.clear();
                    st.pending_root_precomputed_meta =
                        Some((std::collections::HashMap::new(), resp.kanji_frequencies));
                }
                let _history_guard = SuppressHistoryResetGuard::enter(&self.state);

                let pb = app.global::<PopupBridge>();
                pb.set_kana_reading_active(false);
                pb.set_active_kana_reading("".into());

                let rows = dictionary_result_rows(&resp.hits, &resp.excluded);
                app.set_anki_added_note_id("".into());
                app.set_anki_added_result_index(-1);
                set_dictionary_rows(app, rows);
                app.set_dict_query(query.into());
                app.set_dict_reading(resp.reading.into());
                app.set_dict_pitch_accent("".into());
                app.set_dict_freq_rank("".into());
                app.set_dict_jpdb_rank("".into());
                app.set_dict_open(true);
                app.invoke_reset_definition_scroll();
            }
            NavQueryKind::NestedTerm { .. } | NavQueryKind::NestedKanji { .. } => {}
        }
    }
}

/// Immediately propagate updated settings from `ConfigController` into the active rich UI bridge.
pub fn refresh_rich_ui_bridges(app: &MementoApp, config: &ConfigController) {
    let Some(bridge) = active_rich_bridge_for_app(app) else {
        return;
    };
    let (
        middle_mouse,
        max_recursion_depth,
        truncated_nested,
        sub_style,
        sub_style_changed,
        glossary_visual_changed,
        popup_theme,
    ) = {
        let mut st = bridge.state.borrow_mut();
        let prev_fonts = st.settings.popup_fonts.clone();
        let prev_glossary_style = st.settings.glossary_style;
        let prev_sub_style = st.settings.subtitle_style.clone();

        st.settings.apply_controller(config);
        st.last_fresh_check = Some(Instant::now());

        let max_depth = st.settings.max_recursion_depth;
        let mut truncated = false;
        if st.nested_stack.len() > max_depth {
            st.nested_stack.truncate(max_depth);
            st.nested_body_hover_count = 0;
            st.pending_nested_anki_adds
                .retain(|p| p.depth <= max_depth);
            if let Some(top) = st.nested_stack.last_mut() {
                top.scan_highlight = None;
            } else {
                st.root_scan_highlight = None;
                st.nested_card_hovered = false;
            }
            truncated = true;
        }

        let css_changed = st.stylesheets.force_reload_user_css();
        let theme_changed = st.refresh_cached_theme(css_changed);
        let glossary_visual_changed = css_changed
            || theme_changed
            || st.settings.glossary_style != prev_glossary_style
            || st.settings.popup_fonts != prev_fonts;
        if glossary_visual_changed {
            bridge.glossary_render_epoch.fetch_add(1, Ordering::AcqRel);
            st.clear_all_glossary_caches();
        }

        let sig = crate::controller::dict::db_file_fingerprint(&bridge.db_path);
        if sig != st.db_sig {
            st.db_sig = sig;
            st.dict_ctrl = DictController::open(&bridge.db_path).unwrap_or_default();
            st.applied_search_cfg = None;
            st.db_helper.force_refresh();
        } else {
            st.db_helper.refresh_if_needed();
        }
        st.apply_search_config_if_needed();

        let sub_style_changed = st.settings.subtitle_style != prev_sub_style;
        (
            st.settings.middle_mouse_scan,
            max_depth,
            truncated,
            st.settings.subtitle_style.clone(),
            sub_style_changed,
            glossary_visual_changed,
            st.effective_popup_theme(),
        )
    };

    let pb = app.global::<PopupBridge>();
    pb.set_anki_show_tags(config.anki_show_tag());
    pb.set_nested_max_depth(max_recursion_depth as i32);
    if truncated_nested {
        bridge.cancel_nested_anki_check(app);
        bridge.sync_nested_popup_to_bridge(app, true);
    }
    if glossary_visual_changed {
        apply_popup_css_theme_to_bridge(app, &popup_theme);
        pb.set_rich_revision(pb.get_rich_revision().wrapping_add(1));
        if pb.get_nested_depth() > 0 {
            pb.set_nested_revision(pb.get_nested_revision().wrapping_add(1));
        }
    }
    if app.get_dict_open() {
        let full = app.get_dict_results();
        let current_rows: Vec<DictResult> = (0..full.row_count())
            .filter_map(|i| full.row_data(i))
            .collect();
        bridge.dispatch_anki_check_for_rows(app, &current_rows, None);
    }

    let sb = app.global::<SubtitleOverlayBridge>();
    sb.set_middle_mouse_scan_enabled(middle_mouse);
    let sub_bg = sub_style.bg_rgba;
    sb.set_subtitle_bg_color(slint::Color::from_argb_u8(
        sub_bg[3], sub_bg[0], sub_bg[1], sub_bg[2],
    ));
    if sub_style_changed {
        set_active_subtitle_render_style(sub_style);
        sb.set_style_revision(sb.get_style_revision().wrapping_add(1));
        let words: Vec<SubtitleWord> = app.get_active_sub_words().iter().collect();
        if !words.is_empty() {
            let lines = subtitle_lines_for_app(app, words);
            app.set_active_sub_lines(Rc::new(VecModel::from(lines)).into());
        }
    }
}

/// Binds `main.rs`'s `lookup_epoch` onto the active `RichUiBridge` so background Anki
/// duplicate/tag checks are cancelled immediately when a subtitle lookup is superseded or dismissed.
pub fn bind_rich_bridge_lookup_epoch(app: &MementoApp, lookup_epoch: Arc<AtomicU64>) {
    if let Some(bridge) = active_rich_bridge_for_app(app) {
        if let Ok(mut guard) = bridge.external_lookup_epoch.lock() {
            *guard = Some(lookup_epoch);
        }
    }
}

/// Binds an [`AssOverlayController`](crate::controller::ass::AssOverlayController) onto the
/// active `RichUiBridge` so hover/modifier/left-click/middle-click events on native ASS lines
/// trigger dictionary scanning, continuous `#38a6d6` highlight bars + crisp `libass` glyph
/// patches, and per-speaker popup anchoring.
pub fn bind_rich_bridge_ass_controller(
    app: &MementoApp,
    ass_controller: Rc<RefCell<crate::controller::ass::AssOverlayController>>,
) {
    if let Some(bridge) = active_rich_bridge_for_app(app) {
        bridge.state.borrow_mut().ass_controller = Some(ass_controller);
    }
}

/// Immediately clears any active ASS lookup highlight / anchor on the bound `AssOverlayController`.
pub fn clear_rich_bridge_ass_lookup(app: &MementoApp) {
    let bridge = active_rich_bridge_for_app(app);
    let ass_ctrl = bridge.as_ref().and_then(|bridge| {
        let mut st = bridge.state.borrow_mut();
        st.pending_ass_scan = None;
        st.ass_controller.clone()
    });
    if let Some(ass_ctrl) = ass_ctrl {
        if let Ok(mut ctrl) = ass_ctrl.try_borrow_mut() {
            ctrl.clear_lookup_highlight(app);
        }
    }
    if let Some(bridge) = bridge {
        bridge.schedule_idle_memory_shrink_if_closed(app);
    }
}

/// Returns the active regular-subtitle highlight UTF-16 range `(start, end)` for the open popup, if any.
pub fn active_subtitle_scan_utf16_range(app: &MementoApp) -> Option<(usize, usize)> {
    active_rich_bridge_for_app(app)
        .and_then(|bridge| bridge.state.borrow().active_scan_utf16_range)
}

/// Returns the active ASS per-speaker lookup context (if the open popup came from
/// hovering/clicking a native ASS line) for accurate `{subtitle}`, `{cloze-*}`, and `{audio-media}`.
pub fn active_ass_lookup_for_app(
    app: &MementoApp,
) -> Option<crate::controller::ass::AssActiveLookup> {
    if !app.global::<SubtitleOverlayBridge>().get_ass_native_active() {
        return None;
    }
    let ass_ctrl = active_rich_bridge_for_app(app)
        .and_then(|bridge| bridge.state.borrow().ass_controller.clone())?;
    let lookup = ass_ctrl.try_borrow().ok()?.active_lookup().cloned();
    lookup
}

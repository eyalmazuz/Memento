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

use slint::{ComponentHandle, Model as _, SharedString, VecModel};

use crate::app::anki_actions::{
    clear_active_glossary_selections, clear_active_row_hit_meta, set_active_row_hit_meta,
    ActiveRowHitMeta,
};
use crate::controller::config::ConfigController;
use crate::controller::dict::DictController;
use crate::ui::popup_css::PopupCssTheme;
use crate::ui::popup_model::{
    active_rich_bridge_for_app, build_dictionary_result_rows_grouped_with_options,
    clone_last_grouped_hits, dictionary_result_rows_with_options, is_kanji_char,
    popup_pitch_graphs, popup_rows_through, set_dictionary_rows, set_last_grouped_hits,
    suppress_first_term_ready_guard,
};
use crate::ui::subtitle_layout::{set_active_subtitle_render_style, subtitle_lines_for_app};
use crate::ui::text_engine::SubtitleRenderStyle;
use crate::ui::{
    DictResult, SubomiApp, PopupBridge, PopupFrequency, SubtitleOverlayBridge, SubtitleWord,
};

use state::{
    apply_kanji_freq_updates_to_app, apply_kanji_freq_updates_to_rows,
    apply_popup_css_theme_to_bridge, build_row_extras_for_grouped, execute_nav_lookup,
    push_history_capped, sync_subtitle_overlay_style_to_bridge, AnkiCheckTarget,
    AnkiPopupCheckRequest, AnkiPopupCheckResponse, DictWorkerRequest, GlossaryPreRenderBatch,
    GlossaryPreRenderDone, NavLookupRequest, NavLookupResponse, NavQueryKind, NestedScanResponse,
    PanelSearchRequest, PanelSearchResponse, PopupHistoryEntry, RichUiMutable,
    SuppressHistoryResetGuard, FRESH_CHECK_THROTTLE, IDLE_DICT_ASSET_CACHE_MAX_BYTES,
    IDLE_DICT_ASSET_CACHE_MAX_ENTRIES, IDLE_GLOSSARY_CACHE_MAX_BYTES,
    IDLE_GLOSSARY_CACHE_MAX_ENTRIES, IDLE_PARSED_GLOSSARY_MAX_ENTRIES, IDLE_POPUP_SHRINK_DELAY,
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
    pub(in crate::ui) app_weak: slint::Weak<SubomiApp>,
    db_path: PathBuf,
    state: RefCell<RichUiMutable>,
    panel_tx: mpsc::Sender<PanelSearchRequest>,
    panel_res_rx: Arc<Mutex<mpsc::Receiver<PanelSearchResponse>>>,
    panel_epoch: Arc<AtomicU64>,
    anki_tx: mpsc::Sender<AnkiPopupCheckRequest>,
    anki_res_rx: Arc<Mutex<mpsc::Receiver<AnkiPopupCheckResponse>>>,
    anki_nav_epoch: Arc<AtomicU64>,
    nested_anki_nav_epoch: Arc<AtomicU64>,
    panel_anki_nav_epoch: Arc<AtomicU64>,
    root_anki_in_flight: Arc<AtomicU64>,
    nested_anki_in_flight: Arc<AtomicU64>,
    panel_anki_in_flight: Arc<AtomicU64>,
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

pub(super) fn drain_panel_results_for_app(app: &SubomiApp) -> bool {
    if let Some(br) = active_rich_bridge_for_app(app) {
        br.drain_panel_results(app)
    } else {
        false
    }
}

pub(super) fn drain_nav_lookup_results_for_app(app: &SubomiApp) -> bool {
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

    pub(super) fn schedule_idle_memory_shrink_if_closed(&self, app: &SubomiApp) {
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
            st.panel
                .glossary_cache
                .shrink_to(IDLE_GLOSSARY_CACHE_MAX_ENTRIES, IDLE_GLOSSARY_CACHE_MAX_BYTES);
            st.panel
                .last_rendered_key
                .retain(|_, key| st.panel.glossary_cache.contains_key(key));
        }
        crate::ui::dict_assets::shrink_dict_asset_cache(
            IDLE_DICT_ASSET_CACHE_MAX_ENTRIES,
            IDLE_DICT_ASSET_CACHE_MAX_BYTES,
        );
        crate::controller::dict::shrink_parsed_glossary_cache(IDLE_PARSED_GLOSSARY_MAX_ENTRIES);
        crate::controller::dict::shrink_all_readonly_dbs();
        crate::ui::release_os_memory();
    }

    pub(super) fn drain_nav_lookup_results(&self, app: &SubomiApp) -> bool {
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

    pub(super) fn drain_panel_results(&self, app: &SubomiApp) -> bool {
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
        let (compact_glossaries, compact_tags) = {
            let st = self.state.borrow();
            (st.settings.compact_glossaries, st.settings.compact_tags)
        };
        // Build DictResult rows WITHOUT touching LAST_GROUPED_HITS or app.dict_results!
        let (mut rows, grouped_hits) = build_dictionary_result_rows_grouped_with_options(
            &resp.hits,
            &resp.excluded,
            None,
            compact_glossaries,
            compact_tags,
        );
        for row in &mut rows {
            if row.term_start && !row.is_kanji {
                if let Some((_, _, graphs, freqs)) = resp.term_meta_rows.iter().find(|(e, r, _, _)| {
                    e == row.expression.as_str() && r == row.reading.as_str()
                }) {
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
        {
            let mut st = self.state.borrow_mut();
            let state_ref = &mut *st;
            let (row_extras, kanji_freq_updates) = build_row_extras_for_grouped(
                &mut state_ref.db_helper,
                &mut state_ref.dict_ctrl,
                &rows,
                &grouped_hits,
                Some(&resp.term_pitch_details),
                Some(&resp.kanji_frequencies),
                compact_tags,
            );
            apply_kanji_freq_updates_to_rows(&mut rows, kanji_freq_updates);
            let row_count = rows.len();
            state_ref.panel.rows = rows.clone();
            state_ref.panel.hits = grouped_hits;
            state_ref.panel.rich_epoch = state_ref.panel.rich_epoch.wrapping_add(1).max(1);
            state_ref.panel.row_extras = row_extras;
            state_ref.panel.glossary_cache.clear();
            state_ref.panel.last_rendered_key.clear();
            state_ref.panel.selections.clear();
            state_ref.panel.active_drag_selection_key = None;
            state_ref.panel.expanded_cards.clear();
            state_ref.panel.row_anki_duplicates = vec![false; row_count];
            state_ref.panel.row_anki_note_ids_i64 = vec![0; row_count];
            state_ref.panel.row_anki_browse_queries = vec![SharedString::default(); row_count];
            state_ref.panel.row_anki_tags = vec![SharedString::default(); row_count];
            state_ref.panel.row_anki_addable = vec![true; row_count];
            state_ref.panel.row_reading_as_expression = vec![false; row_count];
            state_ref.panel.term_kanji_statuses.clear();
            state_ref.panel.term_kana_statuses.clear();
            state_ref.panel.manual_reading_as_expr.clear();
        }
        clear_active_glossary_selections(self.instance_id, Some(-1));
        self.sync_panel_popup_to_bridge(app, true);
        let pb = app.global::<PopupBridge>();
        pb.set_panel_searching(false);
        self.dispatch_anki_check_for_rows(app, &rows, AnkiCheckTarget::Panel);
        true
    }

    pub(super) fn rebuild_row_extras_for_compact_tags(&self, app: &SubomiApp) {
        let root_model = app.get_dict_results();
        let root_rows: Vec<DictResult> = (0..root_model.row_count())
            .filter_map(|i| root_model.row_data(i))
            .collect();
        let mut st_guard = self.state.borrow_mut();
        let st = &mut *st_guard;
        let compact_tags = st.settings.compact_tags;
        st.cached_compact_tags = compact_tags;
        if !st.current_hits.is_empty() && st.current_hits.len() == root_rows.len() {
            let hits = st.current_hits.clone();
            let (extras, _) = build_row_extras_for_grouped(
                &mut st.db_helper,
                &mut st.dict_ctrl,
                &root_rows,
                &hits,
                None,
                None,
                compact_tags,
            );
            st.row_extras = extras;
        }
        for lvl in &mut st.nested_stack {
            if !lvl.hits.is_empty() && lvl.hits.len() == lvl.rows.len() {
                let (extras, _) = build_row_extras_for_grouped(
                    &mut st.db_helper,
                    &mut st.dict_ctrl,
                    &lvl.rows,
                    &lvl.hits,
                    None,
                    None,
                    compact_tags,
                );
                lvl.row_extras = extras;
            }
        }
        if !st.panel.hits.is_empty() && st.panel.hits.len() == st.panel.rows.len() {
            let (extras, _) = build_row_extras_for_grouped(
                &mut st.db_helper,
                &mut st.dict_ctrl,
                &st.panel.rows,
                &st.panel.hits,
                None,
                None,
                compact_tags,
            );
            st.panel.row_extras = extras;
        }
    }

    pub(super) fn ensure_fresh_db_and_config(&self, app: &SubomiApp, force: bool) {
        let mut sub_style_update: Option<(bool, SubtitleRenderStyle)> = None;
        let mut reloaded_css_theme: Option<Arc<PopupCssTheme>> = None;
        let mut reloaded_max_depth: Option<usize> = None;
        let mut truncated_nested = false;
        let mut middle_mouse_update: Option<bool> = None;
        let mut theme_and_compact_sync: Option<(bool, bool, bool)> = None;
        let mut compact_tags_changed = false;
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
            let prev_is_light = st.settings.is_light_theme;
            let prev_compact_glossaries = st.settings.compact_glossaries;
            let prev_compact_tags = st.settings.compact_tags;

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
                if st.settings.is_light_theme != prev_is_light
                    || st.settings.compact_glossaries != prev_compact_glossaries
                    || st.settings.compact_tags != prev_compact_tags
                {
                    theme_and_compact_sync = Some((
                        st.settings.is_light_theme,
                        st.settings.compact_glossaries,
                        st.settings.compact_tags,
                    ));
                }
                if st.settings.compact_tags != prev_compact_tags {
                    compact_tags_changed = true;
                }
            }

            let glossary_visual_changed = css_changed
                || theme_changed
                || compact_tags_changed
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

        if compact_tags_changed {
            self.rebuild_row_extras_for_compact_tags(app);
        }
        if let Some((is_light, compact_gloss, compact_tags)) = theme_and_compact_sync {
            app.set_is_light_theme(is_light);
            let pb = app.global::<PopupBridge>();
            pb.set_compact_glossaries(compact_gloss);
            pb.set_compact_tags(compact_tags);
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
            pb.set_panel_revision(pb.get_panel_revision().wrapping_add(1));
        }
        if let Some((relayout, new_sub_style)) = sub_style_update {
            let bg = new_sub_style.bg_rgba;
            sync_subtitle_overlay_style_to_bridge(app, &new_sub_style);
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

    pub(in crate::ui) fn on_dictionary_rows_updated(&self, app: &SubomiApp, rows: &[DictResult]) {
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

        let (
            can_back,
            can_fwd,
            crumb,
            reset_kana,
            kanji_freq_updates,
            ass_ctrl,
            ass_action,
            old_nested_depth,
            root_hit_meta,
        ) = {
            let mut st = self.state.borrow_mut();
            if !st.suppress_history_reset {
                st.back_stack.clear();
                st.forward_stack.clear();
            }
            let old_nested_depth = st.nested_stack.len();
            st.rich_epoch = st.rich_epoch.wrapping_add(1);
            st.row_extras.clear();
            st.glossary_cache.clear();
            st.last_rendered_key.clear();
            st.selections.clear();
            st.active_drag_selection_key = None;
            st.root_scan_highlight = None;
            st.root_expanded_cards.clear();
            st.root_row_anki_note_ids_i64.clear();
            st.root_row_anki_addable = vec![true; rows.len()];
            st.root_row_reading_as_expression = vec![false; rows.len()];
            st.root_term_kanji_statuses.clear();
            st.root_term_kana_statuses.clear();
            st.root_manual_reading_as_expr.clear();
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
            let mut root_hit_meta = Vec::new();

            if matches_hits {
                st.current_hits = candidate_hits.clone();
                root_hit_meta = candidate_hits
                    .iter()
                    .map(|h| ActiveRowHitMeta {
                        dictionary_id: h.dictionary_id,
                        dict_name: h.dict_name.clone(),
                        raw_glossary: h.raw_glossary.clone(),
                    })
                    .collect();
                let compact_tags = st.settings.compact_tags;
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
                    compact_tags,
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
                old_nested_depth,
                root_hit_meta,
            )
        };

        clear_active_glossary_selections(self.instance_id, Some(0));
        set_active_row_hit_meta(self.instance_id, 0, root_hit_meta);
        for d in 1..=old_nested_depth {
            clear_active_glossary_selections(self.instance_id, Some(d as i32));
            clear_active_row_hit_meta(self.instance_id, Some(d as i32));
        }

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
        pb.set_selected_glossary_text("".into());
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
            {
                let mut st = self.state.borrow_mut();
                st.panel.rows = rows.to_vec();
                st.panel.hits = st.current_hits.clone();
                st.panel.row_extras = st.row_extras.clone();
                st.panel.rich_epoch = st.panel.rich_epoch.wrapping_add(1).max(1);
                st.panel.glossary_cache.clear();
                st.panel.last_rendered_key.clear();
                st.panel.selections.clear();
                st.panel.active_drag_selection_key = None;
                st.panel.expanded_cards.clear();
                st.panel.row_anki_duplicates = vec![false; rows.len()];
                st.panel.row_anki_note_ids_i64 = vec![0; rows.len()];
                st.panel.row_anki_browse_queries = vec![SharedString::default(); rows.len()];
                st.panel.row_anki_tags = vec![SharedString::default(); rows.len()];
                st.panel.row_anki_addable = vec![true; rows.len()];
                st.panel.row_reading_as_expression = vec![false; rows.len()];
                st.panel.term_kanji_statuses.clear();
                st.panel.term_kana_statuses.clear();
                st.panel.manual_reading_as_expr.clear();
            }
            self.sync_panel_popup_to_bridge(app, true);
        }
        pb.set_rich_revision(pb.get_rich_revision().wrapping_add(1));
        pb.set_selection_revision(pb.get_selection_revision().wrapping_add(1));
        self.sync_nested_popup_to_bridge(app, true);

        let sb = app.global::<SubtitleOverlayBridge>();
        sb.set_highlight_revision(sb.get_highlight_revision().wrapping_add(1));

        if matches_hits && !rows.is_empty() {
            self.dispatch_glossary_prerender(app, rows);
        }

        self.dispatch_anki_check_for_rows(app, rows, AnkiCheckTarget::Root);
    }

    fn snapshot_current_popup(&self, app: &SubomiApp) -> Option<PopupHistoryEntry> {
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

    fn restore_history_entry(&self, app: &SubomiApp, entry: PopupHistoryEntry) {
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
        app: &SubomiApp,
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
        app: &SubomiApp,
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

    fn apply_root_nav_lookup_response(&self, app: &SubomiApp, resp: NavLookupResponse) {
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

                let (compact_glossaries, compact_tags) = {
                    let st = self.state.borrow();
                    (st.settings.compact_glossaries, st.settings.compact_tags)
                };
                let mut rows = dictionary_result_rows_with_options(
                    &resp.hits,
                    &resp.excluded,
                    None,
                    compact_glossaries,
                    compact_tags,
                );
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

                let (compact_glossaries, compact_tags) = {
                    let st = self.state.borrow();
                    (st.settings.compact_glossaries, st.settings.compact_tags)
                };
                let rows = dictionary_result_rows_with_options(
                    &resp.hits,
                    &resp.excluded,
                    None,
                    compact_glossaries,
                    compact_tags,
                );
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
pub fn refresh_rich_ui_bridges(app: &SubomiApp, config: &ConfigController) {
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
        compact_tags_changed,
        is_light_theme,
        compact_glossaries,
        compact_tags,
        popup_theme,
        panel_rows_for_anki,
    ) = {
        let mut st = bridge.state.borrow_mut();
        let prev_fonts = st.settings.popup_fonts.clone();
        let prev_glossary_style = st.settings.glossary_style;
        let prev_sub_style = st.settings.subtitle_style.clone();
        let prev_compact_tags = st.settings.compact_tags;

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

        let compact_tags_changed = st.settings.compact_tags != prev_compact_tags;
        let css_changed = st.stylesheets.force_reload_user_css();
        let theme_changed = st.refresh_cached_theme(css_changed);
        let glossary_visual_changed = css_changed
            || theme_changed
            || compact_tags_changed
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
        let panel_rows = (!st.panel.rows.is_empty()).then(|| st.panel.rows.clone());
        (
            st.settings.middle_mouse_scan,
            max_depth,
            truncated,
            st.settings.subtitle_style.clone(),
            sub_style_changed,
            glossary_visual_changed,
            compact_tags_changed,
            st.settings.is_light_theme,
            st.settings.compact_glossaries,
            st.settings.compact_tags,
            st.effective_popup_theme(),
            panel_rows,
        )
    };

    if compact_tags_changed {
        bridge.rebuild_row_extras_for_compact_tags(app);
    }

    app.set_is_light_theme(is_light_theme);
    let pb = app.global::<PopupBridge>();
    pb.set_compact_glossaries(compact_glossaries);
    pb.set_compact_tags(compact_tags);
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
        pb.set_panel_revision(pb.get_panel_revision().wrapping_add(1));
    }
    if app.get_dict_open() {
        let full = app.get_dict_results();
        let current_rows: Vec<DictResult> = (0..full.row_count())
            .filter_map(|i| full.row_data(i))
            .collect();
        bridge.dispatch_anki_check_for_rows(app, &current_rows, AnkiCheckTarget::Root);
    }
    if app.get_dict_search_open() {
        if let Some(panel_rows) = panel_rows_for_anki {
            bridge.dispatch_anki_check_for_rows(app, &panel_rows, AnkiCheckTarget::Panel);
        }
    }

    let sb = app.global::<SubtitleOverlayBridge>();
    sb.set_middle_mouse_scan_enabled(middle_mouse);
    let sub_bg = sub_style.bg_rgba;
    sb.set_subtitle_bg_color(slint::Color::from_argb_u8(
        sub_bg[3], sub_bg[0], sub_bg[1], sub_bg[2],
    ));
    sync_subtitle_overlay_style_to_bridge(app, &sub_style);
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
pub fn bind_rich_bridge_lookup_epoch(app: &SubomiApp, lookup_epoch: Arc<AtomicU64>) {
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
    app: &SubomiApp,
    ass_controller: Rc<RefCell<crate::controller::ass::AssOverlayController>>,
) {
    if let Some(bridge) = active_rich_bridge_for_app(app) {
        bridge.state.borrow_mut().ass_controller = Some(ass_controller);
    }
}

/// Immediately clears any active ASS lookup highlight / anchor on the bound `AssOverlayController`.
pub fn clear_rich_bridge_ass_lookup(app: &SubomiApp) {
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
pub fn active_subtitle_scan_utf16_range(app: &SubomiApp) -> Option<(usize, usize)> {
    active_rich_bridge_for_app(app)
        .and_then(|bridge| bridge.state.borrow().active_scan_utf16_range)
}

/// Returns the active ASS per-speaker lookup context (if the open popup came from
/// hovering/clicking a native ASS line) for accurate `{subtitle}`, `{cloze-*}`, and `{audio-media}`.
pub fn active_ass_lookup_for_app(
    app: &SubomiApp,
) -> Option<crate::controller::ass::AssActiveLookup> {
    if !app.global::<SubtitleOverlayBridge>().get_ass_native_active() {
        return None;
    }
    let ass_ctrl = active_rich_bridge_for_app(app)
        .and_then(|bridge| bridge.state.borrow().ass_controller.clone())?;
    let lookup = ass_ctrl.try_borrow().ok()?.active_lookup().cloned();
    lookup
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::anki_actions::get_active_glossary_selections;
    use crate::controller::anki::EntryAnkiStatus;
    use crate::controller::dict::{SearchHit, TermPitchDetail};
    use crate::ui::popup_css::CUSTOM_POPUP_CSS_FILENAME;
    use crate::ui::popup_model::{dictionary_result_rows, install_popup_navigation};
    use crate::ui::PopupLayout;
    use subomi_dict::yomidbbuilder::prepare_db;
    use subomi_setting::keys as raw;
    use state::{
        build_rich_pitch_entries_from_details, classify_pitch_category, CachedConfigSettings,
    };

    fn create_test_dict_db(path: &std::path::Path) {
        prepare_db(path).unwrap();
        let conn = subomi_dict::sqlite_db::open_read_write_connection(path).unwrap();
        conn.execute(
            "INSERT INTO directory (dic_id, title, format, revision, sequenced) VALUES \
             (1, 'Jitendex', 3, '1', 1), \
             (2, 'Daijirin', 3, '1', 1);",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO term_bank (dic_id, expression, reading, def_tags, rules, score, glossary, sequence, term_tags) VALUES \
             (1, '食べる', 'たべる', 'v1 ichidan', 'v1', 100, '[\"to eat food\"]', 1, 'common'), \
             (2, '食べる', 'たべる', 'v1 ichidan', 'v1', 90, '[\"consume meal\"]', 2, 'common'), \
             (1, '食べログ', 'たべろぐ', 'n', '', 80, '[\"restaurant review site\"]', 3, ''), \
             (1, '世界', 'せかい', 'n', '', 70, '[\"the world\"]', 4, '');",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO kanji_bank (dic_id, char, onyomi, kunyomi, tags, meanings, stats) VALUES \
             (1, '食', 'ショク', 'た.べる', '', '[\"eat\",\"food\"]', '{}');",
            [],
        )
        .unwrap();
    }

    #[test]
    fn multi_definition_selection_persists_across_definitions_and_clears_on_escape_and_lookup() {
        i_slint_backend_testing::init_no_event_loop();
        let dir = tempfile::TempDir::new().unwrap();
        let config_dir = dir.path().join("cfg");
        std::fs::create_dir_all(&config_dir).unwrap();
        let db_path = dir.path().join("dictionaries.sqlite");
        create_test_dict_db(&db_path);

        let cfg = ConfigController::new();
        let app = SubomiApp::new().unwrap();
        install_popup_navigation(&app);
        install_rich_ui_bridges(&app, &db_path, &config_dir, &cfg);

        let mut dict = DictController::open(&db_path).unwrap();
        let hits = dict.search("食べる").to_vec();
        assert!(hits.len() >= 2);
        set_dictionary_rows(&app, dictionary_result_rows(&hits, &[]));
        app.set_dict_open(true);

        let pb = app.global::<PopupBridge>();
        let bridge_id = pb.get_bridge_instance_id();
        let rev = pb.get_rich_revision();
        let sel_rev = pb.get_selection_revision();
        let r1 = pb.invoke_render_glossary(1, 320.0, true, rev, sel_rev);
        let r2 = pb.invoke_render_glossary(2, 320.0, true, rev, sel_rev);
        assert!(r1.has_rich && r2.has_rich);

        // Double-click word in definition 1, then double-click word in definition 2.
        pb.invoke_glossary_double_clicked(1, 10.0, 10.0);
        pb.invoke_glossary_double_clicked(2, 10.0, 10.0);

        let active = get_active_glossary_selections(bridge_id, 0);
        assert_eq!(
            active.len(),
            2,
            "both definitions' selections must persist in order: {active:?}"
        );
        assert_eq!(pb.get_selected_glossary_text().as_str(), active.join("\n"));

        // Escape clears all selections cleanly.
        assert!(pb.invoke_popup_key_pressed("\u{1b}".into(), false, false, false, false));
        assert!(get_active_glossary_selections(bridge_id, 0).is_empty());
        assert!(pb.get_selected_glossary_text().is_empty());

        // New lookup also clears selections.
        pb.invoke_glossary_double_clicked(1, 10.0, 10.0);
        assert_eq!(get_active_glossary_selections(bridge_id, 0).len(), 1);
        let world_hits = dict.search("世界").to_vec();
        set_dictionary_rows(&app, dictionary_result_rows(&world_hits, &[]));
        assert!(get_active_glossary_selections(bridge_id, 0).is_empty());
        assert!(pb.get_selected_glossary_text().is_empty());
    }

    #[test]
    fn light_dark_theme_switch_propagates_css_theme_and_custom_root_vars() {
        i_slint_backend_testing::init_no_event_loop();
        let dir = tempfile::TempDir::new().unwrap();
        let config_dir = dir.path().join("cfg");
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::write(
            config_dir.join(CUSTOM_POPUP_CSS_FILENAME),
            ":root { --bg: #112233; }\n\
             :root[data-theme=\"light\"] { --bg: #fafbfc; }\n\
             body { background-color: var(--bg); }\n",
        )
        .unwrap();
        let db_path = dir.path().join("dictionaries.sqlite");
        create_test_dict_db(&db_path);

        let cfg = ConfigController::new();
        cfg.edit_setting("interface/theme", "dark").unwrap();
        let app = SubomiApp::new().unwrap();
        install_rich_ui_bridges(&app, &db_path, &config_dir, &cfg);

        let pb = app.global::<PopupBridge>();
        assert!(!app.get_is_light_theme());
        assert_eq!(
            pb.get_css_popup_bg(),
            slint::Color::from_argb_u8(0xFF, 0x11, 0x22, 0x33)
        );
        let rev_before = pb.get_rich_revision();

        cfg.edit_setting("interface/theme", "light").unwrap();
        refresh_rich_ui_bridges(&app, &cfg);

        assert!(app.get_is_light_theme());
        assert_eq!(
            pb.get_css_popup_bg(),
            slint::Color::from_argb_u8(0xFF, 0xFA, 0xFB, 0xFC)
        );
        assert_ne!(pb.get_rich_revision(), rev_before);
        let bridge = active_rich_bridge_for_app(&app).unwrap();
        assert!(bridge.state.borrow().effective_popup_theme().is_light_theme());
    }

    #[test]
    fn compact_tags_compact_frequencies_and_exact_string_pitch_entries() {
        i_slint_backend_testing::init_no_event_loop();
        let dir = tempfile::TempDir::new().unwrap();
        let config_dir = dir.path().join("cfg");
        std::fs::create_dir_all(&config_dir).unwrap();
        let db_path = dir.path().join("dictionaries.sqlite");
        create_test_dict_db(&db_path);

        let cfg = ConfigController::new();
        cfg.edit_setting("interface/compact-tags", "true").unwrap();
        let app = SubomiApp::new().unwrap();
        install_popup_navigation(&app);
        install_rich_ui_bridges(&app, &db_path, &config_dir, &cfg);

        let mut dict = DictController::open(&db_path).unwrap();
        let hits = dict.search("食べる").to_vec();
        let mut rows = dictionary_result_rows(&hits, &[]);
        rows[0].frequencies = Rc::new(VecModel::from(vec![
            PopupFrequency {
                dictionary: "JPDB".into(),
                value: "123".into(),
            },
            PopupFrequency {
                dictionary: "JPDB".into(),
                value: "456㋕".into(),
            },
            PopupFrequency {
                dictionary: "BCCWJ".into(),
                value: "99".into(),
            },
        ]))
        .into();
        set_dictionary_rows(&app, rows);

        let pb = app.global::<PopupBridge>();
        let rev = pb.get_rich_revision();

        // Compact tags deduplicates repeated `v1`/`ichidan` pills on the second sense.
        let pills_1 = pb.invoke_def_tag_pills(1, rev);
        let pills_2 = pb.invoke_def_tag_pills(2, rev);
        assert!(pills_1.row_count() >= 1);
        assert_eq!(pills_2.row_count(), 0);

        // Compact frequency badges omit repeated consecutive dictionary names.
        let compact_freqs = pb.invoke_row_compact_frequencies(0, 1, rev);
        assert_eq!(compact_freqs.row_count(), 3);
        assert!(!compact_freqs.row_data(0).unwrap().omit_dict_name);
        assert!(compact_freqs.row_data(1).unwrap().omit_dict_name);
        assert!(!compact_freqs.row_data(2).unwrap().omit_dict_name);

        let lines = app
            .global::<PopupLayout>()
            .invoke_compact_frequency_lines(compact_freqs, 120.0);
        assert!(lines.row_count() >= 1);

        // Exact string pitch patterns ("LHH", "HLL") & consecutive repeated dictionary hiding.
        let details = vec![
            TermPitchDetail {
                dictionary: "NHKJ".into(),
                mora: vec!["た".into(), "べ".into(), "る".into()],
                position: 0,
                pattern: Some("LHH".into()),
                nasal: Vec::new(),
                devoice: Vec::new(),
                tags: Vec::new(),
            },
            TermPitchDetail {
                dictionary: "NHKJ".into(),
                mora: vec!["た".into(), "べ".into(), "る".into()],
                position: 1,
                pattern: Some("HLL".into()),
                nasal: Vec::new(),
                devoice: Vec::new(),
                tags: Vec::new(),
            },
        ];
        let entries = build_rich_pitch_entries_from_details(&details, true);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].pattern_text.as_str(), "LHH");
        assert!(!entries[0].hide_dict_name);
        assert_eq!(entries[1].pattern_text.as_str(), "HLL");
        assert!(entries[1].hide_dict_name);
        let graphs = crate::ui::popup_model::rich_pitch_graphs_from_details(&details);
        assert_eq!(classify_pitch_category(&graphs, &details, "n"), Some("heiban"));
        assert_eq!(
            classify_pitch_category(&graphs[1..], &details[1..], "n"),
            Some("atamadaka")
        );
    }

    #[test]
    fn interactive_panel_wildcards_select_collapse_and_reading_as_expression_toggle() {
        i_slint_backend_testing::init_no_event_loop();
        let dir = tempfile::TempDir::new().unwrap();
        let config_dir = dir.path().join("cfg");
        std::fs::create_dir_all(&config_dir).unwrap();
        let db_path = dir.path().join("dictionaries.sqlite");
        create_test_dict_db(&db_path);

        let cfg = ConfigController::new();
        let app = SubomiApp::new().unwrap();
        install_popup_navigation(&app);
        install_rich_ui_bridges(&app, &db_path, &config_dir, &cfg);

        let pb = app.global::<PopupBridge>();
        assert!(pb.get_panel_rich_enabled());

        // Wildcard search in panel (`食べ*`) finds both `食べる` and `食べログ`.
        pb.invoke_panel_search_requested("食べ*".into());
        let deadline = Instant::now() + Duration::from_secs(3);
        while pb.get_panel_searching() && Instant::now() < deadline {
            i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(10));
            std::thread::sleep(Duration::from_millis(10));
        }
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(10));
        let panel_rows = pb.get_panel_results();
        assert!(
            panel_rows.row_count() >= 2,
            "wildcard panel search must return matches, got {}",
            panel_rows.row_count()
        );

        // Toggle definition selection & term collapse on panel rows.
        let initial_sel = panel_rows.row_data(0).unwrap().selected;
        pb.invoke_panel_toggle_select_definition(0);
        assert_eq!(
            pb.get_panel_results().row_data(0).unwrap().selected,
            !initial_sel
        );
        assert!(!pb.get_panel_results().row_data(0).unwrap().collapsed);
        pb.invoke_panel_toggle_term_collapse(0);
        assert!(pb.get_panel_results().row_data(0).unwrap().collapsed);

        // `readingAsExpression` auto-toggle + manual toggle on root popup.
        let hit = SearchHit::new("食べる", "たべる", "to eat", "v1", 10, false);
        set_dictionary_rows(&app, dictionary_result_rows(&[hit], &[]));
        let br = active_rich_bridge_for_app(&app).unwrap();
        {
            let mut st = br.state.borrow_mut();
            st.root_term_kanji_statuses.insert(
                ("食べる".into(), "たべる".into()),
                EntryAnkiStatus {
                    can_add: false,
                    is_duplicate: true,
                    note_ids: vec![42],
                    tags: vec!["mined".into()],
                    browse_query: "nid:42".into(),
                },
            );
            st.root_term_kana_statuses.insert(
                ("食べる".into(), "たべる".into()),
                EntryAnkiStatus {
                    can_add: true,
                    is_duplicate: false,
                    note_ids: Vec::new(),
                    tags: Vec::new(),
                    browse_query: String::new(),
                },
            );
        }
        // Manual toggle enables reading_as_expression -> switches to kana (addable, non-duplicate).
        pb.invoke_toggle_reading_as_expression(0);
        assert_eq!(pb.get_row_reading_as_expression().row_data(0), Some(true));
        assert_eq!(pb.get_row_anki_addable().row_data(0), Some(true));
        assert_eq!(pb.get_row_anki_duplicates().row_data(0), Some(false));
        // Second toggle switches back to kanji (duplicate, not addable).
        pb.invoke_toggle_reading_as_expression(0);
        assert_eq!(pb.get_row_reading_as_expression().row_data(0), Some(false));
        assert_eq!(pb.get_row_anki_addable().row_data(0), Some(false));
        assert_eq!(pb.get_row_anki_duplicates().row_data(0), Some(true));
    }

    #[test]
    fn search_kanji_font_cap_relaxed_to_200_and_font_attrs_preserved() {
        let dir = tempfile::TempDir::new().unwrap();
        let cfg = ConfigController::new();
        // 135pt * 96/72 = 180px (exceeds old 96px cap, stays within new 200px cap).
        cfg.edit_setting(
            &format!("{}/{}", raw::interface::GROUP, raw::interface::SEARCH_KANJI_FONT),
            "KanjiStrokeOrders,135,-1,5,50,0,1,1,0,0,letterSpacing=2.0",
        )
        .unwrap();
        let cached = CachedConfigSettings::from_controller(&cfg, dir.path());
        let kanji_font = cached.popup_fonts.kanji.expect("custom kanji font parsed");
        assert!((kanji_font.size_px - 180.0).abs() < 1e-3);
        let attrs = cached
            .popup_font_attrs
            .get(raw::interface::SEARCH_KANJI_FONT)
            .expect("extra font attributes captured");
        assert!(attrs.underline);
        assert!(attrs.strikeout);
        assert!((attrs.letter_spacing - 2.0).abs() < 1e-3);
    }

    #[test]
    fn popup_hover_tooltip_timer_starts_cancels_and_restarts_cleanly() {
        i_slint_backend_testing::init_no_event_loop();
        let dir = tempfile::TempDir::new().unwrap();
        let config_dir = dir.path().join("cfg");
        std::fs::create_dir_all(&config_dir).unwrap();
        let db_path = dir.path().join("dictionaries.sqlite");
        create_test_dict_db(&db_path);

        let cfg = ConfigController::new();
        let app = SubomiApp::new().unwrap();
        install_popup_navigation(&app);
        install_rich_ui_bridges(&app, &db_path, &config_dir, &cfg);
        app.show().unwrap();

        let pb = app.global::<PopupBridge>();
        assert_eq!(pb.get_tooltip_hover_delay_ms(), 500);
        assert!(!pb.get_tooltip_visible());

        // 1) First hover starts the previously-stopped timer and fires after 500ms.
        pb.invoke_request_popup_tooltip("Search by kana reading".into(), 24.0, 60.0);
        // Flush Slint's ChangeTracker queue at t=0 (done automatically at the end of
        // WindowInner::process_mouse_input in real UI pointer events).
        i_slint_backend_testing::mock_elapsed_time(Duration::ZERO);
        assert!(!pb.get_tooltip_visible(), "delayed tooltip must not appear immediately");
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(400));
        assert!(!pb.get_tooltip_visible(), "delayed tooltip must not fire before 500ms");
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(150));
        assert!(
            pb.get_tooltip_visible(),
            "delayed tooltip timer must start and show tooltip once 500ms elapses"
        );
        assert_eq!(pb.get_tooltip_text().as_str(), "Search by kana reading");

        // 2) Leaving the hovered target cancels the pending tooltip before it fires.
        pb.invoke_hide_tooltip();
        assert!(!pb.get_tooltip_visible());
        pb.invoke_request_popup_tooltip("Kanji info: 亡".into(), 30.0, 64.0);
        i_slint_backend_testing::mock_elapsed_time(Duration::ZERO);
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(250));
        pb.invoke_cancel_pending_tooltip();
        i_slint_backend_testing::mock_elapsed_time(Duration::ZERO);
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(500));
        assert!(
            !pb.get_tooltip_visible(),
            "cancelled pending tooltip must never appear after mouse leaves"
        );

        // 3) Moving straight to another target restarts the 500ms countdown cleanly.
        pb.invoke_request_popup_tooltip("Play audio".into(), 100.0, 40.0);
        i_slint_backend_testing::mock_elapsed_time(Duration::ZERO);
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(300));
        assert!(!pb.get_tooltip_visible());
        pb.invoke_request_popup_tooltip("Kanji info: 食".into(), 40.0, 64.0);
        i_slint_backend_testing::mock_elapsed_time(Duration::ZERO);
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(300));
        assert!(
            !pb.get_tooltip_visible(),
            "re-requesting tooltip must reset the 500ms delay"
        );
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(250));
        assert!(pb.get_tooltip_visible());
        assert_eq!(pb.get_tooltip_text().as_str(), "Kanji info: 食");

        app.hide().unwrap();
    }
}

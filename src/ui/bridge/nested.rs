use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::atomic::Ordering;

use slint::{ComponentHandle, SharedString, VecModel};

use crate::ui::glossary_render::{extract_glossary_scan_substring, strict_char_index_at_point};
use crate::ui::popup_model::{
    active_rich_bridge_for_app, build_dictionary_result_rows_grouped, is_kanji_char,
    popup_pitch_graphs,
};
use crate::ui::{DictResult, MementoApp, PopupBridge, PopupFrequency};

use super::state::{
    apply_kanji_freq_updates_to_rows, build_row_extras_for_grouped, encode_note_id_i32,
    modifier_matches_setting, GlossaryDocLruCache, GlossaryScanHighlight, HoveredGlossaryTarget,
    NestedPopupLevel, NestedScanRequest, NestedScanResponse, PendingNestedAnkiAdd,
    GLOSSARY_SCAN_MAX_CHARS, MAX_PENDING_NESTED_ANKI_ADDS, NESTED_DISMISS_GRACE_MS,
};
use super::RichUiBridge;

/// RAII guard that deterministically restores `app.dict_results` after temporarily exposing
/// nested popup rows to `app.invoke_add_anki_note(...)`, even on early return or panic.
struct RestoreDictResultsGuard<'a> {
    app: &'a MementoApp,
    swapped: slint::ModelRc<DictResult>,
    parent: slint::ModelRc<DictResult>,
}

impl Drop for RestoreDictResultsGuard<'_> {
    fn drop(&mut self) {
        if self.app.get_dict_results() == self.swapped {
            self.app.set_dict_results(self.parent.clone());
        }
    }
}

pub(super) fn drain_nested_scan_results_for_app(app: &MementoApp) -> bool {
    if let Some(br) = active_rich_bridge_for_app(app) {
        br.drain_nested_scan_results(app)
    } else {
        false
    }
}

impl RichUiBridge {
    pub(super) fn sync_nested_popup_to_bridge(&self, app: &MementoApp, nav_changed: bool) {
        let (
            depth,
            max_depth,
            locked,
            breadcrumb,
            headword,
            reading,
            anchor_x,
            anchor_y,
            rows,
            dups,
            note_ids,
            queries,
            tags,
        ) = {
            let mut st = self.state.borrow_mut();
            if nav_changed {
                st.nested_body_hover_count = 0;
            }
            let depth = st.nested_stack.len();
            let max_depth = st.settings.max_recursion_depth;
            if let Some(top) = st.nested_stack.last() {
                let crumb = st
                    .nested_stack
                    .iter()
                    .map(|lvl| lvl.label.as_str())
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>()
                    .join(" › ");
                let note_ids: Vec<i32> = top
                    .row_anki_note_ids_i64
                    .iter()
                    .copied()
                    .map(encode_note_id_i32)
                    .collect();
                (
                    depth,
                    max_depth,
                    top.locked,
                    crumb,
                    top.headword.clone(),
                    top.reading.clone(),
                    top.anchor_x,
                    top.anchor_y,
                    top.rows.clone(),
                    top.row_anki_duplicates.clone(),
                    note_ids,
                    top.row_anki_browse_queries.clone(),
                    top.row_anki_tags.clone(),
                )
            } else {
                (
                    0,
                    max_depth,
                    false,
                    String::new(),
                    String::new(),
                    String::new(),
                    0.0,
                    0.0,
                    Vec::new(),
                    Vec::new(),
                    Vec::new(),
                    Vec::new(),
                    Vec::new(),
                )
            }
        };
        let pb = app.global::<PopupBridge>();
        pb.set_nested_depth(depth as i32);
        pb.set_nested_max_depth(max_depth as i32);
        pb.set_nested_locked(locked);
        pb.set_nested_breadcrumb(breadcrumb.into());
        pb.set_nested_headword(headword.into());
        pb.set_nested_reading(reading.into());
        pb.set_nested_anchor_x(anchor_x);
        pb.set_nested_anchor_y(anchor_y);
        pb.set_nested_results(Rc::new(VecModel::from(rows)).into());
        pb.set_nested_row_anki_duplicates(Rc::new(VecModel::from(dups)).into());
        pb.set_nested_row_anki_note_ids(Rc::new(VecModel::from(note_ids)).into());
        pb.set_nested_row_anki_browse_queries(Rc::new(VecModel::from(queries)).into());
        pb.set_nested_row_anki_tags(Rc::new(VecModel::from(tags)).into());
        pb.set_nested_revision(pb.get_nested_revision().wrapping_add(1));
        if nav_changed {
            pb.set_nested_nav_revision(pb.get_nested_nav_revision().wrapping_add(1));
        }
        if depth == 0 {
            pb.set_nested_searching(false);
        }
    }

    pub(super) fn pop_nested_popup(&self, app: &MementoApp) {
        self.nested_epoch.fetch_add(1, Ordering::AcqRel);
        let popped = {
            let mut st = self.state.borrow_mut();
            st.nested_scan_timer.stop();
            st.nested_dismiss_timer.stop();
            let popped = st.nested_stack.pop().is_some();
            let new_len = st.nested_stack.len();
            st.nested_body_hover_count = 0;
            st.pending_nested_anki_adds.retain(|p| p.depth <= new_len);
            if let Some(top) = st.nested_stack.last_mut() {
                top.scan_highlight = None;
            } else {
                st.root_scan_highlight = None;
                st.nested_card_hovered = false;
                st.pending_nested_anki_adds.clear();
            }
            popped
        };
        self.cancel_nested_anki_check(app);
        if popped {
            let pb = app.global::<PopupBridge>();
            pb.set_tooltip_visible(false);
            pb.set_nested_searching(false);
            self.sync_nested_popup_to_bridge(app, true);
            pb.set_rich_revision(pb.get_rich_revision().wrapping_add(1));
        }
    }

    pub(super) fn dismiss_all_nested_popups(&self, app: &MementoApp) {
        self.nested_epoch.fetch_add(1, Ordering::AcqRel);
        let had_any = {
            let mut st = self.state.borrow_mut();
            st.nested_scan_timer.stop();
            st.nested_dismiss_timer.stop();
            let had_any = !st.nested_stack.is_empty() || st.root_scan_highlight.is_some();
            st.nested_stack.clear();
            st.root_scan_highlight = None;
            st.nested_card_hovered = false;
            st.nested_body_hover_count = 0;
            st.pending_nested_anki_adds.clear();
            had_any
        };
        self.cancel_nested_anki_check(app);
        let pb = app.global::<PopupBridge>();
        pb.set_nested_searching(false);
        if had_any {
            pb.set_tooltip_visible(false);
            self.sync_nested_popup_to_bridge(app, true);
            pb.set_rich_revision(pb.get_rich_revision().wrapping_add(1));
        }
    }

    pub(super) fn schedule_unlocked_nested_dismiss(self: &Rc<Self>) {
        let should_schedule = {
            let st = self.state.borrow();
            let Some(top) = st.nested_stack.last() else {
                return;
            };
            if top.locked || st.nested_card_hovered || st.nested_body_hover_count > 0 {
                false
            } else {
                let parent_depth = (st.nested_stack.len() - 1) as i32;
                let still_on_source = st.hovered_glossary_target.as_ref().is_some_and(|t| {
                    t.depth == parent_depth
                        && t.row_index == top.source_row_index
                        && t.start_char == top.source_start_char
                });
                !still_on_source
            }
        };
        if !should_schedule {
            return;
        }
        let weak_br = Rc::downgrade(self);
        self.state.borrow_mut().nested_dismiss_timer.start(
            slint::TimerMode::SingleShot,
            std::time::Duration::from_millis(NESTED_DISMISS_GRACE_MS),
            move || {
                let Some(br) = weak_br.upgrade() else {
                    return;
                };
                let Some(app) = br.app_weak.upgrade() else {
                    return;
                };
                let dismiss_now = {
                    let st = br.state.borrow();
                    let Some(top) = st.nested_stack.last() else {
                        return;
                    };
                    if top.locked || st.nested_card_hovered || st.nested_body_hover_count > 0 {
                        false
                    } else {
                        let parent_depth = (st.nested_stack.len() - 1) as i32;
                        let still_on_source = st.hovered_glossary_target.as_ref().is_some_and(|t| {
                            t.depth == parent_depth
                                && t.row_index == top.source_row_index
                                && t.start_char == top.source_start_char
                        });
                        !still_on_source
                    }
                };
                if dismiss_now {
                    br.pop_nested_popup(&app);
                }
            },
        );
    }

    pub(super) fn dispatch_nested_scan(
        &self,
        app: &MementoApp,
        target: HoveredGlossaryTarget,
        locked: bool,
    ) {
        let (max_depth, excluded, search_config) = {
            let st = self.state.borrow();
            let max_depth = st.settings.max_recursion_depth;
            if target.depth < 0 || (target.depth as usize) >= max_depth {
                return;
            }
            st.nested_scan_timer.stop();
            st.nested_dismiss_timer.stop();
            (
                max_depth,
                st.settings.excluded_glossaries.clone(),
                st.settings.dict_search_config(),
            )
        };
        if max_depth == 0 {
            return;
        }
        let epoch = self.nested_epoch.fetch_add(1, Ordering::AcqRel) + 1;
        app.global::<PopupBridge>().set_nested_searching(true);
        let _ = self.nested_tx.send(NestedScanRequest {
            epoch,
            parent_depth: target.depth,
            row_index: target.row_index,
            start_char: target.start_char,
            query: target.query,
            cell_indices: target.cell_indices,
            anchor_x: target.anchor_x,
            anchor_y: target.anchor_y,
            locked,
            excluded_glossaries: excluded,
            search_config,
        });
        self.kick_no_event_loop_pump();
    }

    pub(super) fn drain_nested_scan_results(&self, app: &MementoApp) -> bool {
        let mut latest: Option<NestedScanResponse> = None;
        if let Ok(guard) = self.nested_res_rx.lock() {
            while let Ok(item) = guard.try_recv() {
                latest = Some(item);
            }
        }
        let Some(resp) = latest else {
            return false;
        };
        if resp.epoch != self.nested_epoch.load(Ordering::Acquire) {
            return false;
        }
        let pb = app.global::<PopupBridge>();
        pb.set_nested_searching(false);
        if !app.get_dict_open() {
            return false;
        }
        if resp.hits.is_empty() {
            return true;
        }

        let (mut rows, grouped_hits) =
            build_dictionary_result_rows_grouped(&resp.hits, &resp.excluded_glossaries);
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

        let new_depth_opt = {
            let mut st = self.state.borrow_mut();
            let parent_depth = resp.parent_depth.max(0) as usize;
            let max_depth = st.settings.max_recursion_depth;
            if parent_depth > st.nested_stack.len() || parent_depth >= max_depth {
                None
            } else {
                st.nested_stack.truncate(parent_depth);
                st.nested_body_hover_count = 0;
                st.pending_nested_anki_adds
                    .retain(|p| p.depth <= parent_depth);
                let hl_count = resp
                    .matched_char_count
                    .clamp(1, resp.cell_indices.len().max(1));
                let highlight = GlossaryScanHighlight {
                    row_index: resp.row_index,
                    cell_indices: resp.cell_indices.into_iter().take(hl_count).collect(),
                };
                if parent_depth == 0 {
                    st.root_scan_highlight = Some(highlight);
                } else if let Some(parent_lvl) = st.nested_stack.get_mut(parent_depth - 1) {
                    parent_lvl.scan_highlight = Some(highlight);
                    parent_lvl.locked = true;
                }
                let state_ref = &mut *st;
                let (row_extras, kanji_freq_updates) = build_row_extras_for_grouped(
                    &mut state_ref.db_helper,
                    &mut state_ref.dict_ctrl,
                    &rows,
                    &grouped_hits,
                    Some(&resp.term_pitch_details),
                );
                apply_kanji_freq_updates_to_rows(&mut rows, kanji_freq_updates);
                let row_count = rows.len();
                let level = NestedPopupLevel {
                    label: resp.headword.clone(),
                    rows: rows.clone(),
                    headword: resp.headword,
                    reading: resp.reading,
                    anchor_x: resp.anchor_x,
                    anchor_y: resp.anchor_y,
                    locked: resp.locked,
                    source_row_index: resp.row_index,
                    source_start_char: resp.start_char,
                    rich_epoch: 1,
                    row_extras,
                    glossary_cache: GlossaryDocLruCache::new_nested(),
                    last_rendered_key: HashMap::new(),
                    selection: None,
                    scan_highlight: None,
                    expanded_cards: HashSet::new(),
                    row_anki_duplicates: vec![false; row_count],
                    row_anki_note_ids_i64: vec![0; row_count],
                    row_anki_browse_queries: vec![SharedString::default(); row_count],
                    row_anki_tags: vec![SharedString::default(); row_count],
                };
                st.nested_stack.push(level);
                Some(st.nested_stack.len())
            }
        };

        if let Some(new_depth) = new_depth_opt {
            self.sync_nested_popup_to_bridge(app, true);
            pb.set_rich_revision(pb.get_rich_revision().wrapping_add(1));
            self.dispatch_anki_check_for_rows(app, &rows, Some(new_depth));
        }
        true
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn handle_glossary_scan_hover(
        self: &Rc<Self>,
        app: &MementoApp,
        depth: i32,
        row_idx: i32,
        x: f32,
        y: f32,
        popup_x: f32,
        popup_y: f32,
        control: bool,
        shift: bool,
        alt: bool,
        meta: bool,
    ) {
        if depth == 0 && row_idx > 0 {
            self.state.borrow_mut().last_active_glossary_row_idx = row_idx;
        }
        let pb = app.global::<PopupBridge>();
        let Some(doc) = self.latest_cached_doc_at_depth(depth, row_idx) else {
            pb.set_tooltip_visible(false);
            return;
        };

        if let Some(zone) = doc.hit_zones.iter().rev().find(|z| {
            z.tooltip.is_some() && x >= z.x && x <= z.x + z.w && y >= z.y && y <= z.y + z.h
        }) {
            if let Some(ref tip) = zone.tooltip {
                pb.set_tooltip_text(tip.clone().into());
                pb.set_tooltip_x(popup_x);
                pb.set_tooltip_y(popup_y);
                pb.set_tooltip_visible(true);
            }
        } else {
            pb.set_tooltip_visible(false);
        }

        // Never hijack explicit links / <details> disclosure headers or active drag-selection.
        let over_action_zone = doc.hit_zones.iter().any(|z| {
            z.action.is_some() && x >= z.x && x <= z.x + z.w && y >= z.y && y <= z.y + z.h
        });
        let (is_dragging, max_depth) = {
            let st = self.state.borrow();
            let dragging = if depth == 0 {
                st.selection.as_ref().is_some_and(|s| s.dragging)
            } else {
                st.nested_level(depth)
                    .and_then(|lvl| lvl.selection.as_ref())
                    .is_some_and(|s| s.dragging)
            };
            (dragging, st.settings.max_recursion_depth)
        };
        if over_action_zone || is_dragging || depth < 0 || (depth as usize) >= max_depth {
            let mut st = self.state.borrow_mut();
            st.hovered_glossary_target = None;
            st.nested_scan_timer.stop();
            return;
        }

        let scan_match = strict_char_index_at_point(&doc, x, y).and_then(|ci| {
            extract_glossary_scan_substring(&doc.selectable_chars, ci, GLOSSARY_SCAN_MAX_CHARS)
                .map(|(q, indices)| (ci, q, indices))
        });

        let Some((char_idx, query, cell_indices)) = scan_match else {
            {
                let mut st = self.state.borrow_mut();
                st.hovered_glossary_target = None;
                st.nested_scan_timer.stop();
            }
            self.schedule_unlocked_nested_dismiss();
            return;
        };

        let target = HoveredGlossaryTarget {
            depth,
            row_index: row_idx,
            start_char: char_idx,
            query,
            cell_indices,
            anchor_x: popup_x,
            anchor_y: popup_y,
        };

        let (method, mod_setting, delay_ms, already_showing_this) = {
            let mut st = self.state.borrow_mut();
            // Clear stale subtitle / ASS hover targets so pressing the scan modifier while
            // hovering a glossary word NEVER re-scans the underlying subtitle instead.
            st.hovered_ass_target = None;
            st.hovered_cluster = None;
            st.scan_timer.stop();
            st.hovered_glossary_target = Some(target.clone());
            let already = st.nested_stack.get(depth as usize).is_some_and(|child| {
                child.source_row_index == row_idx && child.source_start_char == char_idx
            });
            if already {
                st.nested_dismiss_timer.stop();
            }
            (
                st.settings.search_method,
                st.settings.search_modifier,
                st.settings.search_delay_ms,
                already,
            )
        };

        let mod_held = modifier_matches_setting(mod_setting, control, shift, alt, meta);
        if method == 1 && !mod_held {
            self.state.borrow_mut().nested_scan_timer.stop();
            return;
        }
        if already_showing_this {
            self.state.borrow_mut().nested_scan_timer.stop();
            return;
        }

        let wait_ms = if mod_held { delay_ms.min(50) } else { delay_ms };
        if wait_ms == 0 {
            self.dispatch_nested_scan(app, target, false);
            return;
        }

        let expected_depth = depth;
        let expected_row = row_idx;
        let expected_char = char_idx;
        let weak_br = Rc::downgrade(self);
        self.state.borrow_mut().nested_scan_timer.start(
            slint::TimerMode::SingleShot,
            std::time::Duration::from_millis(wait_ms),
            move || {
                let Some(br) = weak_br.upgrade() else {
                    return;
                };
                let Some(app) = br.app_weak.upgrade() else {
                    return;
                };
                let current_opt = {
                    let st = br.state.borrow();
                    let dragging = if expected_depth == 0 {
                        st.selection.as_ref().is_some_and(|s| s.dragging)
                    } else {
                        st.nested_level(expected_depth)
                            .and_then(|l| l.selection.as_ref())
                            .is_some_and(|s| s.dragging)
                    };
                    if dragging {
                        None
                    } else {
                        st.hovered_glossary_target.clone()
                    }
                };
                if let Some(cur) = current_opt {
                    if cur.depth == expected_depth
                        && cur.row_index == expected_row
                        && cur.start_char == expected_char
                    {
                        br.dispatch_nested_scan(&app, cur, false);
                    }
                }
            },
        );
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn handle_glossary_scan_middle_click(
        &self,
        app: &MementoApp,
        depth: i32,
        row_idx: i32,
        x: f32,
        y: f32,
        popup_x: f32,
        popup_y: f32,
    ) {
        self.ensure_fresh_db_and_config(app, false);
        let (enabled, max_depth) = {
            let st = self.state.borrow();
            (st.settings.middle_mouse_scan, st.settings.max_recursion_depth)
        };
        if !enabled || depth < 0 || (depth as usize) >= max_depth {
            return;
        }
        let Some(doc) = self.latest_cached_doc_at_depth(depth, row_idx) else {
            return;
        };
        let Some(char_idx) = strict_char_index_at_point(&doc, x, y) else {
            return;
        };
        let Some((query, cell_indices)) =
            extract_glossary_scan_substring(&doc.selectable_chars, char_idx, GLOSSARY_SCAN_MAX_CHARS)
        else {
            return;
        };
        let target = HoveredGlossaryTarget {
            depth,
            row_index: row_idx,
            start_char: char_idx,
            query,
            cell_indices,
            anchor_x: popup_x,
            anchor_y: popup_y,
        };
        {
            let mut st = self.state.borrow_mut();
            st.hovered_ass_target = None;
            st.hovered_cluster = None;
            st.scan_timer.stop();
            st.hovered_glossary_target = Some(target.clone());
        }
        self.dispatch_nested_scan(app, target, true);
    }

    pub(super) fn handle_glossary_scan_leave(self: &Rc<Self>, depth: i32, row_idx: i32) {
        {
            let mut st = self.state.borrow_mut();
            if st
                .hovered_glossary_target
                .as_ref()
                .is_some_and(|t| t.depth == depth && t.row_index == row_idx)
            {
                st.hovered_glossary_target = None;
                st.nested_scan_timer.stop();
            }
        }
        self.schedule_unlocked_nested_dismiss();
    }

    pub(super) fn navigate_nested_query(&self, app: &MementoApp, from_depth: i32, query: &str) {
        let trimmed = query.trim();
        if trimmed.is_empty() {
            return;
        }
        self.ensure_fresh_db_and_config(app, true);
        let (hits, excluded, headword, reading, term_meta_rows) = {
            let mut st = self.state.borrow_mut();
            let mut hits = st.dict_ctrl.search(trimmed).to_vec();
            let excluded = st.settings.excluded_glossaries.clone();
            if hits.is_empty() && trimmed.chars().count() == 1 {
                hits = st.dict_ctrl.search_kanji(trimmed).to_vec();
            }
            let headword = hits
                .first()
                .map(|h| h.expression.clone())
                .unwrap_or_else(|| trimmed.to_owned());
            let reading = hits.first().map(|h| h.reading.clone()).unwrap_or_default();
            let mut term_meta_rows = Vec::new();
            for h in &hits {
                if !h.is_kanji
                    && !term_meta_rows
                        .iter()
                        .any(|(e, r, _, _): &(String, String, _, _)| {
                            e == &h.expression && r == &h.reading
                        })
                {
                    let graphs = st.dict_ctrl.get_pitch_graphs(&h.expression, &h.reading);
                    let freqs = st.dict_ctrl.get_frequencies(&h.expression, &h.reading);
                    term_meta_rows.push((h.expression.clone(), h.reading.clone(), graphs, freqs));
                }
            }
            (hits, excluded, headword, reading, term_meta_rows)
        };
        if hits.is_empty() {
            app.set_status_message(format!("No dictionary entries found for {trimmed}").into());
            return;
        }
        let (mut rows, grouped_hits) = build_dictionary_result_rows_grouped(&hits, &excluded);
        for row in &mut rows {
            if row.term_start && !row.is_kanji {
                if let Some((_, _, graphs, freqs)) = term_meta_rows.iter().find(|(e, r, _, _)| {
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
        let target_depth = {
            let mut st = self.state.borrow_mut();
            let max_depth = st.settings.max_recursion_depth;
            if max_depth == 0 {
                return;
            }
            let (anchor_x, anchor_y) = st
                .nested_level(from_depth)
                .map(|l| (l.anchor_x, l.anchor_y))
                .unwrap_or((80.0, 80.0));
            let state_ref = &mut *st;
            let (row_extras, kanji_freq_updates) = build_row_extras_for_grouped(
                &mut state_ref.db_helper,
                &mut state_ref.dict_ctrl,
                &rows,
                &grouped_hits,
                None,
            );
            apply_kanji_freq_updates_to_rows(&mut rows, kanji_freq_updates);
            let row_count = rows.len();
            let level = NestedPopupLevel {
                label: headword.clone(),
                rows: rows.clone(),
                headword,
                reading,
                anchor_x,
                anchor_y,
                locked: true,
                source_row_index: 0,
                source_start_char: 0,
                rich_epoch: 1,
                row_extras,
                glossary_cache: GlossaryDocLruCache::new_nested(),
                last_rendered_key: HashMap::new(),
                selection: None,
                scan_highlight: None,
                expanded_cards: HashSet::new(),
                row_anki_duplicates: vec![false; row_count],
                row_anki_note_ids_i64: vec![0; row_count],
                row_anki_browse_queries: vec![SharedString::default(); row_count],
                row_anki_tags: vec![SharedString::default(); row_count],
            };
            let base = (from_depth.max(0) as usize).min(st.nested_stack.len());
            st.nested_stack.truncate(base);
            if st.nested_stack.len() >= max_depth {
                st.nested_stack.pop();
            }
            let keep = st.nested_stack.len();
            st.pending_nested_anki_adds.retain(|p| p.depth <= keep);
            st.nested_body_hover_count = 0;
            st.nested_stack.push(level);
            st.nested_stack.len()
        };
        self.sync_nested_popup_to_bridge(app, true);
        self.dispatch_anki_check_for_rows(app, &rows, Some(target_depth));
    }

    pub(super) fn navigate_nested_kanji(
        &self,
        app: &MementoApp,
        from_depth: i32,
        expression: &str,
        clicked_kanji: &str,
    ) {
        let target_char = clicked_kanji
            .chars()
            .find(|&c| is_kanji_char(c))
            .or_else(|| expression.chars().find(|&c| is_kanji_char(c)));
        let Some(ch) = target_char else {
            return;
        };
        let query = ch.to_string();
        self.ensure_fresh_db_and_config(app, true);
        let (hits, excluded) = {
            let mut st = self.state.borrow_mut();
            let hits = st.dict_ctrl.search_kanji(&query).to_vec();
            let excluded = st.settings.excluded_glossaries.clone();
            (hits, excluded)
        };
        if hits.is_empty() {
            app.set_status_message(format!("No kanji entry found for {query}").into());
            return;
        }
        let (mut rows, grouped_hits) = build_dictionary_result_rows_grouped(&hits, &excluded);
        let reading = hits.first().map(|h| h.reading.clone()).unwrap_or_default();
        let target_depth = {
            let mut st = self.state.borrow_mut();
            let max_depth = st.settings.max_recursion_depth;
            if max_depth == 0 {
                return;
            }
            let (anchor_x, anchor_y) = st
                .nested_level(from_depth)
                .map(|l| (l.anchor_x, l.anchor_y))
                .unwrap_or((80.0, 80.0));
            let state_ref = &mut *st;
            let (row_extras, kanji_freq_updates) = build_row_extras_for_grouped(
                &mut state_ref.db_helper,
                &mut state_ref.dict_ctrl,
                &rows,
                &grouped_hits,
                None,
            );
            apply_kanji_freq_updates_to_rows(&mut rows, kanji_freq_updates);
            let row_count = rows.len();
            let level = NestedPopupLevel {
                label: query.clone(),
                rows: rows.clone(),
                headword: query,
                reading,
                anchor_x,
                anchor_y,
                locked: true,
                source_row_index: 0,
                source_start_char: 0,
                rich_epoch: 1,
                row_extras,
                glossary_cache: GlossaryDocLruCache::new_nested(),
                last_rendered_key: HashMap::new(),
                selection: None,
                scan_highlight: None,
                expanded_cards: HashSet::new(),
                row_anki_duplicates: vec![false; row_count],
                row_anki_note_ids_i64: vec![0; row_count],
                row_anki_browse_queries: vec![SharedString::default(); row_count],
                row_anki_tags: vec![SharedString::default(); row_count],
            };
            let base = (from_depth.max(0) as usize).min(st.nested_stack.len());
            st.nested_stack.truncate(base);
            if st.nested_stack.len() >= max_depth {
                st.nested_stack.pop();
            }
            let keep = st.nested_stack.len();
            st.pending_nested_anki_adds.retain(|p| p.depth <= keep);
            st.nested_body_hover_count = 0;
            st.nested_stack.push(level);
            st.nested_stack.len()
        };
        self.sync_nested_popup_to_bridge(app, true);
        self.dispatch_anki_check_for_rows(app, &rows, Some(target_depth));
    }

    pub(super) fn is_nested_target_already_shown(&self, target: &HoveredGlossaryTarget) -> bool {
        if target.depth < 0 {
            return false;
        }
        let st = self.state.borrow();
        st.nested_stack
            .get(target.depth as usize)
            .is_some_and(|child| {
                child.source_row_index == target.row_index
                    && child.source_start_char == target.start_char
            })
    }

    pub(super) fn toggle_nested_select_definition(
        &self,
        app: &MementoApp,
        depth: i32,
        zero_based_idx: i32,
    ) {
        if zero_based_idx < 0 {
            return;
        }
        let mut changed = false;
        {
            let mut st = self.state.borrow_mut();
            if let Some(lvl) = st.nested_level_mut(depth) {
                lvl.locked = true;
                if let Some(row) = lvl.rows.get_mut(zero_based_idx as usize) {
                    row.selected = !row.selected;
                    changed = true;
                }
            }
        }
        if changed {
            self.sync_nested_popup_to_bridge(app, false);
        }
    }

    pub(super) fn toggle_nested_term_collapse(
        &self,
        app: &MementoApp,
        depth: i32,
        term_start_zero_idx: i32,
    ) {
        if term_start_zero_idx < 0 {
            return;
        }
        let mut changed = false;
        {
            let mut st = self.state.borrow_mut();
            if let Some(lvl) = st.nested_level_mut(depth) {
                lvl.locked = true;
                let start = term_start_zero_idx as usize;
                if start < lvl.rows.len() {
                    let target_collapsed = !lvl.rows[start].collapsed;
                    lvl.rows[start].collapsed = target_collapsed;
                    changed = true;
                    for row in lvl.rows.iter_mut().skip(start + 1) {
                        if row.term_start {
                            break;
                        }
                        row.collapsed = target_collapsed;
                    }
                }
            }
        }
        if changed {
            self.sync_nested_popup_to_bridge(app, false);
        }
    }

    pub(super) fn add_nested_anki_note(&self, app: &MementoApp, row_zero_idx: i32) {
        let (depth, nested_rows) = {
            let mut st = self.state.borrow_mut();
            let depth = st.nested_stack.len();
            let Some(top) = st.nested_stack.last_mut() else {
                return;
            };
            top.locked = true;
            let idx = row_zero_idx.max(0) as usize;
            let Some(target) = top.rows.get(idx) else {
                return;
            };
            let pending = PendingNestedAnkiAdd {
                depth,
                row_index: idx,
                expression: target.expression.to_string(),
                reading: target.reading.to_string(),
                is_kanji: target.is_kanji,
            };
            let rows = top.rows.clone();
            while st.pending_nested_anki_adds.len() >= MAX_PENDING_NESTED_ANKI_ADDS {
                st.pending_nested_anki_adds.pop_front();
            }
            st.pending_nested_anki_adds.push_back(pending);
            (depth, rows)
        };

        // `main.rs`'s `on_add_anki_note` reads `app.get_dict_results()` synchronously before
        // spawning its background Anki thread. Guard the temporary swap with an RAII Drop guard
        // so root `dict_results` is always restored immediately and never left clobbered.
        let parent_rows = app.get_dict_results();
        let swapped_rows: slint::ModelRc<DictResult> = Rc::new(VecModel::from(nested_rows)).into();
        {
            app.set_dict_results(swapped_rows.clone());
            let _restore = RestoreDictResultsGuard {
                app,
                swapped: swapped_rows,
                parent: parent_rows,
            };
            app.invoke_add_anki_note(row_zero_idx);
        }
        if depth > 0 {
            app.global::<PopupBridge>().set_nested_locked(true);
        }
    }
}

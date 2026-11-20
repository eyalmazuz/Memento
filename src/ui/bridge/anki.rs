use std::collections::HashMap;
use std::rc::Rc;
use std::sync::atomic::Ordering;

use slint::{ComponentHandle, Model as _, SharedString, VecModel};

use crate::controller::anki::EntryAnkiStatus;
use crate::ui::popup_model::{active_rich_bridge_for_app, split_anki_tags_str};
use crate::ui::{DictResult, MementoApp, PopupBridge};

use super::state::{
    current_external_lookup_epoch, encode_note_id_i32, AnkiCheckTarget, AnkiPopupCheckRequest,
    AnkiPopupCheckResponse, NestedPopupLevel, PanelPopupState,
};
use super::RichUiBridge;

pub(super) fn drain_anki_results_for_app(app: &MementoApp) -> bool {
    if let Some(br) = active_rich_bridge_for_app(app) {
        br.drain_anki_results(app)
    } else {
        false
    }
}

pub(super) fn sync_anki_checking_for_app(app: &MementoApp) {
    if let Some(br) = active_rich_bridge_for_app(app) {
        br.sync_anki_checking_flag(app);
    }
}

struct ResolvedRowAnkiVectors {
    dups: Vec<bool>,
    note_ids_i32: Vec<i32>,
    note_ids_i64: Vec<i64>,
    note_id_strings: Vec<SharedString>,
    queries: Vec<SharedString>,
    tags: Vec<SharedString>,
    addable: Vec<bool>,
    reading_as_expr: Vec<bool>,
}

fn resolve_row_anki_vectors(
    rows: &[DictResult],
    term_kanji: &HashMap<(String, String), EntryAnkiStatus>,
    term_kana: &HashMap<(String, String), EntryAnkiStatus>,
    kanji: &HashMap<String, EntryAnkiStatus>,
    manual_overrides: &HashMap<(String, String), bool>,
) -> ResolvedRowAnkiVectors {
    let count = rows.len();
    let mut out = ResolvedRowAnkiVectors {
        dups: Vec::with_capacity(count),
        note_ids_i32: Vec::with_capacity(count),
        note_ids_i64: Vec::with_capacity(count),
        note_id_strings: Vec::with_capacity(count),
        queries: Vec::with_capacity(count),
        tags: Vec::with_capacity(count),
        addable: Vec::with_capacity(count),
        reading_as_expr: Vec::with_capacity(count),
    };

    for row in rows {
        if row.is_kanji {
            if let Some(st) = kanji.get(row.expression.as_str()) {
                let nid = st.note_ids.first().copied().unwrap_or(0);
                out.dups.push(st.is_duplicate);
                out.note_ids_i32.push(encode_note_id_i32(nid));
                out.note_ids_i64.push(nid);
                out.note_id_strings.push(if nid > 0 {
                    SharedString::from(nid.to_string())
                } else {
                    SharedString::default()
                });
                out.queries.push(SharedString::from(st.browse_query.as_str()));
                out.tags.push(SharedString::from(st.tags.join(" ")));
                out.addable.push(st.can_add);
            } else {
                out.dups.push(false);
                out.note_ids_i32.push(0);
                out.note_ids_i64.push(0);
                out.note_id_strings.push(SharedString::default());
                out.queries.push(SharedString::default());
                out.tags.push(SharedString::default());
                out.addable.push(true);
            }
            out.reading_as_expr.push(false);
            continue;
        }

        let key = (row.expression.to_string(), row.reading.to_string());
        let can_use_kana = !row.reading.is_empty() && row.reading != row.expression;
        let kanji_st = term_kanji.get(&key);
        let kana_st = can_use_kana.then(|| term_kana.get(&key)).flatten();

        let (rae, addable, display_st) = match manual_overrides.get(&key).copied() {
            Some(manual_rae) => {
                let effective_rae = manual_rae && can_use_kana;
                let active_st = if effective_rae {
                    kana_st.or(kanji_st)
                } else {
                    kanji_st
                };
                let addable = active_st.map_or(true, |st| st.can_add);
                (effective_rae, addable, active_st)
            }
            None => {
                let auto_rae = can_use_kana
                    && kanji_st.is_some_and(|st| !st.can_add)
                    && kana_st.is_some_and(|st| st.can_add);
                let addable = if auto_rae {
                    true
                } else {
                    kanji_st.map_or(true, |st| st.can_add)
                };
                (auto_rae, addable, kanji_st)
            }
        };

        if let Some(st) = display_st {
            let nid = st.note_ids.first().copied().unwrap_or(0);
            out.dups.push(st.is_duplicate);
            out.note_ids_i32.push(encode_note_id_i32(nid));
            out.note_ids_i64.push(nid);
            out.note_id_strings.push(if nid > 0 {
                SharedString::from(nid.to_string())
            } else {
                SharedString::default()
            });
            out.queries.push(SharedString::from(st.browse_query.as_str()));
            out.tags.push(SharedString::from(st.tags.join(" ")));
        } else {
            out.dups.push(false);
            out.note_ids_i32.push(0);
            out.note_ids_i64.push(0);
            out.note_id_strings.push(SharedString::default());
            out.queries.push(SharedString::default());
            out.tags.push(SharedString::default());
        }
        out.addable.push(addable);
        out.reading_as_expr.push(rae);
    }

    out
}

impl RichUiBridge {
    pub(super) fn sync_anki_checking_flag(&self, app: &MementoApp) {
        let checking = self.root_anki_in_flight.load(Ordering::Acquire) != 0
            || self.nested_anki_in_flight.load(Ordering::Acquire) != 0
            || self.panel_anki_in_flight.load(Ordering::Acquire) != 0;
        app.global::<PopupBridge>().set_anki_checking(checking);
    }

    pub(super) fn cancel_nested_anki_check(&self, app: &MementoApp) {
        self.nested_anki_nav_epoch.fetch_add(1, Ordering::AcqRel);
        self.nested_anki_in_flight.store(0, Ordering::Release);
        self.sync_anki_checking_flag(app);
    }

    pub(super) fn cancel_panel_anki_check(&self, app: &MementoApp) {
        self.panel_anki_nav_epoch.fetch_add(1, Ordering::AcqRel);
        self.panel_anki_in_flight.store(0, Ordering::Release);
        self.sync_anki_checking_flag(app);
    }

    pub(super) fn dispatch_anki_check_for_rows(
        &self,
        app: &MementoApp,
        rows: &[DictResult],
        target: AnkiCheckTarget,
    ) {
        let next_nav = match target {
            AnkiCheckTarget::Root => self.anki_nav_epoch.fetch_add(1, Ordering::AcqRel) + 1,
            AnkiCheckTarget::Nested(_) => {
                self.nested_anki_nav_epoch.fetch_add(1, Ordering::AcqRel) + 1
            }
            AnkiCheckTarget::Panel => self.panel_anki_nav_epoch.fetch_add(1, Ordering::AcqRel) + 1,
        };
        let (anki_enabled, show_tags, term_profile, kanji_profile) = {
            let st = self.state.borrow();
            (
                st.settings.anki_enabled,
                st.settings.anki_show_tag,
                st.settings.anki_term_profile.clone(),
                st.settings.anki_kanji_profile.clone(),
            )
        };
        let default_addable = vec![true; rows.len()];
        let default_rae = vec![false; rows.len()];
        match target {
            AnkiCheckTarget::Root => {
                let mut st = self.state.borrow_mut();
                st.root_row_anki_note_ids_i64.clear();
                st.root_row_anki_addable = default_addable.clone();
                st.root_row_reading_as_expression = default_rae.clone();
                st.root_term_kanji_statuses.clear();
                st.root_term_kana_statuses.clear();
            }
            AnkiCheckTarget::Panel => {
                let mut st = self.state.borrow_mut();
                st.panel.row_anki_duplicates.clear();
                st.panel.row_anki_note_ids_i64.clear();
                st.panel.row_anki_browse_queries.clear();
                st.panel.row_anki_tags.clear();
                st.panel.row_anki_addable = default_addable.clone();
                st.panel.row_reading_as_expression = default_rae.clone();
                st.panel.term_kanji_statuses.clear();
                st.panel.term_kana_statuses.clear();
            }
            AnkiCheckTarget::Nested(_) => {}
        }

        let pb = app.global::<PopupBridge>();
        pb.set_anki_show_tags(show_tags);
        match target {
            AnkiCheckTarget::Root => {
                pb.set_row_anki_duplicates(Rc::new(VecModel::from(Vec::<bool>::new())).into());
                pb.set_row_anki_note_ids(Rc::new(VecModel::from(Vec::<i32>::new())).into());
                pb.set_row_anki_note_id_strings(
                    Rc::new(VecModel::from(Vec::<SharedString>::new())).into(),
                );
                pb.set_row_anki_browse_queries(
                    Rc::new(VecModel::from(Vec::<SharedString>::new())).into(),
                );
                pb.set_row_anki_tags(Rc::new(VecModel::from(Vec::<SharedString>::new())).into());
                pb.set_row_anki_addable(Rc::new(VecModel::from(default_addable)).into());
                pb.set_row_reading_as_expression(Rc::new(VecModel::from(default_rae)).into());
                pb.set_popup_revision(pb.get_popup_revision().wrapping_add(1));
            }
            AnkiCheckTarget::Panel => {
                pb.set_panel_row_anki_duplicates(Rc::new(VecModel::from(Vec::<bool>::new())).into());
                pb.set_panel_row_anki_note_ids(Rc::new(VecModel::from(Vec::<i32>::new())).into());
                pb.set_panel_row_anki_browse_queries(
                    Rc::new(VecModel::from(Vec::<SharedString>::new())).into(),
                );
                pb.set_panel_row_anki_tags(
                    Rc::new(VecModel::from(Vec::<SharedString>::new())).into(),
                );
                pb.set_panel_row_anki_addable(Rc::new(VecModel::from(default_addable)).into());
                pb.set_panel_row_reading_as_expression(Rc::new(VecModel::from(default_rae)).into());
                pb.set_panel_revision(pb.get_panel_revision().wrapping_add(1));
            }
            AnkiCheckTarget::Nested(_) => {}
        }

        let clear_in_flight = || {
            match target {
                AnkiCheckTarget::Root => self.root_anki_in_flight.store(0, Ordering::Release),
                AnkiCheckTarget::Nested(_) => {
                    self.nested_anki_in_flight.store(0, Ordering::Release)
                }
                AnkiCheckTarget::Panel => self.panel_anki_in_flight.store(0, Ordering::Release),
            }
            self.sync_anki_checking_flag(app);
        };

        if rows.is_empty() || !anki_enabled {
            clear_in_flight();
            return;
        }

        let mut term_entries: Vec<(String, String)> = Vec::new();
        let mut kanji_entries: Vec<(String, String)> = Vec::new();
        for row in rows {
            if !row.term_start {
                continue;
            }
            if row.is_kanji {
                let expr = row.expression.to_string();
                if !expr.is_empty() && !kanji_entries.iter().any(|(e, _)| e == &expr) {
                    kanji_entries.push((expr, String::new()));
                }
            } else {
                let pair = (row.expression.to_string(), row.reading.to_string());
                if !pair.0.is_empty() && !term_entries.contains(&pair) {
                    term_entries.push(pair);
                }
            }
        }

        let has_valid_term = !term_entries.is_empty()
            && term_profile.as_ref().is_some_and(|p| {
                p.enabled
                    && !p.deck.trim().is_empty()
                    && !p.model.trim().is_empty()
                    && !p.field_templates.is_empty()
            });
        let has_valid_kanji = !kanji_entries.is_empty()
            && kanji_profile.as_ref().is_some_and(|p| {
                p.enabled
                    && !p.deck.trim().is_empty()
                    && !p.model.trim().is_empty()
                    && !p.field_templates.is_empty()
            });
        if !has_valid_term && !has_valid_kanji {
            clear_in_flight();
            return;
        }

        let lookup_epoch = if target == AnkiCheckTarget::Panel {
            None
        } else {
            current_external_lookup_epoch(&self.external_lookup_epoch)
        };
        match target {
            AnkiCheckTarget::Root => self.root_anki_in_flight.store(next_nav, Ordering::Release),
            AnkiCheckTarget::Nested(_) => {
                self.nested_anki_in_flight.store(next_nav, Ordering::Release)
            }
            AnkiCheckTarget::Panel => self.panel_anki_in_flight.store(next_nav, Ordering::Release),
        }
        self.sync_anki_checking_flag(app);

        let sent = self
            .anki_tx
            .send(AnkiPopupCheckRequest {
                nav_epoch: next_nav,
                lookup_epoch,
                target,
                show_tags,
                term_profile: has_valid_term.then_some(term_profile).flatten(),
                kanji_profile: has_valid_kanji.then_some(kanji_profile).flatten(),
                term_entries,
                kanji_entries,
            })
            .is_ok();
        if !sent {
            clear_in_flight();
            return;
        }
        self.kick_no_event_loop_pump();
    }

    pub(super) fn toggle_reading_as_expression(
        &self,
        app: &MementoApp,
        target: AnkiCheckTarget,
        row_zero_idx: i32,
    ) {
        if row_zero_idx < 0 {
            return;
        }
        let row_usize = row_zero_idx as usize;
        let pb = app.global::<PopupBridge>();
        match target {
            AnkiCheckTarget::Root => {
                let full = app.get_dict_results();
                let count = full.row_count();
                let rows: Vec<DictResult> = (0..count).filter_map(|i| full.row_data(i)).collect();
                let Some(row) = rows.get(row_usize) else {
                    return;
                };
                if row.is_kanji || row.reading.is_empty() || row.reading == row.expression {
                    return;
                }
                let key = (row.expression.to_string(), row.reading.to_string());
                let resolved = {
                    let mut st = self.state.borrow_mut();
                    let cur = st
                        .root_row_reading_as_expression
                        .get(row_usize)
                        .copied()
                        .unwrap_or_else(|| {
                            pb.get_row_reading_as_expression()
                                .row_data(row_usize)
                                .unwrap_or(false)
                        });
                    st.root_manual_reading_as_expr.insert(key, !cur);
                    let res = resolve_row_anki_vectors(
                        &rows,
                        &st.root_term_kanji_statuses,
                        &st.root_term_kana_statuses,
                        &HashMap::new(),
                        &st.root_manual_reading_as_expr,
                    );
                    st.root_row_anki_note_ids_i64 = res.note_ids_i64.clone();
                    st.root_row_anki_addable = res.addable.clone();
                    st.root_row_reading_as_expression = res.reading_as_expr.clone();
                    res
                };
                // Preserve kanji rows' existing duplicate/tag state while updating toggled term rows.
                let existing_dups = pb.get_row_anki_duplicates();
                let existing_note_ids = pb.get_row_anki_note_ids();
                let existing_note_id_strings = pb.get_row_anki_note_id_strings();
                let existing_queries = pb.get_row_anki_browse_queries();
                let existing_tags = pb.get_row_anki_tags();
                let existing_addable = pb.get_row_anki_addable();
                let mut dups = resolved.dups;
                let mut note_ids_i32 = resolved.note_ids_i32;
                let mut note_id_strings = resolved.note_id_strings;
                let mut queries = resolved.queries;
                let mut tags = resolved.tags;
                let mut addable = resolved.addable;
                for (i, r) in rows.iter().enumerate() {
                    if r.is_kanji {
                        dups[i] = existing_dups.row_data(i).unwrap_or(false);
                        note_ids_i32[i] = existing_note_ids.row_data(i).unwrap_or(0);
                        note_id_strings[i] = existing_note_id_strings.row_data(i).unwrap_or_default();
                        queries[i] = existing_queries.row_data(i).unwrap_or_default();
                        tags[i] = existing_tags.row_data(i).unwrap_or_default();
                        addable[i] = existing_addable.row_data(i).unwrap_or(true);
                    }
                }
                pb.set_row_anki_duplicates(Rc::new(VecModel::from(dups)).into());
                pb.set_row_anki_note_ids(Rc::new(VecModel::from(note_ids_i32)).into());
                pb.set_row_anki_note_id_strings(Rc::new(VecModel::from(note_id_strings)).into());
                pb.set_row_anki_browse_queries(Rc::new(VecModel::from(queries)).into());
                pb.set_row_anki_tags(Rc::new(VecModel::from(tags)).into());
                pb.set_row_anki_addable(Rc::new(VecModel::from(addable)).into());
                pb.set_row_reading_as_expression(
                    Rc::new(VecModel::from(resolved.reading_as_expr)).into(),
                );
                pb.set_popup_revision(pb.get_popup_revision().wrapping_add(1));
                pb.set_rich_revision(pb.get_rich_revision().wrapping_add(1));
            }
            AnkiCheckTarget::Nested(depth) => {
                let updated = {
                    let mut st = self.state.borrow_mut();
                    let Some(lvl) = st.nested_level_mut(depth as i32) else {
                        return;
                    };
                    let Some(row) = lvl.rows.get(row_usize).cloned() else {
                        return;
                    };
                    if row.is_kanji || row.reading.is_empty() || row.reading == row.expression {
                        return;
                    }
                    let key = (row.expression.to_string(), row.reading.to_string());
                    let cur = lvl
                        .row_reading_as_expression
                        .get(row_usize)
                        .copied()
                        .unwrap_or(false);
                    lvl.manual_reading_as_expr.insert(key, !cur);
                    let res = resolve_row_anki_vectors(
                        &lvl.rows,
                        &lvl.term_kanji_statuses,
                        &lvl.term_kana_statuses,
                        &HashMap::new(),
                        &lvl.manual_reading_as_expr,
                    );
                    for (i, r) in lvl.rows.iter().enumerate() {
                        if !r.is_kanji {
                            if lvl.row_anki_duplicates.len() <= i {
                                lvl.row_anki_duplicates.resize(lvl.rows.len(), false);
                            }
                            if lvl.row_anki_note_ids_i64.len() <= i {
                                lvl.row_anki_note_ids_i64.resize(lvl.rows.len(), 0);
                            }
                            if lvl.row_anki_browse_queries.len() <= i {
                                lvl.row_anki_browse_queries
                                    .resize(lvl.rows.len(), SharedString::default());
                            }
                            if lvl.row_anki_tags.len() <= i {
                                lvl.row_anki_tags
                                    .resize(lvl.rows.len(), SharedString::default());
                            }
                            if lvl.row_anki_addable.len() <= i {
                                lvl.row_anki_addable.resize(lvl.rows.len(), true);
                            }
                            if lvl.row_reading_as_expression.len() <= i {
                                lvl.row_reading_as_expression.resize(lvl.rows.len(), false);
                            }
                            lvl.row_anki_duplicates[i] = res.dups[i];
                            lvl.row_anki_note_ids_i64[i] = res.note_ids_i64[i];
                            lvl.row_anki_browse_queries[i] = res.queries[i].clone();
                            lvl.row_anki_tags[i] = res.tags[i].clone();
                            lvl.row_anki_addable[i] = res.addable[i];
                            lvl.row_reading_as_expression[i] = res.reading_as_expr[i];
                        }
                    }
                    true
                };
                if updated {
                    self.sync_nested_popup_to_bridge(app, false);
                }
            }
            AnkiCheckTarget::Panel => {
                let updated = {
                    let mut st = self.state.borrow_mut();
                    let panel = &mut st.panel;
                    let Some(row) = panel.rows.get(row_usize).cloned() else {
                        return;
                    };
                    if row.is_kanji || row.reading.is_empty() || row.reading == row.expression {
                        return;
                    }
                    let key = (row.expression.to_string(), row.reading.to_string());
                    let cur = panel
                        .row_reading_as_expression
                        .get(row_usize)
                        .copied()
                        .unwrap_or(false);
                    panel.manual_reading_as_expr.insert(key, !cur);
                    let res = resolve_row_anki_vectors(
                        &panel.rows,
                        &panel.term_kanji_statuses,
                        &panel.term_kana_statuses,
                        &HashMap::new(),
                        &panel.manual_reading_as_expr,
                    );
                    let len = panel.rows.len();
                    panel.row_anki_duplicates.resize(len, false);
                    panel.row_anki_note_ids_i64.resize(len, 0);
                    panel
                        .row_anki_browse_queries
                        .resize(len, SharedString::default());
                    panel.row_anki_tags.resize(len, SharedString::default());
                    panel.row_anki_addable.resize(len, true);
                    panel.row_reading_as_expression.resize(len, false);
                    for (i, r) in panel.rows.iter().enumerate() {
                        if !r.is_kanji {
                            panel.row_anki_duplicates[i] = res.dups[i];
                            panel.row_anki_note_ids_i64[i] = res.note_ids_i64[i];
                            panel.row_anki_browse_queries[i] = res.queries[i].clone();
                            panel.row_anki_tags[i] = res.tags[i].clone();
                            panel.row_anki_addable[i] = res.addable[i];
                            panel.row_reading_as_expression[i] = res.reading_as_expr[i];
                        }
                    }
                    true
                };
                if updated {
                    self.sync_panel_popup_to_bridge(app, false);
                }
            }
        }
    }

    pub(super) fn drain_anki_results(&self, app: &MementoApp) -> bool {
        let mut latest_root: Option<AnkiPopupCheckResponse> = None;
        let mut latest_nested: Option<AnkiPopupCheckResponse> = None;
        let mut latest_panel: Option<AnkiPopupCheckResponse> = None;
        if let Ok(guard) = self.anki_res_rx.lock() {
            while let Ok(item) = guard.try_recv() {
                match item.target {
                    AnkiCheckTarget::Root => latest_root = Some(item),
                    AnkiCheckTarget::Nested(_) => latest_nested = Some(item),
                    AnkiCheckTarget::Panel => latest_panel = Some(item),
                }
            }
        }
        if latest_root.is_none() && latest_nested.is_none() && latest_panel.is_none() {
            self.sync_anki_checking_flag(app);
            return false;
        }

        let current_ext = current_external_lookup_epoch(&self.external_lookup_epoch);
        let pb = app.global::<PopupBridge>();
        let mut applied_any = false;

        if let Some(resp) = latest_root {
            let _ = self.root_anki_in_flight.compare_exchange(
                resp.nav_epoch,
                0,
                Ordering::AcqRel,
                Ordering::Acquire,
            );
            if resp.nav_epoch == self.anki_nav_epoch.load(Ordering::Acquire)
                && resp.lookup_epoch == current_ext
                && app.get_dict_open()
            {
                let full = app.get_dict_results();
                let count = full.row_count();
                let rows: Vec<DictResult> = (0..count).filter_map(|i| full.row_data(i)).collect();
                let resolved = {
                    let mut st = self.state.borrow_mut();
                    st.root_term_kanji_statuses = resp.term_statuses;
                    st.root_term_kana_statuses = resp.term_kana_statuses;
                    let res = resolve_row_anki_vectors(
                        &rows,
                        &st.root_term_kanji_statuses,
                        &st.root_term_kana_statuses,
                        &resp.kanji_statuses,
                        &st.root_manual_reading_as_expr,
                    );
                    st.root_row_anki_note_ids_i64 = res.note_ids_i64.clone();
                    st.root_row_anki_addable = res.addable.clone();
                    st.root_row_reading_as_expression = res.reading_as_expr.clone();
                    res
                };
                pb.set_anki_show_tags(resp.show_tags);
                pb.set_row_anki_duplicates(Rc::new(VecModel::from(resolved.dups)).into());
                pb.set_row_anki_note_ids(Rc::new(VecModel::from(resolved.note_ids_i32)).into());
                pb.set_row_anki_note_id_strings(
                    Rc::new(VecModel::from(resolved.note_id_strings)).into(),
                );
                pb.set_row_anki_browse_queries(Rc::new(VecModel::from(resolved.queries)).into());
                pb.set_row_anki_tags(Rc::new(VecModel::from(resolved.tags)).into());
                pb.set_row_anki_addable(Rc::new(VecModel::from(resolved.addable)).into());
                pb.set_row_reading_as_expression(
                    Rc::new(VecModel::from(resolved.reading_as_expr)).into(),
                );
                pb.set_popup_revision(pb.get_popup_revision().wrapping_add(1));
                pb.set_rich_revision(pb.get_rich_revision().wrapping_add(1));
                applied_any = true;
            }
        }

        if let Some(resp) = latest_nested {
            let _ = self.nested_anki_in_flight.compare_exchange(
                resp.nav_epoch,
                0,
                Ordering::AcqRel,
                Ordering::Acquire,
            );
            if resp.nav_epoch == self.nested_anki_nav_epoch.load(Ordering::Acquire)
                && resp.lookup_epoch == current_ext
                && app.get_dict_open()
            {
                if let AnkiCheckTarget::Nested(depth) = resp.target {
                    let sync_needed = {
                        let mut st = self.state.borrow_mut();
                        let stack_len = st.nested_stack.len();
                        if let Some(lvl) = st.nested_level_mut(depth as i32) {
                            lvl.term_kanji_statuses = resp.term_statuses;
                            lvl.term_kana_statuses = resp.term_kana_statuses;
                            let res = resolve_row_anki_vectors(
                                &lvl.rows,
                                &lvl.term_kanji_statuses,
                                &lvl.term_kana_statuses,
                                &resp.kanji_statuses,
                                &lvl.manual_reading_as_expr,
                            );
                            lvl.row_anki_duplicates = res.dups;
                            lvl.row_anki_note_ids_i64 = res.note_ids_i64;
                            lvl.row_anki_browse_queries = res.queries;
                            lvl.row_anki_tags = res.tags;
                            lvl.row_anki_addable = res.addable;
                            lvl.row_reading_as_expression = res.reading_as_expr;
                            depth == stack_len
                        } else {
                            false
                        }
                    };
                    if sync_needed {
                        pb.set_anki_show_tags(resp.show_tags);
                        self.sync_nested_popup_to_bridge(app, false);
                        applied_any = true;
                    }
                }
            }
        }

        if let Some(resp) = latest_panel {
            let _ = self.panel_anki_in_flight.compare_exchange(
                resp.nav_epoch,
                0,
                Ordering::AcqRel,
                Ordering::Acquire,
            );
            if resp.nav_epoch == self.panel_anki_nav_epoch.load(Ordering::Acquire) {
                {
                    let mut st = self.state.borrow_mut();
                    let panel = &mut st.panel;
                    panel.term_kanji_statuses = resp.term_statuses;
                    panel.term_kana_statuses = resp.term_kana_statuses;
                    let res = resolve_row_anki_vectors(
                        &panel.rows,
                        &panel.term_kanji_statuses,
                        &panel.term_kana_statuses,
                        &resp.kanji_statuses,
                        &panel.manual_reading_as_expr,
                    );
                    panel.row_anki_duplicates = res.dups;
                    panel.row_anki_note_ids_i64 = res.note_ids_i64;
                    panel.row_anki_browse_queries = res.queries;
                    panel.row_anki_tags = res.tags;
                    panel.row_anki_addable = res.addable;
                    panel.row_reading_as_expression = res.reading_as_expr;
                }
                pb.set_anki_show_tags(resp.show_tags);
                self.sync_panel_popup_to_bridge(app, false);
                applied_any = true;
            }
        }

        self.sync_anki_checking_flag(app);
        applied_any
    }
}

/// Returns the lossless 64-bit Anki note ID for a popup row (or active nested row if
/// `nested_depth > 0`), avoiding `i32` truncation for 13-digit AnkiConnect epoch-ms note IDs.
pub fn popup_row_anki_note_id_i64(app: &MementoApp, row_idx: usize) -> Option<i64> {
    let pb = app.global::<PopupBridge>();
    if pb.get_nested_depth() > 0 {
        return nested_popup_row_anki_note_id_i64(app, row_idx);
    }
    if let Some(s) = pb.get_row_anki_note_id_strings().row_data(row_idx) {
        if let Ok(parsed) = s.as_str().trim().parse::<i64>() {
            if parsed > 0 {
                return Some(parsed);
            }
        }
    }
    let from_bridge = active_rich_bridge_for_app(app).and_then(|b| {
        b.state
            .borrow()
            .root_row_anki_note_ids_i64
            .get(row_idx)
            .copied()
            .filter(|&id| id > 0)
    });
    if from_bridge.is_some() {
        return from_bridge;
    }
    pb.get_row_anki_note_ids()
        .row_data(row_idx)
        .filter(|&id| id > 0)
        .map(i64::from)
}

/// Returns the lossless 64-bit Anki note ID for a row in the active nested popup level.
pub fn nested_popup_row_anki_note_id_i64(app: &MementoApp, row_idx: usize) -> Option<i64> {
    let from_bridge = active_rich_bridge_for_app(app).and_then(|b| {
        b.state
            .borrow()
            .nested_stack
            .last()
            .and_then(|lvl| lvl.row_anki_note_ids_i64.get(row_idx).copied())
            .filter(|&id| id > 0)
    });
    if from_bridge.is_some() {
        return from_bridge;
    }
    app.global::<PopupBridge>()
        .get_nested_row_anki_note_ids()
        .row_data(row_idx)
        .filter(|&id| id > 0)
        .map(i64::from)
}

fn merge_anki_tags(existing: &SharedString, joined_profile_tags: &str) -> SharedString {
    let combined = if existing.is_empty() {
        joined_profile_tags.to_owned()
    } else {
        format!("{existing} {joined_profile_tags}")
    };
    let dedup: Vec<String> = split_anki_tags_str(&combined)
        .into_iter()
        .map(|s| s.to_string())
        .collect();
    SharedString::from(dedup.join(" "))
}

#[allow(clippy::too_many_arguments)]
fn apply_anki_state_to_nested_level(
    lvl: &mut NestedPopupLevel,
    expression: &str,
    reading: &str,
    is_kanji: bool,
    reading_as_expression: bool,
    note_id: i64,
    nid_query: &str,
    joined_profile_tags: &str,
    show_tags: bool,
    mark_duplicate: bool,
    overwrite_browse_query: bool,
) {
    let count = lvl.rows.len();
    lvl.row_anki_duplicates.resize(count, false);
    lvl.row_anki_note_ids_i64.resize(count, 0);
    lvl.row_anki_browse_queries
        .resize(count, SharedString::default());
    lvl.row_anki_tags.resize(count, SharedString::default());
    lvl.row_anki_addable.resize(count, true);
    lvl.row_reading_as_expression.resize(count, false);
    if !is_kanji {
        let key = (expression.to_owned(), reading.to_owned());
        let map = if reading_as_expression {
            &mut lvl.term_kana_statuses
        } else {
            &mut lvl.term_kanji_statuses
        };
        let entry = map.entry(key).or_insert_with(|| EntryAnkiStatus {
            can_add: !mark_duplicate,
            is_duplicate: mark_duplicate,
            note_ids: Vec::new(),
            tags: Vec::new(),
            browse_query: String::new(),
        });
        if mark_duplicate {
            entry.can_add = false;
            entry.is_duplicate = true;
            if note_id > 0 && !entry.note_ids.contains(&note_id) {
                entry.note_ids.insert(0, note_id);
            }
            if overwrite_browse_query || entry.browse_query.is_empty() {
                entry.browse_query = nid_query.to_owned();
            }
        }
    }
    for idx in 0..count {
        let row = &lvl.rows[idx];
        if row.expression.as_str() == expression
            && row.reading.as_str() == reading
            && row.is_kanji == is_kanji
        {
            if mark_duplicate {
                lvl.row_anki_duplicates[idx] = true;
                lvl.row_anki_addable[idx] = false;
                lvl.row_anki_note_ids_i64[idx] = note_id;
                if overwrite_browse_query || lvl.row_anki_browse_queries[idx].is_empty() {
                    lvl.row_anki_browse_queries[idx] = SharedString::from(nid_query);
                }
            }
            if show_tags && !joined_profile_tags.is_empty() {
                lvl.row_anki_tags[idx] =
                    merge_anki_tags(&lvl.row_anki_tags[idx], joined_profile_tags);
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn apply_anki_state_to_panel(
    panel: &mut PanelPopupState,
    expression: &str,
    reading: &str,
    is_kanji: bool,
    reading_as_expression: bool,
    note_id: i64,
    nid_query: &str,
    joined_profile_tags: &str,
    show_tags: bool,
    mark_duplicate: bool,
    overwrite_browse_query: bool,
) {
    let count = panel.rows.len();
    panel.row_anki_duplicates.resize(count, false);
    panel.row_anki_note_ids_i64.resize(count, 0);
    panel
        .row_anki_browse_queries
        .resize(count, SharedString::default());
    panel.row_anki_tags.resize(count, SharedString::default());
    panel.row_anki_addable.resize(count, true);
    panel.row_reading_as_expression.resize(count, false);
    if !is_kanji {
        let key = (expression.to_owned(), reading.to_owned());
        let map = if reading_as_expression {
            &mut panel.term_kana_statuses
        } else {
            &mut panel.term_kanji_statuses
        };
        let entry = map.entry(key).or_insert_with(|| EntryAnkiStatus {
            can_add: !mark_duplicate,
            is_duplicate: mark_duplicate,
            note_ids: Vec::new(),
            tags: Vec::new(),
            browse_query: String::new(),
        });
        if mark_duplicate {
            entry.can_add = false;
            entry.is_duplicate = true;
            if note_id > 0 && !entry.note_ids.contains(&note_id) {
                entry.note_ids.insert(0, note_id);
            }
            if overwrite_browse_query || entry.browse_query.is_empty() {
                entry.browse_query = nid_query.to_owned();
            }
        }
    }
    for idx in 0..count {
        let row = &panel.rows[idx];
        if row.expression.as_str() == expression
            && row.reading.as_str() == reading
            && row.is_kanji == is_kanji
        {
            if mark_duplicate {
                panel.row_anki_duplicates[idx] = true;
                panel.row_anki_addable[idx] = false;
                panel.row_anki_note_ids_i64[idx] = note_id;
                if overwrite_browse_query || panel.row_anki_browse_queries[idx].is_empty() {
                    panel.row_anki_browse_queries[idx] = SharedString::from(nid_query);
                }
            }
            if show_tags && !joined_profile_tags.is_empty() {
                panel.row_anki_tags[idx] =
                    merge_anki_tags(&panel.row_anki_tags[idx], joined_profile_tags);
            }
        }
    }
}

enum AnkiStateTarget {
    Added,
    Updated { nested_depth: i32 },
}

fn mark_popup_term_anki_state(
    app: &MementoApp,
    row_index: usize,
    note_id: i64,
    profile: &crate::controller::config::AnkiNoteConfig,
    show_tags: bool,
    target: AnkiStateTarget,
) {
    let pb = app.global::<PopupBridge>();
    let active_bridge = active_rich_bridge_for_app(app);
    let nid_query = format!("nid:{note_id}");
    let joined_profile_tags = profile.tags.join(" ");

    let (mark_duplicate, overwrite_browse_query, set_last_added_flag) = match target {
        AnkiStateTarget::Added => {
            let is_same_deck = matches!(
                crate::controller::anki::AnkiDuplicatePolicy::try_from(profile.duplicate_policy),
                Ok(crate::controller::anki::AnkiDuplicatePolicy::SameDeck)
            );
            (!is_same_deck, true, true)
        }
        AnkiStateTarget::Updated { .. } => (true, false, false),
    };

    match target {
        AnkiStateTarget::Added => {
            if let Some(ref br) = active_bridge {
                let updated_panel = {
                    let mut st = br.state.borrow_mut();
                    let match_pos = st.pending_panel_anki_adds.iter().position(|pending| {
                        pending.row_index == row_index
                            && st.panel.rows.get(row_index).is_some_and(|r| {
                                r.expression.as_str() == pending.expression
                                    && r.reading.as_str() == pending.reading
                                    && r.is_kanji == pending.is_kanji
                            })
                    });
                    if let Some(pending) =
                        match_pos.and_then(|pos| st.pending_panel_anki_adds.remove(pos))
                    {
                        apply_anki_state_to_panel(
                            &mut st.panel,
                            &pending.expression,
                            &pending.reading,
                            pending.is_kanji,
                            pending.reading_as_expression,
                            note_id,
                            &nid_query,
                            &joined_profile_tags,
                            show_tags,
                            mark_duplicate,
                            overwrite_browse_query,
                        );
                        true
                    } else {
                        false
                    }
                };
                if updated_panel {
                    pb.set_anki_show_tags(show_tags);
                    br.sync_panel_popup_to_bridge(app, false);
                    return;
                }

                let updated_nested = {
                    let mut st = br.state.borrow_mut();
                    let match_pos = st.pending_nested_anki_adds.iter().position(|pending| {
                        pending.row_index == row_index
                            && st.nested_level(pending.depth as i32).is_some_and(|lvl| {
                                lvl.rows.get(row_index).is_some_and(|r| {
                                    r.expression.as_str() == pending.expression
                                        && r.reading.as_str() == pending.reading
                                        && r.is_kanji == pending.is_kanji
                                })
                            })
                    });
                    if let Some(pending) =
                        match_pos.and_then(|pos| st.pending_nested_anki_adds.remove(pos))
                    {
                        if let Some(lvl) = st.nested_level_mut(pending.depth as i32) {
                            apply_anki_state_to_nested_level(
                                lvl,
                                &pending.expression,
                                &pending.reading,
                                pending.is_kanji,
                                pending.reading_as_expression,
                                note_id,
                                &nid_query,
                                &joined_profile_tags,
                                show_tags,
                                mark_duplicate,
                                overwrite_browse_query,
                            );
                            true
                        } else {
                            false
                        }
                    } else {
                        false
                    }
                };
                if updated_nested {
                    pb.set_anki_show_tags(show_tags);
                    if set_last_added_flag {
                        pb.set_last_added_was_nested(true);
                    }
                    br.sync_nested_popup_to_bridge(app, false);
                    return;
                }
            }
        }
        AnkiStateTarget::Updated { nested_depth } if nested_depth > 0 => {
            if let Some(ref br) = active_bridge {
                let updated_nested = {
                    let mut st = br.state.borrow_mut();
                    if let Some(lvl) = st.nested_level_mut(nested_depth) {
                        if let Some(row) = lvl.rows.get(row_index).cloned() {
                            let rae = lvl
                                .row_reading_as_expression
                                .get(row_index)
                                .copied()
                                .unwrap_or(false);
                            apply_anki_state_to_nested_level(
                                lvl,
                                row.expression.as_str(),
                                row.reading.as_str(),
                                row.is_kanji,
                                rae,
                                note_id,
                                &nid_query,
                                &joined_profile_tags,
                                show_tags,
                                mark_duplicate,
                                overwrite_browse_query,
                            );
                            true
                        } else {
                            false
                        }
                    } else {
                        false
                    }
                };
                if updated_nested {
                    pb.set_anki_show_tags(show_tags);
                    br.sync_nested_popup_to_bridge(app, false);
                }
            }
            return;
        }
        AnkiStateTarget::Updated { .. } => {}
    }

    let full = app.get_dict_results();
    let count = full.row_count();
    let Some(target_row) = full.row_data(row_index) else {
        return;
    };
    let existing_dups = pb.get_row_anki_duplicates();
    let existing_note_ids = pb.get_row_anki_note_ids();
    let existing_note_id_strings = pb.get_row_anki_note_id_strings();
    let existing_queries = pb.get_row_anki_browse_queries();
    let existing_tags = pb.get_row_anki_tags();
    let existing_addable = pb.get_row_anki_addable();
    let target_rae = pb
        .get_row_reading_as_expression()
        .row_data(row_index)
        .unwrap_or(false);

    let mut dups = Vec::with_capacity(count);
    let mut note_ids_i32 = Vec::with_capacity(count);
    let mut note_ids_i64 = Vec::with_capacity(count);
    let mut note_id_strings = Vec::with_capacity(count);
    let mut queries = Vec::with_capacity(count);
    let mut tags = Vec::with_capacity(count);
    let mut addable = Vec::with_capacity(count);
    for idx in 0..count {
        let mut d = existing_dups.row_data(idx).unwrap_or(false);
        let mut nid32 = existing_note_ids.row_data(idx).unwrap_or(0);
        let mut nid_str = existing_note_id_strings.row_data(idx).unwrap_or_default();
        let mut nid64 = nid_str
            .as_str()
            .trim()
            .parse::<i64>()
            .ok()
            .unwrap_or_else(|| i64::from(nid32));
        let mut q = existing_queries.row_data(idx).unwrap_or_default();
        let mut t = existing_tags.row_data(idx).unwrap_or_default();
        let mut add = existing_addable.row_data(idx).unwrap_or(!d);
        if let Some(row) = full.row_data(idx) {
            if row.expression == target_row.expression
                && row.reading == target_row.reading
                && row.is_kanji == target_row.is_kanji
            {
                if mark_duplicate {
                    d = true;
                    add = false;
                    nid64 = note_id;
                    nid32 = encode_note_id_i32(note_id);
                    nid_str = if note_id > 0 {
                        SharedString::from(note_id.to_string())
                    } else {
                        SharedString::default()
                    };
                    if overwrite_browse_query || q.is_empty() {
                        q = SharedString::from(nid_query.as_str());
                    }
                }
                if show_tags && !joined_profile_tags.is_empty() {
                    t = merge_anki_tags(&t, &joined_profile_tags);
                }
            }
        }
        dups.push(d);
        note_ids_i32.push(nid32);
        note_ids_i64.push(nid64);
        note_id_strings.push(nid_str);
        queries.push(q);
        tags.push(t);
        addable.push(add);
    }
    if let Some(ref br) = active_bridge {
        let mut st = br.state.borrow_mut();
        st.root_row_anki_note_ids_i64 = note_ids_i64;
        st.root_row_anki_addable = addable.clone();
        if !target_row.is_kanji {
            let key = (
                target_row.expression.to_string(),
                target_row.reading.to_string(),
            );
            let map = if target_rae {
                &mut st.root_term_kana_statuses
            } else {
                &mut st.root_term_kanji_statuses
            };
            let entry = map.entry(key).or_insert_with(|| EntryAnkiStatus {
                can_add: !mark_duplicate,
                is_duplicate: mark_duplicate,
                note_ids: Vec::new(),
                tags: Vec::new(),
                browse_query: String::new(),
            });
            if mark_duplicate {
                entry.can_add = false;
                entry.is_duplicate = true;
                if note_id > 0 && !entry.note_ids.contains(&note_id) {
                    entry.note_ids.insert(0, note_id);
                }
                if overwrite_browse_query || entry.browse_query.is_empty() {
                    entry.browse_query = nid_query;
                }
            }
        }
    }
    if set_last_added_flag {
        pb.set_last_added_was_nested(false);
    }
    pb.set_anki_show_tags(show_tags);
    pb.set_row_anki_duplicates(Rc::new(VecModel::from(dups)).into());
    pb.set_row_anki_note_ids(Rc::new(VecModel::from(note_ids_i32)).into());
    pb.set_row_anki_note_id_strings(Rc::new(VecModel::from(note_id_strings)).into());
    pb.set_row_anki_browse_queries(Rc::new(VecModel::from(queries)).into());
    pb.set_row_anki_tags(Rc::new(VecModel::from(tags)).into());
    pb.set_row_anki_addable(Rc::new(VecModel::from(addable)).into());
    pb.set_popup_revision(pb.get_popup_revision().wrapping_add(1));
    pb.set_rich_revision(pb.get_rich_revision().wrapping_add(1));
}

/// Immediately updates `PopupBridge` Anki duplicate/browse/tag/note-id state for a term right after
/// `add_note_from_profile` succeeds so the `Browse in Anki` / `Update existing note` badges stay
/// accurate without waiting.
pub fn mark_popup_term_anki_added(
    app: &MementoApp,
    added_index: usize,
    note_id: i64,
    profile: &crate::controller::config::AnkiNoteConfig,
    show_tags: bool,
) {
    mark_popup_term_anki_state(
        app,
        added_index,
        note_id,
        profile,
        show_tags,
        AnkiStateTarget::Added,
    );
}

/// Immediately refreshes `PopupBridge` Anki duplicate/browse/tag/note-id state for a term right
/// after `update_existing_note_with_media` succeeds (supporting both root and nested popups).
pub fn mark_popup_term_anki_updated(
    app: &MementoApp,
    updated_index: usize,
    nested_depth: i32,
    note_id: i64,
    profile: &crate::controller::config::AnkiNoteConfig,
    show_tags: bool,
) {
    mark_popup_term_anki_state(
        app,
        updated_index,
        note_id,
        profile,
        show_tags,
        AnkiStateTarget::Updated { nested_depth },
    );
}

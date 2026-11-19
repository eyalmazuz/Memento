use std::rc::Rc;
use std::sync::atomic::Ordering;

use slint::{ComponentHandle, Model as _, SharedString, VecModel};

use crate::ui::popup_model::{active_rich_bridge_for_app, split_anki_tags_str};
use crate::ui::{DictResult, MementoApp, PopupBridge};

use super::state::{
    current_external_lookup_epoch, encode_note_id_i32, AnkiPopupCheckRequest,
    AnkiPopupCheckResponse, NestedPopupLevel,
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

impl RichUiBridge {
    pub(super) fn sync_anki_checking_flag(&self, app: &MementoApp) {
        let checking = self.root_anki_in_flight.load(Ordering::Acquire) != 0
            || self.nested_anki_in_flight.load(Ordering::Acquire) != 0;
        app.global::<PopupBridge>().set_anki_checking(checking);
    }

    pub(super) fn cancel_nested_anki_check(&self, app: &MementoApp) {
        self.nested_anki_nav_epoch.fetch_add(1, Ordering::AcqRel);
        self.nested_anki_in_flight.store(0, Ordering::Release);
        self.sync_anki_checking_flag(app);
    }

    pub(super) fn dispatch_anki_check_for_rows(
        &self,
        app: &MementoApp,
        rows: &[DictResult],
        target_nested_depth: Option<usize>,
    ) {
        let is_nested = target_nested_depth.is_some();
        let next_nav = if is_nested {
            self.nested_anki_nav_epoch.fetch_add(1, Ordering::AcqRel) + 1
        } else {
            self.anki_nav_epoch.fetch_add(1, Ordering::AcqRel) + 1
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
        if !is_nested {
            self.state.borrow_mut().root_row_anki_note_ids_i64.clear();
        }

        let pb = app.global::<PopupBridge>();
        pb.set_anki_show_tags(show_tags);
        if !is_nested {
            pb.set_row_anki_duplicates(Rc::new(VecModel::from(Vec::<bool>::new())).into());
            pb.set_row_anki_note_ids(Rc::new(VecModel::from(Vec::<i32>::new())).into());
            pb.set_row_anki_note_id_strings(
                Rc::new(VecModel::from(Vec::<SharedString>::new())).into(),
            );
            pb.set_row_anki_browse_queries(
                Rc::new(VecModel::from(Vec::<SharedString>::new())).into(),
            );
            pb.set_row_anki_tags(Rc::new(VecModel::from(Vec::<SharedString>::new())).into());
            pb.set_popup_revision(pb.get_popup_revision().wrapping_add(1));
        }

        let clear_in_flight = || {
            if is_nested {
                self.nested_anki_in_flight.store(0, Ordering::Release);
            } else {
                self.root_anki_in_flight.store(0, Ordering::Release);
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

        let lookup_epoch = current_external_lookup_epoch(&self.external_lookup_epoch);
        if is_nested {
            self.nested_anki_in_flight
                .store(next_nav, Ordering::Release);
        } else {
            self.root_anki_in_flight.store(next_nav, Ordering::Release);
        }
        self.sync_anki_checking_flag(app);

        let sent = self
            .anki_tx
            .send(AnkiPopupCheckRequest {
                nav_epoch: next_nav,
                lookup_epoch,
                target_nested_depth,
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

    pub(super) fn drain_anki_results(&self, app: &MementoApp) -> bool {
        let mut latest_root: Option<AnkiPopupCheckResponse> = None;
        let mut latest_nested: Option<AnkiPopupCheckResponse> = None;
        if let Ok(guard) = self.anki_res_rx.lock() {
            while let Ok(item) = guard.try_recv() {
                if item.target_nested_depth.is_some() {
                    latest_nested = Some(item);
                } else {
                    latest_root = Some(item);
                }
            }
        }
        if latest_root.is_none() && latest_nested.is_none() {
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
                let mut dups = Vec::with_capacity(count);
                let mut note_ids_i32 = Vec::with_capacity(count);
                let mut note_ids_i64 = Vec::with_capacity(count);
                let mut note_id_strings = Vec::with_capacity(count);
                let mut queries = Vec::with_capacity(count);
                let mut tags = Vec::with_capacity(count);
                for idx in 0..count {
                    if let Some(row) = full.row_data(idx) {
                        let status_opt = if row.is_kanji {
                            resp.kanji_statuses.get(row.expression.as_str())
                        } else {
                            resp.term_statuses
                                .get(&(row.expression.to_string(), row.reading.to_string()))
                        };
                        if let Some(st) = status_opt {
                            let nid = st.note_ids.first().copied().unwrap_or(0);
                            dups.push(st.is_duplicate);
                            note_ids_i32.push(encode_note_id_i32(nid));
                            note_ids_i64.push(nid);
                            note_id_strings.push(if nid > 0 {
                                SharedString::from(nid.to_string())
                            } else {
                                SharedString::default()
                            });
                            queries.push(SharedString::from(st.browse_query.as_str()));
                            tags.push(SharedString::from(st.tags.join(" ")));
                        } else {
                            dups.push(false);
                            note_ids_i32.push(0);
                            note_ids_i64.push(0);
                            note_id_strings.push(SharedString::default());
                            queries.push(SharedString::default());
                            tags.push(SharedString::default());
                        }
                    } else {
                        dups.push(false);
                        note_ids_i32.push(0);
                        note_ids_i64.push(0);
                        note_id_strings.push(SharedString::default());
                        queries.push(SharedString::default());
                        tags.push(SharedString::default());
                    }
                }
                self.state.borrow_mut().root_row_anki_note_ids_i64 = note_ids_i64;
                pb.set_anki_show_tags(resp.show_tags);
                pb.set_row_anki_duplicates(Rc::new(VecModel::from(dups)).into());
                pb.set_row_anki_note_ids(Rc::new(VecModel::from(note_ids_i32)).into());
                pb.set_row_anki_note_id_strings(Rc::new(VecModel::from(note_id_strings)).into());
                pb.set_row_anki_browse_queries(Rc::new(VecModel::from(queries)).into());
                pb.set_row_anki_tags(Rc::new(VecModel::from(tags)).into());
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
                if let Some(depth) = resp.target_nested_depth {
                    let sync_needed = {
                        let mut st = self.state.borrow_mut();
                        let stack_len = st.nested_stack.len();
                        if let Some(lvl) = st.nested_level_mut(depth as i32) {
                            let count = lvl.rows.len();
                            let mut dups = Vec::with_capacity(count);
                            let mut note_ids_i64 = Vec::with_capacity(count);
                            let mut queries = Vec::with_capacity(count);
                            let mut tags = Vec::with_capacity(count);
                            for row in &lvl.rows {
                                let status_opt = if row.is_kanji {
                                    resp.kanji_statuses.get(row.expression.as_str())
                                } else {
                                    resp.term_statuses
                                        .get(&(row.expression.to_string(), row.reading.to_string()))
                                };
                                if let Some(entry_st) = status_opt {
                                    let nid = entry_st.note_ids.first().copied().unwrap_or(0);
                                    dups.push(entry_st.is_duplicate);
                                    note_ids_i64.push(nid);
                                    queries.push(SharedString::from(entry_st.browse_query.as_str()));
                                    tags.push(SharedString::from(entry_st.tags.join(" ")));
                                } else {
                                    dups.push(false);
                                    note_ids_i64.push(0);
                                    queries.push(SharedString::default());
                                    tags.push(SharedString::default());
                                }
                            }
                            lvl.row_anki_duplicates = dups;
                            lvl.row_anki_note_ids_i64 = note_ids_i64;
                            lvl.row_anki_browse_queries = queries;
                            lvl.row_anki_tags = tags;
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

fn apply_anki_state_to_nested_level(
    lvl: &mut NestedPopupLevel,
    expression: &str,
    reading: &str,
    is_kanji: bool,
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
    for idx in 0..count {
        let row = &lvl.rows[idx];
        if row.expression.as_str() == expression
            && row.reading.as_str() == reading
            && row.is_kanji == is_kanji
        {
            if mark_duplicate {
                lvl.row_anki_duplicates[idx] = true;
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
                            apply_anki_state_to_nested_level(
                                lvl,
                                row.expression.as_str(),
                                row.reading.as_str(),
                                row.is_kanji,
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

    let mut dups = Vec::with_capacity(count);
    let mut note_ids_i32 = Vec::with_capacity(count);
    let mut note_ids_i64 = Vec::with_capacity(count);
    let mut note_id_strings = Vec::with_capacity(count);
    let mut queries = Vec::with_capacity(count);
    let mut tags = Vec::with_capacity(count);
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
        if let Some(row) = full.row_data(idx) {
            if row.expression == target_row.expression
                && row.reading == target_row.reading
                && row.is_kanji == target_row.is_kanji
            {
                if mark_duplicate {
                    d = true;
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
    }
    if let Some(ref br) = active_bridge {
        br.state.borrow_mut().root_row_anki_note_ids_i64 = note_ids_i64;
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

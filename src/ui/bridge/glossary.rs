use std::rc::Rc;
use std::sync::atomic::Ordering;

use slint::{ComponentHandle, VecModel};

use crate::ui::glossary_render::{
    compute_collapsible_metrics, quantize_glossary_render_scale,
    render_structured_glossary_entry_with_value_scaled, GlossaryZoneAction, RenderedGlossaryDoc,
};
use crate::ui::subtitle_layout::is_japanese_lookup_char;
use crate::ui::{MementoApp, PopupBridge, RichGlossaryRender};

use super::state::{
    build_selection_rects_for_cells, GlossaryDocCacheKey, GlossarySelectionState,
};
use super::RichUiBridge;

impl RichUiBridge {
    pub(super) fn render_or_get_glossary_at_depth(
        &self,
        depth: i32,
        row_index: i32,
        width_px: f32,
        selected: bool,
    ) -> Option<RenderedGlossaryDoc> {
        let bucket_w = ((width_px.max(120.0) / 4.0).floor() as i32 * 4).max(120);
        let window_scale = self
            .app_weak
            .upgrade()
            .map(|app| app.window().scale_factor())
            .unwrap_or(1.0);
        let (render_scale, scale_quarter) = quantize_glossary_render_scale(window_scale);
        let mut st = self.state.borrow_mut();
        let (hit, parsed_glossary, detail_states, epoch) = if depth == 0 {
            let extras = st.row_extras.get(&row_index)?;
            let hit = extras.hit.as_ref()?.clone();
            (
                hit,
                extras.parsed_glossary.clone(),
                extras.detail_states.clone(),
                st.rich_epoch,
            )
        } else {
            let lvl = st.nested_level(depth)?;
            let extras = lvl.row_extras.get(&row_index)?;
            let hit = extras.hit.as_ref()?.clone();
            (
                hit,
                extras.parsed_glossary.clone(),
                extras.detail_states.clone(),
                lvl.rich_epoch,
            )
        };
        if hit.is_kanji {
            return None;
        }
        let mut pairs: Vec<_> = detail_states.iter().collect();
        pairs.sort_by_key(|(k, _)| *k);
        let details_sig: String = pairs
            .into_iter()
            .map(|(k, v)| format!("{k}:{};", u8::from(*v)))
            .collect();
        let cache_key = GlossaryDocCacheKey {
            row_index,
            width_px: bucket_w,
            selected,
            details_sig,
            rich_epoch: epoch,
            scale_quarter,
        };
        if depth == 0 {
            if let Some(doc) = st.glossary_cache.get(&cache_key) {
                st.last_rendered_key.insert(row_index, cache_key);
                return Some(doc);
            }
        } else if let Some(lvl) = st.nested_level_mut(depth) {
            if let Some(doc) = lvl.glossary_cache.get(&cache_key) {
                lvl.last_rendered_key.insert(row_index, cache_key);
                return Some(doc);
            }
        }
        let res_dirs = st.dict_resource_dirs.clone();
        let theme = st.effective_popup_theme();
        let legacy_style = st.settings.glossary_style;
        let sheet = st
            .stylesheets
            .get_for_dictionary(&res_dirs, hit.dictionary_id, &hit.dict_name);
        let doc = render_structured_glossary_entry_with_value_scaled(
            &hit.raw_glossary,
            &hit.glossary,
            parsed_glossary.as_deref(),
            sheet,
            &detail_states,
            legacy_style,
            &res_dirs,
            hit.dictionary_id,
            &hit.dict_name,
            bucket_w as f32,
            selected,
            &theme,
            render_scale,
        )?;
        if depth == 0 {
            st.last_rendered_key.insert(row_index, cache_key.clone());
            st.glossary_cache.insert(cache_key, doc.clone());
        } else if let Some(lvl) = st.nested_level_mut(depth) {
            lvl.last_rendered_key.insert(row_index, cache_key.clone());
            lvl.glossary_cache.insert(cache_key, doc.clone());
        }
        Some(doc)
    }

    pub(super) fn latest_cached_doc_at_depth(
        &self,
        depth: i32,
        row_index: i32,
    ) -> Option<RenderedGlossaryDoc> {
        let st = self.state.borrow();
        let (epoch, last_key, cache) = if depth == 0 {
            (st.rich_epoch, &st.last_rendered_key, &st.glossary_cache)
        } else {
            let lvl = st.nested_level(depth)?;
            (lvl.rich_epoch, &lvl.last_rendered_key, &lvl.glossary_cache)
        };
        if let Some(key) = last_key.get(&row_index) {
            if key.rich_epoch == epoch {
                if let Some(doc) = cache.peek(key) {
                    return Some(doc);
                }
            }
        }
        cache.find_by_row_and_epoch(row_index, epoch)
    }

    pub(super) fn build_rich_glossary_render(
        &self,
        depth: i32,
        row_index: i32,
        avail_w: f32,
        selected: bool,
    ) -> RichGlossaryRender {
        let Some(doc) = self.render_or_get_glossary_at_depth(depth, row_index, avail_w, selected)
        else {
            return RichGlossaryRender::default();
        };
        let (sel_rects, scan_rects, collapsible, collapsed_h, total_lines, expanded) = {
            let st = self.state.borrow();
            let (sel_opt, scan_opt, expanded, dict_name) = if depth == 0 {
                let dname = st
                    .row_extras
                    .get(&row_index)
                    .and_then(|e| e.hit.as_ref())
                    .map(|h| h.dict_name.as_str())
                    .unwrap_or("");
                (
                    st.selection.as_ref(),
                    st.root_scan_highlight.as_ref(),
                    st.root_expanded_cards.contains(&row_index),
                    dname,
                )
            } else if let Some(lvl) = st.nested_level(depth) {
                let dname = lvl
                    .row_extras
                    .get(&row_index)
                    .and_then(|e| e.hit.as_ref())
                    .map(|h| h.dict_name.as_str())
                    .unwrap_or("");
                (
                    lvl.selection.as_ref(),
                    lvl.scan_highlight.as_ref(),
                    lvl.expanded_cards.contains(&row_index),
                    dname,
                )
            } else {
                (None, None, false, "")
            };
            let sel_rects = if let Some(sel) = sel_opt {
                if sel.row_index == row_index && sel.anchor_char != sel.focus_char {
                    let lo = sel
                        .anchor_char
                        .min(sel.focus_char)
                        .min(doc.selectable_chars.len());
                    let hi = sel
                        .anchor_char
                        .max(sel.focus_char)
                        .min(doc.selectable_chars.len());
                    let indices: Vec<usize> = (lo..hi).collect();
                    build_selection_rects_for_cells(&doc.selectable_chars, &indices)
                } else {
                    Vec::new()
                }
            } else {
                Vec::new()
            };
            let scan_rects = if let Some(hl) = scan_opt {
                if hl.row_index == row_index && !hl.cell_indices.is_empty() {
                    build_selection_rects_for_cells(&doc.selectable_chars, &hl.cell_indices)
                } else {
                    Vec::new()
                }
            } else {
                Vec::new()
            };
            let line_limit = st
                .effective_popup_theme()
                .resolve_dict_style(dict_name)
                .collapsible_line_count;
            let (collapsible, collapsed_h, total_lines) =
                compute_collapsible_metrics(&doc, line_limit);
            (
                sel_rects,
                scan_rects,
                collapsible,
                collapsed_h,
                total_lines,
                expanded,
            )
        };
        RichGlossaryRender {
            has_rich: true,
            width_px: doc.width_px,
            height_px: doc.height_px,
            rendered: doc.image,
            selection_rects: Rc::new(VecModel::from(sel_rects)).into(),
            scan_highlight_rects: Rc::new(VecModel::from(scan_rects)).into(),
            collapsible,
            collapsed_height: collapsed_h,
            total_lines,
            expanded,
        }
    }

    pub(super) fn char_index_at_point(doc: &RenderedGlossaryDoc, x: f32, y: f32) -> Option<usize> {
        if doc.selectable_chars.is_empty() {
            return None;
        }
        let mut best: Option<(usize, f32)> = None;
        for (idx, cell) in doc.selectable_chars.iter().enumerate() {
            if cell.ch == '\n' {
                continue;
            }
            let dy = if y < cell.y {
                cell.y - y
            } else if y > cell.y + cell.h {
                y - (cell.y + cell.h)
            } else {
                0.0
            };
            let dx = if x < cell.x {
                cell.x - x
            } else if x > cell.x + cell.w {
                x - (cell.x + cell.w)
            } else {
                0.0
            };
            let score = dy * 8.0 + dx;
            if best.is_none_or(|(_, b_score)| score < b_score) {
                best = Some((idx, score));
            }
        }
        best.map(|(idx, _)| idx)
    }

    pub(super) fn update_glossary_selection_text_at_depth(&self, app: &MementoApp, depth: i32) {
        let sel_opt = {
            let st = self.state.borrow();
            if depth == 0 {
                st.selection.clone()
            } else {
                st.nested_level(depth).and_then(|lvl| lvl.selection.clone())
            }
        };
        let text = match sel_opt {
            Some(sel) if sel.anchor_char != sel.focus_char => {
                if let Some(doc) = self.latest_cached_doc_at_depth(depth, sel.row_index) {
                    let lo = sel
                        .anchor_char
                        .min(sel.focus_char)
                        .min(doc.selectable_chars.len());
                    let hi = sel
                        .anchor_char
                        .max(sel.focus_char)
                        .min(doc.selectable_chars.len());
                    doc.selectable_chars[lo..hi]
                        .iter()
                        .map(|c| c.ch)
                        .collect::<String>()
                        .trim_matches(|c: char| {
                            matches!(c, '\u{2060}'..='\u{206a}' | '\u{fdd0}'..='\u{fdef}' | '\u{fffc}')
                        })
                        .trim()
                        .to_owned()
                } else {
                    String::new()
                }
            }
            _ => String::new(),
        };
        let pb = app.global::<PopupBridge>();
        pb.set_selected_glossary_text(text.into());
        pb.set_selection_revision(pb.get_selection_revision().wrapping_add(1));
    }

    pub(super) fn toggle_glossary_details_at_depth(
        &self,
        app: &MementoApp,
        depth: i32,
        row_idx: i32,
        detail_id: &str,
    ) {
        {
            let mut st = self.state.borrow_mut();
            if depth == 0 {
                if let Some(extras) = st.row_extras.get_mut(&row_idx) {
                    let current = extras.detail_states.get(detail_id).copied().unwrap_or(false);
                    extras.detail_states.insert(detail_id.to_owned(), !current);
                }
                st.glossary_cache.retain(|k| k.row_index != row_idx);
                st.last_rendered_key.remove(&row_idx);
            } else if let Some(lvl) = st.nested_level_mut(depth) {
                if let Some(extras) = lvl.row_extras.get_mut(&row_idx) {
                    let current = extras.detail_states.get(detail_id).copied().unwrap_or(false);
                    extras.detail_states.insert(detail_id.to_owned(), !current);
                }
                lvl.glossary_cache.retain(|k| k.row_index != row_idx);
                lvl.last_rendered_key.remove(&row_idx);
            }
        }
        let pb = app.global::<PopupBridge>();
        pb.set_tooltip_visible(false);
        if depth == 0 {
            pb.set_rich_revision(pb.get_rich_revision().wrapping_add(1));
        } else {
            pb.set_nested_revision(pb.get_nested_revision().wrapping_add(1));
        }
    }

    /// Resolve which row owns `detail_id` when a fallback link callback does not supply `row_idx`.
    fn resolve_details_row_index(
        &self,
        depth: i32,
        explicit_row: Option<i32>,
        detail_id: &str,
    ) -> Option<i32> {
        if let Some(r) = explicit_row.filter(|&r| r > 0) {
            return Some(r);
        }
        let st = self.state.borrow();
        let (extras_map, preferred_rows): (_, [Option<i32>; 2]) = if depth == 0 {
            (
                &st.row_extras,
                [
                    (st.last_active_glossary_row_idx > 0).then_some(st.last_active_glossary_row_idx),
                    st.selection.as_ref().map(|s| s.row_index),
                ],
            )
        } else {
            let lvl = st.nested_level(depth)?;
            (
                &lvl.row_extras,
                [lvl.selection.as_ref().map(|s| s.row_index), None],
            )
        };

        for candidate in preferred_rows.into_iter().flatten() {
            if extras_map
                .get(&candidate)
                .is_some_and(|e| e.detail_states.contains_key(detail_id))
            {
                return Some(candidate);
            }
        }
        if let Some(matching_row) = extras_map
            .iter()
            .filter(|(_, e)| e.detail_states.contains_key(detail_id))
            .map(|(&idx, _)| idx)
            .min()
        {
            return Some(matching_row);
        }
        preferred_rows
            .into_iter()
            .flatten()
            .find(|idx| extras_map.contains_key(idx))
            .or_else(|| extras_map.keys().copied().min())
    }

    pub(super) fn handle_internal_link_str_at_depth(
        &self,
        app: &MementoApp,
        depth: i32,
        row_idx: Option<i32>,
        link: &str,
    ) -> bool {
        if let Some(rest) = link.strip_prefix("memento://glossary-details?") {
            let mut id = String::new();
            let mut link_row: Option<i32> = None;
            for (k, v) in url::form_urlencoded::parse(rest.as_bytes()) {
                if k == "id" {
                    id = v.into_owned();
                } else if k == "row" {
                    link_row = v.parse::<i32>().ok().filter(|&r| r > 0);
                }
            }
            if !id.is_empty() {
                if let Some(resolved_row) =
                    self.resolve_details_row_index(depth, link_row.or(row_idx), &id)
                {
                    self.toggle_glossary_details_at_depth(app, depth, resolved_row, &id);
                }
            }
            return true;
        }
        if let Some(rest) = link.strip_prefix("memento://glossary-link?") {
            let mut target = String::new();
            let mut link_row: Option<i32> = None;
            for (k, v) in url::form_urlencoded::parse(rest.as_bytes()) {
                if k == "target" {
                    target = v.into_owned();
                } else if k == "row" {
                    link_row = v.parse::<i32>().ok().filter(|&r| r > 0);
                }
            }
            if !target.is_empty() {
                return self.handle_internal_link_str_at_depth(
                    app,
                    depth,
                    link_row.or(row_idx),
                    &target,
                );
            }
            return true;
        }
        if let Some(query_str) = link.strip_prefix('?') {
            if let Some((_, q)) =
                url::form_urlencoded::parse(query_str.as_bytes()).find(|(k, _)| k == "query")
            {
                let q_owned = q.into_owned();
                if !q_owned.is_empty() {
                    if depth == 0 {
                        self.navigate_to_term_query(app, &q_owned, None);
                    } else {
                        self.navigate_nested_query(app, depth, &q_owned);
                    }
                    return true;
                }
            }
        }
        false
    }

    pub(super) fn handle_glossary_pointer_down_at_depth(
        &self,
        app: &MementoApp,
        depth: i32,
        row_idx: i32,
        x: f32,
        y: f32,
    ) {
        // Clicking/dragging immediately cancels any pending hover-scan timer or in-flight scan.
        self.nested_epoch.fetch_add(1, Ordering::AcqRel);
        let mut truncated_nested = false;
        let mut locked_nested = false;
        {
            let mut st = self.state.borrow_mut();
            st.nested_scan_timer.stop();
            if depth == 0 && row_idx > 0 {
                st.last_active_glossary_row_idx = row_idx;
            }
            if depth >= 0 && (depth as usize) < st.nested_stack.len() {
                let keep = depth as usize;
                st.nested_stack.truncate(keep);
                st.nested_body_hover_count = 0;
                st.pending_nested_anki_adds.retain(|p| p.depth <= keep);
                if depth == 0 {
                    st.root_scan_highlight = None;
                    st.nested_card_hovered = false;
                } else if let Some(lvl) = st.nested_level_mut(depth) {
                    lvl.scan_highlight = None;
                }
                truncated_nested = true;
            } else if depth > 0 {
                st.nested_dismiss_timer.stop();
                if let Some(lvl) = st.nested_level_mut(depth) {
                    lvl.locked = true;
                    locked_nested = true;
                }
            }
        }
        if truncated_nested {
            self.cancel_nested_anki_check(app);
            let pb = app.global::<PopupBridge>();
            pb.set_nested_searching(false);
            self.sync_nested_popup_to_bridge(app, true);
            pb.set_rich_revision(pb.get_rich_revision().wrapping_add(1));
        } else if locked_nested {
            app.global::<PopupBridge>().set_nested_locked(true);
        }

        let Some(doc) = self.latest_cached_doc_at_depth(depth, row_idx) else {
            return;
        };
        if let Some(ci) = RichUiBridge::char_index_at_point(&doc, x, y) {
            let sel = GlossarySelectionState {
                row_index: row_idx,
                anchor_char: ci,
                focus_char: ci,
                dragging: false,
                press_pos: (x, y),
            };
            {
                let mut st = self.state.borrow_mut();
                if depth == 0 {
                    st.selection = Some(sel);
                } else if let Some(lvl) = st.nested_level_mut(depth) {
                    lvl.selection = Some(sel);
                }
            }
            self.update_glossary_selection_text_at_depth(app, depth);
        }
    }

    pub(super) fn handle_glossary_pointer_drag_at_depth(
        &self,
        app: &MementoApp,
        depth: i32,
        row_idx: i32,
        x: f32,
        y: f32,
    ) {
        let Some(doc) = self.latest_cached_doc_at_depth(depth, row_idx) else {
            return;
        };
        let mut updated = false;
        {
            let mut st = self.state.borrow_mut();
            st.nested_scan_timer.stop();
            let sel_slot = if depth == 0 {
                &mut st.selection
            } else if let Some(lvl) = st.nested_level_mut(depth) {
                &mut lvl.selection
            } else {
                return;
            };
            if let Some(ref mut sel) = sel_slot {
                if sel.row_index == row_idx {
                    let dist = (x - sel.press_pos.0).hypot(y - sel.press_pos.1);
                    if dist >= 3.0 {
                        sel.dragging = true;
                    }
                    if sel.dragging {
                        if let Some(ci) = RichUiBridge::char_index_at_point(&doc, x, y) {
                            let target = if x
                                > doc.selectable_chars[ci].x + doc.selectable_chars[ci].w * 0.5
                            {
                                (ci + 1).min(doc.selectable_chars.len())
                            } else {
                                ci
                            };
                            if sel.focus_char != target {
                                sel.focus_char = target;
                                updated = true;
                            }
                        }
                    }
                }
            }
        }
        if updated {
            self.update_glossary_selection_text_at_depth(app, depth);
        }
    }

    pub(super) fn handle_glossary_pointer_up_at_depth(
        &self,
        app: &MementoApp,
        depth: i32,
        row_idx: i32,
        x: f32,
        y: f32,
    ) {
        let was_dragging = {
            let mut st = self.state.borrow_mut();
            let sel_slot = if depth == 0 {
                &mut st.selection
            } else if let Some(lvl) = st.nested_level_mut(depth) {
                &mut lvl.selection
            } else {
                return;
            };
            let dragged = sel_slot
                .as_ref()
                .is_some_and(|s| s.dragging && s.anchor_char != s.focus_char);
            if let Some(ref mut s) = sel_slot {
                s.dragging = false;
            }
            dragged
        };
        if was_dragging {
            return;
        }
        if let Some(doc) = self.latest_cached_doc_at_depth(depth, row_idx) {
            if let Some(zone) = doc.hit_zones.iter().rev().find(|z| {
                z.action.is_some() && x >= z.x && x <= z.x + z.w && y >= z.y && y <= z.y + z.h
            }) {
                match zone.action.as_ref() {
                    Some(GlossaryZoneAction::ToggleDetails(did)) => {
                        self.toggle_glossary_details_at_depth(app, depth, row_idx, did);
                    }
                    Some(GlossaryZoneAction::Link(href)) => {
                        if !self.handle_internal_link_str_at_depth(
                            app,
                            depth,
                            Some(row_idx),
                            href,
                        ) {
                            app.invoke_open_glossary_link(href.clone().into());
                        }
                    }
                    None => {}
                }
            }
        }
    }

    pub(super) fn handle_glossary_double_click_at_depth(
        &self,
        app: &MementoApp,
        depth: i32,
        row_idx: i32,
        x: f32,
        y: f32,
    ) {
        let Some(doc) = self.latest_cached_doc_at_depth(depth, row_idx) else {
            return;
        };
        let Some(ci) = RichUiBridge::char_index_at_point(&doc, x, y) else {
            return;
        };
        let is_word = |c: char| c.is_alphanumeric() || is_japanese_lookup_char(c);
        let mut lo = ci;
        while lo > 0 && is_word(doc.selectable_chars[lo - 1].ch) {
            lo -= 1;
        }
        let mut hi = (ci + 1).min(doc.selectable_chars.len());
        while hi < doc.selectable_chars.len() && is_word(doc.selectable_chars[hi].ch) {
            hi += 1;
        }
        let sel = GlossarySelectionState {
            row_index: row_idx,
            anchor_char: lo,
            focus_char: hi,
            dragging: false,
            press_pos: (x, y),
        };
        {
            let mut st = self.state.borrow_mut();
            if depth == 0 {
                st.selection = Some(sel);
            } else if let Some(lvl) = st.nested_level_mut(depth) {
                lvl.selection = Some(sel);
            }
        }
        self.update_glossary_selection_text_at_depth(app, depth);
    }
}

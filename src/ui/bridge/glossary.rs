use std::collections::HashMap;
use std::rc::Rc;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use slint::{ComponentHandle, Model as _, VecModel};

use crate::ui::glossary_render::{
    compute_collapsible_metrics, quantize_glossary_render_scale,
    render_structured_glossary_entry_with_value_scaled, GlossaryZoneAction, RenderedGlossaryDoc,
};
use crate::ui::popup_model::active_rich_bridge_for_app;
use crate::ui::subtitle_layout::is_japanese_lookup_char;
use crate::ui::{DictResult, SubomiApp, PopupBridge, RichGlossaryRender};

use super::state::{
    build_selection_rects_for_cells, row_term_def_key, row_term_def_key_model, GlossaryDocCacheKey,
    GlossaryPreRenderBatch, GlossaryPreRenderItem, GlossarySelectionState,
    GLOSSARY_PRERENDER_MAX_ROWS,
};
use super::RichUiBridge;

pub(super) fn format_details_sig(detail_states: &HashMap<String, bool>) -> String {
    if detail_states.is_empty() {
        return String::new();
    }
    let mut pairs: Vec<_> = detail_states.iter().collect();
    pairs.sort_by_key(|(k, _)| *k);
    pairs
        .into_iter()
        .map(|(k, v)| format!("{k}:{};", u8::from(*v)))
        .collect()
}

fn popup_width_and_cols(app: &SubomiApp) -> (f32, bool, f32, f32) {
    let pb = app.global::<PopupBridge>();
    let live_w = app.get_popup_width();
    let popup_w = if live_w > 1.0 {
        live_w
    } else {
        let css_w = pb.get_css_popup_width();
        if css_w > 0.0 {
            css_w
        } else {
            app.get_popup_configured_width().max(320.0)
        }
    };
    let use_two_cols = pb.get_css_definition_columns() >= 2
        && popup_w >= pb.get_css_definition_columns_min_width();
    let popup_pad = pb.get_css_popup_padding().max(0.0);
    let col_gap = pb.get_css_definition_gap().round().max(0.0);
    (popup_w, use_two_cols, popup_pad, col_gap)
}

pub(super) fn drain_glossary_prerender_results_for_app(app: &SubomiApp) -> bool {
    if let Some(br) = active_rich_bridge_for_app(app) {
        br.drain_glossary_prerender_results(app)
    } else {
        false
    }
}

impl RichUiBridge {
    pub(super) fn dispatch_glossary_prerender(&self, app: &SubomiApp, rows: &[DictResult]) {
        let Some(ref tx) = self.glossary_render_tx else {
            return;
        };
        if rows.is_empty() {
            return;
        }

        let window_scale = app.window().scale_factor();
        let (render_scale, scale_quarter) = quantize_glossary_render_scale(window_scale);
        let (popup_w, use_two_cols, popup_pad, col_gap) = popup_width_and_cols(app);
        let popup_w_rounded = popup_w.round() as i32;
        let grid_w = (popup_w - popup_pad * 2.0 - 12.0).max(160.0);
        let col_w = ((grid_w - col_gap) * 0.5).floor().max(120.0);
        let solo_w = grid_w.floor().max(120.0);

        let epoch = self.glossary_render_epoch.fetch_add(1, Ordering::AcqRel) + 1;
        let items: Vec<GlossaryPreRenderItem> = {
            let mut st_guard = self.state.borrow_mut();
            let st = &mut *st_guard;
            let rich_epoch = st.rich_epoch;
            let res_dirs = st.dict_resource_dirs.clone();
            let theme = st.effective_popup_theme();
            let legacy_style = st.settings.glossary_style;
            let observed_solo = st
                .last_observed_solo_width_px
                .filter(|&(pw, two, _)| pw == popup_w_rounded && two == use_two_cols)
                .map(|(_, _, bw)| bw);
            let observed_col = st
                .last_observed_col_width_px
                .filter(|&(pw, two, _)| pw == popup_w_rounded && two == use_two_cols)
                .map(|(_, _, bw)| bw);

            let mut out = Vec::with_capacity(GLOSSARY_PRERENDER_MAX_ROWS);
            let mut i = 0usize;
            while i < rows.len() && out.len() < GLOSSARY_PRERENDER_MAX_ROWS {
                let row = &rows[i];
                if row.collapsed && !row.term_start {
                    i += 1;
                    continue;
                }
                if row.is_kanji {
                    i += 1;
                    continue;
                }
                let is_solo = !use_two_cols || (row.term_start && row.term_end);
                let row_index = row.index;
                let Some(extras) = st.row_extras.get(&row_index) else {
                    i += 1;
                    continue;
                };
                let Some(hit) = extras.hit.as_ref() else {
                    i += 1;
                    continue;
                };
                if hit.is_kanji {
                    i += 1;
                    continue;
                }
                let card_pad = theme.resolve_dict_style(&hit.dict_name).card_padding.max(0.0);
                let bucket_w = if is_solo {
                    observed_solo.unwrap_or_else(|| {
                        (((solo_w - card_pad * 2.0).max(120.0) / 4.0).floor() as i32 * 4).max(120)
                    })
                } else {
                    observed_col.unwrap_or_else(|| {
                        (((col_w - card_pad * 2.0).max(120.0) / 4.0).floor() as i32 * 4).max(120)
                    })
                };
                let details_sig = format_details_sig(&extras.detail_states);
                let cache_key = GlossaryDocCacheKey {
                    row_index,
                    width_px: bucket_w,
                    selected: row.selected,
                    details_sig,
                    rich_epoch,
                    scale_quarter,
                    css_theme_light: theme.is_light_theme(),
                    compact_glossaries: theme.compact_glossaries,
                };
                if st.glossary_cache.contains_key(&cache_key) {
                    i += 1;
                    continue;
                }
                let raw_glossary = hit.raw_glossary.clone();
                let fallback_glossary = hit.glossary.clone();
                let parsed_glossary = extras.parsed_glossary.clone();
                let detail_states = extras.detail_states.clone();
                let dic_id = hit.dictionary_id;
                let dict_name = hit.dict_name.clone();
                let sheet = st
                    .stylesheets
                    .get_for_dictionary(&res_dirs, dic_id, &dict_name);
                out.push(GlossaryPreRenderItem {
                    cache_key,
                    raw_glossary,
                    fallback_glossary,
                    parsed_glossary,
                    stylesheet: sheet,
                    detail_states,
                    legacy_style,
                    dict_resource_dirs: res_dirs.clone(),
                    dic_id,
                    dict_name,
                    avail_width_px: bucket_w as f32,
                    selected: row.selected,
                    theme: theme.clone(),
                    render_scale,
                });
                i += 1;
            }
            out
        };

        if !items.is_empty() {
            let _ = tx.send(GlossaryPreRenderBatch { epoch, items });
        }
    }

    pub(super) fn drain_glossary_prerender_results(&self, app: &SubomiApp) -> bool {
        let mut completed = Vec::new();
        if let Ok(guard) = self.glossary_render_res_rx.lock() {
            while let Ok(item) = guard.try_recv() {
                completed.push(item);
            }
        }
        if completed.is_empty() {
            return false;
        }
        if !app.get_dict_open() {
            return false;
        }
        let current_epoch = self.glossary_render_epoch.load(Ordering::Acquire);
        let mut st = self.state.borrow_mut();
        let mut inserted_any = false;
        for done in completed {
            if done.epoch != current_epoch || done.cache_key.rich_epoch != st.rich_epoch {
                continue;
            }
            if st.glossary_cache.contains_key(&done.cache_key) {
                continue;
            }
            let rendered = Arc::new(done.doc.into_rendered_doc());
            st.glossary_cache.insert(done.cache_key, rendered);
            inserted_any = true;
        }
        inserted_any
    }

    pub(super) fn render_or_get_glossary_at_depth(
        &self,
        depth: i32,
        row_index: i32,
        width_px: f32,
        selected: bool,
    ) -> Option<Arc<RenderedGlossaryDoc>> {
        self.cancel_idle_memory_shrink();
        let bucket_w = ((width_px.max(120.0) / 4.0).floor() as i32 * 4).max(120);
        let app_opt = self.app_weak.upgrade();
        let window_scale = app_opt
            .as_ref()
            .map(|app| app.window().scale_factor())
            .unwrap_or(1.0);
        let (render_scale, scale_quarter) = quantize_glossary_render_scale(window_scale);
        let observed_root_width = if depth == 0 {
            app_opt.as_ref().map(|app| {
                let (popup_w, use_two_cols, _, _) = popup_width_and_cols(app);
                let pw_rounded = popup_w.round() as i32;
                let is_solo = if !use_two_cols {
                    true
                } else {
                    let full = app.get_dict_results();
                    let zero_idx = row_index.saturating_sub(1) as usize;
                    full.row_data(zero_idx)
                        .is_some_and(|r| r.term_start && r.term_end)
                };
                (pw_rounded, use_two_cols, is_solo)
            })
        } else {
            None
        };
        let mut st = self.state.borrow_mut();
        if let Some((pw_rounded, use_two_cols, is_solo)) = observed_root_width {
            if is_solo {
                st.last_observed_solo_width_px = Some((pw_rounded, use_two_cols, bucket_w));
            } else {
                st.last_observed_col_width_px = Some((pw_rounded, use_two_cols, bucket_w));
            }
        }
        // Build the cache key from borrowed row metadata first so cache hits never
        // clone `SearchHit` or `detail_states`.
        let (is_kanji, details_sig, epoch) = if depth < 0 {
            let extras = st.panel.row_extras.get(&row_index)?;
            let hit = extras.hit.as_ref()?;
            (
                hit.is_kanji,
                format_details_sig(&extras.detail_states),
                st.panel.rich_epoch,
            )
        } else if depth == 0 {
            let extras = st.row_extras.get(&row_index)?;
            let hit = extras.hit.as_ref()?;
            (
                hit.is_kanji,
                format_details_sig(&extras.detail_states),
                st.rich_epoch,
            )
        } else {
            let lvl = st.nested_level(depth)?;
            let extras = lvl.row_extras.get(&row_index)?;
            let hit = extras.hit.as_ref()?;
            (
                hit.is_kanji,
                format_details_sig(&extras.detail_states),
                lvl.rich_epoch,
            )
        };
        if is_kanji {
            return None;
        }
        let theme = st.effective_popup_theme();
        let cache_key = GlossaryDocCacheKey {
            row_index,
            width_px: bucket_w,
            selected,
            details_sig,
            rich_epoch: epoch,
            scale_quarter,
            css_theme_light: theme.is_light_theme(),
            compact_glossaries: theme.compact_glossaries,
        };
        if depth < 0 {
            if let Some(doc) = st.panel.glossary_cache.get(&cache_key) {
                st.panel.last_rendered_key.insert(row_index, cache_key);
                return Some(doc);
            }
        } else if depth == 0 {
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
        let (hit, parsed_glossary, detail_states) = if depth < 0 {
            let extras = st.panel.row_extras.get(&row_index)?;
            (
                extras.hit.as_ref()?.clone(),
                extras.parsed_glossary.clone(),
                extras.detail_states.clone(),
            )
        } else if depth == 0 {
            let extras = st.row_extras.get(&row_index)?;
            (
                extras.hit.as_ref()?.clone(),
                extras.parsed_glossary.clone(),
                extras.detail_states.clone(),
            )
        } else {
            let lvl = st.nested_level(depth)?;
            let extras = lvl.row_extras.get(&row_index)?;
            (
                extras.hit.as_ref()?.clone(),
                extras.parsed_glossary.clone(),
                extras.detail_states.clone(),
            )
        };
        let res_dirs = st.dict_resource_dirs.clone();
        let legacy_style = st.settings.glossary_style;
        let sheet = st
            .stylesheets
            .get_for_dictionary(&res_dirs, hit.dictionary_id, &hit.dict_name);
        let doc = Arc::new(render_structured_glossary_entry_with_value_scaled(
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
        )?);
        if depth < 0 {
            st.panel
                .last_rendered_key
                .insert(row_index, cache_key.clone());
            st.panel
                .glossary_cache
                .insert(cache_key, Arc::clone(&doc));
        } else if depth == 0 {
            st.last_rendered_key.insert(row_index, cache_key.clone());
            st.glossary_cache.insert(cache_key, Arc::clone(&doc));
        } else if let Some(lvl) = st.nested_level_mut(depth) {
            lvl.last_rendered_key.insert(row_index, cache_key.clone());
            lvl.glossary_cache.insert(cache_key, Arc::clone(&doc));
        }
        Some(doc)
    }

    pub(super) fn latest_cached_doc_at_depth(
        &self,
        depth: i32,
        row_index: i32,
    ) -> Option<Arc<RenderedGlossaryDoc>> {
        let st = self.state.borrow();
        let (epoch, last_key, cache) = if depth < 0 {
            (
                st.panel.rich_epoch,
                &st.panel.last_rendered_key,
                &st.panel.glossary_cache,
            )
        } else if depth == 0 {
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
            let (sel_opt, scan_opt, expanded, dict_name) = if depth < 0 {
                let dname = st
                    .panel
                    .row_extras
                    .get(&row_index)
                    .and_then(|e| e.hit.as_ref())
                    .map(|h| h.dict_name.as_str())
                    .unwrap_or("");
                let sel = st
                    .panel
                    .selections
                    .values()
                    .find(|s| s.row_index == row_index && s.anchor_char != s.focus_char);
                (
                    sel,
                    None,
                    st.panel.expanded_cards.contains(&row_index),
                    dname,
                )
            } else if depth == 0 {
                let dname = st
                    .row_extras
                    .get(&row_index)
                    .and_then(|e| e.hit.as_ref())
                    .map(|h| h.dict_name.as_str())
                    .unwrap_or("");
                let sel = st
                    .selections
                    .values()
                    .find(|s| s.row_index == row_index && s.anchor_char != s.focus_char);
                (
                    sel,
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
                let sel = lvl
                    .selections
                    .values()
                    .find(|s| s.row_index == row_index && s.anchor_char != s.focus_char);
                (
                    sel,
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
            rendered: doc.image.clone(),
            selection_rects: Rc::new(VecModel::from(sel_rects)).into(),
            scan_highlight_rects: Rc::new(VecModel::from(scan_rects)).into(),
            collapsible,
            collapsed_height: collapsed_h,
            total_lines,
            expanded,
        }
    }

    pub(super) fn char_index_at_point(doc: &RenderedGlossaryDoc, x: f32, y: f32) -> Option<usize> {
        let chars = doc.selectable_chars.as_slice();
        if chars.is_empty() {
            return None;
        }
        let mut best: Option<(usize, f32)> = None;
        let eval_range = |start: usize, end: usize, best: &mut Option<(usize, f32)>| {
            for (idx, cell) in chars[start..end].iter().enumerate() {
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
                let dy_score = dy * 8.0;
                if best.is_some_and(|(_, b_score)| dy_score >= b_score) {
                    continue;
                }
                let dx = if x < cell.x {
                    cell.x - x
                } else if x > cell.x + cell.w {
                    x - (cell.x + cell.w)
                } else {
                    0.0
                };
                let score = dy_score + dx;
                if best.is_none_or(|(_, b_score)| score < b_score) {
                    *best = Some((start + idx, score));
                }
            }
        };
        if !doc.line_bands.is_empty() {
            for band in &doc.line_bands {
                let band_dy = if y < band.min_y {
                    band.min_y - y
                } else if y > band.max_y {
                    y - band.max_y
                } else {
                    0.0
                };
                if best.is_some_and(|(_, b_score)| band_dy * 8.0 >= b_score) {
                    continue;
                }
                eval_range(band.start_idx, band.end_idx, &mut best);
            }
        } else {
            eval_range(0, chars.len(), &mut best);
        }
        best.map(|(idx, _)| idx)
    }

    pub(super) fn update_glossary_selection_text_at_depth(&self, app: &SubomiApp, depth: i32) {
        let ordered_selections: Vec<GlossarySelectionState> = {
            let st = self.state.borrow();
            if depth < 0 {
                st.panel.selections.values().cloned().collect()
            } else if depth == 0 {
                st.selections.values().cloned().collect()
            } else {
                st.nested_level(depth)
                    .map(|lvl| lvl.selections.values().cloned().collect())
                    .unwrap_or_default()
            }
        };
        let mut texts = Vec::new();
        for sel in ordered_selections {
            if sel.anchor_char == sel.focus_char {
                continue;
            }
            let Some(doc) = self.latest_cached_doc_at_depth(depth, sel.row_index) else {
                continue;
            };
            let lo = sel
                .anchor_char
                .min(sel.focus_char)
                .min(doc.selectable_chars.len());
            let hi = sel
                .anchor_char
                .max(sel.focus_char)
                .min(doc.selectable_chars.len());
            let trimmed = doc.selectable_chars[lo..hi]
                .iter()
                .map(|c| c.ch)
                .collect::<String>()
                .trim_matches(|c: char| {
                    matches!(c, '\u{2060}'..='\u{206a}' | '\u{fdd0}'..='\u{fdef}' | '\u{fffc}')
                })
                .trim()
                .to_owned();
            if !trimmed.is_empty() {
                texts.push(trimmed);
            }
        }
        crate::app::anki_actions::set_active_glossary_selections(
            self.instance_id,
            depth,
            texts.clone(),
        );
        let pb = app.global::<PopupBridge>();
        pb.set_selected_glossary_text(texts.join("\n").into());
        pb.set_selection_revision(pb.get_selection_revision().wrapping_add(1));
    }

    pub(super) fn clear_glossary_selections_at_depth(&self, app: &SubomiApp, depth: i32) {
        let had_any = {
            let mut st = self.state.borrow_mut();
            if depth < 0 {
                let had = !st.panel.selections.is_empty();
                st.panel.selections.clear();
                st.panel.active_drag_selection_key = None;
                had
            } else if depth == 0 {
                let had = !st.selections.is_empty();
                st.selections.clear();
                st.active_drag_selection_key = None;
                had
            } else if let Some(lvl) = st.nested_level_mut(depth) {
                let had = !lvl.selections.is_empty();
                lvl.selections.clear();
                lvl.active_drag_selection_key = None;
                had
            } else {
                false
            }
        };
        crate::app::anki_actions::clear_active_glossary_selections(self.instance_id, Some(depth));
        let pb = app.global::<PopupBridge>();
        if had_any || !pb.get_selected_glossary_text().is_empty() {
            pb.set_selected_glossary_text("".into());
            pb.set_selection_revision(pb.get_selection_revision().wrapping_add(1));
        }
    }

    pub(super) fn toggle_glossary_details_at_depth(
        &self,
        app: &SubomiApp,
        depth: i32,
        row_idx: i32,
        detail_id: &str,
    ) {
        {
            let mut st = self.state.borrow_mut();
            if depth < 0 {
                if let Some(extras) = st.panel.row_extras.get_mut(&row_idx) {
                    let current = extras.detail_states.get(detail_id).copied().unwrap_or(false);
                    extras.detail_states.insert(detail_id.to_owned(), !current);
                }
                st.panel.glossary_cache.retain(|k| k.row_index != row_idx);
                st.panel.last_rendered_key.remove(&row_idx);
            } else if depth == 0 {
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
        if depth < 0 {
            pb.set_panel_revision(pb.get_panel_revision().wrapping_add(1));
        } else if depth == 0 {
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
        let (extras_map, preferred_rows): (_, [Option<i32>; 2]) = if depth < 0 {
            let active_row = st
                .panel
                .active_drag_selection_key
                .and_then(|k| st.panel.selections.get(&k))
                .or_else(|| st.panel.selections.values().next_back())
                .map(|s| s.row_index);
            (&st.panel.row_extras, [active_row, None])
        } else if depth == 0 {
            let active_row = st
                .active_drag_selection_key
                .and_then(|k| st.selections.get(&k))
                .or_else(|| st.selections.values().next_back())
                .map(|s| s.row_index);
            (
                &st.row_extras,
                [
                    (st.last_active_glossary_row_idx > 0).then_some(st.last_active_glossary_row_idx),
                    active_row,
                ],
            )
        } else {
            let lvl = st.nested_level(depth)?;
            let active_row = lvl
                .active_drag_selection_key
                .and_then(|k| lvl.selections.get(&k))
                .or_else(|| lvl.selections.values().next_back())
                .map(|s| s.row_index);
            (&lvl.row_extras, [active_row, None])
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
        app: &SubomiApp,
        depth: i32,
        row_idx: Option<i32>,
        link: &str,
    ) -> bool {
        if let Some(rest) = link.strip_prefix("subomi://glossary-details?") {
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
        if let Some(rest) = link.strip_prefix("subomi://glossary-link?") {
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
                    if depth < 0 {
                        let pb = app.global::<PopupBridge>();
                        pb.set_panel_query(q_owned.clone().into());
                        pb.invoke_panel_search_requested(q_owned.into());
                    } else if depth == 0 {
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
        app: &SubomiApp,
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
            let root_key = (depth == 0)
                .then(|| row_term_def_key_model(&app.get_dict_results(), row_idx));
            let sel = GlossarySelectionState {
                row_index: row_idx,
                anchor_char: ci,
                focus_char: ci,
                dragging: false,
                press_pos: (x, y),
            };
            {
                let mut st = self.state.borrow_mut();
                if depth < 0 {
                    let key = row_term_def_key(&st.panel.rows, row_idx);
                    st.panel.selections.insert(key, sel);
                    st.panel.active_drag_selection_key = Some(key);
                } else if depth == 0 {
                    let key = root_key.unwrap_or((row_idx.max(0) as usize, 0));
                    st.selections.insert(key, sel);
                    st.active_drag_selection_key = Some(key);
                } else if let Some(lvl) = st.nested_level_mut(depth) {
                    let key = row_term_def_key(&lvl.rows, row_idx);
                    lvl.selections.insert(key, sel);
                    lvl.active_drag_selection_key = Some(key);
                }
            }
            self.update_glossary_selection_text_at_depth(app, depth);
        }
    }

    pub(super) fn handle_glossary_pointer_drag_at_depth(
        &self,
        app: &SubomiApp,
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
            let sel_opt = if depth < 0 {
                st.panel
                    .active_drag_selection_key
                    .and_then(|k| st.panel.selections.get_mut(&k))
            } else if depth == 0 {
                st.active_drag_selection_key
                    .and_then(|k| st.selections.get_mut(&k))
            } else if let Some(lvl) = st.nested_level_mut(depth) {
                lvl.active_drag_selection_key
                    .and_then(|k| lvl.selections.get_mut(&k))
            } else {
                None
            };
            if let Some(sel) = sel_opt {
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
        app: &SubomiApp,
        depth: i32,
        row_idx: i32,
        x: f32,
        y: f32,
    ) {
        let (was_dragging, removed_empty) = {
            let mut st_guard = self.state.borrow_mut();
            let st = &mut *st_guard;
            let (selections, active_key) = if depth < 0 {
                (
                    &mut st.panel.selections,
                    &mut st.panel.active_drag_selection_key,
                )
            } else if depth == 0 {
                (&mut st.selections, &mut st.active_drag_selection_key)
            } else if let Some(lvl) = st.nested_level_mut(depth) {
                (&mut lvl.selections, &mut lvl.active_drag_selection_key)
            } else {
                return;
            };
            let key = active_key.take();
            if let Some(k) = key {
                let dragged = selections
                    .get(&k)
                    .is_some_and(|s| s.dragging && s.anchor_char != s.focus_char);
                let mut removed = false;
                if let Some(s) = selections.get_mut(&k) {
                    s.dragging = false;
                    if s.anchor_char == s.focus_char {
                        selections.remove(&k);
                        removed = true;
                    }
                }
                (dragged, removed)
            } else {
                (false, false)
            }
        };
        if removed_empty {
            self.update_glossary_selection_text_at_depth(app, depth);
        }
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
        app: &SubomiApp,
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
        let root_key =
            (depth == 0).then(|| row_term_def_key_model(&app.get_dict_results(), row_idx));
        let sel = GlossarySelectionState {
            row_index: row_idx,
            anchor_char: lo,
            focus_char: hi,
            dragging: false,
            press_pos: (x, y),
        };
        {
            let mut st = self.state.borrow_mut();
            if depth < 0 {
                let key = row_term_def_key(&st.panel.rows, row_idx);
                st.panel.selections.insert(key, sel);
                st.panel.active_drag_selection_key = None;
            } else if depth == 0 {
                let key = root_key.unwrap_or((row_idx.max(0) as usize, 0));
                st.selections.insert(key, sel);
                st.active_drag_selection_key = None;
            } else if let Some(lvl) = st.nested_level_mut(depth) {
                let key = row_term_def_key(&lvl.rows, row_idx);
                lvl.selections.insert(key, sel);
                lvl.active_drag_selection_key = None;
            }
        }
        self.update_glossary_selection_text_at_depth(app, depth);
    }
}

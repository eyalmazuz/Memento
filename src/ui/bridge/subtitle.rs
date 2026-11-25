use slint::{ComponentHandle, Model as _};

use crate::controller::dict::{DictController, SearchHit};
use crate::ui::popup_model::active_rich_bridge_for_app;
use crate::ui::subtitle_layout::{is_japanese_lookup_char, subtitle_token_width_with_engine};
use crate::ui::text_engine::{
    subtitle_cluster_at_point_with_style, with_shared_text_engine, SubtitleRenderStyle,
};
use crate::ui::{SubomiApp, SubtitleOverlayBridge, SubtitleTokenMetrics, SubtitleWord};

use super::state::{HoveredSubtitleCluster, KeyboardScanCursor};
use super::RichUiBridge;

struct ScannableSubtitleChar {
    cluster: HoveredSubtitleCluster,
    byte_offset: usize,
}

fn collect_scannable_subtitle_chars(app: &SubomiApp) -> Vec<ScannableSubtitleChar> {
    let words = app.get_active_sub_words();
    let count = words.row_count();
    let mut out = Vec::new();
    let mut prefix_chars = 0usize;
    let mut prefix_utf16 = 0usize;

    for i in 0..count {
        let Some(word) = words.row_data(i) else {
            continue;
        };
        let text = word.text.as_str();
        if !word.query.is_empty() {
            let mut within_utf16 = 0usize;
            for (char_offset, (byte_offset, ch)) in text.char_indices().enumerate() {
                let ch_u16 = ch.len_utf16();
                if ch.is_alphanumeric() || is_japanese_lookup_char(ch) {
                    if let Some(sub_query) = word.query.as_str().get(byte_offset..) {
                        if !sub_query.is_empty() {
                            out.push(ScannableSubtitleChar {
                                cluster: HoveredSubtitleCluster {
                                    word: word.clone(),
                                    char_offset,
                                    global_char_index: prefix_chars + char_offset,
                                    global_utf16_offset: prefix_utf16 + within_utf16,
                                    sub_query: sub_query.to_owned(),
                                },
                                byte_offset,
                            });
                        }
                    }
                }
                within_utf16 += ch_u16;
            }
        }
        prefix_chars += text.chars().count();
        prefix_utf16 += text.encode_utf16().count();
    }
    out
}

fn compute_subtitle_char_popup_offset_x(
    app: &SubomiApp,
    word: &SubtitleWord,
    byte_offset: usize,
    style: &SubtitleRenderStyle,
) -> f32 {
    let font_px = (app.get_video_height() as f32 * app.get_subtitle_scale()).max(8.0);
    let show_furigana = app.get_furigana_visible();
    let lines = app.get_active_sub_lines();
    with_shared_text_engine(|engine| {
        for li in 0..lines.row_count() {
            let Some(line) = lines.row_data(li) else {
                continue;
            };
            let mut found = false;
            let mut total_w = 0.0f32;
            let mut token_x = 0.0f32;
            for wi in 0..line.words.row_count() {
                let Some(w) = line.words.row_data(wi) else {
                    continue;
                };
                let tw = subtitle_token_width_with_engine(engine, &w, font_px, show_furigana, style);
                if w.index == word.index {
                    token_x = total_w;
                    found = true;
                }
                total_w += tw;
            }
            if found {
                let shaped = engine.shape_and_render_subtitle_token(
                    word,
                    font_px,
                    show_furigana,
                    style,
                );
                let char_center_x = shaped
                    .clusters
                    .iter()
                    .find(|&&(b0, b1, _, _)| byte_offset >= b0 && byte_offset < b1)
                    .map(|&(_, _, cx, cadv)| shaped.glyph_x_px + cx + cadv * 0.5)
                    .unwrap_or(shaped.width_px * 0.5);
                return (token_x + char_center_x) - (total_w * 0.5);
            }
        }
        0.0
    })
}

impl RichUiBridge {
    pub(in crate::ui) fn clear_subtitle_scan_state(&self, app: &SubomiApp) {
        let ass_ctrl = {
            let mut st = self.state.borrow_mut();
            st.scan_timer.stop();
            st.hovered_cluster = None;
            st.hovered_ass_target = None;
            st.keyboard_scan_cursor = None;
            st.pending_ass_scan = None;
            st.active_scan_utf16_range = None;
            st.pending_scan_utf16_start = None;
            st.pending_scan_sub_query = None;
            st.subtitle_token_layout_cache.clear();
            st.ass_controller.clone()
        };
        if let Some(ass_ctrl) = ass_ctrl {
            if let Ok(mut ctrl) = ass_ctrl.try_borrow_mut() {
                ctrl.clear_lookup_highlight(app);
            }
        }
        let bridge = app.global::<SubtitleOverlayBridge>();
        bridge.set_hovered_token_index(-1);
        bridge.set_hovered_char_offset(-1);
        bridge.set_hovered_global_char_index(-1);
        bridge.set_hovered_query("".into());
        bridge.set_keyboard_scan_active(false);
        bridge.set_keyboard_scan_char_index(-1);
        bridge.set_highlight_revision(bridge.get_highlight_revision().wrapping_add(1));
        self.schedule_idle_memory_shrink_if_closed(app);
    }

    pub(super) fn resolve_subtitle_cluster(
        &self,
        app: &SubomiApp,
        word: &SubtitleWord,
        font_px: f32,
        show_furigana: bool,
        local_x: f32,
        local_y: f32,
    ) -> Option<HoveredSubtitleCluster> {
        let style = self.state.borrow().settings.subtitle_style.clone();
        let (byte_offset, char_offset) = subtitle_cluster_at_point_with_style(
            word,
            font_px,
            show_furigana,
            &style,
            local_x,
            local_y,
        )?;
        let sub_query = word.query.as_str().get(byte_offset..)?.to_owned();
        let first_ch = sub_query.chars().next()?;
        if !first_ch.is_alphanumeric() && !is_japanese_lookup_char(first_ch) {
            return None;
        }

        let words = app.get_active_sub_words();
        let token_idx = word.index.max(0) as usize;
        let (prefix_chars, prefix_utf16) = self
            .state
            .borrow_mut()
            .subtitle_token_layout_cache
            .token_offsets(&words, token_idx);
        let within_utf16 = word
            .text
            .as_str()
            .get(..byte_offset)
            .map_or(0, |s| s.encode_utf16().count());

        Some(HoveredSubtitleCluster {
            word: word.clone(),
            char_offset,
            global_char_index: prefix_chars + char_offset,
            global_utf16_offset: prefix_utf16 + within_utf16,
            sub_query,
        })
    }

    pub(super) fn render_subtitle_token(
        &self,
        word: SubtitleWord,
        font_px: f32,
        show_furigana: bool,
        token_selected: bool,
    ) -> SubtitleTokenMetrics {
        if word.text.is_empty() {
            return SubtitleTokenMetrics::default();
        }
        let app_opt = self.app_weak.upgrade();
        let app_snapshot = app_opt.as_ref().map(|app| {
            let lines = app.get_active_sub_lines();
            let dict_open = app.get_dict_open();
            let keyboard_scan_active = app
                .global::<SubtitleOverlayBridge>()
                .get_keyboard_scan_active();
            let scan_visible = dict_open || keyboard_scan_active;
            let selected_token_index = app.get_selected_token_index();
            let selected_token_count = app.get_selected_token_count();
            let words =
                (scan_visible && selected_token_index >= 0).then(|| app.get_active_sub_words());
            (
                lines,
                words,
                scan_visible,
                selected_token_index,
                selected_token_count,
            )
        });

        let (style, scan_range, (has_line_prev, has_line_next), token_u16_start) = {
            let mut st = self.state.borrow_mut();
            let style = st.settings.subtitle_style.clone();
            let scan_range = st.active_scan_utf16_range;
            let neighbors = if let Some((ref lines, _, _, _, _)) = app_snapshot {
                st.subtitle_token_layout_cache
                    .line_neighbors(lines, word.index)
            } else {
                (false, false)
            };
            let u16_start = if scan_range.is_some() {
                if let Some((_, Some(ref words), _, _, _)) = app_snapshot {
                    let tidx = word.index.max(0) as usize;
                    st.subtitle_token_layout_cache
                        .token_offsets(words, tidx)
                        .1
                } else {
                    0
                }
            } else {
                0
            };
            (style, scan_range, neighbors, u16_start)
        };

        let shaped = with_shared_text_engine(|engine| {
            engine.shape_and_render_subtitle_token(&word, font_px, show_furigana, &style)
        });

        let mut has_sel = false;
        let mut sel_x = shaped.glyph_x_px;
        let mut sel_w = shaped.glyph_width_px;
        let mut sel_join_left = false;
        let mut sel_join_right = false;

        if let (Some((hl_start, hl_end)), Some((_, _, dict_open, s_idx, s_cnt))) =
            (scan_range, app_snapshot.as_ref())
        {
            if *dict_open && *s_idx >= 0 {
                let token_u16_len = word.text.encode_utf16().count();
                let token_u16_end = token_u16_start + token_u16_len;
                let ov_start = hl_start.max(token_u16_start);
                let ov_end = hl_end.min(token_u16_end);
                if ov_start < ov_end {
                    has_sel = true;
                    sel_join_left = has_line_prev && hl_start < token_u16_start;
                    sel_join_right = has_line_next && hl_end > token_u16_end;
                    let local_u16_lo = ov_start - token_u16_start;
                    let local_u16_hi = ov_end - token_u16_start;
                    let mut x_min = f32::INFINITY;
                    let mut x_max = 0.0f32;
                    let mut cur_u16 = 0usize;
                    let mut char_byte_ranges = Vec::with_capacity(word.text.len());
                    for (bi, ch) in word.text.char_indices() {
                        let next_u16 = cur_u16 + ch.len_utf16();
                        char_byte_ranges.push((bi, bi + ch.len_utf8(), cur_u16, next_u16));
                        cur_u16 = next_u16;
                    }
                    for &(c_b0, c_b1, cx, cadv) in &shaped.clusters {
                        let lo_pos = char_byte_ranges.partition_point(|r| r.0 < c_b0);
                        let cl_u16_lo = char_byte_ranges.get(lo_pos).map_or(0, |r| r.2);
                        let hi_pos = char_byte_ranges.partition_point(|r| r.1 <= c_b1);
                        let cl_u16_hi = if hi_pos > 0 {
                            char_byte_ranges[hi_pos - 1].3
                        } else {
                            token_u16_len
                        };
                        if cl_u16_lo < local_u16_hi && cl_u16_hi > local_u16_lo {
                            x_min = x_min.min(cx);
                            x_max = x_max.max(cx + cadv);
                        }
                    }
                    if x_min.is_finite() && x_max > x_min {
                        sel_x = shaped.glyph_x_px + x_min;
                        sel_w = x_max - x_min;
                    }
                }
            } else if token_selected {
                has_sel = true;
                sel_join_left = has_line_prev && word.index > *s_idx;
                sel_join_right = has_line_next && word.index + 1 < *s_idx + *s_cnt;
            }
        } else if token_selected {
            has_sel = true;
            if let Some((_, _, _, s_idx, s_cnt)) = app_snapshot {
                sel_join_left = has_line_prev && word.index > s_idx;
                sel_join_right = has_line_next && word.index + 1 < s_idx + s_cnt;
            }
        }

        if has_sel {
            // Bridge inter-token padding and subpixel rounding so adjacent highlighted
            // characters form one continuous solid blue bar with zero vertical seams.
            let mut right = sel_x + sel_w;
            if sel_join_left {
                sel_x = 0.0;
            }
            if sel_join_right {
                right = shaped.width_px + 0.5;
            }
            sel_w = (right - sel_x).max(1.0);
        }

        SubtitleTokenMetrics {
            width_px: shaped.width_px,
            height_px: shaped.height_px,
            bleed_x_px: shaped.bleed_x_px,
            glyph_y_px: shaped.glyph_y_px,
            glyph_height_px: shaped.glyph_height_px,
            sel_x_px: sel_x,
            sel_width_px: sel_w,
            sel_join_left,
            sel_join_right,
            has_selection: has_sel,
            has_stroke: shaped.has_stroke,
            rendered_stroke: shaped.stroke_image,
            rendered: shaped.image,
        }
    }

    pub(super) fn trigger_subtitle_scan_now(
        &self,
        app: &SubomiApp,
        cluster: HoveredSubtitleCluster,
    ) {
        self.trigger_subtitle_scan_inner(app, cluster, false);
    }

    fn trigger_subtitle_scan_inner(
        &self,
        app: &SubomiApp,
        cluster: HoveredSubtitleCluster,
        from_keyboard: bool,
    ) {
        {
            let mut st = self.state.borrow_mut();
            st.scan_timer.stop();
            st.pending_ass_scan = None;
            st.pending_scan_utf16_start = Some(cluster.global_utf16_offset);
            st.pending_scan_sub_query = Some(cluster.sub_query.clone());
            st.keyboard_scan_cursor = from_keyboard.then_some(KeyboardScanCursor::Plain {
                global_char_index: cluster.global_char_index,
                global_utf16_offset: cluster.global_utf16_offset,
            });
        }
        let sb = app.global::<SubtitleOverlayBridge>();
        sb.set_keyboard_scan_active(from_keyboard);
        sb.set_keyboard_scan_char_index(if from_keyboard {
            cluster.global_char_index as i32
        } else {
            -1
        });
        app.set_popup_offset_x(app.get_pending_popup_offset_x());
        app.set_selected_token_index(cluster.word.index);
        app.invoke_lookup_word(cluster.sub_query.into());
    }

    pub(super) fn trigger_ass_scan_now(
        &self,
        app: &SubomiApp,
        target: crate::controller::ass::AssHoveredTarget,
    ) {
        self.trigger_ass_scan_inner(app, target, false);
    }

    fn trigger_ass_scan_inner(
        &self,
        app: &SubomiApp,
        target: crate::controller::ass::AssHoveredTarget,
        from_keyboard: bool,
    ) {
        let query = target.scan_substring.clone();
        if query.trim().is_empty() {
            return;
        }
        let global_char_index = target.global_char_index;
        let event_index = target.event_index;
        let char_index = target.char_index;
        {
            let mut st = self.state.borrow_mut();
            st.scan_timer.stop();
            st.pending_scan_utf16_start = None;
            st.pending_ass_scan = Some(target);
            st.pending_scan_sub_query = Some(query.clone());
            st.keyboard_scan_cursor = from_keyboard.then_some(KeyboardScanCursor::Ass {
                event_index,
                char_index,
            });
        }
        let sb = app.global::<SubtitleOverlayBridge>();
        sb.set_keyboard_scan_active(from_keyboard);
        sb.set_keyboard_scan_char_index(if from_keyboard {
            global_char_index as i32
        } else {
            -1
        });
        app.set_selected_token_index(0);
        app.invoke_lookup_word(query.into());
    }

    fn scan_ass_with_keyboard(&self, app: &SubomiApp, delta: Option<i32>) -> bool {
        let ass_ctrl = self.state.borrow().ass_controller.clone();
        let Some(ass_ctrl) = ass_ctrl else {
            return false;
        };
        let (targets, active_lookup) = {
            let Ok(ctrl) = ass_ctrl.try_borrow() else {
                return false;
            };
            (ctrl.scannable_targets(), ctrl.active_lookup().cloned())
        };
        if targets.is_empty() {
            return false;
        }

        let sb = app.global::<SubtitleOverlayBridge>();
        let already_kb = sb.get_keyboard_scan_active();
        let target_idx = match delta {
            None => 0,
            Some(step) => {
                let (cursor, hovered) = {
                    let st = self.state.borrow();
                    (
                        already_kb.then(|| st.keyboard_scan_cursor.clone()).flatten(),
                        st.hovered_ass_target.clone(),
                    )
                };
                let cur_pos = match cursor {
                    Some(KeyboardScanCursor::Ass {
                        event_index,
                        char_index,
                    }) => targets
                        .iter()
                        .position(|t| t.event_index == event_index && t.char_index == char_index),
                    _ => active_lookup
                        .as_ref()
                        .and_then(|lk| {
                            targets
                                .iter()
                                .position(|t| {
                                    t.event_index == lk.event_index
                                        && t.event_utf16_offset == lk.utf16_offset_in_event
                                })
                                .or_else(|| {
                                    targets.iter().position(|t| {
                                        t.event_index == lk.event_index
                                            && t.event_utf16_offset >= lk.utf16_offset_in_event
                                    })
                                })
                        })
                        .or_else(|| {
                            hovered.as_ref().and_then(|h| {
                                targets.iter().position(|t| {
                                    t.event_index == h.event_index && t.char_index == h.char_index
                                })
                            })
                        }),
                };
                match cur_pos {
                    Some(cur) => {
                        let next = if step > 0 {
                            (cur + 1).min(targets.len() - 1)
                        } else if step < 0 {
                            cur.saturating_sub(1)
                        } else {
                            cur
                        };
                        if next == cur && already_kb {
                            return true;
                        }
                        next
                    }
                    None => {
                        if step >= 0 {
                            0
                        } else {
                            targets.len() - 1
                        }
                    }
                }
            }
        };

        let target = targets[target_idx].clone();
        self.trigger_ass_scan_inner(app, target, true);
        true
    }

    fn scan_plain_with_keyboard(&self, app: &SubomiApp, delta: Option<i32>) -> bool {
        let items = collect_scannable_subtitle_chars(app);
        if items.is_empty() {
            return false;
        }

        let sb = app.global::<SubtitleOverlayBridge>();
        let already_kb = sb.get_keyboard_scan_active();
        let target_idx = match delta {
            None => 0,
            Some(step) => {
                let (cursor, active_range, hovered) = {
                    let st = self.state.borrow();
                    (
                        already_kb.then(|| st.keyboard_scan_cursor.clone()).flatten(),
                        st.active_scan_utf16_range,
                        st.hovered_cluster.clone(),
                    )
                };
                let cur_pos = match cursor {
                    Some(KeyboardScanCursor::Plain {
                        global_char_index,
                        global_utf16_offset,
                    }) => items
                        .iter()
                        .position(|item| item.cluster.global_char_index == global_char_index)
                        .or_else(|| {
                            items.iter().position(|item| {
                                item.cluster.global_utf16_offset >= global_utf16_offset
                            })
                        }),
                    _ => active_range
                        .and_then(|(u16_start, _)| {
                            items
                                .iter()
                                .position(|item| item.cluster.global_utf16_offset == u16_start)
                                .or_else(|| {
                                    items.iter().position(|item| {
                                        item.cluster.global_utf16_offset >= u16_start
                                    })
                                })
                        })
                        .or_else(|| {
                            hovered.as_ref().and_then(|h| {
                                items.iter().position(|item| {
                                    item.cluster.global_char_index == h.global_char_index
                                })
                            })
                        })
                        .or_else(|| {
                            let sel = app.get_selected_token_index();
                            (sel >= 0).then(|| {
                                items
                                    .iter()
                                    .position(|item| item.cluster.word.index == sel)
                            })?
                        }),
                };
                match cur_pos {
                    Some(cur) => {
                        let next = if step > 0 {
                            (cur + 1).min(items.len() - 1)
                        } else if step < 0 {
                            cur.saturating_sub(1)
                        } else {
                            cur
                        };
                        if next == cur && already_kb {
                            return true;
                        }
                        next
                    }
                    None => {
                        if step >= 0 {
                            0
                        } else {
                            items.len() - 1
                        }
                    }
                }
            }
        };

        let item = &items[target_idx];
        let style = self.state.borrow().settings.subtitle_style.clone();
        let offset_x =
            compute_subtitle_char_popup_offset_x(app, &item.cluster.word, item.byte_offset, &style);
        app.set_pending_popup_offset_x(offset_x);
        self.trigger_subtitle_scan_inner(app, item.cluster.clone(), true);
        true
    }
}

fn scan_plain_fallback_with_keyboard(app: &SubomiApp, delta: Option<i32>) -> bool {
    let items = collect_scannable_subtitle_chars(app);
    if items.is_empty() {
        return false;
    }

    let sb = app.global::<SubtitleOverlayBridge>();
    let already_kb = sb.get_keyboard_scan_active();
    let target_idx = match delta {
        None => 0,
        Some(step) => {
            let cur_pos = usize::try_from(sb.get_keyboard_scan_char_index())
                .ok()
                .and_then(|cur_char| {
                    items
                        .iter()
                        .position(|item| item.cluster.global_char_index == cur_char)
                })
                .or_else(|| {
                    let sel = app.get_selected_token_index();
                    (sel >= 0).then(|| {
                        items
                            .iter()
                            .position(|item| item.cluster.word.index == sel)
                    })?
                });
            match cur_pos {
                Some(cur) => {
                    let next = if step > 0 {
                        (cur + 1).min(items.len() - 1)
                    } else if step < 0 {
                        cur.saturating_sub(1)
                    } else {
                        cur
                    };
                    if next == cur && already_kb {
                        return true;
                    }
                    next
                }
                None => {
                    if step >= 0 {
                        0
                    } else {
                        items.len() - 1
                    }
                }
            }
        }
    };

    let item = &items[target_idx];
    sb.set_keyboard_scan_active(true);
    sb.set_keyboard_scan_char_index(item.cluster.global_char_index as i32);
    app.set_selected_token_index(item.cluster.word.index);
    app.set_selected_token_count(1);
    app.invoke_lookup_word(item.cluster.sub_query.clone().into());
    true
}

/// Start keyboard-driven dictionary term search at the first lookupable character
/// of the active subtitle (plain/rich or native ASS), matching the exact prefix
/// lookup behavior of hovering that character.
pub fn start_keyboard_subtitle_scan(app: &SubomiApp) -> bool {
    if !app.get_subtitles_visible() {
        return false;
    }
    if let Some(bridge) = active_rich_bridge_for_app(app) {
        if app.global::<SubtitleOverlayBridge>().get_ass_native_active() {
            return bridge.scan_ass_with_keyboard(app, None);
        }
        return bridge.scan_plain_with_keyboard(app, None);
    }
    scan_plain_fallback_with_keyboard(app, None)
}

/// Step keyboard-driven subtitle dictionary search forward (`delta > 0`) or
/// backward (`delta < 0`) by one lookupable character, preserving suffix lookup,
/// highlight spans, and popup anchoring across both plain/rich and native ASS tracks.
pub fn step_keyboard_subtitle_scan(app: &SubomiApp, delta: i32) -> bool {
    if !app.get_subtitles_visible() {
        return false;
    }
    if let Some(bridge) = active_rich_bridge_for_app(app) {
        if app.global::<SubtitleOverlayBridge>().get_ass_native_active() {
            return bridge.scan_ass_with_keyboard(app, Some(delta));
        }
        return bridge.scan_plain_with_keyboard(app, Some(delta));
    }
    scan_plain_fallback_with_keyboard(app, Some(delta))
}

fn matched_prefix_u16_by_norm(sub_q: &str, target: &str) -> Option<usize> {
    if target.is_empty() || sub_q.is_empty() {
        return None;
    }
    if sub_q.starts_with(target) {
        return Some(target.encode_utf16().count());
    }
    let target_norm = crate::controller::dict::normalize_reading(target);
    if target_norm.is_empty() {
        return None;
    }
    let mut acc = String::new();
    let mut u16_len = 0usize;
    for ch in sub_q.chars() {
        acc.push(ch);
        u16_len += ch.len_utf16();
        let norm = crate::controller::dict::normalize_reading(&acc);
        if norm == target_norm {
            return Some(u16_len);
        }
        if !target_norm.starts_with(&norm) && norm.len() >= target_norm.len() {
            break;
        }
    }
    None
}

fn prefix_match_u16_for_hit(trimmed: &str, first: &SearchHit) -> Option<usize> {
    let expr_m = matched_prefix_u16_by_norm(trimmed, &first.expression);
    let read_m = matched_prefix_u16_by_norm(trimmed, &first.reading);
    match (expr_m, read_m) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    }
}

/// Compute the exact UTF-16 highlight span length for a subtitle lookup against `pending_sub`.
/// Fast-paths non-deconjugated exact/normalized prefix matches against `first_hit.expression` and
/// `first_hit.reading` (including kana-reading matches like `いいわけ` -> `言い訳` = 4 UTF-16 units)
/// without re-running synchronous `dict_ctrl.search()` on the UI thread. Falls back to
/// `dict_ctrl.search(trimmed)` -> `last_match_units()` only when deconjugation, custom regex
/// text filters, or irregular surface forms require it.
pub(super) fn compute_subtitle_lookup_match_u16(
    dict_ctrl: &mut DictController,
    first_hit: Option<&SearchHit>,
    pending_sub: Option<&str>,
    has_text_filters: bool,
) -> usize {
    if let Some(sub_q) = pending_sub {
        let trimmed = sub_q.trim();
        if !trimmed.is_empty() {
            if !has_text_filters {
                if let Some(first) = first_hit {
                    if first.derivation.is_empty() {
                        if let Some(best) = prefix_match_u16_for_hit(trimmed, first) {
                            return best.max(1);
                        }
                    }
                }
            }
            let _ = dict_ctrl.search(trimmed);
            let units = dict_ctrl.last_match_units();
            if units > 0 {
                return units;
            }
            if let Some(first) = first_hit {
                if let Some(best) = prefix_match_u16_for_hit(trimmed, first) {
                    return best.max(1);
                }
            }
        }
    }
    first_hit
        .map(|h| h.expression.encode_utf16().count())
        .unwrap_or(1)
        .max(1)
}

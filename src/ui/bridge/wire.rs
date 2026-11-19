use std::cell::RefCell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Instant;

use slint::{ComponentHandle, Model as _, SharedString, VecModel};

use crate::controller::config::ConfigController;
use crate::controller::dict::DictController;
use crate::ui::dict_assets::collect_dictionary_resource_dirs;
use crate::ui::popup_css::{rgba_to_slint_color, StylesheetCache};
use crate::ui::popup_model::{
    is_kanji_char, register_active_rich_bridge, split_anki_tags_str, NEXT_BRIDGE_INSTANCE_ID,
};
use crate::ui::rich_db::RichDbHelper;
use crate::ui::subtitle_layout::set_active_subtitle_render_style;
use crate::ui::text_engine::{subtitle_cluster_at_point_with_style, with_shared_text_engine};
use crate::ui::{
    DefinitionGridRow, DictResult, MementoApp, PopupBridge, PopupDictStyle, PopupFreqStyle,
    RichHeadwordChar, RichHeadwordLine, SubtitleOverlayBridge,
};

use super::anki::{drain_anki_results_for_app, sync_anki_checking_for_app};
use super::nested::drain_nested_scan_results_for_app;
use super::state::{
    apply_popup_css_theme_to_bridge, copy_to_persistent_clipboard, current_external_lookup_epoch,
    modifier_matches_setting, push_history_capped, resolve_bare_modifier_event,
    utf16_units_to_char_count, AnkiPopupCheckRequest, AnkiPopupCheckResponse, CachedConfigSettings,
    DictSearchConfig, GlossaryDocLruCache, NestedScanRequest, NestedScanResponse,
    PanelSearchRequest, PanelSearchResponse, RichUiMutable, SubtitleTokenLayoutCache,
};
use super::{drain_panel_results_for_app, RichUiBridge};

fn spawn_panel_worker(
    dictionary_db_path: PathBuf,
    panel_rx: mpsc::Receiver<PanelSearchRequest>,
    panel_res_tx: mpsc::Sender<PanelSearchResponse>,
    worker_epoch: Arc<AtomicU64>,
    worker_app: slint::Weak<MementoApp>,
) {
    std::thread::spawn(move || {
        let mut sig = crate::controller::dict::db_file_fingerprint(&dictionary_db_path);
        let mut dict = DictController::open(&dictionary_db_path).unwrap_or_default();
        let mut applied_cfg: Option<DictSearchConfig> = None;
        while let Ok(mut req) = panel_rx.recv() {
            while let Ok(newer) = panel_rx.try_recv() {
                req = newer;
            }
            if req.gen_id != worker_epoch.load(Ordering::Acquire) {
                continue;
            }
            let trimmed = req.query.trim().to_owned();
            if trimmed.is_empty() {
                let _ = panel_res_tx.send(PanelSearchResponse {
                    gen_id: req.gen_id,
                    hits: Vec::new(),
                    excluded: req.excluded,
                });
                let _ = worker_app.upgrade_in_event_loop(move |app| {
                    let _ = drain_panel_results_for_app(&app);
                });
                continue;
            }
            let cur_sig = crate::controller::dict::db_file_fingerprint(&dictionary_db_path);
            if cur_sig != sig {
                sig = cur_sig;
                dict = DictController::open(&dictionary_db_path).unwrap_or_default();
                applied_cfg = None;
            }
            if applied_cfg.as_ref() != Some(&req.search_cfg) {
                req.search_cfg.apply_to(&mut dict);
                applied_cfg = Some(req.search_cfg);
            }
            let search_key = if req.kana_only {
                crate::controller::dict::normalize_reading(&trimmed)
            } else {
                trimmed.clone()
            };
            let mut hits = dict.search(&search_key).to_vec();
            if req.kana_only {
                let norm = crate::controller::dict::normalize_reading(&trimmed);
                hits.retain(|h| {
                    crate::controller::dict::normalize_reading(&h.reading).starts_with(&norm)
                        || crate::controller::dict::normalize_reading(&h.expression)
                            .starts_with(&norm)
                });
            }
            if req.gen_id != worker_epoch.load(Ordering::Acquire) {
                continue;
            }
            let _ = panel_res_tx.send(PanelSearchResponse {
                gen_id: req.gen_id,
                hits,
                excluded: req.excluded,
            });
            let _ = worker_app.upgrade_in_event_loop(move |app| {
                let _ = drain_panel_results_for_app(&app);
            });
        }
    });
}

fn spawn_nested_worker(
    dictionary_db_path: PathBuf,
    nested_rx: mpsc::Receiver<NestedScanRequest>,
    nested_res_tx: mpsc::Sender<NestedScanResponse>,
    worker_nested_epoch: Arc<AtomicU64>,
    worker_nested_app: slint::Weak<MementoApp>,
) {
    std::thread::spawn(move || {
        let mut sig = crate::controller::dict::db_file_fingerprint(&dictionary_db_path);
        let mut dict = DictController::open(&dictionary_db_path).unwrap_or_default();
        let mut applied_cfg: Option<DictSearchConfig> = None;
        while let Ok(mut req) = nested_rx.recv() {
            while let Ok(newer) = nested_rx.try_recv() {
                req = newer;
            }
            if req.epoch != worker_nested_epoch.load(Ordering::Acquire) {
                continue;
            }
            let cur_sig = crate::controller::dict::db_file_fingerprint(&dictionary_db_path);
            if cur_sig != sig {
                sig = cur_sig;
                dict = DictController::open(&dictionary_db_path).unwrap_or_default();
                applied_cfg = None;
            }
            if applied_cfg.as_ref() != Some(&req.search_config) {
                req.search_config.apply_to(&mut dict);
                applied_cfg = Some(req.search_config);
            }

            let mut hits = dict.search(&req.query).to_vec();
            let match_units = dict.last_match_units();
            let mut matched_char_count = utf16_units_to_char_count(&req.query, match_units);
            if hits.is_empty() {
                if let Some(first_ch) = req.query.chars().next() {
                    if is_kanji_char(first_ch) {
                        let ch_str = first_ch.to_string();
                        let khits = dict.search_kanji(&ch_str).to_vec();
                        if !khits.is_empty() {
                            hits = khits;
                            matched_char_count = 1;
                        }
                    }
                }
            } else if matched_char_count == 0 {
                matched_char_count = hits
                    .first()
                    .map(|h| h.expression.chars().count())
                    .unwrap_or(1)
                    .max(1);
            }

            if req.epoch != worker_nested_epoch.load(Ordering::Acquire) {
                continue;
            }

            let headword = hits
                .first()
                .map(|h| h.expression.clone())
                .unwrap_or_default();
            let reading = hits.first().map(|h| h.reading.clone()).unwrap_or_default();
            let mut term_meta_rows = Vec::new();
            let mut term_pitch_details = HashMap::new();
            for h in &hits {
                if !h.is_kanji
                    && !term_meta_rows
                        .iter()
                        .any(|(e, r, _, _): &(String, String, _, _)| {
                            e == &h.expression && r == &h.reading
                        })
                {
                    let graphs = dict.get_pitch_graphs(&h.expression, &h.reading);
                    let freqs = dict.get_frequencies(&h.expression, &h.reading);
                    let details = dict.get_pitch_details(&h.expression, &h.reading);
                    term_meta_rows.push((h.expression.clone(), h.reading.clone(), graphs, freqs));
                    term_pitch_details.insert((h.expression.clone(), h.reading.clone()), details);
                }
            }

            if req.epoch != worker_nested_epoch.load(Ordering::Acquire) {
                continue;
            }

            let _ = nested_res_tx.send(NestedScanResponse {
                epoch: req.epoch,
                parent_depth: req.parent_depth,
                row_index: req.row_index,
                start_char: req.start_char,
                matched_char_count,
                cell_indices: req.cell_indices,
                anchor_x: req.anchor_x,
                anchor_y: req.anchor_y,
                locked: req.locked,
                hits,
                excluded_glossaries: req.excluded_glossaries,
                headword,
                reading,
                term_meta_rows,
                term_pitch_details,
            });
            let _ = worker_nested_app.upgrade_in_event_loop(move |app| {
                let _ = drain_nested_scan_results_for_app(&app);
            });
        }
    });
}

#[allow(clippy::too_many_arguments)]
fn spawn_anki_worker(
    anki_rx: mpsc::Receiver<AnkiPopupCheckRequest>,
    anki_res_tx: mpsc::Sender<AnkiPopupCheckResponse>,
    worker_anki_epoch: Arc<AtomicU64>,
    worker_nested_anki_epoch: Arc<AtomicU64>,
    root_in_flight: Arc<AtomicU64>,
    nested_in_flight: Arc<AtomicU64>,
    worker_anki_ext: Arc<Mutex<Option<Arc<AtomicU64>>>>,
    worker_anki_app: slint::Weak<MementoApp>,
) {
    std::thread::spawn(move || {
        while let Ok(first_req) = anki_rx.recv() {
            let mut pending_root: Option<AnkiPopupCheckRequest> = None;
            let mut pending_nested: Option<AnkiPopupCheckRequest> = None;
            if first_req.target_nested_depth.is_some() {
                pending_nested = Some(first_req);
            } else {
                pending_root = Some(first_req);
            }
            while let Ok(newer) = anki_rx.try_recv() {
                if newer.target_nested_depth.is_some() {
                    pending_nested = Some(newer);
                } else {
                    pending_root = Some(newer);
                }
            }
            for req in pending_root.into_iter().chain(pending_nested) {
                let (target_epoch, target_in_flight) = if req.target_nested_depth.is_some() {
                    (&worker_nested_anki_epoch, &nested_in_flight)
                } else {
                    (&worker_anki_epoch, &root_in_flight)
                };
                let is_cancelled = || {
                    req.nav_epoch != target_epoch.load(Ordering::Acquire)
                        || req.lookup_epoch != current_external_lookup_epoch(&worker_anki_ext)
                };
                let clear_if_cancelled = || -> bool {
                    if !is_cancelled() {
                        return false;
                    }
                    if target_in_flight
                        .compare_exchange(req.nav_epoch, 0, Ordering::AcqRel, Ordering::Acquire)
                        .is_ok()
                    {
                        let _ = worker_anki_app.upgrade_in_event_loop(move |app| {
                            sync_anki_checking_for_app(&app);
                        });
                    }
                    true
                };

                if clear_if_cancelled() {
                    continue;
                }

                let mut term_statuses = HashMap::new();
                if let Some(mut profile) = req.term_profile.clone() {
                    if profile.endpoint.timeout.is_none() {
                        profile.endpoint.timeout = Some(std::time::Duration::from_secs(3));
                    }
                    let ctrl = crate::controller::anki::AnkiController::new(profile.endpoint());
                    if let Ok(statuses) =
                        ctrl.check_entries_anki_status(&profile, &req.term_entries, req.show_tags)
                    {
                        for (pair, st) in req.term_entries.iter().cloned().zip(statuses) {
                            term_statuses.insert(pair, st);
                        }
                    }
                }

                if clear_if_cancelled() {
                    continue;
                }

                let mut kanji_statuses = HashMap::new();
                if let Some(mut profile) = req.kanji_profile.clone() {
                    if profile.endpoint.timeout.is_none() {
                        profile.endpoint.timeout = Some(std::time::Duration::from_secs(3));
                    }
                    let ctrl = crate::controller::anki::AnkiController::new(profile.endpoint());
                    if let Ok(statuses) =
                        ctrl.check_entries_anki_status(&profile, &req.kanji_entries, req.show_tags)
                    {
                        for ((expr, _), st) in req.kanji_entries.iter().cloned().zip(statuses) {
                            kanji_statuses.insert(expr, st);
                        }
                    }
                }

                if clear_if_cancelled() {
                    continue;
                }

                let _ = anki_res_tx.send(AnkiPopupCheckResponse {
                    nav_epoch: req.nav_epoch,
                    lookup_epoch: req.lookup_epoch,
                    target_nested_depth: req.target_nested_depth,
                    show_tags: req.show_tags,
                    term_statuses,
                    kanji_statuses,
                });
                let _ = worker_anki_app.upgrade_in_event_loop(move |app| {
                    let _ = drain_anki_results_for_app(&app);
                });
            }
        }
    });
}

fn wire_popup_bridge(app: &MementoApp, bridge: &Rc<RichUiBridge>) {
    let pb = app.global::<PopupBridge>();
    pb.on_split_anki_tags(|raw| Rc::new(VecModel::from(split_anki_tags_str(raw.as_str()))).into());

    pb.on_headword_chars(|expr| {
        let chars: Vec<RichHeadwordChar> = expr
            .chars()
            .enumerate()
            .map(|(i, ch)| RichHeadwordChar {
                text: ch.to_string().into(),
                is_kanji: is_kanji_char(ch),
                char_index: i as i32,
            })
            .collect();
        Rc::new(VecModel::from(chars)).into()
    });

    let b_hw_lines = bridge.clone();
    pb.on_headword_lines(move |expr, avail_w| {
        let hw_scale = (b_hw_lines.state.borrow().effective_popup_theme().headword_size / 23.0)
            .clamp(0.5, 2.5);
        let max_w = avail_w.max(96.0);
        let mut lines: Vec<RichHeadwordLine> = Vec::new();
        let mut cur_line: Vec<RichHeadwordChar> = Vec::new();
        let mut cur_w = 0.0f32;
        for (i, ch) in expr.chars().enumerate() {
            let cw = if ch.is_ascii() { 14.0 } else { 24.0 } * hw_scale;
            if !cur_line.is_empty() && cur_w + cw > max_w {
                lines.push(RichHeadwordLine {
                    chars: Rc::new(VecModel::from(std::mem::take(&mut cur_line))).into(),
                });
                cur_w = 0.0;
            }
            cur_w += cw;
            cur_line.push(RichHeadwordChar {
                text: ch.to_string().into(),
                is_kanji: is_kanji_char(ch),
                char_index: i as i32,
            });
        }
        if !cur_line.is_empty() {
            lines.push(RichHeadwordLine {
                chars: Rc::new(VecModel::from(cur_line)).into(),
            });
        }
        Rc::new(VecModel::from(lines)).into()
    });

    let b_hw_pitch = bridge.clone();
    pb.on_headword_pitch_color(move |row_idx, _rev| {
        let st = b_hw_pitch.state.borrow();
        let cat = st.row_extras.get(&row_idx).and_then(|e| e.pitch_category);
        rgba_to_slint_color(st.effective_popup_theme().resolve_headword_pitch_color(cat))
    });

    let b_dict_style = bridge.clone();
    pb.on_dict_entry_style(move |dict_name, _is_solo, _rev| {
        let st = b_dict_style.state.borrow();
        let resolved = st.effective_popup_theme().resolve_dict_style(dict_name.as_str());
        PopupDictStyle {
            badge_bg: rgba_to_slint_color(resolved.badge_bg),
            badge_color: rgba_to_slint_color(resolved.badge_color),
            card_bg: rgba_to_slint_color(resolved.card_bg),
            card_border: rgba_to_slint_color(resolved.card_border),
            card_border_width: resolved.card_border_width,
            card_radius: resolved.card_radius,
            card_padding: resolved.card_padding,
            glossary_opacity: resolved.glossary_opacity,
            glossary_hover_opacity: resolved.glossary_hover_opacity,
            glossary_solo_opacity: resolved.glossary_solo_opacity,
        }
    });

    let b_freq_style = bridge.clone();
    pb.on_freq_badge_style(move |freq_dict, _rev| {
        let st = b_freq_style.state.borrow();
        let (bg, color) = st.effective_popup_theme().resolve_freq_style(freq_dict.as_str());
        PopupFreqStyle {
            bg: rgba_to_slint_color(bg),
            color: rgba_to_slint_color(color),
        }
    });

    let b_pitch_dict_bg = bridge.clone();
    pb.on_pitch_dict_bg(move |pitch_dict, _rev| {
        let st = b_pitch_dict_bg.state.borrow();
        rgba_to_slint_color(
            st.effective_popup_theme()
                .resolve_pitch_dict_bg(pitch_dict.as_str()),
        )
    });

    pb.on_definition_grid_rows(|all_rows, term_start_index| {
        let total = all_rows.row_count();
        let mut group = Vec::new();
        let mut started = false;
        for i in 0..total {
            let Some(row) = all_rows.row_data(i) else {
                continue;
            };
            if !started {
                if row.index == term_start_index {
                    started = true;
                    group.push(row);
                }
            } else if row.term_start {
                break;
            } else {
                group.push(row);
            }
        }
        let is_solo = group.len() == 1;
        let mut pairs = Vec::with_capacity(group.len().div_ceil(2));
        let mut iter = group.into_iter();
        while let Some(left) = iter.next() {
            if let Some(right) = iter.next() {
                pairs.push(DefinitionGridRow {
                    left,
                    right,
                    has_right: true,
                    is_solo: false,
                });
            } else {
                pairs.push(DefinitionGridRow {
                    left,
                    right: DictResult::default(),
                    has_right: false,
                    is_solo,
                });
            }
        }
        Rc::new(VecModel::from(pairs)).into()
    });

    let b_deriv_summary = bridge.clone();
    pb.on_derivation_summary(move |row_idx, _rev| {
        let st = b_deriv_summary.state.borrow();
        let Some(extras) = st.row_extras.get(&row_idx) else {
            return SharedString::default();
        };
        if extras.derivation_steps.is_empty() {
            return SharedString::default();
        }
        let joined: Vec<String> = extras
            .derivation_steps
            .iter()
            .map(|s| {
                if s.description.is_empty() {
                    s.label.to_string()
                } else {
                    format!("{} ({})", s.label, s.description)
                }
            })
            .collect();
        format!("« {}", joined.join(" ← ")).into()
    });

    let b_term_pills = bridge.clone();
    pb.on_term_tag_pills(move |row_idx, _rev| {
        let st = b_term_pills.state.borrow();
        let pills = st
            .row_extras
            .get(&row_idx)
            .map(|e| e.term_pills.clone())
            .unwrap_or_default();
        Rc::new(VecModel::from(pills)).into()
    });

    let b_def_pills = bridge.clone();
    pb.on_def_tag_pills(move |row_idx, _rev| {
        let st = b_def_pills.state.borrow();
        let pills = st
            .row_extras
            .get(&row_idx)
            .map(|e| e.def_pills.clone())
            .unwrap_or_default();
        Rc::new(VecModel::from(pills)).into()
    });

    let b_deriv = bridge.clone();
    pb.on_derivation_steps(move |row_idx, _rev| {
        let st = b_deriv.state.borrow();
        let steps = st
            .row_extras
            .get(&row_idx)
            .map(|e| e.derivation_steps.clone())
            .unwrap_or_default();
        Rc::new(VecModel::from(steps)).into()
    });

    let b_pitches = bridge.clone();
    pb.on_rich_pitch_graphs(move |row_idx, _rev| {
        let st = b_pitches.state.borrow();
        let graphs = st
            .row_extras
            .get(&row_idx)
            .map(|e| e.pitch_graphs.clone())
            .unwrap_or_default();
        Rc::new(VecModel::from(graphs)).into()
    });

    pb.on_devoice_circle_image(|| with_shared_text_engine(|e| e.devoice_circle_image()));

    let b_kanji_detail = bridge.clone();
    pb.on_kanji_detail(move |row_idx, _rev| {
        let st = b_kanji_detail.state.borrow();
        st.row_extras
            .get(&row_idx)
            .and_then(|e| e.kanji_detail.clone())
            .unwrap_or_default()
    });

    let b_render_gloss = bridge.clone();
    pb.on_render_glossary(move |row_idx, avail_w, selected, _rev, _sel_rev| {
        b_render_gloss.build_rich_glossary_render(0, row_idx, avail_w, selected)
    });

    let b_cursor = bridge.clone();
    pb.on_glossary_cursor_is_pointer(move |row_idx, x, y| {
        let Some(doc) = b_cursor.latest_cached_doc_at_depth(0, row_idx) else {
            return false;
        };
        doc.hit_zones.iter().any(|z| {
            z.action.is_some() && x >= z.x && x <= z.x + z.w && y >= z.y && y <= z.y + z.h
        })
    });

    // Nested child popup rich query callbacks (depth >= 1)
    let b_nhw_pitch = bridge.clone();
    pb.on_nested_headword_pitch_color(move |depth, row_idx, _rev| {
        let st = b_nhw_pitch.state.borrow();
        let cat = st
            .nested_level(depth)
            .and_then(|lvl| lvl.row_extras.get(&row_idx))
            .and_then(|e| e.pitch_category);
        rgba_to_slint_color(st.effective_popup_theme().resolve_headword_pitch_color(cat))
    });

    let b_nterm_pills = bridge.clone();
    pb.on_nested_term_tag_pills(move |depth, row_idx, _rev| {
        let st = b_nterm_pills.state.borrow();
        let pills = st
            .nested_level(depth)
            .and_then(|lvl| lvl.row_extras.get(&row_idx))
            .map(|e| e.term_pills.clone())
            .unwrap_or_default();
        Rc::new(VecModel::from(pills)).into()
    });

    let b_ndef_pills = bridge.clone();
    pb.on_nested_def_tag_pills(move |depth, row_idx, _rev| {
        let st = b_ndef_pills.state.borrow();
        let pills = st
            .nested_level(depth)
            .and_then(|lvl| lvl.row_extras.get(&row_idx))
            .map(|e| e.def_pills.clone())
            .unwrap_or_default();
        Rc::new(VecModel::from(pills)).into()
    });

    let b_nderiv = bridge.clone();
    pb.on_nested_derivation_steps(move |depth, row_idx, _rev| {
        let st = b_nderiv.state.borrow();
        let steps = st
            .nested_level(depth)
            .and_then(|lvl| lvl.row_extras.get(&row_idx))
            .map(|e| e.derivation_steps.clone())
            .unwrap_or_default();
        Rc::new(VecModel::from(steps)).into()
    });

    let b_nderiv_sum = bridge.clone();
    pb.on_nested_derivation_summary(move |depth, row_idx, _rev| {
        let st = b_nderiv_sum.state.borrow();
        let Some(extras) = st
            .nested_level(depth)
            .and_then(|lvl| lvl.row_extras.get(&row_idx))
        else {
            return SharedString::default();
        };
        if extras.derivation_steps.is_empty() {
            return SharedString::default();
        }
        let joined: Vec<String> = extras
            .derivation_steps
            .iter()
            .map(|s| {
                if s.description.is_empty() {
                    s.label.to_string()
                } else {
                    format!("{} ({})", s.label, s.description)
                }
            })
            .collect();
        format!("« {}", joined.join(" ← ")).into()
    });

    let b_npitches = bridge.clone();
    pb.on_nested_rich_pitch_graphs(move |depth, row_idx, _rev| {
        let st = b_npitches.state.borrow();
        let graphs = st
            .nested_level(depth)
            .and_then(|lvl| lvl.row_extras.get(&row_idx))
            .map(|e| e.pitch_graphs.clone())
            .unwrap_or_default();
        Rc::new(VecModel::from(graphs)).into()
    });

    let b_nkanji = bridge.clone();
    pb.on_nested_kanji_detail(move |depth, row_idx, _rev| {
        let st = b_nkanji.state.borrow();
        st.nested_level(depth)
            .and_then(|lvl| lvl.row_extras.get(&row_idx))
            .and_then(|e| e.kanji_detail.clone())
            .unwrap_or_default()
    });

    let b_nrender = bridge.clone();
    pb.on_nested_render_glossary(move |depth, row_idx, avail_w, selected, _rev, _sel_rev| {
        b_nrender.build_rich_glossary_render(depth, row_idx, avail_w, selected)
    });

    let b_ncursor = bridge.clone();
    pb.on_nested_glossary_cursor_is_pointer(move |depth, row_idx, x, y| {
        let Some(doc) = b_ncursor.latest_cached_doc_at_depth(depth, row_idx) else {
            return false;
        };
        doc.hit_zones.iter().any(|z| {
            z.action.is_some() && x >= z.x && x <= z.x + z.w && y >= z.y && y <= z.y + z.h
        })
    });

    let b_nav_back = bridge.clone();
    pb.on_nav_back(move || {
        let Some(app) = b_nav_back.app_weak.upgrade() else {
            return;
        };
        let current = b_nav_back.snapshot_current_popup(&app);
        let prev = {
            let mut st = b_nav_back.state.borrow_mut();
            let Some(prev) = st.back_stack.pop_back() else {
                return;
            };
            if let Some(cur) = current {
                push_history_capped(&mut st.forward_stack, cur);
            }
            prev
        };
        b_nav_back.restore_history_entry(&app, prev);
    });

    let b_nav_fwd = bridge.clone();
    pb.on_nav_forward(move || {
        let Some(app) = b_nav_fwd.app_weak.upgrade() else {
            return;
        };
        let current = b_nav_fwd.snapshot_current_popup(&app);
        let next = {
            let mut st = b_nav_fwd.state.borrow_mut();
            let Some(next) = st.forward_stack.pop_back() else {
                return;
            };
            if let Some(cur) = current {
                push_history_capped(&mut st.back_stack, cur);
            }
            next
        };
        b_nav_fwd.restore_history_entry(&app, next);
    });

    let b_kanji_click = bridge.clone();
    pb.on_lookup_kanji_char(move |expr, ch_str| {
        let Some(app) = b_kanji_click.app_weak.upgrade() else {
            return;
        };
        b_kanji_click.navigate_to_kanji(&app, expr.as_str(), ch_str.as_str());
    });

    let b_kana_toggle = bridge.clone();
    pb.on_toggle_kana_reading_search(move |_expr, reading| {
        let Some(app) = b_kana_toggle.app_weak.upgrade() else {
            return;
        };
        let pb = app.global::<PopupBridge>();
        if pb.get_kana_reading_active()
            && pb.get_active_kana_reading().as_str() == reading.as_str()
            && pb.get_can_go_back()
        {
            pb.invoke_nav_back();
            return;
        }
        b_kana_toggle.navigate_to_term_query(&app, reading.as_str(), Some(reading.as_str()));
    });

    let b_act_link = bridge.clone();
    pb.on_activate_glossary_link(move |href| {
        let Some(app) = b_act_link.app_weak.upgrade() else {
            return false;
        };
        b_act_link.handle_internal_link_str_at_depth(&app, 0, None, href.as_str())
    });

    let b_toggle_details = bridge.clone();
    pb.on_toggle_glossary_details(move |row_idx, detail_id| {
        let Some(app) = b_toggle_details.app_weak.upgrade() else {
            return;
        };
        b_toggle_details.toggle_glossary_details_at_depth(&app, 0, row_idx, detail_id.as_str());
    });

    let b_toggle_exp = bridge.clone();
    pb.on_toggle_definition_expanded(move |depth, row_idx| {
        let Some(app) = b_toggle_exp.app_weak.upgrade() else {
            return;
        };
        {
            let mut st = b_toggle_exp.state.borrow_mut();
            if depth == 0 {
                if !st.root_expanded_cards.remove(&row_idx) {
                    st.root_expanded_cards.insert(row_idx);
                }
            } else if let Some(lvl) = st.nested_level_mut(depth) {
                lvl.locked = true;
                if !lvl.expanded_cards.remove(&row_idx) {
                    lvl.expanded_cards.insert(row_idx);
                }
            }
        }
        let pb = app.global::<PopupBridge>();
        if depth == 0 {
            pb.set_rich_revision(pb.get_rich_revision().wrapping_add(1));
        } else {
            pb.set_nested_locked(true);
            pb.set_nested_revision(pb.get_nested_revision().wrapping_add(1));
        }
    });

    let b_tip_show = bridge.clone();
    pb.on_show_tooltip(move |text, x, y| {
        let Some(app) = b_tip_show.app_weak.upgrade() else {
            return;
        };
        let pb = app.global::<PopupBridge>();
        pb.set_tooltip_text(text);
        pb.set_tooltip_x(x);
        pb.set_tooltip_y(y);
        pb.set_tooltip_visible(true);
    });

    let b_tip_hide = bridge.clone();
    pb.on_hide_tooltip(move || {
        if let Some(app) = b_tip_hide.app_weak.upgrade() {
            app.global::<PopupBridge>().set_tooltip_visible(false);
        }
    });

    let b_g_down = bridge.clone();
    pb.on_glossary_pointer_down(move |row_idx, x, y| {
        if let Some(app) = b_g_down.app_weak.upgrade() {
            b_g_down.handle_glossary_pointer_down_at_depth(&app, 0, row_idx, x, y);
        }
    });

    let b_g_drag = bridge.clone();
    pb.on_glossary_pointer_drag(move |row_idx, x, y| {
        if let Some(app) = b_g_drag.app_weak.upgrade() {
            b_g_drag.handle_glossary_pointer_drag_at_depth(&app, 0, row_idx, x, y);
        }
    });

    let b_g_up = bridge.clone();
    pb.on_glossary_pointer_up(move |row_idx, x, y| {
        if let Some(app) = b_g_up.app_weak.upgrade() {
            b_g_up.handle_glossary_pointer_up_at_depth(&app, 0, row_idx, x, y);
        }
    });

    let b_g_hover = bridge.clone();
    pb.on_glossary_pointer_hover(move |row_idx, x, y, popup_x, popup_y| {
        let Some(app) = b_g_hover.app_weak.upgrade() else {
            return;
        };
        if row_idx > 0 {
            b_g_hover.state.borrow_mut().last_active_glossary_row_idx = row_idx;
        }
        let pb = app.global::<PopupBridge>();
        if let Some(doc) = b_g_hover.latest_cached_doc_at_depth(0, row_idx) {
            if let Some(zone) = doc.hit_zones.iter().rev().find(|z| {
                z.tooltip.is_some() && x >= z.x && x <= z.x + z.w && y >= z.y && y <= z.y + z.h
            }) {
                if let Some(ref tip) = zone.tooltip {
                    pb.set_tooltip_text(tip.clone().into());
                    pb.set_tooltip_x(popup_x);
                    pb.set_tooltip_y(popup_y);
                    pb.set_tooltip_visible(true);
                    return;
                }
            }
        }
        pb.set_tooltip_visible(false);
    });

    let b_g_dbl = bridge.clone();
    pb.on_glossary_double_clicked(move |row_idx, x, y| {
        if let Some(app) = b_g_dbl.app_weak.upgrade() {
            b_g_dbl.handle_glossary_double_click_at_depth(&app, 0, row_idx, x, y);
        }
    });

    // Recursive hover-scan & nested popup interactive callbacks
    let b_scan_hover = bridge.clone();
    pb.on_glossary_scan_hover(
        move |depth, row_idx, x, y, popup_x, popup_y, control, shift, alt, meta| {
            if let Some(app) = b_scan_hover.app_weak.upgrade() {
                b_scan_hover.handle_glossary_scan_hover(
                    &app, depth, row_idx, x, y, popup_x, popup_y, control, shift, alt, meta,
                );
            }
        },
    );

    let b_scan_mid = bridge.clone();
    pb.on_glossary_scan_middle_click(move |depth, row_idx, x, y, popup_x, popup_y| {
        if let Some(app) = b_scan_mid.app_weak.upgrade() {
            b_scan_mid.handle_glossary_scan_middle_click(
                &app, depth, row_idx, x, y, popup_x, popup_y,
            );
        }
    });

    let b_scan_leave = bridge.clone();
    pb.on_glossary_scan_leave(move |depth, row_idx| {
        b_scan_leave.handle_glossary_scan_leave(depth, row_idx);
    });

    let b_ng_down = bridge.clone();
    pb.on_nested_glossary_pointer_down(move |depth, row_idx, x, y| {
        if let Some(app) = b_ng_down.app_weak.upgrade() {
            b_ng_down.handle_glossary_pointer_down_at_depth(&app, depth, row_idx, x, y);
        }
    });

    let b_ng_drag = bridge.clone();
    pb.on_nested_glossary_pointer_drag(move |depth, row_idx, x, y| {
        if let Some(app) = b_ng_drag.app_weak.upgrade() {
            b_ng_drag.handle_glossary_pointer_drag_at_depth(&app, depth, row_idx, x, y);
        }
    });

    let b_ng_up = bridge.clone();
    pb.on_nested_glossary_pointer_up(move |depth, row_idx, x, y| {
        if let Some(app) = b_ng_up.app_weak.upgrade() {
            b_ng_up.handle_glossary_pointer_up_at_depth(&app, depth, row_idx, x, y);
        }
    });

    let b_ng_dbl = bridge.clone();
    pb.on_nested_glossary_double_clicked(move |depth, row_idx, x, y| {
        if let Some(app) = b_ng_dbl.app_weak.upgrade() {
            b_ng_dbl.handle_glossary_double_click_at_depth(&app, depth, row_idx, x, y);
        }
    });

    let b_nkanji_click = bridge.clone();
    pb.on_nested_lookup_kanji_char(move |depth, expr, ch_str| {
        if let Some(app) = b_nkanji_click.app_weak.upgrade() {
            b_nkanji_click.navigate_nested_kanji(&app, depth, expr.as_str(), ch_str.as_str());
        }
    });

    let b_nsel = bridge.clone();
    pb.on_nested_toggle_select_definition(move |depth, idx| {
        if let Some(app) = b_nsel.app_weak.upgrade() {
            b_nsel.toggle_nested_select_definition(&app, depth, idx);
        }
    });

    let b_ncoll = bridge.clone();
    pb.on_nested_toggle_term_collapse(move |depth, term_start_idx| {
        if let Some(app) = b_ncoll.app_weak.upgrade() {
            b_ncoll.toggle_nested_term_collapse(&app, depth, term_start_idx);
        }
    });

    let b_nadd = bridge.clone();
    pb.on_nested_add_anki_note(move |row_zero_idx| {
        if let Some(app) = b_nadd.app_weak.upgrade() {
            b_nadd.add_nested_anki_note(&app, row_zero_idx);
        }
    });

    let b_nhover = bridge.clone();
    pb.on_nested_popup_hover_changed(move |hovered| {
        {
            let mut st = b_nhover.state.borrow_mut();
            st.nested_card_hovered = hovered;
            if hovered {
                st.nested_dismiss_timer.stop();
            }
        }
        if !hovered {
            b_nhover.schedule_unlocked_nested_dismiss();
        }
    });

    let b_nbhover = bridge.clone();
    pb.on_nested_body_hover_changed(move |hovered| {
        {
            let mut st = b_nbhover.state.borrow_mut();
            if hovered {
                st.nested_body_hover_count = st.nested_body_hover_count.saturating_add(1);
                st.nested_dismiss_timer.stop();
            } else {
                st.nested_body_hover_count = st.nested_body_hover_count.saturating_sub(1);
            }
        }
        if !hovered {
            b_nbhover.schedule_unlocked_nested_dismiss();
        }
    });

    let b_nlock = bridge.clone();
    pb.on_lock_nested_popup(move || {
        let Some(app) = b_nlock.app_weak.upgrade() else {
            return;
        };
        let mut locked_any = false;
        {
            let mut st = b_nlock.state.borrow_mut();
            st.nested_dismiss_timer.stop();
            if let Some(top) = st.nested_stack.last_mut() {
                top.locked = true;
                locked_any = true;
            }
        }
        if locked_any {
            app.global::<PopupBridge>().set_nested_locked(true);
        }
    });

    let b_npop = bridge.clone();
    pb.on_pop_nested_popup(move || {
        if let Some(app) = b_npop.app_weak.upgrade() {
            b_npop.pop_nested_popup(&app);
        }
    });

    let b_ndismiss = bridge.clone();
    pb.on_dismiss_all_nested_popups(move || {
        if let Some(app) = b_ndismiss.app_weak.upgrade() {
            b_ndismiss.dismiss_all_nested_popups(&app);
        }
    });

    let b_copy = bridge.clone();
    pb.on_copy_selected_glossary(move || {
        let Some(app) = b_copy.app_weak.upgrade() else {
            return;
        };
        let pb = app.global::<PopupBridge>();
        let text = pb.get_selected_glossary_text().to_string();
        if text.is_empty() {
            return;
        }
        pb.set_last_copied_glossary_text(text.clone().into());
        copy_to_persistent_clipboard(text);
    });

    let b_pkey = bridge.clone();
    pb.on_popup_key_pressed(move |text, control, shift, alt, meta| {
        if let Some((c, s, a, m)) =
            resolve_bare_modifier_event(text.as_str(), control, shift, alt, meta)
        {
            if let Some(app) = b_pkey.app_weak.upgrade() {
                b_pkey.ensure_fresh_db_and_config(&app, false);
                if app.get_dict_open() {
                    let (mod_setting, target_opt) = {
                        let st = b_pkey.state.borrow();
                        (
                            st.settings.search_modifier,
                            st.hovered_glossary_target.clone(),
                        )
                    };
                    if let Some(target) = target_opt {
                        if modifier_matches_setting(mod_setting, c, s, a, m) {
                            if !b_pkey.is_nested_target_already_shown(&target) {
                                b_pkey.dispatch_nested_scan(&app, target, false);
                            }
                            return true;
                        }
                    }
                }
                app.global::<SubtitleOverlayBridge>()
                    .invoke_stationary_modifier_changed(c, s, a, m);
            }
            return true;
        }
        false
    });

    let b_prel = bridge.clone();
    pb.on_popup_key_released(move |control, shift, alt, meta| {
        if let Some(app) = b_prel.app_weak.upgrade() {
            app.global::<SubtitleOverlayBridge>()
                .invoke_stationary_modifier_changed(control, shift, alt, meta);
        }
    });

    // Independent DictionarySearchPanel callbacks
    let b_panel_search = bridge.clone();
    pb.on_panel_search_requested(move |query| {
        let Some(app) = b_panel_search.app_weak.upgrade() else {
            return;
        };
        b_panel_search.ensure_fresh_db_and_config(&app, false);
        let pb = app.global::<PopupBridge>();
        pb.set_panel_query(query.clone());
        let kana_only = pb.get_panel_kana_only();
        let (excluded, search_cfg) = {
            let st = b_panel_search.state.borrow();
            (
                st.settings.excluded_glossaries.clone(),
                st.settings.dict_search_config(),
            )
        };
        let gen_id = b_panel_search.panel_epoch.fetch_add(1, Ordering::AcqRel) + 1;
        if query.trim().is_empty() {
            pb.set_panel_results(Rc::new(VecModel::from(Vec::<DictResult>::new())).into());
            pb.set_panel_searching(false);
            return;
        }
        pb.set_panel_searching(true);
        let _ = b_panel_search.panel_tx.send(PanelSearchRequest {
            gen_id,
            query: query.to_string(),
            kana_only,
            excluded,
            search_cfg,
        });
        b_panel_search.kick_no_event_loop_pump();
    });

    let b_panel_kana = bridge.clone();
    pb.on_panel_toggle_kana_only(move || {
        let Some(app) = b_panel_kana.app_weak.upgrade() else {
            return;
        };
        let pb = app.global::<PopupBridge>();
        let next = !pb.get_panel_kana_only();
        pb.set_panel_kana_only(next);
        let q = pb.get_panel_query();
        if !q.trim().is_empty() {
            pb.invoke_panel_search_requested(q);
        }
    });

    let b_panel_lookup = bridge.clone();
    pb.on_panel_lookup_requested(move |word| {
        let Some(app) = b_panel_lookup.app_weak.upgrade() else {
            return;
        };
        b_panel_lookup.navigate_to_term_query(&app, word.as_str(), None);
    });
}

fn wire_subtitle_bridge(app: &MementoApp, bridge: &Rc<RichUiBridge>) {
    let sb = app.global::<SubtitleOverlayBridge>();

    let b_sub_render = bridge.clone();
    sb.on_render_token(
        move |word, font_px, show_furigana, token_selected, _style_rev, _hl_rev| {
            b_sub_render.render_subtitle_token(word, font_px, show_furigana, token_selected)
        },
    );

    let b_cluster_off = bridge.clone();
    sb.on_cluster_char_offset(move |word, font_px, show_furigana, local_x, local_y| {
        let style = b_cluster_off.state.borrow().settings.subtitle_style.clone();
        subtitle_cluster_at_point_with_style(&word, font_px, show_furigana, &style, local_x, local_y)
            .map_or(-1, |(_, char_off)| char_off as i32)
    });

    let b_can_focus = bridge.clone();
    sb.on_should_capture_hover_focus(move || {
        let Some(app) = b_can_focus.app_weak.upgrade() else {
            return false;
        };
        let pb = app.global::<PopupBridge>();
        let has_glossary_selection =
            app.get_dict_open() && !pb.get_selected_glossary_text().is_empty();
        !app.get_dict_search_open()
            && !app.get_sub_drawer_open()
            && !app.get_menu_open()
            && !app.get_ocr_mode()
            && !app.get_about_open()
            && !app.get_url_dialog_open()
            && !has_glossary_selection
    });

    let b_ptr_hover = bridge.clone();
    sb.on_pointer_hovered(
        move |word, font_px, show_furigana, local_x, local_y, control, shift, alt, meta| {
            let Some(app) = b_ptr_hover.app_weak.upgrade() else {
                return;
            };
            app.invoke_osc_activity();

            let Some(cluster) = b_ptr_hover.resolve_subtitle_cluster(
                &app,
                &word,
                font_px,
                show_furigana,
                local_x,
                local_y,
            ) else {
                return;
            };

            let sb = app.global::<SubtitleOverlayBridge>();
            let prev_global = sb.get_hovered_global_char_index();
            sb.set_hovered_token_index(word.index);
            sb.set_hovered_char_offset(cluster.char_offset as i32);
            sb.set_hovered_global_char_index(cluster.global_char_index as i32);
            sb.set_hovered_query(cluster.sub_query.clone().into());

            let (method, mod_setting, delay_ms) = {
                let mut st = b_ptr_hover.state.borrow_mut();
                st.hovered_cluster = Some(cluster.clone());
                (
                    st.settings.search_method,
                    st.settings.search_modifier,
                    st.settings.search_delay_ms,
                )
            };
            let mod_held = modifier_matches_setting(mod_setting, control, shift, alt, meta);
            if method == 1 && !mod_held {
                b_ptr_hover.state.borrow_mut().scan_timer.stop();
                return;
            }
            if app.get_dict_open() && app.get_selected_token_index() >= 0 && !mod_held {
                return;
            }
            if prev_global == cluster.global_char_index as i32 && app.get_dict_open() {
                return;
            }

            let wait_ms = if mod_held { delay_ms.min(50) } else { delay_ms };
            let target_global = cluster.global_char_index;
            let weak_bridge = Rc::downgrade(&b_ptr_hover);
            b_ptr_hover.state.borrow_mut().scan_timer.start(
                slint::TimerMode::SingleShot,
                std::time::Duration::from_millis(wait_ms),
                move || {
                    let Some(br) = weak_bridge.upgrade() else {
                        return;
                    };
                    let Some(app) = br.app_weak.upgrade() else {
                        return;
                    };
                    let current = br.state.borrow().hovered_cluster.clone();
                    if let Some(c) = current {
                        if c.global_char_index == target_global {
                            br.trigger_subtitle_scan_now(&app, c);
                        }
                    }
                },
            );
        },
    );

    let b_ptr_left = bridge.clone();
    sb.on_pointer_left(move |token_index| {
        let Some(app) = b_ptr_left.app_weak.upgrade() else {
            return;
        };
        let mut clear = false;
        {
            let mut st = b_ptr_left.state.borrow_mut();
            if st
                .hovered_cluster
                .as_ref()
                .is_some_and(|c| c.word.index == token_index)
            {
                st.scan_timer.stop();
                st.hovered_cluster = None;
                clear = true;
            }
        }
        if clear {
            let sb = app.global::<SubtitleOverlayBridge>();
            sb.set_hovered_token_index(-1);
            sb.set_hovered_char_offset(-1);
            sb.set_hovered_global_char_index(-1);
            sb.set_hovered_query("".into());
        }
    });

    let b_left_click = bridge.clone();
    sb.on_token_left_clicked(move |word, font_px, show_furigana, local_x, local_y| {
        let Some(app) = b_left_click.app_weak.upgrade() else {
            return;
        };
        if let Some(cluster) = b_left_click.resolve_subtitle_cluster(
            &app,
            &word,
            font_px,
            show_furigana,
            local_x,
            local_y,
        ) {
            b_left_click.trigger_subtitle_scan_now(&app, cluster);
        } else if !word.query.is_empty() {
            app.set_popup_offset_x(app.get_pending_popup_offset_x());
            app.set_selected_token_index(word.index);
            app.invoke_lookup_word(word.query);
        }
    });

    let b_mid_click = bridge.clone();
    sb.on_token_middle_clicked(move |word, font_px, show_furigana, local_x, local_y| {
        let Some(app) = b_mid_click.app_weak.upgrade() else {
            return;
        };
        b_mid_click.ensure_fresh_db_and_config(&app, false);
        let enabled = app
            .global::<SubtitleOverlayBridge>()
            .get_middle_mouse_scan_enabled();
        if !enabled {
            return;
        }
        if let Some(cluster) = b_mid_click.resolve_subtitle_cluster(
            &app,
            &word,
            font_px,
            show_furigana,
            local_x,
            local_y,
        ) {
            b_mid_click.trigger_subtitle_scan_now(&app, cluster);
        }
    });

    let b_stat_mod = bridge.clone();
    sb.on_stationary_modifier_changed(move |control, shift, alt, meta| {
        let Some(app) = b_stat_mod.app_weak.upgrade() else {
            return;
        };
        b_stat_mod.ensure_fresh_db_and_config(&app, false);
        let (mod_setting, cluster_opt, ass_target_opt, gloss_target_opt) = {
            let st = b_stat_mod.state.borrow();
            (
                st.settings.search_modifier,
                st.hovered_cluster.clone(),
                st.hovered_ass_target.clone(),
                st.hovered_glossary_target.clone(),
            )
        };
        if modifier_matches_setting(mod_setting, control, shift, alt, meta) {
            if app.get_dict_open() {
                if let Some(target) = gloss_target_opt {
                    if !b_stat_mod.is_nested_target_already_shown(&target) {
                        b_stat_mod.dispatch_nested_scan(&app, target, false);
                    }
                    return;
                }
            }
            if let Some(ass_target) = ass_target_opt {
                b_stat_mod.trigger_ass_scan_now(&app, ass_target);
            } else if let Some(cluster) = cluster_opt {
                b_stat_mod.trigger_subtitle_scan_now(&app, cluster);
            }
        }
    });

    let b_ass_line_hover = bridge.clone();
    sb.on_ass_line_hover_changed(move |event_idx, line_idx, hovering| {
        let Some(app) = b_ass_line_hover.app_weak.upgrade() else {
            return;
        };
        let ass_ctrl = b_ass_line_hover.state.borrow().ass_controller.clone();
        if let Some(ass_ctrl) = ass_ctrl {
            if let Ok(mut ctrl) = ass_ctrl.try_borrow_mut() {
                ctrl.on_line_hover_changed(&app, event_idx, line_idx, hovering);
            }
        }
    });

    let b_ass_hover = bridge.clone();
    sb.on_ass_pointer_hovered(move |vx, vy, control, shift, alt, meta| {
        let Some(app) = b_ass_hover.app_weak.upgrade() else {
            return;
        };
        app.invoke_osc_activity();

        let ass_ctrl = b_ass_hover.state.borrow().ass_controller.clone();
        let Some(ass_ctrl) = ass_ctrl else {
            return;
        };
        let target = {
            let Ok(ctrl) = ass_ctrl.try_borrow() else {
                return;
            };
            ctrl.hit_test_logical(&app, vx, vy)
        };
        let Some(target) = target else {
            return;
        };
        if target.ch.is_whitespace() {
            return;
        }

        let sb = app.global::<SubtitleOverlayBridge>();
        let prev_target = b_ass_hover.state.borrow().hovered_ass_target.clone();
        let same_char = prev_target.as_ref().is_some_and(|p| {
            p.event_index == target.event_index && p.char_index == target.char_index
        });
        sb.set_hovered_global_char_index(target.global_char_index as i32);
        sb.set_hovered_query(target.scan_substring.clone().into());

        let (method, mod_setting, delay_ms) = {
            let mut st = b_ass_hover.state.borrow_mut();
            st.hovered_ass_target = Some(target.clone());
            (
                st.settings.search_method,
                st.settings.search_modifier,
                st.settings.search_delay_ms,
            )
        };
        let mod_held = modifier_matches_setting(mod_setting, control, shift, alt, meta);
        if method == 1 && !mod_held {
            b_ass_hover.state.borrow_mut().scan_timer.stop();
            return;
        }
        if app.get_dict_open() && app.get_selected_token_index() >= 0 && !mod_held {
            return;
        }
        if same_char && app.get_dict_open() {
            return;
        }
        if !memento_ass::is_japanese_or_alnum_lookup_char(target.ch) && !mod_held {
            return;
        }

        let wait_ms = if mod_held { delay_ms.min(50) } else { delay_ms };
        let target_key = (target.event_index, target.char_index);
        let weak_bridge = Rc::downgrade(&b_ass_hover);
        b_ass_hover.state.borrow_mut().scan_timer.start(
            slint::TimerMode::SingleShot,
            std::time::Duration::from_millis(wait_ms),
            move || {
                let Some(br) = weak_bridge.upgrade() else {
                    return;
                };
                let Some(app) = br.app_weak.upgrade() else {
                    return;
                };
                let current = br.state.borrow().hovered_ass_target.clone();
                if let Some(cur) = current {
                    if (cur.event_index, cur.char_index) == target_key {
                        br.trigger_ass_scan_now(&app, cur);
                    }
                }
            },
        );
    });

    let b_ass_left = bridge.clone();
    sb.on_ass_pointer_left(move || {
        let Some(app) = b_ass_left.app_weak.upgrade() else {
            return;
        };
        let still_hovered = app.global::<SubtitleOverlayBridge>().get_ass_hovered();
        if !still_hovered {
            {
                let mut st = b_ass_left.state.borrow_mut();
                st.scan_timer.stop();
                st.hovered_ass_target = None;
            }
            let sb = app.global::<SubtitleOverlayBridge>();
            sb.set_hovered_global_char_index(-1);
            sb.set_hovered_query("".into());
        }
    });

    let b_ass_click = bridge.clone();
    sb.on_ass_left_clicked(move |vx, vy| {
        let Some(app) = b_ass_click.app_weak.upgrade() else {
            return;
        };
        b_ass_click.ensure_fresh_db_and_config(&app, false);
        let ass_ctrl = b_ass_click.state.borrow().ass_controller.clone();
        let Some(ass_ctrl) = ass_ctrl else {
            return;
        };
        let target = {
            let Ok(ctrl) = ass_ctrl.try_borrow() else {
                return;
            };
            ctrl.hit_test_logical(&app, vx, vy)
        };
        if let Some(target) = target {
            if !target.ch.is_whitespace() {
                b_ass_click.state.borrow_mut().hovered_ass_target = Some(target.clone());
                b_ass_click.trigger_ass_scan_now(&app, target);
            }
        }
    });

    let b_ass_mid = bridge.clone();
    sb.on_ass_middle_clicked(move |vx, vy| {
        let Some(app) = b_ass_mid.app_weak.upgrade() else {
            return;
        };
        b_ass_mid.ensure_fresh_db_and_config(&app, false);
        if !app
            .global::<SubtitleOverlayBridge>()
            .get_middle_mouse_scan_enabled()
        {
            return;
        }
        let ass_ctrl = b_ass_mid.state.borrow().ass_controller.clone();
        let Some(ass_ctrl) = ass_ctrl else {
            return;
        };
        let target = {
            let Ok(ctrl) = ass_ctrl.try_borrow() else {
                return;
            };
            ctrl.hit_test_logical(&app, vx, vy)
        };
        if let Some(target) = target {
            if !target.ch.is_whitespace() {
                b_ass_mid.state.borrow_mut().hovered_ass_target = Some(target.clone());
                b_ass_mid.trigger_ass_scan_now(&app, target);
            }
        }
    });

    let b_hkey = bridge.clone();
    sb.on_hover_key_pressed(move |text, control, shift, alt, meta| {
        let Some(app) = b_hkey.app_weak.upgrade() else {
            return false;
        };
        if let Some((c, s, a, m)) =
            resolve_bare_modifier_event(text.as_str(), control, shift, alt, meta)
        {
            app.global::<SubtitleOverlayBridge>()
                .invoke_stationary_modifier_changed(c, s, a, m);
            return true;
        }
        false
    });
}

/// Extension hook for rich definition/subtitle UI bridges.
pub fn install_rich_ui_bridges(
    app: &MementoApp,
    dictionary_db_path: &Path,
    config_dir: &Path,
    config: &ConfigController,
) {
    let settings = CachedConfigSettings::from_controller(config, config_dir);
    let middle_mouse_default = settings.middle_mouse_scan;
    let initial_max_recursion_depth = settings.max_recursion_depth;
    let initial_sub_bg = settings.subtitle_style.bg_rgba;
    let initial_anki_show_tag = settings.anki_show_tag;
    set_active_subtitle_render_style(settings.subtitle_style.clone());

    let dict_resource_dirs = collect_dictionary_resource_dirs(config_dir, dictionary_db_path);
    let db_sig = crate::controller::dict::db_file_fingerprint(dictionary_db_path);
    let mut dict_ctrl = DictController::open(dictionary_db_path).unwrap_or_default();
    let initial_search_cfg = settings.dict_search_config();
    initial_search_cfg.apply_to(&mut dict_ctrl);

    // Event-driven worker channels (zero 8ms polling timers in production).
    let (panel_tx, panel_rx) = mpsc::channel::<PanelSearchRequest>();
    let (panel_res_tx, raw_panel_res_rx) = mpsc::channel::<PanelSearchResponse>();
    let panel_res_rx = Arc::new(Mutex::new(raw_panel_res_rx));
    let panel_epoch = Arc::new(AtomicU64::new(0));
    spawn_panel_worker(
        dictionary_db_path.to_path_buf(),
        panel_rx,
        panel_res_tx,
        panel_epoch.clone(),
        app.as_weak(),
    );

    let (nested_tx, nested_rx) = mpsc::channel::<NestedScanRequest>();
    let (nested_res_tx, raw_nested_res_rx) = mpsc::channel::<NestedScanResponse>();
    let nested_res_rx = Arc::new(Mutex::new(raw_nested_res_rx));
    let nested_epoch = Arc::new(AtomicU64::new(0));
    spawn_nested_worker(
        dictionary_db_path.to_path_buf(),
        nested_rx,
        nested_res_tx,
        nested_epoch.clone(),
        app.as_weak(),
    );

    let (anki_tx, anki_rx) = mpsc::channel::<AnkiPopupCheckRequest>();
    let (anki_res_tx, raw_anki_res_rx) = mpsc::channel::<AnkiPopupCheckResponse>();
    let anki_res_rx = Arc::new(Mutex::new(raw_anki_res_rx));
    let anki_nav_epoch = Arc::new(AtomicU64::new(0));
    let nested_anki_nav_epoch = Arc::new(AtomicU64::new(0));
    let root_anki_in_flight = Arc::new(AtomicU64::new(0));
    let nested_anki_in_flight = Arc::new(AtomicU64::new(0));
    let external_lookup_epoch: Arc<Mutex<Option<Arc<AtomicU64>>>> = Arc::new(Mutex::new(None));
    spawn_anki_worker(
        anki_rx,
        anki_res_tx,
        anki_nav_epoch.clone(),
        nested_anki_nav_epoch.clone(),
        root_anki_in_flight.clone(),
        nested_anki_in_flight.clone(),
        external_lookup_epoch.clone(),
        app.as_weak(),
    );

    let has_event_loop_proxy = slint::invoke_from_event_loop(|| {}).is_ok();
    let instance_id = NEXT_BRIDGE_INSTANCE_ID.fetch_add(1, Ordering::Relaxed);
    app.global::<PopupBridge>()
        .set_bridge_instance_id(instance_id);

    let stylesheets = StylesheetCache::new(config_dir);
    let initial_popup_theme = Arc::new(
        stylesheets
            .theme()
            .with_interface_font_fallbacks(stylesheets.overrides(), &settings.popup_fonts),
    );
    let cached_theme_fonts = settings.popup_fonts.clone();

    let bridge = Rc::new(RichUiBridge {
        instance_id,
        app_weak: app.as_weak(),
        db_path: dictionary_db_path.to_path_buf(),
        state: RefCell::new(RichUiMutable {
            config_dir: config_dir.to_path_buf(),
            dict_resource_dirs,
            settings,
            dict_ctrl,
            applied_search_cfg: Some(initial_search_cfg),
            db_sig,
            db_helper: RichDbHelper::new(dictionary_db_path.to_path_buf()),
            stylesheets,
            cached_theme: initial_popup_theme.clone(),
            cached_theme_fonts,
            last_fresh_check: Some(Instant::now()),
            rich_epoch: 1,
            row_extras: HashMap::new(),
            current_hits: Vec::new(),
            glossary_cache: GlossaryDocLruCache::new_root(),
            last_rendered_key: HashMap::new(),
            selection: None,
            root_scan_highlight: None,
            root_expanded_cards: HashSet::new(),
            root_row_anki_note_ids_i64: Vec::new(),
            last_active_glossary_row_idx: 0,
            suppress_history_reset: false,
            back_stack: VecDeque::new(),
            forward_stack: VecDeque::new(),
            nested_stack: Vec::new(),
            hovered_glossary_target: None,
            nested_card_hovered: false,
            nested_body_hover_count: 0,
            nested_scan_timer: slint::Timer::default(),
            nested_dismiss_timer: slint::Timer::default(),
            pending_nested_anki_adds: VecDeque::new(),
            hovered_cluster: None,
            hovered_ass_target: None,
            pending_ass_scan: None,
            ass_controller: None,
            active_scan_utf16_range: None,
            pending_scan_utf16_start: None,
            pending_scan_sub_query: None,
            scan_timer: slint::Timer::default(),
            subtitle_token_layout_cache: SubtitleTokenLayoutCache::default(),
        }),
        panel_tx,
        panel_res_rx,
        panel_epoch,
        anki_tx,
        anki_res_rx,
        anki_nav_epoch,
        nested_anki_nav_epoch,
        root_anki_in_flight,
        nested_anki_in_flight,
        external_lookup_epoch,
        nested_tx,
        nested_res_rx,
        nested_epoch,
        no_event_loop_pump: (!has_event_loop_proxy).then(slint::Timer::default),
    });

    register_active_rich_bridge(bridge.clone());

    let pb = app.global::<PopupBridge>();
    pb.set_rich_enabled(true);
    pb.set_anki_show_tags(initial_anki_show_tag);
    pb.set_nested_max_depth(initial_max_recursion_depth as i32);
    apply_popup_css_theme_to_bridge(app, &initial_popup_theme);
    wire_popup_bridge(app, &bridge);

    let sb = app.global::<SubtitleOverlayBridge>();
    sb.set_rich_enabled(true);
    sb.set_subtitle_bg_color(slint::Color::from_argb_u8(
        initial_sub_bg[3],
        initial_sub_bg[0],
        initial_sub_bg[1],
        initial_sub_bg[2],
    ));
    sb.set_middle_mouse_scan_enabled(middle_mouse_default);
    wire_subtitle_bridge(app, &bridge);
}

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use subomi_definition::CssTheme;
use subomi_dict::searchcontroller::has_wildcard_chars;
use slint::{ComponentHandle, Model as _, SharedString, VecModel};

use crate::controller::anki::EntryAnkiLookup;
use crate::controller::config::ConfigController;
use crate::controller::dict::DictController;
use crate::ui::dict_assets::collect_dictionary_resource_dirs;
use crate::ui::glossary_render::rasterize_structured_glossary_entry_with_engine;
use crate::ui::popup_css::{rgba_to_slint_color, StylesheetCache};
use crate::ui::popup_model::{
    compact_frequency_badges_from_popup, is_kanji_char, register_active_rich_bridge,
    split_anki_tags_str, NEXT_BRIDGE_INSTANCE_ID,
};
use crate::ui::rich_db::RichDbHelper;
use crate::ui::subtitle_layout::set_active_subtitle_render_style;
use crate::ui::text_engine::{
    capture_shared_font_collection, subtitle_cluster_at_point_with_style, with_shared_text_engine,
    TextEngine,
};
use crate::ui::{
    CompactFrequencyItem, CompactFrequencyLine, DefinitionGridRow, DictResult, SubomiApp,
    PopupBridge, PopupDictStyle, PopupFreqStyle, PopupFrequency, PopupLayout, RichHeadwordChar,
    RichHeadwordLine, SubtitleOverlayBridge,
};

use super::anki::{drain_anki_results_for_app, sync_anki_checking_for_app};
use super::glossary::drain_glossary_prerender_results_for_app;
use super::nested::drain_nested_scan_results_for_app;
use super::state::{
    apply_popup_css_theme_to_bridge, collect_hits_term_and_kanji_meta,
    copy_to_persistent_clipboard, current_external_lookup_epoch, execute_nav_lookup,
    modifier_matches_setting, push_history_capped, resolve_bare_modifier_event,
    sync_subtitle_overlay_style_to_bridge, utf16_units_to_char_count, AnkiCheckTarget,
    AnkiPopupCheckRequest, AnkiPopupCheckResponse, CachedConfigSettings, DictSearchConfig,
    DictWorkerRequest, GlossaryDocLruCache, GlossaryPreRenderBatch, GlossaryPreRenderDone,
    NavLookupRequest, NavLookupResponse, NestedScanRequest, NestedScanResponse, PanelPopupState,
    PanelSearchRequest, PanelSearchResponse, RichUiMutable, SubtitleTokenLayoutCache,
};
use super::{drain_nav_lookup_results_for_app, drain_panel_results_for_app, RichUiBridge};

fn spawn_panel_worker(
    dictionary_db_path: PathBuf,
    panel_rx: mpsc::Receiver<PanelSearchRequest>,
    panel_res_tx: mpsc::Sender<PanelSearchResponse>,
    worker_epoch: Arc<AtomicU64>,
    worker_app: slint::Weak<SubomiApp>,
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
                    term_meta_rows: Vec::new(),
                    term_pitch_details: HashMap::new(),
                    kanji_frequencies: HashMap::new(),
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
            let hits = if req.kanji_only {
                let mut kanji_hits = Vec::new();
                let mut seen_chars = HashSet::new();
                for ch in trimmed.chars() {
                    if is_kanji_char(ch) && seen_chars.insert(ch) {
                        let s = ch.to_string();
                        kanji_hits.extend_from_slice(dict.search_kanji(&s));
                    }
                }
                if kanji_hits.is_empty() {
                    kanji_hits = dict.search_kanji(&trimmed).to_vec();
                }
                kanji_hits
            } else if has_wildcard_chars(&trimmed) {
                let mut raw = dict.search_manual(&trimmed).to_vec();
                if req.kana_only {
                    raw.retain(|h| !h.reading.is_empty() || !h.expression.chars().any(is_kanji_char));
                }
                raw
            } else if req.kana_only {
                let norm = crate::controller::dict::normalize_reading(&trimmed);
                let mut raw = dict.search_manual(&norm).to_vec();
                raw.retain(|h| {
                    crate::controller::dict::normalize_reading(&h.reading).starts_with(&norm)
                        || crate::controller::dict::normalize_reading(&h.expression)
                            .starts_with(&norm)
                });
                raw
            } else {
                let mut raw = dict.search_manual(&trimmed).to_vec();
                if raw.is_empty() && trimmed.chars().count() == 1 {
                    raw = dict.search_kanji(&trimmed).to_vec();
                }
                raw
            };
            if req.gen_id != worker_epoch.load(Ordering::Acquire) {
                continue;
            }
            let (term_meta_rows, term_pitch_details, kanji_frequencies) =
                collect_hits_term_and_kanji_meta(&dict, &hits);
            if req.gen_id != worker_epoch.load(Ordering::Acquire) {
                continue;
            }
            let _ = panel_res_tx.send(PanelSearchResponse {
                gen_id: req.gen_id,
                hits,
                excluded: req.excluded,
                term_meta_rows,
                term_pitch_details,
                kanji_frequencies,
            });
            let _ = worker_app.upgrade_in_event_loop(move |app| {
                let _ = drain_panel_results_for_app(&app);
            });
        }
    });
}

#[allow(clippy::too_many_arguments)]
fn spawn_nested_worker(
    dictionary_db_path: PathBuf,
    worker_rx: mpsc::Receiver<DictWorkerRequest>,
    nested_res_tx: mpsc::Sender<NestedScanResponse>,
    nav_res_tx: mpsc::Sender<NavLookupResponse>,
    worker_nested_epoch: Arc<AtomicU64>,
    worker_nav_epoch: Arc<AtomicU64>,
    worker_app: slint::Weak<SubomiApp>,
) {
    std::thread::spawn(move || {
        let mut sig = crate::controller::dict::db_file_fingerprint(&dictionary_db_path);
        let mut dict = DictController::open(&dictionary_db_path).unwrap_or_default();
        let mut applied_cfg: Option<DictSearchConfig> = None;
        while let Ok(first) = worker_rx.recv() {
            let mut pending_scan: Option<NestedScanRequest> = None;
            let mut pending_root_nav: Option<NavLookupRequest> = None;
            let mut pending_nested_nav: Option<NavLookupRequest> = None;
            {
                let mut accept = |item: DictWorkerRequest| match item {
                    DictWorkerRequest::NestedScan(req) => pending_scan = Some(req),
                    DictWorkerRequest::NavLookup(req) => {
                        if req.kind.is_nested() {
                            pending_nested_nav = Some(req);
                        } else {
                            pending_root_nav = Some(req);
                        }
                    }
                };
                accept(first);
                while let Ok(newer) = worker_rx.try_recv() {
                    accept(newer);
                }
            }

            let ensure_dict = |dict: &mut DictController,
                               sig: &mut crate::controller::dict::DbFileFingerprint,
                               applied_cfg: &mut Option<DictSearchConfig>,
                               cfg: &DictSearchConfig| {
                let cur_sig = crate::controller::dict::db_file_fingerprint(&dictionary_db_path);
                if cur_sig != *sig {
                    *sig = cur_sig;
                    *dict = DictController::open(&dictionary_db_path).unwrap_or_default();
                    *applied_cfg = None;
                }
                if applied_cfg.as_ref() != Some(cfg) {
                    cfg.apply_to(dict);
                    *applied_cfg = Some(cfg.clone());
                }
            };

            for nav_req in pending_root_nav.into_iter().chain(pending_nested_nav) {
                let target_epoch = if nav_req.kind.is_nested() {
                    &worker_nested_epoch
                } else {
                    &worker_nav_epoch
                };
                if nav_req.epoch != target_epoch.load(Ordering::Acquire) {
                    continue;
                }
                ensure_dict(&mut dict, &mut sig, &mut applied_cfg, &nav_req.search_config);
                let resp = execute_nav_lookup(&mut dict, nav_req);
                if resp.epoch != target_epoch.load(Ordering::Acquire) {
                    continue;
                }
                let _ = nav_res_tx.send(resp);
                let _ = worker_app.upgrade_in_event_loop(move |app| {
                    let _ = drain_nav_lookup_results_for_app(&app);
                });
            }

            if let Some(req) = pending_scan {
                if req.epoch != worker_nested_epoch.load(Ordering::Acquire) {
                    continue;
                }
                ensure_dict(&mut dict, &mut sig, &mut applied_cfg, &req.search_config);

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
                let (term_meta_rows, term_pitch_details, kanji_frequencies) =
                    collect_hits_term_and_kanji_meta(&dict, &hits);

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
                    kanji_frequencies,
                });
                let _ = worker_app.upgrade_in_event_loop(move |app| {
                    let _ = drain_nested_scan_results_for_app(&app);
                });
            }
        }
    });
}

fn spawn_glossary_render_worker(
    rx: mpsc::Receiver<GlossaryPreRenderBatch>,
    res_tx: mpsc::Sender<GlossaryPreRenderDone>,
    worker_epoch: Arc<AtomicU64>,
    worker_app: slint::Weak<SubomiApp>,
    shared_font_collection: Option<slint::fontique_08::fontique::Collection>,
) {
    const WORKER_IDLE_ENGINE_DROP: Duration = Duration::from_secs(60);
    let _ = std::thread::Builder::new()
        .name("subomi-glossary-prerender".into())
        .spawn(move || {
            let mut engine: Option<TextEngine> = None;
            loop {
                let mut batch = if engine.is_some() {
                    match rx.recv_timeout(WORKER_IDLE_ENGINE_DROP) {
                        Ok(b) => b,
                        Err(mpsc::RecvTimeoutError::Timeout) => {
                            engine = None;
                            crate::ui::release_os_memory();
                            continue;
                        }
                        Err(mpsc::RecvTimeoutError::Disconnected) => return,
                    }
                } else {
                    match rx.recv() {
                        Ok(b) => b,
                        Err(_) => return,
                    }
                };
                while let Ok(newer) = rx.try_recv() {
                    batch = newer;
                }
                if batch.epoch != worker_epoch.load(Ordering::Acquire) {
                    continue;
                }

                let eng = engine.get_or_insert_with(|| {
                    TextEngine::new_with_collection(shared_font_collection.clone())
                });

                for item in batch.items {
                    if batch.epoch != worker_epoch.load(Ordering::Acquire) {
                        break;
                    }
                    let Some(doc) = rasterize_structured_glossary_entry_with_engine(
                        eng,
                        &item.raw_glossary,
                        &item.fallback_glossary,
                        item.parsed_glossary.as_deref(),
                        item.stylesheet,
                        &item.detail_states,
                        item.legacy_style,
                        &item.dict_resource_dirs,
                        item.dic_id,
                        &item.dict_name,
                        item.avail_width_px,
                        item.selected,
                        &item.theme,
                        item.render_scale,
                    ) else {
                        continue;
                    };
                    if batch.epoch != worker_epoch.load(Ordering::Acquire) {
                        break;
                    }
                    if res_tx
                        .send(GlossaryPreRenderDone {
                            epoch: batch.epoch,
                            cache_key: item.cache_key,
                            doc,
                        })
                        .is_err()
                    {
                        return;
                    }
                    let _ = worker_app.upgrade_in_event_loop(move |app| {
                        let _ = drain_glossary_prerender_results_for_app(&app);
                    });
                }
            }
        });
}

#[allow(clippy::too_many_arguments)]
fn spawn_anki_worker(
    anki_rx: mpsc::Receiver<AnkiPopupCheckRequest>,
    anki_res_tx: mpsc::Sender<AnkiPopupCheckResponse>,
    worker_anki_epoch: Arc<AtomicU64>,
    worker_nested_anki_epoch: Arc<AtomicU64>,
    worker_panel_anki_epoch: Arc<AtomicU64>,
    root_in_flight: Arc<AtomicU64>,
    nested_in_flight: Arc<AtomicU64>,
    panel_in_flight: Arc<AtomicU64>,
    worker_anki_ext: Arc<Mutex<Option<Arc<AtomicU64>>>>,
    worker_anki_app: slint::Weak<SubomiApp>,
) {
    std::thread::spawn(move || {
        while let Ok(first_req) = anki_rx.recv() {
            let mut pending_root: Option<AnkiPopupCheckRequest> = None;
            let mut pending_nested: Option<AnkiPopupCheckRequest> = None;
            let mut pending_panel: Option<AnkiPopupCheckRequest> = None;
            {
                let mut accept = |item: AnkiPopupCheckRequest| match item.target {
                    AnkiCheckTarget::Root => pending_root = Some(item),
                    AnkiCheckTarget::Nested(_) => pending_nested = Some(item),
                    AnkiCheckTarget::Panel => pending_panel = Some(item),
                };
                accept(first_req);
                while let Ok(newer) = anki_rx.try_recv() {
                    accept(newer);
                }
            }
            for req in pending_root
                .into_iter()
                .chain(pending_nested)
                .chain(pending_panel)
            {
                let (target_epoch, target_in_flight, check_ext_epoch) = match req.target {
                    AnkiCheckTarget::Root => (&worker_anki_epoch, &root_in_flight, true),
                    AnkiCheckTarget::Nested(_) => {
                        (&worker_nested_anki_epoch, &nested_in_flight, true)
                    }
                    AnkiCheckTarget::Panel => (&worker_panel_anki_epoch, &panel_in_flight, false),
                };
                let is_cancelled = || {
                    req.nav_epoch != target_epoch.load(Ordering::Acquire)
                        || (check_ext_epoch
                            && req.lookup_epoch != current_external_lookup_epoch(&worker_anki_ext))
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
                let mut term_kana_statuses = HashMap::new();
                if let Some(mut profile) = req.term_profile.clone() {
                    if profile.endpoint.timeout.is_none() {
                        profile.endpoint.timeout = Some(std::time::Duration::from_secs(3));
                    }
                    let ctrl = crate::controller::anki::AnkiController::new(profile.endpoint());
                    let mut lookups: Vec<EntryAnkiLookup> = Vec::with_capacity(req.term_entries.len() * 2);
                    for (expr, reading) in &req.term_entries {
                        lookups.push(EntryAnkiLookup::new(expr.clone(), reading.clone()));
                    }
                    let mut kana_map: Vec<(usize, usize)> = Vec::new();
                    for (entry_idx, (expr, reading)) in req.term_entries.iter().enumerate() {
                        if !reading.is_empty() && reading != expr {
                            let lookup_idx = lookups.len();
                            lookups.push(
                                EntryAnkiLookup::new(expr.clone(), reading.clone())
                                    .with_reading_as_expression(true),
                            );
                            kana_map.push((entry_idx, lookup_idx));
                        }
                    }
                    if let Ok(statuses) = ctrl.check_entries_anki_status_with_lookups(
                        &profile,
                        &lookups,
                        req.show_tags,
                    ) {
                        for (entry_idx, pair) in req.term_entries.iter().enumerate() {
                            if let Some(st) = statuses.get(entry_idx) {
                                term_statuses.insert(pair.clone(), st.clone());
                            }
                        }
                        for (entry_idx, lookup_idx) in kana_map {
                            if let Some(st) = statuses.get(lookup_idx) {
                                term_kana_statuses
                                    .insert(req.term_entries[entry_idx].clone(), st.clone());
                            }
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
                    target: req.target,
                    show_tags: req.show_tags,
                    term_statuses,
                    term_kana_statuses,
                    kanji_statuses,
                });
                let _ = worker_anki_app.upgrade_in_event_loop(move |app| {
                    let _ = drain_anki_results_for_app(&app);
                });
            }
        }
    });
}

fn wire_popup_bridge(app: &SubomiApp, bridge: &Rc<RichUiBridge>) {
    app.global::<PopupLayout>()
        .on_compact_frequency_lines(|items, available_width| {
            let available_width = available_width.max(1.0);
            let mut lines = Vec::new();
            let mut entries = Vec::new();
            let mut line_width = 0.0f32;
            for item in items.iter() {
                let label = if item.omit_dict_name || item.dictionary.is_empty() {
                    item.value.to_string()
                } else {
                    format!("{} {}", item.dictionary, item.value)
                };
                let width = label
                    .chars()
                    .map(|ch| if ch.is_ascii() { 6.0 } else { 10.0 })
                    .sum::<f32>()
                    + 10.0;
                let gap = if entries.is_empty() { 0.0 } else { 4.0 };
                if !entries.is_empty() && line_width + gap + width > available_width {
                    lines.push(CompactFrequencyLine {
                        entries: Rc::new(VecModel::from(std::mem::take(&mut entries))).into(),
                    });
                    line_width = 0.0;
                }
                if !entries.is_empty() {
                    line_width += 4.0;
                }
                entries.push(item);
                line_width += width;
            }
            if !entries.is_empty() {
                lines.push(CompactFrequencyLine {
                    entries: Rc::new(VecModel::from(entries)).into(),
                });
            }
            Rc::new(VecModel::from(lines)).into()
        });

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

    let b_pitch_entries = bridge.clone();
    pb.on_rich_pitch_entries(move |row_idx, _rev| {
        let st = b_pitch_entries.state.borrow();
        let entries = st
            .row_extras
            .get(&row_idx)
            .map(|e| e.pitch_entries.clone())
            .unwrap_or_default();
        Rc::new(VecModel::from(entries)).into()
    });

    let b_pitch_pat = bridge.clone();
    pb.on_pitch_pattern_text(move |depth, row_idx, pitch_idx, _rev| {
        if pitch_idx < 0 {
            return SharedString::default();
        }
        let st = b_pitch_pat.state.borrow();
        let extras = if depth < 0 {
            st.panel.row_extras.get(&row_idx)
        } else if depth == 0 {
            st.row_extras.get(&row_idx)
        } else {
            st.nested_level(depth)
                .and_then(|lvl| lvl.row_extras.get(&row_idx))
        };
        extras
            .and_then(|e| e.pitch_details.get(pitch_idx as usize))
            .and_then(|d| d.pattern.as_deref())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(SharedString::from)
            .unwrap_or_default()
    });

    let b_compact_freq = bridge.clone();
    pb.on_row_compact_frequencies(move |depth, row_idx, _rev| {
        let Some(app) = b_compact_freq.app_weak.upgrade() else {
            return Rc::new(VecModel::from(Vec::<CompactFrequencyItem>::new())).into();
        };
        let compact_enabled = app.global::<PopupBridge>().get_compact_freq_badges();
        let raw_freqs: Vec<PopupFrequency> = if depth < 0 {
            let st = b_compact_freq.state.borrow();
            st.panel
                .rows
                .iter()
                .find(|r| r.index == row_idx)
                .map(|r| r.frequencies.iter().collect())
                .unwrap_or_default()
        } else if depth == 0 {
            let full = app.get_dict_results();
            let zero_idx = row_idx.saturating_sub(1) as usize;
            full.row_data(zero_idx)
                .filter(|r| r.index == row_idx)
                .or_else(|| {
                    (0..full.row_count())
                        .filter_map(|i| full.row_data(i))
                        .find(|r| r.index == row_idx)
                })
                .map(|r| r.frequencies.iter().collect())
                .unwrap_or_default()
        } else {
            let st = b_compact_freq.state.borrow();
            st.nested_level(depth)
                .and_then(|lvl| lvl.rows.iter().find(|r| r.index == row_idx))
                .map(|r| r.frequencies.iter().collect())
                .unwrap_or_default()
        };
        let items: Vec<CompactFrequencyItem> = compact_frequency_badges_from_popup(&raw_freqs)
            .into_iter()
            .map(|badge| CompactFrequencyItem {
                omit_dict_name: if compact_enabled {
                    !badge.show_dictionary
                } else {
                    badge.dictionary.is_empty()
                },
                dictionary: badge.dictionary.into(),
                value: badge.value.into(),
            })
            .collect();
        Rc::new(VecModel::from(items)).into()
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

    let b_npitch_entries = bridge.clone();
    pb.on_nested_rich_pitch_entries(move |depth, row_idx, _rev| {
        let st = b_npitch_entries.state.borrow();
        let entries = st
            .nested_level(depth)
            .and_then(|lvl| lvl.row_extras.get(&row_idx))
            .map(|e| e.pitch_entries.clone())
            .unwrap_or_default();
        Rc::new(VecModel::from(entries)).into()
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

    // Panel-isolated rich query callbacks (DictionarySearchPanel, scan-depth == -1)
    let b_phw_pitch = bridge.clone();
    pb.on_panel_headword_pitch_color(move |row_idx, _rev| {
        let st = b_phw_pitch.state.borrow();
        let cat = st
            .panel
            .row_extras
            .get(&row_idx)
            .and_then(|e| e.pitch_category);
        rgba_to_slint_color(st.effective_popup_theme().resolve_headword_pitch_color(cat))
    });

    let b_pterm_pills = bridge.clone();
    pb.on_panel_term_tag_pills(move |row_idx, _rev| {
        let st = b_pterm_pills.state.borrow();
        let pills = st
            .panel
            .row_extras
            .get(&row_idx)
            .map(|e| e.term_pills.clone())
            .unwrap_or_default();
        Rc::new(VecModel::from(pills)).into()
    });

    let b_pdef_pills = bridge.clone();
    pb.on_panel_def_tag_pills(move |row_idx, _rev| {
        let st = b_pdef_pills.state.borrow();
        let pills = st
            .panel
            .row_extras
            .get(&row_idx)
            .map(|e| e.def_pills.clone())
            .unwrap_or_default();
        Rc::new(VecModel::from(pills)).into()
    });

    let b_pderiv = bridge.clone();
    pb.on_panel_derivation_steps(move |row_idx, _rev| {
        let st = b_pderiv.state.borrow();
        let steps = st
            .panel
            .row_extras
            .get(&row_idx)
            .map(|e| e.derivation_steps.clone())
            .unwrap_or_default();
        Rc::new(VecModel::from(steps)).into()
    });

    let b_pderiv_sum = bridge.clone();
    pb.on_panel_derivation_summary(move |row_idx, _rev| {
        let st = b_pderiv_sum.state.borrow();
        let Some(extras) = st.panel.row_extras.get(&row_idx) else {
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

    let b_ppitches = bridge.clone();
    pb.on_panel_rich_pitch_graphs(move |row_idx, _rev| {
        let st = b_ppitches.state.borrow();
        let graphs = st
            .panel
            .row_extras
            .get(&row_idx)
            .map(|e| e.pitch_graphs.clone())
            .unwrap_or_default();
        Rc::new(VecModel::from(graphs)).into()
    });

    let b_ppitch_entries = bridge.clone();
    pb.on_panel_rich_pitch_entries(move |row_idx, _rev| {
        let st = b_ppitch_entries.state.borrow();
        let entries = st
            .panel
            .row_extras
            .get(&row_idx)
            .map(|e| e.pitch_entries.clone())
            .unwrap_or_default();
        Rc::new(VecModel::from(entries)).into()
    });

    let b_pkanji = bridge.clone();
    pb.on_panel_kanji_detail(move |row_idx, _rev| {
        let st = b_pkanji.state.borrow();
        st.panel
            .row_extras
            .get(&row_idx)
            .and_then(|e| e.kanji_detail.clone())
            .unwrap_or_default()
    });

    let b_prender = bridge.clone();
    pb.on_panel_render_glossary(move |row_idx, avail_w, selected, _rev, _sel_rev| {
        b_prender.build_rich_glossary_render(-1, row_idx, avail_w, selected)
    });

    let b_pcursor = bridge.clone();
    pb.on_panel_glossary_cursor_is_pointer(move |row_idx, x, y| {
        let Some(doc) = b_pcursor.latest_cached_doc_at_depth(-1, row_idx) else {
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

    let b_rae = bridge.clone();
    pb.on_toggle_reading_as_expression(move |row_zero_idx| {
        if let Some(app) = b_rae.app_weak.upgrade() {
            b_rae.toggle_reading_as_expression(&app, AnkiCheckTarget::Root, row_zero_idx);
        }
    });

    let b_nrae = bridge.clone();
    pb.on_nested_toggle_reading_as_expression(move |depth, row_zero_idx| {
        if depth > 0 {
            if let Some(app) = b_nrae.app_weak.upgrade() {
                b_nrae.toggle_reading_as_expression(
                    &app,
                    AnkiCheckTarget::Nested(depth as usize),
                    row_zero_idx,
                );
            }
        }
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
            if depth < 0 {
                if !st.panel.expanded_cards.remove(&row_idx) {
                    st.panel.expanded_cards.insert(row_idx);
                }
            } else if depth == 0 {
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
        if depth < 0 {
            pb.set_panel_revision(pb.get_panel_revision().wrapping_add(1));
        } else if depth == 0 {
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
            b_tip_hide.schedule_idle_memory_shrink_if_closed(&app);
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

    let b_nadd_audio = bridge.clone();
    pb.on_nested_add_to_anki_with_audio(move |row_zero_idx, source_idx| {
        if let Some(app) = b_nadd_audio.app_weak.upgrade() {
            b_nadd_audio.add_nested_anki_note_with_audio(&app, row_zero_idx, Some(source_idx));
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
        let is_escape = text.as_str() == "\u{1b}"
            || text.as_str() == "Escape"
            || text
                .chars()
                .next()
                .is_some_and(|ch| ch == char::from(slint::platform::Key::Escape));
        if is_escape {
            if let Some(app) = b_pkey.app_weak.upgrade() {
                let depth = app.global::<PopupBridge>().get_nested_depth();
                let had_sel = {
                    let st = b_pkey.state.borrow();
                    if depth > 0 {
                        st.nested_level(depth)
                            .is_some_and(|lvl| !lvl.selections.is_empty())
                    } else {
                        !st.selections.is_empty() || !st.panel.selections.is_empty()
                    }
                };
                if had_sel {
                    if depth > 0 {
                        b_pkey.clear_glossary_selections_at_depth(&app, depth);
                    } else {
                        b_pkey.clear_glossary_selections_at_depth(&app, 0);
                        b_pkey.clear_glossary_selections_at_depth(&app, -1);
                    }
                    return true;
                }
            }
            return false;
        }
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
            let mut st = b_panel_search.state.borrow_mut();
            st.panel.last_panel_scan.clear();
            (
                st.settings.excluded_glossaries.clone(),
                st.settings.dict_search_config(),
            )
        };
        let gen_id = b_panel_search.panel_epoch.fetch_add(1, Ordering::AcqRel) + 1;
        if query.trim().is_empty() {
            {
                let mut st = b_panel_search.state.borrow_mut();
                st.panel = PanelPopupState::default();
            }
            b_panel_search.cancel_panel_anki_check(&app);
            b_panel_search.clear_glossary_selections_at_depth(&app, -1);
            b_panel_search.sync_panel_popup_to_bridge(&app, true);
            pb.set_panel_searching(false);
            return;
        }
        pb.set_panel_searching(true);
        let _ = b_panel_search.panel_tx.send(PanelSearchRequest {
            gen_id,
            query: query.to_string(),
            kana_only,
            kanji_only: false,
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

    let b_panel_scan = bridge.clone();
    pb.on_panel_scan_query(move |query, char_idx, _x, _y| {
        let Some(app) = b_panel_scan.app_weak.upgrade() else {
            return;
        };
        b_panel_scan.ensure_fresh_db_and_config(&app, false);
        let start = char_idx.max(0) as usize;
        let suffix: String = query.chars().skip(start).collect();
        let trimmed = suffix.trim().to_owned();
        if trimmed.is_empty() {
            return;
        }
        let pb = app.global::<PopupBridge>();
        let kana_only = pb.get_panel_kana_only();
        let (excluded, search_cfg) = {
            let mut st = b_panel_scan.state.borrow_mut();
            if st.panel.last_panel_scan == trimmed {
                return;
            }
            st.panel.last_panel_scan = trimmed.clone();
            (
                st.settings.excluded_glossaries.clone(),
                st.settings.dict_search_config(),
            )
        };
        let gen_id = b_panel_scan.panel_epoch.fetch_add(1, Ordering::AcqRel) + 1;
        pb.set_panel_searching(true);
        let _ = b_panel_scan.panel_tx.send(PanelSearchRequest {
            gen_id,
            query: trimmed,
            kana_only,
            kanji_only: false,
            excluded,
            search_cfg,
        });
        b_panel_scan.kick_no_event_loop_pump();
    });

    let b_panel_add = bridge.clone();
    pb.on_panel_add_anki_note(move |row_zero_idx| {
        if let Some(app) = b_panel_add.app_weak.upgrade() {
            b_panel_add.add_panel_anki_note(&app, row_zero_idx, None);
        }
    });

    let b_panel_add_audio = bridge.clone();
    pb.on_panel_add_to_anki_with_audio(move |row_zero_idx, source_idx| {
        if let Some(app) = b_panel_add_audio.app_weak.upgrade() {
            b_panel_add_audio.add_panel_anki_note(&app, row_zero_idx, Some(source_idx));
        }
    });

    let b_panel_sel = bridge.clone();
    pb.on_panel_toggle_select_definition(move |zero_based_idx| {
        if let Some(app) = b_panel_sel.app_weak.upgrade() {
            b_panel_sel.toggle_panel_select_definition(&app, zero_based_idx);
        }
    });

    let b_panel_coll = bridge.clone();
    pb.on_panel_toggle_term_collapse(move |term_start_zero_idx| {
        if let Some(app) = b_panel_coll.app_weak.upgrade() {
            b_panel_coll.toggle_panel_term_collapse(&app, term_start_zero_idx);
        }
    });

    let b_panel_rae = bridge.clone();
    pb.on_panel_toggle_reading_as_expression(move |row_zero_idx| {
        if let Some(app) = b_panel_rae.app_weak.upgrade() {
            b_panel_rae.toggle_reading_as_expression(&app, AnkiCheckTarget::Panel, row_zero_idx);
        }
    });

    let b_panel_kanji = bridge.clone();
    pb.on_panel_lookup_kanji_char(move |headword, ch_str| {
        let Some(app) = b_panel_kanji.app_weak.upgrade() else {
            return;
        };
        b_panel_kanji.ensure_fresh_db_and_config(&app, false);
        let target: String = if !ch_str.trim().is_empty() {
            ch_str.trim().to_owned()
        } else {
            let mut chars = String::new();
            for ch in headword.chars() {
                if is_kanji_char(ch) && !chars.contains(ch) {
                    chars.push(ch);
                }
            }
            chars
        };
        if target.is_empty() {
            return;
        }
        let pb = app.global::<PopupBridge>();
        pb.set_panel_query(target.clone().into());
        let (excluded, search_cfg) = {
            let mut st = b_panel_kanji.state.borrow_mut();
            st.panel.last_panel_scan.clear();
            (
                st.settings.excluded_glossaries.clone(),
                st.settings.dict_search_config(),
            )
        };
        let gen_id = b_panel_kanji.panel_epoch.fetch_add(1, Ordering::AcqRel) + 1;
        pb.set_panel_searching(true);
        let _ = b_panel_kanji.panel_tx.send(PanelSearchRequest {
            gen_id,
            query: target,
            kana_only: false,
            kanji_only: true,
            excluded,
            search_cfg,
        });
        b_panel_kanji.kick_no_event_loop_pump();
    });
}

fn wire_subtitle_bridge(app: &SubomiApp, bridge: &Rc<RichUiBridge>) {
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
        if !subomi_ass::is_japanese_or_alnum_lookup_char(target.ch) && !mod_held {
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
    app: &SubomiApp,
    dictionary_db_path: &Path,
    config_dir: &Path,
    config: &ConfigController,
) {
    let settings = CachedConfigSettings::from_controller(config, config_dir);
    let middle_mouse_default = settings.middle_mouse_scan;
    let initial_max_recursion_depth = settings.max_recursion_depth;
    let initial_sub_style = settings.subtitle_style.clone();
    let initial_sub_bg = settings.subtitle_style.bg_rgba;
    let initial_anki_show_tag = settings.anki_show_tag;
    let initial_is_light = settings.is_light_theme;
    let initial_compact_glossaries = settings.compact_glossaries;
    let initial_compact_tags = settings.compact_tags;
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

    let (nested_tx, nested_rx) = mpsc::channel::<DictWorkerRequest>();
    let (nested_res_tx, raw_nested_res_rx) = mpsc::channel::<NestedScanResponse>();
    let nested_res_rx = Arc::new(Mutex::new(raw_nested_res_rx));
    let nested_epoch = Arc::new(AtomicU64::new(0));
    let (nav_res_tx, raw_nav_res_rx) = mpsc::channel::<NavLookupResponse>();
    let nav_res_rx = Arc::new(Mutex::new(raw_nav_res_rx));
    let nav_epoch = Arc::new(AtomicU64::new(0));
    spawn_nested_worker(
        dictionary_db_path.to_path_buf(),
        nested_rx,
        nested_res_tx,
        nav_res_tx,
        nested_epoch.clone(),
        nav_epoch.clone(),
        app.as_weak(),
    );

    let (anki_tx, anki_rx) = mpsc::channel::<AnkiPopupCheckRequest>();
    let (anki_res_tx, raw_anki_res_rx) = mpsc::channel::<AnkiPopupCheckResponse>();
    let anki_res_rx = Arc::new(Mutex::new(raw_anki_res_rx));
    let anki_nav_epoch = Arc::new(AtomicU64::new(0));
    let nested_anki_nav_epoch = Arc::new(AtomicU64::new(0));
    let panel_anki_nav_epoch = Arc::new(AtomicU64::new(0));
    let root_anki_in_flight = Arc::new(AtomicU64::new(0));
    let nested_anki_in_flight = Arc::new(AtomicU64::new(0));
    let panel_anki_in_flight = Arc::new(AtomicU64::new(0));
    let external_lookup_epoch: Arc<Mutex<Option<Arc<AtomicU64>>>> = Arc::new(Mutex::new(None));
    spawn_anki_worker(
        anki_rx,
        anki_res_tx,
        anki_nav_epoch.clone(),
        nested_anki_nav_epoch.clone(),
        panel_anki_nav_epoch.clone(),
        root_anki_in_flight.clone(),
        nested_anki_in_flight.clone(),
        panel_anki_in_flight.clone(),
        external_lookup_epoch.clone(),
        app.as_weak(),
    );

    let has_event_loop_proxy = slint::invoke_from_event_loop(|| {}).is_ok();
    let shared_font_collection = capture_shared_font_collection();
    let (glossary_render_res_tx, raw_glossary_render_res_rx) =
        mpsc::channel::<GlossaryPreRenderDone>();
    let glossary_render_res_rx = Arc::new(Mutex::new(raw_glossary_render_res_rx));
    let glossary_render_epoch = Arc::new(AtomicU64::new(0));
    let glossary_render_tx = if has_event_loop_proxy {
        let (tx, rx) = mpsc::channel::<GlossaryPreRenderBatch>();
        spawn_glossary_render_worker(
            rx,
            glossary_render_res_tx,
            glossary_render_epoch.clone(),
            app.as_weak(),
            shared_font_collection,
        );
        Some(tx)
    } else {
        drop(glossary_render_res_tx);
        None
    };

    let instance_id = NEXT_BRIDGE_INSTANCE_ID.fetch_add(1, Ordering::Relaxed);
    app.global::<PopupBridge>()
        .set_bridge_instance_id(instance_id);

    let initial_css_theme = if initial_is_light {
        CssTheme::Light
    } else {
        CssTheme::Dark
    };
    let stylesheets = StylesheetCache::new_with_theme(config_dir, initial_css_theme);
    let mut initial_theme_val = stylesheets
        .theme()
        .with_interface_font_fallbacks(stylesheets.overrides(), &settings.popup_fonts);
    initial_theme_val.compact_glossaries =
        initial_theme_val.compact_glossaries || initial_compact_glossaries;
    let initial_popup_theme = Arc::new(initial_theme_val);
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
            cached_theme_is_light: initial_is_light,
            cached_compact_glossaries: initial_compact_glossaries,
            cached_compact_tags: initial_compact_tags,
            last_fresh_check: Some(Instant::now()),
            rich_epoch: 1,
            row_extras: HashMap::new(),
            current_hits: Vec::new(),
            glossary_cache: GlossaryDocLruCache::new_root(),
            last_rendered_key: HashMap::new(),
            selections: BTreeMap::new(),
            active_drag_selection_key: None,
            root_scan_highlight: None,
            root_expanded_cards: HashSet::new(),
            root_row_anki_note_ids_i64: Vec::new(),
            root_row_anki_addable: Vec::new(),
            root_row_reading_as_expression: Vec::new(),
            root_term_kanji_statuses: HashMap::new(),
            root_term_kana_statuses: HashMap::new(),
            root_manual_reading_as_expr: HashMap::new(),
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
            panel: PanelPopupState::default(),
            pending_panel_anki_adds: VecDeque::new(),
            hovered_cluster: None,
            hovered_ass_target: None,
            pending_ass_scan: None,
            ass_controller: None,
            active_scan_utf16_range: None,
            pending_scan_utf16_start: None,
            pending_scan_sub_query: None,
            scan_timer: slint::Timer::default(),
            idle_shrink_timer: slint::Timer::default(),
            subtitle_token_layout_cache: SubtitleTokenLayoutCache::default(),
            pending_root_precomputed_meta: None,
            last_observed_solo_width_px: None,
            last_observed_col_width_px: None,
        }),
        panel_tx,
        panel_res_rx,
        panel_epoch,
        anki_tx,
        anki_res_rx,
        anki_nav_epoch,
        nested_anki_nav_epoch,
        panel_anki_nav_epoch,
        root_anki_in_flight,
        nested_anki_in_flight,
        panel_anki_in_flight,
        external_lookup_epoch,
        nested_tx,
        nested_res_rx,
        nested_epoch,
        nav_res_rx,
        nav_epoch,
        glossary_render_tx,
        glossary_render_res_rx,
        glossary_render_epoch,
        no_event_loop_pump: (!has_event_loop_proxy).then(slint::Timer::default),
    });

    register_active_rich_bridge(bridge.clone());

    app.set_is_light_theme(initial_is_light);
    let pb = app.global::<PopupBridge>();
    pb.set_rich_enabled(true);
    pb.set_panel_rich_enabled(true);
    pb.set_compact_glossaries(initial_compact_glossaries);
    pb.set_compact_tags(initial_compact_tags);
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
    sync_subtitle_overlay_style_to_bridge(app, &initial_sub_style);
    sb.set_middle_mouse_scan_enabled(middle_mouse_default);
    wire_subtitle_bridge(app, &bridge);
}

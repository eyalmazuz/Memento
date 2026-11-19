use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::atomic::AtomicI32;

use slint::{ComponentHandle, Model as _, SharedString, VecModel};

use crate::controller::dict::{SearchHit, TermPitchDetail};

use super::bridge::RichUiBridge;
use super::{
    DictResult, MementoApp, PitchGraph, PitchMora, PopupBridge, PopupFrequencyLine,
    PopupGlossaryBlock, PopupLayout, RichPitchGraph, RichPitchMora,
};

/// Resolve the system's fallback font metadata before media playback starts.
/// Fontconfig can spend hundreds of milliseconds sorting its font set when a
/// dictionary first introduces a symbol. Cache those family descriptions once;
/// keep the system's font choices and load glyphs only when they are needed.
pub fn prepare_text_fonts() {
    use slint::fontique_08::{
        fontique::{FallbackKey, GenericFamily, Script},
        shared_collection,
    };
    let mut collection = shared_collection();
    let mut families: Vec<_> = collection.generic_families(GenericFamily::Emoji).collect();
    for script in [*b"Hani", *b"Hira", *b"Kana", *b"Zyyy", *b"Latn"] {
        families.extend(
            collection.fallback_families(FallbackKey::new(Script::from_bytes(script), None)),
        );
    }
    if let Some(family) = collection.family_id("Noto Sans CJK JP") {
        families.push(family);
    }
    for family in families {
        let _ = collection.family(family);
    }
}

/// Bound the initial layout work, even for a term with many long definitions.
/// The full result model remains available for Anki and dictionary selection.
pub fn next_popup_batch_end(rows: &slint::ModelRc<DictResult>, start: usize) -> usize {
    let mut visible = 0;
    for index in start..rows.row_count() {
        if let Some(row) = rows.row_data(index) {
            if !row.collapsed || row.term_start {
                visible += 1;
            }
            if visible >= 2 || (row.term_end && visible > 0) {
                return index + 1;
            }
        }
    }
    rows.row_count()
}

pub(super) static NEXT_BRIDGE_INSTANCE_ID: AtomicI32 = AtomicI32::new(1);

thread_local! {
    static LAST_GROUPED_HITS: RefCell<Vec<SearchHit>> = const { RefCell::new(Vec::new()) };
    static ACTIVE_RICH_BRIDGES: RefCell<Vec<Rc<RichUiBridge>>> = const { RefCell::new(Vec::new()) };
    static SUPPRESS_FIRST_TERM_READY: Cell<bool> = const { Cell::new(false) };
}

pub(super) struct SuppressFirstTermReadyGuard {
    prev: bool,
}

impl Drop for SuppressFirstTermReadyGuard {
    fn drop(&mut self) {
        SUPPRESS_FIRST_TERM_READY.with(|flag| flag.set(self.prev));
    }
}

pub(super) fn suppress_first_term_ready_guard() -> SuppressFirstTermReadyGuard {
    let prev = SUPPRESS_FIRST_TERM_READY.with(|flag| flag.replace(true));
    SuppressFirstTermReadyGuard { prev }
}

pub(super) fn clone_last_grouped_hits() -> Vec<SearchHit> {
    LAST_GROUPED_HITS.with(|slot| slot.borrow().clone())
}

pub(super) fn set_last_grouped_hits(hits: Vec<SearchHit>) {
    LAST_GROUPED_HITS.with(|slot| {
        *slot.borrow_mut() = hits;
    });
}

pub(super) fn register_active_rich_bridge(bridge: Rc<RichUiBridge>) {
    ACTIVE_RICH_BRIDGES.with(|slot| {
        let mut bridges = slot.borrow_mut();
        bridges.retain(|b| b.app_weak.upgrade().is_some());
        bridges.push(bridge);
    });
}

pub(super) fn active_rich_bridges_for_app(app: &MementoApp) -> Vec<Rc<RichUiBridge>> {
    let app_bridge_id = app.global::<PopupBridge>().get_bridge_instance_id();
    if app_bridge_id <= 0 {
        return Vec::new();
    }
    ACTIVE_RICH_BRIDGES.with(|slot| {
        let Ok(mut bridges) = slot.try_borrow_mut() else {
            return slot
                .try_borrow()
                .map(|b| {
                    b.iter()
                        .filter(|br| br.instance_id == app_bridge_id)
                        .cloned()
                        .collect()
                })
                .unwrap_or_default();
        };
        bridges.retain(|bridge| bridge.app_weak.upgrade().is_some());
        bridges
            .iter()
            .filter(|bridge| bridge.instance_id == app_bridge_id)
            .cloned()
            .collect()
    })
}

pub(super) fn active_rich_bridge_for_app(app: &MementoApp) -> Option<Rc<RichUiBridge>> {
    let app_bridge_id = app.global::<PopupBridge>().get_bridge_instance_id();
    if app_bridge_id <= 0 {
        return None;
    }
    ACTIVE_RICH_BRIDGES.with(|slot| {
        let bridges = slot.try_borrow().ok()?;
        bridges
            .iter()
            .find(|b| b.instance_id == app_bridge_id && b.app_weak.upgrade().is_some())
            .cloned()
    })
}

pub(super) fn latest_active_rich_bridge() -> Option<Rc<RichUiBridge>> {
    ACTIVE_RICH_BRIDGES.with(|slot| {
        let bridges = slot.try_borrow().ok()?;
        bridges
            .iter()
            .rev()
            .find(|b| b.app_weak.upgrade().is_some())
            .cloned()
    })
}

pub fn is_kanji_char(character: char) -> bool {
    ('\u{3400}'..='\u{9fff}').contains(&character)
        || ('\u{f900}'..='\u{faff}').contains(&character)
        || ('\u{20000}'..='\u{3134f}').contains(&character)
}

pub(super) fn build_dictionary_result_rows_grouped(
    hits: &[SearchHit],
    excluded: &[String],
) -> (Vec<DictResult>, Vec<SearchHit>) {
    let mut grouped: Vec<&SearchHit> = Vec::with_capacity(hits.len());
    let mut terms: Vec<(&str, &str, bool)> = Vec::new();
    let mut by_term: HashMap<(&str, &str, bool), Vec<&SearchHit>> = HashMap::new();
    for hit in hits {
        let key = (hit.expression.as_str(), hit.reading.as_str(), hit.is_kanji);
        match by_term.entry(key) {
            std::collections::hash_map::Entry::Vacant(entry) => {
                terms.push(key);
                entry.insert(vec![hit]);
            }
            std::collections::hash_map::Entry::Occupied(mut entry) => entry.get_mut().push(hit),
        }
    }
    for key in terms {
        if let Some(rows) = by_term.remove(&key) {
            grouped.extend(rows);
        }
    }

    let grouped_owned: Vec<SearchHit> = grouped.iter().map(|hit| (*hit).clone()).collect();
    let rows = grouped
        .iter()
        .enumerate()
        .map(|(index, hit)| DictResult {
            index: (index + 1) as i32,
            expression: hit.expression.clone().into(),
            reading: hit.reading.clone().into(),
            pos_tags: hit.tags.replace('⭐', "★").into(),
            dict_name: hit.dict_name.clone().into(),
            glossary: hit.glossary.clone().into(),
            glossary_blocks: Rc::new(VecModel::from(
                hit.rich_glossary
                    .iter()
                    .map(|block| PopupGlossaryBlock {
                        text: block.text.clone().into(),
                        kind: block.kind,
                        href: block.href.clone().into(),
                    })
                    .collect::<Vec<_>>(),
            ))
            .into(),
            notes: hit.notes.clone().into(),
            example_ja: hit.example_ja.clone().into(),
            example_en: hit.example_en.clone().into(),
            selected: !excluded.iter().any(|name| name == &hit.dict_name),
            score: i32::try_from(hit.score).unwrap_or(if hit.score < 0 {
                i32::MIN
            } else {
                i32::MAX
            }),
            is_kanji: hit.is_kanji,
            has_kanji: hit.expression.chars().any(is_kanji_char),
            term_start: index == 0
                || grouped[index - 1].expression != hit.expression
                || grouped[index - 1].reading != hit.reading
                || grouped[index - 1].is_kanji != hit.is_kanji,
            term_end: index + 1 == grouped.len()
                || grouped[index + 1].expression != hit.expression
                || grouped[index + 1].reading != hit.reading
                || grouped[index + 1].is_kanji != hit.is_kanji,
            collapsed: false,
            pitch_graphs: Default::default(),
            frequencies: Default::default(),
        })
        .collect();
    (rows, grouped_owned)
}

pub fn dictionary_result_rows(hits: &[SearchHit], excluded: &[String]) -> Vec<DictResult> {
    let (rows, grouped_owned) = build_dictionary_result_rows_grouped(hits, excluded);
    set_last_grouped_hits(grouped_owned);
    rows
}

/// Maximum number of definition rows eagerly mounted into `popup_results` on initial lookup.
/// Keeps first-frame rasterization bounded on broad lookups (e.g. 300+ hits) while keeping
/// typical 1..=24 row lookups complete on frame 1; remaining rows stream on scroll via `needs-more`.
const INITIAL_POPUP_BATCH_MIN_TERM_END: usize = 16;
const INITIAL_POPUP_BATCH_MAX_ROWS: usize = 24;

fn initial_popup_batch_end(rows: &slint::ModelRc<DictResult>) -> usize {
    let total = rows.row_count();
    if total <= INITIAL_POPUP_BATCH_MAX_ROWS {
        return total;
    }
    let mut visible = 0usize;
    for index in 0..total {
        if let Some(row) = rows.row_data(index) {
            if !row.collapsed || row.term_start {
                visible += 1;
            }
            if visible >= INITIAL_POPUP_BATCH_MAX_ROWS
                || (visible >= INITIAL_POPUP_BATCH_MIN_TERM_END && row.term_end)
            {
                return index + 1;
            }
        }
    }
    total
}

pub(super) fn popup_rows_through(
    rows: &slint::ModelRc<DictResult>,
    count: usize,
) -> Vec<DictResult> {
    (0..count.min(rows.row_count()))
        .filter_map(|index| rows.row_data(index))
        .filter(|row| !row.collapsed || row.term_start)
        .collect()
}

pub fn set_dictionary_rows(app: &MementoApp, rows: Vec<DictResult>) {
    let full = Rc::new(VecModel::from(rows.clone()));
    app.set_dict_results(full.into());
    let full = app.get_dict_results();
    let initial_loaded = initial_popup_batch_end(&full);
    app.set_popup_loaded_count(initial_loaded as i32);
    app.set_popup_results(
        Rc::new(VecModel::from(popup_rows_through(&full, initial_loaded))).into(),
    );

    let pb = app.global::<PopupBridge>();
    pb.set_selected_glossary_text("".into());
    pb.set_tooltip_visible(false);
    pb.set_last_added_was_nested(false);
    pb.set_row_anki_duplicates(Rc::new(VecModel::from(Vec::<bool>::new())).into());
    pb.set_row_anki_note_ids(Rc::new(VecModel::from(Vec::<i32>::new())).into());
    pb.set_row_anki_note_id_strings(Rc::new(VecModel::from(Vec::<SharedString>::new())).into());
    pb.set_row_anki_browse_queries(Rc::new(VecModel::from(Vec::<SharedString>::new())).into());
    pb.set_row_anki_tags(Rc::new(VecModel::from(Vec::<SharedString>::new())).into());
    pb.set_popup_revision(pb.get_popup_revision().wrapping_add(1));

    // Clone matching bridges out before invoking callbacks so `ACTIVE_RICH_BRIDGES`
    // is never held borrowed across re-entrant UI/Slint mutations.
    let bridges = active_rich_bridges_for_app(app);
    for bridge in bridges {
        bridge.on_dictionary_rows_updated(app, &rows);
    }

    if !SUPPRESS_FIRST_TERM_READY.with(|flag| flag.get()) {
        if let Some(first_term) = rows
            .iter()
            .find(|r| !r.is_kanji && !r.expression.is_empty())
        {
            pb.invoke_first_term_ready(
                first_term.expression.clone(),
                first_term.reading.clone(),
            );
        }
    }
}

fn refresh_popup_rows(app: &MementoApp) {
    let full = app.get_dict_results();
    let visible = popup_rows_through(&full, app.get_popup_loaded_count().max(0) as usize);
    let popup = app.get_popup_results();
    if let Some(model) = popup.as_any().downcast_ref::<VecModel<DictResult>>() {
        model.set_vec(visible);
    } else {
        app.set_popup_results(Rc::new(VecModel::from(visible)).into());
    }
}

pub fn popup_pitch_graphs(graphs: Vec<crate::controller::dict::TermPitchGraph>) -> Vec<PitchGraph> {
    graphs
        .into_iter()
        .map(|graph| {
            let len = graph.mora.len();
            let mora: Vec<PitchMora> = graph
                .mora
                .into_iter()
                .enumerate()
                .map(|(index, text)| {
                    let high = if graph.position == 1 {
                        index == 0
                    } else if graph.position == 0 {
                        index > 0
                    } else {
                        index > 0 && index < graph.position as usize
                    };
                    PitchMora {
                        text: text.into(),
                        high,
                        rises_after: index == 0 && graph.position != 1 && len > 1,
                        falls_after: index + 1 == graph.position as usize && index + 1 < len,
                    }
                })
                .collect();
            PitchGraph {
                dictionary: graph.dictionary.into(),
                position: graph.position,
                mora: Rc::new(VecModel::from(mora)).into(),
            }
        })
        .collect()
}

/// Convert detailed pitch entries (with 1-based `nasal` and `devoice` mora indices) into
/// Slint `RichPitchGraph` rows.
pub fn rich_pitch_graphs_from_details(details: &[TermPitchDetail]) -> Vec<RichPitchGraph> {
    details
        .iter()
        .map(|detail| {
            let len = detail.mora.len();
            let mora: Vec<RichPitchMora> = detail
                .mora
                .iter()
                .enumerate()
                .map(|(index, text)| {
                    let high = if detail.position == 1 {
                        index == 0
                    } else if detail.position == 0 {
                        index > 0
                    } else {
                        index > 0 && index < detail.position as usize
                    };
                    let one_based = index + 1;
                    RichPitchMora {
                        text: text.clone().into(),
                        high,
                        rises_after: index == 0 && detail.position != 1 && len > 1,
                        falls_after: index + 1 == detail.position as usize && index + 1 < len,
                        nasal: detail.nasal.contains(&one_based),
                        devoice: detail.devoice.contains(&one_based),
                    }
                })
                .collect();
            RichPitchGraph {
                dictionary: detail.dictionary.clone().into(),
                position: detail.position,
                mora: Rc::new(VecModel::from(mora)).into(),
                tags: detail.tags.join(", ").into(),
            }
        })
        .collect()
}

/// Splits a whitespace- or comma-separated Anki card tag list into deduplicated pills (#346).
pub fn split_anki_tags_str(raw: &str) -> Vec<SharedString> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for part in raw.split(|ch: char| ch.is_whitespace() || ch == ',') {
        let trimmed = part.trim();
        if !trimmed.is_empty() && seen.insert(trimmed.to_owned()) {
            out.push(SharedString::from(trimmed));
        }
    }
    out
}

/// Shared popup actions used by the desktop window and the GL interaction test.
pub fn install_popup_navigation(app: &MementoApp) {
    app.global::<PopupBridge>()
        .on_split_anki_tags(|raw| Rc::new(VecModel::from(split_anki_tags_str(raw.as_str()))).into());
    app.global::<PopupLayout>()
        .on_frequency_lines(|frequencies, available_width| {
            let available_width = available_width.max(1.0);
            let mut lines = Vec::new();
            let mut entries = Vec::new();
            let mut line_width = 0.0;
            for frequency in frequencies.iter() {
                let label = format!("{} {}", frequency.dictionary, frequency.value);
                let width = label
                    .chars()
                    .map(|ch| if ch.is_ascii() { 6.0 } else { 10.0 })
                    .sum::<f32>()
                    + 10.0;
                let gap = if entries.is_empty() { 0.0 } else { 4.0 };
                if !entries.is_empty() && line_width + gap + width > available_width {
                    lines.push(PopupFrequencyLine {
                        entries: Rc::new(VecModel::from(std::mem::take(&mut entries))).into(),
                    });
                    line_width = 0.0;
                }
                if !entries.is_empty() {
                    line_width += 4.0;
                }
                entries.push(frequency);
                line_width += width;
            }
            if !entries.is_empty() {
                lines.push(PopupFrequencyLine {
                    entries: Rc::new(VecModel::from(entries)).into(),
                });
            }
            Rc::new(VecModel::from(lines)).into()
        });
    let selection_app = app.as_weak();
    app.on_toggle_definition_selection(move |index| {
        let Some(app) = selection_app.upgrade() else {
            return;
        };
        if index < 0 {
            return;
        }
        let rows = app.get_dict_results();
        if let Some(mut row) = rows.row_data(index as usize) {
            row.selected = !row.selected;
            rows.set_row_data(index as usize, row.clone());
            let popup = app.get_popup_results();
            if let Some(popup_index) = (0..popup.row_count()).find(|&popup_index| {
                popup
                    .row_data(popup_index)
                    .is_some_and(|visible| visible.index == row.index)
            }) {
                popup.set_row_data(popup_index, row);
            }
        }
    });
    let collapse_app = app.as_weak();
    app.on_toggle_term_collapse(move |index| {
        let Some(app) = collapse_app.upgrade() else {
            return;
        };
        if index < 0 {
            return;
        }
        let rows = app.get_dict_results();
        let Some(clicked) = rows.row_data(index as usize) else {
            return;
        };
        let collapsed = !clicked.collapsed;
        for row_index in 0..rows.row_count() {
            if let Some(mut row) = rows.row_data(row_index) {
                if row.expression == clicked.expression
                    && row.reading == clicked.reading
                    && row.is_kanji == clicked.is_kanji
                {
                    row.collapsed = collapsed;
                    rows.set_row_data(row_index, row);
                }
            }
        }
        refresh_popup_rows(&app);
    });

    let more_app = app.as_weak();
    let more_pending = Rc::new(Cell::new(false));
    app.on_load_more_definition_results(move |force| {
        if more_pending.replace(true) {
            return;
        }
        let Some(app) = more_app.upgrade() else {
            more_pending.set(false);
            return;
        };
        let expected_rows = app.get_dict_results();
        let weak = app.as_weak();
        let pending = more_pending.clone();
        slint::Timer::single_shot(std::time::Duration::ZERO, move || {
            pending.set(false);
            let Some(app) = weak.upgrade() else { return };
            if app.get_dict_results() != expected_rows {
                return;
            }
            if !force && !app.get_popup_needs_more() {
                return;
            }
            let full = app.get_dict_results();
            let start = app.get_popup_loaded_count().max(0) as usize;
            if start >= full.row_count() {
                return;
            }
            let end = if force {
                full.row_count()
            } else {
                next_popup_batch_end(&full, start)
            };
            let popup = app.get_popup_results();
            if let Some(model) = popup.as_any().downcast_ref::<VecModel<DictResult>>() {
                for index in start..end {
                    if let Some(row) = full
                        .row_data(index)
                        .filter(|row| !row.collapsed || row.term_start)
                    {
                        model.push(row);
                    }
                }
                app.set_popup_loaded_count(end as i32);
            }
        });
    });
}

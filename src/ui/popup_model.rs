use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::atomic::AtomicI32;

use slint::{ComponentHandle, Model as _, SharedString, VecModel};

use crate::controller::dict::{PitchPosition, SearchHit, TermFrequency, TermPitchDetail};

use super::bridge::RichUiBridge;
use super::tags::compact_tag_names_across_senses;
use super::{
    DictResult, SubomiApp, PitchGraph, PitchMora, PopupBridge, PopupFrequency, PopupFrequencyLine,
    PopupGlossaryBlock, PopupLayout, RichPitchGraph, RichPitchMora,
};

fn warm_font_collection(mut collection: slint::fontique_08::fontique::Collection) {
    use slint::fontique_08::fontique::{FallbackKey, GenericFamily, Script};

    let mut seen = HashSet::new();
    let mut families = Vec::new();
    let mut push_unique = |id| {
        if seen.insert(id) {
            families.push(id);
        }
    };
    for id in collection.generic_families(GenericFamily::Emoji) {
        push_unique(id);
    }
    for script in [*b"Hani", *b"Hira", *b"Kana", *b"Zyyy", *b"Latn"] {
        for id in collection.fallback_families(FallbackKey::new(Script::from_bytes(script), None))
        {
            push_unique(id);
        }
    }
    if let Some(family) = collection.family_id("Noto Sans CJK JP") {
        push_unique(family);
    }
    for family in families {
        let _ = collection.family(family);
    }
}

/// Resolve the system's fallback font metadata before media playback starts.
/// Fontconfig can spend hundreds of milliseconds sorting its font set when a
/// dictionary first introduces a symbol. Cache those family descriptions once;
/// keep the system's font choices and load glyphs only when they are needed.
pub fn prepare_text_fonts() {
    if let Some(collection) = super::text_engine::capture_shared_font_collection() {
        warm_font_collection(collection);
    } else {
        warm_font_collection(slint::fontique_08::shared_collection());
    }
}

/// Non-blocking startup variant of [`prepare_text_fonts`]: captures Slint's shared
/// `fontique::Collection` on the UI thread and warms fallback family descriptions on
/// a background thread so window creation, config/SQLite init, and libmpv startup do
/// not stall behind Fontconfig (~200-250 ms).
pub fn spawn_prepare_text_fonts() {
    if let Some(collection) = super::text_engine::capture_shared_font_collection() {
        let _ = std::thread::Builder::new()
            .name("subomi-font-warmup".into())
            .spawn(move || warm_font_collection(collection));
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

pub(super) fn active_rich_bridges_for_app(app: &SubomiApp) -> Vec<Rc<RichUiBridge>> {
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

pub(super) fn active_rich_bridge_for_app(app: &SubomiApp) -> Option<Rc<RichUiBridge>> {
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

/// Returns `true` if `character` is a CJK ideograph (including Extensions A–J,
/// covering Extension H `U+31350..=U+323AF` and Extension J `U+323B0..=U+3347F`,
/// Yomitan PR #2504).
pub fn is_kanji_char(character: char) -> bool {
    subomi_dict::normalize::is_cjk_ideograph(character)
}

#[allow(dead_code)]
fn is_headword_kanji_char(character: char) -> bool {
    is_kanji_char(character) || matches!(character, '\u{3005}' | '\u{3006}' | '\u{3007}')
}

/// Formats a plain multi-line glossary string into compact inline ` | `-separated text
/// (H5.3 / Subomi `#220`).
pub fn compact_glossary_inline_text(glossary: &str) -> String {
    let parts: Vec<&str> = glossary
        .lines()
        .map(|line| line.trim().trim_start_matches(['◦', '•']).trim())
        .filter(|s| !s.is_empty())
        .collect();
    parts.join(" | ")
}

pub(super) fn build_dictionary_result_rows_grouped(
    hits: &[SearchHit],
    excluded: &[String],
) -> (Vec<DictResult>, Vec<SearchHit>) {
    build_dictionary_result_rows_grouped_with_options(hits, excluded, None, false, false)
}

/// Groups search hits by `(expression, reading, is_kanji)`, accumulates `term.score` across
/// dictionaries per group, optionally ranks `expression == cloze_body` ahead of reading-only
/// matches, and supports Yomitan-aligned `compact_glossaries` and `compact_tags` options.
pub(super) fn build_dictionary_result_rows_grouped_with_options(
    hits: &[SearchHit],
    excluded: &[String],
    cloze_body: Option<&str>,
    compact_glossaries: bool,
    compact_tags: bool,
) -> (Vec<DictResult>, Vec<SearchHit>) {
    struct TermGroup<'a> {
        key: (&'a str, &'a str, bool),
        hits: Vec<&'a SearchHit>,
        accumulated_score: i64,
        first_order: usize,
    }

    let mut groups: Vec<TermGroup<'_>> = Vec::new();
    let mut by_term: HashMap<(&str, &str, bool), usize> = HashMap::new();
    for (order_idx, hit) in hits.iter().enumerate() {
        let key = (hit.expression.as_str(), hit.reading.as_str(), hit.is_kanji);
        if let Some(&group_idx) = by_term.get(&key) {
            let group = &mut groups[group_idx];
            group.accumulated_score = group.accumulated_score.saturating_add(hit.score);
            group.hits.push(hit);
        } else {
            let group_idx = groups.len();
            by_term.insert(key, group_idx);
            groups.push(TermGroup {
                key,
                hits: vec![hit],
                accumulated_score: hit.score,
                first_order: order_idx,
            });
        }
    }

    if let Some(body) = cloze_body.map(str::trim).filter(|s| !s.is_empty()) {
        groups.sort_by(|a, b| {
            let a_is_kanji = a.key.2;
            let b_is_kanji = b.key.2;
            let a_expr_eq = a.key.0 == body;
            let b_expr_eq = b.key.0 == body;
            a_is_kanji
                .cmp(&b_is_kanji)
                .then_with(|| b_expr_eq.cmp(&a_expr_eq))
                .then_with(|| b.accumulated_score.cmp(&a.accumulated_score))
                .then_with(|| a.first_order.cmp(&b.first_order))
        });
    } else {
        // Keep kanji footer groups cleanly below non-kanji term groups while preserving
        // upstream longest-prefix-first term group order.
        groups.sort_by(|a, b| a.key.2.cmp(&b.key.2).then_with(|| a.first_order.cmp(&b.first_order)));
    }

    let mut grouped: Vec<&SearchHit> = Vec::with_capacity(hits.len());
    let mut group_scores_per_row: Vec<i64> = Vec::with_capacity(hits.len());
    let mut compact_pos_tags_per_row: Vec<String> = Vec::with_capacity(hits.len());

    for group in groups {
        let sense_tag_refs: Vec<&str> = group.hits.iter().map(|h| h.tags.as_str()).collect();
        let formatted_tags = compact_tag_names_across_senses(&sense_tag_refs, compact_tags);
        for (hit, tag_str) in group.hits.into_iter().zip(formatted_tags) {
            grouped.push(hit);
            group_scores_per_row.push(group.accumulated_score);
            compact_pos_tags_per_row.push(tag_str);
        }
    }

    let grouped_owned: Vec<SearchHit> = grouped.iter().map(|hit| (*hit).clone()).collect();
    let rows = grouped
        .iter()
        .enumerate()
        .map(|(index, hit)| {
            let score_val = group_scores_per_row[index];
            let glossary_text = if compact_glossaries && !hit.is_kanji {
                compact_glossary_inline_text(&hit.glossary)
            } else {
                hit.glossary.clone()
            };
            let pos_tags = if compact_tags {
                compact_pos_tags_per_row[index].clone()
            } else {
                hit.tags.replace('⭐', "★")
            };
            DictResult {
                index: (index + 1) as i32,
                expression: hit.expression.clone().into(),
                reading: hit.reading.clone().into(),
                pos_tags: pos_tags.into(),
                dict_name: hit.dict_name.clone().into(),
                glossary: glossary_text.into(),
                glossary_blocks: if hit.rich_glossary.is_empty() {
                    Default::default()
                } else {
                    Rc::new(VecModel::from(
                        hit.rich_glossary
                            .iter()
                            .map(|block| PopupGlossaryBlock {
                                text: block.text.clone().into(),
                                kind: block.kind,
                                href: block.href.clone().into(),
                            })
                            .collect::<Vec<_>>(),
                    ))
                    .into()
                },
                notes: hit.notes.clone().into(),
                example_ja: hit.example_ja.clone().into(),
                example_en: hit.example_en.clone().into(),
                selected: !excluded.iter().any(|name| name == &hit.dict_name),
                score: i32::try_from(score_val).unwrap_or(if score_val < 0 {
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
            }
        })
        .collect();
    (rows, grouped_owned)
}

pub fn dictionary_result_rows(hits: &[SearchHit], excluded: &[String]) -> Vec<DictResult> {
    let (rows, grouped_owned) = build_dictionary_result_rows_grouped(hits, excluded);
    set_last_grouped_hits(grouped_owned);
    rows
}

/// Additive options-aware wrapper around [`dictionary_result_rows`] supporting cloze-body
/// headword ranking, Yomitan-aligned `compact_glossaries`, and `compact_tags`.
#[allow(dead_code)]
pub fn dictionary_result_rows_with_options(
    hits: &[SearchHit],
    excluded: &[String],
    cloze_body: Option<&str>,
    compact_glossaries: bool,
    compact_tags: bool,
) -> Vec<DictResult> {
    let (rows, grouped_owned) = build_dictionary_result_rows_grouped_with_options(
        hits,
        excluded,
        cloze_body,
        compact_glossaries,
        compact_tags,
    );
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

pub fn set_dictionary_rows(app: &SubomiApp, rows: Vec<DictResult>) {
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

fn refresh_popup_rows(app: &SubomiApp) {
    let full = app.get_dict_results();
    let visible = popup_rows_through(&full, app.get_popup_loaded_count().max(0) as usize);
    let popup = app.get_popup_results();
    if let Some(model) = popup.as_any().downcast_ref::<VecModel<DictResult>>() {
        model.set_vec(visible);
    } else {
        app.set_popup_results(Rc::new(VecModel::from(visible)).into());
    }
}

/// Resolves per-mora high/low levels and following particle pitch `(mora_highs, particle_high)`
/// for either an exact `H`/`L` string pitch pattern (Yomitan PR #2055) or a numeric downstep.
#[must_use]
pub fn resolve_pitch_mora_levels(
    mora_len: usize,
    position: i32,
    pattern: Option<&str>,
) -> (Vec<bool>, bool) {
    if mora_len == 0 {
        return (Vec::new(), false);
    }
    if let Some(pat) = pattern {
        if PitchPosition::is_valid_pattern(pat) {
            let levels = PitchPosition::Pattern(pat.to_owned()).to_levels(mora_len);
            if levels.len() > mora_len {
                return (levels[..mora_len].to_vec(), levels[mora_len]);
            }
            if !levels.is_empty() {
                let mut mora_highs = Vec::with_capacity(mora_len);
                mora_highs.extend_from_slice(&levels);
                let tail = *levels.last().unwrap_or(&false);
                mora_highs.resize(mora_len, tail);
                return (mora_highs, tail);
            }
        }
    }

    let mut mora_highs = vec![false; mora_len];
    let particle_high = if position == 0 {
        for h in mora_highs.iter_mut().skip(1) {
            *h = true;
        }
        true
    } else if position == 1 {
        mora_highs[0] = true;
        false
    } else if position > 1 {
        let limit = (position as usize).min(mora_len);
        for h in mora_highs.iter_mut().take(limit).skip(1) {
            *h = true;
        }
        false
    } else {
        false
    };
    (mora_highs, particle_high)
}

/// Formats a pitch badge label (`"[2]"` or `"[LHHL]"`) from numeric downstep and optional
/// exact string pattern.
#[allow(dead_code)]
#[must_use]
pub fn popup_pitch_badge_text(position: i32, pattern: Option<&str>) -> String {
    if let Some(pat) = pattern {
        let trimmed = pat.trim();
        if !trimmed.is_empty() {
            return format!("[{}]", trimmed.to_ascii_uppercase());
        }
    }
    format!("[{position}]")
}

/// A single SVG/vector point on a pitch accent graph (`x` in `25 + 50*i`, `y` in `{25, 75}`).
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PitchGraphPoint {
    pub x: f32,
    pub y: f32,
    pub high: bool,
    pub is_particle: bool,
    pub downstep: bool,
}

/// Full popup pitch graph geometry & notation supporting both integer downsteps and exact
/// `H`/`L` string pitch patterns (`"LHH"`, `"HLL"`, `"LHHL"`, `"HLHL"`, etc.).
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq)]
pub struct PopupPitchPatternData {
    pub dictionary: String,
    pub position: i32,
    pub pattern: Option<String>,
    pub badge_text: String,
    pub mora_highs: Vec<bool>,
    pub particle_high: bool,
    pub points: Vec<PitchGraphPoint>,
    pub svg: Option<String>,
    pub compact_notation: String,
}

/// Generates an inline SVG pitch graph for `detail` (handling both integer downsteps and
/// exact `H`/`L` string patterns).
#[allow(dead_code)]
#[must_use]
pub fn pitch_graph_svg_for_detail(
    detail: &TermPitchDetail,
    fill: &str,
    stroke: &str,
) -> Option<String> {
    if detail.mora.is_empty() {
        return None;
    }
    let spec = if let Some(ref pat) = detail.pattern {
        subomi_anki::note::PitchSpec::Pattern(pat.as_str())
    } else {
        let pos = u8::try_from(detail.position).ok()?;
        subomi_anki::note::PitchSpec::Downstep(pos)
    };
    subomi_anki::note::generate_pitch_graph_svg(detail.mora.len(), spec, fill, stroke)
}

/// Formats a single `TermPitchDetail` into compact inline notation (e.g. `たべ↓る [2]` or
/// `おとこ↓ [LHHL]`).
#[allow(dead_code)]
#[must_use]
pub fn format_single_compact_pitch(detail: &TermPitchDetail) -> String {
    let (mora_highs, particle_high) =
        resolve_pitch_mora_levels(detail.mora.len(), detail.position, detail.pattern.as_deref());
    let mut reading_marked = String::new();
    for (idx, mora) in detail.mora.iter().enumerate() {
        reading_marked.push_str(mora);
        let high = mora_highs.get(idx).copied().unwrap_or(false);
        let next_high = if idx + 1 < detail.mora.len() {
            mora_highs[idx + 1]
        } else {
            particle_high
        };
        if high && !next_high {
            reading_marked.push('↓');
        }
    }
    let badge = popup_pitch_badge_text(detail.position, detail.pattern.as_deref());
    if reading_marked.is_empty() {
        badge
    } else {
        format!("{reading_marked} {badge}")
    }
}

/// Builds complete L/H mora levels, SVG/vector pitch graph points, SVG markup, badge text,
/// and compact notation for a `TermPitchDetail`.
#[allow(dead_code)]
#[must_use]
pub fn popup_pitch_pattern_data(detail: &TermPitchDetail) -> PopupPitchPatternData {
    let n = detail.mora.len();
    let (mora_highs, particle_high) =
        resolve_pitch_mora_levels(n, detail.position, detail.pattern.as_deref());
    let mut points = Vec::with_capacity(if n > 0 { n + 1 } else { 0 });
    for i in 0..n {
        let high = mora_highs[i];
        let next_high = if i + 1 < n {
            mora_highs[i + 1]
        } else {
            particle_high
        };
        points.push(PitchGraphPoint {
            x: 25.0 + 50.0 * (i as f32),
            y: if high { 25.0 } else { 75.0 },
            high,
            is_particle: false,
            downstep: high && !next_high,
        });
    }
    if n > 0 {
        points.push(PitchGraphPoint {
            x: 25.0 + 50.0 * (n as f32),
            y: if particle_high { 25.0 } else { 75.0 },
            high: particle_high,
            is_particle: true,
            downstep: false,
        });
    }
    let badge_text = popup_pitch_badge_text(detail.position, detail.pattern.as_deref());
    let svg = pitch_graph_svg_for_detail(
        detail,
        subomi_anki::note::DEFAULT_PITCH_GRAPH_FILL,
        subomi_anki::note::DEFAULT_PITCH_GRAPH_STROKE,
    );
    let compact_notation = format_single_compact_pitch(detail);
    PopupPitchPatternData {
        dictionary: detail.dictionary.clone(),
        position: detail.position,
        pattern: detail.pattern.clone(),
        badge_text,
        mora_highs,
        particle_high,
        points,
        svg,
        compact_notation,
    }
}

/// Compact per-dictionary pitch group joining multiple accents with ` | ` (H5.3 / Subomi `#220`).
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactPitchGroup {
    pub dictionary: String,
    pub notations: String,
    pub badges: String,
}

/// Formats a list of pitch details into ` | `-separated compact pitch notation.
#[allow(dead_code)]
#[must_use]
pub fn format_compact_pitches(details: &[TermPitchDetail]) -> String {
    let mut parts = Vec::with_capacity(details.len());
    for detail in details {
        let s = format_single_compact_pitch(detail);
        if !s.is_empty() && !parts.contains(&s) {
            parts.push(s);
        }
    }
    parts.join(" | ")
}

/// Groups pitch details by dictionary (preserving encounter order) and joins multiple pitch
/// notations/badges within each dictionary with ` | ` for compact popup layout.
#[allow(dead_code)]
#[must_use]
pub fn compact_pitch_groups(details: &[TermPitchDetail]) -> Vec<CompactPitchGroup> {
    let mut groups: Vec<(String, Vec<String>, Vec<String>)> = Vec::new();
    let mut index_by_dict: HashMap<&str, usize> = HashMap::new();
    for detail in details {
        let notation = format_single_compact_pitch(detail);
        let badge = popup_pitch_badge_text(detail.position, detail.pattern.as_deref());
        if let Some(&idx) = index_by_dict.get(detail.dictionary.as_str()) {
            if !groups[idx].1.contains(&notation) {
                groups[idx].1.push(notation);
            }
            if !groups[idx].2.contains(&badge) {
                groups[idx].2.push(badge);
            }
        } else {
            let idx = groups.len();
            index_by_dict.insert(detail.dictionary.as_str(), idx);
            groups.push((detail.dictionary.clone(), vec![notation], vec![badge]));
        }
    }
    groups
        .into_iter()
        .map(|(dictionary, notations, badges)| CompactPitchGroup {
            dictionary,
            notations: notations.join(" | "),
            badges: badges.join(" | "),
        })
        .collect()
}

pub fn popup_pitch_graphs(graphs: Vec<crate::controller::dict::TermPitchGraph>) -> Vec<PitchGraph> {
    graphs
        .into_iter()
        .map(|graph| {
            let len = graph.mora.len();
            let (mora_highs, particle_high) = resolve_pitch_mora_levels(len, graph.position, None);
            let mora: Vec<PitchMora> = graph
                .mora
                .into_iter()
                .enumerate()
                .map(|(index, text)| {
                    let high = mora_highs[index];
                    let next_high = if index + 1 < len {
                        mora_highs[index + 1]
                    } else {
                        particle_high
                    };
                    PitchMora {
                        text: text.into(),
                        high,
                        rises_after: index + 1 < len && !high && next_high,
                        falls_after: high && !next_high,
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

/// Convert detailed pitch entries (with 1-based `nasal`/`devoice` mora indices and optional
/// exact `H`/`L` string patterns) into Slint `RichPitchGraph` rows.
pub fn rich_pitch_graphs_from_details(details: &[TermPitchDetail]) -> Vec<RichPitchGraph> {
    details
        .iter()
        .map(|detail| {
            let len = detail.mora.len();
            let (mora_highs, particle_high) =
                resolve_pitch_mora_levels(len, detail.position, detail.pattern.as_deref());
            let mora: Vec<RichPitchMora> = detail
                .mora
                .iter()
                .enumerate()
                .map(|(index, text)| {
                    let high = mora_highs[index];
                    let next_high = if index + 1 < len {
                        mora_highs[index + 1]
                    } else {
                        particle_high
                    };
                    let one_based = index + 1;
                    RichPitchMora {
                        text: text.clone().into(),
                        high,
                        rises_after: index + 1 < len && !high && next_high,
                        falls_after: high && !next_high,
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

/// Compact frequency badge descriptor (`dcd58c3` / `dc2c3eb`) omitting repeated dictionary
/// names on consecutive/grouped frequency badges from the same dictionary while retaining
/// `dictionary` for CSS `[data-frequency="..."]` styling.
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactFrequencyBadge {
    pub dictionary: String,
    pub value: String,
    pub show_dictionary: bool,
    pub display_text: String,
}

/// Groups frequency entries by dictionary (preserving first-seen dictionary order and
/// deduplicating identical `(dictionary, value)` pairs) and omits repeated dictionary names
/// on subsequent badges from the same dictionary.
#[allow(dead_code)]
#[must_use]
pub fn compact_frequency_badges(frequencies: &[TermFrequency]) -> Vec<CompactFrequencyBadge> {
    let mut dict_order: Vec<&str> = Vec::new();
    let mut by_dict: HashMap<&str, Vec<&str>> = HashMap::new();
    for entry in frequencies {
        let dict = entry.dictionary.as_str();
        let val = entry.value.as_str();
        if val.is_empty() {
            continue;
        }
        let values = by_dict.entry(dict).or_insert_with(|| {
            dict_order.push(dict);
            Vec::new()
        });
        if !values.contains(&val) {
            values.push(val);
        }
    }

    let mut out = Vec::new();
    for dict in dict_order {
        let Some(values) = by_dict.remove(dict) else {
            continue;
        };
        for (idx, val) in values.into_iter().enumerate() {
            let show_dictionary = idx == 0 && !dict.is_empty();
            let display_text = if show_dictionary {
                format!("{dict} {val}")
            } else {
                val.to_owned()
            };
            out.push(CompactFrequencyBadge {
                dictionary: dict.to_owned(),
                value: val.to_owned(),
                show_dictionary,
                display_text,
            });
        }
    }
    out
}

/// Same as [`compact_frequency_badges`] accepting Slint `PopupFrequency` slices.
#[allow(dead_code)]
#[must_use]
pub fn compact_frequency_badges_from_popup(
    frequencies: &[PopupFrequency],
) -> Vec<CompactFrequencyBadge> {
    let owned: Vec<TermFrequency> = frequencies
        .iter()
        .map(|f| TermFrequency {
            dictionary: f.dictionary.to_string(),
            value: f.value.to_string(),
        })
        .collect();
    compact_frequency_badges(&owned)
}

/// Merges grouped frequency entries from the same dictionary into a single `TermFrequency`
/// badge whose `value` joins distinct ranks with `", "` (e.g. `JPDB 2058, 86009㋕`).
#[allow(dead_code)]
#[must_use]
pub fn compact_frequency_groups(frequencies: &[TermFrequency]) -> Vec<TermFrequency> {
    let mut dict_order: Vec<&str> = Vec::new();
    let mut by_dict: HashMap<&str, Vec<&str>> = HashMap::new();
    for entry in frequencies {
        let dict = entry.dictionary.as_str();
        let val = entry.value.as_str();
        if val.is_empty() {
            continue;
        }
        let values = by_dict.entry(dict).or_insert_with(|| {
            dict_order.push(dict);
            Vec::new()
        });
        if !values.contains(&val) {
            values.push(val);
        }
    }
    dict_order
        .into_iter()
        .filter_map(|dict| {
            let values = by_dict.remove(dict)?;
            Some(TermFrequency {
                dictionary: dict.to_owned(),
                value: values.join(", "),
            })
        })
        .collect()
}

/// Wraps compacted frequency badges into horizontal lines using `badge.display_text` width.
#[allow(dead_code)]
#[must_use]
pub fn compact_frequency_lines(
    badges: &[CompactFrequencyBadge],
    available_width: f32,
) -> Vec<Vec<CompactFrequencyBadge>> {
    let available_width = available_width.max(1.0);
    let mut lines = Vec::new();
    let mut entries: Vec<CompactFrequencyBadge> = Vec::new();
    let mut line_width = 0.0f32;
    for badge in badges {
        let width = badge
            .display_text
            .chars()
            .map(|ch| if ch.is_ascii() { 6.0 } else { 10.0 })
            .sum::<f32>()
            + 10.0;
        let gap = if entries.is_empty() { 0.0 } else { 4.0 };
        if !entries.is_empty() && line_width + gap + width > available_width {
            lines.push(std::mem::take(&mut entries));
            line_width = 0.0;
        }
        if !entries.is_empty() {
            line_width += 4.0;
        }
        entries.push(badge.clone());
        line_width += width;
    }
    if !entries.is_empty() {
        lines.push(entries);
    }
    lines
}

/// One aligned headword furigana segment (`reading` is empty for kana segments).
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeadwordFuriganaSegment {
    pub text: String,
    pub reading: String,
}

/// Result of headword furigana alignment (Yomitan `#505`), guaranteeing that kana-only
/// expressions never receive duplicate ruby and unaligned mixed kanji+kana readings fall
/// back cleanly without slapping whole-word ruby over kana.
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeadwordFurigana {
    pub segments: Vec<HeadwordFuriganaSegment>,
    pub aligned: bool,
    pub fallback_reading: Option<String>,
}

#[allow(dead_code)]
fn peel_headword_kana_affixes(
    expression: &str,
    reading: &str,
) -> Option<(Vec<HeadwordFuriganaSegment>, bool)> {
    let expr_chars: Vec<char> = expression.chars().collect();
    let read_chars: Vec<char> = reading.chars().collect();
    let fold = |c: char| -> char {
        let s = c.to_string();
        subomi_dict::normalize::normalize_reading_str(&s)
            .chars()
            .next()
            .unwrap_or(c)
    };

    let mut prefix_len = 0usize;
    while prefix_len < expr_chars.len()
        && prefix_len < read_chars.len()
        && !is_headword_kanji_char(expr_chars[prefix_len])
        && !matches!(expr_chars[prefix_len], '\u{30F5}' | '\u{30F6}')
        && fold(expr_chars[prefix_len]) == fold(read_chars[prefix_len])
    {
        prefix_len += 1;
    }

    let mut suffix_len = 0usize;
    while suffix_len < (expr_chars.len() - prefix_len)
        && suffix_len < (read_chars.len() - prefix_len)
        && !is_headword_kanji_char(expr_chars[expr_chars.len() - 1 - suffix_len])
        && !matches!(
            expr_chars[expr_chars.len() - 1 - suffix_len],
            '\u{30F5}' | '\u{30F6}'
        )
        && fold(expr_chars[expr_chars.len() - 1 - suffix_len])
            == fold(read_chars[read_chars.len() - 1 - suffix_len])
    {
        suffix_len += 1;
    }

    if prefix_len == 0 && suffix_len == 0 {
        return None;
    }

    let core_expr: String = expr_chars[prefix_len..expr_chars.len() - suffix_len]
        .iter()
        .collect();
    let core_read: String = read_chars[prefix_len..read_chars.len() - suffix_len]
        .iter()
        .collect();

    if core_expr.is_empty() || core_read.is_empty() {
        return None;
    }
    // Only consider peeled alignment clean if the remaining core has no unaligned kana.
    let core_all_kanji = core_expr.chars().all(is_headword_kanji_char);
    if !core_all_kanji {
        return None;
    }

    let mut segments = Vec::with_capacity(3);
    if prefix_len > 0 {
        segments.push(HeadwordFuriganaSegment {
            text: expr_chars[..prefix_len].iter().collect(),
            reading: String::new(),
        });
    }
    segments.push(HeadwordFuriganaSegment {
        text: core_expr,
        reading: core_read,
    });
    if suffix_len > 0 {
        segments.push(HeadwordFuriganaSegment {
            text: expr_chars[expr_chars.len() - suffix_len..].iter().collect(),
            reading: String::new(),
        });
    }
    Some((segments, true))
}

/// Aligns `reading` over `expression` for popup headword furigana (Yomitan `#505`):
/// - Pure-kana or reading-identical expressions emit plain segments with empty ruby.
/// - Aligned kanji+kana expressions attach ruby strictly to kanji spans (including CJK Ext H & J).
/// - Unaligned mixed kanji+kana readings fall back cleanly (`aligned = false`,
///   `fallback_reading = Some(reading)`) without duplicating whole-word ruby over kana.
#[allow(dead_code)]
#[must_use]
pub fn align_headword_furigana(expression: &str, reading: &str) -> HeadwordFurigana {
    if expression.is_empty() {
        return HeadwordFurigana {
            segments: Vec::new(),
            aligned: true,
            fallback_reading: None,
        };
    }
    let reading_trimmed = reading.trim();
    if reading_trimmed.is_empty() || reading_trimmed == "*" {
        return HeadwordFurigana {
            segments: vec![HeadwordFuriganaSegment {
                text: expression.to_owned(),
                reading: String::new(),
            }],
            aligned: true,
            fallback_reading: None,
        };
    }

    let norm_expr = subomi_dict::normalize::normalize_reading_str(expression);
    let norm_read = subomi_dict::normalize::normalize_reading_str(reading_trimmed);
    if norm_expr == norm_read {
        return HeadwordFurigana {
            segments: vec![HeadwordFuriganaSegment {
                text: expression.to_owned(),
                reading: String::new(),
            }],
            aligned: true,
            fallback_reading: None,
        };
    }

    let has_kanji = expression.chars().any(is_headword_kanji_char);
    if !has_kanji {
        // Never duplicate whole-word ruby over pure kana expressions (#505).
        return HeadwordFurigana {
            segments: vec![HeadwordFuriganaSegment {
                text: expression.to_owned(),
                reading: String::new(),
            }],
            aligned: false,
            fallback_reading: Some(reading_trimmed.to_owned()),
        };
    }

    let all_kanji = expression.chars().all(is_headword_kanji_char);
    if all_kanji {
        return HeadwordFurigana {
            segments: vec![HeadwordFuriganaSegment {
                text: expression.to_owned(),
                reading: reading_trimmed.to_owned(),
            }],
            aligned: true,
            fallback_reading: None,
        };
    }

    let spans = subomi_util::furiganautils::simplify_surface_str(expression, reading_trimmed);
    let fell_back_whole = spans.len() == 1
        && spans[0].surface_start == 0
        && spans[0].surface_end == expression.len()
        && spans[0].reading_start == 0
        && spans[0].reading_end == reading_trimmed.len();

    if fell_back_whole {
        if let Some((segments, aligned)) = peel_headword_kana_affixes(expression, reading_trimmed) {
            return HeadwordFurigana {
                segments,
                aligned,
                fallback_reading: None,
            };
        }
        // Unaligned mixed kanji+kana reading: fall back cleanly without duplicating
        // whole-word ruby over unaligned kana (#505).
        return HeadwordFurigana {
            segments: vec![HeadwordFuriganaSegment {
                text: expression.to_owned(),
                reading: String::new(),
            }],
            aligned: false,
            fallback_reading: Some(reading_trimmed.to_owned()),
        };
    }

    let mut segments = Vec::with_capacity(spans.len());
    for span in spans {
        let surf = expression
            .get(span.surface_start..span.surface_end)
            .unwrap_or_default();
        if surf.is_empty() {
            continue;
        }
        let seg_reading =
            if span.reading_start < span.reading_end && surf.chars().any(is_headword_kanji_char) {
                let r = reading_trimmed
                    .get(span.reading_start..span.reading_end)
                    .unwrap_or_default();
                if subomi_dict::normalize::normalize_reading_str(surf)
                    == subomi_dict::normalize::normalize_reading_str(r)
                {
                    String::new()
                } else {
                    r.to_owned()
                }
            } else {
                String::new()
            };
        segments.push(HeadwordFuriganaSegment {
            text: surf.to_owned(),
            reading: seg_reading,
        });
    }

    HeadwordFurigana {
        segments,
        aligned: true,
        fallback_reading: None,
    }
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
pub fn install_popup_navigation(app: &SubomiApp) {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cjk_extensions_h_and_j_are_recognized_as_kanji() {
        // Basic CJK + Extension H (U+31350..=U+323AF) + Extension J (U+323B0..=U+3347F)
        assert!(is_kanji_char('漢'));
        assert!(is_kanji_char('\u{31350}'));
        assert!(is_kanji_char('\u{323AF}'));
        assert!(is_kanji_char('\u{323B0}'));
        assert!(is_kanji_char('\u{3347F}'));
        assert!(!is_kanji_char('あ'));
        assert!(!is_kanji_char('A'));
    }

    #[test]
    fn string_pitch_patterns_compute_exact_levels_points_svg_and_compact_notation() {
        let lhh = TermPitchDetail {
            dictionary: "Kanjium".to_owned(),
            mora: vec!["さ".into(), "く".into(), "ら".into()],
            position: 0,
            pattern: Some("LHH".to_owned()),
            nasal: Vec::new(),
            devoice: Vec::new(),
            tags: Vec::new(),
        };
        let data_lhh = popup_pitch_pattern_data(&lhh);
        assert_eq!(data_lhh.badge_text, "[LHH]");
        assert_eq!(data_lhh.mora_highs, vec![false, true, true]);
        assert!(data_lhh.particle_high);
        assert_eq!(data_lhh.points.len(), 4);
        assert_eq!(data_lhh.points[0].y, 75.0);
        assert_eq!(data_lhh.points[1].y, 25.0);
        assert!(data_lhh.svg.as_deref().unwrap_or("").contains("<svg"));

        // Non-standard double-peak pattern "HLHL"
        let hlhl = TermPitchDetail {
            dictionary: "Kanjium".to_owned(),
            mora: vec!["あ".into(), "い".into(), "う".into(), "え".into()],
            position: 1,
            pattern: Some("HLHL".to_owned()),
            nasal: Vec::new(),
            devoice: Vec::new(),
            tags: Vec::new(),
        };
        let data_hlhl = popup_pitch_pattern_data(&hlhl);
        assert_eq!(data_hlhl.badge_text, "[HLHL]");
        assert_eq!(data_hlhl.mora_highs, vec![true, false, true, false]);
        assert!(!data_hlhl.particle_high);
        assert!(data_hlhl.points[0].downstep);
        assert!(data_hlhl.points[2].downstep);
        assert_eq!(data_hlhl.compact_notation, "あ↓いう↓え [HLHL]");

        // Odaka exact string pattern "LHHL" (4 chars for 3 morae -> drop onto particle)
        let odaka = TermPitchDetail {
            dictionary: "Kanjium".to_owned(),
            mora: vec!["お".into(), "と".into(), "こ".into()],
            position: 3,
            pattern: Some("LHHL".to_owned()),
            nasal: Vec::new(),
            devoice: Vec::new(),
            tags: Vec::new(),
        };
        let data_odaka = popup_pitch_pattern_data(&odaka);
        assert_eq!(data_odaka.mora_highs, vec![false, true, true]);
        assert!(!data_odaka.particle_high);
        assert!(data_odaka.points[2].downstep);
        assert_eq!(data_odaka.compact_notation, "おとこ↓ [LHHL]");

        let compact = format_compact_pitches(&[lhh.clone(), odaka.clone()]);
        assert_eq!(compact, "さくら [LHH] | おとこ↓ [LHHL]");

        let groups = compact_pitch_groups(&[lhh, odaka]);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].dictionary, "Kanjium");
        assert_eq!(groups[0].notations, "さくら [LHH] | おとこ↓ [LHHL]");
        assert_eq!(groups[0].badges, "[LHH] | [LHHL]");
    }

    #[test]
    fn frequency_badge_compacting_omits_repeated_dictionary_names() {
        let freqs = vec![
            TermFrequency {
                dictionary: "JPDB".to_owned(),
                value: "2058".to_owned(),
            },
            TermFrequency {
                dictionary: "JPDB".to_owned(),
                value: "86009㋕".to_owned(),
            },
            TermFrequency {
                dictionary: "JPDB".to_owned(),
                value: "2058".to_owned(),
            },
            TermFrequency {
                dictionary: "Jiten".to_owned(),
                value: "375".to_owned(),
            },
        ];
        let badges = compact_frequency_badges(&freqs);
        assert_eq!(badges.len(), 3);
        assert!(badges[0].show_dictionary);
        assert_eq!(badges[0].display_text, "JPDB 2058");
        assert!(!badges[1].show_dictionary);
        assert_eq!(badges[1].display_text, "86009㋕");
        assert_eq!(badges[1].dictionary, "JPDB");
        assert!(badges[2].show_dictionary);
        assert_eq!(badges[2].display_text, "Jiten 375");

        let groups = compact_frequency_groups(&freqs);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].dictionary, "JPDB");
        assert_eq!(groups[0].value, "2058, 86009㋕");

        let lines = compact_frequency_lines(&badges, 100.0);
        assert!(!lines.is_empty());
        assert_eq!(lines.iter().map(Vec::len).sum::<usize>(), 3);
    }

    #[test]
    fn grouped_rows_accumulate_scores_rank_cloze_body_and_apply_compact_options() {
        let mut h1 = SearchHit::new("端", "はし", "◦ edge\n◦ tip", "n, common", 30, false);
        h1.dict_name = "JMdict".to_owned();
        let mut h2 = SearchHit::new("はし", "はし", "◦ bridge\n◦ chopsticks", "n, common", 5, false);
        h2.dict_name = "JMdict".to_owned();
        let mut h3 = SearchHit::new("はし", "はし", "◦ edge (sense 2)", "n, common, arch", 5, false);
        h3.dict_name = "Daijisen".to_owned();

        let rows = dictionary_result_rows_with_options(
            &[h1, h2, h3],
            &[],
            Some("はし"),
            true,
            true,
        );
        assert_eq!(rows.len(), 3);
        // Exact expression == cloze_body ('はし') must rank ahead of reading-only '端'
        assert_eq!(rows[0].expression.as_str(), "はし");
        assert_eq!(rows[1].expression.as_str(), "はし");
        assert_eq!(rows[2].expression.as_str(), "端");
        // compact_glossaries joins multi-line glossary into ` | `
        assert_eq!(rows[0].glossary.as_str(), "bridge | chopsticks");
        // compact_tags removes tags already displayed on earlier sense of the same entry
        assert!(rows[0].pos_tags.contains("n"));
        assert!(rows[0].pos_tags.contains("common"));
        assert!(!rows[1].pos_tags.contains("common"));
        assert!(rows[1].pos_tags.contains("arch"));
    }

    #[test]
    fn headword_furigana_alignment_avoids_duplicating_ruby_over_kana() {
        // Pure kana -> never gets ruby
        let kana = align_headword_furigana("ひらがな", "ひらがな");
        assert!(kana.aligned);
        assert_eq!(kana.segments.len(), 1);
        assert_eq!(kana.segments[0].reading, "");

        // Standard kanji + okurigana -> ruby only over kanji
        let taberu = align_headword_furigana("食べる", "たべる");
        assert!(taberu.aligned);
        assert_eq!(
            taberu.segments,
            vec![
                HeadwordFuriganaSegment {
                    text: "食".to_owned(),
                    reading: "た".to_owned(),
                },
                HeadwordFuriganaSegment {
                    text: "べる".to_owned(),
                    reading: String::new(),
                },
            ]
        );

        // CJK Extension J kanji + okurigana -> ruby only over Ext J kanji
        let ext_j = align_headword_furigana("\u{323B0}る", "みる");
        assert!(ext_j.aligned);
        assert_eq!(ext_j.segments[0].text, "\u{323B0}");
        assert_eq!(ext_j.segments[0].reading, "み");
        assert_eq!(ext_j.segments[1].text, "る");
        assert_eq!(ext_j.segments[1].reading, "");

        // Unaligned mixed kanji+kana reading (e.g. gikun/ateji where kana does not match reading)
        // must fall back cleanly instead of putting whole-word ruby over kana (#505)
        let unaligned = align_headword_furigana("お姉さん", "あね");
        assert!(!unaligned.aligned);
        assert_eq!(unaligned.fallback_reading.as_deref(), Some("あね"));
        assert!(
            unaligned.segments.iter().all(|s| s.reading.is_empty()),
            "unaligned mixed kanji+kana must not attach whole-word ruby over kana"
        );
    }
}

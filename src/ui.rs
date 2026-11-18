//! Generated Slint UI bindings and rich dictionary/subtitle bridges for Memento.

slint::include_modules!();

use std::borrow::Cow;
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};
use std::rc::Rc;
use std::sync::{
    atomic::{AtomicI32, AtomicU64, Ordering},
    mpsc, Arc,
};
use std::time::SystemTime;

use memento_definition::{
    self as mdef,
    element::{BOTTOM, LEFT, RIGHT, TOP},
    split_ruby_content, structured_content_text, Color as DefColor, Context as DefContext,
    ElementLayout, Json as DefJson, LayoutDirection, LegacyGlossaryStyle, ParsedStylesheet,
    Renderer as DefRenderer, ScreenMetrics, SiblingState,
};
use memento_setting::keys as raw;
use parley::{
    Alignment, AlignmentOptions, Cluster, FontContext, FontFamily, FontStyle, FontWeight,
    InlineBox, Layout, LayoutContext, LineHeight, PositionedLayoutItem, StyleProperty,
};
use resvg::tiny_skia;
use resvg::usvg;
use slint::{ComponentHandle, Model as _, Rgba8Pixel, SharedPixelBuffer, SharedString, VecModel};

use crate::controller::config::ConfigController;
use crate::controller::dict::{DictController, DictTag, SearchHit, TermFrequency, TermPitchDetail};

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
    use slint::Model;
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

static NEXT_BRIDGE_INSTANCE_ID: AtomicI32 = AtomicI32::new(1);

thread_local! {
    static LAST_GROUPED_HITS: RefCell<Vec<SearchHit>> = const { RefCell::new(Vec::new()) };
    static ACTIVE_RICH_BRIDGES: RefCell<Vec<Rc<RichUiBridge>>> = const { RefCell::new(Vec::new()) };
}

pub fn is_kanji_char(character: char) -> bool {
    ('\u{3400}'..='\u{9fff}').contains(&character)
        || ('\u{f900}'..='\u{faff}').contains(&character)
        || ('\u{20000}'..='\u{3134f}').contains(&character)
}

pub fn dictionary_result_rows(hits: &[SearchHit], excluded: &[String]) -> Vec<DictResult> {
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

    LAST_GROUPED_HITS.with(|slot| {
        *slot.borrow_mut() = grouped.iter().map(|hit| (*hit).clone()).collect();
    });

    grouped
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
        .collect()
}

fn popup_rows_through(rows: &slint::ModelRc<DictResult>, count: usize) -> Vec<DictResult> {
    (0..count.min(rows.row_count()))
        .filter_map(|index| rows.row_data(index))
        .filter(|row| !row.collapsed || row.term_start)
        .collect()
}

pub fn set_dictionary_rows(app: &MementoApp, rows: Vec<DictResult>) {
    let full = Rc::new(VecModel::from(rows.clone()));
    app.set_dict_results(full.into());
    let full = app.get_dict_results();
    let first_batch_end = next_popup_batch_end(&full, 0);
    app.set_popup_loaded_count(first_batch_end as i32);
    app.set_popup_results(
        Rc::new(VecModel::from(popup_rows_through(&full, first_batch_end))).into(),
    );

    app.global::<PopupBridge>().set_selected_glossary_text("".into());
    app.global::<PopupBridge>().set_tooltip_visible(false);

    let app_bridge_id = app.global::<PopupBridge>().get_bridge_instance_id();
    if app_bridge_id > 0 {
        ACTIVE_RICH_BRIDGES.with(|slot| {
            let mut bridges = slot.borrow_mut();
            bridges.retain(|bridge| bridge.app_weak.upgrade().is_some());
            for bridge in bridges.iter() {
                if bridge.instance_id == app_bridge_id {
                    bridge.on_dictionary_rows_updated(app, &rows);
                }
            }
        });
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

/// Shared popup actions used by the desktop window and the GL interaction test.
pub fn install_popup_navigation(app: &MementoApp) {
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
            let end = next_popup_batch_end(&full, start);
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

// ── Subtitle Segmentation & Multi-Row Wrapping ──────────────────────────────

pub fn subtitle_words(text: &str) -> Vec<SubtitleWord> {
    let parts = crate::controller::subtitles::segment_subtitle_text(text)
        .into_iter()
        .map(|token| (token, String::new()))
        .collect();
    subtitle_words_from_parts(text, parts)
}

pub fn subtitle_words_from_parts(text: &str, parts: Vec<(String, String)>) -> Vec<SubtitleWord> {
    let mut offset = 0;
    parts
        .into_iter()
        .enumerate()
        .map(|(index, (token, reading))| {
            let suffix = text.get(offset..).unwrap_or("");
            let query = if token
                .chars()
                .any(|ch| ch.is_alphanumeric() || is_japanese_lookup_char(ch))
            {
                suffix.into()
            } else {
                SharedString::default()
            };
            offset += token.len();
            SubtitleWord {
                text: token.into(),
                reading: reading.into(),
                query,
                index: index as i32,
            }
        })
        .collect()
}

fn is_japanese_lookup_char(ch: char) -> bool {
    matches!(ch, '\u{3005}'..='\u{3007}' | '\u{3040}'..='\u{30ff}' | '\u{ff66}'..='\u{ff9f}')
        || is_kanji_char(ch)
}

fn estimated_subtitle_text_width(text: &str, font_px: f32) -> f32 {
    text.chars()
        .map(|character| {
            if character.is_ascii_whitespace() {
                0.35
            } else if character.is_ascii_punctuation() {
                0.55
            } else if character.is_ascii() {
                0.65
            } else {
                1.05
            }
        })
        .sum::<f32>()
        * font_px
}

fn active_subtitle_render_style() -> SubtitleRenderStyle {
    ACTIVE_RICH_BRIDGES.with(|slot| {
        let Ok(bridges) = slot.try_borrow() else {
            return SubtitleRenderStyle::default();
        };
        bridges
            .iter()
            .rev()
            .find(|b| b.app_weak.upgrade().is_some())
            .and_then(|b| b.state.try_borrow().ok().map(|st| st.settings.subtitle_style.clone()))
            .unwrap_or_default()
    })
}

fn subtitle_style_for_app(app: &MementoApp) -> SubtitleRenderStyle {
    let bridge_id = app.global::<PopupBridge>().get_bridge_instance_id();
    if bridge_id > 0 {
        if let Some(style) = ACTIVE_RICH_BRIDGES.with(|slot| {
            let bridges = slot.try_borrow().ok()?;
            let bridge = bridges.iter().find(|b| b.instance_id == bridge_id)?;
            let st = bridge.state.try_borrow().ok()?;
            Some(st.settings.subtitle_style.clone())
        }) {
            return style;
        }
    }
    active_subtitle_render_style()
}

pub fn subtitle_token_width_with_style(
    word: &SubtitleWord,
    font_px: f32,
    furigana: bool,
    style: &SubtitleRenderStyle,
) -> f32 {
    if let Some(shaped_width) = SHARED_TEXT_ENGINE.with(|engine| {
        engine
            .borrow_mut()
            .measure_subtitle_token_width(word, font_px, furigana, style)
    }) {
        return shaped_width;
    }
    let base_width = estimated_subtitle_text_width(word.text.as_str(), font_px);
    let reading_width = if furigana && !word.reading.is_empty() {
        estimated_subtitle_text_width(word.reading.as_str(), font_px * style.furigana_ratio)
    } else {
        0.0
    };
    base_width.max(reading_width) + 2.0
}

pub fn subtitle_token_width(word: &SubtitleWord, font_px: f32, furigana: bool) -> f32 {
    let style = active_subtitle_render_style();
    subtitle_token_width_with_style(word, font_px, furigana, &style)
}

pub fn subtitle_lines_from_words_with_style(
    all_words: Vec<SubtitleWord>,
    max_width_px: f32,
    font_px: f32,
    furigana: bool,
    style: &SubtitleRenderStyle,
) -> Vec<SubtitleLine> {
    let mut lines = Vec::new();
    let mut current_line: Vec<SubtitleWord> = Vec::new();
    let mut current_width = 0.0;

    for word in all_words {
        let word_text = word.text.as_str();
        if word_text.contains('\n') {
            if !current_line.is_empty() {
                lines.push(SubtitleLine {
                    words: Rc::new(VecModel::from(std::mem::take(&mut current_line))).into(),
                });
                current_width = 0.0;
            }
            continue;
        }
        let token_width = subtitle_token_width_with_style(&word, font_px, furigana, style);
        if current_width + token_width > max_width_px && !current_line.is_empty() {
            lines.push(SubtitleLine {
                words: Rc::new(VecModel::from(std::mem::take(&mut current_line))).into(),
            });
            current_width = 0.0;
            if word_text.trim().is_empty() {
                continue;
            }
        }
        current_width += token_width;
        current_line.push(word);
    }
    if !current_line.is_empty() {
        lines.push(SubtitleLine {
            words: Rc::new(VecModel::from(current_line)).into(),
        });
    }
    lines
}

pub fn subtitle_lines_from_words(
    all_words: Vec<SubtitleWord>,
    max_width_px: f32,
    font_px: f32,
    furigana: bool,
) -> Vec<SubtitleLine> {
    let style = active_subtitle_render_style();
    subtitle_lines_from_words_with_style(all_words, max_width_px, font_px, furigana, &style)
}

pub fn subtitle_lines_for_app(app: &MementoApp, words: Vec<SubtitleWord>) -> Vec<SubtitleLine> {
    let max_width_px = (app.get_video_width() as f32 - 72.0).max(120.0);
    let font_px = (app.get_video_height() as f32 * app.get_subtitle_scale()).max(8.0);
    let style = subtitle_style_for_app(app);
    subtitle_lines_from_words_with_style(words, max_width_px, font_px, app.get_furigana_visible(), &style)
}

pub fn publish_active_subtitle(
    app: &MementoApp,
    text: &str,
    furigana_sender: &std::sync::mpsc::Sender<(u64, String)>,
    furigana_epoch: &std::sync::Arc<std::sync::atomic::AtomicU64>,
) {
    let words = subtitle_words(text);
    let lines = subtitle_lines_for_app(app, words.clone());
    app.set_active_sub_words(Rc::new(VecModel::from(words)).into());
    app.set_active_sub_lines(Rc::new(VecModel::from(lines)).into());
    app.set_selected_token_index(-1);
    app.set_active_sub_text(text.into());
    let epoch = furigana_epoch.fetch_add(1, Ordering::AcqRel) + 1;
    let pending = app.get_furigana_visible()
        && !text.is_empty()
        && furigana_sender.send((epoch, text.to_owned())).is_ok();
    app.set_subtitle_furigana_pending(pending);

    let app_bridge_id = app.global::<PopupBridge>().get_bridge_instance_id();
    if app_bridge_id > 0 {
        ACTIVE_RICH_BRIDGES.with(|slot| {
            for bridge in slot.borrow().iter() {
                if bridge.instance_id == app_bridge_id {
                    bridge.clear_subtitle_scan_state(app);
                }
            }
        });
    }
}

/// Apply a worker result while preserving the selected base-text span.
pub fn apply_subtitle_furigana(app: &MementoApp, text: &str, parts: Vec<(String, String)>) {
    let words = subtitle_words_from_parts(text, parts);
    let previous_query = app
        .get_active_sub_words()
        .row_data(app.get_selected_token_index().max(0) as usize)
        .filter(|_| app.get_selected_token_index() >= 0)
        .map(|word| word.query);
    let selected_index = previous_query
        .as_ref()
        .and_then(|query| words.iter().position(|word| word.query == *query))
        .map(|index| index as i32)
        .unwrap_or(-1);
    let selected_count = if selected_index >= 0 && app.get_dict_open() {
        let target = app.get_dict_query().encode_utf16().count();
        let mut length = 0;
        let mut count = 0;
        for word in words.iter().skip(selected_index as usize) {
            length += word.text.encode_utf16().count();
            count += 1;
            if length >= target {
                break;
            }
        }
        count.max(1)
    } else {
        1
    };
    let lines = subtitle_lines_for_app(app, words.clone());
    app.set_active_sub_words(Rc::new(VecModel::from(words)).into());
    app.set_active_sub_lines(Rc::new(VecModel::from(lines)).into());
    app.set_selected_token_index(selected_index);
    app.set_selected_token_count(selected_count);
    app.set_subtitle_furigana_pending(false);
}

// ── Parley + Resvg Shared Text & Vector Stroke Engine ───────────────────────

#[derive(Clone, Debug, PartialEq)]
pub struct SubtitleRenderStyle {
    pub font_family: String,
    pub text_rgba: [u8; 4],
    pub stroke_rgba: [u8; 4],
    pub bg_rgba: [u8; 4],
    pub stroke_size: f32,
    pub furigana_ratio: f32,
    pub bold: bool,
    pub italic: bool,
}

impl Default for SubtitleRenderStyle {
    fn default() -> Self {
        Self {
            font_family: "Noto Sans CJK JP".to_owned(),
            text_rgba: [255, 255, 255, 255],
            stroke_rgba: [0, 0, 0, 255],
            bg_rgba: [0, 0, 0, 0],
            stroke_size: 6.0,
            furigana_ratio: 0.4,
            bold: true,
            italic: false,
        }
    }
}

#[derive(Clone, Debug)]
struct RegisteredFontInfo {
    family: String,
    weight: u16,
    italic: bool,
}

#[derive(Clone, Debug)]
struct PositionedSvgCluster {
    x: f32,
    baseline_y: f32,
    text: String,
    family: String,
    font_size: f32,
    weight: u16,
    italic: bool,
    fill_rgba: [u8; 4],
    underline: bool,
    strikethrough: bool,
}

#[derive(Clone)]
pub struct ShapedSubtitleToken {
    pub width_px: f32,
    pub height_px: f32,
    pub glyph_x_px: f32,
    pub glyph_y_px: f32,
    pub glyph_width_px: f32,
    pub glyph_height_px: f32,
    /// `(byte_start, byte_end, x_start, advance)` for each cluster in `word.text`
    pub clusters: Vec<(usize, usize, f32, f32)>,
    pub image: slint::Image,
}

#[derive(Hash, PartialEq, Eq, Clone)]
struct SubtitleCacheKey {
    text: String,
    reading: String,
    font_tenth_px: i32,
    show_furigana: bool,
    font_family: String,
    text_rgba: [u8; 4],
    stroke_rgba: [u8; 4],
    bg_rgba: [u8; 4],
    stroke_tenth: i32,
    furigana_pct: i32,
    bold: bool,
    italic: bool,
}

struct TextEngine {
    font_cx: FontContext,
    layout_cx: LayoutContext<[u8; 4]>,
    fontdb: Arc<usvg::fontdb::Database>,
    registered_fonts: HashMap<(u64, u32), RegisteredFontInfo>,
    subtitle_cache: HashMap<SubtitleCacheKey, ShapedSubtitleToken>,
    devoice_circle: Option<slint::Image>,
}

thread_local! {
    static SHARED_TEXT_ENGINE: RefCell<TextEngine> = RefCell::new(TextEngine::new());
}

impl TextEngine {
    fn new() -> Self {
        Self {
            font_cx: FontContext::new(),
            layout_cx: LayoutContext::new(),
            fontdb: Arc::new(usvg::fontdb::Database::new()),
            registered_fonts: HashMap::new(),
            subtitle_cache: HashMap::new(),
            devoice_circle: None,
        }
    }

    fn ensure_font_registered(&mut self, font: &parley::FontData) -> RegisteredFontInfo {
        let key = (font.data.id(), font.index);
        if let Some(info) = self.registered_fonts.get(&key) {
            return info.clone();
        }
        let db = Arc::make_mut(&mut self.fontdb);
        let ids = db.load_font_source(usvg::fontdb::Source::Binary(Arc::new(font.data.clone())));
        let mut chosen: Option<RegisteredFontInfo> = None;
        for id in ids {
            if let Some(face) = db.face(id) {
                let family = face
                    .families
                    .first()
                    .map(|(name, _)| name.clone())
                    .unwrap_or_else(|| "sans-serif".to_owned());
                let info = RegisteredFontInfo {
                    family,
                    weight: face.weight.0,
                    italic: face.style != usvg::fontdb::Style::Normal,
                };
                self.registered_fonts.insert((font.data.id(), face.index), info.clone());
                if face.index == font.index {
                    chosen = Some(info);
                } else if chosen.is_none() {
                    chosen = Some(info);
                }
            }
        }
        let fallback = chosen.unwrap_or_else(|| RegisteredFontInfo {
            family: "Noto Sans CJK JP".to_owned(),
            weight: 400,
            italic: false,
        });
        self.registered_fonts.entry(key).or_insert_with(|| fallback.clone());
        fallback
    }

    fn shape_single_line(
        &mut self,
        text: &str,
        font_family: &str,
        font_px: f32,
        weight: u16,
        italic: bool,
        color: [u8; 4],
    ) -> Layout<[u8; 4]> {
        let family_stack = if font_family.trim().is_empty() {
            Cow::Borrowed("Noto Sans CJK JP, sans-serif")
        } else {
            Cow::Owned(format!("{font_family}, Noto Sans CJK JP, sans-serif"))
        };
        let mut builder = self.layout_cx.ranged_builder(&mut self.font_cx, text, 1.0, true);
        builder.push_default(StyleProperty::FontFamily(FontFamily::Source(family_stack)));
        builder.push_default(StyleProperty::FontSize(font_px.max(6.0)));
        builder.push_default(StyleProperty::FontWeight(FontWeight::new(weight as f32)));
        builder.push_default(StyleProperty::FontStyle(if italic {
            FontStyle::Italic
        } else {
            FontStyle::Normal
        }));
        builder.push_default(StyleProperty::LineHeight(LineHeight::MetricsRelative(1.0)));
        builder.push_default(StyleProperty::Brush(color));
        let mut layout = builder.build(text);
        layout.break_all_lines(None);
        layout.align(None, Alignment::Start, AlignmentOptions::default());
        layout
    }

    fn measure_subtitle_token_width(
        &mut self,
        word: &SubtitleWord,
        font_px: f32,
        furigana: bool,
        style: &SubtitleRenderStyle,
    ) -> Option<f32> {
        if word.text.is_empty() {
            return Some(0.0);
        }
        let shaped = self.shape_and_render_subtitle_token(word, font_px, furigana, style);
        (shaped.width_px > 0.0).then_some(shaped.width_px)
    }

    fn collect_svg_clusters_from_layout(
        &mut self,
        layout: &Layout<[u8; 4]>,
        source_text: &str,
        origin_x: f32,
        origin_y: f32,
        override_underline: Option<&[bool]>,
        override_strike: Option<&[bool]>,
    ) -> Vec<PositionedSvgCluster> {
        let mut out = Vec::new();
        for line in layout.lines() {
            let baseline = origin_y + line.metrics().baseline;
            for item in line.items() {
                if let PositionedLayoutItem::GlyphRun(glyph_run) = item {
                    let run = glyph_run.run();
                    let font_info = self.ensure_font_registered(run.font().clone().borrow_font());
                    let font_size = run.font_size();
                    let brush = glyph_run.style().brush;
                    let run_underline = glyph_run.style().underline.is_some();
                    let run_strike = glyph_run.style().strikethrough.is_some();
                    let mut x_cursor = origin_x + glyph_run.offset();
                    for cluster in run.visual_clusters() {
                        let range = cluster.text_range();
                        let advance = cluster.advance();
                        if let Some(slice) = source_text.get(range.clone()) {
                            if !slice.is_empty() && !slice.chars().all(|c| c == '\n' || c == '\r') {
                                let ul = override_underline
                                    .and_then(|map| map.get(range.start).copied())
                                    .unwrap_or(run_underline);
                                let st = override_strike
                                    .and_then(|map| map.get(range.start).copied())
                                    .unwrap_or(run_strike);
                                out.push(PositionedSvgCluster {
                                    x: x_cursor,
                                    baseline_y: baseline,
                                    text: slice.to_owned(),
                                    family: font_info.family.clone(),
                                    font_size,
                                    weight: font_info.weight,
                                    italic: font_info.italic,
                                    fill_rgba: brush,
                                    underline: ul,
                                    strikethrough: st,
                                });
                            }
                        }
                        x_cursor += advance;
                    }
                }
            }
        }
        out
    }

    fn shape_and_render_subtitle_token(
        &mut self,
        word: &SubtitleWord,
        font_px: f32,
        show_furigana: bool,
        style: &SubtitleRenderStyle,
    ) -> ShapedSubtitleToken {
        let font_px = font_px.clamp(8.0, 140.0);
        let key = SubtitleCacheKey {
            text: word.text.to_string(),
            reading: if show_furigana {
                word.reading.to_string()
            } else {
                String::new()
            },
            font_tenth_px: (font_px * 10.0).round() as i32,
            show_furigana,
            font_family: style.font_family.clone(),
            text_rgba: style.text_rgba,
            stroke_rgba: style.stroke_rgba,
            bg_rgba: style.bg_rgba,
            stroke_tenth: (style.stroke_size * 10.0).round() as i32,
            furigana_pct: (style.furigana_ratio * 100.0).round() as i32,
            bold: style.bold,
            italic: style.italic,
        };
        if let Some(cached) = self.subtitle_cache.get(&key) {
            return cached.clone();
        }
        if self.subtitle_cache.len() > 1024 {
            self.subtitle_cache.clear();
        }

        let weight = if style.bold { 700 } else { 400 };
        let base_layout = self.shape_single_line(
            word.text.as_str(),
            &style.font_family,
            font_px,
            weight,
            style.italic,
            style.text_rgba,
        );

        let base_width = base_layout
            .lines()
            .next()
            .map(|l| l.metrics().advance)
            .unwrap_or(0.0)
            .max(1.0);
        let base_height = base_layout
            .lines()
            .next()
            .map(|l| l.metrics().line_height)
            .unwrap_or(font_px * 1.25)
            .max(font_px);

        let stroke_px = if style.stroke_size <= 0.0 || style.stroke_rgba[3] == 0 {
            0.0
        } else {
            (style.stroke_size * (font_px / 48.0)).clamp(1.2, font_px * 0.28)
        };
        let pad_x = (stroke_px * 0.65).ceil().max(1.0);
        let pad_y = (stroke_px * 0.5).ceil().max(1.0);

        let furigana_ratio = style.furigana_ratio.clamp(0.25, 0.75);
        let ruby_font_px = (font_px * furigana_ratio).max(8.0);
        let ruby_space = if show_furigana {
            (font_px * furigana_ratio.max(0.45) + 3.0).ceil()
        } else {
            0.0
        };

        let (ruby_layout, ruby_width, ruby_height) = if show_furigana && !word.reading.is_empty() {
            let layout = self.shape_single_line(
                word.reading.as_str(),
                &style.font_family,
                ruby_font_px,
                600,
                style.italic,
                style.text_rgba,
            );
            let rw = layout
                .lines()
                .next()
                .map(|l| l.metrics().advance)
                .unwrap_or(0.0);
            let rh = layout
                .lines()
                .next()
                .map(|l| l.metrics().line_height)
                .unwrap_or(ruby_font_px * 1.2);
            (Some(layout), rw, rh)
        } else {
            (None, 0.0, 0.0)
        };

        let total_width = (base_width.max(ruby_width) + pad_x * 2.0).ceil().max(2.0);
        let total_height = (ruby_space + base_height + pad_y * 2.0).ceil().max(2.0);
        let base_origin_x = (total_width - base_width) * 0.5;
        let base_origin_y = ruby_space + pad_y;

        let mut clusters = Vec::new();
        if let Some(line) = base_layout.lines().next() {
            let mut x_pos = line.metrics().offset;
            for run in line.runs() {
                for cluster in run.visual_clusters() {
                    let r = cluster.text_range();
                    let adv = cluster.advance();
                    clusters.push((r.start, r.end, x_pos, adv));
                    x_pos += adv;
                }
            }
        }

        let base_svg_clusters = self.collect_svg_clusters_from_layout(
            &base_layout,
            word.text.as_str(),
            base_origin_x,
            base_origin_y,
            None,
            None,
        );
        let ruby_svg_clusters = if let Some(ref r_layout) = ruby_layout {
            let r_origin_x = (total_width - ruby_width) * 0.5;
            let r_origin_y = (ruby_space - ruby_height).max(0.0) + pad_y * 0.5;
            self.collect_svg_clusters_from_layout(
                r_layout,
                word.reading.as_str(),
                r_origin_x,
                r_origin_y,
                None,
                None,
            )
        } else {
            Vec::new()
        };

        let pix_w = (total_width.ceil() as u32).max(1);
        let pix_h = (total_height.ceil() as u32).max(1);
        let mut pixmap =
            tiny_skia::Pixmap::new(pix_w, pix_h).unwrap_or_else(|| tiny_skia::Pixmap::new(1, 1).unwrap());

        if style.bg_rgba[3] > 0 {
            let mut paint = tiny_skia::Paint::default();
            paint.set_color_rgba8(
                style.bg_rgba[0],
                style.bg_rgba[1],
                style.bg_rgba[2],
                style.bg_rgba[3],
            );
            if let Some(rect) = tiny_skia::Rect::from_xywh(0.0, 0.0, pix_w as f32, pix_h as f32) {
                pixmap.fill_rect(rect, &paint, tiny_skia::Transform::default(), None);
            }
        }

        let mut svg = String::with_capacity(512);
        svg.push_str(&format!(
            "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{pix_w}\" height=\"{pix_h}\">"
        ));
        let ruby_stroke_px = (stroke_px * 0.65).max(if stroke_px > 0.0 { 1.0 } else { 0.0 });
        for (items, s_width) in [
            (ruby_svg_clusters.as_slice(), ruby_stroke_px),
            (base_svg_clusters.as_slice(), stroke_px),
        ] {
            if s_width > 0.0 && style.stroke_rgba[3] > 0 {
                // Crisp subtle drop-shadow pass for StrokeLabel parity
                append_svg_text_items(
                    &mut svg,
                    items,
                    Some(([0, 0, 0, (style.stroke_rgba[3] / 2).max(80)], s_width)),
                    1.0,
                    1.2,
                    true,
                );
            }
            append_svg_text_items(
                &mut svg,
                items,
                (s_width > 0.0).then_some((style.stroke_rgba, s_width)),
                0.0,
                0.0,
                false,
            );
        }
        svg.push_str("</svg>");

        let mut opts = usvg::Options::default();
        opts.fontdb = self.fontdb.clone();
        if let Ok(tree) = usvg::Tree::from_str(&svg, &opts) {
            resvg::render(&tree, tiny_skia::Transform::default(), &mut pixmap.as_mut());
        }

        let image = pixmap_to_slint_image(&pixmap);
        let shaped = ShapedSubtitleToken {
            width_px: total_width,
            height_px: total_height,
            glyph_x_px: base_origin_x,
            glyph_y_px: base_origin_y,
            glyph_width_px: base_width,
            glyph_height_px: base_height,
            clusters,
            image,
        };
        self.subtitle_cache.insert(key, shaped.clone());
        shaped
    }

    fn devoice_circle_image(&mut self) -> slint::Image {
        if let Some(ref img) = self.devoice_circle {
            return img.clone();
        }
        let size = 36u32;
        let mut pixmap = tiny_skia::Pixmap::new(size, size).unwrap();
        let mut pb = tiny_skia::PathBuilder::new();
        pb.push_circle(18.0, 18.0, 15.0);
        if let Some(path) = pb.finish() {
            let mut paint = tiny_skia::Paint::default();
            paint.set_color_rgba8(248, 113, 113, 235);
            paint.anti_alias = true;
            let mut stroke = tiny_skia::Stroke {
                width: 2.2,
                line_cap: tiny_skia::LineCap::Round,
                line_join: tiny_skia::LineJoin::Round,
                ..tiny_skia::Stroke::default()
            };
            stroke.dash = tiny_skia::StrokeDash::new(vec![3.2, 3.2], 0.0);
            pixmap.stroke_path(
                &path,
                &paint,
                &stroke,
                tiny_skia::Transform::default(),
                None,
            );
        }
        let img = pixmap_to_slint_image(&pixmap);
        self.devoice_circle = Some(img.clone());
        img
    }
}

trait BorrowFontExt {
    fn borrow_font(&self) -> &parley::FontData;
}

impl BorrowFontExt for parley::FontData {
    fn borrow_font(&self) -> &parley::FontData {
        self
    }
}

fn pixmap_to_slint_image(pixmap: &tiny_skia::Pixmap) -> slint::Image {
    let buf = SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(
        pixmap.data(),
        pixmap.width(),
        pixmap.height(),
    );
    slint::Image::from_rgba8_premultiplied(buf)
}

fn xml_escape_attr(s: &str, out: &mut String) {
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(ch),
        }
    }
}

fn xml_escape_text(s: &str, out: &mut String) {
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ if ch.is_control() && ch != '\t' => {}
            _ => out.push(ch),
        }
    }
}

fn rgba_hex_and_opacity(rgba: [u8; 4]) -> (String, f32) {
    (
        format!("#{:02x}{:02x}{:02x}", rgba[0], rgba[1], rgba[2]),
        f32::from(rgba[3]) / 255.0,
    )
}

fn append_svg_text_items(
    svg: &mut String,
    items: &[PositionedSvgCluster],
    stroke: Option<([u8; 4], f32)>,
    dx: f32,
    dy: f32,
    shadow_only: bool,
) {
    for item in items {
        if item.text.is_empty() {
            continue;
        }
        svg.push_str("<text xml:space=\"preserve\" x=\"");
        svg.push_str(&format!("{:.2}", item.x + dx));
        svg.push_str("\" y=\"");
        svg.push_str(&format!("{:.2}", item.baseline_y + dy));
        svg.push_str("\" font-family=\"");
        xml_escape_attr(&item.family, svg);
        svg.push_str("\" font-size=\"");
        svg.push_str(&format!("{:.2}", item.font_size));
        svg.push_str("\" font-weight=\"");
        svg.push_str(&item.weight.to_string());
        svg.push_str("\" font-style=\"");
        svg.push_str(if item.italic { "italic" } else { "normal" });
        svg.push('"');
        if item.underline && !shadow_only {
            svg.push_str(" text-decoration=\"underline\"");
        } else if item.strikethrough && !shadow_only {
            svg.push_str(" text-decoration=\"line-through\"");
        }
        if let Some((s_rgba, s_width)) = stroke {
            let (s_hex, s_op) = rgba_hex_and_opacity(s_rgba);
            svg.push_str(" paint-order=\"stroke\" stroke=\"");
            svg.push_str(&s_hex);
            svg.push_str("\" stroke-opacity=\"");
            svg.push_str(&format!("{:.3}", s_op));
            svg.push_str("\" stroke-width=\"");
            svg.push_str(&format!("{:.2}", s_width));
            svg.push_str("\" stroke-linejoin=\"round\" stroke-linecap=\"round\"");
        }
        if shadow_only {
            svg.push_str(" fill=\"none\">");
        } else {
            let (f_hex, f_op) = rgba_hex_and_opacity(item.fill_rgba);
            svg.push_str(" fill=\"");
            svg.push_str(&f_hex);
            svg.push_str("\" fill-opacity=\"");
            svg.push_str(&format!("{:.3}\">", f_op));
        }
        xml_escape_text(&item.text, svg);
        svg.push_str("</text>");
    }
}

/// Exact per-glyph cluster hit-testing on a subtitle token using `parley::Cluster::from_point`
/// and an explicit `SubtitleRenderStyle`.
pub fn subtitle_cluster_at_point_with_style(
    word: &SubtitleWord,
    font_px: f32,
    show_furigana: bool,
    style: &SubtitleRenderStyle,
    local_x: f32,
    local_y: f32,
) -> Option<(usize, usize)> {
    if word.text.is_empty() {
        return None;
    }
    SHARED_TEXT_ENGINE.with(|engine| {
        let mut engine = engine.borrow_mut();
        let shaped = engine.shape_and_render_subtitle_token(word, font_px, show_furigana, style);
        let text_x = local_x - shaped.glyph_x_px;
        let layout = engine.shape_single_line(
            word.text.as_str(),
            &style.font_family,
            font_px.clamp(8.0, 140.0),
            if style.bold { 700 } else { 400 },
            style.italic,
            style.text_rgba,
        );
        let probe_y = (local_y - shaped.glyph_y_px).clamp(0.5, (shaped.glyph_height_px - 0.5).max(0.5));
        let clamped_x = text_x.clamp(0.0, (shaped.glyph_width_px - 0.01).max(0.0));
        if let Some((cluster, _side)) = Cluster::from_point(&layout, clamped_x, probe_y) {
            let byte_offset = cluster.text_range().start.min(word.text.len());
            let char_offset = word
                .text
                .get(..byte_offset)
                .map_or(0, |prefix| prefix.chars().count());
            return Some((byte_offset, char_offset));
        }
        Some((0, 0))
    })
}

/// Exact per-glyph cluster hit-testing on a subtitle token using `parley::Cluster::from_point`.
/// Returns `(byte_offset_in_token, char_offset_in_token)` for `(local_x, local_y)`.
pub fn subtitle_cluster_at_point(
    word: &SubtitleWord,
    font_px: f32,
    show_furigana: bool,
    local_x: f32,
    local_y: f32,
) -> Option<(usize, usize)> {
    let style = active_subtitle_render_style();
    subtitle_cluster_at_point_with_style(word, font_px, show_furigana, &style, local_x, local_y)
}

// ── Tag Colors & Redundant Tag Suppression (#220) ───────────────────────────

pub fn tag_category_hex_color(category: &str) -> [u8; 4] {
    match category {
        "name" => [0xb6, 0x32, 0x7a, 0xff],
        "expression" => [0xf0, 0xad, 0x4e, 0xff],
        "popular" => [0x02, 0x75, 0xd8, 0xff],
        "frequent" => [0x5b, 0xc0, 0xde, 0xff],
        "archaism" => [0xd9, 0x53, 0x4f, 0xff],
        "dictionary" => [0xaa, 0x66, 0xcc, 0xff],
        "frequency" => [0x5c, 0xb8, 0x5c, 0xff],
        "partOfSpeech" => [0x56, 0x56, 0x56, 0xff],
        "search" => [0x8a, 0x8a, 0x91, 0xff],
        "pitch-accent-dictionary" => [0x66, 0x40, 0xbe, 0xff],
        _ => [0x8a, 0x8a, 0x91, 0xff],
    }
}

fn tag_brush(category: &str) -> slint::Brush {
    let [r, g, b, a] = tag_category_hex_color(category);
    slint::Color::from_argb_u8(a, r, g, b).into()
}

fn normalize_tag_key(name: &str) -> String {
    name.replace('⭐', "★").trim().to_lowercase()
}

/// Extract inline tag badge labels already embedded inside a Yomitan `structured-content` JSON
/// payload (`data-sc-class="tag"`, `data-sc-content="tag"`, `data-sc-content="part-of-speech-info"`).
pub fn extract_inline_structured_tags(raw_glossary: &str) -> HashSet<String> {
    fn walk(node: &DefJson, out: &mut HashSet<String>) {
        match node {
            DefJson::Array(items) => {
                for item in items {
                    walk(item, out);
                }
            }
            DefJson::Object(_) => {
                let data = node.get("data");
                let is_tag_node = data.get("class").as_str() == "tag"
                    || matches!(
                        data.get("content").as_str(),
                        "tag" | "part-of-speech-info" | "sense-tags"
                    );
                if is_tag_node {
                    let mut txt = String::new();
                    structured_content_text(node.get("content"), &mut txt);
                    let norm = normalize_tag_key(&txt);
                    if !norm.is_empty() {
                        out.insert(norm);
                    }
                    if let DefJson::String(title) = node.get("title") {
                        let norm_title = normalize_tag_key(title);
                        if !norm_title.is_empty() {
                            out.insert(norm_title);
                        }
                    }
                }
                walk(node.get("content"), out);
            }
            _ => {}
        }
    }

    let mut set = HashSet::new();
    if let Ok(parsed) = DefJson::parse(raw_glossary) {
        walk(&parsed, &mut set);
    }
    set
}

/// Suppress redundant tags (#220):
/// - Deduplicates `term_tags` across all rows of the same term group.
/// - Suppresses `def_tags` that duplicate a tag already shown in the term group's `term_tags`
///   or already rendered inline inside `structured-content`.
pub fn suppress_redundant_tags(
    term_tags: &[DictTag],
    def_tags: &[DictTag],
    raw_glossary: &str,
) -> (Vec<DictTag>, Vec<DictTag>) {
    let mut seen_term = HashSet::new();
    let mut filtered_term = Vec::new();
    for tag in term_tags {
        let display = tag.name.replace('⭐', "★");
        let key = normalize_tag_key(&display);
        if key.is_empty() || !seen_term.insert(key) {
            continue;
        }
        let mut t = tag.clone();
        t.name = display;
        filtered_term.push(t);
    }

    let inline_tags = extract_inline_structured_tags(raw_glossary);
    let mut seen_def = seen_term;
    let mut filtered_def = Vec::new();
    for tag in def_tags {
        let display = tag.name.replace('⭐', "★");
        let key = normalize_tag_key(&display);
        let notes_key = normalize_tag_key(&tag.notes);
        if key.is_empty() || !seen_def.insert(key.clone()) {
            continue;
        }
        if inline_tags.contains(&key)
            || (!notes_key.is_empty() && inline_tags.contains(&notes_key))
        {
            continue;
        }
        let mut t = tag.clone();
        t.name = display;
        filtered_def.push(t);
    }

    (filtered_term, filtered_def)
}

// ── Stylesheet & Local Dictionary Image Asset Caches ────────────────────────

fn convert_parsed_stylesheet(
    src: memento_dict::data::dictionarystyles_parser::ParsedStylesheet,
) -> ParsedStylesheet {
    fn u16_str(v: &[u16]) -> String {
        String::from_utf16_lossy(v)
    }
    fn conv_comb(c: memento_dict::data::dictionarystyles_parser::CssCombinator) -> mdef::CssCombinator {
        match c {
            memento_dict::data::dictionarystyles_parser::CssCombinator::Descendant => {
                mdef::CssCombinator::Descendant
            }
            memento_dict::data::dictionarystyles_parser::CssCombinator::Child => {
                mdef::CssCombinator::Child
            }
            memento_dict::data::dictionarystyles_parser::CssCombinator::AdjacentSibling => {
                mdef::CssCombinator::AdjacentSibling
            }
            memento_dict::data::dictionarystyles_parser::CssCombinator::GeneralSibling => {
                mdef::CssCombinator::GeneralSibling
            }
        }
    }
    fn conv_op(
        op: memento_dict::data::dictionarystyles_parser::CssAttributeOperator,
    ) -> mdef::CssAttributeOperator {
        match op {
            memento_dict::data::dictionarystyles_parser::CssAttributeOperator::Exists => {
                mdef::CssAttributeOperator::Exists
            }
            memento_dict::data::dictionarystyles_parser::CssAttributeOperator::Equals => {
                mdef::CssAttributeOperator::Equals
            }
            memento_dict::data::dictionarystyles_parser::CssAttributeOperator::StartsWith => {
                mdef::CssAttributeOperator::StartsWith
            }
        }
    }
    fn conv_pe(
        pe: memento_dict::data::dictionarystyles_parser::CssPseudoElement,
    ) -> mdef::CssPseudoElement {
        match pe {
            memento_dict::data::dictionarystyles_parser::CssPseudoElement::None => {
                mdef::CssPseudoElement::None
            }
            memento_dict::data::dictionarystyles_parser::CssPseudoElement::Before => {
                mdef::CssPseudoElement::Before
            }
            memento_dict::data::dictionarystyles_parser::CssPseudoElement::After => {
                mdef::CssPseudoElement::After
            }
        }
    }
    fn conv_pc(pc: memento_dict::data::dictionarystyles_parser::CssPseudoClass) -> mdef::CssPseudoClass {
        match pc {
            memento_dict::data::dictionarystyles_parser::CssPseudoClass::FirstChild => {
                mdef::CssPseudoClass::FirstChild
            }
            memento_dict::data::dictionarystyles_parser::CssPseudoClass::LastChild => {
                mdef::CssPseudoClass::LastChild
            }
            memento_dict::data::dictionarystyles_parser::CssPseudoClass::NthChild => {
                mdef::CssPseudoClass::NthChild
            }
        }
    }

    let rules = src
        .rules
        .into_iter()
        .map(|r| mdef::CssRule {
            selector: r
                .selector
                .into_iter()
                .map(|p| mdef::CssSelectorPart {
                    combinator: conv_comb(p.combinator),
                    tag: u16_str(&p.tag),
                    class_names: p.class_names.iter().map(|c| u16_str(c)).collect(),
                    attributes: p
                        .attributes
                        .into_iter()
                        .map(|a| mdef::CssAttributeSelector {
                            name: u16_str(&a.name),
                            value: u16_str(&a.value),
                            op: conv_op(a.op),
                        })
                        .collect(),
                    pseudo_classes: p
                        .pseudo_classes
                        .into_iter()
                        .map(|pc| mdef::CssPseudoClassSelector {
                            pseudo_type: conv_pc(pc.r#type),
                            child_index: pc.child_index,
                            negated: pc.negated,
                        })
                        .collect(),
                })
                .collect(),
            declarations: r
                .declarations
                .into_iter()
                .map(|d| {
                    let prop = u16_str(&d.property);
                    let mut val = u16_str(&d.value);
                    if prop == "content" {
                        val = val.replace("\\'", "'").replace("\\\"", "\"");
                    }
                    mdef::CssDeclaration {
                        property: prop,
                        value: val,
                    }
                })
                .collect(),
            pseudo_element: conv_pe(r.pseudo_element),
            specificity: r.specificity,
            order: r.order,
        })
        .collect();

    ParsedStylesheet {
        rules,
        universal_rule_indexes: src.universal_rule_indexes,
        rule_indexes_by_target_tag: src
            .rule_indexes_by_target_tag
            .into_iter()
            .map(|(k, v)| (u16_str(&k), v))
            .collect(),
        non_nth_rule_indexes_by_target_tag: src
            .non_nth_rule_indexes_by_target_tag
            .into_iter()
            .map(|(k, v)| (u16_str(&k), v))
            .collect(),
        rule_indexes_by_target_tag_and_child_index: src
            .rule_indexes_by_target_tag_and_child_index
            .into_iter()
            .map(|(k, map)| {
                (
                    u16_str(&k),
                    map.into_iter().map(|(idx, v)| (idx as i64, v)).collect(),
                )
            })
            .collect(),
        uses_element_child_count: src.uses_element_child_count,
        element_child_count_tags: src
            .element_child_count_tags
            .into_iter()
            .map(|t| u16_str(&t))
            .collect(),
        diagnostics: src.diagnostics.into_iter().map(|d| u16_str(&d)).collect(),
    }
}

/// Parse raw UTF-8 CSS text into a `memento_definition::ParsedStylesheet`.
pub fn parse_dictionary_css_str(css: &str) -> Arc<ParsedStylesheet> {
    let u16_css: Vec<u16> = css.encode_utf16().collect();
    let parsed = memento_dict::data::dictionarystyles_parser::parse_stylesheet(&u16_css);
    Arc::new(convert_parsed_stylesheet(parsed))
}

pub fn collect_dictionary_resource_dirs(
    config_dir: &Path,
    dictionary_db_path: &Path,
) -> Vec<PathBuf> {
    let mut dirs = vec![config_dir.join("dict_resources")];
    if let Some(parent) = dictionary_db_path.parent() {
        let res = parent.join("res");
        if !dirs.iter().any(|d| d == &res) {
            dirs.push(res);
        }
    }
    dirs
}

#[derive(Default)]
struct StylesheetCache {
    entries: HashMap<PathBuf, (Option<SystemTime>, Arc<ParsedStylesheet>)>,
}

impl StylesheetCache {
    fn get_for_dictionary(
        &mut self,
        dict_resource_dirs: &[PathBuf],
        dic_id: i64,
        dict_name: &str,
    ) -> Option<Arc<ParsedStylesheet>> {
        let style_path = dict_resource_dirs.iter().find_map(|dir| {
            memento_dict::databasemanager::resolve_dictionary_style_path(dir, dic_id, dict_name)
        })?;
        let mtime = std::fs::metadata(&style_path)
            .ok()
            .and_then(|m| m.modified().ok());
        if let Some((cached_mtime, sheet)) = self.entries.get(&style_path) {
            if *cached_mtime == mtime {
                return Some(sheet.clone());
            }
        }
        let css = std::fs::read_to_string(&style_path).ok()?;
        let parsed = parse_dictionary_css_str(&css);
        self.entries.insert(style_path, (mtime, parsed.clone()));
        Some(parsed)
    }
}

fn sanitize_relative_asset_path(rel: &str) -> Option<PathBuf> {
    let rel = rel.trim().trim_start_matches("./");
    if rel.is_empty() || rel.contains('\0') {
        return None;
    }
    let candidate = Path::new(rel);
    if candidate.is_absolute() {
        return None;
    }
    let mut clean = PathBuf::new();
    for comp in candidate.components() {
        match comp {
            Component::Normal(part) => clean.push(part),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    (!clean.as_os_str().is_empty()).then_some(clean)
}

pub fn resolve_dictionary_asset_path(
    dict_resources_dir: &Path,
    dic_id: i64,
    dict_name: &str,
    rel_path: &str,
) -> Option<PathBuf> {
    let clean_rel = sanitize_relative_asset_path(rel_path)?;
    if dic_id > 0 {
        let by_id = dict_resources_dir
            .join(format!("dict-{dic_id}"))
            .join(&clean_rel);
        if by_id.is_file() {
            return Some(by_id);
        }
    }
    if !dict_name.is_empty()
        && !dict_name.contains('/')
        && !dict_name.contains('\\')
        && dict_name != "."
        && dict_name != ".."
    {
        let by_name = dict_resources_dir.join(dict_name).join(&clean_rel);
        if by_name.is_file() {
            return Some(by_name);
        }
    }
    None
}

pub fn resolve_dictionary_asset_path_in_dirs(
    dict_resource_dirs: &[PathBuf],
    dic_id: i64,
    dict_name: &str,
    rel_path: &str,
) -> Option<PathBuf> {
    dict_resource_dirs
        .iter()
        .find_map(|dir| resolve_dictionary_asset_path(dir, dic_id, dict_name, rel_path))
}

/// Rasterizes a local dictionary image or SVG (supporting `appearance: "monochrome"` tinting via `resvg`)
/// into a `tiny_skia::Pixmap`.
pub fn load_dictionary_image_pixmap(
    asset_path: &Path,
    requested_w: Option<f32>,
    requested_h: Option<f32>,
    monochrome_tint: Option<[u8; 4]>,
    current_color_hex: &str,
) -> Option<Arc<tiny_skia::Pixmap>> {
    let bytes = std::fs::read(asset_path).ok()?;
    let ext = asset_path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let looks_svg = ext == "svg"
        || ext == "svgz"
        || bytes
            .iter()
            .copied()
            .skip_while(|b| b.is_ascii_whitespace())
            .take(5)
            .collect::<Vec<_>>()
            .starts_with(b"<svg")
        || bytes
            .iter()
            .copied()
            .skip_while(|b| b.is_ascii_whitespace())
            .take(5)
            .collect::<Vec<_>>()
            .starts_with(b"<?xml");

    if looks_svg {
        let mut opts = usvg::Options::default();
        opts.resources_dir = asset_path.parent().map(Path::to_path_buf);
        SHARED_TEXT_ENGINE.with(|engine| {
            opts.fontdb = engine.borrow().fontdb.clone();
        });
        if !current_color_hex.is_empty() {
            opts.style_sheet = Some(format!("svg {{ color: {current_color_hex}; }}"));
        }
        let tree = usvg::Tree::from_data(&bytes, &opts).ok()?;
        let intrinsic_w = tree.size().width().max(1.0);
        let intrinsic_h = tree.size().height().max(1.0);
        let (target_w, target_h) = match (requested_w, requested_h) {
            (Some(w), Some(h)) => (w.max(1.0), h.max(1.0)),
            (Some(w), None) => {
                let w = w.max(1.0);
                (w, (w * intrinsic_h / intrinsic_w).max(1.0))
            }
            (None, Some(h)) => {
                let h = h.max(1.0);
                ((h * intrinsic_w / intrinsic_h).max(1.0), h)
            }
            (None, None) => (intrinsic_w.clamp(8.0, 360.0), intrinsic_h.clamp(8.0, 360.0)),
        };
        let pw = (target_w.round() as u32).clamp(1, 1024);
        let ph = (target_h.round() as u32).clamp(1, 1024);
        let mut pixmap = tiny_skia::Pixmap::new(pw, ph)?;
        let transform = tiny_skia::Transform::from_scale(
            pw as f32 / intrinsic_w,
            ph as f32 / intrinsic_h,
        );
        resvg::render(&tree, transform, &mut pixmap.as_mut());

        if let Some([tr, tg, tb, ta]) = monochrome_tint {
            for px in pixmap.pixels_mut() {
                let a = (u32::from(px.alpha()) * u32::from(ta) + 127) / 255;
                let r = (u32::from(tr) * a + 127) / 255;
                let g = (u32::from(tg) * a + 127) / 255;
                let b = (u32::from(tb) * a + 127) / 255;
                if let Some(tinted) =
                    tiny_skia::PremultipliedColorU8::from_rgba(r as u8, g as u8, b as u8, a as u8)
                {
                    *px = tinted;
                }
            }
        }
        return Some(Arc::new(pixmap));
    }

    let decoded = image::load_from_memory(&bytes).ok()?.to_rgba8();
    let iw = decoded.width().max(1);
    let ih = decoded.height().max(1);
    let mut src_pixmap = tiny_skia::Pixmap::new(iw, ih)?;
    for (dst, src) in src_pixmap.pixels_mut().iter_mut().zip(decoded.pixels()) {
        let [r, g, b, a] = src.0;
        let pr = (u32::from(r) * u32::from(a) + 127) / 255;
        let pg = (u32::from(g) * u32::from(a) + 127) / 255;
        let pb = (u32::from(b) * u32::from(a) + 127) / 255;
        if let Some(pm) =
            tiny_skia::PremultipliedColorU8::from_rgba(pr as u8, pg as u8, pb as u8, a)
        {
            *dst = pm;
        }
    }
    let (target_w, target_h) = match (requested_w, requested_h) {
        (Some(w), Some(h)) => (w.max(1.0), h.max(1.0)),
        (Some(w), None) => {
            let w = w.max(1.0);
            (w, (w * ih as f32 / iw as f32).max(1.0))
        }
        (None, Some(h)) => {
            let h = h.max(1.0);
            ((h * iw as f32 / ih as f32).max(1.0), h)
        }
        (None, None) => {
            let scale = (340.0 / iw as f32).min(1.0);
            (iw as f32 * scale, ih as f32 * scale)
        }
    };
    let pw = (target_w.round() as u32).clamp(1, 1024);
    let ph = (target_h.round() as u32).clamp(1, 1024);
    if pw == iw && ph == ih {
        return Some(Arc::new(src_pixmap));
    }
    let mut dst_pixmap = tiny_skia::Pixmap::new(pw, ph)?;
    let paint = tiny_skia::PixmapPaint {
        opacity: 1.0,
        blend_mode: tiny_skia::BlendMode::SourceOver,
        quality: tiny_skia::FilterQuality::Bilinear,
    };
    dst_pixmap.draw_pixmap(
        0,
        0,
        src_pixmap.as_ref(),
        &paint,
        tiny_skia::Transform::from_scale(pw as f32 / iw as f32, ph as f32 / ih as f32),
        None,
    );
    Some(Arc::new(dst_pixmap))
}

// ── Rich Structured Content Layout & Painter (`memento-definition` + `parley` + `resvg`) ─

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GlossaryZoneAction {
    Link(String),
    ToggleDetails(String),
}

#[derive(Clone, Debug)]
pub struct GlossaryHitZone {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub action: Option<GlossaryZoneAction>,
    pub tooltip: Option<String>,
}

#[derive(Clone, Debug)]
pub struct GlossaryCharCell {
    pub ch: char,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub line_id: usize,
}

#[derive(Clone)]
pub struct RenderedGlossaryDoc {
    pub width_px: f32,
    pub height_px: f32,
    pub image: slint::Image,
    pub hit_zones: Vec<GlossaryHitZone>,
    pub selectable_chars: Vec<GlossaryCharCell>,
    pub plain_text: String,
}

#[derive(Clone, Debug)]
struct InlineStyleIR {
    font_family: String,
    font_px: f32,
    weight: u16,
    italic: bool,
    underline: bool,
    strikethrough: bool,
    color: [u8; 4],
    pill_bg: Option<[u8; 4]>,
    pill_border: Option<[u8; 4]>,
    link_href: Option<String>,
    tooltip: Option<String>,
}

impl Default for InlineStyleIR {
    fn default() -> Self {
        Self {
            font_family: "Noto Sans CJK JP".to_owned(),
            font_px: 14.5,
            weight: 400,
            italic: false,
            underline: false,
            strikethrough: false,
            color: [241, 245, 249, 255],
            pill_bg: None,
            pill_border: None,
            link_href: None,
            tooltip: None,
        }
    }
}

#[derive(Clone)]
enum InlineAtom {
    Text {
        text: String,
        style: InlineStyleIR,
        selectable: bool,
    },
    Ruby {
        base: String,
        reading: String,
        style: InlineStyleIR,
    },
    Image {
        pixmap: Arc<tiny_skia::Pixmap>,
        w: f32,
        h: f32,
        link_href: Option<String>,
        tooltip: Option<String>,
    },
    DetailsToggle {
        detail_id: String,
        open: bool,
        style: InlineStyleIR,
    },
    LineBreak,
}

#[derive(Clone, Default)]
struct BoxFrameIR {
    padding: [f32; 4],
    margin_top: f32,
    margin_bottom: f32,
    margin_left: f32,
    margin_right: f32,
    bg_color: Option<[u8; 4]>,
    borders: [Option<(f32, [u8; 4])>; 4],
}

#[derive(Clone)]
struct TableCellIR {
    col_span: usize,
    row_span: usize,
    blocks: Vec<BlockIR>,
    frame: BoxFrameIR,
}

#[derive(Clone)]
enum BlockIR {
    Paragraph {
        atoms: Vec<InlineAtom>,
        frame: BoxFrameIR,
        marker: Option<(String, [u8; 4], f32)>,
        summary_detail_id: Option<String>,
    },
    Container {
        blocks: Vec<BlockIR>,
        frame: BoxFrameIR,
        marker: Option<(String, [u8; 4], f32)>,
    },
    Table {
        rows: Vec<Vec<TableCellIR>>,
        frame: BoxFrameIR,
    },
}

fn parse_def_rgba(value: &str, fallback: [u8; 4]) -> [u8; 4] {
    if let Some(c) = DefColor::from_string(value.trim()) {
        let r = ((u32::from(c.red) + 128) / 257) as u8;
        let g = ((u32::from(c.green) + 128) / 257) as u8;
        let b = ((u32::from(c.blue) + 128) / 257) as u8;
        let a = ((u32::from(c.alpha) + 128) / 257) as u8;
        [r, g, b, a]
    } else {
        fallback
    }
}

fn parse_def_rgba_opt(value: &str) -> Option<[u8; 4]> {
    if value.trim().is_empty() || value.trim().eq_ignore_ascii_case("transparent") {
        return None;
    }
    DefColor::from_string(value.trim()).map(|c| {
        [
            ((u32::from(c.red) + 128) / 257) as u8,
            ((u32::from(c.green) + 128) / 257) as u8,
            ((u32::from(c.blue) + 128) / 257) as u8,
            ((u32::from(c.alpha) + 128) / 257) as u8,
        ]
    })
}

fn parse_weight_css(value: Option<&String>, default_weight: u16) -> u16 {
    let Some(v) = value else {
        return default_weight;
    };
    let lower = v.trim().to_ascii_lowercase();
    match lower.as_str() {
        "bold" | "bolder" => 700,
        "normal" => 400,
        "light" | "lighter" => 300,
        _ => lower.parse::<u16>().unwrap_or(default_weight).clamp(100, 900),
    }
}

/// Extract `<ruby>` `(base_text, reading)` pairs supporting both `<rp>` and `<rp>`-less
/// single or multi-segment Yomitan/Jitendex `<ruby>` nodes.
fn extract_ruby_pairs(ruby_content: &DefJson) -> Vec<(String, String)> {
    let children = match ruby_content {
        DefJson::Array(items) => items.as_slice(),
        _ => std::slice::from_ref(ruby_content),
    };
    let mut pairs: Vec<(String, String)> = Vec::new();
    let mut current_base = String::new();
    for child in children {
        if child.is_object() {
            match child.get("tag").as_str() {
                "rt" => {
                    let mut rt_text = String::new();
                    structured_content_text(child.get("content"), &mut rt_text);
                    pairs.push((std::mem::take(&mut current_base), rt_text));
                    continue;
                }
                "rp" => continue,
                _ => {}
            }
        }
        structured_content_text(child, &mut current_base);
    }
    if !current_base.is_empty() {
        pairs.push((current_base, String::new()));
    }
    if pairs.is_empty() {
        let parts = split_ruby_content(ruby_content);
        let mut base = String::new();
        for node in &parts.base {
            structured_content_text(node, &mut base);
        }
        if !base.is_empty() {
            pairs.push((base, parts.reading));
        }
    }
    pairs
}

struct GlossaryBuilder<'a> {
    renderer: DefRenderer,
    ctx: DefContext,
    dict_resource_dirs: &'a [PathBuf],
    dic_id: i64,
    dict_name: &'a str,
}

impl<'a> GlossaryBuilder<'a> {
    fn box_frame_from_state(&self, state: &mdef::ElementRenderState) -> BoxFrameIR {
        let mut borders = [None; 4];
        for side in 0..4 {
            let b = &state.box_style.borders[side];
            if !b.style.eq_ignore_ascii_case("none")
                && !b.style.is_empty()
                && b.width_pixels > 0.05
            {
                let rgba = parse_def_rgba(&b.color, [100, 116, 139, 200]);
                borders[side] = Some(((b.width_pixels as f32).clamp(1.0, 6.0), rgba));
            }
        }
        let margin_px = |side: usize| -> f32 {
            let v = mdef::helpers::css_font_size_to_pixels(
                &state.box_style.margins[side],
                self.ctx.screen.as_ref(),
                state.font_pixel_size,
                self.ctx.root_font_pixel_size,
            );
            if v.is_finite() && v > 0.0 {
                (v as f32).min(24.0)
            } else {
                0.0
            }
        };
        BoxFrameIR {
            padding: [
                state.box_style.padding[TOP] as f32,
                state.box_style.padding[RIGHT] as f32,
                state.box_style.padding[BOTTOM] as f32,
                state.box_style.padding[LEFT] as f32,
            ],
            margin_top: margin_px(TOP),
            margin_right: margin_px(RIGHT),
            margin_bottom: margin_px(BOTTOM),
            margin_left: margin_px(LEFT),
            bg_color: parse_def_rgba_opt(&state.box_style.background_color),
            borders,
        }
    }

    fn inline_style_from_state(
        &self,
        state: &mdef::ElementRenderState,
        inherited: &InlineStyleIR,
    ) -> InlineStyleIR {
        let mut next = inherited.clone();
        if state.font_pixel_size > 0.0 {
            next.font_px = (state.font_pixel_size as f32).clamp(9.0, 32.0);
        }
        if !state.text_color.is_empty() {
            next.color = parse_def_rgba(&state.text_color, inherited.color);
        }
        next.weight = parse_weight_css(state.declarations.get("font-weight"), inherited.weight);
        if let Some(fs) = state.declarations.get("font-style") {
            let l = fs.trim().to_ascii_lowercase();
            if l == "italic" || l == "oblique" {
                next.italic = true;
            } else if l == "normal" {
                next.italic = false;
            }
        }
        if let Some(td) = state.declarations.get("text-decoration") {
            let l = td.to_ascii_lowercase();
            if l.contains("none") {
                next.underline = false;
                next.strikethrough = false;
            } else {
                if l.contains("underline") {
                    next.underline = true;
                }
                if l.contains("line-through") {
                    next.strikethrough = true;
                }
            }
        }
        if !state.title_tooltip.is_empty() {
            next.tooltip = Some(state.title_tooltip.clone());
        }
        if !self.ctx.link_href.is_empty() {
            next.link_href = Some(self.ctx.link_href.clone());
        }
        if state.tag == "span" {
            if let Some(bg) = parse_def_rgba_opt(&state.box_style.background_color)
                .or_else(|| state.declarations.get("background-color").and_then(|s| parse_def_rgba_opt(s)))
            {
                next.pill_bg = Some(bg);
            }
            for side in 0..4 {
                if state.box_style.borders[side].width_pixels > 0.05 {
                    next.pill_border = Some(parse_def_rgba(
                        &state.box_style.borders[side].color,
                        next.color,
                    ));
                    break;
                }
            }
        }
        next
    }

    fn resolve_image_atom(
        &mut self,
        obj: &DefJson,
        inherited: &InlineStyleIR,
    ) -> Option<InlineAtom> {
        let index = self.renderer.structured_element(obj, &mut self.ctx, false);
        self.ctx.elements.push(index);

        let mut declarations = mdef::CssDeclarations::new();
        self.renderer
            .add_matching_css_rules(&mut self.ctx, &mut declarations, None, None);

        let rel_path = obj.get("path").as_str();
        let units = match obj.get("sizeUnits") {
            DefJson::String(u) if !u.is_empty() => u.as_str(),
            _ => "px",
        };
        let dim_px = |key: &str, ctx: &DefContext| -> Option<f32> {
            if let DefJson::Number(v) = obj.get(key) {
                let css_sz = format!("{v}{units}");
                let px = mdef::helpers::css_font_size_to_pixels(
                    &css_sz,
                    ctx.screen.as_ref(),
                    ctx.parent_font_pixel_size,
                    ctx.root_font_pixel_size,
                );
                if px > 0.0 {
                    return Some(px as f32);
                }
            }
            None
        };
        let req_w = dim_px("width", &self.ctx);
        let req_h = dim_px("height", &self.ctx);
        let is_monochrome = obj
            .get("appearance")
            .as_str()
            .eq_ignore_ascii_case("monochrome");
        let color_str = declarations
            .get("color")
            .cloned()
            .unwrap_or_else(|| self.ctx.text_color.clone());
        let tint = is_monochrome.then(|| parse_def_rgba(&color_str, inherited.color));

        let tooltip = match obj.get("title") {
            DefJson::String(t) if !t.is_empty() => Some(t.clone()),
            _ => inherited.tooltip.clone(),
        };
        let link_href = (!self.ctx.link_href.is_empty())
            .then(|| self.ctx.link_href.clone())
            .or_else(|| inherited.link_href.clone());

        self.ctx.elements.pop();

        let asset_path = resolve_dictionary_asset_path_in_dirs(
            self.dict_resource_dirs,
            self.dic_id,
            self.dict_name,
            rel_path,
        )?;
        let pixmap = load_dictionary_image_pixmap(&asset_path, req_w, req_h, tint, &color_str)?;
        let w = pixmap.width() as f32;
        let h = pixmap.height() as f32;
        Some(InlineAtom::Image {
            pixmap,
            w,
            h,
            link_href,
            tooltip,
        })
    }

    fn collect_table_rows(
        &mut self,
        node: &DefJson,
        inherited: &InlineStyleIR,
        rows_out: &mut Vec<Vec<TableCellIR>>,
    ) {
        match node {
            DefJson::Array(items) => {
                for item in items {
                    self.collect_table_rows(item, inherited, rows_out);
                }
            }
            DefJson::Object(_) => {
                let tag = node.get("tag").as_str().to_ascii_lowercase();
                match tag.as_str() {
                    "thead" | "tbody" | "tfoot" => {
                        let idx = self.renderer.structured_element(node, &mut self.ctx, false);
                        self.ctx.elements.push(idx);
                        let mut state = self.build_element_state(node, &tag);
                        let next_style = self.inline_style_from_state(&state, inherited);
                        std::mem::swap(&mut self.ctx.parent_font_pixel_size, &mut state.font_pixel_size);
                        std::mem::swap(&mut self.ctx.text_color, &mut state.text_color);
                        self.ctx.siblings.push(SiblingState::default());
                        self.collect_table_rows(node.get("content"), &next_style, rows_out);
                        self.ctx.siblings.pop();
                        std::mem::swap(&mut self.ctx.text_color, &mut state.text_color);
                        std::mem::swap(&mut self.ctx.parent_font_pixel_size, &mut state.font_pixel_size);
                        self.ctx.elements.pop();
                    }
                    "tr" => {
                        let idx = self.renderer.structured_element(node, &mut self.ctx, false);
                        self.ctx.elements.push(idx);
                        let mut state = self.build_element_state(node, "tr");
                        let row_style = self.inline_style_from_state(&state, inherited);
                        std::mem::swap(&mut self.ctx.parent_font_pixel_size, &mut state.font_pixel_size);
                        std::mem::swap(&mut self.ctx.text_color, &mut state.text_color);
                        self.ctx.siblings.push(SiblingState::default());
                        let mut cells = Vec::new();
                        self.collect_tr_cells(node.get("content"), &row_style, &mut cells);
                        self.ctx.siblings.pop();
                        std::mem::swap(&mut self.ctx.text_color, &mut state.text_color);
                        std::mem::swap(&mut self.ctx.parent_font_pixel_size, &mut state.font_pixel_size);
                        self.ctx.elements.pop();
                        if !cells.is_empty() {
                            rows_out.push(cells);
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    fn collect_tr_cells(
        &mut self,
        node: &DefJson,
        inherited: &InlineStyleIR,
        cells_out: &mut Vec<TableCellIR>,
    ) {
        match node {
            DefJson::Array(items) => {
                for item in items {
                    self.collect_tr_cells(item, inherited, cells_out);
                }
            }
            DefJson::Object(_) => {
                let tag = node.get("tag").as_str().to_ascii_lowercase();
                if tag != "td" && tag != "th" {
                    return;
                }
                let idx = self.renderer.structured_element(node, &mut self.ctx, false);
                self.ctx.elements.push(idx);
                let mut state = self.build_element_state(node, &tag);
                // Ensure table cells retain their default 5px padding & 1px border when resolve_box_style skipped Inline
                self.renderer.resolve_box_style(&self.ctx, &mut state);
                let mut cell_style = self.inline_style_from_state(&state, inherited);
                if tag == "th" && cell_style.weight < 600 {
                    cell_style.weight = 700;
                }
                let mut frame = self.box_frame_from_state(&state);
                for p in &mut frame.padding {
                    if *p < 4.0 {
                        *p = 5.0;
                    }
                }
                for b in &mut frame.borders {
                    if b.is_none() {
                        *b = Some((1.0, [82, 97, 107, 180]));
                    }
                }
                if tag == "th" && frame.bg_color.is_none() {
                    frame.bg_color = Some([43, 50, 56, 220]);
                }

                let col_span = match node.get("colSpan") {
                    DefJson::Number(n) if *n >= 1.0 => (*n as usize).min(16),
                    _ => 1,
                };
                let row_span = match node.get("rowSpan") {
                    DefJson::Number(n) if *n >= 1.0 => (*n as usize).min(32),
                    _ => 1,
                };

                std::mem::swap(&mut self.ctx.parent_font_pixel_size, &mut state.font_pixel_size);
                std::mem::swap(&mut self.ctx.text_color, &mut state.text_color);
                let blocks = self.build_children_blocks(node.get("content"), &cell_style);
                std::mem::swap(&mut self.ctx.text_color, &mut state.text_color);
                std::mem::swap(&mut self.ctx.parent_font_pixel_size, &mut state.font_pixel_size);
                self.ctx.elements.pop();

                cells_out.push(TableCellIR {
                    col_span,
                    row_span,
                    blocks,
                    frame,
                });
            }
            _ => {}
        }
    }

    fn build_element_state(&mut self, obj: &DefJson, tag: &str) -> mdef::ElementRenderState {
        let mut state = mdef::ElementRenderState {
            tag: tag.to_owned(),
            output_tag: tag.to_owned(),
            font_pixel_size: self.ctx.parent_font_pixel_size,
            text_color: self.ctx.text_color.clone(),
            title_tooltip: self.ctx.title_tooltip.clone(),
            text_direction: self.ctx.text_direction,
            painted_background_color: self.ctx.painted_background_color.clone(),
            text_only_content: obj.get("content").is_string(),
            ..mdef::ElementRenderState::default()
        };
        if tag == "summary" && self.ctx.elements.len() > 1 && !self.ctx.details.is_empty() {
            let parent_idx = self.ctx.elements[self.ctx.elements.len() - 2] as usize;
            if self.ctx.selector_elements[parent_idx].tag == "details" {
                state.disclosure_summary = true;
                state.details = self.ctx.details.last().unwrap().clone();
            }
        }
        self.renderer.add_element_attributes(obj, &mut state);
        self.renderer
            .resolve_element_styles(obj, &mut self.ctx, &mut state);
        self.renderer.resolve_element_layout(&mut self.ctx, &mut state);
        self.renderer.apply_element_compatibility(&self.ctx, &mut state);
        state
    }

    fn build_children_blocks(
        &mut self,
        content: &DefJson,
        inherited: &InlineStyleIR,
    ) -> Vec<BlockIR> {
        let mut siblings = SiblingState::default();
        if self.ctx.needs_element_child_count
            && self
                .renderer
                .needs_structured_element_child_count(content, &self.ctx)
        {
            siblings.element_count = self.renderer.structured_element_child_count(content);
        }
        self.ctx.siblings.push(siblings);
        let mut blocks = Vec::new();
        let mut inline_buf: Vec<InlineAtom> = Vec::new();
        self.walk_node(content, inherited, &mut inline_buf, &mut blocks);
        if !inline_buf.is_empty() {
            blocks.push(BlockIR::Paragraph {
                atoms: inline_buf,
                frame: BoxFrameIR::default(),
                marker: None,
                summary_detail_id: None,
            });
        }
        self.ctx.siblings.pop();
        blocks
    }

    fn walk_node(
        &mut self,
        node: &DefJson,
        inherited: &InlineStyleIR,
        inline_out: &mut Vec<InlineAtom>,
        blocks_out: &mut Vec<BlockIR>,
    ) {
        let flush_inline = |inline_out: &mut Vec<InlineAtom>, blocks_out: &mut Vec<BlockIR>| {
            if !inline_out.is_empty() {
                blocks_out.push(BlockIR::Paragraph {
                    atoms: std::mem::take(inline_out),
                    frame: BoxFrameIR::default(),
                    marker: None,
                    summary_detail_id: None,
                });
            }
        };

        match node {
            DefJson::String(text) => {
                if !text.is_empty() {
                    let mut parts = text.split('\n').peekable();
                    while let Some(seg) = parts.next() {
                        if !seg.is_empty() {
                            inline_out.push(InlineAtom::Text {
                                text: seg.to_owned(),
                                style: inherited.clone(),
                                selectable: true,
                            });
                        }
                        if parts.peek().is_some() {
                            inline_out.push(InlineAtom::LineBreak);
                        }
                    }
                }
            }
            DefJson::Array(items) => {
                for item in items {
                    self.walk_node(item, inherited, inline_out, blocks_out);
                }
            }
            DefJson::Object(_) => {
                let raw_tag = node.get("tag").as_str();
                let tag = raw_tag.to_ascii_lowercase();
                if !self.renderer.is_supported_structured_tag(&tag) {
                    return;
                }
                match tag.as_str() {
                    "rt" | "rp" => {}
                    "br" => {
                        let idx = self.renderer.structured_element(node, &mut self.ctx, false);
                        self.ctx.elements.push(idx);
                        inline_out.push(InlineAtom::LineBreak);
                        self.ctx.elements.pop();
                    }
                    "img" => {
                        if let Some(atom) = self.resolve_image_atom(node, inherited) {
                            inline_out.push(atom);
                        }
                    }
                    "ruby" => {
                        let idx = self.renderer.structured_element(node, &mut self.ctx, false);
                        self.ctx.elements.push(idx);
                        let mut state = self.build_element_state(node, "ruby");
                        let ruby_style = self.inline_style_from_state(&state, inherited);
                        for (base, reading) in extract_ruby_pairs(node.get("content")) {
                            if base.is_empty() {
                                continue;
                            }
                            if reading.is_empty() {
                                inline_out.push(InlineAtom::Text {
                                    text: base,
                                    style: ruby_style.clone(),
                                    selectable: true,
                                });
                            } else {
                                let mut s = ruby_style.clone();
                                if s.tooltip.is_none() {
                                    s.tooltip = Some(format!("{base}: {reading}"));
                                }
                                inline_out.push(InlineAtom::Ruby {
                                    base,
                                    reading,
                                    style: s,
                                });
                            }
                        }
                        let _ = &mut state;
                        self.ctx.elements.pop();
                    }
                    "a" => {
                        let idx = self.renderer.structured_element(node, &mut self.ctx, false);
                        self.ctx.elements.push(idx);
                        let state = self.build_element_state(node, "a");
                        let old_href = if let DefJson::String(href) = node.get("href") {
                            Some(std::mem::replace(&mut self.ctx.link_href, href.clone()))
                        } else {
                            None
                        };
                        let old_title = if let DefJson::String(title) = node.get("title") {
                            Some(std::mem::replace(&mut self.ctx.title_tooltip, title.clone()))
                        } else {
                            None
                        };
                        let mut link_style = self.inline_style_from_state(&state, inherited);
                        if !self.ctx.link_href.is_empty() {
                            link_style.link_href = Some(self.ctx.link_href.clone());
                            if state.declarations.get("color").is_none() {
                                link_style.color = [147, 197, 253, 255];
                            }
                            if !self.ctx.suppress_link_decoration
                                && state.declarations.get("text-decoration").is_none()
                            {
                                link_style.underline = true;
                            }
                        }
                        if !state.before_content.is_empty() {
                            inline_out.push(InlineAtom::Text {
                                text: state.before_content.clone(),
                                style: link_style.clone(),
                                selectable: false,
                            });
                        }
                        self.ctx.siblings.push(SiblingState::default());
                        self.walk_node(node.get("content"), &link_style, inline_out, blocks_out);
                        self.ctx.siblings.pop();
                        if !state.after_content.is_empty() {
                            inline_out.push(InlineAtom::Text {
                                text: state.after_content.clone(),
                                style: link_style,
                                selectable: false,
                            });
                        }
                        if let Some(t) = old_title {
                            self.ctx.title_tooltip = t;
                        }
                        if let Some(h) = old_href {
                            self.ctx.link_href = h;
                        }
                        self.ctx.elements.pop();
                    }
                    "table" => {
                        flush_inline(inline_out, blocks_out);
                        let idx = self.renderer.structured_element(node, &mut self.ctx, false);
                        self.ctx.elements.push(idx);
                        let mut state = self.build_element_state(node, "table");
                        self.renderer.resolve_box_style(&self.ctx, &mut state);
                        let table_style = self.inline_style_from_state(&state, inherited);
                        let frame = self.box_frame_from_state(&state);
                        std::mem::swap(&mut self.ctx.parent_font_pixel_size, &mut state.font_pixel_size);
                        std::mem::swap(&mut self.ctx.text_color, &mut state.text_color);
                        self.ctx.siblings.push(SiblingState::default());
                        let mut rows = Vec::new();
                        self.collect_table_rows(node.get("content"), &table_style, &mut rows);
                        self.ctx.siblings.pop();
                        std::mem::swap(&mut self.ctx.text_color, &mut state.text_color);
                        std::mem::swap(&mut self.ctx.parent_font_pixel_size, &mut state.font_pixel_size);
                        self.ctx.elements.pop();
                        if !rows.is_empty() {
                            blocks_out.push(BlockIR::Table { rows, frame });
                        }
                    }
                    _ => {
                        let is_details = tag == "details";
                        let details = is_details.then(|| self.renderer.details_state(node, &mut self.ctx));
                        let idx = self.renderer.structured_element(
                            node,
                            &mut self.ctx,
                            details.as_ref().is_some_and(|d| d.open),
                        );
                        self.ctx.elements.push(idx);
                        let mut state = self.build_element_state(node, &tag);
                        if let Some(d) = details {
                            state.details = d;
                        }
                        let elem_style = self.inline_style_from_state(&state, inherited);

                        if state.layout == ElementLayout::Inline && tag == "span" {
                            if !state.before_content.is_empty() {
                                inline_out.push(InlineAtom::Text {
                                    text: state.before_content.clone(),
                                    style: elem_style.clone(),
                                    selectable: false,
                                });
                            }
                            std::mem::swap(
                                &mut self.ctx.parent_font_pixel_size,
                                &mut state.font_pixel_size,
                            );
                            std::mem::swap(&mut self.ctx.text_color, &mut state.text_color);
                            std::mem::swap(&mut self.ctx.title_tooltip, &mut state.title_tooltip);
                            self.ctx.siblings.push(SiblingState::default());
                            self.walk_node(node.get("content"), &elem_style, inline_out, blocks_out);
                            self.ctx.siblings.pop();
                            std::mem::swap(&mut self.ctx.title_tooltip, &mut state.title_tooltip);
                            std::mem::swap(&mut self.ctx.text_color, &mut state.text_color);
                            std::mem::swap(
                                &mut self.ctx.parent_font_pixel_size,
                                &mut state.font_pixel_size,
                            );
                            if !state.after_content.is_empty() {
                                inline_out.push(InlineAtom::Text {
                                    text: state.after_content.clone(),
                                    style: elem_style,
                                    selectable: false,
                                });
                            }
                            self.ctx.elements.pop();
                            return;
                        }

                        // Block / List / Box / Details / Summary element
                        flush_inline(inline_out, blocks_out);
                        if state.layout == ElementLayout::List {
                            self.ctx.lists.push(mdef::StructuredList {
                                tag: tag.clone(),
                                marker: state.list_marker_type.clone(),
                                item: 0,
                            });
                        }
                        if is_details {
                            self.ctx.details.push(state.details.clone());
                        }

                        let mut frame = self.box_frame_from_state(&state);
                        if is_details && frame.padding == [0.0; 4] {
                            frame.padding = [4.0, 6.0, 4.0, 6.0];
                            if frame.bg_color.is_none() {
                                frame.bg_color = Some([34, 40, 46, 180]);
                            }
                        }

                        std::mem::swap(
                            &mut self.ctx.parent_font_pixel_size,
                            &mut state.font_pixel_size,
                        );
                        std::mem::swap(&mut self.ctx.text_color, &mut state.text_color);
                        std::mem::swap(&mut self.ctx.title_tooltip, &mut state.title_tooltip);
                        std::mem::swap(
                            &mut self.ctx.painted_background_color,
                            &mut state.painted_background_color,
                        );

                        let child_blocks = if is_details && !state.details.open {
                            let mut summary_blocks = Vec::new();
                            let mut siblings = SiblingState::default();
                            self.ctx.siblings.push(siblings);
                            self.walk_closed_details(
                                node.get("content"),
                                &elem_style,
                                &mut summary_blocks,
                            );
                            siblings = self.ctx.siblings.pop().unwrap_or_default();
                            let _ = siblings;
                            summary_blocks
                        } else {
                            let mut inner_siblings = SiblingState::default();
                            if self.ctx.needs_element_child_count
                                && self
                                    .renderer
                                    .needs_structured_element_child_count(node.get("content"), &self.ctx)
                            {
                                inner_siblings.element_count =
                                    self.renderer.structured_element_child_count(node.get("content"));
                            }
                            self.ctx.siblings.push(inner_siblings);
                            let mut inner_blocks = Vec::new();
                            let mut inner_inline = Vec::new();
                            if state.disclosure_summary {
                                inner_inline.push(InlineAtom::DetailsToggle {
                                    detail_id: state.details.id.clone(),
                                    open: state.details.open,
                                    style: elem_style.clone(),
                                });
                            }
                            if !state.before_content.is_empty() {
                                inner_inline.push(InlineAtom::Text {
                                    text: state.before_content.clone(),
                                    style: elem_style.clone(),
                                    selectable: false,
                                });
                            }
                            self.walk_node(
                                node.get("content"),
                                &elem_style,
                                &mut inner_inline,
                                &mut inner_blocks,
                            );
                            if !state.after_content.is_empty() {
                                inner_inline.push(InlineAtom::Text {
                                    text: state.after_content.clone(),
                                    style: elem_style.clone(),
                                    selectable: false,
                                });
                            }
                            if !inner_inline.is_empty() {
                                inner_blocks.push(BlockIR::Paragraph {
                                    atoms: inner_inline,
                                    frame: BoxFrameIR::default(),
                                    marker: None,
                                    summary_detail_id: state
                                        .disclosure_summary
                                        .then(|| state.details.id.clone()),
                                });
                            }
                            self.ctx.siblings.pop();
                            inner_blocks
                        };

                        std::mem::swap(
                            &mut self.ctx.painted_background_color,
                            &mut state.painted_background_color,
                        );
                        std::mem::swap(&mut self.ctx.title_tooltip, &mut state.title_tooltip);
                        std::mem::swap(&mut self.ctx.text_color, &mut state.text_color);
                        std::mem::swap(
                            &mut self.ctx.parent_font_pixel_size,
                            &mut state.font_pixel_size,
                        );

                        if state.layout == ElementLayout::List {
                            self.ctx.lists.pop();
                        }
                        if is_details {
                            self.ctx.details.pop();
                        }
                        self.ctx.elements.pop();

                        let marker = (!state.list_marker.is_empty()).then(|| {
                            (
                                state.list_marker.clone(),
                                elem_style.color,
                                elem_style.font_px,
                            )
                        });
                        if child_blocks.len() == 1 && frame_is_trivial(&frame) && marker.is_none() {
                            blocks_out.extend(child_blocks);
                        } else if !child_blocks.is_empty() {
                            blocks_out.push(BlockIR::Container {
                                blocks: child_blocks,
                                frame,
                                marker,
                            });
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn walk_closed_details(
        &mut self,
        node: &DefJson,
        inherited: &InlineStyleIR,
        blocks_out: &mut Vec<BlockIR>,
    ) {
        match node {
            DefJson::Array(items) => {
                for item in items {
                    self.walk_closed_details(item, inherited, blocks_out);
                }
            }
            DefJson::Object(_) if node.get("tag").as_str().eq_ignore_ascii_case("summary") => {
                let mut inline_buf = Vec::new();
                self.walk_node(node, inherited, &mut inline_buf, blocks_out);
                if !inline_buf.is_empty() {
                    blocks_out.push(BlockIR::Paragraph {
                        atoms: inline_buf,
                        frame: BoxFrameIR::default(),
                        marker: None,
                        summary_detail_id: None,
                    });
                }
            }
            DefJson::Object(_) => {
                self.renderer.skip_closed_details_content(node, &mut self.ctx);
            }
            _ => {}
        }
    }
}

fn frame_is_trivial(f: &BoxFrameIR) -> bool {
    f.bg_color.is_none()
        && f.padding == [0.0; 4]
        && f.margin_top == 0.0
        && f.margin_bottom == 0.0
        && f.margin_left == 0.0
        && f.margin_right == 0.0
        && f.borders.iter().all(Option::is_none)
}

#[derive(Default)]
struct PaintCollector {
    rects: Vec<(f32, f32, f32, f32, [u8; 4])>,
    images: Vec<(f32, f32, Arc<tiny_skia::Pixmap>)>,
    svg_texts: Vec<PositionedSvgCluster>,
    hit_zones: Vec<GlossaryHitZone>,
    selectable_chars: Vec<GlossaryCharCell>,
    next_line_id: usize,
}

impl PaintCollector {
    fn push_frame(&mut self, x: f32, y: f32, w: f32, h: f32, frame: &BoxFrameIR) {
        if w <= 0.0 || h <= 0.0 {
            return;
        }
        if let Some(bg) = frame.bg_color {
            self.rects.push((x, y, w, h, bg));
        }
        if let Some((bw, col)) = frame.borders[TOP] {
            self.rects.push((x, y, w, bw, col));
        }
        if let Some((bw, col)) = frame.borders[BOTTOM] {
            self.rects.push((x, (y + h - bw).max(y), w, bw, col));
        }
        if let Some((bw, col)) = frame.borders[LEFT] {
            self.rects.push((x, y, bw, h, col));
        }
        if let Some((bw, col)) = frame.borders[RIGHT] {
            self.rects.push(((x + w - bw).max(x), y, bw, h, col));
        }
    }
}

fn layout_and_collect_block(
    engine: &mut TextEngine,
    block: &BlockIR,
    x: f32,
    y: f32,
    avail_w: f32,
    collector: &mut PaintCollector,
    measure_only: bool,
) -> (f32, f32) {
    match block {
        BlockIR::Paragraph {
            atoms,
            frame,
            marker,
            summary_detail_id,
        } => {
            let outer_x = x + frame.margin_left;
            let outer_y = y + frame.margin_top;
            let box_w = (avail_w - frame.margin_left - frame.margin_right).max(24.0);
            let border_l = frame.borders[LEFT].map_or(0.0, |(w, _)| w);
            let border_r = frame.borders[RIGHT].map_or(0.0, |(w, _)| w);
            let border_t = frame.borders[TOP].map_or(0.0, |(w, _)| w);
            let border_b = frame.borders[BOTTOM].map_or(0.0, |(w, _)| w);

            let mut content_x = outer_x + border_l + frame.padding[LEFT];
            let content_y = outer_y + border_t + frame.padding[TOP];
            let mut content_w =
                (box_w - border_l - border_r - frame.padding[LEFT] - frame.padding[RIGHT]).max(16.0);

            let mut marker_w = 0.0;
            if let Some((ref m_txt, m_col, m_px)) = *marker {
                let m_layout =
                    engine.shape_single_line(m_txt, "Noto Sans CJK JP", m_px, 600, false, m_col);
                let mw = m_layout
                    .lines()
                    .next()
                    .map_or(m_px, |l| l.metrics().advance)
                    + 6.0;
                marker_w = mw;
                if !measure_only {
                    let clusters = engine.collect_svg_clusters_from_layout(
                        &m_layout, m_txt, content_x, content_y, None, None,
                    );
                    collector.svg_texts.extend(clusters);
                }
                content_x += mw;
                content_w = (content_w - mw).max(16.0);
            }

            let (para_used_w, para_h) = layout_inline_atoms(
                engine,
                atoms,
                content_x,
                content_y,
                content_w,
                summary_detail_id.as_deref(),
                collector,
                measure_only,
            );
            let inner_h = para_h.max(if marker.is_some() { 18.0 } else { 0.0 });
            let box_h = border_t + frame.padding[TOP] + inner_h + frame.padding[BOTTOM] + border_b;
            let used_box_w = if frame.bg_color.is_some() || frame.borders.iter().any(Option::is_some) {
                box_w
            } else {
                (border_l + frame.padding[LEFT] + marker_w + para_used_w + frame.padding[RIGHT] + border_r)
                    .min(box_w)
            };

            if !measure_only {
                collector.push_frame(outer_x, outer_y, box_w, box_h, frame);
                if let Some(detail_id) = summary_detail_id {
                    collector.hit_zones.push(GlossaryHitZone {
                        x: outer_x,
                        y: outer_y,
                        w: box_w,
                        h: box_h.max(18.0),
                        action: Some(GlossaryZoneAction::ToggleDetails(detail_id.clone())),
                        tooltip: None,
                    });
                }
            }
            (
                frame.margin_left + used_box_w + frame.margin_right,
                frame.margin_top + box_h + frame.margin_bottom,
            )
        }
        BlockIR::Container {
            blocks,
            frame,
            marker,
        } => {
            let outer_x = x + frame.margin_left;
            let outer_y = y + frame.margin_top;
            let box_w = (avail_w - frame.margin_left - frame.margin_right).max(24.0);
            let border_l = frame.borders[LEFT].map_or(0.0, |(w, _)| w);
            let border_r = frame.borders[RIGHT].map_or(0.0, |(w, _)| w);
            let border_t = frame.borders[TOP].map_or(0.0, |(w, _)| w);
            let border_b = frame.borders[BOTTOM].map_or(0.0, |(w, _)| w);

            let mut content_x = outer_x + border_l + frame.padding[LEFT];
            let content_y = outer_y + border_t + frame.padding[TOP];
            let mut content_w =
                (box_w - border_l - border_r - frame.padding[LEFT] - frame.padding[RIGHT]).max(16.0);

            let rect_insert_idx = collector.rects.len();

            let mut marker_w = 0.0;
            if let Some((ref m_txt, m_col, m_px)) = *marker {
                let m_layout =
                    engine.shape_single_line(m_txt, "Noto Sans CJK JP", m_px, 600, false, m_col);
                let mw = m_layout
                    .lines()
                    .next()
                    .map_or(m_px, |l| l.metrics().advance)
                    + 6.0;
                marker_w = mw;
                if !measure_only {
                    let clusters = engine.collect_svg_clusters_from_layout(
                        &m_layout, m_txt, content_x, content_y, None, None,
                    );
                    collector.svg_texts.extend(clusters);
                }
                content_x += mw;
                content_w = (content_w - mw).max(16.0);
            }

            let mut cur_y = content_y;
            let mut max_child_w = 0.0f32;
            for (idx, child) in blocks.iter().enumerate() {
                if idx > 0 {
                    cur_y += 2.0;
                }
                let (cw, ch) = layout_and_collect_block(
                    engine,
                    child,
                    content_x,
                    cur_y,
                    content_w,
                    collector,
                    measure_only,
                );
                max_child_w = max_child_w.max(cw);
                cur_y += ch;
            }
            let inner_h = (cur_y - content_y).max(0.0);
            let box_h = border_t + frame.padding[TOP] + inner_h + frame.padding[BOTTOM] + border_b;

            if !measure_only && !frame_is_trivial(frame) {
                let mut frame_collector = PaintCollector::default();
                frame_collector.push_frame(outer_x, outer_y, box_w, box_h, frame);
                collector
                    .rects
                    .splice(rect_insert_idx..rect_insert_idx, frame_collector.rects);
            }
            let used_w = border_l
                + frame.padding[LEFT]
                + marker_w
                + max_child_w
                + frame.padding[RIGHT]
                + border_r;
            (
                frame.margin_left + used_w.min(box_w) + frame.margin_right,
                frame.margin_top + box_h + frame.margin_bottom,
            )
        }
        BlockIR::Table { rows, frame } => layout_table_block(
            engine,
            rows,
            frame,
            x,
            y,
            avail_w,
            collector,
            measure_only,
        ),
    }
}

#[derive(Clone)]
struct AtomSpanMeta {
    byte_range: std::ops::Range<usize>,
    style: InlineStyleIR,
    selectable: bool,
    detail_toggle: Option<String>,
}

struct RubyBoxItem {
    box_id: u64,
    base: String,
    reading: String,
    style: InlineStyleIR,
    w: f32,
    h: f32,
    base_y: f32,
    ruby_h: f32,
}

struct ImageBoxItem {
    box_id: u64,
    pixmap: Arc<tiny_skia::Pixmap>,
    w: f32,
    h: f32,
    link_href: Option<String>,
    tooltip: Option<String>,
}

fn layout_inline_atoms(
    engine: &mut TextEngine,
    atoms: &[InlineAtom],
    origin_x: f32,
    origin_y: f32,
    avail_w: f32,
    summary_detail_id: Option<&str>,
    collector: &mut PaintCollector,
    measure_only: bool,
) -> (f32, f32) {
    if atoms.is_empty() {
        return (0.0, 0.0);
    }

    let mut full_text = String::new();
    let mut spans: Vec<AtomSpanMeta> = Vec::new();
    let mut ruby_boxes: Vec<RubyBoxItem> = Vec::new();
    let mut image_boxes: Vec<ImageBoxItem> = Vec::new();
    let mut inline_boxes: Vec<InlineBox> = Vec::new();
    let mut next_box_id = 1u64;

    for atom in atoms {
        match atom {
            InlineAtom::Text {
                text,
                style,
                selectable,
            } => {
                if text.is_empty() {
                    continue;
                }
                let has_pill = style.pill_bg.is_some() || style.pill_border.is_some();
                let start = full_text.len();
                if has_pill {
                    full_text.push('\u{00A0}');
                }
                full_text.push_str(text);
                if has_pill {
                    full_text.push('\u{00A0}');
                }
                let end = full_text.len();
                spans.push(AtomSpanMeta {
                    byte_range: start..end,
                    style: style.clone(),
                    selectable: *selectable,
                    detail_toggle: None,
                });
                if has_pill {
                    full_text.push(' ');
                }
            }
            InlineAtom::DetailsToggle {
                detail_id,
                open,
                style,
            } => {
                let start = full_text.len();
                full_text.push(if *open { '▾' } else { '▸' });
                full_text.push(' ');
                let end = full_text.len();
                spans.push(AtomSpanMeta {
                    byte_range: start..end,
                    style: style.clone(),
                    selectable: false,
                    detail_toggle: Some(detail_id.clone()),
                });
            }
            InlineAtom::LineBreak => {
                full_text.push('\n');
            }
            InlineAtom::Ruby {
                base,
                reading,
                style,
            } => {
                let base_l = engine.shape_single_line(
                    base,
                    &style.font_family,
                    style.font_px,
                    style.weight,
                    style.italic,
                    style.color,
                );
                let ruby_px = (style.font_px * 0.58).max(8.5);
                let ruby_l = engine.shape_single_line(
                    reading,
                    &style.font_family,
                    ruby_px,
                    style.weight,
                    false,
                    [196, 181, 253, 255],
                );
                let bw = base_l.lines().next().map_or(style.font_px, |l| l.metrics().advance);
                let bh = base_l
                    .lines()
                    .next()
                    .map_or(style.font_px * 1.2, |l| l.metrics().line_height);
                let rw = ruby_l.lines().next().map_or(0.0, |l| l.metrics().advance);
                let rh = ruby_l
                    .lines()
                    .next()
                    .map_or(ruby_px * 1.1, |l| l.metrics().line_height);
                let w = bw.max(rw) + 2.0;
                let h = rh + bh;
                let id = next_box_id;
                next_box_id += 1;
                inline_boxes.push(InlineBox {
                    id,
                    index: full_text.len(),
                    width: w,
                    height: h,
                });
                ruby_boxes.push(RubyBoxItem {
                    box_id: id,
                    base: base.clone(),
                    reading: reading.clone(),
                    style: style.clone(),
                    w,
                    h,
                    base_y: rh,
                    ruby_h: rh,
                });
            }
            InlineAtom::Image {
                pixmap,
                w,
                h,
                link_href,
                tooltip,
            } => {
                let scale = if *w > avail_w { avail_w / *w } else { 1.0 };
                let dw = (*w * scale).max(1.0);
                let dh = (*h * scale).max(1.0);
                let id = next_box_id;
                next_box_id += 1;
                inline_boxes.push(InlineBox {
                    id,
                    index: full_text.len(),
                    width: dw + 2.0,
                    height: dh + 2.0,
                });
                image_boxes.push(ImageBoxItem {
                    box_id: id,
                    pixmap: pixmap.clone(),
                    w: dw,
                    h: dh,
                    link_href: link_href.clone(),
                    tooltip: tooltip.clone(),
                });
            }
        }
    }

    let mut builder = engine
        .layout_cx
        .ranged_builder(&mut engine.font_cx, &full_text, 1.0, true);
    builder.push_default(StyleProperty::FontFamily(FontFamily::Source(Cow::Borrowed(
        "Noto Sans CJK JP, sans-serif",
    ))));
    builder.push_default(StyleProperty::FontSize(14.5));
    builder.push_default(StyleProperty::LineHeight(LineHeight::MetricsRelative(1.18)));
    builder.push_default(StyleProperty::Brush([241, 245, 249, 255]));

    for span in &spans {
        builder.push(
            StyleProperty::FontSize(span.style.font_px),
            span.byte_range.clone(),
        );
        builder.push(
            StyleProperty::FontWeight(FontWeight::new(span.style.weight as f32)),
            span.byte_range.clone(),
        );
        if span.style.italic {
            builder.push(
                StyleProperty::FontStyle(FontStyle::Italic),
                span.byte_range.clone(),
            );
        }
        builder.push(StyleProperty::Brush(span.style.color), span.byte_range.clone());
        if span.style.underline {
            builder.push(StyleProperty::Underline(true), span.byte_range.clone());
        }
        if span.style.strikethrough {
            builder.push(StyleProperty::Strikethrough(true), span.byte_range.clone());
        }
    }
    for ibox in inline_boxes {
        builder.push_inline_box(ibox);
    }

    let mut layout = builder.build(&full_text);
    layout.break_all_lines(Some(avail_w.max(16.0)));
    layout.align(Some(avail_w.max(16.0)), Alignment::Start, AlignmentOptions::default());

    let mut max_line_w = 0.0f32;
    for line in layout.lines() {
        max_line_w = max_line_w.max(line.metrics().advance - line.metrics().trailing_whitespace);
    }
    let total_h = layout.height();
    if measure_only {
        return (max_line_w.max(0.0), total_h);
    }

    let mut byte_to_span = vec![usize::MAX; full_text.len() + 1];
    for (s_idx, span) in spans.iter().enumerate() {
        for b in span.byte_range.start..span.byte_range.end.min(byte_to_span.len()) {
            byte_to_span[b] = s_idx;
        }
    }

    for line in layout.lines() {
        let line_id = collector.next_line_id;
        collector.next_line_id += 1;
        let line_top = origin_y + line.metrics().min_coord;
        let line_h = line.metrics().line_height.max(14.0);
        let baseline = origin_y + line.metrics().baseline;

        for item in line.items() {
            match item {
                PositionedLayoutItem::GlyphRun(glyph_run) => {
                    let run = glyph_run.run();
                    let font_info = engine.ensure_font_registered(run.font().clone().borrow_font());
                    let font_size = run.font_size();
                    let brush = glyph_run.style().brush;
                    let run_ul = glyph_run.style().underline.is_some();
                    let run_st = glyph_run.style().strikethrough.is_some();
                    let mut x_cursor = origin_x + glyph_run.offset();

                    for cluster in run.visual_clusters() {
                        let range = cluster.text_range();
                        let adv = cluster.advance();
                        let span_meta = byte_to_span
                            .get(range.start)
                            .copied()
                            .filter(|&idx| idx != usize::MAX)
                            .and_then(|idx| spans.get(idx));

                        if let Some(meta) = span_meta {
                            if let Some(bg) = meta.style.pill_bg {
                                collector.rects.push((
                                    x_cursor,
                                    (baseline - font_size * 0.92).max(line_top),
                                    adv + 0.5,
                                    font_size * 1.18,
                                    bg,
                                ));
                            }
                            if let Some(border_col) = meta.style.pill_border {
                                collector.rects.push((
                                    x_cursor,
                                    (baseline - font_size * 0.92).max(line_top),
                                    adv + 0.5,
                                    1.0,
                                    border_col,
                                ));
                                collector.rects.push((
                                    x_cursor,
                                    (baseline + font_size * 0.24),
                                    adv + 0.5,
                                    1.0,
                                    border_col,
                                ));
                            }
                            let action = if let Some(ref did) = meta.detail_toggle {
                                Some(GlossaryZoneAction::ToggleDetails(did.clone()))
                            } else if let Some(did) = summary_detail_id {
                                Some(GlossaryZoneAction::ToggleDetails(did.to_owned()))
                            } else {
                                meta.style.link_href.clone().map(GlossaryZoneAction::Link)
                            };
                            if action.is_some() || meta.style.tooltip.is_some() {
                                collector.hit_zones.push(GlossaryHitZone {
                                    x: x_cursor,
                                    y: line_top,
                                    w: adv.max(4.0),
                                    h: line_h,
                                    action,
                                    tooltip: meta.style.tooltip.clone(),
                                });
                            }
                        }

                        if let Some(slice) = full_text.get(range.clone()) {
                            if !slice.is_empty() && !slice.chars().all(|c| c == '\n' || c == '\r') {
                                collector.svg_texts.push(PositionedSvgCluster {
                                    x: x_cursor,
                                    baseline_y: baseline,
                                    text: slice.to_owned(),
                                    family: font_info.family.clone(),
                                    font_size,
                                    weight: font_info.weight,
                                    italic: font_info.italic,
                                    fill_rgba: brush,
                                    underline: run_ul,
                                    strikethrough: run_st,
                                });
                                let selectable = span_meta.is_none_or(|m| m.selectable);
                                if selectable {
                                    let chars: Vec<char> = slice.chars().collect();
                                    let per_w = adv / (chars.len().max(1) as f32);
                                    for (ci, ch) in chars.into_iter().enumerate() {
                                        let norm_ch = if ch == '\u{00A0}' { ' ' } else { ch };
                                        collector.selectable_chars.push(GlossaryCharCell {
                                            ch: norm_ch,
                                            x: x_cursor + per_w * ci as f32,
                                            y: line_top,
                                            w: per_w.max(2.0),
                                            h: line_h,
                                            line_id,
                                        });
                                    }
                                }
                            }
                        }
                        x_cursor += adv;
                    }
                }
                PositionedLayoutItem::InlineBox(ibox) => {
                    let bx = origin_x + ibox.x;
                    let by = origin_y + ibox.y;
                    if let Some(rbox) = ruby_boxes.iter().find(|r| r.box_id == ibox.id) {
                        let base_l = engine.shape_single_line(
                            &rbox.base,
                            &rbox.style.font_family,
                            rbox.style.font_px,
                            rbox.style.weight,
                            rbox.style.italic,
                            rbox.style.color,
                        );
                        let bw = base_l
                            .lines()
                            .next()
                            .map_or(rbox.style.font_px, |l| l.metrics().advance);
                        let ruby_px = (rbox.style.font_px * 0.58).max(8.5);
                        let ruby_l = engine.shape_single_line(
                            &rbox.reading,
                            &rbox.style.font_family,
                            ruby_px,
                            rbox.style.weight,
                            false,
                            [196, 181, 253, 255],
                        );
                        let rw = ruby_l.lines().next().map_or(0.0, |l| l.metrics().advance);
                        let base_clusters = engine.collect_svg_clusters_from_layout(
                            &base_l,
                            &rbox.base,
                            bx + (rbox.w - bw) * 0.5,
                            by + rbox.base_y,
                            Some(&vec![rbox.style.underline; rbox.base.len() + 1]),
                            None,
                        );
                        let ruby_clusters = engine.collect_svg_clusters_from_layout(
                            &ruby_l,
                            &rbox.reading,
                            bx + (rbox.w - rw) * 0.5,
                            by,
                            None,
                            None,
                        );
                        collector.svg_texts.extend(ruby_clusters);
                        collector.svg_texts.extend(base_clusters);

                        let base_chars: Vec<char> = rbox.base.chars().collect();
                        let per_w = rbox.w / (base_chars.len().max(1) as f32);
                        for (ci, ch) in base_chars.into_iter().enumerate() {
                            collector.selectable_chars.push(GlossaryCharCell {
                                ch,
                                x: bx + per_w * ci as f32,
                                y: by + rbox.ruby_h,
                                w: per_w.max(2.0),
                                h: (rbox.h - rbox.ruby_h).max(12.0),
                                line_id,
                            });
                        }

                        let action = if let Some(did) = summary_detail_id {
                            Some(GlossaryZoneAction::ToggleDetails(did.to_owned()))
                        } else {
                            rbox.style.link_href.clone().map(GlossaryZoneAction::Link)
                        };
                        collector.hit_zones.push(GlossaryHitZone {
                            x: bx,
                            y: by,
                            w: rbox.w,
                            h: rbox.h,
                            action,
                            tooltip: rbox
                                .style
                                .tooltip
                                .clone()
                                .or_else(|| Some(format!("{}: {}", rbox.base, rbox.reading))),
                        });
                    } else if let Some(img) = image_boxes.iter().find(|im| im.box_id == ibox.id) {
                        collector.images.push((bx + 1.0, by + 1.0, img.pixmap.clone()));
                        if img.link_href.is_some() || img.tooltip.is_some() {
                            collector.hit_zones.push(GlossaryHitZone {
                                x: bx,
                                y: by,
                                w: img.w + 2.0,
                                h: img.h + 2.0,
                                action: img.link_href.clone().map(GlossaryZoneAction::Link),
                                tooltip: img.tooltip.clone(),
                            });
                        }
                    }
                }
            }
        }
        if !collector.selectable_chars.is_empty()
            && collector.selectable_chars.last().is_some_and(|c| c.ch != '\n')
        {
            collector.selectable_chars.push(GlossaryCharCell {
                ch: '\n',
                x: origin_x + avail_w,
                y: line_top,
                w: 0.0,
                h: line_h,
                line_id,
            });
        }
    }

    (max_line_w.max(0.0), total_h)
}

#[derive(Clone)]
struct PlacedCell<'a> {
    cell: &'a TableCellIR,
    row: usize,
    col: usize,
    col_span: usize,
    row_span: usize,
}

fn layout_table_block(
    engine: &mut TextEngine,
    rows: &[Vec<TableCellIR>],
    frame: &BoxFrameIR,
    x: f32,
    y: f32,
    avail_w: f32,
    collector: &mut PaintCollector,
    measure_only: bool,
) -> (f32, f32) {
    let num_rows = rows.len();
    if num_rows == 0 {
        return (0.0, 0.0);
    }

    let mut occupied: HashSet<(usize, usize)> = HashSet::new();
    let mut placed: Vec<PlacedCell<'_>> = Vec::new();
    let mut num_cols = 0usize;

    for (r_idx, row) in rows.iter().enumerate() {
        let mut c_idx = 0usize;
        for cell in row {
            while occupied.contains(&(r_idx, c_idx)) {
                c_idx += 1;
            }
            let cs = cell.col_span.max(1);
            let rs = cell.row_span.max(1).min(num_rows - r_idx);
            for dr in 0..rs {
                for dc in 0..cs {
                    occupied.insert((r_idx + dr, c_idx + dc));
                }
            }
            num_cols = num_cols.max(c_idx + cs);
            placed.push(PlacedCell {
                cell,
                row: r_idx,
                col: c_idx,
                col_span: cs,
                row_span: rs,
            });
            c_idx += cs;
        }
    }
    if num_cols == 0 {
        return (0.0, 0.0);
    }

    let outer_x = x + frame.margin_left;
    let outer_y = y + frame.margin_top;
    let max_table_w = (avail_w - frame.margin_left - frame.margin_right).max(40.0);

    let mut col_pref = vec![36.0f32; num_cols];
    let mut dummy_collector = PaintCollector::default();
    for p in &placed {
        let pad_h = p.cell.frame.padding[LEFT] + p.cell.frame.padding[RIGHT] + 4.0;
        let mut natural_w = 24.0f32;
        for blk in &p.cell.blocks {
            let (bw, _) = layout_and_collect_block(
                engine,
                blk,
                0.0,
                0.0,
                max_table_w,
                &mut dummy_collector,
                true,
            );
            natural_w = natural_w.max(bw);
        }
        let needed = (natural_w + pad_h) / (p.col_span as f32);
        for dc in 0..p.col_span {
            col_pref[p.col + dc] = col_pref[p.col + dc].max(needed);
        }
    }

    let sum_pref: f32 = col_pref.iter().sum();
    let col_widths: Vec<f32> = if sum_pref > max_table_w && sum_pref > 0.0 {
        col_pref
            .iter()
            .map(|w| (w * max_table_w / sum_pref).max(22.0))
            .collect()
    } else {
        col_pref
    };
    let table_w: f32 = col_widths.iter().sum::<f32>().min(max_table_w);

    let mut col_offsets = Vec::with_capacity(num_cols + 1);
    let mut acc_x = 0.0f32;
    for &cw in &col_widths {
        col_offsets.push(acc_x);
        acc_x += cw;
    }
    col_offsets.push(acc_x);

    let mut row_heights = vec![22.0f32; num_rows];
    let mut cell_measured_heights = Vec::with_capacity(placed.len());
    for p in &placed {
        let cell_w = (col_offsets[p.col + p.col_span] - col_offsets[p.col]).max(18.0);
        let inner_w =
            (cell_w - p.cell.frame.padding[LEFT] - p.cell.frame.padding[RIGHT] - 2.0).max(12.0);
        let mut ch_sum = 0.0f32;
        for (idx, blk) in p.cell.blocks.iter().enumerate() {
            if idx > 0 {
                ch_sum += 2.0;
            }
            let (_, bh) = layout_and_collect_block(
                engine,
                blk,
                0.0,
                0.0,
                inner_w,
                &mut dummy_collector,
                true,
            );
            ch_sum += bh;
        }
        let total_ch = ch_sum + p.cell.frame.padding[TOP] + p.cell.frame.padding[BOTTOM] + 2.0;
        cell_measured_heights.push(total_ch);
        if p.row_span == 1 {
            row_heights[p.row] = row_heights[p.row].max(total_ch);
        }
    }
    for (p, &total_ch) in placed.iter().zip(&cell_measured_heights) {
        if p.row_span > 1 {
            let spanned: f32 = row_heights[p.row..p.row + p.row_span].iter().sum();
            if total_ch > spanned {
                let extra = (total_ch - spanned) / (p.row_span as f32);
                for r in p.row..p.row + p.row_span {
                    row_heights[r] += extra;
                }
            }
        }
    }

    let mut row_offsets = Vec::with_capacity(num_rows + 1);
    let mut acc_y = 0.0f32;
    for &rh in &row_heights {
        row_offsets.push(acc_y);
        acc_y += rh;
    }
    row_offsets.push(acc_y);
    let table_h = acc_y;

    if !measure_only {
        collector.push_frame(outer_x, outer_y, table_w, table_h, frame);
        for p in &placed {
            let cx = outer_x + col_offsets[p.col];
            let cy = outer_y + row_offsets[p.row];
            let cw = col_offsets[p.col + p.col_span] - col_offsets[p.col];
            let ch = row_offsets[p.row + p.row_span] - row_offsets[p.row];
            collector.push_frame(cx, cy, cw, ch, &p.cell.frame);
            let inner_x = cx + p.cell.frame.padding[LEFT] + 1.0;
            let mut inner_y = cy + p.cell.frame.padding[TOP] + 1.0;
            let inner_w =
                (cw - p.cell.frame.padding[LEFT] - p.cell.frame.padding[RIGHT] - 2.0).max(12.0);
            for (idx, blk) in p.cell.blocks.iter().enumerate() {
                if idx > 0 {
                    inner_y += 2.0;
                }
                let (_, bh) = layout_and_collect_block(
                    engine,
                    blk,
                    inner_x,
                    inner_y,
                    inner_w,
                    collector,
                    false,
                );
                inner_y += bh;
            }
        }
    }

    (
        frame.margin_left + table_w + frame.margin_right,
        frame.margin_top + table_h + frame.margin_bottom,
    )
}

/// Build and rasterize a rich structured-content glossary entry into a `RenderedGlossaryDoc`
/// searching an ordered slice of dictionary resource directories (`config_dir/dict_resources`, `data_dir/res`).
pub fn render_structured_glossary_entry_in_dirs(
    raw_glossary: &str,
    fallback_glossary: &str,
    stylesheet: Option<Arc<ParsedStylesheet>>,
    detail_states: &HashMap<String, bool>,
    legacy_style: LegacyGlossaryStyle,
    dict_resource_dirs: &[PathBuf],
    dic_id: i64,
    dict_name: &str,
    avail_width_px: f32,
    selected: bool,
) -> Option<RenderedGlossaryDoc> {
    let avail_w = avail_width_px.clamp(120.0, 960.0);
    let base_text_color = if selected {
        [248, 250, 252, 255]
    } else {
        [148, 163, 184, 255]
    };
    let text_color_hex = if selected { "#F8FAFC" } else { "#94A3B8" };

    let parsed = if !raw_glossary.trim().is_empty() {
        DefJson::parse(raw_glossary).ok()
    } else if !fallback_glossary.trim().is_empty() {
        Some(DefJson::Array(vec![DefJson::String(
            fallback_glossary.to_owned(),
        )]))
    } else {
        None
    }?;

    let entries: Vec<DefJson> = match parsed {
        DefJson::Array(items) => items,
        single => vec![single],
    };
    if entries.is_empty() {
        return None;
    }

    let renderer = DefRenderer::new();
    let contains_structured = renderer.contains_structured_content(&entries);
    let ctx = DefContext {
        screen: Some(ScreenMetrics {
            available_width: avail_w as i32,
            available_height: 440,
            logical_dpi: 96.0,
        }),
        root_font_pixel_size: 14.5,
        parent_font_pixel_size: 14.5,
        text_color: text_color_hex.to_owned(),
        glossary_text_color: "#F8FAFC".to_owned(),
        glossary_background_color: "#1C2024".to_owned(),
        background_color: "#1C2024".to_owned(),
        painted_background_color: "#1C2024".to_owned(),
        needs_element_child_count: stylesheet
            .as_ref()
            .is_some_and(|s| s.uses_element_child_count),
        dictionary_styles: stylesheet,
        text_direction: LayoutDirection::LeftToRight,
        detail_states: detail_states.clone(),
        ..DefContext::default()
    };

    let root_inline = InlineStyleIR {
        color: base_text_color,
        ..InlineStyleIR::default()
    };

    let mut builder = GlossaryBuilder {
        renderer,
        ctx,
        dict_resource_dirs,
        dic_id,
        dict_name,
    };

    let mut top_blocks: Vec<BlockIR> = Vec::new();
    let use_bullets = legacy_style == LegacyGlossaryStyle::Bullet && !contains_structured;

    for (idx, entry) in entries.iter().enumerate() {
        match entry {
            DefJson::String(text) => {
                let mut atoms = Vec::new();
                for (li, line) in text.split('\n').enumerate() {
                    if li > 0 {
                        atoms.push(InlineAtom::LineBreak);
                    }
                    atoms.push(InlineAtom::Text {
                        text: line.to_owned(),
                        style: root_inline.clone(),
                        selectable: true,
                    });
                }
                if !use_bullets && idx + 1 < entries.len() && legacy_style == LegacyGlossaryStyle::Pipe {
                    atoms.push(InlineAtom::Text {
                        text: " | ".to_owned(),
                        style: root_inline.clone(),
                        selectable: true,
                    });
                }
                top_blocks.push(BlockIR::Paragraph {
                    atoms,
                    frame: BoxFrameIR::default(),
                    marker: use_bullets.then(|| ("•".to_owned(), base_text_color, 14.5)),
                    summary_detail_id: None,
                });
            }
            DefJson::Object(_) => match entry.get("type").as_str() {
                "structured-content" => {
                    let blocks = builder.build_children_blocks(entry.get("content"), &root_inline);
                    top_blocks.extend(blocks);
                }
                "image" => {
                    if let Some(atom) = builder.resolve_image_atom(entry, &root_inline) {
                        let mut atoms = vec![atom];
                        if let DefJson::String(desc) = entry.get("description") {
                            if !desc.is_empty() {
                                atoms.push(InlineAtom::LineBreak);
                                atoms.push(InlineAtom::Text {
                                    text: desc.clone(),
                                    style: root_inline.clone(),
                                    selectable: true,
                                });
                            }
                        }
                        top_blocks.push(BlockIR::Paragraph {
                            atoms,
                            frame: BoxFrameIR::default(),
                            marker: None,
                            summary_detail_id: None,
                        });
                    }
                }
                "text" => {
                    let txt = entry.get("text").as_str();
                    if !txt.is_empty() {
                        top_blocks.push(BlockIR::Paragraph {
                            atoms: vec![InlineAtom::Text {
                                text: txt.to_owned(),
                                style: root_inline.clone(),
                                selectable: true,
                            }],
                            frame: BoxFrameIR::default(),
                            marker: use_bullets.then(|| ("•".to_owned(), base_text_color, 14.5)),
                            summary_detail_id: None,
                        });
                    }
                }
                _ => {}
            },
            _ => {}
        }
    }

    if top_blocks.is_empty() {
        return None;
    }

    SHARED_TEXT_ENGINE.with(|engine_cell| {
        let mut engine = engine_cell.borrow_mut();
        let mut collector = PaintCollector::default();
        let mut cur_y = 2.0f32;
        for (idx, blk) in top_blocks.iter().enumerate() {
            if idx > 0 {
                cur_y += 4.0;
            }
            let (_, bh) =
                layout_and_collect_block(&mut engine, blk, 0.0, cur_y, avail_w, &mut collector, false);
            cur_y += bh;
        }
        let total_h = (cur_y + 2.0).ceil().max(18.0);
        let pw = (avail_w.ceil() as u32).clamp(1, 1400);
        let ph = (total_h.ceil() as u32).clamp(1, 8192);
        let mut pixmap = tiny_skia::Pixmap::new(pw, ph)?;

        for &(rx, ry, rw, rh, col) in &collector.rects {
            if let Some(rect) = tiny_skia::Rect::from_xywh(rx, ry, rw.max(0.5), rh.max(0.5)) {
                let mut paint = tiny_skia::Paint::default();
                paint.set_color_rgba8(col[0], col[1], col[2], col[3]);
                pixmap.fill_rect(rect, &paint, tiny_skia::Transform::default(), None);
            }
        }

        let img_paint = tiny_skia::PixmapPaint {
            opacity: 1.0,
            blend_mode: tiny_skia::BlendMode::SourceOver,
            quality: tiny_skia::FilterQuality::Bilinear,
        };
        for (ix, iy, img_pm) in &collector.images {
            pixmap.draw_pixmap(
                ix.round() as i32,
                iy.round() as i32,
                (**img_pm).as_ref(),
                &img_paint,
                tiny_skia::Transform::default(),
                None,
            );
        }

        if !collector.svg_texts.is_empty() {
            let mut svg = String::with_capacity(collector.svg_texts.len() * 96 + 96);
            svg.push_str(&format!(
                "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{pw}\" height=\"{ph}\">"
            ));
            append_svg_text_items(&mut svg, &collector.svg_texts, None, 0.0, 0.0, false);
            svg.push_str("</svg>");

            let mut opts = usvg::Options::default();
            opts.fontdb = engine.fontdb.clone();
            if let Ok(tree) = usvg::Tree::from_str(&svg, &opts) {
                resvg::render(&tree, tiny_skia::Transform::default(), &mut pixmap.as_mut());
            }
        }

        while collector
            .selectable_chars
            .last()
            .is_some_and(|c| c.ch == '\n')
        {
            collector.selectable_chars.pop();
        }
        let plain_text: String = collector.selectable_chars.iter().map(|c| c.ch).collect();

        Some(RenderedGlossaryDoc {
            width_px: pw as f32,
            height_px: ph as f32,
            image: pixmap_to_slint_image(&pixmap),
            hit_zones: collector.hit_zones,
            selectable_chars: collector.selectable_chars,
            plain_text,
        })
    })
}

/// Build and rasterize a rich structured-content glossary entry into a `RenderedGlossaryDoc`
/// with full `memento-definition` CSS styling, tables, ruby, details, links, images/SVGs, and
/// per-character selection hit-boxes.
pub fn render_structured_glossary_entry(
    raw_glossary: &str,
    fallback_glossary: &str,
    stylesheet: Option<Arc<ParsedStylesheet>>,
    detail_states: &HashMap<String, bool>,
    legacy_style: LegacyGlossaryStyle,
    dict_resources_dir: &Path,
    dic_id: i64,
    dict_name: &str,
    avail_width_px: f32,
    selected: bool,
) -> Option<RenderedGlossaryDoc> {
    let dirs = [dict_resources_dir.to_path_buf()];
    render_structured_glossary_entry_in_dirs(
        raw_glossary,
        fallback_glossary,
        stylesheet,
        detail_states,
        legacy_style,
        &dirs,
        dic_id,
        dict_name,
        avail_width_px,
        selected,
    )
}

// ── SQLite Helper Queries for Rich Tag Metadata & 3-Column Kanji Details ────

struct RichDbHelper {
    path: PathBuf,
    fingerprint: crate::controller::dict::DbFileFingerprint,
    conn: Option<memento_dict::sqlite_db::rusqlite::Connection>,
    tags: HashMap<(i64, String), DictTag>,
}

impl RichDbHelper {
    fn new(path: PathBuf) -> Self {
        let mut helper = Self {
            path,
            fingerprint: crate::controller::dict::DbFileFingerprint::default(),
            conn: None,
            tags: HashMap::new(),
        };
        helper.refresh_if_needed();
        helper
    }

    fn refresh_if_needed(&mut self) {
        if !self.path.is_file() {
            self.conn = None;
            self.tags.clear();
            return;
        }
        let fp = crate::controller::dict::db_file_fingerprint(&self.path);
        if self.conn.is_some() && self.fingerprint == fp {
            return;
        }
        self.fingerprint = fp;
        self.tags.clear();
        let Ok(conn) = memento_dict::sqlite_db::open_read_only_connection(&self.path) else {
            self.conn = None;
            return;
        };
        if let Ok(mut stmt) = conn.prepare(
            "SELECT dic_id, category, name, ord, notes, score FROM tag_bank ORDER BY dic_id ASC, ord ASC, score DESC;",
        ) {
            if let Ok(mut rows) = stmt.query([]) {
                while let Ok(Some(row)) = rows.next() {
                    if let (Ok(dic_id), Ok(cat), Ok(name), Ok(ord), Ok(notes), Ok(score)) = (
                        row.get::<_, i64>(0),
                        row.get::<_, String>(1),
                        row.get::<_, String>(2),
                        row.get::<_, i64>(3),
                        row.get::<_, String>(4),
                        row.get::<_, i64>(5),
                    ) {
                        self.tags.insert(
                            (dic_id, name.clone()),
                            DictTag::with_details(name, cat, notes, ord, score),
                        );
                    }
                }
            }
        }
        self.conn = Some(conn);
    }

    fn resolve_tag_list(&self, dic_id: i64, space_sep: &str) -> Vec<DictTag> {
        let mut seen = HashSet::new();
        let mut list = Vec::new();
        for tok in space_sep.split_whitespace() {
            if tok.is_empty() || !seen.insert(tok) {
                continue;
            }
            if let Some(tag) = self.tags.get(&(dic_id, tok.to_owned())) {
                list.push(tag.clone());
            } else {
                list.push(DictTag::with_details(tok, "", "", 0, 0));
            }
        }
        list.sort_by(|a, b| {
            a.order
                .cmp(&b.order)
                .then_with(|| b.score.cmp(&a.score))
                .then_with(|| a.name.cmp(&b.name))
        });
        list
    }

    fn build_kanji_detail(&mut self, character: &str, dic_id: i64) -> Option<RichKanjiDetail> {
        self.refresh_if_needed();
        let conn = self.conn.as_ref()?;
        let rows = memento_dict::sqlite_db::query_kanji_utf8_conn(conn, character).ok()?;
        let row = rows
            .iter()
            .find(|r| dic_id == 0 || r.dic_id == dic_id)
            .or_else(|| rows.first())?;

        let mut readings = Vec::new();
        for part in row
            .onyomi
            .split_whitespace()
            .chain(row.kunyomi.split_whitespace())
        {
            if !part.is_empty() {
                readings.push(SharedString::from(part));
            }
        }

        let meanings: Vec<SharedString> = row
            .meanings
            .iter()
            .filter(|m| !m.trim().is_empty())
            .map(|m| SharedString::from(m.trim()))
            .collect();

        let mut stats = Vec::new();
        let mut classes = Vec::new();
        let mut codes = Vec::new();
        let mut indices = Vec::new();

        if let Ok(serde_json::Value::Object(map)) =
            serde_json::from_str::<serde_json::Value>(&row.stats)
        {
            let mut entries: Vec<(&DictTag, String)> = map
                .iter()
                .filter_map(|(k, v)| {
                    let tag = self.tags.get(&(row.dic_id, k.clone()))?;
                    let val = match v {
                        serde_json::Value::String(s) => s.clone(),
                        serde_json::Value::Number(n) => n.to_string(),
                        serde_json::Value::Bool(b) => b.to_string(),
                        _ => return None,
                    };
                    (!val.is_empty()).then_some((tag, val))
                })
                .collect();
            entries.sort_by(|(a, _), (b, _)| {
                a.order
                    .cmp(&b.order)
                    .then_with(|| b.score.cmp(&a.score))
                    .then_with(|| a.name.cmp(&b.name))
            });
            for (tag, val) in entries {
                let item = RichKanjiStat {
                    label: (if tag.notes.is_empty() {
                        tag.name.as_str()
                    } else {
                        tag.notes.as_str()
                    })
                    .into(),
                    value: val.into(),
                };
                match tag.category.as_str() {
                    "misc" => stats.push(item),
                    "class" => classes.push(item),
                    "code" => codes.push(item),
                    "index" => indices.push(item),
                    _ => {}
                }
            }
        }

        Some(RichKanjiDetail {
            valid: true,
            character: character.into(),
            meanings: Rc::new(VecModel::from(meanings)).into(),
            readings: Rc::new(VecModel::from(readings)).into(),
            stats: Rc::new(VecModel::from(stats)).into(),
            classifications: Rc::new(VecModel::from(classes)).into(),
            codepoints: Rc::new(VecModel::from(codes)).into(),
            indices: Rc::new(VecModel::from(indices)).into(),
        })
    }
}

// ── Rich UI Bridge State & Wiring (`install_rich_ui_bridges`) ───────────────

#[derive(Clone)]
struct PopupHistoryEntry {
    label: String,
    rows: Vec<DictResult>,
    hits: Vec<SearchHit>,
    loaded_count: i32,
    headword: String,
    reading: String,
    pitch_accent: String,
    freq_rank: String,
    jpdb_rank: String,
    kana_reading_active: bool,
    active_kana_reading: String,
}

#[derive(Default, Clone)]
struct RichRowExtras {
    hit: Option<SearchHit>,
    term_pills: Vec<RichTagPill>,
    def_pills: Vec<RichTagPill>,
    derivation_steps: Vec<RichDerivationStep>,
    pitch_graphs: Vec<RichPitchGraph>,
    kanji_detail: Option<RichKanjiDetail>,
    detail_states: HashMap<String, bool>,
}

#[derive(Clone)]
struct GlossarySelectionState {
    row_index: i32,
    anchor_char: usize,
    focus_char: usize,
    dragging: bool,
    press_pos: (f32, f32),
}

#[derive(Clone)]
struct HoveredSubtitleCluster {
    word: SubtitleWord,
    char_offset: usize,
    global_char_index: usize,
    global_utf16_offset: usize,
    sub_query: String,
}

struct CachedConfigSettings {
    subtitle_style: SubtitleRenderStyle,
    search_method: i32,
    search_modifier: i32,
    search_delay_ms: u64,
    middle_mouse_scan: bool,
    glossary_style: LegacyGlossaryStyle,
    dict_order: Vec<i64>,
    excluded_glossaries: Vec<String>,
    conf_mtime: Option<SystemTime>,
    anki_mtime: Option<SystemTime>,
}

impl CachedConfigSettings {
    fn from_controller(config: &ConfigController, config_dir: &Path) -> Self {
        let mut out = Self {
            subtitle_style: SubtitleRenderStyle::default(),
            search_method: raw::search::METHOD_DEFAULT as i32,
            search_modifier: raw::search::MODIFIER_DEFAULT as i32,
            search_delay_ms: raw::search::DELAY_DEFAULT as u64,
            middle_mouse_scan: raw::search::MIDDLE_MOUSE_SCAN_DEFAULT,
            glossary_style: LegacyGlossaryStyle::Bullet,
            dict_order: Vec::new(),
            excluded_glossaries: Vec::new(),
            conf_mtime: std::fs::metadata(config_dir.join("memento.conf"))
                .ok()
                .and_then(|m| m.modified().ok()),
            anki_mtime: std::fs::metadata(config_dir.join("anki_connect.json"))
                .ok()
                .and_then(|m| m.modified().ok()),
        };
        out.apply_controller(config);
        out
    }

    fn apply_controller(&mut self, config: &ConfigController) {
        use memento_config::Value;
        if let Some(Value::Text(font_csv)) =
            config.effective_setting(raw::interface::GROUP, raw::interface::subtitle::FONT)
        {
            let parts: Vec<&str> = font_csv.split(',').collect();
            if let Some(fam) = parts.first().map(|s| s.trim()).filter(|s| !s.is_empty()) {
                self.subtitle_style.font_family = fam.to_owned();
            }
            if let Some(w_str) = parts.get(4).map(|s| s.trim()) {
                if let Ok(w) = w_str.parse::<i32>() {
                    self.subtitle_style.bold = w >= 65 || w >= 600;
                }
            }
            if let Some(i_str) = parts.get(5).map(|s| s.trim()) {
                self.subtitle_style.italic = i_str == "1" || i_str.eq_ignore_ascii_case("true");
            }
        }
        if let Some(Value::Text(c)) =
            config.effective_setting(raw::interface::GROUP, raw::interface::subtitle::TEXT_COLOR)
        {
            self.subtitle_style.text_rgba = parse_def_rgba(&c, [255, 255, 255, 255]);
        }
        if let Some(Value::Text(c)) =
            config.effective_setting(raw::interface::GROUP, raw::interface::subtitle::STROKE_COLOR)
        {
            self.subtitle_style.stroke_rgba = parse_def_rgba(&c, [0, 0, 0, 255]);
        }
        if let Some(Value::Text(c)) = config.effective_setting(
            raw::interface::GROUP,
            raw::interface::subtitle::BACKGROUND_COLOR,
        ) {
            self.subtitle_style.bg_rgba = parse_def_rgba(&c, [0, 0, 0, 0]);
        }
        if let Some(v) =
            config.effective_setting(raw::interface::GROUP, raw::interface::subtitle::STROKE)
        {
            let num = match v {
                Value::Real(f) => Some(f as f32),
                Value::Int(i) => Some(i as f32),
                _ => None,
            };
            if let Some(n) = num {
                self.subtitle_style.stroke_size = n.max(0.0);
            }
        }
        if let Some(v) = config.effective_setting(
            raw::interface::GROUP,
            raw::interface::subtitle::FURIGANA_RATIO,
        ) {
            let num = match v {
                Value::Real(f) => Some(f as f32),
                Value::Int(i) => Some(i as f32),
                _ => None,
            };
            if let Some(n) = num {
                self.subtitle_style.furigana_ratio = n.clamp(0.2, 0.8);
            }
        }
        if let Some(Value::Int(m)) =
            config.effective_setting(raw::search::GROUP, raw::search::METHOD)
        {
            self.search_method = m as i32;
        }
        if let Some(Value::Int(m)) =
            config.effective_setting(raw::search::GROUP, raw::search::MODIFIER)
        {
            self.search_modifier = m as i32;
        }
        if let Some(Value::Int(d)) =
            config.effective_setting(raw::search::GROUP, raw::search::DELAY)
        {
            self.search_delay_ms = d.max(0) as u64;
        }
        if let Some(Value::Bool(b)) =
            config.effective_setting(raw::search::GROUP, raw::search::MIDDLE_MOUSE_SCAN)
        {
            self.middle_mouse_scan = b;
        }
        if let Some(Value::Int(gs)) =
            config.effective_setting(raw::search::GROUP, raw::search::GLOSSARY_STYLE)
        {
            self.glossary_style = match gs {
                0 => LegacyGlossaryStyle::Bullet,
                1 => LegacyGlossaryStyle::LineBreak,
                2 => LegacyGlossaryStyle::Pipe,
                _ => LegacyGlossaryStyle::NoSeparator,
            };
        }
        if let Some(Value::Ints(order)) =
            config.effective_setting(raw::dictionary::GROUP, raw::dictionary::ORDER)
        {
            self.dict_order = order;
        }
        self.excluded_glossaries = config.anki_excluded_glossaries();
    }

    fn reload_if_changed_on_disk(&mut self, config_dir: &Path) -> bool {
        let conf_mtime = std::fs::metadata(config_dir.join("memento.conf"))
            .ok()
            .and_then(|m| m.modified().ok());
        let anki_mtime = std::fs::metadata(config_dir.join("anki_connect.json"))
            .ok()
            .and_then(|m| m.modified().ok());
        if conf_mtime == self.conf_mtime && anki_mtime == self.anki_mtime {
            return false;
        }
        self.conf_mtime = conf_mtime;
        self.anki_mtime = anki_mtime;
        if let Ok(reloaded) = ConfigController::load(config_dir, "memento.conf") {
            let _ = reloaded.load_anki_profiles(config_dir.join("anki_connect.json"));
            self.apply_controller(&reloaded);
            return true;
        }
        false
    }
}

#[derive(Hash, PartialEq, Eq, Clone)]
struct GlossaryDocCacheKey {
    row_index: i32,
    width_px: i32,
    selected: bool,
    details_sig: String,
    rich_epoch: u64,
}

struct RichUiMutable {
    config_dir: PathBuf,
    dict_resource_dirs: Vec<PathBuf>,
    settings: CachedConfigSettings,
    dict_ctrl: DictController,
    db_sig: crate::controller::dict::DbFileFingerprint,
    db_helper: RichDbHelper,
    stylesheets: StylesheetCache,

    rich_epoch: u64,
    row_extras: HashMap<i32, RichRowExtras>,
    current_hits: Vec<SearchHit>,
    glossary_cache: HashMap<GlossaryDocCacheKey, RenderedGlossaryDoc>,
    last_rendered_key: HashMap<i32, GlossaryDocCacheKey>,
    selection: Option<GlossarySelectionState>,

    suppress_history_reset: bool,
    back_stack: Vec<PopupHistoryEntry>,
    forward_stack: Vec<PopupHistoryEntry>,

    hovered_cluster: Option<HoveredSubtitleCluster>,
    active_scan_utf16_range: Option<(usize, usize)>,
    pending_scan_utf16_start: Option<usize>,
    pending_scan_sub_query: Option<String>,
    scan_timer: slint::Timer,
}

/// Collect initial depth-first `<details>` open states (`"0"`, `"1"`, ...) from a
/// Yomitan `structured-content` JSON payload so toggling an initially `open: true` `<details>`
/// closes it on the first click.
fn collect_default_detail_states(raw_glossary: &str) -> HashMap<String, bool> {
    fn walk(node: &DefJson, next_id: &mut usize, out: &mut HashMap<String, bool>) {
        match node {
            DefJson::Array(items) => {
                for item in items {
                    walk(item, next_id, out);
                }
            }
            DefJson::Object(_) => {
                if node.get("tag").as_str().eq_ignore_ascii_case("details") {
                    let id = next_id.to_string();
                    *next_id += 1;
                    let is_open = node.get("open") == &DefJson::Bool(true);
                    out.insert(id, is_open);
                }
                walk(node.get("content"), next_id, out);
            }
            _ => {}
        }
    }

    let mut out = HashMap::new();
    if let Ok(parsed) = DefJson::parse(raw_glossary) {
        let mut next_id = 0usize;
        walk(&parsed, &mut next_id, &mut out);
    }
    out
}

struct RichUiBridge {
    instance_id: i32,
    app_weak: slint::Weak<MementoApp>,
    db_path: PathBuf,
    state: RefCell<RichUiMutable>,
    panel_tx: mpsc::Sender<(u64, String, bool, Vec<i64>, Vec<String>)>,
    panel_res_rx: Arc<std::sync::Mutex<mpsc::Receiver<(u64, Vec<SearchHit>, Vec<String>)>>>,
    panel_poll_timer: slint::Timer,
    panel_epoch: Arc<AtomicU64>,
}

impl RichUiBridge {
    fn ensure_fresh_db_and_config(&self, app: &MementoApp) {
        let mut reloaded_config = false;
        {
            let mut st = self.state.borrow_mut();
            let config_dir = st.config_dir.clone();
            if st.settings.reload_if_changed_on_disk(&config_dir) {
                reloaded_config = true;
                app.global::<SubtitleOverlayBridge>()
                    .set_middle_mouse_scan_enabled(st.settings.middle_mouse_scan);
                let rev = app.global::<SubtitleOverlayBridge>().get_style_revision();
                app.global::<SubtitleOverlayBridge>()
                    .set_style_revision(rev.wrapping_add(1));
            }
            let sig = crate::controller::dict::db_file_fingerprint(&self.db_path);
            if sig != st.db_sig {
                st.db_sig = sig;
                st.dict_ctrl = DictController::open(&self.db_path).unwrap_or_default();
            }
            let order = st.settings.dict_order.clone();
            st.dict_ctrl.set_order(&order);
            st.db_helper.refresh_if_needed();
        }
        if reloaded_config {
            let words: Vec<SubtitleWord> = app.get_active_sub_words().iter().collect();
            if !words.is_empty() {
                let lines = subtitle_lines_for_app(app, words);
                app.set_active_sub_lines(Rc::new(VecModel::from(lines)).into());
            }
        }
    }

    fn clear_subtitle_scan_state(&self, app: &MementoApp) {
        let mut st = self.state.borrow_mut();
        st.scan_timer.stop();
        st.hovered_cluster = None;
        st.active_scan_utf16_range = None;
        st.pending_scan_utf16_start = None;
        st.pending_scan_sub_query = None;
        drop(st);
        let bridge = app.global::<SubtitleOverlayBridge>();
        bridge.set_hovered_token_index(-1);
        bridge.set_hovered_char_offset(-1);
        bridge.set_hovered_global_char_index(-1);
        bridge.set_hovered_query("".into());
        bridge.set_highlight_revision(bridge.get_highlight_revision().wrapping_add(1));
    }

    fn on_dictionary_rows_updated(&self, app: &MementoApp, rows: &[DictResult]) {
        self.ensure_fresh_db_and_config(app);
        let candidate_hits = LAST_GROUPED_HITS.with(|slot| slot.borrow().clone());
        let matches_hits = candidate_hits.len() == rows.len()
            && candidate_hits.iter().zip(rows.iter()).all(|(h, r)| {
                h.expression == r.expression.as_str()
                    && h.reading == r.reading.as_str()
                    && h.dict_name == r.dict_name.as_str()
            });

        let mut st = self.state.borrow_mut();
        if !st.suppress_history_reset {
            st.back_stack.clear();
            st.forward_stack.clear();
        }
        st.rich_epoch = st.rich_epoch.wrapping_add(1);
        st.row_extras.clear();
        st.glossary_cache.clear();
        st.last_rendered_key.clear();
        st.selection = None;

        if matches_hits {
            st.current_hits = candidate_hits.clone();
            let mut group_start = 0usize;
            while group_start < rows.len() {
                let mut group_end = group_start + 1;
                while group_end < rows.len() && !rows[group_end].term_start {
                    group_end += 1;
                }
                let first_hit = &candidate_hits[group_start];
                let mut raw_term_tags = Vec::new();
                for hit in &candidate_hits[group_start..group_end] {
                    raw_term_tags
                        .extend(st.db_helper.resolve_tag_list(hit.dictionary_id, &hit.term_tags));
                }
                let pitch_details = if !first_hit.is_kanji {
                    st.dict_ctrl
                        .get_pitch_details(&first_hit.expression, &first_hit.reading)
                } else {
                    Vec::new()
                };
                let rich_pitches = rich_pitch_graphs_from_details(&pitch_details);
                let kanji_freqs: Vec<TermFrequency> = if first_hit.is_kanji {
                    st.dict_ctrl.get_kanji_frequencies(&first_hit.expression)
                } else {
                    Vec::new()
                };

                for idx in group_start..group_end {
                    let hit = &candidate_hits[idx];
                    let raw_def_tags =
                        st.db_helper.resolve_tag_list(hit.dictionary_id, &hit.def_tags);
                    let (dedup_term, dedup_def) =
                        suppress_redundant_tags(&raw_term_tags, &raw_def_tags, &hit.raw_glossary);

                    let term_pills = if idx == group_start {
                        dedup_term
                            .into_iter()
                            .map(|t| RichTagPill {
                                bg_color: tag_brush(&t.category),
                                name: t.name.into(),
                                category: t.category.into(),
                                notes: t.notes.into(),
                            })
                            .collect()
                    } else {
                        Vec::new()
                    };
                    let def_pills = dedup_def
                        .into_iter()
                        .map(|t| RichTagPill {
                            bg_color: tag_brush(&t.category),
                            name: t.name.into(),
                            category: t.category.into(),
                            notes: t.notes.into(),
                        })
                        .collect();

                    let derivation_steps = if idx == group_start {
                        let total = hit.derivation_descriptions.len();
                        hit.derivation_descriptions
                            .iter()
                            .enumerate()
                            .map(|(si, (label, desc))| RichDerivationStep {
                                label: label.clone().into(),
                                description: desc.clone().into(),
                                has_arrow: si + 1 < total,
                            })
                            .collect()
                    } else {
                        Vec::new()
                    };

                    let kanji_detail = if hit.is_kanji {
                        st.db_helper
                            .build_kanji_detail(&hit.expression, hit.dictionary_id)
                    } else {
                        None
                    };

                    st.row_extras.insert(
                        (idx + 1) as i32,
                        RichRowExtras {
                            hit: Some(hit.clone()),
                            term_pills,
                            def_pills,
                            derivation_steps,
                            pitch_graphs: if idx == group_start {
                                rich_pitches.clone()
                            } else {
                                Vec::new()
                            },
                            kanji_detail,
                            detail_states: collect_default_detail_states(&hit.raw_glossary),
                        },
                    );
                }

                if first_hit.is_kanji && !kanji_freqs.is_empty() {
                    let dict_rows = app.get_dict_results();
                    if let Some(mut first_row) = dict_rows.row_data(group_start) {
                        first_row.frequencies = Rc::new(VecModel::from(
                            kanji_freqs
                                .into_iter()
                                .map(|f| PopupFrequency {
                                    dictionary: f.dictionary.into(),
                                    value: f.value.into(),
                                })
                                .collect::<Vec<_>>(),
                        ))
                        .into();
                        dict_rows.set_row_data(group_start, first_row.clone());
                        let popup_rows = app.get_popup_results();
                        for pi in 0..popup_rows.row_count() {
                            if popup_rows
                                .row_data(pi)
                                .is_some_and(|r| r.index == first_row.index)
                            {
                                popup_rows.set_row_data(pi, first_row);
                                break;
                            }
                        }
                    }
                }

                group_start = group_end;
            }

            let pending_sub = st.pending_scan_sub_query.take();
            if let Some(utf16_start) = st.pending_scan_utf16_start.take() {
                let matched_u16 = if let (Some(first), Some(sub_q)) =
                    (candidate_hits.first(), pending_sub.as_deref())
                {
                    if !first.derivation.is_empty() {
                        let _ = st.dict_ctrl.search(sub_q);
                        let units = st.dict_ctrl.last_match_units();
                        if units > 0 {
                            units
                        } else {
                            first.expression.encode_utf16().count()
                        }
                    } else {
                        first.expression.encode_utf16().count()
                    }
                } else {
                    candidate_hits
                        .first()
                        .map(|h| h.expression.encode_utf16().count())
                        .unwrap_or(1)
                }
                .max(1);
                st.active_scan_utf16_range = Some((utf16_start, utf16_start + matched_u16));
            }
        } else {
            st.current_hits.clear();
            st.pending_scan_utf16_start = None;
            st.pending_scan_sub_query = None;
        }

        let can_back = !st.back_stack.is_empty();
        let can_fwd = !st.forward_stack.is_empty();
        let crumb = st
            .back_stack
            .last()
            .map(|entry| format!("From {}", entry.label))
            .unwrap_or_default();
        let reset_kana = !st.suppress_history_reset;
        drop(st);

        let pb = app.global::<PopupBridge>();
        pb.set_can_go_back(can_back);
        pb.set_can_go_forward(can_fwd);
        pb.set_nav_breadcrumb(crumb.into());
        if reset_kana {
            pb.set_kana_reading_active(false);
            pb.set_active_kana_reading("".into());
        }
        if reset_kana
            && app.get_dict_search_open()
            && app.get_selected_token_index() < 0
            && pb.get_panel_query().is_empty()
            && !app.get_dict_query().is_empty()
        {
            pb.set_panel_query(app.get_dict_query());
            pb.set_panel_results(Rc::new(VecModel::from(rows.to_vec())).into());
        }
        pb.set_rich_revision(pb.get_rich_revision().wrapping_add(1));
        pb.set_selection_revision(pb.get_selection_revision().wrapping_add(1));

        let sb = app.global::<SubtitleOverlayBridge>();
        sb.set_highlight_revision(sb.get_highlight_revision().wrapping_add(1));
    }

    fn snapshot_current_popup(&self, app: &MementoApp) -> Option<PopupHistoryEntry> {
        let full = app.get_dict_results();
        if full.row_count() == 0 {
            return None;
        }
        let rows: Vec<DictResult> = (0..full.row_count())
            .filter_map(|i| full.row_data(i))
            .collect();
        let st = self.state.borrow();
        let pb = app.global::<PopupBridge>();
        let label = if !app.get_dict_query().is_empty() {
            app.get_dict_query().to_string()
        } else {
            rows.first()
                .map(|r| r.expression.to_string())
                .unwrap_or_default()
        };
        Some(PopupHistoryEntry {
            label,
            rows,
            hits: st.current_hits.clone(),
            loaded_count: app.get_popup_loaded_count(),
            headword: app.get_dict_query().to_string(),
            reading: app.get_dict_reading().to_string(),
            pitch_accent: app.get_dict_pitch_accent().to_string(),
            freq_rank: app.get_dict_freq_rank().to_string(),
            jpdb_rank: app.get_dict_jpdb_rank().to_string(),
            kana_reading_active: pb.get_kana_reading_active(),
            active_kana_reading: pb.get_active_kana_reading().to_string(),
        })
    }

    fn restore_history_entry(&self, app: &MementoApp, entry: PopupHistoryEntry) {
        LAST_GROUPED_HITS.with(|slot| {
            *slot.borrow_mut() = entry.hits.clone();
        });
        {
            let mut st = self.state.borrow_mut();
            st.suppress_history_reset = true;
        }
        let pb = app.global::<PopupBridge>();
        pb.set_kana_reading_active(entry.kana_reading_active);
        pb.set_active_kana_reading(entry.active_kana_reading.into());

        app.set_anki_added_note_id("".into());
        app.set_anki_added_result_index(-1);
        set_dictionary_rows(app, entry.rows);
        let full = app.get_dict_results();
        let loaded = entry.loaded_count.max(1) as usize;
        app.set_popup_loaded_count(loaded.min(full.row_count()) as i32);
        app.set_popup_results(Rc::new(VecModel::from(popup_rows_through(&full, loaded))).into());
        app.set_dict_query(entry.headword.into());
        app.set_dict_reading(entry.reading.into());
        app.set_dict_pitch_accent(entry.pitch_accent.into());
        app.set_dict_freq_rank(entry.freq_rank.into());
        app.set_dict_jpdb_rank(entry.jpdb_rank.into());
        app.set_dict_open(full.row_count() > 0);
        app.invoke_reset_definition_scroll();

        self.state.borrow_mut().suppress_history_reset = false;
    }

    fn navigate_to_term_query(&self, app: &MementoApp, query: &str, kana_mode: Option<&str>) {
        let trimmed = query.trim();
        if trimmed.is_empty() {
            return;
        }
        self.ensure_fresh_db_and_config(app);
        let current_snap = self.snapshot_current_popup(app);
        let (mut hits, excluded, headword, reading, pitch, freq, jpdb, term_meta_rows) = {
            let mut st = self.state.borrow_mut();
            let raw_hits = st.dict_ctrl.search(trimmed).to_vec();
            let excluded = st.settings.excluded_glossaries.clone();
            let mut hits = if let Some(kana_target) = kana_mode {
                let norm = crate::controller::dict::normalize_reading(kana_target);
                let filtered: Vec<SearchHit> = raw_hits
                    .iter()
                    .filter(|h| {
                        crate::controller::dict::normalize_reading(&h.reading) == norm
                            || crate::controller::dict::normalize_reading(&h.expression) == norm
                    })
                    .cloned()
                    .collect();
                if filtered.is_empty() { raw_hits } else { filtered }
            } else {
                raw_hits
            };
            if hits.is_empty() && trimmed.chars().count() == 1 {
                hits = st.dict_ctrl.search_kanji(trimmed).to_vec();
            }
            let headword = hits
                .first()
                .map(|h| h.expression.clone())
                .unwrap_or_else(|| trimmed.to_owned());
            let reading = hits.first().map(|h| h.reading.clone()).unwrap_or_default();
            let (pitch, freq, jpdb) = if !headword.is_empty() {
                st.dict_ctrl.get_term_meta(&headword, &reading)
            } else {
                (String::new(), String::new(), String::new())
            };
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
            (
                hits,
                excluded,
                headword,
                reading,
                pitch,
                freq,
                jpdb,
                term_meta_rows,
            )
        };
        if hits.is_empty() {
            app.set_status_message(format!("No dictionary entries found for {trimmed}").into());
            return;
        }
        {
            let mut st = self.state.borrow_mut();
            if let Some(snap) = current_snap {
                st.back_stack.push(snap);
            }
            st.forward_stack.clear();
            st.suppress_history_reset = true;
        }
        let pb = app.global::<PopupBridge>();
        if let Some(kana_target) = kana_mode {
            pb.set_kana_reading_active(true);
            pb.set_active_kana_reading(kana_target.into());
        } else {
            pb.set_kana_reading_active(false);
            pb.set_active_kana_reading("".into());
        }

        let mut rows = dictionary_result_rows(&hits, &excluded);
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
        let _ = &mut hits;
        app.set_anki_added_note_id("".into());
        app.set_anki_added_result_index(-1);
        set_dictionary_rows(app, rows);
        app.set_dict_query(headword.into());
        app.set_dict_reading(reading.into());
        app.set_dict_pitch_accent(pitch.into());
        app.set_dict_freq_rank(freq.into());
        app.set_dict_jpdb_rank(jpdb.into());
        app.set_dict_open(true);
        app.invoke_reset_definition_scroll();
        self.state.borrow_mut().suppress_history_reset = false;
    }

    fn navigate_to_kanji(&self, app: &MementoApp, expression: &str, clicked_kanji: &str) {
        let target_char = clicked_kanji
            .chars()
            .find(|&c| is_kanji_char(c))
            .or_else(|| expression.chars().find(|&c| is_kanji_char(c)));
        let Some(ch) = target_char else {
            app.set_status_message("No kanji in selected word".into());
            return;
        };
        let query = ch.to_string();
        self.ensure_fresh_db_and_config(app);
        let current_snap = self.snapshot_current_popup(app);
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
        {
            let mut st = self.state.borrow_mut();
            if let Some(snap) = current_snap {
                st.back_stack.push(snap);
            }
            st.forward_stack.clear();
            st.suppress_history_reset = true;
        }
        let pb = app.global::<PopupBridge>();
        pb.set_kana_reading_active(false);
        pb.set_active_kana_reading("".into());

        let rows = dictionary_result_rows(&hits, &excluded);
        app.set_anki_added_note_id("".into());
        app.set_anki_added_result_index(-1);
        set_dictionary_rows(app, rows);
        app.set_dict_query(query.into());
        app.set_dict_reading(
            hits.first()
                .map(|h| h.reading.clone())
                .unwrap_or_default()
                .into(),
        );
        app.set_dict_pitch_accent("".into());
        app.set_dict_freq_rank("".into());
        app.set_dict_jpdb_rank("".into());
        app.set_dict_open(true);
        app.invoke_reset_definition_scroll();
        self.state.borrow_mut().suppress_history_reset = false;
    }

    fn render_or_get_glossary(&self, row_index: i32, width_px: f32, selected: bool) -> Option<RenderedGlossaryDoc> {
        let bucket_w = ((width_px.max(120.0) / 4.0).round() as i32 * 4).max(120);
        let mut st = self.state.borrow_mut();
        let extras = st.row_extras.get(&row_index)?.clone();
        let hit = extras.hit.as_ref()?;
        if hit.is_kanji {
            return None;
        }
        let mut pairs: Vec<_> = extras.detail_states.iter().collect();
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
            rich_epoch: st.rich_epoch,
        };
        if let Some(doc) = st.glossary_cache.get(&cache_key).cloned() {
            st.last_rendered_key.insert(row_index, cache_key);
            return Some(doc);
        }
        if st.glossary_cache.len() > 128 {
            st.glossary_cache.clear();
            st.last_rendered_key.clear();
        }
        let res_dirs = st.dict_resource_dirs.clone();
        let sheet = st
            .stylesheets
            .get_for_dictionary(&res_dirs, hit.dictionary_id, &hit.dict_name);
        let legacy_style = st.settings.glossary_style;
        let doc = render_structured_glossary_entry_in_dirs(
            &hit.raw_glossary,
            &hit.glossary,
            sheet,
            &extras.detail_states,
            legacy_style,
            &res_dirs,
            hit.dictionary_id,
            &hit.dict_name,
            bucket_w as f32,
            selected,
        )?;
        st.last_rendered_key.insert(row_index, cache_key.clone());
        st.glossary_cache.insert(cache_key, doc.clone());
        Some(doc)
    }

    fn latest_cached_doc(&self, row_index: i32) -> Option<RenderedGlossaryDoc> {
        let st = self.state.borrow();
        let epoch = st.rich_epoch;
        if let Some(key) = st.last_rendered_key.get(&row_index) {
            if key.rich_epoch == epoch {
                if let Some(doc) = st.glossary_cache.get(key) {
                    return Some(doc.clone());
                }
            }
        }
        st.glossary_cache
            .iter()
            .find(|(k, _)| k.row_index == row_index && k.rich_epoch == epoch)
            .map(|(_, doc)| doc.clone())
    }

    fn char_index_at_point(doc: &RenderedGlossaryDoc, x: f32, y: f32) -> Option<usize> {
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

    fn update_glossary_selection_text(&self, app: &MementoApp) {
        let sel_opt = self.state.borrow().selection.clone();
        let text = match sel_opt {
            Some(sel) if sel.anchor_char != sel.focus_char => {
                if let Some(doc) = self.latest_cached_doc(sel.row_index) {
                    let lo = sel.anchor_char.min(sel.focus_char).min(doc.selectable_chars.len());
                    let hi = sel.anchor_char.max(sel.focus_char).min(doc.selectable_chars.len());
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

    fn handle_internal_link_str(&self, app: &MementoApp, link: &str) -> bool {
        if let Some(rest) = link.strip_prefix("memento://glossary-details?") {
            let mut id = String::new();
            for (k, v) in url::form_urlencoded::parse(rest.as_bytes()) {
                if k == "id" {
                    id = v.into_owned();
                }
            }
            if !id.is_empty() {
                app.global::<PopupBridge>()
                    .invoke_toggle_glossary_details(1, id.into());
            }
            return true;
        }
        if let Some(rest) = link.strip_prefix("memento://glossary-link?") {
            let mut target = String::new();
            for (k, v) in url::form_urlencoded::parse(rest.as_bytes()) {
                if k == "target" {
                    target = v.into_owned();
                }
            }
            if !target.is_empty() {
                return self.handle_internal_link_str(app, &target);
            }
            return true;
        }
        if let Some(query_str) = link.strip_prefix('?') {
            if let Some((_, q)) =
                url::form_urlencoded::parse(query_str.as_bytes()).find(|(k, _)| k == "query")
            {
                let q_owned = q.into_owned();
                if !q_owned.is_empty() {
                    self.navigate_to_term_query(app, &q_owned, None);
                    return true;
                }
            }
        }
        false
    }

    fn resolve_subtitle_cluster(
        &self,
        app: &MementoApp,
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
        let mut prefix_chars = 0usize;
        let mut prefix_utf16 = 0usize;
        let token_idx = word.index.max(0) as usize;
        for i in 0..token_idx.min(words.row_count()) {
            if let Some(w) = words.row_data(i) {
                prefix_chars += w.text.chars().count();
                prefix_utf16 += w.text.encode_utf16().count();
            }
        }
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

    fn trigger_subtitle_scan_now(&self, app: &MementoApp, cluster: HoveredSubtitleCluster) {
        {
            let mut st = self.state.borrow_mut();
            st.scan_timer.stop();
            st.pending_scan_utf16_start = Some(cluster.global_utf16_offset);
            st.pending_scan_sub_query = Some(cluster.sub_query.clone());
        }
        app.set_selected_token_index(cluster.word.index);
        app.invoke_lookup_word(cluster.sub_query.into());
    }
}

fn is_modifier_only_key_text(text: &str) -> Option<(bool, bool, bool, bool)> {
    use slint::platform::Key;
    let mut chars = text.chars();
    let ch = chars.next()?;
    if chars.next().is_some() {
        return None;
    }
    if ch == char::from(Key::Shift) || ch == char::from(Key::ShiftR) {
        Some((false, true, false, false))
    } else if ch == char::from(Key::Control) || ch == char::from(Key::ControlR) {
        Some((true, false, false, false))
    } else if ch == char::from(Key::Alt) || ch == char::from(Key::AltGr) {
        Some((false, false, true, false))
    } else if ch == char::from(Key::Meta) || ch == char::from(Key::MetaR) {
        Some((false, false, false, true))
    } else {
        None
    }
}

fn modifier_matches_setting(
    setting: i32,
    control: bool,
    shift: bool,
    alt: bool,
    meta: bool,
) -> bool {
    match setting {
        0 => alt,
        1 => control,
        2 => shift,
        3 => meta,
        _ => false,
    }
}

fn drain_panel_results(
    app: &MementoApp,
    rx: &std::sync::Mutex<mpsc::Receiver<(u64, Vec<SearchHit>, Vec<String>)>>,
    epoch: &AtomicU64,
) -> bool {
    let mut latest: Option<(u64, Vec<SearchHit>, Vec<String>)> = None;
    if let Ok(guard) = rx.lock() {
        while let Ok(item) = guard.try_recv() {
            latest = Some(item);
        }
    }
    let Some((gen_id, hits, excluded)) = latest else {
        return false;
    };
    if gen_id != epoch.load(Ordering::Acquire) {
        return false;
    }
    // Build DictResult rows WITHOUT touching LAST_GROUPED_HITS or app.dict_results!
    let saved = LAST_GROUPED_HITS.with(|s| s.borrow().clone());
    let rows = dictionary_result_rows(&hits, &excluded);
    LAST_GROUPED_HITS.with(|s| *s.borrow_mut() = saved);
    let pb = app.global::<PopupBridge>();
    pb.set_panel_results(Rc::new(VecModel::from(rows)).into());
    pb.set_panel_searching(false);
    true
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
    let dict_resource_dirs = collect_dictionary_resource_dirs(config_dir, dictionary_db_path);
    let db_sig = crate::controller::dict::db_file_fingerprint(dictionary_db_path);
    let mut dict_ctrl = DictController::open(dictionary_db_path).unwrap_or_default();
    dict_ctrl.set_order(&settings.dict_order);

    // Independent async worker thread for DictionarySearchPanel so panel searches NEVER
    // block the UI thread and NEVER overwrite or get overwritten by subtitle hover popups!
    let (panel_tx, panel_rx) =
        mpsc::channel::<(u64, String, bool, Vec<i64>, Vec<String>)>();
    let (panel_res_tx, raw_panel_res_rx) =
        mpsc::channel::<(u64, Vec<SearchHit>, Vec<String>)>();
    let panel_res_rx = Arc::new(std::sync::Mutex::new(raw_panel_res_rx));
    let panel_epoch = Arc::new(AtomicU64::new(0));
    let worker_epoch = panel_epoch.clone();
    let worker_res_rx = panel_res_rx.clone();
    let worker_db_path = dictionary_db_path.to_path_buf();
    let worker_app = app.as_weak();
    std::thread::spawn(move || {
        let mut sig = crate::controller::dict::db_file_fingerprint(&worker_db_path);
        let mut dict = DictController::open(&worker_db_path).unwrap_or_default();
        while let Ok(mut req) = panel_rx.recv() {
            while let Ok(newer) = panel_rx.try_recv() {
                req = newer;
            }
            let (gen_id, query, kana_only, order, excluded) = req;
            if gen_id != worker_epoch.load(Ordering::Acquire) {
                continue;
            }
            let trimmed = query.trim().to_owned();
            if trimmed.is_empty() {
                let _ = panel_res_tx.send((gen_id, Vec::new(), excluded));
                let rx_cl = worker_res_rx.clone();
                let ep_cl = worker_epoch.clone();
                let _ = worker_app.upgrade_in_event_loop(move |app| {
                    let _ = drain_panel_results(&app, &rx_cl, &ep_cl);
                });
                continue;
            }
            let cur_sig = crate::controller::dict::db_file_fingerprint(&worker_db_path);
            if cur_sig != sig {
                sig = cur_sig;
                dict = DictController::open(&worker_db_path).unwrap_or_default();
            }
            dict.set_order(&order);
            let search_key = if kana_only {
                crate::controller::dict::normalize_reading(&trimmed)
            } else {
                trimmed.clone()
            };
            let mut hits = dict.search(&search_key).to_vec();
            if kana_only {
                let norm = crate::controller::dict::normalize_reading(&trimmed);
                hits.retain(|h| {
                    crate::controller::dict::normalize_reading(&h.reading).starts_with(&norm)
                        || crate::controller::dict::normalize_reading(&h.expression)
                            .starts_with(&norm)
                });
            }
            if gen_id != worker_epoch.load(Ordering::Acquire) {
                continue;
            }
            let _ = panel_res_tx.send((gen_id, hits, excluded));
            let rx_cl = worker_res_rx.clone();
            let ep_cl = worker_epoch.clone();
            let _ = worker_app.upgrade_in_event_loop(move |app| {
                let _ = drain_panel_results(&app, &rx_cl, &ep_cl);
            });
        }
    });

    let instance_id = NEXT_BRIDGE_INSTANCE_ID.fetch_add(1, Ordering::Relaxed);
    let prev_instance_id = app.global::<PopupBridge>().get_bridge_instance_id();
    app.global::<PopupBridge>().set_bridge_instance_id(instance_id);

    let bridge = Rc::new(RichUiBridge {
        instance_id,
        app_weak: app.as_weak(),
        db_path: dictionary_db_path.to_path_buf(),
        state: RefCell::new(RichUiMutable {
            config_dir: config_dir.to_path_buf(),
            dict_resource_dirs,
            settings,
            dict_ctrl,
            db_sig,
            db_helper: RichDbHelper::new(dictionary_db_path.to_path_buf()),
            stylesheets: StylesheetCache::default(),
            rich_epoch: 1,
            row_extras: HashMap::new(),
            current_hits: Vec::new(),
            glossary_cache: HashMap::new(),
            last_rendered_key: HashMap::new(),
            selection: None,
            suppress_history_reset: false,
            back_stack: Vec::new(),
            forward_stack: Vec::new(),
            hovered_cluster: None,
            active_scan_utf16_range: None,
            pending_scan_utf16_start: None,
            pending_scan_sub_query: None,
            scan_timer: slint::Timer::default(),
        }),
        panel_tx,
        panel_res_rx,
        panel_poll_timer: slint::Timer::default(),
        panel_epoch,
    });

    ACTIVE_RICH_BRIDGES.with(|slot| {
        let mut list = slot.borrow_mut();
        list.retain(|b| b.app_weak.upgrade().is_some() && b.instance_id != prev_instance_id);
        list.push(bridge.clone());
    });

    // ── Wire PopupBridge ────────────────────────────────────────────────────
    let pb = app.global::<PopupBridge>();
    pb.set_rich_enabled(true);

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

    pb.on_headword_lines(|expr, avail_w| {
        let max_w = avail_w.max(96.0);
        let mut lines: Vec<RichHeadwordLine> = Vec::new();
        let mut cur_line: Vec<RichHeadwordChar> = Vec::new();
        let mut cur_w = 0.0f32;
        for (i, ch) in expr.chars().enumerate() {
            let cw = if ch.is_ascii() { 14.0 } else { 24.0 };
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

    pb.on_devoice_circle_image(|| {
        SHARED_TEXT_ENGINE.with(|e| e.borrow_mut().devoice_circle_image())
    });

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
        let Some(doc) = b_render_gloss.render_or_get_glossary(row_idx, avail_w, selected) else {
            return RichGlossaryRender::default();
        };
        let mut sel_rects = Vec::new();
        {
            let st = b_render_gloss.state.borrow();
            if let Some(ref sel) = st.selection {
                if sel.row_index == row_idx && sel.anchor_char != sel.focus_char {
                    let lo = sel.anchor_char.min(sel.focus_char).min(doc.selectable_chars.len());
                    let hi = sel.anchor_char.max(sel.focus_char).min(doc.selectable_chars.len());
                    let mut cur: Option<(usize, f32, f32, f32, f32)> = None;
                    for cell in &doc.selectable_chars[lo..hi] {
                        if cell.ch == '\n' || cell.w <= 0.0 {
                            continue;
                        }
                        match cur {
                            Some((lid, sx, sy, sw, sh)) if lid == cell.line_id => {
                                let nx = sx.min(cell.x);
                                let nw = (sx + sw).max(cell.x + cell.w) - nx;
                                cur = Some((lid, nx, sy.min(cell.y), nw, sh.max(cell.h)));
                            }
                            Some((_, sx, sy, sw, sh)) => {
                                sel_rects.push(RichSelectionRect {
                                    x_px: sx,
                                    y_px: sy,
                                    w_px: sw,
                                    h_px: sh,
                                });
                                cur = Some((cell.line_id, cell.x, cell.y, cell.w, cell.h));
                            }
                            None => {
                                cur = Some((cell.line_id, cell.x, cell.y, cell.w, cell.h));
                            }
                        }
                    }
                    if let Some((_, sx, sy, sw, sh)) = cur {
                        sel_rects.push(RichSelectionRect {
                            x_px: sx,
                            y_px: sy,
                            w_px: sw,
                            h_px: sh,
                        });
                    }
                }
            }
        }
        RichGlossaryRender {
            has_rich: true,
            width_px: doc.width_px,
            height_px: doc.height_px,
            rendered: doc.image,
            selection_rects: Rc::new(VecModel::from(sel_rects)).into(),
        }
    });

    let b_cursor = bridge.clone();
    pb.on_glossary_cursor_is_pointer(move |row_idx, x, y| {
        let Some(doc) = b_cursor.latest_cached_doc(row_idx) else {
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
            let Some(prev) = st.back_stack.pop() else {
                return;
            };
            if let Some(cur) = current {
                st.forward_stack.push(cur);
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
            let Some(next) = st.forward_stack.pop() else {
                return;
            };
            if let Some(cur) = current {
                st.back_stack.push(cur);
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
        if pb.get_kana_reading_active() && pb.get_active_kana_reading().as_str() == reading.as_str()
        {
            if pb.get_can_go_back() {
                pb.invoke_nav_back();
                return;
            }
        }
        b_kana_toggle.navigate_to_term_query(&app, reading.as_str(), Some(reading.as_str()));
    });

    let b_act_link = bridge.clone();
    pb.on_activate_glossary_link(move |href| {
        let Some(app) = b_act_link.app_weak.upgrade() else {
            return false;
        };
        b_act_link.handle_internal_link_str(&app, href.as_str())
    });

    let b_toggle_details = bridge.clone();
    pb.on_toggle_glossary_details(move |row_idx, detail_id| {
        let Some(app) = b_toggle_details.app_weak.upgrade() else {
            return;
        };
        {
            let mut st = b_toggle_details.state.borrow_mut();
            if let Some(extras) = st.row_extras.get_mut(&row_idx) {
                let current = extras
                    .detail_states
                    .get(detail_id.as_str())
                    .copied()
                    .unwrap_or(false);
                extras
                    .detail_states
                    .insert(detail_id.to_string(), !current);
            }
            st.glossary_cache.retain(|k, _| k.row_index != row_idx);
            st.last_rendered_key.remove(&row_idx);
        }
        let pb = app.global::<PopupBridge>();
        pb.set_tooltip_visible(false);
        pb.set_rich_revision(pb.get_rich_revision().wrapping_add(1));
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
        let Some(app) = b_g_down.app_weak.upgrade() else {
            return;
        };
        let Some(doc) = b_g_down.latest_cached_doc(row_idx) else {
            return;
        };
        if let Some(ci) = RichUiBridge::char_index_at_point(&doc, x, y) {
            b_g_down.state.borrow_mut().selection = Some(GlossarySelectionState {
                row_index: row_idx,
                anchor_char: ci,
                focus_char: ci,
                dragging: false,
                press_pos: (x, y),
            });
            b_g_down.update_glossary_selection_text(&app);
        }
    });

    let b_g_drag = bridge.clone();
    pb.on_glossary_pointer_drag(move |row_idx, x, y| {
        let Some(app) = b_g_drag.app_weak.upgrade() else {
            return;
        };
        let Some(doc) = b_g_drag.latest_cached_doc(row_idx) else {
            return;
        };
        let mut updated = false;
        {
            let mut st = b_g_drag.state.borrow_mut();
            if let Some(ref mut sel) = st.selection {
                if sel.row_index == row_idx {
                    let dist = (x - sel.press_pos.0).hypot(y - sel.press_pos.1);
                    if dist >= 3.0 {
                        sel.dragging = true;
                    }
                    if sel.dragging {
                        if let Some(ci) = RichUiBridge::char_index_at_point(&doc, x, y) {
                            let target = if x > doc.selectable_chars[ci].x + doc.selectable_chars[ci].w * 0.5 {
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
            b_g_drag.update_glossary_selection_text(&app);
        }
    });

    let b_g_up = bridge.clone();
    pb.on_glossary_pointer_up(move |row_idx, x, y| {
        let Some(app) = b_g_up.app_weak.upgrade() else {
            return;
        };
        let was_dragging = b_g_up
            .state
            .borrow()
            .selection
            .as_ref()
            .is_some_and(|s| s.dragging && s.anchor_char != s.focus_char);
        if was_dragging {
            return;
        }
        if let Some(doc) = b_g_up.latest_cached_doc(row_idx) {
            if let Some(zone) = doc
                .hit_zones
                .iter()
                .rev()
                .find(|z| z.action.is_some() && x >= z.x && x <= z.x + z.w && y >= z.y && y <= z.y + z.h)
            {
                match zone.action.as_ref() {
                    Some(GlossaryZoneAction::ToggleDetails(did)) => {
                        app.global::<PopupBridge>()
                            .invoke_toggle_glossary_details(row_idx, did.clone().into());
                    }
                    Some(GlossaryZoneAction::Link(href)) => {
                        if !b_g_up.handle_internal_link_str(&app, href) {
                            app.invoke_open_glossary_link(href.clone().into());
                        }
                    }
                    None => {}
                }
            }
        }
    });

    let b_g_hover = bridge.clone();
    pb.on_glossary_pointer_hover(move |row_idx, x, y, popup_x, popup_y| {
        let Some(app) = b_g_hover.app_weak.upgrade() else {
            return;
        };
        let pb = app.global::<PopupBridge>();
        if let Some(doc) = b_g_hover.latest_cached_doc(row_idx) {
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
        let Some(app) = b_g_dbl.app_weak.upgrade() else {
            return;
        };
        let Some(doc) = b_g_dbl.latest_cached_doc(row_idx) else {
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
        b_g_dbl.state.borrow_mut().selection = Some(GlossarySelectionState {
            row_index: row_idx,
            anchor_char: lo,
            focus_char: hi,
            dragging: false,
            press_pos: (x, y),
        });
        b_g_dbl.update_glossary_selection_text(&app);
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
        use copypasta::ClipboardProvider;
        if let Ok(mut ctx) = copypasta::ClipboardContext::new() {
            let _ = ctx.set_contents(text);
        }
    });

    let b_pkey = bridge.clone();
    pb.on_popup_key_pressed(move |text, control, shift, alt, meta| {
        if let Some((mc, ms, ma, mm)) = is_modifier_only_key_text(text.as_str()) {
            if let Some(app) = b_pkey.app_weak.upgrade() {
                app.global::<SubtitleOverlayBridge>()
                    .invoke_stationary_modifier_changed(
                        control || mc,
                        shift || ms,
                        alt || ma,
                        meta || mm,
                    );
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
        b_panel_search.ensure_fresh_db_and_config(&app);
        let pb = app.global::<PopupBridge>();
        pb.set_panel_query(query.clone());
        let kana_only = pb.get_panel_kana_only();
        let (order, excluded) = {
            let st = b_panel_search.state.borrow();
            (
                st.settings.dict_order.clone(),
                st.settings.excluded_glossaries.clone(),
            )
        };
        let gen_id = b_panel_search.panel_epoch.fetch_add(1, Ordering::AcqRel) + 1;
        if query.trim().is_empty() {
            b_panel_search.panel_poll_timer.stop();
            pb.set_panel_results(Rc::new(VecModel::from(Vec::<DictResult>::new())).into());
            pb.set_panel_searching(false);
            return;
        }
        pb.set_panel_searching(true);
        let _ = b_panel_search.panel_tx.send((
            gen_id,
            query.to_string(),
            kana_only,
            order,
            excluded,
        ));
        let weak_br = Rc::downgrade(&b_panel_search);
        b_panel_search.panel_poll_timer.start(
            slint::TimerMode::Repeated,
            std::time::Duration::from_millis(8),
            move || {
                let Some(br) = weak_br.upgrade() else {
                    return;
                };
                let Some(app) = br.app_weak.upgrade() else {
                    br.panel_poll_timer.stop();
                    return;
                };
                let _ = drain_panel_results(&app, &br.panel_res_rx, &br.panel_epoch);
                if !app.global::<PopupBridge>().get_panel_searching() {
                    br.panel_poll_timer.stop();
                }
            },
        );
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

    // ── Wire SubtitleOverlayBridge ──────────────────────────────────────────
    let sb = app.global::<SubtitleOverlayBridge>();
    sb.set_rich_enabled(true);
    sb.set_middle_mouse_scan_enabled(middle_mouse_default);

    let b_sub_render = bridge.clone();
    sb.on_render_token(
        move |word, font_px, show_furigana, token_selected, _style_rev, _hl_rev| {
            if word.text.is_empty() {
                return SubtitleTokenMetrics::default();
            }
            let (style, scan_range, app_opt) = {
                let st = b_sub_render.state.borrow();
                (
                    st.settings.subtitle_style.clone(),
                    st.active_scan_utf16_range,
                    b_sub_render.app_weak.upgrade(),
                )
            };
            let shaped = SHARED_TEXT_ENGINE.with(|e| {
                e.borrow_mut()
                    .shape_and_render_subtitle_token(&word, font_px, show_furigana, &style)
            });

            let mut has_sel = false;
            let mut sel_x = shaped.glyph_x_px;
            let mut sel_w = shaped.glyph_width_px;

            if let (Some((hl_start, hl_end)), Some(ref app)) = (scan_range, app_opt) {
                if app.get_dict_open() && app.get_selected_token_index() >= 0 {
                    let words = app.get_active_sub_words();
                    let mut token_u16_start = 0usize;
                    let tidx = word.index.max(0) as usize;
                    for i in 0..tidx.min(words.row_count()) {
                        if let Some(w) = words.row_data(i) {
                            token_u16_start += w.text.encode_utf16().count();
                        }
                    }
                    let token_u16_len = word.text.encode_utf16().count();
                    let token_u16_end = token_u16_start + token_u16_len;
                    let ov_start = hl_start.max(token_u16_start);
                    let ov_end = hl_end.min(token_u16_end);
                    if ov_start < ov_end {
                        has_sel = true;
                        let local_u16_lo = ov_start - token_u16_start;
                        let local_u16_hi = ov_end - token_u16_start;
                        let mut x_min = f32::INFINITY;
                        let mut x_max = 0.0f32;
                        let mut cur_u16 = 0usize;
                        let mut char_byte_ranges = Vec::new();
                        for (bi, ch) in word.text.char_indices() {
                            let next_u16 = cur_u16 + ch.len_utf16();
                            char_byte_ranges.push((bi, bi + ch.len_utf8(), cur_u16, next_u16));
                            cur_u16 = next_u16;
                        }
                        for &(c_b0, c_b1, _, _) in &shaped.clusters {
                            let cl_u16_lo = char_byte_ranges
                                .iter()
                                .find(|&&(b0, _, _, _)| b0 >= c_b0)
                                .map_or(0, |r| r.2);
                            let cl_u16_hi = char_byte_ranges
                                .iter()
                                .rev()
                                .find(|&&(_, b1, _, _)| b1 <= c_b1)
                                .map_or(token_u16_len, |r| r.3);
                            if cl_u16_lo < local_u16_hi && cl_u16_hi > local_u16_lo {
                                if let Some(&(_, _, cx, cadv)) = shaped
                                    .clusters
                                    .iter()
                                    .find(|&&(b0, b1, _, _)| b0 == c_b0 && b1 == c_b1)
                                {
                                    x_min = x_min.min(cx);
                                    x_max = x_max.max(cx + cadv);
                                }
                            }
                        }
                        if x_min.is_finite() && x_max > x_min {
                            sel_x = shaped.glyph_x_px + x_min;
                            sel_w = x_max - x_min;
                        }
                    }
                } else if token_selected {
                    has_sel = true;
                }
            } else if token_selected {
                has_sel = true;
            }

            SubtitleTokenMetrics {
                width_px: shaped.width_px,
                height_px: shaped.height_px,
                glyph_y_px: shaped.glyph_y_px,
                glyph_height_px: shaped.glyph_height_px,
                sel_x_px: sel_x,
                sel_width_px: sel_w,
                has_selection: has_sel,
                rendered: shaped.image,
            }
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
            && !has_glossary_selection
    });

    let b_ptr_hover = bridge.clone();
    sb.on_pointer_hovered(
        move |word, font_px, show_furigana, local_x, local_y, control, shift, alt, meta| {
            let Some(app) = b_ptr_hover.app_weak.upgrade() else {
                return;
            };
            app.invoke_osc_activity();
            b_ptr_hover.ensure_fresh_db_and_config(&app);

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
            app.set_selected_token_index(word.index);
            app.invoke_lookup_word(word.query);
        }
    });

    let b_mid_click = bridge.clone();
    sb.on_token_middle_clicked(move |word, font_px, show_furigana, local_x, local_y| {
        let Some(app) = b_mid_click.app_weak.upgrade() else {
            return;
        };
        b_mid_click.ensure_fresh_db_and_config(&app);
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
        b_stat_mod.ensure_fresh_db_and_config(&app);
        let (mod_setting, cluster_opt) = {
            let st = b_stat_mod.state.borrow();
            (st.settings.search_modifier, st.hovered_cluster.clone())
        };
        if modifier_matches_setting(mod_setting, control, shift, alt, meta) {
            if let Some(cluster) = cluster_opt {
                b_stat_mod.trigger_subtitle_scan_now(&app, cluster);
            }
        }
    });

    let b_hkey = bridge.clone();
    sb.on_hover_key_pressed(move |text, control, shift, alt, meta| {
        let Some(app) = b_hkey.app_weak.upgrade() else {
            return false;
        };
        if let Some((mc, ms, ma, mm)) = is_modifier_only_key_text(text.as_str()) {
            app.global::<SubtitleOverlayBridge>()
                .invoke_stationary_modifier_changed(
                    control || mc,
                    shift || ms,
                    alt || ma,
                    meta || mm,
                );
            return true;
        }
        false
    });
}

/// Immediately propagate updated settings from `ConfigController` into the active rich UI bridge.
pub fn refresh_rich_ui_bridges(app: &MementoApp, config: &ConfigController) {
    let instance_id = app.global::<PopupBridge>().get_bridge_instance_id();
    let bridge = ACTIVE_RICH_BRIDGES.with(|slot| {
        slot.borrow()
            .iter()
            .find(|b| b.instance_id == instance_id)
            .cloned()
    });
    let Some(bridge) = bridge else {
        return;
    };
    let middle_mouse = {
        let mut st = bridge.state.borrow_mut();
        st.settings.apply_controller(config);
        let order = st.settings.dict_order.clone();
        st.dict_ctrl.set_order(&order);
        st.settings.middle_mouse_scan
    };
    let sb = app.global::<SubtitleOverlayBridge>();
    sb.set_middle_mouse_scan_enabled(middle_mouse);
    sb.set_style_revision(sb.get_style_revision().wrapping_add(1));
    let words: Vec<SubtitleWord> = app.get_active_sub_words().iter().collect();
    if !words.is_empty() {
        let lines = subtitle_lines_for_app(app, words);
        app.set_active_sub_lines(Rc::new(VecModel::from(lines)).into());
    }
}

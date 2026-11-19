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
    static SUPPRESS_FIRST_TERM_READY: Cell<bool> = const { Cell::new(false) };
}

pub fn is_kanji_char(character: char) -> bool {
    ('\u{3400}'..='\u{9fff}').contains(&character)
        || ('\u{f900}'..='\u{faff}').contains(&character)
        || ('\u{20000}'..='\u{3134f}').contains(&character)
}

fn build_dictionary_result_rows_grouped(
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
    LAST_GROUPED_HITS.with(|slot| {
        *slot.borrow_mut() = grouped_owned;
    });
    rows
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
    let total = full.row_count();
    app.set_popup_loaded_count(total as i32);
    app.set_popup_results(
        Rc::new(VecModel::from(popup_rows_through(&full, total))).into(),
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

    let app_bridge_id = pb.get_bridge_instance_id();
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

pub fn active_subtitle_style_for_app(app: &MementoApp) -> SubtitleRenderStyle {
    subtitle_style_for_app(app)
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
    pub line_spacing: f32,
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
            line_spacing: raw::interface::subtitle::LINE_SPACING_DEFAULT as f32,
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
struct RenderedColorGlyph {
    pixmap: Arc<tiny_skia::Pixmap>,
    left: f32,
    top: f32,
}

#[derive(Clone)]
pub struct ShapedSubtitleToken {
    pub width_px: f32,
    pub height_px: f32,
    pub bleed_x_px: f32,
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
    line_spacing_tenth: i32,
    bold: bool,
    italic: bool,
}

#[derive(Clone)]
struct CachedAlphaGlyph {
    width: u32,
    height: u32,
    left: i32,
    top: i32,
    mask: Arc<[u8]>,
}

struct PositionedMaskGlyph {
    x: i32,
    y: i32,
    glyph: CachedAlphaGlyph,
    rgba: [u8; 4],
}

fn blit_alpha_glyph(
    pixmap: &mut tiny_skia::Pixmap,
    x0: i32,
    y0: i32,
    glyph: &CachedAlphaGlyph,
    rgba: [u8; 4],
) {
    let pw = pixmap.width() as i32;
    let ph = pixmap.height() as i32;
    let gw = glyph.width as i32;
    let gh = glyph.height as i32;
    let [sr, sg, sb, sa] = rgba;
    let sa_u = u32::from(sa);
    if sa_u == 0 || gw <= 0 || gh <= 0 {
        return;
    }
    let sy_start = (-y0).max(0);
    let sy_end = (ph - y0).min(gh);
    let sx_start = (-x0).max(0);
    let sx_end = (pw - x0).min(gw);
    if sy_start >= sy_end || sx_start >= sx_end {
        return;
    }
    let pixels = pixmap.pixels_mut();
    let pw_u = pw as usize;
    let gw_u = gw as usize;
    for sy in sy_start..sy_end {
        let row_off = (y0 + sy) as usize * pw_u;
        let mask_row = (sy as usize) * gw_u;
        for sx in sx_start..sx_end {
            let cov = u32::from(glyph.mask[mask_row + sx as usize]);
            if cov == 0 {
                continue;
            }
            let alpha = (cov * sa_u + 127) / 255;
            if alpha == 0 {
                continue;
            }
            let inv_a = 255 - alpha;
            let idx = row_off + (x0 + sx) as usize;
            let dst = pixels[idx];
            let out_a = (alpha + (u32::from(dst.alpha()) * inv_a + 127) / 255).min(255) as u8;
            let out_r = ((u32::from(sr) * alpha + u32::from(dst.red()) * inv_a + 127) / 255)
                .min(u32::from(out_a)) as u8;
            let out_g = ((u32::from(sg) * alpha + u32::from(dst.green()) * inv_a + 127) / 255)
                .min(u32::from(out_a)) as u8;
            let out_b = ((u32::from(sb) * alpha + u32::from(dst.blue()) * inv_a + 127) / 255)
                .min(u32::from(out_a)) as u8;
            if let Some(pm) = tiny_skia::PremultipliedColorU8::from_rgba(out_r, out_g, out_b, out_a) {
                pixels[idx] = pm;
            }
        }
    }
}

struct TextEngine {
    font_cx: FontContext,
    layout_cx: LayoutContext<[u8; 4]>,
    scale_cx: swash::scale::ScaleContext,
    fontdb: Arc<usvg::fontdb::Database>,
    registered_fonts: HashMap<(u64, u32), RegisteredFontInfo>,
    font_is_color: HashMap<(u64, u32), bool>,
    emoji_cache: HashMap<(u64, u32, u16, i32), Option<RenderedColorGlyph>>,
    alpha_glyph_cache: HashMap<(u64, u32, u16, i32, i8), Option<CachedAlphaGlyph>>,
    default_svg_fonts_primed: bool,
    subtitle_cache: HashMap<SubtitleCacheKey, ShapedSubtitleToken>,
    devoice_circle: Option<slint::Image>,
}

thread_local! {
    static SHARED_TEXT_ENGINE: RefCell<TextEngine> = RefCell::new(TextEngine::new());
}

const RICH_EMOJI_FALLBACK_STACK: &str =
    "Noto Sans CJK JP, Noto Color Emoji, Apple Color Emoji, Segoe UI Emoji, sans-serif";

impl TextEngine {
    fn new() -> Self {
        Self {
            font_cx: FontContext::new(),
            layout_cx: LayoutContext::new(),
            scale_cx: swash::scale::ScaleContext::new(),
            fontdb: Arc::new(usvg::fontdb::Database::new()),
            registered_fonts: HashMap::new(),
            font_is_color: HashMap::new(),
            emoji_cache: HashMap::new(),
            alpha_glyph_cache: HashMap::new(),
            default_svg_fonts_primed: false,
            subtitle_cache: HashMap::new(),
            devoice_circle: None,
        }
    }

    /// Ensure `self.fontdb` has a real CJK-capable sans-serif font registered and mapped
    /// to SVG generic families (`sans-serif`, `serif`, `monospace`). Dictionary SVGs such
    /// as `smk8/運用-fill.svg` use `<text font-family="sans-serif">` inside `<mask>`; if
    /// `sans-serif` is unresolved, the cutout text disappears and renders as a solid box.
    fn ensure_default_svg_fonts(&mut self) {
        if self.default_svg_fonts_primed {
            return;
        }
        self.default_svg_fonts_primed = true;
        let sample = self.shape_single_line("運用あA", "", 16.0, 400, false, [255, 255, 255, 255]);
        let mut primary_family: Option<String> = None;
        for line in sample.lines() {
            for run in line.runs() {
                let info = self.ensure_font_registered(run.font().clone().borrow_font());
                if primary_family.is_none() {
                    primary_family = Some(info.family);
                }
            }
        }
        if let Some(family) = primary_family {
            let db = Arc::make_mut(&mut self.fontdb);
            db.set_sans_serif_family(&family);
            db.set_serif_family(&family);
            db.set_monospace_family(&family);
        }
    }

    fn font_supports_color(&mut self, font: &parley::FontData) -> bool {
        let key = (font.data.id(), font.index);
        if let Some(&is_color) = self.font_is_color.get(&key) {
            return is_color;
        }
        let is_color = swash::FontRef::from_index(font.data.data(), font.index as usize)
            .is_some_and(|font_ref| {
                let scaler = self.scale_cx.builder(font_ref).size(16.0).build();
                scaler.has_color_bitmaps() || scaler.has_color_outlines()
            });
        self.font_is_color.insert(key, is_color);
        is_color
    }

    fn try_render_color_glyph(
        &mut self,
        font: &parley::FontData,
        glyph_id: u16,
        font_size: f32,
    ) -> Option<RenderedColorGlyph> {
        if !self.font_supports_color(font) {
            return None;
        }
        let size_tenth = (font_size * 10.0).round() as i32;
        let key = (font.data.id(), font.index, glyph_id, size_tenth);
        if let Some(cached) = self.emoji_cache.get(&key) {
            return cached.clone();
        }
        if self.emoji_cache.len() > 2048 {
            self.emoji_cache.clear();
        }
        let rendered = (|| -> Option<RenderedColorGlyph> {
            let font_ref = swash::FontRef::from_index(font.data.data(), font.index as usize)?;
            let mut scaler = self
                .scale_cx
                .builder(font_ref)
                .size(font_size.max(6.0))
                .hint(false)
                .build();
            let image = swash::scale::Render::new(&[
                swash::scale::Source::ColorOutline(0),
                swash::scale::Source::ColorBitmap(swash::scale::StrikeWith::BestFit),
            ])
            .render(&mut scaler, glyph_id)?;
            if image.content != swash::scale::image::Content::Color
                || image.placement.width == 0
                || image.placement.height == 0
            {
                return None;
            }
            let w = image.placement.width;
            let h = image.placement.height;
            let mut pixmap = tiny_skia::Pixmap::new(w, h)?;
            for (dst, src) in pixmap.pixels_mut().iter_mut().zip(image.data.chunks_exact(4)) {
                let [r, g, b, a] = [src[0], src[1], src[2], src[3]];
                let pr = (u32::from(r) * u32::from(a) + 127) / 255;
                let pg = (u32::from(g) * u32::from(a) + 127) / 255;
                let pb = (u32::from(b) * u32::from(a) + 127) / 255;
                if let Some(pm) =
                    tiny_skia::PremultipliedColorU8::from_rgba(pr as u8, pg as u8, pb as u8, a)
                {
                    *dst = pm;
                }
            }
            Some(RenderedColorGlyph {
                pixmap: Arc::new(pixmap),
                left: image.placement.left as f32,
                top: image.placement.top as f32,
            })
        })();
        self.emoji_cache.insert(key, rendered.clone());
        rendered
    }

    fn try_render_alpha_glyph(
        &mut self,
        font: &parley::FontData,
        glyph_id: u16,
        font_size: f32,
        synthetic_italic: bool,
    ) -> Option<CachedAlphaGlyph> {
        let size_tenth = (font_size * 10.0).round() as i32;
        let skew_key: i8 = if synthetic_italic { 1 } else { 0 };
        let key = (font.data.id(), font.index, glyph_id, size_tenth, skew_key);
        if let Some(cached) = self.alpha_glyph_cache.get(&key) {
            return cached.clone();
        }
        if self.alpha_glyph_cache.len() > 16384 {
            self.alpha_glyph_cache.clear();
        }
        let rendered = (|| -> Option<CachedAlphaGlyph> {
            let font_ref = swash::FontRef::from_index(font.data.data(), font.index as usize)?;
            let mut scaler = self
                .scale_cx
                .builder(font_ref)
                .size(font_size.max(6.0))
                .hint(true)
                .build();
            let mut render = swash::scale::Render::new(&[
                swash::scale::Source::Outline,
                swash::scale::Source::Bitmap(swash::scale::StrikeWith::BestFit),
            ]);
            if synthetic_italic {
                render.transform(Some(swash::zeno::Transform::skew(
                    swash::zeno::Angle::from_degrees(-14.0),
                    swash::zeno::Angle::ZERO,
                )));
            }
            let image = render.render(&mut scaler, glyph_id)?;
            if image.content != swash::scale::image::Content::Mask
                || image.placement.width == 0
                || image.placement.height == 0
            {
                return None;
            }
            Some(CachedAlphaGlyph {
                width: image.placement.width,
                height: image.placement.height,
                left: image.placement.left,
                top: image.placement.top,
                mask: Arc::from(image.data),
            })
        })();
        self.alpha_glyph_cache.insert(key, rendered.clone());
        rendered
    }

    fn collect_glossary_glyphs_from_layout(
        &mut self,
        layout: &Layout<[u8; 4]>,
        origin_x: f32,
        origin_y: f32,
        underline: bool,
        strikethrough: bool,
        collector: &mut PaintCollector,
    ) {
        for line in layout.lines() {
            let baseline = origin_y + line.metrics().baseline;
            for item in line.items() {
                if let PositionedLayoutItem::GlyphRun(glyph_run) = item {
                    let run = glyph_run.run();
                    let run_font = run.font().clone();
                    let font_size = run.font_size();
                    let brush = glyph_run.style().brush;
                    let is_color_font = self.font_supports_color(run_font.borrow_font());
                    let synth_italic = run.synthesis().skew().is_some();
                    let run_x = origin_x + glyph_run.offset();
                    let mut gx = run_x;
                    for g in glyph_run.positioned_glyphs() {
                        let px = origin_x + g.x;
                        let py = origin_y + g.y;
                        let rendered_color = is_color_font
                            && self
                                .try_render_color_glyph(
                                    run_font.borrow_font(),
                                    g.id as u16,
                                    font_size,
                                )
                                .is_some_and(|cg| {
                                    collector.images.push((px + cg.left, py - cg.top, cg.pixmap));
                                    true
                                });
                        if !rendered_color {
                            if let Some(ag) = self.try_render_alpha_glyph(
                                run_font.borrow_font(),
                                g.id as u16,
                                font_size,
                                synth_italic,
                            ) {
                                collector.mask_glyphs.push(PositionedMaskGlyph {
                                    x: px.round() as i32 + ag.left,
                                    y: py.round() as i32 - ag.top,
                                    glyph: ag,
                                    rgba: brush,
                                });
                            }
                        }
                        gx = gx.max(px + g.advance);
                    }
                    let adv = (gx - run_x).max(0.0);
                    if underline && adv > 0.0 {
                        collector
                            .rects
                            .push((run_x, baseline + 2.0, adv, 1.0, brush));
                    }
                    if strikethrough && adv > 0.0 {
                        collector
                            .rects
                            .push((run_x, baseline - font_size * 0.32, adv, 1.0, brush));
                    }
                }
            }
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
            Cow::Borrowed(RICH_EMOJI_FALLBACK_STACK)
        } else {
            Cow::Owned(format!("{font_family}, {RICH_EMOJI_FALLBACK_STACK}"))
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
    ) -> (Vec<PositionedSvgCluster>, Vec<(f32, f32, Arc<tiny_skia::Pixmap>)>) {
        let mut out = Vec::new();
        let mut color_images = Vec::new();
        for line in layout.lines() {
            let baseline = origin_y + line.metrics().baseline;
            let mut prev_run_key: Option<(usize, usize, u64, u32)> = None;
            let mut run_glyph_cursor: usize = 0;
            for item in line.items() {
                match item {
                    PositionedLayoutItem::InlineBox(_) => {
                        prev_run_key = None;
                        run_glyph_cursor = 0;
                    }
                    PositionedLayoutItem::GlyphRun(glyph_run) => {
                        let run = glyph_run.run();
                        let run_font = run.font().clone();
                        let run_key = (
                            run.text_range().start,
                            run.text_range().end,
                            run_font.data.id(),
                            run_font.index,
                        );
                        if prev_run_key != Some(run_key) {
                            prev_run_key = Some(run_key);
                            run_glyph_cursor = 0;
                        }
                        let glyph_start = run_glyph_cursor;
                        let glyph_count = glyph_run.glyphs().count();
                        run_glyph_cursor += glyph_count;
                        let glyph_end = glyph_start + glyph_count;

                        let font_info = self.ensure_font_registered(run_font.borrow_font());
                        let font_size = run.font_size();
                        let brush = glyph_run.style().brush;
                        let run_underline = glyph_run.style().underline.is_some();
                        let run_strike = glyph_run.style().strikethrough.is_some();
                        let mut x_cursor = origin_x + glyph_run.offset();
                        let mut cluster_glyph_idx = 0usize;
                        let mut prev_cluster_in_slice = false;
                        let mut prev_cluster_rendered_as_color = false;

                        for cluster in run.visual_clusters() {
                            let c_glyph_count = cluster.glyphs().count();
                            let in_slice = if c_glyph_count == 0 {
                                prev_cluster_in_slice || (cluster_glyph_idx == 0 && glyph_start == 0)
                            } else {
                                let matched = cluster_glyph_idx < glyph_end
                                    && (cluster_glyph_idx + c_glyph_count) > glyph_start;
                                prev_cluster_in_slice = matched;
                                matched
                            };
                            cluster_glyph_idx += c_glyph_count;
                            if !in_slice {
                                if c_glyph_count > 0 {
                                    prev_cluster_rendered_as_color = false;
                                }
                                continue;
                            }

                            let range = cluster.text_range();
                            let advance = cluster.advance();
                            let rendered_as_color = if c_glyph_count == 0 {
                                // Zero-glyph ligature continuation (e.g. `\u{fe0f}`, `\u{20e3}` in
                                // `1️⃣`/`2️⃣`, regional indicator second half in `🇯🇵`, ZWJ emoji parts):
                                // inherit color-emoji suppression so `usvg` never paints a stray box
                                // on top of the rendered color bitmap.
                                prev_cluster_rendered_as_color
                            } else if self.font_supports_color(run_font.borrow_font()) {
                                let mut gx = x_cursor;
                                let mut any_color = false;
                                for g in cluster.glyphs() {
                                    if let Some(cg) = self.try_render_color_glyph(
                                        run_font.borrow_font(),
                                        g.id as u16,
                                        font_size,
                                    ) {
                                        color_images.push((
                                            gx + g.x + cg.left,
                                            baseline + g.y - cg.top,
                                            cg.pixmap,
                                        ));
                                        any_color = true;
                                    }
                                    gx += g.advance;
                                }
                                prev_cluster_rendered_as_color = any_color;
                                any_color
                            } else {
                                prev_cluster_rendered_as_color = false;
                                false
                            };
                            if !rendered_as_color {
                                if let Some(slice) = source_text.get(range.clone()) {
                                    if !slice.is_empty()
                                        && !slice.chars().all(|c| c == '\n' || c == '\r')
                                    {
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
                            }
                            x_cursor += advance;
                        }
                    }
                }
            }
        }
        (out, color_images)
    }

    fn shape_and_render_subtitle_token(
        &mut self,
        word: &SubtitleWord,
        font_px: f32,
        show_furigana: bool,
        style: &SubtitleRenderStyle,
    ) -> ShapedSubtitleToken {
        let font_px = font_px.clamp(8.0, 140.0);
        let line_spacing_extra = style.line_spacing.clamp(0.0, 120.0);
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
            bg_rgba: [0, 0, 0, 0],
            stroke_tenth: (style.stroke_size * 10.0).round() as i32,
            furigana_pct: (style.furigana_ratio * 100.0).round() as i32,
            line_spacing_tenth: (line_spacing_extra * 10.0).round() as i32,
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
        // Keep token layout advance equal to the true typographic width so adjacent tokens
        // never develop artificial gaps, while letting strokes/shadows bleed visually into
        // `bleed_x` on each side of the rendered image.
        let bleed_x = if stroke_px > 0.0 {
            (stroke_px * 0.75 + 1.5).ceil()
        } else {
            1.0
        };
        let pad_y = (stroke_px * 0.55).ceil().max(1.0);

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

        let layout_width = base_width.max(ruby_width).ceil().max(1.0);
        let half_line_spacing = line_spacing_extra * 0.5;
        let total_height = (ruby_space + base_height + pad_y * 2.0 + line_spacing_extra)
            .ceil()
            .max(2.0);
        let base_origin_x_token = (layout_width - base_width) * 0.5;
        let base_origin_x_pix = bleed_x + base_origin_x_token;
        let base_origin_y = ruby_space + pad_y + half_line_spacing;

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

        let (base_svg_clusters, base_color_images) = self.collect_svg_clusters_from_layout(
            &base_layout,
            word.text.as_str(),
            base_origin_x_pix,
            base_origin_y,
            None,
            None,
        );
        let (ruby_svg_clusters, ruby_color_images) = if let Some(ref r_layout) = ruby_layout {
            let r_origin_x_pix = bleed_x + (layout_width - ruby_width) * 0.5;
            let r_origin_y = (ruby_space - ruby_height).max(0.0) + pad_y * 0.5 + half_line_spacing;
            self.collect_svg_clusters_from_layout(
                r_layout,
                word.reading.as_str(),
                r_origin_x_pix,
                r_origin_y,
                None,
                None,
            )
        } else {
            (Vec::new(), Vec::new())
        };

        let pix_w = ((layout_width + bleed_x * 2.0).ceil() as u32).max(1);
        let pix_h = (total_height.ceil() as u32).max(1);
        let mut pixmap =
            tiny_skia::Pixmap::new(pix_w, pix_h).unwrap_or_else(|| tiny_skia::Pixmap::new(1, 1).unwrap());

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

        if !ruby_color_images.is_empty() || !base_color_images.is_empty() {
            let img_paint = tiny_skia::PixmapPaint {
                opacity: 1.0,
                blend_mode: tiny_skia::BlendMode::SourceOver,
                quality: tiny_skia::FilterQuality::Bilinear,
            };
            for (ix, iy, img_pm) in ruby_color_images.into_iter().chain(base_color_images) {
                pixmap.draw_pixmap(
                    ix.round() as i32,
                    iy.round() as i32,
                    (*img_pm).as_ref(),
                    &img_paint,
                    tiny_skia::Transform::default(),
                    None,
                );
            }
        }

        let image = pixmap_to_slint_image(&pixmap);
        let shaped = ShapedSubtitleToken {
            width_px: layout_width,
            height_px: total_height,
            bleed_x_px: bleed_x,
            glyph_x_px: base_origin_x_token,
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

pub const CUSTOM_POPUP_CSS_FILENAME: &str = "popup.css";
pub const CUSTOM_POPUP_OUTER_CSS_FILENAME: &str = "popup-outer.css";

pub fn default_custom_popup_css_template() -> &'static str {
    r#"/* Memento Popup CSS (~/.config/memento/popup.css)
 *
 * Customize inner typography, pitch headword colors, 2-column card layout,
 * per-dictionary badge/tint colors, hover-to-reveal dictionaries, frequency
 * pill colors, and structured-content glossary rules.
 */

:root {
    --text-color: #F8FAFC;
    --muted-color: #94A3B8;
    --headword-color: #FFFFFF;
    --headword-size: 23px;
    --reading-color: #94A3B8;
    --reading-size: 12px;

    --font-family: "Noto Sans CJK JP";
    --glossary-font-size: 14.5px;
    --link-color: #93C5FD;
    --divider-color: #30373D;
    --term-divider-color: #52616B;

    --freq-bg: #07513E;
    --freq-color: #9CE5C7;
    --freq-font-size: 10px;
    --pitch-color: #B9A1E8;
    --pitch-dict-bg: #6640BE;
    --dict-badge-bg: #6D28D9;
    --dict-badge-color: #EDE9FE;
    --anki-tag-bg: #162B3D;
    --anki-tag-border: #38BDF888;
    --anki-tag-color: #BAE6FD;

    /* Set to 2 (with --popup-width >= 600px in popup-outer.css) for a 2-column card grid */
    --definition-columns: 1;
    --definition-columns-min-width: 560px;
    --definition-gap: 8px;

    /* Clamp long definitions to N lines with a "Show more" toggle (0 or none to disable) */
    /* --collapsible-definition-line-count: 5; */
}

/* Example: Color headwords by pitch accent category
:root {
    --pitch-heiban-color: #59b2ff;
    --pitch-atamadaka-color: #ff6666;
    --pitch-nakadaka-color: #ff9b54;
    --pitch-odaka-color: #67e47d;
    --pitch-kifuku-color: #af85f4;
}
*/

/* Example: Per-dictionary badge color, subtle card tint, collapsible lines & hover-to-reveal JMdict
[data-dictionary="デジタル大辞泉"] {
    --dict-color: rgb(170, 0, 0);
    --dict-bg-opacity: 0.08;
    --collapsible-definition-line-count: 4;
}
[data-dictionary*="JMdict"] {
    --dict-color: rgb(0, 132, 255);
    --dict-bg-opacity: 0.04;
    --glossary-opacity: 0;
    --glossary-hover-opacity: 1;
    --glossary-solo-opacity: 1;
    --collapsible-definition-line-count: none;
}
*/

/* Example: Per-frequency-dictionary pill colors
[data-frequency^="JPDB"] { background-color: #ff3e3d; color: #ffffff; }
[data-frequency="CC100"]   { background-color: #6238fa; color: #ffffff; }
*/

/* Example: Hide UI sections or remove link underlines
.term-tags { display: none; }
.pitch-dict { display: none; }
.kanji-extra { display: none; }
.derivations { display: compact; }
a { text-decoration: none; }
*/
"#
}

pub fn default_custom_popup_outer_css_template() -> &'static str {
    r#"/* Memento Popup Outer CSS (~/.config/memento/popup-outer.css)
 *
 * Controls the floating popup card container: translucent RGBA background,
 * border, corner radius, drop shadow, padding, and optional width/height.
 */

.popup {
    background: #1C2024;
    border: 1px solid #384046;
    border-radius: 8px;
    box-shadow: 0 4px 24px #000000DD;
    padding: 12px;
    /* Uncomment to widen the popup (e.g. for 2-column layout):
    width: 780px;
    height: 520px;
    */
}
"#
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CssAttrMatchOp {
    Exists,
    Equals,
    StartsWith,
    Contains,
    EndsWith,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CssAttrMatcher {
    pub op: CssAttrMatchOp,
    pub value: String,
}

impl CssAttrMatcher {
    pub fn matches(&self, target: &str) -> bool {
        match self.op {
            CssAttrMatchOp::Exists => !target.is_empty(),
            CssAttrMatchOp::Equals => target == self.value,
            CssAttrMatchOp::StartsWith => target.starts_with(&self.value),
            CssAttrMatchOp::Contains => target.contains(&self.value),
            CssAttrMatchOp::EndsWith => target.ends_with(&self.value),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct DictStyleRule {
    pub matcher: CssAttrMatcher,
    pub badge_bg: Option<[u8; 4]>,
    pub badge_color: Option<[u8; 4]>,
    pub bg_opacity: Option<f32>,
    pub card_bg: Option<[u8; 4]>,
    pub card_border: Option<[u8; 4]>,
    pub glossary_opacity: Option<f32>,
    pub glossary_hover_opacity: Option<f32>,
    pub glossary_solo_opacity: Option<f32>,
    pub collapsible_line_count: Option<usize>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FreqStyleRule {
    pub matcher: CssAttrMatcher,
    pub bg: Option<[u8; 4]>,
    pub color: Option<[u8; 4]>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PitchDictStyleRule {
    pub matcher: CssAttrMatcher,
    pub bg: [u8; 4],
}

#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedDictStyle {
    pub badge_bg: [u8; 4],
    pub badge_color: [u8; 4],
    pub card_bg: [u8; 4],
    pub card_border: [u8; 4],
    pub card_border_width: f32,
    pub card_radius: f32,
    pub card_padding: f32,
    pub glossary_opacity: f32,
    pub glossary_hover_opacity: f32,
    pub glossary_solo_opacity: f32,
    pub collapsible_line_count: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct InterfaceFontFallback {
    pub family: String,
    pub size_px: f32,
    pub bold: bool,
    pub italic: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct PopupInterfaceFonts {
    pub expression: Option<InterfaceFontFallback>,
    pub reading: Option<InterfaceFontFallback>,
    pub conj: Option<InterfaceFontFallback>,
    pub tag: Option<InterfaceFontFallback>,
    pub glossary: Option<InterfaceFontFallback>,
    pub kanji: Option<InterfaceFontFallback>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PopupCssExplicitOverrides {
    pub font_family: bool,
    pub glossary_font_size: bool,
    pub headword_font_family: bool,
    pub headword_size: bool,
    pub reading_font_family: bool,
    pub reading_size: bool,
    pub conj_font_family: bool,
    pub conj_font_size: bool,
    pub tag_font_family: bool,
    pub tag_font_size: bool,
    pub kanji_font_family: bool,
    pub kanji_font_size: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PopupCssTheme {
    // Outer popup frame
    pub popup_bg: [u8; 4],
    pub popup_border: [u8; 4],
    pub popup_border_width: f32,
    pub popup_radius: f32,
    pub popup_padding: f32,
    pub popup_shadow_blur: f32,
    pub popup_shadow_color: [u8; 4],
    pub popup_width: f32,
    pub popup_height: f32,

    // Typography & core colors
    pub text_color: [u8; 4],
    pub muted_color: [u8; 4],
    pub headword_color: [u8; 4],
    pub headword_size: f32,
    pub reading_color: [u8; 4],
    pub reading_size: f32,
    pub font_family: String,
    pub headword_font_family: String,
    pub reading_font_family: String,
    pub conj_font_family: String,
    pub conj_font_size: f32,
    pub tag_font_family: String,
    pub tag_font_size: f32,
    pub kanji_font_family: String,
    pub kanji_font_size: f32,
    pub glossary_font_size: f32,
    pub link_color: [u8; 4],
    pub divider_color: [u8; 4],
    pub term_divider_color: [u8; 4],

    // Badges & tags
    pub freq_bg: [u8; 4],
    pub freq_color: [u8; 4],
    pub freq_font_size: f32,
    pub pitch_color: [u8; 4],
    pub pitch_dict_bg: [u8; 4],
    pub dict_badge_bg: [u8; 4],
    pub dict_badge_color: [u8; 4],
    pub anki_tag_bg: [u8; 4],
    pub anki_tag_border: [u8; 4],
    pub anki_tag_color: [u8; 4],

    // Definition layout & cards
    pub definition_columns: i32,
    pub definition_columns_min_width: f32,
    pub definition_gap: f32,
    pub collapsible_definition_line_count: usize,
    pub card_bg: [u8; 4],
    pub card_border: [u8; 4],
    pub card_border_width: f32,
    pub card_radius: f32,
    pub card_padding: f32,

    // Visibility & UI toggles
    pub show_term_tags: bool,
    pub show_pitch_dict: bool,
    pub show_kanji_extra: bool,
    pub compact_derivations: bool,

    // Headword pitch category colors
    pub pitch_heiban_color: Option<[u8; 4]>,
    pub pitch_atamadaka_color: Option<[u8; 4]>,
    pub pitch_nakadaka_color: Option<[u8; 4]>,
    pub pitch_odaka_color: Option<[u8; 4]>,
    pub pitch_kifuku_color: Option<[u8; 4]>,

    // Per-dictionary / per-frequency / per-pitch-dict dynamic rules
    pub dict_color_opacity: f32,
    pub dict_rules: Vec<DictStyleRule>,
    pub freq_rules: Vec<FreqStyleRule>,
    pub pitch_dict_rules: Vec<PitchDictStyleRule>,
}

impl Default for PopupCssTheme {
    fn default() -> Self {
        Self {
            popup_bg: [0x1C, 0x20, 0x24, 0xFF],
            popup_border: [0x38, 0x40, 0x46, 0xFF],
            popup_border_width: 1.0,
            popup_radius: 8.0,
            popup_padding: 12.0,
            popup_shadow_blur: 24.0,
            popup_shadow_color: [0x00, 0x00, 0x00, 0xDD],
            popup_width: 0.0,
            popup_height: 0.0,
            text_color: [0xF8, 0xFA, 0xFC, 0xFF],
            muted_color: [0x94, 0xA3, 0xB8, 0xFF],
            headword_color: [0xFF, 0xFF, 0xFF, 0xFF],
            headword_size: 23.0,
            reading_color: [0x94, 0xA3, 0xB8, 0xFF],
            reading_size: 12.0,
            font_family: "Noto Sans CJK JP".to_owned(),
            headword_font_family: String::new(),
            reading_font_family: String::new(),
            conj_font_family: String::new(),
            conj_font_size: 11.0,
            tag_font_family: String::new(),
            tag_font_size: 11.0,
            kanji_font_family: String::new(),
            kanji_font_size: 0.0,
            glossary_font_size: 14.5,
            link_color: [0x93, 0xC5, 0xFD, 0xFF],
            divider_color: [0x30, 0x37, 0x3D, 0xFF],
            term_divider_color: [0x52, 0x61, 0x6B, 0xFF],
            freq_bg: [0x07, 0x51, 0x3E, 0xFF],
            freq_color: [0x9C, 0xE5, 0xC7, 0xFF],
            freq_font_size: 10.0,
            pitch_color: [0xB9, 0xA1, 0xE8, 0xFF],
            pitch_dict_bg: [0x66, 0x40, 0xBE, 0xFF],
            dict_badge_bg: [0x6D, 0x28, 0xD9, 0xFF],
            dict_badge_color: [0xED, 0xE9, 0xFE, 0xFF],
            anki_tag_bg: [0x16, 0x2B, 0x3D, 0xFF],
            anki_tag_border: [0x38, 0xBD, 0xF8, 0x88],
            anki_tag_color: [0xBA, 0xE6, 0xFD, 0xFF],
            definition_columns: 1,
            definition_columns_min_width: 560.0,
            definition_gap: 8.0,
            collapsible_definition_line_count: 0,
            card_bg: [0, 0, 0, 0],
            card_border: [0, 0, 0, 0],
            card_border_width: 0.0,
            card_radius: 0.0,
            card_padding: 0.0,
            show_term_tags: true,
            show_pitch_dict: true,
            show_kanji_extra: true,
            compact_derivations: false,
            pitch_heiban_color: None,
            pitch_atamadaka_color: None,
            pitch_nakadaka_color: None,
            pitch_odaka_color: None,
            pitch_kifuku_color: None,
            dict_color_opacity: 1.0,
            dict_rules: Vec::new(),
            freq_rules: Vec::new(),
            pitch_dict_rules: Vec::new(),
        }
    }
}

impl PopupCssTheme {
    pub fn with_interface_font_fallbacks(
        &self,
        overrides: &PopupCssExplicitOverrides,
        fonts: &PopupInterfaceFonts,
    ) -> Self {
        let mut out = self.clone();
        if let Some(ref f) = fonts.glossary {
            if !overrides.font_family {
                out.font_family = f.family.clone();
            }
            if !overrides.glossary_font_size {
                out.glossary_font_size = f.size_px;
            }
        }
        if let Some(ref f) = fonts.expression {
            if !overrides.headword_font_family && !overrides.font_family {
                out.headword_font_family = f.family.clone();
            }
            if !overrides.headword_size {
                out.headword_size = f.size_px;
            }
        }
        if let Some(ref f) = fonts.reading {
            if !overrides.reading_font_family && !overrides.font_family {
                out.reading_font_family = f.family.clone();
            }
            if !overrides.reading_size {
                out.reading_size = f.size_px;
            }
        }
        if let Some(ref f) = fonts.conj {
            if !overrides.conj_font_family && !overrides.font_family {
                out.conj_font_family = f.family.clone();
            }
            if !overrides.conj_font_size {
                out.conj_font_size = f.size_px;
            }
        }
        if let Some(ref f) = fonts.tag {
            if !overrides.tag_font_family && !overrides.font_family {
                out.tag_font_family = f.family.clone();
            }
            if !overrides.tag_font_size {
                out.tag_font_size = f.size_px;
            }
        }
        if let Some(ref f) = fonts.kanji {
            if !overrides.kanji_font_family && !overrides.font_family {
                out.kanji_font_family = f.family.clone();
            }
            if !overrides.kanji_font_size {
                out.kanji_font_size = f.size_px;
            }
        }
        if out.headword_font_family.is_empty() {
            out.headword_font_family = out.font_family.clone();
        }
        if out.reading_font_family.is_empty() {
            out.reading_font_family = out.font_family.clone();
        }
        if out.conj_font_family.is_empty() {
            out.conj_font_family = out.font_family.clone();
        }
        if out.tag_font_family.is_empty() {
            out.tag_font_family = out.font_family.clone();
        }
        if out.kanji_font_family.is_empty() {
            out.kanji_font_family = out.font_family.clone();
        }
        out
    }

    pub fn resolve_headword_pitch_color(&self, category: Option<&str>) -> [u8; 4] {
        match category {
            Some("heiban") => self.pitch_heiban_color.unwrap_or(self.headword_color),
            Some("atamadaka") => self.pitch_atamadaka_color.unwrap_or(self.headword_color),
            Some("nakadaka") => self.pitch_nakadaka_color.unwrap_or(self.headword_color),
            Some("odaka") => self.pitch_odaka_color.unwrap_or(self.headword_color),
            Some("kifuku") => self.pitch_kifuku_color.unwrap_or(self.headword_color),
            _ => self.headword_color,
        }
    }

    pub fn resolve_dict_style(&self, dict_name: &str) -> ResolvedDictStyle {
        let mut badge_bg = self.dict_badge_bg;
        let mut badge_color = self.dict_badge_color;
        let mut card_bg = self.card_bg;
        let mut card_border = self.card_border;
        let mut bg_opacity = 0.0f32;
        let mut explicit_card_bg = false;
        let mut glossary_opacity = 1.0f32;
        let mut glossary_hover_opacity = 1.0f32;
        let mut glossary_solo_opacity = 1.0f32;
        let mut collapsible_line_count = self.collapsible_definition_line_count;

        for rule in &self.dict_rules {
            if !rule.matcher.matches(dict_name) {
                continue;
            }
            if let Some(c) = rule.badge_bg {
                badge_bg = c;
            }
            if let Some(c) = rule.badge_color {
                badge_color = c;
            }
            if let Some(op) = rule.bg_opacity {
                bg_opacity = op;
            }
            if let Some(c) = rule.card_bg {
                card_bg = c;
                explicit_card_bg = true;
            }
            if let Some(c) = rule.card_border {
                card_border = c;
            }
            if let Some(o) = rule.glossary_opacity {
                glossary_opacity = o;
            }
            if let Some(o) = rule.glossary_hover_opacity {
                glossary_hover_opacity = o;
            }
            if let Some(o) = rule.glossary_solo_opacity {
                glossary_solo_opacity = o;
            }
            if let Some(lines) = rule.collapsible_line_count {
                collapsible_line_count = lines;
            }
        }

        // Blend subtle per-dictionary tint when `--dict-bg-opacity` > 0 and card_bg wasn't overridden.
        let tint_alpha = (bg_opacity * self.dict_color_opacity).clamp(0.0, 1.0);
        if !explicit_card_bg && tint_alpha > 0.001 {
            if card_bg[3] == 0 {
                // Translucent overlay tint so glassy outer popup backgrounds remain translucent!
                card_bg = [
                    badge_bg[0],
                    badge_bg[1],
                    badge_bg[2],
                    (tint_alpha * 255.0).round().clamp(0.0, 255.0) as u8,
                ];
            } else {
                let mix = |a: u8, b: u8| -> u8 {
                    (f32::from(a) * (1.0 - tint_alpha) + f32::from(b) * tint_alpha)
                        .round()
                        .clamp(0.0, 255.0) as u8
                };
                card_bg = [
                    mix(card_bg[0], badge_bg[0]),
                    mix(card_bg[1], badge_bg[1]),
                    mix(card_bg[2], badge_bg[2]),
                    card_bg[3],
                ];
            }
        }

        ResolvedDictStyle {
            badge_bg,
            badge_color,
            card_bg,
            card_border,
            card_border_width: self.card_border_width,
            card_radius: self.card_radius,
            card_padding: self.card_padding,
            glossary_opacity,
            glossary_hover_opacity,
            glossary_solo_opacity,
            collapsible_line_count,
        }
    }

    pub fn resolve_freq_style(&self, freq_dict: &str) -> ([u8; 4], [u8; 4]) {
        let mut bg = self.freq_bg;
        let mut color = self.freq_color;
        for rule in &self.freq_rules {
            if !rule.matcher.matches(freq_dict) {
                continue;
            }
            if let Some(c) = rule.bg {
                bg = c;
            }
            if let Some(c) = rule.color {
                color = c;
            }
        }
        (bg, color)
    }

    pub fn resolve_pitch_dict_bg(&self, pitch_dict: &str) -> [u8; 4] {
        let mut bg = self.pitch_dict_bg;
        for rule in &self.pitch_dict_rules {
            if rule.matcher.matches(pitch_dict) {
                bg = rule.bg;
            }
        }
        bg
    }
}

fn strip_css_comments(css: &str) -> String {
    let mut out = String::with_capacity(css.len());
    let bytes = css.as_bytes();
    let mut i = 0usize;
    let mut in_quote: Option<u8> = None;
    while i < bytes.len() {
        let b = bytes[i];
        if let Some(q) = in_quote {
            if b == b'\\' && i + 1 < bytes.len() {
                out.push(b as char);
                let next_ch = css[i + 1..].chars().next().unwrap();
                out.push(next_ch);
                i += 1 + next_ch.len_utf8();
                continue;
            }
            if b == q {
                in_quote = None;
            }
            let ch = css[i..].chars().next().unwrap();
            out.push(ch);
            i += ch.len_utf8();
            continue;
        }
        if b == b'"' || b == b'\'' {
            in_quote = Some(b);
            out.push(b as char);
            i += 1;
            continue;
        }
        if b == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'*' {
            i += 2;
            while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                i += 1;
            }
            if i + 1 < bytes.len() {
                i += 2;
            } else {
                break;
            }
            continue;
        }
        // Strip JS-style `// ...` comments while preserving URLs like `https://` / `memento://`.
        if b == b'/'
            && i + 1 < bytes.len()
            && bytes[i + 1] == b'/'
            && (i == 0 || bytes[i - 1] != b':')
        {
            i += 2;
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        let ch = css[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

fn clean_css_font_family(raw: &str) -> String {
    let first = raw.split(',').next().unwrap_or("").trim();
    let unquoted = first
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .or_else(|| first.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')))
        .unwrap_or(first)
        .trim();
    unquoted.to_owned()
}

fn parse_css_theme_color(value: &str) -> Option<[u8; 4]> {
    let v = value.trim();
    if v.is_empty() {
        return None;
    }
    if v.eq_ignore_ascii_case("transparent") {
        return Some([0, 0, 0, 0]);
    }
    // Standard CSS #RGBA and #RRGGBBAA (DefColor::from_string uses Qt #AARRGGBB for 8-digit hex).
    if let Some(hex) = v.strip_prefix('#') {
        if hex.len() == 4 && hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            let nibble = |idx: usize| u8::from_str_radix(&hex[idx..idx + 1], 16).ok().map(|n| n * 17);
            return Some([nibble(0)?, nibble(1)?, nibble(2)?, nibble(3)?]);
        }
        if hex.len() == 8 && hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            let byte = |idx: usize| u8::from_str_radix(&hex[idx..idx + 2], 16).ok();
            return Some([byte(0)?, byte(2)?, byte(4)?, byte(6)?]);
        }
    }
    DefColor::from_string(v).map(|c| {
        [
            ((u32::from(c.red) + 128) / 257) as u8,
            ((u32::from(c.green) + 128) / 257) as u8,
            ((u32::from(c.blue) + 128) / 257) as u8,
            ((u32::from(c.alpha) + 128) / 257) as u8,
        ]
    })
}

fn parse_css_length_px(value: &str, base_px: f32) -> Option<f32> {
    let lower = value.trim().to_ascii_lowercase();
    if lower.is_empty() {
        return None;
    }
    let parsed = if lower == "0" {
        Some(0.0)
    } else if let Some(num) = lower.strip_suffix("px") {
        num.trim().parse::<f32>().ok()
    } else if let Some(num) = lower.strip_suffix("rem") {
        num.trim().parse::<f32>().ok().map(|n| n * base_px)
    } else if let Some(num) = lower.strip_suffix("em") {
        num.trim().parse::<f32>().ok().map(|n| n * base_px)
    } else if let Some(num) = lower.strip_suffix("pt") {
        num.trim().parse::<f32>().ok().map(|n| n * (96.0 / 72.0))
    } else if let Some(num) = lower.strip_suffix('%') {
        num.trim().parse::<f32>().ok().map(|n| base_px * (n / 100.0))
    } else {
        None
    }?;
    (parsed.is_finite() && parsed >= 0.0).then_some(parsed)
}

fn parse_css_unit_float(value: &str) -> Option<f32> {
    let t = value.trim();
    if let Some(pct) = t.strip_suffix('%') {
        pct.trim()
            .parse::<f32>()
            .ok()
            .filter(|v| v.is_finite())
            .map(|v| (v / 100.0).clamp(0.0, 1.0))
    } else {
        t.parse::<f32>()
            .ok()
            .filter(|v| v.is_finite())
            .map(|v| v.clamp(0.0, 1.0))
    }
}

fn parse_css_border_shorthand(value: &str, base_px: f32) -> (Option<f32>, Option<[u8; 4]>) {
    let mut width = None;
    let mut color = None;
    for tok in mdef::split_css_tokens(value) {
        let lower = tok.to_ascii_lowercase();
        if lower == "none" {
            width = Some(0.0);
            continue;
        }
        if matches!(
            lower.as_str(),
            "solid" | "dashed" | "dotted" | "double" | "groove" | "ridge" | "inset" | "outset"
        ) {
            continue;
        }
        if width.is_none() {
            if let Some(w) = parse_css_length_px(&tok, base_px) {
                width = Some(w);
                continue;
            }
        }
        if color.is_none() {
            if let Some(c) = parse_css_theme_color(&tok) {
                color = Some(c);
            }
        }
    }
    (width, color)
}

fn parse_css_box_shadow(value: &str, base_px: f32) -> (Option<f32>, Option<[u8; 4]>) {
    let trimmed = value.trim();
    if trimmed.eq_ignore_ascii_case("none") {
        return (Some(0.0), Some([0, 0, 0, 0]));
    }
    // Use first shadow if comma-separated (ignoring commas inside rgba(...))
    let mut first_end = trimmed.len();
    let mut depth = 0i32;
    for (idx, ch) in trimmed.char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => depth = (depth - 1).max(0),
            ',' if depth == 0 => {
                first_end = idx;
                break;
            }
            _ => {}
        }
    }
    let first = &trimmed[..first_end];
    let mut lengths = Vec::new();
    let mut color = None;
    for tok in mdef::split_css_tokens(first) {
        if tok.eq_ignore_ascii_case("inset") {
            continue;
        }
        if let Some(px) = parse_css_length_px(&tok, base_px) {
            lengths.push(px);
        } else if color.is_none() {
            if let Some(c) = parse_css_theme_color(&tok) {
                color = Some(c);
            }
        }
    }
    let blur = lengths.get(2).copied().or_else(|| lengths.first().copied());
    (blur, color)
}

fn parse_attr_matcher(selector: &str, attr_name: &str) -> Option<CssAttrMatcher> {
    let mut rest = selector;
    while let Some(open) = rest.find('[') {
        let after = &rest[open + 1..];
        let close = after.find(']')?;
        let inner = after[..close].trim();
        rest = &after[close + 1..];

        // Check if attribute name matches (case-insensitive)
        let (name_part, op_and_val) = if let Some(idx) = inner.find('=') {
            let prefix_len = if idx > 0 && matches!(inner.as_bytes()[idx - 1], b'^' | b'*' | b'$') {
                idx - 1
            } else {
                idx
            };
            (inner[..prefix_len].trim(), Some(&inner[prefix_len..]))
        } else {
            (inner.trim(), None)
        };

        if !name_part.eq_ignore_ascii_case(attr_name) {
            continue;
        }
        let Some(rhs) = op_and_val else {
            return Some(CssAttrMatcher {
                op: CssAttrMatchOp::Exists,
                value: String::new(),
            });
        };
        let (op, raw_val) = if let Some(v) = rhs.strip_prefix("^=") {
            (CssAttrMatchOp::StartsWith, v.trim())
        } else if let Some(v) = rhs.strip_prefix("*=") {
            (CssAttrMatchOp::Contains, v.trim())
        } else if let Some(v) = rhs.strip_prefix("$=") {
            (CssAttrMatchOp::EndsWith, v.trim())
        } else if let Some(v) = rhs.strip_prefix('=') {
            (CssAttrMatchOp::Equals, v.trim())
        } else {
            continue;
        };
        let unquoted = raw_val
            .strip_prefix('"')
            .and_then(|s| s.strip_suffix('"'))
            .or_else(|| raw_val.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')))
            .unwrap_or(raw_val)
            .replace("\\.", ".");
        return Some(CssAttrMatcher { op, value: unquoted });
    }
    None
}

fn resolve_css_var_refs(value: &str, vars: &HashMap<String, String>, depth: u8) -> String {
    if depth >= 8 || (!value.contains("var(") && !value.contains("VAR(")) {
        return value.to_owned();
    }
    let mut out = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(pos) = rest.to_ascii_lowercase().find("var(") {
        out.push_str(&rest[..pos]);
        let after = &rest[pos + 4..];
        let mut paren_depth = 1i32;
        let mut close_idx = None;
        for (idx, ch) in after.char_indices() {
            match ch {
                '(' => paren_depth += 1,
                ')' => {
                    paren_depth -= 1;
                    if paren_depth == 0 {
                        close_idx = Some(idx);
                        break;
                    }
                }
                _ => {}
            }
        }
        let Some(end) = close_idx else {
            out.push_str(&rest[pos..]);
            return out;
        };
        let inner = &after[..end];
        let (var_name, fallback) = match inner.find(',') {
            Some(comma) => (inner[..comma].trim(), Some(inner[comma + 1..].trim())),
            None => (inner.trim(), None),
        };
        if let Some(replacement) = vars.get(&var_name.to_ascii_lowercase()) {
            out.push_str(&resolve_css_var_refs(replacement, vars, depth + 1));
        } else if let Some(fb) = fallback {
            let lower_name = var_name.to_ascii_lowercase();
            if matches!(
                lower_name.as_str(),
                "--font-size-no-units" | "--text-color" | "--background-color" | "--line-height"
            ) {
                out.push_str(&rest[pos..pos + 4 + end + 1]);
            } else {
                out.push_str(&resolve_css_var_refs(fb, vars, depth + 1));
            }
        } else {
            out.push_str(&rest[pos..pos + 4 + end + 1]);
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    out
}

fn parse_flat_declarations(body: &str) -> Vec<(String, String)> {
    let mut decls = Vec::new();
    for raw_decl in body.split(';') {
        let decl = raw_decl.trim();
        if let Some(colon) = decl.find(':') {
            let prop = decl[..colon].trim().to_ascii_lowercase();
            let mut val = decl[colon + 1..].trim();
            if val.len() >= 10
                && val.as_bytes()[val.len() - 10..].eq_ignore_ascii_case(b"!important")
            {
                val = val[..val.len() - 10].trim();
            }
            if !prop.is_empty() && !val.is_empty() {
                decls.push((prop, val.to_owned()));
            }
        }
    }
    decls
}

fn combine_nested_selectors(parent: &str, child: &str) -> String {
    let p_trimmed = parent.trim();
    if p_trimmed.is_empty()
        || p_trimmed.eq_ignore_ascii_case(":root")
        || p_trimmed.eq_ignore_ascii_case(":root[data-theme=\"dark\"]")
        || p_trimmed.eq_ignore_ascii_case(":root[data-theme='dark']")
    {
        return child.trim().to_owned();
    }
    let mut combined = Vec::new();
    for p in p_trimmed.split(',') {
        let p = p.trim();
        if p.is_empty() {
            continue;
        }
        for c in child.split(',') {
            let c = c.trim();
            if c.is_empty() {
                continue;
            }
            if c.contains('&') {
                combined.push(c.replace('&', p));
            } else {
                combined.push(format!("{p} {c}"));
            }
        }
    }
    combined.join(", ")
}

/// Parse CSS rules recursively, flattening nested CSS blocks (e.g. `:root[data-theme="dark"] { .a { ... } }`
/// or `[data-dictionary="..."] { .gloss-sc-span { ... } }`) into flat `(selector_list, decls)` pairs.
fn parse_css_rules_flattened(
    input: &str,
    parent_selector: &str,
    out: &mut Vec<(String, Vec<(String, String)>)>,
    depth: u8,
) {
    if depth >= 6 {
        return;
    }
    let bytes = input.as_bytes();
    let mut i = 0usize;
    let mut seg_start = 0usize;
    let mut direct_decls_buf = String::new();
    let mut in_quote: Option<u8> = None;

    while i < bytes.len() {
        let b = bytes[i];
        if let Some(q) = in_quote {
            if b == b'\\' && i + 1 < bytes.len() {
                i += 2;
                continue;
            }
            if b == q {
                in_quote = None;
            }
            i += 1;
            continue;
        }
        match b {
            b'"' | b'\'' => {
                in_quote = Some(b);
                i += 1;
            }
            b';' => {
                if !parent_selector.is_empty() {
                    direct_decls_buf.push_str(&input[seg_start..=i]);
                }
                seg_start = i + 1;
                i += 1;
            }
            b'{' => {
                let header = input[seg_start..i].trim();
                let body_start = i + 1;
                let mut brace_depth = 1i32;
                let mut inner_quote: Option<u8> = None;
                i += 1;
                while i < bytes.len() && brace_depth > 0 {
                    let ib = bytes[i];
                    if let Some(q) = inner_quote {
                        if ib == b'\\' && i + 1 < bytes.len() {
                            i += 2;
                            continue;
                        }
                        if ib == q {
                            inner_quote = None;
                        }
                        i += 1;
                        continue;
                    }
                    match ib {
                        b'"' | b'\'' => inner_quote = Some(ib),
                        b'{' => brace_depth += 1,
                        b'}' => brace_depth -= 1,
                        _ => {}
                    }
                    i += 1;
                }
                let body_end = if brace_depth == 0 { i - 1 } else { i };
                let body = &input[body_start..body_end];
                if !header.is_empty() && !header.starts_with('@') {
                    let full_sel = combine_nested_selectors(parent_selector, header);
                    parse_css_rules_flattened(body, &full_sel, out, depth + 1);
                }
                seg_start = i;
            }
            _ => i += 1,
        }
    }

    if !parent_selector.is_empty() {
        if seg_start < bytes.len() {
            direct_decls_buf.push_str(&input[seg_start..]);
        }
        let decls = parse_flat_declarations(&direct_decls_buf);
        if !decls.is_empty() {
            out.push((parent_selector.to_owned(), decls));
        }
    }
}

fn parse_css_collapsible_lines(value: &str) -> Option<usize> {
    let lower = value.trim().to_ascii_lowercase();
    if lower.is_empty() {
        return None;
    }
    if matches!(
        lower.as_str(),
        "none" | "0" | "false" | "off" | "unset" | "initial"
    ) {
        return Some(0);
    }
    if let Ok(n) = lower.parse::<usize>() {
        return Some(n.min(200));
    }
    if let Ok(f) = lower.parse::<f32>() {
        if f.is_finite() && f >= 0.0 {
            return Some((f.round() as usize).min(200));
        }
    }
    None
}

fn apply_theme_rules(
    rules: &[(String, Vec<(String, String)>)],
    vars: &HashMap<String, String>,
    theme: &mut PopupCssTheme,
    overrides: &mut PopupCssExplicitOverrides,
    is_outer: bool,
) {
    for (selector_list, decls) in rules {
        for raw_sel in selector_list.split(',') {
            let sel = raw_sel.trim();
            let lower = sel.to_ascii_lowercase();

            // 1. `:root` / `:root[data-theme="dark"]` / `html` custom properties
            if matches!(
                lower.as_str(),
                ":root" | "html" | ":root[data-theme=\"dark\"]" | ":root[data-theme='dark']"
            ) {
                for (prop, raw_val) in decls {
                    let val = resolve_css_var_refs(raw_val, vars, 0);
                    match prop.as_str() {
                        "--popup-bg" => {
                            if let Some(c) = parse_css_theme_color(&val) {
                                theme.popup_bg = c;
                            }
                        }
                        "--popup-border" => {
                            if let Some(c) = parse_css_theme_color(&val) {
                                theme.popup_border = c;
                            }
                        }
                        "--popup-border-width" => {
                            if let Some(px) = parse_css_length_px(&val, theme.glossary_font_size) {
                                theme.popup_border_width = px.clamp(0.0, 12.0);
                            }
                        }
                        "--popup-radius" => {
                            if let Some(px) = parse_css_length_px(&val, theme.glossary_font_size) {
                                theme.popup_radius = px.clamp(0.0, 32.0);
                            }
                        }
                        "--popup-padding" => {
                            if let Some(px) = parse_css_length_px(&val, theme.glossary_font_size) {
                                theme.popup_padding = px.clamp(0.0, 40.0);
                            }
                        }
                        "--popup-shadow-blur" => {
                            if let Some(px) = parse_css_length_px(&val, theme.glossary_font_size) {
                                theme.popup_shadow_blur = px.clamp(0.0, 64.0);
                            }
                        }
                        "--popup-shadow-color" => {
                            if let Some(c) = parse_css_theme_color(&val) {
                                theme.popup_shadow_color = c;
                            }
                        }
                        "--popup-width" => {
                            if let Some(px) = parse_css_length_px(&val, theme.glossary_font_size) {
                                theme.popup_width = if px == 0.0 { 0.0 } else { px.clamp(280.0, 1600.0) };
                            }
                        }
                        "--popup-height" => {
                            if let Some(px) = parse_css_length_px(&val, theme.glossary_font_size) {
                                theme.popup_height = if px == 0.0 { 0.0 } else { px.clamp(200.0, 1200.0) };
                            }
                        }
                        "--text-color" => {
                            if let Some(c) = parse_css_theme_color(&val) {
                                theme.text_color = c;
                            }
                        }
                        "--muted-color" => {
                            if let Some(c) = parse_css_theme_color(&val) {
                                theme.muted_color = c;
                            }
                        }
                        "--headword-color" => {
                            if let Some(c) = parse_css_theme_color(&val) {
                                theme.headword_color = c;
                            }
                        }
                        "--headword-size" => {
                            if let Some(px) = parse_css_length_px(&val, theme.glossary_font_size) {
                                theme.headword_size = px.clamp(12.0, 48.0);
                                overrides.headword_size = true;
                            }
                        }
                        "--headword-font-family" => {
                            let ff = clean_css_font_family(&val);
                            if !ff.is_empty() {
                                theme.headword_font_family = ff;
                                overrides.headword_font_family = true;
                            }
                        }
                        "--reading-color" => {
                            if let Some(c) = parse_css_theme_color(&val) {
                                theme.reading_color = c;
                            }
                        }
                        "--reading-size" => {
                            if let Some(px) = parse_css_length_px(&val, theme.glossary_font_size) {
                                theme.reading_size = px.clamp(8.0, 28.0);
                                overrides.reading_size = true;
                            }
                        }
                        "--reading-font-family" => {
                            let ff = clean_css_font_family(&val);
                            if !ff.is_empty() {
                                theme.reading_font_family = ff;
                                overrides.reading_font_family = true;
                            }
                        }
                        "--conj-font-family" => {
                            let ff = clean_css_font_family(&val);
                            if !ff.is_empty() {
                                theme.conj_font_family = ff;
                                overrides.conj_font_family = true;
                            }
                        }
                        "--conj-font-size" => {
                            if let Some(px) = parse_css_length_px(&val, 11.0) {
                                theme.conj_font_size = px.clamp(8.0, 24.0);
                                overrides.conj_font_size = true;
                            }
                        }
                        "--tag-font-family" => {
                            let ff = clean_css_font_family(&val);
                            if !ff.is_empty() {
                                theme.tag_font_family = ff;
                                overrides.tag_font_family = true;
                            }
                        }
                        "--tag-font-size" => {
                            if let Some(px) = parse_css_length_px(&val, 11.0) {
                                theme.tag_font_size = px.clamp(8.0, 22.0);
                                overrides.tag_font_size = true;
                            }
                        }
                        "--kanji-font-family" => {
                            let ff = clean_css_font_family(&val);
                            if !ff.is_empty() {
                                theme.kanji_font_family = ff;
                                overrides.kanji_font_family = true;
                            }
                        }
                        "--kanji-font-size" => {
                            if let Some(px) = parse_css_length_px(&val, theme.headword_size) {
                                theme.kanji_font_size = px.clamp(16.0, 96.0);
                                overrides.kanji_font_size = true;
                            }
                        }
                        "--font-family" => {
                            let ff = clean_css_font_family(&val);
                            if !ff.is_empty() {
                                theme.font_family = ff;
                                overrides.font_family = true;
                            }
                        }
                        "--glossary-font-size" => {
                            if let Some(px) = parse_css_length_px(&val, 14.5) {
                                theme.glossary_font_size = px.clamp(9.0, 32.0);
                                overrides.glossary_font_size = true;
                            }
                        }
                        "--link-color" => {
                            if let Some(c) = parse_css_theme_color(&val) {
                                theme.link_color = c;
                            }
                        }
                        "--divider-color" => {
                            if let Some(c) = parse_css_theme_color(&val) {
                                theme.divider_color = c;
                                theme.term_divider_color = c;
                            }
                        }
                        "--term-divider-color" => {
                            if let Some(c) = parse_css_theme_color(&val) {
                                theme.term_divider_color = c;
                            }
                        }
                        "--freq-bg" => {
                            if let Some(c) = parse_css_theme_color(&val) {
                                theme.freq_bg = c;
                            }
                        }
                        "--freq-color" => {
                            if let Some(c) = parse_css_theme_color(&val) {
                                theme.freq_color = c;
                            }
                        }
                        "--freq-font-size" => {
                            if let Some(px) = parse_css_length_px(&val, 10.0) {
                                theme.freq_font_size = px.clamp(7.0, 18.0);
                            }
                        }
                        "--pitch-color" => {
                            if let Some(c) = parse_css_theme_color(&val) {
                                theme.pitch_color = c;
                            }
                        }
                        "--pitch-dict-bg" => {
                            if let Some(c) = parse_css_theme_color(&val) {
                                theme.pitch_dict_bg = c;
                            }
                        }
                        "--dict-badge-bg" => {
                            if let Some(c) = parse_css_theme_color(&val) {
                                theme.dict_badge_bg = c;
                            }
                        }
                        "--dict-badge-color" => {
                            if let Some(c) = parse_css_theme_color(&val) {
                                theme.dict_badge_color = c;
                            }
                        }
                        "--anki-tag-bg" => {
                            if let Some(c) = parse_css_theme_color(&val) {
                                theme.anki_tag_bg = c;
                            }
                        }
                        "--anki-tag-border" => {
                            if let Some(c) = parse_css_theme_color(&val) {
                                theme.anki_tag_border = c;
                            }
                        }
                        "--anki-tag-color" => {
                            if let Some(c) = parse_css_theme_color(&val) {
                                theme.anki_tag_color = c;
                            }
                        }
                        "--definition-columns" => {
                            if let Ok(cols) = val.trim().parse::<i32>() {
                                theme.definition_columns = cols.clamp(1, 2);
                            }
                        }
                        "--definition-columns-min-width" => {
                            if let Some(px) = parse_css_length_px(&val, theme.glossary_font_size) {
                                theme.definition_columns_min_width = px.clamp(320.0, 1400.0);
                            }
                        }
                        "--definition-gap" => {
                            if let Some(px) = parse_css_length_px(&val, theme.glossary_font_size) {
                                theme.definition_gap = px.clamp(0.0, 32.0);
                            }
                        }
                        "--collapsible-definition-line-count" => {
                            if let Some(lines) = parse_css_collapsible_lines(&val) {
                                theme.collapsible_definition_line_count = lines;
                            }
                        }
                        "--card-bg" => {
                            if let Some(c) = parse_css_theme_color(&val) {
                                theme.card_bg = c;
                            }
                        }
                        "--card-border" => {
                            if let Some(c) = parse_css_theme_color(&val) {
                                theme.card_border = c;
                            }
                        }
                        "--card-border-width" => {
                            if let Some(px) = parse_css_length_px(&val, theme.glossary_font_size) {
                                theme.card_border_width = px.clamp(0.0, 8.0);
                            }
                        }
                        "--card-radius" => {
                            if let Some(px) = parse_css_length_px(&val, theme.glossary_font_size) {
                                theme.card_radius = px.clamp(0.0, 24.0);
                            }
                        }
                        "--card-padding" => {
                            if let Some(px) = parse_css_length_px(&val, theme.glossary_font_size) {
                                theme.card_padding = px.clamp(0.0, 32.0);
                            }
                        }
                        "--dict-color-opacity" => {
                            if let Some(op) = parse_css_unit_float(&val) {
                                theme.dict_color_opacity = op;
                            }
                        }
                        "--derivations-compact" => {
                            theme.compact_derivations =
                                matches!(val.trim().to_ascii_lowercase().as_str(), "true" | "1" | "yes");
                        }
                        "--pitch-heiban-color" => {
                            theme.pitch_heiban_color = parse_css_theme_color(&val);
                        }
                        "--pitch-atamadaka-color" => {
                            theme.pitch_atamadaka_color = parse_css_theme_color(&val);
                        }
                        "--pitch-nakadaka-color" => {
                            theme.pitch_nakadaka_color = parse_css_theme_color(&val);
                        }
                        "--pitch-odaka-color" => {
                            theme.pitch_odaka_color = parse_css_theme_color(&val);
                        }
                        "--pitch-kifuku-color" => {
                            theme.pitch_kifuku_color = parse_css_theme_color(&val);
                        }
                        _ => {}
                    }
                }
                continue;
            }

            // 2. Outer container selectors (`.popup`, `.yomitan-popup`, `iframe.yomitan-popup`, `body`)
            if matches!(
                lower.as_str(),
                ".popup" | ".yomitan-popup" | "iframe.yomitan-popup" | "body"
            ) {
                for (prop, raw_val) in decls {
                    let val = resolve_css_var_refs(raw_val, vars, 0);
                    match prop.as_str() {
                        "background" | "background-color" => {
                            if let Some(c) = parse_css_theme_color(&val) {
                                // Ignore inner `body { background: transparent; }` when outer CSS is separate
                                if is_outer || lower != "body" || c[3] > 0 {
                                    theme.popup_bg = c;
                                }
                            }
                        }
                        "border" => {
                            let (w, c) = parse_css_border_shorthand(&val, theme.glossary_font_size);
                            if let Some(px) = w {
                                theme.popup_border_width = px.clamp(0.0, 12.0);
                            }
                            if let Some(col) = c {
                                theme.popup_border = col;
                            }
                        }
                        "border-color" => {
                            if let Some(c) = parse_css_theme_color(&val) {
                                theme.popup_border = c;
                            }
                        }
                        "border-width" => {
                            if let Some(px) = parse_css_length_px(&val, theme.glossary_font_size) {
                                theme.popup_border_width = px.clamp(0.0, 12.0);
                            }
                        }
                        "border-radius" => {
                            if let Some(px) = parse_css_length_px(&val, theme.glossary_font_size) {
                                theme.popup_radius = px.clamp(0.0, 32.0);
                            }
                        }
                        "box-shadow" => {
                            let (blur, col) = parse_css_box_shadow(&val, theme.glossary_font_size);
                            if let Some(b) = blur {
                                theme.popup_shadow_blur = b.clamp(0.0, 64.0);
                            }
                            if let Some(c) = col {
                                theme.popup_shadow_color = c;
                            }
                        }
                        "padding" => {
                            if let Some(px) = parse_css_length_px(&val, theme.glossary_font_size) {
                                theme.popup_padding = px.clamp(0.0, 40.0);
                            }
                        }
                        "width" | "max-width" => {
                            if let Some(px) = parse_css_length_px(&val, theme.glossary_font_size) {
                                theme.popup_width = if px == 0.0 { 0.0 } else { px.clamp(280.0, 1600.0) };
                            }
                        }
                        "height" | "max-height" => {
                            if let Some(px) = parse_css_length_px(&val, theme.glossary_font_size) {
                                theme.popup_height = if px == 0.0 { 0.0 } else { px.clamp(200.0, 1200.0) };
                            }
                        }
                        "color" => {
                            if let Some(c) = parse_css_theme_color(&val) {
                                theme.text_color = c;
                            }
                        }
                        "font-family" => {
                            let ff = clean_css_font_family(&val);
                            if !ff.is_empty() {
                                theme.font_family = ff;
                                overrides.font_family = true;
                            }
                        }
                        "font-size" => {
                            if let Some(px) = parse_css_length_px(&val, 14.5) {
                                theme.glossary_font_size = px.clamp(9.0, 32.0);
                                overrides.glossary_font_size = true;
                            }
                        }
                        _ => {}
                    }
                }
                continue;
            }

            // 3. Per-pitch-category headword colors (`.headword[data-pitch="heiban"]` etc.)
            if lower.starts_with(".headword[") {
                if let Some(m) = parse_attr_matcher(sel, "data-pitch")
                    .or_else(|| parse_attr_matcher(sel, "data-pronunciation-categories"))
                {
                    for (prop, raw_val) in decls {
                        if matches!(prop.as_str(), "color" | "--headword-text-color") {
                            let val = resolve_css_var_refs(raw_val, vars, 0);
                            if let Some(c) = parse_css_theme_color(&val) {
                                match m.value.to_ascii_lowercase().as_str() {
                                    "heiban" => theme.pitch_heiban_color = Some(c),
                                    "atamadaka" => theme.pitch_atamadaka_color = Some(c),
                                    "nakadaka" => theme.pitch_nakadaka_color = Some(c),
                                    "odaka" => theme.pitch_odaka_color = Some(c),
                                    "kifuku" => theme.pitch_kifuku_color = Some(c),
                                    _ => {}
                                }
                            }
                        }
                    }
                }
                continue;
            }

            // 4. Per-dictionary card/badge/opacity rules (`[data-dictionary="..."]`, `.definition-item[data-dictionary^="..."]`)
            if lower.contains("[data-dictionary") && !lower.contains(' ') {
                if let Some(matcher) = parse_attr_matcher(sel, "data-dictionary") {
                    let mut rule = DictStyleRule {
                        matcher,
                        badge_bg: None,
                        badge_color: None,
                        bg_opacity: None,
                        card_bg: None,
                        card_border: None,
                        glossary_opacity: None,
                        glossary_hover_opacity: None,
                        glossary_solo_opacity: None,
                        collapsible_line_count: None,
                    };
                    let mut has_any = false;
                    for (prop, raw_val) in decls {
                        let val = resolve_css_var_refs(raw_val, vars, 0);
                        match prop.as_str() {
                            "--dict-color" | "--dict-badge-bg" => {
                                if let Some(c) = parse_css_theme_color(&val) {
                                    rule.badge_bg = Some(c);
                                    has_any = true;
                                }
                            }
                            "--dict-text-color" | "--tag-text-color" | "--dict-badge-color" => {
                                if let Some(c) = parse_css_theme_color(&val) {
                                    rule.badge_color = Some(c);
                                    has_any = true;
                                }
                            }
                            "--dict-bg-opacity" => {
                                if let Some(op) = parse_css_unit_float(&val) {
                                    rule.bg_opacity = Some(op);
                                    has_any = true;
                                }
                            }
                            "--card-bg" => {
                                if let Some(c) = parse_css_theme_color(&val) {
                                    rule.card_bg = Some(c);
                                    has_any = true;
                                }
                            }
                            "--card-border" => {
                                if let Some(c) = parse_css_theme_color(&val) {
                                    rule.card_border = Some(c);
                                    has_any = true;
                                }
                            }
                            "--glossary-opacity" => {
                                if let Some(op) = parse_css_unit_float(&val) {
                                    rule.glossary_opacity = Some(op);
                                    has_any = true;
                                }
                            }
                            "--glossary-hover-opacity" => {
                                if let Some(op) = parse_css_unit_float(&val) {
                                    rule.glossary_hover_opacity = Some(op);
                                    has_any = true;
                                }
                            }
                            "--glossary-solo-opacity" => {
                                if let Some(op) = parse_css_unit_float(&val) {
                                    rule.glossary_solo_opacity = Some(op);
                                    has_any = true;
                                }
                            }
                            "--collapsible-definition-line-count" => {
                                if let Some(lines) = parse_css_collapsible_lines(&val) {
                                    rule.collapsible_line_count = Some(lines);
                                    has_any = true;
                                }
                            }
                            _ => {}
                        }
                    }
                    if has_any {
                        theme.dict_rules.push(rule);
                    }
                }
                continue;
            }

            // 5. Per-frequency-dictionary pill rules (`[data-frequency^="JPDB"]`, `.frequency-group-item[data-details^="JPDB"]`)
            if (lower.contains("[data-frequency")
                || (lower.starts_with(".frequency") && lower.contains("[data-details")))
                && !lower.contains(' ')
            {
                if let Some(matcher) = parse_attr_matcher(sel, "data-frequency")
                    .or_else(|| parse_attr_matcher(sel, "data-details"))
                {
                    let mut rule = FreqStyleRule {
                        matcher,
                        bg: None,
                        color: None,
                    };
                    for (prop, raw_val) in decls {
                        let val = resolve_css_var_refs(raw_val, vars, 0);
                        match prop.as_str() {
                            "background"
                            | "background-color"
                            | "--tag-frequency-background-color"
                            | "--freq-bg" => {
                                if let Some(c) = parse_css_theme_color(&val) {
                                    rule.bg = Some(c);
                                }
                            }
                            "color" | "--freq-color" => {
                                if let Some(c) = parse_css_theme_color(&val) {
                                    rule.color = Some(c);
                                }
                            }
                            _ => {}
                        }
                    }
                    if rule.bg.is_some() || rule.color.is_some() {
                        theme.freq_rules.push(rule);
                    }
                }
                continue;
            }

            // 6. Per-pitch-dictionary badge rules (`[data-pitch-dict^="NHK"]`)
            if lower.contains("[data-pitch-dict") && !lower.contains(' ') {
                if let Some(matcher) = parse_attr_matcher(sel, "data-pitch-dict") {
                    for (prop, raw_val) in decls {
                        if matches!(prop.as_str(), "background" | "background-color" | "--tag-color") {
                            let val = resolve_css_var_refs(raw_val, vars, 0);
                            if let Some(c) = parse_css_theme_color(&val) {
                                theme.pitch_dict_rules.push(PitchDictStyleRule {
                                    matcher: matcher.clone(),
                                    bg: c,
                                });
                            }
                        }
                    }
                }
                continue;
            }

            // 7. Standard named element & layout class selectors
            match lower.as_str() {
                ".definition-list" => {
                    for (prop, raw_val) in decls {
                        let val = resolve_css_var_refs(raw_val, vars, 0);
                        match prop.as_str() {
                            "columns" | "column-count" => {
                                if let Ok(cols) = val.trim().parse::<i32>() {
                                    theme.definition_columns = cols.clamp(1, 2);
                                }
                            }
                            "gap" => {
                                if let Some(px) = parse_css_length_px(&val, theme.glossary_font_size) {
                                    theme.definition_gap = px.clamp(0.0, 32.0);
                                }
                            }
                            _ => {}
                        }
                    }
                }
                ".definition-card" | ".definition-item" => {
                    for (prop, raw_val) in decls {
                        let val = resolve_css_var_refs(raw_val, vars, 0);
                        match prop.as_str() {
                            "background" | "background-color" => {
                                if let Some(c) = parse_css_theme_color(&val) {
                                    theme.card_bg = c;
                                }
                            }
                            "border" => {
                                let (w, c) = parse_css_border_shorthand(&val, theme.glossary_font_size);
                                if let Some(px) = w {
                                    theme.card_border_width = px.clamp(0.0, 8.0);
                                }
                                if let Some(col) = c {
                                    theme.card_border = col;
                                }
                            }
                            "border-color" => {
                                if let Some(c) = parse_css_theme_color(&val) {
                                    theme.card_border = c;
                                }
                            }
                            "border-width" => {
                                if let Some(px) = parse_css_length_px(&val, theme.glossary_font_size) {
                                    theme.card_border_width = px.clamp(0.0, 8.0);
                                }
                            }
                            "border-radius" => {
                                if let Some(px) = parse_css_length_px(&val, theme.glossary_font_size) {
                                    theme.card_radius = px.clamp(0.0, 24.0);
                                }
                            }
                            "padding" => {
                                if let Some(px) = parse_css_length_px(&val, theme.glossary_font_size) {
                                    theme.card_padding = px.clamp(0.0, 32.0);
                                }
                            }
                            _ => {}
                        }
                    }
                }
                ".term-tags" | ".headword-list-tag-list" => {
                    for (prop, raw_val) in decls {
                        if prop == "display" {
                            theme.show_term_tags = !raw_val.trim().eq_ignore_ascii_case("none");
                        }
                    }
                }
                ".kanji-extra" => {
                    for (prop, raw_val) in decls {
                        if prop == "display" {
                            theme.show_kanji_extra = !raw_val.trim().eq_ignore_ascii_case("none");
                        }
                    }
                }
                ".derivations" | ".inflection-rule-chains" => {
                    for (prop, raw_val) in decls {
                        let val = resolve_css_var_refs(raw_val, vars, 0);
                        match prop.as_str() {
                            "display" => {
                                let v = val.trim().to_ascii_lowercase();
                                if v == "compact" || v == "tooltip" {
                                    theme.compact_derivations = true;
                                } else if v == "flex" || v == "block" {
                                    theme.compact_derivations = false;
                                }
                            }
                            "font-family" => {
                                let ff = clean_css_font_family(&val);
                                if !ff.is_empty() {
                                    theme.conj_font_family = ff;
                                    overrides.conj_font_family = true;
                                }
                            }
                            "font-size" => {
                                if let Some(px) = parse_css_length_px(&val, 11.0) {
                                    theme.conj_font_size = px.clamp(8.0, 24.0);
                                    overrides.conj_font_size = true;
                                }
                            }
                            _ => {}
                        }
                    }
                }
                ".headword" | ".headword-term" => {
                    for (prop, raw_val) in decls {
                        let val = resolve_css_var_refs(raw_val, vars, 0);
                        match prop.as_str() {
                            "color" => {
                                if let Some(c) = parse_css_theme_color(&val) {
                                    theme.headword_color = c;
                                }
                            }
                            "font-family" => {
                                let ff = clean_css_font_family(&val);
                                if !ff.is_empty() {
                                    theme.headword_font_family = ff;
                                    overrides.headword_font_family = true;
                                }
                            }
                            "font-size" => {
                                if let Some(px) = parse_css_length_px(&val, theme.glossary_font_size) {
                                    theme.headword_size = px.clamp(12.0, 48.0);
                                    overrides.headword_size = true;
                                }
                            }
                            _ => {}
                        }
                    }
                }
                ".kanji-headword" => {
                    for (prop, raw_val) in decls {
                        let val = resolve_css_var_refs(raw_val, vars, 0);
                        match prop.as_str() {
                            "font-family" => {
                                let ff = clean_css_font_family(&val);
                                if !ff.is_empty() {
                                    theme.kanji_font_family = ff;
                                    overrides.kanji_font_family = true;
                                }
                            }
                            "font-size" => {
                                if let Some(px) = parse_css_length_px(&val, theme.headword_size) {
                                    theme.kanji_font_size = px.clamp(16.0, 96.0);
                                    overrides.kanji_font_size = true;
                                }
                            }
                            _ => {}
                        }
                    }
                }
                ".reading" | ".headword-reading" => {
                    for (prop, raw_val) in decls {
                        let val = resolve_css_var_refs(raw_val, vars, 0);
                        match prop.as_str() {
                            "color" => {
                                if let Some(c) = parse_css_theme_color(&val) {
                                    theme.reading_color = c;
                                }
                            }
                            "font-family" => {
                                let ff = clean_css_font_family(&val);
                                if !ff.is_empty() {
                                    theme.reading_font_family = ff;
                                    overrides.reading_font_family = true;
                                }
                            }
                            "font-size" => {
                                if let Some(px) = parse_css_length_px(&val, theme.glossary_font_size) {
                                    theme.reading_size = px.clamp(8.0, 28.0);
                                    overrides.reading_size = true;
                                }
                            }
                            _ => {}
                        }
                    }
                }
                ".glossary" | ".gloss-content" => {
                    for (prop, raw_val) in decls {
                        let val = resolve_css_var_refs(raw_val, vars, 0);
                        match prop.as_str() {
                            "color" => {
                                if let Some(c) = parse_css_theme_color(&val) {
                                    theme.text_color = c;
                                }
                            }
                            "font-size" => {
                                if let Some(px) = parse_css_length_px(&val, 14.5) {
                                    theme.glossary_font_size = px.clamp(9.0, 32.0);
                                    overrides.glossary_font_size = true;
                                }
                            }
                            "font-family" => {
                                let ff = clean_css_font_family(&val);
                                if !ff.is_empty() {
                                    theme.font_family = ff;
                                    overrides.font_family = true;
                                }
                            }
                            _ => {}
                        }
                    }
                }
                "a" => {
                    for (prop, raw_val) in decls {
                        if prop == "color" {
                            let val = resolve_css_var_refs(raw_val, vars, 0);
                            if let Some(c) = parse_css_theme_color(&val) {
                                theme.link_color = c;
                            }
                        }
                    }
                }
                ".divider" | "hr" => {
                    for (prop, raw_val) in decls {
                        if matches!(
                            prop.as_str(),
                            "background" | "background-color" | "color" | "border-color"
                        ) {
                            let val = resolve_css_var_refs(raw_val, vars, 0);
                            if let Some(c) = parse_css_theme_color(&val) {
                                theme.divider_color = c;
                                theme.term_divider_color = c;
                            }
                        }
                    }
                }
                ".frequency" | ".frequency-badge" | ".frequency-group-item" => {
                    for (prop, raw_val) in decls {
                        let val = resolve_css_var_refs(raw_val, vars, 0);
                        match prop.as_str() {
                            "background" | "background-color" => {
                                if let Some(c) = parse_css_theme_color(&val) {
                                    theme.freq_bg = c;
                                }
                            }
                            "color" => {
                                if let Some(c) = parse_css_theme_color(&val) {
                                    theme.freq_color = c;
                                }
                            }
                            "font-size" => {
                                if let Some(px) = parse_css_length_px(&val, 10.0) {
                                    theme.freq_font_size = px.clamp(7.0, 18.0);
                                }
                            }
                            _ => {}
                        }
                    }
                }
                ".pitch" | ".pronunciation" => {
                    for (prop, raw_val) in decls {
                        if matches!(
                            prop.as_str(),
                            "color" | "border-color" | "background" | "background-color"
                        ) {
                            let val = resolve_css_var_refs(raw_val, vars, 0);
                            if let Some(c) = parse_css_theme_color(&val) {
                                theme.pitch_color = c;
                            }
                        }
                    }
                }
                ".pitch-dict" | ".pronunciation-group-tag-list" => {
                    for (prop, raw_val) in decls {
                        let val = resolve_css_var_refs(raw_val, vars, 0);
                        if prop == "display" {
                            theme.show_pitch_dict = !val.trim().eq_ignore_ascii_case("none");
                        } else if matches!(prop.as_str(), "background" | "background-color") {
                            if let Some(c) = parse_css_theme_color(&val) {
                                theme.pitch_dict_bg = c;
                            }
                        }
                    }
                }
                ".dictionary-badge" | ".tag-dictionary" => {
                    for (prop, raw_val) in decls {
                        let val = resolve_css_var_refs(raw_val, vars, 0);
                        match prop.as_str() {
                            "background" | "background-color" => {
                                if let Some(c) = parse_css_theme_color(&val) {
                                    theme.dict_badge_bg = c;
                                }
                            }
                            "color" => {
                                if let Some(c) = parse_css_theme_color(&val) {
                                    theme.dict_badge_color = c;
                                }
                            }
                            "font-family" => {
                                let ff = clean_css_font_family(&val);
                                if !ff.is_empty() {
                                    theme.tag_font_family = ff;
                                    overrides.tag_font_family = true;
                                }
                            }
                            "font-size" => {
                                if let Some(px) = parse_css_length_px(&val, 11.0) {
                                    theme.tag_font_size = px.clamp(8.0, 22.0);
                                    overrides.tag_font_size = true;
                                }
                            }
                            _ => {}
                        }
                    }
                }
                ".anki-tag" => {
                    for (prop, raw_val) in decls {
                        let val = resolve_css_var_refs(raw_val, vars, 0);
                        match prop.as_str() {
                            "background" | "background-color" => {
                                if let Some(c) = parse_css_theme_color(&val) {
                                    theme.anki_tag_bg = c;
                                }
                            }
                            "border" => {
                                let (_, c) = parse_css_border_shorthand(&val, theme.glossary_font_size);
                                if let Some(col) = c {
                                    theme.anki_tag_border = col;
                                }
                            }
                            "border-color" => {
                                if let Some(c) = parse_css_theme_color(&val) {
                                    theme.anki_tag_border = c;
                                }
                            }
                            "color" => {
                                if let Some(c) = parse_css_theme_color(&val) {
                                    theme.anki_tag_color = c;
                                }
                            }
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
        }
    }
}

fn collect_root_vars(rules: &[(String, Vec<(String, String)>)], vars: &mut HashMap<String, String>) {
    for (selector_list, decls) in rules {
        let applies_to_root = selector_list.split(',').any(|sel| {
            matches!(
                sel.trim().to_ascii_lowercase().as_str(),
                ":root"
                    | "html"
                    | "body"
                    | ".popup"
                    | ".yomitan-popup"
                    | "iframe.yomitan-popup"
                    | ":root[data-theme=\"dark\"]"
                    | ":root[data-theme='dark']"
            )
        });
        if applies_to_root {
            for (prop, val) in decls {
                if prop.starts_with("--") {
                    vars.insert(prop.clone(), val.clone());
                }
            }
        }
    }
}

fn parse_popup_css_theme_full(
    inner_css: &str,
    outer_css: &str,
) -> (
    PopupCssTheme,
    PopupCssExplicitOverrides,
    HashMap<String, String>,
    Vec<(String, Vec<(String, String)>)>,
) {
    let mut theme = PopupCssTheme::default();
    let mut overrides = PopupCssExplicitOverrides::default();
    let mut vars: HashMap<String, String> = HashMap::new();

    let inner_stripped = strip_css_comments(inner_css);
    let mut inner_rules = Vec::new();
    parse_css_rules_flattened(&inner_stripped, "", &mut inner_rules, 0);

    let outer_stripped = strip_css_comments(outer_css);
    let mut outer_rules = Vec::new();
    parse_css_rules_flattened(&outer_stripped, "", &mut outer_rules, 0);

    collect_root_vars(&inner_rules, &mut vars);
    collect_root_vars(&outer_rules, &mut vars);

    apply_theme_rules(&inner_rules, &vars, &mut theme, &mut overrides, false);
    apply_theme_rules(&outer_rules, &vars, &mut theme, &mut overrides, true);

    (theme, overrides, vars, inner_rules)
}

pub fn parse_popup_css_theme(css: &str) -> PopupCssTheme {
    parse_popup_css_theme_full(css, "").0
}

pub fn parse_split_popup_css_theme(inner_css: &str, outer_css: &str) -> PopupCssTheme {
    parse_popup_css_theme_full(inner_css, outer_css).0
}

fn preprocess_user_glossary_css(
    inner_rules: &[(String, Vec<(String, String)>)],
    vars: &HashMap<String, String>,
) -> String {
    let mut out = String::new();
    for (selector_list, decls) in inner_rules {
        // Normalize Yomitan dictionary container selectors onto our virtual `<memento-dict>` wrapper
        let norm_sel = selector_list
            .replace("li.definition-item[data-dictionary", "[data-dictionary")
            .replace(".definition-item[data-dictionary", "[data-dictionary")
            .replace("li[data-dictionary", "[data-dictionary")
            .replace("LI[data-dictionary", "[data-dictionary")
            .replace("div[data-dictionary", "[data-dictionary")
            .replace("DIV[data-dictionary", "[data-dictionary");
        if norm_sel.trim().is_empty() {
            continue;
        }
        let mut decl_str = String::new();
        for (prop, raw_val) in decls {
            if prop.starts_with("--") {
                continue;
            }
            let val = resolve_css_var_refs(raw_val, vars, 0);
            decl_str.push_str(prop);
            decl_str.push_str(": ");
            decl_str.push_str(&val);
            decl_str.push_str("; ");
        }
        if !decl_str.is_empty() {
            out.push_str(&norm_sel);
            out.push_str(" { ");
            out.push_str(&decl_str);
            out.push_str("}\n");
        }
    }
    out
}

fn rgba_to_css_color(rgba: [u8; 4]) -> String {
    if rgba[3] == 255 {
        format!("#{:02X}{:02X}{:02X}", rgba[0], rgba[1], rgba[2])
    } else {
        let a = f32::from(rgba[3]) / 255.0;
        format!("rgba({}, {}, {}, {:.3})", rgba[0], rgba[1], rgba[2], a)
    }
}

fn rgba_to_slint_color(rgba: [u8; 4]) -> slint::Color {
    slint::Color::from_argb_u8(rgba[3], rgba[0], rgba[1], rgba[2])
}

fn apply_popup_css_theme_to_bridge(app: &MementoApp, theme: &PopupCssTheme) {
    let pb = app.global::<PopupBridge>();
    pb.set_css_popup_bg(rgba_to_slint_color(theme.popup_bg));
    pb.set_css_popup_border(rgba_to_slint_color(theme.popup_border));
    pb.set_css_popup_border_width(theme.popup_border_width);
    pb.set_css_popup_radius(theme.popup_radius);
    pb.set_css_popup_padding(theme.popup_padding);
    pb.set_css_popup_shadow_blur(theme.popup_shadow_blur);
    pb.set_css_popup_shadow_color(rgba_to_slint_color(theme.popup_shadow_color));
    pb.set_css_popup_width(theme.popup_width);
    pb.set_css_popup_height(theme.popup_height);
    pb.set_css_text_color(rgba_to_slint_color(theme.text_color));
    pb.set_css_muted_color(rgba_to_slint_color(theme.muted_color));
    pb.set_css_headword_color(rgba_to_slint_color(theme.headword_color));
    pb.set_css_headword_size(theme.headword_size);
    pb.set_css_reading_color(rgba_to_slint_color(theme.reading_color));
    pb.set_css_reading_size(theme.reading_size);
    pb.set_css_font_family(theme.font_family.clone().into());
    pb.set_css_headword_font_family(
        if theme.headword_font_family.is_empty() {
            theme.font_family.as_str()
        } else {
            theme.headword_font_family.as_str()
        }
        .into(),
    );
    pb.set_css_reading_font_family(
        if theme.reading_font_family.is_empty() {
            theme.font_family.as_str()
        } else {
            theme.reading_font_family.as_str()
        }
        .into(),
    );
    pb.set_css_conj_font_family(
        if theme.conj_font_family.is_empty() {
            theme.font_family.as_str()
        } else {
            theme.conj_font_family.as_str()
        }
        .into(),
    );
    pb.set_css_conj_font_size(theme.conj_font_size);
    pb.set_css_tag_font_family(
        if theme.tag_font_family.is_empty() {
            theme.font_family.as_str()
        } else {
            theme.tag_font_family.as_str()
        }
        .into(),
    );
    pb.set_css_tag_font_size(theme.tag_font_size);
    pb.set_css_kanji_font_family(
        if theme.kanji_font_family.is_empty() {
            theme.font_family.as_str()
        } else {
            theme.kanji_font_family.as_str()
        }
        .into(),
    );
    pb.set_css_kanji_font_size(theme.kanji_font_size);
    pb.set_css_glossary_font_size(theme.glossary_font_size);
    pb.set_css_link_color(rgba_to_slint_color(theme.link_color));
    pb.set_css_divider_color(rgba_to_slint_color(theme.divider_color));
    pb.set_css_term_divider_color(rgba_to_slint_color(theme.term_divider_color));
    pb.set_css_freq_bg(rgba_to_slint_color(theme.freq_bg));
    pb.set_css_freq_color(rgba_to_slint_color(theme.freq_color));
    pb.set_css_freq_font_size(theme.freq_font_size);
    pb.set_css_pitch_color(rgba_to_slint_color(theme.pitch_color));
    pb.set_css_pitch_dict_bg(rgba_to_slint_color(theme.pitch_dict_bg));
    pb.set_css_dict_badge_bg(rgba_to_slint_color(theme.dict_badge_bg));
    pb.set_css_dict_badge_color(rgba_to_slint_color(theme.dict_badge_color));
    pb.set_css_anki_tag_bg(rgba_to_slint_color(theme.anki_tag_bg));
    pb.set_css_anki_tag_border(rgba_to_slint_color(theme.anki_tag_border));
    pb.set_css_anki_tag_color(rgba_to_slint_color(theme.anki_tag_color));
    pb.set_css_definition_columns(theme.definition_columns);
    pb.set_css_definition_columns_min_width(theme.definition_columns_min_width);
    pb.set_css_definition_gap(theme.definition_gap);
    pb.set_css_show_term_tags(theme.show_term_tags);
    pb.set_css_show_pitch_dict(theme.show_pitch_dict);
    pb.set_css_show_kanji_extra(theme.show_kanji_extra);
    pb.set_css_compact_derivations(theme.compact_derivations);
}

#[derive(Default)]
struct StylesheetCache {
    user_css_path: PathBuf,
    user_outer_css_path: PathBuf,
    user_css_sig: Option<(Option<SystemTime>, u64)>,
    user_outer_css_sig: Option<(Option<SystemTime>, u64)>,
    user_css_raw: String,
    user_outer_css_raw: String,
    user_css_glossary_preprocessed: String,
    user_css_theme: PopupCssTheme,
    user_css_overrides: PopupCssExplicitOverrides,
    user_css_revision: u64,
    user_only_sheet: Option<Arc<ParsedStylesheet>>,
    entries: HashMap<PathBuf, (Option<SystemTime>, u64, Arc<ParsedStylesheet>)>,
}

impl StylesheetCache {
    fn new(config_dir: &Path) -> Self {
        let mut cache = Self {
            user_css_path: config_dir.join(CUSTOM_POPUP_CSS_FILENAME),
            user_outer_css_path: config_dir.join(CUSTOM_POPUP_OUTER_CSS_FILENAME),
            ..Self::default()
        };
        cache.force_reload_user_css();
        cache
    }

    fn theme(&self) -> &PopupCssTheme {
        &self.user_css_theme
    }

    fn overrides(&self) -> &PopupCssExplicitOverrides {
        &self.user_css_overrides
    }

    fn read_file_sig(path: &Path) -> Option<(Option<SystemTime>, u64)> {
        if path.as_os_str().is_empty() {
            return None;
        }
        let meta = std::fs::metadata(path).ok()?;
        Some((meta.modified().ok(), meta.len()))
    }

    fn reload_user_css_if_changed(&mut self) -> bool {
        if self.user_css_path.as_os_str().is_empty() && self.user_outer_css_path.as_os_str().is_empty() {
            return false;
        }
        let inner_sig = Self::read_file_sig(&self.user_css_path);
        let outer_sig = Self::read_file_sig(&self.user_outer_css_path);
        if inner_sig == self.user_css_sig && outer_sig == self.user_outer_css_sig {
            return false;
        }
        self.apply_reloaded_user_css(inner_sig, outer_sig)
    }

    fn force_reload_user_css(&mut self) -> bool {
        if self.user_css_path.as_os_str().is_empty() && self.user_outer_css_path.as_os_str().is_empty() {
            return false;
        }
        let inner_sig = Self::read_file_sig(&self.user_css_path);
        let outer_sig = Self::read_file_sig(&self.user_outer_css_path);
        self.apply_reloaded_user_css(inner_sig, outer_sig)
    }

    fn apply_reloaded_user_css(
        &mut self,
        inner_sig: Option<(Option<SystemTime>, u64)>,
        outer_sig: Option<(Option<SystemTime>, u64)>,
    ) -> bool {
        let inner_raw = if inner_sig.is_some() {
            std::fs::read_to_string(&self.user_css_path).unwrap_or_default()
        } else {
            String::new()
        };
        let outer_raw = if outer_sig.is_some() {
            std::fs::read_to_string(&self.user_outer_css_path).unwrap_or_default()
        } else {
            String::new()
        };
        let changed = inner_sig != self.user_css_sig
            || outer_sig != self.user_outer_css_sig
            || inner_raw != self.user_css_raw
            || outer_raw != self.user_outer_css_raw;
        self.user_css_sig = inner_sig;
        self.user_outer_css_sig = outer_sig;
        if !changed && self.user_css_revision > 0 {
            return false;
        }
        self.user_css_raw = inner_raw;
        self.user_outer_css_raw = outer_raw;
        if self.user_css_raw.trim().is_empty() && self.user_outer_css_raw.trim().is_empty() {
            self.user_css_theme = PopupCssTheme::default();
            self.user_css_overrides = PopupCssExplicitOverrides::default();
            self.user_css_glossary_preprocessed.clear();
            self.user_only_sheet = None;
        } else {
            let (theme, overrides, vars, inner_rules) =
                parse_popup_css_theme_full(&self.user_css_raw, &self.user_outer_css_raw);
            let preprocessed = preprocess_user_glossary_css(&inner_rules, &vars);
            self.user_css_theme = theme;
            self.user_css_overrides = overrides;
            self.user_only_sheet = (!preprocessed.trim().is_empty())
                .then(|| parse_dictionary_css_str(&preprocessed));
            self.user_css_glossary_preprocessed = preprocessed;
        }
        self.user_css_revision = self.user_css_revision.wrapping_add(1).max(1);
        self.entries.clear();
        true
    }

    fn get_for_dictionary(
        &mut self,
        dict_resource_dirs: &[PathBuf],
        dic_id: i64,
        dict_name: &str,
    ) -> Option<Arc<ParsedStylesheet>> {
        let style_path = dict_resource_dirs.iter().find_map(|dir| {
            memento_dict::databasemanager::resolve_dictionary_style_path(dir, dic_id, dict_name)
        });
        let Some(style_path) = style_path else {
            return self.user_only_sheet.clone();
        };
        let mtime = std::fs::metadata(&style_path)
            .ok()
            .and_then(|m| m.modified().ok());
        if let Some((cached_mtime, cached_user_rev, sheet)) = self.entries.get(&style_path) {
            if *cached_mtime == mtime && *cached_user_rev == self.user_css_revision {
                return Some(sheet.clone());
            }
        }
        let dict_css = match std::fs::read_to_string(&style_path) {
            Ok(s) => s,
            Err(_) => return self.user_only_sheet.clone(),
        };
        let combined = if self.user_css_glossary_preprocessed.trim().is_empty() {
            dict_css
        } else {
            format!("{dict_css}\n{}", self.user_css_glossary_preprocessed)
        };
        let parsed = parse_dictionary_css_str(&combined);
        self.entries
            .insert(style_path, (mtime, self.user_css_revision, parsed.clone()));
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
        opts.font_family = "sans-serif".to_owned();
        SHARED_TEXT_ENGINE.with(|engine| {
            let mut engine = engine.borrow_mut();
            engine.ensure_default_svg_fonts();
            opts.fontdb = engine.fontdb.clone();
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
    pub line_bottoms: Vec<f32>,
    pub plain_text: String,
}

pub const MAX_NESTED_POPUP_DEPTH: usize = 4;
const GLOSSARY_SCAN_MAX_CHARS: usize = 24;
const NESTED_DISMISS_GRACE_MS: u64 = 260;

fn is_japanese_scan_start_char(ch: char) -> bool {
    is_japanese_lookup_char(ch) && ch != '\u{30fb}' && ch != '\u{30fc}'
}

fn is_japanese_scan_continue_char(ch: char) -> bool {
    is_japanese_lookup_char(ch) && ch != '\u{30fb}'
}

/// Strict bounding-box hit-test over `RenderedGlossaryDoc::selectable_chars` for
/// hover-scanning inside glossary definitions (returns `None` on blank margins/empty space).
pub fn strict_char_index_at_point(doc: &RenderedGlossaryDoc, x: f32, y: f32) -> Option<usize> {
    if doc.selectable_chars.is_empty() {
        return None;
    }
    let mut best: Option<(usize, f32)> = None;
    for (idx, cell) in doc.selectable_chars.iter().enumerate() {
        if cell.ch == '\n' || cell.w <= 0.0 || cell.h <= 0.0 {
            continue;
        }
        let pad_x = 1.5f32;
        let pad_y = 1.5f32;
        if x < cell.x - pad_x
            || x > cell.x + cell.w + pad_x
            || y < cell.y - pad_y
            || y > cell.y + cell.h + pad_y
        {
            continue;
        }
        let cx = cell.x + cell.w * 0.5;
        let cy = cell.y + cell.h * 0.5;
        let dist = (x - cx).hypot(y - cy);
        if best.is_none_or(|(_, best_dist)| dist < best_dist) {
            best = Some((idx, dist));
        }
    }
    best.map(|(idx, _)| idx)
}

/// Extract a contiguous Japanese scan substring (up to `max_chars`) starting at `start_idx`
/// in `selectable_chars`, seamlessly bridging soft line wraps inside the same paragraph while
/// stopping at punctuation, whitespace, non-Japanese characters, or block breaks.
/// Returns `(substring, cell_indices)` where each char maps 1:1 back to its cell index.
pub fn extract_glossary_scan_substring(
    chars: &[GlossaryCharCell],
    start_idx: usize,
    max_chars: usize,
) -> Option<(String, Vec<usize>)> {
    let first = chars.get(start_idx)?;
    if !is_japanese_scan_start_char(first.ch) {
        return None;
    }
    let limit = max_chars.max(1);
    let mut query = String::new();
    let mut indices = Vec::new();
    let mut prev_cell = first;
    query.push(first.ch);
    indices.push(start_idx);

    let mut i = start_idx + 1;
    while i < chars.len() && indices.len() < limit {
        let cell = &chars[i];
        if cell.ch == '\n' {
            // Bridge a single soft visual wrap within the same paragraph when the next
            // visible character continues the Japanese run without a paragraph gap.
            if let Some(next_cell) = chars.get(i + 1) {
                let vertical_step = next_cell.y - prev_cell.y;
                if is_japanese_scan_continue_char(next_cell.ch)
                    && vertical_step >= 0.0
                    && vertical_step <= prev_cell.h.max(14.0) * 1.65
                {
                    i += 1;
                    continue;
                }
            }
            break;
        }
        if !is_japanese_scan_continue_char(cell.ch) {
            break;
        }
        query.push(cell.ch);
        indices.push(i);
        prev_cell = cell;
        i += 1;
    }

    (!query.is_empty()).then_some((query, indices))
}

fn utf16_units_to_char_count(text: &str, utf16_units: usize) -> usize {
    if utf16_units == 0 {
        return 0;
    }
    let mut acc = 0usize;
    let mut count = 0usize;
    for ch in text.chars() {
        acc += ch.len_utf16();
        count += 1;
        if acc >= utf16_units {
            break;
        }
    }
    count
}

fn build_selection_rects_for_cells(
    chars: &[GlossaryCharCell],
    indices: &[usize],
) -> Vec<RichSelectionRect> {
    let mut rects = Vec::new();
    let mut cur: Option<(usize, f32, f32, f32, f32)> = None;
    for &idx in indices {
        let Some(cell) = chars.get(idx) else {
            continue;
        };
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
                rects.push(RichSelectionRect {
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
        rects.push(RichSelectionRect {
            x_px: sx,
            y_px: sy,
            w_px: sw,
            h_px: sh,
        });
    }
    rects
}

fn compute_visual_line_bottoms(bounds: &[(f32, f32)]) -> Vec<f32> {
    if bounds.is_empty() {
        return Vec::new();
    }
    let mut sorted: Vec<(f32, f32)> = bounds
        .iter()
        .copied()
        .filter(|&(top, bottom)| top.is_finite() && bottom.is_finite() && bottom > top)
        .collect();
    sorted.sort_by(|a, b| {
        a.0.partial_cmp(&b.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
    });
    let mut merged: Vec<(f32, f32)> = Vec::new();
    for (top, bottom) in sorted {
        if let Some(last) = merged.last_mut() {
            // Merge horizontally adjacent table cells sharing the same visual row band.
            if (top - last.0).abs() <= 6.0 && top < last.1 - 3.0 {
                last.1 = last.1.max(bottom);
                continue;
            }
        }
        merged.push((top, bottom));
    }
    merged.into_iter().map(|(_, bottom)| bottom).collect()
}

/// Compute `(collapsible, collapsed_height_px, total_lines)` for a rendered glossary card
/// against its resolved `--collapsible-definition-line-count` limit (`0` = disabled).
pub fn compute_collapsible_metrics(
    doc: &RenderedGlossaryDoc,
    line_limit: usize,
) -> (bool, f32, i32) {
    let total_lines = doc.line_bottoms.len() as i32;
    if !doc.height_px.is_finite() || doc.height_px <= 0.0 {
        return (false, 0.0, total_lines);
    }
    if line_limit == 0 || doc.line_bottoms.len() <= line_limit {
        return (false, doc.height_px, total_lines);
    }
    let raw_bottom = doc.line_bottoms[line_limit - 1];
    if !raw_bottom.is_finite() {
        return (false, doc.height_px, total_lines);
    }
    let min_h = 18.0f32.min(doc.height_px);
    let clamped_h = (raw_bottom + 4.0).ceil().clamp(min_h, doc.height_px);
    if clamped_h + 4.0 >= doc.height_px {
        (false, doc.height_px, total_lines)
    } else {
        (true, clamped_h, total_lines)
    }
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
    default_link_color: [u8; 4],
    had_hidden_elements: bool,
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
        if let Some(ff) = state.declarations.get("font-family") {
            let cleaned = clean_css_font_family(ff);
            if !cleaned.is_empty() {
                next.font_family = cleaned;
            }
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
        if declarations
            .get("display")
            .is_some_and(|v| v.trim().eq_ignore_ascii_case("none"))
        {
            self.had_hidden_elements = true;
            self.ctx.elements.pop();
            return None;
        }

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

        // Check `filter: invert(...)` across matching stylesheet rules (e.g. dark-mode gaiji images)
        let mut invert_ratio = 0.0f32;
        if let Some(sheet) = self.ctx.dictionary_styles.clone() {
            for rule in &sheet.rules {
                if !self.renderer.css_rule_matches(rule, &self.ctx) {
                    continue;
                }
                for decl in &rule.declarations {
                    if decl.property.eq_ignore_ascii_case("filter")
                        || decl.property.eq_ignore_ascii_case("-webkit-filter")
                    {
                        if let Some(r) = parse_css_invert_ratio(&decl.value) {
                            invert_ratio = r;
                        }
                    }
                }
            }
        }

        self.ctx.elements.pop();

        let asset_path = resolve_dictionary_asset_path_in_dirs(
            self.dict_resource_dirs,
            self.dic_id,
            self.dict_name,
            rel_path,
        )?;
        let mut pixmap =
            load_dictionary_image_pixmap(&asset_path, req_w, req_h, tint, &color_str)?;
        if invert_ratio > 0.001 && !is_monochrome {
            pixmap = invert_pixmap(&pixmap, invert_ratio);
        }
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
                        let (mut state, hidden) = self.build_element_state_checked(node, &tag);
                        if hidden {
                            self.ctx.elements.pop();
                            return;
                        }
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
                        let (mut state, hidden) = self.build_element_state_checked(node, "tr");
                        if hidden {
                            self.ctx.elements.pop();
                            return;
                        }
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
                let (mut state, hidden) = self.build_element_state_checked(node, &tag);
                if hidden {
                    self.ctx.elements.pop();
                    return;
                }
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

    fn build_element_state_checked(
        &mut self,
        obj: &DefJson,
        tag: &str,
    ) -> (mdef::ElementRenderState, bool) {
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
        if state
            .declarations
            .get("display")
            .is_some_and(|v| v.trim().eq_ignore_ascii_case("none"))
        {
            self.had_hidden_elements = true;
            return (state, true);
        }
        self.renderer.resolve_element_layout(&mut self.ctx, &mut state);
        self.renderer.apply_element_compatibility(&self.ctx, &mut state);
        (state, false)
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
                        let (mut state, hidden) = self.build_element_state_checked(node, "ruby");
                        if hidden {
                            self.ctx.elements.pop();
                            return;
                        }
                        // Check if CSS hides `<rt>` inside this `<ruby>` (e.g. `.gloss-sc-rt { display: none; }`).
                        let rt_hidden = if self.ctx.dictionary_styles.is_some() {
                            let mut rt_el = mdef::StructuredElement::new();
                            rt_el.tag = "rt".to_owned();
                            rt_el.classes.insert("gloss-sc-rt".to_owned());
                            rt_el.attributes.insert("data-sc-rt".to_owned(), String::new());
                            let rt_idx = self.ctx.selector_elements.len() as i64;
                            self.ctx.selector_elements.push(rt_el);
                            self.ctx.elements.push(rt_idx);
                            let mut rt_decls = mdef::CssDeclarations::new();
                            self.renderer.add_matching_css_rules(
                                &mut self.ctx,
                                &mut rt_decls,
                                None,
                                None,
                            );
                            self.ctx.elements.pop();
                            rt_decls
                                .get("display")
                                .is_some_and(|v| v.trim().eq_ignore_ascii_case("none"))
                        } else {
                            false
                        };
                        let ruby_style = self.inline_style_from_state(&state, inherited);
                        for (base, reading) in extract_ruby_pairs(node.get("content")) {
                            if base.is_empty() {
                                continue;
                            }
                            if reading.is_empty() || rt_hidden {
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
                        let (state, hidden) = self.build_element_state_checked(node, "a");
                        if hidden {
                            self.ctx.elements.pop();
                            return;
                        }
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
                                link_style.color = self.default_link_color;
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
                        let idx = self.renderer.structured_element(node, &mut self.ctx, false);
                        self.ctx.elements.push(idx);
                        let (mut state, hidden) = self.build_element_state_checked(node, "table");
                        if hidden {
                            self.ctx.elements.pop();
                            return;
                        }
                        flush_inline(inline_out, blocks_out);
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
                        let (mut state, hidden) = self.build_element_state_checked(node, &tag);
                        if hidden {
                            self.ctx.elements.pop();
                            return;
                        }
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
    mask_glyphs: Vec<PositionedMaskGlyph>,
    hit_zones: Vec<GlossaryHitZone>,
    selectable_chars: Vec<GlossaryCharCell>,
    line_bounds: Vec<(f32, f32)>,
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
                    engine.collect_glossary_glyphs_from_layout(
                        &m_layout,
                        content_x,
                        content_y,
                        false,
                        false,
                        collector,
                    );
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
                    engine.collect_glossary_glyphs_from_layout(
                        &m_layout,
                        content_x,
                        content_y,
                        false,
                        false,
                        collector,
                    );
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
        RICH_EMOJI_FALLBACK_STACK,
    ))));
    builder.push_default(StyleProperty::FontSize(14.5));
    builder.push_default(StyleProperty::LineHeight(LineHeight::MetricsRelative(1.18)));
    builder.push_default(StyleProperty::Brush([241, 245, 249, 255]));

    for span in &spans {
        let ff = span.style.font_family.trim();
        if !ff.is_empty() && !ff.eq_ignore_ascii_case("Noto Sans CJK JP") {
            builder.push(
                StyleProperty::FontFamily(FontFamily::Source(Cow::Owned(format!(
                    "{ff}, {RICH_EMOJI_FALLBACK_STACK}"
                )))),
                span.byte_range.clone(),
            );
        }
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
        collector.line_bounds.push((line_top, line_top + line_h));
        let baseline = origin_y + line.metrics().baseline;
        let mut prev_run_key: Option<(usize, usize, u64, u32)> = None;
        let mut run_glyph_cursor: usize = 0;

        for item in line.items() {
            match item {
                PositionedLayoutItem::GlyphRun(glyph_run) => {
                    let run = glyph_run.run();
                    let run_font = run.font().clone();
                    let run_key = (
                        run.text_range().start,
                        run.text_range().end,
                        run_font.data.id(),
                        run_font.index,
                    );
                    if prev_run_key != Some(run_key) {
                        prev_run_key = Some(run_key);
                        run_glyph_cursor = 0;
                    }
                    let glyph_start = run_glyph_cursor;
                    let glyph_count = glyph_run.glyphs().count();
                    run_glyph_cursor += glyph_count;
                    let glyph_end = glyph_start + glyph_count;

                    let font_size = run.font_size();
                    let brush = glyph_run.style().brush;
                    let run_ul = glyph_run.style().underline.is_some();
                    let run_st = glyph_run.style().strikethrough.is_some();
                    let synth_italic = run.synthesis().skew().is_some();
                    let is_color_font = engine.font_supports_color(run_font.borrow_font());
                    let mut x_cursor = origin_x + glyph_run.offset();
                    let mut cluster_glyph_idx = 0usize;
                    let mut prev_cluster_in_slice = false;

                    for cluster in run.visual_clusters() {
                        let c_glyph_count = cluster.glyphs().count();
                        let in_slice = if c_glyph_count == 0 {
                            prev_cluster_in_slice || (cluster_glyph_idx == 0 && glyph_start == 0)
                        } else {
                            let matched = cluster_glyph_idx < glyph_end
                                && (cluster_glyph_idx + c_glyph_count) > glyph_start;
                            prev_cluster_in_slice = matched;
                            matched
                        };
                        cluster_glyph_idx += c_glyph_count;
                        if !in_slice {
                            continue;
                        }

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

                        if c_glyph_count > 0 {
                            let mut gx = x_cursor;
                            for g in cluster.glyphs() {
                                let rendered_color = is_color_font
                                    && engine
                                        .try_render_color_glyph(
                                            run_font.borrow_font(),
                                            g.id as u16,
                                            font_size,
                                        )
                                        .is_some_and(|cg| {
                                            collector.images.push((
                                                gx + g.x + cg.left,
                                                baseline + g.y - cg.top,
                                                cg.pixmap,
                                            ));
                                            true
                                        });
                                if !rendered_color {
                                    if let Some(ag) = engine.try_render_alpha_glyph(
                                        run_font.borrow_font(),
                                        g.id as u16,
                                        font_size,
                                        synth_italic,
                                    ) {
                                        collector.mask_glyphs.push(PositionedMaskGlyph {
                                            x: (gx + g.x).round() as i32 + ag.left,
                                            y: (baseline + g.y).round() as i32 - ag.top,
                                            glyph: ag,
                                            rgba: brush,
                                        });
                                    }
                                }
                                gx += g.advance;
                            }
                        }
                        if run_ul && adv > 0.0 {
                            collector
                                .rects
                                .push((x_cursor, baseline + 2.0, adv, 1.0, brush));
                        }
                        if run_st && adv > 0.0 {
                            collector
                                .rects
                                .push((x_cursor, baseline - font_size * 0.32, adv, 1.0, brush));
                        }

                        if let Some(slice) = full_text.get(range.clone()) {
                            if !slice.is_empty() && !slice.chars().all(|c| c == '\n' || c == '\r') {
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
                    prev_run_key = None;
                    run_glyph_cursor = 0;
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
                        engine.collect_glossary_glyphs_from_layout(
                            &ruby_l,
                            bx + (rbox.w - rw) * 0.5,
                            by,
                            false,
                            false,
                            collector,
                        );
                        engine.collect_glossary_glyphs_from_layout(
                            &base_l,
                            bx + (rbox.w - bw) * 0.5,
                            by + rbox.base_y,
                            rbox.style.underline,
                            rbox.style.strikethrough,
                            collector,
                        );

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
    render_structured_glossary_entry_with_theme(
        raw_glossary,
        fallback_glossary,
        stylesheet,
        detail_states,
        legacy_style,
        dict_resource_dirs,
        dic_id,
        dict_name,
        avail_width_px,
        selected,
        &PopupCssTheme::default(),
    )
}

#[allow(clippy::too_many_arguments)]
fn render_structured_glossary_entry_with_theme(
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
    theme: &PopupCssTheme,
) -> Option<RenderedGlossaryDoc> {
    let avail_w = avail_width_px.clamp(120.0, 960.0);
    let mut base_text_color = if selected {
        theme.text_color
    } else {
        theme.muted_color
    };
    let text_color_css = rgba_to_css_color(base_text_color);
    let gloss_text_css = rgba_to_css_color(theme.text_color);
    let bg_color_css = rgba_to_css_color(theme.popup_bg);
    let root_font_px = f64::from(theme.glossary_font_size.clamp(9.0, 32.0));

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
    let mut ctx = DefContext {
        screen: Some(ScreenMetrics {
            available_width: avail_w as i32,
            available_height: 440,
            logical_dpi: 96.0,
        }),
        root_font_pixel_size: root_font_px,
        parent_font_pixel_size: root_font_px,
        text_color: text_color_css,
        glossary_text_color: gloss_text_css,
        glossary_background_color: bg_color_css.clone(),
        background_color: bg_color_css.clone(),
        painted_background_color: bg_color_css,
        needs_element_child_count: stylesheet
            .as_ref()
            .is_some_and(|s| s.uses_element_child_count),
        dictionary_styles: stylesheet,
        text_direction: LayoutDirection::LeftToRight,
        detail_states: detail_states.clone(),
        ..DefContext::default()
    };

    // Virtual dictionary container so Yomitan `[data-dictionary="..."]` / `[data-dictionary^="..."]`
    // and `.gloss-item` rules match without interfering with bare tag selectors (`div`, `li`) in
    // dictionary `styles.css`.
    let mut dict_root = mdef::StructuredElement::new();
    dict_root.tag = "memento-dict".to_owned();
    dict_root.classes.insert("gloss-item".to_owned());
    dict_root.classes.insert("yomitan-glossary".to_owned());
    dict_root
        .attributes
        .insert("data-dictionary".to_owned(), dict_name.to_owned());
    ctx.selector_elements.push(dict_root);
    ctx.elements.push(0);

    let mut root_inline = InlineStyleIR {
        font_family: theme.font_family.clone(),
        font_px: theme.glossary_font_size.clamp(9.0, 32.0),
        color: base_text_color,
        ..InlineStyleIR::default()
    };

    // Apply any dictionary-root rules (e.g. `[data-dictionary^="JMdict"] { font-size: 16px; }`).
    let mut root_hidden = false;
    if ctx.dictionary_styles.is_some() {
        let mut root_decls = mdef::CssDeclarations::new();
        renderer.add_matching_css_rules(&mut ctx, &mut root_decls, None, None);
        if root_decls
            .get("display")
            .is_some_and(|v| v.trim().eq_ignore_ascii_case("none"))
        {
            root_hidden = true;
        }
        if let Some(fs) = root_decls.get("font-size") {
            let px = mdef::helpers::css_font_size_to_pixels(
                fs,
                ctx.screen.as_ref(),
                ctx.parent_font_pixel_size,
                ctx.root_font_pixel_size,
            );
            if px > 0.0 {
                ctx.parent_font_pixel_size = px;
                root_inline.font_px = (px as f32).clamp(9.0, 32.0);
            }
        }
        if let Some(ff) = root_decls.get("font-family") {
            let cleaned = clean_css_font_family(ff);
            if !cleaned.is_empty() {
                root_inline.font_family = cleaned;
            }
        }
        if let Some(col) = root_decls.get("color") {
            if let Some(rgba) = parse_css_theme_color(col) {
                base_text_color = rgba;
                root_inline.color = rgba;
                ctx.text_color = rgba_to_css_color(rgba);
            }
        }
        root_inline.weight = parse_weight_css(root_decls.get("font-weight"), root_inline.weight);
        if let Some(st) = root_decls.get("font-style") {
            let l = st.trim().to_ascii_lowercase();
            if l == "italic" || l == "oblique" {
                root_inline.italic = true;
            } else if l == "normal" {
                root_inline.italic = false;
            }
        }
    }

    let mut builder = GlossaryBuilder {
        renderer,
        ctx,
        dict_resource_dirs,
        dic_id,
        dict_name,
        default_link_color: theme.link_color,
        had_hidden_elements: root_hidden,
    };

    let mut top_blocks: Vec<BlockIR> = Vec::new();
    let use_bullets = legacy_style == LegacyGlossaryStyle::Bullet && !contains_structured;

    if !root_hidden {
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
                    marker: use_bullets.then(|| ("•".to_owned(), base_text_color, root_inline.font_px)),
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
                            marker: use_bullets.then(|| ("•".to_owned(), base_text_color, root_inline.font_px)),
                            summary_detail_id: None,
                        });
                    }
                }
                _ => {}
            },
            _ => {}
        }
    }
    }

    if top_blocks.is_empty() {
        if builder.had_hidden_elements {
            let empty_pm = tiny_skia::Pixmap::new(1, 1)?;
            return Some(RenderedGlossaryDoc {
                width_px: 1.0,
                height_px: 1.0,
                image: pixmap_to_slint_image(&empty_pm),
                hit_zones: Vec::new(),
                selectable_chars: Vec::new(),
                line_bottoms: Vec::new(),
                plain_text: String::new(),
            });
        }
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

        for mg in &collector.mask_glyphs {
            blit_alpha_glyph(&mut pixmap, mg.x, mg.y, &mg.glyph, mg.rgba);
        }

        while collector
            .selectable_chars
            .last()
            .is_some_and(|c| c.ch == '\n')
        {
            collector.selectable_chars.pop();
        }
        let plain_text: String = collector.selectable_chars.iter().map(|c| c.ch).collect();
        let line_bottoms = compute_visual_line_bottoms(&collector.line_bounds);

        Some(RenderedGlossaryDoc {
            width_px: pw as f32,
            height_px: ph as f32,
            image: pixmap_to_slint_image(&pixmap),
            hit_zones: collector.hit_zones,
            selectable_chars: collector.selectable_chars,
            line_bottoms,
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

fn parse_css_invert_ratio(filter_value: &str) -> Option<f32> {
    let lower = filter_value.to_ascii_lowercase();
    let pos = lower.find("invert(")?;
    let rest = &lower[pos + 7..];
    let end = rest.find(')')?;
    let arg = rest[..end].trim();
    if arg.is_empty() {
        return Some(1.0);
    }
    parse_css_unit_float(arg)
}

fn invert_pixmap(src: &tiny_skia::Pixmap, ratio: f32) -> Arc<tiny_skia::Pixmap> {
    let ratio = ratio.clamp(0.0, 1.0);
    let mut out = src.clone();
    for px in out.pixels_mut() {
        let a = px.alpha();
        if a == 0 {
            continue;
        }
        let af = f32::from(a) / 255.0;
        let inv_chan = |c: u8| -> u8 {
            let unp = (f32::from(c) / af).clamp(0.0, 255.0);
            let blended = unp * (1.0 - ratio) + (255.0 - unp) * ratio;
            (blended * af).round().clamp(0.0, f32::from(a)) as u8
        };
        if let Some(p) = tiny_skia::PremultipliedColorU8::from_rgba(
            inv_chan(px.red()),
            inv_chan(px.green()),
            inv_chan(px.blue()),
            a,
        ) {
            *px = p;
        }
    }
    Arc::new(out)
}

fn classify_pitch_category(pitches: &[RichPitchGraph], group_tags: &str) -> Option<&'static str> {
    let first = pitches.first()?;
    let pos = first.position;
    if pos == 0 {
        return Some("heiban");
    }
    let is_kifuku_pos = group_tags.split_whitespace().any(|t| {
        t == "adj-i"
            || t == "v1"
            || t == "vk"
            || t == "vs-i"
            || t == "vs-s"
            || t.starts_with("v5")
            || t.starts_with("v1-")
            || t.starts_with("v2")
            || t.starts_with("v4")
    });
    if is_kifuku_pos {
        return Some("kifuku");
    }
    if pos == 1 {
        Some("atamadaka")
    } else {
        let mora_count = first.mora.row_count() as i32;
        if mora_count > 1 && pos >= mora_count {
            Some("odaka")
        } else {
            Some("nakadaka")
        }
    }
}

#[derive(Default, Clone)]
struct RichRowExtras {
    hit: Option<SearchHit>,
    term_pills: Vec<RichTagPill>,
    def_pills: Vec<RichTagPill>,
    derivation_steps: Vec<RichDerivationStep>,
    pitch_graphs: Vec<RichPitchGraph>,
    pitch_category: Option<&'static str>,
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

#[derive(Clone, Debug)]
struct GlossaryScanHighlight {
    row_index: i32,
    cell_indices: Vec<usize>,
}

#[derive(Clone, Debug)]
struct HoveredGlossaryTarget {
    depth: i32,
    row_index: i32,
    start_char: usize,
    query: String,
    cell_indices: Vec<usize>,
    anchor_x: f32,
    anchor_y: f32,
}

#[derive(Default)]
struct NestedPopupLevel {
    label: String,
    rows: Vec<DictResult>,
    headword: String,
    reading: String,
    anchor_x: f32,
    anchor_y: f32,
    locked: bool,
    source_row_index: i32,
    source_start_char: usize,
    rich_epoch: u64,
    row_extras: HashMap<i32, RichRowExtras>,
    glossary_cache: HashMap<GlossaryDocCacheKey, RenderedGlossaryDoc>,
    last_rendered_key: HashMap<i32, GlossaryDocCacheKey>,
    selection: Option<GlossarySelectionState>,
    scan_highlight: Option<GlossaryScanHighlight>,
    expanded_cards: HashSet<i32>,
    row_anki_duplicates: Vec<bool>,
    row_anki_note_ids_i64: Vec<i64>,
    row_anki_browse_queries: Vec<SharedString>,
    row_anki_tags: Vec<SharedString>,
}

fn encode_note_id_i32(nid: i64) -> i32 {
    if nid <= 0 {
        0
    } else if nid <= i64::from(i32::MAX) {
        nid as i32
    } else {
        i32::MAX
    }
}

fn parse_interface_font_setting(
    config: &ConfigController,
    stock_config: &ConfigController,
    key: &str,
    min_px: f32,
    max_px: f32,
) -> Option<InterfaceFontFallback> {
    let raw_val = config.get_setting(raw::interface::GROUP, key)?;
    let memento_config::Value::Text(ref raw_csv) = raw_val else {
        return None;
    };
    // `ConfigController::get_setting` synthesizes legacy Qt defaults (e.g., 10pt glossary,
    // 108pt Kanji Stroke Orders) when unset. Only override Memento's built-in Slint theme
    // defaults when the user actually customized the setting away from the stock Qt default.
    let stock_default = stock_config.effective_setting(raw::interface::GROUP, key);
    if stock_default.as_ref() == Some(&raw_val) {
        return None;
    }
    let spec = memento_config::QtFontSpec::parse(raw_csv).ok()?;
    let size_px = ((spec.point_size as f32) * (96.0 / 72.0)).clamp(min_px, max_px);
    Some(InterfaceFontFallback {
        family: spec.family,
        size_px,
        bold: spec.bold,
        italic: spec.italic,
    })
}

struct CachedConfigSettings {
    subtitle_style: SubtitleRenderStyle,
    popup_fonts: PopupInterfaceFonts,
    search_method: i32,
    search_modifier: i32,
    search_delay_ms: u64,
    middle_mouse_scan: bool,
    mecab_enabled: bool,
    glossary_style: LegacyGlossaryStyle,
    dict_order: Vec<i64>,
    excluded_glossaries: Vec<String>,
    anki_enabled: bool,
    anki_show_tag: bool,
    anki_term_profile: Option<crate::controller::config::AnkiNoteConfig>,
    anki_kanji_profile: Option<crate::controller::config::AnkiNoteConfig>,
    conf_mtime: Option<SystemTime>,
    anki_mtime: Option<SystemTime>,
}

impl CachedConfigSettings {
    fn from_controller(config: &ConfigController, config_dir: &Path) -> Self {
        let mut out = Self {
            subtitle_style: SubtitleRenderStyle::default(),
            popup_fonts: PopupInterfaceFonts::default(),
            search_method: raw::search::METHOD_DEFAULT as i32,
            search_modifier: raw::search::MODIFIER_DEFAULT as i32,
            search_delay_ms: raw::search::DELAY_DEFAULT as u64,
            middle_mouse_scan: raw::search::MIDDLE_MOUSE_SCAN_DEFAULT,
            mecab_enabled: raw::search::matcher::MECAB_IPADIC_DEFAULT,
            glossary_style: LegacyGlossaryStyle::Bullet,
            dict_order: Vec::new(),
            excluded_glossaries: Vec::new(),
            anki_enabled: false,
            anki_show_tag: false,
            anki_term_profile: None,
            anki_kanji_profile: None,
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
        if let Some(v) = config.effective_setting(
            raw::interface::GROUP,
            raw::interface::subtitle::LINE_SPACING,
        ) {
            let num = match v {
                Value::Real(f) => Some(f as f32),
                Value::Int(i) => Some(i as f32),
                _ => None,
            };
            if let Some(n) = num {
                self.subtitle_style.line_spacing = n.clamp(0.0, 120.0);
            }
        }
        let stock_config = ConfigController::new();
        self.popup_fonts = PopupInterfaceFonts {
            expression: parse_interface_font_setting(
                config,
                &stock_config,
                raw::interface::SEARCH_EXPRESSION_FONT,
                12.0,
                48.0,
            ),
            reading: parse_interface_font_setting(
                config,
                &stock_config,
                raw::interface::SEARCH_READING_FONT,
                8.0,
                28.0,
            ),
            conj: parse_interface_font_setting(
                config,
                &stock_config,
                raw::interface::SEARCH_CONJ_FONT,
                8.0,
                24.0,
            ),
            tag: parse_interface_font_setting(
                config,
                &stock_config,
                raw::interface::SEARCH_TAG_FONT,
                8.0,
                22.0,
            ),
            glossary: parse_interface_font_setting(
                config,
                &stock_config,
                raw::interface::SEARCH_GLOSSARY_FONT,
                9.0,
                32.0,
            ),
            kanji: parse_interface_font_setting(
                config,
                &stock_config,
                raw::interface::SEARCH_KANJI_FONT,
                16.0,
                96.0,
            ),
        };
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
        self.mecab_enabled = match config.effective_setting(
            raw::search::GROUP,
            raw::search::matcher::MECAB_IPADIC,
        ) {
            Some(Value::Bool(b)) => b,
            _ => raw::search::matcher::MECAB_IPADIC_DEFAULT,
        };
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
        self.anki_enabled = config.anki_enabled();
        self.anki_show_tag = config.anki_show_tag();
        self.anki_term_profile = config.anki_profile_note_config();
        self.anki_kanji_profile = config.anki_kanji_note_config();
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
    root_scan_highlight: Option<GlossaryScanHighlight>,
    root_expanded_cards: HashSet<i32>,
    root_row_anki_note_ids_i64: Vec<i64>,

    suppress_history_reset: bool,
    back_stack: Vec<PopupHistoryEntry>,
    forward_stack: Vec<PopupHistoryEntry>,

    nested_stack: Vec<NestedPopupLevel>,
    hovered_glossary_target: Option<HoveredGlossaryTarget>,
    nested_card_hovered: bool,
    nested_body_hovered: bool,
    nested_scan_timer: slint::Timer,
    nested_dismiss_timer: slint::Timer,
    pending_nested_anki_add: Option<(usize, String, String, bool)>,

    hovered_cluster: Option<HoveredSubtitleCluster>,
    active_scan_utf16_range: Option<(usize, usize)>,
    pending_scan_utf16_start: Option<usize>,
    pending_scan_sub_query: Option<String>,
    scan_timer: slint::Timer,
}

impl RichUiMutable {
    fn effective_popup_theme(&self) -> PopupCssTheme {
        self.stylesheets.theme().with_interface_font_fallbacks(
            self.stylesheets.overrides(),
            &self.settings.popup_fonts,
        )
    }

    fn nested_level(&self, depth: i32) -> Option<&NestedPopupLevel> {
        if depth <= 0 {
            return None;
        }
        self.nested_stack.get((depth - 1) as usize)
    }

    fn nested_level_mut(&mut self, depth: i32) -> Option<&mut NestedPopupLevel> {
        if depth <= 0 {
            return None;
        }
        self.nested_stack.get_mut((depth - 1) as usize)
    }

    fn clear_all_glossary_caches(&mut self) {
        self.rich_epoch = self.rich_epoch.wrapping_add(1);
        self.glossary_cache.clear();
        self.last_rendered_key.clear();
        for lvl in &mut self.nested_stack {
            lvl.rich_epoch = lvl.rich_epoch.wrapping_add(1);
            lvl.glossary_cache.clear();
            lvl.last_rendered_key.clear();
        }
    }
}

fn build_row_extras_for_grouped(
    db_helper: &mut RichDbHelper,
    dict_ctrl: &mut DictController,
    rows: &mut [DictResult],
    grouped_hits: &[SearchHit],
    precomputed_pitch_details: Option<&HashMap<(String, String), Vec<TermPitchDetail>>>,
) -> HashMap<i32, RichRowExtras> {
    let mut extras_map = HashMap::new();
    let mut group_start = 0usize;
    while group_start < rows.len() && group_start < grouped_hits.len() {
        let mut group_end = group_start + 1;
        while group_end < rows.len() && group_end < grouped_hits.len() && !rows[group_end].term_start {
            group_end += 1;
        }
        let first_hit = &grouped_hits[group_start];
        let mut raw_term_tags = Vec::new();
        for hit in &grouped_hits[group_start..group_end] {
            raw_term_tags.extend(db_helper.resolve_tag_list(hit.dictionary_id, &hit.term_tags));
        }
        let pitch_details = if !first_hit.is_kanji {
            if let Some(pre) = precomputed_pitch_details {
                pre.get(&(first_hit.expression.clone(), first_hit.reading.clone()))
                    .cloned()
                    .unwrap_or_default()
            } else {
                dict_ctrl.get_pitch_details(&first_hit.expression, &first_hit.reading)
            }
        } else {
            Vec::new()
        };
        let rich_pitches = rich_pitch_graphs_from_details(&pitch_details);
        let mut group_pos_tags = String::new();
        for hit in &grouped_hits[group_start..group_end] {
            if !hit.def_tags.is_empty() {
                group_pos_tags.push(' ');
                group_pos_tags.push_str(&hit.def_tags);
            }
            if !hit.term_tags.is_empty() {
                group_pos_tags.push(' ');
                group_pos_tags.push_str(&hit.term_tags);
            }
        }
        let group_pitch_category = classify_pitch_category(&rich_pitches, &group_pos_tags);
        let kanji_freqs: Vec<TermFrequency> = if first_hit.is_kanji {
            dict_ctrl.get_kanji_frequencies(&first_hit.expression)
        } else {
            Vec::new()
        };

        for idx in group_start..group_end {
            let hit = &grouped_hits[idx];
            let raw_def_tags = db_helper.resolve_tag_list(hit.dictionary_id, &hit.def_tags);
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
                db_helper.build_kanji_detail(&hit.expression, hit.dictionary_id)
            } else {
                None
            };

            extras_map.insert(
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
                    pitch_category: if idx == group_start {
                        group_pitch_category
                    } else {
                        None
                    },
                    kanji_detail,
                    detail_states: collect_default_detail_states(&hit.raw_glossary),
                },
            );
        }

        if first_hit.is_kanji && !kanji_freqs.is_empty() {
            rows[group_start].frequencies = Rc::new(VecModel::from(
                kanji_freqs
                    .into_iter()
                    .map(|f| PopupFrequency {
                        dictionary: f.dictionary.into(),
                        value: f.value.into(),
                    })
                    .collect::<Vec<_>>(),
            ))
            .into();
        }

        group_start = group_end;
    }
    extras_map
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

struct AnkiPopupCheckRequest {
    nav_epoch: u64,
    lookup_epoch: Option<u64>,
    target_nested_depth: Option<usize>,
    show_tags: bool,
    term_profile: Option<crate::controller::config::AnkiNoteConfig>,
    kanji_profile: Option<crate::controller::config::AnkiNoteConfig>,
    term_entries: Vec<(String, String)>,
    kanji_entries: Vec<(String, String)>,
}

struct AnkiPopupCheckResponse {
    nav_epoch: u64,
    lookup_epoch: Option<u64>,
    target_nested_depth: Option<usize>,
    show_tags: bool,
    term_statuses: HashMap<(String, String), crate::controller::anki::EntryAnkiStatus>,
    kanji_statuses: HashMap<String, crate::controller::anki::EntryAnkiStatus>,
}

struct NestedScanRequest {
    epoch: u64,
    parent_depth: i32,
    row_index: i32,
    start_char: usize,
    query: String,
    cell_indices: Vec<usize>,
    anchor_x: f32,
    anchor_y: f32,
    locked: bool,
    dict_order: Vec<i64>,
    excluded_glossaries: Vec<String>,
    mecab_enabled: bool,
}

struct NestedScanResponse {
    epoch: u64,
    parent_depth: i32,
    row_index: i32,
    start_char: usize,
    matched_char_count: usize,
    cell_indices: Vec<usize>,
    anchor_x: f32,
    anchor_y: f32,
    locked: bool,
    hits: Vec<SearchHit>,
    excluded_glossaries: Vec<String>,
    headword: String,
    reading: String,
    term_meta_rows: Vec<(
        String,
        String,
        Vec<crate::controller::dict::TermPitchGraph>,
        Vec<TermFrequency>,
    )>,
    term_pitch_details: HashMap<(String, String), Vec<TermPitchDetail>>,
}

fn current_external_lookup_epoch(
    ext: &std::sync::Mutex<Option<Arc<AtomicU64>>>,
) -> Option<u64> {
    ext.lock()
        .ok()
        .and_then(|guard| guard.as_ref().map(|e| e.load(Ordering::Acquire)))
}

struct RichUiBridge {
    instance_id: i32,
    app_weak: slint::Weak<MementoApp>,
    db_path: PathBuf,
    state: RefCell<RichUiMutable>,
    panel_tx: mpsc::Sender<(u64, String, bool, Vec<i64>, Vec<String>, bool)>,
    panel_res_rx: Arc<std::sync::Mutex<mpsc::Receiver<(u64, Vec<SearchHit>, Vec<String>)>>>,
    panel_poll_timer: slint::Timer,
    panel_epoch: Arc<AtomicU64>,
    anki_tx: mpsc::Sender<AnkiPopupCheckRequest>,
    anki_res_rx: Arc<std::sync::Mutex<mpsc::Receiver<AnkiPopupCheckResponse>>>,
    anki_poll_timer: slint::Timer,
    anki_nav_epoch: Arc<AtomicU64>,
    nested_anki_nav_epoch: Arc<AtomicU64>,
    external_lookup_epoch: Arc<std::sync::Mutex<Option<Arc<AtomicU64>>>>,
    nested_tx: mpsc::Sender<NestedScanRequest>,
    nested_res_rx: Arc<std::sync::Mutex<mpsc::Receiver<NestedScanResponse>>>,
    nested_poll_timer: slint::Timer,
    nested_epoch: Arc<AtomicU64>,
}

impl RichUiBridge {
    fn ensure_fresh_db_and_config(&self, app: &MementoApp) {
        let mut reloaded_config = false;
        let mut reloaded_css_theme: Option<PopupCssTheme> = None;
        {
            let mut st = self.state.borrow_mut();
            let config_dir = st.config_dir.clone();
            if st.settings.reload_if_changed_on_disk(&config_dir) {
                reloaded_config = true;
                st.clear_all_glossary_caches();
                let bg = st.settings.subtitle_style.bg_rgba;
                let sb = app.global::<SubtitleOverlayBridge>();
                sb.set_middle_mouse_scan_enabled(st.settings.middle_mouse_scan);
                sb.set_subtitle_bg_color(slint::Color::from_argb_u8(bg[3], bg[0], bg[1], bg[2]));
                let rev = sb.get_style_revision();
                sb.set_style_revision(rev.wrapping_add(1));
                reloaded_css_theme = Some(st.effective_popup_theme());
            }
            if st.stylesheets.reload_user_css_if_changed() {
                st.clear_all_glossary_caches();
                reloaded_css_theme = Some(st.effective_popup_theme());
            }
            let sig = crate::controller::dict::db_file_fingerprint(&self.db_path);
            if sig != st.db_sig {
                st.db_sig = sig;
                st.dict_ctrl = DictController::open(&self.db_path).unwrap_or_default();
            }
            let order = st.settings.dict_order.clone();
            let mecab_enabled = st.settings.mecab_enabled;
            st.dict_ctrl.set_order(&order);
            st.dict_ctrl.set_mecab_enabled(mecab_enabled);
            st.db_helper.refresh_if_needed();
        }
        if let Some(theme) = reloaded_css_theme {
            apply_popup_css_theme_to_bridge(app, &theme);
            let pb = app.global::<PopupBridge>();
            pb.set_rich_revision(pb.get_rich_revision().wrapping_add(1));
            if pb.get_nested_depth() > 0 {
                pb.set_nested_revision(pb.get_nested_revision().wrapping_add(1));
            }
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

    fn on_dictionary_rows_updated(self: &Rc<Self>, app: &MementoApp, rows: &[DictResult]) {
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
        st.root_scan_highlight = None;
        st.root_expanded_cards.clear();
        st.root_row_anki_note_ids_i64.clear();
        st.nested_stack.clear();
        st.hovered_glossary_target = None;
        st.nested_card_hovered = false;
        st.nested_body_hovered = false;
        st.nested_scan_timer.stop();
        st.nested_dismiss_timer.stop();
        st.pending_nested_anki_add = None;
        self.nested_epoch.fetch_add(1, Ordering::AcqRel);
        self.nested_poll_timer.stop();

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
                let mut group_pos_tags = String::new();
                for hit in &candidate_hits[group_start..group_end] {
                    if !hit.def_tags.is_empty() {
                        group_pos_tags.push(' ');
                        group_pos_tags.push_str(&hit.def_tags);
                    }
                    if !hit.term_tags.is_empty() {
                        group_pos_tags.push(' ');
                        group_pos_tags.push_str(&hit.term_tags);
                    }
                }
                let group_pitch_category = classify_pitch_category(&rich_pitches, &group_pos_tags);
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
                            pitch_category: if idx == group_start {
                                group_pitch_category
                            } else {
                                None
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
        self.sync_nested_popup_to_bridge(app, true);

        let sb = app.global::<SubtitleOverlayBridge>();
        sb.set_highlight_revision(sb.get_highlight_revision().wrapping_add(1));

        self.dispatch_anki_check_for_rows(app, rows, None);
    }

    fn dispatch_anki_check_for_rows(
        self: &Rc<Self>,
        app: &MementoApp,
        rows: &[DictResult],
        target_nested_depth: Option<usize>,
    ) {
        let next_nav = if target_nested_depth.is_some() {
            self.nested_anki_nav_epoch.fetch_add(1, Ordering::AcqRel) + 1
        } else {
            self.anki_nav_epoch.fetch_add(1, Ordering::AcqRel) + 1
        };
        let pb = app.global::<PopupBridge>();
        let (anki_enabled, show_tags, term_profile, kanji_profile) = {
            let st = self.state.borrow();
            (
                st.settings.anki_enabled,
                st.settings.anki_show_tag,
                st.settings.anki_term_profile.clone(),
                st.settings.anki_kanji_profile.clone(),
            )
        };
        pb.set_anki_show_tags(show_tags);
        if target_nested_depth.is_none() {
            self.state.borrow_mut().root_row_anki_note_ids_i64.clear();
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

        if rows.is_empty() || !anki_enabled {
            if target_nested_depth.is_none() {
                self.anki_poll_timer.stop();
                pb.set_anki_checking(false);
            }
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
            if target_nested_depth.is_none() {
                self.anki_poll_timer.stop();
                pb.set_anki_checking(false);
            }
            return;
        }

        let lookup_epoch = current_external_lookup_epoch(&self.external_lookup_epoch);
        pb.set_anki_checking(true);
        let _ = self.anki_tx.send(AnkiPopupCheckRequest {
            nav_epoch: next_nav,
            lookup_epoch,
            target_nested_depth,
            show_tags,
            term_profile: has_valid_term.then_some(term_profile).flatten(),
            kanji_profile: has_valid_kanji.then_some(kanji_profile).flatten(),
            term_entries,
            kanji_entries,
        });

        let weak_br = Rc::downgrade(self);
        self.anki_poll_timer.start(
            slint::TimerMode::Repeated,
            std::time::Duration::from_millis(8),
            move || {
                let Some(br) = weak_br.upgrade() else {
                    return;
                };
                let Some(app) = br.app_weak.upgrade() else {
                    br.anki_poll_timer.stop();
                    return;
                };
                let _ = drain_anki_results(
                    &app,
                    &br.anki_res_rx,
                    &br.anki_nav_epoch,
                    &br.nested_anki_nav_epoch,
                    &br.external_lookup_epoch,
                );
                if !app.global::<PopupBridge>().get_anki_checking() {
                    br.anki_poll_timer.stop();
                }
            },
        );
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
        SUPPRESS_FIRST_TERM_READY.with(|flag| flag.set(true));
        set_dictionary_rows(app, entry.rows);
        SUPPRESS_FIRST_TERM_READY.with(|flag| flag.set(false));
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

    fn render_or_get_glossary_at_depth(
        &self,
        depth: i32,
        row_index: i32,
        width_px: f32,
        selected: bool,
    ) -> Option<RenderedGlossaryDoc> {
        let bucket_w = ((width_px.max(120.0) / 4.0).round() as i32 * 4).max(120);
        let mut st = self.state.borrow_mut();
        let (hit, detail_states, epoch) = if depth == 0 {
            let extras = st.row_extras.get(&row_index)?;
            let hit = extras.hit.as_ref()?.clone();
            (hit, extras.detail_states.clone(), st.rich_epoch)
        } else {
            let lvl = st.nested_level(depth)?;
            let extras = lvl.row_extras.get(&row_index)?;
            let hit = extras.hit.as_ref()?.clone();
            (hit, extras.detail_states.clone(), lvl.rich_epoch)
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
        };
        if depth == 0 {
            if let Some(doc) = st.glossary_cache.get(&cache_key).cloned() {
                st.last_rendered_key.insert(row_index, cache_key);
                return Some(doc);
            }
            if st.glossary_cache.len() > 256 {
                st.glossary_cache.clear();
                st.last_rendered_key.clear();
            }
        } else if let Some(lvl) = st.nested_level_mut(depth) {
            if let Some(doc) = lvl.glossary_cache.get(&cache_key).cloned() {
                lvl.last_rendered_key.insert(row_index, cache_key);
                return Some(doc);
            }
            if lvl.glossary_cache.len() > 128 {
                lvl.glossary_cache.clear();
                lvl.last_rendered_key.clear();
            }
        }
        let res_dirs = st.dict_resource_dirs.clone();
        let theme = st.effective_popup_theme();
        let legacy_style = st.settings.glossary_style;
        let sheet = st
            .stylesheets
            .get_for_dictionary(&res_dirs, hit.dictionary_id, &hit.dict_name);
        let doc = render_structured_glossary_entry_with_theme(
            &hit.raw_glossary,
            &hit.glossary,
            sheet,
            &detail_states,
            legacy_style,
            &res_dirs,
            hit.dictionary_id,
            &hit.dict_name,
            bucket_w as f32,
            selected,
            &theme,
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

    fn latest_cached_doc_at_depth(&self, depth: i32, row_index: i32) -> Option<RenderedGlossaryDoc> {
        let st = self.state.borrow();
        let (epoch, last_key, cache) = if depth == 0 {
            (st.rich_epoch, &st.last_rendered_key, &st.glossary_cache)
        } else {
            let lvl = st.nested_level(depth)?;
            (lvl.rich_epoch, &lvl.last_rendered_key, &lvl.glossary_cache)
        };
        if let Some(key) = last_key.get(&row_index) {
            if key.rich_epoch == epoch {
                if let Some(doc) = cache.get(key) {
                    return Some(doc.clone());
                }
            }
        }
        cache
            .iter()
            .find(|(k, _)| k.row_index == row_index && k.rich_epoch == epoch)
            .map(|(_, doc)| doc.clone())
    }

    fn build_rich_glossary_render(
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
                    let lo = sel.anchor_char.min(sel.focus_char).min(doc.selectable_chars.len());
                    let hi = sel.anchor_char.max(sel.focus_char).min(doc.selectable_chars.len());
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

    fn update_glossary_selection_text_at_depth(&self, app: &MementoApp, depth: i32) {
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

    fn toggle_glossary_details_at_depth(
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
                st.glossary_cache.retain(|k, _| k.row_index != row_idx);
                st.last_rendered_key.remove(&row_idx);
            } else if let Some(lvl) = st.nested_level_mut(depth) {
                if let Some(extras) = lvl.row_extras.get_mut(&row_idx) {
                    let current = extras.detail_states.get(detail_id).copied().unwrap_or(false);
                    extras.detail_states.insert(detail_id.to_owned(), !current);
                }
                lvl.glossary_cache.retain(|k, _| k.row_index != row_idx);
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

    fn handle_internal_link_str_at_depth(
        self: &Rc<Self>,
        app: &MementoApp,
        depth: i32,
        row_idx: i32,
        link: &str,
    ) -> bool {
        if let Some(rest) = link.strip_prefix("memento://glossary-details?") {
            let mut id = String::new();
            for (k, v) in url::form_urlencoded::parse(rest.as_bytes()) {
                if k == "id" {
                    id = v.into_owned();
                }
            }
            if !id.is_empty() {
                self.toggle_glossary_details_at_depth(app, depth, row_idx, &id);
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
                return self.handle_internal_link_str_at_depth(app, depth, row_idx, &target);
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

    fn sync_nested_popup_to_bridge(&self, app: &MementoApp, nav_changed: bool) {
        let (
            depth,
            locked,
            breadcrumb,
            headword,
            reading,
            anchor_x,
            anchor_y,
            rows,
            dups,
            note_ids,
            queries,
            tags,
        ) = {
            let st = self.state.borrow();
            let depth = st.nested_stack.len();
            if let Some(top) = st.nested_stack.last() {
                let crumb = st
                    .nested_stack
                    .iter()
                    .map(|lvl| lvl.label.as_str())
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>()
                    .join(" › ");
                let note_ids: Vec<i32> = top
                    .row_anki_note_ids_i64
                    .iter()
                    .copied()
                    .map(encode_note_id_i32)
                    .collect();
                (
                    depth,
                    top.locked,
                    crumb,
                    top.headword.clone(),
                    top.reading.clone(),
                    top.anchor_x,
                    top.anchor_y,
                    top.rows.clone(),
                    top.row_anki_duplicates.clone(),
                    note_ids,
                    top.row_anki_browse_queries.clone(),
                    top.row_anki_tags.clone(),
                )
            } else {
                (
                    0,
                    false,
                    String::new(),
                    String::new(),
                    String::new(),
                    0.0,
                    0.0,
                    Vec::new(),
                    Vec::new(),
                    Vec::new(),
                    Vec::new(),
                    Vec::new(),
                )
            }
        };
        let pb = app.global::<PopupBridge>();
        pb.set_nested_depth(depth as i32);
        pb.set_nested_max_depth(MAX_NESTED_POPUP_DEPTH as i32);
        pb.set_nested_locked(locked);
        pb.set_nested_breadcrumb(breadcrumb.into());
        pb.set_nested_headword(headword.into());
        pb.set_nested_reading(reading.into());
        pb.set_nested_anchor_x(anchor_x);
        pb.set_nested_anchor_y(anchor_y);
        pb.set_nested_results(Rc::new(VecModel::from(rows)).into());
        pb.set_nested_row_anki_duplicates(Rc::new(VecModel::from(dups)).into());
        pb.set_nested_row_anki_note_ids(Rc::new(VecModel::from(note_ids)).into());
        pb.set_nested_row_anki_browse_queries(Rc::new(VecModel::from(queries)).into());
        pb.set_nested_row_anki_tags(Rc::new(VecModel::from(tags)).into());
        pb.set_nested_revision(pb.get_nested_revision().wrapping_add(1));
        if nav_changed {
            pb.set_nested_nav_revision(pb.get_nested_nav_revision().wrapping_add(1));
        }
        if depth == 0 {
            pb.set_nested_searching(false);
        }
    }

    fn pop_nested_popup(&self, app: &MementoApp) {
        self.nested_epoch.fetch_add(1, Ordering::AcqRel);
        self.nested_poll_timer.stop();
        let popped = {
            let mut st = self.state.borrow_mut();
            st.nested_scan_timer.stop();
            st.nested_dismiss_timer.stop();
            let popped = st.nested_stack.pop().is_some();
            if let Some(top) = st.nested_stack.last_mut() {
                top.scan_highlight = None;
            } else {
                st.root_scan_highlight = None;
                st.nested_card_hovered = false;
                st.nested_body_hovered = false;
                st.pending_nested_anki_add = None;
            }
            popped
        };
        if popped {
            let pb = app.global::<PopupBridge>();
            pb.set_tooltip_visible(false);
            pb.set_nested_searching(false);
            self.sync_nested_popup_to_bridge(app, true);
            pb.set_rich_revision(pb.get_rich_revision().wrapping_add(1));
        }
    }

    fn dismiss_all_nested_popups(&self, app: &MementoApp) {
        self.nested_epoch.fetch_add(1, Ordering::AcqRel);
        self.nested_poll_timer.stop();
        let had_any = {
            let mut st = self.state.borrow_mut();
            st.nested_scan_timer.stop();
            st.nested_dismiss_timer.stop();
            let had_any = !st.nested_stack.is_empty() || st.root_scan_highlight.is_some();
            st.nested_stack.clear();
            st.root_scan_highlight = None;
            st.nested_card_hovered = false;
            st.nested_body_hovered = false;
            st.pending_nested_anki_add = None;
            had_any
        };
        let pb = app.global::<PopupBridge>();
        pb.set_nested_searching(false);
        if had_any {
            pb.set_tooltip_visible(false);
            self.sync_nested_popup_to_bridge(app, true);
            pb.set_rich_revision(pb.get_rich_revision().wrapping_add(1));
        }
    }

    fn schedule_unlocked_nested_dismiss(self: &Rc<Self>) {
        let should_schedule = {
            let st = self.state.borrow();
            let Some(top) = st.nested_stack.last() else {
                return;
            };
            if top.locked || st.nested_card_hovered || st.nested_body_hovered {
                false
            } else {
                let parent_depth = (st.nested_stack.len() - 1) as i32;
                let still_on_source = st.hovered_glossary_target.as_ref().is_some_and(|t| {
                    t.depth == parent_depth
                        && t.row_index == top.source_row_index
                        && t.start_char == top.source_start_char
                });
                !still_on_source
            }
        };
        if !should_schedule {
            return;
        }
        let weak_br = Rc::downgrade(self);
        self.state.borrow_mut().nested_dismiss_timer.start(
            slint::TimerMode::SingleShot,
            std::time::Duration::from_millis(NESTED_DISMISS_GRACE_MS),
            move || {
                let Some(br) = weak_br.upgrade() else {
                    return;
                };
                let Some(app) = br.app_weak.upgrade() else {
                    return;
                };
                let dismiss_now = {
                    let st = br.state.borrow();
                    let Some(top) = st.nested_stack.last() else {
                        return;
                    };
                    if top.locked || st.nested_card_hovered || st.nested_body_hovered {
                        false
                    } else {
                        let parent_depth = (st.nested_stack.len() - 1) as i32;
                        let still_on_source = st.hovered_glossary_target.as_ref().is_some_and(|t| {
                            t.depth == parent_depth
                                && t.row_index == top.source_row_index
                                && t.start_char == top.source_start_char
                        });
                        !still_on_source
                    }
                };
                if dismiss_now {
                    br.pop_nested_popup(&app);
                }
            },
        );
    }

    fn dispatch_nested_scan(
        self: &Rc<Self>,
        app: &MementoApp,
        target: HoveredGlossaryTarget,
        locked: bool,
    ) {
        if target.depth < 0 || (target.depth as usize) >= MAX_NESTED_POPUP_DEPTH {
            return;
        }
        let (order, excluded, mecab_enabled) = {
            let st = self.state.borrow();
            st.nested_scan_timer.stop();
            st.nested_dismiss_timer.stop();
            (
                st.settings.dict_order.clone(),
                st.settings.excluded_glossaries.clone(),
                st.settings.mecab_enabled,
            )
        };
        let epoch = self.nested_epoch.fetch_add(1, Ordering::AcqRel) + 1;
        app.global::<PopupBridge>().set_nested_searching(true);
        let _ = self.nested_tx.send(NestedScanRequest {
            epoch,
            parent_depth: target.depth,
            row_index: target.row_index,
            start_char: target.start_char,
            query: target.query,
            cell_indices: target.cell_indices,
            anchor_x: target.anchor_x,
            anchor_y: target.anchor_y,
            locked,
            dict_order: order,
            excluded_glossaries: excluded,
            mecab_enabled,
        });
        let weak_br = Rc::downgrade(self);
        self.nested_poll_timer.start(
            slint::TimerMode::Repeated,
            std::time::Duration::from_millis(8),
            move || {
                let Some(br) = weak_br.upgrade() else {
                    return;
                };
                let Some(app) = br.app_weak.upgrade() else {
                    br.nested_poll_timer.stop();
                    return;
                };
                let _ = br.drain_nested_scan_results(&app);
                if !app.global::<PopupBridge>().get_nested_searching() {
                    br.nested_poll_timer.stop();
                }
            },
        );
    }

    fn drain_nested_scan_results(self: &Rc<Self>, app: &MementoApp) -> bool {
        let mut latest: Option<NestedScanResponse> = None;
        if let Ok(guard) = self.nested_res_rx.lock() {
            while let Ok(item) = guard.try_recv() {
                latest = Some(item);
            }
        }
        let Some(resp) = latest else {
            return false;
        };
        if resp.epoch != self.nested_epoch.load(Ordering::Acquire) {
            return false;
        }
        let pb = app.global::<PopupBridge>();
        pb.set_nested_searching(false);
        if !app.get_dict_open() {
            return false;
        }
        if resp.hits.is_empty() {
            return true;
        }

        let (mut rows, grouped_hits) =
            build_dictionary_result_rows_grouped(&resp.hits, &resp.excluded_glossaries);
        for row in &mut rows {
            if row.term_start && !row.is_kanji {
                if let Some((_, _, graphs, freqs)) = resp.term_meta_rows.iter().find(|(e, r, _, _)| {
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

        let new_depth_opt = {
            let mut st = self.state.borrow_mut();
            let parent_depth = resp.parent_depth.max(0) as usize;
            if parent_depth > st.nested_stack.len() || parent_depth >= MAX_NESTED_POPUP_DEPTH {
                None
            } else {
                st.nested_stack.truncate(parent_depth);
                let hl_count = resp
                    .matched_char_count
                    .clamp(1, resp.cell_indices.len().max(1));
                let highlight = GlossaryScanHighlight {
                    row_index: resp.row_index,
                    cell_indices: resp.cell_indices.into_iter().take(hl_count).collect(),
                };
                if parent_depth == 0 {
                    st.root_scan_highlight = Some(highlight);
                } else if let Some(parent_lvl) = st.nested_stack.get_mut(parent_depth - 1) {
                    parent_lvl.scan_highlight = Some(highlight);
                    parent_lvl.locked = true;
                }
                let state_ref = &mut *st;
                let row_extras = build_row_extras_for_grouped(
                    &mut state_ref.db_helper,
                    &mut state_ref.dict_ctrl,
                    &mut rows,
                    &grouped_hits,
                    Some(&resp.term_pitch_details),
                );
                let row_count = rows.len();
                let level = NestedPopupLevel {
                    label: resp.headword.clone(),
                    rows: rows.clone(),
                    headword: resp.headword,
                    reading: resp.reading,
                    anchor_x: resp.anchor_x,
                    anchor_y: resp.anchor_y,
                    locked: resp.locked,
                    source_row_index: resp.row_index,
                    source_start_char: resp.start_char,
                    rich_epoch: 1,
                    row_extras,
                    glossary_cache: HashMap::new(),
                    last_rendered_key: HashMap::new(),
                    selection: None,
                    scan_highlight: None,
                    expanded_cards: HashSet::new(),
                    row_anki_duplicates: vec![false; row_count],
                    row_anki_note_ids_i64: vec![0; row_count],
                    row_anki_browse_queries: vec![SharedString::default(); row_count],
                    row_anki_tags: vec![SharedString::default(); row_count],
                };
                st.nested_stack.push(level);
                Some(st.nested_stack.len())
            }
        };

        if let Some(new_depth) = new_depth_opt {
            self.sync_nested_popup_to_bridge(app, true);
            pb.set_rich_revision(pb.get_rich_revision().wrapping_add(1));
            self.dispatch_anki_check_for_rows(app, &rows, Some(new_depth));
        }
        true
    }

    #[allow(clippy::too_many_arguments)]
    fn handle_glossary_scan_hover(
        self: &Rc<Self>,
        app: &MementoApp,
        depth: i32,
        row_idx: i32,
        x: f32,
        y: f32,
        popup_x: f32,
        popup_y: f32,
        control: bool,
        shift: bool,
        alt: bool,
        meta: bool,
    ) {
        self.ensure_fresh_db_and_config(app);
        let pb = app.global::<PopupBridge>();
        let Some(doc) = self.latest_cached_doc_at_depth(depth, row_idx) else {
            pb.set_tooltip_visible(false);
            return;
        };

        if let Some(zone) = doc.hit_zones.iter().rev().find(|z| {
            z.tooltip.is_some() && x >= z.x && x <= z.x + z.w && y >= z.y && y <= z.y + z.h
        }) {
            if let Some(ref tip) = zone.tooltip {
                pb.set_tooltip_text(tip.clone().into());
                pb.set_tooltip_x(popup_x);
                pb.set_tooltip_y(popup_y);
                pb.set_tooltip_visible(true);
            }
        } else {
            pb.set_tooltip_visible(false);
        }

        // Never hijack explicit links / <details> disclosure headers or active drag-selection.
        let over_action_zone = doc.hit_zones.iter().any(|z| {
            z.action.is_some() && x >= z.x && x <= z.x + z.w && y >= z.y && y <= z.y + z.h
        });
        let is_dragging = {
            let st = self.state.borrow();
            if depth == 0 {
                st.selection.as_ref().is_some_and(|s| s.dragging)
            } else {
                st.nested_level(depth)
                    .and_then(|lvl| lvl.selection.as_ref())
                    .is_some_and(|s| s.dragging)
            }
        };
        if over_action_zone || is_dragging || depth < 0 || (depth as usize) >= MAX_NESTED_POPUP_DEPTH
        {
            let mut st = self.state.borrow_mut();
            st.hovered_glossary_target = None;
            st.nested_scan_timer.stop();
            return;
        }

        let scan_match = strict_char_index_at_point(&doc, x, y).and_then(|ci| {
            extract_glossary_scan_substring(&doc.selectable_chars, ci, GLOSSARY_SCAN_MAX_CHARS)
                .map(|(q, indices)| (ci, q, indices))
        });

        let Some((char_idx, query, cell_indices)) = scan_match else {
            {
                let mut st = self.state.borrow_mut();
                st.hovered_glossary_target = None;
                st.nested_scan_timer.stop();
            }
            self.schedule_unlocked_nested_dismiss();
            return;
        };

        let target = HoveredGlossaryTarget {
            depth,
            row_index: row_idx,
            start_char: char_idx,
            query,
            cell_indices,
            anchor_x: popup_x,
            anchor_y: popup_y,
        };

        let (method, mod_setting, delay_ms, already_showing_this) = {
            let mut st = self.state.borrow_mut();
            st.hovered_glossary_target = Some(target.clone());
            let already = st.nested_stack.get(depth as usize).is_some_and(|child| {
                child.source_row_index == row_idx && child.source_start_char == char_idx
            });
            if already {
                st.nested_dismiss_timer.stop();
            }
            (
                st.settings.search_method,
                st.settings.search_modifier,
                st.settings.search_delay_ms,
                already,
            )
        };

        let mod_held = modifier_matches_setting(mod_setting, control, shift, alt, meta);
        if method == 1 && !mod_held {
            self.state.borrow_mut().nested_scan_timer.stop();
            return;
        }
        if already_showing_this {
            self.state.borrow_mut().nested_scan_timer.stop();
            return;
        }

        let wait_ms = if mod_held { delay_ms.min(50) } else { delay_ms };
        if wait_ms == 0 {
            self.dispatch_nested_scan(app, target, false);
            return;
        }

        let expected_depth = depth;
        let expected_row = row_idx;
        let expected_char = char_idx;
        let weak_br = Rc::downgrade(self);
        self.state.borrow_mut().nested_scan_timer.start(
            slint::TimerMode::SingleShot,
            std::time::Duration::from_millis(wait_ms),
            move || {
                let Some(br) = weak_br.upgrade() else {
                    return;
                };
                let Some(app) = br.app_weak.upgrade() else {
                    return;
                };
                let current_opt = {
                    let st = br.state.borrow();
                    let dragging = if expected_depth == 0 {
                        st.selection.as_ref().is_some_and(|s| s.dragging)
                    } else {
                        st.nested_level(expected_depth)
                            .and_then(|l| l.selection.as_ref())
                            .is_some_and(|s| s.dragging)
                    };
                    if dragging {
                        None
                    } else {
                        st.hovered_glossary_target.clone()
                    }
                };
                if let Some(cur) = current_opt {
                    if cur.depth == expected_depth
                        && cur.row_index == expected_row
                        && cur.start_char == expected_char
                    {
                        br.dispatch_nested_scan(&app, cur, false);
                    }
                }
            },
        );
    }

    fn handle_glossary_scan_middle_click(
        self: &Rc<Self>,
        app: &MementoApp,
        depth: i32,
        row_idx: i32,
        x: f32,
        y: f32,
        popup_x: f32,
        popup_y: f32,
    ) {
        self.ensure_fresh_db_and_config(app);
        let enabled = self.state.borrow().settings.middle_mouse_scan;
        if !enabled || depth < 0 || (depth as usize) >= MAX_NESTED_POPUP_DEPTH {
            return;
        }
        let Some(doc) = self.latest_cached_doc_at_depth(depth, row_idx) else {
            return;
        };
        let Some(char_idx) = strict_char_index_at_point(&doc, x, y) else {
            return;
        };
        let Some((query, cell_indices)) =
            extract_glossary_scan_substring(&doc.selectable_chars, char_idx, GLOSSARY_SCAN_MAX_CHARS)
        else {
            return;
        };
        let target = HoveredGlossaryTarget {
            depth,
            row_index: row_idx,
            start_char: char_idx,
            query,
            cell_indices,
            anchor_x: popup_x,
            anchor_y: popup_y,
        };
        self.state.borrow_mut().hovered_glossary_target = Some(target.clone());
        self.dispatch_nested_scan(app, target, true);
    }

    fn handle_glossary_scan_leave(self: &Rc<Self>, depth: i32, row_idx: i32) {
        {
            let mut st = self.state.borrow_mut();
            if st
                .hovered_glossary_target
                .as_ref()
                .is_some_and(|t| t.depth == depth && t.row_index == row_idx)
            {
                st.hovered_glossary_target = None;
                st.nested_scan_timer.stop();
            }
        }
        self.schedule_unlocked_nested_dismiss();
    }

    fn handle_glossary_pointer_down_at_depth(
        &self,
        app: &MementoApp,
        depth: i32,
        row_idx: i32,
        x: f32,
        y: f32,
    ) {
        // Clicking/dragging immediately cancels any pending hover-scan timer or in-flight scan.
        self.nested_epoch.fetch_add(1, Ordering::AcqRel);
        self.nested_poll_timer.stop();
        let mut truncated_nested = false;
        let mut locked_nested = false;
        {
            let mut st = self.state.borrow_mut();
            st.nested_scan_timer.stop();
            if depth >= 0 && (depth as usize) < st.nested_stack.len() {
                st.nested_stack.truncate(depth as usize);
                if depth == 0 {
                    st.root_scan_highlight = None;
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

    fn handle_glossary_pointer_drag_at_depth(
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
                            let target =
                                if x > doc.selectable_chars[ci].x + doc.selectable_chars[ci].w * 0.5
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

    fn handle_glossary_pointer_up_at_depth(
        self: &Rc<Self>,
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
                        if !self.handle_internal_link_str_at_depth(app, depth, row_idx, href) {
                            app.invoke_open_glossary_link(href.clone().into());
                        }
                    }
                    None => {}
                }
            }
        }
    }

    fn handle_glossary_double_click_at_depth(
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

    fn navigate_nested_query(self: &Rc<Self>, app: &MementoApp, from_depth: i32, query: &str) {
        let trimmed = query.trim();
        if trimmed.is_empty() {
            return;
        }
        self.ensure_fresh_db_and_config(app);
        let (hits, excluded, headword, reading, term_meta_rows) = {
            let mut st = self.state.borrow_mut();
            let mut hits = st.dict_ctrl.search(trimmed).to_vec();
            let excluded = st.settings.excluded_glossaries.clone();
            if hits.is_empty() && trimmed.chars().count() == 1 {
                hits = st.dict_ctrl.search_kanji(trimmed).to_vec();
            }
            let headword = hits
                .first()
                .map(|h| h.expression.clone())
                .unwrap_or_else(|| trimmed.to_owned());
            let reading = hits.first().map(|h| h.reading.clone()).unwrap_or_default();
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
            (hits, excluded, headword, reading, term_meta_rows)
        };
        if hits.is_empty() {
            app.set_status_message(format!("No dictionary entries found for {trimmed}").into());
            return;
        }
        let (mut rows, grouped_hits) = build_dictionary_result_rows_grouped(&hits, &excluded);
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
        let target_depth = {
            let mut st = self.state.borrow_mut();
            let (anchor_x, anchor_y) = st
                .nested_level(from_depth)
                .map(|l| (l.anchor_x, l.anchor_y))
                .unwrap_or((80.0, 80.0));
            let state_ref = &mut *st;
            let row_extras = build_row_extras_for_grouped(
                &mut state_ref.db_helper,
                &mut state_ref.dict_ctrl,
                &mut rows,
                &grouped_hits,
                None,
            );
            let row_count = rows.len();
            let level = NestedPopupLevel {
                label: headword.clone(),
                rows: rows.clone(),
                headword,
                reading,
                anchor_x,
                anchor_y,
                locked: true,
                source_row_index: 0,
                source_start_char: 0,
                rich_epoch: 1,
                row_extras,
                glossary_cache: HashMap::new(),
                last_rendered_key: HashMap::new(),
                selection: None,
                scan_highlight: None,
                expanded_cards: HashSet::new(),
                row_anki_duplicates: vec![false; row_count],
                row_anki_note_ids_i64: vec![0; row_count],
                row_anki_browse_queries: vec![SharedString::default(); row_count],
                row_anki_tags: vec![SharedString::default(); row_count],
            };
            let base = (from_depth.max(0) as usize).min(st.nested_stack.len());
            st.nested_stack.truncate(base);
            if st.nested_stack.len() >= MAX_NESTED_POPUP_DEPTH {
                st.nested_stack.pop();
            }
            st.nested_stack.push(level);
            st.nested_stack.len()
        };
        self.sync_nested_popup_to_bridge(app, true);
        self.dispatch_anki_check_for_rows(app, &rows, Some(target_depth));
    }

    fn navigate_nested_kanji(
        self: &Rc<Self>,
        app: &MementoApp,
        from_depth: i32,
        expression: &str,
        clicked_kanji: &str,
    ) {
        let target_char = clicked_kanji
            .chars()
            .find(|&c| is_kanji_char(c))
            .or_else(|| expression.chars().find(|&c| is_kanji_char(c)));
        let Some(ch) = target_char else {
            return;
        };
        let query = ch.to_string();
        self.ensure_fresh_db_and_config(app);
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
        let (mut rows, grouped_hits) = build_dictionary_result_rows_grouped(&hits, &excluded);
        let reading = hits.first().map(|h| h.reading.clone()).unwrap_or_default();
        let target_depth = {
            let mut st = self.state.borrow_mut();
            let (anchor_x, anchor_y) = st
                .nested_level(from_depth)
                .map(|l| (l.anchor_x, l.anchor_y))
                .unwrap_or((80.0, 80.0));
            let state_ref = &mut *st;
            let row_extras = build_row_extras_for_grouped(
                &mut state_ref.db_helper,
                &mut state_ref.dict_ctrl,
                &mut rows,
                &grouped_hits,
                None,
            );
            let row_count = rows.len();
            let level = NestedPopupLevel {
                label: query.clone(),
                rows: rows.clone(),
                headword: query,
                reading,
                anchor_x,
                anchor_y,
                locked: true,
                source_row_index: 0,
                source_start_char: 0,
                rich_epoch: 1,
                row_extras,
                glossary_cache: HashMap::new(),
                last_rendered_key: HashMap::new(),
                selection: None,
                scan_highlight: None,
                expanded_cards: HashSet::new(),
                row_anki_duplicates: vec![false; row_count],
                row_anki_note_ids_i64: vec![0; row_count],
                row_anki_browse_queries: vec![SharedString::default(); row_count],
                row_anki_tags: vec![SharedString::default(); row_count],
            };
            let base = (from_depth.max(0) as usize).min(st.nested_stack.len());
            st.nested_stack.truncate(base);
            if st.nested_stack.len() >= MAX_NESTED_POPUP_DEPTH {
                st.nested_stack.pop();
            }
            st.nested_stack.push(level);
            st.nested_stack.len()
        };
        self.sync_nested_popup_to_bridge(app, true);
        self.dispatch_anki_check_for_rows(app, &rows, Some(target_depth));
    }

    fn is_nested_target_already_shown(&self, target: &HoveredGlossaryTarget) -> bool {
        if target.depth < 0 {
            return false;
        }
        let st = self.state.borrow();
        st.nested_stack
            .get(target.depth as usize)
            .is_some_and(|child| {
                child.source_row_index == target.row_index
                    && child.source_start_char == target.start_char
            })
    }

    fn toggle_nested_select_definition(&self, app: &MementoApp, depth: i32, zero_based_idx: i32) {
        if zero_based_idx < 0 {
            return;
        }
        let mut changed = false;
        {
            let mut st = self.state.borrow_mut();
            if let Some(lvl) = st.nested_level_mut(depth) {
                lvl.locked = true;
                if let Some(row) = lvl.rows.get_mut(zero_based_idx as usize) {
                    row.selected = !row.selected;
                    changed = true;
                }
            }
        }
        if changed {
            self.sync_nested_popup_to_bridge(app, false);
        }
    }

    fn toggle_nested_term_collapse(&self, app: &MementoApp, depth: i32, term_start_zero_idx: i32) {
        if term_start_zero_idx < 0 {
            return;
        }
        let mut changed = false;
        {
            let mut st = self.state.borrow_mut();
            if let Some(lvl) = st.nested_level_mut(depth) {
                lvl.locked = true;
                let start = term_start_zero_idx as usize;
                if start < lvl.rows.len() {
                    let target_collapsed = !lvl.rows[start].collapsed;
                    lvl.rows[start].collapsed = target_collapsed;
                    changed = true;
                    for row in lvl.rows.iter_mut().skip(start + 1) {
                        if row.term_start {
                            break;
                        }
                        row.collapsed = target_collapsed;
                    }
                }
            }
        }
        if changed {
            self.sync_nested_popup_to_bridge(app, false);
        }
    }

    fn add_nested_anki_note(&self, app: &MementoApp, row_zero_idx: i32) {
        let (depth, nested_rows, pending) = {
            let mut st = self.state.borrow_mut();
            let depth = st.nested_stack.len();
            let Some(top) = st.nested_stack.last_mut() else {
                return;
            };
            top.locked = true;
            let idx = row_zero_idx.max(0) as usize;
            let Some(target) = top.rows.get(idx) else {
                return;
            };
            (
                depth,
                top.rows.clone(),
                (
                    depth,
                    target.expression.to_string(),
                    target.reading.to_string(),
                    target.is_kanji,
                ),
            )
        };
        let parent_rows = app.get_dict_results();
        self.state.borrow_mut().pending_nested_anki_add = Some(pending);
        app.set_dict_results(Rc::new(VecModel::from(nested_rows)).into());
        app.invoke_add_anki_note(row_zero_idx);
        app.set_dict_results(parent_rows);
        if depth > 0 {
            app.global::<PopupBridge>().set_nested_locked(true);
        }
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
        app.set_popup_offset_x(app.get_pending_popup_offset_x());
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
    let (rows, _) = build_dictionary_result_rows_grouped(&hits, &excluded);
    let pb = app.global::<PopupBridge>();
    pb.set_panel_results(Rc::new(VecModel::from(rows)).into());
    pb.set_panel_searching(false);
    true
}

fn drain_nested_scan_results_for_app(app: &MementoApp) -> bool {
    let instance_id = app.global::<PopupBridge>().get_bridge_instance_id();
    let bridge = ACTIVE_RICH_BRIDGES.with(|slot| {
        slot.borrow()
            .iter()
            .find(|b| b.instance_id == instance_id)
            .cloned()
    });
    if let Some(br) = bridge {
        br.drain_nested_scan_results(app)
    } else {
        false
    }
}

fn drain_anki_results(
    app: &MementoApp,
    rx: &std::sync::Mutex<mpsc::Receiver<AnkiPopupCheckResponse>>,
    nav_epoch: &AtomicU64,
    nested_nav_epoch: &AtomicU64,
    external_lookup_epoch: &std::sync::Mutex<Option<Arc<AtomicU64>>>,
) -> bool {
    let mut latest_root: Option<AnkiPopupCheckResponse> = None;
    let mut latest_nested: Option<AnkiPopupCheckResponse> = None;
    if let Ok(guard) = rx.lock() {
        while let Ok(item) = guard.try_recv() {
            if item.target_nested_depth.is_some() {
                latest_nested = Some(item);
            } else {
                latest_root = Some(item);
            }
        }
    }
    if latest_root.is_none() && latest_nested.is_none() {
        return false;
    }

    let current_ext = current_external_lookup_epoch(external_lookup_epoch);
    let pb = app.global::<PopupBridge>();
    let instance_id = pb.get_bridge_instance_id();
    let active_bridge = ACTIVE_RICH_BRIDGES.with(|slot| {
        slot.borrow()
            .iter()
            .find(|b| b.instance_id == instance_id)
            .cloned()
    });
    let mut applied_any = false;

    if let Some(resp) = latest_root {
        if resp.nav_epoch == nav_epoch.load(Ordering::Acquire) {
            pb.set_anki_checking(false);
            if resp.lookup_epoch == current_ext && app.get_dict_open() {
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
                if let Some(ref br) = active_bridge {
                    br.state.borrow_mut().root_row_anki_note_ids_i64 = note_ids_i64;
                }
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
    }

    if let Some(resp) = latest_nested {
        if resp.nav_epoch == nested_nav_epoch.load(Ordering::Acquire) {
            pb.set_anki_checking(false);
            if resp.lookup_epoch == current_ext && app.get_dict_open() {
                if let (Some(depth), Some(br)) = (resp.target_nested_depth, active_bridge.as_ref()) {
                    let sync_needed = {
                        let mut st = br.state.borrow_mut();
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
                        br.sync_nested_popup_to_bridge(app, false);
                        applied_any = true;
                    }
                }
            }
        }
    }

    applied_any
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
    let initial_sub_bg = settings.subtitle_style.bg_rgba;
    let initial_anki_show_tag = settings.anki_show_tag;
    let dict_resource_dirs = collect_dictionary_resource_dirs(config_dir, dictionary_db_path);
    let db_sig = crate::controller::dict::db_file_fingerprint(dictionary_db_path);
    let mut dict_ctrl = DictController::open(dictionary_db_path).unwrap_or_default();
    dict_ctrl.set_order(&settings.dict_order);
    dict_ctrl.set_mecab_enabled(settings.mecab_enabled);

    // Independent async worker thread for DictionarySearchPanel so panel searches NEVER
    // block the UI thread and NEVER overwrite or get overwritten by subtitle hover popups!
    let (panel_tx, panel_rx) =
        mpsc::channel::<(u64, String, bool, Vec<i64>, Vec<String>, bool)>();
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
            let (gen_id, query, kana_only, order, excluded, mecab_enabled) = req;
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
            dict.set_mecab_enabled(mecab_enabled);
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

    // Independent async worker thread for recursive nested glossary hover-scans (depth 1..=4).
    let (nested_tx, nested_rx) = mpsc::channel::<NestedScanRequest>();
    let (nested_res_tx, raw_nested_res_rx) = mpsc::channel::<NestedScanResponse>();
    let nested_res_rx = Arc::new(std::sync::Mutex::new(raw_nested_res_rx));
    let nested_epoch = Arc::new(AtomicU64::new(0));
    let worker_nested_epoch = nested_epoch.clone();
    let worker_nested_db_path = dictionary_db_path.to_path_buf();
    let worker_nested_app = app.as_weak();
    std::thread::spawn(move || {
        let mut sig = crate::controller::dict::db_file_fingerprint(&worker_nested_db_path);
        let mut dict = DictController::open(&worker_nested_db_path).unwrap_or_default();
        while let Ok(mut req) = nested_rx.recv() {
            while let Ok(newer) = nested_rx.try_recv() {
                req = newer;
            }
            if req.epoch != worker_nested_epoch.load(Ordering::Acquire) {
                continue;
            }
            let cur_sig = crate::controller::dict::db_file_fingerprint(&worker_nested_db_path);
            if cur_sig != sig {
                sig = cur_sig;
                dict = DictController::open(&worker_nested_db_path).unwrap_or_default();
            }
            dict.set_order(&req.dict_order);
            dict.set_mecab_enabled(req.mecab_enabled);

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

    // Background Anki duplicate & `show-tag` checker thread (#340, #346) so AnkiConnect
    // round-trips never block subtitle hover latency and fail silently when Anki is closed.
    let (anki_tx, anki_rx) = mpsc::channel::<AnkiPopupCheckRequest>();
    let (anki_res_tx, raw_anki_res_rx) = mpsc::channel::<AnkiPopupCheckResponse>();
    let anki_res_rx = Arc::new(std::sync::Mutex::new(raw_anki_res_rx));
    let anki_nav_epoch = Arc::new(AtomicU64::new(0));
    let nested_anki_nav_epoch = Arc::new(AtomicU64::new(0));
    let external_lookup_epoch: Arc<std::sync::Mutex<Option<Arc<AtomicU64>>>> =
        Arc::new(std::sync::Mutex::new(None));
    let worker_anki_epoch = anki_nav_epoch.clone();
    let worker_nested_anki_epoch = nested_anki_nav_epoch.clone();
    let worker_anki_ext = external_lookup_epoch.clone();
    let worker_anki_res_rx = anki_res_rx.clone();
    let worker_anki_app = app.as_weak();
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
                let target_epoch = if req.target_nested_depth.is_some() {
                    &worker_nested_anki_epoch
                } else {
                    &worker_anki_epoch
                };
                if req.nav_epoch != target_epoch.load(Ordering::Acquire)
                    || req.lookup_epoch != current_external_lookup_epoch(&worker_anki_ext)
                {
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

                if req.nav_epoch != target_epoch.load(Ordering::Acquire)
                    || req.lookup_epoch != current_external_lookup_epoch(&worker_anki_ext)
                {
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

                if req.nav_epoch != target_epoch.load(Ordering::Acquire)
                    || req.lookup_epoch != current_external_lookup_epoch(&worker_anki_ext)
                {
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
                let rx_cl = worker_anki_res_rx.clone();
                let ep_cl = worker_anki_epoch.clone();
                let nep_cl = worker_nested_anki_epoch.clone();
                let ext_cl = worker_anki_ext.clone();
                let _ = worker_anki_app.upgrade_in_event_loop(move |app| {
                    let _ = drain_anki_results(&app, &rx_cl, &ep_cl, &nep_cl, &ext_cl);
                });
            }
        }
    });

    let instance_id = NEXT_BRIDGE_INSTANCE_ID.fetch_add(1, Ordering::Relaxed);
    let prev_instance_id = app.global::<PopupBridge>().get_bridge_instance_id();
    app.global::<PopupBridge>().set_bridge_instance_id(instance_id);

    let stylesheets = StylesheetCache::new(config_dir);
    let initial_popup_theme = stylesheets
        .theme()
        .with_interface_font_fallbacks(stylesheets.overrides(), &settings.popup_fonts);

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
            stylesheets,
            rich_epoch: 1,
            row_extras: HashMap::new(),
            current_hits: Vec::new(),
            glossary_cache: HashMap::new(),
            last_rendered_key: HashMap::new(),
            selection: None,
            root_scan_highlight: None,
            root_expanded_cards: HashSet::new(),
            root_row_anki_note_ids_i64: Vec::new(),
            suppress_history_reset: false,
            back_stack: Vec::new(),
            forward_stack: Vec::new(),
            nested_stack: Vec::new(),
            hovered_glossary_target: None,
            nested_card_hovered: false,
            nested_body_hovered: false,
            nested_scan_timer: slint::Timer::default(),
            nested_dismiss_timer: slint::Timer::default(),
            pending_nested_anki_add: None,
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
        anki_tx,
        anki_res_rx,
        anki_poll_timer: slint::Timer::default(),
        anki_nav_epoch,
        nested_anki_nav_epoch,
        external_lookup_epoch,
        nested_tx,
        nested_res_rx,
        nested_poll_timer: slint::Timer::default(),
        nested_epoch,
    });

    ACTIVE_RICH_BRIDGES.with(|slot| {
        let mut list = slot.borrow_mut();
        list.retain(|b| b.app_weak.upgrade().is_some() && b.instance_id != prev_instance_id);
        list.push(bridge.clone());
    });

    // ── Wire PopupBridge ────────────────────────────────────────────────────
    let pb = app.global::<PopupBridge>();
    pb.set_rich_enabled(true);
    pb.set_anki_show_tags(initial_anki_show_tag);
    apply_popup_css_theme_to_bridge(app, &initial_popup_theme);
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
        b_act_link.handle_internal_link_str_at_depth(&app, 0, 1, href.as_str())
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
            st.nested_body_hovered = hovered;
            if hovered {
                st.nested_dismiss_timer.stop();
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
        use copypasta::ClipboardProvider;
        if let Ok(mut ctx) = copypasta::ClipboardContext::new() {
            let _ = ctx.set_contents(text);
        }
    });

    let b_pkey = bridge.clone();
    pb.on_popup_key_pressed(move |text, control, shift, alt, meta| {
        if let Some((mc, ms, ma, mm)) = is_modifier_only_key_text(text.as_str()) {
            let c = control || mc;
            let s = shift || ms;
            let a = alt || ma;
            let m = meta || mm;
            if let Some(app) = b_pkey.app_weak.upgrade() {
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
        b_panel_search.ensure_fresh_db_and_config(&app);
        let pb = app.global::<PopupBridge>();
        pb.set_panel_query(query.clone());
        let kana_only = pb.get_panel_kana_only();
        let (order, excluded, mecab_enabled) = {
            let st = b_panel_search.state.borrow();
            (
                st.settings.dict_order.clone(),
                st.settings.excluded_glossaries.clone(),
                st.settings.mecab_enabled,
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
            mecab_enabled,
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
    sb.set_subtitle_bg_color(slint::Color::from_argb_u8(
        initial_sub_bg[3],
        initial_sub_bg[0],
        initial_sub_bg[1],
        initial_sub_bg[2],
    ));
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
            let mut sel_join_left = false;
            let mut sel_join_right = false;

            // Check whether this token has an adjacent token on the same visual subtitle line
            // so wrapped line boundaries keep neat rounded corners instead of overhanging.
            let (has_line_prev, has_line_next) = if let Some(ref app) = app_opt {
                let lines = app.get_active_sub_lines();
                let mut found = (false, false);
                for li in 0..lines.row_count() {
                    if let Some(line) = lines.row_data(li) {
                        let count = line.words.row_count();
                        for wi in 0..count {
                            if line.words.row_data(wi).is_some_and(|w| w.index == word.index) {
                                found = (wi > 0, wi + 1 < count);
                                break;
                            }
                        }
                    }
                }
                found
            } else {
                (false, false)
            };

            if let (Some((hl_start, hl_end)), Some(app)) = (scan_range, app_opt.as_ref()) {
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
                        sel_join_left = has_line_prev && hl_start < token_u16_start;
                        sel_join_right = has_line_next && hl_end > token_u16_end;
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
                    let s_idx = app.get_selected_token_index();
                    let s_cnt = app.get_selected_token_count();
                    sel_join_left = has_line_prev && word.index > s_idx;
                    sel_join_right = has_line_next && word.index + 1 < s_idx + s_cnt;
                }
            } else if token_selected {
                has_sel = true;
                if let Some(ref app) = app_opt {
                    let s_idx = app.get_selected_token_index();
                    let s_cnt = app.get_selected_token_count();
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
        let (mod_setting, cluster_opt, gloss_target_opt) = {
            let st = b_stat_mod.state.borrow();
            (
                st.settings.search_modifier,
                st.hovered_cluster.clone(),
                st.hovered_glossary_target.clone(),
            )
        };
        if modifier_matches_setting(mod_setting, control, shift, alt, meta) {
            if let Some(cluster) = cluster_opt {
                b_stat_mod.trigger_subtitle_scan_now(&app, cluster);
            } else if app.get_dict_open() {
                if let Some(target) = gloss_target_opt {
                    if !b_stat_mod.is_nested_target_already_shown(&target) {
                        b_stat_mod.dispatch_nested_scan(&app, target, false);
                    }
                }
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
    let (middle_mouse, sub_bg, popup_theme) = {
        let mut st = bridge.state.borrow_mut();
        st.settings.apply_controller(config);
        st.stylesheets.force_reload_user_css();
        let sig = crate::controller::dict::db_file_fingerprint(&bridge.db_path);
        if sig != st.db_sig {
            st.db_sig = sig;
            st.dict_ctrl = DictController::open(&bridge.db_path).unwrap_or_default();
        }
        let order = st.settings.dict_order.clone();
        let mecab_enabled = st.settings.mecab_enabled;
        st.dict_ctrl.set_order(&order);
        st.dict_ctrl.set_mecab_enabled(mecab_enabled);
        st.db_helper.refresh_if_needed();
        st.clear_all_glossary_caches();
        (
            st.settings.middle_mouse_scan,
            st.settings.subtitle_style.bg_rgba,
            st.effective_popup_theme(),
        )
    };
    apply_popup_css_theme_to_bridge(app, &popup_theme);
    let pb = app.global::<PopupBridge>();
    pb.set_anki_show_tags(config.anki_show_tag());
    pb.set_rich_revision(pb.get_rich_revision().wrapping_add(1));
    if pb.get_nested_depth() > 0 {
        pb.set_nested_revision(pb.get_nested_revision().wrapping_add(1));
    }
    if app.get_dict_open() {
        let full = app.get_dict_results();
        let current_rows: Vec<DictResult> =
            (0..full.row_count()).filter_map(|i| full.row_data(i)).collect();
        bridge.dispatch_anki_check_for_rows(app, &current_rows, None);
    }
    let sb = app.global::<SubtitleOverlayBridge>();
    sb.set_middle_mouse_scan_enabled(middle_mouse);
    sb.set_subtitle_bg_color(slint::Color::from_argb_u8(
        sub_bg[3], sub_bg[0], sub_bg[1], sub_bg[2],
    ));
    sb.set_style_revision(sb.get_style_revision().wrapping_add(1));
    let words: Vec<SubtitleWord> = app.get_active_sub_words().iter().collect();
    if !words.is_empty() {
        let lines = subtitle_lines_for_app(app, words);
        app.set_active_sub_lines(Rc::new(VecModel::from(lines)).into());
    }
}

/// Binds `main.rs`'s `lookup_epoch` onto the active `RichUiBridge` so background Anki
/// duplicate/tag checks are cancelled immediately when a subtitle lookup is superseded or dismissed.
pub fn bind_rich_bridge_lookup_epoch(app: &MementoApp, lookup_epoch: Arc<AtomicU64>) {
    let instance_id = app.global::<PopupBridge>().get_bridge_instance_id();
    ACTIVE_RICH_BRIDGES.with(|slot| {
        if let Some(bridge) = slot.borrow().iter().find(|b| b.instance_id == instance_id) {
            if let Ok(mut guard) = bridge.external_lookup_epoch.lock() {
                *guard = Some(lookup_epoch);
            }
        }
    });
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
    let instance_id = pb.get_bridge_instance_id();
    let from_bridge = ACTIVE_RICH_BRIDGES.with(|slot| {
        slot.borrow()
            .iter()
            .find(|b| b.instance_id == instance_id)
            .and_then(|b| {
                b.state
                    .borrow()
                    .root_row_anki_note_ids_i64
                    .get(row_idx)
                    .copied()
                    .filter(|&id| id > 0)
            })
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
    let pb = app.global::<PopupBridge>();
    let instance_id = pb.get_bridge_instance_id();
    let from_bridge = ACTIVE_RICH_BRIDGES.with(|slot| {
        slot.borrow()
            .iter()
            .find(|b| b.instance_id == instance_id)
            .and_then(|b| {
                b.state
                    .borrow()
                    .nested_stack
                    .last()
                    .and_then(|lvl| lvl.row_anki_note_ids_i64.get(row_idx).copied())
                    .filter(|&id| id > 0)
            })
    });
    if from_bridge.is_some() {
        return from_bridge;
    }
    pb.get_nested_row_anki_note_ids()
        .row_data(row_idx)
        .filter(|&id| id > 0)
        .map(i64::from)
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
    let pb = app.global::<PopupBridge>();
    let instance_id = pb.get_bridge_instance_id();
    let active_bridge = ACTIVE_RICH_BRIDGES.with(|slot| {
        slot.borrow()
            .iter()
            .find(|b| b.instance_id == instance_id)
            .cloned()
    });

    let is_same_deck = matches!(
        crate::controller::anki::AnkiDuplicatePolicy::try_from(profile.duplicate_policy),
        Ok(crate::controller::anki::AnkiDuplicatePolicy::SameDeck)
    );
    let nid_query = format!("nid:{note_id}");
    let joined_profile_tags = profile.tags.join(" ");

    // If this add was triggered from a nested child popup, update that nested level instead of root.
    if let Some(ref br) = active_bridge {
        let updated_nested = {
            let mut st = br.state.borrow_mut();
            if let Some((depth, expr, reading, is_kanji)) = st.pending_nested_anki_add.take() {
                if let Some(lvl) = st.nested_level_mut(depth as i32) {
                    let matches_row = lvl.rows.get(added_index).is_some_and(|r| {
                        r.expression.as_str() == expr
                            && r.reading.as_str() == reading
                            && r.is_kanji == is_kanji
                    });
                    if matches_row {
                        let count = lvl.rows.len();
                        lvl.row_anki_duplicates.resize(count, false);
                        lvl.row_anki_note_ids_i64.resize(count, 0);
                        lvl.row_anki_browse_queries
                            .resize(count, SharedString::default());
                        lvl.row_anki_tags.resize(count, SharedString::default());
                        for idx in 0..count {
                            let row = &lvl.rows[idx];
                            if row.expression.as_str() == expr
                                && row.reading.as_str() == reading
                                && row.is_kanji == is_kanji
                            {
                                if !is_same_deck {
                                    lvl.row_anki_duplicates[idx] = true;
                                    lvl.row_anki_note_ids_i64[idx] = note_id;
                                    lvl.row_anki_browse_queries[idx] =
                                        SharedString::from(nid_query.as_str());
                                }
                                if show_tags && !joined_profile_tags.is_empty() {
                                    let t = &lvl.row_anki_tags[idx];
                                    let combined = if t.is_empty() {
                                        joined_profile_tags.clone()
                                    } else {
                                        format!("{t} {joined_profile_tags}")
                                    };
                                    let dedup: Vec<String> = split_anki_tags_str(&combined)
                                        .into_iter()
                                        .map(|s| s.to_string())
                                        .collect();
                                    lvl.row_anki_tags[idx] = SharedString::from(dedup.join(" "));
                                }
                            }
                        }
                        true
                    } else {
                        false
                    }
                } else {
                    false
                }
            } else {
                false
            }
        };
        if updated_nested {
            pb.set_anki_show_tags(show_tags);
            pb.set_last_added_was_nested(true);
            br.sync_nested_popup_to_bridge(app, false);
            return;
        }
    }

    let full = app.get_dict_results();
    let count = full.row_count();
    let Some(target) = full.row_data(added_index) else {
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
            if row.expression == target.expression
                && row.reading == target.reading
                && row.is_kanji == target.is_kanji
            {
                if !is_same_deck {
                    d = true;
                    nid64 = note_id;
                    nid32 = encode_note_id_i32(note_id);
                    nid_str = if note_id > 0 {
                        SharedString::from(note_id.to_string())
                    } else {
                        SharedString::default()
                    };
                    q = SharedString::from(nid_query.as_str());
                }
                if show_tags && !joined_profile_tags.is_empty() {
                    let combined = if t.is_empty() {
                        joined_profile_tags.clone()
                    } else {
                        format!("{t} {joined_profile_tags}")
                    };
                    let dedup: Vec<String> = split_anki_tags_str(&combined)
                        .into_iter()
                        .map(|s| s.to_string())
                        .collect();
                    t = SharedString::from(dedup.join(" "));
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
    pb.set_last_added_was_nested(false);
    pb.set_anki_show_tags(show_tags);
    pb.set_row_anki_duplicates(Rc::new(VecModel::from(dups)).into());
    pb.set_row_anki_note_ids(Rc::new(VecModel::from(note_ids_i32)).into());
    pb.set_row_anki_note_id_strings(Rc::new(VecModel::from(note_id_strings)).into());
    pb.set_row_anki_browse_queries(Rc::new(VecModel::from(queries)).into());
    pb.set_row_anki_tags(Rc::new(VecModel::from(tags)).into());
    pb.set_popup_revision(pb.get_popup_revision().wrapping_add(1));
    pb.set_rich_revision(pb.get_rich_revision().wrapping_add(1));
}

#[cfg(test)]
mod mecab_settings_tests {
    use super::*;

    #[test]
    fn cached_config_settings_tracks_mecab_ipadic_matcher_setting() {
        let dir = tempfile::TempDir::new().unwrap();
        let cfg = ConfigController::new();
        let cached = CachedConfigSettings::from_controller(&cfg, dir.path());
        assert_eq!(cached.mecab_enabled, raw::search::matcher::MECAB_IPADIC_DEFAULT);

        let key = format!("{}/{}", raw::search::GROUP, raw::search::matcher::MECAB_IPADIC);
        cfg.edit_setting(&key, "false").unwrap();
        let mut updated = CachedConfigSettings::from_controller(&cfg, dir.path());
        assert!(!updated.mecab_enabled);

        cfg.edit_setting(&key, "true").unwrap();
        updated.apply_controller(&cfg);
        assert!(updated.mecab_enabled);
    }

    #[test]
    fn keycap_emoji_continuations_do_not_emit_stray_svg_overlays() {
        let mut engine = TextEngine::new();
        let txt = "1\u{FE0F}\u{20E3}［連体］《2\u{FE0F}\u{20E3}の一語化》";
        let layout = engine.shape_single_line(txt, "", 14.5, 400, false, [248, 250, 252, 255]);
        let (svg_clusters, color_images) =
            engine.collect_svg_clusters_from_layout(&layout, txt, 0.0, 0.0, None, None);
        assert_eq!(
            color_images.len(),
            2,
            "1️⃣ and 2️⃣ must each render as exactly one color emoji bitmap"
        );
        for item in &svg_clusters {
            assert!(
                !item.text.contains('\u{20E3}') && !item.text.contains('\u{FE0F}'),
                "keycap continuation {:?} must never be emitted to usvg on top of the color emoji",
                item.text
            );
        }
        let svg_text: String = svg_clusters.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(svg_text, "［連体］《の一語化》");
    }

    #[test]
    fn custom_popup_css_parser_handles_variables_selectors_and_template() {
        // Starter templates must reproduce exact defaults cleanly.
        let from_template = parse_split_popup_css_theme(
            default_custom_popup_css_template(),
            default_custom_popup_outer_css_template(),
        );
        assert_eq!(from_template, PopupCssTheme::default());

        // Custom :root variables (including var() indirection, CSS #RRGGBBAA, comments, !important)
        // and class selectors must override defaults accurately.
        let css = r#"
            /* Theme comment */
            :root {
                --brand: #FF3366;
                --popup-bg: #101418EE !important;
                --popup-border: var(--brand);
                --popup-border-width: 2px;
                --popup-radius: 14px;
                --popup-padding: 18px;
                --font-family: "Noto Serif CJK JP", serif;
                --glossary-font-size: 16px;
                --anki-tag-border: #38BDF8AA;
            }
            .headword {
                color: var(--brand);
                font-size: 28px;
            }
            .reading {
                color: rgb(200, 210, 220);
                font-size: 14px;
            }
            .frequency {
                background-color: #112233;
                color: #AAFFEE;
            }
            .dictionary-badge {
                background: #441188;
                color: #FFFFFF;
            }
            .divider {
                background-color: #556677;
            }
        "#;
        let theme = parse_popup_css_theme(css);
        assert_eq!(theme.popup_bg, [0x10, 0x14, 0x18, 0xEE]);
        assert_eq!(theme.popup_border, [0xFF, 0x33, 0x66, 0xFF]);
        assert_eq!(theme.popup_border_width, 2.0);
        assert_eq!(theme.popup_radius, 14.0);
        assert_eq!(theme.popup_padding, 18.0);
        assert_eq!(theme.font_family, "Noto Serif CJK JP");
        assert_eq!(theme.glossary_font_size, 16.0);
        assert_eq!(theme.anki_tag_border, [0x38, 0xBD, 0xF8, 0xAA]);
        assert_eq!(theme.headword_color, [0xFF, 0x33, 0x66, 0xFF]);
        assert_eq!(theme.headword_size, 28.0);
        assert_eq!(theme.reading_color, [200, 210, 220, 255]);
        assert_eq!(theme.reading_size, 14.0);
        assert_eq!(theme.freq_bg, [0x11, 0x22, 0x33, 0xFF]);
        assert_eq!(theme.freq_color, [0xAA, 0xFF, 0xEE, 0xFF]);
        assert_eq!(theme.dict_badge_bg, [0x44, 0x11, 0x88, 0xFF]);
        assert_eq!(theme.dict_badge_color, [0xFF, 0xFF, 0xFF, 0xFF]);
        assert_eq!(theme.divider_color, [0x55, 0x66, 0x77, 0xFF]);
        assert_eq!(theme.term_divider_color, [0x55, 0x66, 0x77, 0xFF]);
    }

    #[test]
    fn custom_popup_css_glossary_cascade_supports_data_dictionary_and_display_none() {
        let dir = tempfile::TempDir::new().unwrap();
        let css_path = dir.path().join(CUSTOM_POPUP_CSS_FILENAME);
        std::fs::write(
            &css_path,
            r#"
            [data-dictionary^="JMdict"] [data-sc-content="examples"] {
                display: none;
            }
            li[data-dictionary="SpecialDict"] [data-sc-content="note"] {
                display: none;
            }
            "#,
        )
        .unwrap();

        let mut cache = StylesheetCache::new(dir.path());
        let res_dirs: Vec<PathBuf> = Vec::new();
        let raw = r#"[{
            "type": "structured-content",
            "content": [
                {"tag": "div", "data": {"content": "glossary"}, "content": "main meaning"},
                {"tag": "div", "data": {"content": "examples"}, "content": "secret example"},
                {"tag": "div", "data": {"content": "note"}, "content": "extra note"}
            ]
        }]"#;

        let jmdict_sheet = cache.get_for_dictionary(&res_dirs, 1, "JMdict [2025-11-01]");
        let jmdict_doc = render_structured_glossary_entry_with_theme(
            raw,
            "",
            jmdict_sheet,
            &HashMap::new(),
            LegacyGlossaryStyle::Bullet,
            &res_dirs,
            1,
            "JMdict [2025-11-01]",
            420.0,
            true,
            cache.theme(),
        )
        .expect("JMdict doc should render");
        assert!(jmdict_doc.plain_text.contains("main meaning"));
        assert!(
            !jmdict_doc.plain_text.contains("secret example"),
            "display: none under [data-dictionary^=\"JMdict\"] must hide examples, got: {:?}",
            jmdict_doc.plain_text
        );
        assert!(jmdict_doc.plain_text.contains("extra note"));

        let other_sheet = cache.get_for_dictionary(&res_dirs, 2, "SpecialDict");
        let other_doc = render_structured_glossary_entry_with_theme(
            raw,
            "",
            other_sheet,
            &HashMap::new(),
            LegacyGlossaryStyle::Bullet,
            &res_dirs,
            2,
            "SpecialDict",
            420.0,
            true,
            cache.theme(),
        )
        .expect("SpecialDict doc should render");
        assert!(other_doc.plain_text.contains("main meaning"));
        assert!(
            other_doc.plain_text.contains("secret example"),
            "examples must stay visible for non-matching dictionary, got: {:?}",
            other_doc.plain_text
        );
        assert!(
            !other_doc.plain_text.contains("extra note"),
            "li[data-dictionary=\"SpecialDict\"] normalization must hide note, got: {:?}",
            other_doc.plain_text
        );
    }

    #[test]
    fn split_popup_and_outer_css_supports_2col_pitch_dict_colors_freq_and_nested_rules() {
        let outer = r#"
            iframe.yomitan-popup {
                background: rgba(45, 45, 55, 0.85) !important;
                border-radius: 12px !important;
                border: 1px solid rgba(255, 255, 255, 0.1) !important;
                box-shadow: 0 5px 15px rgba(0, 0, 0, 0.3);
                width: 760px;
            }
        "#;
        let inner = r#"
            body {
                background: transparent !important;
                color: #dfdfdf;
            }
            :root {
                --font-family: "UD デジタル 教科書体 NK-R";
                // --font-family: "英椎楷書";
                --definition-columns: 2;
                --definition-gap: 10px;
                --card-border: #333333;
                --card-border-width: 2px;
                --card-radius: 10px;
                --card-padding: 10px;
                --pitch-heiban-color: #59b2ff;
                --pitch-atamadaka-color: #ff6666;
                --pitch-kifuku-color: #af85f4;
            }
            .term-tags { display: none; }
            .pitch-dict { display: none; }
            .kanji-extra { display: none; }
            .derivations { display: compact; }

            [data-dictionary="デジタル大辞泉"] {
                --dict-color: rgb(170, 0, 0);
                --dict-bg-opacity: 0.10;
            }
            [data-dictionary*="JMdict"] {
                --dict-color: rgb(0, 132, 255);
                --dict-bg-opacity: 0.05;
                --glossary-opacity: 0;
                --glossary-hover-opacity: 1;
                --glossary-solo-opacity: 1;
            }
            [data-frequency^="JPDB"] { background-color: #ff3e3d; color: #ffffff; }
            [data-frequency="CC100"] { background-color: #6238fa; color: #ffffff; }
        "#;

        let theme = parse_split_popup_css_theme(inner, outer);
        // Outer translucent background must NOT be clobbered by inner `body { background: transparent }`
        assert_eq!(theme.popup_bg[0..3], [45, 45, 55]);
        assert!((theme.popup_bg[3] as i32 - 217).abs() <= 2);
        assert_eq!(theme.popup_radius, 12.0);
        assert_eq!(theme.popup_border_width, 1.0);
        assert_eq!(theme.popup_shadow_blur, 15.0);
        assert_eq!(theme.popup_width, 760.0);

        // Inner typography, 2-col layout, and visibility flags
        assert_eq!(theme.font_family, "UD デジタル 教科書体 NK-R");
        assert_eq!(theme.text_color, [0xDF, 0xDF, 0xDF, 0xFF]);
        assert_eq!(theme.definition_columns, 2);
        assert_eq!(theme.definition_gap, 10.0);
        assert_eq!(theme.card_border_width, 2.0);
        assert_eq!(theme.card_radius, 10.0);
        assert_eq!(theme.card_padding, 10.0);
        assert!(!theme.show_term_tags);
        assert!(!theme.show_pitch_dict);
        assert!(!theme.show_kanji_extra);
        assert!(theme.compact_derivations);

        // Pitch colors
        assert_eq!(theme.resolve_headword_pitch_color(Some("heiban")), [0x59, 0xB2, 0xFF, 0xFF]);
        assert_eq!(theme.resolve_headword_pitch_color(Some("atamadaka")), [0xFF, 0x66, 0x66, 0xFF]);
        assert_eq!(theme.resolve_headword_pitch_color(Some("kifuku")), [0xAF, 0x85, 0xF4, 0xFF]);

        // Per-dictionary badge + tint + hover-to-reveal
        let daijisen = theme.resolve_dict_style("デジタル大辞泉");
        assert_eq!(daijisen.badge_bg, [170, 0, 0, 255]);
        assert_eq!(daijisen.card_bg[0..3], [170, 0, 0]);
        assert!((daijisen.card_bg[3] as i32 - 26).abs() <= 2);
        assert_eq!(daijisen.glossary_opacity, 1.0);

        let jmdict = theme.resolve_dict_style("JMdict [2025-11-01]");
        assert_eq!(jmdict.badge_bg, [0, 132, 255, 255]);
        assert_eq!(jmdict.glossary_opacity, 0.0);
        assert_eq!(jmdict.glossary_hover_opacity, 1.0);
        assert_eq!(jmdict.glossary_solo_opacity, 1.0);

        // Per-frequency colors
        assert_eq!(
            theme.resolve_freq_style("JPDB").0,
            [0xFF, 0x3E, 0x3D, 0xFF]
        );
        assert_eq!(
            theme.resolve_freq_style("CC100").0,
            [0x62, 0x38, 0xFA, 0xFF]
        );
    }

    #[test]
    fn nested_dictionary_css_can_hide_ruby_rt_furigana() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(
            dir.path().join(CUSTOM_POPUP_CSS_FILENAME),
            r#"
            [data-dictionary="明鏡国語辞典 第三版"] {
                .gloss-sc-ruby[data-sc-class="full"] .gloss-sc-rt {
                    display: none;
                }
            }
            "#,
        )
        .unwrap();

        let mut cache = StylesheetCache::new(dir.path());
        let res_dirs: Vec<PathBuf> = Vec::new();
        let raw = r#"[{
            "type": "structured-content",
            "content": {
                "tag": "ruby",
                "data": { "class": "full" },
                "content": ["漢字", { "tag": "rt", "content": "かんじ" }]
            }
        }]"#;
        let sheet = cache.get_for_dictionary(&res_dirs, 1, "明鏡国語辞典 第三版");
        let doc = render_structured_glossary_entry_with_theme(
            raw,
            "",
            sheet,
            &HashMap::new(),
            LegacyGlossaryStyle::Bullet,
            &res_dirs,
            1,
            "明鏡国語辞典 第三版",
            400.0,
            true,
            cache.theme(),
        )
        .expect("ruby doc should render");
        assert_eq!(doc.plain_text, "漢字");
        // When <rt> is hidden via display: none, it renders as plain text rather than a Ruby InlineBox with tooltip
        assert!(
            doc.hit_zones.is_empty(),
            "hidden <rt> should suppress ruby box tooltip hit zone, got {:?}",
            doc.hit_zones.len()
        );
    }
}

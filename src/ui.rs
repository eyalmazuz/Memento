//! Generated Slint UI bindings for Memento.

slint::include_modules!();

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

use slint::{ComponentHandle, Model as _, VecModel};
use std::{cell::Cell, rc::Rc};

pub fn dictionary_result_rows(
    hits: &[crate::controller::dict::SearchHit],
    excluded: &[String],
) -> Vec<DictResult> {
    // Keep all dictionary rows for the same term together. The lookup engine
    // ranks individual hits, so rows from different terms can be interleaved.
    let mut grouped: Vec<&crate::controller::dict::SearchHit> = Vec::with_capacity(hits.len());
    let mut terms: Vec<(&str, &str, bool)> = Vec::new();
    let mut by_term: std::collections::HashMap<
        (&str, &str, bool),
        Vec<&crate::controller::dict::SearchHit>,
    > = std::collections::HashMap::new();
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
    grouped
        .iter()
        .enumerate()
        .map(|(index, hit)| DictResult {
            index: (index + 1) as i32,
            expression: hit.expression.clone().into(),
            reading: hit.reading.clone().into(),
            // JMdict's common-word star is an emoji code point. Use the equivalent
            // text star so showing a tag doesn't load a large color-emoji font.
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
            has_kanji: hit.expression.chars().any(|character| {
                ('\u{3400}'..='\u{9fff}').contains(&character)
                    || ('\u{f900}'..='\u{faff}').contains(&character)
                    || ('\u{20000}'..='\u{3134f}').contains(&character)
            }),
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
    let full = Rc::new(VecModel::from(rows));
    app.set_dict_results(full.into());
    let full = app.get_dict_results();
    let first_batch_end = next_popup_batch_end(&full, 0);
    app.set_popup_loaded_count(first_batch_end as i32);
    app.set_popup_results(
        Rc::new(VecModel::from(popup_rows_through(&full, first_batch_end))).into(),
    );
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

/// Shared popup actions used by the desktop window and the GL interaction test.
pub fn install_popup_navigation(app: &MementoApp) {
    app.global::<PopupLayout>()
        .on_frequency_lines(|frequencies, available_width| {
            // Pack natural-width badges into lines. Use a conservative character
            // advance so each line also fits when the platform uses a fallback font.
            // The UI measures the actual text for each badge; no fixed grid cells.
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
        };
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
            // Opening/resetting the popup can request a page using its old
            // geometry. Re-check after the current layout has settled.
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
            let suffix = &text[offset..];
            let query = if token.chars().any(char::is_alphanumeric) {
                suffix.into()
            } else {
                slint::SharedString::default()
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

pub fn subtitle_token_width(word: &SubtitleWord, font_px: f32, furigana: bool) -> f32 {
    let base_width = estimated_subtitle_text_width(word.text.as_str(), font_px);
    let reading_width = if furigana && !word.reading.is_empty() {
        estimated_subtitle_text_width(word.reading.as_str(), font_px * 0.5)
    } else {
        0.0
    };
    base_width.max(reading_width) + 2.0
}

pub fn subtitle_lines_from_words(
    all_words: Vec<SubtitleWord>,
    max_width_px: f32,
    font_px: f32,
    furigana: bool,
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
        let token_width = subtitle_token_width(&word, font_px, furigana);
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

pub fn subtitle_lines_for_app(app: &MementoApp, words: Vec<SubtitleWord>) -> Vec<SubtitleLine> {
    let max_width_px = (app.get_video_width() as f32 - 72.0).max(120.0);
    let font_px = (app.get_video_height() as f32 * app.get_subtitle_scale()).max(8.0);
    subtitle_lines_from_words(words, max_width_px, font_px, app.get_furigana_visible())
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
    let epoch = furigana_epoch.fetch_add(1, std::sync::atomic::Ordering::AcqRel) + 1;
    let pending = app.get_furigana_visible() && !text.is_empty()
        && furigana_sender.send((epoch, text.to_owned())).is_ok();
    app.set_subtitle_furigana_pending(pending);
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

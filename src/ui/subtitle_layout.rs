use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::Ordering;

use slint::{ComponentHandle, Model as _, SharedString, VecModel};

use super::popup_model::{
    active_rich_bridge_for_app, active_rich_bridges_for_app, is_kanji_char,
    latest_active_rich_bridge,
};
use super::text_engine::{
    cancel_subtitle_token_prewarm, with_shared_text_engine, SubtitleRenderStyle,
};
use super::{MementoApp, SubtitleLine, SubtitleOverlayBridge, SubtitleWord};

thread_local! {
    static FALLBACK_SUBTITLE_STYLE: RefCell<SubtitleRenderStyle> =
        RefCell::new(SubtitleRenderStyle::default());
}

pub(super) fn set_active_subtitle_render_style(style: SubtitleRenderStyle) {
    let changed = FALLBACK_SUBTITLE_STYLE.with(|slot| {
        let mut cur = slot.borrow_mut();
        if *cur != style {
            *cur = style;
            true
        } else {
            false
        }
    });
    if changed {
        cancel_subtitle_token_prewarm();
    }
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

pub(super) fn is_japanese_lookup_char(ch: char) -> bool {
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

pub(super) fn active_subtitle_render_style() -> SubtitleRenderStyle {
    if let Some(bridge) = latest_active_rich_bridge() {
        if let Some(style) = bridge.subtitle_style() {
            return style;
        }
    }
    FALLBACK_SUBTITLE_STYLE.with(|slot| slot.borrow().clone())
}

pub fn active_subtitle_style_for_app(app: &MementoApp) -> SubtitleRenderStyle {
    subtitle_style_for_app(app)
}

fn subtitle_style_for_app(app: &MementoApp) -> SubtitleRenderStyle {
    if let Some(bridge) = active_rich_bridge_for_app(app) {
        if let Some(style) = bridge.subtitle_style() {
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
    if let Some(shaped_width) = with_shared_text_engine(|engine| {
        engine.measure_subtitle_token_width(word, font_px, furigana, style)
    }) {
        return shaped_width;
    }
    let base_width = estimated_subtitle_text_width(word.text.as_str(), font_px);
    let reading_width = if furigana && !word.reading.is_empty() {
        let nominal_ruby_px = (font_px * style.furigana_ratio.clamp(0.25, 0.75)).max(8.0);
        let nominal_w = estimated_subtitle_text_width(word.reading.as_str(), nominal_ruby_px);
        if nominal_w > base_width && nominal_w > 0.0 {
            let fit_scale = (base_width / nominal_w).clamp(0.72, 1.0);
            let fitted_px = (nominal_ruby_px * fit_scale).max(7.0).min(nominal_ruby_px);
            estimated_subtitle_text_width(word.reading.as_str(), fitted_px)
        } else {
            nominal_w
        }
    } else {
        0.0
    };
    let max_ruby_bleed = (font_px * 0.25).max(0.0);
    base_width.max(reading_width - max_ruby_bleed * 2.0) + 2.0
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
    subtitle_lines_from_words_with_style(
        words,
        max_width_px,
        font_px,
        app.get_furigana_visible(),
        &style,
    )
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
    let ass_native = app.global::<SubtitleOverlayBridge>().get_ass_native_active();
    let pending = !ass_native
        && app.get_furigana_visible()
        && !text.is_empty()
        && furigana_sender.send((epoch, text.to_owned())).is_ok();
    app.set_subtitle_furigana_pending(pending);

    let bridges = active_rich_bridges_for_app(app);
    for bridge in bridges {
        bridge.clear_subtitle_scan_state(app);
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

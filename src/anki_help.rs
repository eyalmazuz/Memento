//! Sectioned, searchable Anki marker reference shown by the Anki Integration Help window.

use std::rc::Rc;

use copypasta::{ClipboardContext, ClipboardProvider};
use slint::{ComponentHandle, SharedString, VecModel};

use crate::ui::{AnkiHelpWindow, HelpArgument, HelpMarker, HelpSection};

const TERM_SECTIONS: &[(&str, &str, &[&str])] = &[
    (
        "Word & Definitions",
        "Expressions, readings, furigana, glossaries, tags, and parts of speech",
        &[
            "expression",
            "reading",
            "furigana",
            "furigana-plain",
            "glossary",
            "glossary-brief",
            "glossary-compact",
            "selection",
            "part-of-speech",
            "tags",
            "tags-brief",
        ],
    ),
    (
        "Subtitles & Cloze",
        "Active lines, multi-row subtitle context, cloze splits, clipboard, and media title",
        &[
            "subtitle",
            "subtitle-2",
            "context",
            "context-2",
            "cloze-body",
            "cloze-prefix",
            "cloze-suffix",
            "clipboard",
            "title",
        ],
    ),
    (
        "Pitch & Frequencies",
        "Pitch accent graphs, categories, downstep positions, and frequency ranks",
        &[
            "pitch",
            "pitch-graphs",
            "pitch-positions",
            "pitch-categories",
            "frequencies",
            "frequency-harmonic-rank",
            "frequency-average-rank",
            "frequency-harmonic-occurrence",
            "frequency-average-occurrence",
        ],
    ),
    (
        "Audio & Media",
        "Word audio, subtitle/context audio clips, screenshots, and video clips",
        &[
            "audio",
            "audio-media",
            "audio-context",
            "screenshot",
            "screenshot-video",
            "video",
        ],
    ),
];

const KANJI_SECTIONS: &[(&str, &str, &[&str])] = &[
    (
        "Kanji Info",
        "Character readings, stroke count, kanji glossary, and tags",
        &[
            "character",
            "onyomi",
            "kunyomi",
            "stroke-count",
            "glossary",
            "tags",
            "tags-brief",
        ],
    ),
    (
        "Subtitles & Cloze",
        "Active lines, multi-row context, cloze splits, clipboard, and media title",
        &[
            "subtitle",
            "subtitle-2",
            "context",
            "context-2",
            "cloze-body",
            "cloze-prefix",
            "cloze-suffix",
            "clipboard",
            "title",
        ],
    ),
    (
        "Frequencies",
        "Kanji frequency tags, harmonic means, and averages",
        &[
            "frequencies",
            "frequency-harmonic-rank",
            "frequency-average-rank",
            "frequency-harmonic-occurrence",
            "frequency-average-occurrence",
        ],
    ),
    (
        "Audio & Media",
        "Subtitle/context audio clips, screenshots, and video clips",
        &[
            "audio-media",
            "audio-context",
            "screenshot",
            "screenshot-video",
            "video",
        ],
    ),
];

fn description(name: &str) -> &'static str {
    match name {
        "audio" => "Audio of the word if found and the skip hash is not matched.",
        "audio-context" => "Audio from the first marked subtitle's start to the last marked subtitle's end, including gaps. Click or drag across rows in the subtitle list to mark them, or Shift-click a range. If none are marked, uses the current subtitle.",
        "audio-media" => "Audio of the current subtitle.",
        "character" => "The kanji character.",
        "clipboard" => "The current text in the clipboard.",
        "cloze-body" => "The raw expression as recognized by Memento.",
        "cloze-prefix" => "The text before {cloze-body}.",
        "cloze-suffix" => "The text after {cloze-body}.",
        "context" => "Text of all selected lines in the primary subtitle list.",
        "context-2" => "Text of all selected lines in the secondary subtitle list.",
        "expression" => "The expression in kanji. Kana if there is no kanji or explicitly requested.",
        "frequencies" => "Bulleted list of frequency tags.",
        "frequency-average-occurrence" => "The average of frequency data for the expression. Defaults to 0 occurrences when frequency data is not available.",
        "frequency-average-rank" => "The average of frequency data for the expression. Defaults to 9999999 when frequency data is not available.",
        "frequency-harmonic-occurrence" => "The harmonic mean of frequency data for the expression. Defaults to 0 for occurrences when frequency data is not available.",
        "frequency-harmonic-rank" => "The harmonic mean of frequency data for the expression. Defaults to 9999999 for occurrences when frequency data is not available.",
        "furigana" => "The expression in kanji with furigana above. Kana if there is no kanji.",
        "furigana-plain" => "The expression in kanji with furigana proceeding in brackets. Kana if there is no kanji.",
        "glossary" => "Dictionary definitions of the expression.",
        "glossary-brief" => "Dictionary definitions of the expression without dictionary information.",
        "glossary-compact" => "Dictionary definitions of the expression in compact form.",
        "kunyomi" => "A comma separated list of kunyomi (Japanese) readings written in katakana.",
        "onyomi" => "A comma separated list of onyomi (Chinese) readings written in hiragana.",
        "part-of-speech" => "Part-of-speech tags for the expression.",
        "pitch" => "Pitch accents for the reading of the expression.",
        "pitch-categories" => "A comma separated list of pitch accent categories for the expression: heiban, kifuku, atamadaka, nakadaka, odaka.",
        "pitch-graphs" => "Pitch graphs for the reading of the expression.",
        "pitch-positions" => "Pitch positions for the reading of the expression.",
        "reading" => "The reading of the expression in kana. Empty if the expression has no kanji.",
        "screenshot" => "Screenshot of the current frame.",
        "screenshot-video" => "Screenshot of the current frame without subtitles if visible.",
        "selection" => "The currently selected glossary text. If multiple glossaries are selected, they are ordered as a list.",
        "stroke-count" => "The number of strokes in the character.",
        "subtitle" => "The text of the primary subtitle.",
        "subtitle-2" => "The text of the secondary subtitle.",
        "tags" => "Unordered list of expression tags.",
        "tags-brief" => "Unordered list of expression tags without descriptions.",
        "title" => "Title of the current media. Filename if no title is set.",
        "video" => "An MP4 file containing a clip of the current context.",
        _ => "",
    }
}

type ArgumentInfo = (&'static str, &'static str, &'static str);

const NORM: ArgumentInfo = (
    "norm",
    "If true, normalize audio.",
    "The profile audio media normalization setting",
);
const DB: ArgumentInfo = (
    "db",
    "The decibel value to normalize audio to. Ignored if norm = false.",
    "The profile normalization dB setting",
);
const SOURCE: ArgumentInfo = (
    "source",
    "The source of clip timings. subtitle: use timings from the current subtitle; context: use timings from the currently selected context.",
    "subtitle",
);
const MIN_VALUE: ArgumentInfo = (
    "min-value",
    "If true, only export the lowest frequency value.",
    "false",
);
const VALUE_ONLY: ArgumentInfo = (
    "value-only",
    "If true, export frequency values without dictionary names.",
    "false",
);
const DICT: ArgumentInfo = (
    "dict",
    "Filter to only use entries from a particular dictionary. Takes a dictionary ID. Dictionary IDs can be found in Options under Dictionary at the end of dictionary names.",
    "None",
);
const MAX_WIDTH: ArgumentInfo = (
    "max-width",
    "Used to determine the maximum width of a saved screenshot. A value of -1 leaves this unbounded.",
    "-1",
);
const MAX_HEIGHT: ArgumentInfo = (
    "max-height",
    "Used to determine the maximum height of a saved screenshot. A value of -1 leaves this unbounded.",
    "-1",
);
const KEEP_RATIO: ArgumentInfo = (
    "keep-ratio",
    "If true, maintains the aspect ratio of the image during resize.",
    "true",
);
const AUDIO: ArgumentInfo = (
    "audio",
    "If true, audio is included in the clip.",
    "true",
);
const SUBTITLES: ArgumentInfo = (
    "subtitles",
    "If true, subtitles are included in the clip.",
    "true",
);

fn arguments(name: &str, kanji: bool) -> &'static [ArgumentInfo] {
    match name {
        "audio-context" => &[NORM, DB],
        "audio-media" => &[SOURCE, NORM, DB],
        "frequencies" => &[MIN_VALUE, VALUE_ONLY],
        "glossary" if !kanji => &[DICT],
        "glossary-brief" | "glossary-compact" => &[DICT],
        "screenshot" | "screenshot-video" => &[MAX_WIDTH, MAX_HEIGHT, KEEP_RATIO],
        "video" => &[SOURCE, AUDIO, SUBTITLES, NORM, DB],
        _ => &[],
    }
}

fn marker(name: &str, kanji: bool) -> HelpMarker {
    HelpMarker {
        name: name.into(),
        description: description(name).into(),
        arguments: Rc::new(VecModel::from(
            arguments(name, kanji)
                .iter()
                .map(|&(name, description, default_value)| HelpArgument {
                    name: name.into(),
                    description: description.into(),
                    default_value: default_value.into(),
                })
                .collect::<Vec<_>>(),
        ))
        .into(),
    }
}

fn marker_matches(name: &str, kanji: bool, query_lower: &str) -> bool {
    if query_lower.is_empty() {
        return true;
    }
    if name.to_ascii_lowercase().contains(query_lower)
        || description(name).to_ascii_lowercase().contains(query_lower)
    {
        return true;
    }
    arguments(name, kanji).iter().any(|&(arg_name, arg_desc, _)| {
        arg_name.to_ascii_lowercase().contains(query_lower)
            || arg_desc.to_ascii_lowercase().contains(query_lower)
    })
}

fn build_sections(
    spec: &[(&str, &str, &[&str])],
    kanji: bool,
    query: &str,
) -> (Vec<HelpSection>, Vec<HelpMarker>, usize) {
    let q = query.trim().to_ascii_lowercase();
    let mut sections = Vec::new();
    let mut flat = Vec::new();
    let mut total = 0usize;
    for &(title, subtitle, names) in spec {
        let mut matched = Vec::new();
        for &name in names {
            if marker_matches(name, kanji, &q) {
                let m = marker(name, kanji);
                flat.push(m.clone());
                matched.push(m);
            }
        }
        total += matched.len();
        if !matched.is_empty() || q.is_empty() {
            sections.push(HelpSection {
                title: title.into(),
                subtitle: subtitle.into(),
                markers: Rc::new(VecModel::from(matched)).into(),
            });
        }
    }
    (sections, flat, total)
}

fn apply_filter(window: &AnkiHelpWindow, query: &str) {
    let (term_sections, term_flat, term_total) = build_sections(TERM_SECTIONS, false, query);
    let (kanji_sections, kanji_flat, kanji_total) = build_sections(KANJI_SECTIONS, true, query);
    window.set_term_sections(Rc::new(VecModel::from(term_sections)).into());
    window.set_term_markers(Rc::new(VecModel::from(term_flat)).into());
    window.set_term_total_count(term_total as i32);
    window.set_kanji_sections(Rc::new(VecModel::from(kanji_sections)).into());
    window.set_kanji_markers(Rc::new(VecModel::from(kanji_flat)).into());
    window.set_kanji_total_count(kanji_total as i32);
}

pub fn populate(window: &AnkiHelpWindow) {
    window.set_term_section_names(
        Rc::new(VecModel::from(
            TERM_SECTIONS
                .iter()
                .map(|&(title, _, _)| SharedString::from(title))
                .collect::<Vec<_>>(),
        ))
        .into(),
    );
    window.set_kanji_section_names(
        Rc::new(VecModel::from(
            KANJI_SECTIONS
                .iter()
                .map(|&(title, _, _)| SharedString::from(title))
                .collect::<Vec<_>>(),
        ))
        .into(),
    );
    apply_filter(window, "");

    let weak_search = window.as_weak();
    window.on_search_changed(move |query| {
        if let Some(win) = weak_search.upgrade() {
            if !query.trim().is_empty() {
                win.set_active_term_section(-1);
                win.set_active_kanji_section(-1);
            }
            apply_filter(&win, query.as_str());
        }
    });

    let weak_copy = window.as_weak();
    window.on_copy_marker(move |snippet| {
        let text = snippet.to_string();
        if let Ok(mut ctx) = ClipboardContext::new() {
            let _ = ctx.set_contents(text.clone());
        }
        if let Some(win) = weak_copy.upgrade() {
            let notice = format!("Copied {text}");
            win.set_copied_notice(notice.clone().into());
            let clear_weak = win.as_weak();
            slint::Timer::single_shot(std::time::Duration::from_millis(1800), move || {
                if let Some(w) = clear_weak.upgrade() {
                    if w.get_copied_notice().as_str() == notice {
                        w.set_copied_notice("".into());
                    }
                }
            });
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anki_help_sections_cover_all_markers_and_filter_cleanly() {
        let (term_sections, term_flat, term_total) = build_sections(TERM_SECTIONS, false, "");
        assert_eq!(term_sections.len(), 4);
        assert_eq!(term_total, 35);
        assert_eq!(term_flat.len(), 35);

        let (kanji_sections, kanji_flat, kanji_total) = build_sections(KANJI_SECTIONS, true, "");
        assert_eq!(kanji_sections.len(), 4);
        assert_eq!(kanji_total, 26);
        assert_eq!(kanji_flat.len(), 26);

        let (pitch_sections, pitch_flat, pitch_total) =
            build_sections(TERM_SECTIONS, false, "pitch");
        assert_eq!(pitch_sections.len(), 1);
        assert_eq!(pitch_total, 4);
        assert!(pitch_flat.iter().all(|m| m.name.contains("pitch")));

        let (_, width_flat, _) = build_sections(TERM_SECTIONS, false, "max-width");
        assert_eq!(width_flat.len(), 2, "argument search must match screenshot & screenshot-video");
    }
}

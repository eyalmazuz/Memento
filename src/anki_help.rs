//! Marker reference shown by the Anki help window, following the C++ AnkiMarkers list.

use std::rc::Rc;

use slint::VecModel;

use crate::ui::{AnkiHelpWindow, HelpArgument, HelpMarker};

const TERM_MARKERS: &[&str] = &[
    "audio", "audio-context", "audio-media", "clipboard", "cloze-body", "cloze-prefix",
    "cloze-suffix", "context", "context-2", "expression", "frequencies",
    "frequency-average-occurrence", "frequency-average-rank", "frequency-harmonic-occurrence",
    "frequency-harmonic-rank", "furigana", "furigana-plain", "glossary", "glossary-brief",
    "glossary-compact", "pitch", "pitch-categories", "pitch-graphs", "pitch-positions",
    "reading", "screenshot", "screenshot-video", "selection", "subtitle", "subtitle-2",
    "tags", "tags-brief", "title", "video",
];

const KANJI_MARKERS: &[&str] = &[
    "audio-context", "audio-media", "character", "clipboard", "cloze-body", "cloze-prefix",
    "cloze-suffix", "context", "context-2", "frequencies", "frequency-average-occurrence",
    "frequency-average-rank", "frequency-harmonic-occurrence", "frequency-harmonic-rank",
    "glossary", "kunyomi", "onyomi", "screenshot", "screenshot-video", "stroke-count",
    "subtitle", "subtitle-2", "tags", "tags-brief", "title", "video",
];

fn description(name: &str) -> &'static str {
    match name {
        "audio" => "Audio of the word if found and the skip hash is not matched.",
        "audio-context" => "Audio of the subtitle list selection.",
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

const NORM: ArgumentInfo = ("norm", "If true, normalize audio.", "The profile audio media normalization setting");
const DB: ArgumentInfo = ("db", "The decibel value to normalize audio to. Ignored if norm = false.", "The profile normalization dB setting");
const SOURCE: ArgumentInfo = ("source", "The source of clip timings. subtitle: use timings from the current subtitle; context: use timings from the currently selected context.", "subtitle");
const MIN_VALUE: ArgumentInfo = ("min-value", "If true, only export the lowest frequency value.", "false");
const VALUE_ONLY: ArgumentInfo = ("value-only", "If true, export frequency values without dictionary names.", "false");
const DICT: ArgumentInfo = ("dict", "Filter to only use entries from a particular dictionary. Takes a dictionary ID. Dictionary IDs can be found in Options under Dictionaries at the end of dictionary names.", "None");
const MAX_WIDTH: ArgumentInfo = ("max-width", "Used to determine the maximum width of a saved screenshot. A value of -1 leaves this unbounded.", "-1");
const MAX_HEIGHT: ArgumentInfo = ("max-height", "Used to determine the maximum height of a saved screenshot. A value of -1 leaves this unbounded.", "-1");
const KEEP_RATIO: ArgumentInfo = ("keep-ratio", "If true, maintains the aspect ratio of the image during resize.", "true");
const AUDIO: ArgumentInfo = ("audio", "If true, audio is included in the clip.", "true");
const SUBTITLES: ArgumentInfo = ("subtitles", "If true, subtitles are included in the clip.", "true");

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
            arguments(name, kanji).iter().map(|&(name, description, default_value)| HelpArgument {
                name: name.into(),
                description: description.into(),
                default_value: default_value.into(),
            }).collect::<Vec<_>>(),
        )).into(),
    }
}

pub fn populate(window: &AnkiHelpWindow) {
    window.set_term_markers(Rc::new(VecModel::from(
        TERM_MARKERS.iter().map(|name| marker(name, false)).collect::<Vec<_>>(),
    )).into());
    window.set_kanji_markers(Rc::new(VecModel::from(
        KANJI_MARKERS.iter().map(|name| marker(name, true)).collect::<Vec<_>>(),
    )).into());
}

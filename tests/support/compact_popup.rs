use memento_app::{
    controller::dict::TermPitchGraph, popup_pitch_graphs, DictResult, PopupFrequency,
    PopupGlossaryBlock,
};
use slint::VecModel;
use std::rc::Rc;

pub fn definitions() -> Vec<DictResult> {
    let mut first = DictResult {
        index: 1,
        expression: "傷つく".into(),
        reading: "きずつく".into(),
        dict_name: "JMdict [2025-11-01]".into(),
        pos_tags: "★, 1, v5k, vi, spec".into(),
        selected: true,
        has_kanji: true,
        term_start: true,
        example_ja: "傷ついた鳥が地面に落ちた。".into(),
        example_en: "The injured bird fell to the ground.".into(),
        glossary_blocks: Rc::new(VecModel::from(vec![
            PopupGlossaryBlock {
                text: "to be wounded".into(),
                kind: 2,
                ..Default::default()
            },
            PopupGlossaryBlock {
                text: "to get injured".into(),
                kind: 2,
                ..Default::default()
            },
        ]))
        .into(),
        ..Default::default()
    };
    first.frequencies = Rc::new(VecModel::from(
        [
            ("Jiten", "1695"),
            ("Jiten", "62965㋕"),
            ("CC100", "10052"),
            ("Jiten (Anime)", "1823"),
            ("Jiten (Anime)", "56236㋕"),
            ("JPDB", "2058"),
            ("JPDB", "86009㋕"),
        ]
        .into_iter()
        .map(|(dictionary, value)| PopupFrequency {
            dictionary: dictionary.into(),
            value: value.into(),
        })
        .collect::<Vec<_>>(),
    ))
    .into();
    first.pitch_graphs = Rc::new(VecModel::from(popup_pitch_graphs(vec![TermPitchGraph {
        dictionary: "アクセント辞典".into(),
        position: 3,
        mora: ["き", "ず", "つ", "く"].map(str::to_owned).to_vec(),
    }])))
    .into();
    let second = DictResult {
        index: 2,
        term_start: false,
        term_end: true,
        frequencies: Default::default(),
        pitch_graphs: Default::default(),
        glossary_blocks: Rc::new(VecModel::from(vec![PopupGlossaryBlock {
            text: "to be hurt; to feel offended".into(),
            kind: 2,
            ..Default::default()
        }]))
        .into(),
        example_ja: "彼女の言葉に傷ついた。".into(),
        example_en: "I was hurt by what she said.".into(),
        ..first.clone()
    };
    let third = DictResult {
        index: 3,
        expression: "傷".into(),
        reading: "きず".into(),
        dict_name: "JMdict [2025-11-01]".into(),
        selected: true,
        has_kanji: true,
        term_start: true,
        term_end: true,
        glossary: "◦ wound; injury\n◦ scratch; scar".into(),
        ..Default::default()
    };
    vec![first, second, third]
}

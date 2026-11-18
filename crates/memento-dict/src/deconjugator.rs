// Copyright (c) 2024 Spacehamster
// SPDX-License-Identifier: GPL-2.0-or-later

//! Rule-based Japanese deconjugation, ported from `src/dict/deconjugator.cpp`
//! with Yomitan rule parity, short-causative sub-conditions, and grammar descriptions.
//! Words and results are UTF-16 code units to preserve `QString` values, including
//! unpaired surrogates produced when sentence mode removes one code unit at a time.

use std::sync::OnceLock;

/// `WordForm` values and order match the C++ enum (including its unused members).
#[repr(u32)]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum WordForm {
    GodanVerb = 0,
    IchidanVerb = 1,
    SuruVerb = 2,
    KuruVerb = 3,
    IrregularVerb = 4,
    Adjective = 5,
    Negative = 6,
    Past = 7,
    Te = 8,
    Conjunctive = 9,
    Volitional = 10,
    Passive = 11,
    Causative = 12,
    Imperative = 13,
    Potential = 14,
    PotentialPassive = 15,
    Conditional = 16,
    ImperativeNegative = 17,
    Zaru = 18,
    Zu = 19,
    Nu = 20,
    Neba = 21,
    Tari = 22,
    Shimau = 23,
    Chau = 24,
    Chimau = 25,
    Polite = 26,
    Tara = 27,
    Tai = 28,
    Nasai = 29,
    Sugiru = 30,
    Sou = 31,
    E = 32,
    Ba = 33,
    Ki = 34,
    Toku = 35,
    ColloquialNegative = 36,
    ProvisionalColloquialNegative = 37,
    Continuous = 38,
    Adverbial = 39,
    Noun = 40,
    Any = 41,
    None = 42,
}

/// An ordered candidate; duplicates and alternative derivations are deliberately retained.
/// `derivations` run from the candidate's base type toward its conjugated form.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConjugationInfo {
    pub base: Vec<u16>,
    pub conjugated: Vec<u16>,
    pub derivations: Vec<WordForm>,
    /// Display text assembled from the English translation source strings.
    /// UI callers should reformat `derivations` with `word_form_label` and
    /// `QCoreApplication::translate("Deconjugator", label)` for localized output.
    pub derivation_display: Vec<u16>,
}

/// The untranslated source text used by C++ `Deconjugator::tr`.
/// Qt consumers can translate this in the `Deconjugator` context.
pub fn word_form_label(form: WordForm) -> &'static str {
    match form {
        WordForm::GodanVerb => "godan verb",
        WordForm::IchidanVerb => "ichidan verb",
        WordForm::SuruVerb => "suru verb",
        WordForm::KuruVerb => "kuru verb",
        WordForm::Adjective => "adjective",
        WordForm::Negative => "negative",
        WordForm::Past => "past",
        WordForm::Te => "-te",
        WordForm::Conjunctive => "masu stem",
        WordForm::Volitional => "volitional",
        WordForm::Passive => "passive",
        WordForm::Causative => "causative",
        WordForm::Imperative => "imperative",
        WordForm::Potential => "potential",
        WordForm::PotentialPassive => "potential or passive",
        WordForm::ImperativeNegative => "imperative negative",
        WordForm::Zaru => "-zaru",
        WordForm::Zu => "-zu",
        WordForm::Nu => "-nu",
        WordForm::Neba => "-neba",
        WordForm::Tari => "-tari",
        WordForm::Shimau => "-shimau",
        WordForm::Chau => "-chau",
        WordForm::Chimau => "-chimau",
        WordForm::Polite => "polite",
        WordForm::Tara => "-tara",
        WordForm::Tai => "-tai",
        WordForm::Nasai => "-nasai",
        WordForm::Sugiru => "-sugiru",
        WordForm::Sou => "-sou",
        WordForm::E => "-e",
        WordForm::Ba => "-ba",
        WordForm::Ki => "-ki",
        WordForm::Toku => "-toku",
        WordForm::ColloquialNegative => "colloquial negative",
        WordForm::ProvisionalColloquialNegative => "provisional colloquial negative",
        WordForm::Continuous => "progressive or perfect",
        WordForm::Adverbial => "adv",
        WordForm::Noun => "noun",
        WordForm::IrregularVerb | WordForm::Conditional | WordForm::Any | WordForm::None => {
            "unknown"
        }
    }
}

/// Concise grammar hover description for a conjugation step (#279).
pub fn word_form_description(form: WordForm) -> &'static str {
    match form {
        WordForm::GodanVerb => "Five-row (-u) verb",
        WordForm::IchidanVerb => "One-row (-iru/-eru) verb",
        WordForm::SuruVerb => "Action verb conjugated with suru (to do)",
        WordForm::KuruVerb => "Irregular verb kuru (to come)",
        WordForm::IrregularVerb => "Irregular verb conjugation",
        WordForm::Adjective => "True i-adjective (-i)",
        WordForm::Negative => "Negates the verb or adjective (-nai/-masen)",
        WordForm::Past => "Past tense or completed action (-ta/-da)",
        WordForm::Te => "Conjunctive -te/-de form linking clauses or auxiliaries",
        WordForm::Conjunctive => "Continuative (ren'youkei / -masu) stem",
        WordForm::Volitional => "Expresses intention or suggestion ('let's / will', -ou/-you)",
        WordForm::Passive => "Passive voice ('is done to', -reru/-rareru)",
        WordForm::Causative => "Makes or lets someone perform an action (-seru/-saseru)",
        WordForm::Imperative => "Direct command form",
        WordForm::Potential => "Expresses ability or possibility ('can do')",
        WordForm::PotentialPassive => {
            "Expresses ability ('can do') or passive/honorific (-rareru)"
        }
        WordForm::Conditional => "Conditional form",
        WordForm::ImperativeNegative => "Negative command ('do not', -na)",
        WordForm::Zaru => "Classical/literary negative attributive (-zaru)",
        WordForm::Zu => "Adverbial/classical negative ('without doing', -zu)",
        WordForm::Nu => "Classical/literary negative (-nu)",
        WordForm::Neba => "Negative conditional ('if not / must', -neba)",
        WordForm::Tari => "Lists representative actions or states (-tari/-dari)",
        WordForm::Shimau => "Indicates completion or regrettable outcome (-te shimau)",
        WordForm::Chau => "Colloquial contraction of -te shimau (completion/regret)",
        WordForm::Chimau => "Casual contraction of -te shimau (completion/regret)",
        WordForm::Polite => "Polite speech form (-masu)",
        WordForm::Tara => "Conditional 'if/when' form (-tara/-dara)",
        WordForm::Tai => "Desiderative form ('want to do', -tai)",
        WordForm::Nasai => "Polite/firm command form (-nasai)",
        WordForm::Sugiru => "Indicates excess ('too much', -sugiru)",
        WordForm::Sou => "Indicates appearance or likelihood ('looks like', -sou)",
        WordForm::E => "Colloquial vowel contraction (-ai/-oi to -ee)",
        WordForm::Ba => "Provisional conditional ('if', -eba/-kereba)",
        WordForm::Ki => "Classical attributive i-adjective form (-ki)",
        WordForm::Toku => "Colloquial contraction of -te oku ('do in advance')",
        WordForm::ColloquialNegative => "Colloquial/dialectal negative (-n)",
        WordForm::ProvisionalColloquialNegative => {
            "Colloquial contraction of -nakereba ('must / if not', -nakya)"
        }
        WordForm::Continuous => "Ongoing action or resultant state (-te iru / -teru / -toru)",
        WordForm::Adverbial => "Adverbial form modifying a predicate (-ku)",
        WordForm::Noun => "Nominalized degree or quality noun (-sa)",
        WordForm::Any | WordForm::None => "",
    }
}

/// Formats `derivations` (which run from base type toward surface conjugation)
/// into a human-readable `←`-joined chain (e.g. `"imperative ← passive ← godan verb"`)
/// and matching `(label, description)` hover entries (#279).
#[must_use]
pub fn format_derivation_chain(derivations: &[WordForm]) -> (String, Vec<(String, String)>) {
    if derivations.is_empty() {
        return (String::new(), Vec::new());
    }
    let last_is_conjunctive = derivations.last() == Some(&WordForm::Conjunctive);
    let mut steps: Vec<WordForm> = Vec::with_capacity(derivations.len());
    for (idx, &form) in derivations.iter().enumerate() {
        if matches!(form, WordForm::Any | WordForm::None) {
            continue;
        }
        if form == WordForm::Conjunctive && !(last_is_conjunctive && idx + 1 == derivations.len()) {
            continue;
        }
        steps.push(form);
    }
    steps.reverse();

    let descriptions: Vec<(String, String)> = steps
        .iter()
        .map(|&form| {
            (
                word_form_label(form).to_owned(),
                word_form_description(form).to_owned(),
            )
        })
        .collect();
    let chain = descriptions
        .iter()
        .map(|(label, _)| label.as_str())
        .collect::<Vec<_>>()
        .join(" ← ");
    (chain, descriptions)
}

use self::WordForm as W;

// Yomitan sub-conditions for Godan `-す` vs short causative (`-あす` vs `-さす`):
// - SUB_V5_D: dictionary godan verb
// - SUB_V5_SP: non-`さす` short causative (`書かす`, `読ます`, ...) — allows Passive (`書かされる`)
// - SUB_V5_SS: `さす` short causative (`食べさす`, `話さす`, `来さす`, `さす`) — blocks Passive (`*食べされる`, `*話さされる`, `*される`)
const SUB_NONE: u8 = 0;
const SUB_V5_D: u8 = 1 << 0;
const SUB_V5_SP: u8 = 1 << 1;
const SUB_V5_SS: u8 = 1 << 2;
const SUB_V5_ALL: u8 = SUB_V5_D | SUB_V5_SP | SUB_V5_SS;
const SUB_CAUS_LONG: u8 = 1 << 3;

struct Rule {
    base: &'static str,
    conjugated: &'static str,
    base_type: WordForm,
    conjugated_type: WordForm,
    /// Sub-conditions produced when `base_type == GodanVerb` (for silent short-causative chaining).
    v5_out: u8,
    /// Required sub-condition mask when `conjugated_type == Causative`.
    caus_req: u8,
}

impl Rule {
    const fn new(
        base: &'static str,
        conjugated: &'static str,
        base_type: WordForm,
        conjugated_type: WordForm,
    ) -> Self {
        let v5_out = if matches!(base_type, W::GodanVerb) {
            match conjugated_type {
                // Passive `す <- される` allows dictionary v5 and non-`さす` short causative (`書かされる`),
                // NEVER `さす` short causative (`*食べされる`, `*話さされる`, `*勉強される` as causative).
                W::Passive => SUB_V5_D | SUB_V5_SP,
                // Potential `す <- せる` and Causative `す <- させる` apply only to dictionary v5,
                // never to short causatives (`食べさせる` / `書かせる` are plain causatives, not potentials).
                W::Potential | W::Causative => SUB_V5_D,
                _ => SUB_V5_ALL,
            }
        } else {
            SUB_NONE
        };
        let caus_req = if matches!(conjugated_type, W::Causative) {
            SUB_CAUS_LONG
        } else {
            SUB_NONE
        };
        Self {
            base,
            conjugated,
            base_type,
            conjugated_type,
            v5_out,
            caus_req,
        }
    }

    const fn short_causative(
        base: &'static str,
        conjugated: &'static str,
        base_type: WordForm,
        caus_req: u8,
    ) -> Self {
        Self {
            base,
            conjugated,
            base_type,
            conjugated_type: W::Causative,
            v5_out: SUB_V5_D,
            caus_req,
        }
    }
}

struct SilentRule {
    base: &'static str,
    base_type: WordForm,
    conjugated_type: WordForm,
    /// For `conjugated_type == GodanVerb`, requires overlap with the parent rule's `v5_out`.
    required_v5_sub: u8,
    /// Passed to `deconjugate_recursive` when `base_type == Causative`.
    caus_sub: u8,
}

impl SilentRule {
    const fn new(base: &'static str, base_type: WordForm, conjugated_type: WordForm) -> Self {
        let caus_sub = if matches!(base_type, W::Causative) {
            SUB_CAUS_LONG
        } else {
            SUB_NONE
        };
        let required_v5_sub = if matches!(conjugated_type, W::GodanVerb) {
            SUB_V5_D
        } else {
            SUB_NONE
        };
        Self {
            base,
            base_type,
            conjugated_type,
            required_v5_sub,
            caus_sub,
        }
    }

    const fn short_causative_v5(base: &'static str, sub: u8) -> Self {
        Self {
            base,
            base_type: W::Causative,
            conjugated_type: W::GodanVerb,
            required_v5_sub: sub,
            caus_sub: sub,
        }
    }
}

// These rules keep the C++ table order: the recursion is depth-first, without
// deduplication, and the rule order determines result order.
const SILENT_RULES: &[SilentRule] = &[
    SilentRule::new("ない", W::Negative, W::Adjective),
    SilentRule::new("たい", W::Tai, W::Adjective),
    SilentRule::new("せる", W::Causative, W::IchidanVerb),
    SilentRule::short_causative_v5("す", SUB_V5_SP),
    SilentRule::short_causative_v5("す", SUB_V5_SS),
    SilentRule::new("れる", W::Passive, W::IchidanVerb),
    SilentRule::new("る", W::Potential, W::IchidanVerb),
    SilentRule::new("られる", W::PotentialPassive, W::IchidanVerb),
    SilentRule::new("しまう", W::Shimau, W::GodanVerb),
    SilentRule::new("ゃう", W::Chau, W::GodanVerb),
    SilentRule::new("まう", W::Chimau, W::GodanVerb),
    SilentRule::new("る", W::Continuous, W::IchidanVerb),
    SilentRule::new("おる", W::Continuous, W::GodanVerb),
    SilentRule::new("とる", W::Continuous, W::GodanVerb),
    SilentRule::new("どる", W::Continuous, W::GodanVerb),
    SilentRule::new("すぎる", W::Sugiru, W::IchidanVerb),
    SilentRule::new("とく", W::Toku, W::GodanVerb),
    SilentRule::new("どく", W::Toku, W::GodanVerb),
];

const RULES: &[Rule] = &[
    //Negative
    Rule::new("る", "らない", W::GodanVerb, W::Negative),
    Rule::new("う", "わない", W::GodanVerb, W::Negative),
    Rule::new("つ", "たない", W::GodanVerb, W::Negative),
    Rule::new("す", "さない", W::GodanVerb, W::Negative),
    Rule::new("く", "かない", W::GodanVerb, W::Negative),
    Rule::new("ぐ", "がない", W::GodanVerb, W::Negative),
    Rule::new("ぶ", "ばない", W::GodanVerb, W::Negative),
    Rule::new("む", "まない", W::GodanVerb, W::Negative),
    Rule::new("ぬ", "なない", W::GodanVerb, W::Negative),
    Rule::new("る", "ない", W::IchidanVerb, W::Negative),
    Rule::new("くる", "こない", W::KuruVerb, W::Negative),
    Rule::new("来る", "来ない", W::KuruVerb, W::Negative),
    Rule::new("する", "しない", W::SuruVerb, W::Negative),
    Rule::new("為る", "為ない", W::SuruVerb, W::Negative),
    //Past
    Rule::new("る", "った", W::GodanVerb, W::Past),
    Rule::new("う", "った", W::GodanVerb, W::Past),
    Rule::new("つ", "った", W::GodanVerb, W::Past),
    Rule::new("す", "した", W::GodanVerb, W::Past),
    Rule::new("く", "いた", W::GodanVerb, W::Past),
    Rule::new("ぐ", "いだ", W::GodanVerb, W::Past),
    Rule::new("ぶ", "んだ", W::GodanVerb, W::Past),
    Rule::new("む", "んだ", W::GodanVerb, W::Past),
    Rule::new("ぬ", "んだ", W::GodanVerb, W::Past),
    Rule::new("る", "た", W::IchidanVerb, W::Past),
    Rule::new("くる", "きた", W::KuruVerb, W::Past),
    Rule::new("来る", "来た", W::KuruVerb, W::Past),
    Rule::new("する", "した", W::SuruVerb, W::Past),
    Rule::new("為る", "為た", W::SuruVerb, W::Past),
    Rule::new("行く", "行った", W::GodanVerb, W::Past),
    Rule::new("いく", "いった", W::GodanVerb, W::Past),
    Rule::new("問う", "問うた", W::GodanVerb, W::Past),
    Rule::new("とう", "とうた", W::GodanVerb, W::Past),
    Rule::new("請う", "請うた", W::GodanVerb, W::Past),
    Rule::new("こう", "こうた", W::GodanVerb, W::Past),
    //Te
    Rule::new("る", "って", W::GodanVerb, W::Te),
    Rule::new("う", "って", W::GodanVerb, W::Te),
    Rule::new("つ", "って", W::GodanVerb, W::Te),
    Rule::new("す", "して", W::GodanVerb, W::Te),
    Rule::new("く", "いて", W::GodanVerb, W::Te),
    Rule::new("ぐ", "いで", W::GodanVerb, W::Te),
    Rule::new("ぶ", "んで", W::GodanVerb, W::Te),
    Rule::new("ぬ", "んで", W::GodanVerb, W::Te),
    Rule::new("む", "んで", W::GodanVerb, W::Te),
    Rule::new("る", "て", W::IchidanVerb, W::Te),
    Rule::new("くる", "きて", W::KuruVerb, W::Te),
    Rule::new("来る", "来て", W::KuruVerb, W::Te),
    Rule::new("する", "して", W::SuruVerb, W::Te),
    Rule::new("為る", "為て", W::SuruVerb, W::Te),
    Rule::new("行く", "行って", W::GodanVerb, W::Te),
    Rule::new("いく", "いって", W::GodanVerb, W::Te),
    Rule::new("問う", "問うて", W::GodanVerb, W::Te),
    Rule::new("とう", "とうて", W::GodanVerb, W::Te),
    Rule::new("請う", "請うて", W::GodanVerb, W::Te),
    Rule::new("こう", "こうて", W::GodanVerb, W::Te),
    //toku
    Rule::new("る", "っとく", W::GodanVerb, W::Toku),
    Rule::new("う", "っとく", W::GodanVerb, W::Toku),
    Rule::new("つ", "っとく", W::GodanVerb, W::Toku),
    Rule::new("す", "しとく", W::GodanVerb, W::Toku),
    Rule::new("く", "いとく", W::GodanVerb, W::Toku),
    Rule::new("ぐ", "いどく", W::GodanVerb, W::Toku),
    Rule::new("ぶ", "んどく", W::GodanVerb, W::Toku),
    Rule::new("ぬ", "んどく", W::GodanVerb, W::Toku),
    Rule::new("む", "んどく", W::GodanVerb, W::Toku),
    Rule::new("る", "とく", W::IchidanVerb, W::Toku),
    Rule::new("くる", "きとく", W::KuruVerb, W::Toku),
    Rule::new("来る", "来とく", W::KuruVerb, W::Toku),
    Rule::new("する", "しとく", W::SuruVerb, W::Toku),
    Rule::new("為る", "為とく", W::SuruVerb, W::Toku),
    Rule::new("行く", "行っとく", W::GodanVerb, W::Toku),
    Rule::new("いく", "いっとく", W::GodanVerb, W::Toku),
    Rule::new("問う", "問うとく", W::GodanVerb, W::Toku),
    Rule::new("とう", "とうとく", W::GodanVerb, W::Toku),
    Rule::new("請う", "請うとく", W::GodanVerb, W::Toku),
    Rule::new("こう", "こうとく", W::GodanVerb, W::Toku),
    //Imperative
    Rule::new("る", "れ", W::GodanVerb, W::Imperative),
    Rule::new("う", "え", W::GodanVerb, W::Imperative),
    Rule::new("つ", "て", W::GodanVerb, W::Imperative),
    Rule::new("す", "せ", W::GodanVerb, W::Imperative),
    Rule::new("く", "け", W::GodanVerb, W::Imperative),
    Rule::new("ぐ", "げ", W::GodanVerb, W::Imperative),
    Rule::new("ぶ", "べ", W::GodanVerb, W::Imperative),
    Rule::new("む", "め", W::GodanVerb, W::Imperative),
    Rule::new("ぬ", "ね", W::GodanVerb, W::Imperative),
    Rule::new("る", "ろ", W::IchidanVerb, W::Imperative),
    Rule::new("る", "よ", W::IchidanVerb, W::Imperative),
    Rule::new("くる", "こい", W::KuruVerb, W::Imperative),
    Rule::new("来る", "来い", W::KuruVerb, W::Imperative),
    Rule::new("する", "しろ", W::SuruVerb, W::Imperative),
    Rule::new("為る", "為ろ", W::SuruVerb, W::Imperative),
    Rule::new("する", "せよ", W::SuruVerb, W::Imperative),
    Rule::new("為る", "為よ", W::SuruVerb, W::Imperative),
    //Volitional
    Rule::new("る", "ろう", W::GodanVerb, W::Volitional),
    Rule::new("う", "おう", W::GodanVerb, W::Volitional),
    Rule::new("つ", "とう", W::GodanVerb, W::Volitional),
    Rule::new("す", "そう", W::GodanVerb, W::Volitional),
    Rule::new("く", "こう", W::GodanVerb, W::Volitional),
    Rule::new("ぐ", "ごう", W::GodanVerb, W::Volitional),
    Rule::new("ぶ", "ぼう", W::GodanVerb, W::Volitional),
    Rule::new("む", "もう", W::GodanVerb, W::Volitional),
    Rule::new("ぬ", "のう", W::GodanVerb, W::Volitional),
    Rule::new("る", "よう", W::IchidanVerb, W::Volitional),
    Rule::new("くる", "こよう", W::KuruVerb, W::Volitional),
    Rule::new("来る", "来よう", W::KuruVerb, W::Volitional),
    Rule::new("する", "しよう", W::SuruVerb, W::Volitional),
    Rule::new("為る", "為よう", W::SuruVerb, W::Volitional),
    //Passive
    Rule::new("る", "られる", W::GodanVerb, W::Passive),
    Rule::new("う", "われる", W::GodanVerb, W::Passive),
    Rule::new("つ", "たれる", W::GodanVerb, W::Passive),
    Rule::new("す", "される", W::GodanVerb, W::Passive),
    Rule::new("く", "かれる", W::GodanVerb, W::Passive),
    Rule::new("ぐ", "がれる", W::GodanVerb, W::Passive),
    Rule::new("ぶ", "ばれる", W::GodanVerb, W::Passive),
    Rule::new("む", "まれる", W::GodanVerb, W::Passive),
    Rule::new("ぬ", "なれる", W::GodanVerb, W::Passive),
    Rule::new("る", "られる", W::IchidanVerb, W::PotentialPassive),
    Rule::new("くる", "こられる", W::KuruVerb, W::PotentialPassive),
    Rule::new("来る", "来られる", W::KuruVerb, W::PotentialPassive),
    Rule::new("する", "される", W::SuruVerb, W::Passive),
    Rule::new("為る", "為れる", W::SuruVerb, W::Passive),
    //Potential
    Rule::new("る", "れる", W::GodanVerb, W::Potential),
    Rule::new("う", "える", W::GodanVerb, W::Potential),
    Rule::new("つ", "てる", W::GodanVerb, W::Potential),
    Rule::new("す", "せる", W::GodanVerb, W::Potential),
    Rule::new("く", "ける", W::GodanVerb, W::Potential),
    Rule::new("ぐ", "げる", W::GodanVerb, W::Potential),
    Rule::new("ぶ", "べる", W::GodanVerb, W::Potential),
    Rule::new("む", "める", W::GodanVerb, W::Potential),
    Rule::new("ぬ", "ねる", W::GodanVerb, W::Potential),
    Rule::new("る", "れる", W::IchidanVerb, W::Potential),
    Rule::new("くる", "これる", W::KuruVerb, W::Potential),
    Rule::new("来る", "来れる", W::KuruVerb, W::Potential),
    Rule::new("する", "できる", W::SuruVerb, W::Potential),
    //Causative (long -seru/-saseru)
    Rule::new("る", "らせる", W::GodanVerb, W::Causative),
    Rule::new("う", "わせる", W::GodanVerb, W::Causative),
    Rule::new("つ", "たせる", W::GodanVerb, W::Causative),
    Rule::new("す", "させる", W::GodanVerb, W::Causative),
    Rule::new("く", "かせる", W::GodanVerb, W::Causative),
    Rule::new("ぐ", "がせる", W::GodanVerb, W::Causative),
    Rule::new("ぶ", "ばせる", W::GodanVerb, W::Causative),
    Rule::new("む", "ませる", W::GodanVerb, W::Causative),
    Rule::new("ぬ", "なせる", W::GodanVerb, W::Causative),
    Rule::new("る", "させる", W::IchidanVerb, W::Causative),
    Rule::new("くる", "こさせる", W::KuruVerb, W::Causative),
    Rule::new("来る", "来させる", W::KuruVerb, W::Causative),
    Rule::new("する", "させる", W::SuruVerb, W::Causative),
    Rule::new("為る", "為せる", W::SuruVerb, W::Causative),
    //Causative (short -su/-sasu with Yomitan v5sp vs v5ss sub-conditions)
    Rule::short_causative("る", "らす", W::GodanVerb, SUB_CAUS_LONG | SUB_V5_SP),
    Rule::short_causative("う", "わす", W::GodanVerb, SUB_CAUS_LONG | SUB_V5_SP),
    Rule::short_causative("つ", "たす", W::GodanVerb, SUB_CAUS_LONG | SUB_V5_SP),
    Rule::short_causative("す", "さす", W::GodanVerb, SUB_CAUS_LONG | SUB_V5_SS),
    Rule::short_causative("く", "かす", W::GodanVerb, SUB_CAUS_LONG | SUB_V5_SP),
    Rule::short_causative("ぐ", "がす", W::GodanVerb, SUB_CAUS_LONG | SUB_V5_SP),
    Rule::short_causative("ぶ", "ばす", W::GodanVerb, SUB_CAUS_LONG | SUB_V5_SP),
    Rule::short_causative("む", "ます", W::GodanVerb, SUB_CAUS_LONG | SUB_V5_SP),
    Rule::short_causative("ぬ", "なす", W::GodanVerb, SUB_CAUS_LONG | SUB_V5_SP),
    Rule::short_causative("る", "さす", W::IchidanVerb, SUB_CAUS_LONG | SUB_V5_SS),
    Rule::short_causative("くる", "こさす", W::KuruVerb, SUB_CAUS_LONG | SUB_V5_SS),
    Rule::short_causative("来る", "来さす", W::KuruVerb, SUB_CAUS_LONG | SUB_V5_SS),
    Rule::short_causative("する", "さす", W::SuruVerb, SUB_CAUS_LONG | SUB_V5_SS),
    Rule::short_causative("為る", "為す", W::SuruVerb, SUB_CAUS_LONG | SUB_V5_SS),
    //Ba
    Rule::new("る", "れば", W::GodanVerb, W::Ba),
    Rule::new("う", "えば", W::GodanVerb, W::Ba),
    Rule::new("つ", "てば", W::GodanVerb, W::Ba),
    Rule::new("す", "せば", W::GodanVerb, W::Ba),
    Rule::new("く", "けば", W::GodanVerb, W::Ba),
    Rule::new("ぐ", "げば", W::GodanVerb, W::Ba),
    Rule::new("ぶ", "べば", W::GodanVerb, W::Ba),
    Rule::new("む", "めば", W::GodanVerb, W::Ba),
    Rule::new("ぬ", "ねば", W::GodanVerb, W::Ba),
    Rule::new("る", "れば", W::IchidanVerb, W::Ba),
    Rule::new("くる", "くれば", W::KuruVerb, W::Ba),
    Rule::new("来る", "来れば", W::KuruVerb, W::Ba),
    Rule::new("する", "すれば", W::SuruVerb, W::Ba),
    Rule::new("為る", "為れば", W::SuruVerb, W::Ba),
    //Zaru
    Rule::new("る", "らざる", W::GodanVerb, W::Zaru),
    Rule::new("う", "わざる", W::GodanVerb, W::Zaru),
    Rule::new("つ", "たざる", W::GodanVerb, W::Zaru),
    Rule::new("す", "さざる", W::GodanVerb, W::Zaru),
    Rule::new("く", "かざる", W::GodanVerb, W::Zaru),
    Rule::new("ぐ", "がざる", W::GodanVerb, W::Zaru),
    Rule::new("ぶ", "ばざる", W::GodanVerb, W::Zaru),
    Rule::new("む", "まざる", W::GodanVerb, W::Zaru),
    Rule::new("ぬ", "なざる", W::GodanVerb, W::Zaru),
    Rule::new("る", "ざる", W::IchidanVerb, W::Zaru),
    Rule::new("くる", "こざる", W::KuruVerb, W::Zaru),
    Rule::new("来る", "来ざる", W::KuruVerb, W::Zaru),
    Rule::new("する", "せざる", W::SuruVerb, W::Zaru),
    Rule::new("為る", "為ざる", W::SuruVerb, W::Zaru),
    //Neba
    Rule::new("る", "らねば", W::GodanVerb, W::Neba),
    Rule::new("う", "わねば", W::GodanVerb, W::Neba),
    Rule::new("つ", "たねば", W::GodanVerb, W::Neba),
    Rule::new("す", "さねば", W::GodanVerb, W::Neba),
    Rule::new("く", "かねば", W::GodanVerb, W::Neba),
    Rule::new("ぐ", "がねば", W::GodanVerb, W::Neba),
    Rule::new("ぶ", "ばねば", W::GodanVerb, W::Neba),
    Rule::new("む", "まねば", W::GodanVerb, W::Neba),
    Rule::new("ぬ", "なねば", W::GodanVerb, W::Neba),
    Rule::new("る", "ねば", W::IchidanVerb, W::Neba),
    Rule::new("くる", "こねば", W::KuruVerb, W::Neba),
    Rule::new("来る", "来ねば", W::KuruVerb, W::Neba),
    Rule::new("する", "せねば", W::SuruVerb, W::Neba),
    Rule::new("為る", "為ねば", W::SuruVerb, W::Neba),
    //Zu
    Rule::new("る", "らず", W::GodanVerb, W::Zu),
    Rule::new("う", "わず", W::GodanVerb, W::Zu),
    Rule::new("つ", "たず", W::GodanVerb, W::Zu),
    Rule::new("す", "さず", W::GodanVerb, W::Zu),
    Rule::new("く", "かず", W::GodanVerb, W::Zu),
    Rule::new("ぐ", "がず", W::GodanVerb, W::Zu),
    Rule::new("ぶ", "ばず", W::GodanVerb, W::Zu),
    Rule::new("む", "まず", W::GodanVerb, W::Zu),
    Rule::new("ぬ", "なず", W::GodanVerb, W::Zu),
    Rule::new("る", "ず", W::IchidanVerb, W::Zu),
    Rule::new("くる", "こず", W::KuruVerb, W::Zu),
    Rule::new("来る", "来ず", W::KuruVerb, W::Zu),
    Rule::new("する", "せず", W::SuruVerb, W::Zu),
    Rule::new("為る", "為ず", W::SuruVerb, W::Zu),
    //Nu
    Rule::new("る", "らぬ", W::GodanVerb, W::Nu),
    Rule::new("う", "わぬ", W::GodanVerb, W::Nu),
    Rule::new("つ", "たぬ", W::GodanVerb, W::Nu),
    Rule::new("す", "さぬ", W::GodanVerb, W::Nu),
    Rule::new("く", "かぬ", W::GodanVerb, W::Nu),
    Rule::new("ぐ", "がぬ", W::GodanVerb, W::Nu),
    Rule::new("ぶ", "ばぬ", W::GodanVerb, W::Nu),
    Rule::new("む", "まぬ", W::GodanVerb, W::Nu),
    Rule::new("ぬ", "なぬ", W::GodanVerb, W::Nu),
    Rule::new("る", "ぬ", W::IchidanVerb, W::Nu),
    Rule::new("くる", "こぬ", W::KuruVerb, W::Nu),
    Rule::new("来る", "来ぬ", W::KuruVerb, W::Nu),
    Rule::new("する", "せぬ", W::SuruVerb, W::Nu),
    Rule::new("為る", "為ぬ", W::SuruVerb, W::Nu),
    //Colloquial Masculine Negative
    Rule::new("る", "らん", W::GodanVerb, W::ColloquialNegative),
    Rule::new("う", "わん", W::GodanVerb, W::ColloquialNegative),
    Rule::new("つ", "たん", W::GodanVerb, W::ColloquialNegative),
    Rule::new("す", "さん", W::GodanVerb, W::ColloquialNegative),
    Rule::new("く", "かん", W::GodanVerb, W::ColloquialNegative),
    Rule::new("ぐ", "がん", W::GodanVerb, W::ColloquialNegative),
    Rule::new("ぶ", "ばん", W::GodanVerb, W::ColloquialNegative),
    Rule::new("む", "まん", W::GodanVerb, W::ColloquialNegative),
    Rule::new("ぬ", "なん", W::GodanVerb, W::ColloquialNegative),
    Rule::new("る", "ん", W::IchidanVerb, W::ColloquialNegative),
    Rule::new("くる", "こん", W::KuruVerb, W::ColloquialNegative),
    Rule::new("来る", "来ん", W::KuruVerb, W::ColloquialNegative),
    Rule::new("する", "せん", W::SuruVerb, W::ColloquialNegative),
    Rule::new("為る", "為ん", W::SuruVerb, W::ColloquialNegative),
    //Colloquial provisional Negative
    Rule::new("る", "らなきゃ", W::GodanVerb, W::ProvisionalColloquialNegative),
    Rule::new("う", "わなきゃ", W::GodanVerb, W::ProvisionalColloquialNegative),
    Rule::new("つ", "たなきゃ", W::GodanVerb, W::ProvisionalColloquialNegative),
    Rule::new("す", "さなきゃ", W::GodanVerb, W::ProvisionalColloquialNegative),
    Rule::new("く", "かなきゃ", W::GodanVerb, W::ProvisionalColloquialNegative),
    Rule::new("ぐ", "がなきゃ", W::GodanVerb, W::ProvisionalColloquialNegative),
    Rule::new("ぶ", "ばなきゃ", W::GodanVerb, W::ProvisionalColloquialNegative),
    Rule::new("む", "まなきゃ", W::GodanVerb, W::ProvisionalColloquialNegative),
    Rule::new("ぬ", "ななきゃ", W::GodanVerb, W::ProvisionalColloquialNegative),
    Rule::new("る", "なきゃ", W::IchidanVerb, W::ProvisionalColloquialNegative),
    Rule::new("くる", "こなきゃ", W::KuruVerb, W::ProvisionalColloquialNegative),
    Rule::new("来る", "来なきゃ", W::KuruVerb, W::ProvisionalColloquialNegative),
    Rule::new("する", "しなきゃ", W::SuruVerb, W::ProvisionalColloquialNegative),
    Rule::new("為る", "為なきゃ", W::SuruVerb, W::ProvisionalColloquialNegative),
    //Imperative Negative
    Rule::new("る", "るな", W::GodanVerb, W::ImperativeNegative),
    Rule::new("う", "うな", W::GodanVerb, W::ImperativeNegative),
    Rule::new("つ", "つな", W::GodanVerb, W::ImperativeNegative),
    Rule::new("す", "すな", W::GodanVerb, W::ImperativeNegative),
    Rule::new("く", "くな", W::GodanVerb, W::ImperativeNegative),
    Rule::new("ぐ", "ぐな", W::GodanVerb, W::ImperativeNegative),
    Rule::new("ぶ", "ぶな", W::GodanVerb, W::ImperativeNegative),
    Rule::new("む", "むな", W::GodanVerb, W::ImperativeNegative),
    Rule::new("ぬ", "ぬな", W::GodanVerb, W::ImperativeNegative),
    Rule::new("る", "るな", W::IchidanVerb, W::ImperativeNegative),
    Rule::new("くる", "くるな", W::KuruVerb, W::ImperativeNegative),
    Rule::new("来る", "来るな", W::KuruVerb, W::ImperativeNegative),
    Rule::new("する", "するな", W::SuruVerb, W::ImperativeNegative),
    Rule::new("為る", "為るな", W::SuruVerb, W::ImperativeNegative),
    //Tari
    Rule::new("る", "ったり", W::GodanVerb, W::Tari),
    Rule::new("う", "ったり", W::GodanVerb, W::Tari),
    Rule::new("つ", "ったり", W::GodanVerb, W::Tari),
    Rule::new("す", "したり", W::GodanVerb, W::Tari),
    Rule::new("く", "いたり", W::GodanVerb, W::Tari),
    Rule::new("ぐ", "いだり", W::GodanVerb, W::Tari),
    Rule::new("ぶ", "んだり", W::GodanVerb, W::Tari),
    Rule::new("む", "んだり", W::GodanVerb, W::Tari),
    Rule::new("ぬ", "んだり", W::GodanVerb, W::Tari),
    Rule::new("る", "たり", W::IchidanVerb, W::Tari),
    Rule::new("くる", "きたり", W::KuruVerb, W::Tari),
    Rule::new("来る", "来たり", W::KuruVerb, W::Tari),
    Rule::new("する", "したり", W::SuruVerb, W::Tari),
    Rule::new("為る", "為たり", W::SuruVerb, W::Tari),
    Rule::new("行く", "行ったり", W::GodanVerb, W::Tari),
    Rule::new("いく", "いったり", W::GodanVerb, W::Tari),
    Rule::new("問う", "問うたり", W::GodanVerb, W::Tari),
    Rule::new("とう", "とうたり", W::GodanVerb, W::Tari),
    Rule::new("請う", "請うたり", W::GodanVerb, W::Tari),
    Rule::new("こう", "こうたり", W::GodanVerb, W::Tari),
    //Chau
    Rule::new("る", "っちゃう", W::GodanVerb, W::Chau),
    Rule::new("う", "っちゃう", W::GodanVerb, W::Chau),
    Rule::new("つ", "っちゃう", W::GodanVerb, W::Chau),
    Rule::new("す", "しちゃう", W::GodanVerb, W::Chau),
    Rule::new("く", "いちゃう", W::GodanVerb, W::Chau),
    Rule::new("ぐ", "いじゃう", W::GodanVerb, W::Chau),
    Rule::new("ぶ", "んじゃう", W::GodanVerb, W::Chau),
    Rule::new("ぬ", "んじゃう", W::GodanVerb, W::Chau),
    Rule::new("む", "んじゃう", W::GodanVerb, W::Chau),
    Rule::new("る", "ちゃう", W::IchidanVerb, W::Chau),
    Rule::new("くる", "きちゃう", W::KuruVerb, W::Chau),
    Rule::new("来る", "来ちゃう", W::KuruVerb, W::Chau),
    Rule::new("する", "しちゃう", W::SuruVerb, W::Chau),
    Rule::new("為る", "為ちゃう", W::SuruVerb, W::Chau),
    Rule::new("行く", "行っちゃう", W::GodanVerb, W::Chau),
    Rule::new("いく", "いっちゃう", W::GodanVerb, W::Chau),
    Rule::new("問う", "問うちゃう", W::GodanVerb, W::Chau),
    Rule::new("とう", "とうちゃう", W::GodanVerb, W::Chau),
    Rule::new("請う", "請うちゃう", W::GodanVerb, W::Chau),
    Rule::new("こう", "こうちゃう", W::GodanVerb, W::Chau),
    //Chimau
    Rule::new("る", "っちまう", W::GodanVerb, W::Chimau),
    Rule::new("う", "っちまう", W::GodanVerb, W::Chimau),
    Rule::new("つ", "っちまう", W::GodanVerb, W::Chimau),
    Rule::new("す", "しちまう", W::GodanVerb, W::Chimau),
    Rule::new("く", "いちまう", W::GodanVerb, W::Chimau),
    Rule::new("ぐ", "いじまう", W::GodanVerb, W::Chimau),
    Rule::new("ぐ", "いぢまう", W::GodanVerb, W::Chimau),
    Rule::new("ぶ", "んじまう", W::GodanVerb, W::Chimau),
    Rule::new("ぶ", "んぢまう", W::GodanVerb, W::Chimau),
    Rule::new("ぬ", "んじまう", W::GodanVerb, W::Chimau),
    Rule::new("ぬ", "んぢまう", W::GodanVerb, W::Chimau),
    Rule::new("む", "んじまう", W::GodanVerb, W::Chimau),
    Rule::new("む", "んぢまう", W::GodanVerb, W::Chimau),
    Rule::new("る", "ちまう", W::IchidanVerb, W::Chimau),
    Rule::new("くる", "きちまう", W::KuruVerb, W::Chimau),
    Rule::new("来る", "来ちまう", W::KuruVerb, W::Chimau),
    Rule::new("する", "しちまう", W::SuruVerb, W::Chimau),
    Rule::new("為る", "為ちまう", W::SuruVerb, W::Chimau),
    Rule::new("行く", "行っちまう", W::GodanVerb, W::Chimau),
    Rule::new("いく", "いっちまう", W::GodanVerb, W::Chimau),
    Rule::new("問う", "問うちまう", W::GodanVerb, W::Chimau),
    Rule::new("とう", "とうちまう", W::GodanVerb, W::Chimau),
    Rule::new("請う", "請うちまう", W::GodanVerb, W::Chimau),
    Rule::new("こう", "こうちまう", W::GodanVerb, W::Chimau),
    //Continuous
    Rule::new("で", "でいる", W::Te, W::Continuous),
    Rule::new("て", "ている", W::Te, W::Continuous),
    Rule::new("で", "でおる", W::Te, W::Continuous),
    Rule::new("て", "ておる", W::Te, W::Continuous),
    Rule::new("で", "でる", W::Te, W::Continuous),
    Rule::new("て", "てる", W::Te, W::Continuous),
    Rule::new("て", "とる", W::Te, W::Continuous),
    Rule::new("で", "どる", W::Te, W::Continuous),
    //Shimau
    Rule::new("で", "でしまう", W::Te, W::Shimau),
    Rule::new("て", "てしまう", W::Te, W::Shimau),
    //Adjectives
    Rule::new("い", "くて", W::Adjective, W::Te),
    Rule::new("い", "く", W::Adjective, W::Adverbial),
    Rule::new("い", "くない", W::Adjective, W::Negative),
    Rule::new("い", "かった", W::Adjective, W::Past),
    Rule::new("い", "ければ", W::Adjective, W::Ba),
    Rule::new("い", "くなきゃ", W::Adjective, W::ProvisionalColloquialNegative),
    Rule::new("い", "かったら", W::Adjective, W::Tara),
    Rule::new("い", "さ", W::Adjective, W::Noun),
    Rule::new("い", "そう", W::Adjective, W::Sou),
    Rule::new("い", "すぎる", W::Adjective, W::Sugiru),
    Rule::new("い", "き", W::Adjective, W::Ki),
    Rule::new("い", "かろう", W::Adjective, W::Volitional),
    Rule::new("ない", "ねえ", W::Adjective, W::E),
    Rule::new("ない", "ねぇ", W::Adjective, W::E),
    Rule::new("ない", "ねー", W::Adjective, W::E),
    Rule::new("たい", "てえ", W::Adjective, W::E),
    Rule::new("たい", "てぇ", W::Adjective, W::E),
    Rule::new("たい", "てー", W::Adjective, W::E),
    //Conjunctive (Masu stem)
    Rule::new("る", "り", W::GodanVerb, W::Conjunctive),
    Rule::new("う", "い", W::GodanVerb, W::Conjunctive),
    Rule::new("つ", "ち", W::GodanVerb, W::Conjunctive),
    Rule::new("す", "し", W::GodanVerb, W::Conjunctive),
    Rule::new("く", "き", W::GodanVerb, W::Conjunctive),
    Rule::new("ぐ", "ぎ", W::GodanVerb, W::Conjunctive),
    Rule::new("ぶ", "び", W::GodanVerb, W::Conjunctive),
    Rule::new("む", "み", W::GodanVerb, W::Conjunctive),
    Rule::new("ぬ", "に", W::GodanVerb, W::Conjunctive),
    Rule::new("る", "", W::IchidanVerb, W::Conjunctive),
    Rule::new("くる", "き", W::KuruVerb, W::Conjunctive),
    Rule::new("来る", "来", W::KuruVerb, W::Conjunctive),
    Rule::new("する", "し", W::SuruVerb, W::Conjunctive),
    Rule::new("為る", "為", W::SuruVerb, W::Conjunctive),
    //Stem forms
    Rule::new("", "ます", W::Conjunctive, W::Polite),
    Rule::new("ます", "ません", W::Polite, W::Negative),
    Rule::new("ます", "ました", W::Polite, W::Past),
    Rule::new("ます", "ましょう", W::Polite, W::Volitional),
    Rule::new("せん", "せんでした", W::Negative, W::Past),
    Rule::new("", "たら", W::Conjunctive, W::Tara),
    Rule::new("", "たい", W::Conjunctive, W::Tai),
    Rule::new("", "なさい", W::Conjunctive, W::Nasai),
    Rule::new("", "そう", W::Conjunctive, W::Sou),
    Rule::new("", "すぎる", W::Conjunctive, W::Sugiru),
];

/// UTF-16 rule fragments, compiled once rather than allocated again for every query.
struct CompiledRule {
    base: Vec<u16>,
    conjugated: Vec<u16>,
    base_type: WordForm,
    conjugated_type: WordForm,
    v5_out: u8,
    caus_req: u8,
}

impl From<&Rule> for CompiledRule {
    fn from(rule: &Rule) -> Self {
        Self {
            base: rule.base.encode_utf16().collect(),
            conjugated: rule.conjugated.encode_utf16().collect(),
            base_type: rule.base_type,
            conjugated_type: rule.conjugated_type,
            v5_out: rule.v5_out,
            caus_req: rule.caus_req,
        }
    }
}

struct CompiledSilentRule {
    base: Vec<u16>,
    base_type: WordForm,
    conjugated_type: WordForm,
    required_v5_sub: u8,
    caus_sub: u8,
}

impl From<&SilentRule> for CompiledSilentRule {
    fn from(rule: &SilentRule) -> Self {
        Self {
            base: rule.base.encode_utf16().collect(),
            base_type: rule.base_type,
            conjugated_type: rule.conjugated_type,
            required_v5_sub: rule.required_v5_sub,
            caus_sub: rule.caus_sub,
        }
    }
}

fn compiled_rules(
    source: &'static [Rule],
    compiled: &'static OnceLock<Vec<CompiledRule>>,
) -> &'static [CompiledRule] {
    compiled
        .get_or_init(|| source.iter().map(CompiledRule::from).collect())
        .as_slice()
}

fn compiled_silent_rules(
    source: &'static [SilentRule],
    compiled: &'static OnceLock<Vec<CompiledSilentRule>>,
) -> &'static [CompiledSilentRule] {
    compiled
        .get_or_init(|| source.iter().map(CompiledSilentRule::from).collect())
        .as_slice()
}

fn is_terminal(form: WordForm) -> bool {
    matches!(
        form,
        WordForm::GodanVerb
            | WordForm::IchidanVerb
            | WordForm::SuruVerb
            | WordForm::KuruVerb
            | WordForm::Adjective
    )
}

fn format_derivation(derivations: &[WordForm]) -> Vec<u16> {
    let mut display = Vec::new();
    let mut first = true;
    for form in derivations.iter().copied() {
        if form == WordForm::Conjunctive || is_terminal(form) {
            continue;
        }
        if !first {
            display.extend(" « ".encode_utf16());
        }
        display.extend(word_form_label(form).encode_utf16());
        first = false;
    }
    // A bare masu stem is shown only when it is the final conjugated form.
    if derivations.last() == Some(&WordForm::Conjunctive) {
        if !first {
            display.extend(" « ".encode_utf16());
        }
        display.extend(word_form_label(WordForm::Conjunctive).encode_utf16());
    }
    display
}

fn create_derivation(
    word: &[u16],
    derivations: &[WordForm],
    rule_base: &[u16],
    rule_conjugated: &[u16],
    base_type: WordForm,
    conjugated_type: WordForm,
) -> (Vec<u16>, Vec<WordForm>) {
    let prefix_length = word.len() - rule_conjugated.len();
    let mut child_word = Vec::with_capacity(prefix_length + rule_base.len());
    child_word.extend_from_slice(&word[..prefix_length]);
    child_word.extend_from_slice(rule_base);

    let mut child_derivations =
        Vec::with_capacity(derivations.len() + 1 + usize::from(derivations.is_empty()));
    child_derivations.push(base_type);
    if derivations.is_empty() {
        child_derivations.push(conjugated_type);
    }
    child_derivations.extend_from_slice(derivations);
    (child_word, child_derivations)
}

#[allow(clippy::too_many_arguments)]
fn deconjugate_recursive(
    word: &[u16],
    derivations: &[WordForm],
    conjugated: &[u16],
    caus_sub: u8,
    results: &mut Vec<ConjugationInfo>,
    rules: &[CompiledRule],
    silent_rules: &[CompiledSilentRule],
) {
    let current_type = derivations.first().copied().unwrap_or(WordForm::Any);
    for rule in rules {
        if current_type != WordForm::Any && current_type != rule.conjugated_type {
            continue;
        }
        if rule.conjugated_type == WordForm::Causative {
            let active = if current_type == WordForm::Any {
                SUB_CAUS_LONG
            } else {
                caus_sub
            };
            if (active & rule.caus_req) == 0 {
                continue;
            }
        }
        if !word.ends_with(&rule.conjugated) {
            continue;
        }
        let prefix_length = word.len() - rule.conjugated.len();
        // A suffix-only terminal rule (`base.len() <= 1`, e.g. `る`/`う`/`い`) requires a non-empty stem.
        if prefix_length == 0 && rule.base.len() <= 1 && is_terminal(rule.base_type) {
            continue;
        }
        let (base, chain) = create_derivation(
            word,
            derivations,
            &rule.base,
            &rule.conjugated,
            rule.base_type,
            rule.conjugated_type,
        );
        if is_terminal(rule.base_type) {
            let result_index = results.len();
            results.push(ConjugationInfo {
                derivation_display: format_derivation(&chain),
                base,
                conjugated: conjugated.to_vec(),
                derivations: chain,
            });
            // C++ emits the terminal result first, then re-enters the original
            // parent with the silent rule's base type (not the terminal child).
            for silent_rule in silent_rules {
                if silent_rule.conjugated_type != rule.base_type {
                    continue;
                }
                if rule.base_type == WordForm::GodanVerb
                    && (rule.v5_out & silent_rule.required_v5_sub) == 0
                {
                    continue;
                }
                if results[result_index].base.ends_with(&silent_rule.base) {
                    let (derived_base, derived_chain) = create_derivation(
                        word,
                        derivations,
                        &rule.base,
                        &rule.conjugated,
                        silent_rule.base_type,
                        rule.conjugated_type,
                    );
                    deconjugate_recursive(
                        &derived_base,
                        &derived_chain,
                        conjugated,
                        silent_rule.caus_sub,
                        results,
                        rules,
                        silent_rules,
                    );
                }
            }
        } else {
            let next_caus = if rule.base_type == WordForm::Causative {
                SUB_CAUS_LONG
            } else {
                SUB_NONE
            };
            deconjugate_recursive(
                &base,
                &chain,
                conjugated,
                next_caus,
                results,
                rules,
                silent_rules,
            );
        }
    }
}

// QRegularExpression("\\s") with default pattern options uses ASCII PCRE2
// shorthand whitespace, not Rust's Unicode `char::is_whitespace`.
fn is_regex_whitespace(unit: u16) -> bool {
    matches!(unit, 0x0009..=0x000d | 0x0020)
}

fn deconjugate_prefix(
    conjugated: &[u16],
    results: &mut Vec<ConjugationInfo>,
    rules: &[CompiledRule],
    silent_rules: &[CompiledSilentRule],
) {
    // Qt's PCRE2 matcher validates the whole UTF-16 subject before finding
    // `\s`. On an unpaired surrogate, QString::remove(regex) makes no changes.
    if conjugated.iter().copied().any(is_regex_whitespace)
        && char::decode_utf16(conjugated.iter().copied()).all(|unit| unit.is_ok())
    {
        let word: Vec<u16> = conjugated
            .iter()
            .copied()
            .filter(|unit| !is_regex_whitespace(*unit))
            .collect();
        deconjugate_recursive(
            &word,
            &[],
            conjugated,
            SUB_CAUS_LONG,
            results,
            rules,
            silent_rules,
        );
    } else {
        deconjugate_recursive(
            conjugated,
            &[],
            conjugated,
            SUB_CAUS_LONG,
            results,
            rules,
            silent_rules,
        );
    }
}

/// Attempts all rule chains for `query`, retaining the C++ rule order and duplicates.
/// Sentence mode tries progressively shorter prefixes (in UTF-16 code units),
/// skipping whitespace at each trimmed suffix; otherwise only the full query
/// is tried. The original, unstripped prefix is stored as `conjugated`.
pub fn deconjugate(query: &[u16], sentence_mode: bool) -> Vec<ConjugationInfo> {
    static COMPILED_RULES: OnceLock<Vec<CompiledRule>> = OnceLock::new();
    static COMPILED_SILENT_RULES: OnceLock<Vec<CompiledSilentRule>> = OnceLock::new();
    let rules = compiled_rules(RULES, &COMPILED_RULES);
    let silent_rules = compiled_silent_rules(SILENT_RULES, &COMPILED_SILENT_RULES);
    let mut results = Vec::new();

    if sentence_mode {
        let mut conjugated = query.to_vec();
        while !conjugated.is_empty() {
            deconjugate_prefix(&conjugated, &mut results, rules, silent_rules);
            conjugated.pop();
            while conjugated.last().is_some_and(|unit| is_regex_whitespace(*unit)) {
                conjugated.pop();
            }
        }
    } else {
        deconjugate_prefix(query, &mut results, rules, silent_rules);
    }
    results
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bases_for(input: &str) -> Vec<(String, WordForm)> {
        let u16_in: Vec<u16> = input.encode_utf16().collect();
        deconjugate(&u16_in, false)
            .into_iter()
            .map(|info| {
                (
                    String::from_utf16_lossy(&info.base),
                    info.derivations[0],
                )
            })
            .collect()
    }

    #[test]
    fn fixes_gu_chau_chimau_and_blocks_unvoiced_false_match() {
        let chau = bases_for("泳いじゃう");
        assert!(chau.contains(&("泳ぐ".into(), WordForm::GodanVerb)));
        assert!(!chau.iter().any(|(b, _)| b == "泳く"));

        let chatta = bases_for("泳いじゃった");
        assert!(chatta.contains(&("泳ぐ".into(), WordForm::GodanVerb)));

        let chimau = bases_for("泳いじまう");
        assert!(chimau.contains(&("泳ぐ".into(), WordForm::GodanVerb)));
        assert!(!chimau.iter().any(|(b, _)| b == "泳く"));

        let unvoiced = bases_for("泳いちゃう");
        assert!(!unvoiced.iter().any(|(b, _)| b == "泳ぐ"));
    }

    #[test]
    fn fixes_tou_kou_chimau_voiced_doru_and_kana_iku_geminates() {
        assert!(bases_for("問うちまう").contains(&("問う".into(), WordForm::GodanVerb)));
        assert!(bases_for("請うちまう").contains(&("請う".into(), WordForm::GodanVerb)));
        assert!(bases_for("読んどる").contains(&("読む".into(), WordForm::GodanVerb)));
        assert!(bases_for("読んどった").contains(&("読む".into(), WordForm::GodanVerb)));
        assert!(bases_for("いっとく").contains(&("いく".into(), WordForm::GodanVerb)));
        assert!(bases_for("いったり").contains(&("いく".into(), WordForm::GodanVerb)));
        assert!(bases_for("いっちゃう").contains(&("いく".into(), WordForm::GodanVerb)));
        assert!(bases_for("いっちまう").contains(&("いく".into(), WordForm::GodanVerb)));
    }

    #[test]
    fn enforces_yomitan_sasu_passive_subconditions() {
        // Long causative-passive works for ichidan and godan:
        assert!(bases_for("食べさせられた").contains(&("食べる".into(), WordForm::IchidanVerb)));
        // Contracted short causative-passive works for non-su godan:
        assert!(bases_for("書かされた").contains(&("書く".into(), WordForm::GodanVerb)));
        assert!(bases_for("読まされた").contains(&("読む".into(), WordForm::GodanVerb)));
        // Short causative non-passive works for ichidan:
        assert!(bases_for("食べさした").contains(&("食べる".into(), WordForm::IchidanVerb)));

        // Invalid `さす` + passive is blocked:
        assert!(!bases_for("食べされる").iter().any(|(b, _)| b == "食べる"));
        assert!(!bases_for("食べされた").iter().any(|(b, _)| b == "食べる"));
        assert!(!bases_for("話さされる").iter().any(|(b, _)| b == "話す"));
        assert!(!bases_for("来される").iter().any(|(b, _)| b == "来る"));

        // `勉強される` is plain passive, never causative + passive:
        let benkyou_sareru = deconjugate(&"勉強される".encode_utf16().collect::<Vec<_>>(), false);
        assert!(benkyou_sareru
            .iter()
            .any(|i| String::from_utf16_lossy(&i.base) == "勉強する"
                && !i.derivations.contains(&WordForm::Causative)));
        assert!(!benkyou_sareru
            .iter()
            .any(|i| String::from_utf16_lossy(&i.base) == "勉強する"
                && i.derivations.contains(&WordForm::Causative)));
    }

    #[test]
    fn formats_derivation_chain_and_grammar_descriptions() {
        let u16_in: Vec<u16> = "食べさせられた".encode_utf16().collect();
        let info = deconjugate(&u16_in, false)
            .into_iter()
            .find(|i| String::from_utf16_lossy(&i.base) == "食べる")
            .expect("should deconjugate 食べさせられた to 食べる");
        let (chain, descs) = format_derivation_chain(&info.derivations);
        assert_eq!(chain, "past ← potential or passive ← causative ← ichidan verb");
        assert_eq!(descs.len(), 4);
        assert_eq!(descs[0].0, "past");
        assert!(!descs[0].1.is_empty());
        assert_eq!(descs[3].0, "ichidan verb");
        assert!(!descs[3].1.is_empty());
    }
}

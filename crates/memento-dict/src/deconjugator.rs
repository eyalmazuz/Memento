// Copyright (c) 2024 Spacehamster
// SPDX-License-Identifier: GPL-2.0-or-later

//! Rule-based Japanese deconjugation, ported from `src/dict/deconjugator.cpp`.
//! Words and results are UTF-16 code units to preserve `QString` values, including
//! unpaired surrogates produced when sentence mode removes one code unit at a time.

use std::sync::OnceLock;

/// `WordForm` values and order match the C++ enum (including its unused members).
#[repr(u32)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
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

use self::WordForm as W;

struct Rule {
    base: &'static str,
    conjugated: &'static str,
    base_type: WordForm,
    conjugated_type: WordForm,
}

impl Rule {
    const fn new(
        base: &'static str,
        conjugated: &'static str,
        base_type: WordForm,
        conjugated_type: WordForm,
    ) -> Self {
        Self { base, conjugated, base_type, conjugated_type }
    }
}

// These rules keep the C++ table order: the recursion is depth-first, without
// deduplication, and the rule order determines result order.
const SILENT_RULES: &[Rule] = &[
    Rule::new("ない", "ない", W::Negative, W::Adjective),
    Rule::new("たい", "たい", W::Tai, W::Adjective),
    Rule::new("せる", "せる", W::Causative, W::IchidanVerb),
    Rule::new("れる", "れる", W::Passive, W::IchidanVerb),
    Rule::new("る", "る", W::Potential, W::IchidanVerb),
    Rule::new("られる", "られる", W::PotentialPassive, W::IchidanVerb),
    Rule::new("しまう", "しまう", W::Shimau, W::GodanVerb),
    Rule::new("ゃう", "ゃう", W::Chau, W::GodanVerb),
    Rule::new("まう", "まう", W::Chimau, W::GodanVerb),
    Rule::new("る", "る", W::Continuous, W::IchidanVerb),
    Rule::new("おる", "おる", W::Continuous, W::GodanVerb),
    Rule::new("すぎる", "すぎる", W::Sugiru, W::IchidanVerb),
    Rule::new("とく", "とく", W::Toku, W::GodanVerb),
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
    Rule::new("問う", "問うとく", W::GodanVerb, W::Toku),
    Rule::new("請う", "請うとく", W::GodanVerb, W::Toku),
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
    //Causative
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
    //Zaru
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
    Rule::new("問う", "問うたり", W::GodanVerb, W::Tari),
    Rule::new("請う", "請うたり", W::GodanVerb, W::Tari),
    //Chau
    Rule::new("る", "っちゃう", W::GodanVerb, W::Chau),
    Rule::new("う", "っちゃう", W::GodanVerb, W::Chau),
    Rule::new("つ", "っちゃう", W::GodanVerb, W::Chau),
    Rule::new("す", "しちゃう", W::GodanVerb, W::Chau),
    Rule::new("く", "いちゃう", W::GodanVerb, W::Chau),
    Rule::new("ぐ", "いちゃう", W::GodanVerb, W::Chau),
    Rule::new("ぶ", "んじゃう", W::GodanVerb, W::Chau),
    Rule::new("ぬ", "んじゃう", W::GodanVerb, W::Chau),
    Rule::new("む", "んじゃう", W::GodanVerb, W::Chau),
    Rule::new("る", "ちゃう", W::IchidanVerb, W::Chau),
    Rule::new("くる", "きちゃう", W::KuruVerb, W::Chau),
    Rule::new("来る", "来ちゃう", W::KuruVerb, W::Chau),
    Rule::new("する", "しちゃう", W::SuruVerb, W::Chau),
    Rule::new("為る", "為ちゃう", W::SuruVerb, W::Chau),
    Rule::new("行く", "行っちゃう", W::GodanVerb, W::Chau),
    Rule::new("問う", "問うちゃう", W::GodanVerb, W::Chau),
    Rule::new("請う", "請うちゃう", W::GodanVerb, W::Chau),
    //Chimau
    Rule::new("る", "っちまう", W::GodanVerb, W::Chimau),
    Rule::new("う", "っちまう", W::GodanVerb, W::Chimau),
    Rule::new("つ", "っちまう", W::GodanVerb, W::Chimau),
    Rule::new("す", "しちまう", W::GodanVerb, W::Chimau),
    Rule::new("く", "いちまう", W::GodanVerb, W::Chimau),
    Rule::new("ぐ", "いちまう", W::GodanVerb, W::Chimau),
    Rule::new("ぶ", "んじまう", W::GodanVerb, W::Chimau),
    Rule::new("ぬ", "んじまう", W::GodanVerb, W::Chimau),
    Rule::new("む", "んじまう", W::GodanVerb, W::Chimau),
    Rule::new("る", "ちまう", W::IchidanVerb, W::Chimau),
    Rule::new("くる", "きちまう", W::KuruVerb, W::Chimau),
    Rule::new("来る", "来ちまう", W::KuruVerb, W::Chimau),
    Rule::new("する", "しちまう", W::SuruVerb, W::Chimau),
    Rule::new("為る", "為ちまう", W::SuruVerb, W::Chimau),
    Rule::new("行く", "行っちまう", W::GodanVerb, W::Chimau),
    Rule::new("問う", "問うちゃう", W::GodanVerb, W::Chimau),
    Rule::new("請う", "請うちゃう", W::GodanVerb, W::Chimau),
    //Continuous
    Rule::new("で", "でいる", W::Te, W::Continuous),
    Rule::new("て", "ている", W::Te, W::Continuous),
    Rule::new("で", "でおる", W::Te, W::Continuous),
    Rule::new("て", "ておる", W::Te, W::Continuous),
    Rule::new("で", "でる", W::Te, W::Continuous),
    Rule::new("て", "てる", W::Te, W::Continuous),
    Rule::new("て", "とる", W::Te, W::Continuous),
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
}

impl From<&Rule> for CompiledRule {
    fn from(rule: &Rule) -> Self {
        Self {
            base: rule.base.encode_utf16().collect(),
            conjugated: rule.conjugated.encode_utf16().collect(),
            base_type: rule.base_type,
            conjugated_type: rule.conjugated_type,
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

    let mut child_derivations = Vec::with_capacity(derivations.len() + 1 + usize::from(derivations.is_empty()));
    child_derivations.push(base_type);
    if derivations.is_empty() {
        child_derivations.push(conjugated_type);
    }
    child_derivations.extend_from_slice(derivations);
    (child_word, child_derivations)
}

fn deconjugate_recursive(
    word: &[u16],
    derivations: &[WordForm],
    conjugated: &[u16],
    results: &mut Vec<ConjugationInfo>,
    rules: &[CompiledRule],
    silent_rules: &[CompiledRule],
) {
    let current_type = derivations.first().copied().unwrap_or(WordForm::Any);
    for rule in rules {
        if current_type != WordForm::Any && current_type != rule.conjugated_type {
            continue;
        }
        if !word.ends_with(&rule.conjugated) {
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
            // The result's index remains valid even if descendants reallocate
            // the results vector or insert identical-looking candidates.
            for silent_rule in silent_rules {
                if silent_rule.conjugated_type == rule.base_type
                    && results[result_index].base.ends_with(&silent_rule.base)
                {
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
                        results,
                        rules,
                        silent_rules,
                    );
                }
            }
        } else {
            deconjugate_recursive(&base, &chain, conjugated, results, rules, silent_rules);
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
    silent_rules: &[CompiledRule],
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
        deconjugate_recursive(&word, &[], conjugated, results, rules, silent_rules);
    } else {
        deconjugate_recursive(conjugated, &[], conjugated, results, rules, silent_rules);
    }
}

/// Attempts all rule chains for `query`, retaining the C++ rule order and duplicates.
/// Sentence mode tries progressively shorter prefixes (in UTF-16 code units),
/// skipping whitespace at each trimmed suffix; otherwise only the full query
/// is tried. The original, unstripped prefix is stored as `conjugated`.
pub fn deconjugate(query: &[u16], sentence_mode: bool) -> Vec<ConjugationInfo> {
    static COMPILED_RULES: OnceLock<Vec<CompiledRule>> = OnceLock::new();
    static COMPILED_SILENT_RULES: OnceLock<Vec<CompiledRule>> = OnceLock::new();
    let rules = compiled_rules(RULES, &COMPILED_RULES);
    let silent_rules = compiled_rules(SILENT_RULES, &COMPILED_SILENT_RULES);
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

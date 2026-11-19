// Copyright (c) 2024 Spacehamster
// SPDX-License-Identifier: GPL-2.0-or-later

//! Rule-based Japanese deconjugation, ported from `src/dict/deconjugator.cpp`
//! with Yomitan rule parity, short-causative sub-conditions, indexed rule lookup,
//! recursion depth bounds, and grammar descriptions.

mod rules;

use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use self::rules::{Rule, SilentRule, RULES, SILENT_RULES, SUB_CAUS_LONG, SUB_NONE};

const MAX_DEPTH: usize = 12;

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

/// An ordered UTF-8 deconjugation candidate; alternative derivations are retained while exact
/// duplicates for a prefix are deduplicated before formatting.
/// `derivations` run from the candidate's base type toward its conjugated form.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConjugationInfo {
    pub base: String,
    pub derivations: Vec<WordForm>,
    /// Display text assembled from the English translation source strings.
    pub derivation_display: String,
}

/// The untranslated source text used by C++ `Deconjugator::tr`.
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

    let mut descriptions = Vec::with_capacity(steps.len());
    let mut chain = String::new();
    for (idx, form) in steps.into_iter().enumerate() {
        let label = word_form_label(form);
        if idx > 0 {
            chain.push_str(" ← ");
        }
        chain.push_str(label);
        descriptions.push((label.to_owned(), word_form_description(form).to_owned()));
    }
    (chain, descriptions)
}

/// Zero-copy UTF-8 rule index pointing directly into static `RULES` / `SILENT_RULES`.
struct RuleIndex {
    rules: &'static [Rule],
    /// Maps `(conjugated_type, last_char_of_conjugated_suffix)` -> sorted rule indices in `RULES` order.
    /// Entries under `WordForm::Any` hold all rules matching that suffix character (or `None` for empty suffix).
    by_form_and_last: HashMap<(WordForm, Option<char>), Vec<usize>>,
    silent_by_form: HashMap<WordForm, Vec<&'static SilentRule>>,
}

impl RuleIndex {
    fn build(raw_rules: &'static [Rule], raw_silent: &'static [SilentRule]) -> Self {
        let mut by_form_and_last: HashMap<(WordForm, Option<char>), Vec<usize>> = HashMap::new();

        for (idx, raw) in raw_rules.iter().enumerate() {
            let last_char = raw.conjugated.chars().next_back();
            by_form_and_last
                .entry((raw.conjugated_type, last_char))
                .or_default()
                .push(idx);
            by_form_and_last
                .entry((WordForm::Any, last_char))
                .or_default()
                .push(idx);
        }

        let mut silent_by_form: HashMap<WordForm, Vec<&'static SilentRule>> = HashMap::new();
        for raw in raw_silent {
            silent_by_form
                .entry(raw.conjugated_type)
                .or_default()
                .push(raw);
        }

        Self {
            rules: raw_rules,
            by_form_and_last,
            silent_by_form,
        }
    }

    fn for_each_matching_rule(
        &self,
        current_type: WordForm,
        last_char: Option<char>,
        allow_empty_suffix: bool,
        mut f: impl FnMut(&'static Rule),
    ) {
        let exact = last_char
            .and_then(|ch| self.by_form_and_last.get(&(current_type, Some(ch))))
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let empty = if allow_empty_suffix {
            self.by_form_and_last
                .get(&(current_type, None))
                .map(Vec::as_slice)
                .unwrap_or(&[])
        } else {
            &[]
        };

        // Two-pointer merge preserves original `RULES` order when both slices are non-empty.
        let (mut i, mut j) = (0usize, 0usize);
        while i < exact.len() && j < empty.len() {
            if exact[i] <= empty[j] {
                f(&self.rules[exact[i]]);
                i += 1;
            } else {
                f(&self.rules[empty[j]]);
                j += 1;
            }
        }
        for &idx in &exact[i..] {
            f(&self.rules[idx]);
        }
        for &idx in &empty[j..] {
            f(&self.rules[idx]);
        }
    }
}

fn rule_index() -> &'static RuleIndex {
    static INDEX: OnceLock<RuleIndex> = OnceLock::new();
    INDEX.get_or_init(|| RuleIndex::build(RULES, SILENT_RULES))
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

/// Guards the empty-suffix rule (`る <- ""`) so it only fires on kana or kanji stems,
/// never on ASCII, digits, whitespace, or Japanese punctuation/symbols.
fn is_valid_empty_suffix_stem_end(ch: char) -> bool {
    matches!(
        ch,
        '\u{3041}'..='\u{3096}'
            | '\u{30A1}'..='\u{30FA}'
            | '\u{3400}'..='\u{4DBF}'
            | '\u{4E00}'..='\u{9FFF}'
            | '\u{F900}'..='\u{FAFF}'
            | '\u{20000}'..='\u{323AF}'
    )
}

fn format_derivation(derivations: &[WordForm]) -> String {
    let mut display = String::new();
    let mut first = true;
    for form in derivations.iter().copied() {
        if form == WordForm::Conjunctive || is_terminal(form) {
            continue;
        }
        if !first {
            display.push_str(" « ");
        }
        display.push_str(word_form_label(form));
        first = false;
    }
    // A bare masu stem is shown only when it is the final conjugated form.
    if derivations.last() == Some(&WordForm::Conjunctive) {
        if !first {
            display.push_str(" « ");
        }
        display.push_str(word_form_label(WordForm::Conjunctive));
    }
    display
}

fn create_derivation(
    word: &str,
    derivations: &[WordForm],
    rule_base: &str,
    rule_conjugated: &str,
    base_type: WordForm,
    conjugated_type: WordForm,
) -> (String, Vec<WordForm>) {
    let prefix_length = word.len() - rule_conjugated.len();
    let mut child_word = String::with_capacity(prefix_length + rule_base.len());
    child_word.push_str(&word[..prefix_length]);
    child_word.push_str(rule_base);

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
    word: &str,
    derivations: &[WordForm],
    caus_sub: u8,
    depth: usize,
    results: &mut Vec<ConjugationInfo>,
    seen: &mut HashSet<(String, Vec<WordForm>)>,
    index: &RuleIndex,
) {
    if depth >= MAX_DEPTH || word.is_empty() {
        return;
    }
    let current_type = derivations.first().copied().unwrap_or(WordForm::Any);
    let last_char = word.chars().next_back();
    let allow_empty_suffix = last_char.is_some_and(is_valid_empty_suffix_stem_end);

    index.for_each_matching_rule(current_type, last_char, allow_empty_suffix, |rule| {
        if rule.conjugated_type == WordForm::Causative {
            let active = if current_type == WordForm::Any {
                SUB_CAUS_LONG
            } else {
                caus_sub
            };
            if (active & rule.caus_req) == 0 {
                return;
            }
        }
        if !word.ends_with(rule.conjugated) {
            return;
        }
        let prefix_length = word.len() - rule.conjugated.len();
        // A suffix-only terminal rule (<= 1 char, e.g. `る`/`う`/`い`) requires a non-empty stem.
        if prefix_length == 0
            && rule.base.chars().nth(1).is_none()
            && is_terminal(rule.base_type)
        {
            return;
        }
        let (base, chain) = create_derivation(
            word,
            derivations,
            rule.base,
            rule.conjugated,
            rule.base_type,
            rule.conjugated_type,
        );
        if is_terminal(rule.base_type) {
            if !seen.insert((base.clone(), chain.clone())) {
                return;
            }
            let result_index = results.len();
            results.push(ConjugationInfo {
                derivation_display: format_derivation(&chain),
                base,
                derivations: chain,
            });
            if depth + 1 >= MAX_DEPTH {
                return;
            }
            // Emit the terminal result first, then re-enter the original parent
            // with the silent rule's base type (not the terminal child).
            if let Some(silent_rules) = index.silent_by_form.get(&rule.base_type) {
                for &silent_rule in silent_rules {
                    if rule.base_type == WordForm::GodanVerb
                        && (rule.v5_out & silent_rule.required_v5_sub) == 0
                    {
                        continue;
                    }
                    if results[result_index].base.ends_with(silent_rule.base) {
                        let (derived_base, derived_chain) = create_derivation(
                            word,
                            derivations,
                            rule.base,
                            rule.conjugated,
                            silent_rule.base_type,
                            rule.conjugated_type,
                        );
                        deconjugate_recursive(
                            &derived_base,
                            &derived_chain,
                            silent_rule.caus_sub,
                            depth + 1,
                            results,
                            seen,
                            index,
                        );
                    }
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
                next_caus,
                depth + 1,
                results,
                seen,
                index,
            );
        }
    });
}

// ASCII PCRE2 shorthand whitespace (`\s` without UCP).
#[inline]
fn is_regex_whitespace_char(ch: char) -> bool {
    matches!(ch, '\t'..='\r' | ' ')
}

fn deconjugate_prefix_str(
    conjugated: &str,
    results: &mut Vec<ConjugationInfo>,
    seen: &mut HashSet<(String, Vec<WordForm>)>,
    index: &RuleIndex,
) {
    seen.clear();
    if conjugated.chars().any(is_regex_whitespace_char) {
        let word: String = conjugated
            .chars()
            .filter(|&c| !is_regex_whitespace_char(c))
            .collect();
        deconjugate_recursive(&word, &[], SUB_CAUS_LONG, 0, results, seen, index);
    } else {
        deconjugate_recursive(conjugated, &[], SUB_CAUS_LONG, 0, results, seen, index);
    }
}

/// Attempts all rule chains for UTF-8 `query`, preserving rule order while deduplicating
/// identical `(base, derivations)` hits per prefix before formatting derivations.
pub fn deconjugate_str(query: &str, sentence_mode: bool) -> Vec<ConjugationInfo> {
    let index = rule_index();
    let mut results = Vec::new();
    let mut seen: HashSet<(String, Vec<WordForm>)> = HashSet::new();

    if sentence_mode {
        let mut conjugated = query.to_owned();
        while !conjugated.is_empty() {
            deconjugate_prefix_str(&conjugated, &mut results, &mut seen, index);
            conjugated.pop();
            while conjugated.chars().next_back().is_some_and(is_regex_whitespace_char) {
                conjugated.pop();
            }
        }
    } else {
        deconjugate_prefix_str(query, &mut results, &mut seen, index);
    }
    results
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bases_for(input: &str) -> Vec<(String, WordForm)> {
        deconjugate_str(input, false)
            .into_iter()
            .map(|info| (info.base, info.derivations[0]))
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
        assert!(bases_for("食べさせられた").contains(&("食べる".into(), WordForm::IchidanVerb)));
        assert!(bases_for("書かされた").contains(&("書く".into(), WordForm::GodanVerb)));
        assert!(bases_for("読まされた").contains(&("読む".into(), WordForm::GodanVerb)));
        assert!(bases_for("食べさした").contains(&("食べる".into(), WordForm::IchidanVerb)));

        assert!(!bases_for("食べされる").iter().any(|(b, _)| b == "食べる"));
        assert!(!bases_for("食べされた").iter().any(|(b, _)| b == "食べる"));
        assert!(!bases_for("話さされる").iter().any(|(b, _)| b == "話す"));
        assert!(!bases_for("来される").iter().any(|(b, _)| b == "来る"));

        let benkyou_sareru = deconjugate_str("勉強される", false);
        assert!(benkyou_sareru
            .iter()
            .any(|i| i.base == "勉強する" && !i.derivations.contains(&WordForm::Causative)));
        assert!(!benkyou_sareru
            .iter()
            .any(|i| i.base == "勉強する" && i.derivations.contains(&WordForm::Causative)));
    }

    #[test]
    fn empty_suffix_rule_rejects_punctuation_and_ascii_stems() {
        assert!(bases_for("。").is_empty());
        assert!(bases_for("hello").is_empty());
        assert!(bases_for("123").is_empty());
        assert!(bases_for("食べ").contains(&("食べる".into(), WordForm::IchidanVerb)));
    }

    #[test]
    fn formats_derivation_chain_and_grammar_descriptions() {
        let info = deconjugate_str("食べさせられた", false)
            .into_iter()
            .find(|i| i.base == "食べる")
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

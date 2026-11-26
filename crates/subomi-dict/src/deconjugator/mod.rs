// Copyright (c) 2024 Spacehamster
// SPDX-License-Identifier: GPL-2.0-or-later

//! Rule-based Japanese deconjugation, ported from `src/dict/deconjugator.cpp`
//! with complete Yomitan Japanese transform parity, short-causative sub-conditions,
//! indexed rule lookup, recursion depth and candidate bounds (#2153), and grammar descriptions.

mod rules;

use std::ops::Range;
use std::sync::OnceLock;

use self::rules::{Rule, SilentRule, RULES, SILENT_RULES, SUB_CAUS_LONG, SUB_NONE};

const MAX_DEPTH: usize = 12;
const MAX_DERIVATION_LEN: usize = MAX_DEPTH + 2;
const WORD_FORM_COUNT: usize = 61;
const HIRAGANA_TABLE_START: u32 = 0x3040;
const HIRAGANA_TABLE_LEN: usize = 96;
/// Maximum deconjugation candidates emitted for a single prefix (Yomitan #2153 safeguard).
const MAX_RESULTS_PER_PREFIX: usize = 64;
/// Maximum total deconjugation candidates emitted across all prefixes in sentence mode (#2153).
const MAX_TOTAL_RESULTS: usize = 256;

/// `WordForm` values and order match the C++ enum (0..=42) plus additive Yomitan transform
/// variants (43+).
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
    ZuruVerb = 43,
    Ya = 44,
    Cha = 45,
    NBakari = 46,
    NTosuru = 47,
    Mu = 48,
    Mai = 49,
    Fu = 50,
    Ge = 51,
    Garu = 52,
    Yagaru = 53,
    Yashinai = 54,
    TeOku = 55,
    VolitionalSlang = 56,
    NSlang = 57,
    ImperativeNegativeSlang = 58,
    KansaiBenNegative = 59,
    KansaiBenAdverbial = 60,
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

/// The untranslated English source label for a conjugation step.
pub fn word_form_label(form: WordForm) -> &'static str {
    match form {
        WordForm::GodanVerb => "godan verb",
        WordForm::IchidanVerb => "ichidan verb",
        WordForm::SuruVerb => "suru verb",
        WordForm::KuruVerb => "kuru verb",
        WordForm::ZuruVerb => "zuru verb",
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
        WordForm::Ya => "-ya",
        WordForm::Cha => "-cha",
        WordForm::NBakari => "-n bakari",
        WordForm::NTosuru => "-n to suru",
        WordForm::Mu => "-mu",
        WordForm::Mai => "-mai",
        WordForm::Fu => "-fu",
        WordForm::Ge => "-ge",
        WordForm::Garu => "-garu",
        WordForm::Yagaru => "-yagaru",
        WordForm::Yashinai => "-yashinai",
        WordForm::TeOku => "-te oku",
        WordForm::VolitionalSlang => "volitional slang",
        WordForm::NSlang => "-n slang",
        WordForm::ImperativeNegativeSlang => "imperative negative slang",
        WordForm::KansaiBenNegative => "Kansai-ben negative",
        WordForm::KansaiBenAdverbial => "Kansai-ben -ku",
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
        WordForm::ZuruVerb => "Alternative literary -zuru verb (vz)",
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
        WordForm::E => "Colloquial vowel contraction (-ai/-oi to -ee, -ui to -ii)",
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
        WordForm::Ya => "Colloquial contraction of -ba conditional (-kerya / -kya / -rya)",
        WordForm::Cha => "Colloquial contraction of -te wa / -de wa (-cha / -ja)",
        WordForm::NBakari => "Literary 'as if about to / on the verge of' (-n bakari)",
        WordForm::NTosuru => "Literary 'about to / trying to' (-n to suru)",
        WordForm::Mu => "Archaic/literary volitional form (-mu)",
        WordForm::Mai => "Negative volitional / conjecture ('will probably not', -mai)",
        WordForm::Fu => "Historical kana -fu verb ending (modern -u)",
        WordForm::Ge => "Indicates outward appearance or feeling (-ge)",
        WordForm::Garu => "Shows outward signs of feeling or wanting (-garu)",
        WordForm::Yagaru => "Derogatory/contemptuous auxiliary verb (-yagaru)",
        WordForm::Yashinai => "Emphatic negative ('not at all', -yashinai / -yashimasen)",
        WordForm::TeOku => "Do in advance or leave in a resultant state (-te oku / -de oku)",
        WordForm::VolitionalSlang => "Casual volitional contraction (-okka / -yokka / -kokka)",
        WordForm::NSlang => "Colloquial /r/ -> /n/ assimilation (-nnai / -rannai / -nnasai)",
        WordForm::ImperativeNegativeSlang => {
            "Colloquial negative command (-nna, contraction of -runa)"
        }
        WordForm::KansaiBenNegative => "Kansai-ben negative form (-hen / -hin / seehen)",
        WordForm::KansaiBenAdverbial => {
            "Kansai-ben u-onbin i-adjective adverbial form (-ou / -yuu / -uu)"
        }
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

/// Compact, `Copy` derivation chain (base type first, surface form last) avoiding
/// per-candidate heap `Vec<WordForm>` allocations during deconjugation.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct DerivationChain {
    forms: [WordForm; MAX_DERIVATION_LEN],
    len: u8,
}

impl Default for DerivationChain {
    fn default() -> Self {
        Self::EMPTY
    }
}

impl DerivationChain {
    pub const EMPTY: Self = Self {
        forms: [WordForm::None; MAX_DERIVATION_LEN],
        len: 0,
    };

    #[inline]
    pub fn as_slice(&self) -> &[WordForm] {
        &self.forms[..usize::from(self.len)]
    }

    #[inline]
    pub fn first(&self) -> Option<WordForm> {
        self.as_slice().first().copied()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn to_vec(&self) -> Vec<WordForm> {
        self.as_slice().to_vec()
    }

    fn from_reversed_stack(stack: &[WordForm]) -> Self {
        let mut out = Self::EMPTY;
        let n = stack.len().min(MAX_DERIVATION_LEN);
        for i in 0..n {
            out.forms[i] = stack[n - 1 - i];
        }
        out.len = n as u8;
        out
    }
}

/// Lightweight deconjugation hit carrying the base form and inline `Copy` derivation chain
/// without allocating formatted display strings or grammar hover descriptions until needed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RawConjugation {
    pub base: String,
    pub derivations: DerivationChain,
}

/// Returns `true` when two derivation chains produce the same `format_derivation` display
/// string, without allocating any intermediate `String`.
#[must_use]
pub fn derivation_displays_eq(a: &[WordForm], b: &[WordForm]) -> bool {
    fn visible_labels(derivations: &[WordForm]) -> impl Iterator<Item = &'static str> + '_ {
        let trailing_conj =
            (derivations.last() == Some(&WordForm::Conjunctive)).then_some(WordForm::Conjunctive);
        derivations
            .iter()
            .copied()
            .filter(|&form| form != WordForm::Conjunctive && !is_terminal(form))
            .chain(trailing_conj)
            .map(word_form_label)
    }
    visible_labels(a).eq(visible_labels(b))
}

#[derive(Clone)]
struct FormBucket {
    empty: Range<u32>,
    hiragana: [Range<u32>; HIRAGANA_TABLE_LEN],
    other: Vec<(char, Range<u32>)>,
}

impl Default for FormBucket {
    fn default() -> Self {
        Self {
            empty: 0..0,
            hiragana: std::array::from_fn(|_| 0..0),
            other: Vec::new(),
        }
    }
}

/// Flat array-indexed UTF-8 rule table pointing directly into static `RULES` / `SILENT_RULES`
/// with O(1) `WordForm` and Hiragana suffix lookup (zero hashing during DFS).
struct RuleIndex {
    rules: &'static [Rule],
    pool: Vec<u16>,
    by_form: Vec<FormBucket>,
    silent_by_form: Vec<Vec<&'static SilentRule>>,
}

impl RuleIndex {
    fn build(raw_rules: &'static [Rule], raw_silent: &'static [SilentRule]) -> Self {
        let mut temp: Vec<(Vec<u16>, Vec<Vec<u16>>, Vec<(char, Vec<u16>)>)> = (0..WORD_FORM_COUNT)
            .map(|_| (Vec::new(), vec![Vec::new(); HIRAGANA_TABLE_LEN], Vec::new()))
            .collect();

        let mut push_rule = |form: WordForm, last_char: Option<char>, idx: u16| {
            let slot = &mut temp[form as usize];
            match last_char {
                None => slot.0.push(idx),
                Some(ch) => {
                    let cp = ch as u32;
                    if cp >= HIRAGANA_TABLE_START
                        && (cp - HIRAGANA_TABLE_START) < HIRAGANA_TABLE_LEN as u32
                    {
                        slot.1[(cp - HIRAGANA_TABLE_START) as usize].push(idx);
                    } else if let Some((_, list)) = slot.2.iter_mut().find(|(c, _)| *c == ch) {
                        list.push(idx);
                    } else {
                        slot.2.push((ch, vec![idx]));
                    }
                }
            }
        };

        for (idx, raw) in raw_rules.iter().enumerate() {
            let idx_u16 = idx as u16;
            let last_char = raw.conjugated.chars().next_back();
            push_rule(raw.conjugated_type, last_char, idx_u16);
            push_rule(WordForm::Any, last_char, idx_u16);
        }

        let mut pool = Vec::with_capacity(raw_rules.len() * 2);
        let mut by_form = Vec::with_capacity(WORD_FORM_COUNT);
        for (empty_vec, hira_vecs, mut other_vecs) in temp {
            let mut bucket = FormBucket::default();
            if !empty_vec.is_empty() {
                let start = pool.len() as u32;
                pool.extend_from_slice(&empty_vec);
                bucket.empty = start..pool.len() as u32;
            }
            for (h_idx, vec) in hira_vecs.into_iter().enumerate() {
                if !vec.is_empty() {
                    let start = pool.len() as u32;
                    pool.extend_from_slice(&vec);
                    bucket.hiragana[h_idx] = start..pool.len() as u32;
                }
            }
            other_vecs.sort_by_key(|(c, _)| *c);
            for (ch, vec) in other_vecs {
                let start = pool.len() as u32;
                pool.extend_from_slice(&vec);
                bucket.other.push((ch, start..pool.len() as u32));
            }
            by_form.push(bucket);
        }

        let mut silent_by_form: Vec<Vec<&'static SilentRule>> = vec![Vec::new(); WORD_FORM_COUNT];
        for raw in raw_silent {
            silent_by_form[raw.conjugated_type as usize].push(raw);
        }

        Self {
            rules: raw_rules,
            pool,
            by_form,
            silent_by_form,
        }
    }

    #[inline]
    fn slice_range(&self, r: &Range<u32>) -> &[u16] {
        &self.pool[r.start as usize..r.end as usize]
    }

    #[inline]
    fn for_each_matching_rule(
        &self,
        current_type: WordForm,
        last_char: Option<char>,
        allow_empty_suffix: bool,
        mut f: impl FnMut(&'static Rule),
    ) {
        let bucket = &self.by_form[current_type as usize];
        let exact: &[u16] = match last_char {
            Some(ch) => {
                let cp = ch as u32;
                if cp >= HIRAGANA_TABLE_START
                    && (cp - HIRAGANA_TABLE_START) < HIRAGANA_TABLE_LEN as u32
                {
                    self.slice_range(&bucket.hiragana[(cp - HIRAGANA_TABLE_START) as usize])
                } else if let Ok(pos) = bucket.other.binary_search_by_key(&ch, |(c, _)| *c) {
                    self.slice_range(&bucket.other[pos].1)
                } else {
                    &[]
                }
            }
            None => &[],
        };
        let empty: &[u16] = if allow_empty_suffix {
            self.slice_range(&bucket.empty)
        } else {
            &[]
        };

        // Two-pointer merge preserves original `RULES` order when both slices are non-empty.
        let (mut i, mut j) = (0usize, 0usize);
        while i < exact.len() && j < empty.len() {
            if exact[i] <= empty[j] {
                f(&self.rules[usize::from(exact[i])]);
                i += 1;
            } else {
                f(&self.rules[usize::from(empty[j])]);
                j += 1;
            }
        }
        for &idx in &exact[i..] {
            f(&self.rules[usize::from(idx)]);
        }
        for &idx in &empty[j..] {
            f(&self.rules[usize::from(idx)]);
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
            | WordForm::ZuruVerb
            | WordForm::Adjective
    )
}

/// Guards the empty-suffix rule (`る <- ""`) so it only fires on kana or CJK ideograph stems
/// (including CJK Extensions H & J), never on ASCII, digits, whitespace, or Japanese punctuation.
fn is_valid_empty_suffix_stem_end(ch: char) -> bool {
    matches!(ch, '\u{3041}'..='\u{3096}' | '\u{30A1}'..='\u{30FA}')
        || crate::normalize::is_cjk_ideograph(ch)
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

#[allow(clippy::too_many_arguments)]
fn deconjugate_recursive(
    word_buf: &mut String,
    rev_stack: &mut [WordForm; MAX_DERIVATION_LEN],
    stack_len: usize,
    caus_sub: u8,
    depth: usize,
    prefix_start: usize,
    max_results: usize,
    results: &mut Vec<RawConjugation>,
    index: &RuleIndex,
) {
    if depth >= MAX_DEPTH || word_buf.is_empty() || results.len() >= max_results {
        return;
    }
    let current_type = if stack_len == 0 {
        WordForm::Any
    } else {
        rev_stack[stack_len - 1]
    };
    let last_char = word_buf.chars().next_back();
    let allow_empty_suffix = last_char.is_some_and(is_valid_empty_suffix_stem_end);

    index.for_each_matching_rule(current_type, last_char, allow_empty_suffix, |rule| {
        if results.len() >= max_results {
            return;
        }
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
        if !word_buf.ends_with(rule.conjugated) {
            return;
        }
        let prefix_length = word_buf.len() - rule.conjugated.len();
        // A suffix-only terminal rule (<= 1 char, e.g. `る`/`う`/`い`) requires a non-empty stem.
        if prefix_length == 0
            && rule.base.chars().nth(1).is_none()
            && is_terminal(rule.base_type)
        {
            return;
        }

        // Mutate `word_buf` and `rev_stack` in place (zero heap allocations across DFS).
        word_buf.truncate(prefix_length);
        word_buf.push_str(rule.base);

        let next_len = if stack_len == 0 {
            rev_stack[0] = rule.conjugated_type;
            rev_stack[1] = rule.base_type;
            2
        } else {
            rev_stack[stack_len] = rule.base_type;
            stack_len + 1
        };

        if is_terminal(rule.base_type) {
            let chain = DerivationChain::from_reversed_stack(&rev_stack[..next_len]);
            let already_seen = results[prefix_start..]
                .iter()
                .any(|existing| existing.derivations == chain && existing.base == word_buf.as_str());
            if !already_seen {
                results.push(RawConjugation {
                    base: word_buf.clone(),
                    derivations: chain,
                });
                if depth + 1 < MAX_DEPTH && results.len() < max_results {
                    // Emit the terminal result first, then re-enter with each matching silent
                    // rule's base type (replacing only the top terminal form on the stack).
                    let silent_rules = &index.silent_by_form[rule.base_type as usize];
                    for &silent_rule in silent_rules {
                        if results.len() >= max_results {
                            break;
                        }
                        if rule.base_type == WordForm::GodanVerb
                            && (rule.v5_out & silent_rule.required_v5_sub) == 0
                        {
                            continue;
                        }
                        if word_buf.ends_with(silent_rule.base) {
                            rev_stack[next_len - 1] = silent_rule.base_type;
                            deconjugate_recursive(
                                word_buf,
                                rev_stack,
                                next_len,
                                silent_rule.caus_sub,
                                depth + 1,
                                prefix_start,
                                max_results,
                                results,
                                index,
                            );
                        }
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
                word_buf,
                rev_stack,
                next_len,
                next_caus,
                depth + 1,
                prefix_start,
                max_results,
                results,
                index,
            );
        }

        // Restore `word_buf` to the parent's exact bytes before checking the next rule.
        word_buf.truncate(prefix_length);
        word_buf.push_str(rule.conjugated);
    });
}

// ASCII PCRE2 shorthand whitespace (`\s` without UCP).
#[inline]
fn is_regex_whitespace_char(ch: char) -> bool {
    matches!(ch, '\t'..='\r' | ' ')
}

fn deconjugate_prefix_raw(
    conjugated: &str,
    word_buf: &mut String,
    rev_stack: &mut [WordForm; MAX_DERIVATION_LEN],
    results: &mut Vec<RawConjugation>,
    index: &RuleIndex,
) {
    let trimmed = conjugated.trim_start();
    if trimmed.is_empty() || results.len() >= MAX_TOTAL_RESULTS {
        return;
    }
    let prefix_start = results.len();
    let max_results = (prefix_start + MAX_RESULTS_PER_PREFIX).min(MAX_TOTAL_RESULTS);
    word_buf.clear();
    if trimmed.chars().any(is_regex_whitespace_char) {
        word_buf.extend(trimmed.chars().filter(|&c| !is_regex_whitespace_char(c)));
    } else {
        word_buf.push_str(trimmed);
    }
    deconjugate_recursive(
        word_buf,
        rev_stack,
        0,
        SUB_CAUS_LONG,
        0,
        prefix_start,
        max_results,
        results,
        index,
    );
}

/// Attempts all rule chains for UTF-8 `query`, returning compact [`RawConjugation`] hits
/// without formatting display strings or grammar hover descriptions.
#[must_use]
pub fn deconjugate_str_raw(query: &str, sentence_mode: bool) -> Vec<RawConjugation> {
    let index = rule_index();
    let mut results = Vec::new();
    let mut word_buf = String::with_capacity(query.len() + 16);
    let mut rev_stack = [WordForm::None; MAX_DERIVATION_LEN];

    if sentence_mode {
        let mut conjugated = query.trim_start().to_owned();
        while !conjugated.is_empty() && results.len() < MAX_TOTAL_RESULTS {
            deconjugate_prefix_raw(&conjugated, &mut word_buf, &mut rev_stack, &mut results, index);
            conjugated.pop();
            while conjugated.chars().next_back().is_some_and(is_regex_whitespace_char) {
                conjugated.pop();
            }
        }
    } else {
        deconjugate_prefix_raw(query, &mut word_buf, &mut rev_stack, &mut results, index);
    }
    results
}

/// Attempts all rule chains for UTF-8 `query`, preserving rule order while deduplicating
/// identical `(base, derivations)` hits per prefix before formatting derivations.
pub fn deconjugate_str(query: &str, sentence_mode: bool) -> Vec<ConjugationInfo> {
    deconjugate_str_raw(query, sentence_mode)
        .into_iter()
        .map(|raw| ConjugationInfo {
            derivation_display: format_derivation(raw.derivations.as_slice()),
            base: raw.base,
            derivations: raw.derivations.to_vec(),
        })
        .collect()
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

    fn has_base(input: &str, expected_base: &str, expected_form: WordForm) -> bool {
        bases_for(input).contains(&(expected_base.to_owned(), expected_form))
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
        assert!(has_base("問うちまう", "問う", WordForm::GodanVerb));
        assert!(has_base("請うちまう", "請う", WordForm::GodanVerb));
        assert!(has_base("読んどる", "読む", WordForm::GodanVerb));
        assert!(has_base("読んどった", "読む", WordForm::GodanVerb));
        assert!(has_base("いっとく", "いく", WordForm::GodanVerb));
        assert!(has_base("いったり", "いく", WordForm::GodanVerb));
        assert!(has_base("いっちゃう", "いく", WordForm::GodanVerb));
        assert!(has_base("いっちまう", "いく", WordForm::GodanVerb));
    }

    #[test]
    fn critical_c4_godan_and_all_verb_tara_dara_deconjugation() {
        // Every Godan euphonic -たら/-だら class from C4 spec:
        assert!(has_base("行ったら", "行く", WordForm::GodanVerb));
        assert!(has_base("いったら", "いく", WordForm::GodanVerb));
        assert!(has_base("飲んだら", "飲む", WordForm::GodanVerb));
        assert!(has_base("書いたら", "書く", WordForm::GodanVerb));
        assert!(has_base("泳いだら", "泳ぐ", WordForm::GodanVerb));
        assert!(has_base("買ったら", "買う", WordForm::GodanVerb));
        assert!(has_base("待ったら", "待つ", WordForm::GodanVerb));
        assert!(has_base("取ったら", "取る", WordForm::GodanVerb));
        assert!(has_base("話したら", "話す", WordForm::GodanVerb));
        assert!(has_base("死んだら", "死ぬ", WordForm::GodanVerb));
        assert!(has_base("遊んだら", "遊ぶ", WordForm::GodanVerb));
        // Ichidan, Kuru, Suru, Zuru, Polite, Negative-tara:
        assert!(has_base("食べたら", "食べる", WordForm::IchidanVerb));
        assert!(has_base("来たら", "来る", WordForm::KuruVerb));
        assert!(has_base("きたら", "くる", WordForm::KuruVerb));
        assert!(has_base("したら", "する", WordForm::SuruVerb));
        assert!(has_base("論じたら", "論ずる", WordForm::ZuruVerb));
        assert!(has_base("食べましたら", "食べる", WordForm::IchidanVerb));
        assert!(has_base("食べなかったら", "食べる", WordForm::IchidanVerb));
        // Ungrammatical masu-stem `*書きたら` must NOT deconjugate to `書く`:
        assert!(!has_base("書きたら", "書く", WordForm::GodanVerb));
    }

    #[test]
    fn h6_4_ya_cha_nbakari_ntosuru_mu_mai_rules() {
        // -ゃ (-ば contraction)
        assert!(has_base("高けりゃ", "高い", WordForm::Adjective));
        assert!(has_base("高きゃ", "高い", WordForm::Adjective));
        assert!(has_base("食べなけりゃ", "食べる", WordForm::IchidanVerb));
        assert!(has_base("行きゃ", "行く", WordForm::GodanVerb));
        assert!(has_base("食べりゃ", "食べる", WordForm::IchidanVerb));
        assert!(has_base("くりゃ", "くる", WordForm::KuruVerb));
        assert!(has_base("すりゃ", "する", WordForm::SuruVerb));
        assert!(has_base("行かにゃ", "行く", WordForm::GodanVerb));
        // -ちゃ / -じゃ (-ては / -では contraction)
        assert!(has_base("食べちゃ", "食べる", WordForm::IchidanVerb));
        assert!(has_base("行っちゃ", "行く", WordForm::GodanVerb));
        assert!(has_base("飲んじゃ", "飲む", WordForm::GodanVerb));
        assert!(has_base("泳いじゃ", "泳ぐ", WordForm::GodanVerb));
        assert!(!has_base("泳いちゃ", "泳ぐ", WordForm::GodanVerb));
        assert!(has_base("食べなくちゃ", "食べる", WordForm::IchidanVerb));
        // -んばかり
        assert!(has_base("言わんばかり", "言う", WordForm::GodanVerb));
        assert!(has_base("溢れんばかり", "溢れる", WordForm::IchidanVerb));
        assert!(has_base("破裂せんばかり", "破裂する", WordForm::SuruVerb));
        // -んとする (including conjugated `んとした` / `んとしている`)
        assert!(has_base("言わんとする", "言う", WordForm::GodanVerb));
        assert!(has_base("沈まんとした", "沈む", WordForm::GodanVerb));
        assert!(has_base("消えんとしている", "消える", WordForm::IchidanVerb));
        // -む (archaic volitional)
        assert!(has_base("行かむ", "行く", WordForm::GodanVerb));
        assert!(has_base("食べむ", "食べる", WordForm::IchidanVerb));
        assert!(has_base("せむ", "する", WordForm::SuruVerb));
        // -まい (negative volitional)
        assert!(has_base("行くまい", "行く", WordForm::GodanVerb));
        assert!(has_base("食べるまい", "食べる", WordForm::IchidanVerb));
        assert!(has_base("食べまい", "食べる", WordForm::IchidanVerb));
        assert!(has_base("するまい", "する", WordForm::SuruVerb));
        assert!(has_base("すまい", "する", WordForm::SuruVerb));
        assert!(has_base("行きますまい", "行く", WordForm::GodanVerb));
    }

    #[test]
    fn h6_4_historical_kana_ge_garu_yagaru_yashinai_sugiru_teoku() {
        // -ふ & Historical Kana Orthography (Yomitan PR #2474 / #2501)
        assert!(has_base("思ふ", "思う", WordForm::GodanVerb));
        assert!(has_base("言ふ", "言う", WordForm::GodanVerb));
        assert!(has_base("思ひます", "思う", WordForm::GodanVerb));
        assert!(has_base("言ひました", "言う", WordForm::GodanVerb));
        assert!(has_base("思はない", "思う", WordForm::GodanVerb));
        assert!(has_base("思へば", "思う", WordForm::GodanVerb));
        assert!(has_base("食べさう", "食べる", WordForm::IchidanVerb));
        assert!(has_base("高さう", "高い", WordForm::Adjective));
        assert!(has_base("食べやう", "食べる", WordForm::IchidanVerb));
        assert!(has_base("行きませう", "行く", WordForm::GodanVerb));
        assert!(has_base("食べてゐる", "食べる", WordForm::IchidanVerb));
        assert!(has_base("食べてゐた", "食べる", WordForm::IchidanVerb));
        assert!(has_base("飲んでゐる", "飲む", WordForm::GodanVerb));
        assert!(has_base("食べてしまふ", "食べる", WordForm::IchidanVerb));
        // -げ, -がる, -やがる (PR #2333), -やしない/-やしません (PR #2500)
        assert!(has_base("悲しげ", "悲しい", WordForm::Adjective));
        assert!(has_base("寒がる", "寒い", WordForm::Adjective));
        assert!(has_base("寒がった", "寒い", WordForm::Adjective));
        assert!(has_base("食べたがる", "食べる", WordForm::IchidanVerb));
        assert!(has_base("食べたがっていた", "食べる", WordForm::IchidanVerb));
        assert!(has_base("食べやがる", "食べる", WordForm::IchidanVerb));
        assert!(has_base("逃げやがった", "逃げる", WordForm::IchidanVerb));
        assert!(has_base("飲んでやがった", "飲む", WordForm::GodanVerb));
        assert!(has_base("食べやしない", "食べる", WordForm::IchidanVerb));
        assert!(has_base("行きやしません", "行く", WordForm::GodanVerb));
        assert!(has_base("行きやしませんでした", "行く", WordForm::GodanVerb));
        assert!(has_base("食べやしなかった", "食べる", WordForm::IchidanVerb));
        assert!(has_base("分かりゃしない", "分かる", WordForm::GodanVerb));
        // -過ぎる (kanji form) & full -ておく / -でおく
        assert!(has_base("食べ過ぎる", "食べる", WordForm::IchidanVerb));
        assert!(has_base("食べ過ぎた", "食べる", WordForm::IchidanVerb));
        assert!(has_base("高過ぎる", "高い", WordForm::Adjective));
        assert!(has_base("食べておく", "食べる", WordForm::IchidanVerb));
        assert!(has_base("買っておいた", "買う", WordForm::GodanVerb));
        assert!(has_base("飲んでおきます", "飲む", WordForm::GodanVerb));
        assert!(has_base("書いて置く", "書く", WordForm::GodanVerb));
    }

    #[test]
    fn h6_4_slang_kansai_honorific_irregulars_zuru_and_adj_e() {
        // Volitional slang, n-slang, imperative negative slang
        assert!(has_base("食べよっか", "食べる", WordForm::IchidanVerb));
        assert!(has_base("買おっか", "買う", WordForm::GodanVerb));
        assert!(has_base("行こっか", "行く", WordForm::GodanVerb));
        assert!(has_base("こっか", "くる", WordForm::KuruVerb));
        assert!(has_base("帰んなさい", "帰る", WordForm::GodanVerb));
        assert!(has_base("食べらんない", "食べる", WordForm::IchidanVerb));
        assert!(has_base("食べらんなかった", "食べる", WordForm::IchidanVerb));
        assert!(has_base("わかんない", "わかる", WordForm::GodanVerb));
        assert!(has_base("わかんなきゃ", "わかる", WordForm::GodanVerb));
        assert!(has_base("わかんなかった", "わかる", WordForm::GodanVerb));
        assert!(has_base("食べてん", "食べる", WordForm::IchidanVerb));
        assert!(has_base("見んな", "見る", WordForm::IchidanVerb));
        assert!(has_base("すんな", "する", WordForm::SuruVerb));
        assert!(has_base("来んな", "来る", WordForm::KuruVerb));
        assert!(has_base("触んな", "触る", WordForm::GodanVerb));

        // All 8 Kansai-ben groups
        assert!(has_base("行かへん", "行く", WordForm::GodanVerb));
        assert!(has_base("食べへん", "食べる", WordForm::IchidanVerb));
        assert!(has_base("食べひん", "食べる", WordForm::IchidanVerb));
        assert!(has_base("せえへん", "する", WordForm::SuruVerb));
        assert!(has_base("行かへんかった", "行く", WordForm::GodanVerb));
        assert!(has_base("せえへんかった", "する", WordForm::SuruVerb));
        assert!(has_base("食べひんかったら", "食べる", WordForm::IchidanVerb));
        assert!(has_base("食べられへん", "食べる", WordForm::IchidanVerb));
        assert!(has_base("書けへん", "書く", WordForm::GodanVerb));
        assert!(has_base("見いひん", "見る", WordForm::IchidanVerb));
        assert!(has_base("寝えへん", "寝る", WordForm::IchidanVerb));
        assert!(has_base("こおへん", "くる", WordForm::KuruVerb));
        assert!(has_base("きいひん", "くる", WordForm::KuruVerb));
        assert!(has_base("忘れてもうた", "忘れる", WordForm::IchidanVerb));
        assert!(has_base("こうて", "かう", WordForm::GodanVerb));
        assert!(has_base("こうてる", "かう", WordForm::GodanVerb));
        assert!(has_base("おうた", "あう", WordForm::GodanVerb));
        assert!(has_base("ゆうたら", "いう", WordForm::GodanVerb));
        assert!(has_base("ゆうたり", "いう", WordForm::GodanVerb));
        assert!(has_base("買うて", "買う", WordForm::GodanVerb));
        assert!(has_base("よう", "よい", WordForm::Adjective));
        assert!(has_base("しゅうて", "しい", WordForm::Adjective));
        assert!(has_base("嬉しゅうて", "嬉しい", WordForm::Adjective));
        assert!(has_base("おいしゅうて", "おいしい", WordForm::Adjective));
        assert!(has_base("たこうない", "たかい", WordForm::Adjective));
        assert!(has_base("たこうなかった", "たかい", WordForm::Adjective));
        assert!(has_base("こうない", "こい", WordForm::Adjective));
        assert!(has_base("食べとうない", "食べる", WordForm::IchidanVerb));
        assert!(has_base("食べていらっしゃいます", "食べる", WordForm::IchidanVerb));

        // Special honorific -ます verbs (PR #2324)
        assert!(has_base("いらっしゃいます", "いらっしゃる", WordForm::GodanVerb));
        assert!(has_base("いらっしゃいませ", "いらっしゃる", WordForm::GodanVerb));
        assert!(has_base("ございます", "ござる", WordForm::GodanVerb));
        assert!(has_base("ございました", "ござる", WordForm::GodanVerb));
        assert!(has_base("なさいます", "なさる", WordForm::GodanVerb));
        assert!(has_base("くださいます", "くださる", WordForm::GodanVerb));
        assert!(has_base("下さいます", "下さる", WordForm::GodanVerb));
        assert!(has_base("おっしゃいます", "おっしゃる", WordForm::GodanVerb));
        assert!(has_base("仰います", "仰る", WordForm::GodanVerb));

        // Missing irregular -て/-た/-たら/-たり verbs
        assert!(has_base("逝った", "逝く", WordForm::GodanVerb));
        assert!(has_base("逝って", "逝く", WordForm::GodanVerb));
        assert!(has_base("逝ったら", "逝く", WordForm::GodanVerb));
        assert!(has_base("往った", "往く", WordForm::GodanVerb));
        assert!(has_base("乞うて", "乞う", WordForm::GodanVerb));
        assert!(has_base("恋うた", "恋う", WordForm::GodanVerb));
        assert!(has_base("訪うたら", "訪う", WordForm::GodanVerb));
        assert!(has_base("宣うた", "宣う", WordForm::GodanVerb));
        assert!(has_base("のたまうて", "のたまう", WordForm::GodanVerb));
        assert!(has_base("曰うた", "曰う", WordForm::GodanVerb));
        assert!(has_base("給うて", "給う", WordForm::GodanVerb));
        assert!(has_base("たまうた", "たまう", WordForm::GodanVerb));
        assert!(has_base("賜うたら", "賜う", WordForm::GodanVerb));
        assert!(has_base("揺蕩うて", "揺蕩う", WordForm::GodanVerb));
        assert!(has_base("たゆたうたり", "たゆたう", WordForm::GodanVerb));

        // Zuru verbs (vz)
        assert!(has_base("論じた", "論ずる", WordForm::ZuruVerb));
        assert!(has_base("論じて", "論ずる", WordForm::ZuruVerb));
        assert!(has_base("論ずれば", "論ずる", WordForm::ZuruVerb));
        assert!(has_base("論ぜず", "論ずる", WordForm::ZuruVerb));
        assert!(has_base("信ぜられる", "信ずる", WordForm::ZuruVerb));
        assert!(has_base("生じます", "生ずる", WordForm::ZuruVerb));
        assert!(has_base("論ぜよ", "論ずる", WordForm::ZuruVerb));

        // Full 42-rule i-adjective -え / -い slang table
        assert!(has_base("すげえ", "すごい", WordForm::Adjective));
        assert!(has_base("すげぇ", "すごい", WordForm::Adjective));
        assert!(has_base("すげー", "すごい", WordForm::Adjective));
        assert!(has_base("うぜえ", "うざい", WordForm::Adjective));
        assert!(has_base("だりい", "だるい", WordForm::Adjective));
        assert!(has_base("さみい", "さむい", WordForm::Adjective));
        assert!(has_base("うめえ", "うまい", WordForm::Adjective));
        assert!(has_base("やべえ", "やばい", WordForm::Adjective));
        assert!(has_base("たけえ", "たかい", WordForm::Adjective));
        assert!(has_base("おもしれえ", "おもしろい", WordForm::Adjective));
        assert!(has_base("つええ", "つよい", WordForm::Adjective));
        assert!(has_base("あちい", "あつい", WordForm::Adjective));
        assert!(has_base("わりい", "わるい", WordForm::Adjective));
        assert!(has_base("かっこよかった", "かっこいい", WordForm::Adjective));
    }

    #[test]
    fn yomitan_1411_matta_nda_never_deconjugates_to_false_negative_matan() {
        let collapsed = crate::normalize::collapse_emphatic_sequences("待ったんだ", true);
        let hits = deconjugate_str(&collapsed.text, true);
        assert!(hits
            .iter()
            .any(|i| i.base == "待つ" && i.derivations.contains(&WordForm::Past)));
        assert!(
            !hits
                .iter()
                .any(|i| i.base == "待つ" && i.derivations.contains(&WordForm::ColloquialNegative)),
            "Yomitan #1411 regression: 待ったんだ must never produce colloquial negative 待たん"
        );
    }

    #[test]
    fn enforces_yomitan_sasu_passive_subconditions() {
        assert!(has_base("食べさせられた", "食べる", WordForm::IchidanVerb));
        assert!(has_base("書かされた", "書く", WordForm::GodanVerb));
        assert!(has_base("読まされた", "読む", WordForm::GodanVerb));
        assert!(has_base("食べさした", "食べる", WordForm::IchidanVerb));

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
        assert!(has_base("食べ", "食べる", WordForm::IchidanVerb));
        // CJK Extension J stem works cleanly (PR #2504 / #2522):
        let ext_j_stem = "\u{323B0}";
        assert!(has_base(ext_j_stem, "\u{323B0}る", WordForm::IchidanVerb));
    }

    #[test]
    fn pr_2506_leading_whitespace_is_trimmed_and_2153_candidate_explosion_is_capped() {
        // PR #2506: ASCII and fullwidth leading whitespace trimmed cleanly
        assert!(has_base("  \t食べた", "食べる", WordForm::IchidanVerb));
        assert!(has_base("　食べた", "食べる", WordForm::IchidanVerb));
        assert!(deconjugate_str("　行ったら", true)
            .iter()
            .any(|i| i.base == "行く" && i.derivations.contains(&WordForm::Tara)));

        // #2153: high-frequency hiragana query stays fast and bounded
        let started = std::time::Instant::now();
        let nagara = deconjugate_str("ながらながらながらながら", true);
        assert!(nagara.len() <= MAX_TOTAL_RESULTS);
        let budget_ms = if std::env::var_os("CI").is_some() {
            2000
        } else {
            200
        };
        assert!(started.elapsed().as_millis() < budget_ms);
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

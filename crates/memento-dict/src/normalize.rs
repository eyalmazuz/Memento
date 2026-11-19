//! Yomitan-compatible query normalization, bidirectional kana conversion,
//! prolonged sound mark (`ー`) vowel expansion, `々` iteration mark expansion,
//! and `search/remove-regex` / `search/replace-lines` text filters.

use std::borrow::Cow;
use std::collections::HashSet;

pub use regex::Regex;
use unicode_normalization::UnicodeNormalization;

use crate::databasemanager::{HIRAGANA_HIGH, HIRAGANA_LOW, KATAKANA_HIGH, KATAKANA_LOW};

const MAX_PROLONGED_VARIANTS: usize = 24;

/// Applies full Unicode NFKC normalization (e.g. `ＡＢＣ１２３` -> `ABC123`,
/// `ﾃﾚﾋﾞ` -> `テレビ`, `ｶﾞ` -> `ガ`, combining dakuten composition).
#[must_use]
pub fn normalize_nfkc(text: &str) -> String {
    text.nfkc().collect()
}

/// Converts katakana (`ァ`..=`ヶ`) in `text` to hiragana (`ぁ`..=`ゖ`), leaving other characters intact.
#[must_use]
pub fn kata_to_hira_str(text: &str) -> String {
    text.chars()
        .map(|ch| {
            let code = ch as u32;
            if (u32::from(KATAKANA_LOW)..=u32::from(KATAKANA_HIGH)).contains(&code) {
                char::from_u32(u32::from(HIRAGANA_LOW) + (code - u32::from(KATAKANA_LOW)))
                    .unwrap_or(ch)
            } else {
                ch
            }
        })
        .collect()
}

/// Converts hiragana (`ぁ`..=`ゖ`) in `text` to katakana (`ァ`..=`ヶ`), leaving other characters intact.
#[must_use]
pub fn hira_to_kata_str(text: &str) -> String {
    text.chars()
        .map(|ch| {
            let code = ch as u32;
            if (u32::from(HIRAGANA_LOW)..=u32::from(HIRAGANA_HIGH)).contains(&code) {
                char::from_u32(u32::from(KATAKANA_LOW) + (code - u32::from(HIRAGANA_LOW)))
                    .unwrap_or(ch)
            } else {
                ch
            }
        })
        .collect()
}

/// Normalizes text with NFKC followed by katakana-to-hiragana folding.
#[must_use]
pub fn normalize_reading_str(text: &str) -> String {
    let nfkc = normalize_nfkc(text);
    kata_to_hira_str(&nfkc)
}

/// Returns `true` if `ch` is a CJK Unified, Extension A-I, or Compatibility ideograph.
#[must_use]
pub fn is_cjk_ideograph(ch: char) -> bool {
    matches!(
        ch,
        '\u{3400}'..='\u{9FFF}' | '\u{F900}'..='\u{FAFF}' | '\u{20000}'..='\u{323AF}'
    )
}

fn vowel_expansions_for_prev_hira(prev: char) -> &'static [char] {
    match prev {
        'あ' | 'か' | 'が' | 'さ' | 'ざ' | 'た' | 'だ' | 'な' | 'は' | 'ば' | 'ぱ' | 'ま'
        | 'や' | 'ら' | 'わ' | 'ぁ' | 'ゃ' | 'ゎ' | 'ゕ' => &['あ'],
        'い' | 'き' | 'ぎ' | 'し' | 'じ' | 'ち' | 'ぢ' | 'に' | 'ひ' | 'び' | 'ぴ' | 'み'
        | 'り' | 'ぃ' => &['い'],
        'う' | 'ゔ' | 'く' | 'ぐ' | 'す' | 'ず' | 'つ' | 'づ' | 'ぬ' | 'ふ' | 'ぶ' | 'ぷ'
        | 'む' | 'ゆ' | 'る' | 'ぅ' | 'ゅ' => &['う'],
        'え' | 'け' | 'げ' | 'せ' | 'ぜ' | 'て' | 'で' | 'ね' | 'へ' | 'べ' | 'ぺ' | 'め'
        | 'れ' | 'ぇ' | 'ゖ' => &['え', 'い'],
        'お' | 'こ' | 'ご' | 'そ' | 'ぞ' | 'と' | 'ど' | 'の' | 'ほ' | 'ぼ' | 'ぽ' | 'も'
        | 'よ' | 'ろ' | 'を' | 'ぉ' | 'ょ' => &['う', 'お'],
        _ => &[],
    }
}

/// Expands prolonged sound marks (`ー`) after kana into orthographic vowel spellings,
/// including `ねー` -> `ねえ`, `すげー` -> `すげえ`, `おーきい` -> `おおきい`,
/// and colloquial `ゆー` -> `いう`/`ゆう` (`こーゆー` -> `こういう`/`こうゆう`).
#[must_use]
pub fn expand_prolonged_vowels(text: &str) -> Vec<String> {
    if !text.contains('ー') {
        return vec![text.to_owned()];
    }
    let hira = kata_to_hira_str(text);
    let chars: Vec<char> = hira.chars().collect();
    let mut partials: Vec<String> = vec![String::with_capacity(hira.len())];
    let mut i = 0usize;

    while i < chars.len() {
        let ch = chars[i];
        // Special colloquial `ゆー` -> both `ゆう` and `いう` (e.g. `こーゆー` -> `こういう` / `こうゆう`).
        if ch == 'ゆ' && chars.get(i + 1) == Some(&'ー') {
            let mut next_partials = Vec::with_capacity((partials.len() * 2).min(MAX_PROLONGED_VARIANTS));
            for base in &partials {
                if next_partials.len() >= MAX_PROLONGED_VARIANTS {
                    break;
                }
                let mut v_iu = base.clone();
                v_iu.push_str("いう");
                next_partials.push(v_iu);
                if next_partials.len() < MAX_PROLONGED_VARIANTS {
                    let mut v_yuu = base.clone();
                    v_yuu.push_str("ゆう");
                    next_partials.push(v_yuu);
                }
            }
            partials = next_partials;
            i += 2;
            continue;
        }

        if ch == 'ー' && i > 0 {
            let prev = chars[i - 1];
            let vowels = vowel_expansions_for_prev_hira(prev);
            if !vowels.is_empty() {
                let mut next_partials =
                    Vec::with_capacity((partials.len() * vowels.len()).min(MAX_PROLONGED_VARIANTS));
                'outer: for base in &partials {
                    for &v in vowels {
                        if next_partials.len() >= MAX_PROLONGED_VARIANTS {
                            break 'outer;
                        }
                        let mut candidate = base.clone();
                        candidate.push(v);
                        next_partials.push(candidate);
                    }
                }
                partials = next_partials;
                i += 1;
                continue;
            }
        }

        for p in &mut partials {
            p.push(ch);
        }
        i += 1;
    }

    let mut out = Vec::with_capacity(partials.len() + 1);
    let mut seen = HashSet::with_capacity(partials.len() + 1);
    if seen.insert(text.to_owned()) {
        out.push(text.to_owned());
    }
    for p in partials {
        if seen.insert(p.clone()) {
            out.push(p);
        }
    }
    out
}

/// Expands CJK iteration mark `々` bidirectionally (`人々` <-> `人人`).
#[must_use]
pub fn expand_iteration_marks(text: &str) -> Vec<String> {
    let mut out = vec![text.to_owned()];
    let chars: Vec<char> = text.chars().collect();
    if chars.len() < 2 {
        return out;
    }

    // 1. `X々` -> `XX`
    if chars.contains(&'々') {
        let mut expanded = String::with_capacity(text.len());
        let mut prev: Option<char> = None;
        for &ch in &chars {
            if ch == '々' {
                if let Some(p) = prev {
                    expanded.push(p);
                    prev = Some(p);
                    continue;
                }
            }
            expanded.push(ch);
            prev = Some(ch);
        }
        if expanded != text {
            out.push(expanded);
        }
    }

    // 2. `XX` -> `X々` for repeated CJK ideographs
    let mut has_repeated_kanji = false;
    for pair in chars.windows(2) {
        if pair[0] == pair[1] && is_cjk_ideograph(pair[0]) {
            has_repeated_kanji = true;
            break;
        }
    }
    if has_repeated_kanji {
        let mut folded = String::with_capacity(text.len());
        let mut i = 0usize;
        while i < chars.len() {
            folded.push(chars[i]);
            if i + 1 < chars.len() && chars[i] == chars[i + 1] && is_cjk_ideograph(chars[i]) {
                folded.push('々');
                i += 2;
            } else {
                i += 1;
            }
        }
        if folded != text && !out.contains(&folded) {
            out.push(folded);
        }
    }

    out
}

/// Expands a query or prefix into all Yomitan-equivalent lookup forms:
/// raw, NFKC, bidirectional hiragana <-> katakana, `々` <-> `XX`, and prolonged `ー` vowels.
#[must_use]
pub fn expand_query_variants(text: &str) -> Vec<String> {
    if text.is_empty() {
        return Vec::new();
    }
    let mut out: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut push = |s: String| {
        if !s.is_empty() && seen.insert(s.clone()) {
            out.push(s);
        }
    };

    let nfkc = normalize_nfkc(text);
    push(text.to_owned());
    push(nfkc.clone());

    let seeds: Vec<String> = expand_iteration_marks(&nfkc);
    for seed in seeds {
        let hira = kata_to_hira_str(&seed);
        let kata = hira_to_kata_str(&seed);
        push(seed.clone());
        push(hira.clone());
        push(kata);

        if seed.contains('ー') {
            for expanded in expand_prolonged_vowels(&seed) {
                let exp_kata = hira_to_kata_str(&expanded);
                push(expanded);
                push(exp_kata);
            }
        }
    }

    out
}

/// Compiles a config `search/remove-regex` pattern, returning `None` for empty or invalid regexes.
#[must_use]
pub fn compile_remove_regex(pattern: &str) -> Option<Regex> {
    if pattern.is_empty() {
        return None;
    }
    Regex::new(pattern).ok()
}

/// Applies a compiled `search/remove-regex` filter to `text`.
#[must_use]
pub fn apply_compiled_remove_regex<'a>(text: &'a str, regex: Option<&Regex>) -> Cow<'a, str> {
    match regex {
        Some(re) => re.replace_all(text, ""),
        None => Cow::Borrowed(text),
    }
}

/// Removes all substrings matching `pattern` (`search/remove-regex`).
/// Invalid or empty regex patterns safely return `text` unchanged.
#[must_use]
pub fn apply_remove_regex(text: &str, pattern: &str) -> String {
    match compile_remove_regex(pattern) {
        Some(re) => re.replace_all(text, "").into_owned(),
        None => text.to_owned(),
    }
}

/// Replaces `\n` with `replace_with` when `replace_lines` (`search/replace-lines`) is enabled.
#[must_use]
pub fn apply_replace_lines(text: &str, replace_lines: bool, replace_with: &str) -> String {
    if !replace_lines || !text.contains('\n') {
        return text.to_owned();
    }
    text.replace('\n', replace_with)
}

/// Applies Memento's `search/remove-regex` followed by `search/replace-lines` (`search/replace-with`).
#[must_use]
pub fn preprocess_search_text(
    text: &str,
    remove_regex: &str,
    replace_lines: bool,
    replace_with: &str,
) -> String {
    let filtered = apply_remove_regex(text, remove_regex);
    apply_replace_lines(&filtered, replace_lines, replace_with)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nfkc_and_bidirectional_kana_variants() {
        assert_eq!(normalize_nfkc("ＡＢＣ１２３！？"), "ABC123!?");
        assert_eq!(normalize_nfkc("ﾃﾚﾋﾞ"), "テレビ");

        let from_hira = expand_query_variants("てれび");
        assert!(from_hira.contains(&"てれび".to_owned()));
        assert!(from_hira.contains(&"テレビ".to_owned()));

        let from_kata = expand_query_variants("テレビ");
        assert!(from_kata.contains(&"テレビ".to_owned()));
        assert!(from_kata.contains(&"てれび".to_owned()));

        let from_half = expand_query_variants("ﾃﾚﾋﾞ");
        assert!(from_half.contains(&"テレビ".to_owned()));
        assert!(from_half.contains(&"てれび".to_owned()));
    }

    #[test]
    fn prolonged_sound_mark_and_iteration_expansions() {
        assert!(expand_query_variants("ねー").contains(&"ねえ".to_owned()));
        assert!(expand_query_variants("すげー").contains(&"すげえ".to_owned()));
        assert!(expand_query_variants("おーきい").contains(&"おおきい".to_owned()));
        let koyu = expand_query_variants("こーゆー");
        assert!(koyu.contains(&"こういう".to_owned()));
        assert!(koyu.contains(&"こうゆう".to_owned()));

        assert!(expand_query_variants("人々").contains(&"人人".to_owned()));
        assert!(expand_query_variants("人人").contains(&"人々".to_owned()));
    }

    #[test]
    fn search_regex_and_replace_lines_helpers() {
        assert_eq!(
            preprocess_search_text("（太郎）食べる\n飲む", r"^（[^）]*）", true, " "),
            "食べる 飲む"
        );
        // Invalid regex is safely ignored:
        assert_eq!(apply_remove_regex("食べる", "[unclosed"), "食べる");
        assert_eq!(apply_replace_lines("a\nb", false, " "), "a\nb");
    }
}

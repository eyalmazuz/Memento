//! Pure search controller sorting, deduplication, cloze slicing, model move planning,
//! custom text replacement patterns (H6.5), wildcard detection/escaping (H6.2), and
//! candidate explosion capping (#2153).

use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};

use regex::RegexBuilder;
use serde::{Deserialize, Serialize};

/// Default hard cap on deduplicated search query candidates to prevent candidate explosion
/// on high-frequency hiragana inputs (Yomitan #2153).
pub const MAX_CANDIDATE_QUERIES: usize = 256;

/// Source origin of a search query.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum QuerySource {
    Exact,
    Deconj,
    Mecab,
}

impl QuerySource {
    pub const fn from_u8(val: u8) -> Self {
        match val {
            1 => QuerySource::Deconj,
            2 => QuerySource::Mecab,
            _ => QuerySource::Exact,
        }
    }

    pub const fn to_u8(self) -> u8 {
        match self {
            QuerySource::Exact => 0,
            QuerySource::Deconj => 1,
            QuerySource::Mecab => 2,
        }
    }
}

/// Candidate search query descriptor for sorting and filtering.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CandidateSearchQuery<'a> {
    pub original_index: usize,
    pub deconj: &'a str,
    pub surface_len: u64,
    pub rule_mask: u8,
    pub source: QuerySource,
}

/// Sort candidate queries descending by deconj, breaking ties by descending
/// surface length, and stably preserving original index.
pub fn sort_queries(queries: &mut [CandidateSearchQuery]) {
    queries.sort_by(|a, b| {
        b.deconj
            .cmp(a.deconj)
            .then_with(|| b.surface_len.cmp(&a.surface_len))
            .then_with(|| a.original_index.cmp(&b.original_index))
    });
}

/// Filter candidate queries:
/// 1. If mecab_enabled: removes Mecab queries if any Deconj query has the same deconj string.
/// 2. Deduplicates queries with identical `(deconj, rule_mask)`, keeping the first occurrence.
/// 3. Removes queries with empty deconj.
pub fn filter_duplicates(queries: &mut Vec<CandidateSearchQuery>, mecab_enabled: bool) {
    if mecab_enabled
        && queries.iter().any(|q| q.source == QuerySource::Mecab)
        && queries.iter().any(|q| q.source == QuerySource::Deconj)
    {
        let deconj_set: HashSet<&str> = queries
            .iter()
            .filter(|q| q.source == QuerySource::Deconj)
            .map(|q| q.deconj)
            .collect();

        queries.retain(|q| !(q.source == QuerySource::Mecab && deconj_set.contains(q.deconj)));
    }

    let mut seen: HashSet<(&str, u8)> = HashSet::with_capacity(queries.len());
    queries.retain(|q| !q.deconj.is_empty() && seen.insert((q.deconj, q.rule_mask)));
}

/// Deduplicates and caps candidate queries to at most `max_candidates` (preserving
/// longest `surface_len` candidates first when capping) so high-frequency hiragana queries
/// cannot explode search latency (Yomitan #2153).
pub fn cap_and_dedup_queries(
    queries: &mut Vec<CandidateSearchQuery>,
    mecab_enabled: bool,
    max_candidates: usize,
) {
    filter_duplicates(queries, mecab_enabled);
    if queries.len() > max_candidates {
        queries.sort_by(|a, b| {
            b.surface_len
                .cmp(&a.surface_len)
                .then_with(|| a.source.to_u8().cmp(&b.source.to_u8()))
                .then_with(|| a.original_index.cmp(&b.original_index))
        });
        queries.truncate(max_candidates);
    }
}

/// Sort and deduplicate queries, returning the surviving original indices in sorted order.
pub fn sort_and_filter_queries(
    mut queries: Vec<CandidateSearchQuery>,
    mecab_enabled: bool,
) -> Vec<usize> {
    sort_queries(&mut queries);
    filter_duplicates(&mut queries, mecab_enabled);
    queries.into_iter().map(|q| q.original_index).collect()
}

/// A single Yomitan-compatible custom regex text replacement rule (H6.5).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextReplacementRule {
    pub pattern: String,
    pub replacement: String,
    pub ignore_case: bool,
    pub enabled: bool,
}

/// Ordered set of custom text replacement rules with Yomitan's `search_original` toggle (H6.5).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextReplacementOptions {
    pub rules: Vec<TextReplacementRule>,
    pub search_original: bool,
}

impl Default for TextReplacementOptions {
    fn default() -> Self {
        Self {
            rules: Vec::new(),
            search_original: true,
        }
    }
}

/// Applies ordered enabled regex replacements from `options` to `text`.
///
/// - Invalid or empty regex patterns are gracefully skipped.
/// - Rules are chained sequentially so rule `i+1` operates on the output of rule `i`.
/// - When `options.search_original` is `true`, both the original string and the transformed
///   string (if changed and non-empty) are returned, deduplicated.
/// - When `options.search_original` is `false`, only the transformed string is returned
///   (or empty `Vec` if the transformed string is empty).
#[must_use]
pub fn apply_text_replacements(text: &str, options: &TextReplacementOptions) -> Vec<String> {
    if text.is_empty() {
        return Vec::new();
    }

    let mut current = text.to_owned();
    for rule in &options.rules {
        if !rule.enabled || rule.pattern.is_empty() {
            continue;
        }
        let Ok(re) = RegexBuilder::new(&rule.pattern)
            .case_insensitive(rule.ignore_case)
            .build()
        else {
            continue;
        };
        if let std::borrow::Cow::Owned(replaced) = re.replace_all(&current, rule.replacement.as_str())
        {
            current = replaced;
        }
    }

    let mut out = Vec::with_capacity(2);
    if options.search_original {
        out.push(text.to_owned());
        if !current.is_empty() && current != text {
            out.push(current);
        }
    } else if !current.is_empty() {
        out.push(current);
    }
    out
}

/// Returns `true` if `query` contains any ASCII or fullwidth wildcard character
/// (`*`, `?`, `＊`, `？`) for Yomitan-compatible wildcard search (H6.2).
#[must_use]
pub fn has_wildcard_chars(query: &str) -> bool {
    query
        .chars()
        .any(|ch| matches!(ch, '*' | '?' | '＊' | '？'))
}

/// Returns the literal UTF-8 prefix before the first wildcard character (`*`, `?`, `＊`, `？`),
/// or the entire string if no wildcard character is present.
#[must_use]
pub fn wildcard_literal_prefix(query: &str) -> &str {
    match query.find(['*', '?', '＊', '？']) {
        Some(idx) => &query[..idx],
        None => query,
    }
}

/// Converts a user wildcard query (`*`/`＊` = zero or more chars, `?`/`？` = single char)
/// into a SQL `LIKE ... ESCAPE '\'` pattern, safely escaping literal `\`, `%`, and `_`
/// and collapsing consecutive `%` wildcards.
#[must_use]
pub fn wildcard_to_like_pattern(query: &str) -> String {
    let mut pattern = String::with_capacity(query.len() + 4);
    let mut prev_percent = false;
    for ch in query.chars() {
        match ch {
            '*' | '＊' => {
                if !prev_percent {
                    pattern.push('%');
                    prev_percent = true;
                }
            }
            '?' | '？' => {
                pattern.push('_');
                prev_percent = false;
            }
            '\\' | '%' | '_' => {
                pattern.push('\\');
                pattern.push(ch);
                prev_percent = false;
            }
            other => {
                pattern.push(other);
                prev_percent = false;
            }
        }
    }
    pattern
}

/// Stateful search controller helper managing custom text replacement options,
/// wildcard query detection, leading-whitespace trimming (PR #2506), and bounded
/// candidate generation (#2153).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SearchController {
    replacement_options: TextReplacementOptions,
}

impl SearchController {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn with_text_replacements(replacement_options: TextReplacementOptions) -> Self {
        Self {
            replacement_options,
        }
    }

    #[must_use]
    pub fn text_replacement_options(&self) -> &TextReplacementOptions {
        &self.replacement_options
    }

    pub fn set_text_replacement_options(&mut self, options: TextReplacementOptions) {
        self.replacement_options = options;
    }

    /// Applies configured custom text replacement patterns to `text`.
    #[must_use]
    pub fn apply_text_replacements(&self, text: &str) -> Vec<String> {
        apply_text_replacements(text, &self.replacement_options)
    }

    /// Detects whether a manual search query should use wildcard search (`*`, `?`, `＊`, `？`).
    #[must_use]
    pub fn is_wildcard_query(&self, query: &str) -> bool {
        has_wildcard_chars(query)
    }

    /// Trims leading whitespace (PR #2506), applies configured custom text replacements,
    /// expands Yomitan normalization variants, deduplicates, and caps at [`MAX_CANDIDATE_QUERIES`].
    #[must_use]
    pub fn prepare_query_candidates(&self, raw_query: &str) -> Vec<String> {
        let trimmed = crate::normalize::trim_leading_whitespace(raw_query);
        if trimmed.is_empty() {
            return Vec::new();
        }
        let seeds = self.apply_text_replacements(trimmed);
        let mut out = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        for seed in seeds {
            if out.len() >= MAX_CANDIDATE_QUERIES {
                break;
            }
            for variant in crate::normalize::expand_query_variants(&seed) {
                if out.len() >= MAX_CANDIDATE_QUERIES {
                    break;
                }
                if seen.insert(variant.clone()) {
                    out.push(variant);
                }
            }
        }
        out
    }
}

/// Character-index `(start, len)` spans for Anki sentence cloze formatting.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct ClozeSpans {
    pub prefix_start: u64,
    pub prefix_len: u64,
    pub body_start: u64,
    pub body_len: u64,
    pub suffix_start: u64,
    pub suffix_len: u64,
}

/// Computes `(prefix, body, suffix)` spans for cloze generation without
/// signed/width overflow.
pub fn cloze_spans(text_len: u64, index: i64, surface_len: u64) -> ClozeSpans {
    let prefix = slice_prefix(text_len, index);
    let body = slice_mid(text_len, index, i128::from(surface_len));
    let suffix_n = i128::from(text_len) - i128::from(index) - i128::from(surface_len);
    let suffix = slice_suffix(text_len, suffix_n);

    ClozeSpans {
        prefix_start: prefix.0,
        prefix_len: prefix.1,
        body_start: body.0,
        body_len: body.1,
        suffix_start: suffix.0,
        suffix_len: suffix.1,
    }
}

fn slice_prefix(text_len: u64, n: i64) -> (u64, u64) {
    if n < 0 || (n as u64) >= text_len {
        (0, text_len)
    } else {
        (0, n as u64)
    }
}

fn slice_suffix(text_len: u64, n: i128) -> (u64, u64) {
    if n < 0 || n >= i128::from(text_len) {
        (0, text_len)
    } else {
        let len = n as u64;
        (text_len - len, len)
    }
}

fn slice_mid(text_len: u64, pos: i64, mut len: i128) -> (u64, u64) {
    let orig = i128::from(text_len);
    let mut pos = i128::from(pos);
    if pos > orig {
        return (0, 0);
    }
    if pos < 0 {
        if len < 0 || len + pos >= orig {
            return (0, text_len);
        }
        if len + pos <= 0 {
            return (0, 0);
        }
        len += pos;
        pos = 0;
    } else if len < 0 || len > orig - pos {
        len = orig - pos;
    }

    if len <= 0 {
        (0, 0)
    } else {
        (pos as u64, len as u64)
    }
}

/// Item descriptor for sorting Term search results.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TermSortItem {
    pub original_index: usize,
    pub cloze_body_len: u64,
    pub expr_equals_cloze: bool,
    pub expr_len: u64,
    pub conj_explanation_len: u64,
    pub score: i64,
}

/// Sort terms matching DictionarySearchController::sortTerms comparator:
/// 1. Descending cloze_body length
/// 2. Exact match (expression == cloze_body) before non-exact
/// 3. Descending expression length
/// 4. Non-empty conjugation explanation before empty
/// 5. Descending score
/// 6. Stably preserving input order on ties
pub fn sort_terms(mut items: Vec<TermSortItem>) -> Vec<usize> {
    items.sort_by(|a, b| {
        b.cloze_body_len
            .cmp(&a.cloze_body_len)
            .then_with(|| b.expr_equals_cloze.cmp(&a.expr_equals_cloze))
            .then_with(|| b.expr_len.cmp(&a.expr_len))
            .then_with(|| {
                let a_has_conj = a.conj_explanation_len > 0;
                let b_has_conj = b.conj_explanation_len > 0;
                b_has_conj.cmp(&a_has_conj)
            })
            .then_with(|| b.score.cmp(&a.score))
            .then_with(|| a.original_index.cmp(&b.original_index))
    });
    items.into_iter().map(|item| item.original_index).collect()
}

/// Item descriptor for dictionary priority sorting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PrioritySortItem {
    pub original_index: usize,
    pub dict_id: i64,
    pub score: i64,
    pub use_score_tiebreak: bool,
}

/// Sort items by dictionary priority:
/// 1. Ascending dictionary priority (lowest value = highest priority)
/// 2. If use_score_tiebreak: descending score
/// 3. Stably preserving input order on ties
pub fn sort_by_dict_priority(
    priorities: &[(i64, i64)],
    missing_default: i64,
    mut items: Vec<PrioritySortItem>,
) -> Vec<usize> {
    let priority_map: HashMap<i64, i64> = priorities.iter().copied().collect();

    items.sort_by_key(|item| {
        let prio = priority_map
            .get(&item.dict_id)
            .copied()
            .unwrap_or(missing_default);
        (
            prio,
            !item.use_score_tiebreak,
            Reverse(if item.use_score_tiebreak {
                item.score
            } else {
                0
            }),
            item.original_index,
        )
    });
    items.into_iter().map(|item| item.original_index).collect()
}

/// Item descriptor for Tag sorting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TagSortItem {
    pub original_index: usize,
    pub order: i64,
    pub score: i64,
}

/// Sort tags matching DictionarySearchController::sortTags:
/// 1. Ascending tag order
/// 2. Descending tag score
/// 3. Stably preserving input order
pub fn sort_tags(mut items: Vec<TagSortItem>) -> Vec<usize> {
    items.sort_by(|a, b| {
        a.order
            .cmp(&b.order)
            .then_with(|| b.score.cmp(&a.score))
            .then_with(|| a.original_index.cmp(&b.original_index))
    });
    items.into_iter().map(|item| item.original_index).collect()
}

/// Validate and plan move in DictionaryInfoModel:
/// Returns false if from == to or either index is out of bounds [0, count).
/// Otherwise computes destination = (to > from) ? to + 1 : to and returns true.
pub fn plan_move(from: i32, to: i32, count: i32) -> Option<i32> {
    if from == to || from < 0 || from >= count || to < 0 || to >= count {
        None
    } else {
        Some(if to > from { to + 1 } else { to })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sort_and_filter_queries() {
        let q0 = CandidateSearchQuery {
            original_index: 0,
            deconj: "あ",
            surface_len: 1,
            rule_mask: 1,
            source: QuerySource::Deconj,
        };
        let q1 = CandidateSearchQuery {
            original_index: 1,
            deconj: "あい",
            surface_len: 2,
            rule_mask: 1,
            source: QuerySource::Deconj,
        };
        let q2 = CandidateSearchQuery {
            original_index: 2,
            deconj: "あい",
            surface_len: 2,
            rule_mask: 1,
            source: QuerySource::Mecab,
        };
        let q3 = CandidateSearchQuery {
            original_index: 3,
            deconj: "お",
            surface_len: 3,
            rule_mask: 2,
            source: QuerySource::Exact,
        };
        let q4 = CandidateSearchQuery {
            original_index: 4,
            deconj: "",
            surface_len: 0,
            rule_mask: 0,
            source: QuerySource::Exact,
        };

        let result = sort_and_filter_queries(vec![q0, q1, q2, q3, q4], true);
        // Desired order: descending deconj -> お (index 3), あい (index 1; index 2 mecab removed by deconj set), あ (index 0).
        // Empty is removed.
        assert_eq!(result, vec![3, 1, 0]);
    }

    #[test]
    fn test_cloze_spans() {
        // "abcde", len=5, idx=1, surface_len=3
        let spans = cloze_spans(5, 1, 3);
        assert_eq!(spans.prefix_start, 0);
        assert_eq!(spans.prefix_len, 1);
        assert_eq!(spans.body_start, 1);
        assert_eq!(spans.body_len, 3);
        assert_eq!(spans.suffix_start, 4);
        assert_eq!(spans.suffix_len, 1);

        // Negative index -1, surface 3:
        let spans_neg = cloze_spans(5, -1, 3);
        assert_eq!(spans_neg.prefix_start, 0);
        assert_eq!(spans_neg.prefix_len, 5);
        assert_eq!(spans_neg.body_start, 0);
        assert_eq!(spans_neg.body_len, 2);
        assert_eq!(spans_neg.suffix_start, 2);
        assert_eq!(spans_neg.suffix_len, 3);
    }

    #[test]
    fn test_sort_terms() {
        let t0 = TermSortItem {
            original_index: 0,
            cloze_body_len: 2,
            expr_equals_cloze: false,
            expr_len: 2,
            conj_explanation_len: 0,
            score: 10,
        };
        let t1 = TermSortItem {
            original_index: 1,
            cloze_body_len: 3,
            expr_equals_cloze: true,
            expr_len: 3,
            conj_explanation_len: 0,
            score: 0,
        };
        let t2 = TermSortItem {
            original_index: 2,
            cloze_body_len: 3,
            expr_equals_cloze: false,
            expr_len: 3,
            conj_explanation_len: 5,
            score: 100,
        };

        let result = sort_terms(vec![t0, t1, t2]);
        // Cloze body len 3 beats 2: t1 and t2 first.
        // t1 expr_equals_cloze is true, t2 is false => t1 first.
        // Then t2, then t0.
        assert_eq!(result, vec![1, 2, 0]);
    }

    #[test]
    fn test_sort_by_dict_priority() {
        let priorities = vec![(10, 0), (20, 1), (30, 2)];
        let items = vec![
            PrioritySortItem {
                original_index: 0,
                dict_id: 30,
                score: 5,
                use_score_tiebreak: true,
            },
            PrioritySortItem {
                original_index: 1,
                dict_id: 10,
                score: 1,
                use_score_tiebreak: true,
            },
            PrioritySortItem {
                original_index: 2,
                dict_id: 20,
                score: 10,
                use_score_tiebreak: true,
            },
            PrioritySortItem {
                original_index: 3,
                dict_id: 20,
                score: 20,
                use_score_tiebreak: true,
            },
        ];

        let result = sort_by_dict_priority(&priorities, 0, items);
        // dict 10 has prio 0 -> index 1
        // dict 20 has prio 1: tiebreak on score (descending) -> index 3 (score 20) then index 2 (score 10)
        // dict 30 has prio 2 -> index 0
        assert_eq!(result, vec![1, 3, 2, 0]);
    }

    #[test]
    fn test_sort_tags() {
        let items = vec![
            TagSortItem { original_index: 0, order: 2, score: 10 },
            TagSortItem { original_index: 1, order: 1, score: 5 },
            TagSortItem { original_index: 2, order: 1, score: 20 },
        ];
        let result = sort_tags(items);
        // Order 1 before order 2. On tie order 1: score 20 before score 5 => index 2, then index 1, then index 0.
        assert_eq!(result, vec![2, 1, 0]);
    }

    #[test]
    fn test_plan_move() {
        assert_eq!(plan_move(0, 0, 5), None);
        assert_eq!(plan_move(-1, 2, 5), None);
        assert_eq!(plan_move(1, 5, 5), None);
        assert_eq!(plan_move(1, 3, 5), Some(4)); // to > from => to + 1
        assert_eq!(plan_move(3, 1, 5), Some(1)); // to <= from => to
    }

    #[test]
    fn custom_text_replacement_patterns_respect_order_flags_and_search_original() {
        let opts = TextReplacementOptions {
            rules: vec![
                TextReplacementRule {
                    pattern: r"\[[^\]]*\]".to_owned(),
                    replacement: String::new(),
                    ignore_case: false,
                    enabled: true,
                },
                TextReplacementRule {
                    pattern: "disabled".to_owned(),
                    replacement: "NOPE".to_owned(),
                    ignore_case: true,
                    enabled: false,
                },
                TextReplacementRule {
                    pattern: "[invalid".to_owned(),
                    replacement: "NOPE".to_owned(),
                    ignore_case: false,
                    enabled: true,
                },
                TextReplacementRule {
                    pattern: "abc".to_owned(),
                    replacement: "食べる".to_owned(),
                    ignore_case: true,
                    enabled: true,
                },
            ],
            search_original: true,
        };

        let with_orig = apply_text_replacements("[note]ABC", &opts);
        assert_eq!(with_orig, vec!["[note]ABC".to_owned(), "食べる".to_owned()]);

        let opts_no_orig = TextReplacementOptions {
            search_original: false,
            ..opts.clone()
        };
        let replaced_only = apply_text_replacements("[note]ABC", &opts_no_orig);
        assert_eq!(replaced_only, vec!["食べる".to_owned()]);

        // Chained rules: rule 2 sees output of rule 1
        let chained = TextReplacementOptions {
            rules: vec![
                TextReplacementRule {
                    pattern: "foo".to_owned(),
                    replacement: "bar".to_owned(),
                    ignore_case: false,
                    enabled: true,
                },
                TextReplacementRule {
                    pattern: "bar".to_owned(),
                    replacement: "baz".to_owned(),
                    ignore_case: false,
                    enabled: true,
                },
            ],
            search_original: false,
        };
        assert_eq!(
            apply_text_replacements("foo", &chained),
            vec!["baz".to_owned()]
        );

        let controller = SearchController::with_text_replacements(opts_no_orig);
        let candidates = controller.prepare_query_candidates("  　[note]ABC");
        assert!(candidates.contains(&"食べる".to_owned()));
        assert!(!candidates.iter().any(|c| c.starts_with(char::is_whitespace)));
    }

    #[test]
    fn wildcard_detection_prefix_and_like_escaping_are_utf8_safe() {
        assert!(has_wildcard_chars("食べ*"));
        assert!(has_wildcard_chars("食べ？"));
        assert!(has_wildcard_chars("＊べる"));
        assert!(has_wildcard_chars("た?る"));
        assert!(!has_wildcard_chars("食べる"));

        assert_eq!(wildcard_literal_prefix("食べ*"), "食べ");
        assert_eq!(wildcard_literal_prefix("＊べる"), "");
        assert_eq!(wildcard_literal_prefix("食べる"), "食べる");
        assert_eq!(wildcard_literal_prefix("\u{323B0}\u{31350}＊"), "\u{323B0}\u{31350}");

        // Escapes SQL LIKE metacharacters (%, _, \) and converts */＊->% and ?/？->_
        assert_eq!(wildcard_to_like_pattern("100%_a\\b*？**"), r"100\%\_a\\b%_%");
        assert_eq!(wildcard_to_like_pattern("食＊べ？"), "食%べ_");
    }

    #[test]
    fn cap_and_dedup_queries_bounds_candidate_explosion_2153() {
        let keys: Vec<String> = (0..400u32)
            .map(|i| {
                let c1 = char::from_u32(0x3042 + (i % 60)).unwrap();
                let c2 = char::from_u32(0x3042 + (i / 60)).unwrap();
                format!("{c1}{c2}")
            })
            .collect();
        let mut queries: Vec<CandidateSearchQuery<'_>> = keys
            .iter()
            .enumerate()
            .map(|(idx, k)| CandidateSearchQuery {
                original_index: idx,
                deconj: k.as_str(),
                surface_len: (idx % 10 + 1) as u64,
                rule_mask: 1,
                source: QuerySource::Deconj,
            })
            .collect();
        // Inject non-consecutive duplicate of index 0
        queries.push(CandidateSearchQuery {
            original_index: 999,
            deconj: keys[0].as_str(),
            surface_len: 1,
            rule_mask: 1,
            source: QuerySource::Deconj,
        });

        cap_and_dedup_queries(&mut queries, false, 64);
        assert_eq!(queries.len(), 64);
        assert!(!queries.iter().any(|q| q.original_index == 999));
        // Longest surface_len candidates are prioritized when capping
        assert_eq!(queries[0].surface_len, 10);
    }
}

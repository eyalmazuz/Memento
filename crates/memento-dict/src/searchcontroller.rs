//! Pure search controller sorting, deduplication, cloze slicing, and model move planning.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};

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
    pub deconj: &'a [u16],
    pub surface_len: u64,
    pub rule_mask: u8,
    pub source: QuerySource,
}

/// Sort candidate queries descending by deconj (UTF-16 lexicographical),
/// breaking ties by descending surface length, and stably preserving original index.
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
/// 2. Deduplicates consecutive queries with identical (deconj, rule_mask), keeping the first.
/// 3. Removes queries with empty deconj.
pub fn filter_duplicates(queries: &mut Vec<CandidateSearchQuery>, mecab_enabled: bool) {
    if mecab_enabled {
        let deconj_set: HashSet<&[u16]> = queries
            .iter()
            .filter(|q| q.source == QuerySource::Deconj)
            .map(|q| q.deconj)
            .collect();

        queries.retain(|q| !(q.source == QuerySource::Mecab && deconj_set.contains(q.deconj)));
    }

    // std::unique in C++ keeps the first element of each consecutive group with equal key.
    queries.dedup_by(|a, b| a.deconj == b.deconj && a.rule_mask == b.rule_mask);

    queries.retain(|q| !q.deconj.is_empty());
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

/// Cloze span offsets and lengths matching QString::left/mid/right.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct ClozeSpans {
    pub prefix_start: u64,
    pub prefix_len: u64,
    pub body_start: u64,
    pub body_len: u64,
    pub suffix_start: u64,
    pub suffix_len: u64,
}

/// Computes (prefix, body, suffix) spans for cloze generation matching
/// QString::left(index), QString::mid(index, surface_len), and
/// QString::right(text.size() - (index + surface_len)) bit-for-bit.
pub fn cloze_spans(text_len: u64, index: i64, surface_len: u64) -> ClozeSpans {
    let prefix = qstring_left(text_len, index);
    let body = qstring_mid(text_len, index, surface_len as i64);
    let suffix_n = (text_len as i64).wrapping_sub(index.wrapping_add(surface_len as i64));
    let suffix = qstring_right(text_len, suffix_n);

    ClozeSpans {
        prefix_start: prefix.0,
        prefix_len: prefix.1,
        body_start: body.0,
        body_len: body.1,
        suffix_start: suffix.0,
        suffix_len: suffix.1,
    }
}

fn qstring_left(text_len: u64, n: i64) -> (u64, u64) {
    if (n as usize) >= (text_len as usize) {
        (0, text_len)
    } else {
        (0, n as u64)
    }
}

fn qstring_right(text_len: u64, n: i64) -> (u64, u64) {
    if (n as usize) >= (text_len as usize) {
        (0, text_len)
    } else {
        (text_len - (n as u64), n as u64)
    }
}

fn qstring_mid(text_len: u64, mut pos: i64, mut len: i64) -> (u64, u64) {
    let orig = text_len as i64;
    if pos > orig {
        return (0, 0);
    }
    if pos < 0 {
        if len < 0 || len.saturating_add(pos) >= orig {
            return (0, text_len);
        }
        if len + pos <= 0 {
            return (0, 0);
        }
        len += pos;
        pos = 0;
    } else if (len as usize) > (orig - pos) as usize {
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

    items.sort_by(|a, b| {
        let prio_a = priority_map.get(&a.dict_id).copied().unwrap_or(missing_default);
        let prio_b = priority_map.get(&b.dict_id).copied().unwrap_or(missing_default);

        prio_a
            .cmp(&prio_b)
            .then_with(|| {
                if a.use_score_tiebreak {
                    b.score.cmp(&a.score)
                } else {
                    Ordering::Equal
                }
            })
            .then_with(|| a.original_index.cmp(&b.original_index))
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
        let deconj1 = [0x3042, 0x3044]; // あい
        let deconj2 = [0x3042];         // あ
        let deconj3 = [0x304a];         // お
        let empty = [];

        let q0 = CandidateSearchQuery {
            original_index: 0,
            deconj: &deconj2, // あ
            surface_len: 1,
            rule_mask: 1,
            source: QuerySource::Deconj,
        };
        let q1 = CandidateSearchQuery {
            original_index: 1,
            deconj: &deconj1, // あい
            surface_len: 2,
            rule_mask: 1,
            source: QuerySource::Deconj,
        };
        let q2 = CandidateSearchQuery {
            original_index: 2,
            deconj: &deconj1, // あい (duplicate)
            surface_len: 2,
            rule_mask: 1,
            source: QuerySource::Mecab,
        };
        let q3 = CandidateSearchQuery {
            original_index: 3,
            deconj: &deconj3, // お
            surface_len: 3,
            rule_mask: 2,
            source: QuerySource::Exact,
        };
        let q4 = CandidateSearchQuery {
            original_index: 4,
            deconj: &empty, // empty
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
}

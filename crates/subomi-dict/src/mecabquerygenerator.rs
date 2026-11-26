//! Pure UTF-8 MeCab lattice graph expansion and Yomitan `rule_mask` candidate generation.

use std::collections::HashSet;

use crate::deconjugationquerygenerator::{
    RULE_ADJ_I, RULE_V1, RULE_V5, RULE_VK, RULE_VS, RULE_VZ,
};

/// Borrowed morphological token view used to build a MeCab lattice DAG
/// and derive `rule_mask`-safe dictionary lookup candidates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TokenSpan<'a> {
    pub surface: &'a str,
    pub surface_clean: &'a str,
    pub lemma: &'a str,
    pub alt_lemma: Option<&'a str>,
    pub pos: &'a str,
    pub conj_type: &'a str,
}

/// A UTF-8 MeCab lookup candidate paired with its matched UTF-16 surface length
/// and Yomitan POS `rule_mask`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GeneratedMecabCandidate {
    pub deconj: String,
    pub surface: String,
    pub surface_clean: String,
    pub surface_u16_len: usize,
    pub rule_mask: u8,
    pub span_tokens: usize,
}

#[derive(Clone, Copy)]
struct LatticeNode<'a> {
    deconj: &'a str,
    deconj_u16_len: usize,
    surface: &'a str,
    surface_u16_len: usize,
    surface_clean: &'a str,
    surface_clean_u16_len: usize,
    next: Option<usize>,
    bnext: Option<usize>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum VisitState {
    Unvisited,
    Visiting,
    Done,
}

/// Maximum compound queries retained per lattice offset so pathological
/// distinct-surface diamond lattices remain fast and bounded in memory.
const MAX_QUERIES_PER_NODE: usize = 128;
/// Maximum UTF-16 length of a concatenated compound MeCab lookup candidate.
const MAX_COMPOUND_UTF16_UNITS: usize = 24;

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
struct MeCabQueryKey {
    deconj: String,
    surface: String,
    surface_clean: String,
}

#[derive(Clone)]
struct ExpandedQuery {
    key: MeCabQueryKey,
    deconj_u16_len: usize,
    surface_u16_len: usize,
    span_tokens: usize,
}

fn expand_memoized(
    nodes: &[LatticeNode<'_>],
    start_index: usize,
    state: &mut [VisitState],
    memo: &mut [Vec<ExpandedQuery>],
) -> Option<()> {
    if start_index >= nodes.len() {
        return None;
    }
    match state[start_index] {
        VisitState::Visiting => return None,
        VisitState::Done => return Some(()),
        VisitState::Unvisited => {}
    }

    // Walk the `bnext` sibling chain iteratively while marking every sibling `Visiting`
    // so both `bnext` cycles and `next` <-> `bnext` cycles are caught cleanly.
    let mut chain = Vec::new();
    let mut cursor = Some(start_index);
    while let Some(idx) = cursor {
        if idx >= nodes.len() {
            return None;
        }
        match state[idx] {
            VisitState::Visiting => return None,
            VisitState::Done => break,
            VisitState::Unvisited => {
                state[idx] = VisitState::Visiting;
                chain.push(idx);
                cursor = nodes[idx].bnext;
            }
        }
    }

    // Ensure all `next` targets from this `bnext` group are memoized first.
    for &idx in &chain {
        if let Some(next_index) = nodes[idx].next {
            expand_memoized(nodes, next_index, state, memo)?;
        }
    }

    // Populate `memo` from the tail of the `bnext` chain back to `start_index`.
    for &idx in chain.iter().rev() {
        let node = nodes[idx];
        let mut queries: Vec<ExpandedQuery> = Vec::new();

        if node.deconj != "*" {
            queries.push(ExpandedQuery {
                key: MeCabQueryKey {
                    deconj: node.deconj.to_owned(),
                    surface: node.surface.to_owned(),
                    surface_clean: node.surface_clean.to_owned(),
                },
                deconj_u16_len: node.deconj_u16_len,
                surface_u16_len: node.surface_u16_len,
                span_tokens: 1,
            });
        }

        if let Some(next_index) = node.next {
            for suffix in &memo[next_index] {
                if queries.len() >= MAX_QUERIES_PER_NODE {
                    break;
                }
                let combined_deconj_u16 = node.surface_clean_u16_len + suffix.deconj_u16_len;
                if combined_deconj_u16 > MAX_COMPOUND_UTF16_UNITS {
                    continue;
                }
                let mut deconj = String::with_capacity(node.surface_clean.len() + suffix.key.deconj.len());
                deconj.push_str(node.surface_clean);
                deconj.push_str(&suffix.key.deconj);
                let mut surface = String::with_capacity(node.surface.len() + suffix.key.surface.len());
                surface.push_str(node.surface);
                surface.push_str(&suffix.key.surface);
                let mut clean =
                    String::with_capacity(node.surface_clean.len() + suffix.key.surface_clean.len());
                clean.push_str(node.surface_clean);
                clean.push_str(&suffix.key.surface_clean);
                queries.push(ExpandedQuery {
                    key: MeCabQueryKey {
                        deconj,
                        surface,
                        surface_clean: clean,
                    },
                    deconj_u16_len: combined_deconj_u16,
                    surface_u16_len: node.surface_u16_len + suffix.surface_u16_len,
                    span_tokens: 1 + suffix.span_tokens,
                });
            }
        }

        if let Some(bnext_index) = node.bnext {
            let mut seen: HashSet<MeCabQueryKey> =
                queries.iter().map(|eq| eq.key.clone()).collect();
            for sibling in &memo[bnext_index] {
                if queries.len() >= MAX_QUERIES_PER_NODE {
                    break;
                }
                if seen.insert(sibling.key.clone()) {
                    queries.push(sibling.clone());
                }
            }
        }

        memo[idx] = queries;
        state[idx] = VisitState::Done;
    }

    Some(())
}

fn generate_queries_with_spans(
    nodes: &[LatticeNode<'_>],
    start: Option<usize>,
) -> Option<Vec<ExpandedQuery>> {
    let Some(start_index) = start else {
        return Some(Vec::new());
    };
    if start_index >= nodes.len() {
        return None;
    }
    let mut state = vec![VisitState::Unvisited; nodes.len()];
    let mut memo = vec![Vec::new(); nodes.len()];
    expand_memoized(nodes, start_index, &mut state, &mut memo)?;
    Some(std::mem::take(&mut memo[start_index]))
}

fn is_single_kana_only(s: &str) -> bool {
    let mut chars = s.chars();
    let Some(ch) = chars.next() else {
        return false;
    };
    if chars.next().is_some() {
        return false;
    }
    matches!(
        ch,
        '\u{3040}'..='\u{309F}' | '\u{30A0}'..='\u{30FF}' | '\u{FF65}'..='\u{FF9F}'
    )
}

/// Maps a MeCab `(pos, conj_type, base_form)` triple to the required Yomitan `rule_mask`
/// (`RULE_V5`, `RULE_V1`, `RULE_VK`, `RULE_VS`, `RULE_VZ`, `RULE_ADJ_I`), or `0` for non-inflecting POS.
#[must_use]
pub fn pos_rule_mask(pos: &str, conj_type: &str, base_form: &str) -> u8 {
    match pos {
        "形容詞" => RULE_ADJ_I,
        "動詞" => {
            if conj_type.starts_with("文語") {
                0
            } else if conj_type.contains("五段") {
                RULE_V5
            } else if conj_type.contains("一段") {
                if base_form.ends_with("ずる") {
                    RULE_V1 | RULE_VZ
                } else {
                    RULE_V1
                }
            } else if conj_type.contains("カ変") || conj_type.contains("カ行変格") {
                RULE_VK
            } else if conj_type.contains("サ変") || conj_type.contains("サ行変格") {
                if base_form.ends_with("ずる") || base_form.ends_with("ズル") {
                    RULE_VS | RULE_VZ
                } else {
                    RULE_VS
                }
            } else if conj_type.contains("ザ変") || conj_type.contains("ザ行変格") {
                RULE_VZ
            } else {
                RULE_V5 | RULE_V1 | RULE_VK | RULE_VS | RULE_VZ
            }
        }
        _ => 0,
    }
}

/// Builds a UTF-8 lattice DAG from sequential [`TokenSpan`] morphemes (using `bnext` siblings for
/// `orthBase` + `lemma` alternatives), expands compound/inflected spans without UTF-16 round-trips,
/// and enforces strict single-kana particle & `rule_mask` safety.
#[must_use]
pub fn generate_candidates_from_tokens(tokens: &[TokenSpan<'_>]) -> Vec<GeneratedMecabCandidate> {
    if tokens.is_empty() {
        return Vec::new();
    }

    let total_tokens = tokens.len();
    let mut stage_variants: Vec<(&str, Option<&str>)> = Vec::with_capacity(total_tokens);
    let mut stage_start_idx: Vec<usize> = Vec::with_capacity(total_tokens);
    let mut total_nodes = 0usize;

    for tok in tokens {
        let is_non_inflecting = matches!(tok.pos, "助詞" | "記号" | "補助記号" | "空白");
        let single_kana = is_single_kana_only(tok.surface_clean);
        let suppress_base_form = is_non_inflecting
            || (single_kana
                && (total_tokens == 1
                    || !matches!(tok.pos, "動詞" | "形容詞")
                    || tok.conj_type.starts_with("文語")));

        let (primary, secondary) = if suppress_base_form {
            let p = if tok.surface_clean.is_empty() {
                "*"
            } else {
                tok.surface_clean
            };
            (p, None)
        } else {
            let p = if tok.lemma.is_empty() { "*" } else { tok.lemma };
            let s = tok
                .alt_lemma
                .filter(|&alt| !alt.is_empty() && alt != "*" && alt != p);
            (p, s)
        };

        stage_start_idx.push(total_nodes);
        total_nodes += 1 + usize::from(secondary.is_some());
        stage_variants.push((primary, secondary));
    }

    let mut nodes: Vec<LatticeNode<'_>> = Vec::with_capacity(total_nodes);
    for (stage_idx, (tok, &(primary, secondary))) in
        tokens.iter().zip(stage_variants.iter()).enumerate()
    {
        let next = stage_start_idx.get(stage_idx + 1).copied();
        let base_node_idx = stage_start_idx[stage_idx];
        let surface_u16_len = tok.surface.encode_utf16().count();
        let surface_clean_u16_len = tok.surface_clean.encode_utf16().count();

        nodes.push(LatticeNode {
            deconj: primary,
            deconj_u16_len: primary.encode_utf16().count(),
            surface: tok.surface,
            surface_u16_len,
            surface_clean: tok.surface_clean,
            surface_clean_u16_len,
            next,
            bnext: secondary.map(|_| base_node_idx + 1),
        });
        if let Some(alt) = secondary {
            nodes.push(LatticeNode {
                deconj: alt,
                deconj_u16_len: alt.encode_utf16().count(),
                surface: tok.surface,
                surface_u16_len,
                surface_clean: tok.surface_clean,
                surface_clean_u16_len,
                next,
                bnext: None,
            });
        }
    }

    let Some(raw_queries) = generate_queries_with_spans(&nodes, Some(0)) else {
        return Vec::new();
    };

    let mut out = Vec::with_capacity(raw_queries.len());
    for ExpandedQuery {
        key,
        surface_u16_len,
        span_tokens,
        ..
    } in raw_queries
    {
        if key.deconj.is_empty() || key.surface.is_empty() {
            continue;
        }
        let rule_mask = if span_tokens == 1 && key.deconj != key.surface_clean {
            pos_rule_mask(tokens[0].pos, tokens[0].conj_type, &key.deconj)
        } else {
            0
        };

        out.push(GeneratedMecabCandidate {
            deconj: key.deconj,
            surface: key.surface,
            surface_clean: key.surface_clean,
            surface_u16_len,
            rule_mask,
            span_tokens,
        });
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node<'a>(
        deconj: &'a str,
        surface: &'a str,
        surface_clean: &'a str,
        next: Option<usize>,
        bnext: Option<usize>,
    ) -> LatticeNode<'a> {
        LatticeNode {
            deconj,
            deconj_u16_len: deconj.encode_utf16().count(),
            surface,
            surface_u16_len: surface.encode_utf16().count(),
            surface_clean,
            surface_clean_u16_len: surface_clean.encode_utf16().count(),
            next,
            bnext,
        }
    }

    #[test]
    fn keeps_alternatives_and_prepends_clean_and_raw_surfaces() {
        let nodes = [
            node("食", " 食", "食", Some(2), Some(1)),
            node("*", "別", "別", Some(2), None),
            node("べる", "べ", "べ", None, None),
        ];
        let rows = generate_queries_with_spans(&nodes, Some(0)).unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].key.deconj, "食");
        assert_eq!(rows[1].key.deconj, "食べる");
        assert_eq!(rows[1].key.surface, " 食べ");
        assert_eq!(rows[2].key.deconj, "別べる");
    }

    #[test]
    fn detects_cycles_safely() {
        let next_cycle = [
            node("食", "食", "食", Some(1), None),
            node("べ", "べ", "べ", Some(0), None),
        ];
        assert!(generate_queries_with_spans(&next_cycle, Some(0)).is_none());

        let bnext_cycle = [
            node("食", "食", "食", None, Some(1)),
            node("べ", "べ", "べ", None, Some(0)),
        ];
        assert!(generate_queries_with_spans(&bnext_cycle, Some(0)).is_none());
    }

    #[test]
    fn empty_or_missing_start_returns_empty() {
        assert!(generate_queries_with_spans(&[], None).unwrap().is_empty());
        assert!(generate_queries_with_spans(&[], Some(0)).is_none());
    }

    #[test]
    fn memoizes_deep_diamond_lattice_without_exponential_blowup() {
        // 64-stage reconvergent lattice (4^64 = 2^128 paths without memoization).
        let stages = 64usize;
        let mut nodes = Vec::with_capacity(stages * 2);
        for s in 0..stages {
            let next = (s + 1 < stages).then_some((s + 1) * 2);
            nodes.push(node("ある", "あ", "あ", next, Some(s * 2 + 1)));
            nodes.push(node("いる", "い", "い", next, None));
        }
        let started = std::time::Instant::now();
        let rows = generate_queries_with_spans(&nodes, Some(0)).expect("DAG should succeed");
        assert!(!rows.is_empty());
        let budget_ms = if std::env::var_os("CI").is_some() {
            2000
        } else {
            200
        };
        assert!(started.elapsed().as_millis() < budget_ms);
    }

    #[test]
    fn generates_compound_spans_with_alt_lemmas_and_blocks_single_kana_particles() {
        let tokens = [
            TokenSpan {
                surface: "食べ",
                surface_clean: "食べ",
                lemma: "食べる",
                alt_lemma: None,
                pos: "動詞",
                conj_type: "下一段-バ行",
            },
            TokenSpan {
                surface: "て",
                surface_clean: "て",
                lemma: "て",
                alt_lemma: None,
                pos: "助詞",
                conj_type: "",
            },
            TokenSpan {
                surface: "しまい",
                surface_clean: "しまい",
                lemma: "しまう",
                alt_lemma: Some("仕舞う"),
                pos: "動詞",
                conj_type: "五段-ワア行",
            },
        ];
        let cands = generate_candidates_from_tokens(&tokens);
        let keys: Vec<(&str, usize, u8)> = cands
            .iter()
            .map(|c| (c.deconj.as_str(), c.surface_u16_len, c.rule_mask))
            .collect();
        assert!(keys.contains(&("食べる", 2, RULE_V1)));
        assert!(keys.contains(&("食べてしまう", 6, 0)));
        assert!(keys.contains(&("食べて仕舞う", 6, 0)));

        // Single-kana particles and standalone single-kana tokens never emit bogus verb lemmas:
        for (surf, bogus_lemma, pos, ctype) in [
            ("は", "春", "助詞", ""),
            ("の", "乗る", "助詞", ""),
            ("と", "取る", "助詞", ""),
            ("と", "とる", "助動詞", "五段-ラ行"),
            ("く", "来る", "動詞", "カ行変格"),
        ] {
            let single = [TokenSpan {
                surface: surf,
                surface_clean: surf,
                lemma: bogus_lemma,
                alt_lemma: Some(bogus_lemma),
                pos,
                conj_type: ctype,
            }];
            let out = generate_candidates_from_tokens(&single);
            assert_eq!(out.len(), 1);
            assert_eq!(out[0].deconj, surf);
            assert_eq!(out[0].rule_mask, 0);
        }
    }
}

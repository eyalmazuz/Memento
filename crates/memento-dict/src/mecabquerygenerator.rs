//! MeCab lattice graph expansion from `mecabquerygenerator.cpp`.
//! Qt/MeCab retain tokenization, UTF-8 decoding and tagger lifetime; this
//! module owns the memoized DAG alternatives and query concatenation.

use std::collections::HashSet;

use crate::deconjugationquerygenerator::{
    RULE_ADJ_I, RULE_V1, RULE_V5, RULE_VK, RULE_VS, RULE_VZ,
};

#[derive(Clone, Copy)]
pub struct Node<'a> {
    pub deconj: &'a [u16],
    pub surface: &'a [u16],
    pub surface_clean: &'a [u16],
    pub next: Option<usize>,
    pub bnext: Option<usize>,
}

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct MeCabQuery {
    pub deconj: Vec<u16>,
    pub surface: Vec<u16>,
    pub surface_clean: Vec<u16>,
}

/// Borrowed morphological token view used to build a MeCab lattice [`Node`] DAG
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

/// A UTF-8 / UTF-16 MeCab lookup candidate paired with its matched surface length
/// and Yomitan POS `rule_mask`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GeneratedMecabCandidate {
    pub deconj: String,
    pub deconj_u16: Vec<u16>,
    pub surface: String,
    pub surface_clean: String,
    pub surface_u16_len: usize,
    pub rule_mask: u8,
    pub span_tokens: usize,
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

fn expand_memoized(
    nodes: &[Node<'_>],
    start_index: usize,
    state: &mut [VisitState],
    memo: &mut [Vec<MeCabQuery>],
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
        let mut queries = Vec::new();

        if node.deconj != ['*' as u16] {
            queries.push(MeCabQuery {
                deconj: node.deconj.to_vec(),
                surface: node.surface.to_vec(),
                surface_clean: node.surface_clean.to_vec(),
            });
        }

        // Skip expanding `next` if a later sibling in `bnext` already expanded the
        // exact same `(next_index, surface, surface_clean)` prefix, or deduplicate at end.
        if let Some(next_index) = node.next {
            for suffix in &memo[next_index] {
                if queries.len() >= MAX_QUERIES_PER_NODE {
                    break;
                }
                if node.surface_clean.len() + suffix.deconj.len() > MAX_COMPOUND_UTF16_UNITS {
                    continue;
                }
                let mut deconj =
                    Vec::with_capacity(node.surface_clean.len() + suffix.deconj.len());
                deconj.extend_from_slice(node.surface_clean);
                deconj.extend_from_slice(&suffix.deconj);
                let mut surface = Vec::with_capacity(node.surface.len() + suffix.surface.len());
                surface.extend_from_slice(node.surface);
                surface.extend_from_slice(&suffix.surface);
                let mut clean =
                    Vec::with_capacity(node.surface_clean.len() + suffix.surface_clean.len());
                clean.extend_from_slice(node.surface_clean);
                clean.extend_from_slice(&suffix.surface_clean);
                queries.push(MeCabQuery {
                    deconj,
                    surface,
                    surface_clean: clean,
                });
            }
        }

        if let Some(bnext_index) = node.bnext {
            let mut seen: HashSet<MeCabQuery> = queries.iter().cloned().collect();
            for sibling in &memo[bnext_index] {
                if queries.len() >= MAX_QUERIES_PER_NODE {
                    break;
                }
                if seen.insert(sibling.clone()) {
                    queries.push(sibling.clone());
                }
            }
        }

        memo[idx] = queries;
        state[idx] = VisitState::Done;
    }

    Some(())
}

/// Returns `None` if the caller supplied invalid indexes or cyclic links.
/// Real MeCab lattices are DAGs; memoization prevents exponential blowup on
/// reconvergent multi-morpheme lattices.
pub fn generate_queries(nodes: &[Node<'_>], start: Option<usize>) -> Option<Vec<MeCabQuery>> {
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

/// Builds a [`Node`] DAG from sequential [`TokenSpan`] morphemes (using `bnext` siblings for
/// `orthBase` + `lemma` alternatives), expands compound/inflected spans via [`generate_queries`],
/// and enforces strict single-kana particle & `rule_mask` safety.
#[must_use]
pub fn generate_candidates_from_tokens(tokens: &[TokenSpan<'_>]) -> Vec<GeneratedMecabCandidate> {
    if tokens.is_empty() {
        return Vec::new();
    }

    struct EncodedStage {
        surface_u16: Vec<u16>,
        surface_clean_u16: Vec<u16>,
        deconj_variants: Vec<Vec<u16>>,
    }

    let total_tokens = tokens.len();
    let mut stages: Vec<EncodedStage> = Vec::with_capacity(total_tokens);
    let mut cum_surface_u16: Vec<usize> = Vec::with_capacity(total_tokens);
    let mut running_u16 = 0usize;

    for tok in tokens {
        let surface_u16: Vec<u16> = tok.surface.encode_utf16().collect();
        let surface_clean_u16: Vec<u16> = tok.surface_clean.encode_utf16().collect();
        running_u16 += surface_u16.len();
        cum_surface_u16.push(running_u16);

        let is_non_inflecting = matches!(tok.pos, "助詞" | "記号" | "補助記号" | "空白");
        let single_kana = is_single_kana_only(tok.surface_clean);
        let suppress_base_form = is_non_inflecting
            || (single_kana
                && (total_tokens == 1
                    || !matches!(tok.pos, "動詞" | "形容詞")
                    || tok.conj_type.starts_with("文語")));

        let mut deconj_variants: Vec<Vec<u16>> = Vec::with_capacity(2);
        if suppress_base_form {
            if tok.surface_clean.is_empty() {
                deconj_variants.push(vec![b'*' as u16]);
            } else {
                deconj_variants.push(surface_clean_u16.clone());
            }
        } else {
            let primary = if tok.lemma.is_empty() {
                "*"
            } else {
                tok.lemma
            };
            deconj_variants.push(primary.encode_utf16().collect());
            if let Some(alt) = tok.alt_lemma {
                if !alt.is_empty() && alt != "*" && alt != primary {
                    deconj_variants.push(alt.encode_utf16().collect());
                }
            }
        }

        stages.push(EncodedStage {
            surface_u16,
            surface_clean_u16,
            deconj_variants,
        });
    }

    // Compute starting node index per token stage so `next` and `bnext` wire into a flat `Vec<Node>`.
    let mut stage_start_idx: Vec<usize> = Vec::with_capacity(total_tokens);
    let mut total_nodes = 0usize;
    for stage in &stages {
        stage_start_idx.push(total_nodes);
        total_nodes += stage.deconj_variants.len();
    }

    let mut nodes: Vec<Node<'_>> = Vec::with_capacity(total_nodes);
    for (stage_idx, stage) in stages.iter().enumerate() {
        let next = stage_start_idx.get(stage_idx + 1).copied();
        let base_node_idx = stage_start_idx[stage_idx];
        let variant_count = stage.deconj_variants.len();
        for (v_idx, deconj_u16) in stage.deconj_variants.iter().enumerate() {
            let bnext = (v_idx + 1 < variant_count).then_some(base_node_idx + v_idx + 1);
            nodes.push(Node {
                deconj: deconj_u16.as_slice(),
                surface: stage.surface_u16.as_slice(),
                surface_clean: stage.surface_clean_u16.as_slice(),
                next,
                bnext,
            });
        }
    }

    let Some(raw_queries) = generate_queries(&nodes, Some(0)) else {
        return Vec::new();
    };

    let mut out = Vec::with_capacity(raw_queries.len());
    for mq in raw_queries {
        if mq.deconj.is_empty() || mq.surface.is_empty() {
            continue;
        }
        let surface_u16_len = mq.surface.len();
        let span_tokens = cum_surface_u16
            .binary_search(&surface_u16_len)
            .map(|k| k + 1)
            .unwrap_or(2);
        let deconj = String::from_utf16_lossy(&mq.deconj);
        let surface = String::from_utf16_lossy(&mq.surface);
        let surface_clean = String::from_utf16_lossy(&mq.surface_clean);

        let rule_mask = if span_tokens == 1 && mq.deconj != mq.surface_clean {
            pos_rule_mask(tokens[0].pos, tokens[0].conj_type, &deconj)
        } else {
            0
        };

        out.push(GeneratedMecabCandidate {
            deconj,
            deconj_u16: mq.deconj,
            surface,
            surface_clean,
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

    #[test]
    fn keeps_alternatives_and_prepends_clean_and_raw_surfaces() {
        let nodes = [
            Node {
                deconj: &[0x98df],
                surface: &[0x20, 0x98df],
                surface_clean: &[0x98df],
                next: Some(2),
                bnext: Some(1),
            },
            Node {
                deconj: &['*' as u16],
                surface: &[0x5225],
                surface_clean: &[0x5225],
                next: Some(2),
                bnext: None,
            },
            Node {
                deconj: &[0x3079, 0x308b],
                surface: &[0x3079],
                surface_clean: &[0x3079],
                next: None,
                bnext: None,
            },
        ];
        let rows = generate_queries(&nodes, Some(0)).unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].deconj, vec![0x98df]);
        assert_eq!(rows[1].deconj, vec![0x98df, 0x3079, 0x308b]);
        assert_eq!(rows[1].surface, vec![0x20, 0x98df, 0x3079]);
        assert_eq!(rows[2].deconj, vec![0x5225, 0x3079, 0x308b]);
    }

    #[test]
    fn detects_cycles_safely() {
        let next_cycle = [
            Node {
                deconj: &[0x98df],
                surface: &[0x98df],
                surface_clean: &[0x98df],
                next: Some(1),
                bnext: None,
            },
            Node {
                deconj: &[0x3079],
                surface: &[0x3079],
                surface_clean: &[0x3079],
                next: Some(0),
                bnext: None,
            },
        ];
        assert!(generate_queries(&next_cycle, Some(0)).is_none());

        let bnext_cycle = [
            Node {
                deconj: &[0x98df],
                surface: &[0x98df],
                surface_clean: &[0x98df],
                next: None,
                bnext: Some(1),
            },
            Node {
                deconj: &[0x3079],
                surface: &[0x3079],
                surface_clean: &[0x3079],
                next: None,
                bnext: Some(0),
            },
        ];
        assert!(generate_queries(&bnext_cycle, Some(0)).is_none());
    }

    #[test]
    fn empty_or_missing_start_returns_empty() {
        assert_eq!(generate_queries(&[], None).unwrap(), vec![]);
        assert!(generate_queries(&[], Some(0)).is_none());
    }

    #[test]
    fn memoizes_deep_diamond_lattice_without_exponential_blowup() {
        // 64-stage reconvergent lattice (4^64 = 2^128 paths without memoization).
        let stages = 64usize;
        let surf_a = [0x3042u16]; // あ
        let surf_b = [0x3044u16]; // い
        let deconj_1 = [0x3042u16, 0x308bu16];
        let deconj_2 = [0x3044u16, 0x308bu16];
        let mut nodes = Vec::with_capacity(stages * 2);
        for s in 0..stages {
            let next = (s + 1 < stages).then_some((s + 1) * 2);
            nodes.push(Node {
                deconj: &deconj_1,
                surface: &surf_a,
                surface_clean: &surf_a,
                next,
                bnext: Some(s * 2 + 1),
            });
            nodes.push(Node {
                deconj: &deconj_2,
                surface: &surf_b,
                surface_clean: &surf_b,
                next,
                bnext: None,
            });
        }
        let started = std::time::Instant::now();
        let rows = generate_queries(&nodes, Some(0)).expect("DAG should succeed");
        assert!(!rows.is_empty());
        assert!(started.elapsed().as_millis() < 200);
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

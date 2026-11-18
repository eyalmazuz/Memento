//! MeCab lattice graph expansion from `mecabquerygenerator.cpp`.
//! Qt/MeCab retain tokenization, UTF-8 decoding and tagger lifetime; this
//! module owns the memoized DAG alternatives and query concatenation.

use std::collections::HashSet;

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
}

//! MeCab lattice graph expansion from `mecabquerygenerator.cpp`.
//! Qt/MeCab retain tokenization, UTF-8 decoding and tagger lifetime; this
//! module owns the recursive alternatives and query concatenation.

#[derive(Clone, Copy)]
pub struct Node<'a> {
    pub deconj: &'a [u16],
    pub surface: &'a [u16],
    pub surface_clean: &'a [u16],
    pub next: Option<usize>,
    pub bnext: Option<usize>,
}

#[derive(Debug, Eq, PartialEq)]
pub struct MeCabQuery {
    pub deconj: Vec<u16>,
    pub surface: Vec<u16>,
    pub surface_clean: Vec<u16>,
}

fn expand(nodes: &[Node<'_>], start: Option<usize>, depth: usize) -> Option<Vec<MeCabQuery>> {
    let mut queries = Vec::new();
    let mut current = start;
    let mut bnext_count = 0;
    while let Some(index) = current {
        if bnext_count >= nodes.len() || depth >= nodes.len() || index >= nodes.len() {
            return None;
        }
        bnext_count += 1;
        let node = nodes[index];
        if node.deconj != ['*' as u16] {
            queries.push(MeCabQuery {
                deconj: node.deconj.to_vec(),
                surface: node.surface.to_vec(),
                surface_clean: node.surface_clean.to_vec(),
            });
        }
        if node.next.is_some() {
            let suffixes = expand(nodes, node.next, depth + 1)?;
            for suffix in suffixes {
                let mut deconj = Vec::with_capacity(node.surface_clean.len() + suffix.deconj.len());
                deconj.extend_from_slice(node.surface_clean);
                deconj.extend_from_slice(&suffix.deconj);
                let mut surface = Vec::with_capacity(node.surface.len() + suffix.surface.len());
                surface.extend_from_slice(node.surface);
                surface.extend_from_slice(&suffix.surface);
                let mut clean = Vec::with_capacity(node.surface_clean.len() + suffix.surface_clean.len());
                clean.extend_from_slice(node.surface_clean);
                clean.extend_from_slice(&suffix.surface_clean);
                queries.push(MeCabQuery { deconj, surface, surface_clean: clean });
            }
        }
        current = node.bnext;
    }
    Some(queries)
}

/// Returns `None` if the caller supplied invalid indexes or cyclic links.
/// The C++ reference has no cycle protection; real MeCab lattices are DAGs.
pub fn generate_queries(nodes: &[Node<'_>], start: Option<usize>) -> Option<Vec<MeCabQuery>> {
    expand(nodes, start, 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_alternatives_and_prepends_clean_and_raw_surfaces() {
        let nodes = [
            Node { deconj: &[0x98df], surface: &[0x20, 0x98df], surface_clean: &[0x98df],
                   next: Some(2), bnext: Some(1) },
            Node { deconj: &['*' as u16], surface: &[0x5225], surface_clean: &[0x5225],
                   next: Some(2), bnext: None },
            Node { deconj: &[0x3079, 0x308b], surface: &[0x3079], surface_clean: &[0x3079],
                   next: None, bnext: None },
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
        // next cycle: 0 -> 1 -> 0
        let next_cycle = [
            Node { deconj: &[0x98df], surface: &[0x98df], surface_clean: &[0x98df],
                   next: Some(1), bnext: None },
            Node { deconj: &[0x3079], surface: &[0x3079], surface_clean: &[0x3079],
                   next: Some(0), bnext: None },
        ];
        assert!(generate_queries(&next_cycle, Some(0)).is_none());

        // bnext cycle: 0 <-> 1
        let bnext_cycle = [
            Node { deconj: &[0x98df], surface: &[0x98df], surface_clean: &[0x98df],
                   next: None, bnext: Some(1) },
            Node { deconj: &[0x3079], surface: &[0x3079], surface_clean: &[0x3079],
                   next: None, bnext: Some(0) },
        ];
        assert!(generate_queries(&bnext_cycle, Some(0)).is_none());
    }

    #[test]
    fn empty_or_missing_start_returns_empty() {
        assert_eq!(generate_queries(&[], None).unwrap(), vec![]);
        assert!(generate_queries(&[], Some(0)).is_none());
    }
}

//! Pure search-query deduplication and Yomitan rule bitmask helpers from
//! `deconjugationquerygenerator.cpp`.

use std::collections::HashMap;

use crate::deconjugator::{deconjugate_str, format_derivation_chain, ConjugationInfo, WordForm};

pub const RULE_V5: u8 = 1;
pub const RULE_V1: u8 = 2;
pub const RULE_VK: u8 = 4;
pub const RULE_VS: u8 = 8;
pub const RULE_ADJ_I: u8 = 16;
pub const RULE_EMPTY: u8 = 32;
pub const RULE_VZ: u8 = 64;

/// A dictionary search target; the bit mask contains every C++ QSet rule.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeconjugationQuery {
    pub deconj: Vec<u16>,
    pub rule_mask: u8,
    pub derivation_display: String,
    pub derivations: Vec<WordForm>,
    pub derivation: String,
    pub derivation_descriptions: Vec<(String, String)>,
    /// Per-rule-mask `(rule_bit, chain, descriptions)` variants merged into this query,
    /// allowing callers to pick the exact base verb/adjective label matching `entry_rule_mask`.
    pub rule_derivations: Vec<(u8, String, Vec<(String, String)>)>,
}

impl DeconjugationQuery {
    /// Selects the derivation chain and descriptions matching `entry_rule_mask`,
    /// falling back to the primary derivation when `entry_rule_mask == 0`.
    #[must_use]
    pub fn derivation_for_entry(&self, entry_rule_mask: u8) -> (&str, &[(String, String)]) {
        if entry_rule_mask != 0 {
            for (mask, chain, descs) in &self.rule_derivations {
                if (*mask & entry_rule_mask) != 0 {
                    return (chain.as_str(), descs.as_slice());
                }
            }
        }
        (self.derivation.as_str(), self.derivation_descriptions.as_slice())
    }
}

fn rule_for_info(info: &ConjugationInfo) -> u8 {
    match info.derivations.first().copied() {
        Some(WordForm::GodanVerb) => RULE_V5,
        Some(WordForm::IchidanVerb) => {
            if info.base.ends_with("ずる") {
                RULE_V1 | RULE_VZ
            } else {
                RULE_V1
            }
        }
        Some(WordForm::KuruVerb) => RULE_VK,
        Some(WordForm::SuruVerb) => RULE_VS,
        Some(WordForm::Adjective) => RULE_ADJ_I,
        Some(WordForm::ZuruVerb | WordForm::IrregularVerb) => RULE_VZ,
        _ => RULE_EMPTY,
    }
}

/// Parses a `term_bank.rules` whitespace-delimited string into a `u8` bitmask
/// matching Yomitan/Memento rule identifiers (`v1`, `v5`, `vk`, `vs`, `vz`, `adj-i`).
#[must_use]
pub fn parse_rule_mask(rules: &str) -> u8 {
    let mut mask = 0u8;
    for token in rules.split_whitespace() {
        match token {
            "v1" | "v1-s" => mask |= RULE_V1,
            "vk" => mask |= RULE_VK,
            "vz" => mask |= RULE_VZ,
            "adj-i" | "adj-ix" => mask |= RULE_ADJ_I,
            t if t == "v5" || t.starts_with("v5") => mask |= RULE_V5,
            t if t == "vs" || t.starts_with("vs-") => mask |= RULE_VS,
            _ => {}
        }
    }
    mask
}

/// Returns `true` when `entry_rule_mask` satisfies `query_rule_mask`.
/// When `query_rule_mask == 0` (exact query), any entry is accepted.
/// When `query_rule_mask != 0` (deconjugated query), `(entry_rule_mask & query_rule_mask) != 0` is required.
#[must_use]
pub const fn rule_matches_entry(query_rule_mask: u8, entry_rule_mask: u8) -> bool {
    query_rule_mask == 0 || (entry_rule_mask & query_rule_mask) != 0
}

pub fn generate_queries_from_infos(infos: Vec<ConjugationInfo>) -> Vec<DeconjugationQuery> {
    let mut result: Vec<DeconjugationQuery> = Vec::with_capacity(infos.len());
    let mut by_base: HashMap<String, Vec<usize>> = HashMap::new();
    for info in infos {
        let filter = rule_for_info(&info);
        let existing_idx = by_base.get(&info.base).and_then(|indices| {
            indices.iter().copied().find(|&idx| {
                let query = &result[idx];
                (query.rule_mask & filter != 0)
                    || query.derivation_display == info.derivation_display
            })
        });
        if let Some(idx) = existing_idx {
            let existing = &mut result[idx];
            existing.rule_mask |= filter;
            if !existing
                .rule_derivations
                .iter()
                .any(|(m, _, _)| (*m & filter) == filter)
            {
                let (derivation, derivation_descriptions) =
                    format_derivation_chain(&info.derivations);
                existing
                    .rule_derivations
                    .push((filter, derivation, derivation_descriptions));
            }
        } else {
            let (derivation, derivation_descriptions) =
                format_derivation_chain(&info.derivations);
            let rule_derivations =
                vec![(filter, derivation.clone(), derivation_descriptions.clone())];
            let idx = result.len();
            let deconj: Vec<u16> = info.base.encode_utf16().collect();
            result.push(DeconjugationQuery {
                deconj,
                rule_mask: filter,
                derivation_display: info.derivation_display,
                derivations: info.derivations,
                derivation,
                derivation_descriptions,
                rule_derivations,
            });
            by_base.entry(info.base).or_default().push(idx);
        }
    }
    result
}

/// Generate deconjugated query rows for a single UTF-8 prefix (non-sentence mode).
pub fn generate_queries_single_str(text: &str) -> Vec<DeconjugationQuery> {
    if text.is_empty() {
        return Vec::new();
    }
    generate_queries_from_infos(deconjugate_str(text, false))
}

/// Generate deconjugated query rows for a single exact UTF-16 prefix (non-sentence mode).
pub fn generate_queries_single(text: &[u16]) -> Vec<DeconjugationQuery> {
    if text.is_empty() {
        return Vec::new();
    }
    let decoded = String::from_utf16_lossy(text);
    generate_queries_from_infos(deconjugate_str(&decoded, false))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(base: &str, display: &str, form: WordForm) -> ConjugationInfo {
        ConjugationInfo {
            base: base.to_owned(),
            derivations: vec![form],
            derivation_display: display.to_owned(),
        }
    }

    #[test]
    fn merges_first_equal_rule_or_explanation_and_keeps_other_variants() {
        let result = generate_queries_from_infos(vec![
            info("a", "past", WordForm::GodanVerb),
            info("a", "past", WordForm::IchidanVerb),
            info("a", "different", WordForm::GodanVerb),
            info("a", "different", WordForm::Adjective),
            info("b", "past", WordForm::GodanVerb),
        ]);
        assert_eq!(result.len(), 3);
        assert_eq!(result[0].rule_mask, RULE_V5 | RULE_V1);
        assert_eq!(result[0].derivation_display, "past");
        assert_eq!(result[1].deconj, "a".encode_utf16().collect::<Vec<_>>());
        assert_eq!(result[1].rule_mask, RULE_ADJ_I);
        assert_eq!(result[1].derivation_display, "different");
        assert_eq!(result[2].deconj, "b".encode_utf16().collect::<Vec<_>>());
        assert_eq!(result[2].rule_mask, RULE_V5);
    }

    #[test]
    fn handles_empty_derivations_gracefully() {
        let mut empty_info = info("test", "none", WordForm::None);
        empty_info.derivations.clear();
        let result = generate_queries_from_infos(vec![empty_info]);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].rule_mask, RULE_EMPTY);
    }

    #[test]
    fn parses_rule_masks_and_enforces_bitmask_matching() {
        assert_eq!(parse_rule_mask(""), 0);
        assert_eq!(parse_rule_mask("v1"), RULE_V1);
        assert_eq!(parse_rule_mask("v5 v1"), RULE_V5 | RULE_V1);
        assert_eq!(
            parse_rule_mask("vk vs vz adj-i"),
            RULE_VK | RULE_VS | RULE_VZ | RULE_ADJ_I
        );

        assert!(rule_matches_entry(0, 0));
        assert!(rule_matches_entry(0, RULE_V5));
        assert!(!rule_matches_entry(RULE_V1, 0));
        assert!(!rule_matches_entry(RULE_V1, RULE_V5));
        assert!(rule_matches_entry(RULE_V1 | RULE_V5, RULE_V5));
    }

    #[test]
    fn zuru_verb_maps_to_rule_vz() {
        let queries = generate_queries_single_str("論ずれば");
        let target: Vec<u16> = "論ずる".encode_utf16().collect();
        let found = queries
            .iter()
            .find(|q| q.deconj == target)
            .expect("should deconjugate 論ずれば to 論ずる");
        assert_ne!(found.rule_mask & RULE_VZ, 0);
        assert!(rule_matches_entry(found.rule_mask, parse_rule_mask("vz")));
    }
}

//! Pure search-query deduplication from `deconjugationquerygenerator.cpp`.

use crate::deconjugator::{deconjugate, ConjugationInfo, WordForm};

const RULE_V5: u8 = 1;
const RULE_V1: u8 = 2;
const RULE_VK: u8 = 4;
const RULE_VS: u8 = 8;
const RULE_ADJ_I: u8 = 16;
const RULE_EMPTY: u8 = 32;

/// A dictionary search target; the bit mask contains every C++ QSet rule.
#[derive(Debug, Eq, PartialEq)]
pub struct DeconjugationQuery {
    pub deconj: Vec<u16>,
    pub surface: Vec<u16>,
    pub rule_mask: u8,
    pub derivation_display: Vec<u16>,
}

fn rule(form: WordForm) -> u8 {
    match form {
        WordForm::GodanVerb => RULE_V5,
        WordForm::IchidanVerb => RULE_V1,
        WordForm::KuruVerb => RULE_VK,
        WordForm::SuruVerb => RULE_VS,
        WordForm::Adjective => RULE_ADJ_I,
        _ => RULE_EMPTY,
    }
}

pub fn generate_queries_from_infos(infos: Vec<ConjugationInfo>) -> Vec<DeconjugationQuery> {
    let mut result: Vec<DeconjugationQuery> = Vec::new();
    for info in infos {
        let filter = info.derivations.first().copied().map_or(RULE_EMPTY, rule);
        if let Some(existing) = result.iter_mut().find(|query| {
            query.deconj == info.base &&
            ((query.rule_mask & filter != 0) ||
             query.derivation_display == info.derivation_display)
        }) {
            existing.rule_mask |= filter;
        } else {
            result.push(DeconjugationQuery {
                deconj: info.base,
                surface: info.conjugated,
                rule_mask: filter,
                derivation_display: info.derivation_display,
            });
        }
    }
    result
}

/// Generate deconjugated query rows from the complete UTF-16 input.
pub fn generate_queries(text: &[u16]) -> Vec<DeconjugationQuery> {
    if text.is_empty() { return Vec::new(); }
    generate_queries_from_infos(deconjugate(text, true))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(base: &str, display: &str, form: WordForm) -> ConjugationInfo {
        ConjugationInfo {
            base: base.encode_utf16().collect(),
            conjugated: "surface".encode_utf16().collect(),
            derivations: vec![form],
            derivation_display: display.encode_utf16().collect(),
        }
    }

    #[test]
    fn merges_first_equal_rule_or_explanation_and_keeps_other_variants() {
        // Mirrors the C++ `find_if` merge rule exactly
        // (`ruleFilter.contains(rule) || conjugationExplanation equal`):
        // 1. a/"past"/v5 → new row; 2. a/"past"/v1 → merges by explanation;
        // 3. a/"different"/v5 → merges by rule overlap (mask unchanged);
        // 4. a/"different"/adj-i → disjoint rule AND different explanation →
        //    NEW row; 5. b/"past"/v5 → different deconj → NEW row.
        let result = generate_queries_from_infos(vec![
            info("a", "past", WordForm::GodanVerb),
            info("a", "past", WordForm::IchidanVerb),
            info("a", "different", WordForm::GodanVerb),
            info("a", "different", WordForm::Adjective),
            info("b", "past", WordForm::GodanVerb),
        ]);
        assert_eq!(result.len(), 3);
        assert_eq!(result[0].rule_mask, RULE_V5 | RULE_V1);
        assert_eq!(result[0].derivation_display,
                   "past".encode_utf16().collect::<Vec<_>>());
        // A disjoint rule with a different explanation starts its own row —
        // matching the C++ loop, it is NOT folded into the first row.
        assert_eq!(result[1].deconj, "a".encode_utf16().collect::<Vec<_>>());
        assert_eq!(result[1].rule_mask, RULE_ADJ_I);
        assert_eq!(result[1].derivation_display,
                   "different".encode_utf16().collect::<Vec<_>>());
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
}


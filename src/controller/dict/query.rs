//! Query generation, normalization helpers, in-place hit ranking, and SQLite candidate lookup.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use memento_dict::deconjugationquerygenerator;
use memento_dict::exactquerygenerator;
use memento_dict::mecabquerygenerator;
use memento_dict::normalize;
use memento_dict::searchcontroller::{
    self, CandidateSearchQuery, QuerySource, TagSortItem, TextReplacementOptions,
};
use memento_dict::sqlite_db;
use memento_util::mecab::MecabLookupToken;

use super::db::{DictionaryMetadataSnapshot, ReadOnlyDictionaryDb};
use super::glossary::{
    cache_parsed_glossary, clean_raw_glossary, parse_glossary_entry_value, strip_glossary_markup,
};
use super::meta::select_term_frequencies;
use super::{ClozeSentence, DictControllerError, DictTag, SearchHit};

pub(super) type MecabTokenizerFn = fn(&str) -> Result<Vec<MecabLookupToken>, String>;

/// Reading normalization using full Unicode NFKC + katakana-to-hiragana folding.
pub fn normalize_reading(text: &str) -> String {
    normalize::normalize_reading_str(text)
}

/// Produces the full Unicode NFKC form of `text` (converting halfwidth katakana,
/// fullwidth ASCII/alphanumeric/punctuation, and combining voiced marks).
pub fn normalize_nfkc_query(text: &str) -> String {
    normalize::normalize_nfkc(text)
}

/// Generates prefix queries from text using `memento_dict::exactquerygenerator::generate_queries`.
pub fn generate_prefix_queries(text: &str) -> Vec<String> {
    let u16_text: Vec<u16> = text.encode_utf16().collect();
    let queries = exactquerygenerator::generate_queries(&u16_text);
    queries
        .into_iter()
        .map(|q| String::from_utf16_lossy(&q.deconj))
        .collect()
}

/// Formats tags by sorting them with `memento_dict::searchcontroller::sort_tags`
/// and joining their names with comma separation.
pub fn format_tags(tags: &[DictTag]) -> String {
    if tags.is_empty() {
        return String::new();
    }
    let items: Vec<TagSortItem> = tags
        .iter()
        .enumerate()
        .map(|(i, t)| TagSortItem {
            original_index: i,
            order: t.order,
            score: t.score,
        })
        .collect();
    let sorted_indices = searchcontroller::sort_tags(items);
    sorted_indices
        .into_iter()
        .map(|i| tags[i].name.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Formats tags with notes (e.g. "name: notes").
pub fn format_tags_with_notes(tags: &[DictTag]) -> String {
    if tags.is_empty() {
        return String::new();
    }
    let items: Vec<TagSortItem> = tags
        .iter()
        .enumerate()
        .map(|(i, t)| TagSortItem {
            original_index: i,
            order: t.order,
            score: t.score,
        })
        .collect();
    let sorted_indices = searchcontroller::sort_tags(items);
    sorted_indices
        .into_iter()
        .map(|i| {
            let t = &tags[i];
            if t.notes.is_empty() {
                t.name.clone()
            } else {
                format!("{}: {}", t.name, t.notes)
            }
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// Slices a sentence into cloze spans matching `memento_dict::searchcontroller::cloze_spans`.
pub fn format_cloze_spans(sentence: &str, index: i64, surface_len: u64) -> ClozeSentence {
    let u16_sentence: Vec<u16> = sentence.encode_utf16().collect();
    let text_len = u16_sentence.len() as u64;
    let spans = searchcontroller::cloze_spans(text_len, index, surface_len);

    let p_start = spans.prefix_start as usize;
    let p_end = (spans.prefix_start + spans.prefix_len) as usize;
    let b_start = spans.body_start as usize;
    let b_end = (spans.body_start + spans.body_len) as usize;
    let s_start = spans.suffix_start as usize;
    let s_end = (spans.suffix_start + spans.suffix_len) as usize;

    let prefix = u16_sentence
        .get(p_start..p_end)
        .map(String::from_utf16_lossy)
        .unwrap_or_default();
    let body = u16_sentence
        .get(b_start..b_end)
        .map(String::from_utf16_lossy)
        .unwrap_or_default();
    let suffix = u16_sentence
        .get(s_start..s_end)
        .map(String::from_utf16_lossy)
        .unwrap_or_default();

    ClozeSentence::new(prefix, body, suffix)
}

/// Formats a cloze sentence for a selected term in a sentence.
pub fn format_cloze_for_term(sentence: &str, term: &str) -> Option<ClozeSentence> {
    let u16_sentence: Vec<u16> = sentence.encode_utf16().collect();
    let u16_term: Vec<u16> = term.encode_utf16().collect();
    if u16_term.is_empty() {
        return None;
    }

    let pos = u16_sentence
        .windows(u16_term.len())
        .position(|w| w == u16_term.as_slice())?;

    Some(format_cloze_spans(
        sentence,
        pos as i64,
        u16_term.len() as u64,
    ))
}

/// Sorts search hits in-place: exact matches first, longest prefix matches,
/// `expression == cloze_body` ahead of reading-only matches, accumulated group score,
/// and individual score, applying the permutation with zero `SearchHit` clones.
pub fn rank_hits(query: &str, hits: &mut [SearchHit]) {
    if hits.len() <= 1 {
        return;
    }
    let trimmed = query.trim();
    let norm_query = normalize_reading(trimmed);
    let u16_query: Vec<u16> = trimmed.encode_utf16().collect();

    let mut group_scores: HashMap<(&str, &str, bool), i64> = HashMap::with_capacity(hits.len());
    for h in hits.iter() {
        let key = (h.expression.as_str(), h.reading.as_str(), h.is_kanji);
        let entry = group_scores.entry(key).or_insert(0);
        *entry = entry.saturating_add(h.score);
    }

    let mut indexed: Vec<(usize, bool, bool, usize, bool, i64, i64)> = hits
        .iter()
        .enumerate()
        .map(|(i, h)| {
            let norm_reading = normalize_reading(&h.reading);
            let normalized_expression = normalize_reading(&h.expression);
            let is_exact = h.expression == trimmed
                || normalized_expression == norm_query
                || (!norm_reading.is_empty() && norm_reading == norm_query);

            let u16_expr: Vec<u16> = h.expression.encode_utf16().collect();
            let (prefix_len, expr_equals_cloze) = if u16_query.starts_with(&u16_expr) {
                (u16_expr.len(), true)
            } else if u16_expr.starts_with(&u16_query) {
                (u16_query.len(), h.expression == trimmed)
            } else {
                let u16_read: Vec<u16> = h.reading.encode_utf16().collect();
                if u16_query.starts_with(&u16_read) {
                    (u16_read.len(), false)
                } else if u16_read.starts_with(&u16_query) {
                    (u16_query.len(), false)
                } else {
                    (0, h.expression == trimmed)
                }
            };

            let group_score = group_scores
                .get(&(h.expression.as_str(), h.reading.as_str(), h.is_kanji))
                .copied()
                .unwrap_or(h.score);

            (
                i,
                h.is_kanji,
                is_exact,
                prefix_len,
                expr_equals_cloze,
                group_score,
                h.score,
            )
        })
        .collect();

    indexed.sort_by(|a, b| {
        a.1.cmp(&b.1)
            .then_with(|| b.2.cmp(&a.2))
            .then_with(|| b.3.cmp(&a.3))
            .then_with(|| b.4.cmp(&a.4))
            .then_with(|| b.5.cmp(&a.5))
            .then_with(|| b.6.cmp(&a.6))
            .then_with(|| a.0.cmp(&b.0))
    });

    // Apply permutation in-place via cycle decomposition (zero SearchHit clones).
    let mut perm: Vec<usize> = indexed
        .into_iter()
        .map(|(orig_idx, ..)| orig_idx)
        .collect();
    for i in 0..perm.len() {
        let mut cur = i;
        while perm[cur] != i {
            let next = perm[cur];
            hits.swap(cur, next);
            perm[cur] = cur;
            cur = next;
        }
        perm[cur] = cur;
    }
}

/// One candidate lookup key together with its matched UTF-16 surface length,
/// required Yomitan `rule_mask` (`0` for exact uninflected queries), and derivation chain.
pub(super) struct LookupQuery {
    pub key: String,
    pub deconj_u16: Vec<u16>,
    pub surface_len: usize,
    pub rule_mask: u8,
    pub source: QuerySource,
    pub derivation: String,
    pub derivation_descriptions: Vec<(String, String)>,
    pub rule_derivations: Vec<(u8, String, Vec<(String, String)>)>,
    pub seq: usize,
}

impl LookupQuery {
    pub(super) fn derivation_for_entry(
        &self,
        entry_rule_mask: u8,
    ) -> (String, Vec<(String, String)>) {
        if entry_rule_mask != 0 {
            for (mask, chain, descs) in &self.rule_derivations {
                if (*mask & entry_rule_mask) != 0 {
                    return (chain.clone(), descs.clone());
                }
            }
        }
        (self.derivation.clone(), self.derivation_descriptions.clone())
    }
}

pub(super) struct DatabaseCandidate {
    pub hit: SearchHit,
    pub parsed_glossary: Option<Arc<serde_json::Value>>,
    pub is_exact: bool,
    pub prefix_len: usize,
    pub dic_id: i64,
}

const MAX_DECONJ_PREFIX_CHARS: usize = 36;
const MAX_MECAB_LOOKUP_CHARS: usize = 32;

fn is_ascii_regex_ws(ch: char) -> bool {
    matches!(ch, '\t'..='\r' | ' ')
}

fn slice_mecab_lookup_prefix(trimmed: &str) -> &str {
    let mut end_byte = trimmed.len();
    for (char_idx, (byte_idx, ch)) in trimmed.char_indices().enumerate() {
        if char_idx >= MAX_MECAB_LOOKUP_CHARS {
            end_byte = byte_idx;
            break;
        }
        if char_idx > 0
            && matches!(
                ch,
                '\r' | '\n' | '。' | '、' | '！' | '？' | '「' | '」' | '（' | '）'
            )
        {
            end_byte = byte_idx;
            break;
        }
    }
    &trimmed[..end_byte]
}

#[allow(dead_code)]
pub(super) fn build_lookup_queries(
    trimmed: &str,
    exact_enabled: bool,
    deconj_enabled: bool,
    mecab_enabled: bool,
    mecab_tokenizer: MecabTokenizerFn,
) -> Vec<LookupQuery> {
    build_lookup_queries_with_replacements(
        trimmed,
        exact_enabled,
        deconj_enabled,
        mecab_enabled,
        mecab_tokenizer,
        &TextReplacementOptions::default(),
    )
}

pub(super) fn build_lookup_queries_with_replacements(
    trimmed: &str,
    exact_enabled: bool,
    deconj_enabled: bool,
    mecab_enabled: bool,
    mecab_tokenizer: MecabTokenizerFn,
    text_replacements: &TextReplacementOptions,
) -> Vec<LookupQuery> {
    let mut raw_lookups: Vec<LookupQuery> = Vec::new();
    let mut seq = 0usize;
    let full_orig_u16_len = trimmed.encode_utf16().count();

    let has_active_replacements = text_replacements
        .rules
        .iter()
        .any(|r| r.enabled && !r.pattern.is_empty());
    let seeds: Vec<(String, bool)> = if has_active_replacements {
        searchcontroller::apply_text_replacements(trimmed, text_replacements)
            .into_iter()
            .map(|s| {
                let is_orig = s == trimmed;
                (s, is_orig)
            })
            .collect()
    } else {
        vec![(trimmed.to_owned(), true)]
    };

    if seeds.is_empty() {
        return raw_lookups;
    }

    if exact_enabled || deconj_enabled {
        for (seed, is_original) in &seeds {
            let mut prefixes: Vec<(usize, usize, usize)> = Vec::new();
            let mut char_count = 0usize;
            let mut u16_count = 0usize;
            for (byte_idx, ch) in seed.char_indices() {
                char_count += 1;
                u16_count += ch.len_utf16();
                prefixes.push((byte_idx + ch.len_utf8(), char_count, u16_count));
            }

            for &(byte_end, chars_len, seed_u16_len) in prefixes.iter().rev() {
                let prefix = &seed[..byte_end];
                if prefix.trim().is_empty() {
                    continue;
                }
                let orig_u16_len = if *is_original {
                    seed_u16_len
                } else if byte_end == seed.len() {
                    full_orig_u16_len
                } else {
                    seed_u16_len.min(full_orig_u16_len)
                };

                if exact_enabled {
                    for variant in normalize::expand_query_variants(prefix) {
                        let deconj_u16 = if mecab_enabled {
                            variant.encode_utf16().collect()
                        } else {
                            Vec::new()
                        };
                        raw_lookups.push(LookupQuery {
                            key: variant,
                            deconj_u16,
                            surface_len: orig_u16_len,
                            rule_mask: 0,
                            source: QuerySource::Exact,
                            derivation: String::new(),
                            derivation_descriptions: Vec::new(),
                            rule_derivations: Vec::new(),
                            seq,
                        });
                        seq += 1;
                    }

                    if prefix.chars().any(is_ascii_regex_ws) {
                        let no_ws: String =
                            prefix.chars().filter(|&c| !is_ascii_regex_ws(c)).collect();
                        for variant in normalize::expand_query_variants(&no_ws) {
                            let deconj_u16 = if mecab_enabled {
                                variant.encode_utf16().collect()
                            } else {
                                Vec::new()
                            };
                            raw_lookups.push(LookupQuery {
                                key: variant,
                                deconj_u16,
                                surface_len: orig_u16_len,
                                rule_mask: 0,
                                source: QuerySource::Exact,
                                derivation: String::new(),
                                derivation_descriptions: Vec::new(),
                                rule_derivations: Vec::new(),
                                seq,
                            });
                            seq += 1;
                        }
                    }
                }

                if deconj_enabled && (chars_len <= MAX_DECONJ_PREFIX_CHARS || byte_end == seed.len())
                {
                    let mut deconj_inputs: Vec<String> = Vec::new();
                    let push_deconj_in = |inputs: &mut Vec<String>, s: String| {
                        if !s.is_empty() && !inputs.contains(&s) {
                            inputs.push(s);
                        }
                    };
                    let nfkc = normalize::normalize_nfkc(prefix);
                    push_deconj_in(&mut deconj_inputs, prefix.to_owned());
                    push_deconj_in(&mut deconj_inputs, nfkc.clone());
                    push_deconj_in(&mut deconj_inputs, normalize::kata_to_hira_str(&nfkc));

                    // Wire Wave 1 Japanese text preprocessors into deconjugation input generation:
                    // radical normalization, kyūjitai -> shinjitai standardization, emphatic
                    // sequence collapsing (preserving Yomitan #1411 guard), alphanumeric width
                    // variants, and romaji -> hiragana conversion.
                    let rad = normalize::normalize_radical_characters(&nfkc);
                    if rad != nfkc {
                        push_deconj_in(&mut deconj_inputs, rad.clone());
                    }
                    let std_kanji = normalize::standardize_kanji(&rad);
                    if std_kanji != rad {
                        push_deconj_in(&mut deconj_inputs, std_kanji.clone());
                    }
                    if nfkc.chars().any(|c| c.is_ascii_alphabetic()) {
                        let romaji_hira = normalize::alphabetic_to_hiragana(&nfkc);
                        if romaji_hira != nfkc {
                            push_deconj_in(&mut deconj_inputs, romaji_hira);
                        }
                    }
                    for width_var in normalize::alphanumeric_width_variants(&nfkc) {
                        if width_var != nfkc {
                            push_deconj_in(&mut deconj_inputs, width_var);
                        }
                    }
                    let pre_emph_len = deconj_inputs.len();
                    for idx in 0..pre_emph_len {
                        let base_in = deconj_inputs[idx].clone();
                        if base_in.chars().any(|c| matches!(c, 'っ' | 'ッ' | 'ー')) {
                            for emph in normalize::collapse_emphatic_sequences_variants(&base_in) {
                                push_deconj_in(&mut deconj_inputs, emph.text);
                            }
                        }
                    }
                    for iter_var in normalize::expand_iteration_marks(&std_kanji) {
                        push_deconj_in(&mut deconj_inputs, iter_var.clone());
                        if iter_var.contains('ー') {
                            for pv in normalize::expand_prolonged_vowels(&iter_var) {
                                push_deconj_in(&mut deconj_inputs, pv);
                            }
                        }
                    }

                    for deconj_input in deconj_inputs {
                        let input_u16: Vec<u16> = deconj_input.encode_utf16().collect();
                        for dq in deconjugationquerygenerator::generate_queries_single(&input_u16) {
                            let base = String::from_utf16_lossy(&dq.deconj);
                            for base_variant in normalize::expand_query_variants(&base) {
                                let deconj_u16 = if mecab_enabled {
                                    base_variant.encode_utf16().collect()
                                } else {
                                    Vec::new()
                                };
                                raw_lookups.push(LookupQuery {
                                    key: base_variant,
                                    deconj_u16,
                                    surface_len: orig_u16_len,
                                    rule_mask: dq.rule_mask,
                                    source: QuerySource::Deconj,
                                    derivation: dq.derivation.clone(),
                                    derivation_descriptions: dq.derivation_descriptions.clone(),
                                    rule_derivations: dq.rule_derivations.clone(),
                                    seq,
                                });
                                seq += 1;
                            }
                        }
                    }
                }
            }
        }
    }

    if mecab_enabled {
        let mut distinct_mecab_fallbacks: Vec<LookupQuery> = Vec::new();
        let mut ran_mecab = false;
        for (seed, is_original) in &seeds {
            let mecab_prefix = slice_mecab_lookup_prefix(seed);
            if let Ok(tokens) = (mecab_tokenizer)(mecab_prefix) {
                if !tokens.is_empty() {
                    ran_mecab = true;
                    let spans: Vec<mecabquerygenerator::TokenSpan<'_>> = tokens
                        .iter()
                        .map(|t| mecabquerygenerator::TokenSpan {
                            surface: t.surface.as_str(),
                            surface_clean: t.surface_clean.as_str(),
                            lemma: t.lemma.as_str(),
                            alt_lemma: t.alt_lemma.as_deref(),
                            pos: t.pos.as_str(),
                            conj_type: t.conj_type.as_str(),
                        })
                        .collect();

                    for cand in mecabquerygenerator::generate_candidates_from_tokens(&spans) {
                        let surface_u16 = if *is_original {
                            cand.surface_u16_len
                        } else {
                            cand.surface_u16_len.min(full_orig_u16_len)
                        };
                        for variant in normalize::expand_query_variants(&cand.deconj) {
                            let deconj_u16: Vec<u16> = variant.encode_utf16().collect();
                            let covered_by_deconj = raw_lookups.iter().any(|d| {
                                d.source == QuerySource::Deconj
                                    && d.key == variant
                                    && d.surface_len >= surface_u16
                                    && cand.rule_mask != 0
                                    && (d.rule_mask & cand.rule_mask) == cand.rule_mask
                            });
                            let lq = LookupQuery {
                                key: variant.clone(),
                                deconj_u16: deconj_u16.clone(),
                                surface_len: surface_u16,
                                rule_mask: cand.rule_mask,
                                source: QuerySource::Mecab,
                                derivation: String::new(),
                                derivation_descriptions: Vec::new(),
                                rule_derivations: Vec::new(),
                                seq,
                            };
                            if !covered_by_deconj
                                && ((cand.span_tokens >= 2 && cand.rule_mask == 0)
                                    || cand.rule_mask != 0)
                            {
                                distinct_mecab_fallbacks.push(LookupQuery {
                                    key: variant,
                                    deconj_u16,
                                    surface_len: surface_u16,
                                    rule_mask: cand.rule_mask,
                                    source: QuerySource::Mecab,
                                    derivation: String::new(),
                                    derivation_descriptions: Vec::new(),
                                    rule_derivations: Vec::new(),
                                    seq,
                                });
                            }
                            raw_lookups.push(lq);
                            seq += 1;
                        }
                    }
                }
            }
        }
        if ran_mecab {
            let descriptors: Vec<CandidateSearchQuery<'_>> = raw_lookups
                .iter()
                .enumerate()
                .map(|(idx, lq)| CandidateSearchQuery {
                    original_index: idx,
                    deconj: lq.deconj_u16.as_slice(),
                    surface_len: lq.surface_len as u64,
                    rule_mask: lq.rule_mask,
                    source: lq.source,
                })
                .collect();
            let surviving = searchcontroller::sort_and_filter_queries(descriptors, true);
            let mut filtered: Vec<LookupQuery> =
                Vec::with_capacity(surviving.len() + distinct_mecab_fallbacks.len());
            let mut opts: Vec<Option<LookupQuery>> = raw_lookups.into_iter().map(Some).collect();
            for idx in surviving {
                if let Some(lq) = opts.get_mut(idx).and_then(Option::take) {
                    filtered.push(lq);
                }
            }
            filtered.extend(distinct_mecab_fallbacks);
            raw_lookups = filtered;
        }
    }

    raw_lookups.sort_by(|a, b| {
        b.surface_len
            .cmp(&a.surface_len)
            .then_with(|| a.source.to_u8().cmp(&b.source.to_u8()))
            .then_with(|| (a.rule_mask != 0).cmp(&(b.rule_mask != 0)))
            .then_with(|| a.seq.cmp(&b.seq))
    });

    let mut seen_lookups: HashSet<(String, u8)> = HashSet::with_capacity(raw_lookups.len());
    raw_lookups.retain(|lookup| seen_lookups.insert((lookup.key.clone(), lookup.rule_mask)));
    raw_lookups
}

/// Wildcard glob matcher (`*`/`＊` = zero or more chars, `?`/`？` = single char) with
/// ASCII case-insensitivity and katakana-to-hiragana fallback for in-memory dictionaries.
pub(super) fn wildcard_pattern_matches(pattern: &str, candidate: &str) -> bool {
    fn glob_chars(pat: &[char], text: &[char]) -> bool {
        let (mut pi, mut ti) = (0usize, 0usize);
        let mut star_pi: Option<usize> = None;
        let mut star_ti = 0usize;
        while ti < text.len() {
            if pi < pat.len() && matches!(pat[pi], '*' | '＊') {
                star_pi = Some(pi);
                star_ti = ti;
                pi += 1;
            } else if pi < pat.len()
                && (matches!(pat[pi], '?' | '？')
                    || pat[pi] == text[ti]
                    || (pat[pi].is_ascii()
                        && text[ti].is_ascii()
                        && pat[pi].eq_ignore_ascii_case(&text[ti])))
            {
                pi += 1;
                ti += 1;
            } else if let Some(sp) = star_pi {
                star_ti += 1;
                ti = star_ti;
                pi = sp + 1;
            } else {
                return false;
            }
        }
        while pi < pat.len() && matches!(pat[pi], '*' | '＊') {
            pi += 1;
        }
        pi == pat.len()
    }

    if candidate.is_empty() {
        return false;
    }
    let pat_chars: Vec<char> = pattern.chars().collect();
    let cand_chars: Vec<char> = candidate.chars().collect();
    if glob_chars(&pat_chars, &cand_chars) {
        return true;
    }
    let norm_pat: Vec<char> = normalize::kata_to_hira_str(pattern).chars().collect();
    let norm_cand: Vec<char> = normalize::kata_to_hira_str(candidate).chars().collect();
    glob_chars(&norm_pat, &norm_cand)
}

/// Executes a bounded wildcard SQLite query (`*`/`?`/`＊`/`？`) via Wave 1's
/// `memento_dict::sqlite_db::search_terms_wildcard`.
pub(super) fn database_wildcard_candidates(
    database: &ReadOnlyDictionaryDb,
    snapshot: &DictionaryMetadataSnapshot,
    query: &str,
    full_u16_len: usize,
    limit: usize,
) -> Result<Vec<DatabaseCandidate>, DictControllerError> {
    let batch_rows = database
        .with_conn(|conn| sqlite_db::search_terms_wildcard(conn, query, limit))
        .map_err(DictControllerError::Sqlite)?;
    if batch_rows.is_empty() {
        return Ok(Vec::new());
    }

    let mut results = Vec::with_capacity(batch_rows.len());
    let mut seen_rows: HashSet<(i64, i64)> = HashSet::with_capacity(batch_rows.len());
    for row in batch_rows {
        let Some(dictionary_name) = snapshot.names.get(&row.dic_id) else {
            continue;
        };
        if !seen_rows.insert((row.dic_id, row.rowid)) {
            continue;
        }
        let Some((cleaned_raw, parsed_val)) = clean_raw_glossary(&row.glossary) else {
            continue;
        };
        let (glossary, example_ja, example_en) = match parsed_val.as_deref() {
            Some(value) => parse_glossary_entry_value(value),
            None => (
                strip_glossary_markup(&cleaned_raw),
                String::new(),
                String::new(),
            ),
        };
        if let Some(ref arc_val) = parsed_val {
            cache_parsed_glossary(&cleaned_raw, Arc::clone(arc_val));
        }
        let formatted_tags =
            format_dictionary_tags(&snapshot.tags, row.dic_id, &row.def_tags, &row.term_tags);
        results.push(DatabaseCandidate {
            hit: SearchHit {
                expression: row.expression,
                reading: row.reading,
                glossary,
                tags: formatted_tags,
                score: row.score,
                is_kanji: false,
                notes: String::new(),
                example_ja,
                example_en,
                dict_name: dictionary_name.clone(),
                rich_glossary: Vec::new(),
                derivation: String::new(),
                derivation_descriptions: Vec::new(),
                dictionary_id: row.dic_id,
                rules: row.rules,
                raw_glossary: cleaned_raw,
                term_tags: row.term_tags,
                def_tags: row.def_tags,
            },
            parsed_glossary: parsed_val,
            is_exact: true,
            prefix_len: full_u16_len,
            dic_id: row.dic_id,
        });
    }
    Ok(results)
}

pub(super) fn database_search_candidates(
    database: &ReadOnlyDictionaryDb,
    snapshot: &DictionaryMetadataSnapshot,
    query: &str,
    normalized_query: &str,
    full_u16_len: usize,
    lookups: &[LookupQuery],
) -> Result<Vec<DatabaseCandidate>, DictControllerError> {
    let mut unique_keys: Vec<String> = Vec::with_capacity(lookups.len());
    let mut seen_keys: HashSet<&str> = HashSet::with_capacity(lookups.len());
    for lookup in lookups {
        if !lookup.key.is_empty() && seen_keys.insert(lookup.key.as_str()) {
            unique_keys.push(lookup.key.clone());
        }
    }
    if unique_keys.is_empty() {
        return Ok(Vec::new());
    }

    let batch_rows = database
        .with_conn(|conn| sqlite_db::query_term_details_batch_conn(conn, &unique_keys))
        .map_err(DictControllerError::Sqlite)?;
    if batch_rows.is_empty() {
        return Ok(Vec::new());
    }

    let row_masks: Vec<u8> = batch_rows
        .iter()
        .map(|row| deconjugationquerygenerator::parse_rule_mask(&row.rules))
        .collect();
    let mut by_key: HashMap<&str, Vec<usize>> = HashMap::with_capacity(batch_rows.len() * 2);
    for (idx, row) in batch_rows.iter().enumerate() {
        by_key.entry(row.expression.as_str()).or_default().push(idx);
        if !row.reading.is_empty() && row.reading != row.expression {
            by_key.entry(row.reading.as_str()).or_default().push(idx);
        }
    }

    // Deduplicate `(dic_id, rowid)` BEFORE JSON parsing or cloning, outside any mutex lock.
    let mut results = Vec::new();
    let mut seen_rows: HashSet<(i64, i64)> = HashSet::with_capacity(batch_rows.len());

    for lookup in lookups {
        let Some(row_indices) = by_key.get(lookup.key.as_str()) else {
            continue;
        };
        for &row_idx in row_indices {
            let entry_mask = row_masks[row_idx];
            if !deconjugationquerygenerator::rule_matches_entry(lookup.rule_mask, entry_mask) {
                continue;
            }
            let row = &batch_rows[row_idx];
            let Some(dictionary_name) = snapshot.names.get(&row.dic_id) else {
                continue;
            };
            if !seen_rows.insert((row.dic_id, row.rowid)) {
                continue;
            }

            let Some((cleaned_raw, parsed_val)) = clean_raw_glossary(&row.glossary) else {
                continue;
            };
            let (glossary, example_ja, example_en) = match parsed_val.as_deref() {
                Some(value) => parse_glossary_entry_value(value),
                None => (
                    strip_glossary_markup(&cleaned_raw),
                    String::new(),
                    String::new(),
                ),
            };
            if let Some(ref arc_val) = parsed_val {
                cache_parsed_glossary(&cleaned_raw, Arc::clone(arc_val));
            }

            let norm_expression = normalize_reading(&row.expression);
            let norm_reading = normalize_reading(&row.reading);
            let formatted_tags =
                format_dictionary_tags(&snapshot.tags, row.dic_id, &row.def_tags, &row.term_tags);
            let is_exact = (lookup.source == QuerySource::Exact
                && lookup.rule_mask == 0
                && lookup.surface_len == full_u16_len)
                || (lookup.rule_mask == 0
                    && (row.expression == query
                        || norm_expression == normalized_query
                        || (!norm_reading.is_empty() && norm_reading == normalized_query)));

            let (derivation, derivation_descriptions) = if lookup.rule_mask != 0 {
                lookup.derivation_for_entry(entry_mask)
            } else {
                (String::new(), Vec::new())
            };

            results.push(DatabaseCandidate {
                hit: SearchHit {
                    expression: row.expression.clone(),
                    reading: row.reading.clone(),
                    glossary,
                    tags: formatted_tags,
                    score: row.score,
                    is_kanji: false,
                    notes: String::new(),
                    example_ja,
                    example_en,
                    dict_name: dictionary_name.clone(),
                    rich_glossary: Vec::new(),
                    derivation,
                    derivation_descriptions,
                    dictionary_id: row.dic_id,
                    rules: row.rules.clone(),
                    raw_glossary: cleaned_raw,
                    term_tags: row.term_tags.clone(),
                    def_tags: row.def_tags.clone(),
                },
                parsed_glossary: parsed_val,
                is_exact,
                prefix_len: lookup.surface_len,
                dic_id: row.dic_id,
            });
        }
    }

    Ok(results)
}

pub(super) fn database_kanji_candidates(
    database: &ReadOnlyDictionaryDb,
    snapshot: &DictionaryMetadataSnapshot,
    query: &str,
    prefix_len: usize,
    dictionary_order: &HashMap<i64, i64>,
) -> Result<Vec<DatabaseCandidate>, DictControllerError> {
    let (kanji_rows, kanji_meta_rows) = database.with_conn(|conn| {
        let rows =
            sqlite_db::query_kanji_utf8_conn(conn, query).map_err(DictControllerError::Sqlite)?;
        let meta = if rows.is_empty() {
            Vec::new()
        } else {
            sqlite_db::query_kanji_meta_freq_utf8_conn(conn, query).unwrap_or_default()
        };
        Ok::<_, DictControllerError>((rows, meta))
    })?;
    if kanji_rows.is_empty() {
        return Ok(Vec::new());
    }

    let kanji_freqs =
        select_term_frequencies(snapshot, dictionary_order, &kanji_meta_rows, query, "");
    let freq_notes: String = kanji_freqs
        .iter()
        .filter(|item| !item.parsed.display.is_empty())
        .map(|item| format!("{}: {}", item.dictionary, item.parsed.display))
        .collect::<Vec<_>>()
        .join("; ");

    let mut results = Vec::with_capacity(kanji_rows.len());
    for row in kanji_rows {
        let Some(dictionary_name) = snapshot.names.get(&row.dic_id) else {
            continue;
        };
        let glossary = row.meanings.join("\n");
        let stats_notes = format_kanji_stats(&snapshot.tags, row.dic_id, &row.stats);
        let notes = match (stats_notes.is_empty(), freq_notes.is_empty()) {
            (false, false) => format!("{stats_notes}; {freq_notes}"),
            (false, true) => stats_notes,
            (true, false) => freq_notes.clone(),
            (true, true) => String::new(),
        };
        results.push(DatabaseCandidate {
            hit: SearchHit {
                expression: query.to_owned(),
                reading: format_kanji_reading(&row.onyomi, &row.kunyomi),
                glossary,
                tags: format_dictionary_tags(&snapshot.tags, row.dic_id, "", &row.tags),
                score: 0,
                is_kanji: true,
                notes,
                example_ja: String::new(),
                example_en: String::new(),
                dict_name: dictionary_name.clone(),
                rich_glossary: Vec::new(),
                derivation: String::new(),
                derivation_descriptions: Vec::new(),
                dictionary_id: row.dic_id,
                rules: String::new(),
                raw_glossary: row.raw_meanings,
                term_tags: row.tags,
                def_tags: String::new(),
            },
            parsed_glossary: None,
            is_exact: true,
            prefix_len,
            dic_id: row.dic_id,
        });
    }
    Ok(results)
}

fn format_kanji_reading(onyomi: &str, kunyomi: &str) -> String {
    let mut readings = Vec::with_capacity(2);
    if !onyomi.is_empty() {
        readings.push(format!("On: {onyomi}"));
    }
    if !kunyomi.is_empty() {
        readings.push(format!("Kun: {kunyomi}"));
    }
    readings.join("; ")
}

/// Formats `kanji_bank.stats` using `tag.notes` (falling back to `tag.name` when `notes` is empty),
/// sorted deterministically by `(tag.order ASC, tag.score DESC, tag.name ASC)`.
fn format_kanji_stats(
    tags: &HashMap<(i64, String), DictTag>,
    dictionary_id: i64,
    raw_stats: &str,
) -> String {
    let Ok(serde_json::Value::Object(stats)) = serde_json::from_str::<serde_json::Value>(raw_stats)
    else {
        return String::new();
    };
    let mut entries: Vec<(&DictTag, String)> = stats
        .iter()
        .filter_map(|(name, value)| {
            let tag = tags.get(&(dictionary_id, name.clone()))?;
            if !matches!(tag.category.as_str(), "index" | "misc" | "class" | "code") {
                return None;
            }
            let value = match value {
                serde_json::Value::String(value) => value.clone(),
                serde_json::Value::Number(value) => value.to_string(),
                serde_json::Value::Bool(value) => value.to_string(),
                serde_json::Value::Null
                | serde_json::Value::Array(_)
                | serde_json::Value::Object(_) => return None,
            };
            (!value.is_empty()).then_some((tag, value))
        })
        .collect();

    entries.sort_by(|(a, _), (b, _)| {
        a.order
            .cmp(&b.order)
            .then_with(|| b.score.cmp(&a.score))
            .then_with(|| a.name.cmp(&b.name))
    });

    entries
        .into_iter()
        .map(|(tag, value)| {
            let label = if tag.notes.is_empty() {
                tag.name.as_str()
            } else {
                tag.notes.as_str()
            };
            format!("{label}: {value}")
        })
        .collect::<Vec<_>>()
        .join("; ")
}

fn format_dictionary_tags(
    tags: &HashMap<(i64, String), DictTag>,
    dictionary_id: i64,
    definition_tags: &str,
    term_tags: &str,
) -> String {
    let mut seen = HashSet::new();
    let mut selected = Vec::new();
    for name in definition_tags
        .split_whitespace()
        .chain(term_tags.split_whitespace())
    {
        if name.is_empty() || !seen.insert(name) {
            continue;
        }
        if let Some(tag) = tags.get(&(dictionary_id, name.to_owned())) {
            selected.push(tag.clone());
        }
    }
    selected.sort_by(|a, b| {
        a.order
            .cmp(&b.order)
            .then_with(|| b.score.cmp(&a.score))
            .then_with(|| a.name.cmp(&b.name))
    });
    selected
        .into_iter()
        .map(|t| t.name)
        .collect::<Vec<_>>()
        .join(", ")
}

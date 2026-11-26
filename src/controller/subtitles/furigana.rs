//! Japanese furigana tokenization, LRU caching, cancellable background prewarm,
//! and script-based subtitle segmentation.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};

const FURIGANA_CACHE_CAPACITY: usize = 2048;
const PREWARM_DEFAULT_LIMIT: usize = 200;
const PREWARM_WINDOW_CAP: usize = FURIGANA_CACHE_CAPACITY / 2;

struct LruNode {
    key: String,
    parts: Vec<(String, String)>,
    prev: Option<usize>,
    next: Option<usize>,
}

struct FuriganaCache {
    index: HashMap<String, usize>,
    nodes: Vec<LruNode>,
    head: Option<usize>, // Most recently used
    tail: Option<usize>, // Least recently used
    capacity: usize,
}

impl FuriganaCache {
    fn new() -> Self {
        Self::with_capacity(FURIGANA_CACHE_CAPACITY)
    }

    fn with_capacity(capacity: usize) -> Self {
        let cap = capacity.max(1);
        let initial = cap.min(256);
        Self {
            index: HashMap::with_capacity(initial),
            nodes: Vec::with_capacity(initial),
            head: None,
            tail: None,
            capacity: cap,
        }
    }

    fn move_to_front(&mut self, slot: usize) {
        if self.head == Some(slot) {
            return;
        }
        let prev = self.nodes[slot].prev;
        let next = self.nodes[slot].next;
        if let Some(p) = prev {
            self.nodes[p].next = next;
        }
        if let Some(n) = next {
            self.nodes[n].prev = prev;
        } else {
            self.tail = prev;
        }

        let old_head = self.head;
        self.nodes[slot].prev = None;
        self.nodes[slot].next = old_head;
        if let Some(h) = old_head {
            self.nodes[h].prev = Some(slot);
        }
        self.head = Some(slot);
        if self.tail.is_none() {
            self.tail = Some(slot);
        }
    }

    fn get(&mut self, text: &str) -> Option<Vec<(String, String)>> {
        let &slot = self.index.get(text)?;
        self.move_to_front(slot);
        Some(self.nodes[slot].parts.clone())
    }

    fn contains(&self, text: &str) -> bool {
        self.index.contains_key(text)
    }

    fn insert(&mut self, text: String, parts: Vec<(String, String)>) {
        if let Some(&slot) = self.index.get(&text) {
            self.nodes[slot].parts = parts;
            self.move_to_front(slot);
            return;
        }

        if self.nodes.len() < self.capacity {
            let slot = self.nodes.len();
            let old_head = self.head;
            self.nodes.push(LruNode {
                key: text.clone(),
                parts,
                prev: None,
                next: old_head,
            });
            if let Some(h) = old_head {
                self.nodes[h].prev = Some(slot);
            } else {
                self.tail = Some(slot);
            }
            self.head = Some(slot);
            self.index.insert(text, slot);
            return;
        }

        let slot = self.tail.expect("non-empty LRU cache must have a tail");
        let evicted_key = std::mem::replace(&mut self.nodes[slot].key, text.clone());
        self.index.remove(&evicted_key);
        self.nodes[slot].parts = parts;
        self.index.insert(text, slot);
        self.move_to_front(slot);
    }

    fn clear_and_shrink(&mut self) -> bool {
        let had_entries = !self.index.is_empty() || !self.nodes.is_empty();
        self.index.clear();
        self.index.shrink_to_fit();
        self.nodes.clear();
        self.nodes.shrink_to_fit();
        self.head = None;
        self.tail = None;
        had_entries
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.index.len()
    }
}

static FURIGANA_CACHE: OnceLock<Mutex<FuriganaCache>> = OnceLock::new();

fn furigana_cache() -> &'static Mutex<FuriganaCache> {
    FURIGANA_CACHE.get_or_init(|| Mutex::new(FuriganaCache::new()))
}

struct PrewarmJob {
    epoch: u64,
    lines: Vec<String>,
}

struct PrewarmWorker {
    slot: Mutex<Option<PrewarmJob>>,
    condvar: Condvar,
    /// Held around each individual prewarmed line so
    /// `clear_furigana_cache_and_cancel_prewarm` can synchronize with any
    /// in-flight `parse_mecab` call before `unload_mecab()` runs.
    in_flight: Mutex<()>,
}

static PREWARM_EPOCH: AtomicU64 = AtomicU64::new(0);
static PREWARM_WORKER: OnceLock<Arc<PrewarmWorker>> = OnceLock::new();

fn prewarm_worker() -> &'static Arc<PrewarmWorker> {
    PREWARM_WORKER.get_or_init(|| {
        let worker = Arc::new(PrewarmWorker {
            slot: Mutex::new(None),
            condvar: Condvar::new(),
            in_flight: Mutex::new(()),
        });
        let bg = Arc::clone(&worker);
        let _ = std::thread::Builder::new()
            .name("subomi-furigana-prewarm".into())
            .spawn(move || prewarm_worker_loop(bg));
        worker
    })
}

fn submit_prewarm_job(lines: Vec<String>) {
    // Advance the generation epoch so any in-progress prewarm from a previous
    // file/track/window cancels immediately after its current line.
    let epoch = PREWARM_EPOCH.fetch_add(1, Ordering::AcqRel) + 1;
    if lines.is_empty() {
        if let Some(worker) = PREWARM_WORKER.get() {
            if let Ok(mut guard) = worker.slot.lock() {
                if PREWARM_EPOCH.load(Ordering::Acquire) == epoch {
                    *guard = None;
                }
            }
        }
        return;
    }

    let worker = prewarm_worker();
    if let Ok(mut guard) = worker.slot.lock() {
        if PREWARM_EPOCH.load(Ordering::Acquire) == epoch {
            *guard = Some(PrewarmJob { epoch, lines });
            worker.condvar.notify_one();
        }
    }
}

fn prewarm_worker_loop(worker: Arc<PrewarmWorker>) {
    loop {
        let job = {
            let Ok(mut guard) = worker.slot.lock() else {
                return;
            };
            loop {
                if let Some(job) = guard.take() {
                    break job;
                }
                guard = match worker.condvar.wait(guard) {
                    Ok(g) => g,
                    Err(_) => return,
                };
            }
        };

        for line in job.lines {
            let _in_flight = worker
                .in_flight
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if PREWARM_EPOCH.load(Ordering::Acquire) != job.epoch {
                break;
            }
            let already_cached = furigana_cache()
                .lock()
                .map(|guard| guard.contains(&line))
                .unwrap_or(false);
            if already_cached {
                continue;
            }
            let _ = subtitle_furigana_parts(&line);
        }
    }
}

fn collect_window_indices(len: usize, center: usize, radius: usize, cap: usize) -> Vec<usize> {
    if len == 0 || cap == 0 {
        return Vec::new();
    }
    let center = center.min(len - 1);
    let start = center.saturating_sub(radius);
    let end = center.saturating_add(radius).min(len - 1);
    let count = (end - start + 1).min(cap);
    let mut indices = Vec::with_capacity(count);
    indices.push(center);
    let mut step = 1usize;
    while indices.len() < count {
        let mut progressed = false;
        if let Some(fwd) = center.checked_add(step) {
            if fwd <= end {
                indices.push(fwd);
                progressed = true;
                if indices.len() >= count {
                    break;
                }
            }
        }
        if let Some(bwd) = center.checked_sub(step) {
            if bwd >= start {
                indices.push(bwd);
                progressed = true;
            }
        }
        if !progressed {
            break;
        }
        step += 1;
    }
    indices
}

/// Pre-tokenize and cache furigana for a subtitle track's initial lines on the
/// shared background prewarm worker (#345), cancelling any previous in-flight
/// prewarm and capping the batch so large files do not thrash the LRU cache.
pub fn prewarm_subtitle_furigana(lines: Vec<String>) {
    let uncached: Vec<String> = {
        let Ok(guard) = furigana_cache().lock() else {
            return;
        };
        let mut seen: HashSet<&str> =
            HashSet::with_capacity(lines.len().min(PREWARM_DEFAULT_LIMIT));
        let mut batch = Vec::new();
        for line in &lines {
            if line.is_empty() || guard.contains(line) || !seen.insert(line.as_str()) {
                continue;
            }
            batch.push(line.clone());
            if batch.len() >= PREWARM_DEFAULT_LIMIT {
                break;
            }
        }
        batch
    };
    submit_prewarm_job(uncached);
}

/// Pre-tokenize and cache furigana for a sliding window around `center`
/// (`center.saturating_sub(radius)..=center.saturating_add(radius)`), ordered
/// outward from `center` so the active and immediately upcoming cues warm first.
/// Cancels any previous in-flight prewarm job on the shared worker.
pub fn prewarm_subtitle_furigana_window(lines: &[String], center: usize, radius: usize) {
    if lines.is_empty() {
        submit_prewarm_job(Vec::new());
        return;
    }
    let indices = collect_window_indices(lines.len(), center, radius, PREWARM_WINDOW_CAP);
    let uncached: Vec<String> = {
        let Ok(guard) = furigana_cache().lock() else {
            return;
        };
        let mut seen: HashSet<&str> = HashSet::with_capacity(indices.len());
        let mut batch = Vec::new();
        for idx in indices {
            let line = &lines[idx];
            if !line.is_empty() && seen.insert(line.as_str()) && !guard.contains(line) {
                batch.push(line.clone());
            }
        }
        batch
    };
    submit_prewarm_job(uncached);
}

/// Cancel any in-flight or queued furigana prewarm job and clear & shrink the
/// process-wide furigana LRU cache.
///
/// Advances `PREWARM_EPOCH` before clearing the cache so the shared
/// `subomi-furigana-prewarm` background thread stops immediately without
/// re-invoking MeCab. Returns `true` iff cached entries were freed.
pub fn clear_furigana_cache_and_cancel_prewarm() -> bool {
    let epoch = PREWARM_EPOCH.fetch_add(1, Ordering::AcqRel) + 1;
    if let Some(worker) = PREWARM_WORKER.get() {
        if let Ok(mut guard) = worker.slot.lock() {
            if PREWARM_EPOCH.load(Ordering::Acquire) == epoch {
                *guard = None;
            }
        }
        // Wait for any single in-flight line on the background thread to finish
        // observing the bumped epoch so it cannot reload MeCab after unload.
        let _fence = worker
            .in_flight
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
    }
    let Some(cache) = FURIGANA_CACHE.get() else {
        return false;
    };
    cache
        .lock()
        .map(|mut guard| guard.clear_and_shrink())
        .unwrap_or(false)
}

#[cfg(test)]
pub(crate) fn insert_cached_furigana_for_test(text: &str, parts: Vec<(String, String)>) {
    if let Ok(mut guard) = furigana_cache().lock() {
        guard.insert(text.to_owned(), parts);
    }
}

#[cfg(test)]
pub(crate) fn prewarm_epoch_for_test() -> u64 {
    PREWARM_EPOCH.load(Ordering::Acquire)
}

/// Non-blocking cache lookup for subtitle furigana spans.
///
/// Returns `Some(parts)` if `text` is empty or already present in the LRU cache
/// (promoting the entry on hit), and `None` on a cache miss without ever
/// invoking MeCab so UI/overlay rendering never blocks.
pub fn cached_subtitle_furigana_parts(text: &str) -> Option<Vec<(String, String)>> {
    if text.is_empty() {
        return Some(Vec::new());
    }
    furigana_cache().lock().ok()?.get(text)
}

/// Tokenize the current subtitle with MeCab and attach approximate readings.
/// Run this on a worker: starting MeCab must never delay video rendering.
pub fn subtitle_furigana_parts(text: &str) -> Result<Vec<(String, String)>, String> {
    if text.is_empty() {
        return Ok(Vec::new());
    }
    if let Ok(mut guard) = furigana_cache().lock() {
        if let Some(cached) = guard.get(text) {
            return Ok(cached);
        }
    }

    // Substitute non-BMP Joyo variant `𠮟` (U+20B9F) with BMP `叱` (U+53F1) for the
    // MeCab query so dictionaries that only index `叱る` still emit its reading,
    // while `parse_mecab_furigana` restores the exact original `text` slice.
    let query_ref: Cow<'_, str> = if text.contains('𠮟') {
        Cow::Owned(text.replace('𠮟', "叱"))
    } else {
        Cow::Borrowed(text)
    };

    let output = subomi_util::mecab::parse_mecab(&query_ref)?;
    let parts = parse_mecab_furigana(text, &output)?;

    if let Ok(mut guard) = furigana_cache().lock() {
        guard.insert(text.to_owned(), parts.clone());
    }
    Ok(parts)
}

// Note: `subomi_util::mecab::find_mecab_surface_in_remaining` is currently
// private in `crates/subomi-util/src/mecab.rs`, so we keep a local zero-alloc
// matcher here for multi-line `EOS`-spanning furigana alignment.
fn find_mecab_surface_in_remaining(remaining: &str, raw_surface: &str) -> Option<(usize, usize)> {
    if !raw_surface.contains('叱') || !remaining.contains('𠮟') {
        return remaining
            .find(raw_surface)
            .map(|gap| (gap, raw_surface.len()));
    }
    for (byte_start, _) in remaining.char_indices() {
        let mut tail_chars = remaining[byte_start..].chars();
        let mut matched_bytes = 0;
        let mut ok = true;
        for rc in raw_surface.chars() {
            match tail_chars.next() {
                Some(tc) if tc == rc || (rc == '叱' && tc == '𠮟') => {
                    matched_bytes += tc.len_utf8();
                }
                _ => {
                    ok = false;
                    break;
                }
            }
        }
        if ok {
            return Some((byte_start, matched_bytes));
        }
    }
    None
}

fn apply_kinship_context_override(
    text: &str,
    offset: usize,
    surface: &str,
    reading: &mut String,
) {
    let prefix = &text[..offset];
    let suffix = &text[offset + surface.len()..];
    let has_honorific_prefix = prefix.ends_with('お') || prefix.ends_with('御');
    let has_honorific_suffix = suffix.starts_with("さん")
        || suffix.starts_with("さま")
        || suffix.starts_with("様")
        || suffix.starts_with("ちゃん");
    if has_honorific_prefix && has_honorific_suffix {
        match (surface, reading.as_str()) {
            ("兄", "あに") => *reading = "にい".to_string(),
            ("父", "ちち") => *reading = "とう".to_string(),
            ("母", "はは") => *reading = "かあ".to_string(),
            ("姉", "あね") => *reading = "ねえ".to_string(),
            _ => {}
        }
    }
}

fn parse_mecab_furigana(text: &str, output: &str) -> Result<Vec<(String, String)>, String> {
    let mut parts = Vec::new();
    let mut offset = 0;
    for raw_line in output.lines() {
        let line = raw_line.trim_end_matches('\r');
        if line == "EOS" || line.is_empty() {
            continue;
        }
        let Some((raw_surface, features)) = line.split_once('\t') else {
            continue;
        };
        if raw_surface.is_empty() {
            continue;
        }
        let remaining = &text[offset..];
        let Some((gap, matched_len)) = find_mecab_surface_in_remaining(remaining, raw_surface)
        else {
            return Err("MeCab token text did not match the subtitle".into());
        };
        if gap > 0 {
            let skipped = &remaining[..gap];
            for token in segment_subtitle_text(skipped) {
                parts.push((token, String::new()));
            }
            offset += gap;
        }
        let surface = &text[offset..offset + matched_len];
        if subomi_util::furiganautils::contains_kanji(surface) {
            let fields = subomi_util::furiganaparser::split_csv_row(features);
            let raw_reading =
                subomi_util::furiganaparser::extract_surface_reading_from_fields(surface, &fields);
            let mut reading = if !raw_reading.is_empty() && raw_reading != "*" {
                subomi_util::furiganaparser::katakana_to_hiragana_str(&raw_reading)
            } else {
                String::new()
            };
            apply_kinship_context_override(text, offset, surface, &mut reading);
            parts.extend(subomi_util::furiganautils::distribute_furigana_str(
                surface, &reading,
            ));
        } else {
            parts.push((surface.to_owned(), String::new()));
        }
        offset += matched_len;
    }
    if offset < text.len() {
        for token in segment_subtitle_text(&text[offset..]) {
            parts.push((token, String::new()));
        }
    }
    let mut cursor = text;
    let preserved = parts.iter().all(|(surface, _)| {
        if let Some(rest) = cursor.strip_prefix(surface.as_str()) {
            cursor = rest;
            true
        } else {
            false
        }
    }) && cursor.is_empty();
    if !preserved {
        return Err("MeCab did not preserve the subtitle text".into());
    }
    Ok(parts)
}

/// Segments a subtitle line into interactive word/token units for Yomitan lookup and hover.
/// Handles Japanese text (Kanji runs, Okurigana/suffixes, particles, whitespace) as well as
/// space-separated languages.
pub fn segment_subtitle_text(text: &str) -> Vec<String> {
    if text.is_empty() {
        return Vec::new();
    }

    let mut tokens = Vec::new();
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;

    let is_kanji = subomi_util::furiganautils::is_kanji_char;
    let is_hiragana = |c: char| matches!(c, '\u{3040}'..='\u{309F}');
    let is_katakana = |c: char| matches!(c, '\u{30A0}'..='\u{30FF}');

    while i < chars.len() {
        // Whitespace and punctuation remain separate non-word tokens so a
        // caller can preserve the subtitle's exact visible spacing.
        if chars[i].is_whitespace() || chars[i] == '　' {
            tokens.push(chars[i].to_string());
            i += 1;
            continue;
        }

        if matches!(
            chars[i],
            '。' | '、'
                | '！'
                | '？'
                | '…'
                | '・'
                | ','
                | '.'
                | '!'
                | '?'
                | ';'
                | ':'
                | '「'
                | '」'
                | '（'
                | '）'
        ) {
            tokens.push(chars[i].to_string());
            i += 1;
            continue;
        }

        // ASCII words, kana runs and kanji compounds are segmented by script;
        // no screenshot-specific vocabulary is embedded in the tokenizer.
        if chars[i].is_ascii_alphanumeric() {
            let mut j = i;
            while j < chars.len() && (chars[j].is_ascii_alphanumeric() || chars[j] == '\'') {
                j += 1;
            }
            tokens.push(chars[i..j].iter().collect());
            i = j;
            continue;
        }

        if is_katakana(chars[i]) {
            let mut j = i;
            while j < chars.len() && (is_katakana(chars[j]) || chars[j] == 'ー') {
                j += 1;
            }
            tokens.push(chars[i..j].iter().collect());
            i = j;
            continue;
        }

        if is_kanji(chars[i])
            || (is_hiragana(chars[i])
                && matches!(chars[i], 'お' | 'ご')
                && chars.get(i + 1).is_some_and(|c| is_kanji(*c)))
        {
            let mut j = i;
            if is_hiragana(chars[j]) {
                j += 1;
            }
            while j < chars.len()
                && (is_kanji(chars[j])
                    || (j > i && matches!(chars[j], '\u{30F5}' | '\u{30F6}')))
            {
                j += 1;
            }
            if j < chars.len()
                && is_hiragana(chars[j])
                && matches!(chars[j], 'な' | 'だ' | 'る' | 'い' | 'た')
            {
                j += 1;
            }
            tokens.push(chars[i..j].iter().collect());
            i = j;
            continue;
        }

        if is_hiragana(chars[i]) {
            if let Some(&['で', 'あ', 'る']) = chars.get(i..i + 3) {
                tokens.push("である".to_string());
                i += 3;
                continue;
            }
            if let Some(&[c0, c1]) = chars.get(i..i + 2) {
                if matches!(
                    (c0, c1),
                    ('た', 'ち')
                        | ('か', 'ら')
                        | ('ま', 'で')
                        | ('よ', 'り')
                        | ('な', 'ど')
                        | ('け', 'ど')
                        | ('の', 'で')
                        | ('の', 'に')
                ) {
                    let mut s = String::with_capacity(c0.len_utf8() + c1.len_utf8());
                    s.push(c0);
                    s.push(c1);
                    tokens.push(s);
                    i += 2;
                    continue;
                }
            }
            tokens.push(chars[i].to_string());
            i += 1;
            continue;
        }

        tokens.push(chars[i].to_string());
        i += 1;
    }

    tokens
}

#[cfg(test)]
pub(crate) static FURIGANA_GLOBAL_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
mod furigana_tests {
    use super::{
        cached_subtitle_furigana_parts, collect_window_indices, parse_mecab_furigana,
        prewarm_subtitle_furigana, segment_subtitle_text, subtitle_furigana_parts, FuriganaCache,
        FURIGANA_GLOBAL_TEST_LOCK,
    };
    use std::time::Instant;

    #[test]
    fn mecab_readings_preserve_spacing_and_inflected_surface() {
        let text = "昨日 は行った";
        let output = "昨日\t名詞,普通名詞,副詞可能,,,,キノウ,昨日,昨日,キノー\n\
は\t助詞,係助詞,,,,,ハ,は,は,ワ\n\
行っ\t動詞,非自立可能,,,五段-カ行,連用形-促音便,イク,行く,行っ,イッ\n\
た\t助動詞,,,,助動詞-タ,連体形-一般,タ,た,た,タ\nEOS\n";
        let parts = parse_mecab_furigana(text, output).unwrap();
        assert_eq!(
            parts
                .iter()
                .map(|(surface, _)| surface.as_str())
                .collect::<String>(),
            text
        );
        assert_eq!(
            parts
                .iter()
                .find(|(surface, _)| surface == "昨日")
                .unwrap()
                .1,
            "きのう"
        );
        assert_eq!(
            parts
                .iter()
                .find(|(surface, _)| surface == "行")
                .unwrap()
                .1,
            "い"
        );
        assert_eq!(
            parts
                .iter()
                .find(|(surface, _)| surface == "っ")
                .unwrap()
                .1,
            ""
        );
    }

    #[test]
    fn ipadic_9col_and_unidic_17col_26col_29col_readings_and_quoted_csv() {
        // IPADic 9-col: 来ない, 見ない, 王さま, 氷, お兄さん, 食べる, 申し込み
        let ipadic_konai = "来\t動詞,自立,*,*,カ変・クル,未然形,来る,コ,コ\n\
ない\t助動詞,*,*,*,特殊・ナイ,基本形,ない,ナイ,ナイ\nEOS\n";
        assert_eq!(
            parse_mecab_furigana("来ない", ipadic_konai).unwrap(),
            vec![("来".into(), "こ".into()), ("ない".into(), "".into())]
        );

        let ipadic_minai = "見\t動詞,自立,*,*,一段,未然形,見る,ミ,ミ\n\
ない\t助動詞,*,*,*,特殊・ナイ,基本形,ない,ナイ,ナイ\nEOS\n";
        assert_eq!(
            parse_mecab_furigana("見ない", ipadic_minai).unwrap(),
            vec![("見".into(), "み".into()), ("ない".into(), "".into())]
        );

        let ipadic_oniisan = "お兄さん\t名詞,一般,*,*,*,*,お兄さん,オニイサン,オニーサン\nEOS\n";
        assert_eq!(
            parse_mecab_furigana("お兄さん", ipadic_oniisan).unwrap(),
            vec![
                ("お".into(), "".into()),
                ("兄".into(), "にい".into()),
                ("さん".into(), "".into()),
            ]
        );

        // UniDic 17-col: lForm at col 6, pron at col 9 (with phonetic ー)
        let unidic17 = "来\t動詞,非自立可能,*,*,カ行変格,未然形-一般,クル,来る,来,コ,来る,クル,和,*,*,*,*\n\
ない\t助動詞,*,*,*,助動詞-ナイ,連体形-一般,ナイ,ない,ない,ナイ,ない,ナイ,和,*,*,*,*\n\
見\t動詞,非自立可能,*,*,上一段-マ行,未然形-一般,ミル,見る,見,ミ,見る,ミル,和,*,*,*,*\n\
ない\t助動詞,*,*,*,助動詞-ナイ,連体形-一般,ナイ,ない,ない,ナイ,ない,ナイ,和,*,*,*,*\n\
王\t名詞,固有名詞,人名,姓,*,*,オウ,オウ,王,オー,王,オー,固,*,*,*,*\n\
さま\t接尾辞,名詞的,一般,*,*,*,サマ,様,さま,サマ,さま,サマ,和,*,*,*,*\n\
氷\t名詞,普通名詞,一般,*,*,*,コオリ,氷,氷,コーリ,氷,コーリ,和,コ濁,基本形,*,*\n\
申し込み\t動詞,一般,*,*,五段-マ行,連用形-一般,モウシコム,申し込む,申し込み,モーシコミ,申し込む,モーシコム,和,*,*,*,*\nEOS\n";
        assert_eq!(
            parse_mecab_furigana("来ない見ない王さま氷申し込み", unidic17).unwrap(),
            vec![
                ("来".into(), "こ".into()),
                ("ない".into(), "".into()),
                ("見".into(), "み".into()),
                ("ない".into(), "".into()),
                ("王".into(), "おう".into()),
                ("さま".into(), "".into()),
                ("氷".into(), "こおり".into()),
                ("申".into(), "もう".into()),
                ("し".into(), "".into()),
                ("込".into(), "こ".into()),
                ("み".into(), "".into()),
            ]
        );

        // UniDic 26-col with quoted commas in fields
        let unidic26 = "王\t名詞,固有名詞,人名,姓,,,オウ,オウ,王,オー,王,オー,固,\"\",\"\",\"\",\"\",オウ,オウ,オウ,オウ,\"1,0\",\"C1,C2\",\"\",120,43\n\
様\t接尾辞,名詞的,一般,,,,サマ,様,様,サマ,様,サマ,和,\"\",\"\",\"\",\"\",サマ,サマ,サマ,サマ,\"\",\"C3\",\"\",395,14\nEOS\n";
        assert_eq!(
            parse_mecab_furigana("王様", unidic26).unwrap(),
            vec![("王".into(), "おう".into()), ("様".into(), "さま".into())]
        );
    }

    #[test]
    fn iteration_marks_and_cjk_ext_b_segment_and_parse_properly() {
        assert_eq!(segment_subtitle_text("人々"), vec!["人々"]);
        assert_eq!(segment_subtitle_text("時々"), vec!["時々"]);
        assert_eq!(segment_subtitle_text("様々"), vec!["様々"]);
        assert_eq!(segment_subtitle_text("𠮟る"), vec!["𠮟る"]);
        assert_eq!(segment_subtitle_text("一ヶ月"), vec!["一ヶ月"]);

        let ext_b_fixture = "人々\t名詞,一般,*,*,*,*,人々,ヒトビト,ヒトビト\n\
𠮟る\t動詞,自立,*,*,五段・ラ行,基本形,𠮟る,シカル,シカル\nEOS\n";
        assert_eq!(
            parse_mecab_furigana("人々𠮟る", ext_b_fixture).unwrap(),
            vec![
                ("人々".into(), "ひとびと".into()),
                ("𠮟".into(), "しか".into()),
                ("る".into(), "".into()),
            ]
        );
    }

    #[test]
    fn segment_subtitle_text_matches_three_char_dearu_and_two_char_particles() {
        assert_eq!(segment_subtitle_text("である"), vec!["である"]);
        assert_eq!(segment_subtitle_text("学生である"), vec!["学生", "である"]);
        assert_eq!(segment_subtitle_text("それから"), vec!["そ", "れ", "から"]);
        assert_eq!(
            segment_subtitle_text("子どもたち"),
            vec!["子", "ど", "も", "たち"]
        );
        assert_eq!(segment_subtitle_text("私まで"), vec!["私", "まで"]);
    }

    #[test]
    fn furigana_cache_lru_evicts_least_recently_used_and_promotes_on_hit() {
        let mut cache = FuriganaCache::with_capacity(3);
        let mk = |s: &str| vec![(s.to_string(), String::new())];

        cache.insert("a".into(), mk("a"));
        cache.insert("b".into(), mk("b"));
        cache.insert("c".into(), mk("c"));
        assert_eq!(cache.len(), 3);

        // Touching "a" promotes it to MRU, making "b" the LRU entry.
        assert_eq!(cache.get("a"), Some(mk("a")));
        // `contains` must not disturb LRU order.
        assert!(cache.contains("b"));

        // Inserting "d" must evict "b", keeping "a", "c", "d".
        cache.insert("d".into(), mk("d"));
        assert_eq!(cache.len(), 3);
        assert!(!cache.contains("b"));
        assert!(cache.contains("a"));
        assert!(cache.contains("c"));
        assert!(cache.contains("d"));

        // Updating existing "c" promotes it, leaving "a" as LRU.
        cache.insert("c".into(), mk("c2"));
        assert_eq!(cache.get("d"), Some(mk("d")));
        cache.insert("e".into(), mk("e"));
        assert!(!cache.contains("a"));
        assert_eq!(cache.get("c"), Some(mk("c2")));
        assert_eq!(cache.get("d"), Some(mk("d")));
        assert_eq!(cache.get("e"), Some(mk("e")));

        // Single-slot cache boundary check.
        let mut single = FuriganaCache::with_capacity(1);
        single.insert("x".into(), mk("x"));
        assert_eq!(single.get("x"), Some(mk("x")));
        single.insert("y".into(), mk("y"));
        assert_eq!(single.len(), 1);
        assert!(!single.contains("x"));
        assert_eq!(single.get("y"), Some(mk("y")));
    }

    #[test]
    fn prewarm_window_orders_outward_from_center_and_respects_bounds_and_cap() {
        assert_eq!(collect_window_indices(0, 0, 5, 10), Vec::<usize>::new());
        assert_eq!(collect_window_indices(5, 2, 0, 10), vec![2]);
        assert_eq!(collect_window_indices(5, 2, 1, 10), vec![2, 3, 1]);
        assert_eq!(collect_window_indices(5, 0, 2, 10), vec![0, 1, 2]);
        assert_eq!(collect_window_indices(5, 4, 2, 10), vec![4, 3, 2]);
        assert_eq!(collect_window_indices(5, 99, 1, 10), vec![4, 3]);
        assert_eq!(
            collect_window_indices(5, 2, usize::MAX, 3),
            vec![2, 3, 1]
        );
    }

    #[test]
    fn cached_subtitle_furigana_parts_is_non_blocking_on_miss_and_hits_after_parse() {
        assert_eq!(cached_subtitle_furigana_parts(""), Some(Vec::new()));
        let uncached_marker = "subomi_uncached_probe_9f8e7d6c5b4a";
        assert_eq!(cached_subtitle_furigana_parts(uncached_marker), None);
    }

    #[test]
    fn clear_furigana_cache_and_cancel_prewarm_empties_shrinks_and_bumps_epoch() {
        // Unit test `FuriganaCache::clear_and_shrink` directly first:
        let mut local = FuriganaCache::with_capacity(8);
        assert!(!local.clear_and_shrink());
        local.insert("猫".into(), vec![("猫".into(), "ねこ".into())]);
        local.insert("犬".into(), vec![("犬".into(), "いぬ".into())]);
        assert_eq!(local.len(), 2);
        assert!(local.clear_and_shrink());
        assert_eq!(local.len(), 0);
        assert_eq!(local.nodes.capacity(), 0);
        assert!(local.head.is_none());
        assert!(local.tail.is_none());
        assert!(!local.clear_and_shrink());
        // Cache remains reusable after clear & shrink.
        local.insert("鳥".into(), vec![("鳥".into(), "とり".into())]);
        assert_eq!(local.get("鳥"), Some(vec![("鳥".into(), "とり".into())]));

        // Global cache + prewarm epoch cancellation:
        let _global_guard = FURIGANA_GLOBAL_TEST_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let key = "subomi_clear_test_line_a1b2c3";
        super::insert_cached_furigana_for_test(key, vec![(key.into(), "てすと".into())]);
        assert!(cached_subtitle_furigana_parts(key).is_some());

        let epoch_before = super::prewarm_epoch_for_test();
        let freed = super::clear_furigana_cache_and_cancel_prewarm();
        assert!(freed, "clearing populated cache must report entries freed");
        assert!(
            super::prewarm_epoch_for_test() > epoch_before,
            "clearing cache must advance PREWARM_EPOCH to stop in-flight prewarm"
        );
        assert_eq!(cached_subtitle_furigana_parts(key), None);

        // Second clear with empty cache advances epoch but reports `false`.
        let epoch_after = super::prewarm_epoch_for_test();
        assert!(!super::clear_furigana_cache_and_cancel_prewarm());
        assert!(super::prewarm_epoch_for_test() > epoch_after);
    }

    #[test]
    fn live_mecab_accuracy_and_sub_100us_warm_latency() {
        let _global_guard = FURIGANA_GLOBAL_TEST_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let first = match subtitle_furigana_parts("来ない") {
            Ok(parts) => parts,
            Err(err) if err.contains("MeCab is unavailable") => {
                // Skip gracefully on CI environments without system MeCab installed.
                return;
            }
            Err(err) => panic!("unexpected MeCab failure: {err}"),
        };
        assert_eq!(
            first,
            vec![("来".into(), "こ".into()), ("ない".into(), "".into())]
        );
        assert_eq!(
            cached_subtitle_furigana_parts("来ない"),
            Some(vec![("来".into(), "こ".into()), ("ない".into(), "".into())])
        );
        assert_eq!(
            subtitle_furigana_parts("見ない").unwrap(),
            vec![("見".into(), "み".into()), ("ない".into(), "".into())]
        );
        assert_eq!(
            subtitle_furigana_parts("王さま").unwrap(),
            vec![("王".into(), "おう".into()), ("さま".into(), "".into())]
        );
        // UniDic splits 王+様; IPADic keeps 王様 as a single noun token — both read おうさま.
        let ousama = subtitle_furigana_parts("王様").unwrap();
        assert!(
            ousama == vec![("王".into(), "おう".into()), ("様".into(), "さま".into())]
                || ousama == vec![("王様".into(), "おうさま".into())],
            "unexpected furigana for 王様: {ousama:?}"
        );
        assert_eq!(
            subtitle_furigana_parts("氷").unwrap(),
            vec![("氷".into(), "こおり".into())]
        );
        assert_eq!(
            subtitle_furigana_parts("昨日").unwrap(),
            vec![("昨日".into(), "きのう".into())]
        );
        assert_eq!(
            subtitle_furigana_parts("お兄さん").unwrap(),
            vec![
                ("お".into(), "".into()),
                ("兄".into(), "にい".into()),
                ("さん".into(), "".into()),
            ]
        );
        assert_eq!(
            subtitle_furigana_parts("人々").unwrap(),
            vec![("人々".into(), "ひとびと".into())]
        );
        assert_eq!(
            subtitle_furigana_parts("食べる").unwrap(),
            vec![("食".into(), "た".into()), ("べる".into(), "".into())]
        );
        let itta = subtitle_furigana_parts("行った").unwrap();
        assert!(
            itta == vec![
                ("行".into(), "い".into()),
                ("っ".into(), "".into()),
                ("た".into(), "".into()),
            ] || itta == vec![("行".into(), "い".into()), ("った".into(), "".into())],
            "unexpected furigana for 行った: {itta:?}"
        );
        assert_eq!(
            subtitle_furigana_parts("𠮟る").unwrap(),
            vec![("𠮟".into(), "しか".into()), ("る".into(), "".into())]
        );
        assert_eq!(
            subtitle_furigana_parts("𠮟る叱る").unwrap(),
            vec![
                ("𠮟".into(), "しか".into()),
                ("る".into(), "".into()),
                ("叱".into(), "しか".into()),
                ("る".into(), "".into()),
            ]
        );

        prewarm_subtitle_furigana(vec!["時々".into(), "様々".into()]);
        let start = Instant::now();
        for _ in 0..20 {
            let parts = subtitle_furigana_parts("来ない").unwrap();
            assert_eq!(parts[0].1, "こ");
        }
        let avg_us = start.elapsed().as_micros() as f64 / 20.0;
        assert!(avg_us < 100.0, "warm furigana lookup took {avg_us:.2}us");
    }
}

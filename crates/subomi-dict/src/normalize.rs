//! Yomitan-compatible query normalization, Japanese text preprocessors,
//! bidirectional kana conversion, prolonged sound mark (`ー`) vowel expansion,
//! `々` iteration mark expansion, CJK ideograph classification (through Ext H & J),
//! and `search/remove-regex` / `search/replace-lines` text filters.

use std::borrow::Cow;
use std::collections::HashSet;

pub use regex::Regex;
use unicode_normalization::UnicodeNormalization;

use crate::databasemanager::{HIRAGANA_HIGH, HIRAGANA_LOW, KATAKANA_HIGH, KATAKANA_LOW};

const MAX_PROLONGED_VARIANTS: usize = 24;
const MAX_QUERY_VARIANTS: usize = 64;

/// Result of a Japanese text preprocessor that tracks exact original source lengths
/// (both UTF-8 bytes and UTF-16 code units) for every character in `text`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreprocessedText {
    pub text: String,
    /// Original UTF-8 byte span corresponding to each character of `text`.
    pub char_source_utf8_lens: Vec<usize>,
    /// Original UTF-16 code-unit span corresponding to each character of `text`.
    pub char_source_utf16_lens: Vec<usize>,
}

impl PreprocessedText {
    /// Creates an identity `PreprocessedText` where each character maps 1:1 to its own lengths.
    #[must_use]
    pub fn identity(text: &str) -> Self {
        let mut char_source_utf8_lens = Vec::new();
        let mut char_source_utf16_lens = Vec::new();
        for ch in text.chars() {
            char_source_utf8_lens.push(ch.len_utf8());
            char_source_utf16_lens.push(ch.len_utf16());
        }
        Self {
            text: text.to_owned(),
            char_source_utf8_lens,
            char_source_utf16_lens,
        }
    }

    /// Returns the original UTF-8 byte length covered by the first `char_count` characters of `text`.
    #[must_use]
    pub fn original_utf8_len_for_chars(&self, char_count: usize) -> usize {
        self.char_source_utf8_lens
            .iter()
            .take(char_count)
            .copied()
            .sum()
    }

    /// Returns the original UTF-16 code-unit length covered by the first `char_count` characters of `text`.
    #[must_use]
    pub fn original_utf16_len_for_chars(&self, char_count: usize) -> usize {
        self.char_source_utf16_lens
            .iter()
            .take(char_count)
            .copied()
            .sum()
    }

    /// Returns the total original UTF-8 byte length tracked across all characters.
    #[must_use]
    pub fn total_original_utf8_len(&self) -> usize {
        self.char_source_utf8_lens.iter().copied().sum()
    }

    /// Returns the total original UTF-16 code-unit length tracked across all characters.
    #[must_use]
    pub fn total_original_utf16_len(&self) -> usize {
        self.char_source_utf16_lens.iter().copied().sum()
    }
}

/// Trims leading Unicode whitespace (ASCII whitespace, NBSP, fullwidth ideographic space `　`, etc.)
/// without altering trailing content (Yomitan PR #2506).
#[must_use]
pub fn trim_leading_whitespace(text: &str) -> &str {
    text.trim_start()
}

/// Applies full Unicode NFKC normalization (e.g. `ＡＢＣ１２３` -> `ABC123`,
/// `ﾃﾚﾋﾞ` -> `テレビ`, `ｶﾞ` -> `ガ`, combining dakuten composition).
#[must_use]
pub fn normalize_nfkc(text: &str) -> String {
    text.nfkc().collect()
}

#[inline]
fn kata_to_hira_char(ch: char) -> char {
    let code = ch as u32;
    if (u32::from(KATAKANA_LOW)..=u32::from(KATAKANA_HIGH)).contains(&code) {
        char::from_u32(u32::from(HIRAGANA_LOW) + (code - u32::from(KATAKANA_LOW))).unwrap_or(ch)
    } else {
        ch
    }
}

/// Converts katakana (`ァ`..=`ヶ`) in `text` to hiragana (`ぁ`..=`ゖ`), leaving other characters intact.
#[must_use]
pub fn kata_to_hira_str(text: &str) -> String {
    text.chars().map(kata_to_hira_char).collect()
}

/// Converts hiragana (`ぁ`..=`ゖ`) in `text` to katakana (`ァ`..=`ヶ`), leaving other characters intact.
#[must_use]
pub fn hira_to_kata_str(text: &str) -> String {
    text.chars()
        .map(|ch| {
            let code = ch as u32;
            if (u32::from(HIRAGANA_LOW)..=u32::from(HIRAGANA_HIGH)).contains(&code) {
                char::from_u32(u32::from(KATAKANA_LOW) + (code - u32::from(HIRAGANA_LOW)))
                    .unwrap_or(ch)
            } else {
                ch
            }
        })
        .collect()
}

/// Normalizes text with NFKC followed by katakana-to-hiragana folding in a single pass.
#[must_use]
pub fn normalize_reading_str(text: &str) -> String {
    text.nfkc().map(kata_to_hira_char).collect()
}

/// Compares two readings after NFKC + katakana-to-hiragana folding with zero heap allocations.
#[must_use]
pub(crate) fn normalized_readings_eq(a: &str, b: &str) -> bool {
    a.nfkc()
        .map(kata_to_hira_char)
        .eq(b.nfkc().map(kata_to_hira_char))
}

/// Returns `true` if `ch` is a CJK Unified, Extension A–J (including Extension H
/// `U+31350..=U+323AF` and Extension J `U+323B0..=U+3347F`, Yomitan PR #2504),
/// or CJK Compatibility ideograph.
#[must_use]
pub fn is_cjk_ideograph(ch: char) -> bool {
    matches!(
        ch,
        '\u{3400}'..='\u{4DBF}'       // CJK Unified Ideographs Extension A
            | '\u{4E00}'..='\u{9FFF}' // CJK Unified Ideographs
            | '\u{F900}'..='\u{FAFF}' // CJK Compatibility Ideographs
            | '\u{20000}'..='\u{2A6DF}' // Extension B
            | '\u{2A700}'..='\u{2B73F}' // Extension C
            | '\u{2B740}'..='\u{2B81F}' // Extension D
            | '\u{2B820}'..='\u{2CEAF}' // Extension E
            | '\u{2CEB0}'..='\u{2EBEF}' // Extension F
            | '\u{2EBF0}'..='\u{2EE5F}' // Extension I
            | '\u{2F800}'..='\u{2FA1F}' // CJK Compatibility Ideographs Supplement
            | '\u{30000}'..='\u{3134F}' // Extension G
            | '\u{31350}'..='\u{323AF}' // Extension H
            | '\u{323B0}'..='\u{3347F}' // Extension J (Unicode 17.0 / Yomitan PR #2504)
    )
}

#[inline]
fn is_emphatic_char(ch: char) -> bool {
    matches!(ch, 'っ' | 'ッ' | 'ー')
}

/// Collapses emphatic sequences of `っ`/`ッ` and `ー` matching Yomitan's
/// `collapseEmphaticSequences`, while preserving accurate original source-length tracking.
///
/// - When `full_collapse == false`, collapses repeated consecutive `っ`/`ッ`/`ー` runs to a
///   single character (`すっっごーーい` -> `すっごーい`).
/// - When `full_collapse == true`, removes non-leading emphatic `っ`/`ッ`/`ー` characters
///   (`すっっごーーい` -> `すごい`, `すっごい` -> `すごい`), WITH the Yomitan #1411 guard:
///   geminate `っ` immediately preceding `た` or `て` (e.g. `待ったんだ`, `待ってる`, `やったーーー`)
///   is collapsed at most to a single `っ` and NEVER removed completely so `つ`-ending verbs
///   cannot falsely deconjugate as `-n` negatives (`待たん`) or potentials (`待てる`).
#[must_use]
pub fn collapse_emphatic_sequences(text: &str, full_collapse: bool) -> PreprocessedText {
    if !text.chars().any(is_emphatic_char) {
        return PreprocessedText::identity(text);
    }

    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut utf8_lens: Vec<usize> = Vec::with_capacity(chars.len());
    let mut utf16_lens: Vec<usize> = Vec::with_capacity(chars.len());

    let mut i = 0usize;
    while i < chars.len() {
        let ch = chars[i];
        if !is_emphatic_char(ch) {
            out.push(ch);
            utf8_lens.push(ch.len_utf8());
            utf16_lens.push(ch.len_utf16());
            i += 1;
            continue;
        }

        // Group consecutive identical emphatic characters.
        let run_start = i;
        let mut run_utf8 = 0usize;
        let mut run_utf16 = 0usize;
        while i < chars.len() && chars[i] == ch {
            run_utf8 += chars[i].len_utf8();
            run_utf16 += chars[i].len_utf16();
            i += 1;
        }

        // Yomitan #1411 guard: preserve a single `っ` before `た`/`て` so geminate past/te
        // forms like `待ったんだ`, `待っったんだ`, `待ってる`, `やったーーー` never strip `っ`
        // into false `-n` (`待たん`) or `-eru` (`待てる`) forms.
        let protect_geminate_sokuon =
            ch == 'っ' && matches!(chars.get(i).copied(), Some('た' | 'て'));

        if !full_collapse || run_start == 0 || protect_geminate_sokuon {
            out.push(ch);
            utf8_lens.push(run_utf8);
            utf16_lens.push(run_utf16);
        } else if let (Some(last_u8), Some(last_u16)) =
            (utf8_lens.last_mut(), utf16_lens.last_mut())
        {
            *last_u8 += run_utf8;
            *last_u16 += run_utf16;
        }
    }

    PreprocessedText {
        text: out,
        char_source_utf8_lens: utf8_lens,
        char_source_utf16_lens: utf16_lens,
    }
}

/// Returns deduplicated emphatic-collapsed variants for `text` (original, collapsed repeats,
/// and fully collapsed) with accurate original source-length tracking.
#[must_use]
pub fn collapse_emphatic_sequences_variants(text: &str) -> Vec<PreprocessedText> {
    let orig = PreprocessedText::identity(text);
    if !text.chars().any(is_emphatic_char) {
        return vec![orig];
    }
    let mut out = vec![orig];
    let collapsed = collapse_emphatic_sequences(text, false);
    if !out.iter().any(|item| item.text == collapsed.text) {
        out.push(collapsed);
    }
    let full = collapse_emphatic_sequences(text, true);
    if !out.iter().any(|item| item.text == full.text) {
        out.push(full);
    }
    out
}

/// Maps a single kyūjitai (traditional Japanese kanji) character to its shinjitai
/// (modern standard Japanese) counterpart, returning `ch` unchanged if not mapped.
#[must_use]
pub const fn standardize_kanji_char(ch: char) -> char {
    match ch {
        '亞' => '亜',
        '惡' => '悪',
        '壓' => '圧',
        '圍' => '囲',
        '醫' => '医',
        '爲' => '為',
        '壹' => '壱',
        '隱' => '隠',
        '榮' => '栄',
        '營' => '営',
        '衞' => '衛',
        '驛' => '駅',
        '圓' => '円',
        '鹽' => '塩',
        '奧' => '奥',
        '應' => '応',
        '橫' => '横',
        '歐' => '欧',
        '毆' => '殴',
        '黃' => '黄',
        '溫' => '温',
        '穩' => '穏',
        '假' => '仮',
        '價' => '価',
        '畫' => '画',
        '會' => '会',
        '壞' => '壊',
        '懷' => '懐',
        '繪' => '絵',
        '擴' => '拡',
        '殼' => '殻',
        '覺' => '覚',
        '學' => '学',
        '嶽' => '岳',
        '樂' => '楽',
        '勸' => '勧',
        '卷' => '巻',
        '歡' => '歓',
        '罐' => '缶',
        '觀' => '観',
        '關' => '関',
        '陷' => '陥',
        '巖' => '巌',
        '顏' => '顔',
        '歸' => '帰',
        '氣' => '気',
        '龜' => '亀',
        '僞' => '偽',
        '戲' => '戯',
        '犧' => '犠',
        '舊' => '旧',
        '據' => '拠',
        '擧' => '挙',
        '峽' => '峡',
        '挾' => '挟',
        '狹' => '狭',
        '曉' => '暁',
        '區' => '区',
        '驅' => '駆',
        '勳' => '勲',
        '薰' => '薫',
        '徑' => '径',
        '惠' => '恵',
        '揭' => '掲',
        '溪' => '渓',
        '經' => '経',
        '繼' => '継',
        '莖' => '茎',
        '螢' => '蛍',
        '輕' => '軽',
        '鷄' => '鶏',
        '藝' => '芸',
        '擊' => '撃',
        '缺' => '欠',
        '儉' => '倹',
        '劍' | '劔' | '劒' => '剣',
        '圈' => '圏',
        '檢' => '検',
        '權' => '権',
        '獻' => '献',
        '縣' => '県',
        '險' => '険',
        '顯' => '顕',
        '驗' => '験',
        '嚴' => '厳',
        '廣' => '広',
        '恆' => '恒',
        '鑛' => '鉱',
        '號' => '号',
        '國' => '国',
        '黑' => '黒',
        '濟' => '済',
        '碎' => '砕',
        '齋' => '斎',
        '劑' => '剤',
        '櫻' => '桜',
        '雜' => '雑',
        '參' => '参',
        '慘' => '惨',
        '棧' => '桟',
        '蠶' => '蚕',
        '贊' => '賛',
        '殘' => '残',
        '絲' => '糸',
        '齒' => '歯',
        '兒' => '児',
        '辭' => '辞',
        '濕' => '湿',
        '實' => '実',
        '寫' => '写',
        '釋' => '釈',
        '壽' => '寿',
        '收' => '収',
        '從' => '従',
        '澁' => '渋',
        '獸' => '獣',
        '縱' => '縦',
        '肅' => '粛',
        '處' => '処',
        '敍' => '叙',
        '獎' => '奨',
        '將' => '将',
        '燒' => '焼',
        '稱' => '称',
        '證' => '証',
        '乘' => '乗',
        '剩' => '剰',
        '壤' => '壌',
        '孃' => '嬢',
        '條' => '条',
        '淨' => '浄',
        '狀' => '状',
        '疊' => '畳',
        '讓' => '譲',
        '釀' => '醸',
        '囑' => '嘱',
        '觸' => '触',
        '寢' => '寝',
        '愼' => '慎',
        '眞' => '真',
        '盡' => '尽',
        '圖' => '図',
        '粹' => '粋',
        '醉' => '酔',
        '隨' => '随',
        '髓' => '髄',
        '數' => '数',
        '樞' => '枢',
        '瀨' => '瀬',
        '聲' => '声',
        '靜' => '静',
        '齊' => '斉',
        '攝' => '摂',
        '竊' => '窃',
        '專' => '専',
        '戰' => '戦',
        '淺' => '浅',
        '潛' => '潜',
        '纖' => '繊',
        '踐' => '践',
        '錢' => '銭',
        '禪' => '禅',
        '雙' => '双',
        '壯' => '壮',
        '搜' => '捜',
        '插' => '挿',
        '巢' => '巣',
        '爭' => '争',
        '總' => '総',
        '莊' => '荘',
        '裝' => '装',
        '騷' => '騒',
        '增' => '増',
        '臟' => '臓',
        '藏' => '蔵',
        '屬' => '属',
        '續' => '続',
        '墮' => '堕',
        '體' => '体',
        '對' => '対',
        '帶' => '帯',
        '滯' => '滞',
        '臺' => '台',
        '瀧' => '滝',
        '擇' => '択',
        '澤' => '沢',
        '單' => '単',
        '擔' => '担',
        '膽' => '胆',
        '團' => '団',
        '彈' => '弾',
        '斷' => '断',
        '癡' => '痴',
        '遲' => '遅',
        '晝' => '昼',
        '蟲' => '虫',
        '鑄' => '鋳',
        '廳' => '庁',
        '聽' => '聴',
        '鎭' => '鎮',
        '遞' => '逓',
        '鐵' => '鉄',
        '轉' => '転',
        '點' => '点',
        '傳' => '伝',
        '黨' => '党',
        '盜' => '盗',
        '燈' => '灯',
        '當' => '当',
        '鬪' => '闘',
        '獨' => '独',
        '讀' => '読',
        '屆' => '届',
        '繩' => '縄',
        '貳' => '弐',
        '惱' => '悩',
        '腦' => '脳',
        '霸' => '覇',
        '廢' => '廃',
        '拜' => '拝',
        '賣' => '売',
        '麥' => '麦',
        '發' => '発',
        '髮' => '髪',
        '拔' => '抜',
        '晚' => '晩',
        '蠻' => '蛮',
        '祕' => '秘',
        '濱' => '浜',
        '拂' => '払',
        '佛' => '仏',
        '竝' => '並',
        '變' => '変',
        '邊' | '邉' => '辺',
        '辨' | '瓣' | '辯' => '弁',
        '舖' => '舗',
        '步' => '歩',
        '穗' => '穂',
        '寶' => '宝',
        '豐' => '豊',
        '沒' => '没',
        '飜' => '翻',
        '萬' => '万',
        '滿' => '満',
        '默' => '黙',
        '彌' => '弥',
        '藥' => '薬',
        '譯' => '訳',
        '豫' => '予',
        '餘' => '余',
        '與' => '与',
        '譽' => '誉',
        '搖' => '揺',
        '樣' => '様',
        '謠' => '謡',
        '來' => '来',
        '賴' => '頼',
        '亂' => '乱',
        '覽' => '覧',
        '龍' => '竜',
        '兩' => '両',
        '獵' => '猟',
        '綠' => '緑',
        '鄰' => '隣',
        '壘' => '塁',
        '淚' => '涙',
        '勵' => '励',
        '禮' => '礼',
        '隸' => '隷',
        '靈' => '霊',
        '齡' => '齢',
        '戀' => '恋',
        '爐' => '炉',
        '勞' => '労',
        '樓' => '楼',
        '祿' => '禄',
        '灣' => '湾',
        '彎' => '弯',
        // Common Jinmeiyō / Hyōgai kyūjitai -> shinjitai variants
        '鰺' => '鯵',
        '焰' => '焔',
        '鷗' => '鴎',
        '鹼' => '鹸',
        '麴' => '麹',
        '堯' => '尭',
        '槇' => '槙',
        '遙' => '遥',
        '瑤' => '瑶',
        '瘦' => '痩',
        '禱' => '祷',
        '蠟' => '蝋',
        '頰' => '頬',
        '頸' => '頚',
        '軀' => '躯',
        '繡' => '繍',
        '蔣' => '蒋',
        '醬' => '醤',
        '濤' => '涛',
        '瀆' => '涜',
        '囊' => '嚢',
        '潑' => '溌',
        '醱' => '醗',
        '麪' | '麵' => '麺',
        '聰' => '聡',
        '凜' => '凛',
        '熙' => '煕',
        '餠' => '餅',
        '蟬' => '蝉',
        '蠅' => '蝿',
        '鴬' => '鶯',
        _ => ch,
    }
}

/// Converts kyūjitai (traditional kanji) characters in `text` to their shinjitai equivalents.
///
/// Callers performing dictionary lookups should keep BOTH the original query and the
/// standardized form so distinct words like `罐` vs `缶` still match cleanly.
#[must_use]
pub fn standardize_kanji(text: &str) -> String {
    text.chars().map(standardize_kanji_char).collect()
}

/// Normalizes Kangxi Radicals (`U+2F00..=U+2FD5`) and CJK Radicals Supplement
/// (`U+2E80..=U+2EFF`) characters to their corresponding standard CJK ideographs.
#[must_use]
pub fn normalize_radical_characters(text: &str) -> String {
    let has_radical = text
        .chars()
        .any(|ch| matches!(ch, '\u{2E80}'..='\u{2EFF}' | '\u{2F00}'..='\u{2FD5}'));
    if !has_radical {
        return text.to_owned();
    }

    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        if ('\u{2F00}'..='\u{2FD5}').contains(&ch) {
            // Every Kangxi Radical U+2F00..=U+2FD5 decomposes via NFKC to its exact
            // standard CJK Unified Ideograph counterpart (e.g. ⼀->一, ⼈->人, ⽔->水).
            let mut buf = [0u8; 4];
            let s = ch.encode_utf8(&mut buf);
            for mapped in s.nfkc() {
                out.push(mapped);
            }
        } else if ('\u{2E80}'..='\u{2EFF}').contains(&ch) {
            out.push(map_cjk_radical_supplement(ch));
        } else {
            out.push(ch);
        }
    }
    out
}

const fn map_cjk_radical_supplement(ch: char) -> char {
    match ch {
        '\u{2E80}' => '丷',
        '\u{2E81}' => '厂',
        '\u{2E82}' => '乛',
        '\u{2E83}' => '乚',
        '\u{2E84}' => '乙',
        '\u{2E85}' => '亻',
        '\u{2E86}' => '冂',
        '\u{2E87}' => '几',
        '\u{2E88}' => '刀',
        '\u{2E89}' => '刂',
        '\u{2E8A}' => '卜',
        '\u{2E8B}' => '㔾',
        '\u{2E8C}' => '小',
        '\u{2E8D}' => '小',
        '\u{2E8E}' => '兀',
        '\u{2E8F}' => '尣',
        '\u{2E90}' => '尤',
        '\u{2E91}' => '尢',
        '\u{2E92}' => '巳',
        '\u{2E93}' => '幺',
        '\u{2E94}' => '彑',
        '\u{2E95}' => '彐',
        '\u{2E96}' => '忄',
        '\u{2E97}' => '心',
        '\u{2E98}' => '扌',
        '\u{2E99}' => '攵',
        '\u{2E9B}' => '旡',
        '\u{2E9C}' => '日',
        '\u{2E9D}' => '月',
        '\u{2E9E}' => '歹',
        '\u{2E9F}' => '母',
        '\u{2EA0}' => '民',
        '\u{2EA1}' => '氵',
        '\u{2EA2}' => '氺',
        '\u{2EA3}' => '灬',
        '\u{2EA4}' => '爫',
        '\u{2EA5}' => '爪',
        '\u{2EA6}' => '丬',
        '\u{2EA7}' => '牛',
        '\u{2EA8}' => '犭',
        '\u{2EA9}' => '玉',
        '\u{2EAA}' => '疋',
        '\u{2EAB}' => '罒',
        '\u{2EAC}' => '示',
        '\u{2EAD}' => '礻',
        '\u{2EAE}' => '竹',
        '\u{2EAF}' => '糸',
        '\u{2EB0}' => '糹',
        '\u{2EB1}' => '罓',
        '\u{2EB2}' => '罒',
        '\u{2EB3}' => '网',
        '\u{2EB4}' => '网',
        '\u{2EB5}' => '𦉪',
        '\u{2EB6}' => '羊',
        '\u{2EB7}' => '羊',
        '\u{2EB8}' => '羋',
        '\u{2EB9}' => '耂',
        '\u{2EBA}' => '肀',
        '\u{2EBB}' => '肀',
        '\u{2EBC}' => '肉',
        '\u{2EBD}' => '臼',
        '\u{2EBE}' => '艹',
        '\u{2EBF}' => '艹',
        '\u{2EC0}' => '艹',
        '\u{2EC1}' => '虎',
        '\u{2EC2}' => '衤',
        '\u{2EC3}' => '覀',
        '\u{2EC4}' => '西',
        '\u{2EC5}' => '见',
        '\u{2EC6}' => '角',
        '\u{2EC7}' => '角',
        '\u{2EC8}' => '讠',
        '\u{2EC9}' => '贝',
        '\u{2ECA}' => '足',
        '\u{2ECB}' => '车',
        '\u{2ECC}' => '辶',
        '\u{2ECD}' => '辶',
        '\u{2ECE}' => '辶',
        '\u{2ECF}' => '阝',
        '\u{2ED0}' => '钅',
        '\u{2ED1}' => '長',
        '\u{2ED2}' => '镸',
        '\u{2ED3}' => '长',
        '\u{2ED4}' => '门',
        '\u{2ED5}' => '𨸏',
        '\u{2ED6}' => '阝',
        '\u{2ED7}' => '雨',
        '\u{2ED8}' => '青',
        '\u{2ED9}' => '韦',
        '\u{2EDA}' => '页',
        '\u{2EDB}' => '风',
        '\u{2EDC}' => '飞',
        '\u{2EDD}' => '食',
        '\u{2EDE}' => '𩙿',
        '\u{2EDF}' => '飠',
        '\u{2EE0}' => '饣',
        '\u{2EE1}' => '𩠐',
        '\u{2EE2}' => '马',
        '\u{2EE3}' => '骨',
        '\u{2EE4}' => '鬼',
        '\u{2EE5}' => '鱼',
        '\u{2EE6}' => '鸟',
        '\u{2EE7}' => '卤',
        '\u{2EE8}' => '麦',
        '\u{2EE9}' => '黄',
        '\u{2EEA}' => '黾',
        '\u{2EEB}' => '斉',
        '\u{2EEC}' => '齐',
        '\u{2EED}' => '歯',
        '\u{2EEE}' => '齿',
        '\u{2EEF}' => '竜',
        '\u{2EF0}' => '龙',
        '\u{2EF1}' => '龜',
        '\u{2EF2}' => '亀',
        '\u{2EF3}' => '龟',
        _ => ch,
    }
}

/// Converts halfwidth ASCII alphanumerics (`0-9`, `A-Z`, `a-z`) to fullwidth equivalents
/// (`０-９`, `Ａ-Ｚ`, `ａ-ｚ`).
#[must_use]
pub fn to_fullwidth_alphanumeric(text: &str) -> String {
    text.chars()
        .map(|ch| match ch {
            '0'..='9' => char::from_u32(0xFF10 + (ch as u32 - '0' as u32)).unwrap_or(ch),
            'A'..='Z' => char::from_u32(0xFF21 + (ch as u32 - 'A' as u32)).unwrap_or(ch),
            'a'..='z' => char::from_u32(0xFF41 + (ch as u32 - 'a' as u32)).unwrap_or(ch),
            _ => ch,
        })
        .collect()
}

/// Converts fullwidth alphanumerics (`０-９`, `Ａ-Ｚ`, `ａ-ｚ`) to halfwidth ASCII equivalents.
#[must_use]
pub fn to_halfwidth_alphanumeric(text: &str) -> String {
    text.chars()
        .map(|ch| match ch {
            '０'..='９' => char::from_u32('0' as u32 + (ch as u32 - 0xFF10)).unwrap_or(ch),
            'Ａ'..='Ｚ' => char::from_u32('A' as u32 + (ch as u32 - 0xFF21)).unwrap_or(ch),
            'ａ'..='ｚ' => char::from_u32('a' as u32 + (ch as u32 - 0xFF41)).unwrap_or(ch),
            _ => ch,
        })
        .collect()
}

/// Generates halfwidth and fullwidth alphanumeric variants for `text`, always preserving
/// `text` first and deduplicating unchanged forms.
#[must_use]
pub fn alphanumeric_width_variants(text: &str) -> Vec<String> {
    let mut out = vec![text.to_owned()];
    let half = to_halfwidth_alphanumeric(text);
    if half != text {
        out.push(half);
    }
    let full = to_fullwidth_alphanumeric(text);
    if full != text && !out.contains(&full) {
        out.push(full);
    }
    out
}

#[inline]
fn is_romaji_consonant(ch: char) -> bool {
    matches!(
        ch,
        'b' | 'c'
            | 'd'
            | 'f'
            | 'g'
            | 'h'
            | 'j'
            | 'k'
            | 'l'
            | 'm'
            | 'p'
            | 'q'
            | 'r'
            | 's'
            | 't'
            | 'v'
            | 'w'
            | 'x'
            | 'y'
            | 'z'
    )
}

#[inline]
fn is_romaji_vowel_or_y(ch: char) -> bool {
    matches!(ch, 'a' | 'i' | 'u' | 'e' | 'o' | 'y')
}

fn match_romaji_4(s: [char; 4]) -> Option<&'static str> {
    match s {
        ['l', 't', 's', 'u'] | ['x', 't', 's', 'u'] => Some("っ"),
        _ => None,
    }
}

fn match_romaji_3(s: [char; 3]) -> Option<&'static str> {
    match s {
        ['s', 'h', 'a'] | ['s', 'y', 'a'] => Some("しゃ"),
        ['s', 'h', 'i'] => Some("し"),
        ['s', 'h', 'u'] | ['s', 'y', 'u'] => Some("しゅ"),
        ['s', 'h', 'e'] | ['s', 'y', 'e'] => Some("しぇ"),
        ['s', 'h', 'o'] | ['s', 'y', 'o'] => Some("しょ"),
        ['s', 'y', 'i'] => Some("しぃ"),
        ['c', 'h', 'a'] | ['t', 'y', 'a'] | ['c', 'y', 'a'] => Some("ちゃ"),
        ['c', 'h', 'i'] => Some("ち"),
        ['c', 'h', 'u'] | ['t', 'y', 'u'] | ['c', 'y', 'u'] => Some("ちゅ"),
        ['c', 'h', 'e'] | ['t', 'y', 'e'] | ['c', 'y', 'e'] => Some("ちぇ"),
        ['c', 'h', 'o'] | ['t', 'y', 'o'] | ['c', 'y', 'o'] => Some("ちょ"),
        ['t', 'y', 'i'] | ['c', 'y', 'i'] => Some("ちぃ"),
        ['t', 's', 'u'] => Some("つ"),
        ['t', 's', 'a'] => Some("つぁ"),
        ['t', 's', 'i'] => Some("つぃ"),
        ['t', 's', 'e'] => Some("つぇ"),
        ['t', 's', 'o'] => Some("つぉ"),
        ['d', 'z', 'u'] => Some("づ"),
        ['k', 'y', 'a'] => Some("きゃ"),
        ['k', 'y', 'i'] => Some("きぃ"),
        ['k', 'y', 'u'] => Some("きゅ"),
        ['k', 'y', 'e'] => Some("きぇ"),
        ['k', 'y', 'o'] => Some("きょ"),
        ['g', 'y', 'a'] => Some("ぎゃ"),
        ['g', 'y', 'i'] => Some("ぎぃ"),
        ['g', 'y', 'u'] => Some("ぎゅ"),
        ['g', 'y', 'e'] => Some("ぎぇ"),
        ['g', 'y', 'o'] => Some("ぎょ"),
        ['j', 'y', 'a'] | ['z', 'y', 'a'] => Some("じゃ"),
        ['j', 'y', 'i'] | ['z', 'y', 'i'] => Some("じぃ"),
        ['j', 'y', 'u'] | ['z', 'y', 'u'] => Some("じゅ"),
        ['j', 'y', 'e'] | ['z', 'y', 'e'] => Some("じぇ"),
        ['j', 'y', 'o'] | ['z', 'y', 'o'] => Some("じょ"),
        ['d', 'y', 'a'] => Some("ぢゃ"),
        ['d', 'y', 'i'] => Some("ぢぃ"),
        ['d', 'y', 'u'] => Some("ぢゅ"),
        ['d', 'y', 'e'] => Some("ぢぇ"),
        ['d', 'y', 'o'] => Some("ぢょ"),
        ['n', 'y', 'a'] => Some("にゃ"),
        ['n', 'y', 'i'] => Some("にぃ"),
        ['n', 'y', 'u'] => Some("にゅ"),
        ['n', 'y', 'e'] => Some("にぇ"),
        ['n', 'y', 'o'] => Some("にょ"),
        ['h', 'y', 'a'] => Some("ひゃ"),
        ['h', 'y', 'i'] => Some("ひぃ"),
        ['h', 'y', 'u'] => Some("ひゅ"),
        ['h', 'y', 'e'] => Some("ひぇ"),
        ['h', 'y', 'o'] => Some("ひょ"),
        ['b', 'y', 'a'] => Some("びゃ"),
        ['b', 'y', 'i'] => Some("びぃ"),
        ['b', 'y', 'u'] => Some("びゅ"),
        ['b', 'y', 'e'] => Some("びぇ"),
        ['b', 'y', 'o'] => Some("びょ"),
        ['p', 'y', 'a'] => Some("ぴゃ"),
        ['p', 'y', 'i'] => Some("ぴぃ"),
        ['p', 'y', 'u'] => Some("ぴゅ"),
        ['p', 'y', 'e'] => Some("ぴぇ"),
        ['p', 'y', 'o'] => Some("ぴょ"),
        ['m', 'y', 'a'] => Some("みゃ"),
        ['m', 'y', 'i'] => Some("みぃ"),
        ['m', 'y', 'u'] => Some("みゅ"),
        ['m', 'y', 'e'] => Some("みぇ"),
        ['m', 'y', 'o'] => Some("みょ"),
        ['r', 'y', 'a'] => Some("りゃ"),
        ['r', 'y', 'i'] => Some("りぃ"),
        ['r', 'y', 'u'] => Some("りゅ"),
        ['r', 'y', 'e'] => Some("りぇ"),
        ['r', 'y', 'o'] => Some("りょ"),
        ['f', 'y', 'a'] => Some("ふゃ"),
        ['f', 'y', 'u'] => Some("ふゅ"),
        ['f', 'y', 'o'] => Some("ふょ"),
        ['v', 'y', 'a'] => Some("ゔゃ"),
        ['v', 'y', 'u'] => Some("ゔゅ"),
        ['v', 'y', 'o'] => Some("ゔょ"),
        ['t', 'h', 'a'] => Some("てゃ"),
        ['t', 'h', 'i'] => Some("てぃ"),
        ['t', 'h', 'u'] => Some("てゅ"),
        ['t', 'h', 'e'] => Some("てぇ"),
        ['t', 'h', 'o'] => Some("てょ"),
        ['d', 'h', 'a'] => Some("でゃ"),
        ['d', 'h', 'i'] => Some("でぃ"),
        ['d', 'h', 'u'] => Some("でゅ"),
        ['d', 'h', 'e'] => Some("でぇ"),
        ['d', 'h', 'o'] => Some("でょ"),
        ['t', 'w', 'a'] => Some("とぁ"),
        ['t', 'w', 'i'] => Some("とぃ"),
        ['t', 'w', 'u'] => Some("とぅ"),
        ['t', 'w', 'e'] => Some("とぇ"),
        ['t', 'w', 'o'] => Some("とぉ"),
        ['d', 'w', 'a'] => Some("どぁ"),
        ['d', 'w', 'i'] => Some("どぃ"),
        ['d', 'w', 'u'] => Some("どぅ"),
        ['d', 'w', 'e'] => Some("どぇ"),
        ['d', 'w', 'o'] => Some("どぉ"),
        ['k', 'w', 'a'] => Some("くぁ"),
        ['k', 'w', 'i'] => Some("くぃ"),
        ['k', 'w', 'e'] => Some("くぇ"),
        ['k', 'w', 'o'] => Some("くぉ"),
        ['g', 'w', 'a'] => Some("ぐぁ"),
        ['g', 'w', 'i'] => Some("ぐぃ"),
        ['g', 'w', 'e'] => Some("ぐぇ"),
        ['g', 'w', 'o'] => Some("ぐぉ"),
        ['w', 'h', 'a'] => Some("うぁ"),
        ['w', 'h', 'i'] => Some("うぃ"),
        ['w', 'h', 'e'] => Some("うぇ"),
        ['w', 'h', 'o'] => Some("うぉ"),
        ['l', 't', 'u'] | ['x', 't', 'u'] => Some("っ"),
        ['l', 'y', 'a'] | ['x', 'y', 'a'] => Some("ゃ"),
        ['l', 'y', 'u'] | ['x', 'y', 'u'] => Some("ゅ"),
        ['l', 'y', 'o'] | ['x', 'y', 'o'] => Some("ょ"),
        ['l', 'w', 'a'] | ['x', 'w', 'a'] => Some("ゎ"),
        ['l', 'k', 'a'] | ['x', 'k', 'a'] => Some("ゕ"),
        ['l', 'k', 'e'] | ['x', 'k', 'e'] => Some("ゖ"),
        _ => None,
    }
}

fn match_romaji_2(s: [char; 2]) -> Option<&'static str> {
    match s {
        ['k', 'a'] => Some("か"),
        ['k', 'i'] => Some("き"),
        ['k', 'u'] => Some("く"),
        ['k', 'e'] => Some("け"),
        ['k', 'o'] => Some("こ"),
        ['g', 'a'] => Some("が"),
        ['g', 'i'] => Some("ぎ"),
        ['g', 'u'] => Some("ぐ"),
        ['g', 'e'] => Some("げ"),
        ['g', 'o'] => Some("ご"),
        ['s', 'a'] => Some("さ"),
        ['s', 'i'] => Some("し"),
        ['s', 'u'] => Some("す"),
        ['s', 'e'] => Some("せ"),
        ['s', 'o'] => Some("そ"),
        ['z', 'a'] => Some("ざ"),
        ['z', 'i'] => Some("じ"),
        ['z', 'u'] => Some("ず"),
        ['z', 'e'] => Some("ぜ"),
        ['z', 'o'] => Some("ぞ"),
        ['j', 'a'] => Some("じゃ"),
        ['j', 'i'] => Some("じ"),
        ['j', 'u'] => Some("じゅ"),
        ['j', 'e'] => Some("じぇ"),
        ['j', 'o'] => Some("じょ"),
        ['t', 'a'] => Some("た"),
        ['t', 'i'] => Some("ち"),
        ['t', 'u'] => Some("つ"),
        ['t', 'e'] => Some("て"),
        ['t', 'o'] => Some("と"),
        ['d', 'a'] => Some("だ"),
        ['d', 'i'] => Some("ぢ"),
        ['d', 'u'] => Some("づ"),
        ['d', 'e'] => Some("で"),
        ['d', 'o'] => Some("ど"),
        ['n', 'a'] => Some("な"),
        ['n', 'i'] => Some("に"),
        ['n', 'u'] => Some("ぬ"),
        ['n', 'e'] => Some("ね"),
        ['n', 'o'] => Some("の"),
        ['h', 'a'] => Some("は"),
        ['h', 'i'] => Some("ひ"),
        ['h', 'u'] => Some("ふ"),
        ['h', 'e'] => Some("へ"),
        ['h', 'o'] => Some("ほ"),
        ['f', 'a'] => Some("ふぁ"),
        ['f', 'i'] => Some("ふぃ"),
        ['f', 'u'] => Some("ふ"),
        ['f', 'e'] => Some("ふぇ"),
        ['f', 'o'] => Some("ふぉ"),
        ['b', 'a'] => Some("ば"),
        ['b', 'i'] => Some("び"),
        ['b', 'u'] => Some("ぶ"),
        ['b', 'e'] => Some("べ"),
        ['b', 'o'] => Some("ぼ"),
        ['p', 'a'] => Some("ぱ"),
        ['p', 'i'] => Some("ぴ"),
        ['p', 'u'] => Some("ぷ"),
        ['p', 'e'] => Some("ぺ"),
        ['p', 'o'] => Some("ぽ"),
        ['m', 'a'] => Some("ま"),
        ['m', 'i'] => Some("み"),
        ['m', 'u'] => Some("む"),
        ['m', 'e'] => Some("め"),
        ['m', 'o'] => Some("も"),
        ['y', 'a'] => Some("や"),
        ['y', 'i'] => Some("い"),
        ['y', 'u'] => Some("ゆ"),
        ['y', 'e'] => Some("いぇ"),
        ['y', 'o'] => Some("よ"),
        ['r', 'a'] => Some("ら"),
        ['r', 'i'] => Some("り"),
        ['r', 'u'] => Some("る"),
        ['r', 'e'] => Some("れ"),
        ['r', 'o'] => Some("ろ"),
        ['w', 'a'] => Some("わ"),
        ['w', 'i'] => Some("うぃ"),
        ['w', 'u'] => Some("う"),
        ['w', 'e'] => Some("うぇ"),
        ['w', 'o'] => Some("を"),
        ['v', 'a'] => Some("ゔぁ"),
        ['v', 'i'] => Some("ゔぃ"),
        ['v', 'u'] => Some("ゔ"),
        ['v', 'e'] => Some("ゔぇ"),
        ['v', 'o'] => Some("ゔぉ"),
        ['q', 'a'] => Some("くぁ"),
        ['q', 'i'] => Some("くぃ"),
        ['q', 'u'] => Some("く"),
        ['q', 'e'] => Some("くぇ"),
        ['q', 'o'] => Some("くぉ"),
        ['c', 'a'] => Some("か"),
        ['c', 'i'] => Some("し"),
        ['c', 'u'] => Some("く"),
        ['c', 'e'] => Some("せ"),
        ['c', 'o'] => Some("こ"),
        ['l', 'a'] | ['x', 'a'] => Some("ぁ"),
        ['l', 'i'] | ['x', 'i'] => Some("ぃ"),
        ['l', 'u'] | ['x', 'u'] => Some("ぅ"),
        ['l', 'e'] | ['x', 'e'] => Some("ぇ"),
        ['l', 'o'] | ['x', 'o'] => Some("ぉ"),
        ['x', 'n'] => Some("ん"),
        _ => None,
    }
}

/// WanaKana-compatible romaji -> hiragana converter supporting Hepburn and Kunrei-shiki
/// transliterations, `nn`/`n'` -> `ん`, double-consonant and `tch` sokuon -> `っ`,
/// and `-` -> `ー`. Non-ASCII characters are preserved verbatim.
#[must_use]
pub fn alphabetic_to_hiragana(text: &str) -> String {
    if !text
        .chars()
        .any(|ch| ch.is_ascii_alphabetic() || ch == '-')
    {
        return text.to_owned();
    }

    let orig_chars: Vec<char> = text.chars().collect();
    let lower_chars: Vec<char> = orig_chars
        .iter()
        .map(|&ch| ch.to_ascii_lowercase())
        .collect();
    let len = lower_chars.len();
    let mut out = String::with_capacity(text.len() * 2);
    let mut i = 0usize;

    while i < len {
        let ch = lower_chars[i];

        // Hyphen -> prolonged sound mark `ー`
        if ch == '-' {
            out.push('ー');
            i += 1;
            continue;
        }

        // `n'` -> `ん`
        if ch == 'n' && i + 1 < len && lower_chars[i + 1] == '\'' {
            out.push('ん');
            i += 2;
            continue;
        }

        // `nn`: if followed by vowel or `y` (e.g. `konna`, `minna`, `konnichiwa`),
        // emit `ん` for the first `n` and let the second `n` start `na/ni/nu/ne/no/nya`;
        // otherwise consume both `nn` as `ん` (e.g. `kinn`, `nn`).
        if ch == 'n' && i + 1 < len && lower_chars[i + 1] == 'n' {
            out.push('ん');
            if i + 2 < len && is_romaji_vowel_or_y(lower_chars[i + 2]) {
                i += 1;
            } else {
                i += 2;
            }
            continue;
        }

        // Special Hepburn sokuon `tch` -> `っ` + `ch...` (e.g. `matcha` -> `まっちゃ`)
        if ch == 't'
            && i + 2 < len
            && lower_chars[i + 1] == 'c'
            && lower_chars[i + 2] == 'h'
        {
            out.push('っ');
            i += 1;
            continue;
        }

        // Double consonant sokuon (excluding `n`, handled above): `kk`, `tt`, `pp`, `ss`, `cc`, ...
        if is_romaji_consonant(ch) && i + 1 < len && lower_chars[i + 1] == ch {
            out.push('っ');
            i += 1;
            continue;
        }

        // 4-char sequences (`ltsu`, `xtsu`)
        if i + 3 < len {
            if let Some(kana) = match_romaji_4([
                lower_chars[i],
                lower_chars[i + 1],
                lower_chars[i + 2],
                lower_chars[i + 3],
            ]) {
                out.push_str(kana);
                i += 4;
                continue;
            }
        }

        // 3-char sequences (`sha`, `chi`, `tsu`, `kya`, `thi`, ...)
        if i + 2 < len {
            if let Some(kana) =
                match_romaji_3([lower_chars[i], lower_chars[i + 1], lower_chars[i + 2]])
            {
                out.push_str(kana);
                i += 3;
                continue;
            }
        }

        // 2-char sequences (`ka`, `si`, `tu`, `wo`, `fa`, ...)
        if i + 1 < len {
            if let Some(kana) = match_romaji_2([lower_chars[i], lower_chars[i + 1]]) {
                out.push_str(kana);
                i += 2;
                continue;
            }
        }

        // Single vowels and trailing/pre-consonant `n`
        match ch {
            'a' => {
                out.push('あ');
                i += 1;
            }
            'i' => {
                out.push('い');
                i += 1;
            }
            'u' => {
                out.push('う');
                i += 1;
            }
            'e' => {
                out.push('え');
                i += 1;
            }
            'o' => {
                out.push('お');
                i += 1;
            }
            'n' if i + 1 == len || !is_romaji_vowel_or_y(lower_chars[i + 1]) => {
                out.push('ん');
                i += 1;
            }
            _ => {
                out.push(orig_chars[i]);
                i += 1;
            }
        }
    }

    out
}

fn vowel_expansions_for_prev_hira(prev: char) -> &'static [char] {
    match prev {
        'あ' | 'か' | 'が' | 'さ' | 'ざ' | 'た' | 'だ' | 'な' | 'は' | 'ば' | 'ぱ' | 'ま'
        | 'や' | 'ら' | 'わ' | 'ぁ' | 'ゃ' | 'ゎ' | 'ゕ' => &['あ'],
        'い' | 'き' | 'ぎ' | 'し' | 'じ' | 'ち' | 'ぢ' | 'に' | 'ひ' | 'び' | 'ぴ' | 'み'
        | 'り' | 'ぃ' => &['い'],
        'う' | 'ゔ' | 'く' | 'ぐ' | 'す' | 'ず' | 'つ' | 'づ' | 'ぬ' | 'ふ' | 'ぶ' | 'ぷ'
        | 'む' | 'ゆ' | 'る' | 'ぅ' | 'ゅ' => &['う'],
        'え' | 'け' | 'げ' | 'せ' | 'ぜ' | 'て' | 'で' | 'ね' | 'へ' | 'べ' | 'ぺ' | 'め'
        | 'れ' | 'ぇ' | 'ゖ' => &['え', 'い'],
        'お' | 'こ' | 'ご' | 'そ' | 'ぞ' | 'と' | 'ど' | 'の' | 'ほ' | 'ぼ' | 'ぽ' | 'も'
        | 'よ' | 'ろ' | 'を' | 'ぉ' | 'ょ' => &['う', 'お'],
        _ => &[],
    }
}

/// Expands prolonged sound marks (`ー`) after kana into orthographic vowel spellings,
/// including `ねー` -> `ねえ`, `すげー` -> `すげえ`, `おーきい` -> `おおきい`,
/// and colloquial `ゆー` -> `いう`/`ゆう` (`こーゆー` -> `こういう`/`こうゆう`).
#[must_use]
pub fn expand_prolonged_vowels(text: &str) -> Vec<String> {
    if !text.contains('ー') {
        return vec![text.to_owned()];
    }
    let hira = kata_to_hira_str(text);
    let chars: Vec<char> = hira.chars().collect();
    let mut partials: Vec<String> = vec![String::with_capacity(hira.len())];
    let mut i = 0usize;

    while i < chars.len() {
        let ch = chars[i];
        // Special colloquial `ゆー` -> both `ゆう` and `いう` (e.g. `こーゆー` -> `こういう` / `こうゆう`).
        if ch == 'ゆ' && chars.get(i + 1) == Some(&'ー') {
            let mut next_partials = Vec::with_capacity((partials.len() * 2).min(MAX_PROLONGED_VARIANTS));
            for base in &partials {
                if next_partials.len() >= MAX_PROLONGED_VARIANTS {
                    break;
                }
                let mut v_iu = base.clone();
                v_iu.push_str("いう");
                next_partials.push(v_iu);
                if next_partials.len() < MAX_PROLONGED_VARIANTS {
                    let mut v_yuu = base.clone();
                    v_yuu.push_str("ゆう");
                    next_partials.push(v_yuu);
                }
            }
            partials = next_partials;
            i += 2;
            continue;
        }

        if ch == 'ー' && i > 0 {
            let prev = chars[i - 1];
            let vowels = vowel_expansions_for_prev_hira(prev);
            if !vowels.is_empty() {
                let mut next_partials =
                    Vec::with_capacity((partials.len() * vowels.len()).min(MAX_PROLONGED_VARIANTS));
                'outer: for base in &partials {
                    for &v in vowels {
                        if next_partials.len() >= MAX_PROLONGED_VARIANTS {
                            break 'outer;
                        }
                        let mut candidate = base.clone();
                        candidate.push(v);
                        next_partials.push(candidate);
                    }
                }
                partials = next_partials;
                i += 1;
                continue;
            }
        }

        for p in &mut partials {
            p.push(ch);
        }
        i += 1;
    }

    let mut out = Vec::with_capacity(partials.len() + 1);
    let mut seen = HashSet::with_capacity(partials.len() + 1);
    if seen.insert(text.to_owned()) {
        out.push(text.to_owned());
    }
    for p in partials {
        if seen.insert(p.clone()) {
            out.push(p);
        }
    }
    out
}

/// Expands CJK iteration mark `々` bidirectionally (`人々` <-> `人人`).
#[must_use]
pub fn expand_iteration_marks(text: &str) -> Vec<String> {
    let mut out = vec![text.to_owned()];
    let chars: Vec<char> = text.chars().collect();
    if chars.len() < 2 {
        return out;
    }

    // 1. `X々` -> `XX`
    if chars.contains(&'々') {
        let mut expanded = String::with_capacity(text.len());
        let mut prev: Option<char> = None;
        for &ch in &chars {
            if ch == '々' {
                if let Some(p) = prev {
                    expanded.push(p);
                    prev = Some(p);
                    continue;
                }
            }
            expanded.push(ch);
            prev = Some(ch);
        }
        if expanded != text {
            out.push(expanded);
        }
    }

    // 2. `XX` -> `X々` for repeated CJK ideographs
    let mut has_repeated_kanji = false;
    for pair in chars.windows(2) {
        if pair[0] == pair[1] && is_cjk_ideograph(pair[0]) {
            has_repeated_kanji = true;
            break;
        }
    }
    if has_repeated_kanji {
        let mut folded = String::with_capacity(text.len());
        let mut i = 0usize;
        while i < chars.len() {
            folded.push(chars[i]);
            if i + 1 < chars.len() && chars[i] == chars[i + 1] && is_cjk_ideograph(chars[i]) {
                folded.push('々');
                i += 2;
            } else {
                i += 1;
            }
        }
        if folded != text && !out.contains(&folded) {
            out.push(folded);
        }
    }

    out
}

/// Expands a query or prefix into all Yomitan-equivalent lookup forms:
/// raw, NFKC, radical-normalized, kyūjitai -> shinjitai standardized kanji (always keeping
/// original too), emphatic sequence collapsing (with Yomitan #1411 guard), bidirectional
/// hiragana <-> katakana, `々` <-> `XX`, prolonged `ー` vowels, alphanumeric width variants,
/// and romaji -> hiragana. Capped to prevent candidate explosion (#2153).
#[must_use]
pub fn expand_query_variants(text: &str) -> Vec<String> {
    let trimmed = trim_leading_whitespace(text);
    if trimmed.is_empty() {
        return Vec::new();
    }
    let mut out: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let push_variant = |out: &mut Vec<String>, seen: &mut HashSet<String>, s: String| -> bool {
        if !s.is_empty() && out.len() < MAX_QUERY_VARIANTS && seen.insert(s.clone()) {
            out.push(s);
        }
        out.len() < MAX_QUERY_VARIANTS
    };

    let nfkc = normalize_nfkc(trimmed);
    push_variant(&mut out, &mut seen, trimmed.to_owned());
    push_variant(&mut out, &mut seen, nfkc.clone());

    let mut base_seeds: Vec<String> = Vec::new();
    let push_seed = |seeds: &mut Vec<String>, s: String| {
        if !s.is_empty() && !seeds.contains(&s) {
            seeds.push(s);
        }
    };
    push_seed(&mut base_seeds, nfkc.clone());

    let rad = normalize_radical_characters(&nfkc);
    if rad != nfkc {
        push_seed(&mut base_seeds, rad.clone());
    }
    let std_kanji = standardize_kanji(&rad);
    if std_kanji != rad {
        push_seed(&mut base_seeds, std_kanji);
    }

    // Add emphatic-collapsed variants (both repeat-collapse and full-collapse with #1411 guard).
    let pre_emph_len = base_seeds.len();
    for idx in 0..pre_emph_len {
        let seed = base_seeds[idx].clone();
        if seed.chars().any(is_emphatic_char) {
            for emph in collapse_emphatic_sequences_variants(&seed) {
                push_seed(&mut base_seeds, emph.text);
            }
        }
    }

    // Alphanumeric width variants (halfwidth <-> fullwidth)
    for width_var in alphanumeric_width_variants(&nfkc) {
        push_variant(&mut out, &mut seen, width_var);
    }

    // Romaji -> hiragana (and katakana) when ASCII letters are present
    if nfkc.chars().any(|ch| ch.is_ascii_alphabetic()) {
        let romaji_hira = alphabetic_to_hiragana(&nfkc);
        if romaji_hira != nfkc {
            let romaji_kata = hira_to_kata_str(&romaji_hira);
            push_variant(&mut out, &mut seen, romaji_hira);
            push_variant(&mut out, &mut seen, romaji_kata);
        }
    }

    for base_seed in base_seeds {
        if out.len() >= MAX_QUERY_VARIANTS {
            break;
        }
        let seeds: Vec<String> = expand_iteration_marks(&base_seed);
        for seed in seeds {
            if out.len() >= MAX_QUERY_VARIANTS {
                break;
            }
            let hira = kata_to_hira_str(&seed);
            let kata = hira_to_kata_str(&seed);
            push_variant(&mut out, &mut seen, seed.clone());
            push_variant(&mut out, &mut seen, hira);
            push_variant(&mut out, &mut seen, kata);

            if seed.contains('ー') {
                for expanded in expand_prolonged_vowels(&seed) {
                    if out.len() >= MAX_QUERY_VARIANTS {
                        break;
                    }
                    let exp_kata = hira_to_kata_str(&expanded);
                    push_variant(&mut out, &mut seen, expanded);
                    push_variant(&mut out, &mut seen, exp_kata);
                }
            }
        }
    }

    out
}

/// Compiles a config `search/remove-regex` pattern, returning `None` for empty or invalid regexes.
#[must_use]
pub fn compile_remove_regex(pattern: &str) -> Option<Regex> {
    if pattern.is_empty() {
        return None;
    }
    Regex::new(pattern).ok()
}

/// Applies a compiled `search/remove-regex` filter to `text`.
#[must_use]
pub fn apply_compiled_remove_regex<'a>(text: &'a str, regex: Option<&Regex>) -> Cow<'a, str> {
    match regex {
        Some(re) => re.replace_all(text, ""),
        None => Cow::Borrowed(text),
    }
}

/// Removes all substrings matching `pattern` (`search/remove-regex`).
/// Invalid or empty regex patterns safely return `text` unchanged.
#[must_use]
pub fn apply_remove_regex(text: &str, pattern: &str) -> String {
    match compile_remove_regex(pattern) {
        Some(re) => re.replace_all(text, "").into_owned(),
        None => text.to_owned(),
    }
}

/// Replaces `\n` with `replace_with` when `replace_lines` (`search/replace-lines`) is enabled.
#[must_use]
pub fn apply_replace_lines(text: &str, replace_lines: bool, replace_with: &str) -> String {
    if !replace_lines || !text.contains('\n') {
        return text.to_owned();
    }
    text.replace('\n', replace_with)
}

/// Applies Subomi's `search/remove-regex` followed by `search/replace-lines` (`search/replace-with`).
#[must_use]
pub fn preprocess_search_text(
    text: &str,
    remove_regex: &str,
    replace_lines: bool,
    replace_with: &str,
) -> String {
    let filtered = apply_remove_regex(text, remove_regex);
    apply_replace_lines(&filtered, replace_lines, replace_with)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nfkc_and_bidirectional_kana_variants() {
        assert_eq!(normalize_nfkc("ＡＢＣ１２３！？"), "ABC123!?");
        assert_eq!(normalize_nfkc("ﾃﾚﾋﾞ"), "テレビ");

        let from_hira = expand_query_variants("てれび");
        assert!(from_hira.contains(&"てれび".to_owned()));
        assert!(from_hira.contains(&"テレビ".to_owned()));

        let from_kata = expand_query_variants("テレビ");
        assert!(from_kata.contains(&"テレビ".to_owned()));
        assert!(from_kata.contains(&"てれび".to_owned()));

        let from_half = expand_query_variants("ﾃﾚﾋﾞ");
        assert!(from_half.contains(&"テレビ".to_owned()));
        assert!(from_half.contains(&"てれび".to_owned()));
    }

    #[test]
    fn prolonged_sound_mark_and_iteration_expansions() {
        assert!(expand_query_variants("ねー").contains(&"ねえ".to_owned()));
        assert!(expand_query_variants("すげー").contains(&"すげえ".to_owned()));
        assert!(expand_query_variants("おーきい").contains(&"おおきい".to_owned()));
        let koyu = expand_query_variants("こーゆー");
        assert!(koyu.contains(&"こういう".to_owned()));
        assert!(koyu.contains(&"こうゆう".to_owned()));

        assert!(expand_query_variants("人々").contains(&"人人".to_owned()));
        assert!(expand_query_variants("人人").contains(&"人々".to_owned()));
    }

    #[test]
    fn search_regex_and_replace_lines_helpers() {
        assert_eq!(
            preprocess_search_text("（太郎）食べる\n飲む", r"^（[^）]*）", true, " "),
            "食べる 飲む"
        );
        // Invalid regex is safely ignored:
        assert_eq!(apply_remove_regex("食べる", "[unclosed"), "食べる");
        assert_eq!(apply_replace_lines("a\nb", false, " "), "a\nb");
    }

    #[test]
    fn cjk_ideograph_covers_extensions_through_h_and_j() {
        assert!(is_cjk_ideograph('漢'));
        assert!(is_cjk_ideograph('\u{3400}')); // Ext A start
        assert!(is_cjk_ideograph('\u{4DBF}')); // Ext A end
        assert!(!is_cjk_ideograph('\u{4DC0}')); // Yijing hexagram, not CJK ideograph
        assert!(is_cjk_ideograph('\u{20000}')); // Ext B start
        assert!(is_cjk_ideograph('\u{2EBF0}')); // Ext I start
        assert!(is_cjk_ideograph('\u{31350}')); // Ext H start
        assert!(is_cjk_ideograph('\u{323AF}')); // Ext H end
        assert!(is_cjk_ideograph('\u{323B0}')); // Ext J start (Yomitan PR #2504)
        assert!(is_cjk_ideograph('\u{3347F}')); // Ext J end
        assert!(!is_cjk_ideograph('\u{33480}'));
        assert!(!is_cjk_ideograph('あ'));
        assert!(!is_cjk_ideograph('A'));

        // Multi-byte Ext H & J iteration mark folding safety (PR #2522):
        let ext_j_pair = format!("{0}{0}", '\u{323B0}');
        let ext_j_iter = format!("{0}々", '\u{323B0}');
        assert!(expand_iteration_marks(&ext_j_pair).contains(&ext_j_iter));
        assert!(expand_iteration_marks(&ext_j_iter).contains(&ext_j_pair));
    }

    #[test]
    fn collapse_emphatic_sequences_tracks_source_lengths_and_guards_1411() {
        let rep = collapse_emphatic_sequences("すっっごーーい", false);
        assert_eq!(rep.text, "すっごーい");
        assert_eq!(rep.char_source_utf16_lens, vec![1, 2, 1, 2, 1]);
        assert_eq!(rep.total_original_utf16_len(), 7);
        assert_eq!(rep.total_original_utf8_len(), "すっっごーーい".len());

        let full = collapse_emphatic_sequences("すっっごーーい", true);
        assert_eq!(full.text, "すごい");
        assert_eq!(full.char_source_utf16_lens, vec![3, 3, 1]);
        assert_eq!(full.original_utf16_len_for_chars(3), 7);
        assert_eq!(full.total_original_utf8_len(), "すっっごーーい".len());

        // Yomitan #1411 guard: `待ったんだ` MUST NOT collapse to `待たんだ` / `待たん`!
        let matta_rep = collapse_emphatic_sequences("待ったんだ", false);
        assert_eq!(matta_rep.text, "待ったんだ");
        let matta_full = collapse_emphatic_sequences("待ったんだ", true);
        assert_eq!(matta_full.text, "待ったんだ");
        let matta_multi = collapse_emphatic_sequences("待っったんだ", true);
        assert_eq!(matta_multi.text, "待ったんだ");
        assert_eq!(matta_multi.total_original_utf16_len(), 6);
        let matte_full = collapse_emphatic_sequences("待ってる", true);
        assert_eq!(matte_full.text, "待ってる");

        let yatta = collapse_emphatic_sequences("やったーーー", true);
        assert_eq!(yatta.text, "やった");
        assert_eq!(yatta.total_original_utf16_len(), 6);

        // Integrated into expand_query_variants:
        let variants = expand_query_variants("すっっごーーい");
        assert!(variants.contains(&"すっっごーーい".to_owned()));
        assert!(variants.contains(&"すっごーい".to_owned()));
        assert!(variants.contains(&"すごい".to_owned()));

        let matta_variants = expand_query_variants("待ったん");
        assert!(!matta_variants.contains(&"待たん".to_owned()));

        // Multi-byte Ext H/J char with emphatic sequence preserves exact UTF-16 surrogate len (2):
        let ext_h = format!("{}っっー", '\u{31350}');
        let ext_h_full = collapse_emphatic_sequences(&ext_h, true);
        assert_eq!(ext_h_full.text, "\u{31350}");
        assert_eq!(ext_h_full.char_source_utf16_lens, vec![5]);
        assert_eq!(ext_h_full.total_original_utf8_len(), ext_h.len());
    }

    #[test]
    fn standardize_kanji_maps_kyujitai_and_preserves_original_in_variants() {
        let pairs = [
            ("萬", "万"),
            ("國", "国"),
            ("舊", "旧"),
            ("學", "学"),
            ("體", "体"),
            ("龍", "竜"),
            ("齊", "斉"),
            ("齋", "斎"),
            ("廣", "広"),
            ("應", "応"),
            ("會", "会"),
            ("來", "来"),
            ("實", "実"),
            ("寫", "写"),
            ("寶", "宝"),
            ("當", "当"),
            ("發", "発"),
            ("繪", "絵"),
            ("聲", "声"),
            ("變", "変"),
            ("邊", "辺"),
            ("鐵", "鉄"),
            ("關", "関"),
            ("雜", "雑"),
            ("靈", "霊"),
            ("黑", "黒"),
            ("罐", "缶"),
        ];
        for (old, modern) in pairs {
            assert_eq!(standardize_kanji(old), modern, "failed for {old}");
        }
        assert_eq!(standardize_kanji("舊漢字の學習會"), "旧漢字の学習会");

        // Original `罐` MUST still be kept alongside `缶`:
        let kan_variants = expand_query_variants("罐");
        assert!(kan_variants.contains(&"罐".to_owned()));
        assert!(kan_variants.contains(&"缶".to_owned()));
    }

    #[test]
    fn alphabetic_to_hiragana_handles_hepburn_kunrei_n_sokuon_and_prolonged() {
        assert_eq!(alphabetic_to_hiragana("taberu"), "たべる");
        assert_eq!(alphabetic_to_hiragana("TABERU"), "たべる");
        assert_eq!(alphabetic_to_hiragana("nihongo"), "にほんご");
        assert_eq!(alphabetic_to_hiragana("konnichiwa"), "こんにちわ");
        assert_eq!(alphabetic_to_hiragana("minna"), "みんな");
        assert_eq!(alphabetic_to_hiragana("kan'i"), "かんい");
        assert_eq!(alphabetic_to_hiragana("kinn"), "きん");
        assert_eq!(alphabetic_to_hiragana("gakkou"), "がっこう");
        assert_eq!(alphabetic_to_hiragana("matcha"), "まっちゃ");
        assert_eq!(alphabetic_to_hiragana("kitte"), "きって");
        assert_eq!(alphabetic_to_hiragana("ra-men"), "らーめん");
        assert_eq!(alphabetic_to_hiragana("sya"), "しゃ");
        assert_eq!(alphabetic_to_hiragana("si"), "し");
        assert_eq!(alphabetic_to_hiragana("ti"), "ち");
        assert_eq!(alphabetic_to_hiragana("tu"), "つ");
        assert_eq!(alphabetic_to_hiragana("hu"), "ふ");
        assert_eq!(alphabetic_to_hiragana("tsukue"), "つくえ");
        assert_eq!(alphabetic_to_hiragana("食べる"), "食べる");

        let vars = expand_query_variants("taberu");
        assert!(vars.contains(&"taberu".to_owned()));
        assert!(vars.contains(&"たべる".to_owned()));
        assert!(vars.contains(&"タベル".to_owned()));
    }

    #[test]
    fn alphanumeric_width_and_radical_normalization() {
        let ok_vars = alphanumeric_width_variants("OK123");
        assert!(ok_vars.contains(&"OK123".to_owned()));
        assert!(ok_vars.contains(&"ＯＫ１２３".to_owned()));

        let fw_vars = alphanumeric_width_variants("Ｔシャツ");
        assert!(fw_vars.contains(&"Ｔシャツ".to_owned()));
        assert!(fw_vars.contains(&"Tシャツ".to_owned()));

        // Kangxi Radicals (U+2F00..=U+2FD5):
        assert_eq!(normalize_radical_characters("\u{2F00}\u{2F08}\u{2F54}"), "一人水");
        // CJK Radicals Supplement (U+2E80..=U+2EFF):
        assert_eq!(
            normalize_radical_characters("\u{2E9F}\u{2EA0}\u{2EC4}\u{2EDD}\u{2EE4}\u{2EF2}"),
            "母民西食鬼亀"
        );
        // Preserves non-radicals untouched:
        assert_eq!(normalize_radical_characters("\u{2F08}Ａ"), "人Ａ");

        let rad_vars = expand_query_variants("\u{2EDD}べる");
        assert!(rad_vars.contains(&"食べる".to_owned()));
    }

    #[test]
    fn leading_whitespace_trimmed_cleanly_pr_2506() {
        assert_eq!(trim_leading_whitespace("  \t\n　食べる"), "食べる");
        let vars = expand_query_variants("  \t　食べる");
        assert_eq!(vars[0], "食べる");
        assert!(!vars.iter().any(|v| v.starts_with(char::is_whitespace)));
    }
}

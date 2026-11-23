//! MeCab CSV feature parsing (IPADic and UniDic) and kana normalization helpers.

use std::borrow::Cow;
use unicode_normalization::UnicodeNormalization;

fn katakana_narrow(c: char) -> char {
    match c {
        '\u{30a1}'..='\u{30f6}' => char::from_u32(c as u32 - 0x60).unwrap(),
        other => other,
    }
}

/// NFKC normalization followed by the narrow katakana-to-hiragana shift (`0x30A1..=0x30F6` by `-0x60`).
/// Leaves iteration marks `0x30FD`/`0x30FE` untouched.
pub fn katakana_to_hiragana_str(text: &str) -> String {
    let mut normalized = String::with_capacity(text.len());
    for c in text.nfkc().map(katakana_narrow) {
        normalized.push(c);
    }
    normalized
}

/// Split a MeCab CSV feature row respecting double-quoted fields (`"1,2"`) and
/// escaped quotes (`""`).
///
/// Fast-paths unquoted rows via `row.split(',')` and borrows unescaped quoted
/// fields directly from `row`, avoiding per-column heap `String` allocations on
/// both IPADic and UniDic morphemes.
pub fn split_csv_row(row: &str) -> Vec<Cow<'_, str>> {
    let bytes = row.as_bytes();
    if !bytes.contains(&b'"') {
        return row.split(',').map(Cow::Borrowed).collect();
    }

    let mut fields = Vec::with_capacity(if bytes.len() > 40 { 30 } else { 10 });
    let mut i = 0;
    let len = bytes.len();
    loop {
        if i < len && bytes[i] == b'"' {
            let content_start = i + 1;
            let mut j = content_start;
            let mut has_escape_or_trailing = false;
            while j < len {
                if bytes[j] == b'"' {
                    if j + 1 == len || bytes[j + 1] == b',' {
                        fields.push(Cow::Borrowed(&row[content_start..j]));
                        i = j + 1;
                        break;
                    } else {
                        has_escape_or_trailing = true;
                        break;
                    }
                }
                j += 1;
            }
            if !has_escape_or_trailing {
                if j == len {
                    fields.push(Cow::Borrowed(&row[content_start..len]));
                    break;
                }
                if i < len {
                    i += 1;
                    continue;
                }
                break;
            }
        } else {
            let field_start = i;
            let mut j = i;
            let mut saw_quote = false;
            while j < len && bytes[j] != b',' {
                if bytes[j] == b'"' {
                    saw_quote = true;
                    break;
                }
                j += 1;
            }
            if !saw_quote {
                fields.push(Cow::Borrowed(&row[field_start..j]));
                if j < len {
                    i = j + 1;
                    continue;
                }
                break;
            }
        }

        // Slow path for a single field containing escaped `""` or mid-field quotes.
        let mut current = String::new();
        let mut in_quotes = false;
        let mut seg_start = i;
        while i < len {
            let b = bytes[i];
            if in_quotes {
                if b == b'"' {
                    current.push_str(&row[seg_start..i]);
                    if i + 1 < len && bytes[i + 1] == b'"' {
                        current.push('"');
                        i += 2;
                        seg_start = i;
                    } else {
                        in_quotes = false;
                        i += 1;
                        seg_start = i;
                    }
                } else {
                    i += 1;
                }
            } else if b == b'"' {
                current.push_str(&row[seg_start..i]);
                in_quotes = true;
                i += 1;
                seg_start = i;
            } else if b == b',' {
                current.push_str(&row[seg_start..i]);
                break;
            } else {
                i += 1;
            }
        }
        if i == len {
            current.push_str(&row[seg_start..len]);
            fields.push(Cow::Owned(current));
            break;
        }
        fields.push(Cow::Owned(current));
        i += 1;
    }
    fields
}

/// Returns `true` if `s` is a non-empty kana string (hiragana, katakana, or prolonged sound mark).
pub fn is_kana_field(s: &str) -> bool {
    !s.is_empty()
        && s != "*"
        && s.chars().all(|c| {
            matches!(
                c,
                '\u{3040}'..='\u{309F}'
                    | '\u{30A0}'..='\u{30FF}'
                    | '\u{FF65}'..='\u{FF9F}'
            )
        })
}

fn is_kata_vowel(c: char) -> bool {
    matches!(
        c,
        'ア' | 'イ' | 'ウ' | 'エ' | 'オ' | 'ァ' | 'ィ' | 'ゥ' | 'ェ' | 'ォ'
            | 'あ' | 'い' | 'う' | 'え' | 'お' | 'ぁ' | 'ぃ' | 'ぅ' | 'ぇ' | 'ぉ'
    )
}

fn infer_long_vowel_from_prev(prev: char) -> char {
    match prev {
        'ア' | 'カ' | 'サ' | 'タ' | 'ナ' | 'ハ' | 'マ' | 'ヤ' | 'ラ' | 'ワ' | 'ガ' | 'ザ'
        | 'ダ' | 'バ' | 'パ' | 'ァ' | 'ャ' | 'あ' | 'か' | 'さ' | 'た' | 'な' | 'は' | 'ま'
        | 'や' | 'ら' | 'わ' | 'が' | 'ざ' | 'だ' | 'ば' | 'ぱ' | 'ぁ' | 'ゃ' => 'ア',
        'イ' | 'キ' | 'シ' | 'チ' | 'ニ' | 'ヒ' | 'ミ' | 'リ' | 'ギ' | 'ジ' | 'ヂ' | 'ビ'
        | 'ピ' | 'ィ' | 'い' | 'き' | 'し' | 'ち' | 'に' | 'ひ' | 'み' | 'り' | 'ぎ' | 'じ'
        | 'ぢ' | 'び' | 'ぴ' | 'ぃ' => 'イ',
        'ウ' | 'ク' | 'ス' | 'ツ' | 'ヌ' | 'フ' | 'ム' | 'ユ' | 'ル' | 'グ' | 'ズ' | 'ヅ'
        | 'ブ' | 'プ' | 'ゥ' | 'ュ' | 'ヴ' | 'う' | 'く' | 'す' | 'つ' | 'ぬ' | 'ふ' | 'む'
        | 'ゆ' | 'る' | 'ぐ' | 'ず' | 'づ' | 'ぶ' | 'ぷ' | 'ぅ' | 'ゅ' | 'ゔ' => 'ウ',
        'エ' | 'ケ' | 'セ' | 'テ' | 'ネ' | 'ヘ' | 'メ' | 'レ' | 'ゲ' | 'ゼ' | 'デ' | 'ベ'
        | 'ペ' | 'ェ' | 'え' | 'け' | 'せ' | 'て' | 'ね' | 'へ' | 'め' | 'れ' | 'げ' | 'ぜ'
        | 'で' | 'べ' | 'ぺ' | 'ぇ' => 'イ',
        'オ' | 'コ' | 'ソ' | 'ト' | 'ノ' | 'ホ' | 'モ' | 'ヨ' | 'ロ' | 'ゴ' | 'ゾ' | 'ド'
        | 'ボ' | 'ポ' | 'ォ' | 'ョ' | 'お' | 'こ' | 'そ' | 'と' | 'の' | 'ほ' | 'も' | 'よ'
        | 'ろ' | 'ご' | 'ぞ' | 'ど' | 'ぼ' | 'ぽ' | 'ぉ' | 'ょ' => 'ウ',
        _ => 'ー',
    }
}

/// Reconciles a phonetic katakana reading (`pron`, which uses `ー` for long vowels)
/// with the orthographic lemma reading (`l_form`, which uses `ウ`/`オ`/`イ`/`エ`).
pub fn reconcile_phonetic_with_orthographic(
    surface: &str,
    pron: &str,
    l_form: Option<&str>,
) -> String {
    let p_chars: Vec<char> = pron.chars().collect();
    let l_chars: Vec<char> = l_form
        .filter(|s| is_kana_field(s))
        .map(|s| s.chars().collect())
        .unwrap_or_default();
    let surface_has_choon = surface.contains('ー');

    let mut out = Vec::with_capacity(p_chars.len());
    for (i, &pc) in p_chars.iter().enumerate() {
        if pc == 'ー' && !surface_has_choon {
            if let Some(&lc) = l_chars.get(i) {
                if is_kata_vowel(lc) {
                    out.push(lc);
                    continue;
                }
            }
            if i > 0 {
                let prev = p_chars[i - 1];
                if let Some(pos) = l_chars.iter().position(|&c| c == prev) {
                    if let Some(&lc) = l_chars.get(pos + 1) {
                        if is_kata_vowel(lc) {
                            out.push(lc);
                            continue;
                        }
                    }
                }
                out.push(infer_long_vowel_from_prev(prev));
            } else {
                out.push(pc);
            }
        } else if let Some(&lc) = l_chars.get(i) {
            match (pc, lc) {
                ('ズ', 'ヅ') => out.push('ヅ'),
                ('ジ', 'ヂ') => out.push('ヂ'),
                ('ワ', 'ハ') => out.push('ハ'),
                ('オ', 'ヲ') => out.push('ヲ'),
                ('エ', 'ヘ') => out.push('ヘ'),
                _ => out.push(pc),
            }
        } else {
            out.push(pc);
        }
    }
    out.into_iter().collect()
}

fn derive_inflected_from_lemma(
    surface: &str,
    c_form: Option<&str>,
    l_form: &str,
    lemma: Option<&str>,
) -> String {
    if surface == "来" && lemma == Some("来る") {
        if let Some(cf) = c_form {
            if cf.starts_with("未然形") {
                return "コ".to_string();
            }
            if cf.starts_with("連用形") || cf.starts_with("命令形") {
                return "キ".to_string();
            }
        }
    }
    if let Some(lem) = lemma {
        let lem_chars: Vec<char> = lem.chars().collect();
        let surf_chars: Vec<char> = surface.chars().collect();
        let lform_chars: Vec<char> = l_form.chars().collect();
        // Count trailing non-kanji okurigana on lemma
        let mut lem_oku = 0;
        while lem_oku < lem_chars.len()
            && !crate::furiganautils::is_kanji_char(lem_chars[lem_chars.len() - 1 - lem_oku])
        {
            lem_oku += 1;
        }
        let mut surf_oku = 0;
        while surf_oku < surf_chars.len()
            && !crate::furiganautils::is_kanji_char(surf_chars[surf_chars.len() - 1 - surf_oku])
        {
            surf_oku += 1;
        }
        if lem_oku > 0 && lform_chars.len() > lem_oku {
            let mut base: String = lform_chars[..lform_chars.len() - lem_oku].iter().collect();
            if surf_oku > 0 {
                let oku_str: String = surf_chars[surf_chars.len() - surf_oku..].iter().collect();
                base.push_str(&oku_str);
            }
            return base;
        }
    }
    l_form.to_string()
}

fn field_at<S: AsRef<str>>(fields: &[S], idx: usize) -> Option<&str> {
    fields.get(idx).map(S::as_ref)
}

/// Extract the best orthographic surface reading from a parsed MeCab CSV feature row,
/// supporting IPADic (9 columns) as well as UniDic (10, 17, 26, and 29+ columns).
pub fn extract_surface_reading_from_fields<S: AsRef<str>>(
    surface: &str,
    fields: &[S],
) -> String {
    if fields.len() <= 9 {
        // IPADic (pos1..4, cType, cForm, baseForm, reading, pron):
        // Column 7 is the surface reading (Column 6 is the dictionary base form!).
        if let Some(r) = field_at(fields, 7) {
            if is_kana_field(r) {
                return reconcile_phonetic_with_orthographic(surface, r, None);
            }
            if r == "*" {
                return "*".to_string();
            }
        }
        if let Some(p) = field_at(fields, 8).filter(|s| is_kana_field(s)) {
            return reconcile_phonetic_with_orthographic(surface, p, None);
        }
        return String::new();
    }

    // UniDic (>= 10 columns):
    // 0..3: pos1..4, 4: cType, 5: cForm, 6: lForm, 7: lemma, 8: orth, 9: pron
    let c_form = field_at(fields, 5);
    let l_form = field_at(fields, 6).filter(|s| is_kana_field(s));
    let lemma = field_at(fields, 7);
    let pron = field_at(fields, 9).filter(|s| is_kana_field(s));

    // In 29-col UniDic, column 20 is `kana` (inflected orthographic kana).
    // In 26-col UniDic, column 17 is `kana` (inflected orthographic kana).
    let orth_kana_indices: &[usize] = if fields.len() >= 28 {
        &[20, 17, 18]
    } else if fields.len() >= 18 {
        &[17, 20, 18]
    } else {
        &[]
    };

    for &idx in orth_kana_indices {
        if let Some(k) = field_at(fields, idx).filter(|s| is_kana_field(s)) {
            // Only accept uninflected fallback columns (18, or 20 in 26-col) if `pron` is absent
            // or has the same char count as `k`.
            let is_primary_kana_col =
                (fields.len() >= 28 && idx == 20) || (fields.len() < 28 && idx == 17);
            if is_primary_kana_col
                || pron.is_none_or(|p| p.chars().count() == k.chars().count())
            {
                return reconcile_phonetic_with_orthographic(surface, k, l_form);
            }
        }
    }

    // 10-col / 17-col UniDic: reconcile inflected `pron` (col 9) with orthographic `lForm` (col 6).
    if let Some(p) = pron {
        return reconcile_phonetic_with_orthographic(surface, p, l_form);
    }

    if let Some(lf) = l_form {
        return derive_inflected_from_lemma(surface, c_form, lf, lemma);
    }

    if let Some(r) = field_at(fields, 7).filter(|s| is_kana_field(s)) {
        return reconcile_phonetic_with_orthographic(surface, r, None);
    }

    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn katakana_to_hiragana_str_handles_range_nfkc_and_iteration_marks() {
        assert_eq!(katakana_to_hiragana_str("カタカナ"), "かたかな");
        assert_eq!(katakana_to_hiragana_str("ヴ"), "ゔ");
        assert_eq!(katakana_to_hiragana_str("\u{30f6}"), "\u{3096}");
        assert_eq!(katakana_to_hiragana_str("\u{30a0}"), "\u{30a0}");
        assert_eq!(katakana_to_hiragana_str("ｱｲｳｴｵ"), "あいうえお");
        assert_eq!(katakana_to_hiragana_str("ｶﾞﾊﾟ"), "がぱ");
        assert_eq!(katakana_to_hiragana_str("日本語😀"), "日本語😀");
        // Iteration marks and 0x30A0 are not shifted here.
        assert_eq!(
            katakana_to_hiragana_str("\u{30a0}\u{30fd}\u{30fe}\u{309d}"),
            "\u{30a0}\u{30fd}\u{30fe}\u{309d}"
        );
    }

    #[test]
    fn split_csv_row_fast_path_and_quoted_cases() {
        // Unquoted IPADic row: every field is zero-copy borrowed.
        let ipadic = "動詞,自立,*,*,一段,基本形,食べる,タベル,タベル";
        let fields = split_csv_row(ipadic);
        assert_eq!(fields.len(), 9);
        assert!(fields.iter().all(|f| matches!(f, Cow::Borrowed(_))));
        assert_eq!(extract_surface_reading_from_fields("食べる", &fields), "タベル");

        // 29-column UniDic row with empty/simple quoted columns: all 29 fields borrowed!
        let unidic = "動詞,非自立可能,,,サ行変格,連用形-一般,スル,為る-動詞,し,シ,する,スル,和,\"\",\"\",\"\",\"\",\"\",\"\",用,シ,スル,シ,スル,\"0\",\"C5\",\"\",1,1";
        let ufields = split_csv_row(unidic);
        assert_eq!(ufields.len(), 29);
        assert!(ufields.iter().all(|f| matches!(f, Cow::Borrowed(_))));
        assert_eq!(ufields[0], "動詞");
        assert_eq!(ufields[13], "");
        assert_eq!(ufields[24], "0");
        assert_eq!(ufields[25], "C5");
        assert_eq!(ufields[26], "");
        assert_eq!(ufields[28], "1");
        assert_eq!(extract_surface_reading_from_fields("し", &ufields), "シ");

        // Quoted comma, escaped quotes, trailing commas, empty string, mid-field quotes.
        let quoted = split_csv_row("a,\"1,2\",\"x\"\"y\",\"\",");
        let as_strs: Vec<&str> = quoted.iter().map(|f| &**f).collect();
        assert_eq!(as_strs, vec!["a", "1,2", "x\"y", "", ""]);
        assert!(matches!(quoted[1], Cow::Borrowed(_)));
        assert!(matches!(quoted[2], Cow::Owned(_)));

        let empty = split_csv_row("");
        assert_eq!(empty.len(), 1);
        assert_eq!(empty[0], "");

        let four_quotes = split_csv_row("\"\"\"\"");
        assert_eq!(four_quotes.len(), 1);
        assert_eq!(four_quotes[0], "\"");

        let mid = split_csv_row("a\"b,c\"d,e");
        let mid_strs: Vec<&str> = mid.iter().map(|f| &**f).collect();
        assert_eq!(mid_strs, vec!["ab,cd", "e"]);
    }
}

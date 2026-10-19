////////////////////////////////////////////////////////////////////////////////
//
// Copyright (c) 2026 Ripose
//
// This file is part of Memento.
//
// Memento is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, version 2 of the License.
//
// Memento is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with Memento.  If not, see <https://www.gnu.org/licenses/>.
//
////////////////////////////////////////////////////////////////////////////////

//! Pure port of the fallback pieces of `src/quick/furiganaparser.cpp`:
//! `FuriganaParser::katakanaToHiragana` and `FuriganaParser::getFallbackResult`.
//!
//! Important distinction: `FuriganaParser::katakanaToHiragana` normalizes with
//! `QString::normalized(NFKC)` and maps *only* the contiguous katakana range
//! `0x30A1–0x30F6`. The separate `FuriganaUtils` (this crate's
//! [`furiganautils`]) normalization adds the iteration marks
//! `0x30FD→0x309D`/`0x30FE→0x309E`, which this function must **not** do.

use unicode_normalization::UnicodeNormalization;

fn katakana_narrow(c: char) -> char {
    match c {
        '\u{30a1}'..='\u{30f6}' => char::from_u32(c as u32 - 0x60).unwrap(),
        other => other,
    }
}

/// One NFKC pass plus the narrow katakana mapping, in QString order.
fn normalize_run(text: &str) -> Vec<u16> {
    let mut normalized = Vec::with_capacity(text.len());
    for c in text.nfkc().map(katakana_narrow) {
        let mut units = [0; 2];
        normalized.extend_from_slice(c.encode_utf16(&mut units));
    }
    normalized
}

/// Normalize well-formed UTF-16 runs separately while passing isolated
/// surrogate code units through unchanged.
fn normalize_malformed(text: &[u16]) -> Vec<u16> {
    let mut normalized = Vec::with_capacity(text.len());
    let mut run_start = 0;
    let mut i = 0;
    while i < text.len() {
        let code = text[i];
        if (0xd800..=0xdbff).contains(&code)
            && text
                .get(i + 1)
                .is_some_and(|&low| (0xdc00..=0xdfff).contains(&low))
        {
            i += 2;
            continue;
        }
        if (0xd800..=0xdfff).contains(&code) {
            let run = &text[run_start..i];
            if !run.is_empty() {
                let decoded = String::from_utf16(run).expect("run contains valid UTF-16");
                normalized.extend(normalize_run(&decoded));
            }
            normalized.push(code);
            i += 1;
            run_start = i;
        } else {
            i += 1;
        }
    }
    let run = &text[run_start..];
    if !run.is_empty() {
        let decoded = String::from_utf16(run).expect("run contains valid UTF-16");
        normalized.extend(normalize_run(&decoded));
    }
    normalized
}

/// `FuriganaParser::katakanaToHiragana`: NFKC followed by the
/// `0x30A1–0x30F6` (inclusive) shift by `0x60`. Isolated surrogate units
/// are preserved; behavior for malformed QString input needs Qt comparison.
pub fn katakana_to_hiragana(text: &[u16]) -> Vec<u16> {
    match String::from_utf16(text) {
        Ok(decoded) => normalize_run(&decoded),
        Err(_) => normalize_malformed(text),
    }
}

/// `FuriganaParser::getFallbackResult` uses the unchanged surface as its
/// sole QVariantMap element. A Rust caller can borrow the original UTF-16
/// units until it constructs the corresponding output.
pub fn fallback_surface(text: &[u16]) -> &[u16] {
    text
}

/// A borrowed view of a MeCab lattice node for furigana parsing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FuriganaNodeSlice<'a> {
    pub whitespace: &'a [u8],
    pub surface: &'a [u8],
    pub feature: &'a [u8],
}

/// A parsed token with surface UTF-16 code units and optional reading UTF-16 code units.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParsedToken {
    pub surface: Vec<u16>,
    pub reading: Option<Vec<u16>>,
}

fn utf8_to_utf16(bytes: &[u8]) -> Vec<u16> {
    String::from_utf8_lossy(bytes).encode_utf16().collect()
}

fn extract_reading(feature_bytes: &[u8]) -> Vec<u16> {
    let feature_str = String::from_utf8_lossy(feature_bytes);
    let mut parts = feature_str.split(',');
    match parts.nth(7) {
        Some(reading) => reading.encode_utf16().collect(),
        None => Vec::new(),
    }
}

/// Full `FuriganaParser::parse` token-chain processing over a sequence of MeCab nodes.
pub fn parse_nodes(nodes: &[FuriganaNodeSlice<'_>]) -> Vec<ParsedToken> {
    let mut tokens = Vec::new();

    for node in nodes {
        if !node.whitespace.is_empty() {
            tokens.push(ParsedToken {
                surface: utf8_to_utf16(node.whitespace),
                reading: None,
            });
        }

        let surface_u16 = utf8_to_utf16(node.surface);
        let reading_u16 = extract_reading(node.feature);

        let surface_hiragana = katakana_to_hiragana(&surface_u16);
        let reading_hiragana = katakana_to_hiragana(&reading_u16);

        let is_asterisk = reading_hiragana.len() == 1 && reading_hiragana[0] == b'*' as u16;

        if is_asterisk || surface_hiragana == reading_hiragana {
            tokens.push(ParsedToken {
                surface: surface_u16,
                reading: None,
            });
        } else {
            let spans = crate::furiganautils::simplify_surface(&surface_u16, &reading_hiragana);
            for span in spans {
                let surf_start = span.surface_start as usize;
                let surf_end = span.surface_end as usize;
                let span_surface = surface_u16.get(surf_start..surf_end).unwrap_or(&[]).to_vec();

                let span_reading = if span.reading_start < span.reading_end {
                    let read_start = span.reading_start as usize;
                    let read_end = span.reading_end as usize;
                    reading_hiragana.get(read_start..read_end).map(|s| s.to_vec())
                } else {
                    None
                };

                tokens.push(ParsedToken {
                    surface: span_surface,
                    reading: span_reading,
                });
            }
        }
    }

    tokens
}

#[cfg(test)]
mod tests {
    use super::*;

    fn convert(text: &str) -> String {
        String::from_utf16_lossy(&katakana_to_hiragana(&text.encode_utf16().collect::<Vec<_>>()))
    }

    #[test]
    fn katakana_maps_to_hiragana_inclusively() {
        assert_eq!(convert("カタカナ"), "かたかな");
        // 0x30F4 → 0x3094
        assert_eq!(convert("ヴ"), "ゔ");
        // 0x30F6 (upper bound) → 0x3096
        assert_eq!(katakana_to_hiragana(&[0x30f6]), [0x3096]);
        // 0x30A0 lies outside the inclusive range.
        assert_eq!(katakana_to_hiragana(&[0x30a0]), [0x30a0]);
    }

    #[test]
    fn nfkc_normalizes_before_the_mapping() {
        // Half-width katakana normalizes to full width first.
        assert_eq!(convert("ｱｲｳｴｵ"), "あいうえお");
        // A half-width voiced cluster collapses to one hiragana character.
        assert_eq!(convert("ｶﾞ"), "が");
        // ﾊﾟ (0xFF8A, 0xFF9F) NFKCs to composed 0x30D1 (in range) → 0x3071.
        assert_eq!(
            katakana_to_hiragana(&[0xff8a, 0xff9f]),
            [0x3071]
        );
    }

    #[test]
    fn iteration_marks_are_not_mapped_here() {
        // FuriganaParser::katakanaToHiragana keeps 0x30FD/0x30FE katakana
        // marks; only FuriganaUtils normalizes those.
        assert_eq!(
            katakana_to_hiragana(&[0x30fd]),
            [0x30fd]
        );
        assert_eq!(
            katakana_to_hiragana(&[0x30fe]),
            [0x30fe]
        );
        // Hiragana marks are untouched too.
        assert_eq!(katakana_to_hiragana(&[0x309d]), [0x309d]);
    }

    #[test]
    fn isolated_surrogates_pass_through_normalization() {
        assert_eq!(katakana_to_hiragana(&[0xd800]), [0xd800]);
        assert_eq!(
            katakana_to_hiragana(&[0x30ac, 0xd800, 0x30d5]),
            [0x304c, 0xd800, 0x3075]
        );
        // Valid pairs stay paired.
        assert_eq!(
            katakana_to_hiragana(&[0xd83d, 0xde00]),
            [0xd83d, 0xde00]
        );
    }

    #[test]
    fn fallback_surface_is_the_unchanged_text() {
        assert_eq!(fallback_surface(&[]), &[] as &[u16]);
        assert_eq!(fallback_surface(&[0x3042]), [0x3042]);
        assert_eq!(fallback_surface(&[0xd800, 0x30ac]), [0xd800, 0x30ac]);
        assert_eq!(
            fallback_surface(&"日本語😀".encode_utf16().collect::<Vec<_>>()),
            "日本語😀".encode_utf16().collect::<Vec<_>>()
        );
    }

    #[test]
    fn parse_nodes_token_chain() {
        let nodes = [
            FuriganaNodeSlice {
                whitespace: b"  ",
                surface: "食べる".as_bytes(),
                feature: "動詞,自立,*,*,一段,基本形,食べる,タベル,タベル".as_bytes(),
            },
            FuriganaNodeSlice {
                whitespace: b"",
                surface: "人".as_bytes(),
                feature: "名詞,一般,*,*,*,*,人,ヒト,ヒト".as_bytes(),
            },
            FuriganaNodeSlice {
                whitespace: b" ",
                surface: "カタカナ".as_bytes(),
                feature: "名詞,一般,*,*,*,*,カタカナ,カタカナ,カタカナ".as_bytes(),
            },
            FuriganaNodeSlice {
                whitespace: b"",
                surface: "記号".as_bytes(),
                feature: "名詞,一般,*,*,*,*,記号,*,*".as_bytes(),
            },
        ];

        let tokens = parse_nodes(&nodes);

        let decode = |tok: &ParsedToken| {
            (
                String::from_utf16_lossy(&tok.surface),
                tok.reading.as_ref().map(|r| String::from_utf16_lossy(r)),
            )
        };

        let decoded: Vec<_> = tokens.iter().map(decode).collect();
        assert_eq!(
            decoded,
            vec![
                ("  ".to_string(), None),
                ("食".to_string(), Some("た".to_string())),
                ("べる".to_string(), None),
                ("人".to_string(), Some("ひと".to_string())),
                (" ".to_string(), None),
                ("カタカナ".to_string(), None),
                ("記号".to_string(), None),
            ]
        );
    }
}

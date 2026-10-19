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

use unicode_normalization::UnicodeNormalization;
use unicode_segmentation::UnicodeSegmentation;

/// Half-open offsets into the original UTF-16 surface and reading buffers.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FuriganaSpan {
    pub surface_start: u64,
    pub surface_end: u64,
    pub reading_start: u64,
    pub reading_end: u64,
}

struct Segment<T> {
    anchor: bool,
    start: u64,
    end: u64,
    normalized: T,
}

struct ReadingData<T> {
    // Both offset arrays have one entry for each grapheme boundary, including
    // the final boundary. Original offsets count UTF-16 units; normalized
    // offsets count elements of T (UTF-8 bytes or UTF-16 code units).
    original_offsets: Vec<u64>,
    normalized_offsets: Vec<usize>,
    normalized: T,
}

struct MalformedUnit {
    start: u64,
    end: u64,
    normalized: Vec<u16>,
}

struct MalformedText {
    units: Vec<MalformedUnit>,
    normalized_whole: Vec<u16>,
}

#[derive(Clone, Copy, Default)]
struct SolutionState {
    // Number of alignments for this suffix, capped at two.
    count: u8,
    next_reading_unit_index: Option<usize>,
}

fn kata_to_hira(c: char) -> char {
    match c {
        '\u{30a1}'..='\u{30f6}' => char::from_u32(c as u32 - 0x60).unwrap(),
        '\u{30fd}' => '\u{309d}',
        '\u{30fe}' => '\u{309e}',
        _ => c,
    }
}

// QString::normalized(NFKC) runs before the katakana-to-hiragana conversion.
fn normalize_text(text: &str) -> String {
    text.nfkc().map(kata_to_hira).collect()
}

fn make_reading_data(reading: &str) -> ReadingData<String> {
    let mut original_offsets = vec![0];
    let mut normalized_offsets = vec![0];
    let mut normalized = String::with_capacity(reading.len());
    let mut original_end = 0;

    for unit in reading.graphemes(true) {
        original_end += unit.encode_utf16().count() as u64;
        original_offsets.push(original_end);
        normalized.extend(unit.nfkc().map(kata_to_hira));
        normalized_offsets.push(normalized.len());
    }

    ReadingData {
        original_offsets,
        normalized_offsets,
        normalized,
    }
}

fn make_segments(surface: &str, normalized_reading: &str) -> Vec<Segment<String>> {
    let mut segments: Vec<Segment<String>> = Vec::new();
    let mut original_end = 0;

    for unit in surface.graphemes(true) {
        let start = original_end;
        original_end += unit.encode_utf16().count() as u64;
        let normalized = normalize_text(unit);
        let anchor = !normalized.is_empty() && normalized_reading.contains(normalized.as_str());

        if let Some(last) = segments.last_mut() {
            if last.anchor == anchor {
                last.end = original_end;
                last.normalized.push_str(&normalized);
                continue;
            }
        }
        segments.push(Segment {
            anchor,
            start,
            end: original_end,
            normalized,
        });
    }

    segments
}

// QString::normalized preserves isolated surrogate code units. Normalize the
// valid runs between them and keep the original UTF-16 representation for
// QString-style comparison (including substring matches within surrogate
// pairs); no replacement characters or comparison sentinels are involved.
fn append_valid_normalized(text: &[u16], normalized: &mut Vec<u16>) {
    if !text.is_empty() {
        let decoded = String::from_utf16(text).expect("run contains valid UTF-16");
        normalized.extend(normalize_text(&decoded).encode_utf16());
    }
}

fn normalize_malformed_text(text: &[u16]) -> Vec<u16> {
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
            append_valid_normalized(&text[run_start..i], &mut normalized);
            normalized.push(code);
            i += 1;
            run_start = i;
        } else {
            i += 1;
        }
    }
    append_valid_normalized(&text[run_start..], &mut normalized);
    normalized
}

fn make_malformed_text(text: &[u16]) -> MalformedText {
    let normalized_whole = normalize_malformed_text(text);
    let mut boundary_text = String::new();
    let mut original_offsets = vec![0u64];
    for codepoint in std::char::decode_utf16(text.iter().copied()) {
        // An unpaired surrogate has Grapheme_Cluster_Break=Other in Qt.
        // Private-use U+E000 has the same break property and is used *only*
        // for boundary detection; all normalization/comparison above and
        // below uses the original, unsanitized UTF-16 code units instead.
        let c = codepoint.unwrap_or('\u{e000}');
        boundary_text.push(c);
        original_offsets.push(original_offsets.last().unwrap() + c.len_utf16() as u64);
    }
    let mut units = Vec::new();
    let mut char_index = 0;
    for grapheme in boundary_text.graphemes(true) {
        let start = original_offsets[char_index];
        char_index += grapheme.chars().count();
        let end = original_offsets[char_index];
        units.push(MalformedUnit {
            start,
            end,
            normalized: normalize_malformed_text(&text[start as usize..end as usize]),
        });
    }
    MalformedText {
        units,
        normalized_whole,
    }
}

fn make_malformed_reading_data(units: &[MalformedUnit]) -> ReadingData<Vec<u16>> {
    let mut original_offsets = Vec::with_capacity(units.len() + 1);
    let mut normalized_offsets = Vec::with_capacity(units.len() + 1);
    let mut normalized = Vec::new();
    original_offsets.push(0);
    normalized_offsets.push(0);
    for unit in units {
        original_offsets.push(unit.end);
        normalized.extend_from_slice(&unit.normalized);
        normalized_offsets.push(normalized.len());
    }
    ReadingData {
        original_offsets,
        normalized_offsets,
        normalized,
    }
}

fn make_malformed_segments(units: &[MalformedUnit], reading: &[u16]) -> Vec<Segment<Vec<u16>>> {
    let mut segments: Vec<Segment<Vec<u16>>> = Vec::new();
    for unit in units {
        let anchor = !unit.normalized.is_empty()
            && reading
                .windows(unit.normalized.len())
                .any(|window| window == unit.normalized.as_slice());
        if let Some(last) = segments.last_mut() {
            if last.anchor == anchor {
                last.end = unit.end;
                last.normalized.extend_from_slice(&unit.normalized);
                continue;
            }
        }
        segments.push(Segment {
            anchor,
            start: unit.start,
            end: unit.end,
            normalized: unit.normalized.clone(),
        });
    }
    segments
}

fn simplify_surface_malformed(surface: &[u16], reading: &[u16]) -> Vec<FuriganaSpan> {
    let surface_text = make_malformed_text(surface);
    let reading_text = make_malformed_text(reading);
    if surface_text.normalized_whole == reading_text.normalized_whole {
        return vec![FuriganaSpan {
            surface_start: 0,
            surface_end: surface.len() as u64,
            reading_start: 0,
            reading_end: 0,
        }];
    }

    let reading_data = make_malformed_reading_data(&reading_text.units);
    let segments = make_malformed_segments(&surface_text.units, &reading_data.normalized);
    align_segments(&segments, &reading_data, surface.len() as u64, reading.len() as u64)
}

// An anchor must match *whole reading graphemes*: NFKC may expand one
// grapheme into several code points, but no alignment may split that unit.
fn match_anchor_at<T: AsRef<[E]>, E: PartialEq>(
    reading: &ReadingData<T>,
    unit_index: usize,
    anchor: &T,
) -> Option<usize> {
    let anchor = anchor.as_ref();
    if anchor.is_empty() {
        return None;
    }
    let start = *reading.normalized_offsets.get(unit_index)?;
    let end = start.checked_add(anchor.len())?;
    if reading.normalized.as_ref().get(start..end)? != anchor {
        return None;
    }

    let later_offsets = &reading.normalized_offsets[unit_index..];
    let end_index = unit_index + later_offsets.partition_point(|&offset| offset < end);
    (reading.normalized_offsets.get(end_index) == Some(&end)).then_some(end_index)
}

fn make_solution_table<T: AsRef<[E]>, E: PartialEq>(
    segments: &[Segment<T>],
    reading: &ReadingData<T>,
) -> Vec<Vec<SolutionState>> {
    let reading_units = reading.original_offsets.len() - 1;
    let mut table = vec![vec![SolutionState::default(); reading_units + 1]; segments.len() + 1];
    table[segments.len()][reading_units].count = 1;

    // For each following segment, suffix_counts[j] counts all alignments
    // starting at any reading boundary >= j. This permits non-anchor segments
    // to consume at least one grapheme in O(1) per DP state. Only the next
    // row is needed; reconstruction uses the states kept in `table`.
    let mut suffix_counts = vec![0u8; reading_units + 2];
    let mut suffix_first_indices = vec![None; reading_units + 2];
    suffix_counts[..=reading_units].fill(1);
    suffix_first_indices[..=reading_units].fill(Some(reading_units));

    for i in (0..segments.len()).rev() {
        for j in (0..=reading_units).rev() {
            if segments[i].anchor {
                if let Some(end) = match_anchor_at(reading, j, &segments[i].normalized) {
                    let count = table[i + 1][end].count;
                    table[i][j] = SolutionState {
                        count,
                        next_reading_unit_index: (count == 1).then_some(end),
                    };
                }
            } else {
                let count = suffix_counts[j + 1];
                table[i][j] = SolutionState {
                    count,
                    next_reading_unit_index: if count == 1 {
                        suffix_first_indices[j + 1]
                    } else {
                        None
                    },
                };
            }
        }

        for j in (0..=reading_units).rev() {
            suffix_counts[j] = (table[i][j].count + suffix_counts[j + 1]).min(2);
            suffix_first_indices[j] = if table[i][j].count != 0 {
                Some(j)
            } else {
                suffix_first_indices[j + 1]
            };
        }
    }

    table
}

fn align_segments<T: AsRef<[E]>, E: PartialEq>(
    segments: &[Segment<T>],
    reading: &ReadingData<T>,
    surface_len: u64,
    reading_len: u64,
) -> Vec<FuriganaSpan> {
    let table = make_solution_table(segments, reading);
    if table[0][0].count != 1 {
        return fallback_result(surface_len, reading_len);
    }

    let mut solution = Vec::with_capacity(segments.len());
    let mut reading_unit_index = 0;
    for (i, segment) in segments.iter().enumerate() {
        let next = table[i][reading_unit_index]
            .next_reading_unit_index
            .expect("a unique alignment has a next reading boundary");
        let (reading_start, reading_end) = if segment.anchor {
            (0, 0)
        } else {
            (
                reading.original_offsets[reading_unit_index],
                reading.original_offsets[next],
            )
        };
        solution.push(FuriganaSpan {
            surface_start: segment.start,
            surface_end: segment.end,
            reading_start,
            reading_end,
        });
        reading_unit_index = next;
    }
    solution
}

fn fallback_result(surface_len: u64, reading_len: u64) -> Vec<FuriganaSpan> {
    vec![FuriganaSpan {
        surface_start: 0,
        surface_end: surface_len,
        reading_start: 0,
        reading_end: reading_len,
    }]
}

/// Simplify a surface and reading into uniquely aligned spans.
///
/// Both inputs use the UTF-16 encoding of their original strings. If there is
/// no unique alignment, the result is one span containing both full inputs.
/// Matched surface anchors always have the empty reading range `0..0`.
/// Isolated UTF-16 surrogates remain distinct from U+FFFD during comparison.
pub fn simplify_surface(surface: &[u16], reading: &[u16]) -> Vec<FuriganaSpan> {
    // Decode each input just once on the all-valid path. Only malformed input
    // needs the UTF-16-preserving representation used by QString.
    match (String::from_utf16(surface), String::from_utf16(reading)) {
        (Ok(surface_text), Ok(reading_text)) => simplify_surface_valid(
            &surface_text,
            &reading_text,
            surface.len() as u64,
            reading.len() as u64,
        ),
        _ => simplify_surface_malformed(surface, reading),
    }
}

fn simplify_surface_valid(
    surface_text: &str, reading_text: &str, surface_len: u64, reading_len: u64,
) -> Vec<FuriganaSpan> {
    // Match normalized whole strings before normalizing separate graphemes.
    if surface_text
        .nfkc()
        .map(kata_to_hira)
        .eq(reading_text.nfkc().map(kata_to_hira))
    {
        return vec![FuriganaSpan {
            surface_start: 0,
            surface_end: surface_len,
            reading_start: 0,
            reading_end: 0,
        }];
    }

    let reading_data = make_reading_data(&reading_text);
    let segments = make_segments(&surface_text, &reading_data.normalized);
    align_segments(&segments, &reading_data, surface_len, reading_len)
}

#[cfg(test)]
mod tests {
    use super::{simplify_surface, FuriganaSpan};

    fn span(surface_start: u64, surface_end: u64, reading_start: u64, reading_end: u64) -> FuriganaSpan {
        FuriganaSpan {
            surface_start,
            surface_end,
            reading_start,
            reading_end,
        }
    }

    fn simplify(surface: &str, reading: &str) -> Vec<FuriganaSpan> {
        simplify_surface(
            &surface.encode_utf16().collect::<Vec<_>>(),
            &reading.encode_utf16().collect::<Vec<_>>(),
        )
    }

    #[test]
    fn whole_text_nfkc_kana_and_iteration_marks_are_equivalent() {
        assert_eq!(simplify("ｶﾀｶﾅ", "かたかな"), vec![span(0, 4, 0, 0)]);
        assert_eq!(simplify("カヽ", "かゝ"), vec![span(0, 2, 0, 0)]);
        assert_eq!(simplify("カヾ", "かゞ"), vec![span(0, 2, 0, 0)]);
        assert_eq!(simplify("", ""), vec![span(0, 0, 0, 0)]);
    }

    #[test]
    fn unique_kanji_and_kana_segments_keep_original_offsets() {
        assert_eq!(
            simplify("食べ物", "たべもの"),
            vec![span(0, 1, 0, 1), span(1, 2, 0, 0), span(2, 3, 2, 4)]
        );
    }

    #[test]
    fn combining_grapheme_and_supplementary_character_span_utf16_units() {
        assert_eq!(
            simplify("縞か\u{3099}糸", "しまがいと"),
            vec![span(0, 1, 0, 2), span(1, 3, 0, 0), span(3, 4, 3, 5)]
        );
        assert_eq!(
            simplify("字𠀋語", "じ𠀋ご"),
            vec![span(0, 1, 0, 1), span(1, 3, 0, 0), span(3, 4, 3, 4)]
        );
    }

    #[test]
    fn nfkc_expansion_matches_only_at_reading_grapheme_boundaries() {
        assert_eq!(
            simplify("㍿者", "株式会社しゃ"),
            vec![span(0, 1, 0, 0), span(1, 2, 4, 6)]
        );
        assert_eq!(simplify("株者", "㍿しゃ"), vec![span(0, 2, 0, 3)]);
    }

    #[test]
    fn ambiguous_missing_and_noncontiguous_anchors_fall_back() {
        assert_eq!(simplify("字あ字", "かああか"), vec![span(0, 3, 0, 4)]);
        assert_eq!(simplify("字あ字", "あか"), vec![span(0, 3, 0, 2)]);
        assert_eq!(simplify("あい", "あかい"), vec![span(0, 2, 0, 3)]);
        assert_eq!(simplify("漢あ", "あ"), vec![span(0, 2, 0, 1)]);
        assert_eq!(simplify("", "あ"), vec![span(0, 0, 0, 1)]);
    }

    #[test]
    fn isolated_surrogates_are_distinct_from_replacement_and_each_other() {
        for code in [0xd800, 0xdbff, 0xdc00, 0xdfff] {
            assert_eq!(simplify_surface(&[code], &[code]), vec![span(0, 1, 0, 0)]);
            assert_eq!(simplify_surface(&[code], &[0xfffd]), vec![span(0, 1, 0, 1)]);
        }
        assert_eq!(simplify_surface(&[0xd800], &[0xdc00]), vec![span(0, 1, 0, 1)]);
        assert_eq!(simplify_surface(&[0xd800, 0xd800], &[0xd800, 0xd800]), vec![span(0, 2, 0, 0)]);
    }

    #[test]
    fn malformed_graphemes_preserve_normalized_anchor_boundaries() {
        let surface = [0x6f22, 0xd800, 0x0301, 0x5b57]; // 漢, orphan, accent, 字
        let reading = [0x304b, 0x3093, 0xd800, 0x0301, 0x3058]; // かん, orphan, accent, じ
        assert_eq!(
            simplify_surface(&surface, &reading),
            vec![span(0, 1, 0, 2), span(1, 3, 0, 0), span(3, 4, 4, 5)]
        );
        assert_eq!(
            simplify_surface(&surface, &[0x304b, 0x3093, 0xd800, 0x0300, 0x3058]),
            vec![span(0, 4, 0, 5)]
        );
        assert_eq!(
            simplify_surface(&surface, &[0x304b, 0x3093, 0xd800, 0x3058]),
            vec![span(0, 4, 0, 4)]
        );
        assert_eq!(
            simplify_surface(
                &[0x6f22, 0xd800, 0x5b57],
                &[0x304b, 0x3093, 0xd800, 0x3058]
            ),
            vec![span(0, 1, 0, 2), span(1, 2, 0, 0), span(2, 3, 3, 4)]
        );
        assert_eq!(
            simplify_surface(
                &[0x6f22, 0xd800, 0xff76, 0xff9e, 0x5b57],
                &[0x304b, 0x3093, 0xd800, 0x304c, 0x3058]
            ),
            vec![span(0, 1, 0, 2), span(1, 4, 0, 0), span(4, 5, 4, 5)]
        );
    }

    #[test]
    fn malformed_input_does_not_collide_with_valid_private_use_or_non_bmp() {
        assert_eq!(simplify_surface(&[0xd800], &[0xe000]), vec![span(0, 1, 0, 1)]);
        assert_eq!(simplify_surface(&[0xd800], &[0xd800, 0xdc00]), vec![span(0, 1, 0, 2)]);
        assert_eq!(
            simplify_surface(
                &[0x6f22, 0xd800, 0xe000, 0x5b57],
                &[0x304b, 0x3093, 0xd800, 0xe000, 0x3058]
            ),
            vec![span(0, 1, 0, 2), span(1, 3, 0, 0), span(3, 4, 4, 5)]
        );
        assert_eq!(
            simplify_surface(
                &[0x6f22, 0xd800, 0xdc00, 0xd800, 0x5b57],
                &[0x304b, 0x3093, 0xd800, 0xdc00, 0xd800, 0x3058]
            ),
            vec![span(0, 1, 0, 2), span(1, 4, 0, 0), span(4, 5, 5, 6)]
        );
    }
}

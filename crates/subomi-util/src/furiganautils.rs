use unicode_normalization::UnicodeNormalization;
use unicode_segmentation::UnicodeSegmentation;

/// Half-open UTF-8 byte offsets into the original `surface` and `reading`
/// strings, guaranteed to lie on valid grapheme boundaries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FuriganaSpan {
    pub surface_start: usize,
    pub surface_end: usize,
    pub reading_start: usize,
    pub reading_end: usize,
}

struct Segment {
    anchor: bool,
    start: usize,
    end: usize,
    normalized: String,
}

struct ReadingData {
    original_offsets: Vec<usize>,
    normalized_offsets: Vec<usize>,
    normalized: String,
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

fn normalize_text(text: &str) -> String {
    text.nfkc().map(kata_to_hira).collect()
}

fn make_reading_data(reading: &str) -> ReadingData {
    let mut original_offsets = vec![0];
    let mut normalized_offsets = vec![0];
    let mut normalized = String::with_capacity(reading.len());
    let mut original_end = 0usize;

    for unit in reading.graphemes(true) {
        original_end += unit.len();
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

fn make_segments(surface: &str, normalized_reading: &str) -> Vec<Segment> {
    let mut segments: Vec<Segment> = Vec::new();
    let mut original_end = 0usize;

    for unit in surface.graphemes(true) {
        let start = original_end;
        original_end += unit.len();
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

// An anchor must match *whole reading graphemes*: NFKC may expand one
// grapheme into several code points, but no alignment may split that unit.
fn match_anchor_at(reading: &ReadingData, unit_index: usize, anchor: &str) -> Option<usize> {
    if anchor.is_empty() {
        return None;
    }
    let start = *reading.normalized_offsets.get(unit_index)?;
    let end = start.checked_add(anchor.len())?;
    if reading.normalized.get(start..end)? != anchor {
        return None;
    }

    let later_offsets = &reading.normalized_offsets[unit_index..];
    let end_index = unit_index + later_offsets.partition_point(|&offset| offset < end);
    (reading.normalized_offsets.get(end_index) == Some(&end)).then_some(end_index)
}

struct SolutionTable {
    cols: usize,
    cells: Vec<SolutionState>,
}

impl SolutionTable {
    fn new(rows: usize, cols: usize) -> Self {
        Self {
            cols,
            cells: vec![SolutionState::default(); rows * cols],
        }
    }

    #[inline]
    fn get(&self, row: usize, col: usize) -> SolutionState {
        self.cells[row * self.cols + col]
    }

    #[inline]
    fn set(&mut self, row: usize, col: usize, state: SolutionState) {
        self.cells[row * self.cols + col] = state;
    }
}

fn make_solution_table(segments: &[Segment], reading: &ReadingData) -> SolutionTable {
    let reading_units = reading.original_offsets.len() - 1;
    let mut table = SolutionTable::new(segments.len() + 1, reading_units + 1);
    table.set(
        segments.len(),
        reading_units,
        SolutionState {
            count: 1,
            next_reading_unit_index: None,
        },
    );

    let mut suffix_counts = vec![0u8; reading_units + 2];
    let mut suffix_first_indices = vec![None; reading_units + 2];
    suffix_counts[..=reading_units].fill(1);
    suffix_first_indices[..=reading_units].fill(Some(reading_units));

    for i in (0..segments.len()).rev() {
        for j in (0..=reading_units).rev() {
            if segments[i].anchor {
                if let Some(end) = match_anchor_at(reading, j, &segments[i].normalized) {
                    let count = table.get(i + 1, end).count;
                    table.set(
                        i,
                        j,
                        SolutionState {
                            count,
                            next_reading_unit_index: (count == 1).then_some(end),
                        },
                    );
                }
            } else {
                let count = suffix_counts[j + 1];
                table.set(
                    i,
                    j,
                    SolutionState {
                        count,
                        next_reading_unit_index: if count == 1 {
                            suffix_first_indices[j + 1]
                        } else {
                            None
                        },
                    },
                );
            }
        }

        for j in (0..=reading_units).rev() {
            let cell_count = table.get(i, j).count;
            suffix_counts[j] = (cell_count + suffix_counts[j + 1]).min(2);
            suffix_first_indices[j] = if cell_count != 0 {
                Some(j)
            } else {
                suffix_first_indices[j + 1]
            };
        }
    }

    table
}

fn align_segments(
    segments: &[Segment],
    reading: &ReadingData,
    surface_len: usize,
    reading_len: usize,
) -> Vec<FuriganaSpan> {
    let table = make_solution_table(segments, reading);
    if table.get(0, 0).count != 1 {
        return vec![FuriganaSpan {
            surface_start: 0,
            surface_end: surface_len,
            reading_start: 0,
            reading_end: reading_len,
        }];
    }

    let mut solution = Vec::with_capacity(segments.len());
    let mut reading_unit_index = 0;
    for (i, segment) in segments.iter().enumerate() {
        let next = table
            .get(i, reading_unit_index)
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

/// Aligns a UTF-8 `surface` and `reading` into uniquely matched spans.
///
/// Returns half-open UTF-8 byte offsets into `surface` and `reading`,
/// guaranteed to lie on valid grapheme boundaries. If there is no unique
/// alignment, returns a single span covering both full inputs. Matched kana
/// anchors have the empty reading range `0..0`.
pub fn simplify_surface_str(surface: &str, reading: &str) -> Vec<FuriganaSpan> {
    if surface
        .nfkc()
        .map(kata_to_hira)
        .eq(reading.nfkc().map(kata_to_hira))
    {
        return vec![FuriganaSpan {
            surface_start: 0,
            surface_end: surface.len(),
            reading_start: 0,
            reading_end: 0,
        }];
    }

    let reading_data = make_reading_data(reading);
    let segments = make_segments(surface, &reading_data.normalized);
    align_segments(&segments, &reading_data, surface.len(), reading.len())
}

/// Returns `true` if `c` is a kanji or ideographic iteration/closing mark:
/// - `々` (`\u{3005}`), `〆` (`\u{3006}`), `〇` (`\u{3007}`)
/// - CJK Unified Ideographs + Extension A (`\u{3400}..=\u{9FFF}`)
/// - CJK Compatibility Ideographs (`\u{F900}..=\u{FAFF}`)
/// - CJK Unified Ideographs Extensions B–I (`\u{20000}..=\u{323AF}`)
pub fn is_kanji_char(c: char) -> bool {
    matches!(
        c,
        '\u{3005}'
            | '\u{3006}'
            | '\u{3007}'
            | '\u{3400}'..='\u{9FFF}'
            | '\u{F900}'..='\u{FAFF}'
            | '\u{20000}'..='\u{323AF}'
    )
}

/// Returns `true` if `text` contains any kanji or ideographic mark (`々`, `〆`, `〇`).
pub fn contains_kanji(text: &str) -> bool {
    text.chars().any(is_kanji_char)
}

/// Distributes `reading` across `surface` so furigana attaches strictly over
/// kanji stems while okurigana and kana affixes receive empty readings `""`.
///
/// Preserves the exact surface concatenation invariant:
/// `distribute_furigana_str(surface, reading).iter().map(|(s, _)| s.as_str()).collect::<String>() == surface`.
pub fn distribute_furigana_str(surface: &str, reading: &str) -> Vec<(String, String)> {
    if surface.is_empty() {
        return Vec::new();
    }
    if reading.is_empty() || reading == "*" || !contains_kanji(surface) {
        return vec![(surface.to_string(), String::new())];
    }

    let reading_hira = crate::furiganaparser::katakana_to_hiragana_str(reading);

    if surface
        .nfkc()
        .map(kata_to_hira)
        .eq(reading_hira.nfkc().map(kata_to_hira))
    {
        return vec![(surface.to_string(), String::new())];
    }

    let spans = simplify_surface_str(surface, &reading_hira);

    if spans.len() == 1
        && spans[0].surface_start == 0
        && spans[0].surface_end == surface.len()
        && spans[0].reading_start == 0
        && spans[0].reading_end == reading_hira.len()
    {
        if let Some(peeled) = peel_kana_affixes(surface, &reading_hira) {
            return peeled;
        }
    }

    let mut out = Vec::with_capacity(spans.len());
    for span in spans {
        let span_surf = surface
            .get(span.surface_start..span.surface_end)
            .unwrap_or_default();
        if span_surf.is_empty() {
            continue;
        }
        let span_read = if span.reading_start < span.reading_end && contains_kanji(span_surf) {
            let r = reading_hira
                .get(span.reading_start..span.reading_end)
                .unwrap_or_default();
            if span_surf
                .nfkc()
                .map(kata_to_hira)
                .eq(r.nfkc().map(kata_to_hira))
            {
                String::new()
            } else {
                r.to_owned()
            }
        } else {
            String::new()
        };
        out.push((span_surf.to_owned(), span_read));
    }

    out
}

fn peel_kana_affixes(surface: &str, reading_hira: &str) -> Option<Vec<(String, String)>> {
    let surf_chars: Vec<char> = surface.chars().collect();
    let read_chars: Vec<char> = reading_hira.chars().collect();

    let mut prefix_len = 0;
    while prefix_len < surf_chars.len()
        && prefix_len < read_chars.len()
        && !is_kanji_char(surf_chars[prefix_len])
        && !matches!(surf_chars[prefix_len], '\u{30F5}' | '\u{30F6}')
        && kata_to_hira(surf_chars[prefix_len]) == kata_to_hira(read_chars[prefix_len])
    {
        prefix_len += 1;
    }

    let mut suffix_len = 0;
    while suffix_len < (surf_chars.len() - prefix_len)
        && suffix_len < (read_chars.len() - prefix_len)
        && !is_kanji_char(surf_chars[surf_chars.len() - 1 - suffix_len])
        && !matches!(
            surf_chars[surf_chars.len() - 1 - suffix_len],
            '\u{30F5}' | '\u{30F6}'
        )
        && kata_to_hira(surf_chars[surf_chars.len() - 1 - suffix_len])
            == kata_to_hira(read_chars[read_chars.len() - 1 - suffix_len])
    {
        suffix_len += 1;
    }

    if prefix_len == 0 && suffix_len == 0 {
        return None;
    }

    let stem_surf: String = surf_chars[prefix_len..surf_chars.len() - suffix_len]
        .iter()
        .collect();
    let stem_read: String = read_chars[prefix_len..read_chars.len() - suffix_len]
        .iter()
        .collect();
    if stem_surf.is_empty() {
        return None;
    }

    let mut parts = Vec::new();
    if prefix_len > 0 {
        parts.push((surf_chars[..prefix_len].iter().collect(), String::new()));
    }
    parts.push((
        stem_surf.clone(),
        if contains_kanji(&stem_surf) {
            stem_read
        } else {
            String::new()
        },
    ));
    if suffix_len > 0 {
        parts.push((
            surf_chars[surf_chars.len() - suffix_len..].iter().collect(),
            String::new(),
        ));
    }
    Some(parts)
}

#[cfg(test)]
mod tests {
    use super::{contains_kanji, distribute_furigana_str, is_kanji_char, simplify_surface_str};

    fn slices<'a>(surface: &'a str, reading: &'a str) -> Vec<(&'a str, &'a str)> {
        simplify_surface_str(surface, reading)
            .into_iter()
            .map(|s| {
                (
                    &surface[s.surface_start..s.surface_end],
                    &reading[s.reading_start..s.reading_end],
                )
            })
            .collect()
    }

    #[test]
    fn whole_text_nfkc_kana_and_iteration_marks_are_equivalent() {
        assert_eq!(slices("ｶﾀｶﾅ", "かたかな"), vec![("ｶﾀｶﾅ", "")]);
        assert_eq!(slices("カヽ", "かゝ"), vec![("カヽ", "")]);
        assert_eq!(slices("カヾ", "かゞ"), vec![("カヾ", "")]);
        assert_eq!(slices("", ""), vec![("", "")]);
    }

    #[test]
    fn unique_kanji_and_kana_segments_and_graphemes_align() {
        assert_eq!(
            slices("食べ物", "たべもの"),
            vec![("食", "た"), ("べ", ""), ("物", "もの")]
        );
        assert_eq!(
            slices("縞か\u{3099}糸", "しまがいと"),
            vec![("縞", "しま"), ("か\u{3099}", ""), ("糸", "いと")]
        );
        assert_eq!(
            slices("字𠀋語", "じ𠀋ご"),
            vec![("字", "じ"), ("𠀋", ""), ("語", "ご")]
        );
    }

    #[test]
    fn nfkc_expansion_and_ambiguous_anchors() {
        assert_eq!(
            slices("㍿者", "株式会社しゃ"),
            vec![("㍿", ""), ("者", "しゃ")]
        );
        assert_eq!(slices("株者", "㍿しゃ"), vec![("株者", "㍿しゃ")]);
        assert_eq!(slices("字あ字", "かああか"), vec![("字あ字", "かああか")]);
        assert_eq!(slices("字あ字", "あか"), vec![("字あ字", "あか")]);
        assert_eq!(slices("あい", "あかい"), vec![("あい", "あかい")]);
        assert_eq!(slices("漢あ", "あ"), vec![("漢あ", "あ")]);
        assert_eq!(slices("", "あ"), vec![("", "あ")]);
    }

    #[test]
    fn kanji_detection_covers_iteration_marks_and_cjk_extensions() {
        assert!(is_kanji_char('々'));
        assert!(is_kanji_char('〆'));
        assert!(is_kanji_char('𠮟'));
        assert!(contains_kanji("人々"));
        assert!(contains_kanji("時々"));
        assert!(contains_kanji("様々"));
        assert!(contains_kanji("𠮟る"));
        assert!(contains_kanji("一ヶ月"));
        assert!(contains_kanji("ヶ月"));
        assert!(!contains_kanji("カタカナ"));
        assert!(!contains_kanji("ひらがな"));
    }

    #[test]
    fn distribute_furigana_aligns_stems_only() {
        assert_eq!(
            distribute_furigana_str("食べる", "タベル"),
            vec![("食".into(), "た".into()), ("べる".into(), "".into())]
        );
        assert_eq!(
            distribute_furigana_str("申し込み", "モウシコミ"),
            vec![
                ("申".into(), "もう".into()),
                ("し".into(), "".into()),
                ("込".into(), "こ".into()),
                ("み".into(), "".into()),
            ]
        );
        assert_eq!(
            distribute_furigana_str("お兄さん", "オニイサン"),
            vec![
                ("お".into(), "".into()),
                ("兄".into(), "にい".into()),
                ("さん".into(), "".into()),
            ]
        );
        assert_eq!(
            distribute_furigana_str("行った", "イッタ"),
            vec![("行".into(), "い".into()), ("った".into(), "".into())]
        );
        assert_eq!(
            distribute_furigana_str("𠮟る", "シカル"),
            vec![("𠮟".into(), "しか".into()), ("る".into(), "".into())]
        );
        assert_eq!(
            distribute_furigana_str("人々", "ヒトビト"),
            vec![("人々".into(), "ひとびと".into())]
        );
    }
}

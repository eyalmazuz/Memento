//! Algorithmic core for SubtitleListModel.
//!
//! Provides pure, allocation-conscious planning for adding, removing, selecting,
//! and diffing subtitle model items, matching `src/subtitle/subtitlelistmodel.cpp`
//! bit-for-bit.

use crate::SubtitleEntry;

pub const MAX_TEXT_LENGTH: usize = 2000;
pub const MAX_NEWLINE_COUNT: usize = 15;
pub const TIME_DELTA: f64 = 0.0001;

/// A lightweight borrowed view of a subtitle cue.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct SubtitleCueView<'a> {
    pub text: &'a [u16],
    pub start: f64,
    pub end: f64,
}

impl<'a> SubtitleCueView<'a> {
    pub fn new(text: &'a [u16], start: f64, end: f64) -> Self {
        Self { text, start, end }
    }
}

impl<'a> From<&'a SubtitleEntry> for SubtitleCueView<'a> {
    fn from(entry: &'a SubtitleEntry) -> Self {
        Self {
            text: &entry.text,
            start: entry.start,
            end: entry.end,
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum AddAction {
    Reject = 0,
    Existing = 1,
    Insert = 2,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(C)]
pub struct AddPlan {
    pub action: AddAction,
    pub index: isize,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(C)]
pub struct SubtitleSpanC {
    pub first: usize,
    pub count: usize,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(C)]
pub struct ModelDiffPlan {
    pub prefix: usize,
    pub suffix: usize,
    pub old_mid_count: usize,
    pub new_mid_count: usize,
}

#[derive(Copy, Clone, Debug, PartialEq)]
#[repr(C)]
pub struct MementoSubtitleCandidateCueC {
    pub original_index: usize,
    pub id_valid: bool,
    pub cue_id: i64,
    pub start_valid: bool,
    pub start: f64,
    pub end_valid: bool,
    pub end: f64,
}

pub type CandidateCue = MementoSubtitleCandidateCueC;

/// Evaluates adding a subtitle into a list sorted by `start`.
///
/// Returns an `AddPlan`:
/// - `Reject` with index `-1` if adds are blocked, or `text.len() > 2000`, or newline count > 15.
/// - `Existing` with the index of the existing cue if one already exists with `item.text == text`
///   in the range `[start - TIME_DELTA, start + TIME_DELTA]`.
/// - `Insert` with the index at which to insert the new cue.
pub fn plan_add_subtitle(
    items: &[SubtitleCueView<'_>],
    block_adds: bool,
    text: &[u16],
    start: f64,
    end: f64,
) -> AddPlan {
    let _ = end; // end is preserved for the inserted entry
    if block_adds || !start.is_finite() {
        return AddPlan { action: AddAction::Reject, index: -1 };
    }
    if text.len() > MAX_TEXT_LENGTH {
        return AddPlan { action: AddAction::Reject, index: -1 };
    }
    let newline_count = text.iter().filter(|&&c| c == b'\n' as u16).count();
    if newline_count > MAX_NEWLINE_COUNT {
        return AddPlan { action: AddAction::Reject, index: -1 };
    }

    // std::lower_bound equivalent: first element where !(entry.start < start - TIME_DELTA)
    let mut index = items.partition_point(|entry| entry.start < start - TIME_DELTA);
    while index < items.len() && items[index].start <= start + TIME_DELTA {
        if items[index].text == text {
            return AddPlan {
                action: AddAction::Existing,
                index: index as isize,
            };
        }
        index += 1;
    }

    AddPlan {
        action: AddAction::Insert,
        index: index as isize,
    }
}

/// Finds the first contiguous span of items overlapping `[start, end]` starting from `from_row`.
pub fn find_first_overlapping_span(
    items: &[SubtitleCueView<'_>],
    start: f64,
    end: f64,
    from_row: usize,
) -> Option<SubtitleSpanC> {
    if !(start < end) || from_row >= items.len() {
        return None;
    }
    let end_limit = end - TIME_DELTA;
    let start_limit = start + TIME_DELTA;
    let mut row = from_row;
    while row < items.len() {
        if items[row].start >= end_limit {
            break;
        }
        let overlaps = items[row].start < end_limit && start_limit < items[row].end;
        if !overlaps {
            row += 1;
            continue;
        }
        let first = row;
        while row < items.len() {
            let item = &items[row];
            if !(item.start < end_limit && start_limit < item.end) {
                break;
            }
            row += 1;
        }
        return Some(SubtitleSpanC {
            first,
            count: row - first,
        });
    }
    None
}

/// Collects all contiguous removal spans for `removeOverlapping`, simulating sequential
/// removals so that each returned span's `first` index matches the live model at removal time.
pub fn collect_overlapping_spans(
    items: &[SubtitleCueView<'_>],
    start: f64,
    end: f64,
) -> Vec<SubtitleSpanC> {
    if !(start < end) {
        return Vec::new();
    }
    let end_limit = end - TIME_DELTA;
    let start_limit = start + TIME_DELTA;
    let mut spans = Vec::new();
    let mut current_offset: usize = 0;
    let mut i = 0;
    while i < items.len() {
        if items[i].start >= end_limit {
            break;
        }
        let overlaps = items[i].start < end_limit && start_limit < items[i].end;
        if !overlaps {
            i += 1;
            continue;
        }
        let first_orig = i;
        while i < items.len() {
            if !(items[i].start < end_limit && start_limit < items[i].end) {
                break;
            }
            i += 1;
        }
        let count = i - first_orig;
        spans.push(SubtitleSpanC {
            first: first_orig - current_offset,
            count,
        });
        current_offset += count;
    }
    spans
}

/// Returns the row indices whose time intervals overlap `position`.
///
/// Matches C++ `position += TIME_DELTA; item.start < pos && pos < item.end`.
pub fn select_position_rows(items: &[SubtitleCueView<'_>], position: f64) -> Vec<usize> {
    let pos = position + TIME_DELTA;
    if pos.is_nan() {
        return Vec::new();
    }
    let upper = items.partition_point(|item| item.start < pos);
    let mut rows = Vec::new();
    for (i, item) in items[..upper].iter().enumerate() {
        if item.start < pos && pos < item.end {
            rows.push(i);
        }
    }
    rows
}

/// Compares two cues for equality under the `TIME_DELTA` threshold and exact text match.
pub fn same_entry(lhs: &SubtitleCueView<'_>, rhs: &SubtitleCueView<'_>) -> bool {
    (lhs.start - rhs.start).abs() <= TIME_DELTA
        && (lhs.end - rhs.end).abs() <= TIME_DELTA
        && lhs.text == rhs.text
}

/// Computes common prefix and suffix to minimize row remove/insert operations when setting items.
pub fn diff_items(
    old_items: &[SubtitleCueView<'_>],
    new_items: &[SubtitleCueView<'_>],
) -> ModelDiffPlan {
    let min_size = old_items.len().min(new_items.len());
    let mut prefix = 0;
    while prefix < min_size && same_entry(&old_items[prefix], &new_items[prefix]) {
        prefix += 1;
    }

    let mut suffix = 0;
    while suffix < (min_size - prefix)
        && same_entry(
            &old_items[old_items.len() - 1 - suffix],
            &new_items[new_items.len() - 1 - suffix],
        )
    {
        suffix += 1;
    }

    let old_mid_count = old_items.len() - prefix - suffix;
    let new_mid_count = new_items.len() - prefix - suffix;

    ModelDiffPlan {
        prefix,
        suffix,
        old_mid_count,
        new_mid_count,
    }
}

/// Filters candidate cues matching `SubtitleListManager::handlePrimarySubtitleLinesChanged`:
/// Valid ID (`id_valid && cue_id > 0`), valid start and end (`start_valid && end_valid`),
/// finite timestamps (`start.is_finite() && end.is_finite()`), non-negative start (`start >= 0.0`),
/// and strictly positive duration (`end > start`).
///
/// Then performs a stable sort by `lhs.start < rhs.start`, returning the surviving `original_index`es.
pub fn filter_and_sort_cues(candidates: &[MementoSubtitleCandidateCueC]) -> Vec<usize> {
    struct Surviving {
        original_index: usize,
        start: f64,
    }

    let mut surviving: Vec<Surviving> = candidates
        .iter()
        .filter(|c| {
            c.id_valid
                && c.cue_id > 0
                && c.start_valid
                && c.end_valid
                && c.start.is_finite()
                && c.end.is_finite()
                && c.start >= 0.0
                && c.end > c.start
        })
        .map(|c| Surviving {
            original_index: c.original_index,
            start: c.start,
        })
        .collect();

    surviving.sort_by(|a, b| {
        a.start.partial_cmp(&b.start).unwrap_or(std::cmp::Ordering::Equal)
    });

    surviving.into_iter().map(|s| s.original_index).collect()
}

/// Determines whether to update an in-memory subtitle track and whether to mark it as a memory track.
///
/// Matches lines 236–248 of `SubtitleListManager::handlePrimarySubtitleLinesChanged`:
/// - If `track_id <= 0 || !has_model`, returns `(false, false)`.
/// - `insert_memory_track = lines_non_empty`.
/// - `should_update = lines_non_empty || was_memory_track`.
pub fn should_update_memory_track(
    track_id: i64,
    has_model: bool,
    lines_non_empty: bool,
    was_memory_track: bool,
) -> (bool, bool) {
    if track_id <= 0 || !has_model {
        return (false, false);
    }
    (lines_non_empty || was_memory_track, lines_non_empty)
}

use std::collections::{HashMap, HashSet};

/// An incoming subtitle track representation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubtitleTrackInput<'a> {
    pub is_null: bool,
    pub track_id: i64,
    pub is_external: bool,
    pub external_path: &'a [u16],
}

impl<'a> SubtitleTrackInput<'a> {
    pub fn new(
        is_null: bool,
        track_id: i64,
        is_external: bool,
        external_path: &'a [u16],
    ) -> Self {
        Self {
            is_null,
            track_id,
            is_external,
            external_path,
        }
    }
}

/// An existing external path representation for a track ID.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubtitleExternalPathInput<'a> {
    pub track_id: i64,
    pub path: &'a [u16],
}

impl<'a> SubtitleExternalPathInput<'a> {
    pub fn new(track_id: i64, path: &'a [u16]) -> Self {
        Self { track_id, path }
    }
}

/// Actions to execute on SubtitleListManager when tracks change.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TrackAction<'a> {
    CreateModel {
        track_id: i64,
    },
    ReloadExternal {
        track_id: i64,
        path: &'a [u16],
        read_file: bool,
    },
    ClearExternal {
        track_id: i64,
    },
    RemoveTrack {
        track_id: i64,
    },
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum TrackActionKind {
    CreateModel = 0,
    ReloadExternal = 1,
    ClearExternal = 2,
    RemoveTrack = 3,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TrackUpdatePlan<'a> {
    pub actions: Vec<TrackAction<'a>>,
}

/// Computes the sequence of actions required to synchronize subtitle track models with mpv's track list.
///
/// Matches `SubtitleListManager::handleSubtitleTracksChanged` in `src/manager/subtitlelistmanager.cpp`.
pub fn plan_track_changes<'a>(
    tracks: &[SubtitleTrackInput<'a>],
    existing_model_ids: &[i64],
    existing_external_paths: &[SubtitleExternalPathInput<'a>],
) -> TrackUpdatePlan<'a> {
    let mut actions = Vec::new();
    let mut seen_track_ids: HashSet<i64> = HashSet::new();
    let mut models_set: HashSet<i64> = existing_model_ids.iter().copied().collect();
    let mut external_paths_map: HashMap<i64, &'a [u16]> = existing_external_paths
        .iter()
        .map(|e| (e.track_id, e.path))
        .collect();

    for track in tracks {
        if track.is_null || track.track_id <= 0 {
            continue;
        }

        let track_id = track.track_id;
        seen_track_ids.insert(track_id);

        if models_set.insert(track_id) {
            actions.push(TrackAction::CreateModel { track_id });
        }

        if track.is_external {
            let path = track.external_path;
            let needs_reload = match external_paths_map.get(&track_id) {
                Some(&existing_path) => existing_path != path,
                None => true,
            };

            if needs_reload {
                external_paths_map.insert(track_id, path);
                let read_file = !path.is_empty();
                actions.push(TrackAction::ReloadExternal {
                    track_id,
                    path,
                    read_file,
                });
            }
        } else if external_paths_map.remove(&track_id).is_some() {
            actions.push(TrackAction::ClearExternal { track_id });
        }
    }

    let mut removed_set: HashSet<i64> = HashSet::new();
    for &track_id in existing_model_ids {
        if !seen_track_ids.contains(&track_id) && removed_set.insert(track_id) {
            actions.push(TrackAction::RemoveTrack { track_id });
        }
    }

    TrackUpdatePlan { actions }
}

/// The decision outcome for adding/selecting an active subtitle cue.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum ActiveCueActionKind {
    Noop = 0,
    AddAndSelect = 1,
    ClearSelection = 2,
}

#[derive(Copy, Clone, Debug, PartialEq)]
#[repr(C)]
pub struct ActiveCuePlan {
    pub action: ActiveCueActionKind,
    pub raw_position: f64,
}

/// Matches `SubtitleListManager::addSubtitle` lines 361–381.
pub fn plan_active_cue(
    has_model: bool,
    has_subtitle: bool,
    text_non_empty: bool,
    raw_position: f64,
) -> ActiveCuePlan {
    if !has_model || !has_subtitle {
        ActiveCuePlan {
            action: ActiveCueActionKind::Noop,
            raw_position: 0.0,
        }
    } else if text_non_empty {
        ActiveCuePlan {
            action: ActiveCueActionKind::AddAndSelect,
            raw_position,
        }
    } else {
        ActiveCuePlan {
            action: ActiveCueActionKind::ClearSelection,
            raw_position: 0.0,
        }
    }
}


// ---------------------------------------------------------------------------
// Subtitle Model Find: whitespace stripping & native Rust Unicode case folding
// ---------------------------------------------------------------------------

#[inline]
fn is_ascii_whitespace_char(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\x0b' | '\x0c' | '\r')
}

#[inline]
fn push_case_folded_char(ch: char, out: &mut String) {
    if ch.is_ascii() {
        out.push(ch.to_ascii_lowercase());
        return;
    }
    match ch {
        '\u{03c2}' => out.push('\u{03c3}'), // Greek final sigma ς -> σ
        '\u{00b5}' => out.push('\u{03bc}'), // micro sign µ -> μ
        '\u{017f}' => out.push('s'),        // Latin small long s ſ -> s
        '\u{2126}' => out.push('\u{03c9}'), // ohm sign Ω -> ω
        '\u{212a}' => out.push('k'),        // Kelvin sign K -> k
        '\u{212b}' => out.push('\u{00e5}'), // Angstrom sign Å -> å
        _ => {
            for lc in ch.to_lowercase() {
                out.push(lc);
            }
        }
    }
}

fn fold_str_into(text: &str, ignore_whitespace: bool, out: &mut String) {
    out.clear();
    out.reserve(text.len());
    for ch in text.chars() {
        if ignore_whitespace && is_ascii_whitespace_char(ch) {
            continue;
        }
        push_case_folded_char(ch, out);
    }
}

fn fold_utf16_into_str(units: &[u16], ignore_whitespace: bool, out: &mut String) {
    out.clear();
    out.reserve(units.len());
    for decoded in char::decode_utf16(units.iter().copied()) {
        let ch = decoded.unwrap_or(char::REPLACEMENT_CHARACTER);
        if ignore_whitespace && is_ascii_whitespace_char(ch) {
            continue;
        }
        push_case_folded_char(ch, out);
    }
}

/// Finds all row indices in `candidates` that match `query` (UTF-8 native).
///
/// - If `ignore_whitespace` is true, ASCII whitespace (`[ \t\n\x0b\x0c\r]`) is stripped
///   from both the query and candidate texts.
/// - If the query is empty (or becomes empty after whitespace removal), returns empty.
/// - Candidates are matched case-insensitively using Unicode case folding.
pub fn find_matching_rows_str<T: AsRef<str>>(
    candidates: &[T],
    query: &str,
    ignore_whitespace: bool,
) -> Vec<usize> {
    let mut normalized_query = String::new();
    fold_str_into(query, ignore_whitespace, &mut normalized_query);
    if normalized_query.is_empty() {
        return Vec::new();
    }

    let mut cand_buf = String::new();
    candidates
        .iter()
        .enumerate()
        .filter_map(|(idx, candidate)| {
            fold_str_into(candidate.as_ref(), ignore_whitespace, &mut cand_buf);
            cand_buf.contains(normalized_query.as_str()).then_some(idx)
        })
        .collect()
}

/// Finds all row indices in `candidates` that match `query` (UTF-16 slice input).
///
/// Decodes and folds directly into a reusable UTF-8 buffer without UTF-16 re-encoding.
pub fn find_matching_rows<T: AsRef<[u16]>>(
    candidates: &[T],
    query: &[u16],
    ignore_whitespace: bool,
) -> Vec<usize> {
    let mut normalized_query = String::new();
    fold_utf16_into_str(query, ignore_whitespace, &mut normalized_query);
    if normalized_query.is_empty() {
        return Vec::new();
    }

    let mut cand_buf = String::new();
    candidates
        .iter()
        .enumerate()
        .filter_map(|(idx, candidate)| {
            fold_utf16_into_str(candidate.as_ref(), ignore_whitespace, &mut cand_buf);
            cand_buf.contains(normalized_query.as_str()).then_some(idx)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn to_utf16(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    #[test]
    fn test_plan_add_subtitle_limits() {
        let t1 = to_utf16("hello");
        let items = [SubtitleCueView::new(&t1, 1.0, 2.0)];

        // Blocked adds
        let plan = plan_add_subtitle(&items, true, &t1, 3.0, 4.0);
        assert_eq!(plan.action, AddAction::Reject);
        assert_eq!(plan.index, -1);

        // Max text length: 2000 ok, 2001 reject
        let ok_text: Vec<u16> = vec![b'a' as u16; 2000];
        let plan = plan_add_subtitle(&items, false, &ok_text, 3.0, 4.0);
        assert_eq!(plan.action, AddAction::Insert);
        assert_eq!(plan.index, 1);

        let long_text: Vec<u16> = vec![b'a' as u16; 2001];
        let plan = plan_add_subtitle(&items, false, &long_text, 3.0, 4.0);
        assert_eq!(plan.action, AddAction::Reject);
        assert_eq!(plan.index, -1);

        // Newlines: 15 ok, 16 reject
        let ok_nl: Vec<u16> = "a\n".repeat(15).encode_utf16().collect();
        let plan = plan_add_subtitle(&items, false, &ok_nl, 3.0, 4.0);
        assert_eq!(plan.action, AddAction::Insert);

        let too_many_nl: Vec<u16> = "a\n".repeat(16).encode_utf16().collect();
        let plan = plan_add_subtitle(&items, false, &too_many_nl, 3.0, 4.0);
        assert_eq!(plan.action, AddAction::Reject);

        // Non-finite start timestamps must be rejected so sorted order is never corrupted.
        for bad_start in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let plan = plan_add_subtitle(&items, false, &t1, bad_start, 4.0);
            assert_eq!(plan, AddPlan { action: AddAction::Reject, index: -1 });
        }
    }

    #[test]
    fn test_plan_add_subtitle_ordering_and_dedup() {
        let text_a = to_utf16("A");
        let text_b = to_utf16("B");
        let text_c = to_utf16("C");

        let items = [
            SubtitleCueView::new(&text_a, 1.0, 2.0),
            SubtitleCueView::new(&text_b, 3.0, 4.0),
        ];

        // Insert at beginning
        let plan = plan_add_subtitle(&items, false, &text_c, 0.5, 1.0);
        assert_eq!(plan, AddPlan { action: AddAction::Insert, index: 0 });

        // Insert in middle
        let plan = plan_add_subtitle(&items, false, &text_c, 2.0, 2.5);
        assert_eq!(plan, AddPlan { action: AddAction::Insert, index: 1 });

        // Insert at end
        let plan = plan_add_subtitle(&items, false, &text_c, 5.0, 6.0);
        assert_eq!(plan, AddPlan { action: AddAction::Insert, index: 2 });

        // Existing at exact timestamp
        let plan = plan_add_subtitle(&items, false, &text_a, 1.0, 2.0);
        assert_eq!(plan, AddPlan { action: AddAction::Existing, index: 0 });

        // Existing at timestamp within TIME_DELTA
        let plan = plan_add_subtitle(&items, false, &text_a, 1.00005, 2.0);
        assert_eq!(plan, AddPlan { action: AddAction::Existing, index: 0 });

        let plan = plan_add_subtitle(&items, false, &text_a, 0.99995, 2.0);
        assert_eq!(plan, AddPlan { action: AddAction::Existing, index: 0 });

        // Distinct text at same timestamp
        let plan = plan_add_subtitle(&items, false, &text_c, 1.0, 2.0);
        assert_eq!(plan, AddPlan { action: AddAction::Insert, index: 1 });
    }

    #[test]
    fn test_overlapping_spans() {
        let text = to_utf16("text");
        let items = [
            SubtitleCueView::new(&text, 1.0, 2.0), // 0: [1.0, 2.0]
            SubtitleCueView::new(&text, 2.5, 3.5), // 1: [2.5, 3.5]
            SubtitleCueView::new(&text, 3.0, 4.0), // 2: [3.0, 4.0]
            SubtitleCueView::new(&text, 5.0, 6.0), // 3: [5.0, 6.0]
        ];

        // end <= start
        assert!(collect_overlapping_spans(&items, 3.0, 3.0).is_empty());
        assert!(collect_overlapping_spans(&items, 4.0, 2.0).is_empty());

        // Range [2.0, 4.5] overlaps items 1 and 2
        let spans = collect_overlapping_spans(&items, 2.0, 4.5);
        assert_eq!(spans, vec![SubtitleSpanC { first: 1, count: 2 }]);

        // Boundary test: [2.0001, 3.0] vs item [1.0, 2.0]
        // start + TIME_DELTA = 2.0002 < 2.0 is false -> does NOT overlap item 0
        let spans = collect_overlapping_spans(&items, 2.0001, 2.4);
        assert!(spans.is_empty());

        // Disjoint overlaps:
        // Item 0: [0.5, 3.0], Item 1: [1.0, 1.5], Item 2: [2.5, 3.5]
        let disjoint_items = [
            SubtitleCueView::new(&text, 0.5, 3.0),
            SubtitleCueView::new(&text, 1.0, 1.5),
            SubtitleCueView::new(&text, 2.5, 3.5),
        ];
        // Range [2.0, 4.0]:
        // Item 0 overlaps (0.5 < 3.9999 && 2.0001 < 3.0) -> first=0, count=1.
        // Item 1 does not overlap (2.0001 < 1.5 is false).
        // Item 2 overlaps (2.5 < 3.9999 && 2.0001 < 3.5) -> now at index 1!
        let spans = collect_overlapping_spans(&disjoint_items, 2.0, 4.0);
        assert_eq!(spans, vec![
            SubtitleSpanC { first: 0, count: 1 },
            SubtitleSpanC { first: 1, count: 1 },
        ]);
    }

    #[test]
    fn test_select_position_rows() {
        let text = to_utf16("cue");
        let items = [
            SubtitleCueView::new(&text, 1.0, 5.0),
            SubtitleCueView::new(&text, 2.0, 4.0),
            SubtitleCueView::new(&text, 4.5, 6.0),
        ];

        // position 3.0 -> pos = 3.0001 -> items 0 and 1
        assert_eq!(select_position_rows(&items, 3.0), vec![0, 1]);

        // position 4.7 -> pos = 4.7001 -> items 0 and 2
        assert_eq!(select_position_rows(&items, 4.7), vec![0, 2]);

        // position 0.5 -> pos = 0.5001 -> none
        assert_eq!(select_position_rows(&items, 0.5), Vec::<usize>::new());

        // NaN position -> none
        assert_eq!(select_position_rows(&items, f64::NAN), Vec::<usize>::new());
    }

    #[test]
    fn test_diff_items() {
        let a = to_utf16("A");
        let b = to_utf16("B");
        let c = to_utf16("C");
        let d = to_utf16("D");
        let e = to_utf16("E");
        let x = to_utf16("X");
        let y = to_utf16("Y");

        let old = [
            SubtitleCueView::new(&a, 1.0, 2.0),
            SubtitleCueView::new(&b, 2.0, 3.0),
            SubtitleCueView::new(&c, 3.0, 4.0),
            SubtitleCueView::new(&d, 4.0, 5.0),
            SubtitleCueView::new(&e, 5.0, 6.0),
        ];

        // Identical
        let plan = diff_items(&old, &old);
        assert_eq!(plan, ModelDiffPlan {
            prefix: 5,
            suffix: 0,
            old_mid_count: 0,
            new_mid_count: 0,
        });

        // Middle replaced: A, B, X, Y, D, E
        let new_items = [
            SubtitleCueView::new(&a, 1.0, 2.0),
            SubtitleCueView::new(&b, 2.0, 3.0),
            SubtitleCueView::new(&x, 3.0, 3.5),
            SubtitleCueView::new(&y, 3.5, 4.0),
            SubtitleCueView::new(&d, 4.0, 5.0),
            SubtitleCueView::new(&e, 5.0, 6.0),
        ];
        let plan = diff_items(&old, &new_items);
        assert_eq!(plan, ModelDiffPlan {
            prefix: 2,
            suffix: 2,
            old_mid_count: 1,
            new_mid_count: 2,
        });

        // Growing from empty
        let plan = diff_items(&[], &old);
        assert_eq!(plan, ModelDiffPlan {
            prefix: 0,
            suffix: 0,
            old_mid_count: 0,
            new_mid_count: 5,
        });

        // Shrinking to empty
        let plan = diff_items(&old, &[]);
        assert_eq!(plan, ModelDiffPlan {
            prefix: 0,
            suffix: 0,
            old_mid_count: 5,
            new_mid_count: 0,
        });
    }

    #[test]
    fn test_filter_and_sort_cues() {
        // Base valid candidate generator
        let valid = |idx: usize, start: f64, end: f64| MementoSubtitleCandidateCueC {
            original_index: idx,
            id_valid: true,
            cue_id: (idx + 1) as i64,
            start_valid: true,
            start,
            end_valid: true,
            end,
        };

        // Out-of-order start timestamps
        let candidates = [
            valid(0, 10.0, 12.0),
            valid(1, 2.0, 4.0),
            valid(2, 5.0, 8.0),
        ];
        assert_eq!(filter_and_sort_cues(&candidates), vec![1, 2, 0]);

        // Filter invalid conditions
        let mixed = vec![
            valid(0, 1.0, 2.0),
            // id_valid = false
            MementoSubtitleCandidateCueC { id_valid: false, ..valid(1, 0.5, 1.5) },
            // cue_id <= 0
            MementoSubtitleCandidateCueC { cue_id: 0, ..valid(2, 0.5, 1.5) },
            MementoSubtitleCandidateCueC { cue_id: -5, ..valid(3, 0.5, 1.5) },
            // start_valid = false
            MementoSubtitleCandidateCueC { start_valid: false, ..valid(4, 0.5, 1.5) },
            // end_valid = false
            MementoSubtitleCandidateCueC { end_valid: false, ..valid(5, 0.5, 1.5) },
            // NaN or Inf
            MementoSubtitleCandidateCueC { start: f64::NAN, ..valid(6, 0.5, 1.5) },
            MementoSubtitleCandidateCueC { end: f64::NAN, ..valid(7, 0.5, 1.5) },
            MementoSubtitleCandidateCueC { start: f64::INFINITY, ..valid(8, 0.5, 1.5) },
            MementoSubtitleCandidateCueC { end: f64::INFINITY, ..valid(9, 0.5, 1.5) },
            MementoSubtitleCandidateCueC { start: f64::NEG_INFINITY, ..valid(10, 0.5, 1.5) },
            MementoSubtitleCandidateCueC { end: f64::NEG_INFINITY, ..valid(11, 0.5, 1.5) },
            // start < 0.0
            MementoSubtitleCandidateCueC { start: -0.001, ..valid(12, -0.001, 1.5) },
            // end <= start
            MementoSubtitleCandidateCueC { start: 5.0, end: 5.0, ..valid(13, 5.0, 5.0) },
            MementoSubtitleCandidateCueC { start: 6.0, end: 4.0, ..valid(14, 6.0, 4.0) },
            // valid cue 15
            valid(15, 0.0, 0.5),
        ];
        // Only 0 (start 1.0) and 15 (start 0.0) survive; sorted order is 15, then 0.
        assert_eq!(filter_and_sort_cues(&mixed), vec![15, 0]);

        // Stability test: multiple cues with equal start timestamps
        let equal_starts = [
            valid(100, 3.0, 5.0),
            valid(101, 1.0, 4.0),
            valid(102, 3.0, 6.0),
            valid(103, 1.0, 2.0),
            valid(104, 3.0, 4.0),
        ];
        // At start 1.0: indices 101, then 103
        // At start 3.0: indices 100, then 102, then 104
        assert_eq!(filter_and_sort_cues(&equal_starts), vec![101, 103, 100, 102, 104]);

        // Empty input
        assert_eq!(filter_and_sort_cues(&[]), Vec::<usize>::new());
    }

    #[test]
    fn test_should_update_memory_track() {
        // Invalid track_id
        assert_eq!(should_update_memory_track(0, true, true, true), (false, false));
        assert_eq!(should_update_memory_track(-1, true, true, false), (false, false));

        // Missing model
        assert_eq!(should_update_memory_track(1, false, true, true), (false, false));

        // Non-empty lines
        assert_eq!(should_update_memory_track(1, true, true, false), (true, true));
        assert_eq!(should_update_memory_track(1, true, true, true), (true, true));

        // Empty lines, previously memory track
        assert_eq!(should_update_memory_track(1, true, false, true), (true, false));

        // Empty lines, was not memory track
        assert_eq!(should_update_memory_track(1, true, false, false), (false, false));
    }

    #[test]
    fn test_plan_active_cue() {
        assert_eq!(
            plan_active_cue(false, false, true, 12.5),
            ActiveCuePlan { action: ActiveCueActionKind::Noop, raw_position: 0.0 }
        );
        assert_eq!(
            plan_active_cue(false, true, true, 12.5),
            ActiveCuePlan { action: ActiveCueActionKind::Noop, raw_position: 0.0 }
        );
        assert_eq!(
            plan_active_cue(true, false, true, 12.5),
            ActiveCuePlan { action: ActiveCueActionKind::Noop, raw_position: 0.0 }
        );
        assert_eq!(
            plan_active_cue(true, true, false, 12.5),
            ActiveCuePlan { action: ActiveCueActionKind::ClearSelection, raw_position: 0.0 }
        );
        assert_eq!(
            plan_active_cue(true, true, true, 12.5),
            ActiveCuePlan { action: ActiveCueActionKind::AddAndSelect, raw_position: 12.5 }
        );
    }

    #[test]
    fn test_plan_track_changes() {
        let p1 = to_utf16("path1.srt");
        let p2 = to_utf16("path2.srt");
        let empty = to_utf16("");

        // 1. Invalid / null tracks ignored
        let tracks = [
            SubtitleTrackInput::new(true, 1, false, &[]),
            SubtitleTrackInput::new(false, 0, false, &[]),
            SubtitleTrackInput::new(false, -5, false, &[]),
        ];
        let plan = plan_track_changes(&tracks, &[], &[]);
        assert!(plan.actions.is_empty());

        // 2. Add new tracks (internal, external non-empty, external empty)
        let tracks = [
            SubtitleTrackInput::new(false, 1, false, &[]),
            SubtitleTrackInput::new(false, 2, true, &p1),
            SubtitleTrackInput::new(false, 3, true, &empty),
        ];
        let plan = plan_track_changes(&tracks, &[], &[]);
        assert_eq!(
            plan.actions,
            vec![
                TrackAction::CreateModel { track_id: 1 },
                TrackAction::CreateModel { track_id: 2 },
                TrackAction::ReloadExternal { track_id: 2, path: &p1, read_file: true },
                TrackAction::CreateModel { track_id: 3 },
                TrackAction::ReloadExternal { track_id: 3, path: &empty, read_file: false },
            ]
        );

        // 3. Existing tracks, no changes
        let ext_paths = [SubtitleExternalPathInput::new(2, &p1)];
        let plan = plan_track_changes(&tracks[0..2], &[1, 2], &ext_paths);
        assert!(plan.actions.is_empty());

        // 4. Path change and transition to internal
        let tracks = [
            SubtitleTrackInput::new(false, 1, true, &p2), // internal -> external
            SubtitleTrackInput::new(false, 2, false, &[]), // external -> internal
        ];
        let plan = plan_track_changes(&tracks, &[1, 2], &ext_paths);
        assert_eq!(
            plan.actions,
            vec![
                TrackAction::ReloadExternal { track_id: 1, path: &p2, read_file: true },
                TrackAction::ClearExternal { track_id: 2 },
            ]
        );

        // 5. Removals
        let tracks = [SubtitleTrackInput::new(false, 2, false, &[])];
        let plan = plan_track_changes(&tracks, &[1, 2, 3], &[]);
        assert_eq!(
            plan.actions,
            vec![
                TrackAction::RemoveTrack { track_id: 1 },
                TrackAction::RemoveTrack { track_id: 3 },
            ]
        );

        // 6. Duplicate track ID in incoming tracks
        let tracks = [
            SubtitleTrackInput::new(false, 1, true, &p1),
            SubtitleTrackInput::new(false, 1, true, &p2),
            SubtitleTrackInput::new(false, 1, false, &[]),
        ];
        let plan = plan_track_changes(&tracks, &[], &[]);
        assert_eq!(
            plan.actions,
            vec![
                TrackAction::CreateModel { track_id: 1 },
                TrackAction::ReloadExternal { track_id: 1, path: &p1, read_file: true },
                TrackAction::ReloadExternal { track_id: 1, path: &p2, read_file: true },
                TrackAction::ClearExternal { track_id: 1 },
            ]
        );
    }

    #[test]
    fn test_find_matching_rows() {
        let cues = [
            to_utf16("Hello World"),
            to_utf16("日本語の字幕テスト"),
            to_utf16("Line 1\nLine 2\r\nLine 3"),
            to_utf16("Fullwidth\u{3000}space\u{3000}test"),
            to_utf16("Σίσυφος"),
            to_utf16("Deseret \u{10400} test"),
        ];

        // 1. Empty query
        assert_eq!(find_matching_rows(&cues, &to_utf16(""), false), Vec::<usize>::new());
        assert_eq!(find_matching_rows(&cues, &to_utf16(""), true), Vec::<usize>::new());

        // 2. Whitespace-only query
        assert_eq!(find_matching_rows(&cues, &to_utf16("   \t\n\r"), true), Vec::<usize>::new());
        assert_eq!(find_matching_rows(&cues, &to_utf16(" "), false), vec![0, 2, 5]);
        assert_eq!(find_matching_rows(&cues, &to_utf16("\u{3000}"), false), vec![3]);

        // 3. ASCII Case-insensitive
        assert_eq!(find_matching_rows(&cues, &to_utf16("hello"), false), vec![0]);
        assert_eq!(find_matching_rows(&cues, &to_utf16("HELLO"), false), vec![0]);
        assert_eq!(find_matching_rows(&cues, &to_utf16("wOrLd"), false), vec![0]);
        assert_eq!(find_matching_rows(&cues, &to_utf16("h e l l o"), true), vec![0]);

        // 4. Japanese kanji and kana
        assert_eq!(find_matching_rows(&cues, &to_utf16("字幕"), false), vec![1]);
        assert_eq!(find_matching_rows(&cues, &to_utf16("日本語"), false), vec![1]);
        assert_eq!(find_matching_rows(&cues, &to_utf16("テスト"), false), vec![1]);

        // 5. Multi-line cues and ignore_whitespace
        assert_eq!(find_matching_rows(&cues, &to_utf16("1Line"), false), Vec::<usize>::new());
        assert_eq!(find_matching_rows(&cues, &to_utf16("1Line"), true), vec![2]);
        assert_eq!(find_matching_rows(&cues, &to_utf16("Line 1Line 2"), true), vec![2]);

        // 6. Fullwidth space is NOT ASCII whitespace
        assert_eq!(find_matching_rows(&cues, &to_utf16("Fullwidth\u{3000}space"), true), vec![3]);
        assert_eq!(find_matching_rows(&cues, &to_utf16("Fullwidthspace"), true), Vec::<usize>::new());

        // 7. Greek sigma case folding (upper Σ, lower σ, final ς)
        assert_eq!(find_matching_rows(&cues, &to_utf16("σ"), false), vec![4]);
        assert_eq!(find_matching_rows(&cues, &to_utf16("ς"), false), vec![4]);
        assert_eq!(find_matching_rows(&cues, &to_utf16("ΣΙΣΥΦΟΣ"), false), Vec::<usize>::new()); // without tonos does not match
        assert_eq!(find_matching_rows(&cues, &to_utf16("ΣΊΣΥΦΟΣ"), false), vec![4]); // with tonos matches

        // 8. Astral planes case folding (0x10400 capital vs 0x10428 small)
        assert_eq!(find_matching_rows(&cues, &to_utf16("\u{10428}"), false), vec![5]);

        // 9. Native UTF-8 path matches identically
        let str_cues = [
            "Hello World",
            "日本語の字幕テスト",
            "Line 1\nLine 2\r\nLine 3",
            "Fullwidth\u{3000}space\u{3000}test",
            "Σίσυφος",
            "Deseret \u{10400} test",
        ];
        assert_eq!(find_matching_rows_str(&str_cues, "h e l l o", true), vec![0]);
        assert_eq!(find_matching_rows_str(&str_cues, "ΣΊΣΥΦΟΣ", false), vec![4]);
        assert_eq!(find_matching_rows_str(&str_cues, "\u{10428}", false), vec![5]);
    }

    #[test]
    fn test_find_first_overlapping_span() {
        let text = to_utf16("span");
        let items = [
            SubtitleCueView::new(&text, 1.0, 2.0),
            SubtitleCueView::new(&text, 2.5, 3.5),
            SubtitleCueView::new(&text, 3.0, 4.0),
            SubtitleCueView::new(&text, 5.0, 6.0),
        ];

        // Valid query starting at from_row = 0
        let span = find_first_overlapping_span(&items, 2.0, 4.5, 0);
        assert_eq!(span, Some(SubtitleSpanC { first: 1, count: 2 }));

        // from_row out of bounds
        assert_eq!(find_first_overlapping_span(&items, 1.0, 2.0, 4), None);
        assert_eq!(find_first_overlapping_span(&items, 1.0, 2.0, 100), None);

        // end <= start
        assert_eq!(find_first_overlapping_span(&items, 3.0, 3.0, 0), None);
        assert_eq!(find_first_overlapping_span(&items, 3.5, 2.0, 0), None);

        // from_row after first match
        let span = find_first_overlapping_span(&items, 1.0, 6.0, 3);
        assert_eq!(span, Some(SubtitleSpanC { first: 3, count: 1 }));
    }
}

//! Subtitle list planning: insertion ordering, overlap spans, list diffing,
//! track synchronization, and case-folded search.

use std::collections::{HashMap, HashSet};

use crate::SubtitleEntry;

pub const MAX_TEXT_LENGTH: usize = 2000;
pub const MAX_NEWLINE_COUNT: usize = 15;
pub const TIME_DELTA: f64 = 0.0001;

/// A lightweight borrowed view of a subtitle cue.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct SubtitleCueView<'a> {
    pub text: &'a str,
    pub start: f64,
    pub end: f64,
}

impl<'a> SubtitleCueView<'a> {
    pub fn new(text: &'a str, start: f64, end: f64) -> Self {
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
pub enum AddAction {
    Reject,
    Existing,
    Insert,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct AddPlan {
    pub action: AddAction,
    pub index: isize,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct SubtitleSpan {
    pub first: usize,
    pub count: usize,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct ModelDiffPlan {
    pub prefix: usize,
    pub suffix: usize,
    pub old_mid_count: usize,
    pub new_mid_count: usize,
}

#[derive(Copy, Clone, Debug, PartialEq)]
pub struct CandidateCue {
    pub original_index: usize,
    pub id_valid: bool,
    pub cue_id: i64,
    pub start_valid: bool,
    pub start: f64,
    pub end_valid: bool,
    pub end: f64,
}

/// Evaluates adding a subtitle into a list sorted by `start`.
pub fn plan_add_subtitle(
    items: &[SubtitleCueView<'_>],
    block_adds: bool,
    text: &str,
    start: f64,
    end: f64,
) -> AddPlan {
    let _ = end;
    if block_adds || !start.is_finite() {
        return AddPlan {
            action: AddAction::Reject,
            index: -1,
        };
    }
    if text.chars().count() > MAX_TEXT_LENGTH {
        return AddPlan {
            action: AddAction::Reject,
            index: -1,
        };
    }
    let newline_count = text.bytes().filter(|&b| b == b'\n').count();
    if newline_count > MAX_NEWLINE_COUNT {
        return AddPlan {
            action: AddAction::Reject,
            index: -1,
        };
    }

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
) -> Option<SubtitleSpan> {
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
        if !(items[row].start < end_limit && start_limit < items[row].end) {
            row += 1;
            continue;
        }
        let first = row;
        while row < items.len() && items[row].start < end_limit && start_limit < items[row].end {
            row += 1;
        }
        return Some(SubtitleSpan {
            first,
            count: row - first,
        });
    }
    None
}

/// Collects all contiguous removal spans for `removeOverlapping`, adjusting each
/// returned `first` index to match the live model after prior span removals.
pub fn collect_overlapping_spans(
    items: &[SubtitleCueView<'_>],
    start: f64,
    end: f64,
) -> Vec<SubtitleSpan> {
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
        if !(items[i].start < end_limit && start_limit < items[i].end) {
            i += 1;
            continue;
        }
        let first_orig = i;
        while i < items.len() && items[i].start < end_limit && start_limit < items[i].end {
            i += 1;
        }
        let count = i - first_orig;
        spans.push(SubtitleSpan {
            first: first_orig - current_offset,
            count,
        });
        current_offset += count;
    }
    spans
}

/// Returns the row indices whose time intervals overlap `position`.
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

/// Compares two cues for equality under `TIME_DELTA` and exact text match.
pub fn same_entry(lhs: &SubtitleCueView<'_>, rhs: &SubtitleCueView<'_>) -> bool {
    (lhs.start - rhs.start).abs() <= TIME_DELTA
        && (lhs.end - rhs.end).abs() <= TIME_DELTA
        && lhs.text == rhs.text
}

/// Computes common prefix and suffix to minimize row updates when replacing items.
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

    ModelDiffPlan {
        prefix,
        suffix,
        old_mid_count: old_items.len() - prefix - suffix,
        new_mid_count: new_items.len() - prefix - suffix,
    }
}

/// Filters valid candidate cues and returns their `original_index`es stably sorted by `start`.
pub fn filter_and_sort_cues(candidates: &[CandidateCue]) -> Vec<usize> {
    let mut surviving: Vec<(usize, f64)> = candidates
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
        .map(|c| (c.original_index, c.start))
        .collect();

    surviving.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
    surviving.into_iter().map(|(idx, _)| idx).collect()
}

/// Determines whether to update an in-memory subtitle track and whether to mark it as a memory track.
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

/// An incoming subtitle track representation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubtitleTrackInput<'a> {
    pub is_null: bool,
    pub track_id: i64,
    pub is_external: bool,
    pub external_path: &'a str,
}

impl<'a> SubtitleTrackInput<'a> {
    pub fn new(
        is_null: bool,
        track_id: i64,
        is_external: bool,
        external_path: &'a str,
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
    pub path: &'a str,
}

impl<'a> SubtitleExternalPathInput<'a> {
    pub fn new(track_id: i64, path: &'a str) -> Self {
        Self { track_id, path }
    }
}

/// Actions to execute when reconciling subtitle track models with mpv's track list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TrackAction<'a> {
    CreateModel {
        track_id: i64,
    },
    ReloadExternal {
        track_id: i64,
        path: &'a str,
        read_file: bool,
    },
    ClearExternal {
        track_id: i64,
    },
    RemoveTrack {
        track_id: i64,
    },
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TrackUpdatePlan<'a> {
    pub actions: Vec<TrackAction<'a>>,
}

/// Computes the sequence of actions required to synchronize subtitle track models with mpv's track list.
pub fn plan_track_changes<'a>(
    tracks: &[SubtitleTrackInput<'a>],
    existing_model_ids: &[i64],
    existing_external_paths: &[SubtitleExternalPathInput<'_>],
) -> TrackUpdatePlan<'a> {
    let mut actions = Vec::new();
    let mut seen_track_ids: HashSet<i64> = HashSet::new();
    let mut models_set: HashSet<i64> = existing_model_ids.iter().copied().collect();
    let mut external_paths_map: HashMap<i64, &str> = existing_external_paths
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
            let needs_reload = external_paths_map
                .get(&track_id)
                .is_none_or(|&existing_path| existing_path != path);

            if needs_reload {
                external_paths_map.insert(track_id, path);
                actions.push(TrackAction::ReloadExternal {
                    track_id,
                    path,
                    read_file: !path.is_empty(),
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
pub enum ActiveCueActionKind {
    Noop,
    AddAndSelect,
    ClearSelection,
}

#[derive(Copy, Clone, Debug, PartialEq)]
pub struct ActiveCuePlan {
    pub action: ActiveCueActionKind,
    pub raw_position: f64,
}

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
        '\u{03c2}' => out.push('\u{03c3}'),
        '\u{00b5}' => out.push('\u{03bc}'),
        '\u{017f}' => out.push('s'),
        '\u{2126}' => out.push('\u{03c9}'),
        '\u{212a}' => out.push('k'),
        '\u{212b}' => out.push('\u{00e5}'),
        _ => out.extend(ch.to_lowercase()),
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

/// Finds all row indices in `candidates` that match `query` case-insensitively.
pub fn find_matching_rows<T: AsRef<str>>(
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_plan_add_subtitle_limits() {
        let items = [SubtitleCueView::new("hello", 1.0, 2.0)];

        let plan = plan_add_subtitle(&items, true, "hello", 3.0, 4.0);
        assert_eq!(plan.action, AddAction::Reject);
        assert_eq!(plan.index, -1);

        let ok_text = "a".repeat(2000);
        let plan = plan_add_subtitle(&items, false, &ok_text, 3.0, 4.0);
        assert_eq!(plan.action, AddAction::Insert);
        assert_eq!(plan.index, 1);

        let long_text = "a".repeat(2001);
        let plan = plan_add_subtitle(&items, false, &long_text, 3.0, 4.0);
        assert_eq!(plan.action, AddAction::Reject);

        let ok_nl = "a\n".repeat(15);
        assert_eq!(plan_add_subtitle(&items, false, &ok_nl, 3.0, 4.0).action, AddAction::Insert);

        let too_many_nl = "a\n".repeat(16);
        assert_eq!(plan_add_subtitle(&items, false, &too_many_nl, 3.0, 4.0).action, AddAction::Reject);

        for bad_start in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let plan = plan_add_subtitle(&items, false, "hello", bad_start, 4.0);
            assert_eq!(plan, AddPlan { action: AddAction::Reject, index: -1 });
        }
    }

    #[test]
    fn test_plan_add_subtitle_ordering_and_dedup() {
        let items = [
            SubtitleCueView::new("A", 1.0, 2.0),
            SubtitleCueView::new("B", 3.0, 4.0),
        ];
        assert_eq!(plan_add_subtitle(&items, false, "C", 0.5, 1.0), AddPlan { action: AddAction::Insert, index: 0 });
        assert_eq!(plan_add_subtitle(&items, false, "C", 2.0, 2.5), AddPlan { action: AddAction::Insert, index: 1 });
        assert_eq!(plan_add_subtitle(&items, false, "C", 5.0, 6.0), AddPlan { action: AddAction::Insert, index: 2 });
        assert_eq!(plan_add_subtitle(&items, false, "A", 1.0, 2.0), AddPlan { action: AddAction::Existing, index: 0 });
        assert_eq!(plan_add_subtitle(&items, false, "A", 1.00005, 2.0), AddPlan { action: AddAction::Existing, index: 0 });
        assert_eq!(plan_add_subtitle(&items, false, "A", 0.99995, 2.0), AddPlan { action: AddAction::Existing, index: 0 });
        assert_eq!(plan_add_subtitle(&items, false, "C", 1.0, 2.0), AddPlan { action: AddAction::Insert, index: 1 });
    }

    #[test]
    fn test_overlapping_spans() {
        let items = [
            SubtitleCueView::new("text", 1.0, 2.0),
            SubtitleCueView::new("text", 2.5, 3.5),
            SubtitleCueView::new("text", 3.0, 4.0),
            SubtitleCueView::new("text", 5.0, 6.0),
        ];
        assert!(collect_overlapping_spans(&items, 3.0, 3.0).is_empty());
        assert!(collect_overlapping_spans(&items, 4.0, 2.0).is_empty());
        assert_eq!(collect_overlapping_spans(&items, 2.0, 4.5), vec![SubtitleSpan { first: 1, count: 2 }]);
        assert!(collect_overlapping_spans(&items, 2.0001, 2.4).is_empty());

        let disjoint_items = [
            SubtitleCueView::new("text", 0.5, 3.0),
            SubtitleCueView::new("text", 1.0, 1.5),
            SubtitleCueView::new("text", 2.5, 3.5),
        ];
        assert_eq!(
            collect_overlapping_spans(&disjoint_items, 2.0, 4.0),
            vec![
                SubtitleSpan { first: 0, count: 1 },
                SubtitleSpan { first: 1, count: 1 },
            ]
        );
    }

    #[test]
    fn test_select_position_rows() {
        let items = [
            SubtitleCueView::new("cue", 1.0, 5.0),
            SubtitleCueView::new("cue", 2.0, 4.0),
            SubtitleCueView::new("cue", 4.5, 6.0),
        ];
        assert_eq!(select_position_rows(&items, 3.0), vec![0, 1]);
        assert_eq!(select_position_rows(&items, 4.7), vec![0, 2]);
        assert_eq!(select_position_rows(&items, 0.5), Vec::<usize>::new());
        assert_eq!(select_position_rows(&items, f64::NAN), Vec::<usize>::new());
    }

    #[test]
    fn test_diff_items() {
        let old = [
            SubtitleCueView::new("A", 1.0, 2.0),
            SubtitleCueView::new("B", 2.0, 3.0),
            SubtitleCueView::new("C", 3.0, 4.0),
            SubtitleCueView::new("D", 4.0, 5.0),
            SubtitleCueView::new("E", 5.0, 6.0),
        ];
        assert_eq!(
            diff_items(&old, &old),
            ModelDiffPlan { prefix: 5, suffix: 0, old_mid_count: 0, new_mid_count: 0 }
        );

        let new_items = [
            SubtitleCueView::new("A", 1.0, 2.0),
            SubtitleCueView::new("B", 2.0, 3.0),
            SubtitleCueView::new("X", 3.0, 3.5),
            SubtitleCueView::new("Y", 3.5, 4.0),
            SubtitleCueView::new("D", 4.0, 5.0),
            SubtitleCueView::new("E", 5.0, 6.0),
        ];
        assert_eq!(
            diff_items(&old, &new_items),
            ModelDiffPlan { prefix: 2, suffix: 2, old_mid_count: 1, new_mid_count: 2 }
        );
        assert_eq!(
            diff_items(&[], &old),
            ModelDiffPlan { prefix: 0, suffix: 0, old_mid_count: 0, new_mid_count: 5 }
        );
        assert_eq!(
            diff_items(&old, &[]),
            ModelDiffPlan { prefix: 0, suffix: 0, old_mid_count: 5, new_mid_count: 0 }
        );
    }

    #[test]
    fn test_filter_and_sort_cues() {
        let valid = |idx: usize, start: f64, end: f64| CandidateCue {
            original_index: idx,
            id_valid: true,
            cue_id: (idx + 1) as i64,
            start_valid: true,
            start,
            end_valid: true,
            end,
        };

        let candidates = [valid(0, 10.0, 12.0), valid(1, 2.0, 4.0), valid(2, 5.0, 8.0)];
        assert_eq!(filter_and_sort_cues(&candidates), vec![1, 2, 0]);

        let mixed = vec![
            valid(0, 1.0, 2.0),
            CandidateCue { id_valid: false, ..valid(1, 0.5, 1.5) },
            CandidateCue { cue_id: 0, ..valid(2, 0.5, 1.5) },
            CandidateCue { cue_id: -5, ..valid(3, 0.5, 1.5) },
            CandidateCue { start_valid: false, ..valid(4, 0.5, 1.5) },
            CandidateCue { end_valid: false, ..valid(5, 0.5, 1.5) },
            CandidateCue { start: f64::NAN, ..valid(6, 0.5, 1.5) },
            CandidateCue { end: f64::NAN, ..valid(7, 0.5, 1.5) },
            CandidateCue { start: f64::INFINITY, ..valid(8, 0.5, 1.5) },
            CandidateCue { end: f64::INFINITY, ..valid(9, 0.5, 1.5) },
            CandidateCue { start: f64::NEG_INFINITY, ..valid(10, 0.5, 1.5) },
            CandidateCue { end: f64::NEG_INFINITY, ..valid(11, 0.5, 1.5) },
            CandidateCue { start: -0.001, ..valid(12, -0.001, 1.5) },
            CandidateCue { start: 5.0, end: 5.0, ..valid(13, 5.0, 5.0) },
            CandidateCue { start: 6.0, end: 4.0, ..valid(14, 6.0, 4.0) },
            valid(15, 0.0, 0.5),
        ];
        assert_eq!(filter_and_sort_cues(&mixed), vec![15, 0]);

        let equal_starts = [
            valid(100, 3.0, 5.0),
            valid(101, 1.0, 4.0),
            valid(102, 3.0, 6.0),
            valid(103, 1.0, 2.0),
            valid(104, 3.0, 4.0),
        ];
        assert_eq!(filter_and_sort_cues(&equal_starts), vec![101, 103, 100, 102, 104]);
        assert_eq!(filter_and_sort_cues(&[]), Vec::<usize>::new());
    }

    #[test]
    fn test_should_update_memory_track() {
        assert_eq!(should_update_memory_track(0, true, true, true), (false, false));
        assert_eq!(should_update_memory_track(-1, true, true, false), (false, false));
        assert_eq!(should_update_memory_track(1, false, true, true), (false, false));
        assert_eq!(should_update_memory_track(1, true, true, false), (true, true));
        assert_eq!(should_update_memory_track(1, true, false, true), (true, false));
        assert_eq!(should_update_memory_track(1, true, false, false), (false, false));
    }

    #[test]
    fn test_plan_active_cue() {
        assert_eq!(plan_active_cue(false, false, true, 12.5).action, ActiveCueActionKind::Noop);
        assert_eq!(plan_active_cue(true, true, false, 12.5).action, ActiveCueActionKind::ClearSelection);
        assert_eq!(
            plan_active_cue(true, true, true, 12.5),
            ActiveCuePlan { action: ActiveCueActionKind::AddAndSelect, raw_position: 12.5 }
        );
    }

    #[test]
    fn test_plan_track_changes() {
        let tracks = [
            SubtitleTrackInput::new(true, 1, false, ""),
            SubtitleTrackInput::new(false, 0, false, ""),
            SubtitleTrackInput::new(false, -5, false, ""),
        ];
        assert!(plan_track_changes(&tracks, &[], &[]).actions.is_empty());

        let tracks = [
            SubtitleTrackInput::new(false, 1, false, ""),
            SubtitleTrackInput::new(false, 2, true, "path1.srt"),
            SubtitleTrackInput::new(false, 3, true, ""),
        ];
        assert_eq!(
            plan_track_changes(&tracks, &[], &[]).actions,
            vec![
                TrackAction::CreateModel { track_id: 1 },
                TrackAction::CreateModel { track_id: 2 },
                TrackAction::ReloadExternal { track_id: 2, path: "path1.srt", read_file: true },
                TrackAction::CreateModel { track_id: 3 },
                TrackAction::ReloadExternal { track_id: 3, path: "", read_file: false },
            ]
        );

        let ext_paths = [SubtitleExternalPathInput::new(2, "path1.srt")];
        assert!(plan_track_changes(&tracks[0..2], &[1, 2], &ext_paths).actions.is_empty());

        let tracks = [
            SubtitleTrackInput::new(false, 1, true, "path2.srt"),
            SubtitleTrackInput::new(false, 2, false, ""),
        ];
        assert_eq!(
            plan_track_changes(&tracks, &[1, 2], &ext_paths).actions,
            vec![
                TrackAction::ReloadExternal { track_id: 1, path: "path2.srt", read_file: true },
                TrackAction::ClearExternal { track_id: 2 },
            ]
        );

        let tracks = [SubtitleTrackInput::new(false, 2, false, "")];
        assert_eq!(
            plan_track_changes(&tracks, &[1, 2, 3], &[]).actions,
            vec![
                TrackAction::RemoveTrack { track_id: 1 },
                TrackAction::RemoveTrack { track_id: 3 },
            ]
        );
    }

    #[test]
    fn test_find_matching_rows() {
        let cues = [
            "Hello World",
            "日本語の字幕テスト",
            "Line 1\nLine 2\r\nLine 3",
            "Fullwidth\u{3000}space\u{3000}test",
            "Σίσυφος",
            "Deseret \u{10400} test",
        ];

        assert_eq!(find_matching_rows(&cues, "", false), Vec::<usize>::new());
        assert_eq!(find_matching_rows(&cues, "   \t\n\r", true), Vec::<usize>::new());
        assert_eq!(find_matching_rows(&cues, " ", false), vec![0, 2, 5]);
        assert_eq!(find_matching_rows(&cues, "\u{3000}", false), vec![3]);
        assert_eq!(find_matching_rows(&cues, "hello", false), vec![0]);
        assert_eq!(find_matching_rows(&cues, "h e l l o", true), vec![0]);
        assert_eq!(find_matching_rows(&cues, "字幕", false), vec![1]);
        assert_eq!(find_matching_rows(&cues, "1Line", false), Vec::<usize>::new());
        assert_eq!(find_matching_rows(&cues, "1Line", true), vec![2]);
        assert_eq!(find_matching_rows(&cues, "Fullwidth\u{3000}space", true), vec![3]);
        assert_eq!(find_matching_rows(&cues, "σ", false), vec![4]);
        assert_eq!(find_matching_rows(&cues, "ς", false), vec![4]);
        assert_eq!(find_matching_rows(&cues, "ΣΊΣΥΦΟΣ", false), vec![4]);
        assert_eq!(find_matching_rows(&cues, "\u{10428}", false), vec![5]);
    }

    #[test]
    fn test_find_first_overlapping_span() {
        let items = [
            SubtitleCueView::new("span", 1.0, 2.0),
            SubtitleCueView::new("span", 2.5, 3.5),
            SubtitleCueView::new("span", 3.0, 4.0),
            SubtitleCueView::new("span", 5.0, 6.0),
        ];
        assert_eq!(find_first_overlapping_span(&items, 2.0, 4.5, 0), Some(SubtitleSpan { first: 1, count: 2 }));
        assert_eq!(find_first_overlapping_span(&items, 1.0, 2.0, 4), None);
        assert_eq!(find_first_overlapping_span(&items, 3.0, 3.0, 0), None);
        assert_eq!(find_first_overlapping_span(&items, 1.0, 6.0, 3), Some(SubtitleSpan { first: 3, count: 1 }));
    }
}

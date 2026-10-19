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
    if block_adds {
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
    if end <= start || from_row >= items.len() {
        return None;
    }
    let mut row = from_row;
    while row < items.len() {
        let overlaps = items[row].start < end - TIME_DELTA && start + TIME_DELTA < items[row].end;
        if !overlaps {
            row += 1;
            continue;
        }
        let first = row;
        while row < items.len() {
            let item = &items[row];
            if !(item.start < end - TIME_DELTA && start + TIME_DELTA < item.end) {
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
    if end <= start {
        return Vec::new();
    }
    let mut spans = Vec::new();
    let mut current_offset: usize = 0;
    let mut i = 0;
    while i < items.len() {
        let overlaps = items[i].start < end - TIME_DELTA && start + TIME_DELTA < items[i].end;
        if !overlaps {
            i += 1;
            continue;
        }
        let first_orig = i;
        while i < items.len() {
            if !(items[i].start < end - TIME_DELTA && start + TIME_DELTA < items[i].end) {
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
    let mut rows = Vec::new();
    for (i, item) in items.iter().enumerate() {
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
// Subtitle Model Find: whitespace stripping & Unicode case folding
// ---------------------------------------------------------------------------

const BMP_CASE_FOLD: &[(u16, u16)] = &[
    (0x00b5, 0x03bc), (0x00c0, 0x00e0), (0x00c1, 0x00e1), (0x00c2, 0x00e2), (0x00c3, 0x00e3), (0x00c4, 0x00e4), (0x00c5, 0x00e5), (0x00c6, 0x00e6), 
    (0x00c7, 0x00e7), (0x00c8, 0x00e8), (0x00c9, 0x00e9), (0x00ca, 0x00ea), (0x00cb, 0x00eb), (0x00cc, 0x00ec), (0x00cd, 0x00ed), (0x00ce, 0x00ee), 
    (0x00cf, 0x00ef), (0x00d0, 0x00f0), (0x00d1, 0x00f1), (0x00d2, 0x00f2), (0x00d3, 0x00f3), (0x00d4, 0x00f4), (0x00d5, 0x00f5), (0x00d6, 0x00f6), 
    (0x00d8, 0x00f8), (0x00d9, 0x00f9), (0x00da, 0x00fa), (0x00db, 0x00fb), (0x00dc, 0x00fc), (0x00dd, 0x00fd), (0x00de, 0x00fe), (0x0100, 0x0101), 
    (0x0102, 0x0103), (0x0104, 0x0105), (0x0106, 0x0107), (0x0108, 0x0109), (0x010a, 0x010b), (0x010c, 0x010d), (0x010e, 0x010f), (0x0110, 0x0111), 
    (0x0112, 0x0113), (0x0114, 0x0115), (0x0116, 0x0117), (0x0118, 0x0119), (0x011a, 0x011b), (0x011c, 0x011d), (0x011e, 0x011f), (0x0120, 0x0121), 
    (0x0122, 0x0123), (0x0124, 0x0125), (0x0126, 0x0127), (0x0128, 0x0129), (0x012a, 0x012b), (0x012c, 0x012d), (0x012e, 0x012f), (0x0132, 0x0133), 
    (0x0134, 0x0135), (0x0136, 0x0137), (0x0139, 0x013a), (0x013b, 0x013c), (0x013d, 0x013e), (0x013f, 0x0140), (0x0141, 0x0142), (0x0143, 0x0144), 
    (0x0145, 0x0146), (0x0147, 0x0148), (0x014a, 0x014b), (0x014c, 0x014d), (0x014e, 0x014f), (0x0150, 0x0151), (0x0152, 0x0153), (0x0154, 0x0155), 
    (0x0156, 0x0157), (0x0158, 0x0159), (0x015a, 0x015b), (0x015c, 0x015d), (0x015e, 0x015f), (0x0160, 0x0161), (0x0162, 0x0163), (0x0164, 0x0165), 
    (0x0166, 0x0167), (0x0168, 0x0169), (0x016a, 0x016b), (0x016c, 0x016d), (0x016e, 0x016f), (0x0170, 0x0171), (0x0172, 0x0173), (0x0174, 0x0175), 
    (0x0176, 0x0177), (0x0178, 0x00ff), (0x0179, 0x017a), (0x017b, 0x017c), (0x017d, 0x017e), (0x017f, 0x0073), (0x0181, 0x0253), (0x0182, 0x0183), 
    (0x0184, 0x0185), (0x0186, 0x0254), (0x0187, 0x0188), (0x0189, 0x0256), (0x018a, 0x0257), (0x018b, 0x018c), (0x018e, 0x01dd), (0x018f, 0x0259), 
    (0x0190, 0x025b), (0x0191, 0x0192), (0x0193, 0x0260), (0x0194, 0x0263), (0x0196, 0x0269), (0x0197, 0x0268), (0x0198, 0x0199), (0x019c, 0x026f), 
    (0x019d, 0x0272), (0x019f, 0x0275), (0x01a0, 0x01a1), (0x01a2, 0x01a3), (0x01a4, 0x01a5), (0x01a6, 0x0280), (0x01a7, 0x01a8), (0x01a9, 0x0283), 
    (0x01ac, 0x01ad), (0x01ae, 0x0288), (0x01af, 0x01b0), (0x01b1, 0x028a), (0x01b2, 0x028b), (0x01b3, 0x01b4), (0x01b5, 0x01b6), (0x01b7, 0x0292), 
    (0x01b8, 0x01b9), (0x01bc, 0x01bd), (0x01c4, 0x01c6), (0x01c5, 0x01c6), (0x01c7, 0x01c9), (0x01c8, 0x01c9), (0x01ca, 0x01cc), (0x01cb, 0x01cc), 
    (0x01cd, 0x01ce), (0x01cf, 0x01d0), (0x01d1, 0x01d2), (0x01d3, 0x01d4), (0x01d5, 0x01d6), (0x01d7, 0x01d8), (0x01d9, 0x01da), (0x01db, 0x01dc), 
    (0x01de, 0x01df), (0x01e0, 0x01e1), (0x01e2, 0x01e3), (0x01e4, 0x01e5), (0x01e6, 0x01e7), (0x01e8, 0x01e9), (0x01ea, 0x01eb), (0x01ec, 0x01ed), 
    (0x01ee, 0x01ef), (0x01f1, 0x01f3), (0x01f2, 0x01f3), (0x01f4, 0x01f5), (0x01f6, 0x0195), (0x01f7, 0x01bf), (0x01f8, 0x01f9), (0x01fa, 0x01fb), 
    (0x01fc, 0x01fd), (0x01fe, 0x01ff), (0x0200, 0x0201), (0x0202, 0x0203), (0x0204, 0x0205), (0x0206, 0x0207), (0x0208, 0x0209), (0x020a, 0x020b), 
    (0x020c, 0x020d), (0x020e, 0x020f), (0x0210, 0x0211), (0x0212, 0x0213), (0x0214, 0x0215), (0x0216, 0x0217), (0x0218, 0x0219), (0x021a, 0x021b), 
    (0x021c, 0x021d), (0x021e, 0x021f), (0x0220, 0x019e), (0x0222, 0x0223), (0x0224, 0x0225), (0x0226, 0x0227), (0x0228, 0x0229), (0x022a, 0x022b), 
    (0x022c, 0x022d), (0x022e, 0x022f), (0x0230, 0x0231), (0x0232, 0x0233), (0x023a, 0x2c65), (0x023b, 0x023c), (0x023d, 0x019a), (0x023e, 0x2c66), 
    (0x0241, 0x0242), (0x0243, 0x0180), (0x0244, 0x0289), (0x0245, 0x028c), (0x0246, 0x0247), (0x0248, 0x0249), (0x024a, 0x024b), (0x024c, 0x024d), 
    (0x024e, 0x024f), (0x0345, 0x03b9), (0x0370, 0x0371), (0x0372, 0x0373), (0x0376, 0x0377), (0x037f, 0x03f3), (0x0386, 0x03ac), (0x0388, 0x03ad), 
    (0x0389, 0x03ae), (0x038a, 0x03af), (0x038c, 0x03cc), (0x038e, 0x03cd), (0x038f, 0x03ce), (0x0391, 0x03b1), (0x0392, 0x03b2), (0x0393, 0x03b3), 
    (0x0394, 0x03b4), (0x0395, 0x03b5), (0x0396, 0x03b6), (0x0397, 0x03b7), (0x0398, 0x03b8), (0x0399, 0x03b9), (0x039a, 0x03ba), (0x039b, 0x03bb), 
    (0x039c, 0x03bc), (0x039d, 0x03bd), (0x039e, 0x03be), (0x039f, 0x03bf), (0x03a0, 0x03c0), (0x03a1, 0x03c1), (0x03a3, 0x03c3), (0x03a4, 0x03c4), 
    (0x03a5, 0x03c5), (0x03a6, 0x03c6), (0x03a7, 0x03c7), (0x03a8, 0x03c8), (0x03a9, 0x03c9), (0x03aa, 0x03ca), (0x03ab, 0x03cb), (0x03c2, 0x03c3), 
    (0x03cf, 0x03d7), (0x03d0, 0x03b2), (0x03d1, 0x03b8), (0x03d5, 0x03c6), (0x03d6, 0x03c0), (0x03d8, 0x03d9), (0x03da, 0x03db), (0x03dc, 0x03dd), 
    (0x03de, 0x03df), (0x03e0, 0x03e1), (0x03e2, 0x03e3), (0x03e4, 0x03e5), (0x03e6, 0x03e7), (0x03e8, 0x03e9), (0x03ea, 0x03eb), (0x03ec, 0x03ed), 
    (0x03ee, 0x03ef), (0x03f0, 0x03ba), (0x03f1, 0x03c1), (0x03f4, 0x03b8), (0x03f5, 0x03b5), (0x03f7, 0x03f8), (0x03f9, 0x03f2), (0x03fa, 0x03fb), 
    (0x03fd, 0x037b), (0x03fe, 0x037c), (0x03ff, 0x037d), (0x0400, 0x0450), (0x0401, 0x0451), (0x0402, 0x0452), (0x0403, 0x0453), (0x0404, 0x0454), 
    (0x0405, 0x0455), (0x0406, 0x0456), (0x0407, 0x0457), (0x0408, 0x0458), (0x0409, 0x0459), (0x040a, 0x045a), (0x040b, 0x045b), (0x040c, 0x045c), 
    (0x040d, 0x045d), (0x040e, 0x045e), (0x040f, 0x045f), (0x0410, 0x0430), (0x0411, 0x0431), (0x0412, 0x0432), (0x0413, 0x0433), (0x0414, 0x0434), 
    (0x0415, 0x0435), (0x0416, 0x0436), (0x0417, 0x0437), (0x0418, 0x0438), (0x0419, 0x0439), (0x041a, 0x043a), (0x041b, 0x043b), (0x041c, 0x043c), 
    (0x041d, 0x043d), (0x041e, 0x043e), (0x041f, 0x043f), (0x0420, 0x0440), (0x0421, 0x0441), (0x0422, 0x0442), (0x0423, 0x0443), (0x0424, 0x0444), 
    (0x0425, 0x0445), (0x0426, 0x0446), (0x0427, 0x0447), (0x0428, 0x0448), (0x0429, 0x0449), (0x042a, 0x044a), (0x042b, 0x044b), (0x042c, 0x044c), 
    (0x042d, 0x044d), (0x042e, 0x044e), (0x042f, 0x044f), (0x0460, 0x0461), (0x0462, 0x0463), (0x0464, 0x0465), (0x0466, 0x0467), (0x0468, 0x0469), 
    (0x046a, 0x046b), (0x046c, 0x046d), (0x046e, 0x046f), (0x0470, 0x0471), (0x0472, 0x0473), (0x0474, 0x0475), (0x0476, 0x0477), (0x0478, 0x0479), 
    (0x047a, 0x047b), (0x047c, 0x047d), (0x047e, 0x047f), (0x0480, 0x0481), (0x048a, 0x048b), (0x048c, 0x048d), (0x048e, 0x048f), (0x0490, 0x0491), 
    (0x0492, 0x0493), (0x0494, 0x0495), (0x0496, 0x0497), (0x0498, 0x0499), (0x049a, 0x049b), (0x049c, 0x049d), (0x049e, 0x049f), (0x04a0, 0x04a1), 
    (0x04a2, 0x04a3), (0x04a4, 0x04a5), (0x04a6, 0x04a7), (0x04a8, 0x04a9), (0x04aa, 0x04ab), (0x04ac, 0x04ad), (0x04ae, 0x04af), (0x04b0, 0x04b1), 
    (0x04b2, 0x04b3), (0x04b4, 0x04b5), (0x04b6, 0x04b7), (0x04b8, 0x04b9), (0x04ba, 0x04bb), (0x04bc, 0x04bd), (0x04be, 0x04bf), (0x04c0, 0x04cf), 
    (0x04c1, 0x04c2), (0x04c3, 0x04c4), (0x04c5, 0x04c6), (0x04c7, 0x04c8), (0x04c9, 0x04ca), (0x04cb, 0x04cc), (0x04cd, 0x04ce), (0x04d0, 0x04d1), 
    (0x04d2, 0x04d3), (0x04d4, 0x04d5), (0x04d6, 0x04d7), (0x04d8, 0x04d9), (0x04da, 0x04db), (0x04dc, 0x04dd), (0x04de, 0x04df), (0x04e0, 0x04e1), 
    (0x04e2, 0x04e3), (0x04e4, 0x04e5), (0x04e6, 0x04e7), (0x04e8, 0x04e9), (0x04ea, 0x04eb), (0x04ec, 0x04ed), (0x04ee, 0x04ef), (0x04f0, 0x04f1), 
    (0x04f2, 0x04f3), (0x04f4, 0x04f5), (0x04f6, 0x04f7), (0x04f8, 0x04f9), (0x04fa, 0x04fb), (0x04fc, 0x04fd), (0x04fe, 0x04ff), (0x0500, 0x0501), 
    (0x0502, 0x0503), (0x0504, 0x0505), (0x0506, 0x0507), (0x0508, 0x0509), (0x050a, 0x050b), (0x050c, 0x050d), (0x050e, 0x050f), (0x0510, 0x0511), 
    (0x0512, 0x0513), (0x0514, 0x0515), (0x0516, 0x0517), (0x0518, 0x0519), (0x051a, 0x051b), (0x051c, 0x051d), (0x051e, 0x051f), (0x0520, 0x0521), 
    (0x0522, 0x0523), (0x0524, 0x0525), (0x0526, 0x0527), (0x0528, 0x0529), (0x052a, 0x052b), (0x052c, 0x052d), (0x052e, 0x052f), (0x0531, 0x0561), 
    (0x0532, 0x0562), (0x0533, 0x0563), (0x0534, 0x0564), (0x0535, 0x0565), (0x0536, 0x0566), (0x0537, 0x0567), (0x0538, 0x0568), (0x0539, 0x0569), 
    (0x053a, 0x056a), (0x053b, 0x056b), (0x053c, 0x056c), (0x053d, 0x056d), (0x053e, 0x056e), (0x053f, 0x056f), (0x0540, 0x0570), (0x0541, 0x0571), 
    (0x0542, 0x0572), (0x0543, 0x0573), (0x0544, 0x0574), (0x0545, 0x0575), (0x0546, 0x0576), (0x0547, 0x0577), (0x0548, 0x0578), (0x0549, 0x0579), 
    (0x054a, 0x057a), (0x054b, 0x057b), (0x054c, 0x057c), (0x054d, 0x057d), (0x054e, 0x057e), (0x054f, 0x057f), (0x0550, 0x0580), (0x0551, 0x0581), 
    (0x0552, 0x0582), (0x0553, 0x0583), (0x0554, 0x0584), (0x0555, 0x0585), (0x0556, 0x0586), (0x10a0, 0x2d00), (0x10a1, 0x2d01), (0x10a2, 0x2d02), 
    (0x10a3, 0x2d03), (0x10a4, 0x2d04), (0x10a5, 0x2d05), (0x10a6, 0x2d06), (0x10a7, 0x2d07), (0x10a8, 0x2d08), (0x10a9, 0x2d09), (0x10aa, 0x2d0a), 
    (0x10ab, 0x2d0b), (0x10ac, 0x2d0c), (0x10ad, 0x2d0d), (0x10ae, 0x2d0e), (0x10af, 0x2d0f), (0x10b0, 0x2d10), (0x10b1, 0x2d11), (0x10b2, 0x2d12), 
    (0x10b3, 0x2d13), (0x10b4, 0x2d14), (0x10b5, 0x2d15), (0x10b6, 0x2d16), (0x10b7, 0x2d17), (0x10b8, 0x2d18), (0x10b9, 0x2d19), (0x10ba, 0x2d1a), 
    (0x10bb, 0x2d1b), (0x10bc, 0x2d1c), (0x10bd, 0x2d1d), (0x10be, 0x2d1e), (0x10bf, 0x2d1f), (0x10c0, 0x2d20), (0x10c1, 0x2d21), (0x10c2, 0x2d22), 
    (0x10c3, 0x2d23), (0x10c4, 0x2d24), (0x10c5, 0x2d25), (0x10c7, 0x2d27), (0x10cd, 0x2d2d), (0x13f8, 0x13f0), (0x13f9, 0x13f1), (0x13fa, 0x13f2), 
    (0x13fb, 0x13f3), (0x13fc, 0x13f4), (0x13fd, 0x13f5), (0x1c80, 0x0432), (0x1c81, 0x0434), (0x1c82, 0x043e), (0x1c83, 0x0441), (0x1c84, 0x0442), 
    (0x1c85, 0x0442), (0x1c86, 0x044a), (0x1c87, 0x0463), (0x1c88, 0xa64b), (0x1c89, 0x1c8a), (0x1c90, 0x10d0), (0x1c91, 0x10d1), (0x1c92, 0x10d2), 
    (0x1c93, 0x10d3), (0x1c94, 0x10d4), (0x1c95, 0x10d5), (0x1c96, 0x10d6), (0x1c97, 0x10d7), (0x1c98, 0x10d8), (0x1c99, 0x10d9), (0x1c9a, 0x10da), 
    (0x1c9b, 0x10db), (0x1c9c, 0x10dc), (0x1c9d, 0x10dd), (0x1c9e, 0x10de), (0x1c9f, 0x10df), (0x1ca0, 0x10e0), (0x1ca1, 0x10e1), (0x1ca2, 0x10e2), 
    (0x1ca3, 0x10e3), (0x1ca4, 0x10e4), (0x1ca5, 0x10e5), (0x1ca6, 0x10e6), (0x1ca7, 0x10e7), (0x1ca8, 0x10e8), (0x1ca9, 0x10e9), (0x1caa, 0x10ea), 
    (0x1cab, 0x10eb), (0x1cac, 0x10ec), (0x1cad, 0x10ed), (0x1cae, 0x10ee), (0x1caf, 0x10ef), (0x1cb0, 0x10f0), (0x1cb1, 0x10f1), (0x1cb2, 0x10f2), 
    (0x1cb3, 0x10f3), (0x1cb4, 0x10f4), (0x1cb5, 0x10f5), (0x1cb6, 0x10f6), (0x1cb7, 0x10f7), (0x1cb8, 0x10f8), (0x1cb9, 0x10f9), (0x1cba, 0x10fa), 
    (0x1cbd, 0x10fd), (0x1cbe, 0x10fe), (0x1cbf, 0x10ff), (0x1e00, 0x1e01), (0x1e02, 0x1e03), (0x1e04, 0x1e05), (0x1e06, 0x1e07), (0x1e08, 0x1e09), 
    (0x1e0a, 0x1e0b), (0x1e0c, 0x1e0d), (0x1e0e, 0x1e0f), (0x1e10, 0x1e11), (0x1e12, 0x1e13), (0x1e14, 0x1e15), (0x1e16, 0x1e17), (0x1e18, 0x1e19), 
    (0x1e1a, 0x1e1b), (0x1e1c, 0x1e1d), (0x1e1e, 0x1e1f), (0x1e20, 0x1e21), (0x1e22, 0x1e23), (0x1e24, 0x1e25), (0x1e26, 0x1e27), (0x1e28, 0x1e29), 
    (0x1e2a, 0x1e2b), (0x1e2c, 0x1e2d), (0x1e2e, 0x1e2f), (0x1e30, 0x1e31), (0x1e32, 0x1e33), (0x1e34, 0x1e35), (0x1e36, 0x1e37), (0x1e38, 0x1e39), 
    (0x1e3a, 0x1e3b), (0x1e3c, 0x1e3d), (0x1e3e, 0x1e3f), (0x1e40, 0x1e41), (0x1e42, 0x1e43), (0x1e44, 0x1e45), (0x1e46, 0x1e47), (0x1e48, 0x1e49), 
    (0x1e4a, 0x1e4b), (0x1e4c, 0x1e4d), (0x1e4e, 0x1e4f), (0x1e50, 0x1e51), (0x1e52, 0x1e53), (0x1e54, 0x1e55), (0x1e56, 0x1e57), (0x1e58, 0x1e59), 
    (0x1e5a, 0x1e5b), (0x1e5c, 0x1e5d), (0x1e5e, 0x1e5f), (0x1e60, 0x1e61), (0x1e62, 0x1e63), (0x1e64, 0x1e65), (0x1e66, 0x1e67), (0x1e68, 0x1e69), 
    (0x1e6a, 0x1e6b), (0x1e6c, 0x1e6d), (0x1e6e, 0x1e6f), (0x1e70, 0x1e71), (0x1e72, 0x1e73), (0x1e74, 0x1e75), (0x1e76, 0x1e77), (0x1e78, 0x1e79), 
    (0x1e7a, 0x1e7b), (0x1e7c, 0x1e7d), (0x1e7e, 0x1e7f), (0x1e80, 0x1e81), (0x1e82, 0x1e83), (0x1e84, 0x1e85), (0x1e86, 0x1e87), (0x1e88, 0x1e89), 
    (0x1e8a, 0x1e8b), (0x1e8c, 0x1e8d), (0x1e8e, 0x1e8f), (0x1e90, 0x1e91), (0x1e92, 0x1e93), (0x1e94, 0x1e95), (0x1e9b, 0x1e61), (0x1e9e, 0x00df), 
    (0x1ea0, 0x1ea1), (0x1ea2, 0x1ea3), (0x1ea4, 0x1ea5), (0x1ea6, 0x1ea7), (0x1ea8, 0x1ea9), (0x1eaa, 0x1eab), (0x1eac, 0x1ead), (0x1eae, 0x1eaf), 
    (0x1eb0, 0x1eb1), (0x1eb2, 0x1eb3), (0x1eb4, 0x1eb5), (0x1eb6, 0x1eb7), (0x1eb8, 0x1eb9), (0x1eba, 0x1ebb), (0x1ebc, 0x1ebd), (0x1ebe, 0x1ebf), 
    (0x1ec0, 0x1ec1), (0x1ec2, 0x1ec3), (0x1ec4, 0x1ec5), (0x1ec6, 0x1ec7), (0x1ec8, 0x1ec9), (0x1eca, 0x1ecb), (0x1ecc, 0x1ecd), (0x1ece, 0x1ecf), 
    (0x1ed0, 0x1ed1), (0x1ed2, 0x1ed3), (0x1ed4, 0x1ed5), (0x1ed6, 0x1ed7), (0x1ed8, 0x1ed9), (0x1eda, 0x1edb), (0x1edc, 0x1edd), (0x1ede, 0x1edf), 
    (0x1ee0, 0x1ee1), (0x1ee2, 0x1ee3), (0x1ee4, 0x1ee5), (0x1ee6, 0x1ee7), (0x1ee8, 0x1ee9), (0x1eea, 0x1eeb), (0x1eec, 0x1eed), (0x1eee, 0x1eef), 
    (0x1ef0, 0x1ef1), (0x1ef2, 0x1ef3), (0x1ef4, 0x1ef5), (0x1ef6, 0x1ef7), (0x1ef8, 0x1ef9), (0x1efa, 0x1efb), (0x1efc, 0x1efd), (0x1efe, 0x1eff), 
    (0x1f08, 0x1f00), (0x1f09, 0x1f01), (0x1f0a, 0x1f02), (0x1f0b, 0x1f03), (0x1f0c, 0x1f04), (0x1f0d, 0x1f05), (0x1f0e, 0x1f06), (0x1f0f, 0x1f07), 
    (0x1f18, 0x1f10), (0x1f19, 0x1f11), (0x1f1a, 0x1f12), (0x1f1b, 0x1f13), (0x1f1c, 0x1f14), (0x1f1d, 0x1f15), (0x1f28, 0x1f20), (0x1f29, 0x1f21), 
    (0x1f2a, 0x1f22), (0x1f2b, 0x1f23), (0x1f2c, 0x1f24), (0x1f2d, 0x1f25), (0x1f2e, 0x1f26), (0x1f2f, 0x1f27), (0x1f38, 0x1f30), (0x1f39, 0x1f31), 
    (0x1f3a, 0x1f32), (0x1f3b, 0x1f33), (0x1f3c, 0x1f34), (0x1f3d, 0x1f35), (0x1f3e, 0x1f36), (0x1f3f, 0x1f37), (0x1f48, 0x1f40), (0x1f49, 0x1f41), 
    (0x1f4a, 0x1f42), (0x1f4b, 0x1f43), (0x1f4c, 0x1f44), (0x1f4d, 0x1f45), (0x1f59, 0x1f51), (0x1f5b, 0x1f53), (0x1f5d, 0x1f55), (0x1f5f, 0x1f57), 
    (0x1f68, 0x1f60), (0x1f69, 0x1f61), (0x1f6a, 0x1f62), (0x1f6b, 0x1f63), (0x1f6c, 0x1f64), (0x1f6d, 0x1f65), (0x1f6e, 0x1f66), (0x1f6f, 0x1f67), 
    (0x1f88, 0x1f80), (0x1f89, 0x1f81), (0x1f8a, 0x1f82), (0x1f8b, 0x1f83), (0x1f8c, 0x1f84), (0x1f8d, 0x1f85), (0x1f8e, 0x1f86), (0x1f8f, 0x1f87), 
    (0x1f98, 0x1f90), (0x1f99, 0x1f91), (0x1f9a, 0x1f92), (0x1f9b, 0x1f93), (0x1f9c, 0x1f94), (0x1f9d, 0x1f95), (0x1f9e, 0x1f96), (0x1f9f, 0x1f97), 
    (0x1fa8, 0x1fa0), (0x1fa9, 0x1fa1), (0x1faa, 0x1fa2), (0x1fab, 0x1fa3), (0x1fac, 0x1fa4), (0x1fad, 0x1fa5), (0x1fae, 0x1fa6), (0x1faf, 0x1fa7), 
    (0x1fb8, 0x1fb0), (0x1fb9, 0x1fb1), (0x1fba, 0x1f70), (0x1fbb, 0x1f71), (0x1fbc, 0x1fb3), (0x1fbe, 0x03b9), (0x1fc8, 0x1f72), (0x1fc9, 0x1f73), 
    (0x1fca, 0x1f74), (0x1fcb, 0x1f75), (0x1fcc, 0x1fc3), (0x1fd3, 0x0390), (0x1fd8, 0x1fd0), (0x1fd9, 0x1fd1), (0x1fda, 0x1f76), (0x1fdb, 0x1f77), 
    (0x1fe3, 0x03b0), (0x1fe8, 0x1fe0), (0x1fe9, 0x1fe1), (0x1fea, 0x1f7a), (0x1feb, 0x1f7b), (0x1fec, 0x1fe5), (0x1ff8, 0x1f78), (0x1ff9, 0x1f79), 
    (0x1ffa, 0x1f7c), (0x1ffb, 0x1f7d), (0x1ffc, 0x1ff3), (0x2126, 0x03c9), (0x212a, 0x006b), (0x212b, 0x00e5), (0x2132, 0x214e), (0x2160, 0x2170), 
    (0x2161, 0x2171), (0x2162, 0x2172), (0x2163, 0x2173), (0x2164, 0x2174), (0x2165, 0x2175), (0x2166, 0x2176), (0x2167, 0x2177), (0x2168, 0x2178), 
    (0x2169, 0x2179), (0x216a, 0x217a), (0x216b, 0x217b), (0x216c, 0x217c), (0x216d, 0x217d), (0x216e, 0x217e), (0x216f, 0x217f), (0x2183, 0x2184), 
    (0x24b6, 0x24d0), (0x24b7, 0x24d1), (0x24b8, 0x24d2), (0x24b9, 0x24d3), (0x24ba, 0x24d4), (0x24bb, 0x24d5), (0x24bc, 0x24d6), (0x24bd, 0x24d7), 
    (0x24be, 0x24d8), (0x24bf, 0x24d9), (0x24c0, 0x24da), (0x24c1, 0x24db), (0x24c2, 0x24dc), (0x24c3, 0x24dd), (0x24c4, 0x24de), (0x24c5, 0x24df), 
    (0x24c6, 0x24e0), (0x24c7, 0x24e1), (0x24c8, 0x24e2), (0x24c9, 0x24e3), (0x24ca, 0x24e4), (0x24cb, 0x24e5), (0x24cc, 0x24e6), (0x24cd, 0x24e7), 
    (0x24ce, 0x24e8), (0x24cf, 0x24e9), (0x2c00, 0x2c30), (0x2c01, 0x2c31), (0x2c02, 0x2c32), (0x2c03, 0x2c33), (0x2c04, 0x2c34), (0x2c05, 0x2c35), 
    (0x2c06, 0x2c36), (0x2c07, 0x2c37), (0x2c08, 0x2c38), (0x2c09, 0x2c39), (0x2c0a, 0x2c3a), (0x2c0b, 0x2c3b), (0x2c0c, 0x2c3c), (0x2c0d, 0x2c3d), 
    (0x2c0e, 0x2c3e), (0x2c0f, 0x2c3f), (0x2c10, 0x2c40), (0x2c11, 0x2c41), (0x2c12, 0x2c42), (0x2c13, 0x2c43), (0x2c14, 0x2c44), (0x2c15, 0x2c45), 
    (0x2c16, 0x2c46), (0x2c17, 0x2c47), (0x2c18, 0x2c48), (0x2c19, 0x2c49), (0x2c1a, 0x2c4a), (0x2c1b, 0x2c4b), (0x2c1c, 0x2c4c), (0x2c1d, 0x2c4d), 
    (0x2c1e, 0x2c4e), (0x2c1f, 0x2c4f), (0x2c20, 0x2c50), (0x2c21, 0x2c51), (0x2c22, 0x2c52), (0x2c23, 0x2c53), (0x2c24, 0x2c54), (0x2c25, 0x2c55), 
    (0x2c26, 0x2c56), (0x2c27, 0x2c57), (0x2c28, 0x2c58), (0x2c29, 0x2c59), (0x2c2a, 0x2c5a), (0x2c2b, 0x2c5b), (0x2c2c, 0x2c5c), (0x2c2d, 0x2c5d), 
    (0x2c2e, 0x2c5e), (0x2c2f, 0x2c5f), (0x2c60, 0x2c61), (0x2c62, 0x026b), (0x2c63, 0x1d7d), (0x2c64, 0x027d), (0x2c67, 0x2c68), (0x2c69, 0x2c6a), 
    (0x2c6b, 0x2c6c), (0x2c6d, 0x0251), (0x2c6e, 0x0271), (0x2c6f, 0x0250), (0x2c70, 0x0252), (0x2c72, 0x2c73), (0x2c75, 0x2c76), (0x2c7e, 0x023f), 
    (0x2c7f, 0x0240), (0x2c80, 0x2c81), (0x2c82, 0x2c83), (0x2c84, 0x2c85), (0x2c86, 0x2c87), (0x2c88, 0x2c89), (0x2c8a, 0x2c8b), (0x2c8c, 0x2c8d), 
    (0x2c8e, 0x2c8f), (0x2c90, 0x2c91), (0x2c92, 0x2c93), (0x2c94, 0x2c95), (0x2c96, 0x2c97), (0x2c98, 0x2c99), (0x2c9a, 0x2c9b), (0x2c9c, 0x2c9d), 
    (0x2c9e, 0x2c9f), (0x2ca0, 0x2ca1), (0x2ca2, 0x2ca3), (0x2ca4, 0x2ca5), (0x2ca6, 0x2ca7), (0x2ca8, 0x2ca9), (0x2caa, 0x2cab), (0x2cac, 0x2cad), 
    (0x2cae, 0x2caf), (0x2cb0, 0x2cb1), (0x2cb2, 0x2cb3), (0x2cb4, 0x2cb5), (0x2cb6, 0x2cb7), (0x2cb8, 0x2cb9), (0x2cba, 0x2cbb), (0x2cbc, 0x2cbd), 
    (0x2cbe, 0x2cbf), (0x2cc0, 0x2cc1), (0x2cc2, 0x2cc3), (0x2cc4, 0x2cc5), (0x2cc6, 0x2cc7), (0x2cc8, 0x2cc9), (0x2cca, 0x2ccb), (0x2ccc, 0x2ccd), 
    (0x2cce, 0x2ccf), (0x2cd0, 0x2cd1), (0x2cd2, 0x2cd3), (0x2cd4, 0x2cd5), (0x2cd6, 0x2cd7), (0x2cd8, 0x2cd9), (0x2cda, 0x2cdb), (0x2cdc, 0x2cdd), 
    (0x2cde, 0x2cdf), (0x2ce0, 0x2ce1), (0x2ce2, 0x2ce3), (0x2ceb, 0x2cec), (0x2ced, 0x2cee), (0x2cf2, 0x2cf3), (0xa640, 0xa641), (0xa642, 0xa643), 
    (0xa644, 0xa645), (0xa646, 0xa647), (0xa648, 0xa649), (0xa64a, 0xa64b), (0xa64c, 0xa64d), (0xa64e, 0xa64f), (0xa650, 0xa651), (0xa652, 0xa653), 
    (0xa654, 0xa655), (0xa656, 0xa657), (0xa658, 0xa659), (0xa65a, 0xa65b), (0xa65c, 0xa65d), (0xa65e, 0xa65f), (0xa660, 0xa661), (0xa662, 0xa663), 
    (0xa664, 0xa665), (0xa666, 0xa667), (0xa668, 0xa669), (0xa66a, 0xa66b), (0xa66c, 0xa66d), (0xa680, 0xa681), (0xa682, 0xa683), (0xa684, 0xa685), 
    (0xa686, 0xa687), (0xa688, 0xa689), (0xa68a, 0xa68b), (0xa68c, 0xa68d), (0xa68e, 0xa68f), (0xa690, 0xa691), (0xa692, 0xa693), (0xa694, 0xa695), 
    (0xa696, 0xa697), (0xa698, 0xa699), (0xa69a, 0xa69b), (0xa722, 0xa723), (0xa724, 0xa725), (0xa726, 0xa727), (0xa728, 0xa729), (0xa72a, 0xa72b), 
    (0xa72c, 0xa72d), (0xa72e, 0xa72f), (0xa732, 0xa733), (0xa734, 0xa735), (0xa736, 0xa737), (0xa738, 0xa739), (0xa73a, 0xa73b), (0xa73c, 0xa73d), 
    (0xa73e, 0xa73f), (0xa740, 0xa741), (0xa742, 0xa743), (0xa744, 0xa745), (0xa746, 0xa747), (0xa748, 0xa749), (0xa74a, 0xa74b), (0xa74c, 0xa74d), 
    (0xa74e, 0xa74f), (0xa750, 0xa751), (0xa752, 0xa753), (0xa754, 0xa755), (0xa756, 0xa757), (0xa758, 0xa759), (0xa75a, 0xa75b), (0xa75c, 0xa75d), 
    (0xa75e, 0xa75f), (0xa760, 0xa761), (0xa762, 0xa763), (0xa764, 0xa765), (0xa766, 0xa767), (0xa768, 0xa769), (0xa76a, 0xa76b), (0xa76c, 0xa76d), 
    (0xa76e, 0xa76f), (0xa779, 0xa77a), (0xa77b, 0xa77c), (0xa77d, 0x1d79), (0xa77e, 0xa77f), (0xa780, 0xa781), (0xa782, 0xa783), (0xa784, 0xa785), 
    (0xa786, 0xa787), (0xa78b, 0xa78c), (0xa78d, 0x0265), (0xa790, 0xa791), (0xa792, 0xa793), (0xa796, 0xa797), (0xa798, 0xa799), (0xa79a, 0xa79b), 
    (0xa79c, 0xa79d), (0xa79e, 0xa79f), (0xa7a0, 0xa7a1), (0xa7a2, 0xa7a3), (0xa7a4, 0xa7a5), (0xa7a6, 0xa7a7), (0xa7a8, 0xa7a9), (0xa7aa, 0x0266), 
    (0xa7ab, 0x025c), (0xa7ac, 0x0261), (0xa7ad, 0x026c), (0xa7ae, 0x026a), (0xa7b0, 0x029e), (0xa7b1, 0x0287), (0xa7b2, 0x029d), (0xa7b3, 0xab53), 
    (0xa7b4, 0xa7b5), (0xa7b6, 0xa7b7), (0xa7b8, 0xa7b9), (0xa7ba, 0xa7bb), (0xa7bc, 0xa7bd), (0xa7be, 0xa7bf), (0xa7c0, 0xa7c1), (0xa7c2, 0xa7c3), 
    (0xa7c4, 0xa794), (0xa7c5, 0x0282), (0xa7c6, 0x1d8e), (0xa7c7, 0xa7c8), (0xa7c9, 0xa7ca), (0xa7cb, 0x0264), (0xa7cc, 0xa7cd), (0xa7ce, 0xa7cf), 
    (0xa7d0, 0xa7d1), (0xa7d2, 0xa7d3), (0xa7d4, 0xa7d5), (0xa7d6, 0xa7d7), (0xa7d8, 0xa7d9), (0xa7da, 0xa7db), (0xa7dc, 0x019b), (0xa7f5, 0xa7f6), 
    (0xab70, 0x13a0), (0xab71, 0x13a1), (0xab72, 0x13a2), (0xab73, 0x13a3), (0xab74, 0x13a4), (0xab75, 0x13a5), (0xab76, 0x13a6), (0xab77, 0x13a7), 
    (0xab78, 0x13a8), (0xab79, 0x13a9), (0xab7a, 0x13aa), (0xab7b, 0x13ab), (0xab7c, 0x13ac), (0xab7d, 0x13ad), (0xab7e, 0x13ae), (0xab7f, 0x13af), 
    (0xab80, 0x13b0), (0xab81, 0x13b1), (0xab82, 0x13b2), (0xab83, 0x13b3), (0xab84, 0x13b4), (0xab85, 0x13b5), (0xab86, 0x13b6), (0xab87, 0x13b7), 
    (0xab88, 0x13b8), (0xab89, 0x13b9), (0xab8a, 0x13ba), (0xab8b, 0x13bb), (0xab8c, 0x13bc), (0xab8d, 0x13bd), (0xab8e, 0x13be), (0xab8f, 0x13bf), 
    (0xab90, 0x13c0), (0xab91, 0x13c1), (0xab92, 0x13c2), (0xab93, 0x13c3), (0xab94, 0x13c4), (0xab95, 0x13c5), (0xab96, 0x13c6), (0xab97, 0x13c7), 
    (0xab98, 0x13c8), (0xab99, 0x13c9), (0xab9a, 0x13ca), (0xab9b, 0x13cb), (0xab9c, 0x13cc), (0xab9d, 0x13cd), (0xab9e, 0x13ce), (0xab9f, 0x13cf), 
    (0xaba0, 0x13d0), (0xaba1, 0x13d1), (0xaba2, 0x13d2), (0xaba3, 0x13d3), (0xaba4, 0x13d4), (0xaba5, 0x13d5), (0xaba6, 0x13d6), (0xaba7, 0x13d7), 
    (0xaba8, 0x13d8), (0xaba9, 0x13d9), (0xabaa, 0x13da), (0xabab, 0x13db), (0xabac, 0x13dc), (0xabad, 0x13dd), (0xabae, 0x13de), (0xabaf, 0x13df), 
    (0xabb0, 0x13e0), (0xabb1, 0x13e1), (0xabb2, 0x13e2), (0xabb3, 0x13e3), (0xabb4, 0x13e4), (0xabb5, 0x13e5), (0xabb6, 0x13e6), (0xabb7, 0x13e7), 
    (0xabb8, 0x13e8), (0xabb9, 0x13e9), (0xabba, 0x13ea), (0xabbb, 0x13eb), (0xabbc, 0x13ec), (0xabbd, 0x13ed), (0xabbe, 0x13ee), (0xabbf, 0x13ef), 
    (0xfb05, 0xfb06), (0xff21, 0xff41), (0xff22, 0xff42), (0xff23, 0xff43), (0xff24, 0xff44), (0xff25, 0xff45), (0xff26, 0xff46), (0xff27, 0xff47), 
    (0xff28, 0xff48), (0xff29, 0xff49), (0xff2a, 0xff4a), (0xff2b, 0xff4b), (0xff2c, 0xff4c), (0xff2d, 0xff4d), (0xff2e, 0xff4e), (0xff2f, 0xff4f), 
    (0xff30, 0xff50), (0xff31, 0xff51), (0xff32, 0xff52), (0xff33, 0xff53), (0xff34, 0xff54), (0xff35, 0xff55), (0xff36, 0xff56), (0xff37, 0xff57), 
    (0xff38, 0xff58), (0xff39, 0xff59), (0xff3a, 0xff5a), 
];

const ASTRAL_CASE_FOLD: &[(u32, u32)] = &[
    (0x10400, 0x10428), (0x10401, 0x10429), (0x10402, 0x1042a), (0x10403, 0x1042b), (0x10404, 0x1042c), (0x10405, 0x1042d), 
    (0x10406, 0x1042e), (0x10407, 0x1042f), (0x10408, 0x10430), (0x10409, 0x10431), (0x1040a, 0x10432), (0x1040b, 0x10433), 
    (0x1040c, 0x10434), (0x1040d, 0x10435), (0x1040e, 0x10436), (0x1040f, 0x10437), (0x10410, 0x10438), (0x10411, 0x10439), 
    (0x10412, 0x1043a), (0x10413, 0x1043b), (0x10414, 0x1043c), (0x10415, 0x1043d), (0x10416, 0x1043e), (0x10417, 0x1043f), 
    (0x10418, 0x10440), (0x10419, 0x10441), (0x1041a, 0x10442), (0x1041b, 0x10443), (0x1041c, 0x10444), (0x1041d, 0x10445), 
    (0x1041e, 0x10446), (0x1041f, 0x10447), (0x10420, 0x10448), (0x10421, 0x10449), (0x10422, 0x1044a), (0x10423, 0x1044b), 
    (0x10424, 0x1044c), (0x10425, 0x1044d), (0x10426, 0x1044e), (0x10427, 0x1044f), (0x104b0, 0x104d8), (0x104b1, 0x104d9), 
    (0x104b2, 0x104da), (0x104b3, 0x104db), (0x104b4, 0x104dc), (0x104b5, 0x104dd), (0x104b6, 0x104de), (0x104b7, 0x104df), 
    (0x104b8, 0x104e0), (0x104b9, 0x104e1), (0x104ba, 0x104e2), (0x104bb, 0x104e3), (0x104bc, 0x104e4), (0x104bd, 0x104e5), 
    (0x104be, 0x104e6), (0x104bf, 0x104e7), (0x104c0, 0x104e8), (0x104c1, 0x104e9), (0x104c2, 0x104ea), (0x104c3, 0x104eb), 
    (0x104c4, 0x104ec), (0x104c5, 0x104ed), (0x104c6, 0x104ee), (0x104c7, 0x104ef), (0x104c8, 0x104f0), (0x104c9, 0x104f1), 
    (0x104ca, 0x104f2), (0x104cb, 0x104f3), (0x104cc, 0x104f4), (0x104cd, 0x104f5), (0x104ce, 0x104f6), (0x104cf, 0x104f7), 
    (0x104d0, 0x104f8), (0x104d1, 0x104f9), (0x104d2, 0x104fa), (0x104d3, 0x104fb), (0x10570, 0x10597), (0x10571, 0x10598), 
    (0x10572, 0x10599), (0x10573, 0x1059a), (0x10574, 0x1059b), (0x10575, 0x1059c), (0x10576, 0x1059d), (0x10577, 0x1059e), 
    (0x10578, 0x1059f), (0x10579, 0x105a0), (0x1057a, 0x105a1), (0x1057c, 0x105a3), (0x1057d, 0x105a4), (0x1057e, 0x105a5), 
    (0x1057f, 0x105a6), (0x10580, 0x105a7), (0x10581, 0x105a8), (0x10582, 0x105a9), (0x10583, 0x105aa), (0x10584, 0x105ab), 
    (0x10585, 0x105ac), (0x10586, 0x105ad), (0x10587, 0x105ae), (0x10588, 0x105af), (0x10589, 0x105b0), (0x1058a, 0x105b1), 
    (0x1058c, 0x105b3), (0x1058d, 0x105b4), (0x1058e, 0x105b5), (0x1058f, 0x105b6), (0x10590, 0x105b7), (0x10591, 0x105b8), 
    (0x10592, 0x105b9), (0x10594, 0x105bb), (0x10595, 0x105bc), (0x10c80, 0x10cc0), (0x10c81, 0x10cc1), (0x10c82, 0x10cc2), 
    (0x10c83, 0x10cc3), (0x10c84, 0x10cc4), (0x10c85, 0x10cc5), (0x10c86, 0x10cc6), (0x10c87, 0x10cc7), (0x10c88, 0x10cc8), 
    (0x10c89, 0x10cc9), (0x10c8a, 0x10cca), (0x10c8b, 0x10ccb), (0x10c8c, 0x10ccc), (0x10c8d, 0x10ccd), (0x10c8e, 0x10cce), 
    (0x10c8f, 0x10ccf), (0x10c90, 0x10cd0), (0x10c91, 0x10cd1), (0x10c92, 0x10cd2), (0x10c93, 0x10cd3), (0x10c94, 0x10cd4), 
    (0x10c95, 0x10cd5), (0x10c96, 0x10cd6), (0x10c97, 0x10cd7), (0x10c98, 0x10cd8), (0x10c99, 0x10cd9), (0x10c9a, 0x10cda), 
    (0x10c9b, 0x10cdb), (0x10c9c, 0x10cdc), (0x10c9d, 0x10cdd), (0x10c9e, 0x10cde), (0x10c9f, 0x10cdf), (0x10ca0, 0x10ce0), 
    (0x10ca1, 0x10ce1), (0x10ca2, 0x10ce2), (0x10ca3, 0x10ce3), (0x10ca4, 0x10ce4), (0x10ca5, 0x10ce5), (0x10ca6, 0x10ce6), 
    (0x10ca7, 0x10ce7), (0x10ca8, 0x10ce8), (0x10ca9, 0x10ce9), (0x10caa, 0x10cea), (0x10cab, 0x10ceb), (0x10cac, 0x10cec), 
    (0x10cad, 0x10ced), (0x10cae, 0x10cee), (0x10caf, 0x10cef), (0x10cb0, 0x10cf0), (0x10cb1, 0x10cf1), (0x10cb2, 0x10cf2), 
    (0x10d50, 0x10d70), (0x10d51, 0x10d71), (0x10d52, 0x10d72), (0x10d53, 0x10d73), (0x10d54, 0x10d74), (0x10d55, 0x10d75), 
    (0x10d56, 0x10d76), (0x10d57, 0x10d77), (0x10d58, 0x10d78), (0x10d59, 0x10d79), (0x10d5a, 0x10d7a), (0x10d5b, 0x10d7b), 
    (0x10d5c, 0x10d7c), (0x10d5d, 0x10d7d), (0x10d5e, 0x10d7e), (0x10d5f, 0x10d7f), (0x10d60, 0x10d80), (0x10d61, 0x10d81), 
    (0x10d62, 0x10d82), (0x10d63, 0x10d83), (0x10d64, 0x10d84), (0x10d65, 0x10d85), (0x118a0, 0x118c0), (0x118a1, 0x118c1), 
    (0x118a2, 0x118c2), (0x118a3, 0x118c3), (0x118a4, 0x118c4), (0x118a5, 0x118c5), (0x118a6, 0x118c6), (0x118a7, 0x118c7), 
    (0x118a8, 0x118c8), (0x118a9, 0x118c9), (0x118aa, 0x118ca), (0x118ab, 0x118cb), (0x118ac, 0x118cc), (0x118ad, 0x118cd), 
    (0x118ae, 0x118ce), (0x118af, 0x118cf), (0x118b0, 0x118d0), (0x118b1, 0x118d1), (0x118b2, 0x118d2), (0x118b3, 0x118d3), 
    (0x118b4, 0x118d4), (0x118b5, 0x118d5), (0x118b6, 0x118d6), (0x118b7, 0x118d7), (0x118b8, 0x118d8), (0x118b9, 0x118d9), 
    (0x118ba, 0x118da), (0x118bb, 0x118db), (0x118bc, 0x118dc), (0x118bd, 0x118dd), (0x118be, 0x118de), (0x118bf, 0x118df), 
    (0x16e40, 0x16e60), (0x16e41, 0x16e61), (0x16e42, 0x16e62), (0x16e43, 0x16e63), (0x16e44, 0x16e64), (0x16e45, 0x16e65), 
    (0x16e46, 0x16e66), (0x16e47, 0x16e67), (0x16e48, 0x16e68), (0x16e49, 0x16e69), (0x16e4a, 0x16e6a), (0x16e4b, 0x16e6b), 
    (0x16e4c, 0x16e6c), (0x16e4d, 0x16e6d), (0x16e4e, 0x16e6e), (0x16e4f, 0x16e6f), (0x16e50, 0x16e70), (0x16e51, 0x16e71), 
    (0x16e52, 0x16e72), (0x16e53, 0x16e73), (0x16e54, 0x16e74), (0x16e55, 0x16e75), (0x16e56, 0x16e76), (0x16e57, 0x16e77), 
    (0x16e58, 0x16e78), (0x16e59, 0x16e79), (0x16e5a, 0x16e7a), (0x16e5b, 0x16e7b), (0x16e5c, 0x16e7c), (0x16e5d, 0x16e7d), 
    (0x16e5e, 0x16e7e), (0x16e5f, 0x16e7f), (0x16ea0, 0x16ebb), (0x16ea1, 0x16ebc), (0x16ea2, 0x16ebd), (0x16ea3, 0x16ebe), 
    (0x16ea4, 0x16ebf), (0x16ea5, 0x16ec0), (0x16ea6, 0x16ec1), (0x16ea7, 0x16ec2), (0x16ea8, 0x16ec3), (0x16ea9, 0x16ec4), 
    (0x16eaa, 0x16ec5), (0x16eab, 0x16ec6), (0x16eac, 0x16ec7), (0x16ead, 0x16ec8), (0x16eae, 0x16ec9), (0x16eaf, 0x16eca), 
    (0x16eb0, 0x16ecb), (0x16eb1, 0x16ecc), (0x16eb2, 0x16ecd), (0x16eb3, 0x16ece), (0x16eb4, 0x16ecf), (0x16eb5, 0x16ed0), 
    (0x16eb6, 0x16ed1), (0x16eb7, 0x16ed2), (0x16eb8, 0x16ed3), (0x1e900, 0x1e922), (0x1e901, 0x1e923), (0x1e902, 0x1e924), 
    (0x1e903, 0x1e925), (0x1e904, 0x1e926), (0x1e905, 0x1e927), (0x1e906, 0x1e928), (0x1e907, 0x1e929), (0x1e908, 0x1e92a), 
    (0x1e909, 0x1e92b), (0x1e90a, 0x1e92c), (0x1e90b, 0x1e92d), (0x1e90c, 0x1e92e), (0x1e90d, 0x1e92f), (0x1e90e, 0x1e930), 
    (0x1e90f, 0x1e931), (0x1e910, 0x1e932), (0x1e911, 0x1e933), (0x1e912, 0x1e934), (0x1e913, 0x1e935), (0x1e914, 0x1e936), 
    (0x1e915, 0x1e937), (0x1e916, 0x1e938), (0x1e917, 0x1e939), (0x1e918, 0x1e93a), (0x1e919, 0x1e93b), (0x1e91a, 0x1e93c), 
    (0x1e91b, 0x1e93d), (0x1e91c, 0x1e93e), (0x1e91d, 0x1e93f), (0x1e91e, 0x1e940), (0x1e91f, 0x1e941), (0x1e920, 0x1e942), 
    (0x1e921, 0x1e943), 
];

/// Matches PCRE2's `\s` without `UseUnicodePropertiesOption` in Qt 6.
///
/// In PCRE2 when `PCRE2_UCP` is NOT enabled, `\s` matches ASCII whitespace:
/// ' ' (0x20), '\t' (0x09), '\n' (0x0A), '\v' (0x0B), '\f' (0x0C), '\r' (0x0D).
#[inline]
pub fn is_ascii_whitespace_unit(c: u16) -> bool {
    matches!(c, 0x0020 | 0x0009..=0x000D)
}

/// Matches Qt 6 `QChar::toCaseFolded(char16_t)` for BMP code units.
#[inline]
pub fn to_case_folded_bmp(c: u16) -> u16 {
    if c <= 0x7F {
        if (c as u8).is_ascii_uppercase() {
            c + 32
        } else {
            c
        }
    } else {
        match BMP_CASE_FOLD.binary_search_by_key(&c, |&(cp, _)| cp) {
            Ok(idx) => BMP_CASE_FOLD[idx].1,
            Err(_) => c,
        }
    }
}

/// Matches Qt 6 `QChar::toCaseFolded(char32_t)` for all Unicode code points.
#[inline]
pub fn to_case_folded_u32(cp: u32) -> u32 {
    if cp <= 0xFFFF {
        to_case_folded_bmp(cp as u16) as u32
    } else {
        match ASTRAL_CASE_FOLD.binary_search_by_key(&cp, |&(c, _)| c) {
            Ok(idx) => ASTRAL_CASE_FOLD[idx].1,
            Err(_) => cp,
        }
    }
}

/// Folds UTF-16 units into `out`, removing ASCII whitespace if `ignore_whitespace` is true,
/// and applying Unicode simple case folding matching Qt 6 `QChar::toCaseFolded`.
pub fn fold_utf16_into(units: &[u16], ignore_whitespace: bool, out: &mut Vec<u16>) {
    let mut i = 0;
    while i < units.len() {
        let u1 = units[i];
        if ignore_whitespace && is_ascii_whitespace_unit(u1) {
            i += 1;
            continue;
        }
        if (0xD800..=0xDBFF).contains(&u1)
            && i + 1 < units.len()
            && (0xDC00..=0xDFFF).contains(&units[i + 1])
        {
            let u2 = units[i + 1];
            let cp = 0x10000 + (((u1 - 0xD800) as u32) << 10) + ((u2 - 0xDC00) as u32);
            let folded = to_case_folded_u32(cp);
            if folded > 0xFFFF {
                let high = 0xD800 + (((folded - 0x10000) >> 10) & 0x3FF) as u16;
                let low = 0xDC00 + ((folded - 0x10000) & 0x3FF) as u16;
                out.push(high);
                out.push(low);
            } else {
                out.push(folded as u16);
            }
            i += 2;
        } else {
            out.push(to_case_folded_bmp(u1));
            i += 1;
        }
    }
}

/// Helper returning a freshly allocated folded UTF-16 `Vec<u16>`.
pub fn fold_utf16_units(units: &[u16], ignore_whitespace: bool) -> Vec<u16> {
    let mut out = Vec::with_capacity(units.len());
    fold_utf16_into(units, ignore_whitespace, &mut out);
    out
}

/// Checks whether `haystack` contains `needle` as a contiguous subslice.
#[inline]
pub fn contains_subslice(haystack: &[u16], needle: &[u16]) -> bool {
    if needle.is_empty() {
        return true;
    }
    if haystack.len() < needle.len() {
        return false;
    }
    haystack.windows(needle.len()).any(|w| w == needle)
}

/// Finds all row indices in `candidates` that match `query`.
///
/// Matches Qt 6 `SubtitleListModel::find(str, ignoreWhitespace)` behavior:
/// - If `ignore_whitespace` is true, ASCII whitespace characters (`\s*`) are removed from
///   both the query and candidate texts.
/// - If the query is empty (or becomes empty after whitespace removal), an empty list is returned.
/// - Candidates are matched case-insensitively using Unicode simple case folding.
pub fn find_matching_rows<T: AsRef<[u16]>>(
    candidates: &[T],
    query: &[u16],
    ignore_whitespace: bool,
) -> Vec<usize> {
    let normalized_query = fold_utf16_units(query, ignore_whitespace);
    if normalized_query.is_empty() {
        return Vec::new();
    }

    let mut cand_buf = Vec::new();
    candidates
        .iter()
        .enumerate()
        .filter_map(|(idx, candidate)| {
            cand_buf.clear();
            fold_utf16_into(candidate.as_ref(), ignore_whitespace, &mut cand_buf);
            if contains_subslice(&cand_buf, &normalized_query) {
                Some(idx)
            } else {
                None
            }
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
    }

    #[test]
    fn test_case_fold_tables_strictly_sorted() {
        for window in BMP_CASE_FOLD.windows(2) {
            assert!(
                window[0].0 < window[1].0,
                "BMP_CASE_FOLD must be strictly sorted: {:x} >= {:x}",
                window[0].0,
                window[1].0
            );
        }

        for window in ASTRAL_CASE_FOLD.windows(2) {
            assert!(
                window[0].0 < window[1].0,
                "ASTRAL_CASE_FOLD must be strictly sorted: {:x} >= {:x}",
                window[0].0,
                window[1].0
            );
        }
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

    #[test]
    fn test_contains_subslice() {
        let h = to_utf16("hello world");
        assert!(contains_subslice(&h, &to_utf16("")));
        assert!(contains_subslice(&h, &to_utf16("hello")));
        assert!(contains_subslice(&h, &to_utf16("world")));
        assert!(contains_subslice(&h, &to_utf16("o w")));
        assert!(!contains_subslice(&h, &to_utf16("not present")));
        assert!(!contains_subslice(&h, &to_utf16("hello world plus extra")));
    }
}

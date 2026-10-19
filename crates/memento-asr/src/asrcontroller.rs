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

//! Sorted subtitle cue book and work-window/seek decisions from
//! `asr/asrcontroller.cpp`. QObject state, subtitle text and mpv calls remain
//! with the C++ controller.

const WORK_WINDOW_SECONDS: f64 = 30.0;
pub const MIN_WORK_WINDOW_SECONDS: f64 = 0.25;
const SEEK_RESTART_THRESHOLD_SECONDS: f64 = 5.0;
const SEEK_RESTART_CLOCK_SLOP_SECONDS: f64 = 2.0;
pub const SUBTITLE_TIME_DELTA: f64 = 0.0001;
pub const MAX_TRANSCRIPTION_FAILURES: i32 = 3;

pub const WINDOW_DONE: i32 = 0;
pub const WINDOW_ADVANCE: i32 = 1;
pub const WINDOW_EXTRACT: i32 = 2;

#[repr(i32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PositionUpdateAction {
    None = 0,
    RequestRestartOnly = 1,
    RequestRestartAndSelect = 2,
    SelectOnly = 3,
}

#[repr(i32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FailureAction {
    RetryFromCurrentPosition = 0,
    PauseAfterMaxFailures = 1,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FailureDecision {
    pub consecutive_failures: i32,
    pub action: FailureAction,
    pub next_start: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AdjustedSegment {
    pub start: f64,
    pub end: f64,
}

#[derive(Clone, Copy)]
struct Cue {
    id: i64,
    start: f64,
    end: f64,
}

/// Opaque to C++; its cue ordering matches `m_subtitleCues`' lower_bound
/// insertion, including inserting *before* existing cues at the same start.
#[derive(Default)]
pub struct AsrControllerCore {
    cues: Vec<Cue>,
}

/// The first branch taken by the C++ work loop. `end` is only used for
/// `WINDOW_EXTRACT`; for Done/Advance it equals the returned start.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AsrWindowDecision {
    pub start: f64,
    pub end: f64,
    pub action: i32,
    pub rewound: bool,
}

impl AsrWindowDecision {
    pub(crate) fn done(start: f64, rewound: bool) -> Self {
        Self { start, end: start, action: WINDOW_DONE, rewound }
    }

    fn advance(start: f64, rewound: bool) -> Self {
        Self { start, end: start, action: WINDOW_ADVANCE, rewound }
    }

    fn extract(start: f64, end: f64, rewound: bool) -> Self {
        Self { start, end, action: WINDOW_EXTRACT, rewound }
    }
}

// qMin(a, b) = (a < b) ? a : b; qMax(a, b) = (a < b) ? b : a.
// Do not replace these with f64::min/max/clamp: Qt's comparison order differs
// for NaN, and clamp panics for reversed bounds. qBound(0, val, max) evaluates
// qMax(0, qMin(max, val)).
#[inline]
pub(crate) fn qt_min(a: f64, b: f64) -> f64 {
    if a < b { a } else { b }
}

#[inline]
pub(crate) fn qt_bound(min: f64, value: f64, max: f64) -> f64 {
    let capped = qt_min(max, value);
    if min < capped { capped } else { min }
}

#[inline]
pub(crate) fn qt_bound_zero(value: f64, max: f64) -> f64 {
    qt_bound(0.0, value, max)
}

impl AsrControllerCore {
    pub(crate) fn len(&self) -> usize {
        self.cues.len()
    }

    pub(crate) fn clear(&mut self) {
        self.cues.clear();
    }

    /// Return the first overlapping cue in iteration order, using both of
    /// the strict epsilon comparisons in `AsrController::addSubtitle`.
    pub(crate) fn first_overlap(&self, start: f64, end: f64) -> Option<i64> {
        self.cues.iter().find(|cue| {
            cue.start < end - SUBTITLE_TIME_DELTA &&
                start + SUBTITLE_TIME_DELTA < cue.end
        }).map(|cue| cue.id)
    }

    /// Erase the first cue with this id; mpv owns and assigns cue ids.
    pub(crate) fn erase(&mut self, id: i64) -> bool {
        if let Some(index) = self.cues.iter().position(|cue| cue.id == id) {
            self.cues.remove(index);
            true
        } else {
            false
        }
    }

    pub(crate) fn insert(&mut self, id: i64, start: f64, end: f64) {
        let insertion = self.cues.partition_point(|cue| cue.start < start);
        self.cues.insert(insertion, Cue { id, start, end });
    }

    /// Restart at the first cue after each advance, matching the C++ scan
    /// exactly (including its tie ordering and epsilon comparisons).
    pub(crate) fn skip_covered_position(&self, mut position: f64) -> f64 {
        loop {
            let Some(cue) = self.cues.iter().find(|cue| {
                cue.start <= position + SUBTITLE_TIME_DELTA && position < cue.end
            }) else {
                break;
            };
            position = cue.end;
        }
        position
    }

    pub(crate) fn next_subtitle_start(&self, position: f64) -> f64 {
        let mut next = f64::INFINITY;
        for cue in &self.cues {
            if cue.start > position + SUBTITLE_TIME_DELTA && cue.start < next {
                next = cue.start;
            }
        }
        next
    }

    /// Mirrors the branches of `selectImplAsync`'s work loop: lines 713-735
    /// in the original controller. Advance represents either `continue`
    /// branch (including a single rewind), not an extraction.
    pub(crate) fn plan_window(&self, start: f64, duration: f64, rewound: bool) -> AsrWindowDecision {
        let start = qt_bound_zero(self.skip_covered_position(start), duration);
        if start >= duration - MIN_WORK_WINDOW_SECONDS {
            if !rewound &&
                self.skip_covered_position(0.0) < duration - MIN_WORK_WINDOW_SECONDS {
                return AsrWindowDecision::advance(self.skip_covered_position(0.0), true);
            }
            return AsrWindowDecision::done(start, rewound);
        }

        let next_start = self.next_subtitle_start(start);
        let mut end = qt_min(duration, start + WORK_WINDOW_SECONDS);
        if next_start.is_finite() {
            end = qt_min(end, next_start);
        }
        if end <= start + MIN_WORK_WINDOW_SECONDS {
            return AsrWindowDecision::advance(
                qt_min(duration, start + MIN_WORK_WINDOW_SECONDS), rewound
            );
        }
        AsrWindowDecision::extract(start, end, rewound)
    }

    pub(crate) fn initial_start(
        &self,
        start_from_beginning: bool,
        has_consumed_restart: bool,
        restart_position: f64,
        time_position: f64,
        duration: f64,
    ) -> f64 {
        plan_initial_start(
            Some(self),
            start_from_beginning,
            has_consumed_restart,
            restart_position,
            time_position,
            duration,
        )
    }

    pub(crate) fn plan_position_update(
        &self,
        position: f64,
        duration: f64,
        seeked: bool,
        start_from_beginning: bool,
        running: bool,
        active: bool,
        has_controller: bool,
        retry_paused: bool,
    ) -> PositionUpdateAction {
        plan_position_update(
            Some(self),
            position,
            duration,
            seeked,
            start_from_beginning,
            running,
            active,
            has_controller,
            retry_paused,
        )
    }
}

/// Seek classification in `handlePositionChanged`: the last-position update
/// and restart/QObject work remain in C++.
pub(crate) fn seeked(position: f64, last: f64, elapsed: f64, paused: bool) -> bool {
    if last < 0.0 {
        return false;
    }
    let delta = position - last;
    delta.abs() > SEEK_RESTART_THRESHOLD_SECONDS &&
        (delta < 0.0 || paused || delta > elapsed + SEEK_RESTART_CLOCK_SLOP_SECONDS)
}

pub fn adjust_segment(
    seg_start: f64,
    seg_end: f64,
    win_start: f64,
    win_end: f64,
) -> AdjustedSegment {
    AdjustedSegment {
        start: qt_bound(win_start, seg_start + win_start, win_end),
        end: qt_bound(win_start, seg_end + win_start, win_end),
    }
}

pub fn is_cue_valid(text_non_empty: bool, start: f64, end: f64) -> bool {
    text_non_empty
        && start.is_finite()
        && end.is_finite()
        && end > start + SUBTITLE_TIME_DELTA
}

pub fn on_failure(
    consecutive_failures: i32,
    time_position: f64,
    duration: f64,
) -> FailureDecision {
    let new_failures = consecutive_failures.saturating_add(1);
    let action = if new_failures >= MAX_TRANSCRIPTION_FAILURES {
        FailureAction::PauseAfterMaxFailures
    } else {
        FailureAction::RetryFromCurrentPosition
    };
    let next_start = qt_bound_zero(time_position, duration);
    FailureDecision {
        consecutive_failures: new_failures,
        action,
        next_start,
    }
}

pub fn plan_initial_start(
    core: Option<&AsrControllerCore>,
    start_from_beginning: bool,
    has_consumed_restart: bool,
    restart_position: f64,
    time_position: f64,
    duration: f64,
) -> f64 {
    let mut start = match (start_from_beginning, has_consumed_restart) {
        (true, _) => 0.0,
        (false, true) => restart_position,
        (false, false) => qt_bound_zero(time_position, duration),
    };
    if start >= duration - MIN_WORK_WINDOW_SECONDS {
        start = 0.0;
    }
    core.map_or(start, |c| c.skip_covered_position(start))
}

pub fn plan_position_update(
    core: Option<&AsrControllerCore>,
    position: f64,
    duration: f64,
    seeked: bool,
    start_from_beginning: bool,
    running: bool,
    active: bool,
    has_controller: bool,
    retry_paused: bool,
) -> PositionUpdateAction {
    let can_select = !running && active && has_controller;
    match (seeked, start_from_beginning) {
        (true, false) => {
            if can_select {
                PositionUpdateAction::RequestRestartAndSelect
            } else {
                PositionUpdateAction::RequestRestartOnly
            }
        }
        (true, true) => {
            let skip_zero = core.map_or(0.0, |c| c.skip_covered_position(0.0));
            if can_select && skip_zero < duration - MIN_WORK_WINDOW_SECONDS {
                PositionUpdateAction::SelectOnly
            } else {
                PositionUpdateAction::None
            }
        }
        (false, _) => {
            if running || retry_paused || position >= duration - MIN_WORK_WINDOW_SECONDS {
                PositionUpdateAction::None
            } else {
                let uncovered = core.map_or(position, |c| c.skip_covered_position(position));
                if uncovered <= position + SUBTITLE_TIME_DELTA {
                    PositionUpdateAction::SelectOnly
                } else {
                    PositionUpdateAction::None
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn equal_start_inserts_before_old_cue_and_erases_by_id() {
        let mut core = AsrControllerCore::default();
        core.insert(11, 1.0, 4.0);
        core.insert(22, 1.0, 5.0);
        assert_eq!(core.len(), 2);
        assert_eq!(core.first_overlap(1.5, 2.0), Some(22));
        assert!(core.erase(22));
        assert_eq!(core.first_overlap(1.5, 2.0), Some(11));
        assert!(!core.erase(22));
        core.clear();
        assert_eq!(core.len(), 0);
    }

    #[test]
    fn overlap_epsilon_is_strict_at_both_edges() {
        let mut core = AsrControllerCore::default();
        core.insert(7, 1.0, 2.0);
        assert_eq!(core.first_overlap(0.0, 1.0 + SUBTITLE_TIME_DELTA), None);
        assert_eq!(core.first_overlap(2.0 - SUBTITLE_TIME_DELTA, 3.0), None);
        assert_eq!(core.first_overlap(0.0, 1.0002), Some(7));
        assert_eq!(core.first_overlap(1.9998, 3.0), Some(7));
    }

    #[test]
    fn coverage_restarts_scan_across_epsilon_bridged_cues() {
        let mut core = AsrControllerCore::default();
        core.insert(4, 1.0, 4.0);
        core.insert(3, 0.5, 2.0);
        core.insert(1, 0.0, 1.0);
        core.insert(5, 4.00005, 5.0);
        core.insert(6, 5.0002, 6.0);
        assert_eq!(core.skip_covered_position(0.0), 5.0);
        assert_eq!(core.skip_covered_position(5.0), 5.0);
        assert_eq!(core.next_subtitle_start(0.5), 1.0);
        assert_eq!(core.next_subtitle_start(5.0002), f64::INFINITY);
    }

    #[test]
    fn window_clips_to_next_cue_and_skips_short_gap() {
        let mut core = AsrControllerCore::default();
        assert_eq!(core.plan_window(5.0, 80.0, false),
            AsrWindowDecision::extract(5.0, 35.0, false));
        core.insert(1, 0.2, 1.0);
        assert_eq!(core.plan_window(0.0, 80.0, false),
            AsrWindowDecision::advance(0.25, false));
        core.insert(2, 10.0, 20.0);
        assert_eq!(core.plan_window(2.0, 80.0, false),
            AsrWindowDecision::extract(2.0, 10.0, false));
        assert_eq!(core.plan_window(75.0, 80.0, false),
            AsrWindowDecision::extract(75.0, 80.0, false));
    }

    #[test]
    fn window_rewinds_at_end_only_once_and_stops_at_duration() {
        let mut core = AsrControllerCore::default();
        assert_eq!(core.plan_window(59.8, 60.0, false),
            AsrWindowDecision::advance(0.0, true));
        assert_eq!(core.plan_window(0.0, 60.0, true),
            AsrWindowDecision::extract(0.0, 30.0, true));
        assert_eq!(core.plan_window(59.8, 60.0, true),
            AsrWindowDecision::done(59.8, true));
        core.insert(1, 0.0, 60.0);
        assert_eq!(core.plan_window(59.8, 60.0, false),
            AsrWindowDecision::done(60.0, false));
        assert_eq!(core.plan_window(0.0, 0.25, false),
            AsrWindowDecision::done(0.25, false));
    }

    #[test]
    fn qt_bound_nan_order_and_reversed_bounds_do_not_panic() {
        assert_eq!(qt_bound_zero(f64::NAN, 10.0), 0.0);
        assert_eq!(qt_bound_zero(2.0, f64::NAN), 2.0);
        assert_eq!(qt_bound_zero(2.0, -1.0), 0.0);
        assert_eq!(qt_bound(5.0, 2.0, 10.0), 5.0);
        assert_eq!(qt_bound(5.0, 7.0, 10.0), 7.0);
        assert_eq!(qt_bound(5.0, 12.0, 10.0), 10.0);
    }

    #[test]
    fn seek_threshold_requires_discontinuity_beyond_clock_slop() {
        assert!(!seeked(10.0, -1.0, 0.0, true));
        assert!(!seeked(15.0, 10.0, 0.0, true));
        assert!(seeked(4.0, 10.0, 50.0, false));
        assert!(!seeked(16.0, 10.0, 4.0, false));
        assert!(seeked(16.0, 10.0, 4.0, true));
        assert!(seeked(16.01, 10.0, 4.0, false));
        assert!(!seeked(10.0, f64::NAN, 0.0, false));
    }

    #[test]
    fn initial_start_respects_settings_restarts_and_coverage() {
        let mut core = AsrControllerCore::default();
        core.insert(1, 0.0, 2.5);

        // start_from_beginning = true: ignores restart_position, starts at 0.0 and skips covered range to 2.5
        assert_eq!(core.initial_start(true, true, 10.0, 5.0, 60.0), 2.5);

        // has_consumed_restart = true: uses restart_position (10.0)
        assert_eq!(core.initial_start(false, true, 10.0, 5.0, 60.0), 10.0);

        // neither: uses time_position (5.0)
        assert_eq!(core.initial_start(false, false, 10.0, 5.0, 60.0), 5.0);

        // start >= duration - 0.25 wraps back to 0.0, then skips coverage to 2.5
        assert_eq!(core.initial_start(false, false, 0.0, 59.8, 60.0), 2.5);

        // Without core (None):
        assert_eq!(plan_initial_start(None, true, true, 10.0, 5.0, 60.0), 0.0);
        assert_eq!(plan_initial_start(None, false, true, 10.0, 5.0, 60.0), 10.0);
    }

    #[test]
    fn adjust_segment_clamps_to_window() {
        let seg = adjust_segment(1.0, 3.0, 10.0, 20.0);
        assert_eq!(seg, AdjustedSegment { start: 11.0, end: 13.0 });

        let seg_overflow = adjust_segment(-5.0, 15.0, 10.0, 20.0);
        assert_eq!(seg_overflow, AdjustedSegment { start: 10.0, end: 20.0 });
    }

    #[test]
    fn cue_validation_checks_non_empty_finite_and_delta() {
        assert!(!is_cue_valid(false, 1.0, 2.0));
        assert!(!is_cue_valid(true, f64::NAN, 2.0));
        assert!(!is_cue_valid(true, 1.0, f64::INFINITY));
        assert!(!is_cue_valid(true, 2.0, 1.0));
        assert!(!is_cue_valid(true, 1.0, 1.0 + SUBTITLE_TIME_DELTA));
        assert!(is_cue_valid(true, 1.0, 1.0 + SUBTITLE_TIME_DELTA + 1e-6));
    }

    #[test]
    fn failure_counter_and_pause_threshold() {
        let f1 = on_failure(0, 10.0, 100.0);
        assert_eq!(f1.consecutive_failures, 1);
        assert_eq!(f1.action, FailureAction::RetryFromCurrentPosition);
        assert_eq!(f1.next_start, 10.0);

        let f2 = on_failure(1, 10.0, 100.0);
        assert_eq!(f2.consecutive_failures, 2);
        assert_eq!(f2.action, FailureAction::RetryFromCurrentPosition);

        let f3 = on_failure(2, 10.0, 100.0);
        assert_eq!(f3.consecutive_failures, 3);
        assert_eq!(f3.action, FailureAction::PauseAfterMaxFailures);

        let f4 = on_failure(3, 10.0, 100.0);
        assert_eq!(f4.consecutive_failures, 4);
        assert_eq!(f4.action, FailureAction::PauseAfterMaxFailures);
    }

    #[test]
    fn position_update_planner_branches() {
        let mut core = AsrControllerCore::default();
        core.insert(1, 10.0, 20.0);

        // Seeked, not from beginning:
        // can_select = true -> RequestRestartAndSelect
        assert_eq!(
            core.plan_position_update(5.0, 60.0, true, false, false, true, true, false),
            PositionUpdateAction::RequestRestartAndSelect
        );
        // can_select = false (running=true) -> RequestRestartOnly
        assert_eq!(
            core.plan_position_update(5.0, 60.0, true, false, true, true, true, false),
            PositionUpdateAction::RequestRestartOnly
        );

        // Seeked, from beginning:
        // can_select = true, skip_zero (0.0) < duration - 0.25 -> SelectOnly
        assert_eq!(
            core.plan_position_update(5.0, 60.0, true, true, false, true, true, false),
            PositionUpdateAction::SelectOnly
        );
        // If covered to end:
        core.insert(2, 0.0, 60.0);
        assert_eq!(
            core.plan_position_update(5.0, 60.0, true, true, false, true, true, false),
            PositionUpdateAction::None
        );
        core.clear();

        // Not seeked:
        // running or retry_paused -> None
        assert_eq!(
            core.plan_position_update(5.0, 60.0, false, false, true, true, true, false),
            PositionUpdateAction::None
        );
        assert_eq!(
            core.plan_position_update(5.0, 60.0, false, false, false, true, true, true),
            PositionUpdateAction::None
        );
        // position >= duration - 0.25 -> None
        assert_eq!(
            core.plan_position_update(59.8, 60.0, false, false, false, true, true, false),
            PositionUpdateAction::None
        );

        // Uncovered position -> SelectOnly
        assert_eq!(
            core.plan_position_update(5.0, 60.0, false, false, false, true, true, false),
            PositionUpdateAction::SelectOnly
        );

        // Covered position -> None
        core.insert(1, 0.0, 10.0);
        assert_eq!(
            core.plan_position_update(5.0, 60.0, false, false, false, true, true, false),
            PositionUpdateAction::None
        );
    }
}

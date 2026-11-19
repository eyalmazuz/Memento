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

//! Subtitle segment clamping and cue validation helpers ported from
//! `asr/asrcontroller.cpp`.

pub const SUBTITLE_TIME_DELTA: f64 = 0.0001;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AdjustedSegment {
    pub start: f64,
    pub end: f64,
}

// qMin(a, b) = (a < b) ? a : b; qMax(a, b) = (a < b) ? b : a.
// Do not replace these with f64::min/max/clamp: Qt's comparison order differs
// for NaN, and clamp panics for reversed bounds.
#[inline]
fn qt_min(a: f64, b: f64) -> f64 {
    if a < b { a } else { b }
}

#[inline]
fn qt_bound(min: f64, value: f64, max: f64) -> f64 {
    let capped = qt_min(max, value);
    if min < capped { capped } else { min }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qt_bound_nan_order_and_reversed_bounds_do_not_panic() {
        assert_eq!(qt_bound(0.0, f64::NAN, 10.0), 0.0);
        assert_eq!(qt_bound(0.0, 2.0, f64::NAN), 2.0);
        assert_eq!(qt_bound(0.0, 2.0, -1.0), 0.0);
        assert_eq!(qt_bound(5.0, 2.0, 10.0), 5.0);
        assert_eq!(qt_bound(5.0, 7.0, 10.0), 7.0);
        assert_eq!(qt_bound(5.0, 12.0, 10.0), 10.0);
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
}

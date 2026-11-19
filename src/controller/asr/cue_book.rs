//! Ordered ASR cue storage and O(log N) interval planning.

use std::sync::mpsc;

use super::{AsrCue, AsrEvent};

pub(super) const WORK_WINDOW_SECONDS: f64 = 30.0;
pub(super) const MIN_WORK_WINDOW_SECONDS: f64 = 0.25;
pub(super) const SUBTITLE_TIME_DELTA: f64 = 0.0001;

#[derive(Clone, Debug)]
pub(super) struct Window {
    pub(super) start: f64,
    pub(super) end: f64,
    pub(super) rewound: bool,
}

pub(super) enum WindowPlan {
    Done,
    Advance { start: f64, rewound: bool },
    Extract(Window),
}

pub(super) struct CueDelta {
    pub(super) removed: Vec<u64>,
    pub(super) added: AsrCue,
}

#[derive(Default)]
pub(super) struct CueBook {
    pub(super) cues: Vec<AsrCue>,
    next_id: u64,
    /// Sorted, disjoint, coalesced intervals where consecutive entries satisfy
    /// `processed[i].1 + SUBTITLE_TIME_DELTA < processed[i + 1].0`.
    processed: Vec<(f64, f64)>,
}

impl CueBook {
    #[cfg(test)]
    pub(super) fn processed(&self) -> &[(f64, f64)] {
        &self.processed
    }

    /// Record a processed audio window, coalescing any adjacent or overlapping
    /// intervals (within `SUBTITLE_TIME_DELTA`) so `self.processed` stays
    /// sorted and disjoint.
    pub(super) fn mark_processed(&mut self, start: f64, end: f64) {
        if !start.is_finite() || !end.is_finite() || end <= start {
            return;
        }
        let first = self
            .processed
            .partition_point(|&(_, existing_end)| existing_end + SUBTITLE_TIME_DELTA < start);
        let last = self
            .processed
            .partition_point(|&(existing_start, _)| existing_start <= end + SUBTITLE_TIME_DELTA);
        if first == last {
            self.processed.insert(first, (start, end));
        } else {
            let merged_start = start.min(self.processed[first].0);
            let merged_end = end.max(self.processed[last - 1].1);
            self.processed[first] = (merged_start, merged_end);
            if first + 1 < last {
                self.processed.drain((first + 1)..last);
            }
        }
    }

    /// Advance `position` past any already-processed interval or retained cue
    /// covering it, using O(log N) binary search over sorted intervals and cues.
    pub(super) fn skip_covered_position(&self, mut position: f64) -> f64 {
        loop {
            let p_idx = self
                .processed
                .partition_point(|&(start, _)| start <= position + SUBTITLE_TIME_DELTA);
            if let Some(&(_, end)) = p_idx.checked_sub(1).and_then(|i| self.processed.get(i)) {
                if position < end {
                    position = end;
                    continue;
                }
            }
            let c_idx = self
                .cues
                .partition_point(|cue| cue.start <= position + SUBTITLE_TIME_DELTA);
            if let Some(cue) = c_idx.checked_sub(1).and_then(|i| self.cues.get(i)) {
                if position < cue.end {
                    position = cue.end;
                    continue;
                }
            }
            return position;
        }
    }

    fn next_subtitle_start(&self, position: f64) -> f64 {
        let cutoff = position + SUBTITLE_TIME_DELTA;
        let next_cue = self
            .cues
            .get(self.cues.partition_point(|cue| cue.start <= cutoff))
            .map_or(f64::INFINITY, |cue| cue.start);
        let next_processed = self
            .processed
            .get(
                self.processed
                    .partition_point(|&(start, _)| start <= cutoff),
            )
            .map_or(f64::INFINITY, |&(start, _)| start);
        next_cue.min(next_processed)
    }

    pub(super) fn plan_window(&self, start: f64, duration: f64, rewound: bool) -> WindowPlan {
        let start = self.skip_covered_position(start).max(0.0).min(duration);
        if start >= duration - MIN_WORK_WINDOW_SECONDS {
            if !rewound {
                let beginning = self.skip_covered_position(0.0);
                if beginning < duration - MIN_WORK_WINDOW_SECONDS {
                    return WindowPlan::Advance {
                        start: beginning,
                        rewound: true,
                    };
                }
            }
            return WindowPlan::Done;
        }
        let next_start = self.next_subtitle_start(start);
        let mut end = duration.min(start + WORK_WINDOW_SECONDS);
        if next_start.is_finite() {
            end = end.min(next_start);
        }
        if end <= start + MIN_WORK_WINDOW_SECONDS {
            return WindowPlan::Advance {
                start: duration.min(start + MIN_WORK_WINDOW_SECONDS),
                rewound,
            };
        }
        WindowPlan::Extract(Window {
            start,
            end,
            rewound,
        })
    }

    /// Insert a cue in sorted `start` order, removing any overlapping cues and
    /// returning the incremental delta (`removed` IDs and `added` cue).
    pub(super) fn insert(&mut self, text: String, start: f64, end: f64) -> CueDelta {
        let hi = self
            .cues
            .partition_point(|cue| cue.start < end - SUBTITLE_TIME_DELTA);
        let lo = self.cues[..hi].partition_point(|cue| cue.end <= start + SUBTITLE_TIME_DELTA);
        let removed = if lo < hi {
            self.cues.drain(lo..hi).map(|cue| cue.id).collect()
        } else {
            Vec::new()
        };
        let cue = AsrCue {
            id: self.next_id,
            text,
            start,
            end,
        };
        self.next_id = self.next_id.wrapping_add(1).max(1);
        let insertion = self
            .cues
            .partition_point(|existing| existing.start < cue.start);
        self.cues.insert(insertion, cue.clone());
        CueDelta {
            removed,
            added: cue,
        }
    }

    #[cfg(test)]
    pub(super) fn add(
        &mut self,
        text: String,
        start: f64,
        end: f64,
        generation: u64,
        sender: &mpsc::Sender<AsrEvent>,
    ) {
        let delta = self.insert(text, start, end);
        for cue_id in delta.removed {
            let _ = sender.send(AsrEvent::CueRemoved { generation, cue_id });
        }
        let _ = sender.send(AsrEvent::CueAdded {
            generation,
            cue: delta.added,
        });
    }

    pub(super) fn clear_all(&mut self, generation: u64, sender: &mpsc::Sender<AsrEvent>) {
        for cue in self.cues.drain(..) {
            let _ = sender.send(AsrEvent::CueRemoved {
                generation,
                cue_id: cue.id,
            });
        }
        self.processed.clear();
        let _ = sender.send(AsrEvent::CuesSnapshot {
            generation,
            cues: Vec::new(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cue_book_replaces_overlapping_cues_and_plans_sliding_windows() {
        let (sender, receiver) = mpsc::channel();
        let mut cues = CueBook::default();
        cues.add("first".into(), 4.0, 6.0, 0, &sender);
        cues.add("replacement".into(), 5.0, 7.0, 0, &sender);
        assert_eq!(cues.cues.len(), 1);
        assert_eq!(cues.cues[0].text, "replacement");
        assert!(matches!(receiver.try_recv(), Ok(AsrEvent::CueAdded { .. })));
        assert!(matches!(
            receiver.try_recv(),
            Ok(AsrEvent::CueRemoved { .. })
        ));
        assert!(matches!(receiver.try_recv(), Ok(AsrEvent::CueAdded { .. })));
        assert!(matches!(
            cues.plan_window(0.0, 70.0, false),
            WindowPlan::Extract(Window {
                start: 0.0,
                end: 5.0,
                ..
            })
        ));
    }

    #[test]
    fn processed_silent_windows_are_never_transcribed_twice() {
        let mut cues = CueBook::default();
        cues.mark_processed(0.0, 30.0);
        assert!(matches!(
            cues.plan_window(0.0, 60.0, false),
            WindowPlan::Extract(Window {
                start: 30.0,
                end: 60.0,
                ..
            })
        ));
        cues.mark_processed(30.0, 60.0);
        assert_eq!(cues.processed(), &[(0.0, 60.0)]);
        assert!(matches!(
            cues.plan_window(0.0, 60.0, false),
            WindowPlan::Done
        ));
    }

    #[test]
    fn mark_processed_coalesces_adjacent_overlapping_and_out_of_order_intervals() {
        let mut cues = CueBook::default();
        cues.mark_processed(60.0, 90.0);
        cues.mark_processed(0.0, 30.0);
        assert_eq!(cues.processed(), &[(0.0, 30.0), (60.0, 90.0)]);

        // Contained sub-interval does not split or duplicate.
        cues.mark_processed(10.0, 20.0);
        assert_eq!(cues.processed(), &[(0.0, 30.0), (60.0, 90.0)]);

        // Adjacent within SUBTITLE_TIME_DELTA bridges two intervals into one.
        cues.mark_processed(30.0 + SUBTITLE_TIME_DELTA * 0.5, 60.0);
        assert_eq!(cues.processed(), &[(0.0, 90.0)]);

        // Disjoint later segments collapsed by a single spanning interval.
        cues.mark_processed(110.0, 120.0);
        cues.mark_processed(130.0, 140.0);
        cues.mark_processed(105.0, 145.0);
        assert_eq!(cues.processed(), &[(0.0, 90.0), (105.0, 145.0)]);

        // Degenerate / non-finite intervals are ignored.
        cues.mark_processed(50.0, 50.0);
        cues.mark_processed(60.0, 40.0);
        cues.mark_processed(f64::NAN, 10.0);
        assert_eq!(cues.processed(), &[(0.0, 90.0), (105.0, 145.0)]);
    }

    #[test]
    fn skip_covered_position_and_next_subtitle_start_handle_many_intervals_and_cues() {
        let (sender, _receiver) = mpsc::channel();
        let mut cues = CueBook::default();
        // Create 100 disjoint processed blocks [0,10], [20,30], ..., [1980,1990]
        // and cues bridging [10,20] at [10,15] and [15,20].
        for i in 0..100 {
            let start = (i * 20) as f64;
            cues.mark_processed(start, start + 10.0);
        }
        cues.add("bridge-a".into(), 10.0, 15.0, 0, &sender);
        cues.add("bridge-b".into(), 15.0, 20.0, 0, &sender);

        // Position 0.0 skips processed [0,10] -> cue [10,15] -> cue [15,20] -> processed [20,30]!
        assert!((cues.skip_covered_position(0.0) - 30.0).abs() < 1e-9);
        // Position inside a middle block skips directly to its end.
        assert!((cues.skip_covered_position(1005.0) - 1010.0).abs() < 1e-9);
        // Next uncovered window from 1010.0 stops at next processed block 1020.0.
        assert!(matches!(
            cues.plan_window(1010.0, 2500.0, false),
            WindowPlan::Extract(Window { start, end, .. })
                if (start - 1010.0).abs() < 1e-9 && (end - 1020.0).abs() < 1e-9
        ));
    }
}

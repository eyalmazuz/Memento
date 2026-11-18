//! Auto-pause state machine and subtitle timeline math for Memento player.

/// Tracks subtitle auto-pause timing and seek-reset state.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AutoPauseState {
    pub previous_position: f64,
    pub start_time: f64,
    pub end_time: f64,
    pub last_end_time: f64,
    pub already_paused: bool,
    pub reset: bool,
}

impl Default for AutoPauseState {
    fn default() -> Self {
        Self {
            previous_position: -1.0,
            start_time: 0.0,
            end_time: 0.0,
            last_end_time: -1.0,
            already_paused: false,
            reset: false,
        }
    }
}

/// Returns whether `new_speed` is a valid new subtitle speed multiplier.
pub fn subtitle_valid_speed(current_speed: f64, new_speed: f64) -> bool {
    new_speed.is_finite() && new_speed > 0.0 && current_speed != new_speed
}

/// Converts a raw cue time to the media playback timeline (`raw_time * speed + delay`).
pub fn subtitle_playback_time(raw_time: f64, speed: f64, delay: f64) -> f64 {
    raw_time * speed + delay
}

/// Converts a media playback time to the raw cue timeline (`(playback_time - delay) / speed`).
pub fn subtitle_raw_time(playback_time: f64, speed: f64, delay: f64) -> f64 {
    (playback_time - delay) / speed
}

impl AutoPauseState {
    /// Corrects auto-pause state on player position updates, matching
    /// `PlayerManager::handleAutoPausePosition(double position)`.
    pub fn update_position(&mut self, position: f64, speed: f64, delay: f64) {
        const SEEK_DELTA: f64 = 1.0;
        let start_time = self.start_time * speed + delay;
        let end_time = self.end_time * speed + delay;

        if self.previous_position < 0.0 {
            // noop
        } else if self.end_time > 0.0 && start_time <= position && position <= end_time {
            // noop
        } else if (self.previous_position - position).abs() > SEEK_DELTA {
            self.reset = true;
        }
        self.previous_position = position;
    }

    /// Handles auto pausing on subtitle events, matching `PlayerManager::handleAutoPause()`.
    /// Returns `Some(clamped_seek_time)` if the player should pause and seek.
    #[allow(clippy::too_many_arguments)]
    pub fn step(
        &mut self,
        setting_enabled: bool,
        sub_start_time: f64,
        sub_end_time: f64,
        sub_speed: f64,
        sub_delay: f64,
        player_paused: bool,
        duration: f64,
    ) -> Option<f64> {
        if !setting_enabled {
            return None;
        }

        if self.reset {
            *self = Self::default();
        }

        let start_time = sub_start_time;
        let end_time = sub_end_time;

        if self.last_end_time == end_time {
            self.start_time = 0.0;
            self.end_time = 0.0;
            return None;
        }

        if end_time == 0.0 {
            if self.end_time == 0.0 {
                return None;
            }
        } else if self.end_time == 0.0 || self.end_time > end_time {
            self.start_time = start_time;
            self.end_time = end_time;
            self.already_paused = false;
            return None;
        }

        if player_paused {
            return None;
        }

        if self.already_paused {
            self.already_paused = false;
            return None;
        }

        let raw_seek = self.end_time * sub_speed + sub_delay - 0.1;
        let lo = 0.0f64;
        let hi = duration;
        let clamped_seek = if raw_seek < lo {
            lo
        } else if hi < raw_seek {
            hi
        } else {
            raw_seek
        };

        self.last_end_time = self.end_time;
        self.start_time = 0.0;
        self.end_time = 0.0;
        self.already_paused = false;

        Some(clamped_seek)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subtitle_valid_speed_checks() {
        assert!(subtitle_valid_speed(1.0, 1.25));
        assert!(subtitle_valid_speed(1.0, 0.5));
        assert!(!subtitle_valid_speed(1.0, 1.0));
        assert!(!subtitle_valid_speed(1.0, 0.0));
        assert!(!subtitle_valid_speed(1.0, -1.0));
        assert!(!subtitle_valid_speed(1.0, f64::NAN));
        assert!(!subtitle_valid_speed(1.0, f64::INFINITY));
        assert!(!subtitle_valid_speed(1.0, f64::NEG_INFINITY));
    }

    #[test]
    fn subtitle_time_math_roundtrip() {
        let speed = 1.5;
        let delay = -0.25;
        let raw = 12.34;
        let playback = subtitle_playback_time(raw, speed, delay);
        let back = subtitle_raw_time(playback, speed, delay);
        assert!((back - raw).abs() < 1e-12);
    }

    #[test]
    fn autopause_position_handling() {
        let mut state = AutoPauseState::default();

        // Initial position update from -1: no reset
        state.update_position(10.0, 1.0, 0.0);
        assert_eq!(state.previous_position, 10.0);
        assert!(!state.reset);

        // Small movement <= SEEK_DELTA (1.0): no reset
        state.update_position(10.5, 1.0, 0.0);
        assert_eq!(state.previous_position, 10.5);
        assert!(!state.reset);

        // Large seek jump > SEEK_DELTA: triggers reset
        state.update_position(15.0, 1.0, 0.0);
        assert_eq!(state.previous_position, 15.0);
        assert!(state.reset);

        // Active subtitle range prevents reset even for jump within range
        state.reset = false;
        state.start_time = 20.0;
        state.end_time = 30.0;
        state.previous_position = 20.0;
        state.update_position(25.0, 1.0, 0.0);
        assert!(!state.reset);
        assert_eq!(state.previous_position, 25.0);
    }

    #[test]
    fn autopause_step_lifecycle() {
        let mut state = AutoPauseState::default();

        // 1. Setting disabled: returns None
        assert_eq!(
            state.step(false, 1.0, 3.0, 1.0, 0.0, false, 100.0),
            None
        );

        // 2. Subtitle begins [2.0, 5.0]: registers cue, returns None
        assert_eq!(
            state.step(true, 2.0, 5.0, 1.0, 0.0, false, 100.0),
            None
        );
        assert_eq!(state.start_time, 2.0);
        assert_eq!(state.end_time, 5.0);

        // 3. Subtitle ends (cue clears to [0.0, 0.0]): triggers pause and seek!
        let seek_out = state
            .step(true, 0.0, 0.0, 1.0, 0.0, false, 100.0)
            .expect("expected seek target");
        assert!((seek_out - 4.9).abs() < 1e-12);
        assert_eq!(state.last_end_time, 5.0);
        assert_eq!(state.start_time, 0.0);
        assert_eq!(state.end_time, 0.0);

        // 4. Repeated end event for same last_end_time: noop
        assert_eq!(
            state.step(true, 0.0, 5.0, 1.0, 0.0, false, 100.0),
            None
        );
    }
}

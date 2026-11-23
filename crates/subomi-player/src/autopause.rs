//! Auto-pause state machine and subtitle timeline math for Subomi player.

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
    if !raw_time.is_finite() || !speed.is_finite() || speed <= 0.0 || !delay.is_finite() {
        return 0.0;
    }
    raw_time * speed + delay
}

/// Converts a media playback time to the raw cue timeline (`(playback_time - delay) / speed`).
pub fn subtitle_raw_time(playback_time: f64, speed: f64, delay: f64) -> f64 {
    if !playback_time.is_finite() || !speed.is_finite() || speed <= 0.0 || !delay.is_finite() {
        return 0.0;
    }
    (playback_time - delay) / speed
}

/// Converts a raw subtitle cue timestamp to the media playback timeline
/// (`(cue_time * safe_speed) + safe_delay`, clamped `>= 0.0`).
///
/// This is the exact inverse of [`subtitle_raw_time`] (`(playback_time - delay) / speed`)
/// for non-negative media timestamps, matching mpv's `sub-speed` (timestamp multiplier)
/// and `sub-delay` semantics while safely handling `NaN`/non-finite/`<= 0.0` `sub_speed`
/// (falling back to `1.0`) and non-finite `sub_delay` (falling back to `0.0`).
pub fn subtitle_to_media_time(cue_time: f64, sub_speed: f64, sub_delay: f64) -> f64 {
    if !cue_time.is_finite() {
        return 0.0;
    }
    let safe_speed = if sub_speed.is_finite() && sub_speed > 0.0 {
        sub_speed
    } else {
        1.0
    };
    let safe_delay = if sub_delay.is_finite() {
        sub_delay
    } else {
        0.0
    };
    let media = cue_time * safe_speed + safe_delay;
    if !media.is_finite() || media <= 0.0 {
        0.0
    } else {
        media
    }
}

impl AutoPauseState {
    /// Corrects auto-pause state on player position updates, matching
    /// `PlayerManager::handleAutoPausePosition(double position)`.
    pub fn update_position(&mut self, position: f64, speed: f64, delay: f64) {
        if !position.is_finite() || !speed.is_finite() || speed <= 0.0 || !delay.is_finite() {
            return;
        }
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
        if !sub_start_time.is_finite()
            || !sub_end_time.is_finite()
            || !sub_speed.is_finite()
            || sub_speed <= 0.0
            || !sub_delay.is_finite()
        {
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
        if !raw_seek.is_finite() {
            self.last_end_time = self.end_time;
            self.start_time = 0.0;
            self.end_time = 0.0;
            self.already_paused = false;
            return None;
        }
        let lo = 0.0f64;
        let clamped_seek = if duration.is_finite() && duration > 0.0 {
            raw_seek.clamp(lo, duration)
        } else {
            raw_seek.max(lo)
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
    fn subtitle_to_media_time_roundtrips_with_subtitle_raw_time() {
        let cases = [
            // (cue_time, sub_speed, sub_delay, expected_media)
            (0.0, 1.0, 0.0, 0.0),
            (10.0, 1.0, 0.0, 10.0),
            (10.0, 1.0, 2.5, 12.5),
            (10.0, 1.0, -2.5, 7.5),
            (10.0, 1.5, 0.75, 15.75),
            (10.0, 0.5, -1.25, 3.75),
            (25.0, 23.976 / 25.0, 1.2, 25.0 * (23.976 / 25.0) + 1.2),
            (42.5, 25.0 / 23.976, -3.5, 42.5 * (25.0 / 23.976) - 3.5),
        ];

        for (cue, speed, delay, expected_media) in cases {
            let media = subtitle_to_media_time(cue, speed, delay);
            assert!(
                (media - expected_media).abs() < 1e-12,
                "cue={cue}, speed={speed}, delay={delay}: got {media}, expected {expected_media}"
            );
            // Round-trip: cue -> media -> cue
            let back_to_cue = subtitle_raw_time(media, speed, delay);
            assert!(
                (back_to_cue - cue).abs() < 1e-12,
                "cue={cue}, speed={speed}, delay={delay}: round-trip gave {back_to_cue}"
            );
            // Reverse round-trip: media -> cue -> media
            let back_to_media = subtitle_to_media_time(back_to_cue, speed, delay);
            assert!(
                (back_to_media - media).abs() < 1e-12,
                "media={media}, speed={speed}, delay={delay}: reverse round-trip gave {back_to_media}"
            );
        }
    }

    #[test]
    fn subtitle_to_media_time_clamps_negative_and_handles_invalid_inputs() {
        // Negative media times clamp to +0.0.
        let clamped = subtitle_to_media_time(1.0, 1.0, -5.0);
        assert_eq!(clamped, 0.0);
        assert!(clamped.is_sign_positive());
        assert_eq!(subtitle_to_media_time(-2.0, 1.5, 1.0), 0.0);

        // Invalid / non-positive sub_speed safely falls back to 1.0.
        for bad_speed in [0.0, -0.0, -1.5, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let media = subtitle_to_media_time(10.0, bad_speed, 0.5);
            assert!(
                (media - 10.5).abs() < 1e-12,
                "bad_speed={bad_speed}: expected 10.5, got {media}"
            );
        }

        // Non-finite sub_delay falls back to 0.0.
        for bad_delay in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let media = subtitle_to_media_time(10.0, 1.5, bad_delay);
            assert!(
                (media - 15.0).abs() < 1e-12,
                "bad_delay={bad_delay}: expected 15.0, got {media}"
            );
        }

        // Non-finite cue_time returns 0.0.
        for bad_cue in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert_eq!(subtitle_to_media_time(bad_cue, 1.0, 2.0), 0.0);
        }

        // Overflow returns 0.0 rather than infinity.
        assert_eq!(subtitle_to_media_time(f64::MAX, 2.0, 0.0), 0.0);
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

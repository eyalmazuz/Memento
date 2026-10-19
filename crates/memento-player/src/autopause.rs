//! Auto-pause state machine and subtitle timeline math for Memento player.

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MementoAutoPauseStateC {
    pub previous_position: f64,
    pub start_time: f64,
    pub end_time: f64,
    pub last_end_time: f64,
    pub already_paused: bool,
    pub reset: bool,
}

impl Default for MementoAutoPauseStateC {
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
#[no_mangle]
pub extern "C" fn memento_player_subtitle_valid_speed(current_speed: f64, new_speed: f64) -> bool {
    new_speed.is_finite() && new_speed > 0.0 && current_speed != new_speed
}

/// Converts a raw cue time to the media playback timeline (`raw_time * speed + delay`).
#[no_mangle]
pub extern "C" fn memento_player_subtitle_playback_time(raw_time: f64, speed: f64, delay: f64) -> f64 {
    raw_time * speed + delay
}

/// Converts a media playback time to the raw cue timeline (`(playback_time - delay) / speed`).
#[no_mangle]
pub extern "C" fn memento_player_subtitle_raw_time(playback_time: f64, speed: f64, delay: f64) -> f64 {
    (playback_time - delay) / speed
}

/// Corrects auto-pause state on player position updates, matching
/// `PlayerManager::handleAutoPausePosition(double position)`.
#[no_mangle]
pub unsafe extern "C" fn memento_player_autopause_position(
    state: *mut MementoAutoPauseStateC,
    position: f64,
    speed: f64,
    delay: f64,
) {
    if state.is_null() {
        return;
    }
    let s = &mut *state;
    const SEEK_DELTA: f64 = 1.0;
    let start_time = s.start_time * speed + delay;
    let end_time = s.end_time * speed + delay;

    if s.previous_position < 0.0 {
        // noop
    } else if s.end_time > 0.0 && start_time <= position && position <= end_time {
        // noop
    } else if (s.previous_position - position).abs() > SEEK_DELTA {
        s.reset = true;
    }
    s.previous_position = position;
}

/// Handles auto pausing on subtitle events, matching `PlayerManager::handleAutoPause()`.
/// Returns `true` (and writes clamped seek target to `out_seek_time`) if the player
/// should pause and seek.
#[no_mangle]
pub unsafe extern "C" fn memento_player_autopause_step(
    state: *mut MementoAutoPauseStateC,
    setting_enabled: bool,
    sub_start_time: f64,
    sub_end_time: f64,
    sub_speed: f64,
    sub_delay: f64,
    player_paused: bool,
    duration: f64,
    out_seek_time: *mut f64,
) -> bool {
    if state.is_null() {
        return false;
    }
    let s = &mut *state;

    if !setting_enabled {
        return false;
    }

    if s.reset {
        *s = MementoAutoPauseStateC::default();
    }

    let start_time = sub_start_time;
    let end_time = sub_end_time;

    if s.last_end_time == end_time {
        s.start_time = 0.0;
        s.end_time = 0.0;
        return false;
    }

    if end_time == 0.0 {
        if s.end_time == 0.0 {
            return false;
        }
    } else if s.end_time == 0.0 {
        s.start_time = start_time;
        s.end_time = end_time;
        s.already_paused = false;
        return false;
    } else if s.end_time > end_time {
        s.start_time = start_time;
        s.end_time = end_time;
        s.already_paused = false;
        return false;
    }

    if player_paused {
        return false;
    }

    if s.already_paused {
        s.already_paused = false;
        return false;
    }

    let raw_seek = s.end_time * sub_speed + sub_delay - 0.1;
    let lo = 0.0f64;
    let hi = duration;
    let clamped_seek = if raw_seek < lo {
        lo
    } else if hi < raw_seek {
        hi
    } else {
        raw_seek
    };

    if !out_seek_time.is_null() {
        *out_seek_time = clamped_seek;
    }

    s.last_end_time = s.end_time;
    s.start_time = 0.0;
    s.end_time = 0.0;
    s.already_paused = false;

    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subtitle_valid_speed_checks() {
        assert!(memento_player_subtitle_valid_speed(1.0, 1.25));
        assert!(memento_player_subtitle_valid_speed(1.0, 0.5));
        assert!(!memento_player_subtitle_valid_speed(1.0, 1.0));
        assert!(!memento_player_subtitle_valid_speed(1.0, 0.0));
        assert!(!memento_player_subtitle_valid_speed(1.0, -1.0));
        assert!(!memento_player_subtitle_valid_speed(1.0, f64::NAN));
        assert!(!memento_player_subtitle_valid_speed(1.0, f64::INFINITY));
        assert!(!memento_player_subtitle_valid_speed(1.0, f64::NEG_INFINITY));
    }

    #[test]
    fn subtitle_time_math_roundtrip() {
        let speed = 1.5;
        let delay = -0.25;
        let raw = 12.34;
        let playback = memento_player_subtitle_playback_time(raw, speed, delay);
        let back = memento_player_subtitle_raw_time(playback, speed, delay);
        assert!((back - raw).abs() < 1e-12);
    }

    #[test]
    fn autopause_position_handling() {
        let mut state = MementoAutoPauseStateC::default();
        unsafe {
            // Null safety
            memento_player_autopause_position(std::ptr::null_mut(), 5.0, 1.0, 0.0);

            // Initial position update from -1: no reset
            memento_player_autopause_position(&mut state, 10.0, 1.0, 0.0);
            assert_eq!(state.previous_position, 10.0);
            assert!(!state.reset);

            // Small movement <= SEEK_DELTA (1.0): no reset
            memento_player_autopause_position(&mut state, 10.5, 1.0, 0.0);
            assert_eq!(state.previous_position, 10.5);
            assert!(!state.reset);

            // Large seek jump > SEEK_DELTA: triggers reset
            memento_player_autopause_position(&mut state, 15.0, 1.0, 0.0);
            assert_eq!(state.previous_position, 15.0);
            assert!(state.reset);

            // Active subtitle range prevents reset even for jump within range
            state.reset = false;
            state.start_time = 20.0;
            state.end_time = 30.0;
            state.previous_position = 20.0;
            memento_player_autopause_position(&mut state, 25.0, 1.0, 0.0);
            assert!(!state.reset);
            assert_eq!(state.previous_position, 25.0);
        }
    }

    #[test]
    fn autopause_step_lifecycle() {
        let mut state = MementoAutoPauseStateC::default();
        let mut seek_out = 0.0;

        unsafe {
            // 1. Setting disabled: returns false
            assert!(!memento_player_autopause_step(
                &mut state,
                false,
                1.0,
                3.0,
                1.0,
                0.0,
                false,
                100.0,
                &mut seek_out,
            ));

            // 2. Subtitle begins [2.0, 5.0]: registers cue, returns false
            assert!(!memento_player_autopause_step(
                &mut state,
                true,
                2.0,
                5.0,
                1.0,
                0.0,
                false,
                100.0,
                &mut seek_out,
            ));
            assert_eq!(state.start_time, 2.0);
            assert_eq!(state.end_time, 5.0);

            // 3. Subtitle ends (cue clears to [0.0, 0.0]): triggers pause and seek!
            assert!(memento_player_autopause_step(
                &mut state,
                true,
                0.0,
                0.0,
                1.0,
                0.0,
                false,
                100.0,
                &mut seek_out,
            ));
            assert!((seek_out - 4.9).abs() < 1e-12);
            assert_eq!(state.last_end_time, 5.0);
            assert_eq!(state.start_time, 0.0);
            assert_eq!(state.end_time, 0.0);

            // 4. Repeated end event for same last_end_time: noop
            assert!(!memento_player_autopause_step(
                &mut state,
                true,
                0.0,
                5.0,
                1.0,
                0.0,
                false,
                100.0,
                &mut seek_out,
            ));
        }
    }
}

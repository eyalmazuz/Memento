//! Integration tests for Player and Subtitle Controllers in memento-app.

use memento_app::controller::player::{
    format_time, is_subtitle_file, is_subtitle_file_utf16, PlayerController,
    QT_ALT_MODIFIER, QT_CONTROL_MODIFIER, QT_KEY_ESCAPE, QT_KEY_F1, QT_KEY_F30,
    QT_KEY_RETURN, QT_KEY_SHIFT, QT_NO_MODIFIER, QT_SHIFT_MODIFIER,
};
use memento_app::controller::subtitles::{create_cue, filter_and_sort_cues, MemorySubtitleCue, SubtitleController, SubtitleTrackInfo};

#[test]
fn asr_cues_accumulate_survive_mpv_sync_and_follow_playback() {
    let mut ctrl = SubtitleController::new();
    ctrl.start_asr_track();
    let cue = |id, text: &str, start, end| MemorySubtitleCue {
        id, text: text.encode_utf16().collect(), start, end,
    };
    assert!(ctrl.add_asr_cue(cue(1, "Second", 5.0, 7.0), 0.0));
    assert!(ctrl.add_asr_cue(cue(0, "First", 1.0, 3.0), 0.0));
    assert_eq!(ctrl.primary_entries().len(), 2);
    assert_eq!(ctrl.cue_text(0).as_deref(), Some("First"));
    ctrl.toggle_select(1);
    assert!(ctrl.add_asr_cue(cue(2, "Middle", 3.0, 4.0), 0.0));
    assert_eq!(ctrl.selected_indices(), &[2]);
    ctrl.sync_tracks(&[SubtitleTrackInfo { id: 2, external: false, external_path: String::new() }]);
    assert_eq!(ctrl.primary_entries().len(), 3);
    ctrl.update_position(5.5);
    assert_eq!(ctrl.active_cue_text().as_deref(), Some("Second"));
    assert!(ctrl.remove_asr_cue(1, 5.5));
    assert_eq!(ctrl.primary_entries().len(), 2);
    assert_eq!(ctrl.active_cue_text(), None);
    ctrl.clear_asr_track();
    assert!(ctrl.primary_entries().is_empty());
}

#[test]
fn switching_subtitle_tracks_retains_asr_cues_and_restores_current_position() {
    let mut ctrl = SubtitleController::new();
    ctrl.ensure_track(2);
    ctrl.set_track_cues(2, vec![create_cue("File subtitle", 10.0, 12.0)]);
    ctrl.start_asr_track();
    let cue = |id, text: &str, start, end| MemorySubtitleCue {
        id, text: text.encode_utf16().collect(), start, end,
    };
    ctrl.add_asr_cue(cue(1, "Earlier ASR", 1.0, 3.0), 2.0);
    ctrl.add_asr_cue(cue(2, "Current ASR", 10.0, 12.0), 2.0);
    ctrl.toggle_select(1);
    ctrl.set_primary_sid(2);
    ctrl.update_positions(11.0, 11.0);
    assert_eq!(ctrl.active_cue_text().as_deref(), Some("File subtitle"));
    // An already-running window may finish while the other track is visible.
    ctrl.add_asr_cue(cue(3, "Later ASR", 13.0, 15.0), 11.0);
    ctrl.sync_tracks(&[SubtitleTrackInfo { id: 2, external: false, external_path: String::new() }]);
    assert_eq!(ctrl.primary_entries().len(), 1);
    assert!(ctrl.secondary_entries().is_empty(), "disabled secondary subtitles must not show ASR");
    ctrl.set_primary_sid(0);
    assert!(ctrl.primary_entries().is_empty(), "None must not show the cached ASR track");
    assert!(ctrl.resume_asr_track(11.0));
    assert_eq!(ctrl.primary_entries().len(), 3);
    assert_eq!(ctrl.selected_indices(), &[1]);
    assert_eq!(ctrl.active_cue_text().as_deref(), Some("Current ASR"));
    ctrl.set_primary_sid(2);
    assert!(ctrl.resume_asr_track(14.0));
    assert_eq!(ctrl.active_cue_text().as_deref(), Some("Later ASR"));
    ctrl.set_primary_sid(2);
    ctrl.clear_asr_track();
    assert_eq!(ctrl.primary_entries().len(), 1, "reset must preserve the file track");
    assert!(!ctrl.resume_asr_track(14.0), "reset must invalidate cached ASR cues");
    ctrl.start_asr_track();
    assert!(ctrl.primary_entries().is_empty());
}

#[test]
fn test_subtitle_cue_filtering_and_sorting() {
    let cues = vec![
        // Valid cue at 10.0
        create_cue("Third cue", 10.0, 15.0),
        // Invalid: start is negative
        create_cue("Negative start", -1.0, 5.0),
        // Valid cue at 1.0
        create_cue("First cue", 1.0, 4.0),
        // Invalid: end <= start
        create_cue("End equal to start", 3.0, 3.0),
        create_cue("End before start", 5.0, 2.0),
        // Invalid: non-finite timestamps
        create_cue("NaN start", f64::NAN, 8.0),
        create_cue("NaN end", 2.0, f64::NAN),
        create_cue("Inf start", f64::INFINITY, 12.0),
        create_cue("Inf end", 0.0, f64::INFINITY),
        // Valid cues at 5.0 (stable sort should preserve order)
        create_cue("Second cue A", 5.0, 7.0),
        create_cue("Second cue B", 5.0, 8.0),
    ];

    let filtered = filter_and_sort_cues(&cues);
    assert_eq!(filtered.len(), 4);

    let texts: Vec<String> = filtered
        .iter()
        .map(|c| String::from_utf16_lossy(&c.text))
        .collect();

    assert_eq!(
        texts,
        vec![
            "First cue".to_string(),
            "Second cue A".to_string(),
            "Second cue B".to_string(),
            "Third cue".to_string(),
        ]
    );

    assert_eq!(filtered[0].start, 1.0);
    assert_eq!(filtered[1].start, 5.0);
    assert_eq!(filtered[2].start, 5.0);
    assert_eq!(filtered[3].start, 10.0);
}

#[test]
fn test_subtitle_controller_loading_and_selection() {
    let mut ctrl = SubtitleController::new();
    assert!(ctrl.is_empty());
    assert_eq!(ctrl.len(), 0);

    let raw_cues = vec![
        create_cue("Line 2", 4.0, 6.0),
        create_cue("Invalid", -2.0, 1.0),
        create_cue("Line 1", 1.0, 3.0),
        create_cue("Line 3", 7.0, 9.0),
    ];

    ctrl.load_cues(&raw_cues);
    assert_eq!(ctrl.len(), 3);
    assert!(!ctrl.is_empty());

    assert_eq!(ctrl.cue_text(0).as_deref(), Some("Line 1"));
    assert_eq!(ctrl.cue_text(1).as_deref(), Some("Line 2"));
    assert_eq!(ctrl.cue_text(2).as_deref(), Some("Line 3"));

    // Selection helpers
    assert!(!ctrl.is_selected(0));
    assert!(!ctrl.is_selected(1));

    ctrl.toggle_select(1);
    assert!(ctrl.is_selected(1));
    assert_eq!(ctrl.selected_indices(), &[1]);
    assert_eq!(ctrl.selected_cues().len(), 1);
    assert_eq!(
        String::from_utf16_lossy(&ctrl.selected_cues()[0].text),
        "Line 2"
    );

    // Toggle off
    ctrl.toggle_select(1);
    assert!(!ctrl.is_selected(1));
    assert!(ctrl.selected_indices().is_empty());
    assert!(ctrl.selected_cues().is_empty());

    // Select all
    ctrl.select_all();
    assert_eq!(ctrl.selected_indices(), &[0, 1, 2]);
    assert_eq!(ctrl.selected_cues().len(), 3);
    assert!(ctrl.is_selected(0));
    assert!(ctrl.is_selected(1));
    assert!(ctrl.is_selected(2));

    // Clear selection
    ctrl.clear_selection();
    assert!(ctrl.selected_indices().is_empty());
    assert!(ctrl.selected_cues().is_empty());
}

#[test]
fn test_subtitle_search_english_and_japanese() {
    let mut ctrl = SubtitleController::new();

    let cues = vec![
        create_cue("Hello World from Memento", 1.0, 3.0),
        create_cue("「いったい何を…」", 4.0, 6.0),
        create_cue("「ロケットを打ち上げるんだ」", 7.0, 9.0),
        create_cue("Quick Brown Fox jumps over lazy dog", 10.0, 12.0),
        create_cue("もう一回、最初から。", 13.0, 15.0),
    ];
    ctrl.set_entries(cues);

    // Case-insensitivity in English
    let matches = ctrl.search("hello", false);
    assert_eq!(matches, vec![0]);

    let matches = ctrl.search("WORLD", false);
    assert_eq!(matches, vec![0]);

    let matches = ctrl.search("bRoWn FoX", false);
    assert_eq!(matches, vec![3]);

    // Whitespace stripping in English
    let matches = ctrl.search("HelloWorld", true);
    assert_eq!(matches, vec![0]);

    let matches = ctrl.search("hello   world", true);
    assert_eq!(matches, vec![0]);

    let matches = ctrl.search("HelloWorld", false);
    assert!(matches.is_empty());

    // Japanese exact search
    let matches = ctrl.search("何を", false);
    assert_eq!(matches, vec![1]);

    let matches = ctrl.search("ロケット", false);
    assert_eq!(matches, vec![2]);

    let matches = ctrl.search("最初から", false);
    assert_eq!(matches, vec![4]);

    // Japanese with whitespace stripping
    let matches = ctrl.search("何 を", true);
    assert_eq!(matches, vec![1]);

    let matches = ctrl.search("ロケ ット", true);
    assert_eq!(matches, vec![2]);

    // Empty queries
    assert!(ctrl.search("", false).is_empty());
    assert!(ctrl.search("", true).is_empty());
    assert!(ctrl.search("   ", true).is_empty());

    // Non-matching query
    assert!(ctrl.search("宇宙人", false).is_empty());
}

#[test]
fn test_subtitle_position_tracking_and_active_cue() {
    let mut ctrl = SubtitleController::new();

    let cues = vec![
        create_cue("Intro", 2.0, 5.0),
        create_cue("Dialogue", 6.0, 9.5),
        create_cue("Outro", 12.0, 15.0),
    ];
    ctrl.set_entries(cues);

    // Before any cue
    assert_eq!(ctrl.update_position(1.0), None);
    assert_eq!(ctrl.active_cue_index(), None);
    assert!(ctrl.active_cue().is_none());
    assert_eq!(ctrl.active_cue_text(), None);

    // Exactly at start of first cue
    assert_eq!(ctrl.update_position(2.0), Some(0));
    assert_eq!(ctrl.active_cue_index(), Some(0));
    assert_eq!(ctrl.active_cue_text().as_deref(), Some("Intro"));

    // Middle of first cue
    assert_eq!(ctrl.update_position(3.5), Some(0));
    assert_eq!(ctrl.active_cue_index(), Some(0));

    // Near end of first cue
    assert_eq!(ctrl.update_position(4.9), Some(0));
    assert_eq!(ctrl.active_cue_index(), Some(0));

    // At end of first cue (position 5.0 + TIME_DELTA is past cue end)
    assert_eq!(ctrl.update_position(5.0), None);
    assert_eq!(ctrl.active_cue_index(), None);

    // In gap between cue 0 and cue 1
    assert_eq!(ctrl.update_position(5.5), None);
    assert_eq!(ctrl.active_cue_index(), None);
    assert!(ctrl.active_cue().is_none());

    // Second cue
    assert_eq!(ctrl.update_position(7.0), Some(1));
    assert_eq!(ctrl.active_cue_text().as_deref(), Some("Dialogue"));

    // Past last cue
    assert_eq!(ctrl.update_position(20.0), None);
    assert_eq!(ctrl.active_cue_index(), None);
}

#[test]
fn test_player_controller_state_updates() {
    let mut player = PlayerController::new();

    // Default state
    assert!(!player.is_playing());
    assert!(!player.playing());
    assert_eq!(player.position(), 0.0);
    assert_eq!(player.duration(), 0.0);
    assert_eq!(player.volume(), 100.0);
    assert!(!player.is_muted());
    assert!(!player.mute());
    assert_eq!(player.speed(), 1.0);

    // Playback state transitions
    player.set_playing(true);
    assert!(player.is_playing());
    assert!(player.playing());

    assert!(!player.toggle_play());
    assert!(!player.is_playing());
    assert!(player.toggle_play());
    assert!(player.is_playing());

    // Position and duration
    player.set_position(125.5);
    assert_eq!(player.position(), 125.5);

    player.set_duration(1420.0);
    assert_eq!(player.duration(), 1420.0);

    // Volume clamping
    player.set_volume(45.0);
    assert_eq!(player.volume(), 45.0);

    player.set_volume(150.0);
    assert_eq!(player.volume(), 100.0);

    player.set_volume(-20.0);
    assert_eq!(player.volume(), 0.0);

    // Mute
    player.set_mute(true);
    assert!(player.is_muted());
    assert!(player.mute());

    assert!(!player.toggle_mute());
    assert!(!player.is_muted());

    // Speed
    player.set_speed(1.25);
    assert_eq!(player.speed(), 1.25);
}

#[test]
fn test_player_time_formatting_and_display() {
    // format_time helper
    assert_eq!(format_time(0.0), "00:00");
    assert_eq!(format_time(9.0), "00:09");
    assert_eq!(format_time(65.0), "01:05");
    assert_eq!(format_time(599.0), "09:59");
    assert_eq!(format_time(3599.0), "59:59");
    assert_eq!(format_time(3600.0), "01:00:00");
    assert_eq!(format_time(3665.0), "01:01:05");
    assert_eq!(format_time(7325.0), "02:02:05");
    assert_eq!(format_time(-10.0), "00:00");

    let mut player = PlayerController::new();

    // Short video (< 1 hour) displays MM:SS / MM:SS
    player.set_position(2.3);
    player.set_duration(1420.0);
    assert_eq!(player.time_display(), "00:02 / 23:40");

    player.set_position(125.0);
    player.set_duration(600.0);
    assert_eq!(player.time_display(), "02:05 / 10:00");

    // Long video (>= 1 hour) displays HH:MM:SS / HH:MM:SS on both sides
    player.set_position(65.0);
    player.set_duration(3665.0);
    assert_eq!(player.time_display(), "00:01:05 / 01:01:05");

    player.set_position(3605.0);
    player.set_duration(7200.0);
    assert_eq!(player.time_display(), "01:00:05 / 02:00:00");
}

#[test]
fn test_player_keypress_formatting() {
    // Bare modifier key returns None
    assert_eq!(
        PlayerController::format_keypress(QT_KEY_SHIFT, QT_NO_MODIFIER, false, ""),
        None
    );

    // Regular key with and without modifiers
    let a_key = 0x41; // 'A'
    assert_eq!(
        PlayerController::format_keypress(a_key, QT_NO_MODIFIER, false, "a"),
        Some("a".to_string())
    );
    assert_eq!(
        PlayerController::format_keypress(a_key, QT_SHIFT_MODIFIER, false, "A"),
        Some("Shift+A".to_string())
    );
    assert_eq!(
        PlayerController::format_keypress(
            a_key,
            QT_CONTROL_MODIFIER | QT_ALT_MODIFIER,
            false,
            "a"
        ),
        Some("Ctrl+Alt+a".to_string())
    );

    // Named keys
    assert_eq!(
        PlayerController::format_keypress(QT_KEY_ESCAPE, QT_NO_MODIFIER, false, ""),
        Some("ESC".to_string())
    );
    assert_eq!(
        PlayerController::format_keypress(QT_KEY_RETURN, QT_NO_MODIFIER, false, ""),
        Some("ENTER".to_string())
    );

    // Function keys
    assert_eq!(
        PlayerController::format_keypress(QT_KEY_F1, QT_NO_MODIFIER, false, ""),
        Some("F1".to_string())
    );
    let f11_key = QT_KEY_F1 + 10;
    assert_eq!(
        PlayerController::format_keypress(f11_key, QT_NO_MODIFIER, false, ""),
        Some("F11".to_string())
    );
    assert_eq!(
        PlayerController::format_keypress(QT_KEY_F30, QT_NO_MODIFIER, false, ""),
        Some("F30".to_string())
    );
}

#[test]
fn test_player_is_subtitle_file() {
    // Subtitle extensions (&str)
    assert!(is_subtitle_file("movie.srt"));
    assert!(is_subtitle_file("movie.SRT"));
    assert!(is_subtitle_file("video.ass"));
    assert!(is_subtitle_file("video.vtt"));
    assert!(is_subtitle_file("sub.idx"));
    assert!(is_subtitle_file("track.sub"));
    assert!(is_subtitle_file(".vtt"));

    // Subtitle extensions via PlayerController method
    assert!(PlayerController::is_subtitle_file("movie.srt"));
    assert!(PlayerController::is_subtitle_file("video.ssa"));

    // Non-subtitle extensions
    assert!(!is_subtitle_file("movie.mp4"));
    assert!(!is_subtitle_file("video.mkv"));
    assert!(!is_subtitle_file("audio.mp3"));
    assert!(!is_subtitle_file("document.txt"));
    assert!(!is_subtitle_file("movie."));
    assert!(!is_subtitle_file(""));

    // UTF-16 code units check
    let srt_u16: Vec<u16> = "video.srt".encode_utf16().collect();
    assert!(is_subtitle_file_utf16(&srt_u16));

    let mp4_u16: Vec<u16> = "video.mp4".encode_utf16().collect();
    assert!(!is_subtitle_file_utf16(&mp4_u16));
}

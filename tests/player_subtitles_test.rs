//! Integration tests for Player and Subtitle Controllers in memento-app.

use memento_app::controller::player::{
    format_time, is_subtitle_file, is_subtitle_file_utf16, PlayerController,
    QT_ALT_MODIFIER, QT_CONTROL_MODIFIER, QT_KEY_ESCAPE, QT_KEY_F1, QT_KEY_F30,
    QT_KEY_RETURN, QT_KEY_SHIFT, QT_NO_MODIFIER, QT_SHIFT_MODIFIER,
};
use memento_app::controller::subtitles::{create_cue, filter_and_sort_cues, MemorySubtitleCue, SubtitleController, SubtitleTrackInfo};

#[test]
fn context_marks_and_range_anchor_follow_live_cues() {
    let mut ctrl = SubtitleController::new();
    ctrl.load_cues(vec![create_cue("Middle", 3.0, 4.0), create_cue("Last", 5.0, 6.0)]);
    ctrl.select_row(1, false);
    ctrl.update_active_cue(1, Some(&create_cue("First", 1.0, 2.0)), 1.5);
    assert_eq!(ctrl.selected_indices(), &[2], "insertion must not move the mark onto another cue");
    ctrl.select_row(0, true);
    assert_eq!(ctrl.selected_indices(), &[0, 1, 2]);
    assert_eq!(ctrl.selected_time_range(), Some((1.0, 6.0)));
    ctrl.update_position(5.5);
    assert_eq!(ctrl.selected_indices(), &[0, 1, 2], "playback must not replace context selection");

    let cue = |id, text: &str, start, end| MemorySubtitleCue {
        id, text: text.encode_utf16().collect(), start, end,
    };
    ctrl.update_memory_cues(1, &[
        cue(4, "Earlier", 0.0, 0.5), cue(1, "First", 1.0, 2.0),
        cue(2, "Middle", 3.0, 4.0), cue(3, "Last", 5.0, 6.0),
    ], 5.5);
    assert_eq!(ctrl.selected_indices(), &[1, 2, 3]);
    ctrl.toggle_select(2);
    assert_eq!(ctrl.selected_time_range(), Some((1.0, 6.0)), "non-adjacent marks include the intervening audio");
    ctrl.clear_selection();
    ctrl.select_row(0, true);
    assert_eq!(ctrl.selected_indices(), &[0], "Clear must also reset the range anchor");

    ctrl.start_asr_track();
    ctrl.add_asr_cue(cue(10, "Middle", 3.0, 4.0), 3.5);
    ctrl.add_asr_cue(cue(11, "Last", 5.0, 6.0), 3.5);
    ctrl.select_row(1, false);
    ctrl.add_asr_cue(cue(9, "First", 1.0, 2.0), 3.5);
    ctrl.select_row(0, true);
    assert_eq!(ctrl.selected_indices(), &[0, 1, 2], "ASR insertion must preserve the range anchor");
    ctrl.set_primary_sid(1);
    assert_eq!(ctrl.selected_indices(), &[0], "marks belong to their subtitle track");
    ctrl.resume_asr_track(5.5);
    assert_eq!(ctrl.selected_indices(), &[0, 1, 2]);
    ctrl.remove_asr_cue(10, 5.5);
    assert_eq!(ctrl.selected_indices(), &[0, 1]);
    ctrl.select_row(99, true);
    assert_eq!(ctrl.selected_indices(), &[0, 1]);
}

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

mod phase2c_live_wire_tests {
    use std::cell::{Cell, RefCell};
    use std::path::Path;
    use std::time::{Duration, Instant};

    use memento_app::controller::asr::{AsrController, AsrCue as ControllerAsrCue, AsrEvent};
    use memento_app::controller::player::{
        compute_mpv_subtitle_visibility, compute_video_render_size, effective_media_title,
        is_image_subtitle_codec, refresh_subtitle_list, LivePlayerCoordinator, MouseInputForwarder,
        QT_CONTROL_MODIFIER, QT_SHIFT_MODIFIER,
    };
    use memento_app::controller::subtitles::SubtitleController;
    use memento_app::{MementoApp, SubEntry};
    use memento_player::{CliOption, MpvInstance};
    use slint::{Model as _, VecModel};

    fn make_headless_mpv(dir: &Path) -> MpvInstance {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join("input.conf"), "").unwrap();
        std::fs::write(
            dir.join("mpv.conf"),
            "ao=null\nvo=libmpv\nhwdec=no\nmsg-level=all=error\n",
        )
        .unwrap();
        MpvInstance::new_with_options(
            Some(dir),
            &[CliOption {
                name: "sub-font-size".into(),
                value: "36".into(),
            }],
        )
        .expect("headless MpvInstance must initialize")
    }

    #[allow(clippy::too_many_arguments)]
    fn pump_sync_until(
        coord: &mut LivePlayerCoordinator,
        app: &MementoApp,
        player: &MpvInstance,
        subtitles: &mut SubtitleController,
        sub_model: &VecModel<SubEntry>,
        asr: &mut AsrController,
        media_generation: &Cell<u64>,
        current_media: &RefCell<Option<String>>,
        asr_restart_pending: &Cell<bool>,
        hover_pause_owned: &Cell<bool>,
        timeout: Duration,
        mut predicate: impl FnMut(&LivePlayerCoordinator, &MementoApp, &SubtitleController) -> bool,
    ) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            let _ = coord.sync(
                app,
                player,
                subtitles,
                sub_model,
                asr,
                media_generation,
                current_media,
                asr_restart_pending,
                hover_pause_owned,
                || Err("no settings".into()),
                |app, text| app.set_active_sub_text(text.into()),
            );
            if predicate(coord, app, subtitles) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(12));
        }
        false
    }

    #[test]
    fn mpv_subtitle_visibility_never_flashes_in_gaps_and_preserves_bitmap_secondary() {
        assert!(is_image_subtitle_codec("hdmv_pgs_subtitle"));
        assert!(is_image_subtitle_codec("dvd_subtitle"));
        assert!(is_image_subtitle_codec("dvb_subtitle"));
        assert!(!is_image_subtitle_codec("subrip"));
        assert!(!is_image_subtitle_codec("ass"));
        assert!(!is_image_subtitle_codec("webvtt"));

        // Text primary + text secondary while overlay owns text -> both stay false even during cue gaps!
        assert_eq!(
            compute_mpv_subtitle_visibility(true, true, false, true, false, false),
            (false, false)
        );
        // Text primary + PGS bitmap secondary -> primary hidden, secondary visible!
        assert_eq!(
            compute_mpv_subtitle_visibility(true, true, false, true, false, true),
            (false, true)
        );
        // hide_while_playing && playing -> overlay hides, so mpv renders both.
        assert_eq!(
            compute_mpv_subtitle_visibility(true, true, true, true, false, false),
            (true, true)
        );
        // Paused under hide_while_playing -> overlay owns text again -> mpv text hidden.
        assert_eq!(
            compute_mpv_subtitle_visibility(true, true, true, false, false, true),
            (false, true)
        );
    }

    #[test]
    fn hidpi_render_size_upscales_in_quantized_buckets_and_mouse_forwarder_tracks_buttons() {
        // 1080p video in 1440p logical @ 2x HiDPI (5120x2880 physical): OpenGL upscales past 1920x1080!
        let (gl_w, gl_h) = compute_video_render_size(2560, 1440, 2.0, (1920, 1080), true);
        assert!(gl_w >= 5120 && gl_w % 64 == 0, "expected quantized HiDPI upscale, got {gl_w}x{gl_h}");
        assert_eq!((gl_w * 9) / 16, gl_h);

        // Tiny drag-resize within the same 64px bucket returns the exact same FBO size (zero thrashing).
        let (w1, h1) = compute_video_render_size(1290, 730, 1.0, (1920, 1080), true);
        let (w2, h2) = compute_video_render_size(1310, 740, 1.0, (1920, 1080), true);
        assert_eq!((w1, h1), (w2, h2));

        // Software fallback still caps at 1.0x source resolution.
        assert_eq!(
            compute_video_render_size(2560, 1440, 2.0, (1920, 1080), false),
            (1920, 1080)
        );

        let mut mouse = MouseInputForwarder::new();
        assert_eq!(
            mouse.record_move(120, 240),
            Some(["mouse".into(), "120".into(), "240".into()])
        );
        assert_eq!(mouse.record_move(120, 240), None, "duplicate coordinates coalesce");
        let mods = MouseInputForwarder::modifiers_mask(true, true, false, false);
        assert_eq!(mods, QT_CONTROL_MODIFIER | QT_SHIFT_MODIFIER);
        let press_cmds = mouse.record_button(125, 245, 1, mods, true);
        assert_eq!(
            press_cmds,
            vec![
                vec!["mouse".to_string(), "125".to_string(), "245".to_string()],
                vec!["keydown".to_string(), "Shift+Ctrl+MOUSE_BTN0".to_string()],
            ]
        );
        // Releasing without modifiers still releases the exact chord pressed!
        let release_cmds = mouse.record_button(130, 250, 1, 0, false);
        assert_eq!(
            release_cmds,
            vec![
                vec!["mouse".to_string(), "130".to_string(), "250".to_string()],
                vec!["keyup".to_string(), "Shift+Ctrl+MOUSE_BTN0".to_string()],
            ]
        );
        assert_eq!(
            mouse.record_wheel(130, 250, 0.0, 15.0),
            Some(["mouse".into(), "130".into(), "250".into(), "3".into()])
        );
    }

    fn write_test_wav(path: &Path, freq: f64, duration_sec: f64) {
        let sr = 16_000u32;
        let n = (sr as f64 * duration_sec) as usize;
        let data_len = (n * 2) as u32;
        let mut bytes = Vec::with_capacity(44 + n * 2);
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&(36 + data_len).to_le_bytes());
        bytes.extend_from_slice(b"WAVEfmt ");
        bytes.extend_from_slice(&16u32.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&sr.to_le_bytes());
        bytes.extend_from_slice(&(sr * 2).to_le_bytes());
        bytes.extend_from_slice(&2u16.to_le_bytes());
        bytes.extend_from_slice(&16u16.to_le_bytes());
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&data_len.to_le_bytes());
        for i in 0..n {
            let t = i as f64 / sr as f64;
            let pcm = ((2.0 * std::f64::consts::PI * freq * t).sin() * 12_000.0) as i16;
            bytes.extend_from_slice(&pcm.to_le_bytes());
        }
        std::fs::write(path, bytes).unwrap();
    }

    #[test]
    fn live_asr_track_drives_mpv_sid_and_autopause_without_skipping_events() {
        i_slint_backend_testing::init_no_event_loop();
        let app = MementoApp::new().unwrap();
        app.set_asr_enabled(true);
        let temp = tempfile::TempDir::new().unwrap();
        let wav_path = temp.path().join("speech.wav");
        write_test_wav(&wav_path, 440.0, 10.0);
        let player = make_headless_mpv(temp.path());
        let mut coord = LivePlayerCoordinator::new(&player);
        let mut subtitles = SubtitleController::new();
        let sub_model = VecModel::<SubEntry>::default();
        let mut asr = AsrController::new();
        let media_gen = Cell::new(1u64);
        let current_media = RefCell::new(None);
        let asr_restart = Cell::new(false);
        let hover_owned = Cell::new(false);

        coord.mark_explicit_media_load(1);
        player.pause().unwrap();
        player.load_file(&wav_path.to_string_lossy()).unwrap();

        assert!(
            pump_sync_until(
                &mut coord,
                &app,
                &player,
                &mut subtitles,
                &sub_model,
                &mut asr,
                &media_gen,
                &current_media,
                &asr_restart,
                &hover_owned,
                Duration::from_secs(6),
                |coord, _, _| coord.playback_state().file_loaded && coord.playback_state().duration > 0.0,
            ),
            "lavfi audio stream did not finish loading"
        );

        // (a) Start native libmpv ASR subtitle track (sid > 0) while asr_running == true!
        coord
            .asr_track_mut()
            .start(&player, "ASR Subtitles", "ja")
            .unwrap();
        subtitles.start_asr_track();
        app.set_asr_track_selected(true);
        app.set_asr_running(true);

        for (id, start, end, text) in [
            (1u64, 0.8, 2.0, "最初のASR字幕"),
            (2u64, 3.2, 4.8, "二番目のASR字幕"),
        ] {
            assert!(coord.handle_asr_event(
                &app,
                &player,
                &mut subtitles,
                0.0,
                AsrEvent::CueAdded {
                    generation: 0,
                    cue: ControllerAsrCue {
                        id,
                        text: text.to_owned(),
                        start,
                        end,
                    },
                },
            ));
        }
        refresh_subtitle_list(&app, &sub_model, &subtitles);

        // Wait for libmpv to assign sid > 0 to the ASR track.
        assert!(
            pump_sync_until(
                &mut coord,
                &app,
                &player,
                &mut subtitles,
                &sub_model,
                &mut asr,
                &media_gen,
                &current_media,
                &asr_restart,
                &hover_owned,
                Duration::from_secs(6),
                |coord, _, _| coord.asr_track().sid().is_some_and(|sid| sid > 0)
                    && coord.playback_state().primary_sid > 0,
            ),
            "ASR track did not receive a real sid > 0 from libmpv"
        );
        let asr_sid = coord.asr_track().sid().unwrap();
        assert!(asr_sid > 0);
        assert_eq!(subtitles.primary_sid(), asr_sid);
        assert_eq!(app.get_subtitle_tracks().row_count(), 0, "internal ASR stream URI must not pollute file subtitle list");

        // Seek into cue 1 while `asr_running == true`: libmpv must natively deliver
        // `sub-text`, `sub-start`, `sub-end` with ZERO event skipping!
        player.seek(1.05).unwrap();
        assert!(
            pump_sync_until(
                &mut coord,
                &app,
                &player,
                &mut subtitles,
                &sub_model,
                &mut asr,
                &media_gen,
                &current_media,
                &asr_restart,
                &hover_owned,
                Duration::from_secs(6),
                |coord, app, _| {
                    app.get_asr_running()
                        && app.get_active_sub_text().as_str() == "最初のASR字幕"
                        && (coord.playback_state().sub_end - 2.0).abs() < 0.05
                },
            ),
            "libmpv did not drive active_sub_text during ASR (got {:?})",
            app.get_active_sub_text()
        );

        // Native `sub-seek` (next subtitle) jumps directly to ASR cue 2!
        player.subtitle_seek(1, true).unwrap();
        assert!(
            pump_sync_until(
                &mut coord,
                &app,
                &player,
                &mut subtitles,
                &sub_model,
                &mut asr,
                &media_gen,
                &current_media,
                &asr_restart,
                &hover_owned,
                Duration::from_secs(6),
                |_, app, _| app.get_active_sub_text().as_str() == "二番目のASR字幕",
            ),
            "native mpv sub-seek failed on ASR track (got {:?})",
            app.get_active_sub_text()
        );

        // Decoupling check: Completed / Paused / Failed stop `asr_running` but NEVER hide or deselect ASR cues!
        for terminal in [
            AsrEvent::Completed {
                generation: 0,
                cue_count: 2,
            },
            AsrEvent::Paused {
                generation: 0,
                message: "temporary error".into(),
            },
            AsrEvent::Failed {
                generation: 0,
                message: "worker error".into(),
            },
        ] {
            coord.handle_asr_event(&app, &player, &mut subtitles, 3.5, terminal);
            assert!(!app.get_asr_running());
            assert!(app.get_asr_track_selected());
            assert_eq!(subtitles.primary_entries().len(), 2);
            assert_eq!(app.get_active_sub_text().as_str(), "二番目のASR字幕");
        }

        // (b) AutoPauseState fires and seeks back (`end - 0.1`) on ASR cues!
        app.set_asr_running(true);
        app.set_auto_pause(true);
        player.seek(1.0).unwrap();
        assert!(pump_sync_until(
            &mut coord,
            &app,
            &player,
            &mut subtitles,
            &sub_model,
            &mut asr,
            &media_gen,
            &current_media,
            &asr_restart,
            &hover_owned,
            Duration::from_secs(5),
            |coord, app, _| {
                (coord.playback_state().position - 1.0).abs() < 0.35
                    && app.get_active_sub_text().as_str() == "最初のASR字幕"
            },
        ));
        player.play().unwrap();
        assert!(
            pump_sync_until(
                &mut coord,
                &app,
                &player,
                &mut subtitles,
                &sub_model,
                &mut asr,
                &media_gen,
                &current_media,
                &asr_restart,
                &hover_owned,
                Duration::from_secs(6),
                |coord, _, _| {
                    coord.playback_state().paused && coord.playback_state().position >= 1.75
                },
            ),
            "AutoPauseState did not pause at end of ASR cue 1 (pos={}, paused={})",
            coord.playback_state().position,
            coord.playback_state().paused
        );
        let paused_pos = coord.playback_state().position;
        assert!(
            (1.75..=2.15).contains(&paused_pos),
            "AutoPauseState should seek back near 1.9s (2.0 - 0.1), got {paused_pos}"
        );
    }

    #[test]
    fn playlist_transition_resets_media_state_updates_title_and_queues_asr_restart() {
        assert_eq!(effective_media_title("Episode 01", "/tmp/ep1.mkv"), "Episode 01");
        assert_eq!(effective_media_title("   ", "/tmp/folder/ep02.mkv"), "ep02.mkv");

        i_slint_backend_testing::init_no_event_loop();
        let app = MementoApp::new().unwrap();
        app.set_asr_enabled(true);
        let temp = tempfile::TempDir::new().unwrap();
        let player = make_headless_mpv(temp.path());
        let mut coord = LivePlayerCoordinator::new(&player);
        let mut subtitles = SubtitleController::new();
        let sub_model = VecModel::<SubEntry>::default();
        let mut asr = AsrController::new();
        let media_gen = Cell::new(1u64);
        let current_media = RefCell::new(None);
        let asr_restart = Cell::new(false);
        let hover_owned = Cell::new(false);

        let ep1 = temp.path().join("ep1.wav");
        let ep2 = temp.path().join("ep2.wav");
        let ep3 = temp.path().join("ep3.wav");
        write_test_wav(&ep1, 440.0, 4.0);
        write_test_wav(&ep2, 660.0, 4.0);
        write_test_wav(&ep3, 880.0, 4.0);

        // Load a 3-item playlist (items 1 & 2 with `force-media-title`, item 3 using filename fallback)!
        coord.mark_explicit_media_load(1);
        player.pause().unwrap();
        player
            .load_file_with_options(
                &ep1.to_string_lossy(),
                false,
                &["force-media-title=First Episode".to_owned()],
            )
            .unwrap();
        player
            .load_file_with_options(
                &ep2.to_string_lossy(),
                true,
                &["force-media-title=Second Episode".to_owned()],
            )
            .unwrap();
        player
            .load_file_with_options(&ep3.to_string_lossy(), true, &[])
            .unwrap();

        assert!(pump_sync_until(
            &mut coord,
            &app,
            &player,
            &mut subtitles,
            &sub_model,
            &mut asr,
            &media_gen,
            &current_media,
            &asr_restart,
            &hover_owned,
            Duration::from_secs(6),
            |coord, app, _| coord.playback_state().file_loaded
                && app.get_media_title().as_str() == "First Episode",
        ));
        assert_eq!(media_gen.get(), 1);

        // Populate subtitle state & select ASR on item 1 before advancing playlist.
        subtitles.start_asr_track();
        coord.handle_asr_event(
            &app,
            &player,
            &mut subtitles,
            0.0,
            AsrEvent::CueAdded {
                generation: 0,
                cue: ControllerAsrCue {
                    id: 1,
                    text: "Ep1 Cue".into(),
                    start: 0.5,
                    end: 2.0,
                },
            },
        );
        app.set_asr_track_selected(true);
        refresh_subtitle_list(&app, &sub_model, &subtitles);
        assert_eq!(sub_model.row_count(), 1);

        // Advance to playlist item 2 (`playlist_next` -> `StartFile` + `FileLoaded`).
        // Disable `asr_enabled` temporarily? Keep it enabled and let `asr_settings_provider`
        // keep `asr_restart` observable before clearing, or inspect `media_gen` + title + cleared cues!
        let saw_restart_queued = Cell::new(false);
        player.playlist_next().unwrap();
        let deadline = Instant::now() + Duration::from_secs(6);
        let mut transitioned = false;
        while Instant::now() < deadline {
            let _ = coord.sync(
                &app,
                &player,
                &mut subtitles,
                &sub_model,
                &mut asr,
                &media_gen,
                &current_media,
                &asr_restart,
                &hover_owned,
                || {
                    saw_restart_queued.set(true);
                    // Simulate "still stopping" once so `asr_restart_pending` stays observable,
                    // then clear on assertion.
                    Err("The previous ASR session is still stopping.".into())
                },
                |app, text| app.set_active_sub_text(text.into()),
            );
            if media_gen.get() >= 2
                && coord.playback_state().file_loaded
                && app.get_media_title().as_str() == "Second Episode"
                && saw_restart_queued.get()
            {
                transitioned = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(12));
        }
        assert!(transitioned, "playlist_next did not load Second Episode");
        assert_eq!(media_gen.get(), 2, "playlist StartFile must increment media_generation");
        assert_eq!(sub_model.row_count(), 0, "StartFile must clear previous file's subtitles");
        assert!(subtitles.primary_entries().is_empty());
        assert!(
            saw_restart_queued.get(),
            "active ASR selection must queue and invoke ASR restart on next playlist item"
        );

        // Advance to playlist item 3 (no `force-media-title` -> falls back to filename `ep3.wav`).
        app.set_asr_enabled(false);
        player.playlist_next().unwrap();
        assert!(pump_sync_until(
            &mut coord,
            &app,
            &player,
            &mut subtitles,
            &sub_model,
            &mut asr,
            &media_gen,
            &current_media,
            &asr_restart,
            &hover_owned,
            Duration::from_secs(6),
            |coord, app, _| media_gen.get() == 3
                && coord.playback_state().file_loaded
                && app.get_media_title().as_str() == "ep3.wav",
        ));
    }

    #[test]
    fn dual_observer_none_preserves_track_ids_set_sid_no_suspends_asr_and_silent_playlist_clears_restart() {
        i_slint_backend_testing::init_no_event_loop();
        let app = MementoApp::new().unwrap();
        app.set_asr_enabled(true);
        let temp = tempfile::TempDir::new().unwrap();

        let wav1 = temp.path().join("a1.wav");
        let wav2 = temp.path().join("a2.wav");
        write_test_wav(&wav1, 440.0, 3.0);
        write_test_wav(&wav2, 880.0, 3.0);
        let ext_vtt = temp.path().join("sec.vtt");
        std::fs::write(
            &ext_vtt,
            "WEBVTT\n\n00:00:00.200 --> 00:00:01.000\n   \n\n00:00:01.200 --> 00:00:02.500\nSecondary Line\n\n",
        )
        .unwrap();

        let player = make_headless_mpv(temp.path());
        let mut coord = LivePlayerCoordinator::new(&player);
        let mut subtitles = SubtitleController::new();
        let sub_model = VecModel::<SubEntry>::default();
        let mut asr = AsrController::new();
        let media_gen = Cell::new(1u64);
        let current_media = RefCell::new(None);
        let asr_restart = Cell::new(false);
        let hover_owned = Cell::new(false);

        // Append a silent video as playlist item 2 to verify `asr_restart_pending` clears on `audio_id == 0`.
        coord.mark_explicit_media_load(1);
        player.pause().unwrap();
        player
            .load_file_with_options(
                &wav1.to_string_lossy(),
                false,
                &[
                    "aid=1".to_owned(),
                    format!("audio-files-append={}", wav2.display()),
                ],
            )
            .unwrap();
        player
            .load_file_with_options(
                "av://lavfi:testsrc=size=64x64:rate=10:duration=2,format=yuv420p",
                true,
                &[],
            )
            .unwrap();

        assert!(pump_sync_until(
            &mut coord,
            &app,
            &player,
            &mut subtitles,
            &sub_model,
            &mut asr,
            &media_gen,
            &current_media,
            &asr_restart,
            &hover_owned,
            Duration::from_secs(6),
            |coord, _, _| coord.playback_state().file_loaded
                && coord.playback_state().audio_tracks.len() >= 2
                && coord.playback_state().audio_id == 1,
        ));

        // Add external subtitle track (`sid = 1`) and select it as secondary (`set_secondary_sid(1)`),
        // then switch `set_aid(2)`. Both dual-observed properties (`MPV_FORMAT_INT64` + `MPV_FORMAT_FLAG`)
        // must preserve their positive IDs across subsequent `drain_events()` calls!
        player
            .command_node_async(&["sub-add", &ext_vtt.to_string_lossy(), "auto", "ExtSub"])
            .unwrap();
        assert!(pump_sync_until(
            &mut coord,
            &app,
            &player,
            &mut subtitles,
            &sub_model,
            &mut asr,
            &media_gen,
            &current_media,
            &asr_restart,
            &hover_owned,
            Duration::from_secs(6),
            |coord, _, _| !coord.playback_state().subtitle_tracks.is_empty(),
        ));
        let ext_sid = coord.playback_state().subtitle_tracks[0].id;
        player.set_secondary_sid(ext_sid).unwrap();
        player.set_aid(2).unwrap();
        assert!(pump_sync_until(
            &mut coord,
            &app,
            &player,
            &mut subtitles,
            &sub_model,
            &mut asr,
            &media_gen,
            &current_media,
            &asr_restart,
            &hover_owned,
            Duration::from_secs(6),
            |coord, app, _| coord.playback_state().secondary_sid == ext_sid
                && coord.playback_state().audio_id == 2
                && app.get_secondary_sid() == ext_sid as i32
                && app.get_audio_id() == 2,
        ));
        // Drain several extra ticks (e.g. seek / time-pos) where `track-list` does NOT re-fire,
        // proving `MpvPropertyValue::None` from `MPV_FORMAT_FLAG` never zeroes `secondary_sid` or `audio_id`!
        player.command(&["seek", "0.5", "absolute+exact"]).unwrap();
        for _ in 0..8 {
            let _ = coord.sync(
                &app,
                &player,
                &mut subtitles,
                &sub_model,
                &mut asr,
                &media_gen,
                &current_media,
                &asr_restart,
                &hover_owned,
                || Err("unused".into()),
                |app, text| app.set_active_sub_text(text.into()),
            );
            std::thread::sleep(Duration::from_millis(15));
        }
        assert_eq!(coord.playback_state().secondary_sid, ext_sid);
        assert_eq!(coord.playback_state().audio_id, 2);

        // Start live ASR subtitle track (`sid > 0`) and verify `command_async(&["set", "sid", "no"])`
        // genuinely suspends ASR (`!app.get_asr_track_selected() && !app.get_asr_running()`).
        coord.asr_track_mut().start(&player, "ASR Subtitles", "ja").unwrap();
        subtitles.start_asr_track();
        app.set_asr_track_selected(true);
        app.set_asr_running(true);
        assert!(pump_sync_until(
            &mut coord,
            &app,
            &player,
            &mut subtitles,
            &sub_model,
            &mut asr,
            &media_gen,
            &current_media,
            &asr_restart,
            &hover_owned,
            Duration::from_secs(6),
            |coord, _, subtitles| coord.asr_track().sid().is_some_and(|sid| sid > 0)
                && subtitles.asr_mpv_sid() == coord.asr_track().sid()
                && coord.playback_state().primary_sid == coord.asr_track().sid().unwrap(),
        ));

        player.command_async(&["set", "sid", "no"]).unwrap();
        assert!(
            pump_sync_until(
                &mut coord,
                &app,
                &player,
                &mut subtitles,
                &sub_model,
                &mut asr,
                &media_gen,
                &current_media,
                &asr_restart,
                &hover_owned,
                Duration::from_secs(6),
                |coord, app, _| coord.playback_state().primary_sid == 0
                    && !app.get_asr_track_selected()
                    && !app.get_asr_running(),
            ),
            "`set sid no` must suspend active ASR instead of being dead code from dual-observer clobber"
        );

        // Re-select ASR and advance playlist to silent video (item 2: `audio_id == 0`):
        // `asr_restart_pending` must clear cleanly instead of staying `true` forever!
        coord.asr_track_mut().select(&player).unwrap();
        app.set_asr_track_selected(true);
        assert!(pump_sync_until(
            &mut coord,
            &app,
            &player,
            &mut subtitles,
            &sub_model,
            &mut asr,
            &media_gen,
            &current_media,
            &asr_restart,
            &hover_owned,
            Duration::from_secs(6),
            |coord, app, _| coord.playback_state().primary_sid > 0 && app.get_asr_track_selected(),
        ));

        player.playlist_next().unwrap();
        assert!(
            pump_sync_until(
                &mut coord,
                &app,
                &player,
                &mut subtitles,
                &sub_model,
                &mut asr,
                &media_gen,
                &current_media,
                &asr_restart,
                &hover_owned,
                Duration::from_secs(6),
                |coord, app, _| media_gen.get() == 2
                    && coord.playback_state().file_loaded
                    && !asr_restart.get()
                    && !app.get_asr_track_selected()
                    && !app.get_asr_running()
                    && app.get_status_message().as_str() == "No active audio track; ASR paused",
            ),
            "Silent playlist item did not clear asr_restart_pending (restart={}, selected={}, status={:?})",
            asr_restart.get(),
            app.get_asr_track_selected(),
            app.get_status_message()
        );

        // Also verify `SubtitleController::start_asr_track` resets stale `asr_mpv_sid`:
        let mut stale_check = SubtitleController::new();
        stale_check.start_asr_track();
        stale_check.bind_asr_sid(2);
        // Simulate new external track reusing sid = 2 before `start_asr_track` is called again:
        stale_check.clear();
        stale_check.ensure_track(2);
        stale_check.start_asr_track();
        assert_eq!(stale_check.asr_mpv_sid(), None);
        assert!(
            stale_check.track(2).is_some(),
            "start_asr_track must not evict an external subtitle track at a stale previous asr_mpv_sid"
        );
    }

    #[cfg(feature = "asr-onnx")]
    mod real_onnx_asr_integration {
        use super::*;
        use memento_app::controller::asr::{AsrBackend, AsrSettings, OnnxSettings, WhisperSettings};

        // Minimal protobuf wire-format helpers to build tiny real ONNX graphs in-memory.
        fn pb_varint(mut v: u64, out: &mut Vec<u8>) {
            while v >= 0x80 {
                out.push(((v as u8) & 0x7f) | 0x80);
                v >>= 7;
            }
            out.push(v as u8);
        }
        fn pb_tag(field: u32, wire: u8, out: &mut Vec<u8>) {
            pb_varint(((field as u64) << 3) | (wire as u64), out);
        }
        fn pb_bytes(field: u32, data: &[u8], out: &mut Vec<u8>) {
            pb_tag(field, 2, out);
            pb_varint(data.len() as u64, out);
            out.extend_from_slice(data);
        }
        fn pb_str(field: u32, s: &str, out: &mut Vec<u8>) {
            pb_bytes(field, s.as_bytes(), out);
        }
        fn pb_uint64(field: u32, v: u64, out: &mut Vec<u8>) {
            pb_tag(field, 0, out);
            pb_varint(v, out);
        }
        fn onnx_dim_value(v: i64) -> Vec<u8> {
            let mut d = Vec::new();
            pb_uint64(1, v as u64, &mut d);
            d
        }
        fn onnx_dim_param(p: &str) -> Vec<u8> {
            let mut d = Vec::new();
            pb_str(2, p, &mut d);
            d
        }
        fn onnx_value_info(name: &str, elem_type: u64, dims: &[Vec<u8>]) -> Vec<u8> {
            let mut shape = Vec::new();
            for dim in dims {
                pb_bytes(1, dim, &mut shape);
            }
            let mut tensor_type = Vec::new();
            pb_uint64(1, elem_type, &mut tensor_type);
            pb_bytes(2, &shape, &mut tensor_type);
            let mut type_proto = Vec::new();
            pb_bytes(1, &tensor_type, &mut type_proto);
            let mut vi = Vec::new();
            pb_str(1, name, &mut vi);
            pb_bytes(2, &type_proto, &mut vi);
            vi
        }

        /// Build a tiny valid ONNX ModelProto:
        /// - `valid_3d == true`: `Identity(input_features [1, T, 4]) -> logits [1, T, 4]`
        /// - `valid_3d == false`: `Identity(attention_mask [1, T]) -> logits [1, T]` (loads in ORT,
        ///   fails rank-3 check in `transcribe_ctc` so worker deterministically hits `Paused` after 3 windows!)
        fn build_test_ctc_onnx(valid_3d: bool) -> Vec<u8> {
            let mut node = Vec::new();
            pb_str(1, if valid_3d { "input_features" } else { "attention_mask" }, &mut node);
            pb_str(2, "logits", &mut node);
            pb_str(4, "Identity", &mut node);

            let in_feat = onnx_value_info(
                "input_features",
                1, // FLOAT
                &[onnx_dim_value(1), onnx_dim_param("T"), onnx_dim_value(4)],
            );
            let in_mask = onnx_value_info(
                "attention_mask",
                7, // INT64
                &[onnx_dim_value(1), onnx_dim_param("T")],
            );
            let out_logits = if valid_3d {
                onnx_value_info(
                    "logits",
                    1,
                    &[onnx_dim_value(1), onnx_dim_param("T"), onnx_dim_value(4)],
                )
            } else {
                onnx_value_info("logits", 7, &[onnx_dim_value(1), onnx_dim_param("T")])
            };

            let mut graph = Vec::new();
            pb_bytes(1, &node, &mut graph);
            pb_str(2, "tiny_ctc", &mut graph);
            pb_bytes(11, &in_feat, &mut graph);
            pb_bytes(11, &in_mask, &mut graph);
            pb_bytes(12, &out_logits, &mut graph);

            let mut opset = Vec::new();
            pb_str(1, "", &mut opset);
            pb_uint64(2, 13, &mut opset);

            let mut model = Vec::new();
            pb_uint64(1, 8, &mut model); // ir_version = 8
            pb_bytes(7, &graph, &mut model);
            pb_bytes(8, &opset, &mut model);
            model
        }

        fn write_onnx_model_dir(dir: &Path, valid_3d: bool) {
            std::fs::create_dir_all(dir).unwrap();
            std::fs::write(
                dir.join("config.json"),
                r#"{"model_type":"parakeet_ctc","ctc_blank_token_id":0,"vocab_size":4,"subsampling_factor":1}"#,
            )
            .unwrap();
            std::fs::write(
                dir.join("tokenizer.json"),
                r#"{"model":{"vocab":{"<blank>":0,"あ":1,"い":2,"う":3}}}"#,
            )
            .unwrap();
            std::fs::write(dir.join("model.onnx"), build_test_ctc_onnx(valid_3d)).unwrap();
        }

        fn test_onnx_settings(config_dir: &Path, model_dir: &Path) -> AsrSettings {
            AsrSettings {
                config_dir: config_dir.to_path_buf(),
                enabled: true,
                backend: AsrBackend::Onnx,
                start_from_beginning: false,
                whisper: WhisperSettings {
                    model: "base".into(),
                    custom_model: String::new(),
                    vad_enabled: false,
                    vad_model: String::new(),
                    use_gpu: false,
                    gpu_device: 0,
                    threads: 1,
                    best_of: 1,
                    beam_size: 1,
                    flash_attention: false,
                    suppress_regex: String::new(),
                },
                onnx: OnnxSettings {
                    model: "custom".into(),
                    custom_model: model_dir.to_string_lossy().into_owned(),
                    use_gpu: false,
                    gpu_device: 0,
                    threads: 1,
                    punctuation: false,
                },
            }
        }

        fn write_sine_wav(wav_path: &Path, freq: f64, duration_sec: f64) {
            let sr = 16_000u32;
            let n = (sr as f64 * duration_sec) as usize;
            let mut bytes = Vec::with_capacity(44 + n * 2);
            let data_len = (n * 2) as u32;
            bytes.extend_from_slice(b"RIFF");
            bytes.extend_from_slice(&(36 + data_len).to_le_bytes());
            bytes.extend_from_slice(b"WAVEfmt ");
            bytes.extend_from_slice(&16u32.to_le_bytes());
            bytes.extend_from_slice(&1u16.to_le_bytes()); // PCM
            bytes.extend_from_slice(&1u16.to_le_bytes()); // mono
            bytes.extend_from_slice(&sr.to_le_bytes());
            bytes.extend_from_slice(&(sr * 2).to_le_bytes());
            bytes.extend_from_slice(&2u16.to_le_bytes());
            bytes.extend_from_slice(&16u16.to_le_bytes());
            bytes.extend_from_slice(b"data");
            bytes.extend_from_slice(&data_len.to_le_bytes());
            for i in 0..n {
                let t = i as f64 / sr as f64;
                let s = (2.0 * std::f64::consts::PI * freq * t).sin() * 0.6;
                let pcm = (s * i16::MAX as f64) as i16;
                bytes.extend_from_slice(&pcm.to_le_bytes());
            }
            std::fs::write(wav_path, bytes).unwrap();
        }

        fn mux_dual_audio_mkv(wav1: &Path, wav2: &Path, out_mkv: &Path) {
            let status = std::process::Command::new("ffmpeg")
                .args([
                    "-y",
                    "-loglevel",
                    "error",
                    "-i",
                    &wav1.to_string_lossy(),
                    "-i",
                    &wav2.to_string_lossy(),
                    "-map",
                    "0:a",
                    "-map",
                    "1:a",
                    "-c:a",
                    "pcm_s16le",
                    &out_mkv.to_string_lossy(),
                ])
                .status()
                .expect("ffmpeg must be available to mux 2-stream dual.mkv");
            assert!(status.success(), "ffmpeg muxing dual.mkv failed: {status}");
        }

        #[test]
        fn seeking_during_asr_calls_notify_seek_and_wakes_paused_worker() {
            i_slint_backend_testing::init_no_event_loop();
            let app = MementoApp::new().unwrap();
            app.set_asr_enabled(true);
            let temp = tempfile::TempDir::new().unwrap();
            let failing_model_dir = temp.path().join("failing_ctc");
            write_onnx_model_dir(&failing_model_dir, false);
            let wav_path = temp.path().join("long_tone.wav");
            // Create a 95s WAV so 3 consecutive 25s windows fail -> worker enters `Paused`!
            {
                let sr = 16_000u32;
                let n = sr as usize * 95;
                let data_len = (n * 2) as u32;
                let mut bytes = Vec::with_capacity(44 + n * 2);
                bytes.extend_from_slice(b"RIFF");
                bytes.extend_from_slice(&(36 + data_len).to_le_bytes());
                bytes.extend_from_slice(b"WAVEfmt ");
                bytes.extend_from_slice(&16u32.to_le_bytes());
                bytes.extend_from_slice(&1u16.to_le_bytes());
                bytes.extend_from_slice(&1u16.to_le_bytes());
                bytes.extend_from_slice(&sr.to_le_bytes());
                bytes.extend_from_slice(&(sr * 2).to_le_bytes());
                bytes.extend_from_slice(&2u16.to_le_bytes());
                bytes.extend_from_slice(&16u16.to_le_bytes());
                bytes.extend_from_slice(b"data");
                bytes.extend_from_slice(&data_len.to_le_bytes());
                for i in 0..n {
                    let s = if (i / 160) % 2 == 0 { 6000i16 } else { -6000i16 };
                    bytes.extend_from_slice(&s.to_le_bytes());
                }
                std::fs::write(&wav_path, bytes).unwrap();
            }

            let player = make_headless_mpv(temp.path());
            let mut coord = LivePlayerCoordinator::new(&player);
            let mut subtitles = SubtitleController::new();
            let sub_model = VecModel::<SubEntry>::default();
            let mut asr = AsrController::new();
            let media_gen = Cell::new(1u64);
            let current_media = RefCell::new(None);
            let asr_restart = Cell::new(false);
            let hover_owned = Cell::new(false);

            coord.mark_explicit_media_load(1);
            player.pause().unwrap();
            player.load_file(&wav_path.to_string_lossy()).unwrap();

            assert!(pump_sync_until(
                &mut coord,
                &app,
                &player,
                &mut subtitles,
                &sub_model,
                &mut asr,
                &media_gen,
                &current_media,
                &asr_restart,
                &hover_owned,
                Duration::from_secs(6),
                |coord, _, _| coord.playback_state().file_loaded && coord.playback_state().audio_id > 0,
            ));

            let settings = test_onnx_settings(temp.path(), &failing_model_dir);
            coord.toggle_or_start_asr(
                &app,
                &player,
                &mut asr,
                &mut subtitles,
                &sub_model,
                &asr_restart,
                || Ok(settings.clone()),
            );
            assert!(app.get_asr_track_selected());
            assert!(app.get_asr_running());

            // Wait for 3 window failures -> AsrEvent::Paused sets `asr_running = false` while keeping `asr_track_selected = true`!
            assert!(
                pump_sync_until(
                    &mut coord,
                    &app,
                    &player,
                    &mut subtitles,
                    &sub_model,
                    &mut asr,
                    &media_gen,
                    &current_media,
                    &asr_restart,
                    &hover_owned,
                    Duration::from_secs(12),
                    |_, app, _| !app.get_asr_running()
                        && app.get_status_message().as_str().starts_with("ASR paused:"),
                ),
                "ASR worker did not enter Paused state (status={:?})",
                app.get_status_message()
            );
            assert!(app.get_asr_track_selected(), "Paused ASR must remain selected");

            // (c) Seeking during Paused ASR calls `notify_seek` and wakes the worker (`WindowStarted` -> `asr_running == true`)!
            let dur = coord.playback_state().duration;
            player.seek(80.0).unwrap();
            coord.notify_user_seek(&mut asr, Some(80.0), dur);
            assert!(
                pump_sync_until(
                    &mut coord,
                    &app,
                    &player,
                    &mut subtitles,
                    &sub_model,
                    &mut asr,
                    &media_gen,
                    &current_media,
                    &asr_restart,
                    &hover_owned,
                    Duration::from_secs(8),
                    |_, app, _| app.get_asr_running(),
                ),
                "notify_user_seek / MPV_EVENT_SEEK did not wake Paused ASR worker"
            );
            asr.shutdown();
        }

        #[test]
        fn switching_aid_updates_live_asr_and_clears_previous_track_cues() {
            i_slint_backend_testing::init_no_event_loop();
            let app = MementoApp::new().unwrap();
            app.set_asr_enabled(true);
            let temp = tempfile::TempDir::new().unwrap();
            let model_dir = temp.path().join("valid_ctc");
            write_onnx_model_dir(&model_dir, true);

            let wav1 = temp.path().join("track1.wav");
            let wav2 = temp.path().join("track2.wav");
            let dual_mkv = temp.path().join("dual.mkv");
            write_sine_wav(&wav1, 350.0, 4.0);
            write_sine_wav(&wav2, 3500.0, 4.0);
            mux_dual_audio_mkv(&wav1, &wav2, &dual_mkv);

            let player = make_headless_mpv(temp.path());
            let mut coord = LivePlayerCoordinator::new(&player);
            let mut subtitles = SubtitleController::new();
            let sub_model = VecModel::<SubEntry>::default();
            let mut asr = AsrController::new();
            let media_gen = Cell::new(1u64);
            let current_media = RefCell::new(None);
            let asr_restart = Cell::new(false);
            let hover_owned = Cell::new(false);

            coord.mark_explicit_media_load(1);
            player.pause().unwrap();
            player
                .load_file_with_options(&dual_mkv.to_string_lossy(), false, &["aid=1".to_owned()])
                .unwrap();

            assert!(pump_sync_until(
                &mut coord,
                &app,
                &player,
                &mut subtitles,
                &sub_model,
                &mut asr,
                &media_gen,
                &current_media,
                &asr_restart,
                &hover_owned,
                Duration::from_secs(6),
                |coord, _, _| coord.playback_state().file_loaded
                    && coord.playback_state().audio_tracks.len() >= 2
                    && coord.playback_state().audio_id == 1,
            ));

            let settings = test_onnx_settings(temp.path(), &model_dir);
            coord.toggle_or_start_asr(
                &app,
                &player,
                &mut asr,
                &mut subtitles,
                &sub_model,
                &asr_restart,
                || Ok(settings.clone()),
            );

            // Wait for real ONNX Runtime transcription on aid=1 to populate ASR cues and sid > 0!
            assert!(
                pump_sync_until(
                    &mut coord,
                    &app,
                    &player,
                    &mut subtitles,
                    &sub_model,
                    &mut asr,
                    &media_gen,
                    &current_media,
                    &asr_restart,
                    &hover_owned,
                    Duration::from_secs(10),
                    |coord, _, subtitles| !subtitles.primary_entries().is_empty()
                        && coord.asr_track().sid().is_some_and(|sid| sid > 0),
                ),
                "Real ONNX ASR did not emit cues for aid=1"
            );

            // Plant a sentinel cue on aid=1 (`99.0..100.0`, outside the 4s file) so we deterministically
            // verify BOTH that aid=1's stale cues were wiped AND that aid=2 transcribed fresh ONNX cues!
            subtitles.add_asr_cue(
                memento_app::controller::subtitles::MemorySubtitleCue {
                    id: 999_999,
                    text: "STALE_AID1_SENTINEL".encode_utf16().collect(),
                    start: 99.0,
                    end: 100.0,
                },
                0.0,
            );
            assert!(subtitles
                .primary_entries()
                .iter()
                .any(|e| String::from_utf16_lossy(&e.text).contains("STALE_AID1_SENTINEL")));

            // (d) Switch to real embedded container stream `aid = 2`!
            player.set_aid(2).unwrap();
            assert!(
                pump_sync_until(
                    &mut coord,
                    &app,
                    &player,
                    &mut subtitles,
                    &sub_model,
                    &mut asr,
                    &media_gen,
                    &current_media,
                    &asr_restart,
                    &hover_owned,
                    Duration::from_secs(10),
                    |coord, app, subtitles| coord.playback_state().audio_id == 2
                        && app.get_audio_id() == 2
                        && !subtitles.primary_entries().is_empty()
                        && !subtitles
                            .primary_entries()
                            .iter()
                            .any(|e| String::from_utf16_lossy(&e.text).contains("STALE_AID1_SENTINEL")),
                ),
                "Switching aid=1 -> aid=2 must clear stale aid=1 cues and transcribe fresh aid=2 cues"
            );
            assert!(asr.is_active(), "ASR session must remain active after switching to aid=2");

            // Disabling audio (`set_aid(0)`) stops ASR cleanly.
            player.set_aid(0).unwrap();
            assert!(
                pump_sync_until(
                    &mut coord,
                    &app,
                    &player,
                    &mut subtitles,
                    &sub_model,
                    &mut asr,
                    &media_gen,
                    &current_media,
                    &asr_restart,
                    &hover_owned,
                    Duration::from_secs(6),
                    |coord, app, _| coord.playback_state().audio_id == 0
                        && !app.get_asr_track_selected()
                        && !app.get_asr_running(),
                ),
                "Disabling audio track (aid=0) did not stop ASR cleanly"
            );
            assert!(!asr.is_active());
            assert!(asr.current_cues().is_empty());
            asr.shutdown();
        }
    }
}

#[test]
fn test_secondary_subtitle_entries_and_active_tracking() {
    let mut ctrl = SubtitleController::new();
    ctrl.ensure_track(1);
    ctrl.set_track_cues(
        1,
        vec![
            create_cue("Primary line 1", 1.0, 3.0),
            create_cue("Primary line 2", 4.0, 6.0),
        ],
    );
    ctrl.ensure_track(2);
    ctrl.set_track_cues(
        2,
        vec![
            create_cue("Secondary English 1", 1.0, 3.0),
            create_cue("Secondary English 2", 4.0, 6.0),
        ],
    );
    ctrl.set_primary_sid(1);
    ctrl.set_secondary_sid(2);
    ctrl.update_positions(4.5, 4.5);

    assert_eq!(ctrl.primary_entries().len(), 2);
    assert_eq!(ctrl.secondary_entries().len(), 2);
    assert_eq!(ctrl.active_indices(1), &[1]);
    assert_eq!(ctrl.active_indices(2), &[1]);

    ctrl.set_secondary_sid(0);
    assert!(ctrl.secondary_entries().is_empty());
}

#[test]
fn test_drag_and_drop_uri_list_partitioning_for_playlists_and_subtitles() {
    let uri_list = "\
# Dropped playlist and external subtitles\r\n\
file:///home/user/Videos/Episode%2001.mkv\r\n\
file:///home/user/Videos/Episode%2002.mp4\r\n\
file:///home/user/Videos/Episode%2001.ja.srt\r\n\
file:///home/user/Videos/Episode%2001.en.ass\r\n\
file:///home/user/Videos/Episode%2001.vtt\r\n\
file:///home/user/Videos/Episode%2001.sub\r\n";

    let paths = memento_os::parse_uri_list(uri_list);
    assert_eq!(paths.len(), 6);
    let (subtitles, media): (Vec<_>, Vec<_>) = paths
        .into_iter()
        .partition(|path| is_subtitle_file(path.as_path()));
    assert_eq!(media.len(), 2);
    assert_eq!(subtitles.len(), 4);
    assert_eq!(media[0].to_string_lossy(), "/home/user/Videos/Episode 01.mkv");
    assert_eq!(media[1].to_string_lossy(), "/home/user/Videos/Episode 02.mp4");
}

#[test]
fn test_screensaver_playback_inhibition_lifecycle() {
    let mut ss = memento_os::Screensaver::new();
    assert!(!ss.is_inhibited());
    // Paused or unloaded media must never leave screensaver inhibited.
    ss.sync_playback(false, false, "Playing media");
    assert!(!ss.is_inhibited());
    ss.sync_playback(true, true, "Playing media");
    assert!(!ss.is_inhibited());
    ss.uninhibit();
    assert!(!ss.is_inhibited());
}

#[test]
fn refresh_active_rows_clears_previous_highlights_even_when_previous_slice_is_stale() {
    use memento_app::controller::player::refresh_active_rows;
    use memento_app::SubEntry;
    use slint::{Model, VecModel};

    let model = VecModel::from(vec![
        SubEntry { id: 1, start: 91.0, end: 94.0, timestamp: format_time(91.0).into(), text: "ニア殿".into(), selected: false, is_active: true, search_match: false },
        SubEntry { id: 2, start: 95.0, end: 97.5, timestamp: format_time(95.0).into(), text: "よくぞお越しくださいました!".into(), selected: false, is_active: false, search_match: false },
        SubEntry { id: 3, start: 98.0, end: 99.5, timestamp: format_time(98.0).into(), text: "そういうのやめて".into(), selected: false, is_active: false, search_match: false },
    ]);

    // Simulate update_active_cue mutating SubtitleController::active_indices to [1] before
    // sync() captured before_active=[1] and called refresh_active_rows(model, &[1], &[1]).
    refresh_active_rows(&model, &[1], &[1]);
    assert!(!model.row_data(0).unwrap().is_active, "previous row 0 highlight must be cleared");
    assert!(model.row_data(1).unwrap().is_active, "new row 1 must be highlighted");
    assert!(!model.row_data(2).unwrap().is_active);

    // Advance again with stale previous slice -> row 1 cleared, only row 2 active.
    refresh_active_rows(&model, &[2], &[2]);
    assert!(!model.row_data(0).unwrap().is_active);
    assert!(!model.row_data(1).unwrap().is_active, "previous row 1 highlight must be cleared");
    assert!(model.row_data(2).unwrap().is_active);

    // Gap between cues -> every highlight cleared.
    refresh_active_rows(&model, &[], &[]);
    assert!(!model.row_data(0).unwrap().is_active);
    assert!(!model.row_data(1).unwrap().is_active);
    assert!(!model.row_data(2).unwrap().is_active);
}

#[test]
fn autofit_window_size_scales_accounts_for_drawer_clamps_and_skips_fullscreen_or_maximized() {
    use memento_app::controller::player::compute_autofit_window_size;

    // 100% 720p video fits cleanly without drawer.
    assert_eq!(
        compute_autofit_window_size(1280, 720, true, 100, false, false, false, 360.0, (1920.0, 1080.0)),
        Some((1280.0, 720.0))
    );

    // 50% 1080p video scales to 960x540.
    assert_eq!(
        compute_autofit_window_size(1920, 1080, true, 50, false, false, false, 360.0, (1920.0, 1080.0)),
        Some((960.0, 540.0))
    );

    // Open side drawer width is added to the scaled video width.
    assert_eq!(
        compute_autofit_window_size(1280, 720, true, 100, false, false, true, 360.0, (1920.0, 1080.0)),
        Some((1640.0, 720.0))
    );

    // Oversized 4K video clamps uniformly to the screen bound preserving 16:9 aspect.
    assert_eq!(
        compute_autofit_window_size(3840, 2160, true, 100, false, false, false, 0.0, (1920.0, 1080.0)),
        Some((1920.0, 1080.0))
    );

    // 1080p + 360px drawer exceeds 1920px wide and scales down uniformly within bounds.
    let clamped_with_drawer = compute_autofit_window_size(
        1920, 1080, true, 100, false, false, true, 360.0, (1920.0, 1080.0),
    )
    .expect("should produce clamped size");
    assert!(clamped_with_drawer.0 <= 1920.0 && clamped_with_drawer.1 <= 1080.0);
    assert_eq!(clamped_with_drawer.0, 1920.0);

    // Disabled, fullscreen, maximized, or zero dimensions must never resize.
    assert_eq!(
        compute_autofit_window_size(1280, 720, false, 100, false, false, false, 0.0, (1920.0, 1080.0)),
        None
    );
    assert_eq!(
        compute_autofit_window_size(1280, 720, true, 100, true, false, false, 0.0, (1920.0, 1080.0)),
        None
    );
    assert_eq!(
        compute_autofit_window_size(1280, 720, true, 100, false, true, false, 0.0, (1920.0, 1080.0)),
        None
    );
    assert_eq!(
        compute_autofit_window_size(0, 720, true, 100, false, false, false, 0.0, (1920.0, 1080.0)),
        None
    );
}

#[test]
fn subtitle_cursor_zones_and_overlay_visibility_reveal_primary_bottom_and_secondary_top() {
    use memento_app::controller::player::{
        compute_subtitle_overlay_visibility, subtitle_cursor_zones,
    };

    // 1000px tall video: 25% top band (<= 250px), 25% bottom band (>= 750px), middle = neither.
    assert_eq!(subtitle_cursor_zones(0.0, 1000.0), (true, false));
    assert_eq!(subtitle_cursor_zones(150.0, 1000.0), (true, false));
    assert_eq!(subtitle_cursor_zones(220.0, 1000.0), (true, false));
    assert_eq!(subtitle_cursor_zones(500.0, 1000.0), (false, false));
    assert_eq!(subtitle_cursor_zones(780.0, 1000.0), (false, true));
    assert_eq!(subtitle_cursor_zones(850.0, 1000.0), (false, true));
    assert_eq!(subtitle_cursor_zones(1000.0, 1000.0), (false, true));
    assert_eq!(subtitle_cursor_zones(-5.0, 1000.0), (false, false));
    assert_eq!(subtitle_cursor_zones(500.0, 0.0), (false, false));

    // H3.6 precedence: `!subtitles_visible` and `(hide_while_playing && is_playing)`
    // always hide subtitles even when cursor-show is enabled and the pointer is in the zone.
    assert!(!compute_subtitle_overlay_visibility(true, false, false, false, true, true));
    assert!(!compute_subtitle_overlay_visibility(true, true, true, true, true, true));
    // When subtitles are visible and not hidden while playing (e.g. paused), cursor-show reveals in zone.
    assert!(compute_subtitle_overlay_visibility(true, true, true, false, true, true));
    assert!(compute_subtitle_overlay_visibility(true, true, false, true, true, true));
    // Outside the zone with cursor-show enabled -> hidden.
    assert!(!compute_subtitle_overlay_visibility(true, true, false, false, true, false));
    // No subtitle text -> never visible even in zone.
    assert!(!compute_subtitle_overlay_visibility(false, true, false, false, true, true));
    // Normal mode (cursor-show disabled) respects `subtitles_visible` and `hide_while_playing`.
    assert!(compute_subtitle_overlay_visibility(true, true, false, true, false, false));
    assert!(!compute_subtitle_overlay_visibility(true, false, false, false, false, true));
    assert!(!compute_subtitle_overlay_visibility(true, true, true, true, false, true));
    assert!(compute_subtitle_overlay_visibility(true, true, true, false, false, false));
}

#[test]
fn mpv_cursor_autohide_policy_respects_never_always_timeout_fs_only_and_interactive_overlay() {
    use memento_app::controller::player::mpv_cursor_should_hide;
    use memento_player::mpv::CursorAutohide;

    assert!(!mpv_cursor_should_hide(CursorAutohide::Never, false, true, 10_000, false));
    assert!(mpv_cursor_should_hide(CursorAutohide::Always, false, false, 0, false));
    assert!(!mpv_cursor_should_hide(CursorAutohide::Always, true, false, 0, false));
    assert!(mpv_cursor_should_hide(CursorAutohide::Always, true, true, 0, false));
    assert!(!mpv_cursor_should_hide(CursorAutohide::Always, false, true, 0, true));

    assert!(!mpv_cursor_should_hide(CursorAutohide::Number(1000), false, false, 500, false));
    assert!(mpv_cursor_should_hide(CursorAutohide::Number(1000), false, false, 1000, false));
    assert!(!mpv_cursor_should_hide(CursorAutohide::Number(1000), true, false, 2000, false));
    assert!(mpv_cursor_should_hide(CursorAutohide::Number(1000), true, true, 1200, false));
}

#[test]
fn filter_subtitle_text_and_refresh_subtitle_list_respect_regex_newlines_and_ignore_whitespace() {
    use memento_app::controller::player::{filter_subtitle_text, refresh_subtitle_list};
    use memento_app::{MementoApp, SubEntry};
    use slint::{Model, VecModel};

    assert_eq!(
        filter_subtitle_text("（笑）おはよう\n世界", "（[^）]*）", true, " "),
        "おはよう 世界"
    );
    assert_eq!(
        filter_subtitle_text("行1\n行2", "[invalid", true, ""),
        "行1行2"
    );

    i_slint_backend_testing::init_no_event_loop();
    let app = MementoApp::new().unwrap();
    let mut subs = SubtitleController::new();
    subs.load_cues(vec![
        create_cue("（SE）Alpha one", 1.0, 2.0),
        create_cue("Beta two", 3.0, 4.0),
    ]);
    let model = VecModel::<SubEntry>::default();

    // `remove-regex` strips matched text in displayed rows, and `ignore-whitespace = false`
    // does NOT match "Alphaone" against "Alpha one".
    app.set_search_remove_regex("（[^）]*）".into());
    app.set_subtitle_search_query("Alphaone".into());
    app.set_subtitle_search_ignore_whitespace(false);
    refresh_subtitle_list(&app, &model, &subs);
    assert_eq!(model.row_data(0).unwrap().text.as_str(), "Alpha one");
    assert!(!model.row_data(0).unwrap().search_match);

    // Enabling `ignore-whitespace = true` matches "Alphaone" against "Alpha one".
    app.set_subtitle_search_ignore_whitespace(true);
    refresh_subtitle_list(&app, &model, &subs);
    assert!(model.row_data(0).unwrap().search_match);
    assert!(!model.row_data(1).unwrap().search_match);
}

#[test]
fn config_single_setting_persistence_preserves_unapplied_drafts_and_status_descriptions() {
    use memento_app::controller::config::ConfigController;
    use memento_config::Value;
    use memento_setting::keys as raw;
    use slint::Model;

    let dir = tempfile::TempDir::new().unwrap();
    let cfg = ConfigController::new();

    // Make an unapplied draft edit in Behavior category.
    cfg.edit_setting("behavior/osc-fade", "900").unwrap();

    // Persist individual live toggles via `persist_single_setting`.
    cfg.persist_single_setting("interface/sub-list-timestamps", "true", dir.path(), "memento.conf")
        .unwrap();
    cfg.persist_single_setting("subtitle-list/ignore-whitespace", "true", dir.path(), "memento.conf")
        .unwrap();
    cfg.persist_single_setting("subtitle-list/auto-seek", "true", dir.path(), "memento.conf")
        .unwrap();
    cfg.persist_single_setting("internal/auto-update-opt-in-shown", "true", dir.path(), "memento.conf")
        .unwrap();
    cfg.persist_single_setting("application/auto-update-check", "true", dir.path(), "memento.conf")
        .unwrap();

    // Reloading from disk must have persisted all single settings WITHOUT committing the draft osc-fade.
    let reloaded = ConfigController::load(dir.path(), "memento.conf").unwrap();
    assert_eq!(
        reloaded.effective_setting(raw::interface::GROUP, raw::interface::subtitle_list::TIMESTAMPS),
        Some(Value::Bool(true))
    );
    assert_eq!(
        reloaded.effective_setting(raw::subtitle_list::GROUP, raw::subtitle_list::IGNORE_WHITESPACE),
        Some(Value::Bool(true))
    );
    assert_eq!(
        reloaded.effective_setting(raw::subtitle_list::GROUP, raw::subtitle_list::AUTO_SEEK),
        Some(Value::Bool(true))
    );
    assert_eq!(
        reloaded.effective_setting(raw::internal::GROUP, raw::internal::AUTO_UPDATE_OPT_IN_SHOWN),
        Some(Value::Bool(true))
    );
    assert_eq!(
        reloaded.effective_setting(raw::application::GROUP, raw::application::AUTO_UPDATE_CHECK),
        Some(Value::Bool(true))
    );
    assert_eq!(
        reloaded.effective_setting(raw::behavior::GROUP, raw::behavior::OSC_FADE),
        Some(Value::Int(raw::behavior::OSC_FADE_DEFAULT as i64))
    );

    // Verify `application/language` and `interface/theme` round-trip through category apply,
    // `application/language` exposes an honest description, and hidden dead switch
    // `interface/system-icons` is omitted from Settings UI groups while loading cleanly.
    reloaded.edit_setting("application/language", "한국어 (MTL)").unwrap();
    reloaded.edit_setting("interface/theme", "light").unwrap();
    let anki_path = dir.path().join("anki_connect.json");
    reloaded.apply_settings(1, dir.path(), "memento.conf", &anki_path).unwrap();
    reloaded.apply_settings(5, dir.path(), "memento.conf", &anki_path).unwrap();
    let reloaded2 = ConfigController::load(dir.path(), "memento.conf").unwrap();
    assert_eq!(
        reloaded2.effective_setting(raw::application::GROUP, raw::application::LANGUAGE),
        Some(Value::Int(2))
    );
    assert_eq!(
        reloaded2.effective_setting(raw::interface::GROUP, raw::interface::THEME),
        Some(Value::Text("light".to_owned()))
    );

    let groups = reloaded2.settings_groups();
    let find_field = |key: &str| {
        groups.iter().find_map(|g| {
            (0..g.fields.row_count())
                .filter_map(|i| g.fields.row_data(i))
                .find(|f| f.key.as_str() == key)
        })
    };
    let lang_field = find_field("application/language").expect("application/language field");
    assert!(
        lang_field.description.as_str().contains("English"),
        "language setting must honestly explain available UI language: {:?}",
        lang_field.description
    );
    assert!(
        find_field("interface/system-icons").is_none(),
        "dead switch interface/system-icons must be hidden from Settings UI groups"
    );
}

#[test]
fn embedded_mkv_ass_track_never_inserts_combined_duplicate_and_highlights_simultaneous_rows() {
    use memento_app::controller::anki::{apply_subtitle_cloze, NoteFieldsContext};
    use memento_app::controller::player::{refresh_active_rows, refresh_subtitle_list};
    use memento_app::{MementoApp, SubEntry};
    use slint::{Model, VecModel};

    let mut ctrl = SubtitleController::new();
    ctrl.sync_tracks(&[SubtitleTrackInfo {
        id: 1,
        external: false,
        external_path: String::new(),
    }]);
    ctrl.set_primary_sid(1);
    assert!(ctrl.set_track_cues(
        1,
        vec![
            create_cue("いいわけあるか〜っ！！", 1021.99, 1024.16),
            create_cue("うわっ！", 1021.99, 1024.16),
        ],
    ));
    let track = ctrl.track(1).expect("embedded ASS track");
    assert!(!track.is_external());
    assert!(track.is_loaded_complete());
    assert!(!track.is_memory_track());
    assert_eq!(ctrl.primary_entries().len(), 2);

    // During playback mpv emits combined `sub-text` for overlapping ASS events.
    let combined = create_cue("いいわけあるか〜っ！！\nうわっ！", 1021.99, 1024.16);
    assert!(ctrl.update_active_cue(1, Some(&combined), 1022.5));
    assert_eq!(
        ctrl.primary_entries().len(),
        2,
        "upfront-extracted embedded ASS track must NOT insert mpv's combined sub-text as a 3rd row"
    );
    assert_eq!(ctrl.active_indices(1), &[0, 1]);
    assert!(!ctrl.track(1).unwrap().is_memory_track());

    // Transient `sub-lines` updates must NOT overwrite or duplicate a `loaded_complete` track.
    let mem_changed = ctrl.update_memory_cues(
        1,
        &[MemorySubtitleCue {
            id: 1,
            text: "いいわけあるか〜っ！！\nうわっ！".encode_utf16().collect(),
            start: 1021.99,
            end: 1024.16,
        }],
        1022.5,
    );
    assert!(!mem_changed);
    assert_eq!(ctrl.primary_entries().len(), 2);
    assert_eq!(ctrl.active_indices(1), &[0, 1]);
    assert!(!ctrl.track(1).unwrap().is_memory_track());

    // UI model highlights BOTH simultaneous rows cleanly with zero 3rd duplicate row.
    i_slint_backend_testing::init_no_event_loop();
    let app = MementoApp::new().unwrap();
    let model = VecModel::<SubEntry>::default();
    refresh_subtitle_list(&app, &model, &ctrl);
    assert_eq!(model.row_count(), 2);
    assert!(model.row_data(0).unwrap().is_active);
    assert!(model.row_data(1).unwrap().is_active);
    assert_eq!(model.row_data(0).unwrap().text.as_str(), "いいわけあるか〜っ！！");
    assert_eq!(model.row_data(1).unwrap().text.as_str(), "うわっ！");

    // Advancing past the cue clears both active highlights cleanly.
    ctrl.update_position(1025.0);
    refresh_active_rows(&model, &[0, 1], ctrl.active_indices(1));
    assert!(!model.row_data(0).unwrap().is_active);
    assert!(!model.row_data(1).unwrap().is_active);

    // Multi-row Anki context selection contains each line once with accurate time range.
    ctrl.select_row(0, false);
    ctrl.select_row(1, true);
    assert_eq!(ctrl.selected_indices(), &[0, 1]);
    assert_eq!(ctrl.selected_time_range(), Some((1021.99, 1024.16)));
    let selected_texts: Vec<String> = ctrl
        .selected_cues()
        .iter()
        .map(|cue| String::from_utf16_lossy(&cue.text))
        .collect();
    assert_eq!(selected_texts, vec!["いいわけあるか〜っ！！", "うわっ！"]);
    let mut ctx = NoteFieldsContext::default();
    ctx.sentence = "いいわけあるか〜っ！！".to_owned();
    ctx.expression = "いいわけ".to_owned();
    apply_subtitle_cloze(&mut ctx, &selected_texts, Some(0), "いいわけ", Some(0));
    assert_eq!(ctx.context, "いいわけあるか〜っ！！\nうわっ！");
    assert_eq!(ctx.cloze_body, "いいわけ");
}

#[test]
fn embedded_mkv_srt_track_normalizes_blank_lines_crlf_and_never_duplicates() {
    let mut ctrl = SubtitleController::new();
    ctrl.sync_tracks(&[SubtitleTrackInfo {
        id: 2,
        external: false,
        external_path: String::new(),
    }]);
    ctrl.set_primary_sid(2);
    assert!(ctrl.set_track_cues(
        2,
        vec![
            create_cue("あっ。\n\n勝った勝った！", 926.526, 929.028),
            // Exact duplicate packet (CRLF variant within 0.01s) must also deduplicate.
            create_cue("あっ。\r\n勝った勝った！\r\n", 926.530, 929.030),
        ],
    ));
    assert_eq!(ctrl.primary_entries().len(), 1);
    assert_eq!(
        ctrl.cue_text(0).as_deref(),
        Some("あっ。\n勝った勝った！"),
        "SRT cue text must normalize CRLF and collapse blank gap lines"
    );
    assert!(ctrl.track(2).unwrap().is_loaded_complete());
    assert!(!ctrl.track(2).unwrap().is_memory_track());

    // Playback update with mpv's normalized `sub-text` must NOT append a duplicate row.
    let live = create_cue("あっ。\n勝った勝った！", 926.526, 929.028);
    assert!(ctrl.update_active_cue(2, Some(&live), 927.0));
    assert_eq!(ctrl.primary_entries().len(), 1);
    assert_eq!(ctrl.active_indices(2), &[0]);
    assert!(!ctrl.track(2).unwrap().is_memory_track());

    // Transient `sub-lines` updates also must NOT duplicate or overwrite the track.
    assert!(!ctrl.update_memory_cues(
        2,
        &[MemorySubtitleCue {
            id: 1,
            text: "あっ。\n勝った勝った！".encode_utf16().collect(),
            start: 926.526,
            end: 929.028,
        }],
        927.0,
    ));
    assert_eq!(ctrl.primary_entries().len(), 1);
    assert_eq!(ctrl.active_indices(2), &[0]);
}

#[test]
fn live_memory_track_accumulation_still_works_when_not_loaded_complete_and_resets_cleanly() {
    let mut ctrl = SubtitleController::new();
    ctrl.sync_tracks(&[SubtitleTrackInfo {
        id: 1,
        external: false,
        external_path: String::new(),
    }]);
    ctrl.set_primary_sid(1);
    assert!(!ctrl.track(1).unwrap().is_loaded_complete());

    // Streaming / non-MKV embedded tracks still accumulate live `sub-text` cues.
    assert!(ctrl.update_active_cue(1, Some(&create_cue("First live line", 1.0, 2.5)), 1.5));
    assert!(ctrl.update_active_cue(
        1,
        Some(&create_cue("Second\r\n\r\nlive line", 3.0, 4.5)),
        3.5
    ));
    // Re-emitting normalized form at the same timestamp must NOT duplicate.
    assert!(ctrl.update_active_cue(
        1,
        Some(&create_cue("Second\nlive line", 3.0, 4.5)),
        3.8
    ));
    assert_eq!(ctrl.primary_entries().len(), 2);
    assert_eq!(ctrl.cue_text(0).as_deref(), Some("First live line"));
    assert_eq!(ctrl.cue_text(1).as_deref(), Some("Second\nlive line"));
    assert!(ctrl.track(1).unwrap().is_memory_track());
    assert_eq!(ctrl.active_indices(1), &[1]);

    // `update_memory_cues` continues working while `loaded_complete == false`.
    assert!(ctrl.update_memory_cues(
        1,
        &[
            MemorySubtitleCue {
                id: 10,
                text: "Mem A".encode_utf16().collect(),
                start: 1.0,
                end: 2.0,
            },
            MemorySubtitleCue {
                id: 11,
                text: "Mem B".encode_utf16().collect(),
                start: 3.0,
                end: 4.0,
            },
        ],
        1.5,
    ));
    assert_eq!(ctrl.primary_entries().len(), 2);
    assert_eq!(ctrl.cue_text(0).as_deref(), Some("Mem A"));

    // Once upfront extraction finishes (`set_track_cues`), it replaces live cues and locks out transient inserts.
    assert!(ctrl.set_track_cues(1, vec![create_cue("Extracted full track", 1.0, 4.0)]));
    assert!(ctrl.track(1).unwrap().is_loaded_complete());
    assert!(!ctrl.track(1).unwrap().is_memory_track());
    assert!(ctrl.update_active_cue(1, Some(&create_cue("Should not insert", 1.0, 4.0)), 2.0));
    assert_eq!(ctrl.primary_entries().len(), 1);

    // Removing the track or clearing the controller resets `loaded_complete` cleanly.
    ctrl.sync_tracks(&[]);
    assert!(ctrl.track(1).is_none());
    ctrl.sync_tracks(&[SubtitleTrackInfo {
        id: 1,
        external: false,
        external_path: String::new(),
    }]);
    assert!(!ctrl.track(1).unwrap().is_loaded_complete());

    // Reloading an external track resets `loaded_complete` until `set_external_cues` completes,
    // and switching back to embedded via `ClearExternal` also resets `loaded_complete`.
    ctrl.sync_tracks(&[SubtitleTrackInfo {
        id: 1,
        external: true,
        external_path: "/tmp/sub.srt".to_owned(),
    }]);
    assert!(!ctrl.track(1).unwrap().is_loaded_complete());
    assert!(ctrl.set_external_cues(1, "/tmp/sub.srt", vec![create_cue("Ext cue", 1.0, 2.0)]));
    assert!(ctrl.track(1).unwrap().is_loaded_complete());
    ctrl.sync_tracks(&[SubtitleTrackInfo {
        id: 1,
        external: false,
        external_path: String::new(),
    }]);
    assert!(!ctrl.track(1).unwrap().is_loaded_complete());

    // Secondary embedded track populated via `set_track_cues` also ignores transient inserts.
    ctrl.sync_tracks(&[
        SubtitleTrackInfo { id: 1, external: false, external_path: String::new() },
        SubtitleTrackInfo { id: 2, external: false, external_path: String::new() },
    ]);
    ctrl.set_secondary_sid(2);
    assert!(ctrl.set_track_cues(2, vec![create_cue("Secondary extracted", 5.0, 7.0)]));
    assert!(ctrl.update_active_cue(2, Some(&create_cue("Transient secondary", 5.0, 7.0)), 6.0));
    assert_eq!(ctrl.secondary_entries().len(), 1);
    assert_eq!(ctrl.active_indices(2), &[0]);

    ctrl.clear();
    assert!(ctrl.tracks().is_empty());
}

#[test]
fn filter_and_sort_cues_normalizes_line_breaks_and_deduplicates_exact_matches_within_10ms() {
    let cues = vec![
        // CRLF + leading/trailing blank lines + exact duplicate within 0.01s at large timestamp.
        create_cue("\r\n\nLine A\r\n\r\nLine B \r\n", 100.00, 102.00),
        create_cue("Line A\nLine B", 100.01, 102.01),
        // Same text at a later timestamp (> 0.01s away) must NOT be deduplicated.
        create_cue("Line A\nLine B", 105.00, 107.00),
        // Distinct simultaneous ASS cues at identical timestamps must BOTH survive in order.
        create_cue("いいわけあるか〜っ！！", 50.00, 52.00),
        create_cue("うわっ！", 50.00, 52.00),
        // Whitespace-only cue must be discarded.
        create_cue("  \r\n\t\n  ", 10.00, 12.00),
    ];

    let filtered = filter_and_sort_cues(&cues);
    let texts: Vec<String> = filtered
        .iter()
        .map(|c| String::from_utf16_lossy(&c.text))
        .collect();
    assert_eq!(
        texts,
        vec![
            "いいわけあるか〜っ！！",
            "うわっ！",
            "Line A\nLine B",
            "Line A\nLine B",
        ]
    );
    assert_eq!(filtered[0].start, 50.0);
    assert_eq!(filtered[1].start, 50.0);
    assert_eq!(filtered[2].start, 100.0);
    assert_eq!(filtered[3].start, 105.0);
}

#[test]
fn summer_pockets_ep01_live_mpv_ass_and_srt_zero_duplicates_and_clean_newlines() {
    use std::cell::{Cell, RefCell};
    use std::path::Path;
    use std::time::{Duration, Instant};

    use memento_app::controller::asr::AsrController;
    use memento_app::controller::player::{refresh_subtitle_list, LivePlayerCoordinator};
    use memento_app::{MementoApp, SubEntry};
    use memento_player::{CliOption, MpvInstance};
    use slint::{Model as _, VecModel};

    let mkv_path = Path::new(
        "/home/eyalm/Desktop/japanese/anime/[NanakoRaws] Summer Pockets (AT-X 1920x1080 x265 AAC)/[NanakoRaws] Summer Pockets - 01 (AT-X 1920x1080 x265 AAC).mkv",
    );
    if !mkv_path.is_file() {
        return;
    }

    // Extract both embedded tracks with memento-ass exactly like src/main.rs does.
    let ass_extracted =
        memento_ass::extract_mkv_subtitle_track(mkv_path, 3).expect("extract ep01 ASS track");
    let srt_extracted =
        memento_ass::extract_mkv_subtitle_track(mkv_path, 4).expect("extract ep01 SRT track");
    assert_eq!(ass_extracted.cues.len(), 430);
    assert_eq!(srt_extracted.cues.len(), 297);
    assert!(
        srt_extracted
            .cues
            .iter()
            .all(|c| !c.plain_text.contains('\r') && !c.plain_text.contains("\n\n")),
        "extracted MKV SRT cues must not contain CR or double blank lines"
    );
    assert!(
        srt_extracted
            .cues
            .iter()
            .any(|c| c.plain_text == "あっ。\n勝った勝った！"),
        "ep01 SRT cue 197 must normalize cleanly to 'あっ。\\n勝った勝った！'"
    );

    let to_entries = |cues: &[memento_ass::MkvSubtitleCue]| {
        cues.iter()
            .filter_map(|c| {
                if c.plain_text.trim().is_empty() || c.end_ms <= c.start_ms {
                    return None;
                }
                Some(create_cue(
                    &c.plain_text,
                    c.start_ms as f64 / 1000.0,
                    c.end_ms as f64 / 1000.0,
                ))
            })
            .collect::<Vec<_>>()
    };

    i_slint_backend_testing::init_no_event_loop();
    let app = MementoApp::new().unwrap();
    let temp = tempfile::TempDir::new().unwrap();
    std::fs::write(temp.path().join("input.conf"), "").unwrap();
    std::fs::write(
        temp.path().join("mpv.conf"),
        "ao=null\nvo=libmpv\nhwdec=no\nmsg-level=all=error\n",
    )
    .unwrap();
    let player = MpvInstance::new_with_options(
        Some(temp.path()),
        &[CliOption {
            name: "sub-font-size".into(),
            value: "36".into(),
        }],
    )
    .expect("headless MpvInstance");
    let mut coord = LivePlayerCoordinator::new(&player);
    let mut subtitles = SubtitleController::new();
    let sub_model = VecModel::<SubEntry>::default();
    let mut asr = AsrController::new();
    let media_gen = Cell::new(1u64);
    let current_media = RefCell::new(None);
    let asr_restart = Cell::new(false);
    let hover_owned = Cell::new(false);

    coord.mark_explicit_media_load(1);
    player.pause().unwrap();
    player.load_file(&mkv_path.to_string_lossy()).unwrap();

    let mut pump = |subtitles: &mut SubtitleController,
                    timeout: Duration,
                    mut pred: Box<dyn FnMut(&LivePlayerCoordinator, &MementoApp, &SubtitleController) -> bool>|
     -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            let _ = coord.sync(
                &app,
                &player,
                subtitles,
                &sub_model,
                &mut asr,
                &media_gen,
                &current_media,
                &asr_restart,
                &hover_owned,
                || Err("no settings".into()),
                |app, text| app.set_active_sub_text(text.into()),
            );
            if pred(&coord, &app, subtitles) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(12));
        }
        false
    };

    assert!(pump(
        &mut subtitles,
        Duration::from_secs(6),
        Box::new(|c, _, _| c.playback_state().file_loaded && c.playback_state().subtitle_tracks.len() >= 2),
    ));

    // Populate both embedded tracks via `set_track_cues` just like MKV_EXTRACT_SINK.
    subtitles.ensure_track(1);
    subtitles.set_track_cues(1, to_entries(&ass_extracted.cues));
    let ass_expected_len = subtitles.track(1).unwrap().entries().len();
    subtitles.ensure_track(2);
    subtitles.set_track_cues(2, to_entries(&srt_extracted.cues));
    let srt_expected_len = subtitles.track(2).unwrap().entries().len();
    assert_eq!(srt_expected_len, 297);
    refresh_subtitle_list(&app, &sub_model, &subtitles);

    // 1. Seek into the overlapping ASS scene (`1021.99..1024.16`: いいわけあるか～っ！！ + うわっ！).
    player.set_sid(1).unwrap();
    player.seek(1022.5).unwrap();
    assert!(
        pump(
            &mut subtitles,
            Duration::from_secs(6),
            Box::new(|c, app, _| c.playback_state().primary_sid == 1
                && app.get_active_sub_text().as_str().contains("いいわけあるか")),
        ),
        "libmpv did not emit ASS sub-text at 1022.5s (got {:?})",
        app.get_active_sub_text()
    );
    assert_eq!(
        subtitles.primary_entries().len(),
        ass_expected_len,
        "ASS track entry count must not grow when mpv emits combined sub-text"
    );
    assert_eq!(sub_model.row_count(), ass_expected_len);
    let active_ass_texts: Vec<String> = (0..sub_model.row_count())
        .filter_map(|i| sub_model.row_data(i))
        .filter(|r| r.is_active)
        .map(|r| r.text.to_string())
        .collect();
    assert_eq!(
        active_ass_texts,
        vec!["いいわけあるか～っ！！", "うわっ！"],
        "both overlapping ASS rows must highlight with ZERO 3rd combined duplicate row"
    );

    // 2. Switch to embedded SRT track (`sid = 2`) and seek to `927.0s` (あっ。\n勝った勝った！).
    player.set_sid(2).unwrap();
    player.seek(927.0).unwrap();
    assert!(
        pump(
            &mut subtitles,
            Duration::from_secs(6),
            Box::new(|c, app, _| c.playback_state().primary_sid == 2
                && app.get_active_sub_text().as_str().contains("勝った勝った")),
        ),
        "libmpv did not emit SRT sub-text at 927.0s (got {:?})",
        app.get_active_sub_text()
    );
    assert_eq!(
        subtitles.primary_entries().len(),
        srt_expected_len,
        "SRT track entry count must stay 297 with ZERO duplicates during playback"
    );
    assert_eq!(sub_model.row_count(), srt_expected_len);
    let active_srt_texts: Vec<String> = (0..sub_model.row_count())
        .filter_map(|i| sub_model.row_data(i))
        .filter(|r| r.is_active)
        .map(|r| r.text.to_string())
        .collect();
    assert_eq!(active_srt_texts, vec!["あっ。\n勝った勝った！"]);
    assert!(
        (0..sub_model.row_count())
            .filter_map(|i| sub_model.row_data(i))
            .all(|r| !r.text.as_str().contains('\r') && !r.text.as_str().contains("\n\n")),
        "SRT list rows must have zero CR or blank gap lines"
    );
}

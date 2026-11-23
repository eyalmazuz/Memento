use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use subomi_ass::{
    build_probe_script, extract_mkv_fonts, extract_mkv_subtitle_track,
    is_japanese_or_alnum_lookup_char, libass_version, list_mkv_subtitle_tracks,
    parse_ass_timecode_ms, parse_script_info, AssHighlightRect, AssLayoutEngine, MkvSubtitleCodec,
    MpvAssParams, ViewportTransform,
};

const SUMMER_POCKETS_MKV: &str = "/home/eyalm/Desktop/japanese/anime/[NanakoRaws] Summer Pockets (AT-X 1920x1080 x265 AAC)/[NanakoRaws] Summer Pockets - 01 (AT-X 1920x1080 x265 AAC).mkv";
const VIOLET_EVERGARDEN_DIR: &str = "/home/eyalm/Desktop/japanese/anime/[Erai-raws] Violet Evergarden - 01 ~ 14 (Plus Specials) [1080p][Multiple Subtitle]";
const MPV_REF_DIR: &str = "/home/eyalm/.gemini/antigravity/brain/bc59aa33-0990-4b77-b532-788d1afb2be9/scratch/ass-research/mpv-ref";

const SYNTHETIC_HEADER: &str = "\
[Script Info]
ScriptType: v4.00+
PlayResX: 1280
PlayResY: 720
ScaledBorderAndShadow: yes
Collisions: Normal
WrapStyle: 0

[V4+ Styles]
Format: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding
Style: Default,Noto Sans CJK JP,48,&H00FFFFFF,&H000000FF,&H00000000,&H64000000,0,0,0,0,100,100,2,0,1,2,2,2,40,40,30,1
Style: TopSign,Noto Sans CJK JP,40,&H0000FFFF,&H000000FF,&H00000000,&H64000000,1,0,0,0,100,100,0,0,1,2,1,8,40,40,30,1
";

#[test]
fn libass_runtime_version_is_valid() {
    let ver = libass_version();
    // 0.14.0 = 0x01400000
    assert!(ver >= 0x0140_0000, "unexpected libass version: 0x{ver:08x}");
}

#[test]
fn parser_and_engine_handle_complex_ass_tags_layers_and_wrapping() {
    let mut engine = AssLayoutEngine::new().expect("engine init");
    engine.configure(&MpvAssParams::for_frame(1280, 720));

    // Layer 0 and Layer 5 overlap at the exact same \pos(640,360): Layer 5 must win hit_test!
    // Also includes: \move, alpha-only \t (not animated geometry), geometry \t, \clip,
    // \frz rotation, \N explicit newline, English spacing, and \p1 drawing skip.
    let ass_full = "\
Dialogue: 0,0:00:05.00,0:00:10.00,Default,,0,0,0,,{\\pos(640,360)\\an5\\t(0,500,\\alpha&H00&)}下レイヤー\n\
Dialogue: 5,0:00:05.00,0:00:10.00,Default,,0,0,0,,{\\pos(640,360)\\an5\\fad(100,100)}上レイヤー\n\
Dialogue: 0,0:00:05.00,0:00:10.00,TopSign,,0,0,0,,{\\move(200,80,400,80,0,5000)}Summer Pockets\\N夏休み\n\
Dialogue: 0,0:00:05.00,0:00:10.00,Default,,0,0,0,,{\\pos(150,600)\\frz-12\\t(0,1000,\\fscx120)}看板テキスト\n\
Dialogue: 0,0:00:05.00,0:00:10.00,Default,,0,0,0,,{\\p1\\pos(0,0)}m 0 0 l 200 0 200 50 0 50{\\p0}\n";

    let layout = engine
        .probe_active_events(SYNTHETIC_HEADER, ass_full, 7_500)
        .expect("probe");

    assert_eq!(layout.events.len(), 5);
    assert!(!layout.events[0].has_animated_geometry, "alpha-only \\t must not flag animated geometry");
    assert!(layout.events[2].has_animated_geometry, "\\move must flag animated geometry");
    assert!(layout.events[3].has_animated_geometry, "\\t(\\fscx) must flag animated geometry");
    assert!(layout.events[3].is_rotated_or_sign, "\\frz-12 must flag rotated/sign");
    assert!(layout.events[4].is_drawing_only);
    assert!(layout.has_animated_geometry);

    // Hit-test the overlapping center of event 0 / event 1 -> Layer 5 ("上レイヤー") MUST win!
    let ev1_b = layout.events[1].bounds.expect("ev1 bounds");
    let hit = layout
        .hit_test(ev1_b.x + 12.0, ev1_b.y + ev1_b.h * 0.5)
        .expect("hit on overlapping layers");
    assert_eq!(hit.event_index, 1, "higher Layer event must take hit-test precedence");
    assert_eq!(hit.ch, '上');
    assert_eq!(hit.scan_substring, "上レイヤー");

    // Verify 2 visual lines on event 2 ("Summer Pockets\n夏休み") & whitespace interpolation!
    let ev2 = &layout.events[2];
    assert_eq!(ev2.plain_text, "Summer Pockets\n夏休み");
    assert_eq!(ev2.lines.len(), 2);
    let space_char = ev2.chars.iter().find(|c| c.ch == ' ').expect("space char box");
    assert!(space_char.is_whitespace && !space_char.has_ink);
    assert!(space_char.w > 2.0, "interpolated space width should be > 2px, got {}", space_char.w);

    // Highlight spanning across the \N line break produces 2 clean highlight rectangles.
    let multi_line_rects = layout.highlight_rects_for_byte_span(2, 7, ev2.plain_text.len());
    assert_eq!(multi_line_rects.len(), 2);

    // Test ViewportTransform logical hit-testing at 0.5x scale with pillarbox offset.
    let vt = ViewportTransform::contain(840.0, 360.0, 1280, 720);
    let (lx, ly) = vt.frame_to_logical(ev1_b.x + 12.0, ev1_b.y + ev1_b.h * 0.5);
    let logical_hit = layout
        .hit_test_logical(&vt, lx, ly)
        .expect("logical hit-test");
    assert_eq!(logical_hit.event_index, 1);
    assert_eq!(logical_hit.ch, '上');
}

#[test]
fn probe_handles_large_text_beyond_512_char_cap_safely() {
    let mut engine = AssLayoutEngine::new().expect("engine init");
    engine.configure(&MpvAssParams::for_frame(1280, 720));

    let long_text: String = "あ".repeat(600);
    let ass_full = format!("Dialogue: 0,0:00:01.00,0:00:05.00,Default,,0,0,0,,{long_text}\n");
    let probe = build_probe_script(SYNTHETIC_HEADER, &ass_full, 2_000);
    assert_eq!(probe.total_probed_chars, 512);
    assert_eq!(probe.events[0].chars.len(), 600);

    let layout = engine
        .probe_active_events(SYNTHETIC_HEADER, &ass_full, 2_000)
        .expect("probe large text");
    assert_eq!(layout.events[0].chars.len(), 600);
    assert!(!layout.events[0].lines.is_empty());
}

#[test]
fn lookup_char_classifier_distinguishes_jp_alnum_from_symbols() {
    assert!(is_japanese_or_alnum_lookup_char('鳥'));
    assert!(is_japanese_or_alnum_lookup_char('あ'));
    assert!(is_japanese_or_alnum_lookup_char('ア'));
    assert!(is_japanese_or_alnum_lookup_char('ー'));
    assert!(is_japanese_or_alnum_lookup_char('々'));
    assert!(is_japanese_or_alnum_lookup_char('A'));
    assert!(!is_japanese_or_alnum_lookup_char('🔊'));
    assert!(!is_japanese_or_alnum_lookup_char('➡'));
    assert!(!is_japanese_or_alnum_lookup_char('⚟'));
    assert!(!is_japanese_or_alnum_lookup_char('。'));
}

#[test]
fn synthetic_mkv_ebml_extracts_fonts_tracks_and_zlib_cues() {
    use flate2::{write::ZlibEncoder, Compression};
    use std::io::Write;

    let tmp_path = std::env::temp_dir().join(format!(
        "subomi_ass_synth_{}.mkv",
        std::process::id()
    ));

    let mkv_bytes = build_synthetic_mkv(|raw_block| {
        let mut enc = ZlibEncoder::new(Vec::new(), Compression::default());
        enc.write_all(raw_block).unwrap();
        enc.finish().unwrap()
    });
    fs::write(&tmp_path, &mkv_bytes).expect("write synth mkv");

    let fonts = extract_mkv_fonts(&tmp_path).expect("extract fonts");
    assert_eq!(fonts.len(), 1);
    assert_eq!(fonts[0].file_name, "TestFont.ttf");
    assert_eq!(fonts[0].data, b"FAKE_TTF_BYTES_12345");

    let tracks = list_mkv_subtitle_tracks(&tmp_path).expect("list tracks");
    assert_eq!(tracks.len(), 1);
    assert_eq!(tracks[0].track_number, 3);
    assert_eq!(tracks[0].codec, MkvSubtitleCodec::Ass);
    assert_eq!(tracks[0].name.as_deref(), Some("Japanese ASS"));
    assert_eq!(tracks[0].language.as_deref(), Some("jpn"));

    let extracted = extract_mkv_subtitle_track(&tmp_path, 3).expect("extract track 3");
    assert_eq!(extracted.cues.len(), 2);
    assert_eq!(extracted.cues[0].start_ms, 10_000);
    assert_eq!(extracted.cues[0].end_ms, 13_500);
    assert_eq!(extracted.cues[0].plain_text, "鳥白島に到着");
    assert_eq!(extracted.cues[1].start_ms, 20_500);
    assert_eq!(extracted.cues[1].end_ms, 22_000);
    assert_eq!(extracted.cues[1].plain_text, "二行目のテスト");

    let doc = extracted.to_ass_document();
    assert!(doc.contains("[Script Info]"));
    assert!(doc.contains("Dialogue: 0,0:00:10.00,0:00:13.50,Default,,0,0,0,,{\\pos(480,502)}鳥白島に到着"));

    let _ = fs::remove_file(&tmp_path);
}

#[test]
fn summer_pockets_all_8_timestamps_hitboxes_and_benchmarks() {
    let mkv_path = Path::new(SUMMER_POCKETS_MKV);
    if !mkv_path.is_file() {
        eprintln!(
            "[SKIP] Summer Pockets MKV not found at {}",
            mkv_path.display()
        );
        return;
    }

    // 1. Verify fast EBML font + track metadata + full ASS/SRT track extraction.
    let t_fonts = Instant::now();
    let fonts = extract_mkv_fonts(mkv_path).expect("extract_mkv_fonts");
    let fonts_ms = t_fonts.elapsed().as_secs_f64() * 1000.0;
    assert_eq!(fonts.len(), 0, "Summer Pockets ep01 has 0 embedded fonts");

    let t_list = Instant::now();
    let tracks = list_mkv_subtitle_tracks(mkv_path).expect("list_mkv_subtitle_tracks");
    let list_ms = t_list.elapsed().as_secs_f64() * 1000.0;
    assert_eq!(tracks.len(), 2, "Summer Pockets ep01 should have ASS + SRT tracks");
    assert_eq!(tracks[0].track_number, 3);
    assert_eq!(tracks[0].codec, MkvSubtitleCodec::Ass);
    assert_eq!(tracks[1].track_number, 4);
    assert_eq!(tracks[1].codec, MkvSubtitleCodec::Srt);

    let t_ass = Instant::now();
    let ass_track = extract_mkv_subtitle_track(mkv_path, 3).expect("extract ASS track 3");
    let ass_ms = t_ass.elapsed().as_secs_f64() * 1000.0;
    assert_eq!(ass_track.cues.len(), 430, "expected 430 ASS cues in ep01");

    let t_srt = Instant::now();
    let srt_track = extract_mkv_subtitle_track(mkv_path, 4).expect("extract SRT track 4");
    let srt_ms = t_srt.elapsed().as_secs_f64() * 1000.0;
    assert_eq!(srt_track.cues.len(), 297, "expected 297 SRT cues in ep01");

    let ass_doc = ass_track.to_ass_document();
    let info = parse_script_info(&ass_doc);
    assert_eq!(info.play_res_x, Some(960));
    assert_eq!(info.play_res_y, Some(540));

    eprintln!(
        "[BENCH] Summer Pockets MKV EBML: fonts={fonts_ms:.2}ms (0), list={list_ms:.2}ms (2 tracks), \
         extract_ass={ass_ms:.2}ms (430 cues), extract_srt={srt_ms:.2}ms (297 cues)"
    );

    // 2. Probe ALL 8 verification timestamps and verify hit-testing + highlight patches.
    let mut engine = AssLayoutEngine::new().expect("libass init");
    engine.configure(&MpvAssParams::for_frame(1920, 1080));

    // (timestamp_ms, expected_events, Vec<(event_idx, target_char, expected_scan_prefix, highlight_word)>)
    let cases: &[(i64, usize, &[(usize, char, &str, &str)])] = &[
        (12_000, 1, &[(0, '鳴', "鳴き声）", "鳴き声")]),
        (
            20_000,
            2,
            &[
                (0, 'ま', "まもなく", "まもなく"),
                (1, '鳥', "鳥白島・鳥白島漁港に到着します。", "鳥白島"),
                (1, '到', "到着します。", "到着"),
            ],
        ),
        (
            24_500,
            1,
            &[
                (0, '着', "着岸の際は　船に➡", "着岸"),
                (0, '船', "船に➡", "船"),
            ],
        ),
        (
            29_000,
            2,
            &[
                (0, '座', "座席を立たないよう…。", "座席"),
                (1, '羽', "羽依里）鳥白島　か…。", "羽依里"),
            ],
        ),
        (
            47_200,
            2,
            &[
                (0, 'な', "なんて？", "なんて"),
                (1, 'ハ', "ハッ。", "ハッ"),
            ],
        ),
        (
            420_000,
            2,
            &[
                (0, '明', "明日　７月２６日の", "明日"),
                (1, '天', "天気予報をお伝えします。", "天気予報"),
            ],
        ),
        (
            698_500,
            3,
            &[
                (0, '来', "来た坂を戻って", "来た"),
                (1, '突', "突き当たりを右。", "突き当たり"),
                (2, 'え', "えっ？", "えっ"),
            ],
        ),
        (
            848_500,
            3,
            &[
                (0, '味', "味付けも　ちょうどいい感じで。", "味付け"),
                (1, 'ど', "どうも！", "どうも"),
                (2, 'う', "う～ん！", "う～ん"),
            ],
        ),
    ];

    for &(ts_ms, expected_events, checks) in cases {
        let t_probe = Instant::now();
        let layout = engine
            .probe_active_events(&ass_doc, "", ts_ms)
            .expect("probe_active_events");
        let probe_us = t_probe.elapsed().as_micros();

        assert_eq!(
            layout.events.len(),
            expected_events,
            "timestamp {ts_ms}ms: expected {expected_events} active events, got {}",
            layout.events.len()
        );

        let mut max_patch_us = 0u128;
        for &(ev_idx, target_ch, expected_prefix, hl_word) in checks {
            let ev = &layout.events[ev_idx];
            let target_box = ev
                .chars
                .iter()
                .find(|c| c.ch == target_ch)
                .unwrap_or_else(|| {
                    panic!(
                        "timestamp {ts_ms}ms event {ev_idx}: char '{target_ch}' not found in {:?}",
                        ev.plain_text
                    )
                });
            assert!(
                target_box.has_ink && target_box.w > 2.0 && target_box.h > 4.0,
                "timestamp {ts_ms}ms char '{target_ch}' invalid box: {target_box:?}"
            );

            // Hit-test center of target character.
            let cx = target_box.x + target_box.w * 0.5;
            let cy = target_box.y + target_box.h * 0.5;
            let hit = layout.hit_test(cx, cy).unwrap_or_else(|| {
                panic!("timestamp {ts_ms}ms: hit_test({cx}, {cy}) missed '{target_ch}'")
            });
            assert_eq!(hit.event_index, ev_idx);
            assert_eq!(hit.ch, target_ch);
            assert!(
                hit.scan_substring.starts_with(expected_prefix),
                "timestamp {ts_ms}ms: expected scan prefix {:?}, got {:?}",
                expected_prefix,
                hit.scan_substring
            );

            // Verify continuous spacing coverage between adjacent characters (Spacing=4 = 8px gap).
            if hit.char_index + 1 < ev.chars.len() {
                let next_box = &ev.chars[hit.char_index + 1];
                if next_box.line_index == target_box.line_index {
                    let gap_mid = (target_box.x + target_box.w + next_box.x) * 0.5;
                    let line_mid_y =
                        ev.lines[target_box.line_index].baseline_top + ev.lines[target_box.line_index].line_height * 0.5;
                    assert!(
                        layout.hit_test(gap_mid, line_mid_y).is_some(),
                        "timestamp {ts_ms}ms: dead zone in inter-character spacing at x={gap_mid}"
                    );
                }
            }

            // Compute highlight bar for `hl_word` and render cropped RGBA highlight patch.
            let byte_start = ev
                .plain_text
                .find(hl_word)
                .unwrap_or_else(|| panic!("word {hl_word:?} not in {:?}", ev.plain_text));
            let byte_end = byte_start + hl_word.len();
            let rects = layout.highlight_rects_for_byte_span(ev_idx, byte_start, byte_end);
            assert_eq!(rects.len(), 1, "expected 1 continuous highlight bar for {hl_word:?}");

            let t_patch = Instant::now();
            let patch = engine
                .render_highlight_patch(rects[0], ts_ms)
                .expect("render_highlight_patch");
            max_patch_us = max_patch_us.max(t_patch.elapsed().as_micros());
            assert!(
                patch.has_visible_pixels,
                "timestamp {ts_ms}ms word {hl_word:?}: highlight patch was empty"
            );
        }

        eprintln!(
            "[BENCH] t={:>6}ms ({} events): probe={:>4}us, max_highlight_patch={:>4}us",
            ts_ms, expected_events, probe_us, max_patch_us
        );
    }
}

#[test]
fn pixel_parity_against_stock_mpv_rgb_reference() {
    let mkv_path = Path::new(SUMMER_POCKETS_MKV);
    let rgb_vid_path = Path::new(MPV_REF_DIR).join("rgb_vid/00000001.png");
    let rgb_sub_path = Path::new(MPV_REF_DIR).join("rgb_sub/00000001.png");

    if !mkv_path.is_file() || !rgb_vid_path.is_file() || !rgb_sub_path.is_file() {
        eprintln!("[SKIP] Summer Pockets MKV or mpv RGB reference PNGs not present");
        return;
    }

    let vid_img = image::open(&rgb_vid_path).expect("open rgb_vid").to_rgb8();
    let sub_img = image::open(&rgb_sub_path).expect("open rgb_sub").to_rgb8();
    let (w, h) = vid_img.dimensions();
    assert_eq!((w, h), (1920, 1080));
    assert_eq!(sub_img.dimensions(), (1920, 1080));

    let ass_track = extract_mkv_subtitle_track(mkv_path, 3).expect("extract ASS track 3");
    let ass_doc = ass_track.to_ass_document();

    let mut engine = AssLayoutEngine::new().expect("libass init");
    engine.configure(&MpvAssParams::for_frame(w, h));

    // 1. Full-frame RGBA render composited over clean video vs stock mpv RGB subtitle frame.
    let full_patch = engine
        .render_full_frame_rgba(&ass_doc, "", 20_000)
        .expect("render_full_frame_rgba");
    let mut composited = vid_img.as_raw().clone();
    full_patch.composite_over_rgb8(&mut composited, w, h);

    let sub_raw = sub_img.as_raw();
    let vid_raw = vid_img.as_raw();
    let mut max_diff: u8 = 0;
    let mut diff_gt_2: usize = 0;
    let mut sub_bbox = (w, h, 0u32, 0u32);
    for y in 0..h {
        for x in 0..w {
            let idx = ((y * w + x) * 3) as usize;
            let dr = composited[idx].abs_diff(sub_raw[idx]);
            let dg = composited[idx + 1].abs_diff(sub_raw[idx + 1]);
            let db = composited[idx + 2].abs_diff(sub_raw[idx + 2]);
            let d = dr.max(dg).max(db);
            if d > max_diff {
                max_diff = d;
            }
            if d > 2 {
                diff_gt_2 += 1;
            }
            // Track actual mpv subtitle pixel bounds (where rgb_sub differs from clean rgb_vid).
            let vd = sub_raw[idx]
                .abs_diff(vid_raw[idx])
                .max(sub_raw[idx + 1].abs_diff(vid_raw[idx + 1]))
                .max(sub_raw[idx + 2].abs_diff(vid_raw[idx + 2]));
            if vd > 4 {
                sub_bbox.0 = sub_bbox.0.min(x);
                sub_bbox.1 = sub_bbox.1.min(y);
                sub_bbox.2 = sub_bbox.2.max(x + 1);
                sub_bbox.3 = sub_bbox.3.max(y + 1);
            }
        }
    }

    eprintln!(
        "[PARITY] Full-frame 1920x1080 @ 20.0s vs stock mpv: max_diff={max_diff}/255, pixels_diff_gt_2={diff_gt_2}, mpv_sub_bbox={sub_bbox:?}"
    );
    assert!(
        max_diff <= 2,
        "expected full-frame pixel parity <= 2/255 vs stock mpv, got max_diff={max_diff}"
    );
    assert_eq!(diff_gt_2, 0, "expected 0 pixels with diff > 2/255");

    // 2. Verify highlight-patch cropped render around "鳥白島" matches stock mpv identically
    //    AND verify probed ink bbox aligns with mpv's actual rendered subtitle bbox!
    let layout = engine
        .probe_active_events(&ass_doc, "", 20_000)
        .expect("probe @ 20s");
    let overall = layout.overall_ink_bounds().expect("overall ink bounds");
    // Probed fill glyphs sit tightly inside mpv's (fill + 4px outline + 4px shadow) bbox!
    assert!(
        (overall.x - sub_bbox.0 as f32).abs() <= 8.0
            && (overall.y - sub_bbox.1 as f32).abs() <= 8.0
            && (overall.right() - sub_bbox.2 as f32).abs() <= 12.0
            && (overall.bottom() - sub_bbox.3 as f32).abs() <= 12.0,
        "probed ink bounds {overall:?} misaligned with mpv rendered bbox {sub_bbox:?}"
    );

    let hl_rects = layout.highlight_rects_for_byte_span(1, 0, "鳥白島".len());
    assert_eq!(hl_rects.len(), 1);
    let hl_patch = engine
        .render_highlight_patch(hl_rects[0], 20_000)
        .expect("render highlight patch");

    let mut patch_comp = vid_img.as_raw().clone();
    hl_patch.composite_over_rgb8(&mut patch_comp, w, h);

    let mut patch_max_diff: u8 = 0;
    let mut patch_gt_2: usize = 0;
    for py in 0..hl_patch.height {
        let fy = hl_patch.y + py;
        for px in 0..hl_patch.width {
            let fx = hl_patch.x + px;
            let idx = ((fy * w + fx) * 3) as usize;
            let d = patch_comp[idx]
                .abs_diff(sub_raw[idx])
                .max(patch_comp[idx + 1].abs_diff(sub_raw[idx + 1]))
                .max(patch_comp[idx + 2].abs_diff(sub_raw[idx + 2]));
            patch_max_diff = patch_max_diff.max(d);
            if d > 2 {
                patch_gt_2 += 1;
            }
        }
    }

    eprintln!(
        "[PARITY] Highlight patch ({}x{} at {},{}) vs stock mpv: max_diff={patch_max_diff}/255, pixels_diff_gt_2={patch_gt_2}",
        hl_patch.width, hl_patch.height, hl_patch.x, hl_patch.y
    );
    assert!(patch_max_diff <= 2);
    assert_eq!(patch_gt_2, 0);
}

#[test]
fn violet_evergarden_embedded_fonts_and_ass_extraction() {
    let dir = Path::new(VIOLET_EVERGARDEN_DIR);
    if !dir.is_dir() {
        eprintln!("[SKIP] Violet Evergarden dir not found at {}", dir.display());
        return;
    }

    let mut mkvs: Vec<PathBuf> = fs::read_dir(dir)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("mkv"))
        .collect();
    mkvs.sort();
    let Some(mkv_path) = mkvs.first() else {
        eprintln!("[SKIP] No .mkv found in {}", dir.display());
        return;
    };

    let t_fonts = Instant::now();
    let fonts = extract_mkv_fonts(mkv_path).expect("extract_mkv_fonts");
    let fonts_ms = t_fonts.elapsed().as_secs_f64() * 1000.0;
    assert!(
        fonts.len() >= 5,
        "expected >= 5 embedded fonts in Violet Evergarden, got {}",
        fonts.len()
    );
    assert!(
        fonts.iter().all(|f| !f.data.is_empty()),
        "all extracted fonts must have non-empty payloads"
    );

    let tracks = list_mkv_subtitle_tracks(mkv_path).expect("list_mkv_subtitle_tracks");
    let first_ass = tracks
        .iter()
        .find(|t| t.codec == MkvSubtitleCodec::Ass)
        .expect("at least one ASS track");

    let t_extract = Instant::now();
    let extracted = extract_mkv_subtitle_track(mkv_path, first_ass.track_number)
        .expect("extract first ASS track");
    let extract_ms = t_extract.elapsed().as_secs_f64() * 1000.0;
    assert!(
        extracted.cues.len() > 50,
        "expected > 50 cues in Violet Evergarden ASS track, got {}",
        extracted.cues.len()
    );

    // Load all extracted embedded fonts into AssLayoutEngine and probe a dialogue cue!
    let mut engine = AssLayoutEngine::new().expect("libass init");
    engine.configure(&MpvAssParams::for_frame(1920, 1080));
    let t_load = Instant::now();
    engine.load_memory_fonts(&fonts).expect("load_memory_fonts");
    let load_ms = t_load.elapsed().as_secs_f64() * 1000.0;
    assert_eq!(engine.loaded_font_count(), fonts.len());

    let ass_doc = extracted.to_ass_document();
    let sample_cue = extracted
        .cues
        .iter()
        .find(|c| c.end_ms > c.start_ms + 500 && c.plain_text.chars().any(|ch| ch.is_alphanumeric()))
        .expect("dialogue cue");
    let mid_ms = (sample_cue.start_ms + sample_cue.end_ms) / 2;

    let layout = engine
        .probe_active_events(&ass_doc, "", mid_ms)
        .expect("probe with embedded fonts");
    let active_ev = layout
        .events
        .iter()
        .find(|e| !e.lines.is_empty())
        .expect("at least one visible active line");
    let b = active_ev.bounds.expect("bounds");
    let patch = engine
        .render_highlight_patch(AssHighlightRect::new(b.x, b.y, b.w, b.h), mid_ms)
        .expect("render patch with embedded fonts");
    assert!(patch.has_visible_pixels);

    eprintln!(
        "[BENCH] Violet Evergarden ({}): {} fonts in {:.2}ms (ass_add_font {:.2}ms), {} sub tracks, extracted {} cues in {:.2}ms, sample={:?}",
        mkv_path.file_name().unwrap().to_string_lossy(),
        fonts.len(),
        fonts_ms,
        load_ms,
        tracks.len(),
        extracted.cues.len(),
        extract_ms,
        sample_cue.plain_text,
    );
}

// ---------------------------------------------------------------------------
// Minimal EBML writer helpers for the self-contained synthetic MKV unit test.
// ---------------------------------------------------------------------------

fn encode_vint_size(val: u64) -> Vec<u8> {
    if val < 0x7F {
        vec![0x80 | (val as u8)]
    } else if val < 0x3FFF {
        vec![0x40 | ((val >> 8) as u8), (val & 0xFF) as u8]
    } else {
        vec![
            0x20 | ((val >> 16) as u8),
            ((val >> 8) & 0xFF) as u8,
            (val & 0xFF) as u8,
        ]
    }
}

fn ebml_elem(id: &[u8], payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(id.len() + 4 + payload.len());
    out.extend_from_slice(id);
    out.extend_from_slice(&encode_vint_size(payload.len() as u64));
    out.extend_from_slice(payload);
    out
}

fn ebml_uint(id: &[u8], v: u64) -> Vec<u8> {
    let bytes = if v == 0 {
        vec![0]
    } else {
        let be = v.to_be_bytes();
        let first_nz = be.iter().position(|&b| b != 0).unwrap_or(7);
        be[first_nz..].to_vec()
    };
    ebml_elem(id, &bytes)
}

fn build_synthetic_mkv(compress: impl Fn(&[u8]) -> Vec<u8>) -> Vec<u8> {
    // EBML header
    let ebml_hdr = ebml_elem(&[0x1A, 0x45, 0xDF, 0xA3], &ebml_elem(&[0x42, 0x82], b"matroska"));

    // Info: TimestampScale = 1_000_000 (1ms)
    let info = ebml_elem(&[0x15, 0x49, 0xA9, 0x66], &ebml_uint(&[0x2A, 0xD7, 0xB1], 1_000_000));

    // Tracks: Track 3 (S_TEXT/ASS with zlib compression on frame contents)
    let comp = ebml_elem(&[0x50, 0x34], &ebml_uint(&[0x42, 0x54], 0));
    let mut enc_body = Vec::new();
    enc_body.extend(ebml_uint(&[0x50, 0x32], 1)); // Scope = 1 (frame contents)
    enc_body.extend(ebml_uint(&[0x50, 0x33], 0)); // Type = compression
    enc_body.extend(comp);
    let encodings = ebml_elem(&[0x6D, 0x80], &ebml_elem(&[0x62, 0x40], &enc_body));

    let mut trk3 = Vec::new();
    trk3.extend(ebml_uint(&[0xD7], 3)); // TrackNumber = 3
    trk3.extend(ebml_uint(&[0x83], 0x11)); // Subtitle
    trk3.extend(ebml_elem(&[0x53, 0x6E], b"Japanese ASS"));
    trk3.extend(ebml_elem(&[0x22, 0xB5, 0x9C], b"jpn"));
    trk3.extend(ebml_elem(&[0x86], b"S_TEXT/ASS"));
    trk3.extend(ebml_elem(&[0x63, 0xA2], SYNTHETIC_HEADER.as_bytes()));
    trk3.extend(encodings);
    let tracks = ebml_elem(&[0x16, 0x54, 0xAE, 0x6B], &ebml_elem(&[0xAE], &trk3));

    // Cluster at 10_000ms with:
    //  - Video SimpleBlock on Track 1 (must be skipped!)
    //  - BlockGroup on Track 3 at rel +0ms, duration 3500ms
    //  - BlockGroup on Track 3 at rel +10500ms, duration 1500ms
    let mut vid_sb = vec![0x81, 0x00, 0x00, 0x80]; // track 1, rel 0, keyframe
    vid_sb.extend_from_slice(&[0xAA; 64]);

    let make_sub_bg = |rel_ms: i16, dur_ms: u64, ass_block: &[u8]| -> Vec<u8> {
        let compressed = compress(ass_block);
        let mut blk = vec![0x83]; // track 3 VINT
        blk.extend_from_slice(&rel_ms.to_be_bytes());
        blk.push(0x00); // flags: no lacing
        blk.extend_from_slice(&compressed);
        let mut bg = ebml_elem(&[0xA1], &blk);
        bg.extend(ebml_uint(&[0x9B], dur_ms));
        ebml_elem(&[0xA0], &bg)
    };

    let mut cluster_body = Vec::new();
    cluster_body.extend(ebml_uint(&[0xE7], 10_000));
    cluster_body.extend(ebml_elem(&[0xA3], &vid_sb));
    cluster_body.extend(make_sub_bg(
        0,
        3_500,
        "1,0,Default,,0,0,0,,{\\pos(480,502)}鳥白島に到着".as_bytes(),
    ));
    cluster_body.extend(make_sub_bg(
        10_500,
        1_500,
        "2,0,Default,,0,0,0,,二行目のテスト".as_bytes(),
    ));
    let cluster = ebml_elem(&[0x1F, 0x43, 0xB6, 0x75], &cluster_body);

    // Attachments placed AFTER Cluster (reached via SeekHead fast-jump!): 1 font + 1 cover jpg
    let mut font_file = Vec::new();
    font_file.extend(ebml_elem(&[0x46, 0x6E], b"TestFont.ttf"));
    font_file.extend(ebml_elem(&[0x46, 0x60], b"application/x-truetype-font"));
    font_file.extend(ebml_elem(&[0x46, 0x5C], b"FAKE_TTF_BYTES_12345"));

    let mut cover_file = Vec::new();
    cover_file.extend(ebml_elem(&[0x46, 0x6E], b"cover.jpg"));
    cover_file.extend(ebml_elem(&[0x46, 0x60], b"image/jpeg"));
    cover_file.extend(ebml_elem(&[0x46, 0x5C], b"NOT_A_FONT"));

    let mut att_body = ebml_elem(&[0x61, 0xA7], &font_file);
    att_body.extend(ebml_elem(&[0x61, 0xA7], &cover_file));
    let attachments = ebml_elem(&[0x19, 0x41, 0xA4, 0x69], &att_body);

    // Compute SeekHead pointing to Attachments at end of Segment.
    // Two-pass sizing so SeekPosition is exact.
    let make_seek_head = |att_pos: u64| -> Vec<u8> {
        let mut seek_entry = ebml_elem(&[0x53, 0xAB], &[0x19, 0x41, 0xA4, 0x69]);
        seek_entry.extend(ebml_uint(&[0x53, 0xAC], att_pos));
        ebml_elem(&[0x11, 0x4D, 0x9B, 0x74], &ebml_elem(&[0x4D, 0xBB], &seek_entry))
    };
    let mut seek_head = make_seek_head(0);
    for _ in 0..3 {
        let att_pos = (seek_head.len() + info.len() + tracks.len() + cluster.len()) as u64;
        seek_head = make_seek_head(att_pos);
    }

    let mut seg_body = Vec::new();
    seg_body.extend(seek_head);
    seg_body.extend(info);
    seg_body.extend(tracks);
    seg_body.extend(cluster);
    seg_body.extend(attachments);

    // Verify parse_ass_timecode_ms linkage stays exercised too.
    assert_eq!(parse_ass_timecode_ms("0:00:10.00"), Some(10_000));

    let mut file = ebml_hdr;
    file.extend(ebml_elem(&[0x18, 0x53, 0x80, 0x67], &seg_body));
    file
}

//! Capture the media requested by configured Anki field markers.

use std::io::Cursor;

use image::{DynamicImage, ImageFormat, RgbaImage};
use memento_anki::note::{
    audio_context_params, audio_media_params, compute_clip_range, screenshot_params, video_params,
    AudioMediaParams, ScreenshotParams, VideoParams,
};
use memento_anki::tokenizer::tokenize;
use memento_player::RawFrame;
use serde_json::json;

use super::anki::{evaluate_template_marker_kind, MarkerEvalKind, NoteFieldsContext};
use super::audio::{fetch_word_audio, sanitize_anki_filename};
use super::config::{AnkiNoteConfig, AudioSourceEntry};
use super::media::{extract_clip_audio, extract_clip_mp4, MediaContext};

pub struct NoteMediaInput {
    pub media: Option<MediaContext>,
    pub subtitle_start: f64,
    pub subtitle_end: f64,
    pub context_start: f64,
    pub context_end: f64,
    pub screenshot: Option<RawFrame>,
    pub screenshot_video: Option<RawFrame>,
    pub subtitle_id: i64,
    pub audio_sources: Vec<AudioSourceEntry>,
}

/// Use the same marker grammar for capture and attachment generation, including
/// whitespace and alternatives such as `{screenshot-video|screenshot}`.
/// Alternatives are evaluated sequentially and stop at the first valid
/// screenshot alternative in each token.
pub fn requested_screenshots(profile: &AnkiNoteConfig) -> (bool, bool) {
    let mut pictures = false;
    let mut video_pictures = false;
    for template in profile.field_templates.values() {
        for token in tokenize(&template.encode_utf16().collect::<Vec<_>>()) {
            for marker in token.markers {
                let name = String::from_utf16_lossy(&marker.name);
                match name.as_str() {
                    "screenshot" | "image" if screenshot_params(&marker).is_some() => {
                        pictures = true;
                        break;
                    }
                    "screenshot-video" if screenshot_params(&marker).is_some() => {
                        video_pictures = true;
                        break;
                    }
                    _ => {}
                }
            }
        }
    }
    (pictures, video_pictures)
}

fn encode_base64(bytes: &[u8]) -> Result<String, String> {
    let len = memento_util::fileutils::base64_len(bytes.len())
        .ok_or("Anki media is too large to encode")?;
    let mut encoded = vec![0; len];
    memento_util::fileutils::to_base64(bytes, &mut encoded)
        .ok_or("Could not encode Anki media")?;
    String::from_utf8(encoded).map_err(|error| error.to_string())
}

fn media_filename(bytes: &[u8], extension: &str) -> Result<String, String> {
    let md5 = memento_util::fileutils::calculate_md5(bytes)
        .map_err(|error| format!("Could not hash Anki media: {error}"))?;
    let raw = format!("{:032x}{extension}", u128::from_be_bytes(md5));
    Ok(sanitize_anki_filename(&raw, extension))
}

fn encode_picture(
    frame: &RawFrame,
    params: ScreenshotParams,
    format: i32,
) -> Result<(Vec<u8>, &'static str), String> {
    let rgba = RgbaImage::from_raw(frame.width, frame.height, frame.pixels.clone())
        .ok_or("Video screenshot has invalid dimensions")?;
    let mut image = DynamicImage::ImageRgba8(rgba);
    let max_width = if params.max_width > 0 {
        params.max_width as u32
    } else {
        frame.width
    };
    let max_height = if params.max_height > 0 {
        params.max_height as u32
    } else {
        frame.height
    };
    if max_width < frame.width || max_height < frame.height {
        image = if params.keep_aspect_ratio {
            image.resize(max_width, max_height, image::imageops::FilterType::Lanczos3)
        } else {
            image.resize_exact(max_width, max_height, image::imageops::FilterType::Lanczos3)
        };
    }
    let (image_format, extension) = match format {
        1 => (ImageFormat::Png, ".png"),
        2 => (ImageFormat::WebP, ".webp"),
        _ => (ImageFormat::Jpeg, ".jpg"),
    };
    let mut output = Cursor::new(Vec::new());
    image
        .write_to(&mut output, image_format)
        .map_err(|error| format!("Could not encode Anki screenshot: {error}"))?;
    Ok((output.into_inner(), extension))
}

fn add_unique_field(fields: &mut Vec<String>, field: &str) {
    if !fields.iter().any(|existing| existing == field) {
        fields.push(field.to_owned());
    }
}

fn record_resolved_media_marker(ctx: &mut NoteFieldsContext, field: &str, marker_name: &str) {
    if !ctx
        .resolved_media_markers
        .iter()
        .any(|(f, m)| f == field && m == marker_name)
    {
        ctx.resolved_media_markers
            .push((field.to_owned(), marker_name.to_owned()));
    }
}

/// Fill AnkiConnect's media arrays before `addNote`. Alternatives in `{a | b}`
/// are evaluated sequentially and stop on the first alternative that succeeds;
/// if a media alternative fails and a non-media text marker follows, fallback
/// succeeds without failing the card.
pub fn prepare_note_media(
    profile: &AnkiNoteConfig,
    ctx: &mut NoteFieldsContext,
    input: NoteMediaInput,
) -> Result<(), String> {
    let mut word_audio_cache: Option<Result<(String, String), String>> = None;
    let mut word_audio_fields: Vec<String> = Vec::new();

    let mut clip_cache: Vec<(AudioMediaParams, Result<(String, String), String>, Vec<String>)> =
        Vec::new();
    let mut picture_cache: Vec<(
        (bool, ScreenshotParams),
        Result<(String, String), String>,
        Vec<String>,
    )> = Vec::new();
    let mut video_cache: Vec<(VideoParams, Result<(String, String), String>, Vec<String>)> =
        Vec::new();
    let mut clipboard_cache: Option<String> = None;

    let mut sorted_fields: Vec<(&String, &String)> = profile.field_templates.iter().collect();
    sorted_fields.sort_by_key(|(name, _)| *name);

    for (field, template) in sorted_fields {
        for token in tokenize(&template.encode_utf16().collect::<Vec<_>>()) {
            let mut token_media_error: Option<String> = None;
            let mut token_handled_fallback = false;
            let mut token_succeeded = false;

            for marker in &token.markers {
                match evaluate_template_marker_kind(marker, ctx, &mut clipboard_cache) {
                    MarkerEvalKind::NonEmptyText => {
                        token_succeeded = true;
                        break;
                    }
                    MarkerEvalKind::EmptyText => {
                        token_handled_fallback = true;
                        continue;
                    }
                    MarkerEvalKind::Unknown => {
                        continue;
                    }
                    MarkerEvalKind::Media => {}
                }

                let name = String::from_utf16_lossy(&marker.name);
                match name.as_str() {
                    "audio" => {
                        let cached = word_audio_cache.get_or_insert_with(|| {
                            let (data, raw_filename) =
                                fetch_word_audio(&ctx.expression, &ctx.reading, &input.audio_sources)
                                    .map_err(|error| format!("Could not capture word audio: {error}"))?;
                            let encoded = encode_base64(&data)?;
                            let safe_filename = sanitize_anki_filename(&raw_filename, ".mp3");
                            Ok((encoded, safe_filename))
                        });
                        match cached {
                            Ok(_) => {
                                add_unique_field(&mut word_audio_fields, field);
                                record_resolved_media_marker(ctx, field, &name);
                                token_succeeded = true;
                                break;
                            }
                            Err(error) => {
                                token_media_error = Some(error.clone());
                            }
                        }
                    }
                    "audio-media" | "audio-context" | "sentence-audio" => {
                        let params_opt = if name == "audio-context" {
                            audio_context_params(marker, profile.audio_normalize, profile.audio_db)
                        } else {
                            audio_media_params(marker, profile.audio_normalize, profile.audio_db)
                        };
                        let Some(params) = params_opt else {
                            token_media_error =
                                Some(format!("Invalid {{{name}}} marker in Anki field {field}"));
                            continue;
                        };
                        let idx = if let Some(idx) =
                            clip_cache.iter().position(|(existing, _, _)| *existing == params)
                        {
                            idx
                        } else {
                            let res = (|| -> Result<(String, String), String> {
                                let media = input
                                    .media
                                    .as_ref()
                                    .ok_or("No video is loaded for sentence audio")?;
                                let (start, end) = compute_clip_range(
                                    params.source,
                                    input.subtitle_start,
                                    input.subtitle_end,
                                    input.context_start,
                                    input.context_end,
                                    profile.audio_pad_start,
                                    profile.audio_pad_end,
                                )
                                .ok_or("The selected subtitle has no usable audio time range")?;
                                let data = extract_clip_audio(
                                    media,
                                    start,
                                    end,
                                    params.normalize.then_some(params.db),
                                )?;
                                Ok((encode_base64(&data)?, media_filename(&data, ".aac")?))
                            })();
                            clip_cache.push((params, res, Vec::new()));
                            clip_cache.len() - 1
                        };
                        match &clip_cache[idx].1 {
                            Ok(_) => {
                                add_unique_field(&mut clip_cache[idx].2, field);
                                record_resolved_media_marker(ctx, field, &name);
                                token_succeeded = true;
                                break;
                            }
                            Err(error) => {
                                token_media_error = Some(error.clone());
                            }
                        }
                    }
                    "screenshot" | "screenshot-video" | "image" => {
                        let Some(params) = screenshot_params(marker) else {
                            token_media_error =
                                Some(format!("Invalid {{{name}}} marker in Anki field {field}"));
                            continue;
                        };
                        let is_video_only = name == "screenshot-video";
                        let key = (is_video_only, params);
                        let idx = if let Some(idx) =
                            picture_cache.iter().position(|(existing, _, _)| *existing == key)
                        {
                            idx
                        } else {
                            let frame_opt = if is_video_only {
                                input.screenshot_video.as_ref()
                            } else {
                                input.screenshot.as_ref()
                            };
                            let res = (|| -> Result<(String, String), String> {
                                let frame = frame_opt
                                    .ok_or("Could not capture the video screenshot for Anki")?;
                                let (data, extension) =
                                    encode_picture(frame, params, profile.screenshot_type)?;
                                Ok((encode_base64(&data)?, media_filename(&data, extension)?))
                            })();
                            picture_cache.push((key, res, Vec::new()));
                            picture_cache.len() - 1
                        };
                        match &picture_cache[idx].1 {
                            Ok(_) => {
                                add_unique_field(&mut picture_cache[idx].2, field);
                                record_resolved_media_marker(ctx, field, &name);
                                token_succeeded = true;
                                break;
                            }
                            Err(error) => {
                                token_media_error = Some(error.clone());
                            }
                        }
                    }
                    "video" => {
                        let Some(params) =
                            video_params(marker, profile.audio_normalize, profile.audio_db)
                        else {
                            token_media_error =
                                Some(format!("Invalid {{video}} marker in Anki field {field}"));
                            continue;
                        };
                        let idx = if let Some(idx) =
                            video_cache.iter().position(|(existing, _, _)| *existing == params)
                        {
                            idx
                        } else {
                            let res = (|| -> Result<(String, String), String> {
                                let media = input
                                    .media
                                    .as_ref()
                                    .ok_or("No video is loaded for an Anki video clip")?;
                                let (start, end) = compute_clip_range(
                                    params.source,
                                    input.subtitle_start,
                                    input.subtitle_end,
                                    input.context_start,
                                    input.context_end,
                                    profile.audio_pad_start,
                                    profile.audio_pad_end,
                                )
                                .ok_or("The selected subtitle has no usable video time range")?;
                                let data = extract_clip_mp4(
                                    media,
                                    input.subtitle_id,
                                    start,
                                    end,
                                    params.audio,
                                    params.subtitles,
                                    params.normalize.then_some(params.db),
                                )?;
                                Ok((encode_base64(&data)?, media_filename(&data, ".mp4")?))
                            })();
                            video_cache.push((params, res, Vec::new()));
                            video_cache.len() - 1
                        };
                        match &video_cache[idx].1 {
                            Ok(_) => {
                                add_unique_field(&mut video_cache[idx].2, field);
                                record_resolved_media_marker(ctx, field, &name);
                                token_succeeded = true;
                                break;
                            }
                            Err(error) => {
                                token_media_error = Some(error.clone());
                            }
                        }
                    }
                    _ => {}
                }
            }

            if !token_succeeded && !token_handled_fallback {
                if let Some(error) = token_media_error {
                    return Err(error);
                }
            }
        }
    }

    if !word_audio_fields.is_empty() {
        if let Some(Ok((data, filename))) = word_audio_cache {
            ctx.audio_attachments.push(json!({
                "data": data,
                "filename": filename,
                "fields": word_audio_fields,
            }));
        }
    }

    for (_, result, fields) in clip_cache {
        if !fields.is_empty() {
            if let Ok((data, filename)) = result {
                ctx.audio_attachments.push(json!({
                    "data": data,
                    "filename": filename,
                    "fields": fields,
                }));
            }
        }
    }

    for (_, result, fields) in picture_cache {
        if !fields.is_empty() {
            if let Ok((data, filename)) = result {
                ctx.picture_attachments.push(json!({
                    "data": data,
                    "filename": filename,
                    "fields": fields,
                }));
            }
        }
    }

    for (_, result, fields) in video_cache {
        if !fields.is_empty() {
            if let Ok((data, filename)) = result {
                ctx.video_attachments.push(json!({
                    "data": data,
                    "filename": filename,
                    "fields": fields,
                }));
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::controller::anki::build_note_payload_from_profile;

    fn is_ascii_safe_filename(name: &str) -> bool {
        !name.is_empty()
            && !name.contains("..")
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    }

    #[test]
    #[ignore = "requires libmpv AAC encoding"]
    fn audio_context_attachment_covers_all_marked_subtitles() {
        use crate::controller::subtitles::{create_cue, SubtitleController};
        use base64::{engine::general_purpose::STANDARD, Engine};

        let samples: Vec<i16> = (0..8 * 16000)
            .map(|i| ((i as f64 * 440.0 * std::f64::consts::TAU / 16000.0).sin() * 12000.0) as i16)
            .collect();
        let mut wave = Vec::new();
        wave.extend_from_slice(b"RIFF");
        wave.extend_from_slice(&(36u32 + samples.len() as u32 * 2).to_le_bytes());
        wave.extend_from_slice(b"WAVEfmt \x10\0\0\0\x01\0\x01\0");
        wave.extend_from_slice(&16000u32.to_le_bytes());
        wave.extend_from_slice(&32000u32.to_le_bytes());
        wave.extend_from_slice(b"\x02\0\x10\0data");
        wave.extend_from_slice(&(samples.len() as u32 * 2).to_le_bytes());
        for sample in samples {
            wave.extend_from_slice(&sample.to_le_bytes());
        }
        let path = std::env::temp_dir().join(format!("memento-context-{}.wav", std::process::id()));
        std::fs::write(&path, wave).unwrap();
        let mut subtitles = SubtitleController::new();
        subtitles.load_cues(vec![
            create_cue("First", 1., 2.),
            create_cue("Current word", 3., 4.),
            create_cue("Last", 5., 6.),
        ]);
        subtitles.select_row(0, false);
        subtitles.select_row(2, false);
        subtitles.update_position(3.5);
        let active = subtitles.active_cue().unwrap();
        let (context_start, context_end) = subtitles.selected_time_range().unwrap();
        let profile = AnkiNoteConfig {
            enabled: true,
            endpoint: memento_net::anki::AnkiEndpoint::default(),
            deck: "Japanese".into(),
            model: "Basic".into(),
            field_templates: [
                ("ContextAudio".into(), "{audio-context}".into()),
                ("SentenceAudio".into(), "{audio-media}".into()),
            ]
            .into(),
            tags: Vec::new(),
            duplicate_policy: 1,
            screenshot_type: 0,
            audio_pad_start: 0.0,
            audio_pad_end: 0.0,
            audio_normalize: false,
            audio_db: -20.0,
            newline_replacer: "<br>".into(),
        };
        let input = NoteMediaInput {
            media: Some(MediaContext::new(path.to_string_lossy().into_owned(), 1)),
            subtitle_start: active.start,
            subtitle_end: active.end,
            context_start,
            context_end,
            screenshot: None,
            screenshot_video: None,
            subtitle_id: 0,
            audio_sources: Vec::new(),
        };
        let mut context = NoteFieldsContext::new("word", "word", "Current word");
        let result = prepare_note_media(&profile, &mut context, input);
        let _ = std::fs::remove_file(path);
        result.unwrap();
        assert_eq!(context.audio_attachments.len(), 2);
        let ctx_att = context
            .audio_attachments
            .iter()
            .find(|audio| audio["fields"][0] == "ContextAudio")
            .unwrap();
        let sent_att = context
            .audio_attachments
            .iter()
            .find(|audio| audio["fields"][0] == "SentenceAudio")
            .unwrap();
        for att in [ctx_att, sent_att] {
            let fname = att["filename"].as_str().unwrap();
            assert!(fname.ends_with(".aac"), "expected .aac clip, got {fname}");
            assert!(is_ascii_safe_filename(fname));
        }
        let ctx_bytes = STANDARD.decode(ctx_att["data"].as_str().unwrap()).unwrap();
        let sent_bytes = STANDARD.decode(sent_att["data"].as_str().unwrap()).unwrap();
        assert!(ctx_bytes.len() > sent_bytes.len());
    }

    #[test]
    fn screenshot_profile_formats_produce_real_images() {
        let frame = RawFrame {
            width: 2,
            height: 2,
            pixels: vec![
                255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255,
            ],
        };
        let params = ScreenshotParams {
            max_width: -1,
            max_height: -1,
            keep_aspect_ratio: true,
        };
        for (kind, extension, format) in [
            (0, ".jpg", ImageFormat::Jpeg),
            (1, ".png", ImageFormat::Png),
            (2, ".webp", ImageFormat::WebP),
        ] {
            let (bytes, actual_extension) = encode_picture(&frame, params, kind).unwrap();
            assert_eq!(actual_extension, extension);
            let decoded = image::load_from_memory_with_format(&bytes, format).unwrap();
            assert_eq!((decoded.width(), decoded.height()), (2, 2));
        }
    }

    #[test]
    fn picture_marker_breaks_on_first_succeeding_alternative_and_falls_back() {
        let mut fields = std::collections::HashMap::new();
        fields.insert(
            "Picture".into(),
            "{ screenshot-video : max-width=1 | image : max-width=1 }".into(),
        );
        let profile = AnkiNoteConfig {
            enabled: true,
            endpoint: memento_net::anki::AnkiEndpoint::default(),
            deck: "Japanese".into(),
            model: "Basic".into(),
            field_templates: fields,
            tags: Vec::new(),
            duplicate_policy: 1,
            screenshot_type: 1,
            audio_pad_start: 0.0,
            audio_pad_end: 0.0,
            audio_normalize: false,
            audio_db: -20.0,
            newline_replacer: "<br>".into(),
        };
        // First alternative in `{screenshot-video | image}` is `screenshot-video`.
        assert_eq!(requested_screenshots(&profile), (false, true));

        let frame_video = RawFrame {
            width: 2,
            height: 2,
            pixels: vec![255; 16],
        };
        let frame_sub = RawFrame {
            width: 2,
            height: 2,
            pixels: vec![128; 16],
        };

        // 1. When screenshot-video succeeds, break immediately: attach ONLY 1 picture!
        let mut ctx = NoteFieldsContext::new("日本語", "にほんご", "日本語を勉強する");
        let input = NoteMediaInput {
            media: None,
            subtitle_start: 0.0,
            subtitle_end: 0.0,
            context_start: 0.0,
            context_end: 0.0,
            screenshot: Some(frame_sub.clone()),
            screenshot_video: Some(frame_video),
            subtitle_id: 0,
            audio_sources: Vec::new(),
        };
        prepare_note_media(&profile, &mut ctx, input).unwrap();
        assert_eq!(ctx.picture_attachments.len(), 1);
        assert_eq!(ctx.picture_attachments[0]["fields"][0], "Picture");
        let fname = ctx.picture_attachments[0]["filename"].as_str().unwrap();
        assert!(fname.ends_with(".png"));
        assert!(is_ascii_safe_filename(fname));

        // 2. When screenshot-video is unavailable, fall back to `image` (`screenshot`).
        let mut ctx_fallback = NoteFieldsContext::new("日本語", "にほんご", "日本語を勉強する");
        let input_fallback = NoteMediaInput {
            media: None,
            subtitle_start: 0.0,
            subtitle_end: 0.0,
            context_start: 0.0,
            context_end: 0.0,
            screenshot: Some(frame_sub),
            screenshot_video: None,
            subtitle_id: 0,
            audio_sources: Vec::new(),
        };
        prepare_note_media(&profile, &mut ctx_fallback, input_fallback).unwrap();
        assert_eq!(ctx_fallback.picture_attachments.len(), 1);
        assert_eq!(ctx_fallback.picture_attachments[0]["fields"][0], "Picture");
    }

    #[test]
    fn word_audio_is_attached_as_data_for_anki_and_short_circuits_clip_fallback() {
        let path = std::env::temp_dir().join(format!(
            "memento-anki-word-audio-{}-{}.mp3",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(&path, b"ID3test-audio").unwrap();
        let mut fields = std::collections::HashMap::new();
        // `media: None` would fail `{audio-media}` if `{audio}` did not break on success!
        fields.insert("ExpressionAudio".into(), "{audio | audio-media}".into());
        let profile = AnkiNoteConfig {
            enabled: true,
            endpoint: memento_net::anki::AnkiEndpoint::default(),
            deck: "Japanese".into(),
            model: "Lapis".into(),
            field_templates: fields,
            tags: Vec::new(),
            duplicate_policy: 1,
            screenshot_type: 0,
            audio_pad_start: 0.0,
            audio_pad_end: 0.0,
            audio_normalize: false,
            audio_db: -20.0,
            newline_replacer: "<br>".into(),
        };
        let input = NoteMediaInput {
            media: None,
            subtitle_start: 0.0,
            subtitle_end: 0.0,
            context_start: 0.0,
            context_end: 0.0,
            screenshot: None,
            screenshot_video: None,
            subtitle_id: 0,
            audio_sources: vec![AudioSourceEntry {
                name: "Local 日本語".into(),
                url: path.to_string_lossy().into_owned(),
                kind: memento_setting::enums::AudioSourceType::AudioSourceTypeFile as i64,
                md5: String::new(),
            }],
        };
        let mut ctx = NoteFieldsContext::new("休み", "やすみ", "今日は休み。");
        let result = prepare_note_media(&profile, &mut ctx, input);
        let _ = std::fs::remove_file(path);
        result.unwrap();
        assert_eq!(ctx.audio_attachments.len(), 1);
        assert!(ctx.audio_attachments[0]["data"]
            .as_str()
            .is_some_and(|data| !data.is_empty()));
        assert!(ctx.audio_attachments[0].get("url").is_none());
        assert_eq!(ctx.audio_attachments[0]["fields"][0], "ExpressionAudio");
        let fname = ctx.audio_attachments[0]["filename"].as_str().unwrap();
        assert!(is_ascii_safe_filename(fname), "unsafe filename: {fname}");
    }

    #[test]
    fn failed_media_marker_falls_back_to_following_text_marker_without_failing_card() {
        let mut fields = std::collections::HashMap::new();
        fields.insert("Front".into(), "{audio | expression}".into());
        let profile = AnkiNoteConfig {
            enabled: true,
            endpoint: memento_net::anki::AnkiEndpoint::default(),
            deck: "Japanese".into(),
            model: "Basic".into(),
            field_templates: fields,
            tags: Vec::new(),
            duplicate_policy: 1,
            screenshot_type: 0,
            audio_pad_start: 0.0,
            audio_pad_end: 0.0,
            audio_normalize: false,
            audio_db: -20.0,
            newline_replacer: "<br>".into(),
        };

        // 1. Audio fails (no audio sources) -> falls back to `{expression}` cleanly!
        let mut ctx_fallback = NoteFieldsContext::new("食べる", "たべる", "パンを食べる。");
        let input_fail = NoteMediaInput {
            media: None,
            subtitle_start: 0.0,
            subtitle_end: 0.0,
            context_start: 0.0,
            context_end: 0.0,
            screenshot: None,
            screenshot_video: None,
            subtitle_id: 0,
            audio_sources: Vec::new(),
        };
        prepare_note_media(&profile, &mut ctx_fallback, input_fail).unwrap();
        assert!(ctx_fallback.audio_attachments.is_empty());
        let payload_fallback = build_note_payload_from_profile(&profile, &ctx_fallback).unwrap();
        assert_eq!(payload_fallback.fields["Front"], "食べる");

        // 2. Audio succeeds -> attaches audio AND leaves field text empty (short-circuited)!
        let audio_path = std::env::temp_dir().join(format!(
            "memento-anki-audio-text-fallback-{}-{}.mp3",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(&audio_path, b"ID3ok").unwrap();
        let mut ctx_ok = NoteFieldsContext::new("食べる", "たべる", "パンを食べる。");
        let input_ok = NoteMediaInput {
            media: None,
            subtitle_start: 0.0,
            subtitle_end: 0.0,
            context_start: 0.0,
            context_end: 0.0,
            screenshot: None,
            screenshot_video: None,
            subtitle_id: 0,
            audio_sources: vec![AudioSourceEntry {
                name: "Local".into(),
                url: audio_path.to_string_lossy().into_owned(),
                kind: memento_setting::enums::AudioSourceType::AudioSourceTypeFile as i64,
                md5: String::new(),
            }],
        };
        let res_ok = prepare_note_media(&profile, &mut ctx_ok, input_ok);
        let _ = std::fs::remove_file(audio_path);
        res_ok.unwrap();
        assert_eq!(ctx_ok.audio_attachments.len(), 1);
        let payload_ok = build_note_payload_from_profile(&profile, &ctx_ok).unwrap();
        assert_eq!(payload_ok.fields["Front"], "");
    }
}

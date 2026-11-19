//! Capture the media requested by configured Anki field markers.

use std::ops::Deref;

use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::PngEncoder;
use image::codecs::webp::WebPEncoder;
use image::{ExtendedColorType, ImageBuffer, ImageEncoder, Rgba};
use memento_anki::note::{
    audio_context_params, audio_media_params, compute_clip_range, screenshot_params, video_params,
    AudioMediaParams, ScreenshotParams, VideoParams,
};
use memento_anki::tokenizer::{tokenize, Marker};
use memento_player::RawFrame;
use serde_json::json;

use super::anki::{
    encode_base64, evaluate_template_marker_kind, MarkerEvalKind, NoteFieldsContext,
    RenderedTemplateData,
};
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
/// whitespace and alternatives such as `{screenshot-video | screenshot}`.
/// Inspects every valid alternative in each token so fallback screenshot kinds
/// are captured whenever an earlier alternative (e.g. `screenshot-video`) may
/// be unavailable.
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
                    }
                    "screenshot-video" if screenshot_params(&marker).is_some() => {
                        video_pictures = true;
                    }
                    _ => {}
                }
                if pictures && video_pictures {
                    return (true, true);
                }
            }
        }
    }
    (pictures, video_pictures)
}

fn media_filename(bytes: &[u8], extension: &str) -> Result<String, String> {
    let md5 = memento_util::fileutils::calculate_md5(bytes)
        .map_err(|error| format!("Could not hash Anki media: {error}"))?;
    let raw = format!("{:032x}{extension}", u128::from_be_bytes(md5));
    Ok(sanitize_anki_filename(&raw, extension))
}

fn resize_dimensions(width: u32, height: u32, max_width: u32, max_height: u32) -> (u32, u32) {
    let wratio = f64::from(max_width) / f64::from(width);
    let hratio = f64::from(max_height) / f64::from(height);
    let ratio = f64::min(wratio, hratio);
    let nw = (f64::from(width) * ratio)
        .round()
        .clamp(1.0, f64::from(u32::MAX)) as u32;
    let nh = (f64::from(height) * ratio)
        .round()
        .clamp(1.0, f64::from(u32::MAX)) as u32;
    (nw, nh)
}

fn encode_rgba_buffer<C>(
    image: &ImageBuffer<Rgba<u8>, C>,
    format: i32,
) -> Result<(Vec<u8>, &'static str), String>
where
    C: Deref<Target = [u8]>,
{
    let raw: &[u8] = Deref::deref(image.as_raw());
    let mut output = Vec::new();
    let (result, extension) = match format {
        1 => (
            PngEncoder::new(&mut output).write_image(
                raw,
                image.width(),
                image.height(),
                ExtendedColorType::Rgba8,
            ),
            ".png",
        ),
        2 => (
            WebPEncoder::new_lossless(&mut output).encode(
                raw,
                image.width(),
                image.height(),
                ExtendedColorType::Rgba8,
            ),
            ".webp",
        ),
        _ => (JpegEncoder::new(&mut output).encode_image(image), ".jpg"),
    };
    result.map_err(|error| format!("Could not encode Anki screenshot: {error}"))?;
    Ok((output, extension))
}

fn encode_picture(
    frame: &RawFrame,
    params: ScreenshotParams,
    format: i32,
) -> Result<(Vec<u8>, &'static str), String> {
    if frame.width == 0 || frame.height == 0 {
        return Err("Video screenshot has invalid dimensions".to_owned());
    }
    let expected_len = (frame.width as usize)
        .checked_mul(frame.height as usize)
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or("Video screenshot has invalid dimensions")?;
    let pixels = frame
        .pixels
        .get(..expected_len)
        .ok_or("Video screenshot has invalid dimensions")?;
    let view = ImageBuffer::<Rgba<u8>, &[u8]>::from_raw(frame.width, frame.height, pixels)
        .ok_or("Video screenshot has invalid dimensions")?;

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
        let (target_w, target_h) = if params.keep_aspect_ratio {
            resize_dimensions(frame.width, frame.height, max_width, max_height)
        } else {
            (max_width, max_height)
        };
        let resized = image::imageops::resize(
            &view,
            target_w,
            target_h,
            image::imageops::FilterType::Lanczos3,
        );
        encode_rgba_buffer(&resized, format)
    } else {
        encode_rgba_buffer(&view, format)
    }
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

#[derive(Clone, Copy, Debug, PartialEq)]
enum MediaTaskKey {
    WordAudio,
    AudioClip(AudioMediaParams),
    Picture(bool, ScreenshotParams),
    VideoClip(VideoParams),
}

type MediaResult = Result<(String, String), String>;

#[derive(Default)]
struct MediaCaches {
    word_audio: Option<MediaResult>,
    clips: Vec<(AudioMediaParams, Option<MediaResult>)>,
    pictures: Vec<((bool, ScreenshotParams), Option<MediaResult>)>,
    videos: Vec<(VideoParams, Option<MediaResult>)>,
}

impl MediaCaches {
    fn get(&self, key: &MediaTaskKey) -> Option<&MediaResult> {
        match key {
            MediaTaskKey::WordAudio => self.word_audio.as_ref(),
            MediaTaskKey::AudioClip(params) => self
                .clips
                .iter()
                .find(|(existing, _)| existing == params)
                .and_then(|(_, res)| res.as_ref()),
            MediaTaskKey::Picture(video_only, params) => self
                .pictures
                .iter()
                .find(|(existing, _)| *existing == (*video_only, *params))
                .and_then(|(_, res)| res.as_ref()),
            MediaTaskKey::VideoClip(params) => self
                .videos
                .iter()
                .find(|(existing, _)| existing == params)
                .and_then(|(_, res)| res.as_ref()),
        }
    }

    fn insert(&mut self, key: MediaTaskKey, res: MediaResult) {
        match key {
            MediaTaskKey::WordAudio => self.word_audio = Some(res),
            MediaTaskKey::AudioClip(params) => {
                if let Some((_, slot)) = self
                    .clips
                    .iter_mut()
                    .find(|(existing, _)| *existing == params)
                {
                    *slot = Some(res);
                } else {
                    self.clips.push((params, Some(res)));
                }
            }
            MediaTaskKey::Picture(video_only, params) => {
                let pic_key = (video_only, params);
                if let Some((_, slot)) = self
                    .pictures
                    .iter_mut()
                    .find(|(existing, _)| *existing == pic_key)
                {
                    *slot = Some(res);
                } else {
                    self.pictures.push((pic_key, Some(res)));
                }
            }
            MediaTaskKey::VideoClip(params) => {
                if let Some((_, slot)) = self
                    .videos
                    .iter_mut()
                    .find(|(existing, _)| *existing == params)
                {
                    *slot = Some(res);
                } else {
                    self.videos.push((params, Some(res)));
                }
            }
        }
    }
}

fn parse_media_task_key(
    name: &str,
    marker: &Marker,
    profile: &AnkiNoteConfig,
) -> Option<MediaTaskKey> {
    match name {
        "audio" => Some(MediaTaskKey::WordAudio),
        "audio-context" => audio_context_params(marker, profile.audio_normalize, profile.audio_db)
            .map(MediaTaskKey::AudioClip),
        "audio-media" | "sentence-audio" => {
            audio_media_params(marker, profile.audio_normalize, profile.audio_db)
                .map(MediaTaskKey::AudioClip)
        }
        "screenshot" | "image" => {
            screenshot_params(marker).map(|params| MediaTaskKey::Picture(false, params))
        }
        "screenshot-video" => {
            screenshot_params(marker).map(|params| MediaTaskKey::Picture(true, params))
        }
        "video" => video_params(marker, profile.audio_normalize, profile.audio_db)
            .map(MediaTaskKey::VideoClip),
        _ => None,
    }
}

fn invalid_media_marker_error(name: &str, field: &str) -> String {
    match name {
        "video" => format!("Invalid {{video}} marker in Anki field {field}"),
        _ => format!("Invalid {{{name}}} marker in Anki field {field}"),
    }
}

fn execute_media_task(
    task: MediaTaskKey,
    profile: &AnkiNoteConfig,
    expression: &str,
    reading: &str,
    input: &NoteMediaInput,
) -> MediaResult {
    match task {
        MediaTaskKey::WordAudio => {
            let (data, raw_filename) = fetch_word_audio(expression, reading, &input.audio_sources)
                .map_err(|error| format!("Could not capture word audio: {error}"))?;
            let encoded = encode_base64(&data)?;
            let safe_filename = sanitize_anki_filename(&raw_filename, ".mp3");
            Ok((encoded, safe_filename))
        }
        MediaTaskKey::AudioClip(params) => {
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
            let data = extract_clip_audio(media, start, end, params.normalize.then_some(params.db))?;
            Ok((encode_base64(&data)?, media_filename(&data, ".aac")?))
        }
        MediaTaskKey::Picture(is_video_only, params) => {
            let frame = if is_video_only {
                input.screenshot_video.as_ref()
            } else {
                input.screenshot.as_ref()
            }
            .ok_or("Could not capture the video screenshot for Anki")?;
            let (data, extension) = encode_picture(frame, params, profile.screenshot_type)?;
            Ok((encode_base64(&data)?, media_filename(&data, extension)?))
        }
        MediaTaskKey::VideoClip(params) => {
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
        }
    }
}

/// Fill AnkiConnect's media arrays before `addNote`. Independent media capture
/// tasks (word audio HTTP fetch, AAC clip extraction, screenshot encoding, MP4
/// clip extraction) run concurrently via `std::thread::scope`, while `{a | b}`
/// alternatives are resolved sequentially so primary successes short-circuit
/// fallbacks and results merge deterministically into `ctx`.
pub fn prepare_note_media(
    profile: &AnkiNoteConfig,
    ctx: &mut NoteFieldsContext,
    input: NoteMediaInput,
) -> Result<(), String> {
    let rendered = RenderedTemplateData::for_templates(&profile.field_templates, ctx);

    let mut sorted_fields: Vec<(&String, Vec<_>)> = profile
        .field_templates
        .iter()
        .map(|(field, template)| {
            (
                field,
                tokenize(&template.encode_utf16().collect::<Vec<_>>()),
            )
        })
        .collect();
    sorted_fields.sort_by_key(|(name, _)| *name);

    // Wavefront concurrent execution: each wave collects the first unexecuted
    // media task needed by every unresolved token across all fields and runs
    // independent tasks concurrently via `std::thread::scope`. If any primary
    // task fails and a token has a fallback media marker, the next wave runs
    // those fallbacks concurrently too.
    let mut caches = MediaCaches::default();
    let expression = ctx.expression.as_str();
    let reading = ctx.reading.as_str();

    loop {
        let mut pending: Vec<MediaTaskKey> = Vec::new();
        for (_, tokens) in &sorted_fields {
            for token in tokens {
                for marker in &token.markers {
                    match evaluate_template_marker_kind(marker, ctx, &rendered) {
                        MarkerEvalKind::NonEmptyText => break,
                        MarkerEvalKind::EmptyText | MarkerEvalKind::Unknown => continue,
                        MarkerEvalKind::Media => {}
                    }
                    let name = String::from_utf16_lossy(&marker.name);
                    let Some(task_key) = parse_media_task_key(&name, marker, profile) else {
                        continue;
                    };
                    match caches.get(&task_key) {
                        Some(Ok(_)) => break,
                        Some(Err(_)) => continue,
                        None => {
                            if !pending.contains(&task_key) {
                                pending.push(task_key);
                            }
                            break;
                        }
                    }
                }
            }
        }
        if pending.is_empty() {
            break;
        }
        let completed: Vec<(MediaTaskKey, MediaResult)> = if pending.len() == 1 {
            let task = pending[0];
            vec![(
                task,
                execute_media_task(task, profile, expression, reading, &input),
            )]
        } else {
            std::thread::scope(|scope| {
                let handles: Vec<_> = pending
                    .into_iter()
                    .map(|task| {
                        let input_ref = &input;
                        let handle = scope.spawn(move || {
                            execute_media_task(task, profile, expression, reading, input_ref)
                        });
                        (task, handle)
                    })
                    .collect();
                handles
                    .into_iter()
                    .map(|(task, handle)| {
                        let res = handle
                            .join()
                            .unwrap_or_else(|_| Err("Anki media capture worker panicked".to_owned()));
                        (task, res)
                    })
                    .collect()
            })
        };
        for (task, result) in completed {
            caches.insert(task, result);
        }
    }

    // Deterministic merge pass matching sequential `{a | b}` evaluation semantics.
    let mut word_audio_fields: Vec<String> = Vec::new();
    let mut clip_fields: Vec<Vec<String>> = vec![Vec::new(); caches.clips.len()];
    let mut clip_order: Vec<usize> = Vec::new();
    let mut picture_fields: Vec<Vec<String>> = vec![Vec::new(); caches.pictures.len()];
    let mut picture_order: Vec<usize> = Vec::new();
    let mut video_fields: Vec<Vec<String>> = vec![Vec::new(); caches.videos.len()];
    let mut video_order: Vec<usize> = Vec::new();

    for (field, tokens) in &sorted_fields {
        for token in tokens {
            let mut token_media_error: Option<String> = None;
            let mut token_handled_fallback = false;
            let mut token_succeeded = false;

            for marker in &token.markers {
                match evaluate_template_marker_kind(marker, ctx, &rendered) {
                    MarkerEvalKind::NonEmptyText => {
                        token_succeeded = true;
                        break;
                    }
                    MarkerEvalKind::EmptyText => {
                        if token_media_error.is_some() {
                            token_handled_fallback = true;
                        }
                        continue;
                    }
                    MarkerEvalKind::Unknown => {
                        continue;
                    }
                    MarkerEvalKind::Media => {}
                }

                let name = String::from_utf16_lossy(&marker.name);
                let Some(task_key) = parse_media_task_key(&name, marker, profile) else {
                    token_media_error = Some(invalid_media_marker_error(&name, field));
                    token_handled_fallback = false;
                    continue;
                };
                match caches.get(&task_key) {
                    Some(Ok(_)) => {
                        match task_key {
                            MediaTaskKey::WordAudio => {
                                add_unique_field(&mut word_audio_fields, field);
                            }
                            MediaTaskKey::AudioClip(params) => {
                                if let Some(idx) = caches
                                    .clips
                                    .iter()
                                    .position(|(existing, _)| *existing == params)
                                {
                                    if clip_fields[idx].is_empty() {
                                        clip_order.push(idx);
                                    }
                                    add_unique_field(&mut clip_fields[idx], field);
                                }
                            }
                            MediaTaskKey::Picture(video_only, params) => {
                                let key = (video_only, params);
                                if let Some(idx) = caches
                                    .pictures
                                    .iter()
                                    .position(|(existing, _)| *existing == key)
                                {
                                    if picture_fields[idx].is_empty() {
                                        picture_order.push(idx);
                                    }
                                    add_unique_field(&mut picture_fields[idx], field);
                                }
                            }
                            MediaTaskKey::VideoClip(params) => {
                                if let Some(idx) = caches
                                    .videos
                                    .iter()
                                    .position(|(existing, _)| *existing == params)
                                {
                                    if video_fields[idx].is_empty() {
                                        video_order.push(idx);
                                    }
                                    add_unique_field(&mut video_fields[idx], field);
                                }
                            }
                        }
                        record_resolved_media_marker(ctx, field, &name);
                        token_succeeded = true;
                        break;
                    }
                    Some(Err(error)) => {
                        token_media_error = Some(error.clone());
                        token_handled_fallback = false;
                    }
                    None => {}
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
        if let Some(Ok((data, filename))) = caches.word_audio.take() {
            ctx.audio_attachments.push(json!({
                "data": data,
                "filename": filename,
                "fields": word_audio_fields,
            }));
        }
    }

    for idx in clip_order {
        let fields = std::mem::take(&mut clip_fields[idx]);
        if !fields.is_empty() {
            if let Some(Ok((data, filename))) = caches.clips[idx].1.take() {
                ctx.audio_attachments.push(json!({
                    "data": data,
                    "filename": filename,
                    "fields": fields,
                }));
            }
        }
    }

    for idx in picture_order {
        let fields = std::mem::take(&mut picture_fields[idx]);
        if !fields.is_empty() {
            if let Some(Ok((data, filename))) = caches.pictures[idx].1.take() {
                ctx.picture_attachments.push(json!({
                    "data": data,
                    "filename": filename,
                    "fields": fields,
                }));
            }
        }
    }

    for idx in video_order {
        let fields = std::mem::take(&mut video_fields[idx]);
        if !fields.is_empty() {
            if let Some(Ok((data, filename))) = caches.videos[idx].1.take() {
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
    use crate::controller::config::{anki_note_preset_fields, AnkiNotePreset};
    use image::ImageFormat;

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

        // Resizing also works without cloning `frame.pixels`.
        let wide_frame = RawFrame {
            width: 8,
            height: 4,
            pixels: vec![128; 8 * 4 * 4],
        };
        let (resized_keep, _) = encode_picture(
            &wide_frame,
            ScreenshotParams {
                max_width: 4,
                max_height: 4,
                keep_aspect_ratio: true,
            },
            1,
        )
        .unwrap();
        let decoded_keep =
            image::load_from_memory_with_format(&resized_keep, ImageFormat::Png).unwrap();
        assert_eq!((decoded_keep.width(), decoded_keep.height()), (4, 2));

        let (resized_exact, _) = encode_picture(
            &wide_frame,
            ScreenshotParams {
                max_width: 4,
                max_height: 4,
                keep_aspect_ratio: false,
            },
            1,
        )
        .unwrap();
        let decoded_exact =
            image::load_from_memory_with_format(&resized_exact, ImageFormat::Png).unwrap();
        assert_eq!((decoded_exact.width(), decoded_exact.height()), (4, 4));
    }

    #[test]
    fn requested_screenshots_captures_fallback_across_all_five_builtin_presets() {
        for preset in [
            AnkiNotePreset::Lapis,
            AnkiNotePreset::Kiku,
            AnkiNotePreset::Senren,
            AnkiNotePreset::Animecards,
            AnkiNotePreset::Jpmn,
        ] {
            let fields = anki_note_preset_fields(preset)
                .into_iter()
                .map(|(k, v)| (k.to_owned(), v.to_owned()))
                .collect();
            let profile = AnkiNoteConfig {
                enabled: true,
                endpoint: memento_net::anki::AnkiEndpoint::default(),
                deck: "Japanese".into(),
                model: "Preset".into(),
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
            assert_eq!(
                requested_screenshots(&profile),
                (true, true),
                "preset {preset:?} with {{screenshot-video | screenshot}} must request both frames"
            );
        }

        let make_single = |tmpl: &str| AnkiNoteConfig {
            enabled: true,
            endpoint: memento_net::anki::AnkiEndpoint::default(),
            deck: "Japanese".into(),
            model: "Basic".into(),
            field_templates: std::collections::HashMap::from([("Picture".into(), tmpl.into())]),
            tags: Vec::new(),
            duplicate_policy: 1,
            screenshot_type: 0,
            audio_pad_start: 0.0,
            audio_pad_end: 0.0,
            audio_normalize: false,
            audio_db: -20.0,
            newline_replacer: "<br>".into(),
        };
        assert_eq!(requested_screenshots(&make_single("{screenshot}")), (true, false));
        assert_eq!(
            requested_screenshots(&make_single("{screenshot-video}")),
            (false, true)
        );
        assert_eq!(
            requested_screenshots(&make_single("{screenshot:max-width=bad}")),
            (false, false)
        );
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
        // Both alternatives in `{screenshot-video | image}` are requested so fallback works.
        assert_eq!(requested_screenshots(&profile), (true, true));

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
    fn concurrent_audio_and_picture_fallback_merge_deterministically() {
        let path = std::env::temp_dir().join(format!(
            "memento-anki-concurrent-{}-{}.mp3",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(&path, b"ID3concurrent-audio").unwrap();
        let profile = AnkiNoteConfig {
            enabled: true,
            endpoint: memento_net::anki::AnkiEndpoint::default(),
            deck: "Japanese".into(),
            model: "Lapis".into(),
            field_templates: std::collections::HashMap::from([
                ("ExpressionAudio".into(), "{audio}".into()),
                ("Picture".into(), "{screenshot-video | screenshot}".into()),
            ]),
            tags: Vec::new(),
            duplicate_policy: 1,
            screenshot_type: 1,
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
            screenshot: Some(RawFrame {
                width: 2,
                height: 2,
                pixels: vec![200; 16],
            }),
            screenshot_video: None,
            subtitle_id: 0,
            audio_sources: vec![AudioSourceEntry {
                name: "Local".into(),
                url: path.to_string_lossy().into_owned(),
                kind: memento_setting::enums::AudioSourceType::AudioSourceTypeFile as i64,
                md5: String::new(),
            }],
        };
        let mut ctx = NoteFieldsContext::new("食べる", "たべる", "食べる。");
        let res = prepare_note_media(&profile, &mut ctx, input);
        let _ = std::fs::remove_file(path);
        res.unwrap();
        assert_eq!(ctx.audio_attachments.len(), 1);
        assert_eq!(ctx.audio_attachments[0]["fields"][0], "ExpressionAudio");
        assert_eq!(ctx.picture_attachments.len(), 1);
        assert_eq!(ctx.picture_attachments[0]["fields"][0], "Picture");
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

    #[test]
    fn empty_text_before_failed_media_returns_error_while_empty_text_after_handles_fallback() {
        let make_profile = |template: &str| AnkiNoteConfig {
            enabled: true,
            endpoint: memento_net::anki::AnkiEndpoint::default(),
            deck: "Japanese".into(),
            model: "Basic".into(),
            field_templates: std::collections::HashMap::from([
                ("Expression".into(), "{expression}".into()),
                ("Field".into(), template.into()),
            ]),
            tags: Vec::new(),
            duplicate_policy: 1,
            screenshot_type: 0,
            audio_pad_start: 0.0,
            audio_pad_end: 0.0,
            audio_normalize: false,
            audio_db: -20.0,
            newline_replacer: "<br>".into(),
        };
        let empty_input = || NoteMediaInput {
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

        // 1. `{selection | audio}` with empty `selection` and failing `audio` MUST return error.
        let mut ctx1 = NoteFieldsContext::new("食べる", "たべる", "パンを食べる。");
        let err1 = prepare_note_media(
            &make_profile("{selection | audio}"),
            &mut ctx1,
            empty_input(),
        )
        .expect_err("{selection | audio} must fail when selection is empty and audio fails");
        assert!(err1.contains("word audio"), "unexpected error: {err1}");

        // 2. `{context-2 | screenshot}` with empty `context-2` and failing `screenshot` MUST return error.
        let mut ctx2 = NoteFieldsContext::new("食べる", "たべる", "パンを食べる。");
        let err2 = prepare_note_media(
            &make_profile("{context-2 | screenshot}"),
            &mut ctx2,
            empty_input(),
        )
        .expect_err(
            "{context-2 | screenshot} must fail when context-2 is empty and screenshot fails",
        );
        assert!(err2.contains("screenshot"), "unexpected error: {err2}");

        // 3. `{audio | selection}` with failing `audio` followed by empty `selection` handles fallback cleanly.
        let mut ctx3 = NoteFieldsContext::new("食べる", "たべる", "パンを食べる。");
        prepare_note_media(
            &make_profile("{audio | selection}"),
            &mut ctx3,
            empty_input(),
        )
        .expect("{audio | selection} should fall back to empty selection after audio failure");
        let payload3 =
            build_note_payload_from_profile(&make_profile("{audio | selection}"), &ctx3).unwrap();
        assert_eq!(payload3.fields["Field"], "");

        // 4. `{audio | selection | screenshot}` where final media fallback also fails MUST return error.
        let mut ctx4 = NoteFieldsContext::new("食べる", "たべる", "パンを食べる。");
        let err4 = prepare_note_media(
            &make_profile("{audio | selection | screenshot}"),
            &mut ctx4,
            empty_input(),
        )
        .expect_err("trailing failed screenshot after empty selection must return error");
        assert!(err4.contains("screenshot"), "unexpected error: {err4}");
    }
}

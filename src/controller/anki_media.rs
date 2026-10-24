//! Capture the media requested by configured Anki field markers.

use std::io::Cursor;

use image::{DynamicImage, ImageFormat, RgbaImage};
use memento_anki::note::{
    audio_context_params, audio_media_params, compute_clip_range, screenshot_params,
    video_params, AudioMediaParams, ScreenshotParams, VideoParams,
};
use memento_anki::tokenizer::tokenize;
use memento_player::RawFrame;
use serde_json::json;

use super::anki::NoteFieldsContext;
use super::media::{extract_clip_mp4, extract_clip_wav, MediaContext};
use super::audio::fetch_word_audio;
use super::config::{AnkiNoteConfig, AudioSourceEntry};

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
pub fn requested_screenshots(profile: &AnkiNoteConfig) -> (bool, bool) {
    let mut pictures = false;
    let mut video_pictures = false;
    for template in profile.field_templates.values() {
        for token in tokenize(&template.encode_utf16().collect::<Vec<_>>()) {
            for marker in token.markers {
                match String::from_utf16_lossy(&marker.name).as_ref() {
                    "screenshot" | "image" => pictures = true,
                    "screenshot-video" => video_pictures = true,
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
    Ok(format!("{:032x}{extension}", u128::from_be_bytes(md5)))
}

fn encode_picture(frame: &RawFrame, params: ScreenshotParams, format: i32) -> Result<(Vec<u8>, &'static str), String> {
    let rgba = RgbaImage::from_raw(frame.width, frame.height, frame.pixels.clone())
        .ok_or("Video screenshot has invalid dimensions")?;
    let mut image = DynamicImage::ImageRgba8(rgba);
    let max_width = if params.max_width > 0 { params.max_width as u32 } else { frame.width };
    let max_height = if params.max_height > 0 { params.max_height as u32 } else { frame.height };
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
    image.write_to(&mut output, image_format)
        .map_err(|error| format!("Could not encode Anki screenshot: {error}"))?;
    Ok((output.into_inner(), extension))
}

fn grouped_audio(groups: &mut Vec<(AudioMediaParams, Vec<String>)>, params: AudioMediaParams, field: &str) {
    if let Some((_, fields)) = groups.iter_mut().find(|(existing, _)| *existing == params) {
        if !fields.iter().any(|name| name == field) { fields.push(field.to_owned()); }
    } else {
        groups.push((params, vec![field.to_owned()]));
    }
}

fn grouped_picture(groups: &mut Vec<(ScreenshotParams, Vec<String>)>, params: ScreenshotParams, field: &str) {
    if let Some((_, fields)) = groups.iter_mut().find(|(existing, _)| *existing == params) {
        if !fields.iter().any(|name| name == field) { fields.push(field.to_owned()); }
    } else {
        groups.push((params, vec![field.to_owned()]));
    }
}

fn grouped_video(groups: &mut Vec<(VideoParams, Vec<String>)>, params: VideoParams, field: &str) {
    if let Some((_, fields)) = groups.iter_mut().find(|(existing, _)| *existing == params) {
        if !fields.iter().any(|name| name == field) { fields.push(field.to_owned()); }
    } else {
        groups.push((params, vec![field.to_owned()]));
    }
}

/// Fill AnkiConnect's media arrays before `addNote`. A requested
/// media marker that cannot be captured is reported instead of silently
/// creating a card with an empty field.
pub fn prepare_note_media(profile: &AnkiNoteConfig, ctx: &mut NoteFieldsContext, input: NoteMediaInput) -> Result<(), String> {
    let mut word_audio_fields = Vec::new();
    let mut clips = Vec::new();
    let mut pictures = Vec::new();
    let mut video_pictures = Vec::new();
    let mut videos = Vec::new();

    for (field, template) in &profile.field_templates {
        for token in tokenize(&template.encode_utf16().collect::<Vec<_>>()) {
            for marker in token.markers {
                let name = String::from_utf16_lossy(&marker.name);
                match name.as_str() {
                    "audio" => {
                        if !word_audio_fields.contains(field) { word_audio_fields.push(field.clone()); }
                    }
                    "audio-media" | "audio-context" | "sentence-audio" => {
                        let params = if name == "audio-context" {
                            audio_context_params(&marker, profile.audio_normalize, profile.audio_db)
                        } else {
                            audio_media_params(&marker, profile.audio_normalize, profile.audio_db)
                        }.ok_or_else(|| format!("Invalid {{{name}}} marker in Anki field {field}"))?;
                        grouped_audio(&mut clips, params, field);
                    }
                    "screenshot" | "screenshot-video" | "image" => {
                        let params = screenshot_params(&marker)
                            .ok_or_else(|| format!("Invalid {{{name}}} marker in Anki field {field}"))?;
                        grouped_picture(if name == "screenshot-video" { &mut video_pictures } else { &mut pictures }, params, field);
                    }
                    "video" => {
                        let params = video_params(&marker, profile.audio_normalize, profile.audio_db)
                            .ok_or_else(|| format!("Invalid {{video}} marker in Anki field {field}"))?;
                        grouped_video(&mut videos, params, field);
                    }
                    _ => {}
                }
            }
        }
    }

    if !word_audio_fields.is_empty() {
        let (data, filename) = fetch_word_audio(&ctx.expression, &ctx.reading, &input.audio_sources)
            .map_err(|error| format!("Could not capture word audio: {error}"))?;
        ctx.audio_attachments.push(json!({
            "data": encode_base64(&data)?, "filename": filename, "fields": word_audio_fields,
        }));
    }

    for (params, fields) in clips {
        let media = input.media.as_ref().ok_or("No video is loaded for sentence audio")?;
        let (start, end) = compute_clip_range(
            params.source,
            input.subtitle_start, input.subtitle_end,
            input.context_start, input.context_end,
            profile.audio_pad_start, profile.audio_pad_end,
        ).ok_or("The selected subtitle has no usable audio time range")?;
        let data = extract_clip_wav(media, start, end, params.normalize.then_some(params.db))?;
        ctx.audio_attachments.push(json!({
            "data": encode_base64(&data)?,
            "filename": media_filename(&data, ".wav")?,
            "fields": fields,
        }));
    }

    for (groups, frame) in [(&pictures, &input.screenshot), (&video_pictures, &input.screenshot_video)] {
        for (params, fields) in groups {
            let frame = frame.as_ref().ok_or("Could not capture the video screenshot for Anki")?;
            let (data, extension) = encode_picture(frame, *params, profile.screenshot_type)?;
            ctx.picture_attachments.push(json!({
                "data": encode_base64(&data)?,
                "filename": media_filename(&data, extension)?,
                "fields": fields,
            }));
        }
    }
    for (params, fields) in videos {
        let media = input.media.as_ref().ok_or("No video is loaded for an Anki video clip")?;
        let (start, end) = compute_clip_range(
            params.source,
            input.subtitle_start, input.subtitle_end,
            input.context_start, input.context_end,
            profile.audio_pad_start, profile.audio_pad_end,
        ).ok_or("The selected subtitle has no usable video time range")?;
        let data = extract_clip_mp4(
            media, input.subtitle_id, start, end,
            params.audio, params.subtitles,
            params.normalize.then_some(params.db),
        )?;
        ctx.video_attachments.push(json!({
            "data": encode_base64(&data)?,
            "filename": media_filename(&data, ".mp4")?,
            "fields": fields,
        }));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn screenshot_profile_formats_produce_real_images() {
        let frame = RawFrame {
            width: 2,
            height: 2,
            pixels: vec![255, 0, 0, 255, 0, 255, 0, 255,
                         0, 0, 255, 255, 255, 255, 255, 255],
        };
        let params = ScreenshotParams { max_width: -1, max_height: -1, keep_aspect_ratio: true };
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
    fn picture_marker_attaches_capture_to_configured_field() {
        let mut fields = std::collections::HashMap::new();
        fields.insert("Picture".into(), "{ screenshot-video : max-width=1 | image : max-width=1 }".into());
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
        assert_eq!(requested_screenshots(&profile), (true, true));
        let frame = RawFrame { width: 2, height: 2, pixels: vec![255; 16] };
        let mut ctx = NoteFieldsContext::new("日本語", "にほんご", "日本語を勉強する");
        let input = NoteMediaInput {
            media: None,
            subtitle_start: 0.0,
            subtitle_end: 0.0,
            context_start: 0.0,
            context_end: 0.0,
            screenshot: Some(frame.clone()),
            screenshot_video: Some(frame),
            subtitle_id: 0,
            audio_sources: Vec::new(),
        };
        prepare_note_media(&profile, &mut ctx, input).unwrap();
        assert_eq!(ctx.picture_attachments.len(), 2);
        assert_eq!(ctx.picture_attachments[0]["fields"][0], "Picture");
        assert!(ctx.picture_attachments[0]["filename"].as_str().unwrap().ends_with(".png"));
    }

    #[test]
    fn word_audio_is_attached_as_data_for_anki() {
        let path = std::env::temp_dir().join(format!("memento-anki-word-audio-{}-{}.mp3",
            std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::write(&path, b"ID3test-audio").unwrap();
        let mut fields = std::collections::HashMap::new();
        fields.insert("ExpressionAudio".into(), "{audio}".into());
        let profile = AnkiNoteConfig {
            enabled: true, endpoint: memento_net::anki::AnkiEndpoint::default(),
            deck: "Japanese".into(), model: "Lapis".into(), field_templates: fields,
            tags: Vec::new(), duplicate_policy: 1, screenshot_type: 0,
            audio_pad_start: 0.0, audio_pad_end: 0.0, audio_normalize: false,
            audio_db: -20.0, newline_replacer: "<br>".into(),
        };
        let input = NoteMediaInput {
            media: None, subtitle_start: 0.0, subtitle_end: 0.0,
            context_start: 0.0, context_end: 0.0, screenshot: None,
            screenshot_video: None, subtitle_id: 0,
            audio_sources: vec![AudioSourceEntry {
                name: "Local".into(), url: path.to_string_lossy().into_owned(),
                kind: memento_setting::enums::AudioSourceType::AudioSourceTypeFile as i64,
                md5: String::new(),
            }],
        };
        let mut ctx = NoteFieldsContext::new("休み", "やすみ", "今日は休み。");
        let result = prepare_note_media(&profile, &mut ctx, input);
        let _ = std::fs::remove_file(path);
        result.unwrap();
        assert_eq!(ctx.audio_attachments.len(), 1);
        assert!(ctx.audio_attachments[0]["data"].as_str().is_some_and(|data| !data.is_empty()));
        assert!(ctx.audio_attachments[0].get("url").is_none());
        assert_eq!(ctx.audio_attachments[0]["fields"][0], "ExpressionAudio");
    }
}

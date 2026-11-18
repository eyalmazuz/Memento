//! Background media extraction shared by Anki and optional speech recognition.

#[cfg(any(feature = "asr-whisper", feature = "asr-onnx", test))]
use memento_asr::audio::wavreader::{read_pcm16_wav_bytes, SAMPLE_RATE_16K};
use memento_player::mpv::ffi as mpv;
use std::ffi::{c_char, CStr, CString};
use std::fs;
#[cfg(all(target_os = "linux", any(feature = "asr-whisper", feature = "asr-onnx", test)))]
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use tempfile::TempDir;

/// An immutable snapshot of the media and selected mpv audio track. Capture it
/// on the UI thread with `MpvInstance::capture_encode_context` before starting.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MediaContext {
    pub path: String,
    pub audio_id: i64,
}

impl MediaContext {
    pub fn new(path: impl Into<String>, audio_id: i64) -> Self {
        Self {
            path: path.into(),
            audio_id,
        }
    }
}

pub(super) struct TempMediaFile {
    pub(super) path: PathBuf,
    _directory: TempDir,
}

fn create_temp_media_path(
    prefix: &str,
    file_name: &str,
    context: &str,
) -> Result<TempMediaFile, String> {
    let directory = tempfile::Builder::new()
        .prefix(prefix)
        .tempdir()
        .map_err(|error| format!("Could not create {context}: {error}"))?;
    let path = directory.path().join(file_name);
    Ok(TempMediaFile {
        path,
        _directory: directory,
    })
}

fn create_temp_wav_path() -> Result<TempMediaFile, String> {
    create_temp_media_path("memento-audio-", "window.wav", "temporary audio directory")
}

fn create_temp_aac_path() -> Result<TempMediaFile, String> {
    create_temp_media_path(
        "memento-anki-audio-",
        "clip.aac",
        "temporary Anki audio directory",
    )
}

fn create_temp_mp4_path() -> Result<TempMediaFile, String> {
    create_temp_media_path(
        "memento-anki-video-",
        "clip.mp4",
        "temporary Anki video directory",
    )
}

struct MpvHandle(*mut mpv::mpv_handle);

impl Drop for MpvHandle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { mpv::mpv_terminate_destroy(self.0) };
        }
    }
}

fn mpv_error(status: i32) -> String {
    let message = unsafe { mpv::mpv_error_string(status) };
    if message.is_null() {
        format!("libmpv error {status}")
    } else {
        format!(
            "libmpv error: {}",
            unsafe { CStr::from_ptr(message) }.to_string_lossy()
        )
    }
}

fn validate_audio_clip_args(media: &MediaContext, start: f64, end: f64) -> Result<(), String> {
    if media.path.is_empty()
        || media.audio_id <= 0
        || !start.is_finite()
        || !end.is_finite()
        || end <= start
    {
        return Err(
            "Could not extract audio: media path, selected audio track, or window is invalid."
                .into(),
        );
    }
    Ok(())
}

fn build_loadfile_options(
    start: f64,
    end: f64,
    audio_id: Option<i64>,
    subtitle_id: Option<i64>,
) -> String {
    let mut load_options = format!("start={start:.3},end={end:.3}");
    if let Some(aid) = audio_id {
        load_options.push_str(&format!(",aid={aid}"));
    }
    if let Some(sid) = subtitle_id {
        load_options.push_str(&format!(",sid={sid}"));
    }
    load_options
}

fn ytdl_flag(media_path: &str) -> &'static str {
    if media_path.contains("://") {
        "yes"
    } else {
        "no"
    }
}

/// Build libmpv encoder options for 16 kHz mono PCM16 WAV extraction used by ASR.
fn build_asr_audio_options(
    media: &MediaContext,
    output: &str,
    force_wav_format: bool,
) -> Result<Vec<(&'static str, String)>, String> {
    if media.path.is_empty() || media.audio_id <= 0 {
        return Err(
            "Could not extract audio: media path or selected audio track is invalid.".into(),
        );
    }
    let mut options = vec![
        ("vid", "no".to_owned()),
        ("aid", media.audio_id.to_string()),
        ("sid", "no".to_owned()),
        ("secondary-sid", "no".to_owned()),
        ("oac", "pcm_s16le".to_owned()),
        ("audio-samplerate", "16000".to_owned()),
        ("audio-channels", "mono".to_owned()),
        ("cover-art-auto", "no".to_owned()),
        ("keep-open", "no".to_owned()),
        ("config", "no".to_owned()),
        ("ytdl", ytdl_flag(&media.path).to_owned()),
        ("o", output.to_owned()),
    ];
    if force_wav_format {
        options.push(("of", "wav".to_owned()));
    }
    Ok(options)
}

/// Build libmpv encoder options for high-quality stereo `.aac` Anki audio clips
/// at the source's native sample rate (matching C++ `MpvAudioClipArgs::Preset::Default`),
/// or 16 kHz mono WAV when `wav_compat` is true.
fn build_anki_audio_clip_options(
    media: &MediaContext,
    output: &str,
    normalize_db: Option<f64>,
    wav_compat: bool,
) -> Result<Vec<(&'static str, String)>, String> {
    if media.path.is_empty() || media.audio_id <= 0 {
        return Err(
            "Could not extract audio: media path or selected audio track is invalid.".into(),
        );
    }
    let mut options = if wav_compat {
        build_asr_audio_options(media, output, false)?
    } else {
        vec![
            ("vid", "no".to_owned()),
            ("aid", media.audio_id.to_string()),
            ("sid", "no".to_owned()),
            ("secondary-sid", "no".to_owned()),
            ("oac", "aac".to_owned()),
            ("cover-art-auto", "no".to_owned()),
            ("keep-open", "no".to_owned()),
            ("config", "no".to_owned()),
            ("ytdl", ytdl_flag(&media.path).to_owned()),
            ("o", output.to_owned()),
        ]
    };
    if let Some(db) = normalize_db {
        if !db.is_finite() {
            return Err("Invalid Anki audio normalization dB".into());
        }
        options.push(("af", format!("loudnorm=I={db:.1}")));
    }
    Ok(options)
}

/// Build libmpv encoder options for Anki MP4 video clips with explicit `aid` and `sid`.
fn build_anki_video_clip_options(
    media: &MediaContext,
    subtitle_id: i64,
    audio: bool,
    subtitles: bool,
    normalize_db: Option<f64>,
    output: &str,
) -> Result<Vec<(&'static str, String)>, String> {
    if audio && media.audio_id <= 0 {
        return Err("Could not extract Anki video: no audio track is selected".into());
    }
    if subtitles && subtitle_id <= 0 {
        return Err("Could not extract Anki video with subtitles: no subtitle track is selected; use {video:subtitles=false} for this card".into());
    }
    let aid_value = if audio {
        media.audio_id.to_string()
    } else {
        "no".to_owned()
    };
    let sid_value = if subtitles {
        subtitle_id.to_string()
    } else {
        "no".to_owned()
    };
    let mut options = vec![
        ("ovc", "libx264".to_owned()),
        ("oac", "aac".to_owned()),
        ("vid", "auto".to_owned()),
        ("aid", aid_value),
        ("sid", sid_value),
        ("secondary-sid", "no".to_owned()),
        ("cover-art-auto", "no".to_owned()),
        ("keep-open", "no".to_owned()),
        ("config", "no".to_owned()),
        ("ytdl", ytdl_flag(&media.path).to_owned()),
        ("o", output.to_owned()),
    ];
    if let Some(db) = normalize_db.filter(|_| audio) {
        if !db.is_finite() {
            return Err("Invalid Anki video audio normalization dB".into());
        }
        options.push(("af", format!("loudnorm=I={db:.1}")));
    }
    Ok(options)
}

fn run_encode_job(
    media_path: &str,
    options: &[(&str, String)],
    load_options: &str,
    abort: Option<&AtomicBool>,
    context: &str,
) -> Result<(), String> {
    if abort.is_some_and(|flag| flag.load(Ordering::Acquire)) {
        return Err(format!("{context} canceled."));
    }
    let input = CString::new(media_path.as_bytes())
        .map_err(|_| "Media path contains an invalid NUL byte.".to_owned())?;
    let load_options_c = CString::new(load_options.as_bytes())
        .map_err(|_| format!("Invalid {context} options."))?;

    let handle = unsafe { mpv::mpv_create() };
    if handle.is_null() {
        return Err(format!("Could not create a libmpv {context} instance."));
    }
    let handle = MpvHandle(handle);

    for (name, value) in options {
        let name_c = CString::new(*name)
            .map_err(|_| format!("Invalid libmpv option name '{name}' for {context}"))?;
        let value_c = CString::new(value.as_bytes())
            .map_err(|_| format!("libmpv option '{name}' contains an invalid NUL byte"))?;
        let status =
            unsafe { mpv::mpv_set_option_string(handle.0, name_c.as_ptr(), value_c.as_ptr()) };
        if status < 0 {
            return Err(format!(
                "Could not configure libmpv {context} option {name}: {}",
                mpv_error(status)
            ));
        }
    }

    let status = unsafe { mpv::mpv_initialize(handle.0) };
    if status < 0 {
        return Err(format!(
            "Could not initialize libmpv {context}: {}",
            mpv_error(status)
        ));
    }
    if abort.is_some_and(|flag| flag.load(Ordering::Acquire)) {
        return Err(format!("{context} canceled."));
    }

    let api_version = unsafe { mpv::mpv_client_api_version() };
    // mpv_client_api_version() packs (major << 16) | minor; loadfile gained its
    // <index> argument in client API 2.3.
    let mut command: Vec<*const c_char> =
        vec![c"loadfile".as_ptr(), input.as_ptr(), c"replace".as_ptr()];
    if api_version >= ((2 << 16) | 3) {
        command.push(c"0".as_ptr());
    }
    command.extend([load_options_c.as_ptr(), ptr::null()]);
    let status = unsafe { mpv::mpv_command(handle.0, command.as_mut_ptr()) };
    if status < 0 {
        return Err(format!(
            "Could not start libmpv {context}: {}",
            mpv_error(status)
        ));
    }

    loop {
        if abort.is_some_and(|flag| flag.load(Ordering::Acquire)) {
            return Err(format!("{context} canceled."));
        }
        let event = unsafe { mpv::mpv_wait_event(handle.0, 0.1) };
        if event.is_null() {
            return Err(format!("libmpv returned no event during {context}."));
        }
        let event_id = unsafe { (*event).event_id };
        if event_id == mpv::MPV_EVENT_NONE {
            continue;
        }
        if event_id == mpv::MPV_EVENT_QUEUE_OVERFLOW {
            return Err(format!("libmpv {context} event queue overflowed."));
        }
        if event_id == mpv::MPV_EVENT_END_FILE {
            let end_file_ptr = unsafe { (*event).data as *const mpv::mpv_event_end_file };
            if end_file_ptr.is_null() {
                return Err(format!(
                    "libmpv returned an empty end-file event during {context}."
                ));
            }
            let end_file = unsafe { &*end_file_ptr };
            if end_file.reason == mpv::MPV_END_FILE_REASON_REDIRECT {
                continue;
            }
            if end_file.error < 0 {
                return Err(format!(
                    "libmpv failed {context} for '{media_path}': {}",
                    mpv_error(end_file.error)
                ));
            }
            if end_file.reason != mpv::MPV_END_FILE_REASON_EOF {
                return Err(format!(
                    "libmpv ended {context} early (reason {})",
                    end_file.reason
                ));
            }
            // Terminate and flush the encoder handle before inspecting the output.
            drop(handle);
            return Ok(());
        }
    }
}

/// Extract a high-quality stereo `.aac` audio clip at native sample rate for
/// an Anki note on a background thread (matching C++ `MpvAudioClipArgs::Preset::Default`).
pub fn extract_clip_audio(
    media: &MediaContext,
    start: f64,
    end: f64,
    normalize_db: Option<f64>,
) -> Result<Vec<u8>, String> {
    validate_audio_clip_args(media, start, end)?;
    let temporary = create_temp_aac_path()?;
    let output = temporary.path.to_string_lossy().into_owned();
    let options = build_anki_audio_clip_options(media, &output, normalize_db, false)?;
    let load_options = build_loadfile_options(start, end, Some(media.audio_id), None);
    run_encode_job(
        &media.path,
        &options,
        &load_options,
        None,
        "Anki audio clip encoding",
    )?;
    if !temporary.path.is_file() {
        return Err("libmpv did not create an Anki audio clip".into());
    }
    let bytes = fs::read(&temporary.path)
        .map_err(|error| format!("Could not read extracted note audio: {error}"))?;
    if bytes.is_empty() {
        return Err("libmpv produced an empty Anki audio clip".into());
    }
    Ok(bytes)
}

/// Extract a 16 kHz mono WAV clip for an Anki note on a background thread.
/// Kept compatible for `anki_media.rs`; prefer [`extract_clip_audio`] for new callers.
pub fn extract_clip_wav(
    media: &MediaContext,
    start: f64,
    end: f64,
    normalize_db: Option<f64>,
) -> Result<Vec<u8>, String> {
    let abort = AtomicBool::new(false);
    let wav = extract_audio(media, start, end, &abort, normalize_db)?;
    fs::read(&wav.path).map_err(|error| format!("Could not read extracted note audio: {error}"))
}

/// Encode an Anki video attachment without blocking the UI's mpv instance.
pub fn extract_clip_mp4(
    media: &MediaContext,
    subtitle_id: i64,
    start: f64,
    end: f64,
    audio: bool,
    subtitles: bool,
    normalize_db: Option<f64>,
) -> Result<Vec<u8>, String> {
    if media.path.is_empty() || !start.is_finite() || !end.is_finite() || end <= start {
        return Err("Could not extract Anki video: media path or clip range is invalid".into());
    }
    let temporary = create_temp_mp4_path()?;
    let output = temporary.path.to_string_lossy().into_owned();
    let options = build_anki_video_clip_options(
        media,
        subtitle_id,
        audio,
        subtitles,
        normalize_db,
        &output,
    )?;
    let load_options = build_loadfile_options(
        start,
        end,
        audio.then_some(media.audio_id),
        subtitles.then_some(subtitle_id),
    );
    run_encode_job(
        &media.path,
        &options,
        &load_options,
        None,
        "Anki video encoding",
    )?;
    if !temporary.path.is_file() {
        return Err("libmpv did not create an Anki video clip".into());
    }
    let bytes = fs::read(&temporary.path)
        .map_err(|error| format!("Could not read encoded Anki video: {error}"))?;
    if bytes.len() < 12 || &bytes[4..8] != b"ftyp" {
        return Err(format!(
            "libmpv produced an invalid Anki MP4 clip ({} bytes)",
            bytes.len()
        ));
    }
    Ok(bytes)
}

#[cfg(all(target_os = "linux", any(feature = "asr-whisper", feature = "asr-onnx", test)))]
fn create_memfd_wav() -> Option<(fs::File, String)> {
    use std::os::unix::io::{AsRawFd, FromRawFd};
    let fd = unsafe { libc::memfd_create(c"memento-asr-pcm".as_ptr(), libc::MFD_CLOEXEC) };
    if fd < 0 {
        return None;
    }
    let file = unsafe { fs::File::from_raw_fd(fd) };
    let path = format!("/proc/self/fd/{}", file.as_raw_fd());
    if std::path::Path::new(&path).exists() {
        Some((file, path))
    } else {
        None
    }
}

/// Extract 16 kHz mono `f32` PCM samples once into RAM for ASR (VAD + inference).
///
/// On Linux this encodes directly into an anonymous `memfd` (`of=wav` via
/// `/proc/self/fd/<fd>`) so zero audio bytes touch disk; on other platforms (or
/// if `memfd_create` is unavailable) it falls back to a temporary WAV file that
/// is read once and immediately unlinked before returning the decoded samples.
#[cfg(any(feature = "asr-whisper", feature = "asr-onnx", test))]
pub(super) fn extract_audio_samples(
    media: &MediaContext,
    start: f64,
    end: f64,
    abort: &AtomicBool,
) -> Result<Vec<f32>, String> {
    validate_audio_clip_args(media, start, end)?;
    if abort.load(Ordering::Acquire) {
        return Err("Audio extraction canceled.".into());
    }

    let load_options = build_loadfile_options(start, end, Some(media.audio_id), None);

    #[cfg(target_os = "linux")]
    if let Some((mut memfd, memfd_path)) = create_memfd_wav() {
        let options = build_asr_audio_options(media, &memfd_path, true)?;
        run_encode_job(
            &media.path,
            &options,
            &load_options,
            Some(abort),
            "audio extraction",
        )?;
        memfd
            .seek(SeekFrom::Start(0))
            .map_err(|error| format!("Could not rewind extracted in-memory audio: {error}"))?;
        let mut bytes = Vec::new();
        memfd
            .read_to_end(&mut bytes)
            .map_err(|error| format!("Could not read extracted in-memory audio: {error}"))?;
        drop(memfd);
        return read_pcm16_wav_bytes(&bytes, SAMPLE_RATE_16K as u32);
    }

    let wav = extract_audio(media, start, end, abort, None)?;
    let bytes = fs::read(&wav.path)
        .map_err(|error| format!("Could not read the extracted audio file: {error}"))?;
    drop(wav);
    read_pcm16_wav_bytes(&bytes, SAMPLE_RATE_16K as u32)
}

pub(super) fn extract_audio(
    media: &MediaContext,
    start: f64,
    end: f64,
    abort: &AtomicBool,
    normalize_db: Option<f64>,
) -> Result<TempMediaFile, String> {
    validate_audio_clip_args(media, start, end)?;
    if abort.load(Ordering::Acquire) {
        return Err("Audio extraction canceled.".into());
    }
    let temporary = create_temp_wav_path()?;
    let output = temporary.path.to_string_lossy().into_owned();
    let options = build_anki_audio_clip_options(media, &output, normalize_db, true)?;
    let load_options = build_loadfile_options(start, end, Some(media.audio_id), None);
    run_encode_job(
        &media.path,
        &options,
        &load_options,
        Some(abort),
        "audio extraction",
    )?;
    if !temporary.path.is_file() {
        return Err(format!(
            "libmpv did not create an audio clip for '{}'. Check that the selected audio track is available.",
            media.path,
        ));
    }
    Ok(temporary)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn option_value<'a>(options: &'a [(&'static str, String)], key: &str) -> Option<&'a str> {
        options
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, v)| v.as_str())
    }

    #[test]
    fn asr_audio_arg_builder_passes_explicit_aid_and_16k_mono_pcm() {
        let media = MediaContext::new("/tmp/episode.mkv", 3);
        let options = build_asr_audio_options(&media, "/proc/self/fd/42", true).unwrap();
        assert_eq!(option_value(&options, "aid"), Some("3"));
        assert_eq!(option_value(&options, "vid"), Some("no"));
        assert_eq!(option_value(&options, "sid"), Some("no"));
        assert_eq!(option_value(&options, "oac"), Some("pcm_s16le"));
        assert_eq!(option_value(&options, "of"), Some("wav"));
        assert_eq!(option_value(&options, "audio-samplerate"), Some("16000"));
        assert_eq!(option_value(&options, "audio-channels"), Some("mono"));
        assert_eq!(option_value(&options, "ytdl"), Some("no"));
        assert_ne!(option_value(&options, "aid"), Some("auto"));
        assert_ne!(option_value(&options, "aid"), Some("yes"));

        let load_opts = build_loadfile_options(12.345, 42.345, Some(media.audio_id), None);
        assert_eq!(load_opts, "start=12.345,end=42.345,aid=3");
    }

    #[test]
    fn anki_aac_clip_arg_builder_uses_explicit_aid_native_rate_and_aac() {
        let media = MediaContext::new("https://example.com/video.mp4", 2);
        let options = build_anki_audio_clip_options(&media, "/tmp/clip.aac", Some(-16.5), false)
            .unwrap();
        assert_eq!(option_value(&options, "aid"), Some("2"));
        assert_eq!(option_value(&options, "oac"), Some("aac"));
        assert_eq!(option_value(&options, "ytdl"), Some("yes"));
        assert_eq!(option_value(&options, "af"), Some("loudnorm=I=-16.5"));
        assert_eq!(option_value(&options, "audio-samplerate"), None);
        assert_eq!(option_value(&options, "audio-channels"), None);
        assert_eq!(option_value(&options, "o"), Some("/tmp/clip.aac"));
    }

    #[test]
    fn anki_video_clip_arg_builder_passes_explicit_aid_and_sid_never_auto() {
        let media = MediaContext::new("/tmp/movie.mkv", 4);
        let options =
            build_anki_video_clip_options(&media, 7, true, true, Some(-20.0), "/tmp/clip.mp4")
                .unwrap();
        assert_eq!(option_value(&options, "aid"), Some("4"));
        assert_eq!(option_value(&options, "sid"), Some("7"));
        assert_eq!(option_value(&options, "ovc"), Some("libx264"));
        assert_eq!(option_value(&options, "oac"), Some("aac"));
        assert_eq!(option_value(&options, "af"), Some("loudnorm=I=-20.0"));

        let load_opts = build_loadfile_options(1.0, 2.5, Some(media.audio_id), Some(7));
        assert_eq!(load_opts, "start=1.000,end=2.500,aid=4,sid=7");

        let no_tracks =
            build_anki_video_clip_options(&media, 0, false, false, None, "/tmp/clip.mp4").unwrap();
        assert_eq!(option_value(&no_tracks, "aid"), Some("no"));
        assert_eq!(option_value(&no_tracks, "sid"), Some("no"));
    }

    #[test]
    fn arg_builders_reject_missing_or_zero_audio_track() {
        let zero_aid = MediaContext::new("/tmp/movie.mkv", 0);
        assert!(build_asr_audio_options(&zero_aid, "/tmp/out.wav", false).is_err());
        assert!(build_anki_audio_clip_options(&zero_aid, "/tmp/clip.aac", None, false).is_err());
        assert!(
            build_anki_video_clip_options(&zero_aid, 1, true, false, None, "/tmp/clip.mp4")
                .is_err()
        );
        assert!(build_anki_video_clip_options(&zero_aid, 0, true, true, None, "/tmp/clip.mp4").is_err());
    }

    #[test]
    #[ignore = "requires ffmpeg and libmpv WAV encoding"]
    fn encodes_anki_sentence_audio_without_asr() {
        let path = std::env::temp_dir().join(format!(
            "memento-anki-audio-test-{}.wav",
            std::process::id()
        ));
        let output = std::process::Command::new("ffmpeg")
            .args([
                "-v",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:duration=2",
            ])
            .arg(&path)
            .output()
            .expect("ffmpeg must be installed");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let media = MediaContext::new(path.to_string_lossy().into_owned(), 1);
        let clip = extract_clip_wav(&media, 0.25, 1.25, None);
        let aac_clip = extract_clip_audio(&media, 0.25, 1.25, None);
        let abort = AtomicBool::new(false);
        let samples = extract_audio_samples(&media, 0.25, 1.25, &abort);
        let _ = fs::remove_file(path);
        let clip = clip.unwrap();
        assert!(clip.len() > 16000);
        assert_eq!(&clip[..4], b"RIFF");
        assert_eq!(&clip[8..12], b"WAVE");
        let aac_clip = aac_clip.unwrap();
        assert!(!aac_clip.is_empty());
        let samples = samples.unwrap();
        assert!(samples.len() > 12000 && samples.len() < 20000);
    }

    #[test]
    #[ignore = "requires ffmpeg and a libmpv build with MP4 encoding"]
    fn encodes_real_anki_video_clip() {
        let path = std::env::temp_dir().join(format!(
            "memento-anki-video-test-{}.mp4",
            std::process::id()
        ));
        let output = std::process::Command::new("ffmpeg")
            .args([
                "-v",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "color=c=blue:s=320x180:r=10:d=2",
                "-c:v",
                "libx264",
                "-pix_fmt",
                "yuv420p",
            ])
            .arg(&path)
            .output()
            .expect("ffmpeg must be installed");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let media = MediaContext::new(path.to_string_lossy().into_owned(), -1);
        let clip = extract_clip_mp4(&media, 0, 0.25, 1.25, false, false, None);
        let _ = fs::remove_file(path);
        let clip = clip.unwrap();
        assert!(clip.len() > 100, "encoded clip was {} bytes", clip.len());
        assert_eq!(&clip[4..8], b"ftyp");
    }
}

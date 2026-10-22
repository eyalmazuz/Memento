//! Background media extraction shared by Anki and optional speech recognition.

use memento_player::mpv::ffi as mpv;
use std::ffi::{c_char, CStr, CString};
use std::fs;
use std::path::PathBuf;
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(1);

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

pub(super) struct TempWav {
    pub(super) path: PathBuf,
    directory: PathBuf,
}

impl Drop for TempWav {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
        let _ = fs::remove_dir(&self.directory);
    }
}

fn create_temp_wav_path() -> Result<TempWav, String> {
    let root = std::env::temp_dir();
    for _ in 0..32 {
        let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
        let directory = root.join(format!("memento-audio-{}-{id}", std::process::id()));
        match fs::create_dir(&directory) {
            Ok(()) => {
                return Ok(TempWav {
                    path: directory.join("window.wav"),
                    directory,
                });
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(format!(
                    "Could not create temporary audio directory: {error}"
                ))
            }
        }
    }
    Err("Could not allocate a unique temporary audio directory.".into())
}

fn create_temp_mp4_path() -> Result<TempWav, String> {
    let root = std::env::temp_dir();
    for _ in 0..32 {
        let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
        let directory = root.join(format!("memento-anki-video-{}-{id}", std::process::id()));
        match fs::create_dir(&directory) {
            Ok(()) => {
                return Ok(TempWav {
                    path: directory.join("clip.mp4"),
                    directory,
                })
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(format!(
                    "Could not create temporary Anki video directory: {error}"
                ))
            }
        }
    }
    Err("Could not allocate a temporary Anki video directory".into())
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

/// Extract a subtitle or context clip for an Anki note on a background thread.
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
    if audio && media.audio_id <= 0 {
        return Err("Could not extract Anki video: no audio track is selected".into());
    }
    if subtitles && subtitle_id <= 0 {
        return Err("Could not extract Anki video with subtitles: no subtitle track is selected; use {video:subtitles=false} for this card".into());
    }
    let input = CString::new(media.path.as_bytes())
        .map_err(|_| "Media path contains an invalid NUL byte".to_owned())?;
    let temporary = create_temp_mp4_path()?;
    let output = CString::new(temporary.path.to_string_lossy().as_bytes())
        .map_err(|_| "Temporary video path contains an invalid NUL byte".to_owned())?;
    let handle = unsafe { mpv::mpv_create() };
    if handle.is_null() {
        return Err("Could not create a libmpv video encoding instance".into());
    }
    let handle = MpvHandle(handle);
    let options: [(&CStr, &CStr); 9] = [
        (c"ovc", c"libx264"),
        (c"oac", c"aac"),
        (c"vid", c"auto"),
        (c"aid", if audio { c"auto" } else { c"no" }),
        (c"sid", if subtitles { c"auto" } else { c"no" }),
        (c"secondary-sid", c"no"),
        (c"cover-art-auto", c"no"),
        (c"keep-open", c"no"),
        (c"config", c"no"),
    ];
    for (name, value) in options {
        let status = unsafe { mpv::mpv_set_option_string(handle.0, name.as_ptr(), value.as_ptr()) };
        if status < 0 {
            return Err(format!(
                "Could not configure libmpv video encoding option {}: {}",
                name.to_string_lossy(),
                mpv_error(status)
            ));
        }
    }
    if let Some(db) = normalize_db.filter(|_| audio) {
        if !db.is_finite() {
            return Err("Invalid Anki video audio normalization dB".into());
        }
        let filter = CString::new(format!("loudnorm=I={db:.1}"))
            .map_err(|_| "Invalid Anki video audio filter".to_owned())?;
        let status =
            unsafe { mpv::mpv_set_option_string(handle.0, c"af".as_ptr(), filter.as_ptr()) };
        if status < 0 {
            return Err(format!(
                "Could not configure Anki video audio: {}",
                mpv_error(status)
            ));
        }
    }
    let ytdl = if media.path.contains("://") {
        c"yes"
    } else {
        c"no"
    };
    for (name, value) in [(c"ytdl", ytdl), (c"o", output.as_c_str())] {
        let status = unsafe { mpv::mpv_set_option_string(handle.0, name.as_ptr(), value.as_ptr()) };
        if status < 0 {
            return Err(format!(
                "Could not configure Anki video output: {}",
                mpv_error(status)
            ));
        }
    }
    let status = unsafe { mpv::mpv_initialize(handle.0) };
    if status < 0 {
        return Err(format!(
            "Could not initialize libmpv video encoding: {}",
            mpv_error(status)
        ));
    }
    let mut load_options = format!("start={start:.3},end={end:.3}");
    if audio {
        load_options.push_str(&format!(",aid={}", media.audio_id));
    }
    if subtitles {
        load_options.push_str(&format!(",sid={subtitle_id}"));
    }
    let load_options =
        CString::new(load_options).map_err(|_| "Invalid Anki video options".to_owned())?;
    let api_version = unsafe { mpv::mpv_client_api_version() };
    let mut command: Vec<*const c_char> =
        vec![c"loadfile".as_ptr(), input.as_ptr(), c"replace".as_ptr()];
    if api_version >= ((2 << 16) | 3) {
        command.push(c"0".as_ptr());
    }
    command.extend([load_options.as_ptr(), ptr::null()]);
    let status = unsafe { mpv::mpv_command(handle.0, command.as_mut_ptr()) };
    if status < 0 {
        return Err(format!(
            "Could not start Anki video encoding: {}",
            mpv_error(status)
        ));
    }
    loop {
        let event = unsafe { mpv::mpv_wait_event(handle.0, 0.1) };
        if event.is_null() {
            return Err("libmpv returned no event while encoding Anki video".into());
        }
        match unsafe { (*event).event_id } {
            mpv::MPV_EVENT_QUEUE_OVERFLOW => {
                return Err("libmpv Anki video event queue overflowed".into())
            }
            mpv::MPV_EVENT_END_FILE => {
                let end_file = unsafe { &*((*event).data as *const mpv::mpv_event_end_file) };
                if end_file.reason == mpv::MPV_END_FILE_REASON_REDIRECT {
                    continue;
                }
                if end_file.error < 0 {
                    return Err(format!(
                        "libmpv could not encode the Anki video: {}",
                        mpv_error(end_file.error)
                    ));
                }
                if end_file.reason != mpv::MPV_END_FILE_REASON_EOF {
                    return Err(format!(
                        "libmpv ended Anki video encoding early (reason {})",
                        end_file.reason
                    ));
                }
                // The MP4 footer is written while the encoder shuts down.
                drop(handle);
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
                return Ok(bytes);
            }
            _ => {}
        }
    }
}

pub(super) fn extract_audio(
    media: &MediaContext,
    start: f64,
    end: f64,
    abort: &AtomicBool,
    normalize_db: Option<f64>,
) -> Result<TempWav, String> {
    if abort.load(Ordering::Acquire) {
        return Err("Audio extraction canceled.".into());
    }
    if media.path.is_empty()
        || media.audio_id < 0
        || !start.is_finite()
        || !end.is_finite()
        || end <= start
    {
        return Err(
            "Could not extract audio: media path, selected audio track, or window is invalid."
                .into(),
        );
    }
    let input = CString::new(media.path.as_bytes())
        .map_err(|_| "Media path contains an invalid NUL byte.".to_owned())?;
    let temporary = create_temp_wav_path()?;
    let output = CString::new(temporary.path.to_string_lossy().as_bytes())
        .map_err(|_| "Temporary audio path contains an invalid NUL byte.".to_owned())?;
    let handle = unsafe { mpv::mpv_create() };
    if handle.is_null() {
        return Err("Could not create a libmpv audio extraction instance.".into());
    }
    let handle = MpvHandle(handle);
    let options: [(&CStr, &CStr); 9] = [
        (c"vid", c"no"),
        (c"sid", c"no"),
        (c"secondary-sid", c"no"),
        (c"oac", c"pcm_s16le"),
        (c"audio-samplerate", c"16000"),
        (c"audio-channels", c"mono"),
        (c"cover-art-auto", c"no"),
        (c"keep-open", c"no"),
        (c"config", c"no"),
    ];
    for (name, value) in options {
        let status = unsafe { mpv::mpv_set_option_string(handle.0, name.as_ptr(), value.as_ptr()) };
        if status < 0 {
            return Err(format!(
                "Could not configure libmpv audio extraction: {}",
                mpv_error(status)
            ));
        }
    }
    if let Some(db) = normalize_db {
        if !db.is_finite() {
            return Err("Invalid Anki audio normalization dB".into());
        }
        let filter = CString::new(format!("loudnorm=I={db:.1}"))
            .map_err(|_| "Invalid Anki audio normalization filter".to_owned())?;
        let status =
            unsafe { mpv::mpv_set_option_string(handle.0, c"af".as_ptr(), filter.as_ptr()) };
        if status < 0 {
            return Err(format!(
                "Could not configure note audio normalization: {}",
                mpv_error(status)
            ));
        }
    }
    let ytdl = if media.path.contains("://") {
        c"yes"
    } else {
        c"no"
    };
    let status = unsafe { mpv::mpv_set_option_string(handle.0, c"ytdl".as_ptr(), ytdl.as_ptr()) };
    if status < 0 {
        return Err(format!(
            "Could not configure libmpv media access: {}",
            mpv_error(status)
        ));
    }
    let status = unsafe { mpv::mpv_set_option_string(handle.0, c"o".as_ptr(), output.as_ptr()) };
    if status < 0 {
        return Err(format!(
            "Could not configure the temporary audio WAV output: {}",
            mpv_error(status)
        ));
    }
    let status = unsafe { mpv::mpv_initialize(handle.0) };
    if status < 0 {
        return Err(format!(
            "Could not initialize libmpv audio extraction: {}",
            mpv_error(status)
        ));
    }
    if abort.load(Ordering::Acquire) {
        return Err("Audio extraction canceled.".into());
    }

    let load_options = CString::new(format!(
        "start={start:.3},end={end:.3},aid={}",
        media.audio_id,
    ))
    .map_err(|_| "Invalid audio extraction options.".to_owned())?;
    let api_version = unsafe { mpv::mpv_client_api_version() };
    // mpv_client_api_version() packs (major << 16) | minor; loadfile gained its
    // <index> argument in client API 2.3.
    let is_api_23 = api_version >= ((2 << 16) | 3);
    let mut command: Vec<*const c_char> =
        vec![c"loadfile".as_ptr(), input.as_ptr(), c"replace".as_ptr()];
    if is_api_23 {
        command.push(c"0".as_ptr());
    }
    command.push(load_options.as_ptr());
    command.push(ptr::null());
    let status = unsafe { mpv::mpv_command(handle.0, command.as_mut_ptr()) };
    if status < 0 {
        return Err(format!(
            "Could not start libmpv audio extraction: {}",
            mpv_error(status)
        ));
    }

    loop {
        if abort.load(Ordering::Acquire) {
            return Err("Audio extraction canceled.".into());
        }
        let event = unsafe { mpv::mpv_wait_event(handle.0, 0.1) };
        if event.is_null() {
            return Err("libmpv returned no event while extracting audio.".into());
        }
        let event_id = unsafe { (*event).event_id };
        if event_id == mpv::MPV_EVENT_NONE {
            continue;
        }
        if event_id == mpv::MPV_EVENT_QUEUE_OVERFLOW {
            return Err("libmpv audio extraction event queue overflowed.".into());
        }
        if event_id == mpv::MPV_EVENT_END_FILE {
            if !temporary.path.is_file() {
                return Err(format!(
                    "libmpv did not create an audio clip for '{}'. Check that the selected audio track is available.",
                    media.path,
                ));
            }
            return Ok(temporary);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let _ = fs::remove_file(path);
        let clip = clip.unwrap();
        assert!(clip.len() > 16000);
        assert_eq!(&clip[..4], b"RIFF");
        assert_eq!(&clip[8..12], b"WAVE");
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

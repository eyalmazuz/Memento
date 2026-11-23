//! Background media extraction shared by Anki and optional speech recognition.

#[cfg(any(feature = "asr-whisper", feature = "asr-onnx", test))]
use subomi_asr::audio::wavreader::{read_pcm16_wav_bytes, SAMPLE_RATE_16K};
use subomi_player::mpv::ffi as mpv;
use subomi_player::EncodeContext;
use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::fs;
#[cfg(all(
    target_os = "linux",
    any(feature = "asr-whisper", feature = "asr-onnx", test)
))]
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use tempfile::TempDir;

/// Default maximum wall-clock duration for a background libmpv encode job.
pub const DEFAULT_ENCODE_TIMEOUT: Duration = Duration::from_secs(60);

/// An immutable snapshot of the media, selected mpv audio track, and streaming
/// metadata (yt-dlp/HTTP headers/cookies/split DASH audio). Capture it on the
/// UI thread with `MpvInstance::capture_encode_context` before starting.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MediaContext {
    pub path: String,
    pub audio_id: i64,
    pub stream_open_filename: String,
    pub script_opts: String,
    pub http_header_fields: Vec<String>,
    pub referrer: String,
    pub user_agent: String,
    pub cookies: bool,
    pub cookies_file: String,
    pub ytdl_raw_options: String,
    pub external_audio_filename: Option<String>,
}

impl MediaContext {
    pub fn new(path: impl Into<String>, audio_id: i64) -> Self {
        Self {
            path: path.into(),
            audio_id,
            ..Self::default()
        }
    }

    pub fn from_encode_context(ctx: EncodeContext) -> Self {
        Self::from(ctx)
    }

    /// Resolve the effective media path/URL for off-thread encoding, preferring
    /// `stream_open_filename` when non-empty and not an internal `subomi-asr://`
    /// stream URI.
    pub fn effective_media_path(&self) -> &str {
        EncodeContext::resolve_media_path(&self.stream_open_filename, &self.path)
    }

    /// Returns the trimmed external audio path/URL when configured for split
    /// DASH / external audio tracks (filtering out empty strings and internal
    /// `subomi-asr://` URIs).
    pub fn effective_external_audio(&self) -> Option<&str> {
        self.external_audio_filename
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty() && !EncodeContext::is_internal_asr_uri(s))
    }
}

impl From<EncodeContext> for MediaContext {
    fn from(ctx: EncodeContext) -> Self {
        Self {
            path: ctx.path,
            audio_id: ctx.audio_id,
            stream_open_filename: ctx.stream_open_filename,
            script_opts: ctx.script_opts,
            http_header_fields: ctx.http_header_fields,
            referrer: ctx.referrer,
            user_agent: ctx.user_agent,
            cookies: ctx.cookies,
            cookies_file: ctx.cookies_file,
            ytdl_raw_options: ctx.ytdl_raw_options,
            external_audio_filename: ctx.external_audio_filename,
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
    create_temp_media_path("subomi-audio-", "window.wav", "temporary audio directory")
}

fn create_temp_aac_path() -> Result<TempMediaFile, String> {
    create_temp_media_path(
        "subomi-anki-audio-",
        "clip.aac",
        "temporary Anki audio directory",
    )
}

fn create_temp_mp4_path() -> Result<TempMediaFile, String> {
    create_temp_media_path(
        "subomi-anki-video-",
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

fn mpv_error(status: c_int) -> String {
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
    if media.effective_media_path().trim().is_empty()
        || (media.audio_id <= 0 && media.effective_external_audio().is_none())
        || !start.is_finite()
        || !end.is_finite()
        || start < 0.0
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
    let start = if start == 0.0 { 0.0 } else { start };
    let end = if end == 0.0 { 0.0 } else { end };
    let mut load_options = format!("start={start:.3},end={end:.3}");
    if let Some(aid) = audio_id {
        load_options.push_str(&format!(",aid={aid}"));
    }
    if let Some(sid) = subtitle_id {
        load_options.push_str(&format!(",sid={sid}"));
    }
    load_options
}

fn loadfile_audio_id(media: &MediaContext, include_audio: bool) -> Option<i64> {
    if !include_audio || media.effective_external_audio().is_some() {
        None
    } else {
        Some(media.audio_id)
    }
}

fn ytdl_flag(media_path: &str) -> &'static str {
    if media_path.contains("://") {
        "yes"
    } else {
        "no"
    }
}

fn append_lean_encode_options(
    ytdl: &str,
    include_subtitles: bool,
    options: &mut Vec<(&'static str, String)>,
) {
    options.push(("terminal", "no".to_owned()));
    options.push(("osc", "no".to_owned()));
    options.push(("osd-level", "0".to_owned()));
    options.push(("audio-display", "no".to_owned()));
    options.push(("audio-file-auto", "no".to_owned()));
    options.push(("sub-auto", "no".to_owned()));
    options.push(("load-osd-console", "no".to_owned()));
    options.push(("load-stats-overlay", "no".to_owned()));
    options.push(("load-auto-profiles", "no".to_owned()));
    if !include_subtitles {
        options.push(("embeddedfonts", "no".to_owned()));
    }
    if ytdl == "no" {
        options.push(("load-scripts", "no".to_owned()));
        options.push(("demuxer-readahead-secs", "1".to_owned()));
    }
}

fn append_streaming_options(
    media: &MediaContext,
    include_audio: bool,
    options: &mut Vec<(&'static str, String)>,
) {
    let script_opts = media.script_opts.trim();
    if !script_opts.is_empty() {
        options.push(("script-opts", script_opts.to_owned()));
    }
    let referrer = media.referrer.trim();
    if !referrer.is_empty() {
        options.push(("referrer", referrer.to_owned()));
    }
    let user_agent = media.user_agent.trim();
    if !user_agent.is_empty() {
        options.push(("user-agent", user_agent.to_owned()));
    }
    if media.cookies {
        options.push(("cookies", "yes".to_owned()));
    }
    let cookies_file = media.cookies_file.trim();
    if !cookies_file.is_empty() {
        options.push(("cookies-file", cookies_file.to_owned()));
    }
    let ytdl_raw_options = media.ytdl_raw_options.trim();
    if !ytdl_raw_options.is_empty() {
        options.push(("ytdl-raw-options", ytdl_raw_options.to_owned()));
    }
    if include_audio {
        if let Some(ext_audio) = media.effective_external_audio() {
            options.push(("audio-files", ext_audio.to_owned()));
        }
    }
}

fn non_empty_http_headers(headers: &[String]) -> Vec<&str> {
    headers
        .iter()
        .map(String::as_str)
        .map(str::trim)
        .filter(|header| !header.is_empty())
        .collect()
}

/// Build libmpv encoder options for 16 kHz mono PCM16 WAV extraction used by ASR.
fn build_asr_audio_options(
    media: &MediaContext,
    output: &str,
    force_wav_format: bool,
) -> Result<Vec<(&'static str, String)>, String> {
    let media_path = media.effective_media_path();
    let has_external_audio = media.effective_external_audio().is_some();
    if media_path.trim().is_empty() || (media.audio_id <= 0 && !has_external_audio) {
        return Err(
            "Could not extract audio: media path or selected audio track is invalid.".into(),
        );
    }
    let aid_value = if has_external_audio {
        "auto".to_owned()
    } else {
        media.audio_id.to_string()
    };
    let ytdl = ytdl_flag(media_path);
    let mut options = vec![
        ("vid", "no".to_owned()),
        ("aid", aid_value),
        ("sid", "no".to_owned()),
        ("secondary-sid", "no".to_owned()),
        ("oac", "pcm_s16le".to_owned()),
        ("audio-samplerate", "16000".to_owned()),
        ("audio-channels", "mono".to_owned()),
        ("cover-art-auto", "no".to_owned()),
        ("keep-open", "no".to_owned()),
        ("config", "no".to_owned()),
        ("ytdl", ytdl.to_owned()),
        ("o", output.to_owned()),
    ];
    if force_wav_format {
        options.push(("of", "wav".to_owned()));
    }
    append_lean_encode_options(ytdl, false, &mut options);
    append_streaming_options(media, true, &mut options);
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
    let media_path = media.effective_media_path();
    let has_external_audio = media.effective_external_audio().is_some();
    if media_path.trim().is_empty() || (media.audio_id <= 0 && !has_external_audio) {
        return Err(
            "Could not extract audio: media path or selected audio track is invalid.".into(),
        );
    }
    let mut options = if wav_compat {
        build_asr_audio_options(media, output, false)?
    } else {
        let aid_value = if has_external_audio {
            "auto".to_owned()
        } else {
            media.audio_id.to_string()
        };
        let ytdl = ytdl_flag(media_path);
        let mut opts = vec![
            ("vid", "no".to_owned()),
            ("aid", aid_value),
            ("sid", "no".to_owned()),
            ("secondary-sid", "no".to_owned()),
            ("oac", "aac".to_owned()),
            ("cover-art-auto", "no".to_owned()),
            ("keep-open", "no".to_owned()),
            ("config", "no".to_owned()),
            ("ytdl", ytdl.to_owned()),
            ("o", output.to_owned()),
        ];
        append_lean_encode_options(ytdl, false, &mut opts);
        append_streaming_options(media, true, &mut opts);
        opts
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
    let media_path = media.effective_media_path();
    let has_external_audio = media.effective_external_audio().is_some();
    if audio && media.audio_id <= 0 && !has_external_audio {
        return Err("Could not extract Anki video: no audio track is selected".into());
    }
    if subtitles && subtitle_id <= 0 {
        return Err("Could not extract Anki video with subtitles: no subtitle track is selected; use {video:subtitles=false} for this card".into());
    }
    let aid_value = if !audio {
        "no".to_owned()
    } else if has_external_audio {
        "auto".to_owned()
    } else {
        media.audio_id.to_string()
    };
    let sid_value = if subtitles {
        subtitle_id.to_string()
    } else {
        "no".to_owned()
    };
    let ytdl = ytdl_flag(media_path);
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
        ("ytdl", ytdl.to_owned()),
        ("o", output.to_owned()),
    ];
    append_lean_encode_options(ytdl, subtitles, &mut options);
    append_streaming_options(media, audio, &mut options);
    if let Some(db) = normalize_db.filter(|_| audio) {
        if !db.is_finite() {
            return Err("Invalid Anki video audio normalization dB".into());
        }
        options.push(("af", format!("loudnorm=I={db:.1}")));
    }
    Ok(options)
}

#[derive(Debug, PartialEq, Eq)]
enum EncodeEventAction {
    Continue,
    Finished,
    Failed(String),
}

fn classify_encode_event(
    event_id: mpv::mpv_event_id,
    end_file: Option<(mpv::mpv_end_file_reason, c_int)>,
    media_path: &str,
    context: &str,
) -> EncodeEventAction {
    if event_id == mpv::MPV_EVENT_NONE {
        return EncodeEventAction::Continue;
    }
    if event_id == mpv::MPV_EVENT_QUEUE_OVERFLOW {
        return EncodeEventAction::Failed(format!("libmpv {context} event queue overflowed."));
    }
    if event_id == mpv::MPV_EVENT_SHUTDOWN {
        return EncodeEventAction::Failed(format!(
            "libmpv shut down before completing {context} for '{media_path}'."
        ));
    }
    if event_id == mpv::MPV_EVENT_END_FILE {
        let Some((reason, error)) = end_file else {
            return EncodeEventAction::Failed(format!(
                "libmpv returned an empty end-file event during {context}."
            ));
        };
        if reason == mpv::MPV_END_FILE_REASON_REDIRECT {
            return EncodeEventAction::Continue;
        }
        if error < 0 {
            return EncodeEventAction::Failed(format!(
                "libmpv failed {context} for '{media_path}': {}",
                mpv_error(error)
            ));
        }
        if reason != mpv::MPV_END_FILE_REASON_EOF {
            return EncodeEventAction::Failed(format!(
                "libmpv ended {context} early (reason {reason})"
            ));
        }
        return EncodeEventAction::Finished;
    }
    EncodeEventAction::Continue
}

fn check_abort_or_timeout(
    abort: Option<&AtomicBool>,
    deadline: Option<(Duration, Option<Instant>)>,
    context: &str,
) -> Result<Option<Duration>, String> {
    if abort.is_some_and(|flag| flag.load(Ordering::Acquire)) {
        return Err(format!("{context} canceled."));
    }
    let Some((limit, target)) = deadline else {
        return Ok(None);
    };
    if limit.is_zero() {
        return Err(format!(
            "libmpv {context} timed out after {:.1}s.",
            limit.as_secs_f64()
        ));
    }
    if let Some(target) = target {
        let remaining = target.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(format!(
                "libmpv {context} timed out after {:.1}s.",
                limit.as_secs_f64()
            ));
        }
        return Ok(Some(remaining));
    }
    Ok(None)
}

fn set_string_list_option(
    handle: *mut mpv::mpv_handle,
    name: &str,
    values: &[&str],
    context: &str,
) -> Result<(), String> {
    if values.is_empty() {
        return Ok(());
    }
    let name_c = CString::new(name)
        .map_err(|_| format!("Invalid libmpv option name '{name}' for {context}"))?;
    let mut c_values = Vec::with_capacity(values.len());
    for value in values {
        let c_val = CString::new(value.as_bytes()).map_err(|_| {
            format!("libmpv option '{name}' contains an invalid NUL byte for {context}")
        })?;
        c_values.push(c_val);
    }
    let mut nodes: Vec<mpv::mpv_node> = c_values
        .iter()
        .map(|s| {
            let mut node: mpv::mpv_node = unsafe { std::mem::zeroed() };
            node.format = mpv::MPV_FORMAT_STRING;
            node.u.string = s.as_ptr().cast_mut();
            node
        })
        .collect();
    let num = c_int::try_from(nodes.len())
        .map_err(|_| format!("Too many entries for libmpv option '{name}' during {context}"))?;
    let mut list = mpv::mpv_node_list {
        num,
        values: nodes.as_mut_ptr(),
        keys: ptr::null_mut(),
    };
    let mut array_node: mpv::mpv_node = unsafe { std::mem::zeroed() };
    array_node.format = mpv::MPV_FORMAT_NODE_ARRAY;
    array_node.u.list = &mut list;

    let status = unsafe {
        mpv::mpv_set_option(
            handle,
            name_c.as_ptr(),
            mpv::MPV_FORMAT_NODE,
            (&mut array_node as *mut mpv::mpv_node).cast::<c_void>(),
        )
    };
    if status < 0 {
        let err = mpv_error(status);
        log::warn!(
            "Could not configure libmpv {context} list option '{name}' ({} entries): {err}",
            values.len()
        );
        return Err(format!(
            "Could not configure libmpv {context} option {name}: {err}"
        ));
    }
    Ok(())
}

fn apply_encode_handle_options(
    handle: *mut mpv::mpv_handle,
    options: &[(&str, String)],
    http_header_fields: &[String],
    context: &str,
) -> Result<(), String> {
    for (name, value) in options {
        if *name == "audio-files" || *name == "audio-file" {
            // Pass external audio URLs/paths as a 1-element MPV_FORMAT_NODE_ARRAY
            // so libmpv never splits `https://...` on ':' (Unix) or ';' (Windows).
            set_string_list_option(handle, "audio-files", &[value.as_str()], context)?;
            continue;
        }
        let name_c = CString::new(*name)
            .map_err(|_| format!("Invalid libmpv option name '{name}' for {context}"))?;
        let value_c = CString::new(value.as_bytes())
            .map_err(|_| format!("libmpv option '{name}' contains an invalid NUL byte"))?;
        let status =
            unsafe { mpv::mpv_set_option_string(handle, name_c.as_ptr(), value_c.as_ptr()) };
        if status < 0 {
            let err = mpv_error(status);
            log::warn!("Could not configure libmpv {context} option '{name}': {err}");
            return Err(format!(
                "Could not configure libmpv {context} option {name}: {err}"
            ));
        }
    }
    let headers = non_empty_http_headers(http_header_fields);
    if !headers.is_empty() {
        set_string_list_option(handle, "http-header-fields", &headers, context)?;
    }
    Ok(())
}

fn run_encode_job(
    media_path: &str,
    options: &[(&str, String)],
    http_header_fields: &[String],
    load_options: &str,
    abort: Option<&AtomicBool>,
    timeout: Option<Duration>,
    context: &str,
) -> Result<(), String> {
    let deadline = timeout.map(|limit| (limit, Instant::now().checked_add(limit)));
    check_abort_or_timeout(abort, deadline, context)?;

    let input = CString::new(media_path.as_bytes())
        .map_err(|_| "Media path contains an invalid NUL byte.".to_owned())?;
    let load_options_c =
        CString::new(load_options.as_bytes()).map_err(|_| format!("Invalid {context} options."))?;

    let handle = unsafe { mpv::mpv_create() };
    if handle.is_null() {
        log::warn!("Could not create a libmpv {context} instance for '{media_path}'.");
        return Err(format!("Could not create a libmpv {context} instance."));
    }
    let handle = MpvHandle(handle);

    apply_encode_handle_options(handle.0, options, http_header_fields, context)?;

    let status = unsafe { mpv::mpv_initialize(handle.0) };
    if status < 0 {
        let err = mpv_error(status);
        log::warn!("Could not initialize libmpv {context} for '{media_path}': {err}");
        return Err(format!("Could not initialize libmpv {context}: {err}"));
    }
    check_abort_or_timeout(abort, deadline, context)?;

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
        let err = mpv_error(status);
        log::warn!("Could not start libmpv {context} for '{media_path}': {err}");
        return Err(format!("Could not start libmpv {context}: {err}"));
    }

    loop {
        let wait_secs = check_abort_or_timeout(abort, deadline, context)?
            .map_or(0.1, |remaining| remaining.as_secs_f64().min(0.1));
        let event = unsafe { mpv::mpv_wait_event(handle.0, wait_secs) };
        if event.is_null() {
            log::warn!("libmpv returned no event during {context} for '{media_path}'.");
            return Err(format!("libmpv returned no event during {context}."));
        }
        let event_id = unsafe { (*event).event_id };
        let end_file = if event_id == mpv::MPV_EVENT_END_FILE {
            let end_file_ptr = unsafe { (*event).data as *const mpv::mpv_event_end_file };
            (!end_file_ptr.is_null()).then(|| {
                let info = unsafe { &*end_file_ptr };
                (info.reason, info.error)
            })
        } else {
            None
        };
        match classify_encode_event(event_id, end_file, media_path, context) {
            EncodeEventAction::Continue => continue,
            EncodeEventAction::Finished => {
                // Terminate and flush the encoder handle before inspecting the output.
                drop(handle);
                return Ok(());
            }
            EncodeEventAction::Failed(message) => {
                log::warn!("{message}");
                return Err(message);
            }
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
    extract_clip_audio_with_cancel(
        media,
        start,
        end,
        normalize_db,
        None,
        Some(DEFAULT_ENCODE_TIMEOUT),
    )
}

/// Extract a high-quality stereo `.aac` audio clip with optional cooperative
/// cancellation and timeout bounds.
pub fn extract_clip_audio_with_cancel(
    media: &MediaContext,
    start: f64,
    end: f64,
    normalize_db: Option<f64>,
    abort: Option<&AtomicBool>,
    timeout: Option<Duration>,
) -> Result<Vec<u8>, String> {
    validate_audio_clip_args(media, start, end)?;
    let deadline = timeout.map(|limit| (limit, Instant::now().checked_add(limit)));
    check_abort_or_timeout(abort, deadline, "Anki audio clip encoding")?;
    let temporary = create_temp_aac_path()?;
    let output = temporary.path.to_string_lossy().into_owned();
    let options = build_anki_audio_clip_options(media, &output, normalize_db, false)?;
    let load_options = build_loadfile_options(start, end, loadfile_audio_id(media, true), None);
    run_encode_job(
        media.effective_media_path(),
        &options,
        &media.http_header_fields,
        &load_options,
        abort,
        timeout,
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
    extract_clip_wav_with_cancel(
        media,
        start,
        end,
        normalize_db,
        None,
        Some(DEFAULT_ENCODE_TIMEOUT),
    )
}

/// Extract a 16 kHz mono WAV clip with optional cooperative cancellation and
/// timeout bounds.
pub fn extract_clip_wav_with_cancel(
    media: &MediaContext,
    start: f64,
    end: f64,
    normalize_db: Option<f64>,
    abort: Option<&AtomicBool>,
    timeout: Option<Duration>,
) -> Result<Vec<u8>, String> {
    let wav = extract_audio_with_cancel(media, start, end, abort, normalize_db, timeout)?;
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
    extract_clip_mp4_with_cancel(
        media,
        subtitle_id,
        start,
        end,
        audio,
        subtitles,
        normalize_db,
        None,
        Some(DEFAULT_ENCODE_TIMEOUT),
    )
}

/// Encode an Anki MP4 video attachment with optional cooperative cancellation
/// and timeout bounds.
#[allow(clippy::too_many_arguments)]
pub fn extract_clip_mp4_with_cancel(
    media: &MediaContext,
    subtitle_id: i64,
    start: f64,
    end: f64,
    audio: bool,
    subtitles: bool,
    normalize_db: Option<f64>,
    abort: Option<&AtomicBool>,
    timeout: Option<Duration>,
) -> Result<Vec<u8>, String> {
    if media.effective_media_path().trim().is_empty()
        || !start.is_finite()
        || !end.is_finite()
        || start < 0.0
        || end <= start
    {
        return Err("Could not extract Anki video: media path or clip range is invalid".into());
    }
    let deadline = timeout.map(|limit| (limit, Instant::now().checked_add(limit)));
    check_abort_or_timeout(abort, deadline, "Anki video encoding")?;
    let temporary = create_temp_mp4_path()?;
    let output = temporary.path.to_string_lossy().into_owned();
    let options =
        build_anki_video_clip_options(media, subtitle_id, audio, subtitles, normalize_db, &output)?;
    let load_options = build_loadfile_options(
        start,
        end,
        loadfile_audio_id(media, audio),
        subtitles.then_some(subtitle_id),
    );
    run_encode_job(
        media.effective_media_path(),
        &options,
        &media.http_header_fields,
        &load_options,
        abort,
        timeout,
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

#[cfg(all(
    target_os = "linux",
    any(feature = "asr-whisper", feature = "asr-onnx", test)
))]
fn create_memfd_wav() -> Option<(fs::File, String)> {
    use std::os::unix::io::{AsRawFd, FromRawFd};
    let fd = unsafe { libc::memfd_create(c"subomi-asr-pcm".as_ptr(), libc::MFD_CLOEXEC) };
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

    let load_options = build_loadfile_options(start, end, loadfile_audio_id(media, true), None);

    #[cfg(target_os = "linux")]
    if let Some((mut memfd, memfd_path)) = create_memfd_wav() {
        let options = build_asr_audio_options(media, &memfd_path, true)?;
        run_encode_job(
            media.effective_media_path(),
            &options,
            &media.http_header_fields,
            &load_options,
            Some(abort),
            Some(DEFAULT_ENCODE_TIMEOUT),
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

#[cfg(any(feature = "asr-whisper", feature = "asr-onnx", test))]
pub(super) fn extract_audio(
    media: &MediaContext,
    start: f64,
    end: f64,
    abort: &AtomicBool,
    normalize_db: Option<f64>,
) -> Result<TempMediaFile, String> {
    extract_audio_with_cancel(
        media,
        start,
        end,
        Some(abort),
        normalize_db,
        Some(DEFAULT_ENCODE_TIMEOUT),
    )
}

fn extract_audio_with_cancel(
    media: &MediaContext,
    start: f64,
    end: f64,
    abort: Option<&AtomicBool>,
    normalize_db: Option<f64>,
    timeout: Option<Duration>,
) -> Result<TempMediaFile, String> {
    validate_audio_clip_args(media, start, end)?;
    let deadline = timeout.map(|limit| (limit, Instant::now().checked_add(limit)));
    check_abort_or_timeout(abort, deadline, "audio extraction")?;
    let temporary = create_temp_wav_path()?;
    let output = temporary.path.to_string_lossy().into_owned();
    let options = build_anki_audio_clip_options(media, &output, normalize_db, true)?;
    let load_options = build_loadfile_options(start, end, loadfile_audio_id(media, true), None);
    run_encode_job(
        media.effective_media_path(),
        &options,
        &media.http_header_fields,
        &load_options,
        abort,
        timeout,
        "audio extraction",
    )?;
    if !temporary.path.is_file() {
        return Err(format!(
            "libmpv did not create an audio clip for '{}'. Check that the selected audio track is available.",
            media.effective_media_path(),
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
    fn classify_encode_event_handles_shutdown_overflow_and_end_file_reasons() {
        assert_eq!(
            classify_encode_event(mpv::MPV_EVENT_NONE, None, "/tmp/a.mkv", "encode"),
            EncodeEventAction::Continue
        );
        let shutdown = classify_encode_event(mpv::MPV_EVENT_SHUTDOWN, None, "/tmp/a.mkv", "encode");
        assert!(
            matches!(shutdown, EncodeEventAction::Failed(ref msg) if msg.contains("shut down")),
            "MPV_EVENT_SHUTDOWN must terminate encode loop cleanly: {shutdown:?}"
        );
        let overflow =
            classify_encode_event(mpv::MPV_EVENT_QUEUE_OVERFLOW, None, "/tmp/a.mkv", "encode");
        assert!(matches!(overflow, EncodeEventAction::Failed(ref msg) if msg.contains("overflow")));
        assert_eq!(
            classify_encode_event(
                mpv::MPV_EVENT_END_FILE,
                Some((mpv::MPV_END_FILE_REASON_REDIRECT, 0)),
                "/tmp/a.mkv",
                "encode"
            ),
            EncodeEventAction::Continue
        );
        assert_eq!(
            classify_encode_event(
                mpv::MPV_EVENT_END_FILE,
                Some((mpv::MPV_END_FILE_REASON_EOF, 0)),
                "/tmp/a.mkv",
                "encode"
            ),
            EncodeEventAction::Finished
        );
        let early = classify_encode_event(
            mpv::MPV_EVENT_END_FILE,
            Some((mpv::MPV_END_FILE_REASON_STOP, 0)),
            "/tmp/a.mkv",
            "encode",
        );
        assert!(matches!(early, EncodeEventAction::Failed(ref msg) if msg.contains("early")));
    }

    #[test]
    fn cancel_and_zero_timeout_abort_encode_helpers_immediately() {
        let valid = MediaContext::new("/tmp/movie.mkv", 1);
        let aborted = AtomicBool::new(true);

        let err = extract_clip_audio_with_cancel(
            &valid,
            0.0,
            1.0,
            None,
            Some(&aborted),
            Some(DEFAULT_ENCODE_TIMEOUT),
        )
        .unwrap_err();
        assert!(err.contains("canceled"), "unexpected error: {err}");

        let err = extract_clip_mp4_with_cancel(
            &valid,
            1,
            0.0,
            1.0,
            false,
            false,
            None,
            Some(&aborted),
            Some(DEFAULT_ENCODE_TIMEOUT),
        )
        .unwrap_err();
        assert!(err.contains("canceled"), "unexpected error: {err}");

        let err =
            extract_clip_audio_with_cancel(&valid, 0.0, 1.0, None, None, Some(Duration::ZERO))
                .unwrap_err();
        assert!(err.contains("timed out"), "unexpected error: {err}");

        let err = extract_clip_mp4_with_cancel(
            &valid,
            1,
            0.0,
            1.0,
            false,
            false,
            None,
            None,
            Some(Duration::ZERO),
        )
        .unwrap_err();
        assert!(err.contains("timed out"), "unexpected error: {err}");
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
        let options =
            build_anki_audio_clip_options(&media, "/tmp/clip.aac", Some(-16.5), false).unwrap();
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
        assert!(
            build_anki_video_clip_options(&zero_aid, 0, true, true, None, "/tmp/clip.mp4").is_err()
        );

        let valid = MediaContext::new("/tmp/movie.mkv", 1);
        assert!(validate_audio_clip_args(&valid, -0.3, 1.0).is_err());
        assert!(validate_audio_clip_args(&valid, -0.001, 1.0).is_err());
        assert!(validate_audio_clip_args(&valid, -0.0, 1.0).is_ok());
        assert_eq!(
            build_loadfile_options(-0.0, 1.0, Some(1), None),
            "start=0.000,end=1.000,aid=1"
        );
        assert!(extract_clip_audio(&valid, -0.3, 1.0, None).is_err());
        assert!(extract_clip_wav(&valid, -0.3, 1.0, None).is_err());
        assert!(extract_clip_mp4(&valid, 1, -0.3, 1.0, false, false, None).is_err());
    }

    #[test]
    fn streaming_encode_context_forwards_all_properties_and_split_dash_audio() {
        let encode_ctx = EncodeContext {
            path: "https://www.youtube.com/watch?v=dQw4w9WgXcQ".to_owned(),
            audio_id: 2,
            stream_open_filename: "edl://!no_clip;https://rr1.googlevideo.com/videoplayback"
                .to_owned(),
            script_opts: "ytdl_hook-ytdl_path=yt-dlp".to_owned(),
            http_header_fields: vec![
                "Authorization: Bearer tok123".to_owned(),
                "Accept: text/html, application/xhtml+xml".to_owned(),
                "   ".to_owned(),
            ],
            referrer: "https://www.youtube.com/".to_owned(),
            user_agent: "Subomi/2.0.2".to_owned(),
            cookies: true,
            cookies_file: "/tmp/cookies.txt".to_owned(),
            ytdl_raw_options: "cookies-from-browser=firefox".to_owned(),
            external_audio_filename: Some(
                "https://rr1.googlevideo.com/audioplayback?itag=251".to_owned(),
            ),
        };
        let media = MediaContext::from(encode_ctx);
        assert_eq!(
            media.effective_media_path(),
            "edl://!no_clip;https://rr1.googlevideo.com/videoplayback"
        );
        assert_eq!(
            media.effective_external_audio(),
            Some("https://rr1.googlevideo.com/audioplayback?itag=251")
        );

        let audio_opts = build_anki_audio_clip_options(&media, "/tmp/clip.aac", None, false)
            .expect("streaming audio options");
        assert_eq!(option_value(&audio_opts, "ytdl"), Some("yes"));
        assert_eq!(option_value(&audio_opts, "aid"), Some("auto"));
        assert_eq!(
            option_value(&audio_opts, "script-opts"),
            Some("ytdl_hook-ytdl_path=yt-dlp")
        );
        assert_eq!(
            option_value(&audio_opts, "referrer"),
            Some("https://www.youtube.com/")
        );
        assert_eq!(
            option_value(&audio_opts, "user-agent"),
            Some("Subomi/2.0.2")
        );
        assert_eq!(option_value(&audio_opts, "cookies"), Some("yes"));
        assert_eq!(
            option_value(&audio_opts, "cookies-file"),
            Some("/tmp/cookies.txt")
        );
        assert_eq!(
            option_value(&audio_opts, "ytdl-raw-options"),
            Some("cookies-from-browser=firefox")
        );
        assert_eq!(
            option_value(&audio_opts, "audio-files"),
            Some("https://rr1.googlevideo.com/audioplayback?itag=251")
        );
        assert_eq!(
            build_loadfile_options(1.0, 2.0, loadfile_audio_id(&media, true), None),
            "start=1.000,end=2.000"
        );

        // Video clip without audio must NOT attach external `audio-files`, while still keeping headers/cookies.
        let video_no_audio =
            build_anki_video_clip_options(&media, 0, false, false, None, "/tmp/clip.mp4").unwrap();
        assert_eq!(option_value(&video_no_audio, "aid"), Some("no"));
        assert_eq!(option_value(&video_no_audio, "audio-files"), None);
        assert_eq!(
            option_value(&video_no_audio, "user-agent"),
            Some("Subomi/2.0.2")
        );

        // Verify libmpv accepts every configured streaming option & list option on a real handle.
        let raw_handle = unsafe { mpv::mpv_create() };
        assert!(!raw_handle.is_null());
        let handle = MpvHandle(raw_handle);
        apply_encode_handle_options(
            handle.0,
            &audio_opts,
            &media.http_header_fields,
            "streaming unit test",
        )
        .expect("libmpv must accept all streaming options and http-header-fields");
    }

    #[test]
    fn internal_asr_uri_is_ignored_for_effective_media_path_and_external_audio() {
        let mut media = MediaContext::new("/tmp/local.mkv", 1);
        media.stream_open_filename = "subomi-asr://live.vtt".to_owned();
        media.external_audio_filename = Some("subomi-asr://live.vtt".to_owned());
        assert_eq!(media.effective_media_path(), "/tmp/local.mkv");
        assert_eq!(media.effective_external_audio(), None);
        assert_eq!(loadfile_audio_id(&media, true), Some(1));
    }

    #[test]
    #[ignore = "requires ffmpeg and libmpv WAV encoding"]
    fn encodes_anki_sentence_audio_without_asr() {
        let path = std::env::temp_dir().join(format!(
            "subomi-anki-audio-test-{}.wav",
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
            "subomi-anki-video-test-{}.mp4",
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

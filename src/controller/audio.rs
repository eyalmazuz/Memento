//! Pronunciation audio source resolution and playback on an independent mpv instance.
//!
//! Network access, file copying, hashing, and mpv playback run on one worker
//! thread. The completion callback also runs on that worker after mpv reports
//! that playback has restarted, leaving UI-thread dispatch to the caller.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use memento_audio::audiofiles::{
    expand_url_template, expand_url_template_encoded, md5_matches_skip_hash,
    parse_audio_source_list,
};
use memento_audio::audioplayer::{
    detect_audio_extension, validate_audio_bytes, validate_audio_content_type, AudioCache,
    AUDIO_CACHE_CAPACITY, MAX_AUDIO_PAYLOAD_BYTES,
};
use memento_net::http::{HttpClient, HttpClientConfig, Request, Response, Url as HttpUrl};
use memento_player::AudioInstance;
use memento_setting::enums::AudioSourceType;
use url::Url;

use super::config::AudioSourceEntry;

const NETWORK_TIMEOUT: Duration = Duration::from_secs(30);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_JSON_SOURCE_BYTES: u64 = 8 * 1024 * 1024;
const AUDIO_USER_AGENT: &str = "Memento";
static NEXT_TEMP_FILE_ID: AtomicU64 = AtomicU64::new(0);

type Completion = Box<dyn FnOnce(Result<(), String>) + Send + 'static>;

struct AudioRequest {
    generation: u64,
    expression: String,
    reading: String,
    sources: Vec<AudioSourceEntry>,
    completion: Completion,
}

trait PlaybackBackend {
    fn play_file(&mut self, path: &Path) -> Result<(), String>;
}

impl PlaybackBackend for AudioInstance {
    fn play_file(&mut self, path: &Path) -> Result<(), String> {
        AudioInstance::play_file(self, path)
    }
}

/// Asynchronous pronunciation player. Its libmpv instance is independent from
/// the movie player's instance, so loading audio cannot replace or pause video.
#[derive(Clone)]
pub struct AudioController {
    requests: Sender<AudioRequest>,
    latest_generation: Arc<AtomicU64>,
}

impl AudioController {
    /// Starts the dedicated audio worker and initializes its audio-only mpv
    /// instance. Failure to initialize mpv is returned to the caller.
    pub fn new() -> Result<Self, String> {
        let (requests, receiver) = mpsc::channel();
        let latest_generation = Arc::new(AtomicU64::new(0));
        let worker_generation = Arc::clone(&latest_generation);
        let (ready_sender, ready_receiver) = mpsc::sync_channel(0);
        thread::Builder::new()
            .name("memento-audio".to_owned())
            .spawn(move || match AudioWorker::new(worker_generation) {
                Ok(mut worker) => {
                    if ready_sender.send(Ok(())).is_ok() {
                        worker.run(receiver);
                    }
                }
                Err(error) => {
                    let _ = ready_sender.send(Err(error));
                }
            })
            .map_err(|error| format!("could not start audio worker: {error}"))?;

        match ready_receiver.recv() {
            Ok(Ok(())) => Ok(Self {
                requests,
                latest_generation,
            }),
            Ok(Err(error)) => Err(error),
            Err(_) => Err("audio worker stopped before initialization completed".into()),
        }
    }

    #[cfg(test)]
    fn with_backend<P>(backend: P, http_config: HttpClientConfig) -> Result<Self, String>
    where
        P: PlaybackBackend + Send + 'static,
    {
        let (requests, receiver) = mpsc::channel();
        let latest_generation = Arc::new(AtomicU64::new(0));
        let worker_generation = Arc::clone(&latest_generation);
        let (ready_sender, ready_receiver) = mpsc::sync_channel(0);
        thread::Builder::new()
            .name("memento-audio-test".to_owned())
            .spawn(move || {
                let mut worker = AudioWorker::with_backend(
                    backend,
                    HttpClient::new(http_config),
                    worker_generation,
                );
                if ready_sender.send(Ok(())).is_ok() {
                    worker.run(receiver);
                }
            })
            .map_err(|error| format!("could not start test audio worker: {error}"))?;

        match ready_receiver.recv() {
            Ok(Ok(())) => Ok(Self {
                requests,
                latest_generation,
            }),
            Ok(Err(error)) => Err(error),
            Err(_) => Err("test audio worker stopped before initialization completed".into()),
        }
    }

    /// Resolve and play the first valid recording from the ordered configured
    /// sources without blocking the UI.
    ///
    /// Unavailable sources, empty/non-audio responses, skip-hash matches, and
    /// unplayable candidates fall through cleanly to subsequent sources. Rapid
    /// consecutive calls supersede earlier queued requests so only the latest
    /// requested pronunciation plays. `callback` runs on the audio worker after
    /// playback starts, is superseded, or fails. The returned error only means
    /// the worker could not accept the request.
    pub fn play_word(
        &self,
        expression: impl Into<String>,
        reading: impl Into<String>,
        sources: Vec<AudioSourceEntry>,
        callback: impl FnOnce(Result<(), String>) + Send + 'static,
    ) -> Result<(), String> {
        let generation = self.latest_generation.fetch_add(1, Ordering::AcqRel) + 1;
        self.requests
            .send(AudioRequest {
                generation,
                expression: expression.into(),
                reading: reading.into(),
                sources,
                completion: Box::new(callback),
            })
            .map_err(|_| "audio worker is unavailable".to_owned())
    }
}

/// Sanitize an Anki media filename so every character is strictly in `[A-Za-z0-9._-]`.
pub(crate) fn sanitize_anki_filename(raw: &str, default_ext: &str) -> String {
    let mut sanitized = String::with_capacity(raw.len());
    for ch in raw.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-') {
            sanitized.push(ch);
        } else if ch.is_whitespace() {
            sanitized.push('_');
        }
    }
    while sanitized.contains("..") {
        sanitized = sanitized.replace("..", ".");
    }
    let sanitized = sanitized.trim_matches('.').to_owned();
    if sanitized.is_empty() {
        let ext = default_ext.trim_start_matches('.');
        return format!("memento_media.{ext}");
    }
    sanitized
}

fn ascii_safe_audio_filename_with_ext(
    source_name: &str,
    reading: &str,
    expression: &str,
    payload_md5_hex: Option<&str>,
    extension: &str,
) -> String {
    let mut safe_source = String::new();
    let mut prev_underscore = false;
    for ch in source_name.chars() {
        if ch.is_ascii_alphanumeric() || ch == '-' {
            safe_source.push(ch);
            prev_underscore = false;
        } else if (ch == ' ' || ch == '_') && !safe_source.is_empty() && !prev_underscore {
            safe_source.push('_');
            prev_underscore = true;
        }
    }
    let safe_source = safe_source.trim_matches('_');

    let hash_hex = match payload_md5_hex
        .filter(|hex| !hex.is_empty() && hex.bytes().all(|b| b.is_ascii_hexdigit()))
    {
        Some(hex) => hex.to_ascii_lowercase(),
        None => {
            let key = format!("{source_name}\0{reading}\0{expression}");
            memento_util::fileutils::calculate_md5(key.as_bytes())
                .map(md5_hex)
                .unwrap_or_else(|_| "00000000000000000000000000000000".to_owned())
        }
    };

    let clean_ext = extension.trim_start_matches('.');
    let ext = if clean_ext.is_empty() { "mp3" } else { clean_ext };

    let raw = if safe_source.is_empty() {
        format!("memento_{hash_hex}.{ext}")
    } else {
        format!("memento_{safe_source}_{hash_hex}.{ext}")
    };
    sanitize_anki_filename(&raw, &format!(".{ext}"))
}

/// Build an ASCII-safe (`[A-Za-z0-9._-]`) Anki filename for word pronunciation audio.
pub fn ascii_safe_audio_filename(
    source_name: &str,
    reading: &str,
    expression: &str,
    payload_md5_hex: Option<&str>,
) -> String {
    ascii_safe_audio_filename_with_ext(source_name, reading, expression, payload_md5_hex, ".mp3")
}

fn default_audio_http_client() -> HttpClient {
    HttpClient::new(HttpClientConfig {
        timeout: Some(NETWORK_TIMEOUT),
        connect_timeout: Some(CONNECT_TIMEOUT),
        ..HttpClientConfig::default()
    })
}

fn http_get(http: &HttpClient, url: &str, description: &str) -> Result<Response, String> {
    let parsed = HttpUrl::parse(url)
        .map_err(|error| format!("invalid {description} URL {url:?}: {error}"))?;
    let request = Request::get(parsed).header("User-Agent", AUDIO_USER_AGENT);
    let response = http
        .send(&request)
        .map_err(|error| format!("could not request {description} from {url:?}: {error}"))?;
    if !(200..300).contains(&response.status()) {
        return Err(format!(
            "{description} request to {url:?} returned HTTP {} {}",
            response.status(),
            response.reason()
        ));
    }
    Ok(response)
}

fn read_json_source_body(http: &HttpClient, source_url: &str) -> Result<(Vec<u8>, Option<Url>), String> {
    match classify_url(source_url)? {
        SourceLocation::File(path) => {
            let body = fs::read(&path).map_err(|error| {
                format!(
                    "could not read audio source JSON {}: {error}",
                    path.display()
                )
            })?;
            Ok((body, None))
        }
        SourceLocation::Http(url) => {
            let base = Url::parse(&url).ok();
            let mut response = http_get(http, &url, "audio source JSON")?;
            let body = response
                .read_all(MAX_JSON_SOURCE_BYTES)
                .map_err(|error| format!("could not read audio source JSON response: {error}"))?;
            Ok((body, base))
        }
    }
}

fn expand_audio_source_candidates(
    http: &HttpClient,
    source: &AudioSourceEntry,
    expression: &str,
    reading: &str,
) -> Result<Vec<(String, String)>, String> {
    let source_url = expand_source_url(&source.url, expression, reading)?;
    if source_url.trim().is_empty() {
        return Ok(Vec::new());
    }
    let Some(kind) = i32::try_from(source.kind)
        .ok()
        .and_then(|kind| AudioSourceType::try_from(kind).ok())
    else {
        return Ok(Vec::new());
    };
    match kind {
        AudioSourceType::AudioSourceTypeFile => Ok(vec![(source.name.clone(), source_url)]),
        AudioSourceType::AudioSourceTypeJson => {
            let (body, base_http_url) = read_json_source_body(http, &source_url)?;
            let entries = parse_json_audio_entries(&body)?;
            let resolved = entries
                .into_iter()
                .filter_map(|(entry_name, raw_url)| {
                    let url = resolve_json_candidate_url(
                        base_http_url.as_ref(),
                        &raw_url,
                        expression,
                        reading,
                    )?;
                    let display_name = if entry_name.trim().is_empty() {
                        source.name.clone()
                    } else {
                        entry_name
                    };
                    Some((display_name, url))
                })
                .collect();
            Ok(resolved)
        }
    }
}

/// Resolve the first configured pronunciation source for AnkiConnect's
/// `addNote.audio` attachment. JSON source discovery runs on the caller's
/// background thread; the player remains free to continue rendering.
pub fn resolve_word_audio(
    expression: &str,
    reading: &str,
    sources: &[AudioSourceEntry],
) -> Result<(String, String, String), String> {
    let http = default_audio_http_client();
    for source in sources {
        let Ok(candidates) = expand_audio_source_candidates(&http, source, expression, reading)
        else {
            continue;
        };
        if let Some((candidate_name, resolved_url)) = candidates.into_iter().next() {
            let ext = detect_audio_extension(&[], None, &resolved_url);
            let filename =
                ascii_safe_audio_filename_with_ext(&candidate_name, reading, expression, None, ext);
            return Ok((resolved_url, filename, source.md5.clone()));
        }
    }
    Err("configured audio sources returned no audio files".into())
}

/// Expand every configured source into the playable files shown in a term's
/// audio menu. A JSON source can contribute more than one named recording.
pub fn enumerate_word_audio_sources(
    expression: &str,
    reading: &str,
    sources: &[AudioSourceEntry],
) -> Vec<AudioSourceEntry> {
    let http = default_audio_http_client();
    let mut available = Vec::new();
    for source in sources {
        let Ok(candidates) = expand_audio_source_candidates(&http, source, expression, reading)
        else {
            continue;
        };
        available.extend(candidates.into_iter().map(|(name, url)| AudioSourceEntry {
            name,
            url,
            kind: AudioSourceType::AudioSourceTypeFile as i64,
            md5: source.md5.clone(),
        }));
    }
    available
}

fn fetch_audio_payload(http: &HttpClient, url: &str) -> Result<(Vec<u8>, Option<String>), String> {
    match classify_url(url)? {
        SourceLocation::File(path) => {
            let meta = fs::metadata(&path).map_err(|error| {
                format!("could not read word audio {}: {error}", path.display())
            })?;
            if meta.len() == 0 {
                return Err("word audio source returned an empty file".into());
            }
            if meta.len() > MAX_AUDIO_PAYLOAD_BYTES {
                return Err(format!(
                    "word audio file {} exceeds maximum size ({MAX_AUDIO_PAYLOAD_BYTES} bytes)",
                    path.display()
                ));
            }
            let bytes = fs::read(&path).map_err(|error| {
                format!("could not read word audio {}: {error}", path.display())
            })?;
            Ok((bytes, None))
        }
        SourceLocation::Http(url) => {
            let mut response = http_get(http, &url, "word audio")?;
            let content_type = response.header("content-type").map(str::to_owned);
            validate_audio_content_type(content_type.as_deref())?;
            if let Some(len) = response.content_length() {
                if len == 0 {
                    return Err("word audio source returned an empty file".into());
                }
                if len > MAX_AUDIO_PAYLOAD_BYTES {
                    return Err(format!(
                        "word audio exceeds maximum size ({MAX_AUDIO_PAYLOAD_BYTES} bytes)"
                    ));
                }
            }
            let bytes = response
                .read_all(MAX_AUDIO_PAYLOAD_BYTES)
                .map_err(|error| format!("could not read word audio: {error}"))?;
            Ok((bytes, content_type))
        }
    }
}

/// Fetch word audio before note creation. Iterates all configured
/// `AudioSourceEntry` items (and JSON `audioSourceList` entries) in order,
/// skipping failed sources, empty or non-audio/HTML responses, and
/// case-insensitive MD5 skip-hash matches to return the first valid audio
/// payload with an ASCII-safe filename.
pub fn fetch_word_audio(
    expression: &str,
    reading: &str,
    sources: &[AudioSourceEntry],
) -> Result<(Vec<u8>, String), String> {
    let http = default_audio_http_client();
    fetch_word_audio_with_client(&http, expression, reading, sources)
}

fn fetch_word_audio_with_client(
    http: &HttpClient,
    expression: &str,
    reading: &str,
    sources: &[AudioSourceEntry],
) -> Result<(Vec<u8>, String), String> {
    let mut last_error: Option<String> = None;

    for source in sources {
        let candidates = match expand_audio_source_candidates(http, source, expression, reading) {
            Ok(candidates) => candidates,
            Err(error) => {
                last_error.get_or_insert(error);
                continue;
            }
        };

        for (candidate_name, candidate_url) in candidates {
            let (bytes, content_type) = match fetch_audio_payload(http, &candidate_url) {
                Ok(payload) => payload,
                Err(error) => {
                    last_error = Some(error);
                    continue;
                }
            };
            if bytes.is_empty() {
                last_error = Some("word audio source returned an empty file".into());
                continue;
            }
            let digest = memento_util::fileutils::calculate_md5(bytes.as_slice())
                .map_err(|error| format!("could not hash word audio: {error}"))?;
            let hex = md5_hex(digest);
            if md5_matches_skip_hash(&hex, &source.md5) {
                last_error = Some(skip_hash_message());
                continue;
            }
            if let Err(error) = validate_audio_bytes(&bytes) {
                last_error = Some(error);
                continue;
            }
            let ext = detect_audio_extension(&bytes, content_type.as_deref(), &candidate_url);
            let filename = ascii_safe_audio_filename_with_ext(
                &candidate_name,
                reading,
                expression,
                Some(&hex),
                ext,
            );
            return Ok((bytes, filename));
        }
    }

    Err(last_error.unwrap_or_else(|| "configured audio sources returned no audio files".into()))
}

// Field order is deliberate: mpv (`player`) is terminated before its cached or
// active temporary files are removed when the worker exits.
struct AudioWorker<P: PlaybackBackend = AudioInstance> {
    player: P,
    cache: AudioCache<Arc<TemporaryAudioFile>>,
    active_file: Option<Arc<TemporaryAudioFile>>,
    http: HttpClient,
    latest_generation: Arc<AtomicU64>,
}

impl AudioWorker<AudioInstance> {
    fn new(latest_generation: Arc<AtomicU64>) -> Result<Self, String> {
        let player = AudioInstance::new()?;
        Ok(Self::with_backend(
            player,
            default_audio_http_client(),
            latest_generation,
        ))
    }
}

impl<P: PlaybackBackend> AudioWorker<P> {
    fn with_backend(player: P, http: HttpClient, latest_generation: Arc<AtomicU64>) -> Self {
        Self {
            player,
            cache: AudioCache::new(AUDIO_CACHE_CAPACITY),
            active_file: None,
            http,
            latest_generation,
        }
    }

    fn is_superseded(&self, generation: u64) -> bool {
        self.latest_generation.load(Ordering::Acquire) != generation
    }

    fn run(&mut self, receiver: mpsc::Receiver<AudioRequest>) {
        while let Ok(mut request) = receiver.recv() {
            // Coalesce bursts of rapid play clicks so only the latest request runs.
            while let Ok(newer) = receiver.try_recv() {
                (request.completion)(Ok(()));
                request = newer;
            }
            if self.is_superseded(request.generation) {
                (request.completion)(Ok(()));
                continue;
            }
            let result = self.play_word(
                request.generation,
                &request.expression,
                &request.reading,
                &request.sources,
            );
            (request.completion)(result);
        }
    }

    fn play_word(
        &mut self,
        generation: u64,
        expression: &str,
        reading: &str,
        sources: &[AudioSourceEntry],
    ) -> Result<(), String> {
        let mut last_error: Option<String> = None;
        for source in sources {
            if self.is_superseded(generation) {
                return Ok(());
            }
            let candidates =
                match expand_audio_source_candidates(&self.http, source, expression, reading) {
                    Ok(candidates) => candidates,
                    Err(error) => {
                        last_error.get_or_insert(error);
                        continue;
                    }
                };

            let skip_hash = source.md5.trim();
            for (_, candidate_url) in candidates {
                if self.is_superseded(generation) {
                    return Ok(());
                }
                match self.play_url(generation, &candidate_url, skip_hash) {
                    Ok(()) => return Ok(()),
                    Err(error) => {
                        last_error = Some(error);
                    }
                }
            }
        }
        if self.is_superseded(generation) {
            return Ok(());
        }
        Err(last_error.unwrap_or_else(|| "configured audio sources returned no audio files".into()))
    }

    fn play_url(&mut self, generation: u64, url: &str, skip_hash: &str) -> Result<(), String> {
        // 1. Check LRU cache (including cached skip-hash entries and self-healing if a
        //    temp file was externally deleted from disk).
        enum CacheHit {
            SkipHash,
            Play(Arc<TemporaryAudioFile>),
            StaleOrMissingFile,
            Miss,
        }
        let hit = match self.cache.get_and_touch(url) {
            None => CacheHit::Miss,
            Some(entry) => {
                if md5_matches_skip_hash(&entry.md5, skip_hash) {
                    CacheHit::SkipHash
                } else if let Some(file) = &entry.file {
                    if file.path().is_file() {
                        CacheHit::Play(Arc::clone(file))
                    } else {
                        CacheHit::StaleOrMissingFile
                    }
                } else {
                    // Previously cached as a skip-hash-only entry, but the user cleared or
                    // changed `skip_hash` so we must re-download the actual bytes now.
                    CacheHit::StaleOrMissingFile
                }
            }
        };

        match hit {
            CacheHit::SkipHash => return Err(skip_hash_message()),
            CacheHit::Play(file) => {
                if self.is_superseded(generation) {
                    return Ok(());
                }
                self.player.play_file(file.path())?;
                self.active_file = Some(file);
                return Ok(());
            }
            CacheHit::StaleOrMissingFile => {
                let _ = self.cache.remove(url);
            }
            CacheHit::Miss => {}
        }

        // 2. Download, validate, and hash audio payload.
        let (file, md5) = self.download_audio(url, skip_hash)?;
        if md5_matches_skip_hash(&md5, skip_hash) {
            // Retain only the MD5 in cache and drop the temporary file immediately
            // so skipped placeholders do not linger on disk.
            drop(file);
            let _evicted = self.cache.insert(url.to_owned(), None, md5);
            return Err(skip_hash_message());
        }

        let file = Arc::new(file);
        if self.is_superseded(generation) {
            // Keep the validated download warm in cache for future clicks without
            // interrupting the newer request.
            let _evicted = self.cache.insert(url.to_owned(), Some(file), md5);
            return Ok(());
        }

        // 3. Start playback BEFORE caching/swapping `active_file` so unplayable
        //    corrupt files never poison the cache and the previous audio file stays
        //    on disk until libmpv switches to the new one.
        self.player.play_file(file.path())?;
        let _evicted = self
            .cache
            .insert(url.to_owned(), Some(Arc::clone(&file)), md5);
        self.active_file = Some(file);
        Ok(())
    }

    fn download_audio(
        &self,
        url: &str,
        skip_hash: &str,
    ) -> Result<(TemporaryAudioFile, String), String> {
        match classify_url(url)? {
            SourceLocation::File(path) => {
                let meta = fs::metadata(&path).map_err(|error| {
                    format!(
                        "could not open pronunciation audio file {}: {error}",
                        path.display()
                    )
                })?;
                if meta.len() == 0 {
                    return Err("pronunciation audio source returned an empty file".into());
                }
                if meta.len() > MAX_AUDIO_PAYLOAD_BYTES {
                    return Err(format!(
                        "pronunciation audio file {} exceeds maximum size ({MAX_AUDIO_PAYLOAD_BYTES} bytes)",
                        path.display()
                    ));
                }
                let mut input = File::open(&path).map_err(|error| {
                    format!(
                        "could not open pronunciation audio file {}: {error}",
                        path.display()
                    )
                })?;
                let mut sniff = Vec::new();
                let ext = detect_audio_extension(&[], None, url);
                let (temporary, mut output) = TemporaryAudioFile::create(ext)?;
                let mut buf = [0u8; 8192];
                let mut copied = 0u64;
                loop {
                    let read = io::Read::read(&mut input, &mut buf).map_err(|error| {
                        format!(
                            "could not copy pronunciation audio file {}: {error}",
                            path.display()
                        )
                    })?;
                    if read == 0 {
                        break;
                    }
                    let chunk = &buf[..read];
                    copied += read as u64;
                    if copied > MAX_AUDIO_PAYLOAD_BYTES {
                        return Err(format!(
                            "pronunciation audio exceeds maximum size ({MAX_AUDIO_PAYLOAD_BYTES} bytes)"
                        ));
                    }
                    if sniff.len() < 512 {
                        let take = (512 - sniff.len()).min(chunk.len());
                        sniff.extend_from_slice(&chunk[..take]);
                    }
                    output.write_all(chunk).map_err(|error| {
                        format!(
                            "could not copy pronunciation audio file {}: {error}",
                            path.display()
                        )
                    })?;
                }
                output.flush().map_err(|error| {
                    format!("could not finish pronunciation audio file: {error}")
                })?;
                drop(output);
                if copied == 0 {
                    return Err("pronunciation audio source returned an empty file".into());
                }
                let digest = memento_util::fileutils::calculate_md5(
                    File::open(temporary.path()).map_err(|error| {
                        format!("could not reopen pronunciation audio file: {error}")
                    })?,
                )
                .map_err(|error| format!("could not hash pronunciation audio file: {error}"))?;
                let hex = md5_hex(digest);
                if !md5_matches_skip_hash(&hex, skip_hash) {
                    validate_audio_bytes(&sniff)?;
                }
                Ok((temporary, hex))
            }
            SourceLocation::Http(url) => {
                let mut response = http_get(&self.http, &url, "pronunciation audio")?;
                let content_type = response.header("content-type").map(str::to_owned);
                validate_audio_content_type(content_type.as_deref())?;
                if let Some(len) = response.content_length() {
                    if len == 0 {
                        return Err("pronunciation audio source returned an empty file".into());
                    }
                    if len > MAX_AUDIO_PAYLOAD_BYTES {
                        return Err(format!(
                            "pronunciation audio exceeds maximum size ({MAX_AUDIO_PAYLOAD_BYTES} bytes)"
                        ));
                    }
                }

                // Delay creating a temporary file until we receive the first non-empty chunk
                // so empty/failed streams do not churn `/tmp`, and pick the real extension
                // from the initial magic bytes + Content-Type + URL.
                let mut temporary: Option<TemporaryAudioFile> = None;
                let mut output: Option<File> = None;
                let mut sniff = Vec::with_capacity(512);
                let mut written = 0u64;
                let stream_res = response.stream_chunks(&mut |chunk| {
                    if chunk.is_empty() {
                        return Ok(());
                    }
                    written = written.saturating_add(chunk.len() as u64);
                    if written > MAX_AUDIO_PAYLOAD_BYTES {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            format!(
                                "pronunciation audio exceeds maximum size ({MAX_AUDIO_PAYLOAD_BYTES} bytes)"
                            ),
                        ));
                    }
                    if sniff.len() < 512 {
                        let take = (512 - sniff.len()).min(chunk.len());
                        sniff.extend_from_slice(&chunk[..take]);
                    }
                    if output.is_none() {
                        let ext =
                            detect_audio_extension(&sniff, content_type.as_deref(), &url);
                        let (temp, file) = TemporaryAudioFile::create(ext)
                            .map_err(io::Error::other)?;
                        temporary = Some(temp);
                        output = Some(file);
                    }
                    output
                        .as_mut()
                        .expect("temp file initialized")
                        .write_all(chunk)
                });
                if let Some(mut file) = output.take() {
                    if stream_res.is_ok() {
                        file.flush().map_err(|error| {
                            format!("could not finish pronunciation audio file: {error}")
                        })?;
                    }
                }
                stream_res.map_err(|error| {
                    format!("could not download pronunciation audio: {error}")
                })?;

                let Some(temporary) = temporary else {
                    return Err("pronunciation audio source returned an empty file".into());
                };
                if written == 0 {
                    return Err("pronunciation audio source returned an empty file".into());
                }

                let digest = memento_util::fileutils::calculate_md5(
                    File::open(temporary.path()).map_err(|error| {
                        format!("could not reopen pronunciation audio file: {error}")
                    })?,
                )
                .map_err(|error| format!("could not hash pronunciation audio file: {error}"))?;
                let hex = md5_hex(digest);
                if !md5_matches_skip_hash(&hex, skip_hash) {
                    validate_audio_bytes(&sniff)?;
                }
                Ok((temporary, hex))
            }
        }
    }
}

#[cfg(test)]
fn first_audio_candidate(
    expression: &str,
    reading: &str,
    sources: &[AudioSourceEntry],
    mut load_json: impl FnMut(&str) -> Result<Vec<String>, String>,
) -> Result<(String, String), String> {
    for source in sources {
        let source_url = expand_source_url(&source.url, expression, reading)?;
        if source_url.trim().is_empty() {
            continue;
        }
        let Ok(kind) = i32::try_from(source.kind) else {
            continue;
        };
        match AudioSourceType::try_from(kind) {
            Ok(AudioSourceType::AudioSourceTypeFile) => {
                return Ok((source_url, source.md5.clone()));
            }
            Ok(AudioSourceType::AudioSourceTypeJson) => {
                let base_http = has_http_scheme(&source_url)
                    .then(|| Url::parse(&source_url).ok())
                    .flatten();
                if let Ok(urls) = load_json(&source_url) {
                    if let Some(url) = urls.into_iter().find_map(|candidate| {
                        resolve_json_candidate_url(
                            base_http.as_ref(),
                            &candidate,
                            expression,
                            reading,
                        )
                    }) {
                        return Ok((url, source.md5.clone()));
                    }
                }
            }
            Err(_) => {}
        }
    }
    Err("configured audio sources returned no audio files".into())
}

fn skip_hash_message() -> String {
    "pronunciation audio was skipped because its MD5 matches the configured skip hash".into()
}

fn expand_source_url(template: &str, expression: &str, reading: &str) -> Result<String, String> {
    Ok(expand_url_template(template, expression, reading))
}

#[cfg(test)]
fn parse_json_audio_sources(data: &[u8]) -> Result<Vec<String>, String> {
    parse_json_audio_entries(data).map(|entries| entries.into_iter().map(|(_, url)| url).collect())
}

fn parse_json_audio_entries(data: &[u8]) -> Result<Vec<(String, String)>, String> {
    parse_audio_source_list(data).ok_or_else(|| {
        "audio source response is not a valid audioSourceList JSON document".to_owned()
    })
}

enum SourceLocation {
    File(PathBuf),
    Http(String),
}

fn has_http_scheme(value: &str) -> bool {
    value
        .get(..7)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("http://"))
        || value
            .get(..8)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("https://"))
}

fn resolve_json_candidate_url(
    base_http_url: Option<&Url>,
    raw_candidate: &str,
    expression: &str,
    reading: &str,
) -> Option<String> {
    let trimmed = raw_candidate.trim();
    if trimmed.is_empty() || trimmed.chars().any(char::is_control) {
        return None;
    }
    let expanded = expand_url_template_encoded(trimmed, expression, reading);
    let parsed = if has_http_scheme(&expanded) {
        Url::parse(&expanded).ok()?
    } else if let Some(base) = base_http_url {
        base.join(&expanded).ok()?
    } else {
        return None;
    };
    if !matches!(parsed.scheme(), "http" | "https") {
        return None;
    }
    let resolved = parsed.to_string();
    HttpUrl::parse(&resolved).ok()?;
    Some(resolved)
}

fn classify_url(value: &str) -> Result<SourceLocation, String> {
    let value = value.trim();
    if value.is_empty() {
        return Err("audio URL is empty".into());
    }
    if has_http_scheme(value) {
        let parsed = Url::parse(value).map_err(|error| {
            format!("audio URL must be HTTP(S), a file URL, or an existing local path: {error}")
        })?;
        return Ok(SourceLocation::Http(parsed.to_string()));
    }
    let path = Path::new(value);
    if path.is_file() {
        return Ok(SourceLocation::File(path.to_owned()));
    }

    let parsed = Url::parse(value).map_err(|error| {
        format!("audio URL must be HTTP(S), a file URL, or an existing local path: {error}")
    })?;
    match parsed.scheme() {
        "file" => parsed
            .to_file_path()
            .map(SourceLocation::File)
            .map_err(|_| format!("audio file URL does not identify a local file: {value:?}")),
        "http" | "https" => Ok(SourceLocation::Http(parsed.to_string())),
        scheme => Err(format!("unsupported audio URL scheme {scheme:?}")),
    }
}

fn md5_hex(digest: [u8; 16]) -> String {
    use std::fmt::Write as _;
    let mut encoded = String::with_capacity(32);
    for byte in digest {
        write!(&mut encoded, "{byte:02x}").expect("writing to String cannot fail");
    }
    encoded
}

struct TemporaryAudioFile {
    path: PathBuf,
}

impl TemporaryAudioFile {
    fn create(extension: &str) -> Result<(Self, File), String> {
        let directory = std::env::temp_dir();
        let clean_ext = extension
            .trim_start_matches('.')
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .collect::<String>();
        let suffix = if clean_ext.is_empty() {
            "tmp"
        } else {
            clean_ext.as_str()
        };
        for _ in 0..128 {
            let id = NEXT_TEMP_FILE_ID.fetch_add(1, Ordering::Relaxed);
            let path = directory.join(format!("memento-audio-{}-{id}.{suffix}", std::process::id()));
            let mut options = OpenOptions::new();
            options.read(true).write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            match options.open(&path) {
                Ok(file) => return Ok((Self { path }, file)),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => {
                    return Err(format!(
                        "could not create temporary pronunciation audio file {}: {error}",
                        path.display()
                    ));
                }
            }
        }
        Err("could not allocate a unique temporary pronunciation audio file".into())
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TemporaryAudioFile {
    fn drop(&mut self) {
        if let Err(error) = fs::remove_file(&self.path) {
            if error.kind() != io::ErrorKind::NotFound {
                eprintln!(
                    "Could not remove temporary pronunciation audio file {}: {error}",
                    self.path.display()
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        classify_url, enumerate_word_audio_sources, expand_source_url, fetch_word_audio,
        fetch_word_audio_with_client, first_audio_candidate, md5_hex, parse_json_audio_entries,
        parse_json_audio_sources, resolve_word_audio, AudioController, AudioWorker,
        PlaybackBackend, SourceLocation,
    };
    use super::super::config::AudioSourceEntry;
    use memento_net::http::{HttpClient, HttpClientConfig};
    use memento_setting::enums::AudioSourceType;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
    use std::sync::{mpsc, Arc, Mutex};
    use std::time::Duration;

    fn source(name: &str, url: &str, kind: AudioSourceType, md5: &str) -> AudioSourceEntry {
        AudioSourceEntry {
            name: name.to_owned(),
            url: url.to_owned(),
            kind: kind as i64,
            md5: md5.to_owned(),
        }
    }

    fn fast_http_config() -> HttpClientConfig {
        HttpClientConfig {
            timeout: Some(Duration::from_millis(500)),
            connect_timeout: Some(Duration::from_millis(500)),
            ..HttpClientConfig::default()
        }
    }

    fn fast_http_client() -> HttpClient {
        HttpClient::new(fast_http_config())
    }

    #[derive(Clone)]
    struct MockPlayback {
        played: Arc<Mutex<Vec<(PathBuf, Vec<u8>)>>>,
        fail_prefix: Option<Vec<u8>>,
        delay: Duration,
    }

    impl MockPlayback {
        fn new() -> Self {
            Self {
                played: Arc::new(Mutex::new(Vec::new())),
                fail_prefix: None,
                delay: Duration::ZERO,
            }
        }
    }

    impl PlaybackBackend for MockPlayback {
        fn play_file(&mut self, path: &Path) -> Result<(), String> {
            if !self.delay.is_zero() {
                std::thread::sleep(self.delay);
            }
            let bytes = std::fs::read(path)
                .map_err(|e| format!("mock player could not read temp file {}: {e}", path.display()))?;
            if let Some(prefix) = &self.fail_prefix {
                if bytes.starts_with(prefix) {
                    return Err("mock mpv refused corrupt audio".into());
                }
            }
            self.played
                .lock()
                .unwrap()
                .push((path.to_path_buf(), bytes));
            Ok(())
        }
    }

    #[test]
    fn source_templates_use_expression_aliases_and_reading_fallback() {
        assert_eq!(
            expand_source_url("/{expression}/{term}/{reading}", "食べる", "").unwrap(),
            "/食べる/食べる/食べる"
        );
        assert_eq!(
            expand_source_url("/{expression}/{reading}", "食べる", "たべる").unwrap(),
            "/食べる/たべる"
        );
        assert_eq!(
            expand_source_url(
                "https://assets.languagepod101.com/dictionary/japanese/audiomp3.php?kanji={expression}&kana={reading}",
                "A&B#1?+%",
                "  "
            )
            .unwrap(),
            "https://assets.languagepod101.com/dictionary/japanese/audiomp3.php?kanji=A%26B%231%3F%2B%25&kana=A%26B%231%3F%2B%25"
        );
    }

    #[test]
    fn anki_word_audio_uses_configured_source_percent_encoding_and_skip_hash() {
        let sources = vec![source(
            "Japanese Audio",
            "https://audio.example.test/{expression}/{reading}.mp3",
            AudioSourceType::AudioSourceTypeFile,
            "deadbeef",
        )];
        let (url, filename, skip_hash) =
            resolve_word_audio("迎えに行く", "むかえにいく", &sources).unwrap();
        assert_eq!(
            url,
            "https://audio.example.test/%E8%BF%8E%E3%81%88%E3%81%AB%E8%A1%8C%E3%81%8F/%E3%82%80%E3%81%8B%E3%81%88%E3%81%AB%E3%81%84%E3%81%8F.mp3"
        );
        assert!(url.is_ascii());
        assert!(filename.starts_with("memento_Japanese_Audio_"));
        assert_eq!(skip_hash, "deadbeef");
    }

    #[test]
    fn word_audio_http_url_encodes_japanese_before_download() {
        let SourceLocation::Http(url) =
            classify_url("  http://example.test/?expression=休み&reading=やすみ  ").unwrap()
        else {
            panic!("expected an HTTP URL");
        };
        assert!(url.is_ascii());
        assert!(url.contains("expression=%E4%BC%91%E3%81%BF"));
    }

    #[test]
    fn json_audio_entries_keep_order_accept_missing_name_and_reject_invalid_documents() {
        assert_eq!(
            parse_json_audio_entries(
                br#"{"type":"audioSourceList","audioSources":[{"name":"first","url":"https://example.test/1"},{"url":"/relative/2.ogg"}]}"#
            )
            .unwrap(),
            vec![
                ("first".to_owned(), "https://example.test/1".to_owned()),
                (String::new(), "/relative/2.ogg".to_owned()),
            ]
        );
        assert_eq!(
            parse_json_audio_sources(
                br#"{"type":"audioSourceList","audioSources":[{"name":"first","url":"https://example.test/1"},{"name":"second","url":"https://example.test/2"}]}"#
            )
            .unwrap(),
            vec!["https://example.test/1", "https://example.test/2"]
        );
        assert!(parse_json_audio_sources(br#"{"type":"wrong"}"#).is_err());
        assert!(parse_json_audio_sources(b"not json").is_err());
    }

    #[test]
    fn source_menu_expands_every_named_recording_and_falls_back_missing_names() {
        let path = std::env::temp_dir().join(format!(
            "memento-audio-menu-{}-{}.json",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        std::fs::write(
            &path,
            br#"{"type":"audioSourceList","audioSources":[{"name":"speaker A","url":"https://example.test/a.mp3"},{"url":"https://example.test/b.mp3"},{"name":"local_bad","url":"/etc/passwd"}]}"#,
        )
        .unwrap();
        let sources = vec![
            source(
                "dictionary audio",
                &path.to_string_lossy(),
                AudioSourceType::AudioSourceTypeJson,
                "skip",
            ),
            source(
                "fallback",
                "https://example.test/{expression}.mp3",
                AudioSourceType::AudioSourceTypeFile,
                "",
            ),
        ];
        let files = enumerate_word_audio_sources("食べる", "たべる", &sources);
        let _ = std::fs::remove_file(path);
        assert_eq!(
            files
                .iter()
                .map(|file| file.name.as_str())
                .collect::<Vec<_>>(),
            ["speaker A", "dictionary audio", "fallback"]
        );
        assert_eq!(files[1].url, "https://example.test/b.mp3");
        assert_eq!(files[1].md5, "skip");
        assert_eq!(files[2].url, "https://example.test/%E9%A3%9F%E3%81%B9%E3%82%8B.mp3");
    }

    #[test]
    fn source_resolution_skips_failed_and_empty_json_sources_in_order() {
        let sources = vec![
            source(
                "unavailable",
                "https://source.test/unavailable",
                AudioSourceType::AudioSourceTypeJson,
                "first-hash",
            ),
            source(
                "empty",
                "https://source.test/empty",
                AudioSourceType::AudioSourceTypeJson,
                "second-hash",
            ),
            source(
                "ready",
                "https://source.test/ready/{expression}",
                AudioSourceType::AudioSourceTypeJson,
                "third-hash",
            ),
        ];
        let mut requests = Vec::new();

        let candidate = first_audio_candidate("食べる", "たべる", &sources, |url| {
            requests.push(url.to_owned());
            match url {
                "https://source.test/unavailable" => Err("network failure".into()),
                "https://source.test/empty" => Ok(Vec::new()),
                "https://source.test/ready/%E9%A3%9F%E3%81%B9%E3%82%8B" => {
                    Ok(vec!["https://audio.test/first.ogg".into()])
                }
                _ => Err(format!("unexpected source request {url:?}")),
            }
        })
        .unwrap();

        assert_eq!(
            candidate,
            (
                "https://audio.test/first.ogg".to_owned(),
                "third-hash".to_owned()
            )
        );
        assert_eq!(
            requests,
            vec![
                "https://source.test/unavailable".to_owned(),
                "https://source.test/empty".to_owned(),
                "https://source.test/ready/%E9%A3%9F%E3%81%B9%E3%82%8B".to_owned(),
            ]
        );
    }

    #[test]
    fn remote_json_audio_source_rejects_file_urls_and_resolves_relative_http_urls() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = std::thread::spawn(move || {
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut buf = [0u8; 1024];
                let _ = stream.read(&mut buf);
                let body = r#"{"type":"audioSourceList","audioSources":[{"name":"evil_file","url":"file:///etc/passwd"},{"name":"evil_ftp","url":"ftp://example.test/x.mp3"},{"url":"/clips/{expression}.ogg"},{"name":"rel","url":"sub/word.mp3"}]}"#;
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(resp.as_bytes());
            }
        });

        let endpoint = format!("http://{}:{}/api/audio.json", addr.ip(), addr.port());
        let sources = vec![source(
            "Local Audio Server",
            &endpoint,
            AudioSourceType::AudioSourceTypeJson,
            "",
        )];

        let (resolved_url, filename, _) = resolve_word_audio("猫", "ねこ", &sources).unwrap();
        assert_eq!(
            resolved_url,
            format!("http://{}:{}/clips/%E7%8C%AB.ogg", addr.ip(), addr.port())
        );
        assert!(filename.starts_with("memento_Local_Audio_Server_"));
        assert!(filename.ends_with(".ogg"));

        let listed = enumerate_word_audio_sources("猫", "ねこ", &sources);
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0].name, "Local Audio Server");
        assert_eq!(
            listed[0].url,
            format!("http://{}:{}/clips/%E7%8C%AB.ogg", addr.ip(), addr.port())
        );
        assert_eq!(listed[1].name, "rel");
        assert_eq!(
            listed[1].url,
            format!("http://{}:{}/api/sub/word.mp3", addr.ip(), addr.port())
        );

        let _ = handle.join();
    }

    #[test]
    fn remote_json_candidate_rejects_non_http_schemes_even_when_mixed_with_http() {
        let sources = vec![source(
            "remote json",
            "https://source.test/audio.json",
            AudioSourceType::AudioSourceTypeJson,
            "DEADBEEF",
        )];
        let candidate = first_audio_candidate("猫", "ねこ", &sources, |_| {
            Ok(vec![
                "file:///etc/passwd".to_owned(),
                "javascript:alert(1)".to_owned(),
                "https://audio.test/neko.mp3".to_owned(),
            ])
        })
        .unwrap();
        assert_eq!(candidate.0, "https://audio.test/neko.mp3");
        assert!(candidate.1.eq_ignore_ascii_case("deadbeef"));

        let err = first_audio_candidate("猫", "ねこ", &sources, |_| {
            Ok(vec![
                "file:///etc/passwd".to_owned(),
                "ftp://evil.test/x.mp3".to_owned(),
            ])
        })
        .unwrap_err();
        assert!(err.contains("no audio files"));
    }

    #[test]
    fn first_audio_candidate_skips_empty_and_whitespace_file_urls() {
        let sources = vec![
            source("empty", "", AudioSourceType::AudioSourceTypeFile, "h1"),
            source("spaces", "   \t  ", AudioSourceType::AudioSourceTypeFile, "h2"),
            source(
                "valid",
                "https://audio.test/{expression}.mp3",
                AudioSourceType::AudioSourceTypeFile,
                "h3",
            ),
        ];
        let (url, md5) = first_audio_candidate("猫", "ねこ", &sources, |_| {
            panic!("JSON loader must not be called for file sources");
        })
        .unwrap();
        assert_eq!(url, "https://audio.test/%E7%8C%AB.mp3");
        assert_eq!(md5, "h3");
    }

    #[test]
    fn fetch_word_audio_iterates_sources_skips_md5_case_insensitively_and_produces_ascii_filename() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let skipped_path = std::env::temp_dir().join(format!(
            "memento-audio-skip-{}-{now}.mp3",
            std::process::id()
        ));
        let valid_path = std::env::temp_dir().join(format!(
            "memento-audio-valid-{}-{now}.mp3",
            std::process::id()
        ));
        let skipped_bytes = b"missing-audio-placeholder";
        let valid_bytes = b"ID3real-audio-payload";
        std::fs::write(&skipped_path, skipped_bytes).unwrap();
        std::fs::write(&valid_path, valid_bytes).unwrap();

        let skip_digest = memento_util::fileutils::calculate_md5(&skipped_bytes[..]).unwrap();
        let skip_upper = md5_hex(skip_digest).to_ascii_uppercase();

        let missing_path = std::env::temp_dir().join(format!(
            "memento-audio-nonexistent-{}-{now}.mp3",
            std::process::id()
        ));

        let sources = vec![
            source(
                "Missing Source",
                &missing_path.to_string_lossy(),
                AudioSourceType::AudioSourceTypeFile,
                "",
            ),
            source(
                "JapanesePod101 日本語",
                &skipped_path.to_string_lossy(),
                AudioSourceType::AudioSourceTypeFile,
                &skip_upper,
            ),
            source(
                "Forvo 日本語!",
                &valid_path.to_string_lossy(),
                AudioSourceType::AudioSourceTypeFile,
                "",
            ),
        ];

        let result = fetch_word_audio("食べる", "たべる", &sources);
        let _ = std::fs::remove_file(&skipped_path);
        let _ = std::fs::remove_file(&valid_path);

        let (payload, filename) = result.expect("must fall through to third valid source");
        assert_eq!(payload, valid_bytes);
        assert!(filename.starts_with("memento_Forvo_"));
        assert!(filename.ends_with(".mp3"));
        assert!(
            filename
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')),
            "filename must be strictly ASCII-safe: {filename}"
        );
    }

    #[test]
    fn resolve_word_audio_supports_term_placeholder_and_ascii_safe_filename() {
        let sources = vec![source(
            "Jisho 辞書 Audio",
            "https://audio.example.test/{term}/{reading}.mp3",
            AudioSourceType::AudioSourceTypeFile,
            "",
        )];
        let (url, filename, _) =
            resolve_word_audio("迎えに行く", "むかえにいく", &sources).unwrap();
        assert_eq!(
            url,
            "https://audio.example.test/%E8%BF%8E%E3%81%88%E3%81%AB%E8%A1%8C%E3%81%8F/%E3%82%80%E3%81%8B%E3%81%88%E3%81%AB%E3%81%84%E3%81%8F.mp3"
        );
        assert!(filename.starts_with("memento_Jisho_Audio_"));
        assert!(
            filename
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')),
            "filename must be strictly ASCII-safe: {filename}"
        );
    }

    #[test]
    fn fetch_word_audio_rejects_html_empty_json_timeout_and_falls_through_to_valid_ogg() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let base = format!("http://{}:{}", addr.ip(), addr.port());

        let server = std::thread::spawn(move || {
            for _ in 0..5 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut buf = [0u8; 2048];
                let n = stream.read(&mut buf).unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]);
                let path = req
                    .lines()
                    .next()
                    .and_then(|line| line.split_whitespace().nth(1))
                    .unwrap_or("");
                if path.starts_with("/html") {
                    let body = b"<!DOCTYPE html><html><body>404 Not Found</body></html>";
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = stream.write_all(resp.as_bytes());
                    let _ = stream.write_all(body);
                } else if path.starts_with("/disguised-html") {
                    let body = b"<html><body>Cloudflare challenge</body></html>";
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = stream.write_all(resp.as_bytes());
                    let _ = stream.write_all(body);
                } else if path.starts_with("/empty") {
                    let _ = stream.write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Type: audio/mpeg\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                    );
                } else if path.starts_with("/stall") {
                    std::thread::sleep(Duration::from_millis(800));
                } else if path.starts_with("/good") {
                    let body = b"OggS\x00\x02valid-ogg-audio";
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: audio/ogg\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = stream.write_all(resp.as_bytes());
                    let _ = stream.write_all(body);
                }
            }
        });

        let sources = vec![
            source("HTML 200", &format!("{base}/html?w={{expression}}"), AudioSourceType::AudioSourceTypeFile, ""),
            source("Disguised HTML", &format!("{base}/disguised-html"), AudioSourceType::AudioSourceTypeFile, ""),
            source("Empty 200", &format!("{base}/empty"), AudioSourceType::AudioSourceTypeFile, ""),
            source("Slow Timeout", &format!("{base}/stall"), AudioSourceType::AudioSourceTypeFile, ""),
            source("Valid OGG", &format!("{base}/good?q={{expression}}&r={{reading}}"), AudioSourceType::AudioSourceTypeFile, ""),
        ];

        let (bytes, filename) =
            fetch_word_audio_with_client(&fast_http_client(), "A&B#1", "", &sources)
                .expect("must skip HTML/empty/timeout and succeed on valid OGG");
        assert_eq!(bytes, b"OggS\x00\x02valid-ogg-audio");
        assert!(filename.starts_with("memento_Valid_OGG_"));
        assert!(filename.ends_with(".ogg"));

        let _ = server.join();
    }

    #[test]
    fn audio_worker_caches_self_heals_avoids_poisoning_and_cleans_up_temp_files() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let base = format!("http://{}:{}", addr.ip(), addr.port());
        let good_hits = Arc::new(AtomicUsize::new(0));
        let skip_hits = Arc::new(AtomicUsize::new(0));
        let good_hits_srv = Arc::clone(&good_hits);
        let skip_hits_srv = Arc::clone(&skip_hits);

        let placeholder_bytes = b"ID3jpod-placeholder-audio";
        let placeholder_md5 =
            md5_hex(memento_util::fileutils::calculate_md5(&placeholder_bytes[..]).unwrap())
                .to_ascii_uppercase();

        let server = std::thread::spawn(move || {
            for _ in 0..8 {
                let Ok((mut stream, _)) = listener.accept() else {
                    break;
                };
                let mut buf = [0u8; 2048];
                let n = stream.read(&mut buf).unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]);
                let path = req
                    .lines()
                    .next()
                    .and_then(|line| line.split_whitespace().nth(1))
                    .unwrap_or("");
                let (ct, body): (&str, &[u8]) = match path {
                    "/skip" => {
                        skip_hits_srv.fetch_add(1, Ordering::Relaxed);
                        ("audio/mpeg", placeholder_bytes)
                    }
                    "/html" => ("text/html", b"<!DOCTYPE html><html>error</html>"),
                    "/corrupt" => ("audio/mpeg", b"BAD_CORRUPT_AUDIO_DATA"),
                    "/good" => {
                        good_hits_srv.fetch_add(1, Ordering::Relaxed);
                        ("audio/mpeg", b"ID3good-audio-track")
                    }
                    "/second" => ("audio/ogg", b"OggS\x00\x02second-track"),
                    _ => ("text/plain", b"not found"),
                };
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: {ct}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(resp.as_bytes());
                let _ = stream.write_all(body);
            }
        });

        let mut mock = MockPlayback::new();
        mock.fail_prefix = Some(b"BAD_CORRUPT".to_vec());
        let played = Arc::clone(&mock.played);
        let gen = Arc::new(AtomicU64::new(1));
        let mut worker = AudioWorker::with_backend(mock, fast_http_client(), Arc::clone(&gen));

        // 1. Falls through skip-hash, HTML 200, and corrupt playback failure to `/good`.
        let sources = vec![
            source("Skip", &format!("{base}/skip"), AudioSourceType::AudioSourceTypeFile, &format!("  {placeholder_md5}  ")),
            source("HTML", &format!("{base}/html"), AudioSourceType::AudioSourceTypeFile, ""),
            source("Corrupt", &format!("{base}/corrupt"), AudioSourceType::AudioSourceTypeFile, ""),
            source("Good", &format!("{base}/good"), AudioSourceType::AudioSourceTypeFile, ""),
        ];
        worker.play_word(1, "食べる", "たべる", &sources).unwrap();
        assert_eq!(good_hits.load(Ordering::Relaxed), 1);
        assert_eq!(skip_hits.load(Ordering::Relaxed), 1);

        let first_temp_path = played.lock().unwrap()[0].0.clone();
        assert!(first_temp_path.is_file(), "active temp file must remain alive after play");
        assert!(
            first_temp_path.extension().and_then(|e| e.to_str()) == Some("mp3"),
            "temp file should carry detected audio extension: {}",
            first_temp_path.display()
        );

        // 2. Second play of `/good` hits cache (no extra HTTP hit), and `/skip` hits skip-hash cache!
        worker.play_word(1, "食べる", "たべる", &sources[..1]).unwrap_err();
        assert_eq!(
            skip_hits.load(Ordering::Relaxed),
            1,
            "cached skip-hash URL must not re-download"
        );
        worker.play_word(1, "食べる", "たべる", &sources[3..]).unwrap();
        assert_eq!(good_hits.load(Ordering::Relaxed), 1, "cached audio must not re-download");

        // 3. Corrupt file was NOT cached (retrying `/corrupt` still fails cleanly without leaking temp file).
        assert!(worker.play_word(1, "x", "x", &sources[2..3]).is_err());

        // 4. External deletion of cached temp file self-heals via re-download!
        std::fs::remove_file(&first_temp_path).unwrap();
        worker.play_word(1, "食べる", "たべる", &sources[3..]).unwrap();
        assert_eq!(good_hits.load(Ordering::Relaxed), 2, "missing cached file must re-download");
        let healed_temp_path = played.lock().unwrap().last().unwrap().0.clone();
        assert!(healed_temp_path.is_file());

        // 5. Clearing skip_hash for previously skipped URL re-downloads and plays cleanly!
        let unskipped_src = vec![source("Unskipped", &format!("{base}/skip"), AudioSourceType::AudioSourceTypeFile, "")];
        worker.play_word(1, "食べる", "たべる", &unskipped_src).unwrap();
        assert_eq!(
            skip_hits.load(Ordering::Relaxed),
            2,
            "clearing skip_hash must re-download the previously skip-cached URL"
        );
        let unskipped_temp_path = played.lock().unwrap().last().unwrap().0.clone();
        assert!(unskipped_temp_path.is_file());

        // 6. Playing `/second` succeeds, and dropping worker unlinks ALL temp files on disk!
        let second_src = vec![source("Second", &format!("{base}/second"), AudioSourceType::AudioSourceTypeFile, "")];
        worker.play_word(1, "猫", "ねこ", &second_src).unwrap();
        let second_temp_path = played.lock().unwrap().last().unwrap().0.clone();
        assert!(second_temp_path.is_file());
        assert_eq!(second_temp_path.extension().and_then(|e| e.to_str()), Some("ogg"));

        drop(worker);
        assert!(
            !healed_temp_path.exists(),
            "cached temp file must be unlinked when worker drops"
        );
        assert!(
            !unskipped_temp_path.exists(),
            "unskipped temp file must be unlinked when worker drops"
        );
        assert!(
            !second_temp_path.exists(),
            "active temp file must be unlinked when worker drops"
        );

        let _ = server.join();
    }

    #[test]
    fn rapid_consecutive_play_requests_coalesce_to_latest_without_hanging() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let mut paths = Vec::new();
        for idx in 0..4 {
            let p = std::env::temp_dir().join(format!(
                "memento-rapid-{}-{now}-{idx}.mp3",
                std::process::id()
            ));
            std::fs::write(&p, format!("ID3track-{idx}").as_bytes()).unwrap();
            paths.push(p);
        }

        let mut mock = MockPlayback::new();
        mock.delay = Duration::from_millis(40);
        let played = Arc::clone(&mock.played);
        let controller = AudioController::with_backend(mock, fast_http_config()).unwrap();

        let (tx, rx) = mpsc::channel();
        for (idx, path) in paths.iter().enumerate() {
            let done = tx.clone();
            controller
                .play_word(
                    format!("word{idx}"),
                    "",
                    vec![source(
                        "Local",
                        &path.to_string_lossy(),
                        AudioSourceType::AudioSourceTypeFile,
                        "",
                    )],
                    move |res| {
                        let _ = done.send((idx, res));
                    },
                )
                .unwrap();
        }
        drop(tx);

        let mut completed = Vec::new();
        for _ in 0..4 {
            completed.push(rx.recv_timeout(Duration::from_secs(3)).unwrap());
        }
        for (_, res) in &completed {
            assert!(res.is_ok(), "coalesced/latest plays must succeed: {res:?}");
        }

        let played_tracks: Vec<Vec<u8>> = played
            .lock()
            .unwrap()
            .iter()
            .map(|(_, b)| b.clone())
            .collect();
        assert!(
            !played_tracks.is_empty(),
            "at least the latest request must have played"
        );
        assert_eq!(
            played_tracks.last().unwrap().as_slice(),
            b"ID3track-3",
            "final played track must be the latest clicked word"
        );
        assert!(
            played_tracks.len() < 4,
            "rapid burst of 4 requests should coalesce superseded middle requests, played: {}",
            played_tracks.len()
        );

        drop(controller);
        for p in paths {
            let _ = std::fs::remove_file(p);
        }
    }
}

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

use memento_net::http::{HttpClient, HttpClientConfig, Request, Response, Url as HttpUrl};
use memento_player::AudioInstance;
use memento_setting::enums::AudioSourceType;
use url::Url;

use super::config::AudioSourceEntry;

const AUDIO_CACHE_CAPACITY: usize = 10;
const NETWORK_TIMEOUT: Duration = Duration::from_secs(30);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
static NEXT_TEMP_FILE_ID: AtomicU64 = AtomicU64::new(0);

type Completion = Box<dyn FnOnce(Result<(), String>) + Send + 'static>;

struct AudioRequest {
    expression: String,
    reading: String,
    sources: Vec<AudioSourceEntry>,
    completion: Completion,
}

/// Asynchronous pronunciation player. Its libmpv instance is independent from
/// the movie player's instance, so loading audio cannot replace or pause video.
#[derive(Clone)]
pub struct AudioController {
    requests: Sender<AudioRequest>,
}

impl AudioController {
    /// Starts the dedicated audio worker and initializes its audio-only mpv
    /// instance. Failure to initialize mpv is returned to the caller.
    pub fn new() -> Result<Self, String> {
        let (requests, receiver) = mpsc::channel();
        let (ready_sender, ready_receiver) = mpsc::sync_channel(0);
        thread::Builder::new()
            .name("memento-audio".to_owned())
            .spawn(move || match AudioWorker::new() {
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
            Ok(Ok(())) => Ok(Self { requests }),
            Ok(Err(error)) => Err(error),
            Err(_) => Err("audio worker stopped before initialization completed".into()),
        }
    }

    /// Resolve and play the first available file from the ordered configured
    /// sources without blocking the UI.
    ///
    /// JSON sources that fail to load or contain no audio entries are skipped,
    /// matching `AudioFiles`; playback failures do not fall through to another
    /// source. `callback` runs on the audio worker after playback starts or an
    /// error occurs. The returned error only means the worker could not accept
    /// the request.
    pub fn play_word(
        &self,
        expression: impl Into<String>,
        reading: impl Into<String>,
        sources: Vec<AudioSourceEntry>,
        callback: impl FnOnce(Result<(), String>) + Send + 'static,
    ) -> Result<(), String> {
        self.requests
            .send(AudioRequest {
                expression: expression.into(),
                reading: reading.into(),
                sources,
                completion: Box::new(callback),
            })
            .map_err(|_| "audio worker is unavailable".to_owned())
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
    let http = HttpClient::new(HttpClientConfig {
        timeout: Some(NETWORK_TIMEOUT),
        connect_timeout: Some(CONNECT_TIMEOUT),
        ..HttpClientConfig::default()
    });
    for source in sources {
        let source_url = expand_source_url(&source.url, expression, reading)?;
        if source_url.trim().is_empty() { continue; }
        let url = match i32::try_from(source.kind).ok().and_then(|kind| AudioSourceType::try_from(kind).ok()) {
            Some(AudioSourceType::AudioSourceTypeFile) => source_url,
            Some(AudioSourceType::AudioSourceTypeJson) => {
                let body = (|| -> Result<Vec<u8>, String> {
                    match classify_url(&source_url)? {
                        SourceLocation::File(path) => fs::read(path).map_err(|error| error.to_string()),
                        SourceLocation::Http(url) => {
                            let parsed = HttpUrl::parse(&url).map_err(|error| error.to_string())?;
                            let mut response = http.send(&Request::get(parsed)).map_err(|error| error.to_string())?;
                            if !(200..300).contains(&response.status()) {
                                return Err(format!("audio source returned HTTP {}", response.status()));
                            }
                            response.read_all(8 * 1024 * 1024).map_err(|error| error.to_string())
                        }
                    }
                })();
                let Ok(body) = body else { continue };
                let Ok(urls) = parse_json_audio_sources(&body) else { continue };
                let Some(url) = urls.into_iter().next() else { continue };
                url
            }
            None => continue,
        };
        let (url_units, name_units) = memento_anki::note::format_audio_url_and_filename(
            &url.encode_utf16().collect::<Vec<_>>(),
            &source.name.encode_utf16().collect::<Vec<_>>(),
            &expression.encode_utf16().collect::<Vec<_>>(),
            &reading.encode_utf16().collect::<Vec<_>>(),
            false,
        );
        return Ok((String::from_utf16_lossy(&url_units), String::from_utf16_lossy(&name_units), source.md5.clone()));
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
    let http = HttpClient::new(HttpClientConfig {
        timeout: Some(NETWORK_TIMEOUT),
        connect_timeout: Some(CONNECT_TIMEOUT),
        ..HttpClientConfig::default()
    });
    let mut available = Vec::new();
    for source in sources {
        let Ok(url) = expand_source_url(&source.url, expression, reading) else { continue };
        if url.trim().is_empty() { continue }
        match i32::try_from(source.kind).ok().and_then(|kind| AudioSourceType::try_from(kind).ok()) {
            Some(AudioSourceType::AudioSourceTypeFile) => available.push(AudioSourceEntry {
                name: source.name.clone(), url, kind: AudioSourceType::AudioSourceTypeFile as i64,
                md5: source.md5.clone(),
            }),
            Some(AudioSourceType::AudioSourceTypeJson) => {
                let body = match classify_url(&url) {
                    Ok(SourceLocation::File(path)) => fs::read(path).ok(),
                    Ok(SourceLocation::Http(url)) => HttpUrl::parse(&url).ok().and_then(|url| {
                        let mut response = http.send(&Request::get(url)).ok()?;
                        (200..300).contains(&response.status()).then(|| response.read_all(8 * 1024 * 1024).ok()).flatten()
                    }),
                    Err(_) => None,
                };
                if let Some(body) = body {
                    if let Ok(files) = parse_json_audio_entries(&body) {
                        available.extend(files.into_iter().map(|(name, url)| AudioSourceEntry {
                            name, url, kind: AudioSourceType::AudioSourceTypeFile as i64,
                            md5: source.md5.clone(),
                        }));
                    }
                }
            }
            None => {},
        }
    }
    available
}

/// Fetch word audio before note creation. Passing raw Japanese text in an
/// AnkiConnect attachment URL lets Anki's HTTP client write an encoding error
/// into the field while still reporting that the note was added successfully.
pub fn fetch_word_audio(
    expression: &str,
    reading: &str,
    sources: &[AudioSourceEntry],
) -> Result<(Vec<u8>, String), String> {
    let (url, filename, skip_hash) = resolve_word_audio(expression, reading, sources)?;
    let bytes = match classify_url(&url)? {
        SourceLocation::File(path) => fs::read(&path)
            .map_err(|error| format!("could not read word audio {}: {error}", path.display()))?,
        SourceLocation::Http(url) => {
            let http = HttpClient::new(HttpClientConfig {
                timeout: Some(NETWORK_TIMEOUT),
                connect_timeout: Some(CONNECT_TIMEOUT),
                ..HttpClientConfig::default()
            });
            let parsed = HttpUrl::parse(&url).map_err(|error| format!("invalid word audio URL: {error}"))?;
            let mut response = http.send(&Request::get(parsed))
                .map_err(|error| format!("word audio download failed: {error}"))?;
            if !(200..300).contains(&response.status()) {
                return Err(format!("word audio download returned HTTP {}", response.status()));
            }
            response.read_all(32 * 1024 * 1024)
                .map_err(|error| format!("could not read word audio: {error}"))?
        }
    };
    if bytes.is_empty() {
        return Err("word audio source returned an empty file".into());
    }
    if !skip_hash.is_empty() {
        let digest = memento_util::fileutils::calculate_md5(bytes.as_slice())
            .map_err(|error| format!("could not hash word audio: {error}"))?;
        if md5_hex(digest).eq_ignore_ascii_case(&skip_hash) {
            return Err(skip_hash_message());
        }
    }
    Ok((bytes, filename))
}

struct CachedAudio {
    url: String,
    file: Arc<TemporaryAudioFile>,
    md5: String,
}

// Field order is deliberate: mpv is terminated before its cached files are
// removed when the worker exits.
struct AudioWorker {
    player: AudioInstance,
    cache: Vec<CachedAudio>,
    active_file: Option<Arc<TemporaryAudioFile>>,
    http: HttpClient,
}

impl AudioWorker {
    fn new() -> Result<Self, String> {
        let player = AudioInstance::new()?;
        let http = HttpClient::new(HttpClientConfig {
            timeout: Some(NETWORK_TIMEOUT),
            connect_timeout: Some(CONNECT_TIMEOUT),
            ..HttpClientConfig::default()
        });
        Ok(Self {
            player,
            cache: Vec::new(),
            active_file: None,
            http,
        })
    }

    fn run(&mut self, receiver: mpsc::Receiver<AudioRequest>) {
        while let Ok(request) = receiver.recv() {
            let result = self.play_word(
                &request.expression,
                &request.reading,
                &request.sources,
            );
            (request.completion)(result);
        }
    }

    fn play_word(
        &mut self,
        expression: &str,
        reading: &str,
        sources: &[AudioSourceEntry],
    ) -> Result<(), String> {
        let (audio_url, skip_hash) = first_audio_candidate(
            expression,
            reading,
            sources,
            |url| self.read_source_body(url).and_then(|body| parse_json_audio_sources(&body)),
        )?;
        self.play_url(&audio_url, &skip_hash)
    }

    fn play_url(&mut self, url: &str, skip_hash: &str) -> Result<(), String> {
        if let Some((file, hash_matches)) = self
            .cache
            .iter()
            .find(|entry| entry.url == url)
            .map(|entry| (Arc::clone(&entry.file), entry.md5 == skip_hash))
        {
            if hash_matches {
                return Err(skip_hash_message());
            }
            self.player.play_file(file.path())?;
            self.active_file = Some(file);
            return Ok(());
        }

        let (file, md5) = self.download_audio(url)?;
        let hash_matches = md5 == skip_hash;
        if self.cache.len() >= AUDIO_CACHE_CAPACITY {
            self.cache.clear();
        }
        let file = Arc::new(file);
        self.cache.push(CachedAudio {
            url: url.to_owned(),
            file: Arc::clone(&file),
            md5,
        });
        if hash_matches {
            return Err(skip_hash_message());
        }

        self.player.play_file(file.path())?;
        self.active_file = Some(file);
        Ok(())
    }

    fn read_source_body(&self, url: &str) -> Result<Vec<u8>, String> {
        match classify_url(url)? {
            SourceLocation::File(path) => fs::read(&path).map_err(|error| {
                format!(
                    "could not read audio source JSON {}: {error}",
                    path.display()
                )
            }),
            SourceLocation::Http(url) => {
                let mut response = self.http_get(&url, "audio source JSON")?;
                response
                    .read_all(u64::MAX)
                    .map_err(|error| format!("could not read audio source JSON response: {error}"))
            }
        }
    }

    fn download_audio(&self, url: &str) -> Result<(TemporaryAudioFile, String), String> {
        let (temporary, mut output) = TemporaryAudioFile::create()?;
        match classify_url(url)? {
            SourceLocation::File(path) => {
                let mut input = File::open(&path).map_err(|error| {
                    format!(
                        "could not open pronunciation audio file {}: {error}",
                        path.display()
                    )
                })?;
                io::copy(&mut input, &mut output).map_err(|error| {
                    format!(
                        "could not copy pronunciation audio file {}: {error}",
                        path.display()
                    )
                })?;
            }
            SourceLocation::Http(url) => {
                let mut response = self.http_get(&url, "pronunciation audio")?;
                response
                    .stream_chunks(&mut |chunk| output.write_all(chunk))
                    .map_err(|error| format!("could not download pronunciation audio: {error}"))?;
            }
        }
        output
            .flush()
            .map_err(|error| format!("could not finish pronunciation audio file: {error}"))?;
        drop(output);

        let digest = memento_util::fileutils::calculate_md5(
            File::open(temporary.path()).map_err(|error| {
                format!("could not reopen pronunciation audio file: {error}")
            })?,
        )
        .map_err(|error| format!("could not hash pronunciation audio file: {error}"))?;
        Ok((temporary, md5_hex(digest)))
    }

    fn http_get(&self, url: &str, description: &str) -> Result<Response, String> {
        let parsed = HttpUrl::parse(url)
            .map_err(|error| format!("invalid {description} URL {url:?}: {error}"))?;
        let response = self
            .http
            .send(&Request::get(parsed))
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
}

fn first_audio_candidate(
    expression: &str,
    reading: &str,
    sources: &[AudioSourceEntry],
    mut load_json: impl FnMut(&str) -> Result<Vec<String>, String>,
) -> Result<(String, String), String> {
    for source in sources {
        let source_url = expand_source_url(&source.url, expression, reading)?;
        let Ok(kind) = i32::try_from(source.kind) else {
            continue;
        };
        match AudioSourceType::try_from(kind) {
            Ok(AudioSourceType::AudioSourceTypeFile) => {
                return Ok((source_url, source.md5.clone()));
            }
            Ok(AudioSourceType::AudioSourceTypeJson) => {
                if let Ok(urls) = load_json(&source_url) {
                    if let Some(url) = urls.into_iter().next() {
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
    if !["{expression}", "{term}", "{reading}"]
        .iter()
        .any(|placeholder| template.contains(placeholder))
    {
        return Ok(template.to_owned());
    }

    let template_units: Vec<u16> = template.encode_utf16().collect();
    let expression_units: Vec<u16> = expression.encode_utf16().collect();
    let reading_units: Vec<u16> = reading.encode_utf16().collect();
    let expanded = memento_audio::audiofiles::expand_url(
        &template_units,
        &expression_units,
        &reading_units,
    );
    String::from_utf16(&expanded)
        .map_err(|error| format!("expanded audio source URL is not valid UTF-16: {error}"))
}

fn parse_json_audio_sources(data: &[u8]) -> Result<Vec<String>, String> {
    parse_json_audio_entries(data).map(|entries| entries.into_iter().map(|(_, url)| url).collect())
}

fn parse_json_audio_entries(data: &[u8]) -> Result<Vec<(String, String)>, String> {
    // The audio crate's parser preserves Qt's JSON/QString UTF-16 behavior,
    // including surrogate escapes. Keep the C-ABI allocation under an RAII
    // guard so all parser outcomes release their nested strings exactly once.
    let entries = unsafe {
        memento_audio::audiofiles::memento_audio_parse_json(data.as_ptr(), data.len())
    };
    let entries = ParsedAudioEntries(entries);
    if !entries.0.valid {
        return Err("audio source response is not a valid audioSourceList JSON document".into());
    }
    if entries.0.len != 0 && entries.0.entries.is_null() {
        return Err("audio source JSON parser returned an invalid entry list".into());
    }

    let mut urls = Vec::with_capacity(entries.0.len);
    for item in 0..entries.0.len {
        let entry = unsafe { &*entries.0.entries.add(item) };
        let read = |text: &memento_audio::audiofiles::AudioText| -> Option<String> {
            if text.len > isize::MAX as usize / std::mem::size_of::<u16>()
                || (text.len != 0 && text.data.is_null()) { return None }
            let units = if text.len == 0 { &[] } else { unsafe { std::slice::from_raw_parts(text.data, text.len) } };
            String::from_utf16(units).ok()
        };
        if let (Some(name), Some(url)) = (read(&entry.name), read(&entry.url)) {
            urls.push((name, url));
        }
    }
    Ok(urls)
}

struct ParsedAudioEntries(memento_audio::audiofiles::AudioEntries);

impl Drop for ParsedAudioEntries {
    fn drop(&mut self) {
        let entries = std::mem::replace(
            &mut self.0,
            memento_audio::audiofiles::AudioEntries {
                entries: std::ptr::null_mut(),
                len: 0,
                valid: false,
            },
        );
        unsafe { memento_audio::audiofiles::memento_audio_free_entries(entries) };
    }
}

enum SourceLocation {
    File(PathBuf),
    Http(String),
}

fn classify_url(value: &str) -> Result<SourceLocation, String> {
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
    fn create() -> Result<(Self, File), String> {
        let directory = std::env::temp_dir();
        for _ in 0..128 {
            let id = NEXT_TEMP_FILE_ID.fetch_add(1, Ordering::Relaxed);
            let path = directory.join(format!("memento-audio-{}-{id}.tmp", std::process::id()));
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
    use super::{classify_url, enumerate_word_audio_sources, expand_source_url, first_audio_candidate, parse_json_audio_entries, parse_json_audio_sources, resolve_word_audio, SourceLocation};
    use super::super::config::AudioSourceEntry;
    use memento_setting::enums::AudioSourceType;

    fn source(name: &str, url: &str, kind: AudioSourceType, md5: &str) -> AudioSourceEntry {
        AudioSourceEntry {
            name: name.to_owned(),
            url: url.to_owned(),
            kind: kind as i64,
            md5: md5.to_owned(),
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
    }

    #[test]
    fn anki_word_audio_uses_configured_source_and_skip_hash() {
        let sources = vec![source(
            "Japanese Audio", "https://audio.example.test/{expression}/{reading}.mp3",
            AudioSourceType::AudioSourceTypeFile, "deadbeef",
        )];
        let (url, filename, skip_hash) = resolve_word_audio("迎えに行く", "むかえにいく", &sources).unwrap();
        assert!(url.contains("迎えに行く"));
        assert!(url.contains("むかえにいく"));
        assert!(filename.starts_with("memento_Japanese_Audio_"));
        assert_eq!(skip_hash, "deadbeef");
    }

    #[test]
    fn word_audio_http_url_encodes_japanese_before_download() {
        let SourceLocation::Http(url) = classify_url("http://example.test/?expression=休み&reading=やすみ").unwrap() else {
            panic!("expected an HTTP URL");
        };
        assert!(url.is_ascii());
        assert!(url.contains("expression=%E4%BC%91%E3%81%BF"));
    }

    #[test]
    fn json_audio_entries_keep_order_and_reject_invalid_documents() {
        assert_eq!(
            parse_json_audio_entries(
                br#"{"type":"audioSourceList","audioSources":[{"name":"first","url":"https://example.test/1"},{"name":"second","url":"https://example.test/2"}]}"#
            ).unwrap(),
            vec![("first".to_owned(), "https://example.test/1".to_owned()),
                 ("second".to_owned(), "https://example.test/2".to_owned())]
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
    fn source_menu_expands_every_named_recording_from_json() {
        let path = std::env::temp_dir().join(format!(
            "memento-audio-menu-{}-{}.json", std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos(),
        ));
        std::fs::write(&path,
            br#"{"type":"audioSourceList","audioSources":[{"name":"speaker A","url":"https://example.test/a.mp3"},{"name":"speaker B","url":"https://example.test/b.mp3"}]}"#,
        ).unwrap();
        let sources = vec![
            source("dictionary audio", &path.to_string_lossy(), AudioSourceType::AudioSourceTypeJson, "skip"),
            source("fallback", "https://example.test/{expression}.mp3", AudioSourceType::AudioSourceTypeFile, ""),
        ];
        let files = enumerate_word_audio_sources("食べる", "たべる", &sources);
        let _ = std::fs::remove_file(path);
        assert_eq!(files.iter().map(|file| file.name.as_str()).collect::<Vec<_>>(),
            ["speaker A", "speaker B", "fallback"]);
        assert_eq!(files[1].url, "https://example.test/b.mp3");
        assert_eq!(files[1].md5, "skip");
        assert!(files[2].url.contains("食べる"));
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
                "https://source.test/ready/食べる" => {
                    Ok(vec!["https://audio.test/first.ogg".into()])
                }
                _ => Err(format!("unexpected source request {url:?}")),
            }
        })
        .unwrap();

        assert_eq!(
            candidate,
            ("https://audio.test/first.ogg".to_owned(), "third-hash".to_owned())
        );
        assert_eq!(
            requests,
            vec![
                "https://source.test/unavailable".to_owned(),
                "https://source.test/empty".to_owned(),
                "https://source.test/ready/食べる".to_owned(),
            ]
        );
    }
}

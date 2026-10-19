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
        let text = &entry.url;
        if text.len > isize::MAX as usize / std::mem::size_of::<u16>()
            || (text.len != 0 && text.data.is_null())
        {
            continue;
        }
        let units = if text.len == 0 {
            &[]
        } else {
            unsafe { std::slice::from_raw_parts(text.data, text.len) }
        };
        let url = String::from_utf16(units)
            .map_err(|error| format!("audio source JSON contains an invalid URL string: {error}"))?;
        urls.push(url);
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
    use super::{expand_source_url, first_audio_candidate, parse_json_audio_sources};
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
    fn json_audio_entries_keep_order_and_reject_invalid_documents() {
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

//! Asynchronous pronunciation audio worker thread, caching, and playback.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use memento_audio::audiofiles::md5_matches_skip_hash;
use memento_audio::audioplayer::{
    detect_audio_extension, validate_audio_bytes, validate_audio_content_type, AudioCache,
    AUDIO_CACHE_CAPACITY, MAX_AUDIO_PAYLOAD_BYTES,
};
use memento_net::http::HttpClient;
#[cfg(test)]
use memento_net::http::HttpClientConfig;
use memento_player::AudioInstance;
use tempfile::TempDir;

use super::super::config::AudioSourceEntry;
use super::sources::{
    classify_url, expand_audio_source_candidates, http_get, md5_hex, shared_audio_http_client,
    skip_hash_message, Md5Hasher, SourceLocation,
};

const MAGIC_SNIFF_MIN_BYTES: usize = 64;
const MAGIC_SNIFF_MAX_BYTES: usize = 512;

static NEXT_TEMP_FILE_ID: AtomicU64 = AtomicU64::new(0);

type Completion = Box<dyn FnOnce(Result<(), String>) + Send + 'static>;

struct AudioRequest {
    generation: u64,
    expression: String,
    reading: String,
    sources: Vec<AudioSourceEntry>,
    completion: Completion,
}

pub(super) trait PlaybackBackend {
    fn play_file(&mut self, path: &Path) -> Result<(), String>;
}

impl PlaybackBackend for AudioInstance {
    fn play_file(&mut self, path: &Path) -> Result<(), String> {
        AudioInstance::play_file(self, path)
    }
}

struct ChannelState {
    sender: Option<Sender<AudioRequest>>,
    init_error: Option<String>,
}

struct AudioControllerInner {
    channel: Arc<Mutex<ChannelState>>,
    latest_generation: Arc<AtomicU64>,
    shutdown: Arc<AtomicBool>,
    worker: Mutex<Option<JoinHandle<()>>>,
}

impl Drop for AudioControllerInner {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        self.latest_generation.fetch_add(1, Ordering::AcqRel);
        {
            let mut state = self
                .channel
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state.sender = None;
        }
        let handle = self
            .worker
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        if let Some(handle) = handle {
            if handle.thread().id() != thread::current().id() {
                let _ = handle.join();
            }
        }
    }
}

/// Asynchronous pronunciation player. Its libmpv instance is independent from
/// the movie player's instance, so loading audio cannot replace or pause video.
#[derive(Clone)]
pub struct AudioController {
    inner: Arc<AudioControllerInner>,
}

impl AudioController {
    /// Starts the dedicated audio worker thread immediately without blocking
    /// the caller. The audio-only mpv instance is initialized asynchronously
    /// on the worker before queued requests are drained.
    pub fn new() -> Result<Self, String> {
        Self::spawn_worker("memento-audio", AudioInstance::new, WorkerHttp::Shared)
    }

    #[cfg(test)]
    pub(super) fn with_backend<P>(backend: P, http_config: HttpClientConfig) -> Result<Self, String>
    where
        P: PlaybackBackend + Send + 'static,
    {
        Self::with_backend_factory(move || Ok(backend), http_config)
    }

    #[cfg(test)]
    pub(super) fn with_backend_factory<P, F>(
        factory: F,
        http_config: HttpClientConfig,
    ) -> Result<Self, String>
    where
        P: PlaybackBackend + 'static,
        F: FnOnce() -> Result<P, String> + Send + 'static,
    {
        Self::spawn_worker(
            "memento-audio-test",
            factory,
            WorkerHttp::Custom(HttpClient::new(http_config)),
        )
    }

    fn spawn_worker<P, F>(
        thread_name: &str,
        init_backend: F,
        http: WorkerHttp,
    ) -> Result<Self, String>
    where
        P: PlaybackBackend + 'static,
        F: FnOnce() -> Result<P, String> + Send + 'static,
    {
        let (sender, receiver) = mpsc::channel();
        let channel = Arc::new(Mutex::new(ChannelState {
            sender: Some(sender),
            init_error: None,
        }));
        let latest_generation = Arc::new(AtomicU64::new(0));
        let shutdown = Arc::new(AtomicBool::new(false));

        let worker_channel = Arc::clone(&channel);
        let worker_generation = Arc::clone(&latest_generation);
        let worker_shutdown = Arc::clone(&shutdown);

        let handle = thread::Builder::new()
            .name(thread_name.to_owned())
            .spawn(move || {
                if worker_shutdown.load(Ordering::Acquire) {
                    return;
                }
                match init_backend() {
                    Ok(player) => {
                        if worker_shutdown.load(Ordering::Acquire) {
                            drop(player);
                            return;
                        }
                        let mut worker = AudioWorker::with_parts(
                            player,
                            http,
                            worker_generation,
                            worker_shutdown,
                        );
                        worker.run(receiver);
                    }
                    Err(error) => {
                        {
                            let mut state = worker_channel
                                .lock()
                                .unwrap_or_else(|poisoned| poisoned.into_inner());
                            state.init_error = Some(error.clone());
                            state.sender = None;
                        }
                        if !worker_shutdown.load(Ordering::Acquire) {
                            while let Ok(request) = receiver.recv() {
                                (request.completion)(Err(error.clone()));
                            }
                        }
                    }
                }
            })
            .map_err(|error| format!("could not start audio worker: {error}"))?;

        Ok(Self {
            inner: Arc::new(AudioControllerInner {
                channel,
                latest_generation,
                shutdown,
                worker: Mutex::new(Some(handle)),
            }),
        })
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
        let state = self
            .inner
            .channel
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(sender) = state.sender.as_ref() else {
            return Err(state
                .init_error
                .clone()
                .unwrap_or_else(|| "audio worker is unavailable".to_owned()));
        };
        let generation = self.inner.latest_generation.fetch_add(1, Ordering::AcqRel) + 1;
        sender
            .send(AudioRequest {
                generation,
                expression: expression.into(),
                reading: reading.into(),
                sources,
                completion: Box::new(callback),
            })
            .map_err(|_| {
                state
                    .init_error
                    .clone()
                    .unwrap_or_else(|| "audio worker is unavailable".to_owned())
            })
    }
}

enum WorkerHttp {
    Shared,
    #[cfg(test)]
    Custom(HttpClient),
}

impl WorkerHttp {
    fn client(&self) -> &HttpClient {
        match self {
            Self::Shared => shared_audio_http_client(),
            #[cfg(test)]
            Self::Custom(client) => client,
        }
    }
}

enum DownloadedAudio {
    Skipped(String),
    File {
        file: TemporaryAudioFile,
        md5: String,
    },
}

// Field order is deliberate: mpv (`player`) is terminated before its cached or
// active temporary files and the owning `memento-audio-*` temp directory are
// removed when the worker exits.
pub(super) struct AudioWorker<P: PlaybackBackend = AudioInstance> {
    player: P,
    cache: AudioCache<Arc<TemporaryAudioFile>>,
    active_file: Option<Arc<TemporaryAudioFile>>,
    temp_dir: Option<TempDir>,
    http: WorkerHttp,
    latest_generation: Arc<AtomicU64>,
    shutdown: Arc<AtomicBool>,
}

impl<P: PlaybackBackend> AudioWorker<P> {
    fn with_parts(
        player: P,
        http: WorkerHttp,
        latest_generation: Arc<AtomicU64>,
        shutdown: Arc<AtomicBool>,
    ) -> Self {
        Self {
            player,
            cache: AudioCache::new(AUDIO_CACHE_CAPACITY),
            active_file: None,
            temp_dir: None,
            http,
            latest_generation,
            shutdown,
        }
    }

    #[cfg(test)]
    pub(super) fn with_backend(
        player: P,
        http: HttpClient,
        latest_generation: Arc<AtomicU64>,
    ) -> Self {
        Self::with_parts(
            player,
            WorkerHttp::Custom(http),
            latest_generation,
            Arc::new(AtomicBool::new(false)),
        )
    }

    fn is_superseded(&self, generation: u64) -> bool {
        self.shutdown.load(Ordering::Acquire)
            || self.latest_generation.load(Ordering::Acquire) != generation
    }

    fn run(&mut self, receiver: Receiver<AudioRequest>) {
        while let Ok(mut request) = receiver.recv() {
            if self.shutdown.load(Ordering::Acquire) {
                break;
            }
            // Coalesce bursts of rapid play clicks so only the latest request runs.
            while let Ok(newer) = receiver.try_recv() {
                if self.shutdown.load(Ordering::Acquire) {
                    return;
                }
                (request.completion)(Ok(()));
                request = newer;
            }
            if self.shutdown.load(Ordering::Acquire) {
                break;
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
            if self.shutdown.load(Ordering::Acquire) {
                break;
            }
            (request.completion)(result);
        }
    }

    pub(super) fn play_word(
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
            let candidates = match expand_audio_source_candidates(
                self.http.client(),
                source,
                expression,
                reading,
            ) {
                Ok(candidates) => candidates,
                Err(error) => {
                    last_error = Some(error);
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

        // 2. Download, validate, and stream-hash audio payload inline.
        let (file, md5) = match self.download_audio(url, skip_hash)? {
            DownloadedAudio::Skipped(md5) => {
                let _evicted = self.cache.insert(url.to_owned(), None, md5);
                return Err(skip_hash_message());
            }
            DownloadedAudio::File { file, md5 } => (file, md5),
        };

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

    fn download_audio(&mut self, url: &str, skip_hash: &str) -> Result<DownloadedAudio, String> {
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

                let mut hasher = Md5Hasher::new();
                let mut sniff = Vec::with_capacity(MAGIC_SNIFF_MAX_BYTES);
                let mut pending = Vec::new();
                let mut temporary: Option<TemporaryAudioFile> = None;
                let mut output: Option<File> = None;
                let mut buf = [0u8; 8192];
                let mut copied = 0u64;

                loop {
                    if self.shutdown.load(Ordering::Acquire) {
                        return Err("audio worker is shutting down".into());
                    }
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
                    copied = copied.saturating_add(read as u64);
                    if copied > MAX_AUDIO_PAYLOAD_BYTES {
                        return Err(format!(
                            "pronunciation audio exceeds maximum size ({MAX_AUDIO_PAYLOAD_BYTES} bytes)"
                        ));
                    }
                    hasher.update(chunk);
                    if sniff.len() < MAGIC_SNIFF_MAX_BYTES {
                        let take = (MAGIC_SNIFF_MAX_BYTES - sniff.len()).min(chunk.len());
                        sniff.extend_from_slice(&chunk[..take]);
                    }
                    if let Some(file) = output.as_mut() {
                        file.write_all(chunk).map_err(|error| {
                            format!(
                                "could not copy pronunciation audio file {}: {error}",
                                path.display()
                            )
                        })?;
                    } else {
                        pending.extend_from_slice(chunk);
                        if sniff.len() >= MAGIC_SNIFF_MIN_BYTES {
                            let ext = detect_audio_extension(&sniff, None, url);
                            let dir = ensure_temp_dir(&mut self.temp_dir)?;
                            let (temp, mut file) = TemporaryAudioFile::create_in(dir, ext)?;
                            file.write_all(&pending).map_err(|error| {
                                format!(
                                    "could not copy pronunciation audio file {}: {error}",
                                    path.display()
                                )
                            })?;
                            pending.clear();
                            temporary = Some(temp);
                            output = Some(file);
                        }
                    }
                }

                if copied == 0 {
                    return Err("pronunciation audio source returned an empty file".into());
                }
                let hex = md5_hex(hasher.finalize());
                if md5_matches_skip_hash(&hex, skip_hash) {
                    return Ok(DownloadedAudio::Skipped(hex));
                }
                validate_audio_bytes(&sniff)?;
                let temporary = finish_temp_audio_file(
                    &mut self.temp_dir,
                    temporary,
                    output,
                    &pending,
                    &sniff,
                    None,
                    url,
                )?;
                Ok(DownloadedAudio::File {
                    file: temporary,
                    md5: hex,
                })
            }
            SourceLocation::Http(url) => {
                let mut response = http_get(self.http.client(), &url, "pronunciation audio")?;
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

                // Buffer the initial sniff window before creating a temporary file so
                // magic bytes (including OpusHead at offset 28..36) determine the exact
                // extension and empty/skipped small responses never churn `/tmp`.
                let mut hasher = Md5Hasher::new();
                let mut sniff = Vec::with_capacity(MAGIC_SNIFF_MAX_BYTES);
                let mut pending = Vec::new();
                let mut temporary: Option<TemporaryAudioFile> = None;
                let mut output: Option<File> = None;
                let mut written = 0u64;
                let temp_dir = &mut self.temp_dir;
                let shutdown = &self.shutdown;

                let stream_res = response.stream_chunks(&mut |chunk| {
                    if shutdown.load(Ordering::Acquire) {
                        return Err(io::Error::new(
                            io::ErrorKind::Interrupted,
                            "audio worker is shutting down",
                        ));
                    }
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
                    hasher.update(chunk);
                    if sniff.len() < MAGIC_SNIFF_MAX_BYTES {
                        let take = (MAGIC_SNIFF_MAX_BYTES - sniff.len()).min(chunk.len());
                        sniff.extend_from_slice(&chunk[..take]);
                    }
                    if let Some(file) = output.as_mut() {
                        file.write_all(chunk)
                    } else {
                        pending.extend_from_slice(chunk);
                        if sniff.len() >= MAGIC_SNIFF_MIN_BYTES {
                            let ext =
                                detect_audio_extension(&sniff, content_type.as_deref(), &url);
                            let dir = ensure_temp_dir(temp_dir).map_err(io::Error::other)?;
                            let (temp, mut file) = TemporaryAudioFile::create_in(dir, ext)
                                .map_err(io::Error::other)?;
                            file.write_all(&pending)?;
                            pending.clear();
                            temporary = Some(temp);
                            output = Some(file);
                        }
                        Ok(())
                    }
                });
                stream_res
                    .map_err(|error| format!("could not download pronunciation audio: {error}"))?;

                if written == 0 {
                    return Err("pronunciation audio source returned an empty file".into());
                }
                let hex = md5_hex(hasher.finalize());
                if md5_matches_skip_hash(&hex, skip_hash) {
                    return Ok(DownloadedAudio::Skipped(hex));
                }
                validate_audio_bytes(&sniff)?;
                let temporary = finish_temp_audio_file(
                    &mut self.temp_dir,
                    temporary,
                    output,
                    &pending,
                    &sniff,
                    content_type.as_deref(),
                    &url,
                )?;
                Ok(DownloadedAudio::File {
                    file: temporary,
                    md5: hex,
                })
            }
        }
    }
}

fn ensure_temp_dir(slot: &mut Option<TempDir>) -> Result<&Path, String> {
    if slot.as_ref().is_none_or(|dir| !dir.path().is_dir()) {
        let dir = tempfile::Builder::new()
            .prefix("memento-audio-")
            .tempdir()
            .map_err(|error| format!("could not create temporary audio directory: {error}"))?;
        *slot = Some(dir);
    }
    slot.as_ref()
        .map(TempDir::path)
        .ok_or_else(|| "could not access temporary audio directory".to_owned())
}

fn finish_temp_audio_file(
    temp_dir: &mut Option<TempDir>,
    temporary: Option<TemporaryAudioFile>,
    output: Option<File>,
    pending: &[u8],
    sniff: &[u8],
    content_type: Option<&str>,
    url: &str,
) -> Result<TemporaryAudioFile, String> {
    if let (Some(temp), Some(mut file)) = (temporary, output) {
        let flush_res = file.flush();
        drop(file);
        flush_res.map_err(|error| format!("could not finish pronunciation audio file: {error}"))?;
        return Ok(temp);
    }
    if pending.is_empty() {
        return Err("pronunciation audio source returned an empty file".into());
    }
    let ext = detect_audio_extension(sniff, content_type, url);
    let dir = ensure_temp_dir(temp_dir)?;
    let (temp, mut file) = TemporaryAudioFile::create_in(dir, ext)?;
    let write_res = file.write_all(pending).and_then(|()| file.flush());
    drop(file);
    write_res.map_err(|error| format!("could not finish pronunciation audio file: {error}"))?;
    Ok(temp)
}

struct TemporaryAudioFile {
    path: PathBuf,
}

impl TemporaryAudioFile {
    fn create_in(directory: &Path, extension: &str) -> Result<(Self, File), String> {
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
            let path = directory.join(format!(
                "memento-audio-{}-{id}.{suffix}",
                std::process::id()
            ));
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

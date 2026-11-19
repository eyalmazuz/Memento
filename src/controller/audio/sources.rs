//! Pronunciation audio source URL classification, template expansion,
//! JSON `audioSourceList` parsing, shared HTTP lookups, and incremental MD5.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use memento_audio::audiofiles::{
    expand_url_template, expand_url_template_encoded, md5_matches_skip_hash,
    parse_audio_source_list,
};
use memento_audio::audioplayer::{
    detect_audio_extension, validate_audio_bytes, validate_audio_content_type,
    MAX_AUDIO_PAYLOAD_BYTES,
};
use memento_net::http::{HttpClient, HttpClientConfig, Request, Response, Url as HttpUrl};
use memento_setting::enums::AudioSourceType;
use url::Url;

use super::super::config::AudioSourceEntry;

const NETWORK_TIMEOUT: Duration = Duration::from_secs(30);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_JSON_SOURCE_BYTES: u64 = 8 * 1024 * 1024;
const AUDIO_USER_AGENT: &str = "Memento";

static SHARED_HTTP_CLIENT: OnceLock<HttpClient> = OnceLock::new();

pub(super) fn shared_audio_http_client() -> &'static HttpClient {
    SHARED_HTTP_CLIENT.get_or_init(|| {
        HttpClient::new(HttpClientConfig {
            timeout: Some(NETWORK_TIMEOUT),
            connect_timeout: Some(CONNECT_TIMEOUT),
            ..HttpClientConfig::default()
        })
    })
}

/// Incremental RFC 1321 MD5 hasher for streaming audio downloads without
/// reopening temporary files from disk.
pub(super) struct Md5Hasher {
    state: [u32; 4],
    buffer: [u8; 64],
    buffer_len: usize,
    total_len: u64,
}

impl Md5Hasher {
    pub(super) fn new() -> Self {
        Self {
            state: [0x6745_2301, 0xefcd_ab89, 0x98ba_dcfe, 0x1032_5476],
            buffer: [0u8; 64],
            buffer_len: 0,
            total_len: 0,
        }
    }

    pub(super) fn digest(data: &[u8]) -> [u8; 16] {
        let mut hasher = Self::new();
        hasher.update(data);
        hasher.finalize()
    }

    pub(super) fn update(&mut self, mut data: &[u8]) {
        self.total_len = self.total_len.wrapping_add(data.len() as u64);
        if self.buffer_len > 0 {
            let take = (64 - self.buffer_len).min(data.len());
            self.buffer[self.buffer_len..self.buffer_len + take].copy_from_slice(&data[..take]);
            self.buffer_len += take;
            data = &data[take..];
            if self.buffer_len == 64 {
                let block = self.buffer;
                self.compress(&block);
                self.buffer_len = 0;
            }
        }
        while data.len() >= 64 {
            let mut block = [0u8; 64];
            block.copy_from_slice(&data[..64]);
            self.compress(&block);
            data = &data[64..];
        }
        if !data.is_empty() {
            self.buffer[..data.len()].copy_from_slice(data);
            self.buffer_len = data.len();
        }
    }

    pub(super) fn finalize(mut self) -> [u8; 16] {
        let bit_len = self.total_len.wrapping_mul(8);
        self.buffer[self.buffer_len] = 0x80;
        self.buffer_len += 1;
        if self.buffer_len > 56 {
            self.buffer[self.buffer_len..64].fill(0);
            let block = self.buffer;
            self.compress(&block);
            self.buffer_len = 0;
        }
        self.buffer[self.buffer_len..56].fill(0);
        self.buffer[56..64].copy_from_slice(&bit_len.to_le_bytes());
        let block = self.buffer;
        self.compress(&block);

        let mut out = [0u8; 16];
        for (idx, word) in self.state.iter().enumerate() {
            out[idx * 4..idx * 4 + 4].copy_from_slice(&word.to_le_bytes());
        }
        out
    }

    fn compress(&mut self, block: &[u8; 64]) {
        const S: [u32; 64] = [
            7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20,
            5, 9, 14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23,
            6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
        ];
        const K: [u32; 64] = [
            0xd76aa478, 0xe8c7b756, 0x242070db, 0xc1bdceee, 0xf57c0faf, 0x4787c62a, 0xa8304613,
            0xfd469501, 0x698098d8, 0x8b44f7af, 0xffff5bb1, 0x895cd7be, 0x6b901122, 0xfd987193,
            0xa679438e, 0x49b40821, 0xf61e2562, 0xc040b340, 0x265e5a51, 0xe9b6c7aa, 0xd62f105d,
            0x02441453, 0xd8a1e681, 0xe7d3fbc8, 0x21e1cde6, 0xc33707d6, 0xf4d50d87, 0x455a14ed,
            0xa9e3e905, 0xfcefa3f8, 0x676f02d9, 0x8d2a4c8a, 0xfffa3942, 0x8771f681, 0x6d9d6122,
            0xfde5380c, 0xa4beea44, 0x4bdecfa9, 0xf6bb4b60, 0xbebfbc70, 0x289b7ec6, 0xeaa127fa,
            0xd4ef3085, 0x04881d05, 0xd9d4d039, 0xe6db99e5, 0x1fa27cf8, 0xc4ac5665, 0xf4292244,
            0x432aff97, 0xab9423a7, 0xfc93a039, 0x655b59c3, 0x8f0ccc92, 0xffeff47d, 0x85845dd1,
            0x6fa87e4f, 0xfe2ce6e0, 0xa3014314, 0x4e0811a1, 0xf7537e82, 0xbd3af235, 0x2ad7d2bb,
            0xeb86d391,
        ];

        let mut m = [0u32; 16];
        for (i, chunk) in block.chunks_exact(4).enumerate() {
            m[i] = u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
        }

        let [mut a, mut b, mut c, mut d] = self.state;
        for i in 0..64 {
            let (f, g) = match i {
                0..=15 => ((b & c) | ((!b) & d), i),
                16..=31 => ((d & b) | ((!d) & c), (5 * i + 1) % 16),
                32..=47 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | (!d)), (7 * i) % 16),
            };
            let temp = d;
            d = c;
            c = b;
            let sum = a.wrapping_add(f).wrapping_add(K[i]).wrapping_add(m[g]);
            b = b.wrapping_add(sum.rotate_left(S[i]));
            a = temp;
        }

        self.state[0] = self.state[0].wrapping_add(a);
        self.state[1] = self.state[1].wrapping_add(b);
        self.state[2] = self.state[2].wrapping_add(c);
        self.state[3] = self.state[3].wrapping_add(d);
    }
}

pub(super) fn md5_hex(digest: [u8; 16]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(32);
    for byte in digest {
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 0x0f) as usize] as char);
    }
    encoded
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
            md5_hex(Md5Hasher::digest(key.as_bytes()))
        }
    };

    let clean_ext = extension.trim_start_matches('.');
    let ext = if clean_ext.is_empty() {
        "mp3"
    } else {
        clean_ext
    };

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

pub(super) fn http_get(
    http: &HttpClient,
    url: &str,
    description: &str,
) -> Result<Response, String> {
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

fn read_json_source_body(
    http: &HttpClient,
    source_url: &str,
) -> Result<(Vec<u8>, Option<Url>), String> {
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

pub(super) fn expand_audio_source_candidates(
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
    let http = shared_audio_http_client();
    let mut last_error: Option<String> = None;
    for source in sources {
        let candidates = match expand_audio_source_candidates(http, source, expression, reading) {
            Ok(candidates) => candidates,
            Err(error) => {
                last_error = Some(error);
                continue;
            }
        };
        if let Some((candidate_name, resolved_url)) = candidates.into_iter().next() {
            let ext = detect_audio_extension(&[], None, &resolved_url);
            let filename =
                ascii_safe_audio_filename_with_ext(&candidate_name, reading, expression, None, ext);
            return Ok((resolved_url, filename, source.md5.clone()));
        }
    }
    Err(last_error.unwrap_or_else(|| "configured audio sources returned no audio files".into()))
}

/// Expand every configured source into the playable files shown in a term's
/// audio menu. A JSON source can contribute more than one named recording.
pub fn enumerate_word_audio_sources(
    expression: &str,
    reading: &str,
    sources: &[AudioSourceEntry],
) -> Vec<AudioSourceEntry> {
    let http = shared_audio_http_client();
    let mut available = Vec::new();
    for source in sources {
        let Ok(candidates) = expand_audio_source_candidates(http, source, expression, reading)
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
    fetch_word_audio_with_client(shared_audio_http_client(), expression, reading, sources)
}

pub(super) fn fetch_word_audio_with_client(
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
                last_error = Some(error);
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
            let hex = md5_hex(Md5Hasher::digest(&bytes));
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

#[cfg(test)]
pub(super) fn first_audio_candidate(
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

pub(super) fn skip_hash_message() -> String {
    "pronunciation audio was skipped because its MD5 matches the configured skip hash".into()
}

pub(super) fn expand_source_url(
    template: &str,
    expression: &str,
    reading: &str,
) -> Result<String, String> {
    Ok(expand_url_template(template, expression, reading))
}

#[cfg(test)]
pub(super) fn parse_json_audio_sources(data: &[u8]) -> Result<Vec<String>, String> {
    parse_json_audio_entries(data).map(|entries| entries.into_iter().map(|(_, url)| url).collect())
}

pub(super) fn parse_json_audio_entries(data: &[u8]) -> Result<Vec<(String, String)>, String> {
    parse_audio_source_list(data).ok_or_else(|| {
        "audio source response is not a valid audioSourceList JSON document".to_owned()
    })
}

#[derive(Debug)]
pub(super) enum SourceLocation {
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

fn has_file_scheme(value: &str) -> bool {
    value
        .get(..5)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("file:"))
}

fn is_windows_drive_path(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() >= 2
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes.len() == 2 || matches!(bytes[2], b'/' | b'\\'))
}

fn classify_local_path(path: PathBuf) -> Result<SourceLocation, String> {
    if path.is_file() {
        return Ok(SourceLocation::File(path));
    }
    if path.exists() {
        return Err(format!("audio path is not a file: {}", path.display()));
    }
    Err(format!("audio file not found: {}", path.display()))
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

pub(super) fn classify_url(value: &str) -> Result<SourceLocation, String> {
    let value = value.trim();
    if value.is_empty() {
        return Err("audio URL is empty".into());
    }
    if has_http_scheme(value) {
        let parsed =
            Url::parse(value).map_err(|error| format!("invalid audio URL {value:?}: {error}"))?;
        return Ok(SourceLocation::Http(parsed.to_string()));
    }
    if has_file_scheme(value) {
        let parsed = Url::parse(value)
            .map_err(|error| format!("invalid audio file URL {value:?}: {error}"))?;
        let path = parsed
            .to_file_path()
            .map_err(|_| format!("audio file URL does not identify a local file: {value:?}"))?;
        return classify_local_path(path);
    }
    if is_windows_drive_path(value) {
        return classify_local_path(PathBuf::from(value));
    }

    let path = Path::new(value);
    if path.is_file() {
        return Ok(SourceLocation::File(path.to_owned()));
    }

    if let Ok(parsed) = Url::parse(value) {
        return match parsed.scheme() {
            "file" => {
                let path = parsed.to_file_path().map_err(|_| {
                    format!("audio file URL does not identify a local file: {value:?}")
                })?;
                classify_local_path(path)
            }
            "http" | "https" => Ok(SourceLocation::Http(parsed.to_string())),
            scheme => Err(format!("unsupported audio URL scheme {scheme:?}")),
        };
    }

    classify_local_path(path.to_owned())
}

//! Pronunciation audio source resolution and playback on an independent mpv instance.
//!
//! Network access, file copying, inline stream hashing, and mpv playback run on
//! one worker thread. The completion callback also runs on that worker after
//! mpv reports that playback has restarted, leaving UI-thread dispatch to the
//! caller.

mod sources;
mod worker;

pub(crate) use sources::sanitize_anki_filename;
pub use sources::{
    ascii_safe_audio_filename, clear_shared_audio_cache, download_timeout_config,
    download_timeout_ms, enumerate_word_audio_sources, fetch_word_audio, resolve_word_audio,
    set_download_timeout_ms,
};
pub use worker::AudioController;

#[cfg(test)]
mod tests {
    use super::super::config::AudioSourceEntry;
    use super::sources::{
        classify_url, clear_shared_audio_cache, download_timeout_config, download_timeout_ms,
        enumerate_word_audio_sources, expand_source_url, fetch_word_audio,
        fetch_word_audio_with_client, fetch_word_audio_with_client_and_timeout,
        first_audio_candidate, md5_hex, parse_json_audio_entries, parse_json_audio_sources,
        resolve_word_audio, set_download_timeout_ms, Md5Hasher, SourceLocation,
    };
    use super::worker::{AudioController, AudioWorker, PlaybackBackend};
    use memento_net::http::{HttpClient, HttpClientConfig};
    use memento_setting::enums::AudioSourceType;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
    use std::sync::{mpsc, Arc, Mutex};
    use std::time::{Duration, Instant};

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
            let bytes = std::fs::read(path).map_err(|e| {
                format!(
                    "mock player could not read temp file {}: {e}",
                    path.display()
                )
            })?;
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
    fn incremental_md5_matches_rfc1321_and_fileutils_across_chunk_boundaries() {
        for (input, expected) in [
            (b"".as_slice(), "d41d8cd98f00b204e9800998ecf8427e"),
            (b"a", "0cc175b9c0f1b6a831c399e269772661"),
            (b"abc", "900150983cd24fb0d6963f7d28e17f72"),
            (b"message digest", "f96b697d7cb7938d525a2f31aaf161d0"),
            (
                b"abcdefghijklmnopqrstuvwxyz",
                "c3fcd3d76192e4007dfb496cca67e13b",
            ),
        ] {
            assert_eq!(md5_hex(Md5Hasher::digest(input)), expected);
        }

        // Cross-check boundary lengths (55, 56, 63, 64, 65, 128, 1000) and chunked updates.
        for len in [0usize, 1, 55, 56, 57, 63, 64, 65, 127, 128, 129, 1025] {
            let payload: Vec<u8> = (0..len).map(|i| ((i * 31 + 7) & 0xff) as u8).collect();
            let reference = memento_util::fileutils::calculate_md5(payload.as_slice()).unwrap();
            assert_eq!(Md5Hasher::digest(&payload), reference, "len={len}");

            let mut chunked = Md5Hasher::new();
            for chunk in payload.chunks(7) {
                chunked.update(chunk);
            }
            assert_eq!(chunked.finalize(), reference, "chunked len={len}");
        }
    }

    #[test]
    fn classify_url_reports_accurate_missing_file_and_directory_errors() {
        let missing = std::env::temp_dir().join(format!(
            "memento-audio-definitely-missing-{}.mp3",
            std::process::id()
        ));
        let err = classify_url(&missing.to_string_lossy()).unwrap_err();
        assert!(
            err.contains("not found"),
            "expected missing-file error, got: {err}"
        );

        let dir_err = classify_url(&std::env::temp_dir().to_string_lossy()).unwrap_err();
        assert!(
            dir_err.contains("not a file"),
            "expected directory error, got: {dir_err}"
        );

        let scheme_err = classify_url("ftp://example.test/word.mp3").unwrap_err();
        assert!(
            scheme_err.contains("unsupported audio URL scheme"),
            "expected unsupported scheme error, got: {scheme_err}"
        );
    }

    #[test]
    fn local_file_download_sniffs_audio_magic_bytes_for_extension() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "memento-magic-local-{}-{now}.bin",
            std::process::id()
        ));
        let mut opus = vec![0u8; 80];
        opus[..4].copy_from_slice(b"OggS");
        opus[28..36].copy_from_slice(b"OpusHead");
        std::fs::write(&path, &opus).unwrap();

        let mock = MockPlayback::new();
        let played = Arc::clone(&mock.played);
        let generation = Arc::new(AtomicU64::new(1));
        let mut worker = AudioWorker::with_backend(mock, fast_http_client(), generation);
        let sources = vec![source(
            "LocalMagic",
            &path.to_string_lossy(),
            AudioSourceType::AudioSourceTypeFile,
            "",
        )];
        worker.play_word(1, "猫", "ねこ", &sources).unwrap();
        let _ = std::fs::remove_file(&path);

        let temp_path = played.lock().unwrap()[0].0.clone();
        assert_eq!(
            temp_path.extension().and_then(|ext| ext.to_str()),
            Some("opus"),
            "local file branch must sniff magic bytes: {}",
            temp_path.display()
        );
    }

    #[test]
    fn last_error_precedence_is_consistent_across_json_and_file_failures() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let missing_audio = std::env::temp_dir().join(format!(
            "memento-last-err-audio-{}-{now}.mp3",
            std::process::id()
        ));
        let missing_json = std::env::temp_dir().join(format!(
            "memento-last-err-json-{}-{now}.json",
            std::process::id()
        ));
        let sources = vec![
            source(
                "MissingFile",
                &missing_audio.to_string_lossy(),
                AudioSourceType::AudioSourceTypeFile,
                "",
            ),
            source(
                "MissingJson",
                &missing_json.to_string_lossy(),
                AudioSourceType::AudioSourceTypeJson,
                "",
            ),
        ];

        let err =
            fetch_word_audio_with_client(&fast_http_client(), "猫", "ねこ", &sources).unwrap_err();
        assert!(
            err.contains(&missing_json.to_string_lossy().to_string()),
            "later JSON expansion failure must update last_error consistently: {err}"
        );
    }

    #[test]
    fn controller_initializes_backend_asynchronously_and_joins_cleanly_on_drop() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let audio_path = std::env::temp_dir().join(format!(
            "memento-async-init-{}-{now}.mp3",
            std::process::id()
        ));
        std::fs::write(&audio_path, b"ID3async-init-payload").unwrap();

        let mock = MockPlayback::new();
        let played = Arc::clone(&mock.played);
        let init_started = Arc::new(AtomicBool::new(false));
        let init_started_worker = Arc::clone(&init_started);

        let started = Instant::now();
        let controller = AudioController::with_backend_factory(
            move || {
                init_started_worker.store(true, Ordering::Release);
                std::thread::sleep(Duration::from_millis(80));
                Ok(mock)
            },
            fast_http_config(),
        )
        .unwrap();
        assert!(
            started.elapsed() < Duration::from_millis(60),
            "AudioController creation must not block waiting on backend initialization"
        );

        let (tx, rx) = mpsc::channel();
        controller
            .play_word(
                "猫",
                "ねこ",
                vec![source(
                    "Local",
                    &audio_path.to_string_lossy(),
                    AudioSourceType::AudioSourceTypeFile,
                    "",
                )],
                move |res| {
                    let _ = tx.send(res);
                },
            )
            .unwrap();
        rx.recv_timeout(Duration::from_secs(3)).unwrap().unwrap();
        assert!(init_started.load(Ordering::Acquire));

        let temp_file = played.lock().unwrap()[0].0.clone();
        let temp_dir = temp_file.parent().unwrap().to_path_buf();
        assert!(temp_file.is_file());
        assert!(temp_dir.is_dir());

        drop(controller);
        assert!(
            !temp_file.exists(),
            "Drop must join worker and unlink temp audio file"
        );
        assert!(
            !temp_dir.exists(),
            "Drop must clean up memento-audio-* temporary directory"
        );
        let _ = std::fs::remove_file(&audio_path);
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
        assert_eq!(
            files[2].url,
            "https://example.test/%E9%A3%9F%E3%81%B9%E3%82%8B.mp3"
        );
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
        let done = Arc::new(AtomicBool::new(false));
        let server_done = Arc::clone(&done);
        let handle = std::thread::spawn(move || {
            while let Ok((mut stream, _)) = listener.accept() {
                if server_done.load(Ordering::Acquire) {
                    break;
                }
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

        done.store(true, Ordering::Release);
        let _ = std::net::TcpStream::connect(addr);
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
            source(
                "spaces",
                "   \t  ",
                AudioSourceType::AudioSourceTypeFile,
                "h2",
            ),
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
    fn fetch_word_audio_iterates_sources_skips_md5_case_insensitively_and_produces_ascii_filename()
    {
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
            source(
                "HTML 200",
                &format!("{base}/html?w={{expression}}"),
                AudioSourceType::AudioSourceTypeFile,
                "",
            ),
            source(
                "Disguised HTML",
                &format!("{base}/disguised-html"),
                AudioSourceType::AudioSourceTypeFile,
                "",
            ),
            source(
                "Empty 200",
                &format!("{base}/empty"),
                AudioSourceType::AudioSourceTypeFile,
                "",
            ),
            source(
                "Slow Timeout",
                &format!("{base}/stall"),
                AudioSourceType::AudioSourceTypeFile,
                "",
            ),
            source(
                "Valid OGG",
                &format!("{base}/good?q={{expression}}&r={{reading}}"),
                AudioSourceType::AudioSourceTypeFile,
                "",
            ),
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
        let generation = Arc::new(AtomicU64::new(1));
        let mut worker =
            AudioWorker::with_backend(mock, fast_http_client(), Arc::clone(&generation));

        // 1. Falls through skip-hash, HTML 200, and corrupt playback failure to `/good`.
        let sources = vec![
            source(
                "Skip",
                &format!("{base}/skip"),
                AudioSourceType::AudioSourceTypeFile,
                &format!("  {placeholder_md5}  "),
            ),
            source(
                "HTML",
                &format!("{base}/html"),
                AudioSourceType::AudioSourceTypeFile,
                "",
            ),
            source(
                "Corrupt",
                &format!("{base}/corrupt"),
                AudioSourceType::AudioSourceTypeFile,
                "",
            ),
            source(
                "Good",
                &format!("{base}/good"),
                AudioSourceType::AudioSourceTypeFile,
                "",
            ),
        ];
        worker.play_word(1, "食べる", "たべる", &sources).unwrap();
        assert_eq!(good_hits.load(Ordering::Relaxed), 1);
        assert_eq!(skip_hits.load(Ordering::Relaxed), 1);

        let first_temp_path = played.lock().unwrap()[0].0.clone();
        assert!(
            first_temp_path.is_file(),
            "active temp file must remain alive after play"
        );
        assert!(
            first_temp_path.extension().and_then(|e| e.to_str()) == Some("mp3"),
            "temp file should carry detected audio extension: {}",
            first_temp_path.display()
        );

        // 2. Second play of `/good` hits cache (no extra HTTP hit), and `/skip` hits skip-hash cache!
        worker
            .play_word(1, "食べる", "たべる", &sources[..1])
            .unwrap_err();
        assert_eq!(
            skip_hits.load(Ordering::Relaxed),
            1,
            "cached skip-hash URL must not re-download"
        );
        worker
            .play_word(1, "食べる", "たべる", &sources[3..])
            .unwrap();
        assert_eq!(
            good_hits.load(Ordering::Relaxed),
            1,
            "cached audio must not re-download"
        );

        // 3. Corrupt file was NOT cached (retrying `/corrupt` still fails cleanly without leaking temp file).
        assert!(worker.play_word(1, "x", "x", &sources[2..3]).is_err());

        // 4. External deletion of cached temp file self-heals via re-download!
        std::fs::remove_file(&first_temp_path).unwrap();
        worker
            .play_word(1, "食べる", "たべる", &sources[3..])
            .unwrap();
        assert_eq!(
            good_hits.load(Ordering::Relaxed),
            2,
            "missing cached file must re-download"
        );
        let healed_temp_path = played.lock().unwrap().last().unwrap().0.clone();
        assert!(healed_temp_path.is_file());

        // 5. Clearing skip_hash for previously skipped URL re-downloads and plays cleanly!
        let unskipped_src = vec![source(
            "Unskipped",
            &format!("{base}/skip"),
            AudioSourceType::AudioSourceTypeFile,
            "",
        )];
        worker
            .play_word(1, "食べる", "たべる", &unskipped_src)
            .unwrap();
        assert_eq!(
            skip_hits.load(Ordering::Relaxed),
            2,
            "clearing skip_hash must re-download the previously skip-cached URL"
        );
        let unskipped_temp_path = played.lock().unwrap().last().unwrap().0.clone();
        assert!(unskipped_temp_path.is_file());

        // 6. Playing `/second` succeeds, and dropping worker unlinks ALL temp files on disk!
        let second_src = vec![source(
            "Second",
            &format!("{base}/second"),
            AudioSourceType::AudioSourceTypeFile,
            "",
        )];
        worker.play_word(1, "猫", "ねこ", &second_src).unwrap();
        let second_temp_path = played.lock().unwrap().last().unwrap().0.clone();
        assert!(second_temp_path.is_file());
        assert_eq!(
            second_temp_path.extension().and_then(|e| e.to_str()),
            Some("ogg")
        );

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

    #[test]
    fn download_timeout_uses_bounded_config_and_enforces_per_request_timeout() {
        // Default is 8_000 ms; bounds clamp strictly to 1_000..=30_000 ms.
        set_download_timeout_ms(memento_audio::DEFAULT_AUDIO_DOWNLOAD_TIMEOUT_MS);
        assert_eq!(download_timeout_ms(), 8_000);
        assert_eq!(download_timeout_config().timeout(), Duration::from_secs(8));

        set_download_timeout_ms(250);
        assert_eq!(download_timeout_ms(), 1_000);
        set_download_timeout_ms(12_500);
        assert_eq!(download_timeout_ms(), 12_500);
        set_download_timeout_ms(120_000);
        assert_eq!(download_timeout_ms(), 30_000);
        // Restore default so other tests see the 8 s default.
        set_download_timeout_ms(memento_audio::DEFAULT_AUDIO_DOWNLOAD_TIMEOUT_MS);

        // Verify `send_with_request_timeout` overrides a long-timeout client cleanly.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut req = [0u8; 1024];
                let _ = stream.read(&mut req);
                std::thread::sleep(Duration::from_millis(600));
                let _ = stream.write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 7\r\nContent-Type: audio/mpeg\r\n\r\nID3slow",
                );
            }
        });
        let slow_client = HttpClient::new(HttpClientConfig {
            timeout: Some(Duration::from_secs(30)),
            connect_timeout: Some(Duration::from_secs(5)),
            ..HttpClientConfig::default()
        });
        let sources = vec![source(
            "SlowServer",
            &format!("http://{addr}/slow.mp3"),
            AudioSourceType::AudioSourceTypeFile,
            "",
        )];
        let started = Instant::now();
        let err = fetch_word_audio_with_client_and_timeout(
            &slow_client,
            "猫",
            "ねこ",
            &sources,
            Some(Duration::from_millis(200)),
        )
        .unwrap_err();
        let elapsed = started.elapsed();
        assert!(
            elapsed < Duration::from_secs(2),
            "per-request timeout override must abort quickly, took {elapsed:?} (err: {err})"
        );
        let _ = server.join();
    }

    #[test]
    fn worker_and_controller_clear_cache_force_fresh_download() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let hits = Arc::new(AtomicUsize::new(0));
        let server_hits = Arc::clone(&hits);
        let server = std::thread::spawn(move || {
            for seq in 1..=3 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut buf = [0u8; 1024];
                let _ = stream.read(&mut buf);
                server_hits.fetch_add(1, Ordering::AcqRel);
                let body = format!("ID3payload-v{seq}");
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: audio/mpeg\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream.write_all(resp.as_bytes()).unwrap();
            }
        });

        let url = format!("http://{addr}/word.mp3");
        let sources = vec![source(
            "LocalHttp",
            &url,
            AudioSourceType::AudioSourceTypeFile,
            "",
        )];

        // 1. Direct `AudioWorker::clear_cache()` clears the LRU cache cleanly.
        let mock = MockPlayback::new();
        let played = Arc::clone(&mock.played);
        let generation = Arc::new(AtomicU64::new(1));
        let mut worker = AudioWorker::with_backend(mock, fast_http_client(), generation);
        worker.play_word(1, "猫", "ねこ", &sources).unwrap();
        assert_eq!(worker.cache_len(), 1);
        assert_eq!(hits.load(Ordering::Acquire), 1);

        // Cache hit: no second HTTP request.
        worker.play_word(1, "猫", "ねこ", &sources).unwrap();
        assert_eq!(hits.load(Ordering::Acquire), 1);

        // Clear cache -> next playback fetches v2 from server.
        worker.clear_cache();
        assert_eq!(worker.cache_len(), 0);
        worker.play_word(1, "猫", "ねこ", &sources).unwrap();
        assert_eq!(hits.load(Ordering::Acquire), 2);
        assert_eq!(worker.cache_len(), 1);
        {
            let log = played.lock().unwrap();
            assert_eq!(log[0].1.as_slice(), b"ID3payload-v1");
            assert_eq!(log[2].1.as_slice(), b"ID3payload-v2");
        }
        drop(worker);

        // 2. `AudioController::clear_cache()` dispatches cleanly to the worker thread.
        let ctrl_mock = MockPlayback::new();
        let ctrl_played = Arc::clone(&ctrl_mock.played);
        let controller = AudioController::with_backend(ctrl_mock, fast_http_config()).unwrap();
        let (tx, rx) = mpsc::channel();
        controller
            .play_word("猫", "ねこ", sources.clone(), move |r| {
                let _ = tx.send(r);
            })
            .unwrap();
        rx.recv_timeout(Duration::from_secs(3)).unwrap().unwrap();
        assert_eq!(hits.load(Ordering::Acquire), 3);
        controller.clear_cache();
        // Calling clear_cache again is safe and never hangs on drop.
        drop(controller);
        assert_eq!(
            ctrl_played.lock().unwrap()[0].1.as_slice(),
            b"ID3payload-v3"
        );
        server.join().unwrap();
    }

    #[test]
    fn shared_audio_cache_reuses_downloaded_audio_and_json_and_clears() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let json_hits = Arc::new(AtomicUsize::new(0));
        let mp3_hits = Arc::new(AtomicUsize::new(0));
        let server_json_hits = Arc::clone(&json_hits);
        let server_mp3_hits = Arc::clone(&mp3_hits);
        let server = std::thread::spawn(move || {
            for _ in 0..4 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut buf = [0u8; 1024];
                let n = stream.read(&mut buf).unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]);
                let (ct, body) = if req.starts_with("GET /index.json") {
                    server_json_hits.fetch_add(1, Ordering::AcqRel);
                    (
                        "application/json",
                        format!(
                            r#"{{"type":"audioSourceList","audioSources":[{{"name":"CachedSrc","url":"http://{addr}/clip.mp3"}}]}}"#
                        ),
                    )
                } else {
                    server_mp3_hits.fetch_add(1, Ordering::AcqRel);
                    ("audio/mpeg", "ID3cached-anki-audio-payload".to_owned())
                };
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: {ct}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream.write_all(resp.as_bytes()).unwrap();
            }
        });

        let sources = vec![source(
            "JsonAudio",
            &format!("http://{addr}/index.json?w={{expression}}"),
            AudioSourceType::AudioSourceTypeJson,
            "",
        )];

        // 1st fetch populates both JSON expansion cache and audio payload cache.
        let (bytes1, fname1) = fetch_word_audio("橋", "はし", &sources).unwrap();
        assert_eq!(bytes1, b"ID3cached-anki-audio-payload");
        assert!(fname1.ends_with(".mp3"));
        assert_eq!(json_hits.load(Ordering::Acquire), 1);
        assert_eq!(mp3_hits.load(Ordering::Acquire), 1);

        // 2nd fetch hits shared cache with zero extra HTTP requests.
        let (bytes2, fname2) = fetch_word_audio("橋", "はし", &sources).unwrap();
        assert_eq!(bytes2, bytes1);
        assert_eq!(fname2, fname1);
        assert_eq!(json_hits.load(Ordering::Acquire), 1);
        assert_eq!(mp3_hits.load(Ordering::Acquire), 1);

        // Clearing shared cache forces fresh JSON + MP3 fetches.
        clear_shared_audio_cache();
        let (bytes3, _) = fetch_word_audio("橋", "はし", &sources).unwrap();
        assert_eq!(bytes3, bytes1);
        assert_eq!(json_hits.load(Ordering::Acquire), 2);
        assert_eq!(mp3_hits.load(Ordering::Acquire), 2);

        server.join().unwrap();
    }
}

////////////////////////////////////////////////////////////////////////////////
//
// Copyright (c) 2026 Ripose
//
// This file is part of Memento.
//
// Memento is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, version 2 of the License.
//
// Memento is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with Memento.  If not, see <https://www.gnu.org/licenses/>.
//
////////////////////////////////////////////////////////////////////////////////

/// Maximum number of downloaded pronunciation audio entries retained in cache.
pub const AUDIO_CACHE_CAPACITY: usize = 10;

/// Maximum accepted pronunciation audio payload size (32 MiB).
pub const MAX_AUDIO_PAYLOAD_BYTES: u64 = 32 * 1024 * 1024;

/// Validates an HTTP `Content-Type` header for a pronunciation audio download.
///
/// Rejects obvious non-audio error documents (`text/*`, `image/*`, JSON, HTML/XML,
/// JavaScript, PDF, multipart) while allowing `audio/*`, `video/*` (e.g. `video/mp4`,
/// `video/webm`), `application/octet-stream`, `application/ogg`, or an absent header.
pub fn validate_audio_content_type(content_type: Option<&str>) -> Result<(), String> {
    let Some(raw) = content_type else {
        return Ok(());
    };
    let media_type = raw
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    if media_type.is_empty() {
        return Ok(());
    }
    let rejected = media_type.starts_with("text/")
        || media_type.starts_with("image/")
        || media_type.starts_with("multipart/")
        || matches!(
            media_type.as_str(),
            "application/json"
                | "application/problem+json"
                | "application/xhtml+xml"
                | "application/xml"
                | "application/javascript"
                | "application/ecmascript"
                | "application/pdf"
        );
    if rejected {
        return Err(format!(
            "pronunciation audio source returned non-audio Content-Type {media_type:?}"
        ));
    }
    Ok(())
}

/// Guards against empty payloads or servers returning HTTP 200 with HTML, XML, or
/// JSON error pages disguised as `application/octet-stream` or missing `Content-Type`.
pub fn validate_audio_bytes(bytes: &[u8]) -> Result<(), String> {
    if bytes.is_empty() {
        return Err("pronunciation audio source returned an empty file".into());
    }
    let stripped = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    let trimmed_start = stripped
        .iter()
        .position(|b| !b.is_ascii_whitespace())
        .map(|idx| &stripped[idx..])
        .unwrap_or(&[]);
    if trimmed_start.is_empty() {
        return Err("pronunciation audio source returned an empty whitespace payload".into());
    }

    let prefix_len = trimmed_start.len().min(32);
    let prefix_lower: Vec<u8> = trimmed_start[..prefix_len]
        .iter()
        .map(u8::to_ascii_lowercase)
        .collect();
    for marker in [
        b"<!doctype html".as_slice(),
        b"<html",
        b"<head",
        b"<body",
        b"<?xml",
        b"<svg",
        b"<script",
        b"<title",
        b"<error",
        b"<h1",
        b"<div",
        b"<!--",
    ] {
        if prefix_lower.starts_with(marker) {
            return Err(
                "pronunciation audio source returned an HTML/XML document instead of audio".into(),
            );
        }
    }

    if trimmed_start.starts_with(b"{") {
        let trimmed_end = trimmed_start
            .iter()
            .rposition(|b| !b.is_ascii_whitespace())
            .map(|idx| &trimmed_start[..=idx])
            .unwrap_or(trimmed_start);
        let looks_like_json_prefix = trimmed_start.starts_with(b"{\"")
            && trimmed_start
                .windows(2)
                .take(128)
                .any(|w| w == b"\":");
        let looks_like_full_json = trimmed_end.ends_with(b"}")
            && trimmed_end.contains(&b':')
            && std::str::from_utf8(trimmed_end).is_ok();
        if looks_like_json_prefix || looks_like_full_json {
            return Err(
                "pronunciation audio source returned a JSON document instead of audio".into(),
            );
        }
    }

    Ok(())
}

/// Detects the most appropriate audio file extension from payload magic bytes,
/// HTTP `Content-Type`, or URL/path suffix, falling back to `".mp3"`.
pub fn detect_audio_extension(
    bytes: &[u8],
    content_type: Option<&str>,
    url_or_path: &str,
) -> &'static str {
    if bytes.starts_with(b"ID3") {
        return ".mp3";
    }
    if bytes.starts_with(b"OggS") {
        if bytes.len() >= 36 && &bytes[28..36] == b"OpusHead" {
            return ".opus";
        }
        return ".ogg";
    }
    if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WAVE" {
        return ".wav";
    }
    if bytes.starts_with(b"fLaC") {
        return ".flac";
    }
    if bytes.len() >= 8 && &bytes[4..8] == b"ftyp" {
        return ".m4a";
    }
    if bytes.starts_with(&[0x1A, 0x45, 0xDF, 0xA3]) {
        return ".webm";
    }
    if bytes.len() >= 2 && bytes[0] == 0xFF && (bytes[1] & 0xE0) == 0xE0 {
        // ADTS AAC uses a 12-bit syncword (0xFFF) with layer bits `00` (0xF0/0xF1/0xF8/0xF9).
        if (bytes[1] & 0xF6) == 0xF0 {
            let valid_sample_rate = bytes.len() < 3 || ((bytes[2] >> 2) & 0x0F) < 13;
            if valid_sample_rate {
                return ".aac";
            }
        }
        // MPEG Audio Layer II/III (MP3): reject reserved version (`01`), reject Layer I (`11`,
        // which collides with UTF-16 LE BOM `0xFF 0xFE` and `0xFF 0xFF`), and reject invalid
        // bitrate (`0xF`) or reserved sample-rate (`0x3`) fields in byte 2.
        let version = bytes[1] & 0x18;
        let layer = bytes[1] & 0x06;
        if version != 0x08 && matches!(layer, 0x02 | 0x04) {
            let valid_header =
                bytes.len() < 3 || ((bytes[2] & 0xF0) != 0xF0 && (bytes[2] & 0x0C) != 0x0C);
            if valid_header {
                return ".mp3";
            }
        }
    }

    if let Some(raw_ct) = content_type {
        let ct = raw_ct
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        match ct.as_str() {
            "audio/mpeg" | "audio/mp3" => return ".mp3",
            "audio/ogg" | "application/ogg" => return ".ogg",
            "audio/opus" => return ".opus",
            "audio/wav" | "audio/x-wav" | "audio/wave" => return ".wav",
            "audio/flac" | "audio/x-flac" => return ".flac",
            "audio/mp4" | "audio/x-m4a" | "audio/m4a" | "video/mp4" => return ".m4a",
            "audio/aac" | "audio/aacp" => return ".aac",
            "audio/webm" | "video/webm" => return ".webm",
            _ => {}
        }
    }

    let path_only = url_or_path
        .split(['?', '#'])
        .next()
        .unwrap_or(url_or_path)
        .to_ascii_lowercase();
    for ext in [".mp3", ".ogg", ".opus", ".wav", ".flac", ".m4a", ".aac", ".webm"] {
        if path_only.ends_with(ext) {
            return ext;
        }
    }

    ".mp3"
}

/// Entry stored in [`AudioCache`]. `file` is `None` for cached skip-hash entries
/// so known-placeholder downloads do not occupy temporary files on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachedAudioEntry<T> {
    pub url: String,
    pub file: Option<T>,
    pub md5: String,
}

/// Bounded LRU cache for downloaded pronunciation audio files.
///
/// Evicts only the least-recently-used entry when capacity is reached instead of
/// clearing the entire cache, and returns evicted entries so the caller controls
/// file drop ordering relative to active playback.
#[derive(Debug)]
pub struct AudioCache<T> {
    capacity: usize,
    entries: Vec<CachedAudioEntry<T>>,
}

impl<T> AudioCache<T> {
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            entries: Vec::new(),
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Looks up `url` and promotes it to most-recently-used.
    pub fn get_and_touch(&mut self, url: &str) -> Option<&CachedAudioEntry<T>> {
        let idx = self.entries.iter().position(|entry| entry.url == url)?;
        let last = self.entries.len() - 1;
        if idx != last {
            let entry = self.entries.remove(idx);
            self.entries.push(entry);
        }
        self.entries.last()
    }

    /// Removes and returns the cached entry for `url`, if present.
    pub fn remove(&mut self, url: &str) -> Option<CachedAudioEntry<T>> {
        let idx = self.entries.iter().position(|entry| entry.url == url)?;
        Some(self.entries.remove(idx))
    }

    /// Inserts or replaces `url` as most-recently-used, evicting the oldest entry
    /// if capacity would be exceeded. Returns any replaced or evicted entries.
    pub fn insert(
        &mut self,
        url: String,
        file: Option<T>,
        md5: String,
    ) -> Vec<CachedAudioEntry<T>> {
        let mut evicted = Vec::new();
        if let Some(idx) = self.entries.iter().position(|entry| entry.url == url) {
            evicted.push(self.entries.remove(idx));
        }
        while self.entries.len() >= self.capacity {
            evicted.push(self.entries.remove(0));
        }
        self.entries.push(CachedAudioEntry { url, file, md5 });
        evicted
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::{
        detect_audio_extension, validate_audio_bytes, validate_audio_content_type, AudioCache,
    };

    #[test]
    fn content_type_and_payload_validation_rejects_html_json_and_empty_bodies() {
        for ok_ct in [
            None,
            Some("audio/mpeg"),
            Some("audio/ogg; codecs=opus"),
            Some("video/mp4"),
            Some("application/octet-stream"),
            Some("application/ogg"),
        ] {
            assert!(validate_audio_content_type(ok_ct).is_ok(), "unexpected reject: {ok_ct:?}");
        }
        for bad_ct in [
            Some("text/html; charset=utf-8"),
            Some("text/plain"),
            Some("application/json"),
            Some("application/xhtml+xml"),
            Some("image/png"),
        ] {
            assert!(validate_audio_content_type(bad_ct).is_err(), "expected reject: {bad_ct:?}");
        }

        assert!(validate_audio_bytes(b"").is_err());
        assert!(validate_audio_bytes(b"   \r\n\t  ").is_err());
        assert!(validate_audio_bytes(b"<!DOCTYPE html><html><body>404</body></html>").is_err());
        assert!(validate_audio_bytes(b"\xEF\xBB\xBF<html><title>Error</title></html>").is_err());
        assert!(validate_audio_bytes(b"<?xml version=\"1.0\"?><Error/>").is_err());
        assert!(validate_audio_bytes(b"{\"error\":\"not found\"}").is_err());

        assert!(validate_audio_bytes(b"ID3\x03\x00\x00\x00\x00\x00\x00").is_ok());
        assert!(validate_audio_bytes(b"OggS\x00\x02\x00\x00").is_ok());
        assert!(validate_audio_bytes(b"RIFF\x24\x00\x00\x00WAVEfmt ").is_ok());
        assert!(validate_audio_bytes(b"\xFF\xFB\x90\x00audio").is_ok());
    }

    #[test]
    fn detect_audio_extension_uses_magic_content_type_then_url() {
        assert_eq!(detect_audio_extension(b"ID3...", None, "https://a/x"), ".mp3");
        let mut opus = vec![0u8; 40];
        opus[..4].copy_from_slice(b"OggS");
        opus[28..36].copy_from_slice(b"OpusHead");
        assert_eq!(detect_audio_extension(&opus, None, "https://a/x"), ".opus");
        assert_eq!(detect_audio_extension(b"OggS\x00\x02", None, "https://a/x"), ".ogg");
        assert_eq!(detect_audio_extension(b"RIFF1234WAVE", None, "https://a/x"), ".wav");
        assert_eq!(detect_audio_extension(b"fLaC\x00", None, "https://a/x"), ".flac");
        assert_eq!(detect_audio_extension(b"\x00\x00\x00\x18ftypM4A ", None, "https://a/x"), ".m4a");
        assert_eq!(detect_audio_extension(b"raw", Some("audio/ogg"), "https://a/x"), ".ogg");
        assert_eq!(detect_audio_extension(b"raw", None, "https://a/word.wav?k=1"), ".wav");
        assert_eq!(detect_audio_extension(b"raw", None, "https://a/unknown"), ".mp3");

        // Valid MP3 & ADTS AAC frame sync headers.
        assert_eq!(detect_audio_extension(b"\xFF\xFB\x90\x00", None, "https://a/x.ogg"), ".mp3");
        assert_eq!(detect_audio_extension(b"\xFF\xF1\x50\x80", None, "https://a/x.mp3"), ".aac");

        // UTF-16 LE BOM (0xFF 0xFE) and 0xFF 0xFF must not falsely match MP3 frame sync.
        assert_eq!(
            detect_audio_extension(b"\xFF\xFE<\x00h\x00", Some("audio/ogg"), "https://a/x"),
            ".ogg"
        );
        assert_eq!(
            detect_audio_extension(b"\xFF\xFE\x00\x00", None, "https://a/clip.wav"),
            ".wav"
        );
        assert_eq!(
            detect_audio_extension(b"\xFF\xFF\x00\x00", None, "https://a/clip.flac"),
            ".flac"
        );
    }

    #[test]
    fn lru_audio_cache_promotes_hits_and_evicts_only_oldest() {
        let mut cache = AudioCache::new(3);
        assert!(cache.insert("u1".into(), Some(1), "h1".into()).is_empty());
        assert!(cache.insert("u2".into(), Some(2), "h2".into()).is_empty());
        assert!(cache.insert("u3".into(), Some(3), "h3".into()).is_empty());

        // Touch u1 so u2 becomes LRU.
        assert_eq!(cache.get_and_touch("u1").unwrap().file, Some(1));

        // Insert u4 -> must evict u2 only, retaining u3, u1, u4.
        let evicted = cache.insert("u4".into(), Some(4), "h4".into());
        assert_eq!(evicted.len(), 1);
        assert_eq!(evicted[0].url, "u2");
        assert_eq!(cache.len(), 3);
        assert!(cache.get_and_touch("u2").is_none());
        assert!(cache.get_and_touch("u1").is_some());
        assert!(cache.get_and_touch("u3").is_some());
        assert!(cache.get_and_touch("u4").is_some());
    }
}

//! Audio source URL templates and JSON audioSourceList extraction.

/// RFC 3986 percent-encoding for URL path/query template parameters (`{expression}`,
/// `{term}`, `{reading}`). Unreserved characters (`A-Z`, `a-z`, `0-9`, `-`, `_`, `.`, `~`)
/// are kept literal; all other UTF-8 bytes (kanji, kana, spaces, `&`, `=`, `?`, `#`, `+`,
/// `%`, `/`, etc.) are encoded as uppercase `%XX`.
pub fn percent_encode_url_component(input: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(input.len());
    for byte in input.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(byte as char);
        } else {
            out.push('%');
            out.push(HEX[(byte >> 4) as usize] as char);
            out.push(HEX[(byte & 0x0F) as usize] as char);
        }
    }
    out
}

fn expand_template_single_pass(
    template: &str,
    expression_val: &str,
    reading_val: &str,
) -> String {
    let mut out = String::with_capacity(template.len() + expression_val.len() + reading_val.len());
    let mut rest = template;
    while let Some(brace) = rest.find('{') {
        out.push_str(&rest[..brace]);
        let after = &rest[brace..];
        if let Some(tail) = after.strip_prefix("{expression}") {
            out.push_str(expression_val);
            rest = tail;
        } else if let Some(tail) = after.strip_prefix("{term}") {
            out.push_str(expression_val);
            rest = tail;
        } else if let Some(tail) = after.strip_prefix("{reading}") {
            out.push_str(reading_val);
            rest = tail;
        } else {
            out.push('{');
            rest = &after[1..];
        }
    }
    out.push_str(rest);
    out
}

fn is_url_scheme_template(trimmed: &str) -> bool {
    trimmed
        .get(..7)
        .is_some_and(|p| p.eq_ignore_ascii_case("http://"))
        || trimmed
            .get(..8)
            .is_some_and(|p| p.eq_ignore_ascii_case("https://"))
        || trimmed
            .get(..7)
            .is_some_and(|p| p.eq_ignore_ascii_case("file://"))
}

/// Expands `{expression}`, `{term}`, and `{reading}` in a single pass, always
/// percent-encoding the substituted values for safe embedding in URLs (including
/// relative URLs returned from HTTP JSON audio sources).
///
/// When `reading` is empty or whitespace-only, `{reading}` falls back to `expression`.
pub fn expand_url_template_encoded(template: &str, expression: &str, reading: &str) -> String {
    let trimmed = template.trim();
    let effective_reading = if reading.trim().is_empty() {
        expression
    } else {
        reading
    };
    let expr_encoded = percent_encode_url_component(expression);
    let read_encoded = percent_encode_url_component(effective_reading);
    expand_template_single_pass(trimmed, &expr_encoded, &read_encoded)
}

/// Expands `{expression}`, `{term}`, and `{reading}` in a single pass.
///
/// For `http://`, `https://`, and `file://` URL templates, substituted values are
/// RFC 3986 percent-encoded so kanji, kana, spaces, and URL delimiters (`&`, `#`,
/// `?`, `+`, `%`, `/`) never corrupt the URL. For local filesystem path templates,
/// values are substituted literally so UTF-8 filenames remain valid paths on disk.
///
/// When `reading` is empty or whitespace-only, `{reading}` falls back to `expression`.
pub fn expand_url_template(template: &str, expression: &str, reading: &str) -> String {
    let trimmed = template.trim();
    if is_url_scheme_template(trimmed) {
        return expand_url_template_encoded(trimmed, expression, reading);
    }
    let effective_reading = if reading.trim().is_empty() {
        expression
    } else {
        reading
    };
    expand_template_single_pass(trimmed, expression, effective_reading)
}

/// Returns `true` when `skip_hash` is non-empty (after trimming) and matches
/// `actual_hex` case-insensitively.
pub fn md5_matches_skip_hash(actual_hex: &str, skip_hash: &str) -> bool {
    let expected = skip_hash.trim();
    !expected.is_empty() && actual_hex.trim().eq_ignore_ascii_case(expected)
}

/// Parses a Yomitan/Subomi `audioSourceList` JSON payload into UTF-8 `(name, url)` pairs.
///
/// - Strips an optional leading UTF-8 BOM (`0xEF 0xBB 0xBF`).
/// - Accepts entries where `"name"` is omitted or `null` (yielding an empty name so callers
///   can fall back to the parent source's display name).
/// - Trims `"url"` and drops entries with missing, non-string, or blank URLs.
pub fn parse_audio_source_list(bytes: &[u8]) -> Option<Vec<(String, String)>> {
    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    let root: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    let obj = root.as_object()?;
    if obj.get("type").and_then(serde_json::Value::as_str) != Some("audioSourceList") {
        return None;
    }
    let Some(sources) = obj.get("audioSources").and_then(serde_json::Value::as_array) else {
        return Some(Vec::new());
    };
    let mut entries = Vec::with_capacity(sources.len());
    for source in sources {
        let Some(item) = source.as_object() else {
            continue;
        };
        let Some(raw_url) = item.get("url").and_then(serde_json::Value::as_str) else {
            continue;
        };
        let url = raw_url.trim();
        if url.is_empty() {
            continue;
        }
        let name = match item.get("name") {
            None | Some(serde_json::Value::Null) => String::new(),
            Some(serde_json::Value::String(s)) => s.trim().to_owned(),
            Some(_) => continue,
        };
        entries.push((name, url.to_owned()));
    }
    Some(entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_invalid_json_and_mismatched_payload_types() {
        for json in [
            b"{".as_slice(),
            b"[]",
            b"null",
            br#"{"type":"wrong","audioSources":[]}"#,
            br#"{"type":42,"audioSources":[]}"#,
            br#"{"type":"audioSourceList","audioSources":[}"#,
            br#"{"type":"audioSourceList","audioSources":[1,]}"#,
            br#"{"type":"audioSourceList","audioSources":[{"name":"\uXYZ1","url":"x"}]}"#,
            br#"{"type":"audioSourceList","audioSources":[] }garbage"#,
        ] {
            assert!(
                parse_audio_source_list(json).is_none(),
                "unexpected valid JSON: {json:?}"
            );
        }
        assert_eq!(
            parse_audio_source_list(br#"{"type":"audioSourceList"}"#),
            Some(Vec::new())
        );
        assert_eq!(
            parse_audio_source_list(br#"{"type":"audioSourceList","audioSources":{}}"#),
            Some(Vec::new())
        );
    }

    #[test]
    fn percent_encoding_and_single_pass_url_template_expansion() {
        // Kanji, kana, spaces, and URL delimiters (&, =, ?, #, +, %, /) are encoded.
        assert_eq!(
            expand_url_template(
                "https://assets.languagepod101.com/dictionary/japanese/audiomp3.php?kanji={expression}&kana={reading}",
                "A&B#1?+100% 食べる",
                "たべる/かな"
            ),
            "https://assets.languagepod101.com/dictionary/japanese/audiomp3.php?kanji=A%26B%231%3F%2B100%25%20%E9%A3%9F%E3%81%B9%E3%82%8B&kana=%E3%81%9F%E3%81%B9%E3%82%8B%2F%E3%81%8B%E3%81%AA"
        );
        // Empty or whitespace reading falls back to expression.
        assert_eq!(
            expand_url_template("https://example.test/{term}/{reading}.mp3", "猫", "   "),
            "https://example.test/%E7%8C%AB/%E7%8C%AB.mp3"
        );
        // Reading equal to expression encodes cleanly.
        assert_eq!(
            expand_url_template("https://example.test/?k={expression}&r={reading}", "テレビ", "テレビ"),
            "https://example.test/?k=%E3%83%86%E3%83%AC%E3%83%93&r=%E3%83%86%E3%83%AC%E3%83%93"
        );
        // Local file path preserves raw UTF-8 without percent-encoding, and does not double-expand.
        assert_eq!(
            expand_url_template("/audio/{expression}/{reading}.mp3", "{reading}", "たべる"),
            "/audio/{reading}/たべる.mp3"
        );
        // file:// URL percent-encodes special characters so to_file_path round-trips cleanly.
        assert_eq!(
            expand_url_template("file:///audio/{expression}.mp3", "食#1?.mp3", ""),
            "file:///audio/%E9%A3%9F%231%3F.mp3.mp3"
        );
        // Relative URL encoding helper encodes placeholders even without a scheme.
        assert_eq!(
            expand_url_template_encoded("/audio/{expression}?r={reading}", "食べる", ""),
            "/audio/%E9%A3%9F%E3%81%B9%E3%82%8B?r=%E9%A3%9F%E3%81%B9%E3%82%8B"
        );
    }

    #[test]
    fn audio_source_list_accepts_missing_or_null_names_bom_and_relative_urls() {
        let mut with_bom = vec![0xEF, 0xBB, 0xBF];
        with_bom.extend_from_slice(
            br#"{"type":"audioSourceList","audioSources":[{"url":"/clips/1.mp3"},{"name":null,"url":"sub/2.ogg"},{"name":"NHK","url":"https://a.test/3.mp3"},{"name":"blank","url":"   "},{"name":7,"url":"/skip.mp3"}]}"#,
        );
        assert_eq!(
            parse_audio_source_list(&with_bom),
            Some(vec![
                (String::new(), "/clips/1.mp3".to_owned()),
                (String::new(), "sub/2.ogg".to_owned()),
                ("NHK".to_owned(), "https://a.test/3.mp3".to_owned()),
            ])
        );
        assert_eq!(
            parse_audio_source_list(br#"{"type":"audioSourceList","audioSources":[]}"#),
            Some(Vec::new())
        );
        assert_eq!(parse_audio_source_list(br#"{"type":"other","audioSources":[]}"#), None);
        assert_eq!(parse_audio_source_list(b"<html>502 Bad Gateway</html>"), None);
    }

    #[test]
    fn skip_hash_matching_is_case_insensitive_and_ignores_blank_hashes() {
        assert!(md5_matches_skip_hash(
            "7e2c2f954ef6051373ba916f000168dc",
            "  7E2C2F954EF6051373BA916F000168DC  "
        ));
        assert!(!md5_matches_skip_hash("7e2c2f954ef6051373ba916f000168dc", ""));
        assert!(!md5_matches_skip_hash("7e2c2f954ef6051373ba916f000168dc", "   \t "));
        assert!(!md5_matches_skip_hash("7e2c2f954ef6051373ba916f000168dc", "deadbeef"));
    }
}

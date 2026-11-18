//! Audio source URL templates and JSON audioSourceList extraction.

use std::borrow::Cow;

fn replace_all<'a>(input: &'a [u16], needle: &[u8], replacement: &[u16]) -> Option<Vec<u16>> {
    // The template tokens are ASCII; compare directly to UTF-16 units without
    // allocating an encoded needle or decoding input (which would lose UTF-16).
    let matches_at = |units: &[u16]| units.iter().zip(needle).all(|(&unit, &ascii)| unit == ascii as u16);
    let first = input.windows(needle.len()).position(&matches_at)?;
    let mut result = Vec::with_capacity(input.len());
    result.extend_from_slice(&input[..first]);
    let mut offset = first;
    loop {
        result.extend_from_slice(replacement);
        offset += needle.len();
        let Some(next) = input[offset..].windows(needle.len()).position(&matches_at) else {
            result.extend_from_slice(&input[offset..]);
            break;
        };
        result.extend_from_slice(&input[offset..offset + next]);
        offset += next;
    }
    Some(result)
}

/// Apply each Qt QString::replace pass in its original order. Each pass
/// replaces matches in the current string, but does not rescan its own inserted
/// text. An empty reading uses the expression for `{reading}`.
pub fn expand_url(url: &[u16], expression: &[u16], reading: &[u16]) -> Vec<u16> {
    let mut expanded = Cow::Borrowed(url);
    let reading = if reading.is_empty() { expression } else { reading };
    for (token, replacement) in [
        (b"{expression}".as_slice(), expression),
        (b"{term}".as_slice(), expression),
        (b"{reading}".as_slice(), reading),
    ] {
        if let Some(next) = replace_all(&expanded, token, replacement) {
            expanded = Cow::Owned(next);
        }
    }
    expanded.into_owned()
}

// Qt's QJsonDocument preserves a lone \uD800 escape as one QString UTF-16
// unit. serde_json rejects it, so decoding through String loses observable
// audio-source names. This small JSON reader keeps strings as UTF-16; the
// remaining values only matter for structural validation.
enum JsonValue {
    String(Vec<u16>),
    Array(Vec<JsonValue>),
    Object(Vec<(Vec<u16>, JsonValue)>),
    Other,
}

impl JsonValue {
    fn field(&self, name: &str) -> Option<&Self> {
        let JsonValue::Object(fields) = self else { return None };
        fields.iter().rev().find(|(key, _)| {
            key.len() == name.len()
                && key.iter().zip(name.bytes()).all(|(&unit, byte)| unit == u16::from(byte))
        }).map(|(_, value)| value)
    }

    fn text(&self) -> Option<&[u16]> {
        if let Self::String(value) = self { Some(value) } else { None }
    }
}

struct JsonReader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl JsonReader<'_> {
    fn whitespace(&mut self) {
        while matches!(self.bytes.get(self.pos), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.pos += 1;
        }
    }

    fn consume(&mut self, byte: u8) -> bool {
        if self.bytes.get(self.pos) == Some(&byte) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn literal(&mut self, value: &[u8]) -> Option<JsonValue> {
        if !self.bytes.get(self.pos..)?.starts_with(value) { return None }
        self.pos += value.len();
        Some(JsonValue::Other)
    }

    fn hex_unit(&mut self) -> Option<u16> {
        let mut result = 0u16;
        for _ in 0..4 {
            let hex = self.bytes.get(self.pos)?;
            let digit = (*hex as char).to_digit(16)?;
            result = (result << 4) | digit as u16;
            self.pos += 1;
        }
        Some(result)
    }

    fn string(&mut self) -> Option<Vec<u16>> {
        if !self.consume(b'"') { return None }
        let mut text = Vec::new();
        loop {
            let start = self.pos;
            while let Some(&byte) = self.bytes.get(self.pos) {
                if byte == b'"' || byte == b'\\' { break }
                if byte < 0x20 { return None }
                self.pos += 1;
            }
            text.extend(std::str::from_utf8(self.bytes.get(start..self.pos)?).ok()?.encode_utf16());
            let byte = *self.bytes.get(self.pos)?;
            self.pos += 1;
            if byte == b'"' { return Some(text) }
            let escape = *self.bytes.get(self.pos)?;
            self.pos += 1;
            match escape {
                b'"' | b'\\' | b'/' => text.push(u16::from(escape)),
                b'b' => text.push(8),
                b'f' => text.push(12),
                b'n' => text.push(10),
                b'r' => text.push(13),
                b't' => text.push(9),
                b'u' => text.push(self.hex_unit()?),
                _ => return None,
            }
        }
    }

    fn number(&mut self) -> Option<JsonValue> {
        self.consume(b'-');
        if !self.consume(b'0') {
            if !matches!(self.bytes.get(self.pos), Some(b'1'..=b'9')) { return None }
            self.pos += 1;
            while matches!(self.bytes.get(self.pos), Some(b'0'..=b'9')) { self.pos += 1 }
        }
        if self.consume(b'.') {
            if !matches!(self.bytes.get(self.pos), Some(b'0'..=b'9')) { return None }
            while matches!(self.bytes.get(self.pos), Some(b'0'..=b'9')) { self.pos += 1 }
        }
        if self.consume(b'e') || self.consume(b'E') {
            if !self.consume(b'+') { self.consume(b'-'); }
            if !matches!(self.bytes.get(self.pos), Some(b'0'..=b'9')) { return None }
            while matches!(self.bytes.get(self.pos), Some(b'0'..=b'9')) { self.pos += 1 }
        }
        Some(JsonValue::Other)
    }

    fn value(&mut self, depth: usize) -> Option<JsonValue> {
        if depth >= 128 { return None }
        self.whitespace();
        match *self.bytes.get(self.pos)? {
            b'"' => Some(JsonValue::String(self.string()?)),
            b'{' => {
                self.pos += 1;
                self.whitespace();
                let mut fields = Vec::new();
                if self.consume(b'}') { return Some(JsonValue::Object(fields)) }
                loop {
                    self.whitespace();
                    let key = self.string()?;
                    self.whitespace();
                    if !self.consume(b':') { return None }
                    fields.push((key, self.value(depth + 1)?));
                    self.whitespace();
                    if self.consume(b'}') { return Some(JsonValue::Object(fields)) }
                    if !self.consume(b',') { return None }
                }
            }
            b'[' => {
                self.pos += 1;
                self.whitespace();
                let mut items = Vec::new();
                if self.consume(b']') { return Some(JsonValue::Array(items)) }
                loop {
                    items.push(self.value(depth + 1)?);
                    self.whitespace();
                    if self.consume(b']') { return Some(JsonValue::Array(items)) }
                    if !self.consume(b',') { return None }
                }
            }
            b't' => self.literal(b"true"),
            b'f' => self.literal(b"false"),
            b'n' => self.literal(b"null"),
            b'-' | b'0'..=b'9' => self.number(),
            _ => None,
        }
    }
}

/// Parses an `audioSourceList` JSON payload into `(name, url)` UTF-16 pairs.
pub fn parse_json(bytes: &[u8]) -> Option<Vec<(Vec<u16>, Vec<u16>)>> {
    let mut reader = JsonReader { bytes, pos: 0 };
    let root = reader.value(0)?;
    reader.whitespace();
    if reader.pos != bytes.len() { return None }
    let ty = root.field("type")?.text()?;
    if ty.len() != b"audioSourceList".len()
        || !ty.iter().zip(b"audioSourceList").all(|(&unit, &ascii)| unit == u16::from(ascii))
    {
        return None;
    }
    // QJsonValue::toArray() yields an empty array for missing or wrong-typed
    // fields, even when the root itself is a valid audioSourceList.
    let Some(JsonValue::Array(sources)) = root.field("audioSources") else { return Some(Vec::new()) };
    Some(sources.iter().filter_map(|source| {
        let name = source.field("name")?.text()?;
        let url = source.field("url")?.text()?;
        Some((name.to_vec(), url.to_vec()))
    }).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utf16(text: &str) -> Vec<u16> { text.encode_utf16().collect() }

    #[test]
    fn unicode_replacement_obeys_pass_order_without_rescanning_insertions() {
        let url = utf16("https://x/{expression}/{term}/{reading}/{expression}");
        let expression = utf16("{term}語😀");
        let reading = utf16("{expression}🌸");
        assert_eq!(
            expand_url(&url, &expression, &reading),
            utf16("https://x/{term}語😀語😀/{term}語😀/{expression}🌸/{term}語😀語😀")
        );
    }

    #[test]
    fn empty_reading_falls_back_and_utf16_is_not_normalized() {
        let url = utf16("🍣/{reading}/{expression}/{term}");
        assert_eq!(expand_url(&url, &utf16("辞書"), &[]), utf16("🍣/辞書/辞書/辞書"));
        let mut ill_formed = vec![0xd800];
        ill_formed.extend(utf16("{reading}"));
        ill_formed.push(0xdc00);
        assert_eq!(expand_url(&ill_formed, &[0xdfff], &[]), vec![0xd800, 0xdfff, 0xdc00]);
    }

    #[test]
    fn accepts_in_order_only_entries_with_string_names_and_urls() {
        let json = r#"{"type":"audioSourceList","audioSources":[{"name":"東京","url":"https://例え/😀"},null,{"name":7,"url":"x"},{"name":"","url":""},{"url":"x"},{"name":"last","url":"z","ignored":false}]}"#;
        let parsed = parse_json(json.as_bytes()).unwrap();
        assert_eq!(parsed, vec![
            (utf16("東京"), utf16("https://例え/😀")),
            (utf16(""), utf16("")),
            (utf16("last"), utf16("z")),
        ]);
        assert_eq!(parse_json(br#"{"type":"audioSourceList","audioSources":[]}"#), Some(vec![]));
    }

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
            assert!(parse_json(json).is_none(), "unexpected valid JSON: {json:?}");
        }
        assert_eq!(parse_json(br#"{"type":"audioSourceList"}"#), Some(Vec::new()));
        assert_eq!(parse_json(br#"{"type":"audioSourceList","audioSources":{}}"#), Some(Vec::new()));
    }

    #[test]
    fn json_preserves_qt_unpaired_utf16_and_last_duplicate_fields() {
        let json = br#"{"type":"wrong","type":"audioSourceList","audioSources":[{"name":"\ud800","url":"\udfff"},{"name":"\ud83d\ude3a","url":"A\u0000B"}]}"#;
        assert_eq!(parse_json(json), Some(vec![
            (vec![0xd800], vec![0xdfff]),
            (utf16("😺"), vec![65, 0, 66]),
        ]));
    }
}

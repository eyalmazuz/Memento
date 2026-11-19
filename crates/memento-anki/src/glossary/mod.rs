//! Pure Rust algorithms from `src/anki/glossarybuilder.cpp` for building
//! Anki-compatible HTML glossaries. Strings and JSON keys remain UTF-16
//! as in QString.

pub(crate) mod escape;
mod media;
mod structured;

use std::collections::BTreeSet;

use crate::json::{JsonObject, JsonValue};

use self::escape::{append_escaped, ascii_to_u16};
use self::media::add_image;
use self::structured::add_structured_content;

pub use self::escape::{
    escape_html, escape_html_lines, format_g, qstring_number, qstring_number_i32,
};
pub use self::structured::{
    is_supported_structured_tag, structured_data_attribute_name, MAX_STRUCTURED_CONTENT_DEPTH,
};

const KEY_CONTENT: [u16; 7] = ascii_to_u16("content");
const KEY_TEXT: [u16; 4] = ascii_to_u16("text");
const KEY_TYPE: [u16; 4] = ascii_to_u16("type");

const VALUE_TYPE_IMAGE: [u16; 5] = ascii_to_u16("image");
const VALUE_TYPE_STRUCTURED_CONTENT: [u16; 18] = ascii_to_u16("structured-content");
const VALUE_TYPE_TEXT: [u16; 4] = ascii_to_u16("text");

/// Render just the two top-level *textual* glossary entry forms: a JSON
/// string, or an object with `"type": "text"`. Other values return None;
/// this is not a replacement for `build_glossary`'s image/structured dispatch.
/// `QJsonValue::toString()` on a missing or non-string `text` yields empty.
pub fn render_text_entry(value: &JsonValue) -> Option<Vec<u16>> {
    match value {
        JsonValue::String(text) => Some(escape_html_lines(text)),
        JsonValue::Object(obj)
            if obj.get(KEY_TYPE.as_slice()).and_then(JsonValue::as_qstring)
                == Some(VALUE_TYPE_TEXT.as_slice()) =>
        {
            let text = obj
                .get(KEY_TEXT.as_slice())
                .and_then(JsonValue::as_qstring)
                .unwrap_or(&[]);
            Some(escape_html_lines(text))
        }
        _ => None,
    }
}

fn add_text(obj: &JsonObject, out: &mut Vec<u16>) {
    let text = obj
        .get(KEY_TEXT.as_slice())
        .and_then(JsonValue::as_qstring)
        .unwrap_or(&[]);
    append_escaped(text, out, true);
}

/// The complete `GlossaryBuilder::buildGlossary` pipeline.
/// Returns a tuple of:
/// 1. The list of HTML glossary strings (`QStringList`).
/// 2. The set of unique `(full_path, hash)` file entries inserted into `fileMap`.
pub fn build_glossary<F>(
    definitions: &[JsonValue],
    basepath: &[u16],
    mut md5_calc: F,
) -> (Vec<Vec<u16>>, Vec<(Vec<u16>, Vec<u16>)>)
where
    F: FnMut(&[u16]) -> Option<[u16; 32]>,
{
    let mut basepath_normalized = Vec::new();
    if !basepath.is_empty() {
        basepath_normalized.reserve(basepath.len() + 1);
        basepath_normalized.extend_from_slice(basepath);
        let sep = if cfg!(windows) {
            b'\\' as u16
        } else {
            b'/' as u16
        };
        if !matches!(
            basepath_normalized.last().copied(),
            Some(u) if u == b'/' as u16 || u == b'\\' as u16
        ) {
            basepath_normalized.push(sep);
        }
    }

    let mut glossaries = Vec::new();
    let mut file_map = BTreeSet::new();

    for val in definitions {
        let mut glossary = Vec::new();
        match val {
            JsonValue::String(s) => {
                append_escaped(s, &mut glossary, true);
            }
            JsonValue::Object(obj) => {
                let obj_type = obj.get(KEY_TYPE.as_slice()).and_then(JsonValue::as_qstring);
                if obj_type == Some(VALUE_TYPE_STRUCTURED_CONTENT.as_slice()) {
                    if let Some(content) = obj.get(KEY_CONTENT.as_slice()) {
                        add_structured_content(
                            content,
                            &basepath_normalized,
                            &mut glossary,
                            &mut file_map,
                            &mut md5_calc,
                            0,
                        );
                    }
                } else if obj_type == Some(VALUE_TYPE_IMAGE.as_slice()) {
                    add_image(
                        obj,
                        &basepath_normalized,
                        &mut glossary,
                        &mut file_map,
                        &mut md5_calc,
                    );
                } else if obj_type == Some(VALUE_TYPE_TEXT.as_slice()) {
                    add_text(obj, &mut glossary);
                }
            }
            _ => continue,
        }
        glossaries.push(glossary);
    }

    (glossaries, file_map.into_iter().collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    fn u(text: &str) -> Vec<u16> {
        text.encode_utf16().collect()
    }

    fn u_str(units: &[u16]) -> String {
        String::from_utf16_lossy(units)
    }

    #[test]
    fn escapes_qt_four_html_metacharacters_but_not_apostrophes() {
        assert_eq!(
            escape_html(&u("&<>'\" &amp;")),
            u("&amp;&lt;&gt;'&quot; &amp;amp;")
        );
        assert_eq!(escape_html(&[]), Vec::<u16>::new());
    }

    #[test]
    fn literal_lf_becomes_br_after_escaping_without_double_escape() {
        assert_eq!(
            escape_html_lines(&u("<a>\r\nB\n\u{2028}\u{2029}")),
            u("&lt;a&gt;\r<br>B<br>\u{2028}\u{2029}")
        );
        assert_eq!(escape_html_lines(&u("<br>")), u("&lt;br&gt;"));
    }

    #[test]
    fn preserves_surrogates_and_supplementary_characters() {
        let input = [0xD800, 0xD83D, 0xDE00, 0x26, 0xDC00];
        let escaped = [0xD800, 0xD83D, 0xDE00, 0x26, 0x61, 0x6d, 0x70, 0x3b, 0xDC00];
        assert_eq!(escape_html(&input), escaped);
        assert_eq!(escape_html_lines(&input), escaped);
    }

    #[test]
    fn textual_entries_obey_qjson_type_checks() {
        let mut obj = JsonObject::new();
        obj.insert(u("type"), JsonValue::String(u("text")));
        obj.insert(u("text"), JsonValue::String(u("<hello>\nworld")));
        assert_eq!(
            render_text_entry(&JsonValue::Object(obj.clone())),
            Some(u("&lt;hello&gt;<br>world"))
        );
        assert_eq!(
            render_text_entry(&JsonValue::String(u("x&y"))),
            Some(u("x&amp;y"))
        );
        obj.insert(u("text"), JsonValue::Number(2.0));
        assert_eq!(
            render_text_entry(&JsonValue::Object(obj.clone())),
            Some(Vec::new())
        );
        obj.insert(u("type"), JsonValue::String(u("image")));
        assert_eq!(render_text_entry(&JsonValue::Object(obj)), None);
        assert_eq!(render_text_entry(&JsonValue::Null), None);
    }

    #[test]
    fn tag_whitelist_is_case_sensitive_and_exact() {
        for tag in [
            "br", "ruby", "rt", "rp", "table", "thead", "tbody", "tfoot", "tr", "td", "th", "span",
            "div", "ol", "ul", "li", "details", "summary", "img", "a",
        ] {
            assert!(is_supported_structured_tag(&u(tag)), "{tag}");
        }
        for tag in ["BR", "script", "video", "a ", "", "😀"] {
            assert!(!is_supported_structured_tag(&u(tag)), "{tag}");
        }
    }

    #[test]
    fn structured_data_attribute_name_uses_unicode_lowercase() {
        assert_eq!(
            u_str(&structured_data_attribute_name(&u("reading"))),
            "data-sc-reading"
        );
        assert_eq!(
            u_str(&structured_data_attribute_name(&u("exampleSentence"))),
            "data-sc-example-sentence"
        );
        assert_eq!(
            u_str(&structured_data_attribute_name(&u("fooBarBaz"))),
            "data-sc-foo-bar-baz"
        );
        assert_eq!(
            u_str(&structured_data_attribute_name(&u("ÄÖÜ"))),
            "data-sc--ä-ö-ü"
        );
        // Astral uppercase (Deseret Capital Letter Long I -> Small Letter Long I)
        assert_eq!(
            u_str(&structured_data_attribute_name(&u("\u{10400}"))),
            "data-sc--\u{10428}"
        );
        // Lone surrogate survives cleanly
        let lone = structured_data_attribute_name(&[0xD800]);
        assert!(lone.ends_with(&[0xD800]));
    }

    #[test]
    fn double_formatting_matches_qstring_number() {
        assert_eq!(format_g(0.0, 6), "0");
        assert_eq!(format_g(-0.0, 6), "0");
        assert_eq!(format_g(1.0, 6), "1");
        assert_eq!(format_g(-1.0, 6), "-1");
        assert_eq!(format_g(12.5, 6), "12.5");
        assert_eq!(format_g(120000.0, 6), "120000");
        assert_eq!(format_g(0.0001, 6), "0.0001");
        assert_eq!(format_g(0.00001, 6), "1e-05");
        assert_eq!(format_g(1234567.0, 6), "1.23457e+06");
    }

    #[test]
    fn build_glossary_all_entry_types_and_skips() {
        let mut defs = Vec::new();
        defs.push(JsonValue::String(u("<b>def1</b>\nline2")));
        let mut text_obj = JsonObject::new();
        text_obj.insert(u("type"), JsonValue::String(u("text")));
        text_obj.insert(u("text"), JsonValue::String(u("hello & world")));
        defs.push(JsonValue::Object(text_obj));
        let mut unknown_obj = JsonObject::new();
        unknown_obj.insert(u("type"), JsonValue::String(u("unknown")));
        defs.push(JsonValue::Object(unknown_obj));
        defs.push(JsonValue::Null);
        defs.push(JsonValue::Bool(true));
        defs.push(JsonValue::Number(42.0));
        defs.push(JsonValue::Array(vec![]));

        let mock_md5 = |_path: &[u16]| -> Option<[u16; 32]> { None };
        let (glossaries, files) = build_glossary(&defs, &u("/base"), mock_md5);

        assert_eq!(glossaries.len(), 3);
        assert_eq!(u_str(&glossaries[0]), "&lt;b&gt;def1&lt;/b&gt;<br>line2");
        assert_eq!(u_str(&glossaries[1]), "hello &amp; world");
        assert_eq!(u_str(&glossaries[2]), "");
        assert!(files.is_empty());
    }

    #[test]
    fn build_glossary_structured_content_and_style_uses_em_for_numeric_margins() {
        let mut style = JsonObject::new();
        style.insert(u("color"), JsonValue::String(u("red")));
        style.insert(u("marginTop"), JsonValue::Number(10.5));
        style.insert(u("marginLeft"), JsonValue::Number(1.0));
        style.insert(u("marginRight"), JsonValue::Number(2.0));
        style.insert(u("marginBottom"), JsonValue::Number(0.5));
        style.insert(
            u("textDecorationLine"),
            JsonValue::Array(vec![
                JsonValue::String(u("underline")),
                JsonValue::String(u("line-through")),
            ]),
        );

        let mut data = JsonObject::new();
        data.insert(u("termId"), JsonValue::String(u("123")));

        let mut span = JsonObject::new();
        span.insert(u("tag"), JsonValue::String(u("span")));
        span.insert(u("style"), JsonValue::Object(style));
        span.insert(u("data"), JsonValue::Object(data));
        span.insert(u("content"), JsonValue::String(u("styled text")));

        let mut sc = JsonObject::new();
        sc.insert(u("type"), JsonValue::String(u("structured-content")));
        sc.insert(u("content"), JsonValue::Object(span));

        let mock_md5 = |_path: &[u16]| -> Option<[u16; 32]> { None };
        let (glossaries, _) = build_glossary(&[JsonValue::Object(sc)], &u("/base"), mock_md5);

        assert_eq!(glossaries.len(), 1);
        let html = u_str(&glossaries[0]);
        assert!(html.contains("<span"));
        assert!(html.contains("data-sc-term-id=\"123\""));
        assert!(html.contains("color: red;"));
        assert!(html.contains("margin-top: 10.5em;"));
        assert!(html.contains("margin-left: 1em;"));
        assert!(html.contains("margin-right: 2em;"));
        assert!(html.contains("margin-bottom: 0.5em;"));
        assert!(html.contains("text-decoration-line: underline line-through ;"));
        assert!(html.contains(">styled text</span>"));
    }

    #[test]
    fn build_glossary_image_with_file_map() {
        let mut img = JsonObject::new();
        img.insert(u("type"), JsonValue::String(u("image")));
        img.insert(u("path"), JsonValue::String(u("folder/pic.png")));
        img.insert(u("width"), JsonValue::Number(100.0));
        img.insert(u("height"), JsonValue::Number(50.0));
        img.insert(u("collapsible"), JsonValue::Bool(true));
        img.insert(u("collapsed"), JsonValue::Bool(false));
        img.insert(u("imageRendering"), JsonValue::String(u("pixelated")));

        let hex_hash: [u16; 32] = ascii_to_u16("0123456789abcdef0123456789abcdef");
        let mock_md5 = |path: &[u16]| -> Option<[u16; 32]> {
            if u_str(path) == "/base/folder/pic.png" {
                Some(hex_hash)
            } else {
                None
            }
        };

        let (glossaries, files) = build_glossary(&[JsonValue::Object(img)], &u("/base"), mock_md5);
        assert_eq!(glossaries.len(), 1);
        let html = u_str(&glossaries[0]);
        assert!(html.contains("<details open><summary"));
        assert!(html.contains("src=\"0123456789abcdef0123456789abcdef.png\""));
        assert!(html.contains("width=\"100\""));
        assert!(html.contains("height=\"50\""));
        assert!(html.contains("image-rendering: pixelated;"));
        assert_eq!(files.len(), 1);
        assert_eq!(u_str(&files[0].0), "/base/folder/pic.png");
        assert_eq!(u_str(&files[0].1), "0123456789abcdef0123456789abcdef.png");
    }

    #[test]
    fn add_file_rejects_path_traversal_absolute_and_control_chars() {
        let hex_hash: [u16; 32] = ascii_to_u16("0123456789abcdef0123456789abcdef");
        for bad_path in [
            "../secret.png",
            "a/../../secret.png",
            "..\\secret.png",
            "/etc/passwd",
            "\\Windows\\win.ini",
            "C:\\secret.png",
            "C:secret.png",
            "img/bad\0.png",
            "img/bad\n.png",
            "img/",
            ".",
            "",
        ] {
            let called = Cell::new(false);
            let mut img = JsonObject::new();
            img.insert(u("type"), JsonValue::String(u("image")));
            img.insert(u("path"), JsonValue::String(u(bad_path)));

            let (glossaries, files) =
                build_glossary(&[JsonValue::Object(img)], &u("/base"), |_| {
                    called.set(true);
                    Some(hex_hash)
                });
            assert!(!called.get(), "md5_calc should not run for {bad_path:?}");
            assert!(
                files.is_empty(),
                "file_map must stay empty for {bad_path:?}"
            );
            assert!(u_str(&glossaries[0]).contains("src=\"\""));
        }

        // Unsafe extension is dropped cleanly
        let mut img = JsonObject::new();
        img.insert(u("type"), JsonValue::String(u("image")));
        img.insert(u("path"), JsonValue::String(u("img/pic.png\" onerror=\"1")));
        let (glossaries, files) =
            build_glossary(&[JsonValue::Object(img)], &u("/base"), |_| Some(hex_hash));
        assert_eq!(files.len(), 1);
        assert_eq!(u_str(&files[0].1), "0123456789abcdef0123456789abcdef");
        assert!(!u_str(&glossaries[0]).contains("onerror"));
    }

    #[test]
    fn sanitizes_image_rendering_and_caps_structured_recursion() {
        let mut img = JsonObject::new();
        img.insert(u("type"), JsonValue::String(u("image")));
        img.insert(u("path"), JsonValue::String(u("pic.png")));
        img.insert(
            u("imageRendering"),
            JsonValue::String(u("pixelated\" onload=\"alert(1)")),
        );
        let (glossaries, _) = build_glossary(&[JsonValue::Object(img)], &u("/base"), |_| None);
        let html = u_str(&glossaries[0]);
        assert!(!html.contains("onload"));
        assert!(!html.contains("image-rendering:"));

        // Deep nesting beyond MAX_STRUCTURED_CONTENT_DEPTH must not stack overflow.
        let mut node = JsonValue::String(u("deep"));
        for _ in 0..(MAX_STRUCTURED_CONTENT_DEPTH + 32) {
            let mut span = JsonObject::new();
            span.insert(u("tag"), JsonValue::String(u("span")));
            span.insert(u("content"), node);
            node = JsonValue::Object(span);
        }
        let mut sc = JsonObject::new();
        sc.insert(u("type"), JsonValue::String(u("structured-content")));
        sc.insert(u("content"), node);
        let (glossaries, _) = build_glossary(&[JsonValue::Object(sc)], &u("/base"), |_| None);
        let html = u_str(&glossaries[0]);
        assert!(!html.contains("deep"));
        assert_eq!(html.matches("<span>").count(), MAX_STRUCTURED_CONTENT_DEPTH);
        assert_eq!(
            html.matches("</span>").count(),
            MAX_STRUCTURED_CONTENT_DEPTH
        );
    }
}

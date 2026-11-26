//! Anki-compatible HTML glossary building from Yomitan dictionary JSON definitions.

pub(crate) mod escape;
mod media;
mod structured;

use std::collections::BTreeSet;

use serde_json::{Map, Value};

use self::escape::append_escaped;
use self::media::add_image;
use self::structured::add_structured_content;

pub use self::escape::{append_escaped_str, escape_html_str, format_g};
pub use self::structured::{
    is_supported_structured_tag, structured_data_attribute_name, MAX_STRUCTURED_CONTENT_DEPTH,
};

fn add_text(obj: &Map<String, Value>, out: &mut String) {
    let text = obj.get("text").and_then(Value::as_str).unwrap_or("");
    append_escaped(text, out, true);
}

/// Renders Yomitan dictionary definition JSON values into Anki-compatible HTML
/// strings and returns unique `(full_path, hashed_filename)` media entries.
pub fn build_glossary<F>(
    definitions: &[Value],
    basepath: &str,
    mut md5_calc: F,
) -> (Vec<String>, Vec<(String, String)>)
where
    F: FnMut(&str) -> Option<String>,
{
    let basepath_normalized = if basepath.is_empty() {
        String::new()
    } else {
        let mut normalized = String::with_capacity(basepath.len() + 1);
        normalized.push_str(basepath);
        let sep = if cfg!(windows) { '\\' } else { '/' };
        if !normalized.ends_with(['/', '\\']) {
            normalized.push(sep);
        }
        normalized
    };

    let mut glossaries = Vec::new();
    let mut file_map = BTreeSet::new();

    for val in definitions {
        let mut glossary = String::new();
        match val {
            Value::String(s) => {
                append_escaped(s, &mut glossary, true);
            }
            Value::Object(obj) => match obj.get("type").and_then(Value::as_str) {
                Some("structured-content") => {
                    if let Some(content) = obj.get("content") {
                        add_structured_content(
                            content,
                            &basepath_normalized,
                            &mut glossary,
                            &mut file_map,
                            &mut md5_calc,
                            0,
                        );
                    }
                }
                Some("image") => {
                    add_image(
                        obj,
                        &basepath_normalized,
                        &mut glossary,
                        &mut file_map,
                        &mut md5_calc,
                    );
                }
                Some("text") => {
                    add_text(obj, &mut glossary);
                }
                _ => {}
            },
            _ => continue,
        }
        glossaries.push(glossary);
    }

    (glossaries, file_map.into_iter().collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::cell::Cell;

    fn escape_html_lines(input: &str) -> String {
        let mut out = String::with_capacity(input.len());
        append_escaped(input, &mut out, true);
        out
    }

    #[test]
    fn escapes_four_html_metacharacters_but_not_apostrophes() {
        assert_eq!(
            escape_html_str("&<>'\" &amp;"),
            "&amp;&lt;&gt;'&quot; &amp;amp;"
        );
        assert_eq!(escape_html_str(""), "");
    }

    #[test]
    fn literal_lf_becomes_br_after_escaping_without_double_escape() {
        assert_eq!(
            escape_html_lines("<a>\r\nB\n\u{2028}\u{2029}"),
            "&lt;a&gt;\r<br>B<br>\u{2028}\u{2029}"
        );
        assert_eq!(escape_html_lines("<br>"), "&lt;br&gt;");
    }

    #[test]
    fn preserves_supplementary_characters() {
        assert_eq!(escape_html_str("😀&"), "😀&amp;");
        assert_eq!(escape_html_lines("😀&"), "😀&amp;");
    }

    #[test]
    fn tag_whitelist_is_case_sensitive_and_exact() {
        for tag in [
            "br", "ruby", "rt", "rp", "table", "thead", "tbody", "tfoot", "tr", "td", "th", "span",
            "div", "ol", "ul", "li", "details", "summary", "img", "a",
        ] {
            assert!(is_supported_structured_tag(tag), "{tag}");
        }
        for tag in ["BR", "script", "video", "a ", "", "😀"] {
            assert!(!is_supported_structured_tag(tag), "{tag}");
        }
    }

    #[test]
    fn structured_data_attribute_name_uses_unicode_lowercase() {
        assert_eq!(structured_data_attribute_name("reading"), "data-sc-reading");
        assert_eq!(
            structured_data_attribute_name("exampleSentence"),
            "data-sc-example-sentence"
        );
        assert_eq!(
            structured_data_attribute_name("fooBarBaz"),
            "data-sc-foo-bar-baz"
        );
        assert_eq!(structured_data_attribute_name("ÄÖÜ"), "data-sc--ä-ö-ü");
        assert_eq!(
            structured_data_attribute_name("\u{10400}"),
            "data-sc--\u{10428}"
        );
    }

    #[test]
    fn double_formatting_compact_general() {
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
        let defs = vec![
            json!("<b>def1</b>\nline2"),
            json!({ "type": "text", "text": "hello & world" }),
            json!({ "type": "unknown" }),
            Value::Null,
            json!(true),
            json!(42.0),
            json!([]),
        ];

        let (glossaries, files) = build_glossary(&defs, "/base", |_| None);
        assert_eq!(
            glossaries,
            vec![
                "&lt;b&gt;def1&lt;/b&gt;<br>line2",
                "hello &amp; world",
                "",
            ]
        );
        assert!(files.is_empty());
    }

    #[test]
    fn build_glossary_structured_content_and_style_uses_em_for_numeric_margins() {
        let sc = json!({
            "type": "structured-content",
            "content": {
                "tag": "span",
                "style": {
                    "color": "red",
                    "marginTop": 10.5,
                    "marginLeft": 1.0,
                    "marginRight": 2.0,
                    "marginBottom": 0.5,
                    "textDecorationLine": ["underline", "line-through"]
                },
                "data": { "termId": "123" },
                "content": "styled text"
            }
        });

        let (glossaries, _) = build_glossary(&[sc], "/base", |_| None);
        assert_eq!(glossaries.len(), 1);
        let html = &glossaries[0];
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
        let img = json!({
            "type": "image",
            "path": "folder/pic.png",
            "width": 100.0,
            "height": 50.0,
            "collapsible": true,
            "collapsed": false,
            "imageRendering": "pixelated"
        });

        let expected_path = if cfg!(windows) {
            "/base\\folder\\pic.png"
        } else {
            "/base/folder/pic.png"
        };
        let hex_hash = "0123456789abcdef0123456789abcdef";
        let (glossaries, files) = build_glossary(&[img], "/base", |path| {
            (path == expected_path).then(|| hex_hash.to_owned())
        });
        assert_eq!(glossaries.len(), 1);
        let html = &glossaries[0];
        assert!(html.contains("<details open><summary"));
        assert!(html.contains("src=\"0123456789abcdef0123456789abcdef.png\""));
        assert!(html.contains("width=\"100\""));
        assert!(html.contains("height=\"50\""));
        assert!(html.contains("image-rendering: pixelated;"));
        assert_eq!(
            files,
            vec![(
                expected_path.to_owned(),
                "0123456789abcdef0123456789abcdef.png".to_owned()
            )]
        );
    }

    #[test]
    fn add_file_rejects_path_traversal_absolute_and_control_chars() {
        let hex_hash = "0123456789abcdef0123456789abcdef";
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
            let img = json!({ "type": "image", "path": bad_path });
            let (glossaries, files) = build_glossary(&[img], "/base", |_| {
                called.set(true);
                Some(hex_hash.to_owned())
            });
            assert!(!called.get(), "md5_calc should not run for {bad_path:?}");
            assert!(
                files.is_empty(),
                "file_map must stay empty for {bad_path:?}"
            );
            assert!(glossaries[0].contains("src=\"\""));
        }

        let img = json!({ "type": "image", "path": "img/pic.png\" onerror=\"1" });
        let (glossaries, files) =
            build_glossary(&[img], "/base", |_| Some(hex_hash.to_owned()));
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].1, "0123456789abcdef0123456789abcdef");
        assert!(!glossaries[0].contains("onerror"));
    }

    #[test]
    fn sanitizes_image_rendering_and_caps_structured_recursion() {
        let img = json!({
            "type": "image",
            "path": "pic.png",
            "imageRendering": "pixelated\" onload=\"alert(1)"
        });
        let (glossaries, _) = build_glossary(&[img], "/base", |_| None);
        assert!(!glossaries[0].contains("onload"));
        assert!(!glossaries[0].contains("image-rendering:"));

        let mut node = json!("deep");
        for _ in 0..(MAX_STRUCTURED_CONTENT_DEPTH + 32) {
            node = json!({ "tag": "span", "content": node });
        }
        let sc = json!({ "type": "structured-content", "content": node });
        let (glossaries, _) = build_glossary(&[sc], "/base", |_| None);
        assert!(!glossaries[0].contains("deep"));
        assert_eq!(
            glossaries[0].matches("<span>").count(),
            MAX_STRUCTURED_CONTENT_DEPTH
        );
        assert_eq!(
            glossaries[0].matches("</span>").count(),
            MAX_STRUCTURED_CONTENT_DEPTH
        );
    }
}

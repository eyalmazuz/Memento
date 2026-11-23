//! Integration tests for AnkiController and ConfigController in subomi-app.

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use subomi_app::controller::anki::{
    apply_subtitle_cloze, build_entry_find_query, build_note_payload_from_profile,
    create_cloze_sentence, default_note_fields, escape_anki_search_term, filter_subtitle_text,
    format_cloze, format_cloze_with_hint, format_expression_furigana, format_glossary,
    format_nid_browse_query, map_note_fields, merge_note_update, normalize_anki_browse_target,
    populate_note_context_from_sqlite, scope_dictionary_css, secondary_context_for_range,
    split_cloze, unavailable_profile_markers, validate_json_payload, validate_note_payload,
    AnkiController, AnkiNoteContext, AnkiUpdateMode, EntryAnkiStatus, GlossaryDefinition,
    NoteFieldsContext, NotePayload,
};
use subomi_app::controller::config::{
    anki_note_preset_fields, AnkiNoteConfig, AnkiNotePreset, ConfigController,
};
use subomi_config::{ArgbColor, FontSpec, Value};
use subomi_net::anki::AnkiEndpoint;

fn temp_test_dir(sub: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("subomi_app_test_{}_{}", sub, std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("failed to create temp test directory");
    dir
}

/// Read a complete HTTP/1.1 request (headers + `Content-Length` body) from a TCP stream
/// and parse its JSON body. Handles clients such as `ureq 3` that write headers and body
/// in separate syscalls.
fn read_http_json_body(stream: &mut std::net::TcpStream) -> Option<serde_json::Value> {
    use std::io::Read;
    let mut raw = Vec::new();
    let mut chunk = [0u8; 4096];
    let header_end = loop {
        if let Some(pos) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos + 4;
        }
        let n = stream.read(&mut chunk).ok()?;
        if n == 0 {
            return None;
        }
        raw.extend_from_slice(&chunk[..n]);
    };
    let headers = String::from_utf8_lossy(&raw[..header_end]);
    let content_length = headers
        .lines()
        .find_map(|line| {
            let (name, val) = line.split_once(':')?;
            if name.trim().eq_ignore_ascii_case("content-length") {
                val.trim().parse::<usize>().ok()
            } else {
                None
            }
        })
        .unwrap_or(0);
    while raw.len() < header_end + content_length {
        let n = stream.read(&mut chunk).ok()?;
        if n == 0 {
            break;
        }
        raw.extend_from_slice(&chunk[..n]);
    }
    let body_bytes = &raw[header_end..raw.len().min(header_end + content_length)];
    serde_json::from_slice(body_bytes).ok()
}

// ═══════════════════════════════════════════════════════════════════════════
// ConfigController Tests
// ═══════════════════════════════════════════════════════════════════════════


#[test]
fn test_config_recent_files_cap_and_dedup() {
    let ctrl = ConfigController::new();
    assert!(ctrl.recent_files().is_empty());

    // Add 12 distinct files (cap is 10)
    for i in 1..=12 {
        let file = format!("/path/to/media_{:02}.mkv", i);
        let changed = ctrl.recent_files_add(&file).unwrap();
        assert!(changed);
    }

    let files = ctrl.recent_files();
    assert_eq!(files.len(), 10, "recent files must be capped at 10");

    // Latest file should be at index 0
    assert_eq!(files[0], "/path/to/media_12.mkv");
    assert_eq!(files[1], "/path/to/media_11.mkv");
    assert_eq!(files[9], "/path/to/media_03.mkv");

    // Add duplicate of an existing file (media_05.mkv)
    let changed = ctrl.recent_files_add("/path/to/media_05.mkv").unwrap();
    assert!(changed);

    let files_after_dedup = ctrl.recent_files();
    assert_eq!(files_after_dedup.len(), 10);
    assert_eq!(files_after_dedup[0], "/path/to/media_05.mkv");
    assert_eq!(files_after_dedup[1], "/path/to/media_12.mkv");

    // Clear recent files
    let cleared = ctrl.clear_recent_files().unwrap();
    assert!(cleared);
    assert!(ctrl.recent_files().is_empty());

    // Second clear is no-op
    assert!(!ctrl.clear_recent_files().unwrap());
}

#[test]
fn test_recent_files_round_trip_preserves_commas_and_order() {
    let temp_dir = temp_test_dir("save_load");
    let file_name = "subomi.conf";

    let ctrl = ConfigController::new();
    ctrl.recent_files_add("/path/to/video_a.mp4").unwrap();
    ctrl.recent_files_add("/path/to/video, part 2.mp4").unwrap();

    // Save configuration
    ctrl.save(&temp_dir, file_name).expect("save should succeed");
    assert!(temp_dir.join(file_name).exists());

    // Load in a fresh controller instance
    let loaded = ConfigController::load(&temp_dir, file_name).expect("load should succeed");
    // A filename containing comma-space is one path, not two CSV entries.

    let files = loaded.recent_files();
    assert_eq!(files.len(), 2);
    assert_eq!(files[0], "/path/to/video, part 2.mp4");
    assert_eq!(files[1], "/path/to/video_a.mp4");

    let _ = fs::remove_dir_all(&temp_dir);
}

// ═══════════════════════════════════════════════════════════════════════════
// AnkiController Tests
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_anki_controller_defaults_and_endpoint() {
    let ctrl = AnkiController::default();
    assert_eq!(ctrl.endpoint().hostname, "http://localhost");
    assert_eq!(ctrl.endpoint().port, 8765);

    let custom_ep = AnkiEndpoint {
        hostname: "http://10.0.0.5".to_string(),
        port: 8766,
        api_key: Some("key-abc".to_string()),
        timeout: None,
        use_api_key: true,
    };
    let custom_ctrl = AnkiController::new(custom_ep);
    assert_eq!(custom_ctrl.endpoint().hostname, "http://10.0.0.5");
    assert_eq!(custom_ctrl.endpoint().port, 8766);
    assert_eq!(custom_ctrl.endpoint().api_key.as_deref(), Some("key-abc"));
}

#[test]
fn profile_note_uses_selection_and_context_with_empty_unavailable_fields() {
    let mut fields = HashMap::new();
    fields.insert("Expression".into(), "{expression}".into());
    fields.insert("Selection".into(), "{selection}".into());
    fields.insert("Sentence".into(), "{context}".into());
    fields.insert("Glossary".into(), "{glossary : dict = 9}".into());
    fields.insert("Audio".into(), "{audio}".into());
    let profile = AnkiNoteConfig {
        enabled: true,
        endpoint: AnkiEndpoint::default(),
        deck: "Japanese".into(),
        model: "Vocabulary".into(),
        field_templates: fields,
        tags: vec!["subomi".into()],
        duplicate_policy: 1,
        screenshot_type: 0,
        audio_pad_start: 0.0,
        audio_pad_end: 0.0,
        audio_normalize: false,
        audio_db: -20.0,
        newline_replacer: "<br>".into(),
    };
    let mut ctx = NoteFieldsContext::new("食べる", "たべる", "パンを食べました。");
    ctx.selection = "食べました".into();
    ctx.context = "朝に会いました。\nパンを食べました。".into();
    ctx.glossary_items = vec!["to eat".into()];
    let payload = build_note_payload_from_profile(&profile, &ctx).unwrap();
    assert_eq!(payload.fields["Selection"], "食べました");
    assert_eq!(payload.fields["Sentence"], "朝に会いました。<br>パンを食べました。");
    assert!(payload.fields["Glossary"].contains("to eat"));
    assert_eq!(payload.fields["Audio"], "");
    assert!(unavailable_profile_markers(&profile).is_empty());
}

#[test]
fn profile_note_includes_every_selected_dictionary_and_media_attachment() {
    let mut fields = HashMap::new();
    fields.insert("Expression".into(), "{expression}".into());
    fields.insert("Definition".into(), "{glossary}".into());
    fields.insert("WordAudio".into(), "{audio}".into());
    fields.insert("Picture".into(), "{screenshot}".into());
    fields.insert("Video".into(), "{video}".into());
    let profile = AnkiNoteConfig {
        enabled: true, endpoint: AnkiEndpoint::default(), deck: "Japanese".into(),
        model: "Vocabulary".into(), field_templates: fields, tags: vec![],
        duplicate_policy: 1, screenshot_type: 0, audio_pad_start: 0.0,
        audio_pad_end: 0.0, audio_normalize: false, audio_db: -20.0,
        newline_replacer: "<br>".into(),
    };
    let mut ctx = NoteFieldsContext::new("迎えに行く", "むかえにいく", "港まで迎えに行く");
    ctx.glossary_definitions = vec![
        GlossaryDefinition { dictionary_name: "実用日本語表現辞典".into(), glossary: "相手を迎えるために出かける".into(), tags: vec![], example_ja: "港へ迎えに行く".into(), ..Default::default() },
        GlossaryDefinition { dictionary_name: "明鏡国語辞典".into(), glossary: "迎えに出向く".into(), tags: vec![], ..Default::default() },
    ];
    ctx.audio_attachments.push(serde_json::json!({"url": "https://example.test/word.mp3", "filename": "word.mp3", "fields": ["WordAudio"]}));
    ctx.picture_attachments.push(serde_json::json!({"data": "AAAA", "filename": "still.png", "fields": ["Picture"]}));
    ctx.video_attachments.push(serde_json::json!({"data": "AAAA", "filename": "clip.mp4", "fields": ["Video"]}));
    let payload = build_note_payload_from_profile(&profile, &ctx).unwrap();
    let definition = &payload.fields["Definition"];
    for text in ["実用日本語表現辞典", "相手を迎えるために出かける", "港へ迎えに行く", "明鏡国語辞典", "迎えに出向く"] {
        assert!(definition.contains(text), "missing {text}: {definition}");
    }
    let wire = payload.to_value();
    assert_eq!(wire["audio"][0]["fields"][0], "WordAudio");
    assert_eq!(wire["picture"][0]["fields"][0], "Picture");
    assert_eq!(wire["video"][0]["fields"][0], "Video");
}

#[test]
fn filtered_primary_glossary_keeps_other_dictionaries_available() {
    let mut ctx = NoteFieldsContext::new("去年", "きょねん", "去年の秋に会った。");
    ctx.glossary_definitions = vec![
        GlossaryDefinition { dictionary_id: 9, dictionary_name: "JMdict".into(), glossary: "last year".into(), ..Default::default() },
        GlossaryDefinition { dictionary_id: 12, dictionary_name: "Digital".into(), glossary: "前年".into(), ..Default::default() },
    ];
    let templates = HashMap::from([
        ("MainDefinition".into(), "{glossary : dict = 9}".into()),
        ("Glossary".into(), "{glossary}".into()),
    ]);
    let fields = map_note_fields(&templates, &ctx);
    assert!(fields["MainDefinition"].contains("last year"));
    assert!(!fields["MainDefinition"].contains("前年"));
    assert!(fields["Glossary"].contains("last year"));
    assert!(fields["Glossary"].contains("前年"));
}

#[test]
fn lapis_pitch_and_frequency_fields_use_dictionary_metadata() {
    let mut ctx = NoteFieldsContext::new("食べる", "たべる", "パンを食べる。");
    ctx.pitch_positions = "<span class=\"subomi-pitch-position\">[2]</span>".into();
    ctx.pitch_categories = "kifuku".into();
    ctx.frequency_harmonic_rank = "144".into();
    let fields = HashMap::from([
        ("PitchPosition".into(), "{pitch-positions}".into()),
        ("PitchCategories".into(), "{pitch-categories}".into()),
        ("FreqSort".into(), "{frequency-harmonic-rank}".into()),
    ]);
    let mapped = map_note_fields(&fields, &ctx);
    assert_eq!(mapped["PitchPosition"], ctx.pitch_positions);
    assert_eq!(mapped["PitchCategories"], "kifuku");
    assert_eq!(mapped["FreqSort"], "144");
}

#[test]
fn profile_excluded_glossaries_are_available_for_each_popup() {
    let ctrl = ConfigController::new();
    ctrl.edit_setting("anki/profile/ex-glos", "Dictionary A, Dictionary B").unwrap();
    assert_eq!(ctrl.anki_excluded_glossaries(), ["Dictionary A", "Dictionary B"]);
}

#[test]
fn model_field_loading_maps_common_fields_and_preserves_edits() {
    let ctrl = ConfigController::new();
    ctrl.edit_setting("anki/profile/term/model", "Vocabulary").unwrap();
    ctrl.set_anki_model_fields("term", &[
        "Expression".into(), "Definition".into(), "SentenceAudio".into(),
        "Picture".into(), "ContextAudio".into(), "Text Selection 1/2".into(), "Custom".into(),
    ]).unwrap();
    let note = ctrl.anki_profile_note_config().unwrap();
    assert_eq!(note.field_templates["Definition"], "{glossary}");
    assert_eq!(note.field_templates["SentenceAudio"], "{audio-media}");
    assert_eq!(note.field_templates["Picture"], "{screenshot}");
    assert_eq!(note.field_templates["ContextAudio"], "{audio-context}");
    assert_eq!(note.field_templates["Text Selection 1/2"], "{selection}");
    assert_eq!(note.field_templates["Custom"], "");
    ctrl.edit_setting("anki/profile/term/field/Definition", "{glossary-brief}").unwrap();
    ctrl.set_anki_model_fields("term", &["Definition".into(), "Audio".into()]).unwrap();
    let note = ctrl.anki_profile_note_config().unwrap();
    assert_eq!(note.field_templates["Definition"], "{glossary-brief}");
    assert_eq!(note.field_templates["Audio"], "{audio}");
    assert!(!note.field_templates.contains_key("Picture"));
    ctrl.edit_setting("anki/profile/kanji/deck", "Kanji").unwrap();
    ctrl.edit_setting("anki/profile/kanji/model", "Kanji Card").unwrap();
    ctrl.set_anki_model_fields("kanji", &["Character".into(), "Onyomi".into(), "Meaning".into()]).unwrap();
    let kanji = ctrl.anki_kanji_note_config().unwrap();
    assert_eq!(kanji.deck, "Kanji");
    assert_eq!(kanji.field_templates["Character"], "{character}");
    assert_eq!(kanji.field_templates["Onyomi"], "{onyomi}");
    assert_eq!(kanji.field_templates["Meaning"], "{glossary}");
}

#[test]
fn test_anki_cloze_formatting() {
    // Basic cloze
    let c1 = format_cloze("毎朝パンを", "食べる", "。", 1);
    assert_eq!(c1, "毎朝パンを{{c1::食べる}}。");

    // Cloze index 2
    let c2 = format_cloze("彼らは", "走って", "行った。", 2);
    assert_eq!(c2, "彼らは{{c2::走って}}行った。");

    // Index 0 defaults to 1
    let c0 = format_cloze("前", "中", "後", 0);
    assert_eq!(c0, "前{{c1::中}}後");

    // Cloze with hint
    let ch = format_cloze_with_hint("これは", "問題", "です", "hint", 1);
    assert_eq!(ch, "これは{{c1::問題::hint}}です");

    // Split cloze
    let (prefix, body, suffix) = split_cloze("春の訪れを感じる。", "訪れ");
    assert_eq!(prefix, "春の");
    assert_eq!(body, "訪れ");
    assert_eq!(suffix, "を感じる。");

    // Create cloze sentence directly
    let auto_cloze = create_cloze_sentence("春の訪れを感じる。", "訪れ", 1);
    assert_eq!(auto_cloze, "春の{{c1::訪れ}}を感じる。");

    // Target not found wraps full sentence
    let fallback = create_cloze_sentence("こんにちは世界", "さようなら", 1);
    assert_eq!(fallback, "{{c1::こんにちは世界}}");
}

#[test]
fn test_anki_glossary_formatting() {
    let items = ["to eat", "to consume", "to live on"];
    let out = format_glossary(&items, "JMdict", &["v1", "trans"]);

    // Matches Subomi glossary open & close HTML structure
    assert!(out.glossary.starts_with("<div class=\"yomitan-glossary subomi-glossary\"><ol>"));
    assert!(out.glossary.contains("<li data-dictionary=\"JMdict\">"));
    assert!(out.glossary.contains("v1, trans, JMdict"));
    assert!(out.glossary.contains("to eat"));
    assert!(out.glossary.contains("to consume"));
    assert!(out.glossary.contains("to live on"));
    assert!(out.glossary.ends_with(".subomi-glossary td {border-style: solid;padding: 0.25em;vertical-align: top;border-width: 1px;border-color: currentColor;}</style></ol></div>"));

    // Brief glossary formatting
    assert!(out.glossary_brief.contains("<li>to eat</li>"));
    assert!(out.glossary_brief.contains("<li>to consume</li>"));

    // Compact glossary formatting
    assert!(out.glossary_compact.contains("to eat | to consume | to live on"));
}

#[test]
fn test_anki_note_payload_validation() {
    let mut valid_payload = NotePayload::new("Japanese", "Basic");
    valid_payload.fields.insert("Front".to_string(), "犬".to_string());
    valid_payload.fields.insert("Back".to_string(), "dog".to_string());
    assert!(validate_note_payload(&valid_payload).is_ok());

    // Empty deck
    let mut empty_deck = valid_payload.clone();
    empty_deck.deck_name = "".to_string();
    assert!(validate_note_payload(&empty_deck).is_err());

    // Empty model
    let mut empty_model = valid_payload.clone();
    empty_model.model_name = "".to_string();
    assert!(validate_note_payload(&empty_model).is_err());

    // Empty fields map
    let mut empty_fields = valid_payload.clone();
    empty_fields.fields.clear();
    assert!(validate_note_payload(&empty_fields).is_err());

    // All empty field values
    let mut blank_fields = valid_payload.clone();
    blank_fields.fields.insert("Front".to_string(), "   ".to_string());
    blank_fields.fields.insert("Back".to_string(), "".to_string());
    assert!(validate_note_payload(&blank_fields).is_err());

    // Validate raw JSON string (including video-only attachments)
    assert!(validate_json_payload(r#"{"deckName":"D","modelName":"M","fields":{"F":"V"}}"#).is_ok());
    assert!(validate_json_payload(
        r#"{"deckName":"D","modelName":"M","fields":{"Video":""},"video":[{"filename":"clip.mp4","data":"AAAA","fields":["Video"]}]}"#
    )
    .is_ok());
    assert!(validate_json_payload(
        r#"{"deckName":"D","modelName":"M","fields":{"Video":""},"video":[]}"#
    )
    .is_err());
    assert!(validate_json_payload(r#"{"modelName":"M","fields":{"F":"V"}}"#).is_err());
    assert!(validate_json_payload(r#"{"deckName":"D","fields":{"F":"V"}}"#).is_err());
    assert!(validate_json_payload(r#"{"deckName":"D","modelName":"M"}"#).is_err());
    assert!(validate_json_payload("not-json").is_err());
}

#[test]
fn test_anki_note_field_mapping_and_payload_generation() {
    let ctrl = AnkiController::default();

    let ctx = NoteFieldsContext::new("食べる", "たべる", "毎朝パンを食べる。")
        .with_glossary_item("to eat")
        .with_glossary_item("to consume")
        .with_dictionary("JMdict")
        .with_tag("JLPT-N5")
        .with_cloze_index(1);

    // Verify context split and clean empty selection default
    assert_eq!(ctx.expression, "食べる");
    assert_eq!(ctx.reading, "たべる");
    assert_eq!(ctx.selection, "");
    assert_eq!(ctx.cloze_prefix, "毎朝パンを");
    assert_eq!(ctx.cloze_body, "食べる");
    assert_eq!(ctx.cloze_suffix, "。");

    // Default fields generation
    let defaults = default_note_fields(&ctx);
    assert_eq!(defaults.get("Expression").unwrap(), "食べる");
    assert_eq!(defaults.get("Reading").unwrap(), "たべる");
    assert_eq!(defaults.get("Sentence").unwrap(), "毎朝パンを{{c1::食べる}}。");
    assert!(defaults.get("Glossary").unwrap().contains("to eat"));
    assert_eq!(defaults.get("ClozePrefix").unwrap(), "毎朝パンを");
    assert_eq!(defaults.get("ClozeBody").unwrap(), "食べる");
    assert_eq!(defaults.get("ClozeSuffix").unwrap(), "。");

    // Custom field templates mapping
    let mut templates = HashMap::new();
    templates.insert("Front".to_string(), "{expression}（{reading}）".to_string());
    templates.insert("Back".to_string(), "{glossary}".to_string());
    templates.insert("Reading".to_string(), "{reading}".to_string());
    templates.insert("Expression".to_string(), "{expression}".to_string());
    templates.insert("Sentence".to_string(), "{sentence}".to_string());
    templates.insert("Tags".to_string(), "{tags}".to_string());

    let mapped = map_note_fields(&templates, &ctx);
    assert_eq!(mapped.get("Front").unwrap(), "食べる（たべる）");
    assert_eq!(mapped.get("Reading").unwrap(), "たべる");
    assert_eq!(mapped.get("Expression").unwrap(), "食べる");
    assert_eq!(mapped.get("Sentence").unwrap(), "毎朝パンを食べる。");
    assert_eq!(mapped.get("Tags").unwrap(), "<ul><li>JLPT-N5</li></ul>");
    assert!(mapped.get("Back").unwrap().contains("JMdict"));

    // Payload generation via controller
    let payload = ctrl.create_note_payload("Vocabulary", "JapaneseCloze", &ctx);
    assert_eq!(payload.deck_name, "Vocabulary");
    assert_eq!(payload.model_name, "JapaneseCloze");
    assert_eq!(payload.tags, vec!["JLPT-N5".to_string()]);
    assert!(ctrl.validate_note_payload(&payload).is_ok());

    // Payload JSON serialization
    let json_str = payload.to_json_string();
    assert!(json_str.contains("\"deckName\":\"Vocabulary\""));
    assert!(json_str.contains("\"modelName\":\"JapaneseCloze\""));
    assert!(json_str.contains("\"fields\":{"));
    assert!(json_str.contains("\"tags\":[\"JLPT-N5\"]"));

    // Payload metadata integration with subomi-anki
    let metadata = payload.to_anki_metadata();
    assert_eq!(metadata.len(), 5); // deckName, modelName, tags, options, fields
}

#[test]
fn selection_stays_empty_and_falls_through_clipboard_to_glossary() {
    let profile = AnkiNoteConfig {
        enabled: true,
        endpoint: AnkiEndpoint::default(),
        deck: "Japanese".into(),
        model: "Lapis".into(),
        field_templates: HashMap::from([
            ("Expression".into(), "{expression}".into()),
            (
                "MainDef".into(),
                "{selection | clipboard | glossary}".into(),
            ),
        ]),
        tags: vec![],
        duplicate_policy: 1,
        screenshot_type: 0,
        audio_pad_start: 0.0,
        audio_pad_end: 0.0,
        audio_normalize: false,
        audio_db: -20.0,
        newline_replacer: "<br>".into(),
    };

    // 1. Default context has empty selection; empty clipboard falls through to glossary (#262).
    let mut ctx = NoteFieldsContext::new("食べる", "たべる", "パンを食べました。")
        .with_dictionary("JMdict")
        .with_glossary_item("to eat");
    ctx.clipboard = Some(String::new());
    apply_subtitle_cloze(&mut ctx, &[], None, "食べました", Some(9));
    assert_eq!(ctx.selection, "", "scanned subtitle word must never taint selection");
    assert_eq!(ctx.cloze_prefix, "パンを");
    assert_eq!(ctx.cloze_body, "食べました");
    assert_eq!(ctx.cloze_suffix, "。");

    let p1 = build_note_payload_from_profile(&profile, &ctx).unwrap();
    assert!(p1.fields["MainDef"].contains("to eat"));

    // 2. Non-empty clipboard wins when selection is empty.
    ctx.clipboard = Some("clipboard line 1\nline 2".into());
    let p2 = build_note_payload_from_profile(&profile, &ctx).unwrap();
    assert_eq!(p2.fields["MainDef"], "clipboard line 1<br>line 2");

    // 3. Explicit glossary selection wins over clipboard and glossary.
    ctx.selection = "selected glossary text".into();
    let p3 = build_note_payload_from_profile(&profile, &ctx).unwrap();
    assert_eq!(p3.fields["MainDef"], "selected glossary text");
}

#[test]
fn multi_subtitle_cloze_and_secondary_context_match_active_cue() {
    use subomi_app::controller::subtitles::create_cue;

    // Three marked cues, each containing "食べました"; active cue is index 1 (#173).
    let cues = vec![
        "昨日もパンを食べました。".to_owned(),
        "今日もパンを食べました。".to_owned(),
        "明日もパンを食べました。".to_owned(),
    ];
    let mut ctx = NoteFieldsContext::new("食べる", "たべる", "今日もパンを食べました。");
    apply_subtitle_cloze(&mut ctx, &cues, Some(1), "食べました", None);

    assert_eq!(ctx.selection, "");
    assert_eq!(ctx.context, cues.join("\n"));
    assert_eq!(ctx.cloze_prefix, "昨日もパンを食べました。\n今日もパンを");
    assert_eq!(ctx.cloze_body, "食べました");
    assert_eq!(ctx.cloze_suffix, "。\n明日もパンを食べました。");

    // Secondary context overlaps `[1.0, 5.0]` and excludes non-overlapping cues.
    let secondary = vec![
        create_cue("Too early", 0.0, 0.8),
        create_cue("First EN line", 1.2, 2.5),
        create_cue("Second EN line", 3.0, 4.8),
        create_cue("Too late", 5.2, 6.0),
    ];
    ctx.secondary_subtitle = "Second EN line".into();
    ctx.secondary_context =
        secondary_context_for_range(&secondary, 1.0, 5.0, &ctx.secondary_subtitle);
    assert_eq!(ctx.secondary_context, "First EN line\nSecond EN line");

    let profile = AnkiNoteConfig {
        enabled: true,
        endpoint: AnkiEndpoint::default(),
        deck: "Japanese".into(),
        model: "Sentence".into(),
        field_templates: HashMap::from([
            ("Cloze".into(), "{cloze-prefix}[{cloze-body}]{cloze-suffix}".into()),
            ("Sub2".into(), "{subtitle-2}".into()),
            ("Ctx2".into(), "{context-2}".into()),
        ]),
        tags: vec![],
        duplicate_policy: 1,
        screenshot_type: 0,
        audio_pad_start: 0.0,
        audio_pad_end: 0.0,
        audio_normalize: false,
        audio_db: -20.0,
        newline_replacer: "<br>".into(),
    };
    let payload = build_note_payload_from_profile(&profile, &ctx).unwrap();
    assert_eq!(
        payload.fields["Cloze"],
        "昨日もパンを食べました。<br>今日もパンを[食べました]。<br>明日もパンを食べました。"
    );
    assert_eq!(payload.fields["Sub2"], "Second EN line");
    assert_eq!(payload.fields["Ctx2"], "First EN line<br>Second EN line");

    // Fallback when no secondary cue overlaps
    assert_eq!(
        secondary_context_for_range(&secondary, 10.0, 12.0, "Fallback EN"),
        "Fallback EN"
    );
}

#[test]
fn furigana_simplifies_okurigana_and_leaves_kana_plain() {
    assert_eq!(
        format_expression_furigana("食べる", "たべる"),
        ("<ruby>食<rt>た</rt></ruby>べる".into(), "食[た]べる".into())
    );
    assert_eq!(
        format_expression_furigana("食べ物", "たべもの"),
        (
            "<ruby>食<rt>た</rt></ruby>べ<ruby>物<rt>もの</rt></ruby>".into(),
            "食[た]べ物[もの]".into()
        )
    );
    assert_eq!(
        format_expression_furigana("テレビ", "てれび"),
        ("テレビ".into(), "テレビ".into())
    );
    assert_eq!(
        format_expression_furigana("やはり", ""),
        ("やはり".into(), "やはり".into())
    );
    assert_eq!(
        format_expression_furigana("は", "わ"),
        ("は".into(), "は".into())
    );
    assert_eq!(
        format_expression_furigana("ヴァイオリン", "ばいおりん"),
        ("ヴァイオリン".into(), "ヴァイオリン".into())
    );
}

#[test]
fn invalid_glossary_dict_id_emits_error_string() {
    let mut ctx = NoteFieldsContext::new("食べる", "たべる", "パンを食べる。");
    ctx.glossary_definitions = vec![GlossaryDefinition {
        dictionary_id: 9,
        dictionary_name: "JMdict".into(),
        glossary: "to eat".into(),
        ..Default::default()
    }];
    let templates = HashMap::from([
        ("BadFull".into(), "{glossary:dict=not-a-number}".into()),
        ("BadBrief".into(), "{glossary-brief:dict=abc}".into()),
        ("BadCompact".into(), "{glossary-compact:dict=}".into()),
        ("Valid".into(), "{glossary:dict=9}".into()),
    ]);
    let mapped = map_note_fields(&templates, &ctx);
    assert_eq!(mapped["BadFull"], "ERROR: Invalid dic-id value");
    assert_eq!(mapped["BadBrief"], "ERROR: Invalid dic-id value");
    assert_eq!(mapped["BadCompact"], "ERROR: Invalid dic-id value");
    assert!(mapped["Valid"].contains("to eat"));
}

#[test]
fn sqlite_enrichment_wires_glossary_scoped_css_media_upload_pitch_tags_and_frequencies() {
    use subomi_dict::yomidbbuilder::{dictionary_resource_dir, prepare_db};
    use std::io::Write;
    use std::net::TcpListener;

    let temp_dir = temp_test_dir("sqlite_anki_full");
    let db_path = temp_dir.join("dictionaries.sqlite");
    let config_dict_res = temp_dir.join("config_dict_resources");
    fs::create_dir_all(&config_dict_res).unwrap();
    prepare_db(&db_path).unwrap();

    // Populate SQLite directly using read-write connection
    let conn = subomi_dict::sqlite_db::open_read_write_connection(&db_path).unwrap();
    conn.execute(
        "INSERT INTO directory (dic_id, title, format, revision, sequenced) VALUES (7, 'Jitendex', 3, '1', 1);",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO directory (dic_id, title, format, revision, sequenced) VALUES (8, 'FreqDict2', 3, '1', 0);",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO tag_bank (dic_id, name, category, ord, notes, score) VALUES (7, 'v1', 'partOfSpeech', 1, 'Ichidan verb', 50);",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO tag_bank (dic_id, name, category, ord, notes, score) VALUES (7, 'P', 'popular', 2, 'Common word', 10);",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO tag_bank (dic_id, name, category, ord, notes, score) VALUES (7, 'strokes', 'misc', 1, 'Stroke count', 0);",
        [],
    )
    .unwrap();

    let structured_glossary = serde_json::json!([
        {
            "type": "structured-content",
            "content": {
                "tag": "div",
                "data": {"content": "glossary"},
                "content": [
                    "to eat",
                    {"tag": "img", "path": "img/diagram.png", "width": 32, "height": 32, "collapsible": false}
                ]
            }
        }
    ])
    .to_string();

    conn.execute(
        "INSERT INTO term_bank (dic_id, expression, reading, def_tags, rules, score, glossary, sequence, term_tags) \
         VALUES (7, '食べる', 'たべる', 'v1', 'v1', 100, ?1, 1, 'P');",
        [&structured_glossary],
    )
    .unwrap();

    // Pitch position 2 on 3-mora "たべる" with rule "v1" -> kifuku!
    let pitch_blob = r#"{"reading":"たべる","pitches":[{"position":2}]}"#.as_bytes();
    conn.execute(
        "INSERT INTO term_meta_bank (dic_id, expression, mode, type, data) VALUES (7, '食べる', 'pitch', 4, ?1);",
        [pitch_blob],
    )
    .unwrap();

    // Two frequency dictionaries: 100 and 300 -> harmonic = floor(2 / (1/100 + 1/300)) = 150, average = 200
    let freq1 = r#"{"reading":"たべる","frequency":100}"#.as_bytes();
    let freq2 = r#"{"reading":"たべる","frequency":300}"#.as_bytes();
    conn.execute(
        "INSERT INTO term_meta_bank (dic_id, expression, mode, type, data) VALUES (7, '食べる', 'freq', 4, ?1);",
        [freq1],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO term_meta_bank (dic_id, expression, mode, type, data) VALUES (8, '食べる', 'freq', 4, ?1);",
        [freq2],
    )
    .unwrap();

    // Kanji entry for 食
    conn.execute(
        "INSERT INTO kanji_bank (dic_id, char, onyomi, kunyomi, tags, meanings, stats) \
         VALUES (7, '食', 'ショク ジキ', 'た.べる く.う', 'P', '[\"eat\",\"food\"]', '{\"strokes\":\"9\"}');",
        [],
    )
    .unwrap();
    drop(conn);

    // Create <config_dir>/dict_resources/dict-7/styles.css and img/diagram.png
    let dict7_dir = dictionary_resource_dir(&config_dict_res, 7).unwrap();
    fs::create_dir_all(dict7_dir.join("img")).unwrap();
    fs::write(
        dict7_dir.join("styles.css"),
        ".gloss { color: red; } :root { --accent: blue; }",
    )
    .unwrap();
    let png_bytes = b"\x89PNG\r\n\x1a\nfake-png-payload";
    fs::write(dict7_dir.join("img/diagram.png"), png_bytes).unwrap();

    let mut ctx = NoteFieldsContext::new("食べる", "たべる", "朝ご飯を食べる。");
    populate_note_context_from_sqlite(
        &mut ctx,
        &db_path,
        std::slice::from_ref(&config_dict_res),
        &[("Jitendex".into(), 0)],
    )
    .unwrap();

    let profile = AnkiNoteConfig {
        enabled: true,
        endpoint: AnkiEndpoint::default(),
        deck: "Japanese".into(),
        model: "Lapis".into(),
        field_templates: HashMap::from([
            ("Expression".into(), "{expression}".into()),
            ("Furigana".into(), "{furigana}".into()),
            ("FuriganaPlain".into(), "{furigana-plain}".into()),
            ("Glossary".into(), "{glossary}".into()),
            ("Tags".into(), "{tags}".into()),
            ("TagsBrief".into(), "{tags-brief}".into()),
            ("POS".into(), "{part-of-speech}".into()),
            ("PitchGraphs".into(), "{pitch-graphs}".into()),
            ("PitchCategories".into(), "{pitch-categories}".into()),
            ("FreqAll".into(), "{frequencies}".into()),
            ("FreqMinVal".into(), "{frequencies:min-value=true,value-only=true}".into()),
            ("HarmonicRank".into(), "{frequency-harmonic-rank}".into()),
            ("HarmonicOcc".into(), "{frequency-harmonic-occurrence}".into()),
            ("AverageRank".into(), "{frequency-average-rank}".into()),
            ("AverageOcc".into(), "{frequency-average-occurrence}".into()),
            ("Clipboard".into(), "{clipboard}".into()),
        ]),
        tags: vec!["subomi".into()],
        duplicate_policy: 1,
        screenshot_type: 0,
        audio_pad_start: 0.0,
        audio_pad_end: 0.0,
        audio_normalize: false,
        audio_db: -20.0,
        newline_replacer: "<br>".into(),
    };
    ctx.clipboard = Some(String::new());

    // No false warnings from unavailable_profile_markers!
    assert!(unavailable_profile_markers(&profile).is_empty());

    let payload = build_note_payload_from_profile(&profile, &ctx).unwrap();
    assert_eq!(payload.fields["Furigana"], "<ruby>食<rt>た</rt></ruby>べる");
    assert_eq!(payload.fields["FuriganaPlain"], "食[た]べる");
    assert!(payload.fields["Glossary"].contains("<li data-dictionary=\"Jitendex\">"));
    assert!(payload.fields["Glossary"]
        .contains("[data-dictionary=\"Jitendex\"] .gloss { color: red; }"));
    assert!(payload.fields["Glossary"]
        .contains("[data-dictionary=\"Jitendex\"] { --accent: blue; }"));
    assert!(payload.fields["Glossary"].contains("<img src=\""));
    assert_eq!(payload.fields["Tags"], "<ul><li>P: Common word</li></ul>");
    assert_eq!(payload.fields["TagsBrief"], "<ul><li>P</li></ul>");
    assert_eq!(payload.fields["POS"], "v1");
    assert!(payload.fields["PitchGraphs"].contains("<span class=\"subomi-pitch-graph\"><svg"));
    assert_eq!(payload.fields["PitchCategories"], "kifuku");
    assert!(payload.fields["FreqAll"].contains("Jitendex: 100"));
    assert!(payload.fields["FreqAll"].contains("FreqDict2: 300"));
    assert_eq!(payload.fields["FreqMinVal"], "100");
    assert_eq!(payload.fields["HarmonicRank"], "150");
    assert_eq!(payload.fields["HarmonicOcc"], "150");
    assert_eq!(payload.fields["AverageRank"], "200");
    assert_eq!(payload.fields["AverageOcc"], "200");

    // Verify default rank=9999999 and occurrence=0 when absent
    let empty_freq_ctx = NoteFieldsContext::new("未知語", "みちご", "未知語。");
    let default_freq_fields = map_note_fields(
        &HashMap::from([
            ("HR".into(), "{frequency-harmonic-rank}".into()),
            ("HO".into(), "{frequency-harmonic-occurrence}".into()),
            ("AR".into(), "{frequency-average-rank}".into()),
            ("AO".into(), "{frequency-average-occurrence}".into()),
        ]),
        &empty_freq_ctx,
    );
    assert_eq!(default_freq_fields["HR"], "9999999");
    assert_eq!(default_freq_fields["HO"], "0");
    assert_eq!(default_freq_fields["AR"], "9999999");
    assert_eq!(default_freq_fields["AO"], "0");

    // Verify Kanji enrichment
    let mut kanji_ctx = NoteFieldsContext::new("食", "", "食事をする。");
    kanji_ctx.is_kanji = true;
    populate_note_context_from_sqlite(
        &mut kanji_ctx,
        &db_path,
        std::slice::from_ref(&config_dict_res),
        &[("Jitendex".into(), 0)],
    )
    .unwrap();
    let kanji_mapped = map_note_fields(
        &HashMap::from([
            ("Character".into(), "{character}".into()),
            ("Onyomi".into(), "{onyomi}".into()),
            ("Kunyomi".into(), "{kunyomi}".into()),
            ("StrokeCount".into(), "{stroke-count}".into()),
            ("Glossary".into(), "{glossary}".into()),
        ]),
        &kanji_ctx,
    );
    assert_eq!(kanji_mapped["Character"], "食");
    assert_eq!(kanji_mapped["Onyomi"], "ショク, ジキ");
    assert_eq!(kanji_mapped["Kunyomi"], "た.べる, く.う");
    assert_eq!(kanji_mapped["StrokeCount"], "9");
    assert!(kanji_mapped["Glossary"].contains("<ol><li><i>(P, Jitendex)</i></li><ol><li>eat</li><li>food</li></ol></ol>"));

    // Wire test: verify `can_add_note_from_profile` (#340) AND that `add_note_from_profile`
    // uploads dictionary media via `multi`/`storeMediaFile` BEFORE `addNote`!
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let server_handle = std::thread::spawn(move || {
        let mut actions = Vec::new();
        for idx in 0..3 {
            let (mut stream, _) = listener.accept().unwrap();
            let body = read_http_json_body(&mut stream).unwrap();
            actions.push(body["action"].as_str().unwrap().to_owned());
            let reply_json = match idx {
                0 => serde_json::json!({"error": null, "result": [true]}).to_string(),
                1 => {
                    let store_action = &body["params"]["actions"][0];
                    assert_eq!(store_action["action"], "storeMediaFile");
                    let fname = store_action["params"]["filename"].as_str().unwrap();
                    assert!(fname.ends_with(".png"));
                    assert!(fname.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')));
                    serde_json::json!({"error": null, "result": [fname]}).to_string()
                }
                _ => serde_json::json!({"error": null, "result": 1669909161234_i64}).to_string(),
            };
            let http_resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{}",
                reply_json.len(),
                reply_json
            );
            stream.write_all(http_resp.as_bytes()).unwrap();
        }
        actions
    });

    let mut live_profile = profile.clone();
    live_profile.endpoint = AnkiEndpoint {
        hostname: "http://127.0.0.1".into(),
        port: addr.port(),
        api_key: None,
        timeout: Some(std::time::Duration::from_secs(2)),
        use_api_key: false,
    };
    let live_ctrl = AnkiController::new(live_profile.endpoint());
    assert!(live_ctrl.can_add_note_from_profile(&live_profile, &ctx).unwrap());
    let added_id = live_ctrl.add_note_from_profile(&live_profile, &ctx).unwrap();
    assert_eq!(added_id, 1669909161234);

    let recorded_actions = server_handle.join().unwrap();
    assert_eq!(recorded_actions, vec!["canAddNotes", "multi", "addNote"]);

    let _ = fs::remove_dir_all(&temp_dir);
}

#[test]
fn scope_dictionary_css_handles_media_queries_and_existing_data_dictionary_selectors() {
    let raw = "/* comment */ @import url('evil.css'); .a, .b { color: green; } [data-dictionary=\"Other\"] .x { color: red; } @media (prefers-color-scheme: dark) { .a { color: white; } }";
    let scoped = scope_dictionary_css(raw, "MyDict");
    assert!(!scoped.contains("@import"));
    assert!(scoped.contains("[data-dictionary=\"MyDict\"] .a, [data-dictionary=\"MyDict\"] .b { color: green; }"));
    assert!(scoped.contains("[data-dictionary=\"Other\"] .x { color: red; }"));
    assert!(scoped.contains("@media (prefers-color-scheme: dark) { [data-dictionary=\"MyDict\"] .a { color: white; } }"));
}

#[test]
fn sqlite_selected_occurrences_orders_by_score_desc_and_isolates_rules_for_pitch_categories() {
    use subomi_dict::yomidbbuilder::prepare_db;

    let temp_dir = temp_test_dir("sqlite_anki_score_and_rules");
    let db_path = temp_dir.join("dictionaries.sqlite");
    prepare_db(&db_path).unwrap();

    let conn = subomi_dict::sqlite_db::open_read_write_connection(&db_path).unwrap();
    conn.execute(
        "INSERT INTO directory (dic_id, title, format, revision, sequenced) VALUES \
         (1, 'DictA', 3, '1', 1), \
         (2, 'DictB', 3, '1', 1), \
         (3, 'DictDisabled', 3, '1', 1);",
        [],
    )
    .unwrap();
    conn.execute("INSERT INTO dict_disabled (dic_id) VALUES (3);", [])
        .unwrap();

    // DictA row inserted FIRST (`sequence = 1`, `rowid = 1`) has LOWER score (`10`) and verb rule `"v1"`.
    conn.execute(
        "INSERT INTO term_bank (dic_id, expression, reading, def_tags, rules, score, glossary, sequence, term_tags) \
         VALUES (1, 'かける', '', '', 'v1', 10, '[\"to hang (verb)\"]', 1, '');",
        [],
    )
    .unwrap();
    // DictA row inserted SECOND (`sequence = 2`, `rowid = 2`) has HIGHER score (`200`) and NO verb/adj-i rules.
    conn.execute(
        "INSERT INTO term_bank (dic_id, expression, reading, def_tags, rules, score, glossary, sequence, term_tags) \
         VALUES (1, 'かける', '', '', '', 200, '[\"bet / wager (noun)\"]', 2, '');",
        [],
    )
    .unwrap();
    // Unselected DictB row has `"v5"` rule; disabled DictDisabled row has `"adj-i"` rule.
    conn.execute(
        "INSERT INTO term_bank (dic_id, expression, reading, def_tags, rules, score, glossary, sequence, term_tags) \
         VALUES (2, 'かける', '', '', 'v5', 500, '[\"unselected dict verb\"]', 1, '');",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO term_bank (dic_id, expression, reading, def_tags, rules, score, glossary, sequence, term_tags) \
         VALUES (3, 'かける', '', '', 'adj-i', 999, '[\"disabled dict adj\"]', 1, '');",
        [],
    )
    .unwrap();

    // 3-mora "かける" at pitch position [2]: `"nakadaka"` when noun rules (`""`),
    // `"kifuku"` ONLY when a `v1`/`v5`/`adj-i`/`vs` rule is active.
    let pitch_blob = r#"{"reading":"かける","pitches":[{"position":2}]}"#.as_bytes();
    conn.execute(
        "INSERT INTO term_meta_bank (dic_id, expression, mode, type, data) VALUES (1, 'かける', 'pitch', 4, ?1);",
        [pitch_blob],
    )
    .unwrap();
    drop(conn);

    let templates = HashMap::from([
        ("Glossary".into(), "{glossary}".into()),
        ("PitchCategories".into(), "{pitch-categories}".into()),
    ]);

    // Occurrence 0 in DictController::search order is the `score = 200` noun entry:
    // must pick `"bet / wager (noun)"` and must NOT leak `v1`/`v5`/`adj-i` into `{pitch-categories}`.
    let mut ctx_noun = NoteFieldsContext::new("かける", "かける", "賭けをかける。");
    populate_note_context_from_sqlite(&mut ctx_noun, &db_path, &[], &[("DictA".into(), 0)]).unwrap();
    let mapped_noun = map_note_fields(&templates, &ctx_noun);
    assert!(
        mapped_noun["Glossary"].contains("bet / wager (noun)"),
        "expected highest-score occurrence 0, got: {}",
        mapped_noun["Glossary"]
    );
    assert!(!mapped_noun["Glossary"].contains("to hang (verb)"));
    assert_eq!(mapped_noun["PitchCategories"], "nakadaka");

    // Occurrence 1 is the `score = 10` verb entry (`rules = "v1"`):
    // must pick `"to hang (verb)"` and include `kifuku` in `{pitch-categories}`.
    let mut ctx_verb = NoteFieldsContext::new("かける", "かける", "壁にかける。");
    populate_note_context_from_sqlite(&mut ctx_verb, &db_path, &[], &[("DictA".into(), 1)]).unwrap();
    let mapped_verb = map_note_fields(&templates, &ctx_verb);
    assert!(
        mapped_verb["Glossary"].contains("to hang (verb)"),
        "expected lower-score occurrence 1, got: {}",
        mapped_verb["Glossary"]
    );
    assert!(!mapped_verb["Glossary"].contains("bet / wager (noun)"));
    assert_eq!(mapped_verb["PitchCategories"], "kifuku");

    let _ = fs::remove_dir_all(&temp_dir);
}

#[test]
fn one_click_anki_note_presets_populate_all_five_note_types() {
    let presets = [
        ("Lapis", AnkiNotePreset::Lapis),
        ("Kiku", AnkiNotePreset::Kiku),
        ("Senren", AnkiNotePreset::Senren),
        ("Animecards", AnkiNotePreset::Animecards),
        ("JPMN", AnkiNotePreset::Jpmn),
    ];

    for (name, variant) in presets {
        assert_eq!(AnkiNotePreset::from_name(name), Some(variant));
        let expected_fields = anki_note_preset_fields(variant);
        assert!(!expected_fields.is_empty(), "{name} preset must define fields");

        let ctrl = ConfigController::new();
        let applied = ctrl.apply_anki_note_preset(name).unwrap();
        assert_eq!(applied, expected_fields.len());

        let note = ctrl.anki_profile_note_config().unwrap();
        for (field, marker) in &expected_fields {
            assert_eq!(
                note.field_templates.get(*field).map(String::as_str),
                Some(*marker),
                "{name} field {field} mismatch"
            );
        }
    }

    // Verify all required Subomi markers are covered across the presets.
    let all_markers: Vec<&str> = presets
        .iter()
        .flat_map(|(_, v)| anki_note_preset_fields(*v).into_iter().map(|(_, m)| m))
        .collect();
    for required in [
        "{expression}",
        "{furigana-plain}",
        "{reading}",
        "{audio | audio-media}",
        "{cloze-prefix}<b>{cloze-body}</b>{cloze-suffix}",
        "{audio-context}",
        "{screenshot-video | screenshot}",
        "{selection | glossary}",
        "{pitch-positions}",
        "{pitch-graphs}",
        "{pitch-categories}",
        "{frequency-harmonic-rank}",
        "{frequencies}",
        "{title}",
    ] {
        assert!(
            all_markers.contains(&required),
            "missing required preset marker {required}"
        );
    }

    // When model fields are already loaded from Anki, applying a preset updates matched
    // fields in place while preserving extra custom model fields.
    let ctrl = ConfigController::new();
    ctrl.edit_setting("anki/profile/term/model", "Lapis").unwrap();
    ctrl.set_anki_model_fields(
        "term",
        &[
            "Expression".into(),
            "MainDefinition".into(),
            "Picture".into(),
            "CustomUserField".into(),
        ],
    )
    .unwrap();
    ctrl.edit_setting("anki/profile/term/field/CustomUserField", "{tags}")
        .unwrap();
    let count = ctrl.apply_anki_note_preset("Lapis").unwrap();
    assert_eq!(count, 4);
    let note = ctrl.anki_profile_note_config().unwrap();
    assert_eq!(note.field_templates["Expression"], "{expression}");
    assert_eq!(note.field_templates["MainDefinition"], "{selection | glossary}");
    assert_eq!(note.field_templates["Picture"], "{screenshot-video | screenshot}");
    assert_eq!(note.field_templates["CustomUserField"], "{tags}");
}

#[test]
fn anki_show_tag_toggle_and_profile_switching_persist() {
    let dir = temp_test_dir("anki_show_tag");
    let anki_path = dir.join("anki_connect.json");
    let ctrl = ConfigController::new();

    assert!(!ctrl.anki_show_tag());
    ctrl.edit_setting("anki/profile/show-tag", "true").unwrap();
    assert!(ctrl.anki_show_tag());

    ctrl.add_entry(0, "Mining").unwrap();
    assert_eq!(ctrl.current_anki_profile(), "Mining");
    ctrl.edit_setting("anki/profile/show-tag", "false").unwrap();
    assert!(!ctrl.anki_show_tag());

    ctrl.edit_setting("anki/profile", "Default").unwrap();
    assert!(ctrl.anki_show_tag());

    ctrl.apply_settings(0, &dir, "subomi.conf", &anki_path)
        .unwrap();
    let reloaded = ConfigController::new();
    reloaded.load_anki_profiles(&anki_path).unwrap();
    assert_eq!(reloaded.current_anki_profile(), "Default");
    assert!(reloaded.anki_show_tag());
    reloaded.edit_setting("anki/profile", "Mining").unwrap();
    assert!(!reloaded.anki_show_tag());

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn structured_interface_font_color_keybind_and_osc_limits_work_end_to_end() {
    let dir = temp_test_dir("structured_settings");
    let anki_path = dir.join("anki_connect.json");
    let ctrl = ConfigController::new();

    ctrl.edit_font_setting("interface/sub-font", "Noto Sans CJK JP", "26", true, true)
        .unwrap();
    ctrl.edit_color_setting("interface/sub-text-color", 255, 250, 200, 100)
        .unwrap();
    ctrl.edit_setting("behavior/osc-min-move", "320").unwrap();
    ctrl.edit_setting("subtitle-list/ignore-whitespace", "true")
        .unwrap();
    ctrl.edit_setting("subtitle-list/auto-seek", "true").unwrap();

    ctrl.apply_settings(3, &dir, "subomi.conf", &anki_path).unwrap();
    ctrl.apply_settings(5, &dir, "subomi.conf", &anki_path).unwrap();

    let reloaded = ConfigController::load(&dir, "subomi.conf").unwrap();
    let font_val = match reloaded.effective_setting("interface", "sub-font") {
        Some(Value::Text(t)) => t,
        other => panic!("unexpected font value: {other:?}"),
    };
    let parsed_font = FontSpec::parse(&font_val).unwrap();
    assert_eq!(parsed_font, FontSpec::new("Noto Sans CJK JP", 26.0, true, true));

    let color_val = match reloaded.effective_setting("interface", "sub-text-color") {
        Some(Value::Text(t)) => t,
        other => panic!("unexpected color value: {other:?}"),
    };
    assert_eq!(
        ArgbColor::parse(&color_val).unwrap(),
        ArgbColor::new(255, 250, 200, 100)
    );

    assert_eq!(
        reloaded.effective_setting("behavior", "osc-min-move"),
        Some(Value::Int(320))
    );
    assert_eq!(
        reloaded.effective_setting("subtitle-list", "ignore-whitespace"),
        Some(Value::Bool(true))
    );
    assert_eq!(
        reloaded.effective_setting("subtitle-list", "auto-seek"),
        Some(Value::Bool(true))
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn check_entries_anki_status_batches_can_add_find_notes_and_notes_info_across_policies() {
    use std::io::Write;
    use std::net::TcpListener;
    use std::time::Duration;

    assert_eq!(escape_anki_search_term("a\"b\\c"), "a\\\"b\\\\c");
    assert_eq!(
        format_nid_browse_query(&[1669909161234, 1669909161239, 1669909161234]),
        "nid:1669909161234 or nid:1669909161239"
    );
    assert_eq!(
        normalize_anki_browse_target("1669909161234").as_deref(),
        Some("nid:1669909161234")
    );
    assert_eq!(
        normalize_anki_browse_target("nid:123 or nid:456").as_deref(),
        Some("nid:123 or nid:456")
    );
    assert_eq!(normalize_anki_browse_target("   "), None);

    let mut base_profile = AnkiNoteConfig {
        enabled: true,
        endpoint: AnkiEndpoint::default(),
        deck: "Japanese \"Mining\"".into(),
        model: "Lapis".into(),
        field_templates: HashMap::from([
            ("Expression".into(), "{expression}".into()),
            ("ExpressionReading".into(), "{reading}".into()),
            ("Glossary".into(), "{glossary}".into()),
        ]),
        tags: vec!["subomi".into()],
        duplicate_policy: 1, // DifferentDeck
        screenshot_type: 0,
        audio_pad_start: 0.0,
        audio_pad_end: 0.0,
        audio_normalize: false,
        audio_db: -20.0,
        newline_replacer: "<br>".into(),
    };

    assert_eq!(
        build_entry_find_query(&base_profile, "食\"べる", "たべる").unwrap(),
        "deck:\"Japanese \\\"Mining\\\"\" Expression:\"食\\\"べる\""
    );
    let mut none_profile = base_profile.clone();
    none_profile.duplicate_policy = 0; // None -> all decks
    assert_eq!(
        build_entry_find_query(&none_profile, "食べる", "たべる").unwrap(),
        "Expression:\"食べる\""
    );

    // 1. DifferentDeck (policy = 1), show_tags = false -> `canAddNotes` + `findNotes` only for duplicate
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let handle = std::thread::spawn(move || {
        let mut seen_actions = Vec::new();
        for step in 0..2 {
            let (mut stream, _) = listener.accept().unwrap();
            let body = read_http_json_body(&mut stream).unwrap();
            seen_actions.push(body["action"].as_str().unwrap().to_owned());
            let reply = match step {
                0 => {
                    assert_eq!(body["params"]["notes"][0]["options"]["duplicateScope"], "deck");
                    serde_json::json!({ "error": null, "result": [false, true] }).to_string()
                }
                _ => {
                    assert_eq!(
                        body["params"]["query"],
                        "deck:\"Japanese \\\"Mining\\\"\" Expression:\"食べる\""
                    );
                    serde_json::json!({
                        "error": null,
                        "result": [1669909161234_i64, 1669909161239_i64]
                    })
                    .to_string()
                }
            };
            let http = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{reply}",
                reply.len()
            );
            stream.write_all(http.as_bytes()).unwrap();
        }
        seen_actions
    });

    base_profile.endpoint = AnkiEndpoint {
        hostname: "http://127.0.0.1".into(),
        port,
        api_key: None,
        timeout: Some(Duration::from_secs(2)),
        use_api_key: false,
    };
    let ctrl = AnkiController::new(base_profile.endpoint());
    let entries = vec![
        ("食べる".to_owned(), "たべる".to_owned()),
        ("未知語".to_owned(), "みちご".to_owned()),
    ];
    let statuses = ctrl
        .check_entries_anki_status(&base_profile, &entries, false)
        .unwrap();
    assert_eq!(handle.join().unwrap(), vec!["canAddNotes", "findNotes"]);
    assert_eq!(statuses.len(), 2);
    assert!(!statuses[0].can_add && statuses[0].is_duplicate);
    assert_eq!(
        statuses[0].browse_query,
        "nid:1669909161234 or nid:1669909161239"
    );
    assert!(statuses[0].tags.is_empty());
    assert!(statuses[1].can_add && !statuses[1].is_duplicate);
    assert_eq!(statuses[1].browse_query, "");

    // 2. SameDeck (policy = 2, allowDuplicate = true) with show_tags = true ->
    //    `can_add` stays true (`is_duplicate` = false, so `+` stays enabled!) while
    //    `findNotes` + `notesInfo` still fetch existing card tags!
    let listener2 = TcpListener::bind("127.0.0.1:0").unwrap();
    let port2 = listener2.local_addr().unwrap().port();
    let handle2 = std::thread::spawn(move || {
        let mut seen_actions = Vec::new();
        for step in 0..3 {
            let (mut stream, _) = listener2.accept().unwrap();
            let body = read_http_json_body(&mut stream).unwrap();
            seen_actions.push(body["action"].as_str().unwrap().to_owned());
            let reply = match step {
                0 => {
                    assert_eq!(body["params"]["notes"][0]["options"]["allowDuplicate"], true);
                    serde_json::json!({ "error": null, "result": [true] }).to_string()
                }
                1 => serde_json::json!({ "error": null, "result": [1669909165555_i64] }).to_string(),
                _ => serde_json::json!({
                    "error": null,
                    "result": [{
                        "noteId": 1669909165555_i64,
                        "modelName": "Lapis",
                        "tags": ["leech", "anime", "leech"],
                        "fields": { "Expression": { "value": "食べる", "order": 0 } }
                    }]
                })
                .to_string(),
            };
            let http = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{reply}",
                reply.len()
            );
            stream.write_all(http.as_bytes()).unwrap();
        }
        seen_actions
    });

    let mut same_deck_profile = base_profile.clone();
    same_deck_profile.duplicate_policy = 2;
    same_deck_profile.endpoint.port = port2;
    let ctrl2 = AnkiController::new(same_deck_profile.endpoint());
    let same_statuses = ctrl2
        .check_entries_anki_status(
            &same_deck_profile,
            &[("食べる".to_owned(), "たべる".to_owned())],
            true,
        )
        .unwrap();
    assert_eq!(
        handle2.join().unwrap(),
        vec!["canAddNotes", "findNotes", "notesInfo"]
    );
    assert!(same_statuses[0].can_add && !same_statuses[0].is_duplicate);
    assert_eq!(same_statuses[0].note_ids, vec![1669909165555]);
    assert_eq!(same_statuses[0].tags, vec!["leech", "anime"]);
}

#[test]
fn popup_bridge_async_anki_badges_tags_history_navigation_and_epoch_guard() {
    use subomi_app::controller::dict::DictController;
    use subomi_app::{
        bind_rich_bridge_lookup_epoch, dictionary_result_rows, install_popup_navigation,
        install_rich_ui_bridges, mark_popup_term_anki_added, refresh_rich_ui_bridges,
        set_dictionary_rows, SubomiApp, PopupBridge,
    };
    use subomi_dict::yomidbbuilder::prepare_db;
    use slint::{ComponentHandle, Model};
    use std::io::Write;
    use std::net::TcpListener;
    use std::sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    };
    use std::time::{Duration, Instant};

    i_slint_backend_testing::init_no_event_loop();
    let dir = temp_test_dir("popup_anki_ux");
    let config_dir = dir.join("config");
    fs::create_dir_all(config_dir.join("dict_resources")).unwrap();
    let db_path = dir.join("dictionaries.sqlite");
    prepare_db(&db_path).unwrap();

    let conn = subomi_dict::sqlite_db::open_read_write_connection(&db_path).unwrap();
    conn.execute(
        "INSERT INTO directory (dic_id, title, format, revision, sequenced) VALUES (1, 'Jitendex', 3, '1', 1);",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO term_bank (dic_id, expression, reading, def_tags, rules, score, glossary, sequence, term_tags) VALUES \
         (1, '食べる', 'たべる', 'v1', 'v1', 100, '[\"to eat\"]', 1, ''), \
         (1, '世界', 'せかい', 'n', '', 90, '[\"world\"]', 2, ''), \
         (1, '橋', 'はし', 'n', '', 80, '[\"bridge\"]', 3, '');",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO kanji_bank (dic_id, char, onyomi, kunyomi, tags, meanings, stats) VALUES \
         (1, '食', 'ショク', 'た.べる', '', '[\"eat\"]', '{}');",
        [],
    )
    .unwrap();
    drop(conn);

    // Ephemeral AnkiConnect mock server answering dynamic `canAddNotes`, `findNotes`, `notesInfo`
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let port = listener.local_addr().unwrap().port();
    let stop = Arc::new(AtomicBool::new(false));
    let stop_server = stop.clone();
    let server_thread = std::thread::spawn(move || {
        while !stop_server.load(Ordering::Acquire) {
            let Ok((mut stream, _)) = listener.accept() else {
                std::thread::sleep(Duration::from_millis(5));
                continue;
            };
            stream.set_nonblocking(false).unwrap();
            let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
            let Some(body) = read_http_json_body(&mut stream) else {
                continue;
            };
            let action = body["action"].as_str().unwrap_or("");
            let reply = match action {
                "canAddNotes" => {
                    let notes = body["params"]["notes"].as_array().unwrap();
                    let flags: Vec<bool> = notes
                        .iter()
                        .map(|note| {
                            let expr = note["fields"]["Expression"]
                                .as_str()
                                .or_else(|| note["fields"]["Character"].as_str())
                                .unwrap_or("");
                            !matches!(expr, "食べる" | "橋" | "食")
                        })
                        .collect();
                    serde_json::json!({ "error": null, "result": flags }).to_string()
                }
                "findNotes" => {
                    let q = body["params"]["query"].as_str().unwrap_or("");
                    let ids: Vec<i64> = if q.contains("Expression:\"食べる\"") {
                        vec![1669909161234]
                    } else if q.contains("Expression:\"橋\"") {
                        vec![1669909161001, 1669909161002]
                    } else if q.contains("Character:\"食\"") {
                        vec![1669909169000]
                    } else {
                        vec![]
                    };
                    serde_json::json!({ "error": null, "result": ids }).to_string()
                }
                "notesInfo" => {
                    let ids = body["params"]["notes"].as_array().unwrap();
                    let infos: Vec<serde_json::Value> = ids
                        .iter()
                        .filter_map(|v| v.as_i64())
                        .map(|nid| match nid {
                            1669909161234 => serde_json::json!({
                                "noteId": nid,
                                "modelName": "Lapis",
                                "tags": ["mining", "leech"],
                                "fields": { "Expression": { "value": "食べる", "order": 0 } }
                            }),
                            1669909161001 | 1669909161002 => serde_json::json!({
                                "noteId": nid,
                                "modelName": "Lapis",
                                "tags": ["vocab_n4"],
                                "fields": { "Expression": { "value": "橋", "order": 0 } }
                            }),
                            1669909169000 => serde_json::json!({
                                "noteId": nid,
                                "modelName": "KanjiCard",
                                "tags": ["kanji_known"],
                                "fields": { "Character": { "value": "食", "order": 0 } }
                            }),
                            _ => serde_json::json!({}),
                        })
                        .collect();
                    serde_json::json!({ "error": null, "result": infos }).to_string()
                }
                _ => serde_json::json!({ "error": null, "result": null }).to_string(),
            };
            let http = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{reply}",
                reply.len()
            );
            let _ = stream.write_all(http.as_bytes());
        }
    });

    let config = ConfigController::new();
    config.edit_setting("anki/enabled", "true").unwrap();
    config
        .edit_setting("anki/profile/host", "http://127.0.0.1")
        .unwrap();
    config
        .edit_setting("anki/profile/port", &port.to_string())
        .unwrap();
    config.edit_setting("anki/profile/show-tag", "true").unwrap();
    config
        .edit_setting("anki/profile/term/deck", "Japanese")
        .unwrap();
    config
        .edit_setting("anki/profile/term/model", "Lapis")
        .unwrap();
    config.apply_anki_note_preset("Lapis").unwrap();
    config
        .edit_setting("anki/profile/kanji/deck", "Kanji")
        .unwrap();
    config
        .edit_setting("anki/profile/kanji/model", "KanjiCard")
        .unwrap();
    config
        .set_anki_model_fields("kanji", &["Character".into(), "Meaning".into()])
        .unwrap();
    config
        .apply_settings(0, &config_dir, "subomi.conf", &config_dir.join("anki_connect.json"))
        .unwrap();

    let app = SubomiApp::new().unwrap();
    app.window().set_size(slint::LogicalSize::new(1280., 720.));
    app.set_anki_enabled(true);
    let lookup_epoch = Arc::new(AtomicU64::new(1));
    install_popup_navigation(&app);
    install_rich_ui_bridges(&app, &db_path, &config_dir, &config);
    bind_rich_bridge_lookup_epoch(&app, lookup_epoch.clone());
    app.show().unwrap();

    let pb = app.global::<PopupBridge>();
    let wait_anki = |pb: &PopupBridge| {
        let deadline = Instant::now() + Duration::from_secs(3);
        while pb.get_anki_checking() && Instant::now() < deadline {
            i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(10));
            std::thread::sleep(Duration::from_millis(10));
        }
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(10));
    };

    // 1. Initial popup with 2 entries: `食べる` (duplicate) and `世界` (addable)
    let mut dict_ctrl = DictController::open(&db_path).unwrap();
    let mut combined_hits = dict_ctrl.search("食べる").to_vec();
    combined_hits.extend_from_slice(dict_ctrl.search("世界"));
    set_dictionary_rows(&app, dictionary_result_rows(&combined_hits, &[]));
    app.set_dict_open(true);
    wait_anki(&pb);

    let dups: Vec<bool> = pb.get_row_anki_duplicates().iter().collect();
    let queries: Vec<String> = pb
        .get_row_anki_browse_queries()
        .iter()
        .map(|s| s.to_string())
        .collect();
    let tags: Vec<String> = pb
        .get_row_anki_tags()
        .iter()
        .map(|s| s.to_string())
        .collect();
    assert_eq!(dups, vec![true, false]);
    assert_eq!(queries, vec!["nid:1669909161234".to_owned(), String::new()]);
    assert_eq!(tags, vec!["mining leech".to_owned(), String::new()]);
    let split_pills: Vec<String> = pb
        .invoke_split_anki_tags(tags[0].clone().into())
        .iter()
        .map(|s| s.to_string())
        .collect();
    assert_eq!(split_pills, vec!["mining", "leech"]);

    // 2. Clicking `?query=橋` inside YomitanPopup refreshes duplicate & tag badges for `橋`!
    assert!(pb.invoke_activate_glossary_link("?query=橋".into()));
    wait_anki(&pb);
    assert_eq!(app.get_dict_query().as_str(), "橋");
    assert_eq!(
        pb.get_row_anki_duplicates().iter().collect::<Vec<_>>(),
        vec![true]
    );
    assert_eq!(
        pb.get_row_anki_browse_queries()
            .row_data(0)
            .unwrap()
            .as_str(),
        "nid:1669909161001 or nid:1669909161002"
    );
    assert_eq!(
        pb.get_row_anki_tags().row_data(0).unwrap().as_str(),
        "vocab_n4"
    );

    // 3. Clicking Kanji character `'食'` refreshes Anki status via Kanji profile!
    pb.invoke_lookup_kanji_char("食べる".into(), "食".into());
    wait_anki(&pb);
    assert_eq!(app.get_dict_query().as_str(), "食");
    assert_eq!(
        pb.get_row_anki_duplicates().iter().collect::<Vec<_>>(),
        vec![true]
    );
    assert_eq!(
        pb.get_row_anki_tags().row_data(0).unwrap().as_str(),
        "kanji_known"
    );

    // 4. Navigating Back via `PopupBridge.nav_back()` restores `橋` and refreshes its Anki badges!
    pb.invoke_nav_back();
    wait_anki(&pb);
    assert_eq!(app.get_dict_query().as_str(), "橋");
    assert_eq!(
        pb.get_row_anki_browse_queries()
            .row_data(0)
            .unwrap()
            .as_str(),
        "nid:1669909161001 or nid:1669909161002"
    );

    // 5. Navigate back to `食べる` + `世界` and verify `mark_popup_term_anki_added` marks `世界` (idx 1) immediately!
    pb.invoke_nav_back();
    wait_anki(&pb);
    let term_profile = config.anki_profile_note_config().unwrap();
    mark_popup_term_anki_added(&app, 1, 1669909167777, &term_profile, true);
    assert_eq!(
        pb.get_row_anki_duplicates().iter().collect::<Vec<_>>(),
        vec![true, true]
    );
    assert_eq!(
        pb.get_row_anki_browse_queries()
            .row_data(1)
            .unwrap()
            .as_str(),
        "nid:1669909167777"
    );

    // 6. Bumping `lookup_epoch` (hover cancellation) + unreachable AnkiConnect port fails silently with 0 errors!
    stop.store(true, Ordering::Release);
    let _ = server_thread.join();
    lookup_epoch.fetch_add(1, Ordering::AcqRel);
    config.edit_setting("anki/profile/port", "9").unwrap();
    refresh_rich_ui_bridges(&app, &config);
    wait_anki(&pb);
    assert_eq!(
        app.get_status_message().as_str(),
        "",
        "unreachable AnkiConnect during hover check must fail silently"
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn entry_anki_status_primary_note_id_and_subtitle_filter_helpers() {
    let empty_status = EntryAnkiStatus::default();
    assert_eq!(empty_status.primary_note_id(), None);

    let populated = EntryAnkiStatus {
        can_add: false,
        is_duplicate: true,
        note_ids: vec![1669909161234, 1669909161239],
        tags: vec!["subomi".into()],
        browse_query: "nid:1669909161234 or nid:1669909161239".into(),
    };
    assert_eq!(populated.primary_note_id(), Some(1669909161234));

    // `filter_subtitle_text` applies `search/remove-regex` + `search/replace-lines` safely.
    assert_eq!(
        filter_subtitle_text("（笑い）今日は\nパンを食べる。", Some("（[^）]*）"), true, " "),
        "今日は パンを食べる。"
    );
    // Invalid regex safely leaves text intact while still honoring `replace_lines`.
    assert_eq!(
        filter_subtitle_text("一行目\n二行目", Some("[unclosed"), true, ""),
        "一行目二行目"
    );
    assert_eq!(
        filter_subtitle_text("一行目\n二行目", None, false, " "),
        "一行目\n二行目"
    );

    // Optional fields on `NoteFieldsContext` / `AnkiNoteContext` integrate with profile note building.
    let mut ctx: AnkiNoteContext =
        NoteFieldsContext::new("食べる", "たべる", "（笑い）今日は\nパンを食べる。");
    ctx.subtitle_filter_regex = Some("（[^）]*）".into());
    ctx.replace_lines = true;
    ctx.replace_lines_with = " ".into();
    let profile = AnkiNoteConfig {
        enabled: true,
        endpoint: AnkiEndpoint::default(),
        deck: "Japanese".into(),
        model: "Lapis".into(),
        field_templates: HashMap::from([
            ("Expression".into(), "{expression}".into()),
            ("Sentence".into(), "{sentence}".into()),
            ("Cloze".into(), "{cloze-prefix}[{cloze-body}]{cloze-suffix}".into()),
        ]),
        tags: vec![],
        duplicate_policy: 1,
        screenshot_type: 0,
        audio_pad_start: 0.0,
        audio_pad_end: 0.0,
        audio_normalize: false,
        audio_db: -20.0,
        newline_replacer: "<br>".into(),
    };
    let payload = build_note_payload_from_profile(&profile, &ctx).unwrap();
    assert_eq!(payload.fields["Sentence"], "今日は パンを食べる。");
    assert_eq!(payload.fields["Cloze"], "今日は パンを[食べる]。");
}

#[test]
fn merge_note_update_modes_preserve_sort_field_and_populated_fields() {
    use subomi_net::anki::AnkiNoteInfo;

    let profile = AnkiNoteConfig {
        enabled: true,
        endpoint: AnkiEndpoint::default(),
        deck: "Japanese".into(),
        model: "Lapis".into(),
        field_templates: HashMap::from([
            ("Expression".into(), "{expression}".into()),
            ("Sentence".into(), "{sentence}".into()),
            ("MainDefinition".into(), "{glossary}".into()),
            ("Picture".into(), "{screenshot}".into()),
            ("SentenceAudio".into(), "{audio-media}".into()),
            ("Notes".into(), "{clipboard}".into()),
            ("NotOnModel".into(), "{title}".into()),
        ]),
        tags: vec!["subomi".into(), "Mining".into(), "anime".into()],
        duplicate_policy: 1,
        screenshot_type: 0,
        audio_pad_start: 0.0,
        audio_pad_end: 0.0,
        audio_normalize: false,
        audio_db: -20.0,
        newline_replacer: "<br>".into(),
    };

    let existing = AnkiNoteInfo {
        note_id: 1669909161234,
        model_name: "Lapis".into(),
        tags: vec!["mining".into(), "leech".into()],
        fields: HashMap::from([
            ("Expression".into(), "食べる".into()),
            ("Sentence".into(), "古い例文。".into()),
            ("MainDefinition".into(), String::new()),
            ("Picture".into(), "<img src=\"old.jpg\">".into()),
            ("SentenceAudio".into(), String::new()),
            ("Notes".into(), "personal note".into()),
        ]),
        sort_field: Some("Expression".into()),
    };

    let mut ctx = NoteFieldsContext::new("食べる", "たべる", "新しい例文。")
        .with_dictionary("JMdict")
        .with_glossary_item("to eat");
    ctx.clipboard = Some(String::new());
    ctx.title = "Episode 1".into();
    ctx.picture_attachments.push(serde_json::json!({
        "data": "QkJC",
        "filename": "new.jpg",
        "fields": ["Picture"]
    }));
    ctx.audio_attachments.push(serde_json::json!({
        "data": "QUFB",
        "filename": "sent.aac",
        "fields": ["SentenceAudio"]
    }));
    let payload = build_note_payload_from_profile(&profile, &ctx).unwrap();

    // 1. OverwriteNonEmpty (default): updates non-empty fields + replaces Picture media,
    //    NEVER wipes Notes with empty clipboard, preserves unchanged Expression sort field,
    //    ignores NotOnModel, and deduplicates tags case-insensitively (`Mining` skipped).
    assert_eq!(AnkiUpdateMode::default(), AnkiUpdateMode::OverwriteNonEmpty);
    let overwrite = merge_note_update(&existing, &payload, &profile, AnkiUpdateMode::OverwriteNonEmpty);
    assert!(!overwrite.fields.contains_key("Expression"), "unchanged sort field must be preserved");
    assert!(!overwrite.fields.contains_key("Notes"), "populated field must never be wiped by empty template");
    assert!(!overwrite.fields.contains_key("NotOnModel"), "fields absent from note model must be skipped");
    assert_eq!(overwrite.fields["Sentence"], "新しい例文。");
    assert!(overwrite.fields["MainDefinition"].as_str().unwrap().contains("to eat"));
    assert_eq!(overwrite.fields["Picture"], "", "media-only field cleared so new picture replaces old");
    assert_eq!(overwrite.picture.len(), 1);
    assert_eq!(overwrite.audio.len(), 1);
    assert_eq!(overwrite.new_tags, vec!["subomi", "anime"]);

    // 2. FillEmptyOnly: only fills empty MainDefinition and empty SentenceAudio;
    //    leaves populated Sentence, Notes, and Picture untouched!
    let fill = merge_note_update(&existing, &payload, &profile, AnkiUpdateMode::FillEmptyOnly);
    assert!(!fill.fields.contains_key("Expression"));
    assert!(!fill.fields.contains_key("Sentence"));
    assert!(!fill.fields.contains_key("Notes"));
    assert!(!fill.fields.contains_key("Picture"));
    assert!(fill.fields["MainDefinition"].as_str().unwrap().contains("to eat"));
    assert!(fill.picture.is_empty(), "existing Picture must not receive new attachment in FillEmptyOnly");
    assert_eq!(fill.audio.len(), 1, "empty SentenceAudio must receive new attachment");
    assert_eq!(fill.new_tags, vec!["subomi", "anime"]);

    // 3. AppendOrFill: appends Sentence with `<br>`, fills empty MainDefinition,
    //    never appends to Expression sort field, and leaves Picture uncleared so Anki appends media!
    let append = merge_note_update(&existing, &payload, &profile, AnkiUpdateMode::AppendOrFill);
    assert!(!append.fields.contains_key("Expression"), "sort field must never be appended");
    assert_eq!(append.fields["Sentence"], "古い例文。<br>新しい例文。");
    assert!(append.fields["MainDefinition"].as_str().unwrap().contains("to eat"));
    assert!(!append.fields.contains_key("Picture"), "AppendOrFill must keep existing Picture so media appends");
    assert_eq!(append.picture.len(), 1);
    assert_eq!(append.audio.len(), 1);
}

#[test]
fn update_existing_note_with_media_uploads_glossary_images_updates_fields_and_adds_tags() {
    use subomi_app::controller::anki_media::NoteMediaInput;
    use subomi_app::controller::config::AudioSourceEntry;
    use subomi_net::anki::MediaFilePayload;
    use subomi_player::RawFrame;
    use subomi_setting::enums::AudioSourceType;
    use std::io::Write;
    use std::net::TcpListener;
    use std::time::Duration;

    // Local word audio file for `{audio}` capture
    let audio_path = std::env::temp_dir().join(format!(
        "subomi-update-note-audio-{}.mp3",
        std::process::id()
    ));
    fs::write(&audio_path, b"ID3updated-word-audio").unwrap();

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        let mut recorded = Vec::new();
        for step in 0..4 {
            let (mut stream, _) = listener.accept().unwrap();
            let body = read_http_json_body(&mut stream).unwrap();
            let action = body["action"].as_str().unwrap().to_owned();
            recorded.push(action.clone());
            let reply = match step {
                0 => {
                    assert_eq!(action, "notesInfo");
                    assert_eq!(body["params"]["notes"], serde_json::json!([1669909161234_i64]));
                    serde_json::json!({
                        "error": null,
                        "result": [{
                            "noteId": 1669909161234_i64,
                            "modelName": "Lapis",
                            "tags": ["existing"],
                            "fields": {
                                "Expression": { "value": "食べる", "order": 0 },
                                "Sentence": { "value": "古い文。", "order": 1 },
                                "Glossary": { "value": "", "order": 2 },
                                "Picture": { "value": "<img src=\"old.png\">", "order": 3 },
                                "WordAudio": { "value": "", "order": 4 },
                                "Notes": { "value": "keep me", "order": 5 }
                            }
                        }]
                    })
                    .to_string()
                }
                1 => {
                    assert_eq!(action, "multi");
                    let sub = &body["params"]["actions"][0];
                    assert_eq!(sub["action"], "storeMediaFile");
                    assert_eq!(sub["params"]["filename"], "gloss_img.png");
                    serde_json::json!({ "error": null, "result": ["gloss_img.png"] }).to_string()
                }
                2 => {
                    assert_eq!(action, "updateNoteFields");
                    let note = &body["params"]["note"];
                    assert_eq!(note["id"], 1669909161234_i64);
                    assert!(note["fields"].get("Expression").is_none());
                    assert!(note["fields"].get("Notes").is_none());
                    assert_eq!(note["fields"]["Sentence"], "新しい文。");
                    assert!(note["fields"]["Glossary"]
                        .as_str()
                        .unwrap()
                        .contains("gloss_img.png"));
                    assert_eq!(note["fields"]["Picture"], "");
                    assert_eq!(note["audio"][0]["fields"][0], "WordAudio");
                    assert_eq!(note["picture"][0]["fields"][0], "Picture");
                    assert!(note.get("video").is_none());
                    serde_json::json!({ "error": null, "result": null }).to_string()
                }
                _ => {
                    assert_eq!(action, "addTags");
                    assert_eq!(body["params"]["notes"], serde_json::json!([1669909161234_i64]));
                    assert_eq!(body["params"]["tags"], "subomi");
                    serde_json::json!({ "error": null, "result": null }).to_string()
                }
            };
            let http = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{reply}",
                reply.len()
            );
            stream.write_all(http.as_bytes()).unwrap();
        }
        recorded
    });

    let profile = AnkiNoteConfig {
        enabled: true,
        endpoint: AnkiEndpoint {
            hostname: "http://127.0.0.1".into(),
            port,
            api_key: None,
            timeout: Some(Duration::from_secs(2)),
            use_api_key: false,
        },
        deck: "Japanese".into(),
        model: "Lapis".into(),
        field_templates: HashMap::from([
            ("Expression".into(), "{expression}".into()),
            ("Sentence".into(), "{sentence}".into()),
            ("Glossary".into(), "{glossary}".into()),
            ("Picture".into(), "{screenshot}".into()),
            ("WordAudio".into(), "{audio}".into()),
            ("Notes".into(), "{clipboard}".into()),
        ]),
        tags: vec!["existing".into(), "subomi".into()],
        duplicate_policy: 1,
        screenshot_type: 1,
        audio_pad_start: 0.0,
        audio_pad_end: 0.0,
        audio_normalize: false,
        audio_db: -20.0,
        newline_replacer: "<br>".into(),
    };

    let mut ctx = NoteFieldsContext::new("食べる", "たべる", "新しい文。");
    ctx.clipboard = Some(String::new());
    ctx.glossary_definitions.push(GlossaryDefinition {
        dictionary_id: 1,
        dictionary_name: "Jitendex".into(),
        rendered_items: vec!["<span>to eat <img src=\"gloss_img.png\"></span>".into()],
        media_files: vec![MediaFilePayload {
            filename: "gloss_img.png".into(),
            data_base64: "iVBORw0KGgo=".into(),
        }],
        ..Default::default()
    });

    let media_input = NoteMediaInput {
        media: None,
        subtitle_start: 0.0,
        subtitle_end: 0.0,
        context_start: 0.0,
        context_end: 0.0,
        screenshot: Some(RawFrame {
            width: 2,
            height: 2,
            pixels: vec![255; 16],
        }),
        screenshot_video: None,
        subtitle_id: 0,
        audio_sources: vec![AudioSourceEntry {
            name: "Local".into(),
            url: audio_path.to_string_lossy().into_owned(),
            kind: AudioSourceType::AudioSourceTypeFile as i64,
            md5: String::new(),
        }],
    };

    let ctrl = AnkiController::new(profile.endpoint());
    // Reject non-positive note ID before hitting the network.
    assert!(ctrl
        .update_existing_note(0, &profile, &ctx, AnkiUpdateMode::OverwriteNonEmpty)
        .is_err());

    let updated_id = ctrl
        .update_existing_note_with_media(
            1669909161234,
            &profile,
            &mut ctx,
            media_input,
            AnkiUpdateMode::OverwriteNonEmpty,
        )
        .unwrap();
    let _ = fs::remove_file(&audio_path);
    assert_eq!(updated_id, 1669909161234);
    assert_eq!(
        server.join().unwrap(),
        vec!["notesInfo", "multi", "updateNoteFields", "addTags"]
    );
}

#[test]
fn update_existing_note_reports_missing_note_cleanly() {
    use std::io::Write;
    use std::net::TcpListener;
    use std::time::Duration;

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let _ = read_http_json_body(&mut stream);
        let reply = serde_json::json!({ "error": null, "result": [{}] }).to_string();
        let http = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{reply}",
            reply.len()
        );
        stream.write_all(http.as_bytes()).unwrap();
    });

    let profile = AnkiNoteConfig {
        enabled: true,
        endpoint: AnkiEndpoint {
            hostname: "http://127.0.0.1".into(),
            port,
            api_key: None,
            timeout: Some(Duration::from_secs(2)),
            use_api_key: false,
        },
        deck: "Japanese".into(),
        model: "Basic".into(),
        field_templates: HashMap::from([("Expression".into(), "{expression}".into())]),
        tags: vec![],
        duplicate_policy: 1,
        screenshot_type: 0,
        audio_pad_start: 0.0,
        audio_pad_end: 0.0,
        audio_normalize: false,
        audio_db: -20.0,
        newline_replacer: "<br>".into(),
    };
    let ctx = NoteFieldsContext::new("食べる", "たべる", "食べる。");
    let ctrl = AnkiController::new(profile.endpoint());
    let err = ctrl
        .update_existing_note(999999, &profile, &ctx, AnkiUpdateMode::OverwriteNonEmpty)
        .unwrap_err();
    assert!(err.contains("999999"), "unexpected error: {err}");
    server.join().unwrap();
}

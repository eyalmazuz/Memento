//! Integration tests for AnkiController and ConfigController in memento-app.

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use memento_app::controller::anki::{
    build_note_payload_from_profile, create_cloze_sentence, default_note_fields, format_cloze, format_cloze_with_hint,
    format_glossary, map_note_fields, split_cloze, validate_json_payload, validate_note_payload,
    unavailable_profile_markers, AnkiController, GlossaryDefinition, NoteFieldsContext, NotePayload,
};
use memento_app::controller::config::{AnkiNoteConfig, ConfigController};
use memento_net::anki::AnkiEndpoint;

fn temp_test_dir(sub: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("memento_app_test_{}_{}", sub, std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("failed to create temp test directory");
    dir
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
    let file_name = "memento.conf";

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
        tags: vec!["memento".into()],
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
    ctx.pitch_positions = "<span class=\"memento-pitch-position\">[2]</span>".into();
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

    // Matches Memento glossary open & close HTML structure
    assert!(out.glossary.starts_with("<div class=\"yomitan-glossary memento-glossary\"><ol>"));
    assert!(out.glossary.contains("<li data-dictionary=\"JMdict\">"));
    assert!(out.glossary.contains("v1, trans, JMdict"));
    assert!(out.glossary.contains("to eat"));
    assert!(out.glossary.contains("to consume"));
    assert!(out.glossary.contains("to live on"));
    assert!(out.glossary.ends_with(".memento-glossary td {border-style: solid;padding: 0.25em;vertical-align: top;border-width: 1px;border-color: currentColor;}</style></ol></div>"));

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

    // Validate raw JSON string
    assert!(validate_json_payload(r#"{"deckName":"D","modelName":"M","fields":{"F":"V"}}"#).is_ok());
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

    // Verify context split
    assert_eq!(ctx.expression, "食べる");
    assert_eq!(ctx.reading, "たべる");
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
    assert_eq!(mapped.get("Tags").unwrap(), "JLPT-N5");
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

    // Payload metadata integration with memento-anki
    let metadata = payload.to_anki_metadata();
    assert_eq!(metadata.len(), 5); // deckName, modelName, tags, options, fields
}

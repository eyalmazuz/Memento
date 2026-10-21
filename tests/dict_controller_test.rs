//! Integration tests for Dictionary Search and Definition Controller in memento-app.

use memento_app::controller::dict::{
    format_cloze_for_term, format_cloze_spans, format_tags, format_tags_with_notes,
    generate_prefix_queries, normalize_reading, parse_rich_glossary, rank_hits, ClozeSentence, DictController,
    DictTag, InMemoryDictionary, SearchHit,
};

use memento_dict::sqlite_db;
use std::ffi::CString;
use std::fs::{self, OpenOptions};
use std::io::ErrorKind;
use std::path::PathBuf;
use std::ptr;
use std::sync::atomic::{AtomicU64, Ordering};

struct TemporaryDictionaryDatabase(PathBuf);

#[test]
fn structured_glossary_keeps_heading_list_and_link_blocks() {
    let raw = r#"[{"type":"structured-content","content":[
        {"tag":"span","content":[{"tag":"span","style":{"fontWeight":"bold"},"content":"せかいじゅう"},{"tag":"span","content":"【世界中】"}]},
        {"tag":"div","data":{"name":"definition"},"content":"地球上のあらゆる場所。"},
        {"tag":"ul","content":[{"tag":"li","content":"around the world"}]},
        {"tag":"a","href":"?query=世界","content":"←世界"},
        {"tag":"ul","data":{"content":"examples"},"content":[{"tag":"li","content":"example"}]}
    ]}]"#;
    let blocks = parse_rich_glossary(raw);
    assert_eq!(blocks.iter().map(|block| block.kind).collect::<Vec<_>>(), vec![1, 0, 2, 3]);
    assert_eq!(blocks[0].text, "せかいじゅう【世界中】");
    assert_eq!(blocks[3].href, "?query=世界");
}

impl Drop for TemporaryDictionaryDatabase {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

struct TestSqliteHandle(*mut sqlite_db::sqlite3);

impl Drop for TestSqliteHandle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { sqlite_db::sqlite3_close_v2(self.0) };
        }
    }
}

fn temporary_dictionary_path() -> PathBuf {
    static NEXT_ID: AtomicU64 = AtomicU64::new(0);
    for _ in 0..100 {
        let path = std::env::temp_dir().join(format!(
            "memento-dict-controller-{}-{}.sqlite",
            std::process::id(),
            NEXT_ID.fetch_add(1, Ordering::Relaxed),
        ));
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => {
                drop(file);
                return path;
            }
            Err(error) if error.kind() == ErrorKind::AlreadyExists => continue,
            Err(error) => panic!("could not create dictionary fixture: {error}"),
        }
    }
    panic!("could not allocate a unique dictionary fixture path");
}

fn create_dictionary_fixture() -> TemporaryDictionaryDatabase {
    let path = temporary_dictionary_path();
    let c_path = CString::new(path.to_string_lossy().as_bytes()).unwrap();
    let mut raw_handle = ptr::null_mut();
    let status = unsafe {
        sqlite_db::sqlite3_open_v2(
            c_path.as_ptr(),
            &mut raw_handle,
            0x0000_0002 | 0x0000_0004,
            ptr::null(),
        )
    };
    let handle = TestSqliteHandle(raw_handle);
    assert_eq!(status, sqlite_db::SQLITE_OK);

    let schema_and_data = CString::new(
        "CREATE TABLE directory (
             dic_id INTEGER PRIMARY KEY, title TEXT NOT NULL UNIQUE,
             format INTEGER NOT NULL, revision TEXT NOT NULL, sequenced INTEGER NOT NULL
         );
         CREATE TABLE dict_disabled (dic_id INTEGER PRIMARY KEY);
         CREATE TABLE tag_bank (
             dic_id INTEGER NOT NULL, name TEXT NOT NULL, category TEXT NOT NULL,
             ord INTEGER NOT NULL, notes TEXT NOT NULL, score INTEGER NOT NULL,
             PRIMARY KEY(dic_id, name)
         );
         CREATE TABLE term_bank (
             dic_id INTEGER NOT NULL, expression TEXT NOT NULL, reading TEXT NOT NULL,
             def_tags TEXT NOT NULL, rules TEXT NOT NULL, score INTEGER NOT NULL,
             glossary TEXT NOT NULL, sequence INTEGER NOT NULL, term_tags TEXT NOT NULL
         );
         CREATE INDEX idx_term_bank_exp ON term_bank(expression);
         CREATE INDEX idx_term_bank_reading ON term_bank(reading);
         CREATE INDEX idx_term_bank_combo ON term_bank(expression, reading);
         CREATE TABLE term_meta_bank (
             dic_id INTEGER NOT NULL, expression TEXT NOT NULL, mode TEXT NOT NULL,
             type INTEGER NOT NULL, data BLOB
         );
         CREATE TABLE kanji_bank (
             dic_id INTEGER NOT NULL, char TEXT NOT NULL, onyomi TEXT NOT NULL,
             kunyomi TEXT NOT NULL, tags TEXT NOT NULL, meanings TEXT NOT NULL, stats TEXT NOT NULL
         );
         CREATE INDEX idx_kanji_bank_char ON kanji_bank(char);
         CREATE TABLE kanji_meta_bank (
             dic_id INTEGER NOT NULL, expression TEXT NOT NULL, mode TEXT NOT NULL,
             type INTEGER NOT NULL, data BLOB
         );
         INSERT INTO term_meta_bank VALUES
             (1, '食べる', 'freq', 4, '{\"reading\":\"たべる\",\"frequency\":120}'),
             (2, '食べる', 'freq', 4, '{\"reading\":\"たべる\",\"frequency\":180}'),
             (1, '食べる', 'pitch', 4, '{\"reading\":\"たべる\",\"pitches\":[{\"position\":2}]}');
         INSERT INTO directory VALUES
             (1, 'Primary', 3, 'fixture', 0),
             (2, 'Secondary', 3, 'fixture', 0);
         INSERT INTO tag_bank VALUES
             (1, 'common', 'partOfSpeech', 1, 'common word', 50),
             (1, 'jouyou', 'frequent', -5, 'regular-use kanji', 0),
             (1, 'busy_people', 'misc', 1, 'busy people', 0),
             (2, 'common', 'partOfSpeech', 1, 'common word', 50);
         INSERT INTO term_bank VALUES
             (1, '食べる', 'たべる', 'common', 'v1', 100, '[\"to eat\"]', 1, 'common'),
             (2, '食べる', 'たべる', 'common', 'v1', 50, '[\"to eat\"]', 1, ''),
             (1, '食', 'しょく', '', '', 5, '[\"food\"]', 2, ''),
             (1, 'カタカナ', 'かたかな', '', '', 10, '[\"katakana\"]', 3, '');
        INSERT INTO kanji_bank VALUES
            (1, '食', 'ショク', 'た.べる', 'jouyou',
             '[\"eat\", \"food\"]', '{\"busy_people\":\"2\"}'),
            (2, '食', 'ショク', '', '',
             '[\"to consume\"]', '{}'),
            (1, '食', 'ショク', '', 'jouyou',
             '[\"food source\"]', '{\"busy_people\":3}');",
    )
    .unwrap();
    let status = unsafe {
        sqlite_db::sqlite3_exec(
            handle.0,
            schema_and_data.as_ptr(),
            None,
            ptr::null_mut(),
            ptr::null_mut(),
        )
    };
    assert_eq!(status, sqlite_db::SQLITE_OK);
    drop(handle);
    TemporaryDictionaryDatabase(path)
}

#[test]
fn test_open_dictionary_searches_real_term_kanji_and_normalized_rows_read_only() {
    let fixture = create_dictionary_fixture();
    let original_contents = fs::read(&fixture.0).unwrap();
    let mut controller = DictController::open(&fixture.0).expect("fixture should open");

    let term_hits = controller.search("食べる");
    // Like C++ ExactQueryGenerator, every prefix is queried: both 食べる rows
    // come first (longest match), followed by the shorter term 食.
    assert_eq!(term_hits.len(), 3);
    assert_eq!(term_hits[2].expression, "食");
    assert_eq!(term_hits[0].dict_name, "Primary");
    assert_eq!(term_hits[0].score, 100);
    assert_eq!(term_hits[0].tags, "common");
    assert_eq!(term_hits[0].glossary, "◦ to eat");
    assert_eq!(term_hits[1].dict_name, "Secondary");
    assert_eq!(term_hits[1].score, 50);

    let normalized_hits = controller.search("ｶﾀｶﾅ");
    assert_eq!(normalized_hits.len(), 1);
    assert_eq!(normalized_hits[0].expression, "カタカナ");

    let single_kanji_hits = controller.search("食");
    assert_eq!(single_kanji_hits.len(), 4);
    assert!(!single_kanji_hits[0].is_kanji);
    let primary_kanji = single_kanji_hits
        .iter()
        .find(|hit| hit.is_kanji && hit.dict_name == "Primary")
        .expect("Primary kanji row should be present");
    assert_eq!(primary_kanji.expression, "食");
    assert_eq!(primary_kanji.reading, "On: ショク; Kun: た.べる");
    assert_eq!(primary_kanji.glossary, "eat\nfood");
    assert_eq!(primary_kanji.tags, "jouyou");
    assert_eq!(primary_kanji.notes, "busy_people: 2");

    {
        let kanji_hits = controller.search_kanji("食");
        assert_eq!(kanji_hits.len(), 3);
        assert!(kanji_hits.iter().all(|hit| hit.is_kanji));
        assert_eq!(
            kanji_hits
                .iter()
                .filter(|hit| hit.dict_name == "Primary")
                .count(),
            2
        );
        assert!(kanji_hits.iter().any(|hit| hit.dict_name == "Secondary"));
    }
    assert!(controller.search_kanji("猫").is_empty());

    controller.set_order(&[2, 1]);
    let ordered_terms = controller.search("食べる");
    assert_eq!(ordered_terms[0].dict_name, "Secondary");
    assert_eq!(ordered_terms[1].dict_name, "Primary");
    let ordered_kanji = controller.search_kanji("食");
    assert_eq!(ordered_kanji.len(), 3);
    assert_eq!(ordered_kanji[0].dict_name, "Secondary");
    assert!(ordered_kanji[1..]
        .iter()
        .all(|hit| hit.dict_name == "Primary"));
    drop(controller);
    assert_eq!(fs::read(&fixture.0).unwrap(), original_contents);
}

#[test]
fn anki_term_metadata_fills_frequency_and_pitch_markers() {
    let fixture = create_dictionary_fixture();
    let dictionary = DictController::open(&fixture.0).unwrap();
    let metadata = dictionary.get_anki_term_metadata("食べる", "たべる", &["v1".into()]);
    assert!(metadata.frequencies.contains("Primary: 120"));
    assert!(metadata.frequencies.contains("Secondary: 180"));
    assert_eq!(metadata.frequency_harmonic_rank, "144");
    assert_eq!(metadata.frequency_average_rank, "150");
    assert_eq!(metadata.pitch_positions, "<span class=\"memento-pitch-position\">[2]</span>");
    assert_eq!(metadata.pitch_categories, "kifuku");
    let graphs = dictionary.get_pitch_graphs("食べる", "たべる");
    assert_eq!(graphs.len(), 1);
    assert_eq!(graphs[0].dictionary, "Primary");
    assert_eq!(graphs[0].mora, ["た", "べ", "る"]);
    assert_eq!(graphs[0].position, 2);
    let frequencies = dictionary.get_frequencies("食べる", "たべる");
    assert_eq!(frequencies.len(), 2);
    assert_eq!(frequencies[0].dictionary, "Primary");
    assert_eq!(frequencies[1].dictionary, "Secondary");
}

#[test]
fn test_prefix_query_generation_complex_sentence() {
    let text = "吾輩は猫である。名前はまだ無い。";
    let queries = generate_prefix_queries(text);

    assert!(!queries.is_empty());
    // First query should be the full string
    assert_eq!(queries[0], text);
    // Queries should be in descending prefix length
    assert_eq!(queries.last().unwrap(), "吾");

    // Check specific prefix substrings exist
    assert!(queries.contains(&"吾輩は猫である".to_string()));
    assert!(queries.contains(&"吾輩は猫".to_string()));
    assert!(queries.contains(&"吾輩".to_string()));
    assert!(queries.contains(&"吾".to_string()));

    // Test with text containing whitespace
    let ws_text = " 雨 が 降る ";
    let ws_queries = generate_prefix_queries(ws_text);
    // Should include both whitespace-preserved and trimmed variants
    assert!(ws_queries.iter().any(|q| q.contains("雨")));

    // Empty text generates no queries
    assert!(generate_prefix_queries("").is_empty());

    // Single-word query
    let single = generate_prefix_queries("食べる");
    assert_eq!(single, vec!["食べる", "食べ", "食"]);
}

#[test]
fn test_reading_normalization() {
    // Katakana to Hiragana conversion
    assert_eq!(normalize_reading("カタカナ"), "かたかな");
    assert_eq!(normalize_reading("タベル"), "たべる");
    assert_eq!(normalize_reading("スシ"), "すし");

    // Half-width Katakana (NFKC + kata-to-hira)
    assert_eq!(normalize_reading("ﾀﾍﾞﾙ"), "たべる");
    assert_eq!(normalize_reading("ｶﾞ"), "が");

    // Voiced sound marks and small kana
    assert_eq!(normalize_reading("ヴァイオリン"), "ゔぁいおりん");

    // Kanji and Hiragana remain untouched
    assert_eq!(normalize_reading("食べる"), "食べる");
    assert_eq!(normalize_reading("ひらがな"), "ひらがな");
    assert_eq!(normalize_reading("漢字とひらがな"), "漢字とひらがな");

    // Mixed text
    assert_eq!(normalize_reading("ラーメンを食べる"), "らーめんを食べる");
}

#[test]
fn test_search_hit_ranking_exact_first() {
    let mut ctrl = DictController::new();
    let mut dict = InMemoryDictionary::new("JMdict");

    // Add hits with varying exactness and scores
    dict.add_entry(SearchHit::new(
        "食",
        "しょく",
        "eating, food",
        "common",
        1000, // Very high score but short prefix
        false,
    ));
    dict.add_entry(SearchHit::new(
        "食べ",
        "たべ",
        "eating",
        "common",
        500, // High score, prefix len 2
        false,
    ));
    dict.add_entry(SearchHit::new(
        "食べる",
        "たべる",
        "to eat (lower score)",
        "v1",
        20, // Exact match, lower score
        false,
    ));
    dict.add_entry(SearchHit::new(
        "食べる",
        "たべる",
        "to eat (higher score)",
        "v1, common",
        100, // Exact match, higher score
        false,
    ));

    ctrl.add_dictionary(dict);

    let hits = ctrl.search("食べる");
    assert_eq!(hits.len(), 4);

    // Exact matches MUST come first, sorted by score descending:
    // 1st: "食べる" score 100
    assert_eq!(hits[0].expression, "食べる");
    assert_eq!(hits[0].score, 100);

    // 2nd: "食べる" score 20
    assert_eq!(hits[1].expression, "食べる");
    assert_eq!(hits[1].score, 20);

    // 3rd: "食べ" (prefix len 2)
    assert_eq!(hits[2].expression, "食べ");
    assert_eq!(hits[2].score, 500);

    // 4th: "食" (prefix len 1)
    assert_eq!(hits[3].expression, "食");
    assert_eq!(hits[3].score, 1000);
}

#[test]
fn test_search_hit_ranking_longest_prefix_from_sentence() {
    let mut ctrl = DictController::new();
    let mut dict = InMemoryDictionary::new("JMdict");

    dict.add_entry(SearchHit::new("日", "ひ", "day, sun", "", 50, false));
    dict.add_entry(SearchHit::new("日本", "にほん", "Japan (score 100)", "common", 100, false));
    dict.add_entry(SearchHit::new("日本", "にっぽん", "Japan (score 200)", "common", 200, false));
    dict.add_entry(SearchHit::new("日本語", "にほんご", "Japanese language", "common", 80, false));
    dict.add_entry(SearchHit::new("日本語能力試験", "にほんごのうりょくしけん", "JLPT", "n1", 10, false));

    ctrl.add_dictionary(dict);

    // Search sentence starting with "日本語能力試験"
    let hits = ctrl.search("日本語能力試験を受験する");

    assert_eq!(hits.len(), 5);

    // 1st: Longest prefix "日本語能力試験" (len 7)
    assert_eq!(hits[0].expression, "日本語能力試験");

    // 2nd: Next longest prefix "日本語" (len 3)
    assert_eq!(hits[1].expression, "日本語");

    // 3rd & 4th: Prefix "日本" (len 2) - tie broken by score (200 > 100)
    assert_eq!(hits[2].expression, "日本");
    assert_eq!(hits[2].score, 200);
    assert_eq!(hits[3].expression, "日本");
    assert_eq!(hits[3].score, 100);

    // 5th: Shortest prefix "日" (len 1)
    assert_eq!(hits[4].expression, "日");
    assert_eq!(hits[4].score, 50);
}

#[test]
fn test_search_by_reading_normalization() {
    let mut ctrl = DictController::new();
    let mut dict = InMemoryDictionary::new("JMdict");

    dict.add_entry(SearchHit::new(
        "寿司",
        "すし",
        "sushi",
        "common",
        50,
        false,
    ));

    ctrl.add_dictionary(dict);

    // Search with Katakana query "スシ"
    let hits = ctrl.search("スシ");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].expression, "寿司");
    assert_eq!(hits[0].reading, "すし");

    // Search with Half-width Katakana "ｽｼ"
    let hits_hw = ctrl.search("ｽｼ");
    assert_eq!(hits_hw.len(), 1);
    assert_eq!(hits_hw[0].expression, "寿司");
}

#[test]
fn test_standalone_rank_hits() {
    let mut hits = vec![
        SearchHit::new("食", "しょく", "eat", "", 1000, false),
        SearchHit::new("食べる", "たべる", "to eat", "", 10, false),
        SearchHit::new("食べる", "たべる", "to eat", "", 80, false),
        SearchHit::new("食べ", "たべ", "eating", "", 500, false),
    ];

    rank_hits("食べる", &mut hits);

    assert_eq!(hits[0].expression, "食べる");
    assert_eq!(hits[0].score, 80);
    assert_eq!(hits[1].expression, "食べる");
    assert_eq!(hits[1].score, 10);
    assert_eq!(hits[2].expression, "食べ");
    assert_eq!(hits[2].score, 500);
    assert_eq!(hits[3].expression, "食");
    assert_eq!(hits[3].score, 1000);
}

#[test]
fn test_tag_formatting_and_sorting() {
    let tags = vec![
        DictTag::new("v1", 2, 10),
        DictTag::new("common", 1, 100),
        DictTag::new("jlpt-n5", 1, 200),
        DictTag::new("P", 1, 150),
    ];

    // Order 1 tags come before Order 2 tags.
    // Within Order 1, scores break ties descending:
    // jlpt-n5 (200), P (150), common (100). Then v1 (order 2).
    let formatted = format_tags(&tags);
    assert_eq!(formatted, "jlpt-n5, P, common, v1");

    // With notes
    let tags_with_notes = vec![
        DictTag::with_details("v1", "pos", "Ichidan verb", 2, 10),
        DictTag::with_details("P", "misc", "Popular term", 1, 100),
    ];
    let notes_formatted = format_tags_with_notes(&tags_with_notes);
    assert_eq!(notes_formatted, "P: Popular term; v1: Ichidan verb");

    // Empty tags
    assert_eq!(format_tags(&[]), "");
    assert_eq!(format_tags_with_notes(&[]), "");
}

#[test]
fn test_cloze_span_generation_and_formatting() {
    let sentence = "彼女は図書館で本を読んでいる。";
    let term = "本";

    let cloze = format_cloze_for_term(sentence, term).expect("term should be found");
    assert_eq!(cloze.prefix, "彼女は図書館で");
    assert_eq!(cloze.body, "本");
    assert_eq!(cloze.suffix, "を読んでいる。");
    assert_eq!(cloze.formatted, "彼女は図書館で{{c1::本}}を読んでいる。");

    // Cloze number customization
    let c2 = ClozeSentence::with_cloze_number(
        cloze.prefix.clone(),
        cloze.body.clone(),
        cloze.suffix.clone(),
        2,
    );
    assert_eq!(c2.formatted, "彼女は図書館で{{c2::本}}を読んでいる。");

    // Cloze with hint
    let with_hint = ClozeSentence::with_hint(
        cloze.prefix.clone(),
        cloze.body.clone(),
        cloze.suffix.clone(),
        "book",
    );
    assert_eq!(with_hint.formatted, "彼女は図書館で{{c1::本::book}}を読んでいる。");

    // Direct offset slicing with format_cloze_spans
    // "彼女は図書館で本を読んでいる。"
    // "彼女は図書館で" is 7 chars (UTF-16 len 7)
    let spans_cloze = format_cloze_spans(sentence, 7, 1);
    assert_eq!(spans_cloze.prefix, "彼女は図書館で");
    assert_eq!(spans_cloze.body, "本");
    assert_eq!(spans_cloze.suffix, "を読んでいる。");

    // Non-existent term returns None
    assert!(format_cloze_for_term(sentence, "映画").is_none());
    assert!(format_cloze_for_term(sentence, "").is_none());
}

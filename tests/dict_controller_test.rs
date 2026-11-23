//! Integration tests for Dictionary Search and Definition Controller in subomi-app.

use subomi_app::controller::dict::{
    apply_remove_regex, apply_replace_lines, format_cloze_for_term, format_cloze_spans,
    format_tags, format_tags_with_notes, generate_prefix_queries, normalize_nfkc_query,
    normalize_reading, parse_rich_glossary, preprocess_search_text, rank_hits,
    resolve_dictionary_style_path, ClozeSentence, DictController, DictTag, InMemoryDictionary,
    SearchHit,
};

use subomi_dict::sqlite_db;
use std::ffi::CString;
use std::fs::{self, OpenOptions};
use std::io::ErrorKind;
use std::path::PathBuf;
use std::ptr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

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
    assert_eq!(
        blocks.iter().map(|block| block.kind).collect::<Vec<_>>(),
        vec![1, 0, 2, 3]
    );
    assert_eq!(blocks[0].text, "せかいじゅう【世界中】");
    assert_eq!(blocks[3].href, "?query=世界");
}

impl Drop for TemporaryDictionaryDatabase {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
        for suffix in ["-wal", "-shm"] {
            let mut p = self.0.as_os_str().to_os_string();
            p.push(suffix);
            let _ = fs::remove_file(PathBuf::from(p));
        }
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
            "subomi-dict-controller-{}-{}.sqlite",
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

fn exec_sql_fixture(sql: &str) -> TemporaryDictionaryDatabase {
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

    let c_sql = CString::new(sql).unwrap();
    let status = unsafe {
        sqlite_db::sqlite3_exec(
            handle.0,
            c_sql.as_ptr(),
            None,
            ptr::null_mut(),
            ptr::null_mut(),
        )
    };
    assert_eq!(status, sqlite_db::SQLITE_OK);
    drop(handle);
    TemporaryDictionaryDatabase(path)
}

const BASE_SCHEMA_SQL: &str = "
    CREATE TABLE directory (
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
    CREATE INDEX idx_term_meta_exp ON term_meta_bank(expression, mode);
    CREATE TABLE kanji_bank (
        dic_id INTEGER NOT NULL, char TEXT NOT NULL, onyomi TEXT NOT NULL,
        kunyomi TEXT NOT NULL, tags TEXT NOT NULL, meanings TEXT NOT NULL, stats TEXT NOT NULL
    );
    CREATE INDEX idx_kanji_bank_char ON kanji_bank(char);
    CREATE TABLE kanji_meta_bank (
        dic_id INTEGER NOT NULL, expression TEXT NOT NULL, mode TEXT NOT NULL,
        type INTEGER NOT NULL, data BLOB
    );
    CREATE INDEX idx_kanji_meta_exp ON kanji_meta_bank(expression, mode);
";

fn create_dictionary_fixture() -> TemporaryDictionaryDatabase {
    let sql = format!(
        "{BASE_SCHEMA_SQL}
         INSERT INTO term_meta_bank VALUES
             (1, '食べる', 'freq', 4, '{{\"reading\":\"たべる\",\"frequency\":120}}'),
             (2, '食べる', 'freq', 4, '{{\"reading\":\"たべる\",\"frequency\":180}}'),
             (1, '食べる', 'pitch', 4, '{{\"reading\":\"たべる\",\"pitches\":[{{\"position\":2}}]}}');
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
              '[\"eat\", \"food\"]', '{{\"busy_people\":\"2\"}}'),
             (2, '食', 'ショク', '', '',
              '[\"to consume\"]', '{{}}'),
             (1, '食', 'ショク', '', 'jouyou',
              '[\"food source\"]', '{{\"busy_people\":3}}');"
    );
    exec_sql_fixture(&sql)
}

#[test]
fn test_open_dictionary_searches_real_term_kanji_and_normalized_rows_read_only() {
    let fixture = create_dictionary_fixture();
    let original_contents = fs::read(&fixture.0).unwrap();
    let mut controller = DictController::open(&fixture.0).expect("fixture should open");

    let term_hits = controller.search("食べる");
    assert_eq!(term_hits.len(), 3);
    assert_eq!(term_hits[2].expression, "食");
    assert_eq!(term_hits[0].dict_name, "Primary");
    assert_eq!(term_hits[0].dictionary_id, 1);
    assert_eq!(term_hits[0].rules, "v1");
    assert_eq!(term_hits[0].raw_glossary, "[\"to eat\"]");
    assert_eq!(term_hits[0].def_tags, "common");
    assert_eq!(term_hits[0].term_tags, "common");
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
    assert_eq!(primary_kanji.notes, "busy people: 2");

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
    // Pass non-POS tag ("common") to verify `get_anki_term_metadata` queries `term_bank.rules` ("v1")!
    let metadata = dictionary.get_anki_term_metadata("食べる", "たべる", &["common".into()]);
    assert!(metadata.frequencies.contains("Primary: 120"));
    assert!(metadata.frequencies.contains("Secondary: 180"));
    assert_eq!(metadata.frequency_harmonic_rank, "144");
    assert_eq!(metadata.frequency_average_rank, "150");
    assert_eq!(metadata.frequency_harmonic_occurrence, "144");
    assert_eq!(metadata.frequency_average_occurrence, "150");
    assert_eq!(
        metadata.pitch_positions,
        "<span class=\"subomi-pitch-position\">[2]</span>"
    );
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
    assert_eq!(queries[0], text);
    assert_eq!(queries.last().unwrap(), "吾");

    assert!(queries.contains(&"吾輩は猫である".to_string()));
    assert!(queries.contains(&"吾輩は猫".to_string()));
    assert!(queries.contains(&"吾輩".to_string()));
    assert!(queries.contains(&"吾".to_string()));

    let ws_text = " 雨 が 降る ";
    let ws_queries = generate_prefix_queries(ws_text);
    assert!(ws_queries.iter().any(|q| q.contains("雨")));

    assert!(generate_prefix_queries("").is_empty());

    let single = generate_prefix_queries("食べる");
    assert_eq!(single, vec!["食べる", "食べ", "食"]);
}

#[test]
fn test_reading_normalization() {
    assert_eq!(normalize_reading("カタカナ"), "かたかな");
    assert_eq!(normalize_reading("タベル"), "たべる");
    assert_eq!(normalize_reading("スシ"), "すし");

    assert_eq!(normalize_reading("ﾀﾍﾞﾙ"), "たべる");
    assert_eq!(normalize_reading("ｶﾞ"), "が");

    assert_eq!(normalize_reading("ヴァイオリン"), "ゔぁいおりん");

    assert_eq!(normalize_reading("食べる"), "食べる");
    assert_eq!(normalize_reading("ひらがな"), "ひらがな");
    assert_eq!(normalize_reading("漢字とひらがな"), "漢字とひらがな");

    assert_eq!(normalize_reading("ラーメンを食べる"), "らーめんを食べる");
    assert_eq!(normalize_nfkc_query("ＡＢＣ１２３！？"), "ABC123!?");
}

#[test]
fn test_search_hit_ranking_exact_first() {
    let mut ctrl = DictController::new();
    let mut dict = InMemoryDictionary::new("JMdict");

    dict.add_entry(SearchHit::new(
        "食",
        "しょく",
        "eating, food",
        "common",
        1000,
        false,
    ));
    dict.add_entry(SearchHit::new(
        "食べ",
        "たべ",
        "eating",
        "common",
        500,
        false,
    ));
    dict.add_entry(SearchHit::new(
        "食べる",
        "たべる",
        "to eat (lower score)",
        "v1",
        20,
        false,
    ));
    dict.add_entry(SearchHit::new(
        "食べる",
        "たべる",
        "to eat (higher score)",
        "v1, common",
        100,
        false,
    ));

    ctrl.add_dictionary(dict);

    let hits = ctrl.search("食べる");
    assert_eq!(hits.len(), 4);

    assert_eq!(hits[0].expression, "食べる");
    assert_eq!(hits[0].score, 100);

    assert_eq!(hits[1].expression, "食べる");
    assert_eq!(hits[1].score, 20);

    assert_eq!(hits[2].expression, "食べ");
    assert_eq!(hits[2].score, 500);

    assert_eq!(hits[3].expression, "食");
    assert_eq!(hits[3].score, 1000);
}

#[test]
fn test_search_hit_ranking_longest_prefix_from_sentence() {
    let mut ctrl = DictController::new();
    let mut dict = InMemoryDictionary::new("JMdict");

    dict.add_entry(SearchHit::new("日", "ひ", "day, sun", "", 50, false));
    dict.add_entry(SearchHit::new(
        "日本",
        "にほん",
        "Japan (score 100)",
        "common",
        100,
        false,
    ));
    dict.add_entry(SearchHit::new(
        "日本",
        "にっぽん",
        "Japan (score 200)",
        "common",
        200,
        false,
    ));
    dict.add_entry(SearchHit::new(
        "日本語",
        "にほんご",
        "Japanese language",
        "common",
        80,
        false,
    ));
    dict.add_entry(SearchHit::new(
        "日本語能力試験",
        "にほんごのうりょくしけん",
        "JLPT",
        "n1",
        10,
        false,
    ));

    ctrl.add_dictionary(dict);

    let hits = ctrl.search("日本語能力試験を受験する");

    assert_eq!(hits.len(), 5);
    assert_eq!(hits[0].expression, "日本語能力試験");
    assert_eq!(hits[1].expression, "日本語");
    assert_eq!(hits[2].expression, "日本");
    assert_eq!(hits[2].score, 200);
    assert_eq!(hits[3].expression, "日本");
    assert_eq!(hits[3].score, 100);
    assert_eq!(hits[4].expression, "日");
    assert_eq!(hits[4].score, 50);
}

#[test]
fn test_search_by_reading_normalization() {
    let mut ctrl = DictController::new();
    let mut dict = InMemoryDictionary::new("JMdict");

    dict.add_entry(SearchHit::new("寿司", "すし", "sushi", "common", 50, false));

    ctrl.add_dictionary(dict);

    let hits = ctrl.search("スシ");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].expression, "寿司");
    assert_eq!(hits[0].reading, "すし");

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

    let formatted = format_tags(&tags);
    assert_eq!(formatted, "jlpt-n5, P, common, v1");

    let tags_with_notes = vec![
        DictTag::with_details("v1", "pos", "Ichidan verb", 2, 10),
        DictTag::with_details("P", "misc", "Popular term", 1, 100),
    ];
    let notes_formatted = format_tags_with_notes(&tags_with_notes);
    assert_eq!(notes_formatted, "P: Popular term; v1: Ichidan verb");

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

    let c2 = ClozeSentence::with_cloze_number(
        cloze.prefix.clone(),
        cloze.body.clone(),
        cloze.suffix.clone(),
        2,
    );
    assert_eq!(c2.formatted, "彼女は図書館で{{c2::本}}を読んでいる。");

    let with_hint = ClozeSentence::with_hint(
        cloze.prefix.clone(),
        cloze.body.clone(),
        cloze.suffix.clone(),
        "book",
    );
    assert_eq!(
        with_hint.formatted,
        "彼女は図書館で{{c1::本::book}}を読んでいる。"
    );

    let spans_cloze = format_cloze_spans(sentence, 7, 1);
    assert_eq!(spans_cloze.prefix, "彼女は図書館で");
    assert_eq!(spans_cloze.body, "本");
    assert_eq!(spans_cloze.suffix, "を読んでいる。");

    assert!(format_cloze_for_term(sentence, "映画").is_none());
    assert!(format_cloze_for_term(sentence, "").is_none());
}

#[test]
fn open_dict_controller_refreshes_names_and_tags_across_wal_commits() {
    use subomi_app::controller::dict::db_file_fingerprint;
    use subomi_dict::yomidbbuilder;
    use std::io::Write;

    fn crc32(data: &[u8]) -> u32 {
        let mut crc = 0xffff_ffffu32;
        for &byte in data {
            crc ^= u32::from(byte);
            for _ in 0..8 {
                let mask = (crc & 1).wrapping_neg();
                crc = (crc >> 1) ^ (0xedb8_8320 & mask);
            }
        }
        !crc
    }

    fn write_yomi_zip(path: &std::path::Path, entries: &[(&str, &[u8])]) {
        let mut buf = Vec::new();
        let mut central = Vec::new();
        for &(name, data) in entries {
            let offset = buf.len() as u32;
            let crc = crc32(data);
            let size = data.len() as u32;
            let name_bytes = name.as_bytes();
            let name_len = name_bytes.len() as u16;

            buf.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
            buf.extend_from_slice(&20u16.to_le_bytes());
            buf.extend_from_slice(&0u16.to_le_bytes());
            buf.extend_from_slice(&0u16.to_le_bytes());
            buf.extend_from_slice(&0u16.to_le_bytes());
            buf.extend_from_slice(&0u16.to_le_bytes());
            buf.extend_from_slice(&crc.to_le_bytes());
            buf.extend_from_slice(&size.to_le_bytes());
            buf.extend_from_slice(&size.to_le_bytes());
            buf.extend_from_slice(&name_len.to_le_bytes());
            buf.extend_from_slice(&0u16.to_le_bytes());
            buf.extend_from_slice(name_bytes);
            buf.extend_from_slice(data);

            central.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
            central.extend_from_slice(&20u16.to_le_bytes());
            central.extend_from_slice(&20u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&crc.to_le_bytes());
            central.extend_from_slice(&size.to_le_bytes());
            central.extend_from_slice(&size.to_le_bytes());
            central.extend_from_slice(&name_len.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u32.to_le_bytes());
            central.extend_from_slice(&offset.to_le_bytes());
            central.extend_from_slice(name_bytes);
        }
        let cd_offset = buf.len() as u32;
        let cd_size = central.len() as u32;
        let count = entries.len() as u16;
        buf.extend_from_slice(&central);
        buf.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
        buf.extend_from_slice(&0u16.to_le_bytes());
        buf.extend_from_slice(&0u16.to_le_bytes());
        buf.extend_from_slice(&count.to_le_bytes());
        buf.extend_from_slice(&count.to_le_bytes());
        buf.extend_from_slice(&cd_size.to_le_bytes());
        buf.extend_from_slice(&cd_offset.to_le_bytes());
        buf.extend_from_slice(&0u16.to_le_bytes());

        let mut file = fs::File::create(path).unwrap();
        file.write_all(&buf).unwrap();
    }

    let db = TemporaryDictionaryDatabase(temporary_dictionary_path());
    let work_dir = std::env::temp_dir().join(format!(
        "subomi-dict-wal-test-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let res_dir = work_dir.join("res");
    fs::create_dir_all(&res_dir).unwrap();

    let zip1 = work_dir.join("dict1.zip");
    write_yomi_zip(
        &zip1,
        &[
            (
                "index.json",
                br#"{"title":"FirstDict","format":3,"revision":"1"}"#,
            ),
            (
                "styles.css",
                br#".first-dict { color: red; }"#,
            ),
            (
                "term_bank_1.json",
                r#"[["食べる","たべる","","v1",10,["to eat"],1,""]]"#.as_bytes(),
            ),
        ],
    );
    let dic1_id = yomidbbuilder::process_dictionary(&zip1, &db.0, &res_dir).unwrap();

    let mut controller = DictController::open(&db.0).expect("should open WAL database");
    let fp_before = db_file_fingerprint(&db.0);
    assert_eq!(controller.dictionary_id_for_name("FirstDict"), Some(dic1_id));
    assert_eq!(
        controller.dictionary_style_path(&res_dir, dic1_id),
        Some(res_dir.join(format!("dict-{dic1_id}")).join("styles.css"))
    );
    assert_eq!(
        resolve_dictionary_style_path(&res_dir, dic1_id, "FirstDict"),
        Some(res_dir.join(format!("dict-{dic1_id}")).join("styles.css"))
    );
    assert_eq!(controller.dictionary_id_for_name("SecondDict"), None);
    assert!(controller.search("飲む").is_empty());

    let zip2 = work_dir.join("dict2.zip");
    write_yomi_zip(
        &zip2,
        &[
            (
                "index.json",
                br#"{"title":"SecondDict","format":3,"revision":"1"}"#,
            ),
            (
                "tag_bank_1.json",
                r#"[["v5m","partOfSpeech",1,"Godan verb",20],["jouyou","frequent",-5,"regular-use kanji",10],["strokes","misc",1,"stroke count",0]]"#
                    .as_bytes(),
            ),
            (
                "term_bank_1.json",
                r#"[["飲む","のむ","v5m","v5",80,["to drink"],1,""]]"#.as_bytes(),
            ),
            (
                "kanji_bank_1.json",
                r#"[["飲","イン","の.む","jouyou",["drink"],{"strokes":"12"}]]"#.as_bytes(),
            ),
            (
                "term_meta_bank_1.json",
                r#"[["飲む","freq",{"reading":"のむ","frequency":45}],["飲む","pitch",{"reading":"のむ","pitches":[{"position":1}]}]]"#
                    .as_bytes(),
            ),
        ],
    );
    let dic2_id = yomidbbuilder::process_dictionary(&zip2, &db.0, &res_dir).unwrap();
    let fp_after = db_file_fingerprint(&db.0);
    assert_ne!(
        fp_before, fp_after,
        "db_file_fingerprint must detect WAL commits even while a read-only handle is open"
    );

    assert_eq!(controller.dictionary_id_for_name("SecondDict"), Some(dic2_id));

    let term_hits = controller.search("飲む");
    assert_eq!(term_hits.len(), 1);
    assert_eq!(term_hits[0].dict_name, "SecondDict");
    assert_eq!(term_hits[0].tags, "v5m");
    assert_eq!(term_hits[0].glossary, "◦ to drink");

    let kanji_hits = controller.search_kanji("飲");
    assert_eq!(kanji_hits.len(), 1);
    assert_eq!(kanji_hits[0].dict_name, "SecondDict");
    assert_eq!(kanji_hits[0].tags, "jouyou");
    assert_eq!(kanji_hits[0].notes, "stroke count: 12");

    let pitches = controller.get_pitch_graphs("飲む", "のむ");
    assert_eq!(pitches.len(), 1);
    assert_eq!(pitches[0].dictionary, "SecondDict");
    assert_eq!(pitches[0].position, 1);

    let freqs = controller.get_frequencies("飲む", "のむ");
    assert_eq!(freqs.len(), 1);
    assert_eq!(freqs[0].dictionary, "SecondDict");
    assert_eq!(freqs[0].value, "45");

    let (pitch_str, freq_str, _) = controller.get_term_meta("飲む", "のむ");
    assert_eq!(pitch_str, "のむ [1]");
    assert_eq!(freq_str, "SecondDict 45");

    let anki_meta = controller.get_anki_term_metadata("飲む", "のむ", &[]);
    assert!(anki_meta.frequencies.contains("SecondDict: 45"));
    assert_eq!(anki_meta.pitch_categories, "kifuku");

    yomidbbuilder::delete_dictionary(dic2_id, &db.0, &res_dir).unwrap();
    assert_eq!(controller.dictionary_id_for_name("SecondDict"), None);
    assert!(controller.search("飲む").is_empty());
    assert!(controller.search_kanji("飲").is_empty());
    assert!(controller.get_pitch_graphs("飲む", "のむ").is_empty());
    assert!(controller.get_frequencies("飲む", "のむ").is_empty());

    let _ = fs::remove_dir_all(&work_dir);
}

#[test]
fn rule_mask_blocks_single_kana_particles_and_matches_real_deinflections_with_derivations() {
    let sql = format!(
        "{BASE_SCHEMA_SQL}
         INSERT INTO directory VALUES (1, 'JMdict', 3, 'rev1', 0);
         INSERT INTO term_bank VALUES
             (1, 'は', '', 'prt', '', 100, '[\"topic particle\"]', 1, ''),
             (1, 'の', '', 'prt', '', 100, '[\"possessive particle\"]', 2, ''),
             (1, 'と', '', 'prt', '', 100, '[\"quoting/with particle\"]', 3, ''),
             (1, '春', 'はる', 'n', '', 90, '[\"spring\"]', 4, ''),
             (1, '乗る', 'のる', 'v5r', 'v5', 90, '[\"to ride\"]', 5, ''),
             (1, '取る', 'とる', 'v5r', 'v5', 90, '[\"to take\"]', 6, ''),
             (1, '食べる', 'たべる', 'v1', 'v1', 100, '[\"to eat\"]', 7, 'P'),
             (1, '読む', 'よむ', 'v5m', 'v5', 95, '[\"to read\"]', 8, ''),
             (1, '泳ぐ', 'およぐ', 'v5g', 'v5', 85, '[\"to swim\"]', 9, ''),
             (1, '泳く', 'およぐ', 'n', '', 50, '[\"bogus unvoiced entry\"]', 10, ''),
             (1, '来る', 'くる', 'vk', 'vk', 100, '[\"to come\"]', 11, ''),
             (1, '問う', 'とう', 'v5u-s', 'v5', 70, '[\"to ask\"]', 12, ''),
             (1, '請う', 'こう', 'v5u-s', 'v5', 70, '[\"to beg\"]', 13, ''),
             (1, 'いく', '', 'v5k-s', 'v5', 80, '[\"to go\"]', 14, ''),
             (1, '書く', 'かく', 'v5k', 'v5', 90, '[\"to write\"]', 15, ''),
             (1, '話す', 'はなす', 'v5s', 'v5', 90, '[\"to speak\"]', 16, ''),
             (1, '論ずる', 'ろんずる', 'vz', 'vz', 60, '[\"to argue\"]', 17, ''),
             (1, 'レガシー', 'れがしー', '', '', 50, '[[\"食べる\", [\"v1\"]]]', 18, '');"
    );
    let fixture = exec_sql_fixture(&sql);
    let mut controller = DictController::open(&fixture.0).unwrap();

    // Single-kana particles must NEVER falsely match 春 / 乗る / 取る via deinflection!
    let ha_hits = controller.search("は");
    assert_eq!(
        ha_hits.iter().map(|h| h.expression.as_str()).collect::<Vec<_>>(),
        vec!["は"]
    );
    let no_hits = controller.search("の");
    assert_eq!(
        no_hits.iter().map(|h| h.expression.as_str()).collect::<Vec<_>>(),
        vec!["の"]
    );
    let to_hits = controller.search("と");
    assert_eq!(
        to_hits.iter().map(|h| h.expression.as_str()).collect::<Vec<_>>(),
        vec!["と"]
    );

    // Real multi-step and colloquial deinflections match accurately with derivation chains & descriptions:
    let tabesaserareta = controller.search("食べさせられた");
    assert_eq!(tabesaserareta[0].expression, "食べる");
    assert_eq!(
        tabesaserareta[0].derivation,
        "past ← potential or passive ← causative ← ichidan verb"
    );
    assert_eq!(tabesaserareta[0].derivation_descriptions.len(), 4);
    assert_eq!(tabesaserareta[0].derivation_descriptions[0].0, "past");
    assert!(!tabesaserareta[0].derivation_descriptions[0].1.is_empty());
    assert_eq!(tabesaserareta[0].dictionary_id, 1);
    assert_eq!(tabesaserareta[0].rules, "v1");
    assert_eq!(tabesaserareta[0].def_tags, "v1");
    assert_eq!(tabesaserareta[0].term_tags, "P");

    let yonjatta = controller.search("読んじゃった");
    assert_eq!(yonjatta[0].expression, "読む");
    assert_eq!(yonjatta[0].derivation, "past ← -chau ← godan verb");

    let oyoijau = controller.search("泳いじゃう");
    assert_eq!(oyoijau[0].expression, "泳ぐ");
    assert!(!oyoijau.iter().any(|h| h.expression == "泳く"));

    let oyoijimau = controller.search("泳いじまう");
    assert_eq!(oyoijimau[0].expression, "泳ぐ");

    let konai = controller.search("来ない");
    assert_eq!(konai[0].expression, "来る");
    assert_eq!(konai[0].derivation, "negative ← kuru verb");

    assert_eq!(controller.search("問うちまう")[0].expression, "問う");
    assert_eq!(controller.search("請うちまう")[0].expression, "請う");
    assert_eq!(controller.search("読んどる")[0].expression, "読む");
    assert_eq!(controller.search("いっとく")[0].expression, "いく");
    assert_eq!(controller.search("いったり")[0].expression, "いく");
    assert_eq!(controller.search("いっちゃう")[0].expression, "いく");
    assert_eq!(controller.search("いっちまう")[0].expression, "いく");
    assert_eq!(controller.search("論ずれば")[0].expression, "論ずる");

    // Yomitan `さす`+passive sub-conditions:
    assert_eq!(controller.search("書かされた")[0].expression, "書く");
    assert_eq!(controller.last_match_units(), "書かされた".encode_utf16().count());
    let invalid_tabesareta = controller.search("食べされた").to_vec();
    assert!(
        controller.last_match_units() < "食べされた".encode_utf16().count(),
        "invalid 食べされた must never match across the full sasu+passive surface"
    );
    assert!(!invalid_tabesareta
        .iter()
        .any(|h| h.derivation.contains("passive") || h.derivation.contains("causative")));
    assert!(!controller
        .search("話さされる")
        .iter()
        .any(|h| h.expression == "話す"));

    // Legacy Yomichan nested-array deinflection redirect entry `[[\"食べる\", [\"v1\"]]]` is stripped!
    assert!(controller.search("レガシー").is_empty());
}

#[test]
fn full_yomitan_normalization_kana_nfkc_prolonged_vowels_iteration_and_regex() {
    let sql = format!(
        "{BASE_SCHEMA_SQL}
         INSERT INTO directory VALUES (1, 'JMdict', 3, 'rev1', 0);
         INSERT INTO term_bank VALUES
             (1, 'テレビ', '', 'n', '', 100, '[\"television\"]', 1, ''),
             (1, 'てれび', '', 'n', '', 80, '[\"tv (hiragana)\"]', 2, ''),
             (1, 'ABC123', '', 'n', '', 90, '[\"alphanumeric\"]', 3, ''),
             (1, 'ねえ', '', 'int', '', 90, '[\"hey\"]', 4, ''),
             (1, 'すげえ', '', 'adj-i', 'adj-i', 85, '[\"amazing (slang)\"]', 5, ''),
             (1, '大きい', 'おおきい', 'adj-i', 'adj-i', 100, '[\"big\"]', 6, ''),
             (1, 'こういう', '', 'exp', '', 95, '[\"such / like this\"]', 7, ''),
             (1, 'こうゆう', '', 'exp', '', 70, '[\"like this (colloq)\"]', 8, ''),
             (1, '人々', 'ひとびと', 'n', '', 100, '[\"people\"]', 9, ''),
             (1, '人人', 'ひとびと', 'n', '', 60, '[\"each person\"]', 10, '');"
    );
    let fixture = exec_sql_fixture(&sql);
    let mut controller = DictController::open(&fixture.0).unwrap();

    // Bidirectional hiragana <-> katakana + halfwidth katakana:
    let hira_tv = controller.search("てれび");
    assert!(hira_tv.iter().any(|h| h.expression == "テレビ"));
    assert!(hira_tv.iter().any(|h| h.expression == "てれび"));

    let kata_tv = controller.search("テレビ");
    assert!(kata_tv.iter().any(|h| h.expression == "テレビ"));
    assert!(kata_tv.iter().any(|h| h.expression == "てれび"));

    let half_tv = controller.search("ﾃﾚﾋﾞ");
    assert!(half_tv.iter().any(|h| h.expression == "テレビ"));
    assert!(half_tv.iter().any(|h| h.expression == "てれび"));
    assert_eq!(controller.last_match_units(), 4);

    // Fullwidth alphanumeric NFKC:
    let fw = controller.search("ＡＢＣ１２３");
    assert_eq!(fw.len(), 1);
    assert_eq!(fw[0].expression, "ABC123");

    // Prolonged sound mark `ー` expansion:
    assert_eq!(controller.search("ねー")[0].expression, "ねえ");
    assert_eq!(controller.search("すげー")[0].expression, "すげえ");
    assert_eq!(controller.search("おーきい")[0].expression, "大きい");
    let koyu = controller.search("こーゆー");
    assert!(koyu.iter().any(|h| h.expression == "こういう"));
    assert!(koyu.iter().any(|h| h.expression == "こうゆう"));

    // Bidirectional `々` iteration mark (`人々` <-> `人人`):
    let hitobito = controller.search("人々");
    assert!(hitobito.iter().any(|h| h.expression == "人々"));
    assert!(hitobito.iter().any(|h| h.expression == "人人"));

    let jin_jin = controller.search("人人");
    assert!(jin_jin.iter().any(|h| h.expression == "人々"));
    assert!(jin_jin.iter().any(|h| h.expression == "人人"));

    // Config-compatible `search/remove-regex` & `search/replace-lines`:
    assert_eq!(
        preprocess_search_text("（話者）てれび\nを見る", r"^（[^）]*）", true, ""),
        "てれびを見る"
    );
    assert_eq!(apply_remove_regex("[SFX]テレビ", r"^\[[^\]]*\]"), "テレビ");
    assert_eq!(apply_replace_lines("おー\nきい", true, ""), "おーきい");

    controller.set_search_text_filters(r"^（[^）]*）", true, "");
    let filtered_hits = controller.search("（太郎）おー\nきい");
    assert_eq!(filtered_hits[0].expression, "大きい");
}

#[test]
fn unlisted_priority_i64_max_homograph_isolation_pitch_details_and_kanji_meta() {
    let sql = format!(
        "{BASE_SCHEMA_SQL}
         INSERT INTO directory VALUES
             (1, 'UnlistedDict', 3, 'rev1', 0),
             (2, 'SecondPriority', 3, 'rev1', 0),
             (3, 'FirstPriority', 3, 'rev1', 0);
         INSERT INTO tag_bank VALUES
             (3, 'halpern', 'index', 2, 'Halpern NJECD', 10),
             (3, 'strokes', 'misc', 1, 'Stroke count', 5),
             (3, 'grade', 'misc', 1, 'Grade', 20),
             (3, 'freq_idx', 'misc', 1, 'Frequency rank', 20);
         INSERT INTO term_bank VALUES
             (1, '人気', 'にんき', '', '', 999, '[\"unlisted popularity\"]', 1, ''),
             (2, '人気', 'にんき', '', '', 50, '[\"second popularity\"]', 1, ''),
             (3, '人気', 'にんき', '', '', 50, '[\"first popularity\"]', 1, ''),
             (3, '人気', 'ひとけ', '', '', 40, '[\"sign of life\"]', 2, '');
         INSERT INTO term_meta_bank VALUES
             (1, '人気', 'freq', 4, '{{\"reading\":\"にんき\",\"frequency\":100}}'),
             (2, '人気', 'freq', 4, '{{\"reading\":\"にんき\",\"frequency\":200}}'),
             (3, '人気', 'freq', 4, '{{\"reading\":\"にんき\",\"frequency\":{{\"value\":300,\"displayValue\":\"Top 300\"}}}}'),
             (1, '人気', 'pitch', 4, '{{\"reading\":\"にんき\",\"pitches\":[{{\"position\":0}}]}}'),
             (3, '人気', 'pitch', 4, '{{\"reading\":\"にんき\",\"pitches\":[{{\"position\":0,\"nasal\":[2],\"devoice\":[1],\"tags\":[\"common\"]}}]}}');
         INSERT INTO kanji_bank VALUES
             (1, '人', 'ジン', 'ひと', '', '[\"person (unlisted)\"]', '{{}}'),
             (3, '人', 'ジン ニン', 'ひと', '', '[\"person\"]',
              '{{\"halpern\":\"334\",\"strokes\":\"2\",\"grade\":\"1\",\"freq_idx\":\"5\"}}');
         INSERT INTO kanji_meta_bank VALUES
             (1, '人', 'freq', 4, '{{\"frequency\":10}}'),
             (3, '人', 'freq', 4, '{{\"frequency\":5}}');"
    );
    let fixture = exec_sql_fixture(&sql);
    let mut controller = DictController::open(&fixture.0).unwrap();

    // Configure order `[3, 2]`; dictionary `1` is unlisted and MUST receive `i64::MAX` (never `0`!).
    controller.set_order(&[3, 2]);

    let hits = controller.search("人気");
    let ninki_dicts: Vec<&str> = hits
        .iter()
        .filter(|h| h.reading == "にんき")
        .map(|h| h.dict_name.as_str())
        .collect();
    assert_eq!(
        ninki_dicts,
        vec!["FirstPriority", "SecondPriority", "UnlistedDict"]
    );

    // Frequencies & pitches sort by caller `dictionary_order` (`3`, `2`, then unlisted `1`):
    let ninki_freqs = controller.get_frequencies("人気", "にんき");
    assert_eq!(
        ninki_freqs
            .iter()
            .map(|f| (f.dictionary.as_str(), f.value.as_str()))
            .collect::<Vec<_>>(),
        vec![
            ("FirstPriority", "Top 300"),
            ("SecondPriority", "200"),
            ("UnlistedDict", "100")
        ]
    );
    let ninki_pitches = controller.get_pitch_details("人気", "にんき");
    assert_eq!(ninki_pitches.len(), 2);
    assert_eq!(ninki_pitches[0].dictionary, "FirstPriority");
    assert_eq!(ninki_pitches[0].nasal, vec![2]);
    assert_eq!(ninki_pitches[0].devoice, vec![1]);
    assert_eq!(ninki_pitches[0].tags, vec!["common".to_owned()]);
    assert_eq!(ninki_pitches[1].dictionary, "UnlistedDict");

    // Homograph collision guard: `"ひとけ"` and `""` NEVER leak `"にんき"`'s pitch or frequency!
    assert!(controller.get_frequencies("人気", "ひとけ").is_empty());
    assert!(controller.get_frequencies("人気", "").is_empty());
    assert!(controller.get_pitch_graphs("人気", "ひとけ").is_empty());
    assert!(controller.get_pitch_graphs("人気", "").is_empty());
    assert_eq!(
        controller.get_term_meta("人気", "ひとけ"),
        (String::new(), String::new(), String::new())
    );
    assert_eq!(
        controller.get_term_meta("人気", ""),
        (String::new(), String::new(), String::new())
    );

    // Anki harmonic mean handles `{"value": 300, "displayValue": "Top 300"}` (`3 / (1/300 + 1/200 + 1/100) = 3 / (11/600) = 163`):
    let anki_ninki = controller.get_anki_term_metadata("人気", "にんき", &[]);
    assert_eq!(anki_ninki.frequency_harmonic_rank, "163");
    assert_eq!(anki_ninki.frequency_average_rank, "200");
    assert_eq!(anki_ninki.frequency_harmonic_occurrence, "163");
    assert_eq!(anki_ninki.frequency_average_occurrence, "200");

    // Kanji lookup: `tag.notes` sorted by `(order ASC, score DESC, name ASC)` + `kanji_meta_bank` frequencies!
    let kanji_freqs = controller.get_kanji_frequencies("人");
    assert_eq!(
        kanji_freqs
            .iter()
            .map(|f| (f.dictionary.as_str(), f.value.as_str()))
            .collect::<Vec<_>>(),
        vec![("FirstPriority", "5"), ("UnlistedDict", "10")]
    );
    let kanji_hits = controller.search_kanji("人");
    assert_eq!(kanji_hits.len(), 2);
    assert_eq!(kanji_hits[0].dict_name, "FirstPriority");
    assert_eq!(kanji_hits[1].dict_name, "UnlistedDict");
    // Expected order for `FirstPriority`:
    // ord=1, score=20, name="freq_idx" ("Frequency rank: 5")
    // ord=1, score=20, name="grade" ("Grade: 1")
    // ord=1, score=5, name="strokes" ("Stroke count: 2")
    // ord=2, score=10, name="halpern" ("Halpern NJECD: 334")
    // followed by `kanji_meta_bank` frequencies!
    assert_eq!(
        kanji_hits[0].notes,
        "Frequency rank: 5; Grade: 1; Stroke count: 2; Halpern NJECD: 334; FirstPriority: 5; UnlistedDict: 10"
    );
}

#[test]
fn batched_hover_lookup_completes_under_5ms_on_populated_database() {
    let path = temporary_dictionary_path();
    {
        let mut conn = sqlite_db::open_read_write_connection(&path).unwrap();
        conn.execute_batch(BASE_SCHEMA_SQL).unwrap();
        let tx = conn.transaction().unwrap();
        tx.execute(
            "INSERT INTO directory VALUES (1, 'JMdict', 3, 'rev1', 0), (2, 'JPDB', 3, 'rev1', 0);",
            [],
        )
        .unwrap();
        {
            let mut term_stmt = tx
                .prepare("INSERT INTO term_bank VALUES (?1, ?2, ?3, '', ?4, ?5, ?6, ?7, '');")
                .unwrap();
            let mut meta_stmt = tx
                .prepare("INSERT INTO term_meta_bank VALUES (?1, ?2, ?3, 4, ?4);")
                .unwrap();
            for i in 0..5000 {
                let expr = format!("語彙{i}");
                let read = format!("ごい{i}");
                term_stmt
                    .execute(sqlite_db::rusqlite::params![
                        1i64,
                        expr,
                        read,
                        "",
                        10i64,
                        "[\"vocabulary item\"]",
                        i as i64
                    ])
                    .unwrap();
                if i % 5 == 0 {
                    let freq_json = format!(r#"{{"reading":"ごい{i}","frequency":{}}}"#, i + 10);
                    meta_stmt
                        .execute(sqlite_db::rusqlite::params![
                            2i64,
                            format!("語彙{i}"),
                            "freq",
                            freq_json.as_bytes()
                        ])
                        .unwrap();
                }
            }
            term_stmt
                .execute(sqlite_db::rusqlite::params![
                    1i64,
                    "食べる",
                    "たべる",
                    "v1",
                    100i64,
                    "[\"to eat\"]",
                    90001i64
                ])
                .unwrap();
            term_stmt
                .execute(sqlite_db::rusqlite::params![
                    1i64,
                    "食",
                    "しょく",
                    "",
                    20i64,
                    "[\"meal\"]",
                    90002i64
                ])
                .unwrap();
            meta_stmt
                .execute(sqlite_db::rusqlite::params![
                    2i64,
                    "食べる",
                    "freq",
                    r#"{"reading":"たべる","frequency":120}"#.as_bytes()
                ])
                .unwrap();
            meta_stmt
                .execute(sqlite_db::rusqlite::params![
                    1i64,
                    "食べる",
                    "pitch",
                    r#"{"reading":"たべる","pitches":[{"position":2}]}"#.as_bytes()
                ])
                .unwrap();
        }
        tx.commit().unwrap();
    }
    let _guard = TemporaryDictionaryDatabase(path.clone());
    let mut controller = DictController::open(&path).unwrap();
    controller.set_order(&[1, 2]);

    // Warm prepared statements once:
    let _ = controller.search("食べさせられたくないんだけど");

    let iterations = 10;
    let started = Instant::now();
    for _ in 0..iterations {
        let hits = controller.search("食べさせられたくないんだけど").to_vec();
        assert_eq!(hits[0].expression, "食べる");
        let (pitch, _, jpdb) = controller.get_term_meta(&hits[0].expression, &hits[0].reading);
        assert_eq!(pitch, "たべる [2]");
        assert_eq!(jpdb, "120");
        for hit in &hits {
            let _ = controller.get_pitch_graphs(&hit.expression, &hit.reading);
            let _ = controller.get_frequencies(&hit.expression, &hit.reading);
        }
    }
    let avg_ms = started.elapsed().as_secs_f64() * 1000.0 / (iterations as f64);
    assert!(
        avg_ms < 5.0,
        "expected typical hover lookup < 5ms, got {avg_ms:.3}ms"
    );
}

#[test]
fn mecab_enabled_live_unidic_compounds_lemmas_particle_safety_and_ipadic_fallback() {
    let sql = format!(
        "{BASE_SCHEMA_SQL}
         INSERT INTO directory VALUES (1, 'JMdict', 3, 'rev1', 0);
         INSERT INTO term_bank VALUES
             (1, 'は', '', 'prt', '', 100, '[\"topic particle\"]', 1, ''),
             (1, 'の', '', 'prt', '', 100, '[\"possessive particle\"]', 2, ''),
             (1, 'と', '', 'prt', '', 100, '[\"quoting/with particle\"]', 3, ''),
             (1, '春', 'はる', 'n', '', 90, '[\"spring\"]', 4, ''),
             (1, '乗る', 'のる', 'v5r', 'v5', 90, '[\"to ride\"]', 5, ''),
             (1, '取る', 'とる', 'v5r', 'v5', 90, '[\"to take\"]', 6, ''),
             (1, '食べる', 'たべる', 'v1', 'v1', 100, '[\"to eat\"]', 7, 'P'),
             (1, '食べてしまう', 'たべてしまう', 'exp', '', 95, '[\"to eat completely\"]', 8, ''),
             (1, '食べて仕舞う', 'たべてしまう', 'exp', '', 85, '[\"to eat completely (kanji)\"]', 9, ''),
             (1, '見る', 'みる', 'v1', 'v1', 100, '[\"to see\"]', 10, 'P'),
             (1, '見ている', 'みている', 'exp', '', 92, '[\"to be watching\"]', 11, ''),
             (1, '見て居る', 'みている', 'exp', '', 82, '[\"to be watching (kanji)\"]', 12, ''),
             (1, '見ておく', 'みておく', 'exp', '', 94, '[\"to see in advance\"]', 13, ''),
             (1, '見て置く', 'みておく', 'exp', '', 84, '[\"to see in advance (kanji)\"]', 14, ''),
             (1, 'する', 'する', 'vs-i', 'vs', 100, '[\"to do\"]', 15, ''),
             (1, '為る', 'する', 'vs-i', 'vs', 80, '[\"to do (kanji)\"]', 16, ''),
             (1, 'して居る', 'している', 'exp', '', 85, '[\"to be doing\"]', 17, ''),
             (1, 'くださる', 'くださる', 'v5r-i', 'v5', 95, '[\"to give (honorific)\"]', 18, ''),
             (1, '下さる', 'くださる', 'v5r-i', 'v5', 90, '[\"to give (kanji)\"]', 19, ''),
             (1, '日本語能力試験', 'にほんごのうりょくしけん', 'n', '', 98, '[\"JLPT\"]', 20, ''),
             (1, '日本語能力', 'にほんごのうりょく', 'n', '', 95, '[\"Japanese proficiency\"]', 21, ''),
             (1, '日本語', 'にほんご', 'n', '', 90, '[\"Japanese language\"]', 22, ''),
             (1, '日本', 'にほん', 'n', '', 85, '[\"Japan\"]', 23, ''),
             (1, '泳ぐ', 'およぐ', 'v5g', 'v5', 85, '[\"to swim\"]', 24, ''),
             (1, '泳く', 'およぐ', 'n', '', 50, '[\"bogus unvoiced entry\"]', 25, '');"
    );
    let fixture = exec_sql_fixture(&sql);
    let mut controller = DictController::open(&fixture.0).unwrap();

    // Default `mecab_enabled` is false for deterministic unit behavior:
    assert!(!controller.mecab_enabled());
    assert!(!DictController::new().mecab_enabled());
    assert!(controller.search("ください").is_empty());

    // Enable live UniDic MeCab lookup:
    controller.set_mecab_enabled(true);
    assert!(controller.mecab_enabled());

    // Honorific irregular imperative `ください` -> `くださる` & `下さる` via MeCab `orthBase` + `lemma`:
    let kudasai = controller.search("ください");
    let kudasai_exprs: Vec<&str> = kudasai.iter().map(|h| h.expression.as_str()).collect();
    assert!(kudasai_exprs.contains(&"くださる"));
    assert!(kudasai_exprs.contains(&"下さる"));

    // Compound & lemma lookups (`食べてしまった`, `食べさせられてしまった`, `見ておいた`, `見ていた`, `していました`):
    let tabeteshimatta = controller.search("食べてしまった").to_vec();
    let tabe_exprs: Vec<&str> = tabeteshimatta.iter().map(|h| h.expression.as_str()).collect();
    assert_eq!(tabeteshimatta[0].expression, "食べる");
    assert_eq!(
        controller.last_match_units(),
        "食べてしまった".encode_utf16().count()
    );
    assert!(tabe_exprs.contains(&"食べてしまう"));
    assert!(tabe_exprs.contains(&"食べて仕舞う"));

    let tabesaserareteshimatta = controller.search("食べさせられてしまった").to_vec();
    assert_eq!(tabesaserareteshimatta[0].expression, "食べる");
    assert_eq!(
        controller.last_match_units(),
        "食べさせられてしまった".encode_utf16().count()
    );

    let miteoita = controller.search("見ておいた").to_vec();
    let miteoita_exprs: Vec<&str> = miteoita.iter().map(|h| h.expression.as_str()).collect();
    assert!(miteoita_exprs.contains(&"見ておく"));
    assert!(miteoita_exprs.contains(&"見て置く"));
    assert!(miteoita_exprs.contains(&"見る"));
    assert_eq!(miteoita[0].expression, "見ておく");

    let miteita = controller.search("見ていた").to_vec();
    let miteita_exprs: Vec<&str> = miteita.iter().map(|h| h.expression.as_str()).collect();
    assert_eq!(miteita[0].expression, "見る");
    assert!(miteita_exprs.contains(&"見ている"));
    assert!(miteita_exprs.contains(&"見て居る"));

    let shiteimashita = controller.search("していました").to_vec();
    let shite_exprs: Vec<&str> = shiteimashita.iter().map(|h| h.expression.as_str()).collect();
    assert!(shite_exprs.contains(&"する"));
    assert!(shite_exprs.contains(&"為る"));
    assert!(shite_exprs.contains(&"して居る"));

    // Noun compounds (`日本語能力`, `日本語能力試験`):
    let nouryoku = controller.search("日本語能力").to_vec();
    assert_eq!(nouryoku[0].expression, "日本語能力");
    let jlpt = controller.search("日本語能力試験を受験する").to_vec();
    assert_eq!(
        jlpt.iter().map(|h| h.expression.as_str()).collect::<Vec<_>>(),
        vec!["日本語能力試験", "日本語能力", "日本語", "日本"]
    );

    // Strict single-kana particle & rule_mask safety (`は`, `の`, `と` NEVER match `春`, `乗る`, `取る`):
    for (particle_query, expected) in [
        ("は", "は"),
        ("の", "の"),
        ("と", "と"),
        ("は走る", "は"),
        ("の本", "の"),
        ("と行く", "と"),
    ] {
        let hits = controller.search(particle_query);
        assert_eq!(
            hits.iter().map(|h| h.expression.as_str()).collect::<Vec<_>>(),
            vec![expected],
            "particle query {particle_query:?} must never false-match verbs or homophones"
        );
    }

    let oyoijau = controller.search("泳いじゃう");
    assert_eq!(oyoijau[0].expression, "泳ぐ");
    assert!(!oyoijau.iter().any(|h| h.expression == "泳く"));

    // Deterministic 9-column IPADic tokenizer path:
    controller.set_mecab_tokenizer(|text| {
        let out = "食べ\t動詞,自立,*,*,一段,連用形,食べる,タベ,タベ\n\
て\t助詞,接続助詞,*,*,*,*,て,テ,テ\n\
しまっ\t動詞,非自立,*,*,五段・ワ行促音便,連用タ接続,しまう,シマッ,シマッ\n\
た\t助動詞,*,*,*,特殊・タ,基本形,た,タ,タ\nEOS\n";
        Ok(subomi_util::mecab::parse_mecab_lookup_tokens(text, out))
    });
    let ipadic_hits = controller.search("食べてしまった");
    assert_eq!(ipadic_hits[0].expression, "食べる");
    assert!(ipadic_hits.iter().any(|h| h.expression == "食べてしまう"));

    // Graceful fallback when MeCab is unavailable:
    controller.set_mecab_tokenizer(|_| Err("MeCab is unavailable".into()));
    let fallback_hits = controller.search("食べてしまった").to_vec();
    assert_eq!(fallback_hits[0].expression, "食べる");
    assert!(controller.take_search_error().is_none());
    let fallback_compound = controller.search("日本語能力試験").to_vec();
    assert_eq!(fallback_compound[0].expression, "日本語能力試験");
    assert!(controller.take_search_error().is_none());
}

#[test]
fn mecab_enabled_warm_lookup_completes_under_5ms_on_populated_database() {
    let path = temporary_dictionary_path();
    {
        let mut conn = sqlite_db::open_read_write_connection(&path).unwrap();
        conn.execute_batch(BASE_SCHEMA_SQL).unwrap();
        let tx = conn.transaction().unwrap();
        tx.execute(
            "INSERT INTO directory VALUES (1, 'JMdict', 3, 'rev1', 0), (2, 'JPDB', 3, 'rev1', 0);",
            [],
        )
        .unwrap();
        {
            let mut term_stmt = tx
                .prepare("INSERT INTO term_bank VALUES (?1, ?2, ?3, '', ?4, ?5, ?6, ?7, '');")
                .unwrap();
            let mut meta_stmt = tx
                .prepare("INSERT INTO term_meta_bank VALUES (?1, ?2, ?3, 4, ?4);")
                .unwrap();
            for i in 0..5000 {
                let expr = format!("語彙{i}");
                let read = format!("ごい{i}");
                term_stmt
                    .execute(sqlite_db::rusqlite::params![
                        1i64,
                        expr,
                        read,
                        "",
                        10i64,
                        "[\"vocabulary item\"]",
                        i as i64
                    ])
                    .unwrap();
                if i % 5 == 0 {
                    let freq_json = format!(r#"{{"reading":"ごい{i}","frequency":{}}}"#, i + 10);
                    meta_stmt
                        .execute(sqlite_db::rusqlite::params![
                            2i64,
                            format!("語彙{i}"),
                            "freq",
                            freq_json.as_bytes()
                        ])
                        .unwrap();
                }
            }
            for (seq, expr, read, rules, score, gloss) in [
                (90001i64, "食べる", "たべる", "v1", 100i64, "[\"to eat\"]"),
                (90002i64, "食べてしまう", "たべてしまう", "v5", 95i64, "[\"to eat up\"]"),
                (90003i64, "見る", "みる", "v1", 100i64, "[\"to see\"]"),
                (90004i64, "見ておく", "みておく", "v5", 95i64, "[\"to see beforehand\"]"),
                (90005i64, "日本語能力試験", "にほんごのうりょくしけん", "", 98i64, "[\"JLPT\"]"),
            ] {
                term_stmt
                    .execute(sqlite_db::rusqlite::params![
                        1i64, expr, read, rules, score, gloss, seq
                    ])
                    .unwrap();
            }
            meta_stmt
                .execute(sqlite_db::rusqlite::params![
                    2i64,
                    "食べる",
                    "freq",
                    r#"{"reading":"たべる","frequency":120}"#.as_bytes()
                ])
                .unwrap();
            meta_stmt
                .execute(sqlite_db::rusqlite::params![
                    1i64,
                    "食べる",
                    "pitch",
                    r#"{"reading":"たべる","pitches":[{"position":2}]}"#.as_bytes()
                ])
                .unwrap();
        }
        tx.commit().unwrap();
    }
    let _guard = TemporaryDictionaryDatabase(path.clone());
    let mut controller = DictController::open(&path).unwrap();
    controller.set_order(&[1, 2]);
    controller.set_mecab_enabled(true);

    let queries = [
        "食べさせられてしまったんだけど",
        "食べてしまった",
        "見ておいた",
        "見ていた",
        "日本語能力試験を受験する",
    ];
    for q in &queries {
        let _ = controller.search(q);
    }

    let iterations = 6;
    let started = Instant::now();
    for _ in 0..iterations {
        for q in &queries {
            let hits = controller.search(q);
            assert!(!hits.is_empty());
        }
    }
    let total_calls = (queries.len() * iterations) as f64;
    let avg_ms = started.elapsed().as_secs_f64() * 1000.0 / total_calls;
    assert!(
        avg_ms < 5.0,
        "expected warm MeCab-enabled lookup < 5ms, got {avg_ms:.3}ms"
    );
}

//! Streaming Yomitan index and JSON bank importers without full-file DOM materialization.

use std::fmt;
use std::io::{BufReader, Read, Seek};

use rusqlite::{params, OptionalExtension, Statement, Transaction};
use serde::de::{self, DeserializeSeed, Deserializer, SeqAccess, Visitor};
use serde_json::value::RawValue;
use serde_json::Value;
use zip::ZipArchive;

use super::archive::{
    read_json_entry, validate_entry_safety, LimitViolation, LimitedEntryReader, SizeLimits,
};
use super::YomiError;

fn json_value_as_i64(val: &Value) -> Option<i64> {
    if let Some(i) = val.as_i64() {
        return Some(i);
    }
    if let Some(u) = val.as_u64() {
        return i64::try_from(u).ok();
    }
    let f = val.as_f64()?;
    if f.is_finite()
        && f.fract() == 0.0
        && f >= -9_223_372_036_854_775_808.0
        && f < 9_223_372_036_854_775_808.0
    {
        Some(f as i64)
    } else {
        None
    }
}

fn parse_raw_i64(raw: &RawValue, index: usize, bank: &'static str) -> Result<i64, YomiError> {
    let s = raw.get().trim();
    if let Ok(i) = s.parse::<i64>() {
        return Ok(i);
    }
    if let Ok(f) = s.parse::<f64>() {
        if f.is_finite()
            && f.fract() == 0.0
            && f >= -9_223_372_036_854_775_808.0
            && f < 9_223_372_036_854_775_808.0
        {
            return Ok(f as i64);
        }
    }
    Err(YomiError::MalformedBank {
        bank,
        detail: format!("element {index} must be an integer"),
    })
}

fn parse_raw_str(raw: &RawValue, index: usize, bank: &'static str) -> Result<String, YomiError> {
    let s = raw.get().trim();
    if !s.starts_with('"') {
        return Err(YomiError::MalformedBank {
            bank,
            detail: format!("element {index} must be a string"),
        });
    }
    serde_json::from_str::<String>(s).map_err(|_| YomiError::MalformedBank {
        bank,
        detail: format!("element {index} must be a string"),
    })
}

fn parse_raw_nullable_str(
    raw: &RawValue,
    index: usize,
    field: &'static str,
    bank: &'static str,
) -> Result<String, YomiError> {
    let s = raw.get().trim();
    if s == "null" {
        return Ok(String::new());
    }
    if !s.starts_with('"') {
        return Err(YomiError::MalformedBank {
            bank,
            detail: format!("element {index} ({field}) must be string or null"),
        });
    }
    serde_json::from_str::<String>(s).map_err(|_| YomiError::MalformedBank {
        bank,
        detail: format!("element {index} ({field}) must be string or null"),
    })
}

pub(super) fn add_index<R: Read + Seek>(
    archive: &mut ZipArchive<R>,
    tx: &Transaction<'_>,
    total_bytes: &mut u64,
    limits: SizeLimits,
) -> Result<(i64, Option<i64>), YomiError> {
    let Some(index_val) = read_json_entry(archive, "index.json", total_bytes, limits)? else {
        return Err(YomiError::MalformedIndex("missing index.json".to_owned()));
    };
    let obj = index_val
        .as_object()
        .ok_or_else(|| YomiError::MalformedIndex("index.json must be an object".to_owned()))?;

    let title = obj
        .get("title")
        .and_then(Value::as_str)
        .ok_or_else(|| YomiError::MalformedIndex("missing or non-string 'title'".to_owned()))?;
    let format = obj
        .get("format")
        .or_else(|| obj.get("version"))
        .and_then(json_value_as_i64)
        .ok_or_else(|| {
            YomiError::MalformedIndex("missing or non-integer 'format' or 'version'".to_owned())
        })?;
    if format != 3 {
        return Err(YomiError::UnsupportedFormat(format));
    }
    let revision = obj
        .get("revision")
        .and_then(Value::as_str)
        .ok_or_else(|| YomiError::MalformedIndex("missing or non-string 'revision'".to_owned()))?;
    let sequenced = match obj.get("sequenced") {
        None => false,
        Some(val) => val.as_bool().ok_or_else(|| {
            YomiError::MalformedIndex("'sequenced' must be a boolean".to_owned())
        })?,
    };

    let replaced_dic_id: Option<i64> = tx
        .query_row(
            "SELECT dic_id FROM directory WHERE title = ?1;",
            params![title],
            |row| row.get(0),
        )
        .optional()?;

    tx.execute(
        "INSERT OR REPLACE INTO directory (title, format, revision, sequenced) VALUES (?1, ?2, ?3, ?4);",
        params![title, format, revision, i64::from(sequenced)],
    )?;
    let dic_id = tx.last_insert_rowid();
    if dic_id <= 0 {
        return Err(YomiError::InvalidDictionaryId(dic_id));
    }
    Ok((dic_id, replaced_dic_id))
}

#[derive(Clone, Copy)]
pub(super) enum Bank {
    Tag,
    Term,
    TermMeta,
    Kanji,
    KanjiMeta,
}

impl Bank {
    fn prefix(self) -> &'static str {
        match self {
            Self::Tag => "tag_bank_",
            Self::Term => "term_bank_",
            Self::TermMeta => "term_meta_bank_",
            Self::Kanji => "kanji_bank_",
            Self::KanjiMeta => "kanji_meta_bank_",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Tag => "tag_bank",
            Self::Term => "term_bank",
            Self::TermMeta => "term_meta_bank",
            Self::Kanji => "kanji_bank",
            Self::KanjiMeta => "kanji_meta_bank",
        }
    }

    fn insert_sql(self) -> &'static str {
        match self {
            Self::Tag => {
                "INSERT OR REPLACE INTO tag_bank (dic_id, name, category, ord, notes, score) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6);"
            }
            Self::Term => {
                "INSERT INTO term_bank (dic_id, expression, reading, def_tags, rules, score, glossary, sequence, term_tags) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9);"
            }
            Self::TermMeta => {
                "INSERT INTO term_meta_bank (dic_id, expression, mode, type, data) \
                 VALUES (?1, ?2, ?3, ?4, ?5);"
            }
            Self::Kanji => {
                "INSERT INTO kanji_bank (dic_id, char, onyomi, kunyomi, tags, meanings, stats) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7);"
            }
            Self::KanjiMeta => {
                "INSERT INTO kanji_meta_bank (dic_id, expression, mode, type, data) \
                 VALUES (?1, ?2, ?3, ?4, ?5);"
            }
        }
    }
}

fn with_nul(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len() + 1);
    out.extend_from_slice(bytes);
    out.push(0);
    out
}

fn encode_meta_raw(raw: &RawValue, entry: &str) -> Result<(i64, Option<Vec<u8>>), YomiError> {
    let s = raw.get().trim();
    match s.as_bytes().first().copied() {
        Some(b'n') if s == "null" => Ok((0, None)),
        Some(b't') if s == "true" => Ok((6, Some(1_i32.to_le_bytes().to_vec()))),
        Some(b'f') if s == "false" => Ok((6, Some(0_i32.to_le_bytes().to_vec()))),
        Some(b'"') => {
            let text: String = serde_json::from_str(s).map_err(|source| YomiError::Json {
                entry: entry.to_owned(),
                source,
            })?;
            Ok((2, Some(with_nul(text.as_bytes()))))
        }
        Some(b'{') => Ok((4, Some(with_nul(s.as_bytes())))),
        Some(b'[') => Ok((5, Some(with_nul(s.as_bytes())))),
        Some(b'-' | b'0'..=b'9') => {
            if let Ok(i) = s.parse::<i64>() {
                Ok((1, Some(i.to_le_bytes().to_vec())))
            } else if let Ok(f) = s.parse::<f64>() {
                if f.is_finite() {
                    Ok((3, Some(f.to_le_bytes().to_vec())))
                } else {
                    Err(YomiError::MalformedBank {
                        bank: "meta_bank",
                        detail: format!("non-finite JSON number in '{entry}'"),
                    })
                }
            } else {
                Err(YomiError::MalformedBank {
                    bank: "meta_bank",
                    detail: format!("unsupported JSON number in '{entry}'"),
                })
            }
        }
        _ => Err(YomiError::MalformedBank {
            bank: "meta_bank",
            detail: format!("unsupported JSON value in '{entry}'"),
        }),
    }
}

fn insert_row(
    bank: Bank,
    dic_id: i64,
    filename: &str,
    stmt: &mut Statement<'_>,
    row: &[Box<RawValue>],
) -> Result<(), YomiError> {
    let bank_label = bank.label();
    match bank {
        Bank::Tag => {
            if row.len() != 5 {
                return Err(YomiError::MalformedBank {
                    bank: bank_label,
                    detail: format!("expected 5 elements, got {}", row.len()),
                });
            }
            let name = parse_raw_str(&row[0], 0, bank_label)?;
            let category = parse_raw_str(&row[1], 1, bank_label)?;
            let order = parse_raw_i64(&row[2], 2, bank_label)?;
            let notes = parse_raw_str(&row[3], 3, bank_label)?;
            let score = parse_raw_i64(&row[4], 4, bank_label)?;
            stmt.execute(params![dic_id, name, category, order, notes, score])?;
        }
        Bank::Term => {
            if row.len() != 8 {
                return Err(YomiError::MalformedBank {
                    bank: bank_label,
                    detail: format!("expected 8 elements, got {}", row.len()),
                });
            }
            let expression = parse_raw_str(&row[0], 0, bank_label)?;
            let raw_reading = parse_raw_str(&row[1], 1, bank_label)?;
            let reading = if expression == raw_reading {
                ""
            } else {
                raw_reading.as_str()
            };
            let def_tags = parse_raw_nullable_str(&row[2], 2, "def_tags", bank_label)?;
            let rules = parse_raw_str(&row[3], 3, bank_label)?;
            let score = parse_raw_i64(&row[4], 4, bank_label)?;
            let glossary = row[5].get().trim();
            if !glossary.starts_with('[') {
                return Err(YomiError::MalformedBank {
                    bank: bank_label,
                    detail: "element 5 (glossary) must be an array".to_owned(),
                });
            }
            let sequence = parse_raw_i64(&row[6], 6, bank_label)?;
            let term_tags = parse_raw_nullable_str(&row[7], 7, "term_tags", bank_label)?;
            stmt.execute(params![
                dic_id, expression, reading, def_tags, rules, score, glossary, sequence, term_tags,
            ])?;
        }
        Bank::Kanji => {
            if row.len() != 6 {
                return Err(YomiError::MalformedBank {
                    bank: bank_label,
                    detail: format!("expected 6 elements, got {}", row.len()),
                });
            }
            let character = parse_raw_str(&row[0], 0, bank_label)?;
            let onyomi = parse_raw_str(&row[1], 1, bank_label)?;
            let kunyomi = parse_raw_str(&row[2], 2, bank_label)?;
            let tags = parse_raw_str(&row[3], 3, bank_label)?;
            let meanings = row[4].get().trim();
            if !meanings.starts_with('[') {
                return Err(YomiError::MalformedBank {
                    bank: bank_label,
                    detail: "element 4 (meanings) must be an array".to_owned(),
                });
            }
            let stats = row[5].get().trim();
            if !stats.starts_with('{') {
                return Err(YomiError::MalformedBank {
                    bank: bank_label,
                    detail: "element 5 (stats) must be an object".to_owned(),
                });
            }
            stmt.execute(params![
                dic_id, character, onyomi, kunyomi, tags, meanings, stats,
            ])?;
        }
        Bank::TermMeta | Bank::KanjiMeta => {
            if row.len() != 3 {
                return Err(YomiError::MalformedBank {
                    bank: bank_label,
                    detail: format!("expected 3 elements, got {}", row.len()),
                });
            }
            let expression = parse_raw_str(&row[0], 0, bank_label)?;
            let mode = parse_raw_str(&row[1], 1, bank_label)?;
            let (kind, blob) = encode_meta_raw(&row[2], filename)?;
            stmt.execute(params![dic_id, expression, mode, kind, blob])?;
        }
    }
    Ok(())
}

/// Deserializes a single JSON row array into a reused `Vec<Box<RawValue>>` buffer.
struct RowSeed<'a> {
    bank: Bank,
    filename: &'a str,
    buf: &'a mut Vec<Box<RawValue>>,
    error_slot: &'a mut Option<YomiError>,
}

impl<'de> DeserializeSeed<'de> for RowSeed<'_> {
    type Value = ();

    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<(), D::Error> {
        deserializer.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for RowSeed<'_> {
    type Value = ();

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a JSON array row")
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
        self.buf.clear();
        while let Some(elem) = seq.next_element::<Box<RawValue>>()? {
            self.buf.push(elem);
        }
        Ok(())
    }

    fn visit_map<A: de::MapAccess<'de>>(self, _map: A) -> Result<(), A::Error> {
        self.record_non_array()
    }

    fn visit_bool<E: de::Error>(self, _v: bool) -> Result<(), E> {
        self.record_non_array()
    }

    fn visit_i64<E: de::Error>(self, _v: i64) -> Result<(), E> {
        self.record_non_array()
    }

    fn visit_u64<E: de::Error>(self, _v: u64) -> Result<(), E> {
        self.record_non_array()
    }

    fn visit_f64<E: de::Error>(self, _v: f64) -> Result<(), E> {
        self.record_non_array()
    }

    fn visit_str<E: de::Error>(self, _v: &str) -> Result<(), E> {
        self.record_non_array()
    }

    fn visit_unit<E: de::Error>(self) -> Result<(), E> {
        self.record_non_array()
    }
}

impl RowSeed<'_> {
    fn record_non_array<E: de::Error>(self) -> Result<(), E> {
        *self.error_slot = Some(YomiError::MalformedBank {
            bank: self.bank.label(),
            detail: format!("row in '{}' must be a JSON array", self.filename),
        });
        Err(E::custom("row is not an array"))
    }
}

/// Streams a bank JSON root array row-by-row directly into a prepared SQLite statement.
struct BankArraySeed<'a, 'stmt> {
    bank: Bank,
    dic_id: i64,
    filename: &'a str,
    stmt: &'a mut Statement<'stmt>,
    error_slot: &'a mut Option<YomiError>,
}

impl<'de> DeserializeSeed<'de> for BankArraySeed<'_, '_> {
    type Value = ();

    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<(), D::Error> {
        deserializer.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for BankArraySeed<'_, '_> {
    type Value = ();

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a top-level JSON array")
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
        let mut row_buf: Vec<Box<RawValue>> = Vec::with_capacity(8);
        loop {
            let next = seq.next_element_seed(RowSeed {
                bank: self.bank,
                filename: self.filename,
                buf: &mut row_buf,
                error_slot: self.error_slot,
            })?;
            if next.is_none() {
                break;
            }
            if let Err(err) = insert_row(self.bank, self.dic_id, self.filename, self.stmt, &row_buf)
            {
                *self.error_slot = Some(err);
                return Err(de::Error::custom("bank row insertion failed"));
            }
        }
        Ok(())
    }

    fn visit_map<A: de::MapAccess<'de>>(self, _map: A) -> Result<(), A::Error> {
        self.record_non_array()
    }

    fn visit_bool<E: de::Error>(self, _v: bool) -> Result<(), E> {
        self.record_non_array()
    }

    fn visit_i64<E: de::Error>(self, _v: i64) -> Result<(), E> {
        self.record_non_array()
    }

    fn visit_u64<E: de::Error>(self, _v: u64) -> Result<(), E> {
        self.record_non_array()
    }

    fn visit_f64<E: de::Error>(self, _v: f64) -> Result<(), E> {
        self.record_non_array()
    }

    fn visit_str<E: de::Error>(self, _v: &str) -> Result<(), E> {
        self.record_non_array()
    }

    fn visit_unit<E: de::Error>(self) -> Result<(), E> {
        self.record_non_array()
    }
}

impl BankArraySeed<'_, '_> {
    fn record_non_array<E: de::Error>(self) -> Result<(), E> {
        *self.error_slot = Some(YomiError::MalformedBank {
            bank: self.bank.label(),
            detail: format!("'{}' root must be a JSON array", self.filename),
        });
        Err(E::custom("root is not an array"))
    }
}

pub(super) fn add_bank<R: Read + Seek>(
    archive: &mut ZipArchive<R>,
    tx: &Transaction<'_>,
    dic_id: i64,
    bank: Bank,
    total_bytes: &mut u64,
    limits: SizeLimits,
) -> Result<(), YomiError> {
    let mut stmt = tx.prepare(bank.insert_sql())?;
    let prefix = bank.prefix();
    let mut number = 1usize;

    loop {
        let filename = format!("{prefix}{number}.json");
        let entry = match archive.by_name(&filename) {
            Ok(entry) => entry,
            Err(zip::result::ZipError::FileNotFound) => break,
            Err(err) => return Err(YomiError::Zip(err)),
        };
        let _ = validate_entry_safety(&entry, limits)?;
        if entry.is_dir() {
            return Err(YomiError::UnsafeArchiveEntry {
                entry: filename,
                reason: "expected regular file, found directory",
            });
        }

        let limited = LimitedEntryReader::new(entry, total_bytes, limits);
        let mut reader = BufReader::with_capacity(64 * 1024, limited);
        let mut error_slot: Option<YomiError> = None;

        let parse_res = {
            let mut de = serde_json::Deserializer::from_reader(&mut reader);
            let res = BankArraySeed {
                bank,
                dic_id,
                filename: &filename,
                stmt: &mut stmt,
                error_slot: &mut error_slot,
            }
            .deserialize(&mut de);
            match res {
                Ok(()) => de.end(),
                Err(err) => Err(err),
            }
        };

        if let Some(violation) = reader.get_ref().violation() {
            return Err(match violation {
                LimitViolation::EntryTooLarge => YomiError::EntryTooLarge {
                    entry: filename,
                    limit: limits.max_entry_bytes,
                },
                LimitViolation::ArchiveTooLarge => YomiError::ArchiveTooLarge {
                    limit: limits.max_total_bytes,
                },
            });
        }
        if let Some(err) = error_slot {
            return Err(err);
        }
        if let Err(source) = parse_res {
            return Err(YomiError::Json {
                entry: filename,
                source,
            });
        }

        number += 1;
    }

    Ok(())
}

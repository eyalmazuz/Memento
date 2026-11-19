use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use slint::{SharedString, VecModel};

use crate::controller::dict::{shared_readonly_db, DictTag, ReadOnlyDictionaryDb};

use super::{RichKanjiDetail, RichKanjiStat};

const RICH_DB_REFRESH_INTERVAL: Duration = Duration::from_millis(1500);

/// SQLite helper for rich tag metadata and 3-column kanji detail cards, sharing the
/// single process-wide `ReadOnlyDictionaryDb` and its cached `tag_bank` metadata snapshot.
pub(super) struct RichDbHelper {
    path: PathBuf,
    db: Option<Arc<ReadOnlyDictionaryDb>>,
    tags: Arc<HashMap<(i64, String), DictTag>>,
    last_refresh_at: Option<Instant>,
}

impl RichDbHelper {
    pub(super) fn new(path: PathBuf) -> Self {
        let mut helper = Self {
            path,
            db: None,
            tags: Arc::new(HashMap::new()),
            last_refresh_at: None,
        };
        helper.force_refresh();
        helper
    }

    pub(super) fn refresh_if_needed(&mut self) {
        if self.db.is_some()
            && self
                .last_refresh_at
                .is_some_and(|t| t.elapsed() < RICH_DB_REFRESH_INTERVAL)
        {
            return;
        }
        self.force_refresh();
    }

    pub(super) fn force_refresh(&mut self) {
        self.last_refresh_at = Some(Instant::now());
        if !self.path.is_file() {
            self.db = None;
            self.tags = Arc::new(HashMap::new());
            return;
        }
        let Ok(db) = shared_readonly_db(&self.path) else {
            self.db = None;
            self.tags = Arc::new(HashMap::new());
            return;
        };
        if let Ok(snapshot) = db.metadata_snapshot() {
            self.tags = snapshot.tags;
        } else {
            self.tags = Arc::new(HashMap::new());
        }
        self.db = Some(db);
    }

    pub(super) fn resolve_tag_list(&self, dic_id: i64, space_sep: &str) -> Vec<DictTag> {
        let mut seen = HashSet::new();
        let mut list = Vec::new();
        for tok in space_sep.split_whitespace() {
            if tok.is_empty() || !seen.insert(tok) {
                continue;
            }
            if let Some(tag) = self.tags.get(&(dic_id, tok.to_owned())) {
                list.push(tag.clone());
            } else {
                list.push(DictTag::with_details(tok, "", "", 0, 0));
            }
        }
        list.sort_by(|a, b| {
            a.order
                .cmp(&b.order)
                .then_with(|| b.score.cmp(&a.score))
                .then_with(|| a.name.cmp(&b.name))
        });
        list
    }

    pub(super) fn build_kanji_detail(
        &mut self,
        character: &str,
        dic_id: i64,
    ) -> Option<RichKanjiDetail> {
        self.refresh_if_needed();
        let db = self.db.as_ref()?;
        let rows = db
            .with_conn(|conn| memento_dict::sqlite_db::query_kanji_utf8_conn(conn, character).ok())?;
        let row = rows
            .iter()
            .find(|r| dic_id == 0 || r.dic_id == dic_id)
            .or_else(|| rows.first())?;

        let mut readings = Vec::new();
        for part in row
            .onyomi
            .split_whitespace()
            .chain(row.kunyomi.split_whitespace())
        {
            if !part.is_empty() {
                readings.push(SharedString::from(part));
            }
        }

        let meanings: Vec<SharedString> = row
            .meanings
            .iter()
            .filter(|m| !m.trim().is_empty())
            .map(|m| SharedString::from(m.trim()))
            .collect();

        let mut stats = Vec::new();
        let mut classes = Vec::new();
        let mut codes = Vec::new();
        let mut indices = Vec::new();

        if let Ok(serde_json::Value::Object(map)) =
            serde_json::from_str::<serde_json::Value>(&row.stats)
        {
            let mut entries: Vec<(&DictTag, String)> = map
                .iter()
                .filter_map(|(k, v)| {
                    let tag = self.tags.get(&(row.dic_id, k.clone()))?;
                    let val = match v {
                        serde_json::Value::String(s) => s.clone(),
                        serde_json::Value::Number(n) => n.to_string(),
                        serde_json::Value::Bool(b) => b.to_string(),
                        _ => return None,
                    };
                    (!val.is_empty()).then_some((tag, val))
                })
                .collect();
            entries.sort_by(|(a, _), (b, _)| {
                a.order
                    .cmp(&b.order)
                    .then_with(|| b.score.cmp(&a.score))
                    .then_with(|| a.name.cmp(&b.name))
            });
            for (tag, val) in entries {
                let item = RichKanjiStat {
                    label: (if tag.notes.is_empty() {
                        tag.name.as_str()
                    } else {
                        tag.notes.as_str()
                    })
                    .into(),
                    value: val.into(),
                };
                match tag.category.as_str() {
                    "misc" => stats.push(item),
                    "class" => classes.push(item),
                    "code" => codes.push(item),
                    "index" => indices.push(item),
                    _ => {}
                }
            }
        }

        Some(RichKanjiDetail {
            valid: true,
            character: character.into(),
            meanings: Rc::new(VecModel::from(meanings)).into(),
            readings: Rc::new(VecModel::from(readings)).into(),
            stats: Rc::new(VecModel::from(stats)).into(),
            classifications: Rc::new(VecModel::from(classes)).into(),
            codepoints: Rc::new(VecModel::from(codes)).into(),
            indices: Rc::new(VecModel::from(indices)).into(),
        })
    }
}

////////////////////////////////////////////////////////////////////////////////
//
// Copyright (c) 2026 Ripose
//
// This file is part of Memento.
//
// Memento is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, version 2 of the License.
//
// Memento is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with Memento.  If not, see <https://www.gnu.org/licenses/>.
//
////////////////////////////////////////////////////////////////////////////////

//! The durable configuration store that replaces `QSettings` for the
//! pure-Rust Memento.
//!
//! Responsibilities (vs. the `Settings` facade this replaces):
//!
//! * raw map of `group path → key → canonical raw value`; unknown keys
//!   survive every round-trip untouched;
//! * typed accessors [`Config::get`] / [`Config::set`] that fall back to
//!   the certified `keys.h` defaults when a key is absent;
//! * dirty tracking (a write is only flagged when the value actually
//!   changes, matching the C++ setters' early-return-on-equal semantics);
//! * batch transactions ([`Config::transaction`]) with commit/rollback and
//!   a single observer notification on commit;
//! * synchronous change observation ([`Config::subscribe`]) with batched,
//!   deferred notifications;
//! * atomic saving ([`Config::save`]): write to a temporary file, flush,
//!   rename over the target; on any failure the previous file is untouched;
//! * an explicit, diffable change-log written as file header comments
//!   (one row per save that changed content).

use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::path::Path;

use crate::canonical::{
    encode_value, percent_decode_component, percent_encode_component,
    CHANGELOG_PREFIX, CHANGELOG_ROW_PREFIX, FORMAT_MAGIC, REVISION_PREFIX,
};
use crate::error::{ConfigError, ConfigResult};
use crate::keys::{models, DefaultContext, Key, KeyPath, RecentField};
use crate::value::{FromValue, IntoValue, Value};

/// One recorded change in the change-log / event stream.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChangeKind {
    /// The key did not exist and was created.
    Added,
    /// An existing key received a new value.
    Modified,
    /// The key existed and was deleted.
    Removed,
}

/// A single observed key change, addressed by its full path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Change {
    pub path: KeyPath,
    pub kind: ChangeKind,
}

impl fmt::Display for Change {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.kind {
            ChangeKind::Added => write!(f, "+ {}", self.path),
            ChangeKind::Modified => write!(f, "~ {}", self.path),
            ChangeKind::Removed => write!(f, "- {}", self.path),
        }
    }
}

/// The batch of changes handed to every observer. `revision` is the store
/// revision at notification time.
#[derive(Clone, Debug, Default)]
pub struct ChangeSet {
    pub events: Vec<Change>,
    pub revision: u64,
}

impl ChangeSet {
    /// True when the batch touched the given coordinate.
    #[must_use]
    pub fn touches(&self, group: &str, key: &str) -> bool {
        self.events
            .iter()
            .any(|c| c.path.group == group && c.path.key == key)
    }
}

type ObserverFn = Box<dyn FnMut(&ChangeSet) + Send>;

struct ObserverSlot {
    id: u32,
    callback: ObserverFn,
}

/// Handle for unregistering an observer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Subscription {
    id: u32,
}

/// Change-log row materialization (one header comment per content-changing
/// save).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChangelogEntry {
    pub revision: u64,
    pub added: Vec<String>,
    pub modified: Vec<String>,
    pub removed: Vec<String>,
}

impl ChangelogEntry {
    fn to_row(&self) -> String {
        let mut row = format!(
            "rev {}: +{} ~{} -{}",
            self.revision,
            self.added.len(),
            self.modified.len(),
            self.removed.len()
        );
        let mut paths: Vec<String> = Vec::new();
        for path in self.added.iter() {
            paths.push(format!("+ {path}"));
        }
        for path in self.modified.iter() {
            paths.push(format!("~ {path}"));
        }
        for path in self.removed.iter() {
            paths.push(format!("- {path}"));
        }
        if !paths.is_empty() {
            row.push_str(&format!("  [{}]", paths.join(", ")));
        }
        row
    }
}

/// The configuration store.
pub struct Config {
    ctx: DefaultContext,
    map: BTreeMap<String, BTreeMap<String, String>>,
    dirty: bool,
    revision: u64,
    changelog: Vec<ChangelogEntry>,
    pending: Vec<Change>,
    observers: Vec<ObserverSlot>,
    next_observer_id: u32,
    dispatching: bool,
    deferred: Vec<Change>,
}

impl Config {
    /// An empty store with the given runtime defaults context.
    #[must_use]
    pub fn new(ctx: DefaultContext) -> Self {
        Config {
            ctx,
            map: BTreeMap::new(),
            dirty: false,
            revision: 0,
            changelog: Vec::new(),
            pending: Vec::new(),
            observers: Vec::new(),
            next_observer_id: 0,
            dispatching: false,
            deferred: Vec::new(),
        }
    }

    /// An empty store with default context.
    #[must_use]
    pub fn default_empty() -> Self {
        Config::new(DefaultContext::default())
    }

    /// The runtime defaults context of this store.
    #[must_use]
    pub fn context(&self) -> &DefaultContext {
        &self.ctx
    }

    /// `true` when any tracked value differs from the last save.
    #[must_use]
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// The current store revision (number of content-changing saves).
    #[must_use]
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// The recorded change-log, oldest first.
    #[must_use]
    pub fn changelog(&self) -> &[ChangelogEntry] {
        &self.changelog
    }

    // ----------------------------------------------------------- typed API

    /// Typed read with default fallback.
    ///
    /// * absent key → the declared default from the certified `keys.h`
    ///   constants, or the neutral value of `T` when the key has none
    ///   (C++ semantics: a missing key reads into a default-constructed
    ///   `QVariant`);
    /// * present key → strict conversion, `Err` on a type mismatch.
    pub fn get<T: FromValue>(&self, key: &Key) -> ConfigResult<T> {
        let path = key.path();
        match self.raw_at(&path) {
            Some(raw) => {
                let value = decode_typed_value(key, raw).map_err(|reason| {
                    ConfigError::TypeMismatch {
                        key: path.to_string(),
                        expected: key.value_type(&self.ctx),
                        found: None,
                        reason,
                    }
                })?;
                T::from_value(&value).map_err(|(expected, found)| {
                    ConfigError::TypeMismatch {
                        key: path.to_string(),
                        expected,
                        found: Some(found),
                        reason: "value slot does not match the requested type"
                            .to_owned(),
                    }
                })
            }
            None => match key.declared_default(&self.ctx) {
                Some(value) => T::from_value(&value).map_err(|(expected, found)| {
                    ConfigError::TypeMismatch {
                        key: path.to_string(),
                        expected,
                        found: Some(found),
                        reason: "declared default has an unexpected shape"
                            .to_owned(),
                    }
                }),
                None => Ok(T::neutral()),
            },
        }
    }

    /// Typed read reporting the raw [`Value`]; `Ok(None)` when the key is
    /// absent.
    pub fn try_get_value(&self, key: &Key) -> ConfigResult<Option<Value>> {
        match self.raw_at(&key.path()) {
            Some(raw) => {
                decode_typed_value(key, raw).map(Some).map_err(|reason| {
                    ConfigError::TypeMismatch {
                        key: key.path().to_string(),
                        expected: key.value_type(&self.ctx),
                        found: None,
                        reason,
                    }
                })
            }
            None => Ok(None),
        }
    }

    /// Typed write. Returns `true` when the stored value changed; setting
    /// an equal value is a no-op (parity with the C++ setters' early
    /// return): no dirty flag, no notification.
    ///
    /// The value's slot type must match the key's declared slot type;
    /// mismatches are reported as [`ConfigError::TypeMismatch`] and never
    /// touch the store.
    pub fn set<T: IntoValue>(&mut self, key: &Key, value: T) -> ConfigResult<bool> {
        self.set_value(key, value.into_value())
    }

    /// [`Config::set`] with an already typed [`Value`].
    pub fn set_value(&mut self, key: &Key, value: Value) -> ConfigResult<bool> {
        let path = key.path();
        let ty = key.value_type(&self.ctx);
        if value.value_type() != ty {
            return Err(ConfigError::TypeMismatch {
                key: path.to_string(),
                expected: ty,
                found: Some(value.value_type()),
                reason: "set value does not match the key's declared type"
                    .to_owned(),
            });
        }
        let raw = encode_value(&value)?;
        let existed = self.contains(&path);
        let changed = self.apply_mutation(&path, Some(raw));
        if changed {
            let change = Change {
                path: path.clone(),
                kind: if existed {
                    ChangeKind::Modified
                } else {
                    ChangeKind::Added
                },
            };
            self.pending.push(change.clone());
            self.dispatch(std::slice::from_ref(&change));
        }
        Ok(changed)
    }

    /// Removes a key; `Ok(true)` when it existed and was removed. This is
    /// the leaf-scope equivalent of `QSettings::remove(path)`.
    pub fn remove(&mut self, key: &Key) -> bool {
        let path = key.path();
        let changed = self.apply_mutation(&path, None);
        if changed {
            let change = Change {
                path: path.clone(),
                kind: ChangeKind::Removed,
            };
            self.pending.push(change.clone());
            self.dispatch(std::slice::from_ref(&change));
        }
        changed
    }

    /// Removes every key of a group (the group scope of
    /// `QSettings::remove`).
    pub fn remove_group(&mut self, group: &str) -> bool {
        let Some(section) = self.map.remove(group) else {
            return false;
        };
        self.dirty = true;
        let events: Vec<Change> = section
            .keys()
            .map(|key| Change {
                path: KeyPath::new(group, key),
                kind: ChangeKind::Removed,
            })
            .collect();
        self.pending.extend(events.iter().cloned());
        self.dispatch(&events);
        true
    }

    // --------------------------------------------------------- raw/API

    /// Raw read of a coordinate in canonical encoding, or `None`.
    #[must_use]
    pub fn raw_at(&self, path: &KeyPath) -> Option<&str> {
        self.map
            .get(&path.group)
            .and_then(|section| section.get(&path.key))
            .map(String::as_str)
    }

    fn contains(&self, path: &KeyPath) -> bool {
        self.map
            .get(&path.group)
            .is_some_and(|section| section.contains_key(&path.key))
    }

    /// Raw access through any `(group, key)` pair, including coordinates
    /// outside the key model (migration data plane, unknown keys).
    /// Decoding follows the key model when the coordinate is one of its
    /// keys, otherwise it is inferred.
    #[must_use]
    pub fn get_at(&self, group: &str, key: &str) -> Option<Value> {
        let raw = self.raw_at(&KeyPath::new(group, key))?;
        match crate::keys::lookup(group, key) {
            Some(typed_key) => decode_typed_value(&typed_key, raw)
                .ok()
                .or_else(|| Some(crate::canonical::decode_value_inferred(raw))),
            None => Some(crate::canonical::decode_value_inferred(raw)),
        }
    }

    /// Raw write through any `(group, key)` pair; used by the migration
    /// data plane. Coordinates inside the key model still enforce their
    /// declared type.
    pub fn put_at(
        &mut self,
        group: &str,
        key: &str,
        value: &Value,
    ) -> ConfigResult<bool> {
        let ty = crate::keys::lookup(group, key).map(|k| k.value_type(&self.ctx));
        if let Some(ty) = ty {
            if value.value_type() != ty {
                return Err(ConfigError::TypeMismatch {
                    key: KeyPath::new(group, key).to_string(),
                    expected: ty,
                    found: Some(value.value_type()),
                    reason: "set value does not match the key's declared type"
                        .to_owned(),
                });
            }
        }
        let path = KeyPath::new(group, key);
        let raw = encode_value(value)?;
        let existed = self.contains(&path);
        let changed = self.apply_mutation(&path, Some(raw));
        if changed {
            let change = Change {
                path: path.clone(),
                kind: if existed {
                    ChangeKind::Modified
                } else {
                    ChangeKind::Added
                },
            };
            self.pending.push(change.clone());
            self.dispatch(std::slice::from_ref(&change));
        }
        Ok(changed)
    }

    /// Removes any `(group, key)` pair; `true` when it existed.
    pub fn remove_at(&mut self, group: &str, key: &str) -> bool {
        let path = KeyPath::new(group, key);
        let changed = self.apply_mutation(&path, None);
        if changed {
            let change = Change {
                path: path.clone(),
                kind: ChangeKind::Removed,
            };
            self.pending.push(change.clone());
            self.dispatch(std::slice::from_ref(&change));
        }
        changed
    }

    /// Group paths currently stored, sorted.
    #[must_use]
    pub fn groups(&self) -> Vec<String> {
        self.map.keys().cloned().collect()
    }

    /// Group paths starting with `prefix` (the `keybind/profiles/`
    /// enumeration `loadKeybindSettings` needs).
    #[must_use]
    pub fn groups_with_prefix(&self, prefix: &str) -> Vec<String> {
        self.map
            .keys()
            .filter(|g| g.starts_with(prefix))
            .cloned()
            .collect()
    }

    /// Applies a raw mutation to the map without change bookkeeping; the
    /// caller (typed API or transaction) performs it.
    fn apply_mutation(&mut self, path: &KeyPath, raw: Option<String>) -> bool {
        let changed = match raw {
            Some(raw) => {
                let section = self.map.entry(path.group.clone()).or_default();
                let changed = section.get(&path.key).map_or(true, |old| old != &raw);
                if changed {
                    section.insert(path.key.clone(), raw);
                }
                changed
            }
            None => {
                let changed = self.contains(path);
                if changed {
                    if let Some(section) = self.map.get_mut(&path.group) {
                        section.remove(&path.key);
                    }
                    if self
                        .map
                        .get(&path.group)
                        .is_some_and(|section| section.is_empty())
                    {
                        self.map.remove(&path.group);
                    }
                }
                changed
            }
        };
        if changed {
            self.dirty = true;
        }
        changed
    }

    // ------------------------------------------------ version / recent files

    /// The settings-file version ([`Key::Version`]); reads default `0` when
    /// absent, matching `Settings::loadVersion`'s
    /// `s.value(Keys::Version::VERSION, 0)`.
    pub fn version(&self) -> ConfigResult<i64> {
        self.get::<i64>(&Key::Version)
    }

    /// The settings-file version. Equal writes are a no-op (parity with
    /// `Settings::setVersion`'s early return).
    pub fn set_version(&mut self, value: i64) -> ConfigResult<bool> {
        self.set(&Key::Version, value)
    }

    /// The recent-file paths, newest first (`[recent] files`).
    pub fn recent_files(&self) -> ConfigResult<Vec<String>> {
        self.get::<Vec<String>>(&Key::Recent(RecentField::Files))
    }

    /// Prepends a recent file, de-duplicating against the existing list and
    /// trimming the tail beyond [`models::RECENT_FILES_MAX_SIZE`].
    ///
    /// The plan comes from the certified
    /// [`models::plan_recent_files_add_with_limit`] model (UTF-16 based, so
    /// the comparison behaves exactly like the C++ `QString` comparison);
    /// this method only translates between the [`Value::Strings`] storage
    /// and the model's UTF-16 views.
    pub fn recent_files_add(&mut self, value: &str) -> ConfigResult<bool> {
        let items = self.recent_files()?;
        let new_val: Vec<u16> = value.encode_utf16().collect();
        let item_views: Vec<Vec<u16>> = items
            .iter()
            .map(|item| item.encode_utf16().collect())
            .collect();
        let surviving = models::plan_recent_files_add_with_limit(
            &item_views,
            &new_val,
            models::RECENT_FILES_MAX_SIZE,
        );
        let mut next = Vec::with_capacity(surviving.len() + 1);
        next.push(value.to_owned());
        for index in surviving {
            next.push(items[index].clone());
        }
        self.set(&Key::Recent(RecentField::Files), next)
    }

    /// Empties the recent-file list (parity with
    /// `Settings::recentFilesClear`); a no-op when the list is already
    /// empty.
    pub fn recent_files_clear(&mut self) -> ConfigResult<bool> {
        if self.recent_files()?.is_empty() {
            return Ok(false);
        }
        self.set(&Key::Recent(RecentField::Files), Vec::<String>::new())
    }

    // --------------------------------------------------------- observers

    /// Registers a synchronous observer; notifications are batched
    /// ([`ChangeSet`]) and re-entrant mutations during dispatch are
    /// deferred to a follow-up turn. Returns a handle for
    /// [`Config::unsubscribe`].
    pub fn subscribe(&mut self, callback: ObserverFn) -> Subscription {
        let id = self.next_observer_id;
        self.next_observer_id += 1;
        self.observers.push(ObserverSlot { id, callback });
        Subscription { id }
    }

    /// Drops an observer; `true` when it existed.
    pub fn unsubscribe(&mut self, subscription: &Subscription) -> bool {
        let before = self.observers.len();
        self.observers.retain(|slot| slot.id != subscription.id);
        self.observers.len() != before
    }

    fn dispatch(&mut self, events: &[Change]) {
        if events.is_empty() || self.observers.is_empty() {
            if self.dispatching {
                self.deferred.extend(events.iter().cloned());
            }
            return;
        }
        if self.dispatching {
            // re-entrant mutation: defer the changes
            self.deferred.extend(events.iter().cloned());
            return;
        }
        self.dispatching = true;
        let set = ChangeSet {
            events: events.to_vec(),
            revision: self.revision,
        };
        let slots = std::mem::take(&mut self.observers);
        for mut slot in slots {
            (slot.callback)(&set);
            self.observers.push(slot);
        }
        self.dispatching = false;
        let deferred = std::mem::take(&mut self.deferred);
        if !deferred.is_empty() {
            self.dispatch(&deferred);
        }
    }

    // ---------------------------------------------------------- documents

    /// Loads the canonical store file `config_dir/file_name`; a missing
    /// file produces an empty store (`QSettings` semantics); unknown keys
    /// are preserved verbatim.
    pub fn load(config_dir: &Path, file_name: &str) -> ConfigResult<Config> {
        let target = config_dir.join(file_name);
        let text = match fs::read_to_string(&target) {
            Ok(text) => text,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Config::default_empty());
            }
            Err(err) => {
                return Err(ConfigError::Io {
                    context: "reading configuration file",
                    source: err,
                })
            }
        };
        let doc = parse_document(&text)?;
        let mut cfg = Config::from_document(doc);
        cfg.dirty = false;
        Ok(cfg)
    }

    /// Builds a store from a parsed canonical document.
    #[must_use]
    pub fn from_document(doc: RawDocument) -> Config {
        let mut cfg = Config::default_empty();
        cfg.revision = doc.revision.unwrap_or(0);
        cfg.map = doc.sections;
        for row in doc.changelog_rows {
            if let Some(entry) = ChangelogEntry::parse_row(&row) {
                cfg.changelog.push(entry);
            }
        }
        cfg
    }

    /// Loads the canonical store file, and when missing falls back to the
    /// legacy QSettings INI `legacy_file_name`, converted in memory into
    /// the canonical representation (the migration path off Qt).
    pub fn load_interop(
        config_dir: &Path,
        file_name: &str,
        legacy_file_name: &str,
    ) -> ConfigResult<Config> {
        let canonical_path = config_dir.join(file_name);
        if canonical_path.exists() {
            return Config::load(config_dir, file_name);
        }
        let legacy_path = config_dir.join(legacy_file_name);
        let text = match fs::read_to_string(&legacy_path) {
            Ok(text) => text,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Config::default_empty());
            }
            Err(err) => {
                return Err(ConfigError::Io {
                    context: "reading legacy configuration file",
                    source: err,
                })
            }
        };
        let legacy = crate::legacy_ini::parse_legacy_ini(&text)?;
        let mut cfg = Config::default_empty();
        for (group, section) in legacy.sections {
            for (key, raw) in section {
                cfg.map
                    .entry(group.clone())
                    .or_default()
                    .insert(key, raw);
            }
        }
        // a converted store is pending its first canonical save
        cfg.dirty = true;
        Ok(cfg)
    }

    /// Saves atomically to `config_dir/file_name` (temporary file +
    /// rename). The directory is created when missing.
    ///
    /// When the content changed since the last save the revision bumps and
    /// one change-log entry covering all pending changes is appended.
    pub fn save(&mut self, config_dir: &Path, file_name: &str) -> ConfigResult<()> {
        if config_dir.exists() && !config_dir.is_dir() {
            return Err(ConfigError::NotADirectory(config_dir.to_path_buf()));
        }
        if !config_dir.exists() {
            fs::create_dir_all(config_dir).map_err(|source| ConfigError::Io {
                context: "creating configuration directory",
                source,
            })?;
        }

        if !self.pending.is_empty() {
            self.revision += 1;
            let entry = summarize(&self.pending, self.revision);
            self.changelog.push(entry);
            self.pending.clear();
            self.dirty = false;
        }

        let document = self.render_document();
        let target = config_dir.join(file_name);
        let temp = config_dir.join(format!("{file_name}.tmp"));
        let mut file = fs::File::create(&temp).map_err(|source| ConfigError::Io {
            context: "creating temporary configuration file",
            source,
        })?;
        {
            use std::io::Write;
            file.write_all(document.as_bytes())
                .map_err(|source| ConfigError::Io {
                    context: "writing configuration content",
                    source,
                })?;
            file.sync_all().map_err(|source| ConfigError::Io {
                context: "flushing configuration content",
                source,
            })?;
        }
        drop(file);
        fs::rename(&temp, &target).map_err(|source| {
            let _ = fs::remove_file(&temp);
            ConfigError::Io {
                context: "atomic rename of configuration file",
                source,
            }
        })?;
        Ok(())
    }

    /// Renders the canonical document (header + sorted groups). Empty
    /// sections are dropped (group-scope `remove` effect).
    #[must_use]
    pub fn render_document(&self) -> String {
        let mut out = String::new();
        out.push_str(FORMAT_MAGIC);
        out.push('\n');
        out.push_str(REVISION_PREFIX);
        out.push_str(&self.revision.to_string());
        out.push('\n');
        if !self.changelog.is_empty() {
            out.push_str(CHANGELOG_PREFIX);
            out.push('\n');
            for entry in &self.changelog {
                out.push_str(CHANGELOG_ROW_PREFIX);
                out.push_str(&entry.to_row());
                out.push('\n');
            }
        }
        out.push('\n');
        for (group, section) in self.map.iter().filter(|(_, s)| !s.is_empty()) {
            out.push('[');
            out.push_str(&percent_encode_component(group));
            out.push_str("]\n");
            for (key, raw) in section {
                out.push_str(&percent_encode_component(key));
                out.push_str(" = ");
                // raw values are already canonical; verbatim keeps saves
                // bit-stable for untouched keys
                out.push_str(raw);
                out.push('\n');
            }
            out.push('\n');
        }
        out
    }

    /// Parses a canonical document text (exposed for tests and standalone
    /// tooling).
    pub fn parse_document_text(text: &str) -> ConfigResult<RawDocument> {
        parse_document(text)
    }
}

/// Net-effect collapse of the pending changes into one change-log entry.
fn summarize(pending: &[Change], revision: u64) -> ChangelogEntry {
    let mut added = Vec::new();
    let mut modified = Vec::new();
    let mut removed = Vec::new();
    for change in pending {
        let path = change.path.to_string();
        match change.kind {
            ChangeKind::Added => {
                added.push(path);
            }
            ChangeKind::Modified => {
                modified.push(path);
            }
            ChangeKind::Removed => {
                removed.retain(|p| p != &path);
                modified.retain(|p| p != &path);
                if !added.contains(&path) {
                    removed.push(path.clone());
                }
                added.retain(|p| p != &path);
            }
        }
    }
    let dedup = |mut xs: Vec<String>| {
        xs.sort();
        xs.dedup();
        xs
    };
    ChangelogEntry {
        revision,
        added: dedup(added),
        modified: dedup(modified),
        removed: dedup(removed),
    }
}

impl ChangelogEntry {
    /// Parses one change-log row back; unrecognized rows yield `None`.
    #[must_use]
    pub fn parse_row(row: &str) -> Option<ChangelogEntry> {
        let rest = row.trim().strip_prefix("rev ")?;
        let (rev, _) = rest.split_once(':')?;
        let revision: u64 = rev.trim().parse().ok()?;
        // paths are informational; only the revision is authoritative
        Some(ChangelogEntry {
            revision,
            added: Vec::new(),
            modified: Vec::new(),
            removed: Vec::new(),
        })
    }
}

// ------------------------------------------------------------ document I/O

/// Parsed canonical document: header metadata plus raw sections.
#[derive(Debug, Default)]
pub struct RawDocument {
    pub revision: Option<u64>,
    /// Verbatim change-log rows (without the `#   ` comment prefix).
    pub changelog_rows: Vec<String>,
    /// Sections, already percent-decoded; values kept as raw strings.
    pub sections: BTreeMap<String, BTreeMap<String, String>>,
}

fn parse_document(text: &str) -> ConfigResult<RawDocument> {
    let mut doc = RawDocument::default();
    let mut first_line = true;
    let mut header_done = false;
    let mut changelog_mode = false;
    let mut current_section: Option<String> = None;
    for (idx, line) in text
        .lines()
        .map(|l| l.strip_suffix('\r').map(str::to_owned).unwrap_or_else(|| l.to_owned()))
        .enumerate()
    {
        let line_no = idx + 1;
        let line = if first_line {
            first_line = false;
            line.strip_prefix('\u{FEFF}')
                .map(str::to_owned)
                .unwrap_or(line)
        } else {
            line
        };
        let trimmed = line.trim();
        if !header_done {
            if trimmed.is_empty() {
                continue;
            }
            if trimmed == FORMAT_MAGIC {
                continue;
            }
            if let Some(rest) = trimmed.strip_prefix(REVISION_PREFIX) {
                doc.revision = rest.trim().parse().ok();
                continue;
            }
            if trimmed == CHANGELOG_PREFIX {
                changelog_mode = true;
                continue;
            }
            if changelog_mode && trimmed.starts_with(CHANGELOG_ROW_PREFIX) {
                doc.changelog_rows
                    .push(trimmed[CHANGELOG_ROW_PREFIX.len()..].to_owned());
                continue;
            }
            if trimmed.starts_with('#') || trimmed.starts_with(';') {
                continue; // other comments: ignored
            }
            header_done = true;
        }
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with(';') {
            continue;
        }
        if trimmed.starts_with('[') {
            let Some(end) = trimmed.find(']') else {
                return Err(ConfigError::Parse {
                    file: "canonical",
                    line: line_no,
                    message: "unterminated section heading".to_owned(),
                });
            };
            let heading = trimmed[1..end].trim();
            let group = percent_decode_component(&heading);
            doc.sections.entry(group.clone()).or_default();
            current_section = Some(group);
            continue;
        }
        let Some(eq) = trimmed.find('=') else {
            return Err(ConfigError::Parse {
                file: "canonical",
                line: line_no,
                message: format!("row without '=': {trimmed:?}"),
            });
        };
        let key = percent_decode_component(trimmed[..eq].trim());
        let value = trimmed[eq + 1..].trim_end();
        let Some(group) = current_section.clone() else {
            return Err(ConfigError::Parse {
                file: "canonical",
                line: line_no,
                message: "key row before any section heading".to_owned(),
            });
        };
        doc.sections.entry(group).or_default().insert(key, value.to_owned());
    }
    Ok(doc)
}

// ------------------------------------------------------------ transactions

/// A batch write window: mutations apply to the store immediately, but are
/// grouped for one observer notification on [`Transaction::commit`] (and
/// one change-log summary at save time). Dropping without committing rolls
/// the mutations back and restores the dirty state.
#[must_use = "transactions are rolled back unless committed"]
pub struct Transaction<'a> {
    cfg: &'a mut Config,
    journal: Vec<Journal>,
    changes: Vec<Change>,
    previous_dirty: bool,
    committed: bool,
}

struct Journal {
    path: KeyPath,
    /// raw value before the transaction touched the coordinate
    previous: Option<String>,
}

impl<'a> Transaction<'a> {
    fn new(cfg: &'a mut Config) -> Self {
        Transaction {
            previous_dirty: cfg.dirty,
            journal: Vec::new(),
            changes: Vec::new(),
            cfg,
            committed: false,
        }
    }

    /// Typed read within the transaction (sees uncommitted writes).
    pub fn get<T: FromValue>(&self, key: &Key) -> ConfigResult<T> {
        self.cfg.get(key)
    }

    /// Typed set within the transaction.
    pub fn set<T: IntoValue>(&mut self, key: &Key, value: T) -> ConfigResult<bool> {
        self.set_value(key, value.into_value())
    }

    /// [`Value`]-flavoured set within the transaction.
    pub fn set_value(&mut self, key: &Key, value: Value) -> ConfigResult<bool> {
        let path = key.path();
        let ty = key.value_type(self.cfg.context());
        if value.value_type() != ty {
            return Err(ConfigError::TypeMismatch {
                key: path.to_string(),
                expected: ty,
                found: Some(value.value_type()),
                reason: "set value does not match the key's declared type"
                    .to_owned(),
            });
        }
        let raw = encode_value(&value)?;
        self.apply(path, Some(raw))
    }

    /// Typed remove within the transaction; `Ok(true)` when the key
    /// existed.
    pub fn remove(&mut self, key: &Key) -> ConfigResult<bool> {
        self.apply(key.path(), None)
    }

    fn apply(&mut self, path: KeyPath, raw: Option<String>) -> ConfigResult<bool> {
        let existed = self.cfg.contains(&path);
        let previous = self.cfg.raw_at(&path).map(str::to_owned);
        let msg_removed = raw.is_none();
        let changed = self.cfg.apply_mutation(&path, raw);
        if changed {
            self.journal.push(Journal {
                path: path.clone(),
                previous,
            });
            self.changes.push(Change {
                path,
                kind: match (existed, msg_removed) {
                    (false, false) => ChangeKind::Added,
                    (true, false) => ChangeKind::Modified,
                    (_, true) => ChangeKind::Removed,
                },
            });
        }
        Ok(changed)
    }

    /// Commits the batch: one observer notification carrying all changed
    /// coordinates; the changes join the pending change-log summary.
    pub fn commit(mut self) -> ChangeSet {
        self.committed = true;
        let events = std::mem::take(&mut self.changes);
        self.cfg.pending.extend(events.iter().cloned());
        self.cfg.dispatch(&events);
        ChangeSet {
            events: events.clone(),
            revision: self.cfg.revision,
        }
    }
}

impl Drop for Transaction<'_> {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        for entry in self.journal.iter().rev() {
            match &entry.previous {
                Some(raw) => {
                    let section =
                        self.cfg.map.entry(entry.path.group.clone()).or_default();
                    section.insert(entry.path.key.clone(), raw.clone());
                }
                None => {
                    if let Some(section) = self.cfg.map.get_mut(&entry.path.group) {
                        section.remove(&entry.path.key);
                    }
                    if self
                        .cfg
                        .map
                        .get(&entry.path.group)
                        .is_some_and(|s| s.is_empty())
                    {
                        self.cfg.map.remove(&entry.path.group);
                    }
                }
            }
        }
        self.cfg.dirty = self.previous_dirty;
    }
}

impl Config {
    pub fn transaction(&mut self) -> Transaction<'_> {
        Transaction::new(self)
    }
}

fn decode_typed_value(key: &Key, raw: &str) -> Result<Value, String> {
    let ty = key.value_type(&DefaultContext::default());
    crate::canonical::decode_value(raw, ty)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::{
        ApplicationField, OnnxField, RecentField, SearchField, SubtitleListField,
        WhisperField,
    };
    use crate::value::ValueType;

    fn test_ctx() -> DefaultContext {
        DefaultContext::new("/home/user")
    }

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    // declared default fallback for typed reads
    #[test]
    fn get_falls_back_to_declared_default() {
        let cfg = Config::new(test_ctx());
        assert_eq!(
            crate::keys::Key::Behavior(crate::keys::BehaviorField::AutoFit)
                .path()
                .group,
            "behavior"
        );
        assert_eq!(
            cfg.get::<bool>(&crate::keys::Key::Behavior(
                crate::keys::BehaviorField::AutoFit
            )),
            Ok(true) // keys.h AUTO_FIT_DEFAULT
        );
        assert_eq!(
            cfg.get::<i64>(&crate::keys::Key::Behavior(
                crate::keys::BehaviorField::OscDuration
            )),
            Ok(700)
        );
        assert_eq!(
            cfg.get::<bool>(&crate::keys::Key::Search(SearchField::HideMpvSubs)),
            Ok(true)
        );
    }

    // absent keys without declared defaults read into neutral values
    #[test]
    fn get_neutral_when_no_default() {
        let cfg = Config::new(test_ctx());
        assert_eq!(cfg.get::<i64>(&Key::Version), Ok(0));
        assert_eq!(
            cfg.get::<Vec<String>>(&Key::Recent(RecentField::Files)),
            Ok(Vec::new())
        );
        assert_eq!(cfg.get::<String>(&Key::KeybindCurrent), Ok(String::new()));
        // environment default comes from the context param
        assert_eq!(
            cfg.get::<String>(&Key::Behavior(crate::keys::BehaviorField::FileOpenCustom)),
            Ok("file:///home/user".to_owned())
        );
    }

    // set + dirty tracking; equal set is a full no-op
    #[test]
    fn set_tracks_dirty_and_dedupes() {
        let mut cfg = Config::new(test_ctx());
        assert!(
            cfg.set(
                &Key::SubtitleList(SubtitleListField::AutoSeek),
                false
            )
            .expect("set")
        );
        assert!(cfg.is_dirty());
        let notifications = std::cell::RefCell::new(0usize);
        let hits = notifications.clone();
        let sub = cfg.subscribe(Box::new(move |_| {
            *hits.borrow_mut() += 1;
        }));
        // setting the same value again: no change, no notify
        assert!(
            !cfg.set(
                &Key::SubtitleList(SubtitleListField::AutoSeek),
                false
            )
            .expect("set")
        );
        assert_eq!(*notifications.borrow(), 0);
        cfg.unsubscribe(&sub);
    }

    // type mismatch is rejected and leaves the store untouched
    #[test]
    fn set_type_mismatch_rejected() {
        let mut cfg = Config::new(test_ctx());
        let err = cfg
            .set_value(
                &Key::Behavior(crate::keys::BehaviorField::OscDuration),
                Value::Text("nope".to_owned()),
            )
            .expect_err("mismatch");
        match err {
            ConfigError::TypeMismatch { expected, found, .. } => {
                assert_eq!(expected, ValueType::Int);
                assert_eq!(found, Some(ValueType::Text));
            }
            other => panic!("unexpected error: {other}"),
        }
        assert_eq!(
            cfg.get::<i64>(&Key::Behavior(crate::keys::BehaviorField::OscDuration)),
            Ok(700)
        );
        assert!(!cfg.is_dirty());
    }

    // save + load round trip incl. unknown keys and tricky strings
    #[test]
    fn save_load_round_trip_unknown_keys() {
        let dir = scratch("memento-config-roundtrip");
        let mut cfg = Config::new(test_ctx());
        cfg.set(
            &Key::Whisper(WhisperField::SuppressRegex),
            r"^\[_x_[0-9]+\]$",
        )
        .expect("set");
        cfg.set(
            &Key::KeybindProfile {
                profile: "My [1] Prof".to_owned().into_boxed_str(),
                field: crate::keys::KeybindProfileField::CardAdd,
            },
            "Ctrl+=[\ntab=\t",
        )
        .expect("profile set");
        cfg.put_at(
            "search",
            "totally-unknown",
            &Value::Text("keep=me;#yes".to_owned()),
        )
        .expect("put");
        cfg.save(&dir, "memento.conf").expect("save");

        let reloaded = Config::load(&dir, "memento.conf").expect("load");
        assert_eq!(
            reloaded
                .get::<String>(&Key::Whisper(WhisperField::SuppressRegex))
                .expect("regex"),
            r"^\[_x_[0-9]+\]$"
        );
        assert_eq!(
            reloaded
                .get::<String>(&Key::KeybindProfile {
                    profile: "My [1] Prof".into(),
                    field: crate::keys::KeybindProfileField::CardAdd,
                })
                .expect("profile"),
            "Ctrl+=[\ntab=\t"
        );
        assert_eq!(
            reloaded.get_at("search", "totally-unknown"),
            Some(Value::Text("keep=me;#yes".to_owned()))
        );
        assert!(!reloaded.is_dirty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    // canonical ordering: groups then keys, lexicographic
    #[test]
    fn canonical_ordering() {
        let mut cfg = Config::new(test_ctx());
        cfg.set(&Key::Search(SearchField::Delay), 250).expect("s");
        cfg.set(&Key::Search(SearchField::Modifier), 2).expect("s");
        cfg.set(&Key::Onnx(OnnxField::Threads), 4).expect("s");
        cfg.set(&Key::Application(ApplicationField::Language), 1)
            .expect("s");
        // insert a group that sorts before an existing one
        cfg.put_at("audio-srcs/1", "name", &Value::Text("x".to_owned()))
            .expect("put");
        let doc = cfg.render_document();
        let groups: Vec<String> = doc
            .lines()
            .filter_map(|l| {
                l.strip_prefix('[')
                    .map(|s| s.trim_end_matches(']').to_owned())
            })
            .collect();
        let mut sorted = groups.clone();
        sorted.sort();
        assert_eq!(groups, sorted, "groups must render sorted");
        assert!(doc.contains("[application]"));
        assert!(doc.contains("[audio-srcs/1]"));
        assert!(doc.contains("[onnx]"));
        assert!(doc.contains("[search]"));
        // inside a group keys are sorted too (delay < modifier in search)
        let start = doc.find("[search]").expect("search section");
        let tail = &doc[start..];
        let delay_at = tail.find("delay = ").expect("delay row");
        let modifier_at = tail.find("modifier = ").expect("modifier row");
        assert!(delay_at < modifier_at);
    }

    // atomicity failure path: an unwritable dir leaves the target intact
    #[test]
    fn atomic_save_failure_path() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch("memento-config-fail");
        std::fs::create_dir_all(&dir).expect("mkdir");
        let target = dir.join("memento.conf");
        std::fs::write(&target, b"# previous state\n").expect("seed");
        // probe: skip when permissions cannot actually block us (root)
        let probe = dir.join(".probe");
        if std::fs::write(&probe, b"").is_ok() {
            let _ = std::fs::remove_file(&probe);
            let _ = std::fs::remove_dir_all(&dir);
            return;
        }
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o500))
            .expect("chmod read-only");
        let mut cfg = Config::new(test_ctx());
        cfg.set(&Key::Search(SearchField::Delay), 1).expect("set");
        let result = cfg.save(&dir, "memento.conf");
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755))
            .expect("restore permissions");
        assert!(result.is_err(), "expected an io error, got {result:?}");
        let contents = std::fs::read_to_string(&target).expect("read");
        assert_eq!(contents, "# previous state\n", "target must be untouched");
        let leaked: Vec<_> = std::fs::read_dir(&dir)
            .expect("readdir")
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leaked.is_empty(), "temp file leaked: {leaked:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    // the change-log sums up all pending changes per save
    #[test]
    fn changelog_accumulates() {
        let dir = scratch("memento-config-log");
        let mut cfg = Config::new(test_ctx());
        cfg.set(&Key::Search(SearchField::Delay), 100).expect("s");
        cfg.set(&Key::Search(SearchField::Delay), 200).expect("s");
        cfg.put_at("search", "gone", &Value::Bool(true)).expect("put");
        cfg.save(&dir, "memento.conf").expect("save");
        assert_eq!(cfg.revision(), 1);

        cfg.set(&Key::Search(SearchField::Delay), 210).expect("s");
        cfg.save(&dir, "memento.conf").expect("save");
        assert_eq!(cfg.revision(), 2);

        let entries = cfg.changelog();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].revision, 1);
        assert!(entries[0].modified.contains(&"search/delay".to_owned()));
        assert!(entries[0].added.contains(&"search/gone".to_owned()));
        assert_eq!(entries[1].revision, 2);
        assert_eq!(entries[1].modified.len(), 1);

        // a reloaded store carries the same history and revision
        let reloaded = Config::load(&dir, "memento.conf").expect("load");
        assert_eq!(reloaded.revision(), 2);
        assert_eq!(reloaded.changelog().len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    // transaction: commit notifies once with all changes; rollback restores
    #[test]
    fn transaction_commit_rollback() {
        let mut cfg = Config::new(test_ctx());
        let notifications: std::sync::Arc<std::sync::Mutex<Vec<Vec<String>>>> =
            std::sync::Arc::default();
        let sink = notifications.clone();
        let _sub = cfg.subscribe(Box::new(move |set: &ChangeSet| {
            sink.lock().expect("locked").push(
                set.events.iter().map(|e| e.to_string()).collect(),
            );
        }));
        // seed a value so the rollback has something non-trivial to restore
        cfg.set(&Key::Search(SearchField::Delay), 210).expect("s");
        {
            let mut tx = cfg.transaction();
            tx.set(&Key::Search(SearchField::Delay), 9).expect("set");
            tx.set(&Key::Whisper(WhisperField::BeamSize), 3).expect("set");
            tx.commit();
        }
        assert_eq!(
            cfg.get::<i64>(&Key::Search(SearchField::Delay)),
            Ok(9)
        );
        let batched = notifications.lock().expect("locked");
        assert_eq!(batched.len(), 2, "seed + commit = 2 notifications");
        assert_eq!(
            batched[1].len(),
            2,
            "both changes in one batch: {:?}",
            batched[1]
        );
        drop(batched);

        // rollback path: no commit, no notification, values restored
        {
            let mut tx = cfg.transaction();
            tx.set(&Key::Search(SearchField::Method), 1)
                .expect("set (would be an addition)");
            tx.remove(&Key::Search(SearchField::Delay)).expect("remove");
        }
        assert_eq!(
            cfg.get::<i64>(&Key::Search(SearchField::Delay)),
            Ok(9),
            "rollback restored the seeded value"
        );
        assert!(
            cfg.get::<i64>(&Key::Search(SearchField::Method)).is_ok(),
            "absent key falls back to default"
        );
        assert_eq!(notifications.lock().expect("locked").len(), 2);
    }

    // subscriptions are removable; observers receive sets and removals
    #[test]
    fn subscribe_unsubscribe_and_kinds() {
        let mut cfg = Config::new(test_ctx());
        let k: std::sync::Arc<std::sync::Mutex<Vec<String>>> = std::sync::Arc::default();
        let sink = k.clone();
        let sub = cfg.subscribe(Box::new(move |set: &ChangeSet| {
            for event in &set.events {
                sink.lock().expect("locked").push(event.to_string());
            }
        }));
        cfg.set(&Key::Version, 5).expect("add");
        cfg.remove(&Key::Version);
        assert_eq!(
            k.lock().expect("locked")[0],
            "+ general/version"
        );
        assert_eq!(
            k.lock().expect("locked")[1],
            "- general/version"
        );
        assert!(cfg.unsubscribe(&sub));
        assert!(!cfg.unsubscribe(&sub));
        cfg.set(&Key::Version, 7).expect("set after unsubscribe");
        assert_eq!(k.lock().expect("locked").len(), 2);
    }

    // remove_group clears whole groups (QSettings::remove(group) parity)
    #[test]
    fn remove_group_clears_the_section() {
        let mut cfg = Config::new(test_ctx());
        cfg.set(&Key::Search(SearchField::Delay), 5).expect("s");
        cfg.set(&Key::Search(SearchField::Modifier), 0).expect("s");
        assert!(cfg.remove_group("search"));
        assert!(!cfg.remove_group("search"));
        assert!(
            !cfg.map.contains_key("search"),
            "empty sections collapse"
        );
        assert!(cfg.groups().is_empty());
    }
}

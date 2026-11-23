use std::collections::HashMap;
use std::error::Error;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, Weak};

use subomi_dict::sqlite_db::{self, rusqlite};
use subomi_dict::yomidbbuilder::{self, YomiError};

use crate::controller::dict::invalidate_shared_readonly_db;

/// An installed dictionary row read from Subomi's Yomitan database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledDictionary {
    pub id: i64,
    pub title: String,
    pub disabled: bool,
}

/// Errors returned by dictionary database setup and management operations.
#[derive(Debug)]
pub enum DictionaryManagerError {
    InvalidPath { path: PathBuf, reason: String },
    Io {
        operation: &'static str,
        path: PathBuf,
        source: std::io::Error,
    },
    Yomi {
        operation: &'static str,
        source: YomiError,
    },
    Sqlite(sqlite_db::DictError),
    InvalidDictionaryId(i64),
    DictionaryNotFound(i64),
    MutationLockPoisoned,
}

impl fmt::Display for DictionaryManagerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPath { path, reason } => {
                write!(f, "invalid path '{}': {reason}", path.display())
            }
            Self::Io {
                operation,
                path,
                source,
            } => write!(f, "could not {operation} '{}': {source}", path.display()),
            Self::Yomi { operation, source } => {
                write!(f, "could not {operation}: {source}")
            }
            Self::Sqlite(error) => write!(f, "{error}"),
            Self::InvalidDictionaryId(id) => {
                write!(f, "dictionary ID {id} is invalid")
            }
            Self::DictionaryNotFound(id) => write!(f, "dictionary {id} is not installed"),
            Self::MutationLockPoisoned => write!(f, "dictionary manager mutation lock is poisoned"),
        }
    }
}

impl Error for DictionaryManagerError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Yomi { source, .. } => Some(source),
            Self::Sqlite(error) => Some(error),
            _ => None,
        }
    }
}

fn shared_mutation_lock(database_path: &Path) -> Arc<Mutex<()>> {
    static LOCKS: OnceLock<Mutex<HashMap<PathBuf, Weak<Mutex<()>>>>> = OnceLock::new();
    let registry = LOCKS.get_or_init(|| Mutex::new(HashMap::new()));
    let key = std::fs::canonicalize(database_path).unwrap_or_else(|_| database_path.to_path_buf());
    let mut map = registry
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(existing) = map.get(&key).and_then(Weak::upgrade) {
        return existing;
    }
    if map.len() > 32 {
        map.retain(|_, weak| weak.strong_count() > 0);
    }
    let lock = Arc::new(Mutex::new(()));
    map.insert(key, Arc::downgrade(&lock));
    lock
}

struct DictionaryManagerInner {
    database_path: PathBuf,
    resources_path: PathBuf,
    mutation_lock: Arc<Mutex<()>>,
}

/// Owns the Yomi database operations used by the dictionary settings.
/// Cheaply cloneable via `Arc` with per-database cross-thread mutation serialization.
#[derive(Clone)]
pub struct DictionaryManager {
    inner: Arc<DictionaryManagerInner>,
}

impl DictionaryManager {
    /// Prepare the Yomitan database and resource directory at the supplied paths.
    pub fn new(
        database_path: impl AsRef<Path>,
        resources_path: impl AsRef<Path>,
    ) -> Result<Self, DictionaryManagerError> {
        let database_path = validate_path(database_path.as_ref())?;
        let resources_path = validate_path(resources_path.as_ref())?;

        if let Some(parent) = database_path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent).map_err(|source| DictionaryManagerError::Io {
                operation: "create the dictionary database directory",
                path: parent.to_path_buf(),
                source,
            })?;
        }
        std::fs::create_dir_all(&resources_path).map_err(|source| DictionaryManagerError::Io {
            operation: "create the dictionary resources directory",
            path: resources_path.clone(),
            source,
        })?;

        yomidbbuilder::prepare_db(&database_path).map_err(|source| DictionaryManagerError::Yomi {
            operation: "prepare the dictionary database",
            source,
        })?;

        let mutation_lock = shared_mutation_lock(&database_path);
        Ok(Self {
            inner: Arc::new(DictionaryManagerInner {
                database_path,
                resources_path,
                mutation_lock,
            }),
        })
    }

    /// Returns a shared `Arc<Self>` handle for passing into background worker threads.
    pub fn into_shared(self) -> Arc<Self> {
        Arc::new(self)
    }

    /// Read the installed dictionary rows from the database, including each
    /// dictionary's current enabled/disabled state.
    pub fn list_dictionaries(&self) -> Result<Vec<InstalledDictionary>, DictionaryManagerError> {
        let _guard = self
            .inner
            .mutation_lock
            .lock()
            .map_err(|_| DictionaryManagerError::MutationLockPoisoned)?;
        self.list_dictionaries_unlocked()
    }

    fn list_dictionaries_unlocked(
        &self,
    ) -> Result<Vec<InstalledDictionary>, DictionaryManagerError> {
        let conn = sqlite_db::open_read_only_connection(&self.inner.database_path)
            .map_err(DictionaryManagerError::Sqlite)?;
        let mut statement = conn
            .prepare(
                "SELECT directory.dic_id, directory.title, \
                 EXISTS (SELECT 1 FROM dict_disabled WHERE dict_disabled.dic_id = directory.dic_id) \
                 FROM directory ORDER BY directory.dic_id;",
            )
            .map_err(|err| DictionaryManagerError::Sqlite(err.into()))?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)? != 0,
                ))
            })
            .map_err(|err| DictionaryManagerError::Sqlite(err.into()))?;

        let mut dictionaries = Vec::new();
        for row_res in rows {
            let (id, title, disabled) =
                row_res.map_err(|err| DictionaryManagerError::Sqlite(err.into()))?;
            if id <= 0 {
                return Err(DictionaryManagerError::InvalidDictionaryId(id));
            }
            dictionaries.push(InstalledDictionary {
                id,
                title,
                disabled,
            });
        }
        Ok(dictionaries)
    }

    /// Import a Yomichan/Yomitan ZIP archive and return the refreshed list.
    pub fn add_dictionary(
        &self,
        archive_path: impl AsRef<Path>,
    ) -> Result<Vec<InstalledDictionary>, DictionaryManagerError> {
        let archive_path = validate_path(archive_path.as_ref())?;
        let _guard = self
            .inner
            .mutation_lock
            .lock()
            .map_err(|_| DictionaryManagerError::MutationLockPoisoned)?;
        yomidbbuilder::process_dictionary(
            &archive_path,
            &self.inner.database_path,
            &self.inner.resources_path,
        )
        .map_err(|source| DictionaryManagerError::Yomi {
            operation: "add the dictionary archive",
            source,
        })?;
        invalidate_shared_readonly_db(&self.inner.database_path);
        self.list_dictionaries_unlocked()
    }

    /// Enable an installed dictionary and return the refreshed list.
    pub fn enable_dictionary(
        &self,
        id: i64,
    ) -> Result<Vec<InstalledDictionary>, DictionaryManagerError> {
        self.set_dictionary_enabled(id, true)
    }

    /// Disable an installed dictionary and return the refreshed list.
    pub fn disable_dictionary(
        &self,
        id: i64,
    ) -> Result<Vec<InstalledDictionary>, DictionaryManagerError> {
        self.set_dictionary_enabled(id, false)
    }

    /// Delete an installed dictionary and its extracted resources, returning
    /// the refreshed list.
    pub fn delete_dictionary(
        &self,
        id: i64,
    ) -> Result<Vec<InstalledDictionary>, DictionaryManagerError> {
        let _guard = self
            .inner
            .mutation_lock
            .lock()
            .map_err(|_| DictionaryManagerError::MutationLockPoisoned)?;
        self.require_installed(id)?;
        yomidbbuilder::delete_dictionary(
            id,
            &self.inner.database_path,
            &self.inner.resources_path,
        )
        .map_err(|source| DictionaryManagerError::Yomi {
            operation: "delete the dictionary",
            source,
        })?;
        invalidate_shared_readonly_db(&self.inner.database_path);
        self.list_dictionaries_unlocked()
    }

    fn set_dictionary_enabled(
        &self,
        id: i64,
        enabled: bool,
    ) -> Result<Vec<InstalledDictionary>, DictionaryManagerError> {
        let _guard = self
            .inner
            .mutation_lock
            .lock()
            .map_err(|_| DictionaryManagerError::MutationLockPoisoned)?;
        self.require_installed(id)?;
        let result = if enabled {
            yomidbbuilder::enable_dictionary(id, &self.inner.database_path)
        } else {
            yomidbbuilder::disable_dictionary(id, &self.inner.database_path)
        };
        result.map_err(|source| DictionaryManagerError::Yomi {
            operation: if enabled {
                "enable the dictionary"
            } else {
                "disable the dictionary"
            },
            source,
        })?;
        invalidate_shared_readonly_db(&self.inner.database_path);
        self.list_dictionaries_unlocked()
    }

    fn require_installed(&self, id: i64) -> Result<(), DictionaryManagerError> {
        if id <= 0 {
            return Err(DictionaryManagerError::InvalidDictionaryId(id));
        }
        let conn = sqlite_db::open_read_only_connection(&self.inner.database_path)
            .map_err(DictionaryManagerError::Sqlite)?;
        let mut statement = conn
            .prepare("SELECT 1 FROM directory WHERE dic_id = ?1 LIMIT 1;")
            .map_err(|err| DictionaryManagerError::Sqlite(err.into()))?;
        let exists = statement
            .exists(rusqlite::params![id])
            .map_err(|err| DictionaryManagerError::Sqlite(err.into()))?;
        if exists {
            Ok(())
        } else {
            Err(DictionaryManagerError::DictionaryNotFound(id))
        }
    }
}

fn validate_path(path: &Path) -> Result<PathBuf, DictionaryManagerError> {
    if path.as_os_str().is_empty() {
        return Err(DictionaryManagerError::InvalidPath {
            path: path.to_path_buf(),
            reason: "path is empty".to_owned(),
        });
    }
    Ok(path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{self, File};
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct TempDirGuard(PathBuf);

    impl TempDirGuard {
        fn new() -> Self {
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let dir = std::env::temp_dir().join(format!(
                "subomi-dictmanager-test-{}-{}",
                std::process::id(),
                COUNTER.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for TempDirGuard {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

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

    fn write_stored_zip(path: &Path, entries: &[(&str, &[u8])]) {
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

        let mut file = File::create(path).unwrap();
        file.write_all(&buf).unwrap();
    }

    #[test]
    fn dictionary_manager_lifecycle_add_toggle_and_delete() {
        let tmp = TempDirGuard::new();
        let db_path = tmp.0.join("dictionaries.sqlite");
        let res_dir = tmp.0.join("res");
        let zip_path = tmp.0.join("sample.zip");
        write_stored_zip(
            &zip_path,
            &[
                (
                    "index.json",
                    br#"{"title":"SampleMgr","format":3,"revision":"1"}"#,
                ),
                (
                    "term_bank_1.json",
                    r#"[["水","みず","","",10,["water"],1,""]]"#.as_bytes(),
                ),
                ("styles.css", b"body{}"),
            ],
        );

        let manager = DictionaryManager::new(&db_path, &res_dir).unwrap();
        assert!(manager.list_dictionaries().unwrap().is_empty());

        let added = manager.add_dictionary(&zip_path).unwrap();
        assert_eq!(added.len(), 1);
        assert_eq!(added[0].title, "SampleMgr");
        assert!(!added[0].disabled);
        let id = added[0].id;

        let disabled = manager.disable_dictionary(id).unwrap();
        assert!(disabled[0].disabled);

        let enabled = manager.enable_dictionary(id).unwrap();
        assert!(!enabled[0].disabled);

        assert!(matches!(
            manager.delete_dictionary(999_999),
            Err(DictionaryManagerError::DictionaryNotFound(999_999))
        ));
        assert!(matches!(
            manager.disable_dictionary(0),
            Err(DictionaryManagerError::InvalidDictionaryId(0))
        ));

        let after_delete = manager.delete_dictionary(id).unwrap();
        assert!(after_delete.is_empty());
        assert!(!res_dir.join(format!("dict-{id}")).exists());
    }
}

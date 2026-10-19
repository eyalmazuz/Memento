use std::error::Error;
use std::ffi::{CStr, CString};
use std::fmt;
use std::path::{Path, PathBuf};
use std::ptr;
use std::sync::Mutex;

use memento_dict::{sqlite_db, yomidbbuilder};

/// An installed dictionary row read from Memento's Yomitan database.
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
    Yomi { operation: &'static str, code: i32 },
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
            Self::Yomi { operation, code } => {
                write!(f, "could not {operation} (Yomi error {code})")
            }
            Self::Sqlite(error) => write!(f, "{error}"),
            Self::InvalidDictionaryId(id) => {
                write!(f, "dictionary ID {id} cannot be represented by the Yomi API")
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
            Self::Sqlite(error) => Some(error),
            _ => None,
        }
    }
}

struct ReadOnlyDatabase(*mut sqlite_db::sqlite3);

impl Drop for ReadOnlyDatabase {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { sqlite_db::sqlite3_close_v2(self.0) };
        }
    }
}

/// Owns the Yomi database operations used by the dictionary settings.
/// Dictionary ordering is deliberately left to the caller's configuration.
pub struct DictionaryManager {
    database_path_c: CString,
    resources_path_c: CString,
    mutation_lock: Mutex<()>,
}

impl DictionaryManager {
    /// Prepare the Yomitan database and resource directory at the supplied paths.
    pub fn new(
        database_path: impl AsRef<Path>,
        resources_path: impl AsRef<Path>,
    ) -> Result<Self, DictionaryManagerError> {
        let database_path = database_path.as_ref().to_path_buf();
        let resources_path = resources_path.as_ref().to_path_buf();
        let database_path_c = path_to_cstring(&database_path)?;
        let resources_path_c = path_to_cstring(&resources_path)?;

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

        let code = unsafe {
            yomidbbuilder::memento_dict_yomi_prepare_db(database_path_c.as_ptr(), ptr::null_mut())
        };
        if code != 0 {
            return Err(DictionaryManagerError::Yomi {
                operation: "prepare the dictionary database",
                code,
            });
        }

        Ok(Self {
            database_path_c,
            resources_path_c,
            mutation_lock: Mutex::new(()),
        })
    }

    /// Read the installed dictionary rows from the database, including each
    /// dictionary's current enabled/disabled state.
    pub fn list_dictionaries(&self) -> Result<Vec<InstalledDictionary>, DictionaryManagerError> {
        let _guard = self
            .mutation_lock
            .lock()
            .map_err(|_| DictionaryManagerError::MutationLockPoisoned)?;
        self.list_dictionaries_unlocked()
    }

    fn list_dictionaries_unlocked(
        &self,
    ) -> Result<Vec<InstalledDictionary>, DictionaryManagerError> {
        let database = open_read_only(&self.database_path_c)?;
        let mut statement = sqlite_db::Statement::prepare(
            database.0,
            "SELECT directory.dic_id, directory.title, \
             EXISTS (SELECT 1 FROM dict_disabled WHERE dict_disabled.dic_id = directory.dic_id) \
             FROM directory ORDER BY directory.dic_id;",
        )
        .map_err(DictionaryManagerError::Sqlite)?;
        let mut dictionaries = Vec::new();
        while statement.step().map_err(DictionaryManagerError::Sqlite)? {
            let id = statement.column_int64(0);
            if id <= 0 || i32::try_from(id).is_err() {
                return Err(DictionaryManagerError::InvalidDictionaryId(id));
            }
            dictionaries.push(InstalledDictionary {
                id,
                title: statement.column_text_string(1),
                disabled: statement.column_int(2) != 0,
            });
        }
        Ok(dictionaries)
    }

    /// Import a Yomichan/Yomitan ZIP archive and return the refreshed list.
    pub fn add_dictionary(
        &self,
        archive_path: impl AsRef<Path>,
    ) -> Result<Vec<InstalledDictionary>, DictionaryManagerError> {
        let archive_path = path_to_cstring(archive_path.as_ref())?;
        let _guard = self
            .mutation_lock
            .lock()
            .map_err(|_| DictionaryManagerError::MutationLockPoisoned)?;
        let code = unsafe {
            yomidbbuilder::memento_dict_yomi_process_dictionary(
                archive_path.as_ptr(),
                self.database_path_c.as_ptr(),
                self.resources_path_c.as_ptr(),
            )
        };
        if code != 0 {
            return Err(DictionaryManagerError::Yomi {
                operation: "add the dictionary archive",
                code,
            });
        }
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
            .mutation_lock
            .lock()
            .map_err(|_| DictionaryManagerError::MutationLockPoisoned)?;
        self.require_installed(id)?;
        let code = unsafe {
            yomidbbuilder::memento_dict_yomi_delete_dictionary(
                id,
                self.database_path_c.as_ptr(),
                self.resources_path_c.as_ptr(),
            )
        };
        if code != 0 {
            return Err(DictionaryManagerError::Yomi {
                operation: "delete the dictionary",
                code,
            });
        }
        self.list_dictionaries_unlocked()
    }

    fn set_dictionary_enabled(
        &self,
        id: i64,
        enabled: bool,
    ) -> Result<Vec<InstalledDictionary>, DictionaryManagerError> {
        let _guard = self
            .mutation_lock
            .lock()
            .map_err(|_| DictionaryManagerError::MutationLockPoisoned)?;
        self.require_installed(id)?;
        let code = unsafe {
            if enabled {
                yomidbbuilder::memento_dict_yomi_enable_dictionary(id, self.database_path_c.as_ptr())
            } else {
                yomidbbuilder::memento_dict_yomi_disable_dictionary(id, self.database_path_c.as_ptr())
            }
        };
        if code != 0 {
            return Err(DictionaryManagerError::Yomi {
                operation: if enabled {
                    "enable the dictionary"
                } else {
                    "disable the dictionary"
                },
                code,
            });
        }
        self.list_dictionaries_unlocked()
    }

    fn require_installed(&self, id: i64) -> Result<(), DictionaryManagerError> {
        if id <= 0 || i32::try_from(id).is_err() {
            return Err(DictionaryManagerError::InvalidDictionaryId(id));
        }
        let database = open_read_only(&self.database_path_c)?;
        let query = format!("SELECT 1 FROM directory WHERE dic_id = {id} LIMIT 1;");
        let mut statement = sqlite_db::Statement::prepare(database.0, &query)
            .map_err(DictionaryManagerError::Sqlite)?;
        if statement.step().map_err(DictionaryManagerError::Sqlite)? {
            Ok(())
        } else {
            Err(DictionaryManagerError::DictionaryNotFound(id))
        }
    }
}

fn open_read_only(path: &CString) -> Result<ReadOnlyDatabase, DictionaryManagerError> {
    const SQLITE_OPEN_READONLY: i32 = 0x0000_0001;
    const SQLITE_OPEN_FULLMUTEX: i32 = 0x0001_0000;
    let mut handle = ptr::null_mut();
    let status = unsafe {
        sqlite_db::sqlite3_open_v2(
            path.as_ptr(),
            &mut handle,
            SQLITE_OPEN_READONLY | SQLITE_OPEN_FULLMUTEX,
            ptr::null(),
        )
    };
    if status != sqlite_db::SQLITE_OK {
        let message = if handle.is_null() {
            "could not open database".to_owned()
        } else {
            let error = unsafe { sqlite_db::sqlite3_errmsg(handle) };
            if error.is_null() {
                "could not open database".to_owned()
            } else {
                unsafe { CStr::from_ptr(error) }
                    .to_string_lossy()
                    .into_owned()
            }
        };
        if !handle.is_null() {
            unsafe { sqlite_db::sqlite3_close_v2(handle) };
        }
        return Err(DictionaryManagerError::Sqlite(
            sqlite_db::DictError::Sqlite {
                code: status,
                message,
            },
        ));
    }
    Ok(ReadOnlyDatabase(handle))
}

fn path_to_cstring(path: &Path) -> Result<CString, DictionaryManagerError> {
    if path.as_os_str().is_empty() {
        return Err(DictionaryManagerError::InvalidPath {
            path: path.to_path_buf(),
            reason: "path is empty".to_owned(),
        });
    }
    #[cfg(unix)]
    let bytes = {
        use std::os::unix::ffi::OsStrExt;
        path.as_os_str().as_bytes()
    };
    #[cfg(not(unix))]
    let bytes = path
        .to_str()
        .ok_or_else(|| DictionaryManagerError::InvalidPath {
            path: path.to_path_buf(),
            reason: "path is not valid UTF-8".to_owned(),
        })?
        .as_bytes();

    CString::new(bytes).map_err(|error| DictionaryManagerError::InvalidPath {
        path: path.to_path_buf(),
        reason: error.to_string(),
    })
}

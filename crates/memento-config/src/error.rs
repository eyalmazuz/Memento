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

//! Error types for the `memento-config` persistence engine.

use std::fmt;
use std::path::PathBuf;

use crate::value::ValueType;

/// Result alias used across the crate.
pub type ConfigResult<T> = Result<T, ConfigError>;

/// Everything the store can report while loading, decoding, typing or
/// persisting configuration data.
///
/// Parse errors carry the originating file *kind* (canonical store vs. legacy
/// QSettings INI) and the 1-based line number; they never contain a resolved
/// filesystem path (paths are supplied by the caller at call time).
#[derive(Debug)]
pub enum ConfigError {
    /// `std::io` failure, e.g. while creating the temporary file or renaming
    /// it into place during an atomic save.
    Io {
        context: &'static str,
        source: std::io::Error,
    },
    /// The directory handed to [`crate::config::Config::load`] /
    /// [`crate::config::Config::save`] exists but is not a directory.
    NotADirectory(PathBuf),
    /// Syntax error while parsing a document.
    Parse {
        file: &'static str,
        line: usize,
        message: String,
    },
    /// A value does not have the type declared for its key.
    TypeMismatch {
        key: String,
        expected: ValueType,
        found: Option<ValueType>,
        reason: String,
    },
    /// The store file names a document magic it cannot read.
    Format {
        message: String,
    },
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConfigError::Io { context, source } => {
                write!(f, "{context}: {source}")
            }
            ConfigError::NotADirectory(path) => {
                write!(f, "not a directory: {}", path.display())
            }
            ConfigError::Parse {
                file,
                line,
                message,
            } => write!(f, "{file}: line {line}: {message}"),
        ConfigError::TypeMismatch {
                key,
                expected,
                found,
                reason,
            } => {
                let found = match found {
                    Some(vt) => vt.name().to_owned(),
                    None => "malformed value".to_owned(),
                };
                write!(
                    f,
                    "{key}: expected type {expected}, found {found}: {reason}"
                )
            }
            ConfigError::Format { message } => write!(f, "format error: {message}"),
        }
    }
}

impl std::error::Error for ConfigError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ConfigError::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// Structural equality for tests and callers; `io` errors compare by their
/// `Display` text (the OS layer has no identity to compare).
impl PartialEq for ConfigError {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (ConfigError::Io { context: a, source: sa }, ConfigError::Io { context: b, source: sb }) => {
                a == b && sa.to_string() == sb.to_string()
            }
            (ConfigError::NotADirectory(a), ConfigError::NotADirectory(b)) => a == b,
            (
                ConfigError::Parse { file: fa, line: la, message: ma },
                ConfigError::Parse { file: fb, line: lb, message: mb },
            ) => fa == fb && la == lb && ma == mb,
            (
                ConfigError::TypeMismatch { key: ka, expected: ea, found: fa, reason: ra },
                ConfigError::TypeMismatch { key: kb, expected: eb, found: fb, reason: rb },
            ) => ka == kb && ea == eb && fa == fb && ra == rb,
            (ConfigError::Format { message: a }, ConfigError::Format { message: b }) => a == b,
            _ => false,
        }
    }
}

impl From<std::io::Error> for ConfigError {
    fn from(source: std::io::Error) -> Self {
        // io error where no more specific context is attached
        ConfigError::Io {
            context: "io error",
            source,
        }
    }
}

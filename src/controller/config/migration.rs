//! Canonical config file loading and legacy QSettings INI migration.

use std::fs;
use std::path::Path;

use memento_config::canonical::FORMAT_MAGIC;
use memento_config::config::{Config, RawDocument};
use memento_config::{parse_legacy_ini, Value};
use memento_setting::keys as raw;
use memento_setting::migration::{migrate_2_to_3_list_result, migration_plan};

/// Loads `config_dir/file_name`, reading the file at most once.
///
/// Canonical `memento-config-store` files are parsed directly from the in-memory
/// buffer via [`Config::from_document_text`]; legacy QSettings INI files are
/// converted and migrated in memory.
pub(crate) fn load_config_store(config_dir: &Path, file_name: &str) -> Result<Config, String> {
    let path = config_dir.join(file_name);
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Config::default_empty());
        }
        Err(error) => return Err(format!("could not read settings: {error}")),
    };
    let clean = text.strip_prefix('\u{feff}').unwrap_or(&text).trim_start();
    if clean.starts_with(FORMAT_MAGIC) {
        return Config::from_document_text(&text).map_err(|error| format!("{error:?}"));
    }
    let legacy = parse_legacy_ini(&text).map_err(|error| format!("{error:?}"))?;
    let mut config = Config::from_document(RawDocument {
        revision: Some(0),
        changelog_rows: Vec::new(),
        sections: legacy.sections,
    });
    migrate_legacy_config(&mut config);
    Ok(config)
}

pub(crate) fn migrate_legacy_config(config: &mut Config) {
    let raw_version = config.version().unwrap_or(0);
    let version = u32::try_from(raw_version).unwrap_or(0);
    let plan = migration_plan(version);
    if plan.already_current {
        return;
    }
    if plan.newer_version_warning {
        eprintln!(
            "Warning: configuration version {version} is newer than supported version {}.",
            plan.target_version
        );
        return;
    }
    if plan.run_2_to_3 {
        let legacy_bool = match config.get_at(raw::search::GROUP, raw::search::GLOSSARY_STYLE) {
            Some(Value::Bool(list)) => Some(list),
            Some(Value::Text(ref text)) if text.eq_ignore_ascii_case("true") => Some(true),
            Some(Value::Text(ref text)) if text.eq_ignore_ascii_case("false") => Some(false),
            _ => None,
        };
        if let Some(list) = legacy_bool {
            let migrated = migrate_2_to_3_list_result(list);
            let _ = config.put_at(
                raw::search::GROUP,
                raw::search::GLOSSARY_STYLE,
                &Value::Int(i64::from(migrated)),
            );
        }
    }
    if version >= 1 && version < plan.target_version {
        let _ = config.set_version(i64::from(plan.target_version));
    }
}

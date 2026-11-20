//! Canonical config file loading, legacy QSettings INI migrations (`v1→6`),
//! and Linux XDG cache config split helpers (Memento `#337`).

use std::fs;
use std::path::Path;

use memento_config::canonical::FORMAT_MAGIC;
use memento_config::config::{Config, RawDocument};
use memento_config::{parse_legacy_ini, QtFontSpec, Value};
use memento_setting::keys as raw;
use memento_setting::migration::{migrate_2_to_3_list_result, migration_plan};

use super::schema::{declared_setting_default, INTERFACE_FONT_KEYS};

/// Optional filesystem directories used by disk-moving legacy migrations
/// (`v1→2` and `v5→6`).
#[derive(Copy, Clone, Debug, Default)]
pub(crate) struct MigrationPaths<'a> {
    pub(crate) config_dir: Option<&'a Path>,
    pub(crate) data_dir: Option<&'a Path>,
}

/// Returns `true` when `(group, key)` belongs to the volatile Linux XDG cache
/// config file (`~/.cache/memento/memento.conf`, Memento `#337`):
/// `recent/files`, `dictionary/order`, and `main-window/*`.
pub fn is_volatile_cache_key(group: &str, key: &str) -> bool {
    (group == raw::recent::GROUP && key == raw::recent::FILES)
        || (group == raw::dictionary::GROUP && key == raw::dictionary::ORDER)
        || group == raw::window::GROUP
}

/// Loads `config_dir/file_name`, reading the file at most once.
///
/// Canonical `memento-config-store` files are parsed directly from the in-memory
/// buffer via [`Config::from_document_text`]; legacy QSettings INI files are
/// converted and migrated in memory.
pub(crate) fn load_config_store(config_dir: &Path, file_name: &str) -> Result<Config, String> {
    load_config_store_with_fallback(
        config_dir,
        file_name,
        None,
        MigrationPaths {
            config_dir: Some(config_dir),
            data_dir: None,
        },
        None,
    )
}

/// Loads `config_dir/file_name` with an optional legacy fallback directory
/// (e.g. macOS `~/.config/memento`, Windows `%APPDATA%\memento`) and optional
/// migration paths / cache store output.
pub(crate) fn load_config_store_with_fallback(
    config_dir: &Path,
    file_name: &str,
    fallback_dir: Option<&Path>,
    migration_paths: MigrationPaths<'_>,
    cache_out: Option<&mut Config>,
) -> Result<Config, String> {
    let primary = config_dir.join(file_name);
    let path = if primary.is_file() {
        primary
    } else if let Some(fallback) = fallback_dir.filter(|fb| *fb != config_dir) {
        let candidate = fallback.join(file_name);
        if candidate.is_file() {
            candidate
        } else {
            primary
        }
    } else {
        primary
    };

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
    migrate_legacy_config_with_paths(&mut config, migration_paths, cache_out);
    Ok(config)
}

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn migrate_legacy_config(config: &mut Config) {
    migrate_legacy_config_with_paths(config, MigrationPaths::default(), None);
}

pub(crate) fn migrate_legacy_config_with_paths(
    config: &mut Config,
    paths: MigrationPaths<'_>,
    mut cache_config: Option<&mut Config>,
) {
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

    if plan.run_1_to_2 {
        migrate_1_to_2(paths.config_dir);
    }
    if plan.run_2_to_3 {
        migrate_2_to_3(config);
    }
    if plan.run_3_to_4 {
        migrate_3_to_4(config);
    }
    if plan.run_4_to_5 {
        migrate_4_to_5(config);
    }
    if plan.run_5_to_6 {
        migrate_5_to_6(config, paths, cache_config.as_deref_mut());
    }

    if version >= 1 && version < plan.target_version {
        let _ = config.set_version(i64::from(plan.target_version));
    }
}

fn move_file_or_copy(src: &Path, dst: &Path) -> std::io::Result<()> {
    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent)?;
    }
    if fs::rename(src, dst).is_ok() {
        return Ok(());
    }
    fs::copy(src, dst)?;
    fs::remove_file(src)?;
    Ok(())
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let target = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir_recursive(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

fn move_dir_or_copy(src: &Path, dst: &Path) -> std::io::Result<()> {
    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent)?;
    }
    if fs::rename(src, dst).is_ok() {
        return Ok(());
    }
    copy_dir_recursive(src, dst)?;
    fs::remove_dir_all(src)?;
    Ok(())
}

/// v1 → v2: move `<config_dir>/dict/dictionaries.sqlite` to
/// `<config_dir>/dictionaries.sqlite` and remove the legacy `dict/` directory.
fn migrate_1_to_2(config_dir: Option<&Path>) {
    let Some(dir) = config_dir else {
        return;
    };
    let legacy_dict_dir = dir.join("dict");
    let legacy_db = legacy_dict_dir.join("dictionaries.sqlite");
    let target_db = dir.join("dictionaries.sqlite");
    if legacy_db.is_file() && !target_db.exists() {
        let _ = move_file_or_copy(&legacy_db, &target_db);
    }
    if legacy_dict_dir.is_dir() {
        let _ = fs::remove_dir_all(&legacy_dict_dir);
    }
}

/// v2 → v3: convert legacy boolean `search/list-result` into the
/// `GlossaryStyle` integer enum discriminant.
fn migrate_2_to_3(config: &mut Config) {
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

/// v3 → v4: remove obsolete v3 settings whose formats changed incompatibly
/// (`behavior/file-open-custom`, `dictionaries`, `interface`, `search/modifier`,
/// `main-window`).
fn migrate_3_to_4(config: &mut Config) {
    config.remove_at(raw::behavior::GROUP, raw::behavior::FILE_OPEN_CUSTOM);
    config.remove_group("dictionaries");
    config.remove_group(raw::interface::GROUP);
    config.remove_at(raw::search::GROUP, raw::search::MODIFIER);
    config.remove_group(raw::window::GROUP);
}

fn normalize_legacy_qfont_v4_to_v5(config: &Config, key: &str, raw_value: &str) -> Option<String> {
    let trimmed = raw_value.trim();
    let unwrapped = trimmed
        .strip_prefix("@Font(")
        .and_then(|rest| rest.strip_suffix(')'))
        .map(str::trim)
        .unwrap_or(trimmed);

    let looks_corrupt = unwrapped.is_empty()
        || unwrapped.starts_with("@Variant(")
        || unwrapped.starts_with("@Invalid(")
        || unwrapped.chars().any(char::is_control);

    if !looks_corrupt {
        if let Ok(spec) = QtFontSpec::parse(unwrapped) {
            return Some(spec.to_qt_string());
        }
    }

    match declared_setting_default(config, raw::interface::GROUP, key) {
        Some(Value::Text(default_font)) => Some(default_font),
        _ => None,
    }
}

/// v4 → v5: normalize serialized `QFont` settings under `[interface]` to
/// canonical 10-field `QFont::toString()` text, falling back to declared
/// defaults if a legacy binary `QVariant` / invalid string is encountered.
fn migrate_4_to_5(config: &mut Config) {
    for &key in INTERFACE_FONT_KEYS {
        let Some(raw_value) = config.get_at(raw::interface::GROUP, key) else {
            continue;
        };
        let raw_text = match raw_value {
            Value::Text(s) => s,
            other => format!("{other:?}"),
        };
        if let Some(normalized) = normalize_legacy_qfont_v4_to_v5(config, key, &raw_text) {
            let _ = config.put_at(raw::interface::GROUP, key, &Value::Text(normalized));
        }
    }
}

/// v5 → v6: move `dictionaries.sqlite` and `res/` from `config_dir` to
/// `data_dir` when distinct (Linux XDG `#337`), and migrate volatile keys
/// (`recent/files`, `dictionary/order`, `main-window/*`) into `cache_config`
/// when cache split is active.
fn migrate_5_to_6(
    config: &mut Config,
    paths: MigrationPaths<'_>,
    cache_config: Option<&mut Config>,
) {
    if let (Some(config_dir), Some(data_dir)) = (paths.config_dir, paths.data_dir) {
        if config_dir != data_dir {
            let src_db = config_dir.join("dictionaries.sqlite");
            let dst_db = data_dir.join("dictionaries.sqlite");
            if src_db.is_file() && !dst_db.exists() {
                let _ = move_file_or_copy(&src_db, &dst_db);
            }
            let src_res = config_dir.join("res");
            let dst_res = data_dir.join("res");
            if src_res.is_dir() && !dst_res.exists() {
                let _ = move_dir_or_copy(&src_res, &dst_res);
            }
        }
    }

    if let Some(cache) = cache_config {
        overlay_volatile_keys(cache, config);
        strip_volatile_keys(config);
    }
}

const VOLATILE_EXACT_KEYS: &[(&str, &str)] = &[
    (raw::recent::GROUP, raw::recent::FILES),
    (raw::dictionary::GROUP, raw::dictionary::ORDER),
];

const VOLATILE_WINDOW_KEYS: &[&str] = &[raw::window::SUBTITLE_LIST, raw::window::SEARCH];

/// Copy any present volatile keys (`recent/files`, `dictionary/order`,
/// `main-window/*`) from `source` onto `target`.
pub(crate) fn overlay_volatile_keys(target: &mut Config, source: &Config) {
    for &(group, key) in VOLATILE_EXACT_KEYS {
        if let Some(val) = source.get_at(group, key) {
            let _ = target.put_at(group, key, &val);
        }
    }
    // Preserve all keys inside `[main-window]` (known and any extra geometry keys).
    for &key in VOLATILE_WINDOW_KEYS {
        if let Some(val) = source.get_at(raw::window::GROUP, key) {
            let _ = target.put_at(raw::window::GROUP, key, &val);
        }
    }
    if let Ok(doc) = Config::parse_document_text(&source.render_document()) {
        if let Some(section) = doc.sections.get(raw::window::GROUP) {
            for key in section.keys() {
                if !VOLATILE_WINDOW_KEYS.contains(&key.as_str()) {
                    if let Some(val) = source.get_at(raw::window::GROUP, key) {
                        let _ = target.put_at(raw::window::GROUP, key, &val);
                    }
                }
            }
        }
    }
}

/// Remove all volatile keys (`recent/files`, `dictionary/order`, `main-window/*`)
/// from `config` so the primary config file stays clean for dotfiles syncing.
pub(crate) fn strip_volatile_keys(config: &mut Config) {
    for &(group, key) in VOLATILE_EXACT_KEYS {
        config.remove_at(group, key);
    }
    config.remove_group(raw::window::GROUP);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrate_v1_to_v2_moves_dict_database_and_removes_dict_dir() {
        let root = tempfile::TempDir::new().unwrap();
        let cfg_dir = root.path().join("config/memento");
        let legacy_dict = cfg_dir.join("dict");
        fs::create_dir_all(&legacy_dict).unwrap();
        fs::write(legacy_dict.join("dictionaries.sqlite"), b"sqlite-db").unwrap();

        let mut cfg = Config::default_empty();
        cfg.set_version(1).unwrap();
        migrate_legacy_config_with_paths(
            &mut cfg,
            MigrationPaths {
                config_dir: Some(&cfg_dir),
                data_dir: None,
            },
            None,
        );

        assert!(!legacy_dict.exists(), "legacy dict/ dir should be removed");
        assert_eq!(
            fs::read(cfg_dir.join("dictionaries.sqlite")).unwrap(),
            b"sqlite-db"
        );
        assert_eq!(cfg.version().unwrap(), 6);
    }

    #[test]
    fn migrate_v3_to_v4_removes_obsolete_groups_and_keys() {
        let mut cfg = Config::default_empty();
        cfg.set_version(3).unwrap();
        cfg.put_at(
            raw::behavior::GROUP,
            raw::behavior::FILE_OPEN_CUSTOM,
            &Value::Text("file:///old".to_owned()),
        )
        .unwrap();
        cfg.put_at(
            raw::behavior::GROUP,
            raw::behavior::AUTO_FIT,
            &Value::Bool(false),
        )
        .unwrap();
        cfg.put_at("dictionaries", "legacy", &Value::Text("x".to_owned()))
            .unwrap();
        cfg.put_at(
            raw::interface::GROUP,
            raw::interface::POPUP_WIDTH,
            &Value::Int(777),
        )
        .unwrap();
        cfg.put_at(raw::search::GROUP, raw::search::MODIFIER, &Value::Int(2))
            .unwrap();
        cfg.put_at(
            raw::window::GROUP,
            raw::window::SUBTITLE_LIST,
            &Value::Bool(true),
        )
        .unwrap();

        migrate_legacy_config(&mut cfg);

        assert!(cfg
            .raw_at(raw::behavior::GROUP, raw::behavior::FILE_OPEN_CUSTOM)
            .is_none());
        assert!(cfg.raw_at("dictionaries", "legacy").is_none());
        assert!(cfg
            .raw_at(raw::interface::GROUP, raw::interface::POPUP_WIDTH)
            .is_none());
        assert!(cfg
            .raw_at(raw::search::GROUP, raw::search::MODIFIER)
            .is_none());
        assert!(cfg
            .raw_at(raw::window::GROUP, raw::window::SUBTITLE_LIST)
            .is_none());
        // Unrelated settings survive untouched.
        assert_eq!(
            cfg.get_at(raw::behavior::GROUP, raw::behavior::AUTO_FIT),
            Some(Value::Bool(false))
        );
        assert_eq!(cfg.version().unwrap(), 6);
    }

    #[test]
    fn migrate_v4_to_v5_normalizes_qfont_and_falls_back_on_corrupt_qvariant() {
        let mut cfg = Config::default_empty();
        cfg.set_version(4).unwrap();
        cfg.put_at(
            raw::interface::GROUP,
            raw::interface::subtitle::FONT,
            &Value::Text("@Font(Noto Sans CJK JP,28)".to_owned()),
        )
        .unwrap();
        cfg.put_at(
            raw::interface::GROUP,
            raw::interface::SEARCH_TAG_FONT,
            &Value::Text("@Variant(\\0\\0\\0@\\0\\0\\0)".to_owned()),
        )
        .unwrap();

        migrate_legacy_config(&mut cfg);

        assert_eq!(
            cfg.get_at(raw::interface::GROUP, raw::interface::subtitle::FONT),
            Some(Value::Text("Noto Sans CJK JP,28,-1,5,50,0,0,0,0,0".to_owned()))
        );
        let fallback_tag = cfg
            .get_at(raw::interface::GROUP, raw::interface::SEARCH_TAG_FONT)
            .unwrap();
        let expected_tag =
            declared_setting_default(&cfg, raw::interface::GROUP, raw::interface::SEARCH_TAG_FONT)
                .unwrap();
        assert_eq!(fallback_tag, expected_tag);
        assert_eq!(cfg.version().unwrap(), 6);
    }

    #[test]
    fn migrate_v5_to_v6_moves_dicts_to_data_dir_and_splits_volatile_keys_to_cache() {
        let root = tempfile::TempDir::new().unwrap();
        let cfg_dir = root.path().join("config/memento");
        let data_dir = root.path().join("data/memento");
        fs::create_dir_all(cfg_dir.join("res/img")).unwrap();
        fs::write(cfg_dir.join("dictionaries.sqlite"), b"db-v5").unwrap();
        fs::write(cfg_dir.join("res/img/a.png"), b"png").unwrap();

        let mut cfg = Config::default_empty();
        cfg.set_version(5).unwrap();
        cfg.put_at(
            raw::behavior::GROUP,
            raw::behavior::AUTO_FIT,
            &Value::Bool(false),
        )
        .unwrap();
        cfg.recent_files_add("/videos/ep01.mkv").unwrap();
        cfg.put_at(
            raw::dictionary::GROUP,
            raw::dictionary::ORDER,
            &Value::Ints(vec![3, 1, 2]),
        )
        .unwrap();
        cfg.put_at(
            raw::window::GROUP,
            raw::window::SUBTITLE_LIST,
            &Value::Bool(true),
        )
        .unwrap();

        let mut cache_cfg = Config::default_empty();
        migrate_legacy_config_with_paths(
            &mut cfg,
            MigrationPaths {
                config_dir: Some(&cfg_dir),
                data_dir: Some(&data_dir),
            },
            Some(&mut cache_cfg),
        );

        assert!(!cfg_dir.join("dictionaries.sqlite").exists());
        assert_eq!(
            fs::read(data_dir.join("dictionaries.sqlite")).unwrap(),
            b"db-v5"
        );
        assert!(!cfg_dir.join("res").exists());
        assert_eq!(fs::read(data_dir.join("res/img/a.png")).unwrap(), b"png");

        // Volatile keys moved out of primary config into cache_cfg.
        assert!(cfg.raw_at(raw::recent::GROUP, raw::recent::FILES).is_none());
        assert!(cfg
            .raw_at(raw::dictionary::GROUP, raw::dictionary::ORDER)
            .is_none());
        assert!(cfg
            .raw_at(raw::window::GROUP, raw::window::SUBTITLE_LIST)
            .is_none());
        assert_eq!(cache_cfg.recent_files().unwrap(), vec!["/videos/ep01.mkv"]);
        assert_eq!(
            cache_cfg.get_at(raw::dictionary::GROUP, raw::dictionary::ORDER),
            Some(Value::Ints(vec![3, 1, 2]))
        );
        assert_eq!(
            cache_cfg.get_at(raw::window::GROUP, raw::window::SUBTITLE_LIST),
            Some(Value::Bool(true))
        );
        // Persistent settings remain in primary config.
        assert_eq!(
            cfg.get_at(raw::behavior::GROUP, raw::behavior::AUTO_FIT),
            Some(Value::Bool(false))
        );
        assert_eq!(cfg.version().unwrap(), 6);
    }

    #[test]
    fn full_v1_to_v6_chain_runs_every_step_in_order() {
        let root = tempfile::TempDir::new().unwrap();
        let cfg_dir = root.path().join("config/memento");
        let data_dir = root.path().join("data/memento");
        fs::create_dir_all(cfg_dir.join("dict")).unwrap();
        fs::write(cfg_dir.join("dict/dictionaries.sqlite"), b"v1-db").unwrap();
        fs::write(
            cfg_dir.join("memento.conf"),
            "version=1\n\
             [search]\n\
             list-result=false\n\
             modifier=2\n\
             [recent]\n\
             files=/anime/ep1.mkv\n\
             [interface]\n\
             sub-popup-width=999\n",
        )
        .unwrap();

        let mut cache_cfg = Config::default_empty();
        let loaded = load_config_store_with_fallback(
            &cfg_dir,
            "memento.conf",
            None,
            MigrationPaths {
                config_dir: Some(&cfg_dir),
                data_dir: Some(&data_dir),
            },
            Some(&mut cache_cfg),
        )
        .unwrap();

        // v1->2 moved dict/dictionaries.sqlite -> config_dir, then v5->6 moved it -> data_dir!
        assert!(!cfg_dir.join("dict").exists());
        assert!(!cfg_dir.join("dictionaries.sqlite").exists());
        assert_eq!(
            fs::read(data_dir.join("dictionaries.sqlite")).unwrap(),
            b"v1-db"
        );
        // v2->3 converted list-result=false -> 1 (LineBreak).
        assert_eq!(
            loaded.get_at(raw::search::GROUP, raw::search::GLOSSARY_STYLE),
            Some(Value::Int(1))
        );
        // v3->4 removed search/modifier and old interface settings.
        assert!(loaded
            .raw_at(raw::search::GROUP, raw::search::MODIFIER)
            .is_none());
        assert!(loaded
            .raw_at(raw::interface::GROUP, raw::interface::POPUP_WIDTH)
            .is_none());
        // v5->6 split recent/files into cache_cfg.
        assert!(loaded
            .raw_at(raw::recent::GROUP, raw::recent::FILES)
            .is_none());
        assert_eq!(cache_cfg.recent_files().unwrap(), vec!["/anime/ep1.mkv"]);
        assert_eq!(loaded.version().unwrap(), 6);
    }
}

//! Keybind profile management and shortcut conflict detection.

use std::collections::HashMap;

use memento_config::config::Config;
use memento_config::Value;
use memento_setting::keys as raw;

use super::anki_profiles::ANKI_PROFILE_DEFAULT;
use super::schema::{effective_text, value_text};

pub(crate) const KEYBIND_FIELDS: &[(&str, &str)] = &[
    (
        raw::keybind::profile::CARD_ADD,
        "Add current expression to Anki",
    ),
    (
        raw::keybind::profile::CARD_NEXT,
        "Scroll to next search result",
    ),
    (
        raw::keybind::profile::CARD_PREVIOUS,
        "Scroll to previous search result",
    ),
    (raw::keybind::profile::OPEN_FILE, "Open file"),
    (raw::keybind::profile::OPEN_URL, "Open URL"),
    (
        raw::keybind::profile::OSC_VISIBILITY,
        "Cycle OSC visibility",
    ),
    (raw::keybind::profile::SHOW_SEARCH, "Show auxiliary search"),
    (
        raw::keybind::profile::SHOW_SUBTITLE_LIST,
        "Show subtitle list",
    ),
    (raw::keybind::profile::START_OCR, "Start OCR"),
    (
        raw::keybind::profile::SUBTITLE_AUTO_PAUSE,
        "Toggle subtitle auto-pause",
    ),
    (
        raw::keybind::profile::SUBTITLE_DECREASE_SIZE,
        "Decrease subtitle size",
    ),
    (
        raw::keybind::profile::SUBTITLE_INCREASE_SIZE,
        "Increase subtitle size",
    ),
    (
        raw::keybind::profile::SUBTITLE_MOVE_DOWN,
        "Move subtitles down",
    ),
    (raw::keybind::profile::SUBTITLE_MOVE_UP, "Move subtitles up"),
    (
        raw::keybind::profile::SUBTITLE_SHOW,
        "Toggle subtitle visibility",
    ),
    (
        raw::keybind::profile::SUBTITLE_FURIGANA,
        "Toggle subtitle furigana",
    ),
    (
        raw::keybind::profile::SUBTITLE_LIST_COPY_SELECTED,
        "Copy selected subtitle",
    ),
    (
        raw::keybind::profile::SUBTITLE_LIST_FIND,
        "Find in subtitle list",
    ),
    (
        raw::keybind::profile::SUBTITLE_LIST_FIND_NEXT,
        "Find next in subtitle list",
    ),
    (
        raw::keybind::profile::SUBTITLE_LIST_FIND_PREVIOUS,
        "Find previous in subtitle list",
    ),
];

pub(crate) fn keybind_default(key: &str) -> &'static str {
    match key {
        raw::keybind::profile::CARD_ADD => "Ctrl+D",
        raw::keybind::profile::CARD_NEXT => "Ctrl+Down",
        raw::keybind::profile::CARD_PREVIOUS => "Ctrl+Up",
        raw::keybind::profile::OPEN_FILE => "Ctrl+O",
        raw::keybind::profile::OPEN_URL => "Ctrl+U",
        raw::keybind::profile::OSC_VISIBILITY => "Delete",
        raw::keybind::profile::SHOW_SEARCH => "Ctrl+R",
        raw::keybind::profile::SHOW_SUBTITLE_LIST => "Ctrl+E",
        raw::keybind::profile::START_OCR => "Ctrl+L",
        raw::keybind::profile::SUBTITLE_AUTO_PAUSE => "Ctrl+P",
        raw::keybind::profile::SUBTITLE_DECREASE_SIZE => "Ctrl+1",
        raw::keybind::profile::SUBTITLE_INCREASE_SIZE => "Ctrl+2",
        raw::keybind::profile::SUBTITLE_MOVE_DOWN => "Alt+2",
        raw::keybind::profile::SUBTITLE_MOVE_UP => "Alt+1",
        raw::keybind::profile::SUBTITLE_SHOW => "Ctrl+0",
        raw::keybind::profile::SUBTITLE_FURIGANA => "Alt+A",
        raw::keybind::profile::SUBTITLE_LIST_COPY_SELECTED => "Ctrl+Shift+C",
        raw::keybind::profile::SUBTITLE_LIST_FIND => "Ctrl+F",
        raw::keybind::profile::SUBTITLE_LIST_FIND_NEXT => "F3",
        raw::keybind::profile::SUBTITLE_LIST_FIND_PREVIOUS => "Shift+F3",
        _ => "",
    }
}

pub(crate) fn keybind_value(config: &Config, group: &str, key: &str) -> String {
    config
        .get_at(group, key)
        .map(|value| value_text(&value))
        .unwrap_or_else(|| keybind_default(key).to_owned())
}

pub(crate) fn keybind_profile_prefix() -> String {
    format!("{}/{}/", raw::keybind::GROUP, raw::keybind::profile::GROUP)
}

pub(crate) fn keybind_profile_group(name: &str) -> String {
    format!(
        "{}/{}/{}",
        raw::keybind::GROUP,
        raw::keybind::profile::GROUP,
        name
    )
}

pub(crate) fn keybind_profile_names(config: &Config) -> Vec<String> {
    let prefix = keybind_profile_prefix();
    let mut names = config
        .groups_with_prefix(&prefix)
        .into_iter()
        .filter_map(|group| group.strip_prefix(&prefix).map(str::to_owned))
        .filter(|name| !name.contains('/'))
        .collect::<Vec<_>>();
    names.sort_by(|left, right| left.encode_utf16().cmp(right.encode_utf16()));
    names.dedup();
    names
}

pub(crate) fn active_keybind_profile_name(config: &Config) -> String {
    let names = keybind_profile_names(config);
    effective_text(config, raw::keybind::GROUP, raw::keybind::CURRENT_PROFILE)
        .filter(|current| names.iter().any(|name| name == current))
        .or_else(|| names.first().cloned())
        .unwrap_or_else(|| ANKI_PROFILE_DEFAULT.to_owned())
}

pub(crate) fn ensure_default_keybind_profile(config: &mut Config) {
    if keybind_profile_names(config).is_empty() {
        let _ = reset_keybind_profiles(config);
    }
}

pub(crate) fn reset_keybind_profiles(config: &mut Config) -> Result<(), String> {
    let prefix = keybind_profile_prefix();
    for group in config.groups_with_prefix(&prefix) {
        config.remove_group(&group);
    }
    let profile = ANKI_PROFILE_DEFAULT;
    config
        .put_at(
            raw::keybind::GROUP,
            raw::keybind::CURRENT_PROFILE,
            &Value::Text(profile.to_owned()),
        )
        .map_err(|error| format!("{error:?}"))?;
    let profile_group = format!("{prefix}{profile}");
    for (key, _) in KEYBIND_FIELDS {
        config
            .put_at(
                &profile_group,
                key,
                &Value::Text(keybind_default(key).to_owned()),
            )
            .map_err(|error| format!("{error:?}"))?;
    }
    Ok(())
}

pub(crate) fn add_keybind_profile(config: &mut Config, requested_name: &str) -> Result<(), String> {
    let name = requested_name.trim();
    if name.is_empty() {
        return Err("profile name cannot be empty".to_owned());
    }
    if name.contains('/') {
        return Err("profile name cannot contain '/'".to_owned());
    }
    let names = keybind_profile_names(config);
    if names.iter().any(|existing| existing == name) {
        return Err(format!("keybind profile {name:?} already exists"));
    }
    let current = effective_text(config, raw::keybind::GROUP, raw::keybind::CURRENT_PROFILE)
        .filter(|current| names.iter().any(|item| item == current))
        .or_else(|| names.first().cloned())
        .unwrap_or_default();
    let source_group = keybind_profile_group(&current);
    let target_group = keybind_profile_group(name);
    for (key, _) in KEYBIND_FIELDS {
        let value = config
            .get_at(&source_group, key)
            .unwrap_or_else(|| Value::Text(keybind_default(key).to_owned()));
        config
            .put_at(&target_group, key, &value)
            .map_err(|error| format!("{error:?}"))?;
    }
    config
        .put_at(
            raw::keybind::GROUP,
            raw::keybind::CURRENT_PROFILE,
            &Value::Text(name.to_owned()),
        )
        .map(|_| ())
        .map_err(|error| format!("{error:?}"))
}

pub(crate) fn remove_keybind_profile(config: &mut Config, name: &str) -> Result<(), String> {
    if !keybind_profile_names(config)
        .iter()
        .any(|profile| profile == name)
    {
        return Err(format!("keybind profile {name:?} does not exist"));
    }
    let profile_group = keybind_profile_group(name);
    config.remove_group(&profile_group);
    let mut names = keybind_profile_names(config);
    if names.is_empty() {
        return reset_keybind_profiles(config);
    }
    let current = effective_text(config, raw::keybind::GROUP, raw::keybind::CURRENT_PROFILE);
    if current.as_deref() == Some(name)
        || !current
            .as_ref()
            .is_some_and(|current| names.iter().any(|profile| profile == current))
    {
        names.sort_by(|left, right| left.encode_utf16().cmp(right.encode_utf16()));
        config
            .put_at(
                raw::keybind::GROUP,
                raw::keybind::CURRENT_PROFILE,
                &Value::Text(names[0].clone()),
            )
            .map_err(|error| format!("{error:?}"))?;
    }
    Ok(())
}

fn normalize_shortcut_for_conflict(chord: &str) -> String {
    let trimmed = chord.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let parts: Vec<String> = trimmed
        .split('+')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(|part| part.to_ascii_lowercase())
        .collect();
    if parts.is_empty() {
        return String::new();
    }
    let mut has_ctrl = false;
    let mut has_alt = false;
    let mut has_shift = false;
    let mut has_meta = false;
    let mut rest = Vec::new();
    for part in parts {
        match part.as_str() {
            "ctrl" | "control" => has_ctrl = true,
            "alt" => has_alt = true,
            "shift" => has_shift = true,
            "meta" | "super" => has_meta = true,
            _ => rest.push(part),
        }
    }
    let mut canonical = Vec::new();
    if has_ctrl {
        canonical.push("ctrl".to_owned());
    }
    if has_alt {
        canonical.push("alt".to_owned());
    }
    if has_shift {
        canonical.push("shift".to_owned());
    }
    if has_meta {
        canonical.push("meta".to_owned());
    }
    canonical.extend(rest);
    canonical.join("+")
}

pub(crate) fn compute_keybind_conflicts(config: &Config) -> Vec<(String, String, Vec<String>)> {
    let current = active_keybind_profile_name(config);
    let profile_group = keybind_profile_group(&current);
    let mut by_chord: HashMap<String, Vec<(&'static str, &'static str)>> = HashMap::new();
    for &(key, label) in KEYBIND_FIELDS {
        let chord = keybind_value(config, &profile_group, key);
        let norm = normalize_shortcut_for_conflict(&chord);
        if !norm.is_empty() {
            by_chord.entry(norm).or_default().push((key, label));
        }
    }
    let mut conflicts = Vec::new();
    for &(key, _) in KEYBIND_FIELDS {
        let chord = keybind_value(config, &profile_group, key);
        let norm = normalize_shortcut_for_conflict(&chord);
        if norm.is_empty() {
            continue;
        }
        if let Some(entries) = by_chord.get(&norm) {
            if entries.len() > 1 {
                let others: Vec<String> = entries
                    .iter()
                    .filter(|(other_key, _)| *other_key != key)
                    .map(|(_, other_label)| (*other_label).to_owned())
                    .collect();
                if !others.is_empty() {
                    conflicts.push((key.to_owned(), chord, others));
                }
            }
        }
    }
    conflicts
}

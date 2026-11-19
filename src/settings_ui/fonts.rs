//! System and configured font family discovery with lazy, in-place UI model updates.

use std::rc::Rc;
use std::sync::OnceLock;

use slint::{Model as _, SharedString, VecModel};

use crate::controller::config::ConfigController;
use crate::SettingsWindow;

const DEFAULT_FONT_FAMILIES: &[&str] = &[
    "Kanji Stroke Orders",
    "Monospace",
    "Noto Sans",
    "Noto Sans CJK JP",
    "Sans Serif",
    "Serif",
];

static SYSTEM_FONT_FAMILIES: OnceLock<Vec<String>> = OnceLock::new();

fn system_font_families() -> &'static [String] {
    SYSTEM_FONT_FAMILIES.get_or_init(|| {
        let mut collection = slint::fontique_08::shared_collection();
        let mut families: Vec<String> = collection.family_names().map(str::to_owned).collect();
        for &default_family in DEFAULT_FONT_FAMILIES {
            families.push(default_family.to_owned());
        }
        families.sort_by_key(|name| name.to_ascii_lowercase());
        families.dedup();
        families
    })
}

fn available_font_families(config: &ConfigController) -> Vec<SharedString> {
    let mut families: Vec<String> = system_font_families().to_vec();
    for configured in config.configured_font_families() {
        if !families.iter().any(|existing| existing == &configured) {
            families.push(configured);
        }
    }
    families.sort_by_key(|name| name.to_ascii_lowercase());
    families.dedup();
    families.into_iter().map(Into::into).collect()
}

/// Populate `font_families` on `window` at most once (when empty), avoiding
/// repeated system font sorting on toggles, color drags, or minor edits.
pub(crate) fn ensure_font_families_loaded(window: &SettingsWindow, config: &ConfigController) {
    if window.get_font_families().row_count() == 0 {
        window.set_font_families(Rc::new(VecModel::from(available_font_families(config))).into());
    } else {
        sync_configured_font_families(window, config);
    }
}

/// Insert a single custom font family into the existing `VecModel` if missing,
/// preserving sorted order without replacing the model or re-scanning system fonts.
pub(crate) fn ensure_font_family_listed(window: &SettingsWindow, family: &str) {
    let trimmed = family.trim();
    if trimmed.is_empty() {
        return;
    }
    let model = window.get_font_families();
    if model.row_count() == 0 {
        return;
    }
    let Some(vec_model) = model.as_any().downcast_ref::<VecModel<SharedString>>() else {
        return;
    };
    let needle = trimmed.to_ascii_lowercase();
    let mut insert_idx = vec_model.row_count();
    for i in 0..vec_model.row_count() {
        let Some(existing) = vec_model.row_data(i) else {
            continue;
        };
        if existing.as_str() == trimmed {
            return;
        }
        if existing.as_str().to_ascii_lowercase() > needle {
            insert_idx = i;
            break;
        }
    }
    vec_model.insert(insert_idx, SharedString::from(trimmed));
}

/// Ensure any newly configured custom font families exist in the loaded model.
pub(crate) fn sync_configured_font_families(window: &SettingsWindow, config: &ConfigController) {
    if window.get_font_families().row_count() == 0 {
        return;
    }
    for configured in config.configured_font_families() {
        ensure_font_family_listed(window, &configured);
    }
}

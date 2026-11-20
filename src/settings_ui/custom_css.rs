//! Custom Popup CSS (inner & outer) file loading, saving, template insertion, and callbacks.

use std::cell::RefCell;
use std::io;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::SystemTime;

use slint::ComponentHandle;

use crate::ui::{
    default_custom_popup_css_template, default_custom_popup_outer_css_template,
    refresh_rich_ui_bridges, CUSTOM_POPUP_CSS_FILENAME, CUSTOM_POPUP_OUTER_CSS_FILENAME,
};
use crate::{open_directory_command, spawn_and_reap, SettingsWindow};

use super::SettingsContext;

pub(crate) type CustomCssFingerprint = (
    Option<(u64, Option<SystemTime>)>,
    Option<(u64, Option<SystemTime>)>,
);

fn file_meta_stamp(path: &Path) -> Option<(u64, Option<SystemTime>)> {
    std::fs::metadata(path)
        .ok()
        .map(|meta| (meta.len(), meta.modified().ok()))
}

pub(crate) fn custom_css_fingerprint(config_dir: &Path) -> CustomCssFingerprint {
    let inner_path = config_dir.join(CUSTOM_POPUP_CSS_FILENAME);
    let outer_path = config_dir.join(CUSTOM_POPUP_OUTER_CSS_FILENAME);
    (file_meta_stamp(&inner_path), file_meta_stamp(&outer_path))
}

pub(crate) fn invalidate_custom_css_fingerprint(
    cached_sig: &RefCell<Option<CustomCssFingerprint>>,
) {
    *cached_sig.borrow_mut() = None;
}

pub(crate) fn refresh_custom_popup_css(window: &SettingsWindow, config_dir: &Path) {
    let inner_path = config_dir.join(CUSTOM_POPUP_CSS_FILENAME);
    window.set_custom_popup_css_path(inner_path.display().to_string().into());
    let inner_content = std::fs::read_to_string(&inner_path).unwrap_or_default();
    window.set_custom_popup_css(inner_content.into());

    let outer_path = config_dir.join(CUSTOM_POPUP_OUTER_CSS_FILENAME);
    window.set_custom_popup_outer_css_path(outer_path.display().to_string().into());
    let outer_content = std::fs::read_to_string(&outer_path).unwrap_or_default();
    window.set_custom_popup_outer_css(outer_content.into());
}

/// Reloads `popup.css` and `popup_outer.css` into `SettingsWindow` only when
/// their `(len, mtime)` metadata fingerprint differs from `cached_sig`.
///
/// Returns `true` when disk files were read and written to `window`, or `false`
/// when the fingerprint matched and both disk I/O and Slint property writes were skipped.
pub(crate) fn refresh_custom_popup_css_if_changed(
    window: &SettingsWindow,
    config_dir: &Path,
    cached_sig: &RefCell<Option<CustomCssFingerprint>>,
) -> bool {
    let sig = custom_css_fingerprint(config_dir);
    if cached_sig.borrow().as_ref() == Some(&sig) {
        return false;
    }
    refresh_custom_popup_css(window, config_dir);
    *cached_sig.borrow_mut() = Some(sig);
    true
}

pub(crate) fn save_custom_css_file(
    config_dir: &Path,
    filename: &str,
    css: &str,
) -> Result<PathBuf, String> {
    std::fs::create_dir_all(config_dir)
        .map_err(|e| format!("Could not create config directory: {e}"))?;
    let target_path = config_dir.join(filename);
    let mut temp = tempfile::NamedTempFile::new_in(config_dir)
        .map_err(|e| format!("Could not create temp CSS file: {e}"))?;
    io::Write::write_all(&mut temp, css.as_bytes())
        .map_err(|e| format!("Could not write {filename}: {e}"))?;
    temp.persist(&target_path)
        .map_err(|e| format!("Could not save {filename}: {}", e.error))?;
    Ok(target_path)
}

pub(crate) fn save_custom_css_on_apply(
    window: &SettingsWindow,
    cache_dir: &Path,
    cached_sig: &RefCell<Option<CustomCssFingerprint>>,
) -> Result<(), String> {
    let inner_text = window.get_custom_popup_css().to_string();
    let inner_path = cache_dir.join(CUSTOM_POPUP_CSS_FILENAME);
    if !inner_text.trim().is_empty() || inner_path.exists() {
        save_custom_css_file(cache_dir, CUSTOM_POPUP_CSS_FILENAME, &inner_text)?;
        invalidate_custom_css_fingerprint(cached_sig);
    }
    let outer_text = window.get_custom_popup_outer_css().to_string();
    let outer_path = cache_dir.join(CUSTOM_POPUP_OUTER_CSS_FILENAME);
    if !outer_text.trim().is_empty() || outer_path.exists() {
        save_custom_css_file(cache_dir, CUSTOM_POPUP_OUTER_CSS_FILENAME, &outer_text)?;
        invalidate_custom_css_fingerprint(cached_sig);
    }
    Ok(())
}

pub(crate) fn wire(window: &SettingsWindow, ctx: &Rc<SettingsContext>) {
    let tpl_window = window.as_weak();
    window.on_insert_custom_popup_css_template(move |tab| {
        if let Some(window) = tpl_window.upgrade() {
            if tab == 1 {
                window.set_custom_popup_outer_css(default_custom_popup_outer_css_template().into());
                window.set_settings_status(
                    "Starter Popup Outer CSS template inserted — click Save & Reload CSS".into(),
                );
            } else {
                window.set_custom_popup_css(default_custom_popup_css_template().into());
                window.set_settings_status(
                    "Starter Popup CSS template inserted — click Save & Reload CSS".into(),
                );
            }
        }
    });

    let save_window = window.as_weak();
    let save_ctx = ctx.clone();
    window.on_save_custom_popup_css(move |inner_css, outer_css| {
        let Some(window) = save_window.upgrade() else {
            return;
        };
        if let Err(error) = save_custom_css_file(
            &save_ctx.cache_dir,
            CUSTOM_POPUP_CSS_FILENAME,
            inner_css.as_str(),
        ) {
            window.set_settings_status(error.into());
            return;
        }
        invalidate_custom_css_fingerprint(&save_ctx.custom_css_fingerprint);
        let outer_path = save_ctx.cache_dir.join(CUSTOM_POPUP_OUTER_CSS_FILENAME);
        if !outer_css.trim().is_empty() || outer_path.exists() {
            if let Err(error) = save_custom_css_file(
                &save_ctx.cache_dir,
                CUSTOM_POPUP_OUTER_CSS_FILENAME,
                outer_css.as_str(),
            ) {
                window.set_settings_status(error.into());
                return;
            }
            invalidate_custom_css_fingerprint(&save_ctx.custom_css_fingerprint);
        }
        if let Some(app) = save_ctx.app_weak.upgrade() {
            refresh_rich_ui_bridges(&app, &save_ctx.config);
        }
        window.set_settings_status("Custom popup CSS saved & reloaded".into());
    });

    let open_window = window.as_weak();
    let open_ctx = ctx.clone();
    window.on_open_custom_popup_css(move |tab| {
        let Some(window) = open_window.upgrade() else {
            return;
        };
        let (filename, default_tpl) = if tab == 1 {
            (
                CUSTOM_POPUP_OUTER_CSS_FILENAME,
                default_custom_popup_outer_css_template(),
            )
        } else {
            (
                CUSTOM_POPUP_CSS_FILENAME,
                default_custom_popup_css_template(),
            )
        };
        let css_path = open_ctx.cache_dir.join(filename);
        if !css_path.exists() {
            let current = if tab == 1 {
                window.get_custom_popup_outer_css().to_string()
            } else {
                window.get_custom_popup_css().to_string()
            };
            let initial = if current.trim().is_empty() {
                default_tpl.to_owned()
            } else {
                current
            };
            match save_custom_css_file(&open_ctx.cache_dir, filename, &initial) {
                Ok(_) => {
                    invalidate_custom_css_fingerprint(&open_ctx.custom_css_fingerprint);
                    if tab == 1 {
                        window.set_custom_popup_outer_css(initial.into());
                    } else {
                        window.set_custom_popup_css(initial.into());
                    }
                }
                Err(error) => {
                    window.set_settings_status(error.into());
                    return;
                }
            }
        }
        match spawn_and_reap(open_directory_command(&css_path)) {
            Ok(_) => window.set_settings_status(
                format!("Opened {filename} — external edits hot-reload automatically").into(),
            ),
            Err(error) => window.set_settings_status(
                format!("Could not open {}: {error}", css_path.display()).into(),
            ),
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn custom_css_refresh_skips_unchanged_and_reloads_on_modification_or_invalidation() {
        i_slint_backend_testing::init_no_event_loop();
        let dir = tempfile::tempdir().unwrap();
        let win = SettingsWindow::new().unwrap();
        let cached_sig: RefCell<Option<CustomCssFingerprint>> = RefCell::new(None);

        let inner_path = dir.path().join(CUSTOM_POPUP_CSS_FILENAME);
        let outer_path = dir.path().join(CUSTOM_POPUP_OUTER_CSS_FILENAME);
        std::fs::write(&inner_path, ".term { color: red; }").unwrap();
        std::fs::write(&outer_path, ".popup { border: 1px solid #fff; }").unwrap();

        // First load populates Slint text and caches fingerprint.
        assert!(refresh_custom_popup_css_if_changed(
            &win,
            dir.path(),
            &cached_sig
        ));
        assert_eq!(win.get_custom_popup_css().as_str(), ".term { color: red; }");
        assert_eq!(
            win.get_custom_popup_outer_css().as_str(),
            ".popup { border: 1px solid #fff; }"
        );
        assert!(cached_sig.borrow().is_some());

        // Second call with unchanged files skips disk reads AND Slint writes.
        win.set_custom_popup_css("sentinel-untouched".into());
        assert!(!refresh_custom_popup_css_if_changed(
            &win,
            dir.path(),
            &cached_sig
        ));
        assert_eq!(
            win.get_custom_popup_css().as_str(),
            "sentinel-untouched",
            "matching fingerprint must skip overwriting Slint text"
        );

        // External file modification changes fingerprint -> immediate reload.
        std::fs::write(&inner_path, ".term { color: #00ff88; font-size: 16px; }").unwrap();
        assert!(refresh_custom_popup_css_if_changed(
            &win,
            dir.path(),
            &cached_sig
        ));
        assert_eq!(
            win.get_custom_popup_css().as_str(),
            ".term { color: #00ff88; font-size: 16px; }"
        );

        // Saving via `save_custom_css_on_apply` invalidates cached fingerprint so next refresh reads.
        win.set_custom_popup_outer_css(".popup { border: 2px solid #0af; }".into());
        save_custom_css_on_apply(&win, dir.path(), &cached_sig).unwrap();
        assert!(
            cached_sig.borrow().is_none(),
            "save must invalidate custom CSS fingerprint"
        );
        assert!(refresh_custom_popup_css_if_changed(
            &win,
            dir.path(),
            &cached_sig
        ));
        assert_eq!(
            win.get_custom_popup_outer_css().as_str(),
            ".popup { border: 2px solid #0af; }"
        );

        // Explicit invalidation (as on Reset/Revert) forces reload even with unchanged disk files.
        win.set_custom_popup_css("unsaved-draft".into());
        invalidate_custom_css_fingerprint(&cached_sig);
        assert!(refresh_custom_popup_css_if_changed(
            &win,
            dir.path(),
            &cached_sig
        ));
        assert_eq!(
            win.get_custom_popup_css().as_str(),
            ".term { color: #00ff88; font-size: 16px; }"
        );
    }
}

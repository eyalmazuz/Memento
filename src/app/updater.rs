//! GitHub release update check, opt-in dialog, config folder opener, and external links.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use slint::ComponentHandle;

use crate::app::paths::{memento_xdg_dir, open_directory_command, spawn_and_reap};
use crate::app::preferences::setting_enabled;
use crate::controller::config::ConfigController;
use crate::settings_ui::{refresh_category_settings, SettingsSession};
use crate::ui::MementoApp;
use memento_setting::keys as raw;

pub(crate) const MEMENTO_RELEASES_URL: &str =
    "https://github.com/ripose-jp/Memento/releases/latest";

/// Query the same release endpoint as the original Memento update dialog.
pub(crate) fn latest_release() -> Result<String, String> {
    use memento_net::http::{HttpClient, Request, Url};
    let url = Url::parse("https://api.github.com/repos/ripose-jp/memento/releases/latest")
        .map_err(|error| error.to_string())?;
    let request = Request::get(url)
        .header(
            "User-Agent",
            format!("Memento/{}", env!("CARGO_PKG_VERSION")),
        )
        .header("Accept", "application/vnd.github+json");
    let mut response = HttpClient::default()
        .send(&request)
        .map_err(|error| error.to_string())?;
    if response.status() != 200 {
        return Err(format!("GitHub returned HTTP {}", response.status()));
    }
    let body = response
        .read_all(256 * 1024)
        .map_err(|error| error.to_string())?;
    let value: serde_json::Value =
        serde_json::from_slice(&body).map_err(|error| error.to_string())?;
    value
        .get("tag_name")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| "GitHub did not return a release tag".to_string())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StartupUpdateAction {
    ShowOptInPrompt,
    RunSilentCheck,
    Nothing,
}

pub(crate) fn determine_startup_update_action(config: &ConfigController) -> StartupUpdateAction {
    let opt_in_shown = setting_enabled(
        config,
        raw::internal::GROUP,
        raw::internal::AUTO_UPDATE_OPT_IN_SHOWN,
        false,
    );
    if !opt_in_shown {
        return StartupUpdateAction::ShowOptInPrompt;
    }
    let auto_check = setting_enabled(
        config,
        raw::application::GROUP,
        raw::application::AUTO_UPDATE_CHECK,
        raw::application::AUTO_UPDATE_CHECK_DEFAULT,
    );
    if auto_check {
        StartupUpdateAction::RunSilentCheck
    } else {
        StartupUpdateAction::Nothing
    }
}

pub(crate) fn update_opt_in_message(is_macos: bool) -> &'static str {
    if is_macos {
        "Do you want to check for updates on launch?\n\nThis can be changed at any time in Settings → Options (Cmd+,) → Application.\nThis will query GitHub's releases API when Memento starts."
    } else {
        "Do you want to check for updates on launch?\n\nThis can be changed at any time in Settings → Options (Ctrl+,) → Application.\nThis will query GitHub's releases API when Memento starts."
    }
}

pub(crate) fn open_update_opt_in_dialog(app: &MementoApp) {
    app.set_update_dialog_opt_in(true);
    app.set_update_dialog_title("Auto Update".into());
    app.set_update_dialog_message(update_opt_in_message(cfg!(target_os = "macos")).into());
    app.set_update_dialog_release_tag("".into());
    app.set_update_dialog_release_url(MEMENTO_RELEASES_URL.into());
    app.set_update_dialog_open(true);
}

pub(crate) fn apply_update_check_outcome(
    app: &MementoApp,
    current_version: &str,
    outcome: Result<String, String>,
    silent: bool,
) {
    let current_tag = format!("v{}", current_version.trim_start_matches('v'));
    match outcome {
        Ok(tag) => {
            let trimmed = tag.trim();
            if trimmed == current_tag || trimmed == current_version {
                if !silent {
                    app.set_update_dialog_opt_in(false);
                    app.set_update_dialog_title("Up to Date".into());
                    app.set_update_dialog_message(
                        format!("You're on the latest version ({current_tag}).").into(),
                    );
                    app.set_update_dialog_release_tag("".into());
                    app.set_update_dialog_release_url(MEMENTO_RELEASES_URL.into());
                    app.set_update_dialog_open(true);
                    app.set_status_message("You are on the latest version.".into());
                }
            } else {
                app.set_update_dialog_opt_in(false);
                app.set_update_dialog_title("New Version Available".into());
                app.set_update_dialog_message(
                    format!(
                        "Memento {trimmed} is available on GitHub Releases (currently installed: {current_tag})."
                    )
                    .into(),
                );
                app.set_update_dialog_release_tag(trimmed.into());
                app.set_update_dialog_release_url(MEMENTO_RELEASES_URL.into());
                app.set_update_dialog_open(true);
                app.set_status_message(format!("New Memento release available: {trimmed}").into());
            }
        }
        Err(error) => {
            if !silent {
                app.set_update_dialog_opt_in(false);
                app.set_update_dialog_title("Update Check Failed".into());
                app.set_update_dialog_message(
                    format!(
                        "Could not check GitHub for updates: {error}\n\nYou can check for updates manually on GitHub Releases."
                    )
                    .into(),
                );
                app.set_update_dialog_release_tag("".into());
                app.set_update_dialog_release_url(MEMENTO_RELEASES_URL.into());
                app.set_update_dialog_open(true);
                app.set_status_message(format!("Could not check for updates: {error}").into());
            }
        }
    }
}

pub(crate) fn spawn_update_check(response_app: slint::Weak<MementoApp>, silent: bool) {
    if !silent {
        if let Some(app) = response_app.upgrade() {
            app.set_status_message("Checking for updates…".into());
        }
    }
    std::thread::spawn(move || {
        let outcome = latest_release();
        let _ = response_app.upgrade_in_event_loop(move |app| {
            apply_update_check_outcome(&app, env!("CARGO_PKG_VERSION"), outcome, silent);
        });
    });
}

pub(crate) fn install_update_and_link_handlers(
    app: &MementoApp,
    config_ctrl: Rc<ConfigController>,
    config_dir: PathBuf,
    settings_session: Rc<RefCell<Option<SettingsSession>>>,
) {
    let update_app = app.as_weak();
    app.on_check_updates(move || {
        spawn_update_check(update_app.clone(), false);
    });

    let opt_in_app = app.as_weak();
    let opt_in_config = config_ctrl;
    let opt_in_cache = config_dir;
    let opt_in_settings = settings_session;
    app.on_update_opt_in_responded(move |accepted| {
        let Some(app) = opt_in_app.upgrade() else {
            return;
        };
        app.set_update_dialog_open(false);
        app.set_update_dialog_opt_in(false);
        let opt_in_key = format!(
            "{}/{}",
            raw::internal::GROUP,
            raw::internal::AUTO_UPDATE_OPT_IN_SHOWN
        );
        let auto_check_key = format!(
            "{}/{}",
            raw::application::GROUP,
            raw::application::AUTO_UPDATE_CHECK
        );
        let auto_val = if accepted { "true" } else { "false" };
        if let Err(error) = opt_in_config
            .persist_single_setting(&opt_in_key, "true", &opt_in_cache, "memento.conf")
            .and_then(|()| {
                opt_in_config.persist_single_setting(
                    &auto_check_key,
                    auto_val,
                    &opt_in_cache,
                    "memento.conf",
                )
            })
        {
            app.set_status_message(format!("Could not save update preference: {error}").into());
        }
        if let Some(session) = opt_in_settings.borrow().as_ref() {
            if session.window.get_active_category() == 1 {
                refresh_category_settings(&session.window, &opt_in_config, 1);
            }
        }
        if accepted {
            spawn_update_check(opt_in_app.clone(), true);
        }
    });

    let dismiss_update_app = app.as_weak();
    app.on_update_dialog_dismissed(move || {
        if let Some(app) = dismiss_update_app.upgrade() {
            app.set_update_dialog_open(false);
            app.set_update_dialog_opt_in(false);
        }
    });

    let config_dir_app = app.as_weak();
    app.on_open_config(move || {
        if let Some(path) = memento_xdg_dir("XDG_CONFIG_HOME", ".config") {
            if let Err(error) = std::fs::create_dir_all(&path)
                .and_then(|()| spawn_and_reap(open_directory_command(&path)).map(|_| ()))
            {
                if let Some(app) = config_dir_app.upgrade() {
                    app.set_status_message(
                        format!("Could not open configuration directory: {error}").into(),
                    );
                }
            }
        }
    });

    let ext_link_app = app.as_weak();
    app.on_open_external_link(move |url| {
        let url_str = url.trim().to_owned();
        if url_str.is_empty() {
            return;
        }
        let weak = ext_link_app.clone();
        std::thread::spawn(move || {
            if let Err(error) = webbrowser::open(&url_str) {
                let _ = weak.upgrade_in_event_loop(move |app| {
                    app.set_status_message(format!("Could not open link: {error}").into());
                });
            }
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn startup_update_decision_and_dialog_outcomes() {
        let cfg = ConfigController::new();
        assert_eq!(
            determine_startup_update_action(&cfg),
            StartupUpdateAction::ShowOptInPrompt
        );
        cfg.edit_setting("internal/auto-update-opt-in-shown", "true")
            .unwrap();
        assert_eq!(
            determine_startup_update_action(&cfg),
            StartupUpdateAction::Nothing
        );
        cfg.edit_setting("application/auto-update-check", "true")
            .unwrap();
        assert_eq!(
            determine_startup_update_action(&cfg),
            StartupUpdateAction::RunSilentCheck
        );

        i_slint_backend_testing::init_no_event_loop();
        let app = MementoApp::new().unwrap();
        open_update_opt_in_dialog(&app);
        assert!(app.get_update_dialog_open());
        assert!(app.get_update_dialog_opt_in());
        app.set_update_dialog_open(false);

        // Silent up-to-date and silent failure must not pop up the dialog.
        apply_update_check_outcome(&app, "2.0.2", Ok("v2.0.2".to_owned()), true);
        assert!(!app.get_update_dialog_open());
        apply_update_check_outcome(&app, "2.0.2", Err("offline".to_owned()), true);
        assert!(!app.get_update_dialog_open());

        // Silent newer release DOES open the modal with the release tag.
        apply_update_check_outcome(&app, "2.0.2", Ok("v2.1.0".to_owned()), true);
        assert!(app.get_update_dialog_open());
        assert!(!app.get_update_dialog_opt_in());
        assert_eq!(app.get_update_dialog_release_tag().as_str(), "v2.1.0");
        assert_eq!(
            app.get_update_dialog_title().as_str(),
            "New Version Available"
        );

        // Manual up-to-date check opens modal with "Up to Date".
        app.set_update_dialog_open(false);
        apply_update_check_outcome(&app, "2.0.2", Ok("v2.0.2".to_owned()), false);
        assert!(app.get_update_dialog_open());
        assert_eq!(app.get_update_dialog_title().as_str(), "Up to Date");
        assert!(app.get_update_dialog_release_tag().is_empty());

        assert!(update_opt_in_message(true).contains("Cmd+,"));
        assert!(!update_opt_in_message(true).contains("Ctrl+,"));
        assert!(update_opt_in_message(false).contains("Ctrl+,"));
        assert!(!update_opt_in_message(false).contains("Cmd+,"));
    }
}

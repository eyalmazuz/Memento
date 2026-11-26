//! GitHub release update check (Stable & Nightly channels), opt-in dialog,
//! config folder opener, and external links.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use slint::ComponentHandle;

use crate::app::paths::{open_directory_command, spawn_and_reap, subomi_xdg_dir};
use crate::app::preferences::setting_enabled;
use crate::controller::config::ConfigController;
use crate::settings_ui::{refresh_category_settings, SettingsSession};
use crate::ui::SubomiApp;
use subomi_config::Value;
use subomi_setting::enums::UpdateChannel;
use subomi_setting::keys as raw;

pub(crate) const SUBOMI_RELEASES_URL: &str =
    "https://github.com/eyalmazuz/Subomi/releases/latest";
pub(crate) const SUBOMI_NIGHTLY_RELEASE_URL: &str =
    "https://github.com/eyalmazuz/Subomi/releases/tag/nightly";
const SUBOMI_STABLE_API_URL: &str =
    "https://api.github.com/repos/eyalmazuz/Subomi/releases/latest";
const SUBOMI_NIGHTLY_API_URL: &str =
    "https://api.github.com/repos/eyalmazuz/Subomi/releases/tags/nightly";

/// Compile-time origin of this binary (`SUBOMI_BUILD_CHANNEL` set by CI, else `Dev`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BuildChannel {
    Stable,
    Nightly,
    Dev,
}

impl BuildChannel {
    pub(crate) fn current() -> Self {
        Self::from_env(option_env!("SUBOMI_BUILD_CHANNEL"))
    }

    pub(crate) fn from_env(raw_val: Option<&str>) -> Self {
        match raw_val.map(str::trim).unwrap_or("").to_ascii_lowercase().as_str() {
            "stable" => Self::Stable,
            "nightly" => Self::Nightly,
            _ => Self::Dev,
        }
    }

    pub(crate) const fn default_update_channel(self) -> UpdateChannel {
        match self {
            Self::Nightly => UpdateChannel::Nightly,
            Self::Stable | Self::Dev => UpdateChannel::Stable,
        }
    }
}

/// Metadata parsed from the rolling GitHub `nightly` prerelease.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NightlyReleaseInfo {
    pub(crate) tag: String,
    pub(crate) name: String,
    pub(crate) commit_sha: String,
    pub(crate) published_date: String,
    pub(crate) url: String,
}

impl NightlyReleaseInfo {
    pub(crate) fn short_sha(&self) -> &str {
        let trimmed = self.commit_sha.trim();
        let end = trimmed.len().min(7);
        &trimmed[..end]
    }

    pub(crate) fn badge_label(&self) -> String {
        let short = self.short_sha();
        if short.is_empty() {
            "nightly".to_owned()
        } else {
            format!("nightly-{short}")
        }
    }

    pub(crate) fn summary_label(&self) -> String {
        let short = self.short_sha();
        match (!short.is_empty(), !self.published_date.is_empty()) {
            (true, true) => format!("{short} ({})", self.published_date),
            (true, false) => short.to_owned(),
            (false, true) => format!("nightly ({})", self.published_date),
            (false, false) => {
                if self.name.trim().is_empty() {
                    "nightly".to_owned()
                } else {
                    self.name.trim().to_owned()
                }
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum UpdateCheckOutcome {
    Stable(Result<String, String>),
    Nightly(Result<NightlyReleaseInfo, String>),
}

/// Resolve the active update channel from `config`, defaulting to `Nightly` on
/// official nightly binaries and `Stable` on stable or local dev builds.
pub(crate) fn active_update_channel_for_build(
    config: &ConfigController,
    build_channel: BuildChannel,
) -> UpdateChannel {
    let default_channel = build_channel.default_update_channel();
    match config.explicit_setting(raw::application::GROUP, raw::application::UPDATE_CHANNEL) {
        Some(Value::Text(configured)) => {
            UpdateChannel::from_str_with_default(&configured, default_channel)
        }
        _ => default_channel,
    }
}

pub(crate) fn active_update_channel(config: &ConfigController) -> UpdateChannel {
    active_update_channel_for_build(config, BuildChannel::current())
}

/// Extracts the release tag segment from a GitHub `/releases/tag/<tag>` redirect `Location`.
pub(crate) fn parse_release_tag_from_location(location: &str) -> Option<String> {
    let (_, after_marker) = location.split_once("/releases/tag/")?;
    let trimmed = after_marker
        .split(['?', '#'])
        .next()
        .unwrap_or("")
        .trim_matches('/');
    (!trimmed.is_empty() && !trimmed.contains('/')).then(|| trimmed.to_owned())
}

fn is_commit_hex(token: &str) -> bool {
    let len = token.len();
    (7..=40).contains(&len) && token.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Extract a 7–40 character git commit SHA from a string (e.g. `"Nightly (2026-11-26 · 566a82d)"`).
pub(crate) fn extract_commit_sha(text: &str) -> Option<String> {
    for raw_token in text.split(|c: char| !c.is_ascii_alphanumeric()) {
        if is_commit_hex(raw_token) {
            return Some(raw_token.to_ascii_lowercase());
        }
    }
    None
}

/// Compare two git commit hashes (full or abbreviated, >= 7 hex chars).
pub(crate) fn commit_hashes_match(a: &str, b: &str) -> bool {
    let a = a.trim().to_ascii_lowercase();
    let b = b.trim().to_ascii_lowercase();
    if !is_commit_hex(&a) || !is_commit_hex(&b) {
        return false;
    }
    a.starts_with(&b) || b.starts_with(&a)
}

fn extract_iso_date(timestamp: &str) -> String {
    let trimmed = timestamp.trim();
    if trimmed.len() >= 10 {
        let candidate = &trimmed[..10];
        let bytes = candidate.as_bytes();
        if bytes[4] == b'-'
            && bytes[7] == b'-'
            && bytes
                .iter()
                .enumerate()
                .all(|(i, b)| i == 4 || i == 7 || b.is_ascii_digit())
        {
            return candidate.to_owned();
        }
    }
    String::new()
}

/// Parse a GitHub `/releases/tags/nightly` JSON object into [`NightlyReleaseInfo`].
pub(crate) fn parse_nightly_release_json(value: &serde_json::Value) -> Result<NightlyReleaseInfo, String> {
    let tag = value
        .get("tag_name")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("nightly")
        .to_owned();
    let name = value
        .get("name")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .unwrap_or("")
        .to_owned();
    let target_commitish = value
        .get("target_commitish")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .unwrap_or("");
    let body = value
        .get("body")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    let commit_sha = if is_commit_hex(target_commitish) {
        target_commitish.to_ascii_lowercase()
    } else {
        extract_commit_sha(&name)
            .or_else(|| extract_commit_sha(body))
            .unwrap_or_default()
    };
    let published_date = value
        .get("published_at")
        .or_else(|| value.get("created_at"))
        .and_then(serde_json::Value::as_str)
        .map(extract_iso_date)
        .unwrap_or_default();
    let url = value
        .get("html_url")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(SUBOMI_NIGHTLY_RELEASE_URL)
        .to_owned();

    if commit_sha.is_empty() && name.is_empty() && published_date.is_empty() {
        return Err("GitHub nightly release metadata did not include a commit or date".to_owned());
    }

    Ok(NightlyReleaseInfo {
        tag,
        name,
        commit_sha,
        published_date,
        url,
    })
}

/// Query the GitHub releases endpoint for the latest stable Subomi release.
///
/// Prefers a no-redirect `HEAD` request to `/releases/latest` (~400 bytes, no REST API
/// rate limit) to read the `Location: .../releases/tag/<tag>` header, falling back to
/// the gzipped GitHub REST API JSON endpoint if no tag redirect is returned.
pub(crate) fn latest_release() -> Result<String, String> {
    use subomi_net::http::{HttpClient, HttpClientConfig, Method, Request, Url};
    let user_agent = format!("Subomi/{}", env!("CARGO_PKG_VERSION"));

    if let Ok(releases_url) = Url::parse(SUBOMI_RELEASES_URL) {
        let head_client = HttpClient::new(HttpClientConfig {
            follow_redirects: false,
            ..HttpClientConfig::default()
        });
        let head_req = Request::new(Method::Head, releases_url).header("User-Agent", &user_agent);
        if let Ok(response) = head_client.send(&head_req) {
            if matches!(response.status(), 301 | 302 | 303 | 307 | 308) {
                if let Some(tag) = response
                    .header("location")
                    .and_then(parse_release_tag_from_location)
                {
                    return Ok(tag);
                }
            }
        }
    }

    let url = Url::parse(SUBOMI_STABLE_API_URL).map_err(|error| error.to_string())?;
    let request = Request::get(url)
        .header("User-Agent", user_agent)
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

/// Query GitHub for the rolling `nightly` prerelease metadata.
pub(crate) fn latest_nightly_release() -> Result<NightlyReleaseInfo, String> {
    use subomi_net::http::{HttpClient, Request, Url};
    let user_agent = format!("Subomi/{}", env!("CARGO_PKG_VERSION"));
    let url = Url::parse(SUBOMI_NIGHTLY_API_URL).map_err(|error| error.to_string())?;
    let request = Request::get(url)
        .header("User-Agent", user_agent)
        .header("Accept", "application/vnd.github+json");
    let mut response = HttpClient::default()
        .send(&request)
        .map_err(|error| error.to_string())?;
    if response.status() == 404 {
        return Err("No nightly release has been published yet".to_owned());
    }
    if response.status() != 200 {
        return Err(format!("GitHub returned HTTP {}", response.status()));
    }
    let body = response
        .read_all(256 * 1024)
        .map_err(|error| error.to_string())?;
    let value: serde_json::Value =
        serde_json::from_slice(&body).map_err(|error| error.to_string())?;
    parse_nightly_release_json(&value)
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
        "Do you want to check for updates on launch?\n\nThis can be changed at any time in Settings → Options (Cmd+,) → Application.\nThis will query GitHub's releases API when Subomi starts."
    } else {
        "Do you want to check for updates on launch?\n\nThis can be changed at any time in Settings → Options (Ctrl+,) → Application.\nThis will query GitHub's releases API when Subomi starts."
    }
}

pub(crate) fn open_update_opt_in_dialog(app: &SubomiApp) {
    app.set_update_dialog_opt_in(true);
    app.set_update_dialog_title("Auto Update".into());
    app.set_update_dialog_message(update_opt_in_message(cfg!(target_os = "macos")).into());
    app.set_update_dialog_release_tag("".into());
    app.set_update_dialog_release_url(SUBOMI_RELEASES_URL.into());
    app.set_update_dialog_open(true);
}

fn show_update_failure(app: &SubomiApp, error: &str, releases_url: &str, silent: bool) {
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
        app.set_update_dialog_release_url(releases_url.into());
        app.set_update_dialog_open(true);
        app.set_status_message(format!("Could not check for updates: {error}").into());
    }
}

pub(crate) fn apply_update_check_outcome(
    app: &SubomiApp,
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
                    app.set_update_dialog_release_url(SUBOMI_RELEASES_URL.into());
                    app.set_update_dialog_open(true);
                    app.set_status_message("You are on the latest version.".into());
                }
            } else {
                app.set_update_dialog_opt_in(false);
                app.set_update_dialog_title("New Version Available".into());
                app.set_update_dialog_message(
                    format!(
                        "Subomi {trimmed} is available on GitHub Releases (currently installed: {current_tag})."
                    )
                    .into(),
                );
                app.set_update_dialog_release_tag(trimmed.into());
                app.set_update_dialog_release_url(SUBOMI_RELEASES_URL.into());
                app.set_update_dialog_open(true);
                app.set_status_message(format!("New Subomi release available: {trimmed}").into());
            }
        }
        Err(error) => show_update_failure(app, &error, SUBOMI_RELEASES_URL, silent),
    }
}

pub(crate) fn apply_nightly_update_check_outcome(
    app: &SubomiApp,
    current_version: &str,
    local_git_hash: Option<&str>,
    build_channel: BuildChannel,
    outcome: Result<NightlyReleaseInfo, String>,
    silent: bool,
) {
    let local_hash = local_git_hash.map(str::trim).unwrap_or("");
    let current_tag = format!("v{}", current_version.trim_start_matches('v'));
    match outcome {
        Ok(info) => {
            let remote_desc = info.summary_label();
            let same_commit = !local_hash.is_empty()
                && !info.commit_sha.is_empty()
                && commit_hashes_match(local_hash, &info.commit_sha);

            match build_channel {
                BuildChannel::Dev => {
                    // Never silently nag local source builds just because their git SHA
                    // differs from GitHub's base nightly build.
                    if silent {
                        return;
                    }
                    let local_desc = if local_hash.is_empty() {
                        format!("{current_tag} local build")
                    } else {
                        format!("{current_tag} · {local_hash}")
                    };
                    app.set_update_dialog_opt_in(false);
                    if same_commit {
                        app.set_update_dialog_title("Up to Date".into());
                        app.set_update_dialog_message(
                            format!(
                                "You're running a local development build ({local_desc}) matching the latest GitHub nightly ({remote_desc})."
                            )
                            .into(),
                        );
                        app.set_update_dialog_release_tag("".into());
                        app.set_status_message(
                            "Local dev build matches the latest nightly.".into(),
                        );
                    } else {
                        app.set_update_dialog_title("Local Development Build".into());
                        app.set_update_dialog_message(
                            format!(
                                "You're running a local development build ({local_desc}).\n\nLatest GitHub nightly: {remote_desc}\nNote: official nightly downloads are base builds (no ASR/OCR)."
                            )
                            .into(),
                        );
                        app.set_update_dialog_release_tag(info.badge_label().into());
                        app.set_status_message(
                            format!("Local dev build; latest GitHub nightly is {remote_desc}")
                                .into(),
                        );
                    }
                    app.set_update_dialog_release_url(info.url.into());
                    app.set_update_dialog_open(true);
                }
                BuildChannel::Nightly => {
                    if same_commit {
                        if !silent {
                            app.set_update_dialog_opt_in(false);
                            app.set_update_dialog_title("Up to Date".into());
                            app.set_update_dialog_message(
                                format!("You're on the latest nightly build ({remote_desc}).")
                                    .into(),
                            );
                            app.set_update_dialog_release_tag("".into());
                            app.set_update_dialog_release_url(info.url.into());
                            app.set_update_dialog_open(true);
                            app.set_status_message("You are on the latest nightly build.".into());
                        }
                    } else {
                        let installed = if local_hash.is_empty() {
                            current_tag
                        } else {
                            local_hash.to_owned()
                        };
                        app.set_update_dialog_opt_in(false);
                        app.set_update_dialog_title("New Nightly Available".into());
                        app.set_update_dialog_message(
                            format!(
                                "A newer Subomi nightly build ({remote_desc}) is available on GitHub Releases (currently installed: {installed})."
                            )
                            .into(),
                        );
                        app.set_update_dialog_release_tag(info.badge_label().into());
                        app.set_update_dialog_release_url(info.url.into());
                        app.set_update_dialog_open(true);
                        app.set_status_message(
                            format!("New Subomi nightly available: {remote_desc}").into(),
                        );
                    }
                }
                BuildChannel::Stable => {
                    if same_commit {
                        if !silent {
                            app.set_update_dialog_opt_in(false);
                            app.set_update_dialog_title("Up to Date".into());
                            app.set_update_dialog_message(
                                format!(
                                    "Your stable release ({current_tag}) matches the latest nightly build ({remote_desc})."
                                )
                                .into(),
                            );
                            app.set_update_dialog_release_tag("".into());
                            app.set_update_dialog_release_url(info.url.into());
                            app.set_update_dialog_open(true);
                            app.set_status_message("Up to date with the latest nightly.".into());
                        }
                    } else {
                        app.set_update_dialog_opt_in(false);
                        app.set_update_dialog_title("Nightly Build Available".into());
                        app.set_update_dialog_message(
                            format!(
                                "Subomi nightly ({remote_desc}) is available on GitHub Releases (currently installed: {current_tag})."
                            )
                            .into(),
                        );
                        app.set_update_dialog_release_tag(info.badge_label().into());
                        app.set_update_dialog_release_url(info.url.into());
                        app.set_update_dialog_open(true);
                        app.set_status_message(
                            format!("Subomi nightly available: {remote_desc}").into(),
                        );
                    }
                }
            }
        }
        Err(error) => show_update_failure(app, &error, SUBOMI_NIGHTLY_RELEASE_URL, silent),
    }
}

pub(crate) fn spawn_update_check(
    response_app: slint::Weak<SubomiApp>,
    config: &ConfigController,
    silent: bool,
) {
    let channel = active_update_channel(config);
    let build_channel = BuildChannel::current();
    if !silent {
        if let Some(app) = response_app.upgrade() {
            let label = match channel {
                UpdateChannel::Stable => "Checking for updates…",
                UpdateChannel::Nightly => "Checking for nightly updates…",
            };
            app.set_status_message(label.into());
        }
    }
    std::thread::spawn(move || {
        let outcome = match channel {
            UpdateChannel::Stable => UpdateCheckOutcome::Stable(latest_release()),
            UpdateChannel::Nightly => UpdateCheckOutcome::Nightly(latest_nightly_release()),
        };
        let _ = response_app.upgrade_in_event_loop(move |app| match outcome {
            UpdateCheckOutcome::Stable(res) => {
                apply_update_check_outcome(&app, env!("CARGO_PKG_VERSION"), res, silent);
            }
            UpdateCheckOutcome::Nightly(res) => {
                apply_nightly_update_check_outcome(
                    &app,
                    env!("CARGO_PKG_VERSION"),
                    option_env!("SUBOMI_GIT_HASH"),
                    build_channel,
                    res,
                    silent,
                );
            }
        });
    });
}

pub(crate) fn install_update_and_link_handlers(
    app: &SubomiApp,
    config_ctrl: Rc<ConfigController>,
    config_dir: PathBuf,
    settings_session: Rc<RefCell<Option<SettingsSession>>>,
) {
    let update_app = app.as_weak();
    let update_config = config_ctrl.clone();
    app.on_check_updates(move || {
        spawn_update_check(update_app.clone(), &update_config, false);
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
            .persist_single_setting(&opt_in_key, "true", &opt_in_cache, "subomi.conf")
            .and_then(|()| {
                opt_in_config.persist_single_setting(
                    &auto_check_key,
                    auto_val,
                    &opt_in_cache,
                    "subomi.conf",
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
            spawn_update_check(opt_in_app.clone(), &opt_in_config, true);
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
        if let Some(path) = subomi_xdg_dir("XDG_CONFIG_HOME", ".config") {
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
        let app = SubomiApp::new().unwrap();
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

        assert_eq!(
            parse_release_tag_from_location(
                "https://github.com/eyalmazuz/Subomi/releases/tag/v2.1.0"
            ),
            Some("v2.1.0".to_owned())
        );
        assert_eq!(
            parse_release_tag_from_location(
                "/eyalmazuz/Subomi/releases/tag/v2.1.0/?ref=latest#top"
            ),
            Some("v2.1.0".to_owned())
        );
        assert_eq!(
            parse_release_tag_from_location("https://github.com/eyalmazuz/Subomi/releases"),
            None
        );
    }

    #[test]
    fn nightly_channel_defaults_parsing_and_dev_build_behavior() {
        assert_eq!(
            BuildChannel::Nightly.default_update_channel(),
            UpdateChannel::Nightly
        );
        assert_eq!(
            BuildChannel::Stable.default_update_channel(),
            UpdateChannel::Stable
        );
        assert_eq!(
            BuildChannel::Dev.default_update_channel(),
            UpdateChannel::Stable
        );

        let cfg = ConfigController::new();
        assert_eq!(
            active_update_channel_for_build(&cfg, BuildChannel::Nightly),
            UpdateChannel::Nightly
        );
        assert_eq!(
            active_update_channel_for_build(&cfg, BuildChannel::Stable),
            UpdateChannel::Stable
        );
        cfg.edit_setting("application/update-channel", "Nightly")
            .unwrap();
        assert_eq!(
            active_update_channel_for_build(&cfg, BuildChannel::Stable),
            UpdateChannel::Nightly
        );

        let parsed = parse_nightly_release_json(&serde_json::json!({
            "tag_name": "nightly",
            "name": "Nightly (2026-11-26 · 566a82d)",
            "target_commitish": "566a82d1234567890abcdef1234567890abcdef1",
            "published_at": "2026-11-26T09:15:00Z",
            "html_url": "https://github.com/eyalmazuz/Subomi/releases/tag/nightly"
        }))
        .unwrap();
        assert_eq!(parsed.short_sha(), "566a82d");
        assert_eq!(parsed.published_date, "2026-11-26");
        assert_eq!(parsed.badge_label(), "nightly-566a82d");
        assert!(commit_hashes_match("566a82d", &parsed.commit_sha));
        assert!(!commit_hashes_match("9999aaa", &parsed.commit_sha));

        i_slint_backend_testing::init_no_event_loop();
        let app = SubomiApp::new().unwrap();

        // Official nightly: silent check with matching SHA does not open modal.
        apply_nightly_update_check_outcome(
            &app,
            "3.0.0",
            Some("566a82d"),
            BuildChannel::Nightly,
            Ok(parsed.clone()),
            true,
        );
        assert!(!app.get_update_dialog_open());

        // Official nightly: silent check with different SHA opens "New Nightly Available".
        apply_nightly_update_check_outcome(
            &app,
            "3.0.0",
            Some("1111aaa"),
            BuildChannel::Nightly,
            Ok(parsed.clone()),
            true,
        );
        assert!(app.get_update_dialog_open());
        assert_eq!(
            app.get_update_dialog_title().as_str(),
            "New Nightly Available"
        );
        assert_eq!(
            app.get_update_dialog_release_url().as_str(),
            SUBOMI_NIGHTLY_RELEASE_URL
        );

        // Local dev build: silent check must NEVER nag even when SHA differs.
        app.set_update_dialog_open(false);
        apply_nightly_update_check_outcome(
            &app,
            "3.0.0",
            Some("9999aaa"),
            BuildChannel::Dev,
            Ok(parsed.clone()),
            true,
        );
        assert!(!app.get_update_dialog_open());

        // Local dev build: manual check informs clearly without calling it outdated.
        apply_nightly_update_check_outcome(
            &app,
            "3.0.0",
            Some("9999aaa"),
            BuildChannel::Dev,
            Ok(parsed),
            false,
        );
        assert!(app.get_update_dialog_open());
        assert_eq!(
            app.get_update_dialog_title().as_str(),
            "Local Development Build"
        );
        assert!(app
            .get_update_dialog_message()
            .as_str()
            .contains("no ASR/OCR"));
    }
}
